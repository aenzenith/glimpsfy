//! A tiny HTTP/1.1 server on 127.0.0.1 for the tests: it records every request it receives and
//! answers with whatever the test's handler returns. No test talks to a real host.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use url::Url;

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    /// The path with its query string.
    pub target: String,
    /// Header names in lower case.
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    /// Whether the whole body arrived.
    pub complete: bool,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
    }
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn text(status: u16, body: &str) -> Reply {
        Reply { status, headers: vec![("content-type".into(), "text/plain".into())], body: body.as_bytes().to_vec() }
    }

    pub fn json(status: u16, body: serde_json::Value) -> Reply {
        Reply {
            status,
            headers: vec![("content-type".into(), "application/json".into())],
            body: body.to_string().into_bytes(),
        }
    }

    pub fn header(mut self, name: &str, value: &str) -> Reply {
        self.headers.push((name.into(), value.into()));
        self
    }
}

type Handler = dyn Fn(&Recorded) -> Reply + Send + Sync;

pub struct TestServer {
    pub url: Url,
    requests: Arc<Mutex<Vec<Recorded>>>,
}

impl TestServer {
    pub fn start(handler: impl Fn(&Recorded) -> Reply + Send + Sync + 'static) -> TestServer {
        let listener = TcpListener::bind("127.0.0.1:0").expect("a free port on 127.0.0.1");
        let port = listener.local_addr().expect("the listener's address").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler: Arc<Handler> = Arc::new(handler);
        let seen = requests.clone();
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let seen = seen.clone();
                let handler = handler.clone();
                thread::spawn(move || serve(stream, &seen, handler.as_ref()));
            }
        });
        TestServer { url: Url::parse(&format!("http://127.0.0.1:{port}/")).expect("a valid URL"), requests }
    }

    /// A server that answers 200 to everything, to prove that nobody called it.
    pub fn canary() -> TestServer {
        TestServer::start(|_| Reply::text(200, "canary"))
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("the request log").clone()
    }

    pub fn origin(&self) -> String {
        self.url.as_str().trim_end_matches('/').to_string()
    }
}

fn serve(stream: TcpStream, seen: &Mutex<Vec<Recorded>>, handler: &Handler) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut writer = match stream.try_clone() {
        Ok(writer) => writer,
        Err(_) => return,
    };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    if reader.read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let target = parts.next().unwrap_or("").to_string();
    let mut headers = Vec::new();
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            break;
        }
        let header = header.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    let length: usize =
        headers.iter().find(|(n, _)| n == "content-length").and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    let mut body = vec![0u8; length];
    let mut read = 0;
    while read < length {
        match reader.read(&mut body[read..]) {
            Ok(0) | Err(_) => break,
            Ok(n) => read += n,
        }
    }
    body.truncate(read);
    let recorded = Recorded { method, target, headers, body, complete: read == length };
    seen.lock().expect("the request log").push(recorded.clone());
    if !recorded.complete {
        return;
    }
    let reply = handler(&recorded);
    let mut head =
        format!("HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n", reply.status, reply.body.len());
    for (name, value) in &reply.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let _ = writer.write_all(head.as_bytes());
    let _ = writer.write_all(&reply.body);
    let _ = writer.flush();
}
