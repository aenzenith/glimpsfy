// The one renderer: it puts the image, the redactions and the drawings of a document together,
// for the live view and for the export. The live view and the export run the same steps over the
// same pixels, so what the user sees is what the file holds:
//
// 1. the image pixels of the area, with every visible blur and pixelate written over them
//    (`putImageData`, which replaces pixels and never blends them),
// 2. every other visible object in stacking order, the spotlights' shared dim at the place of the
//    topmost spotlight. A magnifier's loupe shows what steps 1 and 2 have put under it so far: the
//    pixels of its ring with the redactions flattened in and the drawings below it, never itself
//    or anything above it (`Loupes`, magnify.ts).
//
// The live view keeps a copy of the area's original pixels on a canvas and the redacted patches as
// `ImageData`, so a redraw while the user drags an arrow costs a canvas copy and the drawing; the
// export flattens the redactions into one pixel array (redact.ts), which is the exact same data.

import type { AnnotationDocument, AnnotationObject } from "../lib/annotate";
import { drawMagnifier, drawObject, drawSpotlights } from "./draw";
import { loupePatch, loupePixels, smoothAt, sourceRegion, type LoupePatch, type Magnify } from "./magnify";
import { PatchCache, flattenRedactions, intersect, isRedaction, type Bytes, type PixelRect, type Pixels } from "./redact";

type Ctx = CanvasRenderingContext2D | OffscreenCanvasRenderingContext2D;
type Canvas = HTMLCanvasElement | OffscreenCanvas;
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
export function drawDrawings(ctx: Ctx, doc: AnnotationDocument, area: PixelRect, layer: Canvas, skip: string | null, loupes: Loupes): void {
  const visible = doc.objects.filter((o) => !o.hidden && o.id !== skip);
  drawRange(ctx, visible, visible.length, doc, area, layer, loupes);
}

/** The index of the topmost spotlight in `visible`, where the shared dim is drawn; -1 for none. */
function topSpot(visible: readonly AnnotationObject[]): number {
  for (let i = visible.length - 1; i >= 0; i--) if (visible[i].kind === "spotlight") return i;
  return -1;
}

/** Draws the first `end` objects of `visible` over `area`. The dim follows the whole list, so a
 * loupe's pixels hold it only when the topmost spotlight lies below the magnifier, exactly as the
 * image holds it at that point. */
function drawRange(ctx: Ctx, visible: readonly AnnotationObject[], end: number, doc: AnnotationDocument, area: PixelRect, layer: Canvas, loupes: Loupes): void {
  const top = topSpot(visible);
  for (let i = 0; i < end; i++) {
    const object = visible[i];
    if (isRedaction(object) || object.kind === "crop") continue;
    if (object.kind === "spotlight") {
      if (i === top) drawSpotlights(ctx, visible.filter((o): o is Spotlight => o.kind === "spotlight"), area, doc.scale, layer);
      continue;
    }
    if (object.kind === "magnify") {
      loupes.draw(ctx, object, visible, i, doc, area);
      continue;
    }
    drawObject(ctx, object, doc.scale);
  }
}

type LoupeEntry = { key: string; below: readonly AnnotationObject[]; canvas: Canvas; x: number; y: number };

/**
 * The loupes of one image. A loupe's pixels are the ring's pixels with the redactions flattened in
 * (`loupePixels`); when drawings lie below the magnifier, they are drawn over those pixels on a
 * canvas of the ring's size first, nested magnifiers included, and the result is enlarged the same
 * way. The enlarged pixels go into the canvas one to one. The live view keeps them while nothing
 * they depend on changes, so dragging another object does not enlarge them again.
 */
export class Loupes {
  private entries = new Map<string, LoupeEntry>();
  private pixels: Pixels | null = null;
  private layer: Canvas | null = null;

  constructor(
    private readonly source: ImageSource,
    private readonly patches?: PatchCache,
  ) {}

  /** Draws magnifier `visible[index]` on `ctx`, which holds `clip` of the image. */
  draw(ctx: Ctx, object: Magnify, visible: readonly AnnotationObject[], index: number, doc: AnnotationDocument, clip: PixelRect): void {
    const patch = this.patch(object, visible, index, doc, clip);
    drawMagnifier(ctx, object, doc.scale, (c) => {
      if (patch) c.drawImage(patch.canvas, patch.x, patch.y);
    });
  }

