//! Drawing on the screen and the spotlight while a take runs: the state that the Presentation
//! card, the rail and the canvas share (`record://present`), drawing mode's keys and its hold on
//! the mouse, the rail window (`rec-rail`) beside the recorded area, and the pointer the canvas
//! draws the brush ring and the spotlight at. The drawings themselves live in the canvas page,
//! which is part of the file; the core only says what they are drawn with and when the last one
//! or all of them go (`record://ink`).
//!
//! Drawing mode never takes the keyboard and never activates the app: the canvas only stops
//! letting clicks through, the app in front keeps the keyboard, and the mode's keys arrive as
//! temporary global shortcuts (`shortcuts::bind_ink`). The state lives from the moment a take
//! goes live until it ends, and the drawings end with it.

use std::sync::Mutex;

use glimpsfy_capture::{LogicalPoint, LogicalRect};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, EventTarget, Manager};

use super::{lock, state, CANVAS, RAIL};
use crate::commands::error::{CommandError, CommandResult};
use crate::commands::record::events;
use crate::shortcuts::Action;
use crate::{trace, AppState};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InkTool {
    Pen,
    Highlighter,
    Arrow,
    Rect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum InkColor {
    Red,
    Yellow,
    Blue,
}

/// A key of drawing mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InkKey {
    Tool(InkTool),
    Ink(InkColor),
    Undo,
    Clear,
    Done,
}

/// Drawing mode's keys: the editor's tool letters, the three inks, undo, clear and Esc. They are
/// registered only while the mode is on, and a capture that starts ends the mode first, so the
/// overlay's own letters never meet them.
pub const INK_KEYS: [(&str, InkKey); 10] = [
    ("P", InkKey::Tool(InkTool::Pen)),
    ("H", InkKey::Tool(InkTool::Highlighter)),
    ("A", InkKey::Tool(InkTool::Arrow)),
    ("R", InkKey::Tool(InkTool::Rect)),
    ("1", InkKey::Ink(InkColor::Red)),
    ("2", InkKey::Ink(InkColor::Yellow)),
    ("3", InkKey::Ink(InkColor::Blue)),
    ("CommandOrControl+Z", InkKey::Undo),
    ("Backspace", InkKey::Clear),
    ("Escape", InkKey::Done),
];

/// The spotlight's radius when a take starts, and how far ⌥ with the wheel moves it, in points.
const SPOT_RADIUS: f64 = 80.0;
const SPOT_RADIUS_MIN: f64 = 40.0;
const SPOT_RADIUS_MAX: f64 = 240.0;
/// The rail's gap to the recorded area, and its inset when it has to sit inside the area.
const RAIL_GAP: f64 = 8.0;
const RAIL_INSET: f64 = 12.0;
/// Room right of the rail for its tooltips; the window takes the mouse only over the rail itself.
const RAIL_TIP_ROOM: f64 = 8.0 + 320.0;

/// What the Presentation card, the rail and the canvas show (`record://present`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentState {
    pub drawing: bool,
    pub spotlight: bool,
    /// Drawings fade 3 s after the last stroke.
    pub fade: bool,
    pub tool: InkTool,
    pub ink: InkColor,
    /// The spotlight's radius, in points.
    pub radius: f64,
}

/// A change from the card, the rail or the canvas; what is left out stays.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentChange {
    pub drawing: Option<bool>,
    pub spotlight: Option<bool>,
    pub fade: Option<bool>,
    pub tool: Option<InkTool>,
    pub ink: Option<InkColor>,
    pub radius: Option<f64>,
    /// The last drawing goes.
    #[serde(default)]
    pub undo: bool,
    /// Every drawing goes.
    #[serde(default)]
    pub clear: bool,
}

/// What the canvas does with its drawings (`record://ink`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum InkCommand {
    Undo,
    Clear,
}

/// The rail as its page measured it: the glass's size and corner radius, in points.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize)]
pub struct RailSize {
    pub width: f64,
    pub height: f64,
    pub radius: f64,
}

