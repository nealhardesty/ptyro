// SHELLS (SPEC §23.14, ROADMAP SH.4): a workspace's numbered terminals on the web. The session
// list has a SHELLS group (numbered cards, ＋ with a new shell, a scratch shell or a command);
// a shell's page has the strip (tmux's status line: tap a number, swipe on a phone); its ⋯ menu
// moves it to another number, watches it, or adopts it into a worktree. The web has no Alt it
// can count on, so the strip and the digits on the list are its keys.

import { startSession } from "./flow.js";
import type { Session } from "./screen.js";
import { actions, ask, enc, form, go, h, menu, ui } from "./kit.js";
import type { Item } from "./kit.js";

const ACTIONS = {
  "shells": "SHELLS: your numbered terminals that belong to the workspace, not a worktree (any command)",
  "shells.new": "A new shell in SHELLS (or a command: htop, ssh…), in your home directory",
  "shells.go": "Show shell N of SHELLS: tap its number (on the list, the digit keys 1–9)",
  "shells.scratch": "A scratch shell: a new, empty directory of its own that goes with it",
  "shells.move": "Give this shell another number in SHELLS (one already there swaps)",
  "shells.watch": "Tell me when this shell's command ends, it prints, goes quiet, or shows a line that matches",
  "shells.loose": "Move this session to its workspace's SHELLS: it outlives its worktree",
  "shells.adopt": "Move this shell from SHELLS into the worktree its directory is in",
} as const;
export const tip = actions(ACTIONS);

/// A workspace's shells by number (every workspace's with none named).
export function shellsOf(sessions: Iterable<Session>, workspace?: string): Session[] {
  return [...sessions]
    .filter((s) => s.loose && s.slot != null && (!workspace || (s.workspace ?? "main") === workspace))
    .sort((a, b) => (a.workspace ?? "").localeCompare(b.workspace ?? "") || (a.slot ?? 0) - (b.slot ?? 0));
}

const base = (p: string): string => p.split("/").filter(Boolean).pop() ?? p;

/// A shell's label (SPEC §23.4): its title, else what runs in it and where, else where.
export function shellLabel(s: Session): string {
  if (s.title && s.title !== s.window_title) return s.title;
  if (s.kind !== "shell") return s.name;
  const dir = s.live_cwd ?? s.cwd ?? "";
  const where = dir ? `…/${base(dir)}` : "";
  const shell = s.argv?.[0] ? base(s.argv[0]).replace(/^-/, "") : "";
  const fg = s.foreground?.replace(/^-/, "");
  if (fg && fg !== shell) return `${fg} · ${where}`;
  return where || s.name;
}

/// What its card says it's doing: the command running, else the last one and how it went.
function doing(s: Session): string {
  if (s.lost) return "lost in a restart: tap to start it again";
  if (s.attention) return s.attention.text;
  const c = s.command;
  if (c && c.ended_ms == null) return `● ${c.text ?? "running"}`;
  if (c?.text) return `${(c.exit ?? 0) === 0 ? "✓" : `✗${c.exit}`} ${c.text}`;
  return s.state.t === "running" ? "" : s.state.t;
}

const watchLabel = (w: NonNullable<Session["watch"]>): string =>
  w.t === "silence" ? `silence ${Math.round((w.ms ?? 0) / 1000)}s` : w.t === "match" ? `match ${w.re}` : w.t;

/// Start a shell in SHELLS: `slot` asks for a number, `argv` a command, `scratch` its own
/// directory. The daemon starts it in its home when no directory is given here.
export async function newLooseShell(opts: { workspace?: string; slot?: number; argv?: string[]; scratch?: boolean; cwd?: string } = {}): Promise<void> {
  await startSession({ cwd: opts.cwd ?? "~", argv: opts.argv ?? [], loose: true, slot: opts.slot ?? null, scratch: !!opts.scratch, workspace: opts.workspace ?? null });
}

