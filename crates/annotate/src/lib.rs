//! The annotation document: what the user drew on a capture, kept as plain data.
//!
//! The overlay and the editor keep their drawings as a [`Document`]. The document crosses the
//! bridge as JSON, is saved beside a capture in the history (milestone 4) so the capture can be
//! edited again later, and is the input of the exporter. No rendering code lives here: the canvas
//! renderer in the webview draws a document (milestone 2), and a native renderer could draw the
//! same data, which is why the model stays plain.
//!
//! `src/lib/annotate.ts` mirrors every type by hand. `fixtures/annotate/document.json` holds one
//! object of every kind, and a test on each side reads it and writes it back unchanged, so a field
//! renamed on one side only fails both test suites.
//!
//! # Coordinates
//!
//! Every position and length is in pixels of the document's image: the origin is the image's
//! top-left corner, x grows to the right, y grows downwards, and fractional values are allowed
//! (the centre of the top-left pixel is `0.5, 0.5`). Stroke widths, text sizes and marker
//! diameters are image pixels as well.
//!
//! Image pixels are used instead of logical points because the exported file is made of pixels:
//! a drawing then lands on the same pixels in every renderer and on every display, and no reader
//! needs to know the scale of the display it was drawn on. [`Document::scale`] records how many
//! image pixels made one point when the capture was taken, so the editor can still show widths
//! and text sizes in the points the user picked.
//!
//! In the overlay, the image is the whole frozen frame of one display and the selection is the
//! document's [`Shape::Crop`] object, so moving or resizing the selection never moves a drawing.
//! In the editor, the image is the saved capture.
//!
//! # Stacking order
//!
//! The order of [`Document::objects`] is the stacking order: the first object is drawn first and
//! sits at the bottom, the last one sits on top. There is no separate z field, so the order can
//! never disagree with itself. Blur and pixelate act on the image pixels under them, never on the
//! drawings, and the exporter flattens them into the pixels so the hidden content cannot be
//! recovered from the file.
//!
//! Blur and pixelate are drawn first, straight into the image pixels, whatever their place in
//! the list; their place only decides which of two overlapping ones wins. Every other object is
//! drawn in list order above them. The spotlights of a document form one dim layer: everything
//! outside the union of their rectangles is dimmed, drawings below the topmost spotlight
//! included, with the darkest dim any of them asks for.
//!
//! # Versions
//!
//! A document carries the [`Version`] it was written in. A minor version only adds optional
//! fields, and a reader ignores the fields it does not know. A major version changes the meaning
//! of existing data, so a reader refuses a document with a newer major version
//! ([`AnnotateError::NewerVersion`]) instead of drawing it wrongly and saving the damage back.
//!
//! An older document is brought up to [`CURRENT_VERSION`] by [`migrate`] before it is read, one
//! version step at a time, and `fixtures/annotate/versions/` keeps one document per older version
//! that a test loads through the migration.
//!
//! | Version | Change |
//! |---|---|
//! | 1.0 | The first format. |
//! | 1.1 | Every object gains `hidden` and `locked` (the objects panel); both default to `false`. |
//! | 1.2 | The document gains `beautify` ([`BeautifyStyle`]); a document without it is not beautified. |

pub mod beautify;

use std::collections::HashSet;
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use beautify::{built_in_presets, Background, BeautifyPreset, BeautifyStyle, Shadow, WindowFrame, DEFAULT_PRESET};

