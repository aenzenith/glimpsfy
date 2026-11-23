//! One connection's MCP session: `initialize`, `ping`, `tools/list`, `tools/call` and the
//! notifications, with every call that reads the screen sent through the gate.
//!
//! The app is the [`Host`]: it lists the displays, prepares what a request would send (the
//! capture is taken when the request arrives, so the card shows exactly what goes), puts the card
//! up, lets the user select an area, and records the outcome. What a request would send stays on
//! the thread that serves it and is written to the socket only after the gate allowed it.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use crate::gate::{Admission, ConnectionId, Decision, Gate, RequestId};
use crate::rpc::{self, Incoming, Line, RpcError};
use crate::tools::{self, Ask, Content, ToolError};

/// How many calls of one connection run at once; more wait for nothing and are refused.
const IN_FLIGHT: usize = 8;

/// What the agent says about itself in `initialize`, cleaned up for the card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClientInfo {
    pub name: Option<String>,
    pub version: Option<String>,
}

/// What became of a request, for the log, the history and the toasts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Sent to the agent; `granted` when a timed permission sent it without a card.
    Sent {
        granted: bool,
        timed: bool,
    },
    Denied,
    TimedOut,
    Locked,
    QueueFull,
    /// The agent cancelled it, or its connection closed.
    Gone,
    Failed(ToolError),
}

/// The app behind the server.
pub trait Host: Send + Sync + 'static {
    /// What the card shows of one request; it never holds what would be sent.
    type Card: Send;

    /// The agent introduced itself.
    fn hello(&self, connection: ConnectionId, client: &ClientInfo);
    /// The connection closed.
    fn bye(&self, connection: ConnectionId);
    /// The displays as JSON, for `list_displays`.
    fn displays(&self) -> Result<Value, ToolError>;
    /// Takes now what `ask` would send, and the card's picture of it. A request whose area the
    /// user selects takes nothing yet.
    fn prepare(&self, connection: ConnectionId, ask: &Ask) -> Result<(Vec<Content>, Self::Card), ToolError>;
    /// Keeps the card of request `id` before the gate shows it.
    fn offer(&self, id: RequestId, connection: ConnectionId, card: Self::Card);
    /// Lets the user select the area of an allowed request and takes what it asks there; the
    /// user's cancel denies it.
    fn select(&self, id: RequestId, connection: ConnectionId, ask: &Ask) -> Result<Vec<Content>, ToolError>;
    /// A request ended; `content` is what went to the agent (empty unless it was sent).
    fn finish(
        &self,
        id: Option<RequestId>,
        connection: ConnectionId,
        ask: &Ask,
        outcome: &Outcome,
        content: &[Content],
    );
}

/// This server's name and version for `initialize`.
#[derive(Clone, Debug)]
pub struct ServerInfo {
    pub version: String,
}

const INSTRUCTIONS: &str = "Glimpsfy takes screenshots and reads text on this Mac for you. Every capture waits for the user's approval in a card in the corner of the screen, so a call may take up to a minute; a denied, timed-out or locked request returns an error and should not be retried at once. Call list_displays first to learn the displays' frames when you pass a rectangle.";

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

fn send(writer: &Writer, message: &Value) {
    let mut line = serde_json::to_vec(message).unwrap_or_default();
    line.push(b'\n');
    let mut writer = writer.lock().unwrap_or_else(|p| p.into_inner());
    // A closed socket ends the session through the reader; nothing to do here.
    let _ = writer.write_all(&line).and_then(|()| writer.flush());
}

