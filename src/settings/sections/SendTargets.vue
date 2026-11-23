<script setup lang="ts">
// Settings › Output › Send to app (A2, macOS): the Accessibility permission that lets Glimpsfy
// press ⌘V, and the apps the Send list shows, in its order. A row is dragged by its grip (or moved
// with ↑ and ↓ on it), says what the app gets pasted, carries its digit in the list and is removed
// with −. Add app… picks an app bundle; a terminal is added to get the path.
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import SelectControl from "../../components/SelectControl.vue";
import { on } from "../../lib/api/core";
import { permissions_open_settings, permissions_status, type PermissionState, type Permissions } from "../../lib/api/permissions";
import { send_list_targets, send_pick_app, type SendTargetInfo } from "../../lib/api/send";
import { PERMISSIONS_CHANGED } from "../../lib/events";
import { t } from "../../lib/i18n";
import type { SendPaste, SendTarget, Settings } from "../../lib/settings";
import { update } from "../store";
import { describeError } from "../text";

const props = defineProps<{ settings: Settings }>();
const targets = computed(() => props.settings.output.sendTargets);

const access = ref<PermissionState | null>(null);
/** What the system says about each target now: its icon, whether it is installed and running. */
const known = ref<Map<string, SendTargetInfo>>(new Map());
const note = ref<{ text: string; tone: "warn" | "danger" } | null>(null);
const adding = ref(false);
let stops: (() => void)[] = [];

async function loadAccess(): Promise<void> {
  try {
    access.value = (await permissions_status()).accessibility;
  } catch {
    access.value = null;
  }
}

async function loadTargets(): Promise<void> {
  try {
    const list = await send_list_targets();
    known.value = new Map(list.targets.map((target) => [target.bundleId, target]));
  } catch {
    known.value = new Map();
  }
}

// System Settings sends no word when the switch is turned, so the window asks again when it
// comes back to the front.
const refocus = () => void (loadAccess(), loadTargets());

onMounted(async () => {
  void loadAccess();
  void loadTargets();
  window.addEventListener("focus", refocus);
  try {
    stops.push(await on<Permissions>(PERMISSIONS_CHANGED, (p) => (access.value = p.accessibility)));
  } catch {
    // Outside Tauri there are no events.
  }
});
onBeforeUnmount(() => {
  window.removeEventListener("focus", refocus);
  stops.forEach((stop) => stop());
  stops = [];
});

watch(
  () => targets.value.map((target) => target.bundleId).join("\n"),
  () => void loadTargets(),
);

const granted = computed(() => access.value === "granted");
const pastes = computed(() => [
  { value: "image" as SendPaste, label: t("settings.output.send.paste.image") },
  { value: "path" as SendPaste, label: t("settings.output.send.paste.path") },
  { value: "file" as SendPaste, label: t("settings.output.send.paste.file") },
]);

const info = (target: SendTarget) => known.value.get(target.bundleId);
const missing = (target: SendTarget) => info(target)?.installed === false;

function sub(target: SendTarget): { text: string; tone?: "warn" } | null {
  if (missing(target)) return { text: t("settings.output.send.missing"), tone: "warn" };
  if (target.paste === "path") return { text: t("settings.output.send.pathNote") };
  if (info(target)?.running === false) return { text: t("settings.output.send.closedNote") };
  return null;
}

function setTargets(change: (list: SendTarget[]) => void): void {
  void update((s) => {
    const list = s.output.sendTargets.map((target) => ({ ...target }));
    change(list);
    s.output.sendTargets = list;
  });
}

function setPaste(index: number, paste: SendPaste): void {
  setTargets((list) => (list[index].paste = paste));
}

function remove(index: number): void {
  note.value = null;
  setTargets((list) => list.splice(index, 1));
}

function moveTo(from: number, to: number): void {
  if (from === to || to < 0 || to >= targets.value.length) return;
  setTargets((list) => list.splice(to, 0, ...list.splice(from, 1)));
}

async function add(): Promise<void> {
  if (adding.value) return;
  adding.value = true;
  note.value = null;
  try {
    const picked = await send_pick_app();
    if (!picked) return;
    if (targets.value.some((target) => target.bundleId === picked.bundleId)) {
      note.value = { text: t("settings.output.send.already", { app: picked.name }), tone: "warn" };
      return;
    }
    setTargets((list) => list.push({ bundleId: picked.bundleId, name: picked.name, paste: picked.paste }));
  } catch (error) {
    note.value = { text: describeError(error), tone: "danger" };
  } finally {
    adding.value = false;
  }
}

// ---------------------------------------------------------------------------- reordering

const dragging = ref<number | null>(null);
const over = ref<number | null>(null);

function dragStart(index: number, event: DragEvent): void {
  dragging.value = index;
  event.dataTransfer?.setData("text/plain", targets.value[index].bundleId);
  if (event.dataTransfer) event.dataTransfer.effectAllowed = "move";
}

function dragOver(index: number, event: DragEvent): void {
  if (dragging.value === null) return;
  event.preventDefault();
  over.value = index;
}

function drop(index: number, event: DragEvent): void {
  event.preventDefault();
  if (dragging.value !== null) moveTo(dragging.value, index);
  dragging.value = null;
  over.value = null;
}

function dragEnd(): void {
  dragging.value = null;
  over.value = null;
}

