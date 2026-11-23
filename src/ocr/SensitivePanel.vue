<script setup lang="ts">
// The list of smart redaction (D28): what `useSensitive` found in the area, one row per item with
// its kind, its text and a switch, grouped by kind past twelve items, then the method (blur or
// pixelate) and Redact (↩). In the overlay it is a 264 px glass panel in the action bar's place
// (`variant="glass"`); in the editor it takes the objects panel's place (`variant="side"`). The
// page handles ↩ and Esc and places the glass panel.
import { computed } from "vue";

import GIcon from "../components/GIcon.vue";
import type { EntityKind } from "../lib/api/ocr";
import { t, type Key } from "../lib/i18n";
import type { Method, Sensitive, SensitiveItem } from "./sensitive";

const props = withDefaults(defineProps<{ sensitive: Sensitive; variant?: "glass" | "side" }>(), { variant: "glass" });
const emit = defineEmits<{ close: []; apply: []; settings: [] }>();

const s = props.sensitive;
const side = computed(() => props.variant === "side");
const count = computed(() => s.items.value.length);
const chosen = computed(() => s.chosen.value.length);
const applyLabel = computed(() => (chosen.value === 1 || count.value === 1 ? t("sensitive.apply.one") : t("sensitive.apply.many", { n: chosen.value })));
const problem = computed(() => s.phase.value === "missing" || s.phase.value === "unavailable");

const kindName = (kind: EntityKind) => t(`sensitive.kind.${kind}` as Key);
const groupName = (kind: EntityKind) => t(`sensitive.group.${kind}` as Key);
const isOn = (item: SensitiveItem) => !s.off.value.has(item.id);
const groupOn = (items: SensitiveItem[]) => items.every(isOn);

const METHODS: { id: Method; label: Key }[] = [
  { id: "blur", label: "tool.blur" },
  { id: "pixelate", label: "tool.pixelate" },
];

function hover(item: SensitiveItem | null): void {
  s.hovered.value = item ? item.id : null;
}
</script>

