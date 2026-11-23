// Beautify on the page (B11): the presets of Settings › Beautify as the pages read them, the
// panel's swatches, and the preview, which draws a style around an image the way
// src-tauri/src/output/beautify.rs renders it into the delivered file.
//
// The preview is drawn in image pixels on a canvas in the image's own colour space: the style's
// sRGB colours are given as CSS colours, which the canvas converts, and a Match colour, taken from
// the image's own values, is given in that space, so nothing of the capture is converted. A canvas
// shadow approximates the core's three box blurs; both are the Gaussian of half the blur.

import { ref, shallowRef, watch } from "vue";

import { FRAME_BAR, MAX_CORNER, MAX_PADDING, SHADOW_GEOMETRY, readBeautify, writeBeautify, type Background, type BeautifyStyle } from "../lib/annotate";
import type { ColorSpace } from "../lib/api/capture";
import { t, type Key } from "../lib/i18n";
import { platform } from "../lib/platform";
import type { Beautify, Settings } from "../lib/settings";

/** A preset as Settings › Beautify keeps it. A built-in one has no `name` until it is renamed. */
export type Preset = { id: string; name: string | null; style: BeautifyStyle };

/** The presets a new installation has (`built_in_presets` in crates/annotate). */
export const BUILT_IN = ["ocean", "sunset", "paper", "night", "match"] as const;
export const DEFAULT_PRESET = "ocean";

const OCEAN: BeautifyStyle = {
  preset: "ocean",
  padding: 48,
  corner: 12,
  shadow: "medium",
  background: { kind: "gradient", stops: ["#2563ff", "#2fc6c0"] },
  frame: "none",
};

/** The readable presets of the settings, in their order; an entry a newer version wrote is
 * skipped here and stays in the file. */
export function readPresets(beautify: Beautify | undefined): Preset[] {
  const list = Array.isArray(beautify?.presets) ? beautify.presets : [];
  const out: Preset[] = [];
  for (const entry of list) {
    try {
      if (typeof entry !== "object" || entry === null) continue;
      const e = entry as Record<string, unknown>;
      if (typeof e.id !== "string" || e.id === "") continue;
      out.push({ id: e.id, name: typeof e.name === "string" ? e.name : null, style: readBeautify(e.style, "preset.style") });
    } catch {
      // A preset this version cannot read is not offered.
    }
  }
  return out;
}

/** The name the user sees: their own, or the catalogue's for a built-in preset. */
export function presetName(preset: Preset): string {
  if (preset.name) return preset.name;
  return (BUILT_IN as readonly string[]).includes(preset.id) ? t(`beautify.preset.${preset.id}` as Key) : preset.id;
}

/** The style of `preset` with its id in it. */
export const styleOf = (preset: Preset): BeautifyStyle => ({ ...preset.style, preset: preset.id });

/** The style F turns on: the chosen preset's, else the first readable one, else Ocean
 * (`Beautify::chosen` in settings.rs). */
export function chosenStyle(settings: Settings | null): BeautifyStyle {
  const presets = readPresets(settings?.beautify);
  const chosen = presets.find((p) => p.id === settings?.beautify?.preset) ?? presets[0];
  return chosen ? styleOf(chosen) : { ...OCEAN };
}

/** The style inside its ranges, as the core clamps it. */
export function clamped(style: BeautifyStyle): BeautifyStyle {
  const fit = (v: number, max: number) => (Number.isFinite(v) ? Math.min(max, Math.max(0, v)) : 0);
  return { ...style, padding: fit(style.padding, MAX_PADDING), corner: fit(style.corner, MAX_CORNER) };
}

// ------------------------------------------------------------------------------- swatches

export type Swatch = { id: string; label: Key; background: Background };

/** The panel's backgrounds in their order; Image comes after them as its own button. */
export const SWATCHES: Swatch[] = [
  { id: "transparent", label: "beautify.bg.transparent", background: { kind: "transparent" } },
  { id: "match", label: "beautify.bg.match", background: { kind: "match" } },
  { id: "paper", label: "beautify.bg.paper", background: { kind: "solid", color: "#efe8dc" } },
  { id: "night", label: "beautify.bg.night", background: { kind: "solid", color: "#17181f" } },
  { id: "ocean", label: "beautify.bg.ocean", background: { kind: "gradient", stops: ["#2563ff", "#2fc6c0"] } },
  { id: "sunset", label: "beautify.bg.sunset", background: { kind: "gradient", stops: ["#ff8a4c", "#ff4f8b", "#8b4dff"] } },
  { id: "lavender", label: "beautify.bg.lavender", background: { kind: "gradient", stops: ["#c9c2ff", "#f6d9ef"] } },
];

