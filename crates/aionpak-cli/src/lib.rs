mod report;

use aionpak_commons::ops::{self, ExtractOptions, FlavorMode, PakOptions, XmlMode};
use aionpak_commons::pak::{self, PakReader};
use aionpak_commons::progress::Progress;
use aionpak_commons::xml::{Encoding, Form};
use aionpak_commons::{names, optimize, tree};
use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use glob::{MatchOptions, Pattern};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

// Workers allocate from many threads at once, which musl's allocator serialises: the static Linux build runs up to
// three times slower with it. The binary installs mimalloc, not this library: a global allocator belongs to the program.

const WORKFLOWS: &str = "\
Typical work:
  Edit one archive:
    aionpak extract Data/skills/skills.pak -o skills -d
    (edit the files in skills/, add or delete files if needed)
    aionpak pack skills -o Data/skills/skills.pak

  Edit a whole client:
    aionpak extract <client> -o tree -d          every archive becomes tree/<path>/<name>.pak/
    aionpak repack tree -o <client>              only the <name>.pak/ folders are packed back
    aionpak compare <client-copy> <client>       what changed, archive by archive

  Every <name>.pak/ folder holds .aionpak, the manifest that lets pack rebuild the archive: keep it.
  XML is compiled and HTML stored as text, the forms the client loads fastest; --xml original --html original
  puts every file back in the form it had. Keep a copy of the client before writing into it.";

#[derive(Parser)]
#[command(name = "aionpak", version, about = "Extract, pack and optimize Aion client .pak archives", after_help = WORKFLOWS)]
struct Cli {
	#[command(subcommand)]
	command: Command,
	#[command(flatten)]
	common: Common,
}

#[derive(Args)]
struct Common {
	// Listed after every command's own options: left to count from 0 as those do, they landed among them.
	/// List every warning, not only the count
	#[arg(short, long, global = true, display_order = 100)]
	verbose: bool,
	/// No progress bar, only errors and the summary
	#[arg(short, long, global = true, display_order = 101)]
	quiet: bool,
	/// Also write all errors and warnings to this file
	#[arg(long, global = true, value_name = "FILE", display_order = 102)]
	log: Option<PathBuf>,
	/// Worker threads (default: all cores)
	#[arg(short, long, global = true, display_order = 103)]
	jobs: Option<usize>,
}

#[derive(Subcommand)]
enum Command {
	/// List the entries of an archive
	#[command(after_help = "Example: aionpak list Data/World/World.pak")]
	List { pak: PathBuf },
	/// Extract an archive, or every archive under a folder
	#[command(after_help = "\
One archive: the entries go straight into the output folder, with the manifest .aionpak.
A folder: the tree is kept and every archive becomes a folder of the same name,
  <client>/Objects/npc/pet/pet.pak  ->  <out>/Objects/npc/pet/pet.pak/...
Files that are not zip archives (browser .pak files in bin32/bin64) are skipped with a warning.

Examples:
  aionpak extract Data/World/World.pak -o world -d
  aionpak extract <client> -o tree -d --utf8
  aionpak extract <client>/Data -o data --include \"*.{xml,html}\"   (partial: no manifest, do not repack it over the client)")]
	Extract {
		input: PathBuf,
		/// Folder to extract into
		#[arg(short, long)]
		out: PathBuf,
		/// Turn XML and HTML into readable text (UTF-16 unless --utf8)
		#[arg(short, long)]
		decode: bool,
		/// Decode to UTF-8 instead of UTF-16
		#[arg(long, requires = "decode")]
		utf8: bool,
		/// Only entries whose name inside the archive matches one of these globs (any case, `{a,b}`
		/// alternatives); such a partial extraction writes no manifest
		#[arg(long, value_name = "GLOB")]
		include: Vec<String>,
		/// Extract into a folder that is not empty (a tree's <name>.pak/ folders are replaced, not merged into)
		#[arg(long)]
		force: bool,
	},
	/// Pack a folder into an archive
	#[command(
		name = "pack",
		after_help = "\
Entries listed in the folder's manifest keep their order, names and times; other files follow, sorted;
listed files that are gone are dropped with a warning. The archive is written to a temporary file first,
so a failure leaves the old one untouched.

