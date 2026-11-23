//! The app's two network switches (Settings › General › Network) and the one check every client
//! asks before it opens a connection: the upload client (`crates/upload/src/http.rs`, through its
//! gate), the model download (`crates/models/src/download.rs`, through `online`) and the updater
//! (`update.rs`). The pages reach no remote origin because the CSP in `tauri.conf.json` names local
//! ones only. A link opened in the browser, the agents' local socket and the loopback server that
//! plays `<video>` on Linux are not connections Glimpsfy makes to the network, so no switch stops
//! them. The structural test below keeps this list complete.

use tauri::{AppHandle, Manager};

use crate::commands::error::{CommandError, ErrorCode};
use crate::settings::General;
use crate::AppState;

/// What a connection is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Traffic {
    /// An upload, a connection test or a deletion at an upload host.
    Upload,
    /// A PP-OCR or speech model the user downloads.
    ModelDownload,
    /// Glimpsfy's own update: the ask at the release and its download.
    UpdateCheck,
}

/// The switch that keeps a connection from opening.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// Work offline is on.
    Offline,
    /// Allow uploads is off.
    UploadsOff,
}

/// Whether the switches stop `traffic`. Work offline comes first, so it stops uploads too
/// whatever Allow uploads says, and the pages show that reason.
pub fn blocked(general: &General, traffic: Traffic) -> Option<Blocked> {
    if general.offline {
        Some(Blocked::Offline)
    } else if traffic == Traffic::Upload && !general.allow_uploads {
        Some(Blocked::UploadsOff)
    } else {
        None
    }
}

/// The switches as they are now. Before the settings are loaded the answer is closed: a
/// connection that cannot learn the switches does not open.
pub fn gate(app: &AppHandle, traffic: Traffic) -> Option<Blocked> {
    match app.try_state::<AppState>() {
        Some(state) => state.settings.read(|settings| blocked(&settings.general, traffic)),
        None => Some(Blocked::Offline),
    }
}

/// Whether `traffic` may open a connection now, for the clients that take a plain `online`.
pub fn online(app: &AppHandle, traffic: Traffic) -> bool {
    gate(app, traffic).is_none()
}

/// A command's early refusal: `uploads_off` or `offline` before it does anything else. The client
/// asks the gate again before every connection, so this only saves the work in between.
pub fn admit(app: &AppHandle, traffic: Traffic) -> Result<(), CommandError> {
    gate(app, traffic).map_or(Ok(()), |blocked| Err(blocked.into()))
}

impl From<Blocked> for CommandError {
    fn from(blocked: Blocked) -> Self {
        match blocked {
            Blocked::UploadsOff => {
                CommandError::new(ErrorCode::UploadsOff, "uploads are turned off in Settings, so nothing was sent")
            }
            Blocked::Offline => {
                CommandError::new(ErrorCode::Offline, "Glimpsfy is offline, so it opened no connection")
            }
        }
    }
}

impl From<Blocked> for glimpsfy_upload::SwitchedOff {
    fn from(blocked: Blocked) -> Self {
        match blocked {
            Blocked::UploadsOff => glimpsfy_upload::SwitchedOff::Uploads,
            Blocked::Offline => glimpsfy_upload::SwitchedOff::Offline,
        }
    }
}

#[cfg(test)]
mod tests {
    //! Switch off means no request leaves: the switches are kept only if every way out of the app
    //! is one of the gated clients and asks its gate first. This test reads the sources, the
    //! manifests, the CSP and the capabilities, and fails when a client, a socket or a remote
    //! origin appears that the switches would not stop.

    use std::fs;
    use std::path::{Path, PathBuf};

    fn repo() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    fn rel(path: &Path) -> String {
        path.strip_prefix(repo()).unwrap_or(path).to_string_lossy().replace('\\', "/")
    }

