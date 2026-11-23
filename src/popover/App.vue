<script setup lang="ts">
// The menu bar popover (DESIGN.md "Popover"): a strip of capture modes with a line that names the
// one under the pointer or the focus and its live shortcut, the one-shot delay at that line's end,
// a large preview of the picked capture over a strip of the five latest, the picked capture's
// actions (Copy and its own second action labelled, the rest as icons), and History, Settings and
// Quit. The window is a transparent native panel that is made once at launch and stays alive,
// so this page hears every event while it is hidden; `popover://opened` tells it to refresh.
// The page draws the panel itself (the kit's `g-popover` on its solid material, since a
// transparent window has nothing behind it to blur) and tells the window its height after each
// render (`popover_fit`).
//
// States: the screen recording permission missing (an amber note on top, rows disabled), a
// restart needed, a recording in progress (the rows become the recording bar), an upload in
// progress (a ring on the thumbnail, Upload becomes Cancel upload), an empty or unreadable
// history, and an update that waits for a restart (a quiet iris note above the footer).
// "Record screen" opens the recording's setup on an area (`record_setup`), as its shortcut does.
import { computed, nextTick, onMounted, onUnmounted, onUpdated, reactive, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import KeyCaps from "../components/KeyCaps.vue";
import ProgressRing from "../components/ProgressRing.vue";
import TipWrap from "../components/TipWrap.vue";
import Menu, { type MenuItem } from "../history/Menu.vue";
import { mediaPath } from "../history/model";
import { sendMenuItems } from "../history/send";
import {
  CAPTURE_COUNTDOWN,
  CAPTURE_INTERVAL,
  capture_cancel_delay,
  capture_delay,
  capture_interval,
  capture_set_delay,
  capture_start,
  capture_start_interval,
  capture_stop_interval,
  type CaptureMode,
  type DelayState,
  type IntervalState,
} from "../lib/api/capture";
import { on } from "../lib/api/core";
import {
  DEFAULT_HISTORY_QUERY,
  entryName,
  frameUrl,
  history_copy,
  history_get,
  history_list,
  history_open_in_editor,
  onHistoryChanged,
  thumbUrl,
  type Entry,
  type EntryId,
} from "../lib/api/history";
import { output_reveal } from "../lib/api/output";
import { permissions_open_settings, permissions_request, permissions_status, type Permissions } from "../lib/api/permissions";
import {
  onRecordState,
  onRecordTick,
  record_discard,
  record_open_trim,
  record_pause,
  record_resume,
  record_setup,
  record_state,
  record_stop,
  type RecordingState,
} from "../lib/api/record";
import { send_copy_path, send_list_targets, send_to_app } from "../lib/api/send";
import { settings_get } from "../lib/api/settings";
import { RELEASES_URL, update_install } from "../lib/api/update";
import {
  onUploadDone,
  onUploadFailed,
  onUploadProgress,
  upload_cancel,
  upload_file,
  upload_providers,
  type ConsentInfo,
} from "../lib/api/upload";
import {
  POPOVER_OPENED,
  window_open_settings,
  app_copy_text,
  app_open_url,
  app_quit,
  app_restart,
  pin_open,
  popover_drag_out,
  popover_fit,
  popover_hide,
  window_open,
  type WindowKind,
} from "../lib/api/windows";
import { PERMISSIONS_CHANGED, SETTINGS_CHANGED } from "../lib/events";
import { language, t, type Key } from "../lib/i18n";
import { capsOf, capsText, type ShortcutAction } from "../lib/keys";
import { can } from "../lib/platform";
import type { IntervalPlan, Settings, Shortcuts } from "../lib/settings";
import { useUpdate } from "../lib/update";
import { codeOf, reasonText } from "../toast/reason";
import FirstUploadDialog, { type DialogFile } from "../upload/FirstUploadDialog.vue";
import IntervalForm from "./IntervalForm.vue";
import IntervalRun from "./IntervalRun.vue";
import { prepareUpload } from "../upload/flow";

/** How many captures the strip shows. */
const RECENT = 5;
/** How far the pointer moves with the button held before a thumbnail is dragged out as a file. */
const DRAG_SLOP = 4;

/** A capture mode, or `record`, which opens the recording's setup; `sep` draws a line before it. */
type Row = { mode: CaptureMode | "record"; icon: string; label: Key; shortcut: ShortcutAction; sep?: true };
const ROWS: Row[] = [
  { mode: "area", icon: "area", label: "popover.capture.area", shortcut: "captureArea" },
  { mode: "window", icon: "window", label: "popover.capture.window", shortcut: "captureWindow" },
  { mode: "display", icon: "display", label: "popover.capture.display", shortcut: "captureDisplay" },
  { mode: "repeatLastArea", icon: "repeat", label: "popover.capture.repeat", shortcut: "repeatLastArea" },
  { mode: "copyText", icon: "ocr", label: "popover.capture.copyText", shortcut: "copyText", sep: true },
  { mode: "record", icon: "record", label: "popover.capture.record", shortcut: "recordScreen" },
];

const DELAYS = [0, 3, 5, 10] as const;

// ------------------------------------------------------------------------------------ state

const root = ref<HTMLElement | null>(null);
/** Everything the panel holds; its height is what the panel moves to. */
const body = ref<HTMLElement | null>(null);
const shortcuts = ref<Shortcuts | null>(null);
const defaultProvider = ref<string | null>(null);
const entries = ref<Entry[]>([]);
const historyState = ref<"loading" | "ready" | "unavailable" | "failed">("loading");
const sel = ref(0);
const permissions = ref<Permissions | null>(null);
const delay = ref<DelayState>({ pendingSeconds: 0, remainingSeconds: null, mode: null });
const recording = ref<RecordingState>({ kind: "idle" });
/** The interval series that runs (`capture://interval`), its form, and the plan the form starts
 * from (Settings › general.interval). */
const interval = ref<IntervalState | null>(null);
const intervalPlan = ref<IntervalPlan | null>(null);
const formOpen = ref(false);
const startingInterval = ref(false);
const intervalRun = ref<InstanceType<typeof IntervalRun> | null>(null);
/** The take's time to the whole second, which is all the clock shows (`duration`). */
const elapsedMs = ref(0);
/** The core ticks four times a second through the whole take, and this page lives (mostly hidden)
 * through it: keeping only the shown second leaves three ticks in four without a render, and
 * without the measuring `fit` after it. */
const showElapsed = (ms: number) => void (elapsedMs.value = Math.round(ms / 1000) * 1000);
/** A line under the panel's sections: the result or the failure of the last action. */
const notice = ref<{ tone: "ok" | "warn"; text: string } | null>(null);

type Uploading = { uploadId: string | null; provider: string; sentBytes: number; totalBytes: number };
/** Uploads this popover started, by entry: a ring on the thumbnail and Cancel upload. */
const uploads = reactive(new Map<EntryId, Uploading>());
const providerNames = reactive(new Map<string, string>());

const restartNeeded = computed(() => permissions.value?.restartNeeded === true);
const blocked = computed(() => {
  const state = permissions.value?.screenRecording;
  return state !== undefined && state !== "granted" && state !== "notRequired";
});
const recordingNow = computed(() => recording.value.kind !== "idle");
const intervalNow = computed(() => interval.value?.running === true);
const selected = computed<Entry | null>(() => entries.value[sel.value] ?? null);
const slots = computed(() => Array.from({ length: RECENT }, (_, i) => entries.value[i] ?? null));

/** The mode under the pointer, and the one with the keyboard focus: the line under the strip
 * names the first of them, or Capture area. `lastMode` is where ↑ returns from the thumbnails. */
const pointMode = ref<number | null>(null);
const focusMode = ref<number | null>(null);
const lastMode = ref(0);
const shownIndex = computed(() => pointMode.value ?? focusMode.value ?? 0);
const shownMode = computed(() => ROWS[shownIndex.value]!);

const delayOpen = ref(false);
const delayName = (seconds: number) => (seconds === 0 ? t("popover.delay.off") : t("popover.delay.seconds", { n: seconds }));
/** The delay's button shows the chosen value, and only its icon while there is none: a long mode
 * name, its keys and a worded "no delay" do not share 320 px in every language. */
const delayButton = computed(() => (delay.value.pendingSeconds === 0 ? "" : t("popover.delay.seconds", { n: delay.value.pendingSeconds })));

/** The first lines of each text copy the preview has shown: a page of the history leaves the text
 * out, so the preview asks for the entry once. */
const texts = reactive(new Map<EntryId, string>());
const TEXT_LINES = 5;

// ---------------------------------------------------------------------------------- loading

async function loadSettings(settings?: Settings): Promise<void> {
  try {
    const s = settings ?? (await settings_get());
    shortcuts.value = s.shortcuts;
    defaultProvider.value = s.upload.defaultProvider;
    intervalPlan.value = s.general.interval;
  } catch (error) {
    console.warn("settings_get", error);
  }
}

async function loadHistory(): Promise<void> {
  try {
    const page = await history_list({ ...DEFAULT_HISTORY_QUERY, offset: 0, limit: RECENT });
    entries.value = page.items.map((item) => item.entry);
    historyState.value = "ready";
    if (sel.value >= entries.value.length) sel.value = Math.max(0, entries.value.length - 1);
  } catch (error) {
    entries.value = [];
    historyState.value = codeOf(error) === "not_implemented" ? "unavailable" : "failed";
  }
}

async function loadPermissions(): Promise<void> {
  try {
    permissions.value = await permissions_status();
  } catch (error) {
    console.warn("permissions_status", error);
  }
}

async function loadDelay(): Promise<void> {
  try {
    delay.value = await capture_delay();
  } catch (error) {
    console.warn("capture_delay", error);
  }
}

/** The take that runs now, for a page that reloaded in the middle of one: the events carry on. */
async function loadRecording(): Promise<void> {
  try {
    const state = await record_state();
    recording.value = state;
    if (state.kind === "recording" || state.kind === "paused") showElapsed(state.elapsedMs);
  } catch (error) {
    console.warn("record_state", error);
  }
}

async function loadInterval(): Promise<void> {
  try {
    interval.value = await capture_interval();
  } catch (error) {
    console.warn("capture_interval", error);
  }
}

async function opened(): Promise<void> {
  quietUntil = performance.now() + 400;
  notice.value = null;
  // A question or a menu left open when the panel closed is not shown again on its own.
  asking.value = null;
  delayOpen.value = false;
  formOpen.value = false;
  pointMode.value = null;
  sel.value = 0;
  await Promise.all([loadSettings(), loadHistory(), loadPermissions(), loadDelay(), loadRecording(), loadInterval()]);
  await nextTick();
  root.value?.focus({ preventScroll: true });
  // A series that runs takes the modes' place, and Return on the panel stops it.
  if (intervalNow.value) intervalRun.value?.focus();
}

let historyTimer: ReturnType<typeof setTimeout> | null = null;
function historyChanged(): void {
  if (historyTimer) clearTimeout(historyTimer);
  historyTimer = setTimeout(() => void loadHistory(), 150);
}

// ------------------------------------------------------------------------------- the window

/* The panel's height moves rather than jumps when what it holds changes (a notice, a section).
   The window is transparent around the panel, so it takes the taller of the two heights for the
   move and the new one once the move ends, and each frame of the move has AppKit recompute the
   window's shadow, which it takes from the panel's pixels: a stale one would trail under a
   shrinking panel. Right after the popover opens the page refills, and that lands at once. */
const MOVE_MS = 220; // --dur-move
const MOVE_EASE = "cubic-bezier(0.22, 1, 0.36, 1)"; // --ease-out
const stillMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
let goal = 0;
let windowHeight = 0;
let move: Animation | null = null;
let quietUntil = 0;

function natural(): number {
  const panel = root.value;
  if (!panel) return 0;
  if (!body.value) return Math.ceil(panel.getBoundingClientRect().height);
  const style = getComputedStyle(panel);
  const padding = parseFloat(style.paddingTop) + parseFloat(style.paddingBottom);
  return Math.ceil(body.value.getBoundingClientRect().height + padding);
}

function sizeWindow(height: number, again = false): void {
  if (height === windowHeight && !again) return;
  windowHeight = height;
  popover_fit(height).catch((error) => console.warn("popover_fit", error));
}

function fit(): void {
  const panel = root.value;
  const target = natural();
  if (!panel || !target || target === goal) return;
  const from = move ? panel.getBoundingClientRect().height : goal;
  move?.cancel();
  move = null;
  goal = target;
  if (!body.value || !from || performance.now() < quietUntil || stillMotion.matches || document.hidden) {
    panel.style.height = "";
    panel.style.overflow = "";
    sizeWindow(target);
    return;
  }
  panel.style.height = `${from}px`;
  panel.style.overflow = "hidden";
  sizeWindow(Math.max(Math.ceil(from), target));
  const running = panel.animate([{ height: `${from}px` }, { height: `${target}px` }], {
    duration: MOVE_MS,
    easing: MOVE_EASE,
  });
  move = running;
  const shade = () => {
    if (move !== running) return;
    sizeWindow(windowHeight, true);
    requestAnimationFrame(shade);
  };
  requestAnimationFrame(shade);
  running.onfinish = () => {
    if (move !== running) return;
    move = null;
    panel.style.height = "";
    panel.style.overflow = "";
    sizeWindow(target, true);
  };
}

/** Watches the content when the panel has it apart (the panel's own height is held during a
 * move), and the panel itself while the first-upload dialog fills it. */
function follow(): void {
  observer?.disconnect();
  const watched = body.value ?? root.value;
  if (watched) observer?.observe(watched);
  fit();
}

function hide(): void {
  popover_hide().catch((error) => console.warn("popover_hide", error));
}

function fail(error: unknown): void {
  notice.value = { tone: "warn", text: reasonText(codeOf(error)) };
}

// --------------------------------------------------------------------------------- capturing

async function capture(row: Row): Promise<void> {
  if (blocked.value || recordingNow.value) return;
  try {
    // The setup surface takes over from here: the user draws the area or switches to a window
    // or a display on its bar, as after the Record screen shortcut.
    if (row.mode === "record") await record_setup({ mode: "area" });
    else await capture_start(row.mode);
    hide();
  } catch (error) {
    fail(error);
  }
}

async function setDelay(seconds: number): Promise<void> {
  // The chosen item goes away with the menu, so the focus returns to the button that opened it
  // and the panel's keys keep working.
  closeDelayMenu(true);
  try {
    delay.value = await capture_set_delay(seconds);
  } catch (error) {
    fail(error);
  }
}

const delayToggle = ref<HTMLElement | null>(null);

/** Opens the delay's menu with the focus on the chosen value, so ↑↓ and Return pick another. */
function openDelayMenu(): void {
  if (blocked.value) return;
  delayOpen.value = true;
  void nextTick(() => root.value?.querySelector<HTMLElement>(".s-delay-menu [aria-checked='true']")?.focus());
}

function closeDelayMenu(refocus = false): void {
  if (!delayOpen.value) return;
  delayOpen.value = false;
  if (refocus) delayToggle.value?.focus();
}

/** A press anywhere outside the delay's menu and its button closes the menu. */
function pressOutside(event: PointerEvent): void {
  if (delayOpen.value && !(event.target as Element | null)?.closest(".s-delay-pick")) closeDelayMenu();
}

// ------------------------------------------------------------------------- the interval series

const intervalKeys = computed(() => capsText(shortcuts.value?.intervalCapture));

function openIntervalForm(): void {
  closeDelayMenu();
  if (blocked.value || recordingNow.value || intervalNow.value || !intervalPlan.value) return;
  formOpen.value = true;
}

function closeIntervalForm(): void {
  formOpen.value = false;
  void nextTick(() => delayToggle.value?.focus());
}

/** The area target opens the overlay and the series starts with its selection; the others start
 * at once. Either way the panel goes, as after a capture mode. */
async function startInterval(plan: IntervalPlan): Promise<void> {
  if (startingInterval.value) return;
  startingInterval.value = true;
  try {
    interval.value = await capture_start_interval(plan);
    intervalPlan.value = plan;
    formOpen.value = false;
    hide();
  } catch (error) {
    fail(error);
  } finally {
    startingInterval.value = false;
  }
}

async function stopInterval(): Promise<void> {
  try {
    await capture_stop_interval();
    await loadInterval();
  } catch (error) {
    fail(error);
  }
}

const seriesCount = (state: IntervalState) => (state.manual ? String(state.taken) : `${state.taken}/${state.total}`);
const clockOf = (ms: number, seconds = true) =>
  new Date(ms).toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", second: seconds ? "2-digit" : undefined });
const seriesFrame = computed(() => {
  const state = interval.value;
  return state?.entryId && state.latestIndex !== null ? frameUrl(state.entryId, state.latestIndex) : null;
});
const seriesTag = computed(() => {
  const state = interval.value;
  if (!state) return "";
  return state.latestAtMs !== null ? `${seriesCount(state)} · ${clockOf(state.latestAtMs)}` : seriesCount(state);
});
const seriesSub = computed(() => {
  const state = interval.value;
  if (!state) return "";
  if (state.skipped === 1) return t("popover.interval.runningSkipped.one");
  if (state.skipped > 1) return t("popover.interval.runningSkipped.many", { n: state.skipped });
  return t("popover.interval.since", { time: clockOf(state.startedAtMs, false) });
});
/** What the hero says while it has no frame to show: the first one is on its way, or the history
 * is off and the frames go to the folder alone. */
const seriesWaiting = computed(() => {
  const state = interval.value;
  if (state?.latestIndex !== null && state?.entryId === null) {
    return t("popover.interval.noHistory", { folder: state.folderName ?? "" });
  }
  return t("popover.interval.waiting");
});

async function cancelDelay(): Promise<void> {
  try {
    await capture_cancel_delay();
    await loadDelay();
  } catch (error) {
    fail(error);
  }
}

// ----------------------------------------------------------------------------- the captures

function dayStart(date: Date): number {
  return new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
}

function when(ms: number): string {
  const date = new Date(ms);
  const time = date.toLocaleTimeString(language.value, { hour: "2-digit", minute: "2-digit" });
  const days = Math.round((dayStart(new Date()) - dayStart(date)) / 86_400_000);
  if (days === 0) return t("popover.item.today", { time });
  if (days === 1) return t("popover.item.yesterday", { time });
  return date.toLocaleDateString(language.value, { day: "numeric", month: "short" });
}

function duration(ms: number): string {
  const seconds = Math.round(ms / 1000);
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

function bytes(n: number): string {
  const format = (v: number, digits: number) =>
    new Intl.NumberFormat(language.value, { maximumFractionDigits: digits, minimumFractionDigits: digits }).format(v);
  if (n < 1000) return `${n} B`;
  if (n < 1_000_000) return `${format(n / 1000, 0)} KB`;
  return `${format(n / 1_000_000, 1)} MB`;
}

function title(entry: Entry): string {
  return entry.kind === "textCopy" ? t("popover.item.text") : entryName(entry);
}

function uploadPercent(entry: Entry): number | null {
  const u = uploads.get(entry.id);
  return u && u.totalBytes > 0 ? Math.min(100, (u.sentBytes / u.totalBytes) * 100) : null;
}

function sub(entry: Entry): string {
  const u = uploads.get(entry.id);
  if (u) {
    const host = providerNames.get(u.provider) ?? u.provider;
    const p = uploadPercent(entry);
    return p === null ? t("popover.item.uploadStarting", { host }) : t("popover.item.uploading", { host, p: Math.round(p) });
  }
  if (entry.hosted) return entry.hosted.url;
  const parts = [when(entry.createdAtMs)];
  if (entry.kind === "textCopy") {
    parts.push(entry.textLineCount === 1 ? t("popover.item.line") : t("popover.item.lines", { n: entry.textLineCount }));
  } else if (entry.kind === "recording") {
    if (entry.durationMs !== null) parts.push(duration(entry.durationMs));
    if (entry.fileBytes !== null) parts.push(bytes(entry.fileBytes));
  } else if (entry.size) {
    parts.push(`${entry.size.width} × ${entry.size.height}`);
  }
  return parts.join(" · ");
}

/** The file an upload works on and whose format the first-upload dialog names: the image the entry
 * shows (`mediaPath`), never an older saved file. Show in Finder goes to `savedPath` itself. */
const pathOf = (entry: Entry) => mediaPath(entry);

type ActId = "copy" | "path" | "send" | "upload" | "link" | "cancel" | "pin" | "editor" | "trim" | "folder";
/** `short` is the name a labelled button shows; the tip keeps the full name and the keys. */
type Act = { id: ActId; icon: string; label: Key; caps: string[]; short?: Key };

const ACT: Record<ActId, Omit<Act, "id">> = {
  copy: { icon: "copy", label: "popover.act.copy", caps: ["⌘", "C"], short: "popover.act.copy" },
  path: { icon: "path", label: "send.copyPath", caps: ["⌥", "⌘", "C"] },
  send: { icon: "send", label: "send.send", caps: ["⌘", "↩"] },
  upload: { icon: "upload", label: "popover.act.upload", caps: ["⌘", "U"] },
  link: { icon: "link", label: "popover.act.link", caps: ["⇧", "⌘", "L"] },
  cancel: { icon: "close", label: "popover.act.cancel", caps: ["⌘", "."] },
  pin: { icon: "pin", label: "popover.act.pin", caps: ["⌘", "P"] },
  editor: { icon: "external-link", label: "popover.act.editor", caps: ["⌘", "O"], short: "popover.act.editShort" },
  trim: { icon: "trim", label: "popover.act.trim", caps: ["⌘", "T"], short: "popover.act.trimShort" },
  folder: { icon: "folder", label: "popover.act.folder", caps: ["⌥", "⌘", "R"] },
};

/** The actions that work for this entry now; a control that could not work is left out. */
function actionsFor(entry: Entry): Act[] {
  const ids: ActId[] = ["copy"];
  // Send holds Copy path in its menu; where there is no Send (Windows, Linux), Copy path stands
  // on its own. A text copy has no file, and its text goes to an app with Send only.
  if (can("sendToApp")) ids.push("send");
  else if (entry.kind !== "textCopy" && pathOf(entry)) ids.push("path");
  if (uploads.has(entry.id)) ids.push("cancel");
  else if (entry.hosted) ids.push("link");
  else if (entry.kind !== "textCopy" && defaultProvider.value && pathOf(entry)) ids.push("upload");
  if (entry.kind === "screenshot") ids.push("pin", "editor");
  // A recording offers Trim where a screenshot offers the editor (DESIGN.md, Popover).
  if (entry.kind === "recording") ids.push("trim");
  if (entry.savedPath) ids.push("folder");
  return ids.map((id) => ({ id, ...ACT[id] }));
}

/** Copy and the capture's own second action (the editor, or Trim for a recording) as labelled
 * buttons, every other action as an icon at the row's end. */
const shelf = computed(() => {
  const all = selected.value ? actionsFor(selected.value) : [];
  return { labelled: all.filter((a) => a.short), icons: all.filter((a) => !a.short) };
});

async function learnProviders(): Promise<void> {
  if (providerNames.size) return;
  try {
    for (const provider of await upload_providers()) providerNames.set(provider.id, provider.name);
  } catch {
    // The id stands in for the name.
  }
}

/** The Send menu under its button (A2), and the entry it sends. */
const sendMenu = ref<{ items: MenuItem[]; at: { x: number; y: number } } | null>(null);

async function openSend(entry: Entry): Promise<void> {
  if (sendMenu.value) {
    sendMenu.value = null;
    return;
  }
  const button = root.value?.querySelector<HTMLElement>('.s-acts [data-act="send"]');
  const list = await send_list_targets();
  const box = button?.getBoundingClientRect();
  const items = sendMenuItems(list, {
    noPath: entry.kind === "textCopy",
    copyPath: () => void run("path", entry),
    send: (bundleId) => void sendTo(entry, bundleId),
    edit: () => {
      hide();
      window_open_settings("output").catch((error) => console.warn("window_open_settings", error));
    },
  });
  sendMenu.value = { items, at: box ? { x: box.right - 236, y: box.bottom + 4 } : { x: 8, y: 8 } };
}

async function sendTo(entry: Entry, bundleId: string): Promise<void> {
  try {
    await send_to_app({ kind: "entry", entryId: entry.id }, bundleId);
    // The app comes forward, and the toast follows the send.
    hide();
  } catch (error) {
    fail(error);
  }
}

async function run(id: ActId, entry: Entry): Promise<void> {
  notice.value = null;
  try {
    switch (id) {
      case "path": {
        const sent = await send_copy_path({ kind: "entry", entryId: entry.id });
        notice.value = { tone: "ok", text: t(sent.temporary ? "send.copiedTemporary" : "send.copied") };
        return;
      }
      case "send":
        return openSend(entry);
      case "copy":
        if (entry.kind === "textCopy") {
          const full = await history_get(entry.id);
          await app_copy_text(full.text ?? "");
        } else {
          await history_copy(entry.id);
        }
        notice.value = { tone: "ok", text: t("popover.act.copied") };
        return;
      case "link":
        if (!entry.hosted) return;
        await app_copy_text(entry.hosted.url);
        notice.value = { tone: "ok", text: t("popover.act.linkCopied") };
        return;
      case "upload":
        return upload(entry);
      case "cancel": {
        const u = uploads.get(entry.id);
        if (u?.uploadId) await upload_cancel(u.uploadId);
        return;
      }
      case "pin":
        await pin_open({ kind: "history", entryId: entry.id });
        hide();
        return;
      case "editor":
        await history_open_in_editor(entry.id);
        hide();
        return;
      case "trim":
        await record_open_trim({ entryId: entry.id }, "mp4");
        hide();
        return;
      case "folder":
        if (!entry.savedPath) return;
        await output_reveal(entry.savedPath);
        hide();
        return;
    }
  } catch (error) {
    fail(error);
  }
}

/** The first-upload question, shown in place of the popover's content, and the capture it is for. */
const asking = ref<{ consent: ConsentInfo; entry: Entry } | null>(null);

/** Checks the default host first, as every Upload does: a destination the user has not confirmed
 * yet gets the first-upload dialog, and a host whose settings stop it sends the user to Settings. */
async function upload(entry: Entry): Promise<void> {
  const path = pathOf(entry);
  const provider = defaultProvider.value;
  if (!path || !provider || uploads.has(entry.id)) return;
  void learnProviders();
  try {
    const ready = await prepareUpload(provider);
    if (ready.kind === "noProvider" || ready.kind === "blocked") {
      // Settings › Upload marks what is missing, which is where the fix happens.
      await window_open_settings("upload");
      hide();
    } else if (ready.kind === "needsConsent") {
      asking.value = { consent: ready.preflight.consent, entry };
    } else {
      await startUpload(entry, path, provider);
    }
  } catch (error) {
    fail(error);
  }
}

function dialogFile(entry: Entry): DialogFile {
  const format = pathOf(entry)?.split(".").pop()?.toUpperCase() ?? null;
  return {
    name: entryName(entry),
    thumbnail: thumbUrl(entry),
    width: entry.size?.width ?? null,
    height: entry.size?.height ?? null,
    format: format === "JPG" ? "JPEG" : format === "WEBP" ? "WebP" : format,
    bytes: entry.fileBytes,
  };
}

function consentGiven(): void {
  const entry = asking.value?.entry;
  asking.value = null;
  const path = entry ? pathOf(entry) : null;
  if (entry && path && defaultProvider.value) void startUpload(entry, path, defaultProvider.value);
}

/** The settings point elsewhere by now: the dialog asks again with the new destination. */
async function consentStale(): Promise<void> {
  const entry = asking.value?.entry;
  if (!entry) return;
  try {
    const ready = await prepareUpload(defaultProvider.value);
    asking.value = ready.kind === "needsConsent" ? { consent: ready.preflight.consent, entry } : null;
    if (ready.kind === "ready") consentGiven();
  } catch (error) {
    asking.value = null;
    fail(error);
  }
}

async function startUpload(entry: Entry, path: string, provider: string): Promise<void> {
  uploads.set(entry.id, { uploadId: null, provider, sentBytes: 0, totalBytes: 0 });
  try {
    // The upload runs on in the core and ends with `upload://done` or `upload://failed`; the
    // entry id lets the history record the link itself.
    const started = await upload_file(path, provider, entry.id);
    const u = uploads.get(entry.id);
    if (u) {
      u.uploadId = started.uploadId;
      u.totalBytes = started.totalBytes;
    }
  } catch (error) {
    uploads.delete(entry.id);
    if (codeOf(error) !== "cancelled") fail(error);
  }
}

/** An upload this popover started ended (done, failed or cancelled). */
function uploadEnded(uploadId: string): void {
  for (const [id, u] of uploads) if (u.uploadId === uploadId) uploads.delete(id);
  historyChanged();
}

/** The first progress event carries the upload's id: it belongs to the upload this popover
 * started for the same host and has no id yet. */
function uploadProgress(uploadId: string, provider: string, sentBytes: number, totalBytes: number): void {
  for (const u of uploads.values()) {
    if (u.uploadId === uploadId || (u.uploadId === null && u.provider === provider)) {
      u.uploadId = uploadId;
      u.sentBytes = sentBytes;
      u.totalBytes = totalBytes;
      return;
    }
  }
}

// ----------------------------------------------------------------- strip: select and drag out

const thumbs = ref<HTMLElement[]>([]);

function select(index: number, focus = false): void {
  if (!entries.value.length) return;
  sel.value = Math.max(0, Math.min(entries.value.length - 1, index));
  if (focus) void nextTick(() => thumbs.value[sel.value]?.focus());
}

let press: { x: number; y: number; id: EntryId; dragged: boolean } | null = null;

function thumbDown(event: PointerEvent, entry: Entry, index: number): void {
  if (event.button !== 0) return;
  select(index);
  press = { x: event.screenX, y: event.screenY, id: entry.id, dragged: false };
}

function thumbMove(event: PointerEvent): void {
  if (!press || press.dragged) return;
  if (!(event.buttons & 1)) {
    press = null;
    return;
  }
  if (Math.hypot(event.screenX - press.x, event.screenY - press.y) <= DRAG_SLOP) return;
  press.dragged = true;
  // The native drag takes over while the button is still down.
  popover_drag_out(press.id).catch(fail);
}

function thumbUp(): void {
  press = null;
}

/** Return on a thumbnail opens it: a screenshot in the editor, a recording in the trim window, a
 * text copy to the clipboard. */
function open(entry: Entry): void {
  if (entry.kind === "screenshot") void run("editor", entry);
  else if (entry.kind === "recording") void run("trim", entry);
  else void run("copy", entry);
}

watch(selected, async (entry) => {
  if (!entry || entry.kind !== "textCopy" || texts.has(entry.id)) return;
  texts.set(entry.id, "");
  try {
    const full = await history_get(entry.id);
    const lines = (full.text ?? "").split(/\r?\n/).filter((line) => line.trim());
    texts.set(entry.id, lines.slice(0, TEXT_LINES).join("\n"));
  } catch (error) {
    // The preview stays empty; Copy still reads the text when it is pressed.
    console.warn("history_get", error);
  }
});

// -------------------------------------------------------------------------------- recording

async function recordAction(action: "pause" | "resume" | "stop" | "discard"): Promise<void> {
  try {
    if (action === "pause") await record_pause();
    else if (action === "resume") await record_resume();
    else if (action === "stop") await record_stop();
    else await record_discard();
  } catch (error) {
    fail(error);
  }
}

const recordSeconds = computed(() => (recording.value.kind === "countdown" ? recording.value.remainingSeconds : 0));
const recordMuted = computed(() =>
  recording.value.kind === "recording" || recording.value.kind === "paused" ? recording.value.microphoneMuted : false,
);

// ---------------------------------------------------------------------- update

const updateView = useUpdate();

function restartForUpdate(): void {
  void update_install().catch((error) => (notice.value = { tone: "warn", text: reasonText(codeOf(error)) }));
}

function openReleases(): void {
  void app_open_url(RELEASES_URL).catch((error) => (notice.value = { tone: "warn", text: reasonText(codeOf(error)) }));
}

// ---------------------------------------------------------------------- permission and footer

async function openPermission(): Promise<void> {
  try {
    const state = await permissions_request("screenRecording");
    if (state !== "granted") await permissions_open_settings("screenRecording");
    hide();
  } catch (error) {
    fail(error);
  }
}

async function openWindow(kind: WindowKind): Promise<void> {
  try {
    await window_open(kind);
    hide();
  } catch (error) {
    fail(error);
  }
}

// ---------------------------------------------------------------------------------- keyboard

function modeButtons(): HTMLElement[] {
  return [...(root.value?.querySelectorAll<HTMLElement>(".s-mode:not([aria-disabled='true'])") ?? [])];
}

/** ↑↓ move through the delay's menu while it is open, Esc closes it and nothing else. */
function onDelayMenuKey(event: KeyboardEvent): boolean {
  if (event.key === "Escape") {
    event.preventDefault();
    closeDelayMenu(true);
    return true;
  }
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return false;
  event.preventDefault();
  const items = [...(root.value?.querySelectorAll<HTMLElement>(".s-delay-menu .g-menu-item") ?? [])];
  const at = items.indexOf(document.activeElement as HTMLElement);
  const next = at === -1 ? 0 : Math.max(0, Math.min(items.length - 1, at + (event.key === "ArrowDown" ? 1 : -1)));
  items[next]?.focus();
  return true;
}

function onKey(event: KeyboardEvent): void {
  const active = document.activeElement as HTMLElement | null;
  const onThumb = !!active?.closest(".s-thumb");
  const mod = event.metaKey || event.ctrlKey;

  if (delayOpen.value && onDelayMenuKey(event)) return;

  if (event.key === "Escape") {
    event.preventDefault();
    if (formOpen.value) closeIntervalForm();
    else hide();
    return;
  }

  if (mod) {
    const k = event.key.toLowerCase();
    // The form and a running series hide the picked capture, so its keys rest with it.
    const entry = formOpen.value || intervalNow.value ? null : selected.value;
    const offered = (id: ActId) => (entry && actionsFor(entry).some((a) => a.id === id) ? entry : null);
    let handled = true;
    if (k === "y" && !event.shiftKey) void openWindow("history");
    else if (k === ",") void openWindow("settings");
    else if (k === "q") void app_quit();
    // ⌥ turns the key's character into another one ("ç"), so Copy path is read by the key's code.
    else if (event.code === "KeyC" && event.altKey && !event.shiftKey && entry && entry.kind !== "textCopy" && pathOf(entry)) void run("path", entry);
    else if (event.key === "Enter" && !event.shiftKey && !event.altKey && offered("send")) void run("send", entry!);
    else if (k === "c" && !event.shiftKey && offered("copy")) void run("copy", entry!);
    else if (k === "u" && offered("upload")) void run("upload", entry!);
    else if (k === "l" && event.shiftKey && offered("link")) void run("link", entry!);
    else if (k === "." && offered("cancel")) void run("cancel", entry!);
    else if (k === "p" && offered("pin")) void run("pin", entry!);
    else if (k === "o" && offered("editor")) void run("editor", entry!);
    else if (k === "t" && offered("trim")) void run("trim", entry!);
    else if ((k === "r" || event.code === "KeyR") && event.altKey && offered("folder")) void run("folder", entry!);
    else handled = false;
    if (handled) event.preventDefault();
    return;
  }

  const modes = modeButtons();
  const atMode = active ? modes.indexOf(active) : -1;
  // The panel itself holds the focus when it opens, and an arrow from there enters the modes.
  const onPanel = active === root.value;
  const onDelay = !!active?.closest(".s-delay-pick");

  if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
    const step = event.key === "ArrowRight" ? 1 : -1;
    if (onThumb) {
      event.preventDefault();
      select(sel.value + step, true);
    } else if (atMode !== -1 || onPanel) {
      event.preventDefault();
      modes[atMode === -1 ? 0 : Math.max(0, Math.min(modes.length - 1, atMode + step))]?.focus();
    }
    return;
  }

  if (event.key === "ArrowDown") {
    if (atMode === -1 && !onPanel && !onDelay) return;
    event.preventDefault();
    if (onPanel && modes.length) modes[Math.min(lastMode.value, modes.length - 1)]!.focus();
    else if (entries.value.length) select(sel.value, true);
    return;
  }

  if (event.key === "ArrowUp") {
    if (!onThumb && !onPanel && !onDelay) return;
    event.preventDefault();
    modes[Math.min(lastMode.value, modes.length - 1)]?.focus();
    return;
  }

  if (event.key === "Enter" && onThumb && selected.value) {
    event.preventDefault();
    open(selected.value);
  }
}

// ------------------------------------------------------------------------------------ wiring

const stops: (() => void)[] = [];
let observer: ResizeObserver | null = null;

onMounted(async () => {
  observer = new ResizeObserver(fit);
  follow();
  stops.push(
    await on<null>(POPOVER_OPENED, () => void opened()),
    await on<Settings>(SETTINGS_CHANGED, (settings) => void loadSettings(settings)),
    await on<Permissions>(PERMISSIONS_CHANGED, (next) => (permissions.value = next)),
    await on<DelayState>(CAPTURE_COUNTDOWN, (state) => (delay.value = state)),
    await on<IntervalState>(CAPTURE_INTERVAL, (state) => (interval.value = state)),
    await onHistoryChanged(historyChanged),
    await onRecordState((state) => {
      recording.value = state;
      if (state.kind === "recording" || state.kind === "paused") showElapsed(state.elapsedMs);
      if (state.kind === "idle") elapsedMs.value = 0;
    }),
    await onRecordTick((tick) => showElapsed(tick.elapsedMs)),
    await onUploadProgress((p) => uploadProgress(p.uploadId, p.provider, p.sentBytes, p.totalBytes)),
    await onUploadDone((done) => uploadEnded(done.result.uploadId)),
    await onUploadFailed((failed) => uploadEnded(failed.uploadId)),
  );
  await opened();
});

onUpdated(fit);
// The first-upload dialog replaces the panel's element, so the observer follows the new one.
watch([root, body], follow);

onUnmounted(() => {
  observer?.disconnect();
  move?.cancel();
  stops.forEach((stop) => stop());
  if (historyTimer) clearTimeout(historyTimer);
});

/** The empty strip's sentence around the area shortcut's caps, which the page draws as keys. */
const emptyKeys = computed(() => capsOf(shortcuts.value?.captureArea));
const emptyParts = computed(() => {
  const [before = "", after = ""] = t("popover.empty.keys").split("{keys}");
  return { before, after };
});
</script>

<template>
  <div v-if="asking" ref="root" class="g-popover solid s-pop s-consent">
    <FirstUploadDialog
      :consent="asking.consent"
      :file="dialogFile(asking.entry)"
      :can-choose-host="false"
      @confirmed="consentGiven"
      @cancel="asking = null"
      @stale="consentStale"
    />
  </div>
  <div
    v-else
    ref="root"
    class="g-popover solid s-pop"
    role="dialog"
    aria-label="Glimpsfy"
    tabindex="-1"
    @keydown="onKey"
    @pointerdown.capture="pressOutside"
  >
    <div ref="body" class="s-body">
      <!-- the screen recording permission is missing, or works only after a restart -->
      <div v-if="restartNeeded" class="g-popover-section">
        <div class="s-note" role="alert">
          <span class="s-note-icon"><GIcon name="warning" :size="16" /></span>
          <div class="s-note-text">
            <b class="s-note-title">{{ t("popover.perm.restartTitle") }}</b>
            <p class="s-note-body">{{ t("popover.perm.restartBody") }}</p>
            <div class="s-note-actions">
              <button type="button" class="g-button" data-variant="primary" @click="app_restart()">
                <GIcon name="retry" :size="16" />{{ t("popover.perm.restart") }}
              </button>
            </div>
          </div>
        </div>
      </div>
      <div v-else-if="blocked" class="g-popover-section">
        <div class="s-note" role="alert">
          <span class="s-note-icon"><GIcon name="warning" :size="16" /></span>
          <div class="s-note-text">
            <b class="s-note-title">{{ t("popover.perm.title") }}</b>
            <p class="s-note-body">{{ t("popover.perm.body") }}</p>
            <div class="s-note-actions">
              <button type="button" class="g-button" data-variant="primary" @click="openPermission">
                <GIcon name="external-link" :size="16" />{{ t("popover.perm.open") }}
              </button>
              <button type="button" class="g-button" data-variant="plain" @click="openWindow('firstrun')">
                {{ t("popover.perm.setup") }}
              </button>
            </div>
          </div>
        </div>
      </div>

      <!-- a recording runs: the rows become its control bar -->
      <div v-if="recordingNow" class="g-popover-section s-rec">
        <div class="g-bar" role="toolbar" :aria-label="t('popover.rec.aria')">
          <span class="g-timecode">
            <span class="g-rec-dot" :data-state="recording.kind === 'paused' ? 'paused' : undefined" />
            <span>{{ duration(elapsedMs) }}</span>
          </span>
          <span class="g-divider" role="separator" />
          <span class="s-rec-what">{{ recording.kind === "paused" ? t("popover.rec.paused") : t("popover.rec.recording") }}</span>
          <button
            v-if="recording.kind === 'recording' || recording.kind === 'paused'"
            type="button"
            class="g-btn"
            :aria-label="recording.kind === 'paused' ? t('popover.rec.resume') : t('popover.rec.pause')"
            @click="recordAction(recording.kind === 'paused' ? 'resume' : 'pause')"
          >
            <GIcon :name="recording.kind === 'paused' ? 'play' : 'pause'" />
          </button>
          <button
            type="button"
            class="g-btn"
            data-tone="record"
            :aria-label="t('popover.rec.stop')"
            :disabled="recording.kind === 'finishing' || recording.kind === 'countdown'"
            @click="recordAction('stop')"
          >
            <GIcon name="stop" />
          </button>
          <span class="g-divider" role="separator" />
          <button
            type="button"
            class="g-btn"
            data-tone="muted"
            :aria-label="t('popover.rec.discard')"
            :disabled="recording.kind === 'finishing'"
            @click="recordAction('discard')"
          >
            <GIcon name="trash" />
          </button>
        </div>
        <p v-if="recording.kind === 'countdown'" class="s-rec-note">
          <GIcon name="timer" :size="16" /><span>{{ t("popover.rec.countdown", { n: recordSeconds }) }}</span>
        </p>
        <p v-else-if="recording.kind === 'finishing'" class="s-rec-note">
          <ProgressRing :progress="null" :label="t('popover.rec.finishing')" :size="14" /><span>{{ t("popover.rec.finishing") }}</span>
        </p>
        <p v-else-if="recordMuted" class="s-rec-note">
          <GIcon name="microphone-off" :size="16" /><span>{{ t("popover.rec.muted") }}</span>
        </p>
      </div>

      <!-- the interval series that runs: its count, Stop, its bar and its next frame -->
      <div v-else-if="intervalNow && interval" class="g-popover-section">
        <IntervalRun ref="intervalRun" :state="interval" @stop="stopInterval" />
      </div>

      <!-- the capture modes, the line that names one, and the one-shot delay -->
      <div v-else class="g-popover-section">
        <div class="s-modes" role="menu" :aria-label="t('popover.capture.label')" @pointerleave="pointMode = null">
          <template v-for="(row, i) in ROWS" :key="row.mode">
            <span v-if="row.sep" class="s-modes-sep" role="separator" />
            <button
              type="button"
              class="s-mode"
              role="menuitem"
              :aria-label="t(row.label)"
              :aria-disabled="blocked ? 'true' : undefined"
              :data-on="i === shownIndex ? '' : undefined"
              @pointerenter="pointMode = i"
              @focus="(focusMode = i), (lastMode = i)"
              @blur="focusMode = null"
              @click="capture(row)"
            >
              <GIcon :name="row.icon" :size="20" />
            </button>
          </template>
        </div>
        <div v-if="delay.remainingSeconds !== null" class="s-line" role="status">
          <span class="s-line-count"><GIcon name="timer" :size="16" />{{ t("popover.delay.counting", { n: delay.remainingSeconds }) }}</span>
          <button type="button" class="g-button" data-variant="plain" @click="cancelDelay">{{ t("popover.delay.cancel") }}</button>
        </div>
        <div v-else class="s-line" :aria-disabled="blocked ? 'true' : undefined">
          <b class="s-line-name">{{ t(shownMode.label) }}</b>
          <KeyCaps :accelerator="shortcuts?.[shownMode.shortcut]" />
          <div class="s-delay-pick">
            <TipWrap :tip="`${t('popover.delay.label')}: ${delayName(delay.pendingSeconds)}`" align="end">
              <button
                ref="delayToggle"
                type="button"
                class="g-button"
                data-variant="plain"
                aria-haspopup="menu"
                :aria-expanded="delayOpen"
                :aria-label="`${t('popover.delay.aria')}: ${delayName(delay.pendingSeconds)}`"
                :disabled="blocked"
                :data-on="delay.pendingSeconds ? '' : undefined"
                @click="delayOpen ? closeDelayMenu() : openDelayMenu()"
              >
                <GIcon name="timer" :size="16" />{{ delayButton }}<GIcon name="chevron-down" :size="12" />
              </button>
            </TipWrap>
            <Transition name="s-menu">
              <div v-if="delayOpen" class="g-menu solid s-delay-menu" role="menu" :aria-label="t('popover.delay.aria')">
                <button
                  v-for="seconds in DELAYS"
                  :key="seconds"
                  type="button"
                  class="g-menu-item"
                  role="menuitemradio"
                  :aria-checked="seconds === delay.pendingSeconds"
                  @click="setDelay(seconds)"
                >
                  <span class="g-menu-check"><GIcon name="check" :size="16" /></span>
                  <span>{{ delayName(seconds) }}</span>
                </button>
                <div class="g-menu-separator" role="separator" />
                <button type="button" class="g-menu-item" role="menuitem" @click="openIntervalForm">
                  <span class="g-menu-check" />
                  <span>{{ t("popover.interval.open") }}</span>
                  <span v-if="intervalKeys" class="g-menu-key">{{ intervalKeys }}</span>
                </button>
              </div>
            </Transition>
          </div>
        </div>
      </div>

      <!-- the picked capture large, the five latest under it, and the picked one's actions -->
      <div class="g-popover-section" @pointermove="thumbMove" @pointerup="thumbUp" @pointercancel="thumbUp">
        <IntervalForm
          v-if="formOpen && intervalPlan"
          :plan="intervalPlan"
          :accelerator="shortcuts?.intervalCapture"
          :busy="startingInterval"
          @cancel="closeIntervalForm"
          @start="startInterval"
        />
        <!-- a running series shows its newest frame in the hero, without the strip and the actions -->
        <template v-else-if="intervalNow && interval">
          <div
            class="s-hero"
            data-kind="series"
            :data-empty="seriesFrame ? undefined : ''"
            role="img"
            :aria-label="`${interval.name ?? ''}, ${seriesTag}`"
          >
            <img v-if="seriesFrame" :src="seriesFrame" alt="" draggable="false" />
            <div v-else class="s-empty-note">
              <GIcon name="grid" :size="24" />
              <span>{{ seriesWaiting }}</span>
            </div>
            <span v-if="seriesFrame" class="g-tag s-hero-dur"><GIcon name="grid" :size="12" />{{ seriesTag }}</span>
          </div>
          <div class="s-info">
            <span class="s-info-title">{{ interval.name ?? interval.folderName }}</span>
            <span class="s-info-sub">{{ seriesSub }}</span>
          </div>
        </template>
        <p v-else-if="historyState === 'unavailable'" class="s-empty-note">
          <span>{{ t("popover.recent.unavailable") }}</span>
        </p>
        <div v-else-if="historyState === 'failed'" class="s-empty-note s-row">
          <span>{{ t("popover.recent.failed") }}</span>
          <button type="button" class="g-button" data-variant="plain" @click="loadHistory">{{ t("popover.recent.retry") }}</button>
        </div>
        <template v-else>
          <div v-if="historyState === 'ready' && !entries.length" class="s-hero" data-empty>
            <div class="s-empty-note">
              <GIcon name="area" :size="24" />
              <b>{{ t("popover.empty.title") }}</b>
              <span v-if="emptyKeys.length">
                {{ emptyParts.before }}<KeyCaps :caps="emptyKeys" />{{ emptyParts.after }}
              </span>
              <span v-else>{{ t("popover.empty.noKeys") }}</span>
            </div>
          </div>
          <div
            v-else-if="selected"
            class="s-hero"
            :data-kind="selected.kind"
            role="img"
            :aria-label="`${title(selected)}, ${sub(selected)}`"
            :title="t('popover.hint')"
            @pointerdown="thumbDown($event, selected, sel)"
            @dblclick="open(selected)"
          >
            <div v-if="selected.kind === 'textCopy'" class="s-hero-text">
              <b><GIcon name="ocr" :size="12" />{{ selected.source.appName ?? t("popover.item.text") }}</b>
              <span>{{ texts.get(selected.id) }}</span>
            </div>
            <img v-else-if="thumbUrl(selected)" :src="thumbUrl(selected)!" alt="" draggable="false" />
            <button
              v-if="selected.kind === 'recording'"
              type="button"
              class="g-glass s-play"
              :aria-label="t('popover.act.trim')"
              @pointerdown.stop
              @click="run('trim', selected)"
            >
              <GIcon name="play" :size="20" />
            </button>
            <span v-if="selected.kind === 'recording' && selected.durationMs !== null" class="g-tag s-hero-dur">
              <GIcon name="film" :size="12" />{{ duration(selected.durationMs) }}
            </span>
            <span v-if="uploads.has(selected.id)" class="g-status" data-status="progress">
              <ProgressRing :progress="uploadPercent(selected)" :label="sub(selected)" :size="22" />
            </span>
            <span v-else-if="selected.hosted" class="g-status" data-status="ok"><GIcon name="link" :size="12" /></span>
          </div>
          <div v-else class="s-hero" aria-hidden="true" />

          <div class="s-strip" role="listbox" :aria-label="t('popover.recent.aria')" aria-orientation="horizontal">
            <template v-for="(entry, i) in slots" :key="entry ? entry.id : `empty-${i}`">
              <button
                v-if="entry"
                :ref="(el) => (thumbs[i] = el as HTMLElement)"
                type="button"
                class="s-thumb"
                role="option"
                :aria-selected="i === sel"
                :tabindex="i === sel ? 0 : -1"
                :aria-label="`${title(entry)}, ${sub(entry)}`"
                :title="t('popover.hint')"
                :data-badge="uploads.has(entry.id) || entry.hosted ? '' : undefined"
                @pointerdown="thumbDown($event, entry, i)"
                @focus="select(i)"
                @dblclick="open(entry)"
              >
                <span v-if="entry.kind === 'textCopy'" class="s-thumb-media s-thumb-text"><GIcon name="ocr" :size="16" /></span>
                <span v-else class="s-thumb-media">
                  <img v-if="thumbUrl(entry)" :src="thumbUrl(entry)!" alt="" draggable="false" />
                </span>
                <span v-if="entry.kind === 'recording' && entry.durationMs !== null" class="s-thumb-dur">{{
                  duration(entry.durationMs)
                }}</span>
                <span v-if="uploads.has(entry.id)" class="g-status" data-status="progress">
                  <ProgressRing :progress="uploadPercent(entry)" :label="sub(entry)" :size="22" />
                </span>
                <span v-else-if="entry.hosted" class="g-status" data-status="ok"><GIcon name="link" :size="12" /></span>
              </button>
              <span v-else class="s-thumb" data-empty aria-hidden="true" />
            </template>
          </div>

          <template v-if="selected">
            <div class="s-info">
              <span class="s-info-title">{{ title(selected) }}</span>
              <span class="s-info-sub" :data-kind="selected.hosted && !uploads.has(selected.id) ? 'link' : undefined">{{ sub(selected) }}</span>
            </div>
            <div class="s-acts" role="toolbar" :aria-label="t('popover.actions.aria')">
              <TipWrap v-for="act in shelf.labelled" :key="act.id" :tip="t(act.label)" :caps="act.caps" align="start">
                <button
                  type="button"
                  class="g-button s-act"
                  data-variant="plain"
                  :data-main="act.id === 'copy' ? '' : undefined"
                  @click="run(act.id, selected)"
                >
                  <GIcon :name="act.icon" :size="16" />{{ t(act.short!) }}
                </button>
              </TipWrap>
              <span class="g-spacer" />
              <TipWrap
                v-for="(act, j) in shelf.icons"
                :key="act.id"
                :tip="t(act.label)"
                :caps="act.caps"
                :align="j >= shelf.icons.length - 2 ? 'end' : undefined"
              >
                <button
                  type="button"
                  class="g-btn"
                  data-size="small"
                  :data-act="act.id"
                  :aria-label="t(act.label)"
                  :aria-expanded="act.id === 'send' ? !!sendMenu : undefined"
                  :data-state="act.id === 'send' && sendMenu ? 'pressed' : undefined"
                  @click="run(act.id, selected)"
                >
                  <GIcon :name="act.icon" :size="16" />
                </button>
              </TipWrap>
            </div>
          </template>
        </template>
        <Transition name="s-fade">
          <p v-if="notice" class="s-notice" :data-tone="notice.tone" role="status">
            <GIcon :name="notice.tone === 'ok' ? 'check' : 'warning'" :size="14" /><span>{{ notice.text }}</span>
          </p>
        </Transition>
      </div>

      <div v-if="updateView?.status === 'ready'" class="g-popover-section">
        <div class="s-note" data-tone="iris" role="status">
          <span class="s-note-icon"><GIcon name="save" :size="16" /></span>
          <div class="s-note-text">
            <b class="s-note-title">{{ t("update.ready.title", { version: updateView.version ?? "" }) }}</b>
            <p class="s-note-body">{{ t("update.ready.body") }}</p>
            <div class="s-note-actions">
              <button type="button" class="g-button" data-variant="primary" @click="restartForUpdate">{{ t("update.ready.now") }}</button>
              <button type="button" class="g-button" data-variant="plain" @click="openReleases">{{ t("update.ready.notes") }}</button>
            </div>
          </div>
        </div>
      </div>

      <!-- footer -->
      <div class="g-popover-section">
        <div class="g-popover-actions">
          <button type="button" class="g-button" data-variant="plain" @click="openWindow('history')">
            {{ t("popover.history") }}<span class="g-muted">⌘Y</span>
          </button>
          <span class="g-spacer" />
          <button type="button" class="g-button" data-variant="plain" @click="openWindow('settings')">
            {{ t("popover.settings") }}<span class="g-muted">⌘,</span>
          </button>
          <button type="button" class="g-button" data-variant="plain" @click="app_quit()">
            {{ t("popover.quit") }}<span class="g-muted">⌘Q</span>
          </button>
        </div>
      </div>
    </div>
    <Menu v-if="sendMenu" :items="sendMenu.items" :at="sendMenu.at" :label="t('send.send')" @close="sendMenu = null" />
  </div>
</template>

<style>
/* Page-local parts of the popover, from design/canvas row 06 (Y2) and design/round-2/small-surfaces
   (s-*). Every value is a kit token. The popover window is its own page, so these rules reach
   nothing else. */
.s-pop {
  width: 340px;
  /* The window draws the shadow natively; the panel keeps only its rim. */
  box-shadow: var(--win-material-rim);
  outline: 0;
}
/* The first-upload dialog is the popover's whole content while it asks, so it flows in the panel
   instead of floating over a scrim, and the window takes its height. */
.s-consent .g-scrim { position: static; padding: 0; background: none; }
.s-consent .g-dialog { width: auto; box-shadow: none; background: none; }
/* The content keeps its own height while the panel around it holds an older one for a move. */
.s-body { display: flex; flex-direction: column; flex: none; }
.s-pop :where(.s-thumb, .s-hero, .s-note, .s-line, .s-strip, .s-modes, .s-mode) {
  box-sizing: border-box;
}
button:is(.s-thumb, .s-mode) { margin: 0; padding: 0; border: 0; font: inherit; color: inherit; background: none; -webkit-appearance: none; appearance: none; }

/* The capture modes: a flat, darker well, the way the system's own capture bar sits, and one
   lit key riding in it. The key marks the mode the line below names (the one under the
   pointer or the focus, else the first), so the strip always shows depth rather than a flat
   slab of grey icons. */
.s-modes {
  display: flex; align-items: center; gap: 2px; height: 44px; padding: 3px; border-radius: 13px;
  background: var(--win-well);
}
.s-mode {
  position: relative; flex: 1; display: grid; place-items: center; height: 38px; border-radius: 10px;
  color: var(--win-text-2); isolation: isolate;
  transition: color var(--dur-fast) var(--ease-out);
}
.s-mode::before {
  content: ""; position: absolute; inset: 0; border-radius: inherit; z-index: -1; opacity: 0;
  background: var(--win-lens); box-shadow: var(--win-lens-edge);
  transition: opacity var(--dur-fast) var(--ease-out);
}
.s-mode[data-on] { color: var(--win-text); }
.s-mode[data-on]::before { opacity: 1; }
.s-mode[data-on]:active::before { background: var(--win-lens-press); }
.s-mode:focus-visible { outline: 0; }
.s-mode[aria-disabled="true"] { color: var(--win-text-disabled); --rec: currentColor; }
.s-mode[aria-disabled="true"]::before { opacity: 0; }
.s-modes-sep { flex: none; width: 1px; height: 20px; margin: 0 2px; background: var(--win-line); }

/* The line under the modes names the one under the pointer and ends with the delay. */
.s-line { position: relative; display: flex; align-items: center; gap: 8px; min-height: 32px; margin-top: 4px; padding-left: 6px; }
.s-line-name { font: var(--type-label); color: var(--win-text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.s-line[aria-disabled="true"] .s-line-name { color: var(--win-text-disabled); }
.s-line-count { flex: 1; display: inline-flex; align-items: center; gap: 8px; font: var(--type-text); color: var(--win-text); }
.s-line-count > g-icon { color: var(--win-text-2); }
.s-delay-pick { position: relative; margin-left: auto; }
.s-delay-pick .g-button { gap: 4px; padding-right: 9px; color: var(--win-text-2); }
/* A chosen delay is the line's one live setting, so it reads in iris. */
.s-delay-pick .g-button[data-on] { color: var(--win-iris-text); }
.s-delay-pick .g-tipwrap:has(> .g-button[aria-expanded="true"]) > .g-tip { display: none; }
.s-delay-menu { position: absolute; right: 0; top: calc(100% + 4px); z-index: 5; min-width: 150px; }
/* The menu grows out of its button's corner and fades away, as the system's menus do. */
.s-menu-enter-active { transition: opacity var(--dur-fast) var(--ease-out), transform var(--dur-base) var(--ease-out); }
.s-menu-leave-active { transition: opacity var(--dur-fast) var(--ease-out); }
.s-menu-enter-from { opacity: 0; transform: scale(0.96); }
.s-menu-leave-to { opacity: 0; }
.s-delay-menu { transform-origin: top right; }

/* The picked capture, whole: a contained image keeps a tall or a wide capture recognisable. */
.s-hero {
  position: relative; display: block; height: 172px; border-radius: var(--radius-group);
  background: var(--win-stage); cursor: grab; overflow: hidden;
}
/* The picture sits a few points inside the stage with its own corners, so it reads as a print
   laid on the well rather than paint flooding it. It is sized
   by max-width/max-height, not object-fit, so the radius lands on the picture's real edge. */
.s-hero > img {
  position: absolute; inset: 10px; margin: auto; display: block;
  width: auto; height: auto; max-width: calc(100% - 20px); max-height: calc(100% - 20px);
  border-radius: 6px;
}
.s-hero::after { content: ""; position: absolute; inset: 0; border-radius: inherit; box-shadow: inset 0 0 0 var(--hairline) var(--win-media-rim); pointer-events: none; }
.s-hero[data-empty] { display: grid; place-items: center; padding: 0 28px; background: none; cursor: default; text-align: center; }
.s-hero[data-empty]::after { box-shadow: none; border: 1px dashed var(--win-line-strong); }
.s-hero[data-empty] .s-empty-note { margin: 0; padding: 0; }
.s-hero[data-empty] .s-empty-note > g-icon { display: block; margin: 0 auto 8px; color: var(--win-text-3); }
.s-hero-text { position: absolute; inset: 0; padding: 12px 14px; background: var(--win-raised); color: var(--win-text-2); font: 500 11px/17px var(--font-mono); white-space: pre-line; overflow: hidden; }
.s-hero-text > b { display: flex; align-items: center; gap: 5px; margin-bottom: 6px; font: var(--type-caption-strong); font-family: var(--font); color: var(--win-iris-text); }
.s-play { position: absolute; left: 50%; top: 50%; z-index: 1; display: grid; place-items: center; width: 44px; height: 44px; margin: -22px 0 0 -22px; padding: 0; border: 0; border-radius: 50%; font: inherit; }
.s-play:focus-visible { outline: 2px solid var(--surface-focus); outline-offset: 2px; }
.s-hero-dur { position: absolute; left: 8px; bottom: 8px; z-index: 1; pointer-events: none; }
.s-hero > .g-status { position: absolute; right: 8px; top: 8px; z-index: 1; }

.s-strip { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 6px; margin-top: 8px; }
.s-thumb {
  position: relative; display: block; width: 100%; height: 36px; border-radius: var(--radius-thumb);
  background: var(--win-sunken); cursor: grab;
  transition: box-shadow var(--dur-fast) var(--ease-out);
}
.s-thumb-media { position: absolute; inset: 0; border-radius: inherit; overflow: hidden; }
.s-thumb-media > img { display: block; width: 100%; height: 100%; object-fit: cover; }
.s-thumb::after { content: ""; position: absolute; inset: 0; border-radius: inherit; box-shadow: inset 0 0 0 0.5px var(--win-line-strong); pointer-events: none; }
/* A press picks a thumbnail (passing over it would swap the preview while the pointer travels
   from the preview to the actions), and the pick shows as the ring alone. */
.s-thumb[aria-selected="true"] { box-shadow: 0 0 0 2px var(--iris); }
.s-thumb:focus { outline: 0; }
.s-thumb:focus-visible { outline: 2px solid var(--surface-focus); outline-offset: 3px; }
/* The length sits on the picture itself, so its shadow keeps it readable on a light frame. */
.s-thumb-dur {
  position: absolute; right: 4px; bottom: 2px; z-index: 1; pointer-events: none;
  font: 600 9.5px/12px var(--font-mono); font-variant-numeric: tabular-nums; color: #fff;
  text-shadow: 0 0 2px rgba(0, 0, 0, 0.9), 0 1px 2px rgba(0, 0, 0, 0.6);
}
/* The upload badge hangs over the bottom-right corner; the length steps left of it. */
.s-thumb[data-badge] > .s-thumb-dur { right: 15px; }
.s-thumb > .g-status { position: absolute; right: -5px; bottom: -5px; z-index: 1; box-shadow: 0 0 0 2px var(--win-material-solid); }
.s-thumb > .g-status[data-status="progress"] { right: -7px; bottom: -7px; }
.s-thumb[data-empty] { background: none; cursor: default; }
.s-thumb[data-empty]::after { box-shadow: none; border: 1px dashed var(--win-line-strong); }
.s-thumb-text { display: grid; place-items: center; background: var(--win-raised); color: var(--win-iris-text); }

.s-info { margin-top: 12px; padding: 0 4px; }
.s-info-title { display: block; font: var(--type-title); color: var(--win-text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.s-info-sub { display: block; margin-top: 1px; font: var(--type-body); color: var(--win-text-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-variant-numeric: tabular-nums; }
.s-info-sub[data-kind="link"] { font: 500 11.5px/16px var(--font-mono); color: var(--win-iris-text); }
.s-acts { display: flex; align-items: center; gap: 2px; margin-top: 10px; margin-left: -4px; }
/* Text buttons: the picture above already carries the weight, so the actions stay light; the
   row is pulled left so the first label lines up with the name above it, and Copy alone keeps
   iris as the one primary action. */
.s-act { padding: 0 8px; color: var(--win-text); }
.s-act[data-main] { color: var(--win-iris-text); }
.s-acts > .g-spacer { flex: 1; }
.s-empty-note { margin: 10px 0 2px; padding: 0 8px; }
.s-empty-note b { display: block; font: var(--type-button); color: var(--win-text); }
.s-empty-note > span { display: block; margin-top: 2px; font: var(--type-body); color: var(--win-text-2); }
.s-empty-note .g-keys { display: inline-flex; margin: 0 3px; vertical-align: -3px; }
.s-empty-note.s-row { display: flex; align-items: center; gap: 8px; }
.s-empty-note.s-row > span { flex: 1; margin: 0; }
/* The notice fades in while the panel grows to make room for it, and out before it shrinks. */
.s-fade-enter-active, .s-fade-leave-active { transition: opacity var(--dur-base) var(--ease-out), transform var(--dur-base) var(--ease-out); }
.s-fade-enter-from, .s-fade-leave-to { opacity: 0; transform: translateY(-3px); }
.s-notice { display: flex; align-items: center; gap: 6px; margin: 8px 0 0; padding: 0 8px; font: var(--type-body); color: var(--win-text-2); }
.s-notice[data-tone="warn"] > g-icon { color: var(--win-warn); }
.s-notice[data-tone="ok"] > g-icon { color: var(--win-ok); }

.s-note { display: flex; gap: 10px; padding: 10px 12px 12px 10px; border-radius: var(--radius-field); background: var(--win-warn-soft); }
.s-note-icon { flex: none; display: grid; place-items: center; width: 20px; height: 18px; color: var(--win-warn); }
.s-note[data-tone="iris"] { background: var(--win-selection-soft); }
.s-note[data-tone="iris"] .s-note-icon { color: var(--win-iris-text); }
.s-note-text { flex: 1; min-width: 0; }
.s-note-title { display: block; font: var(--type-title); color: var(--win-text); }
.s-note-body { margin: 2px 0 0; font: var(--type-body); color: var(--win-text-2); }
.s-note-actions { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 10px; }

.s-rec > .g-bar { display: flex; width: 100%; }
.s-rec-what { flex: 1; min-width: 0; padding: 0 6px 0 4px; font: var(--type-label); color: var(--text-muted); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.s-rec-note { display: flex; align-items: center; gap: 6px; margin: 8px 0 0; padding: 0 8px; font: var(--type-body); color: var(--win-text-2); }
.s-rec-note > g-icon { color: var(--win-text-2); }
.s-rec-note .g-ring { --ring-size: 14px; }
</style>
