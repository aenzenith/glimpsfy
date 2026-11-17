//! Measures the character error rate of the default engine over the sample set in
//! `tests/samples`, with and without each preparation step and with several language choices.
//!
//! ```sh
//! cargo run -p glimpsfy-ocr --example accuracy --release           # every table
//! cargo run -p glimpsfy-ocr --example accuracy --release -- code   # one sample, with its text
//! ```
//!
//! The error rate is the edit distance between the recognised text and the expected text,
//! divided by the expected text's length. Both texts are compared after trailing spaces are
//! trimmed and blank lines are dropped, so the score counts characters, their order, line breaks
//! and indentation, and not how many blank lines separate paragraphs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use glimpsfy_capture::{Frame, PhysicalSize, PixelFormat};
use glimpsfy_ocr::{default_engine, recognize, CancelToken, LanguageTag, OcrRequest, Preparation};

struct Variant {
    name: &'static str,
    preparation: Preparation,
    languages: &'static [&'static str],
    detect: bool,
    half: bool,
}

const DEFAULT_LANGUAGES: &[&str] = &["en", "tr"];

fn variants() -> Vec<Variant> {
    let base = Preparation::default();
    vec![
        Variant { name: "default", preparation: base, languages: DEFAULT_LANGUAGES, detect: true, half: false },
        Variant {
            name: "no prep",
            preparation: Preparation::none(),
            languages: DEFAULT_LANGUAGES,
            detect: true,
            half: false,
        },
        Variant {
            name: "no invert",
            preparation: Preparation { invert_dark: false, ..base },
            languages: DEFAULT_LANGUAGES,
            detect: true,
            half: false,
        },
        Variant {
            name: "no upscale",
            preparation: Preparation { upscale_small_text: false, ..base },
            languages: DEFAULT_LANGUAGES,
            detect: true,
            half: false,
        },
        Variant {
            name: "correction everywhere",
            preparation: Preparation { code_without_correction: false, ..base },
            languages: DEFAULT_LANGUAGES,
            detect: true,
            half: false,
        },
        Variant {
            name: "no correction",
            preparation: Preparation { language_correction: false, ..base },
            languages: DEFAULT_LANGUAGES,
            detect: true,
            half: false,
        },
        Variant {
            name: "upscale < 16 px",
            preparation: Preparation { small_text_px: 16.0, target_text_px: 36.0, ..base },
            languages: DEFAULT_LANGUAGES,
            detect: true,
            half: false,
        },
        Variant { name: "half res", preparation: base, languages: DEFAULT_LANGUAGES, detect: true, half: true },
        Variant {
            name: "half, no prep",
            preparation: Preparation::none(),
            languages: DEFAULT_LANGUAGES,
            detect: true,
            half: true,
        },
    ]
}

fn language_variants() -> Vec<Variant> {
    let base = Preparation::default();
    let v = |name, languages, detect| Variant { name, preparation: base, languages, detect, half: false };
    vec![
        v("auto only", &[], true),
        v("en", &["en"], false),
        v("tr", &["tr"], false),
        v("tr, en", &["tr", "en"], false),
        v("en, tr", &["en", "tr"], false),
        v("auto + tr, en", &["tr", "en"], true),
        v("auto + en, tr", &["en", "tr"], true),
    ]
}

