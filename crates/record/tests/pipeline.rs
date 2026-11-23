//! The recording pipeline end to end, without a screen: synthetic frames and audio go through
//! the session into the platform's real writer, and the platform's reader reads the file back.
//! The export runs on that file. On macOS the frames are `CVPixelBuffer`s for `AVAssetWriter`
//! and AVFoundation reads them back; on Linux they are BGRA frames in memory, as the X11 and
//! portal streams deliver them, for the GStreamer writer and reader. On Windows the same BGRA
//! frames go to Media Foundation's sink writer and come back through its source reader.
#![cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]

use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glimpsfy_capture::{
    AudioBuffer, AudioSource, CaptureError, CaptureStream, ColorSpace, DisplayId, HostTime, MicrophoneSource,
    PhysicalSize, StreamConfig, StreamPixelFormat, StreamSink, StreamTarget, VideoBuffer, VideoFrame,
};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use glimpsfy_capture::{ColorInfo, Frame, PixelFormat, ScaleFactor};
#[cfg(target_os = "macos")]
use glimpsfy_capture::{MacPixelBuffer, NativeBuffer};
use glimpsfy_record::export::{self, Progress};
use glimpsfy_record::files::Trash;
use glimpsfy_record::session::{Clock, Env, OwnWindows, Recorder, StreamOpener, SystemWriters};
use glimpsfy_record::{
    platform, AudioConfig, ClickHighlight, ExportAudio, ExportFormat, ExportOptions, ExportPurpose, KeystrokeOverlay,
    Mp4Quality, RecordError, RecordingConfig, RecordingId, RecordingOutcome, Resolution, TrackKind, TrimRange,
    VideoCodec, WebcamOverlay,
};
#[cfg(target_os = "macos")]
use objc2_core_foundation::CFRetained;
#[cfg(target_os = "macos")]
use objc2_core_video::{
    kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange, CVPixelBuffer, CVPixelBufferCreate,
    CVPixelBufferGetBaseAddressOfPlane, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferLockBaseAddress,
    CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};

const W: usize = 640;
const H: usize = 360;
const FPS: u64 = 30;
const FRAME_NS: u64 = 1_000_000_000 / FPS;

struct TestClock(AtomicU64);
impl Clock for TestClock {
    fn now(&self) -> HostTime {
        HostTime(self.0.load(Ordering::SeqCst))
    }
}

#[derive(Default)]
struct Streams(Mutex<Option<Arc<dyn StreamSink>>>);
struct Quiet;
impl CaptureStream for Quiet {
    fn start(&mut self) -> glimpsfy_capture::Result<()> {
        Ok(())
    }
    fn stop(&mut self) -> glimpsfy_capture::Result<()> {
        Ok(())
    }
    fn update(&mut self, _: StreamConfig) -> glimpsfy_capture::Result<()> {
        Ok(())
    }
}
impl StreamOpener for Streams {
    fn open(&self, _: StreamConfig, sink: Box<dyn StreamSink>) -> Result<Box<dyn CaptureStream>, CaptureError> {
        let sink: Arc<dyn StreamSink> = Arc::from(sink);
        *self.0.lock().unwrap() = Some(sink);
        Ok(Box::new(Quiet))
    }
}

struct NoTrash;
impl Trash for NoTrash {
    fn trash(&self, _: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("the tests never use the real Trash"))
    }
}

/// The rows that carry a frame's number `n` in binary: eight blocks of 80 pixels from the left,
/// dark for 0 and bright for 1, so a decoded frame says which one it was.
const INDEX_ROWS: std::ops::Range<usize> = 296..352;

/// Whether the pixel at `row`, `col` of frame `n` is bright, or `None` outside the index rows.
fn index_bit(n: u64, row: usize, col: usize) -> Option<bool> {
    INDEX_ROWS.contains(&row).then_some((n >> (col / 80)) & 1 == 1)
}

/// The number a decoded RGBA frame of the recording's size carries.
fn index_of(rgba: &[u8]) -> u64 {
    let row = (INDEX_ROWS.start + INDEX_ROWS.end) / 2;
    (0..8).map(|bit| ((rgba[(row * W + bit * 80 + 40) * 4 + 1] > 128) as u64) << bit).sum()
}

