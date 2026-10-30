// The web terminal's pure parts (ROADMAP WEB1.8): a frozen document of scrollback plus screen,
// selections over it, words, and the text they copy. No DOM, so `web/test/term.mjs` can run them.

import { cellsOf } from "./screen.js";
import type { Cell, Cursor, RowUpdate, Size, Style } from "./screen.js";

/// A frozen copy of history plus the screen (`read_scrollback`), top to bottom.
export interface Doc {
  rows: Cell[][];
  wraps: boolean[];
  styles: Map<number, Style>;
  /// How many of `rows` are history; the rest is the screen at the time.
  history: number;
  size: Size;
  cursor: Cursor;
}

export function docOf(d: { size: Size; history: number; rows: RowUpdate[]; styles: { id: number; style: Style }[]; cursor: Cursor }): Doc {
  return {
    rows: d.rows.map((r) => cellsOf(r.runs, Math.max(d.size.cols, 1))),
    wraps: d.rows.map((r) => !!r.wrapped),
    styles: new Map(d.styles.map((s) => [s.id, s.style])),
    history: d.history,
    size: d.size,
    cursor: d.cursor,
  };
}

/// A position in a document: its row, and the cell in it.
export interface Pos { row: number; col: number }
/// A selection from `a` to `b` (either order), by lines, or as a block of columns.
export interface Sel { a: Pos; b: Pos; block?: boolean }

export const before = (p: Pos, q: Pos): boolean => p.row < q.row || (p.row === q.row && p.col < q.col);

export function ordered(s: Sel): [Pos, Pos] {
  return before(s.b, s.a) ? [s.b, s.a] : [s.a, s.b];
}

/// The columns of `row` the selection covers, as [from, to) (the cells that hold something
/// or are inside it), or null for none.
export function span(s: Sel, row: number, width: number): [number, number] | null {
  const [p, q] = ordered(s);
  if (row < p.row || row > q.row) return null;
  if (s.block) {
    const [l, r] = [Math.min(s.a.col, s.b.col), Math.max(s.a.col, s.b.col)];
    return [l, r + 1];
  }
  const from = row === p.row ? p.col : 0;
  const to = row === q.row ? q.col + 1 : width;
  return from < to ? [from, to] : null;
}

const text = (cells: Cell[], from: number, to: number): string => {
  let out = "";
  for (let x = from; x < to && x < cells.length; x++) { const c = cells[x]; if (!c.spacer) out += c.t || " "; }
  return out;
};

/// What the selection copies: a line each, but a wrapped line continues on the next without a
/// break, and trailing blanks go.
export function selText(rows: Cell[][], wraps: boolean[], s: Sel, width = Infinity): string {
  const [p, q] = ordered(s);
  const lines: string[] = [];
  let line = "";
  for (let y = Math.max(p.row, 0); y <= q.row && y < rows.length; y++) {
    const sp = span(s, y, Math.min(width, rows[y].length));
    let t = sp ? text(rows[y], sp[0], sp[1]) : "";
    const joined = !s.block && wraps[y] && y < q.row;
    if (!joined) t = t.replace(/\s+$/, "");
    line += t;
    if (!joined) { lines.push(line); line = ""; }
  }
  if (line) lines.push(line);
  return lines.join("\n").replace(/\n+$/, "");
}

