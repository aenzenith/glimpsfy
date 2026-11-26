//! Cutting a recording by its text: the speech model (listed in Settings › Recording) and the
//! transcript and cuts of a recording in the trim window. Owner: Speech (card C21); the work runs
//! in `recording/transcript.rs`.
//!
//! The speech runs on this machine. The only network traffic is the model download, and only
//! `transcript_model_download` starts it: the trim window's Download button and the one in
//! Settings call it, nothing else does (`crates/ocr/tests/network.rs` checks the sources).

use std::time::{Duration, Instant};

use glimpsfy_record::{RecordingId, TrimRange};
use glimpsfy_speech::model::SpeechModelInfo;
use glimpsfy_speech::{Span, SpeechError, Transcript};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::network::Traffic;
use crate::recording::transcript::{self as work, TranscriptState};

/// Events this area owns. The TypeScript copy is in `src/lib/api/transcript.ts`.
pub mod transcript_events {
    /// A model download made progress. Payload: `commands::transcript::ModelProgress`.
    pub const MODEL_PROGRESS: &str = "transcript://model-progress";
    /// A transcription made progress, with the words read since the last event. Payload:
    /// `recording::transcript::TranscriptProgress`.
    pub const PROGRESS: &str = "transcript://progress";
}

/// How often a model download reports its progress at most.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

impl From<SpeechError> for CommandError {
    fn from(error: SpeechError) -> Self {
        use glimpsfy_models::ModelError;
        let code = match &error {
            SpeechError::Cancelled | SpeechError::Model(ModelError::Cancelled) => ErrorCode::Cancelled,
            SpeechError::Model(ModelError::Missing(_)) => ErrorCode::NotFound,
            SpeechError::Model(ModelError::Download(_)) => ErrorCode::Provider,
            SpeechError::Model(ModelError::Offline) => ErrorCode::Offline,
            SpeechError::Model(ModelError::ChecksumMismatch { .. } | ModelError::Io(_)) | SpeechError::Io(_) => {
                ErrorCode::Io
            }
            SpeechError::Engine(_) => ErrorCode::Internal,
        };
        CommandError::new(code, error.to_string())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProgress {
    pub received: u64,
    pub total: u64,
    pub remaining_ms: Option<u64>,
}

async fn off_main<T: Send + 'static>(work: impl FnOnce() -> CommandResult<T> + Send + 'static) -> CommandResult<T> {
    tauri::async_runtime::spawn_blocking(work).await.map_err(|e| CommandError::internal(e.to_string()))?
}

/// The speech model and whether it is downloaded.
#[tauri::command]
pub async fn transcript_model(app: AppHandle) -> CommandResult<SpeechModelInfo> {
    off_main(move || Ok(work::model_store(&app)?.info())).await
}

/// Downloads the speech model from the reviewed table, reporting `transcript://model-progress`.
/// Only a user's click calls this (the trim window's panel, Settings › Recording); nothing in the
/// app downloads the model on its own. Cancelling deletes the partial file. Fails with `offline`
/// while Settings › General keeps the app offline.
#[tauri::command]
pub async fn transcript_model_download(app: AppHandle, job: Option<String>) -> CommandResult<SpeechModelInfo> {
    let (name, cancel) = work::begin_download(&app, job)?;
    off_main(move || {
        let store = work::model_store(&app)?;
        let emitter = app.clone();
        let started = Instant::now();
        let mut last = Instant::now() - PROGRESS_EVERY;
        let mut progress = |received: u64, total: u64| {
            if received < total && last.elapsed() < PROGRESS_EVERY {
                return;
            }
            last = Instant::now();
            let fraction = received as f64 / total.max(1) as f64;
            let remaining_ms = (fraction > 0.02 && fraction < 1.0).then(|| {
                let spent = started.elapsed().as_millis() as f64;
                (spent / fraction * (1.0 - fraction)) as u64
            });
            let _ = emitter.emit(transcript_events::MODEL_PROGRESS, ModelProgress { received, total, remaining_ms });
        };
        // Work offline is asked before the first connection and again at every hop and chunk.
        let outcome = store.download(&mut progress, &cancel, &|| crate::network::online(&app, Traffic::ModelDownload));
        work::end_download(&app, &name);
        outcome?;
        Ok(store.info())
    })
    .await
}

/// Deletes the downloaded speech model. Refused while a transcription or a download runs.
#[tauri::command]
pub async fn transcript_model_remove(app: AppHandle) -> CommandResult<()> {
    if work::running(&app) || work::downloading(&app) {
        return Err(CommandError::new(ErrorCode::Busy, "the speech model is in use"));
    }
    off_main(move || Ok(work::model_store(&app)?.remove()?)).await
}

/// The saved transcript of a recording, when it still belongs to the file, and the running job.
#[tauri::command]
pub async fn transcript_get(app: AppHandle, recording_id: RecordingId) -> CommandResult<TranscriptState> {
    off_main(move || work::get(&app, &recording_id)).await
}

/// Transcribes a recording on this machine and saves the text; `language` is a speech model code
/// or "auto". A second job while one runs fails with `busy`.
#[tauri::command]
pub async fn transcript_start(
    app: AppHandle,
    recording_id: RecordingId,
    language: String,
) -> CommandResult<Transcript> {
    work::start(app, recording_id, language).await
}

/// Stops the model download started under `job`, or the transcription when `job` is null.
#[tauri::command]
pub fn transcript_cancel(app: AppHandle, job: Option<String>) {
    match job {
        Some(job) => work::cancel_download(&app, &job),
        None => work::cancel(&app),
    }
}

/// Saves the cuts with the recording's transcript, so the recording opens with them again.
#[tauri::command]
pub async fn transcript_save_cuts(
    app: AppHandle,
    recording_id: RecordingId,
    cuts: Vec<TrimRange>,
) -> CommandResult<()> {
    let cuts = cuts.into_iter().map(|c| Span { start_ms: c.start_ms, end_ms: c.end_ms }).collect();
    off_main(move || work::save_cuts(&app, &recording_id, cuts)).await
}

/// Saves the user's corrections of words in the recording's transcript (the subtitles show
/// them); a `null` text gives a word back its own. Returns the words as saved.
#[tauri::command]
pub async fn transcript_save_edits(
    app: AppHandle,
    recording_id: RecordingId,
    edits: Vec<work::WordEdit>,
) -> CommandResult<Vec<glimpsfy_speech::Word>> {
    off_main(move || work::save_edits(&app, &recording_id, edits)).await
}
