//! The floor for extraction: loads every entry of an archive tree into memory first, then times only writing the
//! same files under the same names (`real`), or as many files of the same sizes filled with one byte (`dummy`).

use aionpak_commons::pak::{Codec, PakReader};
use aionpak_commons::{names, tree};
use rayon::prelude::*;
use std::path::PathBuf;
use std::time::Instant;

fn main() {
	let args: Vec<String> = std::env::args().collect();
	let (src, out, real) = (
		PathBuf::from(&args[1]),
		PathBuf::from(&args[2]),
		args.get(3).is_some_and(|m| m == "real"),
	);
	let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
	for rel in tree::find_paks(&src).unwrap() {
		let Ok(p) = PakReader::open(&src.join(&rel)) else {
			continue;
		};
		let base = out.join(&rel);
		let read: Vec<_> = p
			.entries()
			.par_iter()
			.filter(|e| !e.is_dir())
			.map_init(
				|| Codec::new(0),
				|c, e| {
					let data = if real {
						p.read(e, c).unwrap()
					} else {
						vec![0x5a; e.size as usize]
					};
					(names::entry_path(&base, &e.name).unwrap(), data)
				},
			)
			.collect();
		files.extend(read);
	}
	let total: usize = files.iter().map(|f| f.1.len()).sum();
	let start = Instant::now();
	let mut dirs: Vec<_> = files
		.iter()
		.filter_map(|f| f.0.parent().map(|p| p.to_path_buf()))
		.collect();
	dirs.sort();
	dirs.dedup();
	for d in &dirs {
		std::fs::create_dir_all(d).unwrap();
	}
	files
		.par_iter()
		.for_each(|(path, data)| std::fs::write(path, data).unwrap());
	let secs = start.elapsed().as_secs_f64();
	println!(
		"{} files, {:.2} GiB in {secs:.1}s ({:.0} files/s, {:.0} MiB/s), {}",
		files.len(),
		total as f64 / (1u64 << 30) as f64,
		files.len() as f64 / secs,
		total as f64 / (1u64 << 20) as f64 / secs,
		if real {
			"real names and data"
		} else {
			"real names, dummy data"
		}
	);
}