const WORD = /[\p{L}\p{N}_\-./~:@%+=#?&]/u;
const isWord = (c: Cell | undefined): boolean => !!c && !c.spacer && WORD.test(c.t || " ");

/// The word under a cell as [from, to] inclusive: letters, digits and what a path or a URL is
/// made of; or the run of blanks or punctuation it is in.
export function wordAt(row: Cell[], col: number): [number, number] {
  // A wide character's spacer belongs to the cell before it.
  if (row[col]?.spacer && col > 0) col--;
  const kind = (c: Cell | undefined) => (isWord(c) ? 1 : !c || !c.t || c.t === " " ? 0 : 2);
  const k = kind(row[col]);
  let a = col, b = col;
  while (a > 0 && kind(row[a - 1]) === k) a--;
  while (b + 1 < row.length && kind(row[b + 1]) === k) b++;
  // A wide character's spacer is part of it.
  if (row[b]?.wide && row[b + 1]?.spacer) b++;
  return [a, b];
}

/// The last cell of a row that holds something, for "select the line".
export function lastUsed(row: Cell[]): number {
  let x = row.length - 1;
  while (x > 0 && (!row[x].t || row[x].t === " ") && !row[x].spacer) x--;
  return Math.max(x, 0);
}

/// The window onto a document: its rows from `top`, `height` of them (short documents are
/// padded with empty rows).
export function windowOf(rows: Cell[][], top: number, height: number): Cell[][] {
  const out: Cell[][] = [];
  for (let y = 0; y < height; y++) out.push(rows[top + y] ?? []);
  return out;
}

/// How far the document can scroll back from the live screen.
export const maxOffset = (docRows: number, height: number): number => Math.max(0, docRows - height);

/// The cell under a point, from the canvas's own top-left, given the cell size.
export function cellAt(x: number, y: number, cw: number, ch: number, cols: number, rows: number): { col: number; row: number } {
  return {
    col: Math.min(Math.max(Math.floor(x / cw), 0), cols - 1),
    row: Math.min(Math.max(Math.floor(y / ch), 0), rows - 1),
  };
}

/// How many columns and rows of cells fit in a box.
export function fitCells(w: number, h: number, cw: number, ch: number): Size {
  return { cols: Math.max(20, Math.floor(w / cw)), rows: Math.max(5, Math.floor(h / ch)) };
}

// ── zoom ─────────────────────────────────────────────────────────────────────────────────

/// How the terminal sizes its text: fitted to the width, a size of its own (1:1, scrolling
/// sideways), or automatic (fitted while that is readable, otherwise 1:1).
export type Zoom = "auto" | "fit" | "size";
/// The text sizes a pinch, the wheel or the menu can choose.
export const MIN_PX = 6, MAX_PX = 32;
/// Fitted text smaller than this is too small to read on a phone: automatic shows it 1:1.
export const READABLE_PX = 9;

/// Fitted text is no bigger than this: a narrow session fitted leaves room at the side.
export const FIT_MAX_PX = 16;

/// The fitted text size: the session's width across the box, at most 16 px, at least 5.
export function fitPx(boxWidth: number, cols: number, ratio: number): number {
  return Math.max(5, Math.min(FIT_MAX_PX, Math.max(boxWidth, 40) / (Math.max(cols, 1) * ratio)));
}

/// Whether the screen is drawn fitted to the width.
export function fitted(zoom: Zoom, fit: number): boolean {
  return zoom === "fit" || (zoom === "auto" && fit >= READABLE_PX);
}

export const clampPx = (px: number): number => Math.min(MAX_PX, Math.max(MIN_PX, px));

/// What a pinch asks for: the size it started at, scaled by how far apart the fingers are now;
/// at or below the fitted size, fitted (pinching in as far as it goes shows the whole screen).
/// A session narrow enough to fit at the biggest fitted size can still be made smaller.
export function pinchZoom(px0: number, d0: number, d: number, fit: number): { zoom: "fit" | "size"; px: number } {
  // Compared before the floor: a wide session fits below the smallest size of its own.
  const px = px0 * (d0 > 0 ? d / d0 : 1);
  return px <= fit && fit < FIT_MAX_PX ? { zoom: "fit", px: fit } : { zoom: "size", px: clampPx(px) };
}

/// A vertical drag of `dy` pixels (down is positive) on a screen that can be taller than its
/// box (`top` is how far the box is scrolled, `maxTop` how far it can be): the box scrolls first,
/// and what is left over goes on to the history (pixels, the same sign). Coming back from the
/// history (`back`), the history comes first, so the box stays where the person left it.
export function splitPan(dy: number, top: number, maxTop: number, back: boolean): { top: number; rest: number } {
  if (dy > 0) {
    const next = Math.max(0, top - dy);
    return { top: next, rest: dy - (top - next) };
  }
  if (back) return { top, rest: dy };
  const next = Math.max(top, Math.min(Math.max(maxTop, 0), top - dy));
  return { top: next, rest: dy + (next - top) };
}

/// A row as text, and where each character of it sits (a wide character's spacer has none).
function flat(row: Cell[]): { text: string; at: number[] } {
  let text = "";
  const at: number[] = [];
  row.forEach((c, x) => {
    if (c.spacer) return;
    const t = c.t || " ";
    for (let i = 0; i < t.length; i++) at.push(x);
    text += t;
  });
  return { text, at };
}

const URL_RE = /https?:\/\/[^\s<>"'`]+/g;

/// The link under a cell, as [from, to] cells inclusive and its address: what a click opens.
export function urlAt(row: Cell[], col: number): { from: number; to: number; url: string } | null {
  const { text, at } = flat(row);
  for (const m of text.matchAll(URL_RE)) {
    // A sentence's own full stop, comma or closing bracket isn't the link's.
    const url = m[0].replace(/[.,;:!?)\]}]+$/, "");
    const a = m.index ?? 0, b = a + url.length - 1;
    if (at[a] <= col && col <= (at[b] ?? at[at.length - 1])) return { from: at[a], to: at[b], url };
  }
  return null;
}

/// The next place `query` is in the document from `from` (not including it), going down or up,
/// wrapping round once; the match as a selection. Case-insensitive.
export function findIn(rows: Cell[][], query: string, from: Pos, dir: 1 | -1): Sel | null {
  if (!query || !rows.length) return null;
  const q = query.toLowerCase();
  const n = rows.length;
  for (let i = 0; i <= n; i++) {
    const y = ((from.row + dir * i) % n + n) % n;
    const { text, at } = flat(rows[y]);
    const low = text.toLowerCase();
    const hits: number[] = [];
    for (let k = low.indexOf(q); k >= 0; k = low.indexOf(q, k + 1)) hits.push(k);
    const ok = (k: number) => i > 0 || (dir === 1 ? at[k] > from.col : at[k] < from.col);
    const k = dir === 1 ? hits.find(ok) : [...hits].reverse().find(ok);
    if (k !== undefined) return { a: { row: y, col: at[k] }, b: { row: y, col: at[k + q.length - 1] ?? at[k] } };
  }
  return null;
}
