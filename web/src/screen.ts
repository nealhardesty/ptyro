// The daemon's screen model on the client (SPEC §6.5; crates/ptyro-core/src/cache.rs in
// TypeScript): apply keyframes and deltas, read the text back. Shared by the web app and its
// tests (web/test), which feed it live frames from a real daemon.

// ── protocol types (the subset this app uses; see crates/ptyro-protocol) ─────────────────────

export type Color = { t: "default" } | { t: "indexed"; i: number } | { t: "rgb"; r: number; g: number; b: number };
export interface Style { fg?: Color; bg?: Color; ul?: Color; flags?: number }
export interface Run { style: number; text: string; wide?: boolean; clusters?: number[] }
export interface RowUpdate { y: number; runs: Run[]; wrapped?: boolean }
export interface Cursor { x: number; y: number; visible: boolean; shape?: string }
export interface Size { cols: number; rows: number }
export interface Modes { mouse: boolean; alt_screen: boolean; bracketed_paste: boolean; focus_events: boolean; kitty_keyboard: boolean; app_cursor: boolean }
export interface Delta {
  size?: Size;
  modes?: Modes;
  styles?: { id: number; style: Style }[];
  rows?: RowUpdate[];
  cursor?: Cursor;
  title?: string;
}
export interface Frame { session: string; seq: number; keyframe: boolean; delta: Delta }
export interface StatusRecord { status: string; kind?: string; detail?: string; since_ms: number; source: string }
export interface Attention { kind: string; since_ms: number; text: string }
export interface Session {
  id: string;
  handle: string;
  name: string;
  kind: string;
  state: { t: string; code?: number };
  size: Size;
  agent?: string | null;
  status?: StatusRecord | null;
  attention?: Attention | null;
  activity?: string | null;
  project?: string | null;
  worktree?: string | null;
  workspace?: string;
  last_output_ms?: number | null;
  created_ms: number;
  title?: string | null;
  task?: string | null;
  last_reply?: string | null;
  held?: boolean;
  structured?: string | null;
  inner_agent?: string | null;
  quick?: boolean;
  profile?: string;
  run?: string | null;
  summary?: { text: string; turn: number; at_ms: number } | null;
  verify?: { check: string; [k: string]: any } | null;
  command?: { text?: string | null; exit?: number | null } | null;
  ports?: number[];
  turns?: number;
}
export interface InboxEntry { id: number; session: string; session_name: string; at_ms: number; kind: string; title: string; body: string; read_ms?: number | null }
export type Outcome = { t: "ok"; reply: any } | { t: "err"; error: { code: string; message: string } };

// ── a screen cache (crates/ptyro-core/src/cache.rs, in TypeScript) ───────────────────────────

export interface Cell { t: string; s: number; wide?: boolean; spacer?: boolean }

export function runCells(run: Run): string[] {
  const chars = Array.from(run.text);
  if (!run.clusters) return chars;
  const out: string[] = [];
  let i = 0;
  for (const n of run.clusters) { out.push(chars.slice(i, i + n).join("")); i += n; }
  return out;
}

/// A row's cells from its runs (a wide cell is followed by its spacer).
export function cellsOf(runs: Run[], max = Infinity): Cell[] {
  const cells: Cell[] = [];
  for (const run of runs) {
    for (const t of runCells(run)) {
      cells.push({ t, s: run.style, wide: run.wide });
      if (run.wide) cells.push({ t: "", s: run.style, spacer: true });
    }
  }
  return cells.length > max ? cells.slice(0, max) : cells;
}

export class Screen {
  size: Size = { cols: 80, rows: 24 };
  rows: Cell[][] = [];
  /// A row continues on the next one (a long line the terminal wrapped).
  wraps: boolean[] = [];
  /// What the program asked of the terminal: the mouse, the alternate screen, bracketed paste…
  modes: Modes = { mouse: false, alt_screen: false, bracketed_paste: false, focus_events: false, kitty_keyboard: false, app_cursor: false };
  styles = new Map<number, Style>();
  cursor: Cursor = { x: 0, y: 0, visible: true };
  seq = 0;
  title = "";

  reshape(size: Size): void {
    this.size = size;
    const rows: Cell[][] = [];
    for (let y = 0; y < size.rows; y++) rows.push(this.rows[y]?.slice(0, size.cols) ?? []);
    this.rows = rows;
  }

  apply(f: Frame): void {
    if (f.keyframe) { this.styles.clear(); this.rows = []; this.wraps = []; this.title = ""; }
    if (f.delta.size) this.reshape(f.delta.size);
    else if (f.keyframe) this.reshape(this.size);
    for (const d of f.delta.styles ?? []) this.styles.set(d.id, d.style);
    for (const r of f.delta.rows ?? []) {
      if (r.y >= this.rows.length) continue;
      this.rows[r.y] = cellsOf(r.runs, this.size.cols);
      this.wraps[r.y] = !!r.wrapped;
    }
    if (f.delta.modes) this.modes = f.delta.modes;
    if (f.delta.cursor) this.cursor = f.delta.cursor;
    if (f.delta.title !== undefined) this.title = f.delta.title;
    this.seq = f.seq;
  }

  text(): string {
    return this.rows.map((r) => r.map((c) => (c.spacer ? "" : c.t || " ")).join("").replace(/\s+$/, "")).join("\n").replace(/\n+$/, "");
  }
}

