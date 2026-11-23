//! Interval capture: one target (an area, a window or a display) taken silently every few seconds
//! and written as one series into a folder of its own, which the history lists as one entry
//! (`EntryKind::Series`).
//!
//! - **Start.** `capture_start_interval` with the popover's plan, or ⌥⇧⌘K and the menu with the
//!   plan of Settings (the form's last one). The Window target takes the frontmost window and
//!   the Display target the display under the pointer, at once; the Area target opens the
//!   overlay first, whose selection comes back through `capture_interval_area`. Without the
//!   screen recording permission the series never starts.
//! - **Schedule.** The `glimpsfy-interval` thread ticks at start + k·interval, so a slow frame
//!   never pushes the later ones back. A tick sends one request to the
//!   `glimpsfy-interval-capture` worker, never a second while the first still runs (the system's
//!   capture queue would only grow), and waits [`FRAME_WAIT`] for its frame. A frame that does
//!   not come in time, or a tick that finds the worker still busy, is skipped: an empty row of
//!   the series and a line in `capture.log`. [`STALL_AFTER`] skips in a row stop the series.
//!   Ticks missed while the computer slept are skipped too, without counting toward a stall.
//! - **Writing.** The `glimpsfy-interval-write` thread encodes each frame in the format and scale
//!   of Settings › Output and writes it with `create_new` into the series' folder
//!   (`output::write_series_frame`). The history's entry is opened once the first frame is on
//!   disk, and every later frame, written or skipped, is added to it in order; its thumbnail
//!   follows the newest frame at most every [`THUMBNAIL_EVERY`]. Nothing passes through
//!   `output::deliver`: no toast, no shutter, no clipboard.
//! - **Stop.** The plan's end, ⌥⇧⌘K, the menu, ⌥-click on the menu bar icon and Stop in the
//!   popover stop it quietly. A full disk, a stalled capture service, a lost permission or a
//!   closed window stop it with a toast that stays, and the frames so far stay in the folder.
//!
//! Every change of the series reaches every window as `capture://interval`, and the menu bar
//! icon shows the count ("12/60").

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use glimpsfy_capture::{
    selection_to_frame, CaptureError, CaptureOptions, Display, DisplayId, Frame, LogicalRect, WindowCaptureOptions,
    WindowId, WindowListOptions,
};
use glimpsfy_history::{EntryDisplay, EntryId, EntryKind, EntrySource, NewEntry, PixelSize, SeriesFrame};
use tauri::{AppHandle, Emitter, Manager};

use crate::commands::capture::{events::INTERVAL, CaptureMode, CloseReason, IntervalState, Selection};
use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::settings::{IntervalEnd, IntervalPlan, IntervalTarget, Output, WindowBackground};
use crate::windows::{SeriesStop, Toast, ToastKind};
use crate::{capture_log, dev, events, i18n, output, trace, tray, windows, AppState};

/// How long a tick waits for its frame before the frame counts as skipped.
const FRAME_WAIT: Duration = Duration::from_secs(1);
/// Skipped frames in a row that stop the series.
const STALL_AFTER: u32 = 5;
/// Frames waiting to be written at most; a tick that finds more counts as skipped, so a slow
/// disk never piles frames up in memory.
const QUEUE: usize = 2;
/// The history's card follows the newest frame at most this often.
const THUMBNAIL_EVERY: Duration = Duration::from_secs(2);
/// An area series waits this long for its selection; an older plan is not used.
const AREA_WAIT: Duration = Duration::from_secs(30);

/// What a running series takes each tick.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Target {
    /// A display-local, logical rectangle of a display, taken while the display keeps its size.
    Area {
        display: DisplayId,
        display_frame: LogicalRect,
        rect: LogicalRect,
    },
    Window {
        id: WindowId,
    },
    Display {
        id: DisplayId,
    },
}

/// What the history and the popover say about a series' source.
#[derive(Clone, Debug, Default)]
struct About {
    /// The window's app, or the display's name; the popover's line names it.
    label: Option<String>,
    source: EntrySource,
    display: Option<EntryDisplay>,
    scale: Option<f64>,
}

struct Run {
    generation: u64,
    stop: Arc<AtomicBool>,
    state: IntervalState,
}

