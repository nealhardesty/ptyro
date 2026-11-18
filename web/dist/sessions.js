// Sessions (ROADMAP WEB1.2): start a task, and everything a session's row menu does in the TUI
// (stop, restart, rename, hibernate…), plus its scrollback and events, and talking to several
// at once.
import { conn, host } from "./conn.js";
import { css, F } from "./color.js";
import { againLabel, can, data, liveWorktrees, refreshLaunch, splitMachine } from "./data.js";
import { createWorktree, startSession } from "./flow.js";
import { actions, NAME_HINT, slugInput, confirmsKill, argWords, ask, Cache, confirm, enc, errText, form, go, h, menu, pending, sheet, statusWord, textSheet, tipOf, ui, views } from "./kit.js";
import { fillProfiles } from "./settings.js";
import { shellMenuItems } from "./shells.js";
const ACTIONS = {
    "session.new": "Start a task: say what to do, which agent, and where",
    "session.menu": "More for this session: stop, restart, rename, hibernate…",
    "session.stop": "Stop the session: its agent's turn is interrupted and it gets a moment to exit, then it's signalled; it stays listed",
    "session.kill": "Kill the session now (SIGKILL, no chance to clean up); it stays listed",
    "session.remove": "Remove the session from the list: an ended one is forgotten, a running one is stopped first",
    "session.restart": "Start the session's command again, in place",
    "session.rename": "Rename the session: its name is its address (what you type to find it), its title is what people read",
    "session.tab": "Open this session in a terminal on the computer: a new Windows Terminal tab (when the computer is WSL)",
    "session.resume": "Bring a hibernated agent back into its conversation",
    "session.hibernate": "Stop an idle agent and keep it resumable",
    "session.shell": "A shell in this session's worktree",
    "shell.here": "This worktree's quick terminal (its one shell), on a page of its own (the web has no split panes, so there is no drawer)",
    "shell.pane": "This worktree's quick terminal, on a page of its own; the same one the terminal's T and C-t show",
    "shell.new": "A new shell of its own in this worktree, besides its quick terminal (the terminal's !)",
    "session.again": "Quick new: another of the agent this worktree last started (the default agent the first time), here, no questions",
    "session.explain": "Why the session has the status it has",
    "session.mute": "Silence the session's notifications for a while",
    "session.broadcast": "Type the same thing into several sessions",
    "session.scrollback": "The session's scrollback, searchable",
    "session.events": "The session's events: status changes, turns, exits",
    "session.find": "Find a session by name, title, project, agent or status",
    "sidebar.filter": "Show every session, only the running ones (a live process), or only the active ones (an agent working or waiting for you, a shell running a command)",
};
const tip = actions(ACTIONS);
// A live process, as the TUI and the daemon say it (`is_live`): starting counts.
const live = (s) => s.state.t === "running" || s.state.t === "starting";
const addr = (s) => s.id;
// ── start a task ────────────────────────────────────────────────────────────────────────────
const SOURCES = [
    ["default", "The project's main branch"],
    ["pr", "A pull request"],
    ["issue", "An issue"],
    ["branch", "An existing branch"],
];
/// `12`, `#12`, `pr 12`, a pull request or issue URL → its number.
const numberIn = (t) => {
    const m = t.match(/(\d+)\s*$/) ?? t.match(/\/(?:pull|issues)\/(\d+)/);
    return m ? Number(m[1]) : undefined;
};
const AUTO_PROFILE = "automatic (worktree → project pin → workspace)";
/// The new-task sheet (UX1.9): what to do, which agent, and where. `where` preselects a
/// project (`p:<id>`) or a worktree (`w:<id>`).
export function newTask(where, task) {
    const projects = [...data.projects.values()];
    const wts = liveWorktrees();
    if (!projects.length) {
        menu("No projects yet", [{ label: "Add a project…", hint: "Pick a repository on your computer", run: () => go("#/p") }]);
        return;
    }
    const agents = data.agents.filter((a) => a.installed || !a.program);
    const agentOptions = [
        ...agents.map((a) => [a.name, a.name === data.defaultAgent ? `${a.name} (default)` : a.name]),
        ["@shell", "A shell (no agent)"],
        ...(can("full") ? [["@command", "A command…"]] : []),
    ];
    const place = [
        ...projects.map((p) => [`p:${p.id}`, `${p.slug} · in a new worktree`]),
        ...wts.map((w) => [`w:${w.id}`, `${w.project_slug}/${w.name}${w.is_root ? " (main checkout)" : ""}${w.title ? ` — ${w.title}` : ""}`]),
    ];
    const input = (el) => el;
    const prompt = input(h("textarea", { rows: 3, placeholder: "What should it do? e.g. “fix the flaky login test”", autocapitalize: "sentences" }));
    if (task)
        prompt.value = task;
    const agent = input(h("select", {}, ...agentOptions.map(([v, l]) => h("option", { value: v }, l))));
    agent.value = data.defaultAgent in Object.fromEntries(agentOptions) ? data.defaultAgent : agentOptions[0][0];
    const command = input(h("input", { placeholder: "e.g. npm run dev", autocapitalize: "off", spellcheck: "false" }));
    const placeSel = input(h("select", {}, ...place.map(([v, l]) => h("option", { value: v }, l))));
    placeSel.value = where && place.some(([v]) => v === where) ? where : place[0][0];
    const source = input(h("select", {}, ...SOURCES.map(([v, l]) => h("option", { value: v }, l))));
    const which = input(h("input", { placeholder: "", autocapitalize: "off", spellcheck: "false" }));
    const names = h("datalist", { id: "branch-names" });
    which.setAttribute("list", "branch-names");
    const name = input(h("input", { placeholder: "a friendly name is picked for you", autocapitalize: "off", spellcheck: "false" }));
    // More options (`ptyro new`'s and `ptyro wt new`'s other flags), folded: the quick path stays short.
    const profile = input(h("select", {}, h("option", { value: "" }, AUTO_PROFILE)));
    const title = input(h("input", { placeholder: "default: the task", autocapitalize: "sentences" }));
    const base = input(h("input", { placeholder: "default: the project's base, e.g. origin/main", autocapitalize: "off", spellcheck: "false" }));
    base.setAttribute("list", "branch-names");
    const args = input(h("input", { placeholder: "e.g. --model opus", autocapitalize: "off", spellcheck: "false" }));
    const box = () => input(h("input", { type: "checkbox" }));
    const noSetup = box(), noFetch = box();
    const check = (c, label, hint) => h("label", { class: "field check" }, c, h("span", {}, label), h("small", {}, hint));
    const err = h("p", { class: "form-err" });
    const commandRow = h("label", { class: "field" }, h("span", {}, "Command"), command);
    const sourceRow = h("label", { class: "field" }, h("span", {}, "Start from"), source);
    const whichRow = h("label", { class: "field" }, h("span", {}, "Which one?"), which);
    slugInput(name, "name");
    const nameRow = h("label", { class: "field" }, h("span", {}, "Name (optional)"), name, h("small", {}, NAME_HINT));
    const baseRow = h("label", { class: "field" }, h("span", {}, "Base"), base, h("small", {}, "The ref the new branch starts from"));
    const argsRow = h("label", { class: "field" }, h("span", {}, "Agent arguments"), args, h("small", {}, "Extra arguments for the agent's command"));
    const freshOnly = [baseRow, check(noSetup, "Skip setup scripts", "Don't run the project's setup script"), check(noFetch, "Don't fetch", "Don't fetch the base first")];
    const more = h("details", { class: "more" }, h("summary", { style: "cursor: pointer; color: var(--muted)" }, "More options"), h("div", { style: "display: grid; gap: 12px; padding-top: 10px" }, h("label", { class: "field" }, h("span", {}, "Profile"), profile, h("small", {}, "The account it runs under")), h("label", { class: "field" }, h("span", {}, "Title"), title, h("small", {}, "What people read: what it's for; its name stays its address")), ...freshOnly, argsRow));
    // The profiles of the machine the place is on.
    const machineOfPlace = () => {
        const [kind, id] = [placeSel.value.slice(0, 1), placeSel.value.slice(2)];
        const slug = kind === "p" ? data.projects.get(id)?.slug : data.worktrees.get(id)?.project_slug;
        return slug ? splitMachine(slug)[0] : null;
    };
    let profileMachine;
    const loadProfiles = () => {
        const m = machineOfPlace();
        if (m === profileMachine)
            return;
        profileMachine = m;
        void fillProfiles(profile, m, profile.value, ["", AUTO_PROFILE]);
    };
    const sync = () => {
        const fresh = placeSel.value.startsWith("p:");
        const isAgent = !agent.value.startsWith("@");
        commandRow.hidden = agent.value !== "@command";
        sourceRow.hidden = !fresh;
        whichRow.hidden = !fresh || !["pr", "issue", "branch"].includes(source.value);
        which.placeholder = source.value === "pr" ? "number or URL" : source.value === "issue" ? "number or URL" : "branch name";
        prompt.hidden = agent.value === "@shell" || agent.value === "@command";
        for (const r of freshOnly)
            r.hidden = !fresh;
        // A new worktree's agent takes no extra arguments (`worktree_new` has none).
        argsRow.hidden = !isAgent || fresh;
        name.placeholder = fresh ? "a friendly name is picked for you" : "the session's name; a friendly one is picked for you";
        loadProfiles();
    };
    const loadBranches = () => {
        if (!placeSel.value.startsWith("p:"))
            return;
        conn.request({ t: "branches", project: placeSel.value.slice(2) }).then((r) => names.replaceChildren(...r.names.map((n) => h("option", { value: n }))), () => { });
    };
    for (const el of [agent, placeSel, source])
        el.addEventListener("change", () => { sync(); if (source.value === "branch")
            loadBranches(); });
    base.addEventListener("focus", loadBranches);
    sync();
    const go1 = h("button", { class: "primary" }, "Start");
    // Setup is a tap away (BOOT9): with no agent on the computer, said first.
    const none = data.agents.length > 0 && !agents.length;
    // Only a device that can install (full scope) is offered it; others are told where.
    const full = conn.scope === "full";
    const setupLink = full
        ? h("a", { href: "#/setup", title: tipOf("setup.offer"), onclick: () => s.close() }, none ? "Set up agents and tools" : "Install another agent")
        : h("span", {}, "run ", h("code", {}, "ptyro setup"), " on the computer");
    const agentHint = none
        ? h("p", { class: "form-err" }, "No coding agent is installed on the computer yet: ", setupLink, " (Claude Code, Codex or OpenCode).")
        : full ? h("small", { class: "sub" }, setupLink) : null;
    const s = sheet("New task", [
        h("label", { class: "field" }, h("span", {}, "Task"), prompt),
        h("label", { class: "field" }, h("span", {}, "Agent"), agent), agentHint, commandRow,
        h("label", { class: "field" }, h("span", {}, "Where"), placeSel), sourceRow, whichRow, names, nameRow, more, err,
    ], [h("button", { onclick: () => s.close() }, "Cancel"), go1]);
    go1.addEventListener("click", async () => {
        err.textContent = "";
        const text = prompt.value.trim();
        const a = agent.value;
        const isAgent = !a.startsWith("@");
        if (isAgent && !text) {
            err.textContent = "Say what it should do.";
            prompt.focus();
            return;
        }
        if (a === "@command" && !command.value.trim()) {
            err.textContent = "Which command?";
            command.focus();
            return;
        }
        if (profile.value === "@new") {
            err.textContent = "Pick a profile.";
            profile.focus();
            return;
        }
        go1.disabled = true;
        const [kind, id] = [placeSel.value.slice(0, 1), placeSel.value.slice(2)];
        const prof = profile.value || undefined;
        const label = title.value.trim() || text || undefined;
        try {
            if (kind === "w") {
                const w = data.worktrees.get(id);
                const spec = { worktree: w.id, cwd: "", profile: prof, name: name.value.trim() || undefined, title: label };
                if (isAgent)
                    Object.assign(spec, { agent: a, prompt: text, argv: argWords(args.value) });
                else if (a === "@command")
                    spec.command = command.value.trim();
                s.close();
                await startSession(spec);
            }
            else {
                const wt = { project: id, name: name.value.trim() || undefined, agent: isAgent ? a : undefined, prompt: isAgent ? text : undefined, title: label, profile: prof, base: base.value.trim() || undefined };
                if (noSetup.checked)
                    wt.setup = false;
                if (noFetch.checked)
                    wt.fetch = false;
                const src = source.value;
                if (src === "pr" || src === "issue") {
                    const n = numberIn(which.value);
                    if (!which.value.trim() || (src === "pr" && n === undefined)) {
                        err.textContent = src === "pr" ? "Give the pull request's number or URL." : "Give the issue's number or URL.";
                        go1.disabled = false;
                        return;
                    }
                    if (src === "pr")
                        wt.pr = n;
                    else
                        wt.issue = which.value.trim();
                }
                else if (src === "branch") {
                    if (!which.value.trim()) {
                        err.textContent = "Which branch?";
                        go1.disabled = false;
                        return;
                    }
                    Object.assign(wt, { existing: true, branch: which.value.trim() });
                }
                s.close();
                const r = await createWorktree(wt);
                // A shell or command in a new worktree: start it once the worktree exists.
                if (r && !isAgent) {
                    const spec = { worktree: r.info.id, cwd: "", profile: prof };
                    if (a === "@command")
                        spec.command = command.value.trim();
                    await startSession(spec);
                }
            }
        }
        catch (e) {
            err.textContent = errText(e);
            go1.disabled = false;
        }
    });
    requestAnimationFrame(() => prompt.focus());
}
// ── a session's menu ────────────────────────────────────────────────────────────────────────
const notice = (r, ok) => { if (r)
    ui.toast(ok); };
