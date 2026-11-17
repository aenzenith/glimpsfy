use crate::display::{DisplayId, WindowId};

pub type Result<T, E = CaptureError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// The contract exists, the implementation does not yet. Every stub returns this and never
    /// panics, so a caller can show "not available yet" instead of crashing.
    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),

    /// The screen recording permission is missing.
    #[error("the screen recording permission is not granted")]
    PermissionDenied,

    #[error("display {0:?} is not connected")]
    DisplayNotFound(DisplayId),

    #[error("window {0:?} no longer exists")]
    WindowNotFound(WindowId),

    #[error("{0}")]
    InvalidArgument(String),

    /// The system did not answer in time.
    #[error("the capture did not finish within {0} ms")]
    Timeout(u64),

    /// The platform API failed; the message carries its own words.
    #[error("capture failed: {0}")]
    Backend(String),

    #[error("could not encode the image: {0}")]
    Encode(String),
}
