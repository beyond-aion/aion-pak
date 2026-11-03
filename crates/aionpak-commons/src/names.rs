//! Entry names are bytes; the few non-ASCII ones in client archives are Korean in code page 949.

use crate::error::{Result, format_err};
use encoding_rs::EUC_KR;
use std::path::{Component, Path, PathBuf};

pub fn to_text(name: &[u8]) -> String {
	if name.is_ascii() {
		return String::from_utf8_lossy(name).into_owned();
	}
	EUC_KR.decode_without_bom_handling(name).0.into_owned()
}

pub fn from_text(text: &str) -> Result<Vec<u8>> {
	if text.is_ascii() {
		return Ok(text.as_bytes().to_vec());
	}
	let (bytes, _, unmappable) = EUC_KR.encode(text);
	if unmappable {
		return format_err(format!("{text}: name cannot be stored in code page 949"));
	}
	Ok(bytes.into_owned())
}

/// Where an entry lands under `root`. Refuses names that would escape it, and on every system the drive and
/// stream names (`:`) that would on Windows, so an archive extracts the same everywhere.
pub fn entry_path(root: &Path, name: &[u8]) -> Result<PathBuf> {
	let text = to_text(name);
	let mut path = root.to_path_buf();
	for part in text.split(['/', '\\']).filter(|p| !p.is_empty()) {
		let mut c = Path::new(part).components();
		match (c.next(), c.next()) {
			(Some(Component::Normal(_)), None) if !part.contains(':') => path.push(part),
			_ => return format_err(format!("{text}: unsafe entry name")),
		}
	}
	if path == root {
		return format_err(format!("{text}: empty entry name"));
	}
	Ok(path)
}

/// The entry name for a file at `rel` (relative to the folder being packed).
pub fn path_name(rel: &Path) -> Result<Vec<u8>> {
	let mut parts = Vec::new();
	for c in rel.components() {
		match c {
			Component::Normal(p) => match p.to_str() {
				Some(s) => parts.push(s),
				None => return format_err(format!("{}: name is not valid Unicode", rel.display())),
			},
			_ => return format_err(format!("{}: unexpected path component", rel.display())),
		}
	}
	from_text(&parts.join("/"))
}

/// The key the client looks names up by: ASCII lower case, `/` separators.
pub fn lookup_key(name: &[u8]) -> Vec<u8> {
	name.iter()
		.map(|&b| if b == b'\\' { b'/' } else { b.to_ascii_lowercase() })
		.collect()
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn names() {
		let root = Path::new("out");
		assert_eq!(entry_path(root, b"a/b\\c.xml").unwrap(), Path::new("out/a/b/c.xml"));
		assert!(entry_path(root, b"../x").is_err());
		assert!(entry_path(root, b"a/../../x").is_err());
		assert!(entry_path(root, b"C:/x").is_err());
		let korean = from_text("test가.txt").unwrap();
		assert_eq!(to_text(&korean), "test가.txt");
		assert_eq!(path_name(Path::new("a/B.xml")).unwrap(), b"a/B.xml");
	}
}
