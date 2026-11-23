<script setup lang="ts">
// The explanation for a blocked microphone or camera (design `permPop`): what macOS blocks, Open
// System Settings (focused) and Record without it, which turns the source off.
import { nextTick, onMounted, ref } from "vue";

import { t, tp } from "../lib/i18n";
import { can } from "../lib/platform";

const props = defineProps<{ device: "microphone" | "camera" }>();
const emit = defineEmits<{ open: []; without: [] }>();

const primary = ref<HTMLButtonElement | null>(null);

const TEXT = {
  microphone: { label: "record.perm.mic.label", title: "record.perm.mic.title", text: "record.perm.mic.text" },
  camera: { label: "record.perm.camera.label", title: "record.perm.camera.title", text: "record.perm.camera.text" },
} as const;

onMounted(async () => {
  await nextTick();
  primary.value?.focus({ preventScroll: true });
});

const text = TEXT[props.device];
</script>

<template>
  <div class="g-pop g-glass" data-material="strong" role="dialog" :aria-label="t(text.label)">
    <p class="g-pop-title">{{ t(text.title) }}</p>
    <p class="g-pop-text">{{ tp(text.text) }}</p>
    <div class="g-pop-actions">
      <button v-if="can('systemSettingsPanes')" ref="primary" type="button" class="g-button" data-variant="primary" @click="emit('open')">
        {{ tp("record.perm.open") }}
      </button>
      <button type="button" class="g-button" data-variant="plain" @click="emit('without')">{{ t("record.perm.without") }}</button>
    </div>
  </div>
</template>
