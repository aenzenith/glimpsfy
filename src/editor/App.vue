<script setup lang="ts">
// The editor window (DESIGN.md "Editor window", windows ab7–ab11): one image, a capture the
// overlay handed over with its drawings, a history entry or a file dropped on the window, with
// the overlay's tools in a vertical rail on the left and the active tool's or the selected
// object's options in a glass strip that floats at the bottom of the canvas. The
// window toolbar holds the outputs, which send the rendered image to the same commands the overlay
// uses; the status bar holds the facts and the zoom. Closing with unsaved changes asks once.
//
// The core keeps the image and hands its bytes over as a raw binary response (`editor_image`),
// never as base64. The page decodes them into a canvas in the image's colour space, and the
// annotation surface composes over that.
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { computed, nextTick, onMounted, onUnmounted, provide, reactive, ref, shallowRef, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import TipWrap from "../components/TipWrap.vue";
import OptionsStrip from "../annotate/OptionsStrip.vue";
import RedactionNotice from "../annotate/RedactionNotice.vue";
import Surface from "../annotate/Surface.vue";
import { context2d, exportArea, redactedPixels, renderExport, scratchCanvas, type ImageSource } from "../annotate/compose";
import { AnnotationEngine, TOOL_OF } from "../annotate/engine";
import { defaultPrefs, flushPrefs, loadPrefs, savePrefs, type ToolId as AnnotateTool } from "../annotate/prefs";
import type { Bytes, PixelRect } from "../annotate/redact";
import { TEXT_LAYER, textLayerComponent, type View } from "../annotate/textLayer";
import { thumbnailOfPixels, uploadFlow } from "../annotate/upload";
import { CURRENT_VERSION, parseDocument, serializeDocument, type AnnotationDocument } from "../lib/annotate";
import { ApiError, onWindow } from "../lib/api/core";
import {
  EDITOR_OPENED,
  editor_close,
  editor_current,
  editor_image,
  editor_open,
  editor_set_document,
  output_copy_rendered,
  output_save_as_rendered,
  output_save_rendered,
  output_stage_rendered,
  upload_capture_rendered,
  type EditorImage,
  type RenderedImage,
} from "../lib/api/render";
import { pin_open } from "../lib/api/windows";
import { language, t, type Key } from "../lib/i18n";
import { TOOLS, toolForKey, type ToolId } from "../overlay/tools";
import FirstUploadDialog from "../upload/FirstUploadDialog.vue";
import ObjectsPanel from "./ObjectsPanel.vue";
import UnsavedSheet from "./UnsavedSheet.vue";
import { GRID_ZOOM, MAX_ZOOM, MIN_ZOOM, ZOOM_MENU, clampPan, fit, fitWidth, kOf, opening, percentOf, step, zoomAt, type Room, type ZoomMode } from "./view";

/** A handle's hit radius in screen points, as in the overlay. */
const HANDLE_HIT = 14;
/** Fit leaves this much under the image, shown or not, so the options strip never moves or
 * covers it: the strip, 12 px under it and 12 px over it. */
const OPTIONS_ROOM = 60;

function emptyDocument(width: number, height: number, scale: number): AnnotationDocument {
  return { version: CURRENT_VERSION, image: { width, height }, scale, objects: [] };
}

// ------------------------------------------------------------------------------- the state

const engine = new AnnotationEngine(emptyDocument(1, 1, 1), defaultPrefs());
engine.onPrefs = savePrefs;
const noticeShown = ref(false);
engine.onRedaction = () => (noticeShown.value = true);
const rev = computed(() => engine.rev.value);

const image = shallowRef<EditorImage | null>(null);
const source = shallowRef<ImageSource | null>(null);
const fileBytes = ref<number | null>(null);
const fromHistory = ref(false);
const loading = ref(false);
const busy = ref(false);

const canvasEl = ref<HTMLElement | null>(null);
const railEl = ref<HTMLElement | null>(null);
const optionsEl = ref<HTMLElement | null>(null);
/** A window too short for the rail, or a canvas too narrow for the options, scrolls it; otherwise
 * their tooltips may reach past their edge. */
const railScrolls = ref(false);
const optionsScroll = ref(false);
const room = reactive<Room>({ width: 0, height: 0, top: 24, bottom: OPTIONS_ROOM });
const view = ref<View>({ k: 1, ox: 0, oy: 0 });
const zoomMode = ref<ZoomMode>("fit");
const objectsOpen = ref(false);
const textLayerVisible = ref(false);
const message = ref<string | null>(null);
const dropName = ref<string | null>(null);
const cursor = ref("default");
const spaceHeld = ref(false);
const readout = ref<{ x: number; y: number; hex: string; css: string } | null>(null);
type Sheet = { title: string; text: string; cancellable: boolean; resolve: (go: boolean) => void };
const sheet = shallowRef<Sheet | null>(null);

let pan: { x: number; y: number; view: View } | null = null;
let messageTimer = 0;
let loadingToken: string | null = null;
/** An image the page asked for itself, so its `editor://opened` needs no second question. */
let expected = false;
let closing = false;

// ------------------------------------------------------------------------------ derived

const tool = computed(() => (rev.value, engine.tool));
const cropMode = computed(() => tool.value === "crop");
const scale = computed(() => (rev.value, engine.doc.scale));
/** The part of the image on the canvas: all of it while cropping, else the crop. */
const area = computed<PixelRect | null>(() => {
  void rev.value;
  if (!source.value) return null;
  const { width, height } = engine.doc.image;
  return cropMode.value ? { x: 0, y: 0, width, height } : exportArea(engine.doc);
});
/** What the options strip shows: the selected object's tool, whose choices then restyle that
 * object, else the chosen tool. Crop mode keeps the crop's ratios. */
const pick = computed<{ tool: AnnotateTool; object: boolean } | null>(() => {
  void rev.value;
  if (!source.value) return null;
  const object = cropMode.value ? null : engine.selected();
  if (object) return { tool: TOOL_OF[object.kind], object: true };
  return engine.tool ? { tool: engine.tool, object: false } : null;
});
const percent = computed(() => percentOf(view.value.k, scale.value));
const pct = (n: number) => t("editor.percent", { n: Math.round(n) });
const zoomLabel = computed(() => {
  if (zoomMode.value === "fit") return `${t("editor.fit")} · ${pct(percent.value)}`;
  if (zoomMode.value === "width") return `${t("editor.fitWidth")} · ${pct(percent.value)}`;
  return pct(percent.value);
});
const zoomValue = computed(() => {
  if (zoomMode.value !== "custom") return zoomMode.value;
  const near = ZOOM_MENU.find((z) => Math.abs(z - percent.value) < 0.5);
  return near ? String(near) : "current";
});
const count = computed(() => (rev.value, engine.drawings().length));
const countText = computed(() => (count.value === 1 ? t("editor.count.one") : t("editor.count.other", { n: count.value })));
const dirty = computed(() => (rev.value, source.value !== null && engine.dirty()));
const subtitle = computed(() => {
  if (!image.value) return t("editor.noImage");
  return dirty.value ? `${t("editor.edited")} · ${countText.value}` : countText.value;
});

function size(bytes: number): string {
  const n = new Intl.NumberFormat(language.value, { maximumFractionDigits: 1 });
  if (bytes < 1024 * 1024) return `${n.format(Math.max(1, Math.round(bytes / 1024)))} KB`;
  return `${n.format(bytes / 1024 / 1024)} MB`;
}

const facts = computed(() => {
  const img = image.value;
  if (!img || !source.value) return t("editor.noImage");
  const { width, height } = source.value.pixels;
  const parts = [`${width} × ${height} px`];
  const extension = img.path?.split(".").pop()?.toUpperCase();
  if (extension && extension.length <= 5) parts.push(extension === "JPG" ? "JPEG" : extension);
  if (fileBytes.value !== null) parts.push(size(fileBytes.value));
  parts.push(countText.value);
  return parts.join(" · ");
});

const px = (v: number) => `${v}px`;
function screen(r: { x: number; y: number; width: number; height: number }) {
  const { k, ox, oy } = view.value;
  return { x: ox + r.x * k, y: oy + r.y * k, w: r.width * k, h: r.height * k };
}
const boxStyle = (r: { x: number; y: number; w: number; h: number }) => ({ left: px(r.x), top: px(r.y), width: px(r.w), height: px(r.h) });

const imageBox = computed(() => (area.value ? boxStyle(screen(area.value)) : null));
const gridStyle = computed(() => {
  if (!area.value || percent.value < GRID_ZOOM - 0.5) return null;
  const k = view.value.k;
  return { ...boxStyle(screen(area.value)), backgroundSize: `${k}px ${k}px` };
});
const cropBox = computed(() => {
  void rev.value;
  const c = engine.crop();
  return cropMode.value && c ? { screen: screen(c), width: Math.round(c.width), height: Math.round(c.height) } : null;
});
// The text recognition layer reads the image through this (src/annotate/textLayer.ts).
provide(TEXT_LAYER, {
  area,
  view,
  pixels: () => (source.value && area.value ? redactedPixels(source.value, engine.doc, area.value) : null),
  colorSpace: computed(() => source.value?.colorSpace ?? "srgb"),
  scale,
  visible: textLayerVisible,
});

// ------------------------------------------------------------------------------- messages

function say(text: string): void {
  message.value = text;
  clearTimeout(messageTimer);
  messageTimer = window.setTimeout(() => (message.value = null), 3200);
}

function errorText(error: unknown): string {
  const code = error instanceof ApiError ? error.code : "internal";
  if (code === "not_found") return t("editor.gone");
  const known: Key[] = ["error.permission_denied", "error.not_implemented", "error.io"];
  const key = `error.${code}` as Key;
  return t(known.includes(key) ? key : "error.other");
}

// ---------------------------------------------------------------------------- the image

/** Decodes a file's bytes into the image's colour space. */
async function decode(buffer: ArrayBuffer, colorSpace: PredefinedColorSpace): Promise<ImageSource> {
  const blob = new Blob([buffer]);
  let drawable: ImageBitmap | HTMLImageElement;
  try {
    drawable = await createImageBitmap(blob);
  } catch {
    // WebKit decodes some formats (TIFF) only through an image element.
    const url = URL.createObjectURL(blob);
    try {
      const img = new Image();
      img.src = url;
      await img.decode();
      drawable = img;
    } finally {
      URL.revokeObjectURL(url);
    }
  }
  const width = drawable instanceof HTMLImageElement ? drawable.naturalWidth : drawable.width;
  const height = drawable instanceof HTMLImageElement ? drawable.naturalHeight : drawable.height;
  if (!width || !height) throw new Error("the image has no pixels");
  const canvas = scratchCanvas(width, height);
  const ctx = context2d(canvas, colorSpace, true);
  ctx.drawImage(drawable, 0, 0);
  if (drawable instanceof ImageBitmap) drawable.close();
  const data = ctx.getImageData(0, 0, width, height, { colorSpace }).data as Bytes;
  return { pixels: { width, height, data }, colorSpace, canvas };
}

function raw(buffer: ArrayBuffer, img: EditorImage): ImageSource {
  const width = img.width ?? 0;
  const height = img.height ?? 0;
  const data = new Uint8ClampedArray(buffer) as Bytes;
  if (!width || !height || data.length !== width * height * 4) throw new Error("the pixels do not match the image's size");
  const canvas = scratchCanvas(width, height);
  context2d(canvas, img.colorSpace).putImageData(new ImageData(data, width, height, { colorSpace: img.colorSpace }), 0, 0);
  return { pixels: { width, height, data }, colorSpace: img.colorSpace, canvas };
}

/** The overlay's document when it fits the image, else an empty one. */
function documentFor(img: EditorImage, width: number, height: number): AnnotationDocument {
  if (img.document) {
    try {
      const doc = parseDocument(JSON.parse(img.document));
      if (doc.image.width === width && doc.image.height === height) return doc;
      console.warn("[glimpsfy] the handed-over drawings do not fit the image and were left out");
    } catch (error) {
      console.warn("[glimpsfy] the handed-over drawings could not be read", error);
    }
  }
  return emptyDocument(width, height, img.scale);
}

async function show(img: EditorImage): Promise<void> {
  if (image.value?.token === img.token || loadingToken === img.token) return;
  loadingToken = img.token;
  loading.value = true;
  try {
    const buffer = await editor_image(img.token);
    let next: ImageSource;
    try {
      next = img.encoding === "rgba" ? raw(buffer, img) : await decode(buffer, img.colorSpace);
    } catch (error) {
      console.warn("[glimpsfy] the editor could not decode the image", error);
      say(t("editor.notImage", { name: img.name }));
      void editor_close(img.token).catch(() => {});
      return;
    }
    if (loadingToken !== img.token) return;
    const previous = image.value;
    const { width, height } = next.pixels;
    image.value = img;
    source.value = next;
    fileBytes.value = img.encoding === "file" ? buffer.byteLength : null;
    fromHistory.value = img.entryId !== null;
    engine.setTool(null);
    // Drawings handed over by the overlay are not kept anywhere yet; an edit reopened from the
    // history is already what its entry holds.
    engine.load(documentFor(img, width, height), img.document !== null && img.entryId === null);
    textLayerVisible.value = false;
    noticeShown.value = false;
    if (previous) void editor_close(previous.token).catch(() => {});
    await nextTick();
    openingView();
  } catch (error) {
    say(errorText(error));
  } finally {
    if (loadingToken === img.token) {
      loadingToken = null;
      loading.value = false;
    }
    expected = false;
  }
}

/** An image arrived from the core (`editor://opened`): a capture the overlay handed over, or a
 * history entry (`history_open_in_editor` calls `editor_open`). */
async function incoming(img: EditorImage): Promise<void> {
  if (image.value?.token === img.token || loadingToken === img.token) return;
  // A file can be opened again, so its question offers Cancel. A capture is already closed and
  // cannot be refused, so only Save and Don't save remain.
  if (!expected && !(await settle(t("editor.unsaved.titleNext"), img.path !== null))) {
    void editor_close(img.token).catch(() => {});
    return;
  }
  await show(img);
}

/** Opens a file dropped on the window, after the unsaved question. */
async function openPath(path: string): Promise<void> {
  if (!(await settle(t("editor.unsaved.titleNext"), true))) return;
  expected = true;
  try {
    await show(await editor_open(path));
  } catch (error) {
    expected = false;
    say(errorText(error));
  }
}

// ------------------------------------------------------------------------- the question

function unsavedText(): string {
  const n = count.value;
  const lost = n === 0 ? t("editor.unsaved.lostCrop") : n === 1 ? t("editor.unsaved.lostOne") : t("editor.unsaved.lostOther", { n });
  const keep = !image.value?.path ? t("editor.unsaved.keepNothing") : fromHistory.value ? t("editor.unsaved.keepHistory") : t("editor.unsaved.keepFile");
  return `${lost} ${keep}`;
}

/** Asks about unsaved changes; resolves whether the page may go on. */
function settle(title: string, cancellable: boolean): Promise<boolean> {
  engine.finishText();
  if (!dirty.value) return Promise.resolve(true);
  return new Promise((resolve) => {
    sheet.value = { title, text: unsavedText(), cancellable, resolve };
  });
}

function answer(go: boolean): void {
  const s = sheet.value;
  sheet.value = null;
  s?.resolve(go);
}

async function saveAndGo(): Promise<void> {
  if (await output("save")) answer(true);
}

// ------------------------------------------------------------------------------- outputs

function rendered(): RenderedImage | null {
  const s = source.value;
  const img = image.value;
  if (!s || !img) return null;
  engine.finishText();
  const r = renderExport(s, engine.doc);
  return r ? { source: { kind: "editor", token: img.token }, width: r.width, height: r.height, colorSpace: r.colorSpace, data: r.data } : null;
}

/** Copy, Save and Save as; resolves whether the image left. */
async function output(kind: "copy" | "save" | "saveAs"): Promise<boolean> {
  if (busy.value) return false;
  const r = rendered();
  if (!r) return false;
  busy.value = true;
  try {
    // The history keeps the document with the entry this output makes or updates.
    if (r.source.kind === "editor") await editor_set_document(r.source.token, JSON.stringify(serializeDocument(engine.doc)));
    if (kind === "copy") await output_copy_rendered(r);
    else {
      await (kind === "save" ? output_save_rendered(r) : output_save_as_rendered(r));
      engine.markSaved();
    }
    return true;
  } catch (error) {
    if (!(error instanceof ApiError && error.code === "cancelled")) say(errorText(error));
    return false;
  } finally {
    busy.value = false;
  }
}

/** Upload and Pin take a file: the rendered image is staged first. */
async function staged(then: (path: string) => Promise<unknown>): Promise<void> {
  if (busy.value) return;
  const r = rendered();
  if (!r) return;
  busy.value = true;
  try {
    await then(await output_stage_rendered(r));
  } catch (error) {
    say(errorText(error));
  } finally {
    busy.value = false;
  }
}

/** Upload (⌘U): the rendered image goes once the provider is checked and, the first time,
 * confirmed in the dialog. */
const uploads = uploadFlow({
  async send(provider) {
    if (busy.value) return;
    const r = rendered();
    if (!r) return;
    busy.value = true;
    try {
      await upload_capture_rendered(r, provider);
    } finally {
      busy.value = false;
    }
  },
  file() {
    const r = rendered();
    if (!r || !image.value) return null;
    return { name: image.value.name, thumbnail: thumbnailOfPixels(r), width: r.width, height: r.height, format: "PNG", bytes: null };
  },
  say,
});
const uploadAsk = uploads.asking;
const upload = () => void uploads.start();

const pin = () => staged((path) => pin_open({ kind: "file", path }));

function copyText(): void {
  if (!textLayerComponent) return say(t("overlay.notReady"));
  textLayerVisible.value = !textLayerVisible.value;
}

// ----------------------------------------------------------------------------- the view

function measure(): void {
  const el = canvasEl.value;
  if (!el) return;
  room.width = el.clientWidth;
  room.height = el.clientHeight;
  const rail = railEl.value;
  railScrolls.value = rail !== null && rail.scrollHeight > rail.clientHeight + 1;
}

function measureOptions(): void {
  const strip = optionsEl.value;
  optionsScroll.value = strip !== null && strip.scrollWidth > strip.clientWidth + 1;
}

// The strip comes and goes with the choice; it is measured when it changes size and when its
// tool changes, which can grow its content past a width it already holds at its limit.
const optionsResize = new ResizeObserver(measureOptions);
watch(
  optionsEl,
  (now, before) => {
    if (before) optionsResize.unobserve(before);
    if (now) optionsResize.observe(now);
  },
  { flush: "post" },
);
watch(pick, measureOptions, { flush: "post" });

function openingView(): void {
  if (!area.value) return;
  const chosen = opening(area.value, room, scale.value);
  view.value = chosen.view;
  zoomMode.value = chosen.mode;
}

function applyFit(mode: "fit" | "width"): void {
  if (!area.value) return;
  view.value = mode === "fit" ? fit(area.value, room, scale.value) : fitWidth(area.value, room, scale.value);
  zoomMode.value = mode;
}

function zoomTo(percentValue: number, at?: { x: number; y: number }): void {
  if (!area.value) return;
  const p = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, percentValue));
  const centre = at ?? { x: room.width / 2, y: (room.top + room.height) / 2 };
  view.value = clampPan(zoomAt(view.value, kOf(p, scale.value), centre.x, centre.y), area.value, room);
  zoomMode.value = "custom";
}

