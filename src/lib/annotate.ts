// The annotation document, mirrored by hand from crates/annotate/src/lib.rs. Read that file's
// module docs for the rules: every coordinate and length is in pixels of the document's image,
// the order of `objects` is the stacking order (the first object sits at the bottom), and a
// document from a newer major version is refused instead of being drawn wrongly.
//
// `parseDocument` rebuilds a document field by field, so a field that is renamed or missing on
// either side throws here instead of disappearing from a saved document. The shared fixture
// (fixtures/annotate/document.json) is read by a test on both sides to keep them in step, and an
// older document is brought up to the current version by `migrateDocument` (the same steps as the
// Rust `migrate`) before it is read.

export type Version = { major: number; minor: number };

/** The version this build writes; it reads every document whose major version is not higher. */
export const CURRENT_VERSION: Version = { major: 1, minor: 3 };

export type ImageSize = { width: number; height: number };
export type Point = { x: number; y: number };
export type Rect = { x: number; y: number; width: number; height: number };
export type ObjectId = string;

export const INKS = ["red", "orange", "yellow", "green", "cyan", "blue", "white", "black"] as const;
export type Ink = (typeof INKS)[number];

export type Color = { kind: "ink"; name: Ink } | { kind: "hex"; value: string };
export type Stroke = { color: Color; width: number };

export const BLUR_STRENGTHS = ["soft", "medium", "strong"] as const;
export type BlurStrength = (typeof BLUR_STRENGTHS)[number];
export const CELL_SIZES = ["fine", "medium", "coarse"] as const;
export type CellSize = (typeof CELL_SIZES)[number];
export const SPOTLIGHT_DIMS = ["light", "medium", "dark"] as const;
export type SpotlightDim = (typeof SPOTLIGHT_DIMS)[number];
/** How many times a magnifier enlarges its source (format 1.3). */
export const MAGNIFY_FACTORS = [2, 3, 4] as const;
export type MagnifyFactor = (typeof MAGNIFY_FACTORS)[number];

export type Shape =
  | { kind: "arrow"; from: Point; to: Point; stroke: Stroke }
  | { kind: "line"; from: Point; to: Point; stroke: Stroke }
  | { kind: "rectangle"; rect: Rect; stroke: Stroke; fill: Color | null }
  | { kind: "ellipse"; rect: Rect; stroke: Stroke; fill: Color | null }
  | { kind: "pen"; points: Point[]; stroke: Stroke }
  | { kind: "highlighter"; points: Point[]; color: Color; width: number }
  | {
      kind: "text";
      origin: Point;
      text: string;
      fontSize: number;
      color: Color;
      background: Color | null;
      maxWidth: number | null;
    }
  | { kind: "step"; center: Point; number: number; color: Color; diameter: number }
  /** The circle of `radius` around `source` enlarged `factor` times into a loupe centred on `at`
   * (format 1.3); see `Shape::Magnify` for what the loupe shows. */
  | { kind: "magnify"; source: Point; radius: number; at: Point; factor: MagnifyFactor; smooth: boolean; color: Color }
  | { kind: "blur"; rect: Rect; strength: BlurStrength }
  | { kind: "pixelate"; rect: Rect; cell: CellSize }
  | { kind: "spotlight"; rect: Rect; dim: SpotlightDim }
  | { kind: "crop"; rect: Rect };

export type ObjectKind = Shape["kind"];

/** This lists every kind in the order of the tool bar. */
export const OBJECT_KINDS = [
  "arrow",
  "line",
  "rectangle",
  "ellipse",
  "pen",
  "highlighter",
  "text",
  "step",
  "magnify",
  "blur",
  "pixelate",
  "spotlight",
  "crop",
] as const satisfies readonly ObjectKind[];

