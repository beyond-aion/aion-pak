//! Background work: one job at a time, the same `Progress` the command line uses, and a way to stop it.

use aionpak_commons::ops::{self, ExtractOptions, FlavorMode, PakOptions, XmlMode};
use aionpak_commons::pak::{Codec, PakReader};
use aionpak_commons::progress::Progress;
use aionpak_commons::xml::{self, Encoding};
use crate::browse::Stored;
use aionpak_commons::{compare, optimize, tree, verify};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Instant;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
	Verify,
	Forms,
	Extract,
	Convert,
	Pak,
	Compare,
	Optimize,
}

pub enum JobEvent {
	Done(Result<String, String>),
}

pub struct Job {
	pub kind: JobKind,
	pub title: String,
	pub progress: Arc<Progress>,
	pub started: Instant,
	pub finished: bool,
	pub finished_at: Option<Instant>,
	pub summary: String,
	pub failed: bool,
	pub events: Receiver<JobEvent>,
	/// The folder the job works in, if any. Its problems name files under it, and the log shows those paths
	/// relative to it, so the part that differs is not cut off.
	pub base: Option<PathBuf>,
}

impl Job {
	/// Seconds the job has run, frozen when it finished, so the status bar stops counting.
	pub fn duration(&self) -> f64 {
		match self.finished_at {
			Some(end) => end.duration_since(self.started).as_secs_f64(),
			None => self.started.elapsed().as_secs_f64(),
		}
	}

	pub fn rate(&self) -> f64 {
		let seconds = self.duration();
		if seconds <= 0.0 {
			return 0.0;
		}
		self.progress.bytes_done.load(Relaxed) as f64 / seconds
	}

	pub fn eta(&self) -> Option<f64> {
		if self.finished {
			return None;
		}
		let done = self.progress.bytes_done.load(Relaxed);
		let total = self.progress.bytes_total.load(Relaxed);
		if done == 0 || total <= done {
			return None;
		}
		let rate = self.rate();
		(rate > 1.0).then(|| (total - done) as f64 / rate)
	}

	pub fn fraction(&self) -> f32 {
		let done = self.progress.bytes_done.load(Relaxed);
		let total = self.progress.bytes_total.load(Relaxed);
		if total == 0 {
			return 0.0;
		}
		(done as f32 / total as f32).clamp(0.0, 1.0)
	}

	pub fn files(&self) -> (u64, u64) {
		(
			self.progress.files_done.load(Relaxed),
			self.progress.files_total.load(Relaxed),
		)
	}

	pub fn finish(&mut self, result: Result<String, String>) {
		self.finished = true;
		self.finished_at = Some(Instant::now());
		match result {
			Ok(summary) => self.summary = summary,
			Err(error) => {
				self.summary = error;
				self.failed = true;
			}
		}
	}
}

fn spawn<F>(kind: JobKind, title: impl Into<String>, base: Option<PathBuf>, work: F) -> Job
where
	F: FnOnce(Arc<Progress>) -> Result<String, String> + Send + 'static,
{
	let title = title.into();
	let progress = Arc::new(Progress::default());
	let (sender, events) = mpsc::channel();
	let worker_progress = Arc::clone(&progress);
	thread::spawn(move || {
		let result = work(worker_progress);
		let _ = sender.send(JobEvent::Done(result));
	});
	Job {
		kind,
		title,
		progress,
		started: Instant::now(),
		finished: false,
		finished_at: None,
		summary: String::new(),
		failed: false,
		events,
		base,
	}
}

/// Files and their unpacked bytes, the totals the progress bar of a check runs against.
fn file_totals(reader: &PakReader) -> (u64, u64) {
	reader
		.entries()
		.iter()
		.filter(|entry| !entry.is_dir())
		.fold((0, 0), |(files, bytes), entry| (files + 1, bytes + entry.size as u64))
}

