<script setup lang="ts">
// Settings › Output: where captures go and how they are named and encoded, the apps Send goes to
// (macOS), window captures (where single windows can be captured), and the history's retention,
// size and clearing.
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from "vue";

import ConfirmDialog from "../../components/ConfirmDialog.vue";
import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import SegmentedControl from "../../components/SegmentedControl.vue";
import SelectControl from "../../components/SelectControl.vue";
import SwitchControl from "../../components/SwitchControl.vue";
import { history_clear, history_usage, onHistoryChanged, type HistoryUsage } from "../../lib/api/history";
import { settings_preview_name, type NamePreview, type TemplateProblem } from "../../lib/api/settings";
import { t, tp } from "../../lib/i18n";
import { can } from "../../lib/platform";
import type { HistoryRetention, ImageFormat, RetinaScale, Settings, WindowBackground } from "../../lib/settings";
import { update } from "../store";
import { describeError, formatBytes, formatCount, isNotImplemented, uid } from "../text";
import FolderRow from "./FolderRow.vue";
import SendTargets from "./SendTargets.vue";

const props = defineProps<{ settings: Settings }>();
const o = computed(() => props.settings.output);

const ids = { template: uid("tpl"), templateError: uid("tpl-err"), format: uid("fmt"), quality: uid("q"), retina: uid("ret"), copy: uid("copy"), shadow: uid("shadow"), corners: uid("corners"), background: uid("bg"), keep: uid("keep"), trash: uid("trash") };

// ------------------------------------------------------------------ file name template
// `{title}` is the captured window's title; an area or display capture leaves it empty. `{text}`
// is the first line of the text read from a saved image (a Save waits up to 2 s for it), and the
// core also takes `{window}` for `{title}` without a chip.
const TOKENS = ["{date}", "{time}", "{app}", "{title}", "{text}", "{counter}", "{width}×{height}"];
const field = ref<HTMLInputElement | null>(null);
const draft = ref(o.value.fileNameTemplate);
const nameHelp = computed(() => {
  const help = t("settings.output.nameHelp");
  return draft.value.includes("{text}") ? `${help} ${t("settings.output.textHelp", { token: "{text}" })}` : help;
});
const focused = ref(false);
const touched = ref(false);
const preview = ref<NamePreview | null>(null);
let timer: ReturnType<typeof setTimeout> | undefined;
let seq = 0;

watch(
  () => o.value.fileNameTemplate,
  (value) => {
    if (!focused.value && timer === undefined) draft.value = value;
  },
);

async function refresh(template: string, save: boolean): Promise<void> {
  const mine = ++seq;
  let result: NamePreview | null = null;
  try {
    result = await settings_preview_name(template, o.value.format);
  } catch {
    result = null;
  }
  if (mine !== seq) return;
  preview.value = result;
  if (save && !result?.problem && template !== o.value.fileNameTemplate) void update((s) => (s.output.fileNameTemplate = template));
}

watch(() => o.value.format, () => void refresh(draft.value, false), { immediate: true });

function schedule(): void {
  if (timer !== undefined) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = undefined;
    void refresh(draft.value, true);
  }, 400);
}

function onInput(event: Event): void {
  draft.value = (event.target as HTMLInputElement).value;
  schedule();
}

function onBlur(): void {
  focused.value = false;
  touched.value = true;
  if (timer !== undefined) {
    clearTimeout(timer);
    timer = undefined;
    void refresh(draft.value, true);
  }
}

async function insert(token: string): Promise<void> {
  const input = field.value;
  const start = input?.selectionStart ?? draft.value.length;
  const end = input?.selectionEnd ?? start;
  draft.value = draft.value.slice(0, start) + token + draft.value.slice(end);
  schedule();
  await nextTick();
  if (input) {
    input.focus();
    input.setSelectionRange(start + token.length, start + token.length);
  }
}

