<script setup lang="ts">
// The toast window: a stack at the bottom-right corner of the visible area of the display the
// capture was taken on (above the Dock), the newest at the bottom. A toast is one row (a short
// title, one line of detail, icon buttons named in their tips), and a warning or a failure grows
// into whole sentences with its main action as a labelled button under them. Capture toasts come
// from `toast://show`; upload toasts follow `upload://progress`, `upload://done` and
// `upload://failed`; an export toast follows `record://export-progress` while the trim window
// that started it is closed, since the window shows its own progress otherwise. A success leaves
// on its own after a few seconds (the timers pause while the pointer rests on the stack), a
// failure stays until it is dismissed, and every toast has a dismiss button.
//
// A toast rises from under its place and fades in, lifts and fades out when it leaves, and the
// others glide into their new places. The page moves the toasts itself, frame by frame, and
// reports each frame with `toast_fit`: the window stays on screen at one size that leaves the
// toasts' shadow room on every side, invisible while the stack is empty, and on macOS the native
// blur under each toast (an NSVisualEffectView per toast, `macos.rs`) takes the toast's place
// and opacity, so the blur moves with it; the page draws only the tint, the edges and the
// shadow. The panel never becomes key, so WebKit reports a click in it but no pointer move: the
// page asks the core where the pointer is (`toast_pointer`).
import { nextTick, onMounted, onUnmounted, reactive, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import ProgressRing from "../components/ProgressRing.vue";
import TipWrap from "../components/TipWrap.vue";
import { Window } from "@tauri-apps/api/window";

import { onWindow, type ErrorCode } from "../lib/api/core";
import { history_get, thumbUrl } from "../lib/api/history";
import { output_reveal } from "../lib/api/output";
import { permissions_open_settings } from "../lib/api/permissions";
import { onRecordExportProgress, record_export_cancel, type ExportProgress } from "../lib/api/record";
import {
  onUploadDone,
  onUploadFailed,
  onUploadProgress,
  upload_cancel,
  upload_file,
  upload_providers,
  type UploadDone,
  type UploadFailed,
  type UploadProgress,
} from "../lib/api/upload";
import type { LogicalPoint } from "../lib/api/capture";
import {
  app_copy_text,
  toast_fit,
  toast_pointer,
  window_open_settings,
  type Toast,
  type ToastFrame,
  type ToastGlass,
} from "../lib/api/windows";
import { TOAST_SHOW } from "../lib/events";
import { language, t } from "../lib/i18n";
import { doneText, failureText as uploadFailure } from "../upload/text";
import { fixText, reasonText } from "./reason";

/** How long a toast that reports success stays. */
const STAY_MS = 4000;
/** A save that went to the fallback folder stays longer, so the broken folder is noticed. */
const STAY_WARN_MS = 8000;
/** An uploaded link stays a little longer, so there is time to copy it again. */
const STAY_LINK_MS = 6000;
/** A toast rises this far from under its place as it comes, and lifts this far above it as it
 * fades out. */
const RISE = 28;
const LIFT = 18;
/** How long a toast takes to rise into place and to fade in, to lift and to fade out, and to
 * glide to a new place in the stack. The glide that closes a leaving toast's gap waits a little,
 * so the leaving one is mostly gone before the toast above comes down. */
const ENTER_MS = 480;
const FADE_IN_MS = 320;
const LEAVE_MS = 420;
const FADE_OUT_MS = 380;
const MOVE_MS = 440;
const CLOSE_WAIT_MS = 120;
/** How long a new toast waits for its picture to be decoded before it rises anyway: a picture
 * decoded mid-rise would stall the frames and pop in. */
const DECODE_WAIT_MS = 200;
/** How often the page asks where the pointer is while toasts are on screen. */
const POINTER_MS = 100;
/** How long the pointer rests on an icon button before its tip shows (the kit's tip delay). */
const TIP_DELAY_MS = 500;
/** The stack's distance to the bottom-right corner of the display's visible area. */
const GAP_RIGHT = 16;
const GAP_BELOW = 12;
/** The room the toast's shadow (`--shadow-float`: 32 px of blur, 12 px down) takes beside and
 * under a toast. The window reaches that far past the stack, past the visible area on the right
 * and over the Dock at the bottom, so the shadow is never cut at the window's edge; the core lets
 * clicks through the window everywhere but on a toast. */
const SHADOW_SIDE = 32;
const SHADOW_BELOW = 44;
/** How many characters of a file name or a link a one-row toast shows before it cuts the middle. */
const NAME_CHARS = 25;
/** How many words of copied text the toast quotes. */
const QUOTE_WORDS = 10;
/** An export that stops reporting for this long ended without a last report (a failure or a
 * cancel, which their own toasts or windows tell about). */
const EXPORT_QUIET_MS = 5000;
/** The trim window's label (`src/recorder/App.vue`). */
const TRIM_WINDOW = "recorder";

type UploadPhase = "uploading" | "done" | "failed";

type Upload = {
  uploadId: string;
  provider: string;
  phase: UploadPhase;
  sentBytes: number;
  totalBytes: number;
  done: UploadDone | null;
  failed: UploadFailed | null;
  thumb: string | null;
  copiedNow: boolean;
};

type Export = { recordingId: string; format: "mp4" | "gif"; fraction: number; remainingMs: number | null };

type Item = {
  key: string;
  /** It is on its way out and no longer counts as shown. */
  leaving: boolean;
  capture: Toast | null;
  upload: Upload | null;
  export?: Export | null;
};

/** macOS draws the blur under each toast natively; the page then drops its own backdrop filter. */
const NATIVE_GLASS = /Mac/.test(navigator.platform || navigator.userAgent);
if (NATIVE_GLASS) document.documentElement.dataset.glass = "native";

const items = ref<Item[]>([]);
const stack = ref<HTMLElement | null>(null);
const providerNames = reactive(new Map<string, string>());

// -------------------------------------------------------------------------------- motion

/** A timing curve as CSS writes it, `cubic-bezier(x1, y1, x2, y2)`: it finds the curve's
 * parameter for the time `t` by halving, then returns the progress there. */
function bezier(x1: number, y1: number, x2: number, y2: number): (t: number) => number {
  const curve = (s: number, a: number, b: number) => 3 * (1 - s) ** 2 * s * a + 3 * (1 - s) * s * s * b + s ** 3;
  return (t) => {
    if (t <= 0) return 0;
    if (t >= 1) return 1;
    let low = 0;
    let high = 1;
    let s = t;
    for (let i = 0; i < 24; i++) {
      s = (low + high) / 2;
      if (curve(s, x1, x2) < t) low = s;
      else high = s;
    }
    return curve(s, y1, y2);
  };
}

/** A toast that rises, lifts or glides starts gently and settles long (the curve of iOS sheets).
 * It fades in along an ease-out and fades out evenly, so its last steps to nothing are as small
 * as its first: an S-curve would hold it, then drop it at once. */
const ARRIVE = bezier(0.32, 0.72, 0, 1);
const FADE_IN = bezier(0, 0, 0.58, 1);
const FADE_OUT = (t: number) => t;

/** One moving number, from `from` to `to` over `ms` along `ease`, `wait` ms after it is let go.
 * `start` is a frame's time; -1 lets it go at the next frame, so a toast that arrives while
 * nothing draws starts once frames come, and -2 holds it until the toast's picture is decoded. */
type Tween = { from: number; to: number; start: number; ms: number; wait: number; ease: (t: number) => number };

/** How a toast is drawn: `y` points under its place (it rises from there, glides back there when
 * the stack moves, and lifts above it when it leaves), and its opacity. A toast that leaves is out
 * of the stack's flow and is not moved by the others. */
type Motion = { y: Tween; alpha: Tween; radius: number; leaving: boolean };

const LET_GO = -1;
const HELD = -2;

const tween = (from: number, to: number, ms: number, ease: (t: number) => number, wait = 0): Tween => ({
  from,
  to,
  start: LET_GO,
  ms,
  wait,
  ease,
});

const still = (value: number): Tween => ({ from: value, to: value, start: 0, ms: 0, wait: 0, ease: ARRIVE });

function valueAt(t: Tween, now: number): number {
  if (t.start < 0) return t.from;
  if (t.ms <= 0 || now >= t.start + t.ms) return t.to;
  return t.from + (t.to - t.from) * t.ease(Math.max(0, now - t.start) / t.ms);
}

const running = (t: Tween, now: number) => t.start < 0 || (t.ms > 0 && now < t.start + t.ms);
const moving = (motion: Motion, now: number) => running(motion.y, now) || running(motion.alpha, now);

/** Reduced motion keeps the fades and drops the rises, the lifts and the glides. */
const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)");

