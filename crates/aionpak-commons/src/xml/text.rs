//! Text form of compiled XML. The writer and the parser agree so that compiled → text → compiled is exact:
//! a run of whitespace that contains a line break is layout, everything else (and any CDATA section) is content.

use super::compiled::{self, HAS_ATTRS, HAS_CHILDREN, HAS_TEXT, MAX_DEPTH, PoolStr, Reader};
use crate::error::{Result, xml_err};
use indexmap::IndexSet;
use rustc_hash::FxBuildHasher;
use std::borrow::Cow;

const NL: &str = "\r\n";

/// Where the text goes: UTF-8 into a `String`, or UTF-16LE bytes.
pub trait Out {
	fn push(&mut self, c: char);
	fn push_str(&mut self, s: &str);
}

impl Out for String {
	fn push(&mut self, c: char) {
		String::push(self, c);
	}

	fn push_str(&mut self, s: &str) {
		String::push_str(self, s);
	}
}

pub struct Utf16Le(pub Vec<u8>);

impl Out for Utf16Le {
	fn push(&mut self, c: char) {
		if c.is_ascii() {
			self.0.extend_from_slice(&[c as u8, 0]);
			return;
		}
		let mut units = [0; 2];
		for u in c.encode_utf16(&mut units) {
			self.0.extend_from_slice(&u.to_le_bytes());
		}
	}

	fn push_str(&mut self, s: &str) {
		for c in s.chars() {
			self.push(c);
		}
	}
}

/// Writes compiled XML as text straight from its bytes, without building the tree.
pub fn write(compiled: &[u8], encoding: &str, out: &mut impl Out) -> Result<()> {
	let mut r = Reader::new(compiled)?;
	out.push_str(&format!("<?xml version=\"1.0\" encoding=\"{encoding}\"?>{NL}"));
	write_node(&mut r, 0, true, out)?;
	r.finish()
}

fn write_node(r: &mut Reader, depth: usize, indent: bool, out: &mut impl Out) -> Result<()> {
	if depth > MAX_DEPTH {
		return xml_err("tree too deep");
	}
	let name = r.string()?;
	let kind = r.kind()?;
	let text = if kind & HAS_TEXT != 0 { Some(r.string()?) } else { None };
	if indent {
		tabs(depth, out);
	}
	out.push('<');
	raw(name, out)?;
	if kind & HAS_ATTRS != 0 {
		for _ in 0..r.varint()? {
			let (k, v) = (r.string()?, r.string()?);
			out.push(' ');
			raw(k, out)?;
			out.push_str("=\"");
			escape(v, true, out)?;
			out.push('"');
		}
	}
	let children = if kind & HAS_CHILDREN != 0 { r.varint()? } else { 0 };
	match (text, children == 0) {
		(None, true) => out.push_str("/>"),
		(Some(t), true) => {
			out.push('>');
			write_text(t, false, out)?;
			close(name, out)?;
		}
		(text, false) => {
			out.push('>');
			let mut first_indent = true;
			if let Some(t) = text {
				write_text(t, true, out)?;
				first_indent = false;
			} else {
				out.push_str(NL);
			}
			for i in 0..children {
				write_node(r, depth + 1, first_indent || i > 0, out)?;
				out.push_str(NL);
			}
			tabs(depth, out);
			close(name, out)?;
		}
	}
	if depth == 0 {
		out.push_str(NL);
	}
	Ok(())
}

fn tabs(depth: usize, out: &mut impl Out) {
	for _ in 0..depth {
		out.push('\t');
	}
}

fn raw(s: PoolStr, out: &mut impl Out) -> Result<()> {
	for c in s.chars() {
		out.push(c?);
	}
	Ok(())
}

fn close(name: PoolStr, out: &mut impl Out) -> Result<()> {
	out.push_str("</");
	raw(name, out)?;
	out.push('>');
	Ok(())
}

