# CLAUDE.md — Wutils architecture & conventions

Guidance for working in this repo. Read this before changing the taskbar hook,
the shell extensions, or the registry layout.

## What this is

A tray-resident Windows 11 utilities app. Performance-first native stack:
**Rust + Slint** (Fluent style), no web runtime. Target: `x86_64-pc-windows-msvc`
only. Personal tool, not code-signed.

## Why this stack

Two of the three features need code running **inside `explorer.exe`**:
context-menu entries are in-process COM DLLs, and the taskbar move is a hook
DLL. That rules out .NET (unsupported for in-proc shell extensions) and Go
(runtime inside a host process). Rust gives native DLLs with memory safety, one
language for the app + DLLs + updater, and a good base for the planned file
browser. The `windows` crate covers Win32/COM/DisplayConfig/registry.

## Workspace layout

```
crates/
  core/      wutils-core   — shared Win32 helpers (no deps beyond `windows`)
  shell/     wutils_shell.dll   — in-proc COM context-menu handlers
  taskbar/   wutils_taskbar.dll — hook DLL loaded into the shell
  app/       wutils.exe    — Slint UI + tray + feature orchestration + updater
xtask/       `cargo xtask dist` release packaging
assets/      app icon
```

`default-members` excludes `xtask` and `core` from bare `cargo build` noise;
`cargo build` builds the exe + both DLLs. **The three output files must ship in
the same directory** — the app resolves the DLLs next to its own exe
(`features::exe_dir`).

Build config of note (`.cargo/config.toml`): `+crt-static` so the DLLs load into
Explorer without needing a VC++ redistributable. Release profile uses fat LTO.

## Hard invariants

- **Never panic inside a DLL.** Both DLLs run in `explorer.exe`. Hook/COM entry
  points use `catch_unwind` or return error HRESULTs. No blocking work in
  `QueryContextMenu`.
- **CLSIDs are frozen.** `core::clsid` holds the COM class IDs written into the
  user's registry. Changing one orphans existing registrations. Add new GUIDs,
  don't edit old ones.
