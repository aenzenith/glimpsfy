//! The dev-only driver: it runs scripted steps against the real app so a flow can be exercised
//! without a mouse. Debug builds compile it; it starts only when `GLIMPSFY_DRIVER=<folder>` is
//! set, and release builds have no trace of it.
//!
//! A client (`scripts/driver.py`) drops one JSON request per file into `<folder>/in/` and reads
//! the answer from `<folder>/out/` under the same name. Every app event is appended to
//! `<folder>/events.jsonl` with its time, so a script can check what the core said.
//!
//! The pointer and keys a script sends are synthesised inside the page (`eval` with the helper
//! library in `PRELUDE`): they run the page's real handlers and its real IPC, but they do not
//! test how macOS delivers real input (key focus of a non-activating panel, global shortcuts,
//! clicks on the tray icon), which only a person or the Accessibility permission can drive.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use tauri::{AppHandle, Listener, Manager};

use crate::commands::capture::CaptureMode;
use crate::commands::windows::WindowKind;
use crate::shortcuts::Action;
use crate::{session, shortcuts, trace, tray, windows};

/// Every event the core or a page sends, so the log sees all of them.
const EVENTS: &[&str] = &[
    "capture://started",
    "capture://closed",
    "capture://countdown",
    "overlay://selection-started",
    "output://copied",
    "output://saved",
    "output://failed",
    "output://handoff",
    "toast://show",
    "popover://opened",
    "pin://option-key",
    "settings://section-requested",
    "settings://changed",
    "editor://opened",
    "permissions://changed",
    "displays://changed",
    "shortcut://failed",
    "upload://progress",
    "upload://done",
    "upload://failed",
    "history://changed",
    "ocr://model-progress",
    "record://tick",
    "record://state",
    "record://finished",
    "record://export-progress",
    "record://setup",
    "record://levels",
    "record://bubble",
    "record://canvas",
    "record://click",
    "record://pointer",
    "record://trim",
];

/// Helpers every `eval` can use as `D`: pointer and key events dispatched the way WebKit orders
/// them, waiting, and pointer capture that tolerates synthetic pointers.
const PRELUDE: &str = r#"
if (!window.__drvlib) {
  const proto = Element.prototype;
  const setCap = proto.setPointerCapture, relCap = proto.releasePointerCapture, hasCap = proto.hasPointerCapture;
  let captured = null;
  proto.setPointerCapture = function (id) { try { setCap.call(this, id); } catch (_) {} captured = this; };
  proto.releasePointerCapture = function (id) { try { relCap.call(this, id); } catch (_) {} if (captured === this) captured = null; };
  proto.hasPointerCapture = function (id) { return captured === this || hasCap.call(this, id); };
  const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
  const frame = () => new Promise((r) => requestAnimationFrame(() => r()));
  const target = (x, y) => document.elementFromPoint(x, y) || document.body;
  const mods = (o) => ({ shiftKey: !!o.shift, altKey: !!o.alt, metaKey: !!o.meta, ctrlKey: !!o.ctrl });
  let down = null;
  function fire(kind, x, y, o = {}) {
    const pointerType = { down: "pointerdown", move: "pointermove", up: "pointerup" }[kind];
    const mouseType = { down: "mousedown", move: "mousemove", up: "mouseup" }[kind];
    const el = o.el || (captured && kind !== "down" ? captured : target(x, y));
    const buttons = kind === "up" ? 0 : down ? 1 : 0;
    const base = { bubbles: true, cancelable: true, composed: true, clientX: x, clientY: y, screenX: x, screenY: y, button: kind === "move" ? -1 : 0, buttons: kind === "down" ? 1 : buttons, view: window, ...mods(o) };
    const pe = new PointerEvent(pointerType, { ...base, pointerId: 1, pointerType: "mouse", isPrimary: true, pressure: kind === "up" ? 0 : (down || kind === "down") ? 0.5 : 0 });
    const allowed = el.dispatchEvent(pe);
    if (allowed || kind !== "down") el.dispatchEvent(new MouseEvent(mouseType, { ...base, button: kind === "move" ? 0 : 0, detail: o.detail ?? 1 }));
    if (kind === "down") down = el;
    if (kind === "up") {
      if (down && (down === el || down.contains(el) || el.contains(down)) && !o.noClick) el.dispatchEvent(new MouseEvent("click", { ...base, button: 0, detail: o.detail ?? 1 }));
      down = null;
      captured = null;
    }
    return el;
  }
  async function drag(points, o = {}) {
    const [first, ...rest] = points;
    fire("move", first[0], first[1], o);
    fire("down", first[0], first[1], o);
    for (const [x, y] of rest) { if (o.ms) await sleep(o.ms); else await frame(); fire("move", x, y, o); }
    const last = points[points.length - 1];
    fire("up", last[0], last[1], { ...o, noClick: o.click !== true });
  }
  function line(x0, y0, x1, y1, n = 12) {
    const out = [];
    for (let i = 0; i <= n; i++) out.push([x0 + ((x1 - x0) * i) / n, y0 + ((y1 - y0) * i) / n]);
    return out;
  }
  async function click(x, y, o = {}) { fire("move", x, y, o); fire("down", x, y, o); await frame(); return fire("up", x, y, o); }
  async function dblclick(x, y, o = {}) {
    await click(x, y, o); await click(x, y, { ...o, detail: 2 });
    target(x, y).dispatchEvent(new MouseEvent("dblclick", { bubbles: true, cancelable: true, clientX: x, clientY: y, detail: 2, view: window }));
  }
  function el(sel) { const e = typeof sel === "string" ? document.querySelector(sel) : sel; if (!e) throw new Error("no element " + sel); return e; }
  function center(sel) { const r = el(sel).getBoundingClientRect(); return [r.left + r.width / 2, r.top + r.height / 2]; }
  async function press(sel, o = {}) { const [x, y] = center(sel); return click(x, y, { ...o, el: el(sel) }); }
  function key(k, o = {}) {
    const code = o.code || (k.length === 1 ? (/[a-z]/i.test(k) ? "Key" + k.toUpperCase() : /[0-9]/.test(k) ? "Digit" + k : "") : k);
    const at = o.el ? el(o.el) : (document.activeElement || document.body);
    const init = { key: k, code, bubbles: true, cancelable: true, composed: true, ...mods(o) };
    const ok = at.dispatchEvent(new KeyboardEvent("keydown", init));
    if (!o.noUp) at.dispatchEvent(new KeyboardEvent("keyup", init));
    return ok;
  }
  function type(text) { for (const ch of text) { key(ch, { noUp: true }); document.execCommand("insertText", false, ch); } }
  async function until(fn, ms = 5000, step = 25) {
    const end = performance.now() + ms;
    for (;;) { const v = await fn(); if (v) return v; if (performance.now() > end) throw new Error("timed out waiting: " + fn); await sleep(step); }
  }
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__.invoke(cmd, args || {});
  function visible(sel) { const e = document.querySelector(sel); if (!e) return false; const r = e.getBoundingClientRect(); const s = getComputedStyle(e); return r.width > 0 && r.height > 0 && s.visibility !== "hidden" && s.display !== "none" && +s.opacity > 0; }
  function texts(sel) { return [...document.querySelectorAll(sel)].map((e) => e.textContent.trim()); }
  window.__drvlib = { sleep, frame, fire, drag, line, click, dblclick, el, center, press, key, type, until, invoke, visible, texts };
}
"#;

