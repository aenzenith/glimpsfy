//! The transcript of a recording as the app writes it next to its data and the trim window reads
//! it: the words with their times and filler marks, the silences, and the cuts the user kept.
//! The JSON shape mirrors `src/lib/api/transcript.ts` field by field.

use serde::{Deserialize, Serialize};

/// The version of [`Transcript`]'s shape. A transcript of another version is read again from the
/// recording instead of being trusted.
pub const TRANSCRIPT_VERSION: u32 = 1;

/// One spoken word, timed in milliseconds of the recording.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Word {
    /// The word as spoken, with the punctuation the model gave it.
    pub text: String,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Set when the word is a filler that "Cut all" removes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filler: Option<FillerKind>,
    /// The model's confidence, from 0 to 1. A filler found from the sound alone has 0.
    pub probability: f32,
}

/// What kind of filler a word is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FillerKind {
    /// A hesitation sound: ııı, eee, um, äh.
    Sound,
    /// A filler word: şey, yani, hani, like.
    Word,
}

/// A stretch without speech, found from the sound's level, at least half a second long.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Silence {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// A removed part of the recording; the export skips it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    pub start_ms: u64,
    pub end_ms: u64,
}

/// What identifies the recording a transcript was made from, so a changed file is read again.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fingerprint {
    pub bytes: u64,
    pub duration_ms: u64,
    pub modified_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Transcript {
    pub version: u32,
    pub fingerprint: Fingerprint,
    /// The language the speech was read in, as the model's code ("tr", "en").
    pub language: String,
    /// Whether the language was detected rather than chosen.
    pub language_auto: bool,
    /// How sure the detection was, from 0 to 1, when the language was detected.
    pub language_probability: Option<f32>,
    pub model: String,
    pub duration_ms: u64,
    pub words: Vec<Word>,
    pub silences: Vec<Silence>,
    /// The removed parts of the recording, sorted and apart.
    #[serde(default)]
    pub cuts: Vec<Span>,
    pub created_at_ms: u64,
}
