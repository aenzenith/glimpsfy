//! Transcribes a WAV file the way the app does and prints what the analysis found: the language,
//! every word with its time and filler mark, the silences, and how fast the model read.
//!
//! ```sh
//! cargo run -p glimpsfy-speech --example transcribe -- clip.wav
//! cargo run -p glimpsfy-speech --example transcribe -- clip.wav --language tr --gap-fillers
//! cargo run -p glimpsfy-speech --example transcribe -- clip.wav --cpu      # the Windows/Linux path
//! cargo run -p glimpsfy-speech --example transcribe -- clip.wav --cancel-after 2   # how fast a cancel lands
//! ```
//!
//! The model is read from the app's model folder (`GLIMPSFY_MODELS` or `--models` choose
//! another); `--download` fetches it there through the crate's own download when it is missing.
//! The dev profile builds whisper.cpp and this crate optimised, so the numbers are close to a
//! release build's.

mod common;

use std::path::PathBuf;
use std::time::Instant;

use common::seconds;
use glimpsfy_speech::model::SpeechModelStore;
use glimpsfy_speech::whisper::WhisperEngine;
use glimpsfy_speech::{analyze, CancelToken, Options};

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut file, mut models, mut language, mut download, mut cpu, mut gaps) = (None, None, None, false, false, false);
    let mut cancel_after: Option<f64> = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--models" => models = args.next().map(PathBuf::from),
            "--language" => language = args.next(),
            "--download" => download = true,
            "--cpu" => cpu = true,
            "--gap-fillers" => gaps = true,
            "--cancel-after" => cancel_after = args.next().and_then(|s| s.parse().ok()),
            _ => file = Some(PathBuf::from(arg)),
        }
    }
    let Some(file) = file else {
        eprintln!("usage: transcribe <file.wav> [--language tr] [--models <dir>] [--download] [--cpu] [--gap-fillers] [--cancel-after <s>]");
        std::process::exit(2)
    };
    let store = SpeechModelStore::new(models.unwrap_or_else(common::default_models));
    common::ensure_model(&store, download);

    let started = Instant::now();
    let audio = common::read_wav(&file);
    let audio_ms = audio.len() as u64 * 1000 / u64::from(glimpsfy_speech::SAMPLE_RATE);
    println!("audio: {} s at 16 kHz mono, read in {} ms", seconds(audio_ms), started.elapsed().as_millis());

    let loading = Instant::now();
    let engine = WhisperEngine::load_with(&store, !cpu).unwrap_or_else(|e| {
        eprintln!("the model did not load: {e}");
        std::process::exit(1)
    });
    println!(
        "model: loaded in {} ms on the {} with {} threads",
        loading.elapsed().as_millis(),
        if cpu || !cfg!(target_os = "macos") { "CPU" } else { "GPU (Metal)" },
        engine.threads()
    );

    let reading = Instant::now();
    let cancel = CancelToken::new();
    if let Some(seconds) = cancel_after {
        let token = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
            token.cancel();
        });
    }
    let mut first_words = None;
    let options = Options { language, voiced_gap_fillers: gaps };
    let analysis = match analyze(
        &engine,
        &audio,
        &options,
        &mut |p| {
            if first_words.is_none() && !p.words.is_empty() {
                first_words = Some(reading.elapsed());
            }
            eprintln!("  {} / {} s, {} new words", seconds(p.done_ms), seconds(p.total_ms), p.words.len());
        },
        &cancel,
    ) {
        Ok(analysis) => analysis,
        Err(e) => {
            eprintln!("the analysis failed after {:.2} s: {e}", reading.elapsed().as_secs_f64());
            // ggml's Metal device asserts at exit while a model still holds GPU memory.
            drop(engine);
            std::process::exit(1)
        }
    };
    let spent = reading.elapsed();

    println!(
        "language: {} ({}{})",
        analysis.language,
        if analysis.language_auto { "detected" } else { "chosen" },
        analysis.language_probability.map(|p| format!(", {:.0} %", p * 100.0)).unwrap_or_default()
    );
    println!("words:");
    for w in &analysis.words {
        let mark = match w.filler {
            Some(kind) => format!("  <- filler ({kind:?})"),
            None => String::new(),
        };
        println!(
            "  {:>7} – {:>7}  {:<24} p={:.2}{mark}",
            seconds(w.start_ms),
            seconds(w.end_ms),
            w.text,
            w.probability
        );
    }
    println!("silences:");
    for s in &analysis.silences {
        println!("  {:>7} – {:>7}  ({} s)", seconds(s.start_ms), seconds(s.end_ms), seconds(s.end_ms - s.start_ms));
    }
    let text: Vec<&str> = analysis.words.iter().map(|w| w.text.as_str()).collect();
    println!("text: {}", text.join(" "));
    println!(
        "fillers: {} of {} words; silences: {}",
        analysis.words.iter().filter(|w| w.filler.is_some()).count(),
        analysis.words.len(),
        analysis.silences.len()
    );
    println!(
        "speed: analysed in {:.2} s, {:.3} × real time; first words after {:.2} s",
        spent.as_secs_f64(),
        spent.as_secs_f64() * 1000.0 / audio_ms.max(1) as f64,
        first_words.map_or(0.0, |d| d.as_secs_f64())
    );
}
