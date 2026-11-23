//! GStreamer's part of recording on Linux: starting it, making elements with the package that
//! provides each named when one is missing, choosing the encoders and decoders that really work
//! on this machine, and following a pipeline's bus without a main loop.
//!
//! Encoders are tried in order and the first one that encodes a real frame wins: software first,
//! because a broken VA-API or NVIDIA driver is the most common reason a recording fails on Linux,
//! and `openh264enc` right after `x264enc`, because a build without the GPL x264 has only it.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use gstreamer as gst;
use gstreamer::glib;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;

use crate::session::codec_for;
use crate::{RecordError, Result, VideoCodec};

/// How long an encoder may take to encode one trial frame before it counts as broken.
const TRIAL_TIMEOUT: Duration = Duration::from_secs(10);

/// Starts GStreamer once for the process. The capture crate's stream may have started it
/// already, which `gst::init` allows.
pub(super) fn init() -> Result<()> {
    static INIT: OnceLock<std::result::Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| gst::init().map_err(|e| e.to_string()))
        .clone()
        .map_err(|e| RecordError::Encoder(format!("GStreamer could not start: {e}")))
}

/// The packages that hold an element, on Debian and Ubuntu first and on Fedora after.
fn package(factory: &str) -> &'static str {
    match factory {
        "appsrc" | "appsink" | "videoconvert" | "videoscale" | "audioconvert" => {
            "gstreamer1.0-plugins-base, or gstreamer1-plugins-base on Fedora"
        }
        "mp4mux" | "qtdemux" | "aacparse" | "taginject" => {
            "gstreamer1.0-plugins-good, or gstreamer1-plugins-good on Fedora"
        }
        "x264enc" => "gstreamer1.0-plugins-ugly",
        "openh264enc" | "openh264dec" => "gstreamer1.0-plugins-bad, or gstreamer1-plugin-openh264 on Fedora",
        f if f.starts_with("avenc_") || f.starts_with("avdec_") => {
            "gstreamer1.0-libav, or gstreamer1-plugin-libav on Fedora"
        }
        f if f.starts_with("vaapi") => "gstreamer1.0-vaapi, or gstreamer1-vaapi on Fedora",
        "filesrc" | "filesink" | "fakesink" | "capsfilter" | "queue" => "libgstreamer1.0-0, or gstreamer1 on Fedora",
        _ => "gstreamer1.0-plugins-bad, or gstreamer1-plugins-bad-free on Fedora",
    }
}

pub(super) fn missing(factory: &str) -> RecordError {
    RecordError::Encoder(format!(
        "GStreamer's {factory} element is missing (the package {} provides it)",
        package(factory)
    ))
}

/// Whether `factory` is installed and not turned off. Ranks only steer GStreamer's own choice
/// and the encoders here are picked by name, so a rank of `NONE` set through
/// `GST_PLUGIN_FEATURE_RANK` (the documented way to keep a broken element out) is read as
/// turning the element off. An element whose rank is `NONE` by default stays usable: the VA
/// encoders ship that way.
pub(super) fn installed(factory: &str) -> bool {
    let turned_off = std::env::var("GST_PLUGIN_FEATURE_RANK").is_ok_and(|ranks| {
        ranks.split(',').filter_map(|entry| entry.split_once(':')).any(|(name, rank)| {
            name.trim() == factory && matches!(rank.trim().to_ascii_uppercase().as_str(), "NONE" | "0")
        })
    });
    !turned_off && gst::ElementFactory::find(factory).is_some()
}

/// One element, or an error that names the package it comes in.
pub(super) fn make(factory: &str) -> Result<gst::Element> {
    gst::ElementFactory::make(factory).build().map_err(|_| missing(factory))
}

pub(super) fn capsfilter(caps: &gst::Caps) -> Result<gst::Element> {
    let filter = make("capsfilter")?;
    filter.set_property("caps", caps);
    Ok(filter)
}

/// Sets `name` to `value`, written the way `gst-launch-1.0` takes it, when the object has such a
/// writable property. Elements differ between releases and between encoders, and a property one
/// lacks only keeps its default; a value it cannot take is refused the same way.
pub(super) fn set(object: &impl IsA<glib::Object>, name: &str, value: &str) -> bool {
    let object = object.upcast_ref::<glib::Object>();
    let Some(spec) = object.find_property(name) else { return false };
    let flags = spec.flags();
    if !flags.contains(glib::ParamFlags::WRITABLE) || flags.contains(glib::ParamFlags::CONSTRUCT_ONLY) {
        return false;
    }
    match <glib::Value as GstValueExt>::deserialize(value, spec.value_type()) {
        Ok(value) => {
            object.set_property_from_value(name, &value);
            true
        }
        Err(_) => false,
    }
}

