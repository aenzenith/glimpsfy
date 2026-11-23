// The bridge every typed wrapper in `src/lib/api/` goes through. A command's failure arrives as
// `{ code, message }` (src-tauri/src/commands/error.rs) and is rethrown here as an `ApiError`, so
// callers branch on `error.code` and never parse a message.

import { invoke, type InvokeArgs } from "@tauri-apps/api/core";
import { emit, listen, type UnlistenFn } from "@tauri-apps/api/event";
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";

export type ErrorCode =
  | "not_implemented"
  | "permission_denied"
  | "not_found"
  | "invalid_argument"
  | "busy"
  | "io"
  | "capture"
  | "provider"
  | "not_confirmed"
  | "keychain"
  | "uploads_off"
  | "offline"
  | "cancelled"
  | "internal";

export class ApiError extends Error {
  readonly code: ErrorCode;

  constructor(code: ErrorCode, message: string) {
    super(message);
    this.name = "ApiError";
    this.code = code;
  }
}

function isCommandError(value: unknown): value is { code: ErrorCode; message: string } {
  return typeof value === "object" && value !== null && "code" in value && "message" in value;
}

/** Calls a command; a failure becomes an `ApiError` with the command's own code. */
export async function call<T>(command: string, args?: InvokeArgs): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch (error) {
    if (isCommandError(error)) throw new ApiError(error.code, error.message);
    throw new ApiError("internal", String(error));
  }
}

/** Listens to an event with a typed payload; the returned function stops listening. It hears the
 * event whichever window it was sent to. */
export function on<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return listen<T>(event, (message) => handler(message.payload));
}

/** Listens to an event sent to this window (or to every window), and not to events the core
 * sends to another window, such as another display's `capture://started`. */
export function onWindow<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
  return getCurrentWebviewWindow().listen<T>(event, (message) => handler(message.payload));
}

/** Sends an event to every window, for events that start in the UI. */
export function send<T>(event: string, payload: T): Promise<void> {
  return emit(event, payload);
}

/** Milliseconds since the Unix epoch, the form every timestamp crosses the bridge in. */
export type EpochMs = number;
