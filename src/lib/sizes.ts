// The selection's size presets (DESIGN.md "Overlay", the size menu): the ratios and the sizes the
// size label's menu offers, the sizes the user saved there, and the geometry that holds a
// selection to a ratio or places one of a fixed size. Sizes count frame pixels, as the size label
// does; the geometry works in the page's points, with `k` frame pixels per point on each axis.
// The capture overlay and the recording setup's area both use it.

import { CROP_RATIOS } from "../annotate/style";
import { settings_get, settings_set } from "./api/settings";
import type { Key } from "./i18n";
import type { SavedSize, Settings } from "./settings";

export type Ratio = Exclude<(typeof CROP_RATIOS)[number], "free" | "original">;
/** The menu's ratios, in the order Tab steps through them while dragging. The editor's `original`
 * has no meaning before there is an image, so the menu leaves it out. */
export const RATIOS: readonly Ratio[] = CROP_RATIOS.filter((r): r is Ratio => r !== "free" && r !== "original");

export const PRESET_SIZES: readonly { key: Key; width: number; height: number }[] = [
  { key: "overlay.size.appStore", width: 1280, height: 800 },
  { key: "overlay.size.fullHd", width: 1920, height: 1080 },
  { key: "overlay.size.story", width: 1080, height: 1920 },
];

/** Free, a locked ratio, or a fixed size in frame pixels with the name the label shows. */
export type SizeChoice = { kind: "free" } | { kind: "ratio"; ratio: Ratio } | { kind: "size"; width: number; height: number; name: string };

export const FREE: SizeChoice = { kind: "free" };

/** The most sizes the menu keeps; saving one more lets the oldest go. */
export const MAX_SAVED = 12;

export type Point = { x: number; y: number };
export type Box = { x: number; y: number; w: number; h: number };
export type Area = { w: number; h: number };

const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

/** Width over height. */
export function ratioValue(r: Ratio): number {
  const [w, h] = r.split(":").map(Number);
  return w / h;
}

/** The ratio a choice locks the selection to, or `null`. */
export const lockedRatio = (choice: SizeChoice): number | null =>
  choice.kind === "ratio" ? ratioValue(choice.ratio) : choice.kind === "size" ? choice.width / choice.height : null;

/** The menu's ratio that `width` × `height` has, for the label of a size that had to shrink. */
export function ratioOf(width: number, height: number): Ratio | null {
  return RATIOS.find((r) => Math.abs(ratioValue(r) - width / height) < 0.01) ?? null;
}

/** What the size label shows after its lock: the ratio, or the fixed size's name; a size that
 * shrank to fit the screen shows the ratio it kept when the menu has that ratio. */
export function lockOf(choice: SizeChoice, fitted: boolean): { text: string; name: boolean } | null {
  if (choice.kind === "free") return null;
  if (choice.kind === "ratio") return { text: choice.ratio, name: false };
  const kept = fitted ? ratioOf(choice.width, choice.height) : null;
  return kept ? { text: kept, name: false } : { text: choice.name, name: true };
}

/** Tab's round while dragging: Free, 16:9, 4:3, 1:1, 9:16 and Free again; ⇧Tab goes back. */
export function nextRatio(choice: SizeChoice, back = false): SizeChoice {
  const round: SizeChoice[] = [FREE, ...RATIOS.map((ratio) => ({ kind: "ratio" as const, ratio }))];
  const at = choice.kind === "ratio" ? round.findIndex((c) => c.kind === "ratio" && c.ratio === choice.ratio) : 0;
  return round[(at + (back ? round.length - 1 : 1)) % round.length];
}

// ------------------------------------------------------------------------------- geometry

/** A pixel ratio as a ratio of points. */
const inPoints = (ratio: number, k: Point) => (ratio * k.y) / k.x;

/** The box from `anchor` toward `p` at `ratio` (frame pixels), as large as the pointer asks and as
 * the screen allows on that side; the free corner leaves the pointer. */
export function ratioBox(anchor: Point, p: Point, ratio: number, view: Area, k: Point): Box {
  const rp = inPoints(ratio, k);
  const sx = p.x >= anchor.x ? 1 : -1;
  const sy = p.y >= anchor.y ? 1 : -1;
  let w = Math.abs(p.x - anchor.x);
  let h = Math.abs(p.y - anchor.y);
  if (w / Math.max(h, 1e-6) > rp) h = w / rp;
  else w = h * rp;
  const roomW = sx > 0 ? view.w - anchor.x : anchor.x;
  const roomH = sy > 0 ? view.h - anchor.y : anchor.y;
  if (w > roomW) [w, h] = [roomW, roomW / rp];
  if (h > roomH) [w, h] = [roomH * rp, roomH];
  return { x: sx > 0 ? anchor.x : anchor.x - w, y: sy > 0 ? anchor.y : anchor.y - h, w, h };
}

/** `origin` resized by handle `[fx, fy]` to `p` at `ratio`: a corner keeps the opposite corner, an
 * edge keeps the opposite edge and the centre across it. */
