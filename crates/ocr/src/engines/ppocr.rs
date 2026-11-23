//! PP-OCR on RTen, the engine for Linux and the choice for people who want the same engine
//! everywhere (milestone 8). The models come from the reviewed table in [`crate::models`].
//!
//! # How a reading runs
//!
//! The engine picks the downloaded model that reads the most of the wanted languages, loads it
//! once (see below) and runs two stages: the detection model finds the text lines
//! (`ppocr/detect.rs`), and the recognition model reads each of them (`ppocr/recognize.rs`).
//! Every line comes back with a box of its own and with a box for each of its words, upright and
//! in pixels of the image, exactly as Vision returns them, so the layout step and the text layer
//! treat both engines alike. There is no orientation model: text on a screen is not upside down,
//! and the rare vertical line is read in both directions and the surer reading is kept.
//!
//! The models read what they see and have no language model, so the pipeline's
//! `language_correction` changes nothing here (the engine says so, and the pipeline skips the
//! second reading of code without it), and the engine reports no language of its own. The
//! recogniser scales every line to one height, so the pipeline's second reading of small text
//! from an enlarged copy runs only for text far smaller than Vision needs it for.
//!
//! # Loading
//!
//! The detector runs on one lane with every thread the engine may use, and the recogniser on one
//! lane per thread (`ppocr/runtime.rs`). Loading hashes and parses tens of megabytes, so the
//! loaded model stays in the process and every engine value shares it. It is loaded again when
//! the model's files change on disk (a new download, a removal), and loading always goes through
//! the store's hash check: the bytes the runtime gets are the bytes whose SHA-256 the table names.
//!
//! # Without the `ppocr` feature
//!
//! The inference and its runtime are compiled only with the feature. Without it the engine still
//! lists the models' languages, and reading fails with [`OcrError::EngineUnavailable`].

use glimpsfy_capture::Frame;

use crate::models::{ModelSpec, ModelStore, MODELS};
use crate::{
    CancelToken, CodeResult, EngineId, LanguageInfo, LanguageTag, OcrEngine, OcrError, RawText, ReadOptions, Result,
};

#[cfg(feature = "ppocr")]
mod detect;
#[cfg(feature = "ppocr")]
mod dictionary;
#[cfg(feature = "ppocr")]
mod pixels;
#[cfg(feature = "ppocr")]
mod recognize;
#[cfg(feature = "ppocr")]
mod runtime;

/// Whether this build can read text with PP-OCR, which is what lets Settings offer the engine.
pub const INFERENCE: bool = cfg!(feature = "ppocr");

/// Text whose median line is shorter than this many pixels is read again from an enlarged copy.
/// In the sample set at reduced sizes, lines of 8 px read the same without the copy; at 5 px the
/// detector finds only a few lines, and their boxes, which span two lines, measure 8 to 10 px.
#[cfg(feature = "ppocr")]
const SMALL_TEXT_PX: f64 = 12.0;

/// Reads text with PP-OCR models from a model folder.
#[derive(Clone, Debug, Default)]
pub struct PpOcrEngine {
    models: Option<ModelStore>,
}

impl PpOcrEngine {
    pub fn new(models: ModelStore) -> Self {
        PpOcrEngine { models: Some(models) }
    }

    /// The engine's own model folder, or the app's once the app has named it. An engine made
    /// without a folder (the platform default on Linux) finds the app's folder this way.
    fn store(&self) -> Option<ModelStore> {
        self.models.clone().or_else(crate::models::shared_store)
    }
}

impl OcrEngine for PpOcrEngine {
    fn id(&self) -> EngineId {
        EngineId::PpOcr
    }

    fn read(&self, image: &Frame, options: &ReadOptions, cancel: &CancelToken) -> Result<RawText> {
        cancel.check()?;
        let store = self.store();
        let installed: Vec<String> = store
            .as_ref()
            .map(|s| s.list().into_iter().filter(|m| m.installed).map(|m| m.id).collect())
            .unwrap_or_default();
        let spec = choose(&options.languages, &installed);
        let store = store.ok_or_else(|| OcrError::ModelMissing(spec.id.to_string()))?;
        read(&store, spec, image, cancel)
    }

