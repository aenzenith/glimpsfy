<script setup lang="ts">
// The unsaved-changes sheet (windows ab10): it hangs under the toolbar and asks once. Don't save
// is red on the left, Cancel and Save follow, and Save is the default, so Return never throws
// work away. Esc cancels. When the next image is already on its way, there is nothing to cancel
// and the sheet offers the two other choices only.
import { nextTick, onMounted, ref } from "vue";

import { t } from "../lib/i18n";

const props = defineProps<{ title: string; text: string; cancellable: boolean; busy: boolean }>();
const emit = defineEmits<{ save: []; discard: []; cancel: [] }>();

const sheet = ref<HTMLElement | null>(null);
const save = ref<HTMLButtonElement | null>(null);
const id = `e-sheet-${Math.random().toString(36).slice(2)}`;

onMounted(() => nextTick(() => save.value?.focus()));

function key(e: KeyboardEvent): void {
  e.stopPropagation();
  if (e.key === "Escape" && props.cancellable && !props.busy) {
    e.preventDefault();
    emit("cancel");
    return;
  }
  if (e.key !== "Tab" || !sheet.value) return;
  e.preventDefault();
  const buttons = [...sheet.value.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
  const at = buttons.indexOf(document.activeElement as HTMLButtonElement);
  buttons[(at + (e.shiftKey ? -1 : 1) + buttons.length) % buttons.length]?.focus();
}
</script>

<template>
  <div class="e-sheet-host" @keydown="key">
    <div ref="sheet" class="e-sheet" role="alertdialog" aria-modal="true" :aria-labelledby="`${id}-t`" :aria-describedby="`${id}-d`">
      <div class="e-sheet-title" :id="`${id}-t`">{{ title }}</div>
      <p class="e-sheet-text" :id="`${id}-d`">{{ text }}</p>
      <div class="e-sheet-foot">
        <button type="button" class="g-button e-left" data-variant="destructive" :disabled="busy" @click="emit('discard')">
          {{ t("editor.unsaved.dontSave") }}
        </button>
        <button v-if="cancellable" type="button" class="g-button" :disabled="busy" @click="emit('cancel')">
          {{ t("editor.unsaved.cancel") }}
        </button>
        <button ref="save" type="button" class="g-button" data-variant="primary" :disabled="busy" @click="emit('save')">
          {{ t("editor.unsaved.save") }}
        </button>
      </div>
    </div>
  </div>
</template>

<style scoped>
.e-sheet-host {
  position: absolute;
  inset: var(--toolbar-h) 0 0 0;
  z-index: 30;
  display: flex;
  justify-content: center;
  align-items: flex-start;
}
.e-sheet {
  width: 520px;
  margin-top: 10px;
  padding: 20px;
  border-radius: var(--radius-window);
  background: var(--win-raised);
  box-shadow: var(--win-rim), var(--win-shadow-popover);
  color: var(--win-text);
}
.e-sheet-title {
  font: var(--type-heading);
}
.e-sheet-text {
  margin: 4px 0 0;
  font: var(--type-text);
  color: var(--win-text-2);
}
.e-sheet-foot {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
  margin-top: 18px;
}
.e-sheet-foot > .e-left {
  margin-right: auto;
}
</style>
