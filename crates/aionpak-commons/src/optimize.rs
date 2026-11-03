//! Merging the archives of each folder the client scans recursively into a few volumes at the folder's top.
//!
//! The client opens every `*.pak` of a scanned folder (sorted by name), then descends into the subfolders in
//! directory order; a name resolves to the last opened archive that has it, and within one archive to its first
//! entry. A volume at the top of the folder is opened before anything left below it, so an archive that stays (it
//! is opened by name elsewhere, or signed) must not hold a name that a merged, later archive overrode: such
//! archives stay as well.

use crate::error::{Error, Result};
use crate::names;
use crate::ops::{self, BIG_BYTES, FileWriter, Prepared, WINDOW_BYTES};
use crate::ordered;
use crate::pak::{CodecPool, Entry, Flavor, MAX_ENTRIES, PakReader, PakWriter, WriteEntry};
use crate::progress::Progress;
use crate::xml::{self, Form};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

/// Folders the client scans recursively at start-up, in its order, besides every folder under `L10N`.
pub const SCAN_ROOTS: [&str; 8] = [
	"Data",
	"Levels/Common",
	"Objects",
	"Sounds",
	"Textures",
	"Effects",
	"event",
	"Shaders",
];

/// Archives the client also opens by name, so they must stay where they are.
pub const OPENED_BY_NAME: [&str; 1] = ["Data/func_pet/func_pet.pak"];

/// The smallest volume asked for. Smaller ones only multiply the archives the client opens, which is what merging
/// is there to cut.
pub const MIN_VOLUME_SIZE: u64 = 50 << 20;

/// Sorts before any letter or digit, so a volume opens first in its folder.
const VOLUME_PREFIX: &str = "!merged_";

pub struct OptimizeOptions {
	/// Recompress at this level instead of copying the stored data.
	pub level: Option<u8>,
	/// Bring every XML and HTML into this form (implies decompression).
	pub xml: Option<Form>,
	/// Store as UTF-16 text every XML and HTML file that would end up encrypted.
	pub decrypt: bool,
	/// HTML files on their own terms: their form (`None`: as stored) and whether the encrypted ones become text.
	/// `None`: as the XML files, by `xml` and `decrypt`.
	pub html: Option<(Option<Form>, bool)>,
	/// The largest volume in bytes, from `MIN_VOLUME_SIZE` up to `pak::MAX_ARCHIVE_SIZE`.
	pub volume_size: u64,
	/// With `level`, also rewrite in place every archive that is not merged (those that stay in the scanned
	/// folders and those under `Levels/<map>`), keeping its name, entries and their order.
	pub recompress_rest: bool,
}

pub struct Plan {
	pub root: PathBuf,
	/// Archives in the client's opening order, relative to the client folder.
	pub merged: Vec<PathBuf>,
	pub kept: Vec<PathBuf>,
}

pub struct Outcome {
	pub volumes: Vec<PathBuf>,
	pub replaced: Vec<PathBuf>,
	/// Archives rewritten in place at the new level (`recompress_rest`), relative to the client folder.
	pub recompressed: Vec<PathBuf>,
	/// Their total size before and after.
	pub recompressed_bytes: (u64, u64),
}

fn is_pak(p: &Path) -> bool {
	p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pak"))
}

fn find_dir_ci(parent: &Path, name: &str) -> Option<PathBuf> {
	fs::read_dir(parent)
		.ok()?
		.filter_map(|e| e.ok())
		.find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(name) && e.path().is_dir())
		.map(|e| e.path())
}

/// Resolves a client-relative folder such as `Levels/Common` regardless of case.
fn resolve(client: &Path, rel: &str) -> Option<PathBuf> {
	let mut p = client.to_path_buf();
	for part in rel.split('/') {
		p = find_dir_ci(&p, part)?;
	}
	Some(p)
}

/// The folders to optimise, present in this client.
pub fn scan_roots(client: &Path) -> Vec<PathBuf> {
	let mut roots: Vec<PathBuf> = Vec::new();
	if let Some(l10n) = resolve(client, "L10N")
		&& let Ok(rd) = fs::read_dir(&l10n)
	{
		let mut langs: Vec<PathBuf> = rd
			.filter_map(|e| e.ok())
			.map(|e| e.path())
			.filter(|p| p.is_dir())
			.collect();
		langs.sort();
		roots.extend(langs);
	}
	roots.extend(SCAN_ROOTS.iter().filter_map(|r| resolve(client, r)));
	roots
}

