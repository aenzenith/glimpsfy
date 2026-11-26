//! The text of a recording for the trim window's text panel: the one transcription at a time on
//! the `glimpsfy-speech` thread, its cancel, the transcript files in `<app data>/transcripts`, and
//! removing them with the recording they belong to.
//!
//! A transcript is named after the history entry of its recording, or `r-<recording id>` for a
//! take the history does not keep. It holds the recording's fingerprint (size, length, change
//! time), so a recording opened again is not transcribed again, and a file that changed is. Cuts
//! are saved with it and never touch the recording; the export applies them to a new file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use glimpsfy_record::{RecordingId, TrackKind, TrimRange};
use glimpsfy_speech::model::SpeechModelStore;
use glimpsfy_speech::{CancelToken, Fingerprint, Span, Transcript, Word};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use super::media::Known;
use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::commands::transcript::transcript_events;
use crate::settings::write_atomically;
use crate::trace;

/// How often a transcription reports its progress at most; the words in between are kept and
/// sent with the next report, so none is lost.
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_millis(250);

/// `transcript://progress`, and the running job in `transcript_get`.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptProgress {
    pub recording_id: RecordingId,
    pub done_ms: u64,
    pub total_ms: u64,
    pub remaining_ms: Option<u64>,
    pub words: Vec<Word>,
}

/// What the trim window shows for a recording.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptState {
    pub transcript: Option<Transcript>,
    pub running: Option<TranscriptProgress>,
}

/// The transcription that runs now. Its words so far are kept, so a window that opens while it
/// runs shows the text that is already read.
struct Job {
    recording_id: RecordingId,
    /// The transcript file's name, so removing a recording's text also stops the job writing it.
    key: String,
    cancel: CancelToken,
    /// The same cancel for the audio decoder, which takes a plain flag.
    stop: Arc<AtomicBool>,
    progress: TranscriptProgress,
}

/// The speech area's state, managed by Tauri.
#[derive(Default)]
pub struct Speech {
    job: Mutex<Option<Job>>,
    /// Model downloads by the job name the page gave, for `transcript_cancel(job)`.
    downloads: Mutex<HashMap<String, CancelToken>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn speech(app: &AppHandle) -> &Speech {
    app.state::<Speech>().inner()
}

/// Makes the speech state and clears transcripts no recording can open any more. Once, at start.
pub fn setup(app: &AppHandle) {
    app.manage(Speech::default());
    let handle = app.clone();
    let _ = std::thread::Builder::new().name("glimpsfy-speech-sweep".into()).spawn(move || sweep(&handle));
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default()
}

/// The folder of the speech model and the OCR models, `<app data>/models`.
pub fn model_store(app: &AppHandle) -> CommandResult<SpeechModelStore> {
    let root = crate::paths::data_dir(app).map_err(|e| CommandError::internal(e.to_string()))?.join("models");
    Ok(SpeechModelStore::new(root))
}

fn folder(app: &AppHandle) -> Option<PathBuf> {
    crate::paths::data_dir(app).ok().map(|d| d.join("transcripts"))
}

/// A history entry id as a file name: the characters a recording id may hold, as
/// `media::entry_recording_id` keeps them.
fn clean(id: &str) -> String {
    id.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').take(60).collect()
}

/// The transcript's name for a recording: its history entry, so the take and the same entry
/// opened from the history share one text, or the take's own id.
fn key(known: &Known) -> String {
    match &known.entry_id {
        Some(entry) => clean(entry),
        None => format!("r-{}", clean(&known.result.id.0)),
    }
}

fn path_of(app: &AppHandle, key: &str) -> CommandResult<PathBuf> {
    folder(app).map(|d| d.join(format!("{key}.json"))).ok_or_else(|| CommandError::internal("no data folder"))
}

fn known(app: &AppHandle, id: &RecordingId) -> CommandResult<Known> {
    super::state(app).library.get(&id.0).ok_or_else(|| CommandError::not_found("the recording is not known any more"))
}

fn fingerprint(known: &Known) -> Fingerprint {
    let meta = std::fs::metadata(&known.result.path).ok();
    let modified_ms = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Fingerprint {
        bytes: meta.map(|m| m.len()).unwrap_or(known.result.file_bytes),
        duration_ms: known.result.duration_ms,
        modified_ms,
    }
}

fn read(path: &Path) -> Option<Transcript> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<Transcript>(&bytes).ok().filter(|t| t.version == glimpsfy_speech::TRANSCRIPT_VERSION)
}

