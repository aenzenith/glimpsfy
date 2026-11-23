<script setup lang="ts">
// The recorder's bar window (`rec-bar`): the setup bar while a setup is open and nothing records,
// the control bar while a take runs, pauses or finishes, and nothing during the countdown (the
// core hides the window then). The window is exactly as large as the bar and whatever hangs from
// it (a popover, a device menu, a failure sentence or a tooltip): the page measures both and calls
// `record_bar_fit`, and the core sizes and places the window and says on which side the popover
// hangs. The bar keeps the window's right edge, so it grows to the left and Record and Stop keep
// their place in every language. The window sits over the live screen, where `backdrop-filter`
// blurs nothing and a shadow falls outside the window, so the page also reports how its glass is
// drawn and the core lays a native blur, the kit's shadow and the outer ring under it.
//
// The window is a stage with room for whatever hangs from the bar, so opening a popover or a tip
// never moves or grows it, and the core shows it only once the page lays the bar out for the
// side it chose (`record://bar-layout`). A popover that closes leaves the page only after the
// core took its native glass away, so no blur shows for a moment with nothing over it.
//
// Tooltips are drawn by this page in the popover's place instead of the kit's `g-tipwrap`, because
// a tip outside the bar would fall outside the window and never show.
import { computed, nextTick, onMounted, onUnmounted, reactive, ref, shallowRef, watch } from "vue";

import GIcon from "../components/GIcon.vue";
import KeyCaps from "../components/KeyCaps.vue";
import { on } from "../lib/api/core";
import { permissions_open_settings, permissions_request, permissions_status, type Permissions, type PermissionState } from "../lib/api/permissions";
import {
  onRecordBarLayout,
  onRecordLevels,
  onRecordPresent,
  onRecordSetup,
  onRecordState,
  onRecordTick,
  record_bar_fit,
  record_devices,
  record_discard,
  record_keep,
  record_meter,
  record_pause,
  record_present,
  record_present_state,
  record_resume,
  record_set_microphone_muted,
  record_setup_close,
  record_setup_state,
  record_state,
  record_setup_update,
  record_start,
  record_stop,
  type BarGlass,
  type BarLayout,
  type MeterLevels,
  type PresentChange,
  type PresentState,
  type RecordingDevices,
  type RecordingState,
  type RecordSetup,
  type Resolution,
  type SetupUpdate,
  type TargetMode,
} from "../lib/api/record";
import { settings_get, settings_set } from "../lib/api/settings";
import { PERMISSIONS_CHANGED, SETTINGS_CHANGED } from "../lib/events";
import { t } from "../lib/i18n";
import { capsText } from "../lib/keys";
import type { Recording, Settings } from "../lib/settings";
import ControlBar, { type ControlAction } from "./ControlBar.vue";
import DeviceMenu from "./DeviceMenu.vue";
import { clamp } from "./geometry";
import { clock, codeOf, errorText, type ErrorPlace } from "./format";
import OptionsPop from "./OptionsPop.vue";
import PermissionPop from "./PermissionPop.vue";
import PresentPop from "./PresentPop.vue";
import SetupBar, { type SetupAction } from "./SetupBar.vue";

type Popover = "more" | "mic" | "cam" | "permMic" | "permCam" | "present" | "error";

/** The gap between the bar and what hangs from it. */
const GAP = 8;
/** The kit's tooltip delay. */
const TIP_DELAY = 500;
/** The kit's glass that casts `--shadow-small` instead of `--shadow-float` (`components.css`). */
const SMALL_SHADOW = '.g-tip, .g-size, .g-hint, .g-tag, .g-glass[data-material="strong"]';

const setup = ref<RecordSetup | null>(null);
const state = ref<RecordingState>({ kind: "idle" });
const settings = ref<Settings | null>(null);
const devices = ref<RecordingDevices | null>(null);
const perms = reactive<{ microphone: PermissionState; camera: PermissionState }>({ microphone: "notDetermined", camera: "notDetermined" });
const levels = shallowRef<MeterLevels>({ microphones: {}, system: null });
const popover = ref<Popover | null>(null);
/** Drawing and the spotlight on the take, `null` without one. */
const present = shallowRef<PresentState | null>(null);
const problem = ref("");
const side = ref<"below" | "above">("below");
/** The bar window's size as the core made it, 0 until it has. */
const stage = reactive({ width: 0, height: 0 });
const starting = ref(false);
const stopping = ref(false);

