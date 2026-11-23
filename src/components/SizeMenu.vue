<script setup lang="ts">
// The size menu that hangs from the selection's size label (DESIGN.md "Overlay"): the ratios as a
// segmented control, the ready sizes, the sizes the user saved, and "Save this size…" with its name
// field. A size larger than the screen shows in amber. It never takes the keyboard from the page:
// the page hands it its keys (`key`) while it is open, and only the name field takes the focus.
// While a window is picked the presets do not apply, and the menu says so with its items off.
import { computed, nextTick, ref, watch } from "vue";

import GIcon from "./GIcon.vue";
import { t } from "../lib/i18n";
import type { SavedSize } from "../lib/settings";
import { PRESET_SIZES, RATIOS, ratioOf, type SizeChoice } from "../lib/sizes";

const props = defineProps<{
  choice: SizeChoice;
  saved: SavedSize[];
  /** The screen in frame pixels: a larger size is marked. */
  limit: { width: number; height: number };
  /** The selection's size in frame pixels, which "Save this size…" saves; `null` without one. */
  current: { width: number; height: number } | null;
  /** While a window is picked, the window sets the size. */
  disabled?: boolean;
}>();

const emit = defineEmits<{
  /** `keep`: the keyboard stepped to a ratio, and the menu stays open for the next step. */
  choose: [choice: SizeChoice, keep: boolean];
  save: [name: string];
  remove: [size: SavedSize];
}>();

type SizeRow = { kind: "preset" | "saved"; name: string; width: number; height: number; size?: SavedSize };
type Row = SizeRow | { kind: "save" };

const rows = computed<Row[]>(() => {
  const presets: Row[] = PRESET_SIZES.map((p) => ({ kind: "preset", name: t(p.key), width: p.width, height: p.height }));
  if (props.disabled) return presets;
  const saved: Row[] = props.saved.map((s) => ({ kind: "saved", name: s.name, width: s.width, height: s.height, size: s }));
  return [...presets, ...saved, { kind: "save" }];
});
const presetRows = computed(() => rows.value.filter((r): r is SizeRow => r.kind === "preset"));
const savedRows = computed(() => rows.value.filter((r): r is SizeRow => r.kind === "saved"));

/** The row the keyboard or the pointer is on; -1 for none. */
const hot = ref(-1);
const saving = ref(false);
const nameField = ref<HTMLInputElement | null>(null);
const name = ref("");

const seg = computed(() => [{ kind: "free" as const }, ...RATIOS.map((ratio) => ({ kind: "ratio" as const, ratio }))]);
const segOn = computed(() => {
  const c = props.choice;
  if (c.kind === "free") return 0;
  const ratio = c.kind === "ratio" ? c.ratio : ratioOf(c.width, c.height);
  return ratio ? RATIOS.indexOf(ratio) + 1 : -1;
});

const fits = (r: { width: number; height: number }) => r.width <= props.limit.width && r.height <= props.limit.height;
const sizeText = (r: { width: number; height: number }) => `${r.width} × ${r.height}`;
const checked = (r: Row) =>
  r.kind !== "save" && props.choice.kind === "size" && props.choice.width === r.width && props.choice.height === r.height && props.choice.name === r.name;
const indexOf = (r: Row) => rows.value.indexOf(r);

watch(
  () => rows.value.length,
  (n) => {
    if (hot.value >= n) hot.value = n - 1;
  },
);

function pick(r: Row): void {
  if (props.disabled) return;
  if (r.kind === "save") return void startSaving();
  emit("choose", { kind: "size", width: r.width, height: r.height, name: r.name }, false);
}

function pickRatio(i: number, keep: boolean): void {
  if (props.disabled) return;
  const s = seg.value[i];
  emit("choose", s.kind === "free" ? { kind: "free" } : s, keep);
}

async function startSaving(): Promise<void> {
  if (!props.current) return;
  name.value = "";
  saving.value = true;
  await nextTick();
  nameField.value?.focus();
}

function keep(): void {
  const c = props.current;
  if (!c) return;
  emit("save", name.value.trim() || sizeText(c));
  saving.value = false;
}

function onNameKey(e: KeyboardEvent): void {
  if (e.key === "Enter") {
    e.preventDefault();
    keep();
  } else if (e.key === "Escape") {
    e.preventDefault();
    saving.value = false;
  }
}

/**
 * A key of the page while the menu is open. ↑ and ↓ go through the sizes, ← and → step through
 * the ratios, Return takes the row, ⌫ deletes a saved size; Esc is the page's, which closes the
 * menu. Returns whether the key was the menu's.
 */
function key(e: KeyboardEvent): boolean {
  const n = rows.value.length;
  switch (e.key) {
    case "ArrowDown":
    case "ArrowUp":
      if (props.disabled || !n) return true;
      hot.value = e.key === "ArrowDown" ? (hot.value + 1) % n : hot.value <= 0 ? n - 1 : hot.value - 1;
      return true;
    case "ArrowLeft":
    case "ArrowRight": {
      if (props.disabled) return true;
      const count = seg.value.length;
      const at = Math.max(0, segOn.value);
      pickRatio(e.key === "ArrowRight" ? (at + 1) % count : (at + count - 1) % count, true);
      return true;
    }
    case "Enter": {
      const r = rows.value[hot.value];
      if (r) pick(r);
      return true;
    }
    case "Backspace":
    case "Delete": {
      const r = rows.value[hot.value];
      if (r?.kind === "saved" && r.size) emit("remove", r.size);
      return true;
    }
    default:
      return false;
  }
}

