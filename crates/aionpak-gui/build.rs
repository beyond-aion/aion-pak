//! Puts the app icon into the .exe file, so Explorer and the task bar show it.

fn main() {
	println!("cargo:rerun-if-changed=../../assets/aionpak.ico");
	// The target, not the machine running the build: a Windows build made on Linux needs the icon too.
	if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
		return;
	}
	let mut resource = winresource::WindowsResource::new();
	resource.set_icon("../../assets/aionpak.ico");
	resource.set("ProductName", "Aion Pak");
	resource.set("FileDescription", "Aion .pak archive browser");
	// A machine without a resource compiler (rc.exe, llvm-rc or windres) still builds, without the icon.
	if let Err(error) = resource.compile() {
		println!("cargo:warning=could not embed the icon: {error}");
	}
}
