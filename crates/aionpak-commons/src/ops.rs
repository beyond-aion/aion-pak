//! Extracting one archive into a folder and packing a folder into one archive.

use crate::error::{Error, Result};
use crate::manifest::{self, Manifest, ManifestEntry, Source};
use crate::names;
use crate::ordered;
use crate::pak::{Codec, CodecPool, DosTime, Entry, Flavor, Head, METHOD_STORE, PakReader, PakWriter, WriteEntry};
use crate::progress::Progress;
use crate::xml::{self, Encoding, Form};
use rayon::prelude::*;
use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

pub type NameFilter<'a> = dyn Fn(&str) -> bool + Sync + 'a;

pub struct ExtractOptions<'a> {
	/// Turn client XML and HTML into readable text, in this encoding.
	pub decode: Option<Encoding>,
	/// Only entries whose name passes. A filtered extraction writes no manifest.
	pub filter: Option<&'a NameFilter<'a>>,
	/// Extract into a folder that already has files in it.
	pub force: bool,
}

fn is_empty_dir(dir: &Path) -> bool {
	fs::read_dir(dir).map_or(true, |mut d| d.next().is_none())
}

fn selected<'p>(pak: &'p PakReader, filter: Option<&NameFilter>) -> impl Iterator<Item = &'p Entry> {
	pak.entries()
		.iter()
		.filter(move |e| filter.is_none_or(|f| f(&names::to_text(&e.name))))
}

/// Entries and their unpacked bytes, of those that pass `filter`.
pub fn totals(pak: &PakReader, filter: Option<&NameFilter>) -> (u64, u64) {
	selected(pak, filter).fold((0, 0), |(f, b), e| (f + 1, b + e.size as u64))
}

/// Extracts every entry under `out`, keeping the archive's folders, and writes the manifest.
/// Per-entry failures are reported to `progress` and do not stop the rest.
pub fn extract(pak: &PakReader, label: &str, out: &Path, opts: &ExtractOptions, progress: &Progress) -> Result<()> {
	if !opts.force && !is_empty_dir(out) {
		return Err(Error::Format(format!(
			"{} is not empty; files left there from before would be packed with the rest (--force to extract anyway)",
			out.display()
		)));
	}
	let selected: Vec<_> = selected(pak, opts.filter).collect();
	let mut paths = Vec::with_capacity(selected.len());
	let mut dirs = BTreeSet::new();
	dirs.insert(out.to_path_buf());
	for e in &selected {
		match names::entry_path(out, &e.name) {
			Ok(p) => {
				if e.is_dir() {
					dirs.insert(p.clone());
				} else if let Some(parent) = p.parent() {
					dirs.insert(parent.to_path_buf());
				}
				paths.push(Some(p));
			}
			Err(err) => {
				progress.error(format!("{label}: {err}"));
				paths.push(None);
			}
		}
	}
	for d in &dirs {
		fs::create_dir_all(d)?;
	}

	let results: Vec<Option<ManifestEntry>> = selected
		.par_iter()
		.zip(paths.par_iter())
		.map_init(
			|| (Codec::new(0), Vec::new()),
			|(codec, buf), (e, path)| {
				if progress.cancelled() {
					return None;
				}
				let path = path.as_ref()?;
				let done = |p: &Progress| p.advance(1, e.size as u64);
				let mut entry = ManifestEntry {
					name: e.name.clone(),
					method: e.method,
					time: e.time,
					crc: 0,
					form: None,
					utf8: None,
				};
				if e.is_dir() {
					done(progress);
					return Some(entry);
				}
				let name = names::to_text(&e.name);
				let decode = opts.decode.filter(|_| xml::is_xml_name(&name));
				if decode.is_none() && e.method == METHOD_STORE {
					let written = write_stored(pak, e, path);
					done(progress);
					return match written {
						Ok(()) => {
							entry.crc = e.crc;
							Some(entry)
						}
						Err(err) => {
							progress.error(format!("{label}: {name}: {err}"));
							None
						}
					};
				}
				if let Err(err) = pak.read_into(e, codec, buf) {
					progress.error(format!("{label}: {err}"));
					done(progress);
					return None;
				}
				let mut data: &[u8] = buf;
				let decoded;
				// What was read passed the CRC check, so only decoded text needs a new CRC.
				entry.crc = e.crc;
				if let Some(enc) = decode {
					match xml::decode(buf, &name, enc) {
						Ok(d) => {
							entry.form = Some(d.form);
							entry.utf8 = d.utf8;
							decoded = d.text;
							data = &decoded;
							entry.crc = crc32fast::hash(data);
						}
						Err(err) => progress.warn(format!("{label}: {name}: left as is, {err}")),
					}
				}
				let written = fs::write(path, data);
				done(progress);
				match written {
					Ok(()) => Some(entry),
					Err(err) => {
						progress.error(format!("{label}: {name}: {err}"));
						None
					}
				}
			},
		)
		.collect();

	if opts.filter.is_none() {
		let entries = results.into_iter().flatten().collect();
		Manifest {
			flavor: pak.flavor(),
			source: Source::of(pak.path()),
			entries,
		}
		.save(out)?;
	}
	Ok(())
}

