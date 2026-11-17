// Redaction must destroy pixels: once a capture leaves the machine with a blurred API key in it,
// nothing can take the key back. These tests run the export's own flattening step
// (`flattenRedactions`, the pixels the export writes before it draws anything on top) over an
// image of black text-like strokes on white, the content redaction exists for, and check that the
// region keeps nothing but block means:
//
// - Two images whose pixels differ only inside blocks, with the same block means, give the same
//   output, so the output cannot carry anything finer than a block.
// - Pixelate paints each block in one colour: its rounded mean. Changing one source pixel changes
//   that whole block uniformly and nothing else.
// - Blur cannot reproduce an edge: the source has 255-step edges between neighbouring pixels, and
//   no two neighbouring output pixels differ by more than 255 / block + 1.
//
// Every redaction kind and level is walked at 1× and 2×, because the block size grows with the
// scale and a regression at one size would leak only there.

import { describe, expect, it } from "vitest";

import type { AnnotationObject, Rect } from "../lib/annotate";
import { blockSize, flattenRedactions, pixelBounds, type PixelRect, type Pixels, type Redaction } from "./redact";
import { BLUR_LEVELS, CELL_LEVELS } from "./style";

/** A deterministic random source, so a failure can be replayed. */
function random(seed: number): () => number {
  let s = seed >>> 0;
  return () => {
    s = (s * 1664525 + 1013904223) >>> 0;
    return s / 2 ** 32;
  };
}

/** White paper with lines of black "glyphs": short vertical and horizontal strokes 1–2 px wide,
 * the hardest content to hide, at full contrast. */
function textLike(width: number, height: number, seed: number): Pixels {
  const rnd = random(seed);
  const data = new Uint8ClampedArray(width * height * 4).fill(255);
  const ink = (x: number, y: number) => {
    if (x < 0 || y < 0 || x >= width || y >= height) return;
    const i = (y * width + x) * 4;
    data[i] = data[i + 1] = data[i + 2] = 0;
  };
  for (let line = 4; line + 12 < height; line += 16) {
    for (let x = 2; x < width - 8; x += 3 + Math.floor(rnd() * 4)) {
      const tall = 6 + Math.floor(rnd() * 6);
      const thick = 1 + Math.floor(rnd() * 2);
      for (let dy = 0; dy < tall; dy++) for (let dx = 0; dx < thick; dx++) ink(x + dx, line + dy);
      if (rnd() < 0.4) for (let dx = 0; dx < 4; dx++) ink(x + dx, line + Math.floor(tall / 2));
    }
  }
  return { width, height, data };
}

function object(kind: "blur" | "pixelate", level: string, rect: Rect): Redaction {
  const base = { id: `${kind}-1`, hidden: false, locked: false, rect };
  return (kind === "blur" ? { ...base, kind, strength: level } : { ...base, kind, cell: level }) as Redaction;
}

/** Calls `visit` for every block of `region` with its pixels' coordinates. */
function forEachBlock(region: PixelRect, size: number, visit: (pixels: [number, number][]) => void): void {
  for (let by = region.y; by < region.y + region.height; by += size) {
    for (let bx = region.x; bx < region.x + region.width; bx += size) {
      const pixels: [number, number][] = [];
      for (let y = by; y < Math.min(by + size, region.y + region.height); y++) {
        for (let x = bx; x < Math.min(bx + size, region.x + region.width); x++) pixels.push([x, y]);
      }
      visit(pixels);
    }
  }
}

/** The same image with the pixels of every block shuffled: different content, same block means. */
function shuffledWithinBlocks(src: Pixels, region: PixelRect, size: number, seed: number): Pixels {
  const rnd = random(seed);
  const data = new Uint8ClampedArray(src.data);
  forEachBlock(region, size, (pixels) => {
    const order = pixels.map((p) => ({ p, k: rnd() })).sort((a, b) => a.k - b.k).map((e) => e.p);
    pixels.forEach(([x, y], i) => {
      const [sx, sy] = order[i];
      data.set(src.data.subarray((sy * src.width + sx) * 4, (sy * src.width + sx) * 4 + 4), (y * src.width + x) * 4);
    });
  });
  return { width: src.width, height: src.height, data };
}

const W = 240;
const H = 160;
/** A crop that starts inside the redaction, so the blocks must stay aligned to the redaction's own
 * corner and not to the exported area's. */
const AREA: PixelRect = { x: 30, y: 6, width: 200, height: 150 };
const RECT: Rect = { x: 13.5, y: 9.25, width: 171, height: 118.5 };

const cases = [
  ...BLUR_LEVELS.map((level) => ({ kind: "blur" as const, level })),
  ...CELL_LEVELS.map((level) => ({ kind: "pixelate" as const, level })),
].flatMap((c) => [1, 2].map((scale) => ({ ...c, scale })));

const px = (data: Uint8ClampedArray, area: PixelRect, x: number, y: number) => {
  const i = ((y - area.y) * area.width + (x - area.x)) * 4;
  return [data[i], data[i + 1], data[i + 2], data[i + 3]];
};