function onZoomMenu(e: Event): void {
  const value = (e.target as HTMLSelectElement).value;
  if (value === "fit" || value === "width") applyFit(value);
  else if (value !== "current") zoomTo(Number(value));
}

// A new crop, a cancelled crop and a resized window each refit a fitted view.
watch(
  () => (area.value ? `${area.value.x},${area.value.y},${area.value.width},${area.value.height}` : ""),
  (now, before) => {
    if (now && before && now !== before) applyFit(zoomMode.value === "width" ? "width" : "fit");
  },
);

// ---------------------------------------------------------------------------- the pointer

function local(e: MouseEvent): { x: number; y: number } {
  const box = canvasEl.value?.getBoundingClientRect();
  return { x: e.clientX - (box?.left ?? 0), y: e.clientY - (box?.top ?? 0) };
}
function toImage(p: { x: number; y: number }): { x: number; y: number } {
  return { x: (p.x - view.value.ox) / view.value.k, y: (p.y - view.value.oy) / view.value.k };
}
const reach = () => HANDLE_HIT / view.value.k;

function onPointerDown(e: PointerEvent): void {
  if (!source.value || sheet.value) return;
  const target = e.currentTarget as HTMLElement;
  if (e.button === 1 || (e.button === 0 && spaceHeld.value)) {
    pan = { x: e.clientX, y: e.clientY, view: view.value };
    target.setPointerCapture(e.pointerId);
    cursor.value = "grabbing";
    e.preventDefault();
    return;
  }
  if (e.button !== 0) return;
  const result = engine.pointerDown(toImage(local(e)), reach());
  if (result === "handle" || result === "object" || result === "draw") target.setPointerCapture(e.pointerId);
  // A press that starts a text leaves the focus to its field.
  if (result !== "text") e.preventDefault();
}

