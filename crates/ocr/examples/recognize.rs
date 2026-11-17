//! Reads one image file with the default engine and the default preparation, and prints the
//! result and the codes as the commands return them (JSON), or the text alone with `--text`.
//!
//! ```sh
//! cargo run -p glimpsfy-ocr --example recognize -- tests/samples/qr.png
//! cargo run -p glimpsfy-ocr --example recognize -- tests/samples/code.png --text
//! ```

use std::path::Path;
use std::sync::Arc;

use glimpsfy_ocr::{default_engine, detect_codes, load_image, recognize, CancelToken, LanguageTag, OcrRequest};

fn main() {
    let mut args = std::env::args().skip(1);
    let Some(path) = args.next() else {
        eprintln!("usage: recognize <image> [--text]");
        std::process::exit(2);
    };
    let text_only = args.any(|a| a == "--text");
    let frame = match load_image(Path::new(&path)) {
        Ok(frame) => Arc::new(frame),
        Err(error) => {
            eprintln!("{path}: {error}");
            std::process::exit(1);
        }
    };
    let engine = default_engine();
    let mut request = OcrRequest::new(frame.clone());
    request.languages = vec![LanguageTag::new("en"), LanguageTag::new("tr")];
    let started = std::time::Instant::now();
    let result = recognize(&*engine, &request);
    let codes = detect_codes(&*engine, &frame, &CancelToken::new());
    eprintln!("{} × {} px in {:?}", frame.size.width, frame.size.height, started.elapsed());
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
