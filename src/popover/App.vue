<script setup lang="ts">
// The menu bar popover (DESIGN.md "Popover"): the capture rows with their live shortcuts, the
// one-shot delay, the five latest captures with a shelf of actions, and History, Settings and
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
import SegmentedControl from "../components/SegmentedControl.vue";
import TipWrap from "../components/TipWrap.vue";
import {
  CAPTURE_COUNTDOWN,
  capture_cancel_delay,
  capture_delay,
  capture_set_delay,
  capture_start,
  type CaptureMode,
  type DelayState,
} from "../lib/api/capture";
import { on } from "../lib/api/core";
import {
  DEFAULT_HISTORY_QUERY,
  entryName,
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
import type { Settings, Shortcuts } from "../lib/settings";
import { useUpdate } from "../lib/update";
import { codeOf, reasonText } from "../toast/reason";
import FirstUploadDialog, { type DialogFile } from "../upload/FirstUploadDialog.vue";
import { prepareUpload } from "../upload/flow";

/** How many captures the strip shows. */
const RECENT = 5;
/** How far the pointer moves with the button held before a thumbnail is dragged out as a file. */
const DRAG_SLOP = 4;

/** A capture mode, or `record`, which opens the recording's setup. */
type Row = { mode: CaptureMode | "record"; icon: string; label: Key; shortcut: ShortcutAction };
const ROWS: (Row | null)[] = [
  { mode: "area", icon: "area", label: "popover.capture.area", shortcut: "captureArea" },
  { mode: "window", icon: "window", label: "popover.capture.window", shortcut: "captureWindow" },
  { mode: "display", icon: "display", label: "popover.capture.display", shortcut: "captureDisplay" },
  { mode: "repeatLastArea", icon: "repeat", label: "popover.capture.repeat", shortcut: "repeatLastArea" },
  null,
  { mode: "copyText", icon: "ocr", label: "popover.capture.copyText", shortcut: "copyText" },
  { mode: "record", icon: "record", label: "popover.capture.record", shortcut: "recordScreen" },
];

const DELAYS = [0, 3, 5, 10] as const;

// ------------------------------------------------------------------------------------ state

const root = ref<HTMLElement | null>(null);
const shortcuts = ref<Shortcuts | null>(null);
const defaultProvider = ref<string | null>(null);
const entries = ref<Entry[]>([]);
const historyState = ref<"loading" | "ready" | "unavailable" | "failed">("loading");
const sel = ref(0);
const permissions = ref<Permissions | null>(null);
const delay = ref<DelayState>({ pendingSeconds: 0, remainingSeconds: null, mode: null });
const recording = ref<RecordingState>({ kind: "idle" });
const elapsedMs = ref(0);
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
const selected = computed<Entry | null>(() => entries.value[sel.value] ?? null);
const slots = computed(() => Array.from({ length: RECENT }, (_, i) => entries.value[i] ?? null));

const delayOptions = computed(() =>
  DELAYS.map((value) => ({ value, label: value === 0 ? t("popover.delay.off") : t("popover.delay.seconds", { n: value }) })),
);

// ---------------------------------------------------------------------------------- loading

async function loadSettings(settings?: Settings): Promise<void> {
  try {
    const s = settings ?? (await settings_get());
    shortcuts.value = s.shortcuts;
    defaultProvider.value = s.upload.defaultProvider;
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
    if (state.kind === "recording" || state.kind === "paused") elapsedMs.value = state.elapsedMs;
  } catch (error) {
    console.warn("record_state", error);
  }
}

async function opened(): Promise<void> {
  notice.value = null;
  // A question left open when the panel closed is not asked again on its own.
  asking.value = null;
  sel.value = 0;
  await Promise.all([loadSettings(), loadHistory(), loadPermissions(), loadDelay(), loadRecording()]);
  await nextTick();
  root.value?.focus({ preventScroll: true });
}

let historyTimer: ReturnType<typeof setTimeout> | null = null;
function historyChanged(): void {
  if (historyTimer) clearTimeout(historyTimer);
  historyTimer = setTimeout(() => void loadHistory(), 150);
}

// ------------------------------------------------------------------------------- the window

let lastHeight = 0;
function fit(): void {
  const height = Math.ceil(root.value?.getBoundingClientRect().height ?? 0);
  if (!height || height === lastHeight) return;
  lastHeight = height;
  popover_fit(height).catch((error) => console.warn("popover_fit", error));
}

function hide(): void {
  popover_hide().catch((error) => console.warn("popover_hide", error));
}

function fail(error: unknown): void {
  notice.value = { tone: "warn", text: reasonText(codeOf(error)) };
}

// --------------------------------------------------------------------------------- capturing

const caps = (row: Row) => capsText(shortcuts.value?.[row.shortcut]);

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
  try {
    delay.value = await capture_set_delay(seconds);
  } catch (error) {
    fail(error);
  }
}

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

