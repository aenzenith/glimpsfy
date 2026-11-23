<script setup lang="ts">
// The agents' toasts (D34, Y1): `agentSent`, the row a timed permission's capture brings, with
// the lock that takes the permission back; `agentTimedOut`, the quiet card of a request nobody
// answered, which leaves by itself; and `agentLocked`, the quiet card that tells once, after the
// unlock, which requests the screen lock refused. The quiet cards share the approval card's
// width and picture (AgentCard.vue) and open the log in Settings › Agents.
import { computed } from "vue";

import GIcon from "../components/GIcon.vue";
import TipWrap from "../components/TipWrap.vue";
import { agentName, kindIcon, kindName, nameList } from "../lib/agents";
import { agents_revoke } from "../lib/api/agents";
import { window_open_settings, type Toast } from "../lib/api/windows";
import { t } from "../lib/i18n";
import { fitIn } from "./agent";

const props = defineProps<{ toast: Toast }>();
const emit = defineEmits<{ dismiss: [] }>();

const agent = computed(() => props.toast.agent ?? null);
const name = computed(() => agentName(agent.value?.agent));
const picture = computed(() => fitIn(props.toast.width, props.toast.height, 76, 52));

const sentMeta = computed(() => {
  const a = agent.value;
  if (!a) return "";
  const parts = [kindName(a.kind)];
  if (a.minutesLeft != null) parts.push(t("toast.agent.grantLeft", { n: a.minutesLeft }));
  return parts.join(" · ");
});

const lockedTitle = computed(() => {
  const n = agent.value?.count ?? 1;
  return n === 1 ? t("toast.agent.locked.one") : t("toast.agent.locked.other", { n });
});

const lockedBody = computed(() => {
  const a = agent.value;
  const agents = nameList(a?.agents ?? []);
  return (a?.count ?? 1) === 1 ? t("toast.agent.lockedBody.one", { agents }) : t("toast.agent.lockedBody.other", { agents });
});

function revoke(): void {
  const connection = agent.value?.connection;
  emit("dismiss");
  if (connection != null) agents_revoke(connection).catch((error) => console.warn("agents_revoke", error));
}

function openLog(): void {
  emit("dismiss");
  window_open_settings("agents").catch((error) => console.warn("window_open_settings", error));
}
</script>

<template>
  <!-- A capture a timed permission sent: an ordinary toast row. -->
  <div v-if="toast.kind === 'agentSent'" class="g-toast" role="status">
    <div class="g-toast-thumb">
      <img v-if="toast.thumbnail" :src="toast.thumbnail" alt="" draggable="false" />
      <span v-else class="ag-icon"><GIcon :name="kindIcon(agent?.kind ?? 'window')" :size="18" /></span>
      <span class="g-status" data-status="ok"><GIcon name="check" :size="12" /></span>
    </div>
    <div class="g-toast-text">
      <div class="g-toast-title" :title="t('toast.agent.sent', { agent: name })">{{ t("toast.agent.sent", { agent: name }) }}</div>
      <div class="g-toast-body">{{ sentMeta }}</div>
    </div>
    <div class="g-toast-actions">
      <!-- The tips open to the left: the window leaves no room above or below the stack. -->
      <TipWrap :tip="t('agent.revoke')" side="left">
        <button type="button" class="g-btn" data-size="small" :aria-label="t('agent.revoke')" @click="revoke">
          <GIcon name="lock" :size="16" />
        </button>
      </TipWrap>
      <TipWrap :tip="t('toast.dismiss')" side="left">
        <button type="button" class="g-btn" data-size="small" :aria-label="t('toast.dismiss')" @click="emit('dismiss')">
          <GIcon name="close" :size="16" />
        </button>
      </TipWrap>
    </div>
  </div>

  <!-- A request nobody answered, or the requests the screen lock refused: a quiet card. -->
  <div v-else class="g-toast ag-note" role="status">
    <div class="ag-top">
      <span class="ag-thumb">
        <template v-if="toast.kind === 'agentTimedOut'">
          <span v-if="toast.thumbnail" class="ag-pic" :data-whole="picture ? undefined : ''" :style="picture ?? undefined">
            <img :src="toast.thumbnail" alt="" draggable="false" />
          </span>
          <span v-else class="ag-pic" data-ph><GIcon :name="kindIcon(agent?.kind ?? 'window')" :size="20" /></span>
          <span class="g-status" data-status="warn"><GIcon name="timer" :size="12" /></span>
        </template>
        <span v-else class="ag-pic" data-ph><GIcon name="lock" :size="20" /></span>
      </span>
      <div class="ag-text">
        <template v-if="toast.kind === 'agentTimedOut'">
          <div class="ag-title">{{ t("toast.agent.timedOut") }}</div>
          <div class="ag-body">{{ t("toast.agent.timedOutBody", { agent: name }) }}</div>
        </template>
        <template v-else>
          <div class="ag-title">{{ lockedTitle }}</div>
          <div class="ag-body">{{ lockedBody }}</div>
        </template>
      </div>
    </div>
    <div class="ag-acts2">
      <button type="button" class="g-button" data-variant="plain" @click="openLog"><GIcon name="history" :size="16" />{{ t("toast.agent.openLog") }}</button>
      <span class="g-spacer"></span>
      <TipWrap :tip="t('toast.dismiss')" side="left">
        <button type="button" class="g-btn" data-size="small" :aria-label="t('toast.dismiss')" @click="emit('dismiss')">
          <GIcon name="close" :size="16" />
        </button>
      </TipWrap>
    </div>
  </div>
</template>

<style scoped>
.ag-icon {
  display: grid;
  place-items: center;
  background: var(--glass-solid);
  color: var(--iris-text);
}
.g-toast.ag-note {
  display: block;
  width: 340px;
  max-width: none;
  padding: 10px 10px 8px;
}
.ag-top {
  display: grid;
  grid-template-columns: 76px minmax(0, 1fr);
  column-gap: 12px;
  align-items: start;
}
.ag-text {
  min-width: 0;
}
.ag-title {
  font: var(--type-title);
}
.ag-body {
  margin-top: 2px;
  font: var(--type-body);
  color: var(--text-muted);
}
.ag-acts2 {
  display: flex;
  align-items: center;
  gap: 2px;
  margin: 8px -2px 0 -4px;
}
.ag-acts2 > .g-button {
  color: var(--text-muted);
}
.ag-acts2 .g-btn {
  color: var(--text-muted);
}
/* WebKit's own hover in the toast panel is the spot of the last click (App.vue, "the pointer"). */
.ag-acts2 > .g-button[data-variant="plain"]:hover:not([data-state="hover"], :active) {
  background: none;
}
</style>
