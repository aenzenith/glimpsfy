//! PP-OCR on ONNX Runtime, the engine for Linux and the choice for people who want the same
//! engine everywhere (milestone 8). The models come from the reviewed table in [`crate::models`].
//!
//! What exists now: the engine checks the model's hashes before it would load it, and reports
//! which languages the downloaded models read. The ONNX inference itself (DB text detection,
//! CTC recognition) is not written yet, so `read` ends with [`OcrError::NotImplemented`] after
//! the model check, and the `ppocr` feature that lists the engine stays off by default.

use glimpsfy_capture::Frame;

use crate::models::{ModelStore, MODELS};
use crate::{
    CancelToken, CodeResult, EngineId, LanguageInfo, LanguageTag, OcrEngine, OcrError, RawText, ReadOptions, Result,
};

/// Whether the detection and recognition inference is written. Until it is, the engine stops
/// with `NotImplemented` after the model check, and the settings cannot choose it.
pub const INFERENCE: bool = false;

/// Reads text with PP-OCR models from a model folder.
#[derive(Clone, Debug, Default)]
pub struct PpOcrEngine {
    models: Option<ModelStore>,
}

impl PpOcrEngine {
    pub fn new(models: ModelStore) -> Self {
        PpOcrEngine { models: Some(models) }
    }
}

impl OcrEngine for PpOcrEngine {
    fn id(&self) -> EngineId {
        EngineId::PpOcr
    }

    fn read(&self, _image: &Frame, options: &ReadOptions, cancel: &CancelToken) -> Result<RawText> {
        let model = MODELS
            .iter()
            .find(|m| {
                options.languages.is_empty()
                    || options.languages.iter().any(|l| m.languages.contains(&l.primary().as_str()))
            })
            .unwrap_or(&MODELS[0]);
        let store = self.models.as_ref().ok_or_else(|| OcrError::ModelMissing(model.id.to_string()))?;
        // The hashes are checked before anything is loaded, so a damaged file is deleted instead.
        let _verified = store.verified(model.id)?;
        cancel.check()?;
        Err(OcrError::NotImplemented("text recognition with PP-OCR"))
    }

    fn supported_languages(&self) -> Result<Vec<LanguageInfo>> {
        let installed = self.models.as_ref().map(|s| s.list()).unwrap_or_default();
        Ok(MODELS
            .iter()
            .flat_map(|model| {
                let available = installed.iter().any(|m| m.id == model.id && m.installed);
                let bytes: u64 = model.files.iter().map(|f| f.bytes).sum();
                model.languages.iter().map(move |tag| LanguageInfo {
                    tag: LanguageTag::new(*tag),
                    available,
                    download_bytes: (!available).then_some(bytes),
                })
            })
            .collect())
    }

    fn detect_codes(&self, _image: &Frame, _cancel: &CancelToken) -> Result<Vec<CodeResult>> {
        Err(OcrError::NotImplemented("code detection beside PP-OCR"))
    }
}