function onPointerMove(e: PointerEvent): void {
  if (pan && area.value) {
    view.value = clampPan({ k: pan.view.k, ox: pan.view.ox + e.clientX - pan.x, oy: pan.view.oy + e.clientY - pan.y }, area.value, room);
    zoomMode.value = "custom";
    return;
  }
  const p = toImage(local(e));
  if (engine.busy()) {
    engine.pointerMove(p, { shift: e.shiftKey, alt: e.altKey });
    return;
  }
  cursor.value = spaceHeld.value ? "grab" : (engine.cursorAt(p, reach()) ?? "default");
  updateReadout(p);
}

function onPointerUp(e: PointerEvent): void {
  const target = e.currentTarget as HTMLElement;
  if (target.hasPointerCapture(e.pointerId)) target.releasePointerCapture(e.pointerId);
  if (pan) {
    pan = null;
    cursor.value = spaceHeld.value ? "grab" : "default";
    return;
  }
  if (engine.busy()) engine.pointerUp();
}

function onDoubleClick(e: MouseEvent): void {
  if (!source.value) return;
  const object = engine.objectAt(toImage(local(e)), reach(), true);
  if (object?.kind === "text") engine.editText(object.id);
}

function onWheel(e: WheelEvent): void {
  if (!area.value) return;
  e.preventDefault();
  if (e.ctrlKey || e.metaKey) {
    // A pinch arrives as a wheel with Control held.
    zoomTo(percent.value * Math.exp(-e.deltaY * 0.01), local(e));
    return;
  }
  view.value = clampPan({ k: view.value.k, ox: view.value.ox - e.deltaX, oy: view.value.oy - e.deltaY }, area.value, room);
  zoomMode.value = "custom";
}

