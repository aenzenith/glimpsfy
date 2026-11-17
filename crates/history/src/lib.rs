//! The capture history: every screenshot, recording and text copy, with search and retention.
//!
//! The history window, its search, pinned captures and retention read and write the history
//! through [`HistoryStore`], a SQLite database (bundled SQLite with FTS5) in WAL mode with one
//! writer, kept with the thumbnails and the history's own image files in one folder of the app's
//! data directory ([`HistoryPaths`]).
//!
//! # Files an entry owns, and files it does not
//!
//! An entry owns [`Entry::file`], [`Entry::thumbnail`] and, for an image the editor made,
//! [`Entry::annotations`] with the image its drawings sit on; they live inside the history's
//! folder, and the store deletes them together with the entry. [`Entry::saved_path`] is where the user
//! saved the capture; it belongs to the user, so neither retention nor clearing touches it unless
//! [`SweepOptions::trash_saved_files`] says so, and a delete moves it to the Trash only when
//! [`DeleteOptions::trash_saved_file`] asks for it. A user's file is never unlinked: it goes to
//! the Trash through [`Trash`]. The store also refuses to unlink an "own" file whose path leads
//! outside the history's folder, so a damaged row can never make it delete a user's file.
//!
//! # The crash invariant
//!
//! A row in `entries` is the only thing that makes an entry. Every file the history writes for a
//! new entry reaches its final name (through a temporary file and a rename) before the row that
//! names it is committed, and a deleted entry loses its row before its files are unlinked. A crash
//! at any point therefore leaves, at worst, files that no row names; the app never lists or
//! searches them, and [`HistoryStore::reconcile`] removes them at the next start.
//!
//! # Threads
//!
//! Every method blocks on the database and the disk. Call them on a worker thread. The store is
//! shared between threads (`Send + Sync`): writes go through one connection behind a mutex, and
//! reads use a small pool of their own connections, which WAL lets run beside the writer.

use std::path::{Path, PathBuf};

use glimpsfy_upload::{DeletionHandle, ProviderId};
use serde::{Deserialize, Serialize};

pub mod fold;
mod paths;
mod schema;
mod search;
mod store;
#[cfg(test)]
mod tests;
pub mod thumbnail;
mod trash;

pub use paths::HistoryPaths;
pub use schema::SCHEMA_VERSION;
pub use store::{HistoryStore, Media, OwnFile, ReconcileReport, Revision, TextJob};
pub use trash::{SystemTrash, Trash};

pub type Result<T, E = HistoryError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),
    #[error("the history has no entry {0:?}")]
    NotFound(EntryId),
    #[error("{0}")]
    InvalidArgument(String),
    /// The database failed, or a migration from an older version could not finish.
    #[error("the history database failed: {0}")]
    Storage(String),
    /// The database was written by a newer Glimpsfy, whose layout this build does not know. The
    /// file is left as it is.
    #[error("the history was written by a newer Glimpsfy (schema {found}, this build knows {known})")]
    NewerSchema { found: i64, known: i64 },
    /// An image could not be read or a thumbnail could not be made.
    #[error("the image cannot be read: {0}")]
    Image(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl From<rusqlite::Error> for HistoryError {
    fn from(error: rusqlite::Error) -> Self {
        HistoryError::Storage(error.to_string())
    }
}

/// Identifies an entry and stays the same for the entry's whole life. It is made of lower-case
/// hexadecimal digits and dashes only, so it is safe inside a file name and a URL.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EntryId(pub String);

impl EntryId {
    /// A new id that sorts by creation time.
    pub fn new(created_at_ms: i64) -> EntryId {
        use std::hash::{BuildHasher, Hasher};
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.write_u128(
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos(),
        );
        EntryId(format!("{:012x}-{:016x}", created_at_ms.max(0), hasher.finish()))
    }

    /// Whether `id` has the shape of an id this crate makes, which the URL scheme checks before
    /// it looks anything up.
    pub fn is_well_formed(id: &str) -> bool {
        !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-')
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for EntryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// What an entry holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum EntryKind {
    /// Holds an image of an area, a window or a display, with or without annotations.
    Screenshot,
    /// Holds a video, together with the GIFs exported from it.
    Recording,
    /// Text from "Copy text", which has no image of its own.
    TextCopy,
}

impl EntryKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            EntryKind::Screenshot => "screenshot",
            EntryKind::Recording => "recording",
            EntryKind::TextCopy => "textCopy",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<EntryKind> {
        match value {
            "screenshot" => Some(EntryKind::Screenshot),
            "recording" => Some(EntryKind::Recording),
            "textCopy" => Some(EntryKind::TextCopy),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PixelSize {
    pub width: u32,
    pub height: u32,
}

/// Where a capture came from, as far as the system told.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntrySource {
    pub app_name: Option<String>,
    pub bundle_id: Option<String>,
    pub window_title: Option<String>,
}

/// The display a capture was taken on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EntryDisplay {
    pub id: u32,
    pub name: Option<String>,
}

/// An axis-aligned box in pixels of the image the text was read from, origin at the top left.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextBox {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl TextBox {
    fn union(self, other: TextBox) -> TextBox {
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let right = (self.x + self.width).max(other.x + other.width);
        let bottom = (self.y + self.height).max(other.y + other.height);
        TextBox { x, y, width: right - x, height: bottom - y }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextWord {
    pub text: String,
    pub bounds: TextBox,
}

/// One recognised line with its box and, when the engine gives them, its words.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextLine {
    pub text: String,
    pub bounds: TextBox,
    #[serde(default)]
    pub words: Vec<TextWord>,
}

/// What text recognition read from an entry's image, with the boxes the search outlines.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecognizedText {
    /// The pixel size of the image the boxes refer to.
    pub image_size: PixelSize,
    pub lines: Vec<TextLine>,
}

/// Where the background text recognition stands for an entry.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TextState {
    /// Nothing is read for this entry (a recording, or a text copy whose text is its content).
    #[default]
    None,
    /// The entry waits for the engine, or for a retry after the engine was not available.
    Pending,
    Done,
    /// The engine could not read the image, and it is not tried again.
    Failed,
}

