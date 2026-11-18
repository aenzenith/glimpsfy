//! The template syntax of ShareX custom uploaders, as the custom HTTP provider uses it.
//!
//! A template is text with `{function:arguments}` calls; `\{`, `\}`, `\|` and `\\` stand for the
//! characters themselves (<https://getsharex.com/docs/custom-uploader>, read 2026-11-16). Glimpsfy
//! reads two kinds of template:
//!
//! - **Request templates** (the request URL, parameters, headers, form arguments) may call
//!   `{filename}`, `{random:a|b|…}` and `{base64:text}`.
//! - **Response templates** (the link, the deletion link, the error message) may also call
//!   `{response}`, `{responseurl}`, `{header:name}`, `{json:path}` and `{regex:pattern|group}`.
//!
//! Evaluation is a pure function of the answer the host already sent: no function reads a file,
//! opens a connection or asks the user anything, and a value taken from the answer is inserted as
//! text and never read as a template again. Calls that would prompt the user (`{select}`,
//! `{inputbox}`, `{outputbox}`), read another source (`{input}`, the two-argument `{json}` and the
//! three-argument `{regex}`) or parse XML are refused when the template is read, with a message
//! that names the function.

use std::cell::OnceCell;

use serde_json::Value;

/// Where a template is used, which decides the functions it may call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Request,
    Response,
}

/// A parsed template.
#[derive(Clone, Debug)]
pub struct Template(Vec<Piece>);

#[derive(Clone, Debug)]
enum Piece {
    Text(String),
    Call(Call),
}

#[derive(Clone, Debug)]
enum Call {
    Response,
    ResponseUrl,
    Header(String),
    Json(Vec<Step>),
    Regex { pattern: regex_lite::Regex, group: Group },
    FileName,
    Random(Vec<Template>),
    Base64(Template),
}

#[derive(Clone, Debug, PartialEq)]
enum Group {
    Index(usize),
    Name(String),
}

#[derive(Clone, Debug, PartialEq)]
enum Step {
    Key(String),
    Index(i64),
}

/// Why a template cannot be used. `message` is a key of `src/locales/*/upload.json`, and
/// `function` names the call it is about.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message} ({})", function.as_deref().unwrap_or("-"))]
pub struct SyntaxError {
    pub message: &'static str,
    pub function: Option<String>,
}

impl SyntaxError {
    fn new(message: &'static str, function: Option<&str>) -> SyntaxError {
        SyntaxError { message, function: function.map(str::to_string) }
    }
}

/// The longest regular expression a template may hold.
const MAX_PATTERN: usize = 1000;

/// How deep calls may sit inside one another's arguments. Only `random` and `base64` take calls
/// as arguments, and a real template nests them two or three deep; the parser recurses once per
/// level, so a file with thousands of `{x:` would otherwise exhaust the thread's stack.
const MAX_DEPTH: usize = 16;

impl Template {
    /// Reads a template for `place`.
    pub fn parse(text: &str, place: Place) -> Result<Template, SyntaxError> {
        let chars: Vec<char> = text.chars().collect();
        let mut at = 0;
        let pieces = parse_pieces(&chars, &mut at, place, 0)?;
        if at < chars.len() {
            return Err(SyntaxError::new("upload.syntax.unbalanced", None));
        }
        Ok(Template(pieces))
    }

    /// Whether the template is plain text without calls.
    pub fn is_plain(&self) -> bool {
        self.0.iter().all(|p| matches!(p, Piece::Text(_)))
    }

    /// Fills a request template.
    pub fn fill(&self, file_name: &str) -> String {
        self.evaluate(&Answer::none(file_name)).unwrap_or_default()
    }

    /// Fills a template from a host's answer.
    pub fn evaluate(&self, answer: &Answer) -> Result<String, EvalError> {
        let mut out = String::new();
        for piece in &self.0 {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Call(call) => out.push_str(&call.evaluate(answer)?),
            }
        }
        Ok(out)
    }
}

/// A host's answer, as response templates read it.
pub struct Answer<'a> {
    pub body: &'a str,
    /// Header names in lower case.
    pub headers: &'a [(String, String)],
    /// The URL the request went to, or the `Location` of a redirect, for `{responseurl}`.
    pub url: &'a str,
    pub file_name: &'a str,
    json: OnceCell<Option<Value>>,
}

impl<'a> Answer<'a> {
    pub fn new(body: &'a str, headers: &'a [(String, String)], url: &'a str, file_name: &'a str) -> Answer<'a> {
        Answer { body, headers, url, file_name, json: OnceCell::new() }
    }

    fn none(file_name: &'a str) -> Answer<'a> {
        Answer::new("", &[], "", file_name)
    }

    fn json(&self) -> Option<&Value> {
        self.json.get_or_init(|| serde_json::from_str(self.body).ok()).as_ref()
    }
}

