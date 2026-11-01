//! Minimal HKEY_CURRENT_USER registry helpers. Wutils never writes to HKLM,
//! so nothing requires elevation.

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR};
use windows::Win32::System::Registry::*;
use windows::core::{HSTRING, PCWSTR, Result};

fn check(err: WIN32_ERROR) -> Result<()> {
    err.ok()
}

fn create(path: &str) -> Result<HKEY> {
    let mut key = HKEY::default();
    unsafe {
        check(RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        ))?;
    }
    Ok(key)
}

fn set_raw(path: &str, name: Option<&str>, kind: REG_VALUE_TYPE, data: &[u8]) -> Result<()> {
    let key = create(path)?;
    let name = name.map(HSTRING::from).unwrap_or_default();
    let res = unsafe { RegSetValueExW(key, &name, None, kind, Some(data)) };
    unsafe {
        let _ = RegCloseKey(key);
    }
    check(res)
}

/// Sets a REG_SZ value (`name = None` is the key's default value).
pub fn set_string(path: &str, name: Option<&str>, value: &str) -> Result<()> {
    let bytes: Vec<u8> = crate::wide(value).iter().flat_map(|c| c.to_le_bytes()).collect();
    set_raw(path, name, REG_SZ, &bytes)
}

pub fn set_dword(path: &str, name: Option<&str>, value: u32) -> Result<()> {
    set_raw(path, name, REG_DWORD, &value.to_le_bytes())
}

/// Reads a REG_SZ / REG_EXPAND_SZ value. `None` if the key or value is missing.
pub fn get_string(path: &str, name: Option<&str>) -> Option<String> {
    let path = HSTRING::from(path);
    let name = name.map(HSTRING::from).unwrap_or_default();
    let mut size = 0u32;
    unsafe {
        check(RegGetValueW(HKEY_CURRENT_USER, &path, &name, RRF_RT_REG_SZ, None, None, Some(&mut size)))
            .ok()?;
        let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
        check(RegGetValueW(
            HKEY_CURRENT_USER,
            &path,
            &name,
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        ))
        .ok()?;
        Some(crate::from_wide(&buf))
    }
}

pub fn key_exists(path: &str) -> bool {
    let mut key = HKEY::default();
    unsafe {
        let ok = RegOpenKeyExW(HKEY_CURRENT_USER, &HSTRING::from(path), None, KEY_READ, &mut key).is_ok();
        if ok {
            let _ = RegCloseKey(key);
        }
        ok
    }
}

/// Deletes a key with all subkeys. Missing keys are not an error.
pub fn delete_tree(path: &str) -> Result<()> {
    let res = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(path)) };
    if res == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(res) }?;
    // RegDeleteTreeW only empties the key; remove the key itself too.
    let res = unsafe { RegDeleteKeyW(HKEY_CURRENT_USER, &HSTRING::from(path)) };
    if res == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(res) }
}

/// Deletes a single value. Missing values are not an error.
pub fn delete_value(path: &str, name: &str) -> Result<()> {
    let res = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, &HSTRING::from(path), &HSTRING::from(name)) };
    if res == ERROR_FILE_NOT_FOUND { Ok(()) } else { check(res) }
}
