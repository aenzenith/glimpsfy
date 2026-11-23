<script setup lang="ts">
// One row of a provider's settings form, drawn from its description: a text, URL, secret, select,
// number, toggle or name-and-value list. Plain values save into
// `settings.upload.providers[<id>][<key>]`; a secret goes once to the keychain through
// `upload_set_secret` and never comes back, so its field always starts empty, and
// `upload_secret_status` says whether one is stored. The core's own check of the saved values
// (`issues`, from `upload_validate`) shows under the row once the user has typed nothing newer.
import { computed, onBeforeUnmount, onMounted, reactive, ref, watch } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SelectControl from "../../components/SelectControl.vue";
import SwitchControl from "../../components/SwitchControl.vue";
import { upload_remove_secret, upload_secret_status, upload_set_secret, type Issue } from "../../lib/api/upload";
import { t, tp } from "../../lib/i18n";
import { issueText, tk } from "../../upload/text";
import { describeError, uid } from "../text";
import { helpOf, labelOf, optionLabel, pairSecretKey, placeholderOf, ruleProblem, type Field, type Pair } from "../upload";

const props = withDefaults(defineProps<{ providerId: string; field: Field; value: unknown; issues?: Issue[] }>(), { issues: () => [] });
const emit = defineEmits<{
  change: [value: unknown];
  /** A value went into the keychain or left it, so the page checks the form again. */
  secretChanged: [];
}>();

const id = uid(`f-${props.field.key}`);
const errorId = `${id}-error`;
const label = computed(() => labelOf(props.field));
const help = computed(() => helpOf(props.field));

/** The core's first word on this row: an error before a warning. */
const issue = computed(() => props.issues.find((i) => i.severity === "error") ?? props.issues[0] ?? null);
const issueHelp = computed<{ text: string; tone: "danger" | "warn" } | null>(() =>
  issue.value ? { text: issueText(issue.value), tone: issue.value.severity === "error" ? "danger" : "warn" } : null,
);

// ------------------------------------------------------------------------- text, url, number
const textValue = (value: unknown): string => {
  if (typeof value === "string") return value;
  if (typeof value === "number") return String(value);
  const f = props.field;
  if ((f.kind === "text" || f.kind === "url") && f.default) return f.default;
  if (f.kind === "number" && f.default !== null && f.default !== undefined) return String(f.default);
  return "";
};

const draft = ref(textValue(props.value));
const focused = ref(false);
const touched = ref(false);
let timer: ReturnType<typeof setTimeout> | undefined;

watch(
  () => props.value,
  (value) => {
    if (!focused.value && timer === undefined) draft.value = textValue(value);
  },
);

const numberProblem = (text: string): string | null => {
  const f = props.field;
  if (f.kind !== "number") return null;
  if (!text.trim()) return f.required ? tk("upload.issue.required") : null;
  const n = Number(text);
  if (!Number.isFinite(n)) return t("settings.upload.number");
  if ((f.min ?? null) !== null && n < (f.min as number)) return t("settings.upload.numberMin", { min: f.min as number });
  if ((f.max ?? null) !== null && n > (f.max as number)) return t("settings.upload.numberMax", { max: f.max as number });
  return null;
};

const problem = computed(() => (props.field.kind === "number" ? numberProblem(draft.value) : ruleProblem(props.field, draft.value)));
const showProblem = computed(() => touched.value && !!problem.value);
/** What the row says under its value: the typed value's problem, else the core's, else help. */
const textHelp = computed<{ text: string; tone: "danger" | "warn" | null }>(() => {
  if (showProblem.value) return { text: problem.value as string, tone: "danger" };
  // A required row that was never filled in is not an error until the user has been there.
  if (issueHelp.value && (touched.value || draft.value || issueHelp.value.tone === "warn")) return issueHelp.value;
  return { text: help.value, tone: null };
});

function commit(): void {
  if (problem.value && draft.value.trim()) return;
  if (props.field.kind === "number") {
    const next = draft.value.trim() ? Number(draft.value) : null;
    if (next !== props.value) emit("change", next);
    return;
  }
  if (draft.value !== textValue(props.value) || props.value === undefined) emit("change", draft.value);
}