/// Checks a single archive, or every archive under a folder, the way `aionpak verify <path>` does.
pub fn verify_path(path: PathBuf, deep: bool) -> Job {
	if path.is_dir() {
		return verify_folder(path, deep);
	}
	let label = path.display().to_string();
	spawn(JobKind::Verify, format!("checking {label}"), None, move |progress| {
		let reader = PakReader::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
		let (files, bytes) = file_totals(&reader);
		progress.add_total(files, bytes);
		progress.archives_total.store(1, Relaxed);
		verify::archive(&reader, &label, deep, &progress);
		progress.archives_done.store(1, Relaxed);
		let errors = progress.error_count();
		Ok(if progress.cancelled() {
			"cancelled".to_owned()
		} else if errors == 0 {
			"checked, no problems".to_owned()
		} else {
			format!("{errors} problems")
		})
	})
}

/// Checks every archive under a folder, the way `aionpak verify <folder>` does.
pub fn verify_folder(root: PathBuf, deep: bool) -> Job {
	let title = format!("checking {}", root.display());
	spawn(JobKind::Verify, title, Some(root.clone()), move |progress| {
		let paks = tree::find_paks(&root).map_err(|e| e.to_string())?;
		verify::archives(&root, &paks, deep, &progress);
		// A file the client never loads because a later archive has another copy is a warning, as in the CLI.
		for over in optimize::overrides(&root, &progress).map_err(|e| e.to_string())? {
			progress.warn(optimize::override_text(&root, &over));
		}
		let errors = progress.error_count();
		Ok(if progress.cancelled() {
			"cancelled".to_owned()
		} else if errors == 0 {
			format!("{} archives checked, no problems", paks.len())
		} else {
			format!("{} archives, {errors} problems", paks.len())
		})
	})
}

/// Reads every entry of an archive, or only its headers, and lets the problems land in the progress.
pub fn verify_archive(reader: Arc<PakReader>, label: String, deep: bool) -> Job {
	spawn(JobKind::Verify, format!("checking {label}"), None, move |progress| {
		let (files, bytes) = file_totals(&reader);
		progress.add_total(files, bytes);
		verify::archive(&reader, &label, deep, &progress);
		let errors = progress.error_count();
		if progress.cancelled() {
			return Ok("cancelled".to_owned());
		}
		Ok(if errors == 0 {
			format!("{files} entries checked, no problems")
		} else {
			format!("{files} entries, {errors} problems")
		})
	})
}

/// Reads the XML and HTML entries once and records the form each one is stored in.
pub fn scan_forms(reader: Arc<PakReader>, entries: Vec<(usize, String)>, forms: Arc<Vec<AtomicU8>>) -> Job {
	spawn(JobKind::Forms, "reading forms", None, move |progress| {
		let all = reader.entries();
		let (files, bytes) = entries
			.iter()
			.fold((0u64, 0u64), |(f, b), (index, _)| (f + 1, b + all[*index].size as u64));
		progress.add_total(files, bytes);

		entries.par_iter().for_each_init(
			|| (Codec::new(0), Vec::new()),
			|(codec, buffer), (index, name)| {
				if progress.cancelled() {
					return;
				}
				progress.set_current(name);
				match reader.read_into(&all[*index], codec, buffer) {
					Ok(()) => {
						let stored = Stored::from_form(xml::detect(buffer), buffer);
						forms[*index].store(stored as u8, Relaxed);
					}
					Err(err) => progress.error(format!("{name}: {err}")),
				}
				progress.advance(1, all[*index].size as u64);
			},
		);
		Ok(format!("{files} XML and HTML files read"))
	})
}

/// The options of one extraction, shared by the archive and folder sides of the workspace.
pub struct ExtractRequest {
	pub out: PathBuf,
	pub decode: Option<Encoding>,
	pub force: bool,
	/// Entry names to take; `None` takes everything that passes the archive's own filter.
	pub filter: Option<std::collections::HashSet<String>>,
}

