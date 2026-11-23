//! The take's writer on Windows: Media Foundation's sink writer makes an MP4 with H.264 (or HEVC)
//! video and one AAC track per audio input. All Media Foundation work happens on one thread the
//! writer owns, inside the multithreaded apartment, so the capture callbacks that feed it never
//! wait for the encoder and nothing is assumed about their apartment.
//!
//! The file is a plain MP4, not a fragmented one. Media Foundation documents its fragmented MP4
//! sink only as a bare reference, its own MPEG-4 source reads movie fragments without the
//! random-access index, and a fragmented file shows no length or seek bar in some Windows
//! players, while the trim window and the export need both. The price is that a take cut off by
//! a crash or a power cut has no index and does not play; `finish` (and dropping the writer)
//! always finalises the file, which covers every ordinary stop, a full disk included.
//!
//! The encoder is chosen in this order: the wanted codec (HEVC only when one is registered),
//! H.264 when the picture fits its limits, each first with hardware encoders allowed, then
//! without them, then (with more than one audio track) with the mix as the only audio track,
//! since older MP4 sinks take one audio stream only. A hardware encoder that fails on its first
//! frame is replaced by the software one, and that frame is written again.
//!
//! A frame is dropped (`append_video` returns `false`) when two are already waiting for the
//! writer thread or the encoder holds more than its share; throttling in the sink writer is
//! turned off, so it never blocks the thread that converts and hands over the frames. An
//! offline writer (an export) refuses a frame the same way, and the export hands it over again
//! a moment later; it drops no audio but waits for the writer thread instead.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use glimpsfy_capture::{Frame, PixelFormat, VideoBuffer, VideoFrame};
use windows::core::{Interface, GUID};
use windows::Win32::Media::MediaFoundation::{
    eAVEncH264VProfile_High, eAVEncH265VProfile_Main_420_8, CODECAPI_AVEncCommonMeanBitRate,
    CODECAPI_AVEncMPVDefaultBPictureCount, CODECAPI_AVEncMPVGOPSize, CODECAPI_AVEncVideoForceKeyFrame, ICodecAPI,
    IMFAttributes, IMFByteStream, IMFMediaBuffer, IMFMediaType, IMFSinkWriter, MFAudioFormat_AAC, MFAudioFormat_PCM,
    MFMediaType_Audio, MFMediaType_Video, MFNominalRange_16_235, MFTranscodeContainerType_MPEG4, MFVideoFormat_H264,
    MFVideoFormat_HEVC, MFVideoFormat_NV12, MFVideoInterlace_Progressive, MFVideoPrimaries_BT709, MFVideoTransFunc_709,
    MFVideoTransferMatrix_BT709, MF_ACCESSMODE_READWRITE, MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION,
    MF_MT_AAC_PAYLOAD_TYPE, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
    MF_MT_AUDIO_BITS_PER_SAMPLE, MF_MT_AUDIO_BLOCK_ALIGNMENT, MF_MT_AUDIO_NUM_CHANNELS, MF_MT_AUDIO_SAMPLES_PER_SECOND,
    MF_MT_AVG_BITRATE, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE,
    MF_MT_MAJOR_TYPE, MF_MT_MPEG2_LEVEL, MF_MT_MPEG2_PROFILE, MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SUBTYPE,
    MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE, MF_MT_VIDEO_PRIMARIES, MF_MT_VIDEO_PROFILE, MF_MT_YUV_MATRIX,
    MF_OPENMODE_FAIL_IF_EXIST, MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, MF_SINK_WRITER_DISABLE_THROTTLING,
    MF_SINK_WRITER_STATISTICS, MF_TRANSCODE_CONTAINERTYPE,
};
use windows::Win32::System::Variant::{VARIANT, VT_UI4};

use super::logic::{self, Pixels};
use super::mf::{self, Apartment, Api, Locked, Platform};
use crate::audio;
use crate::session::{codec_for, TakeWriter, WriterSpec};
use crate::{RecordError, Result, TrackKind, VideoCodec};

/// What the writer says when the system has no encoder at all for the video.
pub(crate) const NO_ENCODER: &str =
    "this Windows has no H.264 encoder; Windows N editions need the Media Feature Pack to record";

/// How long `create` waits for the encoder to start, and `finish` for the file to be closed.
const START_LIMIT: Duration = Duration::from_secs(15);
const FINISH_LIMIT: Duration = Duration::from_secs(60);

/// Audio waiting for the writer thread is capped at ten seconds per track; the oldest goes first.
const MAX_QUEUED_AUDIO_FRAMES: u64 = audio::RATE as u64 * 10;
/// An offline writer (an export) drops no audio: past a second queued per track it waits for the
/// writer thread, for at most `OFFLINE_STALL` without progress.
const OFFLINE_QUEUED_AUDIO_FRAMES: u64 = audio::RATE as u64;
const OFFLINE_STALL: Duration = Duration::from_secs(30);

