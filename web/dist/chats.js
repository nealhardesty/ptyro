// Chats (SPEC §22.12, ROADMAP CHAT1.8): plain conversations with your agents, kept by ptyro.
// `#/chats` lists them (pinned first, then by day, searchable, one workspace's or all), `#/c/<id>`
// is one chat (its messages, tool calls and permission cards as they stream, a composer), and
// `#/c/new` starts one. Every control is an action below, as in the TUI and the CLI.
import { conn } from "./conn.js";
import { data, machineOf, wsLabel } from "./data.js";
import { machineChip } from "./machines.js";
import { actions, ago, ask, confirm, download, errText, form, h, menu, patch, ui, views } from "./kit.js";
const ACTIONS = {
    "chats": "Your chats: pinned first, then the newest; search them, or start one",
    "chat.new": "A new chat with your agent: type, then send (it can search the web, and it is kept)",
    "chat.open": "Open a chat",
    "chat.send": "Send the message; the answer streams in (Enter sends, Shift+Enter starts a new line)",
    "chat.answer": "Answer what the chat's agent asks: allow once, always, or deny",
    "chat.stop": "Stop the answer being written",
    "chat.retitle": "Change the chat's title (empty: a new automatic one)",
    "chat.pin": "Pin the chat to the top, or unpin it",
    "chat.archive": "Archive the chat (Archived shows them; again brings it back)",
    "chat.delete": "Delete the chat",
    "chat.agent": "Switch the chat to another agent: the next message carries the conversation over",
    "chat.preset": "What the chat may use: web search, read folders, or no tools",
    "chat.export": "Download the chat as Markdown",
    "chat.find": "Find a chat by its title or agent",
    "chat.archived": "Show archived chats instead",
};
const tip = actions(ACTIONS);
const state = {
    list: new Map(),
    archived: null,
    loaded: false,
    /// `[chat] enabled = false`: the daemon says why, and the Chats tab goes.
    off: "",
    query: "",
    workspace: "",
    /// The chat followed now, and its items.
    following: null,
    items: [],
};
const label = (c) => c?.title?.trim() || "new chat";
const addr = (c) => `c:${c.handle}`;
const working = (c) => c.status === "working" || c.status === "starting";
const glyph = (c) => c.attention || c.status === "blocked" ? "⚠" : working(c) ? "●" : c.status === "errored" ? "✗" : c.pinned ? "★" : "·";
/// Whether the Chats tab shows (chats are on, as far as the daemon has said).
export function chatsShown() {
    return !state.off;
}
export async function loadChats() {
    try {
        const r = await conn.request({ t: "chat_list", all: true });
        state.list = new Map(r.chats.map((c) => [c.id, c]));
        state.off = "";
    }
    catch (e) {
        state.off = errText(e);
    }
    state.loaded = true;
    ui.render();
}
function sorted(list) {
    return [...list].sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.last_ms - a.last_ms);
}
function dayOf(ms) {
    const day = 86_400_000;
    const today = new Date();
    today.setHours(0, 0, 0, 0);
    const t = today.getTime();
    if (ms >= t)
        return "Today";
    if (ms >= t - day)
        return "Yesterday";
    if (ms >= t - 6 * day)
        return "This week";
    return "Earlier";
}
// ── the list ────────────────────────────────────────────────────────────────────────────────
const findBox = h("input", { class: "search", type: "search", placeholder: "Find a chat", "aria-label": "Find a chat", autocapitalize: "off", autocomplete: "off", title: tip("chat.find") });
findBox.addEventListener("input", () => { state.query = findBox.value.trim().toLowerCase(); const l = document.querySelector("main.chats .rows"); if (l)
    patch(l, rows()); });