// Fails to compile when a kind of `Shape` is missing from OBJECT_KINDS.
type MissingKinds = Exclude<ObjectKind, (typeof OBJECT_KINDS)[number]>;
const allKindsListed: [MissingKinds] extends [never] ? true : MissingKinds = true;
void allKindsListed;

/** State the objects panel keeps on every object (format 1.1). */
export type ObjectState = {
  /** A hidden object is neither drawn nor exported. */
  hidden: boolean;
  /** A locked object cannot be selected on the canvas. */
  locked: boolean;
};

export type AnnotationObject = { id: ObjectId } & ObjectState & Shape;

// ------------------------------------------------------------------------------- beautify (1.2)
// crates/annotate/src/beautify.rs: lengths are logical points, colours are sRGB `#rrggbb`, and a
// window capture takes only the padding and the background.

/** The largest padding a style may ask for, in points. */
export const MAX_PADDING = 160;
/** The largest corner radius, in points. */
export const MAX_CORNER = 32;
/** The height of the window frame's title bar, in points. */
export const FRAME_BAR = 28;

export const SHADOWS = ["none", "soft", "medium", "deep"] as const;
export type Shadow = (typeof SHADOWS)[number];
export const WINDOW_FRAMES = ["none", "light", "dark"] as const;
export type WindowFrame = (typeof WINDOW_FRAMES)[number];

/** The blur, the offset downwards (both in points) and the opacity of each shadow. */
export const SHADOW_GEOMETRY: Record<Exclude<Shadow, "none">, { blur: number; dy: number; opacity: number }> = {
  soft: { blur: 8, dy: 3, opacity: 0.22 },
  medium: { blur: 18, dy: 8, opacity: 0.3 },
  deep: { blur: 30, dy: 16, opacity: 0.42 },
};

export type Background =
  | { kind: "transparent" }
  | { kind: "solid"; color: string }
  /** A linear gradient at 135°, through evenly spaced stops. */
  | { kind: "gradient"; stops: string[] }
  /** An image file that covers the output; `fallback` is used when it cannot be read. */
  | { kind: "image"; path: string; fallback: string[] }
  /** The median colour of the capture's outermost 2-pixel band. */
  | { kind: "match" };

export type BeautifyStyle = {
  /** The preset the style was taken from, so the panel can name it. */
  preset: string | null;
  padding: number;
  corner: number;
  shadow: Shadow;
  background: Background;
  frame: WindowFrame;
};

export type AnnotationDocument = {
  version: Version;
  image: ImageSize;
  /** Image pixels per logical point when the capture was taken: 2 for a Retina display. */
  scale: number;
  /** The objects are listed in stacking order, the bottom one first. */
  objects: AnnotationObject[];
  /** The style the outputs put around the image, or `null` for none (format 1.2). */
  beautify: BeautifyStyle | null;
};

/** `parseDocument` throws this; `path` names the field that broke the rule. */
export class DocumentError extends Error {
  readonly path: string;

  constructor(path: string, message: string) {
    super(`${path}: ${message}`);
    this.name = "DocumentError";
    this.path = path;
  }
}

/** `parseDocument` throws this for a document that a newer Glimpsfy wrote. */
export class NewerVersionError extends Error {
  readonly found: Version;

  constructor(found: Version) {
    super(
      `The document is in format ${found.major}.${found.minor}, and this version reads format ${CURRENT_VERSION.major}.${CURRENT_VERSION.minor} and older.`,
    );
    this.name = "NewerVersionError";
    this.found = found;
  }
}

// ---------------------------------------------------------------------------- field readers

type Fields = Record<string, unknown>;

function record(value: unknown, path: string): Fields {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new DocumentError(path, "an object is expected");
  }
  return value as Fields;
}

function field(from: Fields, key: string, path: string): unknown {
  if (!(key in from)) throw new DocumentError(`${path}.${key}`, "the field is missing");
  return from[key];
}

function num(from: Fields, key: string, path: string): number {
  const value = field(from, key, path);
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new DocumentError(`${path}.${key}`, "a finite number is expected");
  }
  return value;
}

