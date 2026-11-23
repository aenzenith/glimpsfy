//! Text detection with a DB (differentiable binarisation) model: the image becomes the model's
//! input, and the probability map it returns becomes text boxes in pixels of the image.
//!
//! The steps follow PaddleOCR's `DetResizeForTest`, `NormalizeImage` and `DBPostProcess`, with
//! two differences that suit captures of screens. The image is padded to a multiple of 32 with
//! its own edge colour instead of being stretched to one, so glyphs keep their shape. And a box
//! is the upright bounding box of its region unless the region is clearly a turned line, because
//! screen text is upright and the layout step wants tight, upright boxes; PaddleOCR's own
//! minimum-area rectangle tilts by a fraction of a degree on ordinary lines.

use super::pixels::Pixels;
use crate::models::Detector;
use crate::Bounds;

/// The model's stride: its input sides are multiples of this.
const STRIDE: usize = 32;
/// An image whose shorter side is below this is enlarged for detection, as PaddleOCR does.
const MIN_SIDE: f32 = 64.0;
/// The most pixels the detector is given. A 2560 × 1440 capture passes unscaled; larger ones are
/// reduced for detection only, and recognition still cuts its lines from the full image.
pub const MAX_PIXELS: f32 = 4_200_000.0;
/// PaddleOCR's `min_size`: a region whose shorter side is below this is noise.
const MIN_REGION: f32 = 3.0;
/// The most regions that become boxes, largest first (`max_candidates`).
const MAX_BOXES: usize = 3000;
/// The channel means and deviations of `NormalizeImage`, applied in the image's channel order
/// (blue, green, red) exactly as PaddleOCR applies them.
const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
const DEVIATION: [f32; 3] = [0.229, 0.224, 0.225];

/// The shape of the detector's input and how it maps back to the image.
pub struct Input {
    pub shape: [usize; 4],
    /// Input pixels per image pixel.
    scale: f32,
    /// The part of the input that holds the image; the rest is padding.
    valid: (usize, usize),
}

/// A text line's box: a rectangle around `(cx, cy)` whose reading axis points along
/// `(cos, sin)`, in pixels of the image. An upright box has the axis `(1, 0)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextBox {
    pub cx: f32,
    pub cy: f32,
    /// The extent along the reading axis.
    pub width: f32,
    /// The extent across it.
    pub height: f32,
    pub cos: f32,
    pub sin: f32,
}

impl TextBox {
    pub fn upright(&self) -> bool {
        self.sin == 0.0
    }

    /// The image position of a point `along` the reading axis from the box's left edge and
    /// `across` it from the top edge.
    #[inline]
    pub fn point(&self, along: f32, across: f32) -> (f32, f32) {
        let (a, b) = (along - self.width / 2.0, across - self.height / 2.0);
        (self.cx + a * self.cos - b * self.sin, self.cy + a * self.sin + b * self.cos)
    }

    /// The upright box that encloses the part of this box between two positions along its
    /// reading axis, kept inside an image of the given size.
    pub fn enclosing(&self, from: f32, to: f32, image: (usize, usize)) -> Bounds {
        let corners =
            [self.point(from, 0.0), self.point(to, 0.0), self.point(to, self.height), self.point(from, self.height)];
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for (x, y) in corners {
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        }
        let (w, h) = (image.0 as f32, image.1 as f32);
        let (x0, y0, x1, y1) = (x0.clamp(0.0, w), y0.clamp(0.0, h), x1.clamp(0.0, w), y1.clamp(0.0, h));
        Bounds::new(x0 as f64, y0 as f64, (x1 - x0).max(0.0) as f64, (y1 - y0).max(0.0) as f64)
    }
}

