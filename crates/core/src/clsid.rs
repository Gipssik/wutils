//! COM class IDs of the shell extensions in `wutils_shell.dll`.
//! Never change these: they are written to the registry of existing installs.

use windows::core::GUID;

/// Desktop background menu: "HDR" toggle for the display under the cursor.
pub const HDR_MENU: GUID = GUID::from_u128(0x7112a7a7_1f00_43a5_8eba_679d02306fba);
/// Files/folders menu: "Copy as path" without holding Shift.
pub const COPY_PATH_MENU: GUID = GUID::from_u128(0xe4523e73_1faa_4a60_8c9d_e0322727acf1);
/// Files/folders menu: "Compress to ZIP file".
pub const ZIP_MENU: GUID = GUID::from_u128(0x3f358c72_29e3_47ce_8ee6_25b1ce9d6245);

/// Registry form, e.g. `{7112A7A7-1F00-43A5-8EBA-679D02306FBA}`.
pub fn braced(guid: &GUID) -> String {
    format!("{{{guid:?}}}")
}
