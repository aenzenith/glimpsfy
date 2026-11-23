//! Agents (D34): the local MCP server's place in the app. It owns the listener and its
//! connections, the cards of the waiting requests, the approval card's answers from clicks and
//! keys, the selection an agent asks the user for, the timed permissions' minutes in the menu bar,
//! and the log. The protocol, the tools and the gate itself are `crates/mcp`.
//!
//! Only macOS listens; elsewhere the setting stays off and nothing opens.

mod host;
#[cfg(target_os = "macos")]
mod listener;
pub mod log;

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glimpsfy_mcp::gate::{Answer, Evidence, Gate, Refused, RequestId, SystemClock};
use glimpsfy_mcp::tools::{Ask, Content, ToolError};
use tauri::{AppHandle, Emitter, Manager};

#[cfg(target_os = "macos")]
use crate::commands::agents::AgentToast;
use crate::commands::agents::{events, AgentConnection, AgentKeys, AgentQueue, AgentRequest, AgentsState};
use crate::commands::capture::{CloseReason, Selection};
use crate::commands::error::{CommandError, CommandResult, ErrorCode};
use crate::{shortcuts, trace, windows, AppState};

pub use host::Card;

/// A key of the approval card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalKey {
    Allow,
    Deny,
    Timed,
    Previous,
    Next,
}

/// The card's keys, registered only while a request waits (`shortcuts::bind_approval`).
pub const APPROVAL_KEYS: [(&str, ApprovalKey); 5] = [
    ("Alt+Shift+CommandOrControl+Y", ApprovalKey::Allow),
    ("Alt+Shift+CommandOrControl+N", ApprovalKey::Deny),
    ("Alt+Shift+CommandOrControl+T", ApprovalKey::Timed),
    ("Alt+Shift+CommandOrControl+ArrowLeft", ApprovalKey::Previous),
    ("Alt+Shift+CommandOrControl+ArrowRight", ApprovalKey::Next),
];

/// How recent the click that answers a request must be: the click and the page's call to
/// `agents_answer` follow each other within a frame or two.
#[cfg(target_os = "macos")]
const CLICK_FRESH: Duration = Duration::from_secs(2);
/// How long a selection the user was asked for may take to open before it counts as failed (a
/// capture already open keeps it from opening at all).
const PICK_OPENS_WITHIN: Duration = Duration::from_secs(4);
/// How long the user may take over a selection.
const PICK_LASTS: Duration = Duration::from_secs(10 * 60);

/// One agent that connected since the app started.
#[derive(Debug, Default)]
struct Connection {
    agent: Option<String>,
    process: Option<String>,
    connected: bool,
    /// When its requests came, in milliseconds since the Unix epoch (today's only).
    requests: Vec<i64>,
    /// The card of its last prepared request, for the toast and the log line of a capture a
    /// timed permission sends without a card.
    last: Option<Card>,
}

/// The selection an allowed request waits for.
struct Picking {
    request: RequestId,
    ask: Ask,
    done: mpsc::Sender<Result<Vec<Content>, ToolError>>,
}

#[derive(Default)]
struct Inner {
    next_connection: u64,
    connections: BTreeMap<u64, Connection>,
    cards: HashMap<RequestId, Card>,
    picking: Option<Picking>,
}

/// The agents' state, managed by Tauri.
pub struct Agents {
    gate: Arc<Gate>,
    inner: Mutex<Inner>,
    #[cfg(target_os = "macos")]
    listener: Mutex<Option<listener::Listener>>,
    listen_error: Mutex<Option<String>>,
    /// The approval keys and the input watch are on.
    armed: AtomicBool,
    /// The thread that keeps the menu bar's minutes current runs.
    ticking: AtomicBool,
}

/// The request a selection opening now belongs to; the capture takes it (`take_pick`).
static PICK: Mutex<Option<RequestId>> = Mutex::new(None);

fn agents(app: &AppHandle) -> tauri::State<'_, Agents> {
    app.state::<Agents>()
}