/// Writes a stored entry straight from the archive, checking its CRC on the way.
fn write_stored(pak: &PakReader, e: &Entry, path: &Path) -> Result<()> {
	let (head, tail) = pak.split(e)?;
	let head = head.as_slice();
	if head.len() + tail.len() != e.size as usize {
		return Err(Error::Format("stored entry with differing sizes".into()));
	}
	let mut crc = crc32fast::Hasher::new();
	crc.update(head);
	crc.update(tail);
	let crc = crc.finalize();
	if crc != e.crc {
		return Err(Error::Format(format!("CRC {crc:08x}, expected {:08x}", e.crc)));
	}
	let mut file = fs::File::create(path)?;
	file.write_all(head)?;
	file.write_all(tail)?;
	Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum XmlMode {
	/// Back to the form recorded in the manifest; files without one keep theirs.
	Original,
	/// Bytes exactly as on disk.
	Keep,
	/// Every XML and HTML file into this form.
	To(Form),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlavorMode {
	Plain,
	Aion,
	Original,
}

pub struct PakOptions {
	pub level: u8,
	pub xml: XmlMode,
	pub flavor: FlavorMode,
	/// Replace an existing archive with a folder that has no manifest.
	pub force: bool,
	/// Compress every file, also those unchanged since extraction (otherwise their data is copied from the archive
	/// they came from, while it is still there unchanged).
	pub recompress: bool,
	/// Store as UTF-16 text every XML and HTML file that would end up encrypted.
	pub decrypt: bool,
	/// HTML files on their own terms: their mode, and whether the encrypted ones become text. `None`: as the XML
	/// files, by `xml` and `decrypt`.
	pub html: Option<(XmlMode, bool)>,
}

impl PakOptions {
	/// The mode for one XML or HTML file, and whether it is decrypted if it ends up encrypted.
	pub(crate) fn mode_for(&self, name: &str) -> (XmlMode, bool) {
		match self.html {
			Some(html) if xml::is_html_name(name) => html,
			_ => (self.xml, self.decrypt),
		}
	}
}

/// Input bytes being compressed plus output bytes waiting for their turn to be written (see `ordered::run`).
pub(crate) const WINDOW_BYTES: u64 = 256 << 20;
/// Files this large start first: a single one takes as long to compress as many small ones together.
pub(crate) const BIG_BYTES: u64 = 16 << 20;

struct Item {
	name: Vec<u8>,
	path: Option<PathBuf>,
	record: Option<ManifestEntry>,
	size: u64,
}

/// An entry ready to be written: data compressed anew, or copied as an archive stores it.
pub(crate) struct Prepared<'a> {
	pub method: u16,
	pub crc: u32,
	pub size: u32,
	pub head: Head,
	pub data: Cow<'a, [u8]>,
}

impl<'a> Prepared<'a> {
	fn empty() -> Self {
		Prepared {
			method: 0,
			crc: 0,
			size: 0,
			head: Head::default(),
			data: Cow::Borrowed(&[]),
		}
	}

	pub fn copy(reader: &'a PakReader, e: &Entry) -> Result<Self> {
		let (head, rest) = reader.split(e)?;
		Ok(Prepared {
			method: e.method,
			crc: e.crc,
			size: e.size,
			head,
			data: Cow::Borrowed(rest),
		})
	}

	/// Memory a finished entry holds until it is written: compressed data, not data borrowed from an archive.
	pub fn held(r: &Result<Self>) -> u64 {
		match r {
			Ok(Prepared {
				data: Cow::Owned(d), ..
			}) => d.len() as u64,
			_ => 0,
		}
	}

	pub fn compress(data: Vec<u8>, codec: &mut Codec, name: &str) -> Result<Self> {
		let size = u32::try_from(data.len()).map_err(|_| Error::Format(format!("{name}: larger than 4 GiB")))?;
		let crc = crc32fast::hash(&data);
		let (method, data) = codec.compress(data);
		Ok(Prepared {
			method,
			crc,
			size,
			head: Head::default(),
			data: Cow::Owned(data),
		})
	}
}

pub(crate) type FileWriter = PakWriter<BufWriter<fs::File>>;

/// Writes an archive into `<path>.tmp` and moves it into place, so a failure leaves the old archive untouched.
pub(crate) fn write_archive(
	path: &Path,
	flavor: Flavor,
	fill: impl FnOnce(&mut FileWriter) -> Result<()>,
) -> Result<()> {
	let tmp = path.with_extension("pak.tmp");
	let result = (|| {
		let mut w = PakWriter::new(BufWriter::with_capacity(1 << 20, fs::File::create(&tmp)?), flavor);
		fill(&mut w)?;
		w.finish()?;
		Ok(())
	})();
	match result {
		Ok(()) => Ok(fs::rename(&tmp, path)?),
		Err(e) => {
			let _ = fs::remove_file(&tmp);
			Err(e)
		}
	}
}

/// Entries of the archive a folder was extracted from, by name; names it holds twice are left out.
struct Original<'a> {
	reader: &'a PakReader,
	by_name: HashMap<&'a [u8], Option<&'a Entry>>,
}

