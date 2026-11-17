// Update: Glimpsfy's own updates from the GitHub releases. Mirrors src-tauri/src/commands/update.rs
// and the `UpdateView` of src-tauri/src/update.rs.

import { call } from "./core";

/** To every window: the update's state changed. Payload: `UpdateView`. */
export const UPDATE_CHANGED = "update://changed";

/** The releases page, for "What's new" and for a copy that cannot replace itself. */
export const RELEASES_URL = "https://github.com/aenzenith/glimpsfy/releases/latest";

export type UpdateStatus = "off" | "idle" | "checking" | "downloading" | "ready" | "available" | "installing" | "failed";

export type UpdateView = {
  /** The installed version. */
  current: string;
  status: UpdateStatus;
  /** The new version: from `downloading` on, and with `available` and `failed`. */
  version: string | null;
  downloaded: number;
  total: number | null;
  /** When the release last answered, in ms since the epoch. */
  checkedAt: number | null;
  /** The last ask got no answer. */
  offline: boolean;
  /** With `failed`. */
  error: "download" | "signature" | "install" | null;
};

export function update_state(): Promise<UpdateView> {
  return call("update_state");
}

/** Asks for a new version now; the answer arrives as `update://changed`. */
export function update_check(): Promise<void> {
  return call("update_check");
}

/** Installs the downloaded update and restarts; it resolves only when the install failed. */
export function update_install(): Promise<void> {
  return call("update_install");
}
