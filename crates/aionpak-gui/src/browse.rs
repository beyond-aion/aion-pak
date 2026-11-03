//! Reading one archive into rows the table can draw, and the filter over them.

use aionpak_commons::pak::{DosTime, Flavor, PakReader};
use aionpak_commons::xml::{self, Form};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};

pub struct Archive {
	pub path: PathBuf,
	pub reader: Arc<PakReader>,
	pub rows: Vec<Row>,
	pub flavor: &'static str,
	pub total_size: u64,
	pub total_packed: u64,
	pub dirs: u64,
	/// One [`Stored`] code per row, filled in by the form scan after the archive opened.
	pub forms: Arc<Vec<AtomicU8>>,
	/// Rows the form scan has to read, with their names.
	pub xml_entries: Vec<(usize, String)>,
}

pub struct Row {
	pub name: String,
	/// The name with `/` separators, as the folder view splits it.
	pub path: String,
	/// The path in lower case, so filtering and folder lookups do not allocate on each keystroke.
	pub name_lower: String,
	pub size: u32,
	pub packed: u32,
	pub crc: u32,
	pub date: String,
	pub is_dir: bool,
}

impl Archive {
	/// The archive's file name, for labels and job titles.
	pub fn file_name(&self) -> String {
		self.path
			.file_name()
			.map(|name| name.to_string_lossy().into_owned())
			.unwrap_or_else(|| self.path.display().to_string())
	}

	pub fn ratio(&self) -> f32 {
		if self.total_size == 0 {
			return 0.0;
		}
		self.total_packed as f32 / self.total_size as f32
	}

	pub fn form(&self, index: usize) -> Stored {
		Stored::from_code(self.forms[index].load(Relaxed))
	}
}

/// What the form scan learned about an entry. Kept as a `u8` in an `AtomicU8` so the scan can fill the table
/// from worker threads while it is drawn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Stored {
	/// Not read yet.
	Unknown = 0,
	NotXml = 1,
	Compiled = 2,
	Encrypted = 3,
	Utf16 = 4,
	Text = 5,
	/// 8-bit text that happens to be valid UTF-8 with non-ASCII bytes, which the client would misread.
	Utf8 = 6,
}

impl Stored {
	pub fn from_code(code: u8) -> Stored {
		match code {
			1 => Stored::NotXml,
			2 => Stored::Compiled,
			3 => Stored::Encrypted,
			4 => Stored::Utf16,
			5 => Stored::Text,
			6 => Stored::Utf8,
			_ => Stored::Unknown,
		}
	}

	pub fn from_form(form: Form, data: &[u8]) -> Stored {
		match form {
			Form::Compiled => Stored::Compiled,
			Form::Encrypted => Stored::Encrypted,
			Form::Utf16 => Stored::Utf16,
			Form::Text if std::str::from_utf8(data).is_ok() && data.iter().any(|byte| *byte >= 0x80) => Stored::Utf8,
			Form::Text => Stored::Text,
		}
	}

	/// Compiled and encrypted XML is unreadable in an editor until it is decoded.
	pub fn needs_decode(self) -> bool {
		matches!(self, Stored::Compiled | Stored::Encrypted)
	}

	pub fn name(self) -> &'static str {
		match self {
			Stored::Compiled => "compiled",
			Stored::Encrypted => "encrypted",
			Stored::Utf16 => "utf16",
			Stored::Text => "text",
			Stored::Utf8 => "utf8",
			Stored::Unknown | Stored::NotXml => "",
		}
	}

	/// What the stored form means for the client and for other tools.
	pub fn hint(self) -> &'static str {
		match self {
			Stored::Compiled => "Binary XML with a string pool: the fastest for the client to load.",
			Stored::Encrypted => {
				"Text scrambled with a key taken from the file name, so renaming the file breaks it. Pack can store \
				 it as UTF-16 text instead."
			}
			Stored::Utf16 => "UTF-16 text: the client reads it, and so does every other tool.",
			Stored::Text => "8-bit text in the system code page.",
			Stored::Utf8 => {
				"UTF-8 text. The client reads it as the system code page, so non-ASCII characters may come out wrong."
			}
			Stored::Unknown => "Not read yet.",
			Stored::NotXml => "Not an XML or HTML file, so it has no form.",
		}
	}
}

