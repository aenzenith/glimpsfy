// Smart redaction places what it found in the document's pixels, and the redaction drawn there is
// flattened into the export. A box placed beside its text would export the very value it was
// meant to hide, without a sign: these tests check that every found box is covered whole after
// the area's shift inside the document and the scale of the reading, that a password cut out of
// a longer word is covered wherever a monospaced or a proportional font puts its glyphs, and that
// nothing reaches outside the area.

import { describe, expect, it, vi } from "vitest";

import type { Entity, OcrResult } from "../lib/api/ocr";
import type { PixelRect } from "../annotate/redact";

import { redactionRects } from "./sensitive";

vi.mock("../lib/api/ocr", () => ({ ocr_detect_entities: async () => [] }));
vi.mock("./models", () => ({ ppOcrWithoutModels: async () => false }));

type Box = { x: number; y: number; width: number; height: number };

function word(text: string, bounds: Box) {
  return { text, bounds, confidence: 1 };
}

function reading(lines: { text: string; words: ReturnType<typeof word>[] }[], width: number, height: number): OcrResult {
  return {
    engine: "vision",
    text: lines.map((l) => l.text).join("\n"),
    lines: lines.map((l, i) => ({
      text: l.text,
      bounds: l.words[0].bounds,
      confidence: 1,
      words: l.words,
      block: i,
      kind: "code" as const,
    })),
    language: null,
    imageSize: { width, height },
  };
}

function entity(kind: Entity["kind"], text: string, boxes: { line: number; bounds: Box }[]): Entity {
  return { kind, text, value: text, boxes, date: null };
}

/** Whether `outer` holds `inner` whole. */
function covers(outer: PixelRect, inner: Box): boolean {
  return outer.x <= inner.x && outer.y <= inner.y && outer.x + outer.width >= inner.x + inner.width && outer.y + outer.height >= inner.y + inner.height;
}

function inside(area: PixelRect, r: PixelRect): boolean {
  return r.x >= area.x && r.y >= area.y && r.x + r.width <= area.x + area.width && r.y + r.height <= area.y + area.height;
}

// Helvetica's advances, in thousandths of an em, for the characters of the lines below.
const HELVETICA: Record<string, number> = Object.fromEntries(
  (
    [
      [" ", 278], ["+", 584], [".", 278], ["/", 278], [":", 278], ["=", 584], ["@", 1015], ["_", 556], ["0", 556], ["1", 556],
      ["2", 556], ["3", 556], ["4", 556], ["5", 556], ["6", 556], ["7", 556], ["8", 556], ["9", 556], ["A", 667], ["B", 667],
      ["D", 722], ["E", 667], ["I", 278], ["K", 667], ["L", 556], ["O", 778], ["P", 667], ["R", 722], ["S", 667], ["T", 611],
      ["U", 722], ["W", 944], ["a", 556], ["b", 556], ["c", 500], ["d", 556], ["e", 556], ["g", 556], ["h", 556], ["i", 222],
      ["l", 222], ["m", 833], ["n", 556], ["o", 556], ["p", 556], ["r", 333], ["s", 500], ["t", 278], ["x", 500],
    ] as const
  ).map(([c, w]) => [c, w]),
);

/** A reading of `lines` set at 20 px an em in a font whose glyphs are `em(c)` ems wide, every
 * word's box exact, as Vision gives it, and every glyph's place, which the reading does not hold. */
function setIn(lines: string[], em: (c: string) => number): { result: OcrResult; glyphs: Box[][] } {
  const glyphs: Box[][] = [];
  const read = lines.map((text, i) => {
    const y = 20 + i * 40;
    let x = 20;
    const placed = [...text].map((c) => {
      const glyph = { x, y, width: em(c) * 20, height: 24 };
      x += glyph.width;
      return glyph;
    });
    glyphs.push(placed);
    const words: ReturnType<typeof word>[] = [];
    for (const m of text.matchAll(/\S+/g)) {
      const a = placed[m.index];
      const b = placed[m.index + m[0].length - 1];
      words.push(word(m[0], { x: a.x, y, width: b.x + b.width - a.x, height: 24 }));
    }
    return { text, words };
  });
  const result = reading(read, 1200, 200);
  result.lines.forEach((line, i) => {
    const g = glyphs[i];
    line.bounds = { x: g[0].x, y: g[0].y, width: g[g.length - 1].x + g[g.length - 1].width - g[0].x, height: 24 };
  });
  return { result, glyphs };
}

