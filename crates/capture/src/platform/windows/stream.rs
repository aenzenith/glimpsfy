//! The recording stream on Windows: frames of a display, of a part of one or of a window, with
//! the system's audio and a microphone beside them, and the microphone on its own.
//!
//! - Video comes from a Windows.Graphics.Capture session on a thread of its own. The session
//!   delivers a frame whenever the screen changes; the thread keeps the newest one and lets one
//!   through per slot of the stream's frame rate (`Pacer`). A still screen sends nothing after
//!   its first frame, which is what the recorder expects: its timeline holds the last frame
//!   until the next one, and the frame that arrives while the countdown runs opens the take.
//! - When a session cannot be made or breaks (no Windows.Graphics.Capture on this install, the
//!   graphics device was removed), the thread starts it again, and after a few tries reads the
//!   screen with GDI at the stream's frame rate instead, so a take goes on.
//! - Frames reach the sink as BGRA pixels in memory (`VideoBuffer::Cpu`) of one size for the
//!   whole stream, even when a recorded window is resized.
//! - `stopped(Some(_))` is called once when the display is unplugged or the window closes.
//! - The app's own windows are kept out by the app, which marks them as excluded from capture;
//!   the stream has no list to apply. A window recording shows the window alone, so the app's
//!   shown windows (the click ring, the webcam bubble) are not in it.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Graphics::Capture::Direct3D11CaptureFrame;
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Gdi::HMONITOR;

use super::audio::{self, Endpoint};
use super::sys::{self, Apartment, DpiScope};
use super::wgc::{self, Wake};
use super::{gdi, monitors, window_list};
use crate::display::{DisplayId, WindowId};
use crate::error::{CaptureError, Result};
use crate::frame::{ColorInfo, ColorSpace, Frame, PixelFormat};
use crate::geometry::{selection_to_frame, PhysicalRect, PhysicalSize, ScaleFactor};
use crate::platform::windows_convert::{self as convert, Pacer};
use crate::stream::{
    CaptureStream, HostTime, MicrophoneSource, StreamConfig, StreamPixelFormat, StreamSink, StreamTarget, VideoBuffer,
    VideoFrame,
};

/// How long the video thread may take to deliver its verdict on the start.
const START_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the video thread sleeps at most while no frame is waiting, so it notices a target
/// that vanished without a word.
const IDLE_WAIT: Duration = Duration::from_millis(250);
/// How often the thread checks that the display or the window is still there.
const ALIVE_CHECK: Duration = Duration::from_secs(1);
/// How long a display's capture session may take to send its first frame.
const FIRST_FRAME_LIMIT: Duration = Duration::from_secs(2);
/// How many times a broken capture session is started again before GDI takes over.
const SESSION_RESTARTS: u32 = 3;
/// The frames the pool holds: the one being read, the one waiting for its slot, and a spare.
const POOL_BUFFERS: i32 = 3;
/// The longest the GDI loop sleeps before it looks at the stop flag again.
const STOP_POLL: Duration = Duration::from_millis(50);

/// What a stream records, resolved when it is opened.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Display {
        monitor: isize,
        display: DisplayId,
    },
    /// A part of a display, as a rectangle of its frame's pixels.
    Area {
        monitor: isize,
        display: DisplayId,
        crop: PhysicalRect,
    },
    Window {
        handle: isize,
        window: WindowId,
    },
}

#[derive(Clone, Copy, Debug)]
struct Source {
    kind: Kind,
    /// The pixel size of what is recorded when the stream opens; the frames keep it.
    native: PhysicalSize,
    /// Pixels per point of the display that shows it.
    scale: f64,
}