function problemText(problem: TemplateProblem): string {
  switch (problem.kind) {
    case "empty":
      return t("settings.output.templateEmpty");
    case "character":
      return t("settings.output.templateCharacter", { character: problem.character });
    case "unknownToken": {
      const head = t("settings.output.templateToken", { token: problem.token });
      return problem.suggestion ? `${head} ${t("settings.output.templateMean", { suggestion: problem.suggestion })}` : head;
    }
  }
}

const problem = computed(() => preview.value?.problem ?? null);
const showProblem = computed(() => touched.value && !!problem.value);

onBeforeUnmount(() => {
  if (timer !== undefined) {
    clearTimeout(timer);
    timer = undefined;
    void refresh(draft.value, true);
  }
});

// ------------------------------------------------------------------------ format and quality
const formats = [
  { value: "png" as ImageFormat, label: "PNG" },
  { value: "jpeg" as ImageFormat, label: "JPEG" },
  { value: "webp" as ImageFormat, label: "WebP" },
];
const formatHelp = computed(() => t(o.value.format === "png" ? "settings.output.formatPng" : o.value.format === "jpeg" ? "settings.output.formatJpeg" : "settings.output.formatWebp"));
const optimisePngId = uid("png-small");
const quality = ref(o.value.quality);
watch(() => o.value.quality, (q) => (quality.value = q));
const retinas = computed(() => [
  { value: "keep2x" as RetinaScale, label: t("settings.output.keep2x") },
  { value: "scaleTo1x" as RetinaScale, label: t("settings.output.scaleTo1x") },
]);
const backgrounds = computed(() => [
  { value: "transparent" as WindowBackground, label: t("settings.output.transparent") },
  { value: "white" as WindowBackground, label: t("settings.output.white") },
]);
const retentions = computed(() => [
  { value: "off" as HistoryRetention, label: t("settings.output.keepOff") },
  { value: "days7" as HistoryRetention, label: t("settings.common.days", { n: 7 }) },
  { value: "days30" as HistoryRetention, label: t("settings.common.days", { n: 30 }) },
  { value: "days90" as HistoryRetention, label: t("settings.common.days", { n: 90 }) },
  { value: "forever" as HistoryRetention, label: t("settings.output.keepForever") },
]);

// ------------------------------------------------------------------------------------ history
const usage = ref<HistoryUsage | null>(null);
const usageState = ref<"loading" | "ready" | "unavailable" | "failed">("loading");
const usageError = ref<string | null>(null);
const clearNote = ref<string | null>(null);
const clearError = ref<string | null>(null);
const asking = ref(false);
const clearing = ref(false);
const clearButton = ref<HTMLButtonElement | null>(null);
let unlisten: (() => void) | null = null;
let reloadTimer: ReturnType<typeof setTimeout> | undefined;

async function loadUsage(): Promise<void> {
  try {
    usage.value = await history_usage();
    usageState.value = "ready";
  } catch (error) {
    usage.value = null;
    usageState.value = isNotImplemented(error) ? "unavailable" : "failed";
    usageError.value = describeError(error);
  }
}

onMounted(async () => {
  void loadUsage();
  try {
    unlisten = await onHistoryChanged(() => {
      if (reloadTimer !== undefined) clearTimeout(reloadTimer);
      reloadTimer = setTimeout(() => void loadUsage(), 500);
    });
  } catch {
    unlisten = null;
  }
});
onBeforeUnmount(() => {
  unlisten?.();
  if (reloadTimer !== undefined) clearTimeout(reloadTimer);
});

const spaceHelp = computed<{ text: string; tone: "ok" | "danger" | null }>(() => {
  if (clearError.value) return { text: clearError.value, tone: "danger" };
  if (clearNote.value) return { text: clearNote.value, tone: "ok" };
  if (usageState.value === "loading") return { text: t("settings.output.spaceLoading"), tone: null };
  if (usageState.value === "unavailable") return { text: t("settings.output.spaceUnavailable"), tone: null };
  if (usageState.value === "failed" || !usage.value) return { text: usageError.value ?? t("settings.output.spaceUnavailable"), tone: "danger" };
  const u = usage.value;
  if (u.entries === 0) return { text: t("settings.output.spaceEmpty"), tone: null };
  const size = formatBytes(u.captureBytes);
  const count = formatCount(u.entries);
  const text = u.entries === 1 ? t("settings.output.spaceMetaOne", { size }) : t("settings.output.spaceMeta", { size, count });
  return { text: u.pinned ? `${text} · ${t("settings.output.spacePinned", { n: formatCount(u.pinned) })}` : text, tone: null };
});

