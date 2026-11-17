<script setup lang="ts">
// The trim and export window (`recorder`, design ab6, ab7 and the live ab9): the finished take
// plays in a `<video>` served by `glimpsfy-recording://`, the timeline cuts its start and end, and
// the export panel writes an MP4 or a GIF of the kept part. Save (⌘S) writes into the recordings
// folder, Copy (⌘C) puts the file on the clipboard and Upload (⌘U) goes through the upload area's
// consent first; the core reports a finished save or copy with its own toast. During an export the
// options and handles lock and the footer shows the progress; closing the window loses nothing,
// because the export runs on in the core. Discard (⌘⌫) asks, then moves the take to the Trash and
// closes the window. A new recording (`record://trim`) replaces the one on show.
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import { computed, onMounted, onUnmounted, reactive, ref, shallowRef, watch } from "vue";

import ConfirmDialog from "../components/ConfirmDialog.vue";
import GIcon from "../components/GIcon.vue";
import TipWrap from "../components/TipWrap.vue";
import {
  onRecordExportProgress,
  onRecordTrim,
  record_export,
  record_export_cancel,
  record_trash,
  record_trim_current,
  recordingFrameUrl,
  type ExportProgress,
  type ExportPurpose,
  type ExportResult,
  type RecordingInfo,
} from "../lib/api/record";
import { settings_get } from "../lib/api/settings";
import {
  onUploadDone,
  onUploadFailed,
  onUploadProgress,
  upload_file,
  type ConsentInfo,
  type ProviderId,
  type UploadDone,
  type UploadFailed,
  type UploadId,
} from "../lib/api/upload";
import { window_open_settings } from "../lib/api/windows";
import { t } from "../lib/i18n";
import type { Settings } from "../lib/settings";
import FirstUploadDialog, { type DialogFile } from "../upload/FirstUploadDialog.vue";
import { prepareUpload } from "../upload/flow";
import { doneText, failureText, issueText } from "../upload/text";
import { exportFormat, exportOptions, initialChoice, keptSources, withEdge, type ExportChoice } from "./choice";
import { estimateExport } from "./estimate";
import ExportPanel from "./ExportPanel.vue";
import { codeOf, createdAt, errorText, tenths, type ErrorPlace } from "./format";
import { clamp } from "./geometry";
import Timeline from "./Timeline.vue";

const info = shallowRef<RecordingInfo | null>(null);
const loaded = ref(false);
const settings = shallowRef<Settings | null>(null);

const range = reactive({ startMs: 0, endMs: 0 });
const playMs = ref(0);
const playing = ref(false);
const loop = ref(false);
const muted = ref(false);
const playFailed = ref(false);

const choice = reactive<ExportChoice>({
  format: "mp4",
  resolution: "native",
  quality: "medium",
  microphone: true,
  system: true,
  gifFps: 15,
  gifWidth: 640,
  loopForever: true,
});

type Job = { token: number; recordingId: string; format: "mp4" | "gif"; fraction: number; remainingMs: number | null; adopted: boolean };
const job = ref<Job | null>(null);
const problem = ref<string | null>(null);
/** Guidance that sends the user to Settings › Upload. */
const note = ref<string | null>(null);
const preparing = ref(false);
const confirming = ref(false);
const trashing = ref(false);
const asking = shallowRef<ConsentInfo | null>(null);

type UploadView = { id: UploadId | null; name: string; sent: number; total: number; text: string | null };
const upload = ref<UploadView | null>(null);
/** Ends that arrived before `upload_file` answered with the upload's id. */
const ended = new Map<UploadId, UploadDone | UploadFailed>();

const video = ref<HTMLVideoElement | null>(null);
const stageEl = ref<HTMLElement | null>(null);
const stage = reactive({ w: 0, h: 0 });

let token = 0;
let follow = 0;
let adoptTimer = 0;
let localEnded = 0;
const stops: (() => void)[] = [];

// ------------------------------------------------------------------------------ what shows

const result = computed(() => info.value?.result ?? null);
const totalMs = computed(() => result.value?.durationMs ?? 0);
const keptMs = computed(() => range.endMs - range.startMs);
const locked = computed(() => job.value !== null);
const aspect = computed(() => (result.value ? result.value.size.width / Math.max(1, result.value.size.height) : 16 / 9));

const subtitle = computed(() => {
  const r = result.value;
  return r ? [createdAt(r.createdAtMs), tenths(r.durationMs), `${r.size.width} × ${r.size.height}`].join(" · ") : "";
});

