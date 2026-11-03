//! Small line icons painted from primitives, so there is no icon font or bitmap to keep in sync: one per section
//! of the rail and one per kind of file in the archive table.

use egui::{Color32, Painter, Pos2, Rect, Stroke, StrokeKind, Vec2, pos2};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Glyph {
	Browse,
	Pack,
	Convert,
	Verify,
	Compare,
	Optimize,
	Help,
	Settings,
	About,
}

/// Points of a unit-square path, mapped into `rect`.
fn at(rect: Rect, x: f32, y: f32) -> Pos2 {
	pos2(rect.left() + x * rect.width(), rect.top() + y * rect.height())
}

fn path(painter: &Painter, rect: Rect, points: &[(f32, f32)], stroke: Stroke) {
	let points: Vec<Pos2> = points.iter().map(|&(x, y)| at(rect, x, y)).collect();
	painter.add(egui::Shape::line(points, stroke));
}

/// Paints a section glyph into a square `rect`, 1.5 px strokes like the rest of the chrome.
pub fn glyph(painter: &Painter, rect: Rect, glyph: Glyph, color: Color32) {
	let stroke = Stroke::new(1.5, color);
	let r = rect.shrink(rect.width() * 0.12);
	match glyph {
		Glyph::Browse => {
			// A folder: tab on the left, body below.
			path(
				painter,
				r,
				&[(0.05, 0.85), (0.05, 0.18), (0.38, 0.18), (0.48, 0.30), (0.95, 0.30), (0.95, 0.85), (0.05, 0.85)],
				stroke,
			);
		}
		Glyph::Pack => {
			// A box with an arrow going in.
			path(painter, r, &[(0.08, 0.45), (0.08, 0.92), (0.92, 0.92), (0.92, 0.45)], stroke);
			path(painter, r, &[(0.5, 0.05), (0.5, 0.62)], stroke);
			path(painter, r, &[(0.3, 0.42), (0.5, 0.62), (0.7, 0.42)], stroke);
		}
		Glyph::Convert => {
			// Two arrows passing each other.
			path(painter, r, &[(0.08, 0.32), (0.88, 0.32)], stroke);
			path(painter, r, &[(0.70, 0.14), (0.88, 0.32), (0.70, 0.50)], stroke);
			path(painter, r, &[(0.92, 0.70), (0.12, 0.70)], stroke);
			path(painter, r, &[(0.30, 0.52), (0.12, 0.70), (0.30, 0.88)], stroke);
		}
		Glyph::Verify => {
			// A shield with a tick.
			path(
				painter,
				r,
				&[(0.5, 0.04), (0.90, 0.18), (0.88, 0.55), (0.5, 0.96), (0.12, 0.55), (0.10, 0.18), (0.5, 0.04)],
				stroke,
			);
			path(painter, r, &[(0.32, 0.50), (0.46, 0.64), (0.70, 0.38)], stroke);
		}
		Glyph::Compare => {
			// Two pages side by side.
			painter.rect_stroke(
				Rect::from_min_max(at(r, 0.04, 0.10), at(r, 0.44, 0.90)),
				2.0,
				stroke,
				StrokeKind::Middle,
			);
			painter.rect_stroke(
				Rect::from_min_max(at(r, 0.56, 0.10), at(r, 0.96, 0.90)),
				2.0,
				stroke,
				StrokeKind::Middle,
			);
			path(painter, r, &[(0.14, 0.35), (0.34, 0.35)], stroke);
			path(painter, r, &[(0.66, 0.35), (0.86, 0.35)], stroke);
			path(painter, r, &[(0.14, 0.55), (0.30, 0.55)], stroke);
			path(painter, r, &[(0.66, 0.55), (0.82, 0.55)], stroke);
		}
		Glyph::Optimize => {
			// A lightning bolt.
			path(
				painter,
				r,
				&[(0.58, 0.04), (0.18, 0.56), (0.48, 0.56), (0.40, 0.96), (0.82, 0.42), (0.52, 0.42), (0.58, 0.04)],
				stroke,
			);
		}
		Glyph::Help => {
			let center = r.center();
			painter.circle_stroke(center, r.width() * 0.46, stroke);
			path(
				painter,
				r,
				&[(0.36, 0.38), (0.40, 0.27), (0.5, 0.23), (0.61, 0.27), (0.64, 0.37), (0.58, 0.47), (0.5, 0.53), (0.5, 0.62)],
				stroke,
			);
			painter.circle_filled(at(r, 0.5, 0.76), 1.3, color);
		}
		Glyph::Settings => {
			// Three sliders.
			for (y, knob) in [(0.2, 0.30), (0.5, 0.68), (0.8, 0.42)] {
				path(painter, r, &[(0.04, y), (0.96, y)], stroke);
				painter.circle_filled(at(r, knob, y), r.width() * 0.11, color);
			}
		}
		Glyph::About => {
			let center = r.center();
			painter.circle_stroke(center, r.width() * 0.46, stroke);
			path(painter, r, &[(0.5, 0.45), (0.5, 0.75)], stroke);
			painter.circle_filled(at(r, 0.5, 0.29), 1.3, color);
		}
	}
}

