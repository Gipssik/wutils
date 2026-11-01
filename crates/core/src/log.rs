//! Tiny append-only diagnostic log shared by the app and the injected DLLs.
//!
//! Each process opens the file per line in append mode, so there are no shared
//! handles to coordinate across `explorer.exe` and the app. Safe to call from
//! inside the shell: it never panics and never blocks on a lock we hold.
//!
//! Logging is on only while a marker file exists at `%LOCALAPPDATA%\Wutils\
//! .debug`. The app creates it when started with `WUTILS_LOG` set and removes
//! it otherwise; the injected DLL checks the same marker, so it doesn't depend
//! on inheriting the app's environment. The log is `%LOCALAPPDATA%\Wutils\
//! wutils.log`.

use std::io::Write;
use std::path::PathBuf;

fn dir() -> Option<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("TEMP"))?;
    Some(PathBuf::from(base).join("Wutils"))
}

fn marker() -> Option<PathBuf> {
    Some(dir()?.join(".debug"))
}

fn enabled() -> bool {
    marker().map(|p| p.exists()).unwrap_or(false)
}

/// Turns the diagnostic log on or off (called by the app from `WUTILS_LOG`).
pub fn set_enabled(on: bool) {
    let Some(marker) = marker() else { return };
    if on {
        let _ = std::fs::create_dir_all(marker.parent().unwrap());
        let _ = std::fs::write(&marker, b"");
    } else {
        let _ = std::fs::remove_file(&marker);
    }
}

fn log_path() -> Option<PathBuf> {
    Some(dir()?.join("wutils.log"))
}

fn proc_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "?".into())
}

/// Appends one line to the log, if logging is enabled.
pub fn line(msg: &str) {
    if !enabled() {
        return;
    }
    let Some(path) = log_path() else { return };
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let pid = std::process::id();
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = writeln!(f, "{ts} [{}:{pid}] {msg}", proc_name());
    }
}