pub fn load(path: &Path) -> Result<Archive, String> {
	let reader = PakReader::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
	let flavor = match reader.flavor() {
		Flavor::Plain => "plain zip",
		Flavor::Aion => "aion zip",
	};
	let (mut total_size, mut total_packed, mut dirs) = (0u64, 0u64, 0u64);
	let mut rows = Vec::with_capacity(reader.entries().len());
	let mut forms = Vec::with_capacity(reader.entries().len());
	let mut xml_entries = Vec::new();
	for entry in reader.entries() {
		let is_dir = entry.is_dir();
		if is_dir {
			dirs += 1;
		} else {
			total_size += entry.size as u64;
			total_packed += entry.compressed_size as u64;
		}
		let name = entry.name_lossy().into_owned();
		let index = rows.len();
		if !is_dir && xml::is_xml_name(&name) {
			forms.push(AtomicU8::new(Stored::Unknown as u8));
			xml_entries.push((index, name.clone()));
		} else {
			forms.push(AtomicU8::new(Stored::NotXml as u8));
		}
		rows.push(Row {
			path: name.replace('\\', "/"),
			name_lower: name.replace('\\', "/").to_lowercase(),
			name,
			size: entry.size,
			packed: entry.compressed_size,
			crc: entry.crc,
			date: date_of(entry.time),
			is_dir,
		});
	}
	Ok(Archive {
		path: path.to_path_buf(),
		reader: Arc::new(reader),
		rows,
		flavor,
		total_size,
		total_packed,
		dirs,
		forms: Arc::new(forms),
		xml_entries,
	})
}

/// A folder inside an archive, as one row of the folder view.
#[derive(Clone)]
pub struct Folder {
	/// The lower-case path with a trailing `/`, which identifies the folder.
	pub key: String,
	/// The folder's own name as the archive spells it.
	pub name: String,
	pub files: u64,
	pub size: u64,
	pub packed: u64,
}

/// What one folder of an archive holds: its subfolders, sorted by name, and its files, in archive order.
/// `prefix` is a folder key, or empty for the top.
pub fn listing(rows: &[Row], prefix: &str) -> (Vec<Folder>, Vec<usize>) {
	let depth = prefix.matches('/').count();
	let mut folders: Vec<Folder> = Vec::new();
	let mut slots: std::collections::HashMap<&str, usize> = Default::default();
	let mut files = Vec::new();
	for (index, row) in rows.iter().enumerate() {
		let Some(rest) = row.name_lower.strip_prefix(prefix) else {
			continue;
		};
		if rest.is_empty() {
			continue;
		}
		match rest.find('/') {
			Some(end) => {
				let segment = &rest[..end];
				let slot = *slots.entry(segment).or_insert_with(|| {
					folders.push(Folder {
						key: format!("{prefix}{segment}/"),
						name: row.path.split('/').nth(depth).unwrap_or(segment).to_owned(),
						files: 0,
						size: 0,
						packed: 0,
					});
					folders.len() - 1
				});
				if !row.is_dir {
					let folder = &mut folders[slot];
					folder.files += 1;
					folder.size += row.size as u64;
					folder.packed += row.packed as u64;
				}
			}
			None if !row.is_dir => files.push(index),
			None => {}
		}
	}
	folders.sort_by(|a, b| a.key.cmp(&b.key));
	(folders, files)
}

/// The rows inside a folder, at any depth.
pub fn under<'a>(rows: &'a [Row], key: &'a str) -> impl Iterator<Item = usize> + 'a {
	rows.iter()
		.enumerate()
		.filter(move |(_, row)| !row.is_dir && row.name_lower.starts_with(key))
		.map(|(index, _)| index)
}

/// The key of the folder a row sits in, empty for the top.
pub fn parent_key(row: &Row) -> String {
	row.name_lower
		.trim_end_matches('/')
		.rsplit_once('/')
		.map(|(parent, _)| format!("{parent}/"))
		.unwrap_or_default()
}

/// The names of the folders along `key`, spelled as the archive spells them, for the breadcrumb.
pub fn folder_names(rows: &[Row], key: &str) -> Vec<String> {
	let depth = key.matches('/').count();
	match rows.iter().find(|row| row.name_lower.starts_with(key)) {
		Some(row) => row.path.split('/').take(depth).map(str::to_owned).collect(),
		None => key.split('/').filter(|part| !part.is_empty()).map(str::to_owned).collect(),
	}
}

/// Indexes of the rows matching `query`, in archive order. Without wildcards the query is a case-insensitive
/// substring, so `npc` finds every name that contains it. With `*` or `?` it is a pattern over the whole file
/// name (with or without its extension), or over the whole path when it contains a `/`: `*.xml` takes the XML
/// files and `mission_*` the files whose name starts with it, wherever they sit.
pub fn filter(rows: &[Row], query: &str) -> Vec<usize> {
	let query = query.trim().to_lowercase();
	if query.is_empty() {
		return (0..rows.len()).collect();
	}
	rows.iter()
		.enumerate()
		.filter(|(_, row)| matches(&query, &row.name_lower))
		.map(|(i, _)| i)
		.collect()
}

