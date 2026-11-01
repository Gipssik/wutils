//! Loads `wutils_taskbar.dll` into the shell processes. The DLL installs its own
//! hooks from `DllMain` (see that crate), so injection is just a remote
//! `LoadLibraryW` — there are no remote export calls, which means a stale or
//! rebuilt DLL can never be entered at a mismatched address.
//!
//! Standard load technique (as used by Windhawk / ExplorerPatcher): write the
//! DLL path into the target with `VirtualAllocEx`, then `CreateRemoteThread` on
//! `LoadLibraryW`. It only ever touches this machine's own shell. Config is
//! published separately through the DLL's shared section before injecting.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Diagnostics::ToolHelp::*;
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::Win32::System::Memory::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_READWRITE, VirtualAllocEx, VirtualFreeEx};
use windows::Win32::System::Threading::*;
use windows::core::{Error, HSTRING, Result, s, w};

/// Shell processes that host taskbar / Start-menu UI and must load the hook.
const TARGETS: &[&str] = &["explorer.exe", "ShellHost.exe", "StartMenuExperienceHost.exe"];
const DLL_NAME: &str = "wutils_taskbar.dll";

fn pids_named(names: &[&str]) -> Vec<u32> {
    let mut pids = Vec::new();
    unsafe {
        let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return pids };
        let mut entry = PROCESSENTRY32W { dwSize: size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = wutils_core::from_wide(&entry.szExeFile).to_ascii_lowercase();
                if names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    pids
}

/// Full path of the `dll_name` module loaded in process `pid`, if any. Matched
/// by name, so this also finds a copy loaded from another folder.
fn loaded_path(pid: u32, dll_name: &str) -> Option<PathBuf> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid).ok()?;
        let mut entry = MODULEENTRY32W { dwSize: size_of::<MODULEENTRY32W>() as u32, ..Default::default() };
        let mut found = None;
        if Module32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                if wutils_core::from_wide(&entry.szModule).eq_ignore_ascii_case(dll_name) {
                    found = Some(PathBuf::from(wutils_core::from_wide(&entry.szExePath)));
                    break;
                }
                if Module32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
        found
    }
}

/// Runs `start_routine(arg)` on a new thread in the target and waits for it.
unsafe fn run_remote(process: HANDLE, start_routine: usize, arg: *const c_void) -> Result<u32> {
    unsafe {
        let proc: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(start_routine);
        let thread = CreateRemoteThread(process, None, 0, Some(proc), Some(arg), 0, None)?;
        let code = if WaitForSingleObject(thread, 10_000) == WAIT_OBJECT_0 {
            let mut exit = 0u32;
            let _ = GetExitCodeThread(thread, &mut exit);
            exit
        } else {
            0
        };
        let _ = CloseHandle(thread);
        Ok(code)
    }
}

/// Writes a wide string into the target; caller frees with `VirtualFreeEx`.
unsafe fn alloc_wide(process: HANDLE, text: &HSTRING) -> Option<*mut c_void> {
    unsafe {
        let bytes = (text.len() + 1) * 2;
        let remote = VirtualAllocEx(process, None, bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        if remote.is_null() {
            return None;
        }
        let ok = windows::Win32::System::Diagnostics::Debug::WriteProcessMemory(
            process,
            remote,
            text.as_ptr() as *const c_void,
            bytes,
            None,
        )
        .is_ok();
        if ok {
            Some(remote)
        } else {
            let _ = VirtualFreeEx(process, remote, 0, MEM_RELEASE);
            None
        }
    }
}

fn open(pid: u32) -> Result<HANDLE> {
    unsafe {
        OpenProcess(
            PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION | PROCESS_VM_OPERATION | PROCESS_VM_WRITE | PROCESS_VM_READ,
            false,
            pid,
        )
    }
}

/// Every distinct copy of the hook DLL loaded in the shell processes. Usually
/// just ours, but the shell keeps whichever copy it loaded first — e.g. one from
/// a dev build or another install folder — until it restarts. Each copy has its
/// own `.shared` section, so config must be written to all of them.
pub fn loaded_copies() -> Vec<PathBuf> {
    let mut copies: Vec<PathBuf> = Vec::new();
    for pid in pids_named(TARGETS) {
        if let Some(path) = loaded_path(pid, DLL_NAME)
            && !copies.iter().any(|c| c.as_os_str().eq_ignore_ascii_case(path.as_os_str()))
        {
            copies.push(path);
        }
    }
    copies
}

/// `LoadLibraryW` the DLL into one process (unless a copy is already loaded:
/// a second copy would stack a second set of hooks).
fn inject_one(pid: u32, dll: &HSTRING) -> Result<()> {
    if let Some(path) = loaded_path(pid, DLL_NAME) {
        wutils_core::log::line(&format!("inject: pid={pid} already loaded from {}", path.display()));
        return Ok(());
    }
    let process = match open(pid) {
        Ok(p) => p,
        Err(e) => {
            wutils_core::log::line(&format!("inject: OpenProcess pid={pid} FAILED: {e}"));
            return Err(e);
        }
    };
    let result = (|| unsafe {
        let load_library = {
            let k32 = GetModuleHandleW(w!("kernel32.dll"))?;
            GetProcAddress(k32, s!("LoadLibraryW")).ok_or_else(Error::from_thread)? as usize
        };
        let remote_path = alloc_wide(process, dll).ok_or_else(Error::from_thread)?;
        let code = run_remote(process, load_library, remote_path);
        let _ = VirtualFreeEx(process, remote_path, 0, MEM_RELEASE);
        wutils_core::log::line(&format!("inject: pid={pid} LoadLibraryW -> {code:?}"));
        Ok(())
    })();
    unsafe {
        let _ = CloseHandle(process);
    }
    result
}

/// Injects the DLL into all running shell processes. Per-process errors are
/// collected; the rest still proceed.
pub fn inject_all(dll: &Path) -> Result<()> {
    if !dll.exists() {
        wutils_core::log::line(&format!("inject: DLL missing at {}", dll.display()));
        return Err(Error::from_thread());
    }
    let pids = pids_named(TARGETS);
    wutils_core::log::line(&format!("inject: targets found = {pids:?}"));
    let dll_path = HSTRING::from(dll.as_os_str());
    let mut last_err = None;
    for pid in pids {
        if let Err(e) = inject_one(pid, &dll_path) {
            last_err = Some(e);
        }
    }
    last_err.map_or(Ok(()), Err)
}
