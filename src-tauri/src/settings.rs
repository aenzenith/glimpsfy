//! Settings: every option of the settings chapter in `docs/DESIGN.md`, kept as JSON in the app's
//! config folder (`settings.json`).
//!
//! - **Forward compatible.** A file from an older or newer Glimpsfy loads without losing anything
//!   the user chose: a missing field takes its default, a field whose value no longer fits (a
//!   renamed option, a wrong type) takes its default alone, and a field this version does not know
//!   is kept in `extra` and written back unchanged (`load_lenient`).
//! - **Atomic.** A save writes a temporary file beside the real one, flushes it to disk and renames
//!   it over the old file, so a crash leaves either the old settings or the new ones, never half.
//! - **No secrets.** API keys, tokens and header values live in the system keychain (the upload
//!   module's job); nothing here is secret, and the store drops any value that a provider's form
//!   declares secret before it writes (`strip_secrets`).
//! - **One action per key.** No two actions keep the same key combination, whichever way the
//!   settings arrive (`Shortcuts::resolve_clashes`).
//!
//! The TypeScript mirror is `src/lib/settings.ts`; a field added here is added there too.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use glimpsfy_annotate::{BeautifyPreset, BeautifyStyle};
use glimpsfy_upload::{FieldInput, ProviderDescription};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The layout version of the file. It changes only when a field changes meaning; adding a field
/// never needs a new version, because missing fields take their defaults.
pub const SETTINGS_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub version: u32,
    pub general: General,
    pub shortcuts: Shortcuts,
    pub output: Output,
    pub beautify: Beautify,
    pub recording: Recording,
    pub ocr: Ocr,
    pub upload: Upload,
    pub agents: Agents,
    /// Fields a newer Glimpsfy wrote, kept as they are.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            version: SETTINGS_VERSION,
            general: General::default(),
            shortcuts: Shortcuts::default(),
            output: Output::default(),
            beautify: Beautify::default(),
            recording: Recording::default(),
            ocr: Ocr::default(),
            upload: Upload::default(),
            agents: Agents::default(),
            extra: Map::new(),
        }
    }
}

// ------------------------------------------------------------------------------------- general

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Language {
    /// The system's language when Glimpsfy speaks it, English otherwise.
    System,
    En,
    Tr,
    De,
    Es,
    Ru,
    Ja,
    #[serde(rename = "zh-Hans")]
    ZhHans,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Appearance {
    System,
    Light,
    Dark,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IconClick {
    OpenMenu,
    CaptureArea,
}

/// What the overlay does once a selection is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AfterSelection {
    Annotate,
    CopyAndClose,
    SaveAndClose,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct General {
    pub language: Language,
    pub appearance: Appearance,
    pub start_at_login: bool,
    pub show_menu_bar_icon: bool,
    pub icon_click: IconClick,
    pub after_selection: AfterSelection,
    pub include_pointer: bool,
    /// The wait of a delayed capture: 3, 5 or 10 seconds.
    pub delay_seconds: u32,
    pub shutter_sound: bool,
    pub check_for_updates: bool,
    /// Whether anything may go to an upload host: uploads, connection tests and deletions from
    /// the host. Off hides the Upload buttons; `network::gate` enforces it (on by default).
    pub allow_uploads: bool,
    /// Keeps Glimpsfy from opening any network connection: no upload, model download or update
    /// check. It wins over `allow_uploads` (off by default).
    pub offline: bool,
    /// Whether the user finished the first-run window. Until then it opens at every launch, and
    /// it opens anyway whenever the screen recording permission is missing.
    pub first_run_done: bool,
    /// The sizes saved from the selection's size menu (the overlay and the recording setup),
    /// oldest first. The pages keep at most 12.
    pub saved_sizes: Vec<Kept<SavedSize>>,
    /// The last interval capture's plan, which the popover's form starts from.
    pub interval: IntervalPlan,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What an interval capture takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntervalTarget {
    /// An area selected in the overlay first.
    Area,
    /// The frontmost window when the series starts, followed until it closes.
    #[default]
    Window,
    /// The display under the pointer when the series starts.
    Display,
}

/// When an interval series ends on its own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IntervalEnd {
    /// After `end_value` frames.
    #[default]
    Frames,
    /// After `end_value` minutes.
    Minutes,
    /// When the user stops it, and at the latest after [`IntervalPlan::MAX_MINUTES`].
    Manual,
}

/// An interval capture's plan: the target, the time between frames and the end.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct IntervalPlan {
    pub target: IntervalTarget,
    pub every_seconds: u32,
    pub end: IntervalEnd,
    /// Frames or minutes, as `end` says; a manual end ignores it.
    pub end_value: u32,
}

impl IntervalPlan {
    /// A series never holds more frames than this, whatever its end says.
    pub const MAX_FRAMES: u32 = 2000;
    /// Nor runs longer than this (12 hours).
    pub const MAX_MINUTES: u32 = 12 * 60;
    /// The longest time between two frames (1 hour).
    pub const MAX_EVERY_SECONDS: u32 = 60 * 60;

    /// The plan with every number inside what a series allows.
    pub fn clamped(self) -> IntervalPlan {
        let every_seconds = self.every_seconds.clamp(1, Self::MAX_EVERY_SECONDS);
        let end_value = match self.end {
            IntervalEnd::Frames => self.end_value.clamp(1, Self::MAX_FRAMES),
            IntervalEnd::Minutes => self.end_value.clamp(1, Self::MAX_MINUTES),
            IntervalEnd::Manual => self.end_value,
        };
        IntervalPlan { every_seconds, end_value, ..self }
    }

    /// How many frames the series takes at most.
    pub fn frame_limit(self) -> u32 {
        let plan = self.clamped();
        let by_time = |minutes: u32| (minutes as u64 * 60 / plan.every_seconds as u64) as u32 + 1;
        let planned = match plan.end {
            IntervalEnd::Frames => plan.end_value,
            IntervalEnd::Minutes => by_time(plan.end_value),
            IntervalEnd::Manual => by_time(Self::MAX_MINUTES),
        };
        planned.clamp(1, Self::MAX_FRAMES)
    }
}

impl Default for IntervalPlan {
    fn default() -> Self {
        IntervalPlan { target: IntervalTarget::Window, every_seconds: 5, end: IntervalEnd::Frames, end_value: 60 }
    }
}

/// A size saved from the size menu, in frame pixels, as the size label counts them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedSize {
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Fields a newer Glimpsfy gave the entry, kept as they are.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One entry of a list the user built. `load_lenient` tries a list as one value, so a single entry
/// that does not fit (hand-edited, or of a shape a newer Glimpsfy wrote) would cost the whole
/// list; such an entry is kept here as it is and written back unchanged, and the pages skip it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Kept<T> {
    Known(T),
    Other(Value),
}

impl Default for General {
    fn default() -> Self {
        General {
            language: Language::System,
            appearance: Appearance::System,
            start_at_login: false,
            show_menu_bar_icon: true,
            icon_click: IconClick::OpenMenu,
            after_selection: AfterSelection::Annotate,
            include_pointer: false,
            delay_seconds: 5,
            shutter_sound: true,
            check_for_updates: true,
            allow_uploads: true,
            offline: false,
            first_run_done: false,
            saved_sizes: Vec::new(),
            interval: IntervalPlan::default(),
            extra: Map::new(),
        }
    }
}

