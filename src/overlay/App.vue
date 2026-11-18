<script setup lang="ts">
// The capture overlay of one display (DESIGN.md "Overlay"): the frozen frame under the dim, the
// crosshair and the loupe, a drag selection with eight handles, moving and nudging, window
// snapping, and the two glass bars. Copy, Save and Close work. The annotation tools draw through
// the seam in `annotations.ts` (milestone 2): with drawings, Copy and Save send the rendered
// selection to the core, and ⌘O hands the capture with its drawings to the editor.
//
// The page waits warm in a hidden window. On `capture://started` it fetches and draws its frame
// while still invisible, then calls `overlay_ready`, and the core shows the window.
import { computed, nextTick, onMounted, onUnmounted, provide, reactive, ref, shallowRef, watch, watchEffect } from "vue";

import GIcon from "../components/GIcon.vue";
import OptionsStrip from "../annotate/OptionsStrip.vue";
import RedactionNotice from "../annotate/RedactionNotice.vue";
import Surface from "../annotate/Surface.vue";
import type { ToolId as AnnotateTool } from "../annotate/prefs";
import { TEXT_LAYER, textLayerComponent } from "../annotate/textLayer";
import { thumbnail, thumbnailOfPixels, uploadFlow } from "../annotate/upload";
import {
  editor_open_capture,
  output_copy_rendered,
  output_save_rendered,
  output_stage_rendered,
  upload_capture_rendered,
  type RenderedImage,
} from "../lib/api/render";
import {
  capture_close,
  capture_current,
  overlay_painted,
  overlay_ready,
  type CaptureClosed,
  type OverlayFrame,
  type OverlayWindow,
} from "../lib/api/capture";
import { ApiError, on, onWindow, send } from "../lib/api/core";
import { output_copy, output_save, output_save_as } from "../lib/api/output";
import { record_setup } from "../lib/api/record";
import { output_save_as_rendered } from "../lib/api/render";
import { upload_capture } from "../lib/api/upload";
import { pin_open } from "../lib/api/windows";
import { CAPTURE_CLOSED, CAPTURE_STARTED, OVERLAY_SELECTION_ENDED, OVERLAY_SELECTION_STARTED } from "../lib/events";
import { t, type Key } from "../lib/i18n";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import * as annotations from "./annotations";
import { cssColor, drawLens, ensureContext, hex, loadFrame, pixelAt } from "./frame";
import { place, placeLoupe, type Placement, type Rect } from "./layout";
import { ACTIONS, TOOLS, WORKING_ACTIONS, actionForChord, toolForKey, type ActionId, type ToolId } from "./tools";
import FirstUploadDialog from "../upload/FirstUploadDialog.vue";
import "./overlay.css";
import * as modes from "./modes";

type Mode = "idle" | "drag" | "sel" | "move" | "resize";
type Point = { x: number; y: number };

/** The smallest selection; a smaller drag is a click. */
const MIN_SELECTION = 6;
/** A handle's hit radius (its 28 px hit area). */
const HANDLE_HIT = 14;
const LENS_CELLS = 17;
const LENS_SIZE = 136;
/** How long a pointer move may wait for its animation frame before it is handled at once: WebKit
 * holds back the frames of a window it believes occluded, and the overlay follows the pointer
 * even then. */
const MOVE_WAIT_MS = 48;
/** The selection's iris line outside its edge, in points (the kit's `.g-selection::before`). */
const EDGE_IRIS = 1.5;
const HANDLES: [number, number, "x" | "y" | null][] = [
  [0, 0, null],
  [0.5, 0, "x"],
  [1, 0, null],
  [1, 0.5, "y"],
  [1, 1, null],
  [0.5, 1, "x"],
  [0, 1, null],
  [0, 0.5, "y"],
];

const displayId = Number(getCurrentWebviewWindow().label.replace("overlay-", ""));

// The frame, the selection, the pointer and the hovered window are replaced, never changed in
// place, so shallow refs spare a proxy on every read in the pointer's path.
const frame = shallowRef<OverlayFrame | null>(null);
const mode = ref<Mode>("idle");
const windowMode = ref(false);
const sel = shallowRef<Rect | null>(null);
const pointer = shallowRef<Point | null>(null);
const hovered = shallowRef<OverlayWindow | null>(null);
const tool = ref<ToolId | null>(null);
const barsShown = ref(false);
const placement = shallowRef<Placement | null>(null);
const hintFaded = ref(false);
const notice = ref<string | null>(null);
const allTips = ref(false);
const cursor = ref("crosshair");
/** The display that holds the selection when it is another one. This display then waits: no
 * crosshair, no loupe and no window hover, and a drag here starts a new selection, as a press
 * outside the selection does on its own display. */
const heldElsewhere = ref<number | null>(null);
const view = reactive({ w: window.innerWidth, h: window.innerHeight });
const loupeShown = ref(false);

const frameHost = ref<HTMLElement | null>(null);
const lensHost = ref<HTMLElement | null>(null);
const toolsEl = ref<HTMLElement | null>(null);
const actionsEl = ref<HTMLElement | null>(null);
const stripEl = ref<HTMLElement | null>(null);
const chipEl = ref<HTMLElement | null>(null);
// What follows the pointer, written without a render of the page (see "per-frame painting").
const dimEl = ref<HTMLElement | null>(null);
const edgeEl = ref<HTMLElement | null>(null);
const sizeEl = ref<HTMLElement | null>(null);
const lineH = ref<HTMLElement | null>(null);
const lineV = ref<HTMLElement | null>(null);
const loupeEl = ref<HTMLElement | null>(null);
const swatchEl = ref<HTMLElement | null>(null);
const hexEl = ref<HTMLElement | null>(null);
const xyEl = ref<HTMLElement | null>(null);

let ctx: CanvasRenderingContext2D | null = null;
let lens: CanvasRenderingContext2D | null = null;
let pixels: Uint8ClampedArray | null = null;
/** The frame pixel the lens shows, so a move within one pixel draws nothing; -1 for a new frame. */
const lensAt = { x: -1, y: -1 };
/** The newest pointer move not handled yet: moves are handled once per animation frame. */
let pendingMove: MouseEvent | null = null;
let moveFrame = 0;
let movedAt = 0;
let start: Point | null = null;
let grab: { offset: Point; origin: Rect; handle: [number, number] | null } | null = null;
let busy = false;
/** The window a click took, while the selection is still that window (for Record). */
let pickedWindow: number | null = null;
let hintTimer = 0;
let noticeTimer = 0;

// ------------------------------------------------------------------------------- geometry

