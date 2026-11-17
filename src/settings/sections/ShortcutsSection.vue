<script setup lang="ts">
// Settings › Shortcuts: one recorder per global action, the clashes with another action and with
// macOS, the keys the system refused, Restore defaults, and the fixed keys of the overlay.
import { reactive, ref } from "vue";

import FormRow from "../../components/FormRow.vue";
import GIcon from "../../components/GIcon.vue";
import KeyCaps from "../../components/KeyCaps.vue";
import SectionHead from "../../components/SectionHead.vue";
import ShortcutRecorder from "../../components/ShortcutRecorder.vue";
import { settings_default_shortcuts } from "../../lib/api/settings";
import { app_open_system_settings } from "../../lib/api/windows";
import { t, type Key } from "../../lib/i18n";
import { capsOf, capsText, holderOf, SHORTCUT_GROUPS, systemShortcut, type ShortcutAction } from "../../lib/keys";
import type { Settings } from "../../lib/settings";
import { banner, clearedShortcuts, forgetCleared, shortcutProblems, update } from "../store";
import { describeError, tr, uid } from "../text";

const props = defineProps<{ settings: Settings }>();

type Conflict = { accelerator: string; other: ShortcutAction };
type Refusal = { reason: "needsModifier" | "commandAlone"; caps: string[] };

const listening = ref<ShortcutAction | null>(null);
const refusals = reactive<Partial<Record<ShortcutAction, Refusal>>>({});
const conflicts = reactive<Partial<Record<ShortcutAction, Conflict>>>({});
/** The keys a row had before it took keys macOS uses, for Undo. */
const undo = reactive<Partial<Record<ShortcutAction, string | null>>>({});
const restoring = ref(false);

const helpIds = Object.fromEntries(SHORTCUT_GROUPS.flatMap((g) => g.actions).map((a) => [a, uid(`sc-${a}`)])) as Record<ShortcutAction, string>;

const nameOf = (action: string) => tr(`settings.shortcuts.action.${action}`, {}, action);
const own = (action: ShortcutAction) => props.settings.shortcuts[action] ?? null;

type RowView = { help: string | null; tone: "warn" | "danger" | null; kind: "conflict" | "system" | null };

function view(action: ShortcutAction): RowView {
  const refusal = refusals[action];
  if (listening.value === action) {
    if (refusal) return { help: t(refusal.reason === "needsModifier" ? "settings.shortcuts.needsModifier" : "settings.shortcuts.commandAlone"), tone: "danger", kind: null };
    return { help: t("settings.shortcuts.listening"), tone: null, kind: null };
  }
  const conflict = conflicts[action];
  if (conflict) return { help: t("settings.shortcuts.taken", { other: nameOf(conflict.other), keys: capsText(conflict.accelerator) }), tone: "warn", kind: "conflict" };
  const problem = shortcutProblems.value[action];
  if (problem && problem.accelerator === own(action)) return { help: t("settings.shortcuts.refusedBySystem", { keys: capsText(problem.accelerator) }), tone: "danger", kind: null };
  const system = systemShortcut(own(action));
  if (system) return { help: tr(system, {}, t("settings.shortcuts.systemFallback", { keys: capsText(own(action)) })), tone: "warn", kind: "system" };
  const cleared = clearedShortcuts.value[action];
  if (cleared && !own(action)) return { help: t("settings.shortcuts.cleared", { other: nameOf(cleared.keptBy), keys: capsText(cleared.accelerator) }), tone: "warn", kind: null };
  if (action === "recordScreen") return { help: t("settings.shortcuts.recordScreenHelp"), tone: null, kind: null };
  return { help: null, tone: null, kind: null };
}

function onListening(action: ShortcutAction, on: boolean): void {
  if (on) {
    listening.value = action;
    delete conflicts[action];
  } else if (listening.value === action) {
    listening.value = null;
  }
  delete refusals[action];
}

function onCancelled(action: ShortcutAction): void {
  delete refusals[action];
}

function onRefused(action: ShortcutAction, reason: Refusal["reason"], caps: string[]): void {
  refusals[action] = { reason, caps };
}