fn write_text(t: PoolStr, mixed: bool, out: &mut impl Out) -> Result<()> {
	// Whitespace-only text would read back as layout; a CDATA section keeps it content. Such text cannot hold "]]>".
	let mut blank = true;
	let mut line_break = false;
	for c in t.chars() {
		let c = c?;
		blank &= c.is_whitespace();
		line_break |= c == '\n';
	}
	if blank && (line_break || mixed) {
		out.push_str("<![CDATA[");
		raw(t, out)?;
		out.push_str("]]>");
		return Ok(());
	}
	escape(t, false, out)
}

fn escape(s: PoolStr, attr: bool, out: &mut impl Out) -> Result<()> {
	for c in s.chars() {
		match c? {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' if attr => out.push_str("&quot;"),
			c @ ('\t' | '\n') if attr => out.push_str(&format!("&#{};", c as u32)),
			'\r' => out.push_str("&#13;"),
			c if (c as u32) < 0x20 && c != '\t' && c != '\n' => out.push_str(&format!("&#x{:X};", c as u32)),
			c => out.push(c),
		}
	}
	Ok(())
}

/// Text parsed into a flat document: elements in document order, each followed by its descendants, attributes in
/// one list and every distinct string stored once, in the encoding of the text. Encodes straight to compiled XML.
struct Doc<'a> {
	/// Strings are UTF-16LE, not UTF-8.
	utf16: bool,
	strings: IndexSet<Cow<'a, [u8]>, FxBuildHasher>,
	elems: Vec<Elem>,
	attrs: Vec<(u32, u32)>,
}

#[derive(Clone, Copy)]
struct Elem {
	name: u32,
	/// `NO_TEXT` when the element has none.
	text: u32,
	attrs: u32,
	attr_count: u32,
	children: u32,
}

const NO_TEXT: u32 = u32::MAX;

impl Elem {
	fn kind(&self) -> u32 {
		(if self.text != NO_TEXT { HAS_TEXT } else { 0 })
			| (if self.attr_count > 0 { HAS_ATTRS } else { 0 })
			| (if self.children > 0 { HAS_CHILDREN } else { 0 })
	}
}

/// Parses text into compiled XML: UTF-16 with a byte order mark, else UTF-8. UTF-16 is parsed as it is, without
/// a converted copy.
pub fn compile(data: &[u8]) -> Result<Vec<u8>> {
	let Some(big_endian) = super::utf16_bom(data) else {
		return Ok(parse_doc(super::utf8_text(data)?.as_bytes())?.encode());
	};
	// A stray last byte is not part of the text.
	let units = data[2..data.len() & !1].as_chunks::<2>().0;
	let units = if big_endian {
		Cow::Owned(units.iter().map(|&[a, b]| [b, a]).collect())
	} else {
		Cow::Borrowed(units)
	};
	if !valid_utf16(&units) {
		return xml_err("invalid UTF-16");
	}
	Ok(parse_doc(&units)?.encode())
}

fn valid_utf16(units: &[[u8; 2]]) -> bool {
	let surrogates = units.iter().fold(false, |any, u| any | (u[1] & 0xf8 == 0xd8));
	!surrogates || char::decode_utf16(units.iter().map(|u| u16::from_le_bytes(*u))).all(|c| c.is_ok())
}

fn parse_doc<U: Unit>(s: &[U]) -> Result<Doc<'_>> {
	let mut strings = IndexSet::with_hasher(FxBuildHasher);
	strings.insert(Cow::Borrowed(&[][..]));
	let mut p = Parser {
		s,
		at: 0,
		doc: Doc {
			utf16: U::UTF16,
			strings,
			elems: Vec::new(),
			attrs: Vec::new(),
		},
	};
	p.skip_prolog()?;
	p.element(0)?;
	p.skip_misc();
	if p.at != s.len() {
		return p.fail("content after the root element");
	}
	Ok(p.doc)
}

