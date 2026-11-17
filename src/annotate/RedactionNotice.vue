<script setup lang="ts">
// The first-use notice under the options strip (windows ab10): the first time blur or pixelate is
// chosen, one sentence says that the effect is flattened on export. Got it dismisses it for good.
import GIcon from "../components/GIcon.vue";
import { t } from "../lib/i18n";

defineEmits<{ dismiss: [] }>();
</script>

<template>
  <div class="g-glass a-notice" role="note" @mousedown.stop>
    <GIcon name="shield" :size="16" />
    <p>{{ t("annotate.redactionNotice") }}</p>
    <button type="button" class="g-button" @click="$emit('dismiss')">{{ t("annotate.gotIt") }}</button>
  </div>
</template>

<style scoped>
.a-notice {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  width: 404px;
  padding: 10px 6px 10px 12px;
  border-radius: var(--radius-card);
}
.a-notice > g-icon {
  flex: none;
  margin-top: 1px;
  color: var(--iris-text);
}
.a-notice > p {
  flex: 1;
  margin: 0;
  font: var(--type-body);
  color: var(--text);
}
.a-notice > .g-button {
  align-self: center;
}
</style>
