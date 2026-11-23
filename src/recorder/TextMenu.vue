<script setup lang="ts">
// The small menus of the Text panel (design row 22, `menu`): a right click on the text or on a gap
// of the timeline, and the Shorten button's choices. It opens at a point, or under a button with
// its right edge on the point (`align: "end"`); ↑ and ↓ move, Return picks, Esc and a press
// elsewhere close it. An item marked `keep` leaves the menu open, as a threshold does, so the
// counts it changes can be read before the cut is made.
import { nextTick, onBeforeUnmount, onMounted, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import { chord } from "../lib/keys";

export type TextMenuItem =
  | { label: string; icon?: string; keys?: string; checked?: boolean; disabled?: boolean; keep?: boolean; run: () => void }
  | { heading: string }
  | "separator";

const props = defineProps<{
  items: readonly TextMenuItem[];
  at: { x: number; y: number };
  align?: "start" | "end";
  label: string;
}>();
const emit = defineEmits<{ close: [] }>();

const root = ref<HTMLElement | null>(null);
const pos = ref({ left: props.at.x, top: props.at.y, visibility: "hidden" as "hidden" | "visible" });

function buttons(): HTMLButtonElement[] {
  return [...(root.value?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? [])];
}

function outside(event: MouseEvent): void {
  if (root.value && !root.value.contains(event.target as Node)) emit("close");
}

function close(): void {
  emit("close");
}

onMounted(async () => {
  await nextTick();
  const el = root.value;
  if (el) {
    const r = el.getBoundingClientRect();
    const x = props.align === "end" ? props.at.x - r.width : props.at.x;
    pos.value = {
      left: Math.max(8, Math.min(x, window.innerWidth - r.width - 8)),
      top: props.at.y + r.height > window.innerHeight - 8 ? Math.max(8, props.at.y - r.height - 8) : props.at.y,
      visibility: "visible",
    };
  }
  (buttons().find((b) => b.getAttribute("aria-checked") === "true") ?? buttons()[0])?.focus({ preventScroll: true });
  // The press that opened the menu is over by now; only a later one closes it.
  window.addEventListener("mousedown", outside, true);
  window.addEventListener("blur", close);
  window.addEventListener("resize", close);
});

onBeforeUnmount(() => {
  window.removeEventListener("mousedown", outside, true);
  window.removeEventListener("blur", close);
  window.removeEventListener("resize", close);
});

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

function pick(item: Exclude<TextMenuItem, "separator" | { heading: string }>): void {
  if (!item.keep) emit("close");
  item.run();
}
</script>

<template>
  <div
    ref="root"
    class="g-menu r-tmenu"
    role="menu"
    :aria-label="label"
    :style="{ left: `${pos.left}px`, top: `${pos.top}px`, visibility: pos.visibility }"
    @keydown="key"
    @contextmenu.prevent
  >
    <template v-for="(item, i) in items" :key="i">
      <hr v-if="item === 'separator'" class="g-menu-separator" />
      <div v-else-if="'heading' in item" class="g-menu-label" role="presentation">{{ item.heading }}</div>
      <button
        v-else
        type="button"
        class="g-menu-item"
        :role="item.checked === undefined ? 'menuitem' : 'menuitemradio'"
        :aria-checked="item.checked === undefined ? undefined : item.checked"
        :disabled="item.disabled"
        @click="pick(item)"
      >
        <GIcon v-if="item.checked !== undefined" class="g-menu-check" name="check" :size="16" />
        <GIcon v-else-if="item.icon" :name="item.icon" :size="16" />
        <span class="r-tmenu-label">{{ item.label }}</span>
        <span v-if="item.keys" class="g-menu-key">{{ chord(item.keys) }}</span>
      </button>
    </template>
  </div>
</template>

<style>
.r-tmenu { position: fixed; z-index: 40; min-width: 214px; }
.r-tmenu-label { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
</style>
