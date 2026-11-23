//! Finds the QR codes and barcodes of image files and prints each with its box and the time the
//! search took: with Vision on macOS, with the `barcodes` module everywhere else. `--compose`
//! first draws a full-HD screen with small, inverted and ordinary codes from the samples and
//! writes it as PNG, JPEG and WebP, which checks the search on a large image and `load_image`
//! on every format it reads.
//!
//! ```sh
//! cargo run -p glimpsfy-ocr --example codes --release -- tests/samples/qr.png
//! cargo run -p glimpsfy-ocr --example codes --release -- --compose /tmp/codes
//! ```

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use glimpsfy_capture::{ColorInfo, Frame, ImageEncoding, PhysicalSize, PixelFormat, PngEffort, ScaleFactor};
use glimpsfy_ocr::{load_image, CancelToken, CodeResult};

/// How often each image is searched; the fastest run is printed, as the least disturbed one.
const RUNS: usize = 5;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let files: Vec<PathBuf> = match args.as_slice() {
        [flag, folder] if flag == "--compose" => match compose(Path::new(folder)) {
            Ok(files) => files,
            Err(error) => fail(&format!("{folder}: {error}")),
        },
        [] => fail("usage: codes <image>... | codes --compose <folder>"),
        paths => paths.iter().map(PathBuf::from).collect(),
    };
    for file in files {
        let frame = match load_image(&file) {
            Ok(frame) => frame,
            Err(error) => fail(&format!("{}: {error}", file.display())),
        };
        let mut fastest = Duration::MAX;
        let mut codes = Vec::new();
        for _ in 0..RUNS {
            let started = Instant::now();
            codes = match detect(&frame) {
                Ok(codes) => codes,
                Err(error) => fail(&format!("{}: {error}", file.display())),
            };
            fastest = fastest.min(started.elapsed());
        }
        println!(
            "{}: {} × {} px, {} codes in {:.1} ms",
            file.display(),
            frame.size.width,
            frame.size.height,
            codes.len(),
            fastest.as_secs_f64() * 1000.0
        );
        for code in codes {
            let b = code.bounds;
            println!(
                "  {:?} {:?} at {:.0},{:.0} {:.0}×{:.0} link={:?}",
                code.symbology, code.payload, b.x, b.y, b.width, b.height, code.url
            );
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1)
}

#[cfg(target_os = "macos")]
fn detect(frame: &Frame) -> glimpsfy_ocr::Result<Vec<CodeResult>> {
    glimpsfy_ocr::detect_codes(&*glimpsfy_ocr::default_engine(), frame, &CancelToken::new())
}

#[cfg(not(target_os = "macos"))]
fn detect(frame: &Frame) -> glimpsfy_ocr::Result<Vec<CodeResult>> {
    glimpsfy_ocr::barcodes::detect(frame, &CancelToken::new())
}

/// Draws the test screen into `folder` in every format `load_image` reads, and names the files.
fn compose(folder: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let samples = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples/src");
    let qr = load_image(&samples.join("qr-code.png"))?;
    let barcode = load_image(&samples.join("code128.png"))?;

    let mut screen = Screen::new(1920, 1080);
    // A light page with rows of grey "text" and a dark side panel, so the codes are small
    // things among many edges, as on a real screen.
    screen.fill(0, 0, 1920, 1080, [246, 246, 248]);
    screen.fill(0, 0, 420, 1080, [24, 24, 28]);
    let mut seed = 0x2545_f491u32;
    for row in 0..58 {
        let mut x = 470;
        while x < 1700 {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let length = 12 + (seed >> 24) % 90;
            screen.fill(x, 40 + row * 17, length, 9, [70, 70, 78]);
            x += length + 8;
        }
    }
    // The samples are drawn at 8 px per module; every n-th pixel shrinks them without blur.
    // A QR code of 2 px modules in the top-right corner, one of 3 px modules light on dark in
    // the panel, the sample at full size, and the Code 128 at 2 px modules.
    screen.paste(&qr, 4, 1920 - 62 - 6, 6, false);
    screen.paste(&qr, 3, 40, 960, true);
    screen.paste(&qr, 1, 1000, 420, false);
    screen.paste(&barcode, 4, 1300, 940, false);

    std::fs::create_dir_all(folder)?;
    let frame = screen.frame()?;
    let mut files = Vec::new();
    for encoding in [
        ImageEncoding::Png(PngEffort::Fast),
        ImageEncoding::Jpeg { quality: 92, background: [255, 255, 255] },
        ImageEncoding::WebpLossless,
    ] {
        let file = folder.join(format!("screen.{}", encoding.extension()));
        std::fs::write(&file, frame.encode(encoding)?)?;
        files.push(file);
    }
    Ok(files)
}

struct Screen {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

impl Screen {
    fn new(width: u32, height: u32) -> Screen {
        Screen { width, height, rgba: vec![255; width as usize * height as usize * 4] }
    }

    fn set(&mut self, x: u32, y: u32, rgb: [u8; 3]) {
        if x < self.width && y < self.height {
            let at = (y as usize * self.width as usize + x as usize) * 4;
            self.rgba[at..at + 3].copy_from_slice(&rgb);
        }
    }

    fn fill(&mut self, x: u32, y: u32, width: u32, height: u32, rgb: [u8; 3]) {
        for row in y..y + height {
            for column in x..x + width {
                self.set(column, row, rgb);
            }
        }
    }

    /// Copies every `step`-th pixel of `image` to (`x`, `y`), with its colours inverted or not.
    fn paste(&mut self, image: &Frame, step: u32, x: u32, y: u32, inverted: bool) {
        for row in 0..image.size.height / step {
            for column in 0..image.size.width / step {
                let Some([r, g, b, _]) = image.rgba_at(column * step + step / 2, row * step + step / 2) else {
                    continue;
                };
                let rgb = if inverted { [255 - r, 255 - g, 255 - b] } else { [r, g, b] };
                self.set(x + column, y + row, rgb);
            }
        }
    }

    fn frame(self) -> glimpsfy_capture::Result<Frame> {
        Frame::new(
            PhysicalSize::new(self.width, self.height),
            self.width as usize * 4,
            PixelFormat::Rgba8,
            ColorInfo::srgb(),
            ScaleFactor::ONE,
            self.rgba,
        )
    }
}