/// Builds the detector's input: blue, green and red planes, normalised, at a scale that keeps
/// the image inside the pixel budget, padded to the model's stride.
pub fn input(pixels: &Pixels<'_>) -> (Input, Vec<f32>) {
    let (w, h) = (pixels.width as f32, pixels.height as f32);
    let mut scale = 1.0f32;
    if w.min(h) < MIN_SIDE {
        scale = MIN_SIDE / w.min(h);
    }
    if w * h * scale * scale > MAX_PIXELS {
        scale = (MAX_PIXELS / (w * h)).sqrt();
    }
    let valid = (((w * scale).round() as usize).max(1), ((h * scale).round() as usize).max(1));
    let (width, height) = (valid.0.next_multiple_of(STRIDE), valid.1.next_multiple_of(STRIDE));
    let plane = width * height;
    let mut data = vec![0f32; plane * 3];
    let (blue, rest) = data.split_at_mut(plane);
    let (green, red) = rest.split_at_mut(plane);
    let normal = |value: f32, c: usize| (value / 255.0 - MEAN[c]) / DEVIATION[c];
    // When the image is reduced, every input pixel averages four samples of its footprint, so
    // thin strokes do not fall between the samples.
    let step = 1.0 / scale;
    let taps: &[f32] = if scale < 0.8 { &[0.25, 0.75] } else { &[0.5] };
    for y in 0..height {
        let row = y * width;
        // Padding repeats the image's last row and column: `min` here, the sampler's clamp there.
        let sy = y.min(valid.1 - 1);
        for x in 0..width {
            let sx = x.min(valid.0 - 1);
            let bgr = if scale == 1.0 {
                pixels.bgr(sx.min(pixels.width - 1), sy.min(pixels.height - 1))
            } else {
                let mut sum = [0f32; 3];
                for ty in taps {
                    for tx in taps {
                        let p = pixels.sample((sx as f32 + tx) * step, (sy as f32 + ty) * step);
                        sum = [sum[0] + p[0], sum[1] + p[1], sum[2] + p[2]];
                    }
                }
                let n = (taps.len() * taps.len()) as f32;
                [sum[0] / n, sum[1] / n, sum[2] / n]
            };
            blue[row + x] = normal(bgr[0], 0);
            green[row + x] = normal(bgr[1], 1);
            red[row + x] = normal(bgr[2], 2);
        }
    }
    (Input { shape: [1, 3, height, width], scale, valid }, data)
}

/// A horizontal run of text pixels in the probability map; `x1` is its last pixel.
struct Run {
    y: u32,
    x0: u32,
    x1: u32,
}

/// What is known about one connected region of text pixels.
#[derive(Clone)]
struct Region {
    runs: Vec<u32>,
    x0: u32,
    x1: u32,
    y0: u32,
    y1: u32,
}

/// Turns the model's probability map into text boxes in pixels of the image. `shape` and `map`
/// are the model's output; a map that does not fit the input yields no boxes.
pub fn boxes(input: &Input, image: (usize, usize), shape: &[usize], map: &[f32], detector: &Detector) -> Vec<TextBox> {
    let (map_h, map_w) = match shape {
        [.., h, w] if *h == input.shape[2] && *w == input.shape[3] && map.len() >= h * w => (*h, *w),
        _ => return Vec::new(),
    };
    let (valid_w, valid_h) = (input.valid.0.min(map_w), input.valid.1.min(map_h));
    let (runs, regions) = regions(map, map_w, valid_w, valid_h, detector.threshold);

    let mut out = Vec::new();
    for region in regions.iter().take(MAX_BOXES) {
        // Sizes are distances between pixel centres, as in OpenCV's contours, so the thresholds
        // below mean what they mean in PaddleOCR.
        let (bw, bh) = ((region.x1 - region.x0) as f32, (region.y1 - region.y0) as f32);
        let turned = turned_rectangle(&runs, region)
            // Only a clear line that a turned rectangle fits much better counts as turned text.
            .filter(|r| r.width.max(r.height) >= 3.0 * r.width.min(r.height) && bw * bh > 1.3 * r.width * r.height)
            // A steep line is read along its long side, downwards; the recogniser also tries
            // the other direction.
            .map(|r| {
                if r.height > r.width {
                    Rectangle { width: r.height, height: r.width, cos: -r.sin, sin: r.cos, ..r }
                } else {
                    r
                }
            });
        let (width, height) = turned.as_ref().map_or((bw, bh), |r| (r.width, r.height));
        if width.min(height) < MIN_REGION {
            continue;
        }
        let score = match &turned {
            Some(r) => score_turned(map, map_w, region, r),
            None => score_upright(map, map_w, region),
        };
        if score < detector.box_threshold {
            continue;
        }
        // The model marks a shrunken core of each line; growing it by `area × ratio / perimeter`
        // on every side gives the line's box back. For a rectangle this is what PaddleOCR's
        // polygon offset followed by its minimum-area rectangle amounts to.
        let grow = width * height * detector.unclip_ratio / (2.0 * (width + height));
        if width.min(height) + 2.0 * grow < MIN_REGION + 2.0 {
            continue;
        }
        let s = input.scale;
        out.push(match turned {
            Some(r) => TextBox {
                cx: (r.cx + 0.5) / s,
                cy: (r.cy + 0.5) / s,
                width: (r.width + 1.0 + 2.0 * grow) / s,
                height: (r.height + 1.0 + 2.0 * grow) / s,
                cos: r.cos,
                sin: r.sin,
            },
            None => {
                let x0 = ((region.x0 as f32 - grow) / s).max(0.0);
                let y0 = ((region.y0 as f32 - grow) / s).max(0.0);
                let x1 = ((region.x1 as f32 + 1.0 + grow) / s).min(image.0 as f32);
                let y1 = ((region.y1 as f32 + 1.0 + grow) / s).min(image.1 as f32);
                TextBox {
                    cx: (x0 + x1) / 2.0,
                    cy: (y0 + y1) / 2.0,
                    width: x1 - x0,
                    height: y1 - y0,
                    cos: 1.0,
                    sin: 0.0,
                }
            }
        });
    }
    out
}

