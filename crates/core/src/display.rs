//! Display enumeration and HDR control through the documented DisplayConfig API.
//!
//! Windows 11 24H2 split "advanced color" into separate HDR and WCG states and
//! added `DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO_2` /
//! `DISPLAYCONFIG_DEVICE_INFO_SET_HDR_STATE`. The `windows` crate doesn't ship
//! those yet, so they're declared here. On older builds we fall back to the
//! original advanced-color calls.

use windows::Win32::Devices::Display::*;
use windows::Win32::Foundation::{LPARAM, LUID, POINT, RECT, WIN32_ERROR};
use windows::Win32::Graphics::Gdi::*;
use windows::core::{BOOL, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TargetId {
    adapter_low: u32,
    adapter_high: i32,
    id: u32,
}

impl TargetId {
    fn adapter(&self) -> LUID {
        LUID { LowPart: self.adapter_low, HighPart: self.adapter_high }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hdr {
    pub supported: bool,
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct Monitor {
    pub handle: HMONITOR,
    /// GDI name, e.g. `\\.\DISPLAY2`. Can change between sessions.
    pub gdi_name: String,
    /// Stable device interface path (`\\?\DISPLAY#...`). Use this to persist a choice.
    pub device_path: String,
    /// EDID name, e.g. `PG27AQDM`. Empty for some internal panels.
    pub friendly_name: String,
    pub rect: RECT,
    pub primary: bool,
    pub target: Option<TargetId>,
    pub hdr: Hdr,
}

impl Monitor {
    /// Number shown in `\\.\DISPLAYn`.
    pub fn number(&self) -> u32 {
        self.gdi_name.trim_start_matches(r"\\.\DISPLAY").parse().unwrap_or(0)
    }

    /// Human-readable label like `PG27AQDM (2560×1440)`.
    pub fn label(&self) -> String {
        let name = if self.friendly_name.is_empty() {
            format!("Display {}", self.number())
        } else {
            self.friendly_name.clone()
        };
        let w = self.rect.right - self.rect.left;
        let h = self.rect.bottom - self.rect.top;
        format!("{name} ({w}×{h})")
    }
}

// --- 24H2 additions (wingdi.h) ---

const GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(15);
const SET_HDR_STATE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(16);

#[repr(C)]
#[derive(Default)]
struct AdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    /// Bitfield: 0 advancedColorSupported, 1 advancedColorActive, 3 limitedByPolicy,
    /// 4 highDynamicRangeSupported, 5 highDynamicRangeUserEnabled, 6 wideColorSupported, 7 wideColorUserEnabled.
    value: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}

#[repr(C)]
#[derive(Default)]
struct SetHdrState {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    /// Bit 0: enableHdr.
    value: u32,
}

fn header<T>(kind: DISPLAYCONFIG_DEVICE_INFO_TYPE, adapter: LUID, id: u32) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER { r#type: kind, size: size_of::<T>() as u32, adapterId: adapter, id }
}

fn get_info<T>(packet: &mut T) -> bool {
    unsafe { DisplayConfigGetDeviceInfo(packet as *mut T as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER) == 0 }
}

fn set_info<T>(packet: &T) -> Result<()> {
    let res = unsafe { DisplayConfigSetDeviceInfo(packet as *const T as *const DISPLAYCONFIG_DEVICE_INFO_HEADER) };
    WIN32_ERROR(res as u32).ok()
}

/// Reads HDR support/state of a display target.
pub fn hdr_state(target: TargetId) -> Hdr {
    let mut v2 = AdvancedColorInfo2 {
        header: header::<AdvancedColorInfo2>(GET_ADVANCED_COLOR_INFO_2, target.adapter(), target.id),
        ..Default::default()
    };
    if get_info(&mut v2) {
        return Hdr { supported: v2.value & (1 << 4) != 0, enabled: v2.value & (1 << 5) != 0 };
    }

    let mut v1 = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: header::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            target.adapter(),
            target.id,
        ),
        ..Default::default()
    };
    if get_info(&mut v1) {
        // Bitfield: 0 supported, 1 enabled, 2 wideColorEnforced, 3 forceDisabled.
        let bits = unsafe { v1.Anonymous.value };
        return Hdr { supported: bits & 1 != 0 && bits & (1 << 3) == 0, enabled: bits & 2 != 0 };
    }
    Hdr::default()
}

/// Turns HDR on/off for a display target.
pub fn set_hdr(target: TargetId, enable: bool) -> Result<()> {
    let packet = SetHdrState {
        header: header::<SetHdrState>(SET_HDR_STATE, target.adapter(), target.id),
        value: enable as u32,
    };
    if set_info(&packet).is_ok() {
        return Ok(());
    }

    let mut legacy = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
        header: header::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(
            DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
            target.adapter(),
            target.id,
        ),
        ..Default::default()
    };
    legacy.Anonymous.value = enable as u32;
    set_info(&legacy)
}

