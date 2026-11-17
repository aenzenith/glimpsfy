//! Reads ShareX custom uploader files (`.sxcu`) into the custom HTTP provider's settings.
//!
//! The format is the one <https://getsharex.com/docs/custom-uploader> documents (read
//! 2026-11-16): `Name`, `DestinationType`, `RequestMethod`, `RequestURL`, `Parameters`, `Headers`,
//! `Body`, `Arguments`, `FileFormName`, `URL`, `DeletionURL` and `ErrorMessage`. Older files that
//! say `RequestType` and write `$json:…$` are read too.
//!
//! A file is refused when it could not upload a capture safely: a request URL that is not `http`
//! or `https` (or whose host is a template), a body that does not carry a file, a method without
//! a body, a header Glimpsfy must set itself, or a template call that would prompt the user, read
//! another input or parse XML (`syntax`). The values of parameters, headers and arguments often
//! hold tokens, so all of them go to the keychain; the settings keep only their names.

use std::collections::BTreeMap;

use serde_json::{Map, Value};

use crate::http::parse_http_url;
use crate::syntax::{Place, Template};
use crate::{pair_secret_key, FieldValue};

/// The custom provider's `pairs` rows, whose values live in the keychain.
pub const SECRET_PAIR_FIELDS: [&str; 3] = ["parameters", "headers", "arguments"];

/// The largest `.sxcu` file Glimpsfy reads.
pub const MAX_FILE_BYTES: usize = 256 * 1024;

/// Headers the HTTP client sets itself and a file may not.
const RESERVED_HEADERS: [&str; 5] = ["host", "content-length", "transfer-encoding", "connection", "content-type"];

/// What a file gives the custom provider.
#[derive(Clone, Debug, PartialEq)]
pub struct Imported {
    pub values: BTreeMap<String, FieldValue>,
    /// Keychain keys (`headers:Authorization`) and their values.
    pub secrets: Vec<(String, String)>,
}

/// Why a file was refused. `message` is a key of `src/locales/*/upload.json`; `field` is the
/// custom provider's row it concerns.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{message} ({field})")]
pub struct SxcuError {
    pub field: &'static str,
    pub message: String,
}

fn refuse(field: &'static str, message: &str) -> SxcuError {
    SxcuError { field, message: message.to_string() }
}

