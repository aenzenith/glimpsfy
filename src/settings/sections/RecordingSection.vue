<script setup lang="ts">
// Settings › Recording: the video, the audio sources, what the recording shows (clicks, keys,
// webcam), the recordings folder and the GIF export defaults. Options that depend on a switch sit
// indented under it and grey out with it. The click ring, the key caps and the webcam show only
// where the platform can draw them (`inputOverlays`, `cameraBubble`). The speech model the trim
// window cuts by text with is listed here too, as the PP-OCR models are under Text recognition: a
// download that this section or the trim window started shows its progress, and only the one this
// section started can be cancelled from here.
import { computed, onBeforeUnmount, onMounted, ref } from "vue";

import FormRow from "../../components/FormRow.vue";
import SectionHead from "../../components/SectionHead.vue";
import SegmentedControl from "../../components/SegmentedControl.vue";
import SelectControl from "../../components/SelectControl.vue";
import SwitchControl from "../../components/SwitchControl.vue";
import { ApiError, on } from "../../lib/api/core";
import { record_devices, type Device, type RecordingDevices } from "../../lib/api/record";
import {
  TRANSCRIPT_MODEL_PROGRESS,
  transcript_cancel,
  transcript_model,
  transcript_model_download,
  transcript_model_remove,
  type SpeechModelInfo,
  type SpeechModelProgress,
} from "../../lib/api/transcript";
import { t, tp } from "../../lib/i18n";
import { can } from "../../lib/platform";
import type { Corner, KeysPosition, RecordingResolution, RingColor, Settings, Size2, Size3, VideoCodec, WebcamShape } from "../../lib/settings";
import { update } from "../store";
import { describeError, formatBytes, isNotImplemented, uid } from "../text";
import FolderRow from "./FolderRow.vue";

const props = defineProps<{ settings: Settings }>();
const r = computed(() => props.settings.recording);
const cleanDesktopId = uid("clean");

const ids = {
  codec: uid("codec"), fps: uid("fps"), resolution: uid("res"), system: uid("sys"), mic: uid("mic"), micDevice: uid("micdev"), tracks: uid("tracks"),
  countdown: uid("count"), clicks: uid("clicks"), ring: uid("ring"), ringSize: uid("ringsize"), keys: uid("keys"), keysPosition: uid("keypos"), keysSize: uid("keysize"), keysOnly: uid("keysonly"),
  camera: uid("cam"), shape: uid("shape"), camSize: uid("camsize"), corner: uid("corner"), gifFps: uid("giffps"), gifWidth: uid("gifw"),
};

// ------------------------------------------------------------------------------------ devices
const devices = ref<RecordingDevices | null>(null);
const devicesState = ref<"loading" | "ready" | "unavailable" | "failed">("loading");
const devicesError = ref<string | null>(null);

onMounted(async () => {
  try {
    devices.value = await record_devices();
    devicesState.value = "ready";
  } catch (error) {
    devicesState.value = isNotImplemented(error) ? "unavailable" : "failed";
    devicesError.value = describeError(error);
  }
});

/** The choices of a device row: the system default (an empty value, saved as `null`), every
 * device, and a saved device that is not connected now, so the row never claims another device
 * than the one saved. */
function deviceOptions(list: Device[] | undefined, saved: string | null) {
  const options: { value: string; label: string }[] = [{ value: "", label: t("settings.recording.systemDefault") }];
  for (const device of list ?? []) options.push({ value: device.id, label: device.name });
  if (saved && !(list ?? []).some((d) => d.id === saved)) {
    options.push({ value: saved, label: devicesState.value === "ready" ? t("settings.recording.deviceGone") : t("settings.recording.deviceSaved") });
  }
  return options;
}

const micOptions = computed(() => deviceOptions(devices.value?.microphones, r.value.microphoneDevice));
const cameraOptions = computed(() => deviceOptions(devices.value?.cameras, r.value.webcamDevice));

