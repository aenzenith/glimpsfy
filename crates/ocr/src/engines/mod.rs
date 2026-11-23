//! The engines and the choice between them: Vision on macOS, Windows.Media.Ocr on Windows
//! (milestone 7) and PP-OCR wherever the build has it.

use std::sync::Arc;

use crate::{EngineId, OcrEngine, OcrError as Error, Result};

pub mod ppocr;
#[cfg(target_os = "macos")]
pub mod vision;
#[cfg(target_os = "windows")]
pub mod windows;

/// The engine Settings starts with on this platform: Vision on macOS, Windows.Media.Ocr on
/// Windows and PP-OCR elsewhere.
pub fn default_engine() -> Arc<dyn OcrEngine> {
    #[cfg(target_os = "macos")]
    let engine: Arc<dyn OcrEngine> = Arc::new(vision::VisionEngine);
    #[cfg(target_os = "windows")]
    let engine: Arc<dyn OcrEngine> = Arc::new(windows::WindowsOcrEngine);
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

/// The engines that read text in this build, the default first. PP-OCR is among them when the
/// build has its inference ([`ppocr::INFERENCE`]), which comes with the `ppocr` feature.
pub fn ready_engines() -> Vec<EngineId> {
    available_engines().into_iter().filter(|id| *id != EngineId::PpOcr || ppocr::INFERENCE).collect()
}

/// The engine with this id, or [`Error::EngineUnavailable`] when it does not exist here.
pub fn engine(id: EngineId) -> Result<Arc<dyn OcrEngine>> {
    match id {
        #[cfg(target_os = "macos")]
        EngineId::Vision => Ok(Arc::new(vision::VisionEngine)),
        #[cfg(target_os = "windows")]
        EngineId::WindowsMediaOcr => Ok(Arc::new(windows::WindowsOcrEngine)),
        #[cfg(feature = "ppocr")]
        EngineId::PpOcr => Ok(Arc::new(match crate::models::shared_store() {
            Some(store) => ppocr::PpOcrEngine::new(store),
            None => ppocr::PpOcrEngine::default(),
        })),
        #[allow(unreachable_patterns)]
        other => Err(Error::EngineUnavailable(other)),
    }
}
