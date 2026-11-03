//! The result of a comparison: the archives on the left with what happened to each, the entries of the picked one on
//! the right, filtered by kind of change. A difference is not an error, so nothing here is drawn red but a side that
//! could not be read.

use crate::theme::{self, Palette};
use crate::units::human_bytes;
use aionpak_commons::compare::{Change, PakDiff, PakStatus, Report, Side};
use egui::{Align, Color32, CornerRadius, Label, Layout, Margin, Rect, RichText, Sense, Stroke, StrokeKind, Vec2};
use std::sync::{Arc, Mutex};

/// Where the job leaves its report; the view takes it from there when the job ends.
pub type Slot = Arc<Mutex<Option<Report>>>;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Show {
	All,
	Changed,
	OnlyFirst,
	OnlySecond,
}

impl Show {
	fn keeps(self, change: Change) -> bool {
		match self {
			Show::All => true,
			Show::Changed => change == Change::Changed,
			Show::OnlyFirst => change == Change::OnlyFirst,
			Show::OnlySecond => change == Change::OnlySecond,
		}
	}
}

pub struct CompareView {
	pub slot: Slot,
	report: Option<Report>,
	/// Index into `report.paks`.
	picked: Option<usize>,
	show: Show,
	show_same: bool,
	filter: String,
	/// The filtered entry rows of the picked archive, rebuilt when the pick or a filter changes.
	rows: Vec<usize>,
	rows_for: Option<(usize, Show, String)>,
}

impl Default for CompareView {
	fn default() -> Self {
		CompareView {
			slot: Arc::new(Mutex::new(None)),
			report: None,
			picked: None,
			show: Show::All,
			show_same: false,
			filter: String::new(),
			rows: Vec::new(),
			rows_for: None,
		}
	}
}

fn status_color(pal: &Palette, pak: &PakDiff) -> Color32 {
	match pak.status {
		PakStatus::Same => pal.success,
		PakStatus::Differs => pal.warning,
		PakStatus::OnlyFirst | PakStatus::OnlySecond => pal.accent,
		PakStatus::Unreadable(_) => pal.error,
	}
}

fn change_color(pal: &Palette, change: Change) -> Color32 {
	match change {
		Change::Changed => pal.warning,
		Change::OnlyFirst => pal.error.lerp_to_gamma(pal.warning, 0.35),
		Change::OnlySecond => pal.success,
	}
}

fn change_mark(change: Change) -> &'static str {
	match change {
		Change::Changed => "~",
		Change::OnlyFirst => "−",
		Change::OnlySecond => "+",
	}
}

/// What happened to an archive, in a few words.
fn pak_summary(pak: &PakDiff) -> String {
	match &pak.status {
		PakStatus::Same => format!("same, {} entries", pak.second_entries),
		PakStatus::OnlyFirst => format!("only in the first, {} entries", pak.first_entries),
		PakStatus::OnlySecond => format!("only in the second, {} entries", pak.second_entries),
		PakStatus::Unreadable(why) => why.clone(),
		PakStatus::Differs => {
			let mut parts = Vec::new();
			let (changed, first, second) = (
				pak.count(Change::Changed),
				pak.count(Change::OnlyFirst),
				pak.count(Change::OnlySecond),
			);
			if second > 0 {
				parts.push(format!("+{second}"));
			}
			if first > 0 {
				parts.push(format!("−{first}"));
			}
			if changed > 0 {
				parts.push(format!("~{changed}"));
			}
			if pak.first_empty {
				parts.push("first is empty".into());
			}
			if pak.second_empty {
				parts.push("second is empty".into());
			}
			if pak.reordered {
				parts.push("order".into());
			}
			parts.join("  ")
		}
	}
}

fn side_text(side: Option<Side>) -> String {
	side.map_or_else(|| "—".to_owned(), |s| human_bytes(s.size as u64))
}

impl CompareView {
	/// Takes the report the finished job left. Keeps the archive picked before if it is still there.
	pub fn take_result(&mut self) {
		let Some(report) = self.slot.lock().unwrap().take() else {
			return;
		};
		let previous = self
			.picked
			.and_then(|i| self.report.as_ref()?.paks.get(i).map(|p| p.path.clone()));
		self.picked = previous
			.and_then(|path| {
				report
					.paks
					.iter()
					.position(|p| p.path == path && p.status != PakStatus::Same)
			})
			.or_else(|| report.paks.iter().position(|p| p.status != PakStatus::Same));
		self.report = Some(report);
		self.rows_for = None;
	}

	pub fn clear(&mut self) {
		self.report = None;
		self.picked = None;
		self.rows_for = None;
	}

