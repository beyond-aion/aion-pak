#!/bin/sh
# Installs aionpak for the current user from the repository after `cargo build --release`: both programs into
# ~/.local/bin, the menu entry and the icon. The app writes its menu entry and icon itself on start, so this is
# only needed to get the programs onto the PATH.
set -e
root=$(cd "$(dirname "$0")/../.." && pwd)
bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
install -Dm755 "$root/target/release/aionpak" "$bin/aionpak"
install -Dm755 "$root/target/release/aionpak-cli" "$bin/aionpak-cli"
install -Dm644 "$root/packaging/linux/aionpak.desktop" "$data/applications/aionpak.desktop"
for size in 16 32 48 64 128 256 512; do
	install -Dm644 "$root/assets/icons/aionpak-$size.png" "$data/icons/hicolor/${size}x${size}/apps/aionpak.png"
done
update-desktop-database "$data/applications" 2>/dev/null || true
echo "Installed to $bin; Aion Pak is in the application menu."