/// The threads a converter may use: the cores, up to eight.
pub(super) fn threads() -> String {
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).clamp(1, 8).to_string()
}

pub(super) fn bgra_caps(width: u32, height: u32, fps: u32) -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .field("format", "BGRA")
        .field("width", width as i32)
        .field("height", height as i32)
        .field("framerate", gst::Fraction::new(fps.max(1) as i32, 1))
        .field("pixel-aspect-ratio", gst::Fraction::new(1, 1))
        .build()
}

/// The session's audio: 48 kHz stereo 32-bit float, interleaved.
pub(super) fn pcm_caps() -> gst::Caps {
    gst::Caps::builder("audio/x-raw")
        .field("format", "F32LE")
        .field("layout", "interleaved")
        .field("rate", crate::audio::RATE as i32)
        .field("channels", crate::audio::CHANNELS as i32)
        .field("channel-mask", gst::Bitmask::new(0x3))
        .build()
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct Seen {
    error: Option<String>,
    eos: bool,
}

/// What a pipeline's bus said, recorded on the thread that posted it, so no main loop has to
/// run and a waiting call wakes at once.
#[derive(Default)]
pub(super) struct Watch {
    seen: Mutex<Seen>,
    changed: Condvar,
}

impl Watch {
    pub fn on(pipeline: &gst::Pipeline) -> Arc<Watch> {
        let watch = Arc::new(Watch::default());
        if let Some(bus) = pipeline.bus() {
            let seen = watch.clone();
            bus.set_sync_handler(move |_, message| {
                match message.view() {
                    gst::MessageView::Error(error) => {
                        let mut state = lock(&seen.seen);
                        if state.error.is_none() {
                            state.error = Some(describe(error));
                        }
                        seen.changed.notify_all();
                    }
                    gst::MessageView::Eos(_) => {
                        lock(&seen.seen).eos = true;
                        seen.changed.notify_all();
                    }
                    _ => {}
                }
                gst::BusSyncReply::Drop
            });
        }
        watch
    }

    /// The first error the pipeline posted, in GStreamer's words.
    pub fn error(&self) -> Option<String> {
        lock(&self.seen).error.clone()
    }

    /// Waits until every sink has seen the end of the stream; an error or the timeout ends the
    /// wait with the reason.
    pub fn wait_eos(&self, timeout: Duration) -> std::result::Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut seen = lock(&self.seen);
        loop {
            if let Some(error) = &seen.error {
                return Err(error.clone());
            }
            if seen.eos {
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(format!("the pipeline did not finish within {} seconds", timeout.as_secs()));
            }
            seen = self.changed.wait_timeout(seen, deadline - now).unwrap_or_else(|e| e.into_inner()).0;
        }
    }
}

/// An error message as GStreamer gave it: the element, the message, and the last line of the
/// details, which holds the system's own reason ("No space left on device").
fn describe(error: &gst::message::Error) -> String {
    let mut text = error.error().message().to_string();
    if let Some(debug) = error.debug() {
        let last = debug.lines().map(str::trim).rfind(|l| !l.is_empty()).unwrap_or_default();
        if !last.is_empty() && !last.contains(".c(") && !last.starts_with("/Gst") && !text.contains(last) {
            text = format!("{text} ({last})");
        }
    }
    match error.src() {
        Some(source) => format!("{}: {text}", source.name()),
        None => text,
    }
}

/// Runs `work` on a thread of its own and gives up after `timeout`. A driver that hangs inside
/// its open call then costs a stuck thread, not the recording.
fn within<T: Send + 'static>(
    timeout: Duration,
    work: impl FnOnce() -> std::result::Result<T, String> + Send + 'static,
) -> std::result::Result<T, String> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("glimpsfy-encoder-check".into())
        .spawn(move || {
            let _ = tx.send(work());
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(timeout).unwrap_or_else(|_| Err(format!("it did not answer within {} seconds", timeout.as_secs())))
}

/// What a trial found: what it measured (an AAC encoder's lead, nothing for video), or why the
/// encoder failed.
type Trial = std::result::Result<u64, String>;

/// Trials already run in this process, by encoder and size: an encoder that worked once keeps
/// working, and one that failed is not tried again on every take.
fn trials() -> &'static Mutex<HashMap<String, Trial>> {
    static TRIALS: OnceLock<Mutex<HashMap<String, Trial>>> = OnceLock::new();
    TRIALS.get_or_init(Default::default)
}

