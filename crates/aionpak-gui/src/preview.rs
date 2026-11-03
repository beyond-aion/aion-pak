//! What the details pane shows above the metadata: the picture, the beginning of the text, or the first bytes.

use aionpak_commons::pak::{Codec, Entry, PakReader};
use aionpak_commons::xml::{self, Encoding};
use crate::audio::{self, Clip};
use crate::model::{self, Model};
use egui::{Color32, ColorImage, FontFamily, FontId, Rect, RichText, Sense, TextureHandle, TextureOptions, Vec2};
use std::sync::Arc;

const TEXT_LIMIT: usize = 96 * 1024;
const HEX_LIMIT: usize = 512;
const IMAGE_SIDE: usize = 1024;
/// Enlarged icons keep their pixels crisp; reduced pictures are smoothed.
const PREVIEW_TEXTURE: TextureOptions = TextureOptions {
	magnification: egui::TextureFilter::Nearest,
	minification: egui::TextureFilter::Linear,
	..TextureOptions::LINEAR
};

pub struct Preview {
	pub kind: Kind,
	/// The first bytes of the entry, whatever the file is, for the Hex tab.
	pub hex: Vec<u8>,
	/// What the content says about itself, for the Details tab: its type, and the dimensions, format, length or
	/// lines it has. Taken from the bytes the preview reads anyway, so it costs no extra read.
	pub facts: Vec<(&'static str, String)>,
}

pub enum Kind {
	Image {
		texture: TextureHandle,
		note: String,
	},
	Text {
		text: String,
		truncated: bool,
		note: String,
	},
	Hex {
		note: String,
	},
	/// A font file, set in its own family for a sample.
	Font {
		family: FontFamily,
		note: String,
	},
	Sound(Arc<Clip>),
	Model(Arc<Model>),
	Message(String),
}

#[derive(Clone, Copy)]
enum Detected {
	Image,
	Text,
	Font,
	Sound,
	Model,
	Binary,
}

pub fn load(ctx: &egui::Context, reader: &PakReader, entry: &Entry, name: &str) -> Preview {
	let data = match reader.read(entry, &mut Codec::new(0)) {
		Ok(data) => data,
		Err(err) => {
			return Preview {
				kind: Kind::Message(err.to_string()),
				hex: Vec::new(),
				facts: Vec::new(),
			};
		}
	};
	let hex = data[..data.len().min(HEX_LIMIT)].to_vec();
	let detected = detected(name, &data);
	let mut facts = facts(name, &data, detected);

	let kind = match detected {
		Detected::Image => {
			let (pixels, note) = if let Some((pixels, width, height)) = decode_dds(&data) {
				(
					scaled(&pixels, width, height, IMAGE_SIDE),
					format!("{width}×{height} · uncompressed"),
				)
			} else {
				match image::load_from_memory(&data) {
					Ok(picture) => {
						let (width, height) = (picture.width() as usize, picture.height() as usize);
						let rgba = picture.to_rgba8();
						(
							scaled(rgba.as_raw(), width, height, IMAGE_SIDE),
							format!("{width}×{height}"),
						)
					}
					Err(err) => {
						return Preview {
							kind: Kind::Message(format!("cannot decode the image: {err}")),
							hex,
							facts,
						};
					}
				}
			};
			let texture = ctx.load_texture(format!("preview:{name}"), pixels, PREVIEW_TEXTURE);
			Kind::Image { texture, note }
		}
		Detected::Font => {
			// One family per font, named after the entry and its CRC: egui keeps fonts once added, so viewing the
			// same font again adds nothing.
			let family_name = format!("preview-font:{}:{:08x}", name, entry.crc);
			let family = FontFamily::Name(family_name.clone().into());
			ctx.add_font(egui::epaint::text::FontInsert::new(
				&family_name,
				egui::FontData::from_owned(data.clone()),
				vec![egui::epaint::text::InsertFontFamily {
					family: family.clone(),
					priority: egui::epaint::text::FontPriority::Highest,
				}],
			));
			Kind::Font {
				family,
				note: format!("{} bytes", data.len()),
			}
		}
		Detected::Sound => match audio::decode(&data) {
			Ok(clip) => {
				let channels = match clip.channels {
					1 => "mono".to_owned(),
					2 => "stereo".to_owned(),
					n => format!("{n} channels"),
				};
				facts.push(("SOUND", format!("{} Hz · {channels}", clip.rate)));
				let seconds = ogg_seconds(&data, clip.rate).unwrap_or_else(|| clip.seconds());
				let more = if clip.truncated && !data.starts_with(b"OggS") { "+" } else { "" };
				facts.push(("LENGTH", format!("{}{more}", duration(seconds))));
				Kind::Sound(Arc::new(clip))
			}
			Err(err) => Kind::Message(format!("cannot play this sound: {err}")),
		},
		Detected::Model => match model::parse(&data) {
			Ok(model) => {
				facts.push(("MESHES", model.meshes.to_string()));
				Kind::Model(Arc::new(model))
			}
			Err(err) => Kind::Message(format!("cannot show this model: {err}")),
		},
		Detected::Text => {
			let decoded = if xml::is_xml_name(name) {
				xml::decode(&data, name, Encoding::Utf8)
					.map(|decoded| decoded.text)
					.unwrap_or(data)
			} else {
				plain_text(data)
			};
			let lines = decoded.iter().filter(|&&b| b == b'\n').count() + usize::from(!decoded.ends_with(b"\n"));
			facts.push(("LINES", lines.to_string()));
			let truncated = decoded.len() > TEXT_LIMIT;
			let end = decoded.len().min(TEXT_LIMIT);
			Kind::Text {
				text: String::from_utf8_lossy(&decoded[..end]).into_owned(),
				truncated,
				note: format!("{} bytes", decoded.len()),
			}
		}
		Detected::Binary => Kind::Hex {
			note: format!("{} bytes", data.len()),
		},
	};
	Preview { kind, hex, facts }
}

/// The type, its MIME name, and what the header of a texture or the start of a text tells.
fn facts(name: &str, data: &[u8], detected: Detected) -> Vec<(&'static str, String)> {
	let extension = name
		.rsplit_once('.')
		.map(|(_, extension)| extension.to_ascii_lowercase())
		.unwrap_or_default();
	let (kind, mime) = type_of(&extension, data, detected);
	let mut facts = vec![("TYPE", kind.to_owned()), ("MIME", mime.to_owned())];
	if let Some((width, height, format, mips, cube)) = dds_header(data) {
		facts.push(("DIMENSIONS", format!("{width}×{height}{}", if cube { " · cube map" } else { "" })));
		facts.push(("PIXEL FORMAT", format));
		facts.push(("MIPMAPS", mips.to_string()));
	} else if let Some((width, height)) = image_size(data) {
		facts.push(("DIMENSIONS", format!("{width}×{height}")));
	}
	if matches!(detected, Detected::Text) {
		let encoding = if data.starts_with(&[0xff, 0xfe]) {
			"UTF-16 LE"
		} else if data.starts_with(&[0xfe, 0xff]) {
			"UTF-16 BE"
		} else if data.starts_with(&[0xef, 0xbb, 0xbf]) {
			"UTF-8 with BOM"
		} else if data.is_ascii() {
			"ASCII"
		} else if std::str::from_utf8(data).is_ok() {
			"UTF-8"
		} else {
			"8-bit code page"
		};
		// Compiled and encrypted XML are not text on disk; the Form field says what they are.
		if !xml::is_xml_name(name) || matches!(xml::detect(data), xml::Form::Utf16 | xml::Form::Text) {
			facts.push(("ENCODING", encoding.to_owned()));
		}
	}
	facts
}

/// A name for what the file is, and the MIME type it would be served as. The client's own formats have no
/// registered type and go as application/octet-stream.
fn type_of(extension: &str, data: &[u8], detected: Detected) -> (&'static str, &'static str) {
	match extension {
		"dds" => ("DirectDraw texture", "image/vnd-ms.dds"),
		"tga" => ("Targa image", "image/x-tga"),
		"png" => ("PNG image", "image/png"),
		"jpg" | "jpeg" => ("JPEG image", "image/jpeg"),
		"bmp" => ("bitmap", "image/bmp"),
		"gif" => ("GIF image", "image/gif"),
		"tmb" => ("thumbnail bitmap", "image/bmp"),
		"xml" => ("XML", "application/xml"),
		"html" | "htm" => ("HTML", "text/html"),
		"xsd" => ("XML schema", "application/xml"),
		"xsl" => ("XSL stylesheet", "application/xslt+xml"),
		"lua" => ("Lua script", "text/x-lua"),
		"json" => ("JSON", "application/json"),
		"csv" => ("CSV table", "text/csv"),
		"ini" | "cfg" => ("settings", "text/plain"),
		"txt" | "log" => ("text", "text/plain"),
		"ogg" => ("Ogg Vorbis sound", "audio/ogg"),
		"wav" => ("WAVE sound", "audio/wav"),
		"mp3" => ("MP3 sound", "audio/mpeg"),
		"ttf" => ("TrueType font", "font/ttf"),
		"otf" => ("OpenType font", "font/otf"),
		"cgf" => ("CryEngine geometry", "application/octet-stream"),
		"cga" => ("CryEngine animated geometry", "application/octet-stream"),
		"chr" => ("CryEngine character", "application/octet-stream"),
		"caf" => ("CryEngine animation", "application/octet-stream"),
		"cal" => ("CryEngine animation list", "text/plain"),
		"mtl" => ("CryEngine material", "application/xml"),
		"fsb" => ("FMOD sound bank", "application/octet-stream"),
		"swf" | "gfx" => ("Flash movie", "application/x-shockwave-flash"),
		"dll" | "exe" => ("Windows program", "application/vnd.microsoft.portable-executable"),
		"pak" => ("archive", "application/zip"),
		_ => match detected {
			Detected::Image if data.starts_with(b"DDS ") => ("DirectDraw texture", "image/vnd-ms.dds"),
			Detected::Image if data.starts_with(b"\x89PNG") => ("PNG image", "image/png"),
			Detected::Image if data.starts_with(b"GIF8") => ("GIF image", "image/gif"),
			Detected::Image => ("bitmap", "image/bmp"),
			Detected::Text => ("text", "text/plain"),
			Detected::Font => ("font", "font/ttf"),
			Detected::Sound if data.starts_with(b"OggS") => ("Ogg Vorbis sound", "audio/ogg"),
			Detected::Sound => ("WAVE sound", "audio/wav"),
			Detected::Model => ("CryEngine geometry", "application/octet-stream"),
			Detected::Binary if data.starts_with(b"CryTek") => ("CryEngine file", "application/octet-stream"),
			Detected::Binary if data.starts_with(b"MZ") => ("Windows program", "application/vnd.microsoft.portable-executable"),
			Detected::Binary if data.starts_with(b"PK\x03\x04") => ("zip archive", "application/zip"),
			Detected::Binary => ("binary data", "application/octet-stream"),
		},
	}
}

/// Width, height, pixel format, mipmap count and whether it is a cube map, from a DDS header.
fn dds_header(data: &[u8]) -> Option<(u32, u32, String, u32, bool)> {
	if data.len() < 128 || &data[..4] != b"DDS " {
		return None;
	}
	let word = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
	let (height, width) = (word(12), word(16));
	let mips = word(28).max(1);
	let pixel_flags = word(80);
	let fourcc = &data[84..88];
	let format = if pixel_flags & 0x4 != 0 {
		match fourcc {
			b"DXT1" => "DXT1 (BC1)".to_owned(),
			b"DXT3" => "DXT3 (BC2)".to_owned(),
			b"DXT5" => "DXT5 (BC3)".to_owned(),
			b"ATI1" | b"BC4U" => "ATI1 (BC4)".to_owned(),
			b"ATI2" | b"BC5U" => "ATI2 (BC5)".to_owned(),
			b"DX10" => "DX10 header".to_owned(),
			other if other.iter().all(|b| b.is_ascii_graphic()) => String::from_utf8_lossy(other).into_owned(),
			other => format!("D3DFMT {}", u32::from_le_bytes([other[0], other[1], other[2], other[3]])),
		}
	} else {
		let bits = word(88);
		let alpha = pixel_flags & 0x1 != 0 && word(104) != 0;
		match (bits, alpha) {
			(32, true) => "RGBA 8-bit".to_owned(),
			(32, false) => "RGBX 8-bit".to_owned(),
			(24, _) => "RGB 8-bit".to_owned(),
			(16, _) => "16-bit".to_owned(),
			(8, _) if pixel_flags & 0x20000 != 0 => "luminance 8-bit".to_owned(),
			(8, _) => "alpha 8-bit".to_owned(),
			(bits, _) => format!("{bits}-bit uncompressed"),
		}
	};
	let cube = word(112) & 0x200 != 0;
	Some((width, height, format, mips, cube))
}

/// Width and height of a PNG, GIF, BMP or TGA from its header, without decoding it.
fn image_size(data: &[u8]) -> Option<(u32, u32)> {
	let le16 = |at: usize| Some(u32::from(u16::from_le_bytes([*data.get(at)?, *data.get(at + 1)?])));
	let le32 = |at: usize| Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?));
	let be32 = |at: usize| Some(u32::from_be_bytes(data.get(at..at + 4)?.try_into().ok()?));
	if data.starts_with(b"\x89PNG") {
		return Some((be32(16)?, be32(20)?));
	}
	if data.starts_with(b"GIF8") {
		return Some((le16(6)?, le16(8)?));
	}
	if data.starts_with(b"BM") {
		return Some((le32(18)?, le32(22)?.cast_signed().unsigned_abs()));
	}
	None
}

