// The sentences of the upload area, built from what the core reports: the first-upload dialog's
// summary of a host, a host's terms for Settings › Upload, and the outcome of an upload for the
// toast or the history. Every figure comes from `describe()` and `ConsentInfo`, which carry what
// docs/PROVIDERS.md recorded; a host that states nothing gets a sentence that says so.

import { language, t, type Key } from "../lib/i18n";
import type {
  ConsentInfo,
  FieldOption,
  FormShape,
  Issue,
  ProviderDescription,
  Retention,
  RetentionNow,
  UploadDone,
  UploadFailed,
} from "../lib/api/upload";

/** A key of upload.json that arrived as a string from the core. */
export function tk(key: string, params: Record<string, string | number> = {}): string {
  return t(key as Key, params);
}

/** An option's label: translated, or the text itself for a literal one (POST). */
export function optionLabel(option: FieldOption): string {
  return option.literal ? option.label : tk(option.label);
}

/** An issue's sentence with its parameters. */
export function issueText(issue: Issue): string {
  return tk(issue.message, issue.params);
}

/** "200 MB", "512 MiB", "1 GB": binary units only when the host states its limit in them. */
export function formatBytes(bytes: number): string {
  const mib = 1024 * 1024;
  const number = (value: number) => new Intl.NumberFormat(language.value, { maximumFractionDigits: 1 }).format(value);
  if (bytes % mib === 0 && bytes % 1_000_000 !== 0) {
    return bytes >= 1024 * mib ? `${number(bytes / (1024 * mib))} GiB` : `${number(bytes / mib)} MiB`;
  }
  if (bytes >= 1_000_000_000) return `${number(bytes / 1_000_000_000)} GB`;
  if (bytes >= 1_000_000) return `${number(bytes / 1_000_000)} MB`;
  if (bytes >= 1000) return `${number(Math.round(bytes / 1000))} KB`;
  return `${bytes} B`;
}

/** "16 November 2026" from "2026-11-16". */
export function formatDay(day: string): string {
  const [year, month, date] = day.split("-").map(Number);
  if (!year || !month || !date) return day;
  return new Intl.DateTimeFormat(language.value, { day: "numeric", month: "long", year: "numeric" }).format(
    new Date(Date.UTC(year, month - 1, date)),
  );
}

/** "72 hours", "3 days" or "2 years", whichever reads naturally. */
export function formatPeriod(hours: number): string {
  if (hours % (24 * 365) === 0) {
    const n = hours / (24 * 365);
    return n === 1 ? t("upload.period.year") : t("upload.period.years", { n });
  }
  if (hours % 24 === 0 && hours >= 48) return t("upload.period.days", { n: hours / 24 });
  if (hours === 24) return t("upload.period.day");
  return hours === 1 ? t("upload.period.hour") : t("upload.period.hours", { n: hours });
}

/** The sentence about how long the host keeps an upload made now. */
export function retentionText(name: string, retention: RetentionNow): string {
  switch (retention.kind) {
    case "hours":
      return t("upload.consent.retention.hours", { name, period: formatPeriod(retention.hours) });
    case "bySize":
      return t("upload.consent.retention.bySize", { name, minDays: retention.minDays, maxDays: retention.maxDays });
    case "untilUnvisited":
      return t("upload.consent.retention.untilUnvisited", { name, period: formatPeriod(retention.days * 24) });
    case "noExpiry":
      return t("upload.consent.retention.noExpiry", { name });
    case "ownStorage":
      return t("upload.consent.retention.ownStorage");
    case "notStated":
      return t("upload.consent.retention.notStated", { name });
  }
}

/** What a connection test of this shape sends, for the dialog that confirms it. */
export type TestKind = "image" | "bucket" | "frontPage";

export function testKind(shape: FormShape): TestKind {
  if (shape === "bucket") return "bucket";
  if (shape === "anonymous") return "frontPage";
  return "image";
}

/** The first-upload dialog's body: where the image goes, who runs the host, who can open the
 * file and how long it stays, in that order. */
export function consentSentences(consent: ConsentInfo, purpose: "upload" | "test" = "upload", test: TestKind = "image"): string[] {
  const { name, domain, bucket } = consent;
  const sentences: string[] = [];
  if (purpose === "test") {
    if (test === "bucket" && bucket) sentences.push(t("upload.consent.testBucket", { bucket, domain }));
    else if (test === "frontPage") sentences.push(t("upload.consent.testFrontPage", { domain }));
    else sentences.push(t("upload.consent.testImage", { domain }));
  } else {
    sentences.push(
      bucket ? t("upload.consent.destinationBucket", { bucket, domain }) : t("upload.consent.destination", { domain }),
    );
  }
  if (consent.operator) sentences.push(tk(consent.operator));
  // A test file is deleted again or expires at once, so only an upload gets these two.
  if (purpose === "upload") {
    sentences.push(tk(`upload.consent.visibility.${consent.visibility}`, { name }));
    sentences.push(retentionText(name, consent.retention));
  }
  if (consent.plainHttp) sentences.push(t("upload.consent.plainHttp"));
  return sentences;
}

/** One line of a host's terms in Settings › Upload, with the kit icon it goes with. */
export type TermLine = { icon: string; text: string };

/** A public host's terms, as docs/PROVIDERS.md recorded them; empty for S3 and custom. */
export function hostTerms(description: ProviderDescription): TermLine[] {
  const facts = description.facts;
  if (!facts) return [];
  const name = description.name;
  const lines: TermLine[] = [{ icon: "eye", text: tk(`upload.terms.visibility.${facts.visibility}`, { name }) }];
  if (facts.maxFileBytes) lines.push({ icon: "upload", text: t("upload.terms.maxSize", { size: formatBytes(facts.maxFileBytes) }) });
  lines.push({ icon: "timer", text: termsRetention(name, facts.retention) });
  lines.push({ icon: "trash", text: tk(`upload.terms.deletion.${description.deletion}`, { name }) });
  if (description.operator) lines.push({ icon: "info", text: tk(description.operator) });
  lines.push({ icon: "check", text: t("upload.consent.checked", { name, date: formatDay(facts.checkedOn) }) });
  return lines;
}

function termsRetention(name: string, retention: Retention): string {
  switch (retention.kind) {
    case "chosen":
      return t("upload.terms.retention.chosen", { name });
    case "optional":
      return t("upload.terms.retention.optional", { name });
    case "inactivity":
      return t("upload.terms.retention.inactivity", { name, period: formatPeriod(retention.days * 24) });
    case "bySize":
      return t("upload.terms.retention.bySize", { name, minDays: retention.minDays, maxDays: retention.maxDays });
    case "noExpiry":
      return t("upload.terms.retention.noExpiry", { name });
    case "ownStorage":
      return t("upload.terms.retention.ownStorage");
    case "notStated":
      return t("upload.terms.retention.notStated", { name });
  }
}

/** The title of a finished upload. */
export function doneText(done: UploadDone): string {
  if (done.copied === "markdown") return t("upload.state.copiedMarkdown");
  if (done.copied === "link") return t("upload.state.copiedLink");
  return t("upload.state.done");
}

/** The sentence of a failed upload; the host's own reason (`failed.error.message`) goes under it. */
export function failureText(failed: UploadFailed, name: string): string {
  if (failed.kind === "provider" && failed.status) {
    return t("upload.failed.providerStatus", { name, status: failed.status });
  }
  return tk(`upload.failed.${failed.kind}`, { name });
}
