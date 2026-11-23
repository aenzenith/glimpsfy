//! JSON-RPC 2.0 as MCP's stdio transport carries it: one message per line, the protocol versions
//! this server speaks, and the shapes of results and errors.

use std::io::{self, BufRead};

use serde_json::{json, Value};

/// The longest line a client may send. Tool calls are a few hundred bytes; a longer line is
/// refused before it is parsed, so a client cannot make the app hold megabytes of JSON.
pub const MAX_LINE: usize = 64 * 1024;

/// The protocol versions this server speaks, the newest first.
pub const PROTOCOL_VERSIONS: [&str; 4] = ["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// The version to answer `initialize` with: the client's own when this server speaks it, the
/// newest otherwise (the client then decides whether it can go on).
pub fn negotiate(requested: Option<&str>) -> &'static str {
    requested.and_then(|asked| PROTOCOL_VERSIONS.iter().find(|v| **v == asked).copied()).unwrap_or(PROTOCOL_VERSIONS[0])
}

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
pub const INTERNAL_ERROR: i64 = -32603;
/// A request that came before `initialize`.
pub const NOT_INITIALIZED: i64 = -32002;

/// One message from the client.
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    /// A request, which gets exactly one response under its `id`.
    Request { id: Value, method: String, params: Value },
    /// A notification, which gets none.
    Notification { method: String, params: Value },
    /// A response to something the server asked; this server asks nothing, so it is dropped.
    Response,
}

/// An error that a response carries.
#[derive(Clone, Debug, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> RpcError {
        RpcError { code, message: message.into() }
    }
}

/// Reads one message. A batch (an array) is refused: MCP's later versions dropped batches, and
/// one call per line keeps every answer tied to one approval.
pub fn parse(line: &[u8]) -> Result<Incoming, (Value, RpcError)> {
    let value: Value =
        serde_json::from_slice(line).map_err(|e| (Value::Null, RpcError::new(PARSE_ERROR, e.to_string())))?;
    let Value::Object(map) = value else {
        return Err((Value::Null, RpcError::new(INVALID_REQUEST, "a message is one JSON object")));
    };
    let id = map.get("id").cloned();
    if map.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err((id.unwrap_or(Value::Null), RpcError::new(INVALID_REQUEST, "jsonrpc must be \"2.0\"")));
    }
    let params = map.get("params").cloned().unwrap_or(Value::Null);
    match (map.get("method").and_then(Value::as_str), id) {
        (Some(method), Some(id)) => {
            if !(id.is_string() || id.is_i64() || id.is_u64()) {
                return Err((Value::Null, RpcError::new(INVALID_REQUEST, "an id is a string or an integer")));
            }
            Ok(Incoming::Request { id, method: method.to_string(), params })
        }
        (Some(method), None) => Ok(Incoming::Notification { method: method.to_string(), params }),
        (None, Some(_)) if map.contains_key("result") || map.contains_key("error") => Ok(Incoming::Response),
        (None, id) => Err((id.unwrap_or(Value::Null), RpcError::new(INVALID_REQUEST, "a message needs a method"))),
    }
}

/// A successful response.
pub fn result(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// A failed response.
pub fn error(id: &Value, error: &RpcError) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": error.code, "message": error.message } })
}

/// What reading a line gave.
#[derive(Debug, PartialEq)]
pub enum Line {
    Message(Vec<u8>),
    /// The line was longer than [`MAX_LINE`]; it was read to its end and thrown away.
    TooLong,
    /// The client closed its side.
    End,
}

/// Reads one line of at most [`MAX_LINE`] bytes, without its line break. A line over the limit is
/// consumed and dropped, so the next one starts clean.
pub fn read_line(reader: &mut impl BufRead) -> io::Result<Line> {
    let mut line = Vec::new();
    let mut too_long = false;
    loop {
        let buffer = match reader.fill_buf() {
            Ok(buffer) => buffer,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        if buffer.is_empty() {
            return Ok(if too_long {
                Line::TooLong
            } else if line.is_empty() {
                Line::End
            } else {
                Line::Message(line)
            });
        }
        let (taken, done) = match buffer.iter().position(|&b| b == b'\n') {
            Some(at) => (at + 1, true),
            None => (buffer.len(), false),
        };
        if !too_long {
            let body = &buffer[..if done { taken - 1 } else { taken }];
            if line.len() + body.len() > MAX_LINE {
                too_long = true;
                line = Vec::new();
            } else {
                line.extend_from_slice(body);
            }
        }
        reader.consume(taken);
        if done {
            if too_long {
                return Ok(Line::TooLong);
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(Line::Message(line));
        }
    }
}

/// Standard base64 with padding, for the image content blocks.
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n =
            (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}
