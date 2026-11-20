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

/// Words of the markup a rendering of compiled XML adds around the strings of its pool: the declaration, the
/// entities and CDATA. One per line, as the pool text is.
const MARKUP_WORDS: &str = "xml\nversion\n1.0\nencoding\nUTF-8\namp\nlt\ngt\nquot\nCDATA\n";

/// Files of these kinds are never text: they are skipped without being unpacked.
const BINARY_EXTENSIONS: [&str; 18] = [
	"dds", "tga", "png", "jpg", "jpeg", "bmp", "ogg", "wav", "mp3", "cgf", "cga", "caf", "chr", "anm", "ttf", "otf",
	"fsb", "swf",
];

fn is_binary_name(name: &str) -> bool {
	name.rsplit_once('.')
		.is_some_and(|(_, extension)| BINARY_EXTENSIONS.iter().any(|known| extension.eq_ignore_ascii_case(known)))
}

/// What to look for.
pub struct Query {
	regex: Regex,
	/// The pattern is plain text without markup characters or white space, so a match in a rendering of compiled
	/// XML lies inside one string of its pool or one word of the markup: a file whose pool does not hold it is
	/// skipped without being rendered.
	pool_check: bool,
	/// For plain text: the pattern as UTF-16LE bytes, so UTF-16 files without it are skipped unconverted.
	utf16: Option<Regex>,
}

/// A byte pattern for `pattern` encoded as UTF-16LE, at any alignment. With `ignore_case` only ASCII patterns get
/// one: their case variants are known (with the Kelvin sign and the long s, which fold to k and s), while other
/// scripts fold in ways not worth reproducing for a shortcut.
fn utf16_pattern(pattern: &str, ignore_case: bool) -> Option<Regex> {
	if pattern.is_empty() || (ignore_case && !pattern.is_ascii()) {
		return None;
	}
	let mut source = String::from("(?-u)");
	for c in pattern.chars() {
		let mut variants = vec![c];
		if ignore_case && c.is_ascii_alphabetic() {
			variants = vec![c.to_ascii_lowercase(), c.to_ascii_uppercase()];
			match c.to_ascii_lowercase() {
				'k' => variants.push('\u{212a}'),
				's' => variants.push('\u{17f}'),
				_ => {}
			}
		}
		let alternatives: Vec<String> = variants
			.iter()
			.map(|v| {
				let mut units = [0u16; 2];
				v.encode_utf16(&mut units).iter().flat_map(|u| u.to_le_bytes()).map(|b| format!("\\x{b:02x}")).collect()
			})
			.collect();
		source.push_str(&format!("(?:{})", alternatives.join("|")));
	}
	RegexBuilder::new(&source).unicode(false).build().ok()
}

/// Whether plain text `pattern` can only match inside one string of a compiled XML's pool (or `MARKUP_WORDS`).
/// Markup characters and white space could join a pool string to the markup around it, and a pattern made only
/// of digits, hex letters, `x` and `#` could match inside a character reference like `&#x1F;`.
fn fits_pool(pattern: &str) -> bool {
	!pattern.is_empty()
		&& !pattern.chars().any(|c| c.is_whitespace() || "<>&;\"'=/!?[]".contains(c))
		&& !pattern.chars().all(|c| c.is_ascii_hexdigit() || matches!(c, 'x' | 'X' | '#'))
}

