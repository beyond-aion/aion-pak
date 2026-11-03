//! Whole client folders: every archive is extracted into a folder of the same name (`x.pak/`) at the same place,
//! and every such folder packs back into the archive it came from.

use crate::error::{Error, Result};
use crate::ops::{self, ExtractOptions, PakOptions};
use crate::pak::PakReader;
use crate::progress::Progress;
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::Relaxed;

fn is_pak(name: &std::ffi::OsStr) -> bool {
	Path::new(name)
		.extension()
		.is_some_and(|x| x.eq_ignore_ascii_case("pak"))
}

/// Archives under `root`, relative to it, sorted.
pub fn find_paks(root: &Path) -> Result<Vec<PathBuf>> {
	let mut out = Vec::new();
	for e in walkdir::WalkDir::new(root).sort_by_file_name() {
		let e = e.map_err(|e| Error::Io(e.into()))?;
		if e.file_type().is_file() && is_pak(e.file_name()) {
			out.push(e.path().strip_prefix(root).unwrap_or(e.path()).to_path_buf());
		}
	}
	Ok(out)
}

/// Folders under `root` that stand for archives (`*.pak/`), relative to it, sorted. Their insides are not searched.
pub fn find_pak_dirs(root: &Path) -> Result<Vec<PathBuf>> {
	let mut out = Vec::new();
	let mut it = walkdir::WalkDir::new(root).min_depth(1).sort_by_file_name().into_iter();
	while let Some(e) = it.next() {
		let e = e.map_err(|e| Error::Io(e.into()))?;
		if e.file_type().is_dir() && is_pak(e.file_name()) {
			out.push(e.path().strip_prefix(root).unwrap_or(e.path()).to_path_buf());
			it.skip_current_dir();
		}
	}
	Ok(out)
}

pub fn extract_tree(root: &Path, out: &Path, opts: &ExtractOptions, progress: &Progress) -> Result<()> {
	let paks = find_paks(root)?;
	extract_paks(root, out, &paks, opts, progress)
}

/// Extracts the given archives (paths relative to `root`) into `<out>/<path>/`, the layout `repack` expects.
pub fn extract_paks(
	root: &Path,
	out: &Path,
	paks: &[PathBuf],
	opts: &ExtractOptions,
	progress: &Progress,
) -> Result<()> {
	progress.archives_total.fetch_add(paks.len() as u64, Relaxed);
	let readers: Vec<_> = paks
		.par_iter()
		.filter_map(|rel| match PakReader::open(&root.join(rel)) {
			Ok(r) => {
				let (files, bytes) = ops::totals(&r, opts.filter);
				// A partial extraction leaves archives without a match alone, not even an empty folder.
				if files == 0 && opts.filter.is_some() {
					progress.archives_total.fetch_sub(1, Relaxed);
					return None;
				}
				progress.add_total(files, bytes);
				Some((rel, r))
			}
			Err(Error::NotArchive) => {
				progress.warn(format!("{}: not a zip archive, skipped", rel.display()));
				progress.archives_done.fetch_add(1, Relaxed);
				None
			}
			Err(e) => {
				progress.error(format!("{}: {e}", rel.display()));
				progress.archives_done.fetch_add(1, Relaxed);
				None
			}
		})
		.collect();
	readers.par_iter().try_for_each(|(rel, reader)| -> Result<()> {
		if progress.cancelled() {
			return Err(Error::Cancelled);
		}
		let label = rel.display().to_string();
		progress.set_current(&label);
		let target = out.join(rel);
		// Folders named after an archive are ours: with --force an old extraction is replaced, not merged into.
		if opts.force
			&& target.is_dir()
			&& let Err(e) = std::fs::remove_dir_all(&target)
		{
			progress.error(format!("{label}: {e}"));
			return Ok(());
		}
		if let Err(e) = ops::extract(reader, &label, &target, opts, progress) {
			if matches!(e, Error::Cancelled) {
				return Err(Error::Cancelled);
			}
			progress.error(format!("{label}: {e}"));
		}
		progress.archives_done.fetch_add(1, Relaxed);
		Ok(())
	})?;
	Ok(())
}

pub fn repack_tree(root: &Path, out: &Path, opts: &PakOptions, progress: &Progress) -> Result<()> {
	let dirs = find_pak_dirs(root)?;
	progress.archives_total.fetch_add(dirs.len() as u64, Relaxed);
	// Every folder is planned and counted first, so the progress runs against the whole tree from the start.
	let plans: Vec<(String, ops::PakPlan)> = dirs
		.par_iter()
		.filter_map(|rel| {
			let label = rel.display().to_string();
			match ops::plan_pak(&root.join(rel), &out.join(rel), opts, progress) {
				Ok(plan) => {
					let (files, bytes) = plan.totals();
					progress.add_total(files, bytes);
					Some((label, plan))
				}
				Err(e) => {
					progress.error(format!("{label}: {e}"));
					progress.archives_done.fetch_add(1, Relaxed);
					None
				}
			}
		})
		.collect();
	plans.into_par_iter().try_for_each(|(label, plan)| -> Result<()> {
		if progress.cancelled() {
			return Err(Error::Cancelled);
		}
		progress.set_current(&label);
		if let Err(e) = ops::write_pak(plan, opts, progress) {
			if matches!(e, Error::Cancelled) {
				return Err(Error::Cancelled);
			}
			progress.error(format!("{label}: {e}"));
		}
		progress.archives_done.fetch_add(1, Relaxed);
		Ok(())
	})?;
	Ok(())
}
