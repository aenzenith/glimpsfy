// Where an object is and how it changes shape: its bounds, the handles it offers, whether a point
// hits it, and the object after a move or a handle drag. Everything is in image pixels; the
// surfaces convert the pointer before they ask. Text needs its laid-out size, which only a canvas
// can measure, so the functions that need it take a `TextSize` from the renderer.

import type { AnnotationObject, Point, Rect } from "../lib/annotate";
import { loupeBox, magnifyPart, sourceBox } from "./magnify";
import { MAGNIFY_FRAME, MAGNIFY_SHADOW, arrowMetrics } from "./style";

export type TextSize = (object: Extract<AnnotationObject, { kind: "text" }>) => { width: number; height: number };

/** A handle: the eight of a box (`fx`, `fy` in 0, 0.5, 1 along each side) or an endpoint. */
export type Handle = { id: string; x: number; y: number; kind: "corner" | "x" | "y" | "point" };

export const BOX_HANDLES: [number, number, Handle["kind"]][] = [
  [0, 0, "corner"],
  [0.5, 0, "x"],
  [1, 0, "corner"],
  [1, 0.5, "y"],
  [1, 1, "corner"],
  [0.5, 1, "x"],
  [0, 1, "corner"],
  [0, 0.5, "y"],
];

export const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));

/** The rectangle with two corners at `a` and `b`. */
export function rectFrom(a: Point, b: Point): Rect {
  return { x: Math.min(a.x, b.x), y: Math.min(a.y, b.y), width: Math.abs(b.x - a.x), height: Math.abs(b.y - a.y) };
}

export function inflate(r: Rect, by: number): Rect {
  return { x: r.x - by, y: r.y - by, width: r.width + 2 * by, height: r.height + 2 * by };
}

export function contains(r: Rect, p: Point): boolean {
  return p.x >= r.x && p.y >= r.y && p.x <= r.x + r.width && p.y <= r.y + r.height;
}

function pointsBox(points: readonly Point[]): Rect {
  let x0 = Infinity;
  let y0 = Infinity;
  let x1 = -Infinity;
  let y1 = -Infinity;
  for (const p of points) {
    x0 = Math.min(x0, p.x);
    y0 = Math.min(y0, p.y);
    x1 = Math.max(x1, p.x);
    y1 = Math.max(y1, p.y);
  }
  return { x: x0, y: y0, width: x1 - x0, height: y1 - y0 };
}

/** The box an object's handles sit on: the rectangle of a box shape, the box of a path's points,
 * the text block, the step disc, the magnifier's ring. Strokes are not included. */
export function frameOf(object: AnnotationObject, textSize: TextSize): Rect {
  switch (object.kind) {
    case "arrow":
    case "line":
      return rectFrom(object.from, object.to);
    case "pen":
    case "highlighter":
      return pointsBox(object.points);
    case "text": {
      const size = textSize(object);
      return { x: object.origin.x, y: object.origin.y, width: size.width, height: size.height };
    }
    case "step":
      return { x: object.center.x - object.diameter / 2, y: object.center.y - object.diameter / 2, width: object.diameter, height: object.diameter };
    case "magnify":
      return sourceBox(object);
    default:
      return object.rect;
  }
}

function union(a: Rect, b: Rect): Rect {
  return rectFrom({ x: Math.min(a.x, b.x), y: Math.min(a.y, b.y) }, { x: Math.max(a.x + a.width, b.x + b.width), y: Math.max(a.y + a.height, b.y + b.height) });
}

/** Everything the object paints, strokes and heads included, for the object bar's place and for
 * redraw bounds. */
export function boundsOf(object: AnnotationObject, textSize: TextSize): Rect {
  const frame = frameOf(object, textSize);
  switch (object.kind) {
    case "arrow": {
      const m = arrowMetrics(object.stroke.width, Math.hypot(object.to.x - object.from.x, object.to.y - object.from.y));
      return inflate(frame, m.headHalf + 2);
    }
    case "line":
    case "pen":
    case "rectangle":
    case "ellipse":
      return inflate(frame, object.stroke.width / 2 + 1);
    case "highlighter":
      return inflate(frame, object.width / 2 + 1);
    case "magnify": {
      // The ring's stroke, the loupe's frame and its shadow are sized in points, and the bounds
      // know no scale, so they leave room for them on a 2x capture.
      const room = 2 * (MAGNIFY_FRAME + MAGNIFY_SHADOW.blur + MAGNIFY_SHADOW.y);
      return union(inflate(frame, 2 * MAGNIFY_FRAME), inflate(loupeBox(object), room));
    }
    default:
      return frame;
  }
}

