//! Palettes, typography and the mark. Every colour in the app comes from a [`Palette`]; the current one is
//! stored in the egui context so drawing helpers can read it without threading it through every call.
//!
//! There are two neutral bases, light and dark, plus `System` that follows the OS. The only colour the user
//! picks is the accent: it marks the primary action, the focus, the selection and the progress.

use egui::{
	Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, RichText, Shadow, Stroke, TextStyle, Visuals,
};
use std::sync::Arc;

const PALETTE_KEY: &str = "aionpak.palette";
const DENSITY_KEY: &str = "aionpak.density";

/// Radius of a card, shared so the outline can be repainted over content that covers it.
pub const CARD_RADIUS: u8 = 10;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Theme {
	System,
	Light,
	Dark,
}

impl Theme {
	/// Stable name for the settings file.
	pub fn key(self) -> &'static str {
		match self {
			Theme::System => "system",
			Theme::Light => "light",
			Theme::Dark => "dark",
		}
	}

	/// Reads a settings value, including the names of the palettes earlier versions offered, so an upgrade
	/// keeps the look the user had as closely as the new set allows.
	pub fn from_key(key: &str) -> Option<(Theme, Option<Accent>)> {
		Some(match key {
			"system" => (Theme::System, None),
			"light" => (Theme::Light, None),
			"dark" => (Theme::Dark, None),
			"dual-faction" => (Theme::Dark, Some(Accent::Blue)),
			"aion" | "elyos-tech" | "neon-aether" | "abyss" => (Theme::Dark, Some(Accent::Aether)),
			"asmodian-neon" => (Theme::Dark, Some(Accent::Violet)),
			"ember" => (Theme::Dark, Some(Accent::Ember)),
			"daylight" => (Theme::Light, Some(Accent::Blue)),
			"aion-gold" => (Theme::Light, Some(Accent::Gold)),
			"parchment" => (Theme::Light, Some(Accent::Ember)),
			_ => return None,
		})
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Accent {
	Blue,
	Aether,
	Gold,
	Violet,
	Ember,
}

impl Accent {
	pub const ALL: [Accent; 5] = [Accent::Blue, Accent::Aether, Accent::Gold, Accent::Violet, Accent::Ember];

	pub fn label(self) -> &'static str {
		match self {
			Accent::Blue => "Blue",
			Accent::Aether => "Aether",
			Accent::Gold => "Gold",
			Accent::Violet => "Violet",
			Accent::Ember => "Ember",
		}
	}

	pub fn key(self) -> &'static str {
		match self {
			Accent::Blue => "blue",
			Accent::Aether => "aether",
			Accent::Gold => "gold",
			Accent::Violet => "violet",
			Accent::Ember => "ember",
		}
	}

	pub fn from_key(key: &str) -> Option<Accent> {
		Accent::ALL.into_iter().find(|accent| accent.key() == key)
	}

	/// Accent and the second accent, tuned separately for each base so the contrast holds.
	fn colors(self, dark: bool) -> [u32; 2] {
		match (self, dark) {
			(Accent::Blue, true) => [0x4F9DFF, 0xE0526B],
			(Accent::Blue, false) => [0x2563EB, 0xD9435C],
			(Accent::Aether, true) => [0x3CCFE0, 0xD8B26A],
			(Accent::Aether, false) => [0x0A8A9E, 0xB07E1F],
			(Accent::Gold, true) => [0xE2B85F, 0x62D6E8],
			(Accent::Gold, false) => [0xA36A0D, 0x0A8A9E],
			(Accent::Violet, true) => [0xA98BFF, 0xFF6B8B],
			(Accent::Violet, false) => [0x7445E8, 0xD9435C],
			(Accent::Ember, true) => [0xFF8A3D, 0xF0565B],
			(Accent::Ember, false) => [0xD2600E, 0xC93541],
		}
	}

	/// The swatch shown in Settings, in the shade of the base in force.
	pub fn swatch(self, dark: bool) -> Color32 {
		hex(self.colors(dark)[0])
	}
}