// ----------------------------------------------------------------------------------- shortcuts

/// One global shortcut per action, as an accelerator the global-shortcut plugin parses
/// ("Shift+CommandOrControl+2"). `None` leaves the action without a key. `CommandOrControl` is ⌘
/// on macOS and Ctrl elsewhere, and `Alt` is ⌥, so one string serves every platform.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Shortcuts {
    pub capture_area: Option<String>,
    pub capture_window: Option<String>,
    pub capture_display: Option<String>,
    pub capture_all_displays: Option<String>,
    pub capture_area_delayed: Option<String>,
    pub repeat_last_area: Option<String>,
    pub copy_text: Option<String>,
    pub record_screen: Option<String>,
    pub record_display: Option<String>,
    pub pause_recording: Option<String>,
    pub mute_microphone: Option<String>,
    /// Discard has no default, so a stray key never deletes a recording.
    pub discard_recording: Option<String>,
    pub open_history: Option<String>,
    /// The ruler over the frozen screen. It comes after the shortcut table's rows in `known_mut`,
    /// so a file from before it that gave its default keys to another action keeps them there.
    pub measure_screen: Option<String>,
    /// The colour picker: the magnifier alone. It comes after the ruler in `known_mut`, so a
    /// file from before it that gave ⌥⇧⌘I to another action keeps it there.
    pub pick_color: Option<String>,
    /// Drawing mode and the spotlight on a take. Their keys are registered only while a take
    /// records or pauses (`shortcuts::bind_take`). They come after the colour picker in
    /// `known_mut`, so a file from before them that gave ⌥⇧⌘D or ⌥⇧⌘L to another action keeps
    /// it there.
    pub draw_on_screen: Option<String>,
    pub spotlight: Option<String>,
    /// Starts or stops an interval capture. It comes after the older actions in `known_mut`, so a
    /// file from before it that gave ⌥⇧⌘K to another action keeps it there.
    pub interval_capture: Option<String>,
    /// Selects an area and goes straight into the scrolling capture; macOS only. It comes last
    /// in `known_mut`, so a file from before it that gave ⌥⇧⌘S to another action keeps it there.
    pub scroll_capture: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Shortcuts {
    /// The scheme of DESIGN.md's shortcut table: 1 and 2 capture, ⌥ widens, ⌃ repeats, ⌃⌥ delays,
    /// and ⌥⇧⌘ with a letter does the rest. Windows and Linux have no free fourth modifier, so the
    /// ⌃ layer moves (Repeat last area) or has no default there yet.
    fn default() -> Self {
        let key = |s: &str| Some(s.to_string());
        let mac = cfg!(target_os = "macos");
        Shortcuts {
            capture_area: key("Shift+CommandOrControl+2"),
            capture_window: key("Alt+Shift+CommandOrControl+2"),
            capture_display: key("Shift+CommandOrControl+1"),
            capture_all_displays: key("Alt+Shift+CommandOrControl+1"),
            capture_area_delayed: if mac { key("Control+Alt+Shift+Command+2") } else { None },
            repeat_last_area: if mac { key("Control+Shift+Command+2") } else { key("Control+Shift+3") },
            copy_text: key("Alt+Shift+CommandOrControl+C"),
            record_screen: key("Alt+Shift+CommandOrControl+R"),
            record_display: if mac { key("Control+Alt+Shift+Command+R") } else { None },
            pause_recording: key("Alt+Shift+CommandOrControl+P"),
            mute_microphone: key("Alt+Shift+CommandOrControl+M"),
            discard_recording: None,
            open_history: key("Alt+Shift+CommandOrControl+H"),
            measure_screen: key("Alt+Shift+CommandOrControl+G"),
            pick_color: key("Alt+Shift+CommandOrControl+I"),
            draw_on_screen: key("Alt+Shift+CommandOrControl+D"),
            spotlight: key("Alt+Shift+CommandOrControl+L"),
            interval_capture: key("Alt+Shift+CommandOrControl+K"),
            scroll_capture: if mac { key("Alt+Shift+Command+S") } else { None },
            extra: Map::new(),
        }
    }
}

/// An action that lost its key because another action holds the same combination.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShortcutCleared {
    /// The action's name in the settings file (`captureWindow`).
    pub action: String,
    pub accelerator: String,
    /// The action that keeps the combination.
    pub kept_by: String,
}

impl Shortcuts {
    /// No action has a key: what the shortcut recorder registers while it listens.
    pub fn none() -> Shortcuts {
        let mut none = Shortcuts { extra: Map::new(), ..Shortcuts::default() };
        for (_, slot) in none.known_mut() {
            *slot = None;
        }
        none
    }

