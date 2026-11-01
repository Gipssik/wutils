#![allow(clippy::not_unsafe_ptr_arg_deref, clippy::collapsible_if, clippy::missing_safety_doc)]
//! Injected into `explorer.exe`, `ShellHost.exe` and `StartMenuExperienceHost.exe`
//! to move the *primary* taskbar (tray, clock, flyouts) to another monitor
//! without changing the real primary display.
//!
//! How it works: the shell finds "the primary monitor" by asking for the monitor
//! at point (0,0) / an empty rect, and by looking for the display device flagged
//! `DISPLAY_DEVICE_PRIMARY_DEVICE`. We hook those public user32 APIs and answer
//! with the configured monitor instead. Callers in `shell32.dll` (desktop icons,
//! wallpaper) keep seeing the real primary. While the session is locked the
//! hooks pass through so the lock screen stays on the real primary.
//! (Technique credit: m417z's "Primary taskbar on secondary monitor" Windhawk mod.)
//!
//! The app loads this same DLL file to write the config into the `.shared`
//! section (`wutils_configure`), then `LoadLibraryW`s it into the targets; the
//! DLL installs its hooks itself from a thread started in `DllMain`. Nothing
//! here may panic: a panic would take down Explorer.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU32, AtomicUsize, Ordering::*};

use minhook::MinHook;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::Diagnostics::Debug::RtlCaptureStackBackTrace;
use windows::Win32::System::LibraryLoader::*;
use windows::Win32::System::SystemServices::DLL_PROCESS_ATTACH;
use windows::Win32::System::Threading::{CreateThread, Sleep, THREAD_CREATE_RUN_IMMEDIATELY};
use windows::core::{BOOL, PCWSTR, s, w};

#[repr(C)]
struct Shared {
    /// Non-zero so the section is emitted as initialized data.
    magic: AtomicU32,
    enabled: AtomicU32,
    locked: AtomicU32,
    /// RVA of `TrayUI::_SetStuckMonitor` within `Taskbar.dll` (0 = unknown).
    set_stuck_rva: AtomicU32,
    /// Bumped by the app on every config change; the in-shell watcher thread
    /// re-applies when it sees this move (the taskbar only re-sticks in response
    /// to a display-change nudge delivered from inside its own process).
    generation: AtomicU32,
    left: AtomicI32,
    top: AtomicI32,
    right: AtomicI32,
    bottom: AtomicI32,
}

#[unsafe(link_section = ".shared")]
#[used]
static SHARED: Shared = Shared {
    magic: AtomicU32::new(0x5755_5449),
    enabled: AtomicU32::new(0),
    locked: AtomicU32::new(0),
    set_stuck_rva: AtomicU32::new(0),
    generation: AtomicU32::new(0),
    left: AtomicI32::new(0),
    top: AtomicI32::new(0),
    right: AtomicI32::new(0),
    bottom: AtomicI32::new(0),
};

static INSTALLED: AtomicBool = AtomicBool::new(false);

static ORIG_MONITOR_FROM_POINT: AtomicUsize = AtomicUsize::new(0);
static ORIG_MONITOR_FROM_RECT: AtomicUsize = AtomicUsize::new(0);
static ORIG_MONITOR_FROM_WINDOW: AtomicUsize = AtomicUsize::new(0);
static ORIG_ENUM_DISPLAY_DEVICES: AtomicUsize = AtomicUsize::new(0);
static ORIG_SET_STUCK_MONITOR: AtomicUsize = AtomicUsize::new(0);