impl<'a> Original<'a> {
	fn new(reader: &'a PakReader) -> Self {
		let mut by_name = HashMap::new();
		for e in reader.entries() {
			by_name.entry(&e.name[..]).and_modify(|v| *v = None).or_insert(Some(e));
		}
		Original { reader, by_name }
	}

	/// The archive's data for a file that `pack` would turn back into exactly that entry: unchanged since extraction
	/// (`data` is what the folder holds now) and packed into the form it had.
	fn unchanged(&self, record: &ManifestEntry, data: &[u8], mode: XmlMode, name: &str) -> Option<Prepared<'a>> {
		let e = (*self.by_name.get(&record.name[..])?)?;
		let decoded = record.form.is_some() || record.utf8.is_some();
		// A form the client cannot read for this file falls back to the original one, as in `transform_xml`.
		let mode = match mode {
			XmlMode::To(form) if !xml::accepts(name, form) => XmlMode::Original,
			m => m,
		};
		// Asked for the form the archive already stores it in: compiling an unchanged compiled file gives it back.
		let stored = record.form.unwrap_or_else(|| xml::detect(data));
		let same_form = !xml::is_xml_name(name)
			|| mode == XmlMode::Original
			|| (mode == XmlMode::Keep && !decoded)
			|| mode == XmlMode::To(stored);
		if !same_form || e.method != record.method || e.time != record.time || (!decoded && e.crc != record.crc) {
			return None;
		}
		if crc32fast::hash(data) != record.crc {
			return None;
		}
		Prepared::copy(self.reader, e).ok()
	}
}

