//! The one HTTP client of the upload area, and the connection a provider sends through.
//!
//! **Why ureq and not reqwest.** The provider contract is blocking, and an upload is one request
//! whose body streams from a file. ureq 3 streams a body from any `Read`, which is where progress
//! and cancellation hook in, and needs no async runtime; reqwest would bring hyper, tower and a
//! runtime of its own for the same single request. ureq runs on rustls with the platform verifier,
//! so it trusts what the system trusts, and it never retries a request by itself.
//!
//! **The client** sends `Glimpsfy/<version>` as its user agent and nothing else about the machine.
//! It follows no redirects (a 3xx answer comes back to the provider as it is), and it has
//! time-outs for every phase except the body, which instead stops when one write stalls.
//!
//! **A connection** is the client bound to the origins of one confirmed destination. It refuses
//! any URL outside them before a socket opens, and only `engine` can open one.
//!
//! **The gate** is the app's switches (Allow uploads, Work offline) as the client sees them. It is
//! asked before every request and before every 64 KB of a streamed body, so a switch turned off in
//! the middle of an upload stops it too. A client without a gate (`for_tests`) is always open.

use std::io::{self, Cursor, Read};
use std::sync::Arc;
use std::time::Duration;

use ureq::http::{HeaderName, HeaderValue, Method};
use ureq::tls::{RootCerts, TlsConfig};
use ureq::{Agent, SendBody};
use url::Url;

use crate::{CancelToken, ProviderId, Result, SwitchedOff, UploadData, UploadError};

/// The answer is read up to this size; a link or an error never needs more.
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
/// Progress is reported at most this often, by bytes.
const PROGRESS_STEP: u64 = 64 * 1024;

/// The app's switches as the client asks them: `Some` names the switch that keeps a request
/// from leaving now. It reads the live settings each time and answers closed when it cannot.
pub type Gate = Arc<dyn Fn() -> Option<SwitchedOff> + Send + Sync>;

/// The shared client. Clone it freely; clones share the connection pool and the gate.
#[derive(Clone)]
pub struct HttpClient {
    agent: Agent,
    gate: Option<Gate>,
}

impl HttpClient {
    /// The client with the platform's certificate verifier and the user agent
    /// `Glimpsfy/<version>`.
    pub fn new(version: &str) -> HttpClient {
        let tls = TlsConfig::builder().root_certs(RootCerts::PlatformVerifier).build();
        HttpClient::with_tls(version, tls)
    }

    /// The client for tests against a local plain-HTTP server, which needs no certificates.
    pub fn for_tests() -> HttpClient {
        HttpClient::with_tls("test", TlsConfig::builder().build())
    }

    fn with_tls(version: &str, tls: TlsConfig) -> HttpClient {
        let config = Agent::config_builder()
            .tls_config(tls)
            .user_agent(format!("Glimpsfy/{version}"))
            // Every status comes back as a response, so a provider can quote the host's reason.
            .http_status_as_error(false)
            // A redirect would leave the confirmed host; the provider sees the 3xx itself.
            .max_redirects(0)
            .max_redirects_will_error(false)
            .accept_encoding("identity")
            .timeout_resolve(Some(Duration::from_secs(20)))
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_send_request(Some(Duration::from_secs(30)))
            // A large recording on a slow line takes as long as it takes, but one write that
            // stalls for a minute means the line is gone.
            .timeout_per_write(Some(Duration::from_secs(60)))
            .timeout_recv_response(Some(Duration::from_secs(120)))
            .timeout_recv_body(Some(Duration::from_secs(60)))
            .build();
        HttpClient { agent: config.new_agent(), gate: None }
    }

    /// The client that asks `gate` before every request and every 64 KB of a body.
    pub fn with_gate(mut self, gate: Gate) -> HttpClient {
        self.gate = Some(gate);
        self
    }

    /// The switch that keeps requests from leaving now, if one does.
    pub(crate) fn switched_off(&self) -> Option<SwitchedOff> {
        self.gate.as_ref().and_then(|gate| gate())
    }

    /// Fails with [`UploadError::SwitchedOff`] while a switch keeps requests from leaving.
    pub(crate) fn admit(&self) -> Result<()> {
        match self.switched_off() {
            Some(off) => Err(UploadError::SwitchedOff(off)),
            None => Ok(()),
        }
    }
}

/// A scheme, host and port, the unit a destination is confirmed for.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    pub scheme: String,
    /// Lower case, without brackets around an IPv6 address.
    pub host: String,
    pub port: u16,
}

impl Origin {
    /// The origin of an `http` or `https` URL; any other scheme is refused.
    pub fn of(url: &Url) -> Option<Origin> {
        let scheme = url.scheme();
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let host = url.host_str()?.trim_start_matches('[').trim_end_matches(']').to_ascii_lowercase();
        Some(Origin { scheme: scheme.into(), host, port: url.port_or_known_default()? })
    }

