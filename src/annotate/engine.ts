// The annotation engine that the overlay and the editor share: the document, the active tool and
// its options, the selected object, the gesture under way, and the undo and redo stack.
//
// The engine knows nothing about the screen. A surface converts the pointer into image pixels,
// tells the engine how far a pointer may miss in those pixels (`reach`), and redraws whenever
// `rev` changes. Every operation, a crop included, goes through the history, so undo and redo
// cover all of them: a gesture is one step however many moves it had.
//
// Objects are never changed in place. Every change makes a new object and a new list, so a
// history entry is just the list as it was, and comparing two lists by identity tells whether
// anything changed.

import { ref } from "vue";

import type { AnnotationDocument, AnnotationObject, BeautifyStyle, Color, Point, Rect } from "../lib/annotate";
import { textSize } from "./draw";
import { boxTo, clamp, contains, frameOf, handlesOf, hits, moved, rectFrom, resized, snapAngle, type Handle } from "./geometry";
import type { AnnotatePrefs, ToolId, ToolOptions } from "./prefs";
import {
  BLUR_LEVELS,
  CELL_LEVELS,
  DIM_LEVELS,
  HIGHLIGHTER_FACTOR,
  HIGHLIGHTER_INKS,
  HIGHLIGHTER_WIDTHS,
  INKS,
  STEP_BASE,
  STEP_SIZES,
  STROKE_WIDTHS,
  TEXT_PAD,
  TEXT_SIZES,
  LINE_HEIGHT,
  contrastInk,
  ink,
  nearestIndex,
  type CropRatio,
} from "./style";

type Objects = readonly AnnotationObject[];
type Kind = AnnotationObject["kind"];

export const CROP_ID = "crop";

/** The object kind a tool draws. */
export const KIND_OF: Record<ToolId, Kind> = {
  arrow: "arrow",
  line: "line",
  rect: "rectangle",
  ellipse: "ellipse",
  pen: "pen",
  highlighter: "highlighter",
  text: "text",
  step: "step",
  blur: "blur",
  pixelate: "pixelate",
  spotlight: "spotlight",
  crop: "crop",
};

/** The tool that draws an object kind, for its options. */
export const TOOL_OF: Record<Kind, ToolId> = Object.fromEntries(Object.entries(KIND_OF).map(([tool, kind]) => [kind, tool])) as Record<Kind, ToolId>;

/** What a press started. `none` leaves the press to the surface (the overlay moves its
 * selection, the editor clears the selection). */
export type PressResult = "handle" | "object" | "draw" | "text" | "none";

export type Modifiers = { shift?: boolean; alt?: boolean };

type Gesture =
  | { kind: "draw"; tool: ToolId; start: Point; id: string; travelled: number }
  | { kind: "move"; id: string; start: Point; origin: AnnotationObject }
  | { kind: "handle"; id: string; handle: string; origin: AnnotationObject }
  /** The whole group follows the pointer; `id` is the member that was pressed. */
  | { kind: "group"; id: string; start: Point; origins: ReadonlyMap<string, AnnotationObject> };

let counter = 0;
function newId(kind: string): string {
  counter += 1;
  const random = typeof crypto !== "undefined" && "randomUUID" in crypto ? crypto.randomUUID().slice(0, 8) : Math.random().toString(36).slice(2, 10);
  return `${kind}-${random}${counter}`;
}

/** The undo and redo stack: each entry is a whole object list. */
/** One state of the history: the drawings and the beautify style around them, so a change in the
 * beautify panel is a step like a drawing. */
type Step = { objects: Objects; beautify: BeautifyStyle | null };

class History {
  past: Step[] = [];
  future: Step[] = [];
  static readonly LIMIT = 200;

  push(before: Step): void {
    this.past.push(before);
    if (this.past.length > History.LIMIT) this.past.shift();
    this.future = [];
  }

  clear(): void {
    this.past = [];
    this.future = [];
  }

  /** Changes the crop in every entry, for a crop change that is not a step of its own. */
  rewriteCrop(crop: AnnotationObject | null): void {
    const swap = (step: Step): Step => {
      const rest = step.objects.filter((o) => o.kind !== "crop");
      return { ...step, objects: crop ? [crop, ...rest] : rest };
    };
    this.past = this.past.map(swap);
    this.future = this.future.map(swap);
  }
}

export type EngineOptions = {
  /** The overlay's selection is the crop, so the crop tool draws a new crop even inside it. */
  cropDrawsInside?: boolean;
};

