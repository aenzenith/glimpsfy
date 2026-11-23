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
//!
//! The scheme also serves two stills outside a capture: the backdrop the recording target shows
//! under its setup where windows cannot be transparent (`glimpsfy-frame://backdrop/<id>?t=<token>`,
//! a BMP for an `<img>`; `recording::take_backdrop`), and the preview of a beautify background
//! image (`glimpsfy-frame://background/<longest side>?t=<percent-encoded path>`, a PNG). A
//! background is served only when a preset of Settings › Beautify, the open capture's style, an
//! editor document or the user's own pick in a file panel names it, so a page cannot read any
//! other file through the scheme; a missing one answers 404, which the panel shows as missing.
//!
//! A scrolling capture's strip loads its live preview from it as well
//! (`glimpsfy-frame://scroll/<display>?t=<token>&v=<n>`, a BMP; `scroll.rs`).

use std::borrow::Cow;
use std::sync::{Arc, Mutex};
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
    url(Kind::Display, display, token)
}

/// What a frame address names: a display's frame of the open capture, or the backdrop.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Display,
    Backdrop,
    Background,
    Scroll,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Display => "display",
            Kind::Backdrop => "backdrop",
            Kind::Background => "background",
            Kind::Scroll => "scroll",
        }
    }
}

fn url(kind: Kind, display: DisplayId, token: &str) -> String {
    let kind = kind.name();
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost/{kind}/{}?t={token}", display.0)
    } else {
        format!("{SCHEME}://{kind}/{}?t={token}", display.0)
    }
}

/// The kind, display and token of a frame address, in either form of `url`.
fn parse(uri: &tauri::http::Uri) -> Option<(Kind, DisplayId, String)> {
    let path = uri.path().trim_matches('/');
    let (kind, id) = match uri.host() {
        Some("display") => (Kind::Display, path),
        Some("backdrop") => (Kind::Backdrop, path),
        Some("background") => (Kind::Background, path),
        Some("scroll") => (Kind::Scroll, path),
        _ => match path.split_once('/')? {
            ("display", id) => (Kind::Display, id),
            ("backdrop", id) => (Kind::Backdrop, id),
            ("background", id) => (Kind::Background, id),
            ("scroll", id) => (Kind::Scroll, id),
            _ => return None,
        },
    };
    let id = id.parse().ok()?;
    let token = uri.query()?.split('&').find_map(|pair| pair.strip_prefix("t="))?;
    Some((kind, DisplayId(id), token.to_string()))
}

/// The one backdrop: its display, its token and the still.
static BACKDROP: Mutex<Option<(DisplayId, String, Arc<Frame>)>> = Mutex::new(None);

fn backdrop() -> std::sync::MutexGuard<'static, Option<(DisplayId, String, Arc<Frame>)>> {
    BACKDROP.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Keeps `frame` as the backdrop of `display`, in place of any earlier one, and gives the address
/// the target page loads it from.
pub fn set_backdrop(display: DisplayId, frame: Arc<Frame>) -> String {
    let token = crate::session::token();
    let address = url(Kind::Backdrop, display, &token);
    *backdrop() = Some((display, token, frame));
    address
}

/// The address of a scrolling capture's preview: the token belongs to the scroll, and `version`
/// changes with every new preview, so the page's `<img>` loads the new one.
pub fn scroll_url(display: DisplayId, token: &str, version: u64) -> String {
    format!("{}&v={version}", url(Kind::Scroll, display, token))
}

/// Lets the backdrop go when the setup closes; its address answers 404 from then on.
pub fn clear_backdrop() {
    backdrop().take();
}

/// The files the user picked in a file panel (`app_choose_file`), which a beautify panel may show
/// before any style names them.
static CHOSEN: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());

/// Lets the preview of `path` be served: the user picked it.
pub fn allow_background(path: &std::path::Path) {
    let mut chosen = CHOSEN.lock().unwrap_or_else(|p| p.into_inner());
    if !chosen.iter().any(|p| p == path) {
        chosen.push(path.to_path_buf());
    }
}

/// Whether a style, a document or a pick names `path` as a background image.
fn background_allowed(app: &tauri::AppHandle, path: &std::path::Path) -> bool {
    use glimpsfy_annotate::Background;
    let named = |style: &glimpsfy_annotate::BeautifyStyle| matches!(&style.background, Background::Image { path: p, .. } if std::path::Path::new(p) == path);
    let state = app.state::<AppState>();
    let settings = state.settings.get();
    settings.beautify.offered().any(|preset| named(&preset.style))
        || state.sessions.current().and_then(|s| s.beautify()).is_some_and(|style| named(&style))
        || crate::commands::render::editor_backgrounds().iter().any(|p| std::path::Path::new(p) == path)
        || CHOSEN.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|p| p == path)
}

/// The preview of a background image, at most `side` pixels on its longer side, as an sRGB PNG
/// the page's canvas converts into its own colour space.
fn background_preview(app: &tauri::AppHandle, encoded: &str, side: u32) -> Result<Vec<u8>, StatusCode> {
    let path = crate::commands::render::percent_decode(encoded).ok_or(StatusCode::BAD_REQUEST)?;
    let path = std::path::Path::new(&path);
    if !background_allowed(app, path) {
        return Err(StatusCode::FORBIDDEN);
    }
    let image =
        crate::output::beautify::load_image(path, glimpsfy_capture::ColorSpace::Srgb).ok_or(StatusCode::NOT_FOUND)?;
    let side = side.clamp(64, 2048);
    let longest = image.size.width.max(image.size.height);
    let image = if longest > side {
        let k = side as f64 / longest as f64;
        let scaled = |v: u32| ((v as f64 * k).round() as u32).max(1);
        image.resized(glimpsfy_capture::PhysicalSize::new(scaled(image.size.width), scaled(image.size.height)))
    } else {
        image
    };
    image.encode_png(PngEffort::Fastest).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
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
    let Some((kind, display, token)) = parse(request.uri()) else {
        return reply(StatusCode::BAD_REQUEST);
    };
    if kind == Kind::Background {
        // The number of the address is the longest side the preview needs.
        return match background_preview(app, &token, display.0) {
            Ok(png) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, FrameFormat::Png.content_type())
                .header(header::CACHE_CONTROL, "no-store")
                .header(header::ACCESS_CONTROL_ALLOW_ORIGIN, "*")
                .body(Cow::Owned(png))
                .unwrap_or_else(|_| reply(StatusCode::INTERNAL_SERVER_ERROR)),
            Err(status) => reply(status),
        };
    }
    if kind == Kind::Scroll {
        let Some(bmp) = crate::scroll::preview_bmp(display, &token) else {
            return reply(StatusCode::NOT_FOUND);
        };
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, FrameFormat::Bmp.content_type())
            .header(header::CACHE_CONTROL, "no-store")
            .body(Cow::Owned(bmp))
            .unwrap_or_else(|_| reply(StatusCode::INTERNAL_SERVER_ERROR));
    }
    if kind == Kind::Backdrop {
        let still = backdrop().as_ref().filter(|(id, kept, _)| *id == display && *kept == token).map(|b| b.2.clone());
        let Some(still) = still else {
            return reply(StatusCode::NOT_FOUND);
        };
        // An `<img>` decodes a BMP without the page's help, and the pixels need no compression
        // on their way across the process.
        return Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, FrameFormat::Bmp.content_type())
            .header(header::CACHE_CONTROL, "no-store")
            .body(Cow::Owned(still.to_bmp()))
            .unwrap_or_else(|_| reply(StatusCode::INTERNAL_SERVER_ERROR));
    }
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