fn write(path: &Path, transcript: &Transcript) -> CommandResult<()> {
    let bytes = serde_json::to_vec(transcript).map_err(|e| CommandError::internal(e.to_string()))?;
    write_atomically(path, &bytes).map_err(|e| CommandError::new(ErrorCode::Io, e.to_string()))
}

/// The saved transcript of `known`, when it still belongs to the file. A take's text written
/// before the history kept it moves to the entry's name.
fn saved(app: &AppHandle, known: &Known) -> CommandResult<Option<Transcript>> {
    let path = path_of(app, &key(known))?;
    if !path.is_file() && known.entry_id.is_some() {
        let take = path_of(app, &format!("r-{}", clean(&known.result.id.0)))?;
        if take.is_file() {
            let _ = std::fs::rename(&take, &path);
        }
    }
    Ok(read(&path).filter(|t| t.fingerprint == fingerprint(known)))
}

pub fn get(app: &AppHandle, id: &RecordingId) -> CommandResult<TranscriptState> {
    let known = known(app, id)?;
    let running = lock(&speech(app).job).as_ref().filter(|j| j.recording_id == *id).map(|j| j.progress.clone());
    Ok(TranscriptState { transcript: saved(app, &known)?, running })
}

/// The audio track the speech is read from: the microphone's own, else the mix, else the only
/// one. Indexed among the file's audio tracks, as the export picks its track.
fn speech_track(known: &Known) -> Option<usize> {
    let audio: Vec<TrackKind> = known.result.tracks.iter().map(|t| t.kind).filter(|&k| k != TrackKind::Video).collect();
    audio
        .iter()
        .position(|&k| k == TrackKind::Microphone)
        .or_else(|| audio.iter().position(|&k| k == TrackKind::MixedAudio))
        .or((!audio.is_empty()).then_some(0))
}

/// Transcribes a known recording on the `glimpsfy-speech` thread and saves the text. One job runs
/// at a time; a second one fails with `busy`. Cuts saved for the same file are kept.
pub async fn start(app: AppHandle, id: RecordingId, language: String) -> CommandResult<Transcript> {
    let known = known(&app, &id)?;
    let store = model_store(&app)?;
    if !store.info().installed {
        return Err(CommandError::not_found("the speech model is not downloaded"));
    }
    let track = speech_track(&known).ok_or_else(|| CommandError::invalid("the recording has no sound to read"))?;
    let cancel = CancelToken::new();
    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut job = lock(&speech(&app).job);
        if job.is_some() {
            return Err(CommandError::new(ErrorCode::Busy, "another recording is being transcribed"));
        }
        *job = Some(Job {
            recording_id: id.clone(),
            key: key(&known),
            cancel: cancel.clone(),
            stop: stop.clone(),
            progress: TranscriptProgress {
                recording_id: id.clone(),
                done_ms: 0,
                total_ms: known.result.duration_ms,
                remaining_ms: None,
                words: Vec::new(),
            },
        });
    }
    let (tx, mut rx) = tauri::async_runtime::channel(1);
    let handle = app.clone();
    let spawned = std::thread::Builder::new().name("glimpsfy-speech".into()).spawn(move || {
        let started = Instant::now();
        let outcome = run(&handle, &known, &store, track, &language, &cancel, &stop);
        *lock(&speech(&handle).job) = None;
        match &outcome {
            Ok(t) => trace::log(format!(
                "[glimpsfy] speech: {} words of {} ms in {} ms, language {}",
                t.words.len(),
                t.duration_ms,
                started.elapsed().as_millis(),
                t.language
            )),
            Err(e) => trace::log(format!("[glimpsfy] speech: transcription stopped: {}", e.message)),
        }
        let _ = tx.blocking_send(outcome);
    });
    if let Err(error) = spawned {
        *lock(&speech(&app).job) = None;
        return Err(CommandError::internal(error.to_string()));
    }
    rx.recv().await.unwrap_or_else(|| Err(CommandError::internal("the transcription stopped without an answer")))
}

