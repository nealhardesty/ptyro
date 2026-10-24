// Sessions (ROADMAP WEB1.2): start a task, and everything a session's row menu does in the TUI
// (stop, restart, rename, hibernate, convert, adopt…), plus its scrollback, events, replay and
// export, and talking to several at once.
import { conn } from "./conn.js";
import { css, F } from "./color.js";
import { can, data, liveWorktrees } from "./data.js";
import { createWorktree, startSession } from "./flow.js";
import { Screen } from "./screen.js";
import { actions, ago, ask, badge, Cache, confirm, download, enc, errText, form, go, h, menu, pending, row, sheet, textSheet, ui, views } from "./kit.js";
const ACTIONS = {
    "session.new": "Start a task: say what to do, which agent, and where",
    "session.menu": "More for this session: stop, restart, rename, summarize…",
    "session.stop": "Stop the session (gracefully); a stopped one is removed",
    "session.kill": "Kill the session now",
    "session.restart": "Start the session's command again, in place",
    "session.rename": "Change the session's name (its address)",
    "session.retitle": "Change what the session is titled",
    "session.resume": "Bring a hibernated agent back into its conversation",
    "session.hibernate": "Stop an idle agent and keep it resumable",
    "session.convert": "Switch an agent between its terminal and the chat view",
    "session.adopt": "Make an agent started by hand in this shell a session of its own",
    "session.shell": "A shell in this session's worktree",
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
/// The new-task sheet (UX1.9): what to do, which agent, and where. `where` preselects a
/// project (`p:<id>`) or a worktree (`w:<id>`).
export function newTask(where) {
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
    const agent = input(h("select", {}, ...agentOptions.map(([v, l]) => h("option", { value: v }, l))));
    agent.value = data.defaultAgent in Object.fromEntries(agentOptions) ? data.defaultAgent : agentOptions[0][0];
    const command = input(h("input", { placeholder: "e.g. npm run dev", autocapitalize: "off", spellcheck: "false" }));
    const placeSel = input(h("select", {}, ...place.map(([v, l]) => h("option", { value: v }, l))));
    placeSel.value = where && place.some(([v]) => v === where) ? where : place[0][0];
    const source = input(h("select", {}, ...SOURCES.map(([v, l]) => h("option", { value: v }, l))));
    const which = input(h("input", { placeholder: "", autocapitalize: "off", spellcheck: "false" }));
    const name = input(h("input", { placeholder: "a friendly name is picked for you", autocapitalize: "off", spellcheck: "false" }));
    const err = h("p", { class: "form-err" });
    const commandRow = h("label", { class: "field" }, h("span", {}, "Command"), command);
    const sourceRow = h("label", { class: "field" }, h("span", {}, "Start from"), source);
    const whichRow = h("label", { class: "field" }, h("span", {}, "Which one?"), which);
    const nameRow = h("label", { class: "field" }, h("span", {}, "Name (optional)"), name);
    const sync = () => {
        const fresh = placeSel.value.startsWith("p:");
        commandRow.hidden = agent.value !== "@command";
        sourceRow.hidden = !fresh;
        nameRow.hidden = !fresh;
        whichRow.hidden = !fresh || !["pr", "issue", "branch"].includes(source.value);
        which.placeholder = source.value === "pr" ? "number or URL" : source.value === "issue" ? "number or URL" : "branch name";
        prompt.hidden = agent.value === "@shell" || agent.value === "@command";
    };
    for (const el of [agent, placeSel, source])
        el.addEventListener("change", sync);
    sync();
    const go1 = h("button", { class: "primary" }, "Start");
    const s = sheet("New task", [
        h("label", { class: "field" }, h("span", {}, "Task"), prompt),
        h("label", { class: "field" }, h("span", {}, "Agent"), agent), commandRow,
        h("label", { class: "field" }, h("span", {}, "Where"), placeSel), sourceRow, whichRow, nameRow, err,
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
        go1.disabled = true;
        const [kind, id] = [placeSel.value.slice(0, 1), placeSel.value.slice(2)];
        try {
            if (kind === "w") {
                const w = data.worktrees.get(id);
                const spec = { worktree: w.id, cwd: "" };
                if (isAgent)
                    Object.assign(spec, { agent: a, prompt: text, title: text });
                else if (a === "@command")
                    spec.argv = ["sh", "-c", command.value.trim()];
                s.close();
                await startSession(spec);
            }
            else {
                const wt = { project: id, name: name.value.trim() || undefined, agent: isAgent ? a : undefined, prompt: isAgent ? text : undefined, title: text || undefined };
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
                    const spec = { worktree: r.info.id, cwd: "" };
                    if (a === "@command")
                        spec.argv = ["sh", "-c", command.value.trim()];
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
    form("Rename", [{ key: "name", label: "Name", value: s.name, required: true, hint: "Its address: what you type to find it" }], "Rename", async (v) => {
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
        textSheet(`Why ${s.name} is ${s.status?.status ?? "like this"}`, lines.join("\n") || "No decisions recorded yet.");
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
        if (e.code === "conflict" && !force)
            confirm("It's mid-turn", `${errText(e)} Switch anyway? The turn in progress is interrupted.`, "Switch", () => convert(s, true));
        else
            ui.toast(errText(e));
    }
}
export async function openQuickShell(w, cwd = "") {
    const r = await ask({ t: "quick_shell", worktree: w?.id ?? null, cwd, create: true, env: {}, size: null, workspace: null });
    if (r?.info)
        go(`#/s/${enc(r.info.id)}`);
}
function stop(s) {
    if (!live(s)) {
        void ask({ t: "remove", addr: addr(s) }, `Removed ${s.name}`).then(() => { if (location.hash.startsWith("#/s/"))
            go("#/"); });
        return;
    }
    confirm(`Stop ${s.name}?`, "It gets a moment to finish, then it's stopped.", "Stop", async () => { notice(await ask({ t: "kill", addr: addr(s), signal: null }), `Stopped ${s.name}`); });
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
        s.worktree && { title: tip("session.shell"), label: "A shell in its worktree", run: () => void openQuickShell(w) },
        { title: tip("session.scrollback"), label: "Scrollback", hint: "Everything it printed, searchable", run: () => go(`#/scroll/${enc(s.id)}`) },
        { title: tip("session.events"), label: "Events", run: () => go(`#/events/${enc(s.id)}`) },
        { title: tip("session.replay"), label: "Replay", hint: "Its recording, played back", run: () => go(`#/replay/${enc(s.id)}`) },
        { title: tip("session.export"), label: "Save its recording…", run: () => exportSession(s.id, s.name) },
        { title: tip("session.rename"), label: "Rename…", run: () => renameSession(s) },
        { title: tip("session.retitle"), label: "Set title…", run: () => retitleSession(s) },
        live(s) && { title: tip("session.kill"), label: "Kill now", danger: true, hint: "No waiting", run: () => confirm(`Kill ${s.name}?`, "It's stopped at once, with no chance to clean up.", "Kill", async () => notice(await ask({ t: "kill", addr: addr(s), signal: 9 }), `Killed ${s.name}`)) },
        { title: tip("session.stop"), label: live(s) ? "Stop" : "Remove", danger: true, run: () => stop(s) },
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
function scrollView(id) {
    const s = [...sessionsRef().values()].find((x) => x.id === id);
    const title = s ? `${s.name} · scrollback` : "Scrollback";
    const doc = scroll.get(id);
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
    return h("div", { class: "page" }, ui.header(title, id ? `#/s/${enc(id)}` : true, h("button", { class: "icon", title: "Load again", onclick: () => scroll.reload(id) }, "↻")), h("main", { class: "stack", title: tip("session.scrollback") }, search, body));
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
function play(p) {
    clearTimeout(p.timer);
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
/// The app's session map, for views that title themselves by session.
export function useSessions(get) { sessionsRef = get; setSessionNames((id) => get().get(id)?.name ?? id); }
views.set("scroll", scrollView);
views.set("events", eventsView);
views.set("replay", replayView);
views.set("history", () => historyView());
