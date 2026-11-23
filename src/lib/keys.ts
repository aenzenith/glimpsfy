// Keyboard shortcuts as the pages see them: accelerators (the strings Settings keeps, such as
// "Shift+CommandOrControl+2") turned into key caps, key presses turned into accelerators, the rule
// the recorder applies, and the combinations macOS keeps for itself. The store applies the same
// "one action per key" rule (`Shortcuts::resolve_clashes` in src-tauri/src/settings.rs), and
// `canonical` here matches its `canonical_accelerator`.

import type { Shortcuts } from "./settings";

/** The modifier caps in the order macOS prints them. */
export const MODIFIERS = ["⌃", "⌥", "⇧", "⌘"] as const;
export type Modifier = (typeof MODIFIERS)[number];

/** The shortcut rows of Settings in table order, by group, with their settings field. */
export const SHORTCUT_GROUPS = [
  {
    group: "capture",
    actions: [
      "captureArea",
      "captureWindow",
      "captureDisplay",
      "captureAllDisplays",
      "captureAreaDelayed",
      "repeatLastArea",
      "copyText",
      "measureScreen",
      "pickColor",
      "scrollCapture",
    ],
  },
  {
    group: "recording",
    actions: [
      "recordScreen",
      "recordDisplay",
      "pauseRecording",
      "muteMicrophone",
      "discardRecording",
      "drawOnScreen",
      "spotlight",
    ],
  },
  { group: "app", actions: ["openHistory"] },
] as const;

export type ShortcutAction = (typeof SHORTCUT_GROUPS)[number]["actions"][number];
export const SHORTCUT_ACTIONS: readonly ShortcutAction[] = SHORTCUT_GROUPS.flatMap((g) => g.actions);

const isMac = typeof navigator === "undefined" || /Mac/.test(navigator.platform || navigator.userAgent);

/** A key's cap for a key name of an accelerator ("2", "A", "Space", "ArrowUp", "F5"…). */
const CAPS: Record<string, string> = {
  SPACE: "Space",
  ENTER: "↩",
  RETURN: "↩",
  TAB: "⇥",
  ARROWLEFT: "←",
  ARROWRIGHT: "→",
  ARROWUP: "↑",
  ARROWDOWN: "↓",
  LEFT: "←",
  RIGHT: "→",
  UP: "↑",
  DOWN: "↓",
  MINUS: "-",
  EQUAL: "=",
  BRACKETLEFT: "[",
  BRACKETRIGHT: "]",
  BACKSLASH: "\\",
  SEMICOLON: ";",
  QUOTE: "'",
  COMMA: ",",
  PERIOD: ".",
  SLASH: "/",
  BACKQUOTE: "`",
  HOME: "↖",
  END: "↘",
  PAGEUP: "⇞",
  PAGEDOWN: "⇟",
  BACKSPACE: "⌫",
  DELETE: "⌦",
  ESCAPE: "Esc",
};

/** The accelerator key name for a `KeyboardEvent.code`, or `null` for a key a shortcut cannot use. */
export function keyOfCode(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit\d$/.test(code)) return code.slice(5);
  if (/^Numpad\d$/.test(code)) return `Numpad${code.slice(6)}`;
  if (/^F\d{1,2}$/.test(code)) return code;
  const named = ["Space", "Enter", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "Semicolon", "Quote", "Comma", "Period", "Slash", "Backquote", "Home", "End", "PageUp", "PageDown"];
  return named.includes(code) ? code : null;
}

type Parsed = { mods: Set<Modifier>; key: string | null };

function parse(accelerator: string): Parsed | null {
  const mods = new Set<Modifier>();
  let key: string | null = null;
  for (const raw of accelerator.split("+")) {
    const token = raw.trim().toLowerCase();
    if (!token) return null;
    if (token === "shift") mods.add("⇧");
    else if (token === "control" || token === "ctrl") mods.add("⌃");
    else if (token === "alt" || token === "option") mods.add("⌥");
    else if (["command", "cmd", "super", "meta"].includes(token)) mods.add("⌘");
    else if (["commandorcontrol", "commandorctrl", "cmdorctrl", "cmdorcontrol"].includes(token)) mods.add(isMac ? "⌘" : "⌃");
    else {
      if (key !== null) return null;
      let name = raw.trim();
      for (const prefix of ["digit", "key"]) {
        if (name.toLowerCase().startsWith(prefix) && name.length === prefix.length + 1) name = name.slice(prefix.length);
      }
      key = name.toUpperCase();
    }
  }
  return { mods, key };
}

/** The form two accelerators share when they name the same keys, or `null` when it names none.
 * It matches `canonical_accelerator` in src-tauri/src/settings.rs. */
export function canonical(accelerator: string | null | undefined): string | null {
  if (!accelerator) return null;
  const parsed = parse(accelerator);
  if (!parsed || !parsed.key) return null;
  const names: Record<Modifier, string> = { "⌃": "Control", "⌥": "Alt", "⇧": "Shift", "⌘": "Super" };
  return [...MODIFIERS.filter((m) => parsed.mods.has(m)).map((m) => names[m]), parsed.key].join("+");
}