const barEl = ref<HTMLElement | null>(null);
const popEl = ref<HTMLElement | null>(null);
const geom = reactive({ barW: 0, barH: 40, popW: 0, popH: 0, popRight: 0 });

const stops: (() => void)[] = [];

// ------------------------------------------------------------------------------ what shows

const view = computed<"setup" | "control" | null>(() => {
  const kind = state.value.kind;
  if (kind === "recording" || kind === "paused" || kind === "finishing") return "control";
  if (kind === "idle" && setup.value) return "setup";
  return null;
});

const granted = (p: PermissionState) => p === "granted" || p === "notRequired";
const rec = computed<Recording | null>(() => settings.value?.recording ?? null);

const systemAvailable = computed(() => devices.value?.systemAudio !== false);
const systemOn = computed(() => !!rec.value?.systemAudio && systemAvailable.value);
const micDenied = computed(() => perms.microphone === "denied");
// The microphone of this take: the setup starts from Settings › Recording, and a change in the
// bar holds for this take only.
const micOn = computed(() => !!setup.value?.microphone && granted(perms.microphone));
const cameraDenied = computed(() => perms.camera === "denied");
const cameraOn = computed(() => !!setup.value?.camera.on && !cameraDenied.value);

const defaultOf = (list: readonly { id: string; isDefault: boolean }[] | undefined) =>
  list?.find((d) => d.isDefault)?.id ?? list?.[0]?.id ?? null;
/** `null` in the settings means the system's default input. */
const chosenMic = computed(() => rec.value?.microphoneDevice ?? defaultOf(devices.value?.microphones));
const chosenCamera = computed(() => setup.value?.camera.deviceId ?? rec.value?.webcamDevice ?? defaultOf(devices.value?.cameras));

const canRecord = computed(() => !!setup.value?.target && !starting.value);

const setupModel = computed(() => ({
  system: { on: systemOn.value, available: systemAvailable.value, level: systemOn.value ? (levels.value.system ?? 0) : 0 },
  mic: {
    on: micOn.value,
    denied: micDenied.value,
    level: micOn.value && chosenMic.value ? (levels.value.microphones[chosenMic.value] ?? 0) : 0,
  },
  camera: { on: cameraOn.value, denied: cameraDenied.value },
}));

// The take: the clock runs on between the core's ticks and never goes backwards while it runs.
const take = reactive({ id: "", ms: 0, at: 0, running: false, shown: 0, micLevel: 0 });
let clockTimer = 0;

const paused = computed(() => state.value.kind === "paused");
const asked = computed(() => state.value.kind === "paused" && state.value.discardAsked);
const muted = computed(() => (state.value.kind === "recording" || state.value.kind === "paused" ? state.value.microphoneMuted : false));
const takeHasMic = computed(() => (state.value.kind === "recording" || state.value.kind === "paused" ? state.value.microphone : false));

/** The clock as it reads, to the second. The ticks and the timer move `take.shown` eight times a
 * second; Vue passes a computed on only when its value changed, so the bar draws once a second
 * for the clock (and on each tick for a live microphone meter). */
const time = computed(() => clock(take.shown));

const controlModel = computed(() => ({
  time: time.value,
  paused: paused.value,
  finishing: state.value.kind === "finishing" || stopping.value,
  mic: { recorded: takeHasMic.value, live: !muted.value, level: !muted.value && state.value.kind === "recording" ? take.micLevel : 0 },
  asked: asked.value,
  present: {
    shown: true,
    pressed: popover.value === "present" || !!present.value?.drawing || !!present.value?.spotlight,
  },
  keys: {
    mute: capsText(settings.value?.shortcuts.muteMicrophone),
    pause: capsText(settings.value?.shortcuts.pauseRecording),
    stop: capsText(setup.value?.mode === "display" ? settings.value?.shortcuts.recordDisplay : settings.value?.shortcuts.recordScreen),
  },
}));

