//! The take's MP4 writer on Linux: one GStreamer pipeline that encodes the stream's BGRA frames
//! to H.264 or HEVC and the session's 48 kHz stereo float to one AAC track per entry of
//! [`WriterSpec::audio`], muxed by `mp4mux`.
//!
//! ```text
//! appsrc (BGRA) → videoconvert → I420 or NV12 → encoder → h264parse/h265parse → queue ─┐
//! appsrc (F32, 48 kHz stereo) → audioconvert → taginject → AAC → aacparse ─────────────┼→ mp4mux → filesink
//!                                                                 (one branch per track)┘
//! ```
//!
//! Crash safety: on GStreamer 1.20, `mp4mux`'s fragmented modes leave files that ffmpeg (and so
//! most players outside GStreamer) misreads, or that GStreamer itself reads only up to the first
//! fragment after a crash. Its robust mode does what the macOS writer's movie fragments do: room
//! for the index is reserved at the front, and the index is rewritten there every
//! `fragment_interval_ms` of the take, so a file cut short by a crash or a full disk plays up to
//! its last update, and a finished file is a plain MP4 with its index first. The reserved room
//! bounds a take's length (`MAX_TAKE_HOURS`); the writer reports a failure before it runs out,
//! which finishes the take while its index still fits. The room the index did not use is dropped
//! from a finished take by `mp4::finish` when it is worth a copy.
//!
//! Every buffer carries its media time (video from the session's `media_ns`, audio from the
//! sample position), the sources are not live, and nothing is stamped on arrival, so the muxer
//! keeps the tracks in step by those times.
//!
//! A full encoder queue makes `append_video` refuse a frame. An export hands it over again a
//! moment later, and its offline writer waits for the encoder before it queues more sound.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use glimpsfy_capture::{Frame, PhysicalSize, PixelFormat, VideoBuffer, VideoFrame};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;

use super::elements::{self, set, VideoTarget, Watch};
use crate::audio::{ns_at, CHANNELS, RATE};
use crate::session::{TakeWriter, WriterSpec};
use crate::{RecordError, Result, TrackKind, VideoCodec};

/// Frames waiting for the encoder beyond this are dropped instead of queued, so a busy encoder
/// never holds up the capture thread or grows the memory.
const QUEUED_FRAMES: u64 = 4;
const QUEUED_FRAME_BYTES: u64 = 160 * 1024 * 1024;
/// A still screen sends no frames (the portal's stream does not), while the muxer needs every
/// track to move on before it writes; the last frame is sent again once a second of sound has
/// gone by without one, a quarter second late so a frame that is merely late is not overtaken.
const HEARTBEAT_NS: u64 = 1_000_000_000;
const HEARTBEAT_LAG_NS: u64 = 250_000_000;
/// A keyframe at least every second while frames come, so a pass-through trim starts within a
/// second of where it was asked to.
const KEYFRAME_NS: u64 = 1_000_000_000;
/// Sound the muxer has not taken yet beyond this means it stopped: about 90 seconds of a track.
const AUDIO_BACKLOG_BYTES: u64 = 64 * 1024 * 1024;
/// Coded frames waiting for the muxer: enough for its turns between the tracks, and for a disk
/// that stalls a moment.
const CODED_QUEUE_NS: u64 = 3_000_000_000;
/// The take the index's room is sized for, by `index_bytes_per_second`. That reckons with about
/// twice what the index really takes (1.2 kB a second measured at 60 frames per second with three
/// audio tracks, against 2.7 kB reserved), and `mp4mux` counts the room left from what it really
/// takes, so a take runs for eight hours or more before it is stopped.
const MAX_TAKE_HOURS: u64 = 4;
/// The take is finished when the reserved index has room for less than this much more.
const INDEX_MARGIN_NS: u64 = 120_000_000_000;
/// `mp4mux` reckons the room left from the index used so far over the time muxed so far. At the
/// first index update that time can be a millisecond (the frame the take starts with and the
/// next), and the reckoning came out under the margin: about one take in twenty-five stopped at
/// once. The room is sized for hours, so the reckoning is read only after this much of a take.
const INDEX_ESTIMATE_AFTER_NS: u64 = 60_000_000_000;
/// How long finishing may take: the encoders drain a few frames, and the muxer writes its index.
const FINISH_TIMEOUT: Duration = Duration::from_secs(60);
/// An offline writer (an export) waits past this much sound queued for a track (about ten
/// seconds), so a fast reader never makes the writer give up on its sound.
const OFFLINE_AUDIO_BYTES: u64 = 4 * 1024 * 1024;
/// How long an offline writer waits for an encoder that takes nothing before it gives up.
const OFFLINE_STALL: Duration = Duration::from_secs(30);