static APP: OnceLock<AppHandle> = OnceLock::new();
static EVENT_LOG: OnceLock<Mutex<fs::File>> = OnceLock::new();
static START: OnceLock<Instant> = OnceLock::new();

/// Starts the driver when `GLIMPSFY_DRIVER` names a folder.
pub fn setup(app: &AppHandle) {
    let Ok(folder) = std::env::var("GLIMPSFY_DRIVER") else { return };
    let folder = PathBuf::from(folder);
    let (inbox, outbox) = (folder.join("in"), folder.join("out"));
    if fs::create_dir_all(&inbox).is_err() || fs::create_dir_all(&outbox).is_err() {
        trace::log(format!("[glimpsfy] driver: cannot use {}", folder.display()));
        return;
    }
    let _ = APP.set(app.clone());
    let _ = START.set(Instant::now());
    if let Ok(file) = fs::OpenOptions::new().create(true).append(true).open(folder.join("events.jsonl")) {
        let _ = EVENT_LOG.set(Mutex::new(file));
    }
    for name in EVENTS {
        let name = (*name).to_string();
        app.listen_any(name.clone(), move |event| log_event(&name, event.payload()));
    }
    let _ = thread::Builder::new().name("glimpsfy-driver".into()).spawn(move || serve(inbox, outbox));
    #[cfg(target_os = "macos")]
    log_objc_exceptions();
    trace::log(format!("[glimpsfy] driver: listening in {}", folder.display()));
}