function tickClock(): void {
  const live = take.ms + Math.min(1500, performance.now() - take.at);
  take.shown = take.running ? Math.max(take.shown, live) : take.ms;
}

watch(
  state,
  (s) => {
    if (s.kind === "idle") {
      take.running = false;
      return;
    }
    if (s.id !== take.id) {
      take.id = s.id;
      take.ms = 0;
      take.shown = 0;
      take.micLevel = 0;
    }
    if (s.kind === "recording" || s.kind === "paused") {
      take.ms = s.elapsedMs;
      take.at = performance.now();
    }
    take.running = s.kind === "recording";
    tickClock();
  },
);

watch(view, (v, old) => {
  if (v !== old) {
    popover.value = null;
    hideTip();
  }
  // The core hides the window while there is no bar; the next bar is fitted afresh.
  if (!v) lastFit = "";
  if (v === "setup" && old !== "setup") void loadDevices();
});

// The timer moves the clock only while the take runs: paused or finishing, `tickClock` keeps the
// time as it is, and a timer then would wake the page four times a second for nothing.
watch(
  () => take.running,
  (running) => {
    window.clearInterval(clockTimer);
    clockTimer = running ? window.setInterval(tickClock, 250) : 0;
  },
);

// ----------------------------------------------------------------------------- failures

function fail(error: unknown, place: ErrorPlace): void {
  if (codeOf(error) === "cancelled") return;
  console.error(error);
  problem.value = errorText(error, place);
  popover.value = "error";
  hideTip();
}

// ------------------------------------------------------------------------------- settings

let writes: Promise<void> = Promise.resolve();

/** Changes Settings › Recording: the bar shows the change at once, and the write reads the saved
 * settings again first, so a change made elsewhere in the meantime is kept. */
function persist(change: (r: Recording) => void): Promise<void> {
  if (settings.value) change(settings.value.recording);
  writes = writes
    .then(async () => {
      const fresh = await settings_get();
      change(fresh.recording);
      const saved = await settings_set(fresh);
      settings.value = saved.settings;
    })
    .catch(async (error) => {
      fail(error, "setup");
      settings.value = await settings_get().catch(() => settings.value);
    });
  return writes;
}

async function update(change: SetupUpdate): Promise<boolean> {
  try {
    setup.value = await record_setup_update(change);
    return true;
  } catch (error) {
    fail(error, "setup");
    return false;
  }
}

async function loadDevices(): Promise<void> {
  try {
    const next = await record_devices();
    devices.value = next;
    perms.microphone = next.microphonePermission;
    perms.camera = next.cameraPermission;
  } catch (error) {
    console.error("The recording devices could not be listed.", error);
  }
}

function applyPermissions(p: Permissions): void {
  perms.microphone = p.microphone;
  perms.camera = p.camera;
}

// ------------------------------------------------------------------------------- the setup

/** Asks macOS for a device the user turns on, when it has not been asked yet. */
async function allow(device: "microphone" | "camera"): Promise<boolean> {
  const now = perms[device];
  if (granted(now)) return true;
  if (now === "denied") {
    popover.value = device === "microphone" ? "permMic" : "permCam";
    return false;
  }
  try {
    const answer = await permissions_request(device);
    perms[device] = answer;
    if (answer === "denied") popover.value = device === "microphone" ? "permMic" : "permCam";
    return granted(answer);
  } catch (error) {
    fail(error, "setup");
    return false;
  }
}

function togglePopover(kind: Popover): void {
  popover.value = popover.value === kind ? null : kind;
  if (popover.value === "mic" || popover.value === "cam") void loadDevices();
}

async function toggleMic(): Promise<void> {
  if (micDenied.value) return togglePopover("permMic");
  if (micOn.value) {
    await update({ microphone: false });
    return;
  }
  if (await allow("microphone")) await update({ microphone: true });
}

async function toggleCamera(): Promise<void> {
  if (cameraDenied.value) return togglePopover("permCam");
  if (cameraOn.value) {
    await update({ camera: false });
    return;
  }
  if (await allow("camera")) await update({ camera: true });
}