/// Finds the connected regions (pixels touching by a side or a corner) of the map's pixels
/// above the threshold, largest first.
fn regions(map: &[f32], map_w: usize, valid_w: usize, valid_h: usize, threshold: f32) -> (Vec<Run>, Vec<Region>) {
    let mut runs: Vec<Run> = Vec::new();
    let mut parent: Vec<u32> = Vec::new();
    fn root(parent: &mut [u32], mut i: u32) -> u32 {
        while parent[i as usize] != i {
            parent[i as usize] = parent[parent[i as usize] as usize];
            i = parent[i as usize];
        }
        i
    }
    let mut previous = 0..0;
    for y in 0..valid_h {
        let row = &map[y * map_w..y * map_w + valid_w];
        let start = runs.len();
        let mut x = 0;
        while x < valid_w {
            if row[x] > threshold {
                let x0 = x;
                while x < valid_w && row[x] > threshold {
                    x += 1;
                }
                parent.push(runs.len() as u32);
                runs.push(Run { y: y as u32, x0: x0 as u32, x1: (x - 1) as u32 });
            } else {
                x += 1;
            }
        }
        // Both rows are sorted by x, so one pass joins every run with the runs it touches above.
        let (mut above, mut here) = (previous.start, start);
        while above < previous.end && here < runs.len() {
            let (a, b) = (&runs[above], &runs[here]);
            if a.x1 + 1 >= b.x0 && b.x1 + 1 >= a.x0 {
                let (ra, rb) = (root(&mut parent, above as u32), root(&mut parent, here as u32));
                parent[ra.max(rb) as usize] = ra.min(rb);
            }
            if a.x1 < b.x1 {
                above += 1;
            } else {
                here += 1;
            }
        }
        previous = start..runs.len();
    }

    let mut index = vec![u32::MAX; runs.len()];
    let mut regions: Vec<Region> = Vec::new();
    for (i, run) in runs.iter().enumerate() {
        let r = root(&mut parent, i as u32) as usize;
        if index[r] == u32::MAX {
            index[r] = regions.len() as u32;
            regions.push(Region { runs: Vec::new(), x0: run.x0, x1: run.x1, y0: run.y, y1: run.y });
        }
        let region = &mut regions[index[r] as usize];
        region.runs.push(i as u32);
        (region.x0, region.x1) = (region.x0.min(run.x0), region.x1.max(run.x1));
        (region.y0, region.y1) = (region.y0.min(run.y), region.y1.max(run.y));
    }
    regions.sort_by_key(|r| std::cmp::Reverse((r.x1 - r.x0 + 1) as u64 * (r.y1 - r.y0 + 1) as u64));
    (runs, regions)
}

/// The mean probability inside a region's upright bounding box (PaddleOCR's fast box score).
fn score_upright(map: &[f32], map_w: usize, region: &Region) -> f32 {
    let mut sum = 0.0f64;
    for y in region.y0..=region.y1 {
        let at = y as usize * map_w;
        sum += map[at + region.x0 as usize..=at + region.x1 as usize].iter().map(|v| *v as f64).sum::<f64>();
    }
    (sum / ((region.x1 - region.x0 + 1) as f64 * (region.y1 - region.y0 + 1) as f64)) as f32
}

