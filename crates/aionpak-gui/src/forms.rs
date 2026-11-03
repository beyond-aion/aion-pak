//! The forms of the work screens: paths, options and the button that starts the job.

use crate::jobs;
use crate::theme::{self, Palette};
use aionpak_commons::ops::{self, FlavorMode, XmlMode};
use aionpak_commons::xml::{Encoding, Form};
use egui::{RichText, Vec2};
use std::path::{Path, PathBuf};

/// Width of the label column, so the fields of a form line up.
const LABEL_WIDTH: f32 = 128.0;

#[derive(Clone, Copy)]
enum Pick {
	Folder,
	/// A file or a folder: two buttons.
	Either,
	/// A new archive to write.
	Save,
}

fn pick(pick: Pick, title: &str, current: Option<&PathBuf>) -> Option<PathBuf> {
	let mut dialog = rfd::FileDialog::new().set_title(title);
	if let Some(start) = current.and_then(|path| if path.is_dir() { Some(path.as_path()) } else { path.parent() }) {
		dialog = dialog.set_directory(start);
	}
	match pick {
		Pick::Folder => dialog.pick_folder(),
		Pick::Save => dialog.add_filter("Aion archive", &["pak"]).save_file(),
		Pick::Either => dialog.pick_file(),
	}
}

fn field_label(ui: &mut egui::Ui, pal: &Palette, label: &str) -> egui::Response {
	ui.allocate_ui_with_layout(Vec2::new(LABEL_WIDTH, 24.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
		ui.set_width(LABEL_WIDTH);
		ui.label(RichText::new(label).size(13.0).color(pal.text_dim))
	})
	.inner
}

/// A labelled path that can be typed or pasted, or picked with the button on the right. `hint` shows while it
/// is empty.
fn path_row(ui: &mut egui::Ui, pal: &Palette, label: &str, value: &mut Option<PathBuf>, kind: Pick, hint: &str) {
	ui.horizontal(|ui| {
		field_label(ui, pal, label);
		// Room for the buttons after the field, measured from their labels so nothing is pushed past the edge.
		let labels: &[&str] = match kind {
			Pick::Either => &["File…", "Folder…"],
			Pick::Save => &["Save as…"],
			Pick::Folder => &["Browse…"],
		};
		let font = egui::TextStyle::Button.resolve(ui.style());
		let padding = ui.spacing().button_padding.x * 2.0 + ui.spacing().item_spacing.x;
		let buttons: f32 = labels
			.iter()
			.map(|label| ui.fonts_mut(|f| f.layout_no_wrap((*label).to_owned(), font.clone(), pal.text).size().x) + padding)
			.sum();
		let width = (ui.available_width() - buttons - ui.spacing().item_spacing.x).clamp(120.0, 640.0);
		let mut text = value.as_ref().map(|path| path.display().to_string()).unwrap_or_default();
		let edit = egui::TextEdit::singleline(&mut text)
			.hint_text(hint)
			.font(egui::TextStyle::Monospace)
			.desired_width(width)
			.margin(egui::Margin::symmetric(8, 5));
		if ui.add(edit).changed() {
			*value = (!text.is_empty()).then(|| PathBuf::from(text));
		}
		let picked = match kind {
			Pick::Folder | Pick::Save => {
				let label = if matches!(kind, Pick::Save) { "Save as…" } else { "Browse…" };
				ui.button(label).clicked().then(|| pick(kind, hint, value.as_ref())).flatten()
			}
			Pick::Either => {
				let file = ui.button("File…").clicked();
				let folder = ui.button("Folder…").clicked();
				if file {
					pick(Pick::Either, hint, value.as_ref())
				} else if folder {
					pick(Pick::Folder, hint, value.as_ref())
				} else {
					None
				}
			}
		};
		if let Some(path) = picked {
			*value = Some(path);
		}
	});
}

fn combo_row<T: PartialEq + Copy>(
	ui: &mut egui::Ui,
	pal: &Palette,
	label: &str,
	id: &str,
	value: &mut T,
	options: &[(T, &str)],
	hint: &str,
) {
	ui.horizontal(|ui| {
		field_label(ui, pal, label).on_hover_text(hint);
		let selected = options
			.iter()
			.find(|(option, _)| option == value)
			.map(|(_, text)| *text)
			.unwrap_or_default();
		egui::ComboBox::from_id_salt(id)
			.selected_text(selected)
			.width(260.0)
			.show_ui(ui, |ui| {
				for (option, text) in options {
					ui.selectable_value(value, *option, *text);
				}
			})
			.response
			.on_hover_text(hint);
	});
}

