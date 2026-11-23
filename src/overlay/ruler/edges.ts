// Snapping for the ruler: the frozen frame's pixels are scanned on the device, from the pointer
// along the measure, for the nearest boundary between two neighbouring pixels whose largest
// channel difference passes the threshold. The scan keeps to the pixels the measure covers, so a
// rounded corner or a glyph beside the line cannot pull an end off the edge the user aimed at.
// Nothing leaves the page and nothing is written.

import type { Measure, Point, Rest, Shape } from "./measure";

/** The frame's pixels, read a rectangle at a time. */
export type PixelSource = {
  width: number;
  height: number;
  /** RGBA rows holding (`x`, `y`)–(`x + w`, `y + h`); the region may hold more. */
  region(x: number, y: number, w: number, h: number): Region;
};
export type Region = { data: Uint8ClampedArray; stride: number; x: number; y: number };

/** The frame's pixels from the kept RGBA buffer, or from the canvas when the frame came encoded. */
export function pixelSource(ctx: CanvasRenderingContext2D, pixels: Uint8ClampedArray | null): PixelSource {
  const { width, height } = ctx.canvas;
  return {
    width,
    height,
    region(x, y, w, h) {
      if (pixels) return { data: pixels, stride: width, x: 0, y: 0 };
      return { data: ctx.getImageData(x, y, w, h).data, stride: w, x, y };
    },
  };
}

/** The largest channel difference of pixels (`x0`, `y0`) and (`x1`, `y1`), alpha left out. */
function difference(r: Region, x0: number, y0: number, x1: number, y1: number): number {
  const i = ((y0 - r.y) * r.stride + (x0 - r.x)) * 4;
  const j = ((y1 - r.y) * r.stride + (x1 - r.x)) * 4;
  const d = r.data;
  return Math.max(Math.abs(d[i] - d[j]), Math.abs(d[i + 1] - d[j + 1]), Math.abs(d[i + 2] - d[j + 2]));
}

/**
 * The boundary on `axis` nearest to `at` (a frame coordinate) within `reach` frame pixels where,
 * on any row (or column) from `from` to `to` across it, two neighbours differ by more than
 * `threshold`; `null` when there is none. The frame's own edges count as edges.
 */
export function nearestEdge(src: PixelSource, axis: "x" | "y", at: number, from: number, to: number, reach: number, threshold: number): number | null {
  const size = axis === "x" ? src.width : src.height;
  const other = axis === "x" ? src.height : src.width;
  const lo = Math.max(0, Math.ceil(at - reach));
  const hi = Math.min(size, Math.floor(at + reach));
  if (lo > hi) return null;
  const candidates: number[] = [];
  for (let k = lo; k <= hi; k++) candidates.push(k);
  candidates.sort((p, q) => Math.abs(p - at) - Math.abs(q - at));
  const a = Math.max(0, Math.min(from, to));
  const b = Math.min(other - 1, Math.max(from, to));
  // The pixels on both sides of every candidate, read once.
  const p0 = Math.max(0, lo - 1);
  const p1 = Math.min(size - 1, hi);
  const region = b >= a ? (axis === "x" ? src.region(p0, a, p1 - p0 + 1, b - a + 1) : src.region(a, p0, b - a + 1, p1 - p0 + 1)) : null;
  for (const k of candidates) {
    if (k === 0 || k === size) return k;
    if (!region) continue;
    for (let j = a; j <= b; j++) {
      const d = axis === "x" ? difference(region, k - 1, j, k, j) : difference(region, j, k - 1, j, k);
      if (d > threshold) return k;
    }
  }
  return null;
}

export type SnapOptions = {
  shape: Shape;
  /** ⇧: a line keeps to the axis it runs along more. */
  lock: boolean;
  /** ⌘: no end snaps. */
  free: boolean;
  /** How far an end looks for an edge, in frame pixels (6 pt). */
  reach: number;
  /** The rows (or columns) a 1 pt line covers, in frame pixels. */
  band: number;
  threshold: number;
  /** Whether a point (frame pixels) may snap: not inside a window closed to capture. */
  open: (p: Point) => boolean;
};

const bound = (v: number, size: number) => Math.max(0, Math.min(size, Math.round(v)));

