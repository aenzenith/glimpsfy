<script setup lang="ts">
// The page of a window whose milestone has not shipped yet: its name, in the kit's empty state.
import { t } from "../lib/i18n";
import GIcon from "./GIcon.vue";

defineProps<{ title: string; icon: string }>();
</script>

<template>
  <main class="placeholder">
    <div class="g-empty">
      <div class="g-empty-icon"><GIcon :name="icon" :size="32" /></div>
      <h1 class="g-empty-title">{{ title }}</h1>
      <p class="g-empty-text">{{ t("window.placeholder") }}</p>
    </div>
  </main>
</template>

<style scoped>
.placeholder {
  display: grid;
  place-items: center;
  height: 100%;
}
</style>
