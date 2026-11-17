//! Thumbnails: a PNG capture decoded row by row, converted from its colour space to sRGB, scaled
//! down by area averaging in linear light, and encoded as JPEG (or PNG when it has transparency).
//!
//! Area averaging weighs every source pixel by how much of it falls under each thumbnail pixel,
//! which is what a downscale of screen content needs: thin text and one-pixel rules fade evenly
//! instead of flickering in and out as a nearest or bilinear filter makes them. Averaging in
//! linear light keeps light text on dark backgrounds from thinning out.
//!
//! The colour conversion reads the PNG's ICC profile when it is a matrix-and-curves RGB profile
//! (Display P3, sRGB, Adobe RGB, the profiles macOS writes for displays) and treats every other
//! image as sRGB. The 3 × 3 conversion is linear, so it is applied once per thumbnail pixel after
//! the averaging, not per source pixel.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use crate::{HistoryError, Result};

/// The thumbnail's width, about two card widths of the grid at its largest size.
pub const MAX_WIDTH: u32 = 480;
/// The tallest thumbnail; a very tall capture gets a narrower one.
pub const MAX_HEIGHT: u32 = 1440;
/// Memory the decoder may use for one image, enough for a scrolling capture.
const DECODE_LIMIT: usize = 1 << 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbnailFormat {
    Jpeg,
    Png,
}

#[derive(Clone, Debug)]
pub struct Thumbnail {
    pub bytes: Vec<u8>,
    pub format: ThumbnailFormat,
    pub width: u32,
    pub height: u32,
}

/// An image decoded to tight RGBA rows, for text recognition.
#[derive(Clone, Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// The ICC profile the file carried, if any.
    pub icc_profile: Option<Vec<u8>>,
    /// Whether the profile describes Display P3 (by its primaries).
    pub display_p3: bool,
}

fn image_error(error: impl std::fmt::Display) -> HistoryError {
    HistoryError::Image(error.to_string())
}

fn open_png(path: &Path) -> Result<png::Reader<BufReader<File>>> {
    let file = File::open(path)?;
    let mut decoder = png::Decoder::new_with_limits(BufReader::new(file), png::Limits { bytes: DECODE_LIMIT });
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    decoder.set_ignore_text_chunk(true);
    decoder.read_info().map_err(image_error)
}

/// Converts one decoded row (in the reader's output colour type) to RGBA.
fn to_rgba(color: png::ColorType, row: &[u8], out: &mut Vec<u8>) {
    out.clear();
    match color {
        png::ColorType::Rgba => out.extend_from_slice(row),
        png::ColorType::Rgb => row.chunks_exact(3).for_each(|p| out.extend_from_slice(&[p[0], p[1], p[2], 255])),
        png::ColorType::GrayscaleAlpha => {
            row.chunks_exact(2).for_each(|p| out.extend_from_slice(&[p[0], p[0], p[0], p[1]]))
        }
        png::ColorType::Grayscale => row.iter().for_each(|&g| out.extend_from_slice(&[g, g, g, 255])),
        // EXPAND turns indexed images into RGB(A), so this arm only guards against a decoder change.
        png::ColorType::Indexed => row.iter().for_each(|&g| out.extend_from_slice(&[g, g, g, 255])),
    }
}

/// Decodes a PNG to RGBA, for text recognition.
pub fn decode_png(path: &Path) -> Result<DecodedImage> {
    let mut reader = open_png(path)?;
    let (color, _) = reader.output_color_type();
    let size = reader.output_buffer_size().ok_or_else(|| image_error("the image is too large"))?;
    let mut buffer = vec![0u8; size];
    let frame = reader.next_frame(&mut buffer).map_err(image_error)?;
    let info = reader.info();
    let icc_profile = info.icc_profile.as_ref().map(|p| p.to_vec());
    let display_p3 = icc_profile.as_deref().and_then(Profile::parse).is_some_and(|p| p.is_display_p3());
    let (width, height) = (frame.width, frame.height);
    let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
    let mut row = Vec::new();
    for line in buffer[..frame.buffer_size()].chunks_exact(frame.line_size) {
        to_rgba(color, line, &mut row);
        rgba.extend_from_slice(&row);
    }
    Ok(DecodedImage { width, height, rgba, icc_profile, display_p3 })
}

