//! History commands and the history's runtime: the store in the app's data directory, the
//! ingestion of new captures and uploads, thumbnails and text recognition in the background,
//! the retention sweep, and the `glimpsfy-history://` scheme that serves thumbnails and files.
//! Owner: history (milestone 4).
//!
//! - **Store.** `setup` opens `<app data dir>/history/history.sqlite3` (`glimpsfy_history`). A
//!   database that cannot be opened (damaged, or from a newer Glimpsfy) is left alone, and every
//!   command then fails with `internal` and the reason, so the window can say so.
//! - **Ingestion.** Output's `output://handoff` (a [`CaptureHandoff`]) reaches [`record_capture`],
//!   which other areas (recording, text copies) may also call directly; the entry is written at
//!   once, its thumbnail is made on the `glimpsfy-history-thumbs` thread and its text read on the
//!   `glimpsfy-history-text` thread. A clipboard-only capture's PNG is moved out of output's
//!   inbox (which output empties of day-old files) before its row exists, and moved back if the
//!   row cannot be written. `upload://done` records the link on its entry, and an upload with no
//!   entry becomes a link-only entry, so no link or deletion handle is lost.
//! - **The editor's outputs.** A capture handed to the editor (⌘O) is recorded when it leaves
//!   the editor (Copy, Save, Save as), as an overlay capture is recorded when it is copied or
//!   saved, and not at the hand-off, where it might still be thrown away. Every output of one
//!   editor image lands in one entry: the first adds it, the later ones revise it
//!   (`HistoryStore::revise`). The entry keeps the editor's document and the image the drawings
//!   sit on, so reopening it brings the drawings back editable. See [`EditorRecord`].
//! - **Retention.** At start and every day the `glimpsfy-history` thread removes what Settings ›
//!   Output › Keep captures says, and logs it. With the history off, nothing new is recorded and
//!   nothing is swept: the entries already there stay until the user deletes or clears them.
//! - **Images.** Pages never receive pixels over IPC. They load `glimpsfy-history://thumb/<id>`
//!   and `glimpsfy-history://file/<id>` (on Windows `http://glimpsfy-history.localhost/…`), and
//!   the scheme serves only the thumbnail and the capture file of an entry that exists, so its
//!   reach is exactly the history's entries.
//!
//! The store blocks on the database and the disk, so every command runs it on a blocking worker
//! thread and never on the async runtime's threads.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use glimpsfy_capture::frame::Frame;
use glimpsfy_capture::PngEffort;
use glimpsfy_history::{
    DeleteOptions, DeleteReport, Entry, EntryDisplay, EntryId, EntryKind, EntrySource, HistoryError, HistoryPage,
    HistoryPaths, HistoryQuery, HistoryStore, HistoryUsage, HostDeletion, HostedCopies, HostedCopy, Media, NewEntry,
    OwnFile, PixelSize, RecognizedText, Retention, Revision, SweepOptions, SystemTrash, TextBox, TextLine, TextWord,
};
use glimpsfy_upload::{DeleteOutcome, UploadResult};
use serde::Deserialize;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{AppHandle, Emitter, Listener, Manager, UriSchemeContext, UriSchemeResponder};

use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::settings::HistoryRetention;
use crate::{trace, AppState};

impl From<HistoryError> for CommandError {
    fn from(error: HistoryError) -> Self {
        let code = match &error {
            HistoryError::NotImplemented(_) => ErrorCode::NotImplemented,
            HistoryError::NotFound(_) => ErrorCode::NotFound,
            HistoryError::InvalidArgument(_) | HistoryError::Image(_) => ErrorCode::InvalidArgument,
            HistoryError::Storage(_) | HistoryError::NewerSchema { .. } => ErrorCode::Internal,
            HistoryError::Io(io) if io.kind() == std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            HistoryError::Io(_) => ErrorCode::Io,
        };
        CommandError::new(code, error.to_string())
    }
}

/// Event names of the history area and their payloads.
pub mod events {
    use glimpsfy_history::{Entry, EntryId};
    use serde::{Deserialize, Serialize};

    /// The history changed, from any window or from the core (a new capture, an upload, the
    /// background text recognition, retention). Payload: [`HistoryChanged`].
    pub const CHANGED: &str = "history://changed";

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "kind", rename_all = "camelCase")]
    pub enum HistoryChanged {
        /// A new entry. Like the items of a page, it leaves out the text; `history_get` has it.
        Added {
            entry: Entry,
        },
        Updated {
            entry: Entry,
        },
        Removed {
            ids: Vec<EntryId>,
        },
    }
}

use events::HistoryChanged;

// ------------------------------------------------------------------------------------- state

/// The history's part of the app's state.
pub struct HistoryState {
    /// `None` until the `glimpsfy-history` thread has opened the database.
    store: Mutex<Option<Result<Arc<HistoryStore>, String>>>,
    opened: Condvar,
    thumbnails: Mutex<Option<Sender<EntryId>>>,
    /// Wakes the text recognition thread when an entry starts waiting for it.
    text_wake: Arc<(Mutex<bool>, Condvar)>,
}

/// How long a caller waits for the database to open at start (a migration or an index rebuild).
const OPEN_WAIT: Duration = Duration::from_secs(20);

/// The store, for the commands and for other areas (the upload area records links with
/// `set_hosted`). Fails with `internal` when the database could not be opened. It waits while
/// the database is still opening, so call it from a blocking thread only.
pub fn store(app: &AppHandle) -> CommandResult<Arc<HistoryStore>> {
    let state =
        app.try_state::<HistoryState>().ok_or_else(|| CommandError::internal("the history is not set up yet"))?;
    let guard = state.store.lock().unwrap_or_else(|p| p.into_inner());
    let (guard, _) =
        state.opened.wait_timeout_while(guard, OPEN_WAIT, |store| store.is_none()).unwrap_or_else(|p| p.into_inner());
    match guard.as_ref() {
        Some(store) => store.clone().map_err(CommandError::internal),
        None => Err(CommandError::internal("the history is still opening its database")),
    }
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or_default()
}

fn retention(app: &AppHandle) -> Retention {
    match app.state::<AppState>().settings.get().output.history_retention {
        HistoryRetention::Off => Retention::Off,
        HistoryRetention::Days7 => Retention::Days7,
        HistoryRetention::Days30 => Retention::Days30,
        HistoryRetention::Days90 => Retention::Days90,
        HistoryRetention::Forever => Retention::Forever,
    }
}

/// Pages and events leave the text out to stay small.
fn light(mut entry: Entry) -> Entry {
    entry.text = None;
    entry.text_lines = None;
    entry
}

fn changed(app: &AppHandle, change: HistoryChanged) {
    let _ = app.emit(events::CHANGED, change);
}

