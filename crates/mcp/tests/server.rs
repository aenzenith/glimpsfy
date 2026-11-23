//! What reaches the agent's side of the socket: nothing a request would send goes out before the
//! user answers it, and nothing goes out after a denial, a timeout or a lock.

use std::io::{BufRead, BufReader, Write};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use glimpsfy_mcp::gate::{Answer, Clock, ConnectionId, Evidence, Gate, RequestId, ARM_AFTER};
use glimpsfy_mcp::server::{self, ClientInfo, Host, Outcome, ServerInfo};
use glimpsfy_mcp::tools::{Ask, Content, ToolError};
use serde_json::{json, Value};

/// What the fake capture and the fake recognition hold; neither may reach the output early.
const PIXELS: [u8; 6] = [0x89, b'P', b'N', b'G', 0xde, 0xad];
const SECRET: &str = "the quarterly numbers";

struct FakeClock {
    base: Instant,
    offset: Mutex<Duration>,
}

impl Clock for FakeClock {
    fn now(&self) -> Instant {
        self.base + *self.offset.lock().unwrap()
    }
}

struct FakeHost {
    offered: mpsc::Sender<RequestId>,
}

impl Host for FakeHost {
    type Card = ();
    fn hello(&self, _: ConnectionId, _: &ClientInfo) {}
    fn bye(&self, _: ConnectionId) {}
    fn displays(&self) -> Result<Value, ToolError> {
        Ok(json!([]))
    }
    fn prepare(&self, _: ConnectionId, ask: &Ask) -> Result<(Vec<Content>, ()), ToolError> {
        Ok(match ask {
            Ask::Text { .. } => (vec![Content::Text(SECRET.into())], ()),
            _ => (vec![Content::Png(PIXELS.to_vec())], ()),
        })
    }
    fn offer(&self, id: RequestId, _: ConnectionId, _: ()) {
        let _ = self.offered.send(id);
    }
    fn select(&self, _: RequestId, _: ConnectionId, _: &Ask) -> Result<Vec<Content>, ToolError> {
        Err(ToolError::UserDenied)
    }
    fn finish(&self, _: Option<RequestId>, _: ConnectionId, _: &Ask, _: &Outcome, _: &[Content]) {}
}

struct Session {
    gate: Arc<Gate>,
    clock: Arc<FakeClock>,
    input: std::io::PipeWriter,
    lines: Receiver<String>,
    offered: Receiver<RequestId>,
}

impl Session {
    fn start() -> Session {
        let clock = Arc::new(FakeClock { base: Instant::now(), offset: Mutex::new(Duration::ZERO) });
        let gate = Arc::new(Gate::new(clock.clone(), || {}));
        let (offer_tx, offered) = mpsc::channel();
        let host = Arc::new(FakeHost { offered: offer_tx });
        let (to_server, input) = std::io::pipe().unwrap();
        let (from_server, output) = std::io::pipe().unwrap();
        let served = gate.clone();
        std::thread::spawn(move || {
            server::serve(host, served, 1, ServerInfo { version: "test".into() }, to_server, output)
        });
        let (line_tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(from_server).lines() {
                let Ok(line) = line else { break };
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });
        let mut session = Session { gate, clock, input, lines, offered };
        session.send(json!({ "jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
            "protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": { "name": "Claude Code", "version": "2" }
        }}));
        let hello = session.next();
        assert_eq!(hello["result"]["protocolVersion"], "2025-06-18");
        session
    }

    fn send(&mut self, message: Value) {
        writeln!(self.input, "{message}").unwrap();
        self.input.flush().unwrap();
    }

    fn next(&self) -> Value {
        let line = self.lines.recv_timeout(Duration::from_secs(5)).expect("a response");
        serde_json::from_str(&line).unwrap()
    }

    /// Everything the server wrote so far, without waiting for more.
    fn written(&self) -> String {
        let mut all = String::new();
        while let Ok(line) = self.lines.recv_timeout(Duration::from_millis(150)) {
            all.push_str(&line);
        }
        all
    }

    fn call(&mut self, id: u64, tool: &str, arguments: Value) -> RequestId {
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": { "name": tool, "arguments": arguments } }));
        let id = self.offered.recv_timeout(Duration::from_secs(5)).expect("the card");
        // The server offers the card a moment before it opens the request; a user's answer always
        // comes later than that, so the test waits for the gate to hold the request open.
        let until = Instant::now() + Duration::from_secs(5);
        while !self.gate.view().queue.iter().any(|p| p.id == id) {
            assert!(Instant::now() < until, "the request was never opened");
            std::thread::sleep(Duration::from_millis(1));
        }
        id
    }

    /// The card shows the request long enough for an answer to count.
    fn arm(&self, request: RequestId) {
        self.gate.show(request);
        self.gate.visible(Some(request));
        *self.clock.offset.lock().unwrap() += ARM_AFTER;
    }
}