export class AnnotationEngine {
  /** Counts every change the surfaces show; read it to subscribe. */
  readonly rev = ref(0);
  /** The object a press would pick up, framed before the press. A ref of its own, apart from
   * `rev`, so that the pointer passing over objects does not recompose the canvas. */
  readonly hovered = ref<string | null>(null);
  doc: AnnotationDocument;
  tool: ToolId | null = null;
  selectedId: string | null = null;
  /** The objects ⌘A picked, two or more, moved and deleted together. While it holds anything,
   * `selectedId` is null, so the strip, the handles and the single-object keys stay out of it. */
  groupIds: readonly string[] = [];
  /** The text object being typed into, which the surface's text field shows instead of the
   * canvas. */
  editingId: string | null = null;
  prefs: AnnotatePrefs;
  /** Called after any change to `prefs`, so the surface can save them. */
  onPrefs: (prefs: AnnotatePrefs) => void = () => {};
  /** Called when a blur or pixelate is drawn and the notice was never dismissed. */
  onRedaction: () => void = () => {};

  private history = new History();
  private gesture: Gesture | null = null;
  /** The object list when the current gesture or text edit began. */
  private before: Objects | null = null;
  private saved: Objects;
  private savedBeautify: BeautifyStyle | null;
  private cropSession: { objects: Objects; beautify: BeautifyStyle | null; past: number } | null = null;

  constructor(
    doc: AnnotationDocument,
    prefs: AnnotatePrefs,
    private readonly options: EngineOptions = {},
  ) {
    this.doc = doc;
    this.prefs = prefs;
    this.saved = doc.objects;
    this.savedBeautify = doc.beautify;
  }

  // ------------------------------------------------------------------------------- reading

  private bump(): void {
    this.rev.value += 1;
  }

  get scale(): number {
    return this.doc.scale;
  }

  get(id: string | null): AnnotationObject | null {
    return id === null ? null : (this.doc.objects.find((o) => o.id === id) ?? null);
  }

  selected(): AnnotationObject | null {
    return this.get(this.selectedId);
  }

  /** Whether a press or ⌘A can pick the object up. */
  private selectable(o: AnnotationObject | null): o is AnnotationObject {
    return o !== null && o.kind !== "crop" && !o.hidden && !o.locked;
  }

  inGroup(id: string): boolean {
    return this.groupIds.includes(id);
  }

  /** The group's members, bottom first. */
  group(): AnnotationObject[] {
    return this.doc.objects.filter((o) => this.groupIds.includes(o.id));
  }

  /** The drawn objects, without the crop, bottom first. */
  drawings(): AnnotationObject[] {
    return this.doc.objects.filter((o) => o.kind !== "crop");
  }

  hasDrawings(): boolean {
    return this.doc.objects.some((o) => o.kind !== "crop");
  }

  crop(): Rect | null {
    const crop = this.doc.objects.find((o) => o.kind === "crop");
    return crop && crop.kind === "crop" ? crop.rect : null;
  }

  canUndo(): boolean {
    return this.history.past.length > 0;
  }

  canRedo(): boolean {
    return this.history.future.length > 0;
  }

  /** Whether the document differs from the last `markSaved`. */
  dirty(): boolean {
    return this.doc.objects !== this.saved || this.doc.beautify !== this.savedBeautify;
  }

  markSaved(): void {
    this.saved = this.doc.objects;
    this.savedBeautify = this.doc.beautify;
    this.bump();
  }

  /**
   * Sets the beautify style around the image, or `null` for none. With `step`, the change is one
   * undo step; without it, it joins the step before, so a slider dragged through many values is
   * one ⌘Z.
   */
  setBeautify(style: BeautifyStyle | null, step = true): void {
    this.finishText();
    if (style === this.doc.beautify) return;
    if (step) this.history.push(this.step(this.doc.objects));
    this.doc = { ...this.doc, beautify: style };
    this.bump();
  }

  /** The handles the surface draws: the selected object's, or the crop's in crop mode. */
  handles(): Handle[] {
    const object = this.tool === "crop" && !this.options.cropDrawsInside ? this.get(CROP_ID) : this.selected();
    return object ? handlesOf(object, textSize) : [];
  }

  /** The box the selection outline is drawn around. */
  frame(id: string): Rect | null {
    const object = this.get(id);
    return object ? frameOf(object, textSize) : null;
  }

  // ------------------------------------------------------------------------------ changing

  private set(objects: Objects): void {
    this.doc = { ...this.doc, objects: objects as AnnotationObject[] };
    if (this.selectedId && !this.get(this.selectedId)) this.selectedId = null;
    if (this.groupIds.length) {
      // Undo can take members away, and a lock or a hide drops one; a group of one is no group.
      const left = this.groupIds.filter((id) => this.selectable(this.get(id)));
      this.groupIds = left.length > 1 ? left : [];
    }
    if (this.editingId && !this.get(this.editingId)) this.editingId = null;
    this.bump();
  }

  /** Applies one operation as one history step. */
  private apply(change: (objects: Objects) => Objects): void {
    const before = this.doc.objects;
    const after = change(before);
    if (after === before) return;
    this.history.push(this.step(before));
    this.set(after);
  }

