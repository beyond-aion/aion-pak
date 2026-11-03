//! The folder side of the workspace: a tree of directories and `.pak` files, one level at a time.

use crate::icons;
use crate::theme::{self, Palette};
use crate::units::human_bytes;
use egui::{RichText, Sense, Vec2};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub struct Row {
	pub name: String,
	/// Relative to the root.
	pub path: PathBuf,
	pub is_dir: bool,
	pub size: u64,
}

pub enum FolderAction {
	None,
	Enter(PathBuf),
	Open(PathBuf),
	Pick { path: PathBuf, ctrl: bool, shift: bool },
}

pub struct FolderView {
	pub root: PathBuf,
	/// Current directory, relative to the root; empty means the root itself.
	pub dir: PathBuf,
	pub filter: String,
	pub rows: Vec<Row>,
	pub selection: BTreeSet<PathBuf>,
	pub error: Option<String>,
	/// Whether the current directory or one below it holds an archive: what "Extract every archive" works on.
	pub paks_below: bool,
	anchor: Option<PathBuf>,
	/// The row the arrow keys are on, and whether it still has to be scrolled into view.
	cursor: Option<PathBuf>,
	scroll_to_cursor: bool,
}

impl FolderView {
	pub fn open(root: PathBuf) -> Self {
		let mut view = FolderView {
			root,
			dir: PathBuf::new(),
			filter: String::new(),
			rows: Vec::new(),
			selection: BTreeSet::new(),
			error: None,
			paks_below: false,
			anchor: None,
			cursor: None,
			scroll_to_cursor: false,
		};
		view.rescan();
		view
	}

	pub fn enter(&mut self, dir: PathBuf) {
		// Going up, the cursor lands on the folder just left, so the arrows carry on from there.
		let left = self.dir.ancestors().find(|a| a.parent() == Some(dir.as_path())).map(Path::to_path_buf);
		self.dir = dir;
		self.filter.clear();
		self.selection.clear();
		self.anchor = None;
		self.cursor = left;
		self.scroll_to_cursor = self.cursor.is_some();
		self.rescan();
	}

	fn rescan(&mut self) {
		let path = self.root.join(&self.dir);
		self.paks_below = any_pak(&path);
		match std::fs::read_dir(&path) {
			Ok(entries) => {
				let mut rows: Vec<Row> = entries
					.filter_map(Result::ok)
					.filter_map(|entry| {
						let name = entry.file_name().to_string_lossy().into_owned();
						let meta = entry.metadata().ok()?;
						let is_dir = meta.is_dir();
						if !is_dir && !name.to_ascii_lowercase().ends_with(".pak") {
							return None;
						}
						Some(Row {
							path: self.dir.join(&name),
							name,
							is_dir,
							size: if is_dir { 0 } else { meta.len() },
						})
					})
					.collect();
				rows.sort_by(|a, b| {
					b.is_dir
						.cmp(&a.is_dir)
						.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
				});
				self.rows = rows;
				self.error = None;
			}
			Err(err) => {
				self.rows.clear();
				self.error = Some(err.to_string());
			}
		}
	}

	/// Rows that pass the filter, as indexes into `rows`.
	pub fn shown(&self) -> Vec<usize> {
		let filter = self.filter.trim().to_lowercase();
		self.rows
			.iter()
			.enumerate()
			.filter(|(_, row)| filter.is_empty() || crate::browse::matches(&filter, &row.name.to_lowercase()))
			.map(|(index, _)| index)
			.collect()
	}

