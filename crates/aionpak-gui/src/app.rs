//! The shell: title bar, section rail, content and the status bar at the bottom.

use crate::browse::{self, Archive};
use crate::clock;
use crate::compare_view;
use crate::folder::{self, FolderAction, FolderView};
use crate::forms;
use crate::icons::{self, Glyph};
use crate::jobs::{self, Job, JobEvent, JobKind};
use crate::open;
use crate::preview;
use crate::settings::Settings;
use crate::theme::{self, Accent, Density, Palette, Theme};
use crate::units::{human_bytes, human_time};
use egui::{
	Align, Align2, Color32, CornerRadius, FontFamily, FontId, Frame, Key, Label, Layout, Margin, Modifiers, Rect,
	RichText, Sense, Stroke, StrokeKind, Vec2,
};
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

const TITLE_HEIGHT: f32 = 44.0;
/// Seconds the section rail and the log take to slide open or shut.
const RAIL_SLIDE: f32 = 0.16;
const STATUS_HEIGHT: f32 = 36.0;
const RAIL_OPEN: f32 = 200.0;
const RAIL_FOLDED: f32 = 60.0;
/// The narrowest the preview pane beside the table goes: its Preview / Details / Hex tabs and the card around them.
const PANE_MIN_WIDTH: f32 = 330.0;
/// How wide (beside the table) or tall (under it) the closed preview pane's strip is.
const FOLDED_PANE: f32 = 32.0;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
	Browse,
	Pak,
	Convert,
	Verify,
	Compare,
	Optimize,
	Help,
	Settings,
	About,
}

impl Section {
	/// The work screens, at the top of the rail.
	const MAIN: [Section; 6] = [
		Section::Browse,
		Section::Pak,
		Section::Convert,
		Section::Verify,
		Section::Compare,
		Section::Optimize,
	];
	/// Everything about the app itself, at the bottom of the rail.
	const FOOTER: [Section; 3] = [Section::Help, Section::Settings, Section::About];

	fn title(self) -> &'static str {
		match self {
			Section::Browse => "Browse",
			Section::Pak => "Pack",
			Section::Convert => "Convert",
			Section::Verify => "Verify",
			Section::Compare => "Compare",
			Section::Optimize => "Optimize",
			Section::Help => "Help",
			Section::Settings => "Settings",
			Section::About => "About",
		}
	}

	fn glyph(self) -> Glyph {
		match self {
			Section::Browse => Glyph::Browse,
			Section::Pak => Glyph::Pack,
			Section::Convert => Glyph::Convert,
			Section::Verify => Glyph::Verify,
			Section::Compare => Glyph::Compare,
			Section::Optimize => Glyph::Optimize,
			Section::Help => Glyph::Help,
			Section::Settings => Glyph::Settings,
			Section::About => Glyph::About,
		}
	}

	fn blurb(self) -> &'static str {
		match self {
			Section::Pak => "Build an archive from a folder. Files you did not change keep their compressed data, so it is quick.",
			Section::Convert => "Turn loose XML and HTML files into readable text, or back into a form the client reads.",
			Section::Verify => "Make sure every file in an archive reads back intact, the way the client reads it.",
			Section::Compare => "Check that two archives, or two client folders, hold the same files.",
			Section::Optimize => {
				"Merge the archives the client scans at start into a few large volumes, so the game starts faster."
			}
			Section::Help => "What each screen does, and the same commands for the terminal.",
			Section::Browse | Section::Settings | Section::About => "",
		}
	}
}

/// What the details pane shows for the selected entry.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DetailTab {
	Preview,
	Metadata,
	Hex,
}

/// A click on a row of the archive table: its position in `items`, and the modifiers held.
#[derive(Clone, Copy)]
struct Pick {
	index: usize,
	ctrl: bool,
	shift: bool,
}

/// One row of the archive table: the way up, a folder, or an entry.
#[derive(Clone)]
enum Item {
	Up,
	Folder(browse::Folder),
	Entry(usize),
}

impl Item {
	fn key(&self) -> Option<RowKey> {
		match self {
			Item::Up => None,
			Item::Folder(folder) => Some(RowKey::Folder(folder.key.clone())),
			Item::Entry(index) => Some(RowKey::Entry(*index)),
		}
	}
}

/// What can be selected in the archive table: a folder, by its key, or an entry, by its index.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum RowKey {
	Folder(String),
	Entry(usize),
}

/// What a row asks for besides being selected.
enum TableAction {
	Open(usize, open::Mode),
	/// Sort by this column; again on the same one turns the order round.
	Sort(Col),
	Enter(String),
	Extract(String),
	Up,
}

/// What the workspace asks for, applied after the views have been drawn.
#[derive(Clone)]
enum WorkspaceAction {
	Nothing,
	Open(PathBuf),
	OpenInFolder(PathBuf),
	CheckCrcs,
	ExtractAll,
	ExtractShown,
	ExtractSelected,
	ExtractTree,
	ExtractPaks,
}

struct Loading {
	path: PathBuf,
	receiver: Receiver<Result<Archive, String>>,
}

/// Which screen's own list of problems a report shows.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Report {
	Verify,
}

/// One line of the log: the local time it happened at, and what happened.
#[derive(Clone)]
struct LogLine {
	at: clock::Stamp,
	error: bool,
	text: String,
	/// Starts a new operation: drawn as a rule with the text on it, so one run's lines are not read as another's.
	rule: bool,
}

impl LogLine {
	/// The line as it goes to the clipboard.
	fn plain(&self) -> String {
		if self.rule {
			format!("── {} {} ──", self.at, self.text)
		} else {
			format!("{} {}{}", self.at, if self.error { "! " } else { "  " }, self.text)
		}
	}
}

/// What the user did to one line of a `wrapped_list`.
enum LineEvent {
	Click(usize, egui::Modifiers),
	/// "Copy" from the line's context menu.
	Copy(usize),
}

const LOG_LIMIT: usize = 20_000;

pub struct Shell {
	/// A move or resize of the window was handed to the system: the button is released while the system holds the
	/// pointer, so the app never hears of it (KDE) and would treat the pointer as still down, with no hover anywhere.
	handed_drag: bool,
	settings: Settings,
	section: Section,
	/// Resolved light/dark of the system, to notice when "System" should re-apply.
	system_dark: bool,
	rail_open: bool,
	details_open: bool,
	archive: Option<Archive>,
	folder: Option<FolderView>,
	open_error: Option<String>,
	loading: Option<Loading>,
	filter: String,
	/// The folder of the open archive being shown: its key, empty for the top, and its names for the breadcrumb.
	folder_key: String,
	folder_names: Vec<String>,
	/// The rows of the archive table: the current folder, or the matches of the filter across the archive.
	items: Vec<Item>,
	/// The column the archive table is sorted by, and whether downwards; `None` keeps the archive's order.
	sort: Option<(Col, bool)>,
	/// A row the arrow keys moved to, scrolled into view on the next frame.
	scroll_to: Option<usize>,
	/// The row the details pane is about, and everything selected.
	focus: Option<RowKey>,
	selection: BTreeSet<RowKey>,
	/// The entry the preview was built for, and the preview itself.
	preview: Option<(usize, preview::Preview)>,
	detail_tab: DetailTab,
	/// The job the user started: one at a time, shown in the status bar.
	job: Option<Job>,
	/// Background reading of the XML forms of the open archive. Separate from `job`, so starting real work
	/// does not stop it and leave the FORM column half empty.
	form_scan: Option<Job>,
	log: Vec<LogLine>,
	/// Lines of the log picked for copying, by index, and where a Shift range starts.
	log_selected: BTreeSet<usize>,
	log_anchor: Option<usize>,
	log_open: bool,
	/// Wrapped heights of the log's lines and of the report's, measured once.
	log_wrap: WrapCache,
	report_wrap: WrapCache,
	/// The problems of the job that is running or just finished.
	job_problems: Vec<LogLine>,
	/// The log was cleared since that job started: its warnings and errors are no longer there to show.
	job_lines_cleared: bool,
	verify_report: Vec<LogLine>,
	compare_view: compare_view::CompareView,
	extract_bar: forms::ExtractBar,
	pak_form: forms::PakForm,
	convert_form: forms::ConvertForm,
	compare_form: forms::CompareForm,
	optimize_form: forms::OptimizeForm,
	verify_form: forms::VerifyForm,
	/// What the last check ran on, so a problem line jumps into Browse only when that is the open archive.
	verified: Option<PathBuf>,
	last_window_save: Instant,
}

impl Shell {
	pub fn new(cc: &eframe::CreationContext<'_>, settings: Settings, path: Option<PathBuf>) -> Self {
		let pane_open = settings.pane_open;
		theme::install(&cc.egui_ctx, &settings.look, settings.density);
		let mut shell = Shell {
			handed_drag: false,
			extract_bar: forms::ExtractBar::new(settings.out.clone()),
			settings,
			section: Section::Browse,
			system_dark: cc.egui_ctx.theme() == egui::Theme::Dark,
			rail_open: true,
			details_open: pane_open,
			archive: None,
			folder: None,
			open_error: None,
			loading: None,
			filter: String::new(),
			folder_key: String::new(),
			folder_names: Vec::new(),
			items: Vec::new(),
			sort: None,
			scroll_to: None,
			focus: None,
			selection: BTreeSet::new(),
			preview: None,
			detail_tab: DetailTab::Preview,
			job: None,
			form_scan: None,
			log: Vec::new(),
			log_selected: BTreeSet::new(),
			log_anchor: None,
			log_open: false,
			log_wrap: WrapCache::default(),
			report_wrap: WrapCache::default(),
			job_problems: Vec::new(),
			job_lines_cleared: false,
			verify_report: Vec::new(),
			compare_view: compare_view::CompareView::default(),
			pak_form: forms::PakForm::default(),
			convert_form: forms::ConvertForm::default(),
			compare_form: forms::CompareForm::default(),
			optimize_form: forms::OptimizeForm::default(),
			verify_form: forms::VerifyForm::default(),
			verified: None,
			// Dated in the past so the first frame records where the window is.
			last_window_save: Instant::now() - Duration::from_secs(5),
		};
		match path {
			Some(path) => shell.open(&cc.egui_ctx, path),
			None if shell.settings.reopen => shell.reopen_last(&cc.egui_ctx),
			None => {}
		}
		#[cfg(debug_assertions)]
		shell.debug_setup(&cc.egui_ctx);
		shell
	}

	/// Debug builds only: start on a given screen, so screenshots of every screen can be taken without input.
	/// `AIONPAK_DEBUG=section[,filter[,row]]`, e.g. `browse,*gray*,3` or `settings`.
	#[cfg(debug_assertions)]
	fn debug_setup(&mut self, ctx: &egui::Context) {
		let Ok(spec) = std::env::var("AIONPAK_DEBUG") else {
			return;
		};
		let mut parts = spec.split(',');
		let wanted = parts.next().unwrap_or_default();
		if let Some(section) = Section::MAIN
			.into_iter()
			.chain(Section::FOOTER)
			.find(|section| section.title().eq_ignore_ascii_case(wanted))
		{
			self.section = section;
		}
		if wanted == "log" {
			self.log_open = true;
		}
		if wanted == "folded" {
			self.rail_open = false;
		}
		// `verify=<folder>`: check a folder and show the log, to see long problem lines.
		if let Some(folder) = wanted.strip_prefix("verify=") {
			self.verify_form.source = Some(PathBuf::from(folder));
			self.section = Section::Verify;
			self.log_open = true;
			self.start_verify();
		}
		let filter = parts.next().unwrap_or_default().to_owned();
		let row = parts.next().and_then(|row| row.parse::<usize>().ok());
		// The archive loads on a thread; wait for it here, it is a debug aid.
		while self.loading.is_some() {
			self.poll();
			std::thread::sleep(Duration::from_millis(20));
		}
		self.filter = filter;
		self.refilter();
		if let Some(index) = row.filter(|row| *row < self.items.len()) {
			self.pick_row(Pick { index, ctrl: false, shift: false });
		}
		ctx.request_repaint();
	}

	/// Opens what was open when the app last closed: the last archive or folder, and the archive inside that
	/// folder if one was open. Nothing when it is gone, so the start screen shows instead.
	fn reopen_last(&mut self, ctx: &egui::Context) {
		let Some(last) = self.settings.recent.first().cloned().filter(|path| path.exists()) else {
			return;
		};
		// Taken first: opening the folder forgets the archive that was open in it.
		let archive = self.settings.last_archive.clone();
		self.open(ctx, last.clone());
		if let (Some(folder), Some(archive)) = (&mut self.folder, archive)
			&& archive.starts_with(&last)
			&& archive.is_file()
		{
			// Show the folder the archive sits in, so going back lands where the user was.
			if let Some(dir) = archive.parent().and_then(|dir| dir.strip_prefix(&last).ok()) {
				folder.enter(dir.to_path_buf());
			}
			self.open_archive(ctx, archive);
		}
	}

	/// Opens an archive or a folder, whichever the path is.
	fn open(&mut self, ctx: &egui::Context, path: PathBuf) {
		self.section = Section::Browse;
		if path.is_dir() {
			self.open_folder(path);
		} else {
			self.open_archive_in_tree(ctx, path);
		}
	}

	/// An archive opened on its own opens inside the folder tree around it (see `folder::tree_root`), so `..` leads
	/// on to its neighbours. The archive, not that folder, is what the recent list keeps.
	fn open_archive_in_tree(&mut self, ctx: &egui::Context, path: PathBuf) {
		self.stop_form_scan();
		self.close_archive();
		self.open_error = None;
		let root = folder::tree_root(&path);
		let mut view = FolderView::open(root.clone());
		if let Some(dir) = path.parent().and_then(|dir| dir.strip_prefix(&root).ok())
			&& !dir.as_os_str().is_empty()
		{
			view.enter(dir.to_path_buf());
		}
		self.folder = Some(view);
		self.remember(&path);
		self.open_archive(ctx, path);
	}

	fn open_archive(&mut self, ctx: &egui::Context, path: PathBuf) {
		self.stop_form_scan();
		let (sender, receiver) = std::sync::mpsc::channel();
		let worker_ctx = ctx.clone();
		let worker_path = path.clone();
		std::thread::spawn(move || {
			let _ = sender.send(browse::load(&worker_path));
			worker_ctx.request_repaint();
		});
		self.loading = Some(Loading { path, receiver });
		self.open_error = None;
	}

	fn open_folder(&mut self, path: PathBuf) {
		self.stop_form_scan();
		self.close_archive();
		self.open_error = None;
		self.log_line(false, format!("opened folder {}", path.display()));
		self.remember(&path);
		self.folder = Some(FolderView::open(path));
	}

	fn close_archive(&mut self) {
		self.stop_form_scan();
		self.archive = None;
		self.clear_selection();
		self.filter.clear();
		self.folder_key.clear();
		self.folder_names.clear();
		self.items.clear();
		if self.settings.last_archive.take().is_some() {
			self.settings.save();
		}
	}

	fn remember(&mut self, path: &Path) {
		self.settings.remember(path);
		self.settings.save();
	}

	fn forget_recent(&mut self, path: &Path) {
		self.settings.recent.retain(|known| known != path);
		self.settings.save();
	}

	fn stop_form_scan(&mut self) {
		if let Some(scan) = self.form_scan.take() {
			scan.progress.cancel();
		}
	}

	fn searching(&self) -> bool {
		!self.filter.trim().is_empty()
	}

	/// Rebuilds the table: the current folder, or with a filter, the matching files anywhere in the archive.
	fn refilter(&mut self) {
		let searching = self.searching();
		let Some(archive) = &self.archive else {
			self.items.clear();
			return;
		};
		self.items = if searching {
			browse::filter(&archive.rows, &self.filter)
				.into_iter()
				.filter(|index| !archive.rows[*index].is_dir)
				.map(Item::Entry)
				.collect()
		} else {
			let mut items = Vec::new();
			if !self.folder_key.is_empty() || self.folder.is_some() {
				items.push(Item::Up);
			}
			let (folders, files) = browse::listing(&archive.rows, &self.folder_key);
			items.extend(folders.into_iter().map(Item::Folder));
			items.extend(files.into_iter().map(Item::Entry));
			items
		};
		if let Some((col, descending)) = self.sort {
			sort_items(&mut self.items, archive, col, descending);
		}
	}

	/// Shows another folder of the open archive.
	fn set_folder(&mut self, key: String) {
		if let Some(archive) = &self.archive {
			self.folder_names = browse::folder_names(&archive.rows, &key);
		}
		self.folder_key = key;
		self.clear_selection();
		self.refilter();
	}

	/// One folder up inside the archive, or back to the client folder from its top.
	fn go_up(&mut self) {
		if self.folder_key.is_empty() {
			if self.folder.is_some() {
				self.close_archive();
			}
			return;
		}
		let parent = self
			.folder_key
			.trim_end_matches('/')
			.rsplit_once('/')
			.map(|(parent, _)| format!("{parent}/"))
			.unwrap_or_default();
		self.set_folder(parent);
	}

	fn clear_selection(&mut self) {
		self.focus = None;
		self.selection.clear();
		self.preview = None;
	}

	fn load_preview(&self, ctx: &egui::Context, index: usize) -> Option<(usize, preview::Preview)> {
		let archive = self.archive.as_ref()?;
		let entry = archive.reader.entries().get(index)?;
		let row = archive.rows.get(index)?;
		Some((index, preview::load(ctx, &archive.reader, entry, &row.name)))
	}

	fn open_entry(&mut self, index: usize, mode: open::Mode) {
		let result = match &self.archive {
			Some(archive) => match (archive.reader.entries().get(index), archive.rows.get(index)) {
				(Some(entry), Some(row)) => open::launch(&archive.reader, entry, &row.name, mode),
				_ => return,
			},
			None => return,
		};
		if let Err(error) = result {
			self.log_line(true, error);
			self.log_open = true;
		}
	}


	fn start_verify(&mut self) {
		let deep = self.verify_form.deep;
		let Some(path) = self.verify_form.source.clone() else {
			return;
		};
		// The open archive is checked from the reader Browse already has, with its short name in the problems,
		// which is what lets a problem line jump to its entry.
		let job = match &self.archive {
			Some(archive) if archive.path == path => {
				jobs::verify_archive(Arc::clone(&archive.reader), archive.file_name(), deep)
			}
			_ => jobs::verify_path(path.clone(), deep),
		};
		self.verified = Some(path);
		self.start_job(job);
	}

