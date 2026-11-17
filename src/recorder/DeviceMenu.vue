<script setup lang="ts">
// The microphone and camera menus (design `micMenu`): each device with its kind (built-in, USB,
// Bluetooth, virtual) and, for microphones, its live meter, with a check on the chosen one.
// ↑ and ↓ move between the devices.
import { nextTick, onMounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import type { Device } from "../lib/api/record";
import { t } from "../lib/i18n";
import { transportLabel } from "./format";

const props = defineProps<{
  title: string;
  devices: readonly Device[];
  chosen: string | null;
  /** Live levels by device id, for microphones; cameras have none. */
  levels: Record<string, number> | null;
  /** Shown when the list is empty. */
  empty: string;
}>();
const emit = defineEmits<{ pick: [id: string] }>();

const root = ref<HTMLElement | null>(null);

function items(): HTMLElement[] {
  return [...(root.value?.querySelectorAll<HTMLElement>(".g-pop-item") ?? [])];
}

function key(event: KeyboardEvent): void {
  const step = ({ ArrowDown: 1, ArrowUp: -1 } as Record<string, number>)[event.key];
  if (!step) return;
  event.preventDefault();
  const list = items();
  const index = list.indexOf(document.activeElement as HTMLElement);
  list[(index + step + list.length) % list.length]?.focus();
}

onMounted(async () => {
  await nextTick();
  const list = items();
  (list.find((item) => item.getAttribute("aria-checked") === "true") ?? list[0])?.focus({ preventScroll: true });
});

const level = (id: string) => Math.max(0, Math.min(1, props.levels?.[id] ?? 0));
</script>

<template>
  <div ref="root" class="g-pop g-glass r-menu" data-material="strong" role="menu" :aria-label="title" @keydown="key">
    <div class="g-pop-head">{{ title }}</div>
    <button
      v-for="device in devices"
      :key="device.id"
      type="button"
      class="g-pop-item"
      role="menuitemradio"
      :aria-checked="device.id === chosen"
      @click="emit('pick', device.id)"
    >
      <GIcon name="check" :size="16" class="g-pop-check" />
      <span class="g-pop-name">{{ device.name }}</span>
      <span v-if="transportLabel(device.transport)" class="g-pop-sub">{{ transportLabel(device.transport) }}</span>
      <span
        v-if="levels"
        class="g-meter"
        role="meter"
        :aria-label="t('record.meter.level', { name: device.name })"
        aria-valuemin="0"
        aria-valuemax="1"
        :aria-valuenow="level(device.id).toFixed(2)"
        :style="{ '--level': level(device.id) }"
      />
    </button>
    <p v-if="!devices.length" class="g-pop-help r-menu-empty">{{ empty }}</p>
  </div>
</template>

<style>
.r-menu { --pop-w: 300px; }
.r-menu .g-pop-check { color: var(--iris-text); }
.r-menu-empty { padding-top: var(--space-4); }
</style>
