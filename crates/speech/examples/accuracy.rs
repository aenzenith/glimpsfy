//! Measures cut-by-text on narrations whose text is known: the word error rate, how many of the
//! fillers are found, and how many found fillers are not fillers. The acceptance for Turkish is
//! WER ≤ 15 %, filler recall ≥ 80 % and false fillers ≤ 5 %, over the author's ten narrations.
//!
//! ```sh
//! cargo run -p glimpsfy-speech --example accuracy -- <folder> [--language tr|auto] [--verbose]
//! ```
//!
//! The folder holds pairs: `<name>.wav` (any rate and channel count) and `<name>.txt`, the text as
//! spoken, with every filler in square brackets: `[ııı] bugün size [şey] bunu [yani] göstereceğim`.
//! Write a hesitation as it sounds; any spelling counts, since a filler is matched to a filler. To
//! make the set, record each narration in the app (the microphone track is what the app reads),
//! export or copy its sound to WAV, and write down what was said, fillers included.
//!
//! Each file is read once, with the voiced-gap fillers on; the other mode is the same result
//! without them, because those fillers change nothing else. Both modes are printed. The exit code
//! is 0 when the default mode (prompt only) passes, 2 when only the voiced-gap mode passes (turn
//! [`glimpsfy_speech::Options::voiced_gap_fillers`] on by default), and 1 when neither does.
//!
//! WER counts the words that are not fillers, in normal form (lower case, no punctuation), against
//! the reference's words outside the brackets. Recall is the share of the reference's fillers
//! aligned to a word marked as a filler; false fillers is the share of marked fillers that align
//! to no filler of the reference. Languages without spaces are compared by character.

mod common;

use std::path::{Path, PathBuf};
use std::time::Instant;

use glimpsfy_speech::engine::unspaced;
use glimpsfy_speech::fillers::normalize;
use glimpsfy_speech::model::SpeechModelStore;
use glimpsfy_speech::whisper::WhisperEngine;
use glimpsfy_speech::{analyze, CancelToken, Options, SpeechEngine, Word, GAP_FILLER_TEXT};

const MAX_WER: f64 = 0.15;
const MIN_RECALL: f64 = 0.80;
const MAX_FALSE: f64 = 0.05;

#[derive(Clone, Debug)]
struct Token {
    form: String,
    filler: bool,
}

#[derive(Clone, Copy, Default)]
struct Score {
    /// Edits between the non-filler words, and the reference's non-filler words.
    edits: usize,
    words: usize,
    /// The reference's fillers, those aligned to a marked filler, and all marked fillers.
    fillers: usize,
    found: usize,
    marked: usize,
}

impl Score {
    fn add(&mut self, other: Score) {
        self.edits += other.edits;
        self.words += other.words;
        self.fillers += other.fillers;
        self.found += other.found;
        self.marked += other.marked;
    }

    fn wer(&self) -> f64 {
        self.edits as f64 / self.words.max(1) as f64
    }

    fn recall(&self) -> f64 {
        if self.fillers == 0 {
            1.0
        } else {
            self.found as f64 / self.fillers as f64
        }
    }

    fn false_share(&self) -> f64 {
        if self.marked == 0 {
            0.0
        } else {
            (self.marked - self.found) as f64 / self.marked as f64
        }
    }

    fn passes(&self) -> bool {
        self.wer() <= MAX_WER && self.recall() >= MIN_RECALL && self.false_share() <= MAX_FALSE
    }

