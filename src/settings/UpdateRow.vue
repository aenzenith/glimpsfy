<script setup lang="ts">
// The update row of Settings › General and Settings › About: the installed version, what the
// updater is doing, and the one action that fits it (check now, restart to update, or open the
// releases page for a copy that cannot replace itself). While Work offline is on the row is grey
// and Check now waits; an update downloaded before still installs, which needs no connection.
import { computed } from "vue";

import FormRow from "../components/FormRow.vue";
import { app_open_url } from "../lib/api/windows";
import { RELEASES_URL, update_check, update_install } from "../lib/api/update";
import { t } from "../lib/i18n";
import { online } from "../lib/network";
import { describeUpdate, useUpdate } from "../lib/update";
import { banner } from "./store";
import { describeError } from "./text";

const view = useUpdate();

const label = computed(() => (view.value ? t("settings.general.versionRow", { version: view.value.current }) : "Glimpsfy"));
const help = computed(() => {
  if (!view.value) return null;
  if (!online.value && view.value.status !== "ready" && view.value.status !== "off") return t("settings.common.offlineNote");
  return describeUpdate(view.value);
});
const tone = computed(() => (view.value?.status === "failed" ? "warn" : view.value?.status === "ready" ? "ok" : null));

function fail(error: unknown): void {
  banner.value = { tone: "danger", text: describeError(error) };
}

function check(): void {
  if (!online.value) return;
  void update_check().catch(fail);
}

function restart(): void {
  void update_install().catch(fail);
}

/** The release the core named, which is where this copy's update waits, or the latest one. */
function releases(): void {
  void app_open_url(view.value?.releaseUrl ?? RELEASES_URL).catch(fail);
}
</script>

<template>
  <FormRow :label="label" :help="help" :tone="tone" :disabled="!online && view?.status !== 'ready'">
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
        :disabled="!online || view.status === 'checking' || view.status === 'downloading' || view.status === 'installing'"
        @click="check"
      >
        {{ view.status === "checking" ? t("update.action.checking") : t("update.action.check") }}
      </button>
    </template>
  </FormRow>
</template>
