//! The app behind the MCP server: it lists the displays, takes what a request would send when the
//! request arrives (so the card shows exactly that), keeps the card, and records how the request
//! ended in the log, the toasts and the history.

use glimpsfy_capture::{
    CaptureError, CaptureOptions, Frame, LogicalPoint, LogicalRect, PermissionState, PhysicalSize, PngEffort,
    WindowCaptureOptions, WindowInfo, WindowListOptions,
};
use glimpsfy_history::EntryKind;
use glimpsfy_mcp::gate::{ConnectionId, RequestId};
use glimpsfy_mcp::server::{ClientInfo, Host, Outcome};
use glimpsfy_mcp::tools::{self, Ask, Content, ToolError};
use serde_json::{json, Value};
use tauri::{AppHandle, Manager};

use super::{agents, emit_state, log, now_ms};
use crate::commands::agents::{AgentAskKind, AgentLogEntry, AgentOutcome, AgentToast};
use crate::commands::history::{record_capture, CaptureHandoff};
use crate::{windows, AppState};

/// What the card shows of one request. It holds a small picture, never what would be sent.
#[derive(Clone, Debug)]
pub struct Card {
    pub kind: AgentAskKind,
    pub target: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub thumbnail: Option<String>,
    pub boxes: Vec<[f64; 4]>,
}

pub struct AppHost {
    pub app: AppHandle,
}

fn kind_of(ask: &Ask) -> AgentAskKind {
    match ask {
        Ask::Window { .. } => AgentAskKind::Window,
        Ask::Display { .. } | Ask::ListDisplays => AgentAskKind::Display,
        Ask::Area { .. } => AgentAskKind::Area,
        Ask::Text { .. } => AgentAskKind::Text,
    }
}

fn capture_error(error: CaptureError) -> ToolError {
    match error {
        CaptureError::PermissionDenied => ToolError::PermissionMissing,
        other => ToolError::Failed(other.to_string()),
    }
}

/// The size an image leaves with: its long edge at most `max_edge`, never enlarged.
fn sent_size(size: PhysicalSize, max_edge: u32) -> PhysicalSize {
    let long = size.width.max(size.height).max(1);
    if long <= max_edge {
        return size;
    }
    let scale = max_edge as f64 / long as f64;
    PhysicalSize::new(
        ((size.width as f64 * scale).round() as u32).max(1),
        ((size.height as f64 * scale).round() as u32).max(1),
    )
}

/// The PNG a request sends: scaled to its long edge, and further until it fits in 5 MB.
fn png(frame: &Frame, max_edge: u32) -> Result<Vec<u8>, ToolError> {
    let mut size = sent_size(frame.size, max_edge);
    loop {
        let scaled = if size == frame.size { frame.clone() } else { frame.resized(size) };
        let bytes = scaled.encode_png(PngEffort::Fast).map_err(capture_error)?;
        if bytes.len() <= tools::MAX_IMAGE_BYTES || size.width.max(size.height) <= tools::MIN_EDGE {
            return Ok(bytes);
        }
        size = PhysicalSize::new((size.width * 3 / 4).max(1), (size.height * 3 / 4).max(1));
    }
}

fn read(app: &AppHandle, frame: Frame) -> Result<glimpsfy_ocr::OcrResult, ToolError> {
    crate::commands::ocr::recognize(app, std::sync::Arc::new(frame), None, None)
        .map_err(|error| ToolError::Failed(error.message))
}

/// What an allowed request sends, from the image it took.
pub(super) fn deliverable(app: &AppHandle, ask: &Ask, frame: Frame) -> Result<Vec<Content>, ToolError> {
    match ask {
        Ask::Text { .. } => Ok(vec![Content::Text(read(app, frame)?.text)]),
        Ask::Window { max_edge, .. } | Ask::Display { max_edge, .. } | Ask::Area { max_edge, .. } => {
            Ok(vec![Content::Png(png(&frame, *max_edge)?)])
        }
        Ask::ListDisplays => Err(ToolError::Failed("list_displays sends no image".into())),
    }
}

