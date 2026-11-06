//! Persisted settings, stored as TOML in `%APPDATA%\Wutils\settings.toml`.
//! Unknown/missing fields fall back to defaults so old files keep loading.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Settings {
    pub taskbar: Taskbar,
    pub context_menu: ContextMenu,
    pub hdr: Hdr,
    pub updates: Updates,
    pub general: General,
    pub presence: Presence,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Presence {
    /// Tap F14 when idle so chat apps don't show you as away.
    pub keep_active: bool,
    /// Idle time before each tap (one of `keepactive::INTERVALS`).
    pub interval_secs: u32,
}

impl Default for Presence {
    fn default() -> Self {
        Self { keep_active: false, interval_secs: crate::keepactive::DEFAULT_INTERVAL_SECS }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Taskbar {
    /// Move the primary taskbar to `monitor_device_path`.
    pub move_to_secondary: bool,
    /// Stable device interface path of the chosen monitor (see `display::Monitor`).
    pub monitor_device_path: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct ContextMenu {
    /// Restore the Windows 10 style right-click menu.
    pub classic: bool,
    /// Add "Copy as path" without needing Shift (classic menu only).
    pub copy_as_path: bool,
    /// Add "Compress to ZIP file" (classic menu only).
    pub compress_zip: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Hdr {
    /// Add an HDR toggle to the desktop right-click menu.
    pub menu_entry: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(default)]
pub struct Updates {
    pub auto_check: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct General {
    pub start_with_windows: bool,
    pub start_minimized: bool,
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self { classic: false, copy_as_path: true, compress_zip: true }
    }
}

impl Default for Updates {
    fn default() -> Self {
        Self { auto_check: true }
    }
}

pub fn config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Wutils")
}

fn config_path() -> PathBuf {
    config_dir().join("settings.toml")
}

impl Settings {
    pub fn load() -> Self {
        std::fs::read_to_string(config_path()).ok().and_then(|s| toml::from_str(&s).ok()).unwrap_or_default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(config_dir())?;
        let text = toml::to_string_pretty(self).map_err(std::io::Error::other)?;
        std::fs::write(config_path(), text)
    }
}
