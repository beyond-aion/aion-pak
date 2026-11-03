//! The `.pak` container: a zip archive whose records may carry bit-inverted signatures, in which case the first
//! bytes of every entry's data are XOR-scrambled.

mod compress;
mod dostime;
mod reader;
mod writer;

pub use compress::{Codec, CodecPool, MAX_LEVEL};
pub use dostime::{DosTime, now as dos_now};
pub use reader::PakReader;
pub use writer::{PakWriter, WriteEntry};

use std::borrow::Cow;

pub const LOCAL_SIG: u32 = 0x0403_4b50;
pub const CENTRAL_SIG: u32 = 0x0201_4b50;
pub const END_SIG: u32 = 0x0605_4b50;

pub const METHOD_STORE: u16 = 0;
pub const METHOD_DEFLATE: u16 = 8;

/// The zip format caps entries per archive and offsets at these values; the client does not read zip64.
pub const MAX_ENTRIES: usize = u16::MAX as usize;
pub const MAX_ARCHIVE_SIZE: u64 = u32::MAX as u64;

static XOR_TABLE: &[u8; 1056] = include_bytes!("xor_table.bin");
const SCRAMBLED_PREFIX: usize = 32;

/// How the archive's records are signed. `Aion` archives also scramble entry data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flavor {
	Plain,
	Aion,
}

impl Flavor {
	pub fn sig(self, sig: u32) -> u32 {
		match self {
			Flavor::Plain => sig,
			Flavor::Aion => !sig,
		}
	}
}

/// Toggles the scrambling of the first bytes of an entry's stored data; the operation is its own inverse.
pub fn scramble(data: &mut [u8], compressed_size: u32) {
	let table = &XOR_TABLE[(compressed_size & 0x3ff) as usize..];
	for (b, k) in data.iter_mut().take(SCRAMBLED_PREFIX).zip(table) {
		*b ^= k;
	}
}

/// The first bytes of an entry's stored data, the part scrambling touches, copied out of the archive.
#[derive(Clone, Copy, Debug, Default)]
pub struct Head {
	bytes: [u8; SCRAMBLED_PREFIX],
	len: usize,
}

impl Head {
	pub fn as_slice(&self) -> &[u8] {
		&self.bytes[..self.len]
	}
}

#[derive(Clone, Debug)]
pub struct Entry {
	pub name: Vec<u8>,
	pub method: u16,
	pub crc: u32,
	pub compressed_size: u32,
	pub size: u32,
	pub header_offset: u32,
	pub scrambled: bool,
	pub time: DosTime,
}

impl Entry {
	pub fn name_lossy(&self) -> Cow<'_, str> {
		String::from_utf8_lossy(&self.name)
	}

	pub fn is_dir(&self) -> bool {
		matches!(self.name.last(), Some(b'/' | b'\\'))
	}
}