/** The status bar's position and colour of the pixel under the pointer, from 400 % on. */
function updateReadout(p: { x: number; y: number }): void {
  const s = source.value;
  if (!s || percent.value < GRID_ZOOM - 0.5) {
    readout.value = null;
    return;
  }
  const x = Math.floor(p.x);
  const y = Math.floor(p.y);
  const { width, height, data } = s.pixels;
  if (x < 0 || y < 0 || x >= width || y >= height) {
    readout.value = null;
    return;
  }
  const i = (y * width + x) * 4;
  const [r, g, b] = [data[i], data[i + 1], data[i + 2]];
  const hex = `#${[r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("")}`.toUpperCase();
  const css = s.colorSpace === "display-p3" ? `color(display-p3 ${r / 255} ${g / 255} ${b / 255})` : `rgb(${r} ${g} ${b})`;
  readout.value = { x, y, hex, css };
}

// ---------------------------------------------------------------------------- the tools

function chooseTool(id: ToolId): void {
  if (!source.value) return;
  if (id === "undo") return engine.undo();
  if (id === "redo") return engine.redo();
  const next = engine.tool === id ? null : (id as AnnotateTool);
  if (next === "crop" && !engine.crop()) {
    // Crop mode starts from the whole image, which is no change of its own.
    const { width, height } = engine.doc.image;
    engine.setCrop({ x: 0, y: 0, width, height }, false);
  }
  engine.setTool(next);
}