impl Doc<'_> {
	/// The pool lists strings in order of first use, the empty string first at offset 0.
	fn encode(&self) -> Vec<u8> {
		let mut enc = DocEncoder {
			doc: self,
			offsets: vec![u32::MAX; self.strings.len()],
			pool: vec![0, 0],
			body: Vec::new(),
			next: 0,
		};
		enc.offsets[0] = 0;
		enc.elem();
		let mut out = Vec::with_capacity(8 + enc.pool.len() + enc.body.len());
		out.push(compiled::MAGIC);
		compiled::put_varint(&mut out, enc.pool.len() as u32);
		out.extend_from_slice(&enc.pool);
		out.extend_from_slice(&enc.body);
		out
	}
}

struct DocEncoder<'d, 'a> {
	doc: &'d Doc<'a>,
	offsets: Vec<u32>,
	pool: Vec<u8>,
	body: Vec<u8>,
	next: usize,
}

impl DocEncoder<'_, '_> {
	fn string(&mut self, id: u32) {
		let o = &mut self.offsets[id as usize];
		if *o == u32::MAX {
			*o = (self.pool.len() / 2) as u32;
			let s = &self.doc.strings[id as usize];
			if self.doc.utf16 {
				self.pool.extend_from_slice(s);
			} else {
				for u in String::from_utf8_lossy(s).encode_utf16() {
					self.pool.extend_from_slice(&u.to_le_bytes());
				}
			}
			self.pool.extend_from_slice(&[0, 0]);
		}
		let o = *o;
		compiled::put_varint(&mut self.body, o);
	}

	fn elem(&mut self) {
		let e = self.doc.elems[self.next];
		self.next += 1;
		self.string(e.name);
		let kind = e.kind();
		compiled::put_varint(&mut self.body, kind);
		if kind & HAS_TEXT != 0 {
			self.string(e.text);
		}
		if kind & HAS_ATTRS != 0 {
			compiled::put_varint(&mut self.body, e.attr_count);
			for i in e.attrs..e.attrs + e.attr_count {
				let (k, v) = self.doc.attrs[i as usize];
				self.string(k);
				self.string(v);
			}
		}
		if kind & HAS_CHILDREN != 0 {
			compiled::put_varint(&mut self.body, e.children);
			for _ in 0..e.children {
				self.elem();
			}
		}
	}
}

/// A code unit of the text being parsed: a UTF-8 byte or a UTF-16LE unit. All markup is ASCII, so the parser
/// compares units with ASCII and decodes a character only where non-ASCII text matters (whitespace, names).
trait Unit: Copy + Eq {
	const UTF16: bool;
	fn value(self) -> u32;
	fn bytes(s: &[Self]) -> &[u8];
	fn into_bytes(v: Vec<Self>) -> Vec<u8>;
	fn push(out: &mut Vec<Self>, c: char);
	/// The character `s` starts with and how many units it takes. `s` is valid text and not empty.
	fn first_char(s: &[Self]) -> (char, usize);
}

impl Unit for u8 {
	const UTF16: bool = false;

	fn value(self) -> u32 {
		self as u32
	}

	fn bytes(s: &[u8]) -> &[u8] {
		s
	}

	fn into_bytes(v: Vec<u8>) -> Vec<u8> {
		v
	}

	fn push(out: &mut Vec<u8>, c: char) {
		out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
	}

	fn first_char(s: &[u8]) -> (char, usize) {
		let len = match s[0] {
			..0x80 => 1,
			0x80..0xe0 => 2,
			0xe0..0xf0 => 3,
			_ => 4,
		}
		.min(s.len());
		let c = std::str::from_utf8(&s[..len]).ok().and_then(|t| t.chars().next());
		(c.unwrap_or(char::REPLACEMENT_CHARACTER), len)
	}
}

impl Unit for [u8; 2] {
	const UTF16: bool = true;

	fn value(self) -> u32 {
		u16::from_le_bytes(self) as u32
	}

	fn bytes(s: &[Self]) -> &[u8] {
		s.as_flattened()
	}

	fn into_bytes(v: Vec<Self>) -> Vec<u8> {
		v.into_flattened()
	}

	fn push(out: &mut Vec<Self>, c: char) {
		out.extend(c.encode_utf16(&mut [0; 2]).iter().map(|u| u.to_le_bytes()));
	}

