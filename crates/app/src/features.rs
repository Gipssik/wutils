//! Turns `Settings` into real system state: injecting the taskbar hook,
//! (un)registering context-menu extensions, the classic-menu switch, and the
//! run-at-login entry. Each function is idempotent so it can run on startup and
//! on every settings change.

use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{self, Sender};

use windows::Win32::Foundation::RECT;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::core::{HSTRING, Result, s};
use wutils_core::clsid;
use wutils_core::display::{self, Monitor};

use crate::settings::Settings;
use crate::{inject, shellreg, symbols};

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "Wutils";

/// Directory the running executable lives in; DLLs sit next to it.
pub fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)).unwrap_or_else(|| PathBuf::from("."))
}

pub fn shell_dll() -> PathBuf {
    exe_dir().join("wutils_shell.dll")
}

pub fn taskbar_dll() -> PathBuf {
    exe_dir().join("wutils_taskbar.dll")
}

/// Writes the taskbar config into the DLL's shared section (visible to every
/// process that has the DLL mapped, including Explorer).
fn configure_taskbar(enabled: bool, rva: u32, rect: RECT) {
    type ConfigureFn = unsafe extern "system" fn(u32, u32, u32, *const RECT);
    unsafe {
        let Ok(module) = LoadLibraryW(&HSTRING::from(taskbar_dll().as_os_str())) else {
            wutils_core::log::line("configure: LoadLibraryW(taskbar dll) FAILED");
            return;
        };
        let Some(proc) = GetProcAddress(module, s!("wutils_configure")) else {
            wutils_core::log::line("configure: GetProcAddress(wutils_configure) FAILED");
            return;
        };
        let configure: ConfigureFn = std::mem::transmute(proc as *const c_void);
        configure(enabled as u32, 0, rva, &rect);
        wutils_core::log::line(&format!(
            "configure: enabled={enabled} rva={rva:#x} rect=({},{},{},{})",
            rect.left, rect.top, rect.right, rect.bottom
        ));
    }
}

/// Finds the configured monitor, preferring the stable device path and falling
/// back to the first non-primary display.
fn target_monitor(settings: &Settings, monitors: &[Monitor]) -> Option<Monitor> {
    if !settings.taskbar.monitor_device_path.is_empty()
        && let Some(m) = monitors.iter().find(|m| m.device_path == settings.taskbar.monitor_device_path) {
            return Some(m.clone());
        }
    monitors.iter().find(|m| !m.primary).or_else(|| monitors.first()).cloned()
}

/// Injects/updates or ejects the taskbar hook to match the setting.
pub fn apply_taskbar(settings: &Settings) -> Result<()> {
    let monitors = display::monitors();
    let target = target_monitor(settings, &monitors);
    let enabled = settings.taskbar.move_to_secondary && target.is_some();
    wutils_core::log::line(&format!(
        "apply_taskbar: move={} monitors={} target={:?} -> enabled={enabled}",
        settings.taskbar.move_to_secondary,
        monitors.len(),
        target.as_ref().map(|m| m.label()),
    ));

    if enabled {
        // Resolve the internal function that relocates the bar (cached after the
        // first run). Without it the bar won't move, so skip enabling.
        let rva = symbols::set_stuck_monitor_rva().unwrap_or(0);
        let rect = target.unwrap().rect;
        configure_taskbar(true, rva, rect);
        inject::inject_all(&taskbar_dll())?;
        // Nudge in case the DLL was already loaded (its install thread only runs
        // on first load); harmless otherwise.
        wutils_core::explorer::notify_display_changed();
    } else {
        // Leave the DLL loaded but dormant: with enabled=0 its hooks pass through,
        // so the bar returns to the real primary on the next display-change.
        configure_taskbar(false, 0, RECT::default());
        wutils_core::explorer::notify_display_changed();
    }
    Ok(())
}

/// Runs `apply_taskbar` on a single worker thread. The first enable downloads
/// symbols and injection waits on remote threads, so neither should block the
/// UI. Queued requests collapse to the newest settings, so the user's last
/// change always wins.
pub fn apply_taskbar_async(settings: Settings) {
    static WORKER: OnceLock<Sender<Settings>> = OnceLock::new();
    let tx = WORKER.get_or_init(|| {
        let (tx, rx) = mpsc::channel::<Settings>();
        std::thread::spawn(move || {
            while let Ok(mut latest) = rx.recv() {
                while let Ok(newer) = rx.try_recv() {
                    latest = newer;
                }
                let _ = apply_taskbar(&latest);
            }
        });
        tx
    });
    let _ = tx.send(settings);
}

/// Registers/unregisters the three context-menu extensions. The HDR item and
/// the extra classic-menu items are independent toggles.
pub fn apply_context_extensions(settings: &Settings) -> Result<()> {
    let dll = shell_dll();
    shellreg::set_handler(&clsid::HDR_MENU, settings.hdr.menu_entry, &dll)?;
    shellreg::set_handler(&clsid::COPY_PATH_MENU, settings.context_menu.copy_as_path, &dll)?;
    shellreg::set_handler(&clsid::ZIP_MENU, settings.context_menu.compress_zip, &dll)?;
    Ok(())
}

/// Applies the classic-menu switch. Returns whether Explorer must restart for
/// the change to show (it only matters when the value actually changed).
pub fn apply_classic_menu(settings: &Settings) -> Result<bool> {
    let changed = shellreg::is_classic_menu() != settings.context_menu.classic;
    shellreg::set_classic_menu(settings.context_menu.classic)?;
    Ok(changed)
}

pub fn set_startup(enabled: bool) -> Result<()> {
    if enabled {
        let exe = std::env::current_exe().unwrap_or_default();
        wutils_core::reg::set_string(RUN_KEY, Some(RUN_VALUE), &format!("\"{}\" --minimized", exe.display()))
    } else {
        wutils_core::reg::delete_value(RUN_KEY, RUN_VALUE)
    }
}

/// Applies everything that can be set without restarting Explorer, on startup.
/// (The classic-menu switch is applied separately so the UI can prompt.)
pub fn apply_all_quiet(settings: &Settings) {
    let _ = apply_context_extensions(settings);
    apply_taskbar_async(settings.clone());
    let _ = set_startup(settings.general.start_with_windows);
}