	/// What Browse shows, for the screens that work on it: the open archive, else the folder it is in (the
	/// folder being browsed, not only its root.
	fn open_in_browse(&self) -> Option<PathBuf> {
		if let Some(archive) = &self.archive {
			return Some(archive.path.clone());
		}
		// Joined with an empty `dir`, the root would gain a trailing separator.
		let folder = self.folder.as_ref()?;
		let path = if folder.dir.as_os_str().is_empty() { folder.root.clone() } else { folder.root.join(&folder.dir) };
		Some(path)
	}

	fn busy(&self) -> bool {
		self.job.as_ref().is_some_and(|job| !job.finished)
	}

	fn start_job(&mut self, job: Job) {
		if let Some(previous) = &self.job {
			previous.progress.cancel();
		}
		self.job_problems.clear();
		self.job_lines_cleared = false;
		self.log_push(LogLine {
			at: clock::Stamp::now(),
			error: false,
			text: format!("started: {}", job.title),
			rule: true,
		});
		self.job = Some(job);
	}

	fn log_line(&mut self, error: bool, text: impl Into<String>) -> LogLine {
		let line = LogLine {
			at: clock::Stamp::now(),
			error,
			text: text.into(),
			rule: false,
		};
		self.log_push(line.clone());
		line
	}

	fn log_push(&mut self, line: LogLine) {
		self.log.push(line);
		if self.log.len() > LOG_LIMIT {
			let excess = self.log.len() - LOG_LIMIT;
			self.log.drain(..excess);
			// The picked lines move up with the rest; those dropped are gone.
			self.log_selected = self.log_selected.iter().filter_map(|i| i.checked_sub(excess)).collect();
			self.log_anchor = self.log_anchor.and_then(|i| i.checked_sub(excess));
		}
	}

	fn copy_log(&self, ctx: &egui::Context, lines: impl Iterator<Item = usize>) {
		let text: Vec<String> = lines.filter_map(|i| self.log.get(i)).map(LogLine::plain).collect();
		ctx.copy_text(text.join("\n"));
	}

	/// A click on a log line: picks it, adds it with Ctrl, or picks the range to it with Shift; a plain click on the
	/// only picked line drops it.
	fn pick_log_line(&mut self, index: usize, modifiers: egui::Modifiers) {
		if modifiers.shift
			&& let Some(anchor) = self.log_anchor
		{
			let (low, high) = if anchor <= index { (anchor, index) } else { (index, anchor) };
			self.log_selected = (low..=high).collect();
			return;
		}
		if modifiers.command {
			if !self.log_selected.remove(&index) {
				self.log_selected.insert(index);
			}
		} else if self.log_selected.len() == 1 && self.log_selected.contains(&index) {
			self.log_selected.clear();
		} else {
			self.log_selected = BTreeSet::from([index]);
		}
		self.log_anchor = Some(index);
	}

	/// Remembers where the window was, at most once a second, and never while it is maximised.
	fn remember_window(&mut self, ctx: &egui::Context) {
		if self.last_window_save.elapsed() < Duration::from_secs(1) {
			return;
		}
		let (maximized, inner, outer) = ctx.input(|i| {
			let viewport = i.viewport();
			(viewport.maximized.unwrap_or(false), viewport.inner_rect, viewport.outer_rect)
		});
		if maximized {
			return;
		}
		let Some(rect) = inner.or(outer) else {
			return;
		};
		let current = (rect.width(), rect.height(), rect.min.x, rect.min.y);
		if Some(current) == self.settings.window {
			return;
		}
		self.settings.window = Some(current);
		self.last_window_save = Instant::now();
		self.settings.save();
	}

	fn poll(&mut self) {
		let mut loaded = None;
		if let Some(loading) = &self.loading {
			match loading.receiver.try_recv() {
				Ok(result) => loaded = Some(result),
				Err(TryRecvError::Empty) => {}
				Err(TryRecvError::Disconnected) => {
					loaded = Some(Err("the loader stopped before reading the archive".to_owned()));
				}
			}
		}
		if let Some(result) = loaded {
			let path = self.loading.take().map(|loading| loading.path);
			match result {
				Ok(archive) => {
					self.close_archive();
					self.form_scan = Some(jobs::scan_forms(
						Arc::clone(&archive.reader),
						archive.xml_entries.clone(),
						Arc::clone(&archive.forms),
					));
					let opened = archive.path.clone();
					self.archive = Some(archive);
					self.refilter();
					self.log_line(false, format!("opened {}", opened.display()));
					// An archive opened from a folder belongs to that folder; one opened on its own goes to the list.
					if self.folder.is_none() {
						self.remember(&opened);
					} else {
						self.settings.last_archive = Some(opened);
						self.settings.save();
					}
				}
				Err(error) => {
					if let Some(path) = path {
						// A recent entry that no longer opens is not worth offering again.
						self.settings.recent.retain(|known| *known != path);
						self.settings.save();
					}
					self.open_error = Some(error);
				}
			}
		}

		// The form scan only reports errors worth reading; its count means nothing to the user.
		if let Some(scan) = &self.form_scan
			&& let Ok(JobEvent::Done(_)) = scan.events.try_recv()
		{
			let errors: Vec<String> = scan
				.progress
				.problems_from(0)
				.into_iter()
				.filter(|problem| problem.error)
				.map(|problem| problem.what)
				.collect();
			self.form_scan = None;
			for error in errors {
				self.log_line(true, error);
			}
		}

		let mut events = Vec::new();
		let mut problems = Vec::new();
		if let Some(job) = &self.job {
			while let Ok(event) = job.events.try_recv() {
				events.push(event);
			}
			// Problems live in the job's progress; take the new ones once, into the log and the job's own list.
			problems = job.progress.problems_from(self.job_problems.len());
		}
		let base = self.job.as_ref().and_then(|job| job.base.clone());
		for problem in problems {
			let line = self.log_line(problem.error, relative(&problem.what, base.as_deref()));
			self.job_problems.push(line);
		}
		for JobEvent::Done(result) in events {
			let Some(job) = self.job.as_mut() else {
				continue;
			};
			job.finish(result);
			let (summary, failed, kind) = (job.summary.clone(), job.failed, job.kind);
			self.log_line(failed, if failed { format!("failed: {summary}") } else { summary });
			match kind {
				JobKind::Verify => self.verify_report = std::mem::take(&mut self.job_problems),
				JobKind::Compare => self.compare_view.take_result(),
				_ => {}
			}
		}
	}

