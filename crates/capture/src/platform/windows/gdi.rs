//! Pixels through GDI: the composited screen with `BitBlt`, a window with `PrintWindow`, and the
//! pointer drawn over either. GDI works on every Windows 10 and 11, in a virtual machine and
//! over Remote Desktop, needs no graphics device and draws no capture border, which is why the
//! stills use it and why it is the last resort of the window capture and of the stream.
//!
//! Every function here expects the calling thread to be per-monitor DPI aware (`DpiScope`), so
//! coordinates are virtual-screen pixels.

use std::ffi::c_void;

use windows::core::Error;
use windows::Win32::Foundation::{E_ACCESSDENIED, HANDLE, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC, SelectObject,
    BITMAPINFO, BITMAPINFOHEADER, BI_RGB, CAPTUREBLT, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ, SRCCOPY,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::System::StationsAndDesktops::{
    CloseDesktop, GetThreadDesktop, GetUserObjectInformationW, OpenInputDesktop, DESKTOP_CONTROL_FLAGS,
    DESKTOP_READOBJECTS, HDESK, UOI_NAME,
};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetCursorInfo, GetIconInfo, GetWindowRect, CURSORINFO, CURSOR_SHOWING, DI_NORMAL, HICON, ICONINFO,
    PW_RENDERFULLCONTENT,
};

use super::sys::{self, api_error};
use crate::error::{CaptureError, Result};
use crate::platform::windows_convert as convert;

/// The largest side of a canvas. No desktop comes near it; the limit only keeps a corrupt
/// rectangle from asking GDI for gigabytes.
const MAX_SIDE: u32 = 65_536;

/// The screen's device context, released when dropped.
struct ScreenDc(HDC);

impl ScreenDc {
    fn get() -> Result<ScreenDc> {
        // SAFETY: asking for the device context of the whole screen has no preconditions.
        let dc = unsafe { GetDC(None) };
        if dc.is_invalid() {
            return Err(CaptureError::Backend("GetDC gave no device context for the screen".into()));
        }
        Ok(ScreenDc(dc))
    }
}

impl Drop for ScreenDc {
    fn drop(&mut self) {
        // SAFETY: the context came from `GetDC(None)` on this thread and is released once.
        unsafe { ReleaseDC(None, self.0) };
    }
}

/// A memory device context with a top-down 32-bit bitmap selected into it, whose pixels this
/// process can read directly. Reused from frame to frame by the GDI stream.
pub(super) struct Canvas {
    dc: HDC,
    bitmap: HBITMAP,
    /// The bitmap the context held before, put back before the context is deleted.
    previous: HGDIOBJ,
    bits: *mut u8,
    width: u32,
    height: u32,
}

