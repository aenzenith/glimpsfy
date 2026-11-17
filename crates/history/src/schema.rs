//! The database layout and its forward migrations.
//!
//! `PRAGMA user_version` holds the schema version. Opening a database runs every migration above
//! its version, in order, inside one transaction, so a failed or interrupted migration leaves the
//! database as it was. A database from a newer Glimpsfy is left untouched and refused.
//!
//! Adding a version: append one migration to [`MIGRATIONS`] (never edit a released one), then
//! run `cargo test -p glimpsfy-history -- --ignored write_schema_fixture` once to freeze a
//! database of the new version in `crates/history/fixtures/`. The migration test opens every
//! fixture there and checks that no row is lost.

use rusqlite::functions::FunctionFlags;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use crate::fold::{fold, FOLD_VERSION};
use crate::{HistoryError, Result};

/// One SQL batch per version; `MIGRATIONS[0]` makes version 1 from an empty database.
const MIGRATIONS: &[&str] = &[
    // ----------------------------------------------------------------------------------- 1
    r#"
    CREATE TABLE entries (
        seq INTEGER PRIMARY KEY,
        id TEXT NOT NULL UNIQUE,
        kind TEXT NOT NULL CHECK (kind IN ('screenshot', 'recording', 'textCopy')),
        name TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL,
        -- the history's own files, relative to the history's folder
        file TEXT,
        thumbnail TEXT,
        thumbnail_bytes INTEGER,
        annotations TEXT,
        -- the user's own file, absolute
        saved_path TEXT,
        width INTEGER,
        height INTEGER,
        scale REAL,
        duration_ms INTEGER,
        file_bytes INTEGER,
        app_name TEXT,
        bundle_id TEXT,
        window_title TEXT,
        display_id INTEGER,
        display_name TEXT,
        -- recognised or copied text, and the recognised lines with their boxes as JSON
        text TEXT,
        text_lines TEXT,
        text_line_count INTEGER NOT NULL DEFAULT 0,
        text_state TEXT NOT NULL DEFAULT 'none',
        text_attempts INTEGER NOT NULL DEFAULT 0,
        text_retry_at_ms INTEGER,
        hosted_provider TEXT,
        hosted_url TEXT,
        hosted_deletion TEXT,
        hosted_uploaded_at_ms INTEGER,
        hosted_expires_at_ms INTEGER,
        pinned INTEGER NOT NULL DEFAULT 0
    );
    CREATE INDEX entries_by_time ON entries (created_at_ms, seq);
    CREATE INDEX entries_by_app ON entries (app_name);
    CREATE INDEX entries_text_queue ON entries (text_state, text_retry_at_ms);

    CREATE TABLE meta (key TEXT PRIMARY KEY, value) WITHOUT ROWID;

    -- The search index holds folded copies (fold.rs) of the searchable fields, kept in step with
    -- `entries` by the triggers below; its rowid is `entries.seq`.
    CREATE VIRTUAL TABLE entries_fts USING fts5 (
        name, window_title, app_name, text,
        tokenize = 'unicode61 remove_diacritics 0',
        prefix = '2 3'
    );
    CREATE TRIGGER entries_fts_insert AFTER INSERT ON entries BEGIN
        INSERT INTO entries_fts (rowid, name, window_title, app_name, text)
        VALUES (new.seq, history_fold(new.name), history_fold(new.window_title),
                history_fold(new.app_name), history_fold(new.text));
    END;
    CREATE TRIGGER entries_fts_update AFTER UPDATE OF name, window_title, app_name, text ON entries BEGIN
        UPDATE entries_fts SET name = history_fold(new.name), window_title = history_fold(new.window_title),
            app_name = history_fold(new.app_name), text = history_fold(new.text)
        WHERE rowid = new.seq;
    END;
    CREATE TRIGGER entries_fts_delete AFTER DELETE ON entries BEGIN
        DELETE FROM entries_fts WHERE rowid = old.seq;
    END;
    "#,
];

/// The version this build writes.
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Sets up a connection the way every connection of the store is set up.
pub(crate) fn prepare(conn: &Connection) -> Result<()> {
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.create_scalar_function(
        "history_fold",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get::<Option<String>>(0)?.map(|text| fold(&text))),
    )?;
    conn.create_collation("FOLDED", |a, b| fold(a).cmp(&fold(b)))?;
    Ok(())
}

/// Brings the database up to [`SCHEMA_VERSION`] and the search index up to the current folding.
pub(crate) fn migrate(conn: &mut Connection) -> Result<()> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let found: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if found > SCHEMA_VERSION {
        return Err(HistoryError::NewerSchema { found, known: SCHEMA_VERSION });
    }
    for (index, sql) in MIGRATIONS.iter().enumerate().skip(found.max(0) as usize) {
        tx.execute_batch(sql)
            .map_err(|error| HistoryError::Storage(format!("the migration to schema {} failed: {error}", index + 1)))?;
    }
    if found != SCHEMA_VERSION {
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    }

    // An index folded by another version of `fold` is rebuilt, so search never mixes two foldings.
    let folded_with: Option<i64> =
        tx.query_row("SELECT value FROM meta WHERE key = 'fold_version'", [], |row| row.get(0)).optional()?;
    if folded_with != Some(FOLD_VERSION) {
        tx.execute_batch(
            "DELETE FROM entries_fts;
             INSERT INTO entries_fts (rowid, name, window_title, app_name, text)
             SELECT seq, history_fold(name), history_fold(window_title), history_fold(app_name), history_fold(text)
             FROM entries;",
        )?;
        tx.execute("INSERT OR REPLACE INTO meta (key, value) VALUES ('fold_version', ?1)", params![FOLD_VERSION])?;
    }
    tx.commit()?;
    Ok(())
}