async function pickMic(id: string): Promise<void> {
  popover.value = null;
  if (!(await allow("microphone"))) return;
  await persist((r) => void (r.microphoneDevice = id));
  await update({ microphone: true });
}

async function pickCamera(id: string): Promise<void> {
  popover.value = null;
  if (!(await allow("camera"))) return;
  if (await update({ camera: true, cameraDevice: id })) await persist((r) => void (r.webcamDevice = id));
}

async function withoutDevice(device: "microphone" | "camera"): Promise<void> {
  popover.value = null;
  if (device === "microphone") await update({ microphone: false });
  else await update({ camera: false });
}

function openPrivacy(device: "microphone" | "camera"): void {
  popover.value = null;
  permissions_open_settings(device).catch((error) => fail(error, "setup"));
}

async function setShowKeys(on: boolean): Promise<void> {
  await persist((r) => void (r.showKeys = on));
  if (!on) return;
  // Accessibility is asked for only when the user turns the overlay on, never earlier.
  try {
    const status = await permissions_status();
    if (status.accessibility !== "granted") await permissions_request("accessibility");
  } catch (error) {
    fail(error, "setup");
  }
}

async function start(): Promise<void> {
  if (!canRecord.value) return;
  starting.value = true;
  popover.value = null;
  hideTip();
  try {
    await record_start();
  } catch (error) {
    fail(error, "setup");
  } finally {
    starting.value = false;
  }
}

function closeSetup(): void {
  popover.value = null;
  record_setup_close().catch((error) => fail(error, "setup"));
}

async function setupAction(id: SetupAction): Promise<void> {
  hideTip();
  switch (id) {
    case "close":
      return closeSetup();
    case "display":
    case "window":
    case "area":
      if (setup.value?.mode !== id) await update({ mode: id as TargetMode });
      return;
    case "system": {
      // Read once: `persist` applies the change to the page's copy first, which flips
      // `systemOn`, and then again to a fresh copy for the store.
      const on = !systemOn.value;
      if (systemAvailable.value) await persist((r) => void (r.systemAudio = on));
      return;
    }
    case "mic":
      return toggleMic();
    case "micMenu":
      return togglePopover("mic");
    case "camera":
      return toggleCamera();
    case "cameraMenu":
      return togglePopover("cam");
    case "more":
      return togglePopover("more");
    case "record":
      return start();
  }
}

// ---------------------------------------------------------------------------------- the take

async function controlAction(id: ControlAction): Promise<void> {
  hideTip();
  if (popover.value === "error") popover.value = null;
  try {
    switch (id) {
      case "discard":
        await record_discard();
        break;
      case "keep":
        await record_keep();
        break;
      case "confirm":
        await record_discard(true);
        break;
      case "pause":
        await (state.value.kind === "paused" ? record_resume() : record_pause());
        break;
      case "mute":
        await record_set_microphone_muted(!muted.value);
        break;
      case "present":
        togglePopover("present");
        break;
      case "stop":
        stopping.value = true;
        await record_stop();
        break;
    }
  } catch (error) {
    fail(error, "take");
  } finally {
    if (id === "stop") stopping.value = false;
  }
}

/** A change from the Presentation card. Draw on screen closes the card, since drawing mode hands
 * the keyboard back to the app in front and the card would close with the focus anyway. */
async function presentChange(change: PresentChange): Promise<void> {
  if (change.drawing) popover.value = null;
  try {
    present.value = await record_present(change);
  } catch (error) {
    fail(error, "take");
  }
}

// ---------------------------------------------------------------------------------- meters

// The setup bar meters the chosen microphone and the system audio; the open microphone menu
// meters every microphone. Nothing is metered without access, so macOS never shows its
// microphone light for a meter the user did not ask for.
let meterKey = "";
watch(
  () => {
    if (view.value !== "setup") return null;
    let mics: string[] = [];
    if (granted(perms.microphone) && devices.value) {
      if (popover.value === "mic") mics = devices.value.microphones.map((d) => d.id);
      else if (micOn.value && chosenMic.value) mics = [chosenMic.value];
    }
    return mics.length || systemOn.value ? { mics, system: systemOn.value } : null;
  },
  (request) => {
    const key = JSON.stringify(request);
    if (key === meterKey) return;
    meterKey = key;
    record_meter(request ? request.mics : null, request?.system ?? false).catch((error) =>
      console.error("The meters could not start.", error),
    );
  },
  { immediate: true },
);

