//! Reads every entry of every pak under a client folder and round-trips every XML through text.

use aionpak_commons::pak::{Codec, PakReader};
use aionpak_commons::xml::{self, Encoding, Form, text};
use rayon::prelude::*;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;
use walkdir::WalkDir;

#[derive(Default)]
struct Stats {
	paks: u64,
	entries: u64,
	bytes: u64,
	forms: BTreeMap<String, u64>,
	failures: Vec<String>,
}

fn main() {
	let root = PathBuf::from(std::env::args().nth(1).expect("client folder"));
	let paks: Vec<PathBuf> = WalkDir::new(&root)
		.into_iter()
		.filter_map(|e| e.ok())
		.filter(|e| e.path().extension().is_some_and(|x| x.eq_ignore_ascii_case("pak")))
		.map(|e| e.into_path())
		.collect();
	let start = Instant::now();
	let stats = Mutex::new(Stats::default());
	paks.par_iter().for_each_init(
		|| Codec::new(0),
		|codec, path| {
			let mut local = Stats {
				paks: 1,
				..Default::default()
			};
			let pak = match PakReader::open(path) {
				Ok(p) => p,
				Err(e) => {
					stats.lock().unwrap().failures.push(format!("{}: {e}", path.display()));
					return;
				}
			};
			let mut buf = Vec::new();
			for e in pak.entries() {
				local.entries += 1;
				let name = e.name_lossy().into_owned();
				if let Err(err) = pak.read_into(e, codec, &mut buf) {
					local.failures.push(format!("{}: {err}", path.display()));
					continue;
				}
				local.bytes += buf.len() as u64;
				if !xml::is_xml_name(&name) {
					continue;
				}
				let form = xml::detect(&buf);
				*local.forms.entry(form.name().to_owned()).or_default() += 1;
				let check = || -> Result<(), String> {
					match form {
						Form::Compiled => {
							for enc in [Encoding::Utf16, Encoding::Utf8] {
								let t = xml::render(&buf, enc).map_err(|e| e.to_string())?;
								if text::compile(&t).map_err(|e| e.to_string())? != buf {
									return Err(format!("{enc:?} text round-trip differs"));
								}
							}
							Ok(())
						}
						Form::Encrypted => xml::decode(&buf, &name, Encoding::Utf16)
							.map(|_| ())
							.map_err(|e| e.to_string()),
						_ => Ok(()),
					}
				};
				if let Err(msg) = check() {
					local.failures.push(format!("{} | {name}: {msg}", path.display()));
				}
			}
			let mut s = stats.lock().unwrap();
			s.paks += local.paks;
			s.entries += local.entries;
			s.bytes += local.bytes;
			for (k, v) in local.forms {
				*s.forms.entry(k).or_default() += v;
			}
			s.failures.extend(local.failures);
		},
	);
	let s = stats.into_inner().unwrap();
	println!(
		"{} paks, {} entries, {:.2} GiB in {:.1}s; xml forms {:?}; failures {}",
		s.paks,
		s.entries,
		s.bytes as f64 / (1u64 << 30) as f64,
		start.elapsed().as_secs_f64(),
		s.forms,
		s.failures.len()
	);
	let mut kinds = std::collections::BTreeMap::<String, (u64, String)>::new();
	for f in &s.failures {
		let msg = f.rsplit(": ").next().unwrap_or("").to_owned();
		let top = f.split(['\\', '|']).nth(2).unwrap_or("").to_owned();
		let k = kinds.entry(format!("{top} :: {msg}")).or_insert((0, f.clone()));
		k.0 += 1;
	}
	for (k, (n, ex)) in &kinds {
		println!("  {n:6} {k}   e.g. {ex}");
	}
}
