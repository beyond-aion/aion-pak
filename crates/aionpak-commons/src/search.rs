//! Searching inside archives: the text of every entry, with compiled, encrypted and UTF-16 XML and HTML turned into
//! UTF-8 first, so a search sees what a decoded extraction would show and not the stored bytes.

use crate::names;
use crate::ops::NameFilter;
use crate::ops::{BIG_BYTES, WINDOW_BYTES};
use crate::pak::{Codec, CodecPool, Entry, PakReader};
use crate::progress::Progress;
use crate::xml::{self, Encoding, Form};
use rayon::prelude::*;
use regex::bytes::{Regex, RegexBuilder};
use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

/// Lines longer than this are cut around the match in a hit.
const LINE_CHARS: usize = 240;
/// Data with a NUL byte in its first 8 KiB, after decoding, is binary and not searched, as grep and ripgrep do.
const BINARY_PROBE: usize = 8192;

/// What to look for.
pub struct Query {
	regex: Regex,
}

impl Query {
	/// `literal` takes the pattern as plain text; otherwise it is a regular expression in the syntax of the
	/// `regex` crate. Case is ignored with `ignore_case`.
	pub fn new(pattern: &str, literal: bool, ignore_case: bool) -> Result<Self, regex::Error> {
		let source = if literal { Cow::Owned(regex::escape(pattern)) } else { Cow::Borrowed(pattern) };
		let regex = RegexBuilder::new(&source)
			.case_insensitive(ignore_case)
			// `.` and `$` work on lines, as in a text editor's search.
			.multi_line(true)
			.size_limit(64 << 20)
			.build()?;
		Ok(Query { regex })
	}
}

/// A line of an entry that matches.
#[derive(Clone, Debug)]
pub struct Hit {
	/// The entry's name inside the archive.
	pub entry: String,
	/// From 1.
	pub line: usize,
	/// The line, cut to about `LINE_CHARS` characters around the first match in it.
	pub text: String,
	/// Where the first match lies in `text`, in bytes.
	pub at: std::ops::Range<usize>,
}

/// The text a search looks at: decoded XML and HTML, UTF-16 as UTF-8, other data as it is. `None` for binary data.
fn searchable<'a>(name: &str, data: &'a [u8]) -> Option<Cow<'a, [u8]>> {
	let decoded = match xml::detect(data) {
		Form::Compiled | Form::Encrypted if !xml::is_xml_name(name) => None,
		Form::Text if !data.starts_with(&[0xff, 0xfe]) && !data.starts_with(&[0xfe, 0xff]) => None,
		_ => xml::decode(data, name, Encoding::Utf8).ok().map(|d| d.text),
	};
	let text = match decoded {
		Some(text) => Cow::Owned(text),
		None => Cow::Borrowed(data),
	};
	let probe = &text[..text.len().min(BINARY_PROBE)];
	(!probe.contains(&0)).then_some(text)
}

/// The matching lines of `text`, one hit per line.
fn lines(entry: &str, text: &[u8], query: &Query) -> Vec<Hit> {
	let mut hits = Vec::new();
	let mut line = 1;
	let mut counted = 0;
	let mut next = 0;
	for found in query.regex.find_iter(text) {
		if found.start() < next {
			continue;
		}
		let start = text[..found.start()].iter().rposition(|&b| b == b'\n').map_or(0, |i| i + 1);
		let end = text[found.start()..].iter().position(|&b| b == b'\n').map_or(text.len(), |i| found.start() + i);
		line += text[counted..start].iter().filter(|&&b| b == b'\n').count();
		counted = start;
		next = end + 1;
		let content = &text[start..end];
		let content = content.strip_suffix(b"\r").unwrap_or(content);
		let (from, to) = (found.start() - start, (found.end() - start).min(content.len()));
		hits.push(cut(entry, line, content, from, to));
	}
	hits
}

/// A hit for the match at `from..to` of `content`, which is cut to the match and some text on either side.
fn cut(entry: &str, line: usize, content: &[u8], from: usize, to: usize) -> Hit {
	let (mut start, mut end) = (0, content.len());
	if end > LINE_CHARS * 2 {
		let margin = LINE_CHARS / 3;
		start = from.saturating_sub(margin);
		end = (to.max(from + 1) + LINE_CHARS - margin).min(content.len());
	}
	let before = String::from_utf8_lossy(&content[start..from]);
	let matched = String::from_utf8_lossy(&content[from..to]);
	let after = String::from_utf8_lossy(&content[to..end]);
	let lead = if start > 0 { "…" } else { "" };
	let tail = if end < content.len() { "…" } else { "" };
	let head = format!("{lead}{}", before.trim_start());
	let at = head.len()..head.len() + matched.len();
	Hit {
		entry: entry.to_owned(),
		line,
		text: format!("{head}{matched}{after}{tail}").trim_end().to_owned(),
		at,
	}
}

fn entry_hits(pak: &PakReader, entry: &Entry, query: &Query, codec: &mut Codec, buffer: &mut Vec<u8>) -> crate::Result<Vec<Hit>> {
	pak.read_into(entry, codec, buffer)?;
	let name = names::to_text(&entry.name);
	Ok(match searchable(&name, buffer) {
		Some(text) => lines(&name, &text, query),
		None => Vec::new(),
	})
}

