//! The tools an agent may call, their input schemas, the asks they turn into, and the content and
//! errors a call answers with.

use serde::Serialize;
use serde_json::{json, Map, Value};

/// The long edge an image gets when the agent names none: what vision models take without
/// scaling it again.
pub const DEFAULT_EDGE: u32 = 1568;
/// The longest edge an agent may ask for.
pub const MAX_EDGE: u32 = 3840;
/// The shortest edge an agent may ask for; smaller images say nothing.
pub const MIN_EDGE: u32 = 64;
/// The largest PNG a call returns; a larger one is scaled down until it fits.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// A rectangle of the global desktop, in screen points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// What one tool call asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Ask {
    /// The displays, which needs no approval: their names and frames say nothing about what they
    /// show.
    ListDisplays,
    /// The frontmost window of `app`, or the frontmost one whose title holds `title`.
    Window { app: String, title: Option<String>, max_edge: u32 },
    /// One display, or the one under the pointer.
    Display { display: Option<u32>, max_edge: u32 },
    /// A rectangle, or one the user selects.
    Area { rect: Option<Rect>, max_edge: u32 },
    /// The text in a rectangle, or in one the user selects.
    Text { rect: Option<Rect> },
}

impl Ask {
    /// Whether the user selects the area, which no timed permission covers.
    pub fn is_pick(&self) -> bool {
        matches!(self, Ask::Area { rect: None, .. } | Ask::Text { rect: None })
    }

    pub fn needs_approval(&self) -> bool {
        !matches!(self, Ask::ListDisplays)
    }
}

/// Why a call returned no content. The code is the part an agent reads; the message is for the
/// person who reads the agent's log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolError {
    UserDenied,
    Timeout,
    ScreenLocked,
    QueueFull,
    NotFound(String),
    PermissionMissing,
    InvalidArguments(String),
    Failed(String),
}

impl ToolError {
    pub fn code(&self) -> &'static str {
        match self {
            ToolError::UserDenied => "user_denied",
            ToolError::Timeout => "timeout",
            ToolError::ScreenLocked => "screen_locked",
            ToolError::QueueFull => "queue_full",
            ToolError::NotFound(_) => "not_found",
            ToolError::PermissionMissing => "permission_missing",
            ToolError::InvalidArguments(_) => "invalid_arguments",
            ToolError::Failed(_) => "failed",
        }
    }

    pub fn message(&self) -> String {
        match self {
            ToolError::UserDenied => "The user denied the request.".into(),
            ToolError::Timeout => "The user did not answer within 60 seconds, so the request was denied.".into(),
            ToolError::ScreenLocked => "The screen is locked, so nobody can approve the request.".into(),
            ToolError::QueueFull => {
                "Five requests already wait for the user; try again after they are answered.".into()
            }
            ToolError::NotFound(what) => what.clone(),
            ToolError::PermissionMissing => {
                "Glimpsfy lacks the Screen Recording permission; the user grants it in System Settings.".into()
            }
            ToolError::InvalidArguments(why) => why.clone(),
            ToolError::Failed(why) => why.clone(),
        }
    }
}

/// What a call answers with.
#[derive(Clone, Debug, PartialEq)]
pub enum Content {
    /// A PNG, base64 only on the socket.
    Png(Vec<u8>),
    Text(String),
}

/// The `tools/call` result for a successful call.
pub fn success(content: &[Content]) -> Value {
    let blocks: Vec<Value> = content
        .iter()
        .map(|c| match c {
            Content::Png(png) => json!({ "type": "image", "data": crate::rpc::base64(png), "mimeType": "image/png" }),
            Content::Text(text) => json!({ "type": "text", "text": text }),
        })
        .collect();
    json!({ "content": blocks, "isError": false })
}

/// The `tools/call` result for a call that returned nothing: an error result, not a JSON-RPC
/// error, so the agent's model reads why.
pub fn failure(error: &ToolError) -> Value {
    json!({
        "content": [{ "type": "text", "text": format!("{}: {}", error.code(), error.message()) }],
        "isError": true,
    })
}

fn rect_schema() -> Value {
    json!({
        "type": "object",
        "description": "A rectangle of the desktop in screen points, as list_displays gives the displays' frames. Leave it out to let the user select the area.",
        "properties": {
            "x": { "type": "number" },
            "y": { "type": "number" },
            "width": { "type": "number", "exclusiveMinimum": 0 },
            "height": { "type": "number", "exclusiveMinimum": 0 }
        },
        "required": ["x", "y", "width", "height"],
        "additionalProperties": false
    })
}

fn edge_schema() -> Value {
    json!({
        "type": "integer",
        "minimum": MIN_EDGE,
        "maximum": MAX_EDGE,
        "description": format!("The image's longest edge in pixels; {DEFAULT_EDGE} when left out. A smaller capture is never enlarged.")
    })
}

