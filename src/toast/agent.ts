// What the approval card and the agents' toasts need beyond the shared words (`src/lib/agents.ts`):
// the card's width, the time a request waits, the sentence of what a request asks for, and the
// size a thumbnail takes in its frame.

import type { AgentAskKind, AgentRequest } from "../lib/api/agents";
import { t, type Key } from "../lib/i18n";

/** The card's width, which the toast window always leaves room for (`windowFrame`), so the window
 * never resizes under a moving toast when the card comes. */
export const CARD_WIDTH = 340;
/** How long a request waits for an answer before the core denies it. */
export const ANSWER_MS = 60_000;

const ASK: Record<AgentAskKind, Key> = {
  window: "agent.ask.window",
  display: "agent.ask.display",
  area: "agent.ask.area",
  text: "agent.ask.text",
};

/** What the request asks for, as the sentence under the agent's name. */
export function askText(request: AgentRequest): string {
  if (request.pick) return t(request.kind === "text" ? "agent.ask.textPick" : "agent.ask.areaPick");
  return t(ASK[request.kind]);
}

/** The size of a picture of `width` × `height` drawn whole inside a `frameW` × `frameH` frame, or
 * `null` when its size is not known (it then fills the frame and keeps its own proportions). */
export function fitIn(width: number | null | undefined, height: number | null | undefined, frameW: number, frameH: number) {
  if (!width || !height) return null;
  let w = frameW;
  let h = (frameW * height) / width;
  if (h > frameH) {
    h = frameH;
    w = (frameH * width) / height;
  }
  return { width: `${Math.max(1, Math.round(w))}px`, height: `${Math.max(1, Math.round(h))}px` };
}