/// Archives under `dir` in the order the client opens them.
pub fn opening_order(dir: &Path) -> Result<Vec<PathBuf>> {
	let mut out = Vec::new();
	walk(dir, &mut out)?;
	Ok(out)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
	let mut paks = Vec::new();
	let mut subdirs = Vec::new();
	for e in fs::read_dir(dir)? {
		let e = e?;
		let t = e.file_type()?;
		if t.is_dir() {
			subdirs.push(e.path());
		} else if t.is_file() && is_pak(&e.path()) {
			paks.push(e.path());
		}
	}
	paks.sort_by(|a, b| {
		a.file_name()
			.unwrap()
			.as_encoded_bytes()
			.cmp(b.file_name().unwrap().as_encoded_bytes())
	});
	subdirs.sort_by_key(|p| p.file_name().unwrap().to_string_lossy().to_uppercase());
	out.extend(paks);
	for d in subdirs {
		walk(&d, out)?;
	}
	Ok(())
}

/// Lookup keys of the entries of an archive bound to folder `bind`, relative to `root`, first entry winning
/// within the archive.
fn keys(reader: &PakReader, bind: &Path, root: &Path) -> Vec<(Vec<u8>, usize)> {
	let bind = bind.strip_prefix(root).unwrap_or(Path::new(""));
	let mut prefix = names::path_name(bind).unwrap_or_default();
	if !prefix.is_empty() {
		prefix.push(b'/');
	}
	let mut seen = HashSet::new();
	let mut out = Vec::new();
	for (i, e) in reader.entries().iter().enumerate() {
		if e.is_dir() {
			continue;
		}
		let mut k = prefix.clone();
		k.extend_from_slice(&e.name);
		let k = names::lookup_key(&k);
		if seen.insert(k.clone()) {
			out.push((k, i));
		}
	}
	out
}

/// Files of one archive that the client never loads because a later archive of the same folder tree has them too,
/// with other content.
pub struct Override {
	pub hidden: PathBuf,
	pub winner: PathBuf,
	pub files: usize,
	pub example: String,
}

/// Opening index of an archive and the (CRC, size) of its copy of a name.
type Holder = (usize, (u32, u32));

/// Every pair of archives under `root` where the later one hides files of the earlier one with other content, in
/// opening order. Identical copies are left out: which one loads makes no difference.
fn overrides_in(root: &Path, readers: &HashMap<PathBuf, PakReader>) -> Result<Vec<Override>> {
	let order: Vec<PathBuf> = opening_order(root)?
		.into_iter()
		.filter(|p| readers.contains_key(p))
		.collect();
	// Every name with the archives holding it, in opening order, and the content each one has.
	let mut holders: HashMap<Vec<u8>, Vec<Holder>> = HashMap::new();
	for (i, p) in order.iter().enumerate() {
		let reader = &readers[p];
		for (k, e) in keys(reader, p.parent().unwrap(), root) {
			let e = &reader.entries()[e];
			holders.entry(k).or_default().push((i, (e.crc, e.size)));
		}
	}
	// (hidden, winner) -> files, smallest name as the example
	let mut pairs: HashMap<(usize, usize), (usize, Vec<u8>)> = HashMap::new();
	for (k, hs) in holders {
		let &(winner, loaded) = hs.last().unwrap();
		for &(h, content) in &hs[..hs.len() - 1] {
			if content == loaded {
				continue;
			}
			let pair = pairs.entry((h, winner)).or_insert_with(|| (0, k.clone()));
			pair.0 += 1;
			if k < pair.1 {
				pair.1 = k.clone();
			}
		}
	}
	let mut out: Vec<_> = pairs.into_iter().collect();
	out.sort_by_key(|((h, w), _)| (*h, *w));
	Ok(out
		.into_iter()
		.map(|((h, w), (files, example))| Override {
			hidden: order[h].clone(),
			winner: order[w].clone(),
			files,
			example: names::to_text(&example),
		})
		.collect())
}

/// Archive pairs where a later archive hides files of an earlier one, in every folder the client scans (or in
/// `client` itself when it is not a client folder).
pub fn overrides(client: &Path, progress: &Progress) -> Result<Vec<Override>> {
	let mut roots = scan_roots(client);
	if roots.is_empty() {
		roots.push(client.to_path_buf());
	}
	let mut out = Vec::new();
	for root in &roots {
		let readers: HashMap<PathBuf, PakReader> = opening_order(root)?
			.into_par_iter()
			.filter_map(|p| PakReader::open(&p).ok().map(|r| (p, r)))
			.collect();
		progress.set_current(root.display().to_string());
		out.extend(overrides_in(root, &readers)?);
	}
	Ok(out)
}