function deviceHelp(kind: "microphones" | "cameras"): { text: string; tone: "warn" | "danger" | null } | null {
  if (devicesState.value === "loading") return { text: t("settings.recording.devicesLoading"), tone: null };
  if (devicesState.value === "unavailable") return { text: t("settings.recording.devicesUnavailable"), tone: null };
  if (devicesState.value === "failed") return { text: devicesError.value ?? "", tone: "danger" };
  const list = devices.value?.[kind] ?? [];
  const saved = kind === "microphones" ? r.value.microphoneDevice : r.value.webcamDevice;
  if (!list.length) return { text: t(kind === "microphones" ? "settings.recording.noMicrophones" : "settings.recording.noCameras"), tone: "warn" };
  if (saved && !list.some((d) => d.id === saved)) return { text: t("settings.recording.deviceGoneHelp"), tone: "warn" };
  return null;
}
const micHelp = computed(() => deviceHelp("microphones"));
const cameraHelp = computed(() => deviceHelp("cameras"));
const systemAudioHelp = computed(() =>
  devicesState.value === "ready" && devices.value && !devices.value.systemAudio ? tp("settings.recording.systemAudioMissing") : null,
);

// ------------------------------------------------------------------------------------ choices
const codecs = [
  { value: "h264" as VideoCodec, label: "H.264" },
  { value: "hevc" as VideoCodec, label: "HEVC" },
];
const fpsOptions = computed(() => [30, 60].map((n) => ({ value: n, label: t("settings.common.fps", { n }) })));
const resolutions = computed(() => [
  { value: "native" as RecordingResolution, label: t("settings.recording.native") },
  { value: "half" as RecordingResolution, label: t("settings.recording.half") },
]);
const countdowns = computed(() => [
  { value: 0, label: t("settings.common.off") },
  { value: 3, label: t("settings.common.seconds", { n: 3 }) },
  { value: 5, label: t("settings.common.seconds", { n: 5 }) },
]);
const colours = computed(() => [
  { value: "yellow" as RingColor, label: t("settings.recording.yellow") },
  { value: "red" as RingColor, label: t("settings.recording.red") },
  { value: "blue" as RingColor, label: t("settings.recording.blue") },
  { value: "white" as RingColor, label: t("settings.recording.white") },
]);
const sizes2 = computed(() => [
  { value: "small" as Size2, label: t("settings.common.small") },
  { value: "large" as Size2, label: t("settings.common.large") },
]);
const sizes3 = computed(() => [
  { value: "small" as Size3, label: t("settings.common.small") },
  { value: "medium" as Size3, label: t("settings.common.medium") },
  { value: "large" as Size3, label: t("settings.common.large") },
]);
const positions = computed(() => [
  { value: "bottomCentre" as KeysPosition, label: t("settings.recording.bottomCentre") },
  { value: "bottomLeft" as KeysPosition, label: t("settings.recording.bottomLeft") },
  { value: "bottomRight" as KeysPosition, label: t("settings.recording.bottomRight") },
  { value: "topCentre" as KeysPosition, label: t("settings.recording.topCentre") },
]);
const shapes = computed(() => [
  { value: "circle" as WebcamShape, label: t("settings.recording.circle") },
  { value: "roundedSquare" as WebcamShape, label: t("settings.recording.roundedSquare") },
]);
const corners = computed(() => [
  { value: "bottomRight" as Corner, label: t("settings.recording.cornerBottomRight") },
  { value: "bottomLeft" as Corner, label: t("settings.recording.cornerBottomLeft") },
  { value: "topRight" as Corner, label: t("settings.recording.cornerTopRight") },
  { value: "topLeft" as Corner, label: t("settings.recording.cornerTopLeft") },
]);
const gifFpsOptions = computed(() => [10, 15, 20].map((n) => ({ value: n, label: t("settings.common.fps", { n }) })));
const gifWidths = computed(() => {
  const widths = [480, 640, 800, 1024];
  const saved = r.value.gifWidth;
  if (saved !== null && !widths.includes(saved)) widths.push(saved);
  // 0 stands for the original width, which the settings keep as `null`.
  return [...widths.sort((a, b) => a - b).map((n) => ({ value: n, label: t("settings.common.px", { n }) })), { value: 0, label: t("settings.recording.original") }];
});

// ------------------------------------------------------------------------------ speech model
const speech = ref<SpeechModelInfo | null>(null);
const speechState = ref<"loading" | "ready" | "unavailable" | "failed">("loading");
const speechProblem = ref<string | null>(null);
/** The download under way: this section's own (with the job that cancels it) or the trim
 * window's (`job: null`). */
const speechJob = ref<{ job: string | null; received: number; total: number } | null>(null);
let stopSpeech: (() => void) | null = null;
let foreignTimer = 0;

async function loadSpeech(): Promise<void> {
  try {
    speech.value = await transcript_model();
    speechState.value = "ready";
  } catch (error) {
    speechState.value = isNotImplemented(error) ? "unavailable" : "failed";
    speechProblem.value = isNotImplemented(error) ? null : describeError(error);
  }
}

