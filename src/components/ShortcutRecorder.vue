<script setup lang="ts">
// The kit's shortcut recorder (`g-shortcut`). A click starts listening; held modifiers show as caps
// with a dashed slot for the key; the recorder's rule (`recorderStep` in src/lib/keys.ts) refuses
// a key that would stop typing in other apps. Esc and Tab cancel, ⌫ clears, and a click elsewhere
// cancels. While it listens, Glimpsfy's own global shortcuts are off (`settings_pause_shortcuts`),
// so their keys reach the recorder instead of starting a capture. The caps follow the platform
// (`capsOfModifiers`): ⌘ ⌥ ⇧ ⌃ on macOS, Ctrl, Alt, Shift and Win or Super elsewhere.
//
// The recorder only reports what was typed (`captured`); the row decides what a clash with another
// action or with macOS means, and shows it through `pending` and `warn`.
import { computed, onBeforeUnmount, ref } from "vue";

import { settings_pause_shortcuts } from "../lib/api/settings";
import { t } from "../lib/i18n";
import { capsOf, capsOfModifiers, joinCaps, modifiersOf, recorderStep } from "../lib/keys";
import GIcon from "./GIcon.vue";

const props = defineProps<{
  accelerator: string | null;
  /** The action's name, for the labels a screen reader speaks. */
  name: string;
  describedby?: string;
  /** Keys typed but not saved yet (a clash waits for Move here or Cancel). */
  pending?: readonly string[] | null;
  /** The keys clash with another action or with macOS. */
  warn?: boolean;
}>();
const emit = defineEmits<{
  captured: [accelerator: string];
  cleared: [];
  cancelled: [];
  refused: [reason: "needsModifier" | "commandAlone", caps: string[]];
  listening: [listening: boolean];
}>();

const root = ref<HTMLElement | null>(null);
const keysButton = ref<HTMLButtonElement | null>(null);
const listening = ref(false);
const held = ref<string[]>([]);
/** Modifiers are held and no key yet, so a dashed slot waits for it. */
const slot = ref(false);
/** Print Screen's key-down reached the page since listening began. */
let printScreenDown = false;

const caps = computed(() => (listening.value ? held.value : (props.pending ?? capsOf(props.accelerator))));
const state = computed(() =>
  listening.value ? "recording" : props.pending || props.warn ? "conflict" : props.accelerator ? "set" : "empty",
);
/** While modifiers are held without a key, a dashed slot waits for it. */
const waiting = computed(() => listening.value && held.value.length > 0 && slot.value);
const aria = computed(() => {
  if (listening.value) return t("settings.recorder.ariaListening", { name: props.name });
  if (!caps.value.length) return t("settings.recorder.ariaEmpty", { name: props.name });
  return t("settings.recorder.ariaSet", { name: props.name, keys: joinCaps(caps.value) });
});

function start(): void {
  if (listening.value) return;
  listening.value = true;
  held.value = [];
  printScreenDown = false;
  emit("listening", true);
  void settings_pause_shortcuts(true).catch(() => {});
  window.addEventListener("keydown", onKeyDown, true);
  window.addEventListener("keyup", onKeyUp, true);
  window.addEventListener("pointerdown", onPointerDown, true);
  keysButton.value?.focus();
}

function stop(): void {
  if (!listening.value) return;
  listening.value = false;
  held.value = [];
  emit("listening", false);
  void settings_pause_shortcuts(false).catch(() => {});
  window.removeEventListener("keydown", onKeyDown, true);
  window.removeEventListener("keyup", onKeyUp, true);
  window.removeEventListener("pointerdown", onPointerDown, true);
}

const isPrintScreen = (event: KeyboardEvent) => event.code === "PrintScreen" || event.key === "PrintScreen";

function onKeyDown(event: KeyboardEvent): void {
  event.preventDefault();
  event.stopPropagation();
  if (event.type === "keydown" && isPrintScreen(event)) printScreenDown = true;
  const step = recorderStep(event);
  switch (step.kind) {
    case "held":
      held.value = capsOfModifiers(step.mods);
      slot.value = true;
      return;
    case "cancel":
      stop();
      emit("cancelled");
      return;
    case "clear":
      stop();
      emit("cleared");
      return;
    case "ignore":
      return;
    case "refuse":
      held.value = step.caps;
      slot.value = false;
      emit("refused", step.reason, step.caps);
      return;
    case "keys":
      stop();
      emit("captured", step.accelerator);
  }
}

function onKeyUp(event: KeyboardEvent): void {
  // WebView2 hands Print Screen to the page as a key-up alone, since Windows keeps the key-down,
  // so a Print Screen whose key-down never came is taken from its key-up.
  if (isPrintScreen(event)) {
    if (!printScreenDown) onKeyDown(event);
    printScreenDown = false;
    return;
  }
  if (!["Shift", "Control", "Alt", "Meta"].includes(event.key)) return;
  held.value = capsOfModifiers(modifiersOf(event));
  slot.value = true;
}

function onPointerDown(event: PointerEvent): void {
  if (root.value && !root.value.contains(event.target as Node)) {
    stop();
    emit("cancelled");
  }
}

function toggle(): void {
  if (listening.value) {
    stop();
    emit("cancelled");
  } else start();
}

onBeforeUnmount(stop);
defineExpose({ start, stop });
</script>

<template>
  <div ref="root" class="g-shortcut" :data-state="state">
    <button ref="keysButton" type="button" class="g-shortcut-keys" :aria-label="aria" :aria-describedby="describedby" @click="toggle">
      <template v-if="caps.length">
        <kbd v-for="(cap, i) in caps" :key="i" class="g-kbd">{{ cap }}</kbd>
        <span v-if="waiting" class="g-shortcut-slot"></span>
      </template>
      <template v-else>{{ listening ? t("settings.recorder.listening") : t("settings.recorder.record") }}</template>
    </button>
    <button
      v-if="!listening && (accelerator || pending)"
      type="button"
      class="g-shortcut-clear"
      :aria-label="t('settings.recorder.clear', { name })"
      @click="emit('cleared')"
    >
      <GIcon name="close" :size="14" />
    </button>
  </div>
</template>