    /// Whether the host is this machine.
    pub fn is_local(&self) -> bool {
        self.host == "localhost" || self.host.ends_with(".localhost") || self.host == "127.0.0.1" || self.host == "::1"
    }

    /// `host` or `host:port` when the port is not the scheme's own, as the user reads it.
    pub fn display(&self) -> String {
        let default = if self.scheme == "https" { 443 } else { 80 };
        if self.port == default {
            self.host.clone()
        } else if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Parses an `http` or `https` URL; anything else, or a URL without a host, is refused.
pub fn parse_http_url(text: &str) -> Option<Url> {
    let url = Url::parse(text.trim()).ok()?;
    Origin::of(&url)?;
    Some(url)
}

/// The client bound to the origins of one confirmed destination.
pub struct Connection {
    client: HttpClient,
    provider: ProviderId,
    origins: Vec<Origin>,
    cancel: CancelToken,
}

impl Connection {
    /// Only the engine opens a connection, after the consent check.
    pub(crate) fn open(client: &HttpClient, provider: ProviderId, origins: Vec<Origin>, cancel: CancelToken) -> Self {
        Connection { client: client.clone(), provider, origins, cancel }
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    /// Sends one request and reads the answer. A URL outside the confirmed origins is refused
    /// before anything is sent; a cancelled body ends with [`UploadError::Cancelled`].
    pub fn send(&self, request: Request<'_>) -> Result<Response> {
        // The switches come first: with uploads off or the app offline nothing is even checked.
        self.client.admit()?;
        let url = request.url;
        let origin = Origin::of(&url).ok_or_else(|| UploadError::HostNotAllowed {
            provider: self.provider.clone(),
            host: url.scheme().to_string(),
        })?;
        if !self.origins.contains(&origin) {
            return Err(UploadError::HostNotAllowed { provider: self.provider.clone(), host: origin.display() });
        }
        if self.cancel.is_cancelled() {
            return Err(UploadError::Cancelled);
        }

        let mut builder = ureq::http::Request::builder().method(request.method.clone()).uri(url.as_str());
        for (name, value) in &request.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| UploadError::invalid(&self.provider, "headers", format!("{name} is not a header name")))?;
            let value = HeaderValue::from_str(value).map_err(|_| {
                UploadError::invalid(&self.provider, "headers", format!("the value of {name} cannot be sent"))
            })?;
            builder = builder.header(name, value);
        }

        let result = match request.body {
            Body::Empty => {
                let req = builder.body(()).map_err(|e| UploadError::Network(e.to_string()))?;
                self.client.agent.run(req)
            }
            Body::Bytes(bytes) => {
                let req = builder.body(bytes).map_err(|e| UploadError::Network(e.to_string()))?;
                self.client.agent.run(req)
            }
            Body::Stream { reader, length, progress } => {
                let mut counting = CountingReader {
                    inner: reader,
                    sent: 0,
                    reported: 0,
                    cancel: self.cancel.clone(),
                    client: self.client.clone(),
                    progress,
                };
                let req = builder
                    .header("content-length", length)
                    .body(SendBody::from_reader(&mut counting))
                    .map_err(|e| UploadError::Network(e.to_string()))?;
                let result = self.client.agent.run(req);
                if let Some(progress) = counting.progress {
                    progress(counting.sent);
                }
                result
            }
        };

        let mut response = match result {
            Ok(response) => response,
            Err(_) if self.cancel.is_cancelled() => return Err(UploadError::Cancelled),
            Err(error) => {
                // A body the gate stopped reads as a broken connection to ureq.
                self.client.admit()?;
                return Err(UploadError::Network(network_reason(&error)));
            }
        };
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_ascii_lowercase(), String::from_utf8_lossy(value.as_bytes()).into()))
            .collect();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES)
            .read_to_vec()
            .map_err(|error| UploadError::Network(network_reason(&error)))?;
        Ok(Response { status, headers, body, url: url.to_string() })
    }
}

/// The reason of a failed connection in plain words, without the request's URL (a query string
/// may hold a key).
fn network_reason(error: &ureq::Error) -> String {
    use ureq::Error as E;
    match error {
        E::Timeout(phase) => format!("the host did not answer in time ({phase})"),
        E::HostNotFound => "the host name could not be found".into(),
        E::ConnectionFailed => "the connection to the host failed".into(),
        E::Io(io) => format!("the connection broke ({})", io.kind()),
        E::Tls(_) | E::Rustls(_) => "the secure connection could not be set up".into(),
        E::BodyExceedsLimit(_) => "the host's answer is larger than Glimpsfy reads".into(),
        _ => "the request could not be completed".into(),
    }
}

/// One request a provider sends.
pub struct Request<'a> {
    pub method: Method,
    pub url: Url,
    pub headers: Vec<(String, String)>,
    pub body: Body<'a>,
}

