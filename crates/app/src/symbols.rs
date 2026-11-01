//! Resolves the RVA of the shell's internal `TrayUI::_SetStuckMonitor` function
//! inside `Taskbar.dll` — the function that actually relocates the primary
//! taskbar. It isn't exported, so we resolve it from Microsoft's public symbols:
//!
//! 1. Read `Taskbar.dll`'s CodeView debug record (PDB name, GUID, age).
//! 2. Download the matching PDB from the Microsoft symbol server (the system has
//!    no `symsrv.dll`, so DbgHelp can't fetch it itself) and cache it.
//! 3. Use the system `dbghelp.dll` to look the symbol up in that local PDB.
//!
//! The resulting RVA is cached per PDB identity, so the download and lookup only
//! happen once per Windows/Taskbar.dll version; the PDB is deleted afterwards.
//! Returns `None` (taskbar move then simply doesn't take effect) rather than
//! failing hard.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Diagnostics::Debug::*;
use windows::core::{HSTRING, PCWSTR};

const TASKBAR_DLL: &str = r"C:\Windows\System32\Taskbar.dll";
const SYMBOL: &str = "TrayUI::_SetStuckMonitor";
const MAX_SYM_NAME: usize = 2000;

/// A PDB's identity from the DLL's CodeView record.
struct PdbId {
    name: String,
    // Symbol-server directory component: GUID (no dashes, upper) + age (hex).
    key: String,
}

fn cache_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("Wutils").join("symbols")
}

/// Public entry point: the RVA of `TrayUI::_SetStuckMonitor` in `Taskbar.dll`.
pub fn set_stuck_monitor_rva() -> Option<u32> {
    let dll = Path::new(TASKBAR_DLL);
    let id = read_pdb_id(dll)?;

    let rva_cache = cache_dir().join(format!("{}-{}.rva", id.name, id.key));
    if let Some(rva) = std::fs::read_to_string(&rva_cache).ok().and_then(|s| s.trim().parse().ok()) {
        wutils_core::log::line(&format!("symbols: cached rva {rva:#x}"));
        return Some(rva);
    }

    let pdb = ensure_pdb(&id)?;
    let rva = resolve_rva(dll, &pdb, SYMBOL)?;
    let _ = std::fs::create_dir_all(cache_dir());
    if std::fs::write(&rva_cache, rva.to_string()).is_ok()
        && let Some(dir) = pdb.parent()
    {
        // Only the RVA is needed from now on; don't keep ~7 MB per version.
        let _ = std::fs::remove_dir_all(dir);
    }
    wutils_core::log::line(&format!("symbols: resolved {SYMBOL} rva {rva:#x}"));
    Some(rva)
}

