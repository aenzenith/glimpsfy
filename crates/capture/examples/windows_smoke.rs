//! A manual check of the Windows capture backend on a real machine, since nothing of it can run
//! where the app is developed.
//!
//! ```text
//! cargo run -p glimpsfy-capture --example windows_smoke [-- <folder>]
//! ```
//!
//! It lists the displays and the windows, saves a PNG of every display, of the frontmost window
//! and of a rectangle, records the primary display for three seconds with the system's audio and
//! the default microphone, records the frontmost window for one second, and opens the microphone
//! on its own. Every step prints what it saw and goes on after a failure, so one run shows
//! everything that does not work. The PNGs go into a new folder under `<folder>` (the temporary
//! folder when none is given).

fn main() {
    #[cfg(target_os = "windows")]
    smoke::run();
    #[cfg(not(target_os = "windows"))]
    eprintln!("This example checks the Windows backend and only runs on Windows.");
}

#[cfg(target_os = "windows")]
mod smoke {
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use glimpsfy_capture::{
        desktop, host_now, new_backend, open_microphone, AudioBuffer, AudioSource, CaptureBackend, CaptureError,
        CaptureOptions, CaptureStream, ColorSpace, Display, Frame, LogicalRect, MicrophoneSource, PngEffort,
        StreamConfig, StreamPixelFormat, StreamSink, StreamTarget, VideoBuffer, VideoFrame, WindowCaptureOptions,
        WindowInfo, WindowListOptions,
    };

    pub fn run() {
        let base = std::env::args().nth(1).map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let folder = base.join(format!("glimpsfy-smoke-{seconds}"));
        if let Err(error) = std::fs::create_dir_all(&folder) {
            println!("could not create {}: {error}", folder.display());
            return;
        }
        println!("output folder: {}", folder.display());

        let backend = new_backend();
        println!("permission: {:?}", backend.permission());
        backend.prepare();

        let displays = list_displays(&*backend);
        let windows = list_windows(&*backend);
        stills(&*backend, &displays, &windows, &folder);
        if let Some(primary) = displays.first() {
            println!("\n== stream of {} for 3 s ==", primary.name);
            stream(&*backend, StreamTarget::Display { display: primary.id }, Duration::from_secs(3), true);
        }
        if let Some(window) = frontmost(&windows) {
            println!("\n== stream of the window \"{}\" for 1 s ==", window.title.as_deref().unwrap_or(""));
            stream(&*backend, StreamTarget::Window { window: window.id }, Duration::from_secs(1), false);
        }
        microphone_alone();
    }

    fn list_displays(backend: &dyn CaptureBackend) -> Vec<Display> {
        println!("\n== displays ==");
        let displays = match backend.displays() {
            Ok(displays) => displays,
            Err(error) => {
                println!("FAILED: {error}");
                return Vec::new();
            }
        };
        for d in &displays {
            println!(
                "{:>10}  {:<28} frame {:>7.1},{:>7.1} {:>7.1}×{:<7.1} scale {:.2}  {}×{} px  {}{}{}",
                d.id.0,
                d.name,
                d.frame.x,
                d.frame.y,
                d.frame.width,
                d.frame.height,
                d.scale.0,
                d.pixel_size.width,
                d.pixel_size.height,
                if d.is_primary { "primary " } else { "" },
                if d.is_builtin { "built-in " } else { "" },
                d.refresh_rate.map(|hz| format!("{hz:.2} Hz")).unwrap_or_default(),
            );
            let pixels = desktop::physical_frame(d);
            println!("            pixels at {},{} {}×{}", pixels.x, pixels.y, pixels.width, pixels.height);
        }
        displays
    }

    fn list_windows(backend: &dyn CaptureBackend) -> Vec<WindowInfo> {
        println!("\n== windows, front to back ==");
        let started = Instant::now();
        let windows = match backend.windows(WindowListOptions::default()) {
            Ok(windows) => windows,
            Err(error) => {
                println!("FAILED: {error}");
                return Vec::new();
            }
        };
        println!("{} windows in {:.1} ms", windows.len(), started.elapsed().as_secs_f64() * 1e3);
        for w in windows.iter().take(40) {
            println!(
                "{:>3} {:>10} layer {:>2} alpha {:.2} {:>7.1},{:>7.1} {:>7.1}×{:<7.1} r {:>3.0}  {} — {}",
                w.z_index,
                w.id.0,
                w.layer,
                w.alpha,
                w.bounds.x,
                w.bounds.y,
                w.bounds.width,
                w.bounds.height,
                w.corner_radius.unwrap_or(-1.0),
                w.app_name,
                w.title.as_deref().unwrap_or(""),
            );
        }
        windows
    }