fn cancelled(error: CommandError, cancel: &CancelToken) -> CommandError {
    if cancel.is_cancelled() {
        CommandError::new(ErrorCode::Cancelled, "the transcription was cancelled")
    } else {
        error
    }
}

fn run(
    app: &AppHandle,
    known: &Known,
    store: &SpeechModelStore,
    track: usize,
    language: &str,
    cancel: &CancelToken,
    stop: &AtomicBool,
) -> CommandResult<Transcript> {
    // The sound is brought to the model's rate while it is decoded, so a long recording is never
    // held at 48 kHz stereo.
    let mut audio =
        Vec::with_capacity((known.result.duration_ms as usize / 1000 + 1) * glimpsfy_speech::SAMPLE_RATE as usize);
    let mut downsampler = glimpsfy_speech::audio::Downsampler::new();
    let whole = TrimRange { start_ms: 0, end_ms: known.result.duration_ms.max(1) };
    glimpsfy_record::platform::decode_audio(&known.result.path, track, whole, stop, &mut |samples, _| {
        downsampler.push(samples, &mut audio);
        Ok(())
    })
    .map_err(|e| cancelled(e.into(), cancel))?;
    downsampler.finish(&mut audio);

    let engine = glimpsfy_speech::whisper::WhisperEngine::load(store).map_err(|e| cancelled(e.into(), cancel))?;
    let options = glimpsfy_speech::Options {
        language: (language != "auto" && !language.is_empty()).then(|| language.to_string()),
        voiced_gap_fillers: false,
    };
    let id = known.result.id.clone();
    let reading = Instant::now();
    let mut last = Instant::now() - PROGRESS_EVERY;
    let mut pending: Vec<Word> = Vec::new();
    let mut report = |p: glimpsfy_speech::Progress, flush: bool| {
        pending.extend(p.words.iter().cloned());
        let remaining_ms = (p.done_ms > 0 && p.total_ms > p.done_ms).then(|| {
            let spent = reading.elapsed().as_millis() as u64;
            spent * (p.total_ms - p.done_ms) / p.done_ms
        });
        if let Some(job) = lock(&speech(app).job).as_mut() {
            job.progress.done_ms = p.done_ms;
            job.progress.total_ms = p.total_ms;
            job.progress.remaining_ms = remaining_ms;
            job.progress.words.extend(p.words);
        }
        if !flush && last.elapsed() < PROGRESS_EVERY {
            return;
        }
        last = Instant::now();
        let payload = TranscriptProgress {
            recording_id: id.clone(),
            done_ms: p.done_ms,
            total_ms: p.total_ms,
            remaining_ms,
            words: std::mem::take(&mut pending),
        };
        let _ = app.emit(transcript_events::PROGRESS, payload);
    };
    let analysis = glimpsfy_speech::analyze(&engine, &audio, &options, &mut |p| report(p, false), cancel)
        .map_err(|e| cancelled(e.into(), cancel))?;
    drop(engine);
    let total_ms = known.result.duration_ms;
    report(glimpsfy_speech::Progress { done_ms: total_ms, total_ms, words: Vec::new() }, true);

    let path = path_of(app, &key(known))?;
    let fingerprint = fingerprint(known);
    // A new reading of the same file (another language) keeps the cuts the user made.
    let cuts = read(&path).filter(|t| t.fingerprint == fingerprint).map(|t| t.cuts).unwrap_or_default();
    let transcript = Transcript {
        version: glimpsfy_speech::TRANSCRIPT_VERSION,
        fingerprint,
        language: analysis.language,
        language_auto: analysis.language_auto,
        language_probability: analysis.language_probability,
        model: glimpsfy_speech::model::MODEL_ID.to_string(),
        duration_ms: total_ms,
        words: analysis.words,
        silences: analysis.silences,
        cuts,
        created_at_ms: now_ms(),
    };
    // The check and the write hold the job's lock, which `forget` takes to cancel: a transcript is
    // written either before its recording's text is removed, or not at all.
    let _job = lock(&speech(app).job);
    if cancel.is_cancelled() {
        return Err(CommandError::new(ErrorCode::Cancelled, "the transcription was cancelled"));
    }
    write(&path, &transcript)?;
    Ok(transcript)
}

