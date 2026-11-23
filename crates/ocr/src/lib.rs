//! Text recognition and QR/barcode detection behind one engine trait.
//!
//! "Copy text", the selectable text layer over a capture, the history's search and smart
//! redaction all read [`OcrResult`], so they work the same with every engine. The engines are
//! Vision on macOS (milestone 3), Windows.Media.Ocr on Windows (milestone 7) and PP-OCR on RTen
//! everywhere (the default on Linux, milestone 8).
//!
//! # How a recognition runs
//!
//! [`recognize`] is the one entry point. It prepares the image ([`prepare`]: light-on-dark
//! captures are inverted, small text is upscaled), lets the engine read raw lines and words
//! ([`OcrEngine::read`]), maps the boxes back to the original image and hands them to the
//! layout step ([`layout`]), which puts the lines into reading order, finds columns, tables,
//! paragraphs and code, and writes [`OcrResult::text`]. Everything after the engine is plain
//! Rust over boxes, so every engine gets the same reading order.
//!
//! # Coordinates
//!
//! Every box is an axis-aligned [`Bounds`] in pixels of the request's image, with the origin at
//! its top-left corner and y growing downwards; text set at an angle gets the box that encloses
//! it. The image is the selection cut out of the frozen frame at its physical resolution, so the
//! overlay places a box by dividing it by `image_size / selection size` and adding the
//! selection's origin.
//!
//! # Languages
//!
//! Languages are BCP 47 tags (`en-US`, `tr-TR`). The set an engine supports is read from the
//! engine at run time and is never written into the app, because it differs between OS versions
//! and installed language packs.
//!
//! # Threads and cancellation
//!
//! Every function blocks while the engine works, from tens of milliseconds to a few seconds for a
//! full display. Call them on a worker thread, never on the main thread or an async runtime's
//! reactor. A [`CancelToken`] in the request stops a recognition that is no longer wanted (the
//! user closed the overlay); the call then returns [`OcrError::Cancelled`] as soon as the engine
//! lets go.
//!
//! # Subjects
//!
//! [`find_subjects`] finds the subjects of an image apart from its background, for the editor's
//! Remove background and the overlay's Copy subject. It shares this crate because it is one more
//! Vision request with the same plumbing (the image handler, the cancel watcher, [`CancelToken`]);
//! see [`Subjects`].
//!
//! # Network
//!
//! Recognition never touches the network. The only network traffic this crate may cause is the
//! PP-OCR model download in [`models`], which runs only when the user asks for it and only
//! reaches the hosts in its reviewed table.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use glimpsfy_capture::{CaptureError, Frame, PhysicalSize};
use serde::{Deserialize, Serialize};

#[cfg(not(target_os = "macos"))]
pub mod barcodes;
mod codes;
mod engines;
pub mod entities;
mod image;
pub mod layout;
pub mod models;
mod pipeline;
pub mod prepare;
mod subjects;

pub use codes::link_for;
#[cfg(target_os = "macos")]
pub use engines::vision::VisionEngine;
#[cfg(target_os = "windows")]
pub use engines::windows::WindowsOcrEngine;
pub use engines::{available_engines, default_engine, engine, ppocr::PpOcrEngine, ready_engines};
pub use entities::{find_entities, Entity, EntityBox, EntityDate, EntityKind};
pub use image::{decode_image_as, load_image, load_image_as};
pub use pipeline::{detect_codes, recognize};
pub use prepare::Preparation;
pub use subjects::{find_subjects, Subjects};

