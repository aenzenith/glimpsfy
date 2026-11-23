// The words and the monogram for an agent and what it asks for, which the approval card, the
// agents' toasts and Settings › Agents share (D34).

import type { AgentAskKind } from "./api/agents";
import { language, t, type Key } from "./i18n";

/** The name the agent gave itself, or the words for one that gave none. */
export const agentName = (name: string | null | undefined) => name?.trim() || t("agent.unknown");

/** Two letters for the agent: the first letters of its first two words ("Claude Code" → "CC"),
 * or the first two letters of a one-word name ("Cursor" → "Cu"). */
export function monogram(name: string): string {
  const words = name.split(/[\s()[\]{}\-_.,:;/\\|]+/u).filter((word) => /[\p{L}\p{N}]/u.test(word));
  const first = (word: string) => Array.from(word)[0] ?? "";
  if (words.length >= 2) return (first(words[0]!) + first(words[1]!)).toUpperCase();
  const letters = Array.from(words[0] ?? name);
  return `${(letters[0] ?? "?").toUpperCase()}${letters[1] ?? ""}`;
}

const KIND_ICON: Record<AgentAskKind, string> = { window: "window", display: "display", area: "area", text: "ocr" };
export const kindIcon = (kind: AgentAskKind) => KIND_ICON[kind];

const KIND_NAME: Record<AgentAskKind, Key> = {
  window: "agent.kind.window",
  display: "agent.kind.display",
  area: "agent.kind.area",
  text: "agent.kind.text",
};
/** What a request asks for in two words ("Pencere", "Alan metni"), for meta lines and the log. */
export const kindName = (kind: AgentAskKind) => t(KIND_NAME[kind]);

/** The agents' names as the language lists them ("Claude Code ve Zed"). */
export function nameList(names: readonly string[]): string {
  const shown = names.length ? names.map((name) => agentName(name)) : [agentName(null)];
  try {
    return new Intl.ListFormat(language.value, { type: "conjunction" }).format(shown);
  } catch {
    return shown.join(", ");
  }
}
