//! The SQLite store: one writer connection behind a mutex, a pool of reader connections, the
//! files the entries own, and every operation that deletes something.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::SystemTime;

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row};

use crate::fold::Query;
use crate::paths::HistoryPaths;
use crate::thumbnail::{Thumbnail, ThumbnailFormat};
use crate::trash::Trash;
use crate::{
    schema, search, AppCount, DeleteOptions, DeleteOutcome, DeleteReport, Entry, EntryDisplay, EntryId, EntryKind,
    EntrySource, Facets, HistoryError, HistoryItem, HistoryPage, HistoryQuery, HistoryUsage, HostDeletion,
    HostedCopies, HostedCopy, NewEntry, PixelSize, RecognizedText, Result, Retention, SortOrder, StepOutcome,
    SweepOptions, TextState,
};

/// Milliseconds since the Unix epoch.
fn clock_ms() -> i64 {
    SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or_default()
}

/// A suffix no other file name of this process has, so a new file never takes an old one's name
/// (the old one is unlinked once the row stops naming it).
fn unique() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    format!("{nanos:x}{:x}", NEXT.fetch_add(1, Ordering::Relaxed))
}

/// The longest name a rename accepts, in characters.
const MAX_NAME: usize = 500;
/// How many idle reader connections the pool keeps.
const MAX_IDLE_READERS: usize = 4;

/// Which file of an entry the URL scheme serves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Media {
    Thumbnail,
    /// The capture itself: the history's own copy, or the user's saved file.
    File,
}

/// An entry that waits for text recognition.
#[derive(Clone, Debug, PartialEq)]
pub struct TextJob {
    pub id: EntryId,
    pub image: PathBuf,
    /// How many times the engine failed on it so far.
    pub attempts: u32,
}

/// What [`HistoryStore::reconcile`] found and did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    /// Files in the history's own folders that no entry names, now removed.
    pub removed_files: Vec<PathBuf>,
    /// Screenshots whose thumbnail is missing, to be made again.
    pub missing_thumbnails: Vec<EntryId>,
}

/// The paths an entry names, as `reconcile` reads them: id, own file, thumbnail, saved file, kind
/// and the edit's document.
type NamedFiles = (String, Option<String>, Option<String>, Option<String>, String, Option<String>);

/// The file an entry owns from the start.
pub enum OwnFile<'a> {
    None,
    /// Bytes the store writes into its captures folder under the entry's id.
    Bytes {
        bytes: &'a [u8],
        extension: &'a str,
    },
    /// A finished file the store moves into its captures folder under the entry's id, such as
    /// the PNG the output area leaves in its inbox for a capture that only went to the clipboard.
    /// If the row cannot be written, the file is moved back.
    Move {
        from: &'a Path,
    },
}

/// A later output of an entry's capture, which updates the entry instead of adding another: the
/// editor's Save after its Copy, or a Copy after more drawing.
pub struct Revision<'a> {
    /// A new copy of the image for the history to keep (an output that only went to the
    /// clipboard). It replaces the entry's old copy.
    pub own: OwnFile<'a>,
    /// The file the user saved this time. The entry names it from now on and drops its own copy,
    /// because the saved file holds the newer image; an earlier saved file stays the user's.
    pub saved_path: Option<PathBuf>,
    pub size: Option<PixelSize>,
    pub scale: Option<f64>,
    pub file_bytes: Option<u64>,
    /// The edit's document from [`HistoryStore::write_edit`], when the drawings changed.
    pub annotations: Option<PathBuf>,
}

pub struct HistoryStore {
    paths: HistoryPaths,
    writer: Mutex<Connection>,
    readers: Mutex<Vec<Connection>>,
    trash: Arc<dyn Trash>,
}

const COLUMNS: &str = "e.id, e.kind, e.name, e.created_at_ms, e.file, e.thumbnail, e.annotations, e.saved_path, \
    e.width, e.height, e.scale, e.duration_ms, e.file_bytes, e.app_name, e.bundle_id, e.window_title, e.display_id, \
    e.display_name, e.text_line_count, e.text_state, e.hosted_provider, e.hosted_url, e.hosted_deletion, \
    e.hosted_uploaded_at_ms, e.hosted_expires_at_ms, e.pinned";

impl HistoryStore {
    /// Opens (or makes) the history in `paths`, migrating an older database. A database from a
    /// newer Glimpsfy is refused and left as it is.
    pub fn open(paths: HistoryPaths, trash: Arc<dyn Trash>) -> Result<HistoryStore> {
        paths.create()?;
        let mut conn = Connection::open(paths.database())?;
        schema::prepare(&conn)?;
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        schema::migrate(&mut conn)?;
        Ok(HistoryStore { paths, writer: Mutex::new(conn), readers: Mutex::new(Vec::new()), trash })
    }

    pub fn paths(&self) -> &HistoryPaths {
        &self.paths
    }

