// Blur and pixelate as pure functions over RGBA pixel arrays. The preview and the export both
// take their redacted pixels from here, and the export writes them over the original with
// `putImageData`, so the pixels under a redaction are replaced, never blended with what was
// there.
//
// Both effects keep only the mean of each block of the region and throw the rest away:
// pixelate paints every block in its mean, and blur draws a smooth surface through the block
// means. The output inside a region is therefore a function of the block means alone, and no
// detail smaller than a block (a letter, a digit, a stroke) survives in it in any form. The
// blocks are aligned to the region's own corner and every mean is taken from the original
// image, never from another redaction, so an overlapping pair still reads only the original.

import type { AnnotationObject, Rect } from "../lib/annotate";
import { BLUR_BLOCK, MIN_BLOCK, PIXELATE_CELL } from "./style";

/** Bytes backed by an ordinary buffer, which `ImageData` requires. */
export type Bytes = Uint8ClampedArray<ArrayBuffer>;
/** RGBA pixels in tight rows. */
export type Pixels = { width: number; height: number; data: Bytes };
/** A rectangle of whole pixels. */
export type PixelRect = { x: number; y: number; width: number; height: number };

export type Redaction = Extract<AnnotationObject, { kind: "blur" | "pixelate" }>;

export const isRedaction = (object: AnnotationObject): object is Redaction => object.kind === "blur" || object.kind === "pixelate";

/** Every pixel a rectangle touches, even partly, clipped to a `width` × `height` image; `null`
 * when nothing is left. Rounding outward means a pixel half under a redaction is redacted too. */
export function pixelBounds(rect: Rect, width: number, height: number): PixelRect | null {
  const x0 = Math.max(0, Math.floor(rect.x));
  const y0 = Math.max(0, Math.floor(rect.y));
  const x1 = Math.min(width, Math.ceil(rect.x + rect.width));
  const y1 = Math.min(height, Math.ceil(rect.y + rect.height));
  return x1 > x0 && y1 > y0 ? { x: x0, y: y0, width: x1 - x0, height: y1 - y0 } : null;
}

export function intersect(a: PixelRect, b: PixelRect): PixelRect | null {
  const x0 = Math.max(a.x, b.x);
  const y0 = Math.max(a.y, b.y);
  const x1 = Math.min(a.x + a.width, b.x + b.width);
  const y1 = Math.min(a.y + a.height, b.y + b.height);
  return x1 > x0 && y1 > y0 ? { x: x0, y: y0, width: x1 - x0, height: y1 - y0 } : null;
}

/** The size of a redaction's blocks in image pixels for a document at `scale`. */
export function blockSize(object: Redaction, scale: number): number {
  const points = object.kind === "blur" ? BLUR_BLOCK[object.strength] : PIXELATE_CELL[object.cell];
  return Math.max(MIN_BLOCK, Math.round(points * scale));
}

type Means = { cols: number; rows: number; values: Float64Array };

/** The mean of every `cell` × `cell` block of `region`, per channel, with the blocks aligned to
 * the region's top-left corner; the last row and column of blocks may be narrower. */
function blockMeans(src: Pixels, region: PixelRect, cell: number): Means {
  const cols = Math.ceil(region.width / cell);
  const rows = Math.ceil(region.height / cell);
  const sums = new Float64Array(cols * rows * 4);
  const counts = new Float64Array(cols * rows);
  for (let y = 0; y < region.height; y++) {
    const by = Math.floor(y / cell);
    let at = ((region.y + y) * src.width + region.x) * 4;
    for (let x = 0; x < region.width; x++, at += 4) {
      const b = by * cols + Math.floor(x / cell);
      const s = b * 4;
      sums[s] += src.data[at];
      sums[s + 1] += src.data[at + 1];
      sums[s + 2] += src.data[at + 2];
      sums[s + 3] += src.data[at + 3];
      counts[b] += 1;
    }
  }
  for (let b = 0; b < counts.length; b++) for (let c = 0; c < 4; c++) sums[b * 4 + c] /= counts[b];
  return { cols, rows, values: sums };
}

/** Pixelate: every pixel of a block takes the block's mean. Returns the region's new pixels. */
export function pixelate(src: Pixels, region: PixelRect, cell: number): Bytes {
  const { cols, values } = blockMeans(src, region, cell);
  const out = new Uint8ClampedArray(region.width * region.height * 4);
  for (let y = 0; y < region.height; y++) {
    const by = Math.floor(y / cell);
    for (let x = 0; x < region.width; x++) {
      const s = (by * cols + Math.floor(x / cell)) * 4;
      const o = (y * region.width + x) * 4;
      out[o] = Math.round(values[s]);
      out[o + 1] = Math.round(values[s + 1]);
      out[o + 2] = Math.round(values[s + 2]);
      out[o + 3] = Math.round(values[s + 3]);
    }
  }
  return out;
}

/** A box blur of half-width `half` along rows (`dx` = 4) or columns, with the edge pixels
 * repeated, so the result inside the region reads only the region. */
