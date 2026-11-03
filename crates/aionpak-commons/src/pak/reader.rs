use super::{
	CENTRAL_SIG, Codec, DosTime, END_SIG, Entry, Flavor, Head, LOCAL_SIG, METHOD_DEFLATE, METHOD_STORE,
	SCRAMBLED_PREFIX, scramble,
};
use crate::error::{Error, Result, format_err};
use memmap2::Mmap;
use std::borrow::Cow;
use std::fs::File;
use std::path::{Path, PathBuf};

const END_LEN: usize = 22;
const CENTRAL_LEN: usize = 46;
const LOCAL_LEN: usize = 30;

// Some clients XOR bytes 4..30 of every local header with the repeating key `4e bd 0f dd 6e a3 ee 1a` and undo it
// in a `ReadFile` hook; these are the key bytes over the name and extra field lengths.
const LOCAL_NAME_KEY: u16 = 0x1aee;
const LOCAL_EXTRA_KEY: u16 = 0xbd4e;

pub struct PakReader {
	path: PathBuf,
	map: Mmap,
	flavor: Flavor,
	entries: Vec<Entry>,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
	u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
	u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn is_sig(value: u32, sig: u32) -> bool {
	value == sig || value == !sig
}

impl PakReader {
	pub fn open(path: &Path) -> Result<Self> {
		let file = File::open(path)?;
		// SAFETY: the archive is only read; a concurrent writer could change the bytes under us, which the
		// bounds checks below turn into format errors rather than undefined reads.
		let map = unsafe { Mmap::map(&file)? };
		// Reading the directory touches a few pages at the end of the file. With the default advice every fault
		// also reads ahead, and on a drive set to a large read-ahead (64 MB is common for USB disks) opening a
		// client's 3 000 archives read most of their data. Random access while parsing, then the normal
		// read-ahead back for the entry data, which is read front to back.
		#[cfg(unix)]
		let _ = map.advise(memmap2::Advice::Random);
		let reader = Self::parse(path.to_path_buf(), map)?;
		#[cfg(unix)]
		let _ = reader.map.advise(memmap2::Advice::Normal);
		Ok(reader)
	}

	fn parse(path: PathBuf, map: Mmap) -> Result<Self> {
		let b = &map[..];
		let (end, flavor) = find_end(b)?;
		let count = u16_at(b, end + 10) as usize;
		let cd_size = u32_at(b, end + 12) as usize;
		let cd_offset = u32_at(b, end + 16) as usize;
		if u16_at(b, end + 4) != 0 || u16_at(b, end + 6) != 0 {
			return format_err("multi-volume archives are not supported");
		}
		if cd_offset.checked_add(cd_size).is_none_or(|e| e > end) {
			return format_err("central directory lies outside the archive");
		}
		let mut entries = Vec::with_capacity(count);
		let mut p = cd_offset;
		for i in 0..count {
			if p + CENTRAL_LEN > end {
				return format_err(format!("central directory truncated at entry {i}"));
			}
			let sig = u32_at(b, p);
			if !is_sig(sig, CENTRAL_SIG) {
				return format_err(format!("bad central directory signature {sig:08x} at entry {i}"));
			}
			let name_len = u16_at(b, p + 28) as usize;
			let extra_len = u16_at(b, p + 30) as usize;
			let comment_len = u16_at(b, p + 32) as usize;
			let name_end = p + CENTRAL_LEN + name_len;
			if name_end > end {
				return format_err(format!("central directory truncated at entry {i}"));
			}
			let entry = Entry {
				name: b[p + CENTRAL_LEN..name_end].to_vec(),
				method: u16_at(b, p + 10),
				time: DosTime {
					time: u16_at(b, p + 12),
					date: u16_at(b, p + 14),
				},
				crc: u32_at(b, p + 16),
				compressed_size: u32_at(b, p + 20),
				size: u32_at(b, p + 24),
				header_offset: u32_at(b, p + 42),
				scrambled: sig == !CENTRAL_SIG,
			};
			if entry.method != METHOD_STORE && entry.method != METHOD_DEFLATE {
				return format_err(format!("{}: unsupported method {}", entry.name_lossy(), entry.method));
			}
			entries.push(entry);
			p = name_end + extra_len + comment_len;
		}
		Ok(PakReader {
			path,
			map,
			flavor,
			entries,
		})
	}

