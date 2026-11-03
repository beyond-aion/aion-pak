//! Checking an archive the way a reader would: every header, and optionally every byte.

use crate::pak::{Codec, PakReader};
use crate::progress::Progress;
use rayon::prelude::*;

/// Reads every entry and reports what does not hold up.
///
/// `deep` decompresses the data and compares its CRC with the central directory; without it only the local
/// headers are followed, which catches a truncated or rewritten archive in a fraction of the time.
pub fn archive(pak: &PakReader, label: &str, deep: bool, progress: &Progress) {
	progress.set_current(label);
	if deep {
		pak.entries().par_iter().for_each_init(
			|| (Codec::new(0), Vec::new()),
			|(codec, buffer), entry| {
				if progress.cancelled() || entry.is_dir() {
					return;
				}
				if let Err(err) = pak.read_into(entry, codec, buffer) {
					progress.error(format!("{label}: {err}"));
				}
				progress.advance(1, entry.size as u64);
			},
		);
	} else {
		pak.entries().par_iter().for_each(|entry| {
			if progress.cancelled() || entry.is_dir() {
				return;
			}
			if let Err(err) = pak.stored(entry) {
				progress.error(format!("{label}: {err}"));
			}
			progress.advance(1, entry.size as u64);
		});
	}
}

/// Checks every archive in `paks`, labelled by their path relative to `root`. Every archive is opened first and
/// its size counted, so the progress runs against the whole set instead of growing with each archive and jumping
/// back. Archives that are not zip files are a warning, others that fail to open an error. Returns how many
/// archives were checked.
pub fn archives(
	root: &std::path::Path,
	paks: &[std::path::PathBuf],
	deep: bool,
	progress: &Progress,
) -> usize {
	use std::sync::atomic::Ordering::Relaxed;
	progress.archives_total.fetch_add(paks.len() as u64, Relaxed);
	let readers: Vec<(String, PakReader)> = paks
		.par_iter()
		.filter_map(|rel| {
			let label = rel.display().to_string();
			match PakReader::open(&root.join(rel)) {
				Ok(reader) => {
					let (files, bytes) = crate::ops::totals(&reader, None);
					progress.add_total(files, bytes);
					Some((label, reader))
				}
				Err(crate::Error::NotArchive) => {
					progress.warn(format!("{label}: not a zip archive, skipped"));
					progress.archives_done.fetch_add(1, Relaxed);
					None
				}
				Err(err) => {
					progress.error(format!("{label}: {err}"));
					progress.archives_done.fetch_add(1, Relaxed);
					None
				}
			}
		})
		.collect();
	readers.par_iter().for_each(|(label, reader)| {
		if progress.cancelled() {
			return;
		}
		progress.set_current(label);
		archive(reader, label, deep, progress);
		progress.archives_done.fetch_add(1, Relaxed);
	});
	readers.len()
}