function length(from: Fields, key: string, path: string): number {
  const value = num(from, key, path);
  if (value < 0) throw new DocumentError(`${path}.${key}`, "the value is negative");
  return value;
}

function readLength(value: unknown, path: string): number {
  if (typeof value !== "number" || !Number.isFinite(value) || value < 0) {
    throw new DocumentError(path, "a finite, non-negative number is expected");
  }
  return value;
}

function count(from: Fields, key: string, path: string): number {
  const value = num(from, key, path);
  if (!Number.isInteger(value) || value < 0) throw new DocumentError(`${path}.${key}`, "a whole number is expected");
  return value;
}

function str(from: Fields, key: string, path: string): string {
  const value = field(from, key, path);
  if (typeof value !== "string") throw new DocumentError(`${path}.${key}`, "a string is expected");
  return value;
}

function oneOf<T extends string>(from: Fields, key: string, path: string, allowed: readonly T[]): T {
  const value = str(from, key, path);
  if (!(allowed as readonly string[]).includes(value)) {
    throw new DocumentError(`${path}.${key}`, `${JSON.stringify(value)} is not one of ${allowed.join(", ")}`);
  }
  return value as T;
}

/** A field that must be present and may be `null`. */
function nullable<T>(from: Fields, key: string, path: string, read: (value: unknown, path: string) => T): T | null {
  const value = field(from, key, path);
  return value === null ? null : read(value, `${path}.${key}`);
}

function readVersion(value: unknown, path: string): Version {
  const v = record(value, path);
  return { major: count(v, "major", path), minor: count(v, "minor", path) };
}

function readPoint(value: unknown, path: string): Point {
  const p = record(value, path);
  return { x: num(p, "x", path), y: num(p, "y", path) };
}

function readRect(value: unknown, path: string): Rect {
  const r = record(value, path);
  return { x: num(r, "x", path), y: num(r, "y", path), width: length(r, "width", path), height: length(r, "height", path) };
}

const HEX = /^#[0-9a-fA-F]{6}([0-9a-fA-F]{2})?$/;

function readColor(value: unknown, path: string): Color {
  const c = record(value, path);
  const kind = oneOf(c, "kind", path, ["ink", "hex"] as const);
  if (kind === "ink") return { kind, name: oneOf(c, "name", path, INKS) };
  const hex = str(c, "value", path);
  if (!HEX.test(hex)) throw new DocumentError(`${path}.value`, `${JSON.stringify(hex)} is not #rrggbb or #rrggbbaa`);
  return { kind, value: hex };
}

function readStroke(value: unknown, path: string): Stroke {
  const s = record(value, path);
  return { color: readColor(field(s, "color", path), `${path}.color`), width: length(s, "width", path) };
}

function bool(from: Fields, key: string, path: string): boolean {
  const value = field(from, key, path);
  if (typeof value !== "boolean") throw new DocumentError(`${path}.${key}`, "true or false is expected");
  return value;
}

function readFactor(from: Fields, key: string, path: string): MagnifyFactor {
  const value = num(from, key, path);
  if (!(MAGNIFY_FACTORS as readonly number[]).includes(value)) throw new DocumentError(`${path}.${key}`, `a magnifier enlarges 2, 3 or 4 times, not ${value}`);
  return value as MagnifyFactor;
}

function readPath(value: unknown, path: string): Point[] {
  if (!Array.isArray(value)) throw new DocumentError(path, "an array of points is expected");
  if (value.length === 0) throw new DocumentError(path, "the path has no points");
  return value.map((p, i) => readPoint(p, `${path}[${i}]`));
}