const estimate = computed(() => {
  const i = info.value;
  if (!i) return null;
  return estimateExport({
    format: exportFormat(choice),
    keptMs: keptMs.value,
    totalMs: totalMs.value,
    size: i.result.size,
    fps: i.result.fps,
    fileBytes: i.result.fileBytes,
    audioSources: keptSources(choice, i),
  });
});

const videoBox = computed(() => {
  const r = result.value;
  if (!r || stage.w <= 0 || stage.h <= 0) return {};
  let w = stage.w;
  let h = w / aspect.value;
  if (h > stage.h) {
    h = stage.h;
    w = h * aspect.value;
  }
  return { width: `${Math.floor(w)}px`, height: `${Math.floor(h)}px` };
});

const percent = (fraction: number) => t("record.export.percent", { n: Math.round(clamp(fraction, 0, 1) * 100) });

const left = computed(() => {
  const ms = job.value?.remainingMs;
  if (ms == null) return t("record.export.running");
  if (ms < 2000) return t("record.export.almost");
  const s = Math.ceil(ms / 1000);
  return s < 90 ? t("record.export.leftSeconds", { n: s }) : t("record.export.leftMinutes", { n: Math.round(s / 60) });
});

const uploadLine = computed(() => {
  const u = upload.value;
  if (!u) return null;
  return u.text ?? t("record.upload.sending", { name: u.name, percent: percent(u.total ? u.sent / u.total : 0) });
});

const dialogFile = computed<DialogFile | null>(() => {
  const i = info.value;
  const e = estimate.value;
  if (!i || !e) return null;
  const base = i.result.path.split(/[\\/]/).pop()?.replace(/\.[^.]+$/, "") || t("record.trim.title");
  return {
    name: `${base}.${choice.format}`,
    thumbnail: recordingFrameUrl(i.result.id, range.startMs, 160),
    width: e.width,
    height: e.height,
    format: choice.format.toUpperCase(),
    bytes: null,
  };
});

// ------------------------------------------------------------------------------ the take

function open(next: RecordingInfo): void {
  video.value?.pause();
  info.value = next;
  loaded.value = true;
  range.startMs = 0;
  range.endMs = next.result.durationMs;
  playMs.value = 0;
  playing.value = false;
  playFailed.value = false;
  Object.assign(choice, initialChoice(next, settings.value));
  problem.value = null;
  note.value = null;
  asking.value = null;
  confirming.value = false;
  if (job.value && job.value.recordingId !== next.result.id) job.value = null;
}

// ---------------------------------------------------------------------------- playback

function seekTo(ms: number): void {
  playMs.value = ms;
  const v = video.value;
  if (v) v.currentTime = ms / 1000;
}

/** Playback stays inside the kept part, and starts it again when Loop is on. */
function followPlayback(): void {
  const v = video.value;
  if (!v) return;
  const ms = v.currentTime * 1000;
  if (ms >= range.endMs) {
    if (loop.value) seekTo(range.startMs);
    else {
      v.pause();
      seekTo(range.endMs);
    }
  } else playMs.value = ms;
  if (!v.paused) follow = requestAnimationFrame(followPlayback);
}

function togglePlay(): void {
  const v = video.value;
  if (!v) return;
  if (!v.paused) {
    v.pause();
    return;
  }
  if (playMs.value < range.startMs || playMs.value >= range.endMs - 50) seekTo(range.startMs);
  v.play().catch(playRefused);
}

/** A pause during loading rejects `play()` too; only a file the webview cannot play counts. */
function playRefused(error: unknown): void {
  if (error instanceof DOMException && error.name === "AbortError") return;
  playFailed.value = true;
}

function onPlay(): void {
  playing.value = true;
  cancelAnimationFrame(follow);
  follow = requestAnimationFrame(followPlayback);
}

function onPause(): void {
  playing.value = false;
  cancelAnimationFrame(follow);
}

function onEnded(): void {
  if (loop.value) {
    seekTo(range.startMs);
    void video.value?.play().catch(playRefused);
    return;
  }
  playing.value = false;
  playMs.value = range.endMs;
}

function step(direction: -1 | 1): void {
  const r = result.value;
  if (!r) return;
  video.value?.pause();
  seekTo(clamp(playMs.value + (direction * 1000) / Math.max(1, r.fps), range.startMs, range.endMs));
}

function toggleMute(): void {
  muted.value = !muted.value;
}

function setRange(startMs: number, endMs: number): void {
  if (locked.value) return;
  range.startMs = startMs;
  range.endMs = endMs;
}

