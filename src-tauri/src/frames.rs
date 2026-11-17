//! The `glimpsfy-frame` URI scheme, which serves each overlay the frozen frame of its display.
//!
//! A frame of a 5K display is 59 MB of pixels, far too much for the IPC bridge, so the overlay
//! fetches it from this scheme instead: `glimpsfy-frame://display/<id>?t=<token>` on macOS and
//! Linux, `http://glimpsfy-frame.localhost/display/<id>?t=<token>` on Windows. The token belongs
//! to the capture, so a page never receives a stale frame of an earlier one.
//!
//! The format is chosen by measurement (`docs/ARCHITECTURE.md`, "Frame transport"):
//!
//! - `rgba`: the raw pixels as RGBA rows. The page wraps them in an `ImageData` tagged with the
//!   frame's colour space and puts them on a canvas with no decoding at all.
//! - `bmp`: a top-down 32-bit BMP of the BGRA pixels, decoded by the browser's image decoder.
//! - `png`: a PNG at the fastest compression, which carries the ICC profile.
//!
//! Each request encodes on its own thread, so the displays of a multi-display capture are served
//! in parallel and the main thread never waits for a frame.

use std::borrow::Cow;
use std::time::Instant;

use glimpsfy_capture::{DisplayId, Frame, PngEffort};
use serde::Serialize;
use tauri::http::{header, Request, Response, StatusCode};
use tauri::{Manager, UriSchemeContext, UriSchemeResponder};

use crate::trace::{self, Mark};
use crate::AppState;

pub const SCHEME: &str = "glimpsfy-frame";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum FrameFormat {
    Rgba,
    Bmp,
    Png,
}

impl FrameFormat {
    /// The format the measurements chose; `GLIMPSFY_FRAME_FORMAT` overrides it in dev builds.
    pub const DEFAULT: FrameFormat = FrameFormat::Rgba;

    pub fn current() -> FrameFormat {
        crate::dev::options().frame_format.unwrap_or(FrameFormat::DEFAULT)
    }

    pub fn parse(value: &str) -> Option<FrameFormat> {
        match value.to_ascii_lowercase().as_str() {
            "rgba" | "raw" => Some(FrameFormat::Rgba),
            "bmp" => Some(FrameFormat::Bmp),
            "png" => Some(FrameFormat::Png),
            _ => None,
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            FrameFormat::Rgba => "application/octet-stream",
            FrameFormat::Bmp => "image/bmp",
            FrameFormat::Png => "image/png",
        }
    }

    fn encode(self, frame: &Frame) -> Result<Vec<u8>, String> {
        match self {
            FrameFormat::Rgba => Ok(frame.to_rgba()),
            FrameFormat::Bmp => Ok(frame.to_bmp()),
            FrameFormat::Png => frame.encode_png(PngEffort::Fastest).map_err(|e| e.to_string()),
        }
    }
}

/// The address an overlay fetches its frame from.
pub fn frame_url(display: DisplayId, token: &str) -> String {
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost/display/{}?t={token}", display.0)
    } else {
        format!("{SCHEME}://display/{}?t={token}", display.0)
    }
}

/// The display and token of a frame address, in either form of `frame_url`.
fn parse(uri: &tauri::http::Uri) -> Option<(DisplayId, String)> {
    let path = uri.path().trim_matches('/');
    let id = if uri.host() == Some("display") { path } else { path.strip_prefix("display/")? };
    let id = id.parse().ok()?;
    let token = uri.query()?.split('&').find_map(|pair| pair.strip_prefix("t="))?;
    Some((DisplayId(id), token.to_string()))
}

/// The scheme's handler (`register_asynchronous_uri_scheme_protocol`).
pub fn handle(context: UriSchemeContext<'_, tauri::Wry>, request: Request<Vec<u8>>, responder: UriSchemeResponder) {
    let app = context.app_handle().clone();
    std::thread::spawn(move || responder.respond(serve(&app, &request)));
}

fn serve(app: &tauri::AppHandle, request: &Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let reply = |status: StatusCode| {
        Response::builder()
            .status(status)
            .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
            .body(Cow::Borrowed(&[][..]))
            .unwrap_or_default()
    };
    let Some((display, token)) = parse(request.uri()) else {
        return reply(StatusCode::BAD_REQUEST);
    };
    let Some(session) = app.state::<AppState>().sessions.current() else {
        return reply(StatusCode::NOT_FOUND);
    };
    let Some(entry) = session.displays.iter().find(|d| d.display.id == display && d.token == token) else {
        return reply(StatusCode::NOT_FOUND);
    };

    let started = Instant::now();
    let bytes = match session.format.encode(&entry.frame) {
        Ok(bytes) => bytes,
        Err(error) => {
            trace::log(format!("[glimpsfy] frame of display {} failed to encode: {error}", display.0));
            return reply(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    session.trace.note(format!(
        "display {} {:?} {:.1} MB encoded in {:.1} ms",
        display.0,
        session.format,
        bytes.len() as f64 / 1_000_000.0,
        trace::ms(started.elapsed())
    ));
    session.trace.mark(Mark::Served);

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, session.format.content_type())
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
        .body(Cow::Owned(bytes))
        .unwrap_or_else(|_| reply(StatusCode::INTERNAL_SERVER_ERROR))
}