fn level_row(ui: &mut egui::Ui, pal: &Palette, level: &mut u8) {
	ui.horizontal(|ui| {
		field_label(ui, pal, "Compression")
			.on_hover_text("0 stores the data as is, 9 balances speed and size, 12 is the smallest and slowest");
		ui.spacing_mut().slider_width = 220.0;
		ui.add(egui::Slider::new(level, 0..=12));
		let what = match *level {
			0 => "store only",
			1..=5 => "fast",
			6..=9 => "balanced",
			_ => "smallest, slow",
		};
		ui.label(theme::note(pal, what));
	});
}

/// A titled group of options on a card, so a long form reads as a few steps.
fn section(ui: &mut egui::Ui, pal: &Palette, title: &str, add: impl FnOnce(&mut egui::Ui)) {
	theme::card(pal).show(ui, |ui| {
		ui.set_width(ui.available_width());
		ui.label(theme::caption(pal, title.to_uppercase()));
		ui.add_space(4.0);
		add(ui);
	});
	ui.add_space(10.0);
}

fn run_button(ui: &mut egui::Ui, pal: &Palette, busy: bool, ready: bool, text: &str, missing: &str) -> bool {
	ui.add_space(4.0);
	let response = theme::add_primary(ui, !busy && ready, theme::primary(pal, text).min_size(Vec2::new(120.0, 34.0)));
	let response = if busy {
		response.on_disabled_hover_text("Another job is running; wait for it or cancel it in the status bar")
	} else {
		response.on_disabled_hover_text(missing)
	};
	response.clicked()
}

/// The extraction options of the workspace: where the files go and what happens to the XML on the way.
pub struct ExtractBar {
	pub out: Option<PathBuf>,
	pub decode: bool,
	pub utf8: bool,
	pub force: bool,
}

impl ExtractBar {
	pub fn new(out: Option<PathBuf>) -> Self {
		ExtractBar {
			out,
			decode: true,
			utf8: false,
			force: false,
		}
	}

	/// Makes sure there is a destination, asking for one if there is none yet. False if the user cancelled.
	pub fn ensure_out(&mut self) -> bool {
		if self.out.is_none() {
			self.out = pick(Pick::Folder, "Extract to", None);
		}
		self.out.is_some()
	}

	pub fn encoding(&self) -> Option<Encoding> {
		self.decode
			.then_some(if self.utf8 { Encoding::Utf8 } else { Encoding::Utf16 })
	}

	pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette) {
		ui.horizontal_wrapped(|ui| {
			ui.label(theme::note(pal, "Extract to"));
			let mut text = self.out.as_ref().map(|path| path.display().to_string()).unwrap_or_default();
			// The checkboxes and the button take about 430 px; the path gets the rest, within reason.
			let width = (ui.available_width() - 520.0).clamp(160.0, 460.0);
			let edit = egui::TextEdit::singleline(&mut text)
				.hint_text("asked on the first extract")
				.font(egui::TextStyle::Monospace)
				.desired_width(width)
				.margin(egui::Margin::symmetric(8, 4));
			if ui.add(edit).changed() {
				self.out = (!text.is_empty()).then(|| PathBuf::from(text));
			}
			if ui.button("Browse…").on_hover_text("The folder the files are written under").clicked()
				&& let Some(path) = pick(Pick::Folder, "Extract to", self.out.as_ref())
			{
				self.out = Some(path);
			}
			ui.add_space(6.0);
			ui.checkbox(&mut self.decode, "Decode XML and HTML")
				.on_hover_text("Turn the client's compiled and encrypted XML and HTML into readable text");
			ui.add_enabled_ui(self.decode, |ui| {
				ui.checkbox(&mut self.utf8, "as UTF-8")
					.on_hover_text("Write that text as UTF-8 instead of UTF-16");
			});
			ui.checkbox(&mut self.force, "Overwrite")
				.on_hover_text("Extract into a folder that already holds files, replacing the ones with the same name");
		});
	}
}