fn resolve(target: &StreamTarget) -> Result<Source> {
    let _dpi = DpiScope::per_monitor();
    let monitors = monitors::layout()?;
    let find = |id: DisplayId| {
        monitors.iter().find(|monitor| monitor.display.id == id).ok_or(CaptureError::DisplayNotFound(id))
    };
    match target {
        StreamTarget::Display { display } => {
            let monitor = find(*display)?;
            Ok(Source {
                kind: Kind::Display { monitor: sys::monitor(*display).0 as isize, display: *display },
                native: monitor.display.pixel_size,
                scale: monitor.display.scale.0,
            })
        }
        StreamTarget::Area { display, rect } => {
            let monitor = find(*display)?;
            let crop = selection_to_frame(*rect, monitor.display.frame.size(), monitor.display.pixel_size)
                .ok_or_else(|| CaptureError::InvalidArgument("the area lies outside the display".into()))?;
            Ok(Source {
                kind: Kind::Area { monitor: sys::monitor(*display).0 as isize, display: *display, crop },
                native: crop.size(),
                scale: monitor.display.scale.0,
            })
        }
        StreamTarget::Window { window } => {
            let handle = sys::hwnd(*window);
            if !window_list::exists(handle) {
                return Err(CaptureError::WindowNotFound(*window));
            }
            if window_list::is_minimised(handle) {
                return Err(CaptureError::InvalidArgument("a minimised window cannot be recorded".into()));
            }
            let frame = window_list::visible_frame(handle)
                .and_then(sys::physical)
                .ok_or(CaptureError::WindowNotFound(*window))?;
            let displays: Vec<_> = monitors.iter().map(|monitor| monitor.display.clone()).collect();
            let scale =
                crate::desktop::display_of_physical_rect(&displays, frame).map_or(1.0, |display| display.scale.0);
            Ok(Source {
                kind: Kind::Window { handle: handle.0 as isize, window: *window },
                native: frame.size(),
                scale,
            })
        }
    }
}

/// The error for a target that is gone, or `None` while it is still there.
fn gone(kind: &Kind) -> Option<CaptureError> {
    match *kind {
        Kind::Display { monitor, display } | Kind::Area { monitor, display, .. } => {
            monitors::rect_of(HMONITOR(monitor as *mut _)).is_none().then_some(CaptureError::DisplayNotFound(display))
        }
        Kind::Window { handle, window } => {
            (!window_list::exists(HWND(handle as *mut _))).then_some(CaptureError::WindowNotFound(window))
        }
    }
}

/// What the stream's owner tells the video thread while it runs.
struct Control {
    stop: AtomicBool,
    cursor: AtomicBool,
    fps: AtomicU32,
    wake: Arc<Wake>,
}

/// Tells the sink of the stream's end exactly once, whichever comes first: the stop or a failure.
struct Ending {
    ended: AtomicBool,
    sink: Arc<dyn StreamSink>,
}

impl Ending {
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
}

/// Turns what a capture read into the stream's frames and hands them to the sink.
struct Delivery {
    sink: Arc<dyn StreamSink>,
    native: PhysicalSize,
    output: PhysicalSize,
    scale: ScaleFactor,
    last: u64,
}

impl Delivery {
    /// Sends the `available` pixels in `pixels` (rows `stride` bytes apart) as the frame of
    /// host time `timestamp`. A frame that is not later than the one before it is dropped, as
    /// the recorder's timeline would drop it.
    fn send(&mut self, pixels: &[u8], stride: usize, available: PhysicalSize, timestamp: u64, opaque: bool) {
        if timestamp <= self.last {
            return;
        }
        let data = convert::stream_pixels(pixels, stride, available, self.native, self.output, opaque);
        let row = self.output.width as usize * 4;
        let Ok(frame) = Frame::new(self.output, row, PixelFormat::Bgra8, ColorInfo::srgb(), self.scale, data) else {
            return;
        };
        self.last = timestamp;
        self.sink.video(VideoFrame {
            timestamp: HostTime(timestamp),
            size: self.output,
            format: StreamPixelFormat::Bgra,
            color_space: ColorSpace::Srgb,
            buffer: VideoBuffer::Cpu(Arc::new(frame)),
        });
    }
}

/// Everything the video thread needs.
struct VideoJob {
    source: Source,
    output: PhysicalSize,
    control: Arc<Control>,
    ending: Arc<Ending>,
}

/// How a capture loop ended.
enum Exit {
    /// The owner stopped the stream.
    Stopped,
    /// The session started and then never sent a frame of the display.
    Silent,
    /// The target is gone, or nothing can capture it: the stream is over.
    Ended(CaptureError),
    /// This way of capturing failed; another start or the other way may work.
    Broken(CaptureError),
}