/** Where a toast sits in the window's layout (transforms left out), measured when the stack
 * changes, so a frame reads no layout: from the window's right edge, from the stack's bottom edge
 * (its place), and its size. */
type Layout = { right: number; place: number; width: number; height: number };

const motions = new Map<string, Motion>();
const layouts = new Map<string, Layout>();

function toastElements(): HTMLElement[] {
  return stack.value ? [...stack.value.querySelectorAll<HTMLElement>(":scope > .g-toast")] : [];
}

const elementOf = (key: string) => toastElements().find((el) => el.dataset.key === key);

/** The toast's place in the stack's layout (transforms left out), from the stack's bottom edge. */
const placeOf = (el: HTMLElement) => stack.value!.offsetHeight - (el.offsetTop + el.offsetHeight);

/** A new toast's motion: it rises and fades in once its picture is decoded (or after a short
 * wait), so no decoding stalls its frames. */
function arrival(el: HTMLElement): Motion {
  const motion: Motion = {
    y: tween(reducedMotion.matches ? 0 : RISE, 0, ENTER_MS, ARRIVE),
    alpha: tween(0, 1, FADE_IN_MS, FADE_IN),
    radius: parseFloat(getComputedStyle(el).borderTopLeftRadius) || 0,
    leaving: false,
  };
  const pictures = [...el.querySelectorAll("img")];
  if (pictures.length) {
    motion.y.start = HELD;
    motion.alpha.start = HELD;
    const decoded = Promise.allSettled(pictures.map((img) => img.decode()));
    void Promise.race([decoded, new Promise((done) => setTimeout(done, DECODE_WAIT_MS))]).then(() => {
      for (const t of [motion.y, motion.alpha]) if (t.start === HELD) t.start = LET_GO;
      animate();
    });
  }
  return motion;
}

/** Waits a leaving toast's glides: set by `dismiss`, used by the look that follows it. */
let closeWait = 0;

/** Looks at the stack after a change: a new toast starts to rise, and a toast whose place
 * changed (one under it left or grew, a new one came under it) glides there from where it is
 * drawn. */
