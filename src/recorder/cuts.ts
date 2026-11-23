// The cuts a recording gets from its text: the removed ranges in source milliseconds, kept sorted
// and apart, the edits the text panel makes on them (a selection, every filler word, the long
// silences), what they leave of the recording, and the undo and redo stacks. The recording itself
// never changes; the export skips these ranges.

import type { TrimRange } from "../lib/api/record";
import type { TranscriptSilence, TranscriptWord } from "../lib/api/transcript";

/** A shortened silence keeps this much, half at each side, so the speech still breathes. */
export const SILENCE_KEEP_MS = 300;
/** The thresholds the Shorten menu offers; a silence at least this long counts as long. */
export const SILENCE_THRESHOLDS = [500, 1000, 2000] as const;
export const DEFAULT_SILENCE_THRESHOLD_MS = 1000;

/** A word counts as cut once the cuts cover this share of it. */
const WORD_COVERED = 0.5;
/** A silence counts as cut whole above this share, and as shortened above `SILENCE_SHORT`. */
const SILENCE_WHOLE = 0.95;
const SILENCE_SHORT = 0.3;
/** Shortening a silence that would lose less than this is not worth a cut. */
const MIN_SHORTEN_MS = 50;

// --------------------------------------------------------------------------------- ranges

/** The ranges sorted, empty ones dropped, and overlapping or touching ones merged. */
export function normalize(ranges: readonly TrimRange[]): TrimRange[] {
  const list = ranges
    .filter((r) => Number.isFinite(r.startMs) && Number.isFinite(r.endMs) && r.endMs > r.startMs)
    .map((r) => ({ startMs: Math.max(0, r.startMs), endMs: r.endMs }))
    .sort((a, b) => a.startMs - b.startMs);
  const out: TrimRange[] = [];
  for (const r of list) {
    const last = out[out.length - 1];
    if (last && r.startMs <= last.endMs) last.endMs = Math.max(last.endMs, r.endMs);
    else out.push(r);
  }
  return out;
}

export function addRanges(cuts: readonly TrimRange[], add: readonly TrimRange[]): TrimRange[] {
  return normalize([...cuts, ...add]);
}

/** The cuts with `remove` taken out of them, which can split a cut in two. */
export function subtractRanges(cuts: readonly TrimRange[], remove: readonly TrimRange[]): TrimRange[] {
  const holes = normalize(remove);
  if (!holes.length) return cuts.map((c) => ({ ...c }));
  const out: TrimRange[] = [];
  for (const cut of cuts) {
    let pieces: TrimRange[] = [{ ...cut }];
    for (const hole of holes) {
      if (hole.startMs >= cut.endMs) break;
      if (hole.endMs <= cut.startMs) continue;
      pieces = pieces.flatMap((p) => {
        if (hole.endMs <= p.startMs || hole.startMs >= p.endMs) return [p];
        const left = { startMs: p.startMs, endMs: hole.startMs };
        const right = { startMs: hole.endMs, endMs: p.endMs };
        return [left, right].filter((r) => r.endMs > r.startMs);
      });
    }
    out.push(...pieces);
  }
  return out;
}

/** The cuts without one cut, as "Put back" on a struck word or a gap does. */
export function withoutCut(cuts: readonly TrimRange[], cut: TrimRange): TrimRange[] {
  return cuts.filter((c) => c.startMs !== cut.startMs || c.endMs !== cut.endMs);
}

/** The index of the first cut that ends after `ms`. */
function firstEndingAfter(cuts: readonly TrimRange[], ms: number): number {
  let lo = 0;
  let hi = cuts.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (cuts[mid]!.endMs <= ms) lo = mid + 1;
    else hi = mid;
  }
  return lo;
}

/** The cut that holds `ms`, or `null`. */
export function cutAt(cuts: readonly TrimRange[], ms: number): TrimRange | null {
  const c = cuts[firstEndingAfter(cuts, ms)];
  return c && c.startMs <= ms ? c : null;
}

/** How many milliseconds of `[fromMs, toMs)` the cuts remove. */
export function coveredMs(cuts: readonly TrimRange[], fromMs: number, toMs: number): number {
  let total = 0;
  for (let i = firstEndingAfter(cuts, fromMs); i < cuts.length; i++) {
    const c = cuts[i]!;
    if (c.startMs >= toMs) break;
    total += Math.min(c.endMs, toMs) - Math.max(c.startMs, fromMs);
  }
  return Math.max(0, total);
}