/** The file an upload or a reveal works on: the saved copy, or the history's own. */
const pathOf = (entry: Entry) => entry.savedPath ?? entry.file;

type ActId = "copy" | "upload" | "link" | "cancel" | "pin" | "editor" | "trim" | "folder";
type Act = { id: ActId; icon: string; label: Key; caps: string[] };

const ACT: Record<ActId, Omit<Act, "id">> = {
  copy: { icon: "copy", label: "popover.act.copy", caps: ["⌘", "C"] },
  upload: { icon: "upload", label: "popover.act.upload", caps: ["⌘", "U"] },
  link: { icon: "link", label: "popover.act.link", caps: ["⇧", "⌘", "L"] },
  cancel: { icon: "close", label: "popover.act.cancel", caps: ["⌘", "."] },
  pin: { icon: "pin", label: "popover.act.pin", caps: ["⌘", "P"] },
  editor: { icon: "external-link", label: "popover.act.editor", caps: ["⌘", "O"] },
  trim: { icon: "trim", label: "popover.act.trim", caps: ["⌘", "T"] },
  folder: { icon: "folder", label: "popover.act.folder", caps: ["⌥", "⌘", "R"] },
};

/** The actions that work for this entry now; a control that could not work is left out. */
function actionsFor(entry: Entry): Act[] {
  const ids: ActId[] = ["copy"];
  if (uploads.has(entry.id)) ids.push("cancel");
  else if (entry.hosted) ids.push("link");
  else if (entry.kind !== "textCopy" && defaultProvider.value && pathOf(entry)) ids.push("upload");
  if (entry.kind === "screenshot") ids.push("pin", "editor");
  // A recording offers Trim where a screenshot offers the editor (DESIGN.md, Popover).
  if (entry.kind === "recording") ids.push("trim");
  if (entry.savedPath) ids.push("folder");
  return ids.map((id) => ({ id, ...ACT[id] }));
}

async function learnProviders(): Promise<void> {
  if (providerNames.size) return;
  try {
    for (const provider of await upload_providers()) providerNames.set(provider.id, provider.name);
  } catch {
    // The id stands in for the name.
  }
}