function readShape(o: Fields, path: string): Shape {
  const kind = oneOf(o, "kind", path, OBJECT_KINDS);
  const at = (key: string) => `${path}.${key}`;
  switch (kind) {
    case "arrow":
    case "line":
      return {
        kind,
        from: readPoint(field(o, "from", path), at("from")),
        to: readPoint(field(o, "to", path), at("to")),
        stroke: readStroke(field(o, "stroke", path), at("stroke")),
      };
    case "rectangle":
    case "ellipse":
      return {
        kind,
        rect: readRect(field(o, "rect", path), at("rect")),
        stroke: readStroke(field(o, "stroke", path), at("stroke")),
        fill: nullable(o, "fill", path, readColor),
      };
    case "pen":
      return {
        kind,
        points: readPath(field(o, "points", path), at("points")),
        stroke: readStroke(field(o, "stroke", path), at("stroke")),
      };
    case "highlighter":
      return {
        kind,
        points: readPath(field(o, "points", path), at("points")),
        color: readColor(field(o, "color", path), at("color")),
        width: length(o, "width", path),
      };
    case "text":
      return {
        kind,
        origin: readPoint(field(o, "origin", path), at("origin")),
        text: str(o, "text", path),
        fontSize: length(o, "fontSize", path),
        color: readColor(field(o, "color", path), at("color")),
        background: nullable(o, "background", path, readColor),
        maxWidth: nullable(o, "maxWidth", path, readLength),
      };
    case "step": {
      const number = count(o, "number", path);
      if (number < 1) throw new DocumentError(at("number"), "a step marker starts at 1");
      return {
        kind,
        center: readPoint(field(o, "center", path), at("center")),
        number,
        color: readColor(field(o, "color", path), at("color")),
        diameter: length(o, "diameter", path),
      };
    }
    case "magnify":
      return {
        kind,
        source: readPoint(field(o, "source", path), at("source")),
        radius: length(o, "radius", path),
        at: readPoint(field(o, "at", path), at("at")),
        factor: readFactor(o, "factor", path),
        smooth: bool(o, "smooth", path),
        color: readColor(field(o, "color", path), at("color")),
      };
    case "blur":
      return { kind, rect: readRect(field(o, "rect", path), at("rect")), strength: oneOf(o, "strength", path, BLUR_STRENGTHS) };
    case "pixelate":
      return { kind, rect: readRect(field(o, "rect", path), at("rect")), cell: oneOf(o, "cell", path, CELL_SIZES) };
    case "spotlight":
      return { kind, rect: readRect(field(o, "rect", path), at("rect")), dim: oneOf(o, "dim", path, SPOTLIGHT_DIMS) };
    case "crop":
      return { kind, rect: readRect(field(o, "rect", path), at("rect")) };
  }
}

const older = (a: Version, b: Version) => a.major < b.major || (a.major === b.major && a.minor < b.minor);

/**
 * Brings a document written by an older version up to `CURRENT_VERSION`, one step at a time, the
 * way `migrate` in crates/annotate does, and returns a current or newer-minor document as it is.
 * Each step only fills in what its version added, so nothing the older version wrote is lost.
 * The input is not changed.
 */
export function migrateDocument(raw: unknown): unknown {
  const d = record(raw, "document");
  const version = readVersion(field(d, "version", "document"), "document.version");
  if (version.major > CURRENT_VERSION.major) throw new NewerVersionError(version);
  if (!older(version, CURRENT_VERSION)) return raw;
  let objects = d.objects;
  // 1.0 → 1.1: the objects panel's hidden and locked flags.
  if (older(version, { major: 1, minor: 1 }) && Array.isArray(objects)) {
    objects = objects.map((o: unknown) => (typeof o === "object" && o !== null && !Array.isArray(o) ? { hidden: false, locked: false, ...o } : o));
  }
  // 1.1 → 1.2: the beautify style, which an older document has none of. 1.2 → 1.3: the magnifier,
  // a new kind no older document holds. Only the version changes in both steps.
  return { ...d, version: { ...CURRENT_VERSION }, objects };
}