fn window_name(window: &WindowInfo) -> String {
    match window.title.as_deref().filter(|t| !t.is_empty()) {
        Some(title) => format!("{} · {title}", window.app_name),
        None => window.app_name.clone(),
    }
}

/// The frontmost window of `app` (by the name the Dock shows, or its bundle id), or of its windows
/// the frontmost whose title holds `title`.
fn find_window<'a>(windows: &'a [WindowInfo], app: &str, title: Option<&str>) -> Option<&'a WindowInfo> {
    let app = app.to_lowercase();
    let title = title.map(str::to_lowercase);
    windows
        .iter()
        .filter(|w| w.layer == 0 && w.alpha > 0.0 && w.is_on_screen)
        .filter(|w| w.app_name.to_lowercase() == app || w.bundle_id.as_deref().is_some_and(|b| b.to_lowercase() == app))
        .find(|w| match &title {
            Some(t) => w.title.as_deref().is_some_and(|wt| wt.to_lowercase().contains(t)),
            None => true,
        })
}

/// The display under the pointer, where a new request's card comes up.
pub(super) fn pointer_display(app: &AppHandle) -> Option<glimpsfy_capture::DisplayId> {
    let pointer = AppHost { app: app.clone() }.pointer();
    let displays = app.state::<AppState>().backend.displays().ok()?;
    displays.iter().find(|d| d.frame.contains(pointer)).map(|d| d.id)
}

impl AppHost {
    fn pointer(&self) -> LogicalPoint {
        #[cfg(target_os = "macos")]
        {
            crate::macos::pointer()
        }
        #[cfg(not(target_os = "macos"))]
        {
            LogicalPoint { x: 0.0, y: 0.0 }
        }
    }

    /// The image a request asks for, and the card's line naming it.
    fn take(&self, ask: &Ask) -> Result<(Frame, Option<String>), ToolError> {
        let backend = self.app.state::<AppState>().backend.clone();
        let options = CaptureOptions { shows_cursor: false, exclude_own_windows: true };
        match ask {
            Ask::Window { app, title, .. } => {
                let windows = backend.windows(WindowListOptions::default()).map_err(capture_error)?;
                let window = find_window(&windows, app, title.as_deref()).ok_or_else(|| {
                    let titled = title.as_ref().map(|t| format!(" whose title contains \"{t}\"")).unwrap_or_default();
                    ToolError::NotFound(format!("No window of {app}{titled} is on screen."))
                })?;
                let window_options = WindowCaptureOptions {
                    shows_cursor: false,
                    include_shadow: false,
                    keep_rounded_corners: true,
                    background: None,
                };
                let frame = backend.capture_window(window.id, &window_options).map_err(capture_error)?;
                Ok((frame, Some(window_name(window))))
            }
            Ask::Display { display, .. } => {
                let displays = backend.displays().map_err(capture_error)?;
                let pointer = self.pointer();
                let chosen = match display {
                    Some(id) => displays.iter().find(|d| d.id.0 == *id).ok_or_else(|| {
                        ToolError::NotFound(format!("No display has the id {id}; list_displays names them."))
                    })?,
                    None => displays
                        .iter()
                        .find(|d| d.frame.contains(pointer))
                        .or_else(|| displays.iter().find(|d| d.is_primary))
                        .or(displays.first())
                        .ok_or_else(|| ToolError::NotFound("No display is connected.".into()))?,
                };
                let frame = backend.capture_display(chosen.id, &options).map_err(capture_error)?;
                Ok((frame, Some(chosen.name.clone())))
            }
            Ask::Area { rect: Some(rect), .. } | Ask::Text { rect: Some(rect) } => {
                let area = LogicalRect { x: rect.x, y: rect.y, width: rect.width, height: rect.height };
                let frame = backend.capture_rect(area, &options).map_err(capture_error)?;
                let centre = LogicalPoint { x: rect.x + rect.width / 2.0, y: rect.y + rect.height / 2.0 };
                let under = backend.windows(WindowListOptions::default()).ok().and_then(|ws| {
                    ws.iter().find(|w| w.layer == 0 && w.alpha > 0.0 && w.bounds.contains(centre)).map(window_name)
                });
                let target = under.or_else(|| {
                    backend.displays().ok()?.into_iter().find(|d| d.frame.contains(centre)).map(|d| d.name)
                });
                Ok((frame, target))
            }
            Ask::Area { rect: None, .. } | Ask::Text { rect: None } | Ask::ListDisplays => {
                Err(ToolError::Failed("this request takes nothing before the user selects".into()))
            }
        }
    }
}