fn trial(key: String, run: impl FnOnce() -> Trial + Send + 'static) -> Trial {
    if let Some(known) = lock(trials()).get(&key) {
        return known.clone();
    }
    let outcome = within(TRIAL_TIMEOUT, run);
    lock(trials()).insert(key, outcome.clone());
    outcome
}

/// What a video encoder is set up for.
#[derive(Clone, Copy, Debug)]
pub(super) struct VideoTarget {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bits_per_second: u64,
    pub keyframe_frames: u32,
}

impl VideoTarget {
    fn pixels_per_second(&self) -> u64 {
        self.width as u64 * self.height as u64 * self.fps.max(1) as u64
    }
}

const H264_ENCODERS: [&str; 5] = ["x264enc", "openh264enc", "vah264enc", "vaapih264enc", "nvh264enc"];
const HEVC_ENCODERS: [&str; 4] = ["x265enc", "vah265enc", "vaapih265enc", "nvh265enc"];
const AAC_ENCODERS: [&str; 4] = ["fdkaacenc", "avenc_aac", "voaacenc", "faac"];
/// 1920×1080 at 30 frames per second.
const FULL_HD_30: u64 = 1920 * 1080 * 30;

/// A video encoder that encoded a trial frame of the take's size on this machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct VideoEncoder {
    pub factory: &'static str,
    pub codec: VideoCodec,
}

impl VideoEncoder {
    fn software(&self) -> bool {
        matches!(self.factory, "x264enc" | "openh264enc" | "x265enc")
    }

    /// The raw format handed to the encoder: 4:2:0, which every player decodes (x264 would
    /// pick 4:4:4 from BGRA, which browsers and hardware decoders refuse).
    pub fn raw_caps(&self) -> gst::Caps {
        gst::Caps::builder("video/x-raw")
            .field("format", if self.software() { "I420" } else { "NV12" })
            // Screen pixels are sRGB, which shares its primaries with BT.709; BT.709 is what
            // players read for HD video.
            .field("colorimetry", "bt709")
            .build()
    }

    pub fn parser(&self) -> &'static str {
        match self.codec {
            VideoCodec::H264 => "h264parse",
            VideoCodec::Hevc => "h265parse",
        }
    }

    /// The stream `mp4mux` takes: parameter sets in the sample description (`avc1`, `hvc1`),
    /// which is what QuickTime and the browsers expect.
    pub fn stream_caps(&self) -> gst::Caps {
        stream_caps(self.codec)
    }

    pub fn element(&self, target: &VideoTarget) -> Result<gst::Element> {
        let encoder = make(self.factory)?;
        let kbps = (target.bits_per_second / 1000).max(100).to_string();
        let keyframes = target.keyframe_frames.max(1).to_string();
        let fast = target.pixels_per_second() > FULL_HD_30;
        match self.factory {
            "x264enc" => {
                set(&encoder, "bitrate", &kbps);
                let preset = if target.pixels_per_second() > 2 * FULL_HD_30 {
                    "ultrafast"
                } else if fast {
                    "superfast"
                } else {
                    "veryfast"
                };
                set(&encoder, "speed-preset", preset);
                set(&encoder, "key-int-max", &keyframes);
                // No B-frames: a take is appended and cut in presentation order, and a
                // pass-through trim can end on any frame.
                set(&encoder, "bframes", "0");
                set(&encoder, "b-adapt", "false");
                // A change on screen may spend two seconds' worth of bits before it evens out.
                set(&encoder, "vbv-buf-capacity", "2000");
                set(&encoder, "aud", "false");
            }
            "openh264enc" => {
                set(&encoder, "bitrate", &target.bits_per_second.min(u32::MAX as u64).to_string());
                set(&encoder, "rate-control", "bitrate");
                set(&encoder, "gop-size", &keyframes);
                set(&encoder, "enable-frame-skip", "false");
                set(&encoder, "complexity", if fast { "low" } else { "medium" });
                set(&encoder, "multi-thread", &threads());
                set(&encoder, "slice-mode", "auto");
            }
            "x265enc" => {
                set(&encoder, "bitrate", &kbps);
                set(
                    &encoder,
                    "speed-preset",
                    if target.pixels_per_second() > 1280 * 720 * 30 { "ultrafast" } else { "superfast" },
                );
                set(&encoder, "key-int-max", &keyframes);
                set(&encoder, "option-string", "bframes=0");
            }
            _ => {
                // The hardware encoders name the same settings differently.
                set(&encoder, "bitrate", &kbps);
                for name in ["key-int-max", "keyframe-period", "gop-size"] {
                    set(&encoder, name, &keyframes);
                }
                for name in ["b-frames", "max-bframes", "bframes"] {
                    set(&encoder, name, "0");
                }
                set(&encoder, "rate-control", "cbr");
                set(&encoder, "rc-mode", "cbr");
            }
        }
        Ok(encoder)
    }
}

