// Home (SPEC §26.10): one look at everything. A row a project, ranked by the daemon (`home`,
// `ptyro_core::home`): what needs you, what was left hanging, what is being worked on, what is
// quiet; every profile's usage under it. A row unfolds into its trail: where it was left off,
// what happened there, the files that work touched most. The page only draws what the daemon
// ranked, and asks again a moment after anything changes.
import { conn } from "./conn.js";
import { can } from "./data.js";
import { actions, ago, badge, bar, enc, errText, go, h, poll, ui, views } from "./kit.js";
import { historyOf } from "./library.js";
import { machineChip } from "./machines.js";
import { newTask } from "./sessions.js";
import { CAPS, HANGING, SECTIONS, TITLES, VERBS, awayLine, headline, spark } from "./homekit.js";
const ACTIONS = {
    "home": "Home: everything, ranked. What needs you, what was left hanging, what is being worked on and what is quiet, a row a project, with every profile's usage",
    "home.go": "Go to it: the session (a sleeping one is woken), the worktree with work nobody is running, or what was kept of a session that is gone",
    "home.unfold": "Unfold a project: where it was left off, what happened there lately, and the files that work touched most",
    "home.continue": "Start a task in this project that picks up where it was left off",
    "home.put_away": "Put this away until it changes (only what was left hanging)",
    "home.restore": "Bring back everything that was put away",
    "home.usage": "Every profile's usage of every agent: each window it reports, how full, and when it resets",
};
const tip = actions(ACTIONS);
const state = {
    view: null,
    error: "",
    open: null,
    full: new Set(),
    asking: false,
    again: 0,
};
/// Ask for the view (with the unfolded row's trail). `here`: because of something you did.
async function load(here, extra = {}) {
    if (state.asking) {
        state.again = Date.now();
        return;
    }
    state.asking = true;
    try {
        const r = await conn.request({ t: "home", trail: state.open, here, ...extra });
        state.view = r.home;
        state.error = "";
        if (state.open && !state.view.rows.some((x) => x.key === state.open))
            state.open = null;
    }
    catch (e) {
        state.error = /unknown variant|unsupported/i.test(errText(e)) ? "This computer's ptyro is older than home: update it (More → Updates), then open this again." : errText(e);
    }
    state.asking = false;
    ui.render();
    if (state.again) {
        state.again = 0;
        void load(false);
    }
}
let settle = 0;
conn.listeners.add((msg) => {
    if (!location.hash.startsWith("#/home"))
        return;
    if (msg.t === "connected") {
        void load(true);
        return;
    }
    if (msg.t !== "event" || msg.event?.t === "host_load")
        return;
    // Whatever changed, home shows it: asked again once things settle.
    clearTimeout(settle);
    settle = window.setTimeout(() => void load(false), 700);
});
document.addEventListener("visibilitychange", () => { if (document.visibilityState === "visible" && location.hash.startsWith("#/home"))
    void load(true); });
let shownAt = "";
window.addEventListener("hashchange", () => { if (location.hash.startsWith("#/home") && shownAt !== location.hash)
    void load(true); shownAt = location.hash; });
