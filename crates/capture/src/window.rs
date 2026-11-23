//! What a window capture looks like once the system has handed it over: an opaque body, square
//! corners, a border and a background fill, from Settings › Output › Window captures. Every
//! backend calls [`finish_window`] on its raw window image with what it knows of the window's
//! body ([`BodyHint`]), so the options mean the same on every platform.

use crate::backend::WindowCaptureOptions;
use crate::frame::{Frame, PixelFormat};
use crate::geometry::{PhysicalRect, PhysicalSize};

/// The deepest a rounded corner is searched for along its diagonal, in pixels (a 2× window
/// with a 26 pt radius reaches about 15 px in).
const MAX_CORNER_DEPTH: u32 = 48;

/// The share of the body that may be fully transparent before the window counts as shaped (a
/// round clock, a camera bubble). Such a body is no rounded rectangle, so it gets no fill and no
/// border.
const SHAPED_SHARE: f64 = 0.01;

/// The alpha below which the middle of a side is not the window's edge but something around it,
/// such as the client-side shadow an X11 image holds when the hint names no body.
const EDGE_FLOOR: u8 = 16;

/// The border's opacity: black over a light edge, white over a dark one.
const BORDER_ON_LIGHT: f64 = 0.16;
const BORDER_ON_DARK: f64 = 0.20;

/// What a platform knows of the window's body (the window without its shadow) in its image.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BodyHint {
    /// The body's size in pixels when the image holds more than the body (the shadow around
    /// it); `None` when the image is the body.
    pub size: Option<PhysicalSize>,
    /// The corner radius in pixels when the system names it (Windows 11); `None` reads it from
    /// the image.
    pub radius: Option<f64>,
    /// The colour of the system's windows, as `[r, g, b]` in the frame's colour space, which
    /// fills the translucent parts; `None` is white.
    pub fill: Option<[u8; 3]>,
}

/// The window's body in its image: a rectangle and the radius of each corner in pixels, in the
/// order top left, top right, bottom left, bottom right.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BodyMask {
    pub rect: PhysicalRect,
    pub radii: [f64; 4],
}

/// Applies the options a platform cannot apply itself, in this order: the body's translucent
/// parts filled, the corners filled when they are not kept, the border, and last the
/// transparent pixels composited over the background when one is set. Each step after the fill
/// sees the body as the fill left it, so a border over a translucent body takes the fill's colour
/// into account.
pub fn finish_window(frame: Frame, hint: BodyHint, options: &WindowCaptureOptions) -> Frame {
    let mut frame = frame;
    let shapes_body = options.opaque_body || options.border || !options.keep_rounded_corners;
    if let Some(mask) = shapes_body.then(|| body_mask(&frame, hint)).flatten() {
        let shaped = is_shaped(&frame, &mask);
        if options.opaque_body && !shaped {
            fill_body(&mut frame, &mask, hint.fill.unwrap_or([255, 255, 255]));
        }
        if !options.keep_rounded_corners {
            square_corners(&mut frame, &mask, options.background.unwrap_or([255, 255, 255]));
        }
        if options.border && !shaped {
            let squared = (!options.keep_rounded_corners).then(|| options.background.unwrap_or([255, 255, 255]));
            draw_border(&mut frame, &mask, squared);
        }
    }
    match options.background {
        Some(background) if !frame.is_opaque() => frame.over(background),
        _ => frame,
    }
}

