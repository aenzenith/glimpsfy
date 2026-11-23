//! The connected monitors as `Display`s, in the per-display coordinate contract of
//! `crate::desktop`: where each one sits on the virtual screen, its pixel size, its scale, and
//! the name and kind of the panel behind it.

use std::collections::HashMap;

use windows::core::{BOOL, PCWSTR};
use windows::Win32::Devices::Display::{
    DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, QueryDisplayConfig,
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_MODE_INFO,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL,
    DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS, DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED, DISPLAYCONFIG_PATH_INFO,
    DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME, QDC_ONLY_ACTIVE_PATHS,
};
use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, LPARAM, RECT};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, DEVMODEW, ENUM_CURRENT_SETTINGS, HDC, HMONITOR,
    MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

use super::sys::{self, DpiScope};
use crate::display::Display;
use crate::error::{CaptureError, Result};
use crate::frame::ColorSpace;
use crate::geometry::{LogicalRect, PhysicalSize, ScaleFactor};

/// One monitor: how the app sees it, and its rectangle in virtual-screen pixels.
#[derive(Clone, Debug)]
pub(super) struct Monitor {
    pub(super) display: Display,
    pub(super) rect: RECT,
}

/// What the display configuration says about the panel behind a GDI device name.
struct Panel {
    name: String,
    builtin: bool,
    refresh_rate: Option<f64>,
}

/// Every connected monitor, the primary one first, with the name and kind of its panel.
pub(super) fn list() -> Result<Vec<Monitor>> {
    enumerate(true)
}

/// Every connected monitor, the primary one first, for callers that only need where the
/// monitors are and how they scale: reading the display configuration for the panels' names
/// takes a few milliseconds, which a capture and a window listing do not have to pay.
pub(super) fn layout() -> Result<Vec<Monitor>> {
    enumerate(false)
}

fn enumerate(with_panels: bool) -> Result<Vec<Monitor>> {
    let _dpi = DpiScope::per_monitor();
    let mut handles: Vec<isize> = Vec::new();
    // SAFETY: the callback runs on this thread before the call returns, and the pointer in
    // `LPARAM` is to `handles`, which outlives the call.
    let listed =
        unsafe { EnumDisplayMonitors(None, None, Some(collect), LPARAM(&mut handles as *mut Vec<isize> as isize)) };
    if !listed.as_bool() {
        return Err(CaptureError::Backend("EnumDisplayMonitors failed".into()));
    }
    let panels = if with_panels { panels() } else { HashMap::new() };
    let mut monitors: Vec<Monitor> =
        handles.into_iter().filter_map(|handle| describe(HMONITOR(handle as *mut _), &panels, with_panels)).collect();
    // A stable sort: after the primary display the system's own order stays.
    monitors.sort_by_key(|monitor| !monitor.display.is_primary);
    Ok(monitors)
}

/// The rectangle of one monitor in virtual-screen pixels, or `None` once it is unplugged.
pub(super) fn rect_of(monitor: HMONITOR) -> Option<RECT> {
    let _dpi = DpiScope::per_monitor();
    let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
    // SAFETY: `info` is a `MONITORINFO` with its `cbSize` set, which the call fills in.
    unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool().then_some(info.rcMonitor)
}

unsafe extern "system" fn collect(monitor: HMONITOR, _dc: HDC, _rect: *mut RECT, data: LPARAM) -> BOOL {
    // SAFETY: `data` is the pointer to the `Vec<isize>` that `list` passed to
    // `EnumDisplayMonitors`, which calls this function only while `list` waits for it.
    let handles = unsafe { &mut *(data.0 as *mut Vec<isize>) };
    handles.push(monitor.0 as isize);
    true.into()
}

