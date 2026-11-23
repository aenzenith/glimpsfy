<script setup lang="ts">
// The setup bar (design `setupBar`): Close · Display, Window, Area · system audio with its meter ·
// the microphone with its meter and its source · the camera and its source · options · Record.
// It only draws and reports what was pressed; `Bar.vue` acts. Each control names its tooltip in
// `data-tip` and `data-keys`, which the bar page shows inside its own window.
import GIcon from "../components/GIcon.vue";
import type { TargetMode } from "../lib/api/record";
import { t, tp } from "../lib/i18n";
import { can } from "../lib/platform";

export type SetupAction =
  | "close"
  | "display"
  | "window"
  | "area"
  | "system"
  | "mic"
  | "micMenu"
  | "camera"
  | "cameraMenu"
  | "more"
  | "record";

/** The keys of the setup bar; they work while the bar has the keyboard. */
const SETUP_KEYS = {
  close: "Esc",
  display: "D",
  window: "W",
  area: "A",
  system: "S",
  mic: "M",
  micMenu: "⇧M",
  camera: "C",
  cameraMenu: "⇧C",
  more: "O",
  record: "↩",
} as const;

const props = defineProps<{
  mode: TargetMode;
  system: { on: boolean; available: boolean; level: number };
  mic: { on: boolean; denied: boolean; level: number };
  camera: { on: boolean; denied: boolean };
  open: string | null;
  canRecord: boolean;
}>();
const emit = defineEmits<{ action: [id: SetupAction] }>();

/** A single window is offered only where it can be recorded on its own (`windowCapture`). */
const MODES = (["display", "window", "area"] as const).filter((m) => m !== "window" || can("windowCapture"));
const modeLabel = { display: "record.mode.display", window: "record.mode.window", area: "record.mode.area" } as const;

/** Arrow keys move the choice of target, as in a native segmented control. */
function modeKey(event: KeyboardEvent): void {
  const step = ({ ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 } as Record<string, number>)[event.key];
  if (!step) return;
  event.preventDefault();
  const next = MODES[(MODES.indexOf(props.mode) + step + MODES.length) % MODES.length] ?? props.mode;
  emit("action", next);
  const group = event.currentTarget as HTMLElement;
  requestAnimationFrame(() => group.querySelector<HTMLElement>(`[data-id="${next}"]`)?.focus());
}

const level = (v: number) => Math.max(0, Math.min(1, v));
</script>

