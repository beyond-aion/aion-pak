//! Compiled XML: `0x80`, a varint-sized pool of NUL-terminated UTF-16LE strings, then the node tree. A node is
//! its name, flag bits, then the text, the attributes and the children, each present when its flag is set.

use crate::error::{Result, xml_err};

pub const MAGIC: u8 = 0x80;

pub const HAS_TEXT: u32 = 1;
pub const HAS_ATTRS: u32 = 2;
pub const HAS_CHILDREN: u32 = 4;

/// A string of the pool, still in UTF-16LE.
#[derive(Clone, Copy)]
pub struct PoolStr<'a>(&'a [[u8; 2]]);

impl<'a> PoolStr<'a> {
	pub fn chars(self) -> impl Iterator<Item = Result<char>> + 'a {
		char::decode_utf16(self.0.iter().map(|p| u16::from_le_bytes(*p)))
			.map(|c| c.or_else(|_| xml_err("pool string is not valid UTF-16")))
	}
}

/// Reads compiled XML front to back without building the tree: a node's name, kind, text, attributes and child
/// count, then its children, as they are stored.
pub struct Reader<'a> {
	b: &'a [u8],
	at: usize,
	pool: &'a [[u8; 2]],
}

impl<'a> Reader<'a> {
	pub fn new(b: &'a [u8]) -> Result<Self> {
		if b.first() != Some(&MAGIC) {
			return xml_err("not a compiled xml");
		}
		let mut r = Reader { b, at: 1, pool: &[] };
		let pool_len = r.varint()? as usize;
		let Some(pool_bytes) = b.get(r.at..r.at + pool_len) else {
			return xml_err("string pool truncated");
		};
		if !pool_len.is_multiple_of(2) {
			return xml_err("odd string pool size");
		}
		r.at += pool_len;
		r.pool = pool_bytes.as_chunks::<2>().0;
		Ok(r)
	}

	pub fn varint(&mut self) -> Result<u32> {
		let mut v: u64 = 0;
		for shift in (0..35).step_by(7) {
			let Some(&c) = self.b.get(self.at) else {
				return xml_err("compiled xml truncated");
			};
			self.at += 1;
			v |= ((c & 0x7f) as u64) << shift;
			if c & 0x80 == 0 {
				return u32::try_from(v).or_else(|_| xml_err("varint overflow"));
			}
		}
		xml_err("varint too long")
	}

	pub fn kind(&mut self) -> Result<u32> {
		let kind = self.varint()?;
		if kind & !(HAS_TEXT | HAS_ATTRS | HAS_CHILDREN) != 0 {
			return xml_err(format!("unknown node kind {kind}"));
		}
		Ok(kind)
	}

	pub fn string(&mut self) -> Result<PoolStr<'a>> {
		let offset = self.varint()?;
		let Some(tail) = self.pool.get(offset as usize..) else {
			return xml_err(format!("string offset {offset} outside the pool"));
		};
		let Some(len) = tail.iter().position(|&u| u == [0, 0]) else {
			return xml_err("unterminated pool string");
		};
		Ok(PoolStr(&tail[..len]))
	}

	/// Checks that nothing follows the root node.
	pub fn finish(&self) -> Result<()> {
		match self.b.len() - self.at {
			0 => Ok(()),
			n => xml_err(format!("{n} trailing bytes")),
		}
	}
}

pub const MAX_DEPTH: usize = 512;

pub(super) fn put_varint(out: &mut Vec<u8>, mut v: u32) {
	loop {
		let c = (v & 0x7f) as u8;
		v >>= 7;
		if v == 0 {
			out.push(c);
			return;
		}
		out.push(c | 0x80);
	}
}