/** Bring forward and Send backward for the selected object, from the options strip. */
function arrange(by: 1 | -1): void {
  if (engine.selectedId) engine.shift(engine.selectedId, by);
}

const undoable = computed(() => (rev.value, engine.canUndo()));
const redoable = computed(() => (rev.value, engine.canRedo()));
const caps = (key: string) => [...key];

// ------------------------------------------------------------------------- the keyboard

function onKeyDown(e: KeyboardEvent): void {
  // A dialog that took the key (the first-upload question's Esc) has prevented its default.
  if (sheet.value || uploadAsk.value || e.defaultPrevented) return;
  const target = e.target as HTMLElement | null;
  if (target && (target.isContentEditable || ["INPUT", "SELECT", "TEXTAREA"].includes(target.tagName))) return;
  const command = e.metaKey || e.ctrlKey;
  if (command && e.altKey && e.code === "KeyO") {
    e.preventDefault();
    objectsOpen.value = !objectsOpen.value;
    return;
  }
  if (!source.value) return;
  if (e.key === " " && !command) {
    e.preventDefault();
    spaceHeld.value = true;
    cursor.value = "grab";
    return;
  }
  if (cropMode.value && (e.key === "Enter" || e.key === "Escape") && !engine.busy()) {
    e.preventDefault();
    if (e.key === "Enter") engine.applyCrop();
    else engine.cancelCrop();
    return;
  }
  if (engine.key(e)) {
    e.preventDefault();
    return;
  }
  if (e.key === "Escape") {
    e.preventDefault();
    if (textLayerVisible.value) textLayerVisible.value = false;
    else if (engine.tool) engine.setTool(null);
    return;
  }
  if (command) {
    const k = e.key.toLowerCase();
    const run: Record<string, () => unknown> = {
      z: () => (e.shiftKey ? engine.redo() : engine.undo()),
      c: () => (e.shiftKey ? copyText() : output("copy")),
      s: () => output(e.shiftKey ? "saveAs" : "save"),
      u: upload,
      p: pin,
      "0": () => applyFit("fit"),
      "1": () => zoomTo(100),
      "=": () => zoomTo(step(percent.value, 1)),
      "+": () => zoomTo(step(percent.value, 1)),
      "-": () => zoomTo(step(percent.value, -1)),
    };
    const action = run[k];
    if (action) {
      e.preventDefault();
      void action();
    }
    return;
  }
  if (e.altKey) return;
  const match = toolForKey(e.key);
  if (match) chooseTool(match.id);
}

function onKeyUp(e: KeyboardEvent): void {
  if (e.key === " ") {
    spaceHeld.value = false;
    if (!pan) cursor.value = "default";
  }
}

// ------------------------------------------------------------------------------- wiring

async function release(): Promise<void> {
  await flushPrefs();
  const img = image.value;
  if (img) await editor_close(img.token).catch(() => {});
}

const stops: (() => void)[] = [];
let resize: ResizeObserver | null = null;

onMounted(async () => {
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  if (canvasEl.value) {
    resize = new ResizeObserver(() => {
      measure();
      if (zoomMode.value !== "custom") applyFit(zoomMode.value);
    });
    resize.observe(canvasEl.value);
  }
  measure();
  void loadPrefs().then((prefs) => {
    engine.prefs = prefs;
    engine.rev.value += 1;
  });

  const win = getCurrentWebviewWindow();
  stops.push(await onWindow<EditorImage>(EDITOR_OPENED, (img) => void incoming(img)));
  stops.push(
    await getCurrentWebview().onDragDropEvent((event) => {
      const p = event.payload;
      if (p.type === "enter") dropName.value = p.paths[0]?.split(/[\\/]/).pop() ?? "";
      else if (p.type === "drop") {
        dropName.value = null;
        if (p.paths[0]) void openPath(p.paths[0]);
      } else if (p.type === "leave") dropName.value = null;
    }),
  );
  stops.push(
    await win.onCloseRequested(async (event) => {
      if (closing) return;
      if (dirty.value) {
        event.preventDefault();
        if (!(await settle(t("editor.unsaved.title"), true))) return;
        closing = true;
        await release();
        await win.destroy();
        return;
      }
      await release();
    }),
  );
  // A page that loads after `editor://opened` was sent picks the image up.
  const current = await editor_current().catch(() => null);
  if (current) await show(current);
});