/// The length of an Ogg stream from the granule position of its last page: exact, without decoding.
fn ogg_seconds(data: &[u8], rate: u32) -> Option<f32> {
	if !data.starts_with(b"OggS") || rate == 0 {
		return None;
	}
	let tail = data.len().saturating_sub(64 * 1024);
	let at = data[tail..].windows(4).rposition(|w| w == b"OggS")? + tail;
	let granule = u64::from_le_bytes(data.get(at + 6..at + 14)?.try_into().ok()?);
	(granule != u64::MAX).then(|| granule as f32 / rate as f32)
}

fn duration(seconds: f32) -> String {
	let total = seconds.round() as u64;
	if total < 60 { format!("{seconds:.1} s") } else { format!("{}:{:02}", total / 60, total % 60) }
}

/// By the name where it says enough, else by the bytes: the client keeps text and pictures under names of its own
/// (.seq, .cal, .phy, .cai and more are text; .tmb is a bitmap).
fn detected(name: &str, data: &[u8]) -> Detected {
	let extension = name
		.rsplit_once('.')
		.map(|(_, extension)| extension.to_ascii_lowercase())
		.unwrap_or_default();
	match extension.as_str() {
		"dds" | "tga" | "png" | "jpg" | "jpeg" | "bmp" | "gif" => return Detected::Image,
		"xml" | "html" | "htm" | "txt" | "ini" | "lua" | "json" | "csv" | "cfg" | "log" | "xsd" | "xsl" | "dtd" => {
			return Detected::Text;
		}
		"ttf" | "otf" => return Detected::Font,
		_ => {}
	}
	if audio::is_sound(name) {
		return Detected::Sound;
	}
	if model::is_model(name) {
		return Detected::Model;
	}
	let image_magic: [&[u8]; 4] = [b"BM", b"DDS ", b"\x89PNG", b"GIF8"];
	if data.len() > 54 && image_magic.iter().any(|magic| data.starts_with(magic)) {
		return Detected::Image;
	}
	if looks_like_text(data) {
		return Detected::Text;
	}
	Detected::Binary
}