function onInput(event: Event): void {
  draft.value = (event.target as HTMLInputElement).value;
  if (timer !== undefined) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = undefined;
    commit();
  }, 400);
}

function onBlur(): void {
  focused.value = false;
  touched.value = true;
  if (timer !== undefined) {
    clearTimeout(timer);
    timer = undefined;
    commit();
  }
}

onBeforeUnmount(() => {
  if (timer !== undefined) {
    clearTimeout(timer);
    timer = undefined;
    commit();
  }
});

const input = ref<HTMLInputElement | null>(null);
const tokens = computed(() => (props.field.kind === "text" ? props.field.tokens : []));
function insert(token: string): void {
  const el = input.value;
  const text = `{${token}}`;
  const start = el?.selectionStart ?? draft.value.length;
  const end = el?.selectionEnd ?? start;
  draft.value = draft.value.slice(0, start) + text + draft.value.slice(end);
  if (timer !== undefined) clearTimeout(timer);
  timer = setTimeout(() => {
    timer = undefined;
    commit();
  }, 400);
  requestAnimationFrame(() => {
    el?.focus();
    el?.setSelectionRange(start + text.length, start + text.length);
  });
}

const mono = computed(() => (props.field.kind === "text" && props.field.monospace) || props.field.kind === "url" || props.field.kind === "secret");
const stacked = computed(() => ["url", "secret", "pairs"].includes(props.field.kind) || mono.value || tokens.value.length > 0);

// ---------------------------------------------------------------------------- select, toggle
const selectOptions = computed(() => (props.field.kind === "select" ? props.field.options.map((o) => ({ value: o.value, label: optionLabel(o) })) : []));
const selectValue = computed(() => {
  if (props.field.kind !== "select") return "";
  const value = typeof props.value === "string" ? props.value : (props.field.default ?? props.field.options[0]?.value ?? "");
  return value;
});
const toggleValue = computed(() => (typeof props.value === "boolean" ? props.value : props.field.kind === "toggle" ? !!props.field.default : false));

// ------------------------------------------------------------------------------------ secrets
/** A row whose value lives in the keychain: a `secret` row, or any single-value row marked secret,
 * so a secret never reaches the settings file even if a description mixes the two. */
const isSecret = computed(() => props.field.kind === "secret" || (props.field.secret && props.field.kind !== "pairs"));
/** `onFile`: the keychain held a value when the page opened; `kept`: the user stored one now. */
type SecretState = "unknown" | "onFile" | "missing" | "kept" | "removed";
const secret = ref("");
const secretState = ref<SecretState>("unknown");
const secretBusy = ref(false);
const secretError = ref<string | null>(null);
const reveal = ref(false);

async function storeSecret(): Promise<void> {
  const value = secret.value.trim();
  if (!value || secretBusy.value) return;
  secretBusy.value = true;
  secretError.value = null;
  try {
    await upload_set_secret(props.providerId, props.field.key, value);
    secret.value = "";
    reveal.value = false;
    secretState.value = "kept";
    emit("secretChanged");
  } catch (error) {
    secretError.value = describeError(error);
  } finally {
    secretBusy.value = false;
  }
}

async function removeSecret(): Promise<void> {
  if (secretBusy.value) return;
  secretBusy.value = true;
  secretError.value = null;
  try {
    await upload_remove_secret(props.providerId, props.field.key);
    secret.value = "";
    secretState.value = "removed";
    emit("secretChanged");
  } catch (error) {
    secretError.value = describeError(error);
  } finally {
    secretBusy.value = false;
  }
}

const secretHelp = computed<{ text: string; tone: "ok" | "danger" | "warn" | null; icon: boolean }>(() => {
  if (secretError.value) return { text: secretError.value, tone: "danger", icon: false };
  if (secretState.value === "kept") return { text: tp("settings.upload.secretKept"), tone: "ok", icon: false };
  if (secretState.value === "onFile") return { text: tp("settings.upload.secretOnFile"), tone: null, icon: true };
  if (secretState.value === "removed") return { text: tp("settings.upload.secretRemoved"), tone: null, icon: false };
  // Nothing stored yet is the normal start of a host, so it reads as a note, not an error.
  if (secretState.value === "missing" && issueHelp.value) return { text: issueHelp.value.text, tone: "warn", icon: false };
  return { text: help.value || tk("upload.help.keychain"), tone: null, icon: true };
});