/** The measure a drag from `a` to `b` (frame pixels) draws, its ends snapped as `o` allows. */
export function measure(src: PixelSource, a: Point, b: Point, o: SnapOptions): Measure {
  return o.shape === "box" ? box(src, a, b, o) : line(src, a, b, o);
}

function line(src: PixelSource, rawA: Point, rawB: Point, o: SnapOptions): Measure {
  const a = { ...rawA };
  const b = { ...rawB };
  if (o.lock) {
    if (Math.abs(b.x - a.x) >= Math.abs(b.y - a.y)) b.y = a.y;
    else b.x = a.x;
  }
  const A = { x: bound(a.x, src.width), y: bound(a.y, src.height) };
  const B = { x: bound(b.x, src.width), y: bound(b.y, src.height) };
  const raw = { a: rawA, b: rawB, lock: o.lock, free: o.free };
  if (A.x === B.x && A.y === B.y) return { shape: "line", a: A, b: B, rest: ["free", "free"], raw };
  // Each end slides along the axis the line runs along more, to the nearest edge that crosses
  // the pixels under the line, within the reach on either side of the end.
  const axis = Math.abs(B.x - A.x) >= Math.abs(B.y - A.y) ? "x" : "y";
  const across = axis === "x" ? "y" : "x";
  const rest: Rest[] = [];
  for (const [end, at] of [
    [A, a],
    [B, b],
  ] as const) {
    if (o.free || !o.open(at)) {
      rest.push("free");
      continue;
    }
    const m = end[across];
    const k = nearestEdge(src, axis, at[axis], m, m + o.band - 1, o.reach, o.threshold);
    if (k === null) {
      rest.push("missed");
    } else {
      end[axis] = k;
      rest.push("snapped");
    }
  }
  // A short drag beside one edge would land both ends on it.
  if (A[axis] === B[axis] && A[across] === B[across]) {
    B[axis] = bound(b[axis], axis === "x" ? src.width : src.height);
    rest[1] = "missed";
  }
  return { shape: "line", a: A, b: B, rest, raw };
}

function box(src: PixelSource, rawA: Point, rawB: Point, o: SnapOptions): Measure {
  const A = { x: bound(rawA.x, src.width), y: bound(rawA.y, src.height) };
  const B = { x: bound(rawB.x, src.width), y: bound(rawB.y, src.height) };
  const raw = { a: rawA, b: rawB, lock: o.lock, free: o.free };
  if (A.x === B.x || A.y === B.y) return { shape: "box", a: A, b: B, rest: ["free", "free", "free", "free"], raw };
  const left = Math.min(A.x, B.x);
  const right = Math.max(A.x, B.x);
  const top = Math.min(A.y, B.y);
  const bottom = Math.max(A.y, B.y);
  // Each side slides across its own axis, scanned through the middle of the side, where an
  // element's edge runs straight and its rounded corners are far away.
  const row = Math.floor((top + bottom) / 2);
  const column = Math.floor((left + right) / 2);
  const side = (axis: "x" | "y", at: number, mid: Point): [number, Rest] => {
    if (o.free || !o.open(mid)) return [Math.round(at), "free"];
    const k = axis === "x" ? nearestEdge(src, "x", at, row, row, o.reach, o.threshold) : nearestEdge(src, "y", at, column, column, o.reach, o.threshold);
    return k === null ? [Math.round(at), "missed"] : [k, "snapped"];
  };
  const [ax, ar] = side("x", rawA.x, { x: rawA.x, y: (rawA.y + rawB.y) / 2 });
  const [ay, ary] = side("y", rawA.y, { x: (rawA.x + rawB.x) / 2, y: rawA.y });
  const [bx, br] = side("x", rawB.x, { x: rawB.x, y: (rawA.y + rawB.y) / 2 });
  const [by, bry] = side("y", rawB.y, { x: (rawA.x + rawB.x) / 2, y: rawB.y });
  // Sides that snapped past each other keep where the drag put them.
  if ((ax - bx) * (A.x - B.x) <= 0 || (ay - by) * (A.y - B.y) <= 0) {
    return { shape: "box", a: A, b: B, rest: ["missed", "missed", "missed", "missed"], raw };
  }
  return {
    shape: "box",
    a: { x: bound(ax, src.width), y: bound(ay, src.height) },
    b: { x: bound(bx, src.width), y: bound(by, src.height) },
    rest: [ar, ary, br, bry],
    raw,
  };
}