/// Starts the history: the `glimpsfy-history` thread opens the database (never the main
/// thread) and then its workers, and the listeners are in place at once. Once, at start, after
/// `AppState` is managed.
pub fn setup(app: &AppHandle) {
    let started = SystemTime::now();
    let (sender, receiver) = mpsc::channel::<EntryId>();
    let text_wake = Arc::new((Mutex::new(false), Condvar::new()));
    app.manage(HistoryState {
        store: Mutex::new(None),
        opened: Condvar::new(),
        thumbnails: Mutex::new(Some(sender)),
        text_wake: text_wake.clone(),
    });

    // Open, remove what a crash left, queue what is missing, then sweep once a day.
    {
        let app = app.clone();
        let _ = std::thread::Builder::new().name("glimpsfy-history".into()).spawn(move || {
            let opened = crate::paths::data_dir(&app)
                .map_err(|error| format!("the app data folder is unknown: {error}"))
                .and_then(|dir| {
                    HistoryStore::open(HistoryPaths::new(dir.join("history")), Arc::new(SystemTrash))
                        .map_err(|e| e.to_string())
                })
                .map(Arc::new);
            if let Err(error) = &opened {
                trace::log(format!("[glimpsfy] the history is unavailable: {error}"));
            }
            if let Some(state) = app.try_state::<HistoryState>() {
                *state.store.lock().unwrap_or_else(|p| p.into_inner()) = Some(opened.clone());
                state.opened.notify_all();
            }
            let Ok(store) = opened else { return };

            // Thumbnails, one at a time, in order of arrival.
            {
                let (app, store) = (app.clone(), store.clone());
                let _ = std::thread::Builder::new().name("glimpsfy-history-thumbs".into()).spawn(move || {
                    for id in receiver {
                        make_thumbnail(&app, &store, &id);
                    }
                });
            }
            // Text recognition, newest entry first, retrying an engine that is not available later.
            {
                let (app, store) = (app.clone(), store.clone());
                let _ = std::thread::Builder::new()
                    .name("glimpsfy-history-text".into())
                    .spawn(move || text_loop(&app, &store, &text_wake));
            }

            match store.reconcile(Some(started)) {
                Ok(report) => {
                    if !report.removed_files.is_empty() {
                        trace::log(format!(
                            "[glimpsfy] history: removed {} file(s) no entry names",
                            report.removed_files.len()
                        ));
                    }
                    for id in report.missing_thumbnails {
                        queue_thumbnail(&app, id);
                    }
                }
                Err(error) => trace::log(format!("[glimpsfy] history: the start-up check failed: {error}")),
            }
            if let Err(error) = store.retry_text_now() {
                trace::log(format!("[glimpsfy] history: {error}"));
            }
            wake_text(&app);
            // The clock is checked every hour, because a sleeping Mac stops a long `sleep`.
            let mut last_sweep: Option<SystemTime> = None;
            loop {
                let due = last_sweep.is_none_or(|last| last.elapsed().map_or(true, |e| e >= SWEEP_EVERY));
                if due {
                    sweep(&app, &store);
                    last_sweep = Some(SystemTime::now());
                }
                std::thread::sleep(Duration::from_secs(60 * 60));
            }
        });
    }

    // Uploads record their link on their entry. An upload with no entry (a capture that was
    // only uploaded, never copied or saved) becomes an entry of its own, so its link and deletion
    // handle are never lost.
    {
        let handle = app.clone();
        app.listen_any(crate::commands::upload::events::DONE, move |event| {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct Done {
                result: UploadResult,
                entry_id: Option<EntryId>,
                #[serde(default)]
                file_name: String,
            }
            let done = match serde_json::from_str::<Done>(event.payload()) {
                Ok(done) => done,
                Err(error) => return trace::log(format!("[glimpsfy] history: unreadable upload://done: {error}")),
            };
            let app = handle.clone();
            tauri::async_runtime::spawn_blocking(move || match &done.entry_id {
                Some(id) => record_upload(&app, id, &done.file_name, &done.result),
                None => record_link_only(&app, &done.file_name, &done.result),
            });
        });
    }

    // Every image output delivered (`output://handoff`).
    {
        let handle = app.clone();
        app.listen_any(crate::commands::output::events::HANDOFF, move |event| {
            match serde_json::from_str::<CaptureHandoff>(event.payload()) {
                // Only the editor delivers images that belong to no capture.
                Ok(handoff) if handoff.capture_id == Some(0) => match take_editor_ticket() {
                    Some(ticket) => queue_editor_output(&handle, ticket, handoff),
                    None => {
                        let app = handle.clone();
                        tauri::async_runtime::spawn_blocking(move || {
                            if let Err(error) = record_capture(&app, handoff) {
                                trace::log(format!("[glimpsfy] history: a capture was not recorded: {error}"));
                            }
                        });
                    }
                },
                Ok(handoff) => {
                    let app = handle.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        if let Err(error) = record_capture(&app, handoff) {
                            trace::log(format!("[glimpsfy] history: a capture was not recorded: {error}"));
                        }
                    });
                }
                Err(error) => trace::log(format!("[glimpsfy] history: unreadable hand-off: {error}")),
            }
        });
    }
}

const SWEEP_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

fn sweep(app: &AppHandle, store: &HistoryStore) {
    let keep = retention(app);
    // Read now rather than at start, so a switch changed in Settings counts from the next sweep.
    let options = SweepOptions { trash_saved_files: app.state::<AppState>().settings.get().output.trash_saved_files };
    match store.sweep(now_ms(), keep, options) {
        Ok(report) => {
            let removed = report.removed();
            trace::log(format!(
                "[glimpsfy] history: retention {keep:?} removed {} entr{} and freed {} bytes{}",
                removed.len(),
                if removed.len() == 1 { "y" } else { "ies" },
                report.freed_bytes,
                if report.kept().is_empty() {
                    String::new()
                } else {
                    format!(", kept {} it could not finish", report.kept().len())
                }
            ));
            if !removed.is_empty() {
                changed(app, HistoryChanged::Removed { ids: removed });
            }
        }
        Err(error) => trace::log(format!("[glimpsfy] history: the retention sweep failed: {error}")),
    }
}

// ---------------------------------------------------------------------------------- ingest

/// A finished capture, as the history records it: the payload of output's `output://handoff`
/// (`commands/output.rs`, which only serialises it), read here field by field, so a field either
/// side adds later is ignored or defaulted. A recording or a text copy can be passed to
/// [`record_capture`] directly.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CaptureHandoff {
    pub kind: Option<EntryKind>,
    /// The capture the image belongs to; 0 for the editor's images, which [`EditorRecord`]
    /// matches to their entry.
    pub capture_id: Option<u64>,
    /// A name for an entry with no window title or app (output leaves it out).
    pub name: Option<String>,
    /// The file the user saved, when the capture was saved.
    pub saved_path: Option<PathBuf>,
    /// The image of a capture that only went to the clipboard, in output's inbox. The history
    /// moves it into its own folder; with a saved file it is left alone.
    pub image_path: Option<PathBuf>,
    /// A still to make a recording's thumbnail from.
    pub poster_path: Option<PathBuf>,
    pub annotations: Option<PathBuf>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub scale: Option<f64>,
    pub duration_ms: Option<u64>,
    pub bytes: Option<u64>,
    pub app_name: Option<String>,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    pub display_id: Option<u32>,
    pub display_name: Option<String>,
    pub created_at_ms: Option<i64>,
    /// The copied text of a text copy.
    pub text: Option<String>,
    /// An image that exists only in memory (an upload straight from the overlay), written into
    /// the history's own folder. Callers in the core set it; it never crosses IPC.
    #[serde(skip)]
    pub png: Option<Vec<u8>>,
}