const stored = computed(() => secretState.value === "kept" || secretState.value === "onFile");
const secretPlaceholder = computed(() => {
  if (stored.value) return t("settings.upload.secretStored");
  return (props.field.kind === "secret" || props.field.kind === "text" ? placeholderOf(props.field.placeholder) : "") || t("settings.upload.secretPaste");
});

/** Whether the keychain holds `key`, or `null` when it cannot be asked (locked, refused). */
async function hasSecret(key: string): Promise<boolean | null> {
  try {
    return await upload_secret_status(props.providerId, key);
  } catch {
    return null;
  }
}

onMounted(async () => {
  if (isSecret.value) {
    const held = await hasSecret(props.field.key);
    if (secretState.value === "unknown" && held !== null) secretState.value = held ? "onFile" : "missing";
  } else if (secretValues.value) {
    await Promise.all(
      rows.map(async (row) => {
        if (!row.saved) return;
        const held = await hasSecret(pairSecretKey(props.field.key, row.saved));
        if (row.state === "unknown" && held !== null) row.state = held ? "onFile" : "missing";
      }),
    );
  }
});

// -------------------------------------------------------------------------------------- pairs
const secretValues = computed(() => props.field.kind === "pairs" && (props.field.secretValues || props.field.secret));
type Row = { key: number; name: string; value: string; saved: string | null; state: SecretState; error: string | null };
let nextRow = 0;
const rows = reactive<Row[]>([]);

function loadRows(): void {
  rows.splice(0, rows.length);
  const value = props.value;
  if (!Array.isArray(value)) return;
  for (const item of value) {
    if (typeof item === "string") rows.push({ key: nextRow++, name: item, value: "", saved: item, state: "unknown", error: null });
    else if (item && typeof item === "object" && "name" in item) {
      const pair = item as Pair;
      rows.push({ key: nextRow++, name: String(pair.name ?? ""), value: String(pair.value ?? ""), saved: String(pair.name ?? ""), state: "unknown", error: null });
    }
  }
}
if (props.field.kind === "pairs") loadRows();

function pairsValue(): unknown {
  const named = rows.filter((r) => r.name.trim());
  if (secretValues.value) return named.filter((r) => r.saved !== null).map((r) => r.saved as string);
  return named.map((r) => ({ name: r.name.trim(), value: r.value }));
}

function addRow(): void {
  rows.push({ key: nextRow++, name: "", value: "", saved: null, state: "unknown", error: null });
}

async function onPairName(row: Row): Promise<void> {
  const name = row.name.trim();
  row.error = null;
  if (!secretValues.value) {
    emit("change", pairsValue());
    return;
  }
  if (name === (row.saved ?? "")) return;
  if (!name) return;
  if (rows.some((r) => r !== row && r.saved === name)) {
    row.error = t("settings.upload.pairDuplicate", { name });
    return;
  }
  const old = row.saved;
  if (old) {
    try {
      await upload_remove_secret(props.providerId, pairSecretKey(props.field.key, old));
    } catch {
      // The old value may not exist; the new name needs its value again either way.
    }
    row.state = "unknown";
  }
  row.saved = name;
  emit("change", pairsValue());
  if (row.value) await onPairValue(row);
}

async function onPairValue(row: Row): Promise<void> {
  if (!secretValues.value) {
    emit("change", pairsValue());
    return;
  }
  const value = row.value;
  if (!value || !row.saved) return;
  row.error = null;
  try {
    await upload_set_secret(props.providerId, pairSecretKey(props.field.key, row.saved), value);
    row.value = "";
    row.state = "kept";
    emit("secretChanged");
  } catch (error) {
    row.error = describeError(error);
  }
}