function rows() {
    const source = state.archived ?? sorted(state.list.values());
    const q = state.query;
    const shown = source.filter((c) => (!state.workspace || c.workspace === state.workspace) && (!q || label(c).toLowerCase().includes(q) || c.agent.includes(q)));
    if (!shown.length) {
        return [h("div", { class: "empty" }, h("p", {}, q ? `Nothing matches “${q}”.` : state.archived ? "No archived chats." : "No chats yet. Ask anything: it can search the web, and it is kept here."), q || state.archived ? null : h("button", { class: "primary", title: tip("chat.new"), onclick: () => newChat() }, "Start a chat"))];
    }
    const out = [];
    let group = "";
    for (const c of shown) {
        const g = c.pinned && !state.archived ? "Pinned" : dayOf(c.last_ms);
        if (g !== group) {
            out.push(h("h3", { class: "day" }, g));
            group = g;
        }
        // Another machine's that is down: listed as it was, greyed (SPEC §22.11).
        const down = !!c.machine && machineOf(c.workspace)?.state !== "online";
        out.push(h("div", { class: `chat-row${c.unread ? " unread" : ""}${c.attention ? " blocked" : ""}${down ? " offline" : ""}` }, h("a", { class: "main", href: `#/c/${encodeURIComponent(c.id)}`, title: tip("chat.open") }, h("span", { class: "glyph" }, glyph(c)), h("span", { class: "title" }, label(c)), h("small", {}, ` ${c.agent} · ${ago(c.last_ms)}${state.workspace ? "" : ` · ${wsLabel(c.workspace)}`}`), c.machine ? machineChip(c.workspace) : null, c.unread ? h("b", { class: "unread-dot", "aria-label": "unread" }, "●") : null), h("button", { class: "icon", "aria-label": "More", onclick: () => chatMenu(c) }, "⋯")));
    }
    return out;
}
function listView() {
    if (!state.loaded)
        void loadChats();
    if (state.off) {
        return h("div", {}, ui.header("Chats", false), h("main", { class: "center" }, h("p", {}, state.off)));
    }
    if (findBox.value !== state.query && document.activeElement !== findBox)
        findBox.value = state.query;
    const wsSelect = h("select", { "aria-label": "Workspace", onchange: (e) => { state.workspace = e.target.value; ui.render(); } }, h("option", { value: "" }, "Every workspace"), ...[...data.workspaces.values()].map((w) => h("option", { value: w.name }, wsLabel(w.name))));
    wsSelect.value = state.workspace;
    const archived = h("button", { class: `small${state.archived ? " on" : ""}`, title: tip("chat.archived"), onclick: () => void toggleArchived() }, state.archived ? "Back to chats" : "Archived");
    return h("div", {}, ui.header("Chats", false, h("button", { class: "icon plus", title: tip("chat.new"), "aria-label": "New chat", onclick: () => newChat() }, "＋")), h("div", { class: "searchbar" }, findBox, wsSelect, archived), h("main", { class: "list chats", title: tip("chats") }, h("div", { class: "rows" }, ...rows())));
}
async function toggleArchived() {
    if (state.archived) {
        state.archived = null;
        ui.render();
        return;
    }
    const r = await ask({ t: "chat_list", all: true, archived: true });
    state.archived = r ? sorted(r.chats) : [];
    ui.render();
}
function newChat() {
    location.hash = `#/c/new${state.workspace ? `/${encodeURIComponent(state.workspace)}` : ""}`;
}
function update(c, change, ok) {
    return ask({ t: "chat_update", chat: c.id, ...change }, ok);
}
function chatMenu(c) {
    const archived = c.archived || !!state.archived?.some((x) => x.id === c.id);
    menu(label(c), [
        { title: tip("chat.open"), label: "Open", run: () => { location.hash = `#/c/${encodeURIComponent(c.id)}`; } },
        { title: tip("chat.retitle"), label: "Rename", run: () => retitle(c) },
        { title: tip("chat.pin"), label: c.pinned ? "Unpin" : "Pin", run: () => update(c, { pinned: !c.pinned }, c.pinned ? "Unpinned" : "Pinned") },
        { title: tip("chat.agent"), label: "Switch agent", hint: `now ${c.agent}`, run: () => pickAgent(c) },
        { title: tip("chat.preset"), label: "Tools", hint: `now ${c.tools}`, run: () => pickPreset(c) },
        { title: tip("chat.export"), label: "Download as Markdown", run: () => void exportChat(c) },
        { title: tip("chat.archive"), label: archived ? "Bring back from the archive" : "Archive", run: async () => {
                await update(c, { archived: !archived }, archived ? "Back from the archive" : "Archived");
                if (state.archived)
                    state.archived = state.archived.filter((x) => x.id !== c.id);
                if (state.following === c.id && !archived)
                    location.hash = "#/chats";
                ui.render();
            } },
        { title: tip("chat.delete"), label: "Delete", danger: true, run: () => confirm(`Delete “${label(c)}”?`, "It goes from every list, on every device.", "Delete", async () => {
                await ask({ t: "chat_delete", chat: c.id }, "Deleted");
                if (state.archived)
                    state.archived = state.archived.filter((x) => x.id !== c.id);
                if (state.following === c.id)
                    location.hash = "#/chats";
            }) },
    ]);
}
function retitle(c) {
    form("Rename the chat", [{ key: "title", label: "Title (empty: a new automatic one)", value: c.title ?? "" }], "Save", async (v) => {
        await conn.request({ t: "chat_update", chat: c.id, title: v.title ?? "" });
        ui.toast("Renamed");
    });
}
function agentNames() {
    const names = ["claude", "codex", "gemini", "opencode"];
    for (const a of data.agents)
        if (!names.includes(a.name))
            names.push(a.name);
    // Antigravity has no structured mode, so it can't chat (SPEC §22.5).
    return names.filter((n) => n !== "antigravity" && (data.agents.length === 0 || data.agents.some((a) => a.name === n && a.installed)));
}
function pickAgent(c) {
    menu("Which agent? The conversation carries over", agentNames().map((a) => ({ title: tip("chat.agent"), label: a, hint: a === c.agent ? "now" : "", run: () => update(c, { agent: a }, `The next message goes to ${a}`) })));
}
const PRESETS = [["chat", "web search"], ["read", "web search, and reads its folders"], ["none", "no tools"]];
function pickPreset(c) {
    menu("What may the chat use?", PRESETS.map(([p, what]) => ({ title: tip("chat.preset"), label: p, hint: p === c.tools ? `now · ${what}` : what, run: () => update(c, { preset: p }, `Tools: ${p}`) })));
}
async function exportChat(c) {
    const r = await ask({ t: "chat_export", chat: c.id, format: "md" });
    if (r)
        download(`${label(c).replace(/[^\w.-]+/g, "-").slice(0, 60) || c.handle}.md`, r.text, "text/markdown");
}
let page = null;
const drafts = new Map();
function canWrite() {
    return conn.scope === "control" || conn.scope === "full";
}
function follow(id) {
    if (state.following === id)
        return;
    if (state.following)
        conn.send({ t: "chat_unsubscribe", chat: state.following });
    state.following = id;
    state.items = [];
    if (id)
        conn.send({ t: "chat_subscribe", chat: id });
}
window.addEventListener("hashchange", () => { if (!/^#\/c\//.test(location.hash))
    follow(null); });
function pageFor(id) {
    if (page?.id === id)
        return page;
    const reply = h("textarea", { rows: 2, enterkeyhint: "send", placeholder: "Ask anything…", title: tip("chat.send") });
    reply.value = drafts.get(id) ?? "";
    reply.addEventListener("input", () => drafts.set(id, reply.value));
    reply.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
        e.preventDefault();
        void send();
    } });
    const agent = h("select", { "aria-label": "Agent", title: tip("chat.agent") }, h("option", { value: "" }, "default agent"), ...agentNames().map((a) => h("option", { value: a }, a)));
    const preset = h("select", { "aria-label": "Tools", title: tip("chat.preset") }, ...PRESETS.map(([p, what]) => h("option", { value: p }, `${p}: ${what}`)));
    page = { id, el: h("div", { class: "session chat-page" }), box: h("div", { class: "chat" }), reply, els: new Map(), agent, preset };
    return page;
}
async function send() {
    const p = page;
    if (!p)
        return;
    const text = p.reply.value;
    if (!text.trim())
        return;
    try {
        if (p.id.startsWith("new")) {
            const ws = decodeURIComponent(p.id.split("/")[1] ?? "") || undefined;
            state.items = [{ id: "first", t: "user", text }];
            ui.render();
            const r = await conn.request({ t: "chat_new", workspace: ws, agent: p.agent.value || null, preset: p.preset.value || null, prompt: text });
            p.reply.value = "";
            drafts.delete(p.id);
            const info = r.info;
            state.list.set(info.id, info);
            // The same page, now the chat's own.
            location.replace(`#/c/${encodeURIComponent(info.id)}`);
        }
        else {
            await conn.request({ t: "chat_send", chat: p.id, text });
            if (p.reply.value === text) {
                p.reply.value = "";
                drafts.delete(p.id);
            }
        }
    }
    catch (e) {
        ui.toast(errText(e));
    }
}
function chatItem(id, it) {
    switch (it.t) {
        case "user": return h("div", { class: "msg user" }, it.text);
        case "assistant": return h("div", { class: "msg assistant" }, it.text, it.streaming ? h("span", { class: "caret" }, "▍") : null);
        case "thinking": return h("div", { class: "msg thinking" }, "· ", it.text.split("\n")[0]);
        case "tool": {
            const status = it.status === "done" ? "done" : it.status === "failed" ? "failed" : it.status === "declined" ? "declined" : "running";
            const det = h("details", { class: `tool ${status}` }, h("summary", {}, `${it.name} · ${it.title}`));
            if (it.output)
                det.append(h("pre", {}, it.output));
            return det;
        }
        case "permission":
            if (it.decided)
                return h("div", { class: "turn-end" }, `${it.title.split("\n")[0] || it.tool} → ${it.decided}`);
            return h("section", { class: "prompt" }, h("b", {}, `Wants permission: ${it.tool}`), h("pre", {}, it.title), canWrite() ? h("div", { class: "actions", title: tip("chat.answer") }, ...["allow", "always", "deny"].map((d) => h("button", { class: d, onclick: () => void ask({ t: "chat_answer", chat: id, decision: d }, d === "deny" ? "Denied" : "Allowed") }, d[0].toUpperCase() + d.slice(1)))) : null);
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
function drawItems(p) {
    const box = p.box;
    const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 32;
    const top = box.scrollTop;
    const els = new Map();
    const kids = [];
    for (const it of state.items) {
        const key = JSON.stringify(it);
        const old = p.els.get(it.id);
        const el = old?.key === key ? old.el : chatItem(p.id, it);
        if (!el)
            continue;
        if (old && old.el !== el && old.el instanceof HTMLDetailsElement && el instanceof HTMLDetailsElement)
            el.open = old.el.open;
        els.set(it.id, { key, el });
        kids.push(el);
    }
    p.els = els;
    patch(box, kids);
    if (atBottom)
        requestAnimationFrame(() => { box.scrollTop = box.scrollHeight; });
    else
        box.scrollTop = top;
}
function chatView(rest) {
    if (!state.loaded)
        void loadChats();
    const isNew = rest === "new" || rest.startsWith("new/");
    const id = isNew ? `new${rest.slice(3)}` : decodeURIComponent(rest);
    const p = pageFor(id);
    // A new chat follows nothing yet (and shows nothing of the one left).
    follow(isNew ? null : id);
    const c = isNew ? undefined : state.list.get(id) ?? state.archived?.find((x) => x.id === id);
    const pill = c
        ? [`${c.agent}@${c.profile}`, ` · tools: ${c.tools}`, c.status ? ` · ${c.status}` : "", c.cost_micro_usd ? ` · $${(c.cost_micro_usd / 1e6).toFixed(3)}` : "", ` · ${addr(c)}`]
        : ["A new chat: it can search the web, and it is kept."];
    const empty = isNew && !state.items.length
        ? h("div", { class: "empty" }, h("p", {}, h("b", {}, "Ask anything.")), h("p", { class: "sub" }, "Pick the agent and what it may use below, or keep the defaults."))
        : null;
    if (!isNew || state.items.length)
        drawItems(p);
    const stop = c && working(c) && canWrite() ? h("button", { class: "small", title: tip("chat.stop"), onclick: () => void ask({ t: "chat_stop", chat: c.id }, "Stopped") }, "Stop") : null;
    patch(p.el, [
        ui.header(c ? label(c) : "New chat", "#/chats", c ? h("button", { class: "icon", "aria-label": "More", onclick: () => chatMenu(c) }, "⋯") : null),
        h("div", { class: `pill ${c?.status ?? ""}` }, ...pill, c?.machine ? " · " : null, c?.machine ? machineChip(c.workspace) : null),
        ...(c?.attention ? [h("p", { class: "banner warn" }, c.attention.text)] : []),
        empty ?? p.box,
        ...(isNew ? [h("div", { class: "bar chat-pick" }, p.agent, p.preset)] : []),
        canWrite() ? h("div", { class: "reply" }, p.reply, stop, h("button", { class: "send", title: tip("chat.send"), onclick: () => void send() }, "Send")) : h("p", { class: "sub" }, "This device may read chats, not write in them."),
    ].filter((x) => !!x));
    if (isNew)
        requestAnimationFrame(() => { if (document.activeElement !== p.reply)
            p.reply.focus(); });
    return p.el;
}
// ── wiring ──────────────────────────────────────────────────────────────────────────────────
conn.listeners.add((msg) => {
    if (msg.t === "connected") {
        void loadChats();
        // A fresh connection follows nothing: follow the open chat again.
        if (state.following) {
            const f = state.following;
            state.following = null;
            follow(f);
        }
        return;
    }
    if (msg.t === "chat_live") {
        if (msg.chat !== state.following)
            return;
        if (msg.reset)
            state.items = [];
        for (const it of msg.items) {
            const i = state.items.findIndex((x) => x.id === it.id);
            if (i >= 0)
                state.items[i] = it;
            else
                state.items.push(it);
        }
        ui.render();
        return;
    }
    if (msg.t === "event") {
        const e = msg.event;
        if (e.t === "chat_updated") {
            const c = e.info;
            if (c.archived)
                state.list.delete(c.id);
            else
                state.list.set(c.id, c);
            ui.render();
        }
        else if (e.t === "chat_removed") {
            state.list.delete(e.id);
            ui.render();
        }
    }
});
views.set("chats", () => listView());
views.set("c", (rest) => chatView(rest));
