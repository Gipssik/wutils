//! Shared, dependency-light helpers used by the app and by the DLLs that run
//! inside `explorer.exe`. Nothing in here may panic on bad system state.

pub mod clsid;
pub mod display;
pub mod explorer;
pub mod log;
pub mod reg;

/// Null-terminated UTF-16 copy of `s`.
pub fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// String from a (possibly) null-terminated UTF-16 buffer.
pub fn from_wide(buf: &[u16]) -> String {
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..len])
}
