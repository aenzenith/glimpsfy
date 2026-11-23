<script setup lang="ts">
// Settings › Beautify (B11): whether a preset goes on every capture by itself and which one, and
// the presets with their thumbnails. A preset's values change in a capture's beautify panel
// (Update preset); here a preset is made automatic, renamed, duplicated or deleted.
import { computed, nextTick, ref } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import SelectControl from "../../components/SelectControl.vue";
import SwitchControl from "../../components/SwitchControl.vue";
import Menu, { type MenuItem } from "../../history/Menu.vue";
import type { BeautifyStyle } from "../../lib/annotate";
import { t, type Key } from "../../lib/i18n";
import type { Settings } from "../../lib/settings";
import {
  jpegLoses,
  presetName,
  readPresets,
  suggestedName,
  withDuplicatedPreset,
  withRenamedPreset,
  withoutPreset,
  type Preset,
} from "../../annotate/beautify";
import { update } from "../store";
import { uid } from "../text";
import BeautifyThumb from "./BeautifyThumb.vue";

const props = defineProps<{ settings: Settings }>();

const ids = { auto: uid("bauto"), preset: uid("bpreset") };
const presets = computed(() => readPresets(props.settings.beautify));
const options = computed(() => presets.value.map((p) => ({ value: p.id, label: presetName(p) })));
const jpeg = computed(() => props.settings.output.format === "jpeg");
const jpegWarning = computed(() => jpeg.value && presets.value.some((p) => jpegLoses(p.style)));
/** The automatic preset, which also is the one F turns on. */
const chosen = computed(() => (presets.value.some((p) => p.id === props.settings.beautify.preset) ? props.settings.beautify.preset : (presets.value[0]?.id ?? "")));

const KINDS: Record<BeautifyStyle["background"]["kind"], Key> = {
  transparent: "settings.beautify.kind.transparent",
  solid: "settings.beautify.kind.solid",
  gradient: "settings.beautify.kind.gradient",
  image: "settings.beautify.kind.image",
  match: "settings.beautify.kind.match",
};
const SHADOWS: Record<BeautifyStyle["shadow"], Key> = {
  none: "settings.beautify.shadow.none",
  soft: "settings.beautify.shadow.soft",
  medium: "settings.beautify.shadow.medium",
  deep: "settings.beautify.shadow.deep",
};
const summary = (p: Preset) =>
  t("settings.beautify.summary", {
    padding: Math.round(p.style.padding),
    background: t(KINDS[p.style.background.kind]),
    corner: Math.round(p.style.corner),
    shadow: t(SHADOWS[p.style.shadow]),
  });

// --------------------------------------------------------------------------- changing

function setAuto(on: boolean): void {
  void update((s) => (s.beautify.auto = on));
}

function choose(id: string): void {
  void update((s) => (s.beautify.preset = id));
}

/** Runs a change of the presets' list (`annotate/beautify.ts`), which works on a copy. */
function change(next: (s: Settings) => Settings): void {
  void update((s) => {
    s.beautify = next(s).beautify;
  });
}

const renaming = ref<string | null>(null);
const draft = ref("");
const field = ref<HTMLInputElement[] | HTMLInputElement | null>(null);

async function rename(p: Preset): Promise<void> {
  renaming.value = p.id;
  draft.value = presetName(p);
  await nextTick();
  const el = Array.isArray(field.value) ? field.value[0] : field.value;
  el?.focus();
  el?.select();
}

function commit(): void {
  const id = renaming.value;
  const name = draft.value.trim();
  renaming.value = null;
  if (!id || !name) return;
  const p = presets.value.find((x) => x.id === id);
  if (p && presetName(p) !== name) change((s) => withRenamedPreset(s, id, name));
}

function onField(e: KeyboardEvent): void {
  if (e.key === "Enter") {
    e.preventDefault();
    commit();
  } else if (e.key === "Escape") {
    e.preventDefault();
    e.stopPropagation();
    renaming.value = null;
  }
}

/** New preset: a copy of the automatic one under a free name, ready to be renamed. */
async function create(): Promise<void> {
  const from = presets.value.find((p) => p.id === chosen.value) ?? presets.value[0];
  if (!from) return;
  await duplicate(from, suggestedName(presets.value));
}

async function duplicate(p: Preset, name = t("settings.beautify.copyName", { name: presetName(p) })): Promise<void> {
  let id = "";
  change((s) => {
    const made = withDuplicatedPreset(s, p.id, name);
    id = made.id;
    return made.settings;
  });
  await nextTick();
  const copy = presets.value.find((x) => x.id === id);
  if (copy) await rename(copy);
}

// ------------------------------------------------------------------------------ the menu

const menu = ref<{ preset: Preset; at: { x: number; y: number } } | null>(null);

