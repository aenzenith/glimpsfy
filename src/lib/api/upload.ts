// Uploads: the providers and their settings forms, the first-upload confirmation, uploads with
// progress and cancellation, deletion from the host, connection tests, secrets and `.sxcu`
// import. Mirrors src-tauri/src/commands/upload.rs and crates/upload/src/{lib,schema,consent}.rs.
//
// Nothing leaves the Mac on its own: an upload, a test or a deletion starts only from a user
// action, and the core refuses each one (`permission_denied`) until the user confirmed the
// provider's destination once in `src/upload/FirstUploadDialog.vue`. A secret is sent once with
// `upload_set_secret` and goes to the keychain; it never comes back and is never in the settings.

import type { Selection } from "./capture";
import { call, on, type EpochMs, type ErrorCode } from "./core";
import type { EntryId } from "./history";

export type ProviderId = string;
export type UploadId = string;

// ------------------------------------------------------------------ the form (describe())

/** The four shapes of host page in Settings › Upload. */
export type FormShape = "anonymous" | "key" | "bucket" | "custom";

export type AccountNeed = "none" | "clientId" | "apiKey" | "accessKeys" | "custom";

/** `request`: Glimpsfy deletes with a request. `page`: the host's deletion page opens in the
 * browser. `none`: the host offers no way. */
export type DeletionKind = "none" | "request" | "page";

export type Visibility = "anyoneWithLink" | "bucketPolicy" | "serverOwner" | "notStated";

/** How long a host keeps an upload, as its documentation states it. */
export type Retention =
  | { kind: "noExpiry" }
  | { kind: "inactivity"; days: number }
  | { kind: "chosen"; setting: string; hours: number[] }
  | { kind: "bySize"; minDays: number; maxDays: number; maxBytes: number }
  | { kind: "optional"; setting: string }
  | { kind: "ownStorage" }
  | { kind: "notStated" };

/** What docs/PROVIDERS.md records about a public host, with the day it was read. */
export type HostFacts = {
  /** `YYYY-MM-DD`. */
  checkedOn: string;
  sources: string[];
  visibility: Visibility;
  retention: Retention;
  maxFileBytes: number | null;
  termsUrl: string | null;
};

export type FieldOption = {
  value: string;
  /** An `upload.*` key of src/locales/<lang>.json, or the text itself when `literal` (POST, PUT). */
  label: string;
  literal: boolean;
};

/** A check the form can run while the user types; its message key is `upload.rule.<rule>`.
 * The core runs the same checks, and more, in `upload_validate`, which is the one to trust. */
export type FieldRule =
  | { rule: "maxLength"; max: number }
  | { rule: "httpUrl" }
  | { rule: "tokens"; allowed: string[] }
  | { rule: "noWhitespace" };

/** The input of a row. Text, URL and pairs placeholders are literal examples; a secret row's
 * placeholder is a key of upload.json. */
export type FieldInput =
  | { kind: "text"; default: string | null; placeholder: string | null; monospace: boolean; tokens: string[] }
  | { kind: "secret"; placeholder: string | null }
  | { kind: "url"; default: string | null; placeholder: string | null }
  | { kind: "select"; options: FieldOption[]; default: string | null }
  | { kind: "number"; default: number | null; min: number | null; max: number | null; step: number | null }
  | { kind: "toggle"; default: boolean }
  /** With `secretValues`, the settings keep the names (a list of strings) and each value lives
   * in the keychain under `pairSecretKey(field, name)`. */
  | { kind: "pairs"; secretValues: boolean; namePlaceholder: string | null; valuePlaceholder: string | null };

export type SettingField = {
  /** The key the value is stored under in `settings.upload.providers[id]`, or in the keychain. */
  key: string;
  /** `upload.*` keys of src/locales/<lang>.json. */
  label: string;
  help: string | null;
  /** The heading of the group of rows, a key of upload.json. */
  section: string | null;
  required: boolean;
  /** The value lives in the keychain: send it with `upload_set_secret`, ask `upload_secret_status`. */
  secret: boolean;
  rules: FieldRule[];
} & FieldInput;

