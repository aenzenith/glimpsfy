<script setup lang="ts">
// Settings › Agents (D34): the local MCP server's switch and the command an agent's settings
// take, the agents that connected since the app started with their timed permissions, and the
// log of the last 7 days' requests. The server is off until the user turns it on here; the page
// follows `agents://changed` and `agents://log-changed`, and every request it lists was answered
// on the approval card, never here.
import { computed, onBeforeUnmount, onMounted, ref, watch } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import SectionHead from "../../components/SectionHead.vue";
import SwitchControl from "../../components/SwitchControl.vue";
import { agentName, kindName, monogram } from "../../lib/agents";
import {
  agents_clear_log,
  agents_disconnect,
  agents_log,
  agents_revoke,
  agents_state,
  onAgentsChanged,
  onAgentsLogChanged,
  type AgentConnection,
  type AgentLogEntry,
  type AgentOutcome,
  type AgentsState,
} from "../../lib/api/agents";
import { app_copy_text } from "../../lib/api/windows";
import { language, t, type Key } from "../../lib/i18n";
import type { Settings } from "../../lib/settings";
import { update } from "../store";
import { describeError, uid } from "../text";

const props = defineProps<{ settings: Settings }>();

const ids = { enable: uid("agents-on") };
const on = computed(() => props.settings.agents?.enabled ?? false);

const state = ref<AgentsState | null>(null);
/** When `state` arrived, so the timed permissions count down between the core's reports. */
const stateAt = ref(0);
const stateError = ref<string | null>(null);
const log = ref<AgentLogEntry[]>([]);
const logError = ref<string | null>(null);

function setEnabled(value: boolean): void {
  void update((s) => {
    s.agents = { ...(s.agents ?? {}), enabled: value };
  });
}

/** The server is on but its socket did not open; the English reason stays in the tip. */
const failed = computed(() => on.value && !!state.value && !state.value.listening && !!state.value.error);

// ----------------------------------------------------------------------------------- command

const command = computed(() => state.value?.command ?? "");
const copied = ref(false);
let copiedTimer: ReturnType<typeof setTimeout> | null = null;

async function copyCommand(): Promise<void> {
  if (!command.value) return;
  try {
    await app_copy_text(command.value);
    copied.value = true;
    if (copiedTimer) clearTimeout(copiedTimer);
    copiedTimer = setTimeout(() => (copied.value = false), 1500);
  } catch (error) {
    console.warn("app_copy_text", error);
  }
}

// ---------------------------------------------------------------------------------- agents

const connections = computed(() => state.value?.connections ?? []);

/** Wakes once a minute while a timed permission runs, so its minutes count down here too. */
const now = ref(performance.now());
let tick: ReturnType<typeof setTimeout> | null = null;

const grantLeft = (c: AgentConnection) => (c.grantLeftMs == null ? 0 : Math.max(0, c.grantLeftMs - (now.value - stateAt.value)));

function schedule(): void {
  if (tick) clearTimeout(tick);
  tick = null;
  now.value = performance.now();
  const left = connections.value.map(grantLeft).filter((ms) => ms > 0);
  if (!left.length) return;
  tick = setTimeout(schedule, Math.min(...left.map((ms) => ms % 60_000 || 60_000)) + 50);
}
watch([state, stateAt], schedule);

const minutes = (c: AgentConnection) => t("agent.minutes", { n: Math.ceil(grantLeft(c) / 60_000) });

const sameDay = (a: Date, b: Date) => a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();
const clock = (at: Date) => new Intl.DateTimeFormat(language.value, { hour: "2-digit", minute: "2-digit" }).format(at);
const shortDate = (at: Date) => new Intl.DateTimeFormat(language.value, { day: "numeric", month: "short" }).format(at);
const fullDate = (at: Date) => new Intl.DateTimeFormat(language.value, { dateStyle: "medium", timeStyle: "short" }).format(at);

function lastRequest(ms: number | null): string | null {
  if (ms == null) return null;
  const at = new Date(ms);
  if (sameDay(at, new Date())) return t("settings.agents.lastToday", { time: clock(at) });
  return t("settings.agents.lastOn", { date: `${shortDate(at)} ${clock(at)}` });
}

/** "Terminal · ~/projects/glimpsfy · 6 requests today" while a grant runs, since the count is what
 * the grant let through; "Cursor · q4 · last request today at 13:58" for a connected agent; and
 * "Not connected · last request today at 12:10" for one that left. */
