//! The loopback HTTP server that plays recordings in WebKitGTK's `<video>`.
//!
//! WebKitGTK hands a `<video>` source to its GStreamer player, which loads `http`, `https`,
//! `blob` and `file` URLs and no URI scheme an app registers (WebKit bug 146351, open since
//! 2015; Tauri issue 3725). An `<img>` and `fetch` read `glimpsfy-recording://` and
//! `glimpsfy-history://` fine, so only the two video sources come from here: the trim window's
//! recording and the history player's. Everything else stays on the schemes.
//!
//! The server listens on 127.0.0.1 only, on a port the system picks, and answers only under a
//! random secret path, so another local user or a web page cannot name a file. It serves only
//! the files the schemes would serve (a take this run knows, a history entry), and only video
//! files among them, by ranges, streamed from the disk so a long recording is never read whole.

use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

/// The address and the secret path every URL starts with, once the server listens.
static BASE: OnceLock<Option<Base>> = OnceLock::new();

struct Base {
    /// `http://127.0.0.1:<port>/<secret>`.
    url: String,
    /// What a request's `Host` header must say, so a name that resolves to 127.0.0.1 in a
    /// browser (DNS rebinding) is not answered.
    host: String,
    secret: String,
}

/// How long a connection may take to send its request.
const READ_LIMIT: Duration = Duration::from_secs(10);
/// How long the player may leave a response unread before the connection is dropped.
const WRITE_LIMIT: Duration = Duration::from_secs(30);
/// The longest request head that is read.
const HEAD_LIMIT: usize = 16 * 1024;

/// Where video sources start (`http://127.0.0.1:<port>/<secret>`), starting the server on the
/// first call. `None` when it could not listen, and the pages keep the schemes' URLs.
pub fn base() -> Option<&'static str> {
    BASE.get_or_init(|| match listen() {
        Ok(base) => Some(base),
        Err(error) => {
            eprintln!("[glimpsfy] the loopback server for video previews could not start: {error}");
            None
        }
    })
    .as_ref()
    .map(|base| base.url.as_str())
}

/// The URL a take of this run plays from in the trim window.
pub fn recording_url(id: &str) -> Option<String> {
    base().map(|base| format!("{base}/recording/{id}"))
}

fn listen() -> std::io::Result<Base> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let secret = secret()?;
    let base = Base { url: format!("http://127.0.0.1:{port}/{secret}"), host: format!("127.0.0.1:{port}"), secret };
    std::thread::Builder::new().name("glimpsfy-loopback".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            // A connection per thread: the player opens a few at once and holds them while it
            // plays, and a stalled one must not hold up the next.
            let _ = std::thread::Builder::new().name("glimpsfy-loopback-conn".into()).spawn(move || {
                let _ = serve(stream);
            });
        }
    })?;
    Ok(base)
}

/// 128 random bits from the kernel, as hex.
fn secret() -> std::io::Result<String> {
    let mut bytes = [0u8; 16];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// A request as far as this server reads it.
struct Asked {
    head_only: bool,
    path: String,
    host: Option<String>,
    range: Option<String>,
}

fn read_request(stream: &TcpStream) -> Option<Result<Asked, u16>> {
    let mut reader = BufReader::new(stream.try_clone().ok()?).take(HEAD_LIMIT as u64);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?.to_string(), parts.next()?.to_string());
    let (mut host, mut range) = (None, None);
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).ok()? == 0 {
            return None;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        let Some((name, value)) = header.split_once(':') else { continue };
        if name.eq_ignore_ascii_case("host") {
            host = Some(value.trim().to_string());
        } else if name.eq_ignore_ascii_case("range") {
            range = Some(value.trim().to_string());
        }
    }
    Some(match method.as_str() {
        "GET" | "HEAD" => Ok(Asked { head_only: method == "HEAD", path, host, range }),
        _ => Err(405),
    })
}

fn serve(stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_LIMIT))?;
    stream.set_write_timeout(Some(WRITE_LIMIT))?;
    let asked = match read_request(&stream) {
        Some(Ok(asked)) => asked,
        Some(Err(status)) => return reply(&stream, status, &[]),
        None => return Ok(()),
    };
    let Some(Some(base)) = BASE.get() else { return reply(&stream, 503, &[]) };
    if asked.host.as_deref() != Some(base.host.as_str()) {
        return reply(&stream, 403, &[]);
    }
    let Some(path) = locate(base, &asked.path) else { return reply(&stream, 404, &[]) };
    let Some(kind) = video_type(&path) else { return reply(&stream, 404, &[]) };
    let Ok(mut file) = File::open(&path) else { return reply(&stream, 404, &[]) };
    let length = file.metadata().map(|m| m.len())?;

    let (status, start, end) = match asked.range.as_deref() {
        Some(value) => match range(value, length) {
            Some((start, end)) => (206, start, end),
            None => return reply(&stream, 416, &[("Content-Range", format!("bytes */{length}"))]),
        },
        None => (200, 0, length.saturating_sub(1)),
    };
    let size = if length == 0 { 0 } else { end - start + 1 };
    let mut headers = vec![
        ("Content-Type", kind.to_string()),
        ("Content-Length", size.to_string()),
        ("Accept-Ranges", "bytes".to_string()),
        ("Cache-Control", "no-cache".to_string()),
    ];
    if status == 206 {
        headers.push(("Content-Range", format!("bytes {start}-{end}/{length}")));
    }
    let mut out = BufWriter::new(&stream);
    write_head(&mut out, status, &headers)?;
    if !asked.head_only && size > 0 {
        file.seek(SeekFrom::Start(start))?;
        std::io::copy(&mut file.take(size), &mut out)?;
    }
    out.flush()
}