/// Who hears of the start's result: the thread that waits in `start`, once.
type Ready = Option<mpsc::SyncSender<Result<()>>>;

fn started(ready: &mut Ready) {
    if let Some(ready) = ready.take() {
        let _ = ready.send(Ok(()));
    }
}

fn video_thread(job: VideoJob, ready: mpsc::SyncSender<Result<()>>) {
    let _dpi = DpiScope::per_monitor();
    // Declared before every COM object of this thread, so it is dropped after them.
    let _apartment = Apartment::enter();
    let mut ready: Ready = Some(ready);
    let mut delivery = Delivery {
        sink: job.ending.sink.clone(),
        native: job.source.native,
        output: job.output,
        scale: ScaleFactor(job.source.scale * job.output.width as f64 / job.source.native.width.max(1) as f64),
        last: 0,
    };
    let mut restarts = SESSION_RESTARTS;
    let mut session_usable = wgc::supported();
    let ended = loop {
        if job.control.stop.load(Ordering::SeqCst) {
            break None;
        }
        let exit = if session_usable {
            run_session(&job, &mut delivery, &mut ready)
        } else {
            run_gdi(&job, &mut delivery, &mut ready)
        };
        match exit {
            Exit::Stopped => break None,
            Exit::Ended(error) => break Some(error),
            // Some virtual and remote displays accept a session and deliver nothing; GDI reads
            // them all the same.
            Exit::Silent => session_usable = false,
            Exit::Broken(error) if !session_usable => break Some(error),
            Exit::Broken(_) => {
                if let Some(error) = gone(&job.source.kind) {
                    break Some(error);
                }
                // A session that never started will not start on the next try either.
                restarts = if ready.is_some() { 0 } else { restarts.saturating_sub(1) };
                session_usable = restarts > 0;
            }
        }
    };
    match (ended, ready.take()) {
        (Some(error), Some(ready)) => {
            let _ = ready.send(Err(error));
        }
        (Some(error), None) => job.ending.fail(error),
        (None, Some(ready)) => {
            let _ = ready.send(Err(CaptureError::Backend("the stream was stopped while it started".into())));
        }
        (None, None) => {}
    }
}