impl Agents {
    fn locked(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Makes the state, follows the session's locks and sleeps, and opens the socket when Settings
/// allows it. Once, at start.
pub fn setup(app: &AppHandle) {
    let handle = app.clone();
    let gate = Arc::new(Gate::new(Arc::new(SystemClock), move || changed(&handle)));
    app.manage(Agents {
        gate,
        inner: Mutex::new(Inner { next_connection: 1, ..Inner::default() }),
        #[cfg(target_os = "macos")]
        listener: Mutex::new(None),
        listen_error: Mutex::new(None),
        armed: AtomicBool::new(false),
        ticking: AtomicBool::new(false),
    });
    #[cfg(target_os = "macos")]
    {
        if crate::macos::approval::screen_locked() {
            agents(app).gate.lock_screen();
        }
        crate::macos::on_main(app, {
            let app = app.clone();
            move |mtm| crate::macos::approval::watch_session(&app, session_changed, mtm)
        });
    }
    let enabled = app.state::<AppState>().settings.get().agents.enabled;
    apply(app, enabled);
}

/// Opens or closes the socket after Settings › Agents changed.
pub fn apply(app: &AppHandle, enabled: bool) {
    #[cfg(target_os = "macos")]
    {
        let state = agents(app);
        let mut listener = state.listener.lock().unwrap_or_else(|p| p.into_inner());
        match (enabled, listener.is_some()) {
            (true, false) => match listener::Listener::start(app, state.gate.clone()) {
                Ok(started) => {
                    *listener = Some(started);
                    *state.listen_error.lock().unwrap_or_else(|p| p.into_inner()) = None;
                }
                Err(error) => {
                    trace::log(format!("[glimpsfy] agents: the socket did not open: {error}"));
                    *state.listen_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(error.to_string());
                }
            },
            (false, true) => {
                if let Some(old) = listener.take() {
                    old.stop();
                }
                *state.listen_error.lock().unwrap_or_else(|p| p.into_inner()) = None;
            }
            _ => {}
        }
    }
    #[cfg(not(target_os = "macos"))]
    let _ = enabled;
    emit_state(app);
}

/// The listener took a connection that passed the peer checks.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn connected(app: &AppHandle, process: Option<String>) -> u64 {
    let id = {
        let state = agents(app);
        let mut inner = state.locked();
        let id = inner.next_connection;
        inner.next_connection += 1;
        inner.connections.insert(id, Connection { process, connected: true, ..Connection::default() });
        id
    };
    emit_state(app);
    id
}

// ------------------------------------------------------------------------------ the gate's news

/// Runs after every change of the gate: the card, the keys, the input watch and the menu bar
/// follow it.
fn changed(app: &AppHandle) {
    let state = agents(app);
    let view = state.gate.view();
    let waiting = !view.queue.is_empty();
    let picking = state.locked().picking.is_some();
    // The first request's card comes up on the display the user is looking at, not on the one the
    // last capture was taken on; a toast already showing keeps its place.
    if waiting && !state.armed.load(Ordering::SeqCst) {
        if let Some(display) = host::pointer_display(app) {
            windows::toast_follow(display);
        }
    }
    arm(app, waiting, waiting || picking);
    if !view.grants.is_empty() {
        tick(app);
    }
    show_minutes(app, &view);
    // An emit from this thread reaches the page through the main thread's queue, behind the keys
    // `arm` just queued, so the queue is read there too: read here it would hold no keys and
    // overwrite the one `arm` sends after the keys settle.
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let _ = handle.emit(events::QUEUE_CHANGED, queue(&handle));
    });
    emit_state(app);
}

/// The approval keys while a request waits, and the input watch while a request waits or a
/// selection is open.
fn arm(app: &AppHandle, keys: bool, watch: bool) {
    let state = agents(app);
    let was = state.armed.swap(keys, Ordering::SeqCst);
    if keys != was {
        if keys {
            shortcuts::bind_approval(app);
        } else {
            shortcuts::unbind_approval(app);
        }
        // The keys settle on the main thread in order, so this runs after they did and the card
        // learns which of them Glimpsfy holds.
        let handle = app.clone();
        let _ = app.run_on_main_thread(move || {
            let _ = handle.emit(events::QUEUE_CHANGED, queue(&handle));
        });
    }
    #[cfg(target_os = "macos")]
    crate::macos::on_main(app, move |mtm| crate::macos::approval::watch_input(watch, mtm));
    #[cfg(not(target_os = "macos"))]
    let _ = watch;
}

/// The minutes the longest timed permission has left, beside the menu bar icon.
fn show_minutes(app: &AppHandle, view: &glimpsfy_mcp::gate::View) {
    let left = view.grants.iter().map(|(_, left)| *left).max();
    crate::tray::show_grant(app, left.map(|l| l.as_secs().div_ceil(60) as u32));
}