/** What the export keeps of the trimmed part once the cuts are skipped. */
export function keptMs(cuts: readonly TrimRange[], range: TrimRange): number {
  return Math.max(0, range.endMs - range.startMs - coveredMs(cuts, range.startMs, range.endMs));
}

/** The cuts inside the trim, clipped to it, as the export takes them. */
export function cutsWithin(cuts: readonly TrimRange[], range: TrimRange): TrimRange[] {
  return cuts
    .filter((c) => c.endMs > range.startMs && c.startMs < range.endMs)
    .map((c) => ({ startMs: Math.max(c.startMs, range.startMs), endMs: Math.min(c.endMs, range.endMs) }));
}

/** Where playback that reached `ms` goes on: past the cut it is in, unless that cut is `exempt`
 * (the one "Play from here" started in, so the user can hear what was cut). */
export function playableFrom(cuts: readonly TrimRange[], ms: number, exempt: TrimRange | null = null): number {
  const c = cutAt(cuts, ms);
  if (!c || (exempt && c.startMs === exempt.startMs && c.endMs === exempt.endMs)) return ms;
  return c.endMs;
}

// ---------------------------------------------------------------------------- the text

/** 1 for each word the cuts remove, 0 for each word they keep. */
export function wordsCut(words: readonly TranscriptWord[], cuts: readonly TrimRange[]): Uint8Array {
  const out = new Uint8Array(words.length);
  if (!cuts.length) return out;
  words.forEach((w, i) => {
    const length = Math.max(1, w.endMs - w.startMs);
    if (coveredMs(cuts, w.startMs, Math.max(w.endMs, w.startMs + 1)) / length >= WORD_COVERED) out[i] = 1;
  });
  return out;
}

export type SilenceState = { state: "none" | "short" | "cut"; keptMs: number };

/** Whether each silence is kept, shortened (with what is left of it) or cut whole. */
export function silenceStates(silences: readonly TranscriptSilence[], cuts: readonly TrimRange[]): SilenceState[] {
  return silences.map((s) => {
    const length = Math.max(1, s.endMs - s.startMs);
    const covered = coveredMs(cuts, s.startMs, s.endMs);
    const share = covered / length;
    const state = share >= SILENCE_WHOLE ? "cut" : share >= SILENCE_SHORT ? "short" : "none";
    return { state, keptMs: Math.max(0, length - covered) };
  });
}

export const isLong = (s: TranscriptSilence, thresholdMs: number): boolean => s.endMs - s.startMs >= thresholdMs;

/** The range one selection of words removes: from the first word's start to the last word's end,
 * with the silences between them. */
export function selectionRange(words: readonly TranscriptWord[], from: number, to: number): TrimRange | null {
  const a = words[Math.min(from, to)];
  const b = words[Math.max(from, to)];
  return a && b ? { startMs: a.startMs, endMs: Math.max(b.endMs, a.startMs + 1) } : null;
}

/** "Cut all" on the filler words: every filler word the cuts keep. */
export function fillerCuts(words: readonly TranscriptWord[], cuts: readonly TrimRange[]): TrimRange[] {
  const cut = wordsCut(words, cuts);
  const add = words.filter((w, i) => w.filler && !cut[i]).map((w) => ({ startMs: w.startMs, endMs: w.endMs }));
  return addRanges(cuts, add);
}

/** The index of the last word that starts at or before `ms`, or -1. */
export function wordBefore(words: readonly TranscriptWord[], ms: number): number {
  let lo = 0;
  let hi = words.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (words[mid]!.startMs <= ms) lo = mid + 1;
    else hi = mid;
  }
  return lo - 1;
}

const middle = (r: { startMs: number; endMs: number }) => (r.startMs + r.endMs) / 2;

/** Whether the cut also removes speech next to `[fromMs, toMs)`: a word that is not a filler, on
 * either side, whose middle lies inside the cut. Such a cut was made by hand (a sentence), and
 * putting the fillers or the silences back must not punch a hole into it. */