async function removeRow(row: Row): Promise<void> {
  const index = rows.indexOf(row);
  if (index < 0) return;
  rows.splice(index, 1);
  if (secretValues.value && row.saved) {
    try {
      await upload_remove_secret(props.providerId, pairSecretKey(props.field.key, row.saved));
    } catch {
      // Nothing was stored under it, or the keychain is not reachable; the name is gone anyway.
    }
  }
  emit("change", pairsValue());
  if (secretValues.value) emit("secretChanged");
}

const pairError = computed(() => rows.find((r) => r.error)?.error ?? null);
const pairHelp = computed<{ text: string; tone: "ok" | "danger" | "warn" | null }>(() => {
  if (pairError.value) return { text: pairError.value, tone: "danger" };
  if (issueHelp.value) return issueHelp.value;
  if (secretValues.value && rows.some((r) => r.state === "kept")) return { text: tp("settings.upload.pairsKept"), tone: "ok" };
  if (secretValues.value && rows.some((r) => r.state === "missing")) return { text: tp("settings.upload.pairsMissing"), tone: "warn" };
  return { text: help.value || (secretValues.value ? tp("settings.upload.pairsHelp") : ""), tone: null };
});
const namePlaceholder = computed(() => (props.field.kind === "pairs" ? placeholderOf(props.field.namePlaceholder) : ""));
const valuePlaceholder = (row: Row) => {
  if (secretValues.value && row.saved && (row.state === "onFile" || row.state === "kept")) return t("settings.upload.pairValueReplace");
  return props.field.kind === "pairs" ? placeholderOf(props.field.valuePlaceholder) : "";
};

defineExpose({
  /** Marks the field as visited, so a Test shows its problem. */
  touch: () => (touched.value = true),
  invalid: () => !!problem.value && (props.field.kind === "text" || props.field.kind === "url" || props.field.kind === "number"),
});
</script>