/// Text when the first 4 KB hold no NUL and almost nothing below the space but tabs and line ends, or when they
/// start with a UTF-16 byte order mark.
fn looks_like_text(data: &[u8]) -> bool {
	if data.starts_with(&[0xff, 0xfe]) || data.starts_with(&[0xfe, 0xff]) {
		return true;
	}
	let head = &data[..data.len().min(4096)];
	if head.is_empty() || head.contains(&0) {
		return false;
	}
	let control = head
		.iter()
		.filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0c))
		.count();
	control * 100 <= head.len()
}

/// Text in UTF-16 when it has a byte order mark, else UTF-8, else code page 949: the comments the Korean
/// developers left in scripts.
fn plain_text(data: Vec<u8>) -> Vec<u8> {
	let utf16 = |bytes: &[u8], big: bool| -> Vec<u8> {
		let units: Vec<u16> = bytes
			.chunks_exact(2)
			.map(|p| if big { u16::from_be_bytes([p[0], p[1]]) } else { u16::from_le_bytes([p[0], p[1]]) })
			.collect();
		String::from_utf16_lossy(&units).into_bytes()
	};
	match data.get(..2) {
		Some([0xff, 0xfe]) => utf16(&data[2..], false),
		Some([0xfe, 0xff]) => utf16(&data[2..], true),
		_ if std::str::from_utf8(&data).is_ok() => data,
		_ => aionpak_commons::names::to_text(&data).into_bytes(),
	}
}