pub fn extract_archive(reader: Arc<PakReader>, label: String, request: ExtractRequest) -> Job {
	let title = format!("extracting {label}");
	spawn(JobKind::Extract, title, None, move |progress| {
		let filter = request
			.filter
			.as_ref()
			.map(|names| move |name: &str| names.contains(name));
		let options = ExtractOptions {
			decode: request.decode,
			filter: filter.as_ref().map(|f| f as &ops::NameFilter),
			force: request.force,
		};
		let (files, bytes) = ops::totals(&reader, options.filter);
		progress.add_total(files, bytes);
		progress.archives_total.store(1, Relaxed);
		ops::extract(&reader, &label, &request.out, &options, &progress).map_err(|e| e.to_string())?;
		progress.archives_done.store(1, Relaxed);
		if progress.cancelled() {
			return Ok("cancelled".to_owned());
		}
		Ok(format!("{files} files into {}", request.out.display()))
	})
}

/// Extracts archives of a folder tree, each into `<name>.pak/` at its place relative to `root`: those in `paks`
/// (relative to `root`), or with `None` every archive in `under` (relative to `root`) and below it.
pub fn extract_folder(root: PathBuf, under: PathBuf, paks: Option<Vec<PathBuf>>, request: ExtractRequest) -> Job {
	let title = format!("extracting {}", root.join(&under).display());
	spawn(JobKind::Extract, title, Some(root.clone()), move |progress| {
		let paks = match paks {
			Some(paks) => paks,
			None => tree::find_paks(&root.join(&under))
				.map_err(|e| e.to_string())?
				.into_iter()
				.map(|pak| under.join(pak))
				.collect(),
		};
		let filter = request
			.filter
			.as_ref()
			.map(|names| move |name: &str| names.contains(name));
		let options = ExtractOptions {
			decode: request.decode,
			filter: filter.as_ref().map(|f| f as &ops::NameFilter),
			force: request.force,
		};
		tree::extract_paks(&root, &request.out, &paks, &options, &progress).map_err(|e| e.to_string())?;
		if progress.cancelled() {
			return Ok("cancelled".to_owned());
		}
		Ok(format!("{} archives into {}", paks.len(), request.out.display()))
	})
}

pub struct ConvertSpec {
	pub input: PathBuf,
	/// `None` converts the files in place.
	pub out: Option<PathBuf>,
	pub mode: ops::Convert,
}

pub fn convert(spec: ConvertSpec) -> Job {
	let title = format!("converting {}", spec.input.display());
	spawn(JobKind::Convert, title, None, move |progress| {
		ops::convert(&spec.input, spec.out.as_deref(), spec.mode, &progress).map_err(|e| e.to_string())?;
		if progress.cancelled() {
			return Ok("cancelled".to_owned());
		}
		let files = progress.files_done.load(Relaxed);
		Ok(format!("{files} files converted"))
	})
}

pub struct PakSpec {
	pub dir: PathBuf,
	pub out: PathBuf,
	pub level: u8,
	pub xml: XmlMode,
	/// HTML files' own mode, and whether the encrypted ones become text.
	pub html: (XmlMode, bool),
	pub flavor: FlavorMode,
	pub force: bool,
	pub recompress: bool,
	pub decrypt: bool,
	pub repack: bool,
}

pub fn pak(spec: PakSpec) -> Job {
	let title = format!("packing {}", spec.dir.display());
	spawn(JobKind::Pak, title, Some(spec.dir.clone()), move |progress| {
		let options = PakOptions {
			level: spec.level,
			xml: spec.xml,
			flavor: spec.flavor,
			force: spec.force,
			recompress: spec.recompress,
			decrypt: spec.decrypt,
			html: Some(spec.html),
		};
		if spec.repack {
			tree::repack_tree(&spec.dir, &spec.out, &options, &progress).map_err(|e| e.to_string())?;
		} else {
			progress.archives_total.store(1, Relaxed);
			ops::pak(&spec.dir, &spec.out, &options, &progress).map_err(|e| e.to_string())?;
			progress.archives_done.store(1, Relaxed);
		}
		if progress.cancelled() {
			return Ok("cancelled".to_owned());
		}
		let files = progress.files_done.load(Relaxed);
		Ok(format!("{files} files into {}", spec.out.display()))
	})
}

