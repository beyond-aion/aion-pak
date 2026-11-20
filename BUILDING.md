# Building Aion Pak

You need [Rust](https://rustup.rs) 1.100 or newer and a C compiler (for libdeflate).

```sh
cargo build --release
```

The programs land in `target/release/`.

- **Windows:** use the MSVC toolchain (`rustup default stable-msvc`); the `.exe` files carry the app icon.
- **Linux:** install `gcc` or `clang` (`build-essential` on Debian and Ubuntu). For a command line tool that
  runs on any distribution, build it statically (`musl-tools` on Debian and Ubuntu):

  ```sh
  rustup target add x86_64-unknown-linux-musl
  cargo build --release -p aionpak-cli --target x86_64-unknown-linux-musl
  ```

  The desktop app stays a normal build: it loads the system's OpenGL and Wayland or X11 libraries at run time.

- **Windows programs from Linux:** `packaging/windows/build-on-linux.sh` cross-builds both `.exe` files with
  clang-cl and the MSVC libraries unpacked by [xwin](https://github.com/Jake-Shadle/xwin); the script lists what
  it needs.

The archive format and the job machinery live in `crates/aionpak-commons`, the command line in
`crates/aionpak-cli`, and the desktop app in `crates/aionpak-gui`.

## Release files

Besides the desktop app, Releases carry `aionpak-cli.exe`, `aionpak-cli-linux` and `aionpak-cli-linux-static`,
the command line without the window. The static one needs no system libraries at all, so it runs on any Linux
distribution and in minimal containers; the other uses the system's glibc and allocator, which take less memory
on large jobs.

## Notes

- **XML forms.** The client stores XML and HTML in four ways: compiled (binary, the fastest to load), encrypted
  (scrambled with a key from the file name, so such a file must not be renamed), UTF-16 text and 8-bit text.
  Pack and Optimize compile XML and store HTML as text by default; level files cannot be compiled and stay text.
  Convert (`encode`) compiles XML too and leaves HTML as it is unless you choose UTF-16 text.
- **Linux desktop integration.** On start the app adds itself to the application menu with its icon
  (`~/.local/share/applications/aionpak.desktop`), because on Wayland the task bar takes the icon from there.