/// Where the window's body lies in its image, and how round its corners are; `None` when the
/// image is empty or no corner could be read.
///
/// The body is never the bounding box of the opaque pixels, because a translucent body (the
/// windows macOS 27 hands over dark) leaves that box small or empty. With a size from the
/// platform, the body sits where the alpha jumps most steeply into it and out of it along the
/// middle row and the middle column, since the shadow around it is not even on its sides.
/// Without one, the body is the whole image.
pub fn body_mask(frame: &Frame, hint: BodyHint) -> Option<BodyMask> {
    let (w, h) = (frame.size.width, frame.size.height);
    if w == 0 || h == 0 {
        return None;
    }
    let rect = match hint.size {
        Some(size) if size.width < w || size.height < h => {
            let (body_w, body_h) = (size.width.clamp(1, w), size.height.clamp(1, h));
            let x = body_start(|i| alpha(frame, i, h / 2), w, body_w);
            let y = body_start(|i| alpha(frame, w / 2, i), h, body_h);
            PhysicalRect { x: x as i32, y: y as i32, width: body_w, height: body_h }
        }
        _ => PhysicalRect { x: 0, y: 0, width: w, height: h },
    };
    let largest = rect.width.min(rect.height) as f64 / 2.0;
    let radii = match hint.radius {
        Some(radius) => [radius.clamp(0.0, largest); 4],
        None => {
            let read = [0, 1, 2, 3].map(|corner| corner_radius(frame, rect, corner));
            // A corner that could not be read (a translucent spot under the threshold) takes
            // the roundest of the others, since a window's corners match.
            let roundest = read.iter().flatten().copied().reduce(f64::max)?;
            read.map(|radius| radius.unwrap_or(roundest).min(largest))
        }
    };
    Some(BodyMask { rect, radii })
}

impl BodyMask {
    /// How far the centre of pixel (`x`, `y`) lies inside the body's edge, in pixels; negative
    /// outside it.
    fn inside(&self, x: u32, y: u32) -> f64 {
        let (w, h) = (self.rect.width as f64, self.rect.height as f64);
        let px = x as f64 + 0.5 - self.rect.x as f64;
        let py = y as f64 + 0.5 - self.rect.y as f64;
        let (left, top) = (px < w / 2.0, py < h / 2.0);
        let radius = self.radii[usize::from(!top) * 2 + usize::from(!left)];
        let ex = if left { px } else { w - px };
        let ey = if top { py } else { h - py };
        if ex < radius && ey < radius {
            radius - (radius - ex).hypot(radius - ey)
        } else {
            ex.min(ey)
        }
    }

    /// The share of pixel (`x`, `y`) the body covers, from 0 to 1.
    fn coverage(&self, x: u32, y: u32) -> f64 {
        (self.inside(x, y) + 0.5).clamp(0.0, 1.0)
    }

    /// The pixel of `corner` and the steps that lead from it into the body.
    fn corner(&self, corner: usize) -> (u32, u32, i64, i64) {
        let r = self.rect;
        let (left, top) = (r.x as u32, r.y as u32);
        let (right, bottom) = (left + r.width - 1, top + r.height - 1);
        match corner {
            0 => (left, top, 1, 1),
            1 => (right, top, -1, 1),
            2 => (left, bottom, 1, -1),
            _ => (right, bottom, -1, -1),
        }
    }

    /// The alpha in the middle of each side: top, bottom, left, right.
    fn edge_alphas(&self, frame: &Frame) -> [u8; 4] {
        let r = self.rect;
        let (left, top) = (r.x as u32, r.y as u32);
        let (right, bottom) = (left + r.width - 1, top + r.height - 1);
        let (mid_x, mid_y) = (left + r.width / 2, top + r.height / 2);
        [alpha(frame, mid_x, top), alpha(frame, mid_x, bottom), alpha(frame, left, mid_y), alpha(frame, right, mid_y)]
    }
}

fn alpha(frame: &Frame, x: u32, y: u32) -> u8 {
    frame.data[y as usize * frame.bytes_per_row + x as usize * 4 + 3]
}