fn leaks(text: &str) -> bool {
    text.contains("\"image\"")
        || text.contains("iVBO")
        || text.contains(SECRET)
        || text.contains(&glimpsfy_mcp::rpc::base64(&PIXELS))
}

#[test]
fn nothing_leaves_before_the_answer_or_after_a_denial() {
    let mut session = Session::start();
    let image = session.call(1, "capture_window", json!({ "app": "Terminal" }));
    let text = session.call(2, "read_text", json!({ "rect": { "x": 0, "y": 0, "width": 100, "height": 40 } }));
    assert!(!leaks(&session.written()), "the requests wait with nothing written");

    session.arm(image);
    session.gate.answer(image, Answer::Deny, Evidence::Click { hardware: true }).unwrap();
    let denied = session.next();
    assert_eq!(denied["id"], 1);
    assert_eq!(denied["result"]["isError"], true);
    assert!(denied["result"]["content"][0]["text"].as_str().unwrap().starts_with("user_denied"));
    assert!(!leaks(&denied.to_string()));

    // A posted click answers nothing, so the text stays in the core.
    session.arm(text);
    assert!(session.gate.answer(text, Answer::Allow, Evidence::Click { hardware: false }).is_err());
    assert!(!leaks(&session.written()));
    session.gate.answer(text, Answer::Deny, Evidence::Key { held: true }).unwrap();
    let denied = session.next();
    assert_eq!(denied["id"], 2);
    assert!(!leaks(&denied.to_string()));
}

#[test]
fn a_timeout_and_a_lock_send_nothing() {
    let mut session = Session::start();
    let late = session.call(1, "capture_display", json!({}));
    *session.clock.offset.lock().unwrap() += glimpsfy_mcp::gate::ANSWER_WITHIN;
    let timed_out = session.next();
    assert!(timed_out["result"]["content"][0]["text"].as_str().unwrap().starts_with("timeout"));
    assert!(!leaks(&timed_out.to_string()));
    assert!(session.gate.answer(late, Answer::Allow, Evidence::Click { hardware: true }).is_err());

    let _waiting = session.call(2, "read_text", json!({ "rect": { "x": 0, "y": 0, "width": 10, "height": 10 } }));
    session.gate.lock_screen();
    let locked = session.next();
    assert!(locked["result"]["content"][0]["text"].as_str().unwrap().starts_with("screen_locked"));
    assert!(!leaks(&locked.to_string()));
}

#[test]
fn an_allowed_request_sends_what_the_card_showed() {
    let mut session = Session::start();
    let request = session.call(1, "capture_window", json!({ "app": "Terminal" }));
    session.arm(request);
    session.gate.answer(request, Answer::Allow, Evidence::Click { hardware: true }).unwrap();
    let sent = session.next();
    assert_eq!(sent["result"]["isError"], false);
    assert_eq!(sent["result"]["content"][0]["type"], "image");
    assert_eq!(sent["result"]["content"][0]["data"], glimpsfy_mcp::rpc::base64(&PIXELS));
}