/// Why a response template found nothing in the answer.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EvalError {
    #[error("the answer is not JSON")]
    NotJson,
    #[error("the answer has no text value at {0}")]
    NoValue(String),
    #[error("the answer has no header {0}")]
    NoHeader(String),
    #[error("the pattern did not match the answer")]
    NoMatch,
}

impl Call {
    fn evaluate(&self, answer: &Answer) -> Result<String, EvalError> {
        match self {
            Call::Response => Ok(answer.body.trim().to_string()),
            Call::ResponseUrl => Ok(answer.url.to_string()),
            Call::FileName => Ok(answer.file_name.to_string()),
            Call::Header(name) => answer
                .headers
                .iter()
                .find(|(n, _)| n.eq_ignore_ascii_case(name))
                .map(|(_, v)| v.clone())
                .ok_or_else(|| EvalError::NoHeader(name.clone())),
            Call::Json(steps) => {
                let mut value = answer.json().ok_or(EvalError::NotJson)?;
                for step in steps {
                    value = match (step, value) {
                        (Step::Key(key), Value::Object(map)) => map.get(key),
                        (Step::Index(i), Value::Array(list)) => {
                            let index = if *i < 0 { list.len() as i64 + i } else { *i };
                            usize::try_from(index).ok().and_then(|i| list.get(i))
                        }
                        _ => None,
                    }
                    .ok_or_else(|| EvalError::NoValue(describe_path(steps)))?;
                }
                match value {
                    Value::String(text) => Ok(text.clone()),
                    Value::Number(n) => Ok(n.to_string()),
                    Value::Bool(b) => Ok(b.to_string()),
                    _ => Err(EvalError::NoValue(describe_path(steps))),
                }
            }
            Call::Regex { pattern, group } => {
                let captures = pattern.captures(answer.body).ok_or(EvalError::NoMatch)?;
                let found = match group {
                    Group::Index(i) => captures.get(*i),
                    Group::Name(name) => captures.name(name),
                };
                found.map(|m| m.as_str().to_string()).ok_or(EvalError::NoMatch)
            }
            Call::Random(choices) => {
                let pick = crate::engine::random_index(choices.len());
                choices.get(pick).map(|t| t.evaluate(answer)).unwrap_or_else(|| Ok(String::new()))
            }
            Call::Base64(text) => Ok(base64(text.evaluate(answer)?.as_bytes())),
        }
    }
}