// -------------------------------------------------------------------------------- tooltips

type Tip = { text: string; keys: string; el: HTMLElement };
const tip = shallowRef<Tip | null>(null);
let tipTimer = 0;
let tipFor: HTMLElement | null = null;

function hideTip(): void {
  window.clearTimeout(tipTimer);
  tipFor = null;
  tip.value = null;
}

function scheduleTip(el: HTMLElement | null): void {
  if (el === tipFor) return;
  hideTip();
  if (!el || popover.value) return;
  tipFor = el;
  tipTimer = window.setTimeout(() => {
    if (tipFor !== el || popover.value || !el.isConnected) return;
    tip.value = { text: el.dataset.tip ?? "", keys: el.dataset.keys ?? "", el };
  }, TIP_DELAY);
}

const tipTarget = (event: Event) => (event.target as Element | null)?.closest?.<HTMLElement>("[data-tip]") ?? null;

function onPointerOver(event: PointerEvent): void {
  scheduleTip(tipTarget(event));
}
function onPointerOut(event: PointerEvent): void {
  const next = (event.relatedTarget as Element | null)?.closest?.<HTMLElement>("[data-tip]") ?? null;
  if (next !== tipFor) scheduleTip(next);
}
function onFocusIn(event: FocusEvent): void {
  const el = tipTarget(event);
  if (el?.matches(":focus-visible")) scheduleTip(el);
}

// ---------------------------------------------------------------------------------- layout

const hanging = computed(() => (view.value ? (popover.value ?? (tip.value ? "tip" : null)) : null));

/** What hangs from the bar as the page draws it. It follows `hanging` at once when something
 * opens, and only after the core has taken the native glass away when it closes or changes. */
const drawn = shallowRef<{ kind: string; tip: Tip | null } | null>(null);
/** How many closing fits run; while one does, the page reports no popover. */
let leaving = 0;
let drawTurn = 0;

async function redraw(): Promise<void> {
  const turn = ++drawTurn;
  const kind = hanging.value;
  const nowTip = kind === "tip" ? tip.value : null;
  const was = drawn.value;
  if (was?.kind === kind && was.tip === nowTip) return;
  if (was) {
    leaving++;
    try {
      await fit();
    } finally {
      leaving--;
    }
    if (turn !== drawTurn) return;
  }
  drawn.value = kind ? { kind, tip: nowTip } : null;
}

const popStyle = computed(() => ({
  right: `${geom.popRight}px`,
  top: side.value === "below" ? `${geom.barH + GAP}px` : "auto",
  bottom: side.value === "above" ? `${geom.barH + GAP}px` : "auto",
}));

/** The control a popover starts under, or the tip's control. */
function anchorOf(kind: string | null): HTMLElement | null {
  const bar = barEl.value;
  if (!bar) return null;
  if (kind === "mic" || kind === "permMic") return bar.querySelector<HTMLElement>('[data-id="mic"]');
  if (kind === "cam" || kind === "permCam") return bar.querySelector<HTMLElement>('[data-id="camera"]');
  // The Presentation card hangs from the bar's right end, as the design draws it.
  if (kind === "tip") return drawn.value?.tip?.el ?? null;
  return null;
}

let lastFit = "";
let fitDone: Promise<void> = Promise.resolve();
let layoutFrame = 0;

