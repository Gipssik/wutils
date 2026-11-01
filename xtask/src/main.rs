//! Developer tasks, run via `cargo xtask <cmd>`.
//!
//!   dist   Build the release binaries and package `dist/wutils-x64.zip`
//!          (+ `.sha256`) exactly as a GitHub release expects them.

use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

/// Files that make up a release: the app plus the two DLLs it loads.
const ARTIFACTS: &[&str] = &["wutils.exe", "wutils_shell.dll", "wutils_taskbar.dll"];
const ASSET: &str = "wutils-x64.zip";

fn main() {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    let result = match cmd.as_str() {
        "dist" => dist(),
        other => Err(format!("unknown task {other:?}; try `cargo xtask dist`")),
    };
    if let Err(e) = result {
        eprintln!("xtask: {e}");
        std::process::exit(1);
    }
}

fn root() -> PathBuf {
    // CARGO_MANIFEST_DIR is <root>/xtask.
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd.status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("command failed: {status}")) }
}

fn dist() -> Result<(), String> {
    let root = root();
    let release = root.join("target/release");
    let dist = root.join("dist");

    run(Command::new(env!("CARGO")).current_dir(&root).args([
        "build",
        "--release",
        "-p",
        "wutils",
        "-p",
        "wutils-shell",
        "-p",
        "wutils-taskbar",
    ]))?;

    let _ = std::fs::remove_dir_all(&dist);
    std::fs::create_dir_all(&dist).map_err(|e| e.to_string())?;

    let staging = dist.join("wutils");
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    for name in ARTIFACTS {
        let src = release.join(name);
        if !src.exists() {
            return Err(format!("missing build artifact: {}", src.display()));
        }
        std::fs::copy(&src, staging.join(name)).map_err(|e| e.to_string())?;
    }

    let zip = dist.join(ASSET);
    // Compress-Archive is always present on the Windows targets we ship to.
    run(Command::new("powershell.exe").args([
        "-NoProfile",
        "-Command",
        &format!(
            "Compress-Archive -Path '{}/*' -DestinationPath '{}' -Force",
            staging.display(),
            zip.display()
        ),
    ]))?;

    let bytes = std::fs::read(&zip).map_err(|e| e.to_string())?;
    let digest: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    std::fs::write(dist.join(format!("{ASSET}.sha256")), format!("{digest}  {ASSET}\n")).map_err(|e| e.to_string())?;

    println!("dist ready:");
    println!("  {}", zip.display());
    println!("  {} ({digest})", dist.join(format!("{ASSET}.sha256")).display());
    Ok(())
}
