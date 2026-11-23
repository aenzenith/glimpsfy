//! Reading recordings back on Linux with GStreamer: what a file holds, its frames for the GIF
//! export and the trim timeline, its waveform, the MP4 export (pass-through or re-encoded), and
//! the decoded pictures and sound an export with cuts (and the speech recognizer) reads.
//!
//! Every reader is `filesrc → qtdemux` with an `appsink` per track it needs, so nothing is
//! decoded that is not asked for, and no decoder is plugged by rank: the decoders are tried in
//! the order `elements::decoders` gives, software first. The MP4 export pumps samples from such a
//! reader into a second pipeline that ends in `mp4mux`, oldest sample first, the way the macOS
//! export moves samples from its `AVAssetReader` to its `AVAssetWriter`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use glimpsfy_capture::{
    ColorInfo, ColorSpace, Frame, HostTime, PhysicalSize, PixelFormat, ScaleFactor, StreamPixelFormat, VideoBuffer,
    VideoFrame,
};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use gstreamer_video::VideoFrameExt as _;

use super::elements::{self, set, Coded, VideoTarget, Watch};
use super::writer::track_title;
use crate::export::Mp4Plan;
use crate::{MediaInfo, RecordError, Result, TrackKind, TrimRange, VideoCodec};

/// How long a pipeline may take to open a file and show its first sample.
const OPEN_TIMEOUT: Duration = Duration::from_secs(15);
/// A reader that gives nothing for this long has stopped.
const STALL: Duration = Duration::from_secs(30);
/// How long an export may take to finish once every sample is in.
const EXPORT_FINISH: Duration = Duration::from_secs(120);
/// The coded video a reader reads ahead of its sink: eight seconds at the highest bit rate.
const VIDEO_QUEUE_BYTES: u32 = 64 * 1024 * 1024;

/// What `inspect` found, with what the other readers need to open the file again.
struct Probe {
    info: MediaInfo,
    duration_ns: u64,
    video: Coded,
    audio_tracks: usize,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn missing_file(path: &Path) -> RecordError {
    RecordError::Io(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} does not exist", path.display())))
}

fn location(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| RecordError::InvalidConfig(format!("{} is not a UTF-8 path", path.display())))
}

fn build_error(e: impl std::fmt::Display) -> RecordError {
    RecordError::Encoder(format!("the reading pipeline could not be built: {e}"))
}

/// The codec as the container names it (`avc1`, `hvc1`), from the demuxer's caps.
fn codec_tag(caps: &gst::CapsRef) -> (String, Option<Coded>) {
    let Some(s) = caps.structure(0) else { return (String::new(), None) };
    let format = s.get::<&str>("stream-format").ok();
    match s.name().as_str() {
        "video/x-h264" => (if format == Some("avc3") { "avc3" } else { "avc1" }.into(), Some(Coded::H264)),
        "video/x-h265" => (if format == Some("hev1") { "hev1" } else { "hvc1" }.into(), Some(Coded::Hevc)),
        other => (other.trim_start_matches("video/x-").to_string(), None),
    }
}

fn title_of(pad: &gst::Pad) -> Option<String> {
    (0..8).filter_map(|i| pad.sticky_event::<gst::event::Tag>(i)).find_map(|event| {
        let tags = event.tag();
        tags.generic("title").and_then(|v| v.get::<String>().ok())
    })
}

/// The kinds of the audio tracks, from the titles the writer gave them, or by their count for a
/// file whose titles were lost (the writer's order: the mix first, then microphone, then system).
fn audio_kinds(titles: &[Option<String>]) -> Vec<TrackKind> {
    let by_title: Vec<Option<TrackKind>> = titles
        .iter()
        .map(|t| {
            let title = t.as_deref()?;
            [TrackKind::MixedAudio, TrackKind::Microphone, TrackKind::SystemAudio]
                .into_iter()
                .find(|&k| track_title(k) == title)
        })
        .collect();
    if by_title.iter().all(Option::is_some) {
        return by_title.into_iter().flatten().collect();
    }
    match titles.len() {
        0 => vec![],
        3 => vec![TrackKind::MixedAudio, TrackKind::Microphone, TrackKind::SystemAudio],
        n => vec![TrackKind::MixedAudio; n],
    }
}

/// The demuxer's pads, and whether each has sent its first sample (or its end) yet.
type Exposed = Arc<Mutex<Vec<(gst::Pad, bool)>>>;

