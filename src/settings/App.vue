<script setup lang="ts">
// The settings window: a sidebar of sections (Agents on macOS alone) beside grouped rows, in an
// ordinary titled window (the system draws the title bar). Every control saves at once
// (store.ts). A host of Upload opens its own page with a back button in the toolbar.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import { on } from "../lib/api/core";
import { SETTINGS_SECTION, window_take_settings_section, type SettingsSection } from "../lib/api/windows";
import { t, type Key } from "../lib/i18n";
import { can } from "../lib/platform";
import AboutSection from "./sections/AboutSection.vue";
import AgentsSection from "./sections/AgentsSection.vue";
import BeautifySection from "./sections/BeautifySection.vue";
import GeneralSection from "./sections/GeneralSection.vue";
import OcrSection from "./sections/OcrSection.vue";
import OutputSection from "./sections/OutputSection.vue";
import RecordingSection from "./sections/RecordingSection.vue";
import ShortcutsSection from "./sections/ShortcutsSection.vue";
import { banner, loadError, reload, settings, start } from "./store";
import ProviderPage from "./upload/ProviderPage.vue";
import UploadSection from "./upload/UploadSection.vue";
import type { Provider } from "./upload";

type SectionId = SettingsSection;

const SECTIONS: { id: SectionId; icon: string; title: Key }[] = [
  { id: "general", icon: "settings", title: "settings.nav.general" },
  { id: "shortcuts", icon: "keyboard", title: "settings.nav.shortcuts" },
  { id: "output", icon: "image", title: "settings.nav.output" },
  { id: "beautify", icon: "beautify", title: "settings.nav.beautify" },
  { id: "recording", icon: "record", title: "settings.nav.recording" },
  { id: "ocr", icon: "ocr", title: "settings.nav.ocr" },
  { id: "upload", icon: "upload", title: "settings.nav.upload" },
  // The local MCP server exists on macOS alone.
  ...(can("agents") ? [{ id: "agents" as const, icon: "shield", title: "settings.nav.agents" as const }] : []),
  { id: "about", icon: "info", title: "settings.nav.about" },
];

