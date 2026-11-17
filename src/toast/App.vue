<script setup lang="ts">
// The toast window: a stack at the top-right corner of the display the capture was taken on, the
// newest on top. Capture toasts come from `toast://show`; upload toasts follow `upload://progress`,
// `upload://done` and `upload://failed`; an export toast follows `record://export-progress` while
// the trim window that started it is closed, since the window shows its own progress otherwise. A
// success leaves on its own after a few seconds (the timers pause while the pointer rests on the
// stack), a failure stays until it is dismissed, and every toast has a dismiss button. The window
// takes the stack's height (`toast_fit`) and hides when the stack is empty.
import { nextTick, onMounted, onUnmounted, reactive, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import ProgressRing from "../components/ProgressRing.vue";
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
import { app_copy_text, toast_fit, window_open_settings, type Toast } from "../lib/api/windows";
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
/** The kit's leave animation (`--dur-base`). */
const LEAVE_MS = 220;
/** Room around the stack for the glass shadow. */
const MARGIN = 16;
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
  state: "enter" | "leave" | "";
  capture: Toast | null;
  upload: Upload | null;
  export?: Export | null;
};

const items = ref<Item[]>([]);
const stack = ref<HTMLElement | null>(null);
const providerNames = reactive(new Map<string, string>());

// -------------------------------------------------------------------------------- the window

let lastHeight = -1;
async function fit(): Promise<void> {
  await nextTick();
  const height = items.value.length && stack.value ? Math.ceil(stack.value.offsetHeight + 2 * MARGIN) : 0;
  if (height === lastHeight) return;
  lastHeight = height;
  await toast_fit(height).catch((error) => console.warn("toast_fit", error));
}

// ---------------------------------------------------------------------------------- timers

type Timer = { remaining: number; startedAt: number; handle: ReturnType<typeof setTimeout> | null };
const timers = new Map<string, Timer>();
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

function find(key: string): Item | undefined {
  return items.value.find((x) => x.key === key);
}

function dismiss(key: string): void {
  clearTimer(key);
  const item = find(key);
  if (!item || item.state === "leave") return;
  item.state = "leave";
  setTimeout(() => {
    items.value = items.value.filter((x) => x.key !== key);
    void fit();
  }, LEAVE_MS);
}

