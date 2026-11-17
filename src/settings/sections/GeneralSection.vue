<script setup lang="ts">
// Settings › General: language and appearance, the menu bar, what a capture does, and updates.
import { computed } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import SegmentedControl from "../../components/SegmentedControl.vue";
import SelectControl from "../../components/SelectControl.vue";
import SwitchControl from "../../components/SwitchControl.vue";
import { app_open_system_settings } from "../../lib/api/windows";
import { LANGUAGES, t } from "../../lib/i18n";
import type { AfterSelection, Appearance, IconClick, Language, Settings } from "../../lib/settings";
import { update, type Banner } from "../store";
import UpdateRow from "../UpdateRow.vue";
import { uid } from "../text";

const props = defineProps<{ settings: Settings }>();
const g = computed(() => props.settings.general);


const ids = { language: uid("lang"), appearance: uid("app"), login: uid("login"), icon: uid("icon"), click: uid("click"), after: uid("after"), pointer: uid("ptr"), delay: uid("delay"), shutter: uid("shut"), updates: uid("upd") };

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
  text: t("settings.error.login"),
  action: { label: t("settings.general.openLoginItems"), run: () => void app_open_system_settings("loginItems").catch(() => {}) },
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

  <SectionHead :title="t('settings.general.headMenuBar')" />
  <div class="g-group">
    <FormRow :label="t('settings.general.startAtLogin')" :for="ids.login" :help="t('settings.general.startAtLoginHelp')">
      <SwitchControl :id="ids.login" :model-value="g.startAtLogin" @update:model-value="(v) => update((s) => (s.general.startAtLogin = v), loginRefused)" />
    </FormRow>
    <FormRow :label="t('settings.general.showIcon')" :for="ids.icon" :help="t('settings.general.showIconHelp')">
      <SwitchControl :id="ids.icon" :model-value="g.showMenuBarIcon" @update:model-value="(v) => update((s) => (s.general.showMenuBarIcon = v))" />
    </FormRow>
    <FormRow :label="t('settings.general.iconClick')" :for="ids.click" sub :disabled="!g.showMenuBarIcon">
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
    <FormRow :label="t('settings.general.shutter')" :for="ids.shutter">
      <SwitchControl :id="ids.shutter" :model-value="g.shutterSound" @update:model-value="(v) => update((s) => (s.general.shutterSound = v))" />
    </FormRow>
  </div>

  <SectionHead :title="t('settings.general.headUpdates')" />
  <div class="g-group">
    <FormRow :label="t('settings.general.checkUpdates')" :for="ids.updates" :help="t('settings.general.checkUpdatesHelp')">
      <SwitchControl :id="ids.updates" :model-value="g.checkForUpdates" @update:model-value="(v) => update((s) => (s.general.checkForUpdates = v))" />
    </FormRow>
    <UpdateRow />
  </div>
  <p class="p-note"><GIcon name="shield" :size="16" /><span>{{ t("settings.general.privacy") }}</span></p>
</template>