/// What becomes of the XML files when packing or merging.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum XmlChoice {
	/// Compiled where the client reads them compiled; the rest (level files) as plain text.
	Compile,
	/// Every XML as UTF-16 text.
	Text,
	/// The form each file had in the source archive.
	Original,
	/// The bytes exactly as on disk.
	Keep,
}

impl XmlChoice {
	/// The pack mode, and whether a file that would end up encrypted is stored as text.
	fn pack(self) -> (XmlMode, bool) {
		match self {
			XmlChoice::Compile => (XmlMode::To(Form::Compiled), true),
			XmlChoice::Text => (XmlMode::To(Form::Utf16), true),
			XmlChoice::Original => (XmlMode::Original, false),
			XmlChoice::Keep => (XmlMode::Keep, false),
		}
	}

	/// The same for merging volumes, where there is no source archive apart from the stored data.
	fn optimize(self) -> (Option<Form>, bool) {
		match self {
			XmlChoice::Compile => (Some(Form::Compiled), true),
			XmlChoice::Text => (Some(Form::Utf16), true),
			XmlChoice::Original | XmlChoice::Keep => (None, false),
		}
	}
}

const XML_HINT: &str = "Compiled XML is what the client reads fastest. Level files (Levels/…) cannot be compiled \
	and are stored as text; with Text, every XML becomes UTF-16 text, which any editor opens. The client reads UTF-8 \
	as 8-bit text in the system code page, so it is not offered.";

/// What becomes of the HTML files. They cannot be compiled.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HtmlChoice {
	/// Encrypted ones as text: UTF-16, or the decrypted 8-bit bytes where the text is in a code page.
	Text,
	Original,
	Keep,
}

impl HtmlChoice {
	fn pack(self) -> (XmlMode, bool) {
		match self {
			HtmlChoice::Text => (XmlMode::Original, true),
			HtmlChoice::Original => (XmlMode::Original, false),
			HtmlChoice::Keep => (XmlMode::Keep, false),
		}
	}

	fn optimize(self) -> (Option<Form>, bool) {
		(None, self == HtmlChoice::Text)
	}
}

const HTML_HINT: &str = "The client reads encrypted and plain HTML alike; text is smaller to load and opens in any \
	editor. Files whose text is in a code page (Korean leftovers) keep their 8-bit bytes, decrypted.";