function edgeAtPlayhead(edge: "start" | "end"): void {
  if (locked.value) return;
  const next = withEdge(range, edge, playMs.value, totalMs.value);
  setRange(next.startMs, next.endMs);
}

watch(
  () => [range.startMs, range.endMs],
  () => {
    const inside = clamp(playMs.value, range.startMs, range.endMs);
    if (inside !== playMs.value) seekTo(inside);
  },
);

// ------------------------------------------------------------------------------- export

function fail(error: unknown, place: ErrorPlace): void {
  if (codeOf(error) === "cancelled") return;
  console.error(error);
  problem.value = errorText(error, place);
}

async function runExport(purpose: ExportPurpose): Promise<ExportResult | null> {
  const i = info.value;
  if (!i || job.value) return null;
  problem.value = null;
  note.value = null;
  upload.value = null;
  video.value?.pause();
  const mine = ++token;
  job.value = { token: mine, recordingId: i.result.id, format: choice.format, fraction: 0, remainingMs: null, adopted: false };
  try {
    return await record_export(i.result.id, exportOptions(choice, i, range, purpose));
  } catch (error) {
    if (info.value?.result.id === i.result.id) fail(error, "export");
    return null;
  } finally {
    if (job.value?.token === mine) job.value = null;
    localEnded = Date.now();
  }
}

function onProgress(p: ExportProgress): void {
  if (p.recordingId !== info.value?.result.id) return;
  // An export this page did not start (the window opened again while one ran) shows too, and
  // goes away when it ends or stops reporting. The last report of this page's own export can
  // arrive after `record_export` answered, and starts nothing.
  if (!job.value && (p.fraction >= 1 || Date.now() - localEnded < 1500)) return;
  if (!job.value) job.value = { token: 0, recordingId: p.recordingId, format: p.format, fraction: 0, remainingMs: null, adopted: true };
  job.value.fraction = p.fraction;
  job.value.remainingMs = p.remainingMs;
  job.value.format = p.format;
  if (job.value.adopted) {
    window.clearTimeout(adoptTimer);
    adoptTimer = window.setTimeout(() => {
      if (job.value?.adopted) job.value = null;
    }, p.fraction >= 1 ? 600 : 5000);
  }
}

function cancelExport(): void {
  const id = job.value?.recordingId;
  if (id) record_export_cancel(id).catch((error) => fail(error, "export"));
}

function save(): void {
  void runExport("save");
}

function copy(): void {
  void runExport("copy");
}

// ------------------------------------------------------------------------------- upload

/** Upload checks the host first, as every Upload does: no host or a broken one sends the user to
 * Settings › Upload before anything is encoded, and a host not confirmed yet asks first. */
async function startUpload(): Promise<void> {
  if (!info.value || job.value || preparing.value || asking.value) return;
  problem.value = null;
  note.value = null;
  preparing.value = true;
  try {
    const ready = await prepareUpload(null);
    if (ready.kind === "noProvider") note.value = t("record.upload.noProvider");
    else if (ready.kind === "blocked") note.value = t("record.upload.blocked", { problem: issueText(ready.issues[0]!) });
    else if (ready.kind === "needsConsent") asking.value = ready.preflight.consent;
    else await sendUpload(ready.preflight.provider, ready.preflight.consent.name);
  } catch (error) {
    fail(error, "upload");
  } finally {
    preparing.value = false;
  }
}

async function sendUpload(provider: ProviderId, name: string): Promise<void> {
  const exported = await runExport("upload");
  const i = info.value;
  if (!exported || !i) return;
  upload.value = { id: null, name, sent: 0, total: exported.fileBytes, text: null };
  try {
    const started = await upload_file(exported.path, provider, i.entryId);
    if (!upload.value) return;
    upload.value.id = started.uploadId;
    upload.value.total = started.totalBytes;
    const early = ended.get(started.uploadId);
    if (early) finishUpload(early);
  } catch (error) {
    upload.value = null;
    fail(error, "upload");
  }
}

function finishUpload(end: UploadDone | UploadFailed): void {
  const u = upload.value;
  if (!u) return;
  if ("kind" in end && end.kind === "cancelled") upload.value = null;
  else u.text = "result" in end ? doneText(end) : failureText(end, u.name);
}

function rememberEnd(id: UploadId, end: UploadDone | UploadFailed): void {
  if (upload.value?.id === id) return finishUpload(end);
  ended.set(id, end);
  if (ended.size > 16) ended.delete(ended.keys().next().value as UploadId);
}