/// Stops the running transcription.
pub fn cancel(app: &AppHandle) {
    if let Some(job) = lock(&speech(app).job).as_ref() {
        job.cancel.cancel();
        job.stop.store(true, Ordering::SeqCst);
    }
}

/// How long an exit waits for a cancelled transcription; with Metal a cancel lands in about
/// 0.3 s, on the CPU in about 2 s.
const EXIT_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Asked when the app is about to exit (`RunEvent::ExitRequested`): a running transcription is
/// cancelled and waited for, so its engine is dropped before the process ends. ggml's Metal device
/// asserts at exit while a model still holds GPU memory, which would abort the quit.
pub fn end_for_exit(app: &AppHandle) {
    let Some(state) = app.try_state::<Speech>() else { return };
    match lock(&state.job).as_ref() {
        Some(job) => {
            job.cancel.cancel();
            job.stop.store(true, Ordering::SeqCst);
        }
        None => return,
    }
    let deadline = Instant::now() + EXIT_WAIT;
    while lock(&state.job).is_some() && Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Whether a transcription runs now, so the model is not removed under it.
pub fn running(app: &AppHandle) -> bool {
    lock(&speech(app).job).is_some()
}

/// Registers a model download under the page's job name, or fails when one already runs.
pub fn begin_download(app: &AppHandle, job: Option<String>) -> CommandResult<(String, CancelToken)> {
    let mut downloads = lock(&speech(app).downloads);
    if !downloads.is_empty() {
        return Err(CommandError::new(ErrorCode::Busy, "the speech model is already being downloaded"));
    }
    let name = job.unwrap_or_else(|| "transcript-model".into());
    let cancel = CancelToken::new();
    downloads.insert(name.clone(), cancel.clone());
    Ok((name, cancel))
}

pub fn end_download(app: &AppHandle, job: &str) {
    lock(&speech(app).downloads).remove(job);
}

/// Stops the model download started under `job`.
pub fn cancel_download(app: &AppHandle, job: &str) {
    if let Some(cancel) = lock(&speech(app).downloads).get(job) {
        cancel.cancel();
    }
}

/// Whether a model download runs now.
pub fn downloading(app: &AppHandle) -> bool {
    !lock(&speech(app).downloads).is_empty()
}

/// Saves the cuts with the recording's transcript: sorted, merged and inside the recording.
pub fn save_cuts(app: &AppHandle, id: &RecordingId, cuts: Vec<Span>) -> CommandResult<()> {
    let known = known(app, id)?;
    let mut transcript =
        saved(app, &known)?.ok_or_else(|| CommandError::not_found("the recording has no transcript yet"))?;
    transcript.cuts = tidy(cuts, transcript.duration_ms);
    write(&path_of(app, &key(&known))?, &transcript)
}

/// The longest correction a word takes; a longer text is no correction of one word.
const MAX_EDIT_CHARS: usize = 200;

/// A correction of one word in the text panel: its index in the transcript and the text that
/// replaces it, or `None` to give it back the model's text.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WordEdit {
    pub index: usize,
    pub text: Option<String>,
}