/// A rectangle in map pixels, measured between pixel centres.
struct Rectangle {
    cx: f32,
    cy: f32,
    width: f32,
    height: f32,
    cos: f32,
    sin: f32,
}

/// The mean probability inside a turned rectangle.
fn score_turned(map: &[f32], map_w: usize, region: &Region, r: &Rectangle) -> f32 {
    let (mut sum, mut n) = (0.0f64, 0u64);
    for y in region.y0..=region.y1 {
        for x in region.x0..=region.x1 {
            let (dx, dy) = (x as f32 - r.cx, y as f32 - r.cy);
            let (along, across) = (dx * r.cos + dy * r.sin, dy * r.cos - dx * r.sin);
            if along.abs() <= r.width / 2.0 && across.abs() <= r.height / 2.0 {
                sum += map[y as usize * map_w + x as usize] as f64;
                n += 1;
            }
        }
    }
    if n == 0 {
        0.0
    } else {
        (sum / n as f64) as f32
    }
}

/// The smallest rectangle of any direction around a region, with its reading axis the one
/// nearer to horizontal. `None` for a region too small to have a direction.
fn turned_rectangle(runs: &[Run], region: &Region) -> Option<Rectangle> {
    // The ends of every run hold the region's convex hull.
    let mut points: Vec<(f32, f32)> = Vec::with_capacity(region.runs.len() * 2);
    for &i in &region.runs {
        let run = &runs[i as usize];
        points.push((run.x0 as f32, run.y as f32));
        if run.x1 != run.x0 {
            points.push((run.x1 as f32, run.y as f32));
        }
    }
    let hull = convex_hull(points);
    if hull.len() < 3 {
        return None;
    }
    let mut best: Option<(f32, Rectangle)> = None;
    for i in 0..hull.len() {
        let (a, b) = (hull[i], hull[(i + 1) % hull.len()]);
        let length = ((b.0 - a.0).powi(2) + (b.1 - a.1).powi(2)).sqrt();
        if length < 1e-3 {
            continue;
        }
        let (ux, uy) = ((b.0 - a.0) / length, (b.1 - a.1) / length);
        let (mut u0, mut u1, mut v0, mut v1) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
        for p in &hull {
            let (u, v) = (p.0 * ux + p.1 * uy, p.1 * ux - p.0 * uy);
            (u0, u1, v0, v1) = (u0.min(u), u1.max(u), v0.min(v), v1.max(v));
        }
        let area = (u1 - u0) * (v1 - v0);
        if best.as_ref().is_some_and(|(least, _)| area >= *least) {
            continue;
        }
        let (mu, mv) = ((u0 + u1) / 2.0, (v0 + v1) / 2.0);
        let (cx, cy) = (mu * ux - mv * uy, mu * uy + mv * ux);
        // Of the rectangle's two axes, the reading axis is the one nearer to horizontal, and it
        // points to the right.
        let rectangle = if ux.abs() >= uy.abs() {
            let sign = if ux < 0.0 { -1.0 } else { 1.0 };
            Rectangle { cx, cy, width: u1 - u0, height: v1 - v0, cos: ux * sign, sin: uy * sign }
        } else {
            let sign = if uy > 0.0 { -1.0 } else { 1.0 };
            Rectangle { cx, cy, width: v1 - v0, height: u1 - u0, cos: -uy * sign, sin: ux * sign }
        };
        best = Some((area, rectangle));
    }
    best.map(|(_, rectangle)| rectangle)
}

/// The convex hull of a set of points (Andrew's monotone chain), counter-clockwise.
fn convex_hull(mut points: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    points.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    points.dedup();
    if points.len() < 3 {
        return points;
    }
    let cross = |o: (f32, f32), a: (f32, f32), b: (f32, f32)| (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0);
    let mut hull: Vec<(f32, f32)> = Vec::with_capacity(points.len() * 2);
    for pass in 0..2 {
        let floor = hull.len() + 1;
        let ordered: Box<dyn Iterator<Item = &(f32, f32)>> =
            if pass == 0 { Box::new(points.iter()) } else { Box::new(points.iter().rev()) };
        for p in ordered {
            while hull.len() > floor && cross(hull[hull.len() - 2], hull[hull.len() - 1], *p) <= 0.0 {
                hull.pop();
            }
            hull.push(*p);
        }
        hull.pop();
    }
    hull
}
