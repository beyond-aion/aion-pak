# aionpak-gui — look and feel

Every colour comes from a `Palette` in `theme.rs`, built from two choices the user makes in Settings and that
`settings.rs` stores in `gui.conf` (`%APPDATA%\aionpak\`, `~/.config/aionpak/`):

- **Appearance**: `System`, `Light` or `Dark`. System follows the OS and re-applies when the OS switches.
- **Accent**: Blue (default), Aether, Gold, Violet or Ember. Each has a shade tuned for the light base and one
  for the dark base, so contrast holds on both.

The bases are slate (a graphite with a faint cool tint) and cool grey-white; the accent is the only colour
that changes.

## Roles

`bg` window and rail · `surface` cards and inputs · `surface_hi` hover, wells, segmented controls · `border`
hairlines · `text` / `text_dim` / `muted()` · `accent` primary action, focus, selection tint, progress ·
`accent_alt` the few places two kinds sit side by side (sound files in the table) · `success` / `warning` /
`error` · `brand` and `crystal` the mark.

## Layout

- Title bar (ours, 44 px): drawer handle, mark, wordmark, the open path; minimise / maximise / close on the right.
- Rail: the six work screens at the top, Help / Settings / About at the bottom. The drawer handle folds it to
  icons only (60 px, tooltips) instead of hiding it.
- Content sits on a sheet with a rounded top-left corner. Every work screen is a title, one line saying what it
  is for, then cards of options and one accent button.
- Status bar (36 px): a dot and "Ready", the running job with progress and Cancel, or the last result. The log
  opens from its right end.
- Browse starts empty with two buttons and the recent archives; the toolbar appears once something is open. The
  details pane appears only when a row is selected, beside the table on wide windows and under it on tall ones.

## Rules

- Ornament (the mark, the wordmark) never sits behind data. Data is neutral.
- One filled accent button per screen; everything else is a surface with a hairline.
- Choices of two or three go into a segmented control, not radio buttons.
- Paths are text fields: they can be typed or pasted, and the button beside them picks one.
- A disabled button says on hover what it is waiting for.
- Numbers are right-aligned and monospaced so columns can be compared by eye.
- Status is never colour alone: problems also say "error" or "warning".
- Columns drop from the right as the window narrows instead of scrolling sideways.
- State changes animate over 120–180 ms, and only state changes.

## Chrome

The window has no OS decorations. Dragging the title bar sends `ViewportCommand::StartDrag`, double-click
toggles maximise, and eight 6 px grips around the edges call `ViewportCommand::BeginResize`. The title bar's
text is not selectable, so a drag moves the window. Dropping files anywhere shows an overlay and opens the
first `.pak` or folder.

## Type

Cinzel only for the wordmark, IBM Plex Sans (400/500/600) for everything readable, IBM Plex Mono 12 for names,
sizes, CRCs and paths. 20 screen titles, 14 body, 13 notes, 12 mono, 11 captions.

## Icons

Section and file icons are painted from primitives in `icons.rs`; the mark and the app icon are polygons in
`theme.rs`: an A of two ice-white blades with an aether crystal in its counter, deep slate on the light base.
On Linux, `desktop.rs` writes the `.desktop` file and the icon on start, since Wayland gives a window no other
way to show its icon. `cargo run -p aionpak-gui --example
export_icon` writes the PNGs in `assets/icons` and `assets/aionpak.ico` from the same polygons.

## Renderer

Linux: OpenGL through glow only. egui draws a few thousand triangles a frame, which any GPU does in well under a
millisecond, and the wgpu stack would add about 5 MB for nothing visible. Windows: wgpu first (Direct3D 12, with
the WARP software adapter where there is no GPU driver), then OpenGL. A virtual machine or a remote desktop often
has no usable OpenGL, and glow cannot report that: it panics, so it is only the fallback.

## What to avoid

Bevels, embossing, gradients on panels, textures behind text, UI text under 11 px, glass blur over tables, neon
on every border. The Aion reference is a fantasy *frame*, not a fantasy *desk*.
