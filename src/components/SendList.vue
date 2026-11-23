<script setup lang="ts">
// The list behind Send (DESIGN.md "Annotation bars", A2 Y1): Copy path first, then the targets of
// Settings › Output in their order with their digits, then Edit targets…. It opens on the target
// sent to last, so ⌘↩ and ↩ send to the same app again. Like the size menu it never takes the
// keyboard: the page hands it its keys (`key`) while it is open, and Esc stays the page's, which
// closes only the list.
import { computed, ref, watch } from "vue";

import GIcon from "./GIcon.vue";
import { t } from "../lib/i18n";
import { chord } from "../lib/keys";
import type { SendTargetInfo } from "../lib/api/send";

const props = defineProps<{
  targets: SendTargetInfo[];
  /** The target sent to last, which the list opens on. */
  last: string | null;
  /** Glimpsfy may press ⌘V; without it the list says that the user presses it. */
  canPaste: boolean;
  /** The source has no file (a text copy), so Copy path is off. */
  noPath?: boolean;
  /** Edit targets… is offered. */
  edit?: boolean;
  /** The list is only for the pointer (the toast, which never has the keyboard): no row is
   * chosen ahead and no keys are shown. */
  pointer?: boolean;
}>();

const emit = defineEmits<{
  copyPath: [];
  send: [bundleId: string];
  edit: [];
  permission: [];
}>();

type Row = { kind: "path" } | { kind: "target"; target: SendTargetInfo; digit: number } | { kind: "edit" };

const rows = computed<Row[]>(() => [
  { kind: "path" },
  ...props.targets.map((target, i) => ({ kind: "target" as const, target, digit: i + 1 })),
  ...(props.edit ? [{ kind: "edit" as const }] : []),
]);

const off = (r: Row) => (r.kind === "path" ? !!props.noPath : r.kind === "target" ? !r.target.installed : false);

/** The row the keyboard or the pointer is on; -1 for none. */
const hot = ref(0);

function opening(): number {
  const list = rows.value;
  const last = list.findIndex((r) => r.kind === "target" && r.target.bundleId === props.last && r.target.installed);
  if (last >= 0) return last;
  const first = list.findIndex((r) => r.kind === "target" && r.target.installed);
  if (first >= 0) return first;
  return list.findIndex((r) => !off(r));
}

watch(
  () => [props.targets, props.last] as const,
  () => (hot.value = props.pointer ? -1 : opening()),
  { immediate: true },
);

function sub(target: SendTargetInfo): string {
  if (!target.installed) return t("send.missing");
  if (!target.running) return t("send.closed");
  return t(`send.paste.${target.paste}`);
}

function pick(r: Row): void {
  if (off(r)) return;
  if (r.kind === "path") emit("copyPath");
  else if (r.kind === "target") emit("send", r.target.bundleId);
  else emit("edit");
}

function step(by: 1 | -1): void {
  const list = rows.value;
  const n = list.length;
  for (let i = 1; i <= n; i++) {
    const at = (((hot.value + by * i) % n) + n) % n;
    if (!off(list[at])) {
      hot.value = at;
      return;
    }
  }
}

/**
 * A key of the page while the list is open: ↑ and ↓ go through the rows, Return takes the row and
 * 1–9 send to that target at once. Esc is the page's. Returns whether the key was the list's.
 */
function key(e: KeyboardEvent): boolean {
  if (e.metaKey || e.ctrlKey || e.altKey) return false;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    step(e.key === "ArrowDown" ? 1 : -1);
    return true;
  }
  if (e.key === "Enter") {
    const r = rows.value[hot.value];
    if (r) pick(r);
    return true;
  }
  if (/^[1-9]$/.test(e.key)) {
    const r = rows.value.find((row) => row.kind === "target" && row.digit === Number(e.key));
    if (r) pick(r);
    return true;
  }
  return false;
}

defineExpose({ key });
</script>

<template>
  <div class="g-pop sl-pop" role="listbox" :aria-label="t('send.send')" style="--pop-w: 264px" @mousedown.stop @dblclick.stop>
    <div v-if="!canPaste" class="sl-note" role="alert">
      <GIcon name="warning" :size="16" />
      <div>
        <b>{{ t("send.permTitle") }}</b>
        <p>{{ t("send.permBody") }}</p>
        <button type="button" class="g-button" data-variant="primary" @mousedown.prevent @click="emit('permission')">
          {{ t("send.openSettings") }}
        </button>
      </div>
    </div>
    <template v-for="(r, i) in rows" :key="r.kind === 'target' ? r.target.bundleId : r.kind">
      <hr v-if="r.kind === 'edit' || (r.kind === 'target' && i === 1)" class="g-pop-sep" />
      <button
        type="button"
        class="g-pop-item sl-row"
        role="option"
        :aria-selected="hot === i && !off(r) && !pointer"
        :aria-disabled="off(r) ? 'true' : undefined"
        @mousedown.prevent
        @mouseenter="!off(r) && (hot = i)"
        @click="pick(r)"
      >
        <template v-if="r.kind === 'path'">
          <GIcon name="path" :size="16" />
          <span class="g-pop-name">{{ t("send.copyPath") }}</span>
          <span v-if="noPath" class="g-pop-sub">{{ t("send.noFile") }}</span>
          <kbd v-else-if="!pointer" class="g-kbd">{{ chord("⌥⌘C") }}</kbd>
        </template>
        <template v-else-if="r.kind === 'target'">
          <img v-if="r.target.icon" class="sl-app" :data-off="r.target.installed ? undefined : ''" :src="r.target.icon" alt="" />
          <GIcon v-else name="send" :size="16" />
          <span class="g-pop-name" :title="r.target.name">{{ r.target.name }}</span>
          <span class="g-pop-sub">{{ sub(r.target) }}</span>
          <kbd v-if="r.target.installed && !pointer" class="g-kbd">{{ hot === i ? "↩" : r.digit }}</kbd>
        </template>
        <template v-else>
          <GIcon name="settings" :size="16" />
          <span class="g-pop-name">{{ t("send.editTargets") }}</span>
        </template>
      </button>
    </template>
  </div>
</template>

<style scoped>
.sl-row {
  gap: 10px;
  padding: 0 6px 0 8px;
}
.sl-row[aria-selected="true"] {
  background: var(--iris-soft);
}
.sl-row > .g-kbd {
  min-width: 20px;
}
.sl-app {
  flex: none;
  width: 20px;
  height: 20px;
}
.sl-app[data-off] {
  filter: grayscale(1);
  opacity: 0.4;
}
.sl-note {
  display: flex;
  gap: 8px;
  margin: 2px 2px 6px;
  padding: 9px 10px 10px 8px;
  border-radius: var(--radius-lens);
  background: var(--tone-warn-soft);
}
.sl-note > g-icon {
  margin-top: 1px;
  color: var(--warn);
}
.sl-note b {
  display: block;
  font: var(--type-title);
  color: var(--text);
}
.sl-note p {
  margin: 2px 0 8px;
  font: var(--type-body);
  color: var(--text-muted);
}
</style>