<template>
  <div class="g-bar" role="toolbar" :aria-label="t('record.bar.setup')" data-bar="setup">
    <button
      type="button"
      class="g-btn"
      data-tone="muted"
      data-id="close"
      :aria-label="t('record.close')"
      :data-tip="t('record.close')"
      :data-keys="SETUP_KEYS.close"
      @click="emit('action', 'close')"
    >
      <GIcon name="close" />
    </button>
    <span class="g-divider" role="separator" />
    <div class="g-seg" role="radiogroup" :aria-label="t('record.mode.label')" @keydown="modeKey">
      <button
        v-for="m in MODES"
        :key="m"
        type="button"
        role="radio"
        :data-id="m"
        :aria-checked="mode === m"
        :tabindex="mode === m ? 0 : -1"
        :aria-label="t(modeLabel[m])"
        :data-tip="t(modeLabel[m])"
        :data-keys="SETUP_KEYS[m]"
        @click="emit('action', m)"
      >
        <GIcon :name="m" />
      </button>
    </div>
    <span class="g-divider" role="separator" />
    <button
      type="button"
      class="g-toggle"
      data-id="system"
      :aria-pressed="system.on"
      :disabled="!system.available"
      :data-tip="system.available ? t('record.system.tip') : tp('record.system.unavailable')"
      :data-keys="system.available ? SETUP_KEYS.system : undefined"
      @click="emit('action', 'system')"
    >
      <GIcon :name="system.on ? 'speaker' : 'speaker-off'" /><span>{{ t("record.system") }}</span>
      <span
        class="g-meter"
        role="meter"
        :aria-label="t('record.meter.level', { name: t('record.system.tip') })"
        aria-valuemin="0"
        aria-valuemax="1"
        :aria-valuenow="level(system.level).toFixed(2)"
        :style="{ '--level': level(system.level) }"
      />
    </button>
    <button
      type="button"
      class="g-toggle"
      data-id="mic"
      :aria-pressed="mic.on"
      :aria-haspopup="mic.denied ? 'dialog' : undefined"
      :data-perm="mic.denied ? 'denied' : undefined"
      :data-tip="mic.denied ? t('record.mic.denied') : t('record.mic.tip')"
      :data-keys="SETUP_KEYS.mic"
      @click="emit('action', 'mic')"
    >
      <GIcon :name="mic.on ? 'microphone' : 'microphone-off'" /><span>{{ t("record.mic") }}</span>
      <GIcon v-if="mic.denied" name="warning" :size="16" class="r-warn" />
      <span
        v-else
        class="g-meter"
        role="meter"
        :aria-label="t('record.meter.level', { name: t('record.mic.tip') })"
        aria-valuemin="0"
        aria-valuemax="1"
        :aria-valuenow="level(mic.level).toFixed(2)"
        :style="{ '--level': level(mic.level) }"
      />
    </button>
    <button
      type="button"
      class="g-btn"
      data-size="small"
      data-tone="muted"
      data-id="micMenu"
      :aria-label="t('record.mic.choose')"
      aria-haspopup="menu"
      :aria-expanded="open === 'mic'"
      :data-state="open === 'mic' ? 'pressed' : undefined"
      :data-tip="t('record.mic.choose')"
      :data-keys="SETUP_KEYS.micMenu"
      @click="emit('action', 'micMenu')"
    >
      <GIcon name="chevron-down" :size="16" />
    </button>
    <!-- The webcam bubble only where the platform draws it (`cameraBubble`). -->
    <template v-if="can('cameraBubble')">
      <button
        type="button"
        class="g-toggle"
        data-id="camera"
        :aria-pressed="camera.on"
        :aria-label="camera.denied ? t('record.camera.denied') : t('record.camera')"
        :aria-haspopup="camera.denied ? 'dialog' : undefined"
        :data-perm="camera.denied ? 'denied' : undefined"
        :data-tip="camera.denied ? t('record.camera.denied') : t('record.camera')"
        :data-keys="SETUP_KEYS.camera"
        @click="emit('action', 'camera')"
      >
        <GIcon :name="camera.on ? 'webcam' : 'webcam-off'" />
        <GIcon v-if="camera.denied" name="warning" :size="16" class="r-warn" />
      </button>
      <button
        type="button"
        class="g-btn"
        data-size="small"
        data-tone="muted"
        data-id="cameraMenu"
        :aria-label="t('record.camera.choose')"
        aria-haspopup="menu"
        :aria-expanded="open === 'cam'"
        :data-state="open === 'cam' ? 'pressed' : undefined"
        :data-tip="t('record.camera.choose')"
        :data-keys="SETUP_KEYS.cameraMenu"
        @click="emit('action', 'cameraMenu')"
      >
        <GIcon name="chevron-down" :size="16" />
      </button>
    </template>
    <span class="g-divider" role="separator" />
    <button
      type="button"
      class="g-btn"
      data-id="more"
      :aria-label="t('record.more')"
      aria-haspopup="dialog"
      :aria-expanded="open === 'more'"
      :data-state="open === 'more' ? 'pressed' : undefined"
      :data-tip="t('record.more')"
      :data-keys="SETUP_KEYS.more"
      @click="emit('action', 'more')"
    >
      <GIcon name="more" />
    </button>
    <button
      type="button"
      class="g-button r-record"
      data-variant="record"
      data-id="record"
      :disabled="!canRecord"
      :data-tip="canRecord ? t('record.record.tip') : t('record.record.pickWindow')"
      :data-keys="canRecord ? SETUP_KEYS.record : undefined"
      @click="emit('action', 'record')"
    >
      <GIcon name="record" :size="16" />{{ t("record.record") }}
    </button>
  </div>
</template>

<style>
.r-record { margin-left: var(--space-4); }
.r-warn { color: var(--warn); }
</style>