/// Opens `path` far enough to see its tracks. A track's caps and tags are on its pad before its
/// first sample leaves, so the samples are dropped right there and nothing prerolls: the demuxer
/// pushes every track from one thread, and a sink holding its first sample would hold back the
/// first sample of every other track.
fn probe(path: &Path) -> Result<Probe> {
    if !path.is_file() {
        return Err(missing_file(path));
    }
    elements::init()?;
    let pipeline = gst::Pipeline::new();
    let src = elements::make("filesrc")?;
    src.set_property("location", location(path)?);
    let demux = elements::make("qtdemux")?;
    pipeline.add_many([&src, &demux]).map_err(build_error)?;
    src.link(&demux).map_err(build_error)?;
    let exposed: Exposed = Arc::default();
    {
        let exposed = exposed.clone();
        let weak = pipeline.downgrade();
        demux.connect_pad_added(move |_, pad| {
            // A linked pad, so the demuxer counts the track as wanted and goes on reading.
            if let (Some(pipeline), Ok(sink)) = (weak.upgrade(), elements::make("fakesink")) {
                set(&sink, "sync", "false");
                set(&sink, "async", "false");
                if pipeline.add(&sink).is_ok() {
                    let _ = sink.sync_state_with_parent();
                    if let Some(target) = sink.static_pad("sink") {
                        let _ = pad.link(&target);
                    }
                }
            }
            lock(&exposed).push((pad.clone(), false));
            let exposed = exposed.clone();
            pad.add_probe(gst::PadProbeType::BUFFER | gst::PadProbeType::EVENT_DOWNSTREAM, move |pad, info| {
                let buffer = matches!(info.data, Some(gst::PadProbeData::Buffer(_)));
                let end = matches!(&info.data, Some(gst::PadProbeData::Event(e)) if e.type_() == gst::EventType::Eos);
                if buffer || end {
                    if let Some(entry) = lock(&exposed).iter_mut().find(|(p, _)| p == pad) {
                        entry.1 = true;
                    }
                }
                if buffer {
                    gst::PadProbeReturn::Drop
                } else {
                    gst::PadProbeReturn::Ok
                }
            });
        });
    }
    let all_exposed = Arc::new(AtomicBool::new(false));
    {
        let all_exposed = all_exposed.clone();
        demux.connect_no_more_pads(move |_| all_exposed.store(true, Ordering::SeqCst));
    }
    let watch = Watch::on(&pipeline);
    let opened = (|| -> Result<u64> {
        pipeline.set_state(gst::State::Paused).map_err(|_| unreadable(path, &watch))?;
        let ready = || all_exposed.load(Ordering::SeqCst) && lock(&exposed).iter().all(|(_, seen)| *seen);
        elements::wait_until(&watch, None, OPEN_TIMEOUT, ready).map_err(|_| unreadable(path, &watch))?;
        Ok(lock(&exposed)
            .iter()
            .filter_map(|(pad, _)| pad.query_duration::<gst::ClockTime>())
            .map(|d| d.nseconds())
            .max()
            .unwrap_or(0))
    })();
    let duration_ns = opened.inspect_err(|_| {
        let _ = pipeline.set_state(gst::State::Null);
    })?;
    let mut pads: Vec<gst::Pad> = lock(&exposed).iter().map(|(p, _)| p.clone()).collect();
    pads.sort_by_key(|p| p.name().to_string());
    let video = pads.iter().find(|p| p.name() == "video_0").cloned();
    let mut audio: Vec<(u32, Option<String>)> = pads
        .iter()
        .filter_map(|p| {
            let index = p.name().strip_prefix("audio_")?.parse::<u32>().ok()?;
            Some((index, title_of(p)))
        })
        .collect();
    audio.sort_by_key(|(i, _)| *i);
    let result = (|| {
        let video = video.ok_or_else(|| RecordError::Encoder(format!("{} has no video track", path.display())))?;
        let caps = video.current_caps().ok_or_else(|| unreadable(path, &watch))?;
        let (video_codec, coded) = codec_tag(&caps);
        let coded = coded.ok_or_else(|| {
            RecordError::Encoder(format!("{} holds {video_codec} video, which is not H.264 or HEVC", path.display()))
        })?;
        let s = caps.structure(0).ok_or_else(|| unreadable(path, &watch))?;
        let width = s.get::<i32>("width").unwrap_or(0).max(0) as u32;
        let height = s.get::<i32>("height").unwrap_or(0).max(0) as u32;
        let fps = s
            .get::<gst::Fraction>("framerate")
            .ok()
            .filter(|f| f.denom() > 0)
            .map(|f| f.numer() as f64 / f.denom() as f64)
            .filter(|&fps| fps > 0.0)
            .or_else(|| super::mp4::video_rate(path))
            .unwrap_or(0.0);
        let titles: Vec<Option<String>> = audio.iter().map(|(_, t)| t.clone()).collect();
        Ok(Probe {
            info: MediaInfo {
                duration_ms: duration_ns / 1_000_000,
                size: PhysicalSize { width, height },
                fps,
                video_codec,
                audio: audio_kinds(&titles),
                file_bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            },
            duration_ns,
            video: coded,
            audio_tracks: audio.len(),
        })
    })();
    let _ = pipeline.set_state(gst::State::Null);
    result
}

fn unreadable(path: &Path, watch: &Watch) -> RecordError {
    RecordError::Encoder(
        watch.error().unwrap_or_else(|| format!("{} could not be read as an MP4 file", path.display())),
    )
}

pub fn inspect(path: &Path) -> Result<MediaInfo> {
    Ok(probe(path)?.info)
}

/// Where the video of a reader goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VideoOut {
    Off,
    /// The samples as the file holds them.
    Coded,
    /// Decoded to packed RGBA.
    Rgba,
    /// Decoded, in whatever raw format the decoder gives.
    Raw,
}

/// Where the sound of a reader goes: the audio track by index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioOut {
    Off,
    Coded(usize),
    /// Decoded to interleaved 32-bit float.
    Pcm(usize),
}

/// A reading pipeline over one file.
struct Reader {
    pipeline: gst::Pipeline,
    watch: Arc<Watch>,
    video: Option<gst_app::AppSink>,
    audio: Option<gst_app::AppSink>,
}

