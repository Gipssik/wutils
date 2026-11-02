//! Wutils — a fast Windows 11 utilities app. Tray-resident Slint settings UI
//! that drives the taskbar hook, the context-menu extensions, HDR and updates.

// GUI app: no console window.
#![windows_subsystem = "windows"]

slint::include_modules!();

mod features;
mod inject;
mod model;
mod settings;
mod shellreg;
mod startup;
mod symbols;
mod tray;
mod update;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use slint::{ComponentHandle, SharedString};
use wutils_core::display::{self, Monitor};

use settings::Settings;

/// UI-thread state. Only touched from Slint callbacks.
struct State {
    settings: Settings,
    monitors: Vec<Monitor>,
    pages: Vec<model::Page>,
    page: usize,
    query: String,
}

impl State {
    fn rebuild_pages(&mut self) {
        self.monitors = display::monitors();
        self.pages = model::pages(&self.settings, &self.monitors);
    }
}

/// Signals shared with worker threads (which can't hold the `Rc<State>`).
#[derive(Clone)]
struct Signals {
    update: Arc<Mutex<Option<update::Available>>>,
    /// Why Explorer needs a restart (shown in the banner), if it does.
    restart: Arc<Mutex<Option<&'static str>>>,
}

const RESTART_FOR_MENU: &str = "Restart Explorer to apply the context menu change.";
const RESTART_FOR_UPDATE: &str = "Restart Explorer to finish updating Wutils.";

fn main() -> Result<(), slint::PlatformError> {
    // Launched at sign-in (`--minimized` is what older versions registered).
    let autostart = std::env::args().any(|a| a == startup::AUTOSTART_ARG || a == "--minimized");
    // A sign-in launch that finds Wutils already running just exits; a user
    // launch brings up the running instance's window.
    if !tray::claim_single_instance(!autostart) {
        return Ok(());
    }
    if autostart {
        // Get the shell work done ahead of the other sign-in apps; dropped back
        // to normal once the taskbar is handled (`startup::end_boost`).
        startup::boost();
    }

    // Diagnostic logging: `set WUTILS_LOG=1` before launching to trace the
    // taskbar injection into %LOCALAPPDATA%\Wutils\wutils.log.
    wutils_core::log::set_enabled(std::env::var_os("WUTILS_LOG").is_some());
    wutils_core::log::line("app start");

    // Drop DLLs a previous update moved aside. One the shell still has mapped
    // means Explorer runs the old taskbar hook until it restarts.
    let stale_hook = update::remove_stale_dlls();

    let settings = Settings::load();
    // Reapply everything the settings imply (idempotent) so a fresh login or a
    // manual registry edit converges to what's configured.
    features::apply_all_quiet(&settings);

    let ui = AppWindow::new()?;
    ui.set_version(env!("CARGO_PKG_VERSION").into());

    let mut initial = State { settings, monitors: Vec::new(), pages: Vec::new(), page: 0, query: String::new() };
    initial.rebuild_pages();
    let start_minimized = autostart || initial.settings.general.start_minimized;
    let state = Rc::new(RefCell::new(initial));

    let signals = Signals {
        update: Arc::new(Mutex::new(None)),
        restart: Arc::new(Mutex::new(stale_hook.then_some(RESTART_FOR_UPDATE))),
    };

    ui.set_pages(model::nav(&state.borrow().pages));
    render(&ui, &state);
    refresh_banner(&ui, &signals);

    wire_callbacks(&ui, &state, &signals);

    // Closing the window hides to tray instead of quitting.
    {
        let weak = ui.as_weak();
        ui.window().on_close_requested(move || {
            if let Some(ui) = weak.upgrade() {
                let _ = ui.hide();
            }
            slint::CloseRequestResponse::HideWindow
        });
    }

    let tray_hwnd = start_tray(&ui);

    if state.borrow().settings.updates.auto_check {
        spawn_update_check(&ui, &signals);
    }

    if !start_minimized {
        ui.show()?;
    }

    // Keeps running while only the tray icon remains.
    slint::run_event_loop_until_quit()?;

    tray::close(tray_hwnd);
    Ok(())
}

