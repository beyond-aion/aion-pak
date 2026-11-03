//! `.aionpak` next to extracted files: what the archive looked like, so `pack` can rebuild it. One tab-separated
//! line per entry, in archive order: escaped name, method, DOS date, DOS time, CRC-32 of the extracted file, the
//! XML form the file had in the archive when it was decoded on extraction (`-` otherwise), and, when decoding turned
//! UTF-16 text into UTF-8, what it looked like before (`utf8`, then `,be` for big-endian and `,decl=<encoding>`,
//! `,nodecl` or `,noenc` for the original declaration; `-` otherwise). The header names the archive the files came
//! from, with its size and modification time, so that `pack` can copy what was not changed instead of compressing
//! it again.

use crate::error::{Result, format_err};
use crate::pak::{DosTime, Flavor, PakReader};
use crate::xml::{Declared, Form, Utf8Origin};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub const FILE_NAME: &str = ".aionpak";
const HEADER: &str = "aionpak-manifest 1";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestEntry {
	pub name: Vec<u8>,
	pub method: u16,
	pub time: DosTime,
	pub crc: u32,
	pub form: Option<Form>,
	pub utf8: Option<Utf8Origin>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
	pub flavor: Flavor,
	pub source: Option<Source>,
	pub entries: Vec<ManifestEntry>,
}

/// The archive a folder was extracted from, as it was then.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
	pub path: PathBuf,
	pub size: u64,
	/// Nanoseconds since the Unix epoch.
	pub modified: u128,
}

impl Source {
	pub fn of(path: &Path) -> Option<Self> {
		let path = std::path::absolute(path).ok()?;
		path.to_str()?;
		let meta = std::fs::metadata(&path).ok()?;
		let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_nanos();
		Some(Source {
			size: meta.len(),
			modified,
			path,
		})
	}

	/// The archive, if it is still the one the files were extracted from.
	pub fn open(&self) -> Option<PakReader> {
		if Source::of(&self.path).as_ref() != Some(self) {
			return None;
		}
		PakReader::open(&self.path).ok()
	}
}

fn escape(name: &[u8], out: &mut String) {
	for &b in name {
		if b.is_ascii_graphic() && b != b'%' || b == b' ' {
			out.push(b as char);
		} else {
			let _ = write!(out, "%{b:02X}");
		}
	}
}

fn unescape(s: &str) -> Result<Vec<u8>> {
	let b = s.as_bytes();
	let mut out = Vec::with_capacity(b.len());
	let mut i = 0;
	while i < b.len() {
		if b[i] == b'%' {
			let hex = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok());
			let Some(v) = hex else {
				return format_err(format!("bad escape in manifest name {s}"));
			};
			out.push(v);
			i += 3;
		} else {
			out.push(b[i]);
			i += 1;
		}
	}
	Ok(out)
}

fn origin_text(o: &Utf8Origin) -> String {
	let mut s = String::from("utf8");
	if o.big_endian {
		s += ",be";
	}
	match &o.declared {
		Declared::Missing => s += ",nodecl",
		Declared::Unnamed => s += ",noenc",
		Declared::Named(v) => {
			s += ",decl=";
			let mut esc = String::new();
			escape(v.as_bytes(), &mut esc);
			s += &esc.replace(',', "%2C");
		}
	}
	s
}

fn parse_origin(s: &str) -> Result<Option<Utf8Origin>> {
	if s == "-" {
		return Ok(None);
	}
	let mut parts = s.split(',');
	if parts.next() != Some("utf8") {
		return format_err(format!("bad utf8 field {s} in manifest"));
	}
	let mut origin = Utf8Origin {
		big_endian: false,
		declared: Declared::Missing,
	};
	for part in parts {
		match part {
			"be" => origin.big_endian = true,
			"nodecl" => origin.declared = Declared::Missing,
			"noenc" => origin.declared = Declared::Unnamed,
			_ => match part.strip_prefix("decl=") {
				Some(v) => origin.declared = Declared::Named(String::from_utf8_lossy(&unescape(v)?).into_owned()),
				None => return format_err(format!("bad utf8 field {s} in manifest")),
			},
		}
	}
	Ok(Some(origin))
}

fn form_name(f: Option<Form>) -> &'static str {
	f.map_or("-", Form::name)
}