fn walk_files(dir: &Path) -> Result<Vec<(PathBuf, u64)>> {
	let mut files = Vec::new();
	for e in walkdir::WalkDir::new(dir).min_depth(1).sort_by_file_name() {
		let e = e.map_err(|e| Error::Io(e.into()))?;
		if e.file_type().is_file() && !(e.depth() == 1 && e.file_name() == manifest::FILE_NAME) {
			let size = e.metadata().map_err(|e| Error::Io(e.into()))?.len();
			files.push((e.into_path(), size));
		}
	}
	Ok(files)
}

fn plan(dir: &Path, manifest: Option<&Manifest>, label: &str, progress: &Progress) -> Result<Vec<Item>> {
	let mut on_disk: HashMap<PathBuf, u64> = walk_files(dir)?.into_iter().collect();
	let mut items = Vec::new();
	for m in manifest.map(|m| &m.entries[..]).unwrap_or(&[]) {
		let is_dir = matches!(m.name.last(), Some(b'/' | b'\\'));
		let path = names::entry_path(dir, &m.name)?;
		if is_dir {
			if path.is_dir() {
				items.push(Item {
					name: m.name.clone(),
					path: None,
					record: Some(m.clone()),
					size: 0,
				});
			}
			continue;
		}
		match on_disk.remove(&path) {
			Some(size) => items.push(Item {
				name: m.name.clone(),
				path: Some(path),
				record: Some(m.clone()),
				size,
			}),
			None => progress.warn(format!("{label}: {} is gone, dropped", names::to_text(&m.name))),
		}
	}
	let mut extra: Vec<_> = on_disk.into_iter().collect();
	extra.sort();
	for (path, size) in extra {
		let rel = path.strip_prefix(dir).unwrap_or(&path);
		items.push(Item {
			name: names::path_name(rel)?,
			path: Some(path),
			record: None,
			size,
		});
	}
	Ok(items)
}

/// An encrypted XML or HTML file as UTF-16 text, the same text without the name-keyed scrambling; any other data
/// as it is.
///
/// Some encrypted files hold 8-bit text in a code page (Korean leftovers in European clients are CP949, whatever
/// their XML declaration says), which cannot become UTF-16 without guessing the code page. Those are stored as the
/// decrypted bytes, unchanged: the client reads plain 8-bit text, and gets exactly what it would have decrypted.
pub(crate) fn decrypt(data: Vec<u8>, name: &str, label: &str, progress: &Progress) -> Vec<u8> {
	if xml::detect(&data) != Form::Encrypted {
		return data;
	}
	match xml::encode(&data, Form::Utf16, name) {
		Ok(enc) => {
			progress.decrypted.fetch_add(1, Relaxed);
			enc.data
		}
		Err(err) => {
			let plain = xml::crypt::decrypt(&data, xml::crypt::stem(name)).filter(|_| xml::accepts(name, Form::Text));
			match plain {
				Some(plain) => {
					progress.decrypted.fetch_add(1, Relaxed);
					plain
				}
				None => {
					progress.warn(format!("{label}: {name}: left encrypted, {err}"));
					data
				}
			}
		}
	}
}

fn transform(
	data: Vec<u8>,
	name: &str,
	record: Option<&ManifestEntry>,
	opts: &PakOptions,
	label: &str,
	progress: &Progress,
) -> Vec<u8> {
	if !xml::is_xml_name(name) {
		return data;
	}
	let (mode, decrypt_it) = opts.mode_for(name);
	let data = transform_xml(data, name, record, mode, label, progress);
	if decrypt_it {
		decrypt(data, name, label, progress)
	} else {
		data
	}
}

fn transform_xml(
	data: Vec<u8>,
	name: &str,
	record: Option<&ManifestEntry>,
	mode: XmlMode,
	label: &str,
	progress: &Progress,
) -> Vec<u8> {
	if mode == XmlMode::Keep {
		return data;
	}
	let edited = record.is_none_or(|r| r.crc != crc32fast::hash(&data));
	// A file the client cannot read in the requested form goes back to the form it had, as with `original`.
	let mode = match mode {
		XmlMode::To(form) if !xml::accepts(name, form) => XmlMode::Original,
		m => m,
	};
	let result = match (mode, record.and_then(|r| r.form.map(|f| (f, r.utf8.as_ref())))) {
		(XmlMode::To(form), _) => xml::encode(&data, form, name),
		(_, Some((form, utf8))) => xml::restore(&data, form, utf8, name, edited),
		(_, None) => xml::restore(&data, xml::detect(&data), None, name, edited),
	};
	match result {
		Ok(enc) => {
			if enc.converted {
				progress.warn(format!(
					"{label}: {name}: UTF-8 text stored as UTF-16, the client reads 8-bit text in the system code page"
				));
			}
			enc.data
		}
		Err(err) => {
			progress.warn(format!("{label}: {name}: stored as is, {err}"));
			data
		}
	}
}