/** The gradient an image background falls back to when the user picks an image. */
export const IMAGE_FALLBACK = ["#ff8a4c", "#ff4f8b", "#8b4dff"];
/** The file types a background image may be: what macOS reads (`output/beautify.rs`); elsewhere
 * the core reads PNG and JPEG, and another type shows the fallback gradient. */
export const IMAGE_TYPES = ["png", "jpg", "jpeg", "heic", "heif", "tiff", "tif", "webp", "gif", "bmp"];

export function sameBackground(a: Background, b: Background): boolean {
  if (a.kind !== b.kind) return false;
  if (a.kind === "solid" && b.kind === "solid") return a.color.toLowerCase() === b.color.toLowerCase();
  if (a.kind === "gradient" && b.kind === "gradient") return a.stops.join() === b.stops.join();
  if (a.kind === "image" && b.kind === "image") return a.path === b.path;
  return true;
}

const CHECKER = "repeating-conic-gradient(#c9c9cf 0 25%, #ffffff 0 50%) 50% / 8px 8px";

/** The CSS fill of a swatch or a preset's dot. */
export function backgroundCss(b: Background, match: string | null = null): string {
  switch (b.kind) {
    case "transparent":
      return CHECKER;
    case "solid":
      return b.color;
    case "gradient":
      return b.stops.length > 1 ? `linear-gradient(135deg, ${b.stops.join(", ")})` : (b.stops[0] ?? "#000");
    case "image":
      return `linear-gradient(135deg, ${b.fallback.join(", ")})`;
    case "match":
      return match ?? "#fbfbfd";
  }
}

/** The output's size in points for an image of `w` × `h` points. */
export function outputSize(w: number, h: number, style: BeautifyStyle, window: boolean): { width: number; height: number } {
  const s = clamped(style);
  const bar = !window && s.frame !== "none" ? FRAME_BAR : 0;
  return { width: Math.round(w + 2 * s.padding), height: Math.round(h + bar + 2 * s.padding) };
}

/** The address of a background image's preview, at most `side` pixels long (`frames.rs`). */
export function backgroundUrl(path: string, side: number): string {
  // Windows serves custom schemes over http, as `frames.rs` writes its own addresses.
  const base = platform.os === "windows" ? "http://glimpsfy-frame.localhost/background" : "glimpsfy-frame://background";
  return `${base}/${Math.round(side)}?t=${encodeURIComponent(path)}`;
}

/** The file name of a path, for the missing-image warning. */
export const fileName = (path: string): string => path.split(/[\\/]/).pop() || path;

// ---------------------------------------------------------------------------- the colours

/** The median of each channel over the outermost 2-pixel band of `rect` in `pixels`, counting
 * opaque pixels only, as the core takes it (`edge_median`). */
export function edgeMedian(pixels: { data: Uint8ClampedArray | Uint8Array; width: number; height: number }, rect?: { x: number; y: number; width: number; height: number }): [number, number, number] {
  const r = rect ?? { x: 0, y: 0, width: pixels.width, height: pixels.height };
  const x0 = Math.max(0, Math.round(r.x));
  const y0 = Math.max(0, Math.round(r.y));
  const w = Math.min(pixels.width - x0, Math.round(r.width));
  const h = Math.min(pixels.height - y0, Math.round(r.height));
  const counts = [new Uint32Array(256), new Uint32Array(256), new Uint32Array(256)];
  let total = 0;
  const add = (x: number, y: number) => {
    const i = ((y0 + y) * pixels.width + x0 + x) * 4;
    if (pixels.data[i + 3] !== 255) return;
    for (let c = 0; c < 3; c++) counts[c][pixels.data[i + c]] += 1;
    total += 1;
  };
  for (let y = 0; y < h; y++) {
    if (y < 2 || y + 2 >= h) for (let x = 0; x < w; x++) add(x, y);
    else for (const x of new Set([0, 1, Math.max(0, w - 2), Math.max(0, w - 1)])) add(x, y);
  }
  if (total === 0) for (let y = 0; y < h; y += 4) for (let x = 0; x < w; x += 4) add(x, y);
  if (total === 0) return [255, 255, 255];
  return counts.map((channel) => {
    let seen = 0;
    for (let v = 0; v < 256; v++) {
      seen += channel[v];
      if (seen * 2 >= total) return v;
    }
    return 255;
  }) as [number, number, number];
}