struct Present {
    state: PresentState,
    /// The ink last chosen with a tool other than the highlighter, which those tools go back to:
    /// the highlighter starts yellow, and leaving it gives the pen its colour back.
    pen_ink: InkColor,
    /// The recorded rectangle and the visible part of its display, global; they stay for the take.
    rect: LogicalRect,
    visible: LogicalRect,
    rail: Option<RailSize>,
    /// The rail's glass, global, while the rail shows: its window takes the mouse only over it.
    rail_hit: Option<LogicalRect>,
    rail_mouse: bool,
    /// Whether the canvas takes the mouse now: always in drawing mode, and with the spotlight
    /// alone while ⌥ is held over the area, so ⌥ with the wheel reaches it.
    canvas_mouse: bool,
    /// What the canvas was last told about the pointer.
    told: Option<Option<LogicalPoint>>,
}

static PRESENT: Mutex<Option<Present>> = Mutex::new(None);

impl Present {
    fn apply(&mut self, change: &PresentChange, capturing: bool) {
        if let Some(on) = change.drawing {
            // An open capture's overlay owns the letters drawing mode would take.
            self.state.drawing = on && !capturing;
        }
        if let Some(on) = change.spotlight {
            self.state.spotlight = on;
        }
        if let Some(on) = change.fade {
            self.state.fade = on;
        }
        if let Some(tool) = change.tool {
            let was = self.state.tool;
            if tool == InkTool::Highlighter && was != InkTool::Highlighter {
                self.state.ink = InkColor::Yellow;
            } else if tool != InkTool::Highlighter && was == InkTool::Highlighter {
                self.state.ink = self.pen_ink;
            }
            self.state.tool = tool;
        }
        if let Some(ink) = change.ink {
            self.state.ink = ink;
            if self.state.tool != InkTool::Highlighter {
                self.pen_ink = ink;
            }
        }
        if let Some(radius) = change.radius.filter(|r| r.is_finite()) {
            self.state.radius = radius.clamp(SPOT_RADIUS_MIN, SPOT_RADIUS_MAX);
        }
    }
}

/// Whether drawing and the spotlight work here: both follow the pointer, which a Wayland session
/// does not tell apps, so there the Presentation button and the two shortcuts are left out.
#[cfg(target_os = "macos")]
pub fn available() -> bool {
    true
}

#[cfg(not(target_os = "macos"))]
pub fn available() -> bool {
    crate::desktop::describe().capabilities.global_pointer
}

pub fn state_now() -> Option<PresentState> {
    lock(&PRESENT).as_ref().map(|p| p.state.clone())
}

fn emit(app: &AppHandle) {
    let now = state_now();
    let _ = app.emit_filter(
        events::PRESENT,
        now,
        |target| matches!(target, EventTarget::WebviewWindow { label } if label.starts_with("rec-")),
    );
}

fn capture_open(app: &AppHandle) -> bool {
    app.state::<AppState>().sessions.current().is_some() || crate::session::delay::is_running()
}

/// The take went live: drawing and the spotlight start off, the fade as the last take left it,
/// and the take's two keys are registered.
pub fn begin(app: &AppHandle) {
    let place = lock(&state(app).inner).take.as_ref().map(|t| (t.rect, t.display.visible));
    let Some((rect, visible)) = place else { return };
    let fade = app.state::<AppState>().settings.get().recording.ink_fades;
    *lock(&PRESENT) = Some(Present {
        state: PresentState {
            drawing: false,
            spotlight: false,
            fade,
            tool: InkTool::Pen,
            ink: InkColor::Red,
            radius: SPOT_RADIUS,
        },
        pen_ink: InkColor::Red,
        rect,
        visible,
        rail: None,
        rail_hit: None,
        rail_mouse: false,
        canvas_mouse: false,
        told: None,
    });
    // The rail's page loads now, hidden, so it has measured itself by the time drawing starts.
    if available() {
        let _ = super::panel(app, RAIL);
    }
    crate::shortcuts::bind_take(app);
    emit(app);
}