    fn line(&self) -> String {
        format!(
            "WER {:5.1} % ({}/{})  recall {:5.1} % ({}/{})  false {:5.1} % ({}/{})",
            self.wer() * 100.0,
            self.edits,
            self.words,
            self.recall() * 100.0,
            self.found,
            self.fillers,
            self.false_share() * 100.0,
            self.marked - self.found,
            self.marked
        )
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let (mut folder, mut models, mut language, mut download, mut cpu, mut verbose) =
        (None, None, "tr".to_string(), false, false, false);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--models" => models = args.next().map(PathBuf::from),
            "--language" => language = args.next().unwrap_or(language),
            "--download" => download = true,
            "--cpu" => cpu = true,
            "--verbose" => verbose = true,
            _ => folder = Some(PathBuf::from(arg)),
        }
    }
    let Some(folder) = folder else {
        eprintln!("usage: accuracy <folder> [--language tr|auto] [--models <dir>] [--download] [--cpu] [--verbose]");
        std::process::exit(2)
    };
    let pairs = pairs(&folder);
    if pairs.is_empty() {
        eprintln!("{} holds no <name>.wav with a <name>.txt beside it", folder.display());
        std::process::exit(2);
    }

    let store = SpeechModelStore::new(models.unwrap_or_else(common::default_models));
    common::ensure_model(&store, download);
    let engine = WhisperEngine::load_with(&store, !cpu).unwrap_or_else(|e| {
        eprintln!("the model did not load: {e}");
        std::process::exit(1)
    });

    let (mut prompt_only, mut with_gaps) = (Score::default(), Score::default());
    let (mut audio_s, mut spent_s) = (0.0, 0.0);
    for (name, wav, txt) in &pairs {
        let reference_text = match std::fs::read_to_string(txt) {
            Ok(text) => text,
            Err(e) => {
                eprintln!("{} cannot be read: {e}", txt.display());
                drop(engine);
                std::process::exit(2)
            }
        };
        let audio = common::read_wav(wav);
        let started = Instant::now();
        let options = Options { language: (language != "auto").then(|| language.clone()), voiced_gap_fillers: true };
        let analysis = match analyze(&engine, &audio, &options, &mut |_| {}, &CancelToken::new()) {
            Ok(analysis) => analysis,
            Err(e) => {
                eprintln!("{name}: the analysis failed: {e}");
                drop(engine);
                std::process::exit(1)
            }
        };
        let spent = started.elapsed().as_secs_f64();
        let seconds = audio.len() as f64 / f64::from(glimpsfy_speech::SAMPLE_RATE);
        audio_s += seconds;
        spent_s += spent;

        let code = engine.language(&analysis.language).unwrap_or(analysis.language.clone());
        let reference = reference_tokens(&reference_text, &code);
        let all = hypothesis_tokens(&analysis.words, &code);
        let without: Vec<Word> = analysis.words.iter().filter(|w| !is_gap_filler(w)).cloned().collect();
        let first = score(&reference, &hypothesis_tokens(&without, &code));
        let second = score(&reference, &all);
        prompt_only.add(first);
        with_gaps.add(second);

        println!(
            "{name}: {:.1} s, read in {:.1} s ({:.2} × real time), language {}",
            seconds,
            spent,
            spent / seconds,
            analysis.language
        );
        println!("  prompt only:   {}", first.line());
        println!("  voiced gaps:   {}", second.line());
        if verbose {
            let marked: Vec<String> = without
                .iter()
                .map(|w| if w.filler.is_some() { format!("[{}]", w.text) } else { w.text.clone() })
                .collect();
            println!("  text: {}", marked.join(" "));
        }
    }

    println!(
        "total: {:.1} s of sound read in {:.1} s ({:.2} × real time)",
        audio_s,
        spent_s,
        spent_s / audio_s.max(1e-9)
    );
    println!("  prompt only:   {}  {}", prompt_only.line(), verdict(prompt_only.passes()));
    println!("  voiced gaps:   {}  {}", with_gaps.line(), verdict(with_gaps.passes()));
    println!(
        "  thresholds:    WER ≤ {:.0} %, recall ≥ {:.0} %, false ≤ {:.0} %",
        MAX_WER * 100.0,
        MIN_RECALL * 100.0,
        MAX_FALSE * 100.0
    );
    let code = if prompt_only.passes() {
        0
    } else if with_gaps.passes() {
        println!("only the voiced-gap mode passes: turn Options::voiced_gap_fillers on by default");
        2
    } else {
        1
    };
    // ggml's Metal device asserts at exit while a model still holds GPU memory, so the engine goes
    // first.
    drop(engine);
    std::process::exit(code);
}

fn verdict(pass: bool) -> &'static str {
    if pass {
        "PASS"
    } else {
        "FAIL"
    }
}

fn is_gap_filler(word: &Word) -> bool {
    word.text == GAP_FILLER_TEXT && word.probability == 0.0
}