/// Keeps the menu bar's minutes current and ends a timed permission on time while one runs.
fn tick(app: &AppHandle) {
    if agents(app).ticking.swap(true, Ordering::SeqCst) {
        return;
    }
    let handle = app.clone();
    let spawned = std::thread::Builder::new().name("glimpsfy-agents-tick".into()).spawn(move || loop {
        std::thread::sleep(Duration::from_secs(5));
        let state = agents(&handle);
        let view = state.gate.view();
        show_minutes(&handle, &view);
        if view.grants.is_empty() {
            state.ticking.store(false, Ordering::SeqCst);
            emit_state(&handle);
            return;
        }
    });
    if spawned.is_err() {
        agents(app).ticking.store(false, Ordering::SeqCst);
    }
}

#[cfg(target_os = "macos")]
fn session_changed(app: &AppHandle, change: crate::macos::approval::SessionChange) {
    use crate::macos::approval::SessionChange;
    let state = agents(app);
    match change {
        SessionChange::Locked => state.gate.lock_screen(),
        SessionChange::Away => state.gate.sleep(),
        SessionChange::Unlocked => {
            let missed = state.gate.unlock_screen();
            if missed.is_empty() {
                return;
            }
            let mut names: Vec<String> = Vec::new();
            {
                let inner = state.locked();
                for connection in &missed {
                    let name = inner.connections.get(connection).and_then(|c| c.agent.clone());
                    if let Some(name) = name.filter(|n| !names.contains(n)) {
                        names.push(name);
                    }
                }
            }
            let mut toast = windows::Toast::new(windows::ToastKind::AgentLocked);
            toast.agent = Some(AgentToast {
                agent: None,
                connection: 0,
                kind: crate::commands::agents::AgentAskKind::Display,
                minutes_left: None,
                count: missed.len() as u32,
                agents: names,
            });
            windows::toast(app, None, toast);
        }
    }
}

// ------------------------------------------------------------------------------------ answers

/// A click on one of the card's buttons. It counts only when the click that led to it came from
/// the hardware, to the toast's own window, a moment ago.
pub fn answer_click(app: &AppHandle, id: u64, answer: Answer) -> CommandResult<()> {
    let hardware = clicked_card(app);
    let decided = agents(app).gate.answer(id, answer, Evidence::Click { hardware });
    decided.map(|_| ()).map_err(refusal)
}

#[cfg(target_os = "macos")]
fn clicked_card(app: &AppHandle) -> bool {
    let Some(input) = crate::macos::approval::last_input() else {
        return false;
    };
    let Some(toast) = app.get_webview_window(windows::TOAST) else {
        return false;
    };
    // A synchronous command runs on the main thread, where AppKit may be asked.
    let Some(mtm) = objc2::MainThreadMarker::new() else {
        return false;
    };
    let window = crate::macos::approval::window_number(&toast, mtm);
    input.hardware && input.at.elapsed() <= CLICK_FRESH && window == Some(input.window)
}

#[cfg(not(target_os = "macos"))]
fn clicked_card(_: &AppHandle) -> bool {
    false
}

/// An approval key went down. The chord counts only when the keyboard itself holds it.
pub fn key(app: &AppHandle, key: ApprovalKey) {
    let state = agents(app);
    let answer = match key {
        ApprovalKey::Previous | ApprovalKey::Next => {
            state.gate.step(key == ApprovalKey::Next);
            return;
        }
        ApprovalKey::Allow => Answer::Allow,
        ApprovalKey::Deny => Answer::Deny,
        ApprovalKey::Timed => Answer::AllowTimed,
    };
    let Some(shown) = state.gate.view().shown else {
        return;
    };
    #[cfg(target_os = "macos")]
    let held = {
        use crate::macos::approval as a;
        a::chord_held(match key {
            ApprovalKey::Allow => a::KEY_Y,
            ApprovalKey::Deny => a::KEY_N,
            _ => a::KEY_T,
        })
    };
    #[cfg(not(target_os = "macos"))]
    let held = false;
    if let Err(refused) = state.gate.answer(shown, answer, Evidence::Key { held }) {
        trace::log(format!("[glimpsfy] agents: the key {key:?} answered nothing: {refused:?}"));
    }
}

fn refusal(refused: Refused) -> CommandError {
    let message = match refused {
        Refused::NotWaiting => "the request no longer waits",
        Refused::NotShown => "the card shows another request",
        Refused::NotArmed => "the card has not shown the request for long enough",
        Refused::Synthetic => "the click did not come from the user's hardware",
    };
    CommandError::new(ErrorCode::NotConfirmed, message)
}

/// Shows request `id` on the card.
pub fn show(app: &AppHandle, id: u64) -> bool {
    agents(app).gate.show(id)
}

