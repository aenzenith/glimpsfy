<script setup lang="ts">
// The selection's size label as a control (DESIGN.md "Overlay", the size menu): the size in frame
// pixels, the locked ratio or fixed size after a lock, and the chevron that opens the size menu. A
// press on a number turns the numbers into fields: Tab goes to the other one, Return applies and
// Esc leaves them. The page writes the numbers itself (`write`), so a drag that changes them every
// frame does not render the label again. The capture overlay and the recording setup use it.
import { nextTick, onUpdated, ref } from "vue";

import GIcon from "./GIcon.vue";
import { t } from "../lib/i18n";

const props = defineProps<{
  /** What follows the lock: a ratio ("16:9") or a fixed size's name; `null` while free. */
  lock: { text: string; name: boolean } | null;
  /** The size had to shrink to fit the screen. */
  warn: boolean;
  /** Whether the size menu is open. */
  open: boolean;
  /** Only the choice, without numbers: the label at the crosshair before a drag. */
  bare?: boolean;
  /** Whether a press on a number lets the user type the size. */
  editable: boolean;
  /** The smallest size Return applies, in frame pixels. */
  min: { width: number; height: number };
  /** The beautify glyph after the numbers: iris while the capture is beautified, faint once F
   * skipped it; none without a beautify style. */
  beautify?: "on" | "off" | null;
}>();

const emit = defineEmits<{ menu: []; apply: [width: number, height: number] }>();

const root = ref<HTMLElement | null>(null);
const wEl = ref<HTMLElement | null>(null);
const hEl = ref<HTMLElement | null>(null);
const wIn = ref<HTMLInputElement | null>(null);
const hIn = ref<HTMLInputElement | null>(null);
const editing = ref(false);
const bad = ref(false);
const typed = ref({ w: "", h: "" });
let shown = { w: 0, h: 0 };

function paint(): void {
  if (wEl.value && wEl.value.textContent !== `${shown.w}`) wEl.value.textContent = `${shown.w}`;
  if (hEl.value && hEl.value.textContent !== `${shown.h}`) hEl.value.textContent = `${shown.h}`;
}

/** Shows `w` × `h` without a render. */
function write(w: number, h: number): void {
  shown = { w, h };
  paint();
}

// A render that put the numbers back (after the fields, or as buttons instead of text) leaves
// them empty until they are written again.
onUpdated(paint);

async function edit(axis: "w" | "h"): Promise<void> {
  if (!props.editable) return;
  typed.value = { w: `${shown.w}`, h: `${shown.h}` };
  bad.value = false;
  editing.value = true;
  await nextTick();
  const field = axis === "w" ? wIn.value : hIn.value;
  field?.focus();
  field?.select();
}

function leave(): void {
  editing.value = false;
  bad.value = false;
}

const digits = (v: string) => v.replace(/\D+/g, "").slice(0, 5);

function onInput(axis: "w" | "h", e: Event): void {
  const input = e.target as HTMLInputElement;
  const clean = digits(input.value);
  if (clean !== input.value) input.value = clean;
  typed.value = { ...typed.value, [axis]: clean };
  bad.value = false;
}

function onFieldKey(axis: "w" | "h", e: KeyboardEvent): void {
  if (e.key === "Tab") {
    e.preventDefault();
    const other = axis === "w" ? hIn.value : wIn.value;
    other?.focus();
    other?.select();
  } else if (e.key === "Enter") {
    e.preventDefault();
    const w = Number(typed.value.w);
    const h = Number(typed.value.h);
    // A size below the smallest selection stays in the fields, in amber, until it is fixed.
    if (!(w >= props.min.width && h >= props.min.height)) {
      bad.value = true;
      return;
    }
    leave();
    emit("apply", w, h);
  } else if (e.key === "Escape") {
    e.preventDefault();
    leave();
  }
}

/** Leaving both fields for anything else drops what was typed. */
function onBlur(e: FocusEvent): void {
  const next = e.relatedTarget as Node | null;
  if (next && root.value?.contains(next)) return;
  leave();
}

defineExpose({ write, root });
</script>

