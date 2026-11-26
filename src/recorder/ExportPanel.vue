<script setup lang="ts">
// The export panel of the trim window (design `exportPanel`): MP4 or GIF; for an MP4 the
// resolution, the quality, the audio the file holds and its subtitles; for a GIF the frame rate,
// the width and the loop. The estimate follows every change and warns when a GIF grows past what chat apps
// accept. The footer (actions or progress) comes from the window through the `foot` slot.
import { computed } from "vue";

import FormRow from "../components/FormRow.vue";
import GIcon from "../components/GIcon.vue";
import SelectControl from "../components/SelectControl.vue";
import SwitchControl from "../components/SwitchControl.vue";
import type { ExportCaptions, Mp4Quality, RecordingInfo, Resolution } from "../lib/api/record";
import { t } from "../lib/i18n";
import { audioLayout, gifWidths, GIF_FPS, type ExportChoice } from "./choice";
import { GIF_WARN_BYTES, halfSize, type Estimate } from "./estimate";
import { approxSize, tenths } from "./format";

const props = defineProps<{
  info: RecordingInfo;
  /** The panel's choices; the panel changes them in place. */
  choice: ExportChoice;
  keptMs: number;
  estimate: Estimate;
  locked: boolean;
  /** The text panel has read the speech, so subtitles can be made of it. */
  textReady: boolean;
}>();

const size = computed(() => props.info.result.size);
const half = computed(() => halfSize(size.value));
const layout = computed(() => audioLayout(props.info));

const FORMATS = [
  { value: "mp4" as const, label: "MP4", icon: "film" },
  { value: "gif" as const, label: "GIF", icon: "gif" },
];

const resolutions = computed<{ value: Resolution; label: string }[]>(() => [
  { value: "native", label: t("record.export.native", { width: size.value.width, height: size.value.height }) },
  { value: "half", label: t("record.export.half", { width: half.value.width, height: half.value.height }) },
]);
const QUALITIES: { value: Mp4Quality; label: "record.export.small" | "record.export.balanced" | "record.export.best" }[] = [
  { value: "low", label: "record.export.small" },
  { value: "medium", label: "record.export.balanced" },
  { value: "high", label: "record.export.best" },
];
const widths = computed(() =>
  gifWidths(props.info).map((w) => ({ value: w, label: w ? t("record.export.px", { width: w }) : t("record.export.original") })),
);

const CAPTIONS: { value: ExportCaptions; label: "record.export.captionsNone" | "record.export.captionsSrt" | "record.export.captionsVtt" | "record.export.captionsTrack" }[] = [
  { value: "none", label: "record.export.captionsNone" },
  { value: "srt", label: "record.export.captionsSrt" },
  { value: "vtt", label: "record.export.captionsVtt" },
  { value: "track", label: "record.export.captionsTrack" },
];
const captionOptions = computed(() => CAPTIONS.map((c) => ({ value: c.value, label: t(c.label) })));

/** Until the speech is read the choice shows "None", which is what the export writes then. */
const captions = computed({
  get: () => (props.textReady ? props.choice.captions : "none"),
  set: (value: ExportCaptions) => {
    props.choice.captions = value;
  },
});
const captionsHelp = computed(() => {
  if (!props.textReady) return t("record.export.captionsWait");
  if (props.choice.captions === "srt" || props.choice.captions === "vtt") return t("record.export.captionsFile");
  return props.choice.captions === "track" ? t("record.export.captionsInside") : "";
});

/** One "Sound" switch drives both flags when the file holds one source. */
const sound = computed({
  get: () => props.choice.microphone || props.choice.system,
  set: (on: boolean) => {
    props.choice.microphone = on;
    props.choice.system = on;
  },
});