  /** The history entry of `objects`. */
  private step(objects: Objects): Step {
    return { objects, beautify: this.doc.beautify };
  }

  /** Goes back or forward to a history entry. */
  private restore(step: Step): void {
    if (step.beautify !== this.doc.beautify) this.doc = { ...this.doc, beautify: step.beautify };
    this.set(step.objects);
  }

  private replace(id: string, next: AnnotationObject): Objects {
    return this.doc.objects.map((o) => (o.id === id ? next : o));
  }

  /** Starts a fresh document (a new capture or a new image); the history goes with the old one.
   * `unsaved` marks drawings that exist nowhere else yet, such as the overlay's hand-over. */
  load(doc: AnnotationDocument, unsaved = false): void {
    this.cancelGesture();
    this.doc = doc;
    this.saved = unsaved ? [] : doc.objects;
    this.savedBeautify = unsaved ? null : doc.beautify;
    this.selectedId = null;
    this.groupIds = [];
    this.editingId = null;
    this.cropSession = null;
    this.history.clear();
    this.bump();
  }

  undo(): void {
    this.finishText();
    const previous = this.history.past.pop();
    if (!previous) return;
    this.history.future.push(this.step(this.doc.objects));
    this.restore(previous);
  }

  redo(): void {
    this.finishText();
    const next = this.history.future.pop();
    if (!next) return;
    this.history.past.push(this.step(this.doc.objects));
    this.restore(next);
  }

  setTool(tool: ToolId | null): void {
    this.finishText();
    if (this.tool === "crop" && tool !== "crop") this.cropSession = null;
    this.tool = tool;
    if (tool === "crop") {
      this.selectedId = null;
      this.groupIds = [];
      this.cropSession = { objects: this.doc.objects, beautify: this.doc.beautify, past: this.history.past.length };
    }
    if (tool && (tool === "blur" || tool === "pixelate") && !this.prefs.redactionNoticeSeen) this.onRedaction();
    this.bump();
  }

  select(id: string | null): void {
    if (id !== this.editingId) this.finishText();
    this.selectedId = id;
    this.groupIds = [];
    this.bump();
  }

  /** ⌘A: every object a press could pick up. One object is an ordinary selection, two or more a
   * group. Crop mode has no objects to pick. */
  selectAll(): void {
    if (this.tool === "crop") return;
    this.finishText();
    const ids = this.doc.objects.filter((o) => this.selectable(o)).map((o) => o.id);
    if (ids.length < 2) return this.select(ids[0] ?? null);
    this.selectedId = null;
    this.groupIds = ids;
    this.bump();
  }

  /**
   * Sets the crop. A change the user makes as a crop (`record`) is one undo step; a change that
   * only follows something else, such as the overlay's selection before anything is drawn, is
   * written into every history entry instead, so undoing a drawing never moves the crop back.
   */
  setCrop(rect: Rect | null, record: boolean): void {
    const current = this.crop();
    if (current && rect && current.x === rect.x && current.y === rect.y && current.width === rect.width && current.height === rect.height) return;
    if (!current && !rect) return;
    const crop: AnnotationObject | null = rect ? { id: CROP_ID, hidden: false, locked: false, kind: "crop", rect } : null;
    const without = this.doc.objects.filter((o) => o.kind !== "crop");
    const next = crop ? [crop, ...without] : without;
    if (record) this.apply(() => next);
    else {
      const clean = this.saved === this.doc.objects;
      this.history.rewriteCrop(crop);
      if (clean) this.saved = next;
      this.set(next);
    }
  }

  /** Leaves crop mode keeping the crop (Return). */
  applyCrop(): void {
    this.cropSession = null;
    this.setTool(null);
  }

  /** Leaves crop mode and puts back the crop it started with (Esc). */
  cancelCrop(): void {
    const session = this.cropSession;
    this.cancelGesture();
    if (session) {
      this.history.past.length = Math.min(this.history.past.length, session.past);
      this.history.future = [];
      this.restore({ objects: session.objects, beautify: session.beautify });
    }
    this.cropSession = null;
    this.setTool(null);
  }

  remove(id: string): void {
    this.removeAll([id]);
  }

  /** Removes the objects as one undo step. */
  private removeAll(ids: readonly string[]): void {
    const gone = new Set(ids);
    const steps = this.doc.objects.flatMap((o) => (gone.has(o.id) && o.kind === "step" ? [o.number] : []));
    this.apply((objects) => {
      const rest = objects.filter((o) => !gone.has(o.id));
      if (rest.length === objects.length) return objects;
      // Removing step markers closes the gaps in the sequence.
      if (!steps.length) return rest;
      return rest.map((o) => {
        if (o.kind !== "step") return o;
        const below = steps.filter((n) => n < o.number).length;
        return below ? { ...o, number: o.number - below } : o;
      });
    });
  }