/// What a call on a writer that has closed its file gets.
pub(super) fn closed() -> RecordError {
    RecordError::Encoder("the MP4 writer has closed its file already".into())
}

/// The reason inside an error, without the "encoding failed" its text starts with, so a reason
/// that is passed on is not prefixed twice.
pub(super) fn reason(error: RecordError) -> String {
    match error {
        RecordError::Encoder(text) => text,
        other => other.to_string(),
    }
}

pub fn create(spec: &WriterSpec) -> Result<Box<dyn TakeWriter>> {
    Ok(Box::new(WinWriter::start(spec)?))
}

/// A unit of work for the writer thread, in the order the session handed it over.
enum Job {
    Video { frame: Arc<Frame>, at: i64 },
    Audio { track: usize, position: u64, samples: Vec<f32> },
    Finish { end: i64, reply: mpsc::SyncSender<Result<()>> },
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    /// Video frames among `jobs`.
    video: usize,
    /// Audio frames among `jobs`, per track.
    audio: Vec<u64>,
    /// No more jobs come; the thread finalises the file and ends.
    closed: bool,
}

/// What the session's thread and the writer thread share.
struct Shared {
    queue: Mutex<Queue>,
    wake: Condvar,
    failure: Mutex<Option<String>>,
    /// Video frames handed to the sink writer that it has not passed on to the file yet.
    backlog: AtomicU64,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Shared {
    fn fail(&self, reason: String) {
        let mut failure = lock(&self.failure);
        if failure.is_none() {
            *failure = Some(reason);
        }
    }

    fn failure(&self) -> Option<String> {
        lock(&self.failure).clone()
    }

    fn close(&self) {
        lock(&self.queue).closed = true;
        self.wake.notify_all();
    }
}

struct WinWriter {
    shared: Arc<Shared>,
    tracks: usize,
    /// The setup the file opened with, which may have less than the spec asked for.
    setup: Setup,
    backlog_limit: u64,
    last_video: Option<i64>,
    finished: bool,
    /// An export: audio waits instead of being dropped, and dropping the writer waits until its
    /// thread has closed the file.
    offline: bool,
    worker: Option<JoinHandle<()>>,
}

impl WinWriter {
    fn start(spec: &WriterSpec) -> Result<WinWriter> {
        if spec.size.width < 2
            || spec.size.height < 2
            || !spec.size.width.is_multiple_of(2)
            || !spec.size.height.is_multiple_of(2)
        {
            return Err(RecordError::InvalidConfig(format!(
                "a {}×{} picture cannot be recorded; both sides must be even",
                spec.size.width, spec.size.height
            )));
        }
        // A missing Media Foundation is reported here, before a thread is started for nothing.
        mf::api()?;
        let shared = Arc::new(Shared {
            queue: Mutex::new(Queue { audio: vec![0; spec.audio.len()], ..Queue::default() }),
            wake: Condvar::new(),
            failure: Mutex::new(None),
            backlog: AtomicU64::new(0),
        });
        let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<Setup>>(1);
        let worker = {
            let shared = shared.clone();
            let spec = spec.clone();
            std::thread::Builder::new()
                .name("glimpsfy-mp4-writer".into())
                .spawn(move || run(&shared, &spec, ready_tx))
                .map_err(|e| RecordError::Encoder(format!("could not start the writer thread: {e}")))?
        };
        let frame_bytes = logic::nv12_len(spec.size.width as usize, spec.size.height as usize);
        match ready_rx.recv_timeout(START_LIMIT) {
            Ok(Ok(setup)) => Ok(WinWriter {
                shared,
                tracks: spec.audio.len(),
                setup,
                backlog_limit: logic::backlog_limit(spec.fps, frame_bytes),
                last_video: None,
                finished: false,
                offline: spec.offline,
                worker: Some(worker),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                // The thread sees the closed queue when it is ready at last, and removes its file.
                shared.close();
                Err(RecordError::Encoder(format!(
                    "the video encoder did not start within {} seconds",
                    START_LIMIT.as_secs()
                )))
            }
        }
    }
}

impl TakeWriter for WinWriter {
    fn append_video(&mut self, frame: &VideoFrame, media_ns: u64) -> Result<bool> {
        if let Some(reason) = self.shared.failure() {
            return Err(RecordError::Encoder(reason));
        }
        let VideoBuffer::Cpu(pixels) = &frame.buffer else {
            return Err(RecordError::Encoder("the Windows writer takes frames in memory only".into()));
        };
        let at = logic::hns(media_ns);
        if self.last_video.is_some_and(|last| at <= last) {
            return Ok(false);
        }
        let mut queue = lock(&self.shared.queue);
        if queue.closed {
            return Err(closed());
        }
        if queue.video >= 2 || self.shared.backlog.load(Ordering::SeqCst) >= self.backlog_limit {
            return Ok(false);
        }
        queue.jobs.push_back(Job::Video { frame: pixels.clone(), at });
        queue.video += 1;
        drop(queue);
        self.shared.wake.notify_one();
        self.last_video = Some(at);
        Ok(true)
    }

