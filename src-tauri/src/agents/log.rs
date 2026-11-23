//! The log of agents' requests: one JSON line per request in `agents-log.jsonl` in the app's log
//! folder, kept for 7 days, which Settings › Agents shows.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{AppHandle, Emitter};

use crate::commands::agents::{events, AgentLogEntry};
use crate::commands::error::{CommandError, CommandResult};

/// How long a line stays.
const KEEP_MS: i64 = 7 * 24 * 60 * 60 * 1000;
/// The most lines Settings shows.
const SHOWN: usize = 200;

/// One writer at a time, so a line and a prune never interleave.
static FILE: Mutex<()> = Mutex::new(());

fn path(app: &AppHandle) -> Option<PathBuf> {
    crate::paths::log_dir(app).ok().map(|dir| dir.join("agents-log.jsonl"))
}

fn lines(path: &PathBuf, since: i64) -> Vec<AgentLogEntry> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    text.lines().filter_map(|l| serde_json::from_str::<AgentLogEntry>(l).ok()).filter(|e| e.at_ms >= since).collect()
}

/// Adds a line; a log that cannot be written costs only the line.
pub fn append(app: &AppHandle, entry: &AgentLogEntry) {
    let Some(path) = path(app) else {
        return;
    };
    {
        let _held = FILE.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let written = OpenOptions::new().create(true).append(true).open(&path).and_then(|mut file| {
            let mut line = serde_json::to_vec(entry).unwrap_or_default();
            line.push(b'\n');
            file.write_all(&line)
        });
        if let Err(error) = written {
            crate::trace::log(format!("[glimpsfy] agents: the log was not written: {error}"));
        }
    }
    let _ = app.emit(events::LOG_CHANGED, ());
}

/// The last 7 days, newest first. The lines that are older leave the file here.
pub fn read(app: &AppHandle) -> CommandResult<Vec<AgentLogEntry>> {
    let Some(path) = path(app) else {
        return Ok(Vec::new());
    };
    let _held = FILE.lock().unwrap_or_else(|p| p.into_inner());
    let since = super::now_ms() - KEEP_MS;
    let all = std::fs::read_to_string(&path).map(|t| t.lines().count()).unwrap_or(0);
    let mut kept = lines(&path, since);
    if kept.len() < all {
        let body: String = kept.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
        crate::settings::write_atomically(&path, body.as_bytes()).map_err(CommandError::from)?;
    }
    kept.reverse();
    kept.truncate(SHOWN);
    Ok(kept)
}

pub fn clear(app: &AppHandle) -> CommandResult<()> {
    if let Some(path) = path(app) {
        let _held = FILE.lock().unwrap_or_else(|p| p.into_inner());
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(CommandError::from(error)),
        }
    }
    let _ = app.emit(events::LOG_CHANGED, ());
    Ok(())
}
