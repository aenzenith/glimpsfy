//! Windows.Graphics.Capture on Direct3D 11: a capture session of one monitor or one window, the
//! frames it delivers read back into memory, and a single frame of a window for stills.
//!
//! - The Direct3D device is a hardware one, or WARP (the software rasteriser) where there is no
//!   usable graphics adapter: a virtual machine, a Remote Desktop session.
//! - The frame pool is free-threaded, so it needs no message loop: frames arrive on a thread of
//!   the system's and wake the thread that owns the session through a [`Wake`].
//! - Properties newer than Windows 10 1903 (the pointer switch, the border switch) are asked
//!   for before they are touched; a build without them simply keeps its default.
//! - Everything here runs on threads the backend created inside the multithreaded apartment.

use std::sync::{mpsc, Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use windows::core::{factory, Error, IInspectable, Interface, HSTRING};
use windows::Foundation::Metadata::ApiInformation;
use windows::Foundation::TypedEventHandler;
use windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureAccessKind,
    GraphicsCaptureItem, GraphicsCaptureSession,
};
use windows::Graphics::DirectX::Direct3D11::IDirect3DDevice;
use windows::Graphics::DirectX::DirectXPixelFormat;
use windows::Graphics::SizeInt32;
use windows::Win32::Foundation::{E_FAIL, HMODULE, HWND};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE, D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Multithread, ID3D11Texture2D, D3D11_BOX,
    D3D11_CPU_ACCESS_READ, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ,
    D3D11_SDK_VERSION, D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::Graphics::Dxgi::{IDXGIAdapter, IDXGIDevice};
use windows::Win32::Graphics::Gdi::HMONITOR;
use windows::Win32::System::WinRT::Direct3D11::{CreateDirect3D11DeviceFromDXGIDevice, IDirect3DDxgiInterfaceAccess};
use windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;

use super::sys::api_error;
use crate::error::{CaptureError, Result};
use crate::geometry::{PhysicalRect, PhysicalSize};
use crate::platform::windows_convert as convert;

/// How long the answer to the borderless request is waited for.
const BORDER_CONSENT_WAIT: Duration = Duration::from_secs(2);

/// The runtime class whose properties are asked for before use.
const SESSION_CLASS: &str = "Windows.Graphics.Capture.GraphicsCaptureSession";

/// Whether this Windows has Windows.Graphics.Capture at all (Windows 10 1903 and later, and not
/// every edition: some server and stripped-down installs lack it).
pub(super) fn supported() -> bool {
    static SUPPORTED: OnceLock<bool> = OnceLock::new();
    *SUPPORTED.get_or_init(|| GraphicsCaptureSession::IsSupported().unwrap_or(false))
}

fn session_has(property: &str) -> bool {
    ApiInformation::IsPropertyPresent(&HSTRING::from(SESSION_CLASS), &HSTRING::from(property)).unwrap_or(false)
}

/// Whether a session can leave the pointer out (Windows 10 2004 and later). Older builds always
/// draw it into the frames.
pub(super) fn can_hide_cursor() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| session_has("IsCursorCaptureEnabled"))
}

/// Whether a session can drop the yellow border around what it captures (Windows 11).
fn can_hide_border() -> bool {
    static PRESENT: OnceLock<bool> = OnceLock::new();
    *PRESENT.get_or_init(|| session_has("IsBorderRequired"))
}

/// Asks the system, once per process, to let this app capture without the border. The border
/// switch only takes effect after this request was granted; an app without a package identity,
/// as this one is, gets the answer without a prompt. The answer is not waited for long: without
/// it the border simply stays.
fn request_borderless() {
    static ASKED: OnceLock<()> = OnceLock::new();
    ASKED.get_or_init(|| {
        let Ok(request) = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless) else {
            return;
        };
        let (tx, rx) = mpsc::sync_channel(1);
        let registered = request.when(move |_| {
            let _ = tx.send(());
        });
        if registered.is_ok() {
            let _ = rx.recv_timeout(BORDER_CONSENT_WAIT);
        }
    });
}

