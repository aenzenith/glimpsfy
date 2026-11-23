<script setup lang="ts">
// One host's page: its header with Make default, the terms in short from its facts, its form drawn
// from the description with the core's check of the saved values (`upload_validate`), the ShareX
// import of the custom provider, the connection test and the first-upload confirmation.
//
// The test sends a request to the host, so it needs the confirmation like an upload: an
// unconfirmed destination gets the first-upload dialog with `purpose: "test"` first. Whether the
// destination is confirmed comes from `upload_preflight`; Ask again takes the confirmation back
// with `settings_revoke_consent`, because a page cannot change it through `settings_set`.
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import { ApiError } from "../../lib/api/core";
import { settings_revoke_consent } from "../../lib/api/settings";
import {
  upload_import_sxcu,
  upload_preflight,
  upload_test,
  upload_validate,
  type ConsentInfo,
  type Issue,
} from "../../lib/api/upload";
import { app_open_url } from "../../lib/api/windows";
import { t, tp } from "../../lib/i18n";
import type { Settings } from "../../lib/settings";
import FirstUploadDialog from "../../upload/FirstUploadDialog.vue";
import { hostTerms, issueText, testKind, tk } from "../../upload/text";
import { banner, update } from "../store";
import { describeError } from "../text";
import { bagOf, destinationOf, sectionOf, summaryOf, type Field, type Provider } from "../upload";
import ProviderField from "./ProviderField.vue";

const props = defineProps<{ settings: Settings; provider: Provider }>();

const isDefault = computed(() => props.settings.upload.defaultProvider === props.provider.id);
const bag = computed(() => bagOf(props.settings.upload.providers, props.provider.id));

/** The rows in order, with the rows of one section together under its heading. */
const groups = computed(() => {
  const out: { section: string | null; fields: Field[] }[] = [];
  for (const field of props.provider.settings) {
    const section = field.section ?? null;
    const last = out[out.length - 1];
    if (last && last.section === section) last.fields.push(field);
    else out.push({ section, fields: [field] });
  }
  return out;
});
/** The group whose heading carries the link to get a key: the first that holds a secret. */
const keyGroup = computed(() => groups.value.findIndex((g) => g.fields.some((f) => f.kind === "secret" || f.secret)));
const keyLabel = computed(() => t(props.provider.account === "clientId" ? "settings.upload.registerApp" : "settings.upload.getKey"));

const terms = computed(() => hostTerms(props.provider));
const hostLine = computed(() => summaryOf(props.provider));
const anonymous = computed(() => props.provider.account === "none");

function makeDefault(): void {
  void update((s) => (s.upload.defaultProvider = props.provider.id));
}

function setValue(key: string, value: unknown): void {
  void update((s) => {
    const current = s.upload.providers[props.provider.id];
    const next = current && typeof current === "object" && !Array.isArray(current) ? (current as Record<string, unknown>) : {};
    next[key] = value;
    s.upload.providers[props.provider.id] = next;
  }).then(() => check());
}

function open(url: string | null | undefined): void {
  if (!url) return;
  void app_open_url(url).catch((error) => (banner.value = { tone: "danger", text: describeError(error) }));
}

// ------------------------------------------------------------------- the core's check
/** What `upload_validate` says about the saved values, missing secrets included. */
const issues = ref<Issue[]>([]);
/** The destination as `upload_preflight` sees it, `null` while the settings name none. */
const consent = ref<ConsentInfo | null>(null);
let checkSeq = 0;
let checkTimer: ReturnType<typeof setTimeout> | undefined;

async function check(): Promise<void> {
  const mine = ++checkSeq;
  const [found, preflight] = await Promise.allSettled([upload_validate(props.provider.id, null), upload_preflight(props.provider.id)]);
  if (mine !== checkSeq) return;
  issues.value = found.status === "fulfilled" ? found.value : [];
  consent.value = preflight.status === "fulfilled" ? preflight.value.consent : null;
}