/// The title a track carries, which `reader::inspect` reads back to tell the tracks apart; the
/// macOS writer gives the same ones.
pub(super) fn track_title(kind: TrackKind) -> &'static str {
    match kind {
        TrackKind::Video => "Video",
        TrackKind::MixedAudio => "Mixed audio",
        TrackKind::Microphone => "Microphone",
        TrackKind::SystemAudio => "System audio",
    }
}

/// Bits per second of the recording, for screen content at the quality the design asks for
/// ("crisp text"), as on macOS; HEVC needs about two thirds of H.264's rate.
fn bit_rate(spec: &WriterSpec, codec: VideoCodec) -> u64 {
    let bpp = match codec {
        VideoCodec::H264 => 0.08,
        VideoCodec::Hevc => 0.05,
    };
    let bits = spec.size.width as f64 * spec.size.height as f64 * spec.fps.max(1) as f64 * bpp;
    bits.clamp(2_000_000.0, 60_000_000.0) as u64
}

/// The index bytes one second of the take needs, with room to spare: per video frame its size,
/// its time and its share of the chunk and keyframe tables; per AAC packet (1024 samples) its
/// size and the same share.
fn index_bytes_per_second(spec: &WriterSpec) -> u64 {
    let video = spec.fps.max(1) as u64 * 16 + 64;
    let audio = spec.audio.len() as u64 * ((RATE as u64).div_ceil(1024) * 4 + 64);
    (video + audio) * 3 / 2
}

pub fn create(spec: &WriterSpec) -> Result<Box<dyn TakeWriter>> {
    Ok(Box::new(GstWriter::new(spec)?))
}

/// The pixels of a captured frame, handed to GStreamer without a copy.
struct Pixels(Arc<Frame>);

impl AsRef<[u8]> for Pixels {
    fn as_ref(&self) -> &[u8] {
        &self.0.data
    }
}

struct AudioLane {
    src: gst_app::AppSrc,
    /// The frame position the track has reached; the next buffer must start here.
    next: u64,
    /// The encoder's lead in frames (see `push_audio`).
    lead: u64,
}

pub(crate) struct GstWriter {
    pipeline: gst::Pipeline,
    watch: Arc<Watch>,
    mux: gst::Element,
    video: gst_app::AppSrc,
    audio: Vec<AudioLane>,
    path: PathBuf,
    size: PhysicalSize,
    fps: u64,
    frame_ns: u64,
    queue_bytes: u64,
    /// The last frame sent and its time, sent again on a still screen and at the end.
    last: Option<(gst::Buffer, u64)>,
    last_key: Option<u64>,
    frames_since_key: u64,
    frames: u64,
    fragment_ns: u64,
    update_period_ns: u64,
    /// The writer's own reason to stop, beside the bus's errors.
    stopped: Option<String>,
    finished: bool,
    /// The codec of the encoder found, which is H.264 when HEVC was asked for and none is here.
    codec: VideoCodec,
    /// An export: sound waits for the encoder instead of piling up.
    offline: bool,
}

impl GstWriter {
    fn new(spec: &WriterSpec) -> Result<GstWriter> {
        elements::init()?;
        let (width, height) = (spec.size.width, spec.size.height);
        if width < 2 || height < 2 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return Err(RecordError::Encoder(format!("the encoder cannot write {width}×{height} video")));
        }
        let path_text = spec
            .path
            .to_str()
            .ok_or_else(|| RecordError::InvalidConfig(format!("{} is not a UTF-8 path", spec.path.display())))?
            .to_owned();
        let fps = spec.fps.max(1);
        let mut target = VideoTarget { width, height, fps, bits_per_second: 0, keyframe_frames: fps };
        let rate = |codec| spec.bits_per_second.map_or_else(|| bit_rate(spec, codec), |bits| bits.max(1) as u64);
        target.bits_per_second = rate(spec.codec);
        let mut encoder = elements::video_encoder(spec.codec, target)?;
        if encoder.codec != spec.codec {
            // HEVC fell back to H.264, which needs its own rate.
            target.bits_per_second = rate(encoder.codec);
            encoder = elements::video_encoder(encoder.codec, target)?;
        }
        let aac = if spec.audio.is_empty() { None } else { Some(elements::aac_encoder()?) };