	pub fn path(&self) -> &Path {
		&self.path
	}

	pub fn flavor(&self) -> Flavor {
		self.flavor
	}

	pub fn entries(&self) -> &[Entry] {
		&self.entries
	}

	pub fn size(&self) -> u64 {
		self.map.len() as u64
	}

	/// The entry's data as stored, still scrambled when the entry is.
	pub fn stored(&self, e: &Entry) -> Result<&[u8]> {
		let b = &self.map[..];
		let h = e.header_offset as usize;
		if h + LOCAL_LEN > b.len() || !is_sig(u32_at(b, h), LOCAL_SIG) {
			return format_err(format!("{}: bad local header", e.name_lossy()));
		}
		let (mut name_len, mut extra_len) = (u16_at(b, h + 26), u16_at(b, h + 28));
		if name_len as usize != e.name.len() && (name_len ^ LOCAL_NAME_KEY) as usize == e.name.len() {
			name_len ^= LOCAL_NAME_KEY;
			extra_len ^= LOCAL_EXTRA_KEY;
		}
		let start = h + LOCAL_LEN + name_len as usize + extra_len as usize;
		let end = start + e.compressed_size as usize;
		if end > b.len() {
			return format_err(format!("{}: data runs past the end of the archive", e.name_lossy()));
		}
		Ok(&b[start..end])
	}

	/// The entry's data as stored, unscrambled.
	pub fn raw(&self, e: &Entry) -> Result<Cow<'_, [u8]>> {
		let data = self.stored(e)?;
		if !e.scrambled || data.is_empty() {
			return Ok(Cow::Borrowed(data));
		}
		let mut owned = data.to_vec();
		scramble(&mut owned, e.compressed_size);
		Ok(Cow::Owned(owned))
	}

	/// The entry's data as stored, unscrambled without copying it: the scrambled head copied out, the rest borrowed.
	pub fn split(&self, e: &Entry) -> Result<(Head, &[u8])> {
		let data = self.stored(e)?;
		let at = data.len().min(SCRAMBLED_PREFIX);
		let mut head = Head {
			len: at,
			..Default::default()
		};
		head.bytes[..at].copy_from_slice(&data[..at]);
		if e.scrambled {
			scramble(&mut head.bytes[..at], e.compressed_size);
		}
		Ok((head, &data[at..]))
	}

	/// Decompresses the entry into `out` and checks its CRC.
	pub fn read_into(&self, e: &Entry, codec: &mut Codec, out: &mut Vec<u8>) -> Result<()> {
		let raw = self.raw(e)?;
		if e.method == METHOD_STORE {
			if e.compressed_size != e.size {
				return format_err(format!("{}: stored entry with differing sizes", e.name_lossy()));
			}
			out.clear();
			out.extend_from_slice(&raw);
		} else {
			codec
				.inflate(&raw, e.size as usize, out)
				.map_err(|err| Error::Format(format!("{}: {err}", e.name_lossy())))?;
		}
		let crc = crc32fast::hash(out);
		if crc != e.crc {
			return format_err(format!("{}: CRC {crc:08x}, expected {:08x}", e.name_lossy(), e.crc));
		}
		Ok(())
	}

	pub fn read(&self, e: &Entry, codec: &mut Codec) -> Result<Vec<u8>> {
		let mut out = Vec::new();
		self.read_into(e, codec, &mut out)?;
		Ok(out)
	}
}

fn find_end(b: &[u8]) -> Result<(usize, Flavor)> {
	if b.len() < END_LEN {
		return Err(Error::NotArchive);
	}
	let lowest = b.len().saturating_sub(END_LEN + u16::MAX as usize);
	for at in (lowest..=b.len() - END_LEN).rev() {
		let sig = u32_at(b, at);
		if is_sig(sig, END_SIG) && at + END_LEN + u16_at(b, at + 20) as usize == b.len() {
			let flavor = if sig == END_SIG { Flavor::Plain } else { Flavor::Aion };
			return Ok((at, flavor));
		}
	}
	Err(Error::NotArchive)
}
