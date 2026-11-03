//! Models of the client in the preview: the meshes of a .cgf or .cga, drawn shaded and turned with the pointer.
//!
//! The files are CryEngine 1 chunk files under Aion's own signature (`NCAion`), version 0x750: a table of chunks,
//! of which the meshes (0xCCCC0000: vertices with normals, then triangles) and the nodes (0xCCCC000B: which mesh,
//! and where) are all that a picture needs. The layout is the one aion-geobuilder reads for the server's geodata.
//! The renderer is egui's own triangles, sorted back to front: no GPU work of ours, and nothing drawn again until
//! the view changes.

use egui::{Color32, Pos2, Rect, Sense, Shape, Vec2};
use std::sync::Mutex;

const SIGNATURE: &[u8; 8] = b"NCAion\0\0";
const MESH: u32 = 0xCCCC_0000;
const NODE: u32 = 0xCCCC_000B;
/// More triangles than this are thinned for drawing: a turn must stay smooth on a large building.
const DRAWN_TRIANGLES: usize = 150_000;

pub struct Model {
	vertices: Vec<[f32; 3]>,
	triangles: Vec<[u32; 3]>,
	pub meshes: usize,
	/// Centre and radius of the bounding sphere, so any model fills the view.
	centre: [f32; 3],
	radius: f32,
	cache: Mutex<Option<(View, Rect, Vec<Shape>)>>,
}

#[derive(Clone, Copy, PartialEq)]
struct View {
	yaw: f32,
	pitch: f32,
	zoom: f32,
}

const START: View = View {
	yaw: 0.6,
	pitch: 0.35,
	zoom: 1.0,
};

pub fn is_model(name: &str) -> bool {
	let lower = name.to_ascii_lowercase();
	lower.ends_with(".cgf") || lower.ends_with(".cga")
}

struct Reader<'a> {
	data: &'a [u8],
}

impl Reader<'_> {
	fn u32(&self, at: usize) -> Option<u32> {
		Some(u32::from_le_bytes(self.data.get(at..at + 4)?.try_into().ok()?))
	}

	fn f32(&self, at: usize) -> Option<f32> {
		Some(f32::from_le_bytes(self.data.get(at..at + 4)?.try_into().ok()?))
	}
}

struct Chunk {
	kind: u32,
	offset: usize,
	id: u32,
}