export function hexOf([r, g, b]: [number, number, number]): string {
  return `#${[r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("")}`.toUpperCase();
}

function cssOf([r, g, b]: [number, number, number], space: ColorSpace): string {
  if (space === "display-p3") return `color(display-p3 ${(r / 255).toFixed(4)} ${(g / 255).toFixed(4)} ${(b / 255).toFixed(4)})`;
  return `rgb(${r} ${g} ${b})`;
}

// ------------------------------------------------------------------------------ the drawing

export type Beautified = {
  /** What the style goes around: the capture, the window's own image or the editor's crop. */
  image: CanvasImageSource | null;
  /** The part of `image` to draw, in its pixels. */
  from: { x: number; y: number; width: number; height: number };
  /** Image pixels per point, which turn the style's points into pixels. */
  scale: number;
  style: BeautifyStyle;
  /** A window keeps its own corner and shadow and gets only the padding and the background. */
  window: boolean;
  /** The colour space of the canvas and of `match`. */
  space: ColorSpace;
  /** The Match colour, in the image's own values. */
  match: [number, number, number] | null;
  /** The image background's preview, or `null` when it is missing (the fallback gradient shows). */
  backgroundImage: CanvasImageSource | null;
  /** A JPEG output fills the transparent parts with white, as the encoder will. */
  jpeg: boolean;
  /** Leaves the image out, for a canvas under an image shown by something else (the editor's
   * surface): the background still lies under it, where a transparent image shows it. */
  ringOnly?: boolean;
  /** A window's own corner, in image pixels: the overlay's preview has only the frozen screen
   * behind the window, so it rounds the window as its own image will be. */
  imageRadius?: number;
};

/** The measures of a beautified image in its pixels. */
export function layoutOf(b: Pick<Beautified, "from" | "scale" | "style" | "window">) {
  const s = clamped(b.style);
  const k = Number.isFinite(b.scale) && b.scale > 0 ? b.scale : 1;
  const pad = Math.round(s.padding * k);
  const bar = !b.window && s.frame !== "none" ? Math.round(FRAME_BAR * k) : 0;
  const w = b.from.width;
  const h = b.from.height;
  const radius = b.window ? 0 : Math.min(s.corner * k, Math.min(w, h + bar) / 2);
  return { pad, bar, radius, k, width: w + 2 * pad, height: h + bar + 2 * pad, content: { x: pad, y: pad, width: w, height: h + bar } };
}

function roundedRect(ctx: CanvasRenderingContext2D, x: number, y: number, w: number, h: number, r: number): void {
  if (r <= 0) ctx.rect(x, y, w, h);
  else ctx.roundRect(x, y, w, h, r);
}

/** Draws the beautified image with its top-left corner at the canvas's origin, in image pixels
 * under the canvas's current transform. */