type MonitorFromPointFn = unsafe extern "system" fn(POINT, MONITOR_FROM_FLAGS) -> HMONITOR;
type MonitorFromRectFn = unsafe extern "system" fn(*const RECT, MONITOR_FROM_FLAGS) -> HMONITOR;
type MonitorFromWindowFn = unsafe extern "system" fn(HWND, MONITOR_FROM_FLAGS) -> HMONITOR;
type EnumDisplayDevicesFn = unsafe extern "system" fn(PCWSTR, u32, *mut DISPLAY_DEVICEW, u32) -> BOOL;
/// `long TrayUI::_SetStuckMonitor(HMONITOR)` — `this` in rcx, monitor in rdx.
type SetStuckMonitorFn = unsafe extern "system" fn(*mut c_void, HMONITOR) -> i32;

/// Trampoline to the original function, or the unhooked API when it isn't hooked in this process.
macro_rules! original {
    ($slot:ident, $ty:ty, $api:expr) => {{
        let p = $slot.load(Acquire);
        if p == 0 {
            $api as $ty
        } else {
            unsafe { std::mem::transmute::<usize, $ty>(p) }
        }
    }};
}

/// Return address of the hooked function's caller — i.e. the shell code that
/// called the API. Used to attribute the call to a module (`shell32`, the Start
/// menu). Emitted as a macro, not a function, so `RtlCaptureStackBackTrace` runs
/// directly in the hook frame and `FramesToSkip = 1` reliably lands on the
/// hook's caller regardless of inlining.
macro_rules! caller {
    () => {{
        let mut frames = [std::ptr::null_mut(); 1];
        unsafe { RtlCaptureStackBackTrace(1, &mut frames, None) };
        frames[0] as *const c_void
    }};
}

fn module_of(addr: *const c_void) -> Option<HMODULE> {
    let mut module = HMODULE::default();
    unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            PCWSTR(addr.cast()),
            &mut module,
        )
        .ok()?;
    }
    Some(module)
}

fn is_module(module: Option<HMODULE>, name: PCWSTR) -> bool {
    match (module, unsafe { GetModuleHandleW(name) }) {
        (Some(m), Ok(n)) => m == n,
        _ => false,
    }
}

/// The configured monitor, regardless of who's asking. `None` when disabled,
/// the session is locked, or the rect maps to no monitor.
fn resolve_target() -> Option<HMONITOR> {
    if SHARED.enabled.load(Acquire) == 0 || SHARED.locked.load(Relaxed) != 0 {
        return None;
    }
    let rect = RECT {
        left: SHARED.left.load(Relaxed),
        top: SHARED.top.load(Relaxed),
        right: SHARED.right.load(Relaxed),
        bottom: SHARED.bottom.load(Relaxed),
    };
    let monitor_from_rect = original!(ORIG_MONITOR_FROM_RECT, MonitorFromRectFn, monitor_from_rect_api);
    let monitor = unsafe { monitor_from_rect(&rect, MONITOR_DEFAULTTONULL) };
    (!monitor.is_invalid()).then_some(monitor)
}

/// The monitor that should act as primary for this caller, if any.
fn target_monitor(caller: *const c_void) -> Option<HMONITOR> {
    // Cheap check before the module lookup: these hooks see every matching call.
    if SHARED.enabled.load(Acquire) == 0 {
        return None;
    }
    // Desktop icons and wallpaper handling live in shell32: leave them on the real primary.
    if is_module(module_of(caller), w!("shell32.dll")) {
        return None;
    }
    let monitor = resolve_target()?;
    // Log the first override per process so we can see the hook taking effect
    // without flooding the log on every call.
    if !LOGGED_OVERRIDE.swap(true, Relaxed) {
        wutils_core::log::line(&format!("override (in {}): -> monitor {:p}", process_name(), monitor.0));
    }
    Some(monitor)
}

static LOGGED_OVERRIDE: AtomicBool = AtomicBool::new(false);

unsafe extern "system" fn monitor_from_point_hook(pt: POINT, flags: MONITOR_FROM_FLAGS) -> HMONITOR {
    if pt.x == 0 && pt.y == 0 {
        if let Some(monitor) = target_monitor(caller!()) {
            return monitor;
        }
    }
    let original = original!(ORIG_MONITOR_FROM_POINT, MonitorFromPointFn, monitor_from_point_api);
    unsafe { original(pt, flags) }
}