/// A Direct3D 11 device and its immediate context.
#[derive(Clone)]
pub(super) struct Device {
    d3d: ID3D11Device,
    context: ID3D11DeviceContext,
}

impl Device {
    /// A hardware device, or the software rasteriser when no adapter gives one.
    pub(super) fn create() -> Result<Device> {
        let (d3d, context) = create_device(D3D_DRIVER_TYPE_HARDWARE)
            .or_else(|_| create_device(D3D_DRIVER_TYPE_WARP))
            .map_err(|error| api_error("D3D11CreateDevice", &error))?;
        // The frame pool uses the device from the system's threads while this backend copies
        // frames on its own, so the context takes its lock on every call. A device that cannot
        // is still usable from one thread at a time, which is how the copies run.
        if let Ok(multithread) = context.cast::<ID3D11Multithread>() {
            // SAFETY: a plain setter on a valid interface; its return value is the old setting.
            let _ = unsafe { multithread.SetMultithreadProtected(true) };
        }
        Ok(Device { d3d, context })
    }

    /// The device as the WinRT object a frame pool takes.
    fn winrt(&self) -> windows::core::Result<IDirect3DDevice> {
        let dxgi: IDXGIDevice = self.d3d.cast()?;
        // SAFETY: `dxgi` is a valid DXGI device, which is all the call needs.
        let inspectable: IInspectable = unsafe { CreateDirect3D11DeviceFromDXGIDevice(&dxgi) }?;
        inspectable.cast()
    }
}

fn create_device(driver: D3D_DRIVER_TYPE) -> windows::core::Result<(ID3D11Device, ID3D11DeviceContext)> {
    let mut device: Option<ID3D11Device> = None;
    let mut context: Option<ID3D11DeviceContext> = None;
    // SAFETY: the out pointers are to two `Option`s that live through the call. BGRA support is
    // what Windows.Graphics.Capture requires of the device; no adapter and no software module
    // are named, so the system picks the default adapter of the driver type.
    unsafe {
        D3D11CreateDevice(
            None::<&IDXGIAdapter>,
            driver,
            HMODULE(std::ptr::null_mut()),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )
    }?;
    device.zip(context).ok_or_else(|| Error::from_hresult(E_FAIL))
}

/// What a session captures: a monitor or a window, as the raw handle (a handle is not `Send`,
/// and the session is made on a thread of its own).
#[derive(Clone, Copy, Debug)]
pub(super) enum Item {
    Monitor(isize),
    Window(isize),
}

fn capture_item(item: Item) -> windows::core::Result<GraphicsCaptureItem> {
    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()?;
    // SAFETY: the calls take a handle the system validates itself and return a new object.
    unsafe {
        match item {
            Item::Monitor(handle) => interop.CreateForMonitor(HMONITOR(handle as *mut _)),
            Item::Window(handle) => interop.CreateForWindow(HWND(handle as *mut _)),
        }
    }
}

/// What woke a session's thread.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct Woken {
    /// The captured monitor or window is gone.
    pub(super) closed: bool,
}

#[derive(Default)]
struct WakeState {
    frames: bool,
    closed: bool,
    poked: bool,
}

/// Wakes the thread that owns a session: when a frame arrived, when the captured item closed,
/// and when the stream's owner wants the thread to look at its flags (a stop, a new setting).
#[derive(Default)]
pub(super) struct Wake {
    state: Mutex<WakeState>,
    changed: Condvar,
}