/// Leaves the report in `slot` for the Compare screen. Differences are not problems: the log gets the summary only,
/// and the status bar does not count them as errors.
pub fn compare(first: PathBuf, second: PathBuf, slot: crate::compare_view::Slot) -> Job {
	let title = format!("comparing {} with {}", first.display(), second.display());
	spawn(JobKind::Compare, title, None, move |progress| {
		progress.set_current("comparing the file lists");
		let report = compare::trees(&first, &second).map_err(|e| e.to_string())?;
		for pak in &report.paks {
			if let compare::PakStatus::Unreadable(why) = &pak.status {
				progress.error(format!("{}: {why}", pak.path));
			}
		}
		if progress.cancelled() {
			return Ok("cancelled".to_owned());
		}
		let differ = report
			.paks
			.iter()
			.filter(|p| p.status != compare::PakStatus::Same)
			.count();
		let summary = if differ == 0 {
			"no differences".to_owned()
		} else {
			let files: usize = report.paks.iter().map(|p| p.entries.len()).sum();
			format!("{differ} of {} archives differ, {files} files", report.paks.len())
		};
		*slot.lock().unwrap() = Some(report);
		Ok(summary)
	})
}

pub enum OptimizeMode {
	Copy { out: PathBuf },
	Apply { backup: Option<PathBuf> },
	Undo { backup: PathBuf },
}

pub struct OptimizeSpec {
	pub client: PathBuf,
	pub mode: OptimizeMode,
	pub level: Option<u8>,
	/// Bring every XML and HTML into this form; `None` leaves them as they are.
	pub xml: Option<xml::Form>,
	pub decrypt: bool,
	pub html: Option<(Option<xml::Form>, bool)>,
	pub volume_size: u64,
	pub recompress_rest: bool,
}

pub fn optimize(spec: OptimizeSpec) -> Job {
	let title = match &spec.mode {
		OptimizeMode::Copy { .. } => format!("building volumes from {}", spec.client.display()),
		OptimizeMode::Apply { .. } => format!("applying volumes to {}", spec.client.display()),
		OptimizeMode::Undo { .. } => format!("undoing in {}", spec.client.display()),
	};
	spawn(JobKind::Optimize, title, Some(spec.client.clone()), move |progress| {
		let OptimizeSpec {
			client,
			mode,
			level,
			xml,
			decrypt,
			html,
			volume_size,
			recompress_rest,
		} = spec;
		if let OptimizeMode::Undo { backup } = &mode {
			let (deleted, restored) = optimize::undo(&client, backup, &progress).map_err(|e| e.to_string())?;
			return Ok(format!("{deleted} volumes deleted, {restored} archives moved back"));
		}
		let options = optimize::OptimizeOptions {
			level,
			xml,
			decrypt,
			html,
			volume_size,
			recompress_rest,
		};
		let (target, backup) = match mode {
			OptimizeMode::Copy { out } => (out, None),
			OptimizeMode::Apply { backup } => {
				let stamp = std::time::SystemTime::now()
					.duration_since(std::time::UNIX_EPOCH)
					.map_or(0, |elapsed| elapsed.as_secs());
				let backup = backup.unwrap_or_else(|| client.join(format!("aionpak-backup-{stamp}")));
				(client.clone(), Some(backup))
			}
			OptimizeMode::Undo { .. } => unreachable!("handled above"),
		};
		let outcome =
			optimize::optimize(&client, &target, backup.as_deref(), &options, &progress).map_err(|e| e.to_string())?;
		if progress.cancelled() {
			return Ok("cancelled".to_owned());
		}
		let rest = if outcome.recompressed.is_empty() {
			String::new()
		} else {
			let (before, after) = outcome.recompressed_bytes;
			format!(
				"; {} more archives recompressed, {:.1} MiB -> {:.1} MiB",
				outcome.recompressed.len(),
				before as f64 / 1048576.0,
				after as f64 / 1048576.0
			)
		};
		Ok(match &backup {
			Some(backup) => format!(
				"{} volumes into the client, {} archives moved to {}{rest}",
				outcome.volumes.len(),
				outcome.replaced.len(),
				backup.display()
			),
			None => format!(
				"{} volumes in {} replace {} archives; the client was not touched{rest}",
				outcome.volumes.len(),
				target.display(),
				outcome.replaced.len()
			),
		})
	})
}