pub(super) fn stream_caps(codec: VideoCodec) -> gst::Caps {
    match codec {
        VideoCodec::H264 => {
            gst::Caps::builder("video/x-h264").field("stream-format", "avc").field("alignment", "au").build()
        }
        VideoCodec::Hevc => {
            gst::Caps::builder("video/x-h265").field("stream-format", "hvc1").field("alignment", "au").build()
        }
    }
}

fn codec_name(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "H.264",
        VideoCodec::Hevc => "HEVC",
    }
}

/// The first encoder of `wanted` that encodes a frame of `target`'s size here. HEVC falls back
/// to H.264 when no HEVC encoder works and the picture is small enough for H.264.
pub(super) fn video_encoder(wanted: VideoCodec, target: VideoTarget) -> Result<VideoEncoder> {
    init()?;
    let size = glimpsfy_capture::PhysicalSize { width: target.width, height: target.height };
    let h264_fits = codec_for(VideoCodec::H264, size) == VideoCodec::H264;
    let mut codecs = vec![wanted];
    if wanted == VideoCodec::Hevc && h264_fits {
        codecs.push(VideoCodec::H264);
    }
    let mut failures = Vec::new();
    for codec in codecs {
        let list: &[&'static str] = match codec {
            VideoCodec::H264 => &H264_ENCODERS,
            VideoCodec::Hevc => &HEVC_ENCODERS,
        };
        for &factory in list.iter().filter(|f| installed(f)) {
            let encoder = VideoEncoder { factory, codec };
            let key = format!("{factory} {}x{} {}", target.width, target.height, target.fps);
            match trial(key, move || video_trial(encoder, target).map(|()| 0)) {
                Ok(_) => return Ok(encoder),
                Err(reason) => failures.push(format!("{factory}: {reason}")),
            }
        }
    }
    if !failures.is_empty() {
        return Err(RecordError::Encoder(format!(
            "no {} encoder works on this machine ({})",
            codec_name(wanted),
            failures.join("; ")
        )));
    }
    Err(RecordError::Encoder(if wanted == VideoCodec::Hevc && h264_fits {
        "no HEVC or H.264 encoder is installed (gstreamer1.0-plugins-ugly provides x264enc, and \
         gstreamer1.0-plugins-bad x265enc and openh264enc, on Debian and Ubuntu; gstreamer1-plugin-openh264 \
         provides one on Fedora)"
            .into()
    } else if wanted == VideoCodec::Hevc {
        format!(
            "no HEVC encoder is installed, and a {}×{} picture is larger than H.264 allows \
             (gstreamer1.0-plugins-bad provides x265enc on Debian and Ubuntu, and \
             gstreamer1-plugins-bad-free a VA-API one on Fedora)",
            target.width, target.height
        )
    } else {
        "no H.264 encoder is installed (gstreamer1.0-plugins-ugly or gstreamer1.0-plugins-bad provides one \
         on Debian and Ubuntu, and gstreamer1-plugin-openh264 on Fedora)"
            .into()
    }))
}

/// Encodes one black frame of the take's size with the take's settings, through the parser and
/// into the stream `mp4mux` takes.
fn video_trial(encoder: VideoEncoder, target: VideoTarget) -> std::result::Result<(), String> {
    let text = |e: RecordError| e.to_string();
    let pipeline = gst::Pipeline::new();
    let src = gst_app::AppSrc::builder()
        .caps(&bgra_caps(target.width, target.height, target.fps))
        .format(gst::Format::Time)
        .build();
    let convert = make("videoconvert").map_err(text)?;
    let raw = capsfilter(&encoder.raw_caps()).map_err(text)?;
    let element = encoder.element(&target).map_err(text)?;
    let parse = make(encoder.parser()).map_err(text)?;
    let stream = capsfilter(&encoder.stream_caps()).map_err(text)?;
    let sink = make("fakesink").map_err(text)?;
    set(&sink, "sync", "false");
    let chain = [src.upcast_ref::<gst::Element>(), &convert, &raw, &element, &parse, &stream, &sink];
    pipeline.add_many(chain).map_err(|e| e.to_string())?;
    gst::Element::link_many(chain).map_err(|e| e.to_string())?;
    let frame = vec![0u8; target.width as usize * target.height as usize * 4];
    run_trial(&pipeline, &src, frame, 1_000_000_000 / target.fps.max(1) as u64, || {})
}