fn xml_rows(ui: &mut egui::Ui, pal: &Palette, id: &str, xml: &mut XmlChoice, html: &mut HtmlChoice, original: &str) {
	let xml_options = [
		(XmlChoice::Compile, "compile*"),
		(XmlChoice::Text, "decode to UTF-16 text"),
		(XmlChoice::Original, original),
		(XmlChoice::Keep, "exactly as on disk"),
	];
	let html_options = [
		(HtmlChoice::Text, "decode to text"),
		(HtmlChoice::Original, original),
		(HtmlChoice::Keep, "exactly as on disk"),
	];
	// Merging has no files on disk: "as stored" covers both.
	let keep = !id.starts_with("optimize");
	let xml_options: Vec<_> = xml_options
		.into_iter()
		.filter(|(c, _)| keep || *c != XmlChoice::Keep)
		.collect();
	let html_options: Vec<_> = html_options
		.into_iter()
		.filter(|(c, _)| keep || *c != HtmlChoice::Keep)
		.collect();
	combo_row(ui, pal, "XML files", &format!("{id}-xml"), xml, &xml_options, XML_HINT);
	combo_row(
		ui,
		pal,
		"HTML files",
		&format!("{id}-html"),
		html,
		&html_options,
		HTML_HINT,
	);
	if *xml == XmlChoice::Compile {
		ui.label(theme::note(
			pal,
			"* Level files cannot be compiled; they are stored as plain text.",
		));
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum FlavorChoice {
	Plain,
	Aion,
	Original,
}

impl FlavorChoice {
	fn mode(self) -> FlavorMode {
		match self {
			FlavorChoice::Plain => FlavorMode::Plain,
			FlavorChoice::Aion => FlavorMode::Aion,
			FlavorChoice::Original => FlavorMode::Original,
		}
	}
}

pub struct PakForm {
	pub dir: Option<PathBuf>,
	pub out: Option<PathBuf>,
	pub level: u8,
	pub xml: XmlChoice,
	pub html: HtmlChoice,
	pub flavor: FlavorChoice,
	pub recompress: bool,
	pub force: bool,
	pub repack: bool,
	/// The archive filled in from the folder's name, so a folder picked next replaces it but a typed one stays.
	auto_out: Option<PathBuf>,
	last_dir: Option<PathBuf>,
}

impl Default for PakForm {
	fn default() -> Self {
		PakForm {
			dir: None,
			out: None,
			level: 9,
			xml: XmlChoice::Compile,
			html: HtmlChoice::Text,
			flavor: FlavorChoice::Plain,
			recompress: false,
			force: false,
			repack: false,
			auto_out: None,
			last_dir: None,
		}
	}
}

impl PakForm {
	pub fn complete(&self) -> bool {
		self.dir.is_some() && self.out.is_some()
	}

	pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette, busy: bool) -> bool {
		section(ui, pal, "What to pack", |ui| {
			theme::segmented(
				ui,
				&mut self.repack,
				&[(false, "One folder"), (true, "Whole client tree")],
			);
			ui.add_space(2.0);
			if self.repack {
				ui.label(theme::note(
					pal,
					"Every <name>.pak/ folder under the tree goes back into its archive, as Browse → Extract tree \
					 laid them out. Nothing else in the tree is touched.",
				));
				path_row(ui, pal, "Tree", &mut self.dir, Pick::Folder, "the extracted client folder");
				path_row(
					ui,
					pal,
					"Write into",
					&mut self.out,
					Pick::Folder,
					"the client folder, usually",
				);
			} else {
				path_row(ui, pal, "Folder", &mut self.dir, Pick::Folder, "the folder to pack");
				self.suggest_archive();
				path_row(ui, pal, "Archive", &mut self.out, Pick::Save, "the .pak to write");
			}
		});
		section(ui, pal, "Options", |ui| {
			level_row(ui, pal, &mut self.level);
			xml_rows(
				ui,
				pal,
				"pack",
				&mut self.xml,
				&mut self.html,
				"as in the source archive",
			);
			combo_row(
				ui,
				pal,
				"Zip format",
				"pack-flavor",
				&mut self.flavor,
				&[
					(FlavorChoice::Plain, "standard zip"),
					(FlavorChoice::Aion, "Aion (scrambled headers)"),
					(FlavorChoice::Original, "same as the source archive"),
				],
				"The client reads both. Standard zip also opens in any archiver; the Aion variant scrambles the \
				 headers the way the original archives do.",
			);
			ui.checkbox(&mut self.recompress, "Recompress the data instead of copying it")
				.on_hover_text("Without it, files you did not change keep the compressed data of the source archive");
			ui.checkbox(&mut self.force, "Allow a folder without a .aionpak manifest")
				.on_hover_text(
					"Such a folder may hold only part of the archive it replaces, so this has to be asked for",
				);
		});
		run_button(
			ui,
			pal,
			busy,
			self.complete(),
			if self.repack { "Repack tree" } else { "Pack" },
			"Choose the folder and where to write first",
		)
	}

	/// `skills/` packs into `skills.pak` beside it, unless an archive was chosen by hand. A folder already named
	/// `<name>.pak` (an extracted tree's layout) gets no suggestion: the archive would take the folder's own name.
	fn suggest_archive(&mut self) {
		if self.dir == self.last_dir {
			return;
		}
		self.last_dir = self.dir.clone();
		if self.out.is_some() && self.out != self.auto_out {
			return;
		}
		let suggested = self.dir.as_ref().and_then(|dir| {
			let name = dir.file_name()?.to_str()?;
			if name.to_ascii_lowercase().ends_with(".pak") {
				return None;
			}
			Some(dir.with_file_name(format!("{name}.pak")))
		});
		self.out = suggested.clone();
		self.auto_out = suggested;
	}

	pub fn spec(&self) -> Option<jobs::PakSpec> {
		let (xml, decrypt) = self.xml.pack();
		Some(jobs::PakSpec {
			dir: self.dir.clone()?,
			out: self.out.clone()?,
			level: self.level,
			xml,
			html: self.html.pack(),
			flavor: self.flavor.mode(),
			force: self.force,
			recompress: self.recompress,
			decrypt,
			repack: self.repack,
		})
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConvertKind {
	Decode,
	Encode,
}

pub struct ConvertForm {
	pub input: Option<PathBuf>,
	pub in_place: bool,
	pub out: Option<PathBuf>,
	pub kind: ConvertKind,
	pub utf8: bool,
	pub xml: Form,
	/// None leaves the HTML files as they are.
	pub html: Option<Form>,
}

impl Default for ConvertForm {
	fn default() -> Self {
		ConvertForm {
			input: None,
			in_place: true,
			out: None,
			kind: ConvertKind::Decode,
			utf8: false,
			xml: Form::Compiled,
			html: None,
		}
	}
}

impl ConvertForm {
	pub fn complete(&self) -> bool {
		self.input.is_some() && (self.in_place || self.out.is_some())
	}

	pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette, busy: bool) -> bool {
		section(ui, pal, "Direction", |ui| {
			theme::segmented(
				ui,
				&mut self.kind,
				&[
					(ConvertKind::Decode, "To readable text"),
					(ConvertKind::Encode, "Back for the client"),
				],
			);
			ui.add_space(2.0);
			match self.kind {
				ConvertKind::Decode => {
					ui.checkbox(&mut self.utf8, "Write UTF-8 instead of UTF-16");
				}
				ConvertKind::Encode => {
					combo_row(
						ui,
						pal,
						"XML files",
						"convert-xml",
						&mut self.xml,
						// The forms Pack offers: encrypted only hides the text, and 8-bit text is read in the system
						// code page. The command line still writes both.
						&[(Form::Compiled, "compile*"), (Form::Utf16, "decode to UTF-16 text")],
						XML_HINT,
					);
					combo_row(
						ui,
						pal,
						"HTML files",
						"convert-html",
						&mut self.html,
						// Not "decode to text" as on Pack: that keeps 8-bit bytes where the text is in a code page,
						// and this always writes UTF-16.
						&[(None, "leave as they are"), (Some(Form::Utf16), "decode to UTF-16 text")],
						"HTML cannot be compiled. The client reads plain HTML as it is; UTF-16 text also holds \
						 characters outside the system code page.",
					);
					if self.xml == Form::Compiled {
						ui.label(theme::note(pal, "* Level files cannot be compiled; they are left as they are."));
					}
				}
			}
		});
		section(ui, pal, "Files", |ui| {
			path_row(ui, pal, "Input", &mut self.input, Pick::Either, "an XML or HTML file, or a folder");
			theme::segmented(
				ui,
				&mut self.in_place,
				&[(true, "Replace in place"), (false, "Write to another folder")],
			);
			if !self.in_place {
				path_row(
					ui,
					pal,
					"Output folder",
					&mut self.out,
					Pick::Folder,
					"the folder layout is mirrored under it",
				);
			}
		});
		run_button(ui, pal, busy, self.complete(), "Convert", "Choose the input first")
	}

	pub fn spec(&self) -> Option<jobs::ConvertSpec> {
		Some(jobs::ConvertSpec {
			input: self.input.clone()?,
			out: if self.in_place { None } else { Some(self.out.clone()?) },
			mode: match self.kind {
				ConvertKind::Decode => ops::Convert::Decode(if self.utf8 { Encoding::Utf8 } else { Encoding::Utf16 }),
				ConvertKind::Encode => ops::Convert::Encode { xml: self.xml, html: self.html },
			},
		})
	}
}

/// A path field that follows what is open in Browse until a path is chosen there by hand. Cleared by hand, it
/// stays empty until something else is opened.
#[derive(Default)]
struct FollowOpen {
	/// The path last put into the field.
	offered: Option<PathBuf>,
	/// What was open the last time the screen was shown.
	seen: Option<PathBuf>,
}

impl FollowOpen {
	/// `fits` is asked only when what is open changes, so it may touch the disk.
	fn apply(&mut self, field: &mut Option<PathBuf>, open: Option<&Path>, fits: impl Fn(&Path) -> bool) {
		if open == self.seen.as_deref() {
			return;
		}
		self.seen = open.map(Path::to_path_buf);
		let open = open.filter(|path| fits(path)).map(Path::to_path_buf);
		if field.is_none() || *field == self.offered {
			*field = open.clone();
			self.offered = open;
		}
	}
}

#[derive(Default)]
pub struct CompareForm {
	pub first: Option<PathBuf>,
	pub second: Option<PathBuf>,
	follow: FollowOpen,
}

impl CompareForm {
	/// The archive or folder open in Browse goes in as the first side.
	pub fn follow_open(&mut self, open: Option<&Path>) {
		self.follow.apply(&mut self.first, open, |_| true);
	}

	pub fn complete(&self) -> bool {
		self.first.is_some() && self.second.is_some()
	}

	pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette, busy: bool) -> bool {
		section(ui, pal, "Two archives or two folders", |ui| {
			path_row(ui, pal, "First", &mut self.first, Pick::Either, "archive or folder");
			path_row(ui, pal, "Second", &mut self.second, Pick::Either, "archive or folder");
		});
		run_button(ui, pal, busy, self.complete(), "Compare", "Choose both sides first")
	}

	pub fn spec(&self) -> Option<(PathBuf, PathBuf)> {
		Some((self.first.clone()?, self.second.clone()?))
	}
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OptimizeMode {
	/// Write the volumes to a folder and leave the client alone.
	Copy,
	/// Write the volumes into the client and move the replaced archives to a backup.
	Apply,
	/// Delete the volumes and move the archives back from a backup.
	Undo,
}

impl OptimizeMode {
	/// What the chosen mode does, shown under the mode switch.
	pub fn note(self) -> &'static str {
		match self {
			OptimizeMode::Copy => {
				"The client is not touched: the volumes go to the output folder, and aionpak-optimize.txt lists the \
				 archives they replace. Good for a first try."
			}
			OptimizeMode::Apply => {
				"Close the game first. The volumes go into the client and the archives they replace move to a backup \
				 folder, so Undo can put them back."
			}
			OptimizeMode::Undo => {
				"Deletes the volumes and moves the archives back from the backup folder. Close the game first."
			}
		}
	}
}

/// The unit the volume size is given in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SizeUnit {
	Mb,
	Gb,
}

