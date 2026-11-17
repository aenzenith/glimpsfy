// Pure helpers of the text layer: where a recognised box sits on screen, and which text a
// selection of words stands for. The layer (`TextLayer.vue`) and the composable
// (`useRecognition.ts`) use them; they touch no DOM and no bridge.

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

/** A cheap fingerprint of pixels, so the same area with the same pixels is not read twice. */
export function fingerprint(width: number, height: number, data: ArrayLike<number>): string {
  let hash = 2166136261;
  const step = Math.max(1, Math.floor(data.length / 4096));
  for (let i = 0; i < data.length; i += step) hash = Math.imul(hash ^ data[i], 16777619);
  return `${width}x${height}:${(hash >>> 0).toString(36)}`;
}

/** A name for a job, which `ocr_cancel` takes. */
export function jobName(): string {
  const random = typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID() : Math.random().toString(36).slice(2);
  return `text-layer-${random}`;
}