function add(item: Item): void {
  items.value.unshift(item);
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
  add({ key, state: "enter", capture: toast, upload: null });
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
  if (!item || item.state === "leave") {
    if (p.fraction >= 1) return;
    if (await trimWindowShown()) return;
    // A newer report may have added the toast while this one waited for the window.
    item = find(key);
    if (!item || item.state === "leave") {
      add({ key, state: "enter", capture: null, upload: null, export: { recordingId: p.recordingId, format: p.format, fraction: 0, remainingMs: null } });
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
  if (ms == null) return t("toast.export.running", { p });
  if (ms < 2000) return t("toast.export.almost", { p });
  const s = Math.ceil(ms / 1000);
  return s < 90 ? t("toast.export.leftSeconds", { p, n: s }) : t("toast.export.leftMinutes", { p, n: Math.round(s / 60) });
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
  if (!item || item.state === "leave") {
    clearTimer(key);
    if (item) items.value = items.value.filter((x) => x !== item);
    item = {
      key,
      state: "enter",
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

function title(item: Item): string {
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
});

onUnmounted(() => {
  observer?.disconnect();
  stops.forEach((stop) => stop());
  for (const key of [...timers.keys()]) clearTimer(key);
  for (const handle of exportQuiet.values()) clearTimeout(handle);
});
</script>

<template>
  <div ref="stack" class="g-toasts stack" @mouseenter="pause" @mouseleave="resume">
    <div
      v-for="item in items"
      :key="item.key"
      class="g-toast"
      :role="status(item) === 'warn' ? 'alert' : 'status'"
      :data-state="item.state || undefined"
    >
      <div class="g-toast-thumb">
        <img v-if="thumbOf(item)" :src="thumbOf(item)!" alt="" draggable="false" />
        <span v-else class="thumb-icon"><GIcon :name="placeholderIcon(item)" :size="20" /></span>
        <span v-if="status(item) === 'progress'" class="g-status" data-status="progress">
          <ProgressRing :progress="ringProgress(item)" :label="t(item.export ? 'toast.export.ring' : 'toast.upload.ring')" :size="22" />
        </span>
        <span v-else class="g-status" :data-status="status(item)">
          <GIcon :name="status(item) === 'warn' ? 'bang' : item.upload ? 'link' : 'check'" :size="12" />
        </span>
      </div>

      <!-- capture toasts -->
      <template v-if="item.capture">
        <div class="g-toast-text">
          <div class="g-toast-title">{{ title(item) }}</div>
          <template v-if="item.capture.kind === 'saved' || item.capture.kind === 'diskFull'">
            <div class="g-toast-body" data-kind="mono">{{ fileName(item.capture.path) }}</div>
            <div v-if="item.capture.kind === 'diskFull'" class="g-toast-body" data-kind="wrap">
              {{ t("toast.diskFullSaved", { folder: item.capture.folder ?? "" }) }}
            </div>
            <div v-else-if="savedFallback(item.capture)" class="g-toast-body" data-kind="wrap" :title="item.capture.fallbackFrom ?? undefined">
              {{ savedFallback(item.capture) }}
            </div>
            <div v-else class="g-toast-body">{{ meta(item.capture) }}</div>
          </template>
          <div v-else-if="item.capture.kind === 'copied'" class="g-toast-body">{{ meta(item.capture) }}</div>
          <div v-else-if="item.capture.kind === 'textCopied'" class="g-toast-body">{{ quote(item.capture.text) }}</div>
          <div v-else class="g-toast-body" data-kind="wrap" :title="item.capture.message ?? undefined">
            {{ failureText(item.capture.errorCode) }}
          </div>
        </div>
        <div class="g-toast-actions">
          <button
            v-if="(item.capture.kind === 'saved' || item.capture.kind === 'diskFull') && item.capture.path"
            type="button"
            class="g-button"
            @click="output_reveal(item.capture.path!)"
          >
            {{ t("toast.showInFinder") }}
          </button>
          <button
            v-if="item.capture.kind === 'failed' && item.capture.errorCode === 'permission_denied'"
            type="button"
            class="g-button"
            data-variant="primary"
            @click="permissions_open_settings('screenRecording')"
          >
            {{ t("toast.openSettings") }}
          </button>
          <button type="button" class="g-btn" data-size="small" :aria-label="t('toast.dismiss')" @click="dismiss(item.key)">
            <GIcon name="close" :size="16" />
          </button>
        </div>
      </template>

      <!-- export toasts, while the trim window is closed -->
      <template v-else-if="item.export">
        <div class="g-toast-text">
          <div class="g-toast-title">{{ title(item) }}</div>
          <div class="g-toast-body">{{ exportText(item.export) }}</div>
        </div>
        <div class="g-toast-actions">
          <button type="button" class="g-button" @click="cancelExport(item)">{{ t("toast.upload.cancel") }}</button>
          <button type="button" class="g-btn" data-size="small" :aria-label="t('toast.dismiss')" @click="dismiss(item.key)">
            <GIcon name="close" :size="16" />
          </button>
        </div>
      </template>

      <!-- upload toasts -->
      <template v-else-if="item.upload">
        <div class="g-toast-text">
          <div class="g-toast-title">{{ title(item) }}</div>
          <div v-if="item.upload.phase === 'uploading'" class="g-toast-body">{{ progressText(item.upload) }}</div>
          <div v-else-if="item.upload.phase === 'done'" class="g-toast-body" data-kind="link">{{ item.upload.done?.result.url }}</div>
          <template v-else>
            <div class="g-toast-body" data-kind="wrap">{{ uploadFailureText(item.upload) }}</div>
            <div v-if="hostReason(item.upload)" class="g-toast-body" data-kind="mono" :title="hostReason(item.upload) ?? undefined">
              {{ hostReason(item.upload) }}
            </div>
          </template>
        </div>
        <div class="g-toast-actions">
          <button
            v-if="item.upload.phase === 'uploading'"
            type="button"
            class="g-button"
            :aria-label="t('upload.state.cancel')"
            @click="cancel(item.upload)"
          >
            {{ t("toast.upload.cancel") }}
          </button>
          <button v-else-if="item.upload.phase === 'done'" type="button" class="g-button" @click="copyLink(item.upload)">
            {{ item.upload.copiedNow ? t("toast.upload.copiedNow") : t("toast.upload.copy") }}
          </button>
          <button
            v-else-if="needsSettings(item.upload)"
            type="button"
            class="g-button"
            data-variant="primary"
            @click="openUploadSettings(item)"
          >
            {{ t("toast.upload.settings") }}
          </button>
          <button
            v-else-if="canRetry(item.upload)"
            type="button"
            class="g-button"
            data-variant="primary"
            @click="retry(item)"
          >
            {{ t("toast.upload.retry") }}
          </button>
          <button type="button" class="g-btn" data-size="small" :aria-label="t('toast.dismiss')" @click="dismiss(item.key)">
            <GIcon name="close" :size="16" />
          </button>
        </div>
      </template>
    </div>
  </div>
</template>

<style scoped>
.stack {
  position: absolute;
  top: 16px;
  right: 16px;
}
.thumb-icon {
  display: grid;
  place-items: center;
  background: var(--glass-solid);
  color: var(--iris-text);
}
</style>