/// Captures with a Windows.Graphics.Capture session until the stream stops or the session breaks.
fn run_session(job: &VideoJob, delivery: &mut Delivery, ready: &mut Ready) -> Exit {
    let control = &job.control;
    let item = match job.source.kind {
        Kind::Display { monitor, .. } | Kind::Area { monitor, .. } => wgc::Item::Monitor(monitor),
        Kind::Window { handle, .. } => wgc::Item::Window(handle),
    };
    let region = match job.source.kind {
        Kind::Area { crop, .. } => Some(crop),
        _ => None,
    };
    // A window's pixels are premultiplied around its rounded corners; a video has no alpha.
    let opaque = matches!(job.source.kind, Kind::Window { .. });
    let mut cursor = control.cursor.load(Ordering::SeqCst);
    let device = match wgc::Device::create() {
        Ok(device) => device,
        Err(error) => return Exit::Broken(error),
    };
    let mut session = match wgc::Session::start(&device, item, cursor, POOL_BUFFERS, control.wake.clone()) {
        Ok(session) => session,
        Err(error) => return Exit::Broken(error),
    };
    started(ready);

    let mut fps = control.fps.load(Ordering::SeqCst);
    let mut pacer = Pacer::new(fps);
    let mut waiting: Option<Direct3D11CaptureFrame> = None;
    let mut checked = Instant::now();
    // A display's session sends its first frame at once. A window's may not, while the window
    // draws nothing (it sits on another virtual desktop), and reading the screen in its place
    // would record whatever else is there; so only a display's silence counts as a failure.
    let mut first_frame_by = (!opaque).then(|| Instant::now() + FIRST_FRAME_LIMIT);
    loop {
        // With a frame waiting for its slot the thread wakes when the slot is due; a newer frame
        // wakes it earlier and takes the older one's place.
        let limit = match (&waiting, pacer.due()) {
            (Some(_), Some(due)) => Duration::from_nanos(due.saturating_sub(sys::host_now().0)).min(IDLE_WAIT),
            _ => IDLE_WAIT,
        };
        let woken = control.wake.wait(limit);
        if control.stop.load(Ordering::SeqCst) {
            return Exit::Stopped;
        }
        if woken.closed {
            // The system closed the item, whether or not the handle still answers.
            return Exit::Ended(match job.source.kind {
                Kind::Display { display, .. } | Kind::Area { display, .. } => CaptureError::DisplayNotFound(display),
                Kind::Window { window, .. } => CaptureError::WindowNotFound(window),
            });
        }
        if checked.elapsed() >= ALIVE_CHECK {
            checked = Instant::now();
            if let Some(error) = gone(&job.source.kind) {
                return Exit::Ended(error);
            }
            if session.device_lost() {
                return Exit::Broken(CaptureError::Backend("the graphics device was removed".into()));
            }
        }
        let wanted_cursor = control.cursor.load(Ordering::SeqCst);
        if wanted_cursor != cursor {
            cursor = wanted_cursor;
            session.set_cursor(cursor);
        }
        let wanted_fps = control.fps.load(Ordering::SeqCst);
        if wanted_fps != fps {
            fps = wanted_fps;
            pacer = Pacer::new(fps);
        }

        // The pool is emptied at once, so it never has to drop a frame for lack of a buffer.
        loop {
            match session.next_frame() {
                Ok(Some(frame)) => {
                    if let Some(older) = waiting.replace(frame) {
                        let _ = older.Close();
                    }
                }
                Ok(None) => break,
                Err(error) => return Exit::Broken(error),
            }
        }
        let Some(frame) = waiting.as_ref() else {
            if first_frame_by.is_some_and(|limit| Instant::now() >= limit) {
                return Exit::Silent;
            }
            continue;
        };
        first_frame_by = None;
        // The time the compositor drew the frame, on the QPC clock in 100 ns units.
        let timestamp = match frame.SystemRelativeTime() {
            Ok(time) if time.Duration > 0 => sys::host_time_of(time.Duration),
            _ => sys::host_now().0,
        };
        if !pacer.accept(timestamp, sys::host_now().0) {
            continue;
        }
        let content = frame.ContentSize().ok();
        let read = session.read(frame, region, |pixels, stride, available| {
            delivery.send(pixels, stride, available, timestamp, opaque);
        });
        if let Some(frame) = waiting.take() {
            let _ = frame.Close();
        }
        if let Some(content) = content {
            session.fit_pool(content);
        }
        if let Err(error) = read {
            return Exit::Broken(error);
        }
    }
}