pub type Result<T, E = AnnotateError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum AnnotateError {
    #[error("{0} is not implemented yet")]
    NotImplemented(&'static str),
    /// The document comes from a newer Glimpsfy whose format this build cannot read safely.
    #[error("the document is in format {found}, and this version of Glimpsfy reads format {supported} and older")]
    NewerVersion { found: Version, supported: Version },
    #[error("the document is not valid JSON of the expected shape: {0}")]
    Json(#[from] serde_json::Error),
    /// The JSON has the right shape, and a value in it breaks a rule of the model.
    #[error("the document is invalid: {0}")]
    Invalid(String),
}

/// Gives the format version of a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    pub major: u32,
    pub minor: u32,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The version this build writes. A reader accepts every document whose major version is not
/// higher than this one.
pub const CURRENT_VERSION: Version = Version { major: 1, minor: 2 };

/// Gives the size of the document's image in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageSize {
    pub width: u32,
    pub height: u32,
}

/// Gives a position in image pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// Describes an axis-aligned rectangle in image pixels. `x, y` is the top-left corner, and the width and
/// height are never negative: a renderer normalises a drag that runs up or left before it stores
/// the rectangle.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl Rect {
    /// The part of this rectangle inside `other`, or `None` when they do not overlap.
    pub fn intersection(&self, other: &Rect) -> Option<Rect> {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let right = (self.x + self.width).min(other.x + other.width);
        let bottom = (self.y + self.height).min(other.y + other.height);
        (right > x && bottom > y).then_some(Rect { x, y, width: right - x, height: bottom - y })
    }
}

/// Identifies an object uniquely inside one document. The UI creates it when the object is drawn,
/// and undo, redo and selection refer to objects by it.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObjectId(pub String);

/// Names the annotation palette of the kit (`--ink-*` in `design/kit/glass.css`). A colour stored as an
/// ink follows the kit if a value is ever retuned, which a hex copy of it would not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ink {
    Red,
    Orange,
    Yellow,
    Green,
    Cyan,
    Blue,
    White,
    Black,
}

/// Holds an explicit colour as `#rrggbb` or `#rrggbbaa`. The text is kept as written, so a document
/// reads back byte for byte.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HexColor(String);

impl HexColor {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HexColor {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let digits = value.strip_prefix('#').unwrap_or("");
        let valid = matches!(digits.len(), 6 | 8) && digits.bytes().all(|b| b.is_ascii_hexdigit());
        if valid {
            Ok(HexColor(value))
        } else {
            Err(format!("{value:?} is not a colour of the form #rrggbb or #rrggbbaa"))
        }
    }
}

impl From<HexColor> for String {
    fn from(color: HexColor) -> Self {
        color.0
    }
}

/// Gives the colour of a stroke, a fill, a text or a marker.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Color {
    /// Picks one of the eight inks of the options strip.
    Ink { name: Ink },
    /// A colour the user picked outside the palette.
    Hex { value: HexColor },
}

/// Draws the outline of a shape or the body of a line.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Stroke {
    pub color: Color,
    /// Gives the width in image pixels. The strip offers 2, 4 and 6 points, so a 2× capture stores
    /// 4, 8 and 12.
    pub width: f64,
}

/// How strongly a blur hides what is under it. The renderer turns the step into a radius that
/// grows with [`Document::scale`], so the same step looks the same on every display.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BlurStrength {
    Soft,
    Medium,
    Strong,
}

/// How large the cells of a pixelate object are, scaled with [`Document::scale`] as a blur is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CellSize {
    Fine,
    Medium,
    Coarse,
}

/// How dark a spotlight makes everything outside its rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SpotlightDim {
    Light,
    Medium,
    Dark,
}