/// Records a finished capture, unless the history is off. The image the history keeps is
/// written before the row (the crash invariant of `glimpsfy_history`), the thumbnail and the
/// text follow in the background, and `history://changed` announces the entry.
pub fn record_capture(app: &AppHandle, handoff: CaptureHandoff) -> CommandResult<Option<Entry>> {
    if !retention(app).records() {
        return Ok(None);
    }
    let store = store(app)?;
    let kind = handoff.kind.unwrap_or(EntryKind::Screenshot);
    let name = entry_name(&handoff, kind);
    let new = NewEntry {
        kind: Some(kind),
        name,
        created_at_ms: handoff.created_at_ms.unwrap_or_else(now_ms),
        annotations: handoff.annotations.clone(),
        saved_path: handoff.saved_path.clone(),
        size: handoff.width.zip(handoff.height).map(|(width, height)| PixelSize { width, height }),
        scale: handoff.scale,
        duration_ms: handoff.duration_ms,
        file_bytes: handoff.bytes,
        source: EntrySource {
            app_name: handoff.app_name.clone(),
            bundle_id: handoff.bundle_id.clone(),
            window_title: handoff.window_title.clone(),
        },
        display: handoff.display_id.map(|id| EntryDisplay { id, name: handoff.display_name.clone() }),
        text: handoff.text.clone(),
        text_lines: None,
        recognize_text: kind == EntryKind::Screenshot,
        hosted: None,
    };
    // A saved file is the user's and stays where it is; otherwise the history keeps its own copy.
    // The inbox file is moved out at once (output deletes inbox files after a day), and moved back
    // if the row cannot be written.
    let own = match (&handoff.saved_path, &handoff.image_path, &handoff.png) {
        (None, Some(image), _) => OwnFile::Move { from: image },
        (None, None, Some(png)) => OwnFile::Bytes { bytes: png, extension: "png" },
        _ => OwnFile::None,
    };
    let entry = store.add(new, own)?;
    if let Some(capture) = handoff.capture_id.filter(|id| *id != 0) {
        remember_capture(capture, &entry.id);
    }
    changed(app, HistoryChanged::Added { entry: light(entry.clone()) });
    if kind != EntryKind::TextCopy {
        if let Some(poster) = handoff.poster_path.filter(|_| kind == EntryKind::Recording) {
            let (app, store, id) = (app.clone(), store.clone(), entry.id.clone());
            std::thread::spawn(move || {
                if let Ok(thumbnail) = glimpsfy_history::thumbnail::from_png(&poster) {
                    if let Ok(entry) = store.set_thumbnail(&id, &thumbnail) {
                        changed(&app, HistoryChanged::Updated { entry: light(entry) });
                    }
                }
            });
        } else {
            queue_thumbnail(app, entry.id.clone());
        }
    }
    if kind == EntryKind::Screenshot {
        wake_text(app);
    }
    Ok(Some(entry))
}

/// The entries of the last captures, by capture id, so a toast's Send finds the history's copy of
/// the capture it reports (`send::resolve_file`). The hand-off arrives a moment after the toast,
/// so [`entry_of_capture`] waits for it.
static CAPTURE_ENTRIES: Mutex<VecDeque<(u64, EntryId)>> = Mutex::new(VecDeque::new());
static CAPTURE_RECORDED: Condvar = Condvar::new();
const CAPTURE_ENTRIES_KEPT: usize = 32;

fn remember_capture(capture: u64, entry: &EntryId) {
    let mut list = CAPTURE_ENTRIES.lock().unwrap_or_else(|p| p.into_inner());
    list.retain(|(id, _)| *id != capture);
    list.push_front((capture, entry.clone()));
    list.truncate(CAPTURE_ENTRIES_KEPT);
    CAPTURE_RECORDED.notify_all();
}

/// The entry the history recorded for `capture`, waiting up to `wait` for a hand-off on its way.
/// Call it from a blocking thread only.
pub fn entry_of_capture(capture: u64, wait: Duration) -> Option<EntryId> {
    let find = |list: &VecDeque<(u64, EntryId)>| list.iter().find(|(id, _)| *id == capture).map(|(_, e)| e.clone());
    let list = CAPTURE_ENTRIES.lock().unwrap_or_else(|p| p.into_inner());
    let (list, _) =
        CAPTURE_RECORDED.wait_timeout_while(list, wait, |list| find(list).is_none()).unwrap_or_else(|p| p.into_inner());
    find(&list)
}

/// Whether the history records new captures now (it is not off).
pub fn records(app: &AppHandle) -> bool {
    retention(app).records()
}

/// Cards are titled after the window that was captured, because people remember what they
/// looked at: "Title — App", the app alone, or the file's name. A name the app would have to make
/// up from words ("Text from Safari", "Capture") is not stored: the name stays empty and each
/// window says it in the language it shows (`entryName` in `src/history/model.ts`,
/// [`display_name`] here). Stored words would stay in the language of the day they were made, and
/// search finds such an entry by its app and its text in either language anyway.
fn entry_name(handoff: &CaptureHandoff, kind: EntryKind) -> String {
    let title = handoff.window_title.as_deref().map(str::trim).filter(|t| !t.is_empty());
    let app = handoff.app_name.as_deref().map(str::trim).filter(|a| !a.is_empty());
    match (title, app) {
        (Some(title), Some(app)) if !title.contains(app) => format!("{title} — {app}"),
        (Some(title), _) => title.to_string(),
        (None, Some(_)) if kind == EntryKind::TextCopy => String::new(),
        (None, Some(app)) => app.to_string(),
        (None, None) => handoff
            .name
            .clone()
            .or_else(|| {
                handoff.saved_path.as_deref().and_then(Path::file_stem).map(|s| s.to_string_lossy().into_owned())
            })
            .unwrap_or_default(),
    }
}

/// The name an entry goes by in the settings' language: its own, or the words for one that has
/// none (see [`entry_name`]). The core uses it for the editor's title and for file names.
pub fn display_name(app: &AppHandle, entry: &Entry) -> String {
    let own = entry.name.trim();
    if !own.is_empty() {
        return own.to_string();
    }
    unnamed(app, entry.kind, entry.source.app_name.as_deref())
}

/// The words for something without a name of its own, in the settings' language. The pages say
/// the same through `history.name.*` (`entryName` in `src/lib/api/history.ts`).
pub fn unnamed(app: &AppHandle, kind: EntryKind, source: Option<&str>) -> String {
    let language = app.state::<AppState>().settings.get().general.language;
    let source = source.map(str::trim).filter(|a| !a.is_empty());
    match (kind, source) {
        (EntryKind::TextCopy, Some(source)) => crate::i18n::text(language, "history.name.textFrom", &[("app", source)]),
        (EntryKind::TextCopy, None) => crate::i18n::text(language, "history.name.text", &[]),
        (EntryKind::Recording, _) => crate::i18n::text(language, "history.name.recording", &[]),
        (EntryKind::Screenshot, _) => crate::i18n::text(language, "history.name.capture", &[]),
    }
}

/// A file name stem from an entry's name, with the characters a file name cannot hold replaced.
pub fn file_stem_of(app: &AppHandle, entry: &Entry) -> String {
    let stem = display_name(app, entry)
        .chars()
        .map(|c| if matches!(c, '/' | ':' | '\\') || c.is_control() { '-' } else { c })
        .collect::<String>();
    // A name of dots or spaces alone would leave a hidden file or no name at all.
    let stem = stem.trim().trim_start_matches('.').trim();
    if stem.is_empty() {
        "Glimpsfy".into()
    } else {
        stem.to_string()
    }
}

// ------------------------------------------------------------------------ the editor's outputs

/// One image open in the editor, as the history sees it, so that its outputs make one entry.
/// `commands/render.rs` keeps one per editor image and names it with every output
/// ([`expect_editor_output`]).
pub struct EditorRecord {
    /// The entry the outputs revise: the one the first output added, or the reopened edit.
    entry: Mutex<Option<EntryId>>,
    /// The history entry the image was reopened from, which an output without drawings revises
    /// instead of adding a copy of the same picture.
    source: Option<EntryId>,
    /// The image the drawings sit on, kept beside the entry's document.
    base: EditorBase,
    origin: EditorOrigin,
}

/// The image under the editor's drawings.
pub enum EditorBase {
    /// A capture handed over by the overlay: the whole frame of its display.
    Frame(Arc<Frame>),
    /// A file's own bytes (a history entry's image, the image of a reopened edit, a dropped file).
    Bytes(Arc<Vec<u8>>),
}

/// What a new entry says about where an editor image came from.
#[derive(Clone, Debug, Default)]
pub struct EditorOrigin {
    /// The name when no window title or app names it (a file's stem, a history entry's name).
    pub name: Option<String>,
    pub app_name: Option<String>,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
    pub display_id: Option<u32>,
    pub display_name: Option<String>,
    /// When the pixels were captured; `None` makes the entry as new as its first output.
    pub created_at_ms: Option<i64>,
}