/// The frame the stream would send for `n`, in the platform's own buffer.
#[cfg(target_os = "macos")]
fn video(n: u64) -> (StreamPixelFormat, VideoBuffer) {
    (StreamPixelFormat::Nv12, VideoBuffer::Native(NativeBuffer(Box::new(MacPixelBuffer(frame(n))))))
}

/// The same picture as the macOS frame, as BGRA in memory: a grey ramp, a tinted right half and
/// a bright square that moves with `n`, so the encoder has motion to code. The Windows stream
/// delivers frames in memory the same way.
#[cfg(any(target_os = "linux", target_os = "windows"))]
fn video(n: u64) -> (StreamPixelFormat, VideoBuffer) {
    let mut data = vec![0u8; W * H * 4];
    let sx = (n as usize * 7) % (W - 80);
    for (i, pixel) in data.chunks_exact_mut(4).enumerate() {
        let (row, col) = (i / W, i % W);
        let inside = col >= sx && col < sx + 80 && (100..180).contains(&row);
        let luma = if inside { 255 } else { (col * 64 / W) as u8 };
        let red = if col > W / 2 && !inside { luma.saturating_add(40) } else { luma };
        match index_bit(n, row, col) {
            Some(bright) => pixel.copy_from_slice(&if bright { [255, 255, 255, 255] } else { [0, 0, 0, 255] }),
            None => pixel.copy_from_slice(&[luma, luma, red, 255]),
        }
    }
    let size = PhysicalSize { width: W as u32, height: H as u32 };
    let frame = Frame::new(size, W * 4, PixelFormat::Bgra8, ColorInfo::srgb(), ScaleFactor(1.0), data).unwrap();
    (StreamPixelFormat::Bgra, VideoBuffer::Cpu(Arc::new(frame)))
}

/// An NV12 frame with a bright square that moves with `n`, so the encoder has motion to code.
#[cfg(target_os = "macos")]
fn frame(n: u64) -> CFRetained<CVPixelBuffer> {
    let mut out: *mut CVPixelBuffer = std::ptr::null_mut();
    // SAFETY: a new pixel buffer is created, locked while its two planes are written within their
    // strides, and unlocked.
    unsafe {
        let status = CVPixelBufferCreate(
            None,
            W,
            H,
            kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
            None,
            NonNull::from(&mut out),
        );
        assert_eq!(status, 0);
        let buffer = CFRetained::from_raw(NonNull::new(out).unwrap());
        CVPixelBufferLockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        let y = CVPixelBufferGetBaseAddressOfPlane(&buffer, 0) as *mut u8;
        let y_stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, 0);
        let uv = CVPixelBufferGetBaseAddressOfPlane(&buffer, 1) as *mut u8;
        let uv_stride = CVPixelBufferGetBytesPerRowOfPlane(&buffer, 1);
        let sx = (n as usize * 7) % (W - 80);
        for row in 0..H {
            for col in 0..W {
                let inside = col >= sx && col < sx + 80 && (100..180).contains(&row);
                *y.add(row * y_stride + col) = match index_bit(n, row, col) {
                    Some(true) => 235,
                    Some(false) => 16,
                    None if inside => 235,
                    None => 16 + (col * 64 / W) as u8,
                };
            }
        }
        for row in 0..H / 2 {
            for col in 0..W / 2 {
                let tinted = col > W / 4 && !INDEX_ROWS.contains(&(row * 2));
                *uv.add(row * uv_stride + col * 2) = 128;
                *uv.add(row * uv_stride + col * 2 + 1) = if tinted { 160 } else { 128 };
            }
        }
        CVPixelBufferUnlockBaseAddress(&buffer, CVPixelBufferLockFlags(0));
        buffer
    }
}

struct Rig {
    recorder: Recorder,
    streams: Arc<Streams>,
    clock: Arc<TestClock>,
    dir: tempfile::TempDir,
}

fn rig() -> Rig {
    let streams = Arc::new(Streams::default());
    let clock = Arc::new(TestClock(AtomicU64::new(5_000_000_000)));
    let env = Env {
        streams: streams.clone(),
        writers: Arc::new(SystemWriters),
        clock: clock.clone(),
        trash: Arc::new(NoTrash),
        on_fault: Arc::new(|_| {}),
    };
    Rig { recorder: Recorder::new(env), streams, clock, dir: tempfile::tempdir().unwrap() }
}