function layout(): void {
  layoutFrame = 0;
  const bar = barEl.value;
  if (!bar) return;
  const pop = popEl.value;
  geom.barW = bar.offsetWidth;
  geom.barH = bar.offsetHeight;
  geom.popW = pop?.offsetWidth ?? 0;
  geom.popH = pop?.offsetHeight ?? 0;
  const width = Math.max(stage.width, geom.barW, geom.popW);
  const barLeft = width - geom.barW;
  let left = width - geom.popW;
  const kind = drawn.value?.kind ?? null;
  const anchor = anchorOf(kind);
  if (pop && anchor) {
    const x = barLeft + anchor.getBoundingClientRect().left - bar.getBoundingClientRect().left;
    left = kind === "tip" ? x + anchor.offsetWidth / 2 - geom.popW / 2 : x;
  }
  geom.popRight = Math.round(width - geom.popW - clamp(left, 0, Math.max(0, width - geom.popW)));
  void fit();
}

function scheduleLayout(): void {
  // A timer, not an animation frame: the window stays hidden until this measures the bar, and
  // WebKit gives a hidden window no animation frames, so the bar would never show.
  if (!layoutFrame) layoutFrame = window.setTimeout(layout, 0);
}

/** The corner radius an element draws, in points. */
function radiusOf(el: Element | null | undefined): number {
  return el ? parseFloat(getComputedStyle(el).borderTopLeftRadius) || 0 : 0;
}

/** Reports the bar and what hangs from it; resolves once the core has placed them, at once when
 * nothing changed since the last report that did. */
function fit(): Promise<void> {
  const shown = leaving === 0 && drawn.value && popEl.value ? popEl.value : null;
  const barSize = { width: geom.barW, height: geom.barH };
  const popSize = shown ? { width: geom.popW, height: geom.popH } : null;
  const popGlass = shown?.firstElementChild;
  const glass: BarGlass = {
    side: side.value,
    barRadius: radiusOf(barEl.value?.firstElementChild),
    popoverRadius: radiusOf(popGlass),
    popoverRight: shown ? geom.popRight : 0,
    popoverSmall: popGlass?.matches(SMALL_SHADOW) ?? false,
  };
  // A tip that moves to a button of the same width keeps its size, and its glass moves with it.
  const key = JSON.stringify([barSize, popSize, glass]);
  if (key === lastFit || !barSize.width) return fitDone;
  lastFit = key;
  fitDone = record_bar_fit(barSize, popSize, glass).then(placed, (error) => {
    console.error("The bar window could not be fitted.", error);
  });
  return fitDone;
}

/** The core's stage for the bar. A new side moves the bar to the stage's other edge, and the
 * page fits again so the core shows the window; a new width moves what hangs from the bar. */
function placed(layout: BarLayout): void {
  if (!layout.width) return;
  const moved = layout.popoverSide !== side.value || layout.width !== stage.width;
  side.value = layout.popoverSide;
  stage.width = layout.width;
  stage.height = layout.height;
  if (moved) void nextTick(scheduleLayout);
}

const observer = new ResizeObserver(() => scheduleLayout());

watch(
  [barEl, popEl],
  ([bar, pop], [oldBar, oldPop]) => {
    if (oldBar && oldBar !== bar) observer.unobserve(oldBar);
    if (oldPop && oldPop !== pop) observer.unobserve(oldPop);
    if (bar) observer.observe(bar);
    if (pop) observer.observe(pop);
    scheduleLayout();
  },
  { flush: "post" },
);
watch([hanging, tip], () => void redraw());
watch(drawn, () => nextTick(scheduleLayout));

// -------------------------------------------------------------------------------- keyboard

/** Closes the popover and gives the focus back to the control that opened it. */
function closePopover(): void {
  const kind = popover.value;
  popover.value = null;
  const opener: Partial<Record<Popover, string>> = {
    more: "more",
    mic: "micMenu",
    cam: "cameraMenu",
    permMic: "mic",
    permCam: "camera",
    present: "present",
  };
  const id = kind ? opener[kind] : undefined;
  if (id) barEl.value?.querySelector<HTMLElement>(`[data-id="${id}"]`)?.focus({ preventScroll: true });
}

