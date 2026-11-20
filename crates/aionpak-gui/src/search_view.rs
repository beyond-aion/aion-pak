//! The hits of a search: archive, then file, then each matching line with the match marked. Rows come in while
//! the search runs; a click on a file or a line opens that file in Browse.

use crate::theme::{self, Palette};
use aionpak_commons::search::Hit;
use egui::text::{LayoutJob, TextFormat};
use egui::{FontId, Margin, Sense, Vec2};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The search stops after this many lines: more would not be read, and each takes memory.
pub const MAX_LINES: usize = 50_000;
const ROW_HEIGHT: f32 = 20.0;
/// Width of the line number column.
const NUMBER_WIDTH: f32 = 56.0;

/// The hits of one archive, in the order of its entries.
pub struct Group {
	pub archive: PathBuf,
	/// The archive as shown: its path under the folder searched, or its file name.
	pub label: String,
	pub hits: Vec<Hit>,
}

/// What the job has found so far. The view moves the groups out each frame and keeps the counts.
#[derive(Default)]
pub struct Found {
	pub groups: Vec<Group>,
	pub lines: usize,
	pub files: usize,
	pub capped: bool,
}

pub type Slot = Arc<Mutex<Found>>;

#[derive(Clone, Copy)]
enum Row {
	Archive(usize),
	/// A file: its group, the index of its first hit, and how many lines it has.
	Entry(usize, usize, usize),
	Hit(usize, usize),
}

/// A file to show in Browse: the archive, the entry's name, and the line clicked, if a line was.
pub struct Open {
	pub archive: PathBuf,
	pub entry: String,
	pub line: Option<usize>,
}

#[derive(Default)]
pub struct SearchView {
	pub slot: Slot,
	groups: Vec<Group>,
	rows: Vec<Row>,
	lines: usize,
	files: usize,
	capped: bool,
	/// The row clicked last, kept shaded so it is easy to come back to after a look in Browse.
	picked: Option<usize>,
}

impl SearchView {
	/// Forgets the last results; the next search starts with a fresh slot.
	pub fn clear(&mut self) {
		*self = SearchView::default();
	}

	/// Takes what the job found since the last frame.
	fn take(&mut self) {
		let mut found = self.slot.lock().unwrap();
		self.lines = found.lines;
		self.files = found.files;
		self.capped = found.capped;
		for group in found.groups.drain(..) {
			let index = self.groups.len();
			self.rows.push(Row::Archive(index));
			for (i, hit) in group.hits.iter().enumerate() {
				if i == 0 || group.hits[i - 1].entry != hit.entry {
					let count = group.hits[i..].iter().take_while(|h| h.entry == hit.entry).count();
					self.rows.push(Row::Entry(index, i, count));
				}
				self.rows.push(Row::Hit(index, i));
			}
			self.groups.push(group);
		}
	}

