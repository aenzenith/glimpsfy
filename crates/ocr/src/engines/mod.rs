//! The engines and the choice between them: Vision on macOS, Windows.Media.Ocr on Windows
//! (milestone 7) and PP-OCR wherever the build has it.

use std::sync::Arc;

#[cfg(target_os = "windows")]
use glimpsfy_capture::Frame;

#[cfg(target_os = "windows")]
use crate::{CancelToken, CodeResult, LanguageInfo, OcrError, RawText, ReadOptions, Result as OcrResultAlias};
use crate::{EngineId, OcrEngine, OcrError as Error, Result};

pub mod ppocr;
#[cfg(target_os = "macos")]
pub mod vision;

/// The engine Settings starts with on this platform: Vision on macOS, Windows.Media.Ocr on
/// Windows and PP-OCR elsewhere.
pub fn default_engine() -> Arc<dyn OcrEngine> {
    #[cfg(target_os = "macos")]
    let engine: Arc<dyn OcrEngine> = Arc::new(vision::VisionEngine);
    #[cfg(target_os = "windows")]
    let engine: Arc<dyn OcrEngine> = Arc::new(WindowsOcrEngine);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let engine: Arc<dyn OcrEngine> = Arc::new(ppocr::PpOcrEngine::default());
    engine
}

/// The engines that exist on this platform in this build, the default first. PP-OCR is listed
/// only when the build has the `ppocr` feature.
pub fn available_engines() -> Vec<EngineId> {
    [
        #[cfg(target_os = "macos")]
        EngineId::Vision,
        #[cfg(target_os = "windows")]
        EngineId::WindowsMediaOcr,
        #[cfg(feature = "ppocr")]
        EngineId::PpOcr,
    ]
    .to_vec()
}

/// The engines that read text in this build, the default first. PP-OCR stays out until its
/// inference is written ([`ppocr::INFERENCE`]), whatever the `ppocr` feature says, because its
/// engine stops after the model check.
pub fn ready_engines() -> Vec<EngineId> {
    available_engines().into_iter().filter(|id| *id != EngineId::PpOcr || ppocr::INFERENCE).collect()
}

/// The engine with this id, or [`Error::EngineUnavailable`] when it does not exist here.
pub fn engine(id: EngineId) -> Result<Arc<dyn OcrEngine>> {
    match id {
        #[cfg(target_os = "macos")]
        EngineId::Vision => Ok(Arc::new(vision::VisionEngine)),
        #[cfg(target_os = "windows")]
        EngineId::WindowsMediaOcr => Ok(Arc::new(WindowsOcrEngine)),
        #[cfg(feature = "ppocr")]
        EngineId::PpOcr => Ok(Arc::new(match crate::models::shared_store() {
            Some(store) => ppocr::PpOcrEngine::new(store),
            None => ppocr::PpOcrEngine::default(),
        })),
        #[allow(unreachable_patterns)]
        other => Err(Error::EngineUnavailable(other)),
    }
}

/// Reads text with the engine built into Windows (milestone 7).
#[cfg(target_os = "windows")]
#[derive(Clone, Copy, Debug, Default)]
pub struct WindowsOcrEngine;

#[cfg(target_os = "windows")]
impl OcrEngine for WindowsOcrEngine {
    fn id(&self) -> EngineId {
        EngineId::WindowsMediaOcr
    }

    fn read(&self, _image: &Frame, _options: &ReadOptions, _cancel: &CancelToken) -> OcrResultAlias<RawText> {
        Err(OcrError::NotImplemented("text recognition with Windows.Media.Ocr"))
    }

    fn supported_languages(&self) -> OcrResultAlias<Vec<LanguageInfo>> {
        Err(OcrError::NotImplemented("the language list of Windows.Media.Ocr"))
    }

    fn detect_codes(&self, _image: &Frame, _cancel: &CancelToken) -> OcrResultAlias<Vec<CodeResult>> {
        Err(OcrError::NotImplemented("code detection on Windows"))
    }
}