/// Pushes `data` as one buffer and waits for the end; `drain` takes what the sinks hold before
/// the pipeline stops, which empties them.
fn run_trial(
    pipeline: &gst::Pipeline,
    src: &gst_app::AppSrc,
    data: Vec<u8>,
    duration_ns: u64,
    drain: impl FnOnce(),
) -> std::result::Result<(), String> {
    let watch = Watch::on(pipeline);
    let outcome = (|| {
        pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| watch.error().unwrap_or_else(|| "it could not start".into()))?;
        let mut buffer = gst::Buffer::from_mut_slice(data);
        if let Some(b) = buffer.get_mut() {
            b.set_pts(gst::ClockTime::ZERO);
            b.set_duration(gst::ClockTime::from_nseconds(duration_ns));
        }
        src.push_buffer(buffer).map_err(|e| format!("{e:?}"))?;
        src.end_of_stream().map_err(|e| format!("{e:?}"))?;
        watch.wait_eos(TRIAL_TIMEOUT)?;
        drain();
        Ok(())
    })();
    let _ = pipeline.set_state(gst::State::Null);
    outcome
}

/// An AAC encoder that encodes here, and its lead: the frames of its own it puts in front of the
/// sound (1024 for `avenc_aac`, 1600 for `voaacenc`), which a decoder plays before the first
/// frame it was given.
#[derive(Clone, Copy, Debug)]
pub(super) struct AacEncoder {
    pub factory: &'static str,
    pub lead: u64,
}

/// The lead of almost every AAC-LC encoder, for a machine without a decoder to measure it with.
const USUAL_AAC_LEAD: u64 = 1024;

/// The first AAC encoder that encodes here; chosen once per process.
pub(super) fn aac_encoder() -> Result<AacEncoder> {
    init()?;
    let mut failures = Vec::new();
    for &factory in AAC_ENCODERS.iter().filter(|f| installed(f)) {
        match trial(factory.to_string(), move || aac_trial(factory)) {
            Ok(lead) => return Ok(AacEncoder { factory, lead }),
            Err(reason) => failures.push(format!("{factory}: {reason}")),
        }
    }
    Err(RecordError::Encoder(if failures.is_empty() {
        "no AAC encoder is installed (gstreamer1.0-libav or gstreamer1.0-plugins-bad provides one on Debian \
         and Ubuntu, and gstreamer1-plugin-libav or gstreamer1-plugins-bad-free on Fedora)"
            .into()
    } else {
        format!("no AAC encoder works on this machine ({})", failures.join("; "))
    }))
}

pub(super) fn aac_element(factory: &str, bits_per_second: u32) -> Result<gst::Element> {
    let encoder = make(factory)?;
    set(&encoder, "bitrate", &bits_per_second.to_string());
    Ok(encoder)
}

/// The first frame of `samples` (interleaved stereo) that is clearly sound.
fn first_sound(samples: impl Iterator<Item = f32>) -> Option<u64> {
    samples.step_by(crate::audio::CHANNELS).position(|s| s.abs() > 0.05).map(|i| i as u64)
}