impl TextState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TextState::None => "none",
            TextState::Pending => "pending",
            TextState::Done => "done",
            TextState::Failed => "failed",
        }
    }

    pub(crate) fn parse(value: &str) -> TextState {
        match value {
            "pending" => TextState::Pending,
            "done" => TextState::Done,
            "failed" => TextState::Failed,
            _ => TextState::None,
        }
    }
}

/// Describes a copy of the entry on an upload host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostedCopy {
    pub provider: ProviderId,
    pub url: String,
    /// What deleting the hosted copy needs, if the host allows it.
    pub deletion: Option<DeletionHandle>,
    pub uploaded_at_ms: i64,
    /// When the host removes the copy on its own, if it does.
    pub expires_at_ms: Option<i64>,
}

/// Describes one capture in the history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub id: EntryId,
    pub kind: EntryKind,
    /// The name the history shows, made from the captured window's title and renamable.
    pub name: String,
    pub created_at_ms: i64,
    /// The history's own copy of the image or video, inside the history's folder. A capture that
    /// only went to the clipboard has one; a saved capture is read from [`Entry::saved_path`].
    pub file: Option<PathBuf>,
    /// The thumbnail the history grid shows, once it is made.
    pub thumbnail: Option<PathBuf>,
    /// The editor's annotation document (`glimpsfy-annotate` JSON) in the history's `edits`
    /// folder, with the image its drawings sit on beside it ([`HistoryPaths::edit_base`]), so the
    /// capture opens in the editor again with its drawings still editable. `file` or
    /// `saved_path` holds the flattened image everyone else sees.
    pub annotations: Option<PathBuf>,
    /// Where the user saved the capture, which the history never deletes on its own.
    pub saved_path: Option<PathBuf>,
    /// The pixel size of the image or video.
    pub size: Option<PixelSize>,
    /// Pixels per point of the display the capture came from.
    pub scale: Option<f64>,
    /// The length of a recording.
    pub duration_ms: Option<u64>,
    /// The size of the image or video file on disk.
    pub file_bytes: Option<u64>,
    pub source: EntrySource,
    pub display: Option<EntryDisplay>,
    /// The recognised text of an image, or the copied text of a text copy. Pages of a list or a
    /// search leave it out to stay small; [`HistoryStore::get`] fills it in.
    pub text: Option<String>,
    /// The recognised lines with their boxes, filled in by [`HistoryStore::get`] only.
    pub text_lines: Option<RecognizedText>,
    /// How many lines of text the entry holds, also in pages.
    pub text_line_count: u32,
    pub text_state: TextState,
    pub hosted: Option<HostedCopy>,
    /// A pinned entry is kept by retention and by clearing.
    pub pinned: bool,
}

impl Entry {
    /// The file that holds the capture: the history's own copy, or the user's saved file.
    pub fn media_path(&self) -> Option<&Path> {
        self.file.as_deref().or(self.saved_path.as_deref())
    }
}

