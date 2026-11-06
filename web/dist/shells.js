// SHELLS (SPEC §23.14, ROADMAP SH.4): a workspace's numbered terminals on the web. The session
// list has a SHELLS group (numbered cards, ＋ with a new shell, a scratch shell or a command);
// a shell's page has the strip (tmux's status line: tap a number, swipe on a phone); its ⋯ menu
// moves it to another number, watches it, or adopts it into a worktree. The web has no Alt it
// can count on, so the strip and the digits on the list are its keys.
import { startSession } from "./flow.js";
import { shellLabel } from "./shellkit.js";
import { actions, ask, enc, form, go, h, menu, ui } from "./kit.js";
const ACTIONS = {
    "shells": "SHELLS: your numbered terminals that belong to the workspace, not a worktree (any command)",
    "shells.new": "A new shell in SHELLS (or a command: htop, ssh…), in your home directory",
    "shells.go": "Show shell N of SHELLS: tap its number (on the list, the digit keys 1–9)",
    "shells.scratch": "A scratch shell: a new, empty directory of its own that goes with it",
    "shells.move": "Give this shell another number in SHELLS (one already there swaps)",
    "shells.watch": "Tell me when this shell's command ends, it prints, goes quiet, or shows a line that matches",
    "shells.loose": "Move this session to its workspace's SHELLS: it outlives its worktree",
    "shells.adopt": "Move this shell from SHELLS into the worktree its directory is in",
    "shells.split": "Split this shell's window with a copy of it: a shell in the same directory, a pane of the same number",
    "shells.pane": "Show another pane of this shell's window: tap its .number",
    "shells.break": "Move this pane out of its window to a number of its own",
    "shells.join": "Move this shell into another window as a pane",
};
export const tip = actions(ACTIONS);
/// The workspace the list's digit keys act on: the one whose shell was opened last, else
/// `main`. Digit N is always shell N of one workspace, never of whichever sorts first.
let lastWorkspace;
export const listWorkspace = () => lastWorkspace ?? "main";
/// A workspace's shells by number (every workspace's with none named).
export function shellsOf(sessions, workspace) {
    return [...sessions]
        .filter((s) => s.loose && s.slot != null && (!workspace || (s.workspace ?? "main") === workspace))
        .sort((a, b) => (a.workspace ?? "").localeCompare(b.workspace ?? "") || (a.slot ?? 0) - (b.slot ?? 0) || (a.pane ?? 1) - (b.pane ?? 1));
}
/// A workspace's windows: the first pane of each number (SPEC §23.17).
export function windowsOf(sessions, workspace) {
    const seen = new Set();
    return shellsOf(sessions, workspace).filter((s) => {
        const k = `${s.workspace ?? "main"}%${s.slot}`;
        return seen.has(k) ? false : (seen.add(k), true);
    });
}
/// A window's panes, in pane order.
const panesOf = (sessions, s) => shellsOf(sessions, s.workspace ?? "main").filter((o) => o.slot === s.slot);
/// `3`, or `3.2` for a pane of a window of several.
const paneName = (s) => (s.layout && s.pane ? `${s.slot}.${s.pane}` : String(s.slot));
/// What its card says it's doing: the command running, else the last one and how it went.
function doing(s) {
    if (s.lost)
        return "lost in a restart: tap to start it again";
    if (s.attention)
        return s.attention.text;
    const c = s.command;
    if (c && c.ended_ms == null)
        return `● ${c.text ?? "running"}`;
    if (c?.text)
        return `${(c.exit ?? 0) === 0 ? "✓" : `✗${c.exit}`} ${c.text}`;
    return s.state.t === "running" ? "" : s.state.t;
}
const watchLabel = (w) => w.t === "silence" ? `silence ${Math.round((w.ms ?? 0) / 1000)}s` : w.t === "match" ? `match ${w.re}` : w.t;
/// Start a shell in SHELLS: `slot` asks for a number, `argv` a command, `scratch` its own
/// directory. The daemon starts it in its home when no directory is given here.
export async function newLooseShell(opts = {}) {
    await startSession({ cwd: opts.cwd ?? "~", argv: opts.argv ?? [], loose: true, slot: opts.slot ?? null, scratch: !!opts.scratch, workspace: opts.workspace ?? null });
}
/// Show shell N (one lost in a restart starts again); an empty number gets a new shell.
export async function goShell(sessions, n, workspace) {
    // A window opens on the pane that had focus there.
    const panes = shellsOf(sessions, workspace).filter((x) => x.slot === n);
    const focus = panes[0]?.layout?.focus;
    const s = panes.find((x) => focus != null && x.pane === focus) ?? panes[0];
    if (!s) {
        ui.toast(`Shell ${n} started`);
        return newLooseShell({ workspace, slot: n });
    }
    if (s.lost)
        await ask({ t: "restart", addr: s.id }, `Shell ${n} started again`);
    go(`#/s/${enc(s.id)}`);
}
/// The ＋ of SHELLS: a shell, a scratch shell, or a command.
export function newShellMenu(workspace) {
    menu("New in SHELLS", [
        { title: tip("shells.new"), label: "A shell", hint: "The next free number", run: () => newLooseShell({ workspace }) },
        { title: tip("shells.scratch"), label: "A scratch shell", hint: "A new, empty directory that goes with it", run: () => newLooseShell({ workspace, scratch: true }) },
        { title: tip("shells.new"), label: "A command…", hint: "htop, ssh prod, a dev server", run: () => commandForm(workspace) },
    ]);
}
function commandForm(workspace) {
    form("A command in SHELLS", [
        { key: "cmd", label: "Command", placeholder: "htop", required: true },
        { key: "slot", label: "Number (empty: the next free one)", type: "number" },
    ], "Start", async (v) => {
        const slot = v.slot ? Number(v.slot) : undefined;
        await startSession({ cwd: "~", argv: [], command: String(v.cmd), loose: true, slot: slot ?? null, workspace: workspace ?? null });
    });
}
/// The SHELLS group of the session list: numbered cards, or `＋ new shell`.
export function shellsGroup(sessions, card) {
    const list = shellsOf(sessions);
    const workspaces = new Set(list.map((s) => s.workspace ?? "main"));
    if (lastWorkspace && !workspaces.has(lastWorkspace))
        lastWorkspace = undefined;
    if (!lastWorkspace && workspaces.size === 1)
        lastWorkspace = [...workspaces][0];
    const head = h("div", { class: "shells-head", title: tip("shells") }, h("span", { class: "shells-title" }, "SHELLS"), h("button", { class: "icon plus", title: tip("shells.new"), "aria-label": "New shell", onclick: () => newShellMenu() }, "＋"));
    if (!list.length) {
        return h("section", { class: "shells" }, head, h("button", { class: "small shell-new", title: tip("shells.new"), onclick: () => void newLooseShell() }, "＋ new shell"));
    }
    const firsts = windowsOf(list);
    const cardOf = (s) => {
        const el = card(s);
        el.classList.add("shell-card");
        const later = (s.pane ?? 1) > 1 && !!s.layout && firsts.every((o) => o.id !== s.id);
        if (later)
            el.classList.add("pane");
        el.prepend(h("span", { class: "slot", title: tip(later ? "shells.pane" : "shells.go") }, later ? `.${s.pane}` : String(s.slot)));
        const sub = h("span", { class: "shell-sub" }, shellLabel(s), s.watch ? ` · ◎ ${watchLabel(s.watch)}` : "", s.scratch ? " · scratch" : "", doing(s) ? ` · ${doing(s)}` : "");
        el.append(sub);
        return el;
    };
    // Several workspaces: one numbered list each, as each has its own numbers.
    const groups = [...workspaces].map((ws) => h("div", { class: "shells-ws" }, workspaces.size > 1 ? h("div", { class: "shells-ws-name" }, ws, ws === listWorkspace() ? h("small", {}, " · 1–9 here") : null) : null, h("div", { class: "cards" }, ...list.filter((s) => (s.workspace ?? "main") === ws).map(cardOf))));
    return h("section", { class: "shells" }, head, ...groups);
}
/// The strip on a shell's page: its workspace's shells by number, the shown one marked.
export function shellStrip(sessions, s) {
    const ws = s.workspace ?? "main";
    lastWorkspace = ws;
    const all = [...sessions];
    const list = windowsOf(all, ws);
    const nav = h("nav", { class: "shell-strip", title: tip("shells.go") }, ...list.map((o) => h("button", {
        class: `${o.slot === s.slot ? "on" : ""}${o.attention ? " wants" : ""}${o.lost ? " lost" : ""}`,
        "aria-pressed": String(o.slot === s.slot),
        onclick: () => void goShell(all, o.slot ?? 0, ws),
    }, `${o.slot} `, h("small", {}, shellLabel(o.slot === s.slot ? s : o)))), h("button", { class: "plus", title: tip("shells.new"), "aria-label": "New shell", onclick: () => newShellMenu(ws) }, "＋"));
    // A window of several panes: its panes under the strip, a tap shows one (the web shows one
    // pane at a time; the TUI shows the window's layout).
    const panes = s.layout ? panesOf(all, s) : [];
    const sub = panes.length > 1 ? h("nav", { class: "pane-strip", title: tip("shells.pane") }, ...panes.map((o) => h("button", {
        class: `${o.id === s.id ? "on" : ""}${o.attention ? " wants" : ""}`,
        "aria-pressed": String(o.id === s.id),
        onclick: () => go(`#/s/${enc(o.id)}`),
    }, `.${o.pane} `, h("small", {}, shellLabel(o)))), h("button", { class: "plus", title: tip("shells.split"), "aria-label": "Split", onclick: () => void splitShell(s, "right") }, "＋")) : null;
    // A swipe along it goes to the next or the previous shell (a phone's way to flip through).
    let x0 = null;
    nav.addEventListener("touchstart", (e) => { x0 = e.touches[0]?.clientX ?? null; }, { passive: true });
    nav.addEventListener("touchend", (e) => {
        const x1 = e.changedTouches[0]?.clientX;
        if (x0 == null || x1 == null || Math.abs(x1 - x0) < 60)
            return;
        const to = neighbour(all, s, x1 < x0 ? 1 : -1);
        if (to && to.id !== s.id)
            go(`#/s/${enc(to.id)}`);
        x0 = null;
    });
    return sub ? h("div", { class: "shell-strips" }, nav, sub) : nav;
}
/// A copy of a shell as a new pane of its window (`dir`: beside or below it), then its page.
export async function splitShell(s, dir) {
    const r = await ask({ t: "shell_split", addr: s.id, dir }, `Shell ${s.slot}: a new pane`);
    if (r?.info)
        go(`#/s/${enc(r.info.id)}`);
}
function joinForm(s) {
    form(`Join shell ${paneName(s)} into another window`, [{ key: "into", label: "Shell number", type: "number", required: true }], "Join", async (v) => {
        const into = Number(v.into);
        if (!(into >= 1))
            throw new Error("a shell's number, from 1 up");
        await ask({ t: "shell_join", addr: s.id, into, dir: "down" }, `Joined shell ${into}`);
    });
}
/// The neighbour of a shell in its workspace's SHELLS (`dir` = 1 next, -1 previous), for a swipe.
export function neighbour(sessions, s, dir) {
    const list = windowsOf(sessions, s.workspace ?? "main");
    const i = list.findIndex((o) => o.slot === s.slot);
    return i < 0 ? undefined : list[(i + dir + list.length) % list.length];
}
/// What a session's ⋯ menu offers about SHELLS.
export function shellMenuItems(s) {
    if (s.quick || s.pad || s.machine)
        return [];
    if (!s.loose) {
        return s.state.t === "running" ? [{ title: tip("shells.loose"), label: "Move to SHELLS", hint: "A number of its own; it outlives its worktree", run: () => ask({ t: "shell_place", addr: s.id, loose: true, worktree: null }, `${s.name} is in SHELLS now`) }] : [];
    }
    return [
        { title: tip("shells.split"), label: "Split beside", hint: "A copy of it in the same directory, a pane of this number", run: () => void splitShell(s, "right") },
        { title: tip("shells.split"), label: "Split below", hint: "The same, below it", run: () => void splitShell(s, "down") },
        !!s.layout && { title: tip("shells.break"), label: "Break out to its own number", hint: `It's pane ${s.pane} of shell ${s.slot} now`, run: () => ask({ t: "shell_join", addr: s.id, into: null, dir: "right" }, `${s.name} has a number of its own now`) },
        { title: tip("shells.join"), label: "Join another window…", hint: "As a pane of that number", run: () => joinForm(s) },
        { title: tip("shells.move"), label: "Move to number…", hint: `It's shell ${s.slot} now`, run: () => moveForm(s) },
        { title: tip("shells.watch"), label: "Watch…", hint: s.watch ? `Now: ${watchLabel(s.watch)}` : "Its command ends, output, silence, a match", run: () => watchMenu(s) },
        { title: tip("shells.adopt"), label: "Adopt into its worktree", hint: "The one its directory is in", run: () => ask({ t: "shell_place", addr: s.id, loose: false, worktree: null }, `${s.name} joined its worktree`) },
    ];
}
function moveForm(s) {
    form(`Shell ${s.slot}: another number`, [{ key: "slot", label: "Number", type: "number", required: true }], "Move", async (v) => {
        const slot = Number(v.slot);
        if (!(slot >= 1))
            throw new Error("a number from 1 up");
        await ask({ t: "shell_move", addr: s.id, slot }, `It's shell ${slot} now`);
    });
}
function watchMenu(s) {
    const set = (watch, said) => ask({ t: "shell_watch", addr: s.id, watch }, said);
    menu(`Watch shell ${s.slot}`, [
        { title: tip("shells.watch"), label: "When its command ends", hint: "However short; once", run: () => set({ t: "done" }, "Watching: done") },
        { title: tip("shells.watch"), label: "When it prints", hint: "New output while you aren't looking", run: () => set({ t: "output" }, "Watching: output") },
        { title: tip("shells.watch"), label: "When it goes quiet for 30s", run: () => set({ t: "silence", ms: 30_000 }, "Watching: silence 30s") },
        { title: tip("shells.watch"), label: "When it goes quiet for 5m", run: () => set({ t: "silence", ms: 300_000 }, "Watching: silence 5m") },
        { title: tip("shells.watch"), label: "When a line matches…", hint: "A pattern, like ERROR|listening", run: () => form("Watch for a line", [{ key: "re", label: "Pattern", placeholder: "ERROR|listening", required: true }], "Watch", async (v) => { await set({ t: "match", re: String(v.re) }, "Watching for a match"); }) },
        !!s.watch && { title: tip("shells.watch"), label: "Stop watching", run: () => set(null, "Not watched") },
    ]);
}
