//! Agent commands: the approval card's answers, the connected agents, their timed permissions and
//! the log of requests. Owner: agents (D34). The server, the gate and the listener behind them are
//! `src-tauri/src/agents/` and `crates/mcp`.

use serde::Serialize;
use tauri::{AppHandle, WebviewWindow};

use super::capture::Selection;
use super::error::{CommandError, CommandResult, ErrorCode};
use crate::agents;

pub use glimpsfy_mcp::gate::Answer;

/// What a request asks for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentAskKind {
    Window,
    Display,
    Area,
    /// The text in an area; no image goes.
    Text,
}

/// One request as the card shows it. It never holds what would be sent: the thumbnail is a small
/// picture of it, and the text of a text request stays in the core until the user allows it.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRequest {
    pub id: u64,
    pub connection: u64,
    /// The name the agent gave itself (`clientInfo`), which the user may not take on trust.
    pub agent: Option<String>,
    /// What the system says about the agent's process, such as "Terminal · ~/projects/glimpsfy";
    /// the card shows it in the name's tip.
    pub process: Option<String>,
    pub kind: AgentAskKind,
    /// The user selects the area: the card's Allow becomes Select area.
    pub pick: bool,
    /// The window, display or app under the area, such as "Terminal · glimpsfy — zsh".
    pub target: Option<String>,
    /// The size of what would be sent: the image in pixels, or a text request's area in points.
    pub width: Option<u32>,
    pub height: Option<u32>,
    /// A PNG data URL of at most 152 × 104 of what would be sent (the area, for text).
    pub thumbnail: Option<String>,
    /// A text request's recognised lines over the thumbnail, as fractions of its width and
    /// height: `[x, y, width, height]`.
    pub boxes: Vec<[f64; 4]>,
    /// The time left before it times out.
    pub left_ms: u64,
}

/// The card's keys, as accelerators; a key another app or action holds is `None` and leaves the
/// card.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentKeys {
    pub allow: Option<String>,
    pub deny: Option<String>,
    pub timed: Option<String>,
    pub previous: Option<String>,
    pub next: Option<String>,
}

/// The requests waiting for the user, oldest first, with the one the card shows.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentQueue {
    pub requests: Vec<AgentRequest>,
    pub shown: Option<u64>,
    pub keys: AgentKeys,
}

/// One agent that connected since the app started.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConnection {
    pub connection: u64,
    pub agent: Option<String>,
    pub process: Option<String>,
    pub connected: bool,
    /// Its requests since midnight.
    pub requests_today: u32,
    /// Its last request, in milliseconds since the Unix epoch.
    pub last_request_ms: Option<i64>,
    /// The time its timed permission has left.
    pub grant_left_ms: Option<u64>,
}

/// Settings › Agents.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsState {
    /// The feature exists on this platform (macOS only).
    pub available: bool,
    /// Settings › Agents › Let agents send requests.
    pub enabled: bool,
    /// The socket is open.
    pub listening: bool,
    /// Why the socket could not be opened, in English for the log line.
    pub error: Option<String>,
    /// The line an agent's settings take, such as `claude mcp add glimpsfy -- … mcp`.
    pub command: String,
    pub connections: Vec<AgentConnection>,
    pub queue: AgentQueue,
}

/// How a request ended, in the log.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AgentOutcome {
    /// The user allowed it on the card.
    Allowed,
    /// A timed permission sent it.
    Timed,
    Denied,
    TimedOut,
    Locked,
    QueueFull,
    /// The agent cancelled it or went away.
    Cancelled,
    Failed,
}

/// One line of the log.
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentLogEntry {
    pub at_ms: i64,
    pub agent: Option<String>,
    pub kind: AgentAskKind,
    pub target: Option<String>,
    pub outcome: AgentOutcome,
}

/// What an agent's toast tells: `agentSent` (a timed permission sent a capture), `agentTimedOut`
/// (a request nobody answered) and `agentLocked` (requests refused while the screen was locked,
/// told once after it unlocked).
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToast {
    pub agent: Option<String>,
    /// The connection whose timed permission the toast's lock button takes back.
    pub connection: u64,
    pub kind: AgentAskKind,
    /// The minutes the timed permission has left, rounded up.
    pub minutes_left: Option<u32>,
    /// How many requests the screen lock refused.
    pub count: u32,
    /// The names of the agents whose requests it refused, each once.
    pub agents: Vec<String>,
}

