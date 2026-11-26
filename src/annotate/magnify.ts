// The magnifier (E44): where its ring may sit, where a new loupe opens, which part of it a press
// lands on, and the enlarged pixels its loupe shows. Everything is in image pixels.
//
// The loupe's pixels are computed here and nowhere else, from the image with every redaction
// flattened in (redact.ts), so a blurred or pixelated area stays hidden in the loupe as well. The
// renderer (compose.ts) only adds the drawings below the magnifier to those pixels and copies the
// result one to one into the canvas, which never resamples them.

import { MAGNIFY_FACTORS, type AnnotationObject, type MagnifyFactor, type Point, type Rect } from "../lib/annotate";
import { flattenRedactions, intersect, pixelBounds, type Bytes, type PatchCache, type PixelRect, type Pixels } from "./redact";
import { MAGNIFY_FRAME, MAGNIFY_GAP, MAGNIFY_MARGIN, MAGNIFY_MIN_RADIUS, MAGNIFY_RING } from "./style";

export type Magnify = Extract<AnnotationObject, { kind: "magnify" }>;

/** The part of a magnifier a press lands on: the loupe, the ring with its source, or the line. */
export type MagnifyPart = "loupe" | "source" | "link";

const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

export const loupeRadius = (o: Pick<Magnify, "radius" | "factor">): number => o.radius * o.factor;

/** The square around the ring, where its handles sit. */
export function sourceBox(o: Pick<Magnify, "source" | "radius">): Rect {
  return { x: o.source.x - o.radius, y: o.source.y - o.radius, width: 2 * o.radius, height: 2 * o.radius };
}

export function loupeBox(o: Magnify): Rect {
  const r = loupeRadius(o);
  return { x: o.at.x - r, y: o.at.y - r, width: 2 * r, height: 2 * r };
}

/** A centre between `lo` and `hi` that keeps `half` free on both sides; the middle when the span
 * is too short for that. */
function within(v: number, lo: number, hi: number, half: number): number {
  return lo + half > hi - half ? (lo + hi) / 2 : clamp(v, lo + half, hi - half);
}

/**
 * The ring around `center` with `radius` as it is drawn or resized: at least 8 points across and
 * inside the `width` × `height` image. The centre stays where it is, so a ring that reaches an
 * edge stops growing there; only a centre too close to an edge for the smallest ring moves in.
 */
export function sizeRing(center: Point, radius: number, width: number, height: number, scale: number): { source: Point; radius: number } {
  const min = Math.min(MAGNIFY_MIN_RADIUS * scale, width / 2, height / 2);
  const source = { x: within(center.x, 0, width, min), y: within(center.y, 0, height, min) };
  const room = Math.min(source.x, source.y, width - source.x, height - source.y);
  return { source, radius: clamp(radius, min, Math.max(min, room)) };
}

/** The magnifier after a move: its ring is pushed back inside the image, whose pixels are all the
 * loupe can show, and keeps its size. */
export function keepRing(o: Magnify, width: number, height: number): Magnify {
  const radius = Math.min(o.radius, width / 2, height / 2);
  const source = { x: within(o.source.x, 0, width, radius), y: within(o.source.y, 0, height, radius) };
  return source.x === o.source.x && source.y === o.source.y && radius === o.radius ? o : { ...o, source, radius };
}

/** A loupe dragged to `at` stays inside `area` along each side it fits in, its frame included. */
export function keepLoupe(o: Magnify, at: Point, area: Rect, scale: number): Point {
  const half = loupeRadius(o) + (MAGNIFY_FRAME / 2) * scale;
  const axis = (v: number, lo: number, size: number) => (2 * half <= size ? clamp(v, lo + half, lo + size - half) : v);
  return { x: axis(at.x, area.x, area.width), y: axis(at.y, area.y, area.height) };
}

/** The eight directions a loupe may open in, the sides before the corners. */
const DIRECTIONS: [number, number][] = [
  [1, 0],
  [0, 1],
  [-1, 0],
  [0, -1],
  [Math.SQRT1_2, Math.SQRT1_2],
  [-Math.SQRT1_2, Math.SQRT1_2],
  [-Math.SQRT1_2, -Math.SQRT1_2],
  [Math.SQRT1_2, -Math.SQRT1_2],
];

