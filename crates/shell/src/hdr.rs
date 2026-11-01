//! Desktop background menu item: toggles HDR on the display that was right-clicked.

use std::cell::RefCell;

use windows::Win32::Foundation::{E_FAIL, POINT};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Registry::HKEY;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, GetSystemMetrics, HMENU, SM_CMONITORS};
use windows::core::{HRESULT, PSTR, Ref, Result, implement};
use wutils_core::display::{self, Monitor};

use crate::ModuleRef;
use crate::menu::{self, Item};

#[implement(IShellExtInit, IContextMenu)]
pub struct HdrMenu {
    monitor: RefCell<Option<Monitor>>,
    _ref: ModuleRef,
}

impl HdrMenu {
    pub fn new() -> Self {
        Self { monitor: RefCell::new(None), _ref: ModuleRef::new() }
    }
}

impl IShellExtInit_Impl for HdrMenu_Impl {
    fn Initialize(&self, _folder: *const ITEMIDLIST, _data: Ref<IDataObject>, _key: HKEY) -> Result<()> {
        Ok(())
    }
}

impl IContextMenu_Impl for HdrMenu_Impl {
    fn QueryContextMenu(&self, hmenu: HMENU, index: u32, first: u32, _last: u32, flags: u32) -> HRESULT {
        if flags & CMF_DEFAULTONLY != 0 {
            return menu::used_ids(0);
        }
        // The menu was just opened, so the cursor is still where the user right-clicked.
        let mut pt = POINT::default();
        let _ = unsafe { GetCursorPos(&mut pt) };
        let Some(monitor) = display::monitor_at(pt) else { return menu::used_ids(0) };

        // Cheap count; `monitor_at` already enumerated once, keep the menu fast.
        let multi = unsafe { GetSystemMetrics(SM_CMONITORS) } > 1;
        let text = match (monitor.hdr.supported, multi) {
            (false, _) => "HDR (not supported)".to_string(),
            (true, true) => format!("HDR on {}", monitor.label()),
            (true, false) => "HDR".to_string(),
        };
        let item = Item { text: &text, grayed: !monitor.hdr.supported, checked: monitor.hdr.enabled };
        let inserted = menu::insert(hmenu, index, first, &item);
        *self.monitor.borrow_mut() = Some(monitor);
        menu::used_ids(inserted as u32)
    }

    fn InvokeCommand(&self, info: *const CMINVOKECOMMANDINFO) -> Result<()> {
        if menu::command_offset(info) != Some(0) {
            return Err(E_FAIL.into());
        }
        let monitor = self.monitor.borrow().clone().ok_or(windows::core::Error::from(E_FAIL))?;
        let target = monitor.target.ok_or(windows::core::Error::from(E_FAIL))?;
        // Re-read the state: it may have changed while the menu was open.
        let current = display::hdr_state(target);
        if !current.supported {
            return Err(E_FAIL.into());
        }
        display::set_hdr(target, !current.enabled)
    }

    fn GetCommandString(&self, id: usize, kind: u32, _reserved: *const u32, out: PSTR, max: u32) -> Result<()> {
        menu::command_string(id, kind, out, max, "wutils.hdr", "Turn HDR on or off for this display")
    }
}