/// Encodes silence and then a tone, decodes it again, and measures how much later the tone
/// comes back: the encoder's lead. The lead does not depend on how the sound is cut into
/// buffers, so one buffer measures it.
fn aac_trial(factory: &'static str) -> Trial {
    let text = |e: RecordError| e.to_string();
    let pipeline = gst::Pipeline::new();
    let src = gst_app::AppSrc::builder().caps(&pcm_caps()).format(gst::Format::Time).build();
    let convert = make("audioconvert").map_err(text)?;
    let encoder = aac_element(factory, 160_000).map_err(text)?;
    let parse = make("aacparse").map_err(text)?;
    let mut chain = vec![src.clone().upcast::<gst::Element>(), convert, encoder, parse];
    let decoder = decoders(Coded::Aac).ok().and_then(|found| found.first().copied());
    let sink = match decoder {
        Some(decoder) => {
            chain.push(make(decoder).map_err(text)?);
            chain.push(make("audioconvert").map_err(text)?);
            // The end is reported at once; by default the sink waits for its samples to be taken.
            let sink = gst_app::AppSink::builder().caps(&pcm_caps()).sync(false).wait_on_eos(false).build();
            chain.push(sink.clone().upcast());
            Some(sink)
        }
        None => {
            let sink = make("fakesink").map_err(text)?;
            set(&sink, "sync", "false");
            chain.push(sink);
            None
        }
    };
    pipeline.add_many(&chain).map_err(|e| e.to_string())?;
    gst::Element::link_many(&chain).map_err(|e| e.to_string())?;
    let (silence, tone) = (4096usize, 4096usize);
    let input: Vec<f32> = (0..silence + tone)
        .flat_map(|i| {
            let s = if i < silence {
                0.0
            } else {
                ((i - silence) as f32 * 1000.0 * std::f32::consts::TAU / crate::audio::RATE as f32).sin() * 0.5
            };
            [s; crate::audio::CHANNELS]
        })
        .collect();
    let expected = first_sound(input.iter().copied());
    let data: Vec<u8> = input.iter().flat_map(|s| s.to_le_bytes()).collect();
    let duration = crate::audio::ns_at((silence + tone) as u64);
    let mut decoded = Vec::new();
    run_trial(&pipeline, &src, data, duration, || {
        let Some(sink) = &sink else { return };
        while let Some(sample) = sink.try_pull_sample(gst::ClockTime::ZERO) {
            if let Some(map) = sample.buffer().and_then(|b| b.map_readable().ok()) {
                decoded.extend(map.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])));
            }
        }
    })?;
    if sink.is_none() {
        return Ok(USUAL_AAC_LEAD);
    }
    // A decoder that gives the tone back early or not at all says nothing about the encoder.
    Ok(match (expected, first_sound(decoded.into_iter())) {
        (Some(expected), Some(found)) if found >= expected && found - expected <= 8192 => found - expected,
        _ => USUAL_AAC_LEAD,
    })
}

/// What a decoder decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Coded {
    H264,
    Hevc,
    Aac,
}

/// The installed decoders for `coded`, best first: software before hardware, for the same
/// reason as the encoders.
pub(super) fn decoders(coded: Coded) -> Result<Vec<&'static str>> {
    init()?;
    let all: &[&'static str] = match coded {
        Coded::H264 => &["avdec_h264", "openh264dec", "vah264dec", "vaapih264dec", "nvh264dec", "v4l2h264dec"],
        Coded::Hevc => &["avdec_h265", "libde265dec", "vah265dec", "vaapih265dec", "nvh265dec", "v4l2h265dec"],
        Coded::Aac => &["avdec_aac", "fdkaacdec", "faad"],
    };
    let found: Vec<&'static str> = all.iter().copied().filter(|f| installed(f)).collect();
    if found.is_empty() {
        let (what, packages) = match coded {
            Coded::H264 => ("H.264", "gstreamer1.0-libav provides one on Debian and Ubuntu, and gstreamer1-plugin-libav or gstreamer1-plugin-openh264 on Fedora"),
            Coded::Hevc => ("HEVC", "gstreamer1.0-libav provides one on Debian and Ubuntu, and gstreamer1-plugin-libav on Fedora"),
            Coded::Aac => ("AAC", "gstreamer1.0-libav provides one on Debian and Ubuntu, and gstreamer1-plugin-libav on Fedora"),
        };
        return Err(RecordError::Encoder(format!("no {what} decoder is installed ({packages})")));
    }
    Ok(found)
}

/// Waits until `ready` holds, `cancel` is set or the pipeline failed, checking every couple of
/// milliseconds; gives up with an error after `stall`.
pub(super) fn wait_until(
    watch: &Watch,
    cancel: Option<&AtomicBool>,
    stall: Duration,
    mut ready: impl FnMut() -> bool,
) -> Result<()> {
    let deadline = Instant::now() + stall;
    while !ready() {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            return Err(RecordError::Cancelled);
        }
        if let Some(error) = watch.error() {
            return Err(RecordError::Encoder(error));
        }
        if Instant::now() >= deadline {
            return Err(RecordError::Encoder("the encoder stopped taking frames".into()));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}