impl Drop for Reader {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// A queue that holds up to `max_bytes` of one track (none means no limit), however many samples
/// or seconds that is: a track that starts late must not fill the other track's queue while the
/// pipeline prerolls.
fn track_queue(max_bytes: u32) -> Result<gst::Element> {
    let queue = elements::make("queue")?;
    set(&queue, "max-size-buffers", "0");
    set(&queue, "max-size-time", "0");
    set(&queue, "max-size-bytes", &max_bytes.to_string());
    Ok(queue)
}

fn appsink(caps: Option<&gst::Caps>, max_buffers: u32) -> gst_app::AppSink {
    let builder = gst_app::AppSink::builder().sync(false).max_buffers(max_buffers).drop(false);
    match caps {
        Some(caps) => builder.caps(caps).build(),
        None => builder.build(),
    }
}

impl Reader {
    /// Opens a reader, trying each installed decoder in turn until one prerolls.
    fn open(path: &Path, probe: &Probe, video: VideoOut, audio: AudioOut) -> Result<Reader> {
        let video_decoders: Vec<Option<&'static str>> = match video {
            VideoOut::Rgba | VideoOut::Raw => elements::decoders(probe.video)?.into_iter().map(Some).collect(),
            _ => vec![None],
        };
        let audio_decoders: Vec<Option<&'static str>> = match audio {
            AudioOut::Pcm(_) => elements::decoders(Coded::Aac)?.into_iter().map(Some).collect(),
            _ => vec![None],
        };
        let mut last = None;
        for &video_decoder in &video_decoders {
            for &audio_decoder in &audio_decoders {
                match Reader::open_with(path, probe, video, audio, video_decoder, audio_decoder) {
                    Ok(reader) => return Ok(reader),
                    Err(e) => last = Some(e),
                }
            }
        }
        Err(last.unwrap_or_else(|| RecordError::Encoder(format!("{} could not be read", path.display()))))
    }

    fn open_with(
        path: &Path,
        probe: &Probe,
        video: VideoOut,
        audio: AudioOut,
        video_decoder: Option<&str>,
        audio_decoder: Option<&str>,
    ) -> Result<Reader> {
        let pipeline = gst::Pipeline::new();
        let src = elements::make("filesrc")?;
        src.set_property("location", location(path)?);
        let demux = elements::make("qtdemux")?;
        pipeline.add_many([&src, &demux]).map_err(build_error)?;
        src.link(&demux).map_err(build_error)?;

        let parser = match probe.video {
            Coded::Hevc => "h265parse",
            _ => "h264parse",
        };
        // Every track starts with a queue: the demuxer pushes all tracks from one thread, and
        // without a thread per track a sink holding its first sample while the pipeline prerolls
        // would keep the other track from its own.
        let (video_sink, video_entry) = match (video, video_decoder) {
            (VideoOut::Off, _) => (None, None),
            (VideoOut::Coded, _) => {
                let queue = track_queue(VIDEO_QUEUE_BYTES)?;
                let sink = appsink(None, 8);
                let chain = [queue.clone(), sink.clone().upcast()];
                pipeline.add_many(&chain).map_err(build_error)?;
                gst::Element::link_many(&chain).map_err(build_error)?;
                (Some(sink), queue.static_pad("sink"))
            }
            (_, decoder) => {
                let decoder = decoder.ok_or_else(|| build_error("no video decoder"))?;
                let queue = track_queue(VIDEO_QUEUE_BYTES)?;
                let parse = elements::make(parser)?;
                let decode = elements::make(decoder)?;
                let mut chain = vec![queue.clone(), parse, decode];
                let sink = if video == VideoOut::Rgba {
                    let convert = elements::make("videoconvert")?;
                    set(&convert, "n-threads", &elements::threads());
                    chain.push(convert);
                    appsink(Some(&gst::Caps::builder("video/x-raw").field("format", "RGBA").build()), 2)
                } else {
                    appsink(None, 2)
                };
                chain.push(sink.clone().upcast());
                pipeline.add_many(&chain).map_err(build_error)?;
                gst::Element::link_many(&chain).map_err(build_error)?;
                (Some(sink), queue.static_pad("sink"))
            }
        };
        // The sound is small and runs ahead of decoded video, so its queue and its sink are
        // unbounded: a full one would stall the demuxer, which feeds both.
        let (audio_sink, audio_entry, audio_index) = match (audio, audio_decoder) {
            (AudioOut::Off, _) => (None, None, None),
            (AudioOut::Coded(index), _) => {
                let queue = track_queue(0)?;
                let sink = appsink(None, 0);
                let chain = [queue.clone(), sink.clone().upcast()];
                pipeline.add_many(&chain).map_err(build_error)?;
                gst::Element::link_many(&chain).map_err(build_error)?;
                (Some(sink), queue.static_pad("sink"), Some(index))
            }
            (AudioOut::Pcm(index), decoder) => {
                let decoder = decoder.ok_or_else(|| build_error("no audio decoder"))?;
                let queue = track_queue(0)?;
                let parse = elements::make("aacparse")?;
                let decode = elements::make(decoder)?;
                let convert = elements::make("audioconvert")?;
                let caps =
                    gst::Caps::builder("audio/x-raw").field("format", "F32LE").field("layout", "interleaved").build();
                let sink = appsink(Some(&caps), 0);
                let chain = [queue.clone(), parse, decode, convert, sink.clone().upcast()];
                pipeline.add_many(&chain).map_err(build_error)?;
                gst::Element::link_many(&chain).map_err(build_error)?;
                (Some(sink), queue.static_pad("sink"), Some(index))
            }
        };
        let audio_name = audio_index.map(|i| format!("audio_{i}"));
        demux.connect_pad_added(move |_, pad| {
            let name = pad.name();
            let target = if name == "video_0" {
                video_entry.as_ref()
            } else if audio_name.as_deref() == Some(name.as_str()) {
                audio_entry.as_ref()
            } else {
                None
            };
            if let Some(target) = target.filter(|t| !t.is_linked()) {
                let _ = pad.link(target);
            }
        });
        let watch = Watch::on(&pipeline);
        let reader = Reader { pipeline, watch, video: video_sink, audio: audio_sink };
        reader.pipeline.set_state(gst::State::Paused).map_err(|_| unreadable(path, &reader.watch))?;
        reader.settle(path)?;
        Ok(reader)
    }

    /// Waits until every sink holds its first sample.
    fn settle(&self, path: &Path) -> Result<()> {
        let (state, _, _) = self.pipeline.state(gst::ClockTime::from_nseconds(OPEN_TIMEOUT.as_nanos() as u64));
        match state {
            Ok(gst::StateChangeSuccess::Success) if self.watch.error().is_none() => Ok(()),
            _ => Err(unreadable(path, &self.watch)),
        }
    }

    /// Moves to `start` (and stops at `stop`): `accurate` starts at the sample shown at
    /// `start`, otherwise the reader starts at the keyframe at or before it.
    fn seek(&self, path: &Path, start_ns: u64, stop_ns: Option<u64>, accurate: bool) -> Result<()> {
        let flags = gst::SeekFlags::FLUSH
            | if accurate { gst::SeekFlags::ACCURATE } else { gst::SeekFlags::KEY_UNIT | gst::SeekFlags::SNAP_BEFORE };
        let stop = stop_ns.map(gst::ClockTime::from_nseconds);
        let stop_type = if stop.is_some() { gst::SeekType::Set } else { gst::SeekType::None };
        self.pipeline
            .seek(1.0, flags, gst::SeekType::Set, Some(gst::ClockTime::from_nseconds(start_ns)), stop_type, stop)
            .map_err(|_| {
                RecordError::Encoder(format!("{} could not be read from {} ms", path.display(), start_ns / 1_000_000))
            })?;
        self.settle(path)
    }

    fn play(&self) -> Result<()> {
        self.pipeline.set_state(gst::State::Playing).map(|_| ()).map_err(|_| {
            RecordError::Encoder(self.watch.error().unwrap_or_else(|| "the file could not be read".into()))
        })
    }

    /// The next sample of `sink`, or `None` at the end.
    fn pull(&self, sink: &gst_app::AppSink, cancel: Option<&AtomicBool>) -> Result<Option<gst::Sample>> {
        let deadline = Instant::now() + STALL;
        loop {
            if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
                return Err(RecordError::Cancelled);
            }
            if let Some(error) = self.watch.error() {
                return Err(RecordError::Encoder(error));
            }
            if let Some(sample) = sink.try_pull_sample(gst::ClockTime::from_mseconds(50)) {
                return Ok(Some(sample));
            }
            if sink.is_eos() {
                return Ok(None);
            }
            if Instant::now() >= deadline {
                return Err(RecordError::Encoder("the file stopped giving samples".into()));
            }
        }
    }
}