	fn accept_drops(&mut self, ctx: &egui::Context) {
		let dropped: Vec<PathBuf> = ctx.input(|i| {
			i.raw
				.dropped_files
				.iter()
				.map(|file| file.path().to_path_buf())
				.collect()
		});
		// A folder or a .pak opens; anything else dropped is ignored rather than reported as a broken archive.
		if let Some(path) = dropped
			.into_iter()
			.find(|path| path.is_dir() || path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("pak")))
		{
			self.open(ctx, path);
		}
	}

	fn shortcuts(&mut self, ctx: &egui::Context) {
		let typing = ctx.memory(|m| m.focused().is_some());
		let (open_archive, open_folder, find, escape) = ctx.input_mut(|i| {
			(
				i.consume_key(Modifiers::COMMAND, Key::O),
				i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::O),
				i.consume_key(Modifiers::COMMAND, Key::F),
				!typing && i.key_pressed(Key::Escape),
			)
		});
		if open_archive {
			self.pick_archive(ctx);
		}
		if open_folder {
			self.pick_folder(ctx);
		}
		if find {
			self.section = Section::Browse;
			ctx.memory_mut(|m| m.request_focus(filter_id()));
		}
		if escape {
			self.clear_selection();
		}
	}

	fn pick_archive(&mut self, ctx: &egui::Context) {
		if let Some(path) = rfd::FileDialog::new()
			.add_filter("Aion archive", &["pak"])
			.set_title("Open an archive")
			.pick_file()
		{
			self.open(ctx, path);
		}
	}

	fn pick_folder(&mut self, ctx: &egui::Context) {
		if let Some(path) = rfd::FileDialog::new()
			.set_title("Open a client folder")
			.pick_folder()
		{
			self.open(ctx, path);
		}
	}

	/// Our own title bar: the window has no decorations, so dragging, double-click and the three buttons are ours.
	/// The mark in the title bar breathes while a job has run for more than a second: slow, one breath in 2.4 s,
	/// fading in and out with the job. Quick jobs end before it starts, so it never flickers.
	fn working_glow(&self, ctx: &egui::Context) -> f32 {
		let running = self.job.as_ref().filter(|job| !job.finished);
		let long = running.is_some_and(|job| job.started.elapsed() > Duration::from_secs(1));
		let presence = ctx.animate_bool_with_time(egui::Id::unique("working-glow"), long, 0.5);
		if presence == 0.0 {
			return 0.0;
		}
		// The phase counts from the job's start, so each run begins dark and brightens; once the job is over the
		// phase stops where it was and the glow fades with `presence`.
		let t = self.job.as_ref().map_or(0.0, |job| {
			let end = job.finished_at.unwrap_or_else(Instant::now);
			end.duration_since(job.started).as_secs_f32() - 1.0
		});
		let breath = 0.5 - 0.5 * (t * std::f32::consts::TAU / 2.4).cos();
		ctx.request_repaint_after(Duration::from_millis(33));
		presence * (0.25 + 0.75 * breath)
	}

	fn title_bar(&mut self, ui: &mut egui::Ui) {
		let pal = theme::pal(ui.ctx());
		let (rect, drag) = ui.allocate_exact_size(Vec2::new(ui.available_width(), TITLE_HEIGHT), Sense::click_and_drag());
		let ctx = ui.ctx().clone();
		let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));

		if drag.drag_started() {
			ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
			self.handed_drag = true;
		}
		if drag.double_clicked() {
			ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
		}

		// No line under the bar: the content sheet's own border (with its rounded corner) separates the two, and a
		// second line along it would show as a double border at the corner.
		ui.painter().rect_filled(rect, 0.0, pal.bg);

		let buttons_width = 46.0 * 3.0;
		let left = Rect::from_min_max(
			rect.min + Vec2::new(14.0, 0.0),
			egui::pos2(rect.right() - buttons_width - 8.0, rect.bottom()),
		);
		ui.scope_builder(
			egui::UiBuilder::new().max_rect(left).layout(Layout::left_to_right(Align::Center)),
			|ui| {
				// The bar is a drag handle, so its text must not start a selection instead.
				ui.style_mut().interaction.selectable_labels = false;
				let hint = if self.rail_open { "Fold the sidebar" } else { "Unfold the sidebar" };
				if menu_button(ui, &pal).on_hover_text(hint).clicked() {
					self.rail_open = !self.rail_open;
				}
				ui.add_space(8.0);
				let glow = self.working_glow(ui.ctx());
				theme::mark_lit(ui, 22.0, glow);
				ui.add_space(6.0);
				ui.label(theme::display(&pal, "aionpak"));
				ui.add_space(16.0);
				// What is open belongs to Browse; the other screens show what they work on in their own fields, and
				// a path up here would name something those screens are not about.
				let text = if self.section != Section::Browse {
					String::new()
				} else if let Some(archive) = &self.archive {
					archive.path.display().to_string()
				} else if let Some(loading) = &self.loading {
					format!("opening {}…", loading.path.display())
				} else if let Some(folder) = &self.folder {
					folder.root.display().to_string()
				} else {
					String::new()
				};
				if !text.is_empty() {
					ui.add(Label::new(theme::mono(text).color(pal.muted())).truncate());
				}
			},
		);

		let right = Rect::from_min_max(egui::pos2(rect.right() - buttons_width, rect.top()), rect.max);
		ui.scope_builder(
			egui::UiBuilder::new().max_rect(right).layout(Layout::right_to_left(Align::Center)),
			|ui| {
				if window_button(ui, &pal, WindowButton::Close, maximized).clicked() {
					ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
				}
				if window_button(ui, &pal, WindowButton::Maximize, maximized).clicked() {
					ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
				}
				if window_button(ui, &pal, WindowButton::Minimize, maximized).clicked() {
					ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true));
				}
			},
		);
	}

	fn rail(&mut self, ui: &mut egui::Ui) {
		let pal = theme::pal(ui.ctx());
		let open = ui
			.ctx()
			.animate_bool_with_time(egui::Id::unique("rail"), self.rail_open, RAIL_SLIDE);
		let width = RAIL_FOLDED + (RAIL_OPEN - RAIL_FOLDED) * open;
		let frame = Frame::new()
			.fill(pal.bg)
			.inner_margin(Margin::symmetric(10, 12))
			.stroke(Stroke::NONE);
		egui::Panel::left("rail")
			.exact_size(width)
			.resizable(false)
			.show_separator_line(false)
			.frame(frame)
			.show(ui, |ui| {
				ui.set_clip_rect(ui.max_rect());
				ui.spacing_mut().item_spacing.y = 2.0;
				let mut chosen = None;
				for section in Section::MAIN {
					if rail_item(ui, section, self.section == section, open).clicked() {
						chosen = Some(section);
					}
				}
				ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
					for section in Section::FOOTER.iter().rev() {
						if rail_item(ui, *section, self.section == *section, open).clicked() {
							chosen = Some(*section);
						}
					}
				});
				if let Some(section) = chosen {
					self.section = section;
				}
			});
	}

	fn status(&mut self, ui: &mut egui::Ui) {
		let pal = theme::pal(ui.ctx());
		let frame = Frame::new()
			.fill(pal.bg)
			.stroke(Stroke::NONE)
			.inner_margin(Margin::symmetric(14, 0));
		let mut open_log = false;
		egui::Panel::bottom("status")
			.exact_size(STATUS_HEIGHT)
			.show_separator_line(false)
			.frame(frame)
			.show(ui, |ui| {
				// No line over the bar, as under the title bar: the sheet's border is the edge.
				ui.horizontal_centered(|ui| {
					let log_width = 130.0;
					let width = (ui.available_width() - log_width).max(220.0);
					ui.allocate_ui_with_layout(
						Vec2::new(width, STATUS_HEIGHT),
						Layout::left_to_right(Align::Center),
						|ui| {
							ui.set_clip_rect(ui.max_rect());
							match &mut self.job {
								Some(job) if !job.finished => running_job(ui, job),
								Some(job) => finished_job(ui, job, self.job_lines_cleared, &mut open_log),
								None => idle(ui, &pal, self.form_scan.is_some()),
							}
						},
					);
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						let errors = self.log.iter().filter(|line| line.error).count();
						let hint = if self.log_open { "Hide the log" } else { "Everything that happened, with the time it happened at" };
						if log_button(ui, &pal, self.log_open, errors).on_hover_text(hint).clicked() {
							self.log_open = !self.log_open;
						}
					});
				});
			});
		if open_log {
			self.log_open = true;
		}
	}

	fn workspace_ui(&mut self, ui: &mut egui::Ui) {
		let pal = theme::pal(ui.ctx());
		let mut action = WorkspaceAction::Nothing;
		let has_content = self.archive.is_some() || self.folder.is_some();

		if has_content {
			self.workspace_toolbar(ui, &pal, &mut action);
			ui.add_space(8.0);
		}

		if let Some(error) = self.open_error.clone() {
			let mut dismiss = false;
			theme::card(&pal)
				.fill(pal.surface.lerp_to_gamma(pal.error, 0.08))
				.stroke(Stroke::new(1.0, pal.error.gamma_multiply(0.5)))
				.show(ui, |ui| {
					ui.horizontal(|ui| {
						ui.label(RichText::new("Could not open").size(13.0).strong().color(pal.error));
						ui.add(Label::new(RichText::new(&error).size(13.0).color(pal.text)).wrap());
						ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
							dismiss = close_button(ui, &pal).clicked();
						});
					});
				});
			if dismiss {
				self.open_error = None;
			}
			ui.add_space(8.0);
		}

		if self.archive.is_some() {
			self.archive_view(ui, &pal);
		} else if self.folder.is_some() {
			self.folder_view(ui, &pal, &mut action);
		} else {
			self.empty_state(ui, &pal, &mut action);
		}

		self.apply_workspace(ui.ctx(), action);
	}

	fn workspace_toolbar(&mut self, ui: &mut egui::Ui, pal: &Palette, action: &mut WorkspaceAction) {
		// One row that wraps: on a narrow window the actions drop to a second line instead of overlapping the
		// filter, and on a wide one they sit next to it, so the table starts as high as possible.
		ui.horizontal_wrapped(|ui| {
			self.open_menu(ui, pal, action);
			let filter = match (&self.archive, &mut self.folder) {
				(None, Some(folder)) => &mut folder.filter,
				_ => &mut self.filter,
			};
			let edit = egui::TextEdit::singleline(filter)
				.id(filter_id())
				.hint_text("Filter   *.xml, mission_*, npc")
				.desired_width(260.0)
				.margin(Margin::symmetric(10, 5));
			if ui
				.add(edit)
				.on_hover_text(
					"Show only the names that match. Plain text finds names that contain it; * and ? match any \
					 characters or one character of the file name. Ctrl+F jumps here.",
				)
				.changed() && self.archive.is_some()
			{
				self.refilter();
			}

			let busy = self.busy();
			if self.archive.is_some() {
				if ui
					.add_enabled(!busy, egui::Button::new("Check"))
					.on_hover_text(
						"Decompress every file and compare its checksum with the one the archive records. The result \
						 opens in Verify.",
					)
					.clicked()
				{
					*action = WorkspaceAction::CheckCrcs;
				}
				let searching = self.searching();
				let shown = self.items.len();
				let label = if !self.selection.is_empty() {
					format!("Extract {} selected", self.selection.len())
				} else if searching {
					format!("Extract {shown} shown")
				} else {
					"Extract all".to_owned()
				};
				let nothing = searching && shown == 0 && self.selection.is_empty();
				if theme::add_primary(ui, !busy && !nothing, theme::primary(pal, &label))
					.on_hover_text("Selected folders are extracted with everything in them")
					.on_disabled_hover_text(if busy { "Another job is running" } else { "The filter shows nothing" })
					.clicked()
				{
					*action = if !self.selection.is_empty() {
						WorkspaceAction::ExtractSelected
					} else if searching {
						WorkspaceAction::ExtractShown
					} else {
						WorkspaceAction::ExtractAll
					};
				}
			} else if let Some(folder) = &self.folder {
				let selected = folder.selection.len();
				let label = if selected == 0 {
					"Extract every archive".to_owned()
				} else {
					format!("Extract {selected} archives")
				};
				let nothing = selected == 0 && !folder.paks_below;
				if theme::add_primary(ui, !busy && !nothing, theme::primary(pal, &label))
					.on_hover_text(
						"Every archive in this folder and below is unpacked into a <name>.pak/ folder at its place in \
						 the tree",
					)
					.on_disabled_hover_text(if busy {
						"Another job is running"
					} else {
						"No archives in this folder or below"
					})
					.clicked()
				{
					*action = if selected == 0 {
						WorkspaceAction::ExtractTree
					} else {
						WorkspaceAction::ExtractPaks
					};
				}
			}
		});
		ui.add_space(2.0);
		self.extract_bar.ui(ui, pal);
	}

	/// The Open button: its menu has the recent archives and folders, then the two pickers.
	fn open_menu(&mut self, ui: &mut egui::Ui, pal: &Palette, action: &mut WorkspaceAction) {
		let button = ui
			.add(egui::Button::new(RichText::new("Open      ")).min_size(Vec2::new(0.0, 0.0)))
			.on_hover_text("Recent places, or pick an archive (Ctrl+O) or a folder (Ctrl+Shift+O)");
		// A chevron painted at the button's right end, so it does not depend on a font having the glyph.
		let c = egui::pos2(button.rect.right() - 16.0, button.rect.center().y);
		ui.painter().add(egui::Shape::line(
			vec![c + Vec2::new(-4.0, -2.0), c + Vec2::new(0.0, 2.0), c + Vec2::new(4.0, -2.0)],
			Stroke::new(1.5, pal.text_dim),
		));
		let recent: Vec<PathBuf> = self.settings.recent.iter().take(5).cloned().collect();
		let mut pick = None;
		let mut forget = None;
		// Closed by its entries themselves, so a × can remove several in a row without closing it.
		egui::Popup::menu(&button)
			.close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
			.show(|ui| {
				ui.set_min_width(320.0);
				if !recent.is_empty() {
					ui.label(theme::caption(pal, "RECENT"));
					let texts: Vec<String> = recent
						.iter()
						.zip(distinct_parents(&recent))
						.map(|(path, distinct)| {
							let name = path
								.file_name()
								.map(|name| name.to_string_lossy().into_owned())
								.unwrap_or_else(|| path.display().to_string());
							let kind = if path.is_dir() { "folder" } else { "archive" };
							match distinct {
								Some(tail) => format!("{name}  ·  {tail}  ·  {kind}"),
								None => format!("{name}  ·  {kind}"),
							}
						})
						.collect();
					// One width for all entries, so the × line up in a column.
					let font = egui::TextStyle::Button.resolve(ui.style());
					// Padding on both sides, and the gap egui leaves between the text and the space after it.
					let padding = ui.spacing().button_padding.x * 2.0 + ui.spacing().icon_spacing + 8.0;
					let entry_width = texts
						.iter()
						.map(|text| ui.fonts_mut(|f| f.layout_no_wrap(text.clone(), font.clone(), pal.text).size().x) + padding)
						.fold(MENU_ENTRY_WIDTH, f32::max);
					for (path, text) in recent.iter().zip(texts) {
						ui.horizontal(|ui| {
							ui.spacing_mut().item_spacing.x = 4.0;
							// A fixed width (a menu's room is unbounded) and space growing after the text keep the
							// entries left-aligned and the × in one column.
							let response = ui
								.add_enabled(
									path.exists(),
									egui::Button::new((egui::WidgetText::from(text), egui::Atom::grow()))
										.frame_when_inactive(false)
										.min_size(Vec2::new(entry_width, 0.0)),
								)
								.on_hover_text(path.display().to_string())
								.on_disabled_hover_text("No longer there");
							if response.clicked() {
								*action = WorkspaceAction::Open(path.clone());
								ui.close();
							}
							// The menu stays open, so several entries can go in a row.
							if close_button(ui, pal).on_hover_text("Remove from the list").clicked() {
								forget = Some(path.clone());
							}
						});
					}
					ui.separator();
				}
				if ui.button("Open archive…").clicked() {
					pick = Some(false);
					ui.close();
				}
				if ui.button("Open folder…").clicked() {
					pick = Some(true);
					ui.close();
				}
			});
		if let Some(path) = forget {
			self.forget_recent(&path);
		}
		match pick {
			Some(false) => self.pick_archive(ui.ctx()),
			Some(true) => self.pick_folder(ui.ctx()),
			None => {}
		}
	}

	fn archive_view(&mut self, ui: &mut egui::Ui, pal: &Palette) {
		let typing = ui.ctx().memory(|m| m.focused().is_some());
		let (select_all, up, enter, step) = ui.ctx().input(|i| {
			let step = if typing {
				0
			} else {
				i.num_presses(Key::ArrowDown) as i32 - i.num_presses(Key::ArrowUp) as i32
			};
			(
				!typing && i.modifiers.command && i.key_pressed(Key::A),
				!typing && i.key_pressed(Key::Backspace),
				!typing && i.key_pressed(Key::Enter),
				step,
			)
		});
		if select_all {
			self.selection = self.items.iter().filter_map(Item::key).collect();
		}
		if step != 0 {
			self.step_focus(step);
		}
		if up && !self.searching() {
			self.go_up();
		}
		if enter {
			match self.focus.clone() {
				Some(RowKey::Folder(key)) => self.set_folder(key),
				Some(RowKey::Entry(index)) => {
					if let Some(archive) = &self.archive {
						let mode = preferred_mode(archive, index);
						self.open_entry(index, mode);
					}
				}
				None => {}
			}
		}
		let Some(archive) = self.archive.as_ref() else {
			return;
		};
		let searching = self.searching();
		let shown = searching.then_some(self.items.len());
		archive_strip(ui, archive, shown, self.form_scan.is_some());
		ui.add_space(2.0);
		// Opened from a client folder, the path starts there: the folder's crumbs lead back to it.
		let outside: Vec<(String, PathBuf)> = self.folder.as_ref().map_or_else(Vec::new, |folder| {
			let mut crumbs = vec![(
				folder.root.file_name().map_or_else(|| folder.root.display().to_string(), |n| n.to_string_lossy().into_owned()),
				PathBuf::new(),
			)];
			let inside = archive.path.parent().and_then(|p| p.strip_prefix(&folder.root).ok());
			let mut prefix = PathBuf::new();
			for part in inside.into_iter().flat_map(|p| p.iter()) {
				prefix.push(part);
				crumbs.push((part.to_string_lossy().into_owned(), prefix.clone()));
			}
			crumbs
		});
		let mut back_to = None;
		let jump = breadcrumb(ui, pal, archive, &self.folder_names, searching, &outside, &mut back_to);
		if let Some(dir) = back_to {
			self.close_archive();
			if let Some(folder) = &mut self.folder {
				folder.enter(dir);
			}
			return;
		}
		ui.add_space(4.0);

		let focused_entry = match &self.focus {
			Some(RowKey::Entry(index)) => Some(*index),
			_ => None,
		};
		if let Some(index) = focused_entry {
			if self.preview.as_ref().is_none_or(|(built, _)| *built != index) {
				self.preview = self.load_preview(ui.ctx(), index);
			}
		} else {
			self.preview = None;
		}
		let archive = self.archive.as_ref().expect("checked above");

		let mut pick = None;
		let mut table_action = None;
		let mut close_details = false;
		let mut detail_tab = self.detail_tab;
		// Open unless closed, whatever is picked: a pane that comes and goes with the selection pulls the table
		// about under the pointer.
		let has_details = self.details_open;

		let full = ui.available_rect_before_wrap();
		let inspector = theme::density(ui.ctx()).inspector_width();
		let side = ui.ctx().animate_bool_with_time(
			egui::Id::unique("details-side"),
			has_details && full.width() >= inspector + 460.0,
			0.18,
		);
		// Too narrow for the pane beside the table, it goes under it, however short the window: a pane that never
		// shows is a feature nobody finds. A short window gives the table a little more than half.
		let below = !(side > 0.01) && has_details;
		// The gap between table and pane is a handle: dragged, it sets the pane's width beside the table or its
		// height under it, remembered between runs.
		let (table_rect, details_rect, gap) = if side > 0.01 {
			let wanted = self.settings.pane_width.unwrap_or(inspector);
			let width = wanted.clamp(PANE_MIN_WIDTH, (full.width() - 360.0).max(PANE_MIN_WIDTH)) * side;
			let gap = Rect::from_min_max(
				egui::pos2(full.max.x - width - 12.0 * side, full.min.y),
				egui::pos2(full.max.x - width, full.max.y),
			);
			(
				Rect::from_min_max(full.min, egui::pos2(gap.min.x, full.max.y)),
				Some(Rect::from_min_max(egui::pos2(gap.max.x, full.min.y), full.max)),
				Some((gap, true)),
			)
		} else if below {
			let wanted = self.settings.pane_height.unwrap_or_else(|| (full.height() * 0.45).min(200.0));
			let pane = wanted.clamp(90.0, (full.height() - 130.0).max(90.0));
			let height = full.height() - pane - 10.0;
			let gap = Rect::from_min_max(
				egui::pos2(full.min.x, full.min.y + height),
				egui::pos2(full.max.x, full.min.y + height + 10.0),
			);
			(
				Rect::from_min_max(full.min, egui::pos2(full.max.x, gap.min.y)),
				Some(Rect::from_min_max(egui::pos2(full.min.x, gap.max.y), full.max)),
				Some((gap, false)),
			)
		} else {
			(full, None, None)
		};
		// Closed, the pane leaves a strip where it was: the way back is where it went. A toolbar button for it was
		// not found by testers.
		let (table_rect, folded) = if !has_details && side < 0.01 {
			if full.width() >= inspector + 460.0 {
				let strip = Rect::from_min_max(egui::pos2(full.max.x - FOLDED_PANE, full.min.y), full.max);
				(Rect::from_min_max(full.min, egui::pos2(strip.min.x - 10.0, full.max.y)), Some((strip, true)))
			} else {
				let strip = Rect::from_min_max(egui::pos2(full.min.x, full.max.y - FOLDED_PANE), full.max);
				(Rect::from_min_max(full.min, egui::pos2(full.max.x, strip.min.y - 10.0)), Some((strip, false)))
			}
		} else {
			(table_rect, None)
		};
		if let Some((strip, beside)) = folded {
			if folded_pane(ui, pal, strip, beside).clicked() {
				self.details_open = true;
				self.settings.pane_open = true;
				self.settings.save();
			}
		}
		if let Some((gap, beside)) = gap {
			let response = ui
				.interact(gap, egui::Id::unique("details-gap"), Sense::drag())
				.on_hover_cursor(if beside { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical })
				.on_hover_text("Drag to resize the pane");
			if response.hovered() || response.dragged() {
				let line = if beside {
					[egui::pos2(gap.center().x, gap.top() + 8.0), egui::pos2(gap.center().x, gap.bottom() - 8.0)]
				} else {
					[egui::pos2(gap.left() + 8.0, gap.center().y), egui::pos2(gap.right() - 8.0, gap.center().y)]
				};
				ui.painter().line_segment(line, Stroke::new(2.0, pal.accent.gamma_multiply(0.6)));
			}
			if response.dragged() {
				let delta = response.drag_delta();
				let pane = details_rect.map_or(0.0, |r| if beside { r.width() } else { r.height() });
				if beside {
					self.settings.pane_width = Some(pane - delta.x);
				} else {
					self.settings.pane_height = Some(pane - delta.y);
				}
			}
			if response.drag_stopped() {
				self.settings.save();
			}
		}

		let scroll_to = self.scroll_to.take();
		// Table and pane are laid out in children that do not count towards the sheet's size: whatever they hold
		// that will not fit is cut off at their edge. Counted, it grew the sheet past the window, where it hid the
		// sheet's border and sat over the window's resize edges.
		{
			let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(table_rect));
			ui.set_clip_rect(table_rect.intersect(ui.clip_rect()));
			theme::card(pal)
				.stroke(Stroke::NONE)
				.inner_margin(Margin::same(6))
				.show(ui, |ui| {
					let view = TableView {
						archive,
						items: &self.items,
						selection: &self.selection,
						searching,
						sort: self.sort,
						scroll_to,
					};
					table(ui, &view, &mut pick, &mut table_action);
					// The card fills its place even when the rows end early; otherwise its fill stops under the last
					// row and the outline drawn below encloses an empty band that reads as a blank row. Down to the bottom
					// of its place and no further: set_min_height counts the height from below the last row, and the card,
					// overshooting the window, pushed the sheet under the status bar and over the bottom resize edge.
					ui.expand_to_include_y(ui.max_rect().bottom());
				});
		}
		// The table paints over its card's border; draw the outline again on top.
		ui.painter().rect_stroke(
			table_rect,
			CornerRadius::same(theme::CARD_RADIUS),
			Stroke::new(1.0, pal.border),
			StrokeKind::Inside,
		);
		if let Some(rect) = details_rect {
			{
				let ui = &mut ui.new_child(egui::UiBuilder::new().max_rect(rect));
				ui.set_clip_rect(rect.intersect(ui.clip_rect()));
				theme::card(pal).show(ui, |ui| {
					egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
						match (focused_entry, self.selection.len()) {
							(Some(index), 0 | 1) => {
								let preview = self.preview.as_ref().map(|(_, preview)| preview);
								let columns = if side > 0.01 { 2 } else { 4 };
								details(ui, archive, index, columns, preview, &mut close_details, &mut detail_tab);
							}
							(_, 0) => overview(ui, archive, &self.folder_key, &self.items, pal, &mut close_details),
							_ => multi_details(ui, archive, &self.selection, pal, &mut close_details),
						}
					});
				});
			}
		}
		ui.advance_cursor_after_rect(full);

		self.detail_tab = detail_tab;
		if close_details {
			self.details_open = false;
			self.settings.pane_open = false;
			self.settings.save();
		}
		if let Some(key) = jump {
			self.filter.clear();
			self.set_folder(key);
			return;
		}
		match table_action {
			Some(TableAction::Open(index, mode)) => self.open_entry(index, mode),
			Some(TableAction::Enter(key)) => {
				self.set_folder(key);
				return;
			}
			Some(TableAction::Up) => {
				self.go_up();
				return;
			}
			Some(TableAction::Sort(col)) => {
				// Name and dates start upwards, sizes downwards: the largest file is what one looks for. The second
				// click turns the order round, the third goes back to the archive's own order.
				let first_descending = !matches!(col, Col::Name | Col::Form | Col::Date | Col::Crc);
				self.sort = match self.sort {
					Some((sorted, descending)) if sorted == col && descending == first_descending => {
						Some((col, !descending))
					}
					Some((sorted, _)) if sorted == col => None,
					_ => Some((col, first_descending)),
				};
				self.refilter();
			}
			Some(TableAction::Extract(key)) => {
				if !self.busy() {
					let names = self.names_of(&[RowKey::Folder(key)]);
					self.extract_archive(Some(names));
				}
			}
			None => {}
		}
		if let Some(pick) = pick {
			self.pick_row(pick);
		}
	}

	fn folder_view(&mut self, ui: &mut egui::Ui, pal: &Palette, action: &mut WorkspaceAction) {
		let Some(folder) = &mut self.folder else {
			return;
		};
		if ui.ctx().memory(|m| m.focused().is_none())
			&& ui.ctx().input(|i| i.modifiers.command && i.key_pressed(Key::A))
		{
			folder.select_all_shown();
		}
		if let Some(dir) = folder.breadcrumb(ui, pal) {
			folder.enter(dir);
		}
		ui.add_space(4.0);
		theme::card(pal).inner_margin(Margin::same(6)).show(ui, |ui| {
			match folder.ui(ui, pal) {
				FolderAction::None => {}
				FolderAction::Enter(dir) => folder.enter(dir),
				FolderAction::Open(path) => *action = WorkspaceAction::OpenInFolder(path),
				FolderAction::Pick { path, ctrl, shift } => folder.select(path, ctrl, shift),
			}
		});
	}

	fn pick_row(&mut self, pick: Pick) {
		let Pick { index, ctrl, shift } = pick;
		let Some(key) = self.items.get(index).and_then(Item::key) else {
			return;
		};
		let anchor = self
			.focus
			.as_ref()
			.and_then(|focus| self.items.iter().position(|item| item.key().as_ref() == Some(focus)));
		if shift && let Some(from) = anchor {
			let (low, high) = if from <= index { (from, index) } else { (index, from) };
			self.selection = self.items[low..=high].iter().filter_map(Item::key).collect();
		} else if ctrl {
			if !self.selection.remove(&key) {
				self.selection.insert(key.clone());
			}
		} else {
			self.selection.clear();
			self.selection.insert(key.clone());
		}
		self.focus = Some(key);
	}

	/// Arrow keys: the next or previous row with something to pick, the way up skipped, kept in view.
	fn step_focus(&mut self, step: i32) {
		let keyed: Vec<usize> = (0..self.items.len()).filter(|&i| self.items[i].key().is_some()).collect();
		if keyed.is_empty() {
			return;
		}
		let current = self
			.focus
			.as_ref()
			.and_then(|focus| keyed.iter().position(|&i| self.items[i].key().as_ref() == Some(focus)));
		let next = match current {
			Some(at) => (at as i32 + step).clamp(0, keyed.len() as i32 - 1) as usize,
			None if step > 0 => 0,
			None => keyed.len() - 1,
		};
		let index = keyed[next];
		self.pick_row(Pick {
			index,
			ctrl: false,
			shift: false,
		});
		self.scroll_to = Some(index);
	}

	fn apply_workspace(&mut self, ctx: &egui::Context, action: WorkspaceAction) {
		match action {
			WorkspaceAction::Nothing => {}
			WorkspaceAction::Open(path) => self.open(ctx, path),
			WorkspaceAction::OpenInFolder(path) => {
				if let Some(folder) = &self.folder {
					let full = folder.root.join(path);
					self.open_archive(ctx, full);
				}
			}
			WorkspaceAction::CheckCrcs => {
				// The button belongs to the open archive, whatever Verify was pointed at last.
				if let Some(archive) = &self.archive {
					self.verify_form.point_at(archive.path.clone());
				}
				self.verify_form.deep = true;
				self.section = Section::Verify;
				self.start_verify();
			}
			WorkspaceAction::ExtractAll => self.extract_archive(None),
			WorkspaceAction::ExtractShown => {
				let keys: Vec<RowKey> = self.items.iter().filter_map(Item::key).collect();
				let names = self.names_of(&keys);
				self.extract_archive(Some(names));
			}
			WorkspaceAction::ExtractSelected => {
				let keys: Vec<RowKey> = self.selection.iter().cloned().collect();
				let names = self.names_of(&keys);
				self.extract_archive(Some(names));
			}
			WorkspaceAction::ExtractTree => self.extract_folder(None),
			WorkspaceAction::ExtractPaks => {
				let paks = self.folder.as_ref().map(FolderView::selected_paks).unwrap_or_default();
				self.extract_folder(Some(paks));
			}
		}
	}

	/// The entry names behind selected rows: a folder stands for everything in it.
	fn names_of(&self, keys: &[RowKey]) -> HashSet<String> {
		let Some(archive) = &self.archive else {
			return HashSet::new();
		};
		expand(archive, keys.iter()).map(|index| archive.rows[index].name.clone()).collect()
	}

	/// The options of an extraction, asking for a destination if there is none yet.
	fn extract_request(&mut self, filter: Option<HashSet<String>>) -> Option<jobs::ExtractRequest> {
		if !self.extract_bar.ensure_out() {
			return None;
		}
		let out = self.extract_bar.out.clone()?;
		if self.settings.out.as_ref() != Some(&out) {
			self.settings.out = Some(out.clone());
			self.settings.save();
		}
		Some(jobs::ExtractRequest {
			out,
			decode: self.extract_bar.encoding(),
			force: self.extract_bar.force,
			filter,
		})
	}

	fn extract_archive(&mut self, names: Option<HashSet<String>>) {
		let Some(request) = self.extract_request(names) else {
			return;
		};
		let Some(archive) = &self.archive else {
			return;
		};
		let job = jobs::extract_archive(Arc::clone(&archive.reader), archive.file_name(), request);
		self.start_job(job);
	}

	fn extract_folder(&mut self, paks: Option<Vec<PathBuf>>) {
		let Some(request) = self.extract_request(None) else {
			return;
		};
		let Some(folder) = &self.folder else {
			return;
		};
		let job = jobs::extract_folder(folder.root.clone(), folder.dir.clone(), paks, request);
		self.start_job(job);
	}

	fn empty_state(&mut self, ui: &mut egui::Ui, pal: &Palette, action: &mut WorkspaceAction) {
		let height = ui.available_height();
		let width = ui.available_width().min(520.0);
		// Scrolls: a full recent list under the buttons is taller than a short window.
		screen_scroll(ui, |ui| ui.vertical_centered(|ui| {
			ui.add_space((height * 0.12).min(90.0));
			ui.set_max_width(width);
			theme::mark(ui, 64.0);
			ui.add_space(12.0);
			ui.label(theme::title(pal, "Open an archive or a client folder"));
			ui.add_space(4.0);
			ui.label(theme::note(pal, "Drop a .pak file or a folder anywhere in this window, or pick one."));
			ui.add_space(16.0);
			ui.horizontal(|ui| {
				// Centre the two buttons by hand: a horizontal layout inside a centred column starts at the left.
				let buttons = 300.0;
				ui.add_space(((ui.available_width() - buttons) / 2.0).max(0.0));
				if theme::add_primary(ui, true, theme::primary(pal, "Open archive…").min_size(Vec2::new(140.0, 34.0)))
					.on_hover_text("Ctrl+O")
					.clicked()
				{
					self.pick_archive(ui.ctx());
				}
				if ui
					.add(theme::centered_button("Open folder…").min_size(Vec2::new(140.0, 34.0)))
					.on_hover_text("Ctrl+Shift+O")
					.clicked()
				{
					self.pick_folder(ui.ctx());
				}
			});

			if !self.settings.recent.is_empty() {
				ui.add_space(28.0);
				theme::card(pal).inner_margin(Margin::same(6)).show(ui, |ui| {
					ui.set_width(width - 14.0);
					ui.horizontal(|ui| {
						ui.add_space(8.0);
						ui.label(theme::caption(pal, "RECENT"));
						ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
							if ui
								.add(egui::Button::new(RichText::new("Clear").size(12.0).color(pal.muted())).frame_when_inactive(false))
								.clicked()
							{
								self.settings.recent.clear();
								self.settings.save();
							}
						});
					});
					let recent = self.settings.recent.clone();
					for (path, distinct) in recent.iter().zip(distinct_parents(&recent)) {
						match recent_row(ui, pal, path, distinct.as_deref()) {
							RecentClick::Open => *action = WorkspaceAction::Open(path.clone()),
							RecentClick::Forget => self.forget_recent(path),
							RecentClick::None => {}
						}
					}
				});
				ui.add_space(16.0);
			}
		}));
	}

	fn help_ui(&self, ui: &mut egui::Ui) {
		let pal = theme::pal(ui.ctx());
		screen_scroll(ui, |ui| {
			self.screen_header(ui, Section::Help);
			ui.set_max_width(ui.available_width().min(860.0));
			theme::card(&pal).show(ui, |ui| {
				ui.set_width(ui.available_width());
				for (index, (name, what)) in WINDOW_HELP.iter().enumerate() {
					if index > 0 {
						ui.add_space(4.0);
						ui.separator();
						ui.add_space(4.0);
					}
					ui.label(theme::strong(*name).color(pal.text));
					ui.add(Label::new(theme::note(&pal, *what)).wrap());
				}
			});

			ui.add_space(20.0);
			ui.label(theme::strong("Keyboard").color(pal.text));
			ui.add_space(6.0);
			theme::card(&pal).show(ui, |ui| {
				ui.set_width(ui.available_width());
				egui::Grid::new("keys").num_columns(2).spacing([24.0, 6.0]).show(ui, |ui| {
					for (keys, what) in KEYS {
						ui.label(theme::mono(*keys).color(pal.text));
						ui.label(theme::note(&pal, *what));
						ui.end_row();
					}
				});
			});

			ui.add_space(20.0);
			ui.label(theme::strong("Command line").color(pal.text));
			ui.add_space(2.0);
			ui.label(theme::note(
				&pal,
				"The same program runs these commands in a terminal: put a command in front of the arguments. \
				 Every command also takes -v (list every warning), -q (only errors and the summary), --log <file> \
				 and -j <threads>; --help prints every option.",
			));
			ui.add_space(8.0);
			for entry in &CLI_HELP {
				egui::CollapsingHeader::new(theme::mono(entry.name).size(12.5).color(pal.text))
					.id_salt(entry.name)
					.show(ui, |ui| {
						ui.label(theme::note(&pal, entry.summary));
						if !entry.options.is_empty() {
							ui.add_space(4.0);
							egui::Grid::new(entry.name).num_columns(2).spacing([16.0, 4.0]).show(ui, |ui| {
								for (option, what) in entry.options {
									ui.add(Label::new(theme::mono(*option).color(pal.accent)).extend());
									ui.add(Label::new(RichText::new(*what).size(12.5).color(pal.text_dim)).extend());
									ui.end_row();
								}
							});
						}
						ui.add_space(4.0);
						ui.horizontal(|ui| {
							Frame::new()
								.fill(pal.surface_hi)
								.corner_radius(CornerRadius::same(6))
								.inner_margin(Margin::symmetric(8, 4))
								.show(ui, |ui| {
									ui.add(Label::new(theme::mono(entry.example).color(pal.text)).extend());
								});
							if ui.small_button("Copy").clicked() {
								ui.ctx().copy_text(entry.example.to_owned());
							}
						});
					});
			}
		});
	}

	fn settings_ui(&mut self, ui: &mut egui::Ui) {
		let pal = theme::pal(ui.ctx());
		let mut look = self.settings.look;
		let mut density = self.settings.density;
		let mut reopen = self.settings.reopen;
		screen_scroll(ui, |ui| {
			self.screen_header(ui, Section::Settings);
			ui.set_max_width(ui.available_width().min(760.0));
			theme::card(&pal).show(ui, |ui| {
				ui.set_width(ui.available_width());
				setting_row(ui, &pal, "Appearance", "System follows the light or dark mode of the OS.", |ui| {
					theme::segmented(
						ui,
						&mut look.theme,
						&[(Theme::System, "System"), (Theme::Light, "Light"), (Theme::Dark, "Dark")],
					);
				});
				ui.separator();
				setting_row(
					ui,
					&pal,
					"Accent",
					"The colour of the main buttons, the selection and the progress.",
					|ui| {
						ui.horizontal(|ui| {
							for option in Accent::ALL {
								if accent_swatch(ui, &pal, option, look.accent == option)
									.on_hover_text(option.label())
									.clicked()
								{
									look.accent = option;
								}
							}
						});
					},
				);
				ui.separator();
				setting_row(
					ui,
					&pal,
					"On start",
					"Open the archive or folder you had open last, or show the start page with the recent ones.",
					|ui| {
						theme::segmented(ui, &mut reopen, &[(true, "Last place"), (false, "Recent list")]);
					},
				);
				ui.separator();
				setting_row(ui, &pal, "Density", "Compact fits more rows on screen; the text size stays.", |ui| {
					theme::segmented(
						ui,
						&mut density,
						&[
							(Density::Comfortable, Density::Comfortable.label()),
							(Density::Compact, Density::Compact.label()),
						],
					);
				});
			});

		});
		if look != self.settings.look {
			self.settings.look = look;
			theme::apply(ui.ctx(), &look);
			self.settings.save();
		}
		if reopen != self.settings.reopen {
			self.settings.reopen = reopen;
			self.settings.save();
		}
		if density != self.settings.density {
			self.settings.density = density;
			theme::set_density(ui.ctx(), density);
			self.settings.save();
		}
	}

	fn about_ui(&self, ui: &mut egui::Ui) {
		let pal = theme::pal(ui.ctx());
		screen_scroll(ui, |ui| {
			let height = ui.available_height();
			ui.vertical_centered(|ui| {
				ui.add_space((height * 0.08).min(60.0));
				theme::mark_lit(ui, 64.0, about_glow(ui.ctx()));
				ui.add_space(10.0);
				ui.label(theme::display(&pal, "aionpak").size(24.0));
				ui.label(theme::mono(format!("version {}", env!("CARGO_PKG_VERSION"))).color(pal.muted()));
				ui.add_space(6.0);
				ui.label(theme::note(&pal, "Browse, extract, pack and check the .pak archives of the Aion client."));
				ui.add_space(18.0);
				ui.set_max_width(ui.available_width().min(460.0));
				theme::card(&pal).show(ui, |ui| {
					ui.set_width(436.0);
					egui::Grid::new("about").num_columns(2).spacing([24.0, 8.0]).show(ui, |ui| {
						about_link(ui, "AUTHOR", "Birthright", "https://github.com/Birthright50");
						about_row(ui, "LICENSE", "GPL-3.0-or-later");
						about_row(ui, "FONTS", "IBM Plex Sans, IBM Plex Mono, Cinzel (SIL OFL 1.1)");
						about_link(ui, "SOURCE", "github.com/beyond-aion/aion-pak", "https://github.com/beyond-aion/aion-pak");
					});
				});
			});
		});
	}

	fn screen_header(&self, ui: &mut egui::Ui, section: Section) {
		let pal = theme::pal(ui.ctx());
		ui.label(theme::title(&pal, section.title()));
		let blurb = section.blurb();
		if !blurb.is_empty() {
			ui.add_space(2.0);
			ui.label(theme::note(&pal, blurb));
		}
		ui.add_space(16.0);
	}

	/// A work screen: a header and a form of at most `width`, scrolled when the window is short.
	fn form_screen(&mut self, ui: &mut egui::Ui, section: Section, form: impl FnOnce(&mut Self, &mut egui::Ui)) {
		screen_scroll(ui, |ui| {
			self.screen_header(ui, section);
			ui.set_max_width(ui.available_width().min(820.0));
			form(self, ui);
		});
	}

	fn pak_ui(&mut self, ui: &mut egui::Ui) {
		self.form_screen(ui, Section::Pak, |shell, ui| {
			let pal = theme::pal(ui.ctx());
			let busy = shell.busy();
			if shell.pak_form.ui(ui, &pal, busy)
				&& let Some(spec) = shell.pak_form.spec()
			{
				shell.start_job(jobs::pak(spec));
			}
		});
	}

	fn convert_ui(&mut self, ui: &mut egui::Ui) {
		self.form_screen(ui, Section::Convert, |shell, ui| {
			let pal = theme::pal(ui.ctx());
			let busy = shell.busy();
			if shell.convert_form.ui(ui, &pal, busy)
				&& let Some(spec) = shell.convert_form.spec()
			{
				shell.start_job(jobs::convert(spec));
			}
		});
	}

	fn optimize_ui(&mut self, ui: &mut egui::Ui) {
		let client = self.folder.as_ref().map(|folder| folder.root.clone());
		self.optimize_form.follow_open(client.as_deref());
		self.form_screen(ui, Section::Optimize, |shell, ui| {
			let pal = theme::pal(ui.ctx());
			ui.label(theme::note(
				&pal,
				"The client opens dozens of archives at start and looks every name up in each. Optimize merges the \
				 archives it scans (Data, Levels/Common, Objects, Sounds, Textures, Effects, event, Shaders and each \
				 L10N language) into a few !merged_NNNN.pak volumes, keeping for every name the copy the client would \
				 load. Archives the client opens by name, or that are signed, stay where they are.",
			));
			ui.add_space(10.0);
			let busy = shell.busy();
			if shell.optimize_form.ui(ui, &pal, busy)
				&& let Some(spec) = shell.optimize_form.spec()
			{
				shell.start_job(jobs::optimize(spec));
			}
		});
	}

	fn compare_ui(&mut self, ui: &mut egui::Ui) {
		self.screen_header(ui, Section::Compare);
		let open = self.open_in_browse();
		self.compare_form.follow_open(open.as_deref());
		let pal = theme::pal(ui.ctx());
		let busy = self.busy();
		ui.scope(|ui| {
			ui.set_max_width(ui.available_width().min(820.0));
			if self.compare_form.ui(ui, &pal, busy)
				&& let Some((first, second)) = self.compare_form.spec()
			{
				self.compare_view.clear();
				self.start_job(jobs::compare(first, second, self.compare_view.slot.clone()));
			}
		});
		ui.add_space(14.0);
		self.compare_view.ui(ui, &pal);
	}

	fn verify_ui(&mut self, ui: &mut egui::Ui) {
		self.screen_header(ui, Section::Verify);
		let pal = theme::pal(ui.ctx());
		let busy = self.busy();
		let open = self.open_in_browse();
		self.verify_form.follow_open(open.as_deref());
		let run = ui
			.scope(|ui| {
				ui.set_max_width(ui.available_width().min(820.0));
				self.verify_form.ui(ui, &pal, busy)
			})
			.inner;
		if run {
			self.start_verify();
		}
		ui.add_space(14.0);
		self.report_ui(ui, Report::Verify);
	}

	/// The problems of the last run of a screen, virtualised so a broken archive of 50 000 entries still scrolls
	/// smoothly. A line that names an entry of the open archive jumps to it in Browse when clicked.
	fn report_ui(&mut self, ui: &mut egui::Ui, report: Report) {
		let pal = theme::pal(ui.ctx());
		let kind = match report {
			Report::Verify => JobKind::Verify,
		};
		let job = self.job.as_ref().filter(|job| job.kind == kind);
		let running = job.is_some_and(|job| !job.finished);
		let lines: &[LogLine] = match report {
			_ if running => &self.job_problems,
			Report::Verify => &self.verify_report,
		};
		// Before the first run and while one runs without problems there is nothing to say; the status bar shows
		// the progress.
		if lines.is_empty() {
			if job.is_some_and(|job| job.finished && !job.failed) {
				ui.label(RichText::new("No problems found.").size(13.0).color(pal.success));
			}
			return;
		}
		let errors = lines.iter().filter(|line| line.error).count();
		ui.horizontal(|ui| {
			ui.label(theme::strong(format!("{} problems", lines.len())).color(pal.text));
			if errors > 0 {
				ui.label(theme::mono(format!("{errors} errors")).color(pal.error));
			}
			if lines.len() > errors {
				ui.label(theme::mono(format!("{} warnings", lines.len() - errors)).color(pal.warning));
			}
		});
		ui.add_space(4.0);
		// Problem lines name entries by their name alone; they lead into Browse only when the check ran on the
		// archive open there, or a same-named entry of another archive would be shown.
		let archive = self.archive.as_ref().filter(|archive| self.verified.as_ref() == Some(&archive.path));
		let jump = theme::card(&pal)
			.inner_margin(Margin::same(6))
			.show(ui, |ui| {
				let line = |i: usize| {
					let line = &lines[i];
					let (tag, color) = if line.error { ("error", pal.error) } else { ("warning", pal.warning) };
					(tag.to_owned(), color, line.text.clone(), pal.text)
				};
				let clickable = |i: usize| entry_from_problem(archive, &lines[i].text).is_some();
				let event = wrapped_list(
					ui,
					&mut self.report_wrap,
					(kind as u8, "report"),
					lines,
					60.0,
					false,
					&line,
					&clickable,
					None,
				);
				match event {
					Some(LineEvent::Click(i, _)) => entry_from_problem(archive, &lines[i].text),
					_ => None,
				}
			})
			.inner;
		if let Some(index) = jump
			&& let Some(parent) = self.archive.as_ref().map(|archive| browse::parent_key(&archive.rows[index]))
		{
			// Show the entry in its folder, whatever filter or folder Browse was on.
			self.filter.clear();
			self.set_folder(parent);
			if let Some(position) = self.items.iter().position(|item| matches!(item, Item::Entry(i) if *i == index)) {
				self.pick_row(Pick {
					index: position,
					ctrl: false,
					shift: false,
				});
			}
			self.section = Section::Browse;
		}
	}

	/// The journal: every event with the time it happened at, virtualised and stuck to the newest line.
	fn log_panel(&mut self, ui: &mut egui::Ui) {
		let pal = theme::pal(ui.ctx());
		// Lines only on the left and at the bottom: the sheet above ends in its own border, and a frame stroke plus
		// the panel's separator on top of it would stack into a three-pixel line.
		let frame = Frame::new()
			.fill(pal.surface)
			.stroke(Stroke::NONE)
			.inner_margin(Margin::symmetric(14, 8));
		// Never eat more than half of the content area: on a short window the log would leave the tree nothing.
		let cap = (ui.available_height() * 0.5).max(90.0);
		let mut close = false;
		let mut clear = false;
		// Slides in and out at the pace of the section rail; egui takes the time from the global style, so it is set
		// for this panel only.
		let mut open = self.log_open;
		// What the panel may cover. While it slides in or out, or is dragged shut, its rect hangs below this, over the
		// status bar; the outline is drawn only for the part that shows.
		let area = ui.available_rect_before_wrap();
		let pace = ui.ctx().global_style().animation_time;
		ui.ctx().all_styles_mut(|style| style.animation_time = RAIL_SLIDE);
		let panel = egui::Panel::bottom("log")
			.resizable(true)
			.drag_to_open(false)
			.show_separator_line(false)
			.default_size(220.0f32.min(cap))
			.min_size(90.0)
			.max_size(cap)
			.frame(frame)
			.show_collapsible(ui, &mut open, |ui| {
				ui.horizontal(|ui| {
					ui.label(theme::strong("Log").color(pal.text));
					ui.label(theme::mono(format!("{} lines", self.log.len())).color(pal.muted()));
					ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
						close = close_button(ui, &pal).on_hover_text("Hide the log; it keeps recording").clicked();
						if ui.small_button("Clear").clicked() {
							clear = true;
						}
						let picked = self.log_selected.len();
						let (label, hint) = if picked == 0 {
							("Copy".to_owned(), "Copy every line to the clipboard; click lines to copy only those".to_owned())
						} else {
							(format!("Copy {picked}"), format!("Copy the {picked} picked lines (Ctrl+C)"))
						};
						if ui.small_button(label).on_hover_text(hint).clicked() {
							if picked == 0 {
								self.copy_log(ui.ctx(), 0..self.log.len());
							} else {
								self.copy_log(ui.ctx(), self.log_selected.iter().copied());
							}
						}
					});
				});
				ui.add_space(2.0);
				if self.log.is_empty() {
					return;
				}
				let log = &self.log;
				let line = |i: usize| {
					let line = &log[i];
					let color = if line.rule {
						pal.accent
					} else if line.error {
						pal.error
					} else {
						pal.text_dim
					};
					(line.at.to_string(), pal.muted(), line.text.clone(), color)
				};
				let selected = &self.log_selected;
				let event = wrapped_list(
					ui,
					&mut self.log_wrap,
					"log",
					log,
					196.0,
					true,
					&line,
					&|_| true,
					Some(&|i| selected.contains(&i)),
				);
				match event {
					Some(LineEvent::Click(i, modifiers)) => self.pick_log_line(i, modifiers),
					Some(LineEvent::Copy(i)) => {
						if self.log_selected.contains(&i) {
							self.copy_log(ui.ctx(), self.log_selected.iter().copied());
						} else {
							self.copy_log(ui.ctx(), std::iter::once(i));
						}
					}
					None => {}
				}
				// Ctrl+C copies the picked lines, unless a text field has the keyboard.
				let copy = !self.log_selected.is_empty()
					&& !ui.ctx().egui_wants_keyboard_input()
					&& ui.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Copy)));
				if copy {
					self.copy_log(ui.ctx(), self.log_selected.iter().copied());
				}
			});
		ui.ctx().all_styles_mut(|style| style.animation_time = pace);
		// Dragged below its least height, the panel closes itself.
		self.log_open = open;
		if let Some(panel) = panel {
			let rect = panel.response.rect.intersect(area);
			if rect.is_positive() {
				let stroke = Stroke::new(1.0, pal.border);
				let painter = ui.painter().with_clip_rect(area);
				painter.vline(rect.left() + 0.5, rect.y_range(), stroke);
				painter.hline(rect.x_range(), rect.bottom() - 0.5, stroke);
			}
		}
		if clear {
			self.log.clear();
			self.log_selected.clear();
			self.log_anchor = None;
			self.job_lines_cleared = self.job.is_some();
		}
		if close {
			self.log_open = false;
		}
	}
}