describe("redaction in the exported pixels", () => {
  it.each(cases)("$kind $level at $scale× keeps nothing but block means", ({ kind, level, scale }) => {
    const src = textLike(W, H, 7);
    const redaction = object(kind, level, RECT);
    const objects: AnnotationObject[] = [redaction];
    const region = pixelBounds(RECT, W, H)!;
    const size = blockSize(redaction, scale);
    const inArea = (x: number, y: number) => x >= AREA.x && y >= AREA.y && x < AREA.x + AREA.width && y < AREA.y + AREA.height;

    const out = flattenRedactions(src, objects, scale, AREA);

    // The source really has text edges inside the region.
    let sourceEdge = 0;
    for (let y = region.y; y < region.y + region.height; y++) {
      for (let x = region.x; x + 1 < region.x + region.width; x++) {
        sourceEdge = Math.max(sourceEdge, Math.abs(src.data[(y * W + x) * 4] - src.data[(y * W + x + 1) * 4]));
      }
    }
    expect(sourceEdge).toBe(255);

    // Different content with the same block means gives exactly the same output.
    const twin = shuffledWithinBlocks(src, region, size, 99);
    expect(twin.data).not.toEqual(src.data);
    expect(flattenRedactions(twin, objects, scale, AREA)).toEqual(out);

    // Outside the region the export keeps the original pixels.
    for (let y = AREA.y; y < AREA.y + AREA.height; y++) {
      for (let x = AREA.x; x < AREA.x + AREA.width; x++) {
        const inside = x >= region.x && y >= region.y && x < region.x + region.width && y < region.y + region.height;
        if (!inside) expect(px(out, AREA, x, y)).toEqual([...src.data.subarray((y * W + x) * 4, (y * W + x) * 4 + 4)]);
      }
    }

    if (kind === "pixelate") {
      // Every block shows one colour: its rounded mean.
      forEachBlock(region, size, (pixels) => {
        const mean = [0, 1, 2, 3].map((c) => Math.round(pixels.reduce((s, [x, y]) => s + src.data[(y * W + x) * 4 + c], 0) / pixels.length));
        for (const [x, y] of pixels) if (inArea(x, y)) expect(px(out, AREA, x, y)).toEqual(mean);
      });

      // One source pixel changes its whole block uniformly, and nothing else. A large block's
      // mean moves by less than one step for most pixels, so the test picks a block whose
      // rounded mean does move when its middle pixel flips between black and white.
      let block: [number, number][] = [];
      forEachBlock(region, size, (pixels) => {
        if (block.length > 0 || pixels.length !== size * size || !pixels.every(([x, y]) => inArea(x, y))) return;
        const [mx, my] = pixels[Math.floor(pixels.length / 2)];
        const sum = pixels.reduce((s, [x, y]) => s + src.data[(y * W + x) * 4], 0);
        const old = src.data[(my * W + mx) * 4];
        const flipped = sum - old + (old > 127 ? 0 : 255);
        if (Math.round(sum / pixels.length) !== Math.round(flipped / pixels.length)) block = pixels;
      });
      expect(block.length, "the image has a block whose mean moves").toBeGreaterThan(0);
      const [tx, ty] = block[Math.floor(block.length / 2)];
      const changed = { ...src, data: new Uint8ClampedArray(src.data) };
      const t = (ty * W + tx) * 4;
      const value = changed.data[t] > 127 ? 0 : 255;
      changed.data[t] = changed.data[t + 1] = changed.data[t + 2] = value;
      const after = flattenRedactions(changed, objects, scale, AREA);
      const moved = new Set<string>();
      for (let y = AREA.y; y < AREA.y + AREA.height; y++) {
        for (let x = AREA.x; x < AREA.x + AREA.width; x++) if (px(after, AREA, x, y).join() !== px(out, AREA, x, y).join()) moved.add(`${x},${y}`);
      }
      expect(moved).toEqual(new Set(block.map(([x, y]) => `${x},${y}`)));
      const colours = new Set(block.map(([x, y]) => px(after, AREA, x, y).join()));
      expect(colours.size).toBe(1);
    } else {
      // No edge survives: neighbours inside the region differ by at most 255 / block + 1.
      const limit = Math.ceil(255 / size) + 1;
      let steepest = 0;
      for (let y = Math.max(region.y, AREA.y); y < Math.min(region.y + region.height, AREA.y + AREA.height); y++) {
        for (let x = Math.max(region.x, AREA.x); x < Math.min(region.x + region.width, AREA.x + AREA.width); x++) {
          const here = px(out, AREA, x, y);
          for (const [nx, ny] of [[x + 1, y], [x, y + 1]]) {
            if (nx >= region.x + region.width || ny >= region.y + region.height || !inArea(nx, ny)) continue;
            const there = px(out, AREA, nx, ny);
            for (let c = 0; c < 3; c++) steepest = Math.max(steepest, Math.abs(here[c] - there[c]));
          }
        }
      }
      expect(steepest).toBeLessThanOrEqual(limit);
    }
  });

  it("does not export a hidden redaction's pixels as redacted, and a visible one wins over a hidden one", () => {
    // A hidden blur hides nothing (the objects panel says so); the visible pixelate still applies.
    const src = textLike(W, H, 3);
    const hidden = { ...object("blur", "strong", RECT), hidden: true };
    const visible = object("pixelate", "medium", { x: 60, y: 40, width: 50, height: 40 });
    const out = flattenRedactions(src, [visible, hidden], 1, AREA);
    const alone = flattenRedactions(src, [visible], 1, AREA);
    expect(out).toEqual(alone);
  });
});