/// Reads the screen with GDI at the stream's frame rate until the stream stops.
fn run_gdi(job: &VideoJob, delivery: &mut Delivery, ready: &mut Ready) -> Exit {
    let control = &job.control;
    let native = job.source.native;
    let mut canvas = match gdi::Canvas::new(native.width, native.height) {
        Ok(canvas) => canvas,
        Err(error) => return Exit::Broken(error),
    };
    // Where the recorded pixels start on the virtual screen; read again now and then, because
    // a window moves and a display can be rearranged.
    let origin = |kind: &Kind| -> Option<(i32, i32)> {
        match *kind {
            Kind::Display { monitor, .. } => {
                monitors::rect_of(HMONITOR(monitor as *mut _)).map(|rect| (rect.left, rect.top))
            }
            Kind::Area { monitor, crop, .. } => {
                monitors::rect_of(HMONITOR(monitor as *mut _)).map(|rect| (rect.left + crop.x, rect.top + crop.y))
            }
            Kind::Window { handle, .. } => {
                window_list::visible_frame(HWND(handle as *mut _)).map(|rect| (rect.left, rect.top))
            }
        }
    };
    let follows_window = matches!(job.source.kind, Kind::Window { .. });
    let mut at = origin(&job.source.kind);
    let mut checked = Instant::now();
    let mut next = Instant::now();
    loop {
        if control.stop.load(Ordering::SeqCst) {
            return Exit::Stopped;
        }
        if follows_window || checked.elapsed() >= ALIVE_CHECK {
            if checked.elapsed() >= ALIVE_CHECK {
                checked = Instant::now();
                if let Some(error) = gone(&job.source.kind) {
                    return Exit::Ended(error);
                }
            }
            at = origin(&job.source.kind).or(at);
        }
        if let Some((x, y)) = at {
            let timestamp = sys::host_now().0;
            match canvas.copy_screen(x, y) {
                Ok(()) => {
                    if control.cursor.load(Ordering::SeqCst) {
                        canvas.draw_cursor(x, y);
                    }
                    started(ready);
                    delivery.send(canvas.pixels(), native.width as usize * 4, native, timestamp, true);
                }
                // The first frame decides whether the stream starts at all. Later the screen can
                // be unreadable for a while (the lock screen, a secure prompt); the take goes on
                // and shows its last frame until the desktop is back.
                Err(error) if ready.is_some() => return Exit::Broken(error),
                Err(_) => {}
            }
        } else if ready.is_some() {
            return Exit::Broken(CaptureError::Backend("the recorded area has no place on the screen".into()));
        }
        let interval = Duration::from_nanos(1_000_000_000 / control.fps.load(Ordering::SeqCst).clamp(1, 1_000) as u64);
        next += interval;
        let now = Instant::now();
        if next < now {
            // Reading took longer than a frame: the next one follows at once, without a backlog.
            next = now;
        }
        loop {
            let left = next.saturating_duration_since(Instant::now());
            if left.is_zero() || control.stop.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(left.min(STOP_POLL));
        }
    }
}

/// The running video thread.
struct VideoWorker {
    control: Arc<Control>,
    thread: Option<JoinHandle<()>>,
}

impl VideoWorker {
    fn stop(&mut self) {
        self.control.stop.store(true, Ordering::SeqCst);
        self.control.wake.poke();
        if let Some(thread) = self.thread.take() {
            // A panic on the thread has nothing to report here.
            let _ = thread.join();
        }
    }
}

struct WindowsStream {
    config: StreamConfig,
    source: Source,
    output: PhysicalSize,
    sink: Arc<dyn StreamSink>,
    ending: Arc<Ending>,
    video: Option<VideoWorker>,
    system_audio: Option<audio::Worker>,
    microphone: Option<audio::Worker>,
    running: bool,
}

/// Opens a stream for `config`, stopped; `start` begins delivering to `sink`.
pub(super) fn open(config: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
    if config.fps == 0 {
        return Err(CaptureError::InvalidArgument("the frame rate is 0".into()));
    }
    let source = resolve(&config.target)?;
    if let Some(microphone) = &config.microphone {
        audio::probe(&Endpoint::Microphone(microphone.device_id.clone()))?;
    }
    let output = convert::stream_size(source.native, config.output_size, config.size_divisor);
    let sink: Arc<dyn StreamSink> = Arc::from(sink);
    let ending = Arc::new(Ending { ended: AtomicBool::new(false), sink: sink.clone() });
    Ok(Box::new(WindowsStream {
        config,
        source,
        output,
        sink,
        ending,
        video: None,
        system_audio: None,
        microphone: None,
        running: false,
    }))
}

impl WindowsStream {
    /// Stops every thread of the stream and waits for them.
    fn halt(&mut self) {
        if let Some(mut video) = self.video.take() {
            video.stop();
        }
        // Dropping an audio worker stops its thread.
        self.system_audio = None;
        self.microphone = None;
        self.running = false;
    }

    fn start_audio(&mut self) -> Result<()> {
        if self.config.capture_system_audio && self.system_audio.is_none() {
            self.system_audio = Some(audio::spawn(Endpoint::Loopback, self.sink.clone())?);
        }
        if let (Some(microphone), None) = (&self.config.microphone, &self.microphone) {
            let endpoint = Endpoint::Microphone(microphone.device_id.clone());
            self.microphone = Some(audio::spawn(endpoint, self.sink.clone())?);
        }
        Ok(())
    }
}

