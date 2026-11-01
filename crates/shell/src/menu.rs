//! Shared helpers for the `IContextMenu` implementations.

use windows::Win32::Foundation::{E_INVALIDARG, E_NOTIMPL, HANDLE, HGLOBAL};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows::Win32::Foundation::GlobalFree;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT, ReleaseStgMedium};
use windows::Win32::UI::Shell::{CMINVOKECOMMANDINFO, DragQueryFileW, GCS_HELPTEXTW, GCS_VERBW, HDROP};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HRESULT, PSTR, PWSTR, Result};

pub struct Item<'a> {
    pub text: &'a str,
    pub grayed: bool,
    pub checked: bool,
}

/// Inserts a string item at `index` with command id `id`.
pub fn insert(menu: HMENU, index: u32, id: u32, item: &Item) -> bool {
    let mut text = wutils_core::wide(item.text);
    let mut state = MFS_ENABLED;
    if item.grayed {
        state |= MFS_DISABLED;
    }
    if item.checked {
        state |= MFS_CHECKED;
    }
    let info = MENUITEMINFOW {
        cbSize: size_of::<MENUITEMINFOW>() as u32,
        fMask: MIIM_STRING | MIIM_ID | MIIM_STATE | MIIM_FTYPE,
        fType: MFT_STRING,
        fState: state,
        wID: id,
        dwTypeData: PWSTR(text.as_mut_ptr()),
        ..Default::default()
    };
    unsafe { InsertMenuItemW(menu, index, true, &info) }.is_ok()
}

/// QueryContextMenu's return value: success with the number of command ids used.
pub fn used_ids(count: u32) -> HRESULT {
    HRESULT(count as i32)
}

/// The command offset if the verb was passed as an id (not a string verb).
pub fn command_offset(info: *const CMINVOKECOMMANDINFO) -> Option<usize> {
    let info = unsafe { info.as_ref() }?;
    let verb = info.lpVerb.0 as usize;
    (verb >> 16 == 0).then_some(verb & 0xFFFF)
}

/// Implements `GetCommandString` for single-item handlers.
pub fn command_string(id: usize, kind: u32, out: PSTR, max: u32, verb: &str, help: &str) -> Result<()> {
    if id != 0 {
        return Err(E_INVALIDARG.into());
    }
    let text = match kind {
        k if k == GCS_VERBW => verb,
        k if k == GCS_HELPTEXTW => help,
        _ => return Err(E_NOTIMPL.into()),
    };
    let wide = wutils_core::wide(text);
    if wide.len() > max as usize {
        return Err(E_INVALIDARG.into());
    }
    unsafe { std::ptr::copy_nonoverlapping(wide.as_ptr(), out.0.cast::<u16>(), wide.len()) };
    Ok(())
}

/// File system paths of the selection (empty if it has no CF_HDROP, e.g. virtual items).
pub fn selected_paths(data: &IDataObject) -> Vec<String> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    let mut paths = Vec::new();
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else { return paths };
        let drop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(drop, u32::MAX, None);
        for i in 0..count {
            let len = DragQueryFileW(drop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            DragQueryFileW(drop, i, Some(&mut buf));
            paths.push(wutils_core::from_wide(&buf));
        }
        ReleaseStgMedium(&mut medium);
    }
    paths
}

/// Puts `text` on the clipboard as CF_UNICODETEXT.
pub fn set_clipboard_text(text: &str) -> Result<()> {
    let wide = wutils_core::wide(text);
    unsafe {
        OpenClipboard(None)?;
        let result = (|| {
            EmptyClipboard()?;
            let mem: HGLOBAL = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
            let ptr = GlobalLock(mem).cast::<u16>();
            if ptr.is_null() {
                let _ = GlobalFree(Some(mem));
                return Err(windows::core::Error::from_thread());
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr, wide.len());
            let _ = GlobalUnlock(mem);
            // On success the clipboard owns the memory.
            if let Err(e) = SetClipboardData(CF_UNICODETEXT.0 as u32, Some(HANDLE(mem.0))) {
                let _ = GlobalFree(Some(mem));
                return Err(e);
            }
            Ok(())
        })();
        let _ = CloseClipboard();
        result
    }
}