static RUN: Mutex<Option<Run>> = Mutex::new(None);
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// The last series, which the stalled toast's Restart takes again.
static LAST: Mutex<Option<(IntervalPlan, Target, About)>> = Mutex::new(None);
/// The plan of an area series whose overlay is opening; `capture_frozen` takes it.
static AREA: Mutex<Option<(IntervalPlan, Instant)>> = Mutex::new(None);

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or_default()
}

fn local(ms: i64) -> chrono::DateTime<chrono::Local> {
    chrono::DateTime::<chrono::Local>::from(UNIX_EPOCH + Duration::from_millis(ms.max(0) as u64))
}

/// The series that runs now, or an idle state.
pub fn state() -> IntervalState {
    lock(&RUN).as_ref().map(|run| run.state.clone()).unwrap_or_default()
}

pub fn is_running() -> bool {
    lock(&RUN).is_some()
}

/// The plan an area capture opening now belongs to, if an interval series asked for it.
pub(crate) fn take_area_plan() -> Option<IntervalPlan> {
    lock(&AREA).take().filter(|(_, at)| at.elapsed() < AREA_WAIT).map(|(plan, _)| plan)
}

/// ⌥⇧⌘K and the menu: stops the series that runs, or starts one with the plan of Settings.
pub fn toggle(app: &AppHandle) {
    if stop(app) {
        return;
    }
    let plan = app.state::<AppState>().settings.get().general.interval;
    if let Err(error) = start(app, Some(plan)) {
        windows::toast_failure(app, None, &error);
    }
}

/// Starts a series with `plan`, or, without one, the last series again (the stalled toast's
/// Restart) or the plan of Settings when there was none. A plan is kept in Settings for the next
/// time. An area series opens the overlay and starts once the selection comes back.
pub fn start(app: &AppHandle, plan: Option<IntervalPlan>) -> CommandResult<IntervalState> {
    if is_running() {
        return Err(CommandError::new(ErrorCode::Busy, "an interval capture is already running"));
    }
    let state = app.state::<AppState>();
    let synthetic = dev::options().synthetic;
    let Some(plan) = plan.map(IntervalPlan::clamped) else {
        let last = lock(&LAST).clone();
        return match last {
            Some((plan, target, about)) => {
                super::permitted(app, synthetic)?;
                begin(app, plan, target, about)
            }
            None => start(app, Some(state.settings.get().general.interval)),
        };
    };
    if state.settings.get().general.interval != plan {
        match state.settings.update(|s| s.general.interval = plan) {
            Ok((saved, _)) => {
                let _ = app.emit(events::SETTINGS_CHANGED, saved);
            }
            Err(error) => trace::log(format!("[glimpsfy] the interval plan could not be saved: {error}")),
        }
    }
    super::permitted(app, synthetic)?;
    match plan.target {
        IntervalTarget::Area => {
            if super::delay::is_running() || !state.sessions.is_idle() {
                return Err(CommandError::new(ErrorCode::Busy, "another capture is open"));
            }
            *lock(&AREA) = Some((plan, Instant::now()));
            if let Err(error) = super::start_now(app, CaptureMode::Area, Instant::now()) {
                *lock(&AREA) = None;
                return Err(error);
            }
            Ok(self::state())
        }
        IntervalTarget::Window => {
            let windows = state.backend.windows(WindowListOptions::default())?;
            let front = windows
                .iter()
                .find(|w| w.layer == 0 && w.alpha > 0.0)
                .ok_or_else(|| CommandError::not_found("no window is open to follow"))?;
            let displays = state.backend.displays().unwrap_or_default();
            let display = display_of(&displays, front.bounds);
            let about = About {
                label: Some(front.app_name.clone()),
                source: EntrySource {
                    app_name: Some(front.app_name.clone()),
                    bundle_id: front.bundle_id.clone(),
                    window_title: front.title.clone(),
                },
                display: display.map(|d| EntryDisplay { id: d.id.0, name: Some(d.name.clone()) }),
                scale: display.map(|d| d.scale.0),
            };
            begin(app, plan, Target::Window { id: front.id }, about)
        }
        IntervalTarget::Display => {
            let displays = state.backend.displays()?;
            let pointer = super::pointer();
            let display = displays
                .iter()
                .find(|d| d.frame.contains(pointer))
                .or_else(|| displays.iter().find(|d| d.is_primary))
                .or(displays.first())
                .ok_or_else(|| CommandError::not_found("no display is connected"))?;
            let about = About {
                label: Some(display.name.clone()),
                display: Some(EntryDisplay { id: display.id.0, name: Some(display.name.clone()) }),
                scale: Some(display.scale.0),
                ..About::default()
            };
            begin(app, plan, Target::Display { id: display.id }, about)
        }
    }
}

