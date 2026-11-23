<script setup lang="ts">
// The options popover (design `morePop`): frame rate, resolution with the file's pixel size, the
// time limit, highlight clicks and show keystrokes. The choices persist in Settings › Recording,
// which the bar page writes; the core recomputes the file's size after a change. A time limit
// whose take would not fit on the disk gets a warning, from the writers' bit rate.
import { computed, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import SegmentedControl from "../components/SegmentedControl.vue";
import SwitchControl from "../components/SwitchControl.vue";
import type { PhysicalSize } from "../lib/api/capture";
import type { Resolution } from "../lib/api/record";
import { language, t } from "../lib/i18n";
import { clock } from "./format";

const props = defineProps<{
  fps: number;
  resolution: Resolution;
  outputSize: PhysicalSize;
  clicks: boolean;
  keys: boolean;
  /** Settings › Recording › Time limit in minutes, `null` for none. */
  limitMinutes: number | null;
  /** About how many bytes a take writes per minute, and the free bytes on the recordings
   * folder's volume (`null` when unknown), for the disk warning. */
  bytesPerMinute: number;
  freeBytes: number | null;
}>();
const emit = defineEmits<{
  fps: [fps: number];
  resolution: [resolution: Resolution];
  clicks: [on: boolean];
  keys: [on: boolean];
  limit: [minutes: number | null];
}>();

const FPS = [
  { value: 30, label: "30" },
  { value: 60, label: "60" },
] as const;
const RESOLUTIONS = [
  { value: "native" as const, label: "record.options.native" as const },
  { value: "half" as const, label: "record.options.half" as const },
];

/** The limits offered as one tap; Custom opens the minutes field. */
const PRESETS = [1, 5, 15] as const;
const MAX_MINUTES = 240;
/** What Custom starts with when it is picked first. */
const CUSTOM_START = 90;
type LimitChoice = "none" | "custom" | (typeof PRESETS)[number];

const isPreset = (m: number | null): m is (typeof PRESETS)[number] => PRESETS.includes(m as (typeof PRESETS)[number]);
// Custom stays chosen while the field is in use, even when it holds a preset's number.
const custom = ref(props.limitMinutes !== null && !isPreset(props.limitMinutes));
const draft = ref(String(props.limitMinutes ?? CUSTOM_START));
watch(
  () => props.limitMinutes,
  (m) => {
    if (m === null) custom.value = false;
    else if (!isPreset(m)) custom.value = true;
    if (m !== null) draft.value = String(m);
  },
);

const choice = computed<LimitChoice>(() => {
  const m = props.limitMinutes;
  if (m === null) return "none";
  return custom.value || !isPreset(m) ? "custom" : m;
});
const LIMITS = computed(() => [
  { value: "none" as LimitChoice, label: t("record.options.limitNone") },
  ...PRESETS.map((n) => ({ value: n as LimitChoice, label: t("record.options.limitMinutes", { n }) })),
  { value: "custom" as LimitChoice, label: t("record.options.limitCustom") },
]);

function pickLimit(next: LimitChoice): void {
  if (next === "custom") {
    custom.value = true;
    const kept = props.limitMinutes ?? CUSTOM_START;
    draft.value = String(kept);
    emit("limit", kept);
    return;
  }
  custom.value = false;
  emit("limit", next === "none" ? null : next);
}

function commitDraft(): void {
  const n = Math.round(Number(draft.value));
  const minutes = Number.isFinite(n) && n > 0 ? Math.min(MAX_MINUTES, n) : (props.limitMinutes ?? CUSTOM_START);
  draft.value = String(minutes);
  if (minutes !== props.limitMinutes) emit("limit", minutes);
}

const limitMs = computed(() => (props.limitMinutes === null ? 0 : props.limitMinutes * 60_000));
const size = (bytes: number) =>
  `${new Intl.NumberFormat(language.value, { maximumFractionDigits: 1, minimumFractionDigits: 1 }).format(bytes / 1e9)} GB`;
/** The take of the limit would not fit in the free space: the disk would stop it first. */
const tooBig = computed(() => {
  if (props.limitMinutes === null || props.freeBytes === null) return null;
  const need = props.bytesPerMinute * props.limitMinutes;
  return need > props.freeBytes ? { need: size(need), free: size(props.freeBytes) } : null;
});
</script>

<template>
  <div class="g-pop g-glass r-options" data-material="strong" role="dialog" :aria-label="t('record.more')">
    <div class="g-pop-row">
      <span id="r-opt-fps" class="g-pop-label"><GIcon name="film" :size="16" />{{ t("record.options.fps") }}</span>
      <SegmentedControl :model-value="fps" :options="FPS" labelledby="r-opt-fps" @update:model-value="emit('fps', $event)" />
    </div>
    <div class="g-pop-row">
      <span id="r-opt-res" class="g-pop-label"><GIcon name="fit" :size="16" />{{ t("record.options.resolution") }}</span>
      <SegmentedControl
        :model-value="resolution"
        :options="RESOLUTIONS.map((r) => ({ value: r.value, label: t(r.label) }))"
        labelledby="r-opt-res"
        @update:model-value="emit('resolution', $event)"
      />
    </div>
    <p class="g-pop-help r-pop-indent">{{ t("record.options.pixels", { width: outputSize.width, height: outputSize.height }) }}</p>
    <div class="g-pop-sep" role="separator" />
    <div class="g-pop-row">
      <span id="r-opt-limit" class="g-pop-label"><GIcon name="timer" :size="16" />{{ t("record.options.limit") }}</span>
    </div>
    <div class="r-limit">
      <SegmentedControl :model-value="choice" :options="LIMITS" labelledby="r-opt-limit" @update:model-value="pickLimit" />
    </div>
    <label v-if="choice === 'custom'" class="g-pop-row r-pop-indent">
      <span class="g-pop-label">{{ t("record.options.limitField") }}</span>
      <input
        v-model="draft"
        class="r-limit-field"
        type="number"
        inputmode="numeric"
        min="1"
        :max="MAX_MINUTES"
        @change="commitDraft"
        @keydown.enter="commitDraft"
      />
    </label>
    <p v-if="tooBig" class="g-pop-help r-limit-warn">
      <GIcon name="warning" :size="16" class="r-warn" /><span>{{
        t("record.options.limitDisk", { time: clock(limitMs), need: tooBig.need, free: tooBig.free })
      }}</span>
    </p>
    <p v-else-if="limitMinutes !== null" class="g-pop-help r-pop-indent">{{ t("record.options.limitHelp", { time: clock(limitMs) }) }}</p>
    <div class="g-pop-sep" role="separator" />
    <label class="g-pop-row">
      <span class="g-pop-label"><GIcon name="click" :size="16" />{{ t("record.options.clicks") }}</span>
      <SwitchControl :model-value="clicks" @update:model-value="emit('clicks', $event)" />
    </label>
    <label class="g-pop-row">
      <span class="g-pop-label"><GIcon name="keyboard" :size="16" />{{ t("record.options.keys") }}</span>
      <SwitchControl :model-value="keys" @update:model-value="emit('keys', $event)" />
    </label>
    <div class="g-pop-sep" role="separator" />
    <p class="g-pop-foot">{{ t("record.options.persist") }}</p>
  </div>
</template>

<style>
.r-pop-indent { padding-left: 32px; }
.r-limit { padding: 0 var(--space-4) var(--space-8) var(--space-8); }
.r-limit > .g-seg { display: flex; width: 100%; }
.r-limit > .g-seg > button { flex: 1; }
.r-limit-field {
  width: 72px; height: 28px; margin-left: auto; padding: 0 8px; border: 0; border-radius: var(--radius-thumb); outline: 0;
  -webkit-appearance: none; appearance: none;
  background: var(--control-hover); box-shadow: inset 0 0 0 var(--hairline, 1px) var(--edge-inner);
  font: var(--type-label); font-variant-numeric: tabular-nums; color: var(--text);
}
.r-limit-field:focus { outline: 2px solid var(--surface-focus); outline-offset: 1px; }
.r-limit-field::-webkit-inner-spin-button { -webkit-appearance: none; margin: 0; }
.r-limit-warn { display: flex; gap: var(--space-8); align-items: flex-start; }
.r-limit-warn > .r-warn { flex: none; margin-top: 1px; }
.r-options { --pop-w: 300px; }
</style>
