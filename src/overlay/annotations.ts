// The overlay's annotation layer (milestone 2): the one seam between the overlay page and the
// annotation engine of `src/annotate/`. The page converts the pointer to frame pixels and calls
// these; everything else (the document, the history, the drawing) is the engine's.
//
// The document's image is the display's whole frame, and the selection is its crop, in frame
// pixels rounded the way the core maps a selection (`selection_to_frame`). Drawings keep their
// place on the frame when the selection moves, so a selection can be widened after drawing. Moving
// the selection is an undo step only once something is drawn; before that the crop just follows.

import { shallowRef } from "vue";

import type { OverlayFrame } from "../lib/api/capture";
import { CURRENT_VERSION, serializeDocument, type AnnotationDocument, type Point, type Rect } from "../lib/annotate";
import { renderExport, redactedPixels, type ImageSource, type Rendered } from "../annotate/compose";
import { AnnotationEngine, type Modifiers, type PressResult } from "../annotate/engine";
import { defaultPrefs, loadPrefs, savePrefs, type ToolId } from "../annotate/prefs";
import type { Bytes, PixelRect, Pixels } from "../annotate/redact";

export type { Point };

function emptyDocument(width: number, height: number, scale: number): AnnotationDocument {
  return { version: CURRENT_VERSION, image: { width, height }, scale, objects: [], beautify: null };
}

/** The engine of this overlay page; it lives as long as the page and takes a new document for
 * every capture and every new selection. The crop tool draws a new crop over the old one, because
 * the overlay's crop is its selection. */
export const engine = new AnnotationEngine(emptyDocument(1, 1, 1), defaultPrefs(), { cropDrawsInside: true });
engine.onPrefs = savePrefs;

/** The frame as the renderer reads it; `null` between captures. */
export const source = shallowRef<ImageSource | null>(null);

/** Shows the redaction notice; the page hides it again. */
export const noticeShown = shallowRef(false);
engine.onRedaction = () => (noticeShown.value = true);

/** Reads the remembered inks and widths once, when the page starts. */
export async function init(): Promise<void> {
  const prefs = await loadPrefs();
  engine.prefs = prefs;
  engine.rev.value += 1;
}

/** A capture's frame is on the canvas: a fresh document over it. `pixels` are the frame's RGBA
 * rows when it came raw; otherwise they are read from the canvas the first time a redaction needs
 * them. */
export function attach(frame: OverlayFrame, canvas: HTMLCanvasElement, pixels: Uint8ClampedArray | null, pointsWide: number): void {
  const { width, height } = frame.frameSize;
  let kept: Pixels | null = pixels ? { width, height, data: pixels as Bytes } : null;
  const colorSpace = frame.colorSpace;
  source.value = {
    colorSpace,
    canvas,
    get pixels(): Pixels {
      if (!kept) {
        const ctx = canvas.getContext("2d", { colorSpace });
        const data = ctx ? (ctx.getImageData(0, 0, width, height, { colorSpace }).data as Bytes) : new Uint8ClampedArray(width * height * 4);
        kept = { width, height, data };
      }
      return kept;
    },
  };
  engine.load(emptyDocument(width, height, width / Math.max(1, pointsWide)));
  noticeShown.value = false;
}

/** The capture closed. */
export function detach(): void {
  source.value = null;
  engine.load(emptyDocument(1, 1, 1));
  noticeShown.value = false;
}

/** Forgets every object, when the selection is dropped. */
export function clear(): void {
  const { width, height } = engine.doc.image;
  engine.load(emptyDocument(width, height, engine.doc.scale));
}

/** The selection, in frame pixels, became `rect`. Once something is drawn the change is an undo
 * step of its own; before that the crop only follows the selection. */
export function setSelection(rect: PixelRect | null): void {
  engine.setCrop(rect, engine.hasDrawings());
}

/** The crop as the engine has it (the crop tool and undo change it), in frame pixels. */
export function cropRect(): Rect | null {
  return engine.crop();
}

export function setTool(tool: ToolId | null): void {
  engine.setTool(tool);
}

/**
 * A press at `p` (frame pixels). The selected object's handles come first, wherever they are;
 * anything else only counts inside the selection. `none` leaves the press to the page, which then
 * moves the selection. `free` is ⌥ held, when a tool that snaps to words draws freely.
 */
export function press(p: Point, reach: number, inside: boolean, over = false, free = false): PressResult {
  if (!over && engine.handleAt(p, reach)) return engine.pointerDown(p, reach, false, free);
  if (!inside) {
    if (engine.editingId) engine.finishText();
    return "none";
  }
  return engine.pointerDown(p, reach, over, free);
}

/** What a press at `p` would do, for the page's cursor; `over` is a press with ⌘ held, which
 * only draws, and `free` is ⌥ held. */
export function cursorAt(p: Point, reach: number, over = false, free = false): string | null {
  return engine.cursorAt(p, reach, over, free);
}

/** ⌥ went down or up during a drag (`AnnotationEngine.setFree`). */
export function setFree(free: boolean): void {
  engine.setFree(free);
}

/** The pointer is at `p` (frame pixels), or somewhere a press picks up no object (`null`); the
 * object a press would pick up gets the hover frame. */
export function hover(p: Point | null, reach: number): void {
  engine.hover(p, reach);
}

export function move(p: Point, mods: Modifiers): void {
  engine.pointerMove(p, mods);
}

export function end(): void {
  engine.pointerUp();
}

export function busy(): boolean {
  return engine.busy();
}

/** A double click on a text starts typing into it. */
export function doubleClick(p: Point, reach: number): boolean {
  const object = engine.objectAt(p, reach, true);
  if (!object || object.kind !== "text") return false;
  engine.editText(object.id);
  return true;
}

export function undo(): void {
  engine.undo();
}

export function redo(): void {
  engine.redo();
}

export function canUndo(): boolean {
  return engine.canUndo();
}

export function canRedo(): boolean {
  return engine.canRedo();
}

/** The engine's own keys (delete, duplicate, order, lock, hide, nudge, ⌥C, Esc). */
export function key(e: KeyboardEvent): boolean {
  return engine.key(e);
}

/** Whether there is anything for the output to draw; without it the fast path crops in Rust. */
export function hasDrawings(): boolean {
  return engine.drawings().some((o) => !o.hidden);
}

/** The selection rendered at the frame's resolution and colour space, drawings and redactions
 * flattened in. */
export function render(): Rendered | null {
  const s = source.value;
  return s ? renderExport(s, engine.doc) : null;
}

/** The document as JSON, for the hand-over to the editor. */
export function documentJson(): string {
  return JSON.stringify(serializeDocument(engine.doc));
}

/** The selection's pixels with the redactions flattened in, for the text recognition layer. */
export function pixels(area: PixelRect): Pixels | null {
  const s = source.value;
  return s ? redactedPixels(s, engine.doc, area) : null;
}
