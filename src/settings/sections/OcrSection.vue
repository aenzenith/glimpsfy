<script setup lang="ts">
// Settings › Text recognition: the engine, the PP-OCR models, the languages the engine reads and
// how the text comes out. Every state says what this build can really do: the PP-OCR engine is
// not in this build (`ocr_model_download` answers `not_implemented`), so its models are listed
// with their sizes and nothing offers a download; a model that is on disk can still be removed.
import { computed, onBeforeUnmount, onMounted, reactive, ref } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import SwitchControl from "../../components/SwitchControl.vue";
import {
  OCR_MODEL_PROGRESS,
  ocr_cancel,
  ocr_languages,
  ocr_model_download,
  ocr_model_remove,
  ocr_engines,
  ocr_models,
  type LanguageInfo,
  type ModelInfo,
  type ModelProgress,
} from "../../lib/api/ocr";
import { ApiError, on } from "../../lib/api/core";
import { language, t } from "../../lib/i18n";
import type { OcrEngineChoice, Settings } from "../../lib/settings";
import { update } from "../store";
import { describeError, formatBytes, isNotImplemented, uid } from "../text";

const props = defineProps<{ settings: Settings }>();
const ocr = computed(() => props.settings.ocr);

const ids = { engine: uid("engine"), languages: uid("langs"), detect: uid("detect"), breaks: uid("breaks"), codes: uid("codes") };

const platform = typeof navigator === "undefined" ? "mac" : /Mac/.test(navigator.platform || navigator.userAgent) ? "mac" : /Win/.test(navigator.platform || navigator.userAgent) ? "windows" : "other";
const systemName = computed(() => t(platform === "mac" ? "settings.ocr.vision" : platform === "windows" ? "settings.ocr.windowsOcr" : "settings.ocr.systemEngine"));
const systemHelp = computed(() => t(platform === "mac" ? "settings.ocr.visionHelp" : platform === "windows" ? "settings.ocr.windowsOcrHelp" : "settings.ocr.systemEngineHelp"));

/** Whether PP-OCR reads text in this build (`ocr_engines`); until it does, it can be left but
 * not chosen. */
const ppOcrReady = ref(false);

function pickEngine(engine: OcrEngineChoice): void {
  if (engine === "ppOcr" && !ppOcrReady.value) return;
  if (engine !== ocr.value.engine) void update((s) => (s.ocr.engine = engine));
}

// ------------------------------------------------------------------------------------- models
const models = ref<ModelInfo[] | null>(null);
const modelsError = ref<string | null>(null);
/** Downloads under way, by model: the job that can cancel it and the bytes so far. */
const downloads = reactive(new Map<string, { job: string; received: number; total: number }>());
const modelProblem = ref<string | null>(null);
let stopProgress: (() => void) | null = null;

async function loadModels(): Promise<void> {
  try {
    models.value = await ocr_models();
    modelsError.value = null;
  } catch (error) {
    modelsError.value = isNotImplemented(error) ? t("settings.ocr.modelsMissing") : describeError(error);
  }
}

async function download(model: ModelInfo): Promise<void> {
  if (!ppOcrReady.value || downloads.has(model.id)) return;
  const job = `model-${model.id}-${Date.now()}`;
  downloads.set(model.id, { job, received: 0, total: model.bytes });
  modelProblem.value = null;
  try {
    await ocr_model_download(model.id, job);
  } catch (error) {
    if (!(error instanceof ApiError && error.code === "cancelled")) modelProblem.value = describeError(error);
  } finally {
    downloads.delete(model.id);
    await loadModels();
  }
}

async function remove(model: ModelInfo): Promise<void> {
  modelProblem.value = null;
  try {
    await ocr_model_remove(model.id);
  } catch (error) {
    modelProblem.value = describeError(error);
  }
  await loadModels();
}

function modelMeta(model: ModelInfo): string {
  const progress = downloads.get(model.id);
  const size = progress && progress.total > 0
    ? t("settings.ocr.modelProgress", { p: Math.round((progress.received / progress.total) * 100), size: formatBytes(progress.total) })
    : formatBytes(model.bytes);
  return [model.languages.map(nameOf).join(", "), size].filter(Boolean).join(" · ");
}

