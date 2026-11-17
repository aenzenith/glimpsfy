// Settings › Upload's view of the provider descriptions that `upload_providers` sends. The types
// are the shared mirror in src/lib/api/upload.ts and the host sentences come from
// src/upload/text.ts; this file only words a row of a form and runs the checks a field can run
// while the user types. `upload_validate` runs the same checks and more, and it is the one shown
// once the value is saved.

import type { ConsentInfo, FieldRule, ProviderDescription, SettingField } from "../lib/api/upload";
import { t } from "../lib/i18n";
import { tk } from "../upload/text";
import { tr } from "./text";

export type Provider = ProviderDescription;
export type Field = SettingField;
export type { Pair } from "../lib/api/upload";
export { pairSecretKey } from "../lib/api/upload";
export { optionLabel } from "../upload/text";

/** "accessKeyId" → "Access key id", for a label whose key is not in the catalogs yet. */
export function humanize(key: string): string {
  const last = key.split(".").pop() ?? key;
  const words = last.replace(/([a-z0-9])([A-Z])/g, "$1 $2").replace(/[_-]+/g, " ").toLowerCase();
  return words.charAt(0).toUpperCase() + words.slice(1);
}

export const labelOf = (field: Field) => tr(field.label, {}, humanize(field.key));
export const sectionOf = (key: string) => tr(key, {}, humanize(key));
export const helpOf = (field: Field) => (field.help ? tr(field.help, {}, "") : "");

/** A placeholder is either literal text ("eu-central-1") or a key of the catalogs. */
export function placeholderOf(text: string | null | undefined): string {
  if (!text) return "";
  return text.startsWith("upload.") ? tr(text, {}, "") : text;
}

/** The provider's own settings in `settings.upload.providers`. */
export function bagOf(providers: Record<string, unknown>, id: string): Record<string, unknown> {
  const bag = providers[id];
  return bag && typeof bag === "object" && !Array.isArray(bag) ? (bag as Record<string, unknown>) : {};
}

function ruleText(rule: FieldRule): string {
  switch (rule.rule) {
    case "maxLength":
      return tk("upload.rule.maxLength", { max: rule.max });
    case "tokens":
      return tk("upload.rule.tokens", { allowed: rule.allowed.map((a) => `{${a}}`).join(", ") });
    default:
      return tk(`upload.rule.${rule.rule}`);
  }
}

/** The first rule a typed value breaks, as a sentence, or `null`. */
export function ruleProblem(field: Field, value: string): string | null {
  if (field.required && !value.trim()) return tk("upload.issue.required");
  if (!value) return null;
  for (const rule of field.rules) {
    switch (rule.rule) {
      case "maxLength":
        if (value.length > rule.max) return ruleText(rule);
        break;
      case "httpUrl":
        if (!/^https?:\/\/[^\s/]+/i.test(value)) return ruleText(rule);
        break;
      case "noWhitespace":
        if (/\s/.test(value)) return ruleText(rule);
        break;
      case "tokens":
        if ([...value.matchAll(/\{([^{}]*)\}/g)].some((m) => !rule.allowed.includes(m[1] ?? ""))) return ruleText(rule);
        break;
    }
  }
  return null;
}

/** The meta line under a host in the list: the account it needs, and its domain when fixed. */
export function summaryOf(provider: Provider): string {
  const account = {
    none: "settings.upload.account.none",
    clientId: "settings.upload.account.clientId",
    apiKey: "settings.upload.account.apiKey",
    accessKeys: "settings.upload.account.accessKeys",
    custom: "settings.upload.account.custom",
  } as const;
  const parts = [t(account[provider.account] ?? account.custom)];
  if (provider.domain) parts.push(provider.domain);
  return parts.join(" · ");
}

/** Where a confirmation sends uploads: the domain, and the bucket for S3. */
export const destinationOf = (consent: ConsentInfo) => (consent.bucket ? `${consent.domain}/${consent.bucket}` : consent.domain);
