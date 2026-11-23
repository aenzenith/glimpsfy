// Colour maths for the colour picker, the capture loupe and the editor's readout: the formats a
// colour is copied in, Display P3 to sRGB with OKLCH gamut mapping, and two contrast measures
// (WCAG 2 and APCA 0.0.98G-4g). Pure functions; the formulas are the ones of the design canvas's
// row 29 (design/canvas/index.html).

export type Space = "srgb" | "display-p3";
/** Bytes, 0 to 255. */
export type Rgb = [number, number, number];
/** Components, 0 to 1. */
export type Vec3 = [number, number, number];
export type Format = "hex" | "rgb" | "hsl" | "p3";

/** One frame pixel: its bytes in the frame's own colour space. */
export type Sample = { space: Space; bytes: Rgb };

/** What the picker writes for a pixel. HEX, RGB and HSL are sRGB; a P3 colour outside sRGB is
 * mapped into it (`out`), and the screen writes it with ≈ in front. */
export type Reading = {
  sample: Sample;
  srgb: Rgb;
  out: boolean;
  /** The colour in Display P3, 0 to 1. */
  p3: Vec3;
  /** A CSS colour that paints the pixel as the frame holds it. */
  css: string;
};

const clamp01 = (v: number) => Math.min(1, Math.max(0, v));

/** The sRGB transfer curve, which Display P3 shares. */
const decode = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
const encode = (c: number) => (c <= 0.0031308 ? 12.92 * c : 1.055 * c ** (1 / 2.4) - 0.055);

const mul = (m: number[][], v: Vec3): Vec3 => [
  m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
  m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
  m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
];

/** Linear Display P3 to linear sRGB, and back. */
const P3_TO_SRGB = [
  [1.2249401, -0.2249404, 0],
  [-0.0420569, 1.0420571, 0],
  [-0.0196376, -0.0786361, 1.0982735],
];
const SRGB_TO_P3 = [
  [0.8224621, 0.177538, 0],
  [0.0331941, 0.9668058, 0],
  [0.0170827, 0.0723974, 0.9105199],
];

function oklab([r, g, b]: Vec3): Vec3 {
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  return [
    0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s,
    1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s,
    0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s,
  ];
}

function linearOf([L, a, b]: Vec3): Vec3 {
  const l = (L + 0.3963377774 * a + 0.2158037573 * b) ** 3;
  const m = (L - 0.1055613458 * a - 0.0638541728 * b) ** 3;
  const s = (L - 0.0894841775 * a - 1.291485548 * b) ** 3;
  return [
    4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
    -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
    -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
  ];
}

/** Half a byte: a component this close past 0 or 1 still writes as 0 or 255. */
const HALF_BYTE = 0.5 / 255;
/** Inside sRGB as bytes show it: every component encodes to a byte from 0 to 255. A frame's 8 bits
 * put sRGB's own primaries a little past the edge once they come back from P3. */
const insideSrgb = (v: Vec3) =>
  v.every((c) => {
    const e = encode(c);
    return e >= -HALF_BYTE && e <= 1 + HALF_BYTE;
  });
const clip = (v: Vec3): Vec3 => v.map(clamp01) as Vec3;
const toBytes = (v: Vec3): Rgb => v.map((c) => Math.round(clamp01(encode(c)) * 255)) as Rgb;

/** CSS Color 4's just noticeable difference in OKLab, and the precision of its chroma search. */
const JND = 0.02;
const EPSILON = 0.0001;
const deltaEOK = (x: Vec3, y: Vec3) => Math.hypot(x[0] - y[0], x[1] - y[1], x[2] - y[2]);

/** A Display P3 colour (0 to 1) in sRGB bytes, gamut mapped as CSS Color 4 does it. A colour whose
 * clipped sRGB looks the same (within a JND) is an sRGB colour that 8-bit rounding pushed over the
 * edge, and comes back clipped without `out`. Any other colour outside sRGB keeps its OKLCH
 * lightness and hue and loses chroma until its clipped form is within a JND, and comes back with
 * `out`, which the screen writes as ≈. */