function consentGiven(consent: ConsentInfo): void {
  asking.value = null;
  void sendUpload(consent.provider, consent.name);
}

function consentStale(): void {
  asking.value = null;
  void startUpload();
}

function anotherHost(): void {
  asking.value = null;
  openUploadSettings();
}

function openUploadSettings(): void {
  window_open_settings("upload").catch((error) => fail(error, "upload"));
}

// ------------------------------------------------------------------------------- discard

function askDiscard(): void {
  if (info.value && !job.value) confirming.value = true;
}

async function trash(): Promise<void> {
  const i = info.value;
  if (!i) return;
  trashing.value = true;
  video.value?.pause();
  try {
    await record_trash(i.result.id);
    confirming.value = false;
    await getCurrentWebviewWindow().close();
  } catch (error) {
    confirming.value = false;
    fail(error, "trash");
  } finally {
    trashing.value = false;
  }
}

// ------------------------------------------------------------------------------ keyboard

function onKeyDown(event: KeyboardEvent): void {
  if (event.defaultPrevented || confirming.value || asking.value || !info.value) return;
  const target = event.target as Element | null;
  const mod = event.metaKey || event.ctrlKey;
  if (mod && !event.altKey && !event.shiftKey) {
    const key = event.key.toLowerCase();
    const run: Record<string, () => void> = { s: save, c: copy, u: () => void startUpload(), backspace: askDiscard };
    const action = run[key];
    if (!action) return;
    // ⌘C copies text the user selected in a field instead of the recording.
    if (key === "c" && window.getSelection()?.toString()) return;
    event.preventDefault();
    action();
    return;
  }
  if (mod || event.altKey || target?.closest("input, select, textarea")) return;
  const onButton = !!target?.closest("button");
  const ownsArrows = !!target?.closest('[role="slider"], [role="radiogroup"]');
  switch (event.key) {
    case " ":
      if (onButton) return;
      togglePlay();
      break;
    case "l":
    case "L":
      loop.value = !loop.value;
      break;
    case "i":
    case "I":
      edgeAtPlayhead("start");
      break;
    case "o":
    case "O":
      edgeAtPlayhead("end");
      break;
    case "ArrowLeft":
      if (ownsArrows) return;
      step(-1);
      break;
    case "ArrowRight":
      if (ownsArrows) return;
      step(1);
      break;
    default:
      return;
  }
  event.preventDefault();
}

// ----------------------------------------------------------------------------- lifecycle

const stageObserver = new ResizeObserver(() => {
  const el = stageEl.value;
  if (!el) return;
  stage.w = el.clientWidth - 40;
  stage.h = el.clientHeight - 20;
});

watch(stageEl, (el, old) => {
  if (old) stageObserver.unobserve(old);
  if (el) stageObserver.observe(el);
});

onMounted(async () => {
  window.addEventListener("keydown", onKeyDown);
  let heard = false;
  try {
    stops.push(
      await onRecordTrim((next) => {
        heard = true;
        open(next);
      }),
    );
    stops.push(await onRecordExportProgress(onProgress));
    stops.push(
      await onUploadProgress((p) => {
        const u = upload.value;
        if (u && u.id === p.uploadId && u.text === null) {
          u.sent = p.sentBytes;
          u.total = p.totalBytes;
        }
      }),
    );
    stops.push(await onUploadDone((done) => rememberEnd(done.result.uploadId, done)));
    stops.push(await onUploadFailed((failed) => rememberEnd(failed.uploadId, failed)));
    settings.value = await settings_get().catch(() => null);
    const first = await record_trim_current();
    if (!heard && first) open(first);
  } catch (error) {
    fail(error, "export");
  } finally {
    loaded.value = true;
  }
});

onUnmounted(() => {
  window.removeEventListener("keydown", onKeyDown);
  cancelAnimationFrame(follow);
  window.clearTimeout(adoptTimer);
  stageObserver.disconnect();
  stops.forEach((stop) => stop());
});
</script>

