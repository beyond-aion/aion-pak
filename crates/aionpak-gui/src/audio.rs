//! Sounds of the client in the preview: Ogg Vorbis and PCM WAV, decoded whole into 16-bit samples, and played
//! through what the system has without linking anything for it: PulseAudio's simple API loaded at run time on
//! Linux (PipeWire serves it too), the waveOut functions of winmm on Windows.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Longer sounds are cut: the preview is for listening in, and an ambient loop of ten minutes would take 100 MB.
const MAX_SECONDS: u64 = 180;
/// Columns of the waveform drawn in the pane.
pub const ENVELOPE: usize = 240;

pub struct Clip {
	samples: Arc<Vec<i16>>,
	pub rate: u32,
	pub channels: u16,
	/// The loudest sample, positive and negative, of each column of the waveform, from -1 to 1.
	pub envelope: Vec<(f32, f32)>,
	pub truncated: bool,
	playing: Mutex<Option<Playing>>,
}

struct Playing {
	stop: Arc<AtomicBool>,
	/// Milliseconds played so far, kept by the output thread; `u64::MAX` once it is over.
	played: Arc<AtomicU64>,
	started: Instant,
}

impl Clip {
	pub fn seconds(&self) -> f32 {
		let frames = self.samples.len() / self.channels.max(1) as usize;
		frames as f32 / self.rate.max(1) as f32
	}

	/// Where playback is, in seconds, or `None` when nothing plays.
	pub fn position(&self) -> Option<f32> {
		let guard = self.playing.lock().unwrap();
		let playing = guard.as_ref()?;
		let played = playing.played.load(Relaxed);
		if played == u64::MAX {
			return None;
		}
		// The output thread counts what it handed over; the clock is smoother and never ahead of the sound by more
		// than the buffer.
		Some(playing.started.elapsed().as_secs_f32().min(self.seconds()))
	}

	pub fn play(&self, from: f32) -> Result<(), String> {
		self.stop();
		let channels = self.channels.max(1) as usize;
		let first = ((from.max(0.0) * self.rate as f32) as usize * channels).min(self.samples.len());
		let stop = Arc::new(AtomicBool::new(false));
		let played = Arc::new(AtomicU64::new(0));
		let (samples, rate, chans) = (Arc::clone(&self.samples), self.rate, self.channels);
		let (thread_stop, thread_played) = (Arc::clone(&stop), Arc::clone(&played));
		let (ready_tx, ready_rx) = std::sync::mpsc::channel();
		std::thread::spawn(move || {
			let result = output::play(&samples[first..], rate, chans, &thread_stop, &thread_played, &ready_tx);
			thread_played.store(u64::MAX, Relaxed);
			if let Err(err) = result {
				let _ = ready_tx.send(Err(err));
			}
		});
		// Wait until the device opened, so an error shows instead of a silent progress bar.
		match ready_rx.recv_timeout(Duration::from_secs(3)) {
			Ok(Ok(())) | Err(_) => {}
			Ok(Err(err)) => return Err(err),
		}
		let started = Instant::now() - Duration::from_secs_f32(from.max(0.0));
		*self.playing.lock().unwrap() = Some(Playing { stop, played, started });
		Ok(())
	}

	pub fn stop(&self) {
		if let Some(playing) = self.playing.lock().unwrap().take() {
			playing.stop.store(true, Relaxed);
		}
	}
}

impl Drop for Clip {
	fn drop(&mut self) {
		self.stop();
	}
}

pub fn is_sound(name: &str) -> bool {
	let lower = name.to_ascii_lowercase();
	lower.ends_with(".ogg") || lower.ends_with(".wav")
}

pub fn decode(data: &[u8]) -> Result<Clip, String> {
	let (samples, rate, channels, truncated) = if data.starts_with(b"OggS") {
		ogg(data)?
	} else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WAVE" {
		wav(data)?
	} else {
		return Err("neither Ogg Vorbis nor WAV".into());
	};
	if samples.is_empty() {
		return Err("no samples".into());
	}
	let envelope = envelope(&samples, channels as usize);
	Ok(Clip {
		samples: Arc::new(samples),
		rate,
		channels,
		envelope,
		truncated,
		playing: Mutex::new(None),
	})
}

fn ogg(data: &[u8]) -> Result<(Vec<i16>, u32, u16, bool), String> {
	let mut reader =
		lewton::inside_ogg::OggStreamReader::new(std::io::Cursor::new(data)).map_err(|e| format!("Ogg Vorbis: {e}"))?;
	let rate = reader.ident_hdr.audio_sample_rate;
	let channels = reader.ident_hdr.audio_channels as u16;
	let limit = MAX_SECONDS as usize * rate as usize * channels as usize;
	let mut samples = Vec::new();
	let mut truncated = false;
	while let Some(packet) = reader.read_dec_packet_itl().map_err(|e| format!("Ogg Vorbis: {e}"))? {
		samples.extend_from_slice(&packet);
		if samples.len() >= limit {
			samples.truncate(limit);
			truncated = true;
			break;
		}
	}
	Ok((samples, rate, channels, truncated))
}

