//! "Copy as path" in the classic menu without having to hold Shift.

use std::cell::RefCell;

use windows::Win32::Foundation::E_FAIL;
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Registry::HKEY;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::HMENU;
use windows::core::{HRESULT, PSTR, Ref, Result, implement};

use crate::ModuleRef;
use crate::menu::{self, Item};

#[implement(IShellExtInit, IContextMenu)]
pub struct CopyPathMenu {
    paths: RefCell<Vec<String>>,
    _ref: ModuleRef,
}

impl CopyPathMenu {
    pub fn new() -> Self {
        Self { paths: RefCell::new(Vec::new()), _ref: ModuleRef::new() }
    }
}

impl IShellExtInit_Impl for CopyPathMenu_Impl {
    fn Initialize(&self, _folder: *const ITEMIDLIST, data: Ref<IDataObject>, _key: HKEY) -> Result<()> {
        *self.paths.borrow_mut() = data.as_ref().map(menu::selected_paths).unwrap_or_default();
        Ok(())
    }
}

impl IContextMenu_Impl for CopyPathMenu_Impl {
    fn QueryContextMenu(&self, hmenu: HMENU, index: u32, first: u32, _last: u32, flags: u32) -> HRESULT {
        // With Shift held (extended verbs) Windows shows its own "Copy as path".
        if flags & (CMF_DEFAULTONLY | CMF_EXTENDEDVERBS) != 0 || self.paths.borrow().is_empty() {
            return menu::used_ids(0);
        }
        let item = Item { text: "Copy as path", grayed: false, checked: false };
        menu::used_ids(menu::insert(hmenu, index, first, &item) as u32)
    }

    fn InvokeCommand(&self, info: *const CMINVOKECOMMANDINFO) -> Result<()> {
        if menu::command_offset(info) != Some(0) {
            return Err(E_FAIL.into());
        }
        // Same format as the built-in command: each path quoted, one per line.
        let text = self.paths.borrow().iter().map(|p| format!("\"{p}\"")).collect::<Vec<_>>().join("\r\n");
        menu::set_clipboard_text(&text)
    }

    fn GetCommandString(&self, id: usize, kind: u32, _reserved: *const u32, out: PSTR, max: u32) -> Result<()> {
        menu::command_string(id, kind, out, max, "wutils.copypath", "Copy the full path of the selected items")
    }
}