fn describe_path(steps: &[Step]) -> String {
    let mut out = String::new();
    for step in steps {
        match step {
            Step::Key(key) => {
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            Step::Index(i) => out.push_str(&format!("[{i}]")),
        }
    }
    out
}

// --------------------------------------------------------------------------------------- parser

/// Reads pieces until the end, or, inside a call's arguments (`depth` above 0), until `|` or `}`.
fn parse_pieces(chars: &[char], at: &mut usize, place: Place, depth: usize) -> Result<Vec<Piece>, SyntaxError> {
    let mut pieces = Vec::new();
    let mut text = String::new();
    while *at < chars.len() {
        let c = chars[*at];
        match c {
            '\\' => {
                match chars.get(*at + 1) {
                    Some(next @ ('{' | '}' | '|' | '\\')) => text.push(*next),
                    Some(next) => {
                        text.push('\\');
                        text.push(*next);
                    }
                    None => text.push('\\'),
                }
                *at += 2;
            }
            '{' => {
                if !text.is_empty() {
                    pieces.push(Piece::Text(std::mem::take(&mut text)));
                }
                if depth >= MAX_DEPTH {
                    return Err(SyntaxError::new("upload.syntax.tooDeep", None));
                }
                *at += 1;
                pieces.push(Piece::Call(parse_call(chars, at, place, depth + 1)?));
            }
            '}' | '|' if depth > 0 => break,
            '}' => return Err(SyntaxError::new("upload.syntax.unbalanced", None)),
            _ => {
                text.push(c);
                *at += 1;
            }
        }
    }
    if !text.is_empty() {
        pieces.push(Piece::Text(text));
    }
    Ok(pieces)
}

/// Reads a call after its `{`, up to and including its `}`.
fn parse_call(chars: &[char], at: &mut usize, place: Place, depth: usize) -> Result<Call, SyntaxError> {
    let start = *at;
    while *at < chars.len() && (chars[*at].is_ascii_alphanumeric() || chars[*at] == '_') {
        *at += 1;
    }
    let name: String = chars[start..*at].iter().collect::<String>().to_ascii_lowercase();
    let mut args: Vec<Vec<Piece>> = Vec::new();
    match chars.get(*at) {
        Some('}') => *at += 1,
        Some(':') => {
            *at += 1;
            loop {
                args.push(parse_pieces(chars, at, place, depth)?);
                match chars.get(*at) {
                    Some('|') => *at += 1,
                    Some('}') => {
                        *at += 1;
                        break;
                    }
                    _ => return Err(SyntaxError::new("upload.syntax.unbalanced", Some(&name))),
                }
            }
        }
        _ => return Err(SyntaxError::new("upload.syntax.unbalanced", Some(&name))),
    }

    let response_only = |call: Call| {
        if place == Place::Response {
            Ok(call)
        } else {
            Err(SyntaxError::new("upload.syntax.responseOnly", Some(&name)))
        }
    };
    let literal = |index: usize| -> Result<String, SyntaxError> {
        let pieces = args.get(index).ok_or_else(|| SyntaxError::new("upload.syntax.missingArgument", Some(&name)))?;
        let mut out = String::new();
        for piece in pieces {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Call(_) => return Err(SyntaxError::new("upload.syntax.nested", Some(&name))),
            }
        }
        Ok(out)
    };

    match (name.as_str(), args.len()) {
        ("response", 0) => response_only(Call::Response),
        ("responseurl", 0) => response_only(Call::ResponseUrl),
        ("header", 1) => response_only(Call::Header(literal(0)?.trim().to_string())),
        ("json", 1) => response_only(Call::Json(parse_json_path(&literal(0)?)?)),
        ("regex", 1 | 2) => {
            let pattern = literal(0)?;
            let group = match args.len() {
                2 => {
                    let group = literal(1)?;
                    match group.trim().parse::<usize>() {
                        Ok(i) => Group::Index(i),
                        Err(_) => Group::Name(group.trim().to_string()),
                    }
                }
                _ => Group::Index(0),
            };
            response_only(Call::Regex { pattern: compile(&pattern)?, group })
        }
        ("filename", 0) => Ok(Call::FileName),
        ("random", n) if n > 0 => Ok(Call::Random(args.into_iter().map(Template).collect())),
        ("base64", 1) => Ok(Call::Base64(Template(args.into_iter().next().unwrap_or_default()))),
        ("json", _) | ("regex", _) => Err(SyntaxError::new("upload.syntax.otherInput", Some(&name))),
        ("xml", _) => Err(SyntaxError::new("upload.syntax.xml", Some(&name))),
        ("select" | "inputbox" | "outputbox" | "prompt", _) => {
            Err(SyntaxError::new("upload.syntax.interactive", Some(&name)))
        }
        ("input", _) => Err(SyntaxError::new("upload.syntax.input", Some(&name))),
        _ => Err(SyntaxError::new("upload.syntax.unknown", Some(&name))),
    }
}

fn compile(pattern: &str) -> Result<regex_lite::Regex, SyntaxError> {
    if pattern.chars().count() > MAX_PATTERN {
        return Err(SyntaxError::new("upload.syntax.patternTooLong", Some("regex")));
    }
    regex_lite::RegexBuilder::new(pattern)
        .size_limit(1 << 20)
        .nest_limit(64)
        .build()
        .map_err(|_| SyntaxError::new("upload.syntax.badPattern", Some("regex")))
}

/// Reads a JSON path: `data.link`, `files[0].url`, `$.a['b c']`, `items[-1]`.
fn parse_json_path(path: &str) -> Result<Vec<Step>, SyntaxError> {
    let bad = || SyntaxError::new("upload.syntax.badJsonPath", Some("json"));
    let mut rest = path.trim();
    rest = rest.strip_prefix('$').unwrap_or(rest);
    let mut steps = Vec::new();
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix('.') {
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('[') {
            let close = after.find(']').ok_or_else(bad)?;
            let inside = after[..close].trim();
            if let Some(quoted) = inside
                .strip_prefix('\'')
                .and_then(|s| s.strip_suffix('\''))
                .or_else(|| inside.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
            {
                steps.push(Step::Key(quoted.to_string()));
            } else {
                steps.push(Step::Index(inside.parse().map_err(|_| bad())?));
            }
            rest = &after[close + 1..];
            continue;
        }
        let end = rest.find(['.', '[']).unwrap_or(rest.len());
        let key = &rest[..end];
        if key.is_empty() {
            return Err(bad());
        }
        steps.push(Step::Key(key.to_string()));
        rest = &rest[end..];
    }
    if steps.is_empty() {
        return Err(bad());
    }
    Ok(steps)
}

/// Standard Base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n =
            (chunk[0] as u32) << 16 | (*chunk.get(1).unwrap_or(&0) as u32) << 8 | *chunk.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
