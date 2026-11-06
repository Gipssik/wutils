//! Keep active: stops chat apps (Slack, Teams, …) from marking you "away" by
//! tapping F14 — a key nothing on Windows binds — once you've been idle for the
//! configured interval.
//!
//! Those apps decide you're away from the OS idle time (`GetLastInputInfo`),
//! which synthetic input resets. Pressing only when already idle means the key
//! never lands in the middle of real typing (where it could combine with a held
//! modifier). Side effect, by design: the idle timer reset also keeps the screen
//! from locking or turning off while this is on.
//!
//! One worker thread, started on first enable. It sleeps until the idle time
//! could reach the interval and parks entirely while off; `apply` / `set_paused`
//! unpark it to pick up the change.
//!
//! Two switches: `ENABLED` is the persisted setting (the feature exists), and
//! `PAUSED` is the tray menu's runtime on/off. The tray item only appears while
//! the feature is enabled. A pause isn't saved — it ends when the app restarts
//! or the feature is re-enabled in settings.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::Thread;
use std::time::Duration;

use windows::Win32::System::SystemInformation::GetTickCount;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetLastInputInfo, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBD_EVENT_FLAGS, KEYBDINPUT, KEYEVENTF_KEYUP, LASTINPUTINFO,
    SendInput, VK_F14,
};

/// Interval choices shown in the UI: (seconds, label).
pub const INTERVALS: [(u32, &str); 4] =
    [(30, "30 seconds"), (60, "1 minute"), (120, "2 minutes"), (240, "4 minutes")];

pub const DEFAULT_INTERVAL_SECS: u32 = 60;

static ENABLED: AtomicBool = AtomicBool::new(false);
static PAUSED: AtomicBool = AtomicBool::new(false);
static INTERVAL_SECS: AtomicU32 = AtomicU32::new(DEFAULT_INTERVAL_SECS);
static WORKER: OnceLock<Thread> = OnceLock::new();

/// Index into `INTERVALS` for a saved interval (unknown values → the default).
pub fn interval_index(secs: u32) -> usize {
    INTERVALS
        .iter()
        .position(|&(s, _)| s == secs)
        .or_else(|| INTERVALS.iter().position(|&(s, _)| s == DEFAULT_INTERVAL_SECS))
        .unwrap_or(0)
}

/// Turns the feature on/off and sets its interval. Idempotent; turning it on
/// (off → on) also ends a tray pause.
pub fn apply(enabled: bool, interval_secs: u32) {
    INTERVAL_SECS.store(interval_secs.max(5), Ordering::Relaxed);
    if !ENABLED.swap(enabled, Ordering::Relaxed) && enabled {
        PAUSED.store(false, Ordering::Relaxed);
    }
    wake(enabled);
}

/// Whether the feature is enabled in settings (the tray shows its item then).
pub fn is_enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Whether it's currently pressing: enabled and not paused from the tray.
pub fn is_running() -> bool {
    is_enabled() && !PAUSED.load(Ordering::Relaxed)
}

/// Tray menu on/off, without changing the saved setting.
pub fn set_paused(paused: bool) {
    PAUSED.store(paused, Ordering::Relaxed);
    wake(is_running());
}

/// Unparks the worker so it re-reads the switches, starting it if it's needed.
fn wake(start: bool) {
    if start {
        WORKER.get_or_init(|| std::thread::spawn(run).thread().clone()).unpark();
    } else if let Some(worker) = WORKER.get() {
        worker.unpark();
    }
}

fn run() {
    loop {
        if !is_running() {
            std::thread::park();
            continue;
        }
        let interval_ms = INTERVAL_SECS.load(Ordering::Relaxed) * 1000;
        let idle = idle_ms();
        let wait_ms = if idle >= interval_ms {
            press_f14();
            interval_ms
        } else {
            interval_ms - idle
        };
        // Woken early by `apply` when settings change.
        std::thread::park_timeout(Duration::from_millis(u64::from(wait_ms.max(1000))));
    }
}

/// Milliseconds since the last keyboard/mouse input in this session.
fn idle_ms() -> u32 {
    let mut info = LASTINPUTINFO { cbSize: size_of::<LASTINPUTINFO>() as u32, dwTime: 0 };
    unsafe {
        if !GetLastInputInfo(&mut info).as_bool() {
            return 0;
        }
        // Both are 32-bit tick counts; wrapping_sub stays correct across the
        // 49.7-day rollover.
        GetTickCount().wrapping_sub(info.dwTime)
    }
}

fn press_f14() {
    let key = |flags: KEYBD_EVENT_FLAGS| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VK_F14, wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    };
    let inputs = [key(KEYBD_EVENT_FLAGS(0)), key(KEYEVENTF_KEYUP)];
    // Fails harmlessly while the workstation is locked (secure desktop).
    unsafe { SendInput(&inputs, size_of::<INPUT>() as i32) };
}
