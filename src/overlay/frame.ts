// The frozen frame: fetched from the `glimpsfy-frame` scheme (src-tauri/src/frames.rs), never
// through the IPC bridge, and drawn on a canvas in the frame's own colour space, so the overlay
// shows the captured pixels unchanged.

import type { ColorSpace, OverlayFrame } from "../lib/api/capture";

export type Loaded = {
  /** From the request to the last byte. */
  fetchMs: number;
  /** Turning the bytes into an image. */
  decodeMs: number;
  /** Putting the image on the canvas. */
  drawMs: number;
  bytes: number;
  /** The RGBA pixels, kept for the loupe when the frame came raw. */
  pixels: Uint8ClampedArray | null;
};

/** The 2D context of the canvas inside `host`, in colour space `space`. A canvas keeps the colour
 * space of its first context, so a canvas in another space is replaced by a fresh one. */
export function ensureContext(host: HTMLElement, space: ColorSpace, className: string): CanvasRenderingContext2D {
  const settings: CanvasRenderingContext2DSettings = { colorSpace: space, alpha: false };
  const old = host.querySelector("canvas");
  if (old) {
    const existing = old.getContext("2d", settings);
    if (existing && (existing.getContextAttributes().colorSpace ?? "srgb") === space) return existing;
    old.remove();
  }
  const canvas = document.createElement("canvas");
  canvas.className = className;
  host.append(canvas);
  const ctx = canvas.getContext("2d", settings);
  if (!ctx) throw new Error("no 2D canvas context");
  return ctx;
}

export async function loadFrame(ctx: CanvasRenderingContext2D, frame: OverlayFrame): Promise<Loaded> {
  const { width, height } = frame.frameSize;
  const canvas = ctx.canvas;
  if (canvas.width !== width) canvas.width = width;
  if (canvas.height !== height) canvas.height = height;

  const t0 = performance.now();
  const response = await fetch(frame.frameUrl, { cache: "no-store" });
  if (!response.ok) throw new Error(`the frame answered ${response.status}`);

  if (frame.format === "rgba") {
    const buffer = await response.arrayBuffer();
    const t1 = performance.now();
    const pixels = new Uint8ClampedArray(buffer);
    const image = new ImageData(pixels, width, height, { colorSpace: frame.colorSpace });
    const t2 = performance.now();
    ctx.putImageData(image, 0, 0);
    const t3 = performance.now();
    return { fetchMs: t1 - t0, decodeMs: t2 - t1, drawMs: t3 - t2, bytes: buffer.byteLength, pixels };
  }

  const blob = await response.blob();
  const t1 = performance.now();
  const bitmap = await createImageBitmap(blob);
  const t2 = performance.now();
  ctx.drawImage(bitmap, 0, 0);
  bitmap.close();
  const t3 = performance.now();
  return { fetchMs: t1 - t0, decodeMs: t2 - t1, drawMs: t3 - t2, bytes: blob.size, pixels: null };
}

/** One pixel as [r, g, b], from the kept pixels or, for a decoded image, from the canvas. */
export function pixelAt(ctx: CanvasRenderingContext2D, pixels: Uint8ClampedArray | null, x: number, y: number): [number, number, number] {
  const { width, height } = ctx.canvas;
  const px = Math.max(0, Math.min(width - 1, x));
  const py = Math.max(0, Math.min(height - 1, y));
  if (pixels) {
    const i = (py * width + px) * 4;
    return [pixels[i], pixels[i + 1], pixels[i + 2]];
  }
  const d = ctx.getImageData(px, py, 1, 1).data;
  return [d[0], d[1], d[2]];
}

/** The loupe's lens: `cells` × `cells` frame pixels around (`x`, `y`), each drawn as a block, with
 * black past the frame's edge. */
export function drawLens(lens: CanvasRenderingContext2D, frame: HTMLCanvasElement, x: number, y: number, cells: number): void {
  const out = lens.canvas;
  const cell = out.width / cells;
  const half = Math.floor(cells / 2);
  lens.imageSmoothingEnabled = false;
  lens.fillStyle = "#000";
  lens.fillRect(0, 0, out.width, out.height);
  // Only the part inside the frame: a source rectangle past the edge draws nothing in WebKit.
  const sx = Math.max(0, x - half);
  const sy = Math.max(0, y - half);
  const ex = Math.min(frame.width, x - half + cells);
  const ey = Math.min(frame.height, y - half + cells);
  if (ex <= sx || ey <= sy) return;
  lens.drawImage(frame, sx, sy, ex - sx, ey - sy, (sx - (x - half)) * cell, (sy - (y - half)) * cell, (ex - sx) * cell, (ey - sy) * cell);
}

const hex2 = (n: number) => n.toString(16).padStart(2, "0");

export function hex([r, g, b]: [number, number, number]): string {
  return `#${hex2(r)}${hex2(g)}${hex2(b)}`.toUpperCase();
}

/** A CSS colour for a pixel in `space`, so the swatch shows the colour the frame holds. */
export function cssColor([r, g, b]: [number, number, number], space: ColorSpace): string {
  if (space === "display-p3") return `color(display-p3 ${(r / 255).toFixed(4)} ${(g / 255).toFixed(4)} ${(b / 255).toFixed(4)})`;
  return `rgb(${r} ${g} ${b})`;
}