impl EditorRecord {
    /// A record for an image the history does not know yet (a hand-off or a dropped file), one
    /// reopened from a capture entry (`source`), or a reopened edit, whose outputs revise `entry`.
    pub fn new(base: EditorBase, origin: EditorOrigin, source: Option<EntryId>, entry: Option<EntryId>) -> Arc<Self> {
        Arc::new(EditorRecord { entry: Mutex::new(entry), source, base, origin })
    }

    /// A record for a new image made from this one (the image with its background removed): it
    /// says the same about where the pixels came from, but it is no entry's image yet. It
    /// neither revises the entry this record's outputs made or reopened, nor updates the capture
    /// it was reopened from, so the original pixels stay in the history as they were; its first
    /// output adds an entry of its own.
    pub fn fork(&self, base: EditorBase) -> Arc<Self> {
        EditorRecord::new(base, self.origin.clone(), None, None)
    }

    fn base_bytes(&self) -> CommandResult<Vec<u8>> {
        match &self.base {
            EditorBase::Frame(frame) => Ok(frame.encode_png(PngEffort::Fast)?),
            EditorBase::Bytes(bytes) => Ok(bytes.as_ref().clone()),
        }
    }
}

/// An editor output on its way: the record it belongs to and the document the page sent.
struct EditorTicket {
    seq: u64,
    record: Arc<EditorRecord>,
    document: Option<String>,
}

/// Tickets in the order of their outputs. Outputs of the editor are made one at a time, each
/// sends exactly one hand-off when it succeeds and none when it fails, and nothing else sends a
/// hand-off with capture id 0, so the oldest ticket is always the one a hand-off answers.
static EDITOR_TICKETS: Mutex<VecDeque<EditorTicket>> = Mutex::new(VecDeque::new());

/// Called by the editor's outputs right before they deliver: the hand-off that follows belongs to
/// `record`, with `document` (the page's annotation JSON) as it was at that output. Returns the
/// ticket to give back with [`forget_editor_output`] when the delivery fails.
pub fn expect_editor_output(record: Arc<EditorRecord>, document: Option<String>) -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut tickets = EDITOR_TICKETS.lock().unwrap_or_else(|p| p.into_inner());
    // A hand-off that never came (an event that could not be sent) must not keep a 5K frame
    // alive for good; outputs are one at a time, so a handful is never reached otherwise.
    while tickets.len() >= 8 {
        tickets.pop_front();
    }
    tickets.push_back(EditorTicket { seq, record, document });
    seq
}

/// The delivery behind `seq` failed, so no hand-off answers it.
pub fn forget_editor_output(seq: u64) {
    EDITOR_TICKETS.lock().unwrap_or_else(|p| p.into_inner()).retain(|t| t.seq != seq);
}

fn take_editor_ticket() -> Option<EditorTicket> {
    EDITOR_TICKETS.lock().unwrap_or_else(|p| p.into_inner()).pop_front()
}

/// The editor's outputs are recorded one after another on one thread, so a Save that follows a
/// Copy always finds the entry the Copy added.
fn queue_editor_output(app: &AppHandle, ticket: EditorTicket, handoff: CaptureHandoff) {
    type Job = (AppHandle, EditorTicket, CaptureHandoff);
    static QUEUE: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
    let queue = QUEUE.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Job>();
        let _ = std::thread::Builder::new().name("glimpsfy-history-editor".into()).spawn(move || {
            for (app, ticket, handoff) in receiver {
                if let Err(error) = record_editor_output(&app, ticket, handoff) {
                    trace::log(format!("[glimpsfy] history: an output of the editor was not recorded: {error}"));
                }
            }
        });
        Mutex::new(sender)
    });
    let _ = queue.lock().unwrap_or_else(|p| p.into_inner()).send((app.clone(), ticket, handoff));
}

/// Whether an annotation document changes the image: a drawing (a crop counts) or a beautify
/// style, either of which makes the output a new picture.
fn has_drawings(document: Option<&str>) -> bool {
    document.and_then(|d| glimpsfy_annotate::Document::from_json(d).ok()).is_some_and(|d| d.changes_image())
}

/// Records one output of an editor image: adds its entry the first time and revises it after
/// that, with the editor's document kept beside the image the drawings sit on.
fn record_editor_output(app: &AppHandle, ticket: EditorTicket, handoff: CaptureHandoff) -> CommandResult<()> {
    if !retention(app).records() {
        return Ok(());
    }
    let store = store(app)?;
    let record = &ticket.record;
    let drawn = has_drawings(ticket.document.as_deref());
    let mut slot = record.entry.lock().unwrap_or_else(|p| p.into_inner());
    // An entry deleted meanwhile is not revised; the output then makes a new one.
    let mut target = slot.as_ref().and_then(|id| store.get(id).ok());
    if target.is_none() && !drawn {
        // The same picture as the entry it was reopened from: that entry is the one to update.
        target = record.source.as_ref().and_then(|id| store.get(id).ok());
        if target.is_some() && handoff.saved_path.is_none() {
            // A copy of an unchanged history image changes nothing the history keeps.
            if let Some(inbox) = &handoff.image_path {
                let _ = std::fs::remove_file(inbox);
            }
            return Ok(());
        }
    }

    // The document goes into the edit the entry already has, or into a new one with its image
    // (also when the old edit lost its image, which the revision then removes).
    let into = target
        .as_ref()
        .and_then(|e| e.annotations.clone())
        .filter(|document| HistoryPaths::edit_base(document).is_file());
    let annotations = match &ticket.document {
        Some(document) if into.is_some() || drawn => {
            let base = if into.is_some() { None } else { Some(record.base_bytes()?) };
            Some(store.write_edit(document.as_bytes(), base.as_deref(), into.as_deref())?)
        }
        _ => None,
    };
    let own = match (&handoff.saved_path, &handoff.image_path) {
        (None, Some(image)) => OwnFile::Move { from: image },
        _ => OwnFile::None,
    };
    let size = handoff.width.zip(handoff.height).map(|(width, height)| PixelSize { width, height });
    let entry = match target {
        Some(entry) => {
            let revision = Revision {
                own,
                saved_path: handoff.saved_path.clone(),
                size,
                scale: handoff.scale,
                file_bytes: handoff.bytes,
                annotations,
            };
            let entry = store.revise(&entry.id, revision)?;
            changed(app, HistoryChanged::Updated { entry: light(entry.clone()) });
            entry
        }
        None => {
            let origin = &record.origin;
            let named = CaptureHandoff {
                name: origin.name.clone(),
                saved_path: handoff.saved_path.clone(),
                app_name: origin.app_name.clone(),
                window_title: origin.window_title.clone(),
                ..CaptureHandoff::default()
            };
            let new = NewEntry {
                kind: Some(EntryKind::Screenshot),
                name: entry_name(&named, EntryKind::Screenshot),
                created_at_ms: origin.created_at_ms.or(handoff.created_at_ms).unwrap_or_else(now_ms),
                annotations,
                saved_path: handoff.saved_path.clone(),
                size,
                scale: handoff.scale,
                file_bytes: handoff.bytes,
                source: EntrySource {
                    app_name: origin.app_name.clone(),
                    bundle_id: origin.bundle_id.clone(),
                    window_title: origin.window_title.clone(),
                },
                display: origin.display_id.map(|id| EntryDisplay { id, name: origin.display_name.clone() }),
                recognize_text: true,
                ..NewEntry::default()
            };
            let entry = store.add(new, own)?;
            changed(app, HistoryChanged::Added { entry: light(entry.clone()) });
            entry
        }
    };
    *slot = Some(entry.id.clone());
    drop(slot);
    queue_thumbnail(app, entry.id);
    wake_text(app);
    Ok(())
}

/// What the editor needs to reopen an entry: the image to draw on and, for an edit, its document.
pub struct EditorSource {
    pub image: PathBuf,
    pub document: Option<String>,
    /// The entry the editor's outputs revise (a reopened edit), or `None` for a plain capture,
    /// whose edits become entries of their own.
    pub edit_of: Option<EntryId>,
}