describe("redactionRects", () => {
  // A selection far from the frame's origin, read at half its resolution, on a Retina frame.
  const area = { x: 1203, y: 641, width: 800, height: 400 };
  const scale = 2;
  const sx = area.width / 400;
  const sy = area.height / 200;
  const toDoc = (b: Box): Box => ({ x: area.x + b.x * sx, y: area.y + b.y * sy, width: b.width * sx, height: b.height * sy });

  it("covers every box of a piece after the area's shift and the reading's scale", () => {
    const card = [
      { line: 0, bounds: { x: 120.4, y: 40.2, width: 96.3, height: 11.7 } },
      { line: 1, bounds: { x: 20.6, y: 58.9, width: 48.1, height: 11.2 } },
    ];
    const result = reading(
      [
        { text: "Kart 4111 1111", words: [word("Kart", { x: 80, y: 40, width: 30, height: 12 }), word("41111111", { x: 120.4, y: 40.2, width: 96.3, height: 11.7 })] },
        { text: "1111 1111", words: [word("11111111", { x: 20.6, y: 58.9, width: 48.1, height: 11.2 })] },
      ],
      400,
      200,
    );
    const marks = redactionRects(entity("card", "4111 1111 1111 1111", card), result, area, scale);
    expect(marks).toHaveLength(2);
    marks.forEach((mark, i) => {
      expect(covers(mark.rect, toDoc(card[i].bounds))).toBe(true);
      expect(inside(area, mark.rect)).toBe(true);
      expect(mark.cut).toEqual([]);
    });
  });

  it("covers a password inside a link in a monospaced and in a proportional font, and little around it", () => {
    const lines = ["DATABASE_URL=postgres://app:p@ss9Kx2Lm@db.internal/prod", "REDIS_URL=redis://:s3cr3tPassW0rd@cache.local:6379", "SUPPORT=+90 532 418 27 64"];
    // A selection read at half its resolution, on a Retina frame.
    const wide = { x: 100, y: 50, width: 2400, height: 400 };
    const toWide = (b: Box): Box => ({ x: wide.x + b.x * 2, y: wide.y + b.y * 2, width: b.width * 2, height: b.height * 2 });
    const fonts: [string, (c: string) => number][] = [
      ["Menlo", () => 0.6],
      ["Helvetica", (c) => HELVETICA[c] / 1000],
    ];
    for (const [font, em] of fonts) {
      const { result, glyphs } = setIn(lines, em);
      for (const [line, value] of [
        [0, "p@ss9Kx2Lm"],
        [1, "s3cr3tPassW0rd"],
      ] as const) {
        const from = lines[line].indexOf(value);
        const to = from + value.length;
        // The core gives the piece its share of the word's characters (spans.rs).
        const word = result.lines[line].words[0];
        const n = word.text.length;
        const share = { x: word.bounds.x + (word.bounds.width * from) / n, y: word.bounds.y, width: (word.bounds.width * value.length) / n, height: word.bounds.height };
        const [mark] = redactionRects(entity("urlPassword", value, [{ line, bounds: share }]), result, wide, scale);
        const truth = toWide({ x: glyphs[line][from].x, y: word.bounds.y, width: glyphs[line][to - 1].x + glyphs[line][to - 1].width - glyphs[line][from].x, height: word.bounds.height });
        expect(covers(mark.rect, truth), `${font}: ${value}`).toBe(true);
        // Past the glyphs it reaches no further than one average character and the padding.
        const reach = (word.bounds.width / n) * 2 + 3 * scale + 1;
        expect(truth.x - mark.rect.x, `${font}: ${value} on the left`).toBeLessThanOrEqual(reach);
        expect(mark.rect.x + mark.rect.width - (truth.x + truth.width), `${font}: ${value} on the right`).toBeLessThanOrEqual(reach);
      }
    }
  });

  it("keeps a box at the area's edge inside the area and says it is cut", () => {
    const left = { x: 0, y: 10, width: 60, height: 12 };
    // Vision stops a pixel or two short of a glyph the edge halves.
    const corner = { x: 338, y: 187.5, width: 60, height: 11.5 };
    const result = reading(
      [
        { text: "ghp_8fK2", words: [word("ghp_8fK2", left)] },
        { text: "ayse@ornek.com", words: [word("ayse@ornek.com", corner)] },
      ],
      400,
      200,
    );
    const [a] = redactionRects(entity("token", "ghp_8fK2", [{ line: 0, bounds: left }]), result, area, scale);
    const [b] = redactionRects(entity("email", "ayse@ornek.com", [{ line: 1, bounds: corner }]), result, area, scale);
    expect(a.rect.x).toBe(area.x);
    expect(a.cut).toEqual(["left"]);
    expect(covers(a.rect, toDoc(left))).toBe(true);
    expect(b.cut).toEqual(["right", "bottom"]);
    expect(covers(b.rect, toDoc(corner))).toBe(true);
    expect(inside(area, a.rect) && inside(area, b.rect)).toBe(true);
  });
});
