//! Speech to text on the machine for cutting a recording by its text: the words of a recording
//! with their times, the fillers among them and the silences between them.
//!
//! # How an analysis runs
//!
//! The app decodes the recording's microphone (or mixed) track and brings it to 16 kHz mono with
//! [`audio::Downsampler`] while it decodes, then calls [`analyze`] on a worker thread. The
//! analysis
//!
//! 1. measures the sound's level in 10 ms frames ([`silence::Envelope`]), which decides what is
//!    quiet;
//! 2. takes the chosen language, or lets the engine detect it from the 30 s where most is said;
//! 3. lets the engine ([`SpeechEngine`], whisper.cpp in [`whisper`]) recognize the words, with a
//!    prompt full of the language's fillers so hesitations are written out ([`fillers::prompt`]);
//! 4. moves each word's edges onto its sound, drops words written into silence (a hallucinated
//!    subtitle credit) and marks fillers ([`fillers`]), handing the words out as they are ready;
//! 5. finds the silences of at least half a second that no word overlaps.
//!
//! Every step after the engine is plain Rust over times and levels, so another engine gets the
//! same fillers and silences.
//!
//! # Threads and cancellation
//!
//! [`analyze`] blocks for as long as the engine reads: on a Mac with Metal a fraction of the
//! recording's length, on the CPU of Windows and Linux about as long as the recording or longer.
//! A [`CancelToken`] stops it between the model's steps.
//!
//! # Network
//!
//! Recognition never touches the network. The only network traffic this crate may cause is the
//! model download in [`model`], which runs only when the user asks for it and only reaches the
//! hosts of its reviewed table.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub mod audio;
pub mod captions;
pub mod engine;
pub mod fillers;
pub mod model;
pub mod silence;
mod transcript;
pub mod whisper;

pub use engine::SpeechEngine;
pub use transcript::{FillerKind, Fingerprint, Silence, Span, Transcript, Word, TRANSCRIPT_VERSION};

use engine::Request;
use silence::{Envelope, FRAME_MS, MIN_SILENCE_MS};

/// The sample rate the analysis reads, in Hz.
pub const SAMPLE_RATE: u32 = 16_000;

/// The language of a recording in which nothing can be heard (BCP 47's "undetermined").
pub const UNDETERMINED: &str = "und";

/// How much sound the engine reads to detect the language.
const DETECT_MS: u64 = 30_000;
/// The model's words are held back by this many, so a filler is judged with the words after it.
const LOOKAHEAD: usize = 3;
/// A word with no voiced frame within this distance was written into silence.
const NEAR_MS: u64 = 100;
/// Voiced stretches without words between these lengths are filler candidates when
/// [`Options::voiced_gap_fillers`] is on; longer ones are music or noise rather than a hesitation.
const GAP_FILLER_MIN_MS: u64 = 250;
const GAP_FILLER_MAX_MS: u64 = 3000;
/// Before the first word, only this much is searched for a hesitation; earlier sound is an intro.
const GAP_FILLER_LEAD_MS: u64 = 2000;
/// The text of a filler found from the sound alone.
pub const GAP_FILLER_TEXT: &str = "…";

#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    #[error(transparent)]
    Model(#[from] glimpsfy_models::ModelError),
    /// The engine failed; the text names its reason.
    #[error("the speech engine failed: {0}")]
    Engine(String),
    #[error("the transcription was cancelled")]
    Cancelled,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SpeechError>;

/// Stops a transcription or a model download that is no longer wanted. Clones share one flag.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Fails with [`SpeechError::Cancelled`] once the token is cancelled; called between steps.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(SpeechError::Cancelled)
        } else {
            Ok(())
        }
    }

    fn flag(&self) -> &AtomicBool {
        &self.0
    }
}

/// How a recording is analysed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Options {
    /// The spoken language as a BCP 47 tag or the model's code; `None` detects it.
    pub language: Option<String>,
    /// Also counts a voiced stretch of 0.25 to 3 s without words as a hesitation, for speakers
    /// whose "ııı" the model leaves out despite the prompt. Off by default, because breaths,
    /// clicks and music between words count as well.
    pub voiced_gap_fillers: bool,
}

/// How far an analysis is, with the words finished since the last report.
#[derive(Clone, Debug, PartialEq)]
pub struct Progress {
    pub done_ms: u64,
    pub total_ms: u64,
    /// The words finished since the last call, in order, with their filler marks.
    pub words: Vec<Word>,
}

/// What an analysis found.
#[derive(Clone, Debug, PartialEq)]
pub struct Analysis {
    /// The model's code of the language the speech was read in, or [`UNDETERMINED`] when nothing
    /// could be heard and no language was chosen.
    pub language: String,
    /// Whether the language was detected rather than chosen.
    pub language_auto: bool,
    /// How sure the detection was, when the language was detected.
    pub language_probability: Option<f32>,
    pub words: Vec<Word>,
    pub silences: Vec<Silence>,
}