impl SizeUnit {
	fn bytes(self) -> u64 {
		match self {
			SizeUnit::Mb => 1 << 20,
			SizeUnit::Gb => 1 << 30,
		}
	}

	/// What can be typed in this unit: from 50 MB (`MIN_VOLUME_SIZE`) up to the 4 GB a zip archive can hold.
	fn range(self) -> std::ops::RangeInclusive<u32> {
		match self {
			SizeUnit::Mb => (aionpak_commons::optimize::MIN_VOLUME_SIZE >> 20) as u32..=4096,
			SizeUnit::Gb => 1..=4,
		}
	}
}

pub struct OptimizeForm {
	pub client: Option<PathBuf>,
	pub out: Option<PathBuf>,
	pub mode: OptimizeMode,
	pub backup: Option<PathBuf>,
	/// Largest volume, in `volume_unit`.
	pub volume_size: u32,
	pub volume_unit: SizeUnit,
	pub level: u8,
	pub recompress: bool,
	/// With `recompress`, also recompress in place the archives that are not merged.
	pub recompress_rest: bool,
	pub xml: XmlChoice,
	pub html: HtmlChoice,
	follow: FollowOpen,
}

impl Default for OptimizeForm {
	fn default() -> Self {
		OptimizeForm {
			client: None,
			out: None,
			mode: OptimizeMode::Copy,
			backup: None,
			volume_size: 512,
			volume_unit: SizeUnit::Mb,
			level: 9,
			recompress: true,
			recompress_rest: true,
			xml: XmlChoice::Compile,
			html: HtmlChoice::Text,
			follow: FollowOpen::default(),
		}
	}
}

