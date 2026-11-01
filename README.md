# Wutils

A fast, native Windows 11 utilities app. No Electron, no background web engine —
a single ~14 MB executable plus two small DLLs, written in Rust with a
[Slint](https://slint.dev) UI in the Windows 11 (Fluent) style. It lives in the
system tray and reapplies your settings at every sign-in.

> Built for Windows 11 x64. Personal tool — not code-signed or distributed.

## Features

### 1. Move the taskbar to another display
Shows the **primary** taskbar — tray icons, clock, Start, Search and all the
flyouts — on whichever display you pick, **without** changing which monitor is
primary for everything else (desktop icons and new windows stay put). Pick the
display in the UI; the change applies live, no sign-out needed.

### 2. Classic (Windows 10) context menu
One toggle brings back the old right-click menu in place of the compact Windows
11 one, and back again. Two optional extra items can be added to it:
- **Copy as path** — on files and folders, without holding Shift.
- **Compress to ZIP file** — the Windows 11 command, in the classic menu.

### 3. HDR toggle in the desktop menu
Adds an **HDR** entry to the desktop right-click menu that turns HDR on or off
for the display you clicked on. Displays that don't support HDR show a disabled
entry labelled *not supported*.

### 4. Auto-update (optional)
When enabled, Wutils checks its GitHub Releases on startup, verifies the
download's SHA-256, and installs it. Can be turned off; you can also check on
demand.

## Install / build

Requires the [Rust toolchain](https://rustup.rs) and the MSVC build tools.

```powershell
# Debug build
cargo build

# Optimized, packaged release -> dist\wutils-x64.zip (+ .sha256)
cargo xtask dist
```

Run `target\debug\wutils.exe` (or extract the dist zip and run `wutils.exe`).
The three files — `wutils.exe`, `wutils_shell.dll`, `wutils_taskbar.dll` — must
stay in the same folder.

## Usage

- Launch `wutils.exe`. It opens the settings window and adds a tray icon.
- Closing the window hides it to the tray; **right-click the tray icon** for
  *Open* / *Exit*.
- Toggle features in the UI. Everything is applied immediately, except the
  classic-menu switch, which asks you to **restart Explorer** (one click in the
  in-app banner).
- *General → Start Wutils with Windows* makes your settings reapply at sign-in.

Settings are stored at `%APPDATA%\Wutils\settings.toml`.

## How it works (short version)

- **Taskbar** — a small DLL (`wutils_taskbar.dll`) is loaded into the shell
  processes and makes the taskbar treat your chosen monitor as primary. This is
  the same technique used by [Windhawk](https://windhawk.net) and
  ExplorerPatcher. It relies on Windows shell internals, so a major Windows
  update could change the behaviour until Wutils is updated. The **first time**
  you enable it, Wutils downloads the matching `Taskbar.dll` debug symbols (~7 MB)
  from Microsoft's public symbol server to locate the relevant function; the
  result is cached, so it only happens once per Windows version.
- **Context menu & HDR** — `wutils_shell.dll` is a set of standard COM context-
  menu handlers, registered per-user under `HKCU` (no admin needed).
- **Classic menu** — a per-user registry key that shadows the Windows 11 menu's
  handler.

See [CLAUDE.md](CLAUDE.md) for the full architecture.

## Releasing

Push a `vX.Y.Z` tag (matching the version in `Cargo.toml`). GitHub Actions runs
`cargo xtask dist` and publishes `wutils-x64.zip` + `wutils-x64.zip.sha256`,
which the in-app updater consumes.

## Known limitations

- Windows 11 x64 only.
- Not code-signed: SmartScreen may warn on first run, and the HDR / context-menu
  items appear in the **classic** menu ("Show more options"), not the new
  Windows 11 menu.
- With the taskbar moved, the lock screen may appear on the taskbar's monitor
  rather than the real primary.

## Roadmap

- A faster file/directory browser to replace Explorer for browsing.