/// The overlay's selection of an area series: the capture closes and the series starts.
pub fn start_area(app: &AppHandle, selection: Selection) -> CommandResult<IntervalState> {
    let state = app.state::<AppState>();
    let session = state
        .sessions
        .get(selection.capture_id)
        .ok_or_else(|| CommandError::not_found(format!("capture #{}", selection.capture_id.0)))?;
    let plan = session.interval.ok_or_else(|| CommandError::invalid("this capture does not start a series"))?;
    let entry = session
        .display(selection.display_id)
        .ok_or_else(|| CommandError::not_found(format!("display {}", selection.display_id.0)))?;
    let display = entry.display.clone();
    let bounds = LogicalRect::new(0.0, 0.0, display.frame.width, display.frame.height);
    let rect = selection
        .rect
        .intersection(&bounds)
        .filter(|r| r.width >= 1.0 && r.height >= 1.0)
        .ok_or_else(|| CommandError::invalid("the selection is empty"))?;
    let front = session.front.clone();
    let about = About {
        label: front.app_name.clone(),
        source: EntrySource { app_name: front.app_name, bundle_id: front.bundle_id, window_title: front.title },
        display: Some(EntryDisplay { id: display.id.0, name: Some(display.name.clone()) }),
        scale: Some(display.scale.0),
    };
    super::close(app, selection.capture_id, CloseReason::Interval);
    begin(app, plan, Target::Area { display: display.id, display_frame: display.frame, rect }, about)
}

/// Stops the series that runs; its thread writes what is queued and then tells every window.
/// Answers whether one ran.
pub fn stop(_app: &AppHandle) -> bool {
    match lock(&RUN).as_ref() {
        Some(run) => {
            run.stop.store(true, Ordering::SeqCst);
            trace::log("[glimpsfy] interval capture stopped by the user");
            true
        }
        None => false,
    }
}

fn display_of(displays: &[Display], bounds: LogicalRect) -> Option<&Display> {
    let centre = glimpsfy_capture::LogicalPoint { x: bounds.x + bounds.width / 2.0, y: bounds.y + bounds.height / 2.0 };
    displays.iter().find(|d| d.frame.contains(centre)).or_else(|| displays.iter().find(|d| d.is_primary))
}

/// Makes the series' folder and starts its threads.
fn begin(app: &AppHandle, plan: IntervalPlan, target: Target, about: About) -> CommandResult<IntervalState> {
    let settings = app.state::<AppState>().settings.get();
    let started_at_ms = now_ms();
    let started = local(started_at_ms);
    let folder_name = i18n::text(
        settings.general.language,
        "interval.folder",
        &[("date", &started.format("%Y-%m-%d").to_string()), ("time", &started.format("%H.%M").to_string())],
    );
    let folder = output::series_folder(&settings.output, &folder_name)?;
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    let stop = Arc::new(AtomicBool::new(false));
    let total = plan.frame_limit();
    let folder_name = folder.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(folder_name);
    let state = IntervalState {
        running: true,
        target: Some(plan.target),
        label: about.label.clone(),
        name: Some(series_name(&about, target, &folder_name)),
        every_ms: plan.every_seconds as u64 * 1000,
        taken: 0,
        total,
        manual: plan.end == IntervalEnd::Manual,
        skipped: 0,
        last_skipped_at_ms: None,
        started_at_ms,
        next_at_ms: Some(started_at_ms),
        entry_id: None,
        latest_index: None,
        latest_at_ms: None,
        folder: Some(folder.to_string_lossy().into_owned()),
        folder_name: Some(folder_name.clone()),
    };
    {
        let mut run = lock(&RUN);
        if run.is_some() {
            let _ = std::fs::remove_dir(&folder);
            return Err(CommandError::new(ErrorCode::Busy, "an interval capture is already running"));
        }
        *run = Some(Run { generation, stop: stop.clone(), state: state.clone() });
    }
    *lock(&LAST) = Some((plan, target, about.clone()));
    trace::log(format!(
        "[glimpsfy] interval capture of {target:?} every {} s, at most {total} frames, into {}",
        plan.every_seconds,
        folder.display()
    ));
    announce(app);

    let handle = app.clone();
    let spawned = thread::Builder::new().name("glimpsfy-interval".into()).spawn(move || {
        let series = Series { app: handle, generation, plan, target, about, folder, folder_name, started_at_ms, stop };
        series.run();
    });
    if let Err(error) = spawned {
        *lock(&RUN) = None;
        announce(app);
        return Err(CommandError::internal(error.to_string()));
    }
    Ok(state)
}

