//! The recording pipeline end to end on macOS, without a screen: synthetic `CVPixelBuffer`s and
//! audio go through the session into the real `AVAssetWriter`, and AVFoundation reads the file
//! back. The export runs on that file.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use glimpsfy_capture::{
    AudioBuffer, AudioSource, CaptureError, CaptureStream, ColorSpace, DisplayId, HostTime, MacPixelBuffer,
    MicrophoneSource, NativeBuffer, PhysicalSize, StreamConfig, StreamPixelFormat, StreamSink, StreamTarget,
    VideoBuffer, VideoFrame,
};
use glimpsfy_record::export::{self, Progress};
use glimpsfy_record::files::Trash;
use glimpsfy_record::session::{Clock, Env, OwnWindows, Recorder, StreamOpener, SystemWriters};
use glimpsfy_record::{
    platform, AudioConfig, ClickHighlight, ExportAudio, ExportFormat, ExportOptions, ExportPurpose, KeystrokeOverlay,
    Mp4Quality, RecordError, RecordingConfig, RecordingId, RecordingOutcome, Resolution, TrackKind, TrimRange,
    VideoCodec, WebcamOverlay,
};
use objc2_core_foundation::CFRetained;
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

/// An NV12 frame with a bright square that moves with `n`, so the encoder has motion to code.
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
                *y.add(row * y_stride + col) = if inside { 235 } else { 16 + (col * 64 / W) as u8 };
            }
        }
        for row in 0..H / 2 {
            for col in 0..W / 2 {
                *uv.add(row * uv_stride + col * 2) = 128;
                *uv.add(row * uv_stride + col * 2 + 1) = if col > W / 4 { 160 } else { 128 };
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
            sink.video(VideoFrame {
                timestamp: HostTime(t),
                size: PhysicalSize { width: W as u32, height: H as u32 },
                format: StreamPixelFormat::Nv12,
                color_space: ColorSpace::Srgb,
                buffer: VideoBuffer::Native(NativeBuffer(Box::new(MacPixelBuffer(frame(*counter))))),
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
            std::thread::sleep(Duration::from_millis(4));
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
/// still opens and plays up to its last fragment.
#[test]
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
    ExportOptions { format, trim, destination: None, audio: ExportAudio::default(), purpose: ExportPurpose::Save }
}

/// Exports never replace a file, and a cancelled export leaves nothing under the final name or
/// any working file behind. Pass-through, re-encode and GIF all produce playable files.
#[test]
fn exports_never_replace_and_cancel_cleanly() {
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