/// Where an entry's picture is for the editor: an edit's own image and document when both are
/// still there, the entry's file otherwise.
pub fn editor_source(entry: &Entry) -> CommandResult<EditorSource> {
    if let Some(document_path) = &entry.annotations {
        let base = HistoryPaths::edit_base(document_path);
        if let (true, Ok(document)) = (base.is_file(), std::fs::read_to_string(document_path)) {
            if glimpsfy_annotate::Document::from_json(&document).is_ok() {
                return Ok(EditorSource { image: base, document: Some(document), edit_of: Some(entry.id.clone()) });
            }
        }
    }
    let image = entry
        .media_path()
        .filter(|p| p.is_file())
        .map(Path::to_path_buf)
        .ok_or_else(|| CommandError::not_found("the capture's file"))?;
    Ok(EditorSource { image, document: None, edit_of: None })
}

fn record_upload(app: &AppHandle, id: &EntryId, file_name: &str, result: &UploadResult) {
    let Ok(store) = store(app) else { return };
    match store.set_hosted(id, Some(hosted_copy(result))) {
        Ok(entry) => changed(app, HistoryChanged::Updated { entry: light(entry) }),
        // The entry was deleted while its upload ran. The copy is public all the same, so its link
        // and deletion handle go into a link-only entry, from which the user can still delete it.
        Err(HistoryError::NotFound(_)) => record_link_only(app, file_name, result),
        Err(error) => trace::log(format!("[glimpsfy] history: the link of {id} was not recorded: {error}")),
    }
}

fn hosted_copy(result: &UploadResult) -> HostedCopy {
    HostedCopy {
        provider: result.provider.clone(),
        url: result.url.clone(),
        deletion: result.deletion.clone(),
        uploaded_at_ms: result.uploaded_at_ms,
        expires_at_ms: result.expires_at_ms,
    }
}

/// Records an upload that has no entry, such as a capture uploaded straight from the overlay. The
/// history never saw its pixels, so the entry has the link and no image; it is written in one
/// row with its deletion handle, so the user can still delete the upload later.
fn record_link_only(app: &AppHandle, file_name: &str, result: &UploadResult) {
    if !retention(app).records() {
        return;
    }
    let Ok(store) = store(app) else { return };
    let path = Path::new(file_name);
    let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    let kind = if matches!(extension.as_str(), "mp4" | "mov" | "m4v" | "webm" | "gif") {
        EntryKind::Recording
    } else {
        EntryKind::Screenshot
    };
    // An entry's own copy is named after its id (`<id>.png`, `<id>-<suffix>.png`), which says
    // nothing to the user.
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().trim().to_string())
        .filter(|s| !s.is_empty() && !(s.len() >= 29 && s.as_bytes()[12] == b'-' && EntryId::is_well_formed(s)));
    let new = NewEntry {
        kind: Some(kind),
        name: name.unwrap_or_else(|| result.provider.0.clone()),
        created_at_ms: result.uploaded_at_ms,
        hosted: Some(hosted_copy(result)),
        ..NewEntry::default()
    };
    match store.add(new, OwnFile::None) {
        Ok(entry) => changed(app, HistoryChanged::Added { entry: light(entry) }),
        Err(error) => trace::log(format!("[glimpsfy] history: the upload {} was not recorded: {error}", result.url)),
    }
}

fn queue_thumbnail(app: &AppHandle, id: EntryId) {
    if let Some(state) = app.try_state::<HistoryState>() {
        if let Some(sender) = state.thumbnails.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
            let _ = sender.send(id);
        }
    }
}

fn make_thumbnail(app: &AppHandle, store: &HistoryStore, id: &EntryId) {
    let Ok(entry) = store.get(id) else { return };
    let Some(path) = entry.media_path().filter(|p| p.is_file()).map(Path::to_path_buf) else { return };
    let is_png = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
    let made = if is_png {
        glimpsfy_history::thumbnail::from_png(&path).map_err(|e| e.to_string())
    } else if entry.kind == EntryKind::Screenshot {
        // JPEG, HEIC, WebP and the rest decode through the system, converted to sRGB.
        glimpsfy_ocr::load_image(&path).map_err(|e| e.to_string()).and_then(|frame| {
            let rgba = frame.to_rgba();
            glimpsfy_history::thumbnail::from_srgb_rgba(frame.size.width, frame.size.height, &rgba)
                .map_err(|e| e.to_string())
        })
    } else {
        return;
    };
    match made.and_then(|thumbnail| store.set_thumbnail(id, &thumbnail).map_err(|e| e.to_string())) {
        Ok(entry) => changed(app, HistoryChanged::Updated { entry: light(entry) }),
        Err(error) => trace::log(format!("[glimpsfy] history: no thumbnail for {id}: {error}")),
    }
}

// ------------------------------------------------------------------------- text recognition

fn wake_text(app: &AppHandle) {
    if let Some(state) = app.try_state::<HistoryState>() {
        let (flag, condvar) = &*state.text_wake;
        *flag.lock().unwrap_or_else(|p| p.into_inner()) = true;
        condvar.notify_one();
    }
}

/// How long an engine that is not available (not implemented yet, a model not downloaded, a
/// language pack missing) is left alone before the queue tries again. Every start retries too.
const ENGINE_MISSING_RETRY: Duration = Duration::from_secs(6 * 60 * 60);
/// A failing engine is tried this many times, with a growing pause, before the entry is given up.
const MAX_TEXT_ATTEMPTS: u32 = 3;

fn text_loop(app: &AppHandle, store: &HistoryStore, wake: &(Mutex<bool>, Condvar)) {
    loop {
        let wait = match store.next_text_job(now_ms()) {
            Ok(Ok(job)) => {
                read_text(app, store, &job);
                continue;
            }
            Ok(Err(Some(due))) => Duration::from_millis((due - now_ms()).clamp(1000, 24 * 60 * 60 * 1000) as u64),
            Ok(Err(None)) => Duration::from_secs(24 * 60 * 60),
            Err(error) => {
                trace::log(format!("[glimpsfy] history: the text queue failed: {error}"));
                Duration::from_secs(60)
            }
        };
        let (flag, condvar) = wake;
        let mut woken = flag.lock().unwrap_or_else(|p| p.into_inner());
        if !*woken {
            woken = condvar.wait_timeout(woken, wait).map(|(guard, _)| guard).unwrap_or_else(|p| p.into_inner().0);
        }
        *woken = false;
    }
}

enum TextFailure {
    /// The engine is not there yet: try again later without counting it.
    Unavailable(String),
    /// The image cannot be read: give up.
    Unreadable(String),
    /// The engine failed: count it and try again later.
    Failed(String),
}

/// Reads one job's image. The store writes the answer only while the entry still shows that
/// image, so an entry revised meanwhile stays waiting and the loop reads its new image next.
fn read_text(app: &AppHandle, store: &HistoryStore, job: &glimpsfy_history::TextJob) {
    match recognize(app, &job.image) {
        Ok((text, lines)) => match store.set_text(job, Some(text), Some(lines)) {
            Ok(Some(entry)) => changed(app, HistoryChanged::Updated { entry: light(entry) }),
            Ok(None) => {}
            Err(error) => trace::log(format!("[glimpsfy] history: the text of {} was not stored: {error}", job.id)),
        },
        Err(TextFailure::Unavailable(reason)) => {
            let _ = store.postpone_text(job, now_ms() + ENGINE_MISSING_RETRY.as_millis() as i64, false);
            trace::log(format!("[glimpsfy] history: text recognition waits: {reason}"));
        }
        Err(TextFailure::Unreadable(reason)) => {
            let _ = store.text_failed(job);
            trace::log(format!("[glimpsfy] history: the image of {} cannot be read: {reason}", job.id));
        }
        Err(TextFailure::Failed(reason)) => {
            if job.attempts + 1 >= MAX_TEXT_ATTEMPTS {
                let _ = store.text_failed(job);
            } else {
                let pause = 10 * 60 * 1000 * (1i64 << job.attempts);
                let _ = store.postpone_text(job, now_ms() + pause, true);
            }
            trace::log(format!("[glimpsfy] history: text recognition failed on {}: {reason}", job.id));
        }
    }
}

