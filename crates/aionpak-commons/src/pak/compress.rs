use super::{METHOD_DEFLATE, METHOD_STORE};
use crate::error::{Result, format_err};
use libdeflater::{CompressionLvl, Compressor, Decompressor};
use std::sync::Mutex;

/// The highest compression level; 0 stores data uncompressed.
pub const MAX_LEVEL: u8 = 12;

/// The largest compression buffer a codec keeps between calls; bigger ones are freed after use.
const KEPT_SCRATCH: usize = 64 << 20;

/// Per-thread deflate state. Not `Sync`: give every worker its own.
pub struct Codec {
	level: u8,
	compressor: Option<Compressor>,
	decompressor: Decompressor,
	scratch: Vec<u8>,
}

impl Codec {
	pub fn new(level: u8) -> Self {
		let level = level.min(MAX_LEVEL);
		let compressor = (level > 0).then(|| Compressor::new(CompressionLvl::new(level as i32).unwrap()));
		Codec {
			level,
			compressor,
			decompressor: Decompressor::new(),
			scratch: Vec::new(),
		}
	}

	pub fn level(&self) -> u8 {
		self.level
	}

	/// Returns the method and the stored bytes; falls back to STORE when deflate does not shrink the data.
	pub fn compress(&mut self, data: Vec<u8>) -> (u16, Vec<u8>) {
		if let Some(c) = &mut self.compressor
			&& !data.is_empty()
		{
			let bound = c.deflate_compress_bound(data.len());
			let mut once = Vec::new();
			let out = if bound <= KEPT_SCRATCH {
				if self.scratch.len() < bound {
					self.scratch.resize(bound, 0);
				}
				&mut self.scratch
			} else {
				once.resize(bound, 0);
				&mut once
			};
			if let Ok(n) = c.deflate_compress(&data, out)
				&& n < data.len()
			{
				return (METHOD_DEFLATE, out[..n].to_vec());
			}
		}
		(METHOD_STORE, data)
	}

	/// Decompresses into `out`, whose old contents are overwritten.
	pub fn inflate(&mut self, data: &[u8], size: usize, out: &mut Vec<u8>) -> Result<()> {
		// Only bytes past the old length get zeroed: a reused buffer costs nothing.
		out.resize(size, 0);
		match self.decompressor.deflate_decompress(data, out) {
			Ok(n) if n == size => Ok(()),
			Ok(n) => format_err(format!("inflated to {n} bytes, expected {size}")),
			Err(e) => format_err(format!("corrupt deflate stream: {e:?}")),
		}
	}
}

/// Codecs shared by the threads of one job, each lent out for one item at a time.
pub struct CodecPool {
	level: u8,
	free: Mutex<Vec<Codec>>,
}

impl CodecPool {
	pub fn new(level: u8) -> Self {
		CodecPool {
			level,
			free: Mutex::new(Vec::new()),
		}
	}

	pub fn with<R>(&self, f: impl FnOnce(&mut Codec) -> R) -> R {
		let taken = self.free.lock().unwrap().pop();
		let mut codec = taken.unwrap_or_else(|| Codec::new(self.level));
		let r = f(&mut codec);
		self.free.lock().unwrap().push(codec);
		r
	}
}