/// One line for a warning: which archive hides how many files of which.
pub fn override_text(client: &Path, o: &Override) -> String {
	let rel = |p: &Path| p.strip_prefix(client).unwrap_or(p).display().to_string();
	format!(
		"{}: {} file(s) never loaded, {} (opened later) has other copies, e.g. {}",
		rel(&o.hidden),
		o.files,
		rel(&o.winner),
		o.example
	)
}

fn is_signed(pak: &Path) -> bool {
	pak.with_extension("pak.sig").exists() || {
		let mut s = pak.as_os_str().to_owned();
		s.push(".sig");
		Path::new(&s).exists()
	}
}

fn is_volume(pak: &Path) -> bool {
	pak.file_name()
		.is_some_and(|n| n.to_string_lossy().starts_with(VOLUME_PREFIX))
}

fn must_stay(client: &Path, pak: &Path) -> bool {
	let rel = pak
		.strip_prefix(client)
		.unwrap_or(pak)
		.to_string_lossy()
		.replace('\\', "/");
	is_signed(pak) || OPENED_BY_NAME.iter().any(|n| n.eq_ignore_ascii_case(&rel)) || is_volume(pak)
}

/// Checks that the archive's entries decode: three spread over it are read and their CRC checked. Some clients ship
/// archives whose data is encrypted beyond the usual scrambling and decrypted by their own code; such an archive can
/// be neither recompressed nor moved into a volume, since nobody knows whether its decryption depends on its name.
fn probe(reader: &PakReader, codecs: &CodecPool) -> Result<()> {
	let files: Vec<&Entry> = reader.entries().iter().filter(|e| !e.is_dir() && e.size > 0).collect();
	if files.is_empty() {
		return Ok(());
	}
	codecs.with(|codec| {
		let mut buf = Vec::new();
		for i in [0, files.len() / 2, files.len() - 1] {
			reader.read_into(files[i], codec, &mut buf)?;
		}
		Ok(())
	})
}

/// Archives under `Levels/<map>` for every map but `Common` (which is scanned): they load with their map.
fn level_archives(client: &Path) -> Result<Vec<PathBuf>> {
	let Some(levels) = resolve(client, "Levels") else {
		return Ok(Vec::new());
	};
	let mut out = Vec::new();
	let mut maps = Vec::new();
	for e in fs::read_dir(&levels)? {
		let e = e?;
		let p = e.path();
		if e.file_type()?.is_dir() {
			if !e.file_name().to_string_lossy().eq_ignore_ascii_case("common") {
				maps.push(p);
			}
		} else if is_pak(&p) {
			out.push(p);
		}
	}
	out.sort();
	maps.sort();
	for m in maps {
		walk(&m, &mut out)?;
	}
	Ok(out)
}

/// Decides, for one scanned folder, which archives merge and which stay. Archives in `fixed` stay as well.
pub fn plan(
	client: &Path,
	root: &Path,
	readers: &HashMap<PathBuf, PakReader>,
	fixed: &HashSet<PathBuf>,
) -> Result<Plan> {
	let order = opening_order(root)?;
	// Files that are not archives (an empty data.pak, say) stay where they are and hold no names.
	let mut stay: Vec<bool> = order
		.iter()
		.map(|p| must_stay(client, p) || !readers.contains_key(p) || fixed.contains(p))
		.collect();
	let key_sets: Vec<Vec<(Vec<u8>, usize)>> = order
		.iter()
		.map(|p| {
			readers
				.get(p)
				.map_or_else(Vec::new, |r| keys(r, p.parent().unwrap(), root))
		})
		.collect();
	loop {
		// The last opener of every name, and which archives that stay hold it.
		let mut winner: HashMap<&[u8], usize> = HashMap::new();
		for (i, ks) in key_sets.iter().enumerate() {
			for (k, _) in ks {
				winner.insert(k, i);
			}
		}
		let mut changed = false;
		for (i, ks) in key_sets.iter().enumerate() {
			if !stay[i] {
				continue;
			}
			for (k, _) in ks {
				let w = winner[&k[..]];
				if w > i && !stay[w] {
					stay[w] = true;
					changed = true;
				}
			}
		}
		if !changed {
			break;
		}
	}
	let rel = |p: &PathBuf| p.strip_prefix(client).unwrap_or(p).to_path_buf();
	Ok(Plan {
		root: rel(&root.to_path_buf()),
		merged: order
			.iter()
			.zip(&stay)
			.filter(|(_, s)| !**s)
			.map(|(p, _)| rel(p))
			.collect(),
		kept: order
			.iter()
			.zip(&stay)
			.filter(|(_, s)| **s)
			.map(|(p, _)| rel(p))
			.collect(),
	})
}