/// One sheet for both: its name (the address) and its title (what people read).
export function renameSession(s) {
    // Another machine's session is renamed there, by the name it has there (MM1.B).
    const bare = splitMachine(s.name)[1];
    form("Rename", [
        { key: "name", label: "Name", value: bare, required: true, slug: "name", hint: `Its address: what you type to find it, unique in its project. ${NAME_HINT}` },
        { key: "title", label: "Title", value: s.title ?? "", hint: "What people read: what it's for. Empty clears it." },
    ], "Save", async (v) => {
        const name = String(v.name ?? "").trim(), title = String(v.title ?? "").trim();
        const said = [];
        let at = addr(s);
        if (name && name !== bare) {
            const r = await conn.request({ t: "rename", addr: at, name });
            at = r?.info?.id ?? at;
            said.push(`Renamed to ${name}`);
        }
        if (title !== (s.title ?? "")) {
            await conn.request({ t: "retitle", addr: at, title: title || null, worktree: false });
            said.push(title ? "title set" : "title cleared");
        }
        if (said.length)
            ui.toast(said.join(", ").replace(/^t/, "T"));
    });
}
export function muteSession(s) {
    const mute = (minutes, unmute = false, label = "Muted") => async () => {
        notice(await ask({ t: "mute", target: addr(s), minutes, unmute }), label);
    };
    menu(`Mute ${s.name}`, [
        { label: "For 1 hour", run: mute(60, false, "Muted for an hour") },
        { label: "For 8 hours", run: mute(480, false, "Muted for 8 hours") },
        { label: "Until I unmute it", run: mute(null, false, "Muted") },
        { label: "Unmute", run: mute(null, true, "Notifications back on") },
    ]);
}
function explainSession(s) {
    conn.request({ t: "status_explain", addr: addr(s) }).then((r) => {
        const lines = r.lines.map((l) => `${new Date(l.at_ms).toLocaleTimeString()}  ${l.applied ? "✓" : "·"} ${l.source}: ${l.signal}\n      ${l.reason}`);
        textSheet(`Why ${s.name} is ${s.status ? statusWord(s.status.status) : "like this"}`, lines.join("\n") || "No decisions recorded yet.");
    }, (e) => ui.toast(errText(e)));
}
/// Quick new (UX3.2, UX3.7): another of the agent `w` last started (with its profile and
/// arguments), else its project's default agent, else the default agent. Asked for by name, so a
/// `control` device may start it.
export async function startAgain(w) {
    await refreshLaunch(w.id);
    const l = data.lastLaunch.get(w.id);
    const spec = l?.agent ? { agent: l.agent, profile: l.profile ?? null, argv: l.argv ?? [] } : { agent: data.defaultAgent };
    if (await startSession({ ...spec, worktree: w.id })) {
        await refreshLaunch(w.id);
        ui.render();
    }
}
/// `!` (UX3.4): a new shell of its own in `w`, not its quick terminal.
export async function newShell(w) {
    await startSession({ worktree: w.id });
}
/// The quick-new button's words: `+ claude`.
export const againWords = (w) => `+ ${againLabel(w.id)}`;
export async function openQuickShell(w, cwd = "") {
    const r = await ask({ t: "quick_shell", worktree: w?.id ?? null, cwd, create: true, env: {}, size: null, workspace: null });
    if (r?.info)
        go(`#/s/${enc(r.info.id)}`);
}
/// `[ui] confirm_kill`: Stop, Kill, and Stop and remove ask first, as in the TUI. Read where
/// this device may read the config; elsewhere they always ask.
async function asksFirst() {
    if (conn.scope !== "full")
        return true;
    const v = await host("settings.view", { scope: "user" }).catch(() => null);
    return confirmsKill(conn.scope, v?.entries);
}
/// Leave its page once it's gone from the list.
const left = () => { if (location.hash.startsWith("#/s/"))
    go("#/"); };
