use std::process::ExitCode;

/// Workers allocate from many threads at once, which musl's allocator serialises: the static Linux build runs up to
/// three times slower with it.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

fn main() -> ExitCode {
	aionpak_cli::main_with_args(std::env::args_os().collect())
}