/** Whether the name field is open, so the page leaves Return and Esc to it. */
const typing = computed(() => saving.value);

defineExpose({ key, typing });
</script>

<template>
  <div class="g-pop sz-pop" role="menu" :aria-label="t('overlay.size.menu')" style="--pop-w: 300px" @mousedown.stop @dblclick.stop>
    <div v-if="disabled" class="sz-help">
      <GIcon name="info" :size="16" />
      <span>{{ t("overlay.size.window") }}</span>
    </div>
    <div class="g-pop-row">
      <div class="g-seg" role="radiogroup" :aria-label="t('overlay.ratio')">
        <button
          v-for="(s, i) in seg"
          :key="s.kind === 'free' ? 'free' : s.ratio"
          type="button"
          role="radio"
          :aria-checked="segOn === i"
          :aria-disabled="disabled ? 'true' : undefined"
          @mousedown.prevent
          @click="pickRatio(i, false)"
        >
          {{ s.kind === "free" ? t("overlay.free") : s.ratio }}
        </button>
      </div>
    </div>
    <hr class="g-pop-sep" />
    <button
      v-for="r in presetRows"
      :key="`p${indexOf(r)}`"
      type="button"
      class="g-pop-item"
      role="menuitemradio"
      :aria-checked="checked(r)"
      :aria-disabled="disabled ? 'true' : undefined"
      :data-state="hot === indexOf(r) && !disabled ? 'hover' : undefined"
      @mousedown.prevent
      @mouseenter="hot = indexOf(r)"
      @click="pick(r)"
    >
      <GIcon name="check" :size="16" class="g-pop-check" />
      <span class="g-pop-name">{{ r.name }}</span>
      <span class="g-pop-sub" :class="{ 'sz-warn': !fits(r) && !disabled }">
        <GIcon v-if="!fits(r) && !disabled" name="warning" :size="12" :label="t('overlay.size.tooBig')" />{{ sizeText(r) }}
      </span>
    </button>
    <template v-if="!disabled">
      <hr v-if="savedRows.length" class="g-pop-sep" />
      <button
        v-for="r in savedRows"
        :key="`s${indexOf(r)}`"
        type="button"
        class="g-pop-item"
        role="menuitemradio"
        aria-keyshortcuts="Backspace"
        :aria-checked="checked(r)"
        :data-state="hot === indexOf(r) ? 'hover' : undefined"
        @mousedown.prevent
        @mouseenter="hot = indexOf(r)"
        @click="pick(r)"
      >
        <GIcon name="check" :size="16" class="g-pop-check" />
        <span class="g-pop-name">{{ r.name }}</span>
        <span class="g-pop-sub" :class="{ 'sz-warn': !fits(r) }">
          <GIcon v-if="!fits(r)" name="warning" :size="12" :label="t('overlay.size.tooBig')" />{{ sizeText(r) }}
        </span>
      </button>
      <hr class="g-pop-sep" />
      <div v-if="saving" class="sz-saverow">
        <input
          ref="nameField"
          v-model="name"
          class="sz-field"
          maxlength="60"
          :placeholder="t('overlay.size.name')"
          :aria-label="t('overlay.size.name')"
          @keydown="onNameKey"
        />
        <button type="button" class="g-button" data-variant="primary" @mousedown.prevent @click="keep">{{ t("overlay.size.keep") }}</button>
      </div>
      <button
        v-else
        type="button"
        class="g-pop-item"
        role="menuitem"
        :aria-disabled="current ? undefined : 'true'"
        :data-state="hot === rows.length - 1 && current ? 'hover' : undefined"
        @mousedown.prevent
        @mouseenter="hot = rows.length - 1"
        @click="startSaving"
      >
        <GIcon name="plus" :size="16" />
        <span class="g-pop-name">{{ t("overlay.size.save") }}</span>
      </button>
    </template>
  </div>
</template>

<style>
.sz-pop { cursor: default; }
.sz-pop .g-pop-row { padding: 0; min-height: 36px; }
.sz-pop .g-seg { flex: 1; }
.sz-pop .g-seg > button { flex: 1; padding: 0 6px; }
.sz-pop .g-pop-sub { font-variant-numeric: tabular-nums; }
.sz-pop .g-pop-sub > g-icon { margin-right: 4px; vertical-align: -2px; }
.sz-pop .g-pop-sub.sz-warn { color: var(--warn); }
.sz-help { display: flex; align-items: flex-start; gap: 8px; padding: 8px 8px 10px; font: var(--type-body); color: var(--text-muted); }
.sz-help > g-icon { flex: none; color: var(--warn); }
.sz-saverow { display: flex; align-items: center; gap: 6px; padding: 4px; }
.sz-field {
  flex: 1; min-width: 0; height: 28px; margin: 0; padding: 0 8px; border: 0; border-radius: var(--radius-thumb); outline: 0;
  -webkit-appearance: none; appearance: none;
  background: var(--control-hover); box-shadow: inset 0 0 0 var(--hairline, 1px) var(--edge-inner);
  font: var(--type-label); color: var(--text);
}
.sz-field:focus { outline: 2px solid var(--surface-focus); outline-offset: 1px; }
.sz-field::placeholder { color: var(--text-faint); }
</style>