impl OptimizeForm {
	/// The client folder open in Browse goes in as the client: a folder with Data/ in it, as a client has.
	pub fn follow_open(&mut self, open: Option<&Path>) {
		self.follow.apply(&mut self.client, open, |path| path.join("Data").is_dir());
	}

	/// The largest volume in bytes; 4 GB is one byte more than a zip archive can address, so it is capped.
	fn volume_bytes(&self) -> u64 {
		(u64::from(self.volume_size) * self.volume_unit.bytes()).min(aionpak_commons::pak::MAX_ARCHIVE_SIZE)
	}

	pub fn complete(&self) -> bool {
		if self.client.is_none() {
			return false;
		}
		match self.mode {
			OptimizeMode::Copy => self.out.is_some(),
			OptimizeMode::Apply => true,
			OptimizeMode::Undo => self.backup.is_some(),
		}
	}

	pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette, busy: bool) -> bool {
		section(ui, pal, "Client and mode", |ui| {
			path_row(ui, pal, "Client", &mut self.client, Pick::Folder, "the game folder");
			theme::segmented(
				ui,
				&mut self.mode,
				&[
					(OptimizeMode::Copy, "Build to a folder"),
					(OptimizeMode::Apply, "Apply to the client"),
					(OptimizeMode::Undo, "Undo"),
				],
			);
			let color = if self.mode == OptimizeMode::Copy {
				pal.text_dim
			} else {
				pal.warning
			};
			ui.label(RichText::new(self.mode.note()).size(13.0).color(color));
			match self.mode {
				OptimizeMode::Copy => {
					path_row(ui, pal, "Output folder", &mut self.out, Pick::Folder, "where the volumes go")
				}
				OptimizeMode::Apply => path_row(
					ui,
					pal,
					"Backup folder",
					&mut self.backup,
					Pick::Folder,
					"<client>/aionpak-backup-<time> if left empty",
				),
				OptimizeMode::Undo => path_row(
					ui,
					pal,
					"Backup folder",
					&mut self.backup,
					Pick::Folder,
					"the folder an earlier apply wrote",
				),
			}
		});

		if self.mode != OptimizeMode::Undo {
			section(ui, pal, "Options", |ui| {
				ui.horizontal(|ui| {
					field_label(ui, pal, "Volume size")
						.on_hover_text("The largest a merged volume may grow; a zip archive holds at most 4 GB");
					let range = self.volume_unit.range();
					ui.add(
						egui::DragValue::new(&mut self.volume_size)
							.range(range)
							.speed(if self.volume_unit == SizeUnit::Mb { 10.0 } else { 0.05 })
							.min_decimals(0)
							.max_decimals(0),
					)
					.on_hover_text("Type a number or drag");
					let before = self.volume_unit;
					egui::ComboBox::from_id_salt("volume-unit")
						.selected_text(if self.volume_unit == SizeUnit::Mb { "MB" } else { "GB" })
						.width(64.0)
						.show_ui(ui, |ui| {
							ui.selectable_value(&mut self.volume_unit, SizeUnit::Mb, "MB");
							ui.selectable_value(&mut self.volume_unit, SizeUnit::Gb, "GB");
						});
					// Keep the size when the unit changes: 2 GB becomes 2048 MB, 1536 MB becomes 2 GB.
					if before != self.volume_unit {
						let bytes = u64::from(self.volume_size) * before.bytes();
						let unit = self.volume_unit.bytes();
						let value = ((bytes + unit / 2) / unit) as u32;
						let range = self.volume_unit.range();
						self.volume_size = value.clamp(*range.start(), *range.end());
					}
				});
				xml_rows(ui, pal, "optimize", &mut self.xml, &mut self.html, "as stored");
				ui.checkbox(&mut self.recompress, "Recompress the data instead of copying it")
					.on_hover_text("Slower; the stored data is reused otherwise");
				if self.recompress {
					level_row(ui, pal, &mut self.level);
					ui.checkbox(&mut self.recompress_rest, "Also recompress the archives that are not merged")
						.on_hover_text(
							"Those that stay in their folders and those under Levels/<map>, rewritten in place with the \
							 same names; signed archives and ones that do not decode are left as they are",
						);
				}
			});
		}
		run_button(
			ui,
			pal,
			busy,
			self.complete(),
			match self.mode {
				OptimizeMode::Copy => "Build volumes",
				OptimizeMode::Apply => "Apply",
				OptimizeMode::Undo => "Undo",
			},
			"Fill in the folders above first",
		)
	}

	pub fn spec(&self) -> Option<jobs::OptimizeSpec> {
		let mode = match self.mode {
			OptimizeMode::Copy => jobs::OptimizeMode::Copy { out: self.out.clone()? },
			OptimizeMode::Apply => jobs::OptimizeMode::Apply {
				backup: self.backup.clone(),
			},
			OptimizeMode::Undo => jobs::OptimizeMode::Undo {
				backup: self.backup.clone()?,
			},
		};
		Some(jobs::OptimizeSpec {
			client: self.client.clone()?,
			mode,
			level: self.recompress.then_some(self.level),
			xml: self.xml.optimize().0,
			decrypt: self.xml.optimize().1,
			html: Some(self.html.optimize()),
			volume_size: self.volume_bytes(),
			recompress_rest: self.recompress && self.recompress_rest,
		})
	}
}