function onCaptured(action: ShortcutAction, accelerator: string): void {
  delete refusals[action];
  const other = holderOf(props.settings.shortcuts, accelerator, action);
  if (other) {
    conflicts[action] = { accelerator, other };
    return;
  }
  save(action, accelerator);
}

function save(action: ShortcutAction, accelerator: string | null, other?: ShortcutAction): void {
  const previous = own(action);
  forgetCleared(action);
  if (other) forgetCleared(other);
  delete undo[action];
  void update((s) => {
    s.shortcuts[action] = accelerator;
    if (other) s.shortcuts[other] = null;
  }).then((saved) => {
    if (saved && accelerator && systemShortcut(accelerator)) undo[action] = previous;
  });
}

function moveHere(action: ShortcutAction): void {
  const conflict = conflicts[action];
  if (!conflict) return;
  delete conflicts[action];
  save(action, conflict.accelerator, conflict.other);
}

function cancelConflict(action: ShortcutAction): void {
  delete conflicts[action];
}

function clearKeys(action: ShortcutAction): void {
  delete conflicts[action];
  delete refusals[action];
  if (own(action) === null) return;
  save(action, null);
}

function undoSystem(action: ShortcutAction): void {
  if (!(action in undo)) return;
  const previous = undo[action] ?? null;
  delete undo[action];
  const other = previous ? holderOf(props.settings.shortcuts, previous, action) : null;
  save(action, previous, other ?? undefined);
}

function openKeyboardShortcuts(): void {
  void app_open_system_settings("keyboardShortcuts").catch((error) => (banner.value = { tone: "danger", text: describeError(error) }));
}

async function restoreDefaults(): Promise<void> {
  restoring.value = true;
  try {
    const defaults = await settings_default_shortcuts();
    for (const key of Object.keys(conflicts)) delete conflicts[key as ShortcutAction];
    for (const key of Object.keys(undo)) delete undo[key as ShortcutAction];
    for (const group of SHORTCUT_GROUPS) for (const action of group.actions) forgetCleared(action);
    await update((s) => Object.assign(s.shortcuts, defaults));
  } catch (error) {
    banner.value = { tone: "danger", text: describeError(error) };
  } finally {
    restoring.value = false;
  }
}

type RefRow = { name: Key; caps: string[] };
const ARROWS = ["←", "↑", "→", "↓"];
const TOOLS: RefRow[] = [
  { name: "settings.shortcuts.overlay.arrow", caps: ["A"] },
  { name: "settings.shortcuts.overlay.line", caps: ["L"] },
  { name: "settings.shortcuts.overlay.rectangle", caps: ["R"] },
  { name: "settings.shortcuts.overlay.ellipse", caps: ["O"] },
  { name: "settings.shortcuts.overlay.pen", caps: ["P"] },
  { name: "settings.shortcuts.overlay.highlighter", caps: ["H"] },
  { name: "settings.shortcuts.overlay.text", caps: ["T"] },
  { name: "settings.shortcuts.overlay.step", caps: ["N"] },
  { name: "settings.shortcuts.overlay.blur", caps: ["B"] },
  { name: "settings.shortcuts.overlay.pixelate", caps: ["X"] },
  { name: "settings.shortcuts.overlay.spotlight", caps: ["S"] },
  { name: "settings.shortcuts.overlay.crop", caps: ["C"] },
];
const ACTIONS: RefRow[] = [
  { name: "settings.shortcuts.overlay.copy", caps: ["⌘", "C"] },
  { name: "settings.shortcuts.overlay.save", caps: ["⌘", "S"] },
  { name: "settings.shortcuts.overlay.saveAs", caps: ["⇧", "⌘", "S"] },
  { name: "settings.shortcuts.overlay.upload", caps: ["⌘", "U"] },
  { name: "settings.shortcuts.overlay.copyText", caps: ["⇧", "⌘", "C"] },
  { name: "settings.shortcuts.overlay.pin", caps: ["⌘", "P"] },
  { name: "settings.shortcuts.overlay.record", caps: ["⌘", "R"] },
  { name: "settings.shortcuts.overlay.close", caps: ["Esc"] },
];
const SELECTION: RefRow[] = [
  { name: "settings.shortcuts.overlay.undo", caps: ["⌘", "Z"] },
  { name: "settings.shortcuts.overlay.redo", caps: ["⇧", "⌘", "Z"] },
  { name: "settings.shortcuts.overlay.move1", caps: ARROWS },
  { name: "settings.shortcuts.overlay.move10", caps: ["⇧", ...ARROWS] },
  { name: "settings.shortcuts.overlay.resize1", caps: ["⌥", ...ARROWS] },
];
const DRAWINGS: RefRow[] = [
  { name: "settings.shortcuts.overlay.selectAll", caps: ["⌘", "A"] },
  { name: "settings.shortcuts.overlay.drawOver", caps: ["⌘"] },
];
</script>

