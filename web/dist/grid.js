// The web terminal's pure parts (ROADMAP WEB1.8): a frozen document of scrollback plus screen,
// selections over it, words, and the text they copy. No DOM, so `web/test/term.mjs` can run them.
import { cellsOf } from "./screen.js";
export function docOf(d) {
    return {
        rows: d.rows.map((r) => cellsOf(r.runs, Math.max(d.size.cols, 1))),
        wraps: d.rows.map((r) => !!r.wrapped),
        styles: new Map(d.styles.map((s) => [s.id, s.style])),
        history: d.history,
        size: d.size,
        cursor: d.cursor,
    };
}
export const before = (p, q) => p.row < q.row || (p.row === q.row && p.col < q.col);
export function ordered(s) {
    return before(s.b, s.a) ? [s.b, s.a] : [s.a, s.b];
}
/// The columns of `row` the selection covers, as [from, to) (the cells that hold something
/// or are inside it), or null for none.
export function span(s, row, width) {
    const [p, q] = ordered(s);
    if (row < p.row || row > q.row)
        return null;
    if (s.block) {
        const [l, r] = [Math.min(s.a.col, s.b.col), Math.max(s.a.col, s.b.col)];
        return [l, r + 1];
    }
    const from = row === p.row ? p.col : 0;
    const to = row === q.row ? q.col + 1 : width;
    return from < to ? [from, to] : null;
}
const text = (cells, from, to) => {
    let out = "";
    for (let x = from; x < to && x < cells.length; x++) {
        const c = cells[x];
        if (!c.spacer)
            out += c.t || " ";
    }
    return out;
};
/// What the selection copies: a line each, but a wrapped line continues on the next without a
/// break, and trailing blanks go.
export function selText(rows, wraps, s, width = Infinity) {
    const [p, q] = ordered(s);
    const lines = [];
    let line = "";
    for (let y = Math.max(p.row, 0); y <= q.row && y < rows.length; y++) {
        const sp = span(s, y, Math.min(width, rows[y].length));
        let t = sp ? text(rows[y], sp[0], sp[1]) : "";
        const joined = !s.block && wraps[y] && y < q.row;
        if (!joined)
            t = t.replace(/\s+$/, "");
        line += t;
        if (!joined) {
            lines.push(line);
            line = "";
        }
    }
    if (line)
        lines.push(line);
    return lines.join("\n").replace(/\n+$/, "");
}
const WORD = /[\p{L}\p{N}_\-./~:@%+=#?&]/u;
const isWord = (c) => !!c && !c.spacer && WORD.test(c.t || " ");
/// The word under a cell as [from, to] inclusive: letters, digits and what a path or a URL is
/// made of; or the run of blanks or punctuation it is in.
export function wordAt(row, col) {
    // A wide character's spacer belongs to the cell before it.
    if (row[col]?.spacer && col > 0)
        col--;
    const kind = (c) => (isWord(c) ? 1 : !c || !c.t || c.t === " " ? 0 : 2);
    const k = kind(row[col]);
    let a = col, b = col;
    while (a > 0 && kind(row[a - 1]) === k)
        a--;
    while (b + 1 < row.length && kind(row[b + 1]) === k)
        b++;
    // A wide character's spacer is part of it.
    if (row[b]?.wide && row[b + 1]?.spacer)
        b++;
    return [a, b];
}
/// The last cell of a row that holds something, for "select the line".
export function lastUsed(row) {
    let x = row.length - 1;
    while (x > 0 && (!row[x].t || row[x].t === " ") && !row[x].spacer)
        x--;
    return Math.max(x, 0);
}
/// The window onto a document: its rows from `top`, `height` of them (short documents are
/// padded with empty rows).
export function windowOf(rows, top, height) {
    const out = [];
    for (let y = 0; y < height; y++)
        out.push(rows[top + y] ?? []);
    return out;
}
/// How far the document can scroll back from the live screen.
export const maxOffset = (docRows, height) => Math.max(0, docRows - height);
/// The cell under a point, from the canvas's own top-left, given the cell size.
export function cellAt(x, y, cw, ch, cols, rows) {
    return {
        col: Math.min(Math.max(Math.floor(x / cw), 0), cols - 1),
        row: Math.min(Math.max(Math.floor(y / ch), 0), rows - 1),
    };
}
/// How many columns and rows of cells fit in a box.
export function fitCells(w, h, cw, ch) {
    return { cols: Math.max(20, Math.floor(w / cw)), rows: Math.max(5, Math.floor(h / ch)) };
}
/// A row as text, and where each character of it sits (a wide character's spacer has none).
function flat(row) {
    let text = "";
    const at = [];
    row.forEach((c, x) => {
        if (c.spacer)
            return;
        const t = c.t || " ";
        for (let i = 0; i < t.length; i++)
            at.push(x);
        text += t;
    });
    return { text, at };
}
const URL_RE = /https?:\/\/[^\s<>"'`]+/g;
/// The link under a cell, as [from, to] cells inclusive and its address: what a click opens.
export function urlAt(row, col) {
    const { text, at } = flat(row);
    for (const m of text.matchAll(URL_RE)) {
        // A sentence's own full stop, comma or closing bracket isn't the link's.
        const url = m[0].replace(/[.,;:!?)\]}]+$/, "");
        const a = m.index ?? 0, b = a + url.length - 1;
        if (at[a] <= col && col <= (at[b] ?? at[at.length - 1]))
            return { from: at[a], to: at[b], url };
    }
    return null;
}
/// The next place `query` is in the document from `from` (not including it), going down or up,
/// wrapping round once; the match as a selection. Case-insensitive.
export function findIn(rows, query, from, dir) {
    if (!query || !rows.length)
        return null;
    const q = query.toLowerCase();
    const n = rows.length;
    for (let i = 0; i <= n; i++) {
        const y = ((from.row + dir * i) % n + n) % n;
        const { text, at } = flat(rows[y]);
        const low = text.toLowerCase();
        const hits = [];
        for (let k = low.indexOf(q); k >= 0; k = low.indexOf(q, k + 1))
            hits.push(k);
        const ok = (k) => i > 0 || (dir === 1 ? at[k] > from.col : at[k] < from.col);
        const k = dir === 1 ? hits.find(ok) : [...hits].reverse().find(ok);
        if (k !== undefined)
            return { a: { row: y, col: at[k] }, b: { row: y, col: at[k + q.length - 1] ?? at[k] } };
    }
    return null;
}