pub struct VerifyForm {
	pub deep: bool,
	/// The archive or folder to check; filled in from Browse until chosen by hand.
	pub source: Option<PathBuf>,
	follow: FollowOpen,
}

impl Default for VerifyForm {
	fn default() -> Self {
		VerifyForm {
			deep: true,
			source: None,
			follow: FollowOpen::default(),
		}
	}
}

impl VerifyForm {
	/// The archive open in Browse, else the folder being browsed, goes in as what to check.
	pub fn follow_open(&mut self, open: Option<&Path>) {
		self.follow.apply(&mut self.source, open, |_| true);
	}

	/// Points the form at `path` as if Browse had offered it, so it goes on following what is opened next.
	pub fn point_at(&mut self, path: PathBuf) {
		self.source = Some(path.clone());
		self.follow.offered = Some(path);
	}

	pub fn ui(&mut self, ui: &mut egui::Ui, pal: &Palette, busy: bool) -> bool {
		section(ui, pal, "What to check", |ui| {
			path_row(
				ui,
				pal,
				"Archive or folder",
				&mut self.source,
				Pick::Either,
				"a folder checks every archive under it",
			);
		});
		section(ui, pal, "How thoroughly", |ui| {
			theme::segmented(ui, &mut self.deep, &[(true, "Full"), (false, "Quick")]);
			ui.label(theme::note(
				pal,
				if self.deep {
					"Decompresses every file and compares its checksum (CRC) with the one the archive records. A \
					 mismatch means a broken file: a truncated write or a packer bug."
				} else {
					"Checks the archive's structure only, without decompressing anything. Takes seconds even for \
					 the biggest archives."
				},
			));
		});
		run_button(ui, pal, busy, self.source.is_some(), "Check", "Choose what to check first")
	}
}