impl eframe::App for Shell {
	/// Ends the press a move or resize began, which the system kept to itself (see `handed_drag`).
	/// Opaque, though the window is created transparent on Windows (see main.rs): what shows before a frame is drawn,
	/// while the window grows, is the background, not the desktop behind it.
	fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
		visuals.panel_fill.to_normalized_gamma_f32()
	}

	fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
		if !std::mem::take(&mut self.handed_drag) {
			return;
		}
		let pos = ctx.input(|i| i.pointer.latest_pos()).unwrap_or_default();
		raw_input.events.push(egui::Event::PointerButton {
			pos,
			button: egui::PointerButton::Primary,
			pressed: false,
			modifiers: ctx.input(|i| i.modifiers),
		});
	}

	fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
		if self.settings.look.theme == Theme::System {
			let dark = ui.ctx().theme() == egui::Theme::Dark;
			if dark != self.system_dark {
				self.system_dark = dark;
				theme::apply(ui.ctx(), &self.settings.look);
			}
		}

		let pal = theme::pal(ui.ctx());
		ui.painter().rect_filled(ui.max_rect(), 0.0, pal.bg);

		self.poll();
		self.accept_drops(ui.ctx());
		self.shortcuts(ui.ctx());

		let spacing = ui.spacing().item_spacing.y;
		ui.spacing_mut().item_spacing.y = 0.0;
		self.title_bar(ui);
		ui.spacing_mut().item_spacing.y = spacing;

		self.status(ui);
		self.rail(ui);
		self.log_panel(ui);

		// The content sits on a raised sheet with a rounded corner towards the rail, the way current desktop
		// apps separate navigation from work.
		let sheet = sheet_color(&pal);
		egui::CentralPanel::default()
			.frame(
				Frame::new()
					.fill(sheet)
					.stroke(Stroke::new(1.0, pal.border))
					.corner_radius(CornerRadius { nw: 12, ..Default::default() })
					.inner_margin(Margin::symmetric(22, 18))
					// The right edge under the window's own border, so the two do not show as a double line.
					.outer_margin(Margin { right: -1, ..Default::default() }),
			)
			.show(ui, |ui| match self.section {
				Section::Browse => self.workspace_ui(ui),
				Section::Pak => self.pak_ui(ui),
				Section::Convert => self.convert_ui(ui),
				Section::Verify => self.verify_ui(ui),
				Section::Compare => self.compare_ui(ui),
				Section::Optimize => self.optimize_ui(ui),
				Section::Help => self.help_ui(ui),
				Section::Settings => self.settings_ui(ui),
				Section::About => self.about_ui(ui),
			});

		drop_overlay(ui);
		self.handed_drag |= resize_handles(ui);
		window_border(ui);

		if self.loading.is_some() || self.busy() || self.form_scan.is_some() {
			ui.ctx().request_repaint_after(Duration::from_millis(60));
		}
		pointing_hand(ui.ctx());
		self.remember_window(ui.ctx());
	}
}

