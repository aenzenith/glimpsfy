// The overlay's tools and actions, in bar order, with their in-overlay keys (DESIGN.md, Annotation
// bars). `null` is a divider.

import type { Key } from "../lib/i18n";

export type ToolId =
  | "arrow"
  | "line"
  | "rect"
  | "ellipse"
  | "pen"
  | "highlighter"
  | "text"
  | "step"
  | "blur"
  | "pixelate"
  | "spotlight"
  | "sensitive"
  | "crop"
  | "undo"
  | "redo";

export type ActionId =
  | "beautify"
  | "copy"
  | "send"
  | "save"
  | "upload"
  | "copy-text"
  | "copy-subject"
  | "pin"
  | "scroll"
  | "record"
  | "close";

export type Tool = { id: ToolId; label: Key; key: string; momentary?: boolean };
export type Action = { id: ActionId; icon: string; label: Key; key: string };

export const TOOLS: (Tool | null)[] = [
  { id: "arrow", label: "tool.arrow", key: "A" },
  { id: "line", label: "tool.line", key: "L" },
  { id: "rect", label: "tool.rect", key: "R" },
  { id: "ellipse", label: "tool.ellipse", key: "O" },
  { id: "pen", label: "tool.pen", key: "P" },
  { id: "highlighter", label: "tool.highlighter", key: "H" },
  null,
  { id: "text", label: "tool.text", key: "T" },
  { id: "step", label: "tool.step", key: "N" },
  null,
  { id: "blur", label: "tool.blur", key: "B" },
  { id: "pixelate", label: "tool.pixelate", key: "X" },
  { id: "spotlight", label: "tool.spotlight", key: "S" },
  // Smart redaction (D28) opens its list instead of drawing; the page handles it.
  { id: "sensitive", label: "tool.sensitive", key: "D" },
  null,
  { id: "crop", label: "tool.crop", key: "C" },
  null,
  { id: "undo", label: "tool.undo", key: "⌘Z", momentary: true },
  { id: "redo", label: "tool.redo", key: "⇧⌘Z", momentary: true },
];

export const ACTIONS: (Action | null)[] = [
  // F is a plain key, like the tools' letters; it works during the drag too (B11).
  { id: "beautify", icon: "beautify", label: "beautify.title", key: "F" },
  null,
  { id: "copy", icon: "copy", label: "action.copy", key: "⌘C" },
  { id: "send", icon: "send", label: "action.send", key: "⌘↩" },
  { id: "save", icon: "save", label: "action.save", key: "⌘S" },
  { id: "upload", icon: "upload", label: "action.upload", key: "⌘U" },
  { id: "copy-text", icon: "text-recognition", label: "action.copyText", key: "⇧⌘C" },
  // macOS 14 only (`subjectLift`); the page leaves it out elsewhere.
  { id: "copy-subject", icon: "subject", label: "action.copySubject", key: "⌥⌘B" },
  { id: "pin", icon: "pin", label: "action.pin", key: "⌘P" },
  // macOS only (`scrollCapture`); the page leaves it out elsewhere. App.vue catches ⌘↓ before
  // the arrows move the selection.
  { id: "scroll", icon: "chevron-up-down", label: "action.scrollCapture", key: "⌘↓" },
  null,
  { id: "record", icon: "record", label: "action.record", key: "⌘R" },
  null,
  { id: "close", icon: "close", label: "action.close", key: "Esc" },
];

/** The actions that work in this build; the others say so when used. */
export const WORKING_ACTIONS: ActionId[] = [
  "beautify",
  "copy",
  "send",
  "save",
  "upload",
  "copy-text",
  "copy-subject",
  "pin",
  "scroll",
  "record",
  "close",
];

export const INKS = ["red", "orange", "yellow", "green", "cyan", "blue", "white", "black"] as const;

/** A tool's key for the keyboard, or `undefined` when no tool has it. */
export function toolForKey(key: string): Tool | undefined {
  const k = key.toUpperCase();
  return TOOLS.find((t): t is Tool => t !== null && !t.momentary && t.key === k);
}

/** The action of a ⌘ chord such as "⇧⌘C". */
export function actionForChord(chord: string): Action | undefined {
  return ACTIONS.find((a): a is Action => a !== null && a.key === chord);
}
