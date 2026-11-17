// The editor's zoom and pan: where the image sits in the canvas and how large (DESIGN.md "Editor
// window", Zoom). Zoom is counted in points, as everywhere on the Mac: 100 % shows a Retina capture
// at the size it had on screen, so `k`, the CSS pixels per image pixel, is zoom / scale.

import type { PixelRect } from "../annotate/redact";
import type { View } from "../annotate/textLayer";

export type ZoomMode = "fit" | "width" | "custom";

/** The zoom menu's steps and the steps of ⌘+ and ⌘−, in per cent. */
export const ZOOM_MENU = [50, 100, 200, 400, 800] as const;
const STEPS = [10, 25, 33, 50, 67, 100, 150, 200, 300, 400, 600, 800, 1200, 1600];
export const MIN_ZOOM = 10;
export const MAX_ZOOM = 1600;

/** From this zoom on, the pixel grid shows and the status bar reads the pixel under the pointer. */
export const GRID_ZOOM = 400;

/** The canvas: its size, and the room the floating bars take at its top. */
export type Room = { width: number; height: number; top: number };

const MARGIN = 24;
/** Fit width leaves this much on each side, for the overview of a tall capture. */
const WIDE_MARGIN = 96;

export const percentOf = (k: number, scale: number) => k * scale * 100;
export const kOf = (percent: number, scale: number) => percent / 100 / scale;

/** The whole of `area` in the room, never above 100 %. */
export function fit(area: PixelRect, room: Room, scale: number): View {
  const k = Math.min((room.width - 2 * MARGIN) / area.width, (room.height - room.top - MARGIN) / area.height, kOf(100, scale));
  return centred(area, room, Math.max(kOf(MIN_ZOOM, scale), k));
}

/** The width of `area` in the room, the top at the top: a tall capture. */
export function fitWidth(area: PixelRect, room: Room, scale: number): View {
  const k = Math.max(kOf(MIN_ZOOM, scale), Math.min((room.width - 2 * WIDE_MARGIN) / area.width, kOf(100, scale)));
  return { k, ox: (room.width - area.width * k) / 2 - area.x * k, oy: room.top - area.y * k };
}

/** `area` at `k`, in the middle of the room under the bars. */
export function centred(area: PixelRect, room: Room, k: number): View {
  return {
    k,
    ox: (room.width - area.width * k) / 2 - area.x * k,
    oy: room.top + (room.height - room.top - MARGIN - area.height * k) / 2 - area.y * k,
  };
}

/** How an image opens: a tall one at fit width, one smaller than the tool bar at 200 %, any other
 * at fit. */
export function opening(area: PixelRect, room: Room, scale: number): { view: View; mode: ZoomMode } {
  const points = { width: area.width / scale, height: area.height / scale };
  if (area.height / area.width > 3) return { view: fitWidth(area, room, scale), mode: "width" };
  if (points.width * 2 < 492 && points.height * 2 < room.height - room.top - MARGIN) {
    return { view: centred(area, room, kOf(200, scale)), mode: "custom" };
  }
  return { view: fit(area, room, scale), mode: "fit" };
}

/** `view` zoomed to `k`, keeping the image point under (`x`, `y`) where it is. */
export function zoomAt(view: View, k: number, x: number, y: number): View {
  const px = (x - view.ox) / view.k;
  const py = (y - view.oy) / view.k;
  return { k, ox: x - px * k, oy: y - py * k };
}

/** The next step of ⌘+ (`+1`) or ⌘− (`-1`) from `percent`. */
export function step(percent: number, by: 1 | -1): number {
  if (by > 0) return STEPS.find((s) => s > percent + 0.5) ?? MAX_ZOOM;
  return [...STEPS].reverse().find((s) => s < percent - 0.5) ?? MIN_ZOOM;
}

/** Keeps at least a quarter of the image in the room, so panning never loses it. */
export function clampPan(view: View, area: PixelRect, room: Room): View {
  const w = area.width * view.k;
  const h = area.height * view.k;
  const keepX = Math.min(w, room.width) / 4;
  const keepY = Math.min(h, room.height) / 4;
  const left = view.ox + area.x * view.k;
  const top = view.oy + area.y * view.k;
  const x = Math.min(room.width - keepX, Math.max(keepX - w, left));
  const y = Math.min(room.height - keepY, Math.max(keepY - h, top));
  return { k: view.k, ox: view.ox + (x - left), oy: view.oy + (y - top) };
}
