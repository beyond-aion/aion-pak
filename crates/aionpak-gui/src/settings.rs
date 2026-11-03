//! What survives between runs: the look, where the window was, the last destination and the recent archives.
//! A plain `key=value` file, read once at start and written whole on every change.

use crate::theme::{Accent, Density, Look, Theme};
use std::path::{Path, PathBuf};

/// How many recently opened archives and folders the empty workspace offers.
pub const RECENT_LIMIT: usize = 8;

#[derive(Clone)]
pub struct Settings {
	pub look: Look,
	pub density: Density,
	/// Width, height, x and y of the last window that was not maximised.
	pub window: Option<(f32, f32, f32, f32)>,
	/// Where the last extraction went, offered again next time.
	pub out: Option<PathBuf>,
	/// Archives and folders, newest first.
	pub recent: Vec<PathBuf>,
	/// The archive open inside the newest folder when the app closed, opened again with it.
	pub last_archive: Option<PathBuf>,
	/// Open the last place on start; otherwise show the start page with the recent places.
	pub reopen: bool,
	/// The preview pane in Browse: open or closed, and the size it was dragged to beside or under the table.
	pub pane_open: bool,
	pub pane_width: Option<f32>,
	pub pane_height: Option<f32>,
}

impl Default for Settings {
	fn default() -> Self {
		Settings {
			look: Look::default(),
			density: Density::Comfortable,
			window: None,
			out: None,
			recent: Vec::new(),
			last_archive: None,
			reopen: true,
			pane_open: true,
			pane_width: None,
			pane_height: None,
		}
	}
}

fn file() -> Option<PathBuf> {
	let base = if cfg!(windows) {
		std::env::var_os("APPDATA").map(PathBuf::from)
	} else {
		std::env::var_os("XDG_CONFIG_HOME")
			.map(PathBuf::from)
			.or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
	}?;
	Some(base.join("aionpak").join("gui.conf"))
}

impl Settings {
	pub fn load() -> Self {
		let text = file()
			.and_then(|path| std::fs::read_to_string(path).ok())
			.unwrap_or_default();
		Self::parse(&text)
	}

	fn parse(text: &str) -> Self {
		let mut settings = Settings::default();
		let mut accent = None;
		for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
			let value = value.trim();
			match key.trim() {
				"theme" => {
					if let Some((theme, legacy_accent)) = Theme::from_key(value) {
						settings.look.theme = theme;
						// A palette name from an earlier version implies its accent, unless one is stored.
						accent = accent.or(legacy_accent);
					}
				}
				"accent" => accent = Accent::from_key(value).or(accent),
				"density" => {
					if let Some(density) = Density::ALL.into_iter().find(|density| density.key() == value) {
						settings.density = density;
					}
				}
				"window" => {
					let mut parts = value.split(',').map(|part| part.trim().parse::<f32>().ok());
					let mut next = || parts.next().flatten();
					if let (Some(w), Some(h), Some(x), Some(y)) = (next(), next(), next(), next()) {
						settings.window = Some((w, h, x, y));
					}
				}
				"out" if !value.is_empty() => settings.out = Some(PathBuf::from(value)),
				"pane" => {
					let mut parts = value.split(',').map(str::trim);
					settings.pane_open = parts.next() != Some("closed");
					let mut size = || parts.next().and_then(|part| part.parse::<f32>().ok()).filter(|v| *v > 0.0);
					settings.pane_width = size();
					settings.pane_height = size();
				}
				"on_start" => settings.reopen = value != "recent",
				"last_archive" if !value.is_empty() => settings.last_archive = Some(PathBuf::from(value)),
				"recent" if !value.is_empty() && settings.recent.len() < RECENT_LIMIT => {
					settings.recent.push(PathBuf::from(value));
				}
				_ => {}
			}
		}
		if let Some(accent) = accent {
			settings.look.accent = accent;
		}
		settings
	}

	fn render(&self) -> String {
		let look = &self.look;
		let mut text = format!(
			"theme={}\naccent={}\ndensity={}\n",
			look.theme.key(),
			look.accent.key(),
			self.density.key()
		);
		if let Some((w, h, x, y)) = self.window {
			text.push_str(&format!("window={w:.0},{h:.0},{x:.0},{y:.0}\n"));
		}
		let size = |value: Option<f32>| value.map_or_else(String::new, |v| format!("{v:.0}"));
		text.push_str(&format!(
			"pane={},{},{}\n",
			if self.pane_open { "open" } else { "closed" },
			size(self.pane_width),
			size(self.pane_height)
		));
		if let Some(out) = &self.out {
			text.push_str(&format!("out={}\n", out.display()));
		}
		for path in &self.recent {
			text.push_str(&format!("recent={}\n", path.display()));
		}
		text.push_str(if self.reopen { "on_start=last\n" } else { "on_start=recent\n" });
		if let Some(archive) = &self.last_archive {
			text.push_str(&format!("last_archive={}\n", archive.display()));
		}
		text
	}

	/// Writes the file. A failure is not worth bothering the user with: the settings just do not persist.
	pub fn save(&self) {
		let Some(path) = file() else {
			return;
		};
		if let Some(dir) = path.parent() {
			let _ = std::fs::create_dir_all(dir);
		}
		let _ = std::fs::write(path, self.render());
	}

	/// Puts a path at the top of the recent list.
	pub fn remember(&mut self, path: &Path) {
		self.recent.retain(|known| known != path);
		self.recent.insert(0, path.to_path_buf());
		self.recent.truncate(RECENT_LIMIT);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn round_trips() {
		let mut settings = Settings {
			look: Look {
				theme: Theme::Dark,
				accent: Accent::Gold,
			},
			density: Density::Compact,
			window: Some((1200.0, 800.0, 10.0, 20.0)),
			out: Some(PathBuf::from("/tmp/out")),
			recent: Vec::new(),
			last_archive: Some(PathBuf::from("/client/Data/x.pak")),
			reopen: false,
			pane_open: false,
			pane_width: None,
			pane_height: Some(260.0),
		};
		settings.remember(Path::new("/a.pak"));
		settings.remember(Path::new("/b.pak"));
		settings.remember(Path::new("/a.pak"));
		let back = Settings::parse(&settings.render());
		assert_eq!(back.look, settings.look);
		assert_eq!(back.density, Density::Compact);
		assert_eq!(back.window, settings.window);
		assert_eq!(back.out, settings.out);
		assert_eq!(back.last_archive, settings.last_archive);
		assert!(!back.reopen);
		assert!(!back.pane_open);
		assert_eq!((back.pane_width, back.pane_height), (None, Some(260.0)));
		assert_eq!(back.recent, vec![PathBuf::from("/a.pak"), PathBuf::from("/b.pak")]);
	}

	#[test]
	fn old_palette_names_keep_their_look() {
		let settings = Settings::parse("theme=asmodian-neon\n");
		assert_eq!((settings.look.theme, settings.look.accent), (Theme::Dark, Accent::Violet));
		let settings = Settings::parse("theme=daylight\naccent=ember\n");
		assert_eq!((settings.look.theme, settings.look.accent), (Theme::Light, Accent::Ember));
	}
}