/// A sample's time on the file's timeline: its stream time, which counts the edit list. An
/// export whose sound starts a moment after its picture starts that track with an empty edit,
/// and the demuxer puts that in the segment, not in the timestamps.
fn pts_of(sample: &gst::Sample) -> Option<u64> {
    let pts = sample.buffer()?.pts()?;
    let segment = sample.segment().and_then(|s| s.downcast_ref::<gst::ClockTime>());
    Some(match segment.and_then(|s| s.to_stream_time_full(pts)) {
        Some(gst::Signed::Positive(t)) => t.nseconds(),
        Some(gst::Signed::Negative(_)) => 0,
        None => pts.nseconds(),
    })
}

/// Reads the RGBA picture of a decoded sample into a `Frame`.
fn frame_of(sample: &gst::Sample) -> Option<Frame> {
    let caps = sample.caps()?;
    let info = gst_video::VideoInfo::from_caps(caps).ok()?;
    let frame = gst_video::VideoFrameRef::from_buffer_ref_readable(sample.buffer()?, &info).ok()?;
    let stride = usize::try_from(*frame.plane_stride().first()?).ok()?;
    let data = frame.plane_data(0).ok()?;
    Frame::new(
        PhysicalSize { width: info.width(), height: info.height() },
        stride,
        PixelFormat::Rgba8,
        ColorInfo::srgb(),
        ScaleFactor(1.0),
        data.to_vec(),
    )
    .ok()
}

