//! Client XML and HTML files. The client reads four forms (decided by the leading bytes): compiled (`0x80`),
//! encrypted text (`0x81`), UTF-16 text with a BOM that declares `UTF-16`, and anything else as 8-bit text in the
//! system code page — UTF-8 is not recognised.

pub mod compiled;
pub mod crypt;
pub mod text;

use crate::error::{Result, xml_err};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Form {
	Compiled,
	Encrypted,
	Utf16,
	Text,
}

impl Form {
	pub fn name(self) -> &'static str {
		match self {
			Form::Compiled => "compiled",
			Form::Encrypted => "encrypted",
			Form::Utf16 => "utf16",
			Form::Text => "text",
		}
	}
}

/// Text encoding for decoded output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
	Utf16,
	Utf8,
}

pub fn is_xml_name(name: &str) -> bool {
	let lower = name.to_ascii_lowercase();
	lower.ends_with(".xml") || lower.ends_with(".html") || lower.ends_with(".htm")
}

pub fn is_html_name(name: &str) -> bool {
	let base = base_name(name);
	base.ends_with(".html") || base.ends_with(".htm")
}

fn base_name(name: &str) -> String {
	name.rsplit(['/', '\\']).next().unwrap_or(name).to_ascii_lowercase()
}

/// Level files the engine parses as plain XML text only: neither compiled nor encrypted forms load.
fn is_level_file(name: &str) -> bool {
	let base = base_name(name);
	base == "leveldata.xml" || (base.starts_with("mission_") && base.ends_with(".xml"))
}

/// Level files that only ever ship as text; which parser reads them is unknown, so they are not converted either.
fn is_level_text(name: &str) -> bool {
	let base = base_name(name);
	matches!(
		base.as_str(),
		"levelinfo.xml" | "moviedata.xml" | "materials.xml" | "lightmap_info.xml" | "vegetation_info.xml"
	) || base.ends_with("-bgm.xml")
}

/// Whether the client can read `name` in `form`. Quest pages load compiled but crash the client when shown.
pub fn accepts(name: &str, form: Form) -> bool {
	match form {
		Form::Compiled => {
			let base = base_name(name);
			!(base.ends_with(".html") || base.ends_with(".htm") || is_level_file(name) || is_level_text(name))
		}
		Form::Encrypted => !is_level_file(name) && !is_level_text(name),
		Form::Utf16 | Form::Text => true,
	}
}

const BOM_LE: [u8; 2] = [0xff, 0xfe];
const BOM_BE: [u8; 2] = [0xfe, 0xff];

/// Classifies data the way the client does.
pub fn detect(data: &[u8]) -> Form {
	match data.first() {
		Some(&compiled::MAGIC) => Form::Compiled,
		Some(&crypt::MAGIC) => Form::Encrypted,
		_ if declares_utf16(data) => Form::Utf16,
		_ => Form::Text,
	}
}

/// Whether the data starts with a UTF-16 byte order mark, and which: `Some(true)` for big-endian.
fn utf16_bom(data: &[u8]) -> Option<bool> {
	match data.get(..2)? {
		b if b == BOM_LE => Some(false),
		b if b == BOM_BE => Some(true),
		_ => None,
	}
}

fn utf16_units(data: &[u8]) -> Option<(Vec<u16>, bool)> {
	let be = utf16_bom(data)?;
	let units = data[2..]
		.as_chunks::<2>()
		.0
		.iter()
		.map(|p| {
			if be {
				u16::from_be_bytes([p[0], p[1]])
			} else {
				u16::from_le_bytes([p[0], p[1]])
			}
		})
		.collect();
	Some((units, be))
}

fn declares_utf16(data: &[u8]) -> bool {
	let Some((units, _)) = utf16_units(&data[..data.len().min(2 + 2 * 126)]) else {
		return false;
	};
	let head = String::from_utf16_lossy(&units);
	head.contains("\"utf-16\"") || head.contains("\"UTF-16\"")
}

/// UTF-8 text without its byte order mark.
fn utf8_text(data: &[u8]) -> Result<&str> {
	let data = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
	match std::str::from_utf8(data) {
		Ok(s) => Ok(s),
		Err(_) => xml_err("text is neither UTF-16 nor UTF-8; 8-bit code pages are not supported"),
	}
}