struct Pick<'a> {
	reader: &'a PakReader,
	entry: &'a Entry,
	name: Vec<u8>,
	key: Vec<u8>,
}

/// Names whose content the volumes change on purpose (`--xml`): lookup key, (CRC, size) before and after.
pub type Changes = Vec<(Vec<u8>, (u32, u32), (u32, u32))>;

/// The entries of one plan's volumes, in opening order with later archives replacing earlier ones, each keeping
/// the first position its name had.
fn picks<'a>(client: &Path, plan: &Plan, readers: &'a HashMap<PathBuf, PakReader>) -> Result<Vec<Pick<'a>>> {
	let root = client.join(&plan.root);
	// Entries in opening order with later archives replacing earlier ones, keeping the first position.
	let mut slot: HashMap<Vec<u8>, usize> = HashMap::new();
	let mut picks: Vec<Pick> = Vec::new();
	for rel in &plan.merged {
		let path = client.join(rel);
		let reader = &readers[&path];
		let bind = path.parent().unwrap().strip_prefix(&root).unwrap_or(Path::new(""));
		let mut prefix = names::path_name(bind)?;
		if !prefix.is_empty() {
			prefix.push(b'/');
		}
		for (key, i) in keys(reader, path.parent().unwrap(), &root) {
			let entry = &reader.entries()[i];
			let mut name = prefix.clone();
			name.extend_from_slice(&entry.name);
			let name = name.iter().map(|&b| if b == b'\\' { b'/' } else { b }).collect();
			let pick = Pick {
				reader,
				entry,
				name,
				key: key.clone(),
			};
			match slot.get(&key) {
				Some(&s) => picks[s] = pick,
				None => {
					slot.insert(key, picks.len());
					picks.push(pick);
				}
			}
		}
	}
	Ok(picks)
}

/// Files and stored bytes the volumes of a plan will hold, what the progress of a run counts against.
fn totals(picks: &[Pick]) -> (u64, u64) {
	(picks.len() as u64, picks.iter().map(|p| p.entry.compressed_size as u64).sum())
}

/// Writes the volumes of one plan into `out` (a folder mirroring the client) and returns their paths.
pub fn build(
	client: &Path,
	plan: &Plan,
	readers: &HashMap<PathBuf, PakReader>,
	out: &Path,
	opts: &OptimizeOptions,
	progress: &Progress,
) -> Result<(Vec<PathBuf>, Changes)> {
	let picks = picks(client, plan, readers)?;
	let out_dir = out.join(&plan.root);
	fs::create_dir_all(&out_dir)?;
	// Volume boundaries first, so all volumes go through one pipeline: work on the next volume keeps the pool busy
	// while the last entries of the previous one finish.
	let mut bounds = Vec::new();
	let mut start = 0;
	while start < picks.len() {
		let mut end = start;
		let mut size = 22u64;
		while end < picks.len() && end - start < MAX_ENTRIES {
			let cost = PakWriter::<Vec<u8>>::cost(picks[end].name.len(), picks[end].entry.compressed_size as usize);
			if end > start && size + cost > opts.volume_size {
				break;
			}
			size += cost;
			end += 1;
		}
		let path = out_dir.join(format!("{VOLUME_PREFIX}{:04}.pak", bounds.len()));
		if path.exists() {
			return Err(Error::Format(format!("{} already exists", path.display())));
		}
		bounds.push((end, path));
		start = end;
	}
	let changes = write_volumes(&picks, &bounds, opts, progress)?;
	let volumes = bounds.into_iter().map(|(_, path)| path).collect();
	Ok((volumes, changes))
}

fn prepare<'a>(p: &Pick<'a>, codecs: &CodecPool, opts: &OptimizeOptions, progress: &Progress) -> Result<Prepared<'a>> {
	let name = names::to_text(&p.name);
	let is_xml = xml::is_xml_name(&name);
	let (form, decrypt) = match opts.html {
		Some(html) if xml::is_html_name(&name) => html,
		_ => (opts.xml, opts.decrypt),
	};
	let transform_xml = form.filter(|_| is_xml);
	let decrypt = decrypt && is_xml;
	if opts.level.is_none() && transform_xml.is_none() && !decrypt {
		return Prepared::copy(p.reader, p.entry);
	}
	codecs.with(|codec| {
		let mut data = p
			.reader
			.read(p.entry, codec)
			.map_err(|e| Error::Format(format!("{}: {e}", p.reader.path().display())))?;
		if let Some(form) = transform_xml
			&& let Ok(enc) = xml::encode(&data, form, &name)
		{
			data = enc.data;
		}
		if decrypt {
			if opts.level.is_none() && transform_xml.is_none() && xml::detect(&data) != Form::Encrypted {
				return Prepared::copy(p.reader, p.entry);
			}
			data = ops::decrypt(data, &name, &p.reader.path().display().to_string(), progress);
		}
		Prepared::compress(data, codec, &name)
	})
}