	fn first_char(s: &[Self]) -> (char, usize) {
		match char::decode_utf16(s.iter().take(2).map(|u| u16::from_le_bytes(*u))).next() {
			Some(Ok(c)) => (c, c.len_utf16()),
			_ => (char::REPLACEMENT_CHARACTER, 1),
		}
	}
}

fn starts_with<U: Unit>(s: &[U], ascii: &str) -> bool {
	s.len() >= ascii.len() && s.iter().zip(ascii.bytes()).all(|(u, b)| u.value() == b as u32)
}

fn find_char<U: Unit>(s: &[U], ascii: u8) -> Option<usize> {
	s.iter().position(|u| u.value() == ascii as u32)
}

fn find<U: Unit>(s: &[U], ascii: &str) -> Option<usize> {
	let mut from = 0;
	loop {
		let i = from + find_char(&s[from..], ascii.as_bytes()[0])?;
		if starts_with(&s[i..], ascii) {
			return Some(i);
		}
		from = i + 1;
	}
}

fn chars<U: Unit>(mut s: &[U]) -> impl Iterator<Item = char> {
	std::iter::from_fn(move || {
		if s.is_empty() {
			return None;
		}
		let (c, n) = U::first_char(s);
		s = &s[n..];
		Some(c)
	})
}

fn is_layout<U: Unit>(s: &[U]) -> bool {
	find_char(s, b'\n').is_some() && chars(s).all(char::is_whitespace)
}

struct Parser<'a, U> {
	s: &'a [U],
	at: usize,
	doc: Doc<'a>,
}

impl<'a, U: Unit> Parser<'a, U> {
	fn fail<T>(&self, msg: &str) -> Result<T> {
		let line = self.s[..self.at].iter().filter(|u| u.value() == b'\n' as u32).count() + 1;
		xml_err(format!("line {line}: {msg}"))
	}