    fn write(&self) -> MutexGuard<'_, Connection> {
        // A panic while holding the lock leaves the connection usable: SQLite rolls back the
        // transaction that was open, so the poison is cleared rather than spread.
        self.writer.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn read<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let pooled = self.readers.lock().unwrap_or_else(|p| p.into_inner()).pop();
        let conn = match pooled {
            Some(conn) => conn,
            None => {
                let conn = Connection::open(self.paths.database())?;
                schema::prepare(&conn)?;
                conn.pragma_update(None, "query_only", true)?;
                conn
            }
        };
        let result = work(&conn);
        let mut pool = self.readers.lock().unwrap_or_else(|p| p.into_inner());
        if pool.len() < MAX_IDLE_READERS {
            pool.push(conn);
        }
        result
    }

    // ------------------------------------------------------------------------------- adding

    /// Adds an entry. With [`OwnFile::Bytes`] the image is written into the captures folder first
    /// and the row second, so a crash in between leaves only a file no row names (see the crate's
    /// crash invariant).
    pub fn add(&self, new: NewEntry, own: OwnFile<'_>) -> Result<Entry> {
        let kind = new.kind.ok_or_else(|| HistoryError::InvalidArgument("a new entry needs a kind".into()))?;
        let id = EntryId::new(new.created_at_ms);
        let own_source = match own {
            OwnFile::Move { from } => Some(from),
            _ => None,
        };
        let file = match own {
            OwnFile::None => None,
            OwnFile::Bytes { bytes, extension } => {
                if !extension.chars().all(|c| c.is_ascii_alphanumeric()) || extension.is_empty() {
                    return Err(HistoryError::InvalidArgument(format!("{extension:?} is not a file extension")));
                }
                let path = self.paths.captures().join(format!("{id}.{extension}"));
                self.paths.write_atomically(&path, bytes)?;
                Some((path, bytes.len() as u64))
            }
            OwnFile::Move { from } => {
                let extension = from
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
                    .unwrap_or_else(|| "png".into());
                let path = self.paths.captures().join(format!("{id}.{extension}"));
                let bytes = fs::metadata(from)?.len();
                if fs::rename(from, &path).is_err() {
                    // Another volume: copy it whole; the original goes once the row is written.
                    self.paths.write_atomically(&path, &fs::read(from)?)?;
                }
                Some((path, bytes))
            }
        };
        match self.insert(&id, kind, &new, file.as_ref()) {
            Ok(()) => {
                if let Some(from) = own_source.filter(|from| from.exists()) {
                    let _ = fs::remove_file(from);
                }
                self.get(&id)
            }
            Err(error) => {
                if let Some((path, _)) = &file {
                    // A moved file goes back where it came from; a copy or written bytes go.
                    let restored = own_source.is_some_and(|from| !from.exists() && fs::rename(path, from).is_ok());
                    if !restored {
                        let _ = fs::remove_file(path);
                    }
                }
                Err(error)
            }
        }
    }

    fn insert(&self, id: &EntryId, kind: EntryKind, new: &NewEntry, file: Option<&(PathBuf, u64)>) -> Result<()> {
        let lines_json = new.text_lines.as_ref().map(serde_json::to_string).transpose().map_err(json_error)?;
        let line_count = match (&new.text_lines, &new.text) {
            (Some(lines), _) => lines.lines.len(),
            (None, Some(text)) if kind == EntryKind::TextCopy => text.lines().count(),
            _ => 0,
        };
        let state = if new.text_lines.is_some() || (kind == EntryKind::TextCopy && new.text.is_some()) {
            TextState::Done
        } else if new.recognize_text && kind == EntryKind::Screenshot {
            TextState::Pending
        } else {
            TextState::None
        };
        let file_bytes = new.file_bytes.or(file.map(|(_, bytes)| *bytes));
        let hosted = new.hosted.as_ref();
        let deletion =
            hosted.and_then(|h| h.deletion.as_ref()).map(serde_json::to_string).transpose().map_err(json_error)?;
        self.write().execute(
            "INSERT INTO entries (id, kind, name, created_at_ms, file, annotations, saved_path, width, height, scale,
                duration_ms, file_bytes, app_name, bundle_id, window_title, display_id, display_name, text, text_lines,
                text_line_count, text_state, hosted_provider, hosted_url, hosted_deletion, hosted_uploaded_at_ms,
                hosted_expires_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21,
                ?22, ?23, ?24, ?25, ?26)",
            params![
                id.0,
                kind.as_str(),
                new.name,
                new.created_at_ms,
                file.map(|(path, _)| self.paths.to_stored(path)),
                new.annotations.as_deref().map(|p| self.paths.to_stored(p)),
                new.saved_path.as_deref().map(|p| p.to_string_lossy().into_owned()),
                new.size.map(|s| s.width),
                new.size.map(|s| s.height),
                new.scale,
                new.duration_ms.map(|d| d as i64),
                file_bytes.map(|b| b as i64),
                new.source.app_name,
                new.source.bundle_id,
                new.source.window_title,
                new.display.as_ref().map(|d| d.id),
                new.display.as_ref().and_then(|d| d.name.clone()),
                new.text,
                lines_json,
                line_count as i64,
                state.as_str(),
                hosted.map(|h| h.provider.0.clone()),
                hosted.map(|h| h.url.clone()),
                deletion,
                hosted.map(|h| h.uploaded_at_ms),
                hosted.and_then(|h| h.expires_at_ms),
            ],
        )?;
        Ok(())
    }

    /// Writes an edit into the edits folder: the editor's annotation document and, for a new
    /// edit, the image its drawings sit on (`base`, kept beside the document so the drawings stay
    /// editable). Returns the document's path for [`NewEntry::annotations`] or
    /// [`Revision::annotations`]. Both files reach their names before a row names them (the crash
    /// invariant). With `into`, the document of an existing edit is replaced in place and its
    /// image kept.
    pub fn write_edit(&self, document: &[u8], base: Option<&[u8]>, into: Option<&Path>) -> Result<PathBuf> {
        if let Some(existing) = into.filter(|p| self.paths.owns(p) && HistoryPaths::edit_base(p).is_file()) {
            self.paths.write_atomically(existing, document)?;
            return Ok(existing.to_path_buf());
        }
        let base =
            base.ok_or_else(|| HistoryError::InvalidArgument("a new edit needs the image it draws on".into()))?;
        let stem = EntryId::new(clock_ms());
        let path = self.paths.edits().join(format!("{stem}.json"));
        self.paths.write_atomically(&HistoryPaths::edit_base(&path), base)?;
        if let Err(error) = self.paths.write_atomically(&path, document) {
            let _ = fs::remove_file(HistoryPaths::edit_base(&path));
            return Err(error.into());
        }
        Ok(path)
    }

    /// Updates an entry with a later output of its capture (see [`Revision`]). The new files are
    /// written before the row changes, and the files the row no longer names (the old copy, the
    /// old thumbnail, a replaced edit) are unlinked after it, so a crash in between leaves only
    /// unnamed files. The user's files are never unlinked. The thumbnail and the recognised text
    /// are made again from the new image.
    pub fn revise(&self, id: &EntryId, revision: Revision<'_>) -> Result<Entry> {
        let old = self.get(id)?;
        let moved_from = match revision.own {
            OwnFile::Move { from } => Some(from),
            _ => None,
        };
        // A new name, so a page never shows the old image from its cache.
        let fresh = |extension: &str| self.paths.captures().join(format!("{id}-{}.{extension}", unique()));
        let written: Option<(PathBuf, u64)> = match revision.own {
            OwnFile::None => None,
            OwnFile::Bytes { bytes, extension } => {
                if extension.is_empty() || !extension.chars().all(|c| c.is_ascii_alphanumeric()) {
                    return Err(HistoryError::InvalidArgument(format!("{extension:?} is not a file extension")));
                }
                let path = fresh(extension);
                self.paths.write_atomically(&path, bytes)?;
                Some((path, bytes.len() as u64))
            }
            OwnFile::Move { from } => {
                let extension = from
                    .extension()
                    .map(|e| e.to_string_lossy().to_ascii_lowercase())
                    .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
                    .unwrap_or_else(|| "png".into());
                let path = fresh(&extension);
                let bytes = fs::metadata(from)?.len();
                if fs::rename(from, &path).is_err() {
                    self.paths.write_atomically(&path, &fs::read(from)?)?;
                }
                Some((path, bytes))
            }
        };
        let file = match (&written, &revision.saved_path) {
            (Some((path, _)), _) => Some(path.clone()),
            (None, Some(_)) => None,
            (None, None) => old.file.clone(),
        };
        let saved_path = revision.saved_path.clone().or(old.saved_path.clone());
        let annotations = revision.annotations.clone().or(old.annotations.clone());
        let size = revision.size.or(old.size);
        let file_bytes = revision.file_bytes.or(written.as_ref().map(|(_, b)| *b)).or(old.file_bytes);
        let text_state = if old.kind == EntryKind::Screenshot { TextState::Pending } else { old.text_state };
        let updated = self.write().execute(
            "UPDATE entries SET file = ?2, saved_path = ?3, annotations = ?4, width = ?5, height = ?6, scale = ?7,
                file_bytes = ?8, thumbnail = NULL, thumbnail_bytes = NULL, text = NULL, text_lines = NULL,
                text_line_count = 0, text_state = ?9, text_attempts = 0, text_retry_at_ms = NULL
             WHERE id = ?1",
            params![
                id.0,
                file.as_deref().map(|p| self.paths.to_stored(p)),
                saved_path.as_deref().map(|p| p.to_string_lossy().into_owned()),
                annotations.as_deref().map(|p| self.paths.to_stored(p)),
                size.map(|s| s.width),
                size.map(|s| s.height),
                revision.scale.or(old.scale),
                file_bytes.map(|b| b as i64),
                text_state.as_str(),
            ],
        );
        let failed = match updated {
            Ok(0) => Some(HistoryError::NotFound(id.clone())),
            Ok(_) => None,
            Err(error) => Some(error.into()),
        };
        if let Some(error) = failed {
            if let Some((path, _)) = &written {
                let restored = moved_from.is_some_and(|from| !from.exists() && fs::rename(path, from).is_ok());
                if !restored {
                    let _ = fs::remove_file(path);
                }
            }
            return Err(error);
        }
        if let Some(from) = moved_from.filter(|from| from.exists()) {
            let _ = fs::remove_file(from);
        }
        // What the row no longer names, and only inside the history's own folders.
        let mut gone: Vec<PathBuf> = old.thumbnail.iter().cloned().collect();
        if old.file != file {
            gone.extend(old.file.iter().cloned());
        }
        if let Some(previous) = old.annotations.as_ref().filter(|p| Some(*p) != annotations.as_ref()) {
            gone.push(previous.clone());
            gone.push(HistoryPaths::edit_base(previous));
        }
        for path in gone {
            if self.paths.owns(&path) {
                let _ = fs::remove_file(&path);
            }
        }
        self.get(id)
    }

    // ------------------------------------------------------------------------------ reading

    /// One entry with its text and recognised lines.
    pub fn get(&self, id: &EntryId) -> Result<Entry> {
        self.read(|conn| {
            conn.query_row(
                &format!("SELECT {COLUMNS}, e.text, e.text_lines FROM entries e WHERE e.id = ?1"),
                [&id.0],
                |row| self.entry(row, true),
            )
            .optional()?
            .ok_or_else(|| HistoryError::NotFound(id.clone()))
        })
    }

    /// One page of the history, filtered and sorted as the query says.
    pub fn list(&self, query: &HistoryQuery) -> Result<HistoryPage> {
        let (filter, mut values) = filters(query);
        let order = order_by(query.sort, false);
        let limit = query.limit.min(HistoryQuery::MAX_LIMIT);
        self.read(|conn| {
            let total: u32 = conn.query_row(
                &format!("SELECT count(*) FROM entries e WHERE {filter}"),
                params_from_iter(&values),
                |r| r.get(0),
            )?;
            values.push(Value::Integer(limit as i64));
            values.push(Value::Integer(query.offset as i64));
            let mut statement = conn.prepare(&format!(
                "SELECT {COLUMNS} FROM entries e WHERE {filter} ORDER BY {order} LIMIT ? OFFSET ?"
            ))?;
            let items = statement
                .query_map(params_from_iter(&values), |row| self.entry(row, false))?
                .map(|entry| entry.map(|entry| HistoryItem { entry, text_match: None }))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let facets = if query.offset == 0 { Some(facets(conn, None)?) } else { None };
            Ok(HistoryPage { items, total, offset: query.offset, facets })
        })
    }

    /// Searches the names, window titles, apps and recognised text, folded for case, accents and
    /// the Turkish i's (`fold.rs`), with the last word of each group matching as a prefix. Each
    /// item carries its snippet and the boxes of its matches.
    pub fn search(&self, text: &str, query: &HistoryQuery) -> Result<HistoryPage> {
        let parsed = Query::parse(text);
        if parsed.is_empty() {
            let mut query = query.clone();
            if query.sort == SortOrder::BestMatch {
                query.sort = SortOrder::Newest;
            }
            return self.list(&query);
        }
        let expression = parsed.fts_expression();
        let (filter, filter_values) = filters(query);
        let order = order_by(query.sort, true);
        let limit = query.limit.min(HistoryQuery::MAX_LIMIT);
        let mut values = vec![Value::Text(expression.clone())];
        values.extend(filter_values);
        self.read(|conn| {
            let from = "entries_fts f JOIN entries e ON e.seq = f.rowid";
            let total: u32 = conn.query_row(
                &format!("SELECT count(*) FROM {from} WHERE entries_fts MATCH ? AND {filter}"),
                params_from_iter(&values),
                |r| r.get(0),
            )?;
            values.push(Value::Integer(limit as i64));
            values.push(Value::Integer(query.offset as i64));
            let mut statement = conn.prepare(&format!(
                "SELECT {COLUMNS}, e.text, e.text_lines, bm25(entries_fts, 10.0, 6.0, 3.0, 1.0) AS rank
                 FROM {from} WHERE entries_fts MATCH ? AND {filter} ORDER BY {order} LIMIT ? OFFSET ?"
            ))?;
            let items = statement
                .query_map(params_from_iter(&values), |row| {
                    let mut entry = self.entry(row, true)?;
                    let text = entry.text.take();
                    let lines = entry.text_lines.take();
                    let others = [
                        entry.source.window_title.as_deref().unwrap_or(""),
                        entry.source.app_name.as_deref().unwrap_or(""),
                    ];
                    let found = search::text_match(&parsed, &entry.name, &others, text.as_deref(), lines.as_ref());
                    Ok(HistoryItem { entry, text_match: Some(found) })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let facets = if query.offset == 0 { Some(facets(conn, Some(&expression))?) } else { None };
            Ok(HistoryPage { items, total, offset: query.offset, facets })
        })
    }

    /// How much the history holds.
    pub fn usage(&self) -> Result<HistoryUsage> {
        self.read(|conn| {
            Ok(conn.query_row(
                "SELECT count(*), coalesce(sum(pinned), 0), coalesce(sum(file_bytes), 0),
                    coalesce(sum(CASE WHEN file IS NOT NULL THEN coalesce(file_bytes, 0) ELSE 0 END + coalesce(thumbnail_bytes, 0)), 0),
                    coalesce(sum(CASE WHEN pinned = 0 THEN (CASE WHEN file IS NOT NULL THEN coalesce(file_bytes, 0) ELSE 0 END)
                        + coalesce(thumbnail_bytes, 0) ELSE 0 END), 0),
                    coalesce(sum(pinned = 0), 0), min(created_at_ms)
                 FROM entries",
                [],
                |r| {
                    Ok(HistoryUsage {
                        entries: r.get(0)?,
                        pinned: r.get(1)?,
                        capture_bytes: r.get::<_, i64>(2)?.max(0) as u64,
                        own_bytes: r.get::<_, i64>(3)?.max(0) as u64,
                        clearable_bytes: r.get::<_, i64>(4)?.max(0) as u64,
                        clearable_entries: r.get(5)?,
                        oldest_at_ms: r.get(6)?,
                    })
                },
            )?)
        })
    }

    /// The file the URL scheme serves for an entry, when it exists.
    pub fn media(&self, id: &EntryId, media: Media) -> Result<Option<PathBuf>> {
        let entry = self.read(|conn| {
            conn.query_row(&format!("SELECT {COLUMNS} FROM entries e WHERE e.id = ?1"), [&id.0], |row| {
                self.entry(row, false)
            })
            .optional()
            .map_err(HistoryError::from)
        })?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let path = match media {
            Media::Thumbnail => entry.thumbnail,
            Media::File => entry.media_path().map(Path::to_path_buf),
        };
        Ok(path.filter(|p| p.is_file()))
    }

    // ----------------------------------------------------------------------------- changing

    pub fn set_pinned(&self, id: &EntryId, pinned: bool) -> Result<Entry> {
        self.update(id, "UPDATE entries SET pinned = ?2 WHERE id = ?1", &[&pinned])
    }

    pub fn rename(&self, id: &EntryId, name: &str) -> Result<Entry> {
        let name = name.trim();
        if name.is_empty() {
            return Err(HistoryError::InvalidArgument("a name cannot be empty".into()));
        }
        if name.chars().count() > MAX_NAME {
            return Err(HistoryError::InvalidArgument(format!("a name is at most {MAX_NAME} characters long")));
        }
        self.update(id, "UPDATE entries SET name = ?2 WHERE id = ?1", &[&name])
    }

    /// Stores what text recognition read, which makes the entry searchable by it.
    pub fn set_text(&self, id: &EntryId, text: Option<String>, lines: Option<RecognizedText>) -> Result<Entry> {
        let count = lines.as_ref().map(|l| l.lines.len()).unwrap_or(0) as i64;
        let json = lines.as_ref().map(serde_json::to_string).transpose().map_err(json_error)?;
        self.update(
            id,
            "UPDATE entries SET text = ?2, text_lines = ?3, text_line_count = ?4, text_state = 'done', text_retry_at_ms = NULL
             WHERE id = ?1",
            &[&text, &json, &count],
        )
    }

    /// Puts off an entry's text recognition until `retry_at_ms`, counting a failed attempt when
    /// `failed` is set (an engine that is not there yet is not a failure).
    pub fn postpone_text(&self, id: &EntryId, retry_at_ms: i64, failed: bool) -> Result<()> {
        self.write().execute(
            "UPDATE entries SET text_retry_at_ms = ?2, text_attempts = text_attempts + ?3 WHERE id = ?1 AND text_state = 'pending'",
            params![id.0, retry_at_ms, failed as i64],
        )?;
        Ok(())
    }

    /// Gives up on an entry's text, which leaves it searchable by name only.
    pub fn text_failed(&self, id: &EntryId) -> Result<()> {
        self.write()
            .execute("UPDATE entries SET text_state = 'failed', text_retry_at_ms = NULL WHERE id = ?1", [&id.0])?;
        Ok(())
    }

    /// Makes every waiting text recognition due now, as each start of the app does.
    pub fn retry_text_now(&self) -> Result<()> {
        self.write().execute("UPDATE entries SET text_retry_at_ms = NULL WHERE text_state = 'pending'", [])?;
        Ok(())
    }

    /// The newest entry whose text recognition is due at `now_ms`, or the time the next one is
    /// due when none is.
    pub fn next_text_job(&self, now_ms: i64) -> Result<std::result::Result<TextJob, Option<i64>>> {
        let due = self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT id, file, saved_path, text_attempts FROM entries
                     WHERE text_state = 'pending' AND (text_retry_at_ms IS NULL OR text_retry_at_ms <= ?1)
                     ORDER BY created_at_ms DESC LIMIT 1",
                    [now_ms],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, Option<String>>(1)?,
                            r.get::<_, Option<String>>(2)?,
                            r.get::<_, u32>(3)?,
                        ))
                    },
                )
                .optional()?)
        })?;
        match due {
            Some((id, file, saved, attempts)) => {
                let image = file.map(|f| self.paths.resolve(&f)).or(saved.map(PathBuf::from));
                match image {
                    Some(image) => Ok(Ok(TextJob { id: EntryId(id), image, attempts })),
                    None => {
                        self.text_failed(&EntryId(id))?;
                        self.next_text_job(now_ms)
                    }
                }
            }
            None => {
                let next = self.read(|conn| {
                    Ok(conn.query_row(
                        "SELECT min(text_retry_at_ms) FROM entries WHERE text_state = 'pending'",
                        [],
                        |r| r.get(0),
                    )?)
                })?;
                Ok(Err(next))
            }
        }
    }

    /// Writes an entry's thumbnail and records it. When the entry was deleted meanwhile, the
    /// file is removed again and `NotFound` returned.
    pub fn set_thumbnail(&self, id: &EntryId, thumbnail: &Thumbnail) -> Result<Entry> {
        let extension = match thumbnail.format {
            ThumbnailFormat::Jpeg => "jpg",
            ThumbnailFormat::Png => "png",
        };
        // Each thumbnail gets a name of its own: pages cache a thumbnail's URL for good, and a
        // revised entry gets a new one.
        let previous = self.get(id).ok().and_then(|entry| entry.thumbnail);
        let path = self.paths.thumbnails().join(format!("{id}-{}.{extension}", unique()));
        self.paths.write_atomically(&path, &thumbnail.bytes)?;
        let stored = self.paths.to_stored(&path);
        let changed = self.write().execute(
            "UPDATE entries SET thumbnail = ?2, thumbnail_bytes = ?3 WHERE id = ?1",
            params![id.0, stored, thumbnail.bytes.len() as i64],
        )?;
        if changed == 0 {
            let _ = fs::remove_file(&path);
            return Err(HistoryError::NotFound(id.clone()));
        }
        if let Some(previous) = previous.filter(|p| *p != path && self.paths.owns(p)) {
            let _ = fs::remove_file(previous);
        }
        self.get(id)
    }

    /// Records an upload, or forgets it after the hosted copy was deleted.
    pub fn set_hosted(&self, id: &EntryId, hosted: Option<HostedCopy>) -> Result<Entry> {
        let deletion = hosted
            .as_ref()
            .and_then(|h| h.deletion.as_ref())
            .map(serde_json::to_string)
            .transpose()
            .map_err(json_error)?;
        self.update(
            id,
            "UPDATE entries SET hosted_provider = ?2, hosted_url = ?3, hosted_deletion = ?4, hosted_uploaded_at_ms = ?5,
                hosted_expires_at_ms = ?6 WHERE id = ?1",
            &[
                &hosted.as_ref().map(|h| h.provider.0.clone()),
                &hosted.as_ref().map(|h| h.url.clone()),
                &deletion,
                &hosted.as_ref().map(|h| h.uploaded_at_ms),
                &hosted.as_ref().and_then(|h| h.expires_at_ms),
            ],
        )
    }

    fn update(&self, id: &EntryId, sql: &str, values: &[&dyn rusqlite::ToSql]) -> Result<Entry> {
        let mut all: Vec<&dyn rusqlite::ToSql> = vec![&id.0];
        all.extend_from_slice(values);
        let changed = self.write().execute(sql, all.as_slice())?;
        if changed == 0 {
            return Err(HistoryError::NotFound(id.clone()));
        }
        self.get(id)
    }

    // ---------------------------------------------------------------------------- deleting

    /// Deletes entries, entry by entry in this order: the hosted copy (when asked), the saved
    /// file to the Trash (when asked), then the row and the history's own files. When a step that
    /// was asked for fails, the entry stays in the history with everything it still has (its link
    /// included), and the report says what was and was not removed. Ids that no longer exist are
    /// skipped, so a repeated delete is harmless.
    pub fn delete(&self, ids: &[EntryId], options: &DeleteOptions, hosted: &dyn HostedCopies) -> Result<DeleteReport> {
        let mut seen = HashSet::new();
        let mut doomed = Vec::new();
        let mut kept = Vec::new();
        for id in ids.iter().filter(|id| seen.insert((*id).clone())) {
            let entry = match self.get(id) {
                Ok(entry) => entry,
                Err(HistoryError::NotFound(_)) => continue,
                Err(error) => return Err(error),
            };
            let mut outcome = DeleteOutcome::new(id.clone());

            if options.delete_hosted_copy && entry.hosted.is_some() {
                // A deletion that has not happened yet (a refusal, or a page the user has still to
                // use) keeps the whole entry, so its link is there for the next try.
                let unfinished = match hosted.delete(&entry) {
                    Ok(HostDeletion::Deleted) => None,
                    Ok(HostDeletion::PageOpened { url }) => Some(StepOutcome::PageOpened { url }),
                    Err(reason) => Some(StepOutcome::Failed { reason }),
                };
                match unfinished {
                    None => outcome.hosted_copy = StepOutcome::Removed,
                    Some(step) => {
                        outcome.hosted_copy = step;
                        outcome.own_files = StepOutcome::Skipped;
                        if options.trash_saved_file && entry.saved_path.is_some() {
                            outcome.saved_file = StepOutcome::Skipped;
                        }
                        kept.push(outcome);
                        continue;
                    }
                }
            }

            if options.trash_saved_file {
                if let Some(saved) = &entry.saved_path {
                    outcome.saved_file = self.trash_file(saved);
                    if matches!(outcome.saved_file, StepOutcome::Failed { .. }) {
                        outcome.own_files = StepOutcome::Skipped;
                        if outcome.hosted_copy == StepOutcome::Removed {
                            // The hosted copy is gone, so the entry kept for a retry loses its link.
                            let _ = self.set_hosted(id, None);
                        }
                        kept.push(outcome);
                        continue;
                    }
                }
            }
            doomed.push((entry, outcome));
        }

        let mut report = self.remove(doomed)?;
        report.outcomes.extend(kept);
        Ok(report)
    }

    /// Removes every entry that is not pinned, with the files it owns. Files the user saved and
    /// hosted copies stay.
    pub fn clear(&self) -> Result<DeleteReport> {
        let entries = self.select("e.pinned = 0", &[])?;
        self.remove(
            entries
                .into_iter()
                .map(|e| {
                    let id = e.id.clone();
                    (e, DeleteOutcome::new(id))
                })
                .collect(),
        )
    }

    /// Removes the unpinned entries created before [`Retention::cutoff_ms`], with the files they
    /// own, and nothing else: with [`Retention::Forever`] or [`Retention::Off`] it removes
    /// nothing. A file the user saved outside the history's folder goes to the Trash only with
    /// [`SweepOptions::trash_saved_files`]; when that fails, the entry stays for the next sweep.
    pub fn sweep(&self, now_ms: i64, keep: Retention, options: SweepOptions) -> Result<DeleteReport> {
        let Some(cutoff) = keep.cutoff_ms(now_ms) else {
            return Ok(DeleteReport::default());
        };
        let entries = self.select("e.pinned = 0 AND e.created_at_ms < ?", &[Value::Integer(cutoff)])?;
        let mut doomed = Vec::new();
        let mut kept = Vec::new();
        for entry in entries {
            let mut outcome = DeleteOutcome::new(entry.id.clone());
            if options.trash_saved_files {
                if let Some(saved) = entry.saved_path.as_deref().filter(|p| !self.paths.contains(p)) {
                    outcome.saved_file = self.trash_file(saved);
                    if matches!(outcome.saved_file, StepOutcome::Failed { .. }) {
                        outcome.own_files = StepOutcome::Skipped;
                        kept.push(outcome);
                        continue;
                    }
                }
            }
            doomed.push((entry, outcome));
        }
        let mut report = self.remove(doomed)?;
        report.outcomes.extend(kept);
        Ok(report)
    }

    fn select(&self, filter: &str, values: &[Value]) -> Result<Vec<Entry>> {
        self.read(|conn| {
            let mut statement = conn.prepare(&format!("SELECT {COLUMNS} FROM entries e WHERE {filter}"))?;
            let entries = statement
                .query_map(params_from_iter(values), |row| self.entry(row, false))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(entries)
        })
    }

    fn trash_file(&self, path: &Path) -> StepOutcome {
        if !path.exists() {
            return StepOutcome::Missing;
        }
        match self.trash.trash(path) {
            Ok(()) => StepOutcome::Removed,
            Err(error) => StepOutcome::Failed { reason: error.to_string() },
        }
    }

    /// Deletes the rows in one transaction, then unlinks the files they owned: never the other
    /// way round, so a crash can leave unnamed files but never a row without its files.
    fn remove(&self, doomed: Vec<(Entry, DeleteOutcome)>) -> Result<DeleteReport> {
        let mut report = DeleteReport::default();
        if doomed.is_empty() {
            return Ok(report);
        }
        {
            let mut conn = self.write();
            let tx = conn.transaction()?;
            {
                let mut statement = tx.prepare("DELETE FROM entries WHERE id = ?1")?;
                for (entry, _) in &doomed {
                    statement.execute([&entry.id.0])?;
                }
            }
            tx.commit()?;
        }
        for (entry, mut outcome) in doomed {
            outcome.removed = true;
            let (own, freed) = self.unlink_own(&entry);
            outcome.own_files = own;
            outcome.freed_bytes = freed;
            report.freed_bytes += freed;
            report.outcomes.push(outcome);
        }
        Ok(report)
    }

    /// Unlinks the thumbnail, the history's copy and the edit (document and image) of an entry. A
    /// path that does not lead into the history's own folders is left alone, whatever the row
    /// said.
    fn unlink_own(&self, entry: &Entry) -> (StepOutcome, u64) {
        let base = entry.annotations.as_deref().map(HistoryPaths::edit_base);
        let paths: Vec<&Path> =
            [entry.thumbnail.as_deref(), entry.file.as_deref(), entry.annotations.as_deref(), base.as_deref()]
                .into_iter()
                .flatten()
                .collect();
        if paths.is_empty() {
            return (StepOutcome::Untouched, 0);
        }
        let mut freed = 0;
        let mut missing = 0;
        let mut failure = None;
        for path in &paths {
            if fs::symlink_metadata(path).is_err() {
                missing += 1;
                continue;
            }
            if !self.paths.owns(path) {
                failure.get_or_insert_with(|| {
                    format!("{} lies outside the history's folder, so it was left alone", path.display())
                });
                continue;
            }
            let bytes = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
            match fs::remove_file(path) {
                Ok(()) => freed += bytes,
                Err(error) => {
                    failure.get_or_insert_with(|| format!("{}: {error}", path.display()));
                }
            }
        }
        let outcome = match failure {
            Some(reason) => StepOutcome::Failed { reason },
            None if missing == paths.len() => StepOutcome::Missing,
            None => StepOutcome::Removed,
        };
        (outcome, freed)
    }

    /// Removes the files of the history's own folders that no entry names (left by a crash
    /// between a file write and its row, or a crash during a write), except those changed after
    /// `spare_after`, which may belong to a write in progress. It also lists the screenshots
    /// whose thumbnail is missing, so they can be made again, and forgets thumbnails that are gone.
    pub fn reconcile(&self, spare_after: Option<SystemTime>) -> Result<ReconcileReport> {
        let named: Vec<NamedFiles> = self.read(|conn| {
            let mut statement =
                conn.prepare("SELECT id, file, thumbnail, saved_path, kind, annotations FROM entries")?;
            let rows = statement
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        let mut keep: HashSet<PathBuf> = HashSet::new();
        let mut report = ReconcileReport::default();
        let mut gone_thumbnails = Vec::new();
        for (id, file, thumbnail, saved, kind, annotations) in &named {
            if let Some(file) = file {
                keep.insert(self.paths.resolve(file));
            }
            if let Some(document) = annotations {
                let document = self.paths.resolve(document);
                keep.insert(HistoryPaths::edit_base(&document));
                keep.insert(document);
            }
            let has_media = file.is_some() || saved.is_some();
            match thumbnail {
                Some(stored) => {
                    let path = self.paths.resolve(stored);
                    if path.is_file() {
                        keep.insert(path);
                    } else {
                        gone_thumbnails.push(id.clone());
                        if kind == "screenshot" && has_media {
                            report.missing_thumbnails.push(EntryId(id.clone()));
                        }
                    }
                }
                None if kind == "screenshot" && has_media => report.missing_thumbnails.push(EntryId(id.clone())),
                None => {}
            }
        }
        if !gone_thumbnails.is_empty() {
            let mut conn = self.write();
            let tx = conn.transaction()?;
            for id in &gone_thumbnails {
                tx.execute("UPDATE entries SET thumbnail = NULL, thumbnail_bytes = NULL WHERE id = ?1", [id])?;
            }
            tx.commit()?;
        }
        for folder in self.paths.own_folders() {
            let Ok(listing) = fs::read_dir(&folder) else { continue };
            for item in listing.flatten() {
                let path = item.path();
                let Ok(meta) = item.metadata() else { continue };
                if !meta.is_file() || keep.contains(&path) {
                    continue;
                }
                // A file written after `spare_after` may be a write in progress (its temporary
                // file, or its final file whose row is not committed yet).
                let recent = match (spare_after, meta.modified()) {
                    (Some(after), Ok(modified)) => modified > after,
                    _ => false,
                };
                if recent {
                    continue;
                }
                if fs::remove_file(&path).is_ok() {
                    report.removed_files.push(path);
                }
            }
        }
        Ok(report)
    }

    // ------------------------------------------------------------------------------- rows

    fn entry(&self, row: &Row<'_>, with_text: bool) -> rusqlite::Result<Entry> {
        let kind: String = row.get("kind")?;
        let width: Option<u32> = row.get("width")?;
        let height: Option<u32> = row.get("height")?;
        let display_id: Option<u32> = row.get("display_id")?;
        let hosted_url: Option<String> = row.get("hosted_url")?;
        let hosted = match hosted_url {
            Some(url) => Some(HostedCopy {
                provider: glimpsfy_upload::ProviderId(
                    row.get::<_, Option<String>>("hosted_provider")?.unwrap_or_default(),
                ),
                url,
                deletion: row
                    .get::<_, Option<String>>("hosted_deletion")?
                    .and_then(|json| serde_json::from_str(&json).ok()),
                uploaded_at_ms: row.get::<_, Option<i64>>("hosted_uploaded_at_ms")?.unwrap_or_default(),
                expires_at_ms: row.get("hosted_expires_at_ms")?,
            }),
            None => None,
        };
        let (text, text_lines) = if with_text {
            let lines: Option<String> = row.get("text_lines")?;
            (row.get("text")?, lines.and_then(|json| serde_json::from_str(&json).ok()))
        } else {
            (None, None)
        };
        Ok(Entry {
            id: EntryId(row.get("id")?),
            kind: EntryKind::parse(&kind).unwrap_or(EntryKind::Screenshot),
            name: row.get("name")?,
            created_at_ms: row.get("created_at_ms")?,
            file: row.get::<_, Option<String>>("file")?.map(|p| self.paths.resolve(&p)),
            thumbnail: row.get::<_, Option<String>>("thumbnail")?.map(|p| self.paths.resolve(&p)),
            annotations: row.get::<_, Option<String>>("annotations")?.map(|p| self.paths.resolve(&p)),
            saved_path: row.get::<_, Option<String>>("saved_path")?.map(PathBuf::from),
            size: width.zip(height).map(|(width, height)| PixelSize { width, height }),
            scale: row.get("scale")?,
            duration_ms: row.get::<_, Option<i64>>("duration_ms")?.map(|d| d.max(0) as u64),
            file_bytes: row.get::<_, Option<i64>>("file_bytes")?.map(|b| b.max(0) as u64),
            source: EntrySource {
                app_name: row.get("app_name")?,
                bundle_id: row.get("bundle_id")?,
                window_title: row.get("window_title")?,
            },
            display: display_id.map(|id| EntryDisplay { id, name: None }).map(|mut d| {
                d.name = row.get("display_name").ok().flatten();
                d
            }),
            text,
            text_lines,
            text_line_count: row.get("text_line_count")?,
            text_state: TextState::parse(&row.get::<_, String>("text_state")?),
            hosted,
            pinned: row.get("pinned")?,
        })
    }
}

fn json_error(error: serde_json::Error) -> HistoryError {
    HistoryError::Storage(format!("a value could not be stored as JSON: {error}"))
}

/// The `WHERE` clause of a query's filters (on the alias `e`), with its values.
fn filters(query: &HistoryQuery) -> (String, Vec<Value>) {
    let mut clauses = vec!["1".to_string()];
    let mut values = Vec::new();
    if !query.kinds.is_empty() {
        clauses.push(format!("e.kind IN ({})", vec!["?"; query.kinds.len()].join(", ")));
        values.extend(query.kinds.iter().map(|k| Value::Text(k.as_str().into())));
    }
    match query.uploaded {
        Some(true) => clauses.push("e.hosted_url IS NOT NULL".into()),
        Some(false) => clauses.push("e.hosted_url IS NULL".into()),
        None => {}
    }
    if let Some(pinned) = query.pinned {
        clauses.push("e.pinned = ?".into());
        values.push(Value::Integer(pinned as i64));
    }
    if let Some(app) = &query.app {
        clauses.push("e.app_name = ?".into());
        values.push(Value::Text(app.clone()));
    }
    (clauses.join(" AND "), values)
}

fn order_by(sort: SortOrder, searching: bool) -> &'static str {
    match sort {
        SortOrder::BestMatch if searching => "rank, e.created_at_ms DESC, e.seq DESC",
        SortOrder::Newest | SortOrder::BestMatch => "e.created_at_ms DESC, e.seq DESC",
        SortOrder::Oldest => "e.created_at_ms ASC, e.seq ASC",
        // An entry without a name of its own (the window names it) sorts by its app, near its
        // window-named neighbours, instead of at the top.
        SortOrder::Name => "COALESCE(NULLIF(e.name, ''), e.app_name, '') COLLATE FOLDED ASC, e.created_at_ms DESC",
        SortOrder::Size => "e.file_bytes IS NULL, e.file_bytes DESC, e.created_at_ms DESC",
    }
}