function onKeyDown(event: KeyboardEvent): void {
  if (event.defaultPrevented) return;
  const onControl = !!(event.target as Element | null)?.closest?.("button, input, select");
  if (view.value === "setup") {
    if (event.key === "Escape") {
      event.preventDefault();
      if (popover.value) closePopover();
      else closeSetup();
      return;
    }
    if (event.metaKey || event.ctrlKey || event.altKey || event.repeat) return;
    if (event.key === "Enter") {
      if (onControl) return;
      event.preventDefault();
      void start();
      return;
    }
    const keys: Record<string, SetupAction> = {
      d: "display",
      w: "window",
      a: "area",
      s: "system",
      m: event.shiftKey ? "micMenu" : "mic",
      c: event.shiftKey ? "cameraMenu" : "camera",
      o: "more",
    };
    const id = keys[event.key.toLowerCase()];
    if (!id) return;
    event.preventDefault();
    void setupAction(id);
  } else if (view.value === "control") {
    if (asked.value) {
      if (event.key === "Escape" || (event.key === "Enter" && !onControl)) {
        event.preventDefault();
        void controlAction("keep");
      }
      return;
    }
    if (event.key === "Escape" && popover.value) {
      event.preventDefault();
      popover.value = null;
    }
  }
}

/** A click outside the window closes a menu, as a menu does. */
function onBlur(): void {
  hideTip();
  if (popover.value && popover.value !== "error") popover.value = null;
}

function onFocus(): void {
  if (view.value === "setup") void loadDevices();
}

// ------------------------------------------------------------------------------- lifecycle

onMounted(async () => {
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("blur", onBlur);
  window.addEventListener("focus", onFocus);
  let heardSetup = false;
  try {
    stops.push(
      await onRecordSetup((next) => {
        heardSetup = true;
        setup.value = next;
      }),
    );
    stops.push(await onRecordState((next) => void (state.value = next)));
    stops.push(await onRecordBarLayout(placed));
    stops.push(
      await onRecordTick((tick) => {
        if (tick.id !== take.id && take.id) return;
        take.ms = tick.elapsedMs;
        take.at = performance.now();
        take.micLevel = tick.microphoneLevel ?? 0;
        tickClock();
      }),
    );
    stops.push(await onRecordLevels((next) => void (levels.value = next)));
    let heardPresent = false;
    stops.push(
      await onRecordPresent((next) => {
        heardPresent = true;
        present.value = next;
        if (!next && popover.value === "present") popover.value = null;
      }),
    );
    void record_present_state().then((first) => {
      if (!heardPresent) present.value = first;
    });
    stops.push(await on<Settings>(SETTINGS_CHANGED, (next) => void (settings.value = next)));
    stops.push(await on<Permissions>(PERMISSIONS_CHANGED, applyPermissions));
    const [first, saved, status, now] = await Promise.all([
      record_setup_state(),
      settings_get(),
      permissions_status(),
      record_state(),
    ]);
    if (!heardSetup) setup.value = first;
    if (state.value.kind === "idle") state.value = now;
    settings.value = saved;
    applyPermissions(status);
  } catch (error) {
    console.error("The recording bar could not load.", error);
  }
  await loadDevices();
});

onUnmounted(() => {
  window.removeEventListener("keydown", onKeyDown);
  window.removeEventListener("blur", onBlur);
  window.removeEventListener("focus", onFocus);
  window.clearInterval(clockTimer);
  window.clearTimeout(tipTimer);
  clearTimeout(layoutFrame);
  observer.disconnect();
  stops.forEach((stop) => stop());
  void record_meter(null).catch(() => undefined);
});

const onFps = (fps: number) => persist((r) => void (r.fps = fps));
const onResolution = (resolution: Resolution) => persist((r) => void (r.resolution = resolution));
const onClicks = (on: boolean) => persist((r) => void (r.highlightClicks = on));
</script>