        // The name is taken before anything is written: `filesink` would replace a file that
        // appeared since the session checked.
        std::fs::OpenOptions::new().write(true).create_new(true).open(&spec.path)?;
        let built = GstWriter::build(spec, &path_text, encoder, &target, aac);
        if built.is_err() && std::fs::metadata(&spec.path).is_ok_and(|m| m.len() == 0) {
            let _ = std::fs::remove_file(&spec.path);
        }
        built
    }

    fn build(
        spec: &WriterSpec,
        path: &str,
        encoder: elements::VideoEncoder,
        target: &VideoTarget,
        aac: Option<elements::AacEncoder>,
    ) -> Result<GstWriter> {
        let codec = encoder.codec;
        let fps = target.fps as u64;
        let frame_bytes = target.width as u64 * target.height as u64 * 4;
        let queue_bytes = (frame_bytes * QUEUED_FRAMES).min(QUEUED_FRAME_BYTES).max(frame_bytes * 2);
        let pipeline = gst::Pipeline::new();
        let link =
            |e: gst::glib::BoolError| RecordError::Encoder(format!("the recording pipeline could not be built: {e}"));
        let pad_link =
            |e: gst::PadLinkError| RecordError::Encoder(format!("the recording pipeline could not be linked: {e:?}"));

        let video = gst_app::AppSrc::builder()
            .caps(&elements::bgra_caps(target.width, target.height, target.fps))
            .format(gst::Format::Time)
            .is_live(false)
            .do_timestamp(false)
            .block(false)
            .max_bytes(queue_bytes)
            .stream_type(gst_app::AppStreamType::Stream)
            .build();
        let convert = elements::make("videoconvert")?;
        set(&convert, "n-threads", &elements::threads());
        let raw = elements::capsfilter(&encoder.raw_caps())?;
        let encode = encoder.element(target)?;
        let parse = elements::make(encoder.parser())?;
        let stream = elements::capsfilter(&encoder.stream_caps())?;
        // `mp4mux` takes each track in turns of a quarter second and holds the other track's
        // pad meanwhile. An encoder without a delay of its own (`openh264enc`) would wait there,
        // and the frames behind it would be dropped; the coded frames wait in this queue instead.
        let coded = elements::make("queue")?;
        set(&coded, "max-size-buffers", "0");
        set(&coded, "max-size-bytes", "0");
        set(&coded, "max-size-time", &CODED_QUEUE_NS.to_string());
        let mux = elements::make("mp4mux")?;
        let reserved_ns = MAX_TAKE_HOURS * 3_600_000_000_000;
        let tracks = 1 + spec.audio.len() as u64;
        let per_track = index_bytes_per_second(spec).div_ceil(tracks);
        let fragment_ns = spec.fragment_interval_ms.max(500) * 1_000_000;
        if !(set(&mux, "reserved-max-duration", &reserved_ns.to_string())
            && set(&mux, "reserved-bytes-per-sec", &per_track.to_string())
            && set(&mux, "reserved-moov-update-period", &fragment_ns.to_string()))
        {
            return Err(RecordError::Encoder("this GStreamer's mp4mux cannot reserve room for its index".into()));
        }
        let sink = elements::make("filesink")?;
        sink.set_property("location", path);
        set(&sink, "sync", "false");
        set(&sink, "async", "false");

        let chain = [video.upcast_ref::<gst::Element>(), &convert, &raw, &encode, &parse, &stream, &coded];
        pipeline.add_many(chain).map_err(link)?;
        pipeline.add_many([&mux, &sink]).map_err(link)?;
        gst::Element::link_many(chain).map_err(link)?;
        let video_pad =
            mux.request_pad_simple("video_%u").ok_or_else(|| link(gst::glib::bool_error!("no video pad")))?;
        // The MPEG clock fits every frame rate without rounding drift, as on macOS.
        set(&video_pad, "trak-timescale", "90000");
        coded
            .static_pad("src")
            .ok_or_else(|| link(gst::glib::bool_error!("no stream pad")))?
            .link(&video_pad)
            .map_err(pad_link)?;
        mux.link(&sink).map_err(link)?;

        let mut audio = Vec::new();
        for &kind in &spec.audio {
            let Some(aac) = aac else { break };
            let src = gst_app::AppSrc::builder()
                .caps(&elements::pcm_caps())
                .format(gst::Format::Time)
                .is_live(false)
                .do_timestamp(false)
                .block(false)
                .max_bytes(AUDIO_BACKLOG_BYTES)
                .stream_type(gst_app::AppStreamType::Stream)
                .build();
            let convert = elements::make("audioconvert")?;
            let title = elements::make("taginject")?;
            title.set_property("tags", format!("title=\"{}\"", track_title(kind)));
            let bits = if kind == TrackKind::MixedAudio { 192_000 } else { 160_000 };
            let encode = elements::aac_element(aac.factory, bits)?;
            let parse = elements::make("aacparse")?;
            let branch = [src.upcast_ref::<gst::Element>(), &convert, &title, &encode, &parse];
            pipeline.add_many(branch).map_err(link)?;
            gst::Element::link_many(branch).map_err(link)?;
            let pad = mux.request_pad_simple("audio_%u").ok_or_else(|| link(gst::glib::bool_error!("no audio pad")))?;
            parse
                .static_pad("src")
                .ok_or_else(|| link(gst::glib::bool_error!("no audio pad")))?
                .link(&pad)
                .map_err(pad_link)?;
            audio.push(AudioLane { src, next: 0, lead: aac.lead });
        }

        let watch = Watch::on(&pipeline);
        if pipeline.set_state(gst::State::Playing).is_err() {
            let _ = pipeline.set_state(gst::State::Null);
            return Err(RecordError::Encoder(
                watch.error().unwrap_or_else(|| "the recording pipeline could not start".into()),
            ));
        }
        Ok(GstWriter {
            pipeline,
            watch,
            mux,
            video,
            audio,
            path: spec.path.clone(),
            size: spec.size,
            fps,
            frame_ns: 1_000_000_000 / fps,
            queue_bytes,
            last: None,
            last_key: None,
            frames_since_key: 0,
            frames: 0,
            fragment_ns,
            update_period_ns: fragment_ns,
            stopped: None,
            finished: false,
            codec,
            offline: spec.offline,
        })
    }

    fn video_queue_full(&self, more: u64) -> bool {
        self.video.current_level_bytes() + more > self.queue_bytes
    }

    fn encoder_error(&self, what: &str, flow: gst::FlowError) -> RecordError {
        RecordError::Encoder(self.failure().unwrap_or_else(|| format!("{what} was refused ({flow:?})")))
    }

    /// Hands a frame to the encoder at `at`. A captured frame (`real`) asks for a keyframe when
    /// the last one is a second old; a repeated frame does not, so a still screen stays cheap.
    fn push_video(&mut self, buffer: gst::Buffer, at: u64, real: bool) -> Result<()> {
        let mut keyframe = false;
        if real && self.last_key.is_none_or(|k| at >= k + KEYFRAME_NS) {
            let event = gst_video::DownstreamForceKeyUnitEvent::builder()
                .timestamp(gst::ClockTime::from_nseconds(at))
                .stream_time(gst::ClockTime::from_nseconds(at))
                .running_time(gst::ClockTime::from_nseconds(at))
                .all_headers(true)
                .build();
            keyframe = self.video.send_event(event);
        }
        self.video.push_buffer(buffer.clone()).map_err(|e| self.encoder_error("a frame", e))?;
        // The encoder also makes a keyframe every `fps` frames on its own.
        self.frames_since_key += 1;
        if keyframe || self.frames_since_key >= self.fps || self.last_key.is_none() {
            self.last_key = Some(at);
            self.frames_since_key = 0;
        }
        self.last = Some((buffer, at));
        Ok(())
    }

    /// The last frame again at `at`, lasting `duration`.
    fn repeat_last(&mut self, at: u64, duration: u64) -> Result<()> {
        let Some((buffer, _)) = &self.last else { return Ok(()) };
        let mut copy = buffer.copy();
        if let Some(b) = copy.get_mut() {
            b.set_pts(gst::ClockTime::from_nseconds(at));
            b.set_duration(gst::ClockTime::from_nseconds(duration));
        }
        self.push_video(copy, at, false)
    }

    /// Keeps the video track moving while the sound goes on over a still screen.
    fn heartbeat(&mut self, audio_end: u64) -> Result<()> {
        let Some((_, at)) = &self.last else { return Ok(()) };
        let at = *at;
        if audio_end < at + HEARTBEAT_NS + HEARTBEAT_LAG_NS || self.video_queue_full(0) {
            return Ok(());
        }
        self.repeat_last(at + HEARTBEAT_NS, self.frame_ns)
    }

    /// Sends the frames from `position` on to the encoder. The AAC encoder puts `lead` frames of
    /// its own in front of the sound (its priming), which every decoder plays first and which
    /// `mp4mux` does not mark to be skipped. So the take's first `lead` frames are left out and
    /// the rest is stamped `lead` frames early: the priming then fills the take's first few
    /// milliseconds, and every frame after it plays at its own time, in every player and in a
    /// file a crash cut short.
    fn push_audio(&mut self, track: usize, position: u64, samples: &[f32]) -> Result<()> {
        let frames = (samples.len() / CHANNELS) as u64;
        if frames == 0 {
            return Ok(());
        }
        let lane = &mut self.audio[track];
        lane.next = position + frames;
        let skip = lane.lead.saturating_sub(position).min(frames);
        if skip == frames {
            return Ok(());
        }
        let at = position + skip - lane.lead;
        let mut bytes = Vec::with_capacity((frames - skip) as usize * CHANNELS * 4);
        for s in &samples[skip as usize * CHANNELS..] {
            bytes.extend_from_slice(&s.to_le_bytes());
        }
        let mut buffer = gst::Buffer::from_mut_slice(bytes);
        if let Some(b) = buffer.get_mut() {
            let pts = ns_at(at);
            b.set_pts(gst::ClockTime::from_nseconds(pts));
            b.set_duration(gst::ClockTime::from_nseconds(ns_at(at + frames - skip) - pts));
        }
        let pushed = lane.src.push_buffer(buffer);
        pushed.map(|_| ()).map_err(|e| self.encoder_error("a sound buffer", e))
    }

    /// Checks the room left in the reserved index, and spaces the index updates out as the take
    /// grows: each update rewrites the whole index, so updates every two seconds of a long take
    /// would write more index than video.
    fn check_index(&mut self, media_ns: u64) {
        if media_ns >= INDEX_ESTIMATE_AFTER_NS && self.mux.find_property("reserved-duration-remaining").is_some() {
            let remaining = self.mux.property_value("reserved-duration-remaining").get::<u64>().unwrap_or(u64::MAX);
            if remaining < INDEX_MARGIN_NS && self.stopped.is_none() {
                let minutes = media_ns / 60_000_000_000;
                self.stopped = Some(format!(
                    "the recording reached {} hours {} minutes, the longest its file has room for",
                    minutes / 60,
                    minutes % 60
                ));
            }
        }
        let period = (media_ns / 200).max(self.fragment_ns);
        if period > self.update_period_ns + self.update_period_ns / 4 {
            set(&self.mux, "reserved-moov-update-period", &period.to_string());
            self.update_period_ns = period;
        }
    }

    fn stop(&self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl TakeWriter for GstWriter {
    fn append_video(&mut self, frame: &VideoFrame, media_ns: u64) -> Result<bool> {
        if self.finished {
            return Ok(false);
        }
        if let Some(reason) = self.failure() {
            return Err(RecordError::Encoder(reason));
        }
        let VideoBuffer::Cpu(cpu) = &frame.buffer else {
            return Err(RecordError::Encoder("the frame is not in memory".into()));
        };
        if cpu.size != self.size || self.last.as_ref().is_some_and(|(_, at)| media_ns <= *at) {
            // A frame of another size or out of order cannot go into this track.
            return Ok(false);
        }
        let row = self.size.width as usize * 4;
        if self.video_queue_full((row * self.size.height as usize) as u64) {
            return Ok(false);
        }
        let mut buffer = match cpu.format {
            PixelFormat::Bgra8 => gst::Buffer::from_slice(Pixels(cpu.clone())),
            PixelFormat::Rgba8 => {
                let mut bgra = vec![0u8; row * self.size.height as usize];
                for (y, out) in bgra.chunks_exact_mut(row).enumerate() {
                    let src = &cpu.data[y * cpu.bytes_per_row..y * cpu.bytes_per_row + row];
                    for (s, d) in src.chunks_exact(4).zip(out.chunks_exact_mut(4)) {
                        d.copy_from_slice(&[s[2], s[1], s[0], s[3]]);
                    }
                }
                gst::Buffer::from_mut_slice(bgra)
            }
        };
        if let Some(b) = buffer.get_mut() {
            b.set_pts(gst::ClockTime::from_nseconds(media_ns));
            b.set_duration(gst::ClockTime::from_nseconds(self.frame_ns));
            if cpu.format == PixelFormat::Bgra8 && cpu.bytes_per_row != row {
                gst_video::VideoMeta::add_full(
                    b,
                    gst_video::VideoFrameFlags::empty(),
                    gst_video::VideoFormat::Bgra,
                    self.size.width,
                    self.size.height,
                    &[0],
                    &[cpu.bytes_per_row as i32],
                )
                .map_err(|e| RecordError::Encoder(e.to_string()))?;
            }
        }
        self.push_video(buffer, media_ns, true)?;
        self.frames += 1;
        if self.frames.is_multiple_of(30) {
            self.check_index(media_ns);
        }
        Ok(true)
    }

    fn append_audio(&mut self, track: usize, position: u64, samples: &[f32]) -> Result<()> {
        if self.finished || track >= self.audio.len() || samples.len() < CHANNELS {
            return Ok(());
        }
        if let Some(reason) = self.failure() {
            return Err(RecordError::Encoder(reason));
        }
        if self.offline {
            let deadline = std::time::Instant::now() + OFFLINE_STALL;
            while self.audio[track].src.current_level_bytes() > OFFLINE_AUDIO_BYTES {
                if let Some(reason) = self.failure() {
                    return Err(RecordError::Encoder(reason));
                }
                if std::time::Instant::now() >= deadline {
                    return Err(RecordError::Encoder("the audio encoder stopped taking sound".into()));
                }
                // The muxer takes sound only as far as it has pictures, so on a still screen
                // the last one has to go on for the sound to drain.
                self.heartbeat(ns_at(self.audio[track].next))?;
                std::thread::sleep(Duration::from_millis(2));
            }
        }
        let usable = samples.len() / CHANNELS * CHANNELS;
        let mut body = &samples[..usable];
        let mut at = position;
        let next = self.audio[track].next;
        // An AAC track has no gaps: its packets follow each other. Sound that overlaps what the
        // track holds is cut, and a gap is filled with silence, so the track never drifts from
        // its times; the session already places the sound this way.
        if at < next {
            let skip = ((next - at) as usize).min(body.len() / CHANNELS);
            body = &body[skip * CHANNELS..];
            at += skip as u64;
            if body.is_empty() {
                return Ok(());
            }
        }
        while at > self.audio[track].next {
            let from = self.audio[track].next;
            let gap = (at - from).min(RATE as u64);
            self.push_audio(track, from, &vec![0.0; gap as usize * CHANNELS])?;
        }
        self.push_audio(track, at, body)?;
        if self.audio[track].src.current_level_bytes() >= AUDIO_BACKLOG_BYTES && self.stopped.is_none() {
            self.stopped = Some("the encoder stopped taking sound".into());
        }
        self.heartbeat(ns_at(self.audio[track].next))
    }

    fn failure(&self) -> Option<String> {
        self.watch.error().or_else(|| self.stopped.clone())
    }

    fn finish(&mut self, end_ns: u64) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        if let Some(error) = self.watch.error() {
            self.stop();
            return Err(RecordError::Encoder(error));
        }
        // A still screen sends no frames, so the video would end with the last change while the
        // sound and the take go on: the last frame goes in once more, one frame before the end,
        // so the picture lasts as long as the take.
        if let Some((_, at)) = &self.last {
            let again = end_ns.saturating_sub(self.frame_ns);
            if again > *at {
                // A full queue drains in a moment; the frame is worth the wait.
                let deadline = std::time::Instant::now() + Duration::from_secs(1);
                while self.video_queue_full(0) && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                }
                let _ = self.repeat_last(again, end_ns - again);
            }
        }
        let _ = self.video.end_of_stream();
        for lane in &self.audio {
            let _ = lane.src.end_of_stream();
        }
        let outcome = self.watch.wait_eos(FINISH_TIMEOUT);
        self.stop();
        outcome.map_err(RecordError::Encoder)?;
        // The file plays as it is; what `finish` cannot do only costs room or the alternate marks.
        let _ = super::mp4::finish(&self.path);
        Ok(())
    }

    fn codec(&self) -> Option<VideoCodec> {
        Some(self.codec)
    }
}

impl Drop for GstWriter {
    fn drop(&mut self) {
        // Without `finish`, what the last index update covers stays readable.
        self.stop();
    }
}
