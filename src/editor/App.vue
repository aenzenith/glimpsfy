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

import BeautifyPanel from "../components/BeautifyPanel.vue";
import GIcon from "../components/GIcon.vue";
import ProgressRing from "../components/ProgressRing.vue";
import TipWrap from "../components/TipWrap.vue";
import OptionsStrip from "../annotate/OptionsStrip.vue";
import RedactionNotice from "../annotate/RedactionNotice.vue";
import Surface from "../annotate/Surface.vue";
import { context2d, exportArea, redactedPixels, renderExport, scratchCanvas, type ImageSource } from "../annotate/compose";
import { AnnotationEngine, TOOL_OF } from "../annotate/engine";
import { defaultPrefs, flushPrefs, loadPrefs, savePrefs, type ToolId as AnnotateTool } from "../annotate/prefs";
import type { Bytes, PixelRect } from "../annotate/redact";
import { TEXT_LAYER, textLayerComponent, type TextLayerContext, type View } from "../annotate/textLayer";
import { useSnapWords } from "../annotate/useSnapWords";
import { thumbnailOfPixels, uploadFlow } from "../annotate/upload";
import {
  IMAGE_FALLBACK,
  IMAGE_TYPES,
  chosenStyle,
  drawBeautified,
  edgeMedian,
  layoutOf,
  outputSize,
  presetName,
  readPresets,
  styleOf,
  useBackgroundImage,
  withNewPreset,
  withPresetStyle,
} from "../annotate/beautify";
import { useRecognition } from "../ocr/useRecognition";
import SensitiveMarks from "../ocr/SensitiveMarks.vue";
import SensitivePanel from "../ocr/SensitivePanel.vue";
import { useSensitive } from "../ocr/sensitive";
import { CURRENT_VERSION, parseDocument, serializeDocument, writeBeautify, type AnnotationDocument, type BeautifyStyle } from "../lib/annotate";
import { ApiError, on, onWindow } from "../lib/api/core";
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
import { settings_get, settings_set } from "../lib/api/settings";
import { subject_cancel, subject_cut, subject_find } from "../lib/api/subject";
import { app_choose_file, pin_open, window_open_settings } from "../lib/api/windows";
import { SETTINGS_CHANGED } from "../lib/events";
import { language, t, tp, type Key } from "../lib/i18n";
import { commandHeld, commandKeyName } from "../lib/keys";
import { can, isMac } from "../lib/platform";
import type { ImageFormat, Settings } from "../lib/settings";
import { jobName } from "../ocr/text";
import { TOOLS, toolForKey, type ToolId } from "../overlay/tools";
import FirstUploadDialog from "../upload/FirstUploadDialog.vue";
import ObjectsPanel from "./ObjectsPanel.vue";
import Overview from "./Overview.vue";
import SubjectChip from "./SubjectChip.vue";
import UnsavedSheet from "./UnsavedSheet.vue";
import { hex as hexOf, read as readColor } from "../lib/color";
import { GRID_ZOOM, MAX_ZOOM, MIN_ZOOM, TALL, ZOOM_MENU, clampPan, fit, fitWidth, kOf, opening, percentOf, step, zoomAt, type Room, type ZoomMode } from "./view";

/** A handle's hit radius in screen points, as in the overlay. */
const HANDLE_HIT = 14;
/** Fit leaves this much under the image, shown or not, so the options strip never moves or
 * covers it: the strip, 12 px under it and 12 px over it. */
const OPTIONS_ROOM = 60;