/** Another window (or the ShareX import) changed this host's settings. */
watch(
  () => JSON.stringify([bag.value, props.settings.upload.confirmedProviders]),
  () => {
    if (checkTimer !== undefined) clearTimeout(checkTimer);
    checkTimer = setTimeout(() => {
      checkTimer = undefined;
      void check();
    }, 300);
  },
);
onMounted(() => void check());
onBeforeUnmount(() => {
  if (checkTimer !== undefined) clearTimeout(checkTimer);
});

const keys = computed(() => new Set(props.provider.settings.map((f) => f.key)));
const issuesOf = (key: string) => issues.value.filter((issue) => issue.field === key);
/** Problems that belong to no row of the form, such as a link template that never reads the answer. */
const otherIssues = computed(() => issues.value.filter((issue) => !issue.field || !keys.value.has(issue.field)));
const blocked = computed(() => issues.value.some((issue) => issue.severity === "error"));

// ---------------------------------------------------------------------------- ShareX import
const sxcuInput = ref<HTMLInputElement | null>(null);
const sxcu = ref<{ ok: boolean; text: string; warnings: string[] } | null>(null);
/** Bumped after an import, so the rows ask the keychain about their secrets again. */
const epoch = ref(0);
/** A custom uploader is a few kilobytes; a bigger file is the wrong file. */
const SXCU_READ_LIMIT = 1024 * 1024;

async function importSxcu(event: Event): Promise<void> {
  const input = event.target as HTMLInputElement;
  const file = input.files?.[0];
  input.value = "";
  if (!file) return;
  sxcu.value = null;
  if (file.size > SXCU_READ_LIMIT) {
    sxcu.value = { ok: false, text: tk("upload.sxcu.tooLarge"), warnings: [] };
    return;
  }
  try {
    const outcome = await upload_import_sxcu(await file.text());
    if (outcome.refusal) {
      sxcu.value = { ok: false, text: issueText(outcome.refusal), warnings: [] };
    } else if (outcome.imported) {
      const named = outcome.imported.values.name;
      const name = typeof named === "string" && named.trim() ? named : file.name;
      sxcu.value = { ok: true, text: tk("upload.sxcu.imported", { name }), warnings: outcome.imported.warnings.map(issueText) };
      epoch.value += 1;
      void check();
    }
  } catch (error) {
    sxcu.value = { ok: false, text: describeError(error), warnings: [] };
  }
}

// ---------------------------------------------------------------------------------------- test
const fields = ref<InstanceType<typeof ProviderField>[]>([]);
const testing = ref(false);
const result = ref<{ ok: boolean; text: string; code: string | null; warnings: string[] } | null>(null);
/** The destination the test dialog asks about, while it is open. */
const testConsent = ref<ConsentInfo | null>(null);
/** The test sends a request, so Settings › General's switches keep it off like an upload. */
const uploadsOn = computed(() => props.settings.general.allowUploads !== false && !props.settings.general.offline);

async function test(): Promise<void> {
  if (!uploadsOn.value) return;
  for (const field of fields.value) field?.touch();
  result.value = null;
  if (fields.value.some((f) => f?.invalid())) {
    result.value = { ok: false, text: t("settings.upload.testFix"), code: null, warnings: [] };
    return;
  }
  testing.value = true;
  try {
    const preflight = await upload_preflight(props.provider.id);
    issues.value = preflight.issues;
    consent.value = preflight.consent;
    if (preflight.issues.some((issue) => issue.severity === "error")) {
      result.value = { ok: false, text: t("settings.upload.testFix"), code: null, warnings: [] };
      testing.value = false;
      return;
    }
    if (!preflight.consent.confirmed) {
      // The dialog decides; `testing` stays on until it does.
      testConsent.value = preflight.consent;
      return;
    }
    await runTest();
  } catch (error) {
    result.value = { ok: false, text: describeError(error), code: null, warnings: [] };
    testing.value = false;
  }
}