/// The text of UTF-16 data (converted) or UTF-8 data (borrowed, without a byte order mark).
pub fn as_text(data: &[u8]) -> Result<Cow<'_, str>> {
	if let Some(be) = utf16_bom(data) {
		// A stray last byte is not part of the text.
		let body = &data[2..data.len() & !1];
		let enc = if be {
			encoding_rs::UTF_16BE
		} else {
			encoding_rs::UTF_16LE
		};
		return match enc.decode_without_bom_handling_and_without_replacement(body) {
			Some(s) => Ok(Cow::Owned(s.into_owned())),
			None => xml_err("invalid UTF-16"),
		};
	}
	utf8_text(data).map(Cow::Borrowed)
}

fn utf16_bytes(s: &str, big_endian: bool) -> Vec<u8> {
	let mut out = Vec::with_capacity(2 + 2 * s.len());
	out.extend_from_slice(if big_endian { &BOM_BE } else { &BOM_LE });
	for u in s.encode_utf16() {
		out.extend_from_slice(&if big_endian { u.to_be_bytes() } else { u.to_le_bytes() });
	}
	out
}

/// What the XML declaration of a text said about its encoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Declared {
	/// No declaration at all.
	Missing,
	/// A declaration without an `encoding` attribute.
	Unnamed,
	/// The attribute's value, exactly as written.
	Named(String),
}

/// What UTF-16 text looked like before it was turned into UTF-8, so it can be turned back byte for byte.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Utf8Origin {
	pub big_endian: bool,
	pub declared: Declared,
}

const ADDED_DECL_HEAD: &str = "<?xml version=\"1.0\" encoding=\"";
const ADDED_DECL_TAIL: &str = "\"?>\r\n";

/// Splits text into what precedes the declaration (a BOM, whitespace), the declaration up to `?>`, and the rest.
fn split_decl(s: &str) -> Option<(&str, &str, &str)> {
	let lead = s.len() - s.trim_start_matches(['\u{feff}', ' ', '\t', '\r', '\n']).len();
	let body = &s[lead..];
	if !body.starts_with("<?xml") {
		return None;
	}
	let end = body.find("?>")?;
	Some((&s[..lead], &body[..end], &body[end..]))
}

/// The byte range of the `encoding` value inside a declaration.
fn encoding_value(decl: &str) -> Option<(usize, usize)> {
	let i = decl.find("encoding")?;
	let after = i + "encoding".len();
	let q = after + decl[after..].find(['"', '\''])?;
	let quote = &decl[q..q + 1];
	let end = q + 1 + decl[q + 1..].find(quote)?;
	Some((q + 1, end))
}

pub fn declared(s: &str) -> Declared {
	match split_decl(s) {
		None => Declared::Missing,
		Some((_, decl, _)) => match encoding_value(decl) {
			Some((a, b)) => Declared::Named(decl[a..b].to_owned()),
			None => Declared::Unnamed,
		},
	}
}

/// Makes the XML declaration name `encoding`, adding the attribute or a whole declaration if needed.
fn set_declared_encoding(s: &str, encoding: &str) -> String {
	let s = s.strip_prefix('\u{feff}').unwrap_or(s);
	match split_decl(s) {
		Some((lead, decl, rest)) => match encoding_value(decl) {
			Some((a, b)) => format!("{lead}{}{encoding}{}{rest}", &decl[..a], &decl[b..]),
			None => format!("{lead}{decl} encoding=\"{encoding}\"{rest}"),
		},
		None => format!("{ADDED_DECL_HEAD}{encoding}{ADDED_DECL_TAIL}{s}"),
	}
}

/// Undoes `set_declared_encoding(s, encoding)` on text that had `original`.
fn unset_declared_encoding(s: &str, encoding: &str, original: &Declared) -> String {
	match original {
		Declared::Named(v) => set_declared_encoding(s, v),
		Declared::Missing => {
			let added = format!("{ADDED_DECL_HEAD}{encoding}{ADDED_DECL_TAIL}");
			s.strip_prefix(added.as_str()).unwrap_or(s).to_owned()
		}
		Declared::Unnamed => {
			let added = format!(" encoding=\"{encoding}\"");
			match split_decl(s) {
				Some((lead, decl, rest)) => {
					format!("{lead}{}{rest}", decl.strip_suffix(added.as_str()).unwrap_or(decl))
				}
				None => s.to_owned(),
			}
		}
	}
}

