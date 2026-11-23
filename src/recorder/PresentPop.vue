<script setup lang="ts">
// The Presentation card under the control bar's Sunum button (design `presentPop`, B19 Y2): Draw
// on screen with its keys, Spotlight as a checked item with its keys, and the switch that makes
// drawings fade 3 s after the last stroke. It only draws and reports; `Bar.vue` acts.
import GIcon from "../components/GIcon.vue";
import KeyCaps from "../components/KeyCaps.vue";
import SwitchControl from "../components/SwitchControl.vue";
import { t } from "../lib/i18n";

defineProps<{
  spotlight: boolean;
  fade: boolean;
  /** The two actions' accelerators from Settings › Shortcuts; `null` has no key. */
  keys: { draw: string | null; spotlight: string | null };
}>();
const emit = defineEmits<{ draw: []; spotlight: []; fade: [on: boolean] }>();
</script>

<template>
  <div class="g-pop g-glass r-present" data-material="strong" role="menu" :aria-label="t('record.present')">
    <button type="button" class="g-pop-item" role="menuitem" data-id="present-draw" @click="emit('draw')">
      <GIcon name="pen" :size="16" /><span class="g-pop-name">{{ t("record.present.draw") }}</span>
      <KeyCaps :accelerator="keys.draw" />
    </button>
    <button
      type="button"
      class="g-pop-item"
      role="menuitemcheckbox"
      data-id="present-spotlight"
      :aria-checked="spotlight"
      @click="emit('spotlight')"
    >
      <GIcon name="spotlight" :size="16" /><span class="g-pop-name">{{ t("tool.spotlight") }}</span>
      <KeyCaps :accelerator="keys.spotlight" />
      <GIcon name="check" :size="16" class="g-pop-check" />
    </button>
    <div class="g-pop-sep" role="separator" />
    <label class="g-pop-row">
      <span class="g-pop-label"><GIcon name="timer" :size="16" />{{ t("record.present.fade") }}</span>
      <SwitchControl :model-value="fade" @update:model-value="emit('fade', $event)" />
    </label>
  </div>
</template>

<style>
.r-present { --pop-w: max-content; max-width: 340px; }
.r-present .g-pop-item > .g-keys { margin-left: auto; }
.r-present .g-pop-item > .g-pop-check { margin-left: 2px; }
.r-present .g-pop-name { white-space: normal; padding: 9px 0; line-height: 16px; }
.r-present .g-pop-row .g-pop-label { white-space: normal; }
</style>