pub fn matches(query: &str, name: &str) -> bool {
	if !query.contains(['*', '?']) {
		return name.contains(query);
	}
	if query.contains('/') {
		return glob(query.as_bytes(), name.as_bytes());
	}
	// The file name; and when the pattern names no extension, also the name without one, so `quest_q100?`
	// finds QUEST_Q1000.html while `*.xml` still skips x.xml.bak.
	let file = name.rsplit('/').next().unwrap_or(name);
	if glob(query.as_bytes(), file.as_bytes()) {
		return true;
	}
	!query.contains('.') && file.rsplit_once('.').is_some_and(|(stem, _)| glob(query.as_bytes(), stem.as_bytes()))
}

/// `*` matches any run of characters, `?` exactly one. Backtracks to the last star only, so it stays linear
/// for the patterns people type.
fn glob(pattern: &[u8], text: &[u8]) -> bool {
	let (mut p, mut t) = (0, 0);
	let mut star: Option<(usize, usize)> = None;
	while t < text.len() {
		if p < pattern.len() && (pattern[p] == b'?' || pattern[p] == text[t]) {
			p += 1;
			t += 1;
		} else if p < pattern.len() && pattern[p] == b'*' {
			star = Some((p, t));
			p += 1;
		} else if let Some((star_p, star_t)) = star {
			p = star_p + 1;
			t = star_t + 1;
			star = Some((star_p, star_t + 1));
		} else {
			return false;
		}
	}
	pattern[p..].iter().all(|byte| *byte == b'*')
}

fn date_of(time: DosTime) -> String {
	let year = 1980 + (time.date >> 9);
	let month = (time.date >> 5) & 0x0f;
	let day = time.date & 0x1f;
	let hour = time.time >> 11;
	let minute = (time.time >> 5) & 0x3f;
	format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

#[cfg(test)]
mod tests {
	use super::{Row, listing, matches, parent_key, under};

	fn row(name: &str, size: u32) -> Row {
		Row {
			name: name.to_owned(),
			path: name.replace('\\', "/"),
			name_lower: name.replace('\\', "/").to_lowercase(),
			size,
			packed: size / 2,
			crc: 0,
			date: String::new(),
			is_dir: name.ends_with('/'),
		}
	}

	#[test]
	fn folders_group_their_files() {
		let rows = vec![
			row("Client/", 0),
			row("Client/ui/a.dds", 10),
			row("client/b.xml", 20),
			row("top.xml", 5),
			row("CGFData\\x.cgf", 7),
		];
		let (folders, files) = listing(&rows, "");
		assert_eq!(files, vec![3]);
		let names: Vec<_> = folders.iter().map(|f| (f.key.as_str(), f.name.as_str(), f.files, f.size)).collect();
		assert_eq!(names, vec![("cgfdata/", "CGFData", 1, 7), ("client/", "Client", 2, 30)]);
		let (folders, files) = listing(&rows, "client/");
		assert_eq!(files, vec![2]);
		assert_eq!(folders[0].key, "client/ui/");
		assert_eq!(under(&rows, "client/").collect::<Vec<_>>(), vec![1, 2]);
		assert_eq!(parent_key(&rows[1]), "client/ui/");
		assert_eq!(parent_key(&rows[3]), "");
	}

	#[test]
	fn plain_text_is_a_substring() {
		assert!(matches("npc", "objects/npc/pet.cgf"));
		assert!(!matches("npc", "objects/item.cgf"));
	}

	#[test]
	fn patterns_cover_the_whole_file_name() {
		assert!(matches("*.xml", "data/skills/skill_base.xml"));
		assert!(!matches("*.xml", "data/skills/skill_base.xml.bak"));
		assert!(matches("mission_*", "quest/mission_0001.xml"));
		assert!(!matches("mission_*", "quest/sub_mission_0001.xml"));
		assert!(matches("item_?.dds", "ui/item_7.dds"));
		assert!(!matches("item_?.dds", "ui/item_77.dds"));
		// Without an extension in the pattern, the name without its extension counts too.
		assert!(matches("quest_q100?", "dialogs/quest_q1000.html"));
		assert!(!matches("quest_q100?", "dialogs/quest_q10000.html"));
	}

	#[test]
	fn a_slash_matches_against_the_path() {
		assert!(matches("data/*/skill*", "data/skills/skill_base.xml"));
		assert!(!matches("data/*/skill*", "levels/skills/skill_base.xml"));
	}
}
