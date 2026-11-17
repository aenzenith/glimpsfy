//! The one error every command returns, so the UI handles failures the same way everywhere.
//!
//! A command returns `CommandResult<T>`. Its error reaches TypeScript as
//! `{ code: "not_implemented" | …, message: "…" }` and is rethrown there as `ApiError`
//! (`src/lib/api/core.ts`). `code` is for the code to branch on; `message` is English for the log
//! and is never shown to the user as it is: the UI picks its own localized sentence by `code`.
//!
//! Each area converts its crate's error in its own file (`impl From<OcrError> for CommandError`
//! sits in `commands/ocr.rs`), so no area edits this file to add a variant of its own.

use serde::Serialize;

pub type CommandResult<T> = Result<T, CommandError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The contract exists and the implementation does not yet.
    NotImplemented,
    /// A system permission is missing (screen recording, microphone, camera, accessibility).
    PermissionDenied,
    /// The capture, entry, provider or file named in the call does not exist (any more).
    NotFound,
    /// The arguments do not make sense, such as an empty selection.
    InvalidArgument,
    /// Something else is running that this call would disturb, such as a second recording.
    Busy,
    /// Reading or writing a file failed.
    Io,
    /// The platform's capture API failed.
    Capture,
    /// An upload provider refused or failed; the message carries the provider's reason.
    Provider,
    /// The user cancelled.
    Cancelled,
    Internal,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CommandError {
    pub code: ErrorCode,
    pub message: String,
}

impl CommandError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        CommandError { code, message: message.into() }
    }

    pub fn not_implemented(what: &str) -> Self {
        CommandError::new(ErrorCode::NotImplemented, format!("{what} is not implemented yet"))
    }

    pub fn not_found(what: impl Into<String>) -> Self {
        CommandError::new(ErrorCode::NotFound, what)
    }

    pub fn invalid(what: impl Into<String>) -> Self {
        CommandError::new(ErrorCode::InvalidArgument, what)
    }

    pub fn internal(what: impl Into<String>) -> Self {
        CommandError::new(ErrorCode::Internal, what)
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for CommandError {}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        let code = if error.kind() == std::io::ErrorKind::NotFound { ErrorCode::NotFound } else { ErrorCode::Io };
        CommandError::new(code, error.to_string())
    }
}

impl From<tauri::Error> for CommandError {
    fn from(error: tauri::Error) -> Self {
        CommandError::internal(error.to_string())
    }
}

impl From<glimpsfy_capture::CaptureError> for CommandError {
    fn from(error: glimpsfy_capture::CaptureError) -> Self {
        use glimpsfy_capture::CaptureError as E;
        let code = match &error {
            E::NotImplemented(_) => ErrorCode::NotImplemented,
            E::PermissionDenied => ErrorCode::PermissionDenied,
            E::DisplayNotFound(_) | E::WindowNotFound(_) => ErrorCode::NotFound,
            E::InvalidArgument(_) => ErrorCode::InvalidArgument,
            E::Timeout(_) | E::Backend(_) => ErrorCode::Capture,
            E::Encode(_) => ErrorCode::Internal,
        };
        CommandError::new(code, error.to_string())
    }
}