/// Event names of the agents area and their payloads.
pub mod events {
    /// The waiting requests, the shown one or the card's keys changed, to every window.
    /// Payload: [`super::AgentQueue`].
    pub const QUEUE_CHANGED: &str = "agents://queue-changed";
    /// An agent connected or left, a timed permission began or ended, or the socket opened or
    /// closed. Payload: [`super::AgentsState`].
    pub const CHANGED: &str = "agents://changed";
    /// A line was added to the log or the log was cleared. No payload.
    pub const LOG_CHANGED: &str = "agents://log-changed";
}

/// The page that may answer a request: the toast window, which holds the card.
const CARD_WINDOW: &str = crate::windows::TOAST;

/// The agents, the waiting requests and the command line.
#[tauri::command]
pub fn agents_state(app: AppHandle) -> AgentsState {
    agents::state(&app)
}

/// The user answered the request the card shows. Only the card's own window may answer, after
/// the card has shown it for 0.6 s, with a click the hardware made (`agents::answer`).
#[tauri::command]
pub fn agents_answer(app: AppHandle, window: WebviewWindow, id: u64, answer: Answer) -> CommandResult<()> {
    refuse_other_window(window.label(), "answer a request")?;
    agents::answer_click(&app, id, answer)
}

/// Shows another waiting request on the card (‹ ›).
#[tauri::command]
pub fn agents_show(window: WebviewWindow, app: AppHandle, id: u64) -> CommandResult<bool> {
    refuse_other_window(window.label(), "choose the shown request")?;
    Ok(agents::show(&app, id))
}

/// The overlay finished the selection a request asked the user for; what lies in it goes to the
/// agent, and the capture closes.
#[tauri::command]
pub async fn agents_take_selection(app: AppHandle, window: WebviewWindow, selection: Selection) -> CommandResult<()> {
    if crate::windows::overlay_display(window.label()).is_none() {
        return Err(CommandError::new(ErrorCode::InvalidArgument, "only the overlay hands a selection to an agent"));
    }
    tauri::async_runtime::spawn_blocking(move || agents::take_selection(&app, selection))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))?
}

/// Ends a connection's timed permission.
#[tauri::command]
pub fn agents_revoke(app: AppHandle, connection: u64) -> bool {
    agents::revoke(&app, connection)
}

/// Closes a connection; its waiting requests end and its agent must connect again.
#[tauri::command]
pub fn agents_disconnect(app: AppHandle, connection: u64) -> bool {
    agents::disconnect(&app, connection)
}

/// The log of the last 7 days, newest first.
#[tauri::command]
pub async fn agents_log(app: AppHandle) -> CommandResult<Vec<AgentLogEntry>> {
    tauri::async_runtime::spawn_blocking(move || agents::log::read(&app))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))?
}

#[tauri::command]
pub async fn agents_clear_log(app: AppHandle) -> CommandResult<()> {
    tauri::async_runtime::spawn_blocking(move || agents::log::clear(&app))
        .await
        .map_err(|e| CommandError::internal(e.to_string()))?
}

/// The line an agent's settings take to start the server.
#[tauri::command]
pub fn agents_command() -> String {
    agents::command_line()
}

/// A request is answered from the card's window alone: a page that any other window loads
/// (Settings, the editor) never approves anything.
fn refuse_other_window(label: &str, what: &str) -> CommandResult<()> {
    if label == CARD_WINDOW {
        Ok(())
    } else {
        Err(CommandError::new(ErrorCode::InvalidArgument, format!("only the approval card may {what}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_cards_window_answers() {
        assert!(refuse_other_window(crate::windows::TOAST, "answer").is_ok());
        for other in ["settings", "editor", "overlay-1", "popover", "pin-3", "history"] {
            assert!(refuse_other_window(other, "answer").is_err(), "{other}");
        }
    }
}