onUnmounted(() => {
  window.removeEventListener("keydown", onKeyDown);
  window.removeEventListener("keyup", onKeyUp);
  resize?.disconnect();
  optionsResize.disconnect();
  stops.forEach((stop) => stop());
});
</script>

<template>
  <div class="e-window">
    <header class="g-toolbar">
      <div class="e-doc">
        <span class="e-doc-name">{{ image?.name ?? t("window.editor") }}</span>
        <span class="e-doc-sub">{{ subtitle }}</span>
      </div>
      <span class="g-spacer" />
      <TipWrap :tip="t('editor.objects')" :caps="['⌥', '⌘', 'O']" side="below">
        <button type="button" class="g-btn" :aria-label="t('editor.objects')" :aria-pressed="objectsOpen" @click="objectsOpen = !objectsOpen">
          <GIcon name="sidebar" />
        </button>
      </TipWrap>
      <span class="g-divider" role="separator" />
      <TipWrap :tip="t('action.copy')" :caps="['⌘', 'C']" side="below">
        <button type="button" class="g-btn" :aria-label="t('action.copy')" :disabled="!source || busy" @click="output('copy')"><GIcon name="copy" /></button>
      </TipWrap>
      <TipWrap :tip="t('action.save')" :caps="['⌘', 'S']" side="below">
        <button type="button" class="g-btn" :aria-label="t('action.save')" :disabled="!source || busy" @click="output('save')"><GIcon name="save" /></button>
      </TipWrap>
      <TipWrap :tip="t('editor.saveAs')" :caps="['⇧', '⌘', 'S']" side="below">
        <button type="button" class="g-btn" :aria-label="t('editor.saveAs')" :disabled="!source || busy" @click="output('saveAs')"><GIcon name="save-as" /></button>
      </TipWrap>
      <TipWrap :tip="t('action.upload')" :caps="['⌘', 'U']" side="below">
        <button type="button" class="g-btn" :aria-label="t('action.upload')" :disabled="!source || busy" @click="upload"><GIcon name="upload" /></button>
      </TipWrap>
      <TipWrap :tip="t('action.copyText')" :caps="['⇧', '⌘', 'C']" side="below">
        <button type="button" class="g-btn" :aria-label="t('action.copyText')" :aria-pressed="textLayerVisible" :disabled="!source" @click="copyText">
          <GIcon name="ocr" />
        </button>
      </TipWrap>
      <TipWrap :tip="t('action.pin')" :caps="['⌘', 'P']" side="below" align="end">
        <button type="button" class="g-btn" :aria-label="t('action.pin')" :disabled="!source || busy" @click="pin"><GIcon name="pin" /></button>
      </TipWrap>
    </header>

    <div class="e-main">
      <div
        ref="railEl"
        class="e-rail"
        role="toolbar"
        aria-orientation="vertical"
        :aria-label="t('overlay.tools')"
        :data-scrolls="railScrolls || undefined"
      >
        <template v-for="(item, i) in TOOLS" :key="item ? item.id : `d${i}`">
          <span v-if="!item" class="g-divider" role="separator" />
          <TipWrap v-else :tip="t(item.label)" :caps="caps(item.key)" side="right">
            <button
              type="button"
              class="g-btn"
              :aria-label="t(item.label)"
              :aria-pressed="item.momentary ? undefined : tool === item.id"
              :disabled="!source || (item.id === 'undo' && !undoable) || (item.id === 'redo' && !redoable)"
              @click="chooseTool(item.id)"
            >
              <GIcon :name="item.id" />
            </button>
          </TipWrap>
        </template>
      </div>
      <div
        ref="canvasEl"
        class="e-canvas"
        :aria-label="t('editor.canvas')"
        :style="{ cursor }"
        @pointerdown="onPointerDown"
        @pointermove="onPointerMove"
        @pointerup="onPointerUp"
        @pointercancel="onPointerUp"
        @dblclick="onDoubleClick"
        @wheel="onWheel"
      >
        <div v-if="imageBox" class="e-lift" :style="imageBox" aria-hidden="true" />
        <Surface v-if="source" :engine="engine" :source="source" :view="view" :area="area" />
        <div v-if="gridStyle" class="e-pixgrid" :style="gridStyle" aria-hidden="true" />

        <template v-if="cropBox">
          <div class="g-dim e-crop-dim" :style="boxStyle(cropBox.screen)" />
          <div class="g-selection e-crop-edge" :style="boxStyle(cropBox.screen)" />
          <div class="g-size e-crop-size" :style="{ left: px(cropBox.screen.x), top: px(Math.max(8, cropBox.screen.y - 30)) }">
            {{ cropBox.width }}<span class="g-size-x">×</span>{{ cropBox.height }}
          </div>
          <div class="g-hint e-crop-hint">
            <span>{{ t("editor.crop.title") }}</span>
            <span class="g-divider" role="separator" />
            <kbd class="g-kbd">↩</kbd><span class="g-muted">{{ t("editor.crop.apply") }}</span>
            <kbd class="g-kbd">Esc</kbd><span class="g-muted">{{ t("editor.crop.cancel") }}</span>
          </div>
        </template>

        <!-- Mount point of the text recognition layer (src/ocr/TextLayer.vue); its context comes
             from `provide(TEXT_LAYER)` above. -->
        <div v-if="textLayerComponent && source" class="e-textlayer">
          <component :is="textLayerComponent" />
        </div>

        <RedactionNotice
          v-if="source && noticeShown && !engine.prefs.redactionNoticeSeen"
          class="e-notice"
          @pointerdown.stop
          @dismiss="engine.dismissRedactionNotice(); noticeShown = false"
        />

        <!-- `mousedown.prevent` keeps the focus in a text that is being typed. -->
        <div
          v-if="pick"
          ref="optionsEl"
          class="e-options"
          role="toolbar"
          :aria-label="t('overlay.options')"
          :data-scrolls="optionsScroll || undefined"
          @pointerdown.stop
          @mousedown.prevent
        >
          <span class="e-options-tool">
            <GIcon :name="pick.tool" :size="16" />
            <span class="e-options-name">{{ t(`tool.${pick.tool}` as Key) }}</span>
          </span>
          <span class="g-divider" role="separator" />
          <OptionsStrip :engine="engine" :tool="pick.tool" />
          <template v-if="pick.object">
            <span class="g-divider" role="separator" />
            <TipWrap :tip="t('annotate.bringForward')" :caps="['⌘', ']']">
              <button type="button" class="g-btn" data-size="small" :aria-label="t('annotate.bringForward')" @click="arrange(1)"><GIcon name="bring-forward" :size="16" /></button>
            </TipWrap>
            <TipWrap :tip="t('annotate.sendBackward')" :caps="['⌘', '[']">
              <button type="button" class="g-btn" data-size="small" :aria-label="t('annotate.sendBackward')" @click="arrange(-1)"><GIcon name="send-backward" :size="16" /></button>
            </TipWrap>
            <TipWrap :tip="t('annotate.delete')" :caps="['⌫']">
              <button type="button" class="g-btn" data-size="small" :aria-label="t('annotate.delete')" @click="engine.deleteSelected()"><GIcon name="trash" :size="16" /></button>
            </TipWrap>
          </template>
        </div>

        <div v-if="!source && !loading && dropName === null" class="g-empty e-empty">
          <div class="g-empty-icon"><GIcon name="image" :size="32" /></div>
          <div class="g-empty-title">{{ t("editor.empty.title") }}</div>
          <p class="g-empty-text">{{ t("editor.empty.text") }}</p>
        </div>
        <div v-if="dropName !== null" class="e-drop">
          <div class="g-empty-icon"><GIcon name="image" :size="32" /></div>
          <div class="e-drop-title">{{ t("editor.drop.title", { name: dropName }) }}</div>
          <p class="e-drop-text">{{ t("editor.drop.text") }}</p>
        </div>
        <div v-if="message" class="g-hint g-glass e-message" role="status">{{ message }}</div>
      </div>
      <ObjectsPanel v-if="objectsOpen" :engine="engine" @close="objectsOpen = false" />
    </div>

    <footer class="g-statusbar e-status" data-size="large">
      <span class="e-facts">{{ facts }}</span>
      <span class="g-spacer" />
      <span v-if="readout" class="e-readout">
        {{ t("editor.readout", { x: readout.x, y: readout.y }) }}
        <span class="e-chipcolor" :style="{ background: readout.css }" />{{ readout.hex }}
      </span>
      <div class="e-zoom" role="group" :aria-label="t('editor.zoom')">
        <TipWrap :tip="t('editor.fit')" :caps="['⌘', '0']">
          <button type="button" class="g-btn" data-size="small" :aria-label="t('editor.fit')" :disabled="!source" @click="applyFit('fit')"><GIcon name="fit" :size="16" /></button>
        </TipWrap>
        <TipWrap :tip="t('editor.actual')" :caps="['⌘', '1']">
          <button type="button" class="g-button e-actual" data-variant="plain" :disabled="!source" @click="zoomTo(100)">{{ pct(100) }}</button>
        </TipWrap>
        <span class="g-divider" role="separator" />
        <TipWrap :tip="t('editor.zoomOut')" :caps="['⌘', '−']">
          <button type="button" class="g-btn" data-size="small" :aria-label="t('editor.zoomOut')" :disabled="!source" @click="zoomTo(step(percent, -1))"><GIcon name="zoom-out" :size="16" /></button>
        </TipWrap>
        <label class="g-select e-zoomsel">
          <select :aria-label="t('editor.zoom')" :value="zoomValue" :disabled="!source" @change="onZoomMenu">
            <option value="current" hidden>{{ zoomLabel }}</option>
            <option value="fit">{{ zoomMode === "fit" ? zoomLabel : t("editor.fit") }}</option>
            <option value="width">{{ zoomMode === "width" ? zoomLabel : t("editor.fitWidth") }}</option>
            <option v-for="z in ZOOM_MENU" :key="z" :value="String(z)">{{ pct(z) }}</option>
          </select>
        </label>
        <TipWrap :tip="t('editor.zoomIn')" :caps="['⌘', '+']" align="end">
          <button type="button" class="g-btn" data-size="small" :aria-label="t('editor.zoomIn')" :disabled="!source" @click="zoomTo(step(percent, 1))"><GIcon name="zoom-in" :size="16" /></button>
        </TipWrap>
      </div>
    </footer>

    <UnsavedSheet
      v-if="sheet"
      :title="sheet.title"
      :text="sheet.text"
      :cancellable="sheet.cancellable"
      :busy="busy"
      @save="saveAndGo"
      @discard="answer(true)"
      @cancel="answer(false)"
    />
    <FirstUploadDialog
      v-if="uploadAsk"
      :consent="uploadAsk.consent"
      :file="uploadAsk.file"
      @confirmed="uploads.confirmed"
      @cancel="uploads.cancel"
      @stale="uploads.stale"
      @another-host="uploads.anotherHost"
    />
  </div>
