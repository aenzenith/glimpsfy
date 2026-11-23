// Snapping to words: the highlighter, blur, pixelate and spotlight cover whole recognised words
// the way a text selection does. This module places a recognition's words in image pixels, finds
// the word under a point, takes the words between two of them in reading order, and turns them
// into one shape per run of a line. It touches no DOM; the engine (`engine.ts`) calls it during a
// drag, and `useSnapWords.ts` feeds it the words.

import type { Point, Rect } from "../lib/annotate";
import type { OcrResult } from "../lib/api/ocr";
import { lineRuns } from "../ocr/text";
import type { ToolId } from "./prefs";
import { SNAP_BAR, SNAP_REDACT_PAD, SNAP_SPOT_PAD } from "./style";

/** The tools that snap to words. */
export const SNAP_TOOLS: readonly ToolId[] = ["highlighter", "blur", "pixelate", "spotlight"];

export const snaps = (tool: ToolId | null): boolean => tool !== null && SNAP_TOOLS.includes(tool);

/** A recognised word in image pixels. `order` is its place in reading order and its index in the
 * list `snapWords` returns; the words of one `line` and `run` make one shape. */
export type SnapWord = { box: Rect; line: Rect; lineIndex: number; run: number; order: number };

/** What a snapped drag draws for one run: the highlighter's horizontal bar, as tall as `width`, or
 * the box of a blur, a pixelate or a spotlight. */
export type SnapShape = { kind: "bar"; from: Point; to: Point; width: number } | { kind: "box"; rect: Rect };

/** The words of `result`, which read the pixels of `area`, placed in the image's pixels. */
export function snapWords(result: OcrResult, area: Rect): SnapWord[] {
  const kx = result.imageSize.width > 0 ? area.width / result.imageSize.width : 1;
  const ky = result.imageSize.height > 0 ? area.height / result.imageSize.height : 1;
  const place = (b: Rect): Rect => ({ x: area.x + b.x * kx, y: area.y + b.y * ky, width: b.width * kx, height: b.height * ky });
  const words: SnapWord[] = [];
  result.lines.forEach((line, lineIndex) => {
    const lineBox = place(line.bounds);
    lineRuns(line).forEach((indices, run) => {
      for (const i of indices) {
        const box = place(line.words[i].bounds);
        if (box.width > 0 && box.height > 0) words.push({ box, line: lineBox, lineIndex, run, order: words.length });
      }
    });
  });
  return words;
}

/** How far `p` lies from `r`; 0 inside it. */
function distance(r: Rect, p: Point): number {
  const dx = Math.max(r.x - p.x, 0, p.x - (r.x + r.width));
  const dy = Math.max(r.y - p.y, 0, p.y - (r.y + r.height));
  return Math.hypot(dx, dy);
}

/** The word under `p`, or within `reach` of it; the nearest one where boxes are that close. */
export function wordAt(words: readonly SnapWord[], p: Point, reach: number): SnapWord | null {
  let best: SnapWord | null = null;
  let near = Infinity;
  for (const word of words) {
    const d = distance(word.box, p);
    if (d <= reach && d < near) {
      best = word;
      near = d;
    }
  }
  return best;
}

/** Where a drag that started on a word ends, as a text selection reads it: the word under `p`,
 * else the nearest word of a line whose band holds `p`, else the nearest word. */
export function endWord(words: readonly SnapWord[], p: Point): SnapWord | null {
  const under = wordAt(words, p, 0);
  if (under) return under;
  let best: SnapWord | null = null;
  let near = Infinity;
  for (const word of words) {
    if (p.y < word.line.y || p.y > word.line.y + word.line.height) continue;
    const d = distance(word.box, { x: p.x, y: word.box.y });
    if (d < near) {
      best = word;
      near = d;
    }
  }
  if (best) return best;
  for (const word of words) {
    const d = distance(word.box, p);
    if (d < near) {
      best = word;
      near = d;
    }
  }
  return best;
}

/** The words from `a` to `b` in reading order, whichever comes first. */
export function range(words: readonly SnapWord[], a: SnapWord, b: SnapWord): SnapWord[] {
  return words.slice(Math.min(a.order, b.order), Math.max(a.order, b.order) + 1);
}

function clip(r: Rect, area: Rect): Rect | null {
  const x0 = Math.max(r.x, area.x);
  const y0 = Math.max(r.y, area.y);
  const x1 = Math.min(r.x + r.width, area.x + area.width);
  const y1 = Math.min(r.y + r.height, area.y + area.height);
  return x1 > x0 && y1 > y0 ? { x: x0, y: y0, width: x1 - x0, height: y1 - y0 } : null;
}

/** One shape per run of the words, in image pixels (`scale` pixels per point) and inside `area`,
 * so a long address cut by the selection's edge is covered up to that edge and no further. */
export function shapesFor(tool: ToolId, words: readonly SnapWord[], scale: number, area: Rect): SnapShape[] {
  const runs: SnapWord[][] = [];
  for (const word of words) {
    const run = runs[runs.length - 1];
    const last = run?.[run.length - 1];
    if (last && last.lineIndex === word.lineIndex && last.run === word.run) run.push(word);
    else runs.push([word]);
  }
  const shapes: SnapShape[] = [];
  for (const run of runs) {
    const left = Math.min(...run.map((w) => w.box.x));
    const right = Math.max(...run.map((w) => w.box.x + w.box.width));
    if (tool === "highlighter") {
      const line = run[0].line;
      const side = SNAP_BAR.side * scale;
      const room = SNAP_BAR.line * scale;
      const bar = clip({ x: left - side, y: line.y - room, width: right - left + 2 * side, height: line.height + 2 * room }, area);
      if (!bar) continue;
      const y = bar.y + bar.height / 2;
      shapes.push({ kind: "bar", from: { x: bar.x, y }, to: { x: bar.x + bar.width, y }, width: bar.height });
      continue;
    }
    const top = Math.min(...run.map((w) => w.box.y));
    const bottom = Math.max(...run.map((w) => w.box.y + w.box.height));
    const pad = (tool === "spotlight" ? SNAP_SPOT_PAD : SNAP_REDACT_PAD) * scale;
    const box = clip({ x: left - pad, y: top - pad, width: right - left + 2 * pad, height: bottom - top + 2 * pad }, area);
    if (box) shapes.push({ kind: "box", rect: box });
  }
  return shapes;
}