/// Tells every window, and puts the count on the menu bar icon.
fn announce(app: &AppHandle) {
    let state = state();
    tray::show_series(app, state.running.then(|| tray_count(&state)));
    let _ = app.emit(INTERVAL, &state);
    tray::refresh(app);
}

fn tray_count(state: &IntervalState) -> String {
    if state.manual {
        state.taken.to_string()
    } else {
        format!("{}/{}", state.taken, state.total)
    }
}

/// Changes the running series' state, if it is still `generation`, and tells the windows.
fn update(app: &AppHandle, generation: u64, change: impl FnOnce(&mut IntervalState)) {
    let state = {
        let mut run = lock(&RUN);
        match run.as_mut() {
            Some(run) if run.generation == generation => {
                change(&mut run.state);
                run.state.clone()
            }
            _ => return,
        }
    };
    tray::show_series(app, Some(tray_count(&state)));
    let _ = app.emit(INTERVAL, &state);
}

/// Why the writer stopped the series.
#[derive(Clone, Debug)]
enum WriteFailure {
    DiskFull,
    /// The folder or the entry was deleted while the series ran; it stops without a word.
    Gone,
    Other(String),
}

enum Job {
    Frame { index: u32, due_ms: i64, frame: Frame },
    Skipped { index: u32, due_ms: i64 },
}

struct Series {
    app: AppHandle,
    generation: u64,
    plan: IntervalPlan,
    target: Target,
    about: About,
    folder: std::path::PathBuf,
    folder_name: String,
    started_at_ms: i64,
    stop: Arc<AtomicBool>,
}

