//! Linux only: registers the app with the desktop so the task bar shows its icon.
//!
//! Wayland has no way for a window to hand over its icon (winit's `set_window_icon` does nothing there); the
//! compositor looks up the `.desktop` file named after the window's app id and takes the icon it names. Without
//! one, KDE shows a generic "W". So on start the app writes `aionpak.desktop` and its icon into the user's data
//! folder, and rewrites them only when they differ, for example after the program moved.

use std::path::{Path, PathBuf};

/// The window's app id, the `.desktop` file's name and the icon's name.
pub const APP_ID: &str = "aionpak";

const ICON_SIZES: [u32; 4] = [48, 128, 256, 512];

fn data_home() -> Option<PathBuf> {
	std::env::var_os("XDG_DATA_HOME")
		.map(PathBuf::from)
		.filter(|path| path.is_absolute())
		.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

fn entry(exe: &Path) -> String {
	// The Exec key quotes the path, so a folder with spaces (or Cyrillic) still starts.
	let exe = exe.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
	format!(
		"[Desktop Entry]\n\
		 Type=Application\n\
		 Name=Aion Pak\n\
		 GenericName=Archive tool\n\
		 Comment=Browse, extract, pack and check Aion client .pak archives\n\
		 Exec=\"{exe}\" %f\n\
		 Icon={APP_ID}\n\
		 Terminal=false\n\
		 Categories=Utility;Archiving;\n\
		 StartupWMClass={APP_ID}\n"
	)
}

/// Writes the menu entry and the icons when they are missing or out of date. Failures are ignored: the app
/// works the same, only with the generic icon.
pub fn register() {
	// A distribution package (the AUR one, say) installs its own entry for every user; a second one would only
	// shadow it.
	if installed_system_wide() {
		return;
	}
	let (Some(data), Ok(exe)) = (data_home(), std::env::current_exe()) else {
		return;
	};
	let desktop = data.join("applications").join(format!("{APP_ID}.desktop"));
	let wanted = entry(&exe);
	let icons_present = ICON_SIZES
		.iter()
		.all(|size| icon_path(&data, *size).exists());
	if icons_present && std::fs::read_to_string(&desktop).is_ok_and(|current| current == wanted) {
		return;
	}
	for size in ICON_SIZES {
		let path = icon_path(&data, size);
		let icon = crate::theme::app_icon(size);
		if let Some(dir) = path.parent() {
			let _ = std::fs::create_dir_all(dir);
		}
		let _ = image::save_buffer(&path, &icon.rgba, size, size, image::ColorType::Rgba8);
	}
	if let Some(dir) = desktop.parent() {
		let _ = std::fs::create_dir_all(dir);
	}
	let _ = std::fs::write(&desktop, wanted);
}

/// Whether a menu entry is installed in one of the system data folders (`XDG_DATA_DIRS`).
fn installed_system_wide() -> bool {
	let dirs = std::env::var("XDG_DATA_DIRS")
		.ok()
		.filter(|dirs| !dirs.is_empty())
		.unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
	dirs.split(':')
		.filter(|dir| !dir.is_empty())
		.any(|dir| Path::new(dir).join("applications").join(format!("{APP_ID}.desktop")).exists())
}

fn icon_path(data: &Path, size: u32) -> PathBuf {
	data.join(format!("icons/hicolor/{size}x{size}/apps/{APP_ID}.png"))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn exec_is_quoted() {
		let text = entry(Path::new("/run/media/user/Новый том/aion pak/aionpak"));
		assert!(text.contains("Exec=\"/run/media/user/Новый том/aion pak/aionpak\" %f\n"), "{text}");
		assert!(text.contains("StartupWMClass=aionpak\n"));
	}
}