export function drawBeautified(ctx: CanvasRenderingContext2D, b: Beautified): void {
  const L = layoutOf(b);
  const s = clamped(b.style);
  const { width: W, height: H, content: c } = L;

  // The background.
  ctx.save();
  const bg = s.background;
  if (bg.kind === "image" && b.backgroundImage) {
    const iw = (b.backgroundImage as HTMLImageElement).naturalWidth || (b.backgroundImage as { width: number }).width;
    const ih = (b.backgroundImage as HTMLImageElement).naturalHeight || (b.backgroundImage as { height: number }).height;
    const k = Math.max(W / iw, H / ih);
    const sw = W / k;
    const sh = H / k;
    ctx.drawImage(b.backgroundImage, (iw - sw) / 2, (ih - sh) / 2, sw, sh, 0, 0, W, H);
  } else {
    const stops = bg.kind === "gradient" ? bg.stops : bg.kind === "image" ? bg.fallback : null;
    if (stops && stops.length > 1) {
      // CSS's 135°: the stops lie on the line through the centre whose ends touch the corners.
      const g = ctx.createLinearGradient((W - H) / 4, (H - W) / 4, (3 * W + H) / 4, (3 * H + W) / 4);
      stops.forEach((stop, i) => g.addColorStop(i / (stops.length - 1), stop));
      ctx.fillStyle = g;
    } else if (stops) ctx.fillStyle = stops[0] ?? "#000";
    else if (bg.kind === "solid") ctx.fillStyle = bg.color;
    else if (bg.kind === "match") ctx.fillStyle = cssOf(b.match ?? [255, 255, 255], b.space);
    else ctx.fillStyle = b.jpeg ? "#ffffff" : checker(ctx);
    ctx.fillRect(0, 0, W, H);
  }
  ctx.restore();

  // The shadow, outside the content only, as CSS draws a box shadow.
  const geometry = b.window || s.shadow === "none" ? null : SHADOW_GEOMETRY[s.shadow];
  if (geometry) {
    const zoom = Math.hypot(ctx.getTransform().a, ctx.getTransform().b) || 1;
    ctx.save();
    ctx.beginPath();
    ctx.rect(0, 0, W, H);
    roundedRect(ctx, c.x, c.y, c.width, c.height, L.radius);
    ctx.clip("evenodd");
    ctx.shadowColor = `rgba(0, 0, 0, ${geometry.opacity})`;
    // The shadow's measures are in canvas pixels, which the transform does not reach.
    ctx.shadowBlur = geometry.blur * L.k * zoom;
    ctx.shadowOffsetY = geometry.dy * L.k * zoom;
    ctx.fillStyle = "#000";
    ctx.beginPath();
    roundedRect(ctx, c.x, c.y, c.width, c.height, L.radius);
    ctx.fill();
    ctx.restore();
  }

  // The frame and the image, clipped to the rounded content.
  ctx.save();
  ctx.beginPath();
  roundedRect(ctx, c.x, c.y, c.width, c.height, L.radius);
  ctx.clip();
  if (L.bar) {
    const dark = s.frame === "dark";
    ctx.fillStyle = dark ? "#2b2c35" : "#ececf0";
    ctx.fillRect(c.x, c.y, c.width, L.bar);
    ["#ff5f57", "#febc2e", "#28c840"].forEach((color, i) => {
      ctx.fillStyle = color;
      ctx.beginPath();
      ctx.arc(c.x + (16 + 17 * i) * L.k, c.y + 14 * L.k, 5.5 * L.k, 0, Math.PI * 2);
      ctx.fill();
    });
  }
  if (!b.ringOnly && b.image) {
    if (b.window && b.jpeg) {
      // A JPEG has no alpha: the window's transparent shadow lies over white in the file.
      ctx.fillStyle = "#ffffff";
      ctx.fillRect(c.x, c.y + L.bar, b.from.width, b.from.height);
    }
    if (b.imageRadius) {
      ctx.beginPath();
      ctx.roundRect(c.x, c.y + L.bar, b.from.width, b.from.height, b.imageRadius);
      ctx.clip();
    }
    ctx.drawImage(b.image, b.from.x, b.from.y, b.from.width, b.from.height, c.x, c.y + L.bar, b.from.width, b.from.height);
  }
  ctx.restore();
}

/** The checkerboard that stands for transparency in a preview. */
function checker(ctx: CanvasRenderingContext2D): CanvasPattern | string {
  const tile = document.createElement("canvas");
  tile.width = 16;
  tile.height = 16;
  const t2 = tile.getContext("2d");
  if (!t2) return "#ffffff";
  t2.fillStyle = "#ffffff";
  t2.fillRect(0, 0, 16, 16);
  t2.fillStyle = "#d6d6dc";
  t2.fillRect(0, 0, 8, 8);
  t2.fillRect(8, 8, 8, 8);
  const zoom = Math.hypot(ctx.getTransform().a, ctx.getTransform().b) || 1;
  const pattern = ctx.createPattern(tile, "repeat");
  // The squares stay 8 canvas pixels wide whatever the preview's scale.
  pattern?.setTransform(new DOMMatrix().scale(1 / zoom));
  return pattern ?? "#ffffff";
}

/** Whether a JPEG would lose something of the style: a transparent background, or the
 * transparent ring a rounded corner leaves around the image. */
export function jpegLoses(style: BeautifyStyle): boolean {
  return style.background.kind === "transparent";
}