impl Canvas {
    pub(super) fn new(width: u32, height: u32) -> Result<Canvas> {
        if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
            return Err(CaptureError::InvalidArgument(format!("a {width}×{height} image cannot be captured")));
        }
        let screen = ScreenDc::get()?;
        // SAFETY: `screen` holds a valid device context for the duration of the call.
        let dc = unsafe { CreateCompatibleDC(Some(screen.0)) };
        if dc.is_invalid() {
            return Err(CaptureError::Backend("CreateCompatibleDC gave no device context".into()));
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                // A negative height makes the first row in memory the top row of the picture.
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut c_void = std::ptr::null_mut();
        // SAFETY: `info` describes an uncompressed 32-bit bitmap, which needs no colour table
        // beyond the one entry the struct holds, and `bits` receives the address of the pixels,
        // which the system owns for as long as the bitmap lives.
        let created = unsafe { CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) };
        let bitmap = match created {
            Ok(bitmap) if !bits.is_null() => bitmap,
            other => {
                if let Ok(bitmap) = other {
                    // SAFETY: the bitmap was just created and is selected into no context.
                    let _ = unsafe { DeleteObject(bitmap.into()) };
                }
                // SAFETY: the context was created above and is deleted once.
                let _ = unsafe { DeleteDC(dc) };
                return Err(CaptureError::Backend(format!(
                    "CreateDIBSection could not make a {width}×{height} bitmap: {}",
                    Error::from_thread().message()
                )));
            }
        };
        // SAFETY: both handles are valid and owned by this canvas.
        let previous = unsafe { SelectObject(dc, bitmap.into()) };
        Ok(Canvas { dc, bitmap, previous, bits: bits.cast(), width, height })
    }

    /// Copies the screen from (`x`, `y`) in virtual-screen pixels into the canvas, layered
    /// windows included (`CAPTUREBLT`). Windows that are excluded from capture
    /// (`WDA_EXCLUDEFROMCAPTURE`, which the app gives its own) are left out by the system.
    ///
    /// Fails when the session's desktop cannot be read: on the lock screen and while a prompt
    /// of the secure desktop shows, `BitBlt` either fails or hands back black pixels, and a
    /// black image must not pass for a capture.
    pub(super) fn copy_screen(&mut self, x: i32, y: i32) -> Result<()> {
        let screen = ScreenDc::get()?;
        // SAFETY: both contexts are valid, and the destination bitmap is as large as the copy.
        let copied = unsafe {
            BitBlt(self.dc, 0, 0, self.width as i32, self.height as i32, Some(screen.0), x, y, SRCCOPY | CAPTUREBLT)
        };
        // The desktop is only asked about when the picture gives a reason to, so an oddly
        // configured session whose captures work is never refused on the strength of this check.
        if (copied.is_err() || convert::is_black(self.pixels())) && input_desktop_is_ours() == Some(false) {
            return Err(CaptureError::Backend(
                "the screen cannot be read while the lock screen or a secure prompt is showing".into(),
            ));
        }
        copied.map_err(|error| api_error("BitBlt", &error))
    }

    /// Draws the pointer where it is on screen, for a canvas whose top-left pixel is the
    /// virtual-screen pixel (`origin_x`, `origin_y`). Does nothing while the pointer is hidden;
    /// a pointer that cannot be drawn is left out rather than failing the capture.
    pub(super) fn draw_cursor(&mut self, origin_x: i32, origin_y: i32) {
        let mut cursor = CURSORINFO { cbSize: std::mem::size_of::<CURSORINFO>() as u32, ..Default::default() };
        // SAFETY: `cursor` is a `CURSORINFO` with its `cbSize` set, which the call fills in.
        if unsafe { GetCursorInfo(&mut cursor) }.is_err()
            || cursor.flags.0 & CURSOR_SHOWING.0 == 0
            || cursor.hCursor.is_invalid()
        {
            return;
        }
        let icon = HICON(cursor.hCursor.0);
        let mut shape = ICONINFO::default();
        // SAFETY: `icon` is the handle of the pointer on screen, and the call fills `shape` with
        // the hotspot and with two bitmaps this function owns from then on.
        let hotspot = match unsafe { GetIconInfo(icon, &mut shape) } {
            Ok(()) => {
                for bitmap in [shape.hbmMask, shape.hbmColor] {
                    if !bitmap.is_invalid() {
                        // SAFETY: `GetIconInfo` created the bitmap for the caller to delete.
                        let _ = unsafe { DeleteObject(bitmap.into()) };
                    }
                }
                (shape.xHotspot as i32, shape.yHotspot as i32)
            }
            Err(_) => (0, 0),
        };
        let (left, top) = (cursor.ptScreenPos.x - hotspot.0 - origin_x, cursor.ptScreenPos.y - hotspot.1 - origin_y);
        // SAFETY: the context and the icon are valid; a width and height of 0 without
        // `DI_DEFAULTSIZE` draws the pointer at its own size, and GDI clips it to the bitmap.
        let _ = unsafe { DrawIconEx(self.dc, left, top, icon, 0, 0, 0, None, DI_NORMAL) };
    }

    /// Asks the window to be drawn into the canvas, which must have the size of the window's
    /// rectangle (`GetWindowRect`). `PW_RENDERFULLCONTENT` takes the picture the compositor
    /// holds, so windows drawn with Direct3D or DirectComposition come out as well.
    fn print_window(&mut self, window: HWND) -> Result<()> {
        // SAFETY: the context is valid, and the system checks the window handle itself.
        if unsafe { PrintWindow(window, self.dc, PRINT_WINDOW_FLAGS(PW_RENDERFULLCONTENT)) }.as_bool() {
            Ok(())
        } else {
            Err(CaptureError::Backend("PrintWindow could not draw the window".into()))
        }
    }

    /// The canvas as tight BGRA rows, with whatever alpha GDI left. Row 0 is the top row.
    pub(super) fn pixels(&self) -> &[u8] {
        // SAFETY: flushing the calling thread's batched drawing has no preconditions.
        let _ = unsafe { GdiFlush() };
        // SAFETY: `bits` points at the bitmap's `width × height` 32-bit pixels (rows of a 32-bit
        // DIB carry no padding), which live until the bitmap is deleted in `drop`. GDI drawing
        // into them was flushed above and only happens through `&mut self`.
        unsafe { std::slice::from_raw_parts(self.bits, self.width as usize * self.height as usize * 4) }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // SAFETY: the handles are owned by this canvas and released once, in the order GDI
        // asks for: the bitmap leaves the context before it is deleted.
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// The rectangle `rect` of the virtual screen as tight, opaque BGRA rows, with the pointer drawn
/// in when `cursor` is set. One `BitBlt` takes it all, so every monitor it spans shows the same
/// moment.
pub(super) fn screen(rect: RECT, cursor: bool) -> Result<Vec<u8>> {
    let area =
        sys::physical(rect).ok_or_else(|| CaptureError::InvalidArgument("the rectangle to capture is empty".into()))?;
    let mut canvas = Canvas::new(area.width, area.height)?;
    canvas.copy_screen(rect.left, rect.top)?;
    if cursor {
        canvas.draw_cursor(rect.left, rect.top);
    }
    let mut pixels = canvas.pixels().to_vec();
    convert::make_opaque(&mut pixels);
    Ok(pixels)
}

/// The part `visible` (virtual-screen pixels) of a window, drawn by the window itself, as tight
/// opaque BGRA rows together with the rectangle they cover. Unlike a crop of the screen, this
/// shows the window also where others cover it. Fails when the window draws nothing (some
/// protected and some minimised windows), so the caller can fall back.
pub(super) fn printed_window(window: HWND, visible: RECT, cursor: bool) -> Result<(Vec<u8>, RECT)> {
    let mut whole = RECT::default();
    // SAFETY: the call writes one `RECT` and checks the window handle itself.
    unsafe { GetWindowRect(window, &mut whole) }.map_err(|error| api_error("GetWindowRect", &error))?;
    let area = sys::physical(whole).ok_or_else(|| CaptureError::Backend("the window has no size".into()))?;
    let mut canvas = Canvas::new(area.width, area.height)?;
    canvas.print_window(window)?;
    if convert::is_black(canvas.pixels()) {
        return Err(CaptureError::Backend("PrintWindow gave an empty image".into()));
    }
    if cursor {
        canvas.draw_cursor(whole.left, whole.top);
    }
    // The window's rectangle includes the invisible borders it is resized by; the visible frame
    // lies inside it.
    let covered = sys::intersect(visible, whole)
        .ok_or_else(|| CaptureError::Backend("the window's frame lies outside its rectangle".into()))?;
    let part = sys::physical(covered)
        .ok_or_else(|| CaptureError::Backend("the window's frame lies outside its rectangle".into()))?;
    let mut pixels = convert::copy_region(
        canvas.pixels(),
        area.width as usize * 4,
        (part.x - whole.left) as u32,
        (part.y - whole.top) as u32,
        part.width,
        part.height,
    )
    .ok_or_else(|| CaptureError::Backend("the window's frame lies outside its image".into()))?;
    convert::make_opaque(&mut pixels);
    Ok((pixels, covered))
}

/// A desktop handle that is closed when dropped.
struct Desktop(HDESK);

impl Drop for Desktop {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenInputDesktop` and is closed once.
        let _ = unsafe { CloseDesktop(self.0) };
    }
}

/// Whether the desktop that receives input now is the one this thread draws on. `Some(false)`
/// on the lock screen and while a prompt of the secure desktop shows (the session may not even
/// open that desktop); `None` when the system does not say.
fn input_desktop_is_ours() -> Option<bool> {
    // SAFETY: the call opens a handle, which `Desktop` closes.
    let input = match unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS) } {
        Ok(handle) => Desktop(handle),
        Err(error) => return (error.code() == E_ACCESSDENIED).then_some(false),
    };
    // SAFETY: the calls have no preconditions; the returned handle belongs to the thread and is
    // not closed.
    let ours = unsafe { GetThreadDesktop(GetCurrentThreadId()) }.ok()?;
    Some(desktop_name(input.0)?.eq_ignore_ascii_case(&desktop_name(ours)?))
}

fn desktop_name(desktop: HDESK) -> Option<String> {
    let mut name = [0u16; 256];
    let mut needed = 0u32;
    // SAFETY: the buffer is `size_of_val(&name)` bytes long, as the call is told, and a desktop's
    // name is a NUL-terminated UTF-16 string.
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            Some(&mut needed),
        )
    }
    .ok()?;
    Some(sys::text(&name))
}