/** How far `area` reaches from `p` in the direction `u`. */
function reach(p: Point, [ux, uy]: [number, number], area: Rect): number {
  const tx = ux > 0 ? (area.x + area.width - p.x) / ux : ux < 0 ? (area.x - p.x) / ux : Infinity;
  const ty = uy > 0 ? (area.y + area.height - p.y) / uy : uy < 0 ? (area.y - p.y) / uy : Infinity;
  return Math.max(0, Math.min(tx, ty));
}

/**
 * Where the loupe of a ring opens when the ring is let go: on the side of the ring with the most
 * room in `area` (the exported area), clear of the ring and inside the area with its shadow. When
 * the loupe does not fit at `want`, the factor drops to the largest that fits; when not even 2×
 * fits, the loupe takes the place farthest from the ring that keeps it inside the area.
 */
export function placeLoupe(source: Point, radius: number, want: MagnifyFactor, area: Rect, scale: number): { at: Point; factor: MagnifyFactor } {
  const margin = MAGNIFY_MARGIN * scale;
  const gap = MAGNIFY_GAP * scale;
  let fallback: { at: Point; distance: number } | null = null;
  for (const factor of [...MAGNIFY_FACTORS].reverse()) {
    if (factor > want) continue;
    const big = radius * factor;
    const half = big + margin;
    let best: { at: Point; room: number } | null = null;
    for (const u of DIRECTIONS) {
      const far = radius + gap + big;
      const at = { x: within(source.x + u[0] * far, area.x, area.x + area.width, half), y: within(source.y + u[1] * far, area.y, area.y + area.height, half) };
      const distance = Math.hypot(at.x - source.x, at.y - source.y);
      const inside = at.x - half >= area.x - 0.5 && at.x + half <= area.x + area.width + 0.5 && at.y - half >= area.y - 0.5 && at.y + half <= area.y + area.height + 0.5;
      const room = reach(source, u, area) - radius;
      if (inside && distance >= radius + big + gap / 2) {
        if (!best || room > best.room) best = { at, room };
      } else if (factor === MAGNIFY_FACTORS[0] && (!fallback || distance > fallback.distance)) {
        fallback = { at, distance };
      }
    }
    if (best) return { at: best.at, factor };
  }
  return { at: fallback?.at ?? source, factor: MAGNIFY_FACTORS[0] };
}

/** The line between the ring and the loupe, from edge to edge of their strokes; `null` while the
 * circles touch. */
export function linkEnds(o: Magnify, scale: number): [Point, Point] | null {
  const dx = o.at.x - o.source.x;
  const dy = o.at.y - o.source.y;
  const d = Math.hypot(dx, dy);
  const from = o.radius + (MAGNIFY_RING / 2) * scale;
  const to = loupeRadius(o) + (MAGNIFY_FRAME / 2) * scale;
  if (d <= from + to) return null;
  const ux = dx / d;
  const uy = dy / d;
  return [
    { x: o.source.x + ux * from, y: o.source.y + uy * from },
    { x: o.at.x - ux * to, y: o.at.y - uy * to },
  ];
}

function distanceToSegment(p: Point, a: Point, b: Point): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const length = dx * dx + dy * dy;
  const t = length === 0 ? 0 : clamp(((p.x - a.x) * dx + (p.y - a.y) * dy) / length, 0, 1);
  return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy));
}

/**
 * The part of a magnifier under `p`, the loupe first because it lies on top. The loupe is filled,
 * so all of it counts; the ring counts at its stroke and, with `interior`, inside it too, the way
 * an unfilled shape does. `reach` is the pointer's tolerance in image pixels.
 */
export function magnifyPart(o: Magnify, p: Point, reach: number, interior: boolean): MagnifyPart | null {
  if (Math.hypot(p.x - o.at.x, p.y - o.at.y) <= loupeRadius(o) + reach / 2) return "loupe";
  const d = Math.hypot(p.x - o.source.x, p.y - o.source.y);
  if (interior ? d <= o.radius + reach : Math.abs(d - o.radius) <= reach) return "source";
  const link = linkEnds(o, 0);
  if (link && distanceToSegment(p, link[0], link[1]) <= reach) return "link";
  return null;
}

// ------------------------------------------------------------------------------ the pixels

/** Whether a factor goes past the capture's own pixels, where Sharp and Smooth differ: every factor
 * on a 1x capture, 3× and 4× on a 2x one. */
export const beyondPixels = (factor: number, scale: number): boolean => factor > scale + 1e-6;