    /// The first ordinary window with some size, which is the one a person would call frontmost.
    fn frontmost(windows: &[WindowInfo]) -> Option<&WindowInfo> {
        windows.iter().find(|w| w.layer == 0 && w.alpha > 0.0 && w.bounds.width >= 160.0 && w.bounds.height >= 120.0)
    }

    fn save(frame: &Frame, path: &Path) {
        let opaque = frame.is_opaque();
        let black = frame.data.chunks_exact(4).all(|p| p[0] == 0 && p[1] == 0 && p[2] == 0);
        let written = frame
            .encode_png(PngEffort::Fast)
            .map_err(|e| e.to_string())
            .and_then(|png| std::fs::write(path, png).map_err(|e| e.to_string()));
        println!(
            "  {}×{} px, scale {:.2}, {}{} -> {}",
            frame.size.width,
            frame.size.height,
            frame.scale.0,
            if opaque { "opaque" } else { "has transparent pixels" },
            if black { ", ALL BLACK" } else { "" },
            match written {
                Ok(()) => path.display().to_string(),
                Err(error) => format!("NOT SAVED: {error}"),
            },
        );
    }

    fn stills(backend: &dyn CaptureBackend, displays: &[Display], windows: &[WindowInfo], folder: &Path) {
        println!("\n== stills ==");
        let ids: Vec<_> = displays.iter().map(|d| d.id).collect();
        let options = CaptureOptions { shows_cursor: true, exclude_own_windows: true };
        let started = Instant::now();
        match backend.capture_displays(&ids, &options) {
            Ok(frames) => {
                println!("{} displays in {:.1} ms", frames.len(), started.elapsed().as_secs_f64() * 1e3);
                for (id, frame) in &frames {
                    save(frame, &folder.join(format!("display-{}.png", id.0)));
                }
            }
            Err(error) => println!("displays FAILED: {error}"),
        }
        if let Some(report) = backend.take_report() {
            println!("  report: path {}, total {:.1} ms", report.path.as_str(), report.total.as_secs_f64() * 1e3);
        }

        match frontmost(windows) {
            Some(window) => {
                println!("frontmost window: {} — {}", window.app_name, window.title.as_deref().unwrap_or(""));
                let started = Instant::now();
                match backend.capture_window(window.id, &WindowCaptureOptions::default()) {
                    Ok(frame) => {
                        println!("  captured in {:.1} ms", started.elapsed().as_secs_f64() * 1e3);
                        save(&frame, &folder.join("window.png"));
                    }
                    Err(error) => println!("  FAILED: {error}"),
                }
            }
            None => println!("no ordinary window is on screen"),
        }

        if let Some(primary) = displays.first() {
            let rect = LogicalRect::new(primary.frame.x + 100.0, primary.frame.y + 100.0, 400.0, 300.0);
            match backend.capture_rect(rect, &CaptureOptions::default()) {
                Ok(frame) => {
                    println!("rectangle 400×300 points at 100,100 of {}:", primary.name);
                    save(&frame, &folder.join("rect.png"));
                }
                Err(error) => println!("rectangle FAILED: {error}"),
            }
        }
    }

    #[derive(Default)]
    struct AudioSeen {
        buffers: u64,
        frames: u64,
        rate: u32,
        channels: u16,
        peak: f32,
        first: Option<u64>,
    }

    #[derive(Default)]
    struct Seen {
        frames: u64,
        first: Option<u64>,
        last: Option<u64>,
        longest_gap: u64,
        size: (u32, u32),
        cpu: bool,
        system: AudioSeen,
        microphone: AudioSeen,
        stopped: Vec<String>,
    }

    struct Sink(Arc<Mutex<Seen>>);

    impl StreamSink for Sink {
        fn video(&self, frame: VideoFrame) {
            let mut seen = self.0.lock().unwrap_or_else(|e| e.into_inner());
            seen.frames += 1;
            if let Some(last) = seen.last {
                seen.longest_gap = seen.longest_gap.max(frame.timestamp.0.saturating_sub(last));
            }
            seen.first.get_or_insert(frame.timestamp.0);
            seen.last = Some(frame.timestamp.0);
            seen.size = (frame.size.width, frame.size.height);
            seen.cpu = matches!(frame.buffer, VideoBuffer::Cpu(_));
        }

        fn audio(&self, buffer: AudioBuffer) {
            let mut seen = self.0.lock().unwrap_or_else(|e| e.into_inner());
            let source = match buffer.source {
                AudioSource::System => &mut seen.system,
                AudioSource::Microphone => &mut seen.microphone,
            };
            source.buffers += 1;
            source.frames += (buffer.samples.len() / buffer.channels.max(1) as usize) as u64;
            source.rate = buffer.sample_rate;
            source.channels = buffer.channels;
            source.peak = buffer.samples.iter().fold(source.peak, |peak, s| peak.max(s.abs()));
            source.first.get_or_insert(buffer.timestamp.0);
        }