export function p3ToSrgb(p3: Vec3): { rgb: Rgb; out: boolean } {
  const linear = mul(P3_TO_SRGB, p3.map(decode) as Vec3);
  if (insideSrgb(linear)) return { rgb: toBytes(linear), out: false };
  const origin = oklab(linear);
  if (deltaEOK(oklab(clip(linear)), origin) < JND) return { rgb: toBytes(linear), out: false };
  const [L, a, b] = origin;
  if (L >= 1) return { rgb: [255, 255, 255], out: true };
  if (L <= 0) return { rgb: [0, 0, 0], out: true };
  const hue = Math.atan2(b, a);
  const at = (chroma: number): Vec3 => [L, chroma * Math.cos(hue), chroma * Math.sin(hue)];
  let lo = 0;
  let hi = Math.hypot(a, b);
  let loInside = true;
  let clipped = clip(linear);
  while (hi - lo > EPSILON) {
    const mid = (lo + hi) / 2;
    const current = linearOf(at(mid));
    if (loInside && insideSrgb(current)) {
      lo = mid;
      continue;
    }
    clipped = clip(current);
    const e = deltaEOK(oklab(clipped), at(mid));
    if (e < JND) {
      if (JND - e < EPSILON) break;
      loInside = false;
      lo = mid;
    } else {
      hi = mid;
    }
  }
  return { rgb: toBytes(clipped), out: true };
}

/** sRGB bytes in Display P3 (0 to 1); every sRGB colour fits. */
export const srgbToP3 = (rgb: Rgb): Vec3 => mul(SRGB_TO_P3, rgb.map((n) => decode(n / 255)) as Vec3).map((c) => clamp01(encode(c))) as Vec3;

/** What the picker writes for one frame pixel. */
export function read(sample: Sample): Reading {
  const { space, bytes } = sample;
  if (space === "display-p3") {
    const p3 = bytes.map((n) => n / 255) as Vec3;
    const { rgb, out } = p3ToSrgb(p3);
    return { sample, srgb: rgb, out, p3, css: p3Css(p3) };
  }
  return { sample, srgb: bytes, out: false, p3: srgbToP3(bytes), css: hex(bytes) };
}

// ---------------------------------------------------------------------------------- formats

const hex2 = (n: number) => n.toString(16).padStart(2, "0");

export const hex = ([r, g, b]: Rgb): string => `#${hex2(r)}${hex2(g)}${hex2(b)}`.toUpperCase();

export const rgbText = ([r, g, b]: Rgb): string => `rgb(${r} ${g} ${b})`;

export function hsl([r8, g8, b8]: Rgb): [number, number, number] {
  const r = r8 / 255;
  const g = g8 / 255;
  const b = b8 / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;
  let h = 0;
  let s = 0;
  if (d) {
    s = d / (1 - Math.abs(2 * l - 1));
    h = max === r ? ((g - b) / d) % 6 : max === g ? (b - r) / d + 2 : (r - g) / d + 4;
    h = (h * 60 + 360) % 360;
  }
  return [Math.round(h) % 360, Math.round(s * 100), Math.round(l * 100)];
}

export const hslText = (rgb: Rgb): string => {
  const [h, s, l] = hsl(rgb);
  return `hsl(${h} ${s}% ${l}%)`;
};

/** A component with three decimals at most and no trailing zeros, as `color(display-p3 …)` takes it. */
const p3Number = (v: number) => String(Number(clamp01(v).toFixed(3)));

export const p3Css = (p3: Vec3): string => `color(display-p3 ${p3.map(p3Number).join(" ")})`;

/** The text a copy in `format` puts on the clipboard; ≈ never goes in. */
export function formatted(reading: Reading, format: Format): string {
  switch (format) {
    case "hex":
      return hex(reading.srgb);
    case "rgb":
      return rgbText(reading.srgb);
    case "hsl":
      return hslText(reading.srgb);
    case "p3":
      return p3Css(reading.p3);
  }
}

