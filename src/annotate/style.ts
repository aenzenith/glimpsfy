// The look of every annotation in numbers: the ink values, the sizes the options strip offers,
// the arrowhead, the text plate, the redaction strengths and the spotlight dims. Sizes are in
// logical points, as the strip shows them; the document stores image pixels, so every size is
// multiplied by the document's `scale` (image pixels per point) before it is stored or drawn.
//
// The renderer reads only this file for its numbers, so the preview and the export, which are the
// same renderer, can never disagree about a size.

import type { BlurStrength, CellSize, Color, Ink, SpotlightDim } from "../lib/annotate";

/** The kit's `--ink-*` values (src/styles/tokens.css). A page with the kit loaded reads them from
 * CSS, so a retuned token reaches the drawings; these are the values when no stylesheet is there
 * (tests, an export started before the styles load). */
const INK_FALLBACK: Record<Ink, string> = {
  red: "#ff3b30",
  orange: "#ff9500",
  yellow: "#ffd60a",
  green: "#34c759",
  cyan: "#32ade6",
  blue: "#0a84ff",
  white: "#ffffff",
  black: "#1c1c1e",
};

let inkValues: Record<Ink, string> | null = null;

/** The sRGB hex value of an ink. */
export function inkHex(ink: Ink): string {
  if (!inkValues) {
    const values = { ...INK_FALLBACK };
    if (typeof document !== "undefined" && typeof getComputedStyle === "function") {
      const style = getComputedStyle(document.documentElement);
      for (const name of Object.keys(values) as Ink[]) {
        const value = style.getPropertyValue(`--ink-${name}`).trim();
        if (/^#[0-9a-f]{6}$/i.test(value)) values[name] = value;
      }
    }
    inkValues = values;
  }
  return inkValues[ink];
}

/** A CSS colour for the canvas. Inks and hex colours are sRGB; a canvas in Display P3 converts
 * them, so an ink looks the same on every frame. */
export function cssOf(color: Color): string {
  return color.kind === "ink" ? inkHex(color.name) : color.value;
}

export const ink = (name: Ink): Color => ({ kind: "ink", name });

/** The ink that reads on top of `color`: black on the light inks, white on the others. */
export function contrastInk(color: Color): Ink {
  const hex = cssOf(color).slice(1, 7);
  const [r, g, b] = [0, 2, 4].map((i) => parseInt(hex.slice(i, i + 2), 16) / 255);
  const lin = (c: number) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4);
  const luminance = 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
  return luminance > 0.45 ? "black" : "white";
}

// ------------------------------------------------------------------- the options strip

/** The inks in strip order. */
export const INKS: readonly Ink[] = ["red", "orange", "yellow", "green", "cyan", "blue", "white", "black"];
/** The highlighter's inks: the ones a multiply leaves readable text under. */
export const HIGHLIGHTER_INKS: readonly Ink[] = ["yellow", "green", "cyan", "orange", "red", "blue"];

/** Stroke widths of the arrow, line, rectangle, ellipse and pen, in points. */
export const STROKE_WIDTHS = [2, 4, 6] as const;
/** The highlighter's strip shows these, and its stroke is 2.2 times as wide. */
export const HIGHLIGHTER_WIDTHS = [4, 7, 10] as const;
export const HIGHLIGHTER_FACTOR = 2.2;
export const HIGHLIGHTER_ALPHA = 0.55;
/** The step marker's strip dots; the disc is 18 points wider than its dot. */
export const STEP_SIZES = [8, 11, 14] as const;
export const STEP_BASE = 18;
/** Text sizes in points (11, 14 and 18 pt in the strip). */
export const TEXT_SIZES = [11, 14, 18] as const;

/** The three choices of the blur, pixelate and spotlight strips, in strip order. */
export const BLUR_LEVELS: readonly BlurStrength[] = ["soft", "medium", "strong"];
export const CELL_LEVELS: readonly CellSize[] = ["fine", "medium", "coarse"];
export const DIM_LEVELS: readonly SpotlightDim[] = ["light", "medium", "dark"];

/** The crop strip's aspect ratios; `original` keeps the image's own ratio (editor only). */
export const CROP_RATIOS = ["free", "original", "16:9", "4:3", "1:1"] as const;
export type CropRatio = (typeof CROP_RATIOS)[number];

// ------------------------------------------------------------------------- redaction

/**
 * The blur's block size in points. The blur keeps nothing but the mean of each block and draws a
 * smooth surface through those means, so detail smaller than a block is gone from the output, not
 * hidden in it. Soft is still larger than the x-height of 13-point interface text.
 */
export const BLUR_BLOCK: Record<BlurStrength, number> = { soft: 6, medium: 10, strong: 16 };
/** The pixelate cell in points. Fine is still larger than the x-height of 13-point text, so a
 * letter never keeps more than one or two cells of its own. */
export const PIXELATE_CELL: Record<CellSize, number> = { fine: 10, medium: 14, coarse: 20 };
/** The smallest block or cell in image pixels, whatever the scale. */
export const MIN_BLOCK = 4;

/** The spotlight's dim over everything outside its regions: the overlay's own `--dim`, weakened
 * for the lighter choices (the prototype's 60, 80 and 100 %). */
export const DIM_RGB = [8, 8, 12] as const;
export const DIM_ALPHA: Record<SpotlightDim, number> = { light: 0.48 * 0.6, medium: 0.48 * 0.8, dark: 0.48 };
/** The corner radius of a spotlight region (`--radius-key`). */
export const SPOTLIGHT_RADIUS = 5;

// -------------------------------------------------------------------------- drawing

/** The rectangle's corner radius. */
export const RECT_RADIUS = 4;
/** The 1 point shadow that keeps a drawing readable on any content (DESIGN.md, Overlay). */
export const SHADOW = { color: "rgba(0, 0, 0, 0.30)", blur: 1.5, y: 1 } as const;
/** The step marker's and the text plate's stronger shadow (`--shadow-handle`). */
export const SHADOW_STRONG = { color: "rgba(0, 0, 0, 0.45)", blur: 3, y: 1 } as const;
/** The white ring around a step marker. */
export const STEP_RING = 2;

/** The annotation font (`--type-annotation`). */
export const FONT_FAMILY = '-apple-system, BlinkMacSystemFont, "SF Pro Text", system-ui, "Segoe UI", sans-serif';
export const FONT_WEIGHT = 600;
/** Line height and plate padding as fractions of the font size (18/14, and 5, 10 and 6 points
 * around 14-point text in the prototype). */
export const LINE_HEIGHT = 18 / 14;
export const TEXT_PAD = { x: 10 / 14, top: 5 / 14, bottom: 6 / 14 } as const;
export const TEXT_RADIUS = 7 / 14;

export function textFont(size: number): string {
  return `${FONT_WEIGHT} ${size}px ${FONT_FAMILY}`;
}

/** The arrow's body width for a stroke width, and its head (the prototype's `arrowD`). */
export function arrowMetrics(width: number, length: number): { body: number; headLength: number; headHalf: number } {
  const body = width * 1.1 + 1;
  return { body, headLength: Math.min(body * 3.6, length * 0.6), headHalf: body * 1.9 };
}

/** The index of the strip choice closest to `value` (a stored size divided by the scale). */
export function nearestIndex(choices: readonly number[], value: number): number {
  let best = 0;
  choices.forEach((choice, i) => {
    if (Math.abs(choice - value) < Math.abs(choices[best] - value)) best = i;
  });
  return best;
}