pub type Result<T, E = OcrError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum OcrError {
    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),
    /// The engine does not exist on this platform or in this build, such as Vision on Linux or
    /// PP-OCR in a build without the `ppocr` feature.
    #[error("the {0:?} engine is not available on this platform")]
    EngineUnavailable(EngineId),
    /// The engine cannot read this language, or its language pack is not installed.
    #[error("the language {0} is not available")]
    LanguageUnavailable(LanguageTag),
    /// PP-OCR needs this model, and the model is not downloaded yet.
    #[error("the recognition model {0} is not downloaded")]
    ModelMissing(String),
    #[error("the image cannot be read: {0}")]
    InvalidImage(String),
    #[error(transparent)]
    Capture(#[from] CaptureError),
    /// The engine itself failed; the text is the engine's own reason.
    #[error("text recognition failed: {0}")]
    Engine(String),
    /// The request's [`CancelToken`] was cancelled before the engine finished.
    #[error("the recognition was cancelled")]
    Cancelled,
    /// A model download failed; the text names the host's or the network's reason.
    #[error("the model download failed: {0}")]
    Download(String),
    /// A downloaded or stored model did not have the bytes the reviewed table names. The file
    /// was deleted and is never loaded.
    #[error("the model {model} failed its checksum and was deleted")]
    ChecksumMismatch { model: String },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Names a language with a BCP 47 tag, such as `tr-TR`. The UI names it in the user's language with
/// `Intl.DisplayNames`, so the engines never have to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LanguageTag(pub String);

impl LanguageTag {
    pub fn new(tag: impl Into<String>) -> Self {
        LanguageTag(tag.into())
    }

    /// The primary language subtag in lower case: `tr` for `tr-TR`.
    pub fn primary(&self) -> String {
        self.0.split(['-', '_']).next().unwrap_or_default().to_ascii_lowercase()
    }
}

impl std::fmt::Display for LanguageTag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The engines, as Settings › Text recognition offers them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EngineId {
    /// Apple's Vision framework reads the text (`VNRecognizeTextRequest` at the accurate level)
    /// and exists on macOS only.
    Vision,
    /// The engine built into Windows, which reads the languages whose language pack is installed.
    WindowsMediaOcr,
    /// PP-OCR models run on RTen on every platform, with one model download per script.
    PpOcr,
}

/// Describes an axis-aligned box in pixels of the request's image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Bounds {
    pub fn new(x: f64, y: f64, width: f64, height: f64) -> Self {
        Bounds { x, y, width, height }
    }

    pub fn right(&self) -> f64 {
        self.x + self.width
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.height
    }

    pub fn center_y(&self) -> f64 {
        self.y + self.height / 2.0
    }

    /// The smallest box that holds both.
    pub fn union(&self, other: &Bounds) -> Bounds {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Bounds::new(x, y, self.right().max(other.right()) - x, self.bottom().max(other.bottom()) - y)
    }

    /// The box with every coordinate divided by `factor`, for boxes read from a scaled image.
    pub fn scaled_down(&self, factor: f64) -> Bounds {
        Bounds::new(self.x / factor, self.y / factor, self.width / factor, self.height / factor)
    }
}

/// Stops a recognition that is no longer wanted. Clones share one flag, so the command that
/// started a recognition keeps a clone and cancels it when the overlay closes.
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

    /// Whether the two tokens are clones that share one flag.
    pub fn same(&self, other: &CancelToken) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    /// Fails with [`OcrError::Cancelled`] once the token is cancelled; called between steps.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(OcrError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Says what to read and how.
#[derive(Clone, Debug)]
pub struct OcrRequest {
    /// The image to read, usually the selection cut out of the frozen frame at its physical
    /// resolution. It is shared, so the overlay keeps showing the same frame while the engine
    /// reads it.
    pub image: Arc<Frame>,
    /// The languages to expect, the most likely first. An empty list lets the engine use its own
    /// default (the system's preferred languages on macOS and Windows).
    pub languages: Vec<LanguageTag>,
    /// Lets the engine detect the language itself (Settings › Text recognition › Detect the
    /// language). The engine still prefers the languages in `languages`.
    pub detect_language: bool,
    /// Keeps the line breaks of the image in [`OcrResult::text`]; when off, the lines of one
    /// paragraph are joined with spaces (Settings › Text recognition › Keep line breaks). Code
    /// keeps its line breaks either way.
    pub keep_line_breaks: bool,
    /// The image preparation steps; the defaults are the ones the sample set showed to help.
    pub prepare: Preparation,
    pub cancel: CancelToken,
}

impl OcrRequest {
    /// A request with the default languages and options for `image`.
    pub fn new(image: Arc<Frame>) -> Self {
        OcrRequest {
            image,
            languages: Vec::new(),
            detect_language: true,
            keep_line_breaks: true,
            prepare: Preparation::default(),
            cancel: CancelToken::new(),
        }
    }
}

/// Holds one recognised word.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrWord {
    pub text: String,
    pub bounds: Bounds,
    /// The engine's confidence runs from 0 to 1.
    pub confidence: f64,
}

/// Holds one recognised line, with its words in reading order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrLine {
    /// The line as it appears in [`OcrResult::text`], indentation included. A table row holds
    /// its cells separated by tabs.
    pub text: String,
    pub bounds: Bounds,
    /// The engine's confidence runs from 0 to 1.
    pub confidence: f64,
    pub words: Vec<OcrWord>,
    /// The paragraph the line belongs to. Lines are listed in reading order, and a new block
    /// number starts a new paragraph, which keeps multi-column layouts in order.
    pub block: u32,
    /// What kind of text the line's block holds.
    pub kind: BlockKind,
}

/// What kind of text a block holds; the layout step decides it from the geometry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlockKind {
    /// Running text: its lines may be joined into one paragraph.
    Prose,
    /// Monospaced text such as code or a terminal: line breaks and indentation are kept.
    Code,
    /// Rows of cells, such as a table or a form: each row is one line with tabs between cells.
    Table,
}