/// Compiled XML grows about tenfold as text. Counting part of that keeps a few large files in work at once, which
/// is faster than one at a time and takes no more memory than before the growth was counted at all.
const XML_GROWTH: u64 = 4;

/// Roughly the memory an entry takes while it is prepared.
fn cost(p: &Pick, opts: &OptimizeOptions) -> u64 {
	let size = p.entry.size as u64;
	let html_work = opts.html.is_some_and(|(form, decrypt)| form.is_some() || decrypt);
	if (opts.xml.is_some() || opts.decrypt || html_work) && xml::is_xml_name(&names::to_text(&p.name)) {
		size * XML_GROWTH
	} else {
		size
	}
}

/// Writes the volumes, each ending before the pick index in `bounds`. A volume is written into `<path>.tmp` and
/// moved into place when complete, so a failure leaves no partial volume.
fn write_volumes(
	picks: &[Pick],
	bounds: &[(usize, PathBuf)],
	opts: &OptimizeOptions,
	progress: &Progress,
) -> Result<Changes> {
	let mut changes = Changes::new();
	let codecs = CodecPool::new(opts.level.unwrap_or(9));
	let mut volume = 0;
	let mut writer: Option<FileWriter> = None;
	let tmp = |v: usize| bounds[v].1.with_extension("pak.tmp");
	let result = ordered::run(
		picks,
		|p| cost(p, opts),
		WINDOW_BYTES,
		BIG_BYTES,
		|p| prepare(p, &codecs, opts, progress),
		Prepared::held,
		|i, r| -> Result<()> {
			if progress.cancelled() {
				return Err(Error::Cancelled);
			}
			let (p, r) = (&picks[i], r?);
			if (r.crc, r.size) != (p.entry.crc, p.entry.size) {
				changes.push((p.key.clone(), (p.entry.crc, p.entry.size), (r.crc, r.size)));
			}
			let w = match &mut writer {
				Some(w) => w,
				None => writer.insert(PakWriter::new(
					BufWriter::with_capacity(1 << 20, fs::File::create(tmp(volume))?),
					Flavor::Plain,
				)),
			};
			w.add(&WriteEntry {
				name: &p.name,
				method: r.method,
				crc: r.crc,
				size: r.size,
				prefix: r.head.as_slice(),
				data: &r.data,
				time: p.entry.time,
			})?;
			if i + 1 == bounds[volume].0 {
				writer.take().unwrap().finish()?;
				fs::rename(tmp(volume), &bounds[volume].1)?;
				volume += 1;
			}
			progress.advance(1, p.entry.compressed_size as u64);
			Ok(())
		},
	);
	if result.is_err() {
		drop(writer);
		let _ = fs::remove_file(tmp(volume));
		// Volumes finished before the failure are not in the list yet, so nothing would undo them.
		for (_, done) in &bounds[..volume] {
			let _ = fs::remove_file(done);
		}
	}
	result.map(|()| changes)
}

type Resolution = HashMap<Vec<u8>, (u32, u32)>;

/// What every name resolves to, (CRC, size), for archives opened in this order, each bound to its folder.
fn resolve_names(root: &Path, opened: &[(PathBuf, PathBuf)]) -> Result<Resolution> {
	let mut map = HashMap::new();
	for (pak, bind) in opened {
		let reader = match PakReader::open(pak) {
			Ok(r) => r,
			Err(Error::NotArchive) => continue,
			Err(e) => return Err(Error::Format(format!("{}: {e}", pak.display()))),
		};
		for (k, i) in keys(&reader, bind, root) {
			let e = &reader.entries()[i];
			map.insert(k, (e.crc, e.size));
		}
	}
	Ok(map)
}

fn bound(paks: impl IntoIterator<Item = PathBuf>) -> Vec<(PathBuf, PathBuf)> {
	paks.into_iter()
		.map(|p| (p.clone(), p.parent().unwrap().to_path_buf()))
		.collect()
}

/// Plans and builds every scanned folder of `client`. Volumes go to `out` (mirroring the client); with `apply`,
/// `out` is the client itself and the merged archives are moved into `backup`.
pub const LIST_NAME: &str = "aionpak-optimize.txt";
const LIST_HEADER: &str =
	"# volumes written and archives they replace; aionpak optimize <client> --undo <this folder> reverts it";