function relayout(now: number): void {
  const box = stack.value;
  if (!box) return;
  const seen = new Set<string>();
  for (const el of toastElements()) {
    const key = el.dataset.key!;
    seen.add(key);
    const layout: Layout = {
      right: SHADOW_SIDE + box.offsetWidth - (el.offsetLeft + el.offsetWidth),
      place: placeOf(el),
      width: el.offsetWidth,
      height: el.offsetHeight,
    };
    const motion = motions.get(key);
    const was = layouts.get(key)?.place;
    if (!motion) {
      motions.set(key, arrival(el));
    } else if (!motion.leaving && was !== undefined && was !== layout.place) {
      // A toast that went up by d is drawn d lower (+y) at first, where it was.
      const y = valueAt(motion.y, now) + (layout.place - was);
      motion.y = reducedMotion.matches ? still(0) : tween(y, 0, MOVE_MS, ARRIVE, closeWait);
    }
    layouts.set(key, layout);
  }
  closeWait = 0;
  for (const key of [...motions.keys()]) {
    if (seen.has(key)) continue;
    motions.delete(key);
    layouts.delete(key);
  }
}

/** Draws every toast where its motion is at `now`. A moving toast gets a layer of its own, so a
 * frame only moves the layer; at rest it goes back to the page, where its text is sharpest. */
function paint(now: number): void {
  for (const el of toastElements()) {
    const motion = motions.get(el.dataset.key!);
    if (!motion) continue;
    const y = valueAt(motion.y, now);
    const alpha = valueAt(motion.alpha, now);
    el.style.transform = y ? `translateY(${y}px)` : "";
    el.style.opacity = alpha < 1 ? String(alpha) : "";
    const layer = moving(motion, now) ? "transform, opacity" : "";
    if (el.style.willChange !== layer) el.style.willChange = layer;
  }
}

/** A toast that comes again under a key that is still leaving (a retried upload, an export that
 * started again) starts afresh: its element may be the leaving one, reused. */
function forget(key: string): void {
  motions.delete(key);
  layouts.delete(key);
  const el = elementOf(key);
  if (el) for (const name of ["position", "right", "bottom", "transform", "opacity", "will-change"]) el.style.removeProperty(name);
}

let frameId = 0;
function animate(): void {
  if (!frameId) frameId = requestAnimationFrame(frame);
}

function frame(now: number): void {
  frameId = 0;
  let busy = false;
  for (const motion of motions.values()) {
    for (const t of [motion.y, motion.alpha]) if (t.start === LET_GO) t.start = now + t.wait;
    busy ||= moving(motion, now);
  }
  paint(now);
  report(now);
  // A toast that finished leaving goes out of the list; the others glide into its place.
  const gone = items.value.filter((item) => {
    const motion = motions.get(item.key);
    return item.leaving && (!motion || !moving(motion, now));
  });
  if (gone.length) drop(gone.map((item) => item.key));
  if (busy) animate();
}

// -------------------------------------------------------------------------------- the window

/** Where each toast is drawn at `now`, from the window's bottom-right corner, and how opaque it
 * is: the native blur under it copies both. */
function glass(now: number): ToastGlass[] {
  if (!NATIVE_GLASS) return [];
  const list: ToastGlass[] = [];
  for (const [key, motion] of motions) {
    const layout = layouts.get(key);
    if (!layout) continue;
    list.push({
      key,
      right: layout.right,
      bottom: SHADOW_BELOW + layout.place - valueAt(motion.y, now),
      width: layout.width,
      height: layout.height,
      radius: motion.radius,
      alpha: valueAt(motion.alpha, now),
    });
  }
  return list;
}

/** The toast's width (`--toast-w`), read once. */
let toastWidth = 0;

/** The window: the toast's width with the shadow's room on both sides, reaching past the visible
 * area by what the shadow needs beyond the stack's gap. It keeps this size and stays on screen,
 * invisible and letting every click through while no toast is shown, so its page keeps drawing
 * at full speed and the first toast rises as smoothly as the others. */
function windowFrame(shown: boolean): ToastFrame {
  toastWidth ||= parseFloat(getComputedStyle(document.documentElement).getPropertyValue("--toast-w")) || 300;
  return { width: toastWidth + 2 * SHADOW_SIDE, pastRight: SHADOW_SIDE - GAP_RIGHT, pastBelow: SHADOW_BELOW - GAP_BELOW, shown };
}

let lastSent = "";
let queued: [ToastFrame, ToastGlass[]] | null = null;
let sending = false;

/** Tells the core whether the window shows and where each toast is drawn. One call is on its way
 * at a time; the frames that pass meanwhile leave only the newest waiting. */
function report(now: number): void {
  const shown = items.value.length > 0;
  const frame = windowFrame(shown);
  const rects = shown ? glass(now) : [];
  const sent = JSON.stringify([frame, rects]);
  if (sent === lastSent) return;
  lastSent = sent;
  queued = [frame, rects];
  if (!sending) void flush();
}

async function flush(): Promise<void> {
  sending = true;
  while (queued) {
    const [frame, rects] = queued;
    queued = null;
    await toast_fit(frame, rects).catch((error) => console.warn("toast_fit", error));
  }
  sending = false;
}

/** After the list or a toast changed: looks at the stack, starts what moves, and draws. */
async function fit(): Promise<void> {
  await nextTick();
  const now = performance.now();
  relayout(now);
  paint(now);
  report(now);
  if ([...motions.values()].some((motion) => moving(motion, now))) animate();
  if (items.value.length) watchPointer();
  else unwatchPointer();
}

// --------------------------------------------------------------------------------- the pointer

