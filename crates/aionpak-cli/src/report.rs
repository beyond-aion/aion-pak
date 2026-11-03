use crate::Common;
use aionpak_commons::progress::Progress;
use indicatif::{HumanBytes, ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct Display {
	stop: Arc<AtomicBool>,
	thread: Option<JoinHandle<()>>,
}

impl Display {
	pub fn start(progress: &Arc<Progress>, quiet: bool) -> Self {
		let stop = Arc::new(AtomicBool::new(false));
		if quiet {
			return Display { stop, thread: None };
		}
		let progress = progress.clone();
		let flag = stop.clone();
		let thread = thread::spawn(move || {
			let bar = ProgressBar::with_draw_target(Some(0), ProgressDrawTarget::stderr_with_hz(8));
			bar.set_style(
				ProgressStyle::with_template(
					"{spinner} {elapsed_precise} [{wide_bar}] {bytes}/{total_bytes} {binary_bytes_per_sec} ETA {eta}\n  {msg}",
				)
				.unwrap()
				.progress_chars("=> "),
			);
			let tick = || {
				bar.set_length(progress.bytes_total.load(Relaxed));
				bar.set_position(progress.bytes_done.load(Relaxed));
				let archives = progress.archives_total.load(Relaxed);
				let mut msg = format!(
					"{} / {} files",
					progress.files_done.load(Relaxed),
					progress.files_total.load(Relaxed)
				);
				if archives > 0 {
					msg += &format!(", {} / {archives} archives", progress.archives_done.load(Relaxed));
				}
				let errors = progress.error_count();
				if errors > 0 {
					msg += &format!(", {errors} errors");
				}
				let current = progress.current();
				if !current.is_empty() {
					msg += &format!("  {current}");
				}
				bar.set_message(msg);
			};
			while !flag.load(Relaxed) {
				tick();
				thread::sleep(Duration::from_millis(100));
			}
			tick();
			bar.finish_and_clear();
		});
		Display {
			stop,
			thread: Some(thread),
		}
	}

	pub fn finish(mut self) {
		self.stop.store(true, Relaxed);
		if let Some(t) = self.thread.take() {
			let _ = t.join();
		}
	}
}

impl Drop for Display {
	fn drop(&mut self) {
		self.stop.store(true, Relaxed);
		if let Some(t) = self.thread.take() {
			let _ = t.join();
		}
	}
}

/// Writes a line to stderr. Unlike `eprintln!` it does not panic when stderr is a pipe nobody reads any more.
pub fn say(line: impl std::fmt::Display) {
	let _ = writeln!(std::io::stderr().lock(), "{line}");
}

/// Prints problems and totals; returns whether the run had no errors.
pub fn summary(
	progress: &Progress,
	elapsed: Duration,
	common: &Common,
	result: anyhow::Result<()>,
	compare: bool,
) -> anyhow::Result<bool> {
	let problems = progress.problems();
	let log = common
		.log
		.as_ref()
		.map(std::fs::File::create)
		.transpose()?
		.map(Mutex::new);
	let mut warnings = 0;
	for p in &problems {
		let line = format!("{}: {}", if p.error { "error" } else { "warning" }, p.what);
		if p.error || common.verbose {
			say(&line);
		}
		if !p.error {
			warnings += 1;
		}
		if let Some(f) = &log {
			writeln!(f.lock().unwrap(), "{line}")?;
		}
	}
	result?;
	let errors = problems.len() - warnings;
	let files = progress.files_done.load(Relaxed);
	let bytes = progress.bytes_done.load(Relaxed);
	let archives = progress.archives_done.load(Relaxed);
	if compare {
		if errors == 0 {
			say("no differences");
		} else {
			say(format_args!("{errors} differences"));
		}
		return Ok(errors == 0);
	}
	if files == 0 && archives == 0 {
		if errors > 0 || warnings > 0 {
			say(format_args!("{errors} errors, {warnings} warnings"));
		}
		return Ok(errors == 0);
	}
	let secs = elapsed.as_secs_f64().max(1e-3);
	let mut line = format!("{files} files, {}", HumanBytes(bytes));
	if archives > 0 {
		line += &format!(", {archives} archives");
	}
	line += &format!(
		" in {:.2} s ({}/s)",
		elapsed.as_secs_f64(),
		HumanBytes((bytes as f64 / secs) as u64)
	);
	let decrypted = progress.decrypted.load(Relaxed);
	if decrypted > 0 {
		line += &format!(", {decrypted} decrypted");
	}
	if errors > 0 || warnings > 0 {
		line += &format!("; {errors} errors, {warnings} warnings");
		if warnings > 0 && !common.verbose && common.log.is_none() {
			line += " (-v or --log to see them)";
		}
	}
	say(&line);
	Ok(errors == 0)
}