/// Reads an image file with the engine and languages of Settings › Text recognition.
fn recognize(app: &AppHandle, path: &Path) -> Result<(String, RecognizedText), TextFailure> {
    use glimpsfy_ocr::{EngineId, LanguageTag, OcrError, OcrRequest};

    let classify = |error: OcrError| match error {
        OcrError::NotImplemented(_)
        | OcrError::EngineUnavailable(_)
        | OcrError::LanguageUnavailable(_)
        | OcrError::ModelMissing(_)
        | OcrError::Download(_)
        | OcrError::Cancelled => TextFailure::Unavailable(error.to_string()),
        OcrError::InvalidImage(_) => TextFailure::Unreadable(error.to_string()),
        OcrError::Io(ref io) if io.kind() == std::io::ErrorKind::NotFound => TextFailure::Unreadable(error.to_string()),
        other => TextFailure::Failed(other.to_string()),
    };
    let settings = app.state::<AppState>().settings.get().ocr;
    let engine = match settings.engine {
        crate::settings::OcrEngineChoice::PpOcr => glimpsfy_ocr::engine(EngineId::PpOcr).map_err(classify)?,
        crate::settings::OcrEngineChoice::System => glimpsfy_ocr::default_engine(),
    };
    let frame = glimpsfy_ocr::load_image(path).map_err(classify)?;
    let mut request = OcrRequest::new(Arc::new(frame));
    request.languages = settings.languages.iter().map(|l| LanguageTag(l.clone())).collect();
    request.detect_language = settings.detect_language;
    request.keep_line_breaks = true;
    let result = glimpsfy_ocr::recognize(engine.as_ref(), &request).map_err(classify)?;
    let bounds = |b: &glimpsfy_ocr::Bounds| TextBox { x: b.x, y: b.y, width: b.width, height: b.height };
    let lines = result
        .lines
        .iter()
        .map(|line| TextLine {
            text: line.text.clone(),
            bounds: bounds(&line.bounds),
            words: line.words.iter().map(|w| TextWord { text: w.text.clone(), bounds: bounds(&w.bounds) }).collect(),
        })
        .collect();
    let size = PixelSize { width: result.image_size.width, height: result.image_size.height };
    Ok((result.text, RecognizedText { image_size: size, lines }))
}

// ---------------------------------------------------------------------------------- commands

/// Runs a store call on a blocking worker thread.
async fn with_store<T: Send + 'static>(
    app: &AppHandle,
    call: impl FnOnce(&HistoryStore) -> glimpsfy_history::Result<T> + Send + 'static,
) -> CommandResult<T> {
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || Ok(call(&*store(&app)?)?))
        .await
        .map_err(|error| CommandError::internal(error.to_string()))?
}

/// One page of the history, filtered and sorted as the query says. Items leave out the text;
/// the first page carries the sidebar's counts.
#[tauri::command]
pub async fn history_list(app: AppHandle, query: HistoryQuery) -> CommandResult<HistoryPage> {
    with_store(&app, move |store| store.list(&query)).await
}

/// One entry with its text and its recognised lines.
#[tauri::command]
pub async fn history_get(app: AppHandle, entry_id: EntryId) -> CommandResult<Entry> {
    with_store(&app, move |store| store.get(&entry_id)).await
}

/// Searches names, window titles, apps and recognised text, folded for case, accents and the
/// Turkish i's; each item carries its snippet and the boxes of its matches.
#[tauri::command]
pub async fn history_search(app: AppHandle, text: String, query: HistoryQuery) -> CommandResult<HistoryPage> {
    with_store(&app, move |store| store.search(&text, &query)).await
}

/// Deletes the hosted copy of an entry through the upload area. A host that deletes on its own
/// page (ImgBB) gets that page opened in the browser, and the entry keeps its link until the user
/// confirms with `history_forget_link`. Call it from a blocking thread.
struct UploadArea(AppHandle);

impl HostedCopies for UploadArea {
    fn delete(&self, entry: &Entry) -> Result<HostDeletion, String> {
        let hosted = entry.hosted.as_ref().ok_or("the capture has no hosted copy")?;
        let deletion = hosted
            .deletion
            .as_ref()
            .ok_or_else(|| format!("{} gives no way to delete an upload", hosted.provider.0))?;
        match crate::commands::upload::delete_hosted(&self.0, &hosted.provider, deletion) {
            Ok(DeleteOutcome::Deleted) => Ok(HostDeletion::Deleted),
            Ok(DeleteOutcome::OpenPage { url }) => {
                crate::commands::windows::app_open_url(self.0.clone(), url.clone())
                    .map_err(|error| format!("the deletion page did not open: {}", error.message))?;
                Ok(HostDeletion::PageOpened { url })
            }
            Err(error) if error.code == ErrorCode::NotImplemented => {
                Err("deleting from the host is not available yet".to_string())
            }
            Err(error) => Err(error.message),
        }
    }
}

/// Deletes entries with the files they own and, as the options say, moves the saved file to
/// the Trash and deletes the hosted copy. The report says per entry what was removed; an entry
/// whose hosted copy could not be deleted stays with its link. Ids that no longer exist are
/// skipped.
#[tauri::command]
pub async fn history_delete(
    app: AppHandle,
    entry_ids: Vec<EntryId>,
    options: DeleteOptions,
) -> CommandResult<DeleteReport> {
    let hosts = UploadArea(app.clone());
    let report = with_store(&app, move |store| store.delete(&entry_ids, &options, &hosts)).await?;
    announce(&app, &report).await;
    Ok(report)
}

async fn announce(app: &AppHandle, report: &DeleteReport) {
    let removed = report.removed();
    if !removed.is_empty() {
        changed(app, HistoryChanged::Removed { ids: removed });
    }
    for id in report.kept() {
        if let Ok(entry) = with_store(app, move |store| store.get(&id)).await {
            changed(app, HistoryChanged::Updated { entry: light(entry) });
        }
    }
}

#[tauri::command]
pub async fn history_set_pinned(app: AppHandle, entry_id: EntryId, pinned: bool) -> CommandResult<Entry> {
    let entry = with_store(&app, move |store| store.set_pinned(&entry_id, pinned)).await?;
    changed(&app, HistoryChanged::Updated { entry: light(entry.clone()) });
    Ok(entry)
}

#[tauri::command]
pub async fn history_rename(app: AppHandle, entry_id: EntryId, name: String) -> CommandResult<Entry> {
    let entry = with_store(&app, move |store| store.rename(&entry_id, &name)).await?;
    changed(&app, HistoryChanged::Updated { entry: light(entry.clone()) });
    Ok(entry)
}

/// Removes every entry that is not pinned. Files the user saved and hosted copies stay.
#[tauri::command]
pub async fn history_clear(app: AppHandle) -> CommandResult<DeleteReport> {
    let report = with_store(&app, |store| store.clear()).await?;
    trace::log(format!("[glimpsfy] history: cleared {} entries", report.removed().len()));
    announce(&app, &report).await;
    Ok(report)
}

/// How many entries and bytes the history holds, for the sidebar, the settings and the clear
/// dialog.
#[tauri::command]
pub async fn history_usage(app: AppHandle) -> CommandResult<HistoryUsage> {
    with_store(&app, |store| store.usage()).await
}

