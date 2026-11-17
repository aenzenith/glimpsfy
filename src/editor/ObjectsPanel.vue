<script setup lang="ts">
// The editor's objects panel (windows ab9): the drawn objects with the top of the stack first, in
// 36 px rows of a grip, the tool icon, the name and style, then hide (⇧⌘H), lock (⌘L) and delete.
// The eye and the lock show the current state and the tooltip names the action. Dragging a row
// reorders the stack, and the rows part where it will land.
import { computed, ref } from "vue";

import GIcon from "../components/GIcon.vue";
import TipWrap from "../components/TipWrap.vue";
import type { AnnotationObject, Color } from "../lib/annotate";
import { t, type Key } from "../lib/i18n";
import { TOOL_OF, type AnnotationEngine } from "../annotate/engine";
import { HIGHLIGHTER_FACTOR } from "../annotate/style";

const props = defineProps<{ engine: AnnotationEngine }>();
const emit = defineEmits<{ close: [] }>();

/** Top of the stack first. */
const rows = computed(() => (props.engine.rev.value, [...props.engine.drawings()].reverse()));
const selectedId = computed(() => (props.engine.rev.value, props.engine.selectedId));

const dragged = ref<string | null>(null);
/** Where the dragged row lands: its index in the list without it, top first. */
const landing = ref<number | null>(null);

function inkName(color: Color | null): string {
  if (!color) return "";
  return color.kind === "ink" ? t(`annotate.ink.${color.name}` as Key) : color.value.toUpperCase();
}

const px = (v: number) => t("annotate.width", { n: Math.round(v / props.engine.scale) });

function name(o: AnnotationObject): string {
  if (o.kind === "step") return t("annotate.step", { n: o.number });
  return t(`tool.${TOOL_OF[o.kind]}` as Key);
}

function describe(o: AnnotationObject): string {
  switch (o.kind) {
    case "arrow":
    case "line":
    case "rectangle":
    case "ellipse":
    case "pen":
      return `${inkName(o.stroke.color)} · ${px(o.stroke.width)}`;
    case "highlighter":
      return `${inkName(o.color)} · ${px(o.width / HIGHLIGHTER_FACTOR)}`;
    case "text":
      return `“${o.text.split("\n")[0]}”`;
    case "step":
      return `${inkName(o.color)} · ${px(o.diameter)}`;
    case "blur":
      return t(`overlay.${o.strength}` as Key);
    case "pixelate":
      return t(`overlay.${o.cell}` as Key);
    case "spotlight":
      return t(`overlay.${o.dim}` as Key);
    default:
      return "";
  }
}

const icon = (o: AnnotationObject) => TOOL_OF[o.kind];

function onDragStart(e: DragEvent, id: string): void {
  dragged.value = id;
  e.dataTransfer?.setData("text/plain", id);
  if (e.dataTransfer) e.dataTransfer.effectAllowed = "move";
}

function onDragOver(e: DragEvent, index: number): void {
  if (!dragged.value) return;
  e.preventDefault();
  const row = e.currentTarget as HTMLElement;
  const box = row.getBoundingClientRect();
  const from = rows.value.findIndex((o) => o.id === dragged.value);
  // The index among the other rows: before this row, or after it in its lower half.
  let at = index + (e.clientY > box.top + box.height / 2 ? 1 : 0);
  if (from >= 0 && from < at) at -= 1;
  landing.value = at;
}

function onDrop(e: DragEvent): void {
  e.preventDefault();
  const id = dragged.value;
  const at = landing.value;
  dragged.value = null;
  landing.value = null;
  if (!id || at === null) return;
  // `reorder` counts from the bottom among the other objects.
  props.engine.reorder(id, rows.value.length - 1 - at);
}

function onDragEnd(): void {
  dragged.value = null;
  landing.value = null;
}

/** The gap a dragged row opens: before row `index` of the full list. */
function gapBefore(index: number): boolean {
  if (landing.value === null || !dragged.value) return false;
  const others = rows.value.filter((o) => o.id !== dragged.value);
  const target = others[landing.value];
  return target !== undefined && rows.value[index]?.id === target.id;
}
const gapAtEnd = computed(() => {
  if (landing.value === null || !dragged.value) return false;
  return landing.value >= rows.value.length - 1;
});
</script>

