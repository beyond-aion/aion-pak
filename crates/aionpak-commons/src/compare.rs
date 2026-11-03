//! Whether two archives, or two trees of archives, hold the same files: names, order, sizes and CRCs. The data
//! itself is not read; `verify` checks that it matches the CRCs.
//!
//! Names are matched the way the client looks them up, ignoring case and the slash direction, both for the
//! archives of a tree and for the entries inside one. A `.pak` that is not an archive at all (the empty files some
//! clients ship in place of `Effects_Textures.pak`) counts as an archive with no entries: the client reads nothing
//! from it either.

use crate::error::{Error, Result};
use crate::names;
use crate::pak::{Entry, PakReader};
use crate::tree;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
	OnlyFirst,
	OnlySecond,
	/// Same name, other size or CRC.
	Changed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Side {
	pub size: u32,
	pub packed: u32,
	pub crc: u32,
}

impl From<&Entry> for Side {
	fn from(e: &Entry) -> Self {
		Side {
			size: e.size,
			packed: e.compressed_size,
			crc: e.crc,
		}
	}
}

#[derive(Clone, Debug)]
pub struct EntryDiff {
	pub name: String,
	pub change: Change,
	pub first: Option<Side>,
	pub second: Option<Side>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PakStatus {
	/// The same entries with the same sizes and CRCs.
	Same,
	/// Entries added, removed or changed; maybe only their order.
	Differs,
	OnlyFirst,
	OnlySecond,
	/// One side could not be read; the text says which and why.
	Unreadable(String),
}

#[derive(Clone, Debug)]
pub struct PakDiff {
	/// The archive's path under the compared folder, or its file name when two archives were compared.
	pub path: String,
	pub status: PakStatus,
	/// Entries on each side; 0 for a missing side or for a file that is not an archive.
	pub first_entries: usize,
	pub second_entries: usize,
	/// Set when a side is a `.pak` that is not an archive (an empty placeholder), counted as no entries.
	pub first_empty: bool,
	pub second_empty: bool,
	/// The entries the archives share are stored in another order. Matters only for names stored twice.
	pub reordered: bool,
	pub entries: Vec<EntryDiff>,
}

impl PakDiff {
	pub fn count(&self, change: Change) -> usize {
		self.entries.iter().filter(|e| e.change == change).count()
	}
}

#[derive(Clone, Debug, Default)]
pub struct Report {
	/// Every archive of either side, the same ones included, sorted by path.
	pub paks: Vec<PakDiff>,
}

impl Report {
	pub fn is_same(&self) -> bool {
		self.paks.iter().all(|p| p.status == PakStatus::Same)
	}