impl Host for AppHost {
    type Card = Card;

    fn hello(&self, connection: ConnectionId, client: &ClientInfo) {
        let state = agents(&self.app);
        if let Some(c) = state.locked().connections.get_mut(&connection) {
            c.agent = client.name.clone();
        }
        emit_state(&self.app);
    }

    fn bye(&self, connection: ConnectionId) {
        let state = agents(&self.app);
        if let Some(c) = state.locked().connections.get_mut(&connection) {
            c.connected = false;
        }
        emit_state(&self.app);
    }

    fn displays(&self) -> Result<Value, ToolError> {
        let backend = self.app.state::<AppState>().backend.clone();
        let displays = backend.displays().map_err(capture_error)?;
        Ok(Value::Array(
            displays
                .iter()
                .map(|d| {
                    json!({
                        "id": d.id.0,
                        "name": d.name,
                        "frame": { "x": d.frame.x, "y": d.frame.y, "width": d.frame.width, "height": d.frame.height },
                        "scale": d.scale.0,
                        "pixels": { "width": d.pixel_size.width, "height": d.pixel_size.height },
                        "primary": d.is_primary,
                    })
                })
                .collect(),
        ))
    }

    fn prepare(&self, connection: ConnectionId, ask: &Ask) -> Result<(Vec<Content>, Card), ToolError> {
        let backend = self.app.state::<AppState>().backend.clone();
        if !matches!(backend.permission(), PermissionState::Granted | PermissionState::NotRequired) {
            return Err(ToolError::PermissionMissing);
        }
        let kind = kind_of(ask);
        let mut card = Card { kind, target: None, width: None, height: None, thumbnail: None, boxes: Vec::new() };
        let content = if ask.is_pick() {
            Vec::new()
        } else {
            let (frame, target) = self.take(ask)?;
            card.target = target;
            card.thumbnail = crate::output::thumbnail(&frame);
            match ask {
                Ask::Text { rect: Some(rect) } => {
                    card.width = Some(rect.width.round() as u32);
                    card.height = Some(rect.height.round() as u32);
                    let result = read(&self.app, frame)?;
                    let (w, h) = (result.image_size.width.max(1) as f64, result.image_size.height.max(1) as f64);
                    card.boxes = result
                        .lines
                        .iter()
                        .take(60)
                        .map(|l| [l.bounds.x / w, l.bounds.y / h, l.bounds.width / w, l.bounds.height / h])
                        .collect();
                    vec![Content::Text(result.text)]
                }
                Ask::Window { max_edge, .. } | Ask::Display { max_edge, .. } | Ask::Area { max_edge, .. } => {
                    let size = sent_size(frame.size, *max_edge);
                    card.width = Some(size.width);
                    card.height = Some(size.height);
                    vec![Content::Png(png(&frame, *max_edge)?)]
                }
                _ => Vec::new(),
            }
        };
        let state = agents(&self.app);
        if let Some(c) = state.locked().connections.get_mut(&connection) {
            c.last = Some(card.clone());
        }
        Ok((content, card))
    }

    fn offer(&self, id: RequestId, _: ConnectionId, card: Card) {
        agents(&self.app).locked().cards.insert(id, card);
    }

    fn select(&self, id: RequestId, _: ConnectionId, ask: &Ask) -> Result<Vec<Content>, ToolError> {
        super::select(&self.app, id, ask)
    }