<template>
  <template v-for="group in SHORTCUT_GROUPS" :key="group.group">
    <SectionHead :title="tr(`settings.shortcuts.group.${group.group}`, {}, group.group)" />
    <div class="g-group">
      <FormRow
        v-for="action in group.actions"
        :key="action"
        :label="nameOf(action)"
        :help="view(action).help"
        :tone="view(action).tone"
        :help-id="helpIds[action]"
      >
        <template v-if="view(action).kind === 'conflict'" #after>
          <div class="p-actions p-flush">
            <button type="button" class="g-button" @click="moveHere(action)">{{ t("settings.shortcuts.moveHere") }}</button>
            <button type="button" class="g-button" data-variant="plain" @click="cancelConflict(action)">{{ t("settings.common.cancel") }}</button>
          </div>
        </template>
        <template v-else-if="view(action).kind === 'system'" #after>
          <div class="p-actions p-flush">
            <button type="button" class="g-button" data-variant="plain" @click="openKeyboardShortcuts">
              <GIcon name="external-link" :size="16" />{{ t("settings.shortcuts.openKeyboard") }}
            </button>
            <button v-if="action in undo" type="button" class="g-button" data-variant="plain" @click="undoSystem(action)">{{ t("settings.common.undo") }}</button>
          </div>
        </template>
        <ShortcutRecorder
          :accelerator="own(action)"
          :name="nameOf(action)"
          :describedby="view(action).help ? helpIds[action] : undefined"
          :pending="conflicts[action] ? capsOf(conflicts[action]!.accelerator) : listening === action && refusals[action] ? refusals[action]!.caps : null"
          :warn="!!conflicts[action] || view(action).kind === 'system'"
          @listening="(on) => onListening(action, on)"
          @refused="(reason, caps) => onRefused(action, reason, caps)"
          @captured="(accel) => onCaptured(action, accel)"
          @cleared="clearKeys(action)"
          @cancelled="onCancelled(action)"
        />
      </FormRow>
    </div>
  </template>

  <div class="p-foot">
    <span class="g-help">{{ t("settings.shortcuts.restoreHelp") }}</span>
    <button type="button" class="g-button" :disabled="restoring" @click="restoreDefaults">{{ t("settings.shortcuts.restore") }}</button>
  </div>

  <div class="p-gap">
    <SectionHead :title="t('settings.shortcuts.overlayTitle')" :desc="t('settings.shortcuts.overlayDesc')" />
  </div>
  <div class="g-group">
    <div class="p-keyref">
      <div>
        <h3>{{ t("settings.shortcuts.overlayTools") }}</h3>
        <div v-for="row in TOOLS" :key="row.name" class="p-keyrow"><span>{{ t(row.name) }}</span><KeyCaps :caps="row.caps" /></div>
      </div>
      <div>
        <h3>{{ t("settings.shortcuts.overlayActions") }}</h3>
        <div v-for="row in ACTIONS" :key="row.name" class="p-keyrow"><span>{{ t(row.name) }}</span><KeyCaps :caps="row.caps" /></div>
        <h3>{{ t("settings.shortcuts.overlaySelection") }}</h3>
        <div v-for="row in SELECTION" :key="row.name" class="p-keyrow"><span>{{ t(row.name) }}</span><KeyCaps :caps="row.caps" /></div>
        <h3>{{ t("settings.shortcuts.overlayDrawings") }}</h3>
        <div v-for="row in DRAWINGS" :key="row.name" class="p-keyrow"><span>{{ t(row.name) }}</span><KeyCaps :caps="row.caps" /></div>
      </div>
    </div>
  </div>
</template>