fn main() {
    let only = std::env::args().nth(1);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples");
    let mut samples: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("the sample folder exists")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "png") && p.with_extension("txt").exists())
        .collect();
    samples.sort();
    let engine = default_engine();

    if let Some(name) = only {
        let path = dir.join(format!("{name}.png"));
        for variant in variants().iter().chain(language_variants().iter()) {
            let (text, cer, ms) = run(&*engine, &path, variant);
            println!("--- {} · CER {:.2} % · {} ms\n{}\n", variant.name, cer * 100.0, ms, text);
        }
        return;
    }

    for (title, list) in [("Preparation", variants()), ("Languages", language_variants())] {
        println!("\n## {title}\n");
        print!("| Sample |");
        for v in &list {
            print!(" {} |", v.name);
        }
        println!();
        print!("|---|");
        for _ in &list {
            print!("---:|");
        }
        println!();
        let mut sums = vec![0.0; list.len()];
        let mut times = vec![0u128; list.len()];
        for path in &samples {
            let name = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            print!("| {name} |");
            for (i, v) in list.iter().enumerate() {
                let (_, cer, ms) = run(&*engine, path, v);
                sums[i] += cer;
                times[i] += ms;
                print!(" {:.1} |", cer * 100.0);
            }
            println!();
        }
        print!("| **mean** |");
        for s in &sums {
            print!(" **{:.2}** |", s / samples.len() as f64 * 100.0);
        }
        println!();
        print!("| time, all samples (ms) |");
        for t in &times {
            print!(" {t} |");
        }
        println!();
    }

    println!("\n## Codes\n");
    let codes_path = dir.join("codes.json");
    let expected: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(codes_path).expect("codes.json exists")).expect("valid JSON");
    for (name, codes) in expected.as_object().expect("an object") {
        let frame = glimpsfy_ocr::load_image(&dir.join(format!("{name}.png"))).expect("the sample loads");
        let found = glimpsfy_ocr::detect_codes(&*engine, &frame, &CancelToken::new()).expect("detection runs");
        for code in codes.as_array().expect("a list") {
            let payload = code[1].as_str().unwrap_or_default();
            let hit = found.iter().find(|c| c.payload == payload);
            println!(
                "- {name}: {} {payload} → {}",
                code[0].as_str().unwrap_or_default(),
                match hit {
                    Some(c) => format!("found as {:?}, link {:?}", c.symbology, c.url),
                    None => "MISSING".into(),
                }
            );
        }
    }
}

fn run(engine: &dyn glimpsfy_ocr::OcrEngine, path: &Path, variant: &Variant) -> (String, f64, u128) {
    let mut frame = glimpsfy_ocr::load_image(path).expect("the sample loads");
    if variant.half {
        frame = half(&frame);
    }
    let mut request = OcrRequest::new(Arc::new(frame));
    request.languages = variant.languages.iter().map(|l| LanguageTag::new(*l)).collect();
    request.detect_language = variant.detect;
    request.prepare = variant.preparation;
    let started = Instant::now();
    let result = recognize(engine, &request).expect("recognition runs");
    let ms = started.elapsed().as_millis();
    let expected = std::fs::read_to_string(path.with_extension("txt")).expect("the expected text exists");
    let cer = error_rate(&normalize(&result.text), &normalize(&expected));
    (result.text, cer, ms)
}

/// Trims trailing spaces and drops blank lines.
fn normalize(text: &str) -> Vec<char> {
    text.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n").chars().collect()
}

fn error_rate(got: &[char], want: &[char]) -> f64 {
    let mut row: Vec<usize> = (0..=want.len()).collect();
    for (i, g) in got.iter().enumerate() {
        let mut previous = row[0];
        row[0] = i + 1;
        for (j, w) in want.iter().enumerate() {
            let substitution = previous + usize::from(g != w);
            previous = row[j + 1];
            row[j + 1] = substitution.min(row[j] + 1).min(row[j + 1] + 1);
        }
    }
    row[want.len()] as f64 / want.len().max(1) as f64
}

/// The image at half its size (2 × 2 box filter): what reading at logical points instead of
/// physical pixels would see on a Retina display.
fn half(frame: &Frame) -> Frame {
    let rgba = frame.to_rgba();
    let (w, h) = (frame.size.width as usize, frame.size.height as usize);
    let (nw, nh) = (w / 2, h / 2);
    let mut data = vec![0u8; nw * nh * 4];
    for y in 0..nh {
        for x in 0..nw {
            for c in 0..4 {
                let at = |dx: usize, dy: usize| rgba[((2 * y + dy) * w + 2 * x + dx) * 4 + c] as u32;
                data[(y * nw + x) * 4 + c] = ((at(0, 0) + at(1, 0) + at(0, 1) + at(1, 1) + 2) / 4) as u8;
            }
        }
    }
    Frame {
        size: PhysicalSize::new(nw as u32, nh as u32),
        bytes_per_row: nw * 4,
        format: PixelFormat::Rgba8,
        color: frame.color.clone(),
        scale: frame.scale,
        data,
    }
}