function readHex(value: unknown, path: string): string {
  if (typeof value !== "string" || !HEX.test(value)) throw new DocumentError(path, `${JSON.stringify(value)} is not #rrggbb or #rrggbbaa`);
  return value;
}

function readStops(from: Fields, key: string, path: string): string[] {
  const list = field(from, key, path);
  if (!Array.isArray(list) || list.length === 0) throw new DocumentError(`${path}.${key}`, "a list of at least one colour is expected");
  return list.map((c, i) => readHex(c, `${path}.${key}[${i}]`));
}

function readBackground(value: unknown, path: string): Background {
  const b = record(value, path);
  const kind = oneOf(b, "kind", path, ["transparent", "solid", "gradient", "image", "match"] as const);
  switch (kind) {
    case "transparent":
    case "match":
      return { kind };
    case "solid":
      return { kind, color: readHex(field(b, "color", path), `${path}.color`) };
    case "gradient":
      return { kind, stops: readStops(b, "stops", path) };
    case "image":
      return { kind, path: str(b, "path", path), fallback: readStops(b, "fallback", path) };
  }
}

/** Reads a beautify style, as the document, the settings' presets and the overlay's frame hold it. */
export function readBeautify(value: unknown, path = "beautify"): BeautifyStyle {
  const b = record(value, path);
  const preset = "preset" in b && b.preset !== null && b.preset !== undefined ? str(b, "preset", path) : null;
  return {
    preset,
    padding: length(b, "padding", path),
    corner: length(b, "corner", path),
    shadow: oneOf(b, "shadow", path, SHADOWS),
    background: readBackground(field(b, "background", path), `${path}.background`),
    frame: oneOf(b, "frame", path, WINDOW_FRAMES),
  };
}

function writeBackground(b: Background): unknown {
  switch (b.kind) {
    case "transparent":
    case "match":
      return { kind: b.kind };
    case "solid":
      return { kind: b.kind, color: b.color };
    case "gradient":
      return { kind: b.kind, stops: [...b.stops] };
    case "image":
      return { kind: b.kind, path: b.path, fallback: [...b.fallback] };
  }
}

/** A beautify style as plain JSON data, the shape `capture_set_beautify` and the core read. */
export function writeBeautify(style: BeautifyStyle): unknown {
  return {
    ...(style.preset === null ? {} : { preset: style.preset }),
    padding: style.padding,
    corner: style.corner,
    shadow: style.shadow,
    background: writeBackground(style.background),
    frame: style.frame,
  };
}

function flag(from: Fields, key: string, path: string): boolean {
  if (!(key in from)) return false;
  const value = from[key];
  if (typeof value !== "boolean") throw new DocumentError(`${path}.${key}`, "true or false is expected");
  return value;
}

/**
 * Reads and validates a document from parsed JSON. The version is checked first, so a document
 * from a newer major version throws `NewerVersionError` even when its objects have a shape this
 * build does not know, and an older one goes through `migrateDocument`. Every other problem
 * throws `DocumentError` with the path of the field. Fields this build does not know are dropped,
 * which is what a newer minor version may add.
 */