export type ProviderDescription = {
  id: ProviderId;
  /** The host's product name, which is never translated. */
  name: string;
  monogram: string;
  shape: FormShape;
  account: AccountNeed;
  /** The fixed domain of a public host; S3 and custom name theirs through `upload_preflight`. */
  domain: string | null;
  /** The key of the sentence that says who runs the host. */
  operator: string | null;
  website: string | null;
  /** Where the user gets their own key. */
  keyUrl: string | null;
  /** `null` for S3 and custom, whose rules are the user's own. */
  facts: HostFacts | null;
  maxFileBytes: number | null;
  linksExpire: boolean;
  supportsDelete: boolean;
  deletion: DeletionKind;
  /** Whether "Test the connection" makes sense. */
  testable: boolean;
  settings: SettingField[];
};

export type Pair = { name: string; value: string };

/** A stored value of a non-secret row: text, number, toggle, pairs, or the names of secret pairs. */
export type FieldValue = boolean | number | string | Pair[] | string[];

/** The keychain key of one value of a secret `pairs` row, such as `headers:Authorization`. */
export function pairSecretKey(field: string, name: string): string {
  return `${field}:${name}`;
}

export type Severity = "error" | "warning";

/** A problem of a configuration. `message` is a key of upload.json; `params` fill its
 * placeholders. An `error` stops uploads; a `warning` only informs. */
export type Issue = {
  field: string | null;
  severity: Severity;
  message: string;
  params: Record<string, string>;
};

// ------------------------------------------------------------------------------ consent

/** How long an upload made now stays on the host, as far as the host says. */
export type RetentionNow =
  | { kind: "hours"; hours: number }
  | { kind: "bySize"; minDays: number; maxDays: number }
  | { kind: "untilUnvisited"; days: number }
  | { kind: "noExpiry" }
  | { kind: "ownStorage" }
  | { kind: "notStated" };

/** What the first-upload dialog says about a destination. */
export type ConsentInfo = {
  provider: ProviderId;
  name: string;
  domain: string;
  bucket: string | null;
  operator: string | null;
  visibility: Visibility;
  retention: RetentionNow;
  checkedOn: string | null;
  termsUrl: string | null;
  maxFileBytes: number | null;
  /** The request would travel unencrypted to another machine. */
  plainHttp: boolean;
  confirmed: boolean;
  /** Pass it back to `upload_confirm_provider`. */
  consentKey: string;
};

export type UploadPreflight = {
  provider: ProviderId;
  consent: ConsentInfo;
  issues: Issue[];
};

// ------------------------------------------------------------------------------ uploads

export type DeletionHandle =
  | { kind: "url"; url: string }
  | { kind: "token"; token: string; url: string | null }
  | { kind: "objectKey"; key: string; endpoint: string; bucket: string };

export type DeleteOutcome = { kind: "deleted" } | { kind: "openPage"; url: string };

export type UploadResult = {
  uploadId: UploadId;
  provider: ProviderId;
  url: string;
  deletion: DeletionHandle | null;
  uploadedAtMs: EpochMs;
  /** When the host deletes the file on its own; `null` when it does not or does not say. */
  expiresAtMs: EpochMs | null;
};

export type UploadStarted = {
  uploadId: UploadId;
  provider: ProviderId;
  fileName: string;
  totalBytes: number;
};

export type ProviderTestResult = {
  provider: ProviderId;
  checkedAtMs: EpochMs;
  latencyMs: number;
  url: string | null;
  warnings: Issue[];
};

export type UploadProgress = {
  uploadId: UploadId;
  provider: ProviderId;
  sentBytes: number;
  totalBytes: number;
};

export type CopyAfterUpload = "link" | "markdown" | "nothing";

export type UploadDone = {
  result: UploadResult;
  /** The history entry that records the hosted copy: the one `upload_file` was given, or the one
   * an upload from the overlay recorded first. */
  entryId: EntryId | null;
  fileName: string;
  /** What reached the clipboard; `null` when nothing did. */
  copied: Exclude<CopyAfterUpload, "nothing"> | null;
  opened: boolean;
};

export type FailureKind =
  | "cancelled"
  | "provider"
  | "network"
  | "fileTooLarge"
  | "hostNotAllowed"
  | "notConfirmed"
  | "missingSecret"
  | "invalidConfig"
  | "keychain"
  | "io"
  | "other";