const fromHash = (): SectionId | null => {
  const id = location.hash.replace(/^#/, "");
  return SECTIONS.some((s) => s.id === id) ? (id as SectionId) : null;
};

const section = ref<SectionId>(fromHash() ?? "general");
const provider = ref<Provider | null>(null);
const content = ref<HTMLElement | null>(null);

const title = computed(() => {
  if (section.value === "upload" && provider.value) return provider.value.name;
  const found = SECTIONS.find((s) => s.id === section.value);
  return found ? t(found.title) : "";
});

function go(id: SectionId): void {
  provider.value = null;
  section.value = id;
}

function openProvider(next: Provider): void {
  provider.value = next;
}

function dismiss(): void {
  banner.value = null;
}

function back(): void {
  provider.value = null;
}

watch([section, provider], async () => {
  await nextTick();
  if (content.value) content.value.scrollTop = 0;
});

function onHash(): void {
  const id = fromHash();
  if (id) go(id);
}

/** Up and down move between the sections while the sidebar has the focus. */
function navKey(event: KeyboardEvent): void {
  if (event.key !== "ArrowDown" && event.key !== "ArrowUp") return;
  event.preventDefault();
  const index = SECTIONS.findIndex((s) => s.id === section.value);
  const next = SECTIONS[(index + (event.key === "ArrowDown" ? 1 : -1) + SECTIONS.length) % SECTIONS.length];
  if (!next) return;
  go(next.id);
  void nextTick(() => document.querySelector<HTMLElement>(`[data-nav="${next.id}"]`)?.focus());
}

/** Another window asked for a section (`window_open_settings`). */
async function takeSection(): Promise<void> {
  try {
    const asked = await window_take_settings_section();
    if (asked) go(asked);
  } catch {
    // The section shown last stays.
  }
}

let stopSection: (() => void) | null = null;
onMounted(async () => {
  void start();
  window.addEventListener("hashchange", onHash);
  stopSection = await on<null>(SETTINGS_SECTION, () => void takeSection());
  await takeSection();
});
onBeforeUnmount(() => {
  window.removeEventListener("hashchange", onHash);
  stopSection?.();
});
</script>

<template>
  <div class="g-window p-settings">
    <aside class="g-sidebar">
      <nav class="g-nav" :aria-label="t('settings.title')" @keydown="navKey">
        <a
          v-for="item in SECTIONS"
          :key="item.id"
          class="g-nav-item"
          :href="`#${item.id}`"
          :data-nav="item.id"
          :aria-current="section === item.id ? 'page' : undefined"
          @click.prevent="go(item.id)"
          ><GIcon :name="item.icon" :size="16" /><span class="p-nav-label">{{ t(item.title) }}</span></a
        >
      </nav>
    </aside>
    <div class="g-window-main">
      <header class="g-toolbar">
        <button v-if="section === 'upload' && provider" type="button" class="g-btn" data-size="small" :aria-label="t('settings.upload.back')" @click="back">
          <GIcon name="chevron-left" :size="16" />
        </button>
        <h1 class="g-window-title">{{ title }}</h1>
        <span class="g-spacer"></span>
      </header>
      <main ref="content" class="g-content">
        <div v-if="banner" class="p-banner" :data-tone="banner.tone" role="alert">
          <GIcon :name="banner.tone === 'danger' ? 'bang' : 'warning'" :size="16" />
          <span class="p-banner-text">{{ banner.text }}</span>
          <button v-if="banner.action" type="button" class="g-button" @click="banner.action.run()">{{ banner.action.label }}</button>
          <button type="button" class="g-btn" data-size="small" :aria-label="t('settings.common.dismiss')" @click="dismiss">
            <GIcon name="close" :size="14" />
          </button>
        </div>

        <div v-if="loadError && !settings" class="g-empty">
          <div class="g-empty-icon"><GIcon name="warning" :size="32" /></div>
          <div class="g-empty-title">{{ t("settings.loadFailedTitle") }}</div>
          <p class="g-empty-text">{{ loadError }}</p>
          <div class="g-empty-actions"><button type="button" class="g-button" @click="reload">{{ t("settings.common.tryAgain") }}</button></div>
        </div>

        <template v-else-if="settings">
          <GeneralSection v-if="section === 'general'" :settings="settings" />
          <ShortcutsSection v-else-if="section === 'shortcuts'" :settings="settings" />
          <OutputSection v-else-if="section === 'output'" :settings="settings" />
          <BeautifySection v-else-if="section === 'beautify'" :settings="settings" />
          <RecordingSection v-else-if="section === 'recording'" :settings="settings" />
          <OcrSection v-else-if="section === 'ocr'" :settings="settings" />
          <template v-else-if="section === 'upload'">
            <ProviderPage v-if="provider" :key="provider.id" :settings="settings" :provider="provider" />
            <UploadSection v-else :settings="settings" @open="openProvider" />
          </template>
          <AgentsSection v-else-if="section === 'agents'" :settings="settings" />
          <AboutSection v-else-if="section === 'about'" />
        </template>
      </main>
    </div>
  </div>
</template>

<style>
/* The real window: the system draws the frame, the title bar and the shadow, so the kit's window
   fills the page without its own corners, shadow or rim. */
.p-settings.g-window { height: 100%; border-radius: 0; box-shadow: none; }
.p-settings.g-window::after { display: none; }
/* The sidebar grows past 188 pt to fit the longest section name ("Reconocimiento de texto",
   "Распознавание текста"), up to 240 pt; a name longer than that ends in an ellipsis. */
.p-settings > .g-sidebar { --sidebar-w: 188px; width: auto; min-width: var(--sidebar-w); max-width: 240px; padding-top: 12px; }
.p-settings .g-nav { overflow-x: hidden; }
.p-settings .g-nav-item { text-decoration: none; cursor: default; }
.p-nav-label { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
.p-settings .g-nav-item:focus { outline: 0; }
.p-settings .g-nav-item:focus-visible { outline: 2px solid var(--win-focus); outline-offset: -2px; }
.p-settings .g-toolbar > .g-btn { margin-left: -6px; }
/* A menu is as wide as its longest option, and a device name can be any length (a microphone
   called "Elgato Wave:3 USB Condenser Microphone"); past 260 pt the shown name is cut short. */
.p-settings .g-form-control > .g-select { max-width: 260px; }
.p-settings .g-form-control > .g-select > select { text-overflow: ellipsis; }

/* A save that failed, with the way out. */
.p-banner {
  display: flex; align-items: center; gap: 10px; margin: 0 0 16px; padding: 8px 8px 8px 12px;
  border-radius: var(--radius-group); background: var(--win-danger-soft); color: var(--win-text); font: var(--type-text);
}
.p-banner[data-tone="warn"] { background: var(--win-warn-soft); }
.p-banner > g-icon { flex: none; color: var(--win-danger); }
.p-banner[data-tone="warn"] > g-icon { color: var(--win-warn); }
.p-banner-text { flex: 1; min-width: 0; }

/* ------------------------------------------------------------------------------------------
   Page-local parts ported from design/round-2/settings (every class starts with p-).
   ------------------------------------------------------------------------------------------ */
.p-actions { display: flex; flex-wrap: wrap; gap: 6px; margin-top: 8px; }
.p-flush > .g-button[data-variant="plain"]:first-child { margin-left: -12px; }
.p-flush > .g-button[data-variant="plain"]:first-child:has(> g-icon:first-child) { margin-left: -9px; }
.p-gap { margin-top: 20px; }

.p-foot { display: flex; align-items: center; justify-content: flex-end; gap: 12px; margin-top: 12px; padding: 0 4px; }
.p-foot > .g-help { flex: 1; margin: 0; }
.p-note { display: flex; gap: 10px; align-items: flex-start; margin-top: 20px; padding: 0 4px; font: var(--type-body); color: var(--win-text-2); }
.p-note > g-icon { color: var(--win-iris-text); margin-top: -1px; }

.p-path {
  display: inline-flex; align-items: center; gap: 6px; min-width: 0; max-width: 220px; height: var(--control-h);
  padding: 0 10px 0 8px; border-radius: var(--radius-field); background: var(--win-fill);
  color: var(--win-text); font: var(--type-text); white-space: nowrap;
}
.p-path > g-icon { color: var(--win-iris-text); }
.p-path > span { overflow: hidden; text-overflow: ellipsis; }
.p-path[data-tone="warn"] > g-icon { color: var(--win-warn); }

.p-template { display: grid; gap: 8px; min-width: 0; }
.p-tokens { display: flex; flex-wrap: wrap; align-items: center; gap: 4px; }
.p-tokens-label { margin-right: 4px; font: var(--type-body); color: var(--win-text-2); }
.p-token {
  height: var(--control-h); margin: 0; padding: 0 8px; border: 0; border-radius: var(--radius-field);
  background: var(--win-fill); color: var(--win-iris-text); font: 500 12px/18px var(--font-mono);
  cursor: default; -webkit-appearance: none; appearance: none;
  transition: background-color var(--dur-fast) var(--ease-out);
}
.p-token:hover { background: var(--win-press); }
.p-token:focus { outline: 0; }
.p-token:focus-visible { outline: 2px solid var(--win-focus); outline-offset: 1px; }
.p-token:disabled { color: var(--win-text-disabled); }
.p-preview { display: flex; align-items: baseline; gap: 8px; min-width: 0; font: var(--type-body); color: var(--win-text-2); }
.p-preview > output { min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font: 400 12px/16px var(--font-mono); color: var(--win-text); }
.p-val { min-width: 40px; text-align: right; font: var(--type-num); color: var(--win-text-2); }
.p-slider { width: 200px; }

.p-keyref { display: grid; grid-template-columns: 1fr 1fr; gap: 0 28px; padding: 8px 14px 12px; }
.p-keyref h3 { margin: 8px 0 2px; font: var(--type-caption-strong); color: var(--win-text-3); }
.p-keyrow { display: flex; align-items: center; justify-content: space-between; gap: 8px; height: 26px; font: var(--type-text); color: var(--win-text); }

.p-list { padding: 4px; }
.p-list > .g-row { width: 100%; }
.p-chev { color: var(--win-text-3); }
.p-mono {
  display: inline-grid; place-items: center; flex: none; width: 28px; height: 28px; border-radius: var(--radius-field);
  background: var(--win-selection-soft); color: var(--win-iris-text); font: 600 11px/1 var(--font); letter-spacing: 0.02em;
}
.p-mono[data-size="large"] { width: 40px; height: 40px; border-radius: var(--radius-group); font: 600 14px/1 var(--font); }
.p-provider { display: flex; align-items: center; gap: 12px; padding: 0 4px 20px; }
.p-provider-text { flex: 1; min-width: 0; }
.p-provider-name { font: var(--type-heading); color: var(--win-text); }
.p-provider-host { margin-top: 2px; font: 400 12px/16px var(--font-mono); color: var(--win-text-2); }
.p-provider-host[data-kind="plain"] { font: var(--type-body); }
.p-terms { margin: 0; padding: 2px 12px; list-style: none; }
.p-terms > li { display: flex; gap: 10px; align-items: flex-start; padding: 9px 0; font: var(--type-text); color: var(--win-text); }
.p-terms > li + li { box-shadow: inset 0 var(--hairline) 0 var(--win-line); }
.p-terms > li > g-icon { margin-top: 1px; color: var(--win-iris-text); }
.p-terms > li.p-terms-link { padding: 6px 0; }
.p-secret { display: flex; align-items: center; gap: 4px; }
.p-secret > .g-field { flex: 1; min-width: 0; }
.p-col { display: grid; gap: 10px; min-width: 0; }
.p-test { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 12px; }
.p-test > .g-help { flex: 1 1 220px; min-width: 0; margin: 0; }
.p-test > .g-help[data-tone="danger"] { flex-basis: 100%; }
.p-code { padding: 1px 6px; border-radius: var(--radius-key); background: var(--win-fill); font: 500 11.5px/16px var(--font-mono); color: var(--win-text); white-space: nowrap; }
.p-kv { display: grid; grid-template-columns: minmax(0, 1fr) minmax(0, 1.3fr) 28px; gap: 6px; align-items: center; }
.p-kv-head { font: var(--type-caption-strong); color: var(--win-text-3); }

.p-engine { align-items: flex-start; }
.p-engine > .g-radio { margin-top: 1px; }
.p-engine[aria-disabled="true"] > .g-form-label > .g-label { color: var(--win-text-disabled); }
.p-checks { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 0 16px; }
.p-swatches { display: inline-flex; gap: 2px; }

.p-about { display: flex; flex-direction: column; align-items: center; gap: 4px; padding: 4px 0 20px; text-align: center; }
.p-about-icon { display: grid; place-items: center; width: 72px; height: 72px; margin-bottom: 8px; border-radius: var(--radius-card); background: var(--iris); color: var(--text-on-iris); box-shadow: var(--shadow-active); }
.p-about-name { margin: 0; font: var(--type-display); color: var(--win-text); }
.p-about-meta { margin: 0; font: var(--type-body); color: var(--win-text-2); }
.p-table { padding: 4px 6px 6px; }
/* A project's use and licence wrap to a second line instead of pushing the table past the window;
   the 7 pt above and below give a one-line row the kit's 32 pt. */
.p-table .g-table td { height: auto; padding-top: 7px; padding-bottom: 7px; white-space: normal; overflow-wrap: break-word; }
.p-table .g-table td:first-child { white-space: nowrap; }
</style>
