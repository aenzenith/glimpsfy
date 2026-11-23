// What the delete sheet offers and says for a set of entries: the two choices, which of them take
// effect, the summary sentence and the button. It lives apart from the sheet so a test can hold it
// to the truth about hosted copies: their link and deletion key leave with the entry, so a copy
// the sheet cannot delete (uploads off, or offline) is still shown, and the loss is spelled out.

import type { DeleteOptions, Entry } from "../lib/api/history";
import { t, tp } from "../lib/i18n";

export type DeleteFacts = {
  entries: readonly Entry[];
  /** The two choices as their checkboxes hold them. */
  file: boolean;
  hosted: boolean;
  /** Settings › General's switches, as `lib/network.ts` keeps them. */
  uploadsAllowed: boolean;
  online: boolean;
  hostName(provider: string): string;
};

export type DeletePlan = {
  saved: Entry[];
  uploaded: Entry[];
  host: string;
  /** Why the hosted copies cannot be deleted now (the help under their choice), or null. */
  locked: string | null;
  options: DeleteOptions;
  summary: string;
  label: string;
};

export function deletePlan(f: DeleteFacts): DeletePlan {
  const entries = f.entries;
  const n = entries.length;
  const many = n > 1;
  // A series' saved path is its folder of frames, which goes to the Trash with the series every
  // time (the store does that), so it is not one of the choices.
  const series = entries.filter((e) => e.kind === "series").length;
  const oneSeries = !many && entries[0]?.kind === "series";
  const saved = entries.filter((e) => e.savedPath && e.kind !== "series");
  const uploaded = entries.filter((e) => e.hosted);
  const host = [...new Set(uploaded.map((e) => (e.hosted ? f.hostName(e.hosted.provider) : "")))].filter(Boolean).join(", ");
  // Deleting a hosted copy asks its host, which Settings › General may forbid; the copies still
  // show, because deleting the entry forgets the only link and key Glimpsfy has for them.
  const locked = uploaded.length && !f.uploadsAllowed ? t(f.online ? "history.delete.hostLocked" : "history.delete.hostLockedOffline", { host }) : null;

  const s = saved.length > 0;
  const u = uploaded.length > 0;
  const fileOn = s && f.file;
  const hostOn = u && f.hosted && locked === null;

  function own(): string {
    if (many) {
      if (fileOn && hostOn) return tp("history.delete.many.both", { n, host });
      if (fileOn) return u ? tp("history.delete.many.fileKeepHost", { n, host }) : tp("history.delete.many.file", { n });
      if (hostOn) return s ? t("history.delete.many.hostKeepFile", { n, host }) : tp("history.delete.many.host", { n, host });
      return s || u ? t("history.delete.many.none", { n }) : tp("history.delete.many.own", { n });
    }
    if (s && u) {
      if (fileOn && hostOn) return tp("history.delete.both");
      if (fileOn) return tp("history.delete.fileOnly", { host });
      if (hostOn) return t("history.delete.hostOnly", { host });
      return t("history.delete.none");
    }
    if (s) return fileOn ? tp("history.delete.file") : t("history.delete.keepFile");
    if (u) return hostOn ? tp("history.delete.ownAndHost", { host }) : tp("history.delete.ownKeepHost", { host });
    return tp("history.delete.own");
  }

  function summary(): string {
    if (oneSeries) return tp("history.series.deleteSummary");
    let rest = own();
    if (locked) rest = `${rest} ${t(many ? "history.delete.many.linkLost" : "history.delete.linkLost")}`;
    return many && series ? `${rest} ${tp("history.series.deleteSummaryMany", { n: series })}` : rest;
  }

  function label(): string {
    if (oneSeries) return tp("history.series.deleteLabel");
    if (fileOn && hostOn) return t("history.delete.label.everywhere");
    if (hostOn) return s ? t("history.delete.label.host", { host }) : t("history.delete.label.everywhere");
    if (fileOn) return tp("history.delete.label.mac");
    if (s || u) return t("history.delete.label.history");
    return tp("history.delete.label.mac");
  }

  return { saved, uploaded, host, locked, options: { trashSavedFile: fileOn, deleteHostedCopy: hostOn }, summary: summary(), label: label() };
}
