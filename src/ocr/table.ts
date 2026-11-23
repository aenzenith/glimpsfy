// The tables of a recognition and their text for the clipboard: the rows `layout.rs` marks as a
// table, cut into cells, lined up in columns and written out as TSV or as a Markdown table. The
// text layer's table chip (`TextLayer.vue`) copies them; they touch no DOM and no bridge.

import type { Bounds, OcrLine, OcrResult, OcrWord } from "../lib/api/ocr";

/** A table: its cells row by row, every row as wide as the table (an empty string where a cell
 * holds no text), its box in pixels of the recognised image, and how many of its cells hold text. */
export type OcrTable = { rows: string[][]; bounds: Bounds; cells: number };

/** A horizontal stretch in pixels of the recognised image. */
type Span = { left: number; right: number };

/** A cell as `layout.rs` wrote it, with the words it was cut from; `span` is null when the line's
 * words could not be cut into its cells. */
type Cell = { text: string; words: OcrWord[]; span: Span | null };

/** A gap between two words that is this many times the line's word height is no plain space, so a
 * cell may be cut there when the gap falls between two columns. */
const WIDE_GAP = 0.4;

/** The tables of `result` in reading order, each with at least two rows and two columns. A table is
 * a run of lines of one block that `layout.rs` marked as table rows. */
export function tablesOf(result: OcrResult): OcrTable[] {
  const tables: OcrTable[] = [];
  let group: OcrLine[] = [];
  const close = () => {
    const table = group.length ? tableOf(group) : null;
    if (table) tables.push(table);
    group = [];
  };
  for (const line of result.lines) {
    if (line.kind !== "table" || (group.length && group[0].block !== line.block)) close();
    if (line.kind === "table") group.push(line);
  }
  close();
  return tables;
}

/** The table as tab-separated values: one line per row, every row with the same number of fields.
 * Nothing is quoted, and a cell that starts with `=` or `-` stays as it is, because a prefix would
 * spoil numbers such as "-12" and the text comes from the user's own screen. */
export function toTsv(table: OcrTable): string {
  const width = widthOf(table);
  return table.rows.map((row) => fields(row, width, flat).join("\t")).join("\n");
}

/** The table as a GitHub Markdown table, its first row as the header. */
export function toMarkdown(table: OcrTable): string {
  const width = widthOf(table);
  const escape = (text: string) => flat(text.replace(/\\/g, "\\\\").replace(/\|/g, "\\|"));
  const line = (cells: string[]) => `| ${cells.join(" | ")} |`;
  const [head = [], ...body] = table.rows;
  return [line(fields(head, width, escape)), line(Array(width).fill("---")), ...body.map((row) => line(fields(row, width, escape)))].join(
    "\n",
  );
}

// ------------------------------------------------------------------------------------- cells

function tableOf(lines: OcrLine[]): OcrTable | null {
  const cut = lines.map(cellsOf);
  const most = Math.max(...cut.map((cells) => cells.length));
  const bands = bandsOf(cut.filter((cells) => cells.length === most));
  // A heading over the table (or a note under it) in the same type as its rows lands in the
  // table's block; it is one piece of text across two columns or more, which no row is.
  let from = 0;
  let to = lines.length;
  if (bands) {
    while (from < to && caption(cut[from], bands)) from++;
    while (to > from && caption(cut[to - 1], bands)) to--;
  }
  const rows = cut.slice(from, to).map((cells, i) => (bands ? inBands(cells, bands, wordHeight(lines[from + i])) : inOrder(cells, most)));
  const columns = rows[0]?.length ?? 0;
  if (rows.length < 2 || columns < 2) return null;
  const bounds = lines.slice(from + 1, to).reduce((box, line) => union(box, line.bounds), lines[from].bounds);
  return { rows, bounds, cells: rows.flat().filter((cell) => cell).length };
}

/** The cells of a line, `line.text` split at its tabs. Each cell's stretch comes from the line's
 * words, cut at the n − 1 widest gaps between them (n cells), since `layout.rs` put a tab only
 * where a gap was wider than the text is high. */
function cellsOf(line: OcrLine): Cell[] {
  const texts = line.text.split("\t");
  const words = line.words.filter((word) => word.text.trim());
  if (words.length < texts.length) return texts.map((text) => ({ text, words: [], span: null }));
  const cuts = words
    .slice(1)
    .map((word, i) => ({ at: i + 1, width: word.bounds.x - right(words[i].bounds) }))
    .sort((a, b) => b.width - a.width)
    .slice(0, texts.length - 1)
    .map((gap) => gap.at)
    .sort((a, b) => a - b);
  const edges = [0, ...cuts, words.length];
  return texts.map((text, k) => {
    const own = words.slice(edges[k], edges[k + 1]);
    return { text, words: own, span: spanOf(own) };
  });
}

/** The columns: the k-th band covers the k-th cells of the rows with the most cells. `null` when
 * none of those rows could be measured. */