/// Text the client misreads is UTF-8 with non-ASCII characters (it reads 8-bit text in the system code page):
/// such text becomes UTF-16 with the declaration. Anything else is returned unchanged. Returns the bytes and
/// whether a conversion happened.
pub fn client_text(data: &[u8]) -> Result<(Vec<u8>, bool)> {
	if utf16_bom(data).is_some() {
		return Ok((data.to_vec(), false));
	}
	let body = data.strip_prefix(b"\xef\xbb\xbf").unwrap_or(data);
	if body.is_ascii() {
		return Ok((data.to_vec(), false));
	}
	match std::str::from_utf8(body) {
		Ok(s) => Ok((utf16_bytes(&set_declared_encoding(s, "UTF-16"), false), true)),
		Err(_) => Ok((data.to_vec(), false)),
	}
}

const UTF8_NAME: &str = "UTF-8";

pub struct Decoded {
	pub form: Form,
	pub text: Vec<u8>,
	/// Set when UTF-16 text was turned into UTF-8; `restore` turns it back.
	pub utf8: Option<Utf8Origin>,
}

/// Turns any client form into readable text. 8-bit text is passed through unchanged.
pub fn decode(data: &[u8], file_name: &str, encoding: Encoding) -> Result<Decoded> {
	let form = detect(data);
	let plain = match form {
		Form::Compiled => {
			return Ok(Decoded {
				form,
				text: render(data, encoding)?,
				utf8: None,
			});
		}
		Form::Encrypted => match crypt::decrypt(data, crypt::stem(file_name)) {
			Some(p) => p,
			None => return xml_err("encrypted with a key for another file name"),
		},
		Form::Utf16 | Form::Text => data.to_vec(),
	};
	// UTF-16 with a stray last byte stays as it is: UTF-8 could not carry that byte back.
	if encoding == Encoding::Utf8
		&& plain.len() % 2 == 0
		&& let Some(big_endian) = utf16_bom(&plain)
	{
		let s = as_text(&plain)?;
		let origin = Utf8Origin {
			big_endian,
			declared: declared(&s),
		};
		let text = set_declared_encoding(&s, UTF8_NAME).into_bytes();
		return Ok(Decoded {
			form,
			text,
			utf8: Some(origin),
		});
	}
	Ok(Decoded {
		form,
		text: plain,
		utf8: None,
	})
}

/// Roughly how many characters of text a byte of compiled XML turns into, so the text needs no reallocation.
const TEXT_GROWTH: usize = 5;

/// Compiled XML as text.
pub fn render(compiled: &[u8], encoding: Encoding) -> Result<Vec<u8>> {
	Ok(match encoding {
		Encoding::Utf16 => {
			let mut out = text::Utf16Le(Vec::with_capacity(TEXT_GROWTH * 2 * compiled.len()));
			out.0.extend_from_slice(&BOM_LE);
			text::write(compiled, "UTF-16", &mut out)?;
			out.0
		}
		Encoding::Utf8 => {
			let mut out = String::with_capacity(TEXT_GROWTH * compiled.len());
			text::write(compiled, "UTF-8", &mut out)?;
			out.into_bytes()
		}
	})
}

pub struct Encoded {
	pub data: Vec<u8>,
	/// UTF-8 text with non-ASCII characters was converted to UTF-16 for the client.
	pub converted: bool,
}

/// Undoes `decode`: an unedited file comes back byte for byte. Edited text that the client would misread is
/// converted (see `client_text`).
pub fn restore(data: &[u8], form: Form, utf8: Option<&Utf8Origin>, file_name: &str, edited: bool) -> Result<Encoded> {
	// Data already in a binary form was not touched by decode (or was encoded by hand): just convert it.
	if matches!(detect(data), Form::Compiled | Form::Encrypted) {
		return encode(data, form, file_name);
	}
	// Compiled XML keeps neither the declaration nor the encoding of the text: compile what is there.
	if form == Form::Compiled {
		return Ok(Encoded {
			data: text::compile(data)?,
			converted: false,
		});
	}
	let plain = match utf8 {
		Some(o) => {
			let s = as_text(data)?;
			utf16_bytes(&unset_declared_encoding(&s, UTF8_NAME, &o.declared), o.big_endian)
		}
		None => data.to_vec(),
	};
	let (plain, converted) = if edited { client_text(&plain)? } else { (plain, false) };
	let data = match form {
		Form::Encrypted => crypt::encrypt(&plain, crypt::stem(file_name)),
		_ => plain,
	};
	Ok(Encoded { data, converted })
}