/** The panel never becomes key, and WebKit follows the pointer only in a key window: it sees a
 * click, takes the clicked spot as hovered, and keeps it so until the next click. The page asks
 * the core where the pointer really is instead, while toasts are on screen; the answer pauses the
 * timers, lights the control under the pointer and shows its tip after the kit's delay, and the
 * stale hover draws nothing (the style at the end). */
let pointerTimer: ReturnType<typeof setInterval> | null = null;
let asking = false;
let lit: HTMLElement | null = null;
let tipWrap: HTMLElement | null = null;
let tipSince = 0;

function watchPointer(): void {
  if (!pointerTimer) pointerTimer = setInterval(() => void askPointer(), POINTER_MS);
}

function unwatchPointer(): void {
  if (pointerTimer) clearInterval(pointerTimer);
  pointerTimer = null;
  pointerAt(null);
}

async function askPointer(): Promise<void> {
  if (asking) return;
  asking = true;
  try {
    const point = await toast_pointer();
    if (pointerTimer) pointerAt(point);
  } catch {
    if (pointerTimer) pointerAt(null);
  } finally {
    asking = false;
  }
}

function pointerAt(point: LogicalPoint | null): void {
  const box = stack.value;
  const target = point && box ? document.elementFromPoint(point.x, point.y) : null;
  const over = target && box?.contains(target) ? target : null;
  if (over && !hovering) pause();
  else if (!over && hovering) resume();

  const control = over?.closest<HTMLElement>(".g-btn, .g-button") ?? null;
  if (control !== lit) {
    lit?.removeAttribute("data-state");
    control?.setAttribute("data-state", "hover");
    lit = control;
  }

  const wrap = over?.closest<HTMLElement>(".g-tipwrap") ?? null;
  if (wrap !== tipWrap) {
    tipWrap?.querySelector(":scope > .g-tip")?.removeAttribute("data-state");
    tipWrap = wrap;
    tipSince = performance.now();
  } else if (wrap && performance.now() - tipSince >= TIP_DELAY_MS) {
    wrap.querySelector(":scope > .g-tip")?.setAttribute("data-state", "show");
  }
}

// ---------------------------------------------------------------------------------- timers

type Timer = { remaining: number; startedAt: number; handle: ReturnType<typeof setTimeout> | null };
const timers = new Map<string, Timer>();
/** Whether the pointer rests on the stack, as the core reports it (`pointerAt`). */
let hovering = false;

function runTimer(key: string, timer: Timer): void {
  timer.startedAt = Date.now();
  timer.handle = setTimeout(() => dismiss(key), timer.remaining);
}

/** Lets a success toast leave after `ms`, unless the pointer rests on the stack. */
function arm(key: string, ms: number): void {
  clearTimer(key);
  const timer: Timer = { remaining: ms, startedAt: 0, handle: null };
  timers.set(key, timer);
  if (!hovering) runTimer(key, timer);
}

function clearTimer(key: string): void {
  const timer = timers.get(key);
  if (timer?.handle) clearTimeout(timer.handle);
  timers.delete(key);
}

function pause(): void {
  hovering = true;
  for (const timer of timers.values()) {
    if (!timer.handle) continue;
    clearTimeout(timer.handle);
    timer.handle = null;
    timer.remaining = Math.max(800, timer.remaining - (Date.now() - timer.startedAt));
  }
}

function resume(): void {
  hovering = false;
  for (const [key, timer] of timers) if (!timer.handle) runTimer(key, timer);
}

// ---------------------------------------------------------------------------------- the list

/** The toast under `key` that is shown, not one on its way out. */
function find(key: string): Item | undefined {
  return items.value.find((x) => x.key === key && !x.leaving);
}

/** Sends a toast out: it lifts and fades out, out of the stack's flow where it is drawn, and the
 * toasts above it glide down into its place once it is mostly gone. */
function dismiss(key: string): void {
  clearTimer(key);
  const item = find(key);
  if (!item) return;
  item.leaving = true;
  const el = elementOf(key);
  const motion = motions.get(key);
  if (!el || !motion) {
    drop([key]);
    return;
  }
  const place = placeOf(el);
  el.style.position = "absolute";
  el.style.right = "0";
  el.style.bottom = `${place}px`;
  const now = performance.now();
  motion.leaving = true;
  motion.y = tween(valueAt(motion.y, now), reducedMotion.matches ? 0 : -LIFT, LEAVE_MS, ARRIVE);
  motion.alpha = tween(valueAt(motion.alpha, now), 0, FADE_OUT_MS, FADE_OUT);
  closeWait = reducedMotion.matches ? 0 : CLOSE_WAIT_MS;
  void fit();
  // The frames take it out once it has faded; this covers a window that stopped drawing.
  setTimeout(() => drop([key]), Math.max(LEAVE_MS, FADE_OUT_MS) + 600);
}

/** Takes toasts that left out of the list. */
function drop(keys: string[]): void {
  const before = items.value.length;
  items.value = items.value.filter((x) => !(x.leaving && keys.includes(x.key)));
  if (items.value.length !== before) void fit();
}

function add(item: Item): void {
  // One still leaving under the same key gives way at once.
  if (items.value.some((x) => x.key === item.key)) {
    items.value = items.value.filter((x) => x.key !== item.key);
    forget(item.key);
  }
  // The stack grows upward from the bottom of the screen, so the newest goes last.
  items.value.push(item);
  void fit();
}

/** Toasts that stay until they are dismissed, because they ask for the user's attention. */
const STAYS: ReadonlySet<Toast["kind"]> = new Set(["failed", "diskFull"]);

