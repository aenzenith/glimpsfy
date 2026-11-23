<script setup lang="ts">
// Settings › General: language and appearance, the menu bar (the tray elsewhere), what a capture
// does, the network switches and updates. A row this platform cannot honour (the icon's click
// where the tray reports none, the shutter sound where the system has none) is left out. Work
// offline wins over Allow uploads, so that switch shows off and greyed while the app is offline.
import { computed } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import SegmentedControl from "../../components/SegmentedControl.vue";
import SelectControl from "../../components/SelectControl.vue";
import SwitchControl from "../../components/SwitchControl.vue";
import { app_open_system_settings } from "../../lib/api/windows";
import { LANGUAGES, t, tp } from "../../lib/i18n";
import { can } from "../../lib/platform";
import type { AfterSelection, Appearance, IconClick, Language, Settings } from "../../lib/settings";
import { update, type Banner } from "../store";
import UpdateRow from "../UpdateRow.vue";
import { uid } from "../text";

const props = defineProps<{ settings: Settings }>();
const g = computed(() => props.settings.general);


const ids = { language: uid("lang"), appearance: uid("app"), login: uid("login"), icon: uid("icon"), click: uid("click"), after: uid("after"), pointer: uid("ptr"), delay: uid("delay"), shutter: uid("shut"), updates: uid("upd"), uploads: uid("uploads"), offline: uid("offline") };

const languages = computed(() => [
  { value: "system" as Language, label: t("settings.common.system") },
  ...LANGUAGES.map((l) => ({ value: l.code as Language, label: l.name })),
]);
const appearances = computed(() => [
  { value: "system" as Appearance, label: t("settings.common.system") },
  { value: "light" as Appearance, label: t("settings.general.light") },
  { value: "dark" as Appearance, label: t("settings.general.dark") },
]);
const clicks = computed(() => [
  { value: "openMenu" as IconClick, label: t("settings.general.iconOpenMenu") },
  { value: "captureArea" as IconClick, label: t("settings.general.iconCapture") },
]);
const afters = computed(() => [
  { value: "annotate" as AfterSelection, label: t("settings.general.afterAnnotate") },
  { value: "copyAndClose" as AfterSelection, label: t("settings.general.afterCopy") },
  { value: "saveAndClose" as AfterSelection, label: t("settings.general.afterSave") },
]);
const delays = computed(() => [3, 5, 10].map((n) => ({ value: n, label: t("settings.common.seconds", { n }) })));

const loginRefused = (): Banner => ({
  tone: "danger",
  text: tp("settings.error.login"),
  action: can("systemSettingsPanes")
    ? { label: tp("settings.general.openLoginItems"), run: () => void app_open_system_settings("loginItems").catch(() => {}) }
    : undefined,
});
</script>

<template>
  <SectionHead :title="t('settings.general.headLanguage')" />
  <div class="g-group">
    <FormRow :label="t('settings.general.language')" :for="ids.language">
      <SelectControl :id="ids.language" :model-value="g.language" :options="languages" @update:model-value="(v) => update((s) => (s.general.language = v))" />
    </FormRow>
    <FormRow :label="t('settings.general.appearance')" :label-id="ids.appearance">
      <SegmentedControl :labelledby="ids.appearance" :model-value="g.appearance" :options="appearances" @update:model-value="(v) => update((s) => (s.general.appearance = v))" />
    </FormRow>
  </div>

  <SectionHead :title="tp('settings.general.headMenuBar')" />
  <div class="g-group">
    <FormRow :label="t('settings.general.startAtLogin')" :for="ids.login" :help="tp('settings.general.startAtLoginHelp')">
      <SwitchControl :id="ids.login" :model-value="g.startAtLogin" @update:model-value="(v) => update((s) => (s.general.startAtLogin = v), loginRefused)" />
    </FormRow>
    <FormRow :label="tp('settings.general.showIcon')" :for="ids.icon" :help="tp('settings.general.showIconHelp')">
      <SwitchControl :id="ids.icon" :model-value="g.showMenuBarIcon" @update:model-value="(v) => update((s) => (s.general.showMenuBarIcon = v))" />
    </FormRow>
    <FormRow v-if="can('trayPopover')" :label="t('settings.general.iconClick')" :for="ids.click" sub :disabled="!g.showMenuBarIcon">
      <SelectControl :id="ids.click" :disabled="!g.showMenuBarIcon" :model-value="g.iconClick" :options="clicks" @update:model-value="(v) => update((s) => (s.general.iconClick = v))" />
    </FormRow>
  </div>

  <SectionHead :title="t('settings.general.headCapture')" />
  <div class="g-group">
    <FormRow :label="t('settings.general.afterSelection')" :label-id="ids.after" top>
      <div class="g-choices" role="radiogroup" :aria-labelledby="ids.after">
        <label v-for="option in afters" :key="option.value" class="g-choice">
          <input type="radio" class="g-radio" :name="ids.after" :checked="g.afterSelection === option.value" @change="update((s) => (s.general.afterSelection = option.value))" />{{ option.label }}
        </label>
      </div>
    </FormRow>
    <FormRow :label="t('settings.general.pointer')" :for="ids.pointer">
      <SwitchControl :id="ids.pointer" :model-value="g.includePointer" @update:model-value="(v) => update((s) => (s.general.includePointer = v))" />
    </FormRow>
    <FormRow :label="t('settings.general.delay')" :label-id="ids.delay" :help="t('settings.general.delayHelp')">
      <SegmentedControl :labelledby="ids.delay" :model-value="g.delaySeconds" :options="delays" @update:model-value="(v) => update((s) => (s.general.delaySeconds = v))" />
    </FormRow>
    <FormRow v-if="can('shutterSound')" :label="t('settings.general.shutter')" :for="ids.shutter">
      <SwitchControl :id="ids.shutter" :model-value="g.shutterSound" @update:model-value="(v) => update((s) => (s.general.shutterSound = v))" />
    </FormRow>
  </div>

  <SectionHead :title="t('settings.general.headNetwork')" />
  <div class="g-group">
    <FormRow :label="t('settings.general.allowUploads')" :for="ids.uploads" :help="g.offline ? t('settings.common.offlineNote') : t('settings.general.allowUploadsHelp')" :disabled="g.offline">
      <SwitchControl :id="ids.uploads" :disabled="g.offline" :model-value="g.allowUploads && !g.offline" @update:model-value="(v) => update((s) => (s.general.allowUploads = v))" />
    </FormRow>
    <FormRow :label="t('settings.general.offline')" :for="ids.offline" :help="t('settings.general.offlineHelp')">
      <SwitchControl :id="ids.offline" :model-value="g.offline" @update:model-value="(v) => update((s) => (s.general.offline = v))" />
    </FormRow>
  </div>

  <SectionHead :title="t('settings.general.headUpdates')" />
  <div class="g-group">
    <FormRow :label="t('settings.general.checkUpdates')" :for="ids.updates" :help="g.offline ? t('settings.common.offlineNote') : t('settings.general.checkUpdatesHelp')" :disabled="g.offline">
      <SwitchControl :id="ids.updates" :disabled="g.offline" :model-value="g.checkForUpdates && !g.offline" @update:model-value="(v) => update((s) => (s.general.checkForUpdates = v))" />
    </FormRow>
    <UpdateRow />
  </div>
  <p class="p-note"><GIcon name="shield" :size="16" /><span>{{ t("settings.general.privacy") }}</span></p>
</template>