async function run(id: ActId, entry: Entry): Promise<void> {
  notice.value = null;
  try {
    switch (id) {
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

function rowButtons(): HTMLElement[] {
  return [...(root.value?.querySelectorAll<HTMLElement>(".g-menu-item[data-row]:not([aria-disabled='true'])") ?? [])];
}

function onKey(event: KeyboardEvent): void {
  const active = document.activeElement as HTMLElement | null;
  const onThumb = !!active?.closest(".s-thumb");
  const mod = event.metaKey || event.ctrlKey;

  if (event.key === "Escape") {
    event.preventDefault();
    hide();
    return;
  }

  if (mod) {
    const k = event.key.toLowerCase();
    const entry = selected.value;
    const can = (id: ActId) => (entry && actionsFor(entry).some((a) => a.id === id) ? entry : null);
    let handled = true;
    if (k === "y" && !event.shiftKey) void openWindow("history");
    else if (k === ",") void openWindow("settings");
    else if (k === "q") void app_quit();
    else if (k === "c" && !event.shiftKey && can("copy")) void run("copy", entry!);
    else if (k === "u" && can("upload")) void run("upload", entry!);
    else if (k === "l" && event.shiftKey && can("link")) void run("link", entry!);
    else if (k === "." && can("cancel")) void run("cancel", entry!);
    else if (k === "p" && can("pin")) void run("pin", entry!);
    else if (k === "o" && can("editor")) void run("editor", entry!);
    else if (k === "t" && can("trim")) void run("trim", entry!);
    else if ((k === "r" || event.code === "KeyR") && event.altKey && can("folder")) void run("folder", entry!);
    else handled = false;
    if (handled) event.preventDefault();
    return;
  }

  if (event.key === "ArrowLeft" || event.key === "ArrowRight") {
    if (!onThumb) return;
    event.preventDefault();
    select(sel.value + (event.key === "ArrowRight" ? 1 : -1), true);
    return;
  }

  if (event.key === "ArrowDown" || event.key === "ArrowUp") {
    const rows = rowButtons();
    const down = event.key === "ArrowDown";
    const at = active ? rows.indexOf(active) : -1;
    if (onThumb) {
      if (!down && rows.length) {
        event.preventDefault();
        rows[rows.length - 1]!.focus();
      }
      return;
    }
    if (at === -1 && active?.closest(".g-seg")) return;
    event.preventDefault();
    if (at === -1) {
      rows[down ? 0 : rows.length - 1]?.focus();
    } else if (down && at === rows.length - 1) {
      if (entries.value.length) select(sel.value, true);
    } else {
      rows[Math.max(0, Math.min(rows.length - 1, at + (down ? 1 : -1)))]?.focus();
    }
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
  if (root.value) observer.observe(root.value);
  fit();
  stops.push(
    await on<null>(POPOVER_OPENED, () => void opened()),
    await on<Settings>(SETTINGS_CHANGED, (settings) => void loadSettings(settings)),
    await on<Permissions>(PERMISSIONS_CHANGED, (next) => (permissions.value = next)),
    await on<DelayState>(CAPTURE_COUNTDOWN, (state) => (delay.value = state)),
    await onHistoryChanged(historyChanged),
    await onRecordState((state) => {
      recording.value = state;
      if (state.kind === "recording" || state.kind === "paused") elapsedMs.value = state.elapsedMs;
      if (state.kind === "idle") elapsedMs.value = 0;
    }),
    await onRecordTick((tick) => (elapsedMs.value = tick.elapsedMs)),
    await onUploadProgress((p) => uploadProgress(p.uploadId, p.provider, p.sentBytes, p.totalBytes)),
    await onUploadDone((done) => uploadEnded(done.result.uploadId)),
    await onUploadFailed((failed) => uploadEnded(failed.uploadId)),
  );
  await opened();
});

onUpdated(fit);
// The first-upload dialog replaces the panel's element, so the observer follows the new one.
watch(root, (element, previous) => {
  if (previous) observer?.unobserve(previous);
  if (element) observer?.observe(element);
  fit();
});

onUnmounted(() => {
  observer?.disconnect();
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
  <div v-else ref="root" class="g-popover solid s-pop" role="dialog" aria-label="Glimpsfy" tabindex="-1" @keydown="onKey">
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

    <!-- capture rows and the one-shot delay -->
    <template v-else>
      <div class="g-popover-section" role="menu" :aria-label="t('popover.capture.label')">
        <template v-for="(row, i) in ROWS" :key="row ? row.mode : `sep-${i}`">
          <div v-if="!row" class="g-menu-separator" role="separator" />
          <button
            v-else
            type="button"
            class="g-menu-item"
            role="menuitem"
            data-row
            :aria-disabled="blocked ? 'true' : undefined"
            @click="capture(row)"
          >
            <GIcon :name="row.icon" :size="16" />
            <span>{{ t(row.label) }}</span>
            <span v-if="caps(row)" class="g-menu-key">{{ caps(row) }}</span>
          </button>
        </template>
      </div>
      <div class="g-popover-section">
        <div v-if="delay.remainingSeconds !== null" class="s-delay" role="status">
          <span class="s-delay-label"><GIcon name="timer" :size="16" />{{ t("popover.delay.counting", { n: delay.remainingSeconds }) }}</span>
          <button type="button" class="g-button" data-variant="plain" @click="cancelDelay">{{ t("popover.delay.cancel") }}</button>
        </div>
        <div v-else class="s-delay" :aria-disabled="blocked ? 'true' : undefined">
          <span id="pop-delay" class="s-delay-label"><GIcon name="timer" :size="16" />{{ t("popover.delay.label") }}</span>
          <SegmentedControl
            :model-value="delay.pendingSeconds"
            :options="delayOptions"
            labelledby="pop-delay"
            :disabled="blocked"
            @update:model-value="setDelay"
          />
        </div>
      </div>
    </template>

    <!-- recent captures -->
    <div class="g-popover-section">
      <div class="g-popover-label">{{ t("popover.recent") }}</div>
      <p v-if="historyState === 'unavailable'" class="s-empty-note">
        <span>{{ t("popover.recent.unavailable") }}</span>
      </p>
      <div v-else-if="historyState === 'failed'" class="s-empty-note s-row">
        <span>{{ t("popover.recent.failed") }}</span>
        <button type="button" class="g-button" data-variant="plain" @click="loadHistory">{{ t("popover.recent.retry") }}</button>
      </div>
      <template v-else>
        <div
          class="s-strip"
          role="listbox"
          :aria-label="t('popover.recent.aria')"
          aria-orientation="horizontal"
          @pointermove="thumbMove"
          @pointerup="thumbUp"
          @pointercancel="thumbUp"
        >
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
              @pointerenter="select(i)"
              @pointerdown="thumbDown($event, entry, i)"
              @focus="select(i)"
              @dblclick="open(entry)"
            >
              <span v-if="entry.kind === 'textCopy'" class="s-thumb-media s-thumb-text"><GIcon name="ocr" :size="20" /></span>
              <span v-else class="s-thumb-media">
                <img v-if="thumbUrl(entry)" :src="thumbUrl(entry)!" alt="" draggable="false" />
              </span>
              <span v-if="entry.kind === 'recording' && entry.durationMs !== null" class="g-tag">
                <GIcon name="play" :size="12" />{{ duration(entry.durationMs) }}
              </span>
              <span v-if="uploads.has(entry.id)" class="g-status" data-status="progress">
                <ProgressRing :progress="uploadPercent(entry)" :label="sub(entry)" :size="22" />
              </span>
              <span v-else-if="entry.hosted" class="g-status" data-status="ok"><GIcon name="link" :size="12" /></span>
            </button>
            <span v-else class="s-thumb" data-empty aria-hidden="true" />
          </template>
        </div>

        <p v-if="historyState === 'ready' && !entries.length" class="s-empty-note">
          <b>{{ t("popover.empty.title") }}</b>
          <span v-if="emptyKeys.length">
            {{ emptyParts.before }}<KeyCaps :caps="emptyKeys" />{{ emptyParts.after }}
          </span>
          <span v-else>{{ t("popover.empty.noKeys") }}</span>
        </p>
        <template v-else-if="selected">
          <div class="s-shelf">
            <div class="s-shelf-text">
              <span class="s-shelf-title">{{ title(selected) }}</span>
              <span class="s-shelf-sub" :data-kind="selected.hosted && !uploads.has(selected.id) ? 'link' : undefined">{{ sub(selected) }}</span>
            </div>
            <div class="s-shelf-actions" role="toolbar" :aria-label="t('popover.actions.aria')">
              <TipWrap
                v-for="(act, j) in actionsFor(selected)"
                :key="act.id"
                :tip="t(act.label)"
                :caps="act.caps"
                :align="j >= actionsFor(selected).length - 2 ? 'end' : undefined"
              >
                <button type="button" class="g-btn" data-size="small" :aria-label="t(act.label)" @click="run(act.id, selected)">
                  <GIcon :name="act.icon" :size="16" />
                </button>
              </TipWrap>
            </div>
          </div>
          <p class="s-hint"><GIcon name="drop" :size="14" /><span>{{ t("popover.hint") }}</span></p>
        </template>
      </template>
      <p v-if="notice" class="s-notice" :data-tone="notice.tone" role="status">
        <GIcon :name="notice.tone === 'ok' ? 'check' : 'warning'" :size="14" /><span>{{ notice.text }}</span>
      </p>
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
</template>

<style>
/* Page-local parts of the popover, ported from design/round-2/small-surfaces (s-*). Every value
   is a kit token. The popover window is its own page, so these rules reach nothing else. */
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
.s-pop :where(.s-thumb, .s-shelf, .s-note, .s-delay, .s-strip) {
  box-sizing: border-box;
}
.s-delay { display: flex; align-items: center; gap: 12px; min-height: 32px; padding-left: 8px; }
.s-delay-label { flex: 1; display: inline-flex; align-items: center; gap: 8px; font: var(--type-text); color: var(--win-text); white-space: nowrap; }
.s-delay-label > g-icon { color: var(--win-text-2); }
.s-delay[aria-disabled="true"] .s-delay-label,
.s-delay[aria-disabled="true"] .s-delay-label > g-icon { color: var(--win-text-disabled); }

.s-strip { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: 6px; padding: 2px 4px 0; }
button.s-thumb { margin: 0; padding: 0; border: 0; font: inherit; color: inherit; -webkit-appearance: none; appearance: none; }
.s-thumb {
  position: relative; display: block; width: 100%; height: 40px; border-radius: var(--radius-thumb);
  background: var(--win-sunken); cursor: grab;
  transition: box-shadow var(--dur-fast) var(--ease-out), transform var(--dur-fast) var(--ease-out);
}
.s-thumb-media { position: absolute; inset: 0; border-radius: inherit; overflow: hidden; }
.s-thumb-media > img { display: block; width: 100%; height: 100%; object-fit: cover; }
.s-thumb::after { content: ""; position: absolute; inset: 0; border-radius: inherit; box-shadow: inset 0 0 0 0.5px var(--win-line-strong); pointer-events: none; }
.s-thumb:not([data-empty]):hover { transform: translateY(-1px); box-shadow: var(--win-shadow-lift); }
.s-thumb[aria-selected="true"] { box-shadow: 0 0 0 2px var(--iris); }
.s-thumb[aria-selected="true"]:hover { box-shadow: 0 0 0 2px var(--iris), var(--win-shadow-lift); }
.s-thumb:focus { outline: 0; }
.s-thumb:focus-visible { outline: 2px solid var(--surface-focus); outline-offset: 3px; }
.s-thumb > .g-tag { position: absolute; left: 3px; bottom: -8px; z-index: 1; }
.s-thumb > .g-status { position: absolute; right: -5px; bottom: -5px; z-index: 1; box-shadow: 0 0 0 2px var(--win-material-solid); }
.s-thumb > .g-status[data-status="progress"] { right: -7px; bottom: -7px; }
.s-thumb[data-empty] { background: none; cursor: default; }
.s-thumb[data-empty]::after { box-shadow: none; border: 1px dashed var(--win-line-strong); }
.s-thumb-text { display: grid; place-items: center; background: var(--win-raised); color: var(--win-iris-text); }

.s-shelf { display: flex; align-items: center; gap: 8px; min-height: 40px; margin-top: 10px; padding-left: 8px; }
.s-shelf-text { flex: 1; min-width: 0; }
.s-shelf-title { display: block; font: var(--type-button); color: var(--win-text); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.s-shelf-sub { display: block; margin-top: 1px; font: var(--type-body); color: var(--win-text-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-variant-numeric: tabular-nums; }
.s-shelf-sub[data-kind="link"] { font: 500 11.5px/16px var(--font-mono); color: var(--win-iris-text); }
.s-shelf-actions { display: inline-flex; flex: none; }
.s-hint { display: flex; align-items: center; gap: 6px; margin: 4px 0 0; padding: 0 8px; font: var(--type-caption); color: var(--win-text-3); }
.s-empty-note { margin: 10px 0 2px; padding: 0 8px; }
.s-empty-note b { display: block; font: var(--type-button); color: var(--win-text); }
.s-empty-note > span { display: block; margin-top: 2px; font: var(--type-body); color: var(--win-text-2); }
.s-empty-note .g-keys { display: inline-flex; margin: 0 3px; vertical-align: -3px; }
.s-empty-note.s-row { display: flex; align-items: center; gap: 8px; }
.s-empty-note.s-row > span { flex: 1; margin: 0; }
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