impl Series {
    fn run(self) {
        let app = self.app.clone();
        let interval = Duration::from_secs(self.plan.every_seconds.max(1) as u64);
        let total = self.plan.frame_limit();
        // An area's overlay is still fading out at the first tick otherwise.
        let start = Instant::now()
            + if matches!(self.target, Target::Area { .. }) { Duration::from_millis(250) } else { Duration::ZERO };
        let start_ms = self.started_at_ms + if matches!(self.target, Target::Area { .. }) { 250 } else { 0 };

        // The capture worker: one request at a time.
        let (requests, worker_requests) = mpsc::channel::<u32>();
        let (worker_results, results) = mpsc::channel::<(u32, Result<Frame, CaptureError>)>();
        let busy = Arc::new(AtomicBool::new(false));
        {
            let (app, target, busy) = (app.clone(), self.target, busy.clone());
            let _ = thread::Builder::new().name("glimpsfy-interval-capture".into()).spawn(move || {
                for index in worker_requests {
                    let started = Instant::now();
                    let result = take(&app, target);
                    let backend = app.state::<AppState>().backend.clone();
                    let what = format!("Interval frame {}", index + 1);
                    capture_log::record(&app, &what, started.elapsed(), backend.take_report(), result.as_ref().err());
                    busy.store(false, Ordering::SeqCst);
                    if worker_results.send((index, result)).is_err() {
                        return;
                    }
                }
            });
        }

        // The writer: files and history rows, in order.
        let (jobs, writer_jobs) = mpsc::channel::<Job>();
        let queued = Arc::new(AtomicUsize::new(0));
        let failure: Arc<Mutex<Option<WriteFailure>>> = Arc::new(Mutex::new(None));
        let writer = {
            let writer = Writer {
                app: app.clone(),
                generation: self.generation,
                output: app.state::<AppState>().settings.get().output,
                folder: self.folder.clone(),
                pad: if total > 999 { 4 } else { 3 },
                plan: self.plan,
                about: self.about.clone(),
                name: series_name(&self.about, self.target, &self.folder_name),
                started_at_ms: self.started_at_ms,
                entry: None,
                waiting: Vec::new(),
                written: 0,
                thumbnail_at: None,
                queued: queued.clone(),
                failure: failure.clone(),
            };
            thread::Builder::new().name("glimpsfy-interval-write".into()).spawn(move || writer.run(writer_jobs))
        };

        let mut in_a_row = 0;
        let mut stopped: Option<SeriesStop> = None;
        let mut index = 0;
        while index < total {
            let due = start + interval * index;
            let due_ms = start_ms + (interval * index).as_millis() as i64;
            if !wait_until(due, &self.stop) {
                break;
            }
            let skip = |reason: &str, counts: bool, in_a_row: &mut u32| {
                capture_log::skipped(&app, &format!("Interval frame {}", index + 1), reason);
                let _ = jobs.send(Job::Skipped { index, due_ms });
                if counts {
                    *in_a_row += 1;
                }
                update(&app, self.generation, |s| {
                    s.skipped += 1;
                    s.last_skipped_at_ms = Some(due_ms);
                });
            };
            if Instant::now().saturating_duration_since(due) > interval {
                // The computer slept through this tick: nothing could have taken it.
                skip("the computer was asleep", false, &mut in_a_row);
            } else if busy.load(Ordering::SeqCst) {
                skip("the previous frame's request is still running", true, &mut in_a_row);
            } else {
                busy.store(true, Ordering::SeqCst);
                let _ = requests.send(index);
                let deadline = Instant::now() + FRAME_WAIT;
                let answer = loop {
                    match results.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                        Ok((answered, result)) if answered == index => break Some(result),
                        // A late answer to an earlier tick, which was skipped already.
                        Ok(_) => continue,
                        Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => break None,
                    }
                };
                match answer {
                    None => skip(&format!("no frame within {} ms", FRAME_WAIT.as_millis()), true, &mut in_a_row),
                    Some(Ok(frame)) if queued.load(Ordering::SeqCst) < QUEUE => {
                        queued.fetch_add(1, Ordering::SeqCst);
                        let _ = jobs.send(Job::Frame { index, due_ms, frame });
                        in_a_row = 0;
                    }
                    Some(Ok(_)) => skip("the disk is still writing the frames before", true, &mut in_a_row),
                    Some(Err(CaptureError::PermissionDenied)) => {
                        stopped = Some(SeriesStop::Permission);
                        break;
                    }
                    Some(Err(CaptureError::WindowNotFound(_))) => {
                        stopped = Some(SeriesStop::WindowGone);
                        break;
                    }
                    Some(Err(error)) => skip(&error.to_string(), true, &mut in_a_row),
                }
            }
            index += 1;
            let next = (index < total).then(|| start_ms + (interval * index).as_millis() as i64);
            update(&app, self.generation, |s| {
                s.taken = index;
                s.next_at_ms = next;
            });
            if in_a_row >= STALL_AFTER {
                stopped = Some(SeriesStop::Stalled);
                break;
            }
            if lock(&failure).is_some() {
                break;
            }
        }
        drop(requests);
        drop(jobs);
        let summary = writer.ok().and_then(|w| w.join().ok()).unwrap_or_default();
        let failed = lock(&failure).clone();
        self.finish(stopped, failed, summary);
    }

    /// Ends the series: the state goes, the icon's count goes, and a stop the user did not ask
    /// for gets its toast.
    fn finish(self, stopped: Option<SeriesStop>, failed: Option<WriteFailure>, written: u32) {
        let app = &self.app;
        {
            let mut run = lock(&RUN);
            if run.as_ref().is_some_and(|r| r.generation == self.generation) {
                *run = None;
            }
        }
        // A series that wrote nothing leaves no empty folder behind (only an empty one goes).
        let folder_left = written > 0 || std::fs::remove_dir(&self.folder).is_err();
        trace::log(format!(
            "[glimpsfy] interval capture ended: {written} frame(s) in {}{}",
            self.folder.display(),
            match (&stopped, &failed) {
                (Some(reason), _) => format!(", stopped: {reason:?}"),
                (None, Some(failure)) => format!(", stopped: {failure:?}"),
                _ => String::new(),
            }
        ));
        announce(app);
        let reason = match (stopped, &failed) {
            (Some(reason), _) => Some(reason),
            (None, Some(WriteFailure::DiskFull)) => Some(SeriesStop::DiskFull),
            (None, Some(WriteFailure::Other(message))) => {
                windows::toast_failure(app, None, &CommandError::new(ErrorCode::Io, message.clone()));
                None
            }
            _ => None,
        };
        if let Some(reason) = reason {
            let mut toast = Toast::new(ToastKind::SeriesStopped);
            toast.series_stop = Some(reason);
            toast.frames = Some(written);
            if folder_left {
                toast.path = Some(self.folder.to_string_lossy().into_owned());
                toast.folder = Some(self.folder_name.clone());
            }
            windows::toast(app, None, toast);
        }
    }
}

