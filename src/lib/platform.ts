// The platform a page runs on: the system, the Linux session and what the platform can do
// (`app_platform`). `main.ts` reads it once before the page mounts, so every page decides from
// the same answer, and from its first paint, what it shows. A page asks `can` whether a feature
// works here and reads `platform.os` only for wording and key names.

import { app_platform, type Platform, type PlatformCapabilities } from "./api/windows";

export type Capability = keyof PlatformCapabilities;

/** The answer until the core's arrives, and when the core cannot give one: macOS, where every
 * capability holds, because macOS is the platform that ships. */
export const platform: Platform = {
  os: "macos",
  session: null,
  capabilities: {
    windowCapture: true,
    captureExclusion: true,
    nonActivatingOverlay: true,
    inputOverlays: true,
    cameraBubble: true,
    pinAllSpaces: true,
    pinLock: true,
    nativeGlass: true,
    dragOut: true,
    copyFile: true,
    shutterSound: true,
    systemSettingsPanes: true,
    screenRecordingPermission: true,
    selfUpdate: true,
    globalShortcuts: true,
    trayPopover: true,
    globalPointer: true,
    windowShadow: true,
    sendToApp: true,
    subjectLift: true,
    agents: true,
    scrollCapture: true,
  },
  videoBase: null,
};

/** Whether this platform can do `capability`. A capability the core does not send yet counts as
 * there, as it was before the core learned to say otherwise. */
export function can(capability: Capability): boolean {
  return platform.capabilities[capability] !== false;
}

/** macOS, whose key symbols, wording and order the pages were written in. */
export const isMac = (): boolean => platform.os === "macos";

/** Reads the platform once and marks the page's root with it, so CSS can follow the system
 * (`data-platform`, `data-session`) and draw opaque surfaces where a transparent window would
 * show black (`data-opaque`). */
export async function loadPlatform(): Promise<void> {
  try {
    const answer = await app_platform();
    platform.os = answer.os;
    platform.session = answer.session;
    platform.capabilities = { ...platform.capabilities, ...answer.capabilities };
    platform.videoBase = answer.videoBase ?? null;
  } catch (error) {
    console.warn("The platform could not be read, so the page assumes macOS.", error);
  }
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  root.dataset.platform = platform.os;
  if (platform.session) root.dataset.session = platform.session;
  if (!can("transparentWindows")) root.dataset.opaque = "";
}