/// Records the link of an upload the history window started with `upload_file`, which does
/// not know the entry.
#[tauri::command]
pub async fn history_record_upload(app: AppHandle, entry_id: EntryId, result: UploadResult) -> CommandResult<Entry> {
    let hosted = hosted_copy(&result);
    let entry = with_store(&app, move |store| store.set_hosted(&entry_id, Some(hosted))).await?;
    changed(&app, HistoryChanged::Updated { entry: light(entry.clone()) });
    Ok(entry)
}

/// Copy (⌘C in the history): a screenshot's image, a recording's file, or a text copy's text.
/// A PNG goes on the clipboard as image data and a JPEG under its own type, so any app pastes
/// the pixels; other formats (WebP, HEIC, GIF) and recordings go as the file itself, because
/// apps that take an image paste those as a file, and re-encoding them would change the pixels.
#[tauri::command]
pub async fn history_copy(app: AppHandle, entry_id: EntryId) -> CommandResult<()> {
    let entry = with_store(&app, move |store| store.get(&entry_id)).await?;
    tauri::async_runtime::spawn_blocking(move || -> CommandResult<()> {
        if entry.kind == EntryKind::TextCopy {
            let text = entry.text.as_deref().unwrap_or_default();
            if text.is_empty() {
                return Err(CommandError::not_found("the text of this text copy"));
            }
            return copy_to_clipboard(Clipboard::Text(text));
        }
        let path = entry
            .media_path()
            .filter(|p| p.is_file())
            .map(Path::to_path_buf)
            .ok_or_else(|| CommandError::not_found("the capture's file"))?;
        let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        match (entry.kind, extension.as_str()) {
            (EntryKind::Screenshot, "png") => copy_to_clipboard(Clipboard::Png(&std::fs::read(&path)?)),
            (EntryKind::Screenshot, "jpg" | "jpeg") => copy_to_clipboard(Clipboard::Jpeg(&std::fs::read(&path)?)),
            _ => copy_to_clipboard(Clipboard::File(&path)),
        }
    })
    .await
    .map_err(|error| CommandError::internal(error.to_string()))?
}