    /// The known actions by their names in the file, in the order of the shortcut table and then
    /// the actions added since (the ruler, the colour picker, drawing, the spotlight, the
    /// interval capture and the scrolling capture), so a newer action's default never takes the
    /// keys an older file gave another row. On a clash that no earlier holder decides, the earlier
    /// row keeps the key.
    fn known_mut(&mut self) -> [(&'static str, &mut Option<String>); 19] {
        [
            ("captureArea", &mut self.capture_area),
            ("captureWindow", &mut self.capture_window),
            ("captureDisplay", &mut self.capture_display),
            ("captureAllDisplays", &mut self.capture_all_displays),
            ("captureAreaDelayed", &mut self.capture_area_delayed),
            ("repeatLastArea", &mut self.repeat_last_area),
            ("copyText", &mut self.copy_text),
            ("recordScreen", &mut self.record_screen),
            ("recordDisplay", &mut self.record_display),
            ("pauseRecording", &mut self.pause_recording),
            ("muteMicrophone", &mut self.mute_microphone),
            ("discardRecording", &mut self.discard_recording),
            ("openHistory", &mut self.open_history),
            ("measureScreen", &mut self.measure_screen),
            ("pickColor", &mut self.pick_color),
            ("drawOnScreen", &mut self.draw_on_screen),
            ("spotlight", &mut self.spotlight),
            ("intervalCapture", &mut self.interval_capture),
            ("scrollCapture", &mut self.scroll_capture),
        ]
    }

    /// Every action with a key, by name, in table order and then the actions a newer Glimpsfy
    /// added (kept in `extra`), with the canonical form of the key.
    fn held(&mut self) -> Vec<(String, String)> {
        let mut held: Vec<(String, String)> = self
            .known_mut()
            .into_iter()
            .filter_map(|(name, key)| Some((name.to_string(), canonical_accelerator(key.as_deref()?)?)))
            .collect();
        for (name, value) in &self.extra {
            if let Some(key) = value.as_str().and_then(canonical_accelerator) {
                held.push((name.clone(), key));
            }
        }
        held
    }

    fn clear(&mut self, name: &str) {
        for (known, key) in self.known_mut() {
            if known == name {
                *key = None;
                return;
            }
        }
        if let Some(value) = self.extra.get_mut(name) {
            *value = Value::Null;
        }
    }

    fn accelerator(&mut self, name: &str) -> String {
        for (known, key) in self.known_mut() {
            if known == name {
                return key.clone().unwrap_or_default();
            }
        }
        self.extra.get(name).and_then(Value::as_str).unwrap_or_default().to_string()
    }

    /// Makes sure no two actions hold the same key combination, however the settings were made:
    /// the recorder, Restore defaults, an imported or a hand-edited file. On a clash the action
    /// that held the combination in `previous` keeps it (the user moved nothing, so nothing
    /// moves), otherwise the earlier row of the table does; every other holder is cleared.
    /// Spellings that mean the same keys ("CommandOrControl+Shift+2", "Shift+Cmd+2") clash.
    pub fn resolve_clashes(&mut self, previous: Option<&Shortcuts>) -> Vec<ShortcutCleared> {
        let before: Vec<(String, String)> = previous.map(|p| p.clone().held()).unwrap_or_default();
        let held = self.held();
        let mut cleared = Vec::new();
        let mut seen: Vec<&str> = Vec::new();
        for (_, key) in &held {
            if seen.contains(&key.as_str()) {
                continue;
            }
            seen.push(key);
            let holders: Vec<&String> = held.iter().filter(|(_, k)| k == key).map(|(name, _)| name).collect();
            if holders.len() < 2 {
                continue;
            }
            let keeper = holders
                .iter()
                .find(|name| before.iter().any(|(n, k)| n == **name && k == key))
                .unwrap_or(&holders[0])
                .to_string();
            for name in holders {
                if *name != keeper {
                    let accelerator = self.accelerator(name);
                    self.clear(name);
                    cleared.push(ShortcutCleared { action: name.clone(), accelerator, kept_by: keeper.clone() });
                }
            }
        }
        cleared
    }
}

/// The canonical spelling of an accelerator: the modifiers in one order (Control, Alt, Shift,
/// Super) and the key in upper case, so two spellings of the same keys compare equal.
/// `CommandOrControl` is Super (⌘) on macOS and Control elsewhere. `None` for an empty string or
/// one without a key.
pub fn canonical_accelerator(accelerator: &str) -> Option<String> {
    let (mut control, mut alt, mut shift, mut command) = (false, false, false, false);
    let mut key = None;
    for token in accelerator.split('+').map(str::trim).filter(|t| !t.is_empty()) {
        match token.to_ascii_lowercase().as_str() {
            "shift" => shift = true,
            "control" | "ctrl" => control = true,
            "alt" | "option" => alt = true,
            "command" | "cmd" | "super" | "meta" => command = true,
            "commandorcontrol" | "commandorctrl" | "cmdorctrl" | "cmdorcontrol" => {
                if cfg!(target_os = "macos") {
                    command = true
                } else {
                    control = true
                }
            }
            other => {
                // "Digit2" and "KeyC" are the plugin's long names of "2" and "C".
                let short = other.strip_prefix("digit").or_else(|| other.strip_prefix("key"));
                let other = short.filter(|s| s.chars().count() == 1).unwrap_or(other);
                key = Some(other.to_ascii_uppercase());
            }
        }
    }
    let key = key?;
    let mut parts: Vec<&str> = Vec::new();
    for (on, name) in [(control, "Control"), (alt, "Alt"), (shift, "Shift"), (command, "Super")] {
        if on {
            parts.push(name);
        }
    }
    parts.push(&key);
    Some(parts.join("+"))
}

// -------------------------------------------------------------------------------------- output

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ImageFormat {
    Png,
    Jpeg,
    Webp,
}

impl ImageFormat {
    pub fn extension(self) -> &'static str {
        match self {
            ImageFormat::Png => "png",
            ImageFormat::Jpeg => "jpg",
            ImageFormat::Webp => "webp",
        }
    }
}

/// How a capture of a 2× display is saved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RetinaScale {
    Keep2x,
    ScaleTo1x,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowBackground {
    Transparent,
    White,
}

/// How long the history keeps a capture.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum HistoryRetention {
    Off,
    Days7,
    Days30,
    Days90,
    Forever,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Output {
    pub save_folder: PathBuf,
    /// The file name without its extension. Tokens: `{date}`, `{time}`, `{app}`, `{title}` (or
    /// `{window}`), `{counter}`, `{width}`, `{height}` and `{text}`, the first line of the text read
    /// from a saved image (`output::file_stem_titled`).
    pub file_name_template: String,
    pub format: ImageFormat,
    /// JPEG and WebP quality, 50 to 100.
    pub quality: u32,
    pub retina: RetinaScale,
    pub copy_after_save: bool,
    /// Whether a saved PNG is compressed again (oxipng) before it is written: the same pixels in
    /// a smaller file, for a save that may take a second or two longer.
    pub optimise_png: bool,
    /// Leaves the desktop icons, notification banners and widgets out of screenshots (macOS,
    /// `CaptureOptions::clean_desktop`); the screen and the system's settings stay as they are.
    pub clean_desktop: bool,
    pub window_shadow: bool,
    pub window_corners: bool,
    /// Fills the translucent parts of a captured window with the system's window colour (on by
    /// default); `WindowCaptureOptions::opaque_body`.
    pub window_backing: bool,
    /// Draws a thin line along a captured window's edge and rounded corners (off by default).
    pub window_border: bool,
    pub window_background: WindowBackground,
    pub history_retention: HistoryRetention,
    /// Whether the history's retention sweep also moves an expired entry's saved file (the copy
    /// in the save folder) to the Trash. Off, the saved files are the user's and stay.
    pub trash_saved_files: bool,
    /// The apps of Send to app, in the order of the send list, whose digits 1–9 follow it.
    pub send_targets: Vec<SendTarget>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// What a send target gets on the clipboard before Glimpsfy presses ⌘V in it. A recording goes as
/// its file to an `image` target, since no app pastes a video as pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SendPaste {
    /// The PNG, drawings included.
    Image,
    /// The file's absolute path as text, quoted for a shell (`send::quote_path`).
    Path,
    /// The file itself, as Finder copies it.
    File,
}

/// One app of Send to app, picked in Settings › Output.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendTarget {
    /// The app's bundle identifier, which finds it wherever it is installed; also the target's id.
    pub bundle_id: String,
    /// The app's name when it was added, shown while the app is not installed.
    pub name: String,
    pub paste: SendPaste,
}

impl Default for Output {
    fn default() -> Self {
        Output {
            save_folder: crate::paths::default_pictures(),
            file_name_template: "Glimpsfy {date} at {time}".into(),
            format: ImageFormat::Png,
            quality: 90,
            retina: RetinaScale::Keep2x,
            copy_after_save: false,
            optimise_png: true,
            clean_desktop: false,
            window_shadow: true,
            window_corners: true,
            window_backing: true,
            window_border: false,
            window_background: WindowBackground::Transparent,
            history_retention: HistoryRetention::Days30,
            trash_saved_files: false,
            send_targets: Vec::new(),
            extra: Map::new(),
        }
    }
}

// ------------------------------------------------------------------------------------ beautify

/// The presets of a list that this version reads; the others stay in the list as they are.
fn read_presets(presets: &[Kept<BeautifyPreset>]) -> impl Iterator<Item = &BeautifyPreset> {
    presets.iter().filter_map(|item| match item {
        Kept::Known(preset) => Some(preset),
        Kept::Other(_) => None,
    })
}