Examples:
  aionpak pack world -o Data/World/World.pak
  aionpak pack world -o World.pak -l 12 --xml original --html original"
	)]
	Pak {
		dir: PathBuf,
		/// Archive to write
		#[arg(short, long)]
		out: PathBuf,
		#[command(flatten)]
		pak: PakArgs,
	},
	/// Pack every <name>.pak/ folder under a tree back into <name>.pak at the same place
	#[command(after_help = "\
Only folders named <something>.pak are packed; nothing else in the tree or the client is touched.

Example: aionpak repack tree -o <client>")]
	Repack {
		tree: PathBuf,
		/// Client folder the archives are written into, each at its place in the tree
		#[arg(short, long)]
		out: PathBuf,
		#[command(flatten)]
		pak: PakArgs,
	},
	/// Turn loose XML and HTML files (one file or a folder, recursively) into readable text
	#[command(after_help = "\
Without -o the files are converted in place. Files converted this way have no manifest record of their form;
packed later, they stay text (the client reads UTF-16 text), unless --xml asks for another form.

Example: aionpak decode client_skills.xml -o client_skills.txt.xml")]
	Decode {
		input: PathBuf,
		/// Output file or folder (default: in place)
		#[arg(short, long)]
		out: Option<PathBuf>,
		/// UTF-8 instead of UTF-16
		#[arg(long)]
		utf8: bool,
	},
	/// Bring loose XML and HTML files into a client form
	#[command(after_help = "\
HTML files cannot be compiled and take their own form from --html; by default they are left as they are, since
the client reads plain HTML as well as encrypted. A file the client does not read in the form asked (level files
compiled or encrypted) is left as it is, as pack does.

Examples:
  aionpak encode client_skills.txt.xml -o client_skills.xml
  aionpak encode dialogs_text -o dialogs --xml compiled --html encrypted")]
	Encode {
		input: PathBuf,
		/// Output file or folder (default: in place)
		#[arg(short, long)]
		out: Option<PathBuf>,
		/// The form of the XML files
		#[arg(long, value_enum, default_value_t = FormArg::Compiled)]
		xml: FormArg,
		/// The form of the HTML files
		#[arg(long, value_enum, default_value_t = HtmlFormArg::Keep)]
		html: HtmlFormArg,
	},
	/// Read every entry and check its CRC, as the client does
	#[command(after_help = "Example: aionpak verify <client>")]
	Verify { input: PathBuf },
	/// Check that two archives, or two folders of archives, hold the same files
	#[command(after_help = "\
Compares names, order, sizes and CRCs from the archive directories; run verify as well to check the data
against the CRCs.

Example: aionpak compare <client-backup> <client>")]
	Compare { first: PathBuf, second: PathBuf },
	/// Merge the archives of every folder the client scans into a few volumes
	#[command(after_help = "\
Every folder the client scans at start (Data, Levels/Common, Objects, Sounds, Textures, Effects, event, Shaders,
each L10N/<language>) gets volumes !merged_NNNN.pak holding, for every name, the copy the client would pick.
Archives opened by name, signed or not decoding (encrypted for one client) stay. The result is checked name by
name before it counts. The archives that stay (signed and undecodable ones excepted) and those under
Levels/<map> are recompressed in place too, recorded in the same list, unless --keep-rest or --copy.
XML is compiled and HTML stored as text by default; --xml stored --html stored keeps them as they are.

  -o <out>   volumes go to <out> (mirroring the client), the client is not touched;
             aionpak-optimize.txt lists the archives they replace
  --apply    volumes go into the client and the merged archives move to a backup folder,
             with aionpak-optimize.txt there
  --undo <backup>  reverts --apply

