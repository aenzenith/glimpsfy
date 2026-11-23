//! What every part of the Windows backend shares: errors in the API's own words, the scopes
//! that give back what they took (a thread's DPI context, its COM apartment, a kernel handle),
//! the ids behind `DisplayId` and `WindowId`, UTF-16 strings, rectangles and the QPC host clock.

use std::ffi::c_void;
use std::marker::PhantomData;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use windows::core::Error;
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND, RECT};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::Com::{CoIncrementMTAUsage, CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};

use crate::display::{DisplayId, WindowId};
use crate::error::{CaptureError, Result};
use crate::geometry::PhysicalRect;
use crate::stream::HostTime;

/// A failed call as a `CaptureError`, with the system's message and the code.
pub(super) fn api_error(what: &str, error: &Error) -> CaptureError {
    let message = error.message();
    let code = error.code().0 as u32;
    if message.is_empty() {
        CaptureError::Backend(format!("{what} failed with 0x{code:08X}"))
    } else {
        CaptureError::Backend(format!("{what} failed: {} (0x{code:08X})", message.trim_end()))
    }
}

/// Makes the calling thread per-monitor DPI aware (version 2) until it is dropped.
///
/// The app's manifest already says so for the whole process, but a test or the example binary
/// has no manifest, and Windows then scales every rectangle it reports (monitors, windows, the
/// pointer) to 96 DPI and stretches what GDI reads from the screen. With this scope the backend
/// sees physical pixels whatever the process declared.
pub(super) struct DpiScope {
    previous: DPI_AWARENESS_CONTEXT,
}

impl DpiScope {
    pub(super) fn per_monitor() -> DpiScope {
        // SAFETY: the call changes a setting of the calling thread only and takes a constant the
        // system defines. It returns null when it refuses, and `drop` then restores nothing.
        let previous = unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        DpiScope { previous }
    }
}

impl Drop for DpiScope {
    fn drop(&mut self) {
        if !self.previous.0.is_null() {
            // SAFETY: `previous` is the context this thread had, returned by the call in
            // `per_monitor` on this same thread (the scope is not `Send`).
            unsafe { SetThreadDpiAwarenessContext(self.previous) };
        }
    }
}

/// Keeps the process's multithreaded apartment alive for good. A thread of this backend that
/// leaves the apartment would otherwise tear it down when it is the last one in it, and the
/// WinRT factories the `windows` crate caches would point at unloaded code.
fn keep_apartment_alive() {
    static KEPT: OnceLock<()> = OnceLock::new();
    KEPT.get_or_init(|| {
        // SAFETY: the call has no preconditions. The cookie is never given back on purpose, so
        // the apartment lasts as long as the process; a failure only loses that guarantee.
        let _ = unsafe { CoIncrementMTAUsage() };
    });
}

/// The calling thread inside the multithreaded apartment until this is dropped. Every thread the
/// backend creates enters it before its first COM or WinRT call. Declare it before the objects
/// that live on the thread, so they are released first.
pub(super) struct Apartment {
    entered: bool,
    /// The apartment belongs to its thread, so the scope must not move to another one.
    _thread: PhantomData<*const ()>,
}

impl Apartment {
    pub(super) fn enter() -> Apartment {
        keep_apartment_alive();
        // SAFETY: the call initialises COM for the calling thread. It fails only when the thread
        // already belongs to a single-threaded apartment, which a thread created here never does;
        // success (also "already initialised") is paired with `CoUninitialize` in `drop`.
        let entered = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        Apartment { entered, _thread: PhantomData }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.entered {
            // SAFETY: pairs with the successful `CoInitializeEx` of this thread in `enter` (the
            // scope is not `Send`, so it is dropped on the thread that made it).
            unsafe { CoUninitialize() };
        }
    }
}

/// Runs `work` on a thread of its own inside the multithreaded apartment, per-monitor DPI aware,
/// and waits for its answer at most `limit`. The caller's thread may be a UI thread in a
/// single-threaded apartment or have no apartment at all; nothing is assumed about it. When the
/// time runs out the thread is left to finish on its own and its answer is dropped.
pub(super) fn on_worker<T: Send + 'static>(
    name: &str,
    limit: Duration,
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = mpsc::sync_channel::<Result<T>>(1);
    let spawned = std::thread::Builder::new().name(format!("glimpsfy-{name}")).spawn(move || {
        let _dpi = DpiScope::per_monitor();
        let _apartment = Apartment::enter();
        let _ = tx.send(work());
    });
    if let Err(error) = spawned {
        return Err(CaptureError::Backend(format!("could not start the {name} thread: {error}")));
    }
    match rx.recv_timeout(limit) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(CaptureError::Timeout(limit.as_millis() as u64)),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(CaptureError::Backend(format!("the {name} thread ended without an answer")))
        }
    }
}