/// The take ended: drawing mode, the spotlight and their keys go, and the canvas, whose
/// configuration goes too, drops the drawings.
pub fn end(app: &AppHandle) {
    let Some(was) = lock(&PRESENT).take() else { return };
    crate::shortcuts::unbind_take(app);
    if was.state.drawing {
        crate::shortcuts::unbind_ink(app);
    }
    follow(app, false);
    hide_rail(app);
    if was.canvas_mouse {
        take_mouse(app, CANVAS, false);
    }
    emit(app);
}

/// Applies a change and what follows from it. Fails while no take runs.
pub fn change(app: &AppHandle, change: PresentChange) -> CommandResult<PresentState> {
    let capturing = change.drawing == Some(true) && capture_open(app);
    let (before, after) = {
        let mut guard = lock(&PRESENT);
        let present = guard.as_mut().ok_or_else(|| CommandError::invalid("no take is running"))?;
        let before = present.state.clone();
        present.apply(&change, capturing);
        (before, present.state.clone())
    };
    if capturing {
        trace::log("[glimpsfy] drawing mode stays off: a capture is open");
    }
    if change.undo {
        ink(app, InkCommand::Undo);
    }
    if change.clear {
        ink(app, InkCommand::Clear);
    }
    if before.fade != after.fade {
        persist_fade(app, after.fade);
    }
    if before.drawing != after.drawing {
        if after.drawing {
            start_drawing(app);
        } else {
            stop_drawing(app);
        }
    }
    let follows = |s: &PresentState| s.drawing || s.spotlight;
    if follows(&before) != follows(&after) {
        follow(app, follows(&after));
    }
    if before != after {
        emit(app);
    }
    Ok(after)
}

/// The fade carries over to the next take.
fn persist_fade(app: &AppHandle, on: bool) {
    match app.state::<AppState>().settings.update(|s| s.recording.ink_fades = on) {
        Ok((saved, _)) => {
            let _ = app.emit(crate::events::SETTINGS_CHANGED, saved);
        }
        Err(error) => trace::log(format!("[glimpsfy] the drawings' fade could not be saved: {error}")),
    }
}

fn ink(app: &AppHandle, command: InkCommand) {
    let _ = app.emit_to(EventTarget::webview_window(CANVAS), events::INK, command);
}

/// Drawing mode starts: the canvas takes the mouse (its first click already draws, since the
/// panel accepts the first mouse), the bar gives back the keyboard a click on the card gave it,
/// the mode's keys are registered and the rail shows.
fn start_drawing(app: &AppHandle) {
    crate::shortcuts::bind_ink(app);
    set_canvas_mouse(app, true);
    #[cfg(target_os = "macos")]
    if let Some(bar) = app.get_webview_window(super::BAR) {
        crate::macos::on_main(app, move |mtm| crate::macos::recording::resign_key(&bar, mtm));
    }
    show_rail(app);
}

/// Drawing mode ends (✓, Esc, the shortcut or a capture): the drawings stay on screen, and the
/// next click and key reach the app in front again.
fn stop_drawing(app: &AppHandle) {
    crate::shortcuts::unbind_ink(app);
    set_canvas_mouse(app, false);
    hide_rail(app);
}

fn set_canvas_mouse(app: &AppHandle, takes: bool) {
    let changed = match lock(&PRESENT).as_mut() {
        Some(p) if p.canvas_mouse != takes => {
            p.canvas_mouse = takes;
            true
        }
        _ => false,
    };
    if changed {
        take_mouse(app, CANVAS, takes);
    }
}

fn take_mouse(app: &AppHandle, label: &str, takes: bool) {
    let Some(window) = app.get_webview_window(label) else { return };
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::recording::take_mouse(&window, takes, mtm));
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        crate::desktop::recording::take_mouse(&window, takes);
    }
}