    fn append_audio(&mut self, track: usize, position: u64, samples: &[f32]) -> Result<()> {
        if track >= self.tracks || samples.len() < audio::CHANNELS {
            return Ok(());
        }
        if let Some(reason) = self.shared.failure() {
            return Err(RecordError::Encoder(reason));
        }
        let frames = (samples.len() / audio::CHANNELS) as u64;
        if self.offline {
            let deadline = Instant::now() + OFFLINE_STALL;
            while lock(&self.shared.queue).audio[track] > OFFLINE_QUEUED_AUDIO_FRAMES {
                if let Some(reason) = self.shared.failure() {
                    return Err(RecordError::Encoder(reason));
                }
                if Instant::now() >= deadline {
                    return Err(RecordError::Encoder("the audio encoder stopped taking sound".into()));
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        let mut queue = lock(&self.shared.queue);
        if queue.closed {
            return Err(closed());
        }
        queue.jobs.push_back(Job::Audio { track, position, samples: samples.to_vec() });
        queue.audio[track] += frames;
        while !self.offline && queue.audio[track] > MAX_QUEUED_AUDIO_FRAMES {
            let oldest = queue.jobs.iter().position(|job| matches!(job, Job::Audio { track: t, .. } if *t == track));
            let Some(Job::Audio { samples, .. }) = oldest.and_then(|index| queue.jobs.remove(index)) else { break };
            queue.audio[track] -= (samples.len() / audio::CHANNELS) as u64;
        }
        drop(queue);
        self.shared.wake.notify_one();
        Ok(())
    }

    fn failure(&self) -> Option<String> {
        self.shared.failure()
    }

    fn finish(&mut self, end_ns: u64) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        self.finished = true;
        let (reply, answer) = mpsc::sync_channel(1);
        {
            let mut queue = lock(&self.shared.queue);
            if queue.closed {
                return Err(closed());
            }
            queue.jobs.push_back(Job::Finish { end: logic::hns(end_ns), reply });
            queue.closed = true;
        }
        self.shared.wake.notify_all();
        match answer.recv_timeout(FINISH_LIMIT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(RecordError::Encoder(format!(
                "the recording was not finished within {} seconds",
                FINISH_LIMIT.as_secs()
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(RecordError::Encoder("the writer thread ended before finishing the file".into()))
            }
        }
    }

    fn codec(&self) -> Option<VideoCodec> {
        Some(self.setup.codec)
    }

    fn audio_tracks(&self) -> Option<usize> {
        Some(self.setup.audio)
    }
}

impl Drop for WinWriter {
    fn drop(&mut self) {
        // The session always finishes the take; should it not, the thread still finalises the
        // file on its own, without anyone waiting for it here.
        if !self.finished {
            self.shared.close();
        }
        // An export removes its file after a failure or a cancel, which Windows refuses while
        // the writer thread still has it open; the thread finalises it and ends first.
        if self.offline {
            if let Some(worker) = self.worker.take() {
                let deadline = Instant::now() + FINISH_LIMIT;
                while !worker.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if worker.is_finished() {
                    let _ = worker.join();
                }
            }
        }
    }
}

/// What the writer thread takes next.
enum Next {
    Job(Job),
    /// Nothing came for a while; a good moment to read the encoder's backlog again.
    Idle,
    Closed,
}

fn next(shared: &Shared) -> Next {
    let mut queue = lock(&shared.queue);
    loop {
        if let Some(job) = queue.jobs.pop_front() {
            match &job {
                Job::Video { .. } => queue.video = queue.video.saturating_sub(1),
                Job::Audio { track, samples, .. } => {
                    if let Some(frames) = queue.audio.get_mut(*track) {
                        *frames = frames.saturating_sub((samples.len() / audio::CHANNELS) as u64);
                    }
                }
                Job::Finish { .. } => {}
            }
            return Next::Job(job);
        }
        if queue.closed {
            return Next::Closed;
        }
        let (guard, waited) =
            shared.wake.wait_timeout(queue, Duration::from_millis(20)).unwrap_or_else(|poisoned| poisoned.into_inner());
        queue = guard;
        if waited.timed_out() && queue.jobs.is_empty() {
            return Next::Idle;
        }
    }
}

/// The writer thread: opens the file, then writes what the session hands over until it finishes.
fn run(shared: &Shared, spec: &WriterSpec, ready: mpsc::SyncSender<Result<Setup>>) {
    let _apartment = Apartment::enter();
    let platform = match Platform::start() {
        Ok(platform) => platform,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    let mut take = match Take::open(platform.api(), spec) {
        Ok(take) => take,
        Err(error) => {
            let _ = ready.send(Err(error));
            return;
        }
    };
    if ready.send(Ok(take.setup)).is_err() {
        // `create` gave up waiting: nobody will finish this file, so it goes.
        take.discard();
        return;
    }
    drop(ready);
    loop {
        match next(shared) {
            Next::Idle => take.refresh_backlog(shared),
            Next::Closed => {
                let _ = take.finalize(None);
                return;
            }
            Next::Job(Job::Finish { end, reply }) => {
                let result = take.finalize(Some(end));
                let result = match (shared.failure(), result) {
                    (Some(reason), _) => Err(RecordError::Encoder(reason)),
                    (None, result) => result,
                };
                let _ = reply.send(result);
                return;
            }
            Next::Job(job) => {
                if shared.failure().is_some() {
                    continue;
                }
                let written = match job {
                    Job::Video { frame, at } => take.video(&frame, at),
                    Job::Audio { track, position, samples } => take.audio(track, position, &samples),
                    Job::Finish { .. } => Ok(()),
                };
                match written {
                    Ok(()) => take.refresh_backlog(shared),
                    Err(error) => shared.fail(reason(error)),
                }
            }
        }
    }
}

/// The encoder setup of one attempt to open the file.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Setup {
    codec: VideoCodec,
    hardware: bool,
    /// How many of the take's audio tracks the file gets (the first ones).
    audio: usize,
}

/// The file being written, with its sink writer and streams.
struct Take {
    api: &'static Api,
    path: PathBuf,
    spec: WriterSpec,
    setup: Setup,
    bytes: Option<IMFByteStream>,
    writer: Option<IMFSinkWriter>,
    codec_api: Option<ICodecAPI>,
    video_stream: u32,
    audio_streams: Vec<u32>,
    frame_hns: i64,
    last_video: Option<(i64, IMFMediaBuffer)>,
    last_key: Option<i64>,
    audio_end: i64,
    written: u64,
    nv12_len: usize,
}

/// The order in which encoder setups are tried.
fn setups(api: &Api, spec: &WriterSpec) -> Result<Vec<Setup>> {
    let mut codecs = Vec::new();
    if spec.codec == VideoCodec::Hevc && api.has_video_encoder(MFVideoFormat_HEVC) {
        codecs.push(VideoCodec::Hevc);
    }
    if codec_for(VideoCodec::H264, spec.size) == VideoCodec::H264 && api.has_video_encoder(MFVideoFormat_H264) {
        codecs.push(VideoCodec::H264);
    }
    if codecs.is_empty() {
        let too_large = codec_for(VideoCodec::H264, spec.size) != VideoCodec::H264;
        return Err(RecordError::Encoder(if too_large && api.has_video_encoder(MFVideoFormat_H264) {
            format!(
                "a {}×{} recording needs an HEVC encoder, which this Windows does not have; the HEVC Video \
                 Extensions or a graphics driver add one",
                spec.size.width, spec.size.height
            )
        } else {
            NO_ENCODER.to_string()
        }));
    }
    let mut setups = Vec::new();
    for codec in codecs {
        setups.push(Setup { codec, hardware: true, audio: spec.audio.len() });
        setups.push(Setup { codec, hardware: false, audio: spec.audio.len() });
        if spec.audio.len() > 1 {
            setups.push(Setup { codec, hardware: false, audio: 1 });
        }
    }
    Ok(setups)
}

impl Take {
    fn open(api: &'static Api, spec: &WriterSpec) -> Result<Take> {
        let mut last = None;
        for setup in setups(api, spec)? {
            match Take::open_with(api, spec, setup) {
                Ok(take) => return Ok(take),
                // The file exists already: no other setup changes that.
                Err(RecordError::Io(error)) => return Err(RecordError::Io(error)),
                Err(error) => last = Some(error),
            }
        }
        let why = last.map_or_else(|| "no setup was tried".to_string(), reason);
        Err(RecordError::Encoder(format!("no video encoder could be opened for this recording: {why}")))
    }

    fn open_with(api: &'static Api, spec: &WriterSpec, setup: Setup) -> Result<Take> {
        // The byte stream is created with "fail if it exists", so a take never replaces a file.
        let bytes = api.file(MF_ACCESSMODE_READWRITE, MF_OPENMODE_FAIL_IF_EXIST, &spec.path)?;
        let mut take = Take {
            api,
            path: spec.path.clone(),
            spec: spec.clone(),
            setup,
            bytes: Some(bytes),
            writer: None,
            codec_api: None,
            video_stream: 0,
            audio_streams: Vec::new(),
            frame_hns: logic::HNS_PER_SECOND / spec.fps.max(1) as i64,
            last_video: None,
            last_key: None,
            audio_end: 0,
            written: 0,
            nv12_len: logic::nv12_len(spec.size.width as usize, spec.size.height as usize),
        };
        // On any failure from here on, the take is dropped as discarded: the stream is closed
        // and the file this attempt created is removed.
        match take.configure() {
            Ok(()) => Ok(take),
            Err(error) => {
                take.discard();
                Err(error)
            }
        }
    }

    fn configure(&mut self) -> Result<()> {
        let api = self.api;
        let spec = &self.spec;
        let Some(bytes) = self.bytes.as_ref() else { return Err(closed()) };
        let writer = mp4_writer(api, &self.path, bytes, self.setup.hardware)?;
        let fps = spec.fps.max(1);
        let (width, height) = (spec.size.width, spec.size.height);
        let hevc = self.setup.codec == VideoCodec::Hevc;
        // A key frame every second's worth of frames.
        let settings = VideoSettings {
            codec: self.setup.codec,
            width,
            height,
            fps,
            bits: spec.bits_per_second.unwrap_or_else(|| logic::bit_rate(width, height, fps, hevc)).max(1),
            gop: fps,
        };
        let video_stream = add_video(api, &writer, settings)?;

        let mut audio_streams = Vec::new();
        for &kind in spec.audio.iter().take(self.setup.audio) {
            let output = api.media_type()?;
            mf::set_guid(&output, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            mf::set_guid(&output, &MF_MT_SUBTYPE, &MFAudioFormat_AAC)?;
            mf::set_u32(&output, &MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            mf::set_u32(&output, &MF_MT_AUDIO_SAMPLES_PER_SECOND, audio::RATE)?;
            mf::set_u32(&output, &MF_MT_AUDIO_NUM_CHANNELS, audio::CHANNELS as u32)?;
            mf::set_u32(
                &output,
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                logic::aac_bytes_per_second(kind == TrackKind::MixedAudio),
            )?;
            // Raw AAC frames, as MP4 stores them, at AAC-LC level 2.
            mf::set_u32(&output, &MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            mf::set_u32(&output, &MF_MT_AAC_AUDIO_PROFILE_LEVEL_INDICATION, 0x29)?;
            // SAFETY: as for the video stream.
            let stream = unsafe { writer.AddStream(&output) }.map_err(|e| mf::failed("adding an audio stream", &e))?;
            let input = api.media_type()?;
            mf::set_guid(&input, &MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            mf::set_guid(&input, &MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            mf::set_u32(&input, &MF_MT_AUDIO_BITS_PER_SAMPLE, 16)?;
            mf::set_u32(&input, &MF_MT_AUDIO_SAMPLES_PER_SECOND, audio::RATE)?;
            mf::set_u32(&input, &MF_MT_AUDIO_NUM_CHANNELS, audio::CHANNELS as u32)?;
            mf::set_u32(&input, &MF_MT_AUDIO_BLOCK_ALIGNMENT, audio::CHANNELS as u32 * 2)?;
            mf::set_u32(&input, &MF_MT_AUDIO_AVG_BYTES_PER_SECOND, audio::RATE * audio::CHANNELS as u32 * 2)?;
            mf::set_u32(&input, &MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
            // SAFETY: as for the video stream; the AAC encoder takes no encoding parameters.
            unsafe { writer.SetInputMediaType(stream, &input, None::<&IMFAttributes>) }
                .map_err(|e| mf::failed("setting up the AAC encoder", &e))?;
            audio_streams.push(stream);
        }

        // SAFETY: every stream has its types; the call starts the encoders and the sink.
        unsafe { writer.BeginWriting() }.map_err(|e| mf::failed("starting the MP4 writer", &e))?;
        self.codec_api = codec_api(&writer, video_stream);
        self.video_stream = video_stream;
        self.audio_streams = audio_streams;
        self.writer = Some(writer);
        Ok(())
    }

    /// Converts and writes a frame at `at`; the first frame that fails on a hardware encoder
    /// brings the software one in, and is written again there.
    fn video(&mut self, frame: &Frame, at: i64) -> Result<()> {
        if self.last_video.as_ref().is_some_and(|(last, _)| at <= *last) {
            return Ok(());
        }
        let buffer = self.picture(frame)?;
        match self.write_video(&buffer, at, false) {
            Err(error) if self.written == 0 && self.setup.hardware => {
                let setup = Setup { hardware: false, ..self.setup };
                self.discard();
                *self = Take::open_with(self.api, &self.spec.clone(), setup).map_err(|e| {
                    RecordError::Encoder(format!("{}; without hardware encoding: {}", reason(error), reason(e)))
                })?;
                self.write_video(&buffer, at, false)
            }
            result => result,
        }
    }

    /// The frame as an NV12 buffer of the file's size.
    fn picture(&self, frame: &Frame) -> Result<IMFMediaBuffer> {
        let src = Pixels {
            data: &frame.data,
            stride: frame.bytes_per_row as isize,
            width: frame.size.width as usize,
            height: frame.size.height as usize,
            red_first: frame.format == PixelFormat::Rgba8,
        };
        nv12(self.api, &src, self.spec.size.width, self.spec.size.height)
    }

    fn write_video(&mut self, buffer: &IMFMediaBuffer, at: i64, repeat: bool) -> Result<()> {
        let Some(writer) = self.writer.as_ref() else { return Err(closed()) };
        let sample = self.api.sample()?;
        // SAFETY: the sample and the buffer are live objects; the sample keeps its own reference
        // to the buffer, and the time and duration are plain values.
        unsafe {
            sample.AddBuffer(buffer).map_err(|e| mf::failed("IMFSample::AddBuffer", &e))?;
            sample.SetSampleTime(at).map_err(|e| mf::failed("IMFSample::SetSampleTime", &e))?;
            sample.SetSampleDuration(self.frame_hns).map_err(|e| mf::failed("IMFSample::SetSampleDuration", &e))?;
        }
        // A repeat of the last picture never forces a key frame; it costs next to nothing as a
        // predicted frame.
        if !repeat && logic::keyframe_due(self.last_key, at) {
            if let Some(codec) = self.codec_api.as_ref() {
                force_key_frame(codec);
            }
            self.last_key = Some(at);
        }
        // SAFETY: the stream index is the one `AddStream` gave, and the sample is complete.
        unsafe { writer.WriteSample(self.video_stream, &sample) }
            .map_err(|e| mf::failed("writing a video frame", &e))?;
        self.written += 1;
        self.last_video = Some((at, buffer.clone()));
        Ok(())
    }

    fn audio(&mut self, track: usize, position: u64, samples: &[f32]) -> Result<()> {
        // Tracks the file has no stream for (the sink took only the mix) are left out.
        let Some(&stream) = self.audio_streams.get(track) else { return Ok(()) };
        let Some(writer) = self.writer.as_ref() else { return Err(closed()) };
        let frames = (samples.len() / audio::CHANNELS) as u64;
        if frames == 0 {
            return Ok(());
        }
        let mut pcm = Vec::new();
        logic::pcm16(&samples[..frames as usize * audio::CHANNELS], &mut pcm);
        let buffer = self.api.memory_buffer(pcm.len())?;
        {
            let mut locked = Locked::lock(&buffer)?;
            let Some(out) = locked.bytes_mut().get_mut(..pcm.len()) else {
                return Err(RecordError::Encoder("an audio buffer was smaller than asked for".into()));
            };
            out.copy_from_slice(&pcm);
        }
        let at = hns_of_frames(position);
        // The AAC encoder needs a duration on every sample, and one above zero.
        let duration = (hns_of_frames(position + frames) - at).max(1);
        let sample = self.api.sample()?;
        // SAFETY: as in `write_video`.
        unsafe {
            buffer.SetCurrentLength(pcm.len() as u32).map_err(|e| mf::failed("filling an audio buffer", &e))?;
            sample.AddBuffer(&buffer).map_err(|e| mf::failed("IMFSample::AddBuffer", &e))?;
            sample.SetSampleTime(at).map_err(|e| mf::failed("IMFSample::SetSampleTime", &e))?;
            sample.SetSampleDuration(duration).map_err(|e| mf::failed("IMFSample::SetSampleDuration", &e))?;
            writer.WriteSample(stream, &sample).map_err(|e| mf::failed("writing audio", &e))?;
        }
        self.audio_end = self.audio_end.max(at + duration);
        // A still screen sends no frames; the last picture is written again about every second,
        // so the file's video keeps up with its audio and the sink never waits for video.
        if let Some((last, picture)) = self.last_video.clone() {
            if let Some(repeat) = logic::repeat_due(last, self.audio_end) {
                let copy = self.copy_of(&picture)?;
                self.write_video(&copy, repeat, true)?;
            }
        }
        Ok(())
    }

    /// A fresh buffer with the bytes of `picture`, since the encoder may still hold that one.
    fn copy_of(&self, picture: &IMFMediaBuffer) -> Result<IMFMediaBuffer> {
        let copy = self.api.memory_buffer(self.nv12_len)?;
        {
            let source = Locked::lock(picture)?;
            let mut target = Locked::lock(&copy)?;
            let (Some(from), Some(to)) =
                (source.bytes().get(..self.nv12_len), target.bytes_mut().get_mut(..self.nv12_len))
            else {
                return Err(RecordError::Encoder("a picture buffer was smaller than asked for".into()));
            };
            to.copy_from_slice(from);
        }
        // SAFETY: as in `picture`.
        unsafe { copy.SetCurrentLength(self.nv12_len as u32) }.map_err(|e| mf::failed("filling a picture", &e))?;
        Ok(copy)
    }

    /// Reads how many frames the sink writer still holds, which `append_video` drops frames by.
    fn refresh_backlog(&self, shared: &Shared) {
        let Some(writer) = self.writer.as_ref() else { return };
        let mut stats =
            MF_SINK_WRITER_STATISTICS { cb: size_of::<MF_SINK_WRITER_STATISTICS>() as u32, ..Default::default() };
        // SAFETY: the structure is the size its `cb` says, and the call fills it in.
        if unsafe { writer.GetStatistics(self.video_stream, &mut stats) }.is_ok() {
            let waiting = stats.qwNumSamplesReceived.saturating_sub(stats.qwNumSamplesProcessed);
            shared.backlog.store(waiting, Ordering::SeqCst);
        }
    }

    /// Finishes the file: the last picture is held until `end` (when given), then the index is
    /// written and the file closed. A file that got no frame at all is removed instead.
    fn finalize(&mut self, end: Option<i64>) -> Result<()> {
        if self.written == 0 {
            self.discard();
            return Err(RecordError::Encoder("the recording ended before its first frame was written".into()));
        }
        let mut held = Ok(());
        if let (Some(end), Some((last, picture))) = (end, self.last_video.clone()) {
            let at = end - self.frame_hns;
            if at > last {
                held = self.copy_of(&picture).and_then(|copy| self.write_video(&copy, at, true));
            }
        }
        self.codec_api = None;
        let finalized = match self.writer.take() {
            // SAFETY: the writer is live and finalised once; it drains the encoders, writes the
            // index and stops writing to the byte stream.
            Some(writer) => unsafe { writer.Finalize() }.map_err(|e| mf::failed("finishing the MP4", &e)),
            None => Err(closed()),
        };
        self.close_bytes();
        finalized.and(held)
    }

    /// Lets go of the writer and removes the file this take created.
    fn discard(&mut self) {
        self.codec_api = None;
        self.writer = None;
        self.close_bytes();
        let _ = std::fs::remove_file(&self.path);
    }

    /// Closes the file now, so it can be moved or removed as soon as the writer returns.
    fn close_bytes(&mut self) {
        if let Some(bytes) = self.bytes.take() {
            // SAFETY: the byte stream is live and closed once; nothing writes to it any more.
            let _ = unsafe { bytes.Close() };
        }
    }
}

impl Drop for Take {
    fn drop(&mut self) {
        self.codec_api = None;
        self.writer = None;
        self.close_bytes();
    }
}

/// `frames` audio frames at 48 kHz in 100 ns units.
fn hns_of_frames(frames: u64) -> i64 {
    (frames as i128 * logic::HNS_PER_SECOND as i128 / audio::RATE as i128).min(i64::MAX as i128) as i64
}

/// A sink writer that makes an MP4 on `bytes`, with hardware encoders allowed or not, and with
/// throttling off, so that a write never blocks to slow the caller down.
pub(super) fn mp4_writer(api: &Api, path: &Path, bytes: &IMFByteStream, hardware: bool) -> Result<IMFSinkWriter> {
    let attributes = api.attributes(3)?;
    mf::set_guid(&attributes, &MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
    mf::set_u32(&attributes, &MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, hardware as u32)?;
    mf::set_u32(&attributes, &MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
    api.sink_writer(path, bytes, &attributes)
}

/// What a video encoder is set up with.
#[derive(Clone, Copy, Debug)]
pub(super) struct VideoSettings {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bits: u32,
    /// Frames from one key frame to the next.
    pub gop: u32,
}

/// Adds an H.264 High (or HEVC Main) stream fed with NV12 pictures of the settings' size, and
/// returns its index.
pub(super) fn add_video(api: &Api, writer: &IMFSinkWriter, settings: VideoSettings) -> Result<u32> {
    let VideoSettings { codec, width, height, fps, bits, gop } = settings;
    let hevc = codec == VideoCodec::Hevc;
    let output = video_type(api, if hevc { MFVideoFormat_HEVC } else { MFVideoFormat_H264 }, width, height, fps)?;
    mf::set_u32(&output, &MF_MT_AVG_BITRATE, bits)?;
    if hevc {
        mf::set_u32(&output, &MF_MT_VIDEO_PROFILE, eAVEncH265VProfile_Main_420_8.0 as u32)?;
        mf::set_u32(&output, &MF_MT_MPEG2_LEVEL, logic::hevc_level(width, height, fps))?;
    } else {
        mf::set_u32(&output, &MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
    }
    // SAFETY: the media type is a live object; the call reads it and keeps its own copy.
    let stream = unsafe { writer.AddStream(&output) }.map_err(|e| mf::failed("adding the video stream", &e))?;
    let input = video_type(api, MFVideoFormat_NV12, width, height, fps)?;
    mf::set_u32(&input, &MF_MT_DEFAULT_STRIDE, width)?;
    mf::set_u32(&input, &MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
    // No B-frames: the trim cuts at key frames, and a still screen sends frames rarely.
    let parameters = api.attributes(3)?;
    mf::set_u32(&parameters, &CODECAPI_AVEncMPVGOPSize, gop)?;
    mf::set_u32(&parameters, &CODECAPI_AVEncMPVDefaultBPictureCount, 0)?;
    mf::set_u32(&parameters, &CODECAPI_AVEncCommonMeanBitRate, bits)?;
    // SAFETY: both objects are live; the call reads them.
    unsafe { writer.SetInputMediaType(stream, &input, &parameters) }.map_err(|e| {
        mf::failed(if hevc { "setting up the HEVC encoder" } else { "setting up the H.264 encoder" }, &e)
    })?;
    Ok(stream)
}

/// `src` as an NV12 buffer of `width`×`height`, ready to be written.
pub(super) fn nv12(api: &Api, src: &Pixels, width: u32, height: u32) -> Result<IMFMediaBuffer> {
    let len = logic::nv12_len(width as usize, height as usize);
    let buffer = api.memory_buffer(len)?;
    {
        let mut locked = Locked::lock(&buffer)?;
        let Some(out) = locked.bytes_mut().get_mut(..len) else {
            return Err(RecordError::Encoder("a picture buffer was smaller than asked for".into()));
        };
        if !logic::bgra_to_nv12(src, out, width as usize, height as usize) {
            return Err(RecordError::Encoder(format!(
                "a {}×{} picture could not be converted into a {width}×{height} frame",
                src.width, src.height
            )));
        }
    }
    // SAFETY: the buffer is unlocked and at least `len` bytes long.
    unsafe { buffer.SetCurrentLength(len as u32) }.map_err(|e| mf::failed("filling a picture", &e))?;
    Ok(buffer)
}

/// A video media type of `subtype` with the size, rate and BT.709 colour every recording has.
fn video_type(api: &Api, subtype: GUID, width: u32, height: u32, fps: u32) -> Result<IMFMediaType> {
    let media = api.media_type()?;
    mf::set_guid(&media, &MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
    mf::set_guid(&media, &MF_MT_SUBTYPE, &subtype)?;
    mf::set_u64(&media, &MF_MT_FRAME_SIZE, logic::pack(width, height))?;
    mf::set_u64(&media, &MF_MT_FRAME_RATE, logic::pack(fps, 1))?;
    mf::set_u64(&media, &MF_MT_PIXEL_ASPECT_RATIO, logic::pack(1, 1))?;
    mf::set_u32(&media, &MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
    mf::set_u32(&media, &MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
    mf::set_u32(&media, &MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
    mf::set_u32(&media, &MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
    mf::set_u32(&media, &MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
    Ok(media)
}

/// The video encoder's settings interface, through which a key frame is asked for; `None` when
/// the encoder does not offer it, and the GOP size alone places key frames.
pub(super) fn codec_api(writer: &IMFSinkWriter, stream: u32) -> Option<ICodecAPI> {
    let mut raw = std::ptr::null_mut();
    // SAFETY: the stream index is valid after `BeginWriting`, the GUIDs outlive the call, and on
    // success it writes one reference of `ICodecAPI`.
    unsafe { writer.GetServiceForStream(stream, &GUID::zeroed(), &ICodecAPI::IID, &mut raw) }.ok()?;
    // SAFETY: the call succeeded, so `raw` is an owned `ICodecAPI` reference (or null).
    (!raw.is_null()).then(|| unsafe { ICodecAPI::from_raw(raw) })
}

/// Asks the encoder to make the next frame a key frame. An encoder that refuses still puts one
/// at every GOP, so a refusal is no failure.
pub(super) fn force_key_frame(codec: &ICodecAPI) {
    let mut value = VARIANT::default();
    // SAFETY: a zeroed VARIANT is VT_EMPTY; setting the tag and the member it names gives a
    // valid VT_UI4 value that owns nothing, so it needs no clearing.
    unsafe {
        let inner = &mut *value.Anonymous.Anonymous;
        inner.vt = VT_UI4;
        inner.Anonymous.ulVal = 1;
    }
    // SAFETY: the GUID and the VARIANT outlive the call, which copies the value.
    let _ = unsafe { codec.SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &value) };
}