impl Wake {
    fn set(&self, change: impl FnOnce(&mut WakeState)) {
        change(&mut self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
        self.changed.notify_all();
    }

    /// Wakes the waiting thread without a frame.
    pub(super) fn poke(&self) {
        self.set(|state| state.poked = true);
    }

    /// Forgets what an earlier session left behind.
    fn reset(&self) {
        *self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = WakeState::default();
    }

    /// Waits until something happened or `limit` passed, whichever comes first.
    pub(super) fn wait(&self, limit: Duration) -> Woken {
        let deadline = Instant::now() + limit;
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if state.frames || state.closed || state.poked {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            state = self.changed.wait_timeout(state, left).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
        state.frames = false;
        state.poked = false;
        Woken { closed: state.closed }
    }
}

/// A running capture of one item. Dropping it ends the capture.
pub(super) struct Session {
    device: Device,
    winrt_device: IDirect3DDevice,
    /// Kept alive for the handler registered on it.
    item: GraphicsCaptureItem,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    frame_token: i64,
    closed_token: i64,
    pool_size: SizeInt32,
    buffers: i32,
    /// The texture frames are copied into to be read, and its size.
    staging: Option<(ID3D11Texture2D, u32, u32)>,
}

impl Session {
    /// Starts capturing `item` with frames of its own size in 8-bit BGRA. `wake` is signalled
    /// for every frame and when the item closes.
    pub(super) fn start(device: &Device, item: Item, cursor: bool, buffers: i32, wake: Arc<Wake>) -> Result<Session> {
        let capture_item = capture_item(item).map_err(|error| api_error("creating the capture item", &error))?;
        Session::begin(device, capture_item, cursor, buffers, wake)
            .map_err(|error| api_error("starting the capture session", &error))
    }

    fn begin(
        device: &Device,
        item: GraphicsCaptureItem,
        cursor: bool,
        buffers: i32,
        wake: Arc<Wake>,
    ) -> windows::core::Result<Session> {
        wake.reset();
        let winrt_device = device.winrt()?;
        let size = item.Size()?;
        let pool_size = SizeInt32 { Width: size.Width.max(1), Height: size.Height.max(1) };
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            &winrt_device,
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            buffers,
            pool_size,
        )?;
        let session = pool.CreateCaptureSession(&item)?;
        if can_hide_cursor() {
            // A session that refuses the setting keeps its default, which shows the pointer.
            let _ = session.SetIsCursorCaptureEnabled(cursor);
        }
        if can_hide_border() {
            request_borderless();
            // Without the system's consent the setting is ignored and the border stays.
            let _ = session.SetIsBorderRequired(false);
        }
        let on_frame = wake.clone();
        let frame_token =
            pool.FrameArrived(&TypedEventHandler::<Direct3D11CaptureFramePool, IInspectable>::new(move |_, _| {
                on_frame.set(|state| state.frames = true);
                Ok(())
            }))?;
        let on_closed = wake;
        let closed_token = item.Closed(&TypedEventHandler::<GraphicsCaptureItem, IInspectable>::new(move |_, _| {
            on_closed.set(|state| state.closed = true);
            Ok(())
        }))?;
        let started = Session {
            device: device.clone(),
            winrt_device,
            item,
            pool,
            session,
            frame_token,
            closed_token,
            pool_size,
            buffers,
            staging: None,
        };
        // From here on `Drop` removes the handlers and closes the session, also when the start
        // itself fails.
        started.session.StartCapture()?;
        Ok(started)
    }

    /// Shows or hides the pointer in the frames to come, where the build can.
    pub(super) fn set_cursor(&self, shown: bool) {
        if can_hide_cursor() {
            let _ = self.session.SetIsCursorCaptureEnabled(shown);
        }
    }

    /// The next frame the pool holds, oldest first; `None` when it holds none.
    pub(super) fn next_frame(&self) -> Result<Option<Direct3D11CaptureFrame>> {
        match self.pool.TryGetNextFrame() {
            Ok(frame) => Ok(Some(frame)),
            // An empty pool answers with success and no object, which the bindings turn into
            // an error without a code.
            Err(error) if error.code().is_ok() => Ok(None),
            Err(error) => Err(api_error("taking a captured frame", &error)),
        }
    }

    /// Whether the graphics device was removed or reset (a driver update, a switch of adapters,
    /// a remote session taking over). The session then delivers nothing more and has to be made
    /// again on a new device.
    pub(super) fn device_lost(&self) -> bool {
        // SAFETY: a plain query on a valid device.
        unsafe { self.device.d3d.GetDeviceRemovedReason() }.is_err()
    }

    /// Reads `region` of a frame (all of it for `None`), clipped to what the frame holds, and
    /// hands the pixels to `use_pixels` as BGRA rows `stride` bytes apart together with the size
    /// that was read. The alpha is premultiplied.
    pub(super) fn read<R>(
        &mut self,
        frame: &Direct3D11CaptureFrame,
        region: Option<PhysicalRect>,
        use_pixels: impl FnOnce(&[u8], usize, PhysicalSize) -> R,
    ) -> Result<R> {
        let failed = |error: Error| api_error("reading a captured frame", &error);
        let access: IDirect3DDxgiInterfaceAccess =
            frame.Surface().and_then(|surface| surface.cast()).map_err(failed)?;
        // SAFETY: the surface of a capture frame is backed by a Direct3D 11 texture, which this
        // call hands out with its own reference.
        let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(failed)?;
        let mut described = D3D11_TEXTURE2D_DESC::default();
        // SAFETY: the call fills in the one struct it is given.
        unsafe { texture.GetDesc(&mut described) };
        // The texture has the pool's size; the content is smaller after a window shrank.
        let content = frame.ContentSize().map_err(failed)?;
        let width = described.Width.min(content.Width.max(0) as u32);
        let height = described.Height.min(content.Height.max(0) as u32);
        let (left, top, right, bottom) = match region {
            None => (0, 0, width, height),
            Some(rect) => {
                let left = (rect.x.max(0) as u32).min(width);
                let top = (rect.y.max(0) as u32).min(height);
                (left, top, left.saturating_add(rect.width).min(width), top.saturating_add(rect.height).min(height))
            }
        };
        if right <= left || bottom <= top {
            return Err(CaptureError::Backend("the captured frame is empty".into()));
        }
        let (width, height) = (right - left, bottom - top);

        let staging = match &self.staging {
            Some((texture, w, h)) if (*w, *h) == (width, height) => texture.clone(),
            _ => {
                let description = D3D11_TEXTURE2D_DESC {
                    Width: width,
                    Height: height,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: described.Format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: D3D11_USAGE_STAGING,
                    BindFlags: 0,
                    CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                    MiscFlags: 0,
                };
                let mut created: Option<ID3D11Texture2D> = None;
                // SAFETY: the description is complete and the out pointer lives through the call.
                unsafe { self.device.d3d.CreateTexture2D(&description, None, Some(&mut created)) }.map_err(failed)?;
                let texture = created.ok_or_else(|| failed(Error::from_hresult(E_FAIL)))?;
                self.staging = Some((texture.clone(), width, height));
                texture
            }
        };
        let source = D3D11_BOX { left, top, front: 0, right, bottom, back: 1 };
        // SAFETY: both textures belong to this device and have one mip level; the box lies
        // inside the source (clipped above) and has the size of the destination.
        unsafe { self.device.context.CopySubresourceRegion(&staging, 0, 0, 0, 0, &texture, 0, Some(&source)) };
        let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
        // SAFETY: the staging texture was created readable by the CPU; the call waits for the
        // copy above and fills `mapped`. `Unmap` below pairs with it.
        unsafe { self.device.context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.map_err(failed)?;
        let unmap = Unmap { context: &self.device.context, texture: &staging };
        let stride = mapped.RowPitch as usize;
        if mapped.pData.is_null() || stride < width as usize * 4 {
            return Err(CaptureError::Backend("the captured frame could not be mapped".into()));
        }
        // SAFETY: a mapped 32-bit texture holds `height` rows `RowPitch` bytes apart, each with
        // at least `width × 4` bytes; the slice ends with the last pixel of the last row and is
        // only used before `unmap` is dropped.
        let pixels = unsafe {
            std::slice::from_raw_parts(mapped.pData as *const u8, stride * (height as usize - 1) + width as usize * 4)
        };
        let result = use_pixels(pixels, stride, PhysicalSize::new(width, height));
        drop(unmap);
        Ok(result)
    }

    /// Gives the pool the size `content` of the latest frame when the two differ (the window was
    /// resized, the display changed its mode), so later frames are not cut off. Called after the
    /// frame was closed, as the system's own samples do.
    pub(super) fn fit_pool(&mut self, content: SizeInt32) {
        if content.Width < 1 || content.Height < 1 || content == self.pool_size {
            return;
        }
        let resized =
            self.pool.Recreate(&self.winrt_device, DirectXPixelFormat::B8G8R8A8UIntNormalized, self.buffers, content);
        if resized.is_ok() {
            self.pool_size = content;
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Nothing can be done about a failure here; the objects are released either way.
        let _ = self.pool.RemoveFrameArrived(self.frame_token);
        let _ = self.item.RemoveClosed(self.closed_token);
        let _ = self.session.Close();
        let _ = self.pool.Close();
    }
}

/// Unmaps a staging texture when dropped.
struct Unmap<'a> {
    context: &'a ID3D11DeviceContext,
    texture: &'a ID3D11Texture2D,
}

impl Drop for Unmap<'_> {
    fn drop(&mut self) {
        // SAFETY: pairs with the successful `Map` of subresource 0 of this texture.
        unsafe { self.context.Unmap(self.texture, 0) };
    }
}

/// The device the stills share. Making a device takes tens of milliseconds, which a window
/// capture should not pay every time; a stream makes its own.
static STILL_DEVICE: Mutex<Option<Device>> = Mutex::new(None);

/// Creates the stills' device ahead of the first window capture.
pub(super) fn warm() {
    if !supported() {
        return;
    }
    let mut cached = STILL_DEVICE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if cached.is_none() {
        *cached = Device::create().ok();
    }
}

/// One frame of a window as tight BGRA rows with premultiplied alpha, and its size. The frame
/// covers the window's frame as the compositor draws it (`DWMWA_EXTENDED_FRAME_BOUNDS`), also
/// where other windows cover it or it reaches past the screen's edge. `wait` is how long the
/// first frame may take.
pub(super) fn window_once(window: isize, cursor: bool, wait: Duration) -> Result<(Vec<u8>, PhysicalSize)> {
    // One still at a time uses the shared device; the lock is held for the whole capture.
    let mut cached = STILL_DEVICE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let device = match cached.as_ref() {
        Some(device) => device.clone(),
        None => cached.insert(Device::create()?).clone(),
    };
    let result = frame_of_window(&device, window, cursor, wait);
    if result.is_err() {
        // The device may be the reason (removed, reset by a driver update); the next capture
        // makes a new one.
        *cached = None;
    }
    result
}

fn frame_of_window(device: &Device, window: isize, cursor: bool, wait: Duration) -> Result<(Vec<u8>, PhysicalSize)> {
    let wake = Arc::new(Wake::default());
    let mut session = Session::start(device, Item::Window(window), cursor, 1, wake.clone())?;
    let deadline = Instant::now() + wait;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let woken = wake.wait(left);
        if let Some(frame) = session.next_frame()? {
            let read = session.read(&frame, None, |pixels, stride, size| {
                convert::copy_region(pixels, stride, 0, 0, size.width, size.height).map(|tight| (tight, size))
            });
            let _ = frame.Close();
            return read?.ok_or_else(|| CaptureError::Backend("the captured frame is shorter than its size".into()));
        }
        if woken.closed {
            return Err(CaptureError::Backend("the window closed while it was captured".into()));
        }
        if left.is_zero() {
            return Err(CaptureError::Timeout(wait.as_millis() as u64));
        }
    }
}
