//! Send to app and Copy path (A2): the file a capture's path names, the shell quoting of that
//! path, and the `glimpsfy-send` thread, which brings an app forward and presses ⌘V in it.
//!
//! - **The file.** A path always names a file that exists: the saved file when the capture was
//!   saved; otherwise the history's own copy (`history/captures/<id>.png`) while the history is on;
//!   otherwise a PNG written into output's inbox under the file name template, which
//!   `output::sweep_inbox` deletes a day later. An open capture's pixels go through
//!   `output::finish` first, so the file holds the pixels a copy would.
//! - **Quoting.** The clipboard gets the absolute path, bare when no character means anything to
//!   a shell, in double quotes otherwise, and in single quotes when it holds `!` or a line break
//!   ([`quote_path`]). A name taken from a window title therefore never runs as a command.
//! - **The paste.** The clipboard is written before the app is touched. The app comes forward
//!   (started first when it is not running), its first ordinary window is waited for, 10 seconds
//!   at most, and ⌘V is pressed in it; Return never is. Without Accessibility the send stops once
//!   the app is in front, and the toast asks the user to press ⌘V. The macOS side is
//!   `macos::send`; elsewhere Send to app answers `not_implemented` (`Capabilities::send_to_app`).
//! - **Thread.** `glimpsfy-send` runs one send at a time, in order; between sends it sweeps the
//!   inbox, at start and every hour, whether the history is on or not.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use glimpsfy_capture::{DisplayId, Frame, ImageEncoding, PngEffort};
use glimpsfy_history::{EntryId, EntryKind};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::capture::CloseReason;
use crate::commands::error::{CommandError, CommandResult};
use crate::commands::history::{self, CaptureHandoff, Clipboard};
use crate::commands::send::events::{SendState, STATE};
use crate::commands::send::{PickedApp, SendPhase, SendSource, SendTargetInfo, SendTargets, Sent, SentToast};
use crate::output::{self, Origin, OutputAction, Recent, Report};
use crate::settings::{SendPaste, SendTarget};
use crate::windows::{self, Toast, ToastKind};
use crate::{trace, AppState};

#[cfg(target_os = "macos")]
use crate::macos::send as platform;

/// How often the inbox is swept between sends.
const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);
/// How long a toast's Send waits for the history to record the capture the toast reports.
const HISTORY_WAIT: Duration = Duration::from_secs(3);

/// Terminals take the path, since they cannot paste an image.
#[cfg(target_os = "macos")]
const TERMINALS: &[&str] = &[
    "com.apple.Terminal",
    "com.googlecode.iterm2",
    "dev.warp.Warp-Stable",
    "dev.warp.Warp",
    "net.kovidgoyal.kitty",
    "io.alacritty",
    "org.alacritty",
    "com.mitchellh.ghostty",
    "co.zeit.hyper",
    "com.github.wez.wezterm",
];

// ---------------------------------------------------------------------------------- quoting

