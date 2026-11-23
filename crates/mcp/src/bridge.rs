//! `glimpsfy mcp`: the stdio server an agent starts. It joins the agent's stdin and stdout to the
//! running app's socket and does nothing else, so the app sees every message and decides alone.

use std::io::{self, Read, Write};

/// What the bridge prints when it cannot reach the app.
const UNREACHABLE: &str = "Glimpsfy is not running, or Settings › Agents does not let agents send requests.";

/// Runs the bridge and returns the process's exit code.
pub fn run(dev: bool) -> i32 {
    #[cfg(target_os = "macos")]
    {
        macos(dev)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = dev;
        eprintln!("glimpsfy mcp: the MCP server is available on macOS only.");
        2
    }
}

#[cfg(target_os = "macos")]
fn macos(dev: bool) -> i32 {
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;

    let stream = match crate::peer::socket_path(dev).and_then(UnixStream::connect) {
        Ok(stream) => stream,
        Err(error) => {
            eprintln!("glimpsfy mcp: {UNREACHABLE} ({error})");
            return 1;
        }
    };
    let Ok(mut to_app) = stream.try_clone() else {
        eprintln!("glimpsfy mcp: {UNREACHABLE}");
        return 1;
    };
    // The agent's messages go to the app; when the agent closes stdin, the app hears the end and
    // answers what is still running before it closes its side.
    let upstream = std::thread::spawn(move || {
        let _ = pump(&mut io::stdin().lock(), &mut to_app);
        let _ = to_app.shutdown(Shutdown::Write);
    });
    let mut from_app = stream;
    let result = pump(&mut from_app, &mut io::stdout().lock());
    drop(upstream);
    match result {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("glimpsfy mcp: {error}");
            1
        }
    }
}

/// Copies until the reader ends, flushing after every read so each line goes out at once.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn pump(from: &mut impl Read, to: &mut impl Write) -> io::Result<()> {
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let n = match from.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        to.write_all(&buffer[..n])?;
        to.flush()?;
    }
}