/// What an object draws, with its kind as the `kind` tag.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Shape {
    /// Draws a straight line with an arrowhead at `to`. The head's size follows the stroke width.
    Arrow {
        from: Point,
        to: Point,
        stroke: Stroke,
    },
    Line {
        from: Point,
        to: Point,
        stroke: Stroke,
    },
    /// Draws a rectangle outline with an optional fill under it.
    Rectangle {
        rect: Rect,
        stroke: Stroke,
        fill: Option<Color>,
    },
    /// Draws the ellipse inscribed in `rect`, with an optional fill under it.
    Ellipse {
        rect: Rect,
        stroke: Stroke,
        fill: Option<Color>,
    },
    /// A freehand path through `points` in the order they were drawn. The renderer smooths it.
    Pen {
        points: Vec<Point>,
        stroke: Stroke,
    },
    /// A wide translucent path that marks text without hiding it. The renderer applies the
    /// translucency, so `color` is the plain ink.
    Highlighter {
        points: Vec<Point>,
        color: Color,
        width: f64,
    },
    /// A block of text whose first line starts at `origin`, the top-left corner of the block.
    Text {
        origin: Point,
        text: String,
        /// Gives the size of the font in image pixels. The strip offers 11, 14 and 18 points.
        font_size: f64,
        color: Color,
        /// Puts a rounded plate behind the text; `None` leaves the text bare.
        background: Option<Color>,
        /// The width at which lines wrap, or `None` to break lines only at line breaks.
        max_width: Option<f64>,
    },
    /// Draws a numbered disc. The tool gives a new marker the next number, and deleting a marker
    /// renumbers the markers after it in the same undoable step, so the sequence never has a gap.
    /// The number is stored rather than derived from the order, so every renderer shows the same
    /// number without knowing the rule.
    Step {
        center: Point,
        number: u32,
        color: Color,
        diameter: f64,
    },
    Blur {
        rect: Rect,
        strength: BlurStrength,
    },
    Pixelate {
        rect: Rect,
        cell: CellSize,
    },
    /// Keeps `rect` bright and dims everything outside it, drawings below the spotlight included.
    /// Several spotlights share one dim layer (see the module docs).
    Spotlight {
        rect: Rect,
        dim: SpotlightDim,
    },
    /// The part of the image that is exported. A document has at most one crop, and without one
    /// the whole image is exported. Drawings outside the crop stay in the document, so widening
    /// the crop brings them back.
    Crop {
        rect: Rect,
    },
}

/// The kind of an object without its data, as the `kind` tag spells it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ObjectKind {
    Arrow,
    Line,
    Rectangle,
    Ellipse,
    Pen,
    Highlighter,
    Text,
    Step,
    Blur,
    Pixelate,
    Spotlight,
    Crop,
}

impl ObjectKind {
    /// Lists every kind in the order of the tool bar.
    pub const ALL: [ObjectKind; 12] = [
        ObjectKind::Arrow,
        ObjectKind::Line,
        ObjectKind::Rectangle,
        ObjectKind::Ellipse,
        ObjectKind::Pen,
        ObjectKind::Highlighter,
        ObjectKind::Text,
        ObjectKind::Step,
        ObjectKind::Blur,
        ObjectKind::Pixelate,
        ObjectKind::Spotlight,
        ObjectKind::Crop,
    ];
}

impl Shape {
    pub fn kind(&self) -> ObjectKind {
        match self {
            Shape::Arrow { .. } => ObjectKind::Arrow,
            Shape::Line { .. } => ObjectKind::Line,
            Shape::Rectangle { .. } => ObjectKind::Rectangle,
            Shape::Ellipse { .. } => ObjectKind::Ellipse,
            Shape::Pen { .. } => ObjectKind::Pen,
            Shape::Highlighter { .. } => ObjectKind::Highlighter,
            Shape::Text { .. } => ObjectKind::Text,
            Shape::Step { .. } => ObjectKind::Step,
            Shape::Blur { .. } => ObjectKind::Blur,
            Shape::Pixelate { .. } => ObjectKind::Pixelate,
            Shape::Spotlight { .. } => ObjectKind::Spotlight,
            Shape::Crop { .. } => ObjectKind::Crop,
        }
    }
}

/// One drawn object: its identity, its state in the objects panel and what it draws. In JSON they
/// are one flat object, `{ "id": "…", "hidden": false, "locked": false, "kind": "arrow", … }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Object {
    pub id: ObjectId,
    /// A hidden object is neither drawn nor exported; a hidden blur therefore hides nothing.
    #[serde(default)]
    pub hidden: bool,
    /// A locked object cannot be selected on the canvas, so a drag over it starts a new drawing.
    #[serde(default)]
    pub locked: bool,
    #[serde(flatten)]
    pub shape: Shape,
}