/// The sheet the screens sit on, a shade lighter than the window in the dark themes.
fn sheet_color(pal: &Palette) -> Color32 {
	if pal.dark { pal.bg.lerp_to_gamma(pal.surface, 0.45) } else { pal.bg }
}

/// A whole screen that scrolls when the window is short. Where content goes on past an edge, it fades into the
/// sheet there: cut off hard, the fill of a card ends in a straight edge that reads as a stray line.
fn screen_scroll<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
	let output = egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, add);
	let rect = output.inner_rect;
	let offset = output.state.clamped_offset().y;
	let hidden_below = output.content_size.y - offset - rect.height();
	let sheet = sheet_color(&theme::pal(ui.ctx()));
	let fade = |from: f32, to: f32, strength: f32| {
		let clear = sheet.gamma_multiply(0.0);
		let solid = sheet.gamma_multiply(strength);
		let mut mesh = egui::Mesh::default();
		let (top, bottom, top_color, bottom_color) =
			if from < to { (from, to, solid, clear) } else { (to, from, clear, solid) };
		mesh.colored_vertex(egui::pos2(rect.left(), top), top_color);
		mesh.colored_vertex(egui::pos2(rect.right(), top), top_color);
		mesh.colored_vertex(egui::pos2(rect.right(), bottom), bottom_color);
		mesh.colored_vertex(egui::pos2(rect.left(), bottom), bottom_color);
		mesh.add_triangle(0, 1, 2);
		mesh.add_triangle(0, 2, 3);
		ui.painter().add(egui::Shape::mesh(mesh));
	};
	const FADE: f32 = 28.0;
	if offset > 0.5 {
		fade(rect.top(), rect.top() + FADE.min(offset), 1.0);
	}
	if hidden_below > 0.5 {
		fade(rect.bottom(), rect.bottom() - FADE.min(hidden_below), 1.0);
	}
	output.inner
}

/// The hand over whatever reacts to a click (buttons, checkboxes, switches, combo boxes, the headers in Help), unless
/// a widget asked for another cursor. Widgets that are also dragged keep theirs: text, the title bar, the edges.
fn pointing_hand(ctx: &egui::Context) {
	if ctx.output(|o| o.cursor_icon) != egui::CursorIcon::Default {
		return;
	}
	// Copied out first: reading a response takes the context's lock, which the snapshot holds.
	let hovered: Vec<egui::Id> = ctx.interaction_snapshot(|snapshot| snapshot.hovered.iter().copied().collect());
	let clickable = hovered.iter().any(|id| {
		ctx.read_response(*id)
			.is_some_and(|r| r.enabled() && r.sense.senses_click() && !r.sense.senses_drag())
	});
	if clickable {
		ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
	}
}

/// A problem line reads `archive: entry: what went wrong`; the entry name is in the middle.
fn entry_from_problem(archive: Option<&Archive>, text: &str) -> Option<usize> {
	let archive = archive?;
	let mut parts = text.splitn(3, ": ");
	parts.next()?;
	let name = parts.next()?;
	parts.next()?;
	archive.rows.iter().position(|row| row.name == name)
}

/// A problem's text with the job's folder taken off the paths in it, so the part that tells files apart is what
/// shows: `Data\Dialogs\ldf5b\ldf5b.pak: …` rather than the whole client path in front of it.
fn relative(text: &str, base: Option<&Path>) -> String {
	let Some(base) = base else {
		return text.to_owned();
	};
	let mut prefix = base.display().to_string();
	if !prefix.ends_with(['/', '\\']) {
		prefix.push(std::path::MAIN_SEPARATOR);
	}
	text.replace(&prefix, "")
}

/// Heights of wrapped lines, kept between frames: measuring every line of a long log on each frame would cost far
/// more than drawing the few in view.
#[derive(Default)]
struct WrapCache {
	width: f32,
	/// Identifies the lines measured: their slice, and its first line, which moves when the log drops old lines.
	key: (usize, usize),
	/// Top of every line, then the total height.
	tops: Vec<f32>,
}

const LINE_PREFIX_GAP: f32 = 12.0;
const LINE_GAP: f32 = 4.0;
/// Room above a line that starts an operation, so the rule stands apart from the run before it.
const RULE_SPACE: f32 = 10.0;

/// Lines that wrap to the width instead of ending in "…", virtualised: their heights are measured once, and only
/// those in view are laid out and painted. `line` gives a line's prefix (a time or a tag) with its colour and its
/// text with its colour. Returns the line clicked, among those `clickable` allows.
#[allow(clippy::too_many_arguments)]
fn wrapped_list(
	ui: &mut egui::Ui,
	cache: &mut WrapCache,
	id: impl std::hash::Hash + std::fmt::Debug,
	lines: &[LogLine],
	prefix_width: f32,
	stick_to_bottom: bool,
	line: &dyn Fn(usize) -> (String, Color32, String, Color32),
	clickable: &dyn Fn(usize) -> bool,
	// `Some`: lines are picked for copying (shaded when picked, Copy in their context menu) rather than followed.
	picked: Option<&dyn Fn(usize) -> bool>,
) -> Option<LineEvent> {
	let count = lines.len();
	let font = FontId::new(12.0, FontFamily::Monospace);
	let width = (ui.available_width() - prefix_width - LINE_PREFIX_GAP - 14.0).max(80.0);
	let key = (lines.as_ptr() as usize, lines.first().map_or(0, |line| line.text.as_ptr() as usize));
	if (cache.width - width).abs() > 0.5 || cache.key != key || cache.tops.len() > count + 1 || cache.tops.is_empty() {
		cache.width = width;
		cache.key = key;
		cache.tops.clear();
		cache.tops.push(0.0);
	}
	while cache.tops.len() <= count {
		let (_, _, text, color) = line(cache.tops.len() - 1);
		let height = ui.fonts_mut(|fonts| fonts.layout(text, font.clone(), color, width).size().y);
		let top = cache.tops.last().copied().unwrap_or(0.0);
		let rule = if lines[cache.tops.len() - 1].rule { RULE_SPACE } else { 0.0 };
		cache.tops.push(top + rule + height.max(14.0) + LINE_GAP);
	}
	let total = cache.tops[count];
	let pal = theme::pal(ui.ctx());
	let mut event = None;
	egui::ScrollArea::vertical()
		.id_salt(id)
		.auto_shrink([false, false])
		.stick_to_bottom(stick_to_bottom)
		.show_viewport(ui, |ui, viewport| {
			ui.set_height(total);
			let origin = ui.max_rect().min;
			let full_width = ui.max_rect().width();
			let first = cache.tops.partition_point(|top| *top <= viewport.min.y).saturating_sub(1);
			for i in first..count {
				let top = cache.tops[i];
				if top > viewport.max.y {
					break;
				}
				let rule = lines[i].rule;
				let space = if rule { RULE_SPACE } else { 0.0 };
				let row = Rect::from_min_size(
					origin + Vec2::new(0.0, top + space),
					Vec2::new(full_width, cache.tops[i + 1] - top - space - LINE_GAP / 2.0),
				);
				let is_picked = picked.is_some_and(|picked| picked(i));
				if is_picked {
					ui.painter().rect_filled(row.expand(2.0), CornerRadius::same(4), pal.selection);
				}
				if clickable(i) {
					let response = ui.interact(row, ui.scope_id().with(("line", i)), Sense::click());
					if response.hovered() && !is_picked {
						ui.painter().rect_filled(row.expand(2.0), CornerRadius::same(4), pal.surface_hi);
					}
					// Only lines that jump somewhere show the hand.
					if response.hovered() && picked.is_none() {
						ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
					}
					if response.clicked() {
						event = Some(LineEvent::Click(i, ui.input(|input| input.modifiers)));
					}
					if picked.is_some() {
						response.context_menu(|ui| {
							if ui.button("Copy").clicked() {
								event = Some(LineEvent::Copy(i));
								ui.close();
							}
						});
					}
				}
				let (prefix, prefix_color, text, color) = line(i);
				let prefix = ui.fonts_mut(|fonts| fonts.layout_no_wrap(prefix, font.clone(), prefix_color));
				ui.painter().galley(row.min, prefix, prefix_color);
				let text = ui.fonts_mut(|fonts| fonts.layout(text, font.clone(), color, width));
				let text_pos = row.min + Vec2::new(prefix_width + LINE_PREFIX_GAP, 0.0);
				// A run starts below a rule across the whole width, in the middle of the gap left for it. The first line
				// has nothing above it to set apart.
				if rule && i > 0 {
					let y = (origin.y + top - LINE_GAP + row.min.y) / 2.0;
					ui.painter().hline(origin.x..=origin.x + full_width, y.round() + 0.5, Stroke::new(1.0, pal.border));
				}
				ui.painter().galley(text_pos, text, color);
			}
		});
	event
}

fn filter_id() -> egui::Id {
	egui::Id::unique("workspace-filter")
}

/// While files are dragged over the window, say what dropping them does.
fn drop_overlay(ui: &mut egui::Ui) {
	let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
	if !hovering {
		return;
	}
	let pal = theme::pal(ui.ctx());
	let rect = ui.max_rect().shrink(12.0);
	let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop")));
	painter.rect_filled(ui.max_rect(), 0.0, pal.bg.gamma_multiply(0.82));
	painter.rect_stroke(rect, CornerRadius::same(16), Stroke::new(2.0, pal.accent), StrokeKind::Inside);
	painter.text(
		rect.center(),
		Align2::CENTER_CENTER,
		"Drop a .pak or a folder to open it",
		FontId::new(20.0, FontFamily::Proportional),
		pal.text,
	);
}

/// Edge and corner grips. The window has no decorations, so nothing else would let the user resize it.
/// Returns whether a resize was handed to the system.
fn resize_handles(ui: &mut egui::Ui) -> bool {
	use egui::CursorIcon as C;
	use egui::viewport::ResizeDirection as D;
	let ctx = ui.ctx().clone();
	if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
		return false;
	}
	let mut handed = false;
	let r = ui.max_rect();
	let t = 6.0;
	let (l, rt, tp, b) = (r.min.x, r.max.x, r.min.y, r.max.y);
	let grips: [(Rect, D, C); 8] = [
		(Rect::from_min_max(r.min, egui::pos2(rt, tp + t)), D::North, C::ResizeVertical),
		(Rect::from_min_max(egui::pos2(l, b - t), r.max), D::South, C::ResizeVertical),
		(Rect::from_min_max(r.min, egui::pos2(l + t, b)), D::West, C::ResizeHorizontal),
		(Rect::from_min_max(egui::pos2(rt - t, tp), r.max), D::East, C::ResizeHorizontal),
		(Rect::from_min_max(r.min, egui::pos2(l + t, tp + t)), D::NorthWest, C::ResizeNwSe),
		(Rect::from_min_max(egui::pos2(rt - t, b - t), r.max), D::SouthEast, C::ResizeNwSe),
		(Rect::from_min_max(egui::pos2(rt - t, tp), egui::pos2(rt, tp + t)), D::NorthEast, C::ResizeNeSw),
		(Rect::from_min_max(egui::pos2(l, b - t), egui::pos2(l + t, b)), D::SouthWest, C::ResizeNeSw),
	];
	for (index, (rect, direction, cursor)) in grips.into_iter().enumerate() {
		let response = ui
			.interact(rect, ui.scope_id().with(("grip", index)), Sense::drag())
			.on_hover_cursor(cursor);
		if response.drag_started() {
			ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
			handed = true;
		}
	}
	handed
}

/// How lit the mark on About is: it strikes like a gas tube each time the screen is opened, two short flickers and
/// a steady glow, then fades back to the plain mark. About 1.4 s in all.
fn about_glow(ctx: &egui::Context) -> f32 {
	let id = egui::Id::unique("about-glow");
	let now = ctx.input(|i| i.time);
	let frame = ctx.cumulative_frame_nr();
	// (when it started, the frame About was last drawn in): a frame without it means the screen was left and opened
	// again. Frames, not time: an idle window draws nothing for seconds while About stays open.
	let (start, seen) = ctx.data(|d| d.get_temp::<(f64, u64)>(id)).unwrap_or((now, u64::MAX));
	let start = if seen.wrapping_add(1) != frame && seen != frame {
		now
	} else {
		start
	};
	ctx.data_mut(|d| d.insert_temp(id, (start, frame)));
	let t = (now - start) as f32;
	const FLICKER: [(f32, f32); 4] = [(0.06, 0.55), (0.11, 0.0), (0.16, 0.35), (0.24, 0.0)];
	let glow = match t {
		t if t < 0.24 => FLICKER
			.iter()
			.find(|(end, _)| t < *end)
			.map_or(0.0, |(_, level)| *level),
		t if t < 0.5 => (t - 0.24) / 0.26,
		t if t < 0.8 => 1.0,
		t if t < 1.4 => {
			let x = (t - 0.8) / 0.6;
			1.0 - x * x * (3.0 - 2.0 * x)
		}
		_ => 0.0,
	};
	if t < 1.4 {
		ctx.request_repaint();
	}
	glow
}

