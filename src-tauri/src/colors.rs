//! The colour picker's memory: the last eight colours it copied and the format it copies in, in
//! `colors.json` in the app's data folder. Nothing here leaves the device, and a lost or broken
//! file only empties the strip of recent colours, so the file is read leniently.

use std::fs;
use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::AppHandle;

use crate::{paths, settings, trace};

/// How many recent colours the strip keeps; 1 to 8 copy them.
pub const RECENT: usize = 8;

/// The format a click copies in. `P3` exists only on a Display P3 frame; on an sRGB frame the
/// picker shows HEX and keeps the choice for the next P3 frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColorFormat {
    #[default]
    Hex,
    Rgb,
    Hsl,
    P3,
}

impl ColorFormat {
    /// The format's name as the picker's tabs and the toast write it.
    pub fn label(self) -> &'static str {
        match self {
            ColorFormat::Hex => "HEX",
            ColorFormat::Rgb => "RGB",
            ColorFormat::Hsl => "HSL",
            ColorFormat::P3 => "P3",
        }
    }
}

/// What the picker remembers between captures.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ColorState {
    pub format: ColorFormat,
    /// Newest first, at most [`RECENT`], each a CSS colour: `#RRGGBB` for an sRGB frame and
    /// `color(display-p3 r g b)` for a Display P3 one, so a recent colour keeps its own space.
    pub recent: Vec<String>,
}

/// Every read-change-write of the file runs under this, so two copies cannot drop each other's
/// colour.
static FILE: Mutex<()> = Mutex::new(());

fn path(app: &AppHandle) -> Option<PathBuf> {
    paths::data_dir(app).ok().map(|dir| dir.join("colors.json"))
}

/// The remembered state; the defaults when the file is missing or unreadable.
pub fn state(app: &AppHandle) -> ColorState {
    let _held = FILE.lock().unwrap_or_else(|p| p.into_inner());
    read(app)
}

/// Remembers the format a click copies in.
pub fn set_format(app: &AppHandle, format: ColorFormat) -> std::io::Result<ColorState> {
    change(app, |state| state.format = format)
}

/// Puts `swatch` first in the recent colours, once.
pub fn remember(app: &AppHandle, swatch: &str) -> std::io::Result<ColorState> {
    let swatch = swatch.to_string();
    change(app, move |state| {
        state.recent.retain(|c| !c.eq_ignore_ascii_case(&swatch));
        state.recent.insert(0, swatch);
        state.recent.truncate(RECENT);
    })
}

fn change(app: &AppHandle, edit: impl FnOnce(&mut ColorState)) -> std::io::Result<ColorState> {
    let _held = FILE.lock().unwrap_or_else(|p| p.into_inner());
    let mut state = read(app);
    edit(&mut state);
    let path = path(app).ok_or_else(|| std::io::Error::other("no data folder"))?;
    let bytes = serde_json::to_vec_pretty(&state).map_err(std::io::Error::other)?;
    settings::write_atomically(&path, &bytes)?;
    Ok(state)
}

fn read(app: &AppHandle) -> ColorState {
    let Some(path) = path(app) else {
        return ColorState::default();
    };
    match fs::read(&path) {
        Ok(bytes) => parse(&bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ColorState::default(),
        Err(error) => {
            trace::log(format!("[glimpsfy] colors.json: {error}"));
            ColorState::default()
        }
    }
}

/// The state in a file, keeping what is good: an unknown format reads as HEX, and a recent entry
/// that is not a colour the picker writes is dropped (the toast page paints them as CSS).
fn parse(bytes: &[u8]) -> ColorState {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return ColorState::default();
    };
    let format = value.get("format").cloned().and_then(|f| serde_json::from_value(f).ok()).unwrap_or_default();
    let recent = value
        .get("recent")
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).filter(|c| is_swatch(c)).map(str::to_string).collect())
        .unwrap_or_default();
    let mut state = ColorState { format, recent };
    state.recent.truncate(RECENT);
    state
}

/// Whether `text` is a colour as the picker writes one: `#RRGGBB`, or `color(display-p3 r g b)`
/// with three numbers from 0 to 1.
pub fn is_swatch(text: &str) -> bool {
    if let Some(hex) = text.strip_prefix('#') {
        return hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    let Some(inner) = text.strip_prefix("color(display-p3 ").and_then(|t| t.strip_suffix(')')) else {
        return false;
    };
    let parts: Vec<&str> = inner.split(' ').collect();
    parts.len() == 3
        && parts.iter().all(|p| {
            p.len() <= 8
                && p.chars().all(|c| c.is_ascii_digit() || c == '.')
                && p.parse::<f64>().is_ok_and(|v| (0.0..=1.0).contains(&v))
        })
}
