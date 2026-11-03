use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// Counters shared between workers and whoever displays progress. Totals may grow while work runs.
#[derive(Default)]
pub struct Progress {
	pub files_total: AtomicU64,
	pub files_done: AtomicU64,
	pub bytes_total: AtomicU64,
	pub bytes_done: AtomicU64,
	pub archives_total: AtomicU64,
	pub archives_done: AtomicU64,
	/// Encrypted XML and HTML files stored as text instead (`--decrypt`).
	pub decrypted: AtomicU64,
	/// Set by whoever displays the work; the loops stop at the next item.
	cancel: AtomicBool,
	current: Mutex<String>,
	problems: Mutex<Vec<Problem>>,
}

#[derive(Clone, Debug)]
pub struct Problem {
	pub error: bool,
	pub what: String,
}

impl Progress {
	pub fn add_total(&self, files: u64, bytes: u64) {
		self.files_total.fetch_add(files, Relaxed);
		self.bytes_total.fetch_add(bytes, Relaxed);
	}

	pub fn advance(&self, files: u64, bytes: u64) {
		self.files_done.fetch_add(files, Relaxed);
		self.bytes_done.fetch_add(bytes, Relaxed);
	}

	pub fn set_current(&self, what: impl Into<String>) {
		*self.current.lock().unwrap() = what.into();
	}

	pub fn current(&self) -> String {
		self.current.lock().unwrap().clone()
	}

	pub fn error(&self, what: impl Into<String>) {
		self.problems.lock().unwrap().push(Problem {
			error: true,
			what: what.into(),
		});
	}

	pub fn warn(&self, what: impl Into<String>) {
		self.problems.lock().unwrap().push(Problem {
			error: false,
			what: what.into(),
		});
	}

	pub fn problems(&self) -> Vec<Problem> {
		self.problems.lock().unwrap().clone()
	}

	/// Problems added since `from`, so a display can stream them without copying the whole list each time.
	pub fn problems_from(&self, from: usize) -> Vec<Problem> {
		self.problems.lock().unwrap().iter().skip(from).cloned().collect()
	}

	pub fn problem_count(&self) -> usize {
		self.problems.lock().unwrap().len()
	}

	/// Asks the loops to stop at the next item. Work already in flight finishes.
	pub fn cancel(&self) {
		self.cancel.store(true, Relaxed);
	}

	pub fn cancelled(&self) -> bool {
		self.cancel.load(Relaxed)
	}

	pub fn error_count(&self) -> usize {
		self.problems.lock().unwrap().iter().filter(|p| p.error).count()
	}
}