const nothingToClear = computed(() => usageState.value === "ready" && (usage.value?.clearableEntries ?? 0) === 0);

const clearText = computed(() => {
  const u = usage.value;
  if (!u) return tp("settings.output.clearTextUnknown");
  const size = formatBytes(u.clearableBytes);
  return u.clearableEntries === 1 ? tp("settings.output.clearTextOne", { size }) : tp("settings.output.clearText", { count: formatCount(u.clearableEntries), size });
});

async function confirmClear(): Promise<void> {
  clearing.value = true;
  clearError.value = null;
  try {
    const report = await history_clear();
    clearNote.value = t("settings.output.clearedNote", { size: formatBytes(report.freedBytes) });
  } catch (error) {
    clearError.value = describeError(error);
  } finally {
    clearing.value = false;
    asking.value = false;
    void loadUsage();
    await nextTick();
    clearButton.value?.focus();
  }
}

function cancelClear(): void {
  asking.value = false;
  void nextTick(() => clearButton.value?.focus());
}
</script>

<template>
  <SectionHead :title="t('settings.output.headSaving')" />
  <div class="g-group">
    <FolderRow :label="t('settings.output.folder')" :path="o.saveFolder" @change="(path) => update((s) => (s.output.saveFolder = path))" />
    <FormRow :label="t('settings.output.name')" :for="ids.template" :help="nameHelp" stacked>
      <div class="p-template">
        <input
          :id="ids.template"
          ref="field"
          class="g-field"
          data-kind="mono"
          :value="draft"
          spellcheck="false"
          autocomplete="off"
          :aria-invalid="showProblem ? 'true' : undefined"
          :aria-describedby="showProblem ? ids.templateError : undefined"
          @input="onInput"
          @focus="focused = true"
          @blur="onBlur"
        />
        <span v-if="showProblem && problem" :id="ids.templateError" class="g-help" data-tone="danger"
          ><GIcon name="bang" :size="14" /><span>{{ problemText(problem) }}</span></span
        >
        <div class="p-tokens" role="group" :aria-label="t('settings.output.insertLabel')">
          <span class="p-tokens-label" aria-hidden="true">{{ t("settings.output.insert") }}</span>
          <button
            v-for="token in TOKENS"
            :key="token"
            type="button"
            class="p-token"
            :aria-label="t('settings.output.insertToken', { token })"
            @mousedown.prevent
            @click="insert(token)"
          >
            {{ token }}
          </button>
        </div>
        <div class="p-preview">
          <span>{{ t("settings.output.preview") }}</span>
          <output :for="ids.template">{{ problem || !preview?.fileName ? "—" : preview.fileName }}</output>
        </div>
      </div>
    </FormRow>
    <FormRow :label="t('settings.output.format')" :label-id="ids.format" :help="formatHelp">
      <SegmentedControl :labelledby="ids.format" :model-value="o.format" :options="formats" @update:model-value="(v) => update((s) => (s.output.format = v))" />
    </FormRow>
    <!-- PNG and WebP are written lossless, so only JPEG has a quality. -->
    <FormRow v-if="o.format === 'jpeg'" :label="t('settings.output.quality')" :for="ids.quality">
      <div class="g-slider-row p-slider">
        <input
          :id="ids.quality"
          type="range"
          class="g-slider"
          min="50"
          max="100"
          :value="quality"
          :style="{ '--value': `${(quality - 50) * 2}%` }"
          @input="quality = Number(($event.target as HTMLInputElement).value)"
          @change="update((s) => (s.output.quality = quality))"
        />
        <span class="p-val">{{ t("settings.output.percent", { n: quality }) }}</span>
      </div>
    </FormRow>
    <!-- oxipng reads PNG only, so the switch shows with PNG the way Quality shows with JPEG. -->
    <FormRow v-if="o.format === 'png'" :label="t('settings.output.optimisePng')" :for="optimisePngId" :help="t('settings.output.optimisePngHelp')">
      <SwitchControl :id="optimisePngId" :model-value="o.optimisePng" @update:model-value="(v) => update((s) => (s.output.optimisePng = v))" />
    </FormRow>
    <FormRow :label="tp('settings.output.retina')" :label-id="ids.retina" :help="t('settings.output.retinaHelp')">
      <SegmentedControl :labelledby="ids.retina" :model-value="o.retina" :options="retinas" @update:model-value="(v) => update((s) => (s.output.retina = v))" />
    </FormRow>
    <FormRow :label="t('settings.output.copyAfterSave')" :for="ids.copy">
      <SwitchControl :id="ids.copy" :model-value="o.copyAfterSave" @update:model-value="(v) => update((s) => (s.output.copyAfterSave = v))" />
    </FormRow>
  </div>

  <SendTargets v-if="can('sendToApp')" :settings="settings" />

  <template v-if="can('windowCapture')">
  <SectionHead :title="t('settings.output.headWindows')" />
  <div class="g-group">
    <FormRow v-if="can('windowShadow')" :label="t('settings.output.shadow')" :for="ids.shadow">
      <SwitchControl :id="ids.shadow" :model-value="o.windowShadow" @update:model-value="(v) => update((s) => (s.output.windowShadow = v))" />
    </FormRow>
    <FormRow :label="t('settings.output.corners')" :for="ids.corners">
      <SwitchControl :id="ids.corners" :model-value="o.windowCorners" @update:model-value="(v) => update((s) => (s.output.windowCorners = v))" />
    </FormRow>
    <FormRow :label="t('settings.output.background')" :label-id="ids.background" :help="t('settings.output.backgroundHelp')">
      <SegmentedControl :labelledby="ids.background" :model-value="o.windowBackground" :options="backgrounds" @update:model-value="(v) => update((s) => (s.output.windowBackground = v))" />
    </FormRow>
  </div>
  </template>

  <SectionHead :title="t('settings.output.headHistory')" />
  <div class="g-group">
    <FormRow :label="t('settings.output.keep')" :for="ids.keep" :help="tp(o.trashSavedFiles ? 'settings.output.keepHelpTrash' : 'settings.output.keepHelp')">
      <SelectControl :id="ids.keep" :model-value="o.historyRetention" :options="retentions" @update:model-value="(v) => update((s) => (s.output.historyRetention = v))" />
    </FormRow>
    <FormRow :label="tp('settings.output.trashSaved')" :for="ids.trash" :help="tp('settings.output.trashSavedHelp')" sub :disabled="o.historyRetention === 'forever' || o.historyRetention === 'off'">
      <SwitchControl
        :id="ids.trash"
        :model-value="o.trashSavedFiles"
        :disabled="o.historyRetention === 'forever' || o.historyRetention === 'off'"
        @update:model-value="(v) => update((s) => (s.output.trashSavedFiles = v))"
      />
    </FormRow>
    <FormRow :label="t('settings.output.space')" :help="spaceHelp.text" :tone="spaceHelp.tone">
      <button ref="clearButton" type="button" class="g-button" data-variant="destructive" :disabled="nothingToClear || clearing" @click="(clearNote = null), (clearError = null), (asking = true)">
        {{ t("settings.output.clear") }}
      </button>
    </FormRow>
  </div>

  <Teleport to="body">
    <ConfirmDialog
      v-if="asking"
      icon="trash"
      tone="neutral"
      :title="t('settings.output.clearTitle')"
      :text="clearText"
      :confirm-label="t('settings.output.clearConfirm')"
      :cancel-label="t('settings.common.cancel')"
      destructive
      :busy="clearing"
      @confirm="confirmClear"
      @cancel="cancelClear"
    />
  </Teleport>
</template>