/// The file `/<secret>/recording/<id>` or `/<secret>/history/<id>` names, if the trim window
/// or the history would serve it.
fn locate(base: &Base, path: &str) -> Option<PathBuf> {
    let (kind, id) = asked_for(&base.secret, path)?;
    let app = super::app()?;
    match kind {
        "recording" => crate::recording::media::known_file(app, id),
        "history" => crate::commands::history::entry_file(app, id),
        _ => None,
    }
}

/// The kind and id `/<secret>/<kind>/<id>` asks for, when the path starts with this run's secret.
fn asked_for<'a>(secret: &str, path: &'a str) -> Option<(&'a str, &'a str)> {
    let path = path.split('?').next()?.trim_start_matches('/');
    let mut parts = path.splitn(3, '/');
    let (given, kind, id) = (parts.next()?, parts.next()?, parts.next()?);
    same(given.as_bytes(), secret.as_bytes()).then_some((kind, id))
}

/// Compares the secret in time that does not depend on where it differs.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
}

fn video_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_string_lossy().to_ascii_lowercase().as_str() {
        "mp4" | "m4v" => Some("video/mp4"),
        "mov" => Some("video/quicktime"),
        _ => None,
    }
}

/// The byte range of a `Range: bytes=a-b` header within a file of `length` bytes.
fn range(header: &str, length: u64) -> Option<(u64, u64)> {
    let spec = header.trim().strip_prefix("bytes=")?.split(',').next()?.trim();
    let (start, end) = spec.split_once('-')?;
    let last = length.checked_sub(1)?;
    let (start, end) = match (start.trim(), end.trim()) {
        ("", suffix) => (length.saturating_sub(suffix.parse().ok()?), last),
        (start, "") => (start.parse().ok()?, last),
        (start, end) => (start.parse().ok()?, end.parse::<u64>().ok()?.min(last)),
    };
    (start <= end && start < length).then_some((start, end))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        206 => "Partial Content",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        416 => "Range Not Satisfiable",
        _ => "Service Unavailable",
    }
}

fn write_head(out: &mut impl Write, status: u16, headers: &[(&str, String)]) -> std::io::Result<()> {
    write!(out, "HTTP/1.1 {status} {}\r\n", reason(status))?;
    for (name, value) in headers {
        write!(out, "{name}: {value}\r\n")?;
    }
    // One request per connection keeps the server simple; the player opens another.
    write!(out, "Connection: close\r\n\r\n")
}

fn reply(stream: &TcpStream, status: u16, headers: &[(&str, String)]) -> std::io::Result<()> {
    let mut all = headers.to_vec();
    all.push(("Content-Length", "0".to_string()));
    let mut out = BufWriter::new(stream);
    write_head(&mut out, status, &all)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first line of the server's answer to `request`.
    fn status_of(host: &str, request: &str) -> String {
        let mut stream = TcpStream::connect(host).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        answer.lines().next().unwrap_or_default().to_string()
    }

    /// Any local process and any web page can reach the port, so only a path with this run's
    /// secret names a file.
    #[test]
    fn a_path_names_a_file_only_under_the_secret() {
        let secret = "0123456789abcdef0123456789abcdef";
        assert_eq!(asked_for(secret, &format!("/{secret}/recording/r-1")), Some(("recording", "r-1")));
        assert_eq!(asked_for(secret, &format!("/{secret}/history/e-1?t=2")), Some(("history", "e-1")));
        assert_eq!(asked_for(secret, "/recording/r-1"), None);
        assert_eq!(asked_for(secret, "/0123456789abcdef0123456789abcdee/recording/r-1"), None);
        assert_eq!(asked_for(secret, "/0123456789abcdef/recording/r-1"), None);
        assert_eq!(asked_for(secret, &format!("/{secret}0/recording/r-1")), None);
        assert_eq!(asked_for(secret, &format!("/{secret}/recording")), None);
        assert_eq!(asked_for(secret, "//recording/r-1"), None);
    }

    /// A page served from another name that resolves to 127.0.0.1 (DNS rebinding) is refused
    /// even with the secret, and nothing but a read is answered.
    #[test]
    fn only_reads_addressed_to_the_server_are_answered() {
        base().expect("the server listens");
        let Some(Some(server)) = BASE.get() else { unreachable!() };
        let path = format!("/{}/recording/r-1", server.secret);
        let ask = |method: &str, host: Option<&str>| {
            let host = host.map(|h| format!("Host: {h}\r\n")).unwrap_or_default();
            status_of(&server.host, &format!("{method} {path} HTTP/1.1\r\n{host}\r\n"))
        };
        assert!(ask("GET", Some("rebound.example")).starts_with("HTTP/1.1 403"));
        assert!(ask("GET", Some("localhost")).starts_with("HTTP/1.1 403"));
        assert!(ask("GET", None).starts_with("HTTP/1.1 403"));
        assert!(ask("POST", Some(&server.host)).starts_with("HTTP/1.1 405"));
        assert!(ask("PUT", Some(&server.host)).starts_with("HTTP/1.1 405"));
        // The right host and secret reach the lookup, which knows no take in a test.
        assert!(ask("GET", Some(&server.host)).starts_with("HTTP/1.1 404"));
    }
}