export const FORMAT_LABEL: Record<Format, string> = { hex: "HEX", rgb: "RGB", hsl: "HSL", p3: "P3" };

/** The formats the picker offers for a frame: P3 only where the frame is Display P3. */
export const formatsFor = (space: Space): Format[] => (space === "display-p3" ? ["hex", "rgb", "hsl", "p3"] : ["hex", "rgb", "hsl"]);

/** A colour the picker wrote (`#RRGGBB` or `color(display-p3 r g b)`) as a sample, for the recent
 * colours; `null` for anything else. */
export function parseSwatch(css: string): Sample | null {
  const h = /^#([0-9a-f]{6})$/i.exec(css);
  if (h) {
    const n = parseInt(h[1], 16);
    return { space: "srgb", bytes: [(n >> 16) & 255, (n >> 8) & 255, n & 255] };
  }
  const p = /^color\(display-p3 ([\d.]+) ([\d.]+) ([\d.]+)\)$/.exec(css);
  if (p) return { space: "display-p3", bytes: [p[1], p[2], p[3]].map((v) => Math.round(clamp01(Number(v)) * 255)) as Rgb };
  return null;
}

// --------------------------------------------------------------------------------- contrast

/** The Y row of each space's matrix to CIE XYZ (D65). */
const Y_ROW: Record<Space, Vec3> = {
  srgb: [0.2126729, 0.7151522, 0.072175],
  "display-p3": [0.2289746, 0.6917385, 0.0792869],
};

/** WCAG 2 relative luminance, from the pixel in its own space. */
export function luminance({ space, bytes }: Sample): number {
  const [r, g, b] = Y_ROW[space];
  return r * decode(bytes[0] / 255) + g * decode(bytes[1] / 255) + b * decode(bytes[2] / 255);
}

/** The WCAG 2 contrast ratio, 1 to 21, unrounded: a threshold reads this, never the shown value. */
export function wcagRatio(a: Sample, b: Sample): number {
  const x = luminance(a);
  const y = luminance(b);
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

/** The WCAG 2.2 verdicts: AA and AAA for normal text, and for large text (18 pt, or 14 pt bold). */
export function wcagPasses(ratio: number): { normalAA: boolean; normalAAA: boolean; largeAA: boolean; largeAAA: boolean } {
  return { normalAA: ratio >= 4.5, normalAAA: ratio >= 7, largeAA: ratio >= 3, largeAAA: ratio >= 4.5 };
}

/** APCA's coefficients: sRGB (`sRGBtoY`) and Display P3 (`displayP3toY`), with its simple 2.4 curve. */
const APCA_ROW: Record<Space, Vec3> = {
  srgb: [0.2126729, 0.7151522, 0.072175],
  "display-p3": [0.228982959480578, 0.691749262585238, 0.0792677779341829],
};

/** APCA's screen luminance of a pixel, before its soft clamp. */
export function apcaY({ space, bytes }: Sample): number {
  const [r, g, b] = APCA_ROW[space];
  return r * (bytes[0] / 255) ** 2.4 + g * (bytes[1] / 255) ** 2.4 + b * (bytes[2] / 255) ** 2.4;
}

/** APCA 0.0.98G-4g's lightness contrast Lc of `text` on `ground`: positive for dark text on a
 * light ground, negative for light text on a dark one, 0 below its low clip. */
export function apcaLc(text: Sample, ground: Sample): number {
  const clampBlack = (y: number) => (y > 0.022 ? y : y + (0.022 - y) ** 1.414);
  const t = clampBlack(apcaY(text));
  const b = clampBlack(apcaY(ground));
  if (Math.abs(b - t) < 0.0005) return 0;
  if (b > t) {
    const s = (b ** 0.56 - t ** 0.57) * 1.14;
    return s < 0.1 ? 0 : (s - 0.027) * 100;
  }
  const s = (b ** 0.65 - t ** 0.62) * 1.14;
  return s > -0.1 ? 0 : (s + 0.027) * 100;
}