/// The window's edge: one physical pixel wide, on the outermost pixels. Sized in pixels, not points: one point
/// half a point in from the edge falls between pixels at 125 % or 150 % and smears into a second line inside.
///
/// Not on Windows 11: it draws its own one-pixel border round a window without a title bar, and ours inside it would
/// show as a double line at the top.
fn window_border(ui: &egui::Ui) {
	if system_draws_border() {
		return;
	}
	let pal = theme::pal(ui.ctx());
	let px = 1.0 / ui.ctx().pixels_per_point();
	ui.painter()
		.rect_stroke(ui.max_rect(), 0.0, Stroke::new(px, pal.border), StrokeKind::Inside);
}

/// Windows 11 (build 22000 and later) borders every top-level window itself.
#[cfg(windows)]
fn system_draws_border() -> bool {
	use windows_sys::Win32::System::SystemInformation::OSVERSIONINFOW;
	static ANSWER: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
	// RtlGetVersion, not GetVersionExW: without a compatibility manifest the latter reports Windows 8.
	#[link(name = "ntdll", kind = "raw-dylib")]
	unsafe extern "system" {
		fn RtlGetVersion(info: *mut OSVERSIONINFOW) -> i32;
	}
	*ANSWER.get_or_init(|| {
		let mut info = OSVERSIONINFOW {
			dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
			..Default::default()
		};
		// SAFETY: the struct is sized as the call requires and lives for the call.
		let status = unsafe { RtlGetVersion(&mut info) };
		status == 0 && info.dwMajorVersion >= 10 && info.dwBuildNumber >= 22000
	})
}

#[cfg(not(windows))]
fn system_draws_border() -> bool {
	false
}

#[derive(Clone, Copy)]
enum WindowButton {
	Minimize,
	Maximize,
	Close,
}

/// The drawer handle: three lines that fold and unfold the section rail.
fn menu_button(ui: &mut egui::Ui, pal: &Palette) -> egui::Response {
	let (rect, response) = ui.allocate_exact_size(Vec2::new(34.0, 30.0), Sense::click());
	if response.hovered() {
		ui.painter().rect_filled(rect, CornerRadius::same(7), pal.surface_hi);
	}
	let stroke = Stroke::new(1.5, if response.hovered() { pal.text } else { pal.text_dim });
	let center = rect.center();
	for line in -1..=1 {
		let y = center.y + line as f32 * 4.5;
		ui.painter()
			.line_segment([egui::pos2(center.x - 7.0, y), egui::pos2(center.x + 7.0, y)], stroke);
	}
	response
}

/// The status bar's Log toggle: a framed pill with a list glyph, and a red count when errors were logged.
fn log_button(ui: &mut egui::Ui, pal: &Palette, open: bool, errors: usize) -> egui::Response {
	let font = FontId::new(12.5, FontFamily::Name("medium".into()));
	let label = ui.painter().layout_no_wrap("Log".to_owned(), font, Color32::PLACEHOLDER);
	let badge = (errors > 0).then(|| {
		let text = if errors > 99 { "99+".to_owned() } else { errors.to_string() };
		ui.painter().layout_no_wrap(text, FontId::new(11.0, FontFamily::Name("semibold".into())), Color32::WHITE)
	});
	let badge_width = badge.as_ref().map_or(0.0, |galley| galley.size().x.max(8.0) + 10.0 + 6.0);
	let size = Vec2::new(10.0 + 12.0 + 6.0 + label.size().x + badge_width + 10.0, 24.0);
	let (rect, response) = ui.allocate_exact_size(size, Sense::click());
	let hovered = response.hovered();
	let (fill, stroke) = if open {
		(pal.selection, pal.accent)
	} else if hovered {
		(pal.surface_hi.lerp_to_gamma(pal.text_dim, 0.08), pal.border.lerp_to_gamma(pal.text_dim, 0.4))
	} else {
		(pal.surface_hi, pal.border)
	};
	let painter = ui.painter();
	painter.rect_filled(rect, CornerRadius::same(7), fill);
	painter.rect_stroke(rect, CornerRadius::same(7), Stroke::new(1.0, stroke), StrokeKind::Inside);
	let color = if open || hovered { pal.text } else { pal.text_dim };
	// Three lines of a list.
	let icon_left = rect.left() + 10.0;
	for line in 0..3 {
		let y = rect.center().y - 4.0 + line as f32 * 4.0;
		painter.line_segment(
			[egui::pos2(icon_left, y), egui::pos2(icon_left + if line == 2 { 8.0 } else { 12.0 }, y)],
			Stroke::new(1.5, color),
		);
	}
	let text_left = icon_left + 12.0 + 6.0;
	painter.galley(egui::pos2(text_left, rect.center().y - label.size().y / 2.0), label.clone(), color);
	if let Some(galley) = badge {
		let width = galley.size().x.max(8.0) + 10.0;
		let pill = Rect::from_center_size(
			egui::pos2(text_left + label.size().x + 6.0 + width / 2.0, rect.center().y),
			Vec2::new(width, 16.0),
		);
		painter.rect_filled(pill, CornerRadius::same(8), pal.error);
		painter.galley(pill.center() - galley.size() / 2.0, galley, Color32::WHITE);
	}
	if hovered {
		ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
	}
	response
}

/// A small × that closes a pane or dismisses a message.
/// The closed preview pane: a strip with an arrow and the pane's name, beside the table read upwards, under it
/// across. A click opens the pane again.
fn folded_pane(ui: &mut egui::Ui, pal: &Palette, rect: Rect, beside: bool) -> egui::Response {
	let response = ui
		.interact(rect, egui::Id::unique("folded-pane"), Sense::click())
		.on_hover_cursor(egui::CursorIcon::PointingHand)
		.on_hover_text("Show the preview pane");
	let hovered = response.hovered();
	let painter = ui.painter();
	painter.rect(
		rect,
		CornerRadius::same(theme::CARD_RADIUS),
		if hovered { pal.surface_hi } else { pal.surface },
		Stroke::new(1.0, if hovered { pal.accent.gamma_multiply(0.6) } else { pal.border }),
		StrokeKind::Inside,
	);
	let color = if hovered { pal.text } else { pal.text_dim };
	let stroke = Stroke::new(1.5, color);
	let galley = painter.layout_no_wrap("Preview".to_owned(), FontId::new(12.5, FontFamily::Name("medium".into())), color);
	let size = galley.size();
	if beside {
		// An arrow pointing where the pane opens, then the name read from the bottom up.
		let c = egui::pos2(rect.center().x, rect.top() + 20.0);
		painter.line_segment([c + Vec2::new(2.5, -5.0), c + Vec2::new(-2.5, 0.0)], stroke);
		painter.line_segment([c + Vec2::new(-2.5, 0.0), c + Vec2::new(2.5, 5.0)], stroke);
		let pos = egui::pos2(rect.center().x - size.y / 2.0, c.y + 16.0 + size.x);
		painter.add(egui::epaint::TextShape::new(pos, galley, color).with_angle(-std::f32::consts::FRAC_PI_2));
	} else {
		let total = 10.0 + 8.0 + size.x;
		let left = rect.center().x - total / 2.0;
		let c = egui::pos2(left + 5.0, rect.center().y);
		painter.line_segment([c + Vec2::new(-5.0, 2.5), c + Vec2::new(0.0, -2.5)], stroke);
		painter.line_segment([c + Vec2::new(0.0, -2.5), c + Vec2::new(5.0, 2.5)], stroke);
		painter.galley(egui::pos2(left + 18.0, rect.center().y - size.y / 2.0), galley, color);
	}
	response
}

fn close_button(ui: &mut egui::Ui, pal: &Palette) -> egui::Response {
	let (rect, response) = ui.allocate_exact_size(Vec2::splat(22.0), Sense::click());
	if response.hovered() {
		// A step past the hover shade of rows and buttons, so it still shows on a row that is itself hovered.
		ui.painter()
			.rect_filled(rect, CornerRadius::same(6), pal.surface_hi.lerp_to_gamma(pal.border, 0.7));
	}
	let stroke = Stroke::new(1.5, if response.hovered() { pal.text } else { pal.muted() });
	let c = rect.center();
	ui.painter().line_segment([c + Vec2::new(-4.0, -4.0), c + Vec2::new(4.0, 4.0)], stroke);
	ui.painter().line_segment([c + Vec2::new(-4.0, 4.0), c + Vec2::new(4.0, -4.0)], stroke);
	response
}

fn window_button(ui: &mut egui::Ui, pal: &Palette, kind: WindowButton, maximized: bool) -> egui::Response {
	let (rect, response) = ui.allocate_exact_size(Vec2::new(46.0, TITLE_HEIGHT), Sense::click());
	let hovered = response.hovered();
	let background = match (kind, hovered) {
		(WindowButton::Close, true) => Color32::from_rgb(0xD9, 0x2D, 0x3A),
		(_, true) => pal.surface_hi,
		_ => Color32::TRANSPARENT,
	};
	ui.painter().rect_filled(rect, 0.0, background);

	let color = match (kind, hovered) {
		(WindowButton::Close, true) => Color32::WHITE,
		(_, true) => pal.text,
		_ => pal.text_dim,
	};
	let stroke = Stroke::new(1.0, color);
	let center = rect.center();
	match kind {
		WindowButton::Minimize => {
			ui.painter().hline(center.x - 5.0..=center.x + 5.0, center.y, stroke);
		}
		WindowButton::Maximize => {
			let square = Rect::from_center_size(center, Vec2::new(10.0, 10.0));
			if maximized {
				ui.painter().rect_stroke(
					square.translate(Vec2::new(2.0, -2.0)),
					1.0,
					Stroke::new(1.0, color.gamma_multiply(0.6)),
					StrokeKind::Inside,
				);
				ui.painter().rect_filled(square, 1.0, pal.bg);
			}
			ui.painter().rect_stroke(square, 1.0, stroke, StrokeKind::Inside);
		}
		WindowButton::Close => {
			ui.painter()
				.line_segment([center + Vec2::new(-5.0, -5.0), center + Vec2::new(5.0, 5.0)], stroke);
			ui.painter()
				.line_segment([center + Vec2::new(-5.0, 5.0), center + Vec2::new(5.0, -5.0)], stroke);
		}
	}
	response
}

fn rail_item(ui: &mut egui::Ui, section: Section, active: bool, open: f32) -> egui::Response {
	let pal = theme::pal(ui.ctx());
	let height = 36.0;
	let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), height), Sense::click());
	let hover = ui.ctx().animate_bool_with_time(response.id, response.hovered(), 0.12);
	let fill = if active {
		pal.selection
	} else {
		pal.bg.lerp_to_gamma(pal.surface_hi, hover)
	};
	ui.painter().rect_filled(rect, CornerRadius::same(9), fill);
	let color = if active {
		pal.accent
	} else if response.hovered() {
		pal.text
	} else {
		pal.text_dim
	};
	let icon = Rect::from_center_size(egui::pos2(rect.left() + 20.0, rect.center().y), Vec2::splat(20.0));
	icons::glyph(ui.painter(), icon, section.glyph(), color);
	if open > 0.6 {
		let alpha = ((open - 0.6) / 0.4).clamp(0.0, 1.0);
		let text = if active { pal.text } else { color };
		ui.painter().text(
			egui::pos2(rect.left() + 42.0, rect.center().y),
			Align2::LEFT_CENTER,
			section.title(),
			FontId::new(14.0, FontFamily::Name("medium".into())),
			text.gamma_multiply(alpha),
		);
	}
	if open < 0.5 {
		response.on_hover_text(section.title())
	} else {
		response
	}
}

/// One row of a settings card: name and explanation on the left, the control beside them.
fn setting_row(ui: &mut egui::Ui, pal: &Palette, name: &str, hint: &str, control: impl FnOnce(&mut egui::Ui)) {
	ui.horizontal(|ui| {
		ui.allocate_ui_with_layout(Vec2::new(300.0, 0.0), Layout::top_down(Align::Min), |ui| {
			ui.set_width(300.0);
			ui.label(theme::strong(name).color(pal.text));
			ui.add(Label::new(RichText::new(hint).size(12.5).color(pal.text_dim)).wrap());
		});
		ui.add_space(24.0);
		control(ui);
	});
	ui.add_space(4.0);
}

fn accent_swatch(ui: &mut egui::Ui, pal: &Palette, accent: Accent, selected: bool) -> egui::Response {
	let (rect, response) = ui.allocate_exact_size(Vec2::splat(30.0), Sense::click());
	let center = rect.center();
	if selected {
		ui.painter().circle_stroke(center, 14.0, Stroke::new(2.0, accent.swatch(pal.dark)));
	} else if response.hovered() {
		ui.painter().circle_stroke(center, 14.0, Stroke::new(1.0, pal.border));
	}
	ui.painter().circle_filled(center, 10.0, accent.swatch(pal.dark));
	response
}

/// For each path, the end of its parent folder that tells it apart from the others with the same name (`None` when
/// the name is unique): two `Data.pak` of two clients become `Destiny/Data` and `Gamez/Data`.
fn distinct_parents(paths: &[PathBuf]) -> Vec<Option<String>> {
	let name = |path: &Path| path.file_name().map(|n| n.to_string_lossy().to_lowercase());
	let parts = |path: &Path| -> Vec<String> {
		path.parent()
			.map(|parent| {
				parent
					.components()
					.map(|c| c.as_os_str().to_string_lossy().into_owned())
					.collect()
			})
			.unwrap_or_default()
	};
	let tail = |parts: &[String], k: usize| parts[parts.len().saturating_sub(k)..].join("/");
	paths
		.iter()
		.map(|path| {
			let twins: Vec<&PathBuf> = paths.iter().filter(|other| *other != path && name(other) == name(path)).collect();
			if twins.is_empty() {
				return None;
			}
			let mine = parts(path);
			let theirs: Vec<Vec<String>> = twins.iter().map(|other| parts(other)).collect();
			// The fewest trailing folders that no twin shares; all of them when even that does not do.
			let k = (1..=mine.len())
				.find(|&k| theirs.iter().all(|other| tail(other, k).to_lowercase() != tail(&mine, k).to_lowercase()))
				.unwrap_or(mine.len());
			Some(tail(&mine, k))
		})
		.collect()
}

/// One entry of the start page's recent list: icon, name, and below it the parent folder, or the part of it that
/// tells the entry apart from another with the same name (`distinct`).
/// Least width of a recent entry in the Open menu, its × beside it.
const MENU_ENTRY_WIDTH: f32 = 340.0;

/// What happened to a row of the recent list.
enum RecentClick {
	None,
	Open,
	Forget,
}

fn recent_row(ui: &mut egui::Ui, pal: &Palette, path: &Path, distinct: Option<&str>) -> RecentClick {
	let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 40.0), Sense::click());
	// The × to forget the entry shows while the row is under the pointer; it sits on top of the row, so it takes
	// the click there.
	let hot = response.contains_pointer();
	if hot {
		ui.painter().rect_filled(rect, CornerRadius::same(7), pal.surface_hi);
	}
	let forget_rect = Rect::from_center_size(egui::pos2(rect.right() - 20.0, rect.center().y), Vec2::splat(22.0));
	// A child that takes no room in the list: placed in the list's own layout, the × moved the rows below it.
	let forget = hot.then(|| {
		close_button(&mut ui.new_child(egui::UiBuilder::new().max_rect(forget_rect)), pal)
			.on_hover_text("Remove from the list")
	});
	let on_forget = forget.as_ref().is_some_and(|forget| forget.hovered());
	if hot && !on_forget {
		ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
	}
	let exists = path.exists();
	let kind = if path.is_dir() { icons::Kind::Folder } else { icons::Kind::Archive };
	let icon = Rect::from_center_size(egui::pos2(rect.left() + 18.0, rect.center().y), Vec2::splat(14.0));
	icons::file_icon(
		&mut ui.new_child(egui::UiBuilder::new().max_rect(icon)),
		kind,
		if exists { pal.accent } else { pal.muted() },
	);
	let name = path
		.file_name()
		.map(|name| name.to_string_lossy().into_owned())
		.unwrap_or_else(|| path.display().to_string());
	let parent = match distinct {
		Some(tail) => format!("…/{tail}"),
		None => path.parent().map(|parent| parent.display().to_string()).unwrap_or_default(),
	};
	let text_left = rect.left() + 38.0;
	// Clear of the ×.
	let width = rect.right() - text_left - 40.0;
	let painter = ui.painter().with_clip_rect(rect);
	painter.text(
		egui::pos2(text_left, rect.center().y - 8.0),
		Align2::LEFT_CENTER,
		name,
		FontId::new(13.5, FontFamily::Proportional),
		if exists { pal.text } else { pal.muted() },
	);
	let parent = elide(ui, &parent, width, FontId::new(11.5, FontFamily::Monospace));
	painter.text(
		egui::pos2(text_left, rect.center().y + 9.0),
		Align2::LEFT_CENTER,
		parent,
		FontId::new(11.5, FontFamily::Monospace),
		pal.muted(),
	);
	if forget.is_some_and(|forget| forget.clicked()) {
		return RecentClick::Forget;
	}
	let response = if on_forget {
		response
	} else if exists {
		response.on_hover_text(path.display().to_string())
	} else {
		response.on_hover_text("No longer there")
	};
	if response.clicked() { RecentClick::Open } else { RecentClick::None }
}

/// Shortens a path from the left so its end, the part that tells paths apart, stays visible.
fn elide(ui: &egui::Ui, text: &str, width: f32, font: FontId) -> String {
	let fits = |candidate: &str| {
		ui.fonts_mut(|fonts| fonts.layout_no_wrap(candidate.to_owned(), font.clone(), Color32::WHITE).size().x) <= width
	};
	if fits(text) {
		return text.to_owned();
	}
	let chars: Vec<char> = text.chars().collect();
	for start in 1..chars.len() {
		let candidate: String = std::iter::once('…').chain(chars[start..].iter().copied()).collect();
		if fits(&candidate) {
			return candidate;
		}
	}
	String::new()
}