  deleteSelected(): void {
    if (this.groupIds.length) this.removeAll(this.groupIds);
    else if (this.selectedId) this.remove(this.selectedId);
  }

  duplicate(): void {
    const source = this.selected();
    if (!source || source.kind === "crop") return;
    const offset = 10 * this.scale;
    let copy = { ...moved(source, offset, offset), id: newId(source.kind) } as AnnotationObject;
    if (copy.kind === "step") copy = { ...copy, number: this.nextStep() };
    const at = this.doc.objects.indexOf(source) + 1;
    this.apply((objects) => [...objects.slice(0, at), copy, ...objects.slice(at)]);
    this.selectedId = copy.id;
    this.bump();
  }

  /** Moves an object to `index` in the drawn objects (0 is the bottom). */
  reorder(id: string, index: number): void {
    this.apply((objects) => {
      const object = objects.find((o) => o.id === id);
      if (!object || object.kind === "crop") return objects;
      const crops = objects.filter((o) => o.kind === "crop");
      const drawn = objects.filter((o) => o.kind !== "crop" && o.id !== id);
      const to = clamp(index, 0, drawn.length);
      const next = [...crops, ...drawn.slice(0, to), object, ...drawn.slice(to)];
      return next.every((o, i) => o === objects[i]) ? objects : next;
    });
  }

  /** One step up (`+1`) or down (`-1`) the stack. */
  shift(id: string, by: 1 | -1): void {
    const drawn = this.drawings();
    const i = drawn.findIndex((o) => o.id === id);
    if (i < 0) return;
    this.reorder(id, i + by);
  }

  setHidden(id: string, hidden: boolean): void {
    const object = this.get(id);
    if (!object || object.hidden === hidden) return;
    this.apply(() => this.replace(id, { ...object, hidden }));
    if (hidden && this.selectedId === id) this.select(null);
  }

  setLocked(id: string, locked: boolean): void {
    const object = this.get(id);
    if (!object || object.locked === locked) return;
    this.apply(() => this.replace(id, { ...object, locked }));
    if (locked && this.selectedId === id) this.select(null);
  }

  nudge(dx: number, dy: number): void {
    if (this.groupIds.length) {
      this.apply((objects) => objects.map((o) => (this.inGroup(o.id) ? moved(o, dx, dy) : o)));
      return;
    }
    const object = this.selected();
    if (!object || object.locked) return;
    this.apply(() => this.replace(object.id, moved(object, dx, dy)));
  }

  // ----------------------------------------------------------------------- tool options

  /** The tool whose options the strip or the object bar shows: the selected object's tool,
   * or the active tool. */
  optionsTool(): ToolId | null {
    const object = this.selected();
    return object ? TOOL_OF[object.kind] : this.tool;
  }

  /** The strip's current choices for `tool`, read from the selected object when it is one of
   * that tool's. */
  current(tool: ToolId): ToolOptions {
    const saved = this.prefs.tools[tool];
    const object = this.selected();
    if (!object || TOOL_OF[object.kind] !== tool) return saved;
    const s = this.scale;
    const inkOf = (c: Color | null) => (c && c.kind === "ink" ? c.name : saved.ink);
    switch (object.kind) {
      case "arrow":
      case "line":
      case "rectangle":
      case "ellipse":
      case "pen":
        return { ...saved, ink: inkOf(object.stroke.color), width: nearestIndex(STROKE_WIDTHS, object.stroke.width / s) };
      case "highlighter":
        return { ...saved, ink: inkOf(object.color), width: nearestIndex(HIGHLIGHTER_WIDTHS, object.width / s / HIGHLIGHTER_FACTOR) };
      case "text":
        return {
          ...saved,
          ink: inkOf(object.background ?? object.color),
          size: nearestIndex(TEXT_SIZES, object.fontSize / s),
          background: object.background !== null,
        };
      case "step":
        return { ...saved, ink: inkOf(object.color), width: nearestIndex(STEP_SIZES, object.diameter / s - STEP_BASE) };
      case "blur":
        return { ...saved, level: BLUR_LEVELS.indexOf(object.strength) };
      case "pixelate":
        return { ...saved, level: CELL_LEVELS.indexOf(object.cell) };
      case "spotlight":
        return { ...saved, level: DIM_LEVELS.indexOf(object.dim) };
      default:
        return saved;
    }
  }

  /** Changes a choice: the tool remembers it, and the selected object of that tool takes it. */
  choose(tool: ToolId, change: Partial<ToolOptions>): void {
    this.prefs = { ...this.prefs, tools: { ...this.prefs.tools, [tool]: { ...this.prefs.tools[tool], ...change } } };
    this.onPrefs(this.prefs);
    const object = this.selected();
    if (object && TOOL_OF[object.kind] === tool) {
      const next = this.styled(object, this.prefs.tools[tool]);
      // While a text is being typed, its style belongs to the same undo step as its words.
      if (object.id === this.editingId) this.set(this.replace(object.id, next));
      else if (next !== object) this.apply(() => this.replace(object.id, next));
    }
    if (tool === "crop" && change.ratio) this.applyRatio(change.ratio);
    this.bump();
  }

