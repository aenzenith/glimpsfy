// Agents: the local MCP server's requests as the approval card shows them, the agents that
// connected, their timed permissions and the log of requests. Mirrors
// src-tauri/src/commands/agents.rs (D34).
//
// Nothing reaches an agent without the user: a request waits on the card in the toast window, and
// only that window may answer it (`agents_answer`), once the card has shown the request whole for
// 0.6 s and with a click the hardware made. The card never holds what would be sent; its thumbnail
// is a small picture of it.

import type { Selection } from "./capture";
import { call, on, type EpochMs } from "./core";

/** What a request asks for; `text` is the text in an area, and no image goes. */
export type AgentAskKind = "window" | "display" | "area" | "text";

/** One request as the card shows it. */
export type AgentRequest = {
  id: number;
  connection: number;
  /** The name the agent gave itself, which the user may not take on trust. */
  agent: string | null;
  /** What the system says about the agent's process, such as "Terminal · ~/projects/glimpsfy". */
  process: string | null;
  kind: AgentAskKind;
  /** The user selects the area: the card's Allow becomes Select area. */
  pick: boolean;
  /** The window, display or app under the area, such as "Terminal · glimpsfy — zsh". */
  target: string | null;
  /** The size of what would be sent: the image in pixels, or a text request's area in points. */
  width: number | null;
  height: number | null;
  /** A PNG data URL of at most 152 × 104 of what would be sent (the area, for text). */
  thumbnail: string | null;
  /** A text request's recognised lines over the thumbnail, as fractions of its size:
   * `[x, y, width, height]`. */
  boxes: [number, number, number, number][];
  /** The time left before it times out. */
  leftMs: number;
};

/** The card's keys as accelerators; a key another app or action holds is `null`. */
export type AgentKeys = {
  allow: string | null;
  deny: string | null;
  timed: string | null;
  previous: string | null;
  next: string | null;
};

/** The requests waiting for the user, oldest first, with the one the card shows. */
export type AgentQueue = {
  requests: AgentRequest[];
  shown: number | null;
  keys: AgentKeys;
};

/** One agent that connected since the app started. */
export type AgentConnection = {
  connection: number;
  agent: string | null;
  process: string | null;
  connected: boolean;
  /** Its requests since midnight. */
  requestsToday: number;
  lastRequestMs: EpochMs | null;
  /** The time its timed permission has left. */
  grantLeftMs: number | null;
};

/** Settings › Agents. */
export type AgentsState = {
  /** The feature exists on this platform (macOS only). */
  available: boolean;
  enabled: boolean;
  /** The socket is open. */
  listening: boolean;
  /** Why the socket could not be opened, in English. */
  error: string | null;
  /** The line an agent's settings take, such as `claude mcp add glimpsfy -- … mcp`. */
  command: string;
  connections: AgentConnection[];
  queue: AgentQueue;
};

/** How a request ended: `allowed` on the card, `timed` by a timed permission, `cancelled` by the
 * agent (or the agent went away). */
export type AgentOutcome = "allowed" | "timed" | "denied" | "timedOut" | "locked" | "queueFull" | "cancelled" | "failed";

export type AgentLogEntry = {
  atMs: EpochMs;
  agent: string | null;
  kind: AgentAskKind;
  target: string | null;
  outcome: AgentOutcome;
};

/** What an agent's toast tells (`agentSent`, `agentTimedOut`, `agentLocked`). */
export type AgentToast = {
  agent: string | null;
  /** The connection whose timed permission the toast's lock button takes back. */
  connection: number;
  kind: AgentAskKind;
  /** The minutes the timed permission has left, rounded up. */
  minutesLeft: number | null;
  /** How many requests the screen lock refused. */
  count: number;
  /** The names of the agents whose requests the lock refused, each once. */
  agents: string[];
};

export type AgentAnswer = "allow" | "deny" | "allowTimed";

/** The waiting requests, the shown one or the card's keys changed. Payload: `AgentQueue`. */
export const AGENTS_QUEUE_CHANGED = "agents://queue-changed";
/** An agent connected or left, a timed permission began or ended, or the socket opened or closed.
 * Payload: `AgentsState`. */
export const AGENTS_CHANGED = "agents://changed";
/** A line was added to the log or the log was cleared. No payload. */
export const AGENTS_LOG_CHANGED = "agents://log-changed";

// ----------------------------------------------------------------------------- commands

export const agents_state = () => call<AgentsState>("agents_state");
/** Answers the request the card shows; only the toast window may, and the core refuses an answer
 * that comes before the card showed the request whole for 0.6 s. */
export const agents_answer = (id: number, answer: AgentAnswer) => call<void>("agents_answer", { id, answer });
/** Shows another waiting request on the card; `false` when it is no longer waiting. */
export const agents_show = (id: number) => call<boolean>("agents_show", { id });
/** The overlay hands over the selection a request asked the user for, and the capture closes. */
export const agents_take_selection = (selection: Selection) => call<void>("agents_take_selection", { selection });
/** Ends a connection's timed permission; `false` when none ran. */
export const agents_revoke = (connection: number) => call<boolean>("agents_revoke", { connection });
/** Closes a connection; its waiting requests end. */
export const agents_disconnect = (connection: number) => call<boolean>("agents_disconnect", { connection });
/** The log of the last 7 days, newest first. */
export const agents_log = () => call<AgentLogEntry[]>("agents_log");
export const agents_clear_log = () => call<void>("agents_clear_log");
/** The line an agent's settings take to start the server. */
export const agents_command = () => call<string>("agents_command");

export const onAgentsQueueChanged = (handler: (queue: AgentQueue) => void) => on<AgentQueue>(AGENTS_QUEUE_CHANGED, handler);
export const onAgentsChanged = (handler: (state: AgentsState) => void) => on<AgentsState>(AGENTS_CHANGED, handler);
export const onAgentsLogChanged = (handler: () => void) => on<null>(AGENTS_LOG_CHANGED, () => handler());