impl<'a> Request<'a> {
    pub fn new(method: Method, url: Url) -> Request<'a> {
        Request { method, url, headers: Vec::new(), body: Body::Empty }
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub fn body(mut self, body: Body<'a>) -> Self {
        self.body = body;
        self
    }
}

/// What a request carries.
pub enum Body<'a> {
    Empty,
    /// A small body held in memory.
    Bytes(Vec<u8>),
    /// A body streamed from a reader of a known length; `progress` hears the bytes sent so far.
    Stream {
        reader: Box<dyn Read + Send + 'a>,
        length: u64,
        progress: Option<&'a dyn Fn(u64)>,
    },
}

/// A host's answer, its body read up to 1 MiB.
#[derive(Clone, Debug)]
pub struct Response {
    pub status: u16,
    /// Header names in lower case, in the order they came.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// The URL the request went to.
    pub url: String,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        let name = name.to_ascii_lowercase();
        self.headers.iter().find(|(n, _)| *n == name).map(|(_, v)| v.as_str())
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Counts what the client reads from the body, reports it, and stops when the upload is
/// cancelled or a switch closes the gate.
struct CountingReader<'a> {
    inner: Box<dyn Read + Send + 'a>,
    sent: u64,
    reported: u64,
    cancel: CancelToken,
    client: HttpClient,
    progress: Option<&'a dyn Fn(u64)>,
}

impl Read for CountingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other("the upload was cancelled"));
        }
        // Every read of the body asks, so a switch turned off mid-upload stops it within one chunk.
        if let Some(off) = self.client.switched_off() {
            return Err(io::Error::other(off.to_string()));
        }
        let n = self.inner.read(buf)?;
        self.sent += n as u64;
        if let Some(progress) = self.progress {
            if self.sent - self.reported >= PROGRESS_STEP {
                self.reported = self.sent;
                progress(self.sent);
            }
        }
        Ok(n)
    }
}

// ----------------------------------------------------------------------------------- multipart

/// A `multipart/form-data` body that streams its file part from disk.
pub struct Multipart {
    boundary: String,
    parts: Vec<Part>,
}

enum Part {
    Text { name: String, value: String },
    File { name: String, file_name: String, content_type: String, data: UploadData },
}

impl Multipart {
    pub fn new() -> Multipart {
        Multipart { boundary: format!("----GlimpsfyBoundary{}", crate::engine::random_token(24)), parts: Vec::new() }
    }

    pub fn text(mut self, name: &str, value: &str) -> Self {
        self.parts.push(Part::Text { name: name.into(), value: value.into() });
        self
    }

    pub fn file(mut self, name: &str, file_name: &str, content_type: &str, data: &UploadData) -> Self {
        self.parts.push(Part::File {
            name: name.into(),
            file_name: file_name.into(),
            content_type: content_type.into(),
            data: data.clone(),
        });
        self
    }

    pub fn content_type(&self) -> String {
        format!("multipart/form-data; boundary={}", self.boundary)
    }

    /// The body as one reader and its exact length.
    pub fn into_reader(self) -> io::Result<(Box<dyn Read + Send>, u64)> {
        let mut readers: Vec<Box<dyn Read + Send>> = Vec::new();
        let mut length = 0u64;
        fn push_bytes(readers: &mut Vec<Box<dyn Read + Send>>, bytes: Vec<u8>) -> u64 {
            let n = bytes.len() as u64;
            readers.push(Box::new(Cursor::new(bytes)));
            n
        }
        for part in &self.parts {
            match part {
                Part::Text { name, value } => {
                    let head = format!(
                        "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
                        self.boundary,
                        escape_quoted(name),
                        value
                    );
                    length += push_bytes(&mut readers, head.into_bytes());
                }
                Part::File { name, file_name, content_type, data } => {
                    let head = format!(
                        "--{}\r\nContent-Disposition: form-data; name=\"{}\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                        self.boundary,
                        escape_quoted(name),
                        escape_quoted(file_name),
                        content_type.replace(['\r', '\n'], "")
                    );
                    length += push_bytes(&mut readers, head.into_bytes());
                    let size = data.len()?;
                    readers.push(Box::new(data.open()?.take(size)));
                    length += size;
                    length += push_bytes(&mut readers, b"\r\n".to_vec());
                }
            }
        }
        length += push_bytes(&mut readers, format!("--{}--\r\n", self.boundary).into_bytes());
        let reader = readers
            .into_iter()
            .fold(Box::new(io::empty()) as Box<dyn Read + Send>, |chain, next| Box::new(chain.chain(next)));
        Ok((reader, length))
    }
}

impl Default for Multipart {
    fn default() -> Self {
        Multipart::new()
    }
}

/// Escapes a name for a quoted header parameter the way browsers do (WHATWG HTML).
fn escape_quoted(text: &str) -> String {
    text.replace('"', "%22").replace('\r', "%0D").replace('\n', "%0A")
}
