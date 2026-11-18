// The one renderer: it puts the image, the redactions and the drawings of a document together,
// for the live view and for the export. The live view and the export run the same steps over the
// same pixels, so what the user sees is what the file holds:
//
// 1. the image pixels of the area, with every visible blur and pixelate written over them
//    (`putImageData`, which replaces pixels and never blends them),
// 2. every other visible object in stacking order, the spotlights' shared dim at the place of the
//    topmost spotlight.
//
// The live view keeps a copy of the area's original pixels on a canvas and the redacted patches as
// `ImageData`, so a redraw while the user drags an arrow costs a canvas copy and the drawing; the
// export flattens the redactions into one pixel array (redact.ts), which is the exact same data.

import type { AnnotationDocument, AnnotationObject } from "../lib/annotate";
import { drawObject, drawSpotlights } from "./draw";
import { PatchCache, flattenRedactions, intersect, isRedaction, type Bytes, type PixelRect, type Pixels } from "./redact";

type Ctx = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;
type Spotlight = Extract<AnnotationObject, { kind: "spotlight" }>;

/** The document's image: its RGBA pixels, their colour space, and a canvas holding them for fast
 * copies (the overlay's frame canvas, the editor's image canvas). */
export type ImageSource = {
  pixels: Pixels;
  colorSpace: PredefinedColorSpace;
  canvas: HTMLCanvasElement | OffscreenCanvas;
};

/** The exported part of the image in whole pixels: the crop with each edge rounded to the nearest
 * pixel and clamped to the image (as the core maps a selection), or the whole image. */
export function exportArea(doc: AnnotationDocument): PixelRect | null {
  const crop = doc.objects.find((o) => o.kind === "crop");
  const { width, height } = doc.image;
  if (!crop || crop.kind !== "crop") return { x: 0, y: 0, width, height };
  const x0 = Math.max(0, Math.min(width, Math.round(crop.rect.x)));
  const y0 = Math.max(0, Math.min(height, Math.round(crop.rect.y)));
  const x1 = Math.max(0, Math.min(width, Math.round(crop.rect.x + crop.rect.width)));
  const y1 = Math.max(0, Math.min(height, Math.round(crop.rect.y + crop.rect.height)));
  return x1 > x0 && y1 > y0 ? { x: x0, y: y0, width: x1 - x0, height: y1 - y0 } : null;
}

/** A canvas for work the page never shows. */
export function scratchCanvas(width: number, height: number): HTMLCanvasElement | OffscreenCanvas {
  if (typeof document !== "undefined") {
    const canvas = document.createElement("canvas");
    canvas.width = width;
    canvas.height = height;
    return canvas;
  }
  return new OffscreenCanvas(width, height);
}

export function context2d(canvas: HTMLCanvasElement | OffscreenCanvas, colorSpace: PredefinedColorSpace, read = false): Ctx {
  const ctx = canvas.getContext("2d", { colorSpace, willReadFrequently: read }) as Ctx | null;
  if (!ctx) throw new Error("the canvas has no 2D context");
  return ctx;
}

/** Draws every visible object that is drawn rather than computed, in stacking order, on a context
 * whose origin is the image's (the caller translates it to the area). */
export function drawDrawings(ctx: Ctx, doc: AnnotationDocument, area: PixelRect, layer: HTMLCanvasElement | OffscreenCanvas, skip: string | null): void {
  const visible = doc.objects.filter((o) => !o.hidden && o.id !== skip);
  const spots = visible.filter((o): o is Spotlight => o.kind === "spotlight");
  const top = spots.length > 0 ? spots[spots.length - 1].id : null;
  for (const object of visible) {
    if (isRedaction(object) || object.kind === "crop") continue;
    if (object.kind === "spotlight") {
      if (object.id === top) drawSpotlights(ctx, spots, area, doc.scale, layer);
      continue;
    }
    drawObject(ctx, object, doc.scale);
  }
}