    fn finish(
        &self,
        id: Option<RequestId>,
        connection: ConnectionId,
        ask: &Ask,
        outcome: &Outcome,
        content: &[Content],
    ) {
        let app = &self.app;
        let now = now_ms();
        let midnight = super::local_midnight_ms();
        let (card, agent) = {
            let state = agents(app);
            let mut inner = state.locked();
            let card = id.and_then(|id| inner.cards.remove(&id));
            let connection = inner.connections.get_mut(&connection);
            let agent = connection.as_ref().and_then(|c| c.agent.clone());
            let last = connection.as_ref().and_then(|c| c.last.clone());
            if let Some(c) = connection {
                c.requests.retain(|at| *at >= midnight);
                c.requests.push(now);
            }
            (card.or(last), agent)
        };
        let outcome_name = match outcome {
            Outcome::Sent { granted: true, .. } => AgentOutcome::Timed,
            Outcome::Sent { .. } => AgentOutcome::Allowed,
            Outcome::Denied => AgentOutcome::Denied,
            Outcome::TimedOut => AgentOutcome::TimedOut,
            Outcome::Locked => AgentOutcome::Locked,
            Outcome::QueueFull => AgentOutcome::QueueFull,
            Outcome::Gone => AgentOutcome::Cancelled,
            Outcome::Failed(_) => AgentOutcome::Failed,
        };
        let entry = AgentLogEntry {
            at_ms: now,
            agent: agent.clone(),
            kind: kind_of(ask),
            target: card.as_ref().and_then(|c| c.target.clone()),
            outcome: outcome_name,
        };
        log::append(app, &entry);

        match outcome {
            Outcome::Sent { granted, .. } => {
                if *granted {
                    let state = agents(app);
                    let minutes = state.gate.grant_left(connection).map(|left| left.as_secs().div_ceil(60) as u32);
                    let mut toast = windows::Toast::new(windows::ToastKind::AgentSent);
                    toast.thumbnail = card.as_ref().and_then(|c| c.thumbnail.clone());
                    toast.agent = Some(AgentToast {
                        agent: agent.clone(),
                        connection,
                        kind: kind_of(ask),
                        minutes_left: minutes,
                        count: 1,
                        agents: Vec::new(),
                    });
                    windows::toast(app, None, toast);
                }
                record(app, agent.as_deref(), card.as_ref(), content);
            }
            Outcome::TimedOut => {
                let mut toast = windows::Toast::new(windows::ToastKind::AgentTimedOut);
                toast.thumbnail = card.as_ref().and_then(|c| c.thumbnail.clone());
                toast.agent = Some(AgentToast {
                    agent,
                    connection,
                    kind: kind_of(ask),
                    minutes_left: None,
                    count: 1,
                    agents: Vec::new(),
                });
                windows::toast(app, None, toast);
            }
            _ => {}
        }
        emit_state(app);
    }
}

/// Keeps what went to an agent in the history under the agent's name, when the history is on;
/// with the history off nothing of it reaches the disk.
fn record(app: &AppHandle, agent: Option<&str>, card: Option<&Card>, content: &[Content]) {
    for item in content {
        let mut handoff = CaptureHandoff {
            app_name: agent.map(str::to_string),
            created_at_ms: Some(now_ms()),
            ..CaptureHandoff::default()
        };
        match item {
            Content::Png(png) => {
                handoff.kind = Some(EntryKind::Screenshot);
                handoff.png = Some(png.clone());
                handoff.width = card.and_then(|c| c.width);
                handoff.height = card.and_then(|c| c.height);
                handoff.bytes = Some(png.len() as u64);
            }
            Content::Text(text) => {
                handoff.kind = Some(EntryKind::TextCopy);
                handoff.text = Some(text.clone());
            }
        }
        if let Err(error) = record_capture(app, handoff) {
            crate::trace::log(format!("[glimpsfy] agents: the history did not take the capture: {}", error.message));
        }
    }
}