pub fn parse(data: &[u8]) -> Result<Model, String> {
	if data.get(..8) != Some(SIGNATURE) {
		return Err("not an Aion model (no NCAion signature)".into());
	}
	let r = Reader { data };
	let bad = || "the model is cut short".to_owned();
	if r.u32(8).ok_or_else(bad)? != 0xFFFF_0000 {
		return Err("an animation, not a model".into());
	}
	let table = r.u32(16).ok_or_else(bad)? as usize;
	let count = r.u32(table).ok_or_else(bad)? as usize;
	if count > 100_000 {
		return Err("damaged chunk table".into());
	}
	let mut chunks = Vec::with_capacity(count);
	for i in 0..count {
		let at = table + 4 + i * 16;
		chunks.push(Chunk {
			kind: r.u32(at).ok_or_else(bad)?,
			offset: r.u32(at + 8).ok_or_else(bad)? as usize,
			id: r.u32(at + 12).ok_or_else(bad)?,
		});
	}
	let by_id = |id: u32| chunks.iter().find(|c| c.id == id).or_else(|| chunks.get(id as usize));

	// Each mesh once, placed by the first node that shows it; meshes no node shows (some characters) unplaced.
	let mut placed: Vec<(usize, Option<[f32; 16]>)> = Vec::new();
	for chunk in chunks.iter().filter(|c| c.kind == NODE) {
		let at = chunk.offset + 16 + 64;
		let object = r.u32(at).ok_or_else(bad)?;
		let Some(mesh) = by_id(object).filter(|c| c.kind == MESH) else {
			continue;
		};
		let index = chunks.iter().position(|c| std::ptr::eq(c, mesh)).unwrap_or(0);
		if placed.iter().any(|(i, _)| *i == index) {
			continue;
		}
		let tm_at = at + 4 * 4 + 4;
		let mut tm = [0f32; 16];
		for (k, value) in tm.iter_mut().enumerate() {
			*value = r.f32(tm_at + k * 4).ok_or_else(bad)?;
		}
		placed.push((index, Some(tm)));
	}
	for (index, chunk) in chunks.iter().enumerate() {
		if chunk.kind == MESH && !placed.iter().any(|(i, _)| *i == index) {
			placed.push((index, None));
		}
	}

	let mut vertices = Vec::new();
	let mut triangles = Vec::new();
	let mut meshes = 0;
	for (index, tm) in placed {
		let at = chunks[index].offset + 20;
		let (Some(vertex_count), Some(face_count)) = (r.u32(at), r.u32(at + 8)) else {
			continue;
		};
		let (vertex_count, face_count) = (vertex_count as usize, face_count as usize);
		let vertex_at = at + 16;
		let face_at = vertex_at + vertex_count * 24;
		if face_at + face_count * 20 > data.len() || vertex_count == 0 {
			continue;
		}
		let base = vertices.len() as u32;
		for v in 0..vertex_count {
			let p = vertex_at + v * 24;
			let (x, y, z) = (r.f32(p).unwrap_or(0.0), r.f32(p + 4).unwrap_or(0.0), r.f32(p + 8).unwrap_or(0.0));
			let point = match tm {
				// Row vectors: the node's matrix holds its rotation and scale in the first three rows and its
				// position in the fourth.
				Some(m) => [
					x * m[0] + y * m[4] + z * m[8] + m[12],
					x * m[1] + y * m[5] + z * m[9] + m[13],
					x * m[2] + y * m[6] + z * m[10] + m[14],
				],
				None => [x, y, z],
			};
			vertices.push(point);
		}
		for f in 0..face_count {
			let p = face_at + f * 20;
			let corner = |k: usize| r.u32(p + k * 4).unwrap_or(u32::MAX);
			let (a, b, c) = (corner(0), corner(1), corner(2));
			let n = vertex_count as u32;
			if a < n && b < n && c < n {
				triangles.push([base + a, base + b, base + c]);
			}
		}
		meshes += 1;
	}
	if triangles.is_empty() {
		return Err("no triangles in this model".into());
	}
	let finite = vertices.iter().filter(|v| v.iter().all(|c| c.is_finite()));
	let (mut low, mut high) = ([f32::MAX; 3], [f32::MIN; 3]);
	for v in finite {
		for k in 0..3 {
			low[k] = low[k].min(v[k]);
			high[k] = high[k].max(v[k]);
		}
	}
	let centre = [(low[0] + high[0]) / 2.0, (low[1] + high[1]) / 2.0, (low[2] + high[2]) / 2.0];
	let radius = vertices
		.iter()
		.map(|v| ((v[0] - centre[0]).powi(2) + (v[1] - centre[1]).powi(2) + (v[2] - centre[2]).powi(2)).sqrt())
		.filter(|d| d.is_finite())
		.fold(0.0f32, f32::max)
		.max(1e-3);
	Ok(Model {
		vertices,
		triangles,
		meshes,
		centre,
		radius,
		cache: Mutex::new(None),
	})
}

impl Model {
	pub fn summary(&self) -> String {
		let meshes = if self.meshes == 1 { "1 mesh".to_owned() } else { format!("{} meshes", self.meshes) };
		format!("{meshes} · {} vertices · {} triangles", self.vertices.len(), self.triangles.len())
	}

