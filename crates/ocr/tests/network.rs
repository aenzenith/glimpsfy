//! Text recognition runs on the machine. The only network traffic the OCR area may cause is the
//! PP-OCR model download a user asks for, and only to the hosts of the reviewed model table. This
//! test reads the area's sources and fails when a second way out appears: another HTTP client, a
//! socket, a browser request in the text layer, or another caller of the download.

use std::fs;
use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files(dir: &Path, extensions: &[&str], out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            files(&path, extensions, out);
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| extensions.contains(&e)) {
            out.push(path);
        }
    }
}

/// The source lines that are code, without line comments, as (path, line number, text).
fn code_lines(paths: &[PathBuf]) -> Vec<(PathBuf, usize, String)> {
    let mut out = Vec::new();
    for path in paths {
        let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{} cannot be read: {e}", path.display()));
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
                continue;
            }
            out.push((path.clone(), i + 1, line.to_string()));
        }
    }
    out
}

fn rel(path: &Path) -> String {
    path.strip_prefix(repo()).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

#[test]
fn only_the_model_download_reaches_the_network() {
    let root = repo();
    let mut rust = Vec::new();
    files(&root.join("crates/ocr/src"), &["rs"], &mut rust);
    rust.push(root.join("src-tauri/src/commands/ocr.rs"));
    let mut web = Vec::new();
    files(&root.join("src/ocr"), &["ts", "vue"], &mut web);
    web.push(root.join("src/lib/api/ocr.ts"));
    assert!(rust.len() > 5, "the OCR sources were not found from {}", root.display());

    let rust_lines = code_lines(&rust);
    let mut problems = Vec::new();

    // 1. The HTTP client is used in the download module only, and there inside `fetch` only.
    let download = "crates/ocr/src/models/download.rs";
    for (path, n, line) in &rust_lines {
        if line.contains("ureq") && rel(path) != download {
            problems.push(format!("{}:{n} uses the HTTP client outside {download}", rel(path)));
        }
    }
    let source = fs::read_to_string(root.join(download)).expect("the download module exists");
    let body_start = source.find("pub(super) fn fetch(").expect("the download module has `fetch`");
    let body_end = source[body_start..].find("\n}\n").map(|i| body_start + i).expect("`fetch` ends");
    for (offset, _) in source.match_indices("ureq") {
        if offset < body_start || offset > body_end {
            problems.push(format!("{download} uses the HTTP client outside `fetch`"));
        }
    }

    // 2. `fetch` has one caller, `ModelStore::download`, and the app calls that from one
    //    command, the one behind the user's Download button.
    let fetch_calls: Vec<_> = rust_lines.iter().filter(|(_, _, l)| l.contains("download::fetch")).collect();
    if fetch_calls.len() != 1 || rel(&fetch_calls[0].0) != "crates/ocr/src/models/mod.rs" {
        problems.push(format!("`download::fetch` must have exactly one caller, in models/mod.rs: {fetch_calls:?}"));
    }
    let mut app = Vec::new();
    files(&root.join("src-tauri/src"), &["rs"], &mut app);
    let store_downloads: Vec<_> = code_lines(&app)
        .into_iter()
        .filter(|(_, _, l)| l.contains(".download(&") && l.contains("store"))
        .map(|(p, n, _)| format!("{}:{n}", rel(&p)))
        .collect();
    if store_downloads.len() != 1 || !store_downloads[0].starts_with("src-tauri/src/commands/ocr.rs") {
        problems.push(format!("the model download must be started from ocr_model_download only: {store_downloads:?}"));
    }

    // 3. No other way out in the core: sockets, other HTTP clients, URL loading by the system.
    const RUST_FORBIDDEN: &[&str] = &[
        "TcpStream",
        "UdpSocket",
        "std::net",
        "reqwest",
        "hyper",
        "isahc",
        "attohttpc",
        "curl",
        "NSURLSession",
        "NSURLConnection",
        "tauri_plugin_http",
        "WithURL",
        "with_url",
    ];
    for (path, n, line) in &rust_lines {
        for word in RUST_FORBIDDEN {
            if line.contains(word) {
                problems.push(format!("{}:{n} uses {word}", rel(path)));
            }
        }
    }

    // 4. The text layer and its wrapper make no request of their own.
    const WEB_FORBIDDEN: &[&str] =
        &["fetch(", "XMLHttpRequest", "WebSocket", "EventSource", "sendBeacon", "plugin-http", "http://", "https://"];
    for (path, n, line) in code_lines(&web) {
        for word in WEB_FORBIDDEN {
            if line.contains(word) {
                problems.push(format!("{}:{n} uses {word}", rel(&path)));
            }
        }
    }

    assert!(
        problems.is_empty(),
        "the OCR area may reach the network only through the model download:\n{}",
        problems.join("\n")
    );
}