function boxPass(input: Float32Array<ArrayBuffer>, width: number, height: number, half: number, horizontal: boolean): Float32Array<ArrayBuffer> {
  const out = new Float32Array(input.length);
  const length = horizontal ? width : height;
  const lines = horizontal ? height : width;
  const span = 2 * half + 1;
  const index = (line: number, i: number) => (horizontal ? (line * width + i) * 4 : (i * width + line) * 4);
  const clampI = (i: number) => (i < 0 ? 0 : i >= length ? length - 1 : i);
  for (let line = 0; line < lines; line++) {
    for (let c = 0; c < 4; c++) {
      let sum = 0;
      for (let i = -half; i <= half; i++) sum += input[index(line, clampI(i)) + c];
      for (let i = 0; i < length; i++) {
        out[index(line, i) + c] = sum / span;
        sum += input[index(line, clampI(i + half + 1)) + c] - input[index(line, clampI(i - half)) + c];
      }
    }
  }
  return out;
}

/**
 * Blur: a smooth surface through the block means. The means are spread between the block
 * centres bilinearly and then softened by two box passes of one block's width, which removes the
 * creases of the interpolation. Neighbouring output pixels can differ by at most 255 / `block`.
 * Returns the region's new pixels.
 */
export function blur(src: Pixels, region: PixelRect, block: number): Bytes {
  const { cols, rows, values } = blockMeans(src, region, block);
  const { width, height } = region;
  const surface = new Float32Array(width * height * 4);
  // The centre of block i sits at i * block + block / 2 (the last, narrower block is treated as a
  // full one, which keeps the spacing even).
  const coord = (p: number, count: number) => {
    const f = (p + 0.5) / block - 0.5;
    const i0 = Math.max(0, Math.min(count - 1, Math.floor(f)));
    const i1 = Math.min(count - 1, i0 + 1);
    const t = Math.max(0, Math.min(1, f - i0));
    return { i0, i1, t };
  };
  const xs = Array.from({ length: width }, (_, x) => coord(x, cols));
  for (let y = 0; y < height; y++) {
    const { i0: r0, i1: r1, t: ty } = coord(y, rows);
    for (let x = 0; x < width; x++) {
      const { i0: c0, i1: c1, t: tx } = xs[x];
      const o = (y * width + x) * 4;
      for (let c = 0; c < 4; c++) {
        const top = values[(r0 * cols + c0) * 4 + c] * (1 - tx) + values[(r0 * cols + c1) * 4 + c] * tx;
        const bottom = values[(r1 * cols + c0) * 4 + c] * (1 - tx) + values[(r1 * cols + c1) * 4 + c] * tx;
        surface[o + c] = top * (1 - ty) + bottom * ty;
      }
    }
  }
  const half = Math.max(1, Math.floor(block / 2));
  let smooth = surface;
  for (let pass = 0; pass < 2; pass++) {
    smooth = boxPass(smooth, width, height, half, true);
    smooth = boxPass(smooth, width, height, half, false);
  }
  const out = new Uint8ClampedArray(smooth.length);
  for (let i = 0; i < smooth.length; i++) out[i] = Math.round(smooth[i]);
  return out;
}

/** The redacted pixels of one object over the part of the image it covers, computed from the
 * original pixels; `null` when the object lies outside the image. */
export function redactionPatch(src: Pixels, object: Redaction, scale: number): { region: PixelRect; data: Bytes } | null {
  const region = pixelBounds(object.rect, src.width, src.height);
  if (!region) return null;
  const size = blockSize(object, scale);
  const data = object.kind === "blur" ? blur(src, region, size) : pixelate(src, region, size);
  return { region, data };
}

/** A cache of patches keyed by everything a patch depends on, so a redraw that moves an arrow
 * does not blur the same region again. */
export class PatchCache {
  private patches = new Map<string, { region: PixelRect; data: Bytes } | null>();
  private source: Pixels | null = null;

  get(src: Pixels, object: Redaction, scale: number): { region: PixelRect; data: Bytes } | null {
    if (src !== this.source) {
      this.patches.clear();
      this.source = src;
    }
    const r = object.rect;
    const key = `${object.kind}|${object.kind === "blur" ? object.strength : object.cell}|${scale}|${r.x}|${r.y}|${r.width}|${r.height}`;
    if (!this.patches.has(key)) {
      // Patches of objects that no longer exist would pile up during a drag; a small cap is enough.
      if (this.patches.size > 64) this.patches.clear();
      this.patches.set(key, redactionPatch(src, object, scale));
    }
    return this.patches.get(key) ?? null;
  }

  clear(): void {
    this.patches.clear();
    this.source = null;
  }
}

/**
 * The pixels of `area` with every visible blur and pixelate of `objects` flattened into them, in
 * stacking order. This is the base the export draws its drawings on: the original pixels of a
 * redacted region are overwritten here and never reach the canvas.
 */
export function flattenRedactions(src: Pixels, objects: readonly AnnotationObject[], scale: number, area: PixelRect, cache?: PatchCache): Bytes {
  const out = new Uint8ClampedArray(area.width * area.height * 4);
  for (let y = 0; y < area.height; y++) {
    const from = ((area.y + y) * src.width + area.x) * 4;
    out.set(src.data.subarray(from, from + area.width * 4), y * area.width * 4);
  }
  for (const object of objects) {
    if (object.hidden || !isRedaction(object)) continue;
    const patch = cache ? cache.get(src, object, scale) : redactionPatch(src, object, scale);
    if (!patch) continue;
    const part = intersect(patch.region, area);
    if (!part) continue;
    for (let y = part.y; y < part.y + part.height; y++) {
      const from = ((y - patch.region.y) * patch.region.width + (part.x - patch.region.x)) * 4;
      out.set(patch.data.subarray(from, from + part.width * 4), ((y - area.y) * area.width + (part.x - area.x)) * 4);
    }
  }
  return out;
}