<template>
  <div
    :class="side ? 's-side' : 'g-glass s-glass'"
    class="s-panel"
    role="dialog"
    :aria-label="t('sensitive.title')"
    @mousedown.stop
    @pointerdown.stop
  >
    <template v-if="problem">
      <div class="s-problem">
        <div class="s-problem-title">
          <GIcon name="bang" :size="16" />
          <span>{{ t("sensitive.unavailable") }}</span>
          <span class="g-spacer" />
          <button type="button" class="g-btn" data-size="small" :aria-label="t('action.close')" @click="emit('close')">
            <GIcon name="close" :size="16" />
          </button>
        </div>
        <p class="s-problem-text">{{ s.phase.value === "missing" ? t("ocr.modelsMissing") : t("ocr.unavailable") }}</p>
        <div v-if="s.phase.value === 'missing'" class="s-problem-actions">
          <button type="button" class="g-button" data-variant="primary" @click="emit('settings')">{{ t("ocr.openSettings") }}</button>
        </div>
      </div>
    </template>

    <template v-else>
      <div class="s-head">
        <b>{{ t("sensitive.title") }}</b>
        <span v-if="s.phase.value === 'done'">{{ count }}</span>
        <span class="g-spacer" />
        <button type="button" class="g-btn" data-size="small" :aria-label="t('action.close')" @click="emit('close')">
          <GIcon name="close" :size="16" />
        </button>
      </div>

      <p v-if="s.phase.value === 'reading'" class="s-note" role="status">{{ t("ocr.reading") }}</p>
      <p v-else-if="s.phase.value === 'failed'" class="s-note" data-tone="warn" role="alert">{{ t("sensitive.failed") }}</p>
      <p v-else-if="count === 0" class="s-note">{{ t("sensitive.none") }}</p>

      <template v-else>
        <div class="s-list" :class="{ 'g-list': side }" role="list" @pointerleave="hover(null)">
          <template v-for="row in s.rows.value" :key="row.type === 'item' ? row.item.id : `g-${row.kind}`">
            <template v-if="row.type === 'item'">
              <div class="s-row" role="listitem" :data-hl="s.hovered.value === row.item.id ? '' : undefined" @pointerenter="hover(row.item)">
                <span v-if="side" class="s-lead"><GIcon name="shield" :size="16" /></span>
                <span class="s-text">
                  <b class="s-kind">{{ kindName(row.item.kind) }}</b>
                  <GIcon v-if="row.item.cut && !side" class="s-bang" name="bang" :size="14" />
                  <span class="s-value">{{ row.item.text }}</span>
                </span>
                <GIcon v-if="row.item.cut && side" class="s-bang" name="bang" :size="14" />
                <input
                  type="checkbox"
                  role="switch"
                  class="g-switch"
                  :checked="isOn(row.item)"
                  :aria-label="`${kindName(row.item.kind)}: ${row.item.text}`"
                  @change="s.toggle(row.item.id)"
                />
              </div>
            </template>
            <template v-else>
              <div class="s-row" role="listitem" data-group>
                <button
                  type="button"
                  class="g-btn s-chev"
                  data-size="small"
                  :aria-expanded="row.open"
                  :aria-label="groupName(row.kind)"
                  @click="s.expand(row.kind)"
                >
                  <GIcon :name="row.open ? 'chevron-down' : 'chevron-right'" :size="14" />
                </button>
                <b class="s-kind">{{ groupName(row.kind) }}</b>
                <span class="s-count">{{ row.items.length }}</span>
                <input
                  type="checkbox"
                  role="switch"
                  class="g-switch"
                  :checked="groupOn(row.items)"
                  :aria-label="groupName(row.kind)"
                  @change="s.toggleGroup(row.kind)"
                />
              </div>
              <div
                v-for="item in row.shown"
                :key="item.id"
                class="s-row"
                role="listitem"
                :data-hl="s.hovered.value === item.id ? '' : undefined"
                @pointerenter="hover(item)"
              >
                <span class="s-text">
                  <b class="s-kind">{{ kindName(item.kind) }}</b>
                  <GIcon v-if="item.cut" class="s-bang" name="bang" :size="14" />
                  <span class="s-value">{{ item.text }}</span>
                </span>
                <input
                  type="checkbox"
                  role="switch"
                  class="g-switch"
                  :checked="isOn(item)"
                  :aria-label="`${kindName(item.kind)}: ${item.text}`"
                  @change="s.toggle(item.id)"
                />
              </div>
              <button v-if="row.more > 0" type="button" class="g-button s-more" data-variant="plain" @click="s.showAll(row.kind)">
                {{ t("sensitive.more", { n: row.more }) }}
              </button>
            </template>
          </template>
        </div>

        <p v-if="s.cutCount.value > 0" class="s-note" data-tone="warn">
          {{ s.cutCount.value === 1 ? t("sensitive.cut.one") : t("sensitive.cut.many", { n: s.cutCount.value }) }}
        </p>

        <div class="s-foot">
          <div class="g-seg" role="radiogroup" :aria-label="t('overlay.options')">
            <button
              v-for="m in METHODS"
              :key="m.id"
              type="button"
              role="radio"
              :aria-checked="s.method.value === m.id"
              :aria-label="t(m.label)"
              @click="s.setMethod(m.id)"
            >
              <GIcon :name="m.id" :size="16" />
            </button>
          </div>
          <button type="button" class="g-button" data-variant="primary" :disabled="chosen === 0" @click="emit('apply')">
            {{ applyLabel }}<kbd class="g-kbd">↩</kbd>
          </button>
        </div>
      </template>
    </template>
  </div>
</template>