    fn supported_languages(&self) -> Result<Vec<LanguageInfo>> {
        let installed = self.store().map(|s| s.list()).unwrap_or_default();
        let mut languages: Vec<LanguageInfo> = Vec::new();
        for model in MODELS {
            let available = installed.iter().any(|m| m.id == model.id && m.installed);
            let bytes: u64 = model.files.iter().map(|f| f.bytes).sum();
            for tag in model.languages {
                // A language two models read is listed once, and is available when either is.
                match languages.iter_mut().find(|l| l.tag.0 == *tag) {
                    Some(listed) if available => (listed.available, listed.download_bytes) = (true, None),
                    Some(_) => {}
                    None => languages.push(LanguageInfo {
                        tag: LanguageTag::new(*tag),
                        available,
                        download_bytes: (!available).then_some(bytes),
                    }),
                }
            }
        }
        Ok(languages)
    }

    fn detect_codes(&self, image: &Frame, cancel: &CancelToken) -> Result<Vec<CodeResult>> {
        // PP-OCR reads text only, so codes come from the platform's own detector.
        #[cfg(target_os = "macos")]
        let codes = super::vision::VisionEngine.detect_codes(image, cancel);
        #[cfg(not(target_os = "macos"))]
        let codes = crate::barcodes::detect(image, cancel);
        codes
    }

    fn corrects_language(&self) -> bool {
        false
    }

    #[cfg(feature = "ppocr")]
    fn upscale_factor(&self, raw: &RawText, image: &Frame, preparation: &crate::Preparation) -> Option<u32> {
        // The recogniser scales every line to its own height, so an enlarged copy only helps the
        // detector find text far smaller than the request's threshold. The detector reduces an
        // image above its pixel budget, so it would read the enlarged copy of a large image at
        // little more than the original's scale; an image above half the budget is read once.
        let pixels = image.size.width as f32 * image.size.height as f32;
        if pixels * 2.0 > detect::MAX_PIXELS {
            return None;
        }
        let small = crate::Preparation { small_text_px: preparation.small_text_px.min(SMALL_TEXT_PX), ..*preparation };
        crate::prepare::upscale_factor(raw, image, &small)
    }
}

/// Whether a model's language (`tr`, `zh-Hans`) covers a wanted tag (`tr-TR`, `zh-Hans-CN`,
/// `zh`): the same tag, a longer form of it, or at least the same primary language.
fn reads(language: &str, wanted: &LanguageTag) -> bool {
    let (language, tag) = (language.to_ascii_lowercase(), wanted.0.to_ascii_lowercase().replace('_', "-"));
    tag == language
        || tag.starts_with(&format!("{language}-"))
        || LanguageTag::new(language).primary() == wanted.primary()
}

/// The model for a reading: the downloaded one that reads the most of the wanted languages, and
/// among equals the one that reads the earliest of them. With no language wanted it is the first
/// downloaded model. When no downloaded model fits, it is the table's model for the first wanted
/// language, which the caller then reports as missing, so that Settings can offer its download.
fn choose(wanted: &[LanguageTag], installed: &[String]) -> &'static ModelSpec {
    // How well a model fits: how many wanted languages it reads, then how early the first is.
    let fit = |spec: &ModelSpec| {
        let read: Vec<usize> = wanted
            .iter()
            .enumerate()
            .filter(|(_, tag)| spec.languages.iter().any(|l| reads(l, tag)))
            .map(|(i, _)| i)
            .collect();
        (read.len(), std::cmp::Reverse(read.first().copied().unwrap_or(usize::MAX)))
    };
    let is_installed = |spec: &&ModelSpec| installed.iter().any(|id| id == spec.id);
    // `max_by_key` keeps the last of equals, so the table is walked backwards to prefer its
    // first model.
    let best = |candidates: &mut dyn Iterator<Item = &'static ModelSpec>| candidates.max_by_key(|spec| fit(spec));
    let downloaded = best(&mut MODELS.iter().rev().filter(is_installed));
    match downloaded {
        Some(spec) if wanted.is_empty() || fit(spec).0 > 0 => spec,
        _ => best(&mut MODELS.iter().rev()).filter(|spec| fit(spec).0 > 0).or(downloaded).unwrap_or(&MODELS[0]),
    }
}

