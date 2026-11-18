//! One take at a time: the [`Recorder`] drives the state machine, opens the capture stream,
//! puts every buffer on the file's timeline and hands it to the writer, and finishes the take
//! with exactly one [`RecordingOutcome`].
//!
//! The stream, the writer, the clock and the Trash are traits ([`Env`]), so this policy runs the
//! same in the app and in tests without a screen.
//!
//! The stream opens when the countdown starts, so the first frame is there the moment the
//! countdown ends; until then every buffer is dropped and nothing is written. The writer is
//! created by the first frame of the take, which also gives it the frame size.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use glimpsfy_capture::{
    AudioBuffer, AudioSource, CaptureBackend, CaptureError, CaptureStream, ColorSpace, HostTime, PhysicalSize,
    StreamConfig, StreamPixelFormat, StreamSink, VideoFrame, WindowId,
};

use crate::audio::{self, Conformer, Mixer, Placer};
use crate::files::{self, Trash};
use crate::state::{Ending, Machine};
use crate::timeline::Timeline;
use crate::{
    RecordError, RecordingConfig, RecordingId, RecordingOutcome, RecordingResult, RecordingState, Resolution, Result,
    StopReason, TrackInfo, TrackKind, VideoCodec,
};

/// Movie fragments are written this often, so a crash loses at most this much of the take.
pub const FRAGMENT_INTERVAL_MS: u64 = 2_000;
/// How often the free disk space is checked while recording.
const DISK_CHECK_NS: u64 = 2_000_000_000;

/// The host clock that stamps every buffer.
pub trait Clock: Send + Sync {
    fn now(&self) -> HostTime;
}

/// The system's host clock, the one ScreenCaptureKit and Core Audio stamp buffers with.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> HostTime {
        crate::platform::host_now()
    }
}