/// Searches the entries of one archive that pass `filter` and returns the hits in the archive's order.
pub fn archive(
	pak: &PakReader,
	label: &str,
	query: &Query,
	filter: Option<&NameFilter>,
	progress: &Progress,
) -> Vec<Hit> {
	let chosen: Vec<&Entry> = pak
		.entries()
		.iter()
		.filter(|e| !e.is_dir() && filter.is_none_or(|f| f(&names::to_text(&e.name))))
		.collect();
	chosen
		.par_iter()
		.map_init(
			|| (Codec::new(0), Vec::new()),
			|(codec, buffer), entry| {
				if progress.cancelled() {
					return Vec::new();
				}
				let hits = entry_hits(pak, entry, query, codec, buffer).unwrap_or_else(|err| {
					progress.error(format!("{label}: {}: {err}", names::to_text(&entry.name)));
					Vec::new()
				});
				progress.advance(1, entry.size as u64);
				hits
			},
		)
		.flatten_iter()
		.collect()
}

/// Searches the archives `paks` under `root` and hands the hits of each archive to `found` once it is done,
/// with the archive's path relative to `root`, in the order of `paks`. The entries of all the archives share one
/// queue, largest first, so a big XML file in one archive does not leave the pool idle while it is decoded.
pub fn archives(
	root: &Path,
	paks: &[PathBuf],
	query: &Query,
	filter: Option<&NameFilter>,
	progress: &Progress,
	found: &mut dyn FnMut(&str, Vec<Hit>),
) {
	progress.archives_total.fetch_add(paks.len() as u64, Relaxed);
	let readers: Vec<(String, PakReader)> = paks
		.par_iter()
		.filter_map(|rel| {
			let label = rel.display().to_string();
			match PakReader::open(&root.join(rel)) {
				Ok(reader) => {
					let (files, bytes) = crate::ops::totals(&reader, filter);
					progress.add_total(files, bytes);
					Some((label, reader))
				}
				Err(err) => {
					match err {
						crate::Error::NotArchive => progress.warn(format!("{label}: not a zip archive, skipped")),
						err => progress.error(format!("{label}: {err}")),
					}
					progress.archives_done.fetch_add(1, Relaxed);
					None
				}
			}
		})
		.collect();
	// Every entry to search, as (archive, entry), archive after archive; an empty archive still gets a marker
	// (`None`), so it is counted done in its turn.
	let mut items: Vec<(usize, Option<&Entry>)> = Vec::new();
	for (index, (_, reader)) in readers.iter().enumerate() {
		let before = items.len();
		items.extend(
			reader
				.entries()
				.iter()
				.filter(|e| !e.is_dir() && filter.is_none_or(|f| f(&names::to_text(&e.name))))
				.map(|e| (index, Some(e))),
		);
		if items.len() == before {
			items.push((index, None));
		}
	}
	let codecs = CodecPool::new(0);
	let work = |&(index, entry): &(usize, Option<&Entry>)| -> Vec<Hit> {
		let Some(entry) = entry else {
			return Vec::new();
		};
		if progress.cancelled() {
			return Vec::new();
		}
		let (label, reader) = &readers[index];
		let hits = codecs
			.with(|codec| entry_hits(reader, entry, query, codec, &mut Vec::new()))
			.unwrap_or_else(|err| {
				progress.error(format!("{label}: {}: {err}", names::to_text(&entry.name)));
				Vec::new()
			});
		progress.advance(1, entry.size as u64);
		hits
	};
	let mut current = (0, Vec::new());
	let finish = |(index, hits): (usize, Vec<Hit>), found: &mut dyn FnMut(&str, Vec<Hit>)| {
		if !hits.is_empty() {
			found(&readers[index].0, hits);
		}
		progress.archives_done.fetch_add(1, Relaxed);
	};
	let _ = crate::ordered::run(
		&items,
		|&(_, entry)| entry.map_or(0, |e| e.size as u64),
		WINDOW_BYTES,
		BIG_BYTES,
		work,
		|hits| hits.iter().map(|h| (h.text.len() + h.entry.len()) as u64).sum(),
		|i, hits| -> Result<(), ()> {
			let index = items[i].0;
			if index != current.0 {
				finish(std::mem::replace(&mut current, (index, Vec::new())), found);
			}
			current.1.extend(hits);
			if progress.cancelled() {
				return Err(());
			}
			Ok(())
		},
	);
	if !items.is_empty() && !progress.cancelled() {
		finish(current, found);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn one_hit_per_line_with_numbers() {
		let query = Query::new("b+", false, false).unwrap();
		let hits = lines("x.txt", b"a\nbb b\r\nc\nab\n", &query);
		let found: Vec<(usize, &str, &str)> =
			hits.iter().map(|h| (h.line, h.text.as_str(), &h.text[h.at.clone()])).collect();
		assert_eq!(found, [(2, "bb b", "bb"), (4, "ab", "b")]);
	}

	#[test]
	fn literal_and_case() {
		let query = Query::new("A.B", true, true).unwrap();
		assert_eq!(lines("x", b"axb\na.b\n", &query).len(), 1);
	}

	#[test]
	fn long_lines_are_cut_around_the_match() {
		let mut text = vec![b'x'; 5000];
		text[3000..3006].copy_from_slice(b"needle");
		let hit = &lines("x", &text, &Query::new("needle", true, false).unwrap())[0];
		assert!(hit.text.len() < 400);
		assert_eq!(&hit.text[hit.at.clone()], "needle");
		assert!(hit.text.starts_with('…') && hit.text.ends_with('…'));
	}

	#[test]
	fn utf16_is_searched_as_text_and_binary_is_skipped() {
		let mut utf16 = vec![0xff, 0xfe];
		for unit in "<a>Привет</a>".encode_utf16() {
			utf16.extend_from_slice(&unit.to_le_bytes());
		}
		let text = searchable("x.xml", &utf16).unwrap();
		assert_eq!(lines("x.xml", &text, &Query::new("Привет", true, false).unwrap()).len(), 1);
		assert!(searchable("x.dds", b"DDS \0\0\0").is_none());
	}
}