impl Query {
	/// `literal` takes the pattern as plain text; otherwise it is a regular expression in the syntax of the
	/// `regex` crate. Case is ignored with `ignore_case`.
	pub fn new(pattern: &str, literal: bool, ignore_case: bool) -> Result<Self, regex::Error> {
		// A regular expression without special characters is plain text, and gets its shortcuts.
		let literal = literal || regex::escape(pattern) == pattern;
		let source = if literal { Cow::Owned(regex::escape(pattern)) } else { Cow::Borrowed(pattern) };
		let regex = RegexBuilder::new(&source)
			.case_insensitive(ignore_case)
			// `.` and `$` work on lines, as in a text editor's search.
			.multi_line(true)
			.size_limit(64 << 20)
			.build()?;
		// A UTF-16 file is searched after its declaration is rewritten to say UTF-8; a pattern that matches there
		// must see the converted text.
		let declarations: [&[u8]; 2] =
			[b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>", b"<?xml version=\"1.0\" encoding=\"UTF-16\"?>"];
		let utf16 = if literal && !declarations.iter().any(|d| regex.is_match(d)) {
			utf16_pattern(pattern, ignore_case)
		} else {
			None
		};
		Ok(Query {
			regex,
			pool_check: literal && fits_pool(pattern),
			utf16,
		})
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

/// The text a search looks at: decoded XML and HTML, UTF-16 as UTF-8, other data as it is. `None` for binary data,
/// and for compiled XML that cannot match `query` (see `Query::pool_check`).
fn searchable<'a>(name: &str, data: &'a [u8], query: &Query) -> Option<Cow<'a, [u8]>> {
	let form = xml::detect(data);
	if form == Form::Compiled
		&& query.pool_check
		&& xml::is_xml_name(name)
		&& let Ok(pool) = xml::compiled::pool_text(data)
		&& !query.regex.is_match(pool.as_bytes())
		&& !query.regex.is_match(MARKUP_WORDS.as_bytes())
	{
		return None;
	}
	let decoded = match form {
		Form::Compiled | Form::Encrypted if !xml::is_xml_name(name) => None,
		// UTF-16 text that is not XML is only converted: decoding would put an XML declaration in front of it.
		Form::Text | Form::Utf16 if !xml::is_xml_name(name) => match xml::as_text(data) {
			Ok(text) if text.as_ptr() != data.as_ptr() => Some(text.into_owned().into_bytes()),
			_ => None,
		},
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

const LF: [u8; 2] = [b'\n', 0];

/// Whether a UTF-16LE code unit `LF` starts at `i` of `units`, which starts at a unit boundary.
fn lf_at(units: &[u8], i: usize) -> bool {
	i.is_multiple_of(2) && units.get(i..i + 2) == Some(&LF)
}

fn count_lf(units: &[u8]) -> usize {
	memchr::memchr_iter(b'\n', units).filter(|&i| lf_at(units, i)).count()
}

/// The matching lines of UTF-16LE text (without its byte order mark), found in the UTF-16 itself with `utf16`; only
/// those lines are converted, to be matched again with the query and cut as `lines` cuts them. The same hits as
/// `lines` over the converted text, without converting a 200 MB file to find three lines in it.
fn utf16_lines(entry: &str, units: &[u8], query: &Query, utf16: &Regex) -> Vec<Hit> {
	let mut hits = Vec::new();
	// Decoding XML without a declaration puts one in front, on a line of its own, and the lines are numbered as the
	// decoded file shows them (in the preview, or extracted).
	let first = if xml::is_xml_name(entry) && !utf16_declares(units) { 2 } else { 1 };
	let (mut at, mut line, mut counted) = (0, first, 0);
	while let Some(found) = utf16.find_at(units, at) {
		// A match across two code units is no match; one starting a byte later may still be.
		if !found.start().is_multiple_of(2) {
			at = found.start() + 1;
			continue;
		}
		let start = memchr::memrchr_iter(b'\n', &units[..found.start()])
			.find(|&i| lf_at(units, i))
			.map_or(0, |i| i + 2);
		let end = memchr::memchr_iter(b'\n', &units[found.start()..])
			.map(|i| found.start() + i)
			.find(|&i| lf_at(units, i))
			.unwrap_or(units.len());
		line += count_lf(&units[counted..start]);
		counted = start;
		at = end + 2;
		let text: String = char::decode_utf16(units[start..end].as_chunks::<2>().0.iter().map(|u| u16::from_le_bytes(*u)))
			.map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
			.collect();
		hits.extend(lines(entry, text.as_bytes(), query).into_iter().map(|hit| Hit { line, ..hit }));
		if at >= units.len() {
			break;
		}
	}
	hits
}

/// Whether UTF-16LE text opens with an XML declaration, after white space.
fn utf16_declares(units: &[u8]) -> bool {
	let head = &units[..units.len().min(256)];
	let text: String = char::decode_utf16(head.as_chunks::<2>().0.iter().map(|u| u16::from_le_bytes(*u)))
		.map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
		.collect();
	text.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']).starts_with("<?xml")
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
	let name = names::to_text(&entry.name);
	if is_binary_name(&name) {
		return Ok(Vec::new());
	}
	pak.read_into(entry, codec, buffer)?;
	if let Some(utf16) = &query.utf16
		&& buffer.starts_with(&[0xff, 0xfe])
		&& buffer.len().is_multiple_of(2)
	{
		return Ok(utf16_lines(&name, &buffer[2..], query, utf16));
	}
	Ok(match searchable(&name, buffer, query) {
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
	fn pool_check_finds_what_a_rendering_does() {
		let source = "<root a=\"x&amp;y\" b=\"tab&#9;\"><Kaisinel_npc id=\"7\">Text &lt; more</Kaisinel_npc>\
			<note><![CDATA[raw <b>]]></note></root>";
		let compiled = xml::encode(source.as_bytes(), Form::Compiled, "x.xml").unwrap().data;
		assert_eq!(xml::detect(&compiled), Form::Compiled);
		let rendered = xml::decode(&compiled, "x.xml", Encoding::Utf8).unwrap().text;
		for (needle, case) in [
			("kaisinel", false),
			("Kaisinel_npc", true),
			("kaisinel_npcs", false),
			("amp", true),
			("lt", false),
			("CDATA", true),
			("version", true),
			("UTF", false),
			("raw", true),
			("more", false),
			("nothing", false),
			("x", true),
			("9", true),
		] {
			let query = Query::new(needle, true, !case).unwrap();
			let expected = lines("x.xml", &rendered, &query).len();
			let found = searchable("x.xml", &compiled, &query).map_or(0, |text| lines("x.xml", &text, &query).len());
			assert_eq!(found, expected, "{needle}");
		}
		assert!(Query::new("Kaisinel", true, true).unwrap().pool_check);
		assert!(!Query::new("<name>", true, true).unwrap().pool_check);
		assert!(!Query::new("a b", true, true).unwrap().pool_check);
		assert!(!Query::new("1F", true, true).unwrap().pool_check);
		assert!(!Query::new("Kaisinel\\w", false, true).unwrap().pool_check);
	}

	#[test]
	fn utf16_shortcut_finds_what_converting_does() {
		let text = "<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n<a>Kaisinel \u{212a}ey Straße</a>\r\n\
			<b>x</b>\r\n<c>kaisinel kaisinel</c>\n<d>Kai</d>";
		let mut data = vec![0xff, 0xfe];
		for unit in text.encode_utf16() {
			data.extend_from_slice(&unit.to_le_bytes());
		}
		let converted = xml::decode(&data, "x.xml", Encoding::Utf8).unwrap().text;
		for (needle, case) in [
			("kaisinel", false),
			("Kaisinel", true),
			("kaisinel", true),
			("key", false),
			("Straße", true),
			("strasse", false),
			("UTF-8", true),
			("UTF-16", true),
			("encoding", false),
			("> <", true),
			("nothing", false),
		] {
			let query = Query::new(needle, true, !case).unwrap();
			let expected: Vec<(usize, String)> =
				lines("x.xml", &converted, &query).into_iter().map(|h| (h.line, h.text)).collect();
			let found: Vec<(usize, String)> = match &query.utf16 {
				Some(utf16) => utf16_lines("x.xml", &data[2..], &query, utf16),
				None => lines("x.xml", &searchable("x.xml", &data, &query).unwrap(), &query),
			}
			.into_iter()
			.map(|h| (h.line, h.text))
			.collect();
			assert_eq!(found, expected, "{needle}");
		}
		assert!(Query::new("kaisinel", true, true).unwrap().utf16.is_some());
		assert!(Query::new("straße", true, true).unwrap().utf16.is_none());
		assert!(Query::new("UTF-8", true, false).unwrap().utf16.is_none());
	}

	#[test]
	fn utf16_lines_are_numbered_as_decoded() {
		let utf16 = |text: &str| {
			let mut data = vec![0xff, 0xfe];
			for unit in text.encode_utf16() {
				data.extend_from_slice(&unit.to_le_bytes());
			}
			data
		};
		let query = Query::new("needle", true, false).unwrap();
		let slow = |name: &str, data: &[u8]| -> Vec<usize> {
			lines(name, &searchable(name, data, &query).unwrap(), &query).iter().map(|h| h.line).collect()
		};
		let fast = |name: &str, data: &[u8]| -> Vec<usize> {
			utf16_lines(name, &data[2..], &query, query.utf16.as_ref().unwrap()).iter().map(|h| h.line).collect()
		};
		let plain = utf16("a\r\nneedle\r\n");
		let declared = utf16("<?xml version=\"1.0\" encoding=\"UTF-16\"?>\r\n<a>needle</a>");
		// Text is not given a declaration; XML without one is, as decoding does.
		assert_eq!((slow("x.txt", &plain), fast("x.txt", &plain)), (vec![2], vec![2]));
		assert_eq!((slow("x.xml", &plain), fast("x.xml", &plain)), (vec![3], vec![3]));
		assert_eq!((slow("x.xml", &declared), fast("x.xml", &declared)), (vec![2], vec![2]));
	}

	#[test]
	fn utf16_is_searched_as_text_and_binary_is_skipped() {
		let mut utf16 = vec![0xff, 0xfe];
		for unit in "<a>Привет</a>".encode_utf16() {
			utf16.extend_from_slice(&unit.to_le_bytes());
		}
		let query = Query::new("Привет", true, false).unwrap();
		let text = searchable("x.xml", &utf16, &query).unwrap();
		assert_eq!(lines("x.xml", &text, &query).len(), 1);
		assert!(searchable("x.dds", b"DDS \0\0\0", &query).is_none());
		assert!(is_binary_name("Textures/a.DDS") && !is_binary_name("a.xml"));
	}
}