/// Renders the current page (or search results) into the window.
fn render(ui: &AppWindow, state: &Rc<RefCell<State>>) {
    let s = state.borrow();
    let searching = !s.query.trim().is_empty();
    ui.set_searching(searching);
    if searching {
        ui.set_heading(format!("Results for \u{201C}{}\u{201D}", s.query.trim()).into());
        ui.set_items(model::search_items(&s.pages, &s.query));
    } else {
        ui.set_current_page(s.page as i32);
        ui.set_heading(s.pages.get(s.page).map(|p| p.title).unwrap_or("Wutils").into());
        ui.set_items(model::page_items(&s.pages, s.page));
    }
}

fn refresh_banner(ui: &AppWindow, signals: &Signals) {
    if let Some(reason) = *signals.restart.lock().unwrap() {
        ui.set_banner(Banner {
            visible: true,
            text: reason.into(),
            action: "Restart Explorer".into(),
            error: false,
        });
        return;
    }
    if let Some(update) = signals.update.lock().unwrap().as_ref() {
        ui.set_banner(Banner {
            visible: true,
            text: format!("Version {} is available.", update.version).into(),
            action: "Update now".into(),
            error: false,
        });
        return;
    }
    ui.set_banner(Banner { visible: false, ..Default::default() });
}

fn wire_callbacks(ui: &AppWindow, state: &Rc<RefCell<State>>, signals: &Signals) {
    // Sidebar navigation.
    {
        let state = state.clone();
        let weak = ui.as_weak();
        ui.on_page_selected(move |index| {
            state.borrow_mut().page = index.max(0) as usize;
            state.borrow_mut().query.clear();
            if let Some(ui) = weak.upgrade() {
                render(&ui, &state);
            }
        });
    }

    // Search box.
    {
        let state = state.clone();
        let weak = ui.as_weak();
        ui.on_search_changed(move |text| {
            state.borrow_mut().query = text.to_string();
            if let Some(ui) = weak.upgrade() {
                render(&ui, &state);
            }
        });
    }

    // Toggle switches.
    {
        let state = state.clone();
        let signals = signals.clone();
        let weak = ui.as_weak();
        ui.on_toggled(move |id, value| {
            apply_toggle(&id, value, &state, &signals);
            if let Some(ui) = weak.upgrade() {
                render(&ui, &state);
                refresh_banner(&ui, &signals);
            }
        });
    }

    // Dropdowns.
    {
        let state = state.clone();
        let weak = ui.as_weak();
        ui.on_choice_selected(move |id, index| {
            if id == "taskbar.monitor" {
                let mut s = state.borrow_mut();
                if let Some(m) = s.monitors.get(index.max(0) as usize) {
                    s.settings.taskbar.monitor_device_path = m.device_path.clone();
                }
                let _ = s.settings.save();
                // Rebuild so the re-render shows the new selection, not the old one.
                s.rebuild_pages();
                features::apply_taskbar_async(s.settings.clone());
            }
            if let Some(ui) = weak.upgrade() {
                render(&ui, &state);
            }
        });
    }

    // Buttons.
    {
        let signals = signals.clone();
        let weak = ui.as_weak();
        ui.on_action(move |id| {
            if id == "updates.check"
                && let Some(ui) = weak.upgrade() {
                    spawn_update_check(&ui, &signals);
                }
        });
    }

    // Banner action (restart Explorer / install update).
    {
        let signals = signals.clone();
        let weak = ui.as_weak();
        ui.on_banner_action(move || {
            let Some(ui) = weak.upgrade() else { return };
            // Both actions block for seconds, so they run off the UI thread.
            if signals.restart.lock().unwrap().take().is_some() {
                // The tray's TaskbarCreated handler re-applies once the new shell is up.
                std::thread::spawn(|| {
                    let _ = wutils_core::explorer::restart();
                });
                refresh_banner(&ui, &signals);
                return;
            }
            let update = signals.update.lock().unwrap().take();
            if let Some(update) = update {
                ui.set_banner(Banner {
                    visible: true,
                    text: format!("Downloading version {}\u{2026}", update.version).into(),
                    action: SharedString::new(),
                    error: false,
                });
                let weak = weak.clone();
                std::thread::spawn(move || {
                    let result = update::download_and_apply(&update);
                    let _ = slint::invoke_from_event_loop(move || match result {
                        Ok(()) => {
                            let _ = slint::quit_event_loop();
                        }
                        Err(msg) => {
                            if let Some(ui) = weak.upgrade() {
                                ui.set_banner(Banner {
                                    visible: true,
                                    text: format!("Update failed: {msg}").into(),
                                    action: SharedString::new(),
                                    error: true,
                                });
                            }
                        }
                    });
                });
            }
        });
    }
}