/// Waits until `due`, or until the series is stopped (answers `false`).
fn wait_until(due: Instant, stop: &AtomicBool) -> bool {
    loop {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        let left = due.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        thread::sleep(left.min(Duration::from_millis(50)));
    }
}

/// The name of the series' entry: the followed window as the history names windows ("Title —
/// App"), or the folder's own name for an area or a display.
fn series_name(about: &About, target: Target, folder_name: &str) -> String {
    if let Target::Window { .. } = target {
        let title = about.source.window_title.as_deref().map(str::trim).filter(|t| !t.is_empty());
        let app = about.source.app_name.as_deref().map(str::trim).filter(|a| !a.is_empty());
        match (title, app) {
            (Some(title), Some(app)) if !title.contains(app) => return format!("{title} — {app}"),
            (Some(title), _) => return title.to_string(),
            (None, Some(app)) => return app.to_string(),
            (None, None) => {}
        }
    }
    folder_name.to_string()
}

/// One frame of `target`, as the settings take captures (the pointer, a window's shadow,
/// corners and background).
fn take(app: &AppHandle, target: Target) -> Result<Frame, CaptureError> {
    let state = app.state::<AppState>();
    let settings = state.settings.get();
    let backend = state.backend.clone();
    let synthetic = dev::options().synthetic;
    let options = CaptureOptions { shows_cursor: settings.general.include_pointer, exclude_own_windows: true };
    let display_frame = |id: DisplayId| -> Result<(Display, Frame), CaptureError> {
        let display = backend.displays()?.into_iter().find(|d| d.id == id).ok_or(CaptureError::DisplayNotFound(id))?;
        let frame = if synthetic {
            super::synthetic_frames(std::slice::from_ref(&display)).into_iter().next().map(|(_, f)| (*f).clone())
        } else {
            backend.capture_displays(&[id], &options)?.into_iter().find(|(d, _)| *d == id).map(|(_, f)| f)
        };
        Ok((display, frame.ok_or(CaptureError::DisplayNotFound(id))?))
    };
    match target {
        Target::Display { id } => display_frame(id).map(|(_, frame)| frame),
        Target::Area { display, display_frame: then, rect } => {
            let (now, frame) = display_frame(display)?;
            // The area means the same pixels only while the display keeps its size.
            if (now.frame.width - then.width).abs() >= 0.5 || (now.frame.height - then.height).abs() >= 0.5 {
                return Err(CaptureError::DisplayNotFound(display));
            }
            let pixels = selection_to_frame(rect, now.frame.size(), frame.size)
                .ok_or_else(|| CaptureError::InvalidArgument("the area is outside its display".into()))?;
            frame.crop(pixels)
        }
        Target::Window { id } => {
            if synthetic {
                let displays = backend.displays()?;
                let primary = displays.iter().find(|d| d.is_primary).or(displays.first());
                return match primary {
                    Some(display) => display_frame(display.id).map(|(_, frame)| frame),
                    None => Err(CaptureError::Backend("no display is connected".into())),
                };
            }
            let options = WindowCaptureOptions {
                shows_cursor: settings.general.include_pointer,
                include_shadow: settings.output.window_shadow,
                keep_rounded_corners: settings.output.window_corners,
                background: match settings.output.window_background {
                    WindowBackground::White => Some([255, 255, 255]),
                    WindowBackground::Transparent => None,
                },
            };
            backend.capture_window(id, &options)
        }
    }
}

/// The writer's side of a series.
struct Writer {
    app: AppHandle,
    generation: u64,
    output: Output,
    folder: std::path::PathBuf,
    /// Digits of the frame number in a file name ("001 14.32.00.png").
    pad: usize,
    plan: IntervalPlan,
    about: About,
    name: String,
    started_at_ms: i64,
    entry: Option<EntryId>,
    /// Skipped frames before the first written one, added once the entry exists.
    waiting: Vec<SeriesFrame>,
    written: u32,
    thumbnail_at: Option<Instant>,
    queued: Arc<AtomicUsize>,
    failure: Arc<Mutex<Option<WriteFailure>>>,
}