#[cfg(test)]
mod follow_tests {
	use super::FollowOpen;
	use std::path::{Path, PathBuf};

	#[test]
	fn follows_what_is_open_until_chosen_by_hand() {
		let mut follow = FollowOpen::default();
		let mut field = None;
		follow.apply(&mut field, Some(Path::new("a.pak")), |_| true);
		assert_eq!(field, Some(PathBuf::from("a.pak")));
		// Something else opened: the field still holds the offer, so it follows.
		follow.apply(&mut field, Some(Path::new("b.pak")), |_| true);
		assert_eq!(field, Some(PathBuf::from("b.pak")));
		// Chosen by hand, it stays whatever is opened next.
		field = Some(PathBuf::from("mine.pak"));
		follow.apply(&mut field, Some(Path::new("c.pak")), |_| true);
		assert_eq!(field, Some(PathBuf::from("mine.pak")));
	}

	#[test]
	fn cleared_by_hand_stays_empty_until_something_else_opens() {
		let mut follow = FollowOpen::default();
		let mut field = None;
		follow.apply(&mut field, Some(Path::new("a.pak")), |_| true);
		field = None;
		follow.apply(&mut field, Some(Path::new("a.pak")), |_| true);
		assert_eq!(field, None);
		follow.apply(&mut field, Some(Path::new("b.pak")), |_| true);
		assert_eq!(field, Some(PathBuf::from("b.pak")));
		// What does not fit (a folder that is not a client) is not offered.
		let mut other = None;
		FollowOpen::default().apply(&mut other, Some(Path::new("x")), |_| false);
		assert_eq!(other, None);
	}
}