/// Settings › Beautify: the presets, and whether one is put around every capture by itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Beautify {
    /// Every screenshot gets `preset` unless F skips it for that capture.
    pub auto: bool,
    /// The id of the preset `auto` applies, and F turns on when `auto` is off.
    pub preset: String,
    /// In the order Settings lists them; a new installation has the built-in ones.
    pub presets: Vec<Kept<BeautifyPreset>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Beautify {
    fn default() -> Self {
        Beautify {
            auto: false,
            preset: glimpsfy_annotate::DEFAULT_PRESET.into(),
            presets: glimpsfy_annotate::built_in_presets().into_iter().map(Kept::Known).collect(),
            extra: Map::new(),
        }
    }
}

impl Beautify {
    /// The presets this version reads, in their order; one a newer version wrote stays in the
    /// file without being offered.
    pub fn offered(&self) -> impl Iterator<Item = &BeautifyPreset> {
        read_presets(&self.presets)
    }

    /// The style of preset `id` with its id in it, or `None` when no preset this version reads
    /// has that id.
    pub fn style(&self, id: &str) -> Option<BeautifyStyle> {
        read_presets(&self.presets)
            .find(|p| p.id == id)
            .map(|p| BeautifyStyle { preset: Some(p.id.clone()), ..p.style.clone() })
    }

    /// The style F turns on: the chosen preset's, else the first one this version reads, else
    /// Ocean as it ships.
    pub fn chosen(&self) -> BeautifyStyle {
        self.style(&self.preset)
            .or_else(|| read_presets(&self.presets).next().and_then(|p| self.style(&p.id)))
            .unwrap_or_else(|| {
                let ocean = glimpsfy_annotate::built_in_presets().swap_remove(0);
                BeautifyStyle { preset: Some(ocean.id), ..ocean.style }
            })
    }

    /// What a new capture applies by itself: the chosen preset when `auto` is on.
    pub fn automatic(&self) -> Option<BeautifyStyle> {
        self.auto.then(|| self.chosen())
    }
}