/// A folder ready to be packed: its manifest and the entries the archive will hold, in order.
pub struct PakPlan {
	dir: PathBuf,
	out: PathBuf,
	manifest: Option<Manifest>,
	items: Vec<Item>,
}

impl PakPlan {
	/// Files and bytes the archive will take in, what the progress counts against.
	pub fn totals(&self) -> (u64, u64) {
		(self.items.len() as u64, self.items.iter().map(|i| i.size).sum())
	}
}

/// Packs `dir` into `out`. Entries recorded in the folder's manifest come first, in their order and with their
/// names and times; files without a record follow, sorted.
pub fn pak(dir: &Path, out: &Path, opts: &PakOptions, progress: &Progress) -> Result<()> {
	let plan = plan_pak(dir, out, opts, progress)?;
	let (files, bytes) = plan.totals();
	progress.add_total(files, bytes);
	write_pak(plan, opts, progress)
}

/// The first half of [`pak`]: reads the manifest and lists the entries, without writing anything, so a caller
/// packing many folders can count all of them before the first is written.
pub fn plan_pak(dir: &Path, out: &Path, opts: &PakOptions, progress: &Progress) -> Result<PakPlan> {
	let label = out.display().to_string();
	let manifest = Manifest::load(dir)?;
	if manifest.is_none() && out.exists() && !opts.force {
		return Err(Error::Format(format!(
			"{} has no manifest, so it may hold only part of the archive it would replace (--force to replace it anyway)",
			dir.display()
		)));
	}
	let items = plan(dir, manifest.as_ref(), &label, progress)?;
	Ok(PakPlan {
		dir: dir.to_path_buf(),
		out: out.to_path_buf(),
		manifest,
		items,
	})
}

/// The second half of [`pak`]: writes the planned archive. The progress must already count the plan.
pub fn write_pak(plan: PakPlan, opts: &PakOptions, progress: &Progress) -> Result<()> {
	let PakPlan {
		dir,
		out,
		manifest,
		items,
	} = plan;
	let (dir, out) = (dir.as_path(), out.as_path());
	let label = out.display().to_string();
	let flavor = match opts.flavor {
		FlavorMode::Plain => Flavor::Plain,
		FlavorMode::Aion => Flavor::Aion,
		FlavorMode::Original => manifest.as_ref().map_or(Flavor::Plain, |m| m.flavor),
	};

	if let Some(parent) = out.parent() {
		fs::create_dir_all(parent)?;
	}
	write_archive(out, flavor, |writer| {
		let codecs = CodecPool::new(opts.level);
		// Opened in here, so the archive is closed again before the result may replace it.
		let source = manifest
			.as_ref()
			.and_then(|m| m.source.as_ref())
			.filter(|_| !opts.recompress)
			.and_then(Source::open);
		let original = source.as_ref().map(Original::new);
		let work = |item: &Item| -> Result<Prepared> {
			if progress.cancelled() {
				return Ok(Prepared::empty());
			}
			let Some(path) = &item.path else {
				return Ok(Prepared::empty());
			};
			let name = names::to_text(&item.name);
			let data = fs::read(path)?;
			let unchanged = match (&original, &item.record) {
				// The archive's copy is encrypted, which `--decrypt` replaces.
				(_, Some(r))
					if opts.mode_for(&name).1
						&& xml::is_xml_name(&name)
						&& r.form.unwrap_or_else(|| xml::detect(&data)) == Form::Encrypted =>
				{
					None
				}
				(Some(original), Some(record)) => original.unchanged(record, &data, opts.mode_for(&name).0, &name),
				_ => None,
			};
			let prepared = match unchanged {
				Some(p) => p,
				None => {
					let data = transform(data, &name, item.record.as_ref(), opts, &label, progress);
					codecs.with(|c| Prepared::compress(data, c, &name))?
				}
			};
			progress.advance(1, item.size);
			Ok(prepared)
		};
		ordered::run(
			&items,
			|i| i.size,
			WINDOW_BYTES,
			BIG_BYTES,
			work,
			Prepared::held,
			|i, p| -> Result<()> {
				if progress.cancelled() {
					return Err(Error::Cancelled);
				}
				let p = p?;
				let item = &items[i];
				let time = match &item.record {
					Some(r) => r.time,
					None => item
						.path
						.as_ref()
						.and_then(|p| fs::metadata(p).and_then(|m| m.modified()).ok())
						.map_or_else(crate::pak::dos_now, DosTime::from_system),
				};
				writer.add(&WriteEntry {
					name: &item.name,
					method: p.method,
					crc: p.crc,
					size: p.size,
					prefix: p.head.as_slice(),
					data: &p.data,
					time,
				})
			},
		)
	})?;
	// Packed back over the archive it came from: the next pack copies from the new one. Files changed since
	// extraction still differ from their records, so they are compressed again rather than copied.
	if let Some(mut m) = manifest
		&& let Some(src) = &m.source
		&& std::path::absolute(out).is_ok_and(|o| o == src.path)
	{
		m.source = Source::of(out);
		m.save(dir)?;
	}
	Ok(())
}