/// Everything an engine read from one image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrResult {
    pub engine: EngineId,
    /// The whole text in reading order, with line breaks kept or joined as the request asked.
    /// This is what "Copy text" puts on the clipboard.
    pub text: String,
    pub lines: Vec<OcrLine>,
    /// The dominant language of the text, when the engine or the system can tell it.
    pub language: Option<LanguageTag>,
    /// The pixel size of the image that was read, which the boxes refer to.
    pub image_size: PhysicalSize,
}

/// The text one engine read, before the layout step: lines in the engine's own order with their
/// words, every box in pixels of the image the engine was given.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawText {
    pub lines: Vec<RawLine>,
    /// The language the engine reports, if it reports one.
    pub language: Option<LanguageTag>,
}

/// One piece of text as an engine reports it: usually a line, sometimes a cell or a part of a
/// line. The layout step decides what belongs together.
#[derive(Clone, Debug, PartialEq)]
pub struct RawLine {
    pub text: String,
    pub bounds: Bounds,
    pub confidence: f64,
    /// The words with their own boxes. An engine that cannot box words leaves this empty, and
    /// the layout step spreads the line's box over its words by their length.
    pub words: Vec<OcrWord>,
}

/// How an engine reads, as the pipeline asks for it.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadOptions {
    pub languages: Vec<LanguageTag>,
    pub detect_language: bool,
    /// Lets the engine correct words with its language model. It helps prose and can damage
    /// identifiers in code.
    pub language_correction: bool,
}

/// A language as an engine offers it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LanguageInfo {
    pub tag: LanguageTag,
    /// Whether the engine can read the language now. A Windows language pack or a PP-OCR model
    /// that is not installed makes it `false`.
    pub available: bool,
    /// The size of the download that makes the language available, when one is needed.
    pub download_bytes: Option<u64>,
}

/// Names the kind of a detected code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Symbology {
    Qr,
    MicroQr,
    Aztec,
    DataMatrix,
    Pdf417,
    Ean8,
    Ean13,
    UpcE,
    Code39,
    Code93,
    Code128,
    Itf,
    Codabar,
    /// A symbology the engine reports and this list does not name yet.
    Other,
}

/// Describes a QR code or barcode found in an image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeResult {
    pub symbology: Symbology,
    /// Holds the decoded content as text.
    pub payload: String,
    pub bounds: Bounds,
    /// The payload as a link when it is an `http` or `https` URL, which the chip under the code
    /// offers to open. Other schemes are never offered as links.
    pub url: Option<String>,
}

/// Reads text from images; every engine implements this trait. The engines only read: image
/// preparation, reading order and the final text are the pipeline's ([`recognize`]), so every
/// engine gets them alike.
pub trait OcrEngine: Send + Sync {
    fn id(&self) -> EngineId;

    /// Reads the lines and words of `image` in the engine's own order. Blocks until the engine
    /// finishes, and returns [`OcrError::Cancelled`] soon after `cancel` is cancelled.
    fn read(&self, image: &Frame, options: &ReadOptions, cancel: &CancelToken) -> Result<RawText>;

    /// The languages the engine knows on this machine, read from the engine at run time.
    fn supported_languages(&self) -> Result<Vec<LanguageInfo>>;

    /// Finds QR codes and barcodes in the image. Blocks until the detector finishes. An engine
    /// without a detector of its own (PP-OCR) is paired with a separate decoder in milestone 8.
    fn detect_codes(&self, image: &Frame, cancel: &CancelToken) -> Result<Vec<CodeResult>>;

    /// The dominant language of a recognised text, when the platform can tell it. The pipeline
    /// asks after the layout step, because the engines do not all report a language.
    fn dominant_language(&self, _text: &str, _candidates: &[LanguageTag]) -> Option<LanguageTag> {
        None
    }

    /// Whether [`ReadOptions::language_correction`] changes what the engine reads. An engine
    /// without a language model says no, and the pipeline then skips the second reading of code
    /// without correction, which would only repeat the first.
    fn corrects_language(&self) -> bool {
        true
    }

    /// How much to enlarge `image` before reading it a second time, after the first reading
    /// gave `raw`, or `None` when one reading is enough. The default is the request's own rule
    /// ([`prepare::upscale_factor`]); an engine that scales every line to a fixed height before
    /// reading it gains from the copy only with far smaller text.
    fn upscale_factor(&self, raw: &RawText, image: &Frame, preparation: &Preparation) -> Option<u32> {
        prepare::upscale_factor(raw, image, preparation)
    }
}
