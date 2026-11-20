// The Library (SPEC §25.10), its first part (ROADMAP KB1.7): search what ptyro kept of your work
// (agent sessions, and your chats beside them), the history when nothing is typed, a hit with
// its neighbours, forgetting, and how the knowledge base stands. KB1.9 builds the rest on this
// page: the filters, the session reader, the tabs and the phone's two steps.
import { conn, host } from "./conn.js";
import { Cache, actions, ago, ask, badge, confirm, errText, facts, h, pending, poll, row, sheet, ui, views } from "./kit.js";
const ACTIONS = {
    "kb.open": "The Library: everything ptyro kept of your work, searchable in one place (More → Library)",
    "kb.search": "Search what was kept, and your chats: words (all must match), \"a phrase\", prefix*, -word, kind:session, project:NAME, since:7d, before:30d, agent:codex, by:meaning",
    "kb.recent": "The history: what was kept, newest first, by day (shown while nothing is typed)",
    "kb.show": "Open a hit: the piece that matched, with what was said around it",
    "kb.forget": "Forget it: its text is deleted for good, and nothing of it is kept again (asks first)",
    "kb.status": "How the knowledge base stands: on or off, what it holds, its size, how long sessions are kept, what was not kept and why",
    "kb.enable": "Switch the knowledge base on: agent sessions are kept from then on",
};
const tip = actions(ACTIONS);
const state = {
    query: "",
    hits: [],
    /// The query the hits answer (`null`: none asked yet).
    hitsFor: null,
    /// Why a search was by words alone, or why it couldn't be made.
    note: "",
};
/// How the knowledge base stands, and its history: asked for once the connection is open
/// (a request made before that is refused at once), and again now and then while the page is.
const home = new Cache(async () => {
    const status = (await conn.request({ t: "kb_status" })).status;
    const recent = status.enabled && !status.error ? (await conn.request({ t: "kb_recent", limit: 50 })).items : [];
    return { status, recent };
});
const plural = (n, word) => `${n} ${word}${n === 1 ? "" : "s"}`;
const bytes = (n) => n >= 1 << 20 ? `${(n / (1 << 20)).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`;
const when = (i) => i.ended_ms ?? i.started_ms ?? i.ingested_ms ?? 0;
const day = (ms) => new Date(ms).toLocaleDateString(undefined, { weekday: "short", year: "numeric", month: "short", day: "numeric" });
const about = (...parts) => parts.filter(Boolean).join(" · ");
/// A snippet with its matches marked (control characters from the daemon) as <mark>.
function snippet(text) {
    const el = h("span", { class: "snippet" });
    let on = false;
    for (const part of text.replace(/\n/g, " ").split(/[\u0002\u0003]/)) {
        if (part)
            el.append(on ? h("mark", {}, part) : document.createTextNode(part));
        on = !on;
    }
    return el;
}
let timer = 0;
const findBox = h("input", { class: "search", type: "search", placeholder: "Search what was kept…", "aria-label": "Search the Library", title: tip("kb.search"), autocomplete: "off", spellcheck: false });
findBox.addEventListener("input", () => {
    state.query = findBox.value.trim();
    clearTimeout(timer);
    timer = window.setTimeout(() => void search(state.query), 250);
});
async function search(q) {
    // From 3 letters on (a shorter prefix matches everything).
    if ([...q].length < 3) {
        state.hits = [];
        state.hitsFor = null;
        state.note = "";
        ui.render();
        return;
    }
    try {
        const r = await conn.request({ t: "kb_search", query: q, limit: 30 });
        if (findBox.value.trim() !== q)
            return;
        state.hits = r.hits;
        state.note = r.words_only ?? "";
    }
    catch (e) {
        state.hits = [];
        state.note = errText(e);
    }
    state.hitsFor = q;
    ui.render();
}
function hitRows() {
    if (state.hitsFor === null)
        return [h("p", { class: "sub" }, "Searching…")];
    const note = state.note ? h("p", { class: "sub" }, state.note) : null;
    if (!state.hits.length)
        return [note ?? h("p", { class: "sub" }, "Nothing matches.")];
    return [...state.hits.map((x) => {
            const sub = h("span", {}, x.how === "meaning" ? h("span", { title: "Found by meaning: none of the words is in it" }, "≈ ") : null, snippet(x.snippet));
            const title = h("span", {}, badge(x.kind), " ", x.title ?? x.addr, h("small", {}, ` ${about(x.project_name, x.agent, `${ago(x.at_ms)} ago`)}`));
            // A chat's hit opens the chat at its message; anything else, the piece with its neighbours.
            return x.kind === "chat"
                ? row(title, sub, null, { href: `#/c/${encodeURIComponent(x.item)}${x.node ? `/${encodeURIComponent(x.node)}` : ""}` })
                : h("div", { title: tip("kb.show") }, row(title, sub, null, { onclick: () => void show(x.addr) }));
        }), note];
}
function recentRows(items) {
    if (!items.length)
        return [h("p", { class: "empty" }, "Nothing is kept yet. Agent sessions are, as they happen: what you asked, what the agent answered and what it ran.")];
    const out = [];
    let last = "";
    for (const i of items) {
        const d = day(when(i.item));
        if (d !== last) {
            out.push(h("h3", { class: "day" }, d));
            last = d;
        }
        out.push(h("div", { title: tip("kb.show") }, row(h("span", {}, badge(i.item.kind), " ", i.item.title ?? i.item.rel ?? i.item.uri), about(i.project_name, i.item.agent, plural(i.item.chunks, "piece"), `${ago(when(i.item))} ago`), null, { onclick: () => void show(i.addr) })));
    }
    return out;
}
/// A hit with its neighbours, or an item from its start.
async function show(addr) {
    const r = await ask({ t: "kb_read", addr });
    if (!r)
        return;
    const i = r.item;
    const s = sheet(i.item.title ?? i.item.uri, [
        h("p", { class: "sub" }, about(i.item.kind, i.project_name, i.item.agent, plural(i.item.chunks, "piece"), new Date(when(i.item)).toLocaleString())),
        r.stale ? h("p", { class: "sub" }, "It was read again since: that piece is gone, so here it is from its start.") : null,
        ...r.chunks.map((c) => h("div", { class: c.idx === r.at ? "kb-piece at" : "kb-piece" }, h("small", {}, about(`${i.addr}/${c.idx}`, c.kind, c.heading)), h("pre", { class: "block" }, c.text))),
        addr.includes("/") ? h("p", {}, h("button", { class: "small", title: tip("kb.show"), onclick: () => { s.close(); void show(i.addr); } }, "All of it")) : null,
    ], conn.scope === "full" ? [h("button", { class: "deny", title: tip("kb.forget"), onclick: () => forget(i, () => s.close()) }, "Forget…")] : []);
}
/// Forget an item, asked first in the words every client uses.
function forget(i, then) {
    confirm(`Forget 1 item: ${i.item.kind} “${i.item.title ?? i.item.uri}”?`, "The text is deleted from the knowledge base for good, and nothing of it is kept again. Your files and the agents' own transcripts aren't touched.", "Forget", async () => { if (await ask({ t: "kb_forget", addrs: [i.item.id], confirmed: true }, "Forgotten")) {
        then();
        home.refresh("");
        void search(state.query);
    } });
}
function showStatus(s) {
    const pieces = s.kinds.reduce((n, k) => n + k.chunks, 0);
    sheet("The knowledge base", [facts([
            ["It is", s.enabled ? "on" : "off"],
            ["Kept", s.kinds.length ? s.kinds.map((k) => `${plural(k.items, k.kind)} (${plural(k.chunks, "piece")})`).join(", ") : "nothing yet"],
            ["In", `${s.path} (${bytes(s.bytes)})`],
            ["By meaning", s.vectors ? `${s.vectors} of ${plural(pieces, "piece")}` : null],
            ["Meaning search", s.meaning ?? null],
            ["Agent sessions", s.sessions ? null : "not kept (Knowledge → sessions is off)"],
            ["Sessions are kept", s.retain_days ? `${plural(s.retain_days, "day")}, then deleted` : "for good"],
            ["Forgotten", s.forgotten ? `${s.forgotten}: never kept again` : null],
            ["Waiting to be read", s.queued ? String(s.queued) : null],
        ]),
        s.skipped.length ? h("h3", {}, `Not kept (${s.skipped.length})`) : null,
        ...[...s.skipped].reverse().map((k) => h("p", { class: "sub" }, `${k.uri}: ${k.why} (${ago(k.at_ms)} ago)`))]);
}
async function enable() {
    try {
        await host("settings.apply", { scope: "user", change: { t: "set", key: "kb.enabled", value: "true" } });
        ui.toast("The knowledge base is on: agent sessions are kept from here on");
        home.reload("");
    }
    catch (e) {
        ui.toast(errText(e));
    }
}
function libraryView() {
    const wait = pending(home, "");
    const s = home.get("")?.status;
    // (Things are kept while the page is open.)
    poll("library", () => home.refresh(""), 15_000);
    let body;
    if (wait || !s)
        body = [wait];
    else if (!s.enabled)
        body = [h("div", { class: "empty" }, h("p", {}, "The knowledge base is off: nothing is kept and nothing is searched. What was kept before stays on the computer."), conn.scope === "full" ? h("button", { class: "primary", title: tip("kb.enable"), onclick: () => void enable() }, "Switch it on")
                : h("p", { class: "sub" }, "Switching it on needs a device paired with ", h("code", {}, "ptyro pair --scope full"), " (Settings → Knowledge on the computer)."))];
    else if (s.error)
        body = [h("p", { class: "banner warn" }, s.error)];
    else {
        const kept = s.kinds.length ? s.kinds.map((k) => plural(k.items, k.kind)).join(", ") : "nothing kept yet";
        body = [
            findBox,
            h("p", { class: "sub" }, h("button", { class: "small", title: tip("kb.status"), onclick: () => showStatus(s) }, `${kept} · ${bytes(s.bytes)}`)),
            h("div", { class: "rows", title: tip([...state.query].length >= 3 ? "kb.search" : "kb.recent") }, ...([...state.query].length >= 3 ? hitRows() : recentRows(home.get("")?.recent ?? []))),
        ];
    }
    return h("div", {}, ui.header("Library", "#/more"), h("main", { class: "stack", title: tip("kb.open") }, ...body));
}
// On or off, how long things are kept: as the config now says, whoever changed it.
conn.listeners.add((msg) => { if (msg.t === "event" && msg.event.t === "config_reloaded" && location.hash.startsWith("#/library"))
    home.refresh(""); });
views.set("library", () => libraryView());