impl Object {
    pub fn kind(&self) -> ObjectKind {
        self.shape.kind()
    }
}

/// Holds everything drawn on one image.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Document {
    pub version: Version,
    /// The size of the image the coordinates refer to.
    pub image: ImageSize,
    /// Image pixels per logical point when the capture was taken: 2.0 for a Retina display.
    pub scale: f64,
    /// Lists the objects in stacking order, the bottom one first.
    pub objects: Vec<Object>,
    /// The padding, background, corner, shadow and frame an export puts around the exported area;
    /// `None` exports the area as it is. The exporter applies it after the drawings, so it never
    /// touches the area's own pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub beautify: Option<BeautifyStyle>,
}

impl Document {
    /// Creates an empty document in the current version.
    pub fn new(image: ImageSize, scale: f64) -> Self {
        Document { version: CURRENT_VERSION, image, scale, objects: Vec::new(), beautify: None }
    }

    /// Whether an export differs from the plain image: a drawing, a crop or a beautification.
    pub fn changes_image(&self) -> bool {
        !self.objects.is_empty() || self.beautify.is_some()
    }

    /// Reads a document from JSON text. See [`Document::from_value`].
    pub fn from_json(text: &str) -> Result<Self> {
        Document::from_value(serde_json::from_str(text)?)
    }

    /// Reads a document from a parsed JSON value. The version is checked before anything else,
    /// so a document from a newer major version fails with [`AnnotateError::NewerVersion`] even
    /// when its objects no longer have a shape this build knows. An older document goes through
    /// [`migrate`] first, and the document is validated after it is read.
    pub fn from_value(value: Value) -> Result<Self> {
        let document: Document = serde_json::from_value(migrate(value)?)?;
        document.validate()?;
        Ok(document)
    }

    /// Writes the document as compact JSON.
    pub fn to_json(&self) -> Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    /// The crop object's rectangle, if the document has one.
    pub fn crop(&self) -> Option<Rect> {
        self.objects.iter().find_map(|object| match object.shape {
            Shape::Crop { rect } => Some(rect),
            _ => None,
        })
    }

    /// The rectangle of the image that an export contains: the crop clipped to the image, or the
    /// whole image when there is no crop. `None` means the crop lies entirely outside the image.
    pub fn export_area(&self) -> Option<Rect> {
        let image = Rect { x: 0.0, y: 0.0, width: self.image.width as f64, height: self.image.height as f64 };
        match self.crop() {
            Some(crop) => crop.intersection(&image),
            None => Some(image),
        }
    }

    /// Checks the rules the JSON shape cannot express: a non-empty image, a positive scale,
    /// unique non-empty ids, finite numbers, non-negative sizes, at least one point in every path,
    /// marker numbers from 1, and at most one crop.
    pub fn validate(&self) -> Result<()> {
        let invalid = |message: String| Err(AnnotateError::Invalid(message));
        if self.image.width == 0 || self.image.height == 0 {
            return invalid("the image has no pixels".into());
        }
        if !(self.scale.is_finite() && self.scale > 0.0) {
            return invalid(format!("the scale {} is not a positive number", self.scale));
        }
        let mut ids = HashSet::new();
        let mut crops = 0;
        for object in &self.objects {
            let id = &object.id.0;
            if id.is_empty() {
                return invalid("an object has an empty id".into());
            }
            if !ids.insert(id.as_str()) {
                return invalid(format!("the id {id:?} is used by more than one object"));
            }
            if let Err(reason) = check_shape(&object.shape) {
                return invalid(format!("object {id:?}: {reason}"));
            }
            if object.kind() == ObjectKind::Crop {
                crops += 1;
            }
        }
        if crops > 1 {
            return invalid(format!("the document has {crops} crop objects, and at most one is allowed"));
        }
        if let Some(style) = &self.beautify {
            style.check().map_err(AnnotateError::Invalid)?;
        }
        Ok(())
    }
}