	fn rest(&self) -> &'a [U] {
		&self.s[self.at..]
	}

	fn starts_with(&self, ascii: &str) -> bool {
		starts_with(self.rest(), ascii)
	}

	fn skip_ws(&mut self) {
		while let Some(u) = self.s.get(self.at) {
			match u.value() {
				0x09..=0x0d | 0x20 => self.at += 1,
				0x80.. => match U::first_char(self.rest()) {
					(c, n) if c.is_whitespace() => self.at += n,
					_ => return,
				},
				_ => return,
			}
		}
	}

	fn skip_until(&mut self, end: &str) -> Result<()> {
		match find(self.rest(), end) {
			Some(i) => {
				self.at += i + end.len();
				Ok(())
			}
			None => self.fail(&format!("missing '{end}'")),
		}
	}

	fn skip_misc(&mut self) {
		loop {
			self.skip_ws();
			let end = if self.starts_with("<?") {
				"?>"
			} else if self.starts_with("<!--") {
				"-->"
			} else {
				return;
			};
			if self.skip_until(end).is_err() {
				return;
			}
		}
	}

	fn skip_prolog(&mut self) -> Result<()> {
		if !self.s.is_empty()
			&& let ('\u{feff}', n) = U::first_char(self.s)
		{
			self.at += n;
		}
		loop {
			self.skip_misc();
			if self.starts_with("<!DOCTYPE") {
				self.skip_until(">")?;
			} else {
				return Ok(());
			}
		}
	}

	fn name(&mut self) -> Result<&'a [U]> {
		let start = self.at;
		while let Some(u) = self.s.get(self.at) {
			match u.value() {
				0x09..=0x0d | 0x20 | 0x2f | 0x3c..=0x3e => break,
				0x80.. => match U::first_char(self.rest()) {
					(c, _) if c.is_whitespace() => break,
					(_, n) => self.at += n,
				},
				_ => self.at += 1,
			}
		}
		if self.at == start {
			return self.fail("expected a name");
		}
		Ok(&self.s[start..self.at])
	}

	fn element(&mut self, depth: usize) -> Result<()> {
		if depth > MAX_DEPTH {
			return self.fail("tree too deep");
		}
		if !self.starts_with("<") {
			return self.fail("expected an element");
		}
		self.at += 1;
		let name = self.name()?;
		let me = self.doc.elems.len();
		let elem = Elem {
			name: self.intern(Cow::Borrowed(U::bytes(name))),
			text: NO_TEXT,
			attrs: self.doc.attrs.len() as u32,
			attr_count: 0,
			children: 0,
		};
		self.doc.elems.push(elem);
		loop {
			self.skip_ws();
			if self.starts_with("/>") {
				self.at += 2;
				return Ok(());
			}
			if self.starts_with(">") {
				self.at += 1;
				break;
			}
			let key = self.name()?;
			let key = self.intern(Cow::Borrowed(U::bytes(key)));
			self.skip_ws();
			if !self.starts_with("=") {
				return self.fail("expected '=' after an attribute name");
			}
			self.at += 1;
			self.skip_ws();
			let q = match self.s.get(self.at).map(|u| u.value()) {
				Some(q @ (0x22 | 0x27)) => q as u8,
				_ => return self.fail("expected a quoted attribute value"),
			};
			self.at += 1;
			let Some(len) = find_char(self.rest(), q) else {
				return self.fail("unterminated attribute value");
			};
			let raw = &self.s[self.at..self.at + len];
			self.at += len + 1;
			let value = self.unescape(raw)?;
			let value = self.intern(value);
			self.doc.attrs.push((key, value));
			self.doc.elems[me].attr_count += 1;
		}

		let mut text: Option<Cow<'a, [u8]>> = None;
		let mut segments = 0;
		let mut children = 0;
		loop {
			if self.starts_with("</") {
				self.at += 2;
				if self.name()? != name {
					return self.fail(&format!(
						"mismatched closing tag for <{}>",
						chars(name).collect::<String>()
					));
				}
				self.skip_ws();
				if !self.starts_with(">") {
					return self.fail("expected '>'");
				}
				self.at += 1;
				break;
			}
			if self.starts_with("<![CDATA[") {
				self.at += 9;
				let Some(len) = find(self.rest(), "]]>") else {
					return self.fail("unterminated CDATA section");
				};
				append(&mut text, Cow::Borrowed(U::bytes(&self.s[self.at..self.at + len])));
				self.at += len + 3;
				segments += 1;
			} else if self.starts_with("<!--") {
				self.skip_until("-->")?;
			} else if self.starts_with("<?") {
				self.skip_until("?>")?;
			} else if self.starts_with("<") {
				self.element(depth + 1)?;
				children += 1;
			} else if self.at == self.s.len() {
				return self.fail(&format!("unterminated <{}>", chars(name).collect::<String>()));
			} else {
				let len = find_char(self.rest(), b'<').unwrap_or(self.s.len() - self.at);
				let raw = &self.s[self.at..self.at + len];
				self.at += len;
				segments += 1;
				if !is_layout(raw) {
					let t = self.unescape(raw)?;
					append(&mut text, t);
				}
			}
		}
		self.doc.elems[me].children = children;
		if text.is_some() || (segments == 0 && children == 0) {
			let id = self.intern(text.unwrap_or(Cow::Borrowed(&[])));
			self.doc.elems[me].text = id;
		}
		Ok(())
	}

	fn intern(&mut self, s: Cow<'a, [u8]>) -> u32 {
		match self.doc.strings.get_index_of(&*s) {
			Some(i) => i as u32,
			None => self.doc.strings.insert_full(s).0 as u32,
		}
	}

	fn unescape(&self, raw: &'a [U]) -> Result<Cow<'a, [u8]>> {
		if find_char(raw, b'&').is_none() {
			return Ok(Cow::Borrowed(U::bytes(raw)));
		}
		let mut out = Vec::with_capacity(raw.len());
		let mut rest = raw;
		while let Some(i) = find_char(rest, b'&') {
			out.extend_from_slice(&rest[..i]);
			rest = &rest[i + 1..];
			let Some(end) = find_char(rest, b';') else {
				return self.fail("unterminated entity reference");
			};
			let name: String = chars(&rest[..end]).collect();
			rest = &rest[end + 1..];
			let c = match name.as_str() {
				"amp" => '&',
				"lt" => '<',
				"gt" => '>',
				"quot" => '"',
				"apos" => '\'',
				_ => {
					let code = if let Some(hex) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
						u32::from_str_radix(hex, 16).ok()
					} else if let Some(dec) = name.strip_prefix('#') {
						dec.parse().ok()
					} else {
						None
					};
					match code.and_then(char::from_u32) {
						Some(c) => c,
						None => return self.fail(&format!("unknown entity &{name};")),
					}
				}
			};
			U::push(&mut out, c);
		}
		out.extend_from_slice(rest);
		Ok(Cow::Owned(U::into_bytes(out)))
	}
}

