//! GStreamer's part of a recording: the sound server's sources (system audio and microphones,
//! through `pulsesrc`, which PulseAudio and PipeWire's pulse server both answer) and the PipeWire
//! node of a portal screen cast (`pipewiresrc`), each as a small pipeline that ends in an
//! `appsink` on GStreamer's own streaming thread.
//!
//! Every pipeline runs on the monotonic system clock, so a buffer's time on the host clock is
//! the pipeline's base time plus the buffer's running time.

use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::VideoFrameExt as _;

use super::host_now;
use crate::error::{CaptureError, Result};
use crate::geometry::PhysicalSize;
use crate::stream::{AudioBuffer, AudioSource, HostTime, StreamSink};

/// The name PulseAudio gives the monitor of the default output: what the speakers play.
const DEFAULT_MONITOR: &str = "@DEFAULT_MONITOR@";
/// A buffer's own time is trusted when it lies this close before now; anything else (a source
/// with another clock's stamps, or none) is stamped on arrival instead.
const TRUSTED_AGE_NS: u64 = 2_000_000_000;
const TRUSTED_LEAD_NS: u64 = 50_000_000;

/// Called with the reason a pipeline stopped on its own.
pub(super) type OnError = Arc<dyn Fn(CaptureError) + Send + Sync>;

/// Starts GStreamer once for the process.
pub(super) fn init() -> Result<()> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| CaptureError::Backend(format!("GStreamer could not start: {e}")))
}

/// One element, or an error that names the package the element comes in.
fn element(factory: &str, package: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory).build().map_err(|_| {
        CaptureError::Backend(format!("GStreamer's {factory} element is missing (the package {package} provides it)"))
    })
}

fn pipeline_error(what: &str, error: impl std::fmt::Display) -> CaptureError {
    CaptureError::Backend(format!("{what}: {error}"))
}

/// Hands a pipeline's errors, and the end of its stream, to `on_error` from the thread that
/// posts them, so no main loop has to run.
fn watch(pipeline: &gst::Pipeline, what: &'static str, on_error: OnError) {
    let Some(bus) = pipeline.bus() else { return };
    bus.set_sync_handler(move |_, message| {
        match message.view() {
            gst::MessageView::Error(error) => on_error(pipeline_error(what, error.error())),
            gst::MessageView::Eos(_) => on_error(CaptureError::Backend(format!("{what}: the source ended"))),
            _ => {}
        }
        gst::BusSyncReply::Drop
    });
}

/// The host time of a sample: the pipeline's base time plus the buffer's running time when that
/// is a believable moment, the arrival time otherwise.
fn stamp(pipeline: &gst::Pipeline, sample: &gst::Sample, buffer: &gst::BufferRef) -> HostTime {
    let now = host_now();
    let running = buffer.pts().and_then(|pts| {
        let segment = sample.segment()?.downcast_ref::<gst::ClockTime>()?;
        segment.to_running_time(pts)
    });
    let absolute = match (pipeline.base_time(), running) {
        (Some(base), Some(running)) => base.nseconds().checked_add(running.nseconds()),
        _ => None,
    };
    match absolute {
        Some(at) if at <= now.0 + TRUSTED_LEAD_NS && at + TRUSTED_AGE_NS >= now.0 => HostTime(at),
        _ => now,
    }
}

/// Brings a pipeline to `state` and waits until it is there (a live source answers at once).
fn set_state(pipeline: &gst::Pipeline, state: gst::State, what: &str) -> Result<()> {
    pipeline.set_state(state).map_err(|_| pipeline_error(what, "the pipeline refused to change its state"))?;
    let (result, _, _) = pipeline.state(gst::ClockTime::from_seconds(5));
    result.map(|_| ()).map_err(|_| pipeline_error(what, "the pipeline did not reach its state"))
}

/// One source of the sound server, delivered as interleaved 32-bit float at 48 kHz.
pub(super) struct AudioCapture {
    pipeline: gst::Pipeline,
    what: &'static str,
}

impl AudioCapture {
    /// Opens the system audio (the monitor of the default output).
    pub fn system(sink: Arc<dyn StreamSink>, on_error: OnError) -> Result<AudioCapture> {
        AudioCapture::open(Some(DEFAULT_MONITOR), AudioSource::System, "the system audio", sink, on_error)
    }

    /// Opens a microphone by its PulseAudio source name; `None` is the default source.
    pub fn microphone(device: Option<&str>, sink: Arc<dyn StreamSink>, on_error: OnError) -> Result<AudioCapture> {
        if let Some(name) = device {
            // A source that is gone would only fail once the pipeline plays, so it is looked up
            // first, while a failure can still be an error of the call.
            let known = sources()?;
            if !known.iter().any(|source| source.id == name) {
                return Err(CaptureError::InvalidArgument(format!("the microphone {name} is not connected")));
            }
        }
        AudioCapture::open(device, AudioSource::Microphone, "the microphone", sink, on_error)
    }

