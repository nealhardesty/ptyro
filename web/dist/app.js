// ptyro web app (SPEC §16.4, M9.3): sessions, a live terminal view drawn from the
// daemon's screen frames, quick replies, and answers to permission prompts. Talks the ptyro
// protocol (JSON encoding) over `ptyro serve`'s WebSocket, signed in with a device token from
// `ptyro pair`. No dependencies, no third-party requests.
import { Screen, modelLabel } from "./screen.js";
import { conn, store, token } from "./conn.js";
import { Term } from "./term.js";
import { actions, ago, h, patch, sheet, statusWord, tipOf, ui, views } from "./kit.js";
import { can, data, loadAll, machineOf, onEvent, splitMachine } from "./data.js";
import { staleMaybeChanged } from "./projects.js";
import "./settings.js";
import "./panel.js";
import { chatsAside, chatsShown, findAllChats, newChat } from "./chats.js";
import { machineChip } from "./machines.js";
import { broadcast, marked, newTask, sessionMenu, useSessions } from "./sessions.js";
import { offerSetup, setupBanner, setupConnected, setupEvent } from "./settings.js";
import { goShell, listWorkspace, shellStrip, shellsGroup } from "./shells.js";
import { apply as showMachine, offlineText } from "./machine.js";
// What this app does, by name: the Web column of docs/parity.md names these, and `make parity`
// checks every name it uses is here (SPEC §1.3, ROADMAP WEB1). Each is its control's tooltip.
const ACTIONS = {
    "sessions": "Every session: what it's doing, and who needs you first",
    "session.open": "A session's live screen",
    "session.reply": "Type to the session; Enter sends, Shift+Enter starts a new line",
    "answer": "Answer what it asks: allow once, always, or deny",
    "host.load": "The load average of the computer ptyro runs on, over 1, 5 and 15 minutes: amber once every CPU is busy, red at twice that ([ui] load)",
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
        // (With a pairing already in place the pairing screen isn't showing: say it here.)
        if (store.get("ptyro.token"))
            toast(pairNote);
    }
    render();
}
/// Store a device token and sign in with it (also from a tab that was open, unpaired).
function useToken(t) {
    store.set("ptyro.token", t);
    idbSet("token", t).catch(() => { });
    // A new token may mean new rights (a pairing again for a wider scope): sign in afresh.
    conn.reset();
    conn.connect();
}
const GLYPH = { working: "●", blocked: "⚠", idle: "○", done: "✓", errored: "✗", starting: "◌", unknown: "·" };
function statusOf(s) {
    if (s.state.t === "exited" || s.state.t === "failed")
        return "exited";
    if (s.state.t === "dormant")
        return "dormant";
    return s.status?.status ?? (s.kind === "agent" ? "unknown" : "shell");
}
const SHOWS = ["all", "running", "active"];
const isRunning = (s) => !s.quick && (s.state.t === "running" || s.state.t === "starting");
const isActive = (s) => isRunning(s) && (s.status ? ["working", "blocked", "starting"].includes(s.status.status) : !!s.command && s.command.ended_ms == null);
const KEEPS = { all: (s) => !s.quick, running: isRunning, active: isActive };
let show = (() => { const v = store.get("ptyro.show"); return SHOWS.find((x) => x === v) ?? "all"; })();
// ── app state and views ─────────────────────────────────────────────────────────────────────
const app = {
    sessions: new Map(),
    screens: new Map(),
    /// Unsent replies, per session.
    drafts: new Map(),
    viewing: null,
    toast: "",
};
const root = document.getElementById("app");
/// `#/s/<id>` a session, `#/<name>/<rest>` a registered view (kit.ts), else the list.
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
    const v = location.hash.match(/^#\/([a-z]+)(?:\/(.*))?$/);
    if (v && views.has(v[1]))
        return { view: v[1], id: v[2] ?? "" };
    return { view: "list" };
}
/// The tabs along the bottom of a top-level page; the active one is the page's.
const TABS = [["list", "#/", "Sessions"], ["chats", "#/chats", "Chats"], ["p", "#/p", "Projects"], ["more", "#/more", "More"]];
function withTabs(view, page) {
    // Chats switched off (`[chat] enabled = false`) have no tab.
    const tabs = TABS.filter(([v]) => v !== "chats" || chatsShown());
    if (!tabs.some(([v]) => v === view))
        return page;
    page.classList.add("tabbed");
    page.append(h("nav", { class: "tabs" }, ...tabs.map(([v, href, label]) => h("a", { href, class: v === view ? "on" : "" }, label))));
    return page;
}
async function refresh() {
    try {
        const r = await conn.request({ t: "list" });
        // A chat's engine belongs to its chat, not to the session list (SPEC §22.2).
        app.sessions = new Map(r.sessions.filter((s) => !s.chat).map((s) => [s.id, s]));
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
let hostLoad = null;
const loadText = (l) => l.avg.map((a) => (a / 100).toFixed(2)).join(" ");
const loadLevel = (l) => l.avg[0] >= 200 * Math.max(1, l.cpus) ? "bad" : l.avg[0] >= 100 * Math.max(1, l.cpus) ? "warn" : "";
const machineTitle = () => [conn.machine ? `This ptyro runs on ${conn.machine}` : "", hostLoad ? `load average ${loadText(hostLoad)} (1, 5, 15 minutes) on ${hostLoad.cpus} CPUs` : ""].filter(Boolean).join("; ");
const loadSpan = () => hostLoad ? h("span", { class: `load ${loadLevel(hostLoad)}`, title: tip("host.load") }, `${conn.machine ? " · " : ""}${loadText(hostLoad)}`) : null;
function setHostLoad(l) {
    const had = !!hostLoad;
    hostLoad = l;
    // Shown or hidden: the header changes shape. Else only the numbers.
    if (had !== !!l) {
        render();
        return;
    }
    for (const chip of document.querySelectorAll("header .machine")) {
        chip.title = machineTitle();
        chip.querySelector(".load")?.replaceWith(loadSpan() ?? "");
    }
}
/// `back`: true goes to the session list; a string is where it goes instead.
function header(title, back, ...extra) {
    const dot = h("span", { class: `dot ${conn.state}`, title: conn.state });
    return h("header", {}, back ? h("a", { class: "back", href: typeof back === "string" ? back : "#/", "aria-label": "Back" }, "‹") : h("img", { class: "logo", src: "/icon.svg", alt: "" }), h("h1", {}, title), ...extra, conn.machine || hostLoad ? h("span", { class: "machine", title: machineTitle() }, conn.machine, loadSpan()) : null, dot, h("button", { class: "icon", title: tip("notifications"), onclick: () => void enablePush() }, "🔔"));
}
function sessionCard(s) {
    const st = statusOf(s);
    const detail = s.status?.status === "blocked" ? s.status.detail ?? s.attention?.text : s.activity ?? s.status?.detail ?? s.title;
    const card = h("a", { class: `card ${st}${selecting && marked.has(s.id) ? " picked" : ""}`, href: selecting ? "#/" : `#/s/${encodeURIComponent(s.id)}`, title: tip("session.open"),
        onclick: selecting ? (e) => { e.preventDefault(); if (!marked.delete(s.id))
            marked.add(s.id); render(); } : undefined }, h("span", { class: "glyph" }, selecting ? (marked.has(s.id) ? "☑" : "☐") : GLYPH[st] ?? "·"), h("div", { class: "main" }, 
    // Another machine's (MM1.B): its name without the machine, and the machine's chip.
    h("div", { class: "name" }, s.machine ? splitMachine(s.name)[1] : s.name, s.agent ? h("small", {}, ` ${s.agent}`, s.model ? ` · ${modelLabel(s.model)}` : "") : null, s.machine ? " " : null, machineChip(s.name)), h("div", { class: "sub" }, [statusWord(st), s.project, s.worktree].filter(Boolean).join(" · ")), detail ? h("div", { class: "detail" }, detail) : null), h("time", {}, s.status ? ago(s.status.since_ms) : ago(s.created_ms)));
    if (s.status?.status === "blocked" && s.state.t === "running") {
        return h("div", { class: "blocked-wrap" }, card, h("div", { class: "actions", title: tip("answer") }, h("button", { class: "allow", onclick: () => void answer(s.id, "allow") }, "Allow"), h("button", { class: "deny", onclick: () => void answer(s.id, "deny") }, "Deny")));
    }
    return card;
}
/// Marking sessions to type into several at once (`broadcast`).
let selecting = false;
/// The search box above the list (`session.find`): one element that stays, so typing keeps its
/// focus while the cards under it are replaced.
let query = "";
const searchBox = h("input", { class: "search", type: "search", placeholder: "Find a session", "aria-label": "Find a session", autocapitalize: "off", autocomplete: "off", title: tipOf("session.find") });
const searchBar = h("div", { class: "searchbar" }, searchBox);
let listCards = () => [];
searchBox.addEventListener("input", () => {
    query = searchBox.value.trim().toLowerCase();
    const list = document.querySelector("main.list .cards");
    if (list)
        patch(list, listCards());
});
const matches = (s) => !query || [s.name, s.title, s.agent, s.model && modelLabel(s.model), s.project, s.worktree, s.task, s.activity, statusOf(s)].some((x) => x?.toLowerCase().includes(query));
function setShow(m) {
    show = m;
    store.set("ptyro.show", m);
    render();
}
function listView() {
    const order = ["blocked", "errored", "working", "starting", "done", "idle", "unknown", "shell", "dormant", "exited"];
    const everything = [...app.sessions.values()].filter(KEEPS[show]).sort((a, b) => order.indexOf(statusOf(a)) - order.indexOf(statusOf(b)) || a.name.localeCompare(b.name));
    const all = [...app.sessions.values()].filter(KEEPS.all);
    // SHELLS have their group (SPEC §23.14), so they aren't among the cards twice.
    listCards = () => { const f = everything.filter((s) => !s.loose && matches(s)); return f.length ? f.map(sessionCard) : query ? [h("p", { class: "empty" }, `Nothing matches “${query}”.`)] : []; };
    const body = everything.length ? listCards() : all.length ? [h("div", { class: "empty" }, h("p", {}, `Nothing is ${show} right now.`), h("button", { class: "small", title: tipOf("sidebar.filter"), onclick: () => setShow("all") }, "Show every session"))] : [h("div", { class: "empty" }, h("p", {}, "Nothing is running."), h("button", { class: "primary", title: tipOf("session.new"), onclick: () => newTask() }, "Start a task"), data.projects.size ? null : h("p", {}, h("a", { href: "#/setup", title: tipOf("first.run") }, "Set up ptyro first: your agents, accounts and repositories")))];
    const picked = [...marked].filter((id) => app.sessions.has(id));
    const tools = selecting
        ? h("div", { class: "select-bar" }, h("span", {}, `${picked.length} selected`), h("button", { class: "small primary", disabled: !picked.length, title: tipOf("session.broadcast"), onclick: () => { broadcast(picked); selecting = false; render(); } }, "Type to them"), h("button", { class: "small", onclick: () => { selecting = false; marked.clear(); render(); } }, "Cancel"))
        : null;
    const head = header("ptyro", false, all.length > 1 ? h("button", { class: "icon", title: tipOf("session.broadcast"), onclick: () => { selecting = true; render(); } }, "☑") : null, h("button", { class: "icon plus", title: tipOf("session.new"), "aria-label": "New task", onclick: () => newTask() }, "＋"));
    const counts = SHOWS.map((m) => all.filter(KEEPS[m]).length);
    const filter = all.length ? h("div", { class: "chips filter", title: tipOf("sidebar.filter") }, ...SHOWS.map((m, i) => h("button", { class: `${m === show ? "on" : ""} ${m}`, "aria-pressed": String(m === show), onclick: () => setShow(m) }, `${m} `, h("span", { class: "n" }, String(counts[i]))))) : null;
    // On a wide screen, the CHATS section beside them (the TUI's sidebar has it under each workspace).
    const aside = chatsShown() ? chatsAside() : null;
    const shells = can("full") || [...app.sessions.values()].some((s) => s.loose) ? shellsGroup(app.sessions.values(), sessionCard) : null;
    return h("div", {}, head, tools, filter, all.length > 6 || query ? searchBar : null, h("main", { class: `list${aside ? " with-chats" : ""}`, title: tip("sessions") }, setupBanner(), shells, h("div", { class: "cards" }, ...body), aside));
}
let shell = null;
/// This device may type into the session: `control` scope or more, and it is running.
function canType(id) {
    return (conn.scope === "control" || conn.scope === "full") && app.sessions.get(id)?.state.t === "running";
}
function sessionShell(id) {
    if (shell?.id === id)
        return shell;
    // The terminal of the session left behind: its listeners and its canvas go.
    shell?.term.dispose();
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
    const term = new Term({
        id: () => id,
        screen: () => app.screens.get(id),
        canType: () => canType(id),
        send: (m) => conn.send(m),
        request: (r) => conn.request(r),
        toast,
        changed: render,
    });
    shell = {
        id, reply, term,
        el: h("div", { class: "session" }),
        replyRow: h("div", { class: "reply" }, reply, h("button", { class: "send", onclick: () => void send() }, "Send")),
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
    patch(sh.el, [
        header(s.name, true, h("button", { class: "icon", title: tipOf("session.menu"), "aria-label": "More", onclick: () => sessionMenu(s) }, "⋯")),
        h("div", { class: `pill ${st}` }, GLYPH[st] ?? "·", " ", statusWord(st), s.model && s.kind === "agent" ? ` · ${modelLabel(s.model)}` : "", s.status?.detail && !blocked ? ` · ${s.status.detail}` : "", s.status ? ` · ${ago(s.status.since_ms)}` : "", app.screens.get(id)?.title ? ` · ${app.screens.get(id).title}` : "", s.machine ? " · " : "", machineChip(s.name)),
        // Its machine is down (MM1.B): this is how it was, and what to do.
        ...(machineOf(s.name) && machineOf(s.name).state !== "online" ? [h("p", { class: "banner warn" }, `${machineOf(s.name).name} isn't reachable: this is how it was. `, machineOf(s.name).fix ?? "")] : []),
        ...(blocked ? [h("section", { class: "prompt" }, h("div", {}, h("b", {}, s.status?.kind === "permission" ? "Wants permission" : "Waiting for you"), s.held ? h("span", { class: "tag", title: "answers go to the agent as a decision" }, "exact") : null), h("pre", {}, s.status?.detail ?? s.attention?.text ?? ""), h("div", { class: "actions", title: tip("answer") }, h("button", { class: "allow", onclick: () => void answer(id, "allow") }, "Allow"), h("button", { class: "always", onclick: () => void answer(id, "always") }, "Always"), h("button", { class: "deny", onclick: () => void answer(id, "deny") }, "Deny")))] : []),
        ...(s.state.t === "dormant" ? [h("section", { class: "revive" }, h("span", {}, "Hibernated: its conversation is kept."), h("button", { class: "primary", title: tipOf("session.resume"), onclick: () => void conn.request({ t: "resume", addr: id }).then(() => toast("Resumed"), (e) => toast(e.message)) }, "Resume"))]
            : s.state.t !== "running" ? [h("section", { class: "revive" }, h("span", {}, s.state.t === "failed" ? "It failed to run." : s.state.code != null ? `Exited with ${s.state.code}.` : "Exited."), h("button", { class: "primary", title: tipOf("session.restart"), onclick: () => void conn.request({ t: "restart", addr: id }).then(() => toast("Restarted"), (e) => toast(e.message)) }, "Restart"))] : []),
        // A shell of SHELLS: its workspace's shells by number, tmux's status line (SPEC §23.14).
        s.loose ? shellStrip(app.sessions.values(), s) : null,
        sh.term.el,
        canType(id) ? sh.term.keybar : null,
        h("div", { class: "keys term-more" }, h("button", { title: tipOf("term.options"), onclick: () => sh.term.options() }, "Aa"), canType(id) ? null : h("span", { class: "sub" }, "Watching: this device may look, not type")),
        canType(id) ? sh.replyRow : null,
    ].filter((x) => !!x));
    return sh.el;
}
function paint() {
    if (!shell || shell.id !== app.viewing || !shell.term.canvas.isConnected)
        return;
    shell.term.paint();
}
function render() {
    if (!token()) {
        root.replaceChildren(pairView());
        return;
    }
    if (conn.state === "rejected") {
        root.replaceChildren(h("div", {}, header("ptyro", false), h("main", { class: "center" }, h("p", {}, conn.reason), h("button", { title: tip("unpair"), onclick: () => void forget() }, "Forget this pairing"))));
        return;
    }
    const r = route();
    if (r.view !== "session" || r.id !== app.viewing)
        subscribe(r.view === "session" ? r.id : null);
    const view = withTabs(r.view, r.view === "session" ? sessionView(r.id) : views.has(r.view) ? views.get(r.view)(r.id ?? "") : listView());
    if (conn.state !== "open")
        view.prepend(h("div", { class: "offline", role: "status" }, offlineText(conn.state, location.hostname)));
    if (root.firstChild !== view || root.childNodes.length !== 1)
        root.replaceChildren(view);
    // The number of agents waiting on you, in the tab's title and on an installed app's icon.
    const waiting = [...app.sessions.values()].filter((x) => x.status?.status === "blocked" && x.state.t === "running").length;
    showMachine(conn.machine, waiting);
    navigator[waiting ? "setAppBadge" : "clearAppBadge"]?.(waiting)?.catch(() => { });
    const t = app.toast ? h("div", { class: "toast" }, app.toast) : null;
    for (const old of view.querySelectorAll(":scope > .toast"))
        old.remove();
    if (t)
        view.append(t);
    paint();
}
function subscribe(id) {
    if (app.viewing) {
        if (shell?.id === app.viewing)
            shell.term.release();
        conn.send({ t: "unsubscribe", session: app.viewing });
        // Its screen is no use unsubscribed (a new subscription starts with a keyframe).
        app.screens.delete(app.viewing);
    }
    app.viewing = id;
    if (id) {
        app.screens.set(id, new Screen());
        conn.send({ t: "subscribe", session: id, size: null, control: false });
    }
}
/// Forget this device's pairing (the token here; `ptyro devices revoke` ends it for good).
async function forget() {
    store.remove("ptyro.token");
    await idbSet("token", null).catch(() => { });
    // This device's push subscription goes too: nothing should be sent to a device that forgot.
    try {
        const reg = await Promise.race([navigator.serviceWorker.ready, new Promise((ok) => setTimeout(() => ok(undefined), 1500))]);
        await (await reg?.pushManager.getSubscription())?.unsubscribe();
    }
    catch { /* no service worker, or none to forget */ }
    location.hash = "#/";
    location.reload();
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
        toast("Notifications need HTTPS: open the https:// address `ptyro pair` shows.");
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
            if (shell?.id === app.viewing)
                shell.term.reassert();
        }
        void refresh();
        void loadAll();
        setupConnected();
        conn.request({ t: "status" }).then((r) => setHostLoad(r.status?.load ?? null), () => { });
        return;
    }
    if (msg.t === "screen") {
        const f = msg.frame;
        const s = app.screens.get(f.session);
        if (s) {
            s.apply(f);
            if (f.session === app.viewing && shell?.id === f.session)
                shell.term.onFrame();
        }
        // Acked once drawn, so a slow screen gets fewer, bigger deltas; a hidden tab doesn't wait.
        const ack = () => conn.send({ t: "ack", session: f.session, seq: f.seq });
        if (document.hidden)
            ack();
        else
            requestAnimationFrame(ack);
        return;
    }
    if (msg.t === "event") {
        const e = msg.event;
        if (e.t === "host_load") {
            setHostLoad(e.load ?? null);
            return;
        }
        staleMaybeChanged(e);
        if (onEvent(e)) {
            render();
            return;
        }
        if (e.info?.chat)
            return;
        if (e.info && (e.t === "session_created" || e.t === "session_updated" || e.t === "session_exited" || e.t === "session_restarted")) {
            app.sessions.set(e.info.id, e.info);
            if (e.t === "session_restarted" && e.info.id === app.viewing) {
                app.viewing = null;
            }
            render();
        }
        else if (e.t === "session_removed") {
            app.sessions.delete(e.id);
            app.screens.delete(e.id);
            app.drafts.delete(e.id);
            render();
        }
        else if (e.t === "bell") {
            if (shell && shell.id === e.session)
                shell.term.bell();
        }
        else if (e.t === "clipboard") {
            if (shell && shell.id === e.session)
                shell.term.fromProgram(e.text);
        }
        else if (e.t === "setup_progress") {
            setupEvent(e);
        }
    }
});
useSessions(() => app.sessions);
conn.onState = render;
// Typing while the connection is down goes nowhere: say so (not for every key).
let droppedAt = 0;
conn.onDropped = (m) => {
    if (m.t !== "input" || Date.now() - droppedAt < 4000)
        return;
    droppedAt = Date.now();
    toast("Not sent: offline. It reconnects by itself.");
};
conn.onTooBig = (m) => toast(m.t === "input" ? "Not sent: that paste is too large (over 4 MiB)" : "Not sent: too large (over 4 MiB)");
ui.render = render;
ui.toast = toast;
ui.fail = (text) => { if (!offerSetup(text))
    toast(text); };