/// Opens the capture stream of a take.
pub trait StreamOpener: Send + Sync {
    fn open(&self, config: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>, CaptureError>;
}

impl StreamOpener for Arc<dyn CaptureBackend> {
    fn open(&self, config: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>, CaptureError> {
        self.open_stream(config, sink)
    }
}

/// What a writer is asked to write.
#[derive(Clone, Debug, PartialEq)]
pub struct WriterSpec {
    pub path: PathBuf,
    pub size: PhysicalSize,
    pub fps: u32,
    pub codec: VideoCodec,
    pub color_space: ColorSpace,
    /// The audio tracks in file order. With more than one, the first is the enabled default
    /// (the mix) and the others are alternates a player leaves off.
    pub audio: Vec<TrackKind>,
    pub fragment_interval_ms: u64,
}

impl WriterSpec {
    /// The tracks as the file will list them.
    pub fn tracks(&self) -> Vec<TrackInfo> {
        let video = match self.codec {
            VideoCodec::H264 => "avc1",
            VideoCodec::Hevc => "hvc1",
        };
        let mut tracks = vec![TrackInfo { kind: TrackKind::Video, codec: video.into() }];
        tracks.extend(self.audio.iter().map(|&kind| TrackInfo { kind, codec: "aac".into() }));
        tracks
    }
}

/// The codec a take of `size` is written with: H.264 stops at 4096×2304 worth of macroblocks
/// (level 5.2), so a larger picture (a 5K display) is written as HEVC instead of failing.
pub fn codec_for(wanted: VideoCodec, size: PhysicalSize) -> VideoCodec {
    let blocks = size.width.div_ceil(16) as u64 * size.height.div_ceil(16) as u64;
    if wanted == VideoCodec::H264 && (blocks > 36_864 || size.width > 4096 || size.height > 4096) {
        VideoCodec::Hevc
    } else {
        wanted
    }
}

/// Writes one take. Calls come one at a time (the session holds a lock).
pub trait TakeWriter: Send {
    /// Appends a frame at `media_ns`. Returns `false` when the encoder was busy and the frame
    /// was dropped, which a real-time recording accepts.
    fn append_video(&mut self, frame: &VideoFrame, media_ns: u64) -> Result<bool>;
    /// Appends interleaved 48 kHz stereo `samples` to audio track `track` (an index into
    /// [`WriterSpec::audio`]) at frame position `position`.
    fn append_audio(&mut self, track: usize, position: u64, samples: &[f32]) -> Result<()>;
    /// The writer's own failure (the encoder stopped, the disk is full), if it failed.
    fn failure(&self) -> Option<String>;
    /// Finishes the file at `end_ns`. On failure, what was written so far stays on disk.
    fn finish(&mut self, end_ns: u64) -> Result<()>;
}

pub trait WriterFactory: Send + Sync {
    fn create(&self, spec: &WriterSpec) -> Result<Box<dyn TakeWriter>>;
}

/// The system's writer (`AVAssetWriter` on macOS).
pub struct SystemWriters;

impl WriterFactory for SystemWriters {
    fn create(&self, spec: &WriterSpec) -> Result<Box<dyn TakeWriter>> {
        crate::platform::create_writer(spec)
    }
}

/// Everything the recorder needs from the outside.
#[derive(Clone)]
pub struct Env {
    pub streams: Arc<dyn StreamOpener>,
    pub writers: Arc<dyn WriterFactory>,
    pub clock: Arc<dyn Clock>,
    pub trash: Arc<dyn Trash>,
    /// Called from a capture thread when the take failed on its own (the stream stopped, the
    /// encoder failed, the disk ran low), and again from [`Recorder::begin`] when the stream
    /// stopped while it opened. The app answers with [`Recorder::finish_fault`] on a thread of
    /// its own, which ends a take once however often it is called.
    pub on_fault: Arc<dyn Fn(&RecordingId) + Send + Sync>,
}

/// The app's own windows for the stream's filter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnWindows {
    /// Windows the recording must not show: the bars, the countdown, the bubble's controls.
    pub hidden: Vec<WindowId>,
    /// Windows the recording shows: the webcam bubble, the click ring, the keystroke overlay.
    pub shown: Vec<WindowId>,
}

/// What a running take reports a few times a second.
#[derive(Clone, Debug, PartialEq)]
pub struct TakeProgress {
    pub id: RecordingId,
    pub elapsed_ms: u64,
    pub microphone_level: f32,
    pub system_level: f32,
    pub file_bytes: u64,
}

/// The audio track layout of a configuration: the source of each placed source index, and the
/// tracks in file order.
pub fn audio_layout(config: &RecordingConfig) -> (Vec<AudioSource>, Vec<TrackKind>) {
    let mut sources = Vec::new();
    if config.audio.system {
        sources.push(AudioSource::System);
    }
    if config.audio.microphone.is_some() {
        sources.push(AudioSource::Microphone);
    }
    let tracks = match sources.as_slice() {
        [] => vec![],
        [AudioSource::System] => vec![TrackKind::SystemAudio],
        [AudioSource::Microphone] => vec![TrackKind::Microphone],
        _ if config.audio.separate_tracks => {
            vec![TrackKind::MixedAudio, TrackKind::Microphone, TrackKind::SystemAudio]
        }
        _ => vec![TrackKind::MixedAudio],
    };
    (sources, tracks)
}

/// The stream configuration of a take.
pub fn stream_config(config: &RecordingConfig, own: &OwnWindows) -> StreamConfig {
    StreamConfig {
        target: config.target.clone(),
        fps: config.fps,
        output_size: None,
        size_divisor: match config.resolution {
            Resolution::Native => 1,
            Resolution::Half => 2,
        },
        shows_cursor: config.shows_cursor,
        capture_system_audio: config.audio.system,
        microphone: config.audio.microphone.clone(),
        excluded_windows: own.hidden.clone(),
        shown_windows: own.shown.clone(),
        color_space: ColorSpace::Srgb,
        pixel_format: StreamPixelFormat::Nv12,
        queue_depth: 6,
    }
}

struct Take {
    id: RecordingId,
    path: PathBuf,
    config: RecordingConfig,
    created_at_ms: i64,
    stream: Box<dyn CaptureStream>,
    shared: Arc<Shared>,
}

/// The part of a take the capture threads share.
struct Shared {
    id: RecordingId,
    path: PathBuf,
    codec: VideoCodec,
    fps: u32,
    sources: Vec<AudioSource>,
    tracks: Vec<TrackKind>,
    writers: Arc<dyn WriterFactory>,
    on_fault: Arc<dyn Fn(&RecordingId) + Send + Sync>,
    /// Host time from which buffers belong to the take; 0 while the countdown runs.
    armed_at: AtomicU64,
    muted: AtomicBool,
    microphone_level: AtomicU32,
    system_level: AtomicU32,
    fault: Mutex<Option<Fault>>,
    /// The latest frame of the countdown. A still screen sends no new frame, so this one opens
    /// the take the moment it starts.
    preroll: Mutex<Option<VideoFrame>>,
    inner: Mutex<Inner>,
}

#[derive(Clone, Debug)]
struct Fault {
    code: &'static str,
    message: String,
    disk_full: bool,
}

struct Inner {
    timeline: Timeline,
    writer: Option<Box<dyn TakeWriter>>,
    size: Option<PhysicalSize>,
    conformers: Vec<Conformer>,
    placers: Vec<Placer>,
    mixer: Option<Mixer>,
    frames: u64,
    last_disk_check: u64,
    closed: bool,
    /// The codec the writer really uses (see [`codec_for`]).
    codec: VideoCodec,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic on a capture thread must not take the take down with it: the data stays usable.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Shared {
    fn set_fault(&self, code: &'static str, message: String, disk_full: bool) {
        let mut fault = lock(&self.fault);
        if fault.is_none() {
            *fault = Some(Fault { code, message, disk_full });
            drop(fault);
            (self.on_fault)(&self.id);
        }
    }

    fn armed(&self, at: HostTime) -> bool {
        let armed = self.armed_at.load(Ordering::SeqCst);
        armed != 0 && at.0 >= armed
    }

    fn track_of(&self, kind: TrackKind) -> Option<usize> {
        self.tracks.iter().position(|&k| k == kind)
    }

    fn append_audio(&self, inner: &mut Inner, track: usize, position: u64, samples: &[f32]) {
        let Some(writer) = inner.writer.as_mut() else { return };
        if let Err(e) = writer.append_audio(track, position, samples) {
            self.set_fault("internal", e.to_string(), false);
        }
    }
}

struct Sink(Arc<Shared>);

impl StreamSink for Sink {
    fn video(&self, frame: VideoFrame) {
        let shared = &self.0;
        if !shared.armed(frame.timestamp) {
            if shared.armed_at.load(Ordering::SeqCst) == 0 {
                *lock(&shared.preroll) = Some(frame);
            }
            return;
        }
        let mut inner = lock(&shared.inner);
        if inner.closed {
            return;
        }
        let Some(media) = inner.timeline.map_video(frame.timestamp) else { return };
        if inner.writer.is_none() {
            let spec = WriterSpec {
                path: shared.path.clone(),
                size: frame.size,
                fps: shared.fps,
                codec: codec_for(shared.codec, frame.size),
                color_space: frame.color_space,
                audio: shared.tracks.clone(),
                fragment_interval_ms: FRAGMENT_INTERVAL_MS,
            };
            match shared.writers.create(&spec) {
                Ok(writer) => {
                    inner.writer = Some(writer);
                    inner.size = Some(frame.size);
                    inner.codec = spec.codec;
                }
                Err(e) => {
                    inner.closed = true;
                    drop(inner);
                    shared.set_fault("internal", e.to_string(), false);
                    return;
                }
            }
        }
        let Some(writer) = inner.writer.as_mut() else { return };
        match writer.append_video(&frame, media) {
            Ok(true) => inner.frames += 1,
            Ok(false) => {}
            Err(e) => {
                inner.closed = true;
                drop(inner);
                shared.set_fault("internal", e.to_string(), false);
                return;
            }
        }
        if inner.frames % 30 == 0 {
            if let Some(reason) = inner.writer.as_ref().and_then(|w| w.failure()) {
                inner.closed = true;
                drop(inner);
                shared.set_fault("internal", reason, false);
                return;
            }
        }
        if media.saturating_sub(inner.last_disk_check) >= DISK_CHECK_NS {
            inner.last_disk_check = media;
            let folder = shared.path.parent().unwrap_or(Path::new("/"));
            if let Ok(free) = files::free_bytes(folder) {
                if free < files::LOW_DISK_BYTES {
                    inner.closed = true;
                    drop(inner);
                    shared.set_fault("io", format!("only {free} bytes are free"), true);
                }
            }
        }
    }

