//! Recording streams on Linux: a grab loop on its own X connection for an X11 session, the
//! portal's PipeWire node for a Wayland session, and the sound server's sources beside either.
//!
//! Every frame is BGRA in memory (`VideoBuffer::Cpu`), cropped to the target and brought to the
//! even size the configuration asks for; every buffer carries `CLOCK_MONOTONIC` time.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::host_now;
use super::media::{AudioCapture, NodeCapture, OnError, Silence};
use super::pixels::{crop_padded, fit_frame, stream_size};
use super::portal::{self, Cast, CastRequest};
use super::x11::{Layout, PixelRect, XDisplay, XError, XResult, X11};
use crate::display::DisplayId;
use crate::error::{CaptureError, Result};
use crate::frame::ColorSpace;
use crate::geometry::{selection_to_frame, LogicalRect, LogicalSize, PhysicalRect, PhysicalSize, ScaleFactor};
use crate::stream::{
    CaptureStream, HostTime, MicrophoneSource, StreamConfig, StreamPixelFormat, StreamSink, StreamTarget, VideoBuffer,
    VideoFrame,
};

/// How often a broken X connection is opened again before the stream gives up.
const RECONNECTS: u32 = 3;

/// The sink of a stream and whether it has heard of the end, which it hears of exactly once.
struct Core {
    sink: Arc<dyn StreamSink>,
    ended: AtomicBool,
}

impl Core {
    fn new(sink: Box<dyn StreamSink>) -> Arc<Core> {
        Arc::new(Core { sink: Arc::from(sink), ended: AtomicBool::new(false) })
    }

    fn fail(&self, error: CaptureError) {
        if !self.ended.swap(true, Ordering::SeqCst) {
            self.sink.stopped(Some(error));
        }
    }

    fn finish(&self) {
        if !self.ended.swap(true, Ordering::SeqCst) {
            self.sink.stopped(None);
        }
    }

    fn failing(self: &Arc<Self>) -> OnError {
        let core = self.clone();
        Arc::new(move |error| core.fail(error))
    }

    fn deliver(&self, frame: crate::frame::Frame, at: HostTime) {
        self.sink.video(VideoFrame {
            timestamp: at,
            size: frame.size,
            format: StreamPixelFormat::Bgra,
            color_space: ColorSpace::Srgb,
            buffer: VideoBuffer::Cpu(Arc::new(frame)),
        });
    }
}

/// The sound of a stream: system audio, with silence in its place when the sound server cannot
/// give it, and the microphone.
struct Sound {
    sink: Arc<dyn StreamSink>,
    system: Option<AudioCapture>,
    wants_system: bool,
    /// Whether the system audio's own source delivers; the silence fills in while it does not.
    live: Arc<AtomicBool>,
    silence: Option<Silence>,
    microphone: Option<AudioCapture>,
}

impl Sound {
    /// Opens what `config` asks for. A microphone that cannot be opened is an error, as on
    /// macOS; system audio that cannot be opened is not, because the video must still record.
    fn open(config: &StreamConfig, core: &Arc<Core>) -> Result<Sound> {
        let live = Arc::new(AtomicBool::new(false));
        let system = if config.capture_system_audio {
            let lost = live.clone();
            AudioCapture::system(core.sink.clone(), Arc::new(move |_| lost.store(false, Ordering::SeqCst))).ok()
        } else {
            None
        };
        let microphone = match &config.microphone {
            Some(source) => {
                Some(AudioCapture::microphone(source.device_id.as_deref(), core.sink.clone(), core.failing())?)
            }
            None => None,
        };
        Ok(Sound {
            sink: core.sink.clone(),
            system,
            wants_system: config.capture_system_audio,
            live,
            silence: None,
            microphone,
        })
    }

    fn start(&mut self) -> Result<()> {
        if let Some(system) = &self.system {
            self.live.store(system.start().is_ok(), Ordering::SeqCst);
        }
        if self.wants_system && self.silence.is_none() {
            self.silence = Some(Silence::start(self.sink.clone(), self.live.clone()));
        }
        if let Some(microphone) = &self.microphone {
            microphone.start()?;
        }
        Ok(())
    }