	/// One line per difference, each naming the archive when a tree was compared: what the command line prints.
	pub fn lines(&self, tree: bool) -> Vec<String> {
		let mut out = Vec::new();
		for pak in &self.paks {
			let prefix = if tree { format!("{}: ", pak.path) } else { String::new() };
			match &pak.status {
				PakStatus::Same => continue,
				PakStatus::OnlyFirst => {
					out.push(format!("{prefix}only in the first"));
					continue;
				}
				PakStatus::OnlySecond => {
					out.push(format!("{prefix}only in the second"));
					continue;
				}
				PakStatus::Unreadable(why) => {
					out.push(format!("{prefix}{why}"));
					continue;
				}
				PakStatus::Differs => {}
			}
			for (empty, side) in [(pak.first_empty, "first"), (pak.second_empty, "second")] {
				if empty {
					out.push(format!("{prefix}the {side} is not an archive, taken as empty"));
				}
			}
			if pak.first_entries != pak.second_entries {
				out.push(format!(
					"{prefix}{} entries against {}",
					pak.first_entries, pak.second_entries
				));
			}
			for e in &pak.entries {
				out.push(match (e.change, e.first, e.second) {
					(Change::OnlyFirst, ..) => format!("{prefix}only in the first: {}", e.name),
					(Change::OnlySecond, ..) => format!("{prefix}only in the second: {}", e.name),
					(Change::Changed, Some(x), Some(y)) => format!(
						"{prefix}{}: {} bytes CRC {:08x} against {} bytes CRC {:08x}",
						e.name, x.size, x.crc, y.size, y.crc
					),
					(Change::Changed, ..) => format!("{prefix}{}: differs", e.name),
				});
			}
			if pak.reordered {
				out.push(format!("{prefix}same entries in another order"));
			}
		}
		out
	}
}

/// The entries of one archive by lookup key, in their stored order. A name stored twice keeps its first entry, the
/// one the client finds.
fn by_key(entries: &[Entry]) -> (Vec<Vec<u8>>, HashMap<Vec<u8>, &Entry>) {
	let mut order = Vec::with_capacity(entries.len());
	let mut map = HashMap::with_capacity(entries.len());
	for e in entries {
		let key = names::lookup_key(&e.name);
		if !map.contains_key(&key) {
			map.insert(key.clone(), e);
			order.push(key);
		}
	}
	(order, map)
}

fn diff_entries(path: String, a: &[Entry], b: &[Entry], first_empty: bool, second_empty: bool) -> PakDiff {
	let (order_a, map_a) = by_key(a);
	let (order_b, map_b) = by_key(b);
	let mut entries = Vec::new();
	for key in &order_a {
		let x = map_a[key];
		match map_b.get(key) {
			None => entries.push(EntryDiff {
				name: names::to_text(&x.name),
				change: Change::OnlyFirst,
				first: Some(x.into()),
				second: None,
			}),
			Some(y) if x.size != y.size || x.crc != y.crc => entries.push(EntryDiff {
				name: names::to_text(&x.name),
				change: Change::Changed,
				first: Some(x.into()),
				second: Some((*y).into()),
			}),
			Some(_) => {}
		}
	}
	for key in &order_b {
		if !map_a.contains_key(key) {
			let y = map_b[key];
			entries.push(EntryDiff {
				name: names::to_text(&y.name),
				change: Change::OnlySecond,
				first: None,
				second: Some(y.into()),
			});
		}
	}
	let shared = |order: &[Vec<u8>], other: &HashMap<Vec<u8>, &Entry>| -> Vec<Vec<u8>> {
		order.iter().filter(|k| other.contains_key(*k)).cloned().collect()
	};
	let reordered = shared(&order_a, &map_b) != shared(&order_b, &map_a);
	let status = if entries.is_empty() && !reordered && first_empty == second_empty && a.len() == b.len() {
		PakStatus::Same
	} else {
		PakStatus::Differs
	};
	PakDiff {
		path,
		status,
		first_entries: a.len(),
		second_entries: b.len(),
		first_empty,
		second_empty,
		reordered,
		entries,
	}
}

/// An archive, `None` for a file that is not an archive, or why it could not be read.
fn open(path: &Path) -> std::result::Result<Option<PakReader>, String> {
	match PakReader::open(path) {
		Ok(r) => Ok(Some(r)),
		Err(Error::NotArchive) => Ok(None),
		Err(e) => Err(e.to_string()),
	}
}

pub fn paks(path: String, a: &Path, b: &Path) -> PakDiff {
	let blank = |status| PakDiff {
		path: path.clone(),
		status,
		first_entries: 0,
		second_entries: 0,
		first_empty: false,
		second_empty: false,
		reordered: false,
		entries: Vec::new(),
	};
	match (open(a), open(b)) {
		(Ok(x), Ok(y)) => {
			let none: &[Entry] = &[];
			diff_entries(
				path.clone(),
				x.as_ref().map_or(none, |r| r.entries()),
				y.as_ref().map_or(none, |r| r.entries()),
				x.is_none(),
				y.is_none(),
			)
		}
		(Err(e), _) => blank(PakStatus::Unreadable(format!("the first cannot be read: {e}"))),
		(_, Err(e)) => blank(PakStatus::Unreadable(format!("the second cannot be read: {e}"))),
	}
}

/// Two archives, or two folders of archives.
pub fn trees(a: &Path, b: &Path) -> Result<Report> {
	if a.is_file() && b.is_file() {
		let name = |p: &Path| {
			p.file_name()
				.map(|n| n.to_string_lossy().into_owned())
				.unwrap_or_default()
		};
		let path = if name(a).eq_ignore_ascii_case(&name(b)) {
			name(b)
		} else {
			format!("{} / {}", name(a), name(b))
		};
		return Ok(Report {
			paks: vec![paks(path, a, b)],
		});
	}
	// Archive paths matched without regard to case: `effects_Meshes.pak` is the client's `Effects_Meshes.pak`.
	let list = |root: &Path| -> Result<BTreeMap<String, PathBuf>> {
		Ok(tree::find_paks(root)?
			.into_iter()
			.map(|rel| (rel.to_string_lossy().replace('\\', "/").to_ascii_lowercase(), rel))
			.collect())
	};
	let (la, lb) = (list(a)?, list(b)?);
	let mut keys: Vec<&String> = la.keys().chain(lb.keys()).collect();
	keys.sort();
	keys.dedup();
	let shown = |rel: &PathBuf| rel.to_string_lossy().replace('\\', "/");
	let mut out = Vec::new();
	for key in keys {
		match (la.get(key), lb.get(key)) {
			(Some(x), Some(y)) => {
				let diff = paks(shown(y), &a.join(x), &b.join(y));
				// Two files that are both no archive: another program's .pak, or two placeholders. Nothing to say.
				if diff.first_empty && diff.second_empty {
					continue;
				}
				out.push(diff);
			}
			(Some(x), None) => {
				if let Ok(Some(r)) = open(&a.join(x)) {
					out.push(one_side(shown(x), PakStatus::OnlyFirst, r.entries().len()));
				}
			}
			(None, Some(y)) => {
				if let Ok(Some(r)) = open(&b.join(y)) {
					out.push(one_side(shown(y), PakStatus::OnlySecond, r.entries().len()));
				}
			}
			(None, None) => unreachable!(),
		}
	}
	Ok(Report { paks: out })
}

fn one_side(path: String, status: PakStatus, count: usize) -> PakDiff {
	let first = status == PakStatus::OnlyFirst;
	PakDiff {
		path,
		status,
		first_entries: if first { count } else { 0 },
		second_entries: if first { 0 } else { count },
		first_empty: false,
		second_empty: false,
		reordered: false,
		entries: Vec::new(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::pak::DosTime;

	fn entry(name: &str, size: u32, crc: u32) -> Entry {
		Entry {
			name: name.as_bytes().to_vec(),
			method: 8,
			crc,
			compressed_size: size / 2,
			size,
			header_offset: 0,
			scrambled: false,
			time: DosTime::default(),
		}
	}

	#[test]
	fn names_match_without_case() {
		let a = [entry("Data/A.xml", 10, 1), entry("b.dds", 5, 2)];
		let b = [entry("data/a.XML", 10, 1), entry("b.dds", 5, 3), entry("c.dds", 1, 4)];
		let d = diff_entries("x.pak".into(), &a, &b, false, false);
		assert_eq!(d.status, PakStatus::Differs);
		assert_eq!(d.count(Change::Changed), 1);
		assert_eq!(d.count(Change::OnlySecond), 1);
		assert_eq!(d.count(Change::OnlyFirst), 0);
		assert!(!d.reordered);
	}

	#[test]
	fn same_and_reordered() {
		let a = [entry("a", 1, 1), entry("b", 2, 2)];
		let b = [entry("b", 2, 2), entry("a", 1, 1)];
		assert_eq!(diff_entries("x".into(), &a, &a, false, false).status, PakStatus::Same);
		let d = diff_entries("x".into(), &a, &b, false, false);
		assert!(d.reordered && d.entries.is_empty() && d.status == PakStatus::Differs);
	}

	#[test]
	fn placeholder_counts_as_empty() {
		let b = [entry("a", 1, 1)];
		let d = diff_entries("x".into(), &[], &b, true, false);
		assert!(d.first_empty);
		assert_eq!(d.count(Change::OnlySecond), 1);
		assert!(Report { paks: vec![d] }.lines(false).iter().any(|l| l.contains("not an archive")));
	}
}