/// What the window does, for the Help section.
const WINDOW_HELP: [(&str, &str); 9] = [
	(
		"Browse",
		"Open a .pak, or a client folder to see its directories and archives; double-click an archive to look \
		 inside. Extract takes everything, what the filter shows, or the selection (Ctrl+click, Shift+click, \
		 Ctrl+A). Double-click a file to open it in the program your system uses for it, or right-click for more. \
		 The preview pane, beside the table or under it in a narrow window, shows the selected file (picture, \
		 text, sound, model or font) with its details and first bytes; closed, it leaves a strip that opens it \
		 again.",
	),
	(
		"Pack",
		"Builds an archive from a folder, or puts every <name>.pak/ folder of an extracted client back into its \
		 archive. Files you did not change keep the compressed data of the source archive, so packing after a \
		 small edit is quick. XML is compiled and HTML stored as text unless you choose otherwise.",
	),
	(
		"Convert",
		"Turns loose XML and HTML files into readable text, or back into a form the client reads: one file or a \
		 whole folder. To do the same while extracting, tick Decode XML and HTML in Browse.",
	),
	(
		"Verify",
		"Checks that every file of an archive, or of every archive under a folder, reads back intact. Problems \
		 are listed under the button; when the archive checked is the one open in Browse, click one to jump to \
		 the file there.",
	),
	(
		"Compare",
		"Checks that two archives, or two client folders, hold the same files. Each archive is listed with what \
		 differs; pick one to see its files added, removed or changed, with their sizes. Names are matched as the \
		 client looks them up, without regard to case.",
	),
	(
		"Optimize",
		"Merges the archives the client scans at start into a few large volumes, so the game starts faster. Build \
		 them into a separate folder first; Apply puts them into the client and keeps a backup that Undo restores. \
		 Close the game before Apply or Undo.",
	),
	(
		"What is open in Browse",
		"Stays open whichever screen you switch to. Verify starts with it as what to check, Compare as its \
		 first side and Optimize, for a client folder, as the client, as long as the field is empty or still \
		 holds what was filled in; a path you choose yourself is never replaced.",
	),
	(
		"The .aionpak manifest",
		"Extracting writes a small .aionpak file next to the files. It records how the archive was built (order, \
		 dates, stored forms and where it came from) so Pack can rebuild it the same way and reuse the data of \
		 unchanged files. The client ignores it; keep it with the folder.",
	),
	(
		"Log",
		"Everything that happened, with the time it happened at. Open it from the status bar.",
	),
];

const KEYS: &[(&str, &str)] = &[
	("Ctrl+O", "open an archive"),
	("Ctrl+Shift+O", "open a client folder"),
	("Ctrl+F", "filter the open archive"),
	("Ctrl+A", "select everything shown"),
	("Ctrl+click, Shift+click", "add one row, or a range, to the selection; on a folder, select it"),
	("↑ ↓", "move between rows"),
	("Esc", "clear the selection"),
	("Enter", "open the selected folder or file"),
	("Backspace", "one folder up"),
	("Click a folder", "go into it"),
	("Double-click a file", "open it in its program, or look inside an archive"),
];

/// One command of the command line, shown collapsed in Help.
struct CliHelp {
	name: &'static str,
	summary: &'static str,
	options: &'static [(&'static str, &'static str)],
	example: &'static str,
}

const CLI_HELP: [CliHelp; 9] = [
	CliHelp {
		name: "list <pak>",
		summary: "Lists the entries of an archive: size, stored size, method and name.",
		options: &[],
		example: "aionpak list Data/World/World.pak",
	},
	CliHelp {
		name: "extract <pak|folder> -o <out>",
		summary: "Unpacks one archive, or every archive under a folder, into a tree the packer can rebuild.",
		options: &[
			("-d", "turn XML and HTML into readable text"),
			("--utf8", "that text in UTF-8 instead of UTF-16"),
			("--include <glob>", "only matching names, e.g. \"*.{xml,html}\""),
			("--force", "extract into a folder that is not empty"),
		],
		example: "aionpak extract Data/skills/skills.pak -o skills -d",
	},
	CliHelp {
		name: "pack <folder> -o <pak>",
		summary: "Builds an archive from a folder, reusing the stored data of every file you did not change.",
		options: &[
			("-l 0..12", "compression level, 0 stores, default 9"),
			("--xml <form>", "compiled (default), utf16, original, keep or encrypted"),
			("--html <form>", "text (default), original or keep"),
			("--flavor <kind>", "plain, aion or original"),
			("--recompress", "recompress every file, not only the changed ones"),
			("--decrypt", "store XML that would end up encrypted as text"),
			("--force", "replace an archive even from a folder without a manifest"),
		],
		example: "aionpak pack skills -o Data/skills/skills.pak",
	},
	CliHelp {
		name: "repack <tree> -o <client>",
		summary: "Packs every <name>.pak/ folder under a tree back into its archive in the client.",
		options: &[
			("-l 0..12", "compression level for changed files"),
			("--xml <form>", "compiled (default), utf16, original, keep or encrypted"),
			("--html <form>", "text (default), original or keep"),
			("--flavor <kind>", "plain, aion or original"),
			("--recompress", "recompress every file, not only the changed ones"),
			("--decrypt", "store XML that would end up encrypted as text"),
			("--force", "also folders without a manifest"),
		],
		example: "aionpak repack tree -o <client>",
	},
	CliHelp {
		name: "decode <file|folder>",
		summary: "Turns loose XML and HTML files into readable text, no archive involved.",
		options: &[
			("-o <out>", "write here instead of next to the input"),
			("--utf8", "UTF-8 instead of UTF-16"),
		],
		example: "aionpak decode Data/skills -o skills_text --utf8",
	},
	CliHelp {
		name: "encode <file|folder>",
		summary: "The reverse of decode: brings loose files into a form the client reads.",
		options: &[
			("--xml <form>", "compiled, encrypted, utf16 or text (default compiled)"),
			("--html <form>", "keep, encrypted, utf16 or text (default keep: left as they are)"),
			("-o <out>", "write here instead of over the input"),
		],
		example: "aionpak encode skills_text -o Data/skills --xml compiled",
	},
	CliHelp {
		name: "verify <pak|folder>",
		summary: "Reads every entry and checks its CRC the way the client does.",
		options: &[],
		example: "aionpak verify Data/World/World.pak",
	},
	CliHelp {
		name: "compare <a> <b>",
		summary: "Checks that two archives, or two trees, hold the same files: names, order, sizes and CRCs.",
		options: &[],
		example: "aionpak compare client-before client",
	},
	CliHelp {
		name: "optimize <client>",
		summary: "Merges the archives the client scans at start into a few volumes, keeping the copy it would load.",
		options: &[
			("-o <out>", "write the volumes here and leave the client alone"),
			("--apply", "write into the client and move the replaced archives to a backup"),
			("--undo <backup>", "revert an apply"),
			("--backup <folder>", "where --apply moves the archives (default <client>/aionpak-backup-<time>)"),
			("-l 0..12", "recompress at this level, default 9"),
			("--copy", "copy the stored data instead of recompressing it"),
			("--keep-rest", "leave the archives that are not merged as they are"),
			("--xml <form>", "compiled (default), utf16, text, encrypted or stored"),
			("--html <form>", "text (default) or stored"),
			("--decrypt", "store XML that stays encrypted as text"),
			("--volume-size <size>", "largest volume, 50M to 4G, e.g. 512M or 1G (default 512M)"),
		],
		example: "aionpak optimize <client> -o merged",
	},
];

fn archive_strip(ui: &mut egui::Ui, archive: &Archive, shown: Option<usize>, scanning: bool) {
	let pal = theme::pal(ui.ctx());
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing.x = 6.0;
		ui.label(theme::mono(format!("{} files", archive.rows.len() as u64 - archive.dirs)).color(pal.text));
		let dot = |ui: &mut egui::Ui| {
			ui.label(theme::mono("·").color(pal.muted()));
		};
		dot(ui);
		ui.label(
			theme::mono(format!(
				"{} → {} ({:.0}%)",
				human_bytes(archive.total_size),
				human_bytes(archive.total_packed),
				archive.ratio() * 100.0
			))
			.color(pal.text_dim),
		)
		.on_hover_text("Unpacked size → size in the archive");
		dot(ui);
		ui.label(theme::mono(archive.flavor).color(pal.text_dim))
			.on_hover_text("How the archive stores its headers: standard zip, or the client's scrambled variant");
		if scanning {
			dot(ui);
			ui.label(theme::mono("reading XML forms…").color(pal.muted()));
		}
		if let Some(shown) = shown {
			ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
				ui.label(theme::mono(format!("{shown} matches in the whole archive")).color(pal.accent));
			});
		}
	});
}

/// Where in the archive the table is: the archive's name, then each folder. Returns the folder clicked.
fn breadcrumb(
	ui: &mut egui::Ui,
	pal: &Palette,
	archive: &Archive,
	names: &[String],
	searching: bool,
	outside: &[(String, PathBuf)],
	back_to: &mut Option<PathBuf>,
) -> Option<String> {
	let mut jump = None;
	ui.horizontal_wrapped(|ui| {
		ui.spacing_mut().item_spacing.x = 2.0;
		let crumb = |ui: &mut egui::Ui, text: &str, last: bool| {
			let color = if last { pal.text } else { pal.text_dim };
			ui.add(egui::Button::new(theme::mono(text).color(color)).frame_when_inactive(false))
				.on_hover_cursor(egui::CursorIcon::PointingHand)
		};
		for (label, dir) in outside {
			if crumb(ui, label, false).clicked() {
				*back_to = Some(dir.clone());
			}
			ui.label(theme::mono("/").color(pal.muted()));
		}
		let name = Archive::file_name(archive);
		if crumb(ui, &name, names.is_empty() && !searching).clicked() {
			jump = Some(String::new());
		}
		let mut key = String::new();
		for (depth, part) in names.iter().enumerate() {
			key.push_str(&part.to_lowercase());
			key.push('/');
			ui.label(theme::mono("/").color(pal.muted()));
			if crumb(ui, part, depth + 1 == names.len() && !searching).clicked() {
				jump = Some(key.clone());
			}
		}
		if searching {
			ui.label(theme::mono("  ·  filter applies to every folder").color(pal.muted()));
		}
	});
	jump
}

/// The entries behind a set of rows, each once: a folder stands for everything in it.
fn expand<'a>(archive: &'a Archive, keys: impl Iterator<Item = &'a RowKey> + 'a) -> impl Iterator<Item = usize> + 'a {
	let mut seen = HashSet::new();
	keys.flat_map(move |key| -> Vec<usize> {
		match key {
			RowKey::Entry(index) => vec![*index],
			RowKey::Folder(folder) => browse::under(&archive.rows, folder).collect(),
		}
	})
	.filter(move |index| seen.insert(*index))
}

/// What the archive table draws.
struct TableView<'a> {
	archive: &'a Archive,
	items: &'a [Item],
	selection: &'a BTreeSet<RowKey>,
	/// Showing filter matches from every folder, so names keep their folder.
	searching: bool,
	sort: Option<(Col, bool)>,
	scroll_to: Option<usize>,
}

/// Sorts folders among themselves and files among themselves, folders first, the way up staying on top. Ties keep
/// the archive's order, so sorting by form groups the files without shuffling each group.
fn sort_items(items: &mut [Item], archive: &Archive, col: Col, descending: bool) {
	use std::cmp::Ordering;
	let rank = |item: &Item| match item {
		Item::Up => 0,
		Item::Folder(_) => 1,
		Item::Entry(_) => 2,
	};
	let ratio = |size: u64, packed: u64| if size == 0 { 0.0 } else { packed as f64 / size as f64 };
	let compare = |a: &Item, b: &Item| -> Ordering {
		match (a, b) {
			(Item::Folder(x), Item::Folder(y)) => match col {
				Col::Size => x.size.cmp(&y.size),
				Col::Packed => x.packed.cmp(&y.packed),
				Col::Ratio => ratio(x.size, x.packed).total_cmp(&ratio(y.size, y.packed)),
				Col::Form => x.files.cmp(&y.files),
				Col::Name | Col::Date | Col::Crc => x.name.to_lowercase().cmp(&y.name.to_lowercase()),
			},
			(Item::Entry(i), Item::Entry(j)) => {
				let (x, y) = (&archive.rows[*i], &archive.rows[*j]);
				match col {
					Col::Name => {
						let name = |r: &browse::Row| r.name_lower.rsplit('/').next().unwrap_or(&r.name_lower).to_owned();
						name(x).cmp(&name(y))
					}
					Col::Size => x.size.cmp(&y.size),
					Col::Packed => x.packed.cmp(&y.packed),
					Col::Ratio => ratio(x.size as u64, x.packed as u64).total_cmp(&ratio(y.size as u64, y.packed as u64)),
					Col::Form => archive.form(*i).name().cmp(archive.form(*j).name()),
					Col::Date => x.date.cmp(&y.date),
					Col::Crc => x.crc.cmp(&y.crc),
				}
			}
			_ => Ordering::Equal,
		}
	};
	items.sort_by(|a, b| {
		rank(a).cmp(&rank(b)).then_with(|| {
			let order = compare(a, b);
			if descending { order.reverse() } else { order }
		})
	});
}

fn table(ui: &mut egui::Ui, view: &TableView, pick: &mut Option<Pick>, action: &mut Option<TableAction>) {
	use egui_extras::TableBuilder;

	let archive = view.archive;
	let ctx = ui.ctx().clone();
	let density = theme::density(&ctx);
	let row_height = density.row_height();
	let header_height = density.header_height();
	// The table paints its rows to the parent's clip rect, over the card's margin and border; keep it inside.
	ui.set_clip_rect(ui.max_rect());
	if view.items.is_empty() {
		ui.add_space(24.0);
		ui.vertical_centered(|ui| {
			let text = if view.searching { "No names match the filter." } else { "This folder is empty." };
			ui.label(theme::note(&theme::pal(&ctx), text));
		});
		return;
	}
	// All the height there is, not a whole number of rows: the part of a row left over would show as an empty band
	// over the card's bottom edge, growing and shrinking as the log is dragged. The last row is cut off instead,
	// and scrolls into view.
	let body_height = (ui.available_height() - header_height).max(row_height);
	// Rows are exactly this tall; the default gap between them would push the last one out of the card.
	ui.spacing_mut().item_spacing.y = 0.0;
	let cols = visible_columns(ui.available_width() - 10.0);
	let mut builder = TableBuilder::new(ui)
		.striped(true)
		.sense(Sense::click())
		.min_scrolled_height(body_height)
		.max_scroll_height(body_height)
		.cell_layout(Layout::left_to_right(Align::Center));
	for &col in &cols {
		builder = builder.column(col.column());
	}
	if let Some(row) = view.scroll_to {
		builder = builder.scroll_to_row(row, None);
	}
	let modifiers = |ctx: &egui::Context| ctx.input(|i| (i.modifiers.command, i.modifiers.shift));
	builder
		.header(header_height, |mut header| {
			for &col in &cols {
				let (_, response) = header.col(|ui| {
					let pal = theme::pal(ui.ctx());
					let title = match view.sort {
						Some((sorted, descending)) if sorted == col => {
							format!("{} {}", col.title(), if descending { "▼" } else { "▲" })
						}
						_ => col.title().to_owned(),
					};
					let label = Label::new(theme::caption(&pal, title)).selectable(false);
					if col.numeric() {
						ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
							ui.add_space(NUMBER_GAP);
							ui.add(label)
						});
					} else {
						let response = ui.add(label);
						if matches!(col, Col::Form) {
							response.on_hover_text("How the client stores this XML or HTML file");
						}
					}
				});
				if response.on_hover_text("Sort by this column; a second click reverses, a third restores the archive's order").clicked() {
					*action = Some(TableAction::Sort(col));
				}
			}
		})
		.body(|body| {
			body.rows(row_height, view.items.len(), |mut row| {
				let position = row.index();
				let item = &view.items[position];
				let key = item.key();
				row.set_selected(key.as_ref().is_some_and(|key| view.selection.contains(key)));
				// The name label takes the clicks that land on it; they count as clicks on the row.
				let mut name = None;
				for &col in &cols {
					let (_, cell) = row.col(|ui| {
						if let Some(response) = col.cell(ui, view, item) {
							name = Some(response);
						}
					});
					if cell.hovered() {
						ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
					}
				}
				let mut response = row.response();
				if let Some(name) = name {
					response = response.union(name);
				}
				match item {
					// The way up is not something to select: a click takes it.
					Item::Up => {
						if response.clicked() {
							*action = Some(TableAction::Up);
						}
						response.on_hover_text("Up one folder (Backspace)");
					}
					// A click goes into the folder, as in the client folder view; Ctrl or Shift selects it instead,
					// for extracting.
					Item::Folder(folder) => {
						if response.clicked() {
							let (ctrl, shift) = modifiers(&ctx);
							if ctrl || shift {
								*pick = Some(Pick { index: position, ctrl, shift });
							} else {
								*action = Some(TableAction::Enter(folder.key.clone()));
							}
						}
						response.container_context_menu(|ui| {
							ui.set_min_width(200.0);
							if ui.button("Open folder").clicked() {
								*action = Some(TableAction::Enter(folder.key.clone()));
								ui.close();
							}
							if ui.button("Extract folder").clicked() {
								*action = Some(TableAction::Extract(folder.key.clone()));
								ui.close();
							}
							if ui.button("Copy path").clicked() {
								ui.ctx().copy_text(folder.key.clone());
								ui.close();
							}
						});
					}
					Item::Entry(index) => {
						let index = *index;
						if response.clicked() {
							let (ctrl, shift) = modifiers(&ctx);
							*pick = Some(Pick { index: position, ctrl, shift });
						}
						if response.double_clicked() {
							*action = Some(TableAction::Open(index, preferred_mode(archive, index)));
						}
						let entry = &archive.rows[index];
						let executable = open::is_executable(&entry.name);
						let needs_decode = archive.form(index).needs_decode();
						response.container_context_menu(|ui| {
							ui.set_min_width(200.0);
							if executable {
								ui.label(
									RichText::new("Programs are not started from an archive")
										.size(12.0)
										.color(theme::pal(ui.ctx()).warning),
								);
							} else if ui.button("Open").clicked() {
								*action = Some(TableAction::Open(index, open::Mode::Raw));
								ui.close();
							}
							if !executable && needs_decode && ui.button("Open as text").clicked() {
								*action = Some(TableAction::Open(index, open::Mode::Text));
								ui.close();
							}
							ui.separator();
							if ui.button("Copy path").clicked() {
								ui.ctx().copy_text(entry.name.clone());
								ui.close();
							}
						});
					}
				}
			});
		});
}

/// Compiled and encrypted XML needs decoding before a text editor makes sense of it; everything else is
/// opened as stored.
fn preferred_mode(archive: &Archive, index: usize) -> open::Mode {
	if archive.form(index).needs_decode() {
		open::Mode::Text
	} else {
		open::Mode::Raw
	}
}

/// Columns disappear from the right as the window narrows, so the table never needs a sideways scroll.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Col {
	Name,
	Size,
	Packed,
	Ratio,
	Form,
	Date,
	Crc,
}