</template>

<style scoped>
.e-window {
  position: relative;
  display: flex;
  flex-direction: column;
  height: 100%;
  overflow: hidden;
  background: var(--win-content);
}
.e-doc {
  display: flex;
  flex-direction: column;
  min-width: 0;
}
.e-doc-name {
  font: var(--type-title);
  color: var(--win-text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.e-doc-sub {
  font: var(--type-caption);
  color: var(--win-text-3);
  white-space: nowrap;
}
.e-main {
  position: relative;
  flex: 1;
  min-height: 0;
  display: flex;
}
.e-canvas {
  position: relative;
  flex: 1;
  min-width: 0;
  overflow: hidden;
  background: var(--win-sunken);
  touch-action: none;
  container-type: inline-size;
}
/* The image lifts off the canvas by a hairline shadow. */
.e-lift {
  position: absolute;
  box-shadow: var(--win-shadow-lift);
  pointer-events: none;
}
.e-pixgrid {
  position: absolute;
  pointer-events: none;
  background-image:
    linear-gradient(90deg, var(--crosshair-halo) var(--hairline), transparent var(--hairline)),
    linear-gradient(180deg, var(--crosshair-halo) var(--hairline), transparent var(--hairline));
  opacity: 0.55;
}
.e-crop-dim {
  z-index: 5;
}
.e-crop-edge {
  z-index: 6;
}
.e-crop-size {
  position: absolute;
  z-index: 7;
}
.e-crop-hint {
  position: absolute;
  z-index: 7;
  left: 50%;
  bottom: 60px;
  transform: translateX(-50%);
}
.e-textlayer {
  position: absolute;
  inset: 0;
  z-index: 8;
  pointer-events: none;
}
/* The tools sit in the window's own chrome, not on glass: a rail as tall as the canvas, with the
   room under the last tool left empty. No z-index, so its tooltips rise over the canvas. */
.e-rail {
  position: relative;
  flex: none;
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 2px;
  width: 48px;
  padding: 8px 0;
  background: var(--win-toolbar);
  box-shadow: inset calc(-1 * var(--hairline)) 0 0 var(--win-line);
}
.e-rail[data-scrolls] {
  overflow-y: auto;
}
.e-rail > .g-divider {
  width: 20px;
  height: var(--hairline);
  margin: 5px 0;
}
/* The options strip is a pill in the rail's chrome: the toolbar's background and the rail's
   hairline all round, no glass, so the window's surface roles style its controls. Without a `g-`
   class the kit's border-box reset does not reach it, so it sets its own; its shadow is softer
   than the kit's small one, as the strip sits on the window, not over the desktop. */
.e-options {
  position: absolute;
  left: 50%;
  bottom: 12px;
  z-index: 12;
  box-sizing: border-box;
  display: inline-flex;
  align-items: center;
  max-width: calc(100% - 24px);
  height: var(--strip-h);
  padding: 4px;
  border-radius: 999px;
  background: var(--win-toolbar);
  box-shadow: inset 0 0 0 var(--hairline) var(--win-line), 0 2px 6px rgba(0, 0, 0, 0.16), 0 1px 2px rgba(0, 0, 0, 0.10);
  transform: translateX(-50%);
}
.e-options > .g-divider {
  height: 16px;
}
.e-options[data-scrolls] {
  overflow-x: auto;
  overflow-y: hidden;
}
.e-options-tool {
  display: inline-flex;
  flex: none;
  align-items: center;
  gap: 6px;
  padding: 0 4px 0 8px;
  font: var(--type-label);
  color: var(--surface-text);
  white-space: nowrap;
}
.e-options-tool > g-icon {
  color: var(--surface-text-muted);
}
@container (max-width: 560px) {
  .e-options-name {
    display: none;
  }
}
.e-notice {
  position: absolute;
  top: 12px;
  z-index: 12;
  left: 50%;
  transform: translateX(-50%);
}
.e-empty {
  position: absolute;
  inset: 0;
  justify-content: center;
}
.e-drop {
  position: absolute;
  inset: 16px;
  z-index: 20;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 6px;
  border-radius: var(--radius-card);
  border: 2px dashed var(--iris);
  background: var(--window-tint);
  text-align: center;
  pointer-events: none;
}
.e-drop-title {
  margin-top: 10px;
  font: var(--type-heading);
  color: var(--win-text);
}
.e-drop-text {
  max-width: 360px;
  margin: 0;
  font: var(--type-text);
  color: var(--win-text-2);
}
.e-message {
  position: absolute;
  z-index: 25;
  left: 50%;
  bottom: 60px;
  transform: translateX(-50%);
  padding: 0 14px;
}
.e-status .e-facts {
  flex: 0 1 auto;
  min-width: 0;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.e-readout {
  display: inline-flex;
  flex: none;
  align-items: center;
  gap: 6px;
  font: var(--type-num-small);
  color: var(--win-text-2);
  white-space: nowrap;
}
.e-chipcolor {
  width: 12px;
  height: 12px;
  border-radius: 3px;
  box-shadow: inset 0 0 0 0.5px var(--win-line-strong);
}
.e-zoom {
  display: flex;
  flex: none;
  align-items: center;
  gap: 2px;
  margin-right: -6px;
}
.e-zoom .g-divider {
  height: 16px;
  margin: 0 6px;
  background: var(--win-line);
}
.e-actual {
  height: 28px;
}
.e-zoomsel {
  min-width: 0;
  width: 136px;
}
</style>