fn parse_form(s: &str) -> Result<Option<Form>> {
	Ok(Some(match s {
		"-" => return Ok(None),
		"compiled" => Form::Compiled,
		"encrypted" => Form::Encrypted,
		"utf16" => Form::Utf16,
		"text" => Form::Text,
		_ => return format_err(format!("unknown form {s} in manifest")),
	}))
}

impl Manifest {
	pub fn to_text(&self) -> String {
		let flavor = match self.flavor {
			Flavor::Plain => "plain",
			Flavor::Aion => "aion",
		};
		let mut s = format!("{HEADER}\tflavor={flavor}");
		if let Some(src) = &self.source
			&& let Some(path) = src.path.to_str()
		{
			s += "\tsource=";
			escape(path.as_bytes(), &mut s);
			let _ = write!(s, "\tsize={}\tmodified={}", src.size, src.modified);
		}
		s.push('\n');
		for e in &self.entries {
			escape(&e.name, &mut s);
			let utf8 = e.utf8.as_ref().map_or("-".to_owned(), origin_text);
			let _ = writeln!(
				s,
				"\t{}\t{}\t{}\t{:08x}\t{}\t{utf8}",
				e.method,
				e.time.date,
				e.time.time,
				e.crc,
				form_name(e.form)
			);
		}
		s
	}

	pub fn parse(text: &str) -> Result<Self> {
		let mut lines = text.lines();
		let header = lines.next().unwrap_or("");
		let mut parts = header.split('\t');
		if parts.next() != Some(HEADER) {
			return format_err("not an aionpak manifest");
		}
		let mut flavor = Flavor::Plain;
		let (mut path, mut size, mut modified) = (None, None, None);
		for part in parts {
			match part.split_once('=') {
				Some(("flavor", "aion")) => flavor = Flavor::Aion,
				Some(("source", p)) => path = String::from_utf8(unescape(p)?).ok().map(PathBuf::from),
				Some(("size", v)) => size = v.parse().ok(),
				Some(("modified", v)) => modified = v.parse().ok(),
				_ => {}
			}
		}
		let source = match (path, size, modified) {
			(Some(path), Some(size), Some(modified)) => Some(Source { path, size, modified }),
			_ => None,
		};
		let mut entries = Vec::new();
		for (n, line) in lines.enumerate().filter(|(_, l)| !l.is_empty()) {
			let f: Vec<&str> = line.split('\t').collect();
			let bad = || format_err(format!("manifest line {}: malformed", n + 2));
			if f.len() != 7 {
				return bad();
			}
			let (Ok(method), Ok(date), Ok(time), Ok(crc)) =
				(f[1].parse(), f[2].parse(), f[3].parse(), u32::from_str_radix(f[4], 16))
			else {
				return bad();
			};
			entries.push(ManifestEntry {
				name: unescape(f[0])?,
				method,
				time: DosTime { date, time },
				crc,
				form: parse_form(f[5])?,
				utf8: parse_origin(f[6])?,
			});
		}
		Ok(Manifest {
			flavor,
			source,
			entries,
		})
	}

	pub fn load(dir: &Path) -> Result<Option<Self>> {
		match std::fs::read_to_string(dir.join(FILE_NAME)) {
			Ok(t) => Ok(Some(Self::parse(&t)?)),
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
			Err(e) => Err(e.into()),
		}
	}

	pub fn save(&self, dir: &Path) -> Result<()> {
		std::fs::write(dir.join(FILE_NAME), self.to_text())?;
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trip() {
		let m = Manifest {
			flavor: Flavor::Aion,
			source: Some(Source {
				path: PathBuf::from("C:\\a b\\x%=.pak"),
				size: 12,
				modified: 1_700_000_000_123_456_789,
			}),
			entries: vec![
				ManifestEntry {
					name: b"a b/%c\t.xml".to_vec(),
					method: 8,
					time: DosTime { date: 1, time: 2 },
					crc: 0xdeadbeef,
					form: Some(Form::Compiled),
					utf8: Some(Utf8Origin {
						big_endian: true,
						declared: Declared::Named("utf-16".into()),
					}),
				},
				ManifestEntry {
					name: vec![0xb0, 0xa1, b'/'],
					method: 0,
					time: DosTime::default(),
					crc: 0,
					form: None,
					utf8: None,
				},
			],
		};
		assert_eq!(Manifest::parse(&m.to_text()).unwrap(), m);
	}
}
