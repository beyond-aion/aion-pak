#![cfg_attr(windows, windows_subsystem = "windows")]

mod app;
mod audio;
mod browse;
mod clock;
mod compare_view;
#[cfg(target_os = "linux")]
mod desktop;
mod folder;
mod forms;
mod icons;
mod jobs;
mod model;
mod open;
mod preview;
mod settings;
mod theme;
mod units;

use settings::Settings;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

const COMMANDS: [&str; 10] = [
	"list", "extract", "pack", "repack", "decode", "encode", "verify", "compare", "optimize", "help",
];

fn main() -> ExitCode {
	let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
	if is_command(&args) {
		// The same executable is the command line tool; only the program name in `--help` would give it away.
		if let Some(first) = args.first_mut() {
			*first = "aionpak".into();
		}
		#[cfg(windows)]
		attach_console();
		return aionpak_cli::main_with_args(args);
	}

	// Anything else is a path to open: the first argument that is not an option.
	let path = args
		.iter()
		.skip(1)
		.find(|arg| !arg.to_string_lossy().starts_with('-'))
		.map(PathBuf::from);
	// Windows tries wgpu first and falls back to OpenGL; wgpu reports a failure, while OpenGL can only panic.
	// AIONPAK_RENDERER=opengl skips Direct3D, for systems where it misbehaves.
	#[cfg(windows)]
	let result = if std::env::var("AIONPAK_RENDERER").is_ok_and(|v| v.eq_ignore_ascii_case("opengl")) {
		run(path, eframe::Renderer::Glow)
	} else {
		run(path.clone(), eframe::Renderer::Wgpu).or_else(|err| {
			eprintln!("aionpak: Direct3D could not start ({err}), trying OpenGL");
			run(path, eframe::Renderer::Glow)
		})
	};
	#[cfg(not(windows))]
	let result = run(path, eframe::Renderer::Glow);
	match result {
		Ok(()) => ExitCode::SUCCESS,
		Err(err) => {
			eprintln!("aionpak: {err}");
			ExitCode::from(2)
		}
	}
}

/// A command name or a help flag anywhere on the line means the user wants the CLI, not the window.
fn is_command(args: &[std::ffi::OsString]) -> bool {
	args.iter().skip(1).any(|arg| {
		let arg = arg.to_string_lossy();
		matches!(arg.as_ref(), "-h" | "--help" | "-V" | "--version")
			// A file or folder that happens to be called `list` is something to open, not a command.
			|| (COMMANDS.contains(&arg.as_ref()) && !Path::new(arg.as_ref()).exists())
	})
}

/// The binary is built as a GUI program, so it owns no console. When a command is run from a terminal, borrow the
/// terminal's console and point the standard streams at it, or the output would go nowhere.
#[cfg(windows)]
fn attach_console() {
	use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
	use windows_sys::Win32::Storage::FileSystem::{
		CreateFileW, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
	};
	use windows_sys::Win32::System::Console::{
		ATTACH_PARENT_PROCESS, AttachConsole, GetStdHandle, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
	};

	// SAFETY: plain Win32 calls on handles the process owns; the wide name is NUL-terminated.
	unsafe {
		let existing = GetStdHandle(STD_OUTPUT_HANDLE);
		if !existing.is_null() && existing != INVALID_HANDLE_VALUE {
			// Already redirected by the caller; leave that handle alone.
			return;
		}
		if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
			return;
		}
		let name: Vec<u16> = "CONOUT$\0".encode_utf16().collect();
		let handle = CreateFileW(
			name.as_ptr(),
			FILE_GENERIC_READ | FILE_GENERIC_WRITE,
			FILE_SHARE_READ | FILE_SHARE_WRITE,
			std::ptr::null(),
			OPEN_EXISTING,
			0,
			std::ptr::null_mut(),
		);
		if handle != INVALID_HANDLE_VALUE && !handle.is_null() {
			SetStdHandle(STD_OUTPUT_HANDLE, handle);
			SetStdHandle(STD_ERROR_HANDLE, handle);
		}
	}
}

/// Opens the window with the given renderer. Linux builds carry OpenGL only, which is a fraction of the size of
/// the wgpu stack; Windows builds carry both, see Cargo.toml.
fn run(path: Option<PathBuf>, renderer: eframe::Renderer) -> eframe::Result {
	let settings = Settings::load();
	// Before the window maps, so the compositor finds the icon the first time it looks.
	#[cfg(target_os = "linux")]
	desktop::register();
	let mut viewport = egui::ViewportBuilder::default()
		.with_title("Aion Pak")
		// Wayland shows the icon of the .desktop file with this name, which `desktop::register` writes.
		.with_app_id("aionpak")
		.with_decorations(false)
		// Windows: a window that may be translucent is always composited by the system. An opaque one can be
		// handed to the display directly when maximised, as a game is, and then the drivers' variable refresh
		// (G-Sync, FreeSync) follows its frames, which stop when nothing moves: the screen flickers. Every frame
		// paints the whole window opaque, so nothing shows through.
		.with_transparent(cfg!(windows))
		.with_min_inner_size([900.0, 580.0])
		.with_icon(Arc::new(theme::app_icon(128)));
	viewport = match settings.window {
		Some((width, height, x, y)) => viewport
			.with_inner_size([width.max(900.0), height.max(580.0)])
			.with_position([x, y]),
		None => viewport.with_inner_size([1200.0, 760.0]),
	};
	let options = eframe::NativeOptions {
		renderer,
		centered: settings.window.is_none(),
		viewport,
		..Default::default()
	};
	eframe::run_native(
		"aionpak",
		options,
		Box::new(move |cc| Ok(Box::new(app::Shell::new(cc, settings, path)))),
	)
}