    fn stop(&mut self) {
        if let Some(mut silence) = self.silence.take() {
            silence.stop();
        }
        if let Some(system) = &self.system {
            system.stop();
        }
        if let Some(microphone) = &self.microphone {
            microphone.stop();
        }
    }
}

/// What an X11 stream grabs for each frame.
#[derive(Clone, Copy, Debug)]
enum Region {
    /// A display or a part of one, in root pixels.
    Fixed(PixelRect),
    /// A window that is followed as it moves. The region keeps the size the window had when the
    /// stream opened, since a recording cannot change its frame size.
    Window { id: u32, offset: (i32, i32), size: (i32, i32) },
}

/// What an X11 stream records, found once when it opens.
pub(super) struct X11Plan {
    region: Region,
    scale: u32,
}

struct X11Stream {
    core: Arc<Core>,
    config: Arc<Mutex<StreamConfig>>,
    region: Region,
    target: PhysicalSize,
    scale: ScaleFactor,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    sound: Sound,
    running: bool,
}

/// Finds the display, the area or the window of `config` on the backend's connection.
pub(super) fn plan_x11(x: &mut X11, config: &StreamConfig) -> XResult<X11Plan> {
    let layout = x.layout()?;
    let display = |id: DisplayId| -> XResult<&XDisplay> {
        layout.displays.iter().find(|d| d.id == id.0).ok_or(XError::Capture(CaptureError::DisplayNotFound(id)))
    };
    let region = match &config.target {
        StreamTarget::Display { display: id } => Region::Fixed(display(*id)?.rect),
        StreamTarget::Area { display: id, rect } => {
            let display = display(*id)?;
            let crop = area_crop(*rect, display, layout.scale, display.rect.size())?;
            Region::Fixed(PixelRect::new(
                display.rect.x + crop.x,
                display.rect.y + crop.y,
                crop.width as i32,
                crop.height as i32,
            ))
        }
        StreamTarget::Window { window } => {
            let found = x
                .describe(&[window.0])?
                .pop()
                .flatten()
                .ok_or(XError::Capture(CaptureError::WindowNotFound(*window)))?;
            if !found.viewable || found.frame.is_empty() {
                return Err(CaptureError::InvalidArgument("the window is not on screen".into()).into());
            }
            Region::Window {
                id: window.0,
                offset: (found.frame.x - found.surface.x, found.frame.y - found.surface.y),
                size: (found.frame.width, found.frame.height),
            }
        }
    };
    Ok(X11Plan { region, scale: layout.scale })
}

/// Opens a stream that grabs the X screen, stopped. Its thread opens its own connection, so
/// stills never wait for a recording.
pub(super) fn open_x11(
    plan: X11Plan,
    config: StreamConfig,
    sink: Box<dyn StreamSink>,
) -> Result<Box<dyn CaptureStream>> {
    let native = match plan.region {
        Region::Fixed(rect) => rect.size(),
        Region::Window { size, .. } => PhysicalSize::new(size.0 as u32, size.1 as u32),
    };
    let core = Core::new(sink);
    let sound = Sound::open(&config, &core)?;
    Ok(Box::new(X11Stream {
        target: stream_size(native, config.output_size, config.size_divisor),
        scale: ScaleFactor(plan.scale as f64),
        core,
        config: Arc::new(Mutex::new(config)),
        region: plan.region,
        stop: Arc::new(AtomicBool::new(false)),
        thread: None,
        sound,
        running: false,
    }))
}

/// The pixels of `display`'s frame (of `frame` pixels) an area selection covers.
fn area_crop(rect: LogicalRect, display: &XDisplay, scale: u32, frame: PhysicalSize) -> Result<PhysicalRect> {
    let scale = scale.max(1) as f64;
    let logical = LogicalSize::new(display.rect.width as f64 / scale, display.rect.height as f64 / scale);
    selection_to_frame(rect, logical, frame)
        .ok_or_else(|| CaptureError::InvalidArgument("the area lies outside its display".into()))
}