function openMenu(p: Preset, e: MouseEvent): void {
  const box = (e.currentTarget as HTMLElement).getBoundingClientRect();
  menu.value = { preset: p, at: { x: box.left, y: box.bottom + 4 } };
}

const items = computed<MenuItem[]>(() => {
  const p = menu.value?.preset;
  if (!p) return [];
  const auto = props.settings.beautify.auto && chosen.value === p.id;
  return [
    {
      label: t("settings.beautify.makeAuto"),
      icon: "check",
      disabled: auto,
      run: () =>
        void update((s) => {
          s.beautify.auto = true;
          s.beautify.preset = p.id;
        }),
    },
    { label: t("settings.beautify.rename"), icon: "pencil", run: () => void rename(p) },
    { label: t("settings.beautify.duplicate"), icon: "copy", run: () => void duplicate(p) },
    "separator",
    {
      label: t("settings.beautify.delete"),
      icon: "trash",
      destructive: true,
      // The last preset stays, so F always has one to turn on.
      disabled: presets.value.length <= 1,
      run: () => change((s) => withoutPreset(s, p.id)),
    },
  ];
});

const toOutput = () => (location.hash = "#output");
</script>

<template>
  <div>
    <SectionHead :title="t('settings.beautify.automatic')" />
    <div class="g-group">
      <FormRow :label="t('settings.beautify.auto')" :for="ids.auto" :help="t('settings.beautify.autoHelp')">
        <SwitchControl :id="ids.auto" :model-value="settings.beautify.auto" @update:model-value="setAuto" />
      </FormRow>
      <FormRow :label="t('settings.beautify.preset')" :for="ids.preset">
        <SelectControl :id="ids.preset" :model-value="chosen" :options="options" :min-width="210" @update:model-value="choose" />
      </FormRow>
    </div>

    <SectionHead :title="t('settings.beautify.presets')">
      <button type="button" class="g-button" @click="create"><GIcon name="plus" :size="16" />{{ t("settings.beautify.new") }}</button>
    </SectionHead>
    <div v-if="jpegWarning" class="bs-warn" role="note">
      <GIcon name="warning" :size="16" />
      <span
        >{{ t("settings.beautify.jpeg") }}<a class="g-link" href="#output" @click.prevent="toOutput">{{ t("settings.beautify.outputLink") }}</a></span
      >
    </div>
    <div class="g-group" role="list">
      <div v-for="p in presets" :key="p.id" class="bs-row" role="listitem">
        <BeautifyThumb :current="p.style" :jpeg="jpeg" />
        <span class="bs-text">
          <input
            v-if="renaming === p.id"
            ref="field"
            v-model="draft"
            class="g-field bs-name"
            type="text"
            :aria-label="t('beautify.name')"
            @keydown="onField"
            @blur="commit"
          />
          <b v-else>{{ presetName(p) }}</b>
          <span>{{ summary(p) }}</span>
        </span>
        <span v-if="settings.beautify.auto && chosen === p.id" class="bs-auto"><GIcon name="check" :size="16" />{{ t("settings.beautify.isAuto") }}</span>
        <button
          type="button"
          class="g-btn"
          data-size="small"
          :aria-label="t('settings.beautify.actions')"
          :aria-expanded="menu?.preset.id === p.id"
          @click="openMenu(p, $event)"
        >
          <GIcon name="more" :size="16" />
        </button>
      </div>
    </div>
    <p class="g-section-desc bs-values">{{ t("settings.beautify.valuesHelp") }}</p>
    <Menu v-if="menu" :items="items" :at="menu.at" :label="t('settings.beautify.actions')" @close="menu = null" />
  </div>
</template>

<style scoped>
.bs-row {
  position: relative;
  display: flex;
  align-items: center;
  gap: 12px;
  min-height: 56px;
  padding: 8px 8px 8px 12px;
}
.bs-row + .bs-row::before {
  content: "";
  position: absolute;
  left: 88px;
  right: 12px;
  top: 0;
  height: var(--hairline);
  background: var(--win-line);
}
.bs-text {
  flex: 1;
  min-width: 0;
  display: flex;
  flex-direction: column;
}
.bs-text > b {
  font: var(--type-text);
  color: var(--win-text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.bs-text > span {
  font: var(--type-body);
  color: var(--win-text-2);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.bs-name {
  max-width: 260px;
}
.bs-auto {
  flex: none;
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font: var(--type-body);
  color: var(--win-iris-text);
  white-space: nowrap;
}
.bs-warn {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  margin: 0 0 10px;
  padding: 9px 12px;
  border-radius: var(--radius-group);
  background: var(--win-warn-soft);
  font: var(--type-body);
  color: var(--win-text);
}
.bs-warn > :first-child {
  flex: none;
  color: var(--win-warn);
}
.bs-warn .g-link {
  margin-left: 4px;
}
.bs-values {
  margin-top: 8px;
}
</style>