  dismissRedactionNotice(): void {
    this.prefs = { ...this.prefs, redactionNoticeSeen: true };
    this.onPrefs(this.prefs);
    this.bump();
  }

  /** The next ink of the palette for the selected object (⌥C). */
  cycleInk(): void {
    const tool = this.optionsTool();
    if (!tool || ["blur", "pixelate", "spotlight", "crop"].includes(tool)) return;
    const list = tool === "highlighter" ? HIGHLIGHTER_INKS : INKS;
    const at = list.indexOf(this.current(tool).ink);
    this.choose(tool, { ink: list[(at + 1) % list.length] });
  }

  /** The object with a tool's options applied to its style; its geometry stays. */
  private styled(object: AnnotationObject, o: ToolOptions): AnnotationObject {
    const s = this.scale;
    switch (object.kind) {
      case "arrow":
      case "line":
      case "rectangle":
      case "ellipse":
      case "pen":
        return { ...object, stroke: { color: ink(o.ink), width: STROKE_WIDTHS[o.width] * s } };
      case "highlighter":
        return { ...object, color: ink(o.ink), width: HIGHLIGHTER_WIDTHS[o.width] * HIGHLIGHTER_FACTOR * s };
      case "text":
        return {
          ...object,
          fontSize: TEXT_SIZES[o.size] * s,
          color: o.background ? ink(contrastInk(ink(o.ink))) : ink(o.ink),
          background: o.background ? ink(o.ink) : null,
        };
      case "step":
        return { ...object, color: ink(o.ink), diameter: (STEP_BASE + STEP_SIZES[o.width]) * s };
      case "blur":
        return { ...object, strength: BLUR_LEVELS[o.level] };
      case "pixelate":
        return { ...object, cell: CELL_LEVELS[o.level] };
      case "spotlight":
        return { ...object, dim: DIM_LEVELS[o.level] };
      default:
        return object;
    }
  }

  /** The width / height a crop keeps, or `null` for a free crop. */
  ratio(): number | null {
    const choice: CropRatio = this.prefs.tools.crop.ratio;
    switch (choice) {
      case "free":
        return null;
      case "original":
        return this.doc.image.width / this.doc.image.height;
      default: {
        const [w, h] = choice.split(":").map(Number);
        return w / h;
      }
    }
  }

  /** Reshapes the crop to the chosen ratio around its centre, inside the crop it had. */
  private applyRatio(choice: CropRatio): void {
    const crop = this.crop();
    const r = choice === "free" ? null : this.ratio();
    if (!crop || !r) return;
    let width = crop.width;
    let height = width / r;
    if (height > crop.height) {
      height = crop.height;
      width = height * r;
    }
    const next = { x: Math.round(crop.x + (crop.width - width) / 2), y: Math.round(crop.y + (crop.height - height) / 2), width: Math.round(width), height: Math.round(height) };
    this.setCrop(next, true);
  }

  // ------------------------------------------------------------------------- new objects

  private nextStep(): number {
    return this.doc.objects.reduce((n, o) => (o.kind === "step" ? Math.max(n, o.number) : n), 0) + 1;
  }

  private create(tool: ToolId, p: Point): AnnotationObject {
    const id = newId(KIND_OF[tool]);
    const o = this.prefs.tools[tool];
    const s = this.scale;
    const base = { id, hidden: false, locked: false };
    const stroke = { color: ink(o.ink), width: STROKE_WIDTHS[o.width] * s };
    const box = { x: p.x, y: p.y, width: 0, height: 0 };
    switch (tool) {
      case "arrow":
        return { ...base, kind: "arrow", from: p, to: p, stroke };
      case "line":
        return { ...base, kind: "line", from: p, to: p, stroke };
      case "rect":
        return { ...base, kind: "rectangle", rect: box, stroke, fill: null };
      case "ellipse":
        return { ...base, kind: "ellipse", rect: box, stroke, fill: null };
      case "pen":
        return { ...base, kind: "pen", points: [p], stroke };
      case "highlighter":
        return { ...base, kind: "highlighter", points: [p], color: ink(o.ink), width: HIGHLIGHTER_WIDTHS[o.width] * HIGHLIGHTER_FACTOR * s };
      case "text": {
        const fontSize = TEXT_SIZES[o.size] * s;
        // The caret lands where the user clicked: the block starts one padding up and left.
        const origin = { x: p.x - TEXT_PAD.x * fontSize, y: p.y - TEXT_PAD.top * fontSize - (LINE_HEIGHT * fontSize) / 2 };
        return this.styled({ ...base, kind: "text", origin, text: "", fontSize, color: ink(o.ink), background: null, maxWidth: null }, o);
      }
      case "step":
        return { ...base, kind: "step", center: p, number: this.nextStep(), color: ink(o.ink), diameter: (STEP_BASE + STEP_SIZES[o.width]) * s };
      case "blur":
        return { ...base, kind: "blur", rect: box, strength: BLUR_LEVELS[o.level] };
      case "pixelate":
        return { ...base, kind: "pixelate", rect: box, cell: CELL_LEVELS[o.level] };
      case "spotlight":
        return { ...base, kind: "spotlight", rect: box, dim: DIM_LEVELS[o.level] };
      case "crop":
        return { ...base, id: CROP_ID, kind: "crop", rect: box };
    }
  }