    fn audio(&self, buffer: AudioBuffer) {
        let shared = &self.0;
        let Some(source) = shared.sources.iter().position(|&s| s == buffer.source) else { return };
        let muted = buffer.source == AudioSource::Microphone && shared.muted.load(Ordering::SeqCst);
        let level = if muted { 0.0 } else { audio::level(&buffer.samples) };
        let meter = match buffer.source {
            AudioSource::Microphone => &shared.microphone_level,
            AudioSource::System => &shared.system_level,
        };
        meter.store(level.to_bits(), Ordering::Relaxed);
        if !shared.armed(buffer.timestamp) {
            return;
        }
        let mut inner = lock(&shared.inner);
        if inner.closed || inner.writer.is_none() {
            return;
        }
        let Some(media) = inner.timeline.map(buffer.timestamp) else { return };
        let mut samples = inner.conformers[source].conform(&buffer.samples, buffer.channels, buffer.sample_rate);
        if muted {
            // Muting keeps the timeline: the microphone's track goes on, silent.
            samples.iter_mut().for_each(|s| *s = 0.0);
        }
        let frames = (samples.len() / audio::CHANNELS) as u64;
        if frames == 0 {
            return;
        }
        let place = inner.placers[source].place(audio::frames_at(media), frames);
        let skip = place.skip as usize * audio::CHANNELS;
        let body = &samples[skip.min(samples.len())..];
        let own_kind = match buffer.source {
            AudioSource::System => TrackKind::SystemAudio,
            AudioSource::Microphone => TrackKind::Microphone,
        };
        if let Some(track) = shared.track_of(own_kind) {
            let mut out = vec![0.0; place.silence as usize * audio::CHANNELS];
            out.extend_from_slice(body);
            shared.append_audio(&mut inner, track, place.position, &out);
        }
        if let (Some(track), Some(_)) = (shared.track_of(TrackKind::MixedAudio), inner.mixer.as_ref()) {
            let at = place.position + place.silence;
            let mut ready = Vec::new();
            if let Some(mixer) = inner.mixer.as_mut() {
                mixer.add(source, at, body);
                while let Some(chunk) = mixer.take(false) {
                    ready.push(chunk);
                }
            }
            for (position, mixed) in ready {
                shared.append_audio(&mut inner, track, position, &mixed);
            }
        }
    }