    fn open(
        device: Option<&str>,
        source: AudioSource,
        what: &'static str,
        sink: Arc<dyn StreamSink>,
        on_error: OnError,
    ) -> Result<AudioCapture> {
        init()?;
        let src = element("pulsesrc", "gstreamer1.0-pulseaudio")?;
        if let Some(device) = device {
            src.set_property("device", device);
        }
        // The pipeline keeps the system clock, which every stream of a recording shares.
        src.set_property("provide-clock", false);
        src.set_property("client-name", "Glimpsfy");
        let convert = element("audioconvert", "gstreamer1.0-plugins-base")?;
        let format = if cfg!(target_endian = "big") { "F32BE" } else { "F32LE" };
        let caps = gst::Caps::builder("audio/x-raw")
            .field("format", format)
            .field("layout", "interleaved")
            .field("rate", 48_000i32)
            .build();
        let appsink = gst_app::AppSink::builder().caps(&caps).sync(false).max_buffers(64).build();

        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([&src, &convert, appsink.upcast_ref()])
            .and_then(|_| gst::Element::link_many([&src, &convert, appsink.upcast_ref()]))
            .map_err(|e| pipeline_error(what, e))?;
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));

        let weak = pipeline.downgrade();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let (Some(buffer), Some(caps), Some(pipeline)) = (sample.buffer(), sample.caps(), weak.upgrade())
                    else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let structure = caps.structure(0);
                    let rate = structure.and_then(|s| s.get::<i32>("rate").ok()).unwrap_or(48_000);
                    let channels = structure.and_then(|s| s.get::<i32>("channels").ok()).unwrap_or(2);
                    let Ok(map) = buffer.map_readable() else { return Ok(gst::FlowSuccess::Ok) };
                    let samples: Vec<f32> =
                        map.as_slice().chunks_exact(4).map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]])).collect();
                    sink.audio(AudioBuffer {
                        timestamp: stamp(&pipeline, &sample, buffer),
                        source,
                        sample_rate: rate.max(1) as u32,
                        channels: channels.clamp(1, u16::MAX as i32) as u16,
                        samples,
                    });
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        watch(&pipeline, what, on_error);
        // READY opens the connection to the sound server, so a machine without one fails here
        // and not in the middle of a recording.
        if let Err(error) = set_state(&pipeline, gst::State::Ready, what) {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(match error {
                CaptureError::Backend(words) => {
                    CaptureError::Backend(format!("{words} (no PulseAudio or PipeWire sound server answers)"))
                }
                other => other,
            });
        }
        Ok(AudioCapture { pipeline, what })
    }

    pub fn start(&self) -> Result<()> {
        set_state(&self.pipeline, gst::State::Playing, self.what)
    }

    pub fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// A source of the sound server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SoundSource {
    /// The PulseAudio source name, which `pulsesrc` takes as its device.
    pub id: String,
    pub name: String,
    /// Whether it is the monitor of an output, which carries what the speakers play.
    pub is_monitor: bool,
}

/// Every source the sound server lists, monitors included; none when no sound server answers.
/// An error means GStreamer or its PulseAudio plugin is not installed.
pub(super) fn sources() -> Result<Vec<SoundSource>> {
    init()?;
    let provider = gst::DeviceProviderFactory::by_name("pulsedeviceprovider").ok_or_else(|| {
        CaptureError::Backend("GStreamer's PulseAudio plugin is missing (the package gstreamer1.0-pulseaudio)".into())
    })?;
    // An unstarted provider probes: it connects, lists and disconnects before it returns. No
    // sound server gives an empty list.
    let devices = provider.devices();
    let mut sources = Vec::new();
    for device in devices.iter() {
        if !device.has_classes("Audio/Source") {
            continue;
        }
        let id = if device.find_property("internal-name").is_some() {
            device.property::<Option<String>>("internal-name")
        } else {
            device
                .create_element(None)
                .ok()
                .filter(|e| e.find_property("device").is_some())
                .and_then(|e| e.property::<Option<String>>("device"))
        };
        let Some(id) = id.filter(|id| !id.is_empty()) else { continue };
        let class = device.properties().and_then(|p| p.get::<String>("device.class").ok());
        let is_monitor = class.as_deref() == Some("monitor") || id.ends_with(".monitor");
        sources.push(SoundSource { id, name: device.display_name().to_string(), is_monitor });
    }
    Ok(sources)
}

/// What the video side of a portal stream does with each frame: tight BGRA pixels, their size
/// and their host time.
pub(super) type OnFrame = Box<dyn Fn(&[u8], usize, PhysicalSize, HostTime) + Send + Sync>;

/// The frames of a PipeWire node, converted to BGRA.
pub(super) struct NodeCapture {
    pipeline: gst::Pipeline,
    /// Kept open for as long as the pipeline may connect through it.
    _remote: OwnedFd,
    /// Nanoseconds between two delivered frames, which `update` may change.
    period_ns: Arc<AtomicU64>,
}