/// PCM only, 8, 16, 24 or 32 bits: what the client ships.
fn wav(data: &[u8]) -> Result<(Vec<i16>, u32, u16, bool), String> {
	let u16_at = |at: usize| u16::from_le_bytes([data[at], data[at + 1]]);
	let u32_at = |at: usize| u32::from_le_bytes([data[at], data[at + 1], data[at + 2], data[at + 3]]);
	let mut at = 12;
	let mut format = None;
	while at + 8 <= data.len() {
		let id = &data[at..at + 4];
		let size = u32_at(at + 4) as usize;
		let body = at + 8;
		let end = body.saturating_add(size).min(data.len());
		if id == b"fmt " && end - body >= 16 {
			format = Some((u16_at(body), u16_at(body + 2), u32_at(body + 4), u16_at(body + 14)));
		} else if id == b"data" {
			let (tag, channels, rate, bits) = format.ok_or("WAV: data before its format")?;
			if tag != 1 && tag != 0xFFFE {
				return Err(format!("WAV: format {tag} is not PCM"));
			}
			let bytes = &data[body..end];
			let width = (bits as usize).div_ceil(8);
			let limit = MAX_SECONDS as usize * rate as usize * channels as usize;
			let count = (bytes.len() / width.max(1)).min(limit);
			let samples = (0..count)
				.map(|i| {
					let s = &bytes[i * width..i * width + width];
					match width {
						1 => ((s[0] as i16) - 128) << 8,
						2 => i16::from_le_bytes([s[0], s[1]]),
						3 => i16::from_le_bytes([s[1], s[2]]),
						_ => i16::from_le_bytes([s[2], s[3]]),
					}
				})
				.collect();
			return Ok((samples, rate, channels, bytes.len() / width.max(1) > limit));
		}
		at = body + size + (size & 1);
	}
	Err("WAV: no data".into())
}

fn envelope(samples: &[i16], channels: usize) -> Vec<(f32, f32)> {
	let frames = samples.len() / channels.max(1);
	let per = frames.div_ceil(ENVELOPE).max(1);
	(0..ENVELOPE)
		.map(|column| {
			let from = (column * per * channels).min(samples.len());
			let to = ((column + 1) * per * channels).min(samples.len());
			let (mut low, mut high) = (0i16, 0i16);
			for &s in &samples[from..to] {
				low = low.min(s);
				high = high.max(s);
			}
			(low as f32 / 32768.0, high as f32 / 32768.0)
		})
		.collect()
}

#[cfg(target_os = "linux")]
mod output {
	use std::ffi::{c_char, c_int, c_void};
	use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
	use std::sync::mpsc::Sender;

	#[repr(C)]
	struct SampleSpec {
		format: c_int,
		rate: u32,
		channels: u8,
	}

	const PA_STREAM_PLAYBACK: c_int = 1;
	const PA_SAMPLE_S16LE: c_int = 3;

	type New = unsafe extern "C" fn(
		*const c_char,
		*const c_char,
		c_int,
		*const c_char,
		*const c_char,
		*const SampleSpec,
		*const c_void,
		*const c_void,
		*mut c_int,
	) -> *mut c_void;
	type Write = unsafe extern "C" fn(*mut c_void, *const c_void, usize, *mut c_int) -> c_int;
	type Simple = unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int;
	type Free = unsafe extern "C" fn(*mut c_void);

	pub fn play(
		samples: &[i16],
		rate: u32,
		channels: u16,
		stop: &AtomicBool,
		played: &AtomicU64,
		ready: &Sender<Result<(), String>>,
	) -> Result<(), String> {
		// SAFETY: a system library with a stable C interface; the signatures are those of pulse/simple.h.
		unsafe {
			let library = libloading::Library::new("libpulse-simple.so.0")
				.map_err(|_| "no sound output: libpulse-simple.so.0 (PulseAudio or PipeWire) is not installed".to_owned())?;
			let new: libloading::Symbol<New> = library.get(b"pa_simple_new\0").map_err(|e| e.to_string())?;
			let write: libloading::Symbol<Write> = library.get(b"pa_simple_write\0").map_err(|e| e.to_string())?;
			let drain: libloading::Symbol<Simple> = library.get(b"pa_simple_drain\0").map_err(|e| e.to_string())?;
			let flush: libloading::Symbol<Simple> = library.get(b"pa_simple_flush\0").map_err(|e| e.to_string())?;
			let free: libloading::Symbol<Free> = library.get(b"pa_simple_free\0").map_err(|e| e.to_string())?;
			let spec = SampleSpec {
				format: PA_SAMPLE_S16LE,
				rate,
				channels: channels as u8,
			};
			let mut error = 0;
			let stream = new(
				std::ptr::null(),
				c"Aion Pak".as_ptr(),
				PA_STREAM_PLAYBACK,
				std::ptr::null(),
				c"preview".as_ptr(),
				&spec,
				std::ptr::null(),
				std::ptr::null(),
				&mut error,
			);
			if stream.is_null() {
				return Err(format!("no sound output: PulseAudio error {error}"));
			}
			let _ = ready.send(Ok(()));
			// A twentieth of a second at a time, so Stop is heard at once.
			let chunk = (rate as usize / 20).max(1) * channels.max(1) as usize;
			let mut done = 0usize;
			for part in samples.chunks(chunk) {
				if stop.load(Relaxed) {
					flush(stream, &mut error);
					free(stream);
					return Ok(());
				}
				if write(stream, part.as_ptr().cast(), part.len() * 2, &mut error) < 0 {
					free(stream);
					return Err(format!("sound output failed: PulseAudio error {error}"));
				}
				done += part.len();
				played.store((done as u64 * 1000) / (rate as u64 * channels.max(1) as u64), Relaxed);
			}
			drain(stream, &mut error);
			free(stream);
			Ok(())
		}
	}
}

