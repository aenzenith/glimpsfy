// Settings: read and save the whole settings object, preview a file name and check a folder.
// Mirrors src-tauri/src/commands/settings.rs; the object itself is `src/lib/settings.ts`.

import type { ImageFormat, Settings, Shortcuts } from "../settings";
import { call } from "./core";

export type ShortcutProblem = { action: string; accelerator: string; message: string };

/** An action that lost its keys because another action already held them. */
export type ShortcutCleared = { action: string; accelerator: string; keptBy: string };

/** The saved settings, the shortcuts the system refused and the ones a clash cleared. */
export type SettingsSaved = {
  settings: Settings;
  shortcutProblems: ShortcutProblem[];
  clearedShortcuts: ShortcutCleared[];
};

/** Why a file name template cannot name a file. */
export type TemplateProblem =
  | { kind: "empty" }
  | { kind: "character"; character: string }
  | { kind: "unknownToken"; token: string; suggestion: string | null };

/** What a template names a capture, from the function that names saved files. */
export type NamePreview = { fileName: string | null; problem: TemplateProblem | null };

export type FolderStatus = { exists: boolean; writable: boolean };

export const settings_get = () => call<Settings>("settings_get");
/** Saves every setting at once. Turning on start at login can fail with `io`; the setting then stays off. */
export const settings_set = (settings: Settings) => call<SettingsSaved>("settings_set", { settings });
export const settings_reset = () => call<SettingsSaved>("settings_reset");
export const settings_preview_name = (template: string, format: ImageFormat) =>
  call<NamePreview>("settings_preview_name", { template, format });
export const settings_folder_status = (path: string) => call<FolderStatus>("settings_folder_status", { path });
/** Turns Glimpsfy's global shortcuts off while the recorder listens, and on again after. */
export const settings_pause_shortcuts = (paused: boolean) =>
  call<ShortcutProblem[]>("settings_pause_shortcuts", { paused });
/** This platform's default shortcuts, for Restore defaults (saved with `settings_set`). */
export const settings_default_shortcuts = () => call<Shortcuts>("settings_default_shortcuts");
/** Forgets every first-upload confirmation of a provider, so its next upload asks again. A page
 * cannot change `upload.confirmedProviders` through `settings_set`; only this and
 * `upload_confirm_provider` do. */
export const settings_revoke_consent = (providerId: string) =>
  call<Settings>("settings_revoke_consent", { providerId });