<template>
  <div class="r-trimwin">
    <header class="g-toolbar">
      <h1 class="g-window-title">{{ t("record.trim.title") }}</h1>
      <span class="r-subtitle">{{ subtitle }}</span>
      <span class="g-spacer" />
      <TipWrap :tip="t('record.trim.discardTip')" :caps="['⌘', '⌫']" side="below" align="end">
        <button type="button" class="g-button" data-variant="destructive" :disabled="!info || locked" @click="askDiscard">
          <GIcon name="trash" :size="16" />{{ t("record.trim.discard") }}
        </button>
      </TipWrap>
    </header>

    <div v-if="info && result && estimate" class="r-body">
      <div class="r-stagecol">
        <div ref="stageEl" class="r-player">
          <div class="r-video" :style="videoBox">
            <video
              ref="video"
              :src="info.url"
              preload="auto"
              playsinline
              :muted="muted"
              @play="onPlay"
              @pause="onPause"
              @ended="onEnded"
              @error="playFailed = true"
            />
            <p v-if="playFailed" class="r-video-error" role="alert">{{ t("record.trim.playFailed") }}</p>
          </div>
        </div>
        <div class="r-transport">
          <TipWrap :tip="t('record.trim.prev')" :caps="['←']" align="start">
            <button type="button" class="g-btn" :aria-label="t('record.trim.prev')" @click="step(-1)"><GIcon name="chevron-left" /></button>
          </TipWrap>
          <TipWrap :tip="playing ? t('record.trim.pause') : t('record.trim.play')" :caps="['Space']">
            <button type="button" class="g-btn" :aria-label="playing ? t('record.trim.pause') : t('record.trim.play')" @click="togglePlay">
              <GIcon :name="playing ? 'pause' : 'play'" />
            </button>
          </TipWrap>
          <TipWrap :tip="t('record.trim.next')" :caps="['→']">
            <button type="button" class="g-btn" :aria-label="t('record.trim.next')" @click="step(1)"><GIcon name="chevron-right" /></button>
          </TipWrap>
          <span class="r-time"
            ><b>{{ tenths(playMs) }}</b><span> / {{ tenths(totalMs) }}</span></span
          >
          <span class="g-spacer" />
          <TipWrap :tip="t('record.trim.loop')" :caps="['L']">
            <button type="button" class="g-btn" :aria-label="t('record.trim.loop')" :aria-pressed="loop" @click="loop = !loop">
              <GIcon name="repeat" />
            </button>
          </TipWrap>
          <TipWrap :tip="t('record.trim.mute')" align="end">
            <button type="button" class="g-btn" :aria-label="t('record.trim.mute')" :aria-pressed="muted" @click="toggleMute">
              <GIcon :name="muted ? 'speaker-off' : 'speaker'" />
            </button>
          </TipWrap>
        </div>
        <Timeline
          :recording-id="result.id"
          :total-ms="totalMs"
          :start-ms="range.startMs"
          :end-ms="range.endMs"
          :play-ms="playMs"
          :aspect="aspect"
          :locked="locked"
          :encoded="job?.fraction ?? 0"
          @range="setRange"
          @seek="seekTo"
        />
      </div>

      <ExportPanel :info="info" :choice="choice" :kept-ms="keptMs" :estimate="estimate" :locked="locked">
        <template #foot>
          <div v-if="job" class="r-export-foot">
            <div class="r-progress-head">
              <span>{{ job.format === "gif" ? t("record.export.encodingGif") : t("record.export.encodingMp4") }}</span>
              <span>{{ percent(job.fraction) }}</span>
            </div>
            <div
              class="g-progress"
              role="progressbar"
              :aria-label="t('record.export.encoding')"
              aria-valuemin="0"
              aria-valuemax="100"
              :aria-valuenow="Math.round(job.fraction * 100)"
              :style="{ '--progress': Math.round(job.fraction * 100) }"
            />
            <p class="g-help">{{ left }}</p>
            <div class="r-actions">
              <span class="g-spacer" />
              <button type="button" class="g-button" @click="cancelExport">{{ t("record.export.cancel") }}</button>
            </div>
          </div>
          <div v-else class="r-export-foot">
            <p v-if="result.stoppedBecause === 'diskFull'" class="g-help" data-tone="warn">
              <GIcon name="warning" :size="14" />{{ t("record.trim.diskFull") }}
            </p>
            <p v-if="problem" class="g-help" data-tone="danger" role="alert"><GIcon name="bang" :size="14" />{{ problem }}</p>
            <p v-else-if="note" class="g-help" data-tone="warn" role="alert">
              {{ note }}
              <button type="button" class="g-link r-link" @click="openUploadSettings">{{ t("record.upload.openSettings") }}</button>
            </p>
            <p v-else-if="uploadLine" class="g-help" role="status">{{ uploadLine }}</p>
            <p v-else class="g-help">{{ t("record.export.saveTo", { folder: info.folderName }) }}</p>
            <div class="r-actions">
              <TipWrap :tip="t('record.export.uploadTip')" :caps="['⌘', 'U']" align="start">
                <button type="button" class="g-button" :disabled="preparing" @click="startUpload">
                  <GIcon name="upload" :size="16" />{{ t("record.export.upload") }}
                </button>
              </TipWrap>
              <TipWrap :tip="t('record.export.copyTip')" :caps="['⌘', 'C']">
                <button type="button" class="g-button" @click="copy"><GIcon name="copy" :size="16" />{{ t("record.export.copy") }}</button>
              </TipWrap>
              <span class="g-spacer" />
              <TipWrap :tip="t('record.export.saveTip')" :caps="['⌘', 'S']" align="end">
                <button type="button" class="g-button" data-variant="primary" @click="save">
                  <GIcon name="save" :size="16" />{{ t("record.export.save") }}
                </button>
              </TipWrap>
            </div>
          </div>
        </template>
      </ExportPanel>
    </div>

    <div v-else-if="loaded" class="g-empty r-empty">
      <div class="g-empty-icon"><GIcon name="film" :size="32" /></div>
      <h2 class="g-empty-title">{{ t("record.trim.emptyTitle") }}</h2>
      <p class="g-empty-text">{{ t("record.trim.emptyText") }}</p>
      <p v-if="problem" class="g-help" data-tone="danger" role="alert">{{ problem }}</p>
    </div>

    <ConfirmDialog
      v-if="confirming"
      :title="t('record.trim.trashTitle')"
      :text="t('record.trim.trashText')"
      icon="trash"
      tone="danger"
      :confirm-label="t('record.trim.trashConfirm')"
      :cancel-label="t('record.cancel')"
      destructive
      :busy="trashing"
      @confirm="trash"
      @cancel="confirming = false"
    />
    <FirstUploadDialog
      v-if="asking"
      :consent="asking"
      :file="dialogFile"
      @confirmed="consentGiven"
      @cancel="asking = null"
      @stale="consentStale"
      @another-host="anotherHost"
    />
  </div>