unsafe extern "system" fn monitor_from_rect_hook(rect: *const RECT, flags: MONITOR_FROM_FLAGS) -> HMONITOR {
    if let Some(r) = unsafe { rect.as_ref() } {
        if r.left == 0 && r.top == 0 && r.right == 0 && r.bottom == 0 {
            if let Some(monitor) = target_monitor(caller!()) {
                return monitor;
            }
        }
    }
    let original = original!(ORIG_MONITOR_FROM_RECT, MonitorFromRectFn, monitor_from_rect_api);
    unsafe { original(rect, flags) }
}

/// Start menu only: it reads the taskbar alignment of the monitor its window is on,
/// resolved with MONITOR_DEFAULTTOPRIMARY. Point it at the taskbar's monitor.
unsafe extern "system" fn monitor_from_window_hook(hwnd: HWND, flags: MONITOR_FROM_FLAGS) -> HMONITOR {
    if flags == MONITOR_DEFAULTTOPRIMARY {
        let caller = caller!();
        let module = module_of(caller);
        if is_module(module, w!("StartMenu.dll")) || is_module(module, w!("StartDocked.dll")) {
            if let Some(monitor) = target_monitor(caller) {
                return monitor;
            }
        }
    }
    let original = original!(ORIG_MONITOR_FROM_WINDOW, MonitorFromWindowFn, monitor_from_window_api);
    unsafe { original(hwnd, flags) }
}

unsafe extern "system" fn enum_display_devices_hook(
    device: PCWSTR,
    index: u32,
    info: *mut DISPLAY_DEVICEW,
    flags: u32,
) -> BOOL {
    let original = original!(ORIG_ENUM_DISPLAY_DEVICES, EnumDisplayDevicesFn, enum_display_devices_api);
    let result = unsafe { original(device, index, info, flags) };
    if !result.as_bool() || !device.is_null() {
        return result;
    }
    let Some(info) = (unsafe { info.as_mut() }) else { return result };
    let Some(monitor) = target_monitor(caller!()) else { return result };

    let mut mi = MONITORINFOEXW::default();
    mi.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(monitor, &mut mi.monitorInfo) }.as_bool() {
        if info.DeviceName == mi.szDevice {
            info.StateFlags |= DISPLAY_DEVICE_PRIMARY_DEVICE;
        } else {
            info.StateFlags &= !DISPLAY_DEVICE_PRIMARY_DEVICE;
        }
    }
    result
}

/// The function that actually sticks the primary taskbar to a monitor. Replace
/// its monitor argument with the configured one, then call the original. This is
/// what relocates the bar; the user32 hooks only make the flyouts/Start menu
/// follow it.
unsafe extern "system" fn set_stuck_monitor_hook(this: *mut c_void, monitor: HMONITOR) -> i32 {
    let chosen = desired_monitor(monitor);
    if chosen != monitor {
        wutils_core::log::line(&format!("_SetStuckMonitor: in={:p} -> {:p}", monitor.0, chosen.0));
    }
    let original: SetStuckMonitorFn =
        unsafe { std::mem::transmute(ORIG_SET_STUCK_MONITOR.load(Acquire)) };
    unsafe { original(this, chosen) }
}

/// The monitor the primary taskbar should stick to. When enabled, our target;
/// when disabled, the real OS primary — because once the bar has been moved the
/// taskbar keeps asking for the secondary, so disable must actively pull it back
/// rather than pass the taskbar's own (now secondary) choice through.
fn desired_monitor(incoming: HMONITOR) -> HMONITOR {
    if SHARED.locked.load(Relaxed) != 0 {
        return incoming; // session locked: leave the shell alone.
    }
    if SHARED.enabled.load(Acquire) == 1 {
        resolve_target().unwrap_or(incoming)
    } else {
        real_primary()
    }
}

