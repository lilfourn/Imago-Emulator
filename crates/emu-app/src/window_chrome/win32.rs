//! The refresh rate of the monitor a window is on, on Windows.
//!
//! GPUI follows the window from one monitor to the next, but doesn't say
//! how often a monitor refreshes. The display configuration gives the
//! exact rate of each active monitor (59.94 Hz as 59,940/1,000), matched
//! here by its GDI name to the monitor holding most of the window. The
//! display mode's whole number of hertz (59 for 59.94) is the fallback.
#![allow(unsafe_code)]

use std::ffi::c_void;

use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
    QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, HWND};
use windows::Win32::Graphics::Gdi::{
    DEVMODEW, ENUM_CURRENT_SETTINGS, EnumDisplaySettingsW, GetMonitorInfoW,
    MONITOR_DEFAULTTONEAREST, MONITORINFOEXW, MonitorFromWindow,
};
use windows::core::PCWSTR;

/// How many times to ask for the display configuration while it keeps
/// changing between asking its size and reading it.
const ATTEMPTS: usize = 3;

/// How many times a second the monitor `window` is mostly on refreshes.
pub fn refresh_rate(window: &Window) -> Option<f64> {
    let RawWindowHandle::Win32(handle) = HasWindowHandle::window_handle(window).ok()?.as_raw()
    else {
        return None;
    };
    let hwnd = HWND(handle.hwnd.get() as *mut c_void);
    // SAFETY: the handle is GPUI's live window, borrowed for this call;
    // MonitorFromWindow only reads it.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    if monitor.is_invalid() {
        return None;
    }
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `info` is a MONITORINFOEXW whose size says so, which tells
    // GetMonitorInfoW it may write the monitor's device name after the
    // MONITORINFO it starts with.
    if !unsafe { GetMonitorInfoW(monitor, &mut info.monitorInfo) }.as_bool() {
        return None;
    }
    exact_rate(&info.szDevice).or_else(|| mode_rate(&info.szDevice))
}

/// The refresh rate of the active monitor whose GDI device is `device`
/// (`\\.\DISPLAY1`), as the display configuration has it.
fn exact_rate(device: &[u16; 32]) -> Option<f64> {
    let paths = active_paths()?;
    paths.iter().find_map(|path| {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        // SAFETY: the header starts a DISPLAYCONFIG_SOURCE_DEVICE_NAME
        // whose type and size it gives, as the call requires.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != ERROR_SUCCESS.0 as i32 {
            return None;
        }
        if !same_name(&source.viewGdiDeviceName, device) {
            return None;
        }
        let rate = path.targetInfo.refreshRate;
        (rate.Numerator != 0 && rate.Denominator != 0)
            .then(|| f64::from(rate.Numerator) / f64::from(rate.Denominator))
    })
}

/// The display configuration's active paths: one for each monitor shown.
fn active_paths() -> Option<Vec<DISPLAYCONFIG_PATH_INFO>> {
    for _ in 0..ATTEMPTS {
        let (mut path_count, mut mode_count) = (0, 0);
        // SAFETY: the counts are written through valid pointers.
        unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        }
        .ok()
        .ok()?;
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        // SAFETY: each array holds as many entries as its count says, and
        // the call writes no more than that, updating the counts.
        let result = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if result == ERROR_INSUFFICIENT_BUFFER {
            // A monitor came or went in between: ask again.
            continue;
        }
        result.ok().ok()?;
        paths.truncate(path_count as usize);
        return Some(paths);
    }
    None
}

/// The refresh rate of `device`'s current display mode, in whole hertz.
fn mode_rate(device: &[u16; 32]) -> Option<f64> {
    let mut mode = DEVMODEW {
        dmSize: size_of::<DEVMODEW>() as u16,
        ..DEVMODEW::default()
    };
    // SAFETY: `device` is a NUL-terminated name GetMonitorInfoW wrote,
    // and `mode` a DEVMODEW whose size says so.
    let found =
        unsafe { EnumDisplaySettingsW(PCWSTR(device.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode) };
    // 0 and 1 stand for the hardware's own default rate, which isn't said.
    (found.as_bool() && mode.dmDisplayFrequency > 1).then(|| f64::from(mode.dmDisplayFrequency))
}

/// Whether two NUL-terminated device names are the same, as Windows
/// compares them: without regard to case.
fn same_name(a: &[u16], b: &[u16]) -> bool {
    let name = |wide: &[u16]| {
        let end = wide
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(wide.len());
        String::from_utf16_lossy(&wide[..end]).to_lowercase()
    };
    name(a) == name(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(name: &str) -> [u16; 32] {
        let mut wide = [0; 32];
        for (unit, char) in wide.iter_mut().zip(name.encode_utf16()) {
            *unit = char;
        }
        wide
    }

    #[test]
    fn device_names_match_whatever_their_case() {
        assert!(same_name(&wide(r"\\.\DISPLAY1"), &wide(r"\\.\display1")));
        assert!(!same_name(&wide(r"\\.\DISPLAY1"), &wide(r"\\.\DISPLAY12")));
        assert!(same_name(&wide(""), &wide("")));
    }
}