async function runTest(): Promise<void> {
  testing.value = true;
  try {
    const done = await upload_test(props.provider.id);
    result.value = {
      ok: true,
      text: tk("upload.test.ok", { name: props.provider.name, ms: Math.round(done.latencyMs) }),
      code: null,
      warnings: done.warnings.map(issueText),
    };
  } catch (error) {
    if (error instanceof ApiError && error.code === "provider") {
      result.value = { ok: false, text: t("settings.upload.testRefused", { name: props.provider.name }), code: error.message, warnings: [] };
    } else {
      result.value = { ok: false, text: describeError(error), code: null, warnings: [] };
    }
  } finally {
    testing.value = false;
    void check();
  }
}

function testConfirmed(): void {
  testConsent.value = null;
  void runTest();
}

function testCancelled(): void {
  testConsent.value = null;
  testing.value = false;
}

/** The settings point elsewhere by now: the dialog shows the new address and says why. */
async function testStale(): Promise<void> {
  try {
    testConsent.value = (await upload_preflight(props.provider.id)).consent;
  } catch (error) {
    testCancelled();
    result.value = { ok: false, text: describeError(error), code: null, warnings: [] };
  }
}

// ------------------------------------------------------------------------- first-upload question
/** Any confirmation recorded for this provider, the current destination's or an older one's. */
const anyConfirmation = computed(() =>
  props.settings.upload.confirmedProviders.some((key) => key === props.provider.id || key.startsWith(`${props.provider.id}@`)),
);
const consentHelp = computed(() => {
  const c = consent.value;
  if (!c) return t("settings.upload.notConfirmed", { name: props.provider.name });
  const destination = destinationOf(c);
  return c.confirmed ? t("settings.upload.confirmedFor", { destination }) : t("settings.upload.notConfirmedFor", { destination });
});

async function askAgain(): Promise<void> {
  try {
    await settings_revoke_consent(props.provider.id);
    await check();
  } catch (error) {
    banner.value = { tone: "danger", text: describeError(error) };
  }
}
</script>