function bandsOf(rows: Cell[][]): Span[] | null {
  const measured = rows.filter((cells) => cells.every((cell) => cell.span));
  if (!measured.length) return null;
  return measured[0].map((_, k) => measured.slice(1).reduce((band, cells) => join(band, cells[k].span!), measured[0][k].span!));
}

/** Whether a row is a single piece of text that reaches into two columns or more. */
function caption(cells: Cell[], bands: Span[]): boolean {
  const span = cells.length === 1 ? cells[0].span : null;
  return bands.length > 1 && span !== null && bands.filter((band) => overlap(span, band) > 0).length > 1;
}

/** A row in the table's columns: each cell goes to the band it overlaps most, or to the nearest
 * band when it overlaps none. A band no cell reaches stays empty, so a row with an empty cell
 * keeps its other cells in their columns, and two cells in one band are joined by a space. */
function inBands(cells: Cell[], bands: Span[], height: number): string[] {
  if (cells.some((cell) => !cell.span)) return inOrder(cells, bands.length);
  const out: string[][] = bands.map(() => []);
  for (const cell of cells) {
    for (const part of partsOf(cell, bands, height)) if (part.text) out[bandOf(part.span, bands)].push(part.text);
  }
  return out.map((texts) => texts.join(" "));
}

/** A row whose words could not be measured: its cells in order, padded to `width`. */
function inOrder(cells: Cell[], width: number): string[] {
  return Array.from({ length: width }, (_, k) => cells[k]?.text.trim() ?? "");
}

/** A cell cut where a wide gap between its words falls between two columns. The engine joins two
 * neighbouring cells whose gap is narrower than the text is high ("Q2 Change" under the headers
 * Q2 and Change), and the columns of the other rows show where they part. */
function partsOf(cell: Cell, bands: Span[], height: number): { text: string; span: Span }[] {
  const text = cell.text.trim();
  // Only a cell whose words spell its text can be cut by its words.
  if (cell.words.length < 2 || cell.words.map((word) => word.text).join(" ") !== text) return [{ text, span: cell.span! }];
  const parts: OcrWord[][] = [[cell.words[0]]];
  for (let i = 1; i < cell.words.length; i++) {
    const end = right(cell.words[i - 1].bounds);
    const gap = cell.words[i].bounds.x - end;
    const middle = end + gap / 2;
    const between = bands.some((band, k) => k > 0 && bands[k - 1].right < middle && middle < band.left);
    if (gap > WIDE_GAP * height && between) parts.push([cell.words[i]]);
    else parts[parts.length - 1].push(cell.words[i]);
  }
  return parts.map((words) => ({ text: words.map((word) => word.text).join(" "), span: spanOf(words)! }));
}

/** The band a stretch overlaps most, or the one whose centre is nearest. */
function bandOf(span: Span, bands: Span[]): number {
  let best = -1;
  let most = 0;
  bands.forEach((band, k) => {
    const shared = overlap(span, band);
    if (shared > most) [best, most] = [k, shared];
  });
  if (best >= 0) return best;
  const centre = (s: Span) => (s.left + s.right) / 2;
  const away = (k: number) => Math.abs(centre(bands[k]) - centre(span));
  return bands.reduce((near, _, k) => (away(k) < away(near) ? k : near), 0);
}

// ---------------------------------------------------------------------------------- geometry

const right = (b: Bounds) => b.x + b.width;

function spanOf(words: OcrWord[]): Span | null {
  if (!words.length) return null;
  return { left: Math.min(...words.map((word) => word.bounds.x)), right: Math.max(...words.map((word) => right(word.bounds))) };
}

const join = (a: Span, b: Span): Span => ({ left: Math.min(a.left, b.left), right: Math.max(a.right, b.right) });
const overlap = (a: Span, b: Span) => Math.min(a.right, b.right) - Math.max(a.left, b.left);

function union(a: Bounds, b: Bounds): Bounds {
  const x = Math.min(a.x, b.x);
  const y = Math.min(a.y, b.y);
  return { x, y, width: Math.max(right(a), right(b)) - x, height: Math.max(a.y + a.height, b.y + b.height) - y };
}

/** The height of a line's words, which a plain space is measured against; the line's box when it
 * has no words. */
function wordHeight(line: OcrLine): number {
  const heights = line.words.map((word) => word.bounds.height).sort((a, b) => a - b);
  return heights.length ? heights[heights.length >> 1] : line.bounds.height;
}

// -------------------------------------------------------------------------------------- text

/** A cell on one line: tabs and line breaks become spaces, and the edges are trimmed. */
const flat = (text: string) => text.replace(/[\t\r\n]/g, " ").trim();

const widthOf = (table: OcrTable) => Math.max(0, ...table.rows.map((row) => row.length));

const fields = (row: string[], width: number, write: (text: string) => string) =>
  Array.from({ length: width }, (_, k) => write(row[k] ?? ""));
