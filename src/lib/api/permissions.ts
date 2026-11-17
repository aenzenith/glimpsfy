// Permissions: screen recording, microphone, camera and accessibility. Mirrors
// src-tauri/src/commands/permissions.rs.

import { call } from "./core";

export type PermissionState = "granted" | "denied" | "notDetermined" | "notRequired";
export type Permission = "screenRecording" | "microphone" | "camera" | "accessibility";

/** The payload of `permissions://changed`, and the result of `permissions_status`. */
export type Permissions = Record<Permission, PermissionState> & {
  /** Screen recording was granted while Glimpsfy ran; it works after `app_restart`. */
  restartNeeded: boolean;
};

export const permissions_status = () => call<Permissions>("permissions_status");
export const permissions_request = (permission: Permission) => call<PermissionState>("permissions_request", { permission });
export const permissions_open_settings = (permission: Permission) => call<void>("permissions_open_settings", { permission });