/// A path as a shell reads it back whole: bare when every character is plain, in double quotes
/// with `"`, `\`, `$` and the backtick escaped when not, and in single quotes when it holds `!` or
/// a line break, because zsh expands `!` inside double quotes. Inside quotes `~` stays a `~`.
#[cfg(not(windows))]
pub fn quote_path(path: &str) -> String {
    let plain = |c: char| c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '+' | ',' | ':' | '@');
    if !path.is_empty() && path.chars().all(plain) {
        return path.to_string();
    }
    if path.contains(['!', '\n', '\r']) {
        return format!("'{}'", path.replace('\'', r"'\''"));
    }
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for c in path.chars() {
        if matches!(c, '"' | '\\' | '$' | '`') {
            quoted.push('\\');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

#[cfg(windows)]
pub fn quote_path(path: &str) -> String {
    quote_windows_path(path)
}

/// A path as PowerShell and cmd read it back whole: bare when it holds only letters, digits and
/// `\:._-`, otherwise in double quotes with a backtick before `$`, the backtick and the
/// typographic double quotes (`“ ” „`). PowerShell expands `$(…)` and `$name` inside double
/// quotes and ends the string at any of its quote marks, and a window title in the file name can
/// hold all of them. cmd keeps the backtick as it is, so a path with one of those characters
/// reads wrong there, but nothing in it runs: cmd ends its quotes only at `"`, which a Windows
/// file name cannot hold, so `&`, `|` and `%` stay inside them. Single quotes would protect
/// nothing in cmd.
#[cfg(any(windows, test))]
fn quote_windows_path(path: &str) -> String {
    let plain = |c: char| c.is_alphanumeric() || matches!(c, '\\' | ':' | '.' | '_' | '-');
    if !path.is_empty() && path.chars().all(plain) {
        return path.to_string();
    }
    let mut quoted = String::with_capacity(path.len() + 2);
    quoted.push('"');
    for c in path.chars() {
        if matches!(c, '$' | '`' | '\u{201C}' | '\u{201D}' | '\u{201E}') {
            quoted.push('`');
        }
        quoted.push(c);
    }
    quoted.push('"');
    quoted
}

// ----------------------------------------------------------------------------------- the file

/// What a send starts from once an open capture's pixels are taken.
enum Item {
    /// An open capture's image: its selection, or the overlay's rendering with drawings. The
    /// origin is boxed, since it is far larger than a source.
    Pixels(Frame, Box<Origin>),
    /// A history entry or a capture a toast reports.
    Stored(SendSource),
}

fn item_of(app: &AppHandle, source: SendSource) -> CommandResult<Item> {
    match source {
        SendSource::Selection { selection } => {
            let (frame, origin) = output::take(app, &selection)?;
            Ok(Item::Pixels(frame, Box::new(origin)))
        }
        other => Ok(Item::Stored(other)),
    }
}

/// The file of a send or a Copy path, with what its toast shows.
struct Resolved {
    /// `None` for a text copy, which is sent as its text.
    file: Option<PathBuf>,
    text: Option<String>,
    kind: EntryKind,
    thumbnail: Option<String>,
    display: Option<DisplayId>,
    /// What Try again sends.
    retry: Option<SendSource>,
    entry_id: Option<EntryId>,
}

fn resolve(app: &AppHandle, item: Item) -> CommandResult<Resolved> {
    match item {
        Item::Pixels(frame, origin) => {
            let (frame, origin) = output::finish(app, vec![(frame, *origin)], CloseReason::Copied)
                .pop()
                .ok_or_else(|| CommandError::internal("no image to send"))?;
            let thumbnail = output::thumbnail(&frame);
            let (path, entry_id) = file_for_frame(app, &frame, &origin, thumbnail.clone())?;
            Ok(Resolved {
                file: Some(path),
                text: None,
                kind: EntryKind::Screenshot,
                thumbnail,
                display: origin.display.as_ref().map(|d| d.id),
                retry: (origin.capture_id.0 != 0).then_some(SendSource::Capture { capture_id: origin.capture_id }),
                entry_id,
            })
        }
        Item::Stored(SendSource::Entry { entry_id }) => {
            let entry = history::store(app)?.get(&entry_id)?;
            let retry = Some(SendSource::Entry { entry_id: entry_id.clone() });
            if entry.kind == EntryKind::TextCopy {
                let text = entry.text.filter(|t| !t.is_empty());
                let text = text.ok_or_else(|| CommandError::not_found("the text of this text copy"))?;
                return Ok(Resolved {
                    file: None,
                    text: Some(text),
                    kind: entry.kind,
                    thumbnail: None,
                    display: None,
                    retry,
                    entry_id: Some(entry_id),
                });
            }
            let file = entry
                .media_path()
                .filter(|p| p.is_file())
                .map(Path::to_path_buf)
                .ok_or_else(|| CommandError::not_found("the capture's file"))?;
            Ok(Resolved {
                file: Some(file),
                text: None,
                kind: entry.kind,
                thumbnail: None,
                display: None,
                retry,
                entry_id: Some(entry_id),
            })
        }
        Item::Stored(SendSource::Capture { capture_id }) => {
            let recent = output::recent(capture_id)
                .ok_or_else(|| CommandError::not_found(format!("capture #{}", capture_id.0)))?;
            let (file, entry_id) = file_of_recent(app, &recent)?;
            output::remember_file(capture_id, &file);
            Ok(Resolved {
                file: Some(file),
                text: None,
                kind: EntryKind::Screenshot,
                thumbnail: recent.thumbnail.clone(),
                display: recent.origin.display.as_ref().map(|d| d.id),
                retry: Some(SendSource::Capture { capture_id }),
                entry_id,
            })
        }
        Item::Stored(SendSource::Selection { .. }) => Err(CommandError::internal("a selection is sent as pixels")),
    }
}

/// The file of an image that was just taken: the history's own copy while the history is on,
/// which records the capture now, or a PNG in the inbox. The delivery is remembered with the file,
/// so the toast's Try again finds the same one.
fn file_for_frame(
    app: &AppHandle,
    frame: &Frame,
    origin: &Origin,
    thumbnail: Option<String>,
) -> CommandResult<(PathBuf, Option<EntryId>)> {
    let png = frame.encode(ImageEncoding::Png(PngEffort::Balanced))?;
    let size = (frame.size.width, frame.size.height);
    let mut found = None;
    if history::records(app) {
        let handoff = CaptureHandoff {
            kind: Some(EntryKind::Screenshot),
            capture_id: Some(origin.capture_id.0),
            width: Some(size.0),
            height: Some(size.1),
            scale: Some(frame.scale.0),
            bytes: Some(png.len() as u64),
            app_name: origin.app_name.clone(),
            bundle_id: origin.bundle_id.clone(),
            window_title: origin.window_title.clone(),
            display_id: origin.display.as_ref().map(|d| d.id.0),
            display_name: origin.display.as_ref().map(|d| d.name.clone()),
            created_at_ms: origin.taken_at.duration_since(UNIX_EPOCH).ok().map(|d| d.as_millis() as i64),
            png: Some(png.clone()),
            ..Default::default()
        };
        match history::record_capture(app, handoff) {
            Ok(Some(entry)) => {
                found = entry.media_path().map(|path| (path.to_path_buf(), Some(entry.id.clone())));
            }
            Ok(None) => {}
            Err(error) => trace::log(format!("[glimpsfy] send: the capture was not recorded: {}", error.message)),
        }
    }
    let (path, entry_id) = match found {
        Some(found) => found,
        None => (output::write_inbox_named(app, origin, size, &png)?, None),
    };
    output::remember(Recent {
        capture_id: origin.capture_id,
        origin: origin.clone(),
        size,
        saved_path: None,
        png: None,
        file: Some(path.clone()),
        thumbnail,
    });
    Ok((path, entry_id))
}

/// The file of a delivery a toast reports: the one a send used before, the saved file, the
/// history's copy (waiting a moment for the history to record it), or its PNG written into the
/// inbox.
fn file_of_recent(app: &AppHandle, recent: &Recent) -> CommandResult<(PathBuf, Option<EntryId>)> {
    if let Some(file) = recent.file.as_ref().filter(|f| f.is_file()) {
        return Ok((file.clone(), None));
    }
    if let Some(saved) = recent.saved_path.as_ref().filter(|f| f.is_file()) {
        return Ok((saved.clone(), None));
    }
    if let Some(png) = &recent.png {
        return Ok((output::write_inbox_named(app, &recent.origin, recent.size, png)?, None));
    }
    if history::records(app) {
        if let Some(id) = history::entry_of_capture(recent.capture_id.0, HISTORY_WAIT) {
            let entry = history::store(app)?.get(&id)?;
            if let Some(file) = entry.media_path().filter(|p| p.is_file()) {
                return Ok((file.to_path_buf(), Some(id)));
            }
        }
    }
    Err(CommandError::not_found("the capture's file"))
}

/// Puts a resolved source on the clipboard the way the target takes it. A text copy goes as its
/// text to every target, and a recording as its file to an image target.
fn put(paste: SendPaste, resolved: &Resolved) -> CommandResult<()> {
    if let Some(text) = &resolved.text {
        return output::copy_text(text);
    }
    let path = resolved.file.as_deref().ok_or_else(|| CommandError::not_found("the capture's file"))?;
    match paste {
        SendPaste::Path => output::copy_text(&quote_path(&path.to_string_lossy())),
        SendPaste::File => history::copy_to_clipboard(Clipboard::File(path)),
        SendPaste::Image => {
            let extension = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
            match (resolved.kind, extension.as_str()) {
                (EntryKind::Screenshot, "png") => history::copy_to_clipboard(Clipboard::Png(&std::fs::read(path)?)),
                (EntryKind::Screenshot, "jpg" | "jpeg") => {
                    history::copy_to_clipboard(Clipboard::Jpeg(&std::fs::read(path)?))
                }
                _ => history::copy_to_clipboard(Clipboard::File(path)),
            }
        }
    }
}

// --------------------------------------------------------------------------------- Copy path

/// Copy path of a source (`send_copy_path`). An entry's path is copied from a window that says so
/// itself (the popover, the history), so only the overlay's and the toast's get a toast.
pub fn copy_path(app: &AppHandle, source: SendSource) -> CommandResult<Sent> {
    let entry = matches!(source, SendSource::Entry { .. });
    if let SendSource::Entry { entry_id } = &source {
        if history::store(app)?.get(entry_id)?.kind == EntryKind::TextCopy {
            return Err(CommandError::invalid("A text copy has no file, so it has no path to copy."));
        }
    }
    let item = item_of(app, source)?;
    copy_path_of(app, item, !entry)
}

fn copy_path_of(app: &AppHandle, item: Item, toasted: bool) -> CommandResult<Sent> {
    let resolved = resolve(app, item)?;
    let path = resolved
        .file
        .ok_or_else(|| CommandError::invalid("A text copy has no file, so it has no path to copy."))?
        .to_string_lossy()
        .into_owned();
    output::copy_text(&quote_path(&path))?;
    let temporary = output::inbox_dir(app).is_some_and(|inbox| Path::new(&path).starts_with(inbox));
    if toasted {
        let mut toast = Toast::new(ToastKind::PathCopied);
        toast.path = Some(path.clone());
        toast.thumbnail = resolved.thumbnail;
        toast.temporary = temporary;
        windows::toast(app, resolved.display, toast);
    }
    Ok(Sent { send_id: None, path: Some(path), temporary })
}

// --------------------------------------------------------------------------------------- send

/// The target of Settings › Output named `target_id`, with the name the installed app goes by.
/// Checked before anything closes, so a selection stays open when the target cannot be sent to.
pub fn target(app: &AppHandle, target_id: &str) -> CommandResult<(SendTarget, String)> {
    if !cfg!(target_os = "macos") {
        return Err(CommandError::not_implemented("Send to app"));
    }
    let settings = app.state::<AppState>().settings.get();
    let target = settings
        .output
        .send_targets
        .iter()
        .find(|t| t.bundle_id == target_id)
        .cloned()
        .ok_or_else(|| CommandError::not_found(format!("the send target {target_id}")))?;
    let name = installed_name(&target.bundle_id)
        .ok_or_else(|| CommandError::not_found(format!("{} is not on this Mac", target.name)))?;
    Ok((target, name))
}

/// Send to app of a source (`send_to_app`).
pub fn send(app: &AppHandle, source: SendSource, target_id: &str) -> CommandResult<Sent> {
    let target = target(app, target_id)?;
    let item = item_of(app, source)?;
    send_item(app, item, target)
}

/// The overlay's rendering with drawings (`send_rendered`): sent to `target`, or its path copied
/// when there is none.
pub fn send_pixels(
    app: &AppHandle,
    frame: Frame,
    origin: Origin,
    target: Option<(SendTarget, String)>,
) -> CommandResult<Sent> {
    match target {
        Some(target) => send_item(app, Item::Pixels(frame, Box::new(origin)), target),
        None => copy_path_of(app, Item::Pixels(frame, Box::new(origin)), true),
    }
}

fn send_item(app: &AppHandle, item: Item, (target, name): (SendTarget, String)) -> CommandResult<Sent> {
    let seed = match item {
        // An open capture's image goes the way of Copy, without its toast: the history records it
        // as a copy, and the send's own toast follows.
        Item::Pixels(frame, origin) if target.paste == SendPaste::Image => {
            let id = origin.capture_id;
            let display = origin.display.as_ref().map(|d| d.id);
            output::deliver_reported(app, frame, *origin, OutputAction::Copy, Report { toast: false, handoff: true })?;
            Seed {
                thumbnail: output::recent(id).and_then(|r| r.thumbnail),
                display,
                retry: (id.0 != 0).then_some(SendSource::Capture { capture_id: id }),
                entry_id: None,
            }
        }
        item => {
            let resolved = resolve(app, item)?;
            put(target.paste, &resolved)?;
            Seed {
                thumbnail: resolved.thumbnail,
                display: resolved.display,
                retry: resolved.retry,
                entry_id: resolved.entry_id,
            }
        }
    };
    *LAST.lock().unwrap_or_else(|p| p.into_inner()) = Some(target.bundle_id.clone());
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let send_id = NEXT.fetch_add(1, Ordering::SeqCst);
    let cancel = Arc::new(AtomicBool::new(false));
    CANCELS.lock().unwrap_or_else(|p| p.into_inner()).push((send_id, cancel.clone()));
    let job = Job { send_id, bundle_id: target.bundle_id, name, seed, cancel };
    let queued = JOBS.get().is_some_and(|jobs| jobs.lock().unwrap_or_else(|p| p.into_inner()).send(job).is_ok());
    if !queued {
        return Err(CommandError::internal("the send thread is not running"));
    }
    Ok(Sent { send_id: Some(send_id), ..Sent::default() })
}

/// Stops the wait of a send for its app.
pub fn cancel(send_id: u64) {
    if let Some((_, flag)) = CANCELS.lock().unwrap_or_else(|p| p.into_inner()).iter().find(|(id, _)| *id == send_id) {
        flag.store(true, Ordering::SeqCst);
    }
}

// ------------------------------------------------------------------------------- the targets

/// The target sent to last since Glimpsfy started; it is never saved.
static LAST: Mutex<Option<String>> = Mutex::new(None);

/// The send list (`send_list_targets`); on the main thread.
pub fn list_targets(app: &AppHandle) -> SendTargets {
    let settings = app.state::<AppState>().settings.get();
    let targets: Vec<SendTargetInfo> = settings
        .output
        .send_targets
        .iter()
        .map(|target| {
            let found = installed(&target.bundle_id);
            SendTargetInfo {
                bundle_id: target.bundle_id.clone(),
                name: found.as_ref().map_or_else(|| target.name.clone(), |(_, name)| name.clone()),
                paste: target.paste,
                installed: found.is_some(),
                running: running(&target.bundle_id),
                icon: found.as_ref().and_then(|(path, _)| icon(path)),
            }
        })
        .collect();
    let last = LAST.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let last = last.filter(|id| targets.iter().any(|t| &t.bundle_id == id && t.installed));
    SendTargets { targets, last, can_paste: can_paste(app) }
}

/// The app the user picks (`send_pick_app`).
pub fn pick_app(app: &AppHandle) -> CommandResult<Option<PickedApp>> {
    #[cfg(target_os = "macos")]
    {
        use tauri_plugin_dialog::DialogExt;
        if app.try_state::<tauri_plugin_dialog::Dialog<tauri::Wry>>().is_none() {
            return Err(CommandError::not_implemented("the file panel"));
        }
        output::bring_forward(app);
        let Some(picked) =
            app.dialog().file().set_directory("/Applications").add_filter("App", &["app"]).blocking_pick_file()
        else {
            return Ok(None);
        };
        let path = picked.into_path().map_err(|e| CommandError::invalid(e.to_string()))?;
        let (bundle_id, name) =
            platform::bundle_of(&path).ok_or_else(|| CommandError::invalid("the chosen file is not an app"))?;
        let paste = if TERMINALS.contains(&bundle_id.as_str()) { SendPaste::Path } else { SendPaste::Image };
        Ok(Some(PickedApp { icon: platform::icon(&path), bundle_id, name, paste }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err(CommandError::not_implemented("Send to app"))
    }
}

#[cfg(target_os = "macos")]
fn installed(bundle_id: &str) -> Option<(PathBuf, String)> {
    platform::installed(bundle_id)
}
#[cfg(not(target_os = "macos"))]
fn installed(_bundle_id: &str) -> Option<(PathBuf, String)> {
    None
}

fn installed_name(bundle_id: &str) -> Option<String> {
    installed(bundle_id).map(|(_, name)| name)
}

#[cfg(target_os = "macos")]
fn running(bundle_id: &str) -> bool {
    platform::running(bundle_id)
}
#[cfg(not(target_os = "macos"))]
fn running(_bundle_id: &str) -> bool {
    false
}

#[cfg(target_os = "macos")]
fn icon(path: &Path) -> Option<String> {
    platform::icon(path)
}
#[cfg(not(target_os = "macos"))]
fn icon(_path: &Path) -> Option<String> {
    None
}

fn can_paste(app: &AppHandle) -> bool {
    cfg!(target_os = "macos")
        && crate::permissions::status(app).accessibility == glimpsfy_capture::PermissionState::Granted
}

// -------------------------------------------------------------------------------- the thread

/// What a send's toast shows besides its phase.
struct Seed {
    thumbnail: Option<String>,
    display: Option<DisplayId>,
    retry: Option<SendSource>,
    entry_id: Option<EntryId>,
}

struct Job {
    send_id: u64,
    bundle_id: String,
    name: String,
    seed: Seed,
    // Only macOS's paste reads it; elsewhere a send has no step to cancel.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    cancel: Arc<AtomicBool>,
}

static JOBS: OnceLock<Mutex<Sender<Job>>> = OnceLock::new();
/// The cancel switch of each send that has not ended.
static CANCELS: Mutex<Vec<(u64, Arc<AtomicBool>)>> = Mutex::new(Vec::new());

/// Starts the `glimpsfy-send` thread, which sweeps the inbox at once. Once, at start.
pub fn setup(app: &AppHandle) {
    let (sender, receiver) = mpsc::channel::<Job>();
    if JOBS.set(Mutex::new(sender)).is_err() {
        return;
    }
    let app = app.clone();
    let _ = std::thread::Builder::new().name("glimpsfy-send".into()).spawn(move || {
        sweep(&app);
        // Wall-clock time, because a sleeping Mac stops the monotonic clock.
        let mut last_sweep = SystemTime::now();
        loop {
            match receiver.recv_timeout(SWEEP_EVERY) {
                Ok(job) => run(&app, job),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            if last_sweep.elapsed().map_or(true, |e| e >= SWEEP_EVERY) {
                sweep(&app);
                last_sweep = SystemTime::now();
            }
        }
    });
}

fn sweep(app: &AppHandle) {
    let Some(dir) = output::inbox_dir(app) else {
        return;
    };
    let removed = output::sweep_inbox(&dir, SystemTime::now());
    if !removed.is_empty() {
        trace::log(format!("[glimpsfy] history inbox: deleted {} day-old file(s)", removed.len()));
    }
}

fn run(app: &AppHandle, job: Job) {
    let mut toasted = false;
    let mut tell = |phase: SendPhase| {
        if !toasted {
            toasted = true;
            let mut toast = Toast::new(ToastKind::Sent);
            toast.thumbnail = job.seed.thumbnail.clone();
            toast.send = Some(SentToast {
                send_id: job.send_id,
                target_id: job.bundle_id.clone(),
                target_name: job.name.clone(),
                phase,
                retry: job.seed.retry.clone(),
                entry_id: job.seed.entry_id.clone(),
            });
            windows::toast(app, job.seed.display, toast);
        }
        let _ = app.emit(STATE, SendState { send_id: job.send_id, target_name: job.name.clone(), phase });
    };
    #[cfg(target_os = "macos")]
    let phase = platform::bring_forward_and_paste(app, &job.bundle_id, &job.cancel, can_paste(app), || {
        tell(SendPhase::Opening)
    });
    #[cfg(not(target_os = "macos"))]
    let phase = SendPhase::Failed;
    if phase == SendPhase::Failed {
        trace::log(format!("[glimpsfy] send: {} could not be brought forward", job.bundle_id));
    }
    tell(phase);
    CANCELS.lock().unwrap_or_else(|p| p.into_inner()).retain(|(id, _)| *id != job.send_id);
}

/// The Windows quoting is tested on every platform, since a path a terminal runs cannot be taken
/// back.
#[cfg(test)]
mod windows_quoting {
    use super::quote_windows_path;

    /// Reads a PowerShell double-quoted string the way PowerShell does, or `None` when something
    /// in it would not stay literal: a `$` that expands, a quote mark that ends the string early,
    /// or a backtick escape that stands for another character (`` `n ``).
    fn powershell_reads(quoted: &str) -> Option<String> {
        let inner = quoted.strip_prefix('"')?.strip_suffix('"')?;
        let mut out = String::new();
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            match c {
                '`' => {
                    let next = chars.next()?;
                    if matches!(next, '0' | 'a' | 'b' | 'e' | 'f' | 'n' | 'r' | 't' | 'u' | 'v') {
                        return None;
                    }
                    out.push(next);
                }
                '$' | '"' | '\u{201C}' | '\u{201D}' | '\u{201E}' => return None,
                other => out.push(other),
            }
        }
        Some(out)
    }

    #[test]
    fn a_windows_path_with_a_command_in_its_name_is_escaped() {
        assert_eq!(quote_windows_path(r"C:\a\Glimpsfy $(calc) `$x.png"), r#""C:\a\Glimpsfy `$(calc) ```$x.png""#);
        assert_eq!(quote_windows_path(r"C:\Users\deniz\Pictures\shot-1.png"), r"C:\Users\deniz\Pictures\shot-1.png");
    }

    /// PowerShell reads every quoted path back whole with nothing expanded, and cmd's quotes are
    /// never closed early, so `&`, `|` and `%` inside them run nothing.
    #[test]
    fn a_quoted_windows_path_reads_back_whole_in_powershell_and_cmd() {
        let names = [
            r"C:\Users\deniz\Pictures\Glimpsfy 2026-11-22 at 14.32.08.png",
            r"C:\a\$(iwr evil|iex).png",
            r"C:\a\${env:PATH} $HOME $$ $_ $?.png",
            r"C:\a\back`tick `n `$x.png",
            "C:\\a\\x\u{201D}; calc; \u{201C}y \u{201E}z.png",
            r"C:\a\amp & pipe %PATH% !x! ^caret (paren) {brace} @at.png",
            r"C:\a\it's ‘single’.png",
            r"C:\Müşteri\Ödeme sayfası — Safari.png",
        ];
        for name in names {
            let quoted = quote_windows_path(name);
            assert_eq!(powershell_reads(&quoted).as_deref(), Some(name), "PowerShell reads {quoted}");
            assert_eq!(quoted.matches('"').count(), 2, "cmd's quotes stay closed only at the ends: {quoted}");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A path that reaches Terminal through the clipboard is read back by the shell as the same
    /// path and runs nothing, whatever the file is called: a window title can name a capture
    /// `$(touch x)`, and a paste that ran it could not be undone.
    #[test]
    fn a_quoted_path_reads_back_whole_and_runs_nothing() {
        let dir = tempfile::tempdir().unwrap();
        // A file name holds no "/", so the command in it touches a file beside it: the shells run
        // in the temporary folder.
        let marker = dir.path().join("ran");
        let names = [
            "plain.png".to_string(),
            "Glimpsfy 2026-11-22 at 14.32.08.png".to_string(),
            "$(touch ran).png".to_string(),
            "`touch ran`.png".to_string(),
            "a$(touch ran)'b'.png".to_string(),
            "say \"hi\".png".to_string(),
            "back\\slash.png".to_string(),
            "it's.png".to_string(),
            "it's a \"bang\"!.png".to_string(),
            "wow!.png".to_string(),
            "two\nlines.png".to_string(),
            "~tilde.png".to_string(),
            "Ödeme sayfası — Safari.png".to_string(),
            "$HOME and ${PATH}.png".to_string(),
            "glob*?[a].png".to_string(),
            "semi;colon && pipe | amp &.png".to_string(),
        ];
        for name in names {
            let path = dir.path().join(&name);
            std::fs::write(&path, b"png").unwrap();
            let text = path.to_string_lossy().into_owned();
            let quoted = quote_path(&text);
            for shell in ["/bin/sh", "/bin/zsh", "/bin/bash"] {
                if !Path::new(shell).exists() {
                    continue;
                }
                let out = std::process::Command::new(shell)
                    .arg("-c")
                    .arg(format!("printf %s {quoted}"))
                    .current_dir(dir.path())
                    .output()
                    .unwrap();
                assert!(out.status.success(), "{shell} failed on {quoted}");
                assert_eq!(String::from_utf8_lossy(&out.stdout), text, "{shell} read {quoted} back as another path");
                assert!(!marker.exists(), "{shell} ran a command from {quoted}");
            }
        }
        assert_eq!(quote_path("/Users/deniz/a.png"), "/Users/deniz/a.png");
    }
}