/// The thumbnail of a PNG file.
pub fn from_png(path: &Path) -> Result<Thumbnail> {
    let mut reader = open_png(path)?;
    let info = reader.info();
    let (width, height) = (info.width, info.height);
    if width == 0 || height == 0 {
        return Err(image_error("the image is empty"));
    }
    let profile = info.icc_profile.as_deref().and_then(Profile::parse).unwrap_or_else(Profile::srgb);
    let (color, _) = reader.output_color_type();
    let mut scaler = Scaler::new(width, height, &profile);
    let mut rgba = Vec::with_capacity(width as usize * 4);
    if info.interlaced {
        let size = reader.output_buffer_size().ok_or_else(|| image_error("the image is too large"))?;
        let mut buffer = vec![0u8; size];
        let frame = reader.next_frame(&mut buffer).map_err(image_error)?;
        for line in buffer[..frame.buffer_size()].chunks_exact(frame.line_size) {
            to_rgba(color, line, &mut rgba);
            scaler.push_row(&rgba);
        }
    } else {
        while let Some(row) = reader.next_row().map_err(image_error)? {
            to_rgba(color, row.data(), &mut rgba);
            scaler.push_row(&rgba);
        }
    }
    scaler.finish()
}

/// The thumbnail of an image already decoded to tight sRGB RGBA rows (by the system's decoder,
/// for formats other than PNG).
pub fn from_srgb_rgba(width: u32, height: u32, rgba: &[u8]) -> Result<Thumbnail> {
    let row = width as usize * 4;
    if width == 0 || height == 0 || rgba.len() < row * height as usize {
        return Err(image_error("the image is empty or shorter than its size says"));
    }
    let mut scaler = Scaler::new(width, height, &Profile::srgb());
    for line in rgba.chunks_exact(row).take(height as usize) {
        scaler.push_row(line);
    }
    scaler.finish()
}

/// The size a thumbnail of a `width` × `height` image gets: at most [`MAX_WIDTH`] wide and
/// [`MAX_HEIGHT`] tall, never larger than the image.
pub fn thumbnail_size(width: u32, height: u32) -> (u32, u32) {
    let (w, h) = (width.max(1) as f64, height.max(1) as f64);
    let scale = (MAX_WIDTH as f64 / w).min(MAX_HEIGHT as f64 / h).min(1.0);
    (((w * scale).round() as u32).max(1), ((h * scale).round() as u32).max(1))
}

/// For each destination index, the first source index it covers and the weight of each source
/// index from there, summing to 1.
fn area_weights(source: u32, target: u32) -> Vec<(u32, Vec<f32>)> {
    let ratio = source as f64 / target as f64;
    (0..target)
        .map(|i| {
            let (start, end) = (i as f64 * ratio, ((i + 1) as f64 * ratio).min(source as f64));
            let first = start.floor() as u32;
            let last = (end.ceil() as u32).min(source).max(first + 1);
            let weights = (first..last)
                .map(|s| ((end.min(s as f64 + 1.0) - start.max(s as f64)).max(0.0) / (end - start)) as f32)
                .collect();
            (first, weights)
        })
        .collect()
}

/// Accumulates source rows into the thumbnail, in linear light with premultiplied alpha.
struct Scaler {
    width: u32,
    target: (u32, u32),
    columns: Vec<(u32, Vec<f32>)>,
    /// For each source row, the thumbnail rows it falls into and its weight there.
    rows: Vec<Vec<(u32, f32)>>,
    linear: [[f32; 256]; 3],
    to_srgb: [[f32; 3]; 3],
    accumulated: Vec<[f32; 4]>,
    line: Vec<[f32; 4]>,
    next_row: u32,
    opaque: bool,
}

impl Scaler {
    fn new(width: u32, height: u32, profile: &Profile) -> Scaler {
        let target = thumbnail_size(width, height);
        let mut rows = vec![Vec::new(); height as usize];
        for (j, (first, weights)) in area_weights(height, target.1).into_iter().enumerate() {
            for (k, w) in weights.into_iter().enumerate() {
                if w > 0.0 {
                    rows[(first as usize + k).min(height as usize - 1)].push((j as u32, w));
                }
            }
        }
        let linear = std::array::from_fn(|c| std::array::from_fn(|v| profile.curves[c].eval(v as f32 / 255.0)));
        Scaler {
            width,
            target,
            columns: area_weights(width, target.0),
            rows,
            linear,
            to_srgb: profile.to_srgb,
            accumulated: vec![[0.0; 4]; (target.0 * target.1) as usize],
            line: vec![[0.0; 4]; target.0 as usize],
            next_row: 0,
            opaque: true,
        }
    }

