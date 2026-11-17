<script setup lang="ts">
// A confirmation over the whole window (`g-scrim` and `g-dialog`), for an action that cannot be
// undone. It is modal: focus starts on Cancel, Tab stays among its buttons, and Esc cancels. The
// window's root must be positioned, because the scrim covers its nearest positioned ancestor.
import { nextTick, onMounted, ref } from "vue";

import GIcon from "./GIcon.vue";

const props = defineProps<{
  title: string;
  text: string;
  icon?: string;
  tone?: "neutral" | "warn" | "danger";
  confirmLabel: string;
  cancelLabel: string;
  destructive?: boolean;
  busy?: boolean;
}>();
const emit = defineEmits<{ confirm: []; cancel: [] }>();

const dialog = ref<HTMLElement | null>(null);
const cancel = ref<HTMLButtonElement | null>(null);
const titleId = `c-dialog-title-${Math.random().toString(36).slice(2)}`;
const textId = `${titleId}-text`;

onMounted(() => nextTick(() => cancel.value?.focus()));

function key(event: KeyboardEvent): void {
  if (event.key === "Escape") {
    event.preventDefault();
    if (!props.busy) emit("cancel");
    return;
  }
  if (event.key !== "Tab" || !dialog.value) return;
  event.preventDefault();
  const buttons = [...dialog.value.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
  const index = buttons.indexOf(document.activeElement as HTMLButtonElement);
  buttons[(index + (event.shiftKey ? -1 : 1) + buttons.length) % buttons.length]?.focus();
}
</script>

<template>
  <div class="g-scrim" @keydown="key">
    <div ref="dialog" class="g-dialog" role="alertdialog" aria-modal="true" :aria-labelledby="titleId" :aria-describedby="textId">
      <div class="g-dialog-head">
        <span class="g-dialog-icon" :data-tone="tone"><GIcon :name="icon ?? 'warning'" :size="24" /></span>
        <div>
          <h2 :id="titleId" class="g-dialog-title">{{ title }}</h2>
          <p :id="textId" class="g-dialog-text">{{ text }}</p>
        </div>
      </div>
      <slot />
      <div class="g-dialog-foot">
        <button ref="cancel" type="button" class="g-button" data-variant="primary" :disabled="busy" @click="emit('cancel')">
          {{ cancelLabel }}
        </button>
        <button
          type="button"
          class="g-button"
          :data-variant="destructive ? 'destructive' : undefined"
          :disabled="busy"
          @click="emit('confirm')"
        >
          {{ confirmLabel }}
        </button>
      </div>
    </div>
  </div>
</template>