/// A capture to add. The store gives it an id, and it starts unpinned and, unless `hosted` says
/// otherwise, not uploaded.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct NewEntry {
    pub kind: Option<EntryKind>,
    pub name: String,
    pub created_at_ms: i64,
    pub annotations: Option<PathBuf>,
    pub saved_path: Option<PathBuf>,
    pub size: Option<PixelSize>,
    pub scale: Option<f64>,
    pub duration_ms: Option<u64>,
    pub file_bytes: Option<u64>,
    pub source: EntrySource,
    pub display: Option<EntryDisplay>,
    /// The copied text of a text copy, or text that was already recognised in the overlay.
    pub text: Option<String>,
    pub text_lines: Option<RecognizedText>,
    /// Queues the image for the background text recognition.
    pub recognize_text: bool,
    /// The hosted copy of a capture that was uploaded before it was recorded, written with the
    /// row so its link and deletion handle cannot be lost between two writes.
    pub hosted: Option<HostedCopy>,
}

/// Sets the order of a page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortOrder {
    #[default]
    Newest,
    Oldest,
    Name,
    /// Puts the largest file first.
    Size,
    /// Puts the closest match to the search text first, then the newest. Outside a search it
    /// sorts as `Newest`.
    BestMatch,
}

/// Which entries a page holds and in what order. Every filter that is empty or `None` lets
/// every entry through.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct HistoryQuery {
    pub kinds: Vec<EntryKind>,
    /// Keeps only entries with (`true`) or without (`false`) a hosted copy.
    pub uploaded: Option<bool>,
    pub pinned: Option<bool>,
    /// Keeps only entries captured from this app, matched by its name.
    pub app: Option<String>,
    pub sort: SortOrder,
    /// Says how many entries of the full result to skip.
    pub offset: u32,
    /// The most entries a page holds; the store caps it at [`HistoryQuery::MAX_LIMIT`].
    pub limit: u32,
}

impl HistoryQuery {
    pub const MAX_LIMIT: u32 = 2000;
}

impl Default for HistoryQuery {
    fn default() -> Self {
        HistoryQuery {
            kinds: Vec::new(),
            uploaded: None,
            pinned: None,
            app: None,
            sort: SortOrder::Newest,
            offset: 0,
            limit: 100,
        }
    }
}

/// A span of a snippet, in UTF-16 code units so the UI can slice the string directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextRange {
    pub start: u32,
    pub end: u32,
}

/// Why an entry matched a search: a short excerpt with the matching words marked, and the boxes
/// of every match in the recognised text, so the UI can zoom onto one and outline them all.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextMatch {
    /// The longest line of the recognised text that matches, cut around the first match, or the
    /// name when only the name matches.
    pub snippet: String,
    pub highlights: Vec<TextRange>,
    /// Whether the name, the window title or the app matched.
    pub in_name: bool,
    /// How many matches the recognised text holds.
    pub count: u32,
    /// One box per match in the recognised text, in reading order, in pixels of `image_size`.
    pub boxes: Vec<TextBox>,
    /// The index in `boxes` of the first match in the quoted line, which a card zooms onto.
    pub best: Option<u32>,
    pub image_size: Option<PixelSize>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryItem {
    pub entry: Entry,
    /// A search fills it in, and a plain list leaves it `None`.
    pub text_match: Option<TextMatch>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppCount {
    pub name: String,
    pub count: u32,
}

/// The counts of the sidebar's filters, for the whole history or for the search text, with no
/// filter applied.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    pub all: u32,
    pub screenshots: u32,
    pub recordings: u32,
    pub text_copies: u32,
    pub uploaded: u32,
    pub pinned: u32,
    /// The apps with the most entries first.
    pub apps: Vec<AppCount>,
}

/// Holds one page of a list or a search.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub items: Vec<HistoryItem>,
    /// How many entries the whole result has, across every page.
    pub total: u32,
    pub offset: u32,
    /// The filter counts, on the first page (`offset` 0) only.
    pub facets: Option<Facets>,
}

/// What a delete removes besides the entry and the files it owns.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeleteOptions {
    /// Moves the file the user saved ([`Entry::saved_path`]) to the Trash as well.
    pub trash_saved_file: bool,
    /// Deletes the copy on the upload host as well, through the [`HostedCopies`] the caller
    /// passes, before anything else happens to the entry.
    pub delete_hosted_copy: bool,
}

/// What happened to one part of an entry during a delete, a clear or a sweep.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum StepOutcome {
    /// The part was not asked to go, or the entry has no such part.
    #[default]
    Untouched,
    Removed,
    /// The part was already gone.
    Missing,
    /// The step was asked for and failed; the part is still there.
    Failed {
        reason: String,
    },
    /// The step was not tried, because an earlier step of the same entry failed.
    Skipped,
    /// The host deletes uploads on its own page, which was opened in the browser. The part stays
    /// (the entry keeps its link) until the user confirms that it is gone there.
    PageOpened {
        url: String,
    },
}