/// The real OS primary monitor (the hook passes `{0,0}` through when disabled).
fn real_primary() -> HMONITOR {
    let f = original!(ORIG_MONITOR_FROM_POINT, MonitorFromPointFn, monitor_from_point_api);
    unsafe { f(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) }
}

// Unhooked APIs with raw ABI signatures, used where a hook isn't installed in this process.
unsafe extern "system" fn monitor_from_point_api(pt: POINT, flags: MONITOR_FROM_FLAGS) -> HMONITOR {
    unsafe { MonitorFromPoint(pt, flags) }
}

unsafe extern "system" fn monitor_from_rect_api(rect: *const RECT, flags: MONITOR_FROM_FLAGS) -> HMONITOR {
    unsafe { MonitorFromRect(rect, flags) }
}

unsafe extern "system" fn monitor_from_window_api(hwnd: HWND, flags: MONITOR_FROM_FLAGS) -> HMONITOR {
    unsafe { MonitorFromWindow(hwnd, flags) }
}

unsafe extern "system" fn enum_display_devices_api(
    device: PCWSTR,
    index: u32,
    info: *mut DISPLAY_DEVICEW,
    flags: u32,
) -> BOOL {
    unsafe { EnumDisplayDevicesW(device, index, info, flags) }
}

fn process_name() -> String {
    let mut buf = [0u16; 260];
    let len = unsafe { GetModuleFileNameW(None, &mut buf) } as usize;
    let path = String::from_utf16_lossy(&buf[..len]);
    path.rsplit('\\').next().unwrap_or_default().to_ascii_lowercase()
}

fn install() -> u32 {
    let Ok(user32) = (unsafe { GetModuleHandleW(w!("user32.dll")) }) else { return 0 };
    let hooks: &[(windows::core::PCSTR, *mut c_void, &AtomicUsize)] = if process_name() == "startmenuexperiencehost.exe" {
        &[(s!("MonitorFromWindow"), monitor_from_window_hook as *mut c_void, &ORIG_MONITOR_FROM_WINDOW)]
    } else {
        &[
            (s!("MonitorFromPoint"), monitor_from_point_hook as *mut c_void, &ORIG_MONITOR_FROM_POINT),
            (s!("MonitorFromRect"), monitor_from_rect_hook as *mut c_void, &ORIG_MONITOR_FROM_RECT),
            (s!("EnumDisplayDevicesW"), enum_display_devices_hook as *mut c_void, &ORIG_ENUM_DISPLAY_DEVICES),
        ]
    };

    let mut installed = 0;
    for &(name, detour, slot) in hooks {
        let Some(target) = (unsafe { GetProcAddress(user32, name) }) else { continue };
        let target = target as *mut c_void;
        if hook_address(target, detour, slot) {
            installed += 1;
        } else {
            wutils_core::log::line(&format!("hook: {name:?} FAILED"));
        }
    }

    // The core move: hook TrayUI::_SetStuckMonitor in Taskbar.dll (only present
    // in the taskbar host process). Its RVA comes from the app via shared memory.
    let rva = SHARED.set_stuck_rva.load(Acquire);
    if rva != 0 {
        if let Ok(taskbar) = unsafe { GetModuleHandleW(w!("Taskbar.dll")) } {
            let target = (taskbar.0 as usize + rva as usize) as *mut c_void;
            if hook_address(target, set_stuck_monitor_hook as *mut c_void, &ORIG_SET_STUCK_MONITOR) {
                installed += 1;
                wutils_core::log::line(&format!("hook: _SetStuckMonitor @ {target:p} installed"));
            } else {
                wutils_core::log::line("hook: _SetStuckMonitor FAILED");
            }
        }
    }

    wutils_core::log::line(&format!("hook: installed {installed} hooks in {}", process_name()));
    installed
}