function emptyDocument(width: number, height: number, scale: number): AnnotationDocument {
  return { version: CURRENT_VERSION, image: { width, height }, scale, objects: [], beautify: null };
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
/** A combined image the user has not saved yet, which lives only in this window. */
const unsavedNew = ref(false);
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
  if (unsavedNew.value) return count.value ? `${t("editor.combined.subtitle")} · ${countText.value}` : t("editor.combined.subtitle");
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
  // A combined image has no file, so no format or size of one: it says what it was made of.
  if (img.combinedFrom !== null) {
    parts.push(t("editor.combined.from", { n: img.combinedFrom }));
    if (count.value) parts.push(countText.value);
    return parts.join(" · ");
  }
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
/** The lift's shadow goes round the beautified image when there is a style. */
const liftBox = computed(() => (framed.value ? boxStyle(screen(framed.value)) : null));
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
// The text recognition layer reads the image through this (src/annotate/textLayer.ts), and the
// tools that snap to words share its reading.
const textLayer: TextLayerContext = {
  area,
  view,
  pixels: () => (source.value && area.value ? redactedPixels(source.value, engine.doc, area.value) : null),
  colorSpace: computed(() => source.value?.colorSpace ?? "srgb"),
  scale,
  visible: textLayerVisible,
  recognition: useRecognition(),
};
provide(TEXT_LAYER, textLayer);
const snapWords = useSnapWords(engine, textLayer, computed(() => source.value !== null));
const snapProblem = snapWords.problem;

// Smart redaction (D28): D opens its list in the objects panel's place; after Redact the objects
// panel shows the blurs it added, each an object of its own.
const sensitive = useSensitive(textLayer, engine);

function toggleSensitive(): void {
  if (sensitive.open.value) return sensitive.close();
  if (!source.value) return;
  engine.setTool(null);
  sensitive.start();
}

function applySensitive(): void {
  const kind = sensitive.apply();
  if (!kind) return;
  engine.setTool(kind);
  objectsOpen.value = true;
}

function openOcrSettings(): void {
  sensitive.close();
  void window_open_settings("ocr").catch((error) => console.warn("window_open_settings", error));
}

// ------------------------------------------------------------------------------ beautify
//
// The document holds the image's beautify style (B11), so each change in the panel is an undo
// step and the history keeps the style editable. The canvas draws the background, the shadow and
// the frame under the surface, and the surface's corners are rounded to the style's.

const settings = shallowRef<Settings | null>(null);
const beautifyOpen = ref(false);
const beautifyEl = ref<InstanceType<typeof BeautifyPanel> | null>(null);
const ringEl = ref<HTMLCanvasElement | null>(null);
/** Save writes PNG for this image alone, chosen over JPEG in the panel. */
const pngOnce = ref(false);
const beautifyStyle = computed(() => (rev.value, engine.doc.beautify));
/** The style the switch turns on: the last one this image had, else the chosen preset. */
const lastStyle = shallowRef<BeautifyStyle | null>(null);
watch(beautifyStyle, (style) => {
  if (style) lastStyle.value = style;
});
const panelStyle = computed(() => beautifyStyle.value ?? lastStyle.value ?? chosenStyle(settings.value));
const presets = computed(() => readPresets(settings.value?.beautify));
const autoId = computed(() => (settings.value?.beautify?.auto ? settings.value.beautify.preset : null));
const outputFormat = computed<ImageFormat>(() => settings.value?.output.format ?? "png");
/** The crop shows the whole image, so the style waits until the crop is applied. */
const ringShown = computed(() => beautifyStyle.value !== null && !cropMode.value && area.value !== null);
const ringLayout = computed(() => {
  const a = area.value;
  const style = beautifyStyle.value;
  return a && style && ringShown.value ? layoutOf({ from: a, scale: scale.value, style, window: false }) : null;
});
/** What the view fits: the beautified image around the area, or the area alone. */
const framed = computed<PixelRect | null>(() => {
  const a = area.value;
  const L = ringLayout.value;
  if (!a || !L) return a;
  return { x: a.x - L.pad, y: a.y - L.pad - L.bar, width: L.width, height: L.height };
});
/** The image's size in points, which the panel turns into the output's. */
const points = computed(() => {
  const a = area.value;
  const k = scale.value || 1;
  return a ? { width: a.width / k, height: a.height / k } : { width: 0, height: 0 };
});
/** The Match colour of the image's outer band; the core takes it from the rendered pixels. */
const matchColor = computed<[number, number, number] | null>(() => {
  const s = source.value;
  const a = area.value;
  if (!s || !a || (!beautifyOpen.value && panelStyle.value.background.kind !== "match")) return null;
  return edgeMedian(s.pixels, a);
});
const backgroundImage = useBackgroundImage(() => {
  const bg = panelStyle.value.background;
  return bg.kind === "image" ? bg.path : null;
}, 2048);
/** The surface's corners in screen points: the image's own lower corners under a window frame. */
const imageRadius = computed(() => {
  const L = ringLayout.value;
  if (!L || L.radius <= 0) return undefined;
  const r = `${L.radius * view.value.k}px`;
  return L.bar ? `0 0 ${r} ${r}` : r;
});
const beautifyFacts = computed(() => {
  const style = beautifyStyle.value;
  if (!style) return null;
  const size = outputSize(points.value.width, points.value.height, style, false);
  const format = pngOnce.value ? "PNG" : { png: "PNG", jpeg: "JPEG", webp: "WebP" }[outputFormat.value];
  const preset = presets.value.find((p) => p.id === style.preset);
  return `${size.width} × ${size.height} pt · ${format} · ${preset ? presetName(preset) : t("beautify.title")}`;
});

let ringFrame = 0;
function drawRing(): void {
  ringFrame = 0;
  const el = ringEl.value;
  const s = source.value;
  const a = area.value;
  const f = framed.value;
  const style = beautifyStyle.value;
  if (!el || !s || !a || !f || !style || !ringShown.value) return;
  const dpr = window.devicePixelRatio || 1;
  const w = Math.max(1, Math.round(room.width * dpr));
  const h = Math.max(1, Math.round(room.height * dpr));
  if (el.width !== w) el.width = w;
  if (el.height !== h) el.height = h;
  const ctx = el.getContext("2d", { colorSpace: s.colorSpace });
  if (!ctx) return;
  ctx.setTransform(1, 0, 0, 1, 0, 0);
  ctx.clearRect(0, 0, w, h);
  const { k, ox, oy } = view.value;
  ctx.setTransform(k * dpr, 0, 0, k * dpr, (ox + f.x * k) * dpr, (oy + f.y * k) * dpr);
  ctx.imageSmoothingQuality = "high";
  drawBeautified(ctx, {
    image: null,
    from: a,
    scale: scale.value,
    style,
    window: false,
    space: s.colorSpace,
    match: matchColor.value,
    backgroundImage: backgroundImage.image.value,
    jpeg: outputFormat.value === "jpeg" && !pngOnce.value,
    ringOnly: true,
  });
}
watch(
  () => [ringEl.value, ringShown.value, view.value, framed.value, beautifyStyle.value, matchColor.value, backgroundImage.image.value, outputFormat.value, pngOnce.value, room.width, room.height],
  () => {
    if (!ringFrame) ringFrame = requestAnimationFrame(drawRing);
  },
  { flush: "post" },
);

/** The rail's button opens the panel over the canvas, or closes it. */
function toggleBeautifyPanel(): void {
  if (!source.value) return;
  beautifyOpen.value = !beautifyOpen.value;
  if (beautifyOpen.value) void loadSettings();
}

/** F and the panel's switch: on with the last style, or off; one undo step either way. */
function toggleBeautify(next = beautifyStyle.value === null): void {
  if (!source.value || next === (beautifyStyle.value !== null)) return;
  engine.setBeautify(next ? panelStyle.value : null);
}

const changeBeautify = (style: BeautifyStyle, step: boolean) => engine.setBeautify(style, step);

function pickPreset(id: string): void {
  const preset = presets.value.find((p) => p.id === id);
  if (preset) engine.setBeautify(styleOf(preset));
}

async function chooseBackground(): Promise<void> {
  try {
    const path = await app_choose_file([{ name: t("beautify.images"), extensions: IMAGE_TYPES }]);
    if (!path) return;
    const style = panelStyle.value;
    const bg = style.background;
    const fallback = bg.kind === "gradient" ? bg.stops : bg.kind === "image" ? bg.fallback : IMAGE_FALLBACK;
    engine.setBeautify({ ...style, background: { kind: "image", path, fallback } });
  } catch (error) {
    console.warn("[glimpsfy] the image could not be chosen", error);
  }
}

async function loadSettings(): Promise<void> {
  try {
    settings.value = await settings_get();
  } catch (error) {
    console.warn("[glimpsfy] the beautify presets were not read", error);
  }
}

/** Writes the panel's values into its preset in Settings › Beautify. */
async function updatePreset(): Promise<void> {
  const style = panelStyle.value;
  if (!style.preset) return;
  try {
    const next = withPresetStyle(await settings_get(), style.preset, style);
    await settings_set(next);
    settings.value = next;
    const preset = readPresets(next.beautify).find((p) => p.id === style.preset);
    if (preset) say(t("beautify.updated", { name: presetName(preset) }));
  } catch (error) {
    console.warn("[glimpsfy] the preset was not updated", error);
    say(t("beautify.failed"));
  }
}

/** Adds the panel's values as a new preset, which the image's style then names; naming it is no
 * undo step of its own. */
async function createPreset(name: string): Promise<void> {
  try {
    const made = withNewPreset(await settings_get(), name, panelStyle.value);
    await settings_set(made.settings);
    settings.value = made.settings;
    const style = { ...panelStyle.value, preset: made.id };
    if (beautifyStyle.value) engine.setBeautify(style, false);
    else lastStyle.value = style;
    say(t("beautify.added", { name }));
  } catch (error) {
    console.warn("[glimpsfy] the preset was not added", error);
    say(t("beautify.failed"));
  }
}

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
  return tp(known.includes(key) ? key : "error.other");
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
    original = next;
    forgetSubjects();
    fileBytes.value = img.encoding === "file" ? buffer.byteLength : null;
    fromHistory.value = img.entryId !== null;
    unsavedNew.value = img.combinedFrom !== null;
    pngOnce.value = false;
    lastStyle.value = null;
    engine.setTool(null);
    // Drawings handed over by the overlay are not kept anywhere yet, and neither is a combined
    // image; an edit reopened from the history is already what its entry holds.
    engine.load(documentFor(img, width, height), (img.document !== null && img.entryId === null) || img.combinedFrom !== null);
    textLayerVisible.value = false;
    snapWords.reset();
    sensitive.close();
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
  // A file can be opened again, and a combined image made again, so their question offers
  // Cancel. A capture is already closed and cannot be refused, so only Save and Don't save remain.
  if (!expected && !(await settle(t("editor.unsaved.titleNext"), img.path !== null || img.combinedFrom !== null))) {
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
  if (unsavedNew.value) return t("editor.unsaved.lostCombined");
  const n = count.value;
  const unchanged = engine.baseDirty() ? t("editor.unsaved.lostBackground") : t("editor.unsaved.lostCrop");
  const lost = n === 0 ? unchanged : n === 1 ? t("editor.unsaved.lostOne") : t("editor.unsaved.lostOther", { n });
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
  if (!r) return null;
  // The core puts the document's beautify style around the pixels (`output/beautify.rs`).
  const beautify = engine.doc.beautify ? writeBeautify(engine.doc.beautify) : null;
  return { source: { kind: "editor", token: img.token }, width: r.width, height: r.height, colorSpace: r.colorSpace, data: r.data, beautify, format: pngOnce.value ? "png" : null };
}

/** Copy, Save and Save as; resolves whether the image left. `format` names the format Save as
 * suggests instead of the settings' (Save as PNG in the subject chip). */
async function output(kind: "copy" | "save" | "saveAs", format: ImageFormat | null = null): Promise<boolean> {
  if (busy.value) return false;
  busy.value = true;
  try {
    // The image without its background may still be on its way.
    await baseChain;
    const r = rendered();
    if (!r) return false;
    // The history keeps the document with the entry this output makes or updates.
    if (r.source.kind === "editor") await editor_set_document(r.source.token, JSON.stringify(serializeDocument(engine.doc)));
    if (kind === "copy") await output_copy_rendered(r);
    else {
      await (kind === "save" ? output_save_rendered(r) : output_save_as_rendered(format ? { ...r, format } : r));
      engine.markSaved();
      unsavedNew.value = false;
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
  busy.value = true;
  try {
    await baseChain;
    const r = rendered();
    if (!r) return;
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
    busy.value = true;
    try {
      await baseChain;
      const r = rendered();
      if (!r) return;
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

// The image's own scale, or the pin would take the scale of the screen under the pointer.
const pin = () => staged((path) => pin_open({ kind: "file", path, scale: scale.value }));

function copyText(): void {
  if (!textLayerComponent) return say(t("overlay.notReady"));
  textLayerVisible.value = !textLayerVisible.value;
}

// ---------------------------------------------------------------- removing the background

/** The subjects Vision found in the image: the number of the subject over each pixel (0 for the
 * background) and a thumbnail of each. */
type Lift = { token: string; width: number; height: number; labels: Uint8Array; count: number; thumbs: string[] };

/** A thumbnail's side in pixels: the chip's 28 points on a 2× screen. */
const THUMB = 56;
/** The longest side of the mask the outline of a subject is drawn from. */
const MASK = 2048;

const lift = shallowRef<Lift | null>(null);
/** The image's own pixels: the subjects are found in them and they come back with the background. */
let original: ImageSource | null = null;
const chip = ref<"working" | "none" | "failed" | "done" | null>(null);
/** The subjects the image without its background keeps, by number. */
const keep = ref<number[]>([]);
const jpegOutput = ref(false);
/** Vision is looking; `liftSlow` once it has taken 300 ms, when the ring and the sweep show. */
const lifting = ref(false);
const liftSlow = ref(false);
let liftJob: string | null = null;
let slowTimer = 0;
/** Each change of the base reaches the core in turn, and every output waits for the last one, so
 * the pixels it renders and the record the core files them under belong to the same image. */
let baseChain: Promise<void> = Promise.resolve();
const hoveredSubject = ref<number | null>(null);
const haloEl = ref<HTMLCanvasElement | null>(null);
const masks = new Map<number, HTMLCanvasElement>();

const lifted = computed(() => (rev.value, engine.base !== null));
const keyOf = (subjects: readonly number[]) => [...subjects].sort((a, b) => a - b).join(",");
const subjectsOf = (key: string) => key.split(",").map(Number);

engine.onBase = (base) => {
  void applyBase(base);
  hoveredSubject.value = null;
  if (base === null) chip.value = null;
  else if (lift.value) {
    keep.value = subjectsOf(base);
    chip.value = "done";
  }
};

/** Remove background (⌥⌘B); pressed again, it brings the background back. Each is one undo step;
 * choosing the subjects in the chip is none. */
async function removeBackground(): Promise<void> {
  const img = image.value;
  if (!img || !original || lifting.value || !can("subjectLift")) return;
  engine.finishText();
  if (engine.base !== null) {
    chip.value = null;
    hoveredSubject.value = null;
    engine.setBase(null);
    void applyBase(null);
    return;
  }
  const found = await findSubjects(img.token, original);
  if (!found || image.value?.token !== img.token || engine.base !== null) return;
  if (found.count === 0) {
    chip.value = "none";
    return;
  }
  keep.value = Array.from({ length: found.count }, (_, i) => i + 1);
  const key = keyOf(keep.value);
  engine.setBase(key);
  chip.value = "done";
  void applyBase(key);
  void checkFormat();
}

async function findSubjects(token: string, own: ImageSource): Promise<Lift | null> {
  if (lift.value?.token === token) return lift.value;
  const job = jobName();
  liftJob = job;
  lifting.value = true;
  slowTimer = window.setTimeout(() => {
    if (liftJob !== job) return;
    liftSlow.value = true;
    chip.value = "working";
  }, 300);
  try {
    const { width, height, data } = own.pixels;
    const buffer = await subject_find({ token, width, height, colorSpace: own.colorSpace, data }, job);
    if (liftJob !== job) return null;
    const labels = new Uint8Array(buffer);
    let count = 0;
    for (const label of labels) if (label > count) count = label;
    const found: Lift = { token, width, height, labels, count, thumbs: [] };
    found.thumbs = thumbnails(found, own);
    lift.value = found;
    if (chip.value === "working") chip.value = null;
    return found;
  } catch (error) {
    if (liftJob !== job) return null;
    if (error instanceof ApiError && error.code === "cancelled") chip.value = null;
    else {
      console.warn("[glimpsfy] the subjects could not be lifted", error);
      chip.value = "failed";
    }
    return null;
  } finally {
    if (liftJob === job) endLifting();
  }
}

/** Esc and Cancel while Vision is looking: the image stays as it was. */
function cancelLift(): void {
  const job = liftJob;
  if (!job) return;
  endLifting();
  chip.value = null;
  void subject_cancel(job).catch(() => {});
}

function endLifting(): void {
  liftJob = null;
  lifting.value = false;
  liftSlow.value = false;
  clearTimeout(slowTimer);
}

/** A new image: its subjects are still to be found. */
function forgetSubjects(): void {
  if (liftJob) cancelLift();
  lift.value = null;
  chip.value = null;
  keep.value = [];
  hoveredSubject.value = null;
  masks.clear();
  baseChain = Promise.resolve();
}

/** Shows the image `key` names (the original for `null`) and tells the core which image the
 * outputs are, so they go to the history record that belongs to it. */
function applyBase(key: string | null): Promise<void> {
  const token = image.value?.token;
  const own = original;
  const run = async () => {
    if (!token || !own || image.value?.token !== token) return;
    const buffer = await subject_cut(token, key === null ? null : subjectsOf(key));
    // A newer image or a newer change of the base has the last word.
    if (image.value?.token !== token || engine.base !== key) return;
    if (key === null) source.value = own;
    else {
      const { width, height } = own.pixels;
      const data = new Uint8ClampedArray(buffer) as Bytes;
      if (data.length !== width * height * 4) throw new Error("the image without its background does not match the image");
      const canvas = scratchCanvas(width, height);
      context2d(canvas, own.colorSpace).putImageData(new ImageData(data, width, height, { colorSpace: own.colorSpace }), 0, 0);
      source.value = { pixels: { width, height, data }, colorSpace: own.colorSpace, canvas };
    }
  };
  baseChain = baseChain.then(run).catch((error) => say(errorText(error)));
  return baseChain;
}

/** A tap on a thumbnail drops its subject or brings it back; the last one stays. */
function toggleSubject(n: number): void {
  const next = keep.value.includes(n) ? keep.value.filter((k) => k !== n) : [...keep.value, n];
  if (next.length === 0) return;
  keep.value = next.sort((a, b) => a - b);
  const key = keyOf(next);
  engine.replaceBase(key);
  void applyBase(key);
}

/** Save fills the transparent areas with white when the output format is JPEG, so the chip says so. */
async function checkFormat(): Promise<void> {
  try {
    jpegOutput.value = (await settings_get()).output.format === "jpeg";
  } catch {
    jpegOutput.value = false;
  }
}

/** Each subject on its own, fitted into a square: averaged from four samples per pixel of the
 * image's pixels where the subject covers them. */
function thumbnails(found: Lift, own: ImageSource): string[] {
  const { width, height, labels, count } = found;
  const data = own.pixels.data;
  const boxes = Array.from({ length: count }, () => ({ x0: width, y0: height, x1: -1, y1: -1 }));
  for (let y = 0; y < height; y++) {
    const row = y * width;
    for (let x = 0; x < width; x++) {
      const label = labels[row + x];
      if (!label) continue;
      const b = boxes[label - 1];
      if (x < b.x0) b.x0 = x;
      if (x > b.x1) b.x1 = x;
      if (y < b.y0) b.y0 = y;
      if (y > b.y1) b.y1 = y;
    }
  }
  return boxes.map((b, i) => {
    if (b.x1 < 0) return "";
    const n = i + 1;
    const bw = b.x1 - b.x0 + 1;
    const bh = b.y1 - b.y0 + 1;
    const k = Math.max(bw, bh) / THUMB;
    const left = b.x0 - (THUMB * k - bw) / 2;
    const top = b.y0 - (THUMB * k - bh) / 2;
    const out = new Uint8ClampedArray(THUMB * THUMB * 4) as Bytes;
    for (let ty = 0; ty < THUMB; ty++) {
      for (let tx = 0; tx < THUMB; tx++) {
        let r = 0, g = 0, bl = 0, a = 0;
        for (const [fx, fy] of [[0.25, 0.25], [0.75, 0.25], [0.25, 0.75], [0.75, 0.75]]) {
          const sx = Math.floor(left + (tx + fx) * k);
          const sy = Math.floor(top + (ty + fy) * k);
          if (sx < 0 || sy < 0 || sx >= width || sy >= height || labels[sy * width + sx] !== n) continue;
          const j = (sy * width + sx) * 4;
          const alpha = data[j + 3];
          r += data[j] * alpha;
          g += data[j + 1] * alpha;
          bl += data[j + 2] * alpha;
          a += alpha;
        }
        if (!a) continue;
        const o = (ty * THUMB + tx) * 4;
        out[o] = r / a;
        out[o + 1] = g / a;
        out[o + 2] = bl / a;
        out[o + 3] = a / 4;
      }
    }
    const canvas = document.createElement("canvas");
    canvas.width = THUMB;
    canvas.height = THUMB;
    canvas.getContext("2d", { colorSpace: own.colorSpace })?.putImageData(new ImageData(out, THUMB, THUMB, { colorSpace: own.colorSpace }), 0, 0);
    return canvas.toDataURL("image/png");
  });
}

/** The white silhouette of subject `n` at no more than `MASK` pixels a side, drawn under the
 * image so that only its rim and glow show around the subject. */
function maskOf(n: number): HTMLCanvasElement | null {
  const found = lift.value;
  if (!found) return null;
  const cached = masks.get(n);
  if (cached) return cached;
  const f = Math.min(1, MASK / Math.max(found.width, found.height));
  const w = Math.max(1, Math.round(found.width * f));
  const h = Math.max(1, Math.round(found.height * f));
  const out = new Uint8ClampedArray(w * h * 4) as Bytes;
  for (let y = 0; y < h; y++) {
    const sy = Math.min(found.height - 1, Math.floor((y + 0.5) / f));
    for (let x = 0; x < w; x++) {
      const sx = Math.min(found.width - 1, Math.floor((x + 0.5) / f));
      if (found.labels[sy * found.width + sx] === n) out.fill(255, (y * w + x) * 4, (y * w + x) * 4 + 4);
    }
  }
  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  canvas.getContext("2d")?.putImageData(new ImageData(out, w, h), 0, 0);
  masks.set(n, canvas);
  return canvas;
}

/** Where the outline of the subject under the pointer goes: clipped to the shown part of the
 * image, with the whole image's mask offset inside it. */
const halo = computed(() => {
  const n = hoveredSubject.value;
  if (n === null || !lifted.value || !keep.value.includes(n) || !area.value || !lift.value) return null;
  const { k } = view.value;
  return {
    clip: boxStyle(screen(area.value)),
    image: { left: px(-area.value.x * k), top: px(-area.value.y * k), width: px(lift.value.width * k), height: px(lift.value.height * k) },
  };
});
watch(
  [hoveredSubject, haloEl],
  ([n, canvas]) => {
    if (n === null || !canvas) return;
    const mask = maskOf(n);
    if (!mask) return;
    canvas.width = mask.width;
    canvas.height = mask.height;
    canvas.getContext("2d")?.drawImage(mask, 0, 0);
  },
  { flush: "post" },
);

// A tool takes the strip's place back, and the chip goes.
watch(pick, (now) => {
  if (now && chip.value !== null && chip.value !== "working") chip.value = null;
});

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
  if (!framed.value) return;
  const chosen = opening(framed.value, room, scale.value);
  view.value = chosen.view;
  zoomMode.value = chosen.mode;
}

/** A tall image (a scrolling capture's result) that overflows the canvas shows its overview at
 * the right edge, in the room fit width leaves there. */
const overviewShown = computed(() => {
  const a = area.value;
  if (!a || !source.value || a.height <= a.width * TALL) return false;
  return a.height * view.value.k > room.height - room.top - room.bottom;
});

/** The overview's press or drag: image row `y` comes to the middle of the canvas. */
function panTo(y: number): void {
  if (!framed.value) return;
  const k = view.value.k;
  view.value = clampPan({ k, ox: view.value.ox, oy: room.height / 2 - y * k }, framed.value, room);
}

function applyFit(mode: "fit" | "width"): void {
  if (!framed.value) return;
  view.value = mode === "fit" ? fit(framed.value, room, scale.value) : fitWidth(framed.value, room, scale.value);
  zoomMode.value = mode;
}

function zoomTo(percentValue: number, at?: { x: number; y: number }): void {
  if (!framed.value) return;
  const p = Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, percentValue));
  const centre = at ?? { x: room.width / 2, y: (room.top + room.height) / 2 };
  view.value = clampPan(zoomAt(view.value, kOf(p, scale.value), centre.x, centre.y), framed.value, room);
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
// A beautify style that grows or shrinks the image refits a view that is fitted, and leaves a
// zoom the user chose as it is.
watch(
  () => (framed.value ? `${framed.value.x},${framed.value.y},${framed.value.width},${framed.value.height}` : ""),
  (now, before) => {
    if (now && before && now !== before && zoomMode.value !== "custom") applyFit(zoomMode.value);
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
    engine.hover(null, 0);
    e.preventDefault();
    return;
  }
  if (e.button !== 0) return;
  // With ⌘ held a press only draws, even on top of another object; with ⌥ it draws freely.
  const result = engine.pointerDown(toImage(local(e)), reach(), commandHeld(e), e.altKey);
  if (result === "handle" || result === "object" || result === "draw") target.setPointerCapture(e.pointerId);
  // A press that starts a text leaves the focus to its field.
  if (result !== "text") e.preventDefault();
}

function onPointerMove(e: PointerEvent): void {
  if (pan && framed.value) {
    view.value = clampPan({ k: pan.view.k, ox: pan.view.ox + e.clientX - pan.x, oy: pan.view.oy + e.clientY - pan.y }, framed.value, room);
    zoomMode.value = "custom";
    return;
  }
  const p = toImage(local(e));
  if (engine.busy()) {
    engine.pointerMove(p, { shift: e.shiftKey, alt: e.altKey });
    return;
  }
  lastPoint = p;
  aim(p, commandHeld(e), e.altKey);
  updateReadout(p);
}

/** The pointer's last place in image pixels, so ⌘ or ⌥ going down or up can change the cursor. */
let lastPoint: { x: number; y: number } | null = null;

/** The cursor and the hover frame at `p`; `over` is ⌘ held, when a press only draws, and `free`
 * is ⌥ held, when a snapping tool shows no text cursor. */
function aim(p: { x: number; y: number }, over: boolean, free = false): void {
  cursor.value = spaceHeld.value ? "grab" : (engine.cursorAt(p, reach(), over, free) ?? "default");
  // With Space held a press pans and with ⌘ it only draws, so nothing is about to be picked up.
  engine.hover(spaceHeld.value || over ? null : p, reach());
}

function onPointerLeave(): void {
  lastPoint = null;
  engine.hover(null, 0);
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
  view.value = clampPan({ k: view.value.k, ox: view.value.ox - e.deltaX, oy: view.value.oy - e.deltaY }, framed.value ?? area.value, room);
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
  // HEX is sRGB, as the colour picker writes it: a P3 image's bytes are converted, and a colour
  // outside sRGB shows with ≈.
  const reading = readColor({ space: s.colorSpace === "display-p3" ? "display-p3" : "srgb", bytes: [r, g, b] });
  const hex = `${reading.out ? "≈ " : ""}${hexOf(reading.srgb)}`;
  const css = reading.css;
  readout.value = { x, y, hex, css };
}

// ---------------------------------------------------------------------------- the tools

function chooseTool(id: ToolId): void {
  if (!source.value) return;
  if (id === "undo") return engine.undo();
  if (id === "redo") return engine.redo();
  if (id === "sensitive") return toggleSensitive();
  // A drawing tool puts the list of smart redaction away.
  if (sensitive.open.value) sensitive.close();
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
  // The list of smart redaction keeps ↩ and Esc while one of its switches has the focus.
  if (sensitive.open.value && (e.key === "Enter" || e.key === "Escape") && !target?.isContentEditable && !engine.busy()) {
    e.preventDefault();
    if (e.key === "Enter") applySensitive();
    else sensitive.close();
    return;
  }
  // The beautify panel's sliders keep only their own keys (`BeautifyPanel.vue`).
  const slider = target instanceof HTMLInputElement && target.type === "range";
  if (target && (target.isContentEditable || (!slider && ["INPUT", "SELECT", "TEXTAREA"].includes(target.tagName)))) return;
  // ⌘ on macOS, where ⌃ has always worked too; Ctrl elsewhere, where the Windows key is the system's.
  const command = isMac() ? e.metaKey || e.ctrlKey : e.ctrlKey;
  if (command && e.altKey && e.code === "KeyO") {
    e.preventDefault();
    objectsOpen.value = !objectsOpen.value;
    return;
  }
  if (!source.value) return;
  if (e.key === "Escape" && lifting.value) {
    e.preventDefault();
    cancelLift();
    return;
  }
  // ⌥ changes the letter a key types on macOS, so the key is read by its place.
  if (command && e.altKey && e.code === "KeyB") {
    e.preventDefault();
    void removeBackground();
    return;
  }
  if (e.key === " " && !command) {
    e.preventDefault();
    spaceHeld.value = true;
    cursor.value = "grab";
    return;
  }
  if (e.key === commandKeyName()) {
    if (lastPoint && !pan && !engine.busy()) aim(lastPoint, true, e.altKey);
    return;
  }
  // ⌥ frees a snapping tool's drawing from the words, during a drag too.
  if (e.key === "Alt") {
    if (engine.busy()) engine.setFree(true);
    else if (lastPoint && !pan) aim(lastPoint, commandHeld(e), true);
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
    if (beautifyOpen.value) {
      if (!beautifyEl.value?.escape()) beautifyOpen.value = false;
    } else if (textLayerVisible.value) textLayerVisible.value = false;
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
    // Windows and Linux apps also redo with Ctrl+Y.
    if (!isMac() && !e.shiftKey) run.y = () => engine.redo();
    const action = run[k];
    if (action) {
      e.preventDefault();
      void action();
    }
    return;
  }
  if (e.altKey) return;
  // F turns the image's beautify style on or off; ⇧F is free.
  if (e.key.toLowerCase() === "f" && !e.shiftKey && !cropMode.value) {
    e.preventDefault();
    toggleBeautify();
    return;
  }
  const match = toolForKey(e.key);
  if (match) chooseTool(match.id);
}

function onKeyUp(e: KeyboardEvent): void {
  if (e.key === commandKeyName() && lastPoint && !pan && !engine.busy()) aim(lastPoint, false, e.altKey);
  if (e.key === "Alt") {
    if (engine.busy()) engine.setFree(false);
    else if (lastPoint && !pan) aim(lastPoint, commandHeld(e), false);
  }
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

  void loadSettings();
  const win = getCurrentWebviewWindow();
  stops.push(await on<Settings>(SETTINGS_CHANGED, (next) => (settings.value = next)));
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
  cancelAnimationFrame(ringFrame);
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
      <TipWrap v-if="can('subjectLift')" :tip="t('action.removeBackground')" :caps="['⌥', '⌘', 'B']" side="below">
        <button
          type="button"
          class="g-btn"
          :aria-label="t('action.removeBackground')"
          aria-keyshortcuts="Alt+Meta+B"
          :aria-pressed="liftSlow ? undefined : lifted"
          :aria-busy="lifting || undefined"
          :disabled="!source"
          @click="removeBackground"
        >
          <ProgressRing v-if="liftSlow" :progress="null" :label="t('editor.subject.working')" :size="18" />
          <GIcon v-else name="subject" />
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
              :aria-pressed="item.momentary ? undefined : tool === item.id || (item.id === 'sensitive' && sensitive.open.value)"
              :disabled="!source || (item.id === 'undo' && !undoable) || (item.id === 'redo' && !redoable)"
              @click="chooseTool(item.id)"
            >
              <GIcon :name="item.id" />
            </button>
          </TipWrap>
        </template>
        <span class="e-rail-end">
          <TipWrap :tip="t('beautify.title')" :caps="['F']" side="right">
            <button
              type="button"
              class="g-btn"
              :aria-label="t('beautify.title')"
              :aria-expanded="beautifyOpen"
              :data-state="beautifyOpen ? 'pressed' : undefined"
              :data-on="beautifyStyle ? '' : undefined"
              :disabled="!source"
              @click="toggleBeautifyPanel"
            >
              <GIcon name="beautify" />
            </button>
          </TipWrap>
        </span>
      </div>
      <div
        ref="canvasEl"
        class="e-canvas"
        :aria-label="t('editor.canvas')"
        :style="{ cursor, '--e-image-radius': imageRadius }"
        @pointerdown="onPointerDown"
        @pointermove="onPointerMove"
        @pointerup="onPointerUp"
        @pointercancel="onPointerUp"
        @pointerleave="onPointerLeave"
        @dblclick="onDoubleClick"
        @wheel="onWheel"
      >
        <div v-if="liftBox" class="e-lift" :style="liftBox" :data-alpha="lifted || undefined" aria-hidden="true" />
        <!-- The beautify style's background, shadow and frame, under the surface. -->
        <canvas v-if="ringShown && source" :key="source.colorSpace" ref="ringEl" class="e-ring" aria-hidden="true" />
        <div v-if="halo" class="e-halo" :style="halo.clip" aria-hidden="true"><canvas ref="haloEl" :style="halo.image" /></div>
        <Surface v-if="source" :engine="engine" :source="source" :view="view" :area="area" />
        <Overview v-if="overviewShown && source && area" :source="source" :area="area" :view="view" :room="room" @pan="panTo" />
        <div v-if="liftSlow && imageBox" class="e-sweep" :style="imageBox" aria-hidden="true" />
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

        <!-- What smart redaction found, marked over the image while its list is open. -->
        <div v-if="sensitive.open.value && source" class="e-textlayer">
          <SensitiveMarks :sensitive="sensitive" :view="view" />
        </div>
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
          <OptionsStrip :engine="engine" :tool="pick.tool" :snap-problem="snapProblem" />
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

        <SubjectChip
          v-if="chip && source && !pick"
          :state="chip"
          :count="lift?.count ?? 0"
          :keep="keep"
          :thumbs="lift?.thumbs ?? []"
          :jpeg="jpegOutput"
          @toggle="toggleSubject"
          @hover="hoveredSubject = $event"
          @copy="output('copy')"
          @cancel="cancelLift"
          @hide="chip = null; hoveredSubject = null"
          @save-png="output('saveAs', 'png')"
        />

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
        <BeautifyPanel
          v-if="beautifyOpen && source && !cropMode"
          ref="beautifyEl"
          class="e-beautify"
          variant="window"
          scope="image"
          :on="beautifyStyle !== null"
          :current="panelStyle"
          :presets="presets"
          :auto-id="autoId"
          :window="false"
          :format="outputFormat"
          :png-once="pngOnce"
          :points="points"
          :match="matchColor"
          :missing="backgroundImage.missing.value"
          :background-image="backgroundImage.image.value"
          :space="source.colorSpace"
          :preview="null"
          @toggle="toggleBeautify"
          @change="changeBeautify"
          @pick="pickPreset"
          @png="pngOnce = true"
          @choose-image="chooseBackground"
          @update="updatePreset"
          @create="createPreset"
        />
        <div v-if="message" class="g-hint g-glass e-message" role="status">{{ message }}</div>
      </div>
      <SensitivePanel
        v-if="sensitive.open.value && source"
        variant="side"
        :sensitive="sensitive"
        @close="sensitive.close()"
        @apply="applySensitive()"
        @settings="openOcrSettings()"
      />
      <ObjectsPanel v-else-if="objectsOpen" :engine="engine" @close="objectsOpen = false" />
    </div>

    <footer class="g-statusbar e-status" data-size="large">
      <span class="e-facts">{{ beautifyFacts ?? facts }}</span>
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
/* The beautify style's ring fills the canvas and draws only around the image; the surface's
   corners follow the style's (B11). */
.e-ring {
  position: absolute;
  inset: 0;
  width: 100%;
  height: 100%;
  pointer-events: none;
}
.e-canvas :deep(.a-canvas) {
  border-radius: var(--e-image-radius, 0);
}
.e-beautify {
  position: absolute;
  left: 8px;
  bottom: 10px;
  z-index: 13;
}
.e-rail-end {
  display: flex;
  margin-top: auto;
  padding-top: 8px;
}
.e-rail .g-btn[data-on] {
  color: var(--surface-iris-text);
}
/* Without its background the image sits on the checkerboard that stands for transparency. */
.e-lift[data-alpha] {
  box-shadow: 0 0 0 var(--hairline) var(--win-line);
  background: repeating-conic-gradient(#d8d8de 0 25%, #ffffff 0 50%) 0 0 / 14px 14px;
}
[data-theme="dark"] .e-lift[data-alpha] {
  background: repeating-conic-gradient(#3a3a42 0 25%, #1c1c21 0 50%) 0 0 / 14px 14px;
}
/* The subject under the pointer in the chip: its white silhouette lies under the image, so only
   the rim and the glow around it show. */
.e-halo {
  position: absolute;
  overflow: hidden;
  pointer-events: none;
}
.e-halo > canvas {
  position: absolute;
  filter: drop-shadow(0 0 1.1px #fff) drop-shadow(0 0 1.1px #fff) drop-shadow(0 0 6px rgba(255, 255, 255, 0.8));
}
/* A light passing over the image while Vision looks. */
.e-sweep {
  position: absolute;
  z-index: 4;
  pointer-events: none;
  background: linear-gradient(118deg, rgba(255, 255, 255, 0) 28%, rgba(255, 255, 255, 0.5) 44%, rgba(255, 255, 255, 0) 60%) no-repeat;
  background-size: 300% 100%;
  animation: e-sweep 1.6s ease-in-out infinite;
}
@keyframes e-sweep {
  from {
    background-position: 100% 0;
  }
  to {
    background-position: 0 0;
  }
}
@media (prefers-reduced-motion: reduce) {
  .e-sweep {
    animation: none;
    background-position: 50% 0;
  }
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
