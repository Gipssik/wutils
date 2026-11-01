//! System-tray icon on its own Win32 message loop. It talks to the Slint UI
//! only through the two callbacks it's given (which hop onto the UI thread via
//! `slint::invoke_from_event_loop`), so no Win32/Slint state is shared directly.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::Sender;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW, Shell_NotifyIconW};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

const WM_TRAY: u32 = WM_APP + 1;
const ID_OPEN: usize = 1;
const ID_EXIT: usize = 2;
const TRAY_UID: u32 = 1;
/// `IDI_APPICON` in `wutils.rc`.
const IDI_APPICON: u16 = 1;

/// The `TaskbarCreated` broadcast id (set once in `run`), sent when Explorer
/// (re)starts. We re-add the icon and re-apply the taskbar move on it.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Callbacks invoked from the tray thread.
pub struct Handlers {
    pub on_open: Box<dyn Fn() + Send>,
    pub on_exit: Box<dyn Fn() + Send>,
    /// Explorer (re)started: re-inject the taskbar hook.
    pub on_shell_restart: Box<dyn Fn() + Send>,
}

thread_local! {
    static HANDLERS: std::cell::RefCell<Option<Handlers>> = const { std::cell::RefCell::new(None) };
    /// Loaded once and reused for every `NIM_ADD` (Explorer restarts re-add it).
    static ICON: std::cell::Cell<HICON> = std::cell::Cell::new(HICON::default());
}

/// The app icon from the exe's resources at the small-icon size the tray draws,
/// so Windows picks the matching frame instead of scaling the 32px one down.
fn load_icon() -> HICON {
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        LoadImageW(
            Some(hinst.into()),
            PCWSTR(IDI_APPICON as usize as *const u16), // MAKEINTRESOURCE
            IMAGE_ICON,
            GetSystemMetrics(SM_CXSMICON),
            GetSystemMetrics(SM_CYSMICON),
            LR_DEFAULTCOLOR,
        )
        .map(|h| HICON(h.0))
        .unwrap_or_default()
    }
}

fn notify_data(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_UID,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: ICON.get(),
        ..Default::default()
    };
    let tip = "Wutils";
    for (i, c) in tip.encode_utf16().enumerate() {
        data.szTip[i] = c;
    }
    data
}

fn show_menu(hwnd: HWND) {
    unsafe {
        let menu = match CreatePopupMenu() {
            Ok(m) => m,
            Err(_) => return,
        };
        let _ = AppendMenuW(menu, MF_STRING, ID_OPEN, w!("Open Wutils"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(menu, MF_STRING, ID_EXIT, w!("Exit"));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Required so the menu dismisses when the user clicks elsewhere.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, Some(0), hwnd, None);
        let _ = DestroyMenu(menu);
        // Per the TrackPopupMenu docs, so the next tray click opens the menu reliably.
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));

        let id = cmd.0 as usize;
        HANDLERS.with(|h| {
            if let Some(h) = h.borrow().as_ref() {
                match id {
                    ID_OPEN => (h.on_open)(),
                    ID_EXIT => (h.on_exit)(),
                    _ => {}
                }
            }
        });
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let taskbar_created = TASKBAR_CREATED.load(Ordering::Relaxed);
    if taskbar_created != 0 && msg == taskbar_created {
        // Explorer restarted: our icon and injected DLL are gone. Re-add both.
        let data = notify_data(hwnd);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_ADD, &data);
        }
        HANDLERS.with(|h| {
            if let Some(h) = h.borrow().as_ref() {
                (h.on_shell_restart)();
            }
        });
        return LRESULT(0);
    }
    match msg {
        WM_TRAY => {
            let event = (lparam.0 & 0xFFFF) as u32;
            match event {
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                WM_LBUTTONDBLCLK => HANDLERS.with(|h| {
                    if let Some(h) = h.borrow().as_ref() {
                        (h.on_open)();
                    }
                }),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            let data = NOTIFYICONDATAW {
                cbSize: size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: hwnd,
                uID: TRAY_UID,
                ..Default::default()
            };
            unsafe {
                let _ = Shell_NotifyIconW(NIM_DELETE, &data);
                PostQuitMessage(0);
            }
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Creates the tray icon and pumps messages until the window is closed.
/// Sends the hidden window's handle back so the caller can close it on exit.
/// Blocks; run on a dedicated thread.
pub fn run(handlers: Handlers, hwnd_tx: Sender<isize>) {
    HANDLERS.with(|h| *h.borrow_mut() = Some(handlers));
    unsafe {
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("WutilsTrayWnd");
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: class,
            ..Default::default()
        };
        RegisterClassW(&wc);
        ICON.set(load_icon());
        // A hidden top-level window (not message-only: HWND_MESSAGE windows don't
        // receive the TaskbarCreated broadcast).
        let hwnd = CreateWindowExW(
            Default::default(),
            class,
            w!("Wutils"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )
        .unwrap_or_default();

        let _ = hwnd_tx.send(hwnd.0 as isize);
        if hwnd.is_invalid() {
            return;
        }

        let data = notify_data(hwnd);
        let _ = Shell_NotifyIconW(NIM_ADD, &data);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
}

/// Closes the tray window (by handle), removing the icon. Synchronous, so the
/// icon is gone before the process exits instead of lingering as a ghost.
pub fn close(hwnd: isize) {
    unsafe {
        let _ = SendMessageTimeoutW(HWND(hwnd as *mut _), WM_CLOSE, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, 2000, None);
    }
}
