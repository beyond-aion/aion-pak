use super::{
	CENTRAL_SIG, DosTime, END_SIG, Flavor, LOCAL_SIG, MAX_ARCHIVE_SIZE, MAX_ENTRIES, METHOD_STORE, SCRAMBLED_PREFIX,
	scramble,
};
use crate::error::{Result, format_err};
use std::io::Write;

const VERSION: u16 = 20;
const ATTR_ARCHIVE: u32 = 0x20;

/// One entry ready to be written: `prefix` followed by `data` is already compressed with `method` and not scrambled.
pub struct WriteEntry<'a> {
	pub name: &'a [u8],
	pub method: u16,
	pub crc: u32,
	pub size: u32,
	/// Bytes that come before `data`, so stored data can be written from a copied head and a borrowed rest.
	pub prefix: &'a [u8],
	pub data: &'a [u8],
	pub time: DosTime,
}

/// Writes entries sequentially, then the central directory on `finish`.
pub struct PakWriter<W: Write> {
	out: W,
	flavor: Flavor,
	pos: u64,
	central: Vec<u8>,
	count: usize,
}

impl<W: Write> PakWriter<W> {
	pub fn new(out: W, flavor: Flavor) -> Self {
		PakWriter {
			out,
			flavor,
			pos: 0,
			central: Vec::new(),
			count: 0,
		}
	}

	pub fn size(&self) -> u64 {
		self.pos
	}

	pub fn count(&self) -> usize {
		self.count
	}

	/// The bytes `entry` adds to the archive, local header and central record included.
	pub fn cost(name_len: usize, data_len: usize) -> u64 {
		(30 + 46 + 2 * name_len + data_len) as u64
	}

	pub fn add(&mut self, e: &WriteEntry) -> Result<()> {
		if self.count == MAX_ENTRIES {
			return format_err(format!("more than {MAX_ENTRIES} entries"));
		}
		if e.name.len() > u16::MAX as usize {
			return format_err("entry name too long");
		}
		let len = e.prefix.len() + e.data.len();
		if e.method == METHOD_STORE && len != e.size as usize {
			return format_err("stored entry with differing sizes");
		}
		let offset = self.pos;
		let end = offset + 30 + e.name.len() as u64 + len as u64;
		if end + self.central.len() as u64 + 46 + e.name.len() as u64 + 22 > MAX_ARCHIVE_SIZE {
			return format_err("archive would exceed 4 GiB");
		}
		let csize = len as u32;

		let mut h = Vec::with_capacity(30 + e.name.len());
		put32(&mut h, self.flavor.sig(LOCAL_SIG));
		put16(&mut h, VERSION);
		put16(&mut h, 0);
		put16(&mut h, e.method);
		put16(&mut h, e.time.time);
		put16(&mut h, e.time.date);
		put32(&mut h, e.crc);
		put32(&mut h, csize);
		put32(&mut h, e.size);
		put16(&mut h, e.name.len() as u16);
		put16(&mut h, 0);
		h.extend_from_slice(e.name);
		self.out.write_all(&h)?;
		let split = len.min(SCRAMBLED_PREFIX);
		let from_prefix = split.min(e.prefix.len());
		let mut head = [0; SCRAMBLED_PREFIX];
		head[..from_prefix].copy_from_slice(&e.prefix[..from_prefix]);
		head[from_prefix..split].copy_from_slice(&e.data[..split - from_prefix]);
		if self.flavor == Flavor::Aion {
			scramble(&mut head[..split], csize);
		}
		self.out.write_all(&head[..split])?;
		self.out.write_all(&e.prefix[from_prefix..])?;
		self.out.write_all(&e.data[split - from_prefix..])?;

		let c = &mut self.central;
		put32(c, self.flavor.sig(CENTRAL_SIG));
		put16(c, VERSION);
		put16(c, VERSION);
		put16(c, 0);
		put16(c, e.method);
		put16(c, e.time.time);
		put16(c, e.time.date);
		put32(c, e.crc);
		put32(c, csize);
		put32(c, e.size);
		put16(c, e.name.len() as u16);
		put16(c, 0);
		put16(c, 0);
		put16(c, 0);
		put16(c, 0);
		put32(c, ATTR_ARCHIVE);
		put32(c, offset as u32);
		c.extend_from_slice(e.name);

		self.pos = end;
		self.count += 1;
		Ok(())
	}

	pub fn finish(mut self) -> Result<W> {
		let mut end = Vec::with_capacity(22);
		put32(&mut end, self.flavor.sig(END_SIG));
		put16(&mut end, 0);
		put16(&mut end, 0);
		put16(&mut end, self.count as u16);
		put16(&mut end, self.count as u16);
		put32(&mut end, self.central.len() as u32);
		put32(&mut end, self.pos as u32);
		put16(&mut end, 0);
		self.out.write_all(&self.central)?;
		self.out.write_all(&end)?;
		self.out.flush()?;
		Ok(self.out)
	}
}

fn put16(v: &mut Vec<u8>, x: u16) {
	v.extend_from_slice(&x.to_le_bytes());
}

fn put32(v: &mut Vec<u8>, x: u32) {
	v.extend_from_slice(&x.to_le_bytes());
}