/// Reads the version of a document without reading anything else.
pub fn version_of(value: &Value) -> Result<Version> {
    let version = value.get("version").ok_or_else(|| AnnotateError::Invalid("the document has no version".into()))?;
    Ok(Version::deserialize(version)?)
}

/// Brings a document written by an older version up to [`CURRENT_VERSION`], one step at a time,
/// and leaves a current or newer-minor document as it is. A newer major version fails with
/// [`AnnotateError::NewerVersion`]. Each step only fills in what its version added, so no object
/// and no field the older version wrote is lost.
pub fn migrate(mut value: Value) -> Result<Value> {
    let found = version_of(&value)?;
    if found.major > CURRENT_VERSION.major {
        return Err(AnnotateError::NewerVersion { found, supported: CURRENT_VERSION });
    }
    if found >= CURRENT_VERSION {
        return Ok(value);
    }
    let Some(document) = value.as_object_mut() else {
        return Err(AnnotateError::Invalid("the document is not a JSON object".into()));
    };
    // 1.0 → 1.1: the objects panel's hidden and locked flags.
    if found < (Version { major: 1, minor: 1 }) {
        if let Some(Value::Array(objects)) = document.get_mut("objects") {
            for object in objects.iter_mut().filter_map(Value::as_object_mut) {
                object.entry("hidden").or_insert(Value::Bool(false));
                object.entry("locked").or_insert(Value::Bool(false));
            }
        }
    }
    // 1.1 → 1.2: `beautify` is optional and absent means no beautification, so nothing is filled
    // in; only the version changes.
    document.insert("version".into(), serde_json::to_value(CURRENT_VERSION)?);
    Ok(value)
}

fn check_shape(shape: &Shape) -> std::result::Result<(), String> {
    fn number(name: &str, value: f64) -> std::result::Result<(), String> {
        if value.is_finite() {
            Ok(())
        } else {
            Err(format!("{name} is not a finite number"))
        }
    }
    fn length(name: &str, value: f64) -> std::result::Result<(), String> {
        number(name, value)?;
        if value >= 0.0 {
            Ok(())
        } else {
            Err(format!("{name} is negative"))
        }
    }
    fn point(name: &str, p: &Point) -> std::result::Result<(), String> {
        number(name, p.x)?;
        number(name, p.y)
    }
    fn rect(r: &Rect) -> std::result::Result<(), String> {
        number("rect.x", r.x)?;
        number("rect.y", r.y)?;
        length("rect.width", r.width)?;
        length("rect.height", r.height)
    }
    fn path(points: &[Point]) -> std::result::Result<(), String> {
        if points.is_empty() {
            return Err("the path has no points".into());
        }
        points.iter().try_for_each(|p| point("a path point", p))
    }

    match shape {
        Shape::Arrow { from, to, stroke } | Shape::Line { from, to, stroke } => {
            point("from", from)?;
            point("to", to)?;
            length("stroke.width", stroke.width)
        }
        Shape::Rectangle { rect: r, stroke, .. } | Shape::Ellipse { rect: r, stroke, .. } => {
            rect(r)?;
            length("stroke.width", stroke.width)
        }
        Shape::Pen { points, stroke } => {
            path(points)?;
            length("stroke.width", stroke.width)
        }
        Shape::Highlighter { points, width, .. } => {
            path(points)?;
            length("width", *width)
        }
        Shape::Text { origin, font_size, max_width, .. } => {
            point("origin", origin)?;
            length("fontSize", *font_size)?;
            max_width.map_or(Ok(()), |w| length("maxWidth", w))
        }
        Shape::Step { center, number: n, diameter, .. } => {
            point("center", center)?;
            length("diameter", *diameter)?;
            if *n == 0 {
                return Err("a step marker starts at 1".into());
            }
            Ok(())
        }
        Shape::Blur { rect: r, .. }
        | Shape::Pixelate { rect: r, .. }
        | Shape::Spotlight { rect: r, .. }
        | Shape::Crop { rect: r } => rect(r),
    }
}