const clamp = (v: number, a: number, b: number) => Math.max(a, Math.min(b, v));
/** Frame pixels per point on each axis. */
const ratio = computed(() => {
  const f = frame.value;
  return f ? { x: f.frameSize.width / view.w, y: f.frameSize.height / view.h } : { x: 1, y: 1 };
});
/** A point in frame pixels, the annotation document's coordinates. */
const toFrame = (p: Point) => ({ x: p.x * ratio.value.x, y: p.y * ratio.value.y });
/** How far a press may miss a handle, in frame pixels. */
const reach = computed(() => HANDLE_HIT * ratio.value.x);
/** A selection in frame pixels, each edge rounded as the core rounds it (`selection_to_frame`). */
function frameRect(r: Rect): { x: number; y: number; width: number; height: number } | null {
  const f = frame.value;
  if (!f) return null;
  const { width: fw, height: fh } = f.frameSize;
  const x0 = clamp(Math.round(r.x * ratio.value.x), 0, fw);
  const y0 = clamp(Math.round(r.y * ratio.value.y), 0, fh);
  const x1 = clamp(Math.round((r.x + r.w) * ratio.value.x), 0, fw);
  const y1 = clamp(Math.round((r.y + r.h) * ratio.value.y), 0, fh);
  return x1 > x0 && y1 > y0 ? { x: x0, y: y0, width: x1 - x0, height: y1 - y0 } : null;
}
const box = (r: Rect) => ({ left: `${r.x}px`, top: `${r.y}px`, width: `${r.w}px`, height: `${r.h}px` });
const at = (p: { x: number; y: number }) => ({ left: `${p.x}px`, top: `${p.y}px` });