onMounted(async () => {
  void ocr_engines().then((engines) => (ppOcrReady.value = engines.ready.includes("ppOcr")), () => (ppOcrReady.value = false));
  void loadModels();
  stopProgress = await on<ModelProgress>(OCR_MODEL_PROGRESS, (p) => {
    const d = downloads.get(p.model);
    if (d) Object.assign(d, { received: p.received, total: p.total });
  });
});
onBeforeUnmount(() => {
  stopProgress?.();
  for (const d of downloads.values()) void ocr_cancel(d.job).catch(() => undefined);
});

// ---------------------------------------------------------------------------------- languages
const list = ref<LanguageInfo[] | null>(null);
const listState = ref<"loading" | "ready" | "unavailable" | "failed">("loading");
const listError = ref<string | null>(null);

onMounted(async () => {
  try {
    list.value = await ocr_languages();
    listState.value = "ready";
  } catch (error) {
    listState.value = isNotImplemented(error) ? "unavailable" : "failed";
    listError.value = describeError(error);
  }
});

function nameOf(tag: string): string {
  try {
    const names = new Intl.DisplayNames([language.value], { type: "language" });
    const name = names.of(tag);
    return name ? name.charAt(0).toLocaleUpperCase(language.value) + name.slice(1) : tag;
  } catch {
    return tag;
  }
}

type Choice = { tag: string; name: string; available: boolean };

/** The engine's languages, and a ticked language the engine no longer lists, so it can be
 * unticked. */
const choices = computed<Choice[]>(() => {
  const seen = new Set<string>();
  const out: Choice[] = [];
  for (const info of list.value ?? []) {
    if (seen.has(info.tag)) continue;
    seen.add(info.tag);
    out.push({ tag: info.tag, name: nameOf(info.tag), available: info.available });
  }
  for (const tag of ocr.value.languages) {
    if (!seen.has(tag)) {
      seen.add(tag);
      out.push({ tag, name: nameOf(tag), available: listState.value !== "ready" });
    }
  }
  return out.sort((a, b) => a.name.localeCompare(b.name, language.value));
});

const unavailableCount = computed(() => (list.value ?? []).filter((l) => !l.available).length);

function toggle(tag: string, on: boolean): void {
  void update((s) => {
    const next = s.ocr.languages.filter((l) => l !== tag);
    if (on) next.push(tag);
    s.ocr.languages = next;
  });
}

const languagesHelp = computed<{ text: string; tone: "warn" | "danger" | null }>(() => {
  if (listState.value === "loading") return { text: t("settings.ocr.languagesLoading"), tone: null };
  if (listState.value === "unavailable") return { text: t("settings.ocr.languagesUnavailable"), tone: "warn" };
  if (listState.value === "failed") return { text: listError.value ?? "", tone: "danger" };
  if (!choices.value.length) return { text: t("settings.ocr.languagesEmpty"), tone: "warn" };
  if (unavailableCount.value) return { text: `${t("settings.ocr.languagesHelp")} ${t("settings.ocr.languagesDownload", { n: unavailableCount.value })}`, tone: null };
  return { text: t("settings.ocr.languagesHelp"), tone: null };
});
</script>