/// One frame of `region`: the grab, padded where the region leaves the screen.
fn grab_frame(x: &mut X11, region: Region, with_pointer: bool) -> XResult<(Vec<u8>, PhysicalSize, HostTime)> {
    x.drain_events()?;
    let rect = match region {
        Region::Fixed(rect) => rect,
        Region::Window { id, offset, size } => {
            let surface = x.surface_of(id)?;
            PixelRect::new(surface.x + offset.0, surface.y + offset.1, size.0, size.1)
        }
    };
    let layout = x.layout()?;
    let visible = rect.intersection(&layout.root_rect());
    if visible.is_empty() {
        return match region {
            // The display's place is no longer part of the screen: it was unplugged.
            Region::Fixed(_) => {
                Err(CaptureError::Backend("the recorded display is no longer part of the screen".into()).into())
            }
            // A window dragged off the screen keeps recording, as black, until it comes back.
            Region::Window { .. } => {
                let whole = PhysicalRect::new(0, 0, rect.width as u32, rect.height as u32);
                Ok((crop_padded(&[], PhysicalSize::new(0, 0), whole), rect.size(), host_now()))
            }
        };
    }
    let at = host_now();
    let grab = x.grab_root(visible, with_pointer)?;
    if visible == rect {
        return Ok((grab.data, grab.size, at));
    }
    let local = PhysicalRect::new(rect.x - visible.x, rect.y - visible.y, rect.width as u32, rect.height as u32);
    Ok((crop_padded(&grab.data, grab.size, local), rect.size(), at))
}

/// One frame on the stream's own connection, which is opened when there is none.
fn frame_on(x: &mut Option<X11>, region: Region, with_pointer: bool) -> XResult<(Vec<u8>, PhysicalSize, HostTime)> {
    if x.is_none() {
        // A server that does not answer yet counts as a broken connection, so it is tried again.
        *x = Some(X11::connect().map_err(|e| XError::Broken(e.into_capture().to_string()))?);
    }
    match x.as_mut() {
        Some(x) => grab_frame(x, region, with_pointer),
        None => Err(XError::Broken("there is no connection".into())),
    }
}

