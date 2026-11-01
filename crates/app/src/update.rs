//! Optional auto-update from GitHub Releases.
//!
//! A release tagged `vX.Y.Z` must contain two assets: `wutils-x64.zip` (the
//! exe + both DLLs) and `wutils-x64.zip.sha256` (hex digest of the zip). We
//! download the zip, verify its SHA-256, then hand off to a short PowerShell
//! script that waits for this process to exit, replaces the files in place and
//! relaunches. The network call is skipped entirely when auto-update is off.
//!
//! The DLLs are usually still mapped by Explorer, and a mapped file can't be
//! overwritten — but it can be renamed. The script moves them aside as
//! `<name>.<random>.old` first; `remove_stale_dlls` deletes those once free.

use std::path::Path;

use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPO: &str = "Gipssik/wutils";
const ASSET: &str = "wutils-x64.zip";
const MAX_ZIP: u64 = 100 * 1024 * 1024;
/// Files the shell may hold open; replaced by renaming the old copy aside.
const DLLS: &[&str] = &["wutils_shell.dll", "wutils_taskbar.dll"];

pub struct Available {
    pub version: String,
    pub zip_url: String,
    pub sha_url: String,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
}

/// Parses `a.b.c` (ignoring any leading `v`) into comparable integers.
fn semver(tag: &str) -> Option<(u64, u64, u64)> {
    let t = tag.trim().trim_start_matches(['v', 'V']);
    let mut parts = t.split('.').map(|p| p.trim().parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

fn get(url: &str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    ureq::get(url)
        .header("User-Agent", "wutils-updater")
        .header("Accept", "application/vnd.github+json")
        .call()
}

/// Returns a newer release than the running version, if auto-update finds one.
pub fn check() -> Option<Available> {
    let current = semver(env!("CARGO_PKG_VERSION"))?;
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = get(&url).ok()?.body_mut().read_to_string().ok()?;
    let release: GhRelease = serde_json::from_str(&body).ok()?;
    if release.prerelease || semver(&release.tag_name)? <= current {
        return None;
    }
    let zip = release.assets.iter().find(|a| a.name == ASSET)?;
    let sha = release.assets.iter().find(|a| a.name == format!("{ASSET}.sha256"))?;
    Some(Available {
        version: release.tag_name.trim_start_matches(['v', 'V']).to_string(),
        zip_url: zip.browser_download_url.clone(),
        sha_url: sha.browser_download_url.clone(),
    })
}

fn download_bytes(url: &str, limit: u64) -> Option<Vec<u8>> {
    get(url).ok()?.body_mut().with_config().limit(limit).read_to_vec().ok()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads, verifies and stages the update, then launches the swap script and
/// asks the caller to exit. Returns an error message on failure.
pub fn download_and_apply(update: &Available) -> Result<(), String> {
    let zip = download_bytes(&update.zip_url, MAX_ZIP).ok_or("download failed")?;
    let expected = download_bytes(&update.sha_url, 1024)
        .and_then(|b| String::from_utf8(b).ok())
        .ok_or("checksum download failed")?;
    let expected = expected.split_whitespace().next().unwrap_or("").to_lowercase();

    let actual = hex(&Sha256::digest(&zip));
    if actual != expected {
        return Err("checksum mismatch — update rejected".into());
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let install_dir = exe.parent().ok_or("no install dir")?.to_path_buf();
    let staging = install_dir.join(".update");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let zip_path = staging.join(ASSET);
    std::fs::write(&zip_path, &zip).map_err(|e| e.to_string())?;

    spawn_swapper(&install_dir, &staging, &zip_path, &exe)?;
    Ok(())
}

/// Deletes DLLs a previous update moved aside. Returns whether a stale
/// `wutils_taskbar.dll` couldn't be deleted — i.e. a shell process still runs
/// the old hook (and reads the old copy's shared config) until it restarts.
pub fn remove_stale_dlls() -> bool {
    let Ok(entries) = std::fs::read_dir(crate::features::exe_dir()) else { return false };
    let mut taskbar_in_use = false;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        let Some(dll) = DLLS.iter().find(|d| name.starts_with(*d)) else { continue };
        if name.ends_with(".old") && std::fs::remove_file(entry.path()).is_err() && *dll == "wutils_taskbar.dll" {
            taskbar_in_use = true;
        }
    }
    taskbar_in_use
}

/// A PowerShell single-quoted string literal.
fn ps_quote(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

/// Writes and detaches a PowerShell script that performs the swap after exit.
fn spawn_swapper(install_dir: &Path, staging: &Path, zip: &Path, exe: &Path) -> Result<(), String> {
    let pid = std::process::id();
    let script = staging.join("swap.ps1");
    let dlls = DLLS.iter().map(|d| format!("'{d}'")).collect::<Vec<_>>().join(", ");
    let body = format!(
        r#"$ErrorActionPreference = 'Stop'
try {{ Wait-Process -Id {pid} -Timeout 30 }} catch {{}}
Start-Sleep -Milliseconds 300
$install = {install}
$moved = @()
foreach ($name in @({dlls})) {{
    $path = Join-Path $install $name
    if (Test-Path -LiteralPath $path) {{
        $old = "$name.$([guid]::NewGuid().ToString('N')).old"
        try {{ Rename-Item -LiteralPath $path -NewName $old; $moved += ,@($path, (Join-Path $install $old)) }} catch {{}}
    }}
}}
try {{
    Expand-Archive -LiteralPath {zip} -DestinationPath $install -Force
}} catch {{
    # Put the previous DLLs back so the old version still runs.
    foreach ($m in $moved) {{
        if (-not (Test-Path -LiteralPath $m[0])) {{ try {{ Move-Item -LiteralPath $m[1] -Destination $m[0] }} catch {{}} }}
    }}
}}
Start-Sleep -Milliseconds 200
try {{ Remove-Item -LiteralPath {staging} -Recurse -Force }} catch {{}}
Start-Process -FilePath {exe}
"#,
        zip = ps_quote(zip),
        install = ps_quote(install_dir),
        staging = ps_quote(staging),
        exe = ps_quote(exe),
    );
    std::fs::write(&script, body).map_err(|e| e.to_string())?;

    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
        .arg(&script)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}
