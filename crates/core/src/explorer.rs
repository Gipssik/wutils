//! Talking to the running shell (`explorer.exe`).

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

/// Undocumented Shell_TrayWnd messages (stable since Windows 7).
const WM_TRAY_EXIT_EXPLORER: u32 = 0x5B4;
const WM_TRAY_DISPLAY_CHANGED: u32 = 0x5B8;

/// The primary taskbar window of the running shell.
pub fn taskbar_window() -> Option<HWND> {
    unsafe { FindWindowW(w!("Shell_TrayWnd"), None) }.ok()
}

/// PID of the shell `explorer.exe` (the one owning the taskbar).
pub fn shell_pid() -> Option<u32> {
    let hwnd = taskbar_window()?;
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    (pid != 0).then_some(pid)
}

/// Makes the taskbar re-evaluate which monitor is primary
/// (`CTray::_HandleDisplayChange`) and tells the immersive shell the same.
pub fn notify_display_changed() {
    unsafe {
        if let Some(hwnd) = taskbar_window() {
            let _ = SendMessageTimeoutW(hwnd, WM_TRAY_DISPLAY_CHANGED, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 2000, None);
        }
        // SHELLHOOK code 35: display change, handled by twinui's immersive monitor manager.
        let shellhook = RegisterWindowMessageW(w!("SHELLHOOK"));
        let _ = PostMessageW(Some(HWND_BROADCAST), shellhook, WPARAM(35), LPARAM(0));
    }
}

/// Gracefully exits the shell and starts a new one. Blocks for up to ~10s.
pub fn restart() -> std::io::Result<()> {
    if let Some(pid) = shell_pid() {
        unsafe {
            let process = OpenProcess(PROCESS_SYNCHRONIZE | PROCESS_TERMINATE, false, pid).ok();
            if let Some(hwnd) = taskbar_window() {
                let _ = PostMessageW(Some(hwnd), WM_TRAY_EXIT_EXPLORER, WPARAM(0), LPARAM(0));
            }
            if let Some(process) = process {
                if WaitForSingleObject(process, 5000) != WAIT_OBJECT_0 {
                    let _ = TerminateProcess(process, 1);
                    WaitForSingleObject(process, 3000);
                }
                let _ = CloseHandle(process);
            }
        }
    }

    // Winlogon may restart the shell by itself after a forced kill; don't start a second one.
    let deadline = Instant::now() + Duration::from_millis(1500);
    while Instant::now() < deadline {
        if taskbar_window().is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let windir = std::env::var("WINDIR").unwrap_or_else(|_| r"C:\Windows".into());
    std::process::Command::new(format!(r"{windir}\explorer.exe")).spawn()?;
    Ok(())
}