  private patch(o: Magnify, visible: readonly AnnotationObject[], index: number, doc: AnnotationDocument, clip: PixelRect): LoupeEntry | null {
    const src = this.source.pixels;
    if (src !== this.pixels) {
      this.entries.clear();
      this.pixels = src;
    }
    const region = sourceRegion(o, src.width, src.height);
    if (!region) return null;
    const below = visible.slice(0, index);
    const id = `${o.id}|${clip.x},${clip.y},${clip.width},${clip.height}`;
    const key = `${region.x},${region.y},${region.width},${region.height}|${o.source.x},${o.source.y},${o.at.x},${o.at.y}|${o.factor}|${smoothAt(o, doc.scale)}|${doc.scale}`;
    const kept = this.entries.get(id);
    if (kept && kept.key === key && kept.below.length === below.length && kept.below.every((b, i) => b === below[i])) return kept;
    const patch = this.drawsBelow(visible, index) ? this.composed(o, visible, index, doc, region, clip) : loupePixels(src, doc.objects, doc.scale, o, this.patches, clip);
    if (!patch) {
      this.entries.delete(id);
      return null;
    }
    const { colorSpace } = this.source;
    const canvas = scratchCanvas(patch.width, patch.height);
    context2d(canvas, colorSpace).putImageData(new ImageData(patch.data, patch.width, patch.height, { colorSpace }), 0, 0);
    // Entries of objects that no longer exist would pile up during a drag; a small cap is enough.
    if (this.entries.size > 32) this.entries.clear();
    const entry = { key, below, canvas, x: patch.x, y: patch.y };
    this.entries.set(id, entry);
    return entry;
  }

  /** Whether anything below `visible[index]` paints on the image: a drawing, or the dim. */
  private drawsBelow(visible: readonly AnnotationObject[], index: number): boolean {
    const top = topSpot(visible);
    if (top >= 0 && top < index) return true;
    return visible.slice(0, index).some((o) => !isRedaction(o) && o.kind !== "crop" && o.kind !== "spotlight");
  }

  /** The loupe's pixels over the redacted ring with the drawings below drawn in. */
  private composed(o: Magnify, visible: readonly AnnotationObject[], index: number, doc: AnnotationDocument, region: PixelRect, clip: PixelRect): LoupePatch | null {
    const { colorSpace } = this.source;
    const base = flattenRedactions(this.source.pixels, doc.objects, doc.scale, region, this.patches);
    const canvas = scratchCanvas(region.width, region.height);
    const ctx = context2d(canvas, colorSpace, true);
    ctx.putImageData(new ImageData(base, region.width, region.height, { colorSpace }), 0, 0);
    ctx.save();
    ctx.translate(-region.x, -region.y);
    drawRange(ctx, visible, index, doc, region, (this.layer ??= scratchCanvas(1, 1)), this);
    ctx.restore();
    const data = ctx.getImageData(0, 0, region.width, region.height, { colorSpace }).data as Bytes;
    return loupePatch(data, region, o, doc.scale, clip);
  }
}

/** The live view of one document over one image. */
export class Composer {
  private patches = new PatchCache();
  private patchData = new WeakMap<Bytes, ImageData>();
  private base: HTMLCanvasElement | OffscreenCanvas | null = null;
  private baseKey = "";
  private layer: HTMLCanvasElement | OffscreenCanvas | null = null;
  private loupes: Loupes | null = null;

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
    this.loupes ??= new Loupes(this.source, this.patches);
    ctx.save();
    ctx.translate(-area.x, -area.y);
    drawDrawings(ctx, doc, area, this.layer, skip, this.loupes);
    ctx.restore();
  }

  /** Forgets the copies, after the image changed. */
  reset(): void {
    this.patches.clear();
    this.loupes = null;
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
  drawDrawings(ctx, doc, area, scratchCanvas(1, 1), null, new Loupes(source));
  ctx.restore();
  const data = ctx.getImageData(0, 0, width, height, { colorSpace: source.colorSpace }).data;
  return { area, width, height, colorSpace: source.colorSpace, data };
}

/** The pixels of `area` with the redactions flattened and nothing drawn: what the text
 * recognition layer may read, because a redacted area must never be read back. */
export function redactedPixels(source: ImageSource, doc: AnnotationDocument, area: PixelRect): Pixels {
  return { width: area.width, height: area.height, data: flattenRedactions(source.pixels, doc.objects, doc.scale, area) };
}