/// Recognizes `audio` (16 kHz mono), marks fillers and finds silences. `progress` receives every
/// word once, as soon as the words after it are known; the last call has `done_ms == total_ms`.
/// A recording without any sound gets no words and one silence over its whole length.
pub fn analyze(
    engine: &dyn SpeechEngine,
    audio: &[f32],
    options: &Options,
    progress: &mut dyn FnMut(Progress),
    cancel: &CancelToken,
) -> Result<Analysis> {
    cancel.check()?;
    let envelope = Envelope::new(audio);
    let total_ms = envelope.duration_ms();
    let requested =
        options.language.as_deref().map(str::trim).filter(|l| !l.is_empty() && !l.eq_ignore_ascii_case("auto"));
    let (language, language_probability) = match requested {
        Some(tag) => {
            let code =
                engine.language(tag).ok_or_else(|| SpeechError::Engine(format!("the model cannot read {tag}")))?;
            (code, None)
        }
        None if envelope.any_voiced() => {
            let (from, to) = envelope.busiest(DETECT_MS);
            let detected = engine.detect_language(&audio[sample(from, audio)..sample(to, audio)], cancel)?;
            (detected.language, Some(detected.probability))
        }
        None => (UNDETERMINED.to_string(), None),
    };

    let mut stage = Stage::new(&envelope, &language, options.voiced_gap_fillers);
    let mut words = Vec::new();
    if envelope.any_voiced() {
        let request = Request { language: &language, prompt: fillers::prompt(&language) };
        engine.recognize(
            audio,
            &request,
            &mut |recognized| {
                let ready = stage.push(recognized.words);
                words.extend_from_slice(&ready);
                progress(Progress { done_ms: recognized.done_ms.min(total_ms), total_ms, words: ready });
            },
            cancel,
        )?;
    }
    cancel.check()?;
    let rest = stage.finish();
    words.extend_from_slice(&rest);
    progress(Progress { done_ms: total_ms, total_ms, words: rest });

    let silences = envelope.silences(&words, MIN_SILENCE_MS);
    Ok(Analysis { language_auto: requested.is_none(), language, language_probability, words, silences })
}

/// The index of the sample at `ms`, within `audio`.
fn sample(ms: u64, audio: &[f32]) -> usize {
    ((ms * u64::from(SAMPLE_RATE) / 1000) as usize).min(audio.len())
}

/// The steps between the engine and the caller: the words' edges, the filler marks and, when
/// asked for, the fillers found from the sound alone.
struct Stage<'a> {
    envelope: &'a Envelope,
    language: &'a str,
    voiced_gaps: bool,
    /// Every word kept so far, with its start moved onto its sound. The end is final once the
    /// next word is known.
    words: Vec<Word>,
    /// The end the engine gave each word, used while no word follows it.
    limits: Vec<u64>,
    /// How many of `words` were handed out.
    emitted: usize,
    /// Where the last word handed out ends.
    last_end: Option<u64>,
}

impl<'a> Stage<'a> {
    fn new(envelope: &'a Envelope, language: &'a str, voiced_gaps: bool) -> Self {
        Stage { envelope, language, voiced_gaps, words: Vec::new(), limits: Vec::new(), emitted: 0, last_end: None }
    }

    /// Takes the engine's next words and hands out those that are ready.
    fn push(&mut self, incoming: Vec<Word>) -> Vec<Word> {
        let before = self.words.len();
        for word in incoming {
            let limit = word.end_ms.max(word.start_ms + FRAME_MS);
            // Nothing audible near the word: the model wrote into silence (Whisper's known
            // subtitle credits and repetitions), so the word was never said.
            if self.envelope.voiced_between(word.start_ms.saturating_sub(NEAR_MS), limit + NEAR_MS) == 0 {
                continue;
            }
            let previous = self.words.last().map_or(0, |p| p.start_ms);
            let floor = self.words.last().map_or(0, |p| p.start_ms + 5 * FRAME_MS);
            let start = self.envelope.snap_start(word.start_ms, limit, floor).max(previous);
            self.words.push(Word { start_ms: start, end_ms: limit, filler: None, ..word });
            self.limits.push(limit);
        }
        for i in before.saturating_sub(1)..self.words.len() {
            self.settle_end(i);
        }
        self.emit(self.words.len().saturating_sub(LOOKAHEAD), false)
    }

    /// Hands out the words that are left once the engine is done.
    fn finish(&mut self) -> Vec<Word> {
        if let Some(last) = self.words.len().checked_sub(1) {
            self.settle_end(last);
        }
        self.emit(self.words.len(), true)
    }

    /// Ends word `i` at the end of its sound, before the next word's start.
    fn settle_end(&mut self, i: usize) {
        let start = self.words[i].start_ms;
        let limit = match self.words.get(i + 1) {
            Some(next) => next.start_ms,
            None => self.limits[i].min(self.envelope.duration_ms()),
        };
        self.words[i].end_ms = self.envelope.word_end(start, limit.max(start + FRAME_MS));
    }

    fn emit(&mut self, ready: usize, finished: bool) -> Vec<Word> {
        let mut out = Vec::new();
        while self.emitted < ready {
            let j = self.emitted;
            fillers::mark(self.language, &mut self.words, j, finished);
            if self.voiced_gaps {
                let from = self.last_end.unwrap_or(self.words[j].start_ms.saturating_sub(GAP_FILLER_LEAD_MS));
                self.gap_fillers(from, self.words[j].start_ms, &mut out);
            }
            out.push(self.words[j].clone());
            self.last_end = Some(self.last_end.unwrap_or(0).max(self.words[j].end_ms));
            self.emitted += 1;
        }
        out
    }

    /// Adds a filler for each voiced stretch without words between two times.
    fn gap_fillers(&self, from: u64, to: u64, out: &mut Vec<Word>) {
        for (start_ms, end_ms) in self.envelope.voiced_runs(from, to) {
            if (GAP_FILLER_MIN_MS..=GAP_FILLER_MAX_MS).contains(&(end_ms - start_ms)) {
                out.push(Word {
                    text: GAP_FILLER_TEXT.to_string(),
                    start_ms,
                    end_ms,
                    filler: Some(FillerKind::Sound),
                    probability: 0.0,
                    edited: None,
                });
            }
        }
    }
}