/// The first pixel of a body `body` pixels long in a line of `len` pixels whose alpha is
/// `alpha`: where the rise into the body and the fall out of it are steepest together. Past the
/// line's ends the alpha counts as 0, so a body that touches an end is found too.
fn body_start(alpha: impl Fn(u32) -> u8, len: u32, body: u32) -> u32 {
    let at = |i: i64| if i < 0 || i >= len as i64 { 0 } else { alpha(i as u32) as i32 };
    let body = body as i64;
    (0..=(len as i64 - body))
        .max_by_key(|&start| (at(start) - at(start - 1)) + (at(start + body - 1) - at(start + body)))
        .unwrap_or(0) as u32
}

/// The radius of one corner of `rect`, read along its diagonal; `None` when no pixel there
/// reaches the threshold.
///
/// The threshold is ¾ of the alpha in the middle of the corner's two sides (the lower of them),
/// so a translucent body is read like an opaque one. The first pixel at the threshold sits on
/// the arc; the coverage of the partly covered pixel next to it, with the shadow beneath taken
/// off, says where the arc crosses the diagonal to a fraction of a pixel. The mask's own
/// coverage (`BodyMask::coverage`) is solved for the radius, so the fill and the border meet the
/// window's own edge.
fn corner_radius(frame: &Frame, rect: PhysicalRect, corner: usize) -> Option<f64> {
    let mask = BodyMask { rect, radii: [0.0; 4] };
    let edges = mask.edge_alphas(frame);
    let (side_h, side_v) = (edges[corner / 2], edges[2 + corner % 2]);
    let reference = side_h.min(side_v).max(EDGE_FLOOR) as f64;
    let (cx, cy, dx, dy) = mask.corner(corner);
    let ratio = |d: u32| {
        let (x, y) = ((cx as i64 + dx * d as i64) as u32, (cy as i64 + dy * d as i64) as u32);
        (alpha(frame, x, y) as f64 / reference).min(1.0)
    };
    let limit = MAX_CORNER_DEPTH.min(rect.width / 2).min(rect.height / 2);
    let first = (0..limit).find(|&d| ratio(d) >= 0.75)?;
    if first == 0 {
        return Some(0.0);
    }
    // The shadow lifts every pixel around the arc by about as much as it lifts the one fully
    // outside it.
    let shadow = if first >= 2 { ratio(first - 2).min(0.5) } else { 0.0 };
    let covered = |d: u32| ((ratio(d) - shadow) / (1.0 - shadow)).clamp(0.0, 1.0);
    // Along the diagonal the mask's coverage climbs √2 per pixel: `coverage = r − √2·(r − d −
    // ½) + ½` at depth d, solved here for r.
    let sqrt2 = std::f64::consts::SQRT_2;
    let solve = |d: u32, cover: f64| (sqrt2 * (d as f64 + 0.5) - (cover - 0.5)) / (sqrt2 - 1.0);
    let partial = [(first - 1, covered(first - 1)), (first, covered(first))]
        .into_iter()
        .filter(|&(_, cover)| cover > 0.0 && cover < 1.0)
        .min_by(|a, b| (a.1 - 0.5).abs().total_cmp(&(b.1 - 0.5).abs()));
    Some(match partial {
        Some((d, cover)) => solve(d, cover),
        // The arc runs between two pixel centres: halfway between them.
        None => sqrt2 * first as f64 / (sqrt2 - 1.0),
    })
}

/// Whether the body is no rounded rectangle: a side whose middle is nearly clear, or more than
/// `SHAPED_SHARE` of the body fully transparent.
fn is_shaped(frame: &Frame, mask: &BodyMask) -> bool {
    if mask.edge_alphas(frame).iter().any(|&a| a < EDGE_FLOOR) {
        return true;
    }
    let r = mask.rect;
    let mut clear = 0u64;
    for y in r.y as u32..r.y as u32 + r.height {
        for x in r.x as u32..r.x as u32 + r.width {
            if alpha(frame, x, y) == 0 && mask.coverage(x, y) >= 1.0 {
                clear += 1;
            }
        }
    }
    clear as f64 > SHAPED_SHARE * r.width as f64 * r.height as f64
}