/// Nearest-neighbour is enough for a thumbnail and keeps big textures off the GPU.
fn scaled(pixels: &[u8], width: usize, height: usize, max: usize) -> ColorImage {
	let step = width.max(height).div_ceil(max).max(1);
	if step == 1 {
		return ColorImage::from_rgba_unmultiplied([width, height], pixels);
	}
	let (out_w, out_h) = (width.div_ceil(step), height.div_ceil(step));
	let mut out = Vec::with_capacity(out_w * out_h * 4);
	for y in (0..height).step_by(step) {
		for x in (0..width).step_by(step) {
			let at = (y * width + x) * 4;
			out.extend_from_slice(&pixels[at..at + 4]);
		}
	}
	ColorImage::from_rgba_unmultiplied([out_w, out_h], &out)
}

/// Aion textures are often plain 32-bit DDS, which the `image` crate does not decode: it handles the DXT
/// variants only. This reads the uncompressed case; everything else still goes through `image`.
fn decode_dds(data: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
	if data.len() < 128 || &data[..4] != b"DDS " {
		return None;
	}
	let word = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
	let (width, height) = (word(16) as usize, word(12) as usize);
	let fourcc = word(84);
	if fourcc != 0 || width == 0 || height == 0 {
		return None;
	}
	let bits = word(88) as usize;
	if bits != 24 && bits != 32 {
		return None;
	}
	let (red, green, blue, alpha) = (word(92), word(96), word(100), word(104));
	let stride = bits / 8;
	let start = 128;
	if data.len() < start + width * height * stride {
		return None;
	}

	let channel = |mask: u32, value: u32| -> u8 {
		if mask == 0 {
			return 255;
		}
		let shift = mask.trailing_zeros();
		let max = mask >> shift;
		(((value & mask) >> shift) * 255 / max) as u8
	};

	let mut pixels = Vec::with_capacity(width * height * 4);
	for index in 0..width * height {
		let at = start + index * stride;
		let value = match stride {
			4 => u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]),
			_ => u32::from(data[at]) | (u32::from(data[at + 1]) << 8) | (u32::from(data[at + 2]) << 16),
		};
		pixels.push(channel(red, value));
		pixels.push(channel(green, value));
		pixels.push(channel(blue, value));
		pixels.push(if alpha == 0 { 255 } else { channel(alpha, value) });
	}
	Some((pixels, width, height))
}