/// Which way a loose XML or HTML file is converted.
#[derive(Clone, Copy)]
pub enum Convert {
	/// Into readable text.
	Decode(Encoding),
	/// Into a form the client reads: XML files into `xml`, HTML files into `html`, or left as they are without
	/// one. HTML cannot be compiled, so one form for both would fail every HTML file whenever XML is compiled.
	/// A file the client does not read in its form (level files compiled or encrypted) is left as it is, as
	/// packing does.
	Encode { xml: Form, html: Option<Form> },
}

/// Converts loose XML and HTML files, one file or a whole folder, in place or into `out`.
///
/// A folder is walked recursively and only files whose name looks like XML or HTML are touched; with `out`
/// the folder layout is mirrored under it. One bad file is reported and the rest still run.
pub fn convert(input: &Path, out: Option<&Path>, mode: Convert, progress: &Progress) -> Result<()> {
	let files: Vec<PathBuf> = if input.is_dir() {
		let mut files = Vec::new();
		for entry in walkdir::WalkDir::new(input).sort_by_file_name() {
			let entry = entry.map_err(|e| Error::Io(e.into()))?;
			if entry.file_type().is_file() && xml::is_xml_name(&entry.path().to_string_lossy()) {
				files.push(entry.path().to_path_buf());
			}
		}
		files
	} else {
		vec![input.to_path_buf()]
	};
	let sizes: u64 = files
		.iter()
		.filter_map(|path| fs::metadata(path).ok())
		.map(|meta| meta.len())
		.sum();
	progress.add_total(files.len() as u64, sizes);
	files.par_iter().for_each(|path| {
		let target = match out {
			None => path.clone(),
			Some(dir) if input.is_dir() => dir.join(path.strip_prefix(input).unwrap_or(path)),
			Some(file) => file.to_path_buf(),
		};
		let name = path
			.file_name()
			.map(|name| name.to_string_lossy().into_owned())
			.unwrap_or_default();
		let result = (|| -> Result<u64> {
			let data = fs::read(path)?;
			let read = data.len() as u64;
			let converted = match mode {
				Convert::Decode(encoding) => xml::decode(&data, &name, encoding)?.text,
				Convert::Encode { xml: xml_form, html } => {
					let form = if xml::is_html_name(&name) { html } else { Some(xml_form) };
					match form.filter(|&form| xml::accepts(&name, form)) {
						None if target == *path => return Ok(read),
						None => data,
						Some(form) => {
							let encoded = xml::encode(&data, form, &name)?;
							if encoded.converted {
								progress.warn(format!("{name}: UTF-8 text stored as UTF-16"));
							}
							encoded.data
						}
					}
				}
			};
			if let Some(parent) = target.parent() {
				fs::create_dir_all(parent)?;
			}
			fs::write(&target, converted)?;
			Ok(read)
		})();
		match result {
			Ok(bytes) => progress.advance(1, bytes),
			Err(error) => {
				progress.error(format!("{}: {error}", path.display()));
				progress.advance(1, 0);
			}
		}
	});
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn decrypts_code_page_text_as_its_bytes() {
		// CP949 for a Korean word, which is neither UTF-8 nor UTF-16.
		let plain = b"<?xml version=\"1.0\" encoding=\"UTF-16\" ?>\r\n<HtmlPages><p>\xc3\xd6\xb0\xed</p></HtmlPages>\r\n".to_vec();
		let name = "ldf5b_lv19_healer.html";
		let encrypted = xml::crypt::encrypt(&plain, xml::crypt::stem(name));
		assert_eq!(xml::detect(&encrypted), Form::Encrypted);
		let progress = Progress::default();
		let out = decrypt(encrypted, name, "test.pak", &progress);
		assert_eq!(out, plain);
		assert_eq!(progress.problem_count(), 0);
	}

	#[test]
	fn encodes_xml_and_html_each_into_its_own_form() {
		let dir = std::env::temp_dir().join(format!("aionpak-convert-{}", std::process::id()));
		let _ = fs::remove_dir_all(&dir);
		fs::create_dir_all(&dir).unwrap();
		let xml_text = b"<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\r\n<skills><skill id=\"1\"/></skills>\r\n";
		let html_text = b"<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\r\n<HtmlPages><p>hi</p></HtmlPages>\r\n";
		let level_text = b"<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\r\n<LevelData/>\r\n";
		let (xml_path, html_path) = (dir.join("skills.xml"), dir.join("page.html"));
		let level_path = dir.join("LevelData.xml");
		let reset = || {
			fs::write(&xml_path, xml_text).unwrap();
			fs::write(&html_path, html_text).unwrap();
			fs::write(&level_path, level_text).unwrap();
		};

		// Compiled XML used to fail every HTML file beside it; left alone, they are not touched.
		reset();
		let progress = Progress::default();
		convert(&dir, None, Convert::Encode { xml: Form::Compiled, html: None }, &progress).unwrap();
		assert_eq!(progress.problem_count(), 0);
		assert_eq!(xml::detect(&fs::read(&xml_path).unwrap()), Form::Compiled);
		assert_eq!(fs::read(&html_path).unwrap(), html_text);
		// Level files cannot be compiled: left as they are, as packing does, not failed.
		assert_eq!(fs::read(&level_path).unwrap(), level_text);

		reset();
		let progress = Progress::default();
		let mode = Convert::Encode { xml: Form::Compiled, html: Some(Form::Encrypted) };
		convert(&dir, None, mode, &progress).unwrap();
		assert_eq!(progress.problem_count(), 0);
		assert_eq!(xml::detect(&fs::read(&xml_path).unwrap()), Form::Compiled);
		assert_eq!(xml::detect(&fs::read(&html_path).unwrap()), Form::Encrypted);

		// Written elsewhere, an HTML file left alone is still copied, so the output holds the whole folder.
		reset();
		let out = dir.with_extension("out");
		let _ = fs::remove_dir_all(&out);
		let progress = Progress::default();
		convert(&dir, Some(&out), Convert::Encode { xml: Form::Utf16, html: None }, &progress).unwrap();
		assert_eq!(fs::read(out.join("page.html")).unwrap(), html_text);
		assert_eq!(xml::detect(&fs::read(out.join("skills.xml")).unwrap()), Form::Utf16);

		let _ = fs::remove_dir_all(&dir);
		let _ = fs::remove_dir_all(&out);
	}
}