/// What [`history_copy`] puts on the clipboard; Send to app puts an entry's file there the same way.
pub(crate) enum Clipboard<'a> {
    Text(&'a str),
    Png(&'a [u8]),
    Jpeg(&'a [u8]),
    File(&'a Path),
}

#[cfg(target_os = "macos")]
pub(crate) fn copy_to_clipboard(what: Clipboard<'_>) -> CommandResult<()> {
    let done = match what {
        Clipboard::Text(text) => crate::macos::copy_text(text),
        Clipboard::Png(png) => crate::macos::copy_png(png),
        Clipboard::Jpeg(jpeg) => crate::macos::copy_image(jpeg, true),
        Clipboard::File(path) => crate::macos::recording::copy_file(path),
    };
    done.map_err(|reason| CommandError::new(ErrorCode::Io, reason))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn copy_to_clipboard(_what: Clipboard<'_>) -> CommandResult<()> {
    Err(CommandError::not_implemented("copying from the history on this platform"))
}

/// Saves a copy of an entry's capture where the user picks in the save panel (Save as… ⇧⌘S),
/// and answers the path it got. A taken name gets " (2)" instead of replacing the file, picking
/// the capture's own file is refused (`invalid_argument`), and a cancelled panel answers
/// `cancelled`. The entry is left as it was.
#[tauri::command]
pub async fn history_save_as(app: AppHandle, entry_id: EntryId) -> CommandResult<String> {
    let entry = with_store(&app, move |store| store.get(&entry_id)).await?;
    let source =
        entry.media_path().map(Path::to_path_buf).ok_or_else(|| CommandError::not_found("the capture's file"))?;
    tauri::async_runtime::spawn_blocking(move || -> CommandResult<String> {
        use tauri_plugin_dialog::DialogExt;
        if !source.is_file() {
            return Err(CommandError::not_found(source.to_string_lossy()));
        }
        // `dialog()` panics when the plugin is not registered.
        if app.try_state::<tauri_plugin_dialog::Dialog<tauri::Wry>>().is_none() {
            return Err(CommandError::not_implemented("the save panel"));
        }
        let extension = source.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "png".into());
        let stem = file_stem_of(&app, &entry);
        let mut panel = app
            .dialog()
            .file()
            .set_file_name(format!("{stem}.{extension}"))
            .add_filter(extension.to_ascii_uppercase(), &[extension.as_str()]);
        if let Some(folder) = entry.saved_path.as_deref().and_then(Path::parent).filter(|f| f.is_dir()) {
            panel = panel.set_directory(folder);
        }
        let picked = panel
            .blocking_save_file()
            .ok_or_else(|| CommandError::new(ErrorCode::Cancelled, "the save panel was cancelled"))?
            .into_path()
            .map_err(|e| CommandError::invalid(e.to_string()))?;
        let saved = crate::output::copy_picked(&picked, &source)?;
        Ok(saved.to_string_lossy().into_owned())
    })
    .await
    .map_err(|error| CommandError::internal(error.to_string()))?
}

/// What [`history_delete_hosted`] did.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(tag = "outcome", rename_all = "camelCase")]
pub enum HostedDeletion {
    /// The host deleted the copy, and the entry no longer has a link.
    Deleted { entry: Box<Entry> },
    /// The host deletes on its own page, which is open in the browser now; the entry keeps its
    /// link until the user confirms with `history_forget_link`.
    PageOpened { url: String },
}

/// Deletes the hosted copy of an entry through the upload area and forgets its link; the entry
/// and its files stay. When the host refuses, the link is kept and the error says why.
#[tauri::command]
pub async fn history_delete_hosted(app: AppHandle, entry_id: EntryId) -> CommandResult<HostedDeletion> {
    let entry = with_store(&app, move |store| store.get(&entry_id)).await?;
    if entry.hosted.is_none() {
        return Err(CommandError::invalid("this capture has no hosted copy"));
    }
    let id = entry.id.clone();
    let hosts = UploadArea(app.clone());
    let outcome = tauri::async_runtime::spawn_blocking(move || hosts.delete(&entry))
        .await
        .map_err(|error| CommandError::internal(error.to_string()))?;
    match outcome.map_err(|reason| CommandError::new(ErrorCode::Provider, reason))? {
        HostDeletion::Deleted => {
            let entry = forget_link(&app, id).await?;
            Ok(HostedDeletion::Deleted { entry: Box::new(entry) })
        }
        HostDeletion::PageOpened { url } => Ok(HostedDeletion::PageOpened { url }),
    }
}

/// Forgets an entry's link without asking the host, once the user has deleted the upload on the
/// host's own page (or no longer wants the link).
#[tauri::command]
pub async fn history_forget_link(app: AppHandle, entry_id: EntryId) -> CommandResult<Entry> {
    forget_link(&app, entry_id).await
}

async fn forget_link(app: &AppHandle, id: EntryId) -> CommandResult<Entry> {
    let entry = with_store(app, move |store| store.set_hosted(&id, None)).await?;
    changed(app, HistoryChanged::Updated { entry: light(entry.clone()) });
    Ok(entry)
}

/// What [`history_export`] did.
#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub folder: String,
    pub exported: usize,
    /// Entries whose file was missing or could not be copied.
    pub skipped: Vec<EntryId>,
}

/// Copies the captures of entries into a folder the user picks (Export… ⌘E). A file that
/// exists there is never replaced: the copy gets " (2)", " (3)"… A cancelled panel answers
/// `cancelled`.
#[tauri::command]
pub async fn history_export(app: AppHandle, entry_ids: Vec<EntryId>) -> CommandResult<ExportReport> {
    let entries =
        with_store(&app, move |store| Ok(entry_ids.iter().filter_map(|id| store.get(id).ok()).collect::<Vec<_>>()))
            .await?;
    tauri::async_runtime::spawn_blocking(move || -> CommandResult<ExportReport> {
        use tauri_plugin_dialog::DialogExt;
        if app.try_state::<tauri_plugin_dialog::Dialog<tauri::Wry>>().is_none() {
            return Err(CommandError::not_implemented("the folder panel"));
        }
        let folder = app
            .dialog()
            .file()
            .blocking_pick_folder()
            .ok_or_else(|| CommandError::new(ErrorCode::Cancelled, "the folder panel was cancelled"))?
            .into_path()
            .map_err(|e| CommandError::invalid(e.to_string()))?;
        let mut report =
            ExportReport { folder: folder.to_string_lossy().into_owned(), exported: 0, skipped: Vec::new() };
        for entry in entries {
            let Some(source) = entry.media_path().filter(|p| p.is_file()).map(Path::to_path_buf) else {
                report.skipped.push(entry.id);
                continue;
            };
            let extension =
                source.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "png".into());
            let stem = file_stem_of(&app, &entry);
            // The shared writer cuts a long name to what a file system takes and claims the name
            // in the same step as it creates the file, so a file made meanwhile is never replaced.
            match crate::output::copy_picked(&folder.join(format!("{stem}.{extension}")), &source) {
                Ok(_) => report.exported += 1,
                Err(error) => {
                    trace::log(format!("[glimpsfy] history: {} was not exported: {}", entry.id, error.message));
                    report.skipped.push(entry.id);
                }
            }
        }
        Ok(report)
    })
    .await
    .map_err(|error| CommandError::internal(error.to_string()))?
}

/// Opens an entry in the editor through the render area: an edit with its drawings still
/// editable over the image they sit on, any other capture as its image.
#[tauri::command]
pub async fn history_open_in_editor(app: AppHandle, entry_id: EntryId) -> CommandResult<()> {
    let entry = with_store(&app, move |store| store.get(&entry_id)).await?;
    crate::commands::render::editor_open_entry(app, entry).await.map(|_| ())
}

// ------------------------------------------------------------------------------ the scheme

/// Serves thumbnails and capture files to the pages.
pub const SCHEME: &str = "glimpsfy-history";

/// The largest part of a file one response carries; a video player asks for the rest.
const MAX_CHUNK: u64 = 8 * 1024 * 1024;

/// The scheme's handler (`register_asynchronous_uri_scheme_protocol`).
pub fn serve(context: UriSchemeContext<'_, tauri::Wry>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = context.app_handle().clone();
    std::thread::spawn(move || responder.respond(respond(&app, &request)));
}

/// What a URL asks for: `glimpsfy-history://thumb/<id>` or `…/file/<id>`, in either form of the
/// scheme (`http://glimpsfy-history.localhost/thumb/<id>` on Windows).
fn parse(uri: &tauri::http::Uri) -> Option<(Media, EntryId)> {
    let path = uri.path().trim_matches('/');
    let (kind, id) = match uri.host() {
        Some("thumb") | Some("file") => (uri.host()?, path),
        _ => path.split_once('/')?,
    };
    let media = match kind {
        "thumb" => Media::Thumbnail,
        "file" => Media::File,
        _ => return None,
    };
    EntryId::is_well_formed(id).then(|| (media, EntryId(id.to_string())))
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("gif") => "image/gif",
        Some("heic") => "image/heic",
        Some("mp4" | "m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// The byte range of a `Range: bytes=a-b` header, clamped to the file and to [`MAX_CHUNK`].
fn range(header: Option<&str>, length: u64) -> Option<(u64, u64)> {
    let spec = header?.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (start, end) = spec.split_once('-')?;
    let (start, end) = match (start.trim(), end.trim()) {
        ("", suffix) => {
            let n: u64 = suffix.parse().ok()?;
            (length.saturating_sub(n), length.checked_sub(1)?)
        }
        (start, "") => (start.parse().ok()?, length.checked_sub(1)?),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(length.checked_sub(1)?)),
    };
    (start <= end && start < length).then(|| (start, end.min(start + MAX_CHUNK - 1)))
}

fn respond(app: &AppHandle, request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let reply =
        |status: StatusCode| Response::builder().status(status).body(Cow::Borrowed(&[][..])).unwrap_or_default();
    let Some((media, id)) = parse(request.uri()) else {
        return reply(StatusCode::BAD_REQUEST);
    };
    let Ok(store) = store(app) else {
        return reply(StatusCode::SERVICE_UNAVAILABLE);
    };
    let path = match store.media(&id, media) {
        Ok(Some(path)) => path,
        Ok(None) => return reply(StatusCode::NOT_FOUND),
        Err(_) => return reply(StatusCode::INTERNAL_SERVER_ERROR),
    };
    let Ok(mut file) = File::open(&path) else {
        return reply(StatusCode::NOT_FOUND);
    };
    let length = file.metadata().map(|m| m.len()).unwrap_or(0);
    let cache = if media == Media::Thumbnail { "max-age=31536000, immutable" } else { "no-cache" };
    let kind = content_type(&path);
    // No CORS header: an <img> or a <video> needs none, and pages have no reason to read the
    // bytes with fetch().
    let builder = Response::builder()
        .header(header::CONTENT_TYPE, kind)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, cache);
    // A recording asked for whole is answered with its first part, so a long one is never read
    // into memory at once; the player asks for the rest by range.
    let whole_video = kind.starts_with("video/") && length > MAX_CHUNK;
    let ranged =
        request.headers().get(header::RANGE).and_then(|v| v.to_str().ok()).or(whole_video.then_some("bytes=0-"));
    let (status, start, end) = match ranged {
        Some(value) => match range(Some(value), length) {
            Some((start, end)) => (StatusCode::PARTIAL_CONTENT, start, end),
            None => {
                return Response::builder()
                    .status(StatusCode::RANGE_NOT_SATISFIABLE)
                    .header(header::CONTENT_RANGE, format!("bytes */{length}"))
                    .body(Cow::Borrowed(&[][..]))
                    .unwrap_or_default()
            }
        },
        None => (StatusCode::OK, 0, length.saturating_sub(1)),
    };
    let size = if length == 0 { 0 } else { end - start + 1 };
    let mut body = vec![0u8; size as usize];
    if file.seek(SeekFrom::Start(start)).and_then(|_| file.read_exact(&mut body)).is_err() {
        return reply(StatusCode::INTERNAL_SERVER_ERROR);
    }
    let builder = if status == StatusCode::PARTIAL_CONTENT {
        builder.header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{length}"))
    } else {
        builder
    };
    builder.status(status).body(Cow::Owned(body)).unwrap_or_else(|_| reply(StatusCode::INTERNAL_SERVER_ERROR))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The image with its background removed is a new image. If its record carried the entry it
    /// was reopened from, its Save would replace that entry's file with the cut one, and the
    /// original pixels would be gone from the history for good.
    #[test]
    fn a_forked_record_leaves_the_entries_it_came_from_alone() {
        let source = EntryId("capture".into());
        let edit = EntryId("edit".into());
        let origin = EditorOrigin { name: Some("Product photo".into()), ..EditorOrigin::default() };
        let original =
            EditorRecord::new(EditorBase::Bytes(Arc::new(b"original".to_vec())), origin, Some(source), Some(edit));

        let fork = original.fork(EditorBase::Bytes(Arc::new(b"cut".to_vec())));

        assert_eq!(fork.source, None);
        assert_eq!(*fork.entry.lock().unwrap(), None);
        assert_eq!(fork.base_bytes().unwrap(), b"cut");
        assert_eq!(fork.origin.name.as_deref(), Some("Product photo"));
        // The record it came from still revises its own entry with its own pixels.
        assert_eq!(original.source, Some(EntryId("capture".into())));
        assert_eq!(*original.entry.lock().unwrap(), Some(EntryId("edit".into())));
        assert_eq!(original.base_bytes().unwrap(), b"original");
    }
}