</template>

<style>
.r-trimwin { position: absolute; inset: 0; display: flex; flex-direction: column; overflow: hidden; color: var(--win-text); background: var(--win-content); }
.r-trimwin > .g-toolbar { gap: var(--space-8); }
.r-trimwin .g-window-title { margin: 0; }
.r-body { display: flex; flex: 1; min-height: 0; }
.r-stagecol { display: flex; flex-direction: column; flex: 1; min-width: 0; background: var(--win-ground); }
.r-player { flex: 1; min-height: 0; display: grid; place-items: center; padding: var(--space-20) var(--space-20) 0; }
.r-video { position: relative; overflow: hidden; border-radius: var(--radius-thumb); background: var(--win-sunken);
  box-shadow: 0 0 0 var(--hairline) var(--win-line), var(--win-shadow-lift); }
.r-video > video { display: block; width: 100%; height: 100%; object-fit: contain; }
.r-video-error { position: absolute; inset: auto 12px 12px; margin: 0; padding: var(--space-8) var(--space-12); border-radius: var(--radius-field);
  background: var(--win-raised); color: var(--win-text); font: var(--type-body); box-shadow: var(--win-shadow-lift); }
.r-transport { position: relative; display: flex; align-items: center; gap: var(--space-4); flex: none; height: 52px; padding: 0 var(--space-16); }
.r-time { margin-left: var(--space-8); font: var(--type-timecode); font-variant-numeric: tabular-nums; color: var(--win-text); white-space: nowrap; }
.r-time > b { font-weight: inherit; }
.r-time > span { color: var(--win-text-3); font-weight: 500; }
.r-export-foot { display: flex; flex-direction: column; gap: var(--space-12); padding: var(--space-16) var(--space-20) var(--space-20); box-shadow: inset 0 var(--hairline) 0 var(--win-line); }
.r-export-foot > .g-help { margin: 0; }
/* In a short window the options scroll and the actions stay in reach. */
.r-export > .r-export-foot { position: sticky; bottom: 0; z-index: 1; background: var(--win-content); }
.r-actions { display: flex; align-items: center; gap: var(--space-8); }
.r-progress-head { display: flex; justify-content: space-between; font: var(--type-title); color: var(--win-text); }
.r-progress-head > span:last-child { font: var(--type-num); color: var(--win-text-2); }
.r-link { display: inline; padding: 0; margin-left: 2px; font: inherit; }
.r-empty { flex: 1; justify-content: center; }
</style>
