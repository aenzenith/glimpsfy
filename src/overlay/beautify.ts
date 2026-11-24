// The overlay's beautify state for one capture (B11): whether it is on, the style its panel shows,
// and Save as PNG this time. The core holds the style the outputs use (`capture_set_beautify`);
// every change is sent at once, and an output first waits for the changes sent before it
// (`settled`), so F pressed during a drag in "copy and close" still reaches the copy. Opening the
// panel turns beautify on, so its preview shows the output at once, unless F or the switch already
// decided this capture.

import { computed, ref, shallowRef } from "vue";

import { readBeautify, writeBeautify, type BeautifyStyle } from "../lib/annotate";
import { capture_set_beautify, type OverlayFrame } from "../lib/api/capture";
import { settings_get, settings_set } from "../lib/api/settings";
import { app_choose_file } from "../lib/api/windows";
import { t } from "../lib/i18n";
import type { Settings } from "../lib/settings";
import {
  IMAGE_FALLBACK,
  IMAGE_TYPES,
  chosenStyle,
  clamped,
  presetName,
  readPresets,
  styleOf,
  withNewPreset,
  withPresetStyle,
} from "../annotate/beautify";

export function useOverlayBeautify(say: (text: string) => void) {
  const settings = shallowRef<Settings | null>(null);
  const captureId = ref<number | null>(null);
  const on = ref(false);
  const style = shallowRef<BeautifyStyle>(chosenStyle(null));
  const pngOnce = ref(false);
  const open = ref(false);
  /** F or the panel's switch set this capture's beautify, so opening the panel leaves it alone. */
  let decided = false;
  let pending: Promise<void> = Promise.resolve();

  const presets = computed(() => readPresets(settings.value?.beautify));
  const autoId = computed(() => (settings.value?.beautify?.auto ? settings.value.beautify.preset : null));
  const format = computed(() => settings.value?.output.format ?? "png");

  /** Sends the state as it is now; the sends run in order. */
  function sync(): void {
    const id = captureId.value;
    if (id === null) return;
    const value = on.value ? writeBeautify(clamped(style.value)) : null;
    pending = pending
      .then(() => capture_set_beautify(id, value))
      .catch((error) => console.warn("[glimpsfy] the beautify style did not reach the capture", error));
  }

  /** Resolves once every change made so far reached the core. */
  const settled = () => pending;

  /** A new capture: its style is the one the core started it with (the automatic preset), and
   * the switch turns on the chosen preset when there is none. */
  async function start(frame: OverlayFrame): Promise<void> {
    captureId.value = frame.captureId;
    open.value = false;
    pngOnce.value = false;
    decided = false;
    let given: BeautifyStyle | null = null;
    try {
      given = frame.beautify ? readBeautify(frame.beautify) : null;
    } catch (error) {
      console.warn("[glimpsfy] the capture's beautify style could not be read", error);
    }
    on.value = given !== null;
    style.value = given ?? chosenStyle(settings.value);
    const first = style.value;
    try {
      const s = await settings_get();
      if (captureId.value !== frame.captureId) return;
      settings.value = s;
      // F or the panel may have turned the guessed style on before the presets arrived.
      if (!given && style.value === first) {
        style.value = chosenStyle(s);
        if (on.value) sync();
      }
    } catch (error) {
      console.warn("[glimpsfy] the beautify presets were not read", error);
    }
  }

  function stop(): void {
    captureId.value = null;
    open.value = false;
  }

  /** The action bar's button: the panel opens on the output it previews. */
  function show(): void {
    open.value = true;
    if (on.value || decided) return;
    on.value = true;
    sync();
  }

  /** F and the panel's switch. */
  function toggle(next = !on.value): void {
    decided = true;
    if (on.value === next) return;
    on.value = next;
    sync();
  }

  /** A change in the panel; a value changed while the switch is off turns it on. */
  function change(next: BeautifyStyle): void {
    style.value = next;
    on.value = true;
    sync();
  }

  function pick(id: string): void {
    const preset = presets.value.find((p) => p.id === id);
    if (!preset) return;
    change(styleOf(preset));
  }

  /** The file panel steps the overlays aside (`app_choose_file`) and brings them back. */
  async function chooseImage(): Promise<void> {
    try {
      const path = await app_choose_file([{ name: t("beautify.images"), extensions: IMAGE_TYPES }]);
      if (!path) return;
      const bg = style.value.background;
      const fallback = bg.kind === "gradient" ? bg.stops : bg.kind === "image" ? bg.fallback : IMAGE_FALLBACK;
      change({ ...style.value, background: { kind: "image", path, fallback } });
    } catch (error) {
      console.warn("[glimpsfy] the image could not be chosen", error);
    }
  }

  /** Writes the panel's values into its preset in Settings › Beautify. */
  async function updatePreset(): Promise<void> {
    const id = style.value.preset;
    if (!id) return;
    try {
      const next = withPresetStyle(await settings_get(), id, style.value);
      await settings_set(next);
      settings.value = next;
      const preset = readPresets(next.beautify).find((p) => p.id === id);
      if (preset) say(t("beautify.updated", { name: presetName(preset) }));
    } catch (error) {
      console.warn("[glimpsfy] the preset was not updated", error);
      say(t("beautify.failed"));
    }
  }

  /** Adds the panel's values as a new preset named `name`, which this capture then names. */
  async function createPreset(name: string): Promise<void> {
    try {
      const made = withNewPreset(await settings_get(), name, style.value);
      await settings_set(made.settings);
      settings.value = made.settings;
      change({ ...style.value, preset: made.id });
      say(t("beautify.added", { name }));
    } catch (error) {
      console.warn("[glimpsfy] the preset was not added", error);
      say(t("beautify.failed"));
    }
  }

  return { settings, on, style, pngOnce, open, presets, autoId, format, start, stop, sync, settled, show, toggle, change, pick, chooseImage, updatePreset, createPreset };
}