<template>
  <!-- secret -->
  <FormRow
    v-if="isSecret"
    :label="label"
    :for="id"
    :help="secretHelp.icon ? null : secretHelp.text"
    :tone="secretHelp.tone"
    :help-id="`${id}-help`"
    stacked
  >
    <template v-if="secretHelp.icon" #label>
      <label :for="id">{{ label }}</label>
      <span class="g-help" :id="`${id}-help`"><GIcon name="lock" :size="14" />{{ secretHelp.text }}</span>
    </template>
    <template #after>
      <div class="p-actions p-flush">
        <button v-if="stored" type="button" class="g-button" data-variant="plain" :disabled="secretBusy" @click="removeSecret">{{ tp("settings.upload.removeSecret") }}</button>
      </div>
    </template>
    <div class="p-secret">
      <input
        :id="id"
        v-model="secret"
        class="g-field"
        data-kind="mono"
        :type="reveal ? 'text' : 'password'"
        :placeholder="secretPlaceholder"
        spellcheck="false"
        autocomplete="off"
        :aria-describedby="`${id}-help`"
        :disabled="secretBusy"
        @keydown.enter.prevent="storeSecret"
        @blur="storeSecret"
      />
      <button
        type="button"
        class="g-btn"
        data-size="small"
        :aria-label="t('settings.upload.reveal')"
        :aria-pressed="reveal ? 'true' : 'false'"
        :disabled="!secret"
        @mousedown.prevent
        @click="reveal = !reveal"
      >
        <GIcon name="eye" :size="16" />
      </button>
    </div>
  </FormRow>

  <!-- select -->
  <FormRow v-else-if="field.kind === 'select'" :label="label" :for="id" :help="issueHelp?.text ?? help" :tone="issueHelp?.tone ?? null">
    <SelectControl :id="id" :model-value="selectValue" :options="selectOptions" @update:model-value="(v) => emit('change', v)" />
  </FormRow>

  <!-- toggle -->
  <FormRow v-else-if="field.kind === 'toggle'" :label="label" :for="id" :help="issueHelp?.text ?? help" :tone="issueHelp?.tone ?? null">
    <SwitchControl :id="id" :model-value="toggleValue" @update:model-value="(v) => emit('change', v)" />
  </FormRow>

  <!-- pairs -->
  <FormRow v-else-if="field.kind === 'pairs'" :label="label" :label-id="`${id}-label`" :help="secretValues && !pairHelp.tone ? null : pairHelp.text" :tone="pairHelp.tone" stacked>
    <template v-if="secretValues && !pairHelp.tone" #label>
      <span class="g-label" :id="`${id}-label`">{{ label }}</span>
      <span v-if="pairHelp.text" class="g-help"><GIcon name="lock" :size="14" />{{ pairHelp.text }}</span>
    </template>
    <div class="p-col">
      <div v-if="rows.length" class="p-kv" role="group" :aria-labelledby="`${id}-label`">
        <span class="p-kv-head">{{ t("settings.upload.pairName") }}</span>
        <span class="p-kv-head">{{ t("settings.upload.pairValue") }}</span>
        <span></span>
        <template v-for="(row, index) in rows" :key="row.key">
          <input
            v-model="row.name"
            class="g-field"
            data-kind="mono"
            spellcheck="false"
            autocomplete="off"
            :placeholder="namePlaceholder"
            :aria-label="t('settings.upload.pairNameAria', { n: index + 1 })"
            @blur="onPairName(row)"
          />
          <input
            v-model="row.value"
            class="g-field"
            data-kind="mono"
            :type="secretValues ? 'password' : 'text'"
            spellcheck="false"
            autocomplete="off"
            :placeholder="valuePlaceholder(row)"
            :aria-label="t('settings.upload.pairValueAria', { n: index + 1 })"
            @blur="onPairValue(row)"
          />
          <button type="button" class="g-btn" data-size="small" :aria-label="t('settings.upload.pairRemove', { n: index + 1 })" @click="removeRow(row)">
            <GIcon name="minus" :size="16" />
          </button>
        </template>
      </div>
      <div class="p-flush">
        <button type="button" class="g-button" data-variant="plain" @click="addRow"><GIcon name="plus" :size="16" />{{ t("settings.upload.pairAdd") }}</button>
      </div>
    </div>
  </FormRow>

  <!-- text, url, number -->
  <FormRow
    v-else
    :label="label"
    :for="id"
    :help="textHelp.text"
    :tone="textHelp.tone"
    :help-id="errorId"
    :stacked="stacked"
  >
    <div v-if="tokens.length" class="p-template">
      <input
        :id="id"
        ref="input"
        class="g-field"
        :data-kind="mono ? 'mono' : undefined"
        :value="draft"
        :placeholder="placeholderOf(field.kind === 'text' ? field.placeholder : null)"
        spellcheck="false"
        autocomplete="off"
        :aria-invalid="textHelp.tone === 'danger' ? 'true' : undefined"
        :aria-describedby="errorId"
        @input="onInput"
        @focus="focused = true"
        @blur="onBlur"
      />
      <div class="p-tokens" role="group" :aria-label="t('settings.output.insertLabel')">
        <span class="p-tokens-label" aria-hidden="true">{{ t("settings.output.insert") }}</span>
        <button
          v-for="token in tokens"
          :key="token"
          type="button"
          class="p-token"
          :aria-label="t('settings.output.insertToken', { token: `{${token}}` })"
          @mousedown.prevent
          @click="insert(token)"
          v-text="`{${token}}`"
        />
      </div>
    </div>
    <input
      v-else
      :id="id"
      class="g-field"
      :data-kind="mono ? 'mono' : undefined"
      :type="field.kind === 'number' ? 'number' : field.kind === 'url' ? 'url' : 'text'"
      :min="field.kind === 'number' ? (field.min ?? undefined) : undefined"
      :max="field.kind === 'number' ? (field.max ?? undefined) : undefined"
      :step="field.kind === 'number' ? (field.step ?? undefined) : undefined"
      :value="draft"
      :placeholder="placeholderOf(field.kind === 'text' || field.kind === 'url' ? field.placeholder : null)"
      :style="stacked ? undefined : { width: field.kind === 'number' ? '120px' : '240px' }"
      spellcheck="false"
      autocomplete="off"
      :aria-invalid="textHelp.tone === 'danger' ? 'true' : undefined"
      :aria-describedby="errorId"
      @input="onInput"
      @focus="focused = true"
      @blur="onBlur"
    />
  </FormRow>
</template>