/// What happened to one entry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcome {
    pub entry_id: EntryId,
    /// The entry left the history: its row is gone, so no list or search shows it again.
    pub removed: bool,
    /// The history's own files: the thumbnail and the history's copy of the capture.
    pub own_files: StepOutcome,
    /// The file the user saved, which only goes to the Trash.
    pub saved_file: StepOutcome,
    /// The copy on the upload host. When its deletion fails, the entry stays in the history with
    /// its link, so the user can try again.
    pub hosted_copy: StepOutcome,
    pub freed_bytes: u64,
}

impl DeleteOutcome {
    fn new(entry_id: EntryId) -> DeleteOutcome {
        DeleteOutcome {
            entry_id,
            removed: false,
            own_files: StepOutcome::Untouched,
            saved_file: StepOutcome::Untouched,
            hosted_copy: StepOutcome::Untouched,
            freed_bytes: 0,
        }
    }
}

/// What a delete, a clear or a sweep did, entry by entry.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteReport {
    pub outcomes: Vec<DeleteOutcome>,
    pub freed_bytes: u64,
}

impl DeleteReport {
    /// The entries that left the history.
    pub fn removed(&self) -> Vec<EntryId> {
        self.outcomes.iter().filter(|o| o.removed).map(|o| o.entry_id.clone()).collect()
    }

    /// The entries that stayed because a step failed.
    pub fn kept(&self) -> Vec<EntryId> {
        self.outcomes.iter().filter(|o| !o.removed).map(|o| o.entry_id.clone()).collect()
    }
}

/// How long the history keeps captures (Settings › Output › Keep captures).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Retention {
    /// The history records no new captures. The entries already there stay until the user
    /// deletes or clears them, and a sweep removes nothing, so turning the history off never
    /// deletes anything by itself.
    Off,
    Days7,
    #[default]
    Days30,
    Days90,
    /// The history keeps every capture, and a sweep removes nothing.
    Forever,
}

impl Retention {
    const DAY_MS: i64 = 24 * 60 * 60 * 1000;

    /// The age above which an entry is removed, or `None` when a sweep removes nothing.
    pub fn max_age_ms(self) -> Option<i64> {
        match self {
            Retention::Off | Retention::Forever => None,
            Retention::Days7 => Some(7 * Self::DAY_MS),
            Retention::Days30 => Some(30 * Self::DAY_MS),
            Retention::Days90 => Some(90 * Self::DAY_MS),
        }
    }

    /// The creation time before which unpinned entries are removed at `now_ms`, or `None` when
    /// nothing is removed. An entry created exactly at the cutoff is kept.
    pub fn cutoff_ms(self, now_ms: i64) -> Option<i64> {
        self.max_age_ms().map(|age| now_ms.saturating_sub(age))
    }

    /// Whether new captures are recorded.
    pub fn records(self) -> bool {
        self != Retention::Off
    }
}

/// What a retention sweep may touch besides the entries and their own files.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SweepOptions {
    /// Moves the files the user saved outside the history's folder to the Trash as well. Off
    /// unless the user turned it on.
    pub trash_saved_files: bool,
}

/// How much the history holds, for the sidebar, the settings section and the clear dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryUsage {
    pub entries: u32,
    pub pinned: u32,
    /// The bytes of the captures themselves, wherever they are stored.
    pub capture_bytes: u64,
    /// The bytes of the files the history owns (its copies and the thumbnails).
    pub own_bytes: u64,
    /// The part of `own_bytes` that clearing the history would free (the unpinned entries).
    pub clearable_bytes: u64,
    /// How many entries clearing would remove.
    pub clearable_entries: u32,
    pub oldest_at_ms: Option<i64>,
}

/// Deletes the hosted copy of an entry. The app passes the upload area's deletion; the store
/// never makes a network request itself.
pub trait HostedCopies: Send + Sync {
    /// Deletes `entry`'s hosted copy from its host. The error is the host's reason, in English
    /// for the log.
    fn delete(&self, entry: &Entry) -> std::result::Result<HostDeletion, String>;
}

/// What a host did when it was asked to delete a hosted copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostDeletion {
    /// The host confirmed the deletion.
    Deleted,
    /// The host deletes uploads only on its own page, which is open in the browser now. Nobody
    /// knows whether the user finishes there, so the entry keeps its link until they say so.
    PageOpened { url: String },
}

/// A [`HostedCopies`] for calls that never delete a hosted copy (clearing, sweeps).
pub struct NoHostedCopies;

impl HostedCopies for NoHostedCopies {
    fn delete(&self, _entry: &Entry) -> std::result::Result<HostDeletion, String> {
        Err("deleting hosted copies is not part of this operation".into())
    }
}