	pub fn select(&mut self, path: PathBuf, ctrl: bool, shift: bool) {
		if shift && let Some(anchor) = self.anchor.clone() {
			let shown = self.shown();
			let from = shown.iter().position(|index| self.rows[*index].path == anchor);
			let to = shown.iter().position(|index| self.rows[*index].path == path);
			if let (Some(from), Some(to)) = (from, to) {
				let (low, high) = if from <= to { (from, to) } else { (to, from) };
				self.selection.clear();
				for index in &shown[low..=high] {
					if !self.rows[*index].is_dir {
						self.selection.insert(self.rows[*index].path.clone());
					}
				}
				return;
			}
		}
		if ctrl {
			if !self.selection.remove(&path) {
				self.selection.insert(path.clone());
			}
		} else {
			self.selection.clear();
			self.selection.insert(path.clone());
		}
		self.cursor = Some(path.clone());
		self.anchor = Some(path);
	}

	/// The keys the archive table answers too: arrows move, Enter goes in or opens, Backspace goes up.
	fn keys(&mut self, ui: &egui::Ui) -> FolderAction {
		let typing = ui.ctx().memory(|m| m.focused().is_some());
		if typing {
			return FolderAction::None;
		}
		let (step, enter, back) = ui.input(|i| {
			(
				i.num_presses(egui::Key::ArrowDown) as i32 - i.num_presses(egui::Key::ArrowUp) as i32,
				i.key_pressed(egui::Key::Enter),
				i.key_pressed(egui::Key::Backspace),
			)
		});
		if back && !self.dir.as_os_str().is_empty() {
			return FolderAction::Enter(self.dir.parent().map(PathBuf::from).unwrap_or_default());
		}
		let shown = self.shown();
		if step != 0 && !shown.is_empty() {
			let at = self
				.cursor
				.as_ref()
				.and_then(|cursor| shown.iter().position(|&i| &self.rows[i].path == cursor));
			let next = match at {
				Some(at) => (at as i32 + step).clamp(0, shown.len() as i32 - 1) as usize,
				None if step > 0 => 0,
				None => shown.len() - 1,
			};
			let row = &self.rows[shown[next]];
			let path = row.path.clone();
			if row.is_dir {
				self.selection.clear();
				self.cursor = Some(path.clone());
				self.anchor = Some(path);
			} else {
				self.select(path, false, false);
			}
			self.scroll_to_cursor = true;
		}
		if enter && let Some(cursor) = &self.cursor {
			let is_dir = self.rows.iter().any(|row| &row.path == cursor && row.is_dir);
			return if is_dir {
				FolderAction::Enter(cursor.clone())
			} else {
				FolderAction::Open(cursor.clone())
			};
		}
		FolderAction::None
	}

	pub fn select_all_shown(&mut self) {
		self.selection = self
			.shown()
			.into_iter()
			.filter(|index| !self.rows[*index].is_dir)
			.map(|index| self.rows[index].path.clone())
			.collect();
	}

	pub fn selected_paks(&self) -> Vec<PathBuf> {
		self.selection.iter().cloned().collect()
	}

	/// Breadcrumb: the root, then one button per directory level.
	pub fn breadcrumb(&mut self, ui: &mut egui::Ui, pal: &Palette) -> Option<PathBuf> {
		let mut go = None;
		ui.horizontal(|ui| {
			let root_name = self
				.root
				.file_name()
				.map(|name| name.to_string_lossy().into_owned())
				.unwrap_or_else(|| self.root.display().to_string());
			if ui
				.add(egui::Button::new(theme::mono(root_name).color(pal.text)).frame_when_inactive(false))
				.clicked()
			{
				go = Some(PathBuf::new());
			}
			let mut prefix = PathBuf::new();
			for part in self.dir.iter() {
				prefix.push(part);
				ui.label(theme::mono("/").color(pal.muted()));
				let name = part.to_string_lossy().into_owned();
				if ui
					.add(egui::Button::new(theme::mono(name).color(pal.text_dim)).frame_when_inactive(false))
					.clicked()
				{
					go = Some(prefix.clone());
				}
			}
		});
		go
	}

	pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette) -> FolderAction {
		let mut action = FolderAction::None;
		if let Some(error) = &self.error {
			ui.label(RichText::new(error).size(12.5).color(pal.error));
			return action;
		}
		let keyed = self.keys(ui);
		if !matches!(keyed, FolderAction::None) {
			return keyed;
		}
		let shown = self.shown();
		let mut pick = None;
		let scroll_to_cursor = std::mem::take(&mut self.scroll_to_cursor);
		egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
			ui.spacing_mut().item_spacing.y = 0.0;
			if !self.dir.as_os_str().is_empty() {
				let height = theme::density(ui.ctx()).row_height();
				let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
				if response.hovered() {
					ui.painter().rect_filled(rect, egui::CornerRadius::same(4), pal.surface_hi);
					ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
				}
				let icon_rect = egui::Rect::from_min_size(rect.left_center() + Vec2::new(8.0, -7.0), Vec2::splat(14.0));
				icons::file_icon(&mut ui.new_child(egui::UiBuilder::new().max_rect(icon_rect)), icons::Kind::Folder, pal.text_dim);
				ui.painter().text(
					rect.left_center() + Vec2::new(30.0, 0.0),
					egui::Align2::LEFT_CENTER,
					"..",
					egui::FontId::new(13.5, egui::FontFamily::Proportional),
					pal.text_dim,
				);
				if response.on_hover_text("Up one folder (Backspace)").clicked() {
					action = FolderAction::Enter(self.dir.parent().map(PathBuf::from).unwrap_or_default());
				}
			}
			for index in &shown {
				let row = &self.rows[*index];
				let height = theme::density(ui.ctx()).row_height();
				let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
				let at_cursor = self.cursor.as_ref() == Some(&row.path);
				if at_cursor && scroll_to_cursor {
					response.scroll_to_me(None);
				}
				let selected = self.selection.contains(&row.path) || (at_cursor && row.is_dir);
				if selected {
					ui.painter()
						.rect_filled(rect, egui::CornerRadius::same(4), pal.selection);
				} else if response.hovered() {
					ui.painter()
						.rect_filled(rect, egui::CornerRadius::same(4), pal.surface_hi);
				}
				if response.hovered() {
					ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
				}
				let kind = if row.is_dir { icons::Kind::Folder } else { icons::Kind::Archive };
				let icon_color = if row.is_dir { pal.text_dim } else { pal.accent };
				let icon_rect = egui::Rect::from_min_size(rect.left_center() + Vec2::new(8.0, -7.0), Vec2::splat(14.0));
				icons::file_icon(&mut ui.new_child(egui::UiBuilder::new().max_rect(icon_rect)), kind, icon_color);
				ui.painter().text(
					rect.left_center() + Vec2::new(30.0, 0.0),
					egui::Align2::LEFT_CENTER,
					&row.name,
					egui::FontId::new(13.5, egui::FontFamily::Proportional),
					pal.text,
				);
				if !row.is_dir {
					ui.painter().text(
						rect.right_center() - Vec2::new(8.0, 0.0),
						egui::Align2::RIGHT_CENTER,
						human_bytes(row.size),
						egui::FontId::new(11.0, egui::FontFamily::Monospace),
						pal.muted(),
					);
				}
				if response.clicked() {
					let (ctrl, shift) = ui.ctx().input(|i| (i.modifiers.command, i.modifiers.shift));
					if row.is_dir {
						if !ctrl && !shift {
							action = FolderAction::Enter(row.path.clone());
						}
					} else {
						pick = Some(FolderAction::Pick {
							path: row.path.clone(),
							ctrl,
							shift,
						});
					}
				}
				if response.double_clicked() {
					// A double click also reports a click; the open wins over the selection.
					pick = None;
					action = if row.is_dir {
						FolderAction::Enter(row.path.clone())
					} else {
						FolderAction::Open(row.path.clone())
					};
				}
				if !row.is_dir {
					response.on_hover_text("Double-click to look inside. Click, Ctrl+click or Shift+click to pick archives to extract.");
				}
			}
		});
		if let Some(pick) = pick {
			action = pick;
		}
		action
	}
}