/// Reverts `--apply`: deletes the volumes and moves the merged archives back from `backup`.
/// Returns how many volumes were deleted and archives restored.
pub fn undo(client: &Path, backup: &Path, progress: &Progress) -> Result<(usize, usize)> {
	let list = fs::read_to_string(backup.join(LIST_NAME))
		.map_err(|e| Error::Format(format!("{}: {e}", backup.join(LIST_NAME).display())))?;
	let (mut deleted, mut restored) = (0, 0);
	let entries: Vec<(&str, &str)> = list.lines().filter_map(|l| l.split_once('\t')).collect();
	let replaced: HashSet<&str> = entries.iter().filter(|(k, _)| *k == "replaced").map(|(_, r)| *r).collect();
	for (kind, rel) in &entries {
		if *kind == "volume" {
			let v = client.join(rel);
			// An archive recompressed in place is both; without its backup copy, deleting it would lose it.
			if replaced.contains(rel) && !backup.join(rel).exists() {
				progress.error(format!("{}: no copy in the backup, left as it is", v.display()));
				continue;
			}
			match fs::remove_file(&v) {
				Ok(()) => deleted += 1,
				Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
				Err(e) => progress.error(format!("{}: {e}", v.display())),
			}
		}
	}
	for (kind, rel) in &entries {
		if *kind == "replaced" {
			let (src, dst) = (backup.join(rel), client.join(rel));
			if !src.exists() {
				continue;
			}
			if dst.exists() {
				progress.error(format!(
					"{}: exists in the client, the backup copy was left in place",
					dst.display()
				));
				continue;
			}
			match fs::create_dir_all(dst.parent().unwrap()).and_then(|_| fs::rename(&src, &dst)) {
				Ok(()) => restored += 1,
				Err(e) => progress.error(format!("{}: {e}", src.display())),
			}
		}
	}
	Ok((deleted, restored))
}

/// Whether `path` (which need not exist yet) is `dir` or lies under it.
fn inside(path: &Path, dir: &Path) -> Result<bool> {
	let dir = fs::canonicalize(dir)?;
	let absolute = std::path::absolute(path)?;
	let mut existing = absolute.as_path();
	let mut rest = Vec::new();
	while !existing.exists() {
		let Some(parent) = existing.parent() else { break };
		rest.push(existing.file_name().unwrap_or_default().to_owned());
		existing = parent;
	}
	let mut resolved = fs::canonicalize(existing)?;
	resolved.extend(rest.iter().rev());
	Ok(resolved.starts_with(&dir))
}