/// The toast page reported what the card shows fully at this frame.
pub fn card_visible(app: &AppHandle, shown: Option<u64>) {
    if let Some(state) = app.try_state::<Agents>() {
        state.gate.visible(shown);
    }
}

pub fn revoke(app: &AppHandle, connection: u64) -> bool {
    agents(app).gate.revoke(connection)
}

pub fn disconnect(app: &AppHandle, connection: u64) -> bool {
    #[cfg(target_os = "macos")]
    {
        // The connection's thread ends its requests and its permission as it leaves.
        agents(app).listener.lock().unwrap_or_else(|p| p.into_inner()).as_ref().is_some_and(|l| l.close(connection))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, connection);
        false
    }
}

// -------------------------------------------------------------------------- the user's selection

/// The request a capture that is opening now selects for; the capture takes it once.
pub fn take_pick() -> Option<u64> {
    PICK.lock().unwrap_or_else(|p| p.into_inner()).take()
}

/// Opens the selection an allowed request asks the user for and waits for it.
fn select(app: &AppHandle, request: RequestId, ask: &Ask) -> Result<Vec<Content>, ToolError> {
    let (done, result) = mpsc::channel();
    {
        let state = agents(app);
        let mut inner = state.locked();
        if inner.picking.is_some() {
            return Err(ToolError::Failed("another agent's selection is open".into()));
        }
        inner.picking = Some(Picking { request, ask: ask.clone(), done });
    }
    arm(app, !agents(app).gate.view().queue.is_empty(), true);
    *PICK.lock().unwrap_or_else(|p| p.into_inner()) = Some(request);
    let opened = crate::session::start_now(app, crate::commands::capture::CaptureMode::Area, Instant::now());
    let outcome = match opened {
        Err(error) => Err(ToolError::Failed(error.message)),
        Ok(()) => wait_for_selection(app, request, &result),
    };
    *PICK.lock().unwrap_or_else(|p| p.into_inner()) = None;
    agents(app).locked().picking = None;
    changed(app);
    outcome
}