/// Packed RGBA of `frame` at `size`.
fn rgba(frame: &Frame, size: PhysicalSize) -> Vec<u8> {
    let scaled;
    let frame = if frame.size == size {
        frame
    } else {
        scaled = frame.resized(size);
        &scaled
    };
    let (w, h) = (frame.size.width as usize, frame.size.height as usize);
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let row = &frame.data[y * frame.bytes_per_row..y * frame.bytes_per_row + w * 4];
        for p in row.chunks_exact(4) {
            match frame.format {
                PixelFormat::Bgra8 => out.extend_from_slice(&[p[2], p[1], p[0], 255]),
                PixelFormat::Rgba8 => out.extend_from_slice(&[p[0], p[1], p[2], 255]),
            }
        }
    }
    out
}

/// Decodes the frames shown within `trim`, scaled to `size`. A recording has a variable frame
/// rate (a still screen sends no frames), so the frame on screen at the trim's start may come
/// from before it: the accurate seek decodes from the keyframe before the start and hands out
/// the frame that covers the start first, which goes out at time 0.
pub fn decode_video(
    source: &Path,
    trim: TrimRange,
    size: PhysicalSize,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(&[u8], u64) -> Result<()>,
) -> Result<()> {
    let probe = probe(source)?;
    let start = trim.start_ms * 1_000_000;
    let end = (trim.end_ms * 1_000_000).max(start + 1_000_000);
    let reader = Reader::open(source, &probe, VideoOut::Rgba, AudioOut::Off)?;
    reader.seek(source, start, Some(end), true)?;
    reader.play()?;
    let Some(sink) = reader.video.as_ref() else { return Ok(()) };
    let mut held: Option<Frame> = None;
    while let Some(sample) = reader.pull(sink, Some(cancel))? {
        let Some(at) = pts_of(&sample) else { continue };
        if at >= end {
            break;
        }
        let Some(frame) = frame_of(&sample) else { continue };
        if at < start {
            held = Some(frame);
            continue;
        }
        if let Some(first) = held.take() {
            if at > start {
                each(&rgba(&first, size), 0)?;
            }
        }
        each(&rgba(&frame, size), at - start)?;
    }
    if let Some(first) = held {
        each(&rgba(&first, size), 0)?;
    }
    Ok(())
}

/// The frames of `decode_video` as BGRA frames in memory, which the take writer hands to its
/// encoder without converting them again.
pub fn decode_frames(
    source: &Path,
    span: TrimRange,
    size: PhysicalSize,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(VideoFrame, u64) -> Result<()>,
) -> Result<()> {
    decode_video(source, span, size, cancel, &mut |rgba, at| {
        let mut bgra = vec![0u8; rgba.len()];
        // Swapping red and blue goes both ways.
        glimpsfy_capture::frame::swizzle_bgra_rgba(rgba, &mut bgra);
        let frame =
            Frame::new(size, size.width as usize * 4, PixelFormat::Bgra8, ColorInfo::srgb(), ScaleFactor(1.0), bgra)?;
        let frame = VideoFrame {
            timestamp: HostTime(at),
            size,
            format: StreamPixelFormat::Bgra,
            color_space: ColorSpace::Srgb,
            buffer: VideoBuffer::Cpu(Arc::new(frame)),
        };
        each(frame, at)
    })
}

/// Decodes audio track `track` within `span` to float, brought to 48 kHz stereo, buffer by
/// buffer with each buffer's position from the span's start.
pub fn decode_audio(
    source: &Path,
    track: usize,
    span: TrimRange,
    cancel: &AtomicBool,
    each: &mut dyn FnMut(&[f32], u64) -> Result<()>,
) -> Result<()> {
    let probe = probe(source)?;
    if track >= probe.audio_tracks {
        return Err(RecordError::Encoder(format!("{} has no audio track {track}", source.display())));
    }
    let start = span.start_ms.saturating_mul(1_000_000);
    let end = span.end_ms.saturating_mul(1_000_000).min(probe.duration_ns);
    if end <= start {
        return Ok(());
    }
    let reader = Reader::open(source, &probe, VideoOut::Off, AudioOut::Pcm(track))?;
    reader.seek(source, start, Some(end), true)?;
    reader.play()?;
    let Some(sink) = reader.audio.as_ref() else { return Ok(()) };
    let mut conformer = crate::audio::Conformer::new();
    let mut placed = crate::audio::SpanAudio::new(start, end);
    while let Some(sample) = reader.pull(sink, Some(cancel))? {
        let (Some(buffer), Some(at)) = (sample.buffer(), pts_of(&sample)) else { continue };
        let structure = sample.caps().and_then(|c| c.structure(0));
        let channels = structure.and_then(|s| s.get::<i32>("channels").ok()).unwrap_or(2).clamp(1, 16) as u16;
        let rate = structure.and_then(|s| s.get::<i32>("rate").ok()).unwrap_or(crate::audio::RATE as i32).max(1) as u32;
        let Ok(map) = buffer.map_readable() else { continue };
        let samples: Vec<f32> =
            map.as_slice().chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect();
        let stereo = conformer.conform(&samples, channels, rate);
        if let Some((position, body)) = placed.place(at, &stereo) {
            each(body, position)?;
        }
        if placed.done() {
            break;
        }
    }
    Ok(())
}