/// The `<name>.wav` files of the folder that have a `<name>.txt`, sorted by name.
fn pairs(folder: &Path) -> Vec<(String, PathBuf, PathBuf)> {
    let mut out: Vec<_> = std::fs::read_dir(folder)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("wav")))
                .filter_map(|wav| {
                    let txt = wav.with_extension("txt");
                    let name = wav.file_stem()?.to_string_lossy().into_owned();
                    txt.is_file().then_some((name, wav, txt))
                })
                .collect()
        })
        .unwrap_or_default();
    out.sort();
    out
}

/// The pieces a text is compared in: words, or characters in a language without spaces.
fn pieces(text: &str, language: &str, filler: bool, out: &mut Vec<Token>) {
    if unspaced(language) {
        for c in text.chars() {
            let form = normalize(&c.to_string(), language);
            if !form.is_empty() {
                out.push(Token { form, filler });
            }
        }
    } else {
        for word in text.split_whitespace() {
            let form = normalize(word, language);
            if !form.is_empty() {
                out.push(Token { form, filler });
            }
        }
    }
}

/// The reference's pieces; text in square brackets is a filler.
fn reference_tokens(text: &str, language: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        pieces(&rest[..open], language, false, &mut out);
        let close = rest[open..].find(']').map_or(rest.len(), |c| open + c);
        pieces(&rest[open + 1..close], language, true, &mut out);
        rest = &rest[(close + 1).min(rest.len())..];
    }
    pieces(rest, language, false, &mut out);
    out
}

fn hypothesis_tokens(words: &[Word], language: &str) -> Vec<Token> {
    let mut out = Vec::new();
    for word in words {
        if is_gap_filler(word) {
            // A filler from the sound alone has no text to compare, only its mark.
            out.push(Token { form: String::new(), filler: true });
        } else {
            pieces(&word.text, language, word.filler.is_some(), &mut out);
        }
    }
    out
}

fn score(reference: &[Token], hypothesis: &[Token]) -> Score {
    let content = |tokens: &[Token]| tokens.iter().filter(|t| !t.filler).map(|t| t.form.clone()).collect::<Vec<_>>();
    let (r, h) = (content(reference), content(hypothesis));
    let edits = distance(&r, &h);

    // Fillers are aligned to fillers whatever their spelling.
    let same = |a: &Token, b: &Token| if a.filler || b.filler { a.filler && b.filler } else { a.form == b.form };
    let pairs = align(reference, hypothesis, same);
    let found = pairs.iter().filter(|(a, b)| matches!((a, b), (Some(a), Some(b)) if a.filler && b.filler)).count();
    Score {
        edits,
        words: r.len(),
        fillers: reference.iter().filter(|t| t.filler).count(),
        found,
        marked: hypothesis.iter().filter(|t| t.filler).count(),
    }
}

/// The edit distance between two sequences of words.
fn distance(a: &[String], b: &[String]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.iter().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let next = (diagonal + usize::from(x != y)).min(row[j] + 1).min(row[j + 1] + 1);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// A minimal-edit alignment of two token sequences, as pairs (a gap is `None`).
fn align<'a>(
    a: &'a [Token],
    b: &'a [Token],
    same: impl Fn(&Token, &Token) -> bool,
) -> Vec<(Option<&'a Token>, Option<&'a Token>)> {
    let (n, m) = (a.len(), b.len());
    let mut cost = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in cost.iter_mut().enumerate() {
        row[0] = i;
    }
    cost[0] = (0..=m).collect();
    for i in 1..=n {
        for j in 1..=m {
            let substitute = cost[i - 1][j - 1] + usize::from(!same(&a[i - 1], &b[j - 1]));
            cost[i][j] = substitute.min(cost[i - 1][j] + 1).min(cost[i][j - 1] + 1);
        }
    }
    let (mut i, mut j) = (n, m);
    let mut out = Vec::new();
    while i > 0 || j > 0 {
        if i > 0 && j > 0 && cost[i][j] == cost[i - 1][j - 1] + usize::from(!same(&a[i - 1], &b[j - 1])) {
            out.push((Some(&a[i - 1]), Some(&b[j - 1])));
            (i, j) = (i - 1, j - 1);
        } else if i > 0 && cost[i][j] == cost[i - 1][j] + 1 {
            out.push((Some(&a[i - 1]), None));
            i -= 1;
        } else {
            out.push((None, Some(&b[j - 1])));
            j -= 1;
        }
    }
    out.reverse();
    out
}