struct TargetInfo {
    gdi_name: String,
    device_path: String,
    friendly_name: String,
    target: TargetId,
}

/// Active DisplayConfig paths, one entry per source (clones report the first target).
fn active_targets() -> Vec<TargetInfo> {
    let mut out = Vec::new();
    unsafe {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes).is_err() {
            return out;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        )
        .is_err()
        {
            return out;
        }
        paths.truncate(n_paths as usize);

        for path in &paths {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(
                    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    path.sourceInfo.adapterId,
                    path.sourceInfo.id,
                ),
                ..Default::default()
            };
            if !get_info(&mut source) {
                continue;
            }
            let gdi_name = crate::from_wide(&source.viewGdiDeviceName);
            if out.iter().any(|t: &TargetInfo| t.gdi_name == gdi_name) {
                continue;
            }

            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
                    DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    path.targetInfo.adapterId,
                    path.targetInfo.id,
                ),
                ..Default::default()
            };
            let (friendly_name, device_path) = if get_info(&mut target) {
                (crate::from_wide(&target.monitorFriendlyDeviceName), crate::from_wide(&target.monitorDevicePath))
            } else {
                (String::new(), String::new())
            };

            out.push(TargetInfo {
                gdi_name,
                device_path,
                friendly_name,
                target: TargetId {
                    adapter_low: path.targetInfo.adapterId.LowPart,
                    adapter_high: path.targetInfo.adapterId.HighPart,
                    id: path.targetInfo.id,
                },
            });
        }
    }
    out
}

unsafe extern "system" fn collect_monitor(h: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let list = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
    list.push(h);
    true.into()
}

/// All active monitors in `EnumDisplayMonitors` order, with HDR state.
pub fn monitors() -> Vec<Monitor> {
    let targets = active_targets();
    let mut handles: Vec<HMONITOR> = Vec::new();
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(&mut handles as *mut _ as isize));
    }

    handles
        .into_iter()
        .filter_map(|handle| {
            let mut info = MONITORINFOEXW::default();
            info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
            unsafe { GetMonitorInfoW(handle, &mut info.monitorInfo) }.as_bool().then_some(())?;
            let gdi_name = crate::from_wide(&info.szDevice);
            let t = targets.iter().find(|t| t.gdi_name == gdi_name);
            let target = t.map(|t| t.target);
            Some(Monitor {
                handle,
                device_path: t.map(|t| t.device_path.clone()).unwrap_or_else(|| gdi_name.clone()),
                friendly_name: t.map(|t| t.friendly_name.clone()).unwrap_or_default(),
                gdi_name,
                rect: info.monitorInfo.rcMonitor,
                primary: info.monitorInfo.dwFlags & windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY != 0,
                hdr: target.map(hdr_state).unwrap_or_default(),
                target,
            })
        })
        .collect()
}

/// The monitor containing `pt` (or the nearest one).
pub fn monitor_at(pt: POINT) -> Option<Monitor> {
    let handle = unsafe { MonitorFromPoint(pt, MONITOR_DEFAULTTONEAREST) };
    monitors().into_iter().find(|m| m.handle == handle)
}