/// Show shell N (one lost in a restart starts again); an empty number gets a new shell.
export async function goShell(sessions: Iterable<Session>, n: number, workspace?: string): Promise<void> {
  const s = shellsOf(sessions, workspace).find((x) => x.slot === n);
  if (!s) { ui.toast(`Shell ${n} started`); return newLooseShell({ workspace, slot: n }); }
  if (s.lost) await ask({ t: "restart", addr: s.id }, `Shell ${n} started again`);
  go(`#/s/${enc(s.id)}`);
}

/// The ＋ of SHELLS: a shell, a scratch shell, or a command.
export function newShellMenu(workspace?: string): void {
  menu("New in SHELLS", [
    { title: tip("shells.new"), label: "A shell", hint: "The next free number", run: () => newLooseShell({ workspace }) },
    { title: tip("shells.scratch"), label: "A scratch shell", hint: "A new, empty directory that goes with it", run: () => newLooseShell({ workspace, scratch: true }) },
    { title: tip("shells.new"), label: "A command…", hint: "htop, ssh prod, a dev server", run: () => commandForm(workspace) },
  ]);
}

function commandForm(workspace?: string): void {
  form("A command in SHELLS", [
    { key: "cmd", label: "Command", placeholder: "htop", required: true },
    { key: "slot", label: "Number (empty: the next free one)", type: "number" },
  ], "Start", async (v) => {
    const slot = v.slot ? Number(v.slot) : undefined;
    await startSession({ cwd: "~", argv: [], command: String(v.cmd), loose: true, slot: slot ?? null, workspace: workspace ?? null });
  });
}

/// The SHELLS group of the session list: numbered cards, or `＋ new shell`.
export function shellsGroup(sessions: Iterable<Session>, card: (s: Session) => HTMLElement): HTMLElement {
  const list = shellsOf(sessions);
  const workspaces = new Set(list.map((s) => s.workspace ?? "main"));
  const head = h("div", { class: "shells-head", title: tip("shells") },
    h("span", { class: "shells-title" }, "SHELLS"),
    h("button", { class: "icon plus", title: tip("shells.new"), "aria-label": "New shell", onclick: () => newShellMenu() }, "＋"));
  if (!list.length) {
    return h("section", { class: "shells" }, head,
      h("button", { class: "small shell-new", title: tip("shells.new"), onclick: () => void newLooseShell() }, "＋ new shell"));
  }
  const cards = list.map((s) => {
    const el = card(s);
    el.classList.add("shell-card");
    el.prepend(h("span", { class: "slot", title: tip("shells.go") }, String(s.slot)));
    const sub = h("span", { class: "shell-sub" }, shellLabel(s), workspaces.size > 1 ? ` · ${s.workspace ?? "main"}` : "", s.watch ? ` · ◎ ${watchLabel(s.watch)}` : "", s.scratch ? " · scratch" : "", doing(s) ? ` · ${doing(s)}` : "");
    el.append(sub);
    return el;
  });
  return h("section", { class: "shells" }, head, h("div", { class: "cards" }, ...cards));
}

/// The strip on a shell's page: its workspace's shells by number, the shown one marked.
export function shellStrip(sessions: Iterable<Session>, s: Session): HTMLElement {
  const ws = s.workspace ?? "main";
  const all = [...sessions];
  const list = shellsOf(all, ws);
  const nav = h("nav", { class: "shell-strip", title: tip("shells.go") },
    ...list.map((o) => h("button", {
      class: `${o.id === s.id ? "on" : ""}${o.attention ? " wants" : ""}${o.lost ? " lost" : ""}`,
      "aria-pressed": String(o.id === s.id),
      onclick: () => void goShell(all, o.slot ?? 0, ws),
    }, `${o.slot} `, h("small", {}, shellLabel(o)))),
    h("button", { class: "plus", title: tip("shells.new"), "aria-label": "New shell", onclick: () => newShellMenu(ws) }, "＋"));
  // A swipe along it goes to the next or the previous shell (a phone's way to flip through).
  let x0: number | null = null;
  nav.addEventListener("touchstart", (e) => { x0 = e.touches[0]?.clientX ?? null; }, { passive: true });
  nav.addEventListener("touchend", (e) => {
    const x1 = e.changedTouches[0]?.clientX;
    if (x0 == null || x1 == null || Math.abs(x1 - x0) < 60) return;
    const to = neighbour(all, s, x1 < x0 ? 1 : -1);
    if (to && to.id !== s.id) go(`#/s/${enc(to.id)}`);
    x0 = null;
  });
  return nav;
}