impl NodeCapture {
    /// Builds the pipeline for `node` of the PipeWire remote `remote`. `on_frame` gets a frame at
    /// most every `1 / fps` seconds; the compositor sends none while the picture stands still.
    pub fn open(remote: OwnedFd, node: u32, fps: u32, on_frame: OnFrame, on_error: OnError) -> Result<NodeCapture> {
        init()?;
        let what = "the screen cast";
        let src = element("pipewiresrc", "gstreamer1.0-pipewire")?;
        src.set_property("fd", remote.as_raw_fd());
        src.set_property("path", node.to_string());
        let convert = element("videoconvert", "gstreamer1.0-plugins-base")?;
        let caps = gst::Caps::builder("video/x-raw").field("format", "BGRA").build();
        let appsink = gst_app::AppSink::builder().caps(&caps).sync(false).max_buffers(2).drop(true).build();

        let pipeline = gst::Pipeline::new();
        pipeline
            .add_many([&src, &convert, appsink.upcast_ref()])
            .and_then(|_| gst::Element::link_many([&src, &convert, appsink.upcast_ref()]))
            .map_err(|e| pipeline_error(what, e))?;
        pipeline.use_clock(Some(&gst::SystemClock::obtain()));

        let period_ns = Arc::new(AtomicU64::new(period_of(fps)));
        let period = period_ns.clone();
        let last = Mutex::new(0u64);
        let failed = AtomicBool::new(false);
        let weak = pipeline.downgrade();
        let frame_error = on_error.clone();
        appsink.set_callbacks(
            gst_app::AppSinkCallbacks::builder()
                .new_sample(move |appsink| {
                    let sample = appsink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                    let (Some(buffer), Some(caps), Some(pipeline)) = (sample.buffer(), sample.caps(), weak.upgrade())
                    else {
                        return Ok(gst::FlowSuccess::Ok);
                    };
                    let at = stamp(&pipeline, &sample, buffer);
                    {
                        // Frames closer together than the frame rate allows are left out, and
                        // time never runs backwards for the writer.
                        let mut last = last.lock().unwrap_or_else(|e| e.into_inner());
                        let step = period.load(Ordering::Relaxed);
                        if *last != 0 && at.0 < *last + step - step / 8 {
                            return Ok(gst::FlowSuccess::Ok);
                        }
                        *last = at.0;
                    }
                    let read = gst_video::VideoInfo::from_caps(caps).ok().and_then(|info| {
                        let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(buffer, &info).ok()?;
                        let stride = usize::try_from(*frame.plane_stride().first()?).ok()?;
                        let size = PhysicalSize::new(info.width(), info.height());
                        let data = frame.plane_data(0).ok()?;
                        (stride >= size.width as usize * 4).then(|| on_frame(data, stride, size, at))
                    });
                    if read.is_none() && !failed.swap(true, Ordering::SeqCst) {
                        frame_error(CaptureError::Backend(
                            "the screen cast sent a frame that could not be read".into(),
                        ));
                    }
                    Ok(gst::FlowSuccess::Ok)
                })
                .build(),
        );
        watch(&pipeline, what, on_error);
        Ok(NodeCapture { pipeline, _remote: remote, period_ns })
    }

    pub fn start(&self) -> Result<()> {
        set_state(&self.pipeline, gst::State::Playing, "the screen cast")
    }

    pub fn set_fps(&self, fps: u32) {
        self.period_ns.store(period_of(fps), Ordering::Relaxed);
    }

    pub fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for NodeCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn period_of(fps: u32) -> u64 {
    1_000_000_000 / fps.clamp(1, 240) as u64
}

/// Silence in place of system audio the sound server cannot give, so a recording that asked for
/// it keeps a sound track of the right length instead of failing. It writes only while `live`
/// is false: the real source sets it when it runs and clears it when it fails.
pub(super) struct Silence {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Silence {
    /// Twenty milliseconds of stereo at 48 kHz per buffer.
    const FRAMES: usize = 960;
    const STEP_NS: u64 = 20_000_000;

    pub fn start(sink: Arc<dyn StreamSink>, live: Arc<AtomicBool>) -> Silence {
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let thread = std::thread::Builder::new()
            .name("glimpsfy-silence".into())
            .spawn(move || {
                let mut next: Option<u64> = None;
                while !stopped.load(Ordering::SeqCst) {
                    if live.load(Ordering::SeqCst) {
                        next = None;
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    let now = host_now().0;
                    let at = next.unwrap_or(now);
                    if at > now {
                        std::thread::sleep(Duration::from_nanos(at - now));
                    }
                    sink.audio(AudioBuffer {
                        timestamp: HostTime(at),
                        source: AudioSource::System,
                        sample_rate: 48_000,
                        channels: 2,
                        samples: vec![0.0; Silence::FRAMES * 2],
                    });
                    // After a stall the silence starts again from now instead of catching up.
                    let following = at + Silence::STEP_NS;
                    next =
                        Some(if following + 10 * Silence::STEP_NS < host_now().0 { host_now().0 } else { following });
                }
            })
            .ok();
        Silence { stop, thread }
    }

    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Silence {
    fn drop(&mut self) {
        self.stop();
    }
}