fn is_pak(path: &Path) -> bool {
	path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pak"))
}

/// Whether `dir` or a folder below it holds an archive; stops at the first one, so a client answers at once.
fn any_pak(dir: &Path) -> bool {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return false;
	};
	let mut subdirs = Vec::new();
	for entry in entries.filter_map(Result::ok) {
		let Ok(kind) = entry.file_type() else { continue };
		if kind.is_file() && is_pak(&entry.path()) {
			return true;
		}
		if kind.is_dir() {
			subdirs.push(entry.path());
		}
	}
	subdirs.iter().any(|sub| any_pak(sub))
}

/// Subfolder names of `dir` in lower case; nothing when it cannot be read.
fn subdirs(dir: &Path) -> Vec<(String, PathBuf)> {
	let Ok(entries) = std::fs::read_dir(dir) else {
		return Vec::new();
	};
	entries
		.filter_map(Result::ok)
		.filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
		.map(|entry| (entry.file_name().to_string_lossy().to_lowercase(), entry.path()))
		.collect()
}

/// A client folder: `Data` next to `bin32`, `bin64` or `Objects`.
fn is_client(dir: &Path) -> bool {
	let names: Vec<String> = subdirs(dir).into_iter().map(|(name, _)| name).collect();
	let has = |name: &str| names.iter().any(|n| n == name);
	has("data") && (has("bin32") || has("bin64") || has("objects"))
}

/// Whether `dir` holds a `.pak` itself, not further down.
fn holds_paks(dir: &Path) -> bool {
	std::fs::read_dir(dir).is_ok_and(|entries| {
		entries
			.filter_map(Result::ok)
			.any(|entry| entry.file_type().is_ok_and(|t| t.is_file()) && is_pak(&entry.path()))
	})
}

/// The folder an archive opened on its own is shown in, so `..` leads from it to its neighbours: the client it
/// belongs to, or else the highest folder reached by going up while some direct subfolder of the next one up holds
/// archives. A lone archive in a folder of its own ends up one level above that folder.
pub fn tree_root(archive: &Path) -> PathBuf {
	let Some(start) = archive.parent() else {
		return archive.to_path_buf();
	};
	if let Some(client) = start.ancestors().find(|dir| is_client(dir)) {
		return client.to_path_buf();
	}
	let mut root = start;
	while let Some(parent) = root.parent() {
		if !subdirs(parent).iter().any(|(_, dir)| holds_paks(dir)) {
			break;
		}
		root = parent;
	}
	root.to_path_buf()
}

#[cfg(test)]
mod tests {
	use super::*;
	use std::fs;

	fn touch(path: &Path) {
		fs::create_dir_all(path.parent().unwrap()).unwrap();
		fs::write(path, b"").unwrap();
	}

	#[test]
	fn tree_root_stops_at_the_client() {
		let tmp = std::env::temp_dir().join(format!("aionpak-root-{}", std::process::id()));
		let client = tmp.join("Games/Client");
		fs::create_dir_all(client.join("bin64")).unwrap();
		touch(&client.join("Data/data.pak"));
		let pak = client.join("L10N/rus/Data/Data.pak");
		touch(&pak);
		// A sibling with archives above the client must not pull the root past it.
		touch(&tmp.join("Games/other.pak"));
		assert_eq!(tree_root(&pak), client);

		let _ = fs::remove_dir_all(&tmp);

		// No client: up while a direct subfolder of the next level holds archives.
		let tmp = std::env::temp_dir().join(format!("aionpak-root2-{}", std::process::id()));
		let mods = tmp.join("x/mods");
		let pak = mods.join("Data/Dialogs/Dialogs.pak");
		touch(&pak);
		touch(&mods.join("Data/data.pak"));
		assert_eq!(tree_root(&pak), mods);
		let _ = fs::remove_dir_all(&tmp);
	}
}
