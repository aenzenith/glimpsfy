//! Records the main display for a few seconds through the real stream and writer, then exports
//! an MP4 and a GIF, and prints what each file holds. A developer tool for checking the whole
//! path on a Mac with the screen recording permission, on Windows, or on Linux (an X11 display,
//! Xvfb included, or a Wayland session through the screen cast portal):
//!
//! `cargo run -p glimpsfy-record --example record_screen -- <seconds> <folder> [h264|hevc] [mic]`
//!
//! The folder defaults to `glimpsfy-record` in the system's temporary folder. `hevc` asks for
//! HEVC, which falls back to H.264 where no HEVC encoder is installed, and `mic` adds the default
//! microphone next to the system audio. The devices the system lists are printed first.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use glimpsfy_capture::{new_backend, CaptureBackend, MicrophoneSource, StreamTarget};
use glimpsfy_record::export::{self, Progress};
use glimpsfy_record::files::Trash;
use glimpsfy_record::session::{Env, OwnWindows, Recorder, SystemClock, SystemWriters};
use glimpsfy_record::{
    platform, AudioConfig, ClickHighlight, ExportAudio, ExportFormat, ExportOptions, ExportPurpose, KeystrokeOverlay,
    Mp4Quality, RecordingConfig, RecordingId, RecordingOutcome, Resolution, VideoCodec, WebcamOverlay,
};

struct KeepEverything;
impl Trash for KeepEverything {
    fn trash(&self, _: &std::path::Path) -> std::io::Result<()> {
        Err(std::io::Error::other("this tool never discards"))
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let seconds: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(5);
    let folder = args.next().map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("glimpsfy-record"));
    let codec = match args.next().as_deref() {
        Some("hevc") => VideoCodec::Hevc,
        _ => VideoCodec::H264,
    };
    let microphone = args.next().as_deref() == Some("mic");
    std::fs::create_dir_all(&folder)?;
    println!("devices: {:#?}", platform::devices());

    let backend: Arc<dyn CaptureBackend> = new_backend();
    let display = backend.displays()?.into_iter().find(|d| d.is_primary).ok_or("no main display")?;
    let env = Env {
        streams: Arc::new(backend.clone()),
        writers: Arc::new(SystemWriters),
        clock: Arc::new(SystemClock),
        trash: Arc::new(KeepEverything),
        on_fault: Arc::new(|id| eprintln!("the take {id:?} failed on its own")),
    };
    let recorder = Recorder::new(env);
    let config = RecordingConfig {
        target: StreamTarget::Display { display: display.id },
        fps: 30,
        resolution: Resolution::Native,
        codec,
        shows_cursor: true,
        audio: AudioConfig {
            system: true,
            microphone: microphone.then_some(MicrophoneSource { device_id: None }),
            separate_tracks: true,
        },
        clicks: ClickHighlight::default(),
        keystrokes: KeystrokeOverlay::default(),
        webcam: WebcamOverlay::default(),
        countdown_seconds: 0,
        output_folder: folder.clone(),
        clean_desktop: false,
    };
    let path = glimpsfy_record::files::free_path(&folder, "Screen", "mp4");
    let started = Instant::now();
    recorder.begin(RecordingId::new(), config, path.clone(), &OwnWindows::default())?;
    println!("the stream started in {:?}", started.elapsed());
    recorder.go()?;
    std::thread::sleep(Duration::from_secs(seconds));
    let stopping = Instant::now();
    let outcome = recorder.stop()?;
    println!("the file was finished in {:?}", stopping.elapsed());
    let RecordingOutcome::Saved { result } = outcome else {
        return Err(format!("the take did not save: {outcome:?}").into());
    };
    let info = platform::inspect(&result.path)?;
    println!("recorded: {info:?}");
    let frames = platform::video_timestamps(&result.path)?;
    println!(
        "frames: {} (gaps over 100 ms: {})",
        frames.len(),
        frames.windows(2).filter(|w| w[1] - w[0] > 100_000_000).count()
    );

    let poster = platform::frame_at(&result.path, 0, 720)?;
    let thumb = platform::frame_at(&result.path, result.duration_ms / 2, 90)?;
    let peaks = platform::waveform(&result.path, 64)?;
    println!(
        "poster {}×{}, thumbnail {}×{}, waveform of {} bins peaking at {:.3}",
        poster.size.width,
        poster.size.height,
        thumb.size.width,
        thumb.size.height,
        peaks.len(),
        peaks.iter().fold(0f32, |a, &b| a.max(b))
    );

    let never = AtomicBool::new(false);
    let mut quiet = |_: Progress| {};
    let exports = [
        ("high", ExportFormat::Mp4 { resolution: Resolution::Native, quality: Mp4Quality::High }),
        ("medium", ExportFormat::Mp4 { resolution: Resolution::Native, quality: Mp4Quality::Medium }),
        ("half-low", ExportFormat::Mp4 { resolution: Resolution::Half, quality: Mp4Quality::Low }),
        ("gif-640", ExportFormat::Gif { fps: 15, width: Some(640), loop_forever: true }),
    ];
    for (name, format) in exports {
        let options = ExportOptions {
            format: format.clone(),
            trim: None,
            destination: None,
            audio: ExportAudio::default(),
            purpose: ExportPurpose::Save,
            cuts: Vec::new(),
        };
        let wanted = export::wanted_path(&folder, &result.path, &format, false);
        let wanted =
            wanted.with_file_name(format!("{name}-{}", wanted.file_name().unwrap_or_default().to_string_lossy()));
        let t = Instant::now();
        let out = export::export(&result.id, &result.path, &options, &wanted, &never, &mut quiet)?;
        let px_frames = out.size.width as f64 * out.size.height as f64 * (out.duration_ms as f64 / 1000.0);
        println!(
            "{name}: {} bytes in {:?}, {}×{}, {:.4} bytes per pixel-second",
            out.file_bytes,
            t.elapsed(),
            out.size.width,
            out.size.height,
            out.file_bytes as f64 / px_frames
        );
    }
    Ok(())
}
