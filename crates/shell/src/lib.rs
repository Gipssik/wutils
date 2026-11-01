#![allow(clippy::not_unsafe_ptr_arg_deref, clippy::collapsible_if, clippy::missing_safety_doc)]
//! In-process COM server loaded by `explorer.exe` for Wutils' context menu items.
//!
//! Each item is its own COM class so the app can enable/disable items one by one
//! just by (un)registering them under `HKCU\Software\Classes` — the DLL itself
//! reads no configuration. The items are classic `IContextMenu` handlers, so they
//! show up in the classic (Windows 10 style) menu only.
//!
//! Code here runs inside Explorer: no panics, no blocking work in QueryContextMenu.

mod copy_path;
mod hdr;
mod menu;
mod zip;

use std::ffi::c_void;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::Win32::Foundation::{CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_POINTER, S_FALSE, S_OK};
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::core::{BOOL, GUID, HRESULT, IUnknown, Interface, Ref, Result, implement};
use wutils_core::clsid;

/// Live objects + server locks; the DLL may unload when this is zero.
static REFS: AtomicIsize = AtomicIsize::new(0);

/// RAII counter embedded in every COM object.
pub(crate) struct ModuleRef;

impl ModuleRef {
    pub fn new() -> Self {
        REFS.fetch_add(1, Ordering::SeqCst);
        ModuleRef
    }
}

impl Drop for ModuleRef {
    fn drop(&mut self) {
        REFS.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Hdr,
    CopyPath,
    Zip,
}

#[implement(IClassFactory)]
struct Factory {
    kind: Kind,
    _ref: ModuleRef,
}

impl IClassFactory_Impl for Factory_Impl {
    fn CreateInstance(&self, outer: Ref<IUnknown>, iid: *const GUID, object: *mut *mut c_void) -> Result<()> {
        if object.is_null() || iid.is_null() {
            return Err(E_POINTER.into());
        }
        unsafe { *object = std::ptr::null_mut() };
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let unknown: IUnknown = match self.kind {
            Kind::Hdr => hdr::HdrMenu::new().into(),
            Kind::CopyPath => copy_path::CopyPathMenu::new().into(),
            Kind::Zip => zip::ZipMenu::new().into(),
        };
        unsafe { unknown.query(iid, object).ok() }
    }

    fn LockServer(&self, lock: BOOL) -> Result<()> {
        if lock.as_bool() {
            REFS.fetch_add(1, Ordering::SeqCst);
        } else {
            REFS.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllGetClassObject(clsid: *const GUID, iid: *const GUID, object: *mut *mut c_void) -> HRESULT {
    if clsid.is_null() || iid.is_null() || object.is_null() {
        return E_POINTER;
    }
    unsafe { *object = std::ptr::null_mut() };
    let kind = match unsafe { *clsid } {
        c if c == clsid::HDR_MENU => Kind::Hdr,
        c if c == clsid::COPY_PATH_MENU => Kind::CopyPath,
        c if c == clsid::ZIP_MENU => Kind::Zip,
        _ => return CLASS_E_CLASSNOTAVAILABLE,
    };
    let factory: IClassFactory = Factory { kind, _ref: ModuleRef::new() }.into();
    unsafe { factory.query(iid, object) }
}

#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if REFS.load(Ordering::SeqCst) == 0 { S_OK } else { S_FALSE }
}