// ----------------------------------------------------------------------------------- recording

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum VideoCodec {
    H264,
    Hevc,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RecordingResolution {
    Native,
    Half,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RingColor {
    Yellow,
    Red,
    Blue,
    White,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Size2 {
    Small,
    Large,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Size3 {
    Small,
    Medium,
    Large,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum KeysPosition {
    BottomCentre,
    BottomLeft,
    BottomRight,
    TopCentre,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WebcamShape {
    Circle,
    RoundedSquare,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Corner {
    BottomRight,
    BottomLeft,
    TopRight,
    TopLeft,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Recording {
    pub codec: VideoCodec,
    /// 30 or 60.
    pub fps: u32,
    pub resolution: RecordingResolution,
    pub system_audio: bool,
    pub microphone: bool,
    /// The system's identifier of the chosen microphone; `None` follows the system default.
    pub microphone_device: Option<String>,
    /// Each source on its own track, plus a mixed one.
    pub separate_tracks: bool,
    /// 0 turns the countdown off; otherwise 3 or 5.
    pub countdown_seconds: u32,
    /// Leaves the desktop icons, notification banners and widgets out of display and area takes
    /// (macOS, `RecordingConfig::clean_desktop`).
    pub clean_desktop: bool,
    pub highlight_clicks: bool,
    pub click_ring_color: RingColor,
    pub click_ring_size: Size2,
    pub show_keys: bool,
    pub keys_position: KeysPosition,
    pub keys_size: Size3,
    pub keys_only_shortcuts: bool,
    pub webcam_device: Option<String>,
    pub webcam_shape: WebcamShape,
    pub webcam_size: Size3,
    pub webcam_corner: Corner,
    pub recordings_folder: PathBuf,
    /// GIF export defaults: frames per second (10, 15 or 20) and width (`None` keeps the original).
    pub gif_fps: u32,
    pub gif_width: Option<u32>,
    /// Whether drawings on a take fade 3 s after the last stroke. The Presentation card sets it,
    /// and the next take starts with the same choice.
    pub ink_fades: bool,
    /// A take stops and saves itself after this many minutes of recording (pauses do not
    /// count); `None` records until the user stops. 1 to 240.
    pub time_limit_minutes: Option<u32>,
    /// The language the trim window's text panel reads a recording's speech in, as the speech
    /// model's code ("tr", "en"…), or "auto" to let the model detect it.
    pub transcript_language: String,
    /// A silence counts as long from this many milliseconds: 500, 1000 or 2000 (the Shorten
    /// menu of the text panel).
    pub silence_threshold_ms: u32,
    /// Whether Shorten leaves 0.3 s of a long silence (`true`) or cuts it whole.
    pub silence_shorten: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Recording {
    fn default() -> Self {
        Recording {
            codec: VideoCodec::H264,
            fps: 60,
            resolution: RecordingResolution::Native,
            system_audio: false,
            microphone: true,
            microphone_device: None,
            separate_tracks: true,
            countdown_seconds: 3,
            clean_desktop: false,
            highlight_clicks: true,
            click_ring_color: RingColor::Yellow,
            click_ring_size: Size2::Small,
            show_keys: true,
            keys_position: KeysPosition::BottomCentre,
            keys_size: Size3::Medium,
            keys_only_shortcuts: true,
            webcam_device: None,
            webcam_shape: WebcamShape::Circle,
            webcam_size: Size3::Medium,
            webcam_corner: Corner::BottomRight,
            recordings_folder: crate::paths::default_movies(),
            gif_fps: 15,
            gif_width: Some(640),
            ink_fades: false,
            time_limit_minutes: None,
            transcript_language: "auto".into(),
            silence_threshold_ms: 1000,
            silence_shorten: true,
            extra: Map::new(),
        }
    }
}

// ----------------------------------------------------------------------------------------- ocr

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OcrEngineChoice {
    /// The operating system's engine: Vision on macOS, Windows.Media.Ocr on Windows.
    System,
    /// The PP-OCR models on ONNX Runtime, the only engine on Linux.
    PpOcr,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Ocr {
    pub engine: OcrEngineChoice,
    pub detect_language: bool,
    /// BCP 47 codes, such as "en" and "tr". The engine reports which it supports.
    pub languages: Vec<String>,
    pub keep_line_breaks: bool,
    pub detect_codes: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Ocr {
    fn default() -> Self {
        Ocr {
            engine: if cfg!(target_os = "linux") { OcrEngineChoice::PpOcr } else { OcrEngineChoice::System },
            detect_language: true,
            languages: vec!["en".into(), "tr".into()],
            keep_line_breaks: true,
            detect_codes: true,
            extra: Map::new(),
        }
    }
}

// -------------------------------------------------------------------------------------- upload

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CopyAfterUpload {
    Link,
    Markdown,
    Nothing,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Upload {
    /// No provider is the default until the user picks one.
    pub default_provider: Option<String>,
    pub copy_after_upload: CopyAfterUpload,
    pub open_in_browser: bool,
    /// Providers whose first-upload confirmation the user has already given.
    pub confirmed_providers: Vec<String>,
    /// Each provider's own non-secret settings, keyed by provider id, in the shape the provider's
    /// `describe()` declares (retention, bucket, endpoint, path prefix…). Secrets never go here.
    pub providers: BTreeMap<String, Value>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Upload {
    fn default() -> Self {
        Upload {
            default_provider: None,
            copy_after_upload: CopyAfterUpload::Link,
            open_in_browser: false,
            confirmed_providers: Vec::new(),
            providers: BTreeMap::new(),
            extra: Map::new(),
        }
    }
}

// -------------------------------------------------------------------------------------- agents

/// Settings › Agents: the local MCP server (macOS only). It is off until the user turns it on.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Agents {
    /// Agents may connect and send requests, each of which the user approves.
    pub enabled: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

// ---------------------------------------------------------------------------- load and save

/// Builds a `T` from whatever a file holds, keeping every value that fits and the default for
/// every value that does not. Each leaf of `user` is tried in place and kept only if the whole
/// document still deserializes, so one bad field never costs the others.
pub fn load_lenient<T: Serialize + DeserializeOwned + Default>(user: Value) -> T {
    let mut root = serde_json::to_value(T::default()).unwrap_or(Value::Object(Map::new()));
    let mut path = Vec::new();
    merge::<T>(&mut root, &mut path, user);
    serde_json::from_value(root).unwrap_or_default()
}

fn merge<T: DeserializeOwned>(root: &mut Value, path: &mut Vec<String>, user: Value) {
    match user {
        Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                path.push(key);
                merge::<T>(root, path, value);
                path.pop();
            }
        }
        value => {
            if path.is_empty() {
                return;
            }
            // The leaf goes into a copy: on its way `put` may turn a scalar into an object, and
            // undoing only the leaf would leave that object behind and no later leaf would fit.
            let mut candidate = root.clone();
            put(&mut candidate, path, value);
            if T::deserialize(&candidate).is_ok() {
                *root = candidate;
            }
        }
    }
}

/// Puts `value` at `path`, making objects on the way.
fn put(root: &mut Value, path: &[String], value: Value) -> Option<()> {
    let (last, parents) = path.split_last()?;
    let mut node = root;
    for key in parents {
        if !node.is_object() {
            *node = Value::Object(Map::new());
        }
        node = node.as_object_mut()?.entry(key.clone()).or_insert_with(|| Value::Object(Map::new()));
    }
    if !node.is_object() {
        *node = Value::Object(Map::new());
    }
    node.as_object_mut()?.insert(last.clone(), value);
    Some(())
}

/// Writes `bytes` to `path` through a temporary file in the same folder and a rename, so the
/// file is never seen half-written.
pub fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("settings.json");
    let temp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// How a provider's form keeps a row out of the settings file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretKind {
    /// A `secret` row: its value lives in the keychain, so the file holds nothing under its key.
    Whole,
    /// A `pairs` row with secret values (headers): the file keeps only the names.
    PairValues,
}

/// The secret rows of each provider's settings form, by provider id and row key. A value under
/// one of them never reaches the file.
pub type SecretFields = BTreeMap<String, BTreeMap<String, SecretKind>>;

pub fn secret_fields(providers: &[ProviderDescription]) -> SecretFields {
    providers
        .iter()
        .map(|provider| {
            let keys = provider
                .settings
                .iter()
                .filter_map(|field| match field.input {
                    FieldInput::Secret { .. } => Some((field.key.clone(), SecretKind::Whole)),
                    FieldInput::Pairs { secret_values: true, .. } => Some((field.key.clone(), SecretKind::PairValues)),
                    _ => None,
                })
                .collect();
            (provider.id.as_str().to_string(), keys)
        })
        .collect()
}

/// Removes every value that a provider's form declares secret from `upload.providers`, so a
/// secret typed into a form never lands in `settings.json`, whatever page sent it: a secret row
/// goes, and a secret-valued list keeps only its names. Returns whether anything was removed.
fn strip_secrets(settings: &mut Settings, secrets: &SecretFields) -> bool {
    let mut stripped = false;
    for (provider, values) in settings.upload.providers.iter_mut() {
        let (Some(keys), Some(values)) = (secrets.get(provider), values.as_object_mut()) else {
            continue;
        };
        for (key, kind) in keys {
            match kind {
                SecretKind::Whole => stripped |= values.remove(key).is_some(),
                SecretKind::PairValues => {
                    let Some(list) = values.get_mut(key) else {
                        continue;
                    };
                    let Some(items) = list.as_array() else {
                        // Neither names nor pairs: nothing the form wrote, so nothing is kept.
                        values.remove(key);
                        stripped = true;
                        continue;
                    };
                    let names: Vec<Value> = items
                        .iter()
                        .filter_map(|item| match item {
                            Value::String(_) => Some(item.clone()),
                            Value::Object(pair) => pair.get("name").filter(|n| n.is_string()).cloned(),
                            _ => None,
                        })
                        .collect();
                    if names.len() != items.len() || items.iter().any(|item| !item.is_string()) {
                        *list = Value::Array(names);
                        stripped = true;
                    }
                }
            }
        }
    }
    stripped
}

/// What a save changed beyond what the caller sent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SaveNotes {
    /// Actions that lost their key because another action holds it.
    pub cleared_shortcuts: Vec<ShortcutCleared>,
}

/// The settings of the running app and the file they live in.
pub struct SettingsStore {
    path: PathBuf,
    current: RwLock<Settings>,
    secrets: SecretFields,
}

impl SettingsStore {
    /// Reads the file with the built-in upload providers' forms.
    pub fn open(path: PathBuf) -> SettingsStore {
        let providers: Vec<ProviderDescription> =
            glimpsfy_upload::builtin_providers().iter().map(|provider| provider.describe()).collect();
        SettingsStore::open_with(path, secret_fields(&providers))
    }

    /// Reads the file, or starts from the defaults when there is none. A file that is not JSON at
    /// all is kept beside the new one as `settings.corrupt.json`, so nothing the user had is lost.
    /// A file edited by hand (or imported) that gives two actions the same keys, or holds a secret,
    /// is repaired and written back at once, so the file never keeps either.
    pub fn open_with(path: PathBuf, secrets: SecretFields) -> SettingsStore {
        let mut settings = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
                Ok(value) => load_lenient(value),
                Err(_) => {
                    let _ = fs::rename(&path, path.with_file_name("settings.corrupt.json"));
                    Settings::default()
                }
            },
            Err(_) => Settings::default(),
        };
        let cleared = settings.shortcuts.resolve_clashes(None);
        let stripped = strip_secrets(&mut settings, &secrets);
        if (!cleared.is_empty() || stripped) && path.exists() {
            if let Ok(bytes) = serde_json::to_vec_pretty(&settings) {
                let _ = write_atomically(&path, &bytes);
            }
        }
        SettingsStore { path, current: RwLock::new(settings), secrets }
    }

    pub fn get(&self) -> Settings {
        self.current.read().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
    }

    /// Reads the current settings without copying them. The network gate asks it before every
    /// request and every 64 KB of a body, where a whole copy each time would be waste.
    pub fn read<T>(&self, look: impl FnOnce(&Settings) -> T) -> T {
        look(&self.current.read().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    /// Saves `settings` and makes them current. The file is written first, so the app never runs
    /// with settings it could not keep.
    pub fn set(&self, settings: Settings) -> std::io::Result<Settings> {
        self.save(settings).map(|(saved, _)| saved)
    }

    /// Saves `settings` as `set` does and says what the store had to change: a key combination
    /// that two actions share stays with the one that held it before, and secrets are dropped.
    /// Saves are serialised, so two windows saving at once cannot interleave.
    pub fn save(&self, settings: Settings) -> std::io::Result<(Settings, SaveNotes)> {
        let mut current = self.lock();
        self.write(&mut current, settings)
    }

    /// Saves settings that a page sent. Consent to an upload host is not a page's to give or
    /// take: the stored `upload.confirmedProviders` is kept whatever the payload holds, so a key
    /// is added only by `upload_confirm_provider` after the user confirmed in its dialog, and
    /// stale settings from another window cannot erase one.
    pub fn save_from_page(&self, mut settings: Settings) -> std::io::Result<(Settings, SaveNotes)> {
        let mut current = self.lock();
        settings.upload.confirmed_providers = current.upload.confirmed_providers.clone();
        self.write(&mut current, settings)
    }

    /// Changes the current settings in place and saves them, under one lock, so a change made
    /// from Rust (a consent, an import) cannot undo a page's save that landed in between.
    pub fn update(&self, change: impl FnOnce(&mut Settings)) -> std::io::Result<(Settings, SaveNotes)> {
        let mut current = self.lock();
        let mut settings = current.clone();
        change(&mut settings);
        self.write(&mut current, settings)
    }

    /// Forgets every consent recorded for `provider`, whatever destination it named. Taking
    /// consent back only ever asks the user again, so the settings page may do it.
    pub fn revoke_consent(&self, provider: &str) -> std::io::Result<Settings> {
        let destination = format!("{provider}@");
        self.update(|settings| {
            settings.upload.confirmed_providers.retain(|key| key != provider && !key.starts_with(&destination))
        })
        .map(|(saved, _)| saved)
    }

    fn lock(&self) -> std::sync::RwLockWriteGuard<'_, Settings> {
        self.current.write().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn write(&self, current: &mut Settings, mut settings: Settings) -> std::io::Result<(Settings, SaveNotes)> {
        settings.version = SETTINGS_VERSION;
        let cleared_shortcuts = settings.shortcuts.resolve_clashes(Some(&current.shortcuts));
        strip_secrets(&mut settings, &self.secrets);
        let bytes = serde_json::to_vec_pretty(&settings).map_err(std::io::Error::other)?;
        write_atomically(&self.path, &bytes)?;
        *current = settings.clone();
        Ok((settings, SaveNotes { cleared_shortcuts }))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A settings file from another version: unknown fields, a missing section, values that no
    /// longer fit. Every choice that still fits survives, and the unknown fields are written back,
    /// so neither an older nor a newer Glimpsfy erases what the user set.
    #[test]
    fn a_file_with_unknown_missing_and_invalid_fields_keeps_every_valid_choice() {
        let file = json!({
            "version": 7,
            "fromTheFuture": { "enabled": true, "level": 3 },
            "shortcuts": { "captureArea": "Alt+Shift+CommandOrControl+9", "pinLast": "Shift+CommandOrControl+P" },
            "output": {
                "saveFolder": "/tmp/captures",
                "fileNameTemplate": "Shot {date}",
                "quality": "very high",
                "format": "avif",
                "copyAfterSave": true,
                "newOutputOption": [1, 2]
            },
            "recording": { "codec": "hevc", "fps": "sixty", "gifWidth": null },
            "upload": { "providers": { "s3": { "bucket": "team" } }, "defaultProvider": 42 }
        });

        let settings: Settings = load_lenient(file);
        let defaults = Settings::default();

        // Valid choices survive.
        assert_eq!(settings.shortcuts.capture_area.as_deref(), Some("Alt+Shift+CommandOrControl+9"));
        assert_eq!(settings.output.save_folder, PathBuf::from("/tmp/captures"));
        assert_eq!(settings.output.file_name_template, "Shot {date}");
        assert!(settings.output.copy_after_save);
        assert_eq!(settings.recording.codec, VideoCodec::Hevc);
        assert_eq!(settings.recording.gif_width, None);
        assert_eq!(settings.upload.providers["s3"], json!({ "bucket": "team" }));

        // Values that no longer fit fall back to their defaults alone.
        assert_eq!(settings.output.quality, defaults.output.quality);
        assert_eq!(settings.output.format, defaults.output.format);
        assert_eq!(settings.recording.fps, defaults.recording.fps);
        assert_eq!(settings.upload.default_provider, None);

        // A missing section is the default section; the other shortcuts keep their defaults.
        assert_eq!(settings.general, defaults.general);
        assert_eq!(settings.shortcuts.capture_window, defaults.shortcuts.capture_window);

        // Unknown fields are kept and written back.
        let written = serde_json::to_value(&settings).unwrap();
        assert_eq!(written["fromTheFuture"], json!({ "enabled": true, "level": 3 }));
        assert_eq!(written["shortcuts"]["pinLast"], json!("Shift+CommandOrControl+P"));
        assert_eq!(written["output"]["newOutputOption"], json!([1, 2]));
    }

    /// A field that another version wrote as an object where this one keeps a plain value takes
    /// its default alone: the fields read after it (shortcuts, upload providers, consents) survive
    /// the load and the next save, instead of the whole file falling back to the defaults.
    #[test]
    fn an_object_where_a_plain_value_belongs_costs_only_that_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{ "general": { "language": { "code": "tr" }, "shutterSound": false, "firstRunDone": true },
                  "shortcuts": { "captureArea": "Alt+Shift+CommandOrControl+9" },
                  "upload": { "defaultProvider": "custom",
                              "confirmedProviders": ["custom@share.example.dev"],
                              "providers": { "custom": { "requestUrl": "https://share.example.dev/up" } } } }"#,
        )
        .unwrap();

        let check = |settings: &Settings| {
            assert_eq!(settings.general.language, Language::System);
            assert!(!settings.general.shutter_sound);
            assert!(settings.general.first_run_done);
            assert_eq!(settings.shortcuts.capture_area.as_deref(), Some("Alt+Shift+CommandOrControl+9"));
            assert_eq!(settings.upload.default_provider.as_deref(), Some("custom"));
            assert_eq!(settings.upload.confirmed_providers, vec!["custom@share.example.dev".to_string()]);
            assert_eq!(settings.upload.providers["custom"], json!({ "requestUrl": "https://share.example.dev/up" }));
        };
        let store = SettingsStore::open_with(path.clone(), SecretFields::new());
        check(&store.get());
        store.set(store.get()).unwrap();
        check(&SettingsStore::open_with(path, SecretFields::new()).get());
    }

    /// The saved sizes are a list the user built by hand. One entry that this version cannot read
    /// (edited by hand, or written by a newer Glimpsfy) costs neither the list nor itself: every
    /// entry, the unreadable ones and the unknown fields of the readable ones, survives a load and
    /// the next save in its place.
    #[test]
    fn an_unreadable_saved_size_costs_neither_the_list_nor_itself() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let sizes = json!([
            { "name": "Blog cover", "width": 1200, "height": 630 },
            { "name": "Banner", "width": "wide", "height": 90 },
            { "name": "Story", "width": 1080, "height": 1920, "anchor": "center" },
            42,
            { "name": "Poster", "width": 1000, "height": 500 }
        ]);
        fs::write(
            &path,
            serde_json::to_vec(&json!({ "general": { "savedSizes": sizes, "shutterSound": false } })).unwrap(),
        )
        .unwrap();

        let store = SettingsStore::open_with(path.clone(), SecretFields::new());
        let loaded = store.get();
        assert!(!loaded.general.shutter_sound);
        let known: Vec<(&str, u32, u32)> = loaded
            .general
            .saved_sizes
            .iter()
            .filter_map(|entry| match entry {
                Kept::Known(size) => Some((size.name.as_str(), size.width, size.height)),
                Kept::Other(_) => None,
            })
            .collect();
        assert_eq!(known, vec![("Blog cover", 1200, 630), ("Story", 1080, 1920), ("Poster", 1000, 500)]);

        store.set(loaded).unwrap();
        let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["general"]["savedSizes"], sizes);
    }

    /// A beautify preset whose background a newer Glimpsfy added (a kind this version does not
    /// know) is not offered, and it survives a load and the next save in its place, as does a
    /// readable preset's unknown field; the presets the user made are not reset to the built-in
    /// ones.
    #[test]
    fn a_preset_with_an_unknown_background_survives_a_load_and_a_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let style = |background: Value| json!({ "padding": 40.0, "corner": 10.0, "shadow": "soft", "background": background, "frame": "none" });
        let presets = json!([
            { "id": "mine", "name": "Mine", "style": style(json!({ "kind": "solid", "color": "#112233" })), "pinned": true },
            { "id": "mesh", "name": "Mesh", "style": style(json!({ "kind": "mesh", "points": [[0, 0, "#ff0000"]] })) },
            { "id": "ocean", "name": null, "style": style(json!({ "kind": "gradient", "stops": ["#2563ff", "#2fc6c0"] })) }
        ]);
        let file = json!({ "beautify": { "auto": true, "preset": "mesh", "presets": presets } });
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();

        let store = SettingsStore::open_with(path.clone(), SecretFields::new());
        let loaded = store.get();
        let offered: Vec<&str> = read_presets(&loaded.beautify.presets).map(|p| p.id.as_str()).collect();
        assert_eq!(offered, ["mine", "ocean"]);
        // The automatic preset cannot be read, so a capture gets the first one that can.
        assert_eq!(loaded.beautify.automatic().and_then(|s| s.preset).as_deref(), Some("mine"));

        store.set(loaded).unwrap();
        let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["beautify"]["presets"], presets);
        assert_eq!(written["beautify"]["preset"], "mesh");
    }

    /// Saving replaces the file through a rename: the old file is never truncated in place, the
    /// new one is complete, and no temporary file is left behind. Changing one setting keeps every
    /// other one, the first-run mark and a newer version's fields included.
    #[test]
    fn settings_are_saved_by_replacing_the_file_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            br#"{ "general": { "shutterSound": false, "firstRunDone": true, "dockBadge": "never" },
                  "pinDefaults": { "opacity": 80 } }"#,
        )
        .unwrap();

        let store = SettingsStore::open(path.clone());
        assert!(!store.get().general.shutter_sound);
        assert!(store.get().general.first_run_done);

        #[cfg(unix)]
        let before = std::os::unix::fs::MetadataExt::ino(&fs::metadata(&path).unwrap());
        let mut changed = store.get();
        changed.output.copy_after_save = true;
        store.set(changed).unwrap();

        #[cfg(unix)]
        assert_ne!(
            std::os::unix::fs::MetadataExt::ino(&fs::metadata(&path).unwrap()),
            before,
            "the file was rewritten in place"
        );
        let reread = SettingsStore::open(path.clone()).get();
        assert!(reread.output.copy_after_save);
        assert!(!reread.general.shutter_sound);
        assert!(reread.general.first_run_done);
        let written: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(written["general"]["dockBadge"], json!("never"));
        assert_eq!(written["pinDefaults"], json!({ "opacity": 80 }));
        let names: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(names, vec![std::ffi::OsString::from("settings.json")]);
    }

    /// The key combinations in the saved file that more than one action holds.
    fn clashes_on_disk(path: &Path) -> Vec<String> {
        let written: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let keys: Vec<String> = written["shortcuts"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(|v| v.as_str().and_then(canonical_accelerator))
            .collect();
        let mut clashes: Vec<String> =
            keys.iter().filter(|k| keys.iter().filter(|o| o == k).count() > 1).cloned().collect();
        clashes.dedup();
        clashes
    }

    /// Two actions never end up with the same keys in `settings.json`, whichever path sets them:
    /// a recorder save that repeats another row's keys (in another spelling), Move here, Restore
    /// defaults over a customised file, and a file edited by hand or imported (a newer version's
    /// action included). The action that already held the keys keeps them.
    #[test]
    fn no_two_actions_are_ever_saved_with_the_same_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let store = SettingsStore::open_with(path.clone(), SecretFields::new());
        store.set(Settings::default()).unwrap();
        assert!(clashes_on_disk(&path).is_empty(), "the defaults clash");

        // The recorder sends Capture display with Capture area's keys, spelled another way.
        let mut sent = store.get();
        sent.shortcuts.capture_display = Some("shift+cmdorctrl+Digit2".into());
        let (saved, notes) = store.save(sent).unwrap();
        assert!(clashes_on_disk(&path).is_empty());
        assert_eq!(saved.shortcuts.capture_area.as_deref(), Some("Shift+CommandOrControl+2"));
        assert_eq!(saved.shortcuts.capture_display, None);
        assert_eq!(notes.cleared_shortcuts.len(), 1);
        assert_eq!(notes.cleared_shortcuts[0].kept_by, "captureArea");

        // Move here: the recorder empties the other row first, so the keys move.
        let mut moved = store.get();
        moved.shortcuts.capture_area = None;
        moved.shortcuts.capture_display = Some("Shift+CommandOrControl+2".into());
        let (saved, notes) = store.save(moved).unwrap();
        assert!(clashes_on_disk(&path).is_empty());
        assert_eq!(saved.shortcuts.capture_display.as_deref(), Some("Shift+CommandOrControl+2"));
        assert!(notes.cleared_shortcuts.is_empty());

        // Restore defaults over a customised file whose rows hold the default keys elsewhere.
        let mut custom = store.get();
        custom.shortcuts.open_history = Some("Alt+Shift+CommandOrControl+2".into());
        store.set(custom).unwrap();
        store.set(Settings::default()).unwrap();
        assert!(clashes_on_disk(&path).is_empty());

        // A file edited by hand: three spellings of one combination, one of them a newer
        // version's action, and a pair of rows that share another.
        fs::write(
            &path,
            br#"{ "shortcuts": {
                    "captureArea": "CommandOrControl+Shift+2",
                    "openHistory": "Shift+CommandOrControl+2",
                    "pinLast": "shift+cmdorctrl+2",
                    "copyText": "Alt+Shift+CommandOrControl+R"
                } }"#,
        )
        .unwrap();
        let reopened = SettingsStore::open_with(path.clone(), SecretFields::new());
        assert!(clashes_on_disk(&path).is_empty(), "the hand-edited file kept its clash on disk");
        let loaded = reopened.get();
        assert_eq!(loaded.shortcuts.capture_area.as_deref(), Some("CommandOrControl+Shift+2"));
        assert_eq!(loaded.shortcuts.open_history, None);
        assert_eq!(loaded.shortcuts.extra["pinLast"], Value::Null);
        assert_eq!(loaded.shortcuts.copy_text.as_deref(), Some("Alt+Shift+CommandOrControl+R"));
        assert_eq!(loaded.shortcuts.record_screen, None);
        reopened.set(loaded).unwrap();
        assert!(clashes_on_disk(&path).is_empty());

        // A file from before the ruler whose history row holds the ruler's new default keys:
        // the user's row keeps them and the ruler starts without a key.
        fs::write(&path, br#"{ "shortcuts": { "openHistory": "Alt+Shift+CommandOrControl+G" } }"#).unwrap();
        let older = SettingsStore::open_with(path.clone(), SecretFields::new()).get();
        assert!(clashes_on_disk(&path).is_empty(), "the ruler's default clashed with a key on disk");
        assert_eq!(older.shortcuts.open_history.as_deref(), Some("Alt+Shift+CommandOrControl+G"));
        assert_eq!(older.shortcuts.measure_screen, None);

        // The same for the colour picker's ⌥⇧⌘I, held by the ruler in a file from before it.
        fs::write(&path, br#"{ "shortcuts": { "measureScreen": "Alt+Shift+CommandOrControl+I" } }"#).unwrap();
        let older = SettingsStore::open_with(path.clone(), SecretFields::new()).get();
        assert!(clashes_on_disk(&path).is_empty(), "the colour picker's default clashed with a key on disk");
        assert_eq!(older.shortcuts.measure_screen.as_deref(), Some("Alt+Shift+CommandOrControl+I"));
        assert_eq!(older.shortcuts.pick_color, None);

        // And for Draw on screen's ⌥⇧⌘D, held by the history row in a file from before it: the
        // new default never silently takes the user's key.
        fs::write(&path, br#"{ "shortcuts": { "openHistory": "Alt+Shift+CommandOrControl+D" } }"#).unwrap();
        let older = SettingsStore::open_with(path.clone(), SecretFields::new()).get();
        assert!(clashes_on_disk(&path).is_empty(), "Draw on screen's default clashed with a key on disk");
        assert_eq!(older.shortcuts.open_history.as_deref(), Some("Alt+Shift+CommandOrControl+D"));
        assert_eq!(older.shortcuts.draw_on_screen, None);
        assert_eq!(older.shortcuts.spotlight.as_deref(), Some("Alt+Shift+CommandOrControl+L"));

        // And for the interval capture's ⌥⇧⌘K, held by the colour picker in a file from before it.
        fs::write(&path, br#"{ "shortcuts": { "pickColor": "Alt+Shift+CommandOrControl+K" } }"#).unwrap();
        let older = SettingsStore::open_with(path.clone(), SecretFields::new()).get();
        assert!(clashes_on_disk(&path).is_empty(), "the interval capture's default clashed with a key on disk");
        assert_eq!(older.shortcuts.pick_color.as_deref(), Some("Alt+Shift+CommandOrControl+K"));
        assert_eq!(older.shortcuts.interval_capture, None);

        // And for the scrolling capture's ⌥⇧⌘S (macOS only), held by the history row in a file
        // from before it.
        fs::write(&path, br#"{ "shortcuts": { "openHistory": "Alt+Shift+CommandOrControl+S" } }"#).unwrap();
        let older = SettingsStore::open_with(path.clone(), SecretFields::new()).get();
        assert!(clashes_on_disk(&path).is_empty(), "the scrolling capture's default clashed with a key on disk");
        assert_eq!(older.shortcuts.open_history.as_deref(), Some("Alt+Shift+CommandOrControl+S"));
        assert_eq!(older.shortcuts.scroll_capture, None);
    }

    /// A secret typed into a provider's form never reaches `settings.json`: not when a form
    /// submit sends it along with the provider's other values, and not when a hand-edited file
    /// holds one (the file is rewritten without it at once). The provider's other values stay.
    #[test]
    fn secrets_from_a_provider_form_never_reach_the_settings_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let secrets: SecretFields = [
            ("imgbb".to_string(), BTreeMap::from([("apiKey".to_string(), SecretKind::Whole)])),
            ("custom".to_string(), BTreeMap::from([("headers".to_string(), SecretKind::PairValues)])),
        ]
        .into_iter()
        .collect();
        let store = SettingsStore::open_with(path.clone(), secrets.clone());

        let mut submitted = store.get();
        submitted
            .upload
            .providers
            .insert("imgbb".into(), json!({ "apiKey": "6d1f0c9a2b7e4f3a-typed-secret", "retention": "days7" }));
        submitted.upload.providers.insert(
            "custom".into(),
            json!({ "url": "https://share.example.dev/api/upload",
                    "headers": [{ "name": "Authorization", "value": "Bearer zp-header-secret" }, "Format"] }),
        );
        let (saved, _) = store.save(submitted).unwrap();
        let file = fs::read_to_string(&path).unwrap();
        assert!(!file.contains("typed-secret"), "the secret reached the file");
        assert!(!file.contains("header-secret"), "a header value reached the file");
        assert!(file.contains("days7"));
        assert_eq!(saved.upload.providers["imgbb"], json!({ "retention": "days7" }));
        assert_eq!(saved.upload.providers["custom"]["headers"], json!(["Authorization", "Format"]));
        assert!(!serde_json::to_string(&store.get()).unwrap().contains("secret"));

        let mut edited: Value = serde_json::from_str(&file).unwrap();
        edited["upload"]["providers"]["imgbb"]["apiKey"] = json!("pasted-by-hand-secret");
        fs::write(&path, serde_json::to_vec(&edited).unwrap()).unwrap();
        let reopened = SettingsStore::open_with(path.clone(), secrets);
        assert!(!fs::read_to_string(&path).unwrap().contains("pasted-by-hand-secret"));
        assert_eq!(reopened.get().upload.providers["imgbb"], json!({ "retention": "days7" }));
    }

    /// The consent list on disk after a save.
    fn consents_on_disk(path: &Path) -> Value {
        let written: Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        written["upload"]["confirmedProviders"].clone()
    }

    /// A page cannot give or take upload consent through `settings_set`: a payload that adds a
    /// key (skipping the confirmation dialog) or drops one (stale settings from another window)
    /// leaves the stored list as it was, while the rest of the same save is kept. Only the
    /// confirmation adds a key, and only Forget confirmation removes them.
    #[test]
    fn a_page_save_never_changes_the_upload_consents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let store = SettingsStore::open_with(path.clone(), SecretFields::new());
        store.update(|s| s.upload.confirmed_providers.push("custom@share.example.dev".into())).unwrap();
        let stale = store.get();
        store.update(|s| s.upload.confirmed_providers.push("litterbox".into())).unwrap();
        let confirmed = json!(["custom@share.example.dev", "litterbox"]);

        let mut added = store.get();
        added.upload.confirmed_providers.push("s3@s3.eu-central-1.amazonaws.com/team-captures".into());
        added.output.copy_after_save = true;
        let (saved, _) = store.save_from_page(added).unwrap();
        assert_eq!(consents_on_disk(&path), confirmed, "a page added a consent");
        assert_eq!(json!(saved.upload.confirmed_providers), confirmed);
        assert!(saved.output.copy_after_save);

        let mut removed = stale;
        removed.upload.confirmed_providers.clear();
        removed.general.shutter_sound = false;
        store.save_from_page(removed).unwrap();
        assert_eq!(consents_on_disk(&path), confirmed, "a page removed a consent");
        assert_eq!(json!(store.get().upload.confirmed_providers), confirmed);
        assert!(!store.get().general.shutter_sound);

        store.revoke_consent("custom").unwrap();
        assert_eq!(consents_on_disk(&path), json!(["litterbox"]));
    }
}