#[cfg(windows)]
mod output {
	use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};
	use std::sync::mpsc::Sender;
	use std::time::Duration;
	use windows_sys::Win32::Media::Audio::{
		CALLBACK_NULL, HWAVEOUT, WAVE_FORMAT_PCM, WAVE_MAPPER, WAVEFORMATEX, WAVEHDR, WHDR_DONE, waveOutClose,
		waveOutOpen, waveOutPrepareHeader, waveOutReset, waveOutUnprepareHeader, waveOutWrite,
	};

	pub fn play(
		samples: &[i16],
		rate: u32,
		channels: u16,
		stop: &AtomicBool,
		played: &AtomicU64,
		ready: &Sender<Result<(), String>>,
	) -> Result<(), String> {
		let align = channels * 2;
		let format = WAVEFORMATEX {
			wFormatTag: WAVE_FORMAT_PCM as u16,
			nChannels: channels,
			nSamplesPerSec: rate,
			nAvgBytesPerSec: rate * align as u32,
			nBlockAlign: align,
			wBitsPerSample: 16,
			cbSize: 0,
		};
		let mut device: HWAVEOUT = std::ptr::null_mut();
		// SAFETY: the format and the header outlive the device, which is reset and closed before they go.
		unsafe {
			if waveOutOpen(&mut device, WAVE_MAPPER, &format, 0, 0, CALLBACK_NULL) != 0 {
				return Err("no sound output device".into());
			}
			let _ = ready.send(Ok(()));
			let mut header = WAVEHDR {
				lpData: samples.as_ptr() as *mut u8,
				dwBufferLength: (samples.len() * 2) as u32,
				..Default::default()
			};
			let size = size_of::<WAVEHDR>() as u32;
			waveOutPrepareHeader(device, &mut header, size);
			waveOutWrite(device, &mut header, size);
			let started = std::time::Instant::now();
			loop {
				let flags = std::ptr::read_unaligned(std::ptr::addr_of!(header.dwFlags));
				if flags & WHDR_DONE != 0 {
					break;
				}
				if stop.load(Relaxed) {
					waveOutReset(device);
					break;
				}
				played.store(started.elapsed().as_millis() as u64, Relaxed);
				std::thread::sleep(Duration::from_millis(15));
			}
			waveOutUnprepareHeader(device, &mut header, size);
			waveOutClose(device);
		}
		Ok(())
	}
}

#[cfg(not(any(target_os = "linux", windows)))]
mod output {
	pub fn play(
		_: &[i16],
		_: u32,
		_: u16,
		_: &std::sync::atomic::AtomicBool,
		_: &std::sync::atomic::AtomicU64,
		_: &std::sync::mpsc::Sender<Result<(), String>>,
	) -> Result<(), String> {
		Err("no sound output on this system".into())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn reads_pcm_wav() {
		let mut data = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
		data.extend_from_slice(&16u32.to_le_bytes());
		data.extend_from_slice(&1u16.to_le_bytes());
		data.extend_from_slice(&1u16.to_le_bytes());
		data.extend_from_slice(&8000u32.to_le_bytes());
		data.extend_from_slice(&16000u32.to_le_bytes());
		data.extend_from_slice(&2u16.to_le_bytes());
		data.extend_from_slice(&16u16.to_le_bytes());
		data.extend_from_slice(b"data");
		data.extend_from_slice(&4u32.to_le_bytes());
		data.extend_from_slice(&1000i16.to_le_bytes());
		data.extend_from_slice(&(-2000i16).to_le_bytes());
		let clip = decode(&data).unwrap();
		assert_eq!((clip.rate, clip.channels), (8000, 1));
		assert_eq!(clip.samples.as_slice(), &[1000, -2000]);
	}
}