#[cfg(not(feature = "ppocr"))]
fn read(_: &ModelStore, _: &ModelSpec, _: &Frame, _: &CancelToken) -> Result<RawText> {
    Err(OcrError::EngineUnavailable(EngineId::PpOcr))
}

/// Reads an image with one model: detection, then recognition of every detected line.
#[cfg(feature = "ppocr")]
fn read(store: &ModelStore, spec: &'static ModelSpec, image: &Frame, cancel: &CancelToken) -> Result<RawText> {
    let model = loaded::get(store, spec)?;
    cancel.check()?;
    let pixels = pixels::Pixels::of(image)?;
    if pixels.width == 0 || pixels.height == 0 {
        return Ok(RawText::default());
    }
    let (input, data) = detect::input(&pixels);
    cancel.check()?;
    let size = (pixels.width, pixels.height);
    let boxes = model
        .detection
        .run(0, input.shape, data, |shape, map| Ok(detect::boxes(&input, size, shape, map, &spec.detector)))?;
    cancel.check()?;
    let lines = recognize::read(&model.recognition, &model.alphabet, &pixels, &boxes, cancel)?;
    cancel.check()?;
    Ok(RawText { lines, language: None })
}

/// The loaded model the process keeps, and the check that it still matches the files on disk.
#[cfg(feature = "ppocr")]
mod loaded {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::SystemTime;

    use super::dictionary;
    use super::runtime::{Model, Threads};
    use crate::models::{ModelSpec, ModelStore, DETECTION_FILE, DICTIONARY_FILE, RECOGNITION_FILE};
    use crate::{OcrError, Result};

    pub struct Loaded {
        pub detection: Model,
        pub recognition: Model,
        pub alphabet: Vec<char>,
    }

    /// What the files of a model looked like when it was loaded: each file's path, size and
    /// modification time. A download or a removal changes it.
    type Stamp = Vec<(PathBuf, u64, Option<SystemTime>)>;

    /// The one loaded model. A second model replaces it: reading with two models in turn is
    /// rare, and each holds tens of megabytes.
    static KEPT: Mutex<Option<(Stamp, Arc<Loaded>)>> = Mutex::new(None);

    fn stamp(store: &ModelStore, spec: &ModelSpec) -> Option<Stamp> {
        spec.files
            .iter()
            .map(|file| {
                let path = store.root().join(spec.id).join(file.name);
                let meta = std::fs::metadata(&path).ok()?;
                Some((path, meta.len(), meta.modified().ok()))
            })
            .collect()
    }

    /// The model, loaded now or kept from an earlier reading. Loading checks every file's hash
    /// and fails with `ModelMissing` or `ChecksumMismatch` exactly as the store does.
    pub fn get(store: &ModelStore, spec: &'static ModelSpec) -> Result<Arc<Loaded>> {
        // The lock is held while loading, so two first readings do not both load the model.
        let mut kept = KEPT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let now = stamp(store, spec);
        if let (Some(now), Some((then, model))) = (&now, kept.as_ref()) {
            if now == then {
                return Ok(model.clone());
            }
        }
        *kept = None;
        let verified = store.verified(spec.id)?;
        let alphabet = String::from_utf8(verified.bytes(DICTIONARY_FILE)?)
            .map_err(|_| OcrError::Engine("the recognition model's dictionary is not UTF-8".into()))
            .and_then(|yaml| dictionary::parse(&yaml))?;
        let model = Arc::new(Loaded {
            detection: Model::load(verified.bytes(DETECTION_FILE)?, Threads::one_lane())?,
            recognition: Model::load(verified.bytes(RECOGNITION_FILE)?, Threads::lane_per_thread())?,
            alphabet,
        });
        // The model is kept only when the files did not change while it was loaded; otherwise
        // the next reading loads, and checks, the new files.
        if let Some(now) = now.filter(|now| stamp(store, spec).as_ref() == Some(now)) {
            *kept = Some((now, model.clone()));
        }
        Ok(model)
    }
}