/// The `tools/list` result.
pub fn list() -> Value {
    let approval = " The user approves every request in Glimpsfy before anything is sent, and may deny it.";
    json!({ "tools": [
        {
            "name": "list_displays",
            "description": "Lists the connected displays with their ids, names and frames in screen points. Needs no approval.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "capture_window",
            "description": format!("Takes a PNG of one app's frontmost window, or of its frontmost window whose title contains `title`.{approval}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "app": { "type": "string", "description": "The app's name as the Dock shows it, such as \"Safari\" or \"Terminal\"." },
                    "title": { "type": "string", "description": "Part of the window's title, to choose among the app's windows." },
                    "maxEdge": edge_schema()
                },
                "required": ["app"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "capture_display",
            "description": format!("Takes a PNG of a whole display: the one list_displays names by `display`, or the one under the pointer.{approval}"),
            "inputSchema": {
                "type": "object",
                "properties": { "display": { "type": "integer", "description": "A display id from list_displays." }, "maxEdge": edge_schema() },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "capture_area",
            "description": format!("Takes a PNG of a rectangle of the desktop, or of an area the user selects when `rect` is left out.{approval}"),
            "inputSchema": {
                "type": "object",
                "properties": { "rect": rect_schema(), "maxEdge": edge_schema() },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "read_text",
            "description": format!("Recognises the text in a rectangle of the desktop, or in an area the user selects when `rect` is left out, and returns it as plain text; no image is sent.{approval}"),
            "inputSchema": {
                "type": "object",
                "properties": { "rect": rect_schema() },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }
    ] })
}

fn object(arguments: &Value) -> Result<Map<String, Value>, ToolError> {
    match arguments {
        Value::Null => Ok(Map::new()),
        Value::Object(map) => Ok(map.clone()),
        _ => Err(ToolError::InvalidArguments("arguments must be an object".into())),
    }
}

fn refuse_unknown(map: &Map<String, Value>, known: &[&str]) -> Result<(), ToolError> {
    match map.keys().find(|k| !known.contains(&k.as_str())) {
        Some(key) => Err(ToolError::InvalidArguments(format!("unknown argument `{key}`"))),
        None => Ok(()),
    }
}

fn text(map: &Map<String, Value>, key: &str, max: usize) -> Result<Option<String>, ToolError> {
    match map.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => {
            let s = s.trim();
            if s.is_empty() || s.chars().count() > max {
                return Err(ToolError::InvalidArguments(format!("`{key}` must hold 1 to {max} characters")));
            }
            Ok(Some(s.to_string()))
        }
        Some(_) => Err(ToolError::InvalidArguments(format!("`{key}` must be a string"))),
    }
}

fn max_edge(map: &Map<String, Value>) -> Result<u32, ToolError> {
    match map.get("maxEdge") {
        None | Some(Value::Null) => Ok(DEFAULT_EDGE),
        Some(v) => match v.as_u64() {
            Some(edge) if (MIN_EDGE as u64..=MAX_EDGE as u64).contains(&edge) => Ok(edge as u32),
            _ => {
                Err(ToolError::InvalidArguments(format!("`maxEdge` must be an integer from {MIN_EDGE} to {MAX_EDGE}")))
            }
        },
    }
}

fn rect(map: &Map<String, Value>) -> Result<Option<Rect>, ToolError> {
    let Some(value) = map.get("rect").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let bad = || ToolError::InvalidArguments("`rect` needs finite numbers x, y and a positive width and height".into());
    let Value::Object(r) = value else {
        return Err(bad());
    };
    refuse_unknown(r, &["x", "y", "width", "height"])?;
    let number = |k: &str| r.get(k).and_then(Value::as_f64).filter(|n| n.is_finite() && n.abs() < 1.0e6);
    let (Some(x), Some(y), Some(width), Some(height)) = (number("x"), number("y"), number("width"), number("height"))
    else {
        return Err(bad());
    };
    if width < 1.0 || height < 1.0 {
        return Err(bad());
    }
    Ok(Some(Rect { x, y, width, height }))
}

/// Reads a `tools/call`'s name and arguments into an ask.
pub fn parse(name: &str, arguments: &Value) -> Result<Ask, ToolError> {
    let map = object(arguments)?;
    match name {
        "list_displays" => {
            refuse_unknown(&map, &[])?;
            Ok(Ask::ListDisplays)
        }
        "capture_window" => {
            refuse_unknown(&map, &["app", "title", "maxEdge"])?;
            let app = text(&map, "app", 200)?.ok_or_else(|| ToolError::InvalidArguments("`app` is required".into()))?;
            Ok(Ask::Window { app, title: text(&map, "title", 400)?, max_edge: max_edge(&map)? })
        }
        "capture_display" => {
            refuse_unknown(&map, &["display", "maxEdge"])?;
            let display = match map.get("display") {
                None | Some(Value::Null) => None,
                Some(v) => Some(
                    v.as_u64()
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| ToolError::InvalidArguments("`display` must be a display id".into()))?,
                ),
            };
            Ok(Ask::Display { display, max_edge: max_edge(&map)? })
        }
        "capture_area" => {
            refuse_unknown(&map, &["rect", "maxEdge"])?;
            Ok(Ask::Area { rect: rect(&map)?, max_edge: max_edge(&map)? })
        }
        "read_text" => {
            refuse_unknown(&map, &["rect"])?;
            Ok(Ask::Text { rect: rect(&map)? })
        }
        other => Err(ToolError::InvalidArguments(format!("no tool is named `{other}`"))),
    }
}