pub fn frame_at(source: &Path, at_ms: u64, max_height: u32) -> Result<Frame> {
    let info = inspect(source)?;
    let at_ms = at_ms.min(info.duration_ms.saturating_sub(1));
    let height = max_height.clamp(16, info.size.height.max(16));
    let width = ((info.size.width as f64 * height as f64 / info.size.height.max(1) as f64).round() as u32).max(1);
    let size = PhysicalSize { width, height };
    let mut picture = None;
    let cancel = AtomicBool::new(false);
    let trim = TrimRange { start_ms: at_ms, end_ms: at_ms + 1 };
    let result = decode_video(source, trim, size, &cancel, &mut |rgba, _| {
        if picture.is_none() {
            picture = Some(rgba.to_vec());
            cancel.store(true, Ordering::SeqCst);
        }
        Ok(())
    });
    match (picture, result) {
        (Some(data), _) => {
            Frame::new(size, width as usize * 4, PixelFormat::Rgba8, ColorInfo::srgb(), ScaleFactor(1.0), data)
                .map_err(RecordError::Capture)
        }
        (None, Err(e)) => Err(e),
        (None, Ok(())) => Err(RecordError::Encoder(format!("{} has no frame at {at_ms} ms", source.display()))),
    }
}

pub fn waveform(source: &Path, bins: usize) -> Result<Vec<f32>> {
    let bins = bins.clamp(1, 4096);
    let probe = probe(source)?;
    if probe.audio_tracks == 0 {
        return Ok(vec![0.0; bins]);
    }
    let duration = probe.duration_ns.max(1);
    let reader = Reader::open(source, &probe, VideoOut::Off, AudioOut::Pcm(0))?;
    reader.play()?;
    let Some(sink) = reader.audio.as_ref() else { return Ok(vec![0.0; bins]) };
    let mut peaks = vec![0f32; bins];
    while let Some(sample) = reader.pull(sink, None)? {
        let (Some(buffer), Some(start)) = (sample.buffer(), pts_of(&sample)) else { continue };
        let channels =
            sample.caps().and_then(|c| c.structure(0)).and_then(|s| s.get::<i32>("channels").ok()).unwrap_or(2).max(1)
                as usize;
        let rate = sample
            .caps()
            .and_then(|c| c.structure(0))
            .and_then(|s| s.get::<i32>("rate").ok())
            .unwrap_or(crate::audio::RATE as i32)
            .max(1) as u64;
        let Ok(map) = buffer.map_readable() else { continue };
        for (i, frame) in map.as_slice().chunks_exact(4 * channels).enumerate() {
            let peak =
                frame.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]).abs()).fold(0f32, f32::max);
            let at = start + i as u64 * 1_000_000_000 / rate;
            let bin = ((at as u128 * bins as u128 / duration as u128) as usize).min(bins - 1);
            peaks[bin] = peaks[bin].max(peak);
        }
    }
    Ok(peaks.into_iter().map(|p| p.min(1.0)).collect())
}

/// Every video sample of `path` as the file holds it: its time and whether it is a keyframe.
fn video_samples(path: &Path) -> Result<Vec<(u64, bool)>> {
    let probe = probe(path)?;
    let reader = Reader::open(path, &probe, VideoOut::Coded, AudioOut::Off)?;
    reader.play()?;
    let Some(sink) = reader.video.as_ref() else { return Ok(vec![]) };
    let mut samples = Vec::new();
    while let Some(sample) = reader.pull(sink, None)? {
        let (Some(buffer), Some(at)) = (sample.buffer(), pts_of(&sample)) else { continue };
        samples.push((at, !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT)));
    }
    Ok(samples)
}

/// The presentation times of every video sample, in file order, read without decoding. Tests
/// check a recording's timing with it.
pub fn video_timestamps(source: &Path) -> Result<Vec<u64>> {
    Ok(video_samples(source)?.into_iter().map(|(t, _)| t).collect())
}

/// The keyframes of `path` in nanoseconds; the tests check the pass-through trim with it.
pub fn keyframe_times(source: &Path) -> Result<Vec<u64>> {
    Ok(video_samples(source)?.into_iter().filter(|(_, key)| *key).map(|(t, _)| t).collect())
}

/// The keyframe at or before `start_ns`, where a pass-through cut has to start, since frames
/// after a keyframe only decode from it. A key-unit seek lands on it; `None` when the file has
/// no keyframe before.
fn keyframe_before(path: &Path, probe: &Probe, start_ns: u64) -> Result<Option<u64>> {
    if start_ns == 0 {
        return Ok(Some(0));
    }
    let reader = Reader::open(path, probe, VideoOut::Coded, AudioOut::Off)?;
    reader.seek(path, start_ns, None, false)?;
    reader.play()?;
    if let Some(sink) = reader.video.as_ref() {
        while let Some(sample) = reader.pull(sink, None)? {
            let (Some(buffer), Some(at)) = (sample.buffer(), pts_of(&sample)) else { continue };
            if at > start_ns {
                break;
            }
            if !buffer.flags().contains(gst::BufferFlags::DELTA_UNIT) {
                return Ok(Some(at));
            }
        }
    }
    drop(reader);
    // The seek landed elsewhere (a file without a keyframe index): look through every sample.
    Ok(keyframe_times(path)?.into_iter().filter(|&t| t <= start_ns).max())
}

/// Removes the temporary file `mp4mux` writes the export's samples to before it moves the index
/// to the front, on success, failure and cancel alike.
struct Leftover(PathBuf);

