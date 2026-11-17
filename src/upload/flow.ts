// The upload flow for any page that offers Upload (the overlay's ⌘U, the history, the editor):
// check the provider first, show the first-upload dialog when the destination is not confirmed
// yet, then start, and follow the upload through its events.
//
//   const ready = await prepareUpload(null);            // null: the default provider
//   if (ready.kind === "needsConsent") show <FirstUploadDialog :consent="ready.preflight.consent">,
//     and start after its `confirmed` event;
//   if (ready.kind === "ready") await upload_capture(selection, ready.preflight.provider);
//
// The core enforces the same order on its own: an upload to an unconfirmed destination fails with
// `permission_denied` and sends nothing.

import { reactive } from "vue";

import { ApiError } from "../lib/api/core";
import {
  onUploadDone,
  onUploadFailed,
  onUploadProgress,
  upload_preflight,
  type Issue,
  type ProviderId,
  type UploadDone,
  type UploadFailed,
  type UploadId,
  type UploadPreflight,
} from "../lib/api/upload";

export type Readiness =
  /** Confirmed and configured: start the upload. */
  | { kind: "ready"; preflight: UploadPreflight }
  /** Configured, and the user has not confirmed this destination yet: show the dialog. */
  | { kind: "needsConsent"; preflight: UploadPreflight }
  /** A setting stops it (a missing key, say): `issues` say which, for Settings › Upload. */
  | { kind: "blocked"; preflight: UploadPreflight; issues: Issue[] }
  /** No provider is named and none is the default: send the user to Settings › Upload. */
  | { kind: "noProvider" };

/** What stands between the user and an upload to `providerId` (or to the default provider). */
export async function prepareUpload(providerId: ProviderId | null): Promise<Readiness> {
  let preflight: UploadPreflight;
  try {
    preflight = await upload_preflight(providerId);
  } catch (error) {
    if (error instanceof ApiError && error.code === "invalid_argument" && providerId === null) return { kind: "noProvider" };
    throw error;
  }
  const errors = preflight.issues.filter((issue) => issue.severity === "error");
  if (errors.length > 0) return { kind: "blocked", preflight, issues: errors };
  return preflight.consent.confirmed ? { kind: "ready", preflight } : { kind: "needsConsent", preflight };
}

/** One upload as the pages follow it. */
export type TrackedUpload = {
  uploadId: UploadId;
  provider: ProviderId;
  sentBytes: number;
  totalBytes: number;
  phase: "uploading" | "done" | "failed";
  done: UploadDone | null;
  failed: UploadFailed | null;
};

/** Follows every upload of the app through its events. `uploads` is reactive; call `stop` when
 * the page goes away. */
export async function trackUploads(): Promise<{ uploads: Map<UploadId, TrackedUpload>; stop: () => void }> {
  const uploads = reactive(new Map<UploadId, TrackedUpload>());
  const entry = (uploadId: UploadId, provider: ProviderId): TrackedUpload => {
    let upload = uploads.get(uploadId);
    if (!upload) {
      upload = { uploadId, provider, sentBytes: 0, totalBytes: 0, phase: "uploading", done: null, failed: null };
      uploads.set(uploadId, upload);
      upload = uploads.get(uploadId) as TrackedUpload;
    }
    return upload;
  };
  const stops = await Promise.all([
    onUploadProgress((progress) => {
      const upload = entry(progress.uploadId, progress.provider);
      if (upload.phase !== "uploading") return;
      upload.sentBytes = progress.sentBytes;
      upload.totalBytes = progress.totalBytes;
    }),
    onUploadDone((done) => {
      const upload = entry(done.result.uploadId, done.result.provider);
      upload.phase = "done";
      upload.sentBytes = upload.totalBytes;
      upload.done = done;
    }),
    onUploadFailed((failed) => {
      const upload = entry(failed.uploadId, failed.provider);
      upload.phase = "failed";
      upload.failed = failed;
    }),
  ]);
  return { uploads, stop: () => stops.forEach((stop) => stop()) };
}
