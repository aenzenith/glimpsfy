// Copy path and Send to app. Mirrors src-tauri/src/commands/send.rs. Copy path works everywhere;
// Send to app is macOS only for now (`can("sendToApp")`).

import type { CaptureId, Selection } from "./capture";
import { call, on } from "./core";
import type { SendPaste } from "../settings";

/** A send moved on. Payload: `SendState`. */
export const SEND_STATE = "send://state";

/** How far a send got: the app is starting, ⌘V was pressed, the user must press it (no
 * Accessibility), the app showed no window in 10 seconds, the wait was stopped, or the app could
 * not be brought forward. */
export type SendPhase = "opening" | "pasted" | "noPermission" | "timedOut" | "cancelled" | "failed";

export type SendState = { sendId: number; targetName: string; phase: SendPhase };

/** What is sent: an open capture's selection, a history entry, or a capture a toast reports. */
export type SendSource =
  | { kind: "selection"; selection: Selection }
  | { kind: "entry"; entryId: string }
  | { kind: "capture"; captureId: CaptureId };

/** What a `sent` toast carries. */
export type SentToast = {
  sendId: number;
  targetId: string;
  targetName: string;
  phase: SendPhase;
  /** What Try again sends. */
  retry: SendSource | null;
  /** The entry the send went out from, whose thumbnail the toast shows. */
  entryId: string | null;
};

/** One target of the send list, with what the system says about it now. */
export type SendTargetInfo = {
  bundleId: string;
  name: string;
  paste: SendPaste;
  /** On this Mac; a missing target shows dimmed and cannot be picked. */
  installed: boolean;
  /** Running now; one that is not is started first. */
  running: boolean;
  /** The app's icon as a PNG data URL. */
  icon: string | null;
};

export type SendTargets = {
  targets: SendTargetInfo[];
  /** The target sent to last since Glimpsfy started; the list opens on it. */
  last: string | null;
  /** Glimpsfy may press ⌘V (Accessibility). */
  canPaste: boolean;
};

export type PickedApp = { bundleId: string; name: string; icon: string | null; paste: SendPaste };

/** A send answers its id; Copy path answers the copied path, unquoted. */
export type Sent = {
  sendId: number | null;
  path: string | null;
  /** The copied path points into the inbox, where the file is deleted after a day (history off). */
  temporary: boolean;
};

export const send_list_targets = () => call<SendTargets>("send_list_targets");
/** Lets the user pick an app; `null` when they cancel. The caller adds it to the settings. */
export const send_pick_app = () => call<PickedApp | null>("send_pick_app");
/** Copies the path of the source's file, quoted for a shell. A selection closes its capture. */
export const send_copy_path = (source: SendSource) => call<Sent>("send_copy_path", { source });
/** Sends the source to the target whose bundle identifier is `targetId`; the progress arrives as
 * `send://state`. A selection closes its capture once the target is known to be sendable. */
export const send_to_app = (source: SendSource, targetId: string) => call<Sent>("send_to_app", { source, targetId });
/** Stops waiting for the app of a send that is still opening. */
export const send_cancel = (sendId: number) => call<void>("send_cancel", { sendId });

export function onSendState(handler: (state: SendState) => void) {
  return on<SendState>(SEND_STATE, handler);
}
