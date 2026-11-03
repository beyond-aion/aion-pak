//! Times the stages of turning compiled XML into text and back, single-threaded, on the given files.

use aionpak_commons::xml::{Encoding, render, text};
use std::time::Instant;

fn main() {
	let files: Vec<_> = std::env::args().skip(1).map(|p| std::fs::read(p).unwrap()).collect();
	let mut t_ = [0f64; 4];
	let mut bytes = 0;
	for data in &files {
		bytes += data.len();
		for (i, enc) in [Encoding::Utf16, Encoding::Utf8].into_iter().enumerate() {
			let t = Instant::now();
			let text = render(data, enc).unwrap();
			t_[2 * i] += t.elapsed().as_secs_f64();
			let t = Instant::now();
			let c = text::compile(&text).unwrap();
			t_[2 * i + 1] += t.elapsed().as_secs_f64();
			assert_eq!(&c, data);
		}
	}
	println!(
		"{} files, {:.1} MB compiled\n  utf16: render {:.3} s, compile {:.3} s\n  utf8: render {:.3} s, compile {:.3} s",
		files.len(),
		bytes as f64 / 1e6,
		t_[0],
		t_[1],
		t_[2],
		t_[3]
	);
}