<template>
  <div v-if="view" class="r-bar-page">
    <div
      ref="barEl"
      class="r-bar-slot"
      :data-side="side"
      @pointerover="onPointerOver"
      @pointerout="onPointerOut"
      @pointerdown="hideTip"
      @focusin="onFocusIn"
      @focusout="hideTip"
    >
      <SetupBar
        v-if="view === 'setup' && setup"
        :mode="setup.mode"
        :system="setupModel.system"
        :mic="setupModel.mic"
        :camera="setupModel.camera"
        :open="popover"
        :can-record="canRecord"
        @action="setupAction"
      />
      <ControlBar v-else-if="view === 'control'" v-bind="controlModel" @action="controlAction" />
    </div>

    <div v-if="drawn" ref="popEl" class="r-pop-slot" :style="popStyle">
      <OptionsPop
        v-if="drawn.kind === 'more' && setup && rec"
        :fps="rec.fps"
        :resolution="rec.resolution"
        :output-size="setup.outputSize"
        :clicks="rec.highlightClicks"
        :keys="rec.showKeys"
        @fps="onFps"
        @resolution="onResolution"
        @clicks="onClicks"
        @keys="setShowKeys"
      />
      <DeviceMenu
        v-else-if="drawn.kind === 'mic'"
        :title="t('record.mic.menu')"
        :devices="devices?.microphones ?? []"
        :chosen="chosenMic"
        :levels="granted(perms.microphone) ? levels.microphones : null"
        :empty="t('record.mic.none')"
        @pick="pickMic"
      />
      <DeviceMenu
        v-else-if="drawn.kind === 'cam'"
        :title="t('record.camera.menu')"
        :devices="devices?.cameras ?? []"
        :chosen="chosenCamera"
        :levels="null"
        :empty="t('record.camera.none')"
        @pick="pickCamera"
      />
      <PermissionPop
        v-else-if="drawn.kind === 'permMic'"
        device="microphone"
        @open="openPrivacy('microphone')"
        @without="withoutDevice('microphone')"
      />
      <PermissionPop v-else-if="drawn.kind === 'permCam'" device="camera" @open="openPrivacy('camera')" @without="withoutDevice('camera')" />
      <PresentPop
        v-else-if="drawn.kind === 'present'"
        :spotlight="!!present?.spotlight"
        :fade="present?.fade ?? !!rec?.inkFades"
        :keys="{ draw: settings?.shortcuts.drawOnScreen ?? null, spotlight: settings?.shortcuts.spotlight ?? null }"
        @draw="presentChange({ drawing: !present?.drawing })"
        @spotlight="presentChange({ spotlight: !present?.spotlight })"
        @fade="presentChange({ fade: $event })"
      />
      <div v-else-if="drawn.kind === 'error'" class="g-pop g-glass r-problem" data-material="strong" role="alert">
        <GIcon name="warning" :size="16" class="r-warn" />
        <p class="r-problem-text">{{ problem }}</p>
        <button type="button" class="g-btn" data-size="small" :aria-label="t('record.error.dismiss')" @click="popover = null">
          <GIcon name="close" :size="16" />
        </button>
      </div>
      <div v-else-if="drawn.kind === 'tip' && drawn.tip" class="g-tip" role="tooltip">
        {{ drawn.tip.text }}<KeyCaps v-if="drawn.tip.keys" :caps="[drawn.tip.keys]" />
      </div>
    </div>
  </div>
</template>

<style>
.r-bar-page { position: fixed; inset: 0; overflow: hidden; }
/* The core draws the shadow and the outer ring under this window's glass (`record_bar_fit`); drawn
   here too, they would darken twice wherever the window has room beside the glass. */
.r-bar-page > :is(.r-bar-slot, .r-pop-slot) > :is(.g-bar, .g-pop, .g-tip) {
  box-shadow: inset 0 0 0 var(--hairline) var(--edge-inner), inset 0 1px 0 0 var(--edge-top),
    inset 0 -1px 0 0 var(--edge-bottom), inset 0 0 12px 0 var(--edge-glow);
}
.r-bar-slot { position: absolute; right: 0; top: 0; display: flex; width: max-content; }
.r-bar-slot[data-side="above"] { top: auto; bottom: 0; }
.r-pop-slot { position: absolute; display: flex; width: max-content; }
.r-problem { flex-direction: row; align-items: flex-start; gap: var(--space-8); padding: var(--space-12) var(--space-6) var(--space-12) var(--space-12); }
.r-problem > g-icon { margin-top: 1px; }
.r-problem-text { flex: 1; margin: 0; font: var(--type-body); color: var(--text); }
.r-problem > .g-btn { margin: -6px 0 -6px 0; }
</style>