#[derive(Clone, Copy)]
pub enum Kind {
	Folder,
	Xml,
	Image,
	Mesh,
	Sound,
	Text,
	Archive,
	Other,
}

pub fn kind_of(name: &str, is_dir: bool) -> Kind {
	if is_dir {
		return Kind::Folder;
	}
	let ext = name
		.rsplit_once('.')
		.map(|(_, ext)| ext.to_ascii_lowercase())
		.unwrap_or_default();
	match ext.as_str() {
		"xml" | "html" | "htm" | "xsd" | "xsl" | "dtd" => Kind::Xml,
		"dds" | "tga" | "png" | "jpg" | "jpeg" | "bmp" | "gif" | "tif" | "tiff" | "ico" => Kind::Image,
		"mesh" | "cga" | "cgf" | "skel" | "anm" | "caf" | "chr" | "nif" | "obj" | "fbx" => Kind::Mesh,
		"ogg" | "wav" | "mp3" | "fsb" | "mid" => Kind::Sound,
		"txt" | "ini" | "json" | "lua" | "dat" | "cfg" | "csv" | "log" | "properties" => Kind::Text,
		"pak" | "zip" | "7z" | "gz" => Kind::Archive,
		_ => Kind::Other,
	}
}

/// A 14 px glyph per file kind.
pub fn file_icon(ui: &mut egui::Ui, kind: Kind, color: Color32) {
	let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), egui::Sense::hover());
	let painter = ui.painter();
	let stroke = Stroke::new(1.0, color);
	let bounds = rect.shrink(1.5);
	let page = Rect::from_min_max(
		pos2(bounds.left() + 1.0, bounds.top()),
		pos2(bounds.right() - 1.0, bounds.bottom()),
	);
	let c = page.center();
	let line = |a: Pos2, b: Pos2| {
		painter.line_segment([a, b], stroke);
	};
	match kind {
		Kind::Folder => {
			painter.rect_filled(
				Rect::from_min_max(pos2(page.left(), page.top() + 2.0), pos2(page.left() + 5.0, page.top() + 4.0)),
				1.0,
				color,
			);
			painter.rect_stroke(
				Rect::from_min_max(pos2(page.left(), page.top() + 3.5), page.right_bottom()),
				1.0,
				stroke,
				StrokeKind::Inside,
			);
		}
		Kind::Xml => {
			painter.rect_stroke(page, 1.0, stroke, StrokeKind::Inside);
			line(pos2(c.x - 1.0, c.y - 3.0), pos2(c.x - 3.0, c.y));
			line(pos2(c.x - 3.0, c.y), pos2(c.x - 1.0, c.y + 3.0));
			line(pos2(c.x + 1.0, c.y - 3.0), pos2(c.x + 3.0, c.y));
			line(pos2(c.x + 3.0, c.y), pos2(c.x + 1.0, c.y + 3.0));
		}
		Kind::Image => {
			painter.rect_stroke(page, 1.0, stroke, StrokeKind::Inside);
			painter.circle_filled(pos2(c.x - 2.0, c.y - 2.0), 1.2, color);
			line(pos2(page.left() + 1.0, page.bottom() - 1.5), pos2(c.x, c.y + 1.0));
			line(pos2(c.x, c.y + 1.0), pos2(page.right() - 1.0, page.bottom() - 3.0));
		}
		Kind::Mesh => {
			let top = pos2(c.x, page.top() + 1.0);
			let right = pos2(page.right(), c.y - 1.5);
			let bottom = pos2(c.x, c.y + 2.0);
			let left = pos2(page.left(), c.y - 1.5);
			for (a, b) in [(top, right), (right, bottom), (bottom, left), (left, top)] {
				line(a, b);
			}
			line(left, pos2(page.left(), page.bottom() - 2.5));
			line(right, pos2(page.right(), page.bottom() - 2.5));
			line(bottom, pos2(c.x, page.bottom()));
			line(pos2(page.left(), page.bottom() - 2.5), pos2(c.x, page.bottom()));
			line(pos2(page.right(), page.bottom() - 2.5), pos2(c.x, page.bottom()));
		}
		Kind::Sound => {
			painter.circle_filled(pos2(c.x - 2.5, c.y + 3.0), 2.0, color);
			line(pos2(c.x - 0.5, c.y + 3.0), pos2(c.x - 0.5, c.y - 3.5));
			line(pos2(c.x - 0.5, c.y - 3.5), pos2(c.x + 3.0, c.y - 2.0));
		}
		Kind::Text => {
			painter.rect_stroke(page, 1.0, stroke, StrokeKind::Inside);
			for row in 0..3 {
				let y = page.top() + 3.0 + row as f32 * 3.0;
				line(pos2(page.left() + 2.0, y), pos2(page.right() - 2.0, y));
			}
		}
		Kind::Archive => {
			painter.rect_stroke(page, 1.0, stroke, StrokeKind::Inside);
			for row in 0..3 {
				let y = page.top() + 2.5 + row as f32 * 3.0;
				line(pos2(c.x, y), pos2(c.x, y + 1.5));
			}
		}
		Kind::Other => {
			painter.rect_stroke(page, 1.0, stroke, StrokeKind::Inside);
		}
	}
}
