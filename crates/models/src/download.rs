//! The one place in the app's model areas (OCR and speech) that opens a network connection:
//! fetching a file that the caller's reviewed table names. The structural test in
//! `crates/ocr/tests/network.rs` keeps it that way.

use std::io::{Read, Write};
use std::time::Duration;

use crate::{ModelError, ModelFile, Result, Reviewed};

/// Redirects are followed by hand, so that every hop is checked against the allowed hosts.
const MAX_REDIRECTS: usize = 5;

/// Streams one file of the table into `sink`. Refuses a URL that is not in `reviewed`, and a
/// redirect to a host outside its hosts or to anything but HTTPS, before connecting. `online`
/// (the app's Work offline switch) is asked before every other check, before every hop of a
/// redirect and before every 64 KB of the body, so the switch is kept here, in the one function
/// that could break it, and turning it on stops a download that already runs.
pub(crate) fn fetch(
    file: &ModelFile,
    reviewed: &Reviewed,
    sink: &mut dyn Write,
    progress: &mut dyn FnMut(u64),
    cancelled: &dyn Fn() -> bool,
    online: &dyn Fn() -> bool,
) -> Result<()> {
    stay_offline(online)?;
    if !reviewed.files.iter().any(|f| f.url == file.url) {
        return Err(ModelError::Download(format!("{} is not in the reviewed model table", file.url)));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(20)))
        // The speech model is 574 MB; on a slow line it takes long, and the user can cancel.
        .timeout_global(Some(Duration::from_secs(3 * 60 * 60)))
        .build()
        .into();

    let mut url = file.url.to_string();
    for _ in 0..=MAX_REDIRECTS {
        stay_offline(online)?;
        allowed(&url, reviewed)?;
        check(cancelled)?;
        let response = agent.get(&url).call().map_err(|e| ModelError::Download(e.to_string()))?;
        let status = response.status().as_u16();
        if (300..400).contains(&status) {
            let location = response
                .headers()
                .get("location")
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| ModelError::Download(format!("the server answered {status} without a location")))?;
            url = resolve(&url, location)?;
            continue;
        }
        if status != 200 {
            return Err(ModelError::Download(format!("the server answered {status}")));
        }
        let mut reader = response.into_body().into_with_config().limit(file.bytes + 1).reader();
        let mut buf = vec![0u8; 64 * 1024];
        let mut received = 0u64;
        loop {
            check(cancelled)?;
            stay_offline(online)?;
            let n = reader.read(&mut buf).map_err(|e| ModelError::Download(e.to_string()))?;
            if n == 0 {
                return Ok(());
            }
            sink.write_all(&buf[..n])?;
            received += n as u64;
            progress(received);
        }
    }
    Err(ModelError::Download("the server redirected too many times".into()))
}

fn stay_offline(online: &dyn Fn() -> bool) -> Result<()> {
    if online() {
        Ok(())
    } else {
        Err(ModelError::Offline)
    }
}

fn check(cancelled: &dyn Fn() -> bool) -> Result<()> {
    if cancelled() {
        Err(ModelError::Cancelled)
    } else {
        Ok(())
    }
}

/// The host of an `https` URL, or `None` for any other scheme, a user name, a port or an empty
/// host.
fn host_of(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("https://")?;
    let host = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    (!host.is_empty() && !host.contains(['@', ':', '\\']) && !host.starts_with('.')).then_some(host)
}

/// Fails unless the URL is HTTPS to a host of `reviewed`. A table's own test runs every URL of
/// the table through it.
pub fn allowed(url: &str, reviewed: &Reviewed) -> Result<()> {
    let host = host_of(url).map(str::to_ascii_lowercase);
    let ok = host.as_deref().is_some_and(|h| {
        reviewed.hosts.contains(&h) || reviewed.host_suffixes.iter().any(|suffix| h.ends_with(suffix))
    });
    if ok {
        Ok(())
    } else {
        Err(ModelError::Download(format!("{url} is not on an allowed host")))
    }
}

