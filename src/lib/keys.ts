// Keyboard shortcuts as the pages see them: accelerators (the strings Settings keeps, such as
// "Shift+CommandOrControl+2") turned into key caps, key presses turned into accelerators, the rule
// the recorder applies, the combinations the system keeps for itself, and the in-app chords the
// pages show and listen for. The store applies the same "one action per key" rule
// (`Shortcuts::resolve_clashes` in src-tauri/src/settings.rs), and `canonical` here matches its
// `canonical_accelerator`.
//
// Inside, a modifier is one of macOS's symbols: ⌃ is Control, ⌥ is Alt, ⇧ is Shift and ⌘ is the
// system key (Command on macOS, Windows' Win key, Linux's Super). What the user reads follows the
// platform: macOS's symbols in macOS's order, or Ctrl, Alt, Shift and Win or Super written out.
// The pages write their own chords ("⇧⌘S") in macOS's form, where ⌘ is the command key, which is
// Ctrl on Windows and Linux; `chordCaps` and `chord` put them in the platform's words.

import { platform } from "./platform";
import type { Shortcuts } from "./settings";

/** The modifiers in the order macOS prints them. */
export const MODIFIERS = ["⌃", "⌥", "⇧", "⌘"] as const;
export type Modifier = (typeof MODIFIERS)[number];

const mac = (): boolean => platform.os === "macos";

/** The modifiers in the order Windows and Linux print them: the system key first, as Windows'
 * own documentation writes Win + Shift + S. */
const PC_ORDER: readonly Modifier[] = ["⌘", "⌃", "⌥", "⇧"];

/** A modifier's cap on this platform. */
function modifierCap(m: Modifier): string {
  if (mac()) return m;
  if (m === "⌘") return platform.os === "windows" ? "Win" : "Super";
  return { "⌃": "Ctrl", "⌥": "Alt", "⇧": "Shift" }[m];
}

/** The modifiers in this platform's order. */
const ordered = (mods: Iterable<Modifier>): Modifier[] => {
  const held = new Set(mods);
  return (mac() ? MODIFIERS : PC_ORDER).filter((m) => held.has(m));
};

/** The caps of held modifiers, in this platform's order and words. */
export const capsOfModifiers = (mods: readonly Modifier[]): string[] => ordered(mods).map(modifierCap);

/** Caps as one string for a sentence or a label: "⇧⌘2" on macOS, "Ctrl+Shift+2" elsewhere. */
export const joinCaps = (caps: readonly string[]): string => caps.join(mac() ? "" : "+");

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
      "intervalCapture",
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
  { group: "app", actions: ["openHistory", "openClipboardInEditor", "pinClipboard"] },
] as const;

export type ShortcutAction = (typeof SHORTCUT_GROUPS)[number]["actions"][number];
export const SHORTCUT_ACTIONS: readonly ShortcutAction[] = SHORTCUT_GROUPS.flatMap((g) => g.actions);

/** A key's cap for a key name of an accelerator ("2", "A", "Space", "ArrowUp", "F5"…), on macOS. */
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
  PRINTSCREEN: "PrtSc",
};

/** The keys Windows and Linux write out where macOS draws a symbol. */
const PC_CAPS: Record<string, string> = {
  ENTER: "Enter",
  RETURN: "Enter",
  TAB: "Tab",
  HOME: "Home",
  END: "End",
  PAGEUP: "PgUp",
  PAGEDOWN: "PgDn",
  BACKSPACE: "Backspace",
  DELETE: "Del",
};

/** A key's cap on this platform, from its accelerator name in capitals. */
function keyCap(key: string): string {
  if (!mac()) {
    if (key === "PRINTSCREEN") return platform.os === "windows" ? "PrtSc" : "Print";
    if (PC_CAPS[key]) return PC_CAPS[key];
  }
  return CAPS[key] ?? key.replace(/^NUMPAD/, "");
}

/** The accelerator key name for a `KeyboardEvent.code`, or `null` for a key a shortcut cannot use. */
export function keyOfCode(code: string): string | null {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit\d$/.test(code)) return code.slice(5);
  if (/^Numpad\d$/.test(code)) return `Numpad${code.slice(6)}`;
  if (/^F\d{1,2}$/.test(code)) return code;
  // SCOPE.md: on Windows, the Print Screen key can be assigned to the area capture.
  if (code === "PrintScreen") return code;
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
    else if (["commandorcontrol", "commandorctrl", "cmdorctrl", "cmdorcontrol"].includes(token)) mods.add(mac() ? "⌘" : "⌃");
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