/// The neighbour of a shell in its workspace's SHELLS (`dir` = 1 next, -1 previous), for a swipe.
export function neighbour(sessions: Iterable<Session>, s: Session, dir: 1 | -1): Session | undefined {
  const list = shellsOf(sessions, s.workspace ?? "main");
  const i = list.findIndex((o) => o.id === s.id);
  return i < 0 ? undefined : list[(i + dir + list.length) % list.length];
}

/// What a session's ⋯ menu offers about SHELLS.
export function shellMenuItems(s: Session): Item[] {
  if (s.quick || s.pad || s.machine) return [];
  if (!s.loose) {
    return s.state.t === "running" ? [{ title: tip("shells.loose"), label: "Move to SHELLS", hint: "A number of its own; it outlives its worktree", run: () => ask({ t: "shell_place", addr: s.id, loose: true, worktree: null }, `${s.name} is in SHELLS now`) }] : [];
  }
  return [
    { title: tip("shells.move"), label: "Move to number…", hint: `It's shell ${s.slot} now`, run: () => moveForm(s) },
    { title: tip("shells.watch"), label: "Watch…", hint: s.watch ? `Now: ${watchLabel(s.watch)}` : "Its command ends, output, silence, a match", run: () => watchMenu(s) },
    { title: tip("shells.adopt"), label: "Adopt into its worktree", hint: "The one its directory is in", run: () => ask({ t: "shell_place", addr: s.id, loose: false, worktree: null }, `${s.name} joined its worktree`) },
  ];
}

function moveForm(s: Session): void {
  form(`Shell ${s.slot}: another number`, [{ key: "slot", label: "Number", type: "number", required: true }], "Move", async (v) => {
    const slot = Number(v.slot);
    if (!(slot >= 1)) throw new Error("a number from 1 up");
    await ask({ t: "shell_move", addr: s.id, slot }, `It's shell ${slot} now`);
  });
}

function watchMenu(s: Session): void {
  const set = (watch: object | null, said: string) => ask({ t: "shell_watch", addr: s.id, watch }, said);
  menu(`Watch shell ${s.slot}`, [
    { title: tip("shells.watch"), label: "When its command ends", hint: "However short; once", run: () => set({ t: "done" }, "Watching: done") },
    { title: tip("shells.watch"), label: "When it prints", hint: "New output while you aren't looking", run: () => set({ t: "output" }, "Watching: output") },
    { title: tip("shells.watch"), label: "When it goes quiet for 30s", run: () => set({ t: "silence", ms: 30_000 }, "Watching: silence 30s") },
    { title: tip("shells.watch"), label: "When it goes quiet for 5m", run: () => set({ t: "silence", ms: 300_000 }, "Watching: silence 5m") },
    { title: tip("shells.watch"), label: "When a line matches…", hint: "A pattern, like ERROR|listening", run: () => form("Watch for a line", [{ key: "re", label: "Pattern", placeholder: "ERROR|listening", required: true }], "Watch", async (v) => { await set({ t: "match", re: String(v.re) }, "Watching for a match"); }) },
    !!s.watch && { title: tip("shells.watch"), label: "Stop watching", run: () => set(null, "Not watched") },
  ]);
}
