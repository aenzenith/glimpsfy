<script setup lang="ts">
// Settings › Upload: the hosts, the empty state until one is the default, and what happens after
// an upload. A host opens its own page (ProviderPage).
import { computed, onMounted, reactive, ref, watch } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import SegmentedControl from "../../components/SegmentedControl.vue";
import SwitchControl from "../../components/SwitchControl.vue";
import { upload_providers, upload_validate } from "../../lib/api/upload";
import { t, tp } from "../../lib/i18n";
import type { CopyAfterUpload, Settings } from "../../lib/settings";
import { update } from "../store";
import { describeError, uid } from "../text";
import { summaryOf, type Provider } from "../upload";

const props = defineProps<{ settings: Settings }>();
const emit = defineEmits<{ open: [provider: Provider] }>();

const u = computed(() => props.settings.upload);
/** Settings › General's switches: with uploads off or the app offline nothing reaches these hosts. */
const uploadsOn = computed(() => props.settings.general.allowUploads !== false && !props.settings.general.offline);
const ids = { copy: uid("copy"), open: uid("open") };

const providers = ref<Provider[] | null>(null);
const loadError = ref<string | null>(null);

onMounted(async () => {
  try {
    providers.value = await upload_providers();
  } catch (error) {
    loadError.value = describeError(error);
  }
});

/** Hosts whose saved settings stop an upload (a missing key, say), from `upload_validate`. */
const unready = reactive(new Set<string>());
async function checkHosts(): Promise<void> {
  for (const provider of providers.value ?? []) {
    try {
      const issues = await upload_validate(provider.id, null);
      if (issues.some((issue) => issue.severity === "error")) unready.add(provider.id);
      else unready.delete(provider.id);
    } catch {
      unready.delete(provider.id);
    }
  }
}
watch(() => [providers.value, JSON.stringify(u.value.providers)], () => void checkHosts(), { immediate: true });

const hasDefault = computed(() => !!u.value.defaultProvider && !!providers.value?.some((p) => p.id === u.value.defaultProvider));

/** The default host carries Default; a host that the default would fail with, or that the user
 * has started to fill in, says that it needs setting up. An untouched host carries nothing. */
function badge(provider: Provider): { text: string; tone: string | null; check: boolean } | null {
  const needsSetup = unready.has(provider.id) && (provider.id === u.value.defaultProvider || provider.id in u.value.providers);
  if (needsSetup) return { text: t("settings.upload.needsSetup"), tone: "warn", check: false };
  if (provider.id === u.value.defaultProvider) return { text: t("settings.upload.default"), tone: "iris", check: true };
  return null;
}

const copies = computed(() => [
  { value: "link" as CopyAfterUpload, label: t("settings.upload.copyLink") },
  { value: "markdown" as CopyAfterUpload, label: "Markdown" },
  { value: "nothing" as CopyAfterUpload, label: t("settings.upload.copyNothing") },
]);
</script>

<template>
  <div v-if="!uploadsOn" class="g-group">
    <FormRow :help="t(settings.general.offline ? 'settings.upload.offlineNote' : 'settings.upload.offNote')" tone="warn" />
  </div>

  <template v-if="loadError">
    <div class="g-group">
      <div class="g-empty">
        <div class="g-empty-icon"><GIcon name="cloud" :size="32" /></div>
        <div class="g-empty-title">{{ t("settings.upload.loadFailedTitle") }}</div>
        <p class="g-empty-text">{{ loadError }}</p>
      </div>
    </div>
  </template>

  <template v-else-if="providers && !providers.length">
    <div class="g-group">
      <div class="g-empty">
        <div class="g-empty-icon"><GIcon name="cloud" :size="32" /></div>
        <div class="g-empty-title">{{ t("settings.upload.noHostsTitle") }}</div>
        <p class="g-empty-text">{{ tp("settings.upload.noHostsText") }}</p>
      </div>
    </div>
  </template>

  <template v-else-if="providers">
    <div v-if="!hasDefault" class="g-group">
      <div class="g-empty">
        <div class="g-empty-icon"><GIcon name="cloud" :size="32" /></div>
        <div class="g-empty-title">{{ t("settings.upload.emptyTitle") }}</div>
        <p class="g-empty-text">{{ tp("settings.upload.emptyText") }}</p>
      </div>
    </div>
    <SectionHead :title="t('settings.upload.hosts')" :desc="hasDefault ? t('settings.upload.hostsDesc') : t('settings.upload.pickDesc')" />
    <div class="g-group">
      <div class="g-list p-list">
        <button v-for="provider in providers" :key="provider.id" type="button" class="g-row" @click="emit('open', provider)">
          <span class="p-mono" aria-hidden="true">{{ provider.monogram || provider.name.slice(0, 2) }}</span>
          <span class="g-row-text">
            <span class="g-row-title">{{ provider.name }}</span>
            <span class="g-row-sub">{{ summaryOf(provider) }}</span>
          </span>
          <span class="g-row-trail">
            <span v-if="badge(provider)" class="g-badge" :data-tone="badge(provider)?.tone ?? undefined"
              ><GIcon v-if="badge(provider)?.check" name="check" :size="12" />{{ badge(provider)?.text }}</span
            >
            <GIcon name="chevron-right" :size="16" class="p-chev" />
          </span>
        </button>
      </div>
    </div>

    <template v-if="hasDefault">
      <SectionHead :title="t('settings.upload.after')" />
      <div class="g-group">
        <FormRow :label="t('settings.upload.copy')" :label-id="ids.copy" :help="t('settings.upload.copyHelp')">
          <SegmentedControl :labelledby="ids.copy" :model-value="u.copyAfterUpload" :options="copies" @update:model-value="(v) => update((s) => (s.upload.copyAfterUpload = v))" />
        </FormRow>
        <FormRow :label="t('settings.upload.openInBrowser')" :for="ids.open">
          <SwitchControl :id="ids.open" :model-value="u.openInBrowser" @update:model-value="(v) => update((s) => (s.upload.openInBrowser = v))" />
        </FormRow>
      </div>
    </template>
  </template>

  <template v-else>
    <p class="p-note"><span>{{ t("settings.upload.loading") }}</span></p>
  </template>
</template>
