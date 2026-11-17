// The page's copy of the history's search folding (crates/history/src/fold.rs), used only to
// mark the matches in names and in the recognised text the inspector shows. The store decides
// what matches; this decides which letters are drawn in iris. Both fold the same way: case,
// accents and the Turkish i's (I, İ, ı and i are one letter), so "ışık" marks "IŞIK" and "isik".

const MARK = /\p{M}/u;
const TOKEN = /[\p{L}\p{N}]/u;

function foldChar(c: string): string {
  if (c.length === 1 && c.charCodeAt(0) < 128) return c.toLowerCase();
  // `ı` has no decomposition and lower-casing leaves it alone.
  if (c === "ı") return "i";
  let out = "";
  for (const d of c.normalize("NFKD")) {
    if (MARK.test(d)) continue;
    if (d === "ı") {
      out += "i";
      continue;
    }
    for (const l of d.toLowerCase()) if (!MARK.test(l)) out += l;
  }
  return out;
}

/** The folded text, and for each of its UTF-16 units the source range it came from. */
function foldMapped(text: string): { folded: string; from: number[]; to: number[] } {
  let folded = "";
  const from: number[] = [];
  const to: number[] = [];
  let index = 0;
  for (const c of text) {
    const piece = foldChar(c);
    for (let i = 0; i < piece.length; i++) {
      from.push(index);
      to.push(index + c.length);
    }
    folded += piece;
    index += c.length;
  }
  return { folded, from, to };
}

export function fold(text: string): string {
  return foldMapped(text).folded;
}

/** The query's tokens, folded: words split on spaces and on anything that is not a letter or a
 * digit, as the store's tokenizer does. */
export function queryTokens(query: string): string[] {
  return fold(query)
    .split(/[^\p{L}\p{N}]+/u)
    .filter((t) => t.length > 0);
}

export type Range = { start: number; end: number };

/** The ranges of `text` (UTF-16 units) where a query token starts a word, merged and in order. */
export function matchRanges(text: string, query: string): Range[] {
  const tokens = queryTokens(query);
  if (!tokens.length || !text) return [];
  const { folded, from, to } = foldMapped(text);
  const found: Range[] = [];
  for (const token of tokens) {
    let at = folded.indexOf(token);
    while (at >= 0) {
      const before = at > 0 ? folded[at - 1] : "";
      if (!before || !TOKEN.test(before)) {
        found.push({ start: from[at], end: to[at + token.length - 1] });
      }
      at = folded.indexOf(token, at + 1);
    }
  }
  found.sort((a, b) => a.start - b.start || b.end - a.end);
  const merged: Range[] = [];
  for (const r of found) {
    const last = merged[merged.length - 1];
    if (last && r.start <= last.end) last.end = Math.max(last.end, r.end);
    else merged.push({ ...r });
  }
  return merged;
}

/** Splits text into plain and marked parts, for a template to render without `v-html`. */
export function parts(text: string, ranges: readonly Range[]): { text: string; hit: boolean }[] {
  const out: { text: string; hit: boolean }[] = [];
  let at = 0;
  for (const r of ranges) {
    const start = Math.max(at, Math.min(r.start, text.length));
    const end = Math.min(text.length, r.end);
    if (start > at) out.push({ text: text.slice(at, start), hit: false });
    if (end > start) out.push({ text: text.slice(start, end), hit: true });
    at = Math.max(at, end);
  }
  if (at < text.length) out.push({ text: text.slice(at), hit: false });
  return out;
}