impl Writer {
    /// Writes every job until the series ends; answers how many frames reached the disk.
    fn run(mut self, jobs: mpsc::Receiver<Job>) -> u32 {
        for job in jobs {
            if lock(&self.failure).is_some() {
                if let Job::Frame { .. } = job {
                    self.queued.fetch_sub(1, Ordering::SeqCst);
                }
                continue;
            }
            match job {
                Job::Frame { index, due_ms, frame } => {
                    self.frame(index, due_ms, frame);
                    self.queued.fetch_sub(1, Ordering::SeqCst);
                }
                Job::Skipped { index, due_ms } => {
                    let row = SeriesFrame { index, taken_at_ms: due_ms, file: None };
                    match &self.entry {
                        Some(_) => self.add(&row, None, None),
                        None => self.waiting.push(row),
                    }
                }
            }
        }
        self.written
    }

    fn fail(&self, failure: WriteFailure) {
        lock(&self.failure).get_or_insert(failure);
    }

    fn frame(&mut self, index: u32, due_ms: i64, frame: Frame) {
        let stem = format!("{:0width$} {}", index + 1, local(due_ms).format("%H.%M.%S"), width = self.pad);
        let (path, bytes, size) = match output::write_series_frame(&self.output, &self.folder, &stem, frame) {
            Ok(written) => written,
            Err(error) => {
                trace::log(format!("[glimpsfy] interval frame {} was not written: {error}", index + 1));
                self.fail(match error.kind() {
                    std::io::ErrorKind::StorageFull | std::io::ErrorKind::QuotaExceeded => WriteFailure::DiskFull,
                    std::io::ErrorKind::NotFound => WriteFailure::Gone,
                    _ => WriteFailure::Other(format!(
                        "A frame of the interval capture could not be written in “{}” ({error}).",
                        self.folder.display()
                    )),
                });
                return;
            }
        };
        self.written += 1;
        let size = PixelSize { width: size.width, height: size.height };
        if self.entry.is_none() && self.written == 1 {
            self.open_entry(size);
        }
        let file = path.file_name().map(|n| n.to_string_lossy().into_owned());
        let row = SeriesFrame { index, taken_at_ms: due_ms, file };
        if self.entry.is_some() {
            self.add(&row, Some(bytes), Some(size));
        }
        let entry = self.entry.clone();
        update(&self.app, self.generation, |s| {
            s.entry_id = entry;
            s.latest_index = Some(index);
            s.latest_at_ms = Some(due_ms);
        });
    }

    /// The entry opens with the first frame on disk, and takes the skipped frames before it.
    fn open_entry(&mut self, size: PixelSize) {
        let new = NewEntry {
            kind: Some(EntryKind::Series),
            name: self.name.clone(),
            created_at_ms: self.started_at_ms,
            saved_path: Some(self.folder.clone()),
            size: Some(size),
            scale: self.about.scale,
            source: self.about.source.clone(),
            display: self.about.display.clone(),
            series_interval_ms: Some(self.plan.every_seconds as u64 * 1000),
            ..NewEntry::default()
        };
        match crate::commands::history::record_series(&self.app, new) {
            Ok(Some(entry)) => {
                self.entry = Some(entry.id);
                for row in std::mem::take(&mut self.waiting) {
                    self.add(&row, None, None);
                }
            }
            // The history is off: the frames are written all the same.
            Ok(None) => self.waiting.clear(),
            Err(error) => {
                trace::log(format!("[glimpsfy] the series was not recorded in the history: {}", error.message))
            }
        }
    }

    fn add(&mut self, row: &SeriesFrame, bytes: Option<u64>, size: Option<PixelSize>) {
        let Some(id) = self.entry.clone() else { return };
        let thumbnail = row.file.is_some() && self.thumbnail_at.is_none_or(|at| at.elapsed() >= THUMBNAIL_EVERY);
        if thumbnail {
            self.thumbnail_at = Some(Instant::now());
        }
        match crate::commands::history::add_series_frame(&self.app, &id, row, bytes, size, thumbnail) {
            Ok(_) => {}
            Err(error) if error.code == ErrorCode::NotFound => {
                trace::log("[glimpsfy] the series was deleted from the history while it ran; it stops");
                self.fail(WriteFailure::Gone);
            }
            Err(error) => trace::log(format!("[glimpsfy] a series frame was not recorded: {}", error.message)),
        }
    }
}