fn wait_for_selection(
    app: &AppHandle,
    request: RequestId,
    result: &mpsc::Receiver<Result<Vec<Content>, ToolError>>,
) -> Result<Vec<Content>, ToolError> {
    let started = Instant::now();
    loop {
        match result.recv_timeout(Duration::from_millis(250)) {
            Ok(outcome) => return outcome,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(ToolError::UserDenied),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        let opened = PICK.lock().unwrap_or_else(|p| p.into_inner()).is_none();
        let open = app.state::<AppState>().sessions.current().is_some_and(|s| s.agent == Some(request));
        if !opened && started.elapsed() > PICK_OPENS_WITHIN {
            return Err(ToolError::Failed("the selection could not open; another capture may be open".into()));
        }
        if opened && !open && started.elapsed() > PICK_OPENS_WITHIN {
            // The capture closed without telling (it failed): nothing was selected.
            return Err(ToolError::UserDenied);
        }
        if started.elapsed() > PICK_LASTS {
            if let Some(session) = app.state::<AppState>().sessions.current().filter(|s| s.agent == Some(request)) {
                crate::session::close(app, session.id, CloseReason::Cancelled);
            }
            return Err(ToolError::Timeout);
        }
    }
}

/// The overlay finished the selection: what lies in it goes to the waiting request.
pub fn take_selection(app: &AppHandle, selection: Selection) -> CommandResult<()> {
    let session = app
        .state::<AppState>()
        .sessions
        .get(selection.capture_id)
        .ok_or_else(|| CommandError::not_found(format!("capture #{}", selection.capture_id.0)))?;
    let request = session.agent.ok_or_else(|| CommandError::invalid("this capture was not opened for an agent"))?;
    if !selected_by_hand() {
        return Err(CommandError::new(ErrorCode::NotConfirmed, "the selection did not come from the user's hardware"));
    }
    let ask = {
        let state = agents(app);
        let inner = state.locked();
        match &inner.picking {
            Some(picking) if picking.request == request => picking.ask.clone(),
            _ => return Err(CommandError::not_found("no agent waits for this selection")),
        }
    };
    let (frame, _) = crate::output::take(app, &selection)?;
    let content = host::deliverable(app, &ask, frame);
    crate::session::close(app, selection.capture_id, CloseReason::Agent);
    let state = agents(app);
    let inner = state.locked();
    if let Some(picking) = inner.picking.as_ref().filter(|p| p.request == request) {
        let _ = picking.done.send(content);
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn selected_by_hand() -> bool {
    crate::macos::approval::last_input().is_some_and(|input| input.hardware && input.at.elapsed() <= CLICK_FRESH)
}

#[cfg(not(target_os = "macos"))]
fn selected_by_hand() -> bool {
    false
}

/// The selection's capture closed another way (Esc, Close): the request is denied.
pub fn selection_cancelled(app: &AppHandle, request: u64) {
    let Some(state) = app.try_state::<Agents>() else {
        return;
    };
    let inner = state.locked();
    if let Some(picking) = inner.picking.as_ref().filter(|p| p.request == request) {
        let _ = picking.done.send(Err(ToolError::UserDenied));
    }
}

// ----------------------------------------------------------------------------- what pages read

fn queue(app: &AppHandle) -> AgentQueue {
    let state = agents(app);
    let view = state.gate.view();
    let inner = state.locked();
    let requests = view
        .queue
        .iter()
        .filter_map(|pending| {
            let card = inner.cards.get(&pending.id)?;
            let connection = inner.connections.get(&pending.connection);
            Some(AgentRequest {
                id: pending.id,
                connection: pending.connection,
                agent: connection.and_then(|c| c.agent.clone()),
                process: connection.and_then(|c| c.process.clone()),
                kind: card.kind,
                pick: pending.pick,
                target: card.target.clone(),
                width: card.width,
                height: card.height,
                thumbnail: card.thumbnail.clone(),
                boxes: card.boxes.clone(),
                left_ms: pending.left.as_millis() as u64,
            })
        })
        .collect();
    let held = shortcuts::approval_keys();
    let key = |which: ApprovalKey| {
        held.contains(&which)
            .then(|| APPROVAL_KEYS.iter().find(|(_, k)| *k == which).map(|(a, _)| a.to_string()))
            .flatten()
    };
    AgentQueue {
        requests,
        shown: view.shown,
        keys: AgentKeys {
            allow: key(ApprovalKey::Allow),
            deny: key(ApprovalKey::Deny),
            timed: key(ApprovalKey::Timed),
            previous: key(ApprovalKey::Previous),
            next: key(ApprovalKey::Next),
        },
    }
}

/// Settings › Agents.
pub fn state(app: &AppHandle) -> AgentsState {
    let Some(state) = app.try_state::<Agents>() else {
        return AgentsState {
            available: cfg!(target_os = "macos"),
            enabled: false,
            listening: false,
            error: None,
            command: command_line(),
            connections: Vec::new(),
            queue: AgentQueue::default(),
        };
    };
    let view = state.gate.view();
    let midnight = local_midnight_ms();
    let connections = state
        .locked()
        .connections
        .iter()
        .map(|(id, c)| AgentConnection {
            connection: *id,
            agent: c.agent.clone(),
            process: c.process.clone(),
            connected: c.connected,
            requests_today: c.requests.iter().filter(|at| **at >= midnight).count() as u32,
            last_request_ms: c.requests.last().copied(),
            grant_left_ms: view.grants.iter().find(|(g, _)| g == id).map(|(_, left)| left.as_millis() as u64),
        })
        .collect();
    #[cfg(target_os = "macos")]
    let listening = state.listener.lock().unwrap_or_else(|p| p.into_inner()).is_some();
    #[cfg(not(target_os = "macos"))]
    let listening = false;
    let error = state.listen_error.lock().unwrap_or_else(|p| p.into_inner()).clone();
    AgentsState {
        available: cfg!(target_os = "macos"),
        enabled: app.state::<AppState>().settings.get().agents.enabled,
        listening,
        error,
        command: command_line(),
        connections,
        queue: queue(app),
    }
}

fn emit_state(app: &AppHandle) {
    let _ = app.emit(events::CHANGED, state(app));
}

/// The line an agent's settings take: Claude Code's form, which Cursor and Zed take apart into a
/// command and its argument.
pub fn command_line() -> String {
    let name = if cfg!(debug_assertions) { "glimpsfy-dev" } else { "glimpsfy" };
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "glimpsfy".into());
    let exe = if exe.contains(char::is_whitespace) { format!("'{exe}'") } else { exe };
    format!("claude mcp add {name} -- {exe} mcp")
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn local_midnight_ms() -> i64 {
    use chrono::{Local, TimeZone};
    let today = Local::now().date_naive().and_hms_opt(0, 0, 0);
    today.and_then(|t| Local.from_local_datetime(&t).earliest()).map(|t| t.timestamp_millis()).unwrap_or(0)
}