	/// Nothing before the first run, nor while one runs: the status bar shows its progress.
	pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette) {
		let Some(report) = &self.report else {
			return;
		};
		if report.paks.is_empty() {
			ui.label(
				RichText::new("No archives on either side.")
					.size(13.0)
					.color(pal.muted()),
			);
			return;
		}
		summary(ui, pal, report);
		ui.add_space(8.0);
		if report.is_same() {
			ui.label(
				RichText::new("Both sides hold the same files.")
					.size(13.0)
					.color(pal.success),
			);
			return;
		}

		let full = ui.available_rect_before_wrap();
		let single = report.paks.len() == 1;
		let wide = full.width() >= 720.0;
		let list_width = if single {
			0.0
		} else if wide {
			(full.width() * 0.34).clamp(240.0, 380.0)
		} else {
			full.width()
		};
		let (list_rect, entries_rect) = if single {
			(Rect::NOTHING, full)
		} else if wide {
			(
				Rect::from_min_max(full.min, egui::pos2(full.min.x + list_width, full.max.y)),
				Rect::from_min_max(egui::pos2(full.min.x + list_width + 10.0, full.min.y), full.max),
			)
		} else {
			let split = full.min.y + (full.height() * 0.4).max(120.0);
			(
				Rect::from_min_max(full.min, egui::pos2(full.max.x, split)),
				Rect::from_min_max(egui::pos2(full.min.x, split + 10.0), full.max),
			)
		};
		if !single {
			self.pak_list(ui, pal, list_rect);
		} else {
			self.picked = Some(0);
		}
		self.entry_table(ui, pal, entries_rect);
		ui.advance_cursor_after_rect(full);
	}

	fn pak_list(&mut self, ui: &mut egui::Ui, pal: &Palette, rect: Rect) {
		let Some(report) = &self.report else {
			return;
		};
		let same = report.paks.iter().filter(|p| p.status == PakStatus::Same).count();
		let shown: Vec<usize> = (0..report.paks.len())
			.filter(|&i| self.show_same || report.paks[i].status != PakStatus::Same)
			.collect();
		let mut picked = self.picked;
		let mut show_same = self.show_same;
		panel(ui, pal, rect, 6.0, |ui| {
			ui.horizontal(|ui| {
				ui.add_space(6.0);
				ui.label(theme::caption(pal, "ARCHIVES"));
				if same > 0 {
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						ui.checkbox(&mut show_same, RichText::new(format!("{same} same")).size(12.0))
							.on_hover_text("Also list the archives that hold the same files on both sides");
					});
				}
			});
			ui.add_space(4.0);
			let row_height = 40.0;
			egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(
				ui,
				row_height,
				shown.len(),
				|ui, range| {
					for &i in &shown[range] {
						let pak = &report.paks[i];
						let (row, response) =
							ui.allocate_exact_size(Vec2::new(ui.available_width(), row_height), Sense::click());
						let selected = picked == Some(i);
						if selected {
							ui.painter().rect_filled(row, CornerRadius::same(6), pal.selection);
						} else if response.hovered() {
							ui.painter().rect_filled(row, CornerRadius::same(6), pal.surface_hi);
						}
						let color = status_color(pal, pak);
						ui.painter()
							.circle_filled(egui::pos2(row.left() + 12.0, row.top() + 14.0), 4.0, color);
						let text_left = row.left() + 24.0;
						let width = row.width() - 30.0;
						let painter = ui.painter().with_clip_rect(row.shrink(2.0));
						let name = ui.fonts_mut(|f| {
							f.layout(
								pak.path.clone(),
								egui::FontId::proportional(13.0),
								pal.text,
								f32::INFINITY,
							)
						});
						// A long path keeps its end, the archive's name.
						let shift = (name.size().x - width).max(0.0);
						painter.galley(egui::pos2(text_left - shift, row.top() + 5.0), name, pal.text);
						painter.text(
							egui::pos2(text_left, row.top() + 23.0),
							egui::Align2::LEFT_TOP,
							pak_summary(pak),
							egui::FontId::monospace(11.5),
							if pak.status == PakStatus::Same {
								pal.muted()
							} else {
								color
							},
						);
						let response = response.on_hover_text(&pak.path);
						if response.clicked() {
							picked = Some(i);
						}
					}
				},
			);
		});
		if picked != self.picked {
			self.picked = picked;
			self.rows_for = None;
		}
		self.show_same = show_same;
	}

	fn entry_table(&mut self, ui: &mut egui::Ui, pal: &Palette, rect: Rect) {
		let CompareView {
			report,
			picked,
			show,
			filter,
			rows,
			rows_for,
			..
		} = self;
		panel(ui, pal, rect, 10.0, |ui| {
			let Some(pak) = picked.and_then(|i| report.as_ref()?.paks.get(i)) else {
				ui.label(theme::note(pal, "Pick an archive on the left."));
				return;
			};
			ui.horizontal(|ui| {
				ui.label(theme::strong(pak.path.as_str()).color(pal.text));
				ui.label(
					theme::mono(format!("{} → {} entries", pak.first_entries, pak.second_entries)).color(pal.muted()),
				);
			});
			match &pak.status {
				PakStatus::Same => {
					ui.label(
						RichText::new("The same files on both sides.")
							.size(13.0)
							.color(pal.success),
					);
					return;
				}
				PakStatus::OnlyFirst | PakStatus::OnlySecond => {
					let side = if pak.status == PakStatus::OnlyFirst {
						"first"
					} else {
						"second"
					};
					ui.label(theme::note(pal, format!("This archive is only on the {side} side.")));
					return;
				}
				PakStatus::Unreadable(why) => {
					ui.label(RichText::new(why).size(13.0).color(pal.error));
					return;
				}
				PakStatus::Differs => {}
			}
			for (empty, side) in [(pak.first_empty, "first"), (pak.second_empty, "second")] {
				if empty {
					ui.label(theme::note(
						pal,
						format!(
							"The {side} side is an empty placeholder, not an archive; the client reads nothing from it."
						),
					));
				}
			}
			if pak.reordered {
				ui.label(theme::note(pal, "The shared entries are stored in another order."))
					.on_hover_text(
						"The client finds them all the same; order only decides which copy of a name stored twice wins.",
					);
			}
			ui.add_space(4.0);
			ui.horizontal_wrapped(|ui| {
				let options = [
					(Show::All, format!("All {}", pak.entries.len())),
					(Show::Changed, format!("Changed {}", pak.count(Change::Changed))),
					(Show::OnlySecond, format!("Added {}", pak.count(Change::OnlySecond))),
					(Show::OnlyFirst, format!("Removed {}", pak.count(Change::OnlyFirst))),
				];
				let options: Vec<(Show, &str)> = options.iter().map(|(s, t)| (*s, t.as_str())).collect();
				theme::segmented(ui, show, &options);
				ui.label(RichText::new("ⓘ").size(13.0).color(pal.muted()))
					.on_hover_text(
						"Added: only in the second.\nRemoved: only in the first.\nChanged: the same name with another size or CRC.",
					);
				ui.add_space(8.0);
				ui.add(
					egui::TextEdit::singleline(filter)
						.hint_text("Filter names")
						.desired_width(180.0)
						.margin(Margin::symmetric(10, 5)),
				);
			});
			ui.add_space(6.0);
			let key = (picked.unwrap_or(0), *show, filter.to_ascii_lowercase());
			if rows_for.as_ref() != Some(&key) {
				*rows = (0..pak.entries.len())
					.filter(|&i| {
						let e = &pak.entries[i];
						key.1.keeps(e.change) && (key.2.is_empty() || e.name.to_ascii_lowercase().contains(&key.2))
					})
					.collect();
				*rows_for = Some(key);
			}
			entries(ui, pal, pak, rows);
		});
	}
}