/// The sidebar's counts over every entry, or over the entries the FTS expression matches.
fn facets(conn: &Connection, expression: Option<&str>) -> Result<Facets> {
    let (scope, values): (&str, Vec<Value>) = match expression {
        Some(expression) => {
            ("seq IN (SELECT rowid FROM entries_fts WHERE entries_fts MATCH ?)", vec![Value::Text(expression.into())])
        }
        None => ("1", Vec::new()),
    };
    let mut facets = conn.query_row(
        &format!(
            "SELECT count(*), coalesce(sum(kind = 'screenshot'), 0), coalesce(sum(kind = 'recording'), 0),
                coalesce(sum(kind = 'textCopy'), 0), coalesce(sum(hosted_url IS NOT NULL), 0), coalesce(sum(pinned), 0)
             FROM entries WHERE {scope}"
        ),
        params_from_iter(&values),
        |r| {
            Ok(Facets {
                all: r.get(0)?,
                screenshots: r.get(1)?,
                recordings: r.get(2)?,
                text_copies: r.get(3)?,
                uploaded: r.get(4)?,
                pinned: r.get(5)?,
                apps: Vec::new(),
            })
        },
    )?;
    let mut statement = conn.prepare(&format!(
        "SELECT app_name, count(*) FROM entries WHERE app_name IS NOT NULL AND {scope}
         GROUP BY app_name ORDER BY count(*) DESC, app_name COLLATE FOLDED"
    ))?;
    facets.apps = statement
        .query_map(params_from_iter(&values), |r| Ok(AppCount { name: r.get(0)?, count: r.get(1)? }))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(facets)
}