    fn stopped(&self, error: Option<CaptureError>) {
        if let Some(error) = error {
            let code = match error {
                CaptureError::PermissionDenied => "permission_denied",
                CaptureError::DisplayNotFound(_) | CaptureError::WindowNotFound(_) => "not_found",
                _ => "capture",
            };
            self.0.set_fault(code, error.to_string(), false);
        }
    }
}

impl Take {
    /// Closes the stream and finishes the file. Returns the finished file's description, or the
    /// error together with whatever was written.
    fn close(mut self, now: HostTime) -> (Option<RecordingResult>, Option<RecordError>, Option<Fault>) {
        if let Err(e) = self.stream.stop() {
            // The stream may already have stopped on its own; the file is what matters.
            let _ = e;
        }
        let shared = self.shared.clone();
        let mut inner = lock(&shared.inner);
        inner.closed = true;
        let mut error = None;
        if let (Some(track), Some(mut mixer)) = (shared.track_of(TrackKind::MixedAudio), inner.mixer.take()) {
            while let Some((position, mixed)) = mixer.take(true) {
                shared.append_audio(&mut inner, track, position, &mixed);
            }
        }
        let fps = shared.fps.max(1) as u64;
        // The take lasts until the stop, not until the last frame: a still screen sends no
        // frames, and the last one stays on screen until the end.
        let video_end = inner
            .timeline
            .last_video()
            .map(|t| (t + 1_000_000_000 / fps).max(inner.timeline.elapsed(now)))
            .unwrap_or(0);
        let Some(mut writer) = inner.writer.take() else {
            return (None, None, lock(&shared.fault).clone());
        };
        if let Err(e) = writer.finish(video_end) {
            error = Some(e);
        }
        drop(writer);
        let size = inner.size.unwrap_or_default();
        let codec = inner.codec;
        drop(inner);
        let file_bytes = std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0);
        let fault = lock(&shared.fault).clone();
        let result = (file_bytes > 0).then(|| RecordingResult {
            id: self.id.clone(),
            path: self.path.clone(),
            duration_ms: video_end / 1_000_000,
            size,
            file_bytes,
            tracks: WriterSpec {
                path: self.path.clone(),
                size,
                fps: self.config.fps,
                codec,
                color_space: ColorSpace::Srgb,
                audio: shared.tracks.clone(),
                fragment_interval_ms: FRAGMENT_INTERVAL_MS,
            }
            .tracks(),
            codec,
            fps: self.config.fps,
            created_at_ms: self.created_at_ms,
            stopped_because: fault.as_ref().filter(|f| f.disk_full).map(|_| StopReason::DiskFull),
        });
        (result, error, fault)
    }
}

struct Inside {
    machine: Machine,
    take: Option<Take>,
    /// The take was running when Discard asked, so keeping it goes on recording.
    resume_on_keep: bool,
}

/// Runs one take at a time. Every method is one user action or one tick; the blocking ones
/// (`begin`, `stop`, `discard`, `finish_fault`) run on a worker thread in the app.
pub struct Recorder {
    env: Env,
    inside: Mutex<Inside>,
}

impl Recorder {
    pub fn new(env: Env) -> Recorder {
        Recorder { env, inside: Mutex::new(Inside { machine: Machine::new(), take: None, resume_on_keep: false }) }
    }

    pub fn state(&self) -> RecordingState {
        lock(&self.inside).machine.state()
    }

    pub fn current(&self) -> Option<RecordingId> {
        lock(&self.inside).machine.id().cloned()
    }

    /// Idle → countdown: checks the configuration and the disk, opens the stream and starts it.
    /// Nothing is written until [`Recorder::go`].
    ///
    /// Opening the stream takes a few hundred milliseconds without the lock, so the countdown can
    /// be cancelled meanwhile; the opened stream then stops at once and this returns
    /// [`RecordError::Cancelled`].
    pub fn begin(&self, id: RecordingId, config: RecordingConfig, path: PathBuf, own: &OwnWindows) -> Result<()> {
        config.validate()?;
        let muted = false;
        {
            let mut inside = lock(&self.inside);
            inside.machine.begin(id.clone(), config.countdown_seconds, muted)?;
            inside.machine.set_has_microphone(config.audio.microphone.is_some());
        }
        let opened = self.open(id.clone(), config, path, own);
        let mut inside = lock(&self.inside);
        // A cancel during the open left idle (or a newer take's countdown) behind: this take is
        // over, and storing it would keep its stream and microphone running with no one to stop
        // them.
        let ours =
            inside.machine.id() == Some(&id) && matches!(inside.machine.state(), RecordingState::Countdown { .. });
        match opened {
            Ok(take) if ours => {
                // Muting during the open reached only the machine.
                take.shared.muted.store(inside.machine.microphone_muted(), Ordering::SeqCst);
                // A stream that stopped on its own during the open found no take to fail; it
                // fails now that there is one.
                let faulted = lock(&take.shared.fault).is_some();
                inside.take = Some(take);
                inside.resume_on_keep = false;
                drop(inside);
                if faulted {
                    (self.env.on_fault)(&id);
                }
                Ok(())
            }
            Ok(take) => {
                drop(inside);
                self.close_unused(take);
                Err(RecordError::Cancelled)
            }
            Err(e) => {
                // Nothing was written: the take ends here, and the error says why.
                if ours {
                    let _ = inside.machine.fault("capture", &e.to_string());
                }
                Err(e)
            }
        }
    }

    /// Closes a take that never reached recording. Nothing is armed before [`Recorder::go`], so
    /// no file should exist; one that does is empty of the take and goes to the Trash rather
    /// than being left behind.
    fn close_unused(&self, take: Take) {
        let (result, _, _) = take.close(self.env.clock.now());
        if let Some(result) = result {
            let _ = self.env.trash.trash(&result.path);
        }
    }