/// A key of drawing mode, from the shortcut handler.
pub fn ink_key(app: &AppHandle, key: InkKey) {
    let request = match key {
        InkKey::Tool(tool) => PresentChange { tool: Some(tool), ..Default::default() },
        InkKey::Ink(ink) => PresentChange { ink: Some(ink), ..Default::default() },
        InkKey::Undo => PresentChange { undo: true, ..Default::default() },
        InkKey::Clear => PresentChange { clear: true, ..Default::default() },
        InkKey::Done => PresentChange { drawing: Some(false), ..Default::default() },
    };
    if let Err(error) = change(app, request) {
        trace::log(format!("[glimpsfy] drawing key {key:?}: {error}"));
    }
}

/// ⌥⇧⌘D and ⌥⇧⌘L: each turns its part on or off.
pub fn toggle(app: &AppHandle, action: Action) {
    let Some(now) = state_now() else { return };
    let request = match action {
        Action::DrawOnScreen => PresentChange { drawing: Some(!now.drawing), ..Default::default() },
        Action::Spotlight => PresentChange { spotlight: Some(!now.spotlight), ..Default::default() },
        _ => return,
    };
    if let Err(error) = change(app, request) {
        trace::log(format!("[glimpsfy] {action:?}: {error}"));
    }
}

/// A capture starts: drawing mode ends first, so the overlay gets its letters back.
pub fn end_drawing(app: &AppHandle) {
    if state_now().is_some_and(|s| s.drawing) {
        let _ = change(app, PresentChange { drawing: Some(false), ..Default::default() });
    }
}

// ---------------------------------------------------------------------------------- the rail

/// Where the rail goes: 8 points left of the recorded area, else 8 points right of it, else
/// inside it, 12 points from its left edge; never past the visible part of the display.
pub fn rail_frame(area: LogicalRect, visible: LogicalRect, size: RailSize) -> LogicalRect {
    let left = area.x - RAIL_GAP - size.width;
    let right = area.x + area.width + RAIL_GAP;
    let (x, inside) = if left >= visible.x {
        (left, false)
    } else if right + size.width <= visible.x + visible.width {
        (right, false)
    } else {
        (area.x.max(visible.x) + RAIL_INSET, true)
    };
    let top = area.y.max(visible.y) + if inside { RAIL_INSET } else { 0.0 };
    let y = top.min(visible.y + visible.height - size.height).max(visible.y);
    LogicalRect { x, y, width: size.width, height: size.height }
}

/// The rail page measured the rail; it shows there while drawing mode is on.
pub fn fit_rail(app: &AppHandle, size: RailSize) -> CommandResult<()> {
    if !(size.width > 0.0 && size.height > 0.0) {
        return Err(CommandError::invalid("the rail has no size"));
    }
    let drawing = match lock(&PRESENT).as_mut() {
        Some(p) => {
            let moved = p.rail != Some(size);
            p.rail = Some(size);
            p.state.drawing && (moved || p.rail_hit.is_none())
        }
        None => false,
    };
    if drawing {
        show_rail(app);
    }
    Ok(())
}

/// Shows the rail window with its glass under it, in one turn of the main thread so the blur
/// moves with it. Without a measured size the page fits it once it has one.
fn show_rail(app: &AppHandle) {
    let placed = {
        let mut guard = lock(&PRESENT);
        let Some(p) = guard.as_mut().filter(|p| p.state.drawing) else { return };
        let Some(size) = p.rail else { return };
        let glass = rail_frame(p.rect, p.visible, size);
        let room = (p.visible.x + p.visible.width - glass.x - glass.width).clamp(0.0, RAIL_TIP_ROOM);
        let frame = LogicalRect { width: glass.width + room, ..glass };
        p.rail_hit = Some(glass);
        p.rail_mouse = super::pointer_over(&[glass]);
        (frame, size, p.rail_mouse)
    };
    let (frame, size, mouse) = placed;
    let Some(window) = super::panel(app, RAIL) else { return };
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| {
        let piece = super::setup::GlassPiece {
            rect: LogicalRect { x: 0.0, y: 0.0, width: size.width, height: size.height },
            radius: size.radius.clamp(0.0, size.width.min(size.height) / 2.0),
            small_shadow: false,
        };
        crate::macos::recording::show(&window, frame, false, !mouse, mtm);
        crate::macos::recording::rail_backdrop_show(&window, frame, &[piece], mtm);
    });
    #[cfg(not(target_os = "macos"))]
    {
        let _ = size;
        crate::desktop::recording::show(&window, frame, false, !mouse);
    }
}