fn clean(text: Option<&str>, max: usize) -> Option<String> {
    let text: String = text?.chars().filter(|c| !c.is_control()).take(max).collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// The calls of one connection that are still running, by their JSON-RPC id, with the gate's
/// request once it has one, so a cancel can withdraw it.
type Flights = Arc<Mutex<HashMap<String, Option<RequestId>>>>;

/// Serves one connection until the client closes it. Blocks the calling thread; each call runs on
/// a thread of its own, since one may wait a minute for the user.
pub fn serve<H: Host>(
    host: Arc<H>,
    gate: Arc<Gate>,
    connection: ConnectionId,
    info: ServerInfo,
    reader: impl Read,
    writer: impl Write + Send + 'static,
) {
    let writer: Writer = Arc::new(Mutex::new(Box::new(writer)));
    let flights: Flights = Arc::new(Mutex::new(HashMap::new()));
    let mut reader = BufReader::new(reader);
    let mut initialized = false;

    loop {
        let line = match rpc::read_line(&mut reader) {
            Ok(Line::Message(line)) => line,
            Ok(Line::TooLong) => {
                send(
                    &writer,
                    &rpc::error(&Value::Null, &RpcError::new(rpc::INVALID_REQUEST, "the message is too long")),
                );
                continue;
            }
            Ok(Line::End) | Err(_) => break,
        };
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let (id, method, params) = match rpc::parse(&line) {
            Ok(Incoming::Request { id, method, params }) => (id, method, params),
            Ok(Incoming::Notification { method, params }) => {
                if method == "notifications/cancelled" {
                    let key = params.get("requestId").map(Value::to_string);
                    let request =
                        key.and_then(|k| flights.lock().unwrap_or_else(|p| p.into_inner()).get(&k).copied().flatten());
                    if let Some(request) = request {
                        gate.withdraw(request);
                    }
                }
                continue;
            }
            Ok(Incoming::Response) => continue,
            Err((id, error)) => {
                send(&writer, &rpc::error(&id, &error));
                continue;
            }
        };
        match method.as_str() {
            "initialize" => {
                let client = params.get("clientInfo");
                let client = ClientInfo {
                    name: clean(client.and_then(|c| c.get("name")).and_then(Value::as_str), 64),
                    version: clean(client.and_then(|c| c.get("version")).and_then(Value::as_str), 32),
                };
                host.hello(connection, &client);
                initialized = true;
                let version = rpc::negotiate(params.get("protocolVersion").and_then(Value::as_str));
                send(
                    &writer,
                    &rpc::result(
                        &id,
                        json!({
                            "protocolVersion": version,
                            "capabilities": { "tools": { "listChanged": false } },
                            "serverInfo": { "name": "glimpsfy", "title": "Glimpsfy", "version": info.version },
                            "instructions": INSTRUCTIONS,
                        }),
                    ),
                );
            }
            "ping" => send(&writer, &rpc::result(&id, json!({}))),
            _ if !initialized => {
                send(&writer, &rpc::error(&id, &RpcError::new(rpc::NOT_INITIALIZED, "initialize comes first")));
            }
            "tools/list" => send(&writer, &rpc::result(&id, tools::list())),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
                let arguments = params.get("arguments").cloned().unwrap_or(Value::Null);
                let ask = match tools::parse(name, &arguments) {
                    Ok(ask) => ask,
                    Err(error) if matches!(error, ToolError::InvalidArguments(ref m) if m.starts_with("no tool")) => {
                        send(&writer, &rpc::error(&id, &RpcError::new(rpc::INVALID_PARAMS, error.message())));
                        continue;
                    }
                    Err(error) => {
                        send(&writer, &rpc::result(&id, tools::failure(&error)));
                        continue;
                    }
                };
                let key = id.to_string();
                {
                    let mut flights = flights.lock().unwrap_or_else(|p| p.into_inner());
                    if flights.len() >= IN_FLIGHT || flights.contains_key(&key) {
                        drop(flights);
                        send(&writer, &rpc::result(&id, tools::failure(&ToolError::QueueFull)));
                        continue;
                    }
                    flights.insert(key.clone(), None);
                }
                let worker = {
                    let (host, gate, writer, flights, id) =
                        (host.clone(), gate.clone(), writer.clone(), flights.clone(), id.clone());
                    std::thread::Builder::new().name("glimpsfy-agent-call".into()).spawn(move || {
                        let answer = call(&*host, &gate, connection, &ask, &flights, &key);
                        flights.lock().unwrap_or_else(|p| p.into_inner()).remove(&key);
                        // A cancelled request gets no response, as MCP asks.
                        if let Some(result) = answer {
                            send(&writer, &rpc::result(&id, result));
                        }
                    })
                };
                if let Err(error) = worker {
                    flights.lock().unwrap_or_else(|p| p.into_inner()).remove(&id.to_string());
                    send(&writer, &rpc::error(&id, &RpcError::new(rpc::INTERNAL_ERROR, error.to_string())));
                }
            }
            other => {
                send(&writer, &rpc::error(&id, &RpcError::new(rpc::METHOD_NOT_FOUND, format!("no method `{other}`"))))
            }
        }
    }

    // The client is gone: its requests leave the card and its timed permission ends. A call that
    // is still running (the user selecting an area) finishes on its own thread.
    gate.disconnect(connection);
    host.bye(connection);
}