/** The caps of an accelerator in macOS order, such as ["⇧", "⌘", "2"]. */
export function capsOf(accelerator: string | null | undefined): string[] {
  if (!accelerator) return [];
  const parsed = parse(accelerator);
  if (!parsed) return [];
  const caps: string[] = MODIFIERS.filter((m) => parsed.mods.has(m));
  if (parsed.key) caps.push(CAPS[parsed.key] ?? parsed.key.replace(/^NUMPAD/, ""));
  return caps;
}

/** The caps as one string, such as "⇧⌘2", for sentences. */
export const capsText = (accelerator: string | null | undefined) => capsOf(accelerator).join("");

/** The modifiers held in a key event, in macOS order. */
export function modifiersOf(e: Pick<KeyboardEvent, "ctrlKey" | "altKey" | "shiftKey" | "metaKey">): Modifier[] {
  const held: Modifier[] = [];
  if (e.ctrlKey) held.push("⌃");
  if (e.altKey) held.push("⌥");
  if (e.shiftKey) held.push("⇧");
  if (e.metaKey) held.push("⌘");
  return held;
}

/** The accelerator for modifiers and a key. ⌘ is written `CommandOrControl`, so the same setting
 * serves Windows and Linux, where it means Ctrl. */
export function acceleratorOf(mods: Modifier[], key: string): string {
  const names: Record<Modifier, string> = { "⌃": "Control", "⌥": "Alt", "⇧": "Shift", "⌘": "CommandOrControl" };
  return [...MODIFIERS.filter((m) => mods.includes(m)).map((m) => names[m]), key].join("+");
}

/** What the recorder makes of a key press. */
export type RecorderStep =
  | { kind: "held"; mods: Modifier[] }
  | { kind: "cancel" }
  | { kind: "clear" }
  | { kind: "ignore" }
  | { kind: "refuse"; reason: "needsModifier" | "commandAlone"; caps: string[] }
  | { kind: "keys"; accelerator: string; caps: string[] };

/** The recorder's rule: Esc and Tab cancel, ⌫ clears, a modifier alone waits for the key, a key
 * without ⌘, ⌥ or ⌃ would stop typing in other apps (F-keys are free), and ⌘ with one key belongs
 * to the apps' own commands. */
export function recorderStep(e: Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">): RecorderStep {
  const mods = modifiersOf(e);
  if (["Shift", "Control", "Alt", "Meta"].includes(e.key)) return { kind: "held", mods };
  if (!mods.length && (e.key === "Escape" || e.key === "Tab")) return { kind: "cancel" };
  if (!mods.length && (e.key === "Backspace" || e.key === "Delete")) return { kind: "clear" };
  const key = keyOfCode(e.code);
  if (!key) return { kind: "ignore" };
  const accelerator = acceleratorOf(mods, key);
  const caps = capsOf(accelerator);
  if (!mods.some((m) => m !== "⇧") && !/^F\d+$/.test(key)) return { kind: "refuse", reason: "needsModifier", caps };
  if (mods.length === 1 && mods[0] === "⌘") return { kind: "refuse", reason: "commandAlone", caps };
  return { kind: "keys", accelerator, caps };
}

/** The combinations macOS keeps for itself, by canonical accelerator, with the locale key of the
 * sentence that says what macOS does with it (src/locales/<lang>.json). */
export const SYSTEM_SHORTCUTS: Record<string, string> = {
  "Shift+Super+3": "settings.shortcuts.system.screenFile",
  "Shift+Super+4": "settings.shortcuts.system.areaFile",
  "Shift+Super+5": "settings.shortcuts.system.toolbar",
  "Shift+Super+6": "settings.shortcuts.system.touchBar",
  "Control+Shift+Super+3": "settings.shortcuts.system.screenCopy",
  "Control+Shift+Super+4": "settings.shortcuts.system.areaCopy",
  "Super+SPACE": "settings.shortcuts.system.spotlight",
  "Alt+Super+SPACE": "settings.shortcuts.system.finderSearch",
  "Control+SPACE": "settings.shortcuts.system.inputSource",
  "Control+Super+SPACE": "settings.shortcuts.system.characterViewer",
  "Control+Super+Q": "settings.shortcuts.system.lockScreen",
  "Shift+Super+Q": "settings.shortcuts.system.logOut",
  "Super+TAB": "settings.shortcuts.system.switchApps",
  "Alt+Super+D": "settings.shortcuts.system.dock",
};

/** The locale key of what macOS does with these keys, or `null` when macOS leaves them free. */
export function systemShortcut(accelerator: string | null | undefined): string | null {
  const c = canonical(accelerator);
  return c ? (SYSTEM_SHORTCUTS[c] ?? null) : null;
}

/** The other action that holds the same keys as `accelerator`, if any. */
export function holderOf(shortcuts: Shortcuts, accelerator: string, except: ShortcutAction): ShortcutAction | null {
  const wanted = canonical(accelerator);
  if (!wanted) return null;
  return SHORTCUT_ACTIONS.find((action) => action !== except && canonical(shortcuts[action]) === wanted) ?? null;
}