// ------------------------------------------------------------------------ the presets' file

/** A new preset id that no preset of `list` has. */
function freshId(list: unknown[]): string {
  const taken = new Set(list.map((e) => (typeof e === "object" && e !== null ? (e as { id?: unknown }).id : null)));
  for (let n = 1; ; n++) {
    const id = `preset-${Date.now().toString(36)}-${n}`;
    if (!taken.has(id)) return id;
  }
}

/** Changes the presets of `settings` (a copy is made) with `change`, which works on the list as
 * the file has it, unreadable entries included; `auto` and `preset` may change as well. */
export function changedPresets(settings: Settings, change: (list: unknown[], beautify: Beautify) => void): Settings {
  const beautify: Beautify = { ...settings.beautify, presets: Array.isArray(settings.beautify?.presets) ? [...settings.beautify.presets] : [] };
  change(beautify.presets, beautify);
  return { ...settings, beautify };
}

const indexIn = (list: unknown[], id: string) =>
  list.findIndex((e) => typeof e === "object" && e !== null && (e as { id?: unknown }).id === id);

/** Writes `style`'s values into preset `id`; its id, name and the fields a newer version wrote stay. */
export function withPresetStyle(settings: Settings, id: string, style: BeautifyStyle): Settings {
  return changedPresets(settings, (list) => {
    const at = indexIn(list, id);
    if (at >= 0) list[at] = { ...(list[at] as object), style: writeBeautify({ ...style, preset: null }) };
  });
}

/** Adds a preset named `name` with `style`'s values at the end of the list; returns the settings
 * and the new id. */
export function withNewPreset(settings: Settings, name: string, style: BeautifyStyle): { settings: Settings; id: string } {
  let id = "";
  const next = changedPresets(settings, (list) => {
    id = freshId(list);
    list.push({ id, name, style: writeBeautify({ ...style, preset: null }) });
  });
  return { settings: next, id };
}

export function withRenamedPreset(settings: Settings, id: string, name: string): Settings {
  return changedPresets(settings, (list) => {
    const at = indexIn(list, id);
    if (at >= 0) list[at] = { ...(list[at] as object), name };
  });
}

/** Puts a copy of preset `id` right after it, named `name`; returns the settings and its id. */
export function withDuplicatedPreset(settings: Settings, id: string, name: string): { settings: Settings; id: string } {
  let copy = "";
  const next = changedPresets(settings, (list) => {
    const at = indexIn(list, id);
    if (at < 0) return;
    copy = freshId(list);
    list.splice(at + 1, 0, { ...(list[at] as object), id: copy, name });
  });
  return { settings: next, id: copy };
}

/** Deletes preset `id`; when it was the automatic one, the first readable preset left takes its
 * place. The last readable preset stays. */
export function withoutPreset(settings: Settings, id: string): Settings {
  return changedPresets(settings, (list, beautify) => {
    const at = indexIn(list, id);
    if (at < 0) return;
    const readable = readPresets(beautify);
    if (readable.length <= 1) return;
    list.splice(at, 1);
    if (beautify.preset === id) beautify.preset = readable.find((p) => p.id !== id)?.id ?? DEFAULT_PRESET;
  });
}

/** A name for a new preset that no preset has yet. */
export function suggestedName(presets: Preset[]): string {
  const names = new Set(presets.map(presetName));
  for (let n = presets.length + 1; ; n++) {
    const name = t("beautify.newName", { n });
    if (!names.has(name)) return name;
  }
}

// ----------------------------------------------------------------- the background's preview

/** The preview of an image background, loaded from `frames.rs`; `missing` turns true when the file
 * cannot be read, and the style's fallback gradient shows instead. */
export function useBackgroundImage(path: () => string | null, side: number) {
  const image = shallowRef<HTMLImageElement | null>(null);
  const missing = ref(false);
  let wanted: string | null = null;
  watch(
    path,
    (p) => {
      wanted = p;
      image.value = null;
      missing.value = false;
      if (!p) return;
      const img = new Image();
      img.decoding = "async";
      img.onload = () => {
        if (wanted === p) image.value = img;
      };
      img.onerror = () => {
        if (wanted === p) missing.value = true;
      };
      img.src = backgroundUrl(p, side);
    },
    { immediate: true },
  );
  return { image, missing };
}
