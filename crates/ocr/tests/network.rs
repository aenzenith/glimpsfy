//! Text recognition and speech to text run on the machine. The only network traffic the OCR and
//! speech areas may cause is a model download a user asks for (a PP-OCR model, the speech model),
//! and only to the hosts of the reviewed model tables. This test reads the areas' sources and
//! fails when a second way out appears: another HTTP client, a socket, a browser request in the
//! text layer or the trim window's text panel, or another caller of the download.

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
    for dir in ["crates/models/src", "crates/ocr/src", "crates/speech/src"] {
        files(&root.join(dir), &["rs"], &mut rust);
    }
    for file in [
        "src-tauri/src/commands/ocr.rs",
        "src-tauri/src/commands/transcript.rs",
        "src-tauri/src/recording/transcript.rs",
    ] {
        rust.push(root.join(file));
    }
    let mut web = Vec::new();
    files(&root.join("src/ocr"), &["ts", "vue"], &mut web);
    for file in
        ["src/lib/api/ocr.ts", "src/lib/api/transcript.ts", "src/recorder/Transcript.vue", "src/recorder/cuts.ts"]
    {
        web.push(root.join(file));
    }
    assert!(rust.len() > 5, "the OCR and speech sources were not found from {}", root.display());
    for path in rust.iter().chain(&web) {
        assert!(path.is_file(), "{} is missing; the test must read every file it names", rel(path));
    }

    let rust_lines = code_lines(&rust);
    let mut problems = Vec::new();

    // 1. The HTTP client is used in the download module only, and there inside `fetch` only.
    let download = "crates/models/src/download.rs";
    for (path, n, line) in &rust_lines {
        if line.contains("ureq") && rel(path) != download {
            problems.push(format!("{}:{n} uses the HTTP client outside {download}", rel(path)));
        }
    }
    let source = fs::read_to_string(root.join(download)).expect("the download module exists");
    let body_start = source.find("pub(crate) fn fetch(").expect("the download module has `fetch`");
    let body_end = source[body_start..].find("\n}\n").map(|i| body_start + i).expect("`fetch` ends");
    for (offset, _) in source.match_indices("ureq") {
        if offset < body_start || offset > body_end {
            problems.push(format!("{download} uses the HTTP client outside `fetch`"));
        }
    }

    // 2. `fetch` has one caller, `glimpsfy_models::download`; that has one caller per table (the
    //    PP-OCR models, the speech model); and the app starts each from one command, the one
    //    behind the user's Download button (Settings, and for speech the trim window's panel).
    let fetch_calls: Vec<_> = rust_lines.iter().filter(|(_, _, l)| l.contains("download::fetch")).collect();
    if fetch_calls.len() != 1 || rel(&fetch_calls[0].0) != "crates/models/src/lib.rs" {
        problems.push(format!("`download::fetch` must have exactly one caller, in crates/models: {fetch_calls:?}"));
    }
    let mut table_downloads: Vec<String> = rust_lines
        .iter()
        .filter(|(_, _, l)| l.contains("glimpsfy_models::download("))
        .map(|(p, _, _)| rel(p))
        .collect();
    table_downloads.sort();
    if table_downloads != ["crates/ocr/src/models/mod.rs", "crates/speech/src/model.rs"] {
        problems.push(format!("each model table must start the download in one place: {table_downloads:?}"));
    }
    let mut app = Vec::new();
    files(&root.join("src-tauri/src"), &["rs"], &mut app);
    let mut store_downloads: Vec<String> = code_lines(&app)
        .into_iter()
        .filter(|(_, _, l)| l.contains(".download(&") && l.contains("store"))
        .map(|(p, _, _)| rel(&p))
        .collect();
    store_downloads.sort();
    if store_downloads != ["src-tauri/src/commands/ocr.rs", "src-tauri/src/commands/transcript.rs"] {
        problems.push(format!(
            "the model downloads must be started from ocr_model_download and transcript_model_download only: \
             {store_downloads:?}"
        ));
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
        "the OCR and speech areas may reach the network only through the model downloads:\n{}",
        problems.join("\n")
    );
}