function helpOf(c: AgentConnection): string {
  if (!c.connected) return [t("settings.agents.disconnected"), lastRequest(c.lastRequestMs)].filter(Boolean).join(" · ");
  const n = c.requestsToday;
  const today = n === 0 ? t("settings.agents.todayNone") : n === 1 ? t("settings.agents.today.one") : t("settings.agents.today.other", { n });
  const last = c.grantLeftMs == null ? lastRequest(c.lastRequestMs) : null;
  return [c.process, last ?? today].filter(Boolean).join(" · ");
}

function revoke(c: AgentConnection): void {
  agents_revoke(c.connection).catch((error) => console.warn("agents_revoke", error));
}

function disconnect(c: AgentConnection): void {
  agents_disconnect(c.connection).catch((error) => console.warn("agents_disconnect", error));
}

// -------------------------------------------------------------------------------------- log

const OUTCOME: Record<AgentOutcome, { icon: string; tone?: "ok" | "warn"; label: Key }> = {
  allowed: { icon: "check", tone: "ok", label: "settings.agents.outcome.allowed" },
  timed: { icon: "check", tone: "ok", label: "settings.agents.outcome.timed" },
  denied: { icon: "close", label: "settings.agents.outcome.denied" },
  timedOut: { icon: "timer", tone: "warn", label: "settings.agents.outcome.timedOut" },
  locked: { icon: "lock", label: "settings.agents.outcome.locked" },
  queueFull: { icon: "bang", tone: "warn", label: "settings.agents.outcome.queueFull" },
  cancelled: { icon: "stop", label: "settings.agents.outcome.cancelled" },
  failed: { icon: "bang", tone: "warn", label: "settings.agents.outcome.failed" },
};

/** The time of a request today, or its day for an older one; the tip holds both. */
function when(ms: number): string {
  const at = new Date(ms);
  return sameDay(at, new Date()) ? clock(at) : shortDate(at);
}

const what = (entry: AgentLogEntry) => [kindName(entry.kind), entry.target].filter(Boolean).join(" · ");

async function loadLog(): Promise<void> {
  try {
    log.value = await agents_log();
    logError.value = null;
  } catch (error) {
    logError.value = describeError(error);
  }
}

async function clearLog(): Promise<void> {
  try {
    await agents_clear_log();
  } catch (error) {
    logError.value = describeError(error);
  }
  await loadLog();
}

// ----------------------------------------------------------------------------------- wiring

function apply(next: AgentsState): void {
  state.value = next;
  stateAt.value = performance.now();
  stateError.value = null;
}

const stops: (() => void)[] = [];
onMounted(async () => {
  stops.push(await onAgentsChanged(apply), await onAgentsLogChanged(() => void loadLog()));
  try {
    apply(await agents_state());
  } catch (error) {
    stateError.value = describeError(error);
  }
  await loadLog();
});
onBeforeUnmount(() => {
  stops.forEach((stop) => stop());
  if (tick) clearTimeout(tick);
  if (copiedTimer) clearTimeout(copiedTimer);
});
</script>