/// Creates and enables one MinHook detour, storing the trampoline in `slot`.
fn hook_address(target: *mut c_void, detour: *mut c_void, slot: &AtomicUsize) -> bool {
    match unsafe { MinHook::create_hook(target, detour) } {
        Ok(trampoline) => {
            slot.store(trampoline as usize, Release);
            unsafe { MinHook::enable_hook(target) }.is_ok()
        }
        Err(_) => false,
    }
}

/// Installs hooks once, nudges the taskbar to re-stick, and — in the taskbar
/// host — starts the watcher that re-applies on later config changes.
fn install_once() {
    if INSTALLED.swap(true, AcqRel) {
        return;
    }
    let _ = std::panic::catch_unwind(|| {
        install();
        // Trigger CTray::_HandleDisplayChange so _SetStuckMonitor runs with the
        // hook now active and the bar moves. Only the taskbar host has the window.
        wutils_core::explorer::notify_display_changed();
        // The watcher only matters where _SetStuckMonitor lives (the taskbar host).
        if ORIG_SET_STUCK_MONITOR.load(Acquire) != 0 {
            unsafe {
                let _ = CreateThread(None, 0, Some(watcher_thread), None, THREAD_CREATE_RUN_IMMEDIATELY, None);
            }
        }
    });
}

/// Polls the shared generation counter and re-applies (from inside the taskbar
/// host) whenever the app changes config — the only reliable way to make the
/// bar move on disable or a monitor change, not just on enable.
unsafe extern "system" fn watcher_thread(_: *mut c_void) -> u32 {
    let mut last = SHARED.generation.load(Acquire);
    loop {
        unsafe { Sleep(300) };
        let now = SHARED.generation.load(Acquire);
        if now != last {
            last = now;
            let _ = std::panic::catch_unwind(wutils_core::explorer::notify_display_changed);
        }
    }
}

/// Thread proc spawned from `DllMain`: installs the hooks once loader lock is
/// released. Self-initializing on load means injection is just `LoadLibraryW`
/// with no remote export calls — so a stale/rebuilt DLL can never be entered at
/// a mismatched address (which would crash the shell).
unsafe extern "system" fn install_thread(_: *mut c_void) -> u32 {
    install_once();
    0
}

/// Called by the app (in its own process) to publish config into the shared
/// section before injecting. Visible to every process that maps this DLL.
#[unsafe(no_mangle)]
pub extern "system" fn wutils_configure(enabled: u32, locked: u32, set_stuck_rva: u32, rect: *const RECT) {
    if let Some(r) = unsafe { rect.as_ref() } {
        SHARED.left.store(r.left, Relaxed);
        SHARED.top.store(r.top, Relaxed);
        SHARED.right.store(r.right, Relaxed);
        SHARED.bottom.store(r.bottom, Relaxed);
    }
    if set_stuck_rva != 0 {
        SHARED.set_stuck_rva.store(set_stuck_rva, Relaxed);
    }
    SHARED.locked.store(locked, Relaxed);
    SHARED.enabled.store(enabled, Release);
    // Bump last so the in-shell watcher re-applies (works cross-process: the
    // section is shared with every process that mapped this DLL).
    SHARED.generation.fetch_add(1, Release);
    wutils_core::log::line(&format!(
        "configure (in {}): enabled={enabled} locked={locked} rva={set_stuck_rva:#x}",
        process_name()
    ));
}

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: HINSTANCE, reason: u32, _: *mut c_void) -> BOOL {
    if reason == DLL_PROCESS_ATTACH {
        unsafe {
            let _ = DisableThreadLibraryCalls(module.into());
            // Install off the loader lock. Not in the app's own process — it
            // loads the DLL only to call wutils_configure.
            if process_name() != "wutils.exe" {
                wutils_core::log::line(&format!("DllMain attach in {}", process_name()));
                let _ = CreateThread(None, 0, Some(install_thread), None, THREAD_CREATE_RUN_IMMEDIATELY, None);
            }
        }
    }
    TRUE
}