/// The dark base: slate, a graphite with a faint cool tint.
const DARK_BASE: [u32; 6] = [0x0D1016, 0x141821, 0x1C212C, 0x282E3B, 0xE6E9EF, 0x99A0AE];

/// The light base: cool grey and white.
const LIGHT_BASE: [u32; 6] = [0xF4F5F7, 0xFFFFFF, 0xEEF0F3, 0xDCE0E6, 0x15181D, 0x5A6271];

/// Everything the user picks about the look, in one value.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Look {
	pub theme: Theme,
	pub accent: Accent,
}

impl Default for Look {
	fn default() -> Self {
		Look {
			theme: Theme::System,
			accent: Accent::Blue,
		}
	}
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Density {
	Comfortable,
	Compact,
}

impl Density {
	pub const ALL: [Density; 2] = [Density::Comfortable, Density::Compact];

	pub fn label(self) -> &'static str {
		match self {
			Density::Comfortable => "Comfortable",
			Density::Compact => "Compact",
		}
	}

	pub fn key(self) -> &'static str {
		match self {
			Density::Comfortable => "comfortable",
			Density::Compact => "compact",
		}
	}

	/// Height of one archive row.
	pub fn row_height(self) -> f32 {
		match self {
			Density::Comfortable => 26.0,
			Density::Compact => 21.0,
		}
	}

	/// Height of the table header above the rows.
	pub fn header_height(self) -> f32 {
		match self {
			Density::Comfortable => 28.0,
			Density::Compact => 22.0,
		}
	}

	pub fn inspector_width(self) -> f32 {
		match self {
			Density::Comfortable => 340.0,
			Density::Compact => 300.0,
		}
	}
}

/// The interface density in force, stored next to the palette.
pub fn density(ctx: &egui::Context) -> Density {
	ctx.data(|data| data.get_temp(egui::Id::unique(DENSITY_KEY)))
		.unwrap_or(Density::Comfortable)
}

pub fn set_density(ctx: &egui::Context, density: Density) {
	ctx.data_mut(|data| data.insert_temp(egui::Id::unique(DENSITY_KEY), density));
	ctx.all_styles_mut(|style| spacing_for(&mut style.spacing, density));
}

fn spacing_for(spacing: &mut egui::style::Spacing, density: Density) {
	let (padding, interact, gap) = match density {
		Density::Comfortable => (egui::vec2(12.0, 6.0), egui::vec2(40.0, 30.0), 8.0),
		Density::Compact => (egui::vec2(10.0, 4.0), egui::vec2(38.0, 25.0), 6.0),
	};
	spacing.button_padding = padding;
	spacing.interact_size = interact;
	spacing.item_spacing.y = gap;
}

#[derive(Clone, Copy)]
pub struct Palette {
	pub dark: bool,
	/// Deepest layer: window background.
	pub bg: Color32,
	/// Panels, cards and inputs.
	pub surface: Color32,
	/// Raised surfaces: hover, active row, wells.
	pub surface_hi: Color32,
	pub border: Color32,
	pub text: Color32,
	pub text_dim: Color32,
	/// Primary accent: the main action, focus, progress.
	pub accent: Color32,
	/// Second accent, for the few places two kinds of things sit side by side. Never used for status.
	pub accent_alt: Color32,
	pub success: Color32,
	pub warning: Color32,
	pub error: Color32,
	pub selection: Color32,
	/// The blades of the mark.
	pub brand: Color32,
	/// The crystal of the mark.
	pub crystal: Color32,
}