/** The handles of an object: two endpoints for an arrow or a line (they are rotated by their
 * ends), eight for a shape that can be resized as a box, the four corners of a magnifier's ring,
 * which stays round, and none for a step marker or text, which change size through the strip. */
export function handlesOf(object: AnnotationObject, textSize: TextSize): Handle[] {
  switch (object.kind) {
    case "arrow":
    case "line":
      return [
        { id: "from", x: object.from.x, y: object.from.y, kind: "point" },
        { id: "to", x: object.to.x, y: object.to.y, kind: "point" },
      ];
    case "step":
    case "text":
      return [];
    case "magnify": {
      const r = sourceBox(object);
      return BOX_HANDLES.filter(([, , kind]) => kind === "corner").map(([fx, fy]) => ({ id: `${fx},${fy}`, x: r.x + fx * r.width, y: r.y + fy * r.height, kind: "corner" }));
    }
    default: {
      const r = frameOf(object, textSize);
      return BOX_HANDLES.map(([fx, fy, kind]) => ({ id: `${fx},${fy}`, x: r.x + fx * r.width, y: r.y + fy * r.height, kind }));
    }
  }
}

function distanceToSegment(p: Point, a: Point, b: Point): number {
  const dx = b.x - a.x;
  const dy = b.y - a.y;
  const length = dx * dx + dy * dy;
  const t = length === 0 ? 0 : clamp(((p.x - a.x) * dx + (p.y - a.y) * dy) / length, 0, 1);
  return Math.hypot(p.x - (a.x + t * dx), p.y - (a.y + t * dy));
}

function distanceToPath(p: Point, points: readonly Point[]): number {
  if (points.length === 1) return Math.hypot(p.x - points[0].x, p.y - points[0].y);
  let best = Infinity;
  for (let i = 1; i < points.length; i++) best = Math.min(best, distanceToSegment(p, points[i - 1], points[i]));
  return best;
}

function nearRectEdge(r: Rect, p: Point, reach: number): boolean {
  if (!contains(inflate(r, reach), p)) return false;
  return !contains(inflate(r, -reach), p) || r.width < 2 * reach || r.height < 2 * reach;
}

/**
 * Whether `p` hits `object`. `reach` is how far from a stroke still counts (the pointer's
 * tolerance in image pixels). With `interior`, a click inside an unfilled shape, a redaction or a
 * spotlight counts too; without it, only their edge does, so a drawing tool can start a new
 * drawing inside a rectangle or on a blurred area.
 */
export function hits(object: AnnotationObject, p: Point, reach: number, interior: boolean, textSize: TextSize): boolean {
  switch (object.kind) {
    case "arrow":
    case "line":
      return distanceToSegment(p, object.from, object.to) <= reach + object.stroke.width / 2 + (object.kind === "arrow" ? 2 : 0);
    case "pen":
      return distanceToPath(p, object.points) <= reach + object.stroke.width / 2;
    case "highlighter":
      return distanceToPath(p, object.points) <= reach + object.width / 2;
    case "rectangle":
      if (interior || object.fill) return contains(inflate(object.rect, reach + object.stroke.width / 2), p);
      return nearRectEdge(object.rect, p, reach + object.stroke.width / 2);
    case "ellipse": {
      const r = object.rect;
      const rx = r.width / 2;
      const ry = r.height / 2;
      if (rx <= 0 || ry <= 0) return contains(inflate(r, reach), p);
      const cx = r.x + rx;
      const cy = r.y + ry;
      // The normalised radius approximates the distance to the outline well enough to pick it.
      const d = Math.hypot((p.x - cx) / rx, (p.y - cy) / ry);
      const band = (reach + object.stroke.width / 2) / Math.min(rx, ry);
      return interior || object.fill ? d <= 1 + band : Math.abs(d - 1) <= band;
    }
    case "text":
      return contains(inflate(frameOf(object, textSize), reach / 2), p);
    case "step":
      return Math.hypot(p.x - object.center.x, p.y - object.center.y) <= object.diameter / 2 + reach / 2;
    case "magnify":
      return magnifyPart(object, p, reach, interior) !== null;
    case "blur":
    case "pixelate":
    case "spotlight":
      // With a tool, only the inner band of the edge picks the area. Redacting line after line
      // starts each drag just outside the last area, and an outer band would move that area off
      // the text it hides instead of drawing the next one.
      return interior ? contains(inflate(object.rect, reach / 2), p) : contains(object.rect, p) && nearRectEdge(object.rect, p, reach);
    case "crop":
      return false;
  }
}