  // ------------------------------------------------------------------------------ pointer

  private clampToImage(p: Point): Point {
    return { x: clamp(p.x, 0, this.doc.image.width), y: clamp(p.y, 0, this.doc.image.height) };
  }

  /** The topmost object under `p` that can be selected. */
  objectAt(p: Point, reach: number, interior: boolean): AnnotationObject | null {
    const list = this.doc.objects;
    for (let i = list.length - 1; i >= 0; i--) {
      const o = list[i];
      if (o.hidden || o.locked || o.kind === "crop") continue;
      if (hits(o, p, reach, interior, textSize)) return o;
    }
    return null;
  }

  /** The handle of the selected object (or of the crop in crop mode) under `p`. */
  handleAt(p: Point, reach: number): Handle | null {
    for (const h of this.handles()) if (Math.hypot(p.x - h.x, p.y - h.y) <= reach) return h;
    return null;
  }

  /** What a press at `p` would do, for the surface's cursor. `over` is a press that only draws
   * (⌘ held), so it ignores objects and handles. */
  cursorAt(p: Point, reach: number, over = false): string | null {
    if (over) return this.tool === "text" ? "text" : this.tool ? "crosshair" : null;
    const handle = this.handleAt(p, reach);
    if (handle) return handle.kind === "point" ? "crosshair" : handle.id === "0,0" || handle.id === "1,1" ? "nwse-resize" : handle.id === "1,0" || handle.id === "0,1" ? "nesw-resize" : handle.kind === "x" ? "ns-resize" : "ew-resize";
    if (this.tool === "crop" && !this.options.cropDrawsInside) {
      const crop = this.crop();
      return crop && contains(crop, p) ? "move" : "crosshair";
    }
    if (this.objectAt(p, reach, this.tool === null)) return "move";
    if (this.tool === "text") return "text";
    return this.tool ? "crosshair" : null;
  }

  /** Notes the object a press at `p` would pick up, by the same rules as `pointerDown`; `null`
   * means the pointer is somewhere a press picks up nothing. */
  hover(p: Point | null, reach: number): void {
    const free = p !== null && !this.gesture && !this.editingId && this.tool !== "crop" && !this.handleAt(p, reach);
    const object = free ? this.objectAt(p, reach, this.tool === null) : null;
    const id = object && object.id !== this.selectedId && !this.inGroup(object.id) ? object.id : null;
    if (this.hovered.value !== id) this.hovered.value = id;
  }

  /** A press at `p` in image pixels. With `over` (⌘ held) the press only draws: it starts a new
   * object with the active tool even on top of another, and never picks one up. */
  pointerDown(p: Point, reach: number, over = false): PressResult {
    if (this.editingId) {
      this.finishText();
    }
    const handle = over ? null : this.handleAt(p, reach);
    const crop = this.tool === "crop" && !this.options.cropDrawsInside;
    if (handle) {
      const target = crop ? this.get(CROP_ID) : this.selected();
      if (target && !target.locked) {
        this.begin({ kind: "handle", id: target.id, handle: handle.id, origin: target });
        return "handle";
      }
    }
    // A press that only draws skips both pick-ups below, so nothing stays selected either.
    if (crop && !over) {
      const current = this.crop();
      if (current && contains(current, p)) {
        const object = this.get(CROP_ID)!;
        this.begin({ kind: "move", id: CROP_ID, start: p, origin: object });
        return "object";
      }
    } else if (this.tool !== "crop" && !over) {
      const object = this.objectAt(p, reach, this.tool === null);
      if (object && this.inGroup(object.id)) {
        const origins = new Map(this.group().map((o) => [o.id, o]));
        this.begin({ kind: "group", id: object.id, start: p, origins });
        return "object";
      }
      // Any other press ends the group, as a click beside a selection ends it.
      this.groupIds = [];
      if (object) {
        this.selectedId = object.id;
        this.begin({ kind: "move", id: object.id, start: p, origin: object });
        return "object";
      }
    }
    this.selectedId = null;
    this.groupIds = [];
    if (!this.tool) {
      this.bump();
      return "none";
    }
    const at = this.clampToImage(p);
    const object = this.create(this.tool, at);
    this.before = this.doc.objects;
    if (object.kind === "crop") this.set([object, ...this.doc.objects.filter((o) => o.kind !== "crop")]);
    else this.set([...this.doc.objects, object]);
    if (object.kind === "text") {
      this.editingId = object.id;
      this.selectedId = object.id;
      this.bump();
      return "text";
    }
    this.gesture = { kind: "draw", tool: this.tool, start: at, id: object.id, travelled: 0 };
    return "draw";
  }