fn hide_rail(app: &AppHandle) {
    if let Some(p) = lock(&PRESENT).as_mut() {
        p.rail_hit = None;
        p.rail_mouse = false;
    }
    super::hide(app, RAIL);
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, crate::macos::recording::rail_backdrop_hide);
}

// ------------------------------------------------------------------------------- the pointer

/// Whether the canvas hears the pointer from here, so the keystroke overlay's slower feed in the
/// watch thread stays silent.
pub fn feeds_pointer() -> bool {
    lock(&PRESENT).as_ref().is_some_and(|p| p.state.drawing || p.state.spotlight)
}

/// Whether the watch thread should poll at the display's pace: where no monitor reports every
/// move (outside macOS), the spotlight and the brush ring follow its polls.
pub fn wants_fast_watch() -> bool {
    !cfg!(target_os = "macos") && feeds_pointer()
}

/// Starts or stops following every pointer move for the canvas.
fn follow(app: &AppHandle, on: bool) {
    if let Some(p) = lock(&PRESENT).as_mut() {
        p.told = None;
    }
    #[cfg(target_os = "macos")]
    {
        let handle = app.clone();
        crate::macos::on_main(app, move |mtm| {
            crate::macos::recording::follow_pointer(&handle, on, pointer_moved, mtm);
            // The spotlight shows at once, before the pointer moves.
            if on {
                pointer_moved(&handle, crate::macos::pointer());
            }
        });
    }
    #[cfg(not(target_os = "macos"))]
    let _ = (app, on);
}

/// A pointer move, global: the canvas hears it while it is over the recorded area, and `null` once
/// it leaves (the spotlight and the ring go then).
fn pointer_moved(app: &AppHandle, pointer: LogicalPoint) {
    let told = {
        let mut guard = lock(&PRESENT);
        let Some(p) = guard.as_mut() else { return };
        let inside = p.rect.contains(pointer).then_some(pointer);
        let moved = match (inside, p.told.flatten()) {
            (Some(a), Some(b)) => (a.x - b.x).abs() + (a.y - b.y).abs() >= 0.5,
            _ => p.told != Some(inside),
        };
        if !moved {
            return;
        }
        p.told = Some(inside);
        inside
    };
    let _ = app.emit_to(EventTarget::webview_window(CANVAS), events::POINTER, told);
}

/// One turn of the watch thread: the rail's window takes the mouse only over the rail, the canvas
/// takes it while drawing and while ⌥ is held over the area with the spotlight on, and, outside
/// macOS, the canvas hears the pointer. With a button down nothing changes hands, so a press
/// keeps its window until it is let go.
pub fn watch_tick(app: &AppHandle, pointer: LogicalPoint, down: bool) {
    #[cfg(target_os = "macos")]
    let option = crate::macos::option_key_down();
    #[cfg(not(target_os = "macos"))]
    let option = crate::desktop::option_key_down();
    let (rail, canvas, feeds) = {
        let mut guard = lock(&PRESENT);
        let Some(p) = guard.as_mut() else { return };
        let mut rail = None;
        let mut canvas = None;
        if !down {
            if let Some(hit) = p.rail_hit {
                let over = hit.contains(pointer);
                if over != p.rail_mouse {
                    p.rail_mouse = over;
                    rail = Some(over);
                }
            }
            let wanted = p.state.drawing || (p.state.spotlight && option && p.rect.contains(pointer));
            if wanted != p.canvas_mouse {
                p.canvas_mouse = wanted;
                canvas = Some(wanted);
            }
        }
        (rail, canvas, p.state.drawing || p.state.spotlight)
    };
    if let Some(takes) = rail {
        take_mouse(app, RAIL, takes);
    }
    if let Some(takes) = canvas {
        take_mouse(app, CANVAS, takes);
    }
    if feeds && !cfg!(target_os = "macos") {
        pointer_moved(app, pointer);
    }
}
