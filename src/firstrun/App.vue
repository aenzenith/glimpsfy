<script setup lang="ts">
// First run (DESIGN.md "First run"), an ordinary window in two steps.
//
// Step 1 lists the three permissions with their live state: the page asks `permissions_status`
// about every 1.5 s while the window is visible and listens to `permissions://changed`. Screen
// recording is required, so Continue stays disabled until it is granted; when macOS granted it
// while Glimpsfy ran, the row asks for a restart (`app_restart`). A block below says plainly what
// stays on this Mac. The camera is left out here: nothing asks for it before recordings with a
// webcam exist, and Settings shows its state.
//
// Step 2 picks the capture shortcut: Glimpsfy's own keys, or the macOS area keys (⇧⌘4), which
// Glimpsfy takes for Capture area when the user presses Done. Glimpsfy never changes the system's
// settings; it only explains the Keyboard Shortcuts step and opens that pane. Start at login is
// saved at once and shows macOS's refusal. Done saves `general.firstRunDone` and closes.
//
// Elsewhere the steps follow the platform (`can`): Windows and an X11 desktop ask no screen
// recording permission and a Wayland desktop asks by itself at the first capture, so step 1 lists
// only the permissions the system does ask for; the system keys are Print Screen; and where the
// desktop gives no global shortcuts (Wayland), step 2 hands over the command to bind instead.
import { getCurrentWindow } from "@tauri-apps/api/window";
import { computed, onMounted, onUnmounted, reactive, ref, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import KeyCaps from "../components/KeyCaps.vue";
import ProgressRing from "../components/ProgressRing.vue";
import SwitchControl from "../components/SwitchControl.vue";
import { on } from "../lib/api/core";
import {
  permissions_open_settings,
  permissions_request,
  permissions_status,
  type Permission,
  type Permissions,
} from "../lib/api/permissions";
import { settings_get, settings_set } from "../lib/api/settings";
import { app_copy_text, app_open_system_settings, app_open_url, app_restart } from "../lib/api/windows";
import { PERMISSIONS_CHANGED, SETTINGS_CHANGED } from "../lib/events";
import { t, tp, type Key } from "../lib/i18n";
import { capsOf, capsText, joinCaps } from "../lib/keys";
import { can, isMac, platform } from "../lib/platform";
import type { Settings } from "../lib/settings";
import { tr } from "../settings/text";
import { codeOf, reasonText } from "../toast/reason";

/** How often step 1 asks for the permissions while the window is visible. */
const POLL_MS = 1500;
/** The system's area screenshot keys, which "Replace the macOS keys" gives to Capture area: ⇧⌘4
 * on macOS, and Print Screen on Windows and Linux, whose screenshot tools take that key. */
const MAC_AREA_KEYS = isMac() ? "Shift+CommandOrControl+4" : "PrintScreen";
/** The desktop gives no global shortcuts (Wayland), so step 2 offers the command to bind. */
const commandsOnly = !can("globalShortcuts");
const AREA_COMMAND = "glimpsfy --action capture-area";
/** The public source (Cargo.toml `repository`), for the privacy block's link. */
const SOURCE_URL = "https://github.com/aenzenith/glimpsfy";
const dev = import.meta.env.DEV;

type Row = { id: Exclude<Permission, "camera">; icon: string; pane: Key };
const ROWS: Row[] = [
  { id: "screenRecording", icon: "display", pane: "firstrun.illus.screenRecording" },
  { id: "microphone", icon: "microphone", pane: "firstrun.illus.microphone" },
  { id: "accessibility", icon: "accessibility", pane: "firstrun.illus.accessibility" },
];
type RowState = "none" | "waiting" | "restart" | "granted" | "notRequired";
/** Where the system asks nothing for screen capture (Windows, X11) or asks by itself at the first
 * capture (Wayland), step 1 has no screen recording row and nothing holds Continue back. */
const screenAsked = can("screenRecordingPermission");

const FACTS: Key[] = ["firstrun.privacy.account", "firstrun.privacy.upload", "firstrun.privacy.ocr", "firstrun.privacy.screen"];

const step = ref<1 | 2>(1);
const permissions = ref<Permissions | null>(null);
/** Permissions whose System Settings pane this window opened, until they are granted. */
const waiting = reactive(new Set<Row["id"]>());
const actionError = ref<string | null>(null);

function rowState(id: Row["id"]): RowState {
  const state = permissions.value?.[id];
  if (state === "granted") return "granted";
  if (state === "notRequired") return "notRequired";
  if (id === "screenRecording" && permissions.value?.restartNeeded) return "restart";
  return waiting.has(id) ? "waiting" : "none";
}

/** The rows the system asks for: a permission it does not need (`notRequired`) is left out. Until
 * the first answer, the platform's usual rows stand in. */
const rows = computed(() =>
  ROWS.filter((row) => {
    if (row.id === "screenRecording" && !screenAsked) return false;
    const state = permissions.value?.[row.id];
    if (state) return state !== "notRequired";
    return isMac() || (row.id === "microphone" && platform.os === "windows");
  }),
);
const lead = computed(() => (platform.session === "wayland" ? t("firstrun.welcome.leadWayland") : tp("firstrun.welcome.lead")));

const screenState = computed(() => rowState("screenRecording"));
const canContinue = computed(() => !screenAsked || screenState.value === "granted" || screenState.value === "notRequired");
const hint = computed<Key>(() =>
  !rows.value.length
    ? "firstrun.hint.none"
    : canContinue.value
      ? "firstrun.hint.ok"
      : screenState.value === "restart"
        ? "firstrun.hint.restart"
        : "firstrun.hint.blocked",
);

const tag = (id: Row["id"]) => `firstrun.perm.${id}.tag` as Key;
const name = (id: Row["id"]) => `firstrun.perm.${id}.name` as Key;
const why = (id: Row["id"]) => `firstrun.perm.${id}.why` as Key;
const stateKey = (state: RowState) => `firstrun.state.${state}` as Key;
const TONE: Record<RowState, string | undefined> = { none: undefined, waiting: "iris", restart: "warn", granted: "ok", notRequired: "ok" };

// ------------------------------------------------------------------------------- permissions

let asking = false;
async function refresh(): Promise<void> {
  if (asking) return;
  asking = true;
  try {
    permissions.value = await permissions_status();
  } catch (error) {
    console.warn("permissions_status", error);
  } finally {
    asking = false;
  }
}

watch(permissions, (next) => {
  for (const id of [...waiting]) if (next?.[id] === "granted") waiting.delete(id);
});

/** Asks for the permission (macOS adds Glimpsfy to the list, or shows its prompt) and, when it is
 * not granted at once, opens the pane where the user turns it on. */
async function allow(id: Row["id"]): Promise<void> {
  actionError.value = null;
  try {
    const state = await permissions_request(id);
    // The microphone asks in a system prompt first; its pane opens only after a refusal.
    if (state !== "granted" && !(id === "microphone" && state === "notDetermined")) {
      await permissions_open_settings(id);
      waiting.add(id);
    }
    await refresh();
  } catch (error) {
    actionError.value = reasonText(codeOf(error));
  }
}

async function openAgain(id: Row["id"]): Promise<void> {
  actionError.value = null;
  try {
    await permissions_open_settings(id);
  } catch (error) {
    actionError.value = reasonText(codeOf(error));
  }
}

async function restart(): Promise<void> {
  try {
    await app_restart();
  } catch (error) {
    actionError.value = reasonText(codeOf(error));
  }
}

function openSource(): void {
  app_open_url(SOURCE_URL).catch((error) => (actionError.value = reasonText(codeOf(error))));
}

// ------------------------------------------------------------------------- step 2: shortcut

const settings = ref<Settings | null>(null);
const choice = ref<"ours" | "replace">("ours");
/** The area keys before this window changed them, so choosing "ours" again restores them. */
let originalArea: string | null | undefined;
let replaced = false;
const login = ref(false);
const loginRefused = ref(false);
const saving = ref(false);
const saveError = ref<string | null>(null);

const ourKeys = computed(() => {
  const area = settings.value?.shortcuts.captureArea ?? null;
  return replaced && area === MAC_AREA_KEYS ? (originalArea ?? null) : area;
});
const tryKeys = computed(() => (commandsOnly ? [] : capsOf(choice.value === "replace" ? MAC_AREA_KEYS : ourKeys.value)));
const commandCopied = ref(false);
async function copyCommand(): Promise<void> {
  try {
    await app_copy_text(AREA_COMMAND);
    commandCopied.value = true;
    setTimeout(() => (commandCopied.value = false), 1600);
  } catch (error) {
    saveError.value = reasonText(codeOf(error));
  }
}
const tryParts = computed(() => {
  const [before = "", after = ""] = t("firstrun.fin.tryKeys").split("{keys}");
  return { before, after };
});

function takeSettings(next: Settings): void {
  settings.value = next;
  login.value = next.general.startAtLogin;
  if (originalArea === undefined) originalArea = next.shortcuts.captureArea;
}

async function loadSettings(): Promise<void> {
  try {
    takeSettings(await settings_get());
  } catch (error) {
    console.warn("settings_get", error);
  }
}

async function setLogin(value: boolean): Promise<void> {
  login.value = value;
  loginRefused.value = false;
  saveError.value = null;
  try {
    const current = await settings_get();
    current.general.startAtLogin = value;
    takeSettings((await settings_set(current)).settings);
  } catch (error) {
    // A refused login item fails with `io` and the setting stays off.
    login.value = false;
    if (value && codeOf(error) === "io") loginRefused.value = true;
    else saveError.value = `${t("firstrun.fin.saveFailed")} ${reasonText(codeOf(error))}`;
  }
}

async function openPane(pane: "keyboardShortcuts" | "loginItems"): Promise<void> {
  try {
    await app_open_system_settings(pane);
  } catch (error) {
    saveError.value = reasonText(codeOf(error));
  }
}

async function finish(): Promise<void> {
  saving.value = true;
  saveError.value = null;
  try {
    const current = await settings_get();
    current.general.firstRunDone = true;
    if (choice.value === "replace") {
      current.shortcuts.captureArea = MAC_AREA_KEYS;
      replaced = true;
    } else if (replaced && current.shortcuts.captureArea === MAC_AREA_KEYS) {
      current.shortcuts.captureArea = originalArea ?? null;
    }
    const saved = await settings_set(current);
    takeSettings(saved.settings);
    const taken = choice.value === "replace" ? saved.clearedShortcuts.find((c) => c.action === "captureArea") : undefined;
    if (taken) {
      // Another action held the keys first, so the save cleared Capture area. It gets its own
      // keys back, so a refused replace never leaves the main action without a shortcut.
      const restored = { ...saved.settings, shortcuts: { ...saved.settings.shortcuts, captureArea: originalArea ?? null } };
      takeSettings((await settings_set(restored)).settings);
      replaced = false;
      const other = tr(`settings.shortcuts.action.${taken.keptBy}`, {}, taken.keptBy);
      saveError.value = t("firstrun.fin.keysTaken", { keys: capsText(MAC_AREA_KEYS), other });
      return;
    }
    if (choice.value === "replace" && saved.shortcutProblems.some((p) => p.action === "captureArea")) {
      saveError.value = tp("firstrun.fin.keysRefused", { keys: capsText(MAC_AREA_KEYS) });
      return;
    }
    await getCurrentWindow().close();
  } catch (error) {
    saveError.value = `${t("firstrun.fin.saveFailed")} ${reasonText(codeOf(error))}`;
  } finally {
    saving.value = false;
  }
}

// ------------------------------------------------------------------------------------ wiring

const stops: (() => void)[] = [];
let poll: ReturnType<typeof setInterval> | null = null;

function tick(): void {
  if (step.value === 1 && document.visibilityState === "visible") void refresh();
}

onMounted(async () => {
  poll = setInterval(tick, POLL_MS);
  window.addEventListener("focus", tick);
  stops.push(
    await on<Permissions>(PERMISSIONS_CHANGED, (next) => (permissions.value = next)),
    await on<Settings>(SETTINGS_CHANGED, takeSettings),
  );
  await Promise.all([refresh(), loadSettings()]);
});

onUnmounted(() => {
  if (poll) clearInterval(poll);
  window.removeEventListener("focus", tick);
  stops.forEach((stop) => stop());
});
</script>

<template>
  <main class="s-fr">
    <!-- step 1: permissions -->
    <div v-if="step === 1" class="s-fr-body">
      <div class="s-fr-hero">
        <span class="s-appicon"><GIcon name="glyph" :size="32" /></span>
        <div>
          <h1 class="g-page-title">{{ t("firstrun.welcome.title") }}</h1>
          <p class="s-fr-lead">{{ lead }}</p>
        </div>
      </div>

      <div v-if="rows.length" class="g-group">
        <div v-for="row in rows" :key="row.id" class="s-perm" :data-perm="row.id">
          <span class="s-perm-icon"><GIcon :name="row.icon" :size="20" /></span>
          <div>
            <div class="s-perm-name">
              <span>{{ t(name(row.id)) }}</span>
              <span class="g-badge" :data-tone="row.id === 'screenRecording' ? 'iris' : undefined">{{ t(tag(row.id)) }}</span>
            </div>
            <p class="s-perm-why">{{ t(why(row.id)) }}</p>
            <p class="s-perm-state" :data-tone="TONE[rowState(row.id)]" aria-live="polite">
              <span v-if="rowState(row.id) === 'none'" class="s-open-dot" />
              <ProgressRing v-else-if="rowState(row.id) === 'waiting'" :progress="null" :label="tp('firstrun.state.waiting')" :size="14" />
              <GIcon v-else-if="rowState(row.id) === 'restart'" name="warning" :size="14" />
              <GIcon v-else name="check" :size="14" />
              <span>{{ tp(stateKey(rowState(row.id))) }}</span>
            </p>
          </div>
          <div class="s-perm-act">
            <button
              v-if="rowState(row.id) === 'none'"
              type="button"
              class="g-button"
              :data-variant="row.id === 'screenRecording' ? 'primary' : undefined"
              @click="allow(row.id)"
            >
              {{ row.id === "microphone" ? t("firstrun.btn.ask") : tp("firstrun.btn.open") }}
            </button>
            <button v-else-if="rowState(row.id) === 'waiting'" type="button" class="g-button" data-variant="plain" @click="openAgain(row.id)">
              {{ t("firstrun.btn.again") }}
            </button>
            <button v-else-if="rowState(row.id) === 'restart'" type="button" class="g-button" data-variant="primary" @click="restart">
              <GIcon name="retry" :size="16" />{{ t("firstrun.btn.restart") }}
            </button>
          </div>
          <div v-if="rowState(row.id) === 'waiting'" class="s-perm-more">
            <!-- The System Settings list, drawn small, so the user knows what to look for. -->
            <div v-if="isMac()" class="s-illus" aria-hidden="true">
              <div class="s-illus-side">
                <span class="s-illus-lights"><i /><i /><i /></span>
                <span class="s-illus-nav">{{ t("firstrun.illus.general") }}</span>
                <span class="s-illus-nav">{{ t("firstrun.illus.appearance") }}</span>
                <span class="s-illus-nav" data-on>{{ t("firstrun.illus.privacy") }}</span>
                <span class="s-illus-nav">{{ t("firstrun.illus.keyboard") }}</span>
              </div>
              <div class="s-illus-main">
                <span class="s-illus-head"><GIcon name="chevron-left" :size="14" />{{ t(row.pane) }}</span>
                <span class="s-illus-row"><i class="s-illus-app" />{{ t("firstrun.illus.other") }}<span class="s-illus-sw" data-on /></span>
                <span class="s-illus-row" data-target>
                  <i class="s-illus-app" data-glimpsfy><GIcon name="glyph" :size="12" /></i>Glimpsfy<span class="s-illus-sw" />
                </span>
              </div>
            </div>
            <p class="g-help">{{ tp("firstrun.wait") }}</p>
          </div>
          <div v-else-if="rowState(row.id) === 'restart'" class="s-perm-more s-perm-more-tight">
            <p class="g-help">{{ t("firstrun.restartWhy") }}</p>
          </div>
          <div v-if="dev && row.id === 'screenRecording' && !canContinue" class="s-perm-more s-perm-more-tight">
            <p class="g-help">{{ t("firstrun.dev") }}</p>
          </div>
        </div>
      </div>

      <p v-if="actionError" class="g-help" data-tone="warn" role="alert">
        <GIcon name="warning" :size="14" />{{ actionError }}
      </p>

      <div class="s-privacy">
        <span class="s-privacy-icon"><GIcon name="shield" :size="20" /></span>
        <div>
          <h2 class="s-privacy-title">{{ tp("firstrun.privacy.title") }}</h2>
          <ul class="s-facts">
            <li v-for="fact in FACTS" :key="fact"><GIcon name="check" :size="14" /><span>{{ tp(fact) }}</span></li>
          </ul>
          <p class="s-privacy-src">
            {{ t("firstrun.privacy.source") }}
            <a class="s-link" href="#" @click.prevent="openSource">{{ t("firstrun.privacy.sourceLink") }}</a>
          </p>
        </div>
      </div>
    </div>

    <!-- step 2: the capture shortcut -->
    <div v-else class="s-fr-body">
      <div class="s-fr-hero">
        <span class="s-appicon"><GIcon name="glyph" :size="32" /></span>
        <div>
          <h1 class="g-page-title">{{ commandsOnly ? t("firstrun.fin.titleCommand") : t("firstrun.fin.title") }}</h1>
          <p class="s-fr-lead">{{ commandsOnly ? t("firstrun.fin.leadCommand") : t("firstrun.fin.lead") }}</p>
        </div>
      </div>

      <div v-if="commandsOnly" class="s-replace">
        <input class="g-field" data-kind="mono" readonly :value="AREA_COMMAND" :aria-label="t('firstrun.fin.command')" @focus="($event.target as HTMLInputElement).select()" />
        <button type="button" class="g-button" @click="copyCommand">
          <GIcon :name="commandCopied ? 'check' : 'copy'" :size="16" />{{ t("firstrun.fin.commandCopy") }}
        </button>
      </div>
      <template v-else>
        <div role="radiogroup" :aria-label="t('firstrun.fin.title')" class="s-choices2">
          <label class="s-choice-card">
            <input v-model="choice" type="radio" class="g-radio" name="keyset" value="ours" />
            <span class="s-choice-title">{{ t("firstrun.fin.ours") }}</span>
            <KeyCaps :accelerator="ourKeys" />
            <span class="g-help">{{ tp("firstrun.fin.oursHelp") }}</span>
          </label>
          <label class="s-choice-card">
            <input v-model="choice" type="radio" class="g-radio" name="keyset" value="replace" />
            <span class="s-choice-title">{{ tp("firstrun.fin.replace") }}</span>
            <KeyCaps :accelerator="MAC_AREA_KEYS" />
            <span class="g-help">{{ tp("firstrun.fin.replaceHelp") }}</span>
          </label>
        </div>
        <div v-if="choice === 'replace'" class="s-replace">
          <p class="g-help">{{ tp("firstrun.fin.replaceSteps") }}</p>
          <button v-if="can('systemSettingsPanes')" type="button" class="g-button" @click="openPane('keyboardShortcuts')">
            <GIcon name="keyboard" :size="16" />{{ tp("firstrun.fin.replaceOpen") }}
          </button>
        </div>
      </template>

      <div class="g-group">
        <div class="g-form-row">
          <div class="g-form-label">
            <label for="fr-login">{{ t("firstrun.fin.login") }}</label>
            <span class="g-help">{{ tp("firstrun.fin.loginHelp") }}</span>
            <template v-if="loginRefused">
              <span class="g-help" data-tone="warn"><GIcon name="warning" :size="14" />{{ tp("firstrun.fin.loginRefused") }}</span>
              <button v-if="can('systemSettingsPanes')" type="button" class="g-button s-login-open" @click="openPane('loginItems')">
                {{ tp("firstrun.fin.loginOpen") }}
              </button>
            </template>
          </div>
          <div class="g-form-control">
            <SwitchControl id="fr-login" :model-value="login" @update:model-value="setLogin" />
          </div>
        </div>
      </div>

      <div class="s-try">
        <span v-if="tryKeys.length" class="s-try-keys" aria-hidden="true">
          <span v-for="(cap, i) in tryKeys" :key="i" class="s-keycap">{{ cap }}</span>
        </span>
        <span class="s-try-text">
          <b>{{ t("firstrun.fin.tryTitle") }}</b>
          <span v-if="tryKeys.length">{{ tryParts.before }}{{ joinCaps(tryKeys) }}{{ tryParts.after }}</span>
          <span v-else>{{ commandsOnly ? t("firstrun.fin.tryCommand") : tp("firstrun.fin.tryNoKeys") }}</span>
        </span>
      </div>

      <p v-if="saveError" class="g-help" data-tone="warn" role="alert"><GIcon name="warning" :size="14" />{{ saveError }}</p>
    </div>

    <footer class="s-fr-foot">
      <span class="s-steps" aria-hidden="true"><i :data-on="step === 1 ? '' : undefined" /><i :data-on="step === 2 ? '' : undefined" /></span>
      <span class="s-fr-step">{{ step === 1 ? t("firstrun.step1") : t("firstrun.step2") }}</span>
      <template v-if="step === 1">
        <p class="g-help">{{ t(hint) }}</p>
        <button type="button" class="g-button" data-variant="primary" data-size="large" :disabled="!canContinue" @click="step = 2">
          {{ t("firstrun.continue") }}
        </button>
      </template>
      <template v-else>
        <span class="g-spacer" />
        <button type="button" class="g-button" data-variant="plain" data-size="large" @click="step = 1">{{ t("firstrun.back") }}</button>
        <button type="button" class="g-button" data-variant="primary" data-size="large" :disabled="saving" @click="finish">
          {{ t("firstrun.done") }}
        </button>
      </template>
    </footer>
  </main>
</template>

<style>
/* The first-run window's own parts, ported from design/round-2/small-surfaces (s-fr*). The
   window is its own page, so these rules reach nothing else. */
.s-fr { display: flex; flex-direction: column; height: 100%; }
.s-fr :where(p, h1, h2, ul) { margin: 0; padding: 0; }
.s-fr ul { list-style: none; }
.s-fr-body { flex: 1; min-height: 0; overflow-y: auto; display: flex; flex-direction: column; gap: 18px; padding: 20px 28px 24px; }
.s-fr-hero { display: flex; align-items: center; gap: 16px; }
.s-appicon { flex: none; display: grid; place-items: center; width: 56px; height: 56px; border-radius: 14px; background: var(--iris); color: var(--text-on-iris); }
.s-fr-lead { margin: 4px 0 0; max-width: 440px; font: var(--type-text); color: var(--win-text-2); }

.s-perm { position: relative; display: grid; grid-template-columns: 36px minmax(0, 1fr) auto; column-gap: 12px; align-items: start; padding: 14px 14px 14px 12px; }
.s-perm + .s-perm::before { content: ""; position: absolute; left: 60px; right: 0; top: 0; height: var(--hairline); background: var(--win-line); }
.s-perm-icon { display: grid; place-items: center; width: 36px; height: 36px; border-radius: var(--radius-field); background: var(--win-selection-soft); color: var(--win-iris-text); }
.s-perm-name { display: flex; align-items: center; gap: 8px; min-height: 20px; font: var(--type-title); color: var(--win-text); }
.s-perm-why { margin: 2px 0 0; font: var(--type-body); color: var(--win-text-2); }
.s-perm-state { display: flex; align-items: center; gap: 6px; margin: 6px 0 0; font: var(--type-label); color: var(--win-text-2); }
.s-perm-state[data-tone="ok"] { color: var(--win-ok); }
.s-perm-state[data-tone="warn"] { color: var(--win-warn); }
.s-perm-state[data-tone="iris"] { color: var(--win-iris-text); }
.s-perm-state .g-ring { --ring-size: 14px; }
.s-open-dot { flex: none; width: 10px; height: 10px; margin: 0 2px; border-radius: 50%; box-shadow: inset 0 0 0 1.5px currentColor; }
.s-perm-act { align-self: center; display: flex; gap: 8px; }
.s-perm-more { grid-column: 2 / 4; margin-top: 12px; }
.s-perm-more > .g-help { margin-top: 10px; }
.s-perm-more-tight { margin-top: 6px; }
.s-perm-more-tight > .g-help { margin-top: 0; }

.s-privacy { display: grid; grid-template-columns: 36px minmax(0, 1fr); column-gap: 12px; padding: 14px 16px 14px 12px; border-radius: var(--radius-group); box-shadow: inset 0 0 0 1px var(--win-line); }
.s-privacy-icon { display: grid; place-items: center; width: 36px; height: 36px; border-radius: var(--radius-field); background: var(--win-ok-soft); color: var(--win-ok); }
.s-privacy-title { font: var(--type-title); color: var(--win-text); }
.s-facts { display: grid; gap: 5px; margin-top: 6px; }
.s-facts li { display: grid; grid-template-columns: 14px minmax(0, 1fr); gap: 8px; font: var(--type-body); color: var(--win-text-2); }
.s-facts li > g-icon { margin-top: 1px; color: var(--win-ok); }
.s-privacy-src { margin-top: 8px; font: var(--type-body); color: var(--win-text-2); }
.s-link { color: var(--win-iris-text); font-weight: 500; text-decoration: none; border-radius: 3px; white-space: nowrap; }
.s-link:hover { text-decoration: underline; }
.s-link:focus { outline: 0; }
.s-link:focus-visible { outline: 2px solid var(--surface-focus); outline-offset: 2px; }

.s-fr-foot { flex: none; display: flex; align-items: center; gap: 12px; min-height: 60px; padding: 0 16px 0 28px; background: var(--win-ground); box-shadow: inset 0 var(--hairline) 0 var(--win-line); }
.s-steps { display: inline-flex; gap: 6px; }
.s-steps > i { width: 6px; height: 6px; border-radius: 50%; background: var(--win-track); }
.s-steps > i[data-on] { background: var(--iris); }
.s-fr-step { font: var(--type-caption); color: var(--win-text-3); white-space: nowrap; }
.s-fr-foot > .g-help { flex: 1; margin: 0; text-align: right; }

.s-illus { display: grid; grid-template-columns: 128px minmax(0, 1fr); height: 128px; border-radius: 10px; overflow: hidden; background: var(--win-content); box-shadow: 0 0 0 0.5px var(--win-line-strong), var(--win-shadow-lift); font: var(--type-caption); }
.s-illus-side { display: flex; flex-direction: column; gap: 2px; padding: 9px 8px; background: var(--win-ground); box-shadow: inset calc(-1 * var(--hairline)) 0 0 var(--win-line); }
.s-illus-lights { display: flex; gap: 5px; margin: 0 0 8px 3px; }
.s-illus-lights > i { width: 7px; height: 7px; border-radius: 50%; background: var(--win-track); }
.s-illus-nav { padding: 2px 7px; border-radius: 5px; color: var(--win-text-2); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.s-illus-nav[data-on] { background: var(--win-selection); color: var(--win-selection-text); }
.s-illus-main { position: relative; display: flex; flex-direction: column; gap: 6px; min-width: 0; padding: 9px 12px; }
.s-illus-head { display: flex; align-items: center; gap: 4px; font: var(--type-caption-strong); color: var(--win-text); white-space: nowrap; }
.s-illus-head > g-icon { color: var(--win-text-3); }
.s-illus-row { position: relative; display: flex; align-items: center; gap: 8px; height: 30px; padding: 0 8px; border-radius: 6px; background: var(--win-group); box-shadow: inset 0 0 0 0.5px var(--win-group-edge); color: var(--win-text); }
.s-illus-app { flex: none; display: grid; place-items: center; width: 16px; height: 16px; border-radius: 4px; background: var(--win-track); }
.s-illus-app[data-glimpsfy] { background: var(--iris); color: var(--text-on-iris); }
.s-illus-sw { position: relative; margin-left: auto; width: 26px; height: 15px; border-radius: 8px; background: var(--win-track); }
.s-illus-sw::before { content: ""; position: absolute; left: 2px; top: 2px; width: 11px; height: 11px; border-radius: 50%; background: var(--handle); box-shadow: var(--win-thumb-shadow); }
.s-illus-sw[data-on] { background: var(--iris); }
.s-illus-sw[data-on]::before { transform: translateX(11px); }
.s-illus-row[data-target] .s-illus-sw { box-shadow: 0 0 0 2px var(--win-content), 0 0 0 4px var(--iris); animation: s-ping 1.6s var(--ease-out) infinite; }
@keyframes s-ping {
  0%, 100% { box-shadow: 0 0 0 2px var(--win-content), 0 0 0 4px var(--iris); }
  50% { box-shadow: 0 0 0 2px var(--win-content), 0 0 0 7px var(--iris-soft); }
}
@media (prefers-reduced-motion: reduce) {
  .s-illus-row[data-target] .s-illus-sw { animation: none; }
}

.s-choices2 { display: grid; grid-template-columns: 1fr 1fr; gap: 10px; }
.s-choice-card { position: relative; display: grid; grid-template-columns: 16px minmax(0, 1fr); column-gap: 10px; row-gap: 6px; align-content: start; padding: 12px 12px 14px; border-radius: var(--radius-group); background: var(--win-group); box-shadow: inset 0 0 0 0.5px var(--win-group-edge); cursor: default; }
.s-choice-card:has(input:checked) { background: var(--win-selection-soft); box-shadow: inset 0 0 0 2px var(--iris); }
.s-choice-card > .g-radio { margin-top: 1px; }
.s-choice-title { font: var(--type-title); color: var(--win-text); }
.s-choice-card > .g-keys, .s-choice-card > .g-help { grid-column: 2; margin-top: 0; }
.s-replace { display: flex; flex-direction: column; align-items: flex-start; gap: 10px; margin-top: -8px; }
.s-login-open { margin-top: 8px; align-self: flex-start; }
.s-try { display: flex; align-items: center; gap: 18px; padding: 16px 18px; border-radius: var(--radius-group); background: var(--win-selection-soft); }
.s-try-keys { display: inline-flex; gap: 6px; flex: none; }
.s-keycap { display: inline-grid; place-items: center; min-width: 40px; height: 40px; padding: 0 10px; border-radius: var(--radius-field); background: var(--win-raised); color: var(--win-text); box-shadow: var(--win-button-shadow), inset 0 -2px 0 var(--win-line); font: var(--type-heading); }
.s-try-text b { display: block; font: var(--type-title); color: var(--win-text); }
.s-try-text span { display: block; margin-top: 2px; font: var(--type-body); color: var(--win-text-2); }
</style>
