//! What beautifying a capture asks for: the space around the image, what fills that space, the
//! image's rounded corner and shadow, and an optional window frame (format 1.2).
//!
//! The style is plain data, like the rest of the document. Lengths are logical points, not image
//! pixels, because a preset means the same look on a 1× and a 2× display: the renderer
//! (`src-tauri/src/output/beautify.rs`) multiplies them by the delivered image's pixels per point.
//! Colours are sRGB `#rrggbb`, and a renderer converts them into the image's colour space.
//!
//! A window capture keeps its own corner and shadow: of a style, only the padding and the
//! background apply to it, and the corner, the shadow and the frame apply to areas only.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::HexColor;

/// The largest padding a style may ask for, in points.
pub const MAX_PADDING: f64 = 160.0;
/// The largest corner radius, in points.
pub const MAX_CORNER: f64 = 32.0;
/// The height of the window frame's title bar, in points.
pub const FRAME_BAR: f64 = 28.0;

/// The shadow under the image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Shadow {
    None,
    Soft,
    Medium,
    Deep,
}

impl Shadow {
    /// The blur, the offset downwards (both in points) and the opacity of the shadow, or `None`.
    pub fn geometry(self) -> Option<(f64, f64, f64)> {
        match self {
            Shadow::None => None,
            Shadow::Soft => Some((8.0, 3.0, 0.22)),
            Shadow::Medium => Some((18.0, 8.0, 0.30)),
            Shadow::Deep => Some((30.0, 16.0, 0.42)),
        }
    }
}

/// The window frame drawn around an area: a 28-point title bar with the three window buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum WindowFrame {
    None,
    Light,
    Dark,
}

/// What fills the padding.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Background {
    /// Nothing: the padding stays transparent (white in a JPEG).
    Transparent,
    Solid {
        color: HexColor,
    },
    /// A linear gradient at 135°, from the top-left corner to the bottom-right one, through
    /// evenly spaced stops.
    Gradient {
        stops: Vec<HexColor>,
    },
    /// An image file, scaled to cover the whole output. Only its path is kept, never a copy; when
    /// the file cannot be read, the `fallback` gradient is used for that capture.
    Image {
        path: String,
        fallback: Vec<HexColor>,
    },
    /// The median colour of the capture's outermost 2-pixel band, so the padding continues the
    /// image's own edge.
    Match,
}

/// One beautification, as the panel, the document and the renderer share it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeautifyStyle {
    /// The preset the style was taken from, so the panel can name it. A renderer ignores it, and
    /// a style changed after it was taken still names its preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<String>,
    /// The space around the image on every side, 0 to 160 points.
    pub padding: f64,
    /// The image's corner radius, 0 to 32 points (areas only).
    pub corner: f64,
    pub shadow: Shadow,
    pub background: Background,
    pub frame: WindowFrame,
}

impl BeautifyStyle {
    /// The padding and the corner inside their ranges, and 0 for a value that is not a number,
    /// so a renderer never has to check them again.
    pub fn clamped(&self) -> BeautifyStyle {
        let fit = |v: f64, max: f64| if v.is_finite() { v.clamp(0.0, max) } else { 0.0 };
        BeautifyStyle { padding: fit(self.padding, MAX_PADDING), corner: fit(self.corner, MAX_CORNER), ..self.clone() }
    }

    /// Checks what the JSON shape cannot: finite, non-negative lengths and at least one colour in
    /// every gradient.
    pub fn check(&self) -> Result<(), String> {
        for (name, value) in [("padding", self.padding), ("corner", self.corner)] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("beautify.{name} is not a finite, non-negative number"));
            }
        }
        match &self.background {
            Background::Gradient { stops } if stops.is_empty() => Err("beautify.background has no stops".into()),
            Background::Image { fallback, .. } if fallback.is_empty() => {
                Err("beautify.background.fallback has no stops".into())
            }
            _ => Ok(()),
        }
    }
}

/// A named style the user can apply, as Settings › Beautify lists it. A preset Glimpsfy ships
/// has no `name`: its name comes from the language catalogue under its `id`, until the user
/// renames it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeautifyPreset {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    pub style: BeautifyStyle,
    /// Fields a newer Glimpsfy wrote, kept as they are.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

fn hex(value: &str) -> HexColor {
    HexColor::try_from(value.to_string()).unwrap_or_else(|_| unreachable!("the built-in colours are valid"))
}

fn preset(
    id: &str,
    padding: f64,
    corner: f64,
    shadow: Shadow,
    background: Background,
    frame: WindowFrame,
) -> BeautifyPreset {
    BeautifyPreset {
        id: id.into(),
        name: None,
        style: BeautifyStyle { preset: None, padding, corner, shadow, background, frame },
        extra: Map::new(),
    }
}

/// The id of the preset a new installation applies: Ocean.
pub const DEFAULT_PRESET: &str = "ocean";

/// The presets a new installation has, in the order Settings lists them.
pub fn built_in_presets() -> Vec<BeautifyPreset> {
    let gradient = |stops: &[&str]| Background::Gradient { stops: stops.iter().map(|s| hex(s)).collect() };
    vec![
        preset("ocean", 48.0, 12.0, Shadow::Medium, gradient(&["#2563ff", "#2fc6c0"]), WindowFrame::None),
        preset("sunset", 64.0, 16.0, Shadow::Deep, gradient(&["#ff8a4c", "#ff4f8b", "#8b4dff"]), WindowFrame::None),
        preset("paper", 40.0, 10.0, Shadow::Soft, Background::Solid { color: hex("#efe8dc") }, WindowFrame::Light),
        preset("night", 48.0, 12.0, Shadow::Deep, Background::Solid { color: hex("#17181f") }, WindowFrame::Dark),
        preset("match", 32.0, 8.0, Shadow::Soft, Background::Match, WindowFrame::None),
    ]
}