const fn hex(v: u32) -> Color32 {
	Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

impl Palette {
	pub fn new(dark: bool, look: &Look) -> Self {
		let [main, alt] = look.accent.colors(dark);
		let accent = hex(main);
		let [bg, surface, surface_hi, border, text, text_dim] = if dark { DARK_BASE } else { LIGHT_BASE };
		let surface = hex(surface);
		let text = hex(text);
		// The mark is ice: white blades with an aether crystal, deepened on the light base so it holds contrast.
		let (brand, crystal) = if dark {
			(hex(0xE6F2F6), hex(0x5FD3E6))
		} else {
			(hex(0x1F3A4A), hex(0x0A8A9E))
		};
		Palette {
			dark,
			bg: hex(bg),
			surface,
			surface_hi: hex(surface_hi),
			border: hex(border),
			text,
			text_dim: hex(text_dim),
			accent,
			accent_alt: hex(alt),
			success: hex(if dark { 0x3DD68C } else { 0x16A34A }),
			warning: hex(if dark { 0xF2B33D } else { 0xC97A06 }),
			error: hex(if dark { 0xF0565B } else { 0xDC2626 }),
			// A tint of the accent over the surface: visible on both bases without fighting the text.
			selection: surface.lerp_to_gamma(accent, if dark { 0.22 } else { 0.16 }),
			brand,
			crystal,
		}
	}

	/// Third text level: dimmed towards the background, so it works on light and dark palettes alike.
	pub fn muted(&self) -> Color32 {
		self.text_dim.lerp_to_gamma(self.bg, 0.4)
	}

	/// Text that sits on top of an accent-filled control.
	pub fn on_accent(&self) -> Color32 {
		if self.dark { self.bg } else { Color32::WHITE }
	}
}

fn set_current(ctx: &egui::Context, palette: Palette) {
	ctx.data_mut(|data| data.insert_temp(egui::Id::unique(PALETTE_KEY), palette));
}

/// The palette in force, so drawing helpers work anywhere a context is at hand.
pub fn pal(ctx: &egui::Context) -> Palette {
	ctx.data(|data| data.get_temp(egui::Id::unique(PALETTE_KEY)))
		.unwrap_or_else(|| Palette::new(true, &Look::default()))
}

/// Fonts, spacing and widget states. Called once, before the first frame.
pub fn install(ctx: &egui::Context, look: &Look, density: Density) {
	ctx.set_fonts(fonts());
	ctx.data_mut(|data| data.insert_temp(egui::Id::unique(DENSITY_KEY), density));
	apply(ctx, look);
}

/// Switches palette and widget styles without touching the fonts. With `System` the context follows the OS,
/// and calling this again after the OS switched picks the matching base.
pub fn apply(ctx: &egui::Context, look: &Look) {
	ctx.set_theme(match look.theme {
		Theme::System => egui::ThemePreference::System,
		Theme::Dark => egui::ThemePreference::Dark,
		Theme::Light => egui::ThemePreference::Light,
	});
	let palette = Palette::new(ctx.theme() == egui::Theme::Dark, look);
	let current_density = density(ctx);
	ctx.all_styles_mut(|style| *style = style_for(&palette, current_density));
	set_current(ctx, palette);
}

fn family(name: &str) -> FontFamily {
	FontFamily::Name(name.into())
}

fn fonts() -> FontDefinitions {
	let mut fonts = FontDefinitions::default();
	let faces: [(&str, &'static [u8]); 5] = [
		("plex", include_bytes!("../assets/fonts/IBMPlexSans-Regular.ttf")),
		("plex-medium", include_bytes!("../assets/fonts/IBMPlexSans-Medium.ttf")),
		("plex-semibold", include_bytes!("../assets/fonts/IBMPlexSans-SemiBold.ttf")),
		("plex-mono", include_bytes!("../assets/fonts/IBMPlexMono-Regular.ttf")),
		("cinzel", include_bytes!("../assets/fonts/Cinzel.ttf")),
	];
	for (name, bytes) in faces {
		fonts
			.font_data
			.insert(name.to_owned(), Arc::new(FontData::from_static(bytes)));
	}

	// Whatever egui keeps in its own lists stays behind ours as a fallback for symbols Plex does not draw.
	let mut proportional = fonts.families[&FontFamily::Proportional].clone();
	proportional.insert(0, "plex".to_owned());
	fonts.families.insert(FontFamily::Proportional, proportional.clone());

	let mut monospace = fonts.families[&FontFamily::Monospace].clone();
	monospace.insert(0, "plex-mono".to_owned());
	fonts.families.insert(FontFamily::Monospace, monospace);

	for (name, face) in [("medium", "plex-medium"), ("semibold", "plex-semibold"), ("display", "cinzel")] {
		let mut list = proportional.clone();
		list.insert(0, face.to_owned());
		fonts.families.insert(family(name), list);
	}
	fonts
}

fn style_for(palette: &Palette, density: Density) -> egui::Style {
	let mut style = egui::Style {
		text_styles: [
			(TextStyle::Small, FontId::new(11.5, family("medium"))),
			(TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
			(TextStyle::Button, FontId::new(13.5, family("medium"))),
			(TextStyle::Heading, FontId::new(20.0, family("semibold"))),
			(TextStyle::Monospace, FontId::new(12.0, FontFamily::Monospace)),
		]
		.into(),
		..Default::default()
	};

	let spacing = &mut style.spacing;
	spacing.item_spacing = egui::vec2(8.0, 8.0);
	spacing.window_margin = egui::Margin::same(12);
	spacing.menu_margin = egui::Margin::same(6);
	spacing.scroll = egui::style::ScrollStyle::floating();
	spacing_for(spacing, density);

	style.visuals = visuals(palette);
	style
}

fn visuals(palette: &Palette) -> Visuals {
	let mut v = if palette.dark {
		Visuals::dark()
	} else {
		Visuals::light()
	};
	v.panel_fill = palette.bg;
	v.window_fill = palette.surface;
	// Text inputs: sunk into the dark base, white cards on the light one.
	v.extreme_bg_color = if palette.dark { palette.bg } else { palette.surface };
	// Stripes sit only a little above the surface, so a long table does not look striped.
	v.faint_bg_color = palette.surface.lerp_to_gamma(palette.surface_hi, 0.45);
	v.code_bg_color = palette.surface_hi;
	v.override_text_color = Some(palette.text);
	v.hyperlink_color = palette.accent;
	v.warn_fg_color = palette.warning;
	v.error_fg_color = palette.error;
	v.window_stroke = Stroke::new(1.0, palette.border);
	v.window_corner_radius = CornerRadius::same(10);
	v.menu_corner_radius = CornerRadius::same(8);
	v.window_shadow = Shadow {
		offset: [0, 8],
		blur: 24,
		spread: 0,
		color: Color32::from_black_alpha(if palette.dark { 140 } else { 40 }),
	};
	v.popup_shadow = v.window_shadow;
	v.text_cursor.stroke = Stroke::new(1.5, palette.accent);

	v.selection.bg_fill = palette.selection;
	v.selection.stroke = Stroke::new(1.0, palette.accent);

	let raised = palette.surface_hi;
	// Sliders and checkbox wells paint with the inactive fill; on the light base that would be white on white.
	let well = palette
		.surface_hi
		.lerp_to_gamma(palette.text_dim, if palette.dark { 0.12 } else { 0.18 });
	let state = |bg_fill, weak_bg_fill, stroke: Color32, fg: Color32| egui::style::WidgetVisuals {
		bg_fill,
		weak_bg_fill,
		bg_stroke: Stroke::new(1.0, stroke),
		corner_radius: CornerRadius::same(7),
		fg_stroke: Stroke::new(1.0, fg),
		expansion: 0.0,
	};
	let widgets = &mut v.widgets;
	widgets.noninteractive = state(palette.surface, palette.surface, palette.border, palette.text_dim);
	widgets.inactive = state(well, raised, palette.border, palette.text);
	widgets.hovered = state(
		well,
		raised.lerp_to_gamma(palette.text_dim, 0.08),
		palette.border.lerp_to_gamma(palette.text_dim, 0.35),
		palette.text,
	);
	widgets.active = state(palette.selection, palette.selection, palette.accent, palette.text);
	widgets.open = widgets.active;

	v
}

/// The wordmark face. Only for the app name.
pub fn display(palette: &Palette, text: impl Into<String>) -> RichText {
	RichText::new(text)
		.family(family("display"))
		.size(17.0)
		.color(palette.text)
}

pub fn strong(text: impl Into<String>) -> RichText {
	RichText::new(text).family(family("semibold")).size(13.5)
}

/// The title of a screen.
pub fn title(palette: &Palette, text: impl Into<String>) -> RichText {
	RichText::new(text)
		.family(family("semibold"))
		.size(20.0)
		.color(palette.text)
}

pub fn mono(text: impl Into<String>) -> RichText {
	RichText::new(text).family(FontFamily::Monospace).size(12.0)
}

/// Body text one step down, for explanations under a title or a control.
pub fn note(palette: &Palette, text: impl Into<String>) -> RichText {
	RichText::new(text).size(13.0).color(palette.text_dim)
}

/// Small caps used for table headers and field names.
pub fn caption(palette: &Palette, text: impl Into<String>) -> RichText {
	RichText::new(text)
		.family(family("medium"))
		.size(11.0)
		.color(palette.muted())
}

/// A filled accent button. Used once per screen at most.
/// A button whose text stays in the middle when `min_size` makes it wider than the text. egui aligns a button's
/// content by the layout of the `Ui` it is in, which puts it at the left in the usual left-to-right rows and columns;
/// space that grows on both sides centres it.
pub fn centered_button<'a>(text: impl Into<egui::WidgetText>) -> egui::Button<'a> {
	egui::Button::new((egui::Atom::grow(), text.into(), egui::Atom::grow()))
}

/// The main action of a screen, filled with the accent. Add it with `add_primary`, which gives the fill its hover
/// and press shades; a fill set on the button itself would stay the same in every state.
pub fn primary(palette: &Palette, text: &str) -> egui::Button<'static> {
	centered_button(
		RichText::new(text.to_owned())
			.family(family("semibold"))
			.size(13.5)
			.color(palette.on_accent()),
	)
	.stroke(Stroke::NONE)
}

pub fn add_primary(ui: &mut egui::Ui, enabled: bool, button: egui::Button<'_>) -> egui::Response {
	let palette = pal(ui.ctx());
	let toward = if palette.dark { Color32::WHITE } else { Color32::BLACK };
	ui.scope(|ui| {
		let widgets = &mut ui.visuals_mut().widgets;
		widgets.inactive.weak_bg_fill = palette.accent;
		widgets.hovered.weak_bg_fill = palette.accent.lerp_to_gamma(toward, 0.12);
		widgets.active.weak_bg_fill = palette.accent.lerp_to_gamma(toward, 0.22);
		ui.add_enabled(enabled, button)
	})
	.inner
}

pub fn card(palette: &Palette) -> egui::Frame {
	egui::Frame::new()
		.fill(palette.surface)
		.stroke(Stroke::new(1.0, palette.border))
		.corner_radius(CornerRadius::same(CARD_RADIUS))
		.inner_margin(egui::Margin::same(12))
}

/// Mutually exclusive choices drawn as one pill. Reports whether the value changed.
pub fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
	let pal = pal(ui.ctx());
	let mut changed = false;
	egui::Frame::new()
		.fill(pal.surface_hi)
		.stroke(Stroke::new(1.0, pal.border))
		.corner_radius(CornerRadius::same(9))
		.inner_margin(egui::Margin::same(3))
		.show(ui, |ui| {
			ui.horizontal(|ui| {
				ui.spacing_mut().item_spacing.x = 2.0;
				for (option, label) in options {
					let selected = *value == *option;
					// Hovered in the last frame: the colours are set before the button exists, and an option that
					// lights up a frame late is not noticed.
					let id = ui.next_auto_id();
					let hovered = !selected && ui.ctx().read_response(id).is_some_and(|r| r.hovered());
					let hover = ui.ctx().animate_bool_with_time(id.with("hover"), hovered, 0.08);
					let text = RichText::new(*label)
						.family(family("medium"))
						.size(13.0)
						.color(if selected { pal.text } else { pal.text_dim.lerp_to_gamma(pal.text, hover) });
					let button = centered_button(text)
						.fill(if selected {
							pal.surface
						} else {
							// Half way to the chosen option's fill: a hint of what a click does, not the choice itself.
							pal.surface.gamma_multiply(0.55 * hover)
						})
						.stroke(if selected {
							Stroke::new(1.0, pal.border)
						} else {
							Stroke::NONE
						})
						.corner_radius(CornerRadius::same(7))
						.min_size(egui::vec2(92.0, 26.0));
					if ui.add(button).clicked() && !selected {
						*value = *option;
						changed = true;
					}
				}
			});
		});
	changed
}

// The mark: an A of two blades with an aether crystal in its counter, scaled to a square box. Convex polygons
// only, so the painter can fill them and the icon renderer can test them.
const A_LEFT: [(f32, f32); 4] = [(0.200, 0.800), (0.470, 0.180), (0.535, 0.180), (0.315, 0.800)];
const A_RIGHT: [(f32, f32); 4] = [(0.465, 0.180), (0.530, 0.180), (0.800, 0.800), (0.685, 0.800)];
const CRYSTAL: [(f32, f32); 4] = [(0.500, 0.480), (0.575, 0.600), (0.500, 0.720), (0.425, 0.600)];
const FACET: [(f32, f32); 4] = [(0.500, 0.480), (0.5315, 0.600), (0.500, 0.686), (0.4685, 0.600)];

const BLADES: [&[(f32, f32)]; 2] = [&A_LEFT, &A_RIGHT];

fn poly_points(rect: egui::Rect, poly: &[(f32, f32)]) -> Vec<egui::Pos2> {
	poly.iter()
		.map(|&(x, y)| egui::pos2(rect.left() + x * rect.width(), rect.top() + y * rect.height()))
		.collect()
}

/// Draws the mark in the current layout, in the colours of the palette in force.
pub fn mark(ui: &mut egui::Ui, size: f32) -> egui::Response {
	mark_lit(ui, size, 0.0)
}

/// The mark with its blades lit from inside, `glow` from 0 (dark) to 1: a halo of the crystal's colour round the
/// A and the blades tinted towards it, as a gas tube's glass when the current comes on.
pub fn mark_lit(ui: &mut egui::Ui, size: f32, glow: f32) -> egui::Response {
	let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
	let palette = pal(ui.ctx());
	let painter = ui.painter();
	let glow = glow.clamp(0.0, 1.0);
	if glow > 0.0 {
		// A blur made of strokes: wide and faint outside, narrow and bright at the edge.
		let scale = size / 64.0;
		for (width, alpha) in [(16.0, 0.05), (11.0, 0.08), (7.0, 0.14), (4.0, 0.24), (2.0, 0.42)] {
			let color = palette.crystal.gamma_multiply(alpha * glow);
			for poly in BLADES {
				painter.add(egui::Shape::closed_line(
					poly_points(rect, poly),
					Stroke::new(width * scale, color),
				));
			}
		}
	}
	let blade = palette.brand.lerp_to_gamma(palette.crystal, 0.45 * glow);
	for poly in BLADES {
		painter.add(egui::Shape::convex_polygon(poly_points(rect, poly), blade, Stroke::NONE));
	}
	painter.add(egui::Shape::convex_polygon(
		poly_points(rect, &CRYSTAL),
		palette.crystal,
		Stroke::NONE,
	));
	response
}

// The icon keeps its own colours: it is the product's face, not a themed surface.
const ICON_TOP: u32 = 0x0B1C26;
const ICON_BOTTOM: u32 = 0x14303E;
const ICON_BLADE_TOP: u32 = 0xF2FAFC;
const ICON_BLADE_BOTTOM: u32 = 0xBFD9E2;
const ICON_CRYSTAL: u32 = 0x62D6E8;
const ICON_FACET: u32 = 0xFFFFFF;

/// Window icon, drawn from the same polygons so the app never ships a stale bitmap.
pub fn app_icon(px: u32) -> egui::IconData {
	const SS: usize = 4;
	let n = px.clamp(16, 1024) as usize;
	let mut rgba = vec![0u8; n * n * 4];
	for y in 0..n {
		for x in 0..n {
			let mut acc = [0.0f32; 3];
			let mut hits = 0.0f32;
			for sy in 0..SS {
				for sx in 0..SS {
					let fx = (x as f32 + (sx as f32 + 0.5) / SS as f32) / n as f32;
					let fy = (y as f32 + (sy as f32 + 0.5) / SS as f32) / n as f32;
					if let Some(c) = icon_sample(fx, fy) {
						for (sum, channel) in acc.iter_mut().zip(c) {
							*sum += channel;
						}
						hits += 1.0;
					}
				}
			}
			let i = (y * n + x) * 4;
			if hits > 0.0 {
				for (k, sum) in acc.iter().enumerate() {
					rgba[i + k] = (sum / hits * 255.0).clamp(0.0, 255.0) as u8;
				}
			}
			rgba[i + 3] = (hits / (SS * SS) as f32 * 255.0) as u8;
		}
	}
	egui::IconData {
		rgba,
		width: n as u32,
		height: n as u32,
	}
}

fn icon_sample(x: f32, y: f32) -> Option<[f32; 3]> {
	let c = |v: u32| rgb(hex(v));
	// A rounded square, a little inside the canvas like the system icons around it.
	let half = 0.47;
	let radius = 0.21;
	let dx = (x - 0.5).abs() - (half - radius);
	let dy = (y - 0.5).abs() - (half - radius);
	let distance = dx.max(0.0).hypot(dy.max(0.0)) + dx.max(dy).min(0.0) - radius;
	if distance > 0.0 {
		return None;
	}

	let mut color = mix(c(ICON_TOP), c(ICON_BOTTOM), y);
	// A faint glow of the crystal's colour, and a hairline highlight along the upper edge.
	let glow = (1.0 - ((x - 0.5).powi(2) + (y - 0.6).powi(2)).sqrt() / 0.45).max(0.0);
	color = mix(color, c(ICON_CRYSTAL), 0.10 * glow * glow);
	if distance > -0.006 {
		// Strongest at the top, gone by two thirds down, so the edge has no visible step.
		color = mix(color, [1.0, 1.0, 1.0], 0.12 * (1.0 - y / 0.65).max(0.0));
	}

	if BLADES.iter().any(|p| inside(p, x, y)) {
		color = mix(c(ICON_BLADE_TOP), c(ICON_BLADE_BOTTOM), (y - 0.18) / 0.62);
	}
	if inside(&FACET, x, y) {
		color = c(ICON_FACET);
	} else if inside(&CRYSTAL, x, y) {
		color = c(ICON_CRYSTAL);
	}
	Some(color)
}

fn inside(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
	let mut result = false;
	let mut j = poly.len() - 1;
	for i in 0..poly.len() {
		let (xi, yi) = poly[i];
		let (xj, yj) = poly[j];
		if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
			result = !result;
		}
		j = i;
	}
	result
}

fn rgb(c: Color32) -> [f32; 3] {
	[c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
	let t = t.clamp(0.0, 1.0);
	[
		a[0] + (b[0] - a[0]) * t,
		a[1] + (b[1] - a[1]) * t,
		a[2] + (b[2] - a[2]) * t,
	]
}
