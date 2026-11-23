//! The socket agents connect to: one accept thread, the peer checks, and one thread per
//! connection that runs the MCP session (`glimpsfy_mcp::server::serve`).

use std::collections::HashMap;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use glimpsfy_mcp::gate::Gate;
use glimpsfy_mcp::peer;
use glimpsfy_mcp::server::{self, ServerInfo};
use tauri::AppHandle;

use super::host::AppHost;
use crate::trace;

pub struct Listener {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    /// Each open connection's stream, so Settings can close one and turning the server off closes
    /// them all.
    streams: Arc<Mutex<HashMap<u64, UnixStream>>>,
}

impl Listener {
    pub fn start(app: &AppHandle, gate: Arc<Gate>) -> std::io::Result<Listener> {
        let path = peer::socket_path(cfg!(debug_assertions))?;
        let socket = peer::bind(&path)?;
        let own = std::env::current_exe()?;
        let stop = Arc::new(AtomicBool::new(false));
        let streams: Arc<Mutex<HashMap<u64, UnixStream>>> = Arc::default();
        let host = Arc::new(AppHost { app: app.clone() });
        let version = app.package_info().version.to_string();
        {
            let (app, stop, streams) = (app.clone(), stop.clone(), streams.clone());
            std::thread::Builder::new()
                .name("glimpsfy-agents".into())
                .spawn(move || accept(&app, socket, &own, &stop, &streams, host, gate, version))?;
        }
        trace::log(format!("[glimpsfy] agents: listening on {}", path.display()));
        Ok(Listener { path, stop, streams })
    }

    /// Closes the socket and every connection; their sessions end their requests as they leave.
    pub fn stop(self) {
        self.stop.store(true, Ordering::SeqCst);
        // The accept call wakes for this connection and sees the stop.
        let _ = UnixStream::connect(&self.path);
        let _ = std::fs::remove_file(&self.path);
        for stream in self.streams.lock().unwrap_or_else(|p| p.into_inner()).values() {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    pub fn close(&self, connection: u64) -> bool {
        match self.streams.lock().unwrap_or_else(|p| p.into_inner()).get(&connection) {
            Some(stream) => stream.shutdown(Shutdown::Both).is_ok(),
            None => false,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn accept(
    app: &AppHandle,
    socket: UnixListener,
    own: &std::path::Path,
    stop: &AtomicBool,
    streams: &Arc<Mutex<HashMap<u64, UnixStream>>>,
    host: Arc<AppHost>,
    gate: Arc<Gate>,
    version: String,
) {
    for incoming in socket.incoming() {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let stream = match incoming {
            Ok(stream) => stream,
            Err(error) => {
                trace::log(format!("[glimpsfy] agents: accept: {error}"));
                continue;
            }
        };
        let peer = match peer::verify(&stream, own) {
            Ok(peer) => peer,
            Err(refusal) => {
                trace::log(format!("[glimpsfy] agents: a connection was refused: {refusal}"));
                continue;
            }
        };
        let process = peer::describe(peer.pid);
        let connection = super::connected(app, process);
        let (Ok(reader), Ok(closer)) = (stream.try_clone(), stream.try_clone()) else {
            continue;
        };
        streams.lock().unwrap_or_else(|p| p.into_inner()).insert(connection, closer);
        let (host, gate, streams, info) =
            (host.clone(), gate.clone(), streams.clone(), ServerInfo { version: version.clone() });
        let spawned = std::thread::Builder::new().name(format!("glimpsfy-agent-{connection}")).spawn(move || {
            server::serve(host, gate, connection, info, reader, stream);
            streams.lock().unwrap_or_else(|p| p.into_inner()).remove(&connection);
        });
        if let Err(error) = spawned {
            trace::log(format!("[glimpsfy] agents: connection thread: {error}"));
        }
    }
}