pub fn show(ui: &mut egui::Ui, preview: &Preview) {
	let pal = crate::theme::pal(ui.ctx());
	match &preview.kind {
		Kind::Image { texture, note } => {
			let available = ui.available_width();
			let natural = texture.size_vec2();
			// Small icons are shown up to four times larger, so a 32 px texture is not a speck in the pane.
			let scale = (available / natural.x).min(220.0 / natural.y).min(4.0);
			let shown = (natural * scale).max(Vec2::splat(1.0));
			let (rect, _) = ui.allocate_exact_size(Vec2::new(available, shown.y.max(32.0)), Sense::hover());
			let image_rect = Rect::from_center_size(rect.center(), shown);
			checkerboard(ui, image_rect, &pal);
			ui.painter().image(
				texture.id(),
				image_rect,
				Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
				Color32::WHITE,
			);
			ui.label(crate::theme::mono(note).color(pal.muted()));
		}
		Kind::Text { text, truncated, note } => {
			code_block(ui, text, 210.0);
			let line = if *truncated {
				format!("{note} — showing the beginning")
			} else {
				note.clone()
			};
			ui.label(crate::theme::mono(line).color(pal.muted()));
		}
		Kind::Hex { note } => {
			code_block(ui, &hex_dump(&preview.hex), 190.0);
			ui.label(crate::theme::mono(format!("{note} — first {} bytes", preview.hex.len())).color(pal.muted()));
		}
		Kind::Font { family, note } => {
			egui::Frame::new()
				.fill(pal.bg)
				.stroke(egui::Stroke::new(1.0, pal.border))
				.corner_radius(egui::CornerRadius::same(6))
				.inner_margin(egui::Margin::same(10))
				.show(ui, |ui| {
					ui.set_width(ui.available_width());
					for (size, text) in [
						(26.0, "Aion Pak 0123456789"),
						(17.0, "The quick brown fox jumps over the lazy dog"),
						(17.0, "Съешь же ещё этих мягких французских булок"),
						(14.0, "ÀÉÎÕÜ àéîõü ß ¿¡ — «» € ¥ © ™ ½ ¾"),
						(14.0, "가나다라마바사 아이온"),
					] {
						ui.label(RichText::new(text).font(FontId::new(size, family.clone())).color(pal.text));
					}
				});
			ui.label(crate::theme::mono(format!("{note} — characters the font lacks show as boxes")).color(pal.muted()));
		}
		Kind::Sound(clip) => sound(ui, clip, &pal),
		Kind::Model(model) => {
			model.show(ui, 240.0);
			ui.label(crate::theme::mono(model.summary()).color(pal.muted()));
			ui.label(crate::theme::note(&pal, "Drag to turn, scroll to zoom, double-click to start over."));
		}
		Kind::Message(message) => {
			ui.label(RichText::new(message).size(12.0).color(pal.text_dim));
		}
	}
}