/** The live view of one document over one image. */
export class Composer {
  private patches = new PatchCache();
  private patchData = new WeakMap<Bytes, ImageData>();
  private base: HTMLCanvasElement | OffscreenCanvas | null = null;
  private baseKey = "";
  private layer: HTMLCanvasElement | OffscreenCanvas | null = null;

  constructor(readonly source: ImageSource) {}

  /** Draws `area` of the composed image into `ctx`, whose canvas is `area`'s size. `skip` leaves
   * one object out (the text being typed, which the text field shows). */
  draw(ctx: CanvasRenderingContext2D, doc: AnnotationDocument, area: PixelRect, skip: string | null = null): void {
    const { pixels, colorSpace } = this.source;
    const key = `${area.x},${area.y},${area.width},${area.height}`;
    if (!this.base || key !== this.baseKey) {
      // Moving the overlay's selection changes the area on every frame but not its size: the
      // copy is redrawn into the same canvas rather than into a new one each time.
      if (!this.base || this.base.width !== area.width || this.base.height !== area.height) this.base = scratchCanvas(area.width, area.height);
      const copy = context2d(this.base, colorSpace);
      copy.clearRect(0, 0, area.width, area.height);
      copy.drawImage(this.source.canvas, area.x, area.y, area.width, area.height, 0, 0, area.width, area.height);
      this.baseKey = key;
    }
    ctx.save();
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.globalCompositeOperation = "copy";
    ctx.drawImage(this.base, 0, 0);
    ctx.restore();
    for (const object of doc.objects) {
      if (object.hidden || !isRedaction(object)) continue;
      const patch = this.patches.get(pixels, object, doc.scale);
      if (!patch) continue;
      const part = intersect(patch.region, area);
      if (!part) continue;
      let data = this.patchData.get(patch.data);
      if (!data) {
        data = new ImageData(patch.data, patch.region.width, patch.region.height, { colorSpace });
        this.patchData.set(patch.data, data);
      }
      ctx.putImageData(data, patch.region.x - area.x, patch.region.y - area.y, part.x - patch.region.x, part.y - patch.region.y, part.width, part.height);
    }
    this.layer ??= scratchCanvas(1, 1);
    ctx.save();
    ctx.translate(-area.x, -area.y);
    drawDrawings(ctx, doc, area, this.layer, skip);
    ctx.restore();
  }

  /** Forgets the copies, after the image changed. */
  reset(): void {
    this.patches.clear();
    this.base = null;
    this.baseKey = "";
  }
}

export type Rendered = { area: PixelRect; width: number; height: number; colorSpace: PredefinedColorSpace; data: Bytes };

/**
 * The exported image: the document's export area at the image's own resolution and colour space,
 * as RGBA bytes. The redactions are flattened into the pixel array before anything is drawn, so
 * the original pixels under them never reach the canvas at all.
 */
export function renderExport(source: ImageSource, doc: AnnotationDocument): Rendered | null {
  const area = exportArea(doc);
  if (!area) return null;
  const { width, height } = area;
  const canvas = scratchCanvas(width, height);
  const ctx = context2d(canvas, source.colorSpace, true);
  const base = flattenRedactions(source.pixels, doc.objects, doc.scale, area);
  ctx.putImageData(new ImageData(base, width, height, { colorSpace: source.colorSpace }), 0, 0);
  ctx.save();
  ctx.translate(-area.x, -area.y);
  drawDrawings(ctx, doc, area, scratchCanvas(1, 1), null);
  ctx.restore();
  const data = ctx.getImageData(0, 0, width, height, { colorSpace: source.colorSpace }).data;
  return { area, width, height, colorSpace: source.colorSpace, data };
}

/** The pixels of `area` with the redactions flattened and nothing drawn: what the text
 * recognition layer may read, because a redacted area must never be read back. */
export function redactedPixels(source: ImageSource, doc: AnnotationDocument, area: PixelRect): Pixels {
  return { width: area.width, height: area.height, data: flattenRedactions(source.pixels, doc.objects, doc.scale, area) };
}