    fn open(&self, id: RecordingId, config: RecordingConfig, path: PathBuf, own: &OwnWindows) -> Result<Take> {
        let folder = path.parent().unwrap_or(Path::new("/"));
        std::fs::create_dir_all(folder)?;
        let free = files::free_bytes(folder)?;
        if free < files::LOW_DISK_BYTES {
            return Err(RecordError::DiskFull { path: folder.to_path_buf(), free_bytes: free });
        }
        if path.symlink_metadata().is_ok() {
            return Err(RecordError::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} already exists", path.display()),
            )));
        }
        let (sources, tracks) = audio_layout(&config);
        let mixer = tracks.contains(&TrackKind::MixedAudio).then(|| Mixer::new(sources.len()));
        let shared = Arc::new(Shared {
            id: id.clone(),
            path: path.clone(),
            codec: config.codec,
            fps: config.fps,
            sources: sources.clone(),
            tracks,
            writers: self.env.writers.clone(),
            on_fault: self.env.on_fault.clone(),
            armed_at: AtomicU64::new(0),
            muted: AtomicBool::new(false),
            microphone_level: AtomicU32::new(0),
            system_level: AtomicU32::new(0),
            fault: Mutex::new(None),
            preroll: Mutex::new(None),
            inner: Mutex::new(Inner {
                timeline: Timeline::new(),
                writer: None,
                size: None,
                conformers: sources.iter().map(|_| Conformer::new()).collect(),
                placers: sources.iter().map(|_| Placer::new()).collect(),
                mixer,
                frames: 0,
                last_disk_check: 0,
                closed: false,
                codec: config.codec,
            }),
        });
        let mut stream = self.env.streams.open(stream_config(&config, own), Box::new(Sink(shared.clone())))?;
        stream.start()?;
        let created_at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or_default();
        Ok(Take { id, path, config, created_at_ms, stream, shared })
    }

    /// One second of the countdown passed; returns the seconds left.
    pub fn countdown_tick(&self) -> Result<u32> {
        lock(&self.inside).machine.countdown_tick()
    }

    /// Countdown → recording: buffers from now on belong to the take. Refused while the stream
    /// still opens, since nothing could be armed and the take would record nothing; the countdown
    /// goes on and its end starts the take.
    pub fn go(&self) -> Result<()> {
        let mut inside = lock(&self.inside);
        if inside.take.is_none() && matches!(inside.machine.state(), RecordingState::Countdown { .. }) {
            return Err(RecordError::InvalidState { action: "start recording", state: "opening the stream" });
        }
        inside.machine.started()?;
        if let Some(take) = &inside.take {
            let now = self.env.clock.now().0.max(1);
            take.shared.armed_at.store(now, Ordering::SeqCst);
            let preroll = lock(&take.shared.preroll).take();
            if let Some(mut frame) = preroll {
                frame.timestamp = HostTime(now);
                Sink(take.shared.clone()).video(frame);
            }
        }
        Ok(())
    }

    /// Countdown → idle; the stream closes and no file is left.
    pub fn cancel(&self) -> Result<RecordingOutcome> {
        let (outcome, take) = {
            let mut inside = lock(&self.inside);
            let outcome = inside.machine.cancel_countdown()?;
            (outcome, inside.take.take())
        };
        // Without a take the stream is still opening, and `begin` stops it once it is open.
        if let Some(take) = take {
            self.close_unused(take);
        }
        Ok(outcome)
    }

    pub fn pause(&self) -> Result<()> {
        let mut inside = lock(&self.inside);
        inside.machine.pause()?;
        self.pause_take(&inside);
        Ok(())
    }

    fn pause_take(&self, inside: &Inside) {
        if let Some(take) = &inside.take {
            let now = self.env.clock.now();
            lock(&take.shared.inner).timeline.pause(now);
        }
    }

    pub fn resume(&self) -> Result<()> {
        let mut inside = lock(&self.inside);
        inside.machine.resume()?;
        if let Some(take) = &inside.take {
            let now = self.env.clock.now();
            lock(&take.shared.inner).timeline.resume(now);
        }
        Ok(())
    }

    /// Recording or paused → paused with the question in the bar; nothing is deleted yet.
    pub fn ask_discard(&self) -> Result<()> {
        let mut inside = lock(&self.inside);
        let paused_now = inside.machine.ask_discard()?;
        if paused_now {
            self.pause_take(&inside);
        }
        // Asked twice, the take stays as the first question found it.
        inside.resume_on_keep |= paused_now;
        Ok(())
    }

    /// The user kept the take ("Keep recording"): a take Discard paused goes on recording, and
    /// one the user had paused before stays paused.
    pub fn keep(&self) -> Result<()> {
        let resume = {
            let mut inside = lock(&self.inside);
            inside.machine.keep()?;
            std::mem::take(&mut inside.resume_on_keep)
        };
        if resume {
            self.resume()?;
        }
        Ok(())
    }

    pub fn set_microphone_muted(&self, muted: bool) -> Result<()> {
        let mut inside = lock(&self.inside);
        inside.machine.set_muted(muted)?;
        if let Some(take) = &inside.take {
            take.shared.muted.store(muted, Ordering::SeqCst);
        }
        Ok(())
    }

    /// The running take's progress, for `record://tick`; `None` unless recording or paused.
    pub fn progress(&self) -> Option<TakeProgress> {
        let mut inside = lock(&self.inside);
        if !inside.machine.is_capturing() {
            return None;
        }
        let take = inside.take.as_ref()?;
        let elapsed_ms = lock(&take.shared.inner).timeline.elapsed(self.env.clock.now()) / 1_000_000;
        let progress = TakeProgress {
            id: take.id.clone(),
            elapsed_ms,
            microphone_level: f32::from_bits(take.shared.microphone_level.load(Ordering::Relaxed)),
            system_level: f32::from_bits(take.shared.system_level.load(Ordering::Relaxed)),
            file_bytes: std::fs::metadata(&take.path).map(|m| m.len()).unwrap_or(0),
        };
        inside.machine.set_elapsed(elapsed_ms);
        Some(progress)
    }

    /// Recording or paused → finishing → saved (or failed, keeping what was written).
    pub fn stop(&self) -> Result<RecordingOutcome> {
        let take = {
            let mut inside = lock(&self.inside);
            inside.machine.stop()?;
            inside.take.take()
        };
        let outcome = self.close_take(take, Ending::Save);
        lock(&self.inside).machine.finish(outcome)
    }

    /// Ends a take that failed on its own, keeping what was written; `None` when no take
    /// failed (it already ended).
    pub fn finish_fault(&self) -> Option<RecordingOutcome> {
        let take = {
            let mut inside = lock(&self.inside);
            let take = inside.take.as_ref()?;
            let fault = lock(&take.shared.fault).clone()?;
            match inside.machine.fault(fault.code, &fault.message) {
                Ok(Some(outcome)) => {
                    let take = inside.take.take();
                    drop(inside);
                    if let Some(take) = take {
                        let _ = take.close(self.env.clock.now());
                    }
                    return Some(outcome);
                }
                Ok(None) if inside.machine.ending() == Some(Ending::Fault) => inside.take.take(),
                // Already finishing for a stop or a discard: that step reports the outcome.
                _ => return None,
            }
        };
        let outcome = self.close_take(take, Ending::Fault);
        lock(&self.inside).machine.finish(outcome).ok()
    }

    /// Finishes the take after the user confirmed the discard in the bar, and moves its file to
    /// the Trash. Without the confirmation, this is refused.
    pub fn discard(&self) -> Result<RecordingOutcome> {
        let take = {
            let mut inside = lock(&self.inside);
            inside.machine.confirm_discard()?;
            inside.take.take()
        };
        let outcome = self.close_take(take, Ending::Discard);
        lock(&self.inside).machine.finish(outcome)
    }

    fn close_take(&self, take: Option<Take>, ending: Ending) -> RecordingOutcome {
        let id = self.current().unwrap_or_else(|| RecordingId(String::new()));
        let Some(take) = take else {
            return match ending {
                Ending::Discard => RecordingOutcome::Discarded { id, trashed: false },
                _ => RecordingOutcome::Failed {
                    id,
                    code: "capture".into(),
                    message: "the take had no stream".into(),
                    partial: None,
                },
            };
        };
        let (result, error, fault) = take.close(self.env.clock.now());
        match ending {
            Ending::Discard => match result {
                None => RecordingOutcome::Discarded { id, trashed: false },
                Some(result) => match self.env.trash.trash(&result.path) {
                    Ok(()) => RecordingOutcome::Discarded { id, trashed: true },
                    // The Trash refused: the file stays where it is, and the user hears why.
                    Err(e) => RecordingOutcome::Failed {
                        id,
                        code: "io".into(),
                        message: format!("the recording could not be moved to the Trash: {e}"),
                        partial: Some(result),
                    },
                },
            },
            Ending::Save | Ending::Fault => {
                let disk_full = fault.as_ref().is_some_and(|f| f.disk_full);
                match (result, error, fault) {
                    (Some(result), None, None) => RecordingOutcome::Saved { result },
                    (Some(result), None, Some(_)) if disk_full => RecordingOutcome::Saved { result },
                    (partial, error, fault) => {
                        let (code, message) = match (error, fault) {
                            (_, Some(f)) => (f.code.to_string(), f.message),
                            (Some(e), None) => ("internal".to_string(), e.to_string()),
                            (None, None) => ("capture".to_string(), "no frame was recorded".to_string()),
                        };
                        RecordingOutcome::Failed { id, code, message, partial }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AudioConfig, ClickHighlight, KeystrokeOverlay, WebcamOverlay};
    use glimpsfy_capture::{DisplayId, NativeBuffer, StreamTarget, VideoBuffer};
    use std::sync::atomic::AtomicUsize;

    struct FakeClock(AtomicU64);
    impl Clock for FakeClock {
        fn now(&self) -> HostTime {
            HostTime(self.0.load(Ordering::SeqCst))
        }
    }

    #[derive(Default)]
    struct FakeStreams {
        sink: Mutex<Option<Box<dyn StreamSink>>>,
        /// When set, the next open waits for a message, as a real stream takes a moment to open.
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
        /// Streams opened and not stopped yet.
        running: Arc<AtomicUsize>,
    }
    struct FakeStream {
        running: Arc<AtomicUsize>,
        started: bool,
    }
    impl CaptureStream for FakeStream {
        fn start(&mut self) -> glimpsfy_capture::Result<()> {
            self.started = true;
            self.running.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn stop(&mut self) -> glimpsfy_capture::Result<()> {
            if std::mem::take(&mut self.started) {
                self.running.fetch_sub(1, Ordering::SeqCst);
            }
            Ok(())
        }
        fn update(&mut self, _: StreamConfig) -> glimpsfy_capture::Result<()> {
            Ok(())
        }
    }
    impl StreamOpener for FakeStreams {
        fn open(&self, _: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>, CaptureError> {
            let gate = lock(&self.gate).take();
            if let Some(gate) = gate {
                let _ = gate.recv();
            }
            *lock(&self.sink) = Some(sink);
            Ok(Box::new(FakeStream { running: self.running.clone(), started: false }))
        }
    }

    /// Writes one byte per frame to the real file, and fails on demand.
    struct FakeWriters {
        fail_finish: AtomicBool,
    }
    struct FakeWriter {
        file: std::fs::File,
        fail_finish: bool,
    }
    impl TakeWriter for FakeWriter {
        fn append_video(&mut self, _: &VideoFrame, _: u64) -> Result<bool> {
            use std::io::Write;
            self.file.write_all(b"f")?;
            Ok(true)
        }
        fn append_audio(&mut self, _: usize, _: u64, _: &[f32]) -> Result<()> {
            Ok(())
        }
        fn failure(&self) -> Option<String> {
            None
        }
        fn finish(&mut self, _: u64) -> Result<()> {
            if self.fail_finish {
                return Err(RecordError::Encoder("the encoder stopped".into()));
            }
            Ok(())
        }
    }
    impl WriterFactory for FakeWriters {
        fn create(&self, spec: &WriterSpec) -> Result<Box<dyn TakeWriter>> {
            let file = std::fs::File::create(&spec.path)?;
            Ok(Box::new(FakeWriter { file, fail_finish: self.fail_finish.load(Ordering::SeqCst) }))
        }
    }

    #[derive(Default)]
    struct FakeTrash {
        trashed: Mutex<Vec<PathBuf>>,
        bin: Mutex<Option<PathBuf>>,
    }
    impl Trash for FakeTrash {
        fn trash(&self, path: &Path) -> std::io::Result<()> {
            // Like the real Trash, the file moves somewhere the user can still find it.
            let bin = lock(&self.bin).clone().ok_or_else(|| std::io::Error::other("no bin"))?;
            std::fs::rename(path, bin.join(path.file_name().unwrap_or_default()))?;
            lock(&self.trashed).push(path.to_path_buf());
            Ok(())
        }
    }

    struct Rig {
        recorder: Recorder,
        streams: Arc<FakeStreams>,
        clock: Arc<FakeClock>,
        trash: Arc<FakeTrash>,
        writers: Arc<FakeWriters>,
        faults: Arc<AtomicUsize>,
        dir: tempfile::TempDir,
    }

    fn rig() -> Rig {
        let streams = Arc::new(FakeStreams::default());
        let clock = Arc::new(FakeClock(AtomicU64::new(1_000_000_000)));
        let trash = Arc::new(FakeTrash::default());
        let writers = Arc::new(FakeWriters { fail_finish: AtomicBool::new(false) });
        let faults = Arc::new(AtomicUsize::new(0));
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("Trash");
        std::fs::create_dir(&bin).unwrap();
        *lock(&trash.bin) = Some(bin);
        let counter = faults.clone();
        let env = Env {
            streams: streams.clone(),
            writers: writers.clone(),
            clock: clock.clone(),
            trash: trash.clone(),
            on_fault: Arc::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            }),
        };
        Rig { recorder: Recorder::new(env), streams, clock, trash, writers, faults, dir }
    }

    fn config(dir: &Path) -> RecordingConfig {
        RecordingConfig {
            target: StreamTarget::Display { display: DisplayId(1) },
            fps: 30,
            resolution: Resolution::Native,
            codec: VideoCodec::H264,
            shows_cursor: true,
            audio: AudioConfig::default(),
            clicks: ClickHighlight::default(),
            keystrokes: KeystrokeOverlay::default(),
            webcam: WebcamOverlay::default(),
            countdown_seconds: 3,
            output_folder: dir.to_path_buf(),
        }
    }

    impl Rig {
        fn start(&self, name: &str) -> PathBuf {
            let path = self.dir.path().join(name);
            let id = RecordingId(name.replace('.', "-"));
            self.recorder.begin(id, config(self.dir.path()), path.clone(), &OwnWindows::default()).unwrap();
            self.frames(5);
            assert!(!path.exists(), "nothing is written during the countdown");
            self.recorder.go().unwrap();
            self.frames(30);
            path
        }

        fn frames(&self, n: usize) {
            let sink = lock(&self.streams.sink);
            let sink = sink.as_ref().unwrap();
            for _ in 0..n {
                let t = self.clock.0.fetch_add(33_000_000, Ordering::SeqCst) + 33_000_000;
                sink.video(VideoFrame {
                    timestamp: HostTime(t),
                    size: PhysicalSize { width: 4, height: 4 },
                    format: StreamPixelFormat::Bgra,
                    color_space: ColorSpace::Srgb,
                    buffer: VideoBuffer::Native(NativeBuffer(Box::new(()))),
                });
            }
        }

        fn files(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(self.dir.path())
                .unwrap()
                .chain(std::fs::read_dir(self.dir.path().join("Trash")).unwrap())
                .filter_map(|e| e.ok())
                .filter(|e| e.path().is_file())
                .map(|e| e.path().strip_prefix(self.dir.path()).unwrap().display().to_string())
                .collect();
            names.sort();
            names
        }
    }

    /// Stop, discard and failure each leave one outcome and one file: saved in place, moved to
    /// the Trash, or kept as a partial take.
    #[test]
    fn never_loses_a_take() {
        let rig = rig();
        let saved = rig.start("saved.mp4");
        match rig.recorder.stop().unwrap() {
            RecordingOutcome::Saved { result } => {
                assert_eq!(result.path, saved);
                // The countdown's last frame opens the take, then come its 30 frames.
                assert_eq!(result.file_bytes, 31);
            }
            other => panic!("expected a saved take, got {other:?}"),
        }

        let discarded = rig.start("discarded.mp4");
        assert!(rig.recorder.discard().is_err(), "a discard without the question is refused");
        rig.recorder.ask_discard().unwrap();
        assert!(discarded.exists(), "asking deletes nothing");
        assert!(rig.trash.trashed.lock().unwrap().is_empty());
        assert!(matches!(rig.recorder.discard().unwrap(), RecordingOutcome::Discarded { trashed: true, .. }));
        assert_eq!(*rig.trash.trashed.lock().unwrap(), vec![discarded.clone()]);

        rig.writers.fail_finish.store(true, Ordering::SeqCst);
        let failed = rig.start("failed.mp4");
        match rig.recorder.stop().unwrap() {
            RecordingOutcome::Failed { partial: Some(result), code, .. } => {
                assert_eq!(code, "internal");
                assert_eq!(result.path, failed);
            }
            other => panic!("expected a kept partial take, got {other:?}"),
        }
        rig.writers.fail_finish.store(false, Ordering::SeqCst);

        let faulted = rig.start("faulted.mp4");
        lock(&rig.streams.sink).as_ref().unwrap().stopped(Some(CaptureError::Backend("the display went away".into())));
        assert_eq!(rig.faults.load(Ordering::SeqCst), 1);
        match rig.recorder.finish_fault() {
            Some(RecordingOutcome::Failed { partial: Some(result), code, .. }) => {
                assert_eq!(code, "capture");
                assert_eq!(result.path, faulted);
            }
            other => panic!("expected a kept partial take, got {other:?}"),
        }
        assert!(rig.recorder.finish_fault().is_none(), "a take ends once");

        assert_eq!(rig.files(), vec!["Trash/discarded.mp4", "failed.mp4", "faulted.mp4", "saved.mp4"]);
        assert!(rig.recorder.state() == RecordingState::Idle);
    }

    /// A cancel or an early start while the stream still opens: the cancelled take's stream
    /// stops, so the screen and the microphone are not left recording, and the early start is
    /// refused rather than arming nothing, so the take still records when the countdown ends.
    #[test]
    fn a_stream_still_opening_is_never_lost() {
        let rig = rig();
        let open_slowly = || {
            let (release, gate) = std::sync::mpsc::channel();
            *lock(&rig.streams.gate) = Some(gate);
            release
        };
        let begin = |name: &str| {
            let path = rig.dir.path().join(name);
            rig.recorder.begin(RecordingId(name.into()), config(rig.dir.path()), path, &OwnWindows::default())
        };
        let counting_down = || {
            while !matches!(rig.recorder.state(), RecordingState::Countdown { .. }) {
                std::thread::yield_now();
            }
        };

        // Dropping `release` opens the gate; the checks wait until after it, so a failing one
        // cannot leave the opening thread waiting forever.
        let release = open_slowly();
        let (cancelled, begun) = std::thread::scope(|s| {
            let begun = s.spawn(|| begin("cancelled.mp4"));
            counting_down();
            let cancelled = rig.recorder.cancel();
            drop(release);
            (cancelled, begun.join().unwrap())
        });
        assert!(matches!(cancelled.unwrap(), RecordingOutcome::Cancelled { .. }));
        assert!(matches!(begun, Err(RecordError::Cancelled)));
        assert_eq!(rig.streams.running.load(Ordering::SeqCst), 0, "the cancelled take's stream stopped");
        assert!(rig.recorder.state() == RecordingState::Idle);

        let release = open_slowly();
        let (early, begun) = std::thread::scope(|s| {
            let begun = s.spawn(|| begin("early.mp4"));
            counting_down();
            let early = rig.recorder.go();
            drop(release);
            (early, begun.join().unwrap())
        });
        assert!(early.is_err(), "nothing can be armed before the stream is open");
        begun.unwrap();
        assert!(matches!(rig.recorder.state(), RecordingState::Countdown { .. }), "the countdown goes on");
        rig.recorder.go().unwrap();
        rig.frames(30);
        match rig.recorder.stop().unwrap() {
            RecordingOutcome::Saved { result } => assert_eq!(result.file_bytes, 30),
            other => panic!("expected a saved take, got {other:?}"),
        }
        assert_eq!(rig.streams.running.load(Ordering::SeqCst), 0);
        assert_eq!(rig.files(), vec!["early.mp4"]);
    }
}
