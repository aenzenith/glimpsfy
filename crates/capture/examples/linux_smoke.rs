//! A smoke test of the Linux capture backend on a real desktop (or in Xvfb): it lists displays
//! and windows, saves a PNG of each display and of the frontmost window, runs a display stream
//! and an area stream, and opens the system audio and the microphone.
//!
//!   cargo run -p glimpsfy-capture --example linux_smoke -- [--out DIR] [--seconds N] [--fps N]
//!       [--probe X,Y]... [--state DIR] [--no-audio] [--no-streams] [--sound-only]
//!       [--repeat N]
//!
//! `--probe` prints the colour of a pixel of the primary display's frame. `paint` and `warp X Y`
//! are aids for checking the backend against known content: `paint` opens windows of known
//! colours and stays until it is killed, `warp` moves the pointer.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("linux_smoke only runs on Linux.");
}

#[cfg(target_os = "linux")]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(String::as_str) {
        Some("paint") => linux::paint(),
        Some("warp") => linux::warp(&args[1..]),
        _ => linux::smoke(&args),
    };
    if let Err(error) = code {
        eprintln!("linux_smoke failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::error::Error;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use glimpsfy_capture::{
        host_now, linux_diagnostics, linux_microphones, linux_session, linux_set_state_dir,
        linux_system_audio_available, new_backend, open_microphone, AudioBuffer, AudioSource, CaptureBackend,
        CaptureError, CaptureOptions, ColorSpace, Display, Frame, LogicalRect, MicrophoneSource, PhysicalSize,
        PngEffort, StreamConfig, StreamPixelFormat, StreamSink, StreamTarget, VideoBuffer, VideoFrame,
        WindowCaptureOptions, WindowListOptions,
    };
    use x11rb::connection::Connection;
    use x11rb::properties::{WmSizeHints, WmSizeHintsSpecification};
    use x11rb::protocol::xproto::{
        AtomEnum, ColormapAlloc, ConnectionExt as _, CreateWindowAux, PropMode, VisualClass, WindowClass,
    };
    use x11rb::wrapper::ConnectionExt as _;

    type Outcome = Result<(), Box<dyn Error>>;

    /// What a stream delivered, kept for the report.
    #[derive(Default)]
    struct Seen {
        /// (the frame's stamp, its arrival on the same clock, its size, its first pixel as BGRA)
        video: Vec<(u64, u64, PhysicalSize, [u8; 4])>,
        /// (source, stamp, frames, rate, channels, peak)
        audio: Vec<(AudioSource, u64, usize, u32, u16, f32)>,
        stopped: Option<Option<String>>,
    }

    struct Collector(Arc<Mutex<Seen>>);

    impl StreamSink for Collector {
        fn video(&self, frame: VideoFrame) {
            let first = match &frame.buffer {
                VideoBuffer::Cpu(cpu) => cpu.data.get(..4).map(|p| [p[0], p[1], p[2], p[3]]).unwrap_or_default(),
                VideoBuffer::Native(_) => [0; 4],
            };
            self.0.lock().unwrap().video.push((frame.timestamp.0, host_now().0, frame.size, first));
        }

        fn audio(&self, buffer: AudioBuffer) {
            let peak = buffer.samples.iter().fold(0f32, |peak, s| peak.max(s.abs()));
            let frames = buffer.samples.len() / buffer.channels.max(1) as usize;
            self.0.lock().unwrap().audio.push((
                buffer.source,
                buffer.timestamp.0,
                frames,
                buffer.sample_rate,
                buffer.channels,
                peak,
            ));
        }

        fn stopped(&self, error: Option<CaptureError>) {
            self.0.lock().unwrap().stopped = Some(error.map(|e| e.to_string()));
        }
    }

    fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
        args.iter().position(|a| a == name).and_then(|at| args.get(at + 1)).map(String::as_str)
    }

    fn save(frame: &Frame, path: &Path) -> Outcome {
        std::fs::write(path, frame.encode_png(PngEffort::Fast)?)?;
        Ok(())
    }

    fn rgba(frame: &Frame, x: u32, y: u32) -> String {
        match frame.rgba_at(x, y) {
            Some([r, g, b, a]) => format!("#{r:02X}{g:02X}{b:02X} alpha {a}"),
            None => "outside the frame".into(),
        }
    }

    fn config(target: StreamTarget, fps: u32, system_audio: bool, microphone: bool) -> StreamConfig {
        StreamConfig {
            target,
            fps,
            output_size: None,
            size_divisor: 1,
            shows_cursor: true,
            capture_system_audio: system_audio,
            microphone: microphone.then_some(MicrophoneSource { device_id: None }),
            excluded_windows: Vec::new(),
            shown_windows: Vec::new(),
            color_space: ColorSpace::Srgb,
            pixel_format: StreamPixelFormat::Bgra,
            queue_depth: 6,
        }
    }

    /// Runs one stream for `seconds` and prints what it delivered.
    fn run_stream(backend: &dyn CaptureBackend, label: &str, config: StreamConfig, seconds: u64) {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let fps = config.fps;
        let mut stream = match backend.open_stream(config, Box::new(Collector(seen.clone()))) {
            Ok(stream) => stream,
            Err(error) => return println!("{label}: could not open: {error}"),
        };
        let started = host_now().0;
        if let Err(error) = stream.start() {
            return println!("{label}: could not start: {error}");
        }
        std::thread::sleep(Duration::from_secs(seconds));
        let stop = stream.stop();
        let ended = host_now().0;
        let seen = seen.lock().unwrap();
        report_video(label, &seen, fps, started, ended);
        report_audio(label, &seen, started, ended);
        println!("{label}: stop {stop:?}, the sink heard {:?}", seen.stopped);
    }

    fn report_video(label: &str, seen: &Seen, fps: u32, started: u64, ended: u64) {
        let Some(first) = seen.video.first() else { return println!("{label}: no video frames") };
        let stamps: Vec<u64> = seen.video.iter().map(|v| v.0).collect();
        let gaps: Vec<f64> = stamps.windows(2).map(|w| (w[1] as f64 - w[0] as f64) / 1e6).collect();
        let increasing = stamps.windows(2).all(|w| w[1] > w[0]);
        let inside = stamps.iter().all(|&t| t + 50_000_000 >= started && t <= ended);
        let latency = seen.video.iter().map(|v| (v.1 as f64 - v.0 as f64) / 1e6).fold(0f64, f64::max);
        let sizes_agree = seen.video.iter().all(|v| v.2 == first.2);
        let mean = gaps.iter().sum::<f64>() / gaps.len().max(1) as f64;
        let (min, max) = gaps.iter().fold((f64::MAX, 0f64), |(lo, hi), &g| (lo.min(g), hi.max(g)));
        println!(
            "{label}: {} frames of {}×{} (one size: {sizes_agree}) at {fps} fps asked; gaps mean {mean:.2} ms, \
             min {min:.2}, max {max:.2}; stamps increasing: {increasing}, inside the run: {inside}; \
             largest stamp-to-delivery {latency:.2} ms; first pixel BGRA {:?}",
            seen.video.len(),
            first.2.width,
            first.2.height,
            first.3
        );
    }

    fn report_audio(label: &str, seen: &Seen, started: u64, ended: u64) {
        for source in [AudioSource::System, AudioSource::Microphone] {
            let buffers: Vec<_> = seen.audio.iter().filter(|a| a.0 == source).collect();
            let (Some(first), Some(last)) = (buffers.first(), buffers.last()) else { continue };
            let frames: usize = buffers.iter().map(|b| b.2).sum();
            let peak = buffers.iter().fold(0f32, |peak, b| peak.max(b.5));
            let span = (last.1 - first.1) as f64 / 1e9 + last.2 as f64 / last.3.max(1) as f64;
            let increasing = buffers.windows(2).all(|w| w[1].1 > w[0].1);
            let inside = buffers.iter().all(|b| b.1 + 500_000_000 >= started && b.1 <= ended);
            let silent_tail = buffers.iter().rev().take_while(|b| b.5 == 0.0).count();
            println!(
                "{label}: {source:?} audio: {} buffers, {frames} frames at {} Hz × {} channels = {:.3} s of sound \
                 over {span:.3} s of stamps; peak {peak:.4}, the last {silent_tail} buffers silent; stamps increasing: \
                 {increasing}, inside the run: {inside}",
                buffers.len(),
                first.3,
                first.4,
                frames as f64 / first.3.max(1) as f64,
            );
        }
    }

    pub fn smoke(args: &[String]) -> Outcome {
        let out =
            option(args, "--out").map(PathBuf::from).unwrap_or_else(|| std::env::temp_dir().join("glimpsfy-smoke"));
        let seconds: u64 = option(args, "--seconds").and_then(|s| s.parse().ok()).unwrap_or(3);
        let fps: u32 = option(args, "--fps").and_then(|s| s.parse().ok()).unwrap_or(30);
        let has = |name: &str| args.iter().any(|a| a == name);
        std::fs::create_dir_all(&out)?;
        if let Some(state) = option(args, "--state") {
            linux_set_state_dir(state);
        }

        println!("{}", linux_diagnostics());
        let backend = new_backend();
        println!("session {:?}, permission {:?}", linux_session(), backend.permission());
        backend.prepare();

        // `--repeat N` only captures the primary display once a second, N times, and says how
        // each went: the way to watch the backend find a restarted X server again.
        if let Some(times) = option(args, "--repeat").and_then(|n| n.parse::<u32>().ok()) {
            for round in 1..=times {
                let taken = backend.displays().and_then(|displays| {
                    let primary = displays.first().ok_or(CaptureError::Backend("no display".into()))?;
                    backend.capture_display(primary.id, &CaptureOptions::default())
                });
                match taken {
                    Ok(frame) => println!(
                        "round {round}: {}×{} corner {}",
                        frame.size.width,
                        frame.size.height,
                        rgba(&frame, 0, 0)
                    ),
                    Err(error) => println!("round {round}: {error}"),
                }
                std::thread::sleep(Duration::from_secs(1));
            }
            return Ok(());
        }

        let displays: Vec<Display> = backend.displays()?;
        for d in &displays {
            println!(
                "display {} {:?}: frame {},{} {}×{} points, scale {}, {}×{} px, primary {}, builtin {}, {:?} Hz",
                d.id.0,
                d.name,
                d.frame.x,
                d.frame.y,
                d.frame.width,
                d.frame.height,
                d.scale.0,
                d.pixel_size.width,
                d.pixel_size.height,
                d.is_primary,
                d.is_builtin,
                d.refresh_rate
            );
        }
        let windows = backend.windows(WindowListOptions::default())?;
        for w in &windows {
            println!(
                "window {:#x} z{} layer {} {:?} of {:?} (pid {}): {},{} {}×{} points, alpha {}",
                w.id.0,
                w.z_index,
                w.layer,
                w.title,
                w.app_name,
                w.pid,
                w.bounds.x,
                w.bounds.y,
                w.bounds.width,
                w.bounds.height,
                w.alpha
            );
        }

        let ids: Vec<_> = displays.iter().map(|d| d.id).collect();
        let at = std::time::Instant::now();
        let frames = backend.capture_displays(&ids, &CaptureOptions::default())?;
        println!(
            "captured {} display(s) in {:.1} ms; permission now {:?}",
            frames.len(),
            at.elapsed().as_secs_f64() * 1e3,
            backend.permission()
        );
        for (id, frame) in &frames {
            let path = out.join(format!("display-{}.png", id.0));
            save(frame, &path)?;
            println!(
                "display {} frame {}×{} scale {} opaque {} centre {} -> {}",
                id.0,
                frame.size.width,
                frame.size.height,
                frame.scale.0,
                frame.is_opaque(),
                rgba(frame, frame.size.width / 2, frame.size.height / 2),
                path.display()
            );
        }
        if let Some((_, primary)) = frames.first() {
            for index in args.iter().enumerate().filter(|(_, a)| *a == "--probe").map(|(index, _)| index) {
                let Some((x, y)) = args.get(index + 1).and_then(|v| v.split_once(',')) else { continue };
                let (x, y): (u32, u32) = (x.parse()?, y.parse()?);
                println!("probe {x},{y}: {}", rgba(primary, x, y));
            }
            // Displays listed after a capture carry the measured pixel sizes (Wayland).
            for d in backend.displays()? {
                println!(
                    "display {} after the capture: {}×{} px, scale {}",
                    d.id.0, d.pixel_size.width, d.pixel_size.height, d.scale.0
                );
            }
            // The pointer: the same display with and without it differs only where it is drawn.
            let with =
                backend.capture_display(ids[0], &CaptureOptions { shows_cursor: true, exclude_own_windows: true })?;
            let without = backend.capture_display(ids[0], &CaptureOptions::default())?;
            let differing = with.data.chunks_exact(4).zip(without.data.chunks_exact(4)).filter(|(a, b)| a != b).count();
            println!("pointer: {differing} pixels differ between a capture with it and one without");
        }

        // Every ordinary window (the frontmost four), without and with its shadow.
        for window in windows.iter().filter(|w| w.layer == 0).take(4) {
            for (name, shadow) in [("window", false), ("window-shadow", true)] {
                let options = WindowCaptureOptions { include_shadow: shadow, ..WindowCaptureOptions::default() };
                match backend.capture_window(window.id, &options) {
                    Ok(frame) => {
                        let path = out.join(format!("{name}-{:x}.png", window.id.0));
                        save(&frame, &path)?;
                        let (w, h) = (frame.size.width, frame.size.height);
                        println!(
                            "{name} {:#x}: {w}×{h} opaque {} corner {} centre {} lower right {} -> {}",
                            window.id.0,
                            frame.is_opaque(),
                            rgba(&frame, 0, 0),
                            rgba(&frame, w / 2, h / 2),
                            rgba(&frame, w * 85 / 100, h * 85 / 100),
                            path.display()
                        );
                    }
                    Err(error) => println!("{name} {:#x}: {error}", window.id.0),
                }
            }
        }
        if windows.is_empty() {
            match backend.capture_window(glimpsfy_capture::WindowId(0), &WindowCaptureOptions::default()) {
                Ok(_) => println!("window: no window is listed, yet window 0 was captured"),
                Err(error) => println!("window: none listed; capturing one answers: {error}"),
            }
        }

        let audio = !has("--no-audio");
        if !has("--no-streams") {
            if let Some(primary) = displays.first() {
                let target = StreamTarget::Display { display: primary.id };
                if !has("--sound-only") {
                    run_stream(&*backend, "display stream", config(target.clone(), fps, false, false), seconds);
                    let rect = LogicalRect::new(100.0, 50.0, 321.0, 241.0);
                    let area = StreamTarget::Area { display: primary.id, rect };
                    run_stream(
                        &*backend,
                        "area stream (321×241 points at 100,50)",
                        config(area, fps, false, false),
                        seconds,
                    );
                    let mut half = config(target.clone(), fps, false, false);
                    half.size_divisor = 2;
                    run_stream(&*backend, "half-size stream", half, 1);
                    if let Some(window) = windows.iter().find(|w| w.layer == 0) {
                        let target = StreamTarget::Window { window: window.id };
                        run_stream(&*backend, "window stream", config(target, fps, false, false), 1);
                    }
                }
                if audio {
                    run_stream(&*backend, "stream with sound", config(target.clone(), fps, true, false), seconds);
                    run_stream(&*backend, "stream with sound and microphone", config(target, fps, true, true), 1);
                }
            }
        }

        if audio {
            println!("system audio available: {}", linux_system_audio_available());
            match linux_microphones() {
                Ok(microphones) => {
                    for (id, name) in &microphones {
                        println!("microphone {id:?}: {name:?}");
                    }
                    if microphones.is_empty() {
                        println!("no microphone is listed");
                    }
                }
                Err(error) => println!("microphones: {error}"),
            }
            let seen = Arc::new(Mutex::new(Seen::default()));
            match open_microphone(MicrophoneSource { device_id: None }, Box::new(Collector(seen.clone()))) {
                Ok(mut microphone) => {
                    let started = host_now().0;
                    let start = microphone.start();
                    std::thread::sleep(Duration::from_secs(seconds.min(2)));
                    let stop = microphone.stop();
                    println!("default microphone: start {start:?}, stop {stop:?}");
                    report_audio("default microphone", &seen.lock().unwrap(), started, host_now().0);
                }
                Err(error) => println!("default microphone: {error}"),
            }
            let missing = MicrophoneSource { device_id: Some("glimpsfy-no-such-source".into()) };
            match open_microphone(missing, Box::new(Collector(Arc::new(Mutex::new(Seen::default()))))) {
                Ok(_) => println!("a microphone that does not exist opened, which is wrong"),
                Err(error) => println!("a microphone that does not exist: {error}"),
            }
        }
        Ok(())
    }

    /// Opens a 300×200 window filled #3366CC with a red 20×20 square in its corner and, when the
    /// server has a 32-bit visual, a 200×150 window of half-transparent blue (premultiplied
    /// 0x80000080) over its lower right part. Stays until it is killed.
    pub fn paint() -> Outcome {
        let (conn, screen) = x11rb::connect(None)?;
        let screen = &conn.setup().roots[screen];
        let name = |window: u32, title: &str| -> Outcome {
            conn.change_property8(PropMode::REPLACE, window, AtomEnum::WM_NAME, AtomEnum::STRING, title.as_bytes())?;
            conn.change_property8(
                PropMode::REPLACE,
                window,
                AtomEnum::WM_CLASS,
                AtomEnum::STRING,
                b"paint\0GlimpsfyPaint\0",
            )?;
            Ok(())
        };
        // A position the user is said to have chosen, which a window manager keeps.
        let place = |window: u32, x: i32, y: i32| -> Outcome {
            let hints = WmSizeHints {
                position: Some((WmSizeHintsSpecification::UserSpecified, x, y)),
                ..WmSizeHints::default()
            };
            hints.set_normal_hints(&conn, window)?;
            Ok(())
        };
        let solid = conn.generate_id()?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            solid,
            screen.root,
            100,
            100,
            300,
            200,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(0x33_66CC),
        )?;
        name(solid, "Glimpsfy paint")?;
        place(solid, 100, 100)?;
        let square = conn.generate_id()?;
        conn.create_window(
            x11rb::COPY_DEPTH_FROM_PARENT,
            square,
            solid,
            0,
            0,
            20,
            20,
            0,
            WindowClass::INPUT_OUTPUT,
            x11rb::COPY_FROM_PARENT,
            &CreateWindowAux::new().background_pixel(0xFF_0000),
        )?;
        conn.map_window(square)?;
        conn.map_window(solid)?;

        let deep = screen
            .allowed_depths
            .iter()
            .find(|d| d.depth == 32)
            .and_then(|d| d.visuals.iter().find(|v| v.class == VisualClass::TRUE_COLOR));
        if let Some(visual) = deep {
            let colormap = conn.generate_id()?;
            conn.create_colormap(ColormapAlloc::NONE, colormap, screen.root, visual.visual_id)?;
            let glass = conn.generate_id()?;
            conn.create_window(
                32,
                glass,
                screen.root,
                250,
                200,
                200,
                150,
                0,
                WindowClass::INPUT_OUTPUT,
                visual.visual_id,
                &CreateWindowAux::new().background_pixel(0x8000_0080).border_pixel(0).colormap(colormap),
            )?;
            name(glass, "Glimpsfy glass")?;
            // Over the lower right part of the solid window, so that one is partly covered.
            place(glass, 250, 200)?;
            conn.map_window(glass)?;
            println!("paint: solid {solid:#x}, glass {glass:#x}");
        } else {
            println!("paint: solid {solid:#x}; the server has no 32-bit visual");
        }
        conn.flush()?;
        loop {
            conn.wait_for_event()?;
        }
    }

    pub fn warp(args: &[String]) -> Outcome {
        let (x, y): (i16, i16) = match args {
            [x, y, ..] => (x.parse()?, y.parse()?),
            _ => return Err("warp takes X and Y".into()),
        };
        let (conn, screen) = x11rb::connect(None)?;
        let root = conn.setup().roots[screen].root;
        conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, x, y)?.check()?;
        Ok(())
    }
}
