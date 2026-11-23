// The history's Combine without Vue: where each capture lies on the combined image (the layout
// engine of design/canvas row 13), its scale and its limits. The sheet's preview and the request
// to `editor_combine` both come from one `combine()` result, so the preview is the image.
//
// Captures are laid out in points, the size they had on screen, so a capture from a 1x display
// shows as large as its Retina neighbour. The combined image takes the highest scale among them
// and the core resizes the others up to it.

import type { CombineBackground, CombineRequest } from "../lib/api/render";
import type { Entry, EntryId } from "../lib/api/history";

export const LAYOUTS = ["row", "col", "grid", "free"] as const;
export type Layout = (typeof LAYOUTS)[number];
export const GAPS = [0, 8, 16, 32] as const;
export type Gap = (typeof GAPS)[number];
export const ALIGNS = ["start", "center", "end"] as const;
export type Align = (typeof ALIGNS)[number];
export const BACKGROUNDS = ["clear", "white", "gray", "dark"] as const satisfies readonly CombineBackground[];

/** The colours the preview paints under the pieces; the core uses the same values. */
export const BACKGROUND_CSS: Record<CombineBackground, string | null> = { clear: null, white: "#ffffff", gray: "#e9e9ee", dark: "#1c1c1e" };

/** The longest side a combined image may have (`COMBINE_MAX_SIDE` in `commands/render.rs`). */
export const MAX_SIDE = 16_384;

export type Settings = { layout: Layout; gap: Gap; align: Align; background: CombineBackground };

/** The sheet's last settings, kept while the history window stays open and never written to the
 * settings file. */
export const remembered: Settings = { layout: "row", gap: 16, align: "start", background: "white" };

/** One capture to lay out: its size in points and its pixels per point. */
export type Piece = { id: EntryId; w: number; h: number; scale: number };

export type Rect = { x: number; y: number; w: number; h: number };

/** Where a capture may enter the combine, or why it may not. */
export function pieceOf(entry: Entry): Piece | null {
  if (entry.kind !== "screenshot" || !entry.size?.width || !entry.size.height) return null;
  const scale = entry.scale && entry.scale > 0 ? entry.scale : 1;
  return { id: entry.id, w: entry.size.width / scale, h: entry.size.height / scale, scale };
}

/** Places the pieces in points; the gap also runs round the outside. In the free layout each
 * piece lies where `free` puts it and the image is their bounding box, without a gap. */
export function arrange(pieces: readonly Piece[], s: Settings, free?: ReadonlyMap<EntryId, { x: number; y: number }>): { width: number; height: number; rects: Rect[] } {
  const g = s.gap;
  const off = (room: number, size: number) => (s.align === "center" ? (room - size) / 2 : s.align === "end" ? room - size : 0);
  const rects: Rect[] = [];
  if (!pieces.length) return { width: 0, height: 0, rects };
  if (s.layout === "free") {
    const at = pieces.map((p, i) => free?.get(p.id) ?? { x: i * 24, y: i * 24 });
    const minX = Math.min(...at.map((a) => a.x));
    const minY = Math.min(...at.map((a) => a.y));
    pieces.forEach((p, i) => rects.push({ x: at[i].x - minX, y: at[i].y - minY, w: p.w, h: p.h }));
    return { width: Math.max(...rects.map((r) => r.x + r.w)), height: Math.max(...rects.map((r) => r.y + r.h)), rects };
  }
  if (s.layout === "col") {
    const mw = Math.max(...pieces.map((p) => p.w));
    let y: number = g;
    for (const p of pieces) {
      rects.push({ x: g + off(mw, p.w), y, w: p.w, h: p.h });
      y += p.h + g;
    }
    return { width: mw + 2 * g, height: y, rects };
  }
  if (s.layout === "grid") {
    const cols = Math.ceil(Math.sqrt(pieces.length));
    const rows = Math.ceil(pieces.length / cols);
    const cw = Array<number>(cols).fill(0);
    const rh = Array<number>(rows).fill(0);
    pieces.forEach((p, i) => {
      cw[i % cols] = Math.max(cw[i % cols], p.w);
      rh[Math.floor(i / cols)] = Math.max(rh[Math.floor(i / cols)], p.h);
    });
    const xs: number[] = [g];
    const ys: number[] = [g];
    cw.forEach((w, i) => xs.push(xs[i] + w + g));
    rh.forEach((h, i) => ys.push(ys[i] + h + g));
    pieces.forEach((p, i) => {
      const c = i % cols;
      const r = Math.floor(i / cols);
      rects.push({ x: xs[c] + (cw[c] - p.w) / 2, y: ys[r] + off(rh[r], p.h), w: p.w, h: p.h });
    });
    return { width: xs[cols], height: ys[rows], rects };
  }
  const mh = Math.max(...pieces.map((p) => p.h));
  let x: number = g;
  for (const p of pieces) {
    rects.push({ x, y: g + off(mh, p.h), w: p.w, h: p.h });
    x += p.w + g;
  }
  return { width: x, height: mh + 2 * g, rects };
}

export type Combined = {
  /** The layout in points, for the preview. */
  points: { width: number; height: number; rects: Rect[] };
  /** The request for `editor_combine`, in the combined image's pixels. */
  request: CombineRequest;
  /** The scale the pieces asked for before the size limit, so the sheet can say it dropped. */
  wanted: number;
  /** The pieces drawn at a lower scale than the image, which the core resizes up. */
  upscaled: Piece[];
  /** Past `MAX_SIDE` even at 1x: the image cannot be made. */
  tooLarge: boolean;
};

/** The combined image: the highest scale of the pieces (1 without any), or 1x when that scale
 * would take a side past `MAX_SIDE`. */
export function combine(pieces: readonly Piece[], s: Settings, free?: ReadonlyMap<EntryId, { x: number; y: number }>): Combined {
  const points = arrange(pieces, s, free);
  const wanted = Math.max(1, ...pieces.map((p) => p.scale));
  const side = Math.max(points.width, points.height);
  const scale = Math.round(side * wanted) > MAX_SIDE ? 1 : wanted;
  const px = (v: number) => Math.round(v * scale);
  const request: CombineRequest = {
    width: Math.max(1, px(points.width)),
    height: Math.max(1, px(points.height)),
    scale,
    background: s.background,
    pieces: pieces.map((p, i) => {
      const r = points.rects[i];
      return { entryId: p.id, x: px(r.x), y: px(r.y), width: Math.max(1, px(r.w)), height: Math.max(1, px(r.h)) };
    }),
  };
  return {
    points,
    request,
    wanted,
    upscaled: pieces.filter((p) => p.scale < scale - 1e-6),
    tooLarge: Math.round(side) > MAX_SIDE,
  };
}

/** `list` with the item at `from` moved to `to`. */
export function moved<T>(list: readonly T[], from: number, to: number): T[] {
  const next = list.slice();
  const [item] = next.splice(from, 1);
  next.splice(Math.max(0, Math.min(next.length, to)), 0, item);
  return next;
}

/** A scale as the sheet writes it: "2x", "1.5x". */
export function scaleText(scale: number): string {
  return `${Math.round(scale * 100) / 100}x`;
}
