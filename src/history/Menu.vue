<script setup lang="ts">
// A small menu at a point (a card's More button, a right click): arrow keys move, Return picks,
// Esc and a click elsewhere close it. Its items are the caller's.
import { nextTick, onBeforeUnmount, onMounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";

export type MenuItem = { label: string; icon: string; keys?: string; destructive?: boolean; disabled?: boolean; run: () => void } | "separator";

const props = defineProps<{ items: readonly MenuItem[]; at: { x: number; y: number }; label: string }>();
const emit = defineEmits<{ close: [] }>();

const root = ref<HTMLElement | null>(null);
const pos = ref({ left: props.at.x, top: props.at.y });

function buttons(): HTMLButtonElement[] {
  return [...(root.value?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
}
function outside(event: MouseEvent): void {
  if (root.value && !root.value.contains(event.target as Node)) emit("close");
}
onMounted(async () => {
  await nextTick();
  const el = root.value;
  if (el) {
    const r = el.getBoundingClientRect();
    pos.value = {
      left: Math.max(8, Math.min(props.at.x, window.innerWidth - r.width - 8)),
      top: Math.max(8, props.at.y + r.height > window.innerHeight - 8 ? props.at.y - r.height - 8 : props.at.y),
    };
  }
  buttons()[0]?.focus();
  window.addEventListener("mousedown", outside, true);
  window.addEventListener("blur", close);
});
onBeforeUnmount(() => {
  window.removeEventListener("mousedown", outside, true);
  window.removeEventListener("blur", close);
});
function close(): void {
  emit("close");
}

function key(event: KeyboardEvent): void {
  const list = buttons();
  const index = list.indexOf(document.activeElement as HTMLButtonElement);
  if (event.key === "Escape" || event.key === "Tab") emit("close");
  else if (event.key === "ArrowDown") list[(index + 1) % list.length]?.focus();
  else if (event.key === "ArrowUp") list[(index - 1 + list.length) % list.length]?.focus();
  else return;
  event.preventDefault();
  event.stopPropagation();
}
function pick(item: Exclude<MenuItem, "separator">): void {
  emit("close");
  item.run();
}
</script>

<template>
  <div ref="root" class="g-menu hv-menu" role="menu" :aria-label="label" :style="{ left: `${pos.left}px`, top: `${pos.top}px` }" @keydown="key">
    <template v-for="(item, i) in items" :key="i">
      <hr v-if="item === 'separator'" class="g-menu-separator" />
      <button
        v-else
        type="button"
        class="g-menu-item"
        role="menuitem"
        :data-variant="item.destructive ? 'destructive' : undefined"
        :disabled="item.disabled"
        @click="pick(item)"
      >
        <GIcon :name="item.icon" :size="16" /><span style="flex: 1">{{ item.label }}</span><kbd v-if="item.keys" class="g-kbd">{{ item.keys }}</kbd>
      </button>
    </template>
  </div>
</template>