/// Applies a single toggle: mutates settings, persists, and effects the change.
fn apply_toggle(id: &str, value: bool, state: &Rc<RefCell<State>>, signals: &Signals) {
    let snapshot = {
        let mut s = state.borrow_mut();
        match id {
            "taskbar.move" => s.settings.taskbar.move_to_secondary = value,
            "ctx.classic" => s.settings.context_menu.classic = value,
            "ctx.copypath" => s.settings.context_menu.copy_as_path = value,
            "ctx.zip" => s.settings.context_menu.compress_zip = value,
            "hdr.menu" => s.settings.hdr.menu_entry = value,
            "updates.auto" => s.settings.updates.auto_check = value,
            "general.startup" => s.settings.general.start_with_windows = value,
            "general.minimized" => s.settings.general.start_minimized = value,
            _ => {}
        }
        let _ = s.settings.save();
        s.rebuild_pages();
        s.settings.clone()
    };

    match id {
        "taskbar.move" => features::apply_taskbar_async(snapshot),
        "ctx.classic" => {
            if features::apply_classic_menu(&snapshot).unwrap_or(false) {
                *signals.restart.lock().unwrap() = Some(RESTART_FOR_MENU);
            }
            // The extra items live in the classic menu; re-register to be safe.
            let _ = features::apply_context_extensions(&snapshot);
        }
        "ctx.copypath" | "ctx.zip" | "hdr.menu" => {
            let _ = features::apply_context_extensions(&snapshot);
        }
        "general.startup" => {
            features::set_startup(snapshot.general.start_with_windows);
        }
        _ => {}
    }
}

fn start_tray(ui: &AppWindow) -> isize {
    let open_weak = ui.as_weak();
    let on_open = Box::new(move || {
        let open_weak = open_weak.clone();
        let _ = slint::invoke_from_event_loop(move || {
            if let Some(ui) = open_weak.upgrade() {
                let _ = ui.show();
                ui.window().set_minimized(false);
                bring_to_front(&ui);
            }
        });
    });
    let on_exit = Box::new(|| {
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    });

    // Explorer restarted (e.g. after the classic-menu change): the injected DLL
    // and tray icon are gone, so re-apply the taskbar move. This is pure Win32
    // (no UI), so it's queued straight from the tray thread — not via the Slint
    // loop, which isn't pumping when the app started minimized with no window.
    // Settings on disk match the in-memory state (saved on every change).
    //
    // This is also how an app started at sign-in, before the shell, finds out it
    // is up. ShellHost / StartMenuExperienceHost can come up a little after the
    // taskbar, so a second pass a few seconds later injects those (idempotent:
    // already-injected processes are skipped).
    let on_shell_restart = Box::new(|| {
        update::remove_stale_dlls();
        features::apply_taskbar_async(settings::Settings::load());
        std::thread::spawn(|| {
            std::thread::sleep(std::time::Duration::from_secs(5));
            features::apply_taskbar_async(settings::Settings::load());
        });
    });

    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        tray::run(tray::Handlers { on_open, on_exit, on_shell_restart }, tx);
    });
    rx.recv().unwrap_or(0)
}

/// Raises an already-visible window above others (`show` alone doesn't when
/// the window is open but behind other windows).
fn bring_to_front(ui: &AppWindow) {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = ui.window().window_handle();
    if let Ok(handle) = handle.window_handle()
        && let RawWindowHandle::Win32(h) = handle.as_raw()
    {
        unsafe {
            let _ = windows::Win32::UI::WindowsAndMessaging::SetForegroundWindow(windows::Win32::Foundation::HWND(
                h.hwnd.get() as *mut _,
            ));
        }
    }
}

/// Checks for updates off the UI thread, then refreshes the banner on it.
fn spawn_update_check(ui: &AppWindow, signals: &Signals) {
    let weak = ui.as_weak();
    let signals = signals.clone();
    std::thread::spawn(move || {
        let found = update::check();
        let _ = slint::invoke_from_event_loop(move || {
            *signals.update.lock().unwrap() = found;
            if let Some(ui) = weak.upgrade() {
                refresh_banner(&ui, &signals);
            }
        });
    });
}