/// Logs every Objective-C exception as it is thrown, with the stack that threw it. An exception
/// that unwinds into Rust aborts the app with only "Rust cannot catch foreign exceptions", so
/// this line is the one place a driven run can learn which call threw and why.
#[cfg(target_os = "macos")]
fn log_objc_exceptions() {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::NSString;

    type Preprocessor = extern "C" fn(*mut AnyObject) -> *mut AnyObject;
    extern "C" {
        fn objc_setExceptionPreprocessor(f: Preprocessor) -> Preprocessor;
    }
    static PREVIOUS: OnceLock<Preprocessor> = OnceLock::new();

    extern "C" fn preprocess(exception: *mut AnyObject) -> *mut AnyObject {
        // SAFETY: the runtime hands over the exception object being thrown.
        if let Some(e) = unsafe { exception.as_ref() } {
            let name: Option<objc2::rc::Retained<NSString>> = unsafe { msg_send![e, name] };
            let reason: Option<objc2::rc::Retained<NSString>> = unsafe { msg_send![e, reason] };
            trace::log(format!(
                "[glimpsfy] objc exception {}: {}\n{}",
                name.map(|n| n.to_string()).unwrap_or_default(),
                reason.map(|r| r.to_string()).unwrap_or_default(),
                std::backtrace::Backtrace::force_capture()
            ));
        }
        match PREVIOUS.get() {
            Some(previous) => previous(exception),
            None => exception,
        }
    }

    // SAFETY: installs a function the runtime calls with each exception before it is thrown;
    // the previous one is kept and called after it.
    let previous = unsafe { objc_setExceptionPreprocessor(preprocess) };
    let _ = PREVIOUS.set(previous);
}

/// A finished request: `<name>.json`. The client writes `.<name>.json` first and renames it, so
/// a file still being written is never read.
fn is_request(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "json")
        && path.file_name().and_then(|n| n.to_str()).is_some_and(|n| !n.starts_with('.'))
}

fn now_ms() -> f64 {
    START.get().map_or(0.0, |s| s.elapsed().as_secs_f64() * 1000.0)
}

fn log_event(name: &str, payload: &str) {
    // The click ring's pointer stream and the meters are too chatty for the log.
    if matches!(name, "record://pointer" | "record://levels") {
        return;
    }
    let Some(log) = EVENT_LOG.get() else { return };
    let payload: Value = serde_json::from_str(payload).unwrap_or(Value::Null);
    let wall = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    let line = json!({ "t": now_ms(), "wall": wall, "event": name, "payload": payload });
    let mut file = log.lock().unwrap_or_else(|p| p.into_inner());
    let _ = writeln!(file, "{line}");
}

fn serve(inbox: PathBuf, outbox: PathBuf) {
    loop {
        let mut names: Vec<PathBuf> = fs::read_dir(&inbox)
            .map(|dir| dir.flatten().map(|e| e.path()).filter(|p| is_request(p)).collect())
            .unwrap_or_default();
        names.sort();
        for path in names {
            let request = fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok());
            let _ = fs::remove_file(&path);
            let started = Instant::now();
            let answer = match request {
                Some(request) => match run(&request) {
                    Ok(value) => json!({ "ok": true, "value": value }),
                    Err(error) => json!({ "ok": false, "error": error }),
                },
                None => json!({ "ok": false, "error": "the request is not JSON" }),
            };
            let mut answer = answer;
            answer["ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            answer["t"] = json!(now_ms());
            write_answer(&outbox, &path, &answer);
        }
        thread::sleep(Duration::from_millis(15));
    }
}

fn write_answer(outbox: &Path, request: &Path, answer: &Value) {
    let Some(name) = request.file_name() else { return };
    let tmp = outbox.join(format!(".{}.tmp", name.to_string_lossy()));
    if fs::write(&tmp, answer.to_string()).is_ok() {
        let _ = fs::rename(&tmp, outbox.join(name));
    }
}

fn app() -> &'static AppHandle {
    APP.get().expect("the driver runs after setup")
}

fn str_arg<'a>(request: &'a Value, key: &str) -> Result<&'a str, String> {
    request.get(key).and_then(Value::as_str).ok_or_else(|| format!("`{key}` is missing"))
}