/** Work offline keeps the download off; a model on disk still works and can be removed. */
const offline = computed(() => props.settings.general.offline === true);

async function downloadSpeech(): Promise<void> {
  const model = speech.value;
  if (!model || speechJob.value || offline.value) return;
  const job = `speech-model-${Date.now()}`;
  speechJob.value = { job, received: 0, total: model.bytes };
  speechProblem.value = null;
  try {
    speech.value = await transcript_model_download(job);
  } catch (error) {
    if (!(error instanceof ApiError && error.code === "cancelled")) speechProblem.value = describeError(error);
  } finally {
    speechJob.value = null;
    await loadSpeech();
  }
}

function cancelSpeech(): void {
  const job = speechJob.value?.job;
  if (job) void transcript_cancel(job).catch((error) => (speechProblem.value = describeError(error)));
}

async function removeSpeech(): Promise<void> {
  speechProblem.value = null;
  try {
    await transcript_model_remove();
  } catch (error) {
    speechProblem.value = describeError(error);
  }
  await loadSpeech();
}

/** A download the trim window started shows here as well; once it ends or stops reporting, the
 * model is read again. */
function speechProgress(p: SpeechModelProgress): void {
  const j = speechJob.value;
  if (j) Object.assign(j, { received: p.received, total: p.total });
  else speechJob.value = { job: null, received: p.received, total: p.total };
  if (speechJob.value?.job) return;
  window.clearTimeout(foreignTimer);
  foreignTimer = window.setTimeout(
    () => {
      if (speechJob.value?.job) return;
      speechJob.value = null;
      void loadSpeech();
    },
    p.total > 0 && p.received >= p.total ? 800 : 10000,
  );
}

const speechMeta = computed(() => {
  const model = speech.value;
  if (!model) return "";
  const j = speechJob.value;
  const size =
    j && j.total > 0
      ? t("settings.recording.speechModelProgress", { p: Math.round((j.received / j.total) * 100), size: formatBytes(j.total) })
      : formatBytes(model.bytes);
  return [size, model.folder].filter(Boolean).join(" · ");
});

const speechHelp = computed(() => {
  if (speechState.value === "loading") return t("settings.recording.speechModelLoading");
  if (speechState.value === "unavailable") return t("settings.recording.speechModelMissing");
  return speechProblem.value;
});

onMounted(async () => {
  void loadSpeech();
  try {
    stopSpeech = await on<SpeechModelProgress>(TRANSCRIPT_MODEL_PROGRESS, speechProgress);
  } catch (error) {
    console.warn("Settings cannot hear the speech model's download.", error);
  }
});

onBeforeUnmount(() => {
  stopSpeech?.();
  window.clearTimeout(foreignTimer);
});

// ------------------------------------------------------------------------------ ring swatches
const swatchRow = ref<HTMLElement | null>(null);
function pickColour(value: RingColor, focus = false): void {
  if (!r.value.highlightClicks) return;
  if (value !== r.value.clickRingColor) void update((s) => (s.recording.clickRingColor = value));
  if (focus) {
    const index = colours.value.findIndex((c) => c.value === value);
    (swatchRow.value?.children[index] as HTMLElement | undefined)?.focus();
  }
}
function swatchKey(event: KeyboardEvent): void {
  const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[event.key];
  if (!step) return;
  event.preventDefault();
  const list = colours.value;
  const index = list.findIndex((c) => c.value === r.value.clickRingColor);
  const next = list[(index + step + list.length) % list.length];
  if (next) pickColour(next.value, true);
}
</script>