const codec = computed(() => (props.info.result.codec === "hevc" ? "HEVC" : "H.264"));
const hasSound = computed(() => {
  if (props.choice.format === "gif" || layout.value === "none") return false;
  return layout.value === "one" ? sound.value : props.choice.microphone || props.choice.system;
});
const meta = computed(() => {
  const e = props.estimate;
  const parts = [tenths(props.keptMs), `${e.width} × ${e.height}`, t("record.export.fps", { fps: e.fps })];
  if (props.choice.format === "gif") parts.push(props.choice.loopForever ? t("record.export.loops") : t("record.export.once"));
  else parts.push(codec.value, hasSound.value ? "AAC" : t("record.export.silent"));
  return parts.join(" · ");
});
const warn = computed(() => props.choice.format === "gif" && props.estimate.bytes > GIF_WARN_BYTES);

/** Arrow keys move the format, as in a native segmented control. */
function formatKey(event: KeyboardEvent): void {
  const step = ({ ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 } as Record<string, number>)[event.key];
  if (!step || props.locked) return;
  event.preventDefault();
  const next = props.choice.format === "mp4" ? "gif" : "mp4";
  props.choice.format = next;
  const group = event.currentTarget as HTMLElement;
  requestAnimationFrame(() => group.querySelector<HTMLElement>(`[data-v="${next}"]`)?.focus());
}
</script>

<template>
  <aside class="r-export" :aria-label="t('record.export.label')">
    <div class="r-export-sec">
      <div class="r-export-head">
        <h2 class="g-section-title">{{ t("record.export.title") }}</h2>
        <span class="r-subtitle">{{ t("record.export.kept", { time: tenths(keptMs) }) }}</span>
      </div>
      <div class="g-seg r-fill" role="radiogroup" :aria-label="t('record.export.format')" @keydown="formatKey">
        <button
          v-for="f in FORMATS"
          :key="f.value"
          type="button"
          role="radio"
          :data-v="f.value"
          :aria-checked="choice.format === f.value"
          :tabindex="choice.format === f.value ? 0 : -1"
          :disabled="locked"
          @click="choice.format = f.value"
        >
          <GIcon :name="f.icon" :size="16" />{{ f.label }}
        </button>
      </div>
    </div>

    <div class="r-export-sec">
      <div class="g-group">
        <template v-if="choice.format === 'mp4'">
          <FormRow :label="t('record.export.resolution')" for="r-export-res" stacked>
            <SelectControl id="r-export-res" v-model="choice.resolution" class="r-fill" :options="resolutions" :disabled="locked" />
          </FormRow>
          <FormRow :label="t('record.export.quality')" label-id="r-export-quality" stacked>
            <div class="g-seg r-fill" role="radiogroup" aria-labelledby="r-export-quality">
              <button
                v-for="q in QUALITIES"
                :key="q.value"
                type="button"
                role="radio"
                :aria-checked="choice.quality === q.value"
                :disabled="locked"
                @click="choice.quality = q.value"
              >
                {{ t(q.label) }}
              </button>
            </div>
          </FormRow>
        </template>
        <template v-else>
          <FormRow :label="t('record.export.fpsLabel')" label-id="r-export-gif-fps" stacked>
            <div class="g-seg r-fill" role="radiogroup" aria-labelledby="r-export-gif-fps">
              <button
                v-for="f in GIF_FPS"
                :key="f"
                type="button"
                role="radio"
                :aria-checked="choice.gifFps === f"
                :disabled="locked"
                @click="choice.gifFps = f"
              >
                {{ t("record.export.fps", { fps: f }) }}
              </button>
            </div>
          </FormRow>
          <FormRow :label="t('record.export.width')" for="r-export-width" stacked>
            <SelectControl id="r-export-width" v-model="choice.gifWidth" class="r-fill" :options="widths" :disabled="locked" />
          </FormRow>
          <FormRow :label="t('record.export.loop')" for="r-export-loop">
            <SwitchControl id="r-export-loop" v-model="choice.loopForever" :disabled="locked" />
          </FormRow>
        </template>
      </div>

      <div v-if="choice.format === 'mp4' && layout === 'two'" class="g-group">
        <FormRow for="r-export-mic">
          <template #label>
            <label for="r-export-mic" class="r-switch-label"><GIcon name="microphone" :size="16" />{{ t("record.export.mic") }}</label>
          </template>
          <SwitchControl id="r-export-mic" v-model="choice.microphone" :disabled="locked" />
        </FormRow>
        <FormRow for="r-export-sys">
          <template #label>
            <label for="r-export-sys" class="r-switch-label"><GIcon name="speaker" :size="16" />{{ t("record.export.system") }}</label>
          </template>
          <SwitchControl id="r-export-sys" v-model="choice.system" :disabled="locked" />
        </FormRow>
      </div>
      <div v-else-if="choice.format === 'mp4' && layout === 'one'" class="g-group">
        <FormRow for="r-export-sound">
          <template #label>
            <label for="r-export-sound" class="r-switch-label"><GIcon name="speaker" :size="16" />{{ t("record.export.sound") }}</label>
          </template>
          <SwitchControl id="r-export-sound" v-model="sound" :disabled="locked" />
        </FormRow>
      </div>
      <p v-else-if="choice.format === 'mp4'" class="g-help">{{ t("record.export.noSound") }}</p>
      <p v-else class="g-help">{{ t("record.export.gifSilent") }}</p>

      <template v-if="choice.format === 'mp4' && layout !== 'none'">
        <div class="g-group">
          <FormRow :label="t('record.export.captions')" for="r-export-captions" stacked>
            <SelectControl
              id="r-export-captions"
              v-model="captions"
              class="r-fill"
              :options="captionOptions"
              :disabled="locked || !textReady"
            />
          </FormRow>
        </div>
        <p v-if="captionsHelp" class="g-help">{{ captionsHelp }}</p>
      </template>
    </div>

    <div class="r-export-sec r-export-est" aria-live="polite">
      <div class="r-est-label">{{ t("record.export.estimate") }}</div>
      <div class="r-est">{{ approxSize(estimate.bytes) }}</div>
      <p class="g-help">{{ meta }}</p>
      <p v-if="warn" class="g-help" data-tone="warn"><GIcon name="warning" :size="14" />{{ t("record.export.gifLarge") }}</p>
    </div>

    <slot name="foot" />
  </aside>
