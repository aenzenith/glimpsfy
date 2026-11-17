//! What a window capture looks like once the system has handed it over: square corners and a
//! background fill, from Settings › Output › Window captures. Every backend calls
//! [`finish_window`] on its raw window image, so the options mean the same on every platform.

use crate::backend::WindowCaptureOptions;
use crate::frame::{Frame, PixelFormat};

/// The deepest a rounded corner is searched for along its diagonal, in pixels (a 2× window
/// with a 26 pt radius reaches about 15 px in).
const MAX_CORNER_DEPTH: u32 = 48;

/// Applies the options a platform cannot apply itself: corners filled when they are not kept,
/// and the transparent pixels composited over the background when one is set.
pub fn finish_window(frame: Frame, options: &WindowCaptureOptions) -> Frame {
    let mut frame = frame;
    if !options.keep_rounded_corners {
        square_corners(&mut frame, options.background.unwrap_or([255, 255, 255]));
    }
    match options.background {
        Some(background) if !frame.is_opaque() => frame.over(background),
        _ => frame,
    }
}

/// Fills the transparent corners of the window's opaque body with `fill`, so the window gets
/// square corners while its shadow (outside the body) stays as it is.
///
/// The body is the bounding box of the fully opaque pixels. In each corner the radius is read
/// from the image: walking the diagonal inwards, the first opaque pixel sits at `r · (1 − 1/√2)`
/// from the corner, and the square of side `r` there is composited over `fill`.
fn square_corners(frame: &mut Frame, fill: [u8; 3]) {
    let (w, h) = (frame.size.width, frame.size.height);
    let row = frame.bytes_per_row;
    let alpha = |data: &[u8], x: u32, y: u32| data[y as usize * row + x as usize * 4 + 3];

    let (mut left, mut top, mut right, mut bottom) = (u32::MAX, u32::MAX, 0u32, 0u32);
    for y in 0..h {
        for x in 0..w {
            if alpha(&frame.data, x, y) == 255 {
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
            }
        }
    }
    if left > right || top > bottom {
        return;
    }
    let (body_w, body_h) = (right - left + 1, bottom - top + 1);

    let fill = match frame.format {
        PixelFormat::Bgra8 => [fill[2], fill[1], fill[0]],
        PixelFormat::Rgba8 => fill,
    };
    // (corner x, corner y, step x, step y)
    let corners = [(left, top, 1i64, 1i64), (right, top, -1, 1), (left, bottom, 1, -1), (right, bottom, -1, -1)];
    for (cx, cy, dx, dy) in corners {
        let at = |d: u32| ((cx as i64 + dx * d as i64) as u32, (cy as i64 + dy * d as i64) as u32);
        let limit = MAX_CORNER_DEPTH.min(body_w / 2).min(body_h / 2);
        let Some(depth) = (0..limit).find(|&d| {
            let (x, y) = at(d);
            alpha(&frame.data, x, y) == 255
        }) else {
            continue;
        };
        if depth == 0 {
            continue;
        }
        let radius = depth as f64 / (1.0 - std::f64::consts::FRAC_1_SQRT_2);
        let side = (radius.ceil() as u32 + 1).min(body_w / 2).min(body_h / 2);
        for j in 0..side {
            for i in 0..side {
                let x = (cx as i64 + dx * i as i64) as usize;
                let y = (cy as i64 + dy * j as i64) as usize;
                let p = &mut frame.data[y * row + x * 4..y * row + x * 4 + 4];
                let a = p[3] as u32;
                if a == 255 {
                    continue;
                }
                // Outside the arc only the shadow lies, which a square corner covers; on the
                // arc the window's own edge is blended over the fill.
                let (u, v) = (radius - (i as f64 + 0.5), radius - (j as f64 + 0.5));
                let outside = u > 0.0 && v > 0.0 && (u * u + v * v).sqrt() > radius + 0.5;
                for c in 0..3 {
                    p[c] = if outside {
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