pub fn optimize(
	client: &Path,
	out: &Path,
	apply: Option<&Path>,
	opts: &OptimizeOptions,
	progress: &Progress,
) -> Result<Outcome> {
	if !(MIN_VOLUME_SIZE..=crate::pak::MAX_ARCHIVE_SIZE).contains(&opts.volume_size) {
		return Err(Error::Format("a volume must be between 50 MB and 4 GB".into()));
	}
	if apply.is_none() && inside(out, client)? {
		return Err(Error::Format(
			"the output folder is the client or inside it; use --apply to optimise in place".into(),
		));
	}
	let roots = scan_roots(client);
	let mut all = Vec::new();
	for r in &roots {
		all.extend(opening_order(r)?);
	}
	progress.set_current("reading archive directories");
	let opened: Vec<Result<(PathBuf, PakReader)>> = all
		.par_iter()
		.map(|p| {
			PakReader::open(p)
				.map(|r| (p.clone(), r))
				.map_err(|e| Error::Format(format!("{}: {e}", p.display())))
		})
		.collect();
	let mut readers = HashMap::new();
	for (p, r) in all.iter().zip(opened) {
		match r {
			Ok((p, r)) => {
				readers.insert(p, r);
			}
			Err(_) if matches!(PakReader::open(p), Err(Error::NotArchive)) => {
				progress.warn(format!("{}: not a zip archive, left in place", p.display()))
			}
			Err(e) => return Err(e),
		}
	}

	progress.set_current("checking that the archives decode");
	let probes = CodecPool::new(0);
	let mut undecodable: Vec<(PathBuf, String)> = readers
		.par_iter()
		.filter_map(|(p, r)| probe(r, &probes).err().map(|e| (p.clone(), e.to_string())))
		.collect();
	undecodable.sort();
	for (p, e) in &undecodable {
		progress.warn(format!(
			"{}: does not decode ({e}), likely encrypted for this client; left in place",
			p.strip_prefix(client).unwrap_or(p).display()
		));
	}
	let fixed: HashSet<PathBuf> = undecodable.into_iter().map(|(p, _)| p).collect();

	let mut outcome = Outcome {
		volumes: Vec::new(),
		replaced: Vec::new(),
		recompressed: Vec::new(),
		recompressed_bytes: (0, 0),
	};
	for r in &roots {
		for o in overrides_in(r, &readers)? {
			progress.warn(override_text(client, &o));
		}
	}
	let plans: Vec<Plan> = roots
		.iter()
		.map(|r| plan(client, r, &readers, &fixed))
		.collect::<Result<_>>()?;
	let plans: Vec<Plan> = plans.into_iter().filter(|p| p.merged.len() > 1).collect();
	// Everything not merged, to be recompressed in place, with readers taken now so the totals are known.
	let mut rest: Vec<PathBuf> = Vec::new();
	if opts.recompress_rest && opts.level.is_some() {
		let merged: HashSet<PathBuf> = plans
			.iter()
			.flat_map(|p| p.merged.iter().map(|m| client.join(m)))
			.collect();
		let candidates = all
			.iter()
			.filter(|p| !merged.contains(*p))
			.cloned()
			.chain(level_archives(client)?)
			.filter(|p| !fixed.contains(p) && !is_signed(p) && !is_volume(p));
		for p in candidates {
			if !readers.contains_key(&p) {
				match PakReader::open(&p) {
					Ok(r) => {
						readers.insert(p.clone(), r);
					}
					Err(Error::NotArchive) => continue,
					Err(e) => {
						progress.warn(format!("{}: {e}; left as it is", p.display()));
						continue;
					}
				}
			}
			rest.push(p);
		}
	}
	progress.archives_total.store((plans.len() + rest.len()) as u64, Relaxed);
	// The whole run's size up front, so the percentage and the time left do not jump back at every folder.
	for plan in &plans {
		let (files, bytes) = totals(&picks(client, plan, &readers)?);
		progress.add_total(files, bytes);
	}
	for r in rest.iter().map(|p| &readers[p]) {
		progress.add_total(
			r.entries().len() as u64,
			r.entries().iter().map(|e| e.compressed_size as u64).sum(),
		);
	}
	// Written as work happens, so a run that stops halfway can still be undone.
	let list_dir = apply.unwrap_or(out);
	fs::create_dir_all(list_dir)?;
	let mut list = fs::File::create(list_dir.join(LIST_NAME))?;
	writeln!(list, "{LIST_HEADER}")?;
	for plan in &plans {
		if progress.cancelled() {
			return Err(Error::Cancelled);
		}
		let root = client.join(&plan.root);
		progress.set_current(plan.root.display().to_string());
		let before = resolve_names(&root, &bound(opening_order(&root)?))?;
		let (volumes, changes) = build(client, plan, &readers, out, opts, progress)?;
		// The new layout: the volumes first, bound to the folder's top, then whatever stays, in its order.
		let mut layout: Vec<(PathBuf, PathBuf)> = volumes.iter().map(|v| (v.clone(), root.clone())).collect();
		layout.extend(bound(plan.kept.iter().map(|k| client.join(k))));
		let after = resolve_names(&root, &layout)?;
		// What the names should resolve to: as before, except content the volumes changed on purpose, where the
		// volume is the winner (no archive that stays holds the name).
		let kept_names: HashSet<Vec<u8>> = plan
			.kept
			.iter()
			.filter_map(|k| readers.get(&client.join(k)).map(|r| (k, r)))
			.flat_map(|(k, r)| {
				keys(r, client.join(k).parent().unwrap(), &root)
					.into_iter()
					.map(|(n, _)| n)
			})
			.collect();
		let mut expected = before.clone();
		let mut consistent = true;
		for (key, old, new) in &changes {
			if kept_names.contains(key) {
				continue;
			}
			consistent &= before.get(key) == Some(old);
			expected.insert(key.clone(), *new);
		}
		if !consistent || expected != after {
			for v in &volumes {
				let _ = fs::remove_file(v);
			}
			return Err(Error::Format(format!(
				"{}: names would resolve differently after merging, this folder was left as it was",
				plan.root.display()
			)));
		}
		for v in &volumes {
			writeln!(list, "volume\t{}", v.strip_prefix(out).unwrap_or(v).display())?;
		}
		if let Some(backup) = apply {
			for rel in &plan.merged {
				let src = client.join(rel);
				readers.remove(&src);
				let dst = backup.join(rel);
				fs::create_dir_all(dst.parent().unwrap())?;
				fs::rename(&src, &dst)?;
				writeln!(list, "replaced\t{}", rel.display())?;
			}
		} else {
			for rel in &plan.merged {
				writeln!(list, "replaced\t{}", rel.display())?;
			}
		}
		list.flush()?;
		outcome.volumes.extend(volumes);
		outcome.replaced.extend(plan.merged.iter().cloned());
		progress.archives_done.fetch_add(1, Relaxed);
	}
	for path in rest {
		if progress.cancelled() {
			return Err(Error::Cancelled);
		}
		let reader = readers.remove(&path).unwrap();
		let rel = path.strip_prefix(client).unwrap_or(&path).to_path_buf();
		progress.set_current(rel.display().to_string());
		if let Some((before, after)) = recompress(client, &rel, reader, out, apply, opts, progress, &mut list)? {
			outcome.recompressed.push(rel);
			outcome.recompressed_bytes.0 += before;
			outcome.recompressed_bytes.1 += after;
		}
		progress.archives_done.fetch_add(1, Relaxed);
	}
	Ok(outcome)
}

