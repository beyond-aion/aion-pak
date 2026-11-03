//! Opening an entry in whatever program the system has for it.

use aionpak_commons::pak::{Codec, Entry, PakReader};
use aionpak_commons::xml::{self, Encoding};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
	/// The bytes exactly as the archive stores them.
	Raw,
	/// XML and HTML decoded into readable UTF-8 text.
	Text,
}

/// Extensions the shell would run rather than show. An archive is not a trusted source.
const EXECUTABLE: [&str; 11] = [
	"exe", "dll", "com", "bat", "cmd", "ps1", "scr", "msi", "lnk", "vbs", "js",
];

pub fn is_executable(name: &str) -> bool {
	EXECUTABLE.contains(&extension(name).as_str())
}

/// Writes the entry to a temporary file and hands it to the system. Returns the path it was written to.
pub fn launch(reader: &PakReader, entry: &Entry, name: &str, mode: Mode) -> Result<PathBuf, String> {
	if is_executable(name) {
		return Err(format!("{name}: refusing to launch an executable from an archive"));
	}
	let extension = extension(name);
	let data = reader
		.read(entry, &mut Codec::new(0))
		.map_err(|e| format!("{name}: {e}"))?;

	let (data, suffix) = match mode {
		Mode::Text => {
			let decoded = xml::decode(&data, name, Encoding::Utf8).map_err(|e| format!("{name}: {e}"))?;
			let suffix = if extension == "html" || extension == "htm" {
				"html"
			} else {
				"xml"
			};
			(decoded.text, suffix.to_owned())
		}
		Mode::Raw => {
			let suffix = if (1..=5).contains(&extension.len()) {
				extension
			} else {
				"bin".to_owned()
			};
			(data, suffix)
		}
	};

	let path = temp_path(name, &suffix);
	std::fs::write(&path, &data).map_err(|e| format!("{}: {e}", path.display()))?;
	shell_open(&path)?;
	Ok(path)
}

fn extension(name: &str) -> String {
	name.rsplit_once('.')
		.map(|(_, ext)| ext.to_ascii_lowercase())
		.filter(|ext| ext.len() <= 8 && ext.chars().all(|c| c.is_ascii_alphanumeric()))
		.unwrap_or_default()
}

/// A name the file system accepts, unique per entry name so two `index.xml` do not overwrite each other.
fn temp_path(name: &str, suffix: &str) -> PathBuf {
	let base = name.rsplit(['/', '\\']).next().unwrap_or("entry");
	let stem = base.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(base);
	let stem: String = stem
		.chars()
		.map(|c| {
			if c.is_alphanumeric() || c == '-' || c == '_' {
				c
			} else {
				'_'
			}
		})
		.take(60)
		.collect();
	let stem = if stem.is_empty() { "entry".to_owned() } else { stem };

	let mut hash: u32 = 2_166_136_261;
	for byte in name.as_bytes() {
		hash ^= u32::from(*byte);
		hash = hash.wrapping_mul(16_777_619);
	}

	let dir = std::env::temp_dir()
		.join("aionpak-preview")
		.join(std::process::id().to_string());
	let _ = std::fs::create_dir_all(&dir);
	dir.join(format!("{stem}-{hash:08x}.{suffix}"))
}

#[cfg(windows)]
fn shell_open(path: &Path) -> Result<(), String> {
	use std::os::windows::ffi::OsStrExt;
	use windows_sys::Win32::UI::Shell::ShellExecuteW;
	use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

	let path: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
	let verb: Vec<u16> = "open\0".encode_utf16().collect();
	// SAFETY: both strings are NUL-terminated and live for the call.
	let result = unsafe {
		ShellExecuteW(
			std::ptr::null_mut(),
			verb.as_ptr(),
			path.as_ptr(),
			std::ptr::null(),
			std::ptr::null(),
			SW_SHOWNORMAL,
		)
	};
	// Anything at or below 32 is an error code, per the ShellExecute documentation.
	if result as isize > 32 {
		Ok(())
	} else {
		Err(format!(
			"cannot open {}: ShellExecuteW returned {}",
			String::from_utf16_lossy(&path[..path.len() - 1]),
			result as isize
		))
	}
}

#[cfg(target_os = "macos")]
fn shell_open(path: &Path) -> Result<(), String> {
	Command::new("open")
		.arg(path)
		.spawn()
		.map(|_| ())
		.map_err(|e| format!("cannot open {}: {e}", path.display()))
}

#[cfg(all(not(windows), not(target_os = "macos")))]
fn shell_open(path: &Path) -> Result<(), String> {
	Command::new("xdg-open")
		.arg(path)
		.spawn()
		.map(|_| ())
		.map_err(|e| format!("cannot open {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn temp_paths_are_unique_and_plain() {
		let a = temp_path("Data/Items/Items.pak", "xml");
		let b = temp_path("Data/Other/Items.pak", "xml");
		assert_ne!(a, b);
		assert_eq!(a.extension().unwrap(), "xml");
		let name = a.file_name().unwrap().to_string_lossy().into_owned();
		assert!(!name.contains('/') && !name.contains('\\'), "{name}");
	}

	#[test]
	fn executables_are_refused() {
		assert!(is_executable("bin32/aion.bin.exe"));
		assert!(is_executable("x.DLL"));
		assert!(!is_executable("Data/skills/skills.xml"));
		assert!(!is_executable("no_extension"));
	}
}
