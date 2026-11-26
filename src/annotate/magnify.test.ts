// A magnifier over a blurred or pixelated area must not bring back what the redaction hides: the
// loupe enlarges a small area of the image, and an exported loupe that read the original pixels
// would put a hidden password into the file larger than it ever was. The loupe's pixels are what
// the renderer copies one to one into the export (`loupePixels`, compose.ts), so the test reads
// them directly.
//
// Two images that differ only inside the redaction's blocks, with the same block means, must give
// the same loupe: the loupe can then carry nothing the redaction did not keep. With the redaction
// hidden the two loupes differ, which shows that the test would see a leak. Every redaction kind
// and level is walked at 1× and 2×, Sharp and Smooth, with the ring reaching past the redaction's
// edge.

import { describe, expect, it } from "vitest";

import type { AnnotationObject } from "../lib/annotate";
import { loupePixels, type Magnify } from "./magnify";
import { blockSize, pixelBounds, type Pixels, type Redaction } from "./redact";
import { BLUR_LEVELS, CELL_LEVELS } from "./style";

/** White paper with short black strokes, the content redaction exists for. */
function textLike(width: number, height: number): Pixels {
  let seed = 7;
  const rnd = () => ((seed = (seed * 1664525 + 1013904223) >>> 0), seed / 2 ** 32);
  const data = new Uint8ClampedArray(width * height * 4).fill(255);
  for (let y = 2; y + 8 < height; y += 11) {
    for (let x = 1; x < width - 4; x += 2 + Math.floor(rnd() * 3)) {
      for (let dy = 0; dy < 5 + Math.floor(rnd() * 4); dy++) {
        const i = ((y + dy) * width + x) * 4;
        data[i] = data[i + 1] = data[i + 2] = 0;
      }
    }
  }
  return { width, height, data };
}

/** The image with the pixels of every block of the redaction in reverse order: each block keeps
 * its mean, and almost every pixel changes. */
function shuffled(src: Pixels, redaction: Redaction, scale: number): Pixels {
  const data = new Uint8ClampedArray(src.data);
  const region = pixelBounds(redaction.rect, src.width, src.height)!;
  const size = blockSize(redaction, scale);
  for (let by = region.y; by < region.y + region.height; by += size) {
    for (let bx = region.x; bx < region.x + region.width; bx += size) {
      const cells: number[] = [];
      for (let y = by; y < Math.min(by + size, region.y + region.height); y++) {
        for (let x = bx; x < Math.min(bx + size, region.x + region.width); x++) cells.push((y * src.width + x) * 4);
      }
      cells.forEach((at, i) => data.set(src.data.subarray(cells[cells.length - 1 - i], cells[cells.length - 1 - i] + 4), at));
    }
  }
  return { width: src.width, height: src.height, data };
}

function firstDifference(a: Uint8ClampedArray, b: Uint8ClampedArray): number {
  if (a.length !== b.length) return 0;
  for (let i = 0; i < a.length; i++) if (a[i] !== b[i]) return i;
  return -1;
}

const cases = [1, 2].flatMap((scale) =>
  [false, true].flatMap((smooth) => [
    ...BLUR_LEVELS.map((level) => ({ kind: "blur" as const, level, scale, smooth })),
    ...CELL_LEVELS.map((level) => ({ kind: "pixelate" as const, level, scale, smooth })),
  ]),
);

describe("the magnifier over a redaction", () => {
  it.each(cases)("shows only what $kind $level keeps at $scale×, smooth $smooth", ({ kind, level, scale, smooth }) => {
    const width = 140 * scale;
    const height = 90 * scale;
    const rect = { x: 10.5 * scale, y: 12 * scale, width: 90 * scale, height: 50 * scale };
    const base = { id: "redaction", hidden: false, locked: false, rect };
    const redaction = (kind === "blur" ? { ...base, kind, strength: level } : { ...base, kind, cell: level }) as Redaction;
    // The ring reaches above the redaction, so the loupe also shows pixels the redaction leaves.
    const magnifier: Magnify = {
      id: "magnify",
      hidden: false,
      locked: false,
      kind: "magnify",
      source: { x: 40 * scale, y: 30 * scale },
      radius: 20 * scale,
      at: { x: 100 * scale, y: 60 * scale },
      factor: 3,
      smooth,
      color: { kind: "ink", name: "red" },
    };
    const a = textLike(width, height);
    const b = shuffled(a, redaction, scale);
    expect(firstDifference(a.data, b.data), "the two images differ under the redaction").toBeGreaterThanOrEqual(0);

    const objects: AnnotationObject[] = [redaction, magnifier];
    const loupeA = loupePixels(a, objects, scale, magnifier);
    const loupeB = loupePixels(b, objects, scale, magnifier);
    expect(loupeA).not.toBeNull();
    expect(loupeA!.width).toBe(2 * magnifier.radius * magnifier.factor);
    expect(firstDifference(loupeA!.data, loupeB!.data), "the loupe shows pixels the redaction hides").toBe(-1);

    // Without the redaction the same two images give different loupes, so a leak would show.
    const open: AnnotationObject[] = [{ ...redaction, hidden: true }, magnifier];
    expect(firstDifference(loupePixels(a, open, scale, magnifier)!.data, loupePixels(b, open, scale, magnifier)!.data)).toBeGreaterThanOrEqual(0);
  });
});