	/// The model in a square of the pane's width: drag turns it, the wheel zooms, a double click starts over.
	pub fn show(&self, ui: &mut egui::Ui, height: f32) {
		let pal = crate::theme::pal(ui.ctx());
		let size = Vec2::new(ui.available_width(), height);
		let (rect, response) = ui.allocate_exact_size(size, Sense::click_and_drag());
		let id = response.id.with("view");
		let mut view = ui.data(|d| d.get_temp::<ViewData>(id)).map_or(START, |v| v.0);
		if response.dragged() {
			let delta = response.drag_delta();
			view.yaw += delta.x * 0.01;
			view.pitch = (view.pitch + delta.y * 0.01).clamp(-1.5, 1.5);
		}
		if response.hovered() {
			let scroll = ui.input(|i| i.smooth_scroll_delta.y);
			if scroll != 0.0 {
				view.zoom = (view.zoom * (scroll * 0.0015).exp()).clamp(0.2, 20.0);
				// The wheel turned the model; the pane around it must not scroll as well.
				ui.ctx().input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
			}
		}
		if response.double_clicked() {
			view = START;
		}
		ui.data_mut(|d| d.insert_temp(id, ViewData(view)));
		if response.hovered() || response.dragged() {
			ui.ctx().set_cursor_icon(if response.dragged() {
				egui::CursorIcon::Grabbing
			} else {
				egui::CursorIcon::Grab
			});
		}

		let painter = ui.painter_at(rect);
		painter.rect_filled(rect, egui::CornerRadius::same(6), pal.bg);
		let mut cache = self.cache.lock().unwrap();
		let fresh = cache.as_ref().is_some_and(|(v, r, _)| *v == view && *r == rect);
		if !fresh {
			let base = if pal.dark { Color32::from_rgb(196, 206, 214) } else { Color32::from_rgb(120, 134, 146) };
			*cache = Some((view, rect, self.render(view, rect, base)));
		}
		if let Some((_, _, shapes)) = cache.as_ref() {
			painter.extend(shapes.iter().cloned());
		}
		painter.rect_stroke(
			rect,
			egui::CornerRadius::same(6),
			egui::Stroke::new(1.0, pal.border),
			egui::StrokeKind::Inside,
		);
	}

	fn render(&self, view: View, rect: Rect, base: Color32) -> Vec<Shape> {
		let (sy, cy) = view.yaw.sin_cos();
		let (sp, cp) = view.pitch.sin_cos();
		let scale = rect.width().min(rect.height()) * 0.46 * view.zoom / self.radius;
		let centre = rect.center();
		// Z is up in the client: turn round it, then tilt towards the viewer.
		let turned: Vec<[f32; 3]> = self
			.vertices
			.iter()
			.map(|v| {
				let (x, y, z) = (v[0] - self.centre[0], v[1] - self.centre[1], v[2] - self.centre[2]);
				let (x1, y1) = (x * cy - y * sy, x * sy + y * cy);
				let (y2, z2) = (y1 * cp - z * sp, y1 * sp + z * cp);
				[x1, y2, z2]
			})
			.collect();
		let step = self.triangles.len().div_ceil(DRAWN_TRIANGLES).max(1);
		let mut order: Vec<(f32, [u32; 3])> = self
			.triangles
			.iter()
			.step_by(step)
			.map(|t| {
				let depth = t.iter().map(|&i| turned[i as usize][1]).sum::<f32>();
				(depth, *t)
			})
			.collect();
		// Far first: y grows away from the viewer.
		order.sort_unstable_by(|a, b| b.0.total_cmp(&a.0));
		let light = {
			let l = [0.35f32, -0.8, 0.5];
			let n = (l[0] * l[0] + l[1] * l[1] + l[2] * l[2]).sqrt();
			[l[0] / n, l[1] / n, l[2] / n]
		};
		let project = |p: [f32; 3]| Pos2::new(centre.x + p[0] * scale, centre.y - p[2] * scale);
		let mut mesh = egui::Mesh::default();
		let mut shapes = Vec::new();
		for (_, t) in order {
			let [a, b, c] = t.map(|i| turned[i as usize]);
			let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
			let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
			let n = [u[1] * w[2] - u[2] * w[1], u[2] * w[0] - u[0] * w[2], u[0] * w[1] - u[1] * w[0]];
			let length = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
			if !(length > 0.0) {
				continue;
			}
			// Both sides lit: many client meshes are single planes seen from either side.
			let lambert = ((n[0] * light[0] + n[1] * light[1] + n[2] * light[2]) / length).abs();
			let shade = 0.28 + 0.72 * lambert;
			let color = Color32::from_rgb(
				(base.r() as f32 * shade) as u8,
				(base.g() as f32 * shade) as u8,
				(base.b() as f32 * shade) as u8,
			);
			let first = mesh.vertices.len() as u32;
			for p in [a, b, c] {
				mesh.colored_vertex(project(p), color);
			}
			mesh.add_triangle(first, first + 1, first + 2);
			// egui meshes index with u32 but the backends draw at most 65 536 vertices at a time.
			if mesh.vertices.len() > 65_000 {
				shapes.push(Shape::mesh(std::mem::take(&mut mesh)));
			}
		}
		if !mesh.is_empty() {
			shapes.push(Shape::mesh(mesh));
		}
		shapes
	}
}

#[derive(Clone, Copy)]
struct ViewData(View);