function showCapture(toast: Toast): void {
  // An export's result (its saved file, or why it failed) takes the place of its progress.
  if (toast.kind === "failed" || toast.format === "mp4" || toast.format === "gif") {
    for (const item of items.value) if (item.export) dismiss(item.key);
  }
  const key = `t:${toast.id}`;
  add({ key, leaving: false, capture: toast, upload: null });
  if (!STAYS.has(toast.kind)) arm(key, toast.fallbackFrom ? STAY_WARN_MS : STAY_MS);
}

// ----------------------------------------------------------------------------------- exports

/** Whether the trim window is on screen, where it shows the export's progress itself. */
async function trimWindowShown(): Promise<boolean> {
  try {
    const trim = await Window.getByLabel(TRIM_WINDOW);
    return !!trim && (await trim.isVisible()) && !(await trim.isMinimized());
  } catch {
    return false;
  }
}

const exportQuiet = new Map<string, ReturnType<typeof setTimeout>>();

async function onExportProgress(p: ExportProgress): Promise<void> {
  const key = `e:${p.recordingId}`;
  let item = find(key);
  if (!item) {
    if (p.fraction >= 1) return;
    if (await trimWindowShown()) return;
    // A newer report may have added the toast while this one waited for the window.
    item = find(key);
    if (!item) {
      add({ key, leaving: false, capture: null, upload: null, export: { recordingId: p.recordingId, format: p.format, fraction: 0, remainingMs: null } });
      item = find(key)!;
    }
  }
  Object.assign(item.export!, { format: p.format, fraction: p.fraction, remainingMs: p.remainingMs });
  clearTimeout(exportQuiet.get(key));
  if (p.fraction >= 1) {
    exportQuiet.delete(key);
    dismiss(key);
    return;
  }
  exportQuiet.set(
    key,
    setTimeout(() => {
      exportQuiet.delete(key);
      dismiss(key);
    }, EXPORT_QUIET_MS),
  );
  void fit();
}

function cancelExport(item: Item): void {
  const id = item.export?.recordingId;
  if (!id) return;
  dismiss(item.key);
  record_export_cancel(id).catch((error) => console.warn("record_export_cancel", error));
}

function exportTitle(job: Export): string {
  return t(job.format === "gif" ? "toast.export.gif" : "toast.export.mp4");
}

function exportText(job: Export): string {
  const p = Math.round(Math.min(1, Math.max(0, job.fraction)) * 100);
  const ms = job.remainingMs;
  if (ms == null || ms < 2000) return t("toast.export.meta.percent", { p });
  const s = Math.ceil(ms / 1000);
  return s < 90 ? t("toast.export.meta.seconds", { p, n: s }) : t("toast.export.meta.minutes", { p, n: Math.round(s / 60) });
}

// ----------------------------------------------------------------------------------- uploads

let providersAsked = false;
async function learnProviders(): Promise<void> {
  if (providersAsked) return;
  providersAsked = true;
  try {
    for (const provider of await upload_providers()) providerNames.set(provider.id, provider.name);
  } catch {
    providersAsked = false;
  }
}

const hostName = (id: string) => providerNames.get(id) ?? id;

function uploadItem(uploadId: string, provider: string): Item {
  const key = `u:${uploadId}`;
  let item = find(key);
  if (!item) {
    clearTimer(key);
    item = {
      key,
      leaving: false,
      capture: null,
      upload: {
        uploadId,
        provider,
        phase: "uploading",
        sentBytes: 0,
        totalBytes: 0,
        done: null,
        failed: null,
        thumb: null,
        copiedNow: false,
      },
    };
    add(item);
    // The list's own (reactive) copy, so later changes reach the page.
    item = find(key)!;
  }
  return item;
}

function onProgress(progress: UploadProgress): void {
  void learnProviders();
  const item = uploadItem(progress.uploadId, progress.provider);
  const upload = item.upload!;
  upload.phase = "uploading";
  upload.sentBytes = progress.sentBytes;
  upload.totalBytes = progress.totalBytes;
  void fit();
}

async function onDone(done: UploadDone): Promise<void> {
  void learnProviders();
  const item = uploadItem(done.result.uploadId, done.result.provider);
  const upload = item.upload!;
  upload.phase = "done";
  // The title says what really reached the clipboard, which the core reports.
  upload.done = done;
  arm(item.key, STAY_LINK_MS);
  void fit();
  if (done.entryId) {
    try {
      upload.thumb = thumbUrl(await history_get(done.entryId));
    } catch {
      // The toast shows the upload icon instead of the capture.
    }
  }
}

function onFailed(failed: UploadFailed): void {
  const key = `u:${failed.uploadId}`;
  // The user cancelled it themselves, so there is nothing to tell.
  if (failed.kind === "cancelled") {
    dismiss(key);
    return;
  }
  void learnProviders();
  const item = uploadItem(failed.uploadId, failed.provider);
  const upload = item.upload!;
  clearTimer(item.key);
  upload.phase = "failed";
  upload.failed = failed;
  void fit();
}

/** Failures that end the same way however often they are retried: the settings or the file must
 * change first, and the sentence says how. */
const FINAL: ReadonlySet<string> = new Set(["fileTooLarge", "hostNotAllowed", "notConfirmed", "missingSecret", "invalidConfig"]);

