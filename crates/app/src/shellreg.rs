//! Registers/unregisters the context-menu COM extensions under
//! `HKCU\Software\Classes` (per-user, so no elevation), and the Windows 10
//! classic-menu switch.

use std::path::Path;

use windows::core::{GUID, Result};
use wutils_core::clsid;
use wutils_core::reg;

/// CLSID of the Win11 "command bar" (new) context menu. Shadowing it per-user
/// with an empty in-proc server brings back the classic menu.
const NEW_MENU_CLSID: &str = "{86ca1aa0-34aa-4e8b-a509-50c905bae2a2}";

/// Where each extension hooks in, and the subkey name we use there.
struct Handler {
    guid: GUID,
    /// Shell object whose `shellex\ContextMenuHandlers` we register under.
    object: &'static str,
    name: &'static str,
}

fn handlers() -> [Handler; 3] {
    [
        Handler { guid: clsid::HDR_MENU, object: "DesktopBackground", name: "WutilsHdr" },
        Handler { guid: clsid::COPY_PATH_MENU, object: "AllFilesystemObjects", name: "WutilsCopyPath" },
        Handler { guid: clsid::ZIP_MENU, object: "AllFilesystemObjects", name: "WutilsZip" },
    ]
}

fn clsid_key(guid: &GUID) -> String {
    format!(r"Software\Classes\CLSID\{}", clsid::braced(guid))
}

fn handler_key(object: &str, name: &str) -> String {
    format!(r"Software\Classes\{object}\shellex\ContextMenuHandlers\{name}")
}

/// Registers the COM class pointing at our DLL, and its menu hook point.
fn register_handler(h: &Handler, dll: &Path) -> Result<()> {
    let clsid = clsid_key(&h.guid);
    let inproc = format!(r"{clsid}\InprocServer32");
    reg::set_string(&clsid, None, h.name)?;
    reg::set_string(&inproc, None, &dll.to_string_lossy())?;
    reg::set_string(&inproc, Some("ThreadingModel"), "Apartment")?;
    reg::set_string(&handler_key(h.object, h.name), None, &clsid::braced(&h.guid))?;
    Ok(())
}

fn unregister_handler(h: &Handler) -> Result<()> {
    reg::delete_tree(&handler_key(h.object, h.name))?;
    reg::delete_tree(&clsid_key(&h.guid))?;
    Ok(())
}

/// Enables or disables a single extension by its CLSID.
pub fn set_handler(guid: &GUID, enabled: bool, dll: &Path) -> Result<()> {
    let h = handlers().into_iter().find(|h| h.guid == *guid).expect("known clsid");
    if enabled { register_handler(&h, dll) } else { unregister_handler(&h) }
}

/// Switches between the classic (Windows 10) and new (Windows 11) context menu.
/// Caller restarts Explorer for it to take effect.
pub fn set_classic_menu(classic: bool) -> Result<()> {
    let key = format!(r"Software\Classes\CLSID\{NEW_MENU_CLSID}\InprocServer32");
    if classic {
        // Empty default value => the new menu's handler resolves to nothing.
        reg::set_string(&key, None, "")
    } else {
        reg::delete_tree(&format!(r"Software\Classes\CLSID\{NEW_MENU_CLSID}"))
    }
}

pub fn is_classic_menu() -> bool {
    reg::key_exists(&format!(r"Software\Classes\CLSID\{NEW_MENU_CLSID}\InprocServer32"))
}