    fn push_row(&mut self, rgba: &[u8]) {
        let y = self.next_row;
        self.next_row += 1;
        if y as usize >= self.rows.len() || rgba.len() < self.width as usize * 4 {
            return;
        }
        for (x, (first, weights)) in self.columns.iter().enumerate() {
            let mut sum = [0.0f32; 4];
            for (k, w) in weights.iter().enumerate() {
                let p = (*first as usize + k) * 4;
                let a = rgba[p + 3];
                if a != 255 {
                    self.opaque = false;
                }
                let alpha = a as f32 / 255.0;
                let weight = w * alpha;
                sum[0] += weight * self.linear[0][rgba[p] as usize];
                sum[1] += weight * self.linear[1][rgba[p + 1] as usize];
                sum[2] += weight * self.linear[2][rgba[p + 2] as usize];
                sum[3] += weight;
            }
            self.line[x] = sum;
        }
        let width = self.target.0 as usize;
        for &(j, weight) in &self.rows[y as usize] {
            let row = &mut self.accumulated[j as usize * width..(j as usize + 1) * width];
            for (acc, px) in row.iter_mut().zip(&self.line) {
                for c in 0..4 {
                    acc[c] += weight * px[c];
                }
            }
        }
    }

    fn finish(self) -> Result<Thumbnail> {
        let (width, height) = self.target;
        let channels = if self.opaque { 3 } else { 4 };
        let mut out = Vec::with_capacity((width * height) as usize * channels);
        for px in &self.accumulated {
            let alpha = px[3].clamp(0.0, 1.0);
            let rgb = if px[3] > 0.0 { [px[0] / px[3], px[1] / px[3], px[2] / px[3]] } else { [0.0; 3] };
            for row in &self.to_srgb {
                let v = row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2];
                out.push((encode_srgb(v) * 255.0 + 0.5) as u8);
            }
            if !self.opaque {
                out.push((alpha * 255.0 + 0.5) as u8);
            }
        }
        if self.opaque {
            let mut bytes = Vec::new();
            let mut encoder = jpeg_encoder::Encoder::new(&mut bytes, 85);
            encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::R_4_4_4);
            encoder.set_optimized_huffman_tables(true);
            encoder.encode(&out, width as u16, height as u16, jpeg_encoder::ColorType::Rgb).map_err(image_error)?;
            Ok(Thumbnail { bytes, format: ThumbnailFormat::Jpeg, width, height })
        } else {
            let mut bytes = Vec::new();
            let mut encoder = png::Encoder::new(&mut bytes, width, height);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
            encoder.set_compression(png::Compression::Balanced);
            let mut writer = encoder.write_header().map_err(image_error)?;
            writer.write_image_data(&out).map_err(image_error)?;
            writer.finish().map_err(image_error)?;
            Ok(Thumbnail { bytes, format: ThumbnailFormat::Png, width, height })
        }
    }
}