/// Brings text (or data already in a client form) into `form`.
pub fn encode(data: &[u8], form: Form, file_name: &str) -> Result<Encoded> {
	let current = detect(data);
	if current == form && form != Form::Text {
		return Ok(Encoded {
			data: data.to_vec(),
			converted: false,
		});
	}
	if !accepts(file_name, form) {
		return xml_err(format!("the client does not read this file {}", form.name()));
	}
	let plain = match current {
		Form::Compiled => Cow::Owned(render(data, Encoding::Utf16)?),
		Form::Encrypted => match crypt::decrypt(data, crypt::stem(file_name)) {
			Some(p) => Cow::Owned(p),
			None => return xml_err("encrypted with a key for another file name"),
		},
		Form::Utf16 | Form::Text => Cow::Borrowed(data),
	};
	let (data, converted) = match form {
		Form::Compiled => (text::compile(&plain)?, false),
		Form::Encrypted => {
			let (t, converted) = client_text(&plain)?;
			(crypt::encrypt(&t, crypt::stem(file_name)), converted)
		}
		Form::Utf16 => (
			utf16_bytes(&set_declared_encoding(&as_text(&plain)?, "UTF-16"), false),
			false,
		),
		Form::Text => client_text(&plain)?,
	};
	Ok(Encoded { data, converted })
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn declaration() {
		assert_eq!(
			set_declared_encoding("<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<a/>", "UTF-16"),
			"<?xml version=\"1.0\" encoding=\"UTF-16\" ?>\n<a/>"
		);
		assert_eq!(
			set_declared_encoding("<?xml version='1.0'?><a/>", "UTF-16"),
			"<?xml version='1.0' encoding=\"UTF-16\"?><a/>"
		);
		assert!(set_declared_encoding("<a/>", "UTF-16").starts_with("<?xml version=\"1.0\" encoding=\"UTF-16\"?>"));
	}

	#[test]
	fn forms_the_client_reads() {
		assert!(accepts("Data/skills/client_skills.xml", Form::Compiled));
		assert!(!accepts("dialogs/QUEST_Q1000.html", Form::Compiled));
		assert!(accepts("dialogs/QUEST_Q1000.html", Form::Encrypted));
		assert!(!accepts("LevelData.xml", Form::Compiled));
		assert!(!accepts("ab1\\mission_mission0.xml", Form::Encrypted));
		assert!(!accepts("music/lf1a-bgm.xml", Form::Compiled));
		assert!(accepts("LevelData.xml", Form::Utf16));
		let level = "<?xml version=\"1.0\"?>\n<LevelData/>".as_bytes();
		assert!(encode(level, Form::Compiled, "leveldata.xml").is_err());
	}

	#[test]
	fn utf8_restores_every_declaration() {
		for src in [
			"<?xml version=\"1.0\" encoding=\"utf-16\"?>\r\n<a/>",
			"<?xml version='1.0' encoding='Utf-16' ?><a/>",
			"\r\n <?xml version=\"1.0\" ?>\n<a/>",
			"<?xml version=\"1.0\"?><a/>",
			"<html><body/></html>",
		] {
			for be in [false, true] {
				let original = utf16_bytes(src, be);
				let d = decode(&original, "f.html", Encoding::Utf8).unwrap();
				assert!(
					declared(std::str::from_utf8(&d.text).unwrap()) == Declared::Named("UTF-8".into()),
					"{src}"
				);
				let back = restore(&d.text, d.form, d.utf8.as_ref(), "f.html", false).unwrap();
				assert_eq!(back.data, original, "{src} be={be}");
			}
		}
	}

	#[test]
	fn forms_round_trip() {
		let src = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<a x=\"Привет\">\n\t<b>t</b>\n</a>\n";
		let compiled = encode(src.as_bytes(), Form::Compiled, "f.xml").unwrap().data;
		assert_eq!(detect(&compiled), Form::Compiled);
		let utf16 = encode(&compiled, Form::Utf16, "f.xml").unwrap().data;
		assert_eq!(detect(&utf16), Form::Utf16);
		let enc = encode(&utf16, Form::Encrypted, "dir/F.xml").unwrap().data;
		assert_eq!(detect(&enc), Form::Encrypted);
		let back = encode(&enc, Form::Compiled, "f.xml").unwrap().data;
		assert_eq!(back, compiled);
		let text = encode(src.as_bytes(), Form::Text, "f.xml").unwrap();
		assert!(text.converted && detect(&text.data) == Form::Utf16);
		let d = decode(&utf16, "f.xml", Encoding::Utf8).unwrap();
		assert!(d.utf8.is_some());
		assert_eq!(
			restore(&d.text, Form::Encrypted, d.utf8.as_ref(), "f.xml", false)
				.unwrap()
				.data,
			enc
		);
	}
}