<template>
  <div class="p-provider">
    <span class="p-mono" data-size="large" aria-hidden="true">{{ provider.monogram || provider.name.slice(0, 2) }}</span>
    <div class="p-provider-text">
      <div class="p-provider-name">{{ provider.name }}</div>
      <div v-if="provider.domain" class="p-provider-host">{{ provider.domain }}</div>
      <div v-else class="p-provider-host" data-kind="plain">{{ hostLine }}</div>
    </div>
    <span v-if="isDefault" class="g-badge" data-tone="iris"><GIcon name="check" :size="12" />{{ t("settings.upload.default") }}</span>
    <button v-else type="button" class="g-button" @click="makeDefault">{{ t("settings.upload.makeDefault") }}</button>
  </div>

  <template v-if="anonymous && terms.length">
    <SectionHead :title="t('settings.upload.termsTitle')" />
    <div class="g-group">
      <ul class="p-terms">
        <li v-for="(item, i) in terms" :key="i"><GIcon :name="item.icon" :size="16" /><span>{{ item.text }}</span></li>
        <li v-if="provider.facts?.termsUrl" class="p-terms-link p-flush">
          <button type="button" class="g-button" data-variant="plain" @click="open(provider.facts?.termsUrl)"><GIcon name="external-link" :size="16" />{{ t("settings.upload.readTerms") }}</button>
        </li>
      </ul>
    </div>
  </template>

  <template v-if="provider.shape === 'custom'">
    <SectionHead :title="t('settings.upload.headImport')" />
    <div class="g-group">
      <FormRow
        :label="t('settings.upload.importSxcu')"
        :help="sxcu ? sxcu.text : tp('settings.upload.importHelp')"
        :tone="sxcu ? (sxcu.ok ? 'ok' : 'danger') : null"
      >
        <template v-if="sxcu?.warnings.length" #after>
          <span v-for="(warning, i) in sxcu.warnings" :key="i" class="g-help" data-tone="warn"><GIcon name="warning" :size="14" />{{ warning }}</span>
        </template>
        <button type="button" class="g-button" @click="sxcuInput?.click()">{{ t("settings.upload.importChoose") }}</button>
        <input ref="sxcuInput" type="file" accept=".sxcu,application/json" hidden @change="importSxcu" />
      </FormRow>
    </div>
  </template>

  <template v-for="(group, index) in groups" :key="`${epoch}-${index}`">
    <SectionHead v-if="group.section || index === keyGroup" :title="group.section ? sectionOf(group.section) : t('settings.upload.headAccount')">
      <button v-if="index === keyGroup && provider.keyUrl" type="button" class="g-button" data-variant="plain" @click="open(provider.keyUrl)">
        <GIcon name="external-link" :size="16" />{{ keyLabel }}
      </button>
    </SectionHead>
    <div class="g-group">
      <ProviderField
        v-for="field in group.fields"
        :key="field.key"
        ref="fields"
        :provider-id="provider.id"
        :field="field"
        :value="bag[field.key]"
        :issues="issuesOf(field.key)"
        @change="(value) => setValue(field.key, value)"
        @secret-changed="check"
      />
    </div>
  </template>

  <div v-if="otherIssues.length" class="g-group">
    <FormRow v-for="(issue, i) in otherIssues" :key="i" :help="issueText(issue)" :tone="issue.severity === 'error' ? 'danger' : 'warn'" />
  </div>

  <template v-if="provider.testable">
    <div class="g-group p-test-group">
      <FormRow stacked>
        <div class="p-test">
          <button type="button" class="g-button" :disabled="testing || !uploadsOn" @click="test">
            <GIcon v-if="result && !result.ok" name="retry" :size="16" />{{ testing ? t("settings.upload.testing") : result && !result.ok ? t("settings.upload.testAgain") : t("settings.upload.test") }}
          </button>
          <span v-if="result?.ok" class="g-help" data-tone="ok" role="status"><GIcon name="check" :size="14" />{{ result.text }}</span>
          <span v-else-if="result" class="g-help" data-tone="danger" role="alert"
            ><GIcon name="bang" :size="14" /><span>{{ result.text }}<template v-if="result.code"> <span class="p-code">{{ result.code }}</span></template></span></span
          >
          <span v-else-if="!uploadsOn" class="g-help">{{ t(settings.general.offline ? "settings.upload.offlineNote" : "settings.upload.offNote") }}</span>
          <span v-else class="g-help">{{ blocked ? t("settings.upload.testBlocked") : t("settings.upload.testHelp") }}</span>
        </div>
        <span v-for="(warning, i) in result?.warnings ?? []" :key="i" class="g-help" data-tone="warn"><GIcon name="warning" :size="14" />{{ warning }}</span>
      </FormRow>
    </div>
  </template>

  <template v-if="!anonymous && terms.length">
    <SectionHead :title="t('settings.upload.termsTitle')" />
    <div class="g-group">
      <ul class="p-terms">
        <li v-for="(item, i) in terms" :key="i"><GIcon :name="item.icon" :size="16" /><span>{{ item.text }}</span></li>
        <li v-if="provider.facts?.termsUrl" class="p-terms-link p-flush">
          <button type="button" class="g-button" data-variant="plain" @click="open(provider.facts?.termsUrl)"><GIcon name="external-link" :size="16" />{{ t("settings.upload.readTerms") }}</button>
        </li>
      </ul>
    </div>
  </template>

  <SectionHead :title="t('settings.upload.headConsent')" />
  <div class="g-group">
    <FormRow :label="t('settings.upload.firstUpload')" :help="consentHelp">
      <button v-if="anyConfirmation" type="button" class="g-button" @click="askAgain">{{ t("settings.upload.askAgain") }}</button>
    </FormRow>
  </div>

  <div v-if="provider.website" class="p-foot">
    <button type="button" class="g-button" data-variant="plain" @click="open(provider.website)"><GIcon name="external-link" :size="16" />{{ t("settings.upload.website", { name: provider.name }) }}</button>
  </div>

  <Teleport to="body">
    <FirstUploadDialog
      v-if="testConsent"
      :consent="testConsent"
      purpose="test"
      :test="testKind(provider.shape)"
      :can-choose-host="false"
      @confirmed="testConfirmed"
      @cancel="testCancelled"
      @stale="testStale"
    />
  </Teleport>
</template>
