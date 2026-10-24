// ptyro web app (SPEC §16.4, M9.3): sessions, the inbox, a live terminal view drawn from the
// daemon's screen frames, quick replies, and answers to permission prompts. Talks the ptyro
// protocol (JSON encoding) over `ptyro serve`'s WebSocket, signed in with a device token from
// `ptyro pair`. No dependencies, no third-party requests.
import { Screen } from "./screen.js";
import { conn, token } from "./conn.js";
import { css, F } from "./color.js";
import { actions, ago, h, patch, tipOf, ui, views } from "./kit.js";
import { loadAll, onEvent } from "./data.js";
import "./projects.js";
import "./review.js";
import "./runs.js";
import "./settings.js";
import { broadcast, marked, newTask, sessionMenu, useSessions } from "./sessions.js";
// What this app does, by name: the Web column of docs/parity.md names these, and `make parity`
// checks every name it uses is here (SPEC §1.3, ROADMAP WEB1). Each is its control's tooltip.
const ACTIONS = {
    "sessions": "Every session: what it's doing, and who needs you first",
    "session.open": "A session's live screen, or its chat",
    "session.reply": "Type to the session; Enter sends, Shift+Enter starts a new line",
    "session.keys": "Keys for the session: arrows, Esc, Tab, Enter, Ctrl-C",
    "session.view": "Chat or terminal; the terminal fitted or 1:1",
    "answer": "Answer what it asks: allow once, always, or deny",
    "inbox": "The inbox: what happened while you were away",
    "inbox.read": "Mark everything in the inbox read",
    "notifications": "Notifications on this device (a test is sent)",
    "pair": "Pair this device with the link or QR code from `ptyro pair`",
    "unpair": "Forget this device's pairing",
};
const tip = actions(ACTIONS);
// ── storage: the token lives in localStorage (page) and IndexedDB (the service worker) ────────
const db = () => new Promise((ok, err) => {
    const r = indexedDB.open("ptyro", 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onsuccess = () => ok(r.result);
    r.onerror = () => err(r.error);
});
async function idbSet(key, value) {
    const d = await db();
    await new Promise((ok, err) => {
        const tx = d.transaction("kv", "readwrite");
        if (value === null)
            tx.objectStore("kv").delete(key);
        else
            tx.objectStore("kv").put(value, key);
        tx.oncomplete = () => ok();
        tx.onerror = () => err(tx.error);
    });
}
function takePairCode() {
    const m = location.hash.match(/^#pair=([A-Za-z0-9_\-]+)$/);
    if (m) {
        history.replaceState(null, "", location.pathname + "#/");
        void claim(m[1]);
    }
}
/// Why the last pairing attempt failed (shown on the pairing screen).
let pairNote = "";
/// Trade the one-time code from `ptyro pair` for this device's token (`POST /api/pair`).
async function claim(code) {
    pairNote = "Pairing…";
    render();
    try {
        const r = await fetch("/api/pair", {
            method: "POST",
            headers: { "Content-Type": "application/json" },
            body: JSON.stringify({ code }),
        });
        if (!r.ok)
            throw new Error(await r.text());
        const { token } = await r.json();
        pairNote = "";
        useToken(token);
    }
    catch (e) {
        pairNote = `Pairing failed: ${e.message || e}`;
    }
    render();
}
/// Store a device token and sign in with it (also from a tab that was open, unpaired).
function useToken(t) {
    localStorage.setItem("ptyro.token", t);
    void idbSet("token", t);
    if (conn.state === "rejected" || !conn.ws)
        conn.connect();
}
// ── drawing ─────────────────────────────────────────────────────────────────────────────────
function draw(canvas, s, fit) {
    const wrap = canvas.parentElement;
    const dpr = window.devicePixelRatio || 1;
    const cs = getComputedStyle(document.documentElement);
    const FG = cs.getPropertyValue("--term-fg").trim() || "#e6edf3";
    const BG = cs.getPropertyValue("--term-bg").trim() || "#0d1117";
    const ctx = canvas.getContext("2d");
    if (!ctx)
        return;
    const family = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
    // Fit the session's width to the screen; otherwise a readable size and horizontal scroll.
    ctx.font = `100px ${family}`;
    const ratio = ctx.measureText("M").width / 100;
    const avail = wrap.clientWidth - 8;
    const px = fit ? Math.max(5, Math.min(16, avail / (s.size.cols * ratio))) : 13;
    const cw = px * ratio, ch = Math.round(px * 1.25);
    const w = Math.ceil(cw * s.size.cols), h = ch * s.size.rows;
    canvas.style.width = `${w}px`;
    canvas.style.height = `${h}px`;
    // Resizing the backing store reallocates it: only when the size changed.
    const bw = Math.ceil(w * dpr), bh = Math.ceil(h * dpr);
    if (canvas.width !== bw)
        canvas.width = bw;
    if (canvas.height !== bh)
        canvas.height = bh;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = BG;
    ctx.fillRect(0, 0, w, h);
    ctx.textBaseline = "top";
    for (let y = 0; y < s.rows.length; y++) {
        const row = s.rows[y];
        for (let x = 0; x < row.length; x++) {
            const c = row[x];
            if (c.spacer)
                continue;
            const st = s.styles.get(c.s) ?? {};
            const flags = st.flags ?? 0;
            let fg = css(st.fg, FG), bg = css(st.bg, BG);
            if (flags & F.INVERSE)
                [fg, bg] = [bg, fg];
            const width = c.wide ? 2 : 1;
            if (bg !== BG) {
                ctx.fillStyle = bg;
                ctx.fillRect(x * cw, y * ch, cw * width + 0.5, ch);
            }
            if (!c.t || c.t === " " || flags & F.HIDDEN)
                continue;
            ctx.font = `${flags & F.ITALIC ? "italic " : ""}${flags & F.BOLD ? "bold " : ""}${px}px ${family}`;
            ctx.globalAlpha = flags & F.DIM ? 0.6 : 1;
            ctx.fillStyle = fg;
            ctx.fillText(c.t, x * cw, y * ch + (ch - px) / 2);
            if (flags & F.UNDERLINE)
                ctx.fillRect(x * cw, y * ch + ch - 2, cw * width, 1);
            if (flags & F.STRIKE)
                ctx.fillRect(x * cw, y * ch + ch / 2, cw * width, 1);
            ctx.globalAlpha = 1;
        }
    }
    if (s.cursor.visible && s.cursor.y < s.size.rows) {
        ctx.globalAlpha = 0.55;
        ctx.fillStyle = FG;
        const under = s.cursor.shape === "underline";
        ctx.fillRect(s.cursor.x * cw, s.cursor.y * ch + (under ? ch - 2 : 0), s.cursor.shape === "bar" ? 2 : cw, under ? 2 : ch);
        ctx.globalAlpha = 1;
    }
}
const GLYPH = { working: "●", blocked: "⚠", idle: "○", done: "✓", errored: "✗", starting: "◌", unknown: "·" };
function statusOf(s) {
    if (s.state.t === "exited" || s.state.t === "failed")
        return "exited";
    if (s.state.t === "dormant")
        return "dormant";
    return s.status?.status ?? (s.kind === "agent" ? "unknown" : "shell");
}
// ── app state and views ─────────────────────────────────────────────────────────────────────
const app = {
    sessions: new Map(),
    inbox: [],
    screens: new Map(),
    /// Structured sessions' transcripts (M10), and the ones shown as a terminal instead.
    chats: new Map(),
    termView: new Set(),
    /// Unsent replies, per session.
    drafts: new Map(),
    viewing: null,
    fit: true,
    toast: "",
};
const root = document.getElementById("app");
/// `#/s/<id>` a session, `#/inbox`, `#/<name>/<rest>` a registered view (kit.ts), else the list.
function route() {
    const m = location.hash.match(/^#\/s\/(.+)$/);
    if (m) {
        try {
            return { view: "session", id: decodeURIComponent(m[1]) };
        }
        catch {
            return { view: "list" };
        }
    }
    if (location.hash === "#/inbox")
        return { view: "inbox" };
    const v = location.hash.match(/^#\/([a-z]+)(?:\/(.*))?$/);
    if (v && views.has(v[1]))
        return { view: v[1], id: v[2] ?? "" };
    return { view: "list" };
}
/// The tabs along the bottom of a top-level page; the active one is the page's.
const TABS = [["list", "#/", "Sessions"], ["p", "#/p", "Projects"], ["runs", "#/runs", "Runs"], ["sets", "#/sets", "Sets"], ["more", "#/more", "More"]];
function withTabs(view, page) {
    if (!TABS.some(([v]) => v === view))
        return page;
    page.classList.add("tabbed");
    page.append(h("nav", { class: "tabs" }, ...TABS.map(([v, href, label]) => h("a", { href, class: v === view ? "on" : "" }, label))));
    return page;
}
async function refresh() {
    try {
        const r = await conn.request({ t: "list" });
        app.sessions = new Map(r.sessions.map((s) => [s.id, s]));
        const ib = await conn.request({ t: "inbox", unread_only: false, limit: 50 });
        app.inbox = ib.entries;
    }
    catch { /* reconnecting */ }
    render();
}
function toast(text) {
    app.toast = text;
    render();
    setTimeout(() => { if (app.toast === text) {
        app.toast = "";
        render();
    } }, 3500);
}
async function answer(id, decision) {
    // The prompt this answers (the blocked status shown): a different one pending now is refused.
    const st = app.sessions.get(id)?.status;
    const expect = st?.status === "blocked" ? st.since_ms : undefined;
    try {
        await conn.request({ t: "answer", addr: id, decision, expect });
        toast(decision === "deny" ? "Denied" : "Allowed");
    }
    catch (e) {
        if (e.code === "conflict") {
            toast("That prompt changed — have another look");
            void refresh();
        }
        else
            toast(e.message);
    }
}
/// `back`: true goes to the session list; a string is where it goes instead.
function header(title, back, ...extra) {
    const unread = app.inbox.filter((e) => !e.read_ms).length;
    const dot = h("span", { class: `dot ${conn.state}`, title: conn.state });
    return h("header", {}, back ? h("a", { class: "back", href: typeof back === "string" ? back : "#/", "aria-label": "Back" }, "‹") : h("img", { class: "logo", src: "/icon.svg", alt: "" }), h("h1", {}, title), ...extra, dot, h("a", { class: "inbox-link", href: "#/inbox", title: tip("inbox") }, "✉", unread ? h("b", {}, String(unread)) : null), h("button", { class: "icon", title: tip("notifications"), onclick: () => void enablePush() }, "🔔"));
}
function sessionCard(s) {
    const st = statusOf(s);
    const detail = s.status?.status === "blocked" ? s.status.detail ?? s.attention?.text : s.activity ?? s.status?.detail ?? s.title;
    const card = h("a", { class: `card ${st}${selecting && marked.has(s.id) ? " picked" : ""}`, href: selecting ? "#/" : `#/s/${encodeURIComponent(s.id)}`, title: tip("session.open"),
        onclick: selecting ? (e) => { e.preventDefault(); if (!marked.delete(s.id))
            marked.add(s.id); render(); } : undefined }, h("span", { class: "glyph" }, selecting ? (marked.has(s.id) ? "☑" : "☐") : GLYPH[st] ?? "·"), h("div", { class: "main" }, h("div", { class: "name" }, s.name, s.agent ? h("small", {}, ` ${s.agent}`) : null), h("div", { class: "sub" }, [st, s.project, s.worktree].filter(Boolean).join(" · ")), detail ? h("div", { class: "detail" }, detail) : null), h("time", {}, s.status ? ago(s.status.since_ms) : ago(s.created_ms)));
    if (s.status?.status === "blocked" && s.state.t === "running") {
        return h("div", { class: "blocked-wrap" }, card, h("div", { class: "actions", title: tip("answer") }, h("button", { class: "allow", onclick: () => void answer(s.id, "allow") }, "Allow"), h("button", { class: "deny", onclick: () => void answer(s.id, "deny") }, "Deny")));
    }
    return card;
}
/// Marking sessions to type into several at once (`broadcast`).
let selecting = false;
function listView() {
    const order = ["blocked", "errored", "working", "starting", "done", "idle", "unknown", "shell", "dormant", "exited"];
    const all = [...app.sessions.values()].filter((s) => !s.quick).sort((a, b) => order.indexOf(statusOf(a)) - order.indexOf(statusOf(b)) || a.name.localeCompare(b.name));
    const body = all.length ? all.map(sessionCard) : [h("div", { class: "empty" }, h("p", {}, "Nothing is running."), h("button", { class: "primary", title: tipOf("session.new"), onclick: () => newTask() }, "Start a task"))];
    const picked = [...marked].filter((id) => app.sessions.has(id));
    const tools = selecting
        ? h("div", { class: "select-bar" }, h("span", {}, `${picked.length} selected`), h("button", { class: "small primary", disabled: !picked.length, title: tipOf("session.broadcast"), onclick: () => { broadcast(picked); selecting = false; render(); } }, "Type to them"), h("button", { class: "small", onclick: () => { selecting = false; marked.clear(); render(); } }, "Cancel"))
        : null;
    const head = header("ptyro", false, all.length > 1 ? h("button", { class: "icon", title: tipOf("session.broadcast"), onclick: () => { selecting = true; render(); } }, "☑") : null, h("button", { class: "icon plus", title: tipOf("session.new"), "aria-label": "New task", onclick: () => newTask() }, "＋"));
    return h("div", {}, head, tools, h("main", { class: "list", title: tip("sessions") }, ...body, h("a", { class: "foot-link", href: "#/history", title: tipOf("session.history") }, "Earlier sessions")));
}
function inboxView() {
    const items = app.inbox.map((e) => h("a", { class: `entry ${e.read_ms ? "read" : ""}`, href: `#/s/${encodeURIComponent(e.session)}` }, h("span", { class: `kind ${e.kind}` }, e.kind), h("div", { class: "main" }, h("div", { class: "name" }, e.title), h("div", { class: "detail" }, e.body)), h("time", {}, ago(e.at_ms))));
    const mark = h("button", { class: "small", title: tip("inbox.read"), onclick: async () => { await conn.request({ t: "inbox_mark", all: true, read: true }); await refresh(); } }, "Mark all read");
    return h("div", {}, header("Inbox", true), h("main", { class: "list" }, items.length ? mark : null, ...(items.length ? items : [h("p", { class: "empty" }, "Nothing here.")])));
}
const KEYS = [
    ["Esc", { t: "esc" }], ["Tab", { t: "tab" }], ["↑", { t: "up" }], ["↓", { t: "down" }], ["←", { t: "left" }], ["→", { t: "right" }], ["⏎", { t: "enter" }],
];
function sendKey(id, code, mods = 0) {
    conn.send({ t: "input", session: id, event: { t: "key", key: { code, mods } } });
}
let shell = null;
function sessionShell(id) {
    if (shell?.id === id)
        return shell;
    const reply = h("textarea", { rows: 2, enterkeyhint: "send", title: tip("session.reply") });
    reply.value = app.drafts.get(id) ?? "";
    reply.addEventListener("input", () => app.drafts.set(id, reply.value));
    const send = async () => {
        const text = reply.value;
        if (!text.trim())
            return;
        try {
            await conn.request({ t: "send_text", addr: id, text, enter: true, wait_ready_ms: null });
            if (reply.value === text) {
                reply.value = "";
                app.drafts.delete(id);
            }
        }
        catch (e) {
            toast(e.message);
        }
    };
    reply.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
        e.preventDefault();
        void send();
    } });
    const canvas = h("canvas", { class: "term" });
    const chat = h("div", { class: "chat" });
    requestAnimationFrame(() => { chat.scrollTop = chat.scrollHeight; });
    shell = {
        id, reply, canvas, chat,
        el: h("div", { class: "session" }),
        replyRow: h("div", { class: "reply" }, reply, h("button", { class: "send", onclick: () => void send() }, "Send")),
        termWrap: h("div", { class: "term-wrap" }, canvas),
        chatEls: new Map(),
        empty: h("p", { class: "empty" }, "Nothing yet."),
    };
    return shell;
}
function sessionView(id) {
    const s = app.sessions.get(id);
    if (!s)
        return h("div", {}, header("…", true), h("main", {}, h("p", { class: "empty" }, conn.state === "open" ? "No such session." : "Connecting…")));
    const st = statusOf(s);
    const sh = sessionShell(id);
    sh.reply.placeholder = s.kind === "agent" ? `Reply to ${s.name}…` : "Type a line…";
    const blocked = s.status?.status === "blocked" && s.state.t === "running";
    const chat = s.structured && !app.termView.has(id);
    if (chat)
        updateChat(id, sh);
    patch(sh.el, [
        header(s.name, true, h("button", { class: "icon", title: tipOf("session.menu"), "aria-label": "More", onclick: () => sessionMenu(s) }, "⋯")),
        h("div", { class: `pill ${st}` }, GLYPH[st] ?? "·", " ", st, s.status?.detail && !blocked ? ` · ${s.status.detail}` : "", s.status ? ` · ${ago(s.status.since_ms)}` : ""),
        ...(blocked ? [h("section", { class: "prompt" }, h("div", {}, h("b", {}, s.status?.kind === "permission" ? "Wants permission" : "Waiting for you"), s.held ? h("span", { class: "tag", title: "answers go to the agent as a decision" }, "exact") : null), h("pre", {}, s.status?.detail ?? s.attention?.text ?? ""), h("div", { class: "actions", title: tip("answer") }, h("button", { class: "allow", onclick: () => void answer(id, "allow") }, "Allow"), h("button", { class: "always", onclick: () => void answer(id, "always") }, "Always"), h("button", { class: "deny", onclick: () => void answer(id, "deny") }, "Deny")))] : []),
        chat ? sh.chat : sh.termWrap,
        h("div", { class: "keys", title: tip("session.keys") }, ...KEYS.map(([label, code]) => h("button", { onclick: () => sendKey(id, code) }, label)), h("button", { onclick: () => sendKey(id, { t: "char", c: "c" }, 4) }, "^C"), s.structured
            ? h("button", { title: tip("session.view"), onclick: () => { if (!app.termView.delete(id))
                    app.termView.add(id); render(); } }, app.termView.has(id) ? "Chat" : "Terminal")
            : h("button", { class: app.fit ? "on" : "", title: tip("session.view"), onclick: () => { app.fit = !app.fit; paint(); render(); } }, app.fit ? "Fit" : "1:1")),
        sh.replyRow,
    ]);
    return sh.el;
}
/// A structured session's transcript as cards (M10), updated in place: unchanged items keep
/// their elements; it follows new output only when already scrolled to the bottom.
function updateChat(id, sh) {
    const box = sh.chat;
    const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 32;
    const top = box.scrollTop;
    const items = app.chats.get(id)?.items ?? [];
    const els = new Map();
    const kids = [];
    for (const it of items) {
        const key = JSON.stringify(it);
        const old = sh.chatEls.get(it.id);
        let el = old?.key === key ? old.el : chatItem(id, it);
        if (!el)
            continue;
        if (old && old.el !== el && old.el instanceof HTMLDetailsElement && el instanceof HTMLDetailsElement)
            el.open = old.el.open;
        els.set(it.id, { key, el });
        kids.push(el);
    }
    sh.chatEls = els;
    patch(box, kids.length ? kids : [sh.empty]);
    if (atBottom)
        requestAnimationFrame(() => { box.scrollTop = box.scrollHeight; });
    else
        box.scrollTop = top;
}
function chatItem(id, it) {
    switch (it.t) {
        case "user": return h("div", { class: "msg user" }, it.text);
        case "assistant": return h("div", { class: "msg assistant" }, it.text, it.streaming ? h("span", { class: "caret" }, "▍") : null);
        case "thinking": return h("div", { class: "msg thinking" }, "· ", it.text.split("\n")[0]);
        case "tool": {
            const status = it.status === "done" ? "done" : it.status === "failed" ? "failed" : it.status === "declined" ? "declined" : "running";
            const det = h("details", { class: `tool ${status}` }, h("summary", {}, `${it.name} · ${it.title}`));
            if (it.diff) {
                const pre = h("pre", { class: "diff" });
                for (const l of it.diff.split("\n")) {
                    if (l.startsWith("---") || l.startsWith("+++"))
                        continue;
                    pre.append(h("span", { class: l.startsWith("+") ? "add" : l.startsWith("-") ? "del" : "" }, l + "\n"));
                }
                det.append(pre);
            }
            if (it.output)
                det.append(h("pre", {}, it.output));
            return det;
        }
        case "permission":
            if (it.decided)
                return h("div", { class: "turn-end" }, `${it.title.split("\n")[0] || it.tool} → ${it.decided}`);
            return h("section", { class: "prompt" }, h("b", {}, `Wants permission: ${it.tool}`), h("pre", {}, it.title), h("div", { class: "actions" }, h("button", { class: "allow", onclick: () => void answer(id, "allow") }, "Allow"), h("button", { class: "always", onclick: () => void answer(id, "always") }, "Always"), h("button", { class: "deny", onclick: () => void answer(id, "deny") }, "Deny")));
        case "plan":
            return h("div", { class: "tool" }, "☰ plan", ...it.steps.map((p) => h("div", {}, `${p.status === "completed" ? "✓" : p.status === "in_progress" ? "▸" : "○"} ${p.text}`)));
        case "notice": return h("div", { class: `msg thinking ${it.level}` }, "! ", it.text);
        case "turn_end": {
            const parts = [it.stop];
            if (it.cost_micro_usd != null)
                parts.push(`$${(it.cost_micro_usd / 1e6).toFixed(4)}`);
            if (it.duration_ms != null)
                parts.push(`${(it.duration_ms / 1000).toFixed(1)}s`);
            return h("div", { class: "turn-end" }, `── ${parts.join(" · ")} ──`);
        }
    }
    return null;
}
function paint() {
    if (!shell || shell.id !== app.viewing || !shell.canvas.isConnected)
        return;
    const s = app.screens.get(app.viewing);
    if (s)
        draw(shell.canvas, s, app.fit);
}
function render() {
    if (!token()) {
        root.replaceChildren(pairView());
        return;
    }
    if (conn.state === "rejected") {
        root.replaceChildren(h("div", {}, header("ptyro", false), h("main", { class: "center" }, h("p", {}, conn.reason), h("button", { title: tip("unpair"), onclick: async () => { localStorage.removeItem("ptyro.token"); await idbSet("token", null).catch(() => { }); location.reload(); } }, "Forget this pairing"))));
        return;
    }
    const r = route();
    if (r.view !== "session" || r.id !== app.viewing)
        subscribe(r.view === "session" ? r.id : null);
    const view = withTabs(r.view, r.view === "session" ? sessionView(r.id) : r.view === "inbox" ? inboxView() : views.has(r.view) ? views.get(r.view)(r.id ?? "") : listView());
    if (root.firstChild !== view || root.childNodes.length !== 1)
        root.replaceChildren(view);
    const t = app.toast ? h("div", { class: "toast" }, app.toast) : null;
    for (const old of view.querySelectorAll(":scope > .toast"))
        old.remove();
    if (t)
        view.append(t);
    paint();
}
function subscribe(id) {
    if (app.viewing)
        conn.send({ t: "unsubscribe", session: app.viewing });
    app.viewing = id;
    if (id) {
        app.screens.set(id, new Screen());
        conn.send({ t: "subscribe", session: id, size: null, control: false });
    }
}
function pairView() {
    const input = h("input", { placeholder: "Paste the link from `ptyro pair`", autocomplete: "off", title: tip("pair") });
    return h("main", { class: "center pair" }, h("img", { src: "/icon.svg", width: 72, height: 72, alt: "" }), h("h1", {}, "ptyro"), h("p", {}, "Run ", h("code", {}, "ptyro pair"), " on your computer and scan the QR code with this device."), input, h("button", { onclick: () => {
            const m = input.value.match(/pair=([A-Za-z0-9_\-]+)/) ?? input.value.match(/^(ptp_[A-Za-z0-9_\-]+)$/);
            if (m)
                void claim(m[1]);
        } }, "Sign in"), pairNote ? h("p", {}, pairNote) : null);
}
// ── push notifications ──────────────────────────────────────────────────────────────────────
function keyBytes(b64) {
    const s = atob(b64.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(b64.length / 4) * 4, "="));
    return Uint8Array.from(s, (c) => c.charCodeAt(0));
}
function b64u(buf) {
    if (!buf)
        return "";
    return btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}