/// A play button and the waveform, filled up to where playback is; a click on the waveform plays from there.
fn sound(ui: &mut egui::Ui, clip: &Clip, pal: &crate::theme::Palette) {
	let position = clip.position();
	let playing = position.is_some();
	let error_id = ui.scope_id().with("sound-error");
	ui.horizontal(|ui| {
		let label = if playing { "■  Stop" } else { "▶  Play" };
		if ui.add(egui::Button::new(label).min_size(Vec2::new(84.0, 26.0))).clicked() {
			let result = if playing { Ok(clip.stop()) } else { clip.play(0.0) };
			ui.data_mut(|d| d.insert_temp(error_id, result.err()));
		}
		let total = clip.seconds();
		let at = position.unwrap_or(0.0);
		let time = |s: f32| format!("{}:{:04.1}", (s / 60.0) as u32, s % 60.0);
		ui.label(crate::theme::mono(format!("{} / {}", time(at), time(total))).color(pal.text_dim));
	});
	let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 72.0), Sense::click());
	let painter = ui.painter_at(rect);
	painter.rect_filled(rect, egui::CornerRadius::same(6), pal.bg);
	let columns = clip.envelope.len().max(1);
	let width = rect.width() / columns as f32;
	let middle = rect.center().y;
	let half = rect.height() / 2.0 - 6.0;
	let done = position.map_or(0.0, |p| p / clip.seconds().max(1e-3));
	for (i, (low, high)) in clip.envelope.iter().enumerate() {
		let x = rect.left() + (i as f32 + 0.5) * width;
		let played = (i as f32 + 0.5) / columns as f32 <= done;
		let color = if played { pal.accent } else { pal.text_dim.gamma_multiply(0.6) };
		let top = middle - high.max(0.01) * half;
		let bottom = middle - low.min(-0.01) * half;
		painter.line_segment([egui::pos2(x, top), egui::pos2(x, bottom)], egui::Stroke::new((width * 0.7).max(1.0), color));
	}
	painter.rect_stroke(rect, egui::CornerRadius::same(6), egui::Stroke::new(1.0, pal.border), egui::StrokeKind::Inside);
	if response.clicked()
		&& let Some(pointer) = response.interact_pointer_pos()
	{
		let from = (pointer.x - rect.left()) / rect.width() * clip.seconds();
		let result = clip.play(from);
		ui.data_mut(|d| d.insert_temp(error_id, result.err()));
	}
	let response = response.on_hover_text("Click to play from here");
	if response.hovered() {
		ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
	}
	let channels = if clip.channels == 1 { "mono".to_owned() } else { format!("{} channels", clip.channels) };
	let cut = if clip.truncated { " · the first 3 minutes" } else { "" };
	ui.label(crate::theme::mono(format!("{} Hz · {channels}{cut}", clip.rate)).color(pal.muted()));
	if let Some(Some(err)) = ui.data(|d| d.get_temp::<Option<String>>(error_id)) {
		ui.label(RichText::new(err).size(12.0).color(pal.error));
	}
	// Only while it plays, and ten times a second is enough for the bar and the clock: an idle preview draws nothing.
	if playing {
		ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
	}
}

