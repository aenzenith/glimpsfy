<script setup lang="ts">
// The update row of Settings › General and Settings › About: the installed version, what the
// updater is doing, and the one action that fits it (check now, restart to update, or open the
// releases page for a copy that cannot replace itself).
import { computed } from "vue";

import FormRow from "../components/FormRow.vue";
import { app_open_url } from "../lib/api/windows";
import { RELEASES_URL, update_check, update_install } from "../lib/api/update";
import { t } from "../lib/i18n";
import { describeUpdate, useUpdate } from "../lib/update";
import { banner } from "./store";
import { describeError } from "./text";

const view = useUpdate();

const label = computed(() => (view.value ? t("settings.general.versionRow", { version: view.value.current }) : "Glimpsfy"));
const help = computed(() => (view.value ? describeUpdate(view.value) : null));
const tone = computed(() => (view.value?.status === "failed" ? "warn" : view.value?.status === "ready" ? "ok" : null));

function fail(error: unknown): void {
  banner.value = { tone: "danger", text: describeError(error) };
}

function check(): void {
  void update_check().catch(fail);
}

function restart(): void {
  void update_install().catch(fail);
}

function releases(): void {
  void app_open_url(RELEASES_URL).catch(fail);
}
</script>

<template>
  <FormRow :label="label" :help="help" :tone="tone">
    <template v-if="view && view.status !== 'off'">
      <button v-if="view.status === 'ready'" type="button" class="g-button" data-variant="primary" @click="restart">
        {{ t("update.action.restart") }}
      </button>
      <button v-else-if="view.status === 'available'" type="button" class="g-button" @click="releases">
        {{ t("update.action.download") }}
      </button>
      <button
        v-else
        type="button"
        class="g-button"
        :disabled="view.status === 'checking' || view.status === 'downloading' || view.status === 'installing'"
        @click="check"
      >
        {{ view.status === "checking" ? t("update.action.checking") : t("update.action.check") }}
      </button>
    </template>
  </FormRow>
</template>