/// A kernel handle that is closed when dropped.
pub(super) struct OwnedHandle(pub(super) HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by this process and is closed once, here. A failure to
        // close leaves nothing to do.
        let _ = unsafe { CloseHandle(self.0) };
    }
}

/// The window behind an id. A `WindowId` is the `HWND` itself: window handles are 32-bit values
/// that 64-bit Windows sign-extends, so the id loses nothing.
pub(super) fn hwnd(id: WindowId) -> HWND {
    HWND(id.0 as i32 as isize as *mut c_void)
}

pub(super) fn window_id(window: HWND) -> WindowId {
    WindowId(window.0 as usize as u32)
}

/// The monitor behind an id. A `DisplayId` is the `HMONITOR`, a 32-bit handle as window handles
/// are; it stays the same until the display layout changes.
pub(super) fn monitor(id: DisplayId) -> HMONITOR {
    HMONITOR(id.0 as i32 as isize as *mut c_void)
}

pub(super) fn display_id(monitor: HMONITOR) -> DisplayId {
    DisplayId(monitor.0 as usize as u32)
}

/// `text` as a NUL-terminated UTF-16 string.
pub(super) fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The UTF-16 string in `buffer` up to its first NUL.
pub(super) fn text(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&unit| unit == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// A `RECT` of virtual-screen pixels as a `PhysicalRect`; `None` when it is empty.
pub(super) fn physical(rect: RECT) -> Option<PhysicalRect> {
    let (width, height) = (rect.right as i64 - rect.left as i64, rect.bottom as i64 - rect.top as i64);
    (width > 0 && height > 0 && width <= u32::MAX as i64 && height <= u32::MAX as i64)
        .then(|| PhysicalRect::new(rect.left, rect.top, width as u32, height as u32))
}

/// The part two rectangles share; `None` when they do not overlap.
pub(super) fn intersect(a: RECT, b: RECT) -> Option<RECT> {
    let rect = RECT {
        left: a.left.max(b.left),
        top: a.top.max(b.top),
        right: a.right.min(b.right),
        bottom: a.bottom.min(b.bottom),
    };
    (rect.right > rect.left && rect.bottom > rect.top).then_some(rect)
}

/// Whether this is Windows 11 (build 22000 and later), which rounds the corners of windows.
pub(super) fn is_windows_11() -> bool {
    static BUILD: OnceLock<u32> = OnceLock::new();
    *BUILD.get_or_init(|| windows_version::OsVersion::current().build) >= 22_000
}

/// How many QPC ticks make a second. The frequency is fixed at boot, and the call cannot fail
/// on any Windows this app runs on; 10 MHz is what every current system reports.
fn ticks_per_second() -> u64 {
    static FREQUENCY: OnceLock<u64> = OnceLock::new();
    *FREQUENCY.get_or_init(|| {
        let mut frequency = 0i64;
        // SAFETY: the call writes one i64 to the pointer it is given.
        let read = unsafe { QueryPerformanceFrequency(&mut frequency) };
        if read.is_ok() && frequency > 0 {
            frequency as u64
        } else {
            10_000_000
        }
    })
}

/// Now on the QPC clock, in nanoseconds. Windows.Graphics.Capture stamps frames and WASAPI
/// stamps packets with this same counter (in 100 ns units), so `HostTime` is one clock for
/// video, system audio and the microphone.
pub(crate) fn host_now() -> HostTime {
    let mut ticks = 0i64;
    // SAFETY: the call writes one i64 to the pointer it is given. It cannot fail on Windows XP
    // and later; if it ever did, the time would read 0 and the recorder would drop the buffer.
    let _ = unsafe { QueryPerformanceCounter(&mut ticks) };
    HostTime((ticks.max(0) as u128 * 1_000_000_000 / ticks_per_second() as u128) as u64)
}

/// A time the system gives in 100 ns units of the QPC clock (a frame's `SystemRelativeTime`, a
/// packet's QPC position) as nanoseconds of `HostTime`.
pub(super) fn host_time_of(hundred_nanoseconds: i64) -> u64 {
    hundred_nanoseconds.max(0) as u64 * 100
}