Close the game first and keep the backup.")]
	Optimize {
		client: PathBuf,
		/// Write the volumes into this folder (mirroring the client) and list what they replace
		#[arg(short, long, required_unless_present_any = ["apply", "undo"], conflicts_with_all = ["apply", "undo"])]
		out: Option<PathBuf>,
		/// Write the volumes into the client and move the merged archives into a backup folder
		#[arg(long, conflicts_with = "undo")]
		apply: bool,
		/// Revert an --apply: delete the volumes and move the archives back from this backup folder
		#[arg(long, value_name = "BACKUP")]
		undo: Option<PathBuf>,
		/// Backup folder for --apply (default: <client>/aionpak-backup-<time>)
		#[arg(long, requires = "apply")]
		backup: Option<PathBuf>,
		/// Recompress at this level, 0 (store) to 12
		#[arg(short, long, default_value_t = 9, value_parser = clap::value_parser!(u8).range(0..=12))]
		level: u8,
		/// Copy the stored data instead of recompressing it: quicker, larger volumes
		#[arg(long, conflicts_with = "level")]
		copy: bool,
		/// What to do with XML files
		#[arg(long, value_enum, default_value_t = OptXmlArg::Compiled)]
		xml: OptXmlArg,
		/// What to do with HTML files
		#[arg(long, value_enum, default_value_t = OptHtmlArg::Text)]
		html: OptHtmlArg,
		/// Store as text the XML files that stay encrypted (0x81), so other tools read them
		#[arg(long)]
		decrypt: bool,
		/// Largest volume, from 50M to 4G, e.g. 512M or 1G
		#[arg(long, default_value = "512M", value_parser = parse_size)]
		volume_size: u64,
		/// Leave the archives that are not merged as they are. By default those that stay and those under
		/// Levels/<map> are recompressed in place too; signed archives and archives that do not decode never are
		#[arg(long)]
		keep_rest: bool,
		/// The default now; still accepted
		#[arg(long, hide = true)]
		recompress_rest: bool,
	},
}

fn parse_size(s: &str) -> std::result::Result<u64, String> {
	let s = s.trim();
	let (num, mul) = match s.chars().last().map(|c| c.to_ascii_uppercase()) {
		Some('K') => (&s[..s.len() - 1], 1u64 << 10),
		Some('M') => (&s[..s.len() - 1], 1 << 20),
		Some('G') => (&s[..s.len() - 1], 1 << 30),
		_ => (s, 1),
	};
	let n: u64 = num.parse().map_err(|_| format!("bad size {s}"))?;
	let v = n.checked_mul(mul).ok_or("size too large")?;
	if !(optimize::MIN_VOLUME_SIZE..=pak::MAX_ARCHIVE_SIZE).contains(&v) {
		return Err("size must be between 50M and 4G".into());
	}
	Ok(v)
}

