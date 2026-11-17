// What went wrong and what the user can do about it, for a command's error code. The toast, the
// popover and the pins word their failures with it.

import type { ErrorCode } from "../lib/api/core";
import { t, type Key } from "../lib/i18n";

const REASON: Partial<Record<ErrorCode, Key>> = {
  permission_denied: "error.permission_denied",
  not_implemented: "error.not_implemented",
  io: "toast.reason.io",
  not_found: "toast.reason.not_found",
  busy: "toast.reason.busy",
  capture: "toast.reason.capture",
  provider: "toast.reason.provider",
  invalid_argument: "toast.reason.invalid_argument",
  cancelled: "toast.reason.cancelled",
};

const FIX: Partial<Record<ErrorCode, Key>> = {
  permission_denied: "toast.fix.permission_denied",
  io: "toast.fix.io",
  not_found: "toast.fix.not_found",
  busy: "toast.fix.busy",
  capture: "toast.fix.capture",
  provider: "toast.fix.provider",
  internal: "toast.fix.other",
};

/** One sentence that says what went wrong. */
export function reasonText(code: ErrorCode | null | undefined): string {
  const key = code ? REASON[code] : undefined;
  return t(key ?? "error.other");
}

/** One sentence that says what to do next, or `null` when nothing helps (not implemented yet). */
export function fixText(code: ErrorCode | null | undefined): string | null {
  if (code === "not_implemented" || code === "cancelled") return null;
  const key = code ? FIX[code] : undefined;
  return t(key ?? "toast.fix.other");
}

/** The code of a thrown error, when it came from a command. */
export function codeOf(error: unknown): ErrorCode {
  return typeof error === "object" && error !== null && "code" in error ? ((error as { code: ErrorCode }).code ?? "internal") : "internal";
}