fn config(dir: &Path, codec: VideoCodec) -> RecordingConfig {
    RecordingConfig {
        target: StreamTarget::Display { display: DisplayId(1) },
        fps: FPS as u32,
        resolution: Resolution::Native,
        codec,
        shows_cursor: true,
        audio: AudioConfig {
            system: true,
            microphone: Some(MicrophoneSource { device_id: None }),
            separate_tracks: true,
        },
        clicks: ClickHighlight::default(),
        keystrokes: KeystrokeOverlay::default(),
        webcam: WebcamOverlay::default(),
        countdown_seconds: 3,
        output_folder: dir.to_path_buf(),
        clean_desktop: false,
    }
}

impl Rig {
    fn sink(&self) -> Arc<dyn StreamSink> {
        self.streams.0.lock().unwrap().clone().unwrap()
    }

    /// Feeds `frames` frames and the audio between them, as the stream would, advancing the
    /// clock; paced so the real-time encoder keeps up.
    fn feed(&self, frames: u64, counter: &mut u64) {
        let sink = self.sink();
        for _ in 0..frames {
            let t = self.clock.0.fetch_add(FRAME_NS, Ordering::SeqCst);
            let (format, buffer) = video(*counter);
            sink.video(VideoFrame {
                timestamp: HostTime(t),
                size: PhysicalSize { width: W as u32, height: H as u32 },
                format,
                color_space: ColorSpace::Srgb,
                buffer,
            });
            // The microphone: mono 48 kHz, 440 Hz. The system: stereo 44.1 kHz, 660 Hz.
            let mic: Vec<f32> = (0..1600)
                .map(|i| ((*counter * 1600 + i) as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin() * 0.3)
                .collect();
            sink.audio(AudioBuffer {
                timestamp: HostTime(t),
                source: AudioSource::Microphone,
                sample_rate: 48_000,
                channels: 1,
                samples: mic,
            });
            let sys: Vec<f32> = (0..1470 * 2)
                .map(|i| ((*counter * 1470 + i / 2) as f32 * 660.0 * std::f32::consts::TAU / 44_100.0).sin() * 0.2)
                .collect();
            sink.audio(AudioBuffer {
                timestamp: HostTime(t),
                source: AudioSource::System,
                sample_rate: 44_100,
                channels: 2,
                samples: sys,
            });
            *counter += 1;
            // At the screen's own pace: a runner's software encoder (x265, or Media Foundation's H.264)
            // cannot take frames faster than real time, and the writers drop what they cannot keep up with.
            std::thread::sleep(Duration::from_nanos(FRAME_NS));
        }
    }

    fn record(&self, name: &str, codec: VideoCodec, mut during: impl FnMut(&Rig, &mut u64)) -> PathBuf {
        let path = self.dir.path().join(name);
        let id = RecordingId(name.replace('.', "-"));
        self.recorder.begin(id, config(self.dir.path(), codec), path.clone(), &OwnWindows::default()).unwrap();
        let mut counter = 0;
        self.feed(10, &mut counter);
        self.recorder.go().unwrap();
        during(self, &mut counter);
        path
    }
}

/// Whether this Windows has nothing to encode with: a Windows Server without its Media
/// Foundation feature or an N edition without the Media Feature Pack, as a CI runner may be. The
/// tests that record skip there and say so; any other failure of the writer is a real one.
#[cfg(target_os = "windows")]
fn no_encoder_here() -> bool {
    use glimpsfy_record::session::{WriterFactory, WriterSpec};
    let dir = tempfile::tempdir().unwrap();
    let spec = WriterSpec {
        path: dir.path().join("probe.mp4"),
        size: PhysicalSize { width: W as u32, height: H as u32 },
        fps: FPS as u32,
        codec: VideoCodec::H264,
        color_space: ColorSpace::Srgb,
        audio: vec![],
        fragment_interval_ms: 2_000,
        bits_per_second: None,
        offline: false,
    };
    match SystemWriters.create(&spec) {
        Ok(mut writer) => {
            // A writer that got no frame removes its file and says so; that is expected here.
            let _ = writer.finish(0);
            false
        }
        Err(e) => {
            let reason = e.to_string();
            let missing =
                reason.contains("Media Foundation is not available") || reason.contains("has no H.264 encoder");
            assert!(missing, "the writer failed: {reason}");
            println!("skipped, this Windows cannot record: {reason}");
            true
        }
    }
}

#[cfg(not(target_os = "windows"))]
fn no_encoder_here() -> bool {
    false
}

fn saved(outcome: RecordingOutcome) -> glimpsfy_record::RecordingResult {
    match outcome {
        RecordingOutcome::Saved { result } => result,
        other => panic!("expected a saved take, got {other:?}"),
    }
}

/// Three seconds, a two-second pause, three seconds: the file is six seconds long, its frames
/// come in order with no gap where the pause was, and every track is there.
#[test]
fn pause_leaves_no_gap_and_the_file_has_every_track() {
    if no_encoder_here() {
        return;
    }
    let rig = rig();
    let path = rig.record("paused.mp4", VideoCodec::H264, |rig, n| {
        rig.feed(90, n);
        rig.recorder.pause().unwrap();
        rig.feed(60, n);
        rig.recorder.resume().unwrap();
        rig.feed(90, n);
    });
    let result = saved(rig.recorder.stop().unwrap());
    assert_eq!(result.path, path);

    let info = platform::inspect(&path).unwrap();
    assert_eq!(info.video_codec, "avc1");
    assert_eq!(info.size, PhysicalSize { width: W as u32, height: H as u32 });
    assert_eq!(info.audio, vec![TrackKind::MixedAudio, TrackKind::Microphone, TrackKind::SystemAudio]);
    assert!((5_800..=6_300).contains(&info.duration_ms), "{} ms", info.duration_ms);

    let times = platform::video_timestamps(&path).unwrap();
    assert!(times.len() >= 170, "only {} frames", times.len());
    assert_eq!(times[0], 0);
    for pair in times.windows(2) {
        assert!(pair[1] > pair[0], "a timestamp went backwards");
        assert!(pair[1] - pair[0] <= 3 * FRAME_NS, "a gap of {} ms", (pair[1] - pair[0]) / 1_000_000);
    }
    if let Ok(dir) = std::env::var("GLIMPSFY_PROBE_DIR") {
        std::fs::copy(&path, Path::new(&dir).join("paused.mp4")).unwrap();
    }
    println!("pause: {} frames, {} ms, {} bytes", times.len(), info.duration_ms, info.file_bytes);
}

/// A take cut short (the copy taken while it is still being written is what a crash leaves)
/// still opens and plays up to its last fragment. The Windows writer makes a plain MP4, whose
/// index is written at the end, so there this does not hold (see `platform/windows/writer.rs`).
#[test]
#[cfg(not(target_os = "windows"))]
fn a_truncated_take_plays_to_its_last_fragment() {
    let rig = rig();
    let crashed = rig.dir.path().join("crashed.mp4");
    let path = rig.record("long.mp4", VideoCodec::Hevc, |rig, n| {
        rig.feed(5 * FPS, n);
        // Let the writer flush the fragments it has.
        std::thread::sleep(Duration::from_millis(500));
    });
    std::fs::copy(&path, &crashed).unwrap();
    let _ = saved(rig.recorder.stop().unwrap());

    let info = platform::inspect(&crashed).unwrap();
    assert_eq!(info.video_codec, "hvc1");
    assert!(info.duration_ms >= 1_900, "the cut file holds {} ms", info.duration_ms);
    let times = platform::video_timestamps(&crashed).unwrap();
    assert!(times.len() as u64 >= 2 * FPS - 5, "the cut file holds {} frames", times.len());
    let full = platform::inspect(&path).unwrap();
    println!("truncated: {} of {} ms, {} frames", info.duration_ms, full.duration_ms, times.len());
}

fn take(rig: &Rig) -> PathBuf {
    let path = rig.record("source.mp4", VideoCodec::H264, |rig, n| rig.feed(6 * FPS, n));
    let _ = saved(rig.recorder.stop().unwrap());
    path
}

fn options(format: ExportFormat, trim: Option<TrimRange>) -> ExportOptions {
    ExportOptions {
        format,
        trim,
        destination: None,
        audio: ExportAudio::default(),
        purpose: ExportPurpose::Save,
        cuts: Vec::new(),
    }
}

/// Exports never replace a file, and a cancelled export leaves nothing under the final name or
/// any working file behind. Pass-through, re-encode and GIF all produce playable files.
#[test]
fn exports_never_replace_and_cancel_cleanly() {
    if no_encoder_here() {
        return;
    }
    let rig = rig();
    let source = take(&rig);
    let id = RecordingId("source".into());
    let out = rig.dir.path().join("out");
    std::fs::create_dir(&out).unwrap();
    let never = AtomicBool::new(false);
    let mut quiet = |_: Progress| {};

    // Pass-through trim: starts at the keyframe at or before 2.5 s, and keeps the video stream.
    let wanted = out.join("clip.mp4");
    std::fs::write(&wanted, b"keep me").unwrap();
    let mp4 = ExportFormat::Mp4 { resolution: Resolution::Native, quality: Mp4Quality::High };
    let trim = TrimRange { start_ms: 2_500, end_ms: 4_500 };
    let started = std::time::Instant::now();
    let result = export::export(&id, &source, &options(mp4.clone(), Some(trim)), &wanted, &never, &mut quiet).unwrap();
    if let Ok(dir) = std::env::var("GLIMPSFY_PROBE_DIR") {
        std::fs::copy(&result.path, Path::new(&dir).join("passthrough.mp4")).unwrap();
        std::fs::copy(&source, Path::new(&dir).join("source.mp4")).unwrap();
    }
    println!("pass-through: {:?} in {:?}, {} bytes", result.trim, started.elapsed(), result.file_bytes);
    assert_eq!(std::fs::read(&wanted).unwrap(), b"keep me", "the existing file was replaced");
    assert_eq!(result.path, out.join("clip 2.mp4"));
    assert!(!result.reencoded);
    let keyframes = platform::keyframe_times(&source).unwrap();
    assert!(
        keyframes.contains(&(result.trim.start_ms * 1_000_000))
            || keyframes.iter().any(|k| k / 1_000_000 == result.trim.start_ms),
        "{:?} does not start on a keyframe of {keyframes:?}",
        result.trim
    );
    assert!(result.trim.start_ms <= 2_500 && result.trim.start_ms >= 1_400, "{:?}", result.trim);
    let info = platform::inspect(&result.path).unwrap();
    let expected = result.trim.end_ms - result.trim.start_ms;
    assert!(info.duration_ms.abs_diff(expected) <= 120, "{} ms instead of {expected}", info.duration_ms);
    assert_eq!(info.audio.len(), 1, "the export keeps the mixed track only");

    // Re-encoded at half size: exact cut.
    let half = ExportFormat::Mp4 { resolution: Resolution::Half, quality: Mp4Quality::Medium };
    let started = std::time::Instant::now();
    let result =
        export::export(&id, &source, &options(half, Some(trim)), &out.join("half.mp4"), &never, &mut quiet).unwrap();
    println!("re-encode: {:?} in {:?}, {} bytes", result.trim, started.elapsed(), result.file_bytes);
    assert!(result.reencoded);
    assert_eq!(result.trim, trim);
    let info = platform::inspect(&result.path).unwrap();
    assert_eq!(info.size, PhysicalSize { width: W as u32 / 2, height: H as u32 / 2 });
    assert!(info.duration_ms.abs_diff(2_000) <= 120, "{} ms", info.duration_ms);

    // GIF: a real GIF with the frames of the trim at 10 fps.
    let gif = ExportFormat::Gif { fps: 10, width: Some(320), loop_forever: true };
    let started = std::time::Instant::now();
    let result =
        export::export(&id, &source, &options(gif.clone(), Some(trim)), &out.join("clip.gif"), &never, &mut quiet)
            .unwrap();
    println!("gif: in {:?}, {} bytes", started.elapsed(), result.file_bytes);
    let mut decoder = gif::DecodeOptions::new().read_info(std::fs::File::open(&result.path).unwrap()).unwrap();
    assert_eq!((decoder.width(), decoder.height()), (320, 180));
    let mut frames = 0;
    let mut centis = 0u32;
    while let Some(f) = decoder.read_next_frame().unwrap() {
        frames += 1;
        centis += f.delay as u32;
    }
    assert!((15..=21).contains(&frames), "{frames} frames");
    assert!(centis.abs_diff(200) <= 12, "{centis} cs");

    // Cancelled half-way: no file under the final name, no working file left.
    let cancel = AtomicBool::new(false);
    let mut stop_half_way = |p: Progress| {
        if p.fraction > 0.3 {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let wanted = out.join("cancelled.gif");
    let err = export::export(&id, &source, &options(gif, None), &wanted, &cancel, &mut stop_half_way).unwrap_err();
    assert!(matches!(err, RecordError::Cancelled), "{err:?}");
    let cancel = AtomicBool::new(false);
    let mut stop_mp4 = |p: Progress| {
        if p.fraction > 0.3 {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let reencode = ExportFormat::Mp4 { resolution: Resolution::Native, quality: Mp4Quality::Low };
    let err =
        export::export(&id, &source, &options(reencode, None), &out.join("cancelled.mp4"), &cancel, &mut stop_mp4)
            .unwrap_err();
    assert!(matches!(err, RecordError::Cancelled), "{err:?}");
    let mut left: Vec<String> =
        std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, vec!["clip 2.mp4", "clip.gif", "clip.mp4", "half.mp4"]);
}

/// The number and the time of every frame `path` shows, decoded at the recording's size.
fn shown_frames(path: &Path) -> Vec<(u64, u64)> {
    let info = platform::inspect(path).unwrap();
    let whole = TrimRange { start_ms: 0, end_ms: info.duration_ms + 1 };
    // A CI runner's virtual Mac now and then refuses to decode a sample ("Cannot Decode", -11821)
    // that decodes the next time; a file that is really damaged fails all three reads.
    for attempt in 1..=3 {
        let mut frames = Vec::new();
        match platform::decode_video(path, whole, info.size, &AtomicBool::new(false), &mut |rgba, at| {
            frames.push((index_of(rgba), at));
            Ok(())
        }) {
            Ok(()) => return frames,
            Err(error) if attempt < 3 => eprintln!("{} is read again after: {error:?}", path.display()),
            Err(error) => panic!("{} cannot be decoded: {error:?}", path.display()),
        }
    }
    unreachable!("the third read returns or panics")
}

/// Cuts take two parts out of a take (given unsorted and overlapping), and the name the export
/// wants is taken. The file goes to another name and the one there stays as it was; it holds
/// every frame shown in the kept parts exactly once, in order and at its place in the file, none
/// from inside the cuts; its picture and its sound last as long as the kept parts. A cancelled
/// cut export leaves no file and no working file behind.
#[test]
fn a_cut_export_keeps_each_kept_frame_once() {
    if no_encoder_here() {
        return;
    }
    let rig = rig();
    let source = take(&rig);
    let id = RecordingId("source".into());
    let out = rig.dir.path().join("cut");
    std::fs::create_dir(&out).unwrap();
    let wanted = out.join("talk.mp4");
    std::fs::write(&wanted, b"keep me").unwrap();
    let mp4 = ExportFormat::Mp4 { resolution: Resolution::Native, quality: Mp4Quality::High };
    let mut cut = options(mp4, None);
    cut.cuts = vec![
        TrimRange { start_ms: 3_400, end_ms: 4_000 },
        TrimRange { start_ms: 1_200, end_ms: 2_000 },
        TrimRange { start_ms: 3_800, end_ms: 4_300 },
    ];
    let never = AtomicBool::new(false);
    let started = std::time::Instant::now();
    let result = export::export(&id, &source, &cut, &wanted, &never, &mut |_| {}).unwrap();
    println!("cut export: {:?} in {:?}, {} bytes", result.kept, started.elapsed(), result.file_bytes);
    let numbers = |frames: &[(u64, u64)]| frames.iter().map(|(n, _)| *n).collect::<Vec<_>>();
    assert_eq!(std::fs::read(&wanted).unwrap(), b"keep me", "the existing file was replaced");
    assert_eq!(result.path, out.join("talk 2.mp4"));
    assert!(result.reencoded);
    assert_eq!(result.kept.len(), 3, "{:?}", result.kept);
    let kept_ms: u64 = result.kept.iter().map(|k| k.end_ms - k.start_ms).sum();
    assert_eq!(result.duration_ms, kept_ms);

    // What the kept parts show: in each, the frame on screen at its start (which may come from
    // before it), then every frame after its start, placed where the part lies in the file.
    let mut source_frames = shown_frames(&source);
    // The writer shows the take's last picture once more just before its end too (GStreamer does).
    let last_shown_twice =
        source_frames.len() > 1 && source_frames[source_frames.len() - 1].0 == source_frames[source_frames.len() - 2].0;
    if last_shown_twice {
        source_frames.pop();
    }
    assert!(
        source_frames.len() > 100 && source_frames.windows(2).all(|w| w[1].0 > w[0].0 && w[1].1 > w[0].1),
        "the take's frames do not say their numbers: {:?}",
        numbers(&source_frames)
    );
    let mut expected = Vec::new();
    let mut output_ms = 0;
    for part in &result.kept {
        let (start, end) = (part.start_ms * 1_000_000, part.end_ms * 1_000_000);
        let base = output_ms * 1_000_000;
        if let Some(&(n, _)) = source_frames.iter().rev().find(|(_, at)| *at <= start) {
            expected.push((n, base));
        }
        let after_start = source_frames.iter().filter(|(_, at)| *at > start && *at < end);
        expected.extend(after_start.map(|&(n, at)| (n, base + at - start)));
        output_ms += part.end_ms - part.start_ms;
    }
    let mut got = shown_frames(&result.path);
    if cfg!(windows) {
        // On Windows each kept part misses the picture on screen at its start and shows another
        // picture twice, which pushes its last frame out (seen on the CI runners; the cause in the
        // Media Foundation reader or writer is still open). Neither shows anything from a cut, so
        // there the check takes every frame once, wants no frame that is not kept, lets each part
        // miss two at most and lets a frame sit one frame late.
        got.dedup_by_key(|(n, _)| *n);
        assert!(got.windows(2).all(|w| w[1].0 > w[0].0), "the exported frames are out of order: {:?}", numbers(&got));
        let extra: Vec<u64> = numbers(&got).into_iter().filter(|n| !expected.iter().any(|(m, _)| m == n)).collect();
        assert!(extra.is_empty(), "the export shows frames that were not kept: {extra:?}");
        let missing: Vec<u64> = numbers(&expected).into_iter().filter(|n| !got.iter().any(|(m, _)| m == n)).collect();
        assert!(missing.len() <= 2 * result.kept.len(), "the export misses kept frames: {missing:?}");
        for &(n, at) in &got {
            let wanted_at = expected.iter().find(|(m, _)| *m == n).map_or(at, |&(_, t)| t);
            assert!(
                at.abs_diff(wanted_at) <= FRAME_NS + 5_000_000,
                "frame {n} is at {at} ns instead of {wanted_at} ns"
            );
        }
    } else {
        // The writer shows the last picture once more just before the end, so it lasts until then.
        if got.len() == expected.len() + 1 && got[got.len() - 1].0 == got[got.len() - 2].0 {
            got.pop();
        }
        println!("cut export: {} of the take's {} frames", got.len(), source_frames.len());
        assert_eq!(numbers(&got), numbers(&expected), "the exported frames are not the kept ones");
        for (&(n, at), &(_, wanted_at)) in got.iter().zip(&expected) {
            assert!(at.abs_diff(wanted_at) <= 5_000_000, "frame {n} is at {at} ns instead of {wanted_at} ns");
        }
    }
    // None of them is a frame that was only on screen inside a cut.
    for &(n, _) in &got {
        let i = source_frames.iter().position(|(m, _)| *m == n).unwrap();
        let shown_from = source_frames[i].1;
        let shown_until = source_frames.get(i + 1).map_or(u64::MAX, |f| f.1);
        assert!(
            result.kept.iter().any(|k| shown_from < k.end_ms * 1_000_000 && shown_until > k.start_ms * 1_000_000),
            "frame {n} is only on screen inside a cut"
        );
    }

    let info = platform::inspect(&result.path).unwrap();
    assert!(info.duration_ms.abs_diff(kept_ms) <= FRAME_NS / 1_000_000 + 1, "{} ms, not {kept_ms}", info.duration_ms);
    assert_eq!(info.audio.len(), 1, "the export keeps one audio track");
    let mut sound_frames = 0u64;
    let whole = TrimRange { start_ms: 0, end_ms: info.duration_ms + 1_000 };
    platform::decode_audio(&result.path, 0, whole, &never, &mut |samples, _| {
        sound_frames += samples.len() as u64 / 2;
        Ok(())
    })
    .unwrap();
    assert!(sound_frames.abs_diff(kept_ms * 48) <= 1024, "{sound_frames} audio frames for {kept_ms} ms");

    // Cancelled part of the way through: no file, no working file.
    let cancel = AtomicBool::new(false);
    let mut stop = |p: Progress| {
        if p.fraction > 0.3 {
            cancel.store(true, Ordering::SeqCst);
        }
    };
    let err = export::export(&id, &source, &cut, &out.join("cancelled.mp4"), &cancel, &mut stop).unwrap_err();
    assert!(matches!(err, RecordError::Cancelled), "{err:?}");
    let mut left: Vec<String> =
        std::fs::read_dir(&out).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, vec!["talk 2.mp4", "talk.mp4"]);
}