ui.header = header;
ui.push = () => void enablePush();
ui.forget = () => { void forget(); };
// Keys, for a desktop: n new task, m new chat, / find, C-g find in every chat, g then s c p m go to a tab, ? this list.
let goKey = 0;
document.addEventListener("keydown", (e) => {
    const t = e.target;
    // C-g: find across every chat (not in a terminal, whose program has the key).
    if (e.ctrlKey && !e.metaKey && !e.altKey && e.key === "g" && token() && chatsShown() && route().view !== "session" && !document.querySelector(".scrim")) {
        e.preventDefault();
        findAllChats();
        return;
    }
    if (e.metaKey || e.ctrlKey || e.altKey || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName) || t.isContentEditable || document.querySelector(".scrim") || !token())
        return;
    const tabs = { s: "#/", c: "#/chats", p: "#/p", m: "#/more" };
    if (goKey && Date.now() - goKey < 1500 && tabs[e.key]) {
        goKey = 0;
        location.hash = tabs[e.key];
        e.preventDefault();
        return;
    }
    goKey = 0;
    if (e.key === "g")
        goKey = Date.now();
    else if (e.key === "n") {
        e.preventDefault();
        newTask();
    }
    // SHELLS (SPEC §23.14): a digit shows that shell, on the list and on a shell's page (the
    // terminal, once tapped, has the keys).
    else if (/^[1-9]$/.test(e.key) && (route().view === "list" || (route().view === "session" && app.sessions.get(route().id ?? "")?.loose))) {
        e.preventDefault();
        const here = app.sessions.get(route().id ?? "");
        void goShell(app.sessions.values(), Number(e.key), here?.loose ? here.workspace ?? "main" : listWorkspace());
    }
    else if (e.key === "m" && chatsShown()) {
        e.preventDefault();
        newChat();
    }
    else if (e.key === "/" && route().view === "chats") {
        e.preventDefault();
        document.querySelector("input.search")?.focus();
    }
    // In a chat, `/` starts a slash command in its composer.
    else if (e.key === "/" && route().view === "c") {
        const r = document.querySelector(".chat-page .reply textarea");
        if (r) {
            e.preventDefault();
            if (!r.value) {
                r.value = "/";
                r.dispatchEvent(new Event("input"));
            }
            r.focus();
        }
    }
    else if (e.key === "/") {
        e.preventDefault();
        if (route().view !== "list")
            location.hash = "#/";
        setTimeout(() => (searchBox.isConnected ? searchBox : document.querySelector("input.search"))?.focus(), 50);
    }
    else if (e.key === "?")
        sheet("Keys", [h("dl", { class: "facts" }, ...[["n", "New task"], ["m", "New chat"], ["1…9", "Shell N of SHELLS (on the list and a shell's page)"], ["/", "Find a session (in the chats: a chat; in a chat: a command)"], ["Ctrl-G", "Search every chat"], ["Enter", "In a chat: send (Shift+Enter: a new line)"], ["@", "In a chat's box: its folders"], ["g s", "Sessions"], ["g c", "Chats"], ["g p", "Projects"], ["g m", "More"], ["Esc", "Close a dialog"]].flatMap(([k, d]) => [h("dt", {}, h("kbd", {}, k)), h("dd", {}, d)]))]);
});
window.addEventListener("hashchange", () => { takePairCode(); render(); });
takePairCode();
if ("serviceWorker" in navigator)
    void navigator.serviceWorker.register("/sw.js");
render();
conn.connect();
setInterval(() => { if (route().view !== "session")
    render(); }, 30_000);