    fn files(dir: &Path, extension: &str, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files(&path, extension, out);
            } else if path.extension().is_some_and(|e| e == extension) {
                out.push(path);
            }
        }
    }

    fn read(path: &str) -> String {
        fs::read_to_string(repo().join(path)).unwrap_or_else(|e| panic!("{path} cannot be read: {e}"))
    }

    /// The code of a Rust source without its line comments.
    fn code(text: &str) -> String {
        text.lines()
            .filter(|line| {
                let line = line.trim_start();
                !(line.starts_with("//") || line.starts_with("/*") || line.starts_with('*'))
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The body of the first function whose header contains `header`: up to the first line that
    /// closes at the header's indentation.
    fn body<'a>(text: &'a str, header: &str) -> &'a str {
        let start = text.find(header).unwrap_or_else(|| panic!("`{header}` is missing"));
        let indent = text[..start].rsplit('\n').next().map_or(0, str::len);
        let close = format!("\n{}}}", " ".repeat(indent));
        let end = text[start..].find(&close).map_or(text.len(), |i| start + i);
        &text[start..end]
    }

    fn before(text: &str, first: &str, then: &str) -> bool {
        matches!((text.find(first), text.find(then)), (Some(a), Some(b)) if a < b)
    }

    #[test]
    fn no_request_leaves_while_uploads_or_the_network_are_off() {
        let root = repo();
        let mut sources = Vec::new();
        for crate_dir in fs::read_dir(root.join("crates")).expect("the crates folder").flatten() {
            files(&crate_dir.path().join("src"), "rs", &mut sources);
        }
        files(&root.join("src-tauri/src"), "rs", &mut sources);
        assert!(sources.len() > 50, "the sources were not found from {}", root.display());
        let mut problems = Vec::new();

        // 1. Three clients reach the network: the upload client (its providers name only the
        //    method type), the model download and the updater. No other client, socket or URL
        //    loader exists. The loopback server that plays `<video>` on Linux listens on
        //    127.0.0.1 and the agents' MCP server on a Unix socket; neither reaches the network.
        const HTTP_FILES: &[&str] = &["crates/upload/src/http.rs", "crates/models/src/download.rs"];
        const FORBIDDEN: &[&str] = &[
            "reqwest",
            "hyper::",
            "isahc",
            "attohttpc",
            "curl",
            "wget",
            "tauri_plugin_http",
            "NSURLSession",
            "NSURLConnection",
            "NSURLRequest",
            "ContentsOfURL",
            "UdpSocket",
            "ToSocketAddrs",
            "TcpStream",
            "TcpListener",
        ];
        for path in &sources {
            let name = rel(path);
            // This file names the words it looks for.
            if name == "src-tauri/src/network.rs" {
                continue;
            }
            let text = code(&fs::read_to_string(path).expect("a source file"));
            for (n, line) in text.lines().enumerate() {
                if line.contains("ureq") && !HTTP_FILES.contains(&name.as_str()) {
                    let method_type =
                        name.starts_with("crates/upload/src/hosts/") && line.trim() == "use ureq::http::Method;";
                    if !method_type {
                        problems.push(format!("{name}: `{}` uses the HTTP client outside its gate", line.trim()));
                    }
                }
                for word in FORBIDDEN {
                    let local = name == "src-tauri/src/desktop/loopback.rs" && word.starts_with("Tcp");
                    if line.contains(word) && !local {
                        problems.push(format!("{name}: line {} uses {word}", n + 1));
                    }
                }
                let updater = line.contains(".updater(") || line.contains("UpdaterExt");
                if updater && name != "src-tauri/src/update.rs" {
                    problems.push(format!("{name}: the updater is asked outside update.rs"));
                }
                if line.contains("HttpClient::new(") && name != "src-tauri/src/commands/upload.rs" {
                    problems.push(format!("{name}: an upload client is made outside commands/upload.rs"));
                }
            }
        }
        let loopback = code(&read("src-tauri/src/desktop/loopback.rs"));
        if loopback.matches("::bind(").count() != 1 || !loopback.contains("TcpListener::bind((Ipv4Addr::LOCALHOST, 0))")
        {
            problems.push("the loopback server must listen on 127.0.0.1 only".into());
        }
        let peer = code(&read("crates/mcp/src/peer.rs"));
        if !peer.contains("UnixListener::bind(") {
            problems.push("the agents' MCP server must listen on its Unix socket".into());
        }

        // 2. Each client asks its gate before it connects, and again while it runs.
        let http = code(&read("crates/upload/src/http.rs"));
        let send = body(&http, "pub fn send(");
        if !before(send, "self.client.admit()?", "agent.run(") {
            problems.push("Connection::send must ask the gate before the request".into());
        }
        if !body(&http, "fn read(&mut self, buf").contains("switched_off()") {
            problems.push("the body reader must ask the gate before every chunk".into());
        }
        let upload = code(&read("src-tauri/src/commands/upload.rs"));
        let made = upload.find("HttpClient::new(").map(|i| &upload[i..]).unwrap_or("");
        if !made.split(';').next().is_some_and(|s| s.contains(".with_gate(") && s.contains("network::gate(")) {
            problems.push("the app's upload client must carry the switches as its gate".into());
        }
        let download = code(&read("crates/models/src/download.rs"));
        let fetch = body(&download, "pub(crate) fn fetch(");
        if !before(fetch, "stay_offline(online)?", "reviewed.files")
            || !before(fetch, "stay_offline(online)?", "agent.get(")
        {
            problems.push("the model fetch must ask `online` before any other check".into());
        }
        let hop = fetch.find("for _ in 0..=MAX_REDIRECTS").map(|i| &fetch[i..]).unwrap_or("");
        if !before(hop, "stay_offline(online)?", "agent.get(") {
            problems.push("the model fetch must ask `online` before every hop".into());
        }
        let chunk = fetch.find("loop {").map(|i| &fetch[i..]).unwrap_or("");
        if !before(chunk, "stay_offline(online)?", "reader.read(") {
            problems.push("the model fetch must ask `online` before every chunk".into());
        }
        for file in ["src-tauri/src/commands/ocr.rs", "src-tauri/src/commands/transcript.rs"] {
            let text = code(&read(file));
            let calls: Vec<_> = text.lines().filter(|l| l.contains("store.download(")).collect();
            if calls.is_empty() || !calls.iter().all(|l| l.contains("network::online(&app, Traffic::ModelDownload)")) {
                problems.push(format!("{file}: the model download must take the offline switch as `online`"));
            }
        }
        let update = code(&read("src-tauri/src/update.rs"));
        let ask = body(&update, "async fn ask(");
        if !before(ask, "network::gate(app, Traffic::UpdateCheck)", "updater.check()")
            || !ask.contains("while_online(app, updater.check())")
            || !ask.contains("while_online(app, update.download(")
        {
            problems.push("the updater must ask the gate before and while it asks and downloads".into());
        }
        if !before(body(&update, "pub fn check("), "network::gate(", "self.busy") {
            problems.push("Update::check must change nothing while the app is offline".into());
        }

        // 3. The manifests bring no other HTTP client.
        let mut manifests = vec![root.join("Cargo.toml"), root.join("src-tauri/Cargo.toml")];
        for crate_dir in fs::read_dir(root.join("crates")).expect("the crates folder").flatten() {
            manifests.push(crate_dir.path().join("Cargo.toml"));
        }
        for manifest in &manifests {
            let name = rel(manifest);
            let text = fs::read_to_string(manifest).unwrap_or_default();
            for line in text.lines().map(|l| l.split('#').next().unwrap_or("").trim()) {
                let dependency = line.split(['=', ' ', '.']).next().unwrap_or("");
                let client = matches!(
                    dependency,
                    "reqwest" | "hyper" | "isahc" | "attohttpc" | "curl" | "surf" | "tauri-plugin-http" | "ureq"
                );
                let allowed = dependency == "ureq"
                    && ["crates/upload/Cargo.toml", "crates/models/Cargo.toml"].contains(&name.as_str());
                if client && !allowed {
                    problems.push(format!("{name} depends on the HTTP client {dependency}"));
                }
            }
        }

        // 4. The pages reach no remote origin, and they cannot drive the updater themselves.
        let config: serde_json::Value =
            serde_json::from_str(&read("src-tauri/tauri.conf.json")).expect("tauri.conf.json");
        for key in ["csp", "devCsp"] {
            let csp = config["app"]["security"][key].as_str().unwrap_or_else(|| panic!("the {key} is missing"));
            for directive in csp.split(';') {
                for source in directive.split_whitespace().skip(1) {
                    let local = matches!(source, "'self'" | "'unsafe-inline'" | "data:" | "blob:" | "ipc:")
                        || (source.starts_with("glimpsfy-") && source.ends_with(':'))
                        || source.strip_prefix("http://").is_some_and(|h| h.ends_with(".localhost"))
                        || source == "http://127.0.0.1:*"
                        || ["http://localhost:", "ws://localhost:"].iter().any(|p| {
                            source.strip_prefix(p).is_some_and(|port| port.chars().all(|c| c.is_ascii_digit()))
                        });
                    if !local {
                        problems.push(format!("the {key} lets the pages reach {source}"));
                    }
                }
            }
        }
        let mut capabilities = Vec::new();
        files(&root.join("src-tauri/capabilities"), "json", &mut capabilities);
        assert!(!capabilities.is_empty(), "the capabilities were not found");
        for path in &capabilities {
            let text = fs::read_to_string(path).expect("a capability");
            for permission in ["\"updater:", "\"http:", "\"websocket:"] {
                if text.contains(permission) {
                    problems.push(format!("{} gives the pages {permission}…", rel(path)));
                }
            }
        }

        assert!(problems.is_empty(), "a way out of the app does not stop at the switches:\n{}", problems.join("\n"));
    }
}