/** The caps of an accelerator in this platform's order and words, such as ["⇧", "⌘", "2"] on
 * macOS and ["Ctrl", "Shift", "2"] on Windows. */
export function capsOf(accelerator: string | null | undefined): string[] {
  if (!accelerator) return [];
  const parsed = parse(accelerator);
  if (!parsed) return [];
  const caps = capsOfModifiers([...parsed.mods]);
  if (parsed.key) caps.push(keyCap(parsed.key));
  return caps;
}

/** The caps as one string, such as "⇧⌘2" or "Ctrl+Shift+2", for sentences. */
export const capsText = (accelerator: string | null | undefined) => joinCaps(capsOf(accelerator));

/** The modifiers held in a key event, in macOS order. */
export function modifiersOf(e: Pick<KeyboardEvent, "ctrlKey" | "altKey" | "shiftKey" | "metaKey">): Modifier[] {
  const held: Modifier[] = [];
  if (e.ctrlKey) held.push("⌃");
  if (e.altKey) held.push("⌥");
  if (e.shiftKey) held.push("⇧");
  if (e.metaKey) held.push("⌘");
  return held;
}

/** The accelerator for modifiers and a key. The command key (⌘ on macOS, Ctrl elsewhere) is
 * written `CommandOrControl`, so one settings file means the same keys on every system; the other
 * system key is `Control` on macOS and `Super` (Win) elsewhere. */