export function ratioResize(origin: Box, [fx, fy]: [number, number], p: Point, ratio: number, view: Area, k: Point): Box {
  if (fx !== 0.5 && fy !== 0.5) {
    const anchor = { x: fx === 0 ? origin.x + origin.w : origin.x, y: fy === 0 ? origin.y + origin.h : origin.y };
    return ratioBox(anchor, p, ratio, view, k);
  }
  const rp = inPoints(ratio, k);
  if (fx === 0.5) {
    // The top or the bottom edge: the height follows the pointer, the width follows the height.
    const edge = fy === 0 ? origin.y + origin.h : origin.y;
    let h = Math.min(Math.abs(p.y - edge), fy === 0 ? edge : view.h - edge);
    let w = h * rp;
    if (w > view.w) [w, h] = [view.w, view.w / rp];
    const x = clamp(origin.x + origin.w / 2 - w / 2, 0, view.w - w);
    return { x, y: fy === 0 ? edge - h : edge, w, h };
  }
  const edge = fx === 0 ? origin.x + origin.w : origin.x;
  let w = Math.min(Math.abs(p.x - edge), fx === 0 ? edge : view.w - edge);
  let h = w / rp;
  if (h > view.h) [w, h] = [view.h * rp, view.h];
  const y = clamp(origin.y + origin.h / 2 - h / 2, 0, view.h - h);
  return { x: fx === 0 ? edge - w : edge, y, w, h };
}

/** The largest box of `ratio` inside `r`, around its centre, as the crop strip reshapes a crop. */
export function ratioInside(r: Box, ratio: number, k: Point): Box {
  const rp = inPoints(ratio, k);
  let w = r.w;
  let h = w / rp;
  if (h > r.h) [w, h] = [r.h * rp, r.h];
  return { x: r.x + (r.w - w) / 2, y: r.y + (r.h - h) / 2, w, h };
}

/**
 * A box of `width` × `height` frame pixels with its top-left corner at `at`. It moves inside the
 * screen when it would cross an edge, and only when it is larger than the screen it shrinks,
 * keeping its ratio; it never grows. Its edges land on the frame's pixel grid, so the core's
 * rounding (`selection_to_frame`) crops exactly the size the box was given.
 */
export function placeSize(at: Point, width: number, height: number, view: Area, k: Point): { box: Box; width: number; height: number; fitted: boolean } {
  const maxW = Math.round(view.w * k.x);
  const maxH = Math.round(view.h * k.y);
  let w = width;
  let h = height;
  const fitted = w > maxW || h > maxH;
  if (fitted) {
    if (w / maxW >= h / maxH) [w, h] = [maxW, Math.max(1, Math.round((height * maxW) / width))];
    else [w, h] = [Math.max(1, Math.round((width * maxH) / height)), maxH];
  }
  const x = clamp(Math.round(at.x * k.x), 0, maxW - w);
  const y = clamp(Math.round(at.y * k.y), 0, maxH - h);
  return { box: { x: x / k.x, y: y / k.y, w: w / k.x, h: h / k.y }, width: w, height: h, fitted };
}

/** `at` on the frame's pixel grid, so a moved box of a fixed size keeps every pixel of it. */
export const onGrid = (v: number, k: number) => Math.round(v * k) / k;

// --------------------------------------------------------------------------- saved sizes

/** The entries of `general.savedSizes` this version can read; the others stay in the file. */
export function readSavedSizes(settings: Settings): SavedSize[] {
  const list = Array.isArray(settings.general?.savedSizes) ? settings.general.savedSizes : [];
  return list.filter(readable);
}

function readable(entry: unknown): entry is SavedSize {
  if (typeof entry !== "object" || entry === null) return false;
  const e = entry as Partial<SavedSize>;
  const whole = (n: unknown) => Number.isInteger(n) && (n as number) > 0 && (n as number) <= 0xffffffff;
  return typeof e.name === "string" && whole(e.width) && whole(e.height);
}

export async function loadSavedSizes(): Promise<SavedSize[]> {
  try {
    return readSavedSizes(await settings_get());
  } catch (error) {
    console.warn("[glimpsfy] the saved sizes were not read", error);
    return [];
  }
}

/** Changes the list in the file as it is now, so a change another window made in the meantime is
 * kept, and returns the readable entries after the change. */
async function changeSaved(change: (list: unknown[]) => void): Promise<SavedSize[]> {
  const settings = await settings_get();
  const list = Array.isArray(settings.general.savedSizes) ? [...settings.general.savedSizes] : [];
  change(list);
  const next = { ...settings, general: { ...settings.general, savedSizes: list } };
  await settings_set(next);
  return readSavedSizes(next);
}

/** Saves `width` × `height` under `name`; a size of the same name takes the new numbers. With the
 * list full, the oldest size goes. */
export function saveSize(name: string, width: number, height: number): Promise<SavedSize[]> {
  return changeSaved((list) => {
    const same = list.findIndex((e) => readable(e) && e.name === name);
    if (same >= 0) {
      list[same] = { ...(list[same] as SavedSize), width, height };
      return;
    }
    list.push({ name, width, height });
    while (list.filter(readable).length > MAX_SAVED) list.splice(list.findIndex(readable), 1);
  });
}

/** Deletes the saved size that has `size`'s name and numbers. */
export function removeSavedSize(size: SavedSize): Promise<SavedSize[]> {
  return changeSaved((list) => {
    const at = list.findIndex((e) => readable(e) && e.name === size.name && e.width === size.width && e.height === size.height);
    if (at >= 0) list.splice(at, 1);
  });
}
