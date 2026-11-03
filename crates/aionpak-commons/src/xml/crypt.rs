//! `0x81` files: text XOR-ed with a byte stream keyed by the file name without directory and extension.

pub const MAGIC: u8 = 0x81;

struct Stream {
	key: u8,
	step: u8,
}

impl Stream {
	fn new(stem: &[u8]) -> Self {
		let (mut sum, mut x) = (0u8, 0u8);
		for (i, &c) in stem.iter().enumerate() {
			let v = (i as u8).wrapping_add(c & 0xf);
			sum = sum.wrapping_add(v);
			x ^= v;
		}
		Stream {
			key: x ^ sum.wrapping_add(29),
			step: x.wrapping_add(3),
		}
	}

	fn next(&mut self) -> u8 {
		self.key = self.step ^ self.key.wrapping_add(29);
		self.step = self.step.wrapping_add(3);
		self.key
	}
}

/// The key stem the client derives from a file name: no directory, no extension.
pub fn stem(file_name: &str) -> &[u8] {
	let base = file_name.rsplit(['/', '\\']).next().unwrap_or(file_name);
	let stem = match base.rfind('.') {
		Some(dot) if dot > 0 => &base[..dot],
		_ => base,
	};
	stem.as_bytes()
}

/// Returns `None` when the check byte does not match the name, which the client rejects as well.
pub fn decrypt(data: &[u8], stem: &[u8]) -> Option<Vec<u8>> {
	if data.len() < 2 || data[0] != MAGIC {
		return None;
	}
	let mut s = Stream::new(stem);
	if s.key ^ data[1] != MAGIC {
		return None;
	}
	Some(data[2..].iter().map(|&b| b ^ s.next()).collect())
}

pub fn encrypt(plain: &[u8], stem: &[u8]) -> Vec<u8> {
	let mut s = Stream::new(stem);
	let mut out = Vec::with_capacity(plain.len() + 2);
	out.push(MAGIC);
	out.push(s.key ^ MAGIC);
	out.extend(plain.iter().map(|&b| b ^ s.next()));
	out
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trip() {
		let plain = b"<html>hello</html>".to_vec();
		let enc = encrypt(&plain, b"Quest_Q1234");
		assert_eq!(decrypt(&enc, b"quest_q1234").unwrap(), plain);
		assert!(decrypt(&enc, b"other").is_none() || decrypt(&enc, b"other").unwrap() != plain);
		assert_eq!(stem("a/b\\Quest_Q1.html"), b"Quest_Q1");
	}
}