/** ↑ and ↓ on a row's grip move the row, for the keyboard. */
function gripKey(index: number, event: KeyboardEvent): void {
  if (event.key !== "ArrowUp" && event.key !== "ArrowDown") return;
  event.preventDefault();
  const to = index + (event.key === "ArrowUp" ? -1 : 1);
  moveTo(index, to);
  requestAnimationFrame(() => document.querySelector<HTMLElement>(`[data-grip="${to}"]`)?.focus());
}
</script>

<template>
  <SectionHead :title="t('settings.output.send.head')" :desc="t('settings.output.send.help')" />
  <div class="g-group">
    <FormRow
      :label="t('settings.output.send.permission')"
      :help="access === null ? null : granted ? t('settings.output.send.granted') : t('settings.output.send.denied')"
      :tone="access === null ? null : granted ? 'ok' : 'warn'"
    >
      <button v-if="access !== null && !granted" type="button" class="g-button" @click="permissions_open_settings('accessibility')">
        {{ t("send.openSettings") }}
      </button>
    </FormRow>
  </div>
  <div class="g-group st-list">
    <p v-if="!targets.length" class="st-empty">{{ t("settings.output.send.empty") }}</p>
    <div
      v-for="(target, i) in targets"
      :key="target.bundleId"
      class="st-row"
      :data-missing="missing(target) || undefined"
      :data-over="over === i && dragging !== i ? '' : undefined"
      draggable="true"
      @dragstart="dragStart(i, $event)"
      @dragover="dragOver(i, $event)"
      @drop="drop(i, $event)"
      @dragend="dragEnd"
    >
      <button
        type="button"
        class="st-grip"
        :data-grip="i"
        :aria-label="t('settings.output.send.drag', { app: target.name })"
        @keydown="gripKey(i, $event)"
      >
        <GIcon name="grip" :size="16" />
      </button>
      <img v-if="info(target)?.icon" class="st-app" :src="info(target)!.icon!" alt="" draggable="false" />
      <span v-else class="st-app st-app-none"><GIcon name="send" :size="16" /></span>
      <div class="st-text">
        <span class="st-name">{{ info(target)?.name ?? target.name }}</span>
        <span v-if="sub(target)" class="g-help" :data-tone="sub(target)!.tone">
          <GIcon v-if="sub(target)!.tone" name="warning" :size="14" />{{ sub(target)!.text }}
        </span>
      </div>
      <SelectControl
        class="st-paste"
        :model-value="target.paste"
        :options="pastes"
        :label="t('settings.output.send.pasteLabel', { app: target.name })"
        :disabled="missing(target)"
        @update:model-value="(v) => setPaste(i, v)"
      />
      <kbd v-if="!missing(target) && i < 9" class="g-kbd st-digit">{{ i + 1 }}</kbd>
      <span v-else class="st-digit" />
      <button type="button" class="g-btn" data-size="small" :aria-label="t('settings.output.send.remove', { app: target.name })" @click="remove(i)">
        <GIcon name="minus" :size="16" />
      </button>
    </div>
    <div class="st-add">
      <button type="button" class="g-button" data-variant="plain" :disabled="adding" @click="add">
        <GIcon name="plus" :size="16" />{{ t("settings.output.send.add") }}
      </button>
      <span v-if="note" class="g-help" :data-tone="note.tone"><GIcon name="warning" :size="14" />{{ note.text }}</span>
    </div>
  </div>
</template>

<style scoped>
.st-row {
  position: relative;
  display: flex;
  align-items: center;
  gap: 10px;
  min-height: 52px;
  padding: 8px 12px 8px 8px;
}
.st-row + .st-row::before,
.st-row + .st-add::before,
.st-empty + .st-add::before {
  content: "";
  position: absolute;
  left: 70px;
  right: 12px;
  top: 0;
  height: var(--hairline);
  background: var(--win-line);
}
.st-empty + .st-add::before {
  left: 12px;
}
.st-row[data-over]::after {
  content: "";
  position: absolute;
  left: 8px;
  right: 12px;
  top: -1px;
  height: 2px;
  border-radius: 1px;
  background: var(--iris);
}
.st-grip {
  display: inline-flex;
  flex: none;
  padding: 2px 0;
  border: 0;
  background: none;
  color: var(--win-text-3);
  cursor: grab;
}
.st-grip:focus-visible {
  outline: 2px solid var(--surface-focus);
  outline-offset: 1px;
  border-radius: 4px;
}
.st-app {
  flex: none;
  width: 28px;
  height: 28px;
}
.st-app-none {
  display: grid;
  place-items: center;
  color: var(--win-text-3);
}
.st-row[data-missing] .st-app {
  opacity: 0.4;
  filter: grayscale(1);
}
.st-text {
  flex: 1;
  min-width: 0;
}
.st-name {
  display: block;
  font: var(--type-text);
  color: var(--win-text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.st-row[data-missing] .st-name {
  color: var(--win-text-disabled);
}
.st-paste {
  width: 120px;
}
.st-digit {
  flex: none;
  min-width: 20px;
}
.st-add {
  position: relative;
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 6px 8px;
}
.st-empty {
  margin: 0;
  padding: 12px;
  font: var(--type-body);
  color: var(--win-text-2);
}
</style>