<style scoped>
.s-panel {
  display: flex;
  flex-direction: column;
  min-height: 0;
}
/* The overlay's glass panel (D28 board, Y2). */
.s-glass {
  width: 264px;
  padding: 6px;
  border-radius: var(--radius-card);
}
/* The editor's side panel, in the objects panel's place and measure. */
.s-side {
  position: relative;
  flex: none;
  width: 300px;
  background: var(--win-content);
  box-shadow: inset var(--hairline) 0 0 var(--win-line);
}
.s-head {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: none;
  height: 32px;
  padding: 0 2px 0 8px;
}
.s-side .s-head {
  height: 48px;
  padding: 0 10px 0 16px;
}
.s-head > b {
  font: var(--type-title);
  color: var(--text);
}
.s-head > span:not(.g-spacer) {
  font: var(--type-num-small);
  color: var(--text-muted);
}
.s-side .s-head > b {
  color: var(--win-text);
}
.s-side .s-head > span:not(.g-spacer) {
  color: var(--win-text-3);
}
.s-list {
  flex: 1 1 auto;
  min-height: 0;
  overflow: auto;
}
.s-side .s-list {
  padding: 0 8px;
}
.s-row {
  display: flex;
  align-items: center;
  gap: 8px;
  height: 28px;
  padding: 0 6px 0 8px;
  border-radius: var(--radius-lens);
}
.s-side .s-row {
  height: 36px;
  border-radius: var(--radius-field);
}
.s-row[data-hl] {
  background: var(--surface-hover);
}
.s-side .s-row[data-hl] {
  background: var(--win-hover);
}
.s-text {
  flex: 1;
  min-width: 0;
  display: flex;
  align-items: center;
  gap: 8px;
}
.s-kind {
  flex: none;
  width: 96px;
  font: var(--type-label);
  color: var(--text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.s-value {
  flex: 1;
  min-width: 0;
  font: var(--type-body);
  color: var(--text-muted);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.s-side .s-kind {
  color: var(--win-text);
}
.s-side .s-value {
  color: var(--win-text-2);
}
.s-lead {
  flex: none;
  display: inline-grid;
  place-items: center;
  color: var(--win-text-2);
}
.s-bang {
  flex: none;
  color: var(--warn);
}
.s-row[data-group] > .s-kind {
  flex: 1;
  width: auto;
}
.s-count {
  flex: none;
  font: var(--type-num-small);
  color: var(--text-muted);
}
.s-side .s-count {
  color: var(--win-text-3);
}
.s-chev {
  flex: none;
  margin-left: -4px;
  color: var(--text-muted);
}
.s-more {
  align-self: flex-start;
  margin: 0 0 2px;
}
.g-switch {
  flex: none;
}
.s-note {
  margin: 0;
  padding: 4px 8px 6px;
  font: var(--type-body);
  color: var(--text-muted);
}
.s-side .s-note {
  padding: 8px 16px;
  color: var(--win-text-2);
}
.s-note[data-tone="warn"] {
  color: var(--warn);
}
.s-foot {
  display: flex;
  align-items: center;
  gap: 6px;
  flex: none;
  padding: 8px 2px 2px;
}
.s-side .s-foot {
  gap: 8px;
  padding: 10px 12px;
  box-shadow: inset 0 var(--hairline) 0 var(--win-line);
}
.s-foot > .g-button {
  flex: 1;
  justify-content: center;
}
.s-foot > .g-button > .g-kbd {
  margin-left: 2px;
}
.s-problem {
  padding: 6px 4px 4px 8px;
}
.s-side .s-problem {
  padding: 14px 10px 14px 16px;
}
.s-problem-title {
  display: flex;
  align-items: center;
  gap: 8px;
  min-height: 24px;
  font: var(--type-title);
  color: var(--text);
}
.s-problem-title > g-icon {
  color: var(--warn);
}
.s-side .s-problem-title {
  color: var(--win-text);
}
.s-problem-text {
  margin: 6px 0 0;
  font: var(--type-body);
  color: var(--text-muted);
}
.s-side .s-problem-text {
  color: var(--win-text-2);
}
.s-problem-actions {
  display: flex;
  justify-content: flex-end;
  padding-top: 10px;
}
</style>
