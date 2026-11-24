<script setup lang="ts">
// The foot of the settings sidebar: the installed version and the update action from every
// section, so a check no longer needs a trip to About. It follows the same state and offers the
// same action as UpdateRow; the sentence under the version is that state's own, which already says
// the update installs on a restart, so the button takes the shorter "Restart now".
import { computed } from "vue";

import { app_open_url } from "../lib/api/windows";
import { RELEASES_URL, update_check, update_install } from "../lib/api/update";
import { t } from "../lib/i18n";
import { online } from "../lib/network";
import { describeUpdate, useUpdate } from "../lib/update";
import { banner } from "./store";
import { describeError } from "./text";

const view = useUpdate();

const status = computed(() => {
  if (!view.value) return null;
  if (!online.value && view.value.status !== "ready" && view.value.status !== "off") return t("settings.common.offlineNote");
  return describeUpdate(view.value);
});
const tone = computed(() => (view.value?.status === "failed" ? "warn" : view.value?.status === "ready" ? "ok" : null));
const busy = computed(() => ["checking", "downloading", "installing"].includes(view.value?.status ?? ""));

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

function releases(): void {
  void app_open_url(view.value?.releaseUrl ?? RELEASES_URL).catch(fail);
}
</script>

<template>
  <footer v-if="view" class="p-side-foot">
    <span class="p-side-version">{{ t("settings.general.versionRow", { version: view.current }) }}</span>
    <span v-if="status" class="p-side-status" :data-tone="tone || undefined" aria-live="polite">{{ status }}</span>
    <template v-if="view.status !== 'off'">
      <button v-if="view.status === 'ready'" type="button" class="g-button" data-variant="primary" @click="restart">
        {{ t("update.ready.now") }}
      </button>
      <button v-else-if="view.status === 'available'" type="button" class="g-button" @click="releases">
        {{ t("update.action.download") }}
      </button>
      <button v-else type="button" class="g-button" :disabled="!online || busy" @click="check">
        {{ view.status === "checking" ? t("update.action.checking") : t("update.action.check") }}
      </button>
    </template>
  </footer>
</template>

<style>
/* `contain: inline-size` keeps the foot's sentences out of the sidebar's width, which follows the
   longest section name alone; the sentences wrap inside whatever width that gives. */
.p-side-foot {
  display: flex; flex-direction: column; align-items: flex-start; gap: 2px; flex: none; margin-top: auto;
  padding: 12px 18px 14px; contain: inline-size; box-shadow: inset 0 var(--hairline) 0 var(--win-line);
}
.p-side-version { font: var(--type-caption-strong); color: var(--win-text-2); }
.p-side-status { font: var(--type-caption); color: var(--win-text-3); }
.p-side-status[data-tone="warn"] { color: var(--win-warn); }
.p-side-status[data-tone="ok"] { color: var(--win-ok); }
/* A label longer than the sidebar wraps inside the button rather than spilling past its edge. */
.p-side-foot > .g-button { max-width: 100%; height: auto; min-height: var(--control-h); margin-top: 8px; padding-top: 4px; padding-bottom: 4px; white-space: normal; text-align: center; }
</style>