/// Rewrites one archive at `opts.level`, same entries in the same order, into `out` (in place with `apply`, the
/// original moving into the backup). Returns its size before and after, or `None` when it was left as it is: it did
/// not get smaller, or an entry failed to decode (a warning says which).
#[allow(clippy::too_many_arguments)]
fn recompress(
	client: &Path,
	rel: &Path,
	reader: PakReader,
	out: &Path,
	apply: Option<&Path>,
	opts: &OptimizeOptions,
	progress: &Progress,
	list: &mut fs::File,
) -> Result<Option<(u64, u64)>> {
	let level = opts.level.unwrap_or(9);
	let dst = out.join(rel);
	fs::create_dir_all(dst.parent().unwrap())?;
	let tmp = dst.with_extension("pak.tmp");
	let before = reader.size();
	let picks: Vec<Pick> = reader
		.entries()
		.iter()
		.map(|entry| Pick {
			reader: &reader,
			entry,
			name: entry.name.clone(),
			key: Vec::new(),
		})
		.collect();
	let codecs = CodecPool::new(level);
	let mut written: Vec<(u32, u32)> = Vec::with_capacity(picks.len());
	let mut changed = false;
	let mut writer = PakWriter::new(BufWriter::with_capacity(1 << 20, fs::File::create(&tmp)?), reader.flavor());
	let result = ordered::run(
		&picks,
		|p| cost(p, opts),
		WINDOW_BYTES,
		BIG_BYTES,
		|p| prepare(p, &codecs, opts, progress),
		Prepared::held,
		|i, r| -> Result<()> {
			if progress.cancelled() {
				return Err(Error::Cancelled);
			}
			let (p, r) = (&picks[i], r?);
			changed |= (r.crc, r.size) != (p.entry.crc, p.entry.size);
			writer.add(&WriteEntry {
				name: &p.name,
				method: r.method,
				crc: r.crc,
				size: r.size,
				prefix: r.head.as_slice(),
				data: &r.data,
				time: p.entry.time,
			})?;
			written.push((r.crc, r.size));
			progress.advance(1, p.entry.compressed_size as u64);
			Ok(())
		},
	)
	.and_then(|()| writer.finish().map(drop));
	// The archive as written must list the same names in the same order, with the content just written.
	let result = result.and_then(|()| {
		let new = PakReader::open(&tmp)?;
		let same = new.entries().len() == picks.len()
			&& new
				.entries()
				.iter()
				.zip(&picks)
				.zip(&written)
				.all(|((n, p), w)| n.name == p.name && (n.crc, n.size) == *w);
		if same {
			Ok(())
		} else {
			Err(Error::Format(format!("{}: the rewritten archive differs from what was written", tmp.display())))
		}
	});
	drop(picks);
	if let Err(e) = result {
		let _ = fs::remove_file(&tmp);
		if matches!(e, Error::Cancelled) {
			return Err(e);
		}
		progress.warn(format!("{}: {e}; left as it is", rel.display()));
		return Ok(None);
	}
	let after = fs::metadata(&tmp)?.len();
	if level > 0 && !changed && after >= before {
		let _ = fs::remove_file(&tmp);
		return Ok(None);
	}
	// Unmapped before it moves: Windows does not rename a mapped file.
	drop(reader);
	if let Some(backup) = apply {
		let bak = backup.join(rel);
		fs::create_dir_all(bak.parent().unwrap())?;
		fs::rename(client.join(rel), &bak)?;
	}
	// Listed before the new archive takes the old one's place, so a run that stops here can still be undone.
	writeln!(list, "replaced\t{}", rel.display())?;
	writeln!(list, "volume\t{}", rel.display())?;
	list.flush()?;
	fs::rename(&tmp, &dst)?;
	Ok(Some((before, after)))
}
