//! The speech recognizer the analysis runs on, as one trait: it names its languages, detects the
//! spoken one and turns 16 kHz mono sound into timed words. Fillers, silences and the exact edges
//! of words are the analysis' own work ([`crate::analyze`]), so they are the same for every
//! engine.

use crate::{CancelToken, Result, Word};

/// The language an engine heard.
#[derive(Clone, Debug, PartialEq)]
pub struct Detected {
    /// The engine's code for it ("tr", "en", "zh").
    pub language: String,
    /// How sure the engine is, from 0 to 1.
    pub probability: f32,
}

/// What a recognition reads.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Request<'a> {
    /// The engine's code of the spoken language, as [`SpeechEngine::language`] returns it.
    pub language: &'a str,
    /// Text the model reads before the sound as if it had been said, so it keeps the style of
    /// it; the analysis passes sentences full of fillers, so hesitations are written out instead
    /// of being dropped.
    pub prompt: Option<&'a str>,
}

/// Words a recognition finished, reported while it runs.
#[derive(Clone, Debug, PartialEq)]
pub struct Recognized {
    /// How far the sound is read, in milliseconds.
    pub done_ms: u64,
    /// The words finished since the last report, in order, without filler marks. A word's
    /// `end_ms` is where the next word starts, or where its stretch of speech ends.
    pub words: Vec<Word>,
}

/// A speech recognizer that runs on this machine.
pub trait SpeechEngine: Send + Sync {
    /// The model's id, written into transcripts.
    fn model(&self) -> &str;

    /// The engine's code for a language given as a BCP 47 tag or a model code ("tr-TR", "zh-Hans",
    /// "en"), or `None` when the engine cannot read it.
    fn language(&self, tag: &str) -> Option<String>;

    /// Detects the spoken language of 16 kHz mono `audio`; the engine reads up to its first 30 s.
    fn detect_language(&self, audio: &[f32], cancel: &CancelToken) -> Result<Detected>;

    /// Recognizes the words of 16 kHz mono `audio`. Every word reaches `progress` exactly once,
    /// in order, timed in milliseconds of `audio`. Blocks for minutes on a long recording;
    /// `cancel` stops it with [`crate::SpeechError::Cancelled`].
    fn recognize(
        &self,
        audio: &[f32],
        request: &Request<'_>,
        progress: &mut dyn FnMut(Recognized),
        cancel: &CancelToken,
    ) -> Result<()>;
}

/// Whether a language is written without spaces between words, so a word is what the model's
/// tokens make of it rather than what spaces part.
pub fn unspaced(language: &str) -> bool {
    matches!(language, "ja" | "zh" | "yue" | "th" | "lo" | "my" | "km" | "bo")
}