/** Failures that Settings › Upload fixes, so the toast offers the way there. */
const SETTINGS_FIX: ReadonlySet<string> = new Set(["missingSecret", "invalidConfig"]);
const needsSettings = (upload: Upload) => !!upload.failed && SETTINGS_FIX.has(upload.failed.kind);

function openUploadSettings(item: Item): void {
  dismiss(item.key);
  window_open_settings("upload").catch((error) => console.warn("window_open_settings", error));
}

/** A retry is possible when the core said which file failed and the failure can pass. */
const canRetry = (upload: Upload) => !!upload.failed?.path && !FINAL.has(upload.failed.kind);

function retry(item: Item): void {
  const failed = item.upload!.failed;
  if (!failed?.path) return;
  dismiss(item.key);
  // The new upload's events bring its own toast.
  upload_file(failed.path, failed.provider, failed.entryId ?? null).catch((error) => console.warn("upload_file", error));
}

function cancel(upload: Upload): void {
  // The core answers with `upload://failed` (kind `cancelled`), which takes the toast away.
  upload_cancel(upload.uploadId).catch((error) => console.warn("upload_cancel", error));
}

async function copyLink(upload: Upload): Promise<void> {
  const url = upload.done?.result.url;
  if (!url) return;
  try {
    await app_copy_text(url);
    upload.copiedNow = true;
    setTimeout(() => (upload.copiedNow = false), 1500);
  } catch (error) {
    console.warn("app_copy_text", error);
  }
}

// ---------------------------------------------------------------------------------- wording

/** Whether the toast asks for attention: a warning or a failure wraps its sentences and puts its
 * main action under them. */
function tall(item: Item): boolean {
  const toast = item.capture;
  if (toast) return STAYS.has(toast.kind) || !!toast.fallbackFrom;
  return item.upload?.phase === "failed";
}

const title = (item: Item) => (tall(item) ? sentence(item) : shortTitle(item));

/** A one-row toast's title, short enough to share the row with two icon buttons. The whole
 * sentence stays in the title's tip. */
function shortTitle(item: Item): string {
  const toast = item.capture;
  if (toast) {
    if (toast.kind === "copied") return t("toast.short.copied");
    if (toast.kind === "saved") return t("toast.short.saved");
    if (toast.kind === "textCopied") {
      const n = lineCount(toast.text);
      return n === 1 ? t("toast.short.lines.one") : t("toast.short.lines.many", { n });
    }
    return sentence(item);
  }
  if (item.export) return exportTitle(item.export);
  const upload = item.upload!;
  if (upload.phase === "uploading") return t("toast.short.uploading");
  const done = upload.done;
  if (!done) return sentence(item);
  if (done.opened && !done.copied) return t("toast.short.opened");
  if (done.copied === "markdown") return t("toast.short.copiedMarkdown");
  if (done.copied === "link") return t("toast.short.copiedLink");
  return t("toast.short.uploaded");
}

/** The whole sentence: the title of a toast that asks for attention, and the tip of any other. */
function sentence(item: Item): string {
  const toast = item.capture;
  if (toast) {
    if (toast.kind === "copied") return t("toast.copied");
    if (toast.kind === "saved") return t("toast.saved", { folder: toast.folder ?? "" });
    if (toast.kind === "textCopied") {
      const n = lineCount(toast.text);
      return n === 1 ? t("toast.lines.one") : t("toast.lines.many", { n });
    }
    if (toast.kind === "diskFull") return t("toast.diskFull");
    return t("toast.failed");
  }
  if (item.export) return exportTitle(item.export);
  const upload = item.upload!;
  if (upload.phase === "uploading") return t("upload.state.uploading", { name: hostName(upload.provider) });
  if (upload.phase === "failed" || !upload.done) return t("toast.upload.failed");
  if (upload.done.opened && !upload.done.copied) return t("upload.state.opened");
  return doneText(upload.done);
}

/** "Saved to Pictures › Glimpsfy." with the folder that could not be reached named under it. */
function savedFallback(toast: Toast): string | null {
  return toast.fallbackFrom ? t("toast.savedFallback", { folder: fileName(toast.fallbackFrom) || toast.fallbackFrom }) : null;
}

function lineCount(text: string | null): number {
  if (!text) return 1;
  return text.split(/\r?\n/).filter((line) => line.trim()).length || 1;
}

/** The first words of copied text, its lines joined with a middle dot. */
function quote(text: string | null): string {
  const words = (text ?? "").replace(/\s*\n\s*/g, " · ").split(/\s+/).filter(Boolean);
  const head = words.slice(0, QUOTE_WORDS).join(" ");
  return t("toast.quote", { text: words.length > QUOTE_WORDS ? `${head}…` : head });
}

const number = (n: number, digits = 0) =>
  new Intl.NumberFormat(language.value, { maximumFractionDigits: digits, minimumFractionDigits: digits }).format(n);

function bytes(n: number): string {
  if (n < 1000) return `${n} B`;
  if (n < 1_000_000) return `${number(n / 1000)} KB`;
  return `${number(n / 1_000_000, 1)} MB`;
}

const FORMAT_NAMES = { png: "PNG", jpeg: "JPEG", webp: "WebP", mp4: "MP4", gif: "GIF" } as const;

function meta(toast: Toast): string {
  const parts = [];
  if (toast.width && toast.height) parts.push(`${toast.width} × ${toast.height}`);
  if (toast.format) parts.push(FORMAT_NAMES[toast.format] ?? toast.format.toUpperCase());
  if (toast.bytes) parts.push(bytes(toast.bytes));
  return parts.join(" · ");
}