/// `[r, g, b]` in the frame's byte order.
fn in_order(format: PixelFormat, colour: [u8; 3]) -> [u8; 3] {
    match format {
        PixelFormat::Bgra8 => [colour[2], colour[1], colour[0]],
        PixelFormat::Rgba8 => colour,
    }
}

/// Lays the body over `fill` wherever it is translucent; the shadow and the pixels outside the
/// rounded corners keep their alpha.
///
/// A pixel on the arc is only partly window: its alpha is the share the window covers times the
/// window's own opacity there. The fill takes only the covered share, so the opaque edge of an
/// opaque window is left as it was, and a translucent one becomes opaque with its arc still
/// smooth.
fn fill_body(frame: &mut Frame, mask: &BodyMask, fill: [u8; 3]) {
    let fill = in_order(frame.format, fill);
    let row = frame.bytes_per_row;
    let r = mask.rect;
    for y in r.y as u32..r.y as u32 + r.height {
        for x in r.x as u32..r.x as u32 + r.width {
            let at = y as usize * row + x as usize * 4;
            let a = frame.data[at + 3];
            if a == 255 {
                continue;
            }
            let cover = mask.coverage(x, y);
            if cover <= 0.0 {
                continue;
            }
            let opacity = (a as f64 / (cover * 255.0)).min(1.0);
            let p = &mut frame.data[at..at + 4];
            for c in 0..3 {
                p[c] = (p[c] as f64 * opacity + fill[c] as f64 * (1.0 - opacity)).round() as u8;
            }
            p[3] = (cover * 255.0).max(a as f64).round() as u8;
        }
    }
}

/// Draws a line 1 pt wide just inside the body's edge, along its rounded corners, with the
/// edge of the line smoothed where it crosses pixels. It never changes the image's size: the
/// line lies over the window's outermost pixels.
///
/// With `squared` (the colour `square_corners` filled the corners with) the line is square, and
/// it takes its colour per segment: over the window it contrasts with the window's edge, and
/// over the filled corners with that fill, blended across the window's own arc. No single
/// translucent colour shows on both a dark window and white corners.
fn draw_border(frame: &mut Frame, body: &BodyMask, squared: Option<[u8; 3]>) {
    let r = body.rect;
    let width = frame.scale.0.max(1.0);
    let band = width.ceil() as u32 + 1;
    if r.width <= 2 * band || r.height <= 2 * band {
        return;
    }
    let line = BodyMask { radii: if squared.is_some() { [0.0; 4] } else { body.radii }, ..*body };
    let side = border_over(edge_is_light(frame, body, band));
    let corner = squared.map_or(side, |fill| border_over(luma(fill.map(f64::from)) >= 128.0));
    let row = frame.bytes_per_row;
    // Rows this close to the top or the bottom hold a corner, so every pixel of them is looked
    // at; the others only near the left and right sides.
    let deep = body.radii.iter().copied().fold(0.0, f64::max).ceil() as u32 + band;
    for ly in 0..r.height {
        let y = r.y as u32 + ly;
        let whole = ly < deep || ly + deep >= r.height;
        let (near, far) = if whole { (0..r.width, 0..0) } else { (0..band, r.width - band..r.width) };
        for lx in near.chain(far) {
            let x = r.x as u32 + lx;
            let inside = line.inside(x, y);
            let ring = (inside + 0.5).clamp(0.0, 1.0) - (inside - width + 0.5).clamp(0.0, 1.0);
            if ring <= 0.0 {
                continue;
            }
            // The window's share of the pixel: 1 on the body, 0 on a squared corner's fill.
            let window = if squared.is_some() { body.coverage(x, y) } else { 1.0 };
            let colour: [f64; 3] = std::array::from_fn(|c| corner.0[c] + (side.0[c] - corner.0[c]) * window);
            let beta = ring * (corner.1 + (side.1 - corner.1) * window);
            let at = y as usize * row + x as usize * 4;
            let p = &mut frame.data[at..at + 4];
            let a = p[3] as f64 / 255.0;
            let out = beta + a * (1.0 - beta);
            for c in 0..3 {
                p[c] = ((colour[c] * beta + p[c] as f64 * a * (1.0 - beta)) / out).round() as u8;
            }
            p[3] = (out * 255.0).round() as u8;
        }
    }
}