	/// The results; returns the file to open when a row is clicked.
	pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette, running: bool) -> Option<Open> {
		self.take();
		if self.rows.is_empty() {
			return None;
		}
		ui.horizontal(|ui| {
			ui.label(theme::strong(format!("{} lines", self.lines)).color(pal.text));
			ui.label(theme::mono(format!("in {} files of {} archives", self.files, self.groups.len())).color(pal.text_dim));
			if self.capped {
				ui.label(theme::mono(format!("· stopped at {MAX_LINES}; narrow the search")).color(pal.warning));
			} else if running {
				ui.label(theme::mono("· searching…").color(pal.muted()));
			}
		});
		ui.add_space(4.0);
		let mut open = None;
		theme::card(pal).inner_margin(Margin::same(6)).show(ui, |ui| {
			ui.set_width(ui.available_width());
			egui::ScrollArea::vertical()
				.auto_shrink([false, true])
				.max_height(ui.available_height().max(240.0))
				.show_rows(ui, ROW_HEIGHT, self.rows.len(), |ui, range| {
					ui.spacing_mut().item_spacing.y = 0.0;
					for index in range {
						if let Some(clicked) = self.row(ui, pal, index) {
							self.picked = Some(index);
							open = Some(clicked);
						}
					}
				});
		});
		open
	}

	fn row(&self, ui: &mut egui::Ui, pal: &Palette, index: usize) -> Option<Open> {
		let row = self.rows[index];
		let width = ui.available_width();
		let sense = if matches!(row, Row::Archive(_)) { Sense::hover() } else { Sense::click() };
		let (rect, response) = ui.allocate_exact_size(Vec2::new(width, ROW_HEIGHT), sense);
		if !ui.is_rect_visible(rect) {
			return None;
		}
		let painter = ui.painter_at(rect);
		if self.picked == Some(index) {
			painter.rect_filled(rect, 4.0, pal.selection);
		} else if response.hovered() && !matches!(row, Row::Archive(_)) {
			painter.rect_filled(rect, 4.0, pal.surface_hi);
			ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
		}
		let mono = FontId::monospace(12.0);
		let middle = rect.center().y;
		match row {
			Row::Archive(group) => {
				let galley = painter.layout_no_wrap(
					self.groups[group].label.clone(),
					FontId::new(12.5, egui::FontFamily::Name("semibold".into())),
					pal.text,
				);
				painter.galley(egui::pos2(rect.left() + 4.0, middle - galley.size().y / 2.0), galley, pal.text);
				None
			}
			Row::Entry(group, first, count) => {
				let hit = &self.groups[group].hits[first];
				let text = format!("{}  ({count})", hit.entry);
				let galley = painter.layout_no_wrap(text, mono, pal.accent);
				painter.galley(egui::pos2(rect.left() + 16.0, middle - galley.size().y / 2.0), galley, pal.accent);
				response.clicked().then(|| Open {
					archive: self.groups[group].archive.clone(),
					entry: hit.entry.clone(),
					line: None,
				})
			}
			Row::Hit(group, i) => {
				let hit = &self.groups[group].hits[i];
				let number = painter.layout_no_wrap(hit.line.to_string(), mono.clone(), pal.muted());
				let right = rect.left() + 16.0 + NUMBER_WIDTH;
				painter.galley(egui::pos2(right - number.size().x, middle - number.size().y / 2.0), number, pal.muted());
				let digit = painter.layout_no_wrap("0".to_owned(), mono.clone(), pal.text).size().x.max(1.0);
				let width = rect.right() - right - 16.0;
				let galley = painter.layout_job(highlighted(hit, pal, mono, width, (width / digit) as usize));
				painter.galley(egui::pos2(right + 12.0, middle - galley.size().y / 2.0), galley, pal.text);
				response.on_hover_text(format!("{}, line {}", hit.entry, hit.line)).clicked().then(|| Open {
					archive: self.groups[group].archive.clone(),
					entry: hit.entry.clone(),
					line: Some(hit.line),
				})
			}
		}
	}
}

/// The line with the match on the accent. When the text before the match would take more than a third of the
/// `columns` that fit, its start gives way to an ellipsis, so the match is always in view.
fn highlighted(hit: &Hit, pal: &Palette, font: FontId, width: f32, columns: usize) -> LayoutJob {
	let mut job = LayoutJob::default();
	let plain = TextFormat::simple(font.clone(), pal.text_dim);
	let marked = TextFormat {
		background: pal.accent.gamma_multiply(0.28),
		color: pal.text,
		..TextFormat::simple(font, pal.text)
	};
	let at = hit.at.clone();
	let safe = |i: usize| hit.text.is_char_boundary(i);
	if at.end <= hit.text.len() && safe(at.start) && safe(at.end) {
		let before = &hit.text[..at.start];
		let keep = (columns / 3).max(8);
		let count = before.chars().count();
		if count > keep {
			let cut = before.char_indices().nth(count - keep).map_or(0, |(i, _)| i);
			job.append("…", 0.0, plain.clone());
			job.append(&before[cut..], 0.0, plain.clone());
		} else {
			job.append(before, 0.0, plain.clone());
		}
		job.append(&hit.text[at.clone()], 0.0, marked);
		job.append(&hit.text[at.end..], 0.0, plain);
	} else {
		job.append(&hit.text, 0.0, plain);
	}
	job.wrap.max_width = width.max(40.0);
	job.wrap.max_rows = 1;
	job.wrap.break_anywhere = true;
	job
}