function percent(upload: Upload): number | null {
  return upload.totalBytes > 0 ? Math.min(100, (upload.sentBytes / upload.totalBytes) * 100) : null;
}

function progressText(upload: Upload): string {
  const p = percent(upload);
  if (p === null) return t("toast.upload.starting");
  return t("toast.upload.progress", { p: Math.round(p), sent: bytes(upload.sentBytes), total: bytes(upload.totalBytes) });
}

/** The failure's reason and what to do, as one or two sentences. */
function failureText(code: ErrorCode | null): string {
  const fix = fixText(code);
  return fix ? `${reasonText(code)} ${fix}` : reasonText(code);
}

/** What went wrong and what to do, in the upload area's sentences. */
function uploadFailureText(upload: Upload): string {
  return upload.failed ? uploadFailure(upload.failed, hostName(upload.provider)) : t("upload.failed.other");
}

/** The host's own reason (English, secrets cut out), which the sentence above points to. */
function hostReason(upload: Upload): string | null {
  return upload.failed?.kind === "provider" && upload.failed.error.message ? upload.failed.error.message : null;
}

const fileName = (path: string | null) => (path ? (path.split(/[\\/]/).pop() ?? path) : "");

/** A file name or a link cut in the middle, so its end (the extension, the file's id) stays. */
function middle(text: string, max = NAME_CHARS): string {
  if (text.length <= max) return text;
  const head = Math.ceil((max - 1) * 0.55);
  return `${text.slice(0, head)}…${text.slice(text.length - (max - 1 - head))}`;
}

// ----------------------------------------------------------------------------------- actions

type Action = { key: string; label: string; icon?: string; primary?: boolean; run: () => void };

function reveal(path: string): void {
  output_reveal(path).catch((error) => console.warn("output_reveal", error));
}

/** The icon buttons on a toast's row, the close button last; their names show in their tips. */
function icons(item: Item): Action[] {
  const list: Action[] = [];
  const toast = item.capture;
  if (toast) {
    if (toast.kind === "saved" && toast.path) {
      const path = toast.path;
      list.push({ key: "reveal", icon: "folder", label: t("toast.showInFinder"), run: () => reveal(path) });
    }
  } else if (item.export) {
    list.push({ key: "cancel", icon: "stop", label: t("toast.upload.cancel"), run: () => cancelExport(item) });
  } else if (item.upload) {
    const upload = item.upload;
    if (upload.phase === "uploading") {
      list.push({ key: "cancel", icon: "stop", label: t("upload.state.cancel"), run: () => cancel(upload) });
    } else if (upload.phase === "done") {
      const label = upload.copiedNow ? t("toast.upload.copiedNow") : t("toast.upload.copy");
      list.push({ key: "copy", icon: upload.copiedNow ? "check" : "copy", label, run: () => void copyLink(upload) });
    }
  }
  list.push({ key: "close", icon: "close", label: t("toast.dismiss"), run: () => dismiss(item.key) });
  return list;
}

/** The labelled buttons under a toast that asks for attention. */
function foot(item: Item): Action[] {
  const toast = item.capture;
  if (toast) {
    if (toast.kind === "diskFull" && toast.path) {
      const path = toast.path;
      return [{ key: "reveal", label: t("toast.showInFinder"), run: () => reveal(path) }];
    }
    if (toast.kind === "failed" && toast.errorCode === "permission_denied") {
      const run = () => void permissions_open_settings("screenRecording").catch((error) => console.warn("permissions_open_settings", error));
      return [{ key: "settings", label: t("toast.openSettings"), primary: true, run }];
    }
    return [];
  }
  const upload = item.upload;
  if (upload?.phase !== "failed") return [];
  if (needsSettings(upload)) return [{ key: "settings", label: t("toast.upload.settings"), primary: true, run: () => openUploadSettings(item) }];
  if (canRetry(upload)) return [{ key: "retry", label: t("toast.upload.retry"), primary: true, run: () => retry(item) }];
  return [];
}

function status(item: Item): "ok" | "warn" | "progress" {
  if (item.export) return "progress";
  if (item.capture) return STAYS.has(item.capture.kind) || item.capture.fallbackFrom ? "warn" : "ok";
  if (item.upload!.phase === "uploading") return "progress";
  return item.upload!.phase === "failed" ? "warn" : "ok";
}

function thumbOf(item: Item): string | null {
  if (item.export) return null;
  return item.capture ? item.capture.thumbnail : item.upload!.thumb;
}

function placeholderIcon(item: Item): string {
  if (item.capture?.kind === "textCopied") return "ocr";
  if (item.export) return "record";
  return item.upload ? "upload" : "image";
}

function ringProgress(item: Item): number | null {
  if (item.export) return Math.min(100, Math.max(0, item.export.fraction * 100));
  return percent(item.upload!);
}

// ------------------------------------------------------------------------------------ wiring

const stops: (() => void)[] = [];
let observer: ResizeObserver | null = null;

onMounted(async () => {
  observer = new ResizeObserver(() => void fit());
  if (stack.value) observer.observe(stack.value);
  stops.push(
    await onWindow<Toast>(TOAST_SHOW, showCapture),
    await onUploadProgress(onProgress),
    await onUploadDone((done) => void onDone(done)),
    await onUploadFailed(onFailed),
    await onRecordExportProgress((p) => void onExportProgress(p)),
  );
  // Puts the empty window on screen, invisible, so the first toast finds its page awake.
  report(performance.now());
});

