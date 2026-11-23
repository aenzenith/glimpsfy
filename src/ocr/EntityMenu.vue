<script setup lang="ts">
// The menu of a found piece in the text layer (A3, design/canvas row 08, Y1): how the piece was
// read at the top (an address or a link in the monospaced face), the one-hour sentence of a date
// without an end, then the main action (Return) and the copy row (⌘C). It opens 8 px under the
// piece's line and above it where there is no room below, and stays inside the layer's box.
//
// Props: the piece's `action` (`entities.ts`), the `value` to show, an optional `note`,
// `disabled` and `problem` when no app opens the action, `active` (the row Return runs), the
// padded box of the piece's `line` and the piece's left edge `x` in CSS pixels of the layer, the
// layer's `room` and the `floor` it should not pass, the bottom of the recognised area. It emits `run` with the row picked and `hover` with the row under the pointer;
// the layer owns the keys.

import { computed, nextTick, onMounted, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import { t } from "../lib/i18n";
import { ariaKeys, chord } from "../lib/keys";
import type { EntityAction } from "./entities";
import type { Rect } from "./text";

export type MenuRow = "main" | "copy";

const props = defineProps<{
  action: EntityAction;
  value: string;
  note: string | null;
  disabled: boolean;
  problem: string | null;
  active: MenuRow;
  line: Rect;
  x: number;
  room: { width: number; height: number };
  floor: number | null;
}>();
const emit = defineEmits<{ run: [row: MenuRow]; hover: [row: MenuRow] }>();

const el = ref<HTMLElement | null>(null);
const at = ref({ left: 0, top: 0, placed: false });

/** Under the line with 8 px between them, or above it when the menu would pass the recognised
 * area's bottom or leave the layer and fits above. */
async function place(): Promise<void> {
  await nextTick();
  const menu = el.value;
  if (!menu) return;
  const { width, height } = { width: menu.offsetWidth, height: menu.offsetHeight };
  const below = props.line.y + props.line.height + 8;
  const above = props.line.y - 8 - height;
  const limit = Math.min(props.room.height, props.floor ?? Infinity) - 8;
  const top = below + height > limit && above >= 8 ? above : below;
  const left = Math.max(8, Math.min(props.room.width - width - 8, props.x - 14));
  at.value = { left, top, placed: true };
}
onMounted(place);
watch(() => [props.line, props.x, props.room, props.floor, props.value, props.note, props.problem], place, { deep: true });

const style = computed(() => ({
  left: `${at.value.left}px`,
  top: `${at.value.top}px`,
  visibility: at.value.placed ? ("visible" as const) : ("hidden" as const),
}));

function run(row: MenuRow): void {
  if (row === "main" && props.disabled) return;
  emit("run", row);
}
</script>

<template>
  <div
    ref="el"
    class="g-pop ocr-menu"
    role="menu"
    :aria-label="value"
    :data-kind="action.mono ? 'mono' : undefined"
    :style="style"
    @pointerdown.stop
    @mousedown.stop
    @click.stop
  >
    <div class="ocr-menu-value" :data-kind="action.mono ? 'mono' : undefined">{{ value }}</div>
    <p v-if="note" class="g-pop-text ocr-menu-note">{{ note }}</p>
    <div class="g-pop-sep" role="separator" />
    <button
      type="button"
      class="g-pop-item"
      role="menuitem"
      :data-state="active === 'main' && !disabled ? 'hover' : undefined"
      :aria-disabled="disabled ? 'true' : undefined"
      aria-keyshortcuts="Enter"
      @pointerenter="emit('hover', 'main')"
      @click="run('main')"
    >
      <GIcon :name="action.icon" :size="16" />
      <span class="g-pop-name">{{ t(action.label) }}</span>
      <span class="g-pop-sub">{{ chord("↩") }}</span>
    </button>
    <button
      type="button"
      class="g-pop-item"
      role="menuitem"
      :data-state="active === 'copy' ? 'hover' : undefined"
      :aria-keyshortcuts="ariaKeys('Meta+C')"
      @pointerenter="emit('hover', 'copy')"
      @click="run('copy')"
    >
      <GIcon name="copy" :size="16" />
      <span class="g-pop-name">{{ t(action.copyLabel) }}</span>
      <span class="g-pop-sub">{{ chord("⌘C") }}</span>
    </button>
    <p v-if="problem" class="g-pop-help ocr-menu-warn" role="status">{{ problem }}</p>
  </div>
</template>

<style scoped>
.ocr-menu {
  position: absolute;
  z-index: 4;
  width: max-content;
  min-width: 248px;
  max-width: min(360px, calc(100% - 16px));
  pointer-events: auto;
}
/* An address or a link wraps anywhere inside 300 px, so a long one stays readable whole. */
.ocr-menu[data-kind="mono"] {
  max-width: min(300px, calc(100% - 16px));
}
.ocr-menu-value {
  padding: var(--space-8) var(--space-8) var(--space-4);
  font: var(--type-title);
  color: var(--text);
}
.ocr-menu-value[data-kind="mono"] {
  font: 500 12px/17px var(--font-mono);
  overflow-wrap: anywhere;
}
.ocr-menu-note {
  padding-bottom: var(--space-4);
}
.ocr-menu > .g-pop-item > g-icon {
  color: var(--text-muted);
}
.ocr-menu-warn {
  padding-top: var(--space-4);
  color: var(--warn);
}
</style>