/// A page opened with nowhere to go (no `#…` at all) opens on home, when `[ui] home_at_start`
/// says so. `#/` is always the sessions.
export async function landOnHome() {
    if (location.hash !== "" && location.hash !== "#")
        return;
    try {
        const r = await conn.request({ t: "home", here: true });
        if (!r.home.at_start || (location.hash !== "" && location.hash !== "#"))
            return;
        state.view = r.home;
        go("#/home");
    }
    catch { /* an older daemon: the sessions, as before */ }
}
const levelClass = (l) => (l === "limited" ? "bad" : l === "warn" ? "warn" : "");
/// ⏎ on a thing: where it is.
async function goTo(row, t) {
    if (t.session) {
        go(`#/s/${enc(t.session)}`);
        return;
    }
    if (t.next === "commit" || t.next === "push") {
        if (t.worktree)
            go(`#/wt/${enc(t.worktree)}`);
        return;
    }
    if (t.item) {
        go(`#/library/k/${enc(t.item)}`);
        return;
    }
    if (row.project)
        newTask(`p:${row.project}`);
}
async function putAway(t) {
    await load(true, { dismiss: { key: t.key, stamp: t.stamp } });
    ui.toast("Put away until it changes");
}
function thingLine(row, t, first) {
    const where = [t.session_name ?? t.worktree_name, t.left, first && row.things.length > 1 ? `+${row.things.length - 1} more` : "", t.at_ms ? ago(t.at_ms) : ""].filter(Boolean).join(" · ");
    const kind = ["answer", "failed", "look"].includes(t.next) ? "bad" : t.next === "review" ? "ok" : t.next === "working" ? "info" : HANGING.includes(t.next) ? "warn" : "";
    return h("div", { class: `home-thing tap ${t.next}`, title: tip("home.go"), onclick: (e) => { e.stopPropagation(); void goTo(row, t); } }, VERBS[t.next] ? badge(VERBS[t.next], kind) : null, h("span", { class: "said" }, t.text), h("span", { class: "where" }, where));
}
function trailOf(row, v) {
    const trail = v.trail?.key === row.key ? v.trail : null;
    const kids = row.things.slice(1).map((t) => thingLine(row, t, false));
    if (!trail)
        kids.push(h("p", { class: "sub" }, "…"));
    const l = trail?.left_off;
    if (l) {
        const open = () => { if (l.session)
            go(`#/s/${enc(l.session)}`);
        else if (l.addr)
            go(`#/library/k/${enc(l.addr)}`); };
        kids.push(h("div", { class: "home-left tap", title: tip("home.go"), onclick: (e) => { e.stopPropagation(); open(); } }, h("div", { class: "sub" }, `left off · ${ago(l.at_ms)} ago`), l.you ? h("div", {}, h("b", {}, "you: "), l.you) : null, l.reply ? h("div", {}, h("b", {}, `${l.agent ?? "agent"}: `), l.reply) : null));
    }
    let day = "";
    for (const e of trail?.entries ?? []) {
        const d = new Date(e.at_ms).toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" });
        if (d !== day) {
            kids.push(h("h4", { class: "day" }, d));
            day = d;
        }
        kids.push(h("a", { class: "home-entry", href: e.session ? `#/s/${enc(e.session)}` : `#/library/k/${enc(e.addr)}`, onclick: (ev) => ev.stopPropagation() }, badge(e.kind), h("span", { class: "said" }, e.title), h("span", { class: "where" }, e.detail ?? "")));
    }
    if (trail?.hot.length)
        kids.push(h("p", { class: "sub home-hot" }, "hot files: ", trail.hot.map((x) => `${x.path} ×${x.count}`).join(" · ")));
    if (trail && !l && !trail.entries.length) {
        kids.push(h("p", { class: "sub" }, v.memory === "off" ? "The knowledge base is off: a trail needs it (Settings → Knowledge)."
            : !row.project ? "Sessions of no project have no trail."
                : row.machine ? `Its trail is on ${row.machine}: open ptyro there to see it.`
                    : "Nothing kept of it yet: its trail fills in as agents work here."));
    }
    const next = row.things[0];
    const from = next.item ?? l?.addr;
    const stop = (f) => (e) => { e.stopPropagation(); void f(); };
    kids.push(bar(row.project && can("control") ? h("button", { class: "primary", onclick: stop(() => newTask(`p:${row.project}`)) }, "New task") : null, row.project && from && can("control") ? h("button", { title: tip("home.continue"), onclick: stop(() => newTask(`p:${row.project}`, `Pick up where this was left off (read it first: \`ptyro kb show ${from}\`). `)) }, "Continue") : null, row.project && !row.machine ? h("button", { onclick: stop(() => historyOf(null, row.name)) }, "Its history") : null, row.project ? h("a", { class: "button", href: `#/proj/${enc(row.project)}`, onclick: (e) => e.stopPropagation() }, "The project") : null, HANGING.includes(next.next) && can("control") ? h("button", { title: tip("home.put_away"), onclick: stop(() => putAway(next)) }, "Put away") : null));
    return h("div", { class: "home-trail" }, ...kids);
}
function rowOf(row, v) {
    const open = state.open === row.key;
    const toggle = (e) => {
        e.stopPropagation();
        state.open = open ? null : row.key;
        ui.render();
        if (!open)
            void load(true);
    };
    const week = spark(row.week);
    return h("div", { class: `home-row ${row.section} ${open ? "open" : ""}` }, h("div", { class: "home-head tap", onclick: () => void goTo(row, row.things[0]), title: tip("home.go") }, h("div", { class: "name" }, machineChip(row.machine ? `${row.machine}:` : null), row.machine ? " " : null, row.name, week ? h("span", { class: "spark", title: "Prompts a day, the last seven days" }, ` ${week}`) : null, row.working ? h("small", {}, ` ${row.working} working`) : null), thingLine(row, row.things[0], true), h("button", { class: "icon unfold", title: tip("home.unfold"), "aria-label": open ? "Fold" : "Unfold", "aria-expanded": String(open), onclick: toggle }, open ? "▾" : "▸")), open ? trailOf(row, v) : null);
}
function homeView() {
    if (!state.view && !state.asking && !state.error)
        void load(true);
    poll("home", () => void load(false), 30_000);
    const v = state.view;
    const refresh = h("button", { class: "icon", title: "Look again", "aria-label": "Refresh", onclick: () => void load(true) }, "↻");
    if (!v) {
        return h("div", {}, ui.header("Home", false, refresh), h("main", { class: "stack home", title: tip("home") }, h("p", { class: "empty" }, state.error || "Looking…")));
    }
    const kids = [h("p", { class: "home-headline" }, h("b", {}, headline(v)), v.away ? h("span", { class: "away" }, ` · ${awayLine(v.away)}`) : null)];
    if (state.error)
        kids.push(h("p", { class: "sub" }, state.error));
    for (const s of SECTIONS) {
        const rows = v.rows.filter((r) => r.section === s);
        if (!rows.length)
            continue;
        const cap = state.full.has(s) ? Infinity : CAPS[s] ?? Infinity;
        const shown = rows.filter((r, i) => i < cap || r.key === state.open);
        kids.push(h("section", { class: `block-sec home-sec ${s}` }, h("h3", {}, TITLES[s], h("small", {}, ` ${rows.length}`)), h("div", { class: "rows" }, ...shown.map((r) => rowOf(r, v)), shown.length < rows.length ? h("button", { class: "more", onclick: () => { state.full.add(s); ui.render(); } }, `${rows.length - shown.length} more`) : null)));
    }
    if (!v.rows.length)
        kids.push(h("div", { class: "empty" }, h("p", {}, "No projects yet."), h("a", { class: "button", href: "#/p" }, "Add a project")));
    if (v.meters.length) {
        kids.push(h("section", { class: "block-sec home-usage", title: tip("home.usage") }, h("h3", {}, "Usage"), h("div", { class: "home-meters" }, ...v.meters.map((m) => h("span", { class: `home-meter ${levelClass(m.level)}` }, h("b", {}, m.label), " ", ...m.parts.flatMap((p, i) => [i ? " · " : "", h("span", { class: levelClass(p.level) }, p.text)]))))));
    }
    if (v.memory === "off")
        kids.push(h("p", { class: "sub" }, "The knowledge base is off: only what is live is shown. ", h("a", { href: "#/settings/knowledge" }, "Switch it on"), " for what was left hanging in sessions that are gone, and for each project's trail."));
    if (can("control"))
        kids.push(bar(h("button", { title: tip("home.restore"), onclick: () => void load(true, { restore: true }).then(() => ui.toast("Everything put away is back")) }, "Bring back what was put away")));
    return h("div", {}, ui.header("Home", false, refresh), h("main", { class: "stack home", title: tip("home") }, ...kids));
}
views.set("home", () => homeView());