<template>
  <SectionHead :title="t('settings.ocr.headEngine')" />
  <div class="g-group" role="radiogroup" :aria-label="t('settings.ocr.headEngine')">
    <label class="g-form-row p-engine">
      <input type="radio" class="g-radio" :name="ids.engine" :checked="ocr.engine === 'system'" @change="pickEngine('system')" />
      <span class="g-form-label">
        <span class="g-label">{{ systemName }}</span>
        <span class="g-help">{{ systemHelp }}</span>
      </span>
      <span class="g-form-control"><span class="g-badge" data-tone="iris">{{ t("settings.ocr.recommended") }}</span></span>
    </label>
    <label class="g-form-row p-engine" :aria-disabled="!ppOcrReady && ocr.engine !== 'ppOcr' ? 'true' : undefined">
      <input
        type="radio"
        class="g-radio"
        :name="ids.engine"
        :checked="ocr.engine === 'ppOcr'"
        :disabled="!ppOcrReady && ocr.engine !== 'ppOcr'"
        @change="pickEngine('ppOcr')"
      />
      <span class="g-form-label">
        <span class="g-label">PP-OCR</span>
        <span class="g-help">{{ t("settings.ocr.ppOcrHelp") }}</span>
        <span v-if="!ppOcrReady" class="g-help" data-tone="warn"><GIcon name="warning" :size="14" />{{ t("settings.ocr.ppOcrMissing") }}</span>
      </span>
    </label>
  </div>

  <SectionHead :title="t('settings.ocr.headModels')" :desc="t('settings.ocr.modelsDesc')" />
  <div class="g-group">
    <FormRow v-if="modelsError || !models" :label="t('settings.ocr.models')" :help="modelsError ?? t('settings.ocr.modelsLoading')" :tone="modelsError ? 'warn' : null" />
    <template v-else>
      <FormRow v-if="!ppOcrReady" :help="t('settings.ocr.modelsMissing')" tone="warn" />
      <FormRow v-if="modelProblem" :help="modelProblem" tone="danger" />
      <FormRow v-if="!models.length" :help="t('settings.ocr.modelsNone')" />
      <FormRow v-for="model in models" :key="model.id" :label="model.title" :help="modelMeta(model)">
        <span v-if="model.installed" class="g-badge" data-tone="ok">{{ t("settings.ocr.modelInstalled") }}</span>
        <button v-if="model.installed" type="button" class="g-button" :disabled="downloads.has(model.id)" @click="remove(model)">
          {{ t("settings.ocr.modelRemove") }}
        </button>
        <button v-else-if="downloads.has(model.id)" type="button" class="g-button" @click="ocr_cancel(downloads.get(model.id)!.job)">
          {{ t("settings.common.cancel") }}
        </button>
        <button v-else-if="ppOcrReady" type="button" class="g-button" @click="download(model)">{{ t("settings.ocr.modelDownload") }}</button>
      </FormRow>
    </template>
  </div>

  <SectionHead :title="t('settings.ocr.headRecognition')" />
  <div class="g-group">
    <FormRow :label="t('settings.ocr.detect')" :for="ids.detect">
      <SwitchControl :id="ids.detect" :model-value="ocr.detectLanguage" @update:model-value="(v) => update((s) => (s.ocr.detectLanguage = v))" />
    </FormRow>
    <FormRow :label="t('settings.ocr.languages')" :label-id="ids.languages" :help="languagesHelp.text" :tone="languagesHelp.tone" stacked>
      <div v-if="choices.length" class="p-checks" role="group" :aria-labelledby="ids.languages">
        <label v-for="choice in choices" :key="choice.tag" class="g-choice" :title="choice.tag">
          <input
            type="checkbox"
            class="g-checkbox"
            :checked="ocr.languages.includes(choice.tag)"
            :disabled="!choice.available && !ocr.languages.includes(choice.tag)"
            @change="toggle(choice.tag, ($event.target as HTMLInputElement).checked)"
          />{{ choice.name }}
        </label>
      </div>
    </FormRow>
    <FormRow :label="t('settings.ocr.lineBreaks')" :for="ids.breaks" :help="t('settings.ocr.lineBreaksHelp')">
      <SwitchControl :id="ids.breaks" :model-value="ocr.keepLineBreaks" @update:model-value="(v) => update((s) => (s.ocr.keepLineBreaks = v))" />
    </FormRow>
    <FormRow :label="t('settings.ocr.codes')" :for="ids.codes" :help="t('settings.ocr.codesHelp')">
      <SwitchControl :id="ids.codes" :model-value="ocr.detectCodes" @update:model-value="(v) => update((s) => (s.ocr.detectCodes = v))" />
    </FormRow>
  </div>
  <p class="p-note"><GIcon name="shield" :size="16" /><span>{{ t("settings.ocr.privacy") }}</span></p>
</template>