/// Runs one tool call; `None` when it was cancelled.
fn call<H: Host>(
    host: &H,
    gate: &Gate,
    connection: ConnectionId,
    ask: &Ask,
    flights: &Flights,
    key: &str,
) -> Option<Value> {
    if !ask.needs_approval() {
        return Some(match host.displays() {
            Ok(displays) => tools::success(&[Content::Text(displays.to_string())]),
            Err(error) => tools::failure(&error),
        });
    }
    let end = |id: Option<RequestId>, outcome: Outcome, content: &[Content]| {
        host.finish(id, connection, ask, &outcome, content);
        Some(match outcome {
            Outcome::Sent { .. } => tools::success(content),
            Outcome::Denied => tools::failure(&ToolError::UserDenied),
            Outcome::TimedOut => tools::failure(&ToolError::Timeout),
            Outcome::Locked => tools::failure(&ToolError::ScreenLocked),
            Outcome::QueueFull => tools::failure(&ToolError::QueueFull),
            Outcome::Gone => return None,
            Outcome::Failed(error) => tools::failure(&error),
        })
    };

    let id = match gate.admit(connection, ask.is_pick()) {
        Admission::Locked => return end(None, Outcome::Locked, &[]),
        Admission::QueueFull => return end(None, Outcome::QueueFull, &[]),
        Admission::Granted => {
            return match host.prepare(connection, ask) {
                Ok((content, _)) => end(None, Outcome::Sent { granted: true, timed: true }, &content),
                Err(error) => end(None, Outcome::Failed(error), &[]),
            };
        }
        Admission::Queued(id) => id,
    };
    if let Some(flight) = flights.lock().unwrap_or_else(|p| p.into_inner()).get_mut(key) {
        *flight = Some(id);
    }
    let content = match host.prepare(connection, ask) {
        Ok((content, card)) => {
            host.offer(id, connection, card);
            content
        }
        Err(error) => {
            gate.withdraw(id);
            return end(Some(id), Outcome::Failed(error), &[]);
        }
    };
    let decision = match gate.open(id) {
        Ok(()) => gate.wait(id),
        Err(decision) => decision,
    };
    match decision {
        Decision::Allow | Decision::AllowTimed => {
            let timed = decision == Decision::AllowTimed;
            if ask.is_pick() {
                match host.select(id, connection, ask) {
                    Ok(content) => end(Some(id), Outcome::Sent { granted: false, timed }, &content),
                    Err(ToolError::UserDenied) => end(Some(id), Outcome::Denied, &[]),
                    Err(error) => end(Some(id), Outcome::Failed(error), &[]),
                }
            } else {
                end(Some(id), Outcome::Sent { granted: false, timed }, &content)
            }
        }
        Decision::Deny => end(Some(id), Outcome::Denied, &[]),
        Decision::Timeout => end(Some(id), Outcome::TimedOut, &[]),
        Decision::Locked => end(Some(id), Outcome::Locked, &[]),
        Decision::Gone => end(Some(id), Outcome::Gone, &[]),
    }
}