  private begin(gesture: Gesture): void {
    this.before = this.doc.objects;
    this.gesture = gesture;
    this.bump();
  }

  pointerMove(p: Point, mods: Modifiers = {}): void {
    const g = this.gesture;
    if (!g) return;
    const object = this.get(g.id);
    if (!object) return;
    if (g.kind === "group") {
      const dx = p.x - g.start.x;
      const dy = p.y - g.start.y;
      this.set(this.doc.objects.map((o) => {
        const origin = g.origins.get(o.id);
        return origin ? moved(origin, dx, dy) : o;
      }));
      return;
    }
    if (g.kind === "move") {
      let dx = p.x - g.start.x;
      let dy = p.y - g.start.y;
      if (object.kind === "crop") {
        const r = (g.origin as Extract<AnnotationObject, { kind: "crop" }>).rect;
        dx = clamp(dx, -r.x, this.doc.image.width - r.x - r.width);
        dy = clamp(dy, -r.y, this.doc.image.height - r.y - r.height);
      }
      this.set(this.replace(g.id, moved(g.origin, dx, dy)));
      return;
    }
    if (g.kind === "handle") {
      const ratio = object.kind === "crop" ? this.ratio() : mods.shift && object.kind !== "pen" && object.kind !== "highlighter" ? 1 : null;
      let to = object.kind === "crop" ? this.clampToImage(p) : p;
      if ((object.kind === "arrow" || object.kind === "line") && mods.shift && g.origin.kind === object.kind) {
        to = snapAngle(g.handle === "from" ? g.origin.to : g.origin.from, p);
      }
      this.set(this.replace(g.id, resized(g.origin, g.handle, to, ratio, textSize)));
      return;
    }
    const at = this.clampToImage(p);
    g.travelled = Math.max(g.travelled, Math.hypot(at.x - g.start.x, at.y - g.start.y));
    let next: AnnotationObject = object;
    switch (object.kind) {
      case "arrow":
      case "line":
        next = { ...object, to: mods.shift ? snapAngle(g.start, at) : at };
        break;
      case "pen":
      case "highlighter": {
        const last = object.points[object.points.length - 1];
        // Points closer than a point apart add weight and no shape.
        if (Math.hypot(at.x - last.x, at.y - last.y) < this.scale) return;
        next = { ...object, points: [...object.points, at] };
        break;
      }
      case "step":
        next = { ...object, center: at };
        break;
      case "crop":
        next = { ...object, rect: boxTo(g.start, at, this.ratio()) };
        break;
      case "rectangle":
      case "ellipse":
      case "blur":
      case "pixelate":
      case "spotlight":
        next = { ...object, rect: boxTo(g.start, at, mods.shift ? 1 : null) };
        break;
      default:
        break;
    }
    this.set(this.replace(g.id, next));
  }

  pointerUp(): void {
    const g = this.gesture;
    if (!g) return;
    this.gesture = null;
    const object = this.get(g.id);
    const before = this.before;
    this.before = null;
    if (!before) return;
    if (g.kind === "draw" && object) {
      // A click without a drag draws nothing, except a step marker, which a click places, and a
      // pen dot.
      const min = 3 * this.scale;
      const keep =
        object.kind === "step" ||
        object.kind === "pen" ||
        (object.kind === "highlighter" && g.travelled >= min) ||
        ((object.kind === "arrow" || object.kind === "line") && g.travelled >= min) ||
        ("rect" in object && object.rect.width >= min && object.rect.height >= min);
      if (!keep) {
        this.set(before);
        return;
      }
      let final = object;
      if (object.kind === "crop") final = { ...object, rect: this.roundRect(object.rect) };
      if (final !== object) this.set(this.replace(object.id, final));
      if ((object.kind === "blur" || object.kind === "pixelate") && !this.prefs.redactionNoticeSeen) this.onRedaction();
    }
    if (object && object.kind === "crop" && g.kind !== "draw") {
      this.set(this.replace(object.id, { ...object, rect: this.roundRect(object.rect) }));
    }
    if (this.doc.objects !== before) {
      this.history.push(this.step(before));
      this.bump();
    }
  }

