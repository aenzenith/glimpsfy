// The ruler's measures and how they read: a line's length or a box's size in frame pixels and in
// points, the label's parts and the text ⌘C writes. A measure keeps its ends on the boundaries
// between frame pixels, so a length is a whole number of pixels and a CSS pixel is a point.

import { reactive } from "vue";

export type Shape = "line" | "box";
export type Unit = "px" | "pt";
/** How an end (or a side of a box) came to rest: on an edge the scan found, where the scan found
 * none, or where it was put with snapping off (⌘ held, a window closed to capture, the arrows). */
export type Rest = "snapped" | "missed" | "free";
export type Point = { x: number; y: number };
/** Frame pixels per point on each axis. */
export type Scale = { x: number; y: number };

export type Measure = {
  shape: Shape;
  /** The ends, or a box's opposite corners, in frame pixels on pixel boundaries; `b` is the end
   * the drag released, which the arrows move. */
  a: Point;
  b: Point;
  /** A line's two ends ([a, b]), or a box's four sides ([a.x, a.y, b.x, b.y]). */
  rest: Rest[];
  /** Where the drag went, in frame pixels, so a change of sensitivity can snap the measure again;
   * `null` once the arrows moved it by hand. */
  raw: { a: Point; b: Point; lock: boolean; free: boolean } | null;
};

/** The ruler's choices, which last as long as the warm overlay page and are never saved. */
export const choice = reactive({
  shape: "line" as Shape,
  unit: "px" as Unit,
  /** The edge threshold: the largest channel difference of two neighbours, out of 255, that is
   * still no edge. [ and ] step through `THRESHOLDS`. */
  threshold: 12,
});

/** The thresholds [ and ] step through: ] snaps to fainter edges, [ only to stronger ones. */
export const THRESHOLDS = [2, 4, 8, 12, 16, 24, 32, 48];

/** Whether the display has more than one pixel per point, where the ruler offers points. */
export const scaled = (s: Scale): boolean => Math.max(s.x, s.y) > 1.001;

/** A line that runs along one axis, whose length needs no rounding. */
export const straight = (m: Measure): boolean => m.a.x === m.b.x || m.a.y === m.b.y;

/** A whole number as it is, anything else to one decimal ("120", "120.5"). */
function decimal(v: number): string {
  const r = Math.round(v * 10) / 10;
  return Number.isInteger(r) ? String(r) : r.toFixed(1);
}

/** One reading in one unit: one number for a line, width and height for a box. */
export type Reading = { nums: string[]; unit: Unit };

export function reading(m: Measure, s: Scale, unit: Unit): Reading {
  const dx = Math.abs(m.b.x - m.a.x);
  const dy = Math.abs(m.b.y - m.a.y);
  if (m.shape === "box") {
    return unit === "px" ? { nums: [String(dx), String(dy)], unit } : { nums: [decimal(dx / s.x), decimal(dy / s.y)], unit };
  }
  // A slanted line reads its Euclidean length, rounded to a whole pixel.
  if (unit === "px") return { nums: [String(straight(m) ? dx + dy : Math.round(Math.hypot(dx, dy)))], unit };
  return { nums: [decimal(Math.hypot(dx / s.x, dy / s.y))], unit };
}

/** The label of a measure: the chosen unit first and, where points differ from pixels, the other. */
export function readout(m: Measure, s: Scale, unit: Unit): { main: Reading; alt: Reading | null } {
  if (!scaled(s)) return { main: reading(m, s, "px"), alt: null };
  return { main: reading(m, s, unit), alt: reading(m, s, unit === "px" ? "pt" : "px") };
}

/** A reading as text: "120 px", "96 × 28 px". */
export const text = (r: Reading): string => `${r.nums.join(" × ")} ${r.unit}`;

/** The text ⌘C writes for a measure: its reading in the chosen unit. */
export const copyText = (m: Measure, s: Scale, unit: Unit): string => text(readout(m, s, unit).main);

/** Snapping looked for an edge and found none at any end or side. */
export const noEdge = (m: Measure): boolean => m.rest.length > 0 && m.rest.every((r) => r === "missed");

/** Whether the end `a` or `b` sits on an edge, for its ring. */
export function ringAt(m: Measure, end: "a" | "b"): boolean {
  const [first, second] = end === "a" ? [0, 1] : m.shape === "box" ? [2, 3] : [1, 1];
  return m.rest[first] === "snapped" || m.rest[second] === "snapped";
}

/** The measure with its released end moved by (`dx`, `dy`) frame pixels inside `w` × `h`; the
 * moved end no longer counts as snapped. */
export function nudge(m: Measure, dx: number, dy: number, w: number, h: number): Measure {
  const b = { x: Math.max(0, Math.min(w, m.b.x + dx)), y: Math.max(0, Math.min(h, m.b.y + dy)) };
  const rest = [...m.rest];
  if (m.shape === "box") {
    if (dx) rest[2] = "free";
    if (dy) rest[3] = "free";
  } else {
    rest[1] = "free";
  }
  return { ...m, b, rest, raw: null };
}
