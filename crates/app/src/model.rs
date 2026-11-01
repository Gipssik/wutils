//! Builds the data the Slint UI renders: the sidebar pages and, for each page,
//! a flat list of setting cards. Adding a feature means adding rows here — the
//! `.slint` file doesn't change.

use slint::{ModelRc, SharedString, VecModel};
use wutils_core::display::Monitor;

use crate::settings::Settings;
use crate::{ItemKind, NavPage, SettingItem};

pub struct Page {
    pub title: &'static str,
    pub icon: &'static str,
    pub items: Vec<SettingItem>,
}

fn choices(labels: &[String]) -> ModelRc<SharedString> {
    let v: Vec<SharedString> = labels.iter().map(SharedString::from).collect();
    ModelRc::new(VecModel::from(v))
}

fn item(id: &str, kind: ItemKind, title: &str, desc: &str) -> SettingItem {
    SettingItem {
        id: id.into(),
        kind,
        title: title.into(),
        description: desc.into(),
        badge: SharedString::new(),
        page: SharedString::new(),
        enabled: true,
        checked: false,
        choices: choices(&[]),
        current: 0,
        action: SharedString::new(),
    }
}

fn header(title: &str) -> SettingItem {
    item("", ItemKind::Header, title, "")
}

fn toggle(id: &str, title: &str, desc: &str, checked: bool) -> SettingItem {
    SettingItem { checked, ..item(id, ItemKind::Toggle, title, desc) }
}

/// Index of the saved monitor in `monitors`, defaulting to the first non-primary.
pub fn selected_monitor_index(settings: &Settings, monitors: &[Monitor]) -> usize {
    monitors
        .iter()
        .position(|m| m.device_path == settings.taskbar.monitor_device_path)
        .or_else(|| monitors.iter().position(|m| !m.primary))
        .unwrap_or(0)
}

pub fn pages(settings: &Settings, monitors: &[Monitor]) -> Vec<Page> {
    let multi_monitor = monitors.len() > 1;
    let monitor_labels: Vec<String> = monitors.iter().map(|m| m.label()).collect();

    // --- Taskbar ---
    let mut taskbar_move = toggle(
        "taskbar.move",
        "Move the taskbar to another display",
        "Shows the primary taskbar (tray, clock, Start, search) on the display you pick, \
         without changing which display is primary for everything else.",
        settings.taskbar.move_to_secondary,
    );
    if !multi_monitor {
        taskbar_move.enabled = false;
        taskbar_move.badge = "Needs a second display".into();
    }

    let mut monitor_choice = SettingItem {
        enabled: multi_monitor && settings.taskbar.move_to_secondary,
        choices: choices(&monitor_labels),
        current: selected_monitor_index(settings, monitors) as i32,
        ..item("taskbar.monitor", ItemKind::Choice, "Display for the taskbar", "")
    };
    if !multi_monitor {
        monitor_choice.current = 0;
    }

    let taskbar = Page {
        title: "Taskbar",
        icon: "\u{E7F4}",
        items: vec![header("Secondary display"), taskbar_move, monitor_choice],
    };

    // --- Context menu ---
    let context = Page {
        title: "Context menu",
        icon: "\u{E8FD}",
        items: vec![
            header("Windows 11 menu"),
            toggle(
                "ctx.classic",
                "Use the classic context menu",
                "Brings back the Windows 10 right-click menu in place of the compact \
                 Windows 11 one. Explorer restarts to apply this.",
                settings.context_menu.classic,
            ),
            header("Extra items"),
            toggle(
                "ctx.copypath",
                "Add \u{201C}Copy as path\u{201D}",
                "Adds Copy as path to files and folders so you don't have to hold Shift. \
                 Shown in the classic menu.",
                settings.context_menu.copy_as_path,
            ),
            toggle(
                "ctx.zip",
                "Add \u{201C}Compress to ZIP file\u{201D}",
                "Adds the Windows 11 ZIP command to the classic menu.",
                settings.context_menu.compress_zip,
            ),
        ],
    };

    // --- Displays / HDR ---
    let hdr_supported = monitors.iter().filter(|m| m.hdr.supported).count();
    let mut hdr_toggle = toggle(
        "hdr.menu",
        "Add an HDR toggle to the desktop menu",
        "Right-click the desktop to turn HDR on or off for that display. Displays that \
         don't support HDR show a disabled entry.",
        settings.hdr.menu_entry,
    );
    hdr_toggle.badge = match hdr_supported {
        0 => "No HDR displays detected".into(),
        n => format!("{n} of {} displays support HDR", monitors.len()).into(),
    };

    let displays = Page {
        title: "Displays & HDR",
        icon: "\u{E7F8}",
        items: vec![header("HDR"), hdr_toggle],
    };

    // --- Updates ---
    let updates = Page {
        title: "Updates",
        icon: "\u{E895}",
        items: vec![
            header("Automatic updates"),
            toggle(
                "updates.auto",
                "Check for updates automatically",
                "Checks GitHub Releases on startup and installs new versions after verifying \
                 their checksum.",
                settings.updates.auto_check,
            ),
            SettingItem { action: "Check now".into(), ..item("updates.check", ItemKind::Button, "Check for updates", "") },
        ],
    };

    // --- General ---
    let general = Page {
        title: "General",
        icon: "\u{E713}",
        items: vec![
            header("Startup"),
            toggle(
                "general.startup",
                "Start Wutils with Windows",
                "Runs Wutils at sign-in so your settings are reapplied automatically.",
                settings.general.start_with_windows,
            ),
            toggle(
                "general.minimized",
                "Start minimized to the tray",
                "Starts without opening this window.",
                settings.general.start_minimized,
            ),
        ],
    };

    vec![taskbar, context, displays, updates, general]
}

/// Cards for one page.
pub fn page_items(pages: &[Page], index: usize) -> ModelRc<SettingItem> {
    let items = pages.get(index).map(|p| p.items.clone()).unwrap_or_default();
    ModelRc::new(VecModel::from(items))
}

/// Cards across all pages matching `query`, each tagged with its page title.
pub fn search_items(pages: &[Page], query: &str) -> ModelRc<SettingItem> {
    let q = query.to_lowercase();
    let mut out = Vec::new();
    for page in pages {
        for it in &page.items {
            if matches!(it.kind, ItemKind::Header) {
                continue;
            }
            let hit = it.title.to_lowercase().contains(&q) || it.description.to_lowercase().contains(&q);
            if hit {
                out.push(SettingItem { page: page.title.into(), ..it.clone() });
            }
        }
    }
    ModelRc::new(VecModel::from(out))
}

/// Sidebar entries.
pub fn nav(pages: &[Page]) -> ModelRc<NavPage> {
    let v: Vec<NavPage> = pages.iter().map(|p| NavPage { title: p.title.into(), icon: p.icon.into() }).collect();
    ModelRc::new(VecModel::from(v))
}