export function parseDocument(input: unknown): AnnotationDocument {
  const raw = migrateDocument(input);
  const d = record(raw, "document");
  const version = readVersion(field(d, "version", "document"), "document.version");

  const imageFields = record(field(d, "image", "document"), "document.image");
  const image = { width: count(imageFields, "width", "document.image"), height: count(imageFields, "height", "document.image") };
  if (image.width === 0 || image.height === 0) throw new DocumentError("document.image", "the image has no pixels");

  const scale = num(d, "scale", "document");
  if (scale <= 0) throw new DocumentError("document.scale", "the scale is not positive");

  const list = field(d, "objects", "document");
  if (!Array.isArray(list)) throw new DocumentError("document.objects", "an array is expected");

  const ids = new Set<string>();
  let crops = 0;
  const objects = list.map((item, i): AnnotationObject => {
    const path = `document.objects[${i}]`;
    const o = record(item, path);
    const id = str(o, "id", path);
    if (id === "") throw new DocumentError(`${path}.id`, "the id is empty");
    if (ids.has(id)) throw new DocumentError(`${path}.id`, `the id ${JSON.stringify(id)} is used twice`);
    ids.add(id);
    const shape = readShape(o, path);
    if (shape.kind === "crop") crops += 1;
    return { id, hidden: flag(o, "hidden", path), locked: flag(o, "locked", path), ...shape };
  });
  if (crops > 1) throw new DocumentError("document.objects", `the document has ${crops} crop objects, and one is allowed`);

  const beautify = "beautify" in d && d.beautify !== null && d.beautify !== undefined ? readBeautify(d.beautify, "document.beautify") : null;
  return { version, image, scale, objects, beautify };
}

function writeColor(color: Color): unknown {
  return color.kind === "ink" ? { kind: "ink", name: color.name } : { kind: "hex", value: color.value };
}

function writeStroke(stroke: Stroke): unknown {
  return { color: writeColor(stroke.color), width: stroke.width };
}

const writePoint = (p: Point) => ({ x: p.x, y: p.y });
const writeRect = (r: Rect) => ({ x: r.x, y: r.y, width: r.width, height: r.height });

function writeObject(object: AnnotationObject): unknown {
  return { id: object.id, hidden: object.hidden, locked: object.locked, ...(writeShape(object) as object) };
}

function writeShape(object: AnnotationObject): unknown {
  switch (object.kind) {
    case "arrow":
    case "line":
      return { kind: object.kind, from: writePoint(object.from), to: writePoint(object.to), stroke: writeStroke(object.stroke) };
    case "rectangle":
    case "ellipse":
      return {
        kind: object.kind,
        rect: writeRect(object.rect),
        stroke: writeStroke(object.stroke),
        fill: object.fill === null ? null : writeColor(object.fill),
      };
    case "pen":
      return { kind: object.kind, points: object.points.map(writePoint), stroke: writeStroke(object.stroke) };
    case "highlighter":
      return { kind: object.kind, points: object.points.map(writePoint), color: writeColor(object.color), width: object.width };
    case "text":
      return {
        kind: object.kind,
        origin: writePoint(object.origin),
        text: object.text,
        fontSize: object.fontSize,
        color: writeColor(object.color),
        background: object.background === null ? null : writeColor(object.background),
        maxWidth: object.maxWidth,
      };
    case "step":
      return { kind: object.kind, center: writePoint(object.center), number: object.number, color: writeColor(object.color), diameter: object.diameter };
    case "magnify":
      return {
        kind: object.kind,
        source: writePoint(object.source),
        radius: object.radius,
        at: writePoint(object.at),
        factor: object.factor,
        smooth: object.smooth,
        color: writeColor(object.color),
      };
    case "blur":
      return { kind: object.kind, rect: writeRect(object.rect), strength: object.strength };
    case "pixelate":
      return { kind: object.kind, rect: writeRect(object.rect), cell: object.cell };
    case "spotlight":
      return { kind: object.kind, rect: writeRect(object.rect), dim: object.dim };
    case "crop":
      return { kind: object.kind, rect: writeRect(object.rect) };
  }
}

/**
 * Turns a document into plain JSON data, field by field, so state the UI keeps on its objects
 * (selection, hover) never leaks into a saved document. `JSON.stringify` of the result is what
 * the core reads.
 */
export function serializeDocument(doc: AnnotationDocument): unknown {
  return {
    version: { major: doc.version.major, minor: doc.version.minor },
    image: { width: doc.image.width, height: doc.image.height },
    scale: doc.scale,
    objects: doc.objects.map(writeObject),
    ...(doc.beautify === null ? {} : { beautify: writeBeautify(doc.beautify) }),
  };
}