/// The Hex tab: the first bytes of any entry, whatever the file is.
pub fn show_hex(ui: &mut egui::Ui, preview: &Preview) {
	let pal = crate::theme::pal(ui.ctx());
	code_block(ui, &hex_dump(&preview.hex), 280.0);
	ui.label(crate::theme::mono(format!("first {} bytes of the entry", preview.hex.len())).color(pal.muted()));
}

fn code_block(ui: &mut egui::Ui, text: &str, height: f32) {
	let pal = crate::theme::pal(ui.ctx());
	egui::Frame::new()
		.fill(pal.bg)
		.stroke(egui::Stroke::new(1.0, pal.border))
		.corner_radius(egui::CornerRadius::same(6))
		.inner_margin(egui::Margin::same(6))
		.show(ui, |ui| {
			egui::ScrollArea::both()
				.id_salt("preview-code")
				.max_height(height.min(ui.available_height()).max(60.0))
				.auto_shrink([false, false])
				.show(ui, |ui| {
					ui.add(egui::Label::new(RichText::new(text).monospace().size(11.0).color(pal.text_dim)).extend());
				});
		});
}

fn hex_dump(bytes: &[u8]) -> String {
	let mut dump = String::new();
	for (row, chunk) in bytes.chunks(16).enumerate() {
		let hex: Vec<String> = chunk.iter().map(|byte| format!("{byte:02x}")).collect();
		let ascii: String = chunk
			.iter()
			.map(|byte| {
				if (0x20..0x7f).contains(byte) {
					*byte as char
				} else {
					'.'
				}
			})
			.collect();
		dump.push_str(&format!("{:08x}  {:<47}  {ascii}\n", row * 16, hex.join(" ")));
	}
	dump
}