function holdsSpeech(words: readonly TranscriptWord[], cut: TrimRange, fromMs: number, toMs: number): boolean {
  let i = wordBefore(words, fromMs - 1);
  while (i >= 0 && words[i]!.filler && middle(words[i]!) >= cut.startMs) i--;
  if (i >= 0 && !words[i]!.filler && middle(words[i]!) >= cut.startMs && middle(words[i]!) < toMs) return true;
  let j = wordBefore(words, toMs - 1) + 1;
  while (j < words.length && words[j]!.filler && middle(words[j]!) < cut.endMs) j++;
  return j < words.length && !words[j]!.filler && middle(words[j]!) < cut.endMs && middle(words[j]!) >= fromMs;
}

/** "Put back" on the filler words: each filler cut is taken out, except where the filler went
 * with a sentence the user cut by hand. */
export function restoreFillers(words: readonly TranscriptWord[], cuts: readonly TrimRange[]): TrimRange[] {
  const holes: TrimRange[] = [];
  for (const w of words) {
    if (!w.filler) continue;
    const cut = cutAt(cuts, middle(w));
    if (cut && !holdsSpeech(words, cut, w.startMs, w.endMs)) holes.push({ startMs: w.startMs, endMs: w.endMs });
  }
  return subtractRanges(cuts, holes);
}

/** "Shorten" on the silences at least `thresholdMs` long that are still whole: each one keeps
 * `SILENCE_KEEP_MS` (its middle goes) or, with `shorten` off, goes whole. */
export function silenceCuts(
  silences: readonly TranscriptSilence[],
  cuts: readonly TrimRange[],
  thresholdMs: number,
  shorten: boolean,
): TrimRange[] {
  const states = silenceStates(silences, cuts);
  const add: TrimRange[] = [];
  silences.forEach((s, i) => {
    if (!isLong(s, thresholdMs) || states[i]!.state !== "none") return;
    if (!shorten) add.push({ startMs: s.startMs, endMs: s.endMs });
    else if (s.endMs - s.startMs - SILENCE_KEEP_MS >= MIN_SHORTEN_MS) {
      add.push({ startMs: s.startMs + SILENCE_KEEP_MS / 2, endMs: s.endMs - SILENCE_KEEP_MS / 2 });
    }
  });
  return addRanges(cuts, add);
}

/** "Put back" on the long silences: every cut inside one goes, except where the silence went
 * with speech the user cut by hand. */
export function restoreSilences(
  words: readonly TranscriptWord[],
  silences: readonly TranscriptSilence[],
  cuts: readonly TrimRange[],
  thresholdMs: number,
): TrimRange[] {
  const holes: TrimRange[] = [];
  for (const s of silences) {
    if (!isLong(s, thresholdMs)) continue;
    const cut = cutAt(cuts, middle(s));
    if (cut && !holdsSpeech(words, cut, s.startMs, s.endMs)) holes.push({ startMs: s.startMs, endMs: s.endMs });
  }
  return subtractRanges(cuts, holes);
}

// ---------------------------------------------------------------------------- undo, redo

/** The cut lists before each edit (undo) and after each undone one (redo). */
export class CutHistory {
  private past: TrimRange[][] = [];
  private future: TrimRange[][] = [];
  /** A long session keeps this many steps; the oldest go first. */
  private static readonly LIMIT = 200;

  /** Remembers `before`, the list an edit replaces; a new edit forgets what was undone. */
  push(before: readonly TrimRange[]): void {
    this.past.push(before.map((c) => ({ ...c })));
    if (this.past.length > CutHistory.LIMIT) this.past.shift();
    this.future = [];
  }

  undo(current: readonly TrimRange[]): TrimRange[] | null {
    const before = this.past.pop();
    if (!before) return null;
    this.future.push(current.map((c) => ({ ...c })));
    return before;
  }

  redo(current: readonly TrimRange[]): TrimRange[] | null {
    const after = this.future.pop();
    if (!after) return null;
    this.past.push(current.map((c) => ({ ...c })));
    return after;
  }

  clear(): void {
    this.past = [];
    this.future = [];
  }
}

/** Whether two cut lists remove the same ranges. */
export function sameCuts(a: readonly TrimRange[], b: readonly TrimRange[]): boolean {
  return a.length === b.length && a.every((c, i) => c.startMs === b[i]!.startMs && c.endMs === b[i]!.endMs);
}