function fromCorners(a: Point, b: Point): Rect {
  const x0 = clamp(Math.min(a.x, b.x), 0, view.w);
  const y0 = clamp(Math.min(a.y, b.y), 0, view.h);
  const x1 = clamp(Math.max(a.x, b.x), 0, view.w);
  const y1 = clamp(Math.max(a.y, b.y), 0, view.h);
  return { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
}

const inside = (r: Rect, p: Point) => p.x > r.x && p.y > r.y && p.x < r.x + r.w && p.y < r.y + r.h;

function windowAt(p: Point): OverlayWindow | null {
  const f = frame.value;
  if (!f) return null;
  return f.windows.find((w) => inside({ x: w.rect.x, y: w.rect.y, w: w.rect.width, h: w.rect.height }, p)) ?? null;
}

function handleAt(r: Rect, p: Point): [number, number] | null {
  for (const [fx, fy] of HANDLES) {
    if (Math.hypot(p.x - (r.x + fx * r.w), p.y - (r.y + fy * r.h)) <= HANDLE_HIT) return [fx, fy];
  }
  return null;
}

const RESIZE_CURSORS: Record<string, string> = {
  "0,0": "nwse-resize",
  "1,1": "nwse-resize",
  "1,0": "nesw-resize",
  "0,1": "nesw-resize",
  "0.5,0": "ns-resize",
  "0.5,1": "ns-resize",
  "0,0.5": "ew-resize",
  "1,0.5": "ew-resize",
};

// ---------------------------------------------------------------------------- what shows

// The template reads only these yes-or-no answers about the selection, which change at its start
// and end, so a drag does not render the page again; the moving parts are painted below.
const hasSel = computed(() => sel.value !== null);
const sizeShown = computed(() => sel.value !== null && sel.value.w > 0);
const showLines = computed(() => mode.value === "idle" && !windowMode.value && heldElsewhere.value === null && pointer.value !== null);
type HandleAt = { key: string; kind: "x" | "y" | null; x: number; y: number };
/** One empty list for every state without handles, so a drag leaves the computed unchanged. */
const NO_HANDLES: HandleAt[] = [];
const handles = computed<HandleAt[]>(() => {
  const r = sel.value;
  if (!r || mode.value !== "sel") return NO_HANDLES;
  return HANDLES.map(([fx, fy, kind]) => ({ key: `${fx},${fy}`, kind, x: r.x + fx * r.w, y: r.y + fy * r.h }));
});
const chip = computed(() => {
  const w = hovered.value;
  if (!w || sel.value) return null;
  const name = w.title ? `${w.appName} — ${w.title}` : w.appName;
  return {
    name,
    size: `${Math.round(w.rect.width * ratio.value.x)}`,
    sizeY: `${Math.round(w.rect.height * ratio.value.y)}`,
    cx: w.rect.x + w.rect.width / 2,
    cy: w.rect.y + w.rect.height / 2,
  };
});
const chipAt = ref({ x: 0, y: 0 });

// The annotation engine is not reactive itself; its `rev` counts its changes.
const engine = annotations.engine;
const annotationView = computed(() => ({ k: 1 / ratio.value.x, ox: 0, oy: 0 }));
const liveArea = computed(() => (sel.value ? frameRect(sel.value) : null));
const drawn = computed(() => (engine.rev.value, engine.hasDrawings()));
const undoable = computed(() => (engine.rev.value, annotations.canUndo()));
const redoable = computed(() => (engine.rev.value, annotations.canRedo()));
/** The strip follows the selected object, or the active tool. */
const stripTool = computed<AnnotateTool | null>(() => (engine.rev.value, engine.optionsTool()));
const noticeAt = computed(() => {
  const p = placement.value;
  if (!p?.strip) return null;
  return { x: clamp(p.strip.x, 8, view.w - 412), y: p.strip.y + 44 };
});

// The text recognition layer reads the selection through this (src/annotate/textLayer.ts).
const textLayerVisible = ref(false);
provide(TEXT_LAYER, {
  area: liveArea,
  view: annotationView,
  pixels: () => (liveArea.value ? annotations.pixels(liveArea.value) : null),
  colorSpace: computed(() => frame.value?.colorSpace ?? "srgb"),
  scale: computed(() => ratio.value.x),
  visible: textLayerVisible,
});

// --------------------------------------------------------------------- per-frame painting
//
// The parts that follow the pointer (the dim, the selection's edge, the size label, the crosshair
// and the loupe) are written straight to the DOM, so a move costs a few style writes instead of a
// render of the whole page with its bars. Each moves by `transform` on a layer of its own, and the
// dim and the edge are boxes of one solid colour, which WebKit composites without a backing store:
// moving the selection repaints nothing. The kit's dim, one box-shadow around the hole, repainted
// the whole display at its full resolution on every move.

const dpr = () => window.devicePixelRatio || 1;
const written = new WeakMap<HTMLElement, Record<string, string>>();

/** Sets one style property, and skips a write that would change nothing. */
function put(el: HTMLElement, name: string, value: string): void {
  let seen = written.get(el);
  if (!seen) written.set(el, (seen = {}));
  if (seen[name] === value) return;
  seen[name] = value;
  el.style.setProperty(name, value);
}

function text(el: Element | null, value: string): void {
  if (el && el.textContent !== value) el.textContent = value;
}

/** A box of `w` × `h` points at (`x`, `y`), moved by its transform. */
function setBox(el: HTMLElement, x: number, y: number, w: number, h: number): void {
  put(el, "transform", `translate(${x}px, ${y}px)`);
  put(el, "width", `${Math.max(0, w)}px`);
  put(el, "height", `${Math.max(0, h)}px`);
}

/** A coordinate in WebKit's layout units: a CSS length is cut to 1/64 point. */
const lu = (v: number) => Math.trunc(v * 64) / 64;

/** A coordinate on the device pixel grid, rounded half away from zero, the way WebKit snaps the
 * edges of a box. The kit's boxes land on these pixels, so the painted parts land there too, and
 * two neighbours share an edge with no seam and no doubled dim between them. */
function snap(v: number, d: number): number {
  const n = lu(v) * d;
  return (n < 0 ? -Math.round(-n) : Math.round(n)) / d;
}

/** The dim around `hole`, or over the whole display without one: four bands, and four corner
 * pieces where a window's hole is rounded. */
function paintDim(host: HTMLElement, hole: Rect | null, radius: number): void {
  const [top, bottom, left, right, tl, tr, bl, br] = Array.from(host.children) as HTMLElement[];
  const d = dpr();
  const W = view.w;
  const H = view.h;
  let l = 0;
  let t = 0;
  let r = 0;
  let b = 0;
  if (hole) {
    l = snap(hole.x, d);
    t = snap(hole.y, d);
    r = snap(lu(hole.x) + lu(hole.w), d);
    b = snap(lu(hole.y) + lu(hole.h), d);
  }
  const cl = clamp(l, 0, W);
  const cr = clamp(r, cl, W);
  const ct = clamp(t, 0, H);
  const cb = clamp(b, ct, H);
  setBox(top, 0, 0, W, ct);
  setBox(bottom, 0, cb, W, H - cb);
  setBox(left, 0, ct, cl, cb - ct);
  setBox(right, cr, ct, W - cr, cb - ct);
  // CSS shrinks radii that do not fit the box. A piece spans whole device pixels, so its layer is
  // never resampled along the edges it shares with the bands; its circle stays where the hole's
  // corner is (overlay.css).
  const k = radius > 0 ? Math.min(radius, (r - l) / 2, (b - t) / 2) : 0;
  const s = k > 0 ? Math.ceil(k * d) / d : 0;
  put(host, "--o-dim-r", `${k}px`);
  put(host, "--o-dim-far", `${s - 2 * k}px`);
  setBox(tl, l, t, s, s);
  setBox(tr, r - s, t, s, s);
  setBox(bl, l, b - s, s, s);
  setBox(br, r - s, b - s, s, s);
}

/** Four strips around the box (`l`, `t`)–(`r`, `b`) less the hole (`hl`, `ht`)–(`hr`, `hb`); a
 * ring without a hole is a filled box, as WebKit draws a border or an inset shadow too wide for
 * its box. */
function ring(strips: HTMLElement[], l: number, t: number, r: number, b: number, hl: number, ht: number, hr: number, hb: number): void {
  if (hr <= hl || hb <= ht) {
    hl = hr = l;
    ht = hb = b;
  }
  const [top, bottom, left, right] = strips;
  setBox(top, l, t, r - l, ht - t);
  setBox(bottom, l, hb, r - l, b - hb);
  setBox(left, l, ht, hl - l, hb - ht);
  setBox(right, hr, ht, r - hr, hb - ht);
}

/** The selection's edge on the pixels where the kit's `.g-selection` draws it: its iris border
 * outside the box (a border is a whole number of device pixels, at least one) and its 1 pt light
 * inset hairline inside. */
function paintEdge(host: HTMLElement, r: Rect): void {
  const strips = Array.from(host.children) as HTMLElement[];
  const d = dpr();
  const x0 = lu(r.x);
  const y0 = lu(r.y);
  const x1 = x0 + lu(r.w);
  const y1 = y0 + lu(r.h);
  const o = EDGE_IRIS;
  const bw = Math.max(1, Math.floor(o * d)) / d;
  const at = (v: number) => snap(v, d);
  ring(strips.slice(0, 4), at(x0 - o), at(y0 - o), at(x1 + o), at(y1 + o), at(x0 - o + bw), at(y0 - o + bw), at(x1 + o - bw), at(y1 + o - bw));
  ring(strips.slice(4, 8), at(x0), at(y0), at(x1), at(y1), at(x0 + 1), at(y0 + 1), at(x1 - 1), at(y1 - 1));
}

// Post-flush effects run after the render that mounts their elements, in the same task as the
// change, so nothing shows in a stale place.
watchEffect(
  () => {
    const host = dimEl.value;
    if (!host) return;
    const s = sel.value;
    const w = s ? null : hovered.value;
    paintDim(host, s ?? (w && { x: w.rect.x, y: w.rect.y, w: w.rect.width, h: w.rect.height }), w ? (w.cornerRadius ?? 10) : 0);
  },
  { flush: "post" },
);
watchEffect(
  () => {
    const host = edgeEl.value;
    const r = sel.value;
    if (host && r) paintEdge(host, r);
  },
  { flush: "post" },
);
watchEffect(
  () => {
    const el = sizeEl.value;
    const r = sel.value;
    if (!el || !r) return;
    const p = placement.value && mode.value === "sel" ? placement.value.size : r.y - 30 >= 8 ? { x: r.x, y: r.y - 30 } : { x: r.x + 6, y: r.y + 6 };
    const d = dpr();
    put(el, "transform", `translate(${snap(p.x, d)}px, ${snap(p.y, d)}px)`);
    text(el.firstElementChild, `${Math.round(r.w * ratio.value.x)}`);
    text(el.lastElementChild, `${Math.round(r.h * ratio.value.y)}`);
  },
  { flush: "post" },
);
watchEffect(
  () => {
    const h = lineH.value;
    const v = lineV.value;
    // `showLines` first: a hidden crosshair does not wake for every move.
    if (!h || !v || !showLines.value || !pointer.value) return;
    const p = pointer.value;
    const d = dpr();
    put(h, "transform", `translate(0px, ${snap(p.y, d)}px)`);
    put(v, "transform", `translate(${snap(p.x, d)}px, 0px)`);
  },
  { flush: "post" },
);

// ---------------------------------------------------------------------------------- loupe

function updateLoupe(p: Point): void {
  if (!ctx || !lens || !frame.value) return;
  const px = clamp(Math.floor(p.x * ratio.value.x), 0, ctx.canvas.width - 1);
  const py = clamp(Math.floor(p.y * ratio.value.y), 0, ctx.canvas.height - 1);
  // A move within one frame pixel shows the same lens and the same colour.
  if (px !== lensAt.x || py !== lensAt.y) {
    lensAt.x = px;
    lensAt.y = py;
    drawLens(lens, ctx.canvas, px, py, LENS_CELLS);
    const rgb = pixelAt(ctx, pixels, px, py);
    text(hexEl.value, hex(rgb));
    text(xyEl.value, `${px}, ${py}`);
    if (swatchEl.value) put(swatchEl.value, "background", cssColor(rgb, frame.value.colorSpace));
  }
  const placed = placeLoupe(p.x, p.y, view);
  if (loupeEl.value) put(loupeEl.value, "transform", `translate(${placed.x}px, ${placed.y}px)`);
  loupeShown.value = true;
}

// ----------------------------------------------------------------------------- the bars

async function layoutBars(): Promise<void> {
  const r = sel.value;
  if (!r || !toolsEl.value || !actionsEl.value) return;
  await nextTick();
  const tools = toolsEl.value;
  const actions = actionsEl.value;
  const strip = stripEl.value;
  const active = tools.querySelector<HTMLElement>('.g-btn[aria-pressed="true"]');
  const center = active ? active.parentElement!.offsetLeft + active.offsetLeft + active.offsetWidth / 2 : 0;
  const widestTip = Math.max(0, ...[...actions.querySelectorAll<HTMLElement>(".g-tip")].map((e) => e.offsetWidth));
  placement.value = place(
    r,
    view,
    { w: tools.offsetWidth, h: tools.offsetHeight },
    { w: actions.offsetWidth, h: actions.offsetHeight },
    strip ? { w: strip.offsetWidth, h: strip.offsetHeight, center: active ? center : strip.offsetWidth / 2 } : null,
    widestTip,
  );
  await nextTick();
  // A tool tip near a screen edge pins to the button's side.
  const p = placement.value;
  for (const wrap of tools.querySelectorAll<HTMLElement>(".g-tipwrap")) {
    const tip = wrap.lastElementChild as HTMLElement;
    const half = tip.offsetWidth / 2;
    const cx = p.tools.x + wrap.offsetLeft + wrap.offsetWidth / 2;
    wrap.removeAttribute("data-align");
    if (cx - half < 8) wrap.dataset.align = "start";
    else if (cx + half > view.w - 8) wrap.dataset.align = "end";
  }
}

async function showBars(): Promise<void> {
  await layoutBars();
  requestAnimationFrame(() => (barsShown.value = true));
}

// ----------------------------------------------------------------------------- the flow

function resetSelection(): void {
  mode.value = "idle";
  sel.value = null;
  hovered.value = null;
  barsShown.value = false;
  placement.value = null;
  start = null;
  grab = null;
  pickedWindow = null;
  annotations.clear();
}

function resetAll(): void {
  resetSelection();
  heldElsewhere.value = null;
  windowMode.value = false;
  tool.value = null;
  annotations.setTool(null);
  textLayerVisible.value = false;
  uploads.cancel();
  pendingMove = null;
  pointer.value = null;
  loupeShown.value = false;
  lensAt.x = lensAt.y = -1;
  hintFaded.value = false;
  notice.value = null;
  allTips.value = false;
  clearTimeout(hintTimer);
  clearTimeout(noticeTimer);
}

async function begin(next: OverlayFrame): Promise<void> {
  if (next.display.id !== displayId) return;
  resetAll();
  frame.value = next;
  windowMode.value = next.mode === "window";
  view.w = window.innerWidth;
  view.h = window.innerHeight;

  if (!frameHost.value || !lensHost.value) return;
  ctx = ensureContext(frameHost.value, next.colorSpace, "frame");
  lens = ensureContext(lensHost.value, next.colorSpace, "lens");
  // Setting a canvas's size clears and reallocates it even when the size is the same.
  const lensPixels = LENS_SIZE * dpr();
  if (lens.canvas.width !== lensPixels) lens.canvas.width = lensPixels;
  if (lens.canvas.height !== lensPixels) lens.canvas.height = lensPixels;

  let loaded;
  try {
    loaded = await loadFrame(ctx, next);
  } catch (error) {
    console.error("[glimpsfy] the frame did not load", error);
    await modes.reportFailure(next, displayId, error);
    return;
  }
  pixels = loaded.pixels;
  annotations.attach(next, ctx.canvas, loaded.pixels, view.w);
  if (next.pointer) {
    pointer.value = next.pointer;
    updateLoupe(next.pointer);
  }
  await nextTick();
  if (frame.value?.captureId !== next.captureId) return;

  // Only the timings go into the frame callbacks: WebKit holds back the frames of a hidden
  // window, and a callback holding `loaded` would keep the frame's pixels alive after the capture.
  const { fetchMs, decodeMs, drawMs, bytes } = loaded;
  await overlay_ready(next.captureId, displayId);
  const ready = performance.now();
  requestAnimationFrame(() =>
    requestAnimationFrame(() => {
      void overlay_painted(next.captureId, displayId, { fetchMs, decodeMs, drawMs, paintMs: performance.now() - ready, bytes });
    }),
  );
  hintTimer = window.setTimeout(() => (hintFaded.value = true), 2000);
}

function end(closed: CaptureClosed): void {
  if (frame.value?.captureId !== closed.captureId) return;
  resetAll();
  frame.value = null;
  pixels = null;
  annotations.detach();
}

function finishSelection(r: Rect, windowId: number | null = null): void {
  sel.value = r;
  pickedWindow = windowId;
  annotations.setSelection(frameRect(r));
  hovered.value = null;
  mode.value = "sel";
  loupeShown.value = false;
  hintFaded.value = true;
  void finishByMode(r, windowId);
}

/** The mode's action on a finished selection (`modes.ts`): the bars, or copy, save or copy text. */
async function finishByMode(r: Rect, windowId: number | null): Promise<void> {
  const f = frame.value;
  if (!f) return;
  busy = true;
  try {
    const selection = { captureId: f.captureId, displayId, rect: { x: r.x, y: r.y, width: r.w, height: r.h } };
    const finished = await modes.finish(f, selection, windowId);
    if (finished === "empty") say(t("ocr.empty"));
    if (finished !== "done") void showBars();
  } catch (error) {
    say(errorText(error));
    void showBars();
  } finally {
    busy = false;
  }
}

// A selection on this display holds the capture: the other displays drop theirs, and the core
// keeps the keyboard here wherever the pointer goes (`windows::follow_pointer`). A press that
// starts over on this display empties and refills `sel` in one tick, which sends nothing.
watch(
  () => sel.value !== null,
  (held) => {
    const f = frame.value;
    if (f) void send(held ? OVERLAY_SELECTION_STARTED : OVERLAY_SELECTION_ENDED, { captureId: f.captureId, displayId });
  },
);

function close(): void {
  if (frame.value) void capture_close(frame.value.captureId);
}

function say(text: string): void {
  notice.value = text;
  clearTimeout(noticeTimer);
  noticeTimer = window.setTimeout(() => (notice.value = null), 2400);
}

function errorText(error: unknown): string {
  const code = error instanceof ApiError ? error.code : "internal";
  const key = `error.${code}` as Key;
  const known: Key[] = ["error.permission_denied", "error.not_implemented", "error.io", "error.not_found"];
  return t(known.includes(key) ? key : "error.other");
}

async function act(id: ActionId): Promise<void> {
  if (id === "close") return close();
  if (id === "copy-text") return toggleTextLayer();
  if (id === "record") return record();
  if (id === "upload") {
    if (frame.value && sel.value && !busy && mode.value === "sel") void uploads.start();
    return;
  }
  if (!WORKING_ACTIONS.includes(id)) return say(t("overlay.notReady"));
  const f = frame.value;
  const r = sel.value;
  if (!f || !r || busy || mode.value !== "sel") return;
  busy = true;
  const selection = { captureId: f.captureId, displayId, rect: { x: r.x, y: r.y, width: r.w, height: r.h } };
  try {
    engine.finishText();
    const rendered = renderedSelection(f, r);
    if (id === "pin") {
      // A pin with drawings opens where the selection was, as a plain capture's pin does.
      await pin_open(rendered ? { kind: "rendered", path: await output_stage_rendered(rendered), selection } : { kind: "capture", selection });
      await capture_close(f.captureId, "pinned");
    } else if (rendered) {
      await (id === "copy" ? output_copy_rendered(rendered) : output_save_rendered(rendered));
    } else {
      await (id === "copy" ? output_copy(selection) : output_save(selection));
    }
  } catch (error) {
    say(errorText(error));
  } finally {
    busy = false;
  }
}

/** Save as (⇧⌘S): the save panel, then a new file in the format of the chosen extension. A
 * cancelled panel brings the overlay back with the selection as it was. */
async function saveAs(): Promise<void> {
  const f = frame.value;
  const r = sel.value;
  if (!f || !r || busy || mode.value !== "sel") return;
  busy = true;
  try {
    engine.finishText();
    const rendered = renderedSelection(f, r);
    const selection = { captureId: f.captureId, displayId, rect: { x: r.x, y: r.y, width: r.w, height: r.h } };
    await (rendered ? output_save_as_rendered(rendered) : output_save_as(selection));
  } catch (error) {
    if (!(error instanceof ApiError && error.code === "cancelled")) say(errorText(error));
  } finally {
    busy = false;
  }
}

/** Record (⌘R): the selection, or the window a click took, becomes the target of a recording's
 * setup, and the capture closes so the setup's frame is what shows. */
async function record(): Promise<void> {
  const f = frame.value;
  const r = sel.value;
  if (!f || !r || busy || mode.value !== "sel") return;
  busy = true;
  try {
    const target =
      pickedWindow !== null
        ? { mode: "window" as const, displayId, windowId: pickedWindow }
        : { mode: "area" as const, displayId, rect: { x: r.x, y: r.y, width: r.w, height: r.h } };
    await record_setup(target);
    await capture_close(f.captureId, "recording");
  } catch (error) {
    say(errorText(error));
  } finally {
    busy = false;
  }
}

/** With drawings, the page renders the selection at the frame's resolution and the core encodes
 * and delivers those pixels; without them, `null`, and the core crops the frame itself. */
function renderedSelection(f: OverlayFrame, r: Rect): RenderedImage | null {
  if (!annotations.hasDrawings()) return null;
  annotations.setSelection(frameRect(r));
  const image = annotations.render();
  if (!image) throw new ApiError("invalid_argument", "the selection is empty");
  return { source: { kind: "capture", captureId: f.captureId, displayId }, ...image };
}

/** Copy text (⇧⌘C) shows the text recognition layer over the selection, or hides it. */
function toggleTextLayer(): void {
  if (!frame.value || !sel.value || mode.value !== "sel") return;
  if (!textLayerComponent) return say(t("overlay.notReady"));
  textLayerVisible.value = !textLayerVisible.value;
}

/** Upload (⌘U): the selection, or its rendering with the drawings, once the provider is checked
 * and, the first time, confirmed. */
const uploads = uploadFlow({
  async send(provider) {
    const f = frame.value;
    const r = sel.value;
    if (!f || !r || busy) return;
    busy = true;
    try {
      engine.finishText();
      const rendered = renderedSelection(f, r);
      const selection = { captureId: f.captureId, displayId, rect: { x: r.x, y: r.y, width: r.w, height: r.h } };
      await (rendered ? upload_capture_rendered(rendered, provider) : upload_capture(selection, provider));
    } finally {
      busy = false;
    }
  },
  file() {
    const f = frame.value;
    const r = sel.value;
    if (!f || !r) return null;
    const rendered = renderedSelection(f, r);
    const area = frameRect(r);
    const thumb = rendered ? thumbnailOfPixels(rendered) : area && ctx ? thumbnail(ctx.canvas, area) : null;
    const size = rendered ?? area;
    return { name: t("annotate.upload.selection"), thumbnail: thumb, width: size?.width ?? null, height: size?.height ?? null, format: "PNG", bytes: null };
  },
  say,
});
const uploadAsk = uploads.asking;

/** Opens the capture in the editor with its drawings (⌘O); the overlay closes. */
async function handOff(): Promise<void> {
  const f = frame.value;
  const r = sel.value;
  if (!f || !r || busy || mode.value !== "sel") return;
  busy = true;
  try {
    engine.finishText();
    annotations.setSelection(frameRect(r));
    await editor_open_capture(f.captureId, displayId, annotations.documentJson());
  } catch (error) {
    say(errorText(error));
  } finally {
    busy = false;
  }
}

function setTool(id: ToolId): void {
  if (id === "undo") return annotations.undo();
  if (id === "redo") return annotations.redo();
  tool.value = tool.value === id ? null : id;
  annotations.setTool(tool.value as AnnotateTool | null);
  void layoutBars();
}

// ---------------------------------------------------------------------------- the pointer

function point(e: MouseEvent): Point {
  return { x: clamp(e.clientX, 0, view.w), y: clamp(e.clientY, 0, view.h) };
}

/** Handles the move waiting for its frame now, so a press, a release or a key sees the pointer
 * where it is and a selection ends exactly where the drag did. */
function flushMove(): void {
  const e = pendingMove;
  pendingMove = null;
  if (e) handleMove(e);
}

function onMoveFrame(): void {
  moveFrame = 0;
  flushMove();
}

// Moves are handled at most once per animation frame, with the newest position: the screen can
// show no more than that, and a fast mouse sends several moves per frame.
function onMouseMove(e: MouseEvent): void {
  // A drawing gesture takes every move, so a pen stroke keeps each point the pointer passed.
  if (mode.value === "sel" && annotations.busy()) {
    flushMove();
    handleMove(e);
    return;
  }
  pendingMove = e;
  const now = performance.now();
  if (!moveFrame) {
    movedAt = now;
    moveFrame = requestAnimationFrame(onMoveFrame);
  } else if (now - movedAt > MOVE_WAIT_MS) {
    movedAt = now;
    flushMove();
  }
}

function onMouseDown(e: MouseEvent): void {
  flushMove();
  if (e.button !== 0 || !frame.value) return;
  const p = point(e);
  e.preventDefault();

  if (mode.value === "sel" && sel.value) {
    const fp = toFrame(p);
    // With ⌘ held a press only draws: inside the selection the tool starts a new object even on
    // top of another one or on a handle, and no object is picked up.
    const over = e.metaKey;
    if (over && engine.tool && inside(sel.value, p)) {
      annotations.press(fp, reach.value, true, true);
      return;
    }
    // The selected object's handles come before the selection's, wherever they are.
    if (!over && engine.handleAt(fp, reach.value) && annotations.press(fp, reach.value, true) !== "none") return;
    const handle = handleAt(sel.value, p);
    if (handle) {
      mode.value = "resize";
      grab = { offset: { x: 0, y: 0 }, origin: { ...sel.value }, handle };
      barsShown.value = false;
      return;
    }
    if (inside(sel.value, p)) {
      // With a tool, a press draws; without one, it picks an object, and on empty ground it
      // moves the selection.
      if (annotations.press(fp, reach.value, true, over) !== "none") return;
      mode.value = "move";
      grab = { offset: { x: p.x - sel.value.x, y: p.y - sel.value.y }, origin: { ...sel.value }, handle: null };
      barsShown.value = false;
      return;
    }
  }

  // While another display holds the selection, no window shows as hovered here, so a press
  // draws a new selection rather than taking a window the user could not see.
  if (mode.value === "idle" && windowMode.value && heldElsewhere.value === null) {
    const w = windowAt(p);
    if (w) finishSelection(fromCorners({ x: w.rect.x, y: w.rect.y }, { x: w.rect.x + w.rect.width, y: w.rect.y + w.rect.height }), w.id);
    return;
  }
  heldElsewhere.value = null;
  resetSelection();
  annotations.clear();
  mode.value = "drag";
  start = p;
  sel.value = { x: p.x, y: p.y, w: 0, h: 0 };
  hintFaded.value = true;
  cursor.value = "crosshair";
  updateLoupe(p);
}

function handleMove(e: MouseEvent): void {
  if (!frame.value) return;
  const p = point(e);
  pointer.value = p;
  if (mode.value !== "sel") annotations.hover(null, reach.value);

  switch (mode.value) {
    case "idle":
      if (heldElsewhere.value !== null) {
        loupeShown.value = false;
        hovered.value = null;
        cursor.value = "default";
        break;
      }
      if (windowMode.value) {
        loupeShown.value = false;
        // The chip is measured only when the window under the pointer changes.
        const w = windowAt(p);
        if (w !== hovered.value) {
          hovered.value = w;
          void placeChip();
        }
      } else {
        hovered.value = null;
        updateLoupe(p);
      }
      cursor.value = "crosshair";
      break;
    case "drag":
      if (start) sel.value = fromCorners(start, p);
      updateLoupe(p);
      break;
    case "move":
      if (grab && sel.value) {
        const r = sel.value;
        sel.value = { ...r, x: clamp(p.x - grab.offset.x, 0, view.w - r.w), y: clamp(p.y - grab.offset.y, 0, view.h - r.h) };
      }
      break;
    case "resize":
      if (grab?.handle) {
        const o = grab.origin;
        const [fx, fy] = grab.handle;
        const a = { x: fx === 0 ? p.x : o.x, y: fy === 0 ? p.y : o.y };
        const b = { x: fx === 1 ? p.x : o.x + o.w, y: fy === 1 ? p.y : o.y + o.h };
        sel.value = fromCorners(a, b);
      }
      break;
    case "sel":
      if (annotations.busy()) {
        annotations.move(toFrame(p), { shift: e.shiftKey, alt: e.altKey });
        break;
      }
      aim(p, e.metaKey);
      break;
  }
}

/** The cursor and the hover frame for the pointer at `p` over the selection; `over` is ⌘ held,
 * when a press only draws. Also called when ⌘ goes down or up without the pointer moving. */
function aim(p: Point, over: boolean): void {
  if (!sel.value) return;
  const fp = toFrame(p);
  const within = inside(sel.value, p);
  if (over && engine.tool && within) {
    cursor.value = annotations.cursorAt(fp, reach.value, true) ?? "crosshair";
    annotations.hover(null, reach.value);
    return;
  }
  const objectHandle = !over && engine.handleAt(fp, reach.value);
  const handle = handleAt(sel.value, p);
  const drawing = within ? annotations.cursorAt(fp, reach.value, over) : null;
  cursor.value = objectHandle ? (drawing ?? "move") : handle ? RESIZE_CURSORS[handle.join(",")] : within ? (drawing ?? "move") : "crosshair";
  // Framed only where a press picks the object up: inside the selection, off its handles.
  annotations.hover(within && !handle && !over ? fp : null, reach.value);
}

function onDoubleClick(e: MouseEvent): void {
  flushMove();
  if (mode.value !== "sel" || !sel.value || !inside(sel.value, point(e))) return;
  annotations.doubleClick(toFrame(point(e)), reach.value);
}

// The crop tool and undo change the crop, and the selection follows it.
watch(
  () => engine.rev.value,
  () => {
    const crop = annotations.cropRect();
    const r = sel.value;
    if (!crop || !r || mode.value !== "sel") return;
    const now = frameRect(r);
    if (now && now.x === crop.x && now.y === crop.y && now.width === crop.width && now.height === crop.height) return;
    sel.value = { x: crop.x / ratio.value.x, y: crop.y / ratio.value.y, w: crop.width / ratio.value.x, h: crop.height / ratio.value.y };
    if (!annotations.busy()) void layoutBars();
  },
);
watch(stripTool, () => void layoutBars());

function onMouseUp(e: MouseEvent): void {
  flushMove();
  if (e.button !== 0) return;
  const p = point(e);
  if (annotations.busy()) {
    annotations.end();
    void layoutBars();
    return;
  }
  if (mode.value === "drag") {
    const r = sel.value;
    if (r && r.w >= MIN_SELECTION && r.h >= MIN_SELECTION) return finishSelection(r);
    // A click without a drag takes the window under the pointer.
    const w = start && Math.hypot(p.x - start.x, p.y - start.y) < 3 ? windowAt(p) : null;
    if (w) return finishSelection(fromCorners({ x: w.rect.x, y: w.rect.y }, { x: w.rect.x + w.rect.width, y: w.rect.y + w.rect.height }), w.id);
    resetSelection();
    updateLoupe(p);
    return;
  }
  if (mode.value === "move" || mode.value === "resize") {
    grab = null;
    mode.value = "sel";
    if (sel.value && (sel.value.w < 1 || sel.value.h < 1)) return resetSelection();
    if (sel.value) annotations.setSelection(frameRect(sel.value));
    void showBars();
  }
}

function onMouseLeave(): void {
  flushMove();
  annotations.hover(null, reach.value);
  if (mode.value === "idle") {
    loupeShown.value = false;
    pointer.value = null;
    hovered.value = null;
  }
}

async function placeChip(): Promise<void> {
  await nextTick();
  const c = chip.value;
  const el = chipEl.value;
  if (!c || !el) return;
  const x = Math.round(clamp(c.cx - el.offsetWidth / 2, 8, view.w - 8 - el.offsetWidth));
  const y = Math.round(clamp(c.cy - el.offsetHeight / 2, 8, view.h - 8 - el.offsetHeight));
  if (x !== chipAt.value.x || y !== chipAt.value.y) chipAt.value = { x, y };
}

// ---------------------------------------------------------------------------- the keyboard

function onKeyDown(e: KeyboardEvent): void {
  flushMove();
  // A dialog that took the key (the first-upload question's Esc) has prevented its default.
  if (!frame.value || uploadAsk.value || e.defaultPrevented) return;
  // A text being typed takes its own keys.
  if (e.target instanceof HTMLElement && e.target.isContentEditable) return;
  if (e.key === "Escape") {
    e.preventDefault();
    // Esc first ends a drawing gesture or drops the selected object, then hides the text layer,
    // and only then closes the capture.
    if (annotations.key(e)) return;
    if (textLayerVisible.value) {
      textLayerVisible.value = false;
      return;
    }
    return close();
  }
  if (e.key === " " && mode.value === "idle") {
    e.preventDefault();
    windowMode.value = !windowMode.value;
    hintFaded.value = true;
    hovered.value = windowMode.value && pointer.value ? windowAt(pointer.value) : null;
    if (windowMode.value) loupeShown.value = false;
    else if (pointer.value) updateLoupe(pointer.value);
    void placeChip();
    return;
  }
  if (e.key === "Alt" && mode.value === "sel") {
    allTips.value = true;
    return;
  }
  if (e.key === "Meta" && mode.value === "sel" && pointer.value && !annotations.busy()) {
    aim(pointer.value, true);
    return;
  }
  if (mode.value !== "sel" || !sel.value) return;

  // The selected object's keys: delete, duplicate, order, lock, hide, nudge and ⌥C.
  if (annotations.key(e)) {
    e.preventDefault();
    return;
  }
  if ((e.metaKey || e.ctrlKey) && !e.shiftKey && e.key.toLowerCase() === "o") {
    e.preventDefault();
    void handOff();
    return;
  }
  if (e.key === "Enter") {
    e.preventDefault();
    void act("copy");
    return;
  }
  const arrows: Record<string, Point> = { ArrowLeft: { x: -1, y: 0 }, ArrowRight: { x: 1, y: 0 }, ArrowUp: { x: 0, y: -1 }, ArrowDown: { x: 0, y: 1 } };
  const step = arrows[e.key];
  if (step) {
    e.preventDefault();
    const by = e.shiftKey ? 10 : 1;
    const r = sel.value;
    sel.value = { ...r, x: clamp(r.x + step.x * by, 0, view.w - r.w), y: clamp(r.y + step.y * by, 0, view.h - r.h) };
    annotations.setSelection(frameRect(sel.value));
    void layoutBars();
    return;
  }
  if (e.metaKey || e.ctrlKey) {
    const k = e.key.toLowerCase();
    if (k === "z") {
      e.preventDefault();
      if (e.shiftKey) annotations.redo();
      else annotations.undo();
      return;
    }
    if (k === "s" && e.shiftKey) {
      e.preventDefault();
      void saveAs();
      return;
    }
    const action = actionForChord(`${e.shiftKey ? "⇧" : ""}⌘${k.toUpperCase()}`);
    if (action) {
      e.preventDefault();
      void act(action.id);
    }
    return;
  }
  if (e.altKey) return;
  const match = toolForKey(e.key);
  if (match) setTool(match.id);
}

function onKeyUp(e: KeyboardEvent): void {
  flushMove();
  if (e.key === "Alt") allTips.value = false;
  if (e.key === "Meta" && mode.value === "sel" && pointer.value && !annotations.busy()) aim(pointer.value, false);
}

function onResize(): void {
  view.w = window.innerWidth;
  view.h = window.innerHeight;
}

// ------------------------------------------------------------------------------- wiring

// The dev driver (`src-tauri/src/driver.rs`) reads the page's state through this; release
// builds leave it out.
if (import.meta.env.DEV) Object.assign(window, { __overlay: { annotations, engine, sel, mode, frame, tool } });

const stops: (() => void)[] = [];
onMounted(async () => {
  window.addEventListener("mousemove", onMouseMove);
  window.addEventListener("mouseup", onMouseUp);
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  window.addEventListener("resize", onResize);
  void annotations.init();
  stops.push(await onWindow<OverlayFrame>(CAPTURE_STARTED, (next) => void begin(next)));
  stops.push(await on<CaptureClosed>(CAPTURE_CLOSED, end));
  stops.push(
    await on<{ captureId: number; displayId: number }>(OVERLAY_SELECTION_STARTED, (other) => {
      if (other.displayId === displayId || other.captureId !== frame.value?.captureId) return;
      // A move that came before the news is handled before it, as it was sent.
      flushMove();
      resetSelection();
      heldElsewhere.value = other.displayId;
      loupeShown.value = false;
      pointer.value = null;
      cursor.value = "default";
    }),
  );
  stops.push(
    await on<{ captureId: number; displayId: number }>(OVERLAY_SELECTION_ENDED, (other) => {
      // Only the holder's own end frees this display; an end from a display that just lost the
      // selection to a third one changes nothing.
      if (other.captureId !== frame.value?.captureId || other.displayId !== heldElsewhere.value) return;
      flushMove();
      heldElsewhere.value = null;
      cursor.value = "crosshair";
    }),
  );
  // A page that loaded after `capture://started` was sent (a reload) picks the capture up.
  const current = await capture_current();
  if (current) void begin(current);
});
onUnmounted(() => {
  window.removeEventListener("mousemove", onMouseMove);
  window.removeEventListener("mouseup", onMouseUp);
  window.removeEventListener("keydown", onKeyDown);
  window.removeEventListener("keyup", onKeyUp);
  window.removeEventListener("resize", onResize);
  cancelAnimationFrame(moveFrame);
  stops.forEach((stop) => stop());
});
</script>

<template>
  <div class="overlay" :style="{ cursor }" @mousedown="onMouseDown" @mouseleave="onMouseLeave" @dblclick="onDoubleClick">
    <div ref="frameHost" class="frame-host" />

    <template v-if="frame">
      <!-- The dim: bands top, bottom, left and right of the hole, then the corners of a rounded
           hole (`paintDim`). -->
      <div ref="dimEl" class="o-dim" aria-hidden="true">
        <i data-band /><i data-band /><i data-band /><i data-band />
        <i data-corner="tl" /><i data-corner="tr" /><i data-corner="bl" /><i data-corner="br" />
      </div>
      <!-- The drawings over the selection, in the document order of the dim and the edge. Without
           drawings it needs no area, so a drag does not render it again. -->
      <Surface
        v-if="hasSel && annotations.source.value"
        :engine="engine"
        :source="annotations.source.value"
        :view="annotationView"
        :area="drawn ? liveArea : null"
        :composed="drawn"
        :chrome="mode === 'sel'"
      />

      <div v-if="hovered && !hasSel" class="o-win" :style="{ ...box({ x: hovered.rect.x, y: hovered.rect.y, w: hovered.rect.width, h: hovered.rect.height }), '--o-radius': `${hovered.cornerRadius ?? 10}px` }" />
      <div v-if="chip" ref="chipEl" class="g-glass o-winchip o-float" :style="at(chipAt)">
        <span class="o-app" aria-hidden="true"><GIcon name="window" :size="24" /></span>
        <div>
          <div class="o-winchip-name">{{ chip.name }}</div>
          <div class="o-winchip-meta">
            <span class="o-winchip-size">{{ chip.size }}<span class="g-size-x">×</span>{{ chip.sizeY }}</span>
            <span>·</span>
            <span>{{ t("overlay.clickToCapture") }}</span>
          </div>
        </div>
      </div>

      <div ref="lineH" v-show="showLines" class="o-xh o-moving" data-axis="h" />
      <div ref="lineV" v-show="showLines" class="o-xh o-moving" data-axis="v" />

      <!-- The selection's edge: the iris line outside, then the light hairline inside, each as
           four strips (`paintEdge`). -->
      <div v-if="hasSel" ref="edgeEl" class="o-edge" aria-hidden="true">
        <i /><i /><i /><i />
        <i data-ring="inner" /><i data-ring="inner" /><i data-ring="inner" /><i data-ring="inner" />
      </div>
      <span
        v-for="h in handles"
        :key="h.key"
        class="g-handle"
        :data-handle="h.kind ?? undefined"
        :style="at(h)"
      />
      <div v-if="sizeShown" ref="sizeEl" class="g-size o-float o-moving"><span>0</span><span class="g-size-x">×</span><span>0</span></div>
      <!-- Mount point of the text recognition layer (src/ocr/TextLayer.vue); its context comes
           from `provide(TEXT_LAYER)` above. -->
      <div v-if="textLayerComponent && hasSel && mode === 'sel'" class="o-textlayer">
        <component :is="textLayerComponent" />
      </div>
    </template>

    <div ref="loupeEl" v-show="frame && loupeShown" class="g-glass o-loupe o-moving" aria-hidden="true">
      <div class="o-lens">
        <div ref="lensHost" class="lens-host" />
        <span class="o-lens-mid" />
      </div>
      <!-- `updateLoupe` writes the colour and the position. -->
      <div class="o-loupe-meta">
        <span ref="swatchEl" class="o-loupe-swatch" style="background: #000" />
        <span ref="hexEl">#000000</span>
        <span ref="xyEl" class="o-loupe-xy">0, 0</span>
      </div>
    </div>

    <div v-if="frame" class="g-hint o-float hint" :class="{ 'o-hint-fade': hintFaded }">
      <span>{{ t("overlay.hintDrag") }}</span>
      <span class="g-divider" role="separator" />
      <kbd class="g-kbd">Space</kbd><span class="g-muted">{{ t("overlay.hintWindow") }}</span>
      <kbd class="g-kbd">Esc</kbd><span class="g-muted">{{ t("overlay.hintCancel") }}</span>
    </div>
    <div v-if="notice" class="g-hint o-float hint notice" role="status">{{ notice }}</div>

    <!-- The bars stay in the page while hidden, so they can be measured before they show. -->
    <div
      ref="toolsEl"
      class="g-bar o-bar o-tools o-live-bar"
      role="toolbar"
      :aria-label="t('overlay.tools')"
      :data-shown="barsShown && mode === 'sel' ? 'true' : 'false'"
      :data-tips="placement?.toolTips ?? 'above'"
      :style="at(placement?.tools ?? { x: -9999, y: -9999 })"
      @mousedown.stop
    >
      <template v-for="(item, i) in TOOLS" :key="item ? item.id : `d${i}`">
        <span v-if="!item" class="g-divider" role="separator" />
        <span v-else class="g-tipwrap">
          <button
            type="button"
            class="g-btn"
            :data-id="item.id"
            :aria-label="t(item.label)"
            :aria-pressed="item.momentary ? undefined : tool === item.id"
            :disabled="(item.id === 'undo' && !undoable) || (item.id === 'redo' && !redoable)"
            @click="setTool(item.id)"
          >
            <GIcon :name="item.id" />
          </button>
          <span class="g-tip" role="tooltip">{{ t(item.label) }}<kbd class="g-kbd">{{ item.key }}</kbd></span>
        </span>
      </template>
    </div>

    <div
      v-if="stripTool"
      ref="stripEl"
      class="g-strip o-bar o-strip o-live-bar"
      role="toolbar"
      :aria-label="t('overlay.options')"
      :data-shown="barsShown && mode === 'sel' ? 'true' : 'false'"
      :style="at(placement?.strip ?? { x: -9999, y: -9999 })"
      @mousedown.stop
    >
      <OptionsStrip :engine="engine" :tool="stripTool" segmented />
    </div>
    <RedactionNotice
      v-if="annotations.noticeShown.value && !engine.prefs.redactionNoticeSeen && noticeAt && barsShown && mode === 'sel'"
      class="o-bar"
      :style="at(noticeAt)"
      @dismiss="engine.dismissRedactionNotice(); annotations.noticeShown.value = false"
    />

    <div
      ref="actionsEl"
      class="g-bar o-bar o-actions o-live-bar"
      role="toolbar"
      aria-orientation="vertical"
      :aria-label="t('overlay.actions')"
      :data-shown="barsShown && mode === 'sel' ? 'true' : 'false'"
      :data-tips="placement?.actionTips ?? 'right'"
      :style="at(placement?.actions ?? { x: -9999, y: -9999 })"
      @mousedown.stop
    >
      <template v-for="(item, i) in ACTIONS" :key="item ? item.id : `d${i}`">
        <span v-if="!item" class="g-divider" role="separator" />
        <span v-else class="g-tipwrap">
          <button type="button" class="g-btn" :data-id="item.id" :aria-label="t(item.label)" @click="act(item.id)">
            <GIcon :name="item.icon" />
          </button>
          <span class="g-tip" role="tooltip" :data-state="allTips ? 'show' : undefined">{{ t(item.label) }}<kbd class="g-kbd">{{ item.key }}</kbd></span>
        </span>
      </template>
    </div>

    <!-- The first-upload question; the overlay's own pointer and keys wait while it asks. -->
    <div v-if="uploadAsk" @mousedown.stop @dblclick.stop>
      <FirstUploadDialog
        :consent="uploadAsk.consent"
        :file="uploadAsk.file"
        @confirmed="uploads.confirmed"
        @cancel="uploads.cancel"
        @stale="uploads.stale"
        @another-host="uploads.anotherHost"
      />
    </div>
  </div>
</template>

<style scoped>
.overlay {
  position: fixed;
  inset: 0;
  overflow: hidden;
}
.frame-host,
.frame-host :deep(canvas) {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  display: block;
}
.lens-host :deep(canvas) {
  display: block;
  width: 136px;
  height: 136px;
  image-rendering: pixelated;
}
.lens-host {
  grid-column: 1 / -1;
  grid-row: 1 / -1;
}
.o-bar {
  cursor: default;
}
.hint {
  left: 50%;
  bottom: 120px;
  transform: translateX(-50%);
}
.notice {
  bottom: 168px;
}
.o-textlayer {
  position: absolute;
  inset: 0;
  z-index: 8;
  pointer-events: none;
}
</style>