async function enablePush() {
    if (!("serviceWorker" in navigator) || !("PushManager" in window)) {
        toast(/iPhone|iPad/.test(navigator.userAgent) ? "Add ptyro to your Home Screen first (Share → Add to Home Screen), then open it from there." : "This browser can't receive push notifications.");
        return;
    }
    if (!isSecureContext) {
        toast("Notifications need HTTPS: open the https:// address `ptyro pair` shows (ptyro serve speaks HTTPS unless it was started with --insecure).");
        return;
    }
    const perm = await Notification.requestPermission();
    if (perm !== "granted") {
        toast("Notifications are blocked for this site.");
        return;
    }
    try {
        const reg = await navigator.serviceWorker.ready;
        const { key } = await conn.request({ t: "push_key" });
        let sub = await reg.pushManager.getSubscription();
        if (sub)
            await sub.unsubscribe();
        sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: keyBytes(key) });
        await conn.request({ t: "push_subscribe", sub: { endpoint: sub.endpoint, p256dh: b64u(sub.getKey("p256dh")), auth: b64u(sub.getKey("auth")) } });
        await conn.request({ t: "push_test" });
        toast("Notifications on — a test is on its way.");
    }
    catch (e) {
        toast(`Notifications: ${e.message}`);
    }
}
// ── wiring ──────────────────────────────────────────────────────────────────────────────────
conn.listeners.add((msg) => {
    if (msg.t === "connected") {
        // A fresh connection (start-up, reconnect, live upgrade) has no subscriptions: renew the
        // live view's, from a clean screen.
        if (app.viewing) {
            app.screens.set(app.viewing, new Screen());
            conn.send({ t: "subscribe", session: app.viewing, size: null, control: false });
        }
        void refresh();
        void loadAll();
        return;
    }
    if (msg.t === "chat") {
        let c = app.chats.get(msg.session);
        if (!c || msg.reset) {
            c = { items: [], draft: "" };
            app.chats.set(msg.session, c);
        }
        for (const it of msg.items) {
            const i = c.items.findIndex((x) => x.id === it.id);
            if (i >= 0)
                c.items[i] = it;
            else
                c.items.push(it);
        }
        c.draft = msg.draft;
        if (msg.session === app.viewing)
            render();
        return;
    }
    if (msg.t === "screen") {
        const f = msg.frame;
        const s = app.screens.get(f.session);
        if (s) {
            s.apply(f);
            if (f.session === app.viewing)
                paint();
        }
        conn.send({ t: "ack", session: f.session, seq: f.seq });
        return;
    }
    if (msg.t === "event") {
        const e = msg.event;
        if (onEvent(e)) {
            render();
            return;
        }
        if (e.info && (e.t === "session_created" || e.t === "session_updated" || e.t === "session_exited" || e.t === "session_restarted")) {
            app.sessions.set(e.info.id, e.info);
            if (e.t === "session_restarted" && e.info.id === app.viewing) {
                app.viewing = null;
            }
            render();
        }
        else if (e.t === "session_removed") {
            app.sessions.delete(e.id);
            app.chats.delete(e.id);
            app.screens.delete(e.id);
            app.drafts.delete(e.id);
            render();
        }
        else if (e.t === "inbox_added") {
            app.inbox.unshift(e.entry);
            render();
        }
    }
});
useSessions(() => app.sessions);
conn.onState = render;
ui.render = render;
ui.toast = toast;
ui.header = header;
window.addEventListener("hashchange", () => { takePairCode(); render(); });
window.addEventListener("resize", paint);
takePairCode();
if ("serviceWorker" in navigator)
    void navigator.serviceWorker.register("/sw.js");
render();
conn.connect();
setInterval(() => { if (route().view !== "session")
    render(); }, 30_000);