fn append<'a>(text: &mut Option<Cow<'a, [u8]>>, more: Cow<'a, [u8]>) {
	match text {
		None => *text = Some(more),
		Some(t) => t.to_mut().extend_from_slice(&more),
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::xml::{Encoding, render};

	fn utf16(s: &str, big_endian: bool) -> Vec<u8> {
		let mut out = if big_endian { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
		for u in s.encode_utf16() {
			out.extend_from_slice(&if big_endian { u.to_be_bytes() } else { u.to_le_bytes() });
		}
		out
	}

	#[test]
	fn round_trip_edge_cases() {
		let src = "<root a=\"x&#9;&quot;y&quot;&#10;&lt;&amp;&gt;&#13;\">mixed<empty/><blank></blank>\
			<spaces>  </spaces><newline><![CDATA[\r\n\t]]></newline><text>a &amp; b &lt; c&#13;\nd</text>\
			<wsmixed> <k/></wsmixed><emptymixed><![CDATA[]]><k>v</k></emptymixed></root>";
		let compiled = compile(src.as_bytes()).unwrap();
		for enc in [Encoding::Utf16, Encoding::Utf8] {
			let text = render(&compiled, enc).unwrap();
			assert_eq!(compile(&text).unwrap(), compiled, "{}", String::from_utf8_lossy(&text));
		}
		let text = String::from_utf8(render(&compiled, Encoding::Utf8).unwrap()).unwrap();
		assert!(text.contains("<wsmixed><![CDATA[ ]]><k/>"), "{text}");
		assert!(text.contains("<blank></blank>") && text.contains("<empty/>"), "{text}");
	}

	#[test]
	fn foreign_layout() {
		let s = "<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n<a>\n\t<b x=\"1\">\n\t</b>\n\t<c>t</c>\n</a>\n";
		assert_eq!(
			compile(s.as_bytes()).unwrap(),
			compile(b"<a><b x=\"1\"/><c>t</c></a>").unwrap()
		);
	}

	#[test]
	fn utf16_parses_like_utf8() {
		let s = "\u{feff}<?xml version=\"1.0\"?>\r\n<корень\u{3000}a=\"Привет &#x1F600; 😀\">\u{a0}\
			<b\u{2003}/>текст &amp; 😀<![CDATA[ ]]></корень>";
		let expected = compile(s.as_bytes()).unwrap();
		assert_eq!(compile(&utf16(s, false)).unwrap(), expected);
		assert_eq!(compile(&utf16(s, true)).unwrap(), expected);
		let mut odd = utf16(s, false);
		odd.push(0);
		assert_eq!(compile(&odd).unwrap(), expected);
	}

	#[test]
	fn errors() {
		let mut lone = utf16("<a>x</a>", false);
		lone[8..10].copy_from_slice(&0xd800u16.to_le_bytes());
		assert!(compile(&lone).unwrap_err().to_string().contains("invalid UTF-16"));
		let err = compile(&utf16("<a>\n<б></в></a>", false)).unwrap_err().to_string();
		assert!(err.contains("line 2") && err.contains("<б>"), "{err}");
	}
}