<template>
  <div ref="root" class="g-size sz" :class="{ 'sz-focus': editing, 'sz-bad': bad }" @mousedown.stop>
    <GIcon v-if="warn && !editing" name="warning" :size="12" class="sz-warn-ic" :label="t('overlay.size.tooBig')" />
    <span v-if="!bare" class="sz-num" :class="{ 'sz-warn': (warn && !editing) || bad }">
      <template v-if="editing">
        <input
          ref="wIn"
          class="sz-in"
          inputmode="numeric"
          :value="typed.w"
          :style="{ width: `${Math.max(1, typed.w.length)}ch` }"
          :aria-label="t('overlay.size.width')"
          @input="onInput('w', $event)"
          @keydown="onFieldKey('w', $event)"
          @blur="onBlur"
        /><span class="g-size-x">×</span
        ><input
          ref="hIn"
          class="sz-in"
          inputmode="numeric"
          :value="typed.h"
          :style="{ width: `${Math.max(1, typed.h.length)}ch` }"
          :aria-label="t('overlay.size.height')"
          @input="onInput('h', $event)"
          @keydown="onFieldKey('h', $event)"
          @blur="onBlur"
        />
      </template>
      <template v-else-if="editable">
        <button ref="wEl" type="button" class="sz-n" :aria-label="t('overlay.size.width')" @mousedown.prevent @click="edit('w')" /><span
          class="g-size-x"
          >×</span
        ><button ref="hEl" type="button" class="sz-n" :aria-label="t('overlay.size.height')" @mousedown.prevent @click="edit('h')" />
      </template>
      <template v-else><span ref="wEl" /><span class="g-size-x">×</span><span ref="hEl" /></template>
    </span>
    <span v-if="beautify && !bare" class="sz-beautify" :data-off="beautify === 'off' || undefined" aria-hidden="true">
      <GIcon name="beautify" :size="14" />
    </span>
    <button
      type="button"
      class="sz-menu"
      aria-haspopup="menu"
      :aria-expanded="open"
      :aria-label="t('overlay.size.menu')"
      @mousedown.prevent
      @click="emit('menu')"
    >
      <span v-if="lock && !bare" class="sz-sep" />
      <span v-if="lock" class="sz-lock">
        <GIcon name="lock" :size="12" :label="t('overlay.size.locked')" />
        <span :class="{ 'sz-name': lock.name }">{{ lock.text }}</span>
      </span>
      <span v-else-if="bare" class="sz-name">{{ t("overlay.free") }}</span>
      <GIcon name="chevron-down" :size="12" class="sz-chev" />
    </button>
  </div>
</template>

<style>
.sz-beautify { display: inline-flex; margin: 0 -3px 0 7px; color: var(--surface-iris-text); }
.sz-beautify[data-off] { color: var(--text-faint); }
/* The label stays 24 px high; the lock, the name (at most 190 px) and the chevron add at most 70 px
   besides the name. Ported from the canvas's row 10 (r10-) rules. */
.sz { gap: 0; }
.sz-num { display: inline-flex; align-items: center; }
.sz-n, .sz-menu { display: inline-flex; align-items: center; height: 24px; margin: 0; padding: 0; border: 0; background: none; color: inherit; font: inherit; cursor: default; -webkit-appearance: none; appearance: none; }
.sz-n:hover { color: var(--iris-text); }
.sz-menu { margin-right: -10px; padding-right: 10px; }
.sz-num + .sz-menu { padding-left: 2px; }
.sz-sep { flex: none; width: 1px; height: 12px; margin: 0 8px; border-radius: 1px; background: var(--divider); }
.sz-lock { display: inline-flex; align-items: center; gap: 4px; }
.sz-lock > g-icon { color: var(--text-muted); }
.sz-name { display: inline-block; max-width: 190px; overflow: hidden; text-overflow: ellipsis; font: var(--type-label); vertical-align: top; }
.sz-chev { margin: 0 -4px 0 6px; color: var(--text-muted); }
.sz-menu:hover .sz-chev, .sz-menu[aria-expanded="true"] .sz-chev { color: var(--text); }
.sz-warn { color: var(--warn); }
.sz-warn .g-size-x { color: inherit; opacity: 0.7; }
.sz-warn-ic { margin: 0 5px 0 -3px; color: var(--warn); }
.sz-focus { outline: 2px solid var(--surface-focus); outline-offset: 1px; }
.sz-focus.sz-bad { outline-color: var(--warn); }
.sz-in { box-sizing: content-box; min-width: 1ch; height: 16px; margin: 0; padding: 0; border: 0; outline: 0; background: none; color: inherit; font: inherit; font-variant-numeric: tabular-nums; caret-color: var(--iris-text); -webkit-appearance: none; appearance: none; }
.sz-bad .sz-in { caret-color: var(--warn); }
</style>
