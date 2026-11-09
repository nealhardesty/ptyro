// Sessions (ROADMAP WEB1.2): start a task, and everything a session's row menu does in the TUI
// (stop, restart, rename, hibernate, convert, adopt…), plus its scrollback, events, replay and
// export, and talking to several at once.
import { conn, host } from "./conn.js";
import { css, F } from "./color.js";
import { againLabel, can, data, liveWorktrees, refreshLaunch, splitMachine } from "./data.js";
import { createWorktree, startSession } from "./flow.js";
import { Screen } from "./screen.js";
import { actions, ago, argWords, ask, badge, Cache, confirm, download, enc, errText, fixOf, form, go, h, menu, pending, row, sheet, statusWord, textSheet, tipOf, ui, views } from "./kit.js";
import { fillProfiles } from "./settings.js";
import { shellMenuItems } from "./shells.js";
const ACTIONS = {
    "session.new": "Start a task: say what to do, which agent, and where",
    "session.menu": "More for this session: stop, restart, rename, summarize…",
    "session.stop": "Stop the session: its agent's turn is interrupted and it gets a moment to exit, then it's signalled; it stays listed",
    "session.kill": "Kill the session now (SIGKILL, no chance to clean up); it stays listed",
    "session.remove": "Remove the session from the list: an ended one is forgotten, a running one is stopped first",
    "session.restart": "Start the session's command again, in place",
    "session.rename": "Change the session's name (its address)",
    "session.tab": "Open this session in a terminal on the computer: a new Windows Terminal tab (when the computer is WSL)",
    "session.retitle": "Change what the session is titled",
    "session.resume": "Bring a hibernated agent back into its conversation",
    "session.hibernate": "Stop an idle agent and keep it resumable",
    "session.convert": "Switch an agent between its terminal and the chat view",
    "session.adopt": "Make an agent started by hand in this shell a session of its own",
    "session.shell": "A shell in this session's worktree",
    "shell.here": "This worktree's quick terminal (its one shell), on a page of its own (the web has no split panes, so there is no drawer)",
    "shell.pane": "This worktree's quick terminal, on a page of its own; the same one the terminal's T and C-t show",
    "shell.new": "A new shell of its own in this worktree, besides its quick terminal (the terminal's !)",
    "session.again": "Quick new: another of the agent this worktree last started (the default agent the first time), here, no questions",
    "session.summarize": "Summarize what the session is doing",
    "session.explain": "Why the session has the status it has",
    "session.mute": "Silence the session's notifications for a while",
    "session.message": "Send the session a message through its mailbox",
    "session.verify": "Hold the session's turns until a check passes",
    "session.broadcast": "Type the same thing into several sessions",
    "session.scrollback": "The session's scrollback, searchable",
    "session.events": "The session's events: status changes, turns, exits",
    "session.replay": "Play the session's recording back",
    "session.export": "Save the session's recording as text or asciicast",
    "session.history": "Sessions from earlier, with their recordings",
    "session.find": "Find a session by name, title, project, agent or status",
    "sidebar.filter": "Show every session, only the running ones (a live process), or only the active ones (an agent working or waiting for you, a shell running a command)",
};
const tip = actions(ACTIONS);
const live = (s) => s.state.t === "running";
const addr = (s) => s.id;
// ── start a task ────────────────────────────────────────────────────────────────────────────
const SOURCES = [
    ["default", "The project's main branch"],
    ["pr", "A pull request"],
    ["issue", "An issue"],
    ["branch", "An existing branch"],
    ["take", "My uncommitted changes (moved over)"],
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
    const verify = input(h("input", { placeholder: "e.g. cargo test, or a run script's name", autocapitalize: "off", spellcheck: "false" }));
    const args = input(h("input", { placeholder: "e.g. --model opus", autocapitalize: "off", spellcheck: "false" }));
    const box = () => input(h("input", { type: "checkbox" }));
    const structured = box(), noSetup = box(), noFetch = box(), lead = box();
    const maxWorkers = input(h("input", { type: "number", inputmode: "numeric", placeholder: "4" }));
    const leadAgents = agents.map((a) => { const c = box(); c.value = a.name; return c; });
    const check = (c, label, hint) => h("label", { class: "field check" }, c, h("span", {}, label), h("small", {}, hint));
    const err = h("p", { class: "form-err" });
    const commandRow = h("label", { class: "field" }, h("span", {}, "Command"), command);
    const sourceRow = h("label", { class: "field" }, h("span", {}, "Start from"), source);
    const whichRow = h("label", { class: "field" }, h("span", {}, "Which one?"), which);
    const nameRow = h("label", { class: "field" }, h("span", {}, "Name (optional)"), name);
    const baseRow = h("label", { class: "field" }, h("span", {}, "Base"), base, h("small", {}, "The ref the new branch starts from"));
    const verifyRow = h("label", { class: "field" }, h("span", {}, "Done when"), verify, h("small", {}, "Verified-done: a turn counts as done only once this passes; failures go back to the agent"));
    const argsRow = h("label", { class: "field" }, h("span", {}, "Agent arguments"), args, h("small", {}, "Extra arguments for the agent's command"));
    const leadRows = h("div", { style: "display: grid; gap: 12px" }, h("label", { class: "field" }, h("span", {}, "Workers at once"), maxWorkers), h("div", { class: "field", role: "group", "aria-label": "Agents it may start" }, h("span", {}, "Agents it may start"), ...leadAgents.map((c) => h("label", { class: "field check" }, c, h("span", {}, c.value))), h("small", {}, "None ticked: any")));
    const agentOnly = [verifyRow, argsRow, check(structured, "Chat mode (full tool-call visibility)", "Drive the agent over its structured protocol and show a chat view"), check(lead, "Make it a lead", "It gets `ptyro mcp` to start and coordinate workers, within limits"), leadRows];
    const freshOnly = [baseRow, check(noSetup, "Skip setup scripts", "Don't run the project's setup script"), check(noFetch, "Don't fetch", "Don't fetch the base first")];
    const more = h("details", { class: "more" }, h("summary", { style: "cursor: pointer; color: var(--muted)" }, "More options"), h("div", { style: "display: grid; gap: 12px; padding-top: 10px" }, h("label", { class: "field" }, h("span", {}, "Profile"), profile, h("small", {}, "The account it runs under")), h("label", { class: "field" }, h("span", {}, "Title"), title, h("small", {}, "What it's for, for people to read; its name stays its address")), ...freshOnly, ...agentOnly));
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
        for (const r of agentOnly)
            r.hidden = !isAgent;
        // A new worktree's agent takes no extra arguments (`worktree_new` has none).
        argsRow.hidden = !isAgent || fresh;
        leadRows.hidden = !isAgent || !lead.checked;
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
    lead.addEventListener("change", sync);
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
        ? h("p", { class: "form-err" }, "No coding agent is installed on the computer yet: ", setupLink, " (Claude Code, Codex, or Antigravity for a Google account).")
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
        // For an agent: verified-done, chat mode and lead (`ptyro new --verify --structured --lead`).
        const leadPolicy = () => {
            if (!lead.checked)
                return undefined;
            const p = {};
            if (maxWorkers.value.trim())
                p.max_sessions = Number(maxWorkers.value);
            const may = leadAgents.filter((c) => c.checked).map((c) => c.value);
            if (may.length)
                p.agents = may;
            return p;
        };
        const agentBits = () => isAgent ? { verify: verify.value.trim() || undefined, structured: structured.checked, lead: leadPolicy() } : {};
        try {
            if (kind === "w") {
                const w = data.worktrees.get(id);
                const spec = { worktree: w.id, cwd: "", profile: prof, name: name.value.trim() || undefined, title: label };
                if (isAgent)
                    Object.assign(spec, { agent: a, prompt: text, argv: argWords(args.value) }, agentBits());
                else if (a === "@command")
                    spec.command = command.value.trim();
                s.close();
                await startSession(spec);
            }
            else {
                const wt = { project: id, name: name.value.trim() || undefined, agent: isAgent ? a : undefined, prompt: isAgent ? text : undefined, title: label, profile: prof, base: base.value.trim() || undefined, ...agentBits() };
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
                else if (src === "take")
                    wt.take_changes = true;
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
export function renameSession(s) {
    form("Rename", [{ key: "name", label: "Name", value: s.name, required: true, slug: "name", hint: "Its address: what you type to find it, unique in its project. A space or other odd character becomes _ (a-z 0-9 . _ -)" }], "Rename", async (v) => {
        await conn.request({ t: "rename", addr: addr(s), name: v.name });
        ui.toast(`Renamed to ${v.name}`);
    });
}
export function retitleSession(s) {
    form("Title", [{ key: "title", label: "Title", value: s.title ?? "", hint: "What it's for, for people to read. Empty goes back to the default." }], "Save", async (v) => {
        await conn.request({ t: "retitle", addr: addr(s), title: v.title || null, worktree: false });
    });
}
export function messageSession(s) {
    form(`Message ${s.name}`, [
        { key: "body", label: "Message", type: "textarea", required: true, placeholder: "It reaches the agent through its mailbox" },
        { key: "interrupt", label: "Interrupt it", type: "check", hint: "Deliver in the middle of a turn, where the agent allows it" },
    ], "Send", async (v) => {
        await conn.request({ t: "msg_post", from: null, to: addr(s), kind: "info", body: v.body, run: null, in_reply_to: null, priority: v.interrupt ? "interrupt" : null, ttl_ms: null, key: null });
        ui.toast("Sent");
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
export function verifySession(s) {
    form(`Verify ${s.name}`, [
        { key: "check", label: "Check", value: s.verify?.check ?? "", placeholder: "e.g. cargo test", hint: "Its turns count as done only once this command passes. Empty turns it off." },
        { key: "max", label: "Bounces", type: "number", value: "", placeholder: "default", hint: "How many times a failing check sends it back to work" },
    ], "Save", async (v) => {
        await conn.request({ t: "set_verify", addr: addr(s), check: v.check || null, max_bounces: v.max ? Number(v.max) : null });
        ui.toast(v.check ? "Verifying" : "Verification off");
    });
}
function explainSession(s) {
    conn.request({ t: "status_explain", addr: addr(s) }).then((r) => {
        const lines = r.lines.map((l) => `${new Date(l.at_ms).toLocaleTimeString()}  ${l.applied ? "✓" : "·"} ${l.source}: ${l.signal}\n      ${l.reason}`);
        textSheet(`Why ${s.name} is ${s.status ? statusWord(s.status.status) : "like this"}`, lines.join("\n") || "No decisions recorded yet.");
    }, (e) => ui.toast(errText(e)));
}
async function summarize(s) {
    const r = await ask({ t: "summarize", addr: addr(s) });
    if (r)
        textSheet(`${s.name}, in a line`, r.info?.summary?.text ?? "(no summary: set `[summaries] command` in your config)");
}
async function convert(s, force = false) {
    const to = !s.structured;
    try {
        await conn.request({ t: "convert", addr: addr(s), structured: to, force });
        ui.toast(to ? "Now a chat" : "Now a terminal");
    }
    catch (e) {
        const f = fixOf(e);
        if (f?.fix === "force" && !force)
            confirm("It's mid-turn", errText(e), f.label, () => convert(s, true));
        else
            ui.toast(errText(e));
    }
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
    const e = v?.entries?.find((x) => x.key === "ui.confirm_kill");
    return !e || String(e.value) !== "false";
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
    try {
        await conn.request({ t: "remove", addr: addr(s), stop: false });
        ui.toast(`Removed ${s.name}`);
        left();
    }
    catch (e) {
        // It started again meanwhile: the daemon says so, and offers to stop it too.
        const f = fixOf(e);
        if (f?.fix === "stop")
            confirm("It's running", errText(e), f.label, async () => { if (await ask(ENDS.remove.req(addr(s)), `${ENDS.remove.said} ${s.name}`))
                left(); });
        else
            ui.fail(errText(e));
    }
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
        agentLive && { title: tip("session.convert"), label: s.structured ? "Show the terminal UI" : "Show as a chat", hint: "Continues the same conversation", run: () => convert(s) },
        live(s) && s.inner_agent === "claude" && { title: tip("session.adopt"), label: "Adopt the agent running here", hint: "Manage it like one started by ptyro", run: async () => notice(await ask({ t: "adopt", addr: addr(s) }), "Adopted") },
        { title: tip("session.explain"), label: "Why this status?", run: () => explainSession(s) },
        live(s) && { title: tip("session.summarize"), label: "Summarize", run: () => void summarize(s) },
        agentLive && { title: tip("session.hibernate"), label: "Hibernate", hint: "Stop it, keep it resumable", run: async () => notice(await ask({ t: "hibernate", addr: addr(s) }), "Hibernated") },
        s.kind === "agent" && { title: tip("session.message"), label: "Message…", hint: "Through its mailbox", run: () => messageSession(s) },
        agentLive && { title: tip("session.mute"), label: "Mute notifications…", run: () => muteSession(s) },
        agentLive && can("full") && { title: tip("session.verify"), label: "Verify when done…", hint: s.verify ? `Checking: ${s.verify.check}` : "Hold its turns until a check passes", run: () => verifySession(s) },
        w && { label: "Open its worktree", hint: `${w.project_slug}/${w.name}`, run: () => go(`#/wt/${enc(w.id)}`) },
        w && { title: tip("session.again"), label: `Another ${againLabel(w.id)}`, hint: "The agent its worktree last started, there", run: () => void startAgain(w) },
        w && { title: tip("shell.new"), label: "A new shell in its worktree", hint: "Besides its quick terminal", run: () => void newShell(w) },
        s.worktree && { title: tip("session.shell"), label: "Its worktree's quick terminal", hint: "On a page of its own", run: () => void openQuickShell(w) },
        { title: tip("session.scrollback"), label: "Scrollback", hint: "Everything it printed, searchable", run: () => go(`#/scroll/${enc(s.id)}`) },
        { title: tip("session.events"), label: "Events", run: () => go(`#/events/${enc(s.id)}`) },
        { title: tip("session.replay"), label: "Replay", hint: "Its recording, played back", run: () => go(`#/replay/${enc(s.id)}`) },
        { title: tip("session.export"), label: "Save its recording…", run: () => exportSession(s.id, s.name) },
        can("full") && { title: tip("session.tab"), label: "Open on the computer", hint: "A new Windows Terminal tab there", run: () => void openOnComputer(s) },
        ...shellMenuItems(s),
        { title: tip("session.rename"), label: "Rename…", run: () => renameSession(s) },
        { title: tip("session.retitle"), label: "Set title…", run: () => retitleSession(s) },
        live(s) && { title: tip("session.stop"), label: "Stop", danger: true, hint: "Interrupt it, then end it; it stays listed", run: () => end(s, "stop") },
        live(s) && { title: tip("session.kill"), label: "Kill", danger: true, hint: "SIGKILL now; it stays listed", run: () => end(s, "kill") },
        { title: tip("session.remove"), label: live(s) ? "Stop and remove" : "Remove", danger: true, hint: live(s) ? "Stop it, then forget it" : "Forget it", run: () => void remove(s) },
    ];
    menu(s.name, items);
}
export function exportSession(id, name) {
    menu(`Save ${name}'s recording`, [
        { label: "Text", hint: "What the screen ended up showing, with its scrollback", run: async () => { const r = await ask({ t: "export", addr: id, format: "text" }); if (r)
                download(`${name}.txt`, r.text); } },
        { label: "asciicast", hint: "Plays in asciinema", run: async () => { const r = await ask({ t: "export", addr: id, format: "asciicast" }); if (r)
                download(`${name}.cast`, r.text, "application/x-asciicast"); } },
    ]);
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
const replays = new Cache((id) => conn.request({ t: "replay", addr: id }).then((r) => r.replay));
let player = null;
/// Draw the replay's current step: its frames are applied from the start (cheap: they're deltas).
function seek(p, to) {
    const steps = p.doc.steps;
    to = Math.max(0, Math.min(steps.length - 1, to));
    if (to < p.at || p.at < 0) {
        p.screen = new Screen();
        p.at = -1;
    }
    for (let i = p.at + 1; i <= to; i++)
        p.screen.apply({ session: "replay", seq: i, keyframe: steps[i].keyframe, delta: steps[i].delta });
    p.at = to;
    p.slider.value = String(to);
    p.label.textContent = `${(steps[to].at_ms / 1000).toFixed(1)}s / ${(p.doc.duration_ms / 1000).toFixed(1)}s`;
    drawReplay(p);
}
function drawReplay(p) {
    // The live-screen painter is the app's; a replay draws the cell grid itself, simply.
    const s = p.screen, c = p.canvas, ctx = c.getContext("2d");
    if (!ctx)
        return;
    const cs = getComputedStyle(document.documentElement);
    const FG = cs.getPropertyValue("--term-fg").trim() || "#e6edf3", BG = cs.getPropertyValue("--term-bg").trim() || "#0d1117";
    const family = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
    const wrap = c.parentElement;
    const dpr = window.devicePixelRatio || 1;
    ctx.font = `100px ${family}`;
    const ratio = ctx.measureText("M").width / 100;
    const px = Math.max(5, Math.min(15, (wrap.clientWidth - 8 || 360) / (s.size.cols * ratio)));
    const cw = px * ratio, ch = Math.round(px * 1.25);
    const w = Math.ceil(cw * s.size.cols), hh = ch * s.size.rows;
    c.style.width = `${w}px`;
    c.style.height = `${hh}px`;
    if (c.width !== Math.ceil(w * dpr))
        c.width = Math.ceil(w * dpr);
    if (c.height !== Math.ceil(hh * dpr))
        c.height = Math.ceil(hh * dpr);
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = BG;
    ctx.fillRect(0, 0, w, hh);
    ctx.textBaseline = "top";
    s.rows.forEach((row, y) => row.forEach((cell, x) => {
        if (cell.spacer)
            return;
        const st = s.styles.get(cell.s) ?? {};
        const flags = st.flags ?? 0;
        let fg = css(st.fg, FG), bg = css(st.bg, BG);
        if (flags & F.INVERSE)
            [fg, bg] = [bg, fg];
        if (bg !== BG) {
            ctx.fillStyle = bg;
            ctx.fillRect(x * cw, y * ch, cw * (cell.wide ? 2 : 1) + 0.5, ch);
        }
        if (!cell.t || cell.t === " ")
            return;
        ctx.font = `${flags & F.ITALIC ? "italic " : ""}${flags & F.BOLD ? "bold " : ""}${px}px ${family}`;
        ctx.globalAlpha = flags & F.DIM ? 0.6 : 1;
        ctx.fillStyle = fg;
        ctx.fillText(cell.t, x * cw, y * ch + (ch - px) / 2);
        ctx.globalAlpha = 1;
    }));
}
/// Players that may have a timer running: leaving the page stops them.
const players = new Set();
window.addEventListener("hashchange", () => { for (const p of players) {
    clearTimeout(p.timer);
    p.playing = false;
} players.clear(); });
function play(p) {
    clearTimeout(p.timer);
    players.add(p);
    if (!p.playing)
        return;
    const steps = p.doc.steps;
    if (p.at >= steps.length - 1) {
        p.playing = false;
        ui.render();
        return;
    }
    const gap = Math.min(1200, Math.max(30, (steps[p.at + 1].at_ms - steps[p.at].at_ms) / p.speed));
    p.timer = window.setTimeout(() => { seek(p, p.at + 1); play(p); }, gap);
}
function replayView(id) {
    const s = sessionsRef().get(id);
    const doc = replays.get(id);
    const failed = pending(replays, id, "Loading the recording…");
    const head = ui.header(s ? `${s.name} · replay` : "Replay", `#/s/${enc(id)}`, h("button", { class: "icon", title: tip("session.export"), onclick: () => exportSession(id, s?.name ?? id) }, "⤓"));
    if (failed)
        return h("div", { class: "page" }, head, h("main", { class: "stack" }, failed));
    if (!doc.steps.length)
        return h("div", { class: "page" }, head, h("main", { class: "stack" }, h("p", { class: "empty" }, "That recording is empty.")));
    if (!player || player.id !== id || player.doc !== doc) {
        clearTimeout(player?.timer);
        const canvas = h("canvas", { class: "term" });
        const slider = h("input", { type: "range", min: 0, max: doc.steps.length - 1, value: 0, "aria-label": "Position" });
        const p = { id, doc, screen: new Screen(), at: -1, playing: false, speed: 1, timer: 0, canvas, label: h("span", { class: "replay-time" }), slider };
        slider.addEventListener("input", () => { p.playing = false; seek(p, Number(slider.value)); ui.render(); });
        player = p;
        requestAnimationFrame(() => seek(p, 0));
    }
    const p = player;
    const toggle = h("button", { class: "primary", onclick: () => {
            if (!p.playing && p.at >= doc.steps.length - 1)
                seek(p, 0);
            p.playing = !p.playing;
            play(p);
            ui.render();
        } }, p.playing ? "Pause" : "Play");
    const speeds = [1, 2, 4, 8].map((x) => h("button", { class: p.speed === x ? "on" : "", onclick: () => { p.speed = x; ui.render(); } }, `${x}×`));
    return h("div", { class: "page" }, head, h("main", { class: "replay", title: tip("session.replay") }, h("div", { class: "term-wrap" }, p.canvas), h("div", { class: "replay-bar" }, toggle, p.slider, p.label), h("div", { class: "keys" }, ...speeds), doc.truncated ? h("p", { class: "empty" }, "This recording is long: the replay covers its start.") : null));
}
// ── earlier sessions ────────────────────────────────────────────────────────────────────────
const history = new Cache(() => conn.request({ t: "history", limit: 200 }).then((r) => r.entries));
function historyView() {
    const entries = history.get("");
    const body = pending(history, "") ?? (entries.length ? h("div", { class: "rows" }, ...entries.map((e) => row(h("span", {}, e.name, " ", badge(e.state.t ?? String(e.state), e.current ? "ok" : "")), `${e.kind} · ${e.argv.join(" ").slice(0, 80)}`, e.ended_ms ? ago(e.ended_ms) : "", { onclick: () => menu(e.name, [
            e.current && { label: "Open", run: () => go(`#/s/${enc(e.id)}`) },
            { label: "Replay", run: () => go(`#/replay/${enc(e.id)}`) },
            { label: "Save its recording…", run: () => exportSession(e.id, e.name) },
        ]) }))) : h("p", { class: "empty" }, "No earlier sessions."));
    return h("div", { class: "page" }, ui.header("Earlier sessions", true), h("main", { class: "stack", title: tip("session.history") }, body));
}
// ── wiring ──────────────────────────────────────────────────────────────────────────────────
let sessionsRef = () => new Map();
/// Every session the app knows.
export const allSessions = () => sessionsRef();
/// The app's session map, for views that title themselves by session.
export function useSessions(get) { sessionsRef = get; setSessionNames((id) => get().get(id)?.name ?? id); }
views.set("scroll", scrollView);
views.set("events", eventsView);
views.set("replay", replayView);
views.set("history", () => historyView());