<template>
  <aside class="e-objects" :aria-label="t('editor.objects')">
    <div class="e-objects-head">
      <span class="e-sec-title">{{ t("editor.objects") }}</span>
      <span class="e-count">{{ rows.length }}</span>
      <span class="g-spacer" />
      <TipWrap :tip="t('editor.close')" :caps="['⌥', '⌘', 'O']" side="below" align="end">
        <button type="button" class="g-btn" data-size="small" :aria-label="t('editor.closeObjects')" @click="emit('close')">
          <GIcon name="close" :size="16" />
        </button>
      </TipWrap>
    </div>
    <div class="g-list" role="listbox" aria-multiselectable="true" :aria-label="t('editor.list')" @dragover.prevent @drop="onDrop">
      <template v-for="(o, i) in rows" :key="o.id">
        <div v-if="gapBefore(i)" class="e-gap" aria-hidden="true" />
        <div
          class="g-row"
          role="option"
          :tabindex="selectedId === o.id ? 0 : -1"
          :aria-selected="selectedId === o.id || engine.inGroup(o.id)"
          :data-hidden="o.hidden ? '' : undefined"
          :data-dragging="dragged === o.id ? '' : undefined"
          draggable="true"
          @dragstart="onDragStart($event, o.id)"
          @dragover="onDragOver($event, i)"
          @dragend="onDragEnd"
          @click="engine.select(o.id)"
        >
          <span class="e-grip" aria-hidden="true"><GIcon name="grip" :size="16" /></span>
          <span class="g-row-lead"><GIcon :name="icon(o)" :size="16" /></span>
          <span class="g-row-text">
            <span class="g-row-title">{{ name(o) }}</span>
            <span class="g-row-sub">{{ describe(o) }}</span>
          </span>
          <span class="g-row-trail">
            <TipWrap :tip="o.hidden ? t('editor.show') : t('editor.hide')" :caps="['⇧', '⌘', 'H']">
              <button
                type="button"
                class="g-btn e-rowbtn"
                data-size="small"
                :data-on="o.hidden ? '' : undefined"
                :aria-label="t('editor.rowAction', { action: o.hidden ? t('editor.show') : t('editor.hide'), name: name(o) })"
                @click.stop="engine.setHidden(o.id, !o.hidden)"
              >
                <GIcon :name="o.hidden ? 'eye-off' : 'eye'" :size="16" />
              </button>
            </TipWrap>
            <TipWrap :tip="o.locked ? t('editor.unlock') : t('editor.lock')" :caps="['⌘', 'L']">
              <button
                type="button"
                class="g-btn e-rowbtn"
                data-size="small"
                :data-on="o.locked ? '' : undefined"
                :aria-label="t('editor.rowAction', { action: o.locked ? t('editor.unlock') : t('editor.lock'), name: name(o) })"
                @click.stop="engine.setLocked(o.id, !o.locked)"
              >
                <GIcon :name="o.locked ? 'lock' : 'unlock'" :size="16" />
              </button>
            </TipWrap>
            <TipWrap :tip="t('editor.delete')" :caps="['⌫']" align="end">
              <button
                type="button"
                class="g-btn e-rowbtn"
                data-size="small"
                :aria-label="t('editor.rowAction', { action: t('editor.delete'), name: name(o) })"
                @click.stop="engine.remove(o.id)"
              >
                <GIcon name="trash" :size="16" />
              </button>
            </TipWrap>
          </span>
        </div>
      </template>
      <div v-if="gapAtEnd" class="e-gap" aria-hidden="true" />
      <p v-if="rows.length === 0" class="e-objects-empty">{{ t("editor.objectsEmpty") }}</p>
    </div>
    <p class="e-objects-foot">{{ t("editor.objectsFoot") }}</p>
  </aside>
</template>

<style scoped>
.e-objects {
  position: relative;
  flex: none;
  width: 300px;
  display: flex;
  flex-direction: column;
  min-height: 0;
  background: var(--win-content);
  box-shadow: inset var(--hairline) 0 0 var(--win-line);
}
.e-objects-head {
  display: flex;
  align-items: center;
  gap: 6px;
  height: 48px;
  flex: none;
  padding: 0 10px 0 16px;
}
.e-sec-title {
  font: var(--type-title);
  color: var(--win-text);
}
.e-count {
  font: var(--type-num-small);
  color: var(--win-text-3);
}
.e-objects .g-list {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 0 8px;
}
.e-objects .g-row {
  gap: 8px;
  padding-right: 4px;
}
.e-objects .g-row-trail {
  gap: 0;
}
.e-grip {
  flex: none;
  display: inline-grid;
  place-items: center;
  width: 14px;
  color: var(--win-text-3);
  cursor: grab;
}
.g-row[aria-selected="true"] .e-grip {
  color: var(--win-selection-text);
}
.g-row[data-dragging] {
  opacity: 0.4;
}
.e-rowbtn {
  opacity: 0;
  transition: opacity var(--dur-fast) var(--ease-out);
}
.g-row:hover .e-rowbtn,
.g-row:focus-within .e-rowbtn,
.g-row[aria-selected="true"] .e-rowbtn,
.e-rowbtn[data-on] {
  opacity: 1;
}
.g-row[data-hidden]:not([aria-selected="true"]) :is(.g-row-title, .g-row-lead) {
  color: var(--win-text-2);
}
/* The rows part where the dragged row will land. */
.e-gap {
  height: 36px;
  margin: 0 0 0 10px;
  border-radius: var(--radius-field);
  box-shadow: inset 0 0 0 1.5px var(--iris);
}
.e-objects-empty {
  margin: 8px;
  font: var(--type-body);
  color: var(--win-text-3);
}
.e-objects-foot {
  flex: none;
  margin: 0;
  padding: 12px 16px 14px;
  font: var(--type-body);
  color: var(--win-text-2);
  box-shadow: inset 0 var(--hairline) 0 var(--win-line);
}
</style>