onUnmounted(() => {
  observer?.disconnect();
  stops.forEach((stop) => stop());
  for (const key of [...timers.keys()]) clearTimer(key);
  for (const handle of exportQuiet.values()) clearTimeout(handle);
  unwatchPointer();
  cancelAnimationFrame(frameId);
});
</script>

<template>
  <div ref="stack" class="g-toasts stack" :style="{ right: `${SHADOW_SIDE}px`, bottom: `${SHADOW_BELOW}px` }">
    <div
      v-for="item in items"
      :key="item.key"
      class="g-toast"
      :data-key="item.key"
      :role="status(item) === 'warn' ? 'alert' : 'status'"
      :data-tall="tall(item) || undefined"
    >
      <div class="g-toast-thumb">
        <img v-if="thumbOf(item)" :src="thumbOf(item)!" alt="" draggable="false" />
        <span v-else class="thumb-icon"><GIcon :name="placeholderIcon(item)" :size="18" /></span>
        <span v-if="status(item) === 'progress'" class="g-status" data-status="progress">
          <ProgressRing :progress="ringProgress(item)" :label="t(item.export ? 'toast.export.ring' : 'toast.upload.ring')" :size="22" />
        </span>
        <span v-else class="g-status" :data-status="status(item)">
          <GIcon :name="status(item) === 'warn' ? 'bang' : item.upload ? 'link' : 'check'" :size="12" />
        </span>
      </div>

      <div class="g-toast-text">
        <div class="g-toast-title" :title="sentence(item)">{{ title(item) }}</div>

        <!-- capture toasts -->
        <template v-if="item.capture">
          <template v-if="item.capture.kind === 'saved' || item.capture.kind === 'diskFull'">
            <div class="g-toast-body" data-kind="mono" :title="item.capture.path ?? undefined">
              {{ middle(fileName(item.capture.path)) }}
            </div>
            <div v-if="item.capture.kind === 'diskFull'" class="g-toast-body" data-kind="wrap">
              {{ t("toast.diskFullSaved", { folder: item.capture.folder ?? "" }) }}
            </div>
            <div v-else-if="savedFallback(item.capture)" class="g-toast-body" data-kind="wrap" :title="item.capture.fallbackFrom ?? undefined">
              {{ savedFallback(item.capture) }}
            </div>
          </template>
          <div v-else-if="item.capture.kind === 'copied'" class="g-toast-body">{{ meta(item.capture) }}</div>
          <div v-else-if="item.capture.kind === 'textCopied'" class="g-toast-body" :title="item.capture.text ?? undefined">
            {{ quote(item.capture.text) }}
          </div>
          <div v-else class="g-toast-body" data-kind="wrap" :title="item.capture.message ?? undefined">
            {{ failureText(item.capture.errorCode) }}
          </div>
        </template>

        <!-- export toasts, while the trim window is closed -->
        <div v-else-if="item.export" class="g-toast-body">{{ exportText(item.export) }}</div>

        <!-- upload toasts -->
        <template v-else-if="item.upload">
          <div v-if="item.upload.phase === 'uploading'" class="g-toast-body">{{ progressText(item.upload) }}</div>
          <div v-else-if="item.upload.phase === 'done'" class="g-toast-body" data-kind="link" :title="item.upload.done?.result.url">
            {{ middle(item.upload.done?.result.url ?? "") }}
          </div>
          <template v-else>
            <div class="g-toast-body" data-kind="wrap">{{ uploadFailureText(item.upload) }}</div>
            <div v-if="hostReason(item.upload)" class="g-toast-body" data-kind="mono" :title="hostReason(item.upload) ?? undefined">
              {{ hostReason(item.upload) }}
            </div>
          </template>
        </template>
      </div>

      <div class="g-toast-actions">
        <!-- The tips open to the left: the window leaves no room above or below the stack. -->
        <TipWrap v-for="action in icons(item)" :key="action.key" :tip="action.label" side="left">
          <button type="button" class="g-btn" data-size="small" :aria-label="action.label" @click="action.run">
            <GIcon :name="action.icon!" :size="16" />
          </button>
        </TipWrap>
      </div>

      <div v-if="foot(item).length" class="g-toast-foot">
        <button
          v-for="action in foot(item)"
          :key="action.key"
          type="button"
          class="g-button"
          :data-variant="action.primary ? 'primary' : undefined"
          @click="action.run"
        >
          {{ action.label }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
/* The stack keeps the toast's width even with no toast in its flow: a leaving toast is taken out
   of the flow, and when it was the last one a shrink-to-fit stack fell to 0 px wide, and the
   toast's max-width: 100% squeezed it from 300 px to its padding as it faded out. */
.stack {
  position: absolute;
  width: var(--toast-w);
}
.thumb-icon {
  display: grid;
  place-items: center;
  background: var(--glass-solid);
  color: var(--iris-text);
}
/* WebKit's own hover in this panel is the spot of the last click and stays there (see "the
   pointer" in the script), so it draws nothing; the page lights the control under the pointer
   with data-state and shows its tip with the tip's data-state instead. */
.stack :deep(.g-btn:hover:not([data-state="hover"], :active)) {
  background: none;
}
.stack :deep(.g-button:hover:not([data-state="hover"], :active)) {
  background: var(--surface-button);
}
.stack :deep(.g-button[data-variant="primary"]:hover:not([data-state="hover"], :active)) {
  background: var(--iris);
}
.stack :deep(.g-tipwrap:hover > .g-tip:not([data-state="show"])) {
  opacity: 0;
  visibility: hidden;
}
</style>