/// A card at a fixed place: drawn first and outlined last, so content wider or taller than the place is cut off
/// inside it instead of pushing the card past its edges.
fn panel(ui: &mut egui::Ui, pal: &Palette, rect: Rect, margin: f32, add: impl FnOnce(&mut egui::Ui)) {
	let radius = CornerRadius::same(theme::CARD_RADIUS);
	ui.painter().rect_filled(rect, radius, pal.surface);
	let inner = rect.shrink(margin);
	ui.scope_builder(egui::UiBuilder::new().max_rect(inner), |ui| {
		ui.set_clip_rect(rect.shrink(1.0).intersect(ui.clip_rect()));
		add(ui);
	});
	ui.painter()
		.rect_stroke(rect, radius, Stroke::new(1.0, pal.border), StrokeKind::Inside);
}

fn summary(ui: &mut egui::Ui, pal: &Palette, report: &Report) {
	let count = |f: &dyn Fn(&PakDiff) -> bool| report.paks.iter().filter(|p| f(p)).count();
	let differ = count(&|p| p.status == PakStatus::Differs);
	let same = count(&|p| p.status == PakStatus::Same);
	let first = count(&|p| p.status == PakStatus::OnlyFirst);
	let second = count(&|p| p.status == PakStatus::OnlySecond);
	let broken = count(&|p| matches!(p.status, PakStatus::Unreadable(_)));
	let entries = |change| report.paks.iter().map(|p| p.count(change)).sum::<usize>();
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing.x = 6.0;
		let chip = |ui: &mut egui::Ui, text: String, color: Color32| {
			egui::Frame::new()
				.fill(color.gamma_multiply(0.14))
				.corner_radius(CornerRadius::same(10))
				.inner_margin(Margin::symmetric(9, 3))
				.show(ui, |ui| ui.add(Label::new(RichText::new(text).size(12.0).color(color))));
		};
		if differ > 0 {
			chip(ui, format!("{differ} differ"), pal.warning);
		}
		if second > 0 {
			chip(ui, format!("{second} only in the second"), pal.accent);
		}
		if first > 0 {
			chip(ui, format!("{first} only in the first"), pal.accent);
		}
		if broken > 0 {
			chip(ui, format!("{broken} unreadable"), pal.error);
		}
		if same > 0 {
			chip(ui, format!("{same} same"), pal.success);
		}
		ui.add_space(10.0);
		let files = format!(
			"files: +{} added  −{} removed  ~{} changed",
			entries(Change::OnlySecond),
			entries(Change::OnlyFirst),
			entries(Change::Changed)
		);
		ui.label(theme::mono(files).color(pal.text_dim));
	});
}