        fn stopped(&self, error: Option<CaptureError>) {
            let mut seen = self.0.lock().unwrap_or_else(|e| e.into_inner());
            seen.stopped.push(error.map(|e| e.to_string()).unwrap_or_else(|| "stopped".into()));
        }
    }

    fn decibels(peak: f32) -> String {
        if peak <= 0.0 {
            "silent".into()
        } else {
            format!("{:.1} dBFS", 20.0 * peak.log10())
        }
    }

    fn print_audio(name: &str, audio: &AudioSeen, opened_at: u64) {
        if audio.buffers == 0 {
            println!("  {name}: no buffers");
            return;
        }
        println!(
            "  {name}: {} buffers, {:.2} s at {} Hz × {}, peak {}, first {:+.1} ms after the start",
            audio.buffers,
            audio.frames as f64 / audio.rate.max(1) as f64,
            audio.rate,
            audio.channels,
            decibels(audio.peak),
            (audio.first.unwrap_or(opened_at) as f64 - opened_at as f64) / 1e6,
        );
    }

    fn stream(backend: &dyn CaptureBackend, target: StreamTarget, length: Duration, audio: bool) {
        let config = |microphone: bool| StreamConfig {
            target: target.clone(),
            fps: 30,
            output_size: None,
            size_divisor: 1,
            shows_cursor: true,
            capture_system_audio: audio,
            microphone: (audio && microphone).then_some(MicrophoneSource { device_id: None }),
            excluded_windows: Vec::new(),
            shown_windows: Vec::new(),
            color_space: ColorSpace::Srgb,
            pixel_format: StreamPixelFormat::Bgra,
            queue_depth: 3,
        };
        let seen = Arc::new(Mutex::new(Seen::default()));
        let opened = backend.open_stream(config(true), Box::new(Sink(seen.clone()))).or_else(|error| {
            println!("  with the microphone: {error}; trying without it");
            backend.open_stream(config(false), Box::new(Sink(seen.clone())))
        });
        let mut stream: Box<dyn CaptureStream> = match opened {
            Ok(stream) => stream,
            Err(error) => {
                println!("  open FAILED: {error}");
                return;
            }
        };
        let starting = Instant::now();
        if let Err(error) = stream.start() {
            println!("  start FAILED: {error}");
            return;
        }
        let started_at = host_now().0;
        println!("  started in {:.1} ms", starting.elapsed().as_secs_f64() * 1e3);
        std::thread::sleep(length);
        let stopping = Instant::now();
        if let Err(error) = stream.stop() {
            println!("  stop FAILED: {error}");
        }
        println!("  stopped in {:.1} ms", stopping.elapsed().as_secs_f64() * 1e3);

        let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
        let span = seen.last.zip(seen.first).map(|(last, first)| last - first).unwrap_or(0);
        println!(
            "  video: {} frames of {}×{} ({}), first {:+.1} ms after the start, {:.2} s from first to last, longest gap {:.1} ms",
            seen.frames,
            seen.size.0,
            seen.size.1,
            if seen.cpu { "pixels in memory" } else { "a native buffer" },
            (seen.first.unwrap_or(started_at) as f64 - started_at as f64) / 1e6,
            span as f64 / 1e9,
            seen.longest_gap as f64 / 1e6,
        );
        println!("         (a still screen sends few frames; move a window while this runs to see the rate)");
        if audio {
            print_audio("system audio", &seen.system, started_at);
            print_audio("microphone", &seen.microphone, started_at);
        }
        println!("  end: {}", seen.stopped.join(", "));
    }

    fn microphone_alone() {
        println!("\n== the default microphone on its own for 1 s ==");
        let seen = Arc::new(Mutex::new(Seen::default()));
        let mut stream = match open_microphone(MicrophoneSource { device_id: None }, Box::new(Sink(seen.clone()))) {
            Ok(stream) => stream,
            Err(error) => {
                println!("  open FAILED: {error}");
                return;
            }
        };
        if let Err(error) = stream.start() {
            println!("  start FAILED: {error}");
            return;
        }
        let started_at = host_now().0;
        std::thread::sleep(Duration::from_secs(1));
        if let Err(error) = stream.stop() {
            println!("  stop FAILED: {error}");
        }
        let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
        print_audio("microphone", &seen.microphone, started_at);
        println!("  end: {}", seen.stopped.join(", "));
    }
}