fn describe(monitor: HMONITOR, panels: &HashMap<String, Panel>, with_mode: bool) -> Option<Monitor> {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    // SAFETY: `MONITORINFOEXW` starts with a `MONITORINFO`, and its `cbSize` tells the call that
    // the device name follows, so the call writes inside `info` only.
    let found = unsafe { GetMonitorInfoW(monitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) };
    if !found.as_bool() {
        // The monitor was unplugged between the listing and this call.
        return None;
    }
    let rect = info.monitorInfo.rcMonitor;
    let pixels = sys::physical(rect)?;

    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    // SAFETY: the call writes two u32 values to the pointers it is given.
    let read = unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) };
    let scale = if read.is_ok() && dpi_x >= 48 { dpi_x as f64 / 96.0 } else { 1.0 };

    let device = sys::text(&info.szDevice);
    let panel = panels.get(&device);
    let is_builtin = panel.is_some_and(|panel| panel.builtin);
    let name = match panel.map(|panel| panel.name.trim()).filter(|name| !name.is_empty()) {
        Some(name) => name.to_string(),
        None if is_builtin => "Built-in Display".to_string(),
        // `\\.\DISPLAY2` becomes "Display 2", the number Windows' own settings show.
        None => match device.rsplit("DISPLAY").next().filter(|n| !n.is_empty() && n.len() < device.len()) {
            Some(number) => format!("Display {number}"),
            None => "Display".to_string(),
        },
    };
    let refresh_rate = panel
        .and_then(|panel| panel.refresh_rate)
        .or_else(|| with_mode.then(|| mode_refresh_rate(&info.szDevice)).flatten());

    Some(Monitor {
        display: Display {
            id: sys::display_id(monitor),
            name,
            frame: LogicalRect::new(
                rect.left as f64,
                rect.top as f64,
                pixels.width as f64 / scale,
                pixels.height as f64 / scale,
            ),
            scale: ScaleFactor(scale),
            pixel_size: PhysicalSize::new(pixels.width, pixels.height),
            is_primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            is_builtin,
            // GDI and Windows.Graphics.Capture deliver 8-bit sRGB also from a wide-gamut or HDR
            // panel, so that is what a frame of any display is tagged with.
            color_space: ColorSpace::Srgb,
            refresh_rate,
        },
        rect,
    })
}

/// The refresh rate of the device's current mode in whole hertz, when the mode names one (0 and
/// 1 mean "the hardware's default").
fn mode_refresh_rate(device: &[u16; 32]) -> Option<f64> {
    let mut mode = DEVMODEW { dmSize: std::mem::size_of::<DEVMODEW>() as u16, ..Default::default() };
    // SAFETY: `device` is the NUL-terminated name `GetMonitorInfoW` wrote, and `mode` is a
    // `DEVMODEW` with its `dmSize` set, which the call fills in.
    let read = unsafe { EnumDisplaySettingsW(PCWSTR(device.as_ptr()), ENUM_CURRENT_SETTINGS, &mut mode) };
    (read.as_bool() && mode.dmDisplayFrequency > 1).then_some(mode.dmDisplayFrequency as f64)
}

/// The active display paths: which source feeds which panel. Empty when the configuration
/// cannot be read.
fn active_paths() -> Vec<DISPLAYCONFIG_PATH_INFO> {
    // The configuration can change between asking for the sizes and reading it; then the read
    // says the buffers are too small and both steps run again.
    for _ in 0..3 {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        // SAFETY: the call writes two u32 counts to the pointers it is given.
        if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count) }
            != ERROR_SUCCESS
        {
            break;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        // SAFETY: the two buffers hold as many elements as the counts say, and the call writes
        // at most that many and sets the counts to what it wrote.
        let status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if status == ERROR_SUCCESS {
            paths.truncate(path_count as usize);
            return paths;
        }
        if status != ERROR_INSUFFICIENT_BUFFER {
            break;
        }
    }
    Vec::new()
}

/// The panels of the active display paths, by the GDI device name of the source that feeds them
/// (`\\.\DISPLAY1`). Empty when the display configuration cannot be read (some remote sessions),
/// and the monitors then get generic names.
fn panels() -> HashMap<String, Panel> {
    let mut found = HashMap::new();
    let paths = active_paths();
    for path in &paths {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
        source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
        source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
        source.header.adapterId = path.sourceInfo.adapterId;
        source.header.id = path.sourceInfo.id;
        // SAFETY: the header names the packet's type and its full size, so the call writes
        // inside `source` only.
        if unsafe { DisplayConfigGetDeviceInfo(&mut source.header) } != ERROR_SUCCESS.0 as i32 {
            continue;
        }
        let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
        target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
        target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
        target.header.adapterId = path.targetInfo.adapterId;
        target.header.id = path.targetInfo.id;
        // SAFETY: as above, for `target`.
        if unsafe { DisplayConfigGetDeviceInfo(&mut target.header) } != ERROR_SUCCESS.0 as i32 {
            continue;
        }
        let technology = target.outputTechnology;
        let builtin = technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INTERNAL
            || technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_DISPLAYPORT_EMBEDDED
            || technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_UDI_EMBEDDED
            || technology == DISPLAYCONFIG_OUTPUT_TECHNOLOGY_LVDS;
        let rate = path.targetInfo.refreshRate;
        let refresh_rate = (rate.Numerator > 0 && rate.Denominator > 0)
            .then(|| rate.Numerator as f64 / rate.Denominator as f64)
            .filter(|hertz| *hertz > 1.0);
        // A source that is duplicated onto two panels keeps the first one, which is the one
        // Windows names in its own settings.
        found.entry(sys::text(&source.viewGdiDeviceName)).or_insert(Panel {
            name: sys::text(&target.monitorFriendlyDeviceName),
            builtin,
            refresh_rate,
        });
    }
    found
}