  /** A crop in whole pixels inside the image. */
  private roundRect(r: Rect): Rect {
    const x0 = clamp(Math.round(r.x), 0, this.doc.image.width);
    const y0 = clamp(Math.round(r.y), 0, this.doc.image.height);
    const x1 = clamp(Math.round(r.x + r.width), 0, this.doc.image.width);
    const y1 = clamp(Math.round(r.y + r.height), 0, this.doc.image.height);
    return rectFrom({ x: x0, y: y0 }, { x: x1, y: y1 });
  }

  /** Ends a gesture without keeping it (Esc during a drag, a lost pointer). */
  cancelGesture(): void {
    if (this.gesture && this.before) this.set(this.before);
    this.gesture = null;
    this.before = null;
  }

  busy(): boolean {
    return this.gesture !== null;
  }

  // --------------------------------------------------------------------------------- text

  /** Starts typing into an existing text object (a double click). */
  editText(id: string): void {
    const object = this.get(id);
    if (!object || object.kind !== "text" || object.locked) return;
    this.finishText();
    this.before = this.doc.objects;
    this.editingId = id;
    this.selectedId = id;
    this.bump();
  }

  /** The text field changed. */
  typeText(text: string): void {
    const object = this.get(this.editingId);
    if (!object || object.kind !== "text" || object.text === text) return;
    this.set(this.replace(object.id, { ...object, text }));
  }

  /** Ends typing: an empty text goes away, any other change is one undo step. */
  finishText(): void {
    const id = this.editingId;
    if (!id) return;
    this.editingId = null;
    const before = this.before ?? this.doc.objects;
    this.before = null;
    const object = this.get(id);
    if (object && object.kind === "text" && object.text.trim() === "") {
      this.selectedId = null;
      this.set(this.doc.objects.filter((o) => o.id !== id));
    }
    if (this.doc.objects !== before) {
      // A text that was created and emptied again leaves the list as it was.
      const same = before.length === this.doc.objects.length && before.every((o, i) => o === this.doc.objects[i]);
      if (!same) this.history.push(this.step(before));
      else this.set(before);
    }
    this.bump();
  }

  // ------------------------------------------------------------------------------ keyboard

  /**
   * The keys the engine answers, whichever surface it is on: ⌫ deletes, ⌘D duplicates, ⌘] and ⌘[
   * change the order, ⌘L locks, ⇧⌘H hides, ⌥C changes the colour, the arrows nudge (⇧ by ten),
   * ⌘A picks every object up as a group, and Esc ends a gesture or drops the selection. A group
   * answers only to ⌫, the arrows and Esc. Returns whether the key was used.
   */
  key(e: KeyboardEvent): boolean {
    if (this.editingId) return false;
    const command = e.metaKey || e.ctrlKey;
    if (e.key === "Escape") {
      if (this.gesture) {
        this.cancelGesture();
        return true;
      }
      if (this.selectedId || this.groupIds.length) {
        this.select(null);
        return true;
      }
      return false;
    }
    // ⌘A is always the engine's, even with nothing drawn, so the page never selects its own text.
    if (command && !e.shiftKey && !e.altKey && e.code === "KeyA") {
      if (!this.gesture) this.selectAll();
      return true;
    }
    const arrows: Record<string, [number, number]> = { ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1] };
    const step = arrows[e.key];
    if (this.groupIds.length) {
      if ((e.key === "Backspace" || e.key === "Delete") && !command) {
        this.deleteSelected();
        return true;
      }
      if (step && !command) {
        const by = (e.shiftKey ? 10 : 1) * this.scale;
        this.nudge(step[0] * by, step[1] * by);
        return true;
      }
      return false;
    }
    if (e.altKey && !command && e.code === "KeyC") {
      this.cycleInk();
      return true;
    }
    const object = this.selected();
    if (!object) return false;
    if ((e.key === "Backspace" || e.key === "Delete") && !command) {
      this.deleteSelected();
      return true;
    }
    if (command && !e.shiftKey && e.key.toLowerCase() === "d") {
      this.duplicate();
      return true;
    }
    if (command && (e.key === "]" || e.key === "[")) {
      this.shift(object.id, e.key === "]" ? 1 : -1);
      return true;
    }
    if (command && !e.shiftKey && e.key.toLowerCase() === "l") {
      this.setLocked(object.id, !object.locked);
      return true;
    }
    if (command && e.shiftKey && e.key.toLowerCase() === "h") {
      this.setHidden(object.id, !object.hidden);
      return true;
    }
    if (step && !command) {
      const by = (e.shiftKey ? 10 : 1) * this.scale;
      this.nudge(step[0] * by, step[1] * by);
      return true;
    }
    if (object.kind === "text" && e.key === "Enter" && !command) {
      this.editText(object.id);
      return true;
    }
    return false;
  }

}