fn checkerboard(ui: &egui::Ui, rect: Rect, pal: &crate::theme::Palette) {
	const CELL: f32 = 8.0;
	let light = pal.surface_hi;
	let dark = pal.border.gamma_multiply(0.5);
	let mut y = rect.top();
	let mut row = 0;
	while y < rect.bottom() {
		let mut x = rect.left();
		let mut column = 0;
		while x < rect.right() {
			let cell = Rect::from_min_max(
				egui::pos2(x, y),
				egui::pos2((x + CELL).min(rect.right()), (y + CELL).min(rect.bottom())),
			);
			ui.painter()
				.rect_filled(cell, 0.0, if (row + column) % 2 == 0 { light } else { dark });
			x += CELL;
			column += 1;
		}
		y += CELL;
		row += 1;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// A 32-bit uncompressed header, 128 bytes, the shape the client ships most of its icons in.
	fn dds_header(width: u32, height: u32) -> Vec<u8> {
		let mut data = Vec::new();
		data.extend_from_slice(b"DDS ");
		data.extend_from_slice(&124u32.to_le_bytes());
		data.extend_from_slice(&0x1007u32.to_le_bytes());
		data.extend_from_slice(&height.to_le_bytes());
		data.extend_from_slice(&width.to_le_bytes());
		data.extend_from_slice(&0u32.to_le_bytes());
		data.extend_from_slice(&0u32.to_le_bytes());
		data.extend_from_slice(&0u32.to_le_bytes());
		for _ in 0..11 {
			data.extend_from_slice(&0u32.to_le_bytes());
		}
		data.extend_from_slice(&32u32.to_le_bytes());
		data.extend_from_slice(&0x41u32.to_le_bytes());
		data.extend_from_slice(&0u32.to_le_bytes());
		data.extend_from_slice(&32u32.to_le_bytes());
		data.extend_from_slice(&0x00ff0000u32.to_le_bytes());
		data.extend_from_slice(&0x0000ff00u32.to_le_bytes());
		data.extend_from_slice(&0x000000ffu32.to_le_bytes());
		data.extend_from_slice(&0xff000000u32.to_le_bytes());
		for _ in 0..5 {
			data.extend_from_slice(&0u32.to_le_bytes());
		}
		assert_eq!(data.len(), 128);
		data
	}

	#[test]
	fn decodes_uncompressed_dds() {
		let mut data = dds_header(2, 2);
		data.extend_from_slice(&[0, 0, 255, 255]);
		data.extend_from_slice(&[0, 255, 0, 255]);
		data.extend_from_slice(&[255, 0, 0, 255]);
		data.extend_from_slice(&[255, 255, 255, 255]);

		let (pixels, width, height) = decode_dds(&data).expect("decoded");
		assert_eq!((width, height), (2, 2));
		assert_eq!(&pixels[..4], &[255, 0, 0, 255], "first pixel is red");
		assert_eq!(&pixels[4..8], &[0, 255, 0, 255], "second is green");
	}

	#[test]
	fn rejects_compressed_dds() {
		let mut data = dds_header(2, 2);
		data[84..88].copy_from_slice(b"DXT1");
		assert!(decode_dds(&data).is_none());
	}

	#[test]
	fn hex_dump_is_aligned() {
		let dump = hex_dump(&[0x41, 0x42]);
		assert!(dump.starts_with("00000000  41 42"), "{dump}");
		assert!(dump.trim_end().ends_with("AB"), "{dump}");
	}
}
