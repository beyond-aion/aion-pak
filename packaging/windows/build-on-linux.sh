#!/bin/sh
# Builds the Windows programs on Linux: target/x86_64-pc-windows-msvc/release/aionpak.exe and aionpak-cli.exe,
# both with the icon. Needs clang, lld and llvm (clang-cl, lld-link, llvm-lib, llvm-rc), the Rust target
# (rustup target add x86_64-pc-windows-msvc) and the MSVC CRT and Windows SDK unpacked by xwin
# (cargo install xwin; xwin --accept-license splat --output ~/.xwin). Set XWIN to use another folder.
set -e
xwin="${XWIN:-$HOME/.xwin}"
export CC_x86_64_pc_windows_msvc=clang-cl
export CXX_x86_64_pc_windows_msvc=clang-cl
export AR_x86_64_pc_windows_msvc=llvm-lib
export CFLAGS_x86_64_pc_windows_msvc="--target=x86_64-pc-windows-msvc -Wno-unused-command-line-argument \
/imsvc $xwin/crt/include /imsvc $xwin/sdk/include/ucrt /imsvc $xwin/sdk/include/um /imsvc $xwin/sdk/include/shared"
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_LINKER=lld-link
export CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS="-Lnative=$xwin/crt/lib/x86_64 \
-Lnative=$xwin/sdk/lib/um/x86_64 -Lnative=$xwin/sdk/lib/ucrt/x86_64"
cd "$(dirname "$0")/../.."
exec cargo build --release --target x86_64-pc-windows-msvc "$@"