/// The border's colour and opacity over a light or a dark surface.
fn border_over(light: bool) -> ([f64; 3], f64) {
    if light {
        ([0.0; 3], BORDER_ON_LIGHT)
    } else {
        ([255.0; 3], BORDER_ON_DARK)
    }
}

/// The luma of `[r, g, b]`, 0 to 255.
fn luma(rgb: [f64; 3]) -> f64 {
    0.299 * rgb[0] + 0.587 * rgb[1] + 0.114 * rgb[2]
}

/// Whether the body is light just inside its straight sides, where the border goes: the mean
/// luma of the pixels there, each counted by its alpha, against the middle grey. The sides are
/// taken between the arcs, so squared corners never count.
fn edge_is_light(frame: &Frame, mask: &BodyMask, band: u32) -> bool {
    let r = mask.rect;
    let (left, top) = (r.x as u32, r.y as u32);
    let (right, bottom) = (left + r.width - 1 - band, top + r.height - 1 - band);
    let (red, blue) = match frame.format {
        PixelFormat::Bgra8 => (2, 0),
        PixelFormat::Rgba8 => (0, 2),
    };
    let (mut sum, mut weight) = (0.0, 0.0);
    let mut sample = |x: u32, y: u32| {
        let at = y as usize * frame.bytes_per_row + x as usize * 4;
        let p = &frame.data[at..at + 4];
        let a = p[3] as f64;
        sum += a * luma([p[red], p[1], p[blue]].map(f64::from));
        weight += a;
    };
    let corner = mask.radii.iter().copied().fold(0.0, f64::max).ceil() as u32 + band;
    for x in (left + corner..(left + r.width).saturating_sub(corner)).step_by(4) {
        sample(x, top + band);
        sample(x, bottom);
    }
    for y in (top + corner..(top + r.height).saturating_sub(corner)).step_by(4) {
        sample(left + band, y);
        sample(right, y);
    }
    weight == 0.0 || sum / weight >= 128.0
}

/// Fills the corners of the body outside its arcs with `fill`, so the window gets square
/// corners while its shadow (outside the body) stays as it is. On the arc the window's own edge
/// is laid over the fill; the body's own translucency inside the arc is left to `fill_body`.
fn square_corners(frame: &mut Frame, mask: &BodyMask, fill: [u8; 3]) {
    let fill = in_order(frame.format, fill);
    let row = frame.bytes_per_row;
    let r = mask.rect;
    for (corner, &radius) in mask.radii.iter().enumerate() {
        if radius <= 0.0 {
            continue;
        }
        let side = (radius.ceil() as u32 + 1).min(r.width / 2).min(r.height / 2);
        let (cx, cy, dx, dy) = mask.corner(corner);
        for j in 0..side {
            for i in 0..side {
                let x = (cx as i64 + dx * i as i64) as u32;
                let y = (cy as i64 + dy * j as i64) as u32;
                let at = y as usize * row + x as usize * 4;
                let a = frame.data[at + 3] as u32;
                if a == 255 {
                    continue;
                }
                let cover = mask.coverage(x, y);
                if cover >= 1.0 {
                    continue;
                }
                // Outside the arc only the shadow lies, which a square corner covers.
                let p = &mut frame.data[at..at + 4];
                for c in 0..3 {
                    p[c] = if cover <= 0.0 {
                        fill[c]
                    } else {
                        ((p[c] as u32 * a + fill[c] as u32 * (255 - a) + 127) / 255) as u8
                    };
                }
                p[3] = 255;
            }
        }
    }
}