/// The target of a redirect: an absolute `https` URL, or a path on the current host.
fn resolve(current: &str, location: &str) -> Result<String> {
    if location.starts_with("https://") {
        return Ok(location.to_string());
    }
    match (host_of(current), location.strip_prefix('/')) {
        (Some(host), Some(path)) if !path.starts_with('/') => Ok(format!("https://{host}/{path}")),
        _ => Err(ModelError::Download(format!("the server redirected to {location}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILE: ModelFile = ModelFile {
        name: "model.bin",
        url: "https://huggingface.co/someone/model/resolve/0123456789abcdef/model.bin",
        sha256: "",
        bytes: 1,
    };

    fn reviewed() -> Reviewed<'static> {
        Reviewed { files: &[&FILE], hosts: &["huggingface.co"], host_suffixes: &[".hf.co"] }
    }

    #[test]
    fn fetch_refuses_urls_and_redirects_outside_the_table_before_connecting() {
        // Not in the table, even on an allowed host: refused before any connection is made.
        for url in ["https://example.com/det.onnx", "https://huggingface.co/someone/else/resolve/main/rec.onnx"] {
            let file = ModelFile { name: "x", url, sha256: "", bytes: 1 };
            let result = fetch(&file, &reviewed(), &mut Vec::new(), &mut |_| {}, &|| false, &|| true);
            assert!(matches!(result, Err(ModelError::Download(_))), "{url}: {result:?}");
        }
        // Offline, a file of the table is refused before connecting as well.
        let result = fetch(&FILE, &reviewed(), &mut Vec::new(), &mut |_| {}, &|| false, &|| false);
        assert!(matches!(result, Err(ModelError::Offline)), "{result:?}");
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
            assert!(allowed(url, &reviewed()).is_err(), "{url} must be refused");
        }
        for url in ["https://huggingface.co/a", "https://us.aws.cdn.hf.co/xet-bridge-us/a?b=c"] {
            assert!(allowed(url, &reviewed()).is_ok(), "{url} must be allowed");
        }
        assert!(resolve("https://huggingface.co/a", "//evil.example/a").is_err());
        assert!(resolve("https://huggingface.co/a", "ftp://huggingface.co/a").is_err());
        assert_eq!(resolve("https://huggingface.co/a", "/api/b").ok().as_deref(), Some("https://huggingface.co/api/b"));
    }

    /// Work offline comes before every other check of the fetch: a URL outside the table, one on
    /// a host that is not allowed and one over plain HTTP all answer `Offline`, and `online` is
    /// asked before anything else could happen. A download that runs while the switch turns on
    /// stops at its next hop or chunk, which `fetch` asks it before.
    #[test]
    fn no_request_leaves_while_the_app_is_offline() {
        for url in [
            FILE.url,
            "https://example.com/det.onnx",
            "http://huggingface.co/someone/model/resolve/0123456789abcdef/model.bin",
            "https://evil.example/model.bin",
        ] {
            let file = ModelFile { name: "x", url, sha256: "", bytes: 1 };
            let asked = std::cell::Cell::new(0);
            let mut sink = Vec::new();
            let result = fetch(&file, &reviewed(), &mut sink, &mut |_| {}, &|| false, &|| {
                asked.set(asked.get() + 1);
                false
            });
            assert!(matches!(result, Err(ModelError::Offline)), "{url}: {result:?}");
            assert_eq!(asked.get(), 1, "{url}: the switch was not the first thing asked");
            assert!(sink.is_empty());
        }
        // The same holds through the public download, which leaves no part file behind.
        let dir = tempfile::tempdir().expect("a temporary folder");
        let result = crate::download(dir.path(), "m", &[FILE], &reviewed(), &mut |_, _| {}, &|| false, &|| false);
        assert!(matches!(result, Err(ModelError::Offline)), "{result:?}");
        assert!(!dir.path().join("model.bin").exists() && !dir.path().join("model.bin.part").exists());
    }
}