/** The object moved by `dx`, `dy`. */
export function moved(object: AnnotationObject, dx: number, dy: number): AnnotationObject {
  const at = (p: Point) => ({ x: p.x + dx, y: p.y + dy });
  switch (object.kind) {
    case "arrow":
    case "line":
      return { ...object, from: at(object.from), to: at(object.to) };
    case "pen":
    case "highlighter":
      return { ...object, points: object.points.map(at) };
    case "text":
      return { ...object, origin: at(object.origin) };
    case "step":
      return { ...object, center: at(object.center) };
    case "magnify":
      return { ...object, source: at(object.source), at: at(object.at) };
    default:
      return { ...object, rect: { ...object.rect, x: object.rect.x + dx, y: object.rect.y + dy } };
  }
}

/** Snaps the direction from `a` to `b` to the nearest multiple of 45°, for a ⇧ drag. */
export function snapAngle(a: Point, b: Point): Point {
  const length = Math.hypot(b.x - a.x, b.y - a.y);
  const angle = Math.round(Math.atan2(b.y - a.y, b.x - a.x) / (Math.PI / 4)) * (Math.PI / 4);
  return { x: a.x + Math.cos(angle) * length, y: a.y + Math.sin(angle) * length };
}

/** The rectangle from `anchor` to `p`, kept at `ratio` (width / height) when one is given. */
export function boxTo(anchor: Point, p: Point, ratio: number | null): Rect {
  if (!ratio) return rectFrom(anchor, p);
  const dx = p.x - anchor.x;
  const dy = p.y - anchor.y;
  let w = Math.abs(dx);
  let h = Math.abs(dy);
  if (w / Math.max(h, 1e-6) > ratio) h = w / ratio;
  else w = h * ratio;
  return rectFrom(anchor, { x: anchor.x + Math.sign(dx || 1) * w, y: anchor.y + Math.sign(dy || 1) * h });
}

/**
 * The object after dragging handle `id` to `p`, starting from `origin` (the object when the drag
 * began). An endpoint moves that end; a box handle moves its edges, and the opposite corner or
 * edge stays put. A path is scaled with its box. A magnifier's ring keeps its centre and reaches
 * the handle; its loupe keeps its own centre and grows with the ring.
 */
export function resized(origin: AnnotationObject, id: string, p: Point, ratio: number | null, textSize: TextSize): AnnotationObject {
  if (origin.kind === "arrow" || origin.kind === "line") {
    return id === "from" ? { ...origin, from: p } : { ...origin, to: p };
  }
  if (origin.kind === "magnify") {
    return { ...origin, radius: Math.max(Math.abs(p.x - origin.source.x), Math.abs(p.y - origin.source.y)) };
  }
  const box = frameOf(origin, textSize);
  const [fx, fy] = id.split(",").map(Number);
  let x0 = box.x;
  let y0 = box.y;
  let x1 = box.x + box.width;
  let y1 = box.y + box.height;
  if (fx === 0) x0 = p.x;
  if (fx === 1) x1 = p.x;
  if (fy === 0) y0 = p.y;
  if (fy === 1) y1 = p.y;
  let next = rectFrom({ x: x0, y: y0 }, { x: x1, y: y1 });
  if (ratio && fx !== 0.5 && fy !== 0.5) {
    const anchor = { x: fx === 0 ? box.x + box.width : box.x, y: fy === 0 ? box.y + box.height : box.y };
    next = boxTo(anchor, p, ratio);
  }
  switch (origin.kind) {
    case "pen":
    case "highlighter": {
      const sx = box.width > 0 ? next.width / box.width : 1;
      const sy = box.height > 0 ? next.height / box.height : 1;
      // A handle dragged past the opposite side mirrors the path, as it mirrors a box.
      const flipX = fx === 0 ? p.x > box.x + box.width : fx === 1 ? p.x < box.x : false;
      const flipY = fy === 0 ? p.y > box.y + box.height : fy === 1 ? p.y < box.y : false;
      const map = (q: Point) => ({
        x: flipX ? next.x + next.width - (q.x - box.x) * sx : next.x + (q.x - box.x) * sx,
        y: flipY ? next.y + next.height - (q.y - box.y) * sy : next.y + (q.y - box.y) * sy,
      });
      return { ...origin, points: origin.points.map(map) };
    }
    case "rectangle":
    case "ellipse":
    case "blur":
    case "pixelate":
    case "spotlight":
    case "crop":
      return { ...origin, rect: next };
    default:
      return origin;
  }
}