/// Parses the PE32+ CodeView (RSDS) record of `dll`.
fn read_pdb_id(dll: &Path) -> Option<PdbId> {
    let bytes = std::fs::read(dll).ok()?;
    let u32_at = |o: usize| -> Option<u32> { Some(u32::from_le_bytes(bytes.get(o..o + 4)?.try_into().ok()?)) };
    let u16_at = |o: usize| -> Option<u16> { Some(u16::from_le_bytes(bytes.get(o..o + 2)?.try_into().ok()?)) };

    if bytes.get(0..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(0x3C)? as usize;
    if bytes.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    let opt = pe + 24; // optional header starts after the 24-byte file header (incl. PE sig).
    let num_sections = u16_at(pe + 6)? as usize;
    if u16_at(opt)? != 0x20B {
        return None; // not PE32+.
    }
    // Debug directory is data directory #6; directories start at opt+112 in PE32+.
    let debug_rva = u32_at(opt + 112 + 6 * 8)?;
    let debug_size = u32_at(opt + 112 + 6 * 8 + 4)?;
    if debug_rva == 0 {
        return None;
    }

    // Section headers follow the optional header (its size is at file header +16).
    let opt_size = u16_at(pe + 20)? as usize;
    let sections = opt + opt_size;
    let rva_to_off = |rva: u32| -> Option<usize> {
        for i in 0..num_sections {
            let s = sections + i * 40;
            let va = u32_at(s + 12)?;
            let raw_size = u32_at(s + 16)?;
            let raw_ptr = u32_at(s + 20)?;
            if rva >= va && rva < va + raw_size {
                return Some((rva - va + raw_ptr) as usize);
            }
        }
        None
    };

    let debug_off = rva_to_off(debug_rva)?;
    // Walk IMAGE_DEBUG_DIRECTORY entries (28 bytes) for a CodeView (type 2) record.
    for i in 0..(debug_size as usize / 28) {
        let e = debug_off + i * 28;
        if u32_at(e + 12)? != 2 {
            continue; // IMAGE_DEBUG_TYPE_CODEVIEW
        }
        let cv = u32_at(e + 24)? as usize; // PointerToRawData
        if bytes.get(cv..cv + 4)? != b"RSDS" {
            continue;
        }
        let d1 = u32_at(cv + 4)?;
        let d2 = u16_at(cv + 8)?;
        let d3 = u16_at(cv + 10)?;
        let d4 = bytes.get(cv + 12..cv + 20)?;
        let age = u32_at(cv + 20)?;
        let name_bytes = &bytes[cv + 24..];
        let end = name_bytes.iter().position(|&b| b == 0).unwrap_or(0);
        let path = String::from_utf8_lossy(&name_bytes[..end]);
        let name = path.rsplit(['\\', '/']).next()?.to_string();

        let mut key = format!("{d1:08X}{d2:04X}{d3:04X}");
        for b in d4 {
            key.push_str(&format!("{b:02X}"));
        }
        key.push_str(&format!("{age:X}")); // age is appended in hex, no padding.
        return Some(PdbId { name, key });
    }
    None
}

/// Returns the cached PDB path, downloading it from the symbol server if needed.
fn ensure_pdb(id: &PdbId) -> Option<PathBuf> {
    let dir = cache_dir().join(&id.name).join(&id.key);
    let pdb = dir.join(&id.name);
    if pdb.exists() {
        return Some(pdb);
    }
    std::fs::create_dir_all(&dir).ok()?;
    let url = format!("https://msdl.microsoft.com/download/symbols/{}/{}/{}", id.name, id.key, id.name);
    wutils_core::log::line(&format!("symbols: downloading {url}"));
    // The symbol server requires a Microsoft-Symbol-Server user agent.
    let bytes = ureq::get(&url)
        .header("User-Agent", "Microsoft-Symbol-Server/10.0.0.0")
        .call()
        .ok()?
        .body_mut()
        .with_config()
        .limit(256 * 1024 * 1024)
        .read_to_vec()
        .ok()?;
    // Write-then-rename: a partial file must never pass the `exists` check above.
    let partial = dir.join(format!("{}.partial", id.name));
    std::fs::write(&partial, &bytes).ok()?;
    std::fs::rename(&partial, &pdb).ok()?;
    Some(pdb)
}

/// Looks `symbol` up in `pdb` (matched against `dll`) and returns its RVA.
fn resolve_rva(dll: &Path, pdb: &Path, symbol: &str) -> Option<u32> {
    let search = HSTRING::from(pdb.parent().unwrap_or(Path::new(".")).as_os_str());
    let image = HSTRING::from(dll.as_os_str());
    // A unique pseudo-handle for an offline symbol session.
    let handle = HANDLE(0x5755_5453 as *mut _);
    const BASE: u64 = 0x1000_0000;

    unsafe {
        SymSetOptions(SYMOPT_UNDNAME | SYMOPT_DEFERRED_LOADS);
        if SymInitializeW(handle, PCWSTR(search.as_ptr()), false).is_err() {
            return None;
        }
        let result = (|| {
            let loaded = SymLoadModuleExW(handle, None, PCWSTR(image.as_ptr()), PCWSTR::null(), BASE, 0, None, None);
            if loaded == 0 {
                wutils_core::log::line("symbols: SymLoadModuleExW failed");
                return None;
            }
            let mut buf = vec![0u8; size_of::<SYMBOL_INFOW>() + MAX_SYM_NAME * 2];
            let info = buf.as_mut_ptr() as *mut SYMBOL_INFOW;
            (*info).SizeOfStruct = size_of::<SYMBOL_INFOW>() as u32;
            (*info).MaxNameLen = MAX_SYM_NAME as u32;
            if SymFromNameW(handle, &HSTRING::from(symbol), info).is_err() {
                wutils_core::log::line(&format!("symbols: {symbol} not found in pdb"));
                return None;
            }
            Some(((*info).Address - BASE) as u32)
        })();
        let _ = SymCleanup(handle);
        result
    }
}
