//! Reads one image file with the default preparation and prints the result and the codes as the
//! commands return them (JSON), or the text alone with `--text`. The engine is the platform's
//! default, or PP-OCR with the models of a folder (which needs the `ppocr` feature). The languages
//! are English and Turkish unless `--languages` names others, the most likely first.
//!
//! ```sh
//! cargo run -p glimpsfy-ocr --example recognize -- tests/samples/qr.png
//! cargo run -p glimpsfy-ocr --example recognize -- tests/samples/code.png --text
//! cargo run -p glimpsfy-ocr --features ppocr --example recognize -- shot.png --text --models /tmp/models
//! cargo run -p glimpsfy-ocr --features ppocr --example recognize -- ru.png --models /tmp/models --languages ru,en
//! ```

use std::path::Path;
use std::sync::Arc;

use glimpsfy_ocr::models::ModelStore;
use glimpsfy_ocr::{
    default_engine, detect_codes, load_image, recognize, CancelToken, LanguageTag, OcrEngine, OcrRequest, PpOcrEngine,
};

fn main() {
    let (mut path, mut models, mut text_only, mut languages) = (None, None, false, String::from("en,tr"));
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--text" => text_only = true,
            "--models" => models = args.next(),
            "--languages" => languages = args.next().unwrap_or_default(),
            _ => path = Some(arg),
        }
    }
    let Some(path) = path else {
        eprintln!("usage: recognize <image> [--text] [--models <folder>] [--languages <tag,tag>]");
        std::process::exit(2);
    };
    let frame = match load_image(Path::new(&path)) {
        Ok(frame) => Arc::new(frame),
        Err(error) => {
            eprintln!("{path}: {error}");
            std::process::exit(1);
        }
    };
    let engine: Arc<dyn OcrEngine> = match models {
        Some(folder) => Arc::new(PpOcrEngine::new(ModelStore::new(folder))),
        None => default_engine(),
    };
    let mut request = OcrRequest::new(frame.clone());
    request.languages = languages.split(',').filter(|tag| !tag.is_empty()).map(LanguageTag::new).collect();
    let started = std::time::Instant::now();
    let result = recognize(&*engine, &request);
    let read = started.elapsed();
    let codes = detect_codes(&*engine, &frame, &CancelToken::new());
    eprintln!(
        "{} × {} px: text in {read:?}, codes in {:?}",
        frame.size.width,
        frame.size.height,
        started.elapsed() - read
    );
    match (result, codes) {
        (Ok(result), _) if text_only => println!("{}", result.text),
        (Ok(result), codes) => {
            let codes = codes.unwrap_or_default();
            let json = serde_json::json!({ "result": result, "codes": codes });
            println!("{}", serde_json::to_string_pretty(&json).unwrap_or_default());
        }
        (Err(error), _) => {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