impl Drop for Leftover {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// The pipeline an export writes through.
struct Output {
    pipeline: gst::Pipeline,
    watch: Arc<Watch>,
    video: gst_app::AppSrc,
    audio: Option<gst_app::AppSrc>,
    /// Raw frames queued for the encoder beyond this wait, which paces the decoder.
    video_bytes: u64,
}

impl Drop for Output {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Output {
    fn new(dest: &Path, scratch: &Path, probe: &Probe, plan: &Mp4Plan, with_audio: bool) -> Result<Output> {
        let pipeline = gst::Pipeline::new();
        let video = gst_app::AppSrc::builder()
            .format(gst::Format::Time)
            .is_live(false)
            .do_timestamp(false)
            .block(false)
            .stream_type(gst_app::AppStreamType::Stream)
            .build();
        let mut chain: Vec<gst::Element> = vec![video.clone().upcast()];
        let video_bytes = match plan.reencode {
            None => {
                let codec = if probe.video == Coded::Hevc { VideoCodec::Hevc } else { VideoCodec::H264 };
                chain.push(elements::make(if codec == VideoCodec::Hevc { "h265parse" } else { "h264parse" })?);
                chain.push(elements::capsfilter(&elements::stream_caps(codec))?);
                16 * 1024 * 1024
            }
            Some(re) => {
                // The nominal rate of a variable-rate recording is low; a keyframe every two
                // seconds of 30 frames keeps seeking quick, as on macOS.
                let fps = (probe.info.fps.round() as u32).max(30);
                let target = VideoTarget {
                    width: re.size.width,
                    height: re.size.height,
                    fps,
                    bits_per_second: re.bits_per_second as u64,
                    keyframe_frames: fps * 2,
                };
                let encoder = elements::video_encoder(VideoCodec::H264, target)?;
                let convert = elements::make("videoconvert")?;
                set(&convert, "n-threads", &elements::threads());
                let scale = elements::make("videoscale")?;
                set(&scale, "n-threads", &elements::threads());
                let mut caps = encoder.raw_caps();
                if let Some(caps) = caps.get_mut() {
                    caps.set("width", re.size.width as i32);
                    caps.set("height", re.size.height as i32);
                    caps.set("pixel-aspect-ratio", gst::Fraction::new(1, 1));
                }
                chain.push(convert);
                chain.push(scale);
                chain.push(elements::capsfilter(&caps)?);
                chain.push(encoder.element(&target)?);
                chain.push(elements::make(encoder.parser())?);
                chain.push(elements::capsfilter(&encoder.stream_caps())?);
                let source = probe.info.size;
                (source.width as u64 * source.height as u64 * 4 * 3).max(4 * 1024 * 1024)
            }
        };
        let mux = elements::make("mp4mux")?;
        // The index goes to the front, so a browser starts playing before the whole file has
        // arrived; the samples wait in a hidden file next to the export meanwhile.
        set(&mux, "faststart", "true");
        mux.set_property("faststart-file", location(scratch)?);
        let sink = elements::make("filesink")?;
        sink.set_property("location", location(dest)?);
        set(&sink, "sync", "false");
        set(&sink, "async", "false");
        pipeline.add_many(&chain).map_err(build_error)?;
        pipeline.add_many([&mux, &sink]).map_err(build_error)?;
        gst::Element::link_many(&chain).map_err(build_error)?;
        let last = chain.last().ok_or_else(|| build_error("an empty chain"))?;
        last.link_pads(Some("src"), &mux, Some("video_%u")).map_err(build_error)?;
        mux.link(&sink).map_err(build_error)?;
        let audio = if with_audio {
            let src = gst_app::AppSrc::builder()
                .format(gst::Format::Time)
                .is_live(false)
                .do_timestamp(false)
                .block(false)
                .stream_type(gst_app::AppStreamType::Stream)
                .build();
            let parse = elements::make("aacparse")?;
            pipeline.add_many([src.upcast_ref::<gst::Element>(), &parse]).map_err(build_error)?;
            src.link(&parse).map_err(build_error)?;
            parse.link_pads(Some("src"), &mux, Some("audio_%u")).map_err(build_error)?;
            Some(src)
        } else {
            None
        };
        let watch = Watch::on(&pipeline);
        let output = Output { pipeline, watch, video, audio, video_bytes };
        if output.pipeline.set_state(gst::State::Playing).is_err() {
            return Err(RecordError::Encoder(
                output.watch.error().unwrap_or_else(|| "the export could not start".into()),
            ));
        }
        Ok(output)
    }
}

/// One track of the export on its way from the reader to the output.
struct Lane<'a> {
    sink: &'a gst_app::AppSink,
    src: &'a gst_app::AppSrc,
    video: bool,
    next: Option<gst::Sample>,
    done: bool,
    caps_set: bool,
}

/// A copy of `buffer` (sharing its memory) at `at` for `duration`.
fn retimed(buffer: &gst::Buffer, at: u64, duration: Option<u64>) -> gst::Buffer {
    let mut copy = buffer.copy();
    if let Some(b) = copy.get_mut() {
        b.set_pts(gst::ClockTime::from_nseconds(at));
        b.set_dts(gst::ClockTime::from_nseconds(at));
        if let Some(duration) = duration {
            b.set_duration(gst::ClockTime::from_nseconds(duration.max(1)));
        }
    }
    copy
}

pub fn write_mp4(
    source: &Path,
    dest: &Path,
    plan: &Mp4Plan,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(f64),
) -> Result<TrimRange> {
    let probe = probe(source)?;
    let wanted_start = plan.trim.start_ms * 1_000_000;
    let end = plan.trim.end_ms * 1_000_000;
    // A re-encode cuts exactly, even on a still screen that sent no frame at the start: the frame
    // on screen at the start goes out at time 0. A pass-through cut starts on a keyframe.
    let start = if plan.reencode.is_none() {
        keyframe_before(source, &probe, wanted_start)?.unwrap_or(0)
    } else {
        wanted_start
    };
    let end = end.max(start + 1_000_000);
    let audio_track = plan.audio_track.filter(|&i| i < probe.audio_tracks);
    let scratch = {
        let name = dest.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        dest.with_file_name(format!("{name}.samples"))
    };
    let _scratch = Leftover(scratch.clone());
    // The name is taken first: `filesink` would replace a file that appeared since it was picked.
    std::fs::OpenOptions::new().write(true).create_new(true).open(dest)?;

    let video_out = if plan.reencode.is_some() { VideoOut::Raw } else { VideoOut::Coded };
    let reader = Reader::open(source, &probe, video_out, audio_track.map_or(AudioOut::Off, AudioOut::Coded))?;
    reader.seek(source, start, Some(end), true)?;
    let output = Output::new(dest, &scratch, &probe, plan, audio_track.is_some())?;
    reader.play()?;

    let mut lanes = Vec::new();
    if let Some(sink) = reader.video.as_ref() {
        lanes.push(Lane { sink, src: &output.video, video: true, next: None, done: false, caps_set: false });
    }
    if let (Some(sink), Some(src)) = (reader.audio.as_ref(), output.audio.as_ref()) {
        lanes.push(Lane { sink, src, video: false, next: None, done: false, caps_set: false });
    }
    let length = end - start;
    // The frame on screen at a re-encoded cut's start, decoded before it.
    let mut before: Option<gst::Buffer> = None;
    // The video frame read last and its time in the export, sent when the next one comes (which
    // sets its duration) or, lasting to the cut's end, when the track ends: a still screen sends
    // no frames, so the last one can be seconds before the end.
    let mut held: Option<(gst::Buffer, u64)> = None;
    let push = |src: &gst_app::AppSrc, buffer: gst::Buffer, video: bool| -> Result<()> {
        if video {
            elements::wait_until(&output.watch, Some(cancel), STALL, || {
                src.current_level_bytes() < output.video_bytes
            })?;
        }
        src.push_buffer(buffer).map(|_| ()).map_err(|e| {
            RecordError::Encoder(output.watch.error().unwrap_or_else(|| format!("the export refused a sample ({e:?})")))
        })
    };
    loop {
        if cancel.load(Ordering::SeqCst) {
            return Err(RecordError::Cancelled);
        }
        if let Some(error) = output.watch.error() {
            return Err(RecordError::Encoder(error));
        }
        for lane in lanes.iter_mut().filter(|l| !l.done && l.next.is_none()) {
            match reader.pull(lane.sink, Some(cancel))? {
                Some(sample) => lane.next = Some(sample),
                None => {
                    lane.done = true;
                    if lane.video {
                        if let Some(first) = before.take() {
                            held = Some((first, 0));
                        }
                        if let Some((last, at)) = held.take() {
                            let lasts = last.duration().map(|d| d.nseconds()).unwrap_or(0);
                            let duration = if length > at { length - at } else { lasts };
                            push(lane.src, retimed(&last, at, Some(duration)), true)?;
                        }
                    }
                    let _ = lane.src.end_of_stream();
                }
            }
        }
        let Some(index) = lanes
            .iter()
            .enumerate()
            .filter_map(|(i, l)| Some((i, pts_of(l.next.as_ref()?).unwrap_or(0))))
            .min_by_key(|&(_, at)| at)
            .map(|(i, _)| i)
        else {
            break;
        };
        let lane = &mut lanes[index];
        let Some(sample) = lane.next.take() else { continue };
        if !lane.caps_set {
            lane.src.set_caps(sample.caps_owned().as_ref());
            lane.caps_set = true;
        }
        let (Some(buffer), Some(at)) = (sample.buffer_owned(), pts_of(&sample)) else { continue };
        if at >= end {
            continue;
        }
        if !lane.video {
            if at >= start {
                let duration = buffer.duration().map(|d| d.nseconds());
                push(lane.src, retimed(&buffer, at - start, duration), false)?;
            }
            continue;
        }
        if at < start {
            // A pass-through cut starts on a keyframe and needs nothing before it; a re-encode
            // keeps the frame on screen at the start.
            if plan.reencode.is_some() {
                before = Some(buffer);
            }
            continue;
        }
        if let Some(first) = before.take() {
            if at > start {
                held = Some((first, 0));
            }
        }
        progress((at - start) as f64 / length as f64);
        if let Some((previous, from)) = held.replace((buffer, at - start)) {
            push(lane.src, retimed(&previous, from, Some(at - start - from)), true)?;
        }
    }
    output.watch.wait_eos(EXPORT_FINISH).map_err(RecordError::Encoder)?;
    drop(output);
    drop(reader);
    Ok(TrimRange { start_ms: start / 1_000_000, end_ms: plan.trim.end_ms })
}
