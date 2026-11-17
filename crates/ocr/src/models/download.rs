//! The one place in the OCR area that opens a network connection: fetching a model file that the
//! reviewed table names. The structural test in `tests/network.rs` keeps it that way.

use std::io::{Read, Write};
use std::time::Duration;

use super::{ModelFile, ALLOWED_HOSTS, ALLOWED_HOST_SUFFIXES, MODELS};
use crate::{CancelToken, OcrError, Result};

/// Redirects are followed by hand, so that every hop is checked against the allowed hosts.
const MAX_REDIRECTS: usize = 5;

/// Streams one file of the table into `sink`. Refuses a URL that is not in the table, and a
/// redirect to a host outside [`ALLOWED_HOSTS`] or to anything but HTTPS, before connecting.
pub(super) fn fetch(
    file: &ModelFile,
    sink: &mut dyn Write,
    progress: &mut dyn FnMut(u64),
    cancel: &CancelToken,
) -> Result<()> {
    if !MODELS.iter().flat_map(|m| m.files).any(|f| f.url == file.url) {
        return Err(OcrError::Download(format!("{} is not in the reviewed model table", file.url)));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_global(Some(Duration::from_secs(15 * 60)))
        .build()
        .into();

    let mut url = file.url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        allowed(&url)?;
        cancel.check()?;
        let response = agent.get(&url).call().map_err(|e| OcrError::Download(e.to_string()))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| OcrError::Download(format!("the server answered {status} without a location")))?;
            url = resolve(&url, location)?;
            continue;
        }
        if status != 200 {
            return Err(OcrError::Download(format!("the server answered {status}")));
        }
        let mut reader = response.into_body().into_with_config().limit(file.bytes + 1).reader();
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            cancel.check()?;
            let n = reader.read(&mut buf).map_err(|e| OcrError::Download(e.to_string()))?;
            if n == 0 {
                return Ok(());
            }
            sink.write_all(&buf[..n])?;
            received += n as u64;
            progress(received);
        }
    }
    Err(OcrError::Download("the server redirected too many times".into()))
}

/// The host of an `https` URL, or `None` for any other scheme, a user name, a port or an empty
/// host.
fn host_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let host = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    (!host.is_empty() && !host.contains(['@', ':', '\\']) && !host.starts_with('.')).then_some(host)
}

/// Fails unless the URL is HTTPS to an allowed host.
fn allowed(url: &str) -> Result<()> {
    let host = host_of(url).map(str::to_ascii_lowercase);
    let ok = host
        .as_deref()
        .is_some_and(|h| ALLOWED_HOSTS.contains(&h) || ALLOWED_HOST_SUFFIXES.iter().any(|suffix| h.ends_with(suffix)));
    if ok {
        Ok(())
    } else {
        Err(OcrError::Download(format!("{url} is not on an allowed host")))
    }
}

/// The target of a redirect: an absolute `https` URL, or a path on the current host.
fn resolve(current: &str, location: &str) -> Result<String> {
    if location.starts_with("https://") {
        return Ok(location.to_string());
    }
    match (host_of(current), location.strip_prefix('/')) {
        (Some(host), Some(path)) if !path.starts_with('/') => Ok(format!("https://{host}/{path}")),
        _ => Err(OcrError::Download(format!("the server redirected to {location}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fetch_refuses_urls_and_redirects_outside_the_table_before_connecting() {
        // Not in the table, even on an allowed host: refused before any connection is made.
        for url in ["https://example.com/det.onnx", "https://huggingface.co/someone/else/resolve/main/rec.onnx"] {
            let file = ModelFile { name: "x", url, sha256: "", bytes: 1 };
            let result = fetch(&file, &mut Vec::new(), &mut |_| {}, &CancelToken::new());
            assert!(matches!(result, Err(OcrError::Download(_))), "{url}: {result:?}");
        }
        // Every hop of a redirect is checked the same way.
        for url in [
            "http://huggingface.co/a",
            "https://evil.example/a",
            "https://huggingface.co.evil.example/a",
            "https://user@huggingface.co/a",
            "https://huggingface.co:8443/a",
            "https://hf.co.example/a",
            "https://.hf.co/a",
        ] {
            assert!(allowed(url).is_err(), "{url} must be refused");
        }
        for url in ["https://huggingface.co/a", "https://us.aws.cdn.hf.co/xet-bridge-us/a?b=c"] {
            assert!(allowed(url).is_ok(), "{url} must be allowed");
        }
        assert!(resolve("https://huggingface.co/a", "//evil.example/a").is_err());
        assert!(resolve("https://huggingface.co/a", "ftp://huggingface.co/a").is_err());
        assert_eq!(resolve("https://huggingface.co/a", "/api/b").ok().as_deref(), Some("https://huggingface.co/api/b"));
        // Every URL in the table itself is on an allowed host.
        for file in MODELS.iter().flat_map(|m| m.files) {
            assert!(allowed(file.url).is_ok(), "{}", file.url);
        }
    }
}