/** Whether the loupe samples bilinearly; below the capture's pixels it always shows blocks. */
export const smoothAt = (o: Magnify, scale: number): boolean => o.smooth && beyondPixels(o.factor, scale);

/** The whole pixels the ring covers, clipped to the image. */
export function sourceRegion(o: Magnify, width: number, height: number): PixelRect | null {
  return pixelBounds(sourceBox(o), width, height);
}

/**
 * Enlarges `width` × `height` RGBA pixels `factor` times and returns the part `window` of the
 * result (in its pixels). Sharp repeats every pixel as a block (nearest neighbour); smooth reads
 * each output pixel's centre between the four nearest pixels (bilinear), with the edge pixels
 * repeated.
 */
export function upscale(data: Bytes, width: number, height: number, factor: number, smooth: boolean, window: PixelRect = { x: 0, y: 0, width: width * factor, height: height * factor }): Bytes {
  const { x: wx, y: wy, width: w, height: h } = window;
  const out = new Uint8ClampedArray(w * h * 4);
  if (!smooth) {
    for (let y = 0; y < h; y++) {
      const row = Math.floor((wy + y) / factor) * width;
      const o = y * w * 4;
      for (let x = 0; x < w; x++) {
        const s = (row + Math.floor((wx + x) / factor)) * 4;
        const t = o + x * 4;
        out[t] = data[s];
        out[t + 1] = data[s + 1];
        out[t + 2] = data[s + 2];
        out[t + 3] = data[s + 3];
      }
    }
    return out;
  }
  const axis = (start: number, count: number, size: number) =>
    Array.from({ length: count }, (_, i) => {
      const f = (start + i + 0.5) / factor - 0.5;
      const i0 = clamp(Math.floor(f), 0, size - 1);
      return { i0, i1: Math.min(size - 1, i0 + 1), t: clamp(f - i0, 0, 1) };
    });
  const xs = axis(wx, w, width);
  const ys = axis(wy, h, height);
  for (let y = 0; y < h; y++) {
    const { i0: r0, i1: r1, t: ty } = ys[y];
    for (let x = 0; x < w; x++) {
      const { i0: c0, i1: c1, t: tx } = xs[x];
      const a = (r0 * width + c0) * 4;
      const b = (r0 * width + c1) * 4;
      const c = (r1 * width + c0) * 4;
      const d = (r1 * width + c1) * 4;
      const t = (y * w + x) * 4;
      for (let k = 0; k < 4; k++) {
        const top = data[a + k] * (1 - tx) + data[b + k] * tx;
        const bottom = data[c + k] * (1 - tx) + data[d + k] * tx;
        out[t + k] = Math.round(top * (1 - ty) + bottom * ty);
      }
    }
  }
  return out;
}

/** The loupe's pixels at their place in the image: `x`, `y` is the top-left pixel. */
export type LoupePatch = { x: number; y: number; width: number; height: number; data: Bytes };

/**
 * The loupe's pixels from `base`, the composed pixels of `region` (the ring's pixels): enlarged so
 * that the ring's centre lands on the loupe's, on whole pixels, and cut to `clip` when one is given
 * (the part of the image a canvas holds). `null` when nothing of the loupe is inside `clip`.
 */
export function loupePatch(base: Bytes, region: PixelRect, o: Magnify, scale: number, clip?: PixelRect): LoupePatch | null {
  const f = o.factor;
  const whole = {
    x: Math.round(o.at.x + (region.x - o.source.x) * f),
    y: Math.round(o.at.y + (region.y - o.source.y) * f),
    width: region.width * f,
    height: region.height * f,
  };
  const part = clip ? intersect(whole, clip) : whole;
  if (!part) return null;
  const window = { x: part.x - whole.x, y: part.y - whole.y, width: part.width, height: part.height };
  return { ...part, data: upscale(base, region.width, region.height, f, smoothAt(o, scale), window) };
}

/**
 * The loupe's pixels when no drawing lies below the magnifier: the ring's pixels with every
 * visible blur and pixelate of `objects` flattened in, enlarged. The original pixels of a redacted
 * area never reach the loupe.
 */
export function loupePixels(src: Pixels, objects: readonly AnnotationObject[], scale: number, o: Magnify, cache?: PatchCache, clip?: PixelRect): LoupePatch | null {
  const region = sourceRegion(o, src.width, src.height);
  if (!region) return null;
  return loupePatch(flattenRedactions(src, objects, scale, region, cache), region, o, scale, clip);
}