export function acceleratorOf(mods: Modifier[], key: string): string {
  const names: Record<Modifier, string> = mac()
    ? { "⌃": "Control", "⌥": "Alt", "⇧": "Shift", "⌘": "CommandOrControl" }
    : { "⌃": "CommandOrControl", "⌥": "Alt", "⇧": "Shift", "⌘": "Super" };
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
 * without ⌘, ⌥ or ⌃ (Ctrl, Alt or Win) would stop typing in other apps (F-keys and Print Screen
 * are free), and the command key (⌘, or Ctrl on Windows and Linux) with one key belongs to the
 * apps' own commands. */
export function recorderStep(e: Pick<KeyboardEvent, "key" | "code" | "ctrlKey" | "altKey" | "shiftKey" | "metaKey">): RecorderStep {
  const mods = modifiersOf(e);
  if (["Shift", "Control", "Alt", "Meta"].includes(e.key)) return { kind: "held", mods };
  if (!mods.length && (e.key === "Escape" || e.key === "Tab")) return { kind: "cancel" };
  if (!mods.length && (e.key === "Backspace" || e.key === "Delete")) return { kind: "clear" };
  // Some webviews leave `code` empty for Print Screen and name the key only.
  const key = keyOfCode(e.code) ?? (e.key === "PrintScreen" ? "PrintScreen" : null);
  if (!key) return { kind: "ignore" };
  const accelerator = acceleratorOf(mods, key);
  const caps = capsOf(accelerator);
  if (!mods.some((m) => m !== "⇧") && !/^F\d+$/.test(key) && key !== "PrintScreen") return { kind: "refuse", reason: "needsModifier", caps };
  if (mods.length === 1 && mods[0] === (mac() ? "⌘" : "⌃")) return { kind: "refuse", reason: "commandAlone", caps };
  return { kind: "keys", accelerator, caps };
}

/** The combinations macOS keeps for itself, by canonical accelerator, with the locale key of the
 * sentence that says what macOS does with it (src/locales/<lang>.json). */
const MAC_SHORTCUTS: Record<string, string> = {
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

/** The combinations Windows keeps for itself, by canonical accelerator (Super is the Win key).
 * Ctrl+Alt+Del and Alt+Tab are left out: the recorder never takes Delete or Tab. Print Screen is
 * a hint, since it is Windows' only while its own setting holds it, and the core reports a key
 * that the system refused anyway. */
const WINDOWS_SHORTCUTS: Record<string, string> = {
  "Super+L": "settings.shortcuts.system.win.lock",
  "Super+D": "settings.shortcuts.system.win.desktop",
  "Alt+F4": "settings.shortcuts.system.win.close",
  PRINTSCREEN: "settings.shortcuts.system.win.printScreen",
  "Shift+Super+S": "settings.shortcuts.system.win.snip",
  "Super+PRINTSCREEN": "settings.shortcuts.system.win.screenFile",
};

/** What most Linux desktops keep, worded as what they usually do: each desktop has its own list. */
const LINUX_SHORTCUTS: Record<string, string> = {
  PRINTSCREEN: "settings.shortcuts.system.linux.printScreen",
  "Super+L": "settings.shortcuts.system.linux.lock",
  "Alt+F4": "settings.shortcuts.system.linux.close",
};

/** The combinations this platform keeps for itself. */
export const systemShortcuts = (): Record<string, string> =>
  platform.os === "windows" ? WINDOWS_SHORTCUTS : platform.os === "linux" ? LINUX_SHORTCUTS : MAC_SHORTCUTS;

/** The locale key of what the system does with these keys, or `null` when it leaves them free. */
export function systemShortcut(accelerator: string | null | undefined): string | null {
  const c = canonical(accelerator);
  return c ? (systemShortcuts()[c] ?? null) : null;
}

// ---------------------------------------------------------------------------- in-app chords

/** The macOS symbols of a chord the pages write, and what Windows and Linux call them. The command
 * key ⌘ is Ctrl there, and the deleting keys are written out. */
const PC_CHORD: Record<string, string> = { "⌘": "Ctrl", "⌃": "Ctrl", "⌥": "Alt", "⇧": "Shift", "⌫": "Backspace", "⌦": "Del", "↩": "Enter", "⇥": "Tab" };
const PC_CHORD_ORDER = ["Ctrl", "Alt", "Shift"];

/** The caps of an in-app chord written in macOS's form (["⇧", "⌘", "S"]) in this platform's words
 * and order (["Ctrl", "Shift", "S"]). Caps already in the platform's words pass unchanged, so a
 * component can run every list through it. */
export function chordCaps(caps: readonly string[]): string[] {
  if (mac()) return [...caps];
  const words = caps.map((c) => PC_CHORD[c] ?? c);
  const mods = PC_CHORD_ORDER.filter((m) => words.includes(m));
  // A list that leads with the system key (from `capsOf`) is in the platform's order already.
  if (!mods.length || words[0] === "Win" || words[0] === "Super") return words;
  return [...mods, ...words.filter((w) => !PC_CHORD_ORDER.includes(w))];
}

/** An in-app chord written in macOS's form ("⇧⌘L", "⌘,"), as this platform writes it
 * ("Ctrl+Shift+L", "Ctrl+,"). Delete's ⌘⌫ becomes the platform's own delete chord. */
export function chord(text: string): string {
  if (mac()) return text;
  if (text === "⌘⌫") return deleteChord();
  const caps: string[] = [];
  let rest = text;
  while (rest && "⌘⌃⌥⇧".includes(rest[0]!)) {
    caps.push(rest[0]!);
    rest = rest.slice(1);
  }
  if (rest) caps.push(rest);
  return joinCaps(chordCaps(caps));
}

/** The chord that deletes the picked captures: ⌘⌫ on macOS, Delete alone on Windows and Linux,
 * as their file managers do. */
export const deleteChord = (): string => (mac() ? "⌘⌫" : "Del");

/** Whether the command key is held: ⌘ on macOS, Ctrl on Windows and Linux, where ⌘ does not
 * exist and the Win key belongs to the system. */
export const commandHeld = (e: Pick<KeyboardEvent | MouseEvent, "metaKey" | "ctrlKey">): boolean => (mac() ? e.metaKey : e.ctrlKey);

/** The `KeyboardEvent.key` of the command key going down or up by itself. */
export const commandKeyName = (): string => (mac() ? "Meta" : "Control");

/** An `aria-keyshortcuts` value written for macOS ("Shift+Meta+C"), with Control for Meta on
 * Windows and Linux. */
export const ariaKeys = (keys: string): string => (mac() ? keys : keys.replace(/\bMeta\b/g, "Control"));

/** The other action that holds the same keys as `accelerator`, if any. */
export function holderOf(shortcuts: Shortcuts, accelerator: string, except: ShortcutAction): ShortcutAction | null {
  const wanted = canonical(accelerator);
  if (!wanted) return null;
  return SHORTCUT_ACTIONS.find((action) => action !== except && canonical(shortcuts[action]) === wanted) ?? null;
}