</template>

<style>
.r-export { display: flex; flex-direction: column; flex: none; width: 300px; min-height: 0; overflow: auto; background: var(--win-content); box-shadow: inset var(--hairline) 0 0 var(--win-line); }
.r-export-sec { padding: var(--space-16) var(--space-20); }
.r-export-sec + .r-export-sec { box-shadow: inset 0 var(--hairline) 0 var(--win-line); }
.r-export-head { display: flex; align-items: baseline; justify-content: space-between; margin-bottom: var(--space-12); }
.r-export-head > .g-section-title { margin: 0; }
.r-fill { width: 100%; }
.g-seg.r-fill > button { flex: 1; }
.r-est-label { font: var(--type-caption-strong); color: var(--win-text-2); }
.r-est { margin-top: 2px; font: var(--type-display); font-variant-numeric: tabular-nums; color: var(--win-text); }
.r-export-sec > .g-group + .g-group { margin-top: var(--space-12); }
.r-export-sec > .g-help + .g-group { margin-top: var(--space-12); }
.r-export-sec > .g-help { margin-top: var(--space-8); }
.r-export-est { margin-top: auto; }
/* `.g-form-label > label` makes a label a block; the icon and its words stay one row with a gap. */
.g-form-label > .r-switch-label { display: inline-flex; align-items: center; gap: var(--space-8); }
.r-switch-label > g-icon { flex: none; color: var(--win-text-2); }
.r-subtitle { font: var(--type-body); color: var(--win-text-2); white-space: nowrap; font-variant-numeric: tabular-nums; }
</style>
