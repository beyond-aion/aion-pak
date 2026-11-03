//! Writes the icon the window draws to PNG files and to the Windows .ico, so the .exe carries the same mark.
//! Run `cargo run -p aionpak-gui --example export_icon` after changing the mark.

#[path = "../src/theme.rs"]
#[allow(dead_code)]
mod theme;

use std::io::Write;

fn main() {
	let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
	let out = root.join("assets/icons");
	std::fs::create_dir_all(&out).unwrap();
	for size in [16u32, 32, 48, 64, 128, 256, 512] {
		let icon = theme::app_icon(size);
		let path = out.join(format!("aionpak-{size}.png"));
		image::save_buffer(&path, &icon.rgba, size, size, image::ColorType::Rgba8).unwrap();
		println!("{}", path.display());
	}

	let ico = root.join("assets/aionpak.ico");
	std::fs::create_dir_all(ico.parent().unwrap()).unwrap();
	let sizes = [16u32, 32, 48, 64, 128, 256];
	let images: Vec<(u32, Vec<u8>)> = sizes
		.iter()
		.map(|&size| (size, bmp_entry(size, &theme::app_icon(size).rgba)))
		.collect();
	let mut file = std::io::BufWriter::new(std::fs::File::create(&ico).unwrap());
	file.write_all(&0u16.to_le_bytes()).unwrap();
	file.write_all(&1u16.to_le_bytes()).unwrap();
	file.write_all(&(images.len() as u16).to_le_bytes()).unwrap();
	let mut offset = 6 + 16 * images.len() as u32;
	for (size, data) in &images {
		let dimension = if *size >= 256 { 0 } else { *size as u8 };
		file.write_all(&[dimension, dimension, 0, 0]).unwrap();
		file.write_all(&1u16.to_le_bytes()).unwrap();
		file.write_all(&32u16.to_le_bytes()).unwrap();
		file.write_all(&(data.len() as u32).to_le_bytes()).unwrap();
		file.write_all(&offset.to_le_bytes()).unwrap();
		offset += data.len() as u32;
	}
	for (_, data) in &images {
		file.write_all(data).unwrap();
	}
	println!("{}", ico.display());
}

/// One ICO entry: a BITMAPINFOHEADER, the BGRA rows bottom-up, and an all-zero AND mask.
fn bmp_entry(size: u32, rgba: &[u8]) -> Vec<u8> {
	let mask_row = size.div_ceil(32) * 4;
	let mut out = Vec::with_capacity(40 + (size * size * 4) as usize + (mask_row * size) as usize);
	out.extend_from_slice(&40u32.to_le_bytes());
	out.extend_from_slice(&(size as i32).to_le_bytes());
	out.extend_from_slice(&((size * 2) as i32).to_le_bytes());
	out.extend_from_slice(&1u16.to_le_bytes());
	out.extend_from_slice(&32u16.to_le_bytes());
	out.extend_from_slice(&0u32.to_le_bytes());
	out.extend_from_slice(&(size * size * 4).to_le_bytes());
	for _ in 0..4 {
		out.extend_from_slice(&0u32.to_le_bytes());
	}
	for y in (0..size).rev() {
		for x in 0..size {
			let i = ((y * size + x) * 4) as usize;
			out.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
		}
	}
	out.resize(out.len() + (mask_row * size) as usize, 0);
	out
}