/// Saves corrections of words with the recording's transcript and returns its words. A
/// correction equal to the model's text clears it, and an empty one hides the word.
pub fn save_edits(app: &AppHandle, id: &RecordingId, edits: Vec<WordEdit>) -> CommandResult<Vec<Word>> {
    let known = known(app, id)?;
    let mut transcript =
        saved(app, &known)?.ok_or_else(|| CommandError::not_found("the recording has no transcript yet"))?;
    for edit in &edits {
        let word = transcript
            .words
            .get_mut(edit.index)
            .ok_or_else(|| CommandError::invalid("a correction names a word the transcript does not hold"))?;
        let text = edit.text.as_deref().map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "));
        if text.as_ref().is_some_and(|t| t.chars().count() > MAX_EDIT_CHARS) {
            return Err(CommandError::invalid("a correction is too long for one word"));
        }
        word.edited = text.filter(|t| *t != word.text.trim());
    }
    write(&path_of(app, &key(&known))?, &transcript)?;
    Ok(transcript.words)
}

/// The transcript an export's subtitles are made from: the saved one, while it belongs to the
/// file.
pub fn for_export(app: &AppHandle, known: &Known) -> CommandResult<Transcript> {
    saved(app, known)?.ok_or_else(|| CommandError::not_found("the recording has no transcript yet"))
}

/// Cuts as the file keeps them: inside `0..duration`, sorted, with overlapping and touching
/// ones merged, and empty ones dropped.
fn tidy(mut cuts: Vec<Span>, duration_ms: u64) -> Vec<Span> {
    for cut in &mut cuts {
        cut.end_ms = cut.end_ms.min(duration_ms);
        cut.start_ms = cut.start_ms.min(cut.end_ms);
    }
    cuts.retain(|c| c.end_ms > c.start_ms);
    cuts.sort_by_key(|c| c.start_ms);
    let mut out: Vec<Span> = Vec::with_capacity(cuts.len());
    for cut in cuts {
        match out.last_mut() {
            Some(last) if cut.start_ms <= last.end_ms => last.end_ms = last.end_ms.max(cut.end_ms),
            _ => out.push(cut),
        }
    }
    out
}

/// Cancels the running transcription when it writes one of `keys`. A transcription that goes on
/// after its recording was removed would write the text back once it ends.
fn cancel_writing(app: &AppHandle, keys: &[String]) {
    if let Some(job) = lock(&speech(app).job).as_ref().filter(|j| keys.contains(&j.key)) {
        job.cancel.cancel();
        job.stop.store(true, Ordering::SeqCst);
    }
}

/// Removes the transcript of a recording that went to the Trash, and stops the reading of it.
pub fn forget(app: &AppHandle, known: &Known) {
    let keys = [key(known), format!("r-{}", clean(&known.result.id.0))];
    cancel_writing(app, &keys);
    for key in &keys {
        if let Ok(path) = path_of(app, key) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// Removes the transcripts of history entries that were deleted, so no text of a deleted
/// recording stays on disk.
pub fn forget_entries(app: &AppHandle, ids: &[glimpsfy_history::EntryId]) {
    let Some(dir) = folder(app) else { return };
    let keys: Vec<String> = ids.iter().map(|id| clean(&id.0)).collect();
    cancel_writing(app, &keys);
    for key in &keys {
        let _ = std::fs::remove_file(dir.join(format!("{key}.json")));
    }
}

/// At start: a take the history did not keep cannot be opened again after a restart, so its
/// text goes; an entry's text goes when the entry is gone; a half-written file goes.
fn sweep(app: &AppHandle) {
    let Some(dir) = folder(app) else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    let history = crate::commands::history::store(app).ok();
    let mut removed = 0;
    for path in files {
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let gone = if name.ends_with(".tmp") {
            true
        } else if let Some(stem) = name.strip_suffix(".json") {
            if stem.starts_with("r-") {
                true
            } else {
                // Without the history (still opening, or off) nothing is known to be gone.
                history.as_ref().is_some_and(|store| {
                    matches!(
                        store.get(&glimpsfy_history::EntryId(stem.to_string())),
                        Err(glimpsfy_history::HistoryError::NotFound(_))
                    )
                })
            }
        } else {
            false
        };
        if gone && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    if removed > 0 {
        trace::log(format!("[glimpsfy] speech: removed {removed} transcript(s) no recording opens"));
    }
}