<template>
  <SectionHead :title="t('settings.recording.headVideo')" :desc="tp('settings.recording.videoDesc')" />
  <div class="g-group">
    <FormRow :label="t('settings.recording.codec')" :label-id="ids.codec" :help="t('settings.recording.codecHelp')">
      <SegmentedControl :labelledby="ids.codec" :model-value="r.codec" :options="codecs" @update:model-value="(v) => update((s) => (s.recording.codec = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.fps')" :label-id="ids.fps">
      <SegmentedControl :labelledby="ids.fps" :model-value="r.fps" :options="fpsOptions" @update:model-value="(v) => update((s) => (s.recording.fps = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.resolution')" :label-id="ids.resolution" :help="t('settings.recording.resolutionHelp')">
      <SegmentedControl :labelledby="ids.resolution" :model-value="r.resolution" :options="resolutions" @update:model-value="(v) => update((s) => (s.recording.resolution = v))" />
    </FormRow>
  </div>

  <SectionHead :title="t('settings.recording.headAudio')" />
  <div class="g-group">
    <FormRow :label="t('settings.recording.systemAudio')" :for="ids.system" :help="systemAudioHelp" :tone="systemAudioHelp ? 'warn' : null">
      <SwitchControl :id="ids.system" :model-value="r.systemAudio" @update:model-value="(v) => update((s) => (s.recording.systemAudio = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.microphone')" :for="ids.mic">
      <SwitchControl :id="ids.mic" :model-value="r.microphone" @update:model-value="(v) => update((s) => (s.recording.microphone = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.microphoneDevice')" :for="ids.micDevice" sub :disabled="!r.microphone" :help="micHelp?.text" :tone="micHelp?.tone">
      <SelectControl
        :id="ids.micDevice"
        :disabled="!r.microphone"
        :min-width="200"
        :model-value="r.microphoneDevice ?? ''"
        :options="micOptions"
        @update:model-value="(v) => update((s) => (s.recording.microphoneDevice = v || null))"
      />
    </FormRow>
    <FormRow :label="t('settings.recording.separateTracks')" :for="ids.tracks" :help="t('settings.recording.separateTracksHelp')">
      <SwitchControl :id="ids.tracks" :model-value="r.separateTracks" @update:model-value="(v) => update((s) => (s.recording.separateTracks = v))" />
    </FormRow>
  </div>

  <SectionHead :title="t('settings.recording.headDuring')" />
  <div class="g-group">
    <FormRow :label="t('settings.recording.countdown')" :label-id="ids.countdown">
      <SegmentedControl :labelledby="ids.countdown" :model-value="r.countdownSeconds" :options="countdowns" @update:model-value="(v) => update((s) => (s.recording.countdownSeconds = v))" />
    </FormRow>
    <FormRow v-if="can('cleanDesktop')" :label="t('settings.recording.cleanDesktop')" :for="cleanDesktopId" :help="t('settings.recording.cleanDesktopHelp')">
      <SwitchControl :id="cleanDesktopId" :model-value="r.cleanDesktop" @update:model-value="(v) => update((s) => (s.recording.cleanDesktop = v))" />
    </FormRow>
    <template v-if="can('inputOverlays')">
    <FormRow :label="t('settings.recording.clicks')" :for="ids.clicks">
      <SwitchControl :id="ids.clicks" :model-value="r.highlightClicks" @update:model-value="(v) => update((s) => (s.recording.highlightClicks = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.ringColour')" :label-id="ids.ring" sub :disabled="!r.highlightClicks">
      <div ref="swatchRow" class="p-swatches" role="radiogroup" :aria-labelledby="ids.ring" @keydown="swatchKey">
        <button
          v-for="colour in colours"
          :key="colour.value"
          type="button"
          class="g-swatch"
          role="radio"
          :aria-checked="r.clickRingColor === colour.value"
          :tabindex="r.clickRingColor === colour.value ? 0 : -1"
          :aria-label="colour.label"
          :style="{ '--swatch': `var(--ink-${colour.value})` }"
          :disabled="!r.highlightClicks"
          @click="pickColour(colour.value)"
        ></button>
      </div>
    </FormRow>
    <FormRow :label="t('settings.recording.ringSize')" :label-id="ids.ringSize" sub :disabled="!r.highlightClicks">
      <SegmentedControl :labelledby="ids.ringSize" :disabled="!r.highlightClicks" :model-value="r.clickRingSize" :options="sizes2" @update:model-value="(v) => update((s) => (s.recording.clickRingSize = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.keys')" :for="ids.keys" :help="t('settings.recording.keysHelp')">
      <SwitchControl :id="ids.keys" :model-value="r.showKeys" @update:model-value="(v) => update((s) => (s.recording.showKeys = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.keysPosition')" :for="ids.keysPosition" sub :disabled="!r.showKeys">
      <SelectControl :id="ids.keysPosition" :disabled="!r.showKeys" :model-value="r.keysPosition" :options="positions" @update:model-value="(v) => update((s) => (s.recording.keysPosition = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.keysSize')" :label-id="ids.keysSize" sub :disabled="!r.showKeys">
      <SegmentedControl :labelledby="ids.keysSize" :disabled="!r.showKeys" :model-value="r.keysSize" :options="sizes3" @update:model-value="(v) => update((s) => (s.recording.keysSize = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.keysOnly')" :for="ids.keysOnly" sub :disabled="!r.showKeys" :help="tp('settings.recording.keysOnlyHelp')">
      <SwitchControl :id="ids.keysOnly" :disabled="!r.showKeys" :model-value="r.keysOnlyShortcuts" @update:model-value="(v) => update((s) => (s.recording.keysOnlyShortcuts = v))" />
    </FormRow>
    </template>
  </div>

  <template v-if="can('cameraBubble')">
  <SectionHead :title="t('settings.recording.headWebcam')" :desc="t('settings.recording.webcamDesc')" />
  <div class="g-group">
    <FormRow :label="t('settings.recording.camera')" :for="ids.camera" :help="cameraHelp?.text" :tone="cameraHelp?.tone">
      <SelectControl :id="ids.camera" :min-width="200" :model-value="r.webcamDevice ?? ''" :options="cameraOptions" @update:model-value="(v) => update((s) => (s.recording.webcamDevice = v || null))" />
    </FormRow>
    <FormRow :label="t('settings.recording.shape')" :label-id="ids.shape">
      <SegmentedControl :labelledby="ids.shape" :model-value="r.webcamShape" :options="shapes" @update:model-value="(v) => update((s) => (s.recording.webcamShape = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.webcamSize')" :label-id="ids.camSize">
      <SegmentedControl :labelledby="ids.camSize" :model-value="r.webcamSize" :options="sizes3" @update:model-value="(v) => update((s) => (s.recording.webcamSize = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.corner')" :for="ids.corner" :help="t('settings.recording.cornerHint')">
      <SelectControl :id="ids.corner" :model-value="r.webcamCorner" :options="corners" @update:model-value="(v) => update((s) => (s.recording.webcamCorner = v))" />
    </FormRow>
  </div>
  </template>

  <SectionHead :title="t('settings.recording.headFiles')" />
  <div class="g-group">
    <FolderRow :label="t('settings.recording.folder')" :path="r.recordingsFolder" @change="(path) => update((s) => (s.recording.recordingsFolder = path))" />
  </div>

  <SectionHead :title="t('settings.recording.headGif')" :desc="t('settings.recording.gifDesc')" />
  <div class="g-group">
    <FormRow :label="t('settings.recording.fps')" :label-id="ids.gifFps">
      <SegmentedControl :labelledby="ids.gifFps" :model-value="r.gifFps" :options="gifFpsOptions" @update:model-value="(v) => update((s) => (s.recording.gifFps = v))" />
    </FormRow>
    <FormRow :label="t('settings.recording.gifWidth')" :for="ids.gifWidth">
      <SelectControl :id="ids.gifWidth" :model-value="r.gifWidth ?? 0" :options="gifWidths" @update:model-value="(v) => update((s) => (s.recording.gifWidth = v || null))" />
    </FormRow>
  </div>

  <SectionHead :title="t('settings.recording.speechModelHead')" :desc="tp('settings.recording.speechModelDesc')" />
  <div class="g-group">
    <FormRow
      v-if="speechState !== 'ready' || !speech"
      :label="t('settings.recording.speechModelHead')"
      :help="speechHelp"
      :tone="speechState === 'loading' ? null : speechState === 'failed' ? 'danger' : 'warn'"
    />
    <template v-else>
      <FormRow v-if="speechProblem" :help="speechProblem" tone="danger" />
      <FormRow
        :label="speech.title"
        :help="offline && !speech.installed && !speechJob ? `${speechMeta} · ${t('settings.common.offlineNote')}` : speechMeta"
        :disabled="offline && !speech.installed && !speechJob"
      >
        <span v-if="speech.installed" class="g-badge" data-tone="ok">{{ t("settings.recording.speechModelInstalled") }}</span>
        <button v-if="speech.installed" type="button" class="g-button" :disabled="!!speechJob" @click="removeSpeech">
          {{ t("settings.recording.speechModelRemove") }}
        </button>
        <button v-else-if="speechJob" type="button" class="g-button" :disabled="!speechJob.job" @click="cancelSpeech">
          {{ t("settings.common.cancel") }}
        </button>
        <button v-else type="button" class="g-button" :disabled="offline" @click="downloadSpeech">{{ t("settings.recording.speechModelDownload") }}</button>
      </FormRow>
    </template>
  </div>
</template>