impl Col {
	fn title(self) -> &'static str {
		match self {
			Col::Name => "NAME",
			Col::Size => "SIZE",
			Col::Packed => "PACKED",
			Col::Ratio => "RATIO",
			Col::Form => "FORM",
			Col::Date => "MODIFIED",
			Col::Crc => "CRC32",
		}
	}

	fn numeric(self) -> bool {
		matches!(self, Col::Size | Col::Packed | Col::Ratio)
	}

	fn column(self) -> egui_extras::Column {
		use egui_extras::Column;
		match self {
			Col::Name => Column::remainder().at_least(160.0).clip(true),
			Col::Size => Column::exact(86.0),
			Col::Packed => Column::exact(86.0),
			Col::Ratio => Column::exact(62.0),
			Col::Form => Column::exact(84.0),
			Col::Date => Column::exact(124.0),
			Col::Crc => Column::exact(78.0),
		}
	}

	/// Draws one cell; for the name, returns the label's response, whose clicks belong to the row.
	fn cell(self, ui: &mut egui::Ui, view: &TableView, item: &Item) -> Option<egui::Response> {
		let pal = theme::pal(ui.ctx());
		let archive = view.archive;
		let name_cell = |ui: &mut egui::Ui, kind: icons::Kind, text: &str, color: Color32| {
			// Selectable, so a name can be copied. A plain click still selects the row; a drag across the text
			// only selects the text and leaves the row alone.
			ui.horizontal(|ui| {
				icons::file_icon(ui, kind, kind_color(kind, &pal));
				ui.add_space(4.0);
				// Not selectable: a double-click would select a word instead of opening, and Copy path is in the
				// row's menu.
				ui.add(Label::new(theme::mono(text).color(color)).truncate().selectable(false))
			})
			.inner
		};
		let ratio = |size: u64, packed: u64| {
			if size == 0 {
				String::new()
			} else {
				format!("{:.0}%", packed as f64 / size as f64 * 100.0)
			}
		};
		match item {
			Item::Up => {
				if matches!(self, Col::Name) {
					return Some(name_cell(ui, icons::Kind::Folder, "..", pal.text_dim));
				}
			}
			Item::Folder(folder) => match self {
				Col::Name => return Some(name_cell(ui, icons::Kind::Folder, &folder.name, pal.text)),
				Col::Size => number(ui, human_bytes(folder.size)),
				Col::Packed => number(ui, human_bytes(folder.packed)),
				Col::Ratio => number(ui, ratio(folder.size, folder.packed)),
				Col::Form => {
					ui.label(theme::mono(files(folder.files)).color(pal.muted()));
				}
				Col::Date | Col::Crc => {}
			},
			Item::Entry(index) => {
				let index = *index;
				let entry = &archive.rows[index];
				match self {
					Col::Name => {
						let text = if view.searching {
							entry.path.as_str()
						} else {
							entry.path.rsplit('/').next().unwrap_or(&entry.path)
						};
						return Some(name_cell(ui, icons::kind_of(&entry.name, false), text, pal.text));
					}
					Col::Size => number(ui, human_bytes(entry.size as u64)),
					Col::Packed => number(ui, human_bytes(entry.packed as u64)),
					Col::Ratio => number(ui, ratio(entry.size as u64, entry.packed as u64)),
					Col::Form => {
						let form = archive.form(index);
						if !form.name().is_empty() {
							// Only UTF-8 text is a real risk (the client misreads it); the other forms are just how
							// the client stores things, and colouring every encrypted row would drown the one that
							// matters.
							let color = if form == browse::Stored::Utf8 { pal.warning } else { pal.text_dim };
							ui.add(Label::new(theme::mono(form.name()).color(color)).extend())
								.on_hover_text(form.hint());
						}
					}
					Col::Date => {
						ui.add(Label::new(theme::mono(&entry.date).color(pal.muted())).extend());
					}
					Col::Crc => {
						ui.add(Label::new(theme::mono(format!("{:08x}", entry.crc)).color(pal.muted())).extend());
					}
				}
			}
		}
		None
	}
}

/// "1 file", "12 files".
fn files(count: u64) -> String {
	if count == 1 { "1 file".to_owned() } else { format!("{count} files") }
}

fn visible_columns(width: f32) -> Vec<Col> {
	let mut cols = vec![Col::Name, Col::Size];
	for (col, needs) in [
		(Col::Packed, 340.0),
		(Col::Ratio, 420.0),
		(Col::Form, 520.0),
		(Col::Date, 660.0),
		(Col::Crc, 760.0),
	] {
		if width >= needs {
			cols.push(col);
		}
	}
	cols
}

fn kind_color(kind: icons::Kind, pal: &Palette) -> Color32 {
	use icons::Kind;
	match kind {
		Kind::Folder | Kind::Text => pal.text_dim,
		Kind::Xml => pal.accent,
		Kind::Image => pal.success,
		Kind::Mesh => pal.warning,
		Kind::Sound => pal.accent_alt,
		Kind::Archive | Kind::Other => pal.muted(),
	}
}

const NUMBER_GAP: f32 = 10.0;

fn number(ui: &mut egui::Ui, text: String) {
	let pal = theme::pal(ui.ctx());
	ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
		// A gap before the next column, so right-aligned numbers do not touch a left-aligned header.
		ui.add_space(NUMBER_GAP);
		ui.add(Label::new(theme::mono(text).color(pal.text_dim)).extend());
	});
}

fn details(
	ui: &mut egui::Ui,
	archive: &Archive,
	index: usize,
	columns: usize,
	preview: Option<&preview::Preview>,
	close: &mut bool,
	tab: &mut DetailTab,
) {
	let pal = theme::pal(ui.ctx());
	let Some(entry) = archive.rows.get(index) else {
		return;
	};
	let short = entry.name.rsplit('/').next().unwrap_or(&entry.name);

	ui.horizontal(|ui| {
		ui.add(Label::new(theme::strong(short).color(pal.text)).truncate());
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			*close = close_button(ui, &pal).on_hover_text("Close the pane; the strip it leaves brings it back").clicked();
		});
	});
	if short != entry.name {
		ui.add(Label::new(theme::mono(&entry.name).color(pal.muted())).truncate());
	}
	ui.add_space(6.0);
	theme::segmented(
		ui,
		tab,
		&[(DetailTab::Preview, "Preview"), (DetailTab::Metadata, "Details"), (DetailTab::Hex, "Hex")],
	);
	ui.add_space(8.0);

	match tab {
		DetailTab::Preview => match preview {
			Some(preview) => preview::show(ui, preview),
			None => {
				ui.label(theme::note(&pal, "No preview for this entry."));
			}
		},
		DetailTab::Metadata => {
			let form = archive.form(index);
			let ratio = if entry.size == 0 {
				"—".to_owned()
			} else {
				format!("{:.1}%", entry.packed as f32 / entry.size as f32 * 100.0)
			};
			// What the content is first, then where and how the archive keeps it. All of it is in memory already:
			// the directory entry, and the bytes the preview read.
			let mut fields: Vec<(&str, String, Option<&str>)> = preview
				.map(|preview| preview.facts.iter().map(|(name, value)| (*name, value.clone(), None)).collect())
				.unwrap_or_default();
			fields.push(("SIZE", human_bytes(entry.size as u64), None));
			fields.push(("PACKED", human_bytes(entry.packed as u64), None));
			fields.push(("RATIO", ratio, None));
			if let Some(stored) = archive.reader.entries().get(index) {
				let method = match stored.method {
					0 => "stored".to_owned(),
					8 => "deflate".to_owned(),
					other => format!("method {other}"),
				};
				fields.push(("COMPRESSION", method, None));
				let header = if stored.scrambled { "Aion (scrambled)" } else { "standard zip" };
				fields.push(("HEADER", header.to_owned(), Some("How the entry's zip header is written")));
				fields.push((
					"OFFSET",
					format!("0x{:08x}", stored.header_offset),
					Some("Where the entry's local header starts in the archive"),
				));
			}
			if !form.name().is_empty() {
				fields.push(("FORM", form.name().to_owned(), Some(form.hint())));
			}
			fields.push(("MODIFIED", entry.date.clone(), None));
			fields.push(("CRC32", format!("{:08x}", entry.crc), None));
			fields.push((
				"POSITION",
				format!("{} of {}", index + 1, archive.rows.len()),
				Some("The entry's place in the archive's directory, the order the client reads it in"),
			));
			egui::Grid::new("details").num_columns(columns).spacing([24.0, 10.0]).show(ui, |ui| {
				for (i, (name, value, hint)) in fields.iter().enumerate() {
					field(ui, name, value, *hint);
					if (i + 1) % columns == 0 {
						ui.end_row();
					}
				}
			});
		}
		DetailTab::Hex => match preview {
			Some(preview) => preview::show_hex(ui, preview),
			None => {
				ui.label(theme::note(&pal, "No bytes to show."));
			}
		},
	}
}

/// The details pane for a folder or several rows: how many files they stand for and their size.
/// The pane with nothing picked: what the folder shown holds, and how to pick.
fn overview(ui: &mut egui::Ui, archive: &Archive, folder_key: &str, items: &[Item], pal: &Palette, close: &mut bool) {
	let keys: Vec<RowKey> = items.iter().filter_map(Item::key).collect();
	let (mut count, mut size, mut packed) = (0u64, 0u64, 0u64);
	for index in expand(archive, keys.iter()) {
		let row = &archive.rows[index];
		count += 1;
		size += row.size as u64;
		packed += row.packed as u64;
	}
	let name = match folder_key.trim_end_matches('/').rsplit('/').next() {
		Some(name) if !name.is_empty() => format!("{name}/"),
		_ => Archive::file_name(archive),
	};
	ui.horizontal(|ui| {
		ui.label(theme::strong(format!("{name} · {}", files(count))).color(pal.text));
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			*close = close_button(ui, pal).on_hover_text("Close the pane; the strip it leaves brings it back").clicked();
		});
	});
	ui.add_space(4.0);
	ui.label(theme::mono(format!("{} → {}", human_bytes(size), human_bytes(packed))).color(pal.text_dim));
	ui.add_space(8.0);
	ui.label(theme::note(
		pal,
		"Pick a file to preview it here. The arrow keys move between rows, Enter opens, Backspace goes up. \
		 Ctrl+click adds or removes a row, Shift+click takes a range, Ctrl+A takes everything shown.",
	));
}

fn multi_details(ui: &mut egui::Ui, archive: &Archive, selection: &BTreeSet<RowKey>, pal: &Palette, close: &mut bool) {
	let (mut count, mut size, mut packed) = (0u64, 0u64, 0u64);
	for index in expand(archive, selection.iter()) {
		let row = &archive.rows[index];
		count += 1;
		size += row.size as u64;
		packed += row.packed as u64;
	}
	let title = match selection.iter().next() {
		Some(RowKey::Folder(key)) if selection.len() == 1 => {
			let name = key.trim_end_matches('/').rsplit('/').next().unwrap_or(key);
			format!("{name}/ · {}", files(count))
		}
		_ => format!("{} selected · {}", selection.len(), files(count)),
	};
	ui.horizontal(|ui| {
		ui.label(theme::strong(title).color(pal.text));
		ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
			*close = close_button(ui, pal)
				.on_hover_text("Close the pane; the strip it leaves brings it back")
				.clicked();
		});
	});
	ui.add_space(4.0);
	ui.label(theme::mono(format!("{} → {}", human_bytes(size), human_bytes(packed))).color(pal.text_dim));
	ui.add_space(8.0);
	ui.label(theme::note(
		pal,
		"Click a folder to open it. Ctrl+click adds or removes a row, Shift+click takes a range, Ctrl+A takes \
		 everything shown, Esc clears. Extract takes folders with everything in them.",
	));
}

fn field(ui: &mut egui::Ui, name: &str, value: &str, hint: Option<&str>) {
	let pal = theme::pal(ui.ctx());
	ui.vertical(|ui| {
		ui.add(Label::new(theme::caption(&pal, name)).extend());
		let response = ui.add(Label::new(theme::mono(value).color(pal.text)).extend());
		if let Some(hint) = hint {
			response.on_hover_text(hint);
		}
	});
}

fn about_row(ui: &mut egui::Ui, name: &str, value: &str) {
	let pal = theme::pal(ui.ctx());
	ui.add(Label::new(theme::caption(&pal, name)).extend());
	ui.add(Label::new(RichText::new(value).size(13.0).color(pal.text)).wrap());
	ui.end_row();
}

fn about_link(ui: &mut egui::Ui, name: &str, text: &str, url: &str) {
	let pal = theme::pal(ui.ctx());
	ui.add(Label::new(theme::caption(&pal, name)).extend());
	// Coloured by hand: the palette's override text colour would otherwise paint links like plain text.
	ui.hyperlink_to(RichText::new(text).size(13.0).color(pal.accent), url).on_hover_text(url);
	ui.end_row();
}

fn idle(ui: &mut egui::Ui, pal: &Palette, scanning: bool) {
	status_dot(ui, if scanning { pal.accent } else { pal.success });
	ui.label(RichText::new("Ready").size(12.5).color(pal.text_dim));
}

fn status_dot(ui: &mut egui::Ui, color: Color32) {
	let (rect, _) = ui.allocate_exact_size(Vec2::splat(8.0), Sense::hover());
	ui.painter().circle_filled(rect.center(), 3.5, color);
}

fn running_job(ui: &mut egui::Ui, job: &mut Job) {
	let pal = theme::pal(ui.ctx());
	ui.spacing_mut().item_spacing.x = 10.0;
	if ui.small_button("Cancel").clicked() {
		job.progress.cancel();
	}
	let total = job.progress.bytes_total.load(std::sync::atomic::Ordering::Relaxed);
	let current = job.progress.current();
	if total == 0 {
		// Still preparing (reading directories, planning): no amount to measure yet, so say what it is doing
		// instead of a frozen 0%.
		busy_bar(ui, 180.0);
		ui.add(Label::new(RichText::new(&job.title).size(12.5).color(pal.text)).truncate());
		let phase = if current.is_empty() { "preparing…".to_owned() } else { format!("{current}…") };
		ui.label(theme::mono(phase).color(pal.text_dim));
		return;
	}
	progress_bar(ui, job.fraction(), 180.0);
	ui.label(theme::mono(format!("{:>3.0}%", job.fraction() * 100.0)).color(pal.text));
	ui.add(Label::new(RichText::new(&job.title).size(12.5).color(pal.text)).truncate());
	let mut facts = Vec::new();
	let (files_done, files_total) = job.files();
	if files_total > 0 {
		facts.push(format!("{files_done}/{files_total} files"));
	}
	facts.push(format!("{}/s", human_bytes(job.rate() as u64)));
	if let Some(eta) = job.eta() {
		facts.push(format!("{} left", human_time(eta)));
	}
	ui.label(theme::mono(facts.join(" · ")).color(pal.text_dim));
	let problems = job.progress.problem_count();
	if problems > 0 {
		ui.label(theme::mono(format!("{problems} problems")).color(pal.warning));
	}
	if !current.is_empty() {
		ui.add(Label::new(theme::mono(current).color(pal.muted())).truncate());
	}
}

/// A bar with a moving segment, for work whose size is not known yet.
fn busy_bar(ui: &mut egui::Ui, width: f32) {
	let pal = theme::pal(ui.ctx());
	let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 6.0), Sense::hover());
	ui.painter().rect_filled(rect, CornerRadius::same(3), pal.surface_hi);
	let t = ui.input(|i| i.time) as f32;
	let segment = width * 0.3;
	let x = rect.left() + ((t * 0.8).fract() * (width + segment)) - segment;
	let bar = Rect::from_min_max(egui::pos2(x.max(rect.left()), rect.top()), egui::pos2((x + segment).min(rect.right()), rect.bottom()));
	if bar.width() > 0.5 {
		ui.painter().rect_filled(bar, CornerRadius::same(3), pal.accent);
	}
}

/// The outcome stays after the log is cleared: it is still what the job did. Only the link to the lines goes, since
/// it would open an empty log.
fn finished_job(ui: &mut egui::Ui, job: &Job, lines_cleared: bool, open_log: &mut bool) {
	let pal = theme::pal(ui.ctx());
	let errors = job.progress.error_count();
	let warnings = job.progress.problem_count() - errors;
	let color = if job.failed || errors > 0 {
		pal.error
	} else if warnings > 0 {
		pal.warning
	} else {
		pal.success
	};
	status_dot(ui, color);
	ui.label(RichText::new(if job.failed { "Failed" } else { "Done" }).size(12.5).strong().color(color));
	ui.add(Label::new(RichText::new(&job.summary).size(12.5).color(pal.text)).truncate())
		.on_hover_text(&job.summary);
	ui.label(theme::mono(human_time(job.duration())).color(pal.muted()));
	if errors > 0 || warnings > 0 {
		let plural = |count: usize, one: &str| if count == 1 { format!("1 {one}") } else { format!("{count} {one}s") };
		let text = match (errors, warnings) {
			(0, w) => plural(w, "warning"),
			(e, 0) => plural(e, "error"),
			(e, w) => format!("{}, {}", plural(e, "error"), plural(w, "warning")),
		};
		if lines_cleared {
			ui.label(theme::mono(text).color(pal.muted())).on_hover_text("Cleared from the log");
		} else if ui.small_button(text).on_hover_text("Show them in the log").clicked() {
			*open_log = true;
		}
	}
}

fn progress_bar(ui: &mut egui::Ui, fraction: f32, width: f32) {
	let pal = theme::pal(ui.ctx());
	let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 6.0), Sense::hover());
	ui.painter().rect_filled(rect, CornerRadius::same(3), pal.surface_hi);
	let filled = rect.width() * fraction.clamp(0.0, 1.0);
	if filled > 0.5 {
		let bar = Rect::from_min_size(rect.min, Vec2::new(filled, rect.height()));
		ui.painter().rect_filled(bar, CornerRadius::same(3), pal.accent);
	}
}

#[cfg(test)]
mod recent_tests {
	use super::*;

	#[test]
	fn distinct_parents_tells_twins_apart() {
		let paths: Vec<PathBuf> = [
			"/games/Destiny/Data/Data.pak",
			"/games/Gamez/Data/Data.pak",
			"/games/Destiny/Data/Items.pak",
			"/a/x/Client",
			"/b/x/Client",
		]
		.iter()
		.map(PathBuf::from)
		.collect();
		let got = distinct_parents(&paths);
		assert_eq!(got[0].as_deref(), Some("Destiny/Data"));
		assert_eq!(got[1].as_deref(), Some("Gamez/Data"));
		assert_eq!(got[2], None);
		assert_eq!(got[3].as_deref(), Some("a/x"));
		assert_eq!(got[4].as_deref(), Some("b/x"));
	}
}