fn entries(ui: &mut egui::Ui, pal: &Palette, pak: &PakDiff, rows: &[usize]) {
	if rows.is_empty() {
		ui.label(theme::note(pal, "Nothing of this kind."));
		return;
	}
	let row_height = 22.0;
	let size_width = 90.0;
	// Painted at the same offsets as the rows, so the titles sit over their columns.
	let (head, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 18.0), Sense::hover());
	let font = egui::FontId::proportional(11.0);
	let caption = pal.muted();
	let name_right = head.right() - 2.0 * size_width - 10.0;
	let painter = ui.painter();
	painter.text(
		egui::pos2(head.left() + 22.0, head.center().y),
		egui::Align2::LEFT_CENTER,
		"NAME",
		font.clone(),
		caption,
	);
	painter.text(
		egui::pos2(name_right + size_width, head.center().y),
		egui::Align2::RIGHT_CENTER,
		"FIRST",
		font.clone(),
		caption,
	);
	painter.text(
		egui::pos2(name_right + 2.0 * size_width, head.center().y),
		egui::Align2::RIGHT_CENTER,
		"SECOND",
		font,
		caption,
	);
	egui::ScrollArea::vertical()
		.auto_shrink([false, false])
		.show_rows(ui, row_height, rows.len(), |ui, range| {
			for (n, &i) in rows[range.clone()].iter().enumerate() {
				let e = &pak.entries[i];
				let (row, response) =
					ui.allocate_exact_size(Vec2::new(ui.available_width(), row_height), Sense::hover());
				if (range.start + n) % 2 == 1 {
					ui.painter().rect_filled(row, 0.0, pal.surface_hi.gamma_multiply(0.6));
				}
				let color = change_color(pal, e.change);
				let painter = ui.painter().with_clip_rect(row);
				let mid = row.center().y;
				painter.text(
					egui::pos2(row.left() + 8.0, mid),
					egui::Align2::LEFT_CENTER,
					change_mark(e.change),
					egui::FontId::monospace(13.0),
					color,
				);
				let name_right = row.right() - 2.0 * size_width - 10.0;
				painter
					.with_clip_rect(Rect::from_min_max(row.min, egui::pos2(name_right - 6.0, row.max.y)))
					.text(
						egui::pos2(row.left() + 22.0, mid),
						egui::Align2::LEFT_CENTER,
						&e.name,
						egui::FontId::monospace(12.0),
						pal.text,
					);
				let sizes_differ = matches!((e.first, e.second), (Some(a), Some(b)) if a.size != b.size);
				for (k, side) in [e.first, e.second].into_iter().enumerate() {
					let right = name_right + size_width * (k as f32 + 1.0);
					let text_color = if side.is_none() {
						pal.muted()
					} else if k == 1 && sizes_differ {
						pal.text
					} else {
						pal.text_dim
					};
					painter.text(
						egui::pos2(right, mid),
						egui::Align2::RIGHT_CENTER,
						side_text(side),
						egui::FontId::monospace(12.0),
						text_color,
					);
				}
				let crc = |s: Option<Side>| {
					s.map_or_else(|| "—".to_owned(), |s| format!("{} bytes, CRC {:08x}", s.size, s.crc))
				};
				response.on_hover_text(format!(
					"{}\nfirst:  {}\nsecond: {}",
					e.name,
					crc(e.first),
					crc(e.second)
				));
			}
		});
}