/// Reads a `.sxcu` file.
pub fn read(text: &str) -> Result<Imported, SxcuError> {
    if text.len() > MAX_FILE_BYTES {
        return Err(refuse("file", "upload.sxcu.tooLarge"));
    }
    let text = text.trim_start_matches('\u{feff}');
    let root: Map<String, Value> = match serde_json::from_str(text) {
        Ok(Value::Object(map)) => map,
        _ => return Err(refuse("file", "upload.sxcu.notJson")),
    };
    // ShareX writes PascalCase; hand-written files vary, so keys are read without case.
    let get = |name: &str| root.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v);
    let text_of = |name: &str| get(name).and_then(Value::as_str).map(str::trim).unwrap_or("").to_string();

    if get("RegexList").is_some() {
        return Err(refuse("urlTemplate", "upload.sxcu.legacyRegex"));
    }

    let destination = text_of("DestinationType");
    if !destination.is_empty() && !destination.contains("ImageUploader") && !destination.contains("FileUploader") {
        return Err(refuse("file", "upload.sxcu.destination"));
    }

    let method = {
        let method = text_of("RequestMethod");
        let method = if method.is_empty() { text_of("RequestType") } else { method };
        let method = if method.is_empty() { "POST".to_string() } else { method.to_ascii_uppercase() };
        if !["POST", "PUT", "PATCH"].contains(&method.as_str()) {
            return Err(refuse("method", "upload.sxcu.method"));
        }
        method
    };

    let request_url = text_of("RequestURL");
    check_request_url(&request_url).map_err(|message| refuse("requestUrl", message))?;
    Template::parse(&request_url, Place::Request).map_err(|e| refuse("requestUrl", e.message))?;

    let file_form_name = text_of("FileFormName");
    let body = match text_of("Body").to_ascii_lowercase().as_str() {
        "multipartformdata" => "multipart",
        "binary" => "binary",
        "" if !file_form_name.is_empty() => "multipart",
        _ => return Err(refuse("body", "upload.sxcu.body")),
    };
    if body == "multipart" && file_form_name.is_empty() {
        return Err(refuse("fileFormName", "upload.sxcu.fileFormName"));
    }

    let mut values = BTreeMap::new();
    let mut secrets = Vec::new();
    let name = text_of("Name");
    let name = if name.is_empty() {
        parse_http_url(&request_url).and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default()
    } else {
        name
    };
    values.insert("name".to_string(), FieldValue::Text(name));
    values.insert("requestUrl".to_string(), FieldValue::Text(request_url));
    values.insert("method".to_string(), FieldValue::Text(method));
    values.insert("body".to_string(), FieldValue::Text(body.to_string()));
    values.insert(
        "fileFormName".to_string(),
        FieldValue::Text(if file_form_name.is_empty() { "file".into() } else { file_form_name }),
    );

    for (field, source) in [("parameters", "Parameters"), ("headers", "Headers"), ("arguments", "Arguments")] {
        let mut names = Vec::new();
        if let Some(Value::Object(map)) = get(source) {
            for (key, value) in map {
                let key = key.trim().to_string();
                if key.is_empty() {
                    continue;
                }
                if field == "headers" && RESERVED_HEADERS.contains(&key.to_ascii_lowercase().as_str()) {
                    return Err(refuse("headers", "upload.sxcu.reservedHeader"));
                }
                let value = match value {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    Value::Bool(b) => b.to_string(),
                    _ => return Err(refuse(field_name(field), "upload.sxcu.pairValue")),
                };
                Template::parse(&value, Place::Request).map_err(|e| refuse(field_name(field), e.message))?;
                if !value.is_empty() {
                    secrets.push((pair_secret_key(field, &key), value));
                }
                names.push(key);
            }
        }
        values.insert(field.to_string(), FieldValue::Names(names));
    }

    for (field, source, fallback) in [
        ("urlTemplate", "URL", "{response}"),
        ("deletionUrlTemplate", "DeletionURL", ""),
        ("errorTemplate", "ErrorMessage", ""),
    ] {
        let template = modernize(&text_of(source));
        let template = if template.is_empty() { fallback.to_string() } else { template };
        Template::parse(&template, Place::Response).map_err(|e| refuse(field_name(field), e.message))?;
        values.insert(field.to_string(), FieldValue::Text(template));
    }

    Ok(Imported { values, secrets })
}

fn field_name(field: &str) -> &'static str {
    match field {
        "parameters" => "parameters",
        "headers" => "headers",
        "arguments" => "arguments",
        "urlTemplate" => "urlTemplate",
        "deletionUrlTemplate" => "deletionUrlTemplate",
        _ => "errorTemplate",
    }
}

/// A request URL must be an absolute `http` or `https` address whose host is plain text.
pub fn check_request_url(url: &str) -> Result<(), &'static str> {
    let lower = url.trim().to_ascii_lowercase();
    let rest =
        lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://")).ok_or("upload.issue.httpOnly")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.is_empty() || authority.contains(['{', '}', '$', '@']) {
        return Err("upload.issue.templateHost");
    }
    // The address must parse with a placeholder in place of every template call.
    let filled = Template::parse(url, Place::Request).map(|t| t.fill("file.png")).map_err(|e| e.message)?;
    parse_http_url(&filled).map(|_| ()).ok_or("upload.issue.httpOnly")
}

/// Turns the `$function:…$` calls of files from ShareX 13 and older into `{function:…}`.
fn modernize(template: &str) -> String {
    const LEGACY: [&str; 6] = ["$json:", "$xml:", "$regex:", "$header:", "$response$", "$filename$"];
    if !LEGACY.iter().any(|l| template.contains(l)) {
        return template.to_string();
    }
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('$') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('$') {
            Some(end) if after[..end].chars().next().is_some_and(|c| c.is_ascii_alphabetic()) => {
                out.push('{');
                out.push_str(&after[..end]);
                out.push('}');
                rest = &after[end + 1..];
            }
            _ => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}