export type UploadFailed = {
  uploadId: UploadId;
  provider: ProviderId;
  /** `message` carries the host's own reason in English, with every secret cut out. */
  error: { code: ErrorCode; message: string };
  kind: FailureKind;
  /** The HTTP status of the host's refusal. */
  status: number | null;
  /** The file Retry sends again with `upload_file`; `null` when nothing is left to retry from. */
  path: string | null;
  /** The history entry the upload belongs to, which Retry passes on. */
  entryId: EntryId | null;
};

export type SxcuImported = {
  /** The custom provider's new settings, already saved. */
  values: Record<string, FieldValue>;
  /** Keychain keys that now hold the file's header, parameter and argument values. */
  secretKeys: string[];
  warnings: Issue[];
};

export type SxcuOutcome = {
  imported: SxcuImported | null;
  /** The row and the reason (a key of upload.json) of a refused file. */
  refusal: Issue | null;
};

/** Bytes sent so far, at most ten times a second; the first event has zero bytes. */
export const UPLOAD_PROGRESS = "upload://progress";
/** The host returned the link. */
export const UPLOAD_DONE = "upload://done";
/** The upload failed or was cancelled; no link exists. */
export const UPLOAD_FAILED = "upload://failed";

// ----------------------------------------------------------------------------- commands

export function upload_providers(): Promise<ProviderDescription[]> {
  return call("upload_providers");
}

/** The form's problems for `values` (or the saved values when `null`), missing secrets included. */
export function upload_validate(providerId: ProviderId, values: Record<string, FieldValue> | null): Promise<Issue[]> {
  return call("upload_validate", { providerId, values });
}

/** The provider an upload would use now (`null`: the default), its confirmation and its problems.
 * Fails with `invalid_argument` when no provider is named and none is the default. */
export function upload_preflight(providerId: ProviderId | null): Promise<UploadPreflight> {
  return call("upload_preflight", { providerId });
}

/** Records the user's confirmation of the destination the dialog showed. Fails with
 * `invalid_argument` when the settings point elsewhere by now; ask again then. */
export function upload_confirm_provider(providerId: ProviderId, consentKey: string): Promise<ConsentInfo> {
  return call("upload_confirm_provider", { providerId, consentKey });
}

/** Uploads the selection as a PNG; checks run before the capture closes. */
export function upload_capture(selection: Selection, providerId: ProviderId | null): Promise<UploadStarted> {
  return call("upload_capture", { selection, providerId });
}

/** Uploads an image or a recording from disk; with `entryId`, the history records the link. */
export function upload_file(path: string, providerId: ProviderId | null, entryId: EntryId | null = null): Promise<UploadStarted> {
  return call("upload_file", { path, providerId, entryId });
}

export function upload_cancel(uploadId: UploadId): Promise<void> {
  return call("upload_cancel", { uploadId });
}

/** Deletes the hosted copy of a history entry. A host with a deletion page gets the page opened
 * in the browser, and the call fails with `provider`, because the copy is gone only once the
 * user confirms there. */
export function upload_delete(entryId: EntryId): Promise<void> {
  return call("upload_delete", { entryId });
}

/** Sends a request to the host, so it needs the confirmation like an upload. */
export function upload_test(providerId: ProviderId): Promise<ProviderTestResult> {
  return call("upload_test", { providerId });
}

export function upload_set_secret(providerId: ProviderId, key: string, value: string): Promise<void> {
  return call("upload_set_secret", { providerId, key, value });
}

export function upload_remove_secret(providerId: ProviderId, key: string): Promise<void> {
  return call("upload_remove_secret", { providerId, key });
}

/** Whether the keychain holds the secret; the value itself never comes back. */
export function upload_secret_status(providerId: ProviderId, key: string): Promise<boolean> {
  return call("upload_secret_status", { providerId, key });
}

/** Imports a ShareX `.sxcu` file's text into the custom provider and saves it, or says why the
 * file was refused (nothing is saved then). */
export function upload_import_sxcu(text: string): Promise<SxcuOutcome> {
  return call("upload_import_sxcu", { text });
}

export function onUploadProgress(handler: (progress: UploadProgress) => void) {
  return on<UploadProgress>(UPLOAD_PROGRESS, handler);
}

export function onUploadDone(handler: (done: UploadDone) => void) {
  return on<UploadDone>(UPLOAD_DONE, handler);
}

export function onUploadFailed(handler: (failed: UploadFailed) => void) {
  return on<UploadFailed>(UPLOAD_FAILED, handler);
}
