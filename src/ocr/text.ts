// Pure helpers of the text layer: where a recognised box sits on screen, how a line's words fall
// into runs, and which text a selection of words stands for. The layer (`TextLayer.vue`), the
// composable (`useRecognition.ts`) and the word snapping of the annotation tools
// (`src/annotate/snap.ts`) use them; they touch no DOM and no bridge.

import type { Bounds, OcrLine, OcrResult } from "../lib/api/ocr";

/** A rectangle in CSS pixels of the layer's box. */
export type Rect = { x: number; y: number; width: number; height: number };

/** Maps boxes in pixels of the recognised image onto `rect`, where the image shows on screen. */
export function placer(rect: Rect, imageSize: { width: number; height: number }): (b: Bounds, pad?: number) => Rect {
  const kx = imageSize.width > 0 ? rect.width / imageSize.width : 1;
  const ky = imageSize.height > 0 ? rect.height / imageSize.height : 1;
  return (b, pad = 0) => ({
    x: rect.x + b.x * kx - pad,
    y: rect.y + b.y * ky - pad,
    width: b.width * kx + 2 * pad,
    height: b.height * ky + 2 * pad,
  });
}

/** The words of a line in runs, as indices into `line.words`: a gap wider than 1.2 times the
 * line's height (a table's cell gap, a sidebar's value) starts a new run. The text layer outlines
 * each run on its own, and the annotation tools that snap to words draw one shape per run. */
export function lineRuns(line: OcrLine): number[][] {
  const runs: number[][] = [];
  line.words.forEach((word, i) => {
    const run = runs[runs.length - 1];
    const before = run ? line.words[run[run.length - 1]].bounds : null;
    if (before && word.bounds.x - (before.x + before.width) <= line.bounds.height * 1.2) run.push(i);
    else runs.push([i]);
  });
  return runs;
}

/** Where each word of each line starts and ends in `result.text`, or `null` for a word that the
 * text spells differently (a hyphen removed at a line end, for example). The words are searched
 * in reading order, each after the one before, so a repeated word finds its own place. */
export function wordOffsets(result: OcrResult): Array<Array<[number, number] | null>> {
  const text = result.text;
  let at = 0;
  return result.lines.map((line) =>
    line.words.map((word) => {
      for (const form of [word.text, word.text.replace(/[-­‐]$/, "")]) {
        if (!form) continue;
        const i = text.indexOf(form, at);
        // A word found far ahead belongs to another line; indentation and table tabs stay short.
        if (i >= 0 && i - at <= 96) {
          at = i + form.length;
          return [i, i + form.length] as [number, number];
        }
      }
      return null;
    }),
  );
}

/** A selected range of words: from character `startChar` of the first word to character
 * `endChar` (exclusive) of the last, both in reading order. */
export type WordRange = { start: [number, number]; startChar: number; end: [number, number]; endChar: number };

/** The text a range of words stands for, cut from `result.text` so that indentation, tabs between
 * table cells and paragraph breaks stay as "Copy all text" writes them. A range that starts at a
 * line's first word keeps the line's indentation. */
export function textOf(result: OcrResult, offsets: Array<Array<[number, number] | null>>, range: WordRange): string {
  const first = offsets[range.start[0]]?.[range.start[1]];
  const last = offsets[range.end[0]]?.[range.end[1]];
  if (first && last && first[0] <= last[1]) {
    let from = Math.min(first[0] + range.startChar, first[1]);
    if (range.start[1] === 0 && range.startChar === 0) from = result.text.lastIndexOf("\n", first[0] - 1) + 1;
    const to = Math.max(Math.min(last[0] + range.endChar, last[1]), from);
    return result.text.slice(from, to);
  }
  return joinWords(result.lines, range);
}

/** The fallback: the words themselves, a space between words and a line break between lines. */
function joinWords(lines: OcrLine[], range: WordRange): string {
  const out: string[] = [];
  for (let l = range.start[0]; l <= range.end[0]; l++) {
    const words = lines[l]?.words ?? [];
    const from = l === range.start[0] ? range.start[1] : 0;
    const to = l === range.end[0] ? range.end[1] : words.length - 1;
    const picked = words.slice(from, to + 1).map((w, i, all) => {
      let text = w.text;
      if (i === all.length - 1 && l === range.end[0]) text = text.slice(0, range.endChar);
      if (i === 0 && l === range.start[0]) text = text.slice(range.startChar);
      return text;
    });
    out.push(picked.join(" "));
  }
  return out.join("\n");
}

/** The number of lines that hold text, as the chip and the toast count them. */
export function lineCount(text: string): number {
  return text.split("\n").filter((line) => line.trim()).length;
}

/** The language's name in the user's language, such as "English" or "Türkçe"; `null` without one. */
export function languageName(tag: string | null, uiLanguage: string): string | null {
  if (!tag) return null;
  try {
    return new Intl.DisplayNames([uiLanguage], { type: "language" }).of(tag) ?? tag;
  } catch {
    return tag;
  }
}

/** A fingerprint of pixels, so the same area with the same pixels is not read twice. Every byte
 * counts: a blur over a password changes a few thousand bytes of a 5K image, which a sample would
 * miss, and the layer would then show and copy the text from before the blur. It hashes 32-bit
 * words with murmur3's mixing, under 20 ms for a 5K image, next to the second or so a reading
 * takes. Each step is a bijection of the state, so inputs that differ in one word never collide. */
export function fingerprint(width: number, height: number, data: Uint8Array | Uint8ClampedArray): string {
  let hash = 0x9747b28c ^ data.length;
  // Pixels from a canvas start on a word boundary; a view that does not is hashed byte by byte.
  const words = data.byteOffset % 4 === 0 ? new Uint32Array(data.buffer, data.byteOffset, data.length >>> 2) : new Uint32Array(0);
  // The mixing is written out in both loops: a shared closure runs several times slower.
  for (let w = 0; w < words.length; w++) {
    let k = Math.imul(words[w], 0xcc9e2d51);
    k = Math.imul((k << 15) | (k >>> 17), 0x1b873593);
    hash ^= k;
    hash = (hash << 13) | (hash >>> 19);
    hash = (Math.imul(hash, 5) + 0xe6546b64) | 0;
  }
  for (let i = words.length * 4; i < data.length; i++) {
    let k = Math.imul(data[i], 0xcc9e2d51);
    k = Math.imul((k << 15) | (k >>> 17), 0x1b873593);
    hash ^= k;
    hash = (hash << 13) | (hash >>> 19);
    hash = (Math.imul(hash, 5) + 0xe6546b64) | 0;
  }
  return `${width}x${height}:${(hash >>> 0).toString(36)}`;
}

/** A name for a job, which `ocr_cancel` takes. */
export function jobName(): string {
  const random = typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : Math.random().toString(36).slice(2);
  return `text-layer-${random}`;
}
