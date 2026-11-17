//! The built-in providers. Each public host's facts come from `docs/PROVIDERS.md`, with the date
//! its documentation was read; a host the documentation does not support is not here.

mod catbox;
mod custom;
mod imgbb;
mod litterbox;
mod s3;
mod zero_x_zero;

use std::sync::Arc;
use std::time::Instant;

use serde_json::Value;
use ureq::http::Method;
use url::Url;

use crate::http::{Body, Connection, Origin, Request, Response};
use crate::{shorten, Destination, ProviderId, ProviderTestResult, Result, UploadError};

pub(crate) use catbox::Catbox;
pub(crate) use custom::Custom;
pub(crate) use imgbb::ImgBb;
pub(crate) use litterbox::Litterbox;
pub(crate) use s3::S3;
pub(crate) use zero_x_zero::ZeroXZero;

/// The day `docs/PROVIDERS.md` read the public hosts' documentation.
pub(crate) const CHECKED_ON: &str = "2026-11-16";

pub(crate) fn builtin() -> Vec<Arc<dyn crate::Provider>> {
    vec![
        Arc::new(Litterbox::new()),
        Arc::new(Catbox::new()),
        Arc::new(ZeroXZero::new()),
        Arc::new(ImgBb::new()),
        Arc::new(S3),
        Arc::new(Custom),
    ]
}

/// The built-in providers with every public host moved to `base`, for tests against a local
/// server. S3 and custom endpoints take their address from their settings anyway.
#[doc(hidden)]
pub fn builtin_at(base: &Url) -> Vec<Arc<dyn crate::Provider>> {
    vec![
        Arc::new(Litterbox::at(base.clone())),
        Arc::new(Catbox::at(base.clone())),
        Arc::new(ZeroXZero::at(base.clone())),
        Arc::new(ImgBb::at(base.clone())),
        Arc::new(S3),
        Arc::new(Custom),
    ]
}

/// The destination of a host whose address is fixed.
pub(crate) fn fixed_destination(domain: &str, base: &Url) -> Result<Destination> {
    let origin =
        Origin::of(base).ok_or_else(|| UploadError::Network("the host's address is not http or https".into()))?;
    Ok(Destination { domain: domain.to_string(), bucket: None, origins: vec![origin], fixed: true })
}

/// Joins a path to a base URL.
pub(crate) fn endpoint(base: &Url, path: &str) -> Result<Url> {
    base.join(path).map_err(|_| UploadError::Network("the host's address could not be formed".into()))
}

/// The host's reason for refusing: the message of a JSON error, or the start of the text.
pub(crate) fn refusal(provider: &ProviderId, response: &Response) -> UploadError {
    let text = response.text();
    let reason = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|json| {
            [
                json.pointer("/error/message"),
                json.pointer("/data/error"),
                json.pointer("/error"),
                json.pointer("/message"),
                json.pointer("/status_txt"),
            ]
            .into_iter()
            .flatten()
            .find_map(|v| v.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| strip_html(&text));
    let reason = if reason.trim().is_empty() { format!("HTTP {}", response.status) } else { reason };
    UploadError::refused(provider, Some(response.status), shorten(&reason))
}

/// The answer of a host that replies with the link as plain text.
pub(crate) fn text_link(provider: &ProviderId, response: &Response) -> Result<Url> {
    if !response.is_success() {
        return Err(refusal(provider, response));
    }
    let text = response.text();
    crate::http::parse_http_url(text.trim()).ok_or_else(|| {
        UploadError::refused(
            provider,
            Some(response.status),
            format!("the host answered without a link: {}", shorten(&text)),
        )
    })
}

/// A request to the host's front page, the smallest check of a host that needs no key.
pub(crate) fn reachability(provider: &ProviderId, connection: &Connection, base: &Url) -> Result<ProviderTestResult> {
    let started = Instant::now();
    let response = connection.send(Request::new(Method::GET, base.clone()).body(Body::Empty))?;
    if response.status >= 500 {
        return Err(refusal(provider, &response));
    }
    Ok(ProviderTestResult {
        provider: provider.clone(),
        checked_at_ms: crate::now_ms(),
        latency_ms: started.elapsed().as_millis() as u64,
        url: None,
        warnings: Vec::new(),
    })
}

fn strip_html(text: &str) -> String {
    if !text.trim_start().starts_with('<') {
        return text.to_string();
    }
    let mut out = String::new();
    let mut in_tag = false;
    for c in text.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

/// A 1 × 1 transparent PNG, the smallest image a host accepts for a connection test.
pub(crate) const TEST_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00,
    0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49,
    0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00,
    0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];