- **Per-user only.** All registry writes go to `HKCU` (`core::reg` only opens
  `HKEY_CURRENT_USER`). No elevation, ever. (The one non-registry system
  object is the start-with-Windows logon task, registered for the current
  user's SID only — a standard user may do that.)
- **Idempotent apply.** `features::apply_*` run on startup *and* on every change;
  they must converge, not accumulate.

## Feature internals

### Taskbar (`crates/taskbar`, `app/src/{inject,symbols,features}.rs`)

**The move itself** is driven by hooking `TrayUI::_SetStuckMonitor` in
`Taskbar.dll` — the internal function that sticks the primary taskbar to a
monitor. That's the one that actually relocates the bar; the user32 hooks below
only make the flyouts / Start menu follow it. `_SetStuckMonitor` isn't exported,
so `app/src/symbols.rs` resolves its RVA from Microsoft's public symbols:

1. Read `Taskbar.dll`'s CodeView record (PDB name/GUID/age) from its PE.
2. Download the matching PDB from the MS symbol server (the box has no
   `symsrv.dll`, so DbgHelp can't fetch it; we download it with `ureq` and the
   `Microsoft-Symbol-Server/...` user-agent — note the server 302-redirects to
   Azure blob storage, so TLS must work: ureq uses **rustls**, see below).
3. Resolve `TrayUI::_SetStuckMonitor` with the system `dbghelp.dll` against that
   local PDB. The RVA is cached in `%LOCALAPPDATA%\Wutils\symbols\` per PDB id,
   so the ~7 MB download happens once per Taskbar.dll version (the PDB is
   written via a `.partial` rename and deleted once the RVA is cached).

`features::apply_taskbar` can block for seconds (that download, remote-thread
waits), so callers use `apply_taskbar_async`: one worker thread, queued
requests collapse to the newest settings.

The RVA goes to the DLL via shared memory; the DLL hooks `Taskbar.dll + rva`.
The `_SetStuckMonitor` detour forces the configured monitor when enabled, and —
crucially — the **real OS primary when disabled**: once moved, the taskbar keeps
asking for the secondary, so disable must actively pull it back, not pass the
taskbar's own (now secondary) choice through.

**Supporting user32 hooks** make other shell parts agree on "primary":
`MonitorFromPoint`/`MonitorFromRect` (called with `{0,0}`), `EnumDisplayDevicesW`
(`DISPLAY_DEVICE_PRIMARY_DEVICE`), and `MonitorFromWindow` in
`StartMenuExperienceHost`. They answer with the configured monitor **except**
for `shell32.dll` callers (desktop icons/wallpaper). All hooks use MinHook.

**Self-initializing injection.** The DLL installs its own hooks from a thread
spawned in `DllMain` (`install_once`), so injection (`inject.rs`) is just a
remote `LoadLibraryW` — no `CreateRemoteThread` onto an export. This matters:
calling an export by RVA computed from the on-disk DLL while a *stale/rebuilt*
copy is still mapped in Explorer would jump to a mismatched address and **crash
the shell** (that was the original "nothing happens" — Explorer crashed and
silently restarted). Config is published first through the `.shared` section
(`RWS`), visible to every process that maps the DLL; the app writes it by
loading the same DLL and calling `wutils_configure`.

**The section belongs to the DLL *file*.** The shell keeps whichever copy it
loaded first (another install folder, `target\debug`) until it restarts, and
injection skips a process that already has *any* `wutils_taskbar.dll` (two
copies would stack hooks). So `features::configure_taskbar` writes to our copy
**and** every copy found in the shell (`inject::loaded_copies`) — otherwise an
app run from a different folder silently configures a section nobody reads.

**Re-applying.** The taskbar only re-sticks in response to a display-change
nudge delivered *from inside its own process*. So: the DLL's install thread
sends it once after hooking, and a **watcher thread** in the DLL re-sends it
whenever the app bumps `generation` in shared memory (enable/disable/monitor
change). A cross-process nudge from the app alone does **not** work.

**Surviving Explorer restarts.** When Explorer restarts (including when the
*classic-menu* feature restarts it) the DLL and tray icon are gone. The tray's
hidden top-level window (not message-only — those don't get broadcasts) listens
for the `TaskbarCreated` broadcast and re-adds the icon + re-injects
(`tray.rs` → `on_shell_restart`, run directly on the tray thread, not via the
Slint loop, so it works when started minimized).

Targets: `explorer.exe`, `ShellHost.exe` (24H2), `StartMenuExperienceHost.exe`.

**Fragile:** depends on undocumented shell internals and a private symbol.
Technique credit: m417z's "Primary taskbar on secondary monitor" Windhawk mod. A
Windows update changes the `_SetStuckMonitor` RVA (re-resolved per version, so
fine) and could change which APIs the shell uses. The lock-screen-on-wrong-
monitor case isn't handled (`.shared` `locked` is always 0).

**Dev note:** a loaded DLL can't be overwritten, so rebuilding the taskbar DLL
requires freeing it first — restart `explorer.exe` and kill any `ShellHost.exe` /
`StartMenuExperienceHost.exe` still holding `wutils_taskbar.dll`.

### Context menu & HDR (`crates/shell`, `app/src/shellreg.rs`)
Each menu item is its own COM class (`IShellExtInit` + `IContextMenu`) so the app
can enable/disable them independently just by (un)registering under
`HKCU\Software\Classes`. The DLL reads no config. These are *classic* handlers —
they appear in the Windows 10 menu / "Show more options", not the new menu.

- **HDR** (`hdr.rs`): on `DesktopBackground`. Reads the clicked display from the
  cursor position, toggles via DisplayConfig. HDR read/write lives in
  `core::display`, which prefers the 24H2 `GET_ADVANCED_COLOR_INFO_2` /
  `SET_HDR_STATE` device-info calls (declared manually — not yet in the
  `windows` crate) and falls back to the older advanced-color calls.
- **Copy as path** (`copy_path.rs`) and **Compress to ZIP** (`zip.rs`): on
  `AllFilesystemObjects`. ZIP reuses Windows' own ZIP folder drop handler
  (`BHID_SFUIObject` → `IDropTarget`) so you get the native progress UI.

Classic-menu switch: `HKCU\...\CLSID\{86ca1aa0-...}\InprocServer32` with an empty
default shadows the Win11 menu handler. Requires an Explorer restart (the UI
prompts via a banner).

### Updater (`app/src/update.rs`)
Off unless enabled. Reads `releases/latest`, compares semver to
`CARGO_PKG_VERSION`, downloads `wutils-x64.zip`, verifies its SHA-256 against the
`.sha256` asset, then spawns a detached PowerShell script that waits for the app
to exit, `Expand-Archive`s over the install dir and relaunches. Download and
Explorer restart run off the UI thread.

Explorer usually still has the DLLs mapped, and a mapped file can't be
overwritten — only renamed. The script renames them to `<dll>.<guid>.old`
first (restoring them if extraction fails); `update::remove_stale_dlls` deletes
those on startup / shell restart. If the old `wutils_taskbar.dll` is still
mapped, Explorer runs the old hook *and reads the old file's `.shared`
section*, so the app shows a "Restart Explorer to finish updating" banner.

### App shell (`app/src/main.rs`, `tray.rs`, `model.rs`)
Slint runs the event loop (`run_event_loop_until_quit`, so closing the window to
tray doesn't exit). Single instance per session: `tray::claim_single_instance`
holds the `Local\Wutils.SingleInstance` mutex; a second launch posts
`WM_SHOW_EXISTING` to the running instance's tray window and exits. The tray is a Win32 `Shell_NotifyIcon` on its own thread
with a hidden top-level window (icon = resource `IDI_APPICON` = 1 from
`wutils.rc`, loaded at `SM_CXSMICON`); it talks to the UI only via
`slint::invoke_from_event_loop`. Worker threads (update check) never hold the
`Rc<State>` — cross-thread state is `Arc<Mutex>`/`Arc<Atomic>` in `Signals`.

### Start with Windows (`app/src/startup.rs`)
A per-user **logon scheduled task** (`Wutils (<username>)`, trigger + principal
= the user's SID, no delay, `Priority` 4, no time limit), *not* the `Run` key:
Explorer runs `Run` entries serially, waiting on each, so a `Run` launch came
minutes after sign-in. If task registration fails, it falls back to the `Run`
key; apply always leaves exactly one of the two (and removes both when off).
Registration is COM on its own MTA worker thread.

The task passes `--autostart` (older builds registered `--minimized`; both are
honored): start in the tray, and if Wutils is already running exit *quietly*
instead of showing the window. It also runs at above-normal priority until a
taskbar apply finds the shell up (`startup::boost` / `end_boost`).

Starting at sign-in usually means starting **before the shell**. So: the tray
retries `NIM_ADD` every second and, when it finally succeeds, runs the same
`on_shell_restart` path as `TaskbarCreated` (in case that broadcast went out
before our window existed); `on_shell_restart` applies twice, 5 s apart, to
catch ShellHost / StartMenuExperienceHost starting after the taskbar. And the
DLL may be injected into Explorer before it loads `Taskbar.dll`: its watcher
thread (explorer.exe only) keeps retrying the `_SetStuckMonitor` hook until
`Taskbar.dll` and the RVA are both present, then nudges once.

## Extending the UI

The UI is **data-driven** — add features without touching `app.slint`:
1. Add fields to `settings.rs` (with a sensible `Default`).
2. Add rows in `model::pages` (`toggle` / choice / button / `header`). Give each
   a stable `id` like `area.name`.
3. Handle the `id` in `main::apply_toggle` / the choice/action callbacks, calling
   a `features::*` function.
4. Implement the effect in `features.rs` (idempotent).

Search and per-page rendering work automatically from the model. New *kinds* of
control (beyond toggle/choice/button/info/header) are the only reason to edit
`app.slint` / the `ItemKind` enum.

Colors: the brand accent is orange (`Theme.brand` #F7630C, the icon's color;
`Theme.accent*` for controls). std-widgets' `Switch` / primary `Button` follow
the *system* accent and Slint can't override it, so toggles and the banner
button are our own `ToggleSwitch` / `AccentButton`. Use those, not std ones,
for anything accent-colored.

## Commands

```
cargo build            # exe + both DLLs (debug)
cargo clippy --all-targets
cargo xtask dist       # release build + dist\wutils-x64.zip (+ .sha256)
```

Release: push a `vX.Y.Z` tag → `.github/workflows/release.yml` packages and
publishes the assets the updater expects.

## Gotchas

- The two DLL crates set crate-level `allow` for FFI clippy lints
  (`not_unsafe_ptr_arg_deref` etc.) — their exported signatures are fixed by the
  OS/COM ABI and can't be `unsafe fn`.
- `wutils_shell.dll` exports `DllGetClassObject`/`DllCanUnloadNow` as PRIVATE via
  `exports.def` (keeps them out of the import library).
- Editing a loaded DLL requires restarting Explorer (taskbar) or re-registering
  (shell ext) — a stale DLL stays mapped otherwise.
- `ureq` must use the **rustls** feature, not native-tls: ureq 3 selects the
  rustls provider by default and *panics at runtime* on an HTTPS call if rustls
  isn't enabled. That panic (on the startup thread, during the symbol download)
  was a silent app exit. rustls also bundles its own roots — no OS TLS dep.
- Diagnostics: set the `WUTILS_LOG` env var before launching to trace taskbar
  injection to `%LOCALAPPDATA%\Wutils\wutils.log`. The app manages a `.debug`
  marker file so the injected DLL logs too (it can't see the app's environment).
  Off by default.
