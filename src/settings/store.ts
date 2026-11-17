// The settings window's state: the saved settings, every change saved at once, and what a save
// could not do as asked. Every control changes `settings` in place and calls `update`; the save
// sends the whole object as it is when its turn comes, so a change never loses another, and saves
// run one after another, so a slow one cannot overwrite a later one. Another window's save
// arrives as `settings://changed` and replaces the state, except while a save of this window is
// still on its way.

import { ref } from "vue";

import { on } from "../lib/api/core";
import {
  settings_get,
  settings_set,
  type SettingsSaved,
  type ShortcutCleared,
  type ShortcutProblem,
} from "../lib/api/settings";
import { SETTINGS_CHANGED, SHORTCUT_FAILED } from "../lib/events";
import type { Settings } from "../lib/settings";
import { describeError } from "./text";

/** A message over the section: what went wrong and, when there is one, the way out. */
export type Banner = { tone: "danger" | "warn"; text: string; action?: { label: string; run: () => void } };

export const settings = ref<Settings | null>(null);
export const loadError = ref<string | null>(null);
export const banner = ref<Banner | null>(null);
/** Shortcuts the system refused, by action. */
export const shortcutProblems = ref<Record<string, ShortcutProblem>>({});
/** Actions whose keys went to another action that held them, by action. */
export const clearedShortcuts = ref<Record<string, ShortcutCleared>>({});

let pending = 0;
let chain: Promise<unknown> = Promise.resolve();
let started = false;

const copy = (value: Settings): Settings => JSON.parse(JSON.stringify(value)) as Settings;

function note(saved: SettingsSaved): void {
  shortcutProblems.value = Object.fromEntries(saved.shortcutProblems.map((p) => [p.action, p]));
  const cleared = { ...clearedShortcuts.value };
  for (const c of saved.clearedShortcuts) cleared[c.action] = c;
  clearedShortcuts.value = cleared;
}

export async function start(): Promise<void> {
  if (started) return;
  started = true;
  try {
    settings.value = await settings_get();
  } catch (error) {
    loadError.value = describeError(error);
  }
  void on<Settings>(SETTINGS_CHANGED, (next) => {
    if (pending === 0) settings.value = next;
  });
  void on<ShortcutProblem>(SHORTCUT_FAILED, (problem) => {
    shortcutProblems.value = { ...shortcutProblems.value, [problem.action]: problem };
  });
}

/** Reads the settings again, after the first read failed. */
export async function reload(): Promise<void> {
  try {
    settings.value = await settings_get();
    loadError.value = null;
  } catch (error) {
    loadError.value = describeError(error);
  }
}

/** Changes the settings in place and saves them. `failed` words the banner for an error this
 * change can cause (such as a refused login item); others get the general sentence. */
export function update(change: (s: Settings) => void, failed?: (error: unknown) => Banner): Promise<SettingsSaved | null> {
  const current = settings.value;
  if (!current) return Promise.resolve(null);
  change(current);
  pending += 1;
  let last: SettingsSaved | null = null;
  const run = chain.then(async () => {
    try {
      const saved = await settings_set(copy(settings.value ?? current));
      last = saved;
      note(saved);
      if (banner.value?.tone === "danger") banner.value = null;
      return saved;
    } catch (error) {
      banner.value = failed ? failed(error) : { tone: "danger", text: describeError(error) };
      // The file is the truth: show what it holds now.
      try {
        last = { settings: await settings_get(), shortcutProblems: [], clearedShortcuts: [] };
      } catch {
        last = null;
      }
      return null;
    } finally {
      pending -= 1;
      if (pending === 0 && last) settings.value = last.settings;
    }
  });
  chain = run.catch(() => undefined);
  return run;
}

/** Forgets that an action lost its keys, once the user acts on the row again. */
export function forgetCleared(action: string): void {
  if (!(action in clearedShortcuts.value)) return;
  const next = { ...clearedShortcuts.value };
  delete next[action];
  clearedShortcuts.value = next;
}