impl CaptureStream for WindowsStream {
    fn start(&mut self) -> Result<()> {
        if self.running {
            return Ok(());
        }
        let control = Arc::new(Control {
            stop: AtomicBool::new(false),
            cursor: AtomicBool::new(self.config.shows_cursor),
            fps: AtomicU32::new(self.config.fps),
            wake: Arc::new(Wake::default()),
        });
        let job = VideoJob {
            source: self.source,
            output: self.output,
            control: control.clone(),
            ending: self.ending.clone(),
        };
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<()>>(1);
        let thread = std::thread::Builder::new()
            .name("glimpsfy-stream-video".into())
            .spawn(move || video_thread(job, ready_tx))
            .map_err(|error| CaptureError::Backend(format!("could not start the video thread: {error}")))?;
        let mut video = VideoWorker { control, thread: Some(thread) };
        match ready_rx.recv_timeout(START_TIMEOUT) {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                video.stop();
                return Err(error);
            }
            Err(_) => {
                // The thread is stuck inside the system. It is told to stop and left behind
                // rather than joined, which would hang the caller with it.
                video.control.stop.store(true, Ordering::SeqCst);
                video.control.wake.poke();
                return Err(CaptureError::Timeout(START_TIMEOUT.as_millis() as u64));
            }
        }
        self.video = Some(video);
        self.running = true;
        if let Err(error) = self.start_audio() {
            self.halt();
            return Err(error);
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        self.halt();
        self.ending.finish();
        Ok(())
    }

    /// Applies what can change while the stream runs: the pointer, the frame rate, the system's
    /// audio and the microphone. The frame size stays as it was opened, since an encoder cannot
    /// follow it, and the target cannot change at all.
    fn update(&mut self, config: StreamConfig) -> Result<()> {
        if config.target != self.config.target {
            return Err(CaptureError::InvalidArgument("a stream cannot change what it records".into()));
        }
        if config.fps == 0 {
            return Err(CaptureError::InvalidArgument("the frame rate is 0".into()));
        }
        if let Some(video) = &self.video {
            video.control.cursor.store(config.shows_cursor, Ordering::SeqCst);
            video.control.fps.store(config.fps, Ordering::SeqCst);
            video.control.wake.poke();
        }
        if !config.capture_system_audio {
            self.system_audio = None;
        }
        if config.microphone != self.config.microphone {
            self.microphone = None;
        }
        self.config = config;
        if self.running {
            // On a failure the stream goes on without the source that failed, and the caller
            // hears why; the next update tries it again.
            self.start_audio()?;
        }
        Ok(())
    }
}

impl Drop for WindowsStream {
    fn drop(&mut self) {
        if self.running {
            let _ = self.stop();
        }
    }
}

/// A microphone on its own, for the level meter of the recording setup.
struct MicrophoneStream {
    endpoint: Endpoint,
    sink: Arc<dyn StreamSink>,
    worker: Option<audio::Worker>,
    ended: bool,
}

/// Opens the microphone `source`, stopped. Fails when it is not connected.
pub(crate) fn open_microphone(source: MicrophoneSource, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>> {
    let endpoint = Endpoint::Microphone(source.device_id);
    audio::probe(&endpoint)?;
    Ok(Box::new(MicrophoneStream { endpoint, sink: Arc::from(sink), worker: None, ended: false }))
}

impl CaptureStream for MicrophoneStream {
    fn start(&mut self) -> Result<()> {
        if self.worker.is_none() {
            self.worker = Some(audio::spawn(self.endpoint.clone(), self.sink.clone())?);
        }
        Ok(())
    }

    fn stop(&mut self) -> Result<()> {
        // Dropping the worker stops its thread and waits for it.
        self.worker = None;
        if !self.ended {
            self.ended = true;
            self.sink.stopped(None);
        }
        Ok(())
    }

    fn update(&mut self, _config: StreamConfig) -> Result<()> {
        // A microphone has nothing to change while it runs; muting is the recorder's.
        Ok(())
    }
}

impl Drop for MicrophoneStream {
    fn drop(&mut self) {
        if self.worker.is_some() {
            let _ = self.stop();
        }
    }
}