/// The same three words as the CLI and the TUI (SPEC §7.3): stop is graceful, kill is SIGKILL
/// now (both keep it listed), remove forgets it (stopping a running one first).
const ENDS = {
    stop: { verb: "Stop", said: "Stopped", what: "Its agent's turn is interrupted and it gets a moment to exit; then it's signalled. It stays in the list.", req: (a) => ({ t: "kill", addr: a, signal: null }) },
    kill: { verb: "Kill", said: "Killed", what: "It's killed at once (SIGKILL), with no chance to clean up. It stays in the list.", req: (a) => ({ t: "kill", addr: a, signal: 9 }) },
    remove: { verb: "Stop and remove", said: "Stopped and removed", what: "It's stopped, then forgotten: it leaves the list.", req: (a) => ({ t: "remove", addr: a, stop: true }) },
};
function end(s, how) {
    const e = ENDS[how];
    const run = async () => { if (await ask(e.req(addr(s)), `${e.said} ${s.name}`) && how === "remove")
        left(); };
    void asksFirst().then((yes) => { if (yes)
        confirm(`${e.verb} ${s.name}?`, e.what, e.verb, run);
    else
        void run(); });
}
/// Forget an ended session; a running one is stopped first.
async function remove(s) {
    if (live(s))
        return end(s, "remove");
    // A dormant agent could still be resumed: forgetting it asks, as `ptyro rm` and the TUI do.
    if (s.state.t === "dormant") {
        confirm(`Remove the dormant session ${s.name}?`, "Its agent's conversation, which it could resume, is forgotten. This can't be undone.", "Remove", () => forget(s));
        return;
    }
    await forget(s);
}
async function forget(s) {
    // It started again meanwhile: the daemon says so, and offers to stop it too (`ask`).
    if (await ask({ t: "remove", addr: addr(s), stop: false }, `Removed ${s.name}`))
        left();
}
/// `ptyro attach <id> --tab` on the computer: a Windows Terminal tab there (WSL), attached.
async function openOnComputer(s) {
    try {
        const r = await host("session.tab", { session: s.id });
        ui.toast(r.said ?? "Opened");
    }
    catch (e) {
        ui.toast(errText(e));
    }
}
/// What the session's ⋯ button offers: only what makes sense for it now.
export function sessionMenu(s) {
    const dormant = s.state.t === "dormant";
    const ended = !live(s) && !dormant;
    const agentLive = s.kind === "agent" && live(s);
    const w = s.worktree ? data.worktrees.get(s.worktree) : undefined;
    const items = [
        dormant && { title: tip("session.resume"), label: "Resume", hint: "Back into its conversation", run: async () => notice(await ask({ t: "resume", addr: addr(s) }), "Resumed") },
        ended && { title: tip("session.restart"), label: "Restart", hint: "Run its command again, in place", run: async () => notice(await ask({ t: "restart", addr: addr(s) }), "Restarted") },
        { title: tip("session.explain"), label: "Why this status?", run: () => explainSession(s) },
        agentLive && { title: tip("session.hibernate"), label: "Hibernate", hint: "Stop it, keep it resumable", run: async () => notice(await ask({ t: "hibernate", addr: addr(s) }), "Hibernated") },
        agentLive && { title: tip("session.mute"), label: "Mute notifications…", run: () => muteSession(s) },
        w && { label: "Open its worktree", hint: `${w.project_slug}/${w.name}`, run: () => go(`#/wt/${enc(w.id)}`) },
        w && { title: tip("session.again"), label: `Another ${againLabel(w.id)}`, hint: "The agent its worktree last started, there", run: () => void startAgain(w) },
        w && { title: tip("shell.new"), label: "A new shell in its worktree", hint: "Besides its quick terminal", run: () => void newShell(w) },
        s.worktree && { title: tip("session.shell"), label: "Its worktree's quick terminal", hint: "On a page of its own", run: () => void openQuickShell(w) },
        { title: tip("session.scrollback"), label: "Scrollback", hint: "Everything it printed, searchable", run: () => go(`#/scroll/${enc(s.id)}`) },
        { title: tip("session.events"), label: "Events", run: () => go(`#/events/${enc(s.id)}`) },
        can("full") && { title: tip("session.tab"), label: "Open on the computer", hint: "A new Windows Terminal tab there", run: () => void openOnComputer(s) },
        ...shellMenuItems(s),
        { title: tip("session.rename"), label: "Rename…", run: () => renameSession(s) },
        live(s) && { title: tip("session.stop"), label: "Stop", danger: true, hint: "Interrupt it, then end it; it stays listed", run: () => end(s, "stop") },
        live(s) && { title: tip("session.kill"), label: "Kill", danger: true, hint: "SIGKILL now; it stays listed", run: () => end(s, "kill") },
        { title: tip("session.remove"), label: live(s) ? "Stop and remove" : "Remove", danger: true, hint: live(s) ? "Stop it, then forget it" : "Forget it", run: () => void remove(s) },
    ];
    menu(s.name, items);
}
// ── several at once ─────────────────────────────────────────────────────────────────────────
/// Sessions marked in the list for `broadcast`.
export const marked = new Set();
export function broadcast(ids) {
    const names = ids.map((i) => data_name(i)).join(", ");
    form("Type into several", [
        { key: "text", label: `Text, for ${ids.length}`, type: "textarea", required: true, hint: names },
        { key: "enter", label: "Press Enter after", type: "check", value: true },
    ], "Send", async (v) => {
        const rs = await Promise.allSettled(ids.map((id) => conn.request({ t: "send_text", addr: id, text: v.text, enter: v.enter, wait_ready_ms: 5000 })));
        const bad = rs.filter((r) => r.status === "rejected").length;
        ui.toast(bad ? `Sent to ${ids.length - bad}; ${bad} refused` : `Sent to ${ids.length}`);
        marked.clear();
    });
}
let sessionNames = (i) => i;
export const setSessionNames = (f) => { sessionNames = f; };
const data_name = (id) => sessionNames(id);
// ── scrollback ──────────────────────────────────────────────────────────────────────────────
const scroll = new Cache((id) => conn.request({ t: "read_scrollback", addr: id, lines: 20000 }).then((r) => r.doc));
/// A frozen row as HTML spans, coloured as the terminal had it.
function rowEl(doc, styles, runs) {
    const line = h("div", { class: "sl" });
    for (const r of runs) {
        const st = styles.get(r.style) ?? {};
        const flags = st.flags ?? 0;
        let fg = st.fg ? css(st.fg, "") : "", bg = st.bg ? css(st.bg, "") : "";
        if (flags & F.INVERSE)
            [fg, bg] = [bg || "var(--term-bg)", fg || "var(--term-fg)"];
        const span = h("span", {}, r.text);
        if (fg)
            span.style.color = fg;
        if (bg)
            span.style.background = bg;
        if (flags & F.BOLD)
            span.style.fontWeight = "bold";
        if (flags & F.ITALIC)
            span.style.fontStyle = "italic";
        if (flags & F.DIM)
            span.style.opacity = "0.6";
        if (flags & F.UNDERLINE)
            span.style.textDecoration = "underline";
        line.append(span);
    }
    if (!runs.length)
        line.append("​");
    return line;
}
let scrollQuery = "";
/// The page as built: every re-render (a session event, the clock) would otherwise throw away the
/// reader's place and the text typed in the search box, and rebuild up to 20,000 rows.
let scrollPage = null;
function scrollView(id) {
    const s = [...sessionsRef().values()].find((x) => x.id === id);
    const title = s ? `${s.name} · scrollback` : "Scrollback";
    const doc = scroll.get(id);
    if (doc && scrollPage && scrollPage.id === id && scrollPage.q === scrollQuery && scrollPage.doc === doc)
        return scrollPage.el;
    const body = pending(scroll, id) ?? (() => {
        const styles = new Map(doc.styles.map((d) => [d.id, d.style]));
        const q = scrollQuery.toLowerCase();
        const lines = doc.rows.map((r, i) => ({ i, text: r.runs.map((x) => x.text).join(""), r }));
        const hits = q ? lines.filter((l) => l.text.toLowerCase().includes(q)) : [];
        const pre = h("div", { class: "scrollback" });
        for (const l of lines) {
            const el = rowEl(doc, styles, l.r.runs);
            if (q && l.text.toLowerCase().includes(q))
                el.classList.add("hit");
            if (l.i === doc.history)
                pre.append(h("div", { class: "sl-split" }, "── screen now ──"));
            pre.append(el);
        }
        requestAnimationFrame(() => {
            const first = q ? pre.querySelector(".hit") : null;
            if (first)
                first.scrollIntoView({ block: "center" });
            else if (!q)
                pre.scrollTop = pre.scrollHeight;
        });
        return h("div", { class: "scroll-wrap" }, h("div", { class: "sl-note" }, q ? `${hits.length} line${hits.length === 1 ? "" : "s"} match` : `${doc.rows.length} lines`), pre);
    })();
    const search = h("input", { class: "search", type: "search", placeholder: "Search", value: scrollQuery, "aria-label": "Search the scrollback", autocapitalize: "off" });
    search.addEventListener("change", () => { scrollQuery = search.value; ui.render(); });
    const page = h("div", { class: "page" }, ui.header(title, id ? `#/s/${enc(id)}` : true, h("button", { class: "icon", title: "Load again", onclick: () => scroll.reload(id) }, "↻")), h("main", { class: "stack", title: tip("session.scrollback") }, search, body));
    scrollPage = doc ? { id, q: scrollQuery, doc, el: page } : null;
    return page;
}
// ── events ──────────────────────────────────────────────────────────────────────────────────
const events = new Cache((id) => conn.request({ t: "events", session: id, limit: 300 }).then((r) => r.events));
function describe(e) {
    const d = e.detail;
    if (d == null || typeof d !== "object")
        return String(d ?? "");
    return Object.entries(d).filter(([, v]) => v != null && v !== "").map(([k, v]) => `${k}=${typeof v === "object" ? JSON.stringify(v) : v}`).join("  ");
}
function eventsView(id) {
    const evs = events.get(id);
    const body = pending(events, id) ?? (evs.length
        ? h("div", { class: "timeline" }, ...[...evs].reverse().map((e) => h("div", { class: "tl" }, h("time", {}, new Date(e.at_ms).toLocaleTimeString()), h("b", {}, e.kind), h("span", {}, describe(e)))))
        : h("p", { class: "empty" }, "No events yet."));
    const s = sessionsRef().get(id);
    return h("div", { class: "page" }, ui.header(s ? `${s.name} · events` : "Events", `#/s/${enc(id)}`, h("button", { class: "icon", onclick: () => events.reload(id) }, "↻")), h("main", { class: "stack", title: tip("session.events") }, body));
}
// ── wiring ──────────────────────────────────────────────────────────────────────────────────
let sessionsRef = () => new Map();
/// Every session the app knows.
export const allSessions = () => sessionsRef();
/// The app's session map, for views that title themselves by session.
export function useSessions(get) { sessionsRef = get; setSessionNames((id) => get().get(id)?.name ?? id); }
views.set("scroll", scrollView);
views.set("events", eventsView);