#[derive(Args)]
struct PakArgs {
	/// Compression level, 0 (store) to 12, for files changed since extraction (see --recompress)
	#[arg(short, long, default_value_t = 9, value_parser = clap::value_parser!(u8).range(0..=12))]
	level: u8,
	/// What to do with XML files
	#[arg(long, value_enum, default_value_t = XmlArg::Compiled)]
	xml: XmlArg,
	/// What to do with HTML files
	#[arg(long, value_enum, default_value_t = HtmlArg::Text)]
	html: HtmlArg,
	/// Record signatures: plain zip, the scrambled client variant, or as recorded on extraction
	#[arg(long, value_enum, default_value_t = FlavorArg::Plain)]
	flavor: FlavorArg,
	/// Replace an existing archive even from a folder without a manifest (possibly a partial extraction)
	#[arg(long)]
	force: bool,
	/// Compress every file at --level. Without it, files unchanged since extraction keep the data they have in the
	/// archive they came from, copied without compressing again, while that archive is still there unchanged
	#[arg(long)]
	recompress: bool,
	/// Store as text the XML files that would end up encrypted (0x81), so other tools read them
	#[arg(long)]
	decrypt: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum XmlArg {
	/// Compiled where the client reads it so (the fastest to load); level files stay text
	Compiled,
	/// UTF-16 text
	Utf16,
	/// The form each file had in the archive (files without a record keep theirs)
	Original,
	/// Bytes exactly as on disk
	Keep,
	/// Encrypted (0x81), keyed by the file name; level files stay as they were
	Encrypted,
}

#[derive(Clone, Copy, ValueEnum)]
enum HtmlArg {
	/// Encrypted files decrypted: UTF-16 text, or their 8-bit bytes where the text is in a code page
	Text,
	/// The form each file had in the archive
	Original,
	/// Bytes exactly as on disk
	Keep,
}

#[derive(Clone, Copy, ValueEnum)]
enum OptXmlArg {
	/// Compiled where the client reads it so (the fastest to load); level files stay as stored
	Compiled,
	/// UTF-16 text
	Utf16,
	/// 8-bit text
	Text,
	/// Encrypted (0x81), keyed by the file name; level files stay as stored
	Encrypted,
	/// As the archives store them
	Stored,
}

#[derive(Clone, Copy, ValueEnum)]
enum OptHtmlArg {
	/// Encrypted files decrypted to text
	Text,
	/// As the archives store them
	Stored,
}

#[derive(Clone, Copy, ValueEnum)]
enum FormArg {
	/// Compiled (0x80), the fastest to load; level files are left as they are
	Compiled,
	/// Encrypted (0x81), keyed by the file name: encode under the final name
	Encrypted,
	/// UTF-16 text
	Utf16,
	/// 8-bit text; UTF-8 with non-ASCII characters is stored as UTF-16, since the client reads 8-bit text in the
	/// system code page
	Text,
}

impl FormArg {
	fn form(self) -> Form {
		match self {
			FormArg::Compiled => Form::Compiled,
			FormArg::Encrypted => Form::Encrypted,
			FormArg::Utf16 => Form::Utf16,
			FormArg::Text => Form::Text,
		}
	}
}

#[derive(Clone, Copy, ValueEnum)]
enum HtmlFormArg {
	/// Left as they are
	Keep,
	/// Encrypted (0x81), keyed by the file name: encode under the final name
	Encrypted,
	/// UTF-16 text
	Utf16,
	/// 8-bit text; UTF-8 with non-ASCII characters is stored as UTF-16, since the client reads 8-bit text in the
	/// system code page
	Text,
}

impl HtmlFormArg {
	fn form(self) -> Option<Form> {
		match self {
			HtmlFormArg::Keep => None,
			HtmlFormArg::Encrypted => Some(Form::Encrypted),
			HtmlFormArg::Utf16 => Some(Form::Utf16),
			HtmlFormArg::Text => Some(Form::Text),
		}
	}
}

#[derive(Clone, Copy, ValueEnum)]
enum FlavorArg {
	Plain,
	Aion,
	Original,
}

impl PakArgs {
	fn options(&self) -> Result<PakOptions> {
		if self.decrypt && matches!(self.xml, XmlArg::Encrypted) {
			bail!("--decrypt undoes --xml encrypted");
		}
		Ok(PakOptions {
			level: self.level,
			xml: match self.xml {
				XmlArg::Original => XmlMode::Original,
				XmlArg::Keep => XmlMode::Keep,
				XmlArg::Compiled => XmlMode::To(Form::Compiled),
				XmlArg::Encrypted => XmlMode::To(Form::Encrypted),
				XmlArg::Utf16 => XmlMode::To(Form::Utf16),
			},
			flavor: match self.flavor {
				FlavorArg::Plain => FlavorMode::Plain,
				FlavorArg::Aion => FlavorMode::Aion,
				FlavorArg::Original => FlavorMode::Original,
			},
			force: self.force,
			recompress: self.recompress,
			decrypt: self.decrypt,
			html: Some(match self.html {
				HtmlArg::Text => (XmlMode::Original, true),
				HtmlArg::Original => (XmlMode::Original, false),
				HtmlArg::Keep => (XmlMode::Keep, false),
			}),
		})
	}
}

/// Runs the command line with the arguments of a whole process, `args[0]` being the program name.
pub fn main_with_args(args: Vec<std::ffi::OsString>) -> ExitCode {
	let cli = Cli::parse_from(args);
	if let Some(n) = cli.common.jobs {
		let _ = rayon::ThreadPoolBuilder::new().num_threads(n).build_global();
	}
	if let Command::List { pak } = &cli.command {
		return match list(pak) {
			Ok(()) => ExitCode::SUCCESS,
			// The reader went away, e.g. `aionpak list x.pak | head`: what it wanted was written.
			Err(e)
				if e.downcast_ref::<io::Error>()
					.is_some_and(|e| e.kind() == io::ErrorKind::BrokenPipe) =>
			{
				ExitCode::SUCCESS
			}
			Err(e) => {
				report::say(format_args!("error: {e:#}"));
				ExitCode::from(2)
			}
		};
	}
	let progress = std::sync::Arc::new(Progress::default());
	let started = std::time::Instant::now();
	let display = report::Display::start(&progress, cli.common.quiet);
	let compare = matches!(cli.command, Command::Compare { .. });
	let result = run(&cli.command, &progress);
	display.finish();
	match report::summary(&progress, started.elapsed(), &cli.common, result, compare) {
		Ok(true) => ExitCode::SUCCESS,
		Ok(false) => ExitCode::from(1),
		Err(e) => {
			report::say(format_args!("error: {e:#}"));
			ExitCode::from(2)
		}
	}
}

/// Entry names are matched the way the client reads them: case does not matter.
const NAME_MATCH: MatchOptions = MatchOptions {
	case_sensitive: false,
	require_literal_separator: false,
	require_literal_leading_dot: false,
};

/// Expands `{a,b}` alternatives, which `glob` does not know: `*.{xml,html}` becomes `*.xml` and `*.html`.
/// An unbalanced brace is left as it is.
fn expand_braces(glob: &str) -> Vec<String> {
	let Some(open) = glob.find('{') else {
		return vec![glob.to_owned()];
	};
	let mut depth = 0;
	let mut start = open + 1;
	let mut parts = Vec::new();
	for (i, c) in glob[open..].char_indices().map(|(i, c)| (open + i, c)) {
		match c {
			'{' => depth += 1,
			',' if depth == 1 => {
				parts.push(&glob[start..i]);
				start = i + 1;
			}
			'}' => {
				depth -= 1;
				if depth == 0 {
					parts.push(&glob[start..i]);
					let (head, tail) = (&glob[..open], &glob[i + 1..]);
					return parts
						.iter()
						.flat_map(|p| expand_braces(&format!("{head}{p}{tail}")))
						.collect();
				}
			}
			_ => {}
		}
	}
	vec![glob.to_owned()]
}

fn run(command: &Command, progress: &Progress) -> Result<()> {
	match command {
		Command::List { pak } => list(pak),
		Command::Extract {
			input,
			out,
			decode,
			utf8,
			include,
			force,
		} => {
			let patterns = include
				.iter()
				.flat_map(|g| expand_braces(g))
				.map(|g| Pattern::new(&g).with_context(|| format!("bad glob {g}")))
				.collect::<Result<Vec<_>>>()?;
			let match_fn = (!patterns.is_empty()).then_some(|name: &str| {
				let name = name.replace('\\', "/");
				patterns.iter().any(|p| p.matches_with(&name, NAME_MATCH))
			});
			let opts = ExtractOptions {
				decode: decode.then_some(if *utf8 { Encoding::Utf8 } else { Encoding::Utf16 }),
				filter: match_fn.as_ref().map(|f| f as &ops::NameFilter),
				force: *force,
			};
			if input.is_dir() {
				tree::extract_tree(input, out, &opts, progress)?;
			} else {
				let reader = PakReader::open(input).with_context(|| input.display().to_string())?;
				let (files, bytes) = ops::totals(&reader, opts.filter);
				progress.add_total(files, bytes);
				progress.archives_total.store(1, std::sync::atomic::Ordering::Relaxed);
				ops::extract(&reader, &input.display().to_string(), out, &opts, progress)?;
				progress.archives_done.store(1, std::sync::atomic::Ordering::Relaxed);
			}
			if match_fn.is_some() && progress.files_total.load(std::sync::atomic::Ordering::Relaxed) == 0 {
				bail!("no entry matches --include (globs match names inside the archives)");
			}
			Ok(())
		}
		Command::Pak { dir, out, pak } => {
			if !dir.is_dir() {
				bail!("{} is not a folder", dir.display());
			}
			progress.archives_total.store(1, std::sync::atomic::Ordering::Relaxed);
			ops::pak(dir, out, &pak.options()?, progress)?;
			progress.archives_done.store(1, std::sync::atomic::Ordering::Relaxed);
			Ok(())
		}
		Command::Repack { tree: root, out, pak } => Ok(tree::repack_tree(root, out, &pak.options()?, progress)?),
		Command::Decode { input, out, utf8 } => {
			let encoding = if *utf8 { Encoding::Utf8 } else { Encoding::Utf16 };
			ops::convert(input, out.as_deref(), ops::Convert::Decode(encoding), progress)?;
			Ok(())
		}
		Command::Encode { input, out, xml, html } => {
			let mode = ops::Convert::Encode { xml: xml.form(), html: html.form() };
			ops::convert(input, out.as_deref(), mode, progress)?;
			Ok(())
		}
		Command::Verify { input } => verify(input, progress),
		Command::Compare { first, second } => {
			let tree = !(first.is_file() && second.is_file());
			for d in aionpak_commons::compare::trees(first, second)?.lines(tree) {
				progress.error(d);
			}
			Ok(())
		}
		Command::Optimize {
			client,
			out,
			apply,
			undo,
			backup,
			level,
			copy,
			xml,
			html,
			decrypt,
			volume_size,
			keep_rest,
			recompress_rest: _,
		} => {
			if *decrypt && matches!(xml, OptXmlArg::Encrypted) {
				bail!("--decrypt undoes --xml encrypted");
			}
			if let Some(backup) = undo {
				let (deleted, restored) = optimize::undo(client, backup, progress)?;
				report::say(format_args!(
					"{deleted} volumes deleted, {restored} archives moved back"
				));
				return Ok(());
			}
			let recompress_rest = !*copy && !*keep_rest;
			let opts = optimize::OptimizeOptions {
				level: (!*copy).then_some(*level),
				xml: match xml {
					OptXmlArg::Compiled => Some(Form::Compiled),
					OptXmlArg::Utf16 => Some(Form::Utf16),
					OptXmlArg::Text => Some(Form::Text),
					OptXmlArg::Encrypted => Some(Form::Encrypted),
					OptXmlArg::Stored => None,
				},
				decrypt: *decrypt,
				html: Some((None, matches!(html, OptHtmlArg::Text))),
				volume_size: *volume_size,
				recompress_rest,
			};
			let stamp = std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.map_or(0, |d| d.as_secs());
			let backup = apply.then(|| {
				backup
					.clone()
					.unwrap_or_else(|| client.join(format!("aionpak-backup-{stamp}")))
			});
			let target = if *apply { client.clone() } else { out.clone().unwrap() };
			let outcome = optimize::optimize(client, &target, backup.as_deref(), &opts, progress)?;
			let list = backup.as_deref().unwrap_or(&target).join(optimize::LIST_NAME);
			report::say(format_args!(
				"{} archives merged into {} volumes; list in {}",
				outcome.replaced.len(),
				outcome.volumes.len(),
				list.display()
			));
			if recompress_rest {
				let (before, after) = outcome.recompressed_bytes;
				report::say(format_args!(
					"{} more archives recompressed in place, {:.1} MiB -> {:.1} MiB",
					outcome.recompressed.len(),
					before as f64 / 1048576.0,
					after as f64 / 1048576.0
				));
			}
			Ok(())
		}
	}
}

fn list(path: &Path) -> Result<()> {
	let pak = PakReader::open(path).with_context(|| path.display().to_string())?;
	let mut out = io::BufWriter::new(io::stdout().lock());
	writeln!(out, "{:>12} {:>12} {:>6}  name", "size", "stored", "method")?;
	for e in pak.entries() {
		let method = if e.method == pak::METHOD_STORE {
			"store"
		} else {
			"deflate"
		};
		writeln!(
			out,
			"{:>12} {:>12} {:>6}  {}",
			e.size,
			e.compressed_size,
			method,
			names::to_text(&e.name)
		)?;
	}
	let (files, bytes) = ops::totals(&pak, None);
	writeln!(out, "{files} entries, {bytes} bytes, {:?} signatures", pak.flavor())?;
	out.flush()?;
	Ok(())
}

fn verify(input: &Path, progress: &Progress) -> Result<()> {
	if input.is_dir() {
		let paks = tree::find_paks(input)?;
		aionpak_commons::verify::archives(input, &paks, true, progress);
	} else {
		let pak = PakReader::open(input).with_context(|| input.display().to_string())?;
		let (files, bytes) = ops::totals(&pak, None);
		progress.add_total(files, bytes);
		progress.archives_total.store(1, std::sync::atomic::Ordering::Relaxed);
		aionpak_commons::verify::archive(&pak, &input.display().to_string(), true, progress);
		progress.archives_done.store(1, std::sync::atomic::Ordering::Relaxed);
	}
	if input.is_dir() {
		for o in optimize::overrides(input, progress)? {
			progress.warn(optimize::override_text(input, &o));
		}
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn braces() {
		assert_eq!(expand_braces("*.xml"), ["*.xml"]);
		assert_eq!(expand_braces("*.{xml,html}"), ["*.xml", "*.html"]);
		assert_eq!(expand_braces("{a,b{c,d}}/*"), ["a/*", "bc/*", "bd/*"]);
		assert_eq!(expand_braces("{a,b}{1,2}"), ["a1", "a2", "b1", "b2"]);
		assert_eq!(expand_braces("x{a,b"), ["x{a,b"]);
	}
}