<template>
  <SectionHead :title="t('settings.agents.server')" :desc="t('settings.agents.serverDesc')" />
  <div class="g-group">
    <FormRow
      :label="t('settings.agents.enable')"
      :for="ids.enable"
      :help="failed ? t('settings.agents.notListening') : t('settings.agents.enableHelp')"
      :tone="failed ? 'warn' : null"
    >
      <SwitchControl :id="ids.enable" :model-value="on" @update:model-value="setEnabled" />
    </FormRow>
    <FormRow :label="t('settings.agents.command')" :help="t('settings.agents.commandHelp')" :disabled="!on" stacked>
      <div class="a-cmd">
        <code class="a-code" :title="command">{{ command }}</code>
        <button
          type="button"
          class="g-btn"
          data-size="small"
          :aria-label="copied ? t('settings.agents.copied') : t('settings.agents.copyCommand')"
          :title="copied ? t('settings.agents.copied') : t('settings.agents.copyCommand')"
          :disabled="!on || !command"
          @click="copyCommand"
        >
          <GIcon :name="copied ? 'check' : 'copy'" :size="16" />
        </button>
      </div>
    </FormRow>
  </div>
  <p v-if="failed && state?.error" class="g-help a-error" :title="state.error">{{ state.error }}</p>

  <SectionHead :title="t('settings.agents.connected')" :desc="t('settings.agents.connectedDesc')" />
  <div class="g-group">
    <p v-if="!on" class="a-off">{{ t("settings.agents.off") }}</p>
    <p v-else-if="stateError" class="a-off">{{ stateError }}</p>
    <p v-else-if="!connections.length" class="a-off">{{ t("settings.agents.none") }}</p>
    <template v-else>
      <div v-for="c in connections" :key="c.connection" class="g-form-row">
        <span class="p-mono a-mono" aria-hidden="true">{{ monogram(agentName(c.agent)) }}</span>
        <div class="g-form-label">
          <span class="g-label">{{ agentName(c.agent) }}</span>
          <span class="g-help">{{ helpOf(c) }}</span>
        </div>
        <div v-if="grantLeft(c) > 0" class="g-form-control">
          <span class="a-timed"><GIcon name="timer" :size="14" />{{ minutes(c) }}</span>
          <button type="button" class="g-button" @click="revoke(c)">{{ t("agent.revoke") }}</button>
        </div>
        <div v-else-if="c.connected" class="g-form-control">
          <button type="button" class="g-button" data-variant="plain" @click="disconnect(c)">{{ t("settings.agents.disconnect") }}</button>
        </div>
      </div>
    </template>
  </div>

  <SectionHead :title="t('settings.agents.log')" :desc="t('settings.agents.logDesc')">
    <button type="button" class="g-button" data-variant="plain" :disabled="!log.length" @click="clearLog">{{ t("settings.agents.clearLog") }}</button>
  </SectionHead>
  <div class="g-group" :class="{ 'a-log': log.length }">
    <p v-if="logError" class="a-off">{{ logError }}</p>
    <p v-else-if="!log.length" class="a-off">{{ t("settings.agents.logEmpty") }}</p>
    <table v-else class="g-table">
      <colgroup>
        <col style="width: 62px" />
        <col style="width: 108px" />
        <col />
        <col style="width: 136px" />
      </colgroup>
      <thead>
        <tr>
          <th>{{ t("settings.agents.col.time") }}</th>
          <th>{{ t("settings.agents.col.agent") }}</th>
          <th>{{ t("settings.agents.col.request") }}</th>
          <th>{{ t("settings.agents.col.answer") }}</th>
        </tr>
      </thead>
      <tbody>
        <tr v-for="(entry, i) in log" :key="`${entry.atMs}-${i}`">
          <td class="g-cell-sub" :title="fullDate(new Date(entry.atMs))">{{ when(entry.atMs) }}</td>
          <td :title="agentName(entry.agent)">{{ agentName(entry.agent) }}</td>
          <td class="g-cell-sub" :title="what(entry)">{{ what(entry) }}</td>
          <td :title="t(OUTCOME[entry.outcome].label)">
            <span class="g-cell-icon" :data-tone="OUTCOME[entry.outcome].tone"
              ><GIcon :name="OUTCOME[entry.outcome].icon" :size="16" />{{ t(OUTCOME[entry.outcome].label) }}</span
            >
          </td>
        </tr>
      </tbody>
    </table>
  </div>

  <p class="p-note"><GIcon name="shield" :size="16" /><span>{{ t("settings.agents.note") }}</span></p>
</template>

<style scoped>
.a-mono {
  margin-right: -4px;
}
.a-cmd {
  display: flex;
  align-items: center;
  gap: 4px;
  min-width: 0;
}
.a-code {
  flex: 1;
  min-width: 0;
  padding: 5px 10px;
  border-radius: var(--radius-field);
  background: var(--win-fill);
  font: 400 12px/18px var(--font-mono);
  color: var(--win-text);
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}
.g-form-row[aria-disabled="true"] .a-code {
  color: var(--win-text-disabled);
}
/* It stands between a group and the next head, so it takes the gap `.g-group + .g-section-head` gives. */
.a-error {
  margin: 6px 0 14px;
  padding: 0 4px;
  font: 400 11.5px/16px var(--font-mono);
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.a-timed {
  display: inline-flex;
  align-items: center;
  gap: 4px;
  font: var(--type-num);
  color: var(--win-iris-text);
}
.a-off {
  margin: 0;
  padding: 14px 12px;
  font: var(--type-body);
  color: var(--win-text-2);
}
.a-log {
  padding: 4px 6px 6px;
}
.a-log .g-table {
  table-layout: fixed;
}
.a-log td {
  overflow: hidden;
  text-overflow: ellipsis;
}
.a-log .g-cell-icon > g-icon {
  color: var(--win-text-2);
}
.a-log .g-cell-icon[data-tone="ok"] > g-icon {
  color: var(--win-ok);
}
.a-log .g-cell-icon[data-tone="warn"] > g-icon {
  color: var(--win-warn);
}
.p-note {
  margin-top: 16px;
}
.p-note > g-icon {
  flex: none;
}
</style>
