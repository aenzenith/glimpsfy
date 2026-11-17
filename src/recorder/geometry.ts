// Geometry the recorder's floating pages share: rectangles in local points, the eight handles of
// an area, and the keystroke overlay's placement rule (DESIGN.md "Keystroke overlay").

import type { LogicalPoint, LogicalRect } from "../lib/api/capture";
import type { KeystrokePosition } from "../lib/api/record";

export type Point = LogicalPoint;
export type Rect = LogicalRect;

export const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

/** CSS for an absolutely placed box. */
export const box = (r: Rect) => ({ left: `${r.x}px`, top: `${r.y}px`, width: `${r.width}px`, height: `${r.height}px` });

export const inside = (r: Rect, p: Point) => p.x >= r.x && p.y >= r.y && p.x < r.x + r.width && p.y < r.y + r.height;

/** A global rectangle moved into a window whose top-left corner is `origin`. */
export const toLocal = (r: Rect, origin: Point): Rect => ({ x: r.x - origin.x, y: r.y - origin.y, width: r.width, height: r.height });

/** The handles of an area: their place as fractions of the area, and the kit's handle kind. */
export const HANDLES: readonly [number, number, "x" | "y" | null][] = [
  [0, 0, null],
  [0.5, 0, "x"],
  [1, 0, null],
  [1, 0.5, "y"],
  [1, 1, null],
  [0.5, 1, "x"],
  [0, 1, null],
  [0, 0.5, "y"],
];

export const RESIZE_CURSORS: Record<string, string> = {
  "0,0": "nwse-resize",
  "1,1": "nwse-resize",
  "1,0": "nesw-resize",
  "0,1": "nesw-resize",
  "0.5,0": "ns-resize",
  "0.5,1": "ns-resize",
  "0,0.5": "ew-resize",
  "1,0.5": "ew-resize",
};

/** The handle within `reach` points of `p`, as its fractions. */
export function handleAt(r: Rect, p: Point, reach: number): [number, number] | null {
  for (const [fx, fy] of HANDLES) {
    if (Math.hypot(p.x - (r.x + fx * r.width), p.y - (r.y + fy * r.height)) <= reach) return [fx, fy];
  }
  return null;
}

/** How far the keystroke overlay keeps from the recorded area's edge, and from the bubble and the
 * pointer before it moves. */
export const KEYS_MARGIN = 24;

/**
 * Where the keystroke overlay of size `keys` goes inside an area of size `area`: its default place
 * from `position`, or the opposite vertical side while something in `near` (the bubble's rect, the
 * pointer as a zero-size rect) comes within 24 points of the default place, unless that side is
 * taken too. `lift` raises the bottom place and `drop` lowers the top place.
 */
export function placeKeys(
  area: { width: number; height: number },
  keys: { width: number; height: number },
  position: KeystrokePosition,
  near: readonly Rect[],
  lift: number,
  drop: number,
): Point {
  const m = KEYS_MARGIN;
  const x =
    position === "bottomLeft" ? m : position === "bottomRight" ? area.width - m - keys.width : area.width / 2 - keys.width / 2;
  const low = area.height - m - keys.height - lift;
  const high = m + drop;
  const hits = (y: number) =>
    near.some((a) => a.x < x + keys.width + m && a.x + a.width > x - m && a.y < y + keys.height + m && a.y + a.height > y - m);
  const [home, away] = position === "topCenter" ? [high, low] : [low, high];
  return { x, y: hits(home) && !hits(away) ? away : home };
}
