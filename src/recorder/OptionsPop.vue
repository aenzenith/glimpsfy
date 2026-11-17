<script setup lang="ts">
// The options popover (design `morePop`): frame rate, resolution with the file's pixel size,
// highlight clicks and show keystrokes. The choices persist in Settings › Recording, which the bar
// page writes; the core recomputes the file's size after a change.
import GIcon from "../components/GIcon.vue";
import SegmentedControl from "../components/SegmentedControl.vue";
import SwitchControl from "../components/SwitchControl.vue";
import type { PhysicalSize } from "../lib/api/capture";
import type { Resolution } from "../lib/api/record";
import { t } from "../lib/i18n";

defineProps<{
  fps: number;
  resolution: Resolution;
  outputSize: PhysicalSize;
  clicks: boolean;
  keys: boolean;
}>();
const emit = defineEmits<{
  fps: [fps: number];
  resolution: [resolution: Resolution];
  clicks: [on: boolean];
  keys: [on: boolean];
}>();

const FPS = [
  { value: 30, label: "30" },
  { value: 60, label: "60" },
] as const;
const RESOLUTIONS = [
  { value: "native" as const, label: "record.options.native" as const },
  { value: "half" as const, label: "record.options.half" as const },
];
</script>

<template>
  <div class="g-pop g-glass" data-material="strong" role="dialog" :aria-label="t('record.more')">
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
</style>