/// The grab loop: one frame every `1 / fps` seconds until `stop` is set or the server is lost.
fn run_x11(
    core: Arc<Core>,
    config: Arc<Mutex<StreamConfig>>,
    region: Region,
    target: PhysicalSize,
    scale: ScaleFactor,
    stop: Arc<AtomicBool>,
) {
    let mut x: Option<X11> = None;
    let mut reconnects = 0;
    let mut next = Instant::now();
    while !stop.load(Ordering::SeqCst) {
        let (fps, with_pointer) = {
            let config = config.lock().unwrap_or_else(|e| e.into_inner());
            (config.fps.clamp(1, 240), config.shows_cursor)
        };
        let now = Instant::now();
        if next > now {
            std::thread::sleep(next - now);
        }
        // A frame that took longer than its slot moves the schedule on; frames are never sent
        // in a burst to catch up.
        next = (next + Duration::from_nanos(1_000_000_000 / fps as u64)).max(Instant::now());

        match frame_on(&mut x, region, with_pointer) {
            Ok((data, size, at)) => {
                reconnects = 0;
                match fit_frame(data, size, target, scale) {
                    Ok(frame) => core.deliver(frame, at),
                    Err(error) => return core.fail(error),
                }
            }
            Err(XError::Broken(_)) if reconnects < RECONNECTS => {
                // The connection broke (the server restarted its end): try a new one.
                x = None;
                reconnects += 1;
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => return core.fail(error.into_capture()),
        }
    }
}

impl CaptureStream for X11Stream {
    fn start(&mut self) -> Result<()> {
        if self.running {
            return Ok(());
        }
        self.stop.store(false, Ordering::SeqCst);
        let (core, config, stop) = (self.core.clone(), self.config.clone(), self.stop.clone());
        let (region, target, scale) = (self.region, self.target, self.scale);
        let thread = std::thread::Builder::new()
            .name("glimpsfy-x11-stream".into())
            .spawn(move || run_x11(core, config, region, target, scale, stop))
            .map_err(|e| CaptureError::Backend(format!("the stream's thread could not start: {e}")))?;
        self.thread = Some(thread);
        self.running = true;
        if let Err(error) = self.sound.start() {
            let _ = self.stop();
            return Err(error);
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        self.sound.stop();
        self.running = false;
        self.core.finish();
        Ok(())
    }

    fn update(&mut self, config: StreamConfig) -> Result<()> {
        // The frame rate and the pointer follow at the next frame; the target and the size stay
        // what the stream was opened with.
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = config;
        Ok(())
    }
}

impl Drop for X11Stream {
    fn drop(&mut self) {
        if self.running {
            let _ = self.stop();
        }
    }
}

struct PortalStream {
    core: Arc<Core>,
    cast: Cast,
    node: NodeCapture,
    sound: Sound,
    running: bool,
}

/// The display the user picked in the system's dialog, when the stream's place and size on the
/// compositor's desktop name exactly one of the X displays. The two layouts use different units
/// (the compositor's logical units, XWayland's pixels), so the test allows for one factor
/// between them per display.
fn picked_display(layout: &Layout, position: Option<(i32, i32)>, size: Option<(i32, i32)>) -> Option<&XDisplay> {
    if let [only] = layout.displays.as_slice() {
        return Some(only);
    }
    let ((x, y), (width, height)) = (position?, size?);
    if width <= 0 || height <= 0 {
        return None;
    }
    let near = |a: f64, b: i32| (a - b as f64).abs() <= 2.0;
    let mut matching = layout.displays.iter().filter(|d| {
        let factor = d.rect.width as f64 / width as f64;
        near(height as f64 * factor, d.rect.height)
            && near(x as f64 * factor, d.rect.x)
            && near(y as f64 * factor, d.rect.y)
    });
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

fn token_key(display: &XDisplay) -> String {
    format!("display {}", display.connector)
}

/// Opens a stream through the ScreenCast portal. The system's picker appears here, unless a
/// restore token of the same display still holds.
pub(super) fn open_portal(
    layout: &Layout,
    config: StreamConfig,
    sink: Box<dyn StreamSink>,
) -> Result<Box<dyn CaptureStream>> {
    let requested = match &config.target {
        StreamTarget::Display { display } | StreamTarget::Area { display, .. } => {
            Some(layout.displays.iter().find(|d| d.id == display.0).ok_or(CaptureError::DisplayNotFound(*display))?)
        }
        StreamTarget::Window { .. } => None,
    };
    let core = Core::new(sink);
    let sound = Sound::open(&config, &core)?;
    let mut cast = portal::open_cast(&CastRequest {
        window: requested.is_none(),
        with_pointer: config.shows_cursor,
        token_key: requested.map(token_key),
    })?;

    // Which display the frames show decides where an area is cropped and whose token this is.
    let mut shown = requested;
    if let Some(requested) = requested {
        let picked = picked_display(layout, cast.position, cast.size);
        if let Some(picked) = picked.filter(|p| p.id != requested.id) {
            // The token (or the user) gave another display than the one asked for.
            portal::forget_token(&token_key(requested));
            if matches!(config.target, StreamTarget::Area { .. }) {
                cast.close();
                return Err(CaptureError::InvalidArgument(
                    "the screen chosen in the system's dialog is not the one the area was selected on".into(),
                ));
            }
            shown = Some(picked);
        }
        if let Some(shown) = shown {
            cast.keep_token(&token_key(shown));
        }
    }

    let area = match &config.target {
        StreamTarget::Area { rect, .. } => Some(*rect),
        _ => None,
    };
    let logical = shown.map(|d| {
        let scale = layout.scale.max(1) as f64;
        LogicalSize::new(d.rect.width as f64 / scale, d.rect.height as f64 / scale)
    });
    let (output, divisor) = (config.output_size, config.size_divisor);
    let target: OnceLock<PhysicalSize> = OnceLock::new();
    let frames = core.clone();
    let on_frame = Box::new(move |data: &[u8], stride: usize, size: PhysicalSize, at: HostTime| {
        // An area is cropped out of the monitor's frame by the same maths as a still's selection.
        let crop = match (area, logical) {
            (Some(rect), Some(logical)) => match selection_to_frame(rect, logical, size) {
                Some(crop) => crop,
                None => return,
            },
            _ => PhysicalRect::new(0, 0, size.width, size.height),
        };
        let row = crop.width as usize * 4;
        let mut tight = Vec::with_capacity(row * crop.height as usize);
        for y in 0..crop.height as usize {
            let start = (crop.y as usize + y) * stride + crop.x as usize * 4;
            let Some(line) = data.get(start..start + row) else { return };
            tight.extend_from_slice(line);
        }
        // The compositor's buffer can carry alpha 0 where an XWayland window lies (seen with
        // sway's screen copy), and the area average of a scaled frame weighs colour by alpha, so
        // such a window would turn black. A screen is opaque, as the still path also says.
        for pixel in tight.as_chunks_mut::<4>().0 {
            pixel[3] = 255;
        }
        let scale = match logical {
            Some(logical) if logical.width > 0.0 => ScaleFactor(size.width as f64 / logical.width),
            _ => ScaleFactor::ONE,
        };
        // The first frame fixes the size of the recording; a later frame of another size (a
        // window that was resized) is fitted into it.
        let target = *target.get_or_init(|| stream_size(crop.size(), output, divisor));
        match fit_frame(tight, crop.size(), target, scale) {
            Ok(frame) => frames.deliver(frame, at),
            Err(error) => frames.fail(error),
        }
    });
    let remote = cast
        .remote
        .try_clone()
        .map_err(|e| CaptureError::Backend(format!("the PipeWire connection could not be shared: {e}")))?;
    let node = NodeCapture::open(remote, cast.node, config.fps, on_frame, core.failing())?;
    Ok(Box::new(PortalStream { core, cast, node, sound, running: false }))
}

impl CaptureStream for PortalStream {
    fn start(&mut self) -> Result<()> {
        if self.running {
            return Ok(());
        }
        self.node.start()?;
        self.running = true;
        if let Err(error) = self.sound.start() {
            let _ = self.stop();
            return Err(error);
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.node.stop();
        self.sound.stop();
        self.cast.close();
        self.running = false;
        self.core.finish();
        Ok(())
    }

    fn update(&mut self, config: StreamConfig) -> Result<()> {
        // The portal fixed the source and the pointer when the session started; only the frame
        // rate can still follow.
        self.node.set_fps(config.fps);
        Ok(())
    }
}

impl Drop for PortalStream {
    fn drop(&mut self) {
        // The cast and the pipeline close themselves; the sink only hears of a stream that ran.
        if self.running {
            let _ = self.stop();
        }
    }
}

struct MicrophoneStream {
    capture: AudioCapture,
    core: Arc<Core>,
    running: bool,
}

/// Opens a microphone on its own, stopped.
pub(super) fn open_microphone(source: MicrophoneSource, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
    let core = Core::new(sink);
    let capture = AudioCapture::microphone(source.device_id.as_deref(), core.sink.clone(), core.failing())?;
    Ok(Box::new(MicrophoneStream { capture, core, running: false }))
}

impl CaptureStream for MicrophoneStream {
    fn start(&mut self) -> Result<()> {
        if !self.running {
            self.capture.start()?;
            self.running = true;
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.capture.stop();
        self.running = false;
        self.core.finish();
        Ok(())
    }

    fn update(&mut self, _config: StreamConfig) -> Result<()> {
        // A microphone has nothing to change while it runs; muting is the recorder's.
        Ok(())
    }
}

impl Drop for MicrophoneStream {
    fn drop(&mut self) {
        if self.running {
            let _ = self.stop();
        }
    }
}