fn encode_srgb(linear: f32) -> f32 {
    let v = linear.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

// --------------------------------------------------------------------------------- profiles

/// A tone curve of an ICC profile.
#[derive(Clone, Debug)]
enum Curve {
    Gamma(f32),
    Table(Vec<f32>),
    /// The ICC parametric curve: `(a·x + b)^g + e` from `d` on, `c·x + f` below it.
    Parametric {
        g: f32,
        a: f32,
        b: f32,
        c: f32,
        d: f32,
        e: f32,
        f: f32,
    },
}

impl Curve {
    fn srgb() -> Curve {
        Curve::Parametric { g: 2.4, a: 1.0 / 1.055, b: 0.055 / 1.055, c: 1.0 / 12.92, d: 0.04045, e: 0.0, f: 0.0 }
    }

    fn eval(&self, x: f32) -> f32 {
        match self {
            Curve::Gamma(g) => x.powf(*g),
            Curve::Table(table) => {
                let at = x.clamp(0.0, 1.0) * (table.len() - 1) as f32;
                let i = at.floor() as usize;
                let j = (i + 1).min(table.len() - 1);
                table[i] + (table[j] - table[i]) * (at - i as f32)
            }
            Curve::Parametric { g, a, b, c, d, e, f } => {
                if x >= *d {
                    (a * x + b).max(0.0).powf(*g) + e
                } else {
                    c * x + f
                }
            }
        }
    }
}

/// The parts of an RGB matrix-and-curves ICC profile a conversion to sRGB needs.
#[derive(Clone, Debug)]
struct Profile {
    curves: [Curve; 3],
    /// Linear RGB of the profile to linear sRGB.
    to_srgb: [[f32; 3]; 3],
    /// The profile's primaries in the D50 connection space, columns red, green, blue.
    primaries: [[f32; 3]; 3],
}

/// XYZ in the ICC connection space (D50) to linear sRGB, with Bradford adaptation to D65.
const XYZ_D50_TO_SRGB: [[f32; 3]; 3] = [
    [3.133_856, -1.616_866_7, -0.490_614_6],
    [-0.978_768_4, 1.916_141_5, 0.033_454],
    [0.071_945_3, -0.228_991_4, 1.405_242_7],
];

/// Display P3's primaries in the D50 connection space (the rXYZ, gXYZ and bXYZ of Apple's
/// profile), to recognise it among other profiles.
const DISPLAY_P3_D50: [[f32; 3]; 3] = [[0.5151, 0.2920, 0.1571], [0.2412, 0.6922, 0.0666], [-0.0011, 0.0419, 0.7841]];

impl Profile {
    fn srgb() -> Profile {
        let identity = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        Profile { curves: [Curve::srgb(), Curve::srgb(), Curve::srgb()], to_srgb: identity, primaries: identity }
    }

    fn is_display_p3(&self) -> bool {
        self.primaries.iter().flatten().zip(DISPLAY_P3_D50.iter().flatten()).all(|(a, b)| (a - b).abs() < 0.003)
    }

    /// Reads an RGB matrix-and-curves profile, or gives `None` for anything else (a LUT-based or
    /// non-RGB profile, or a damaged one), which is then treated as sRGB.
    fn parse(data: &[u8]) -> Option<Profile> {
        let be32 = |at: usize| data.get(at..at + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        let be16 = |at: usize| data.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
        let s15 = |at: usize| be32(at).map(|v| v as i32 as f32 / 65536.0);
        if data.get(16..20)? != b"RGB " || data.get(20..24)? != b"XYZ " {
            return None;
        }
        let count = be32(128)? as usize;
        let tag = |signature: &[u8; 4]| -> Option<(usize, usize)> {
            (0..count.min(256)).find_map(|i| {
                let at = 132 + i * 12;
                (data.get(at..at + 4)? == signature).then(|| Some((be32(at + 4)? as usize, be32(at + 8)? as usize)))?
            })
        };
        let xyz = |signature: &[u8; 4]| -> Option<[f32; 3]> {
            let (at, _) = tag(signature)?;
            (data.get(at..at + 4)? == b"XYZ ").then_some(())?;
            Some([s15(at + 8)?, s15(at + 12)?, s15(at + 16)?])
        };
        let curve = |signature: &[u8; 4]| -> Option<Curve> {
            let (at, _) = tag(signature)?;
            match data.get(at..at + 4)? {
                b"curv" => {
                    let n = be32(at + 8)? as usize;
                    match n {
                        0 => Some(Curve::Gamma(1.0)),
                        1 => Some(Curve::Gamma(be16(at + 12)? as f32 / 256.0)),
                        _ => Some(Curve::Table(
                            (0..n.min(65536))
                                .map(|i| be16(at + 12 + 2 * i).map(|v| v as f32 / 65535.0))
                                .collect::<Option<_>>()?,
                        )),
                    }
                }
                b"para" => {
                    let kind = be16(at + 8)?;
                    let p = |i: usize| s15(at + 12 + 4 * i);
                    let g = p(0)?;
                    Some(match kind {
                        0 => Curve::Gamma(g),
                        1 => {
                            let (a, b) = (p(1)?, p(2)?);
                            Curve::Parametric {
                                g,
                                a,
                                b,
                                c: 0.0,
                                d: if a != 0.0 { -b / a } else { 0.0 },
                                e: 0.0,
                                f: 0.0,
                            }
                        }
                        2 => {
                            let (a, b, c) = (p(1)?, p(2)?, p(3)?);
                            Curve::Parametric { g, a, b, c: 0.0, d: if a != 0.0 { -b / a } else { 0.0 }, e: c, f: c }
                        }
                        3 => Curve::Parametric { g, a: p(1)?, b: p(2)?, c: p(3)?, d: p(4)?, e: 0.0, f: 0.0 },
                        4 => Curve::Parametric { g, a: p(1)?, b: p(2)?, c: p(3)?, d: p(4)?, e: p(5)?, f: p(6)? },
                        _ => return None,
                    })
                }
                _ => None,
            }
        };
        let (r, g, b) = (xyz(b"rXYZ")?, xyz(b"gXYZ")?, xyz(b"bXYZ")?);
        let primaries = [[r[0], g[0], b[0]], [r[1], g[1], b[1]], [r[2], g[2], b[2]]];
        let mut to_srgb = [[0.0f32; 3]; 3];
        for (i, row) in to_srgb.iter_mut().enumerate() {
            for (j, value) in row.iter_mut().enumerate() {
                *value = (0..3).map(|k| XYZ_D50_TO_SRGB[i][k] * primaries[k][j]).sum();
            }
        }
        Some(Profile { curves: [curve(b"rTRC")?, curve(b"gTRC")?, curve(b"bTRC")?], to_srgb, primaries })
    }
}
