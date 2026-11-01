//! "Compress to ZIP file": the Windows 11 menu has it, the classic one only has
//! "Send to > Compressed (zipped) folder". We do exactly what Send To does:
//! create an empty archive and drop the selection onto Windows' own ZIP folder
//! handler, which shows the native progress dialog.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{E_FAIL, POINTL};
use windows::Win32::System::Com::IDataObject;
use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY, IDropTarget};
use windows::Win32::System::Registry::HKEY;
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::HMENU;
use windows::core::{HRESULT, HSTRING, PSTR, Ref, Result, implement};

use crate::ModuleRef;
use crate::menu::{self, Item};

/// End-of-central-directory record of an archive with no entries.
const EMPTY_ZIP: [u8; 22] = [0x50, 0x4B, 0x05, 0x06, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];

#[implement(IShellExtInit, IContextMenu)]
pub struct ZipMenu {
    data: RefCell<Option<IDataObject>>,
    paths: RefCell<Vec<String>>,
    _ref: ModuleRef,
}

impl ZipMenu {
    pub fn new() -> Self {
        Self { data: RefCell::new(None), paths: RefCell::new(Vec::new()), _ref: ModuleRef::new() }
    }
}

/// `<dir>\<name>.zip`, or `<name> (2).zip` etc. if taken.
fn archive_path(first: &Path) -> Option<PathBuf> {
    let dir = first.parent()?;
    let stem = if first.is_dir() { first.file_name()? } else { first.file_stem()? }.to_string_lossy();
    (1..1000).map(|i| if i == 1 { format!("{stem}.zip") } else { format!("{stem} ({i}).zip") }).map(|name| dir.join(name)).find(|p| !p.exists())
}

impl IShellExtInit_Impl for ZipMenu_Impl {
    fn Initialize(&self, _folder: *const ITEMIDLIST, data: Ref<IDataObject>, _key: HKEY) -> Result<()> {
        let data = data.cloned();
        *self.paths.borrow_mut() = data.as_ref().map(menu::selected_paths).unwrap_or_default();
        *self.data.borrow_mut() = data;
        Ok(())
    }
}

impl IContextMenu_Impl for ZipMenu_Impl {
    fn QueryContextMenu(&self, hmenu: HMENU, index: u32, first: u32, _last: u32, flags: u32) -> HRESULT {
        if flags & CMF_DEFAULTONLY != 0 || self.paths.borrow().is_empty() {
            return menu::used_ids(0);
        }
        let item = Item { text: "Compress to ZIP file", grayed: false, checked: false };
        menu::used_ids(menu::insert(hmenu, index, first, &item) as u32)
    }

    fn InvokeCommand(&self, info: *const CMINVOKECOMMANDINFO) -> Result<()> {
        if menu::command_offset(info) != Some(0) {
            return Err(E_FAIL.into());
        }
        let data = self.data.borrow().clone().ok_or(windows::core::Error::from(E_FAIL))?;
        let first = self.paths.borrow().first().map(PathBuf::from).ok_or(windows::core::Error::from(E_FAIL))?;
        let archive = archive_path(&first).ok_or(windows::core::Error::from(E_FAIL))?;
        std::fs::write(&archive, EMPTY_ZIP).map_err(|_| windows::core::Error::from(E_FAIL))?;

        let result = (|| unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(archive.as_os_str()), None)?;
            let target: IDropTarget = item.BindToHandler(None, &BHID_SFUIObject)?;
            let mut effect: DROPEFFECT = DROPEFFECT_COPY;
            target.DragEnter(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect)?;
            effect = DROPEFFECT_COPY;
            target.Drop(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect)
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&archive);
        }
        result
    }

    fn GetCommandString(&self, id: usize, kind: u32, _reserved: *const u32, out: PSTR, max: u32) -> Result<()> {
        menu::command_string(id, kind, out, max, "wutils.zip", "Compress the selected items into a ZIP file")
    }
}