/// Runs a closure on the main thread and waits for its answer.
fn on_main<T: Send + 'static>(f: impl FnOnce(&AppHandle) -> T + Send + 'static) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    let handle = app().clone();
    app()
        .run_on_main_thread(move || {
            let _ = tx.send(f(&handle));
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(Duration::from_secs(10)).map_err(|_| "the main thread did not answer within 10 s".to_string())
}

fn run(request: &Value) -> Result<Value, String> {
    match str_arg(request, "op")? {
        "ping" => Ok(json!("pong")),
        "eval" => {
            let timeout = request.get("timeout_ms").and_then(Value::as_f64).unwrap_or(15_000.0);
            eval(str_arg(request, "label")?, str_arg(request, "js")?, Duration::from_millis(timeout as u64))
        }
        "capture" => {
            let mode: CaptureMode = serde_json::from_value(request["mode"].clone()).map_err(|e| e.to_string())?;
            on_main(move |app| session::start(app, mode, Instant::now()).map_err(|e| e.to_string()))??;
            Ok(Value::Null)
        }
        "shortcut" => {
            let action: Action = serde_json::from_value(request["action"].clone()).map_err(|e| e.to_string())?;
            on_main(move |app| shortcuts::run(app, action, Instant::now()))?;
            Ok(Value::Null)
        }
        "menu" => {
            let id = str_arg(request, "id")?.to_string();
            on_main(move |app| tray::on_menu(app, &id))?;
            Ok(Value::Null)
        }
        "open" => {
            let kind: WindowKind = serde_json::from_value(request["kind"].clone()).map_err(|e| e.to_string())?;
            on_main(move |app| windows::open(app, kind))?;
            Ok(Value::Null)
        }
        "windows" => on_main(window_list),
        "shortcuts" => Ok(json!(crate::shortcuts::bound()
            .into_iter()
            .map(|(keys, action)| json!({ "keys": keys, "action": format!("{action:?}") }))
            .collect::<Vec<_>>())),
        "session" => {
            let current = app().state::<crate::AppState>().sessions.current();
            Ok(current.map_or(Value::Null, |s| json!({ "captureId": s.id.0, "mode": s.mode })))
        }
        "quit" => {
            let handle = app().clone();
            thread::spawn(move || {
                thread::sleep(Duration::from_millis(100));
                handle.exit(0);
            });
            Ok(Value::Null)
        }
        other => Err(format!("unknown op `{other}`")),
    }
}

static EVAL_SEQ: AtomicU64 = AtomicU64::new(0);

/// Runs `body` as an async function in the page and waits for what it returns (as JSON).
fn eval(label: &str, body: &str, timeout: Duration) -> Result<Value, String> {
    let window = app().get_webview_window(label).ok_or_else(|| format!("no window `{label}`"))?;
    let id = format!("e{}", EVAL_SEQ.fetch_add(1, Ordering::Relaxed));
    let script = format!(
        "{PRELUDE}\n(() => {{ const D = window.__drvlib; window.__drv = window.__drv || {{}}; window.__drv[{id:?}] = {{ pending: true }};\n\
         (async () => {{\n{body}\n}})().then((v) => {{ window.__drv[{id:?}] = {{ ok: true, value: v === undefined ? null : v }}; }}, \
         (e) => {{ window.__drv[{id:?}] = {{ ok: false, error: String((e && (e.stack || e.message)) || e) }}; }}); }})();"
    );
    window.eval(script).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + timeout;
    let probe = format!(
        "(() => {{ const r = window.__drv && window.__drv[{id:?}]; if (r && !r.pending) delete window.__drv[{id:?}]; \
         try {{ return JSON.stringify(r ?? null); }} catch (e) {{ return JSON.stringify({{ ok: false, error: 'the result is not JSON: ' + e }}); }} }})()"
    );
    loop {
        // A script may close its own window (⌘W in a pin); then there is nothing to wait for.
        if app().get_webview_window(label).is_none()
            || !window.is_visible().unwrap_or(false) && label.starts_with("pin-")
        {
            return Ok(json!({ "windowClosed": true }));
        }
        let (tx, rx) = mpsc::channel();
        window
            .eval_with_callback(probe.clone(), move |result| {
                let _ = tx.send(result);
            })
            .map_err(|e| e.to_string())?;
        let raw = rx.recv_timeout(Duration::from_secs(5)).map_err(|_| "the page did not answer".to_string())?;
        // WebKit hands back the JSON of the returned string, so it is decoded twice.
        let text: String = serde_json::from_str(&raw).unwrap_or(raw);
        let result: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if result.is_object() && result.get("pending").is_none() {
            return if result["ok"].as_bool() == Some(true) {
                Ok(result["value"].clone())
            } else {
                Err(result["error"].as_str().unwrap_or("the script failed").to_string())
            };
        }
        if Instant::now() > deadline {
            return Err(format!("the script did not finish within {} ms", timeout.as_millis()));
        }
        thread::sleep(Duration::from_millis(15));
    }
}

/// Every webview window with what AppKit says about it.
fn window_list(app: &AppHandle) -> Value {
    let mut list: Vec<Value> = app
        .webview_windows()
        .into_iter()
        .map(|(label, window)| {
            let item = json!({
                "label": label,
                "visible": window.is_visible().unwrap_or(false),
                "focused": window.is_focused().unwrap_or(false),
            });
            #[cfg(target_os = "macos")]
            let item = {
                let mut item = item;
                if let Some(mtm) = objc2::MainThreadMarker::new() {
                    if let Some(facts) = crate::macos::window_facts(&window, mtm) {
                        item["ns"] = facts;
                    }
                }
                item
            };
            item
        })
        .collect();
    list.sort_by(|a, b| a["label"].as_str().cmp(&b["label"].as_str()));
    Value::Array(list)
}
