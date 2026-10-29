// Other machines (ROADMAP MM1.B, SPEC §16.4b): their workspaces show beside this machine's,
// through this machine's daemon, so the phone pairs with one machine and sees them all. This page
// lists them, adds one (over ssh, or with the code `ptyro machine pair` printed on it), makes a
// code for another to add this one, and retries, hides or removes. The chip says which machine
// something is on, in that machine's colour.
import { conn, host } from "./conn.js";
import { data, splitMachine } from "./data.js";
import { accent } from "./machine.js";
import { actions, ago, ask, badge, bar, confirm, errText, form, h, menu, row, section, sheet, ui, views } from "./kit.js";
const ACTIONS = {
    "machine.list": "Other machines whose workspaces show here: how each link stands, and its workspaces",
    "machine.add": "Show another machine's workspaces here: over ssh, or with the code `ptyro machine pair` printed on it",
    "machine.pair": "A code for another machine to show this one's workspaces (`ptyro machine add CODE` there)",
    "machine.retry": "Try a machine's link again now",
    "machine.hide": "Don't show one of a machine's workspaces here (nothing changes there)",
    "machine.unhide": "Show a hidden workspace of a machine again",
    "machine.remove": "Stop showing a machine's workspaces here (nothing changes there)",
};
const tip = actions(ACTIONS);
const dark = () => !(typeof matchMedia === "function" && matchMedia("(prefers-color-scheme: light)").matches);
/// `● laptop2` in its colour, with how its link stands when that isn't well. Nothing for this
/// machine's things.
export function machineChip(name) {
    const [m] = name ? splitMachine(name) : [null];
    if (!m)
        return null;
    const dot = h("span", { class: "machine-dot", "aria-hidden": "true" }, "●");
    // The CSSOM, not a style attribute (the CSP refuses those).
    dot.style.color = accent(m, dark());
    const info = data.machines.get(m);
    const state = info && info.state !== "online" ? badge(stateWords(info), "bad") : null;
    return h("span", { class: "machine-chip", title: info?.fix ?? m }, dot, ` ${m}`, state ? " " : null, state);
}
function stateWords(m) {
    const since = m.since_ms ? ` · ${ago(m.since_ms)} ago` : "";
    switch (m.state) {
        case "offline": return `offline${since}`;
        case "mismatch": return "other version";
        case "connecting": return "connecting…";
        default: return "online";
    }
}
function transportWords(m) {
    return m.transport.t === "ssh" ? `ssh://${m.transport.target.replace(/^ssh:\/\//, "")}` : m.transport.url;
}
function machinesView() {
    const list = [...data.machines.values()];
    const full = conn.scope === "full";
    const body = list.length ? h("div", { class: "rows" }, ...list.map((m) => row(h("span", {}, machineChip(`${m.name}:`) ?? m.name, " ", badge(m.state === "online" ? "online" : stateWords(m), m.state === "online" ? "ok" : "bad")), [transportWords(m), m.version ? `ptyro ${m.version}` : "", `${m.workspaces.length - m.hidden.length} workspace(s)${m.hidden.length ? `, ${m.hidden.length} hidden` : ""}`, m.state !== "online" && m.fix ? `→ ${m.fix}` : ""].filter(Boolean).join(" · "), "⋯", { onclick: () => machineMenu(m) })))
        : h("div", { class: "empty" }, h("p", {}, "No other machines. Add one, and its workspaces show here beside this machine's, named like laptop2 / work."), full ? h("button", { class: "primary", title: tip("machine.add"), onclick: addMachine }, "Add a machine") : null);
    return h("div", {}, ui.header("Machines", "#/more", full ? h("button", { class: "icon plus", title: tip("machine.add"), "aria-label": "Add a machine", onclick: addMachine }, "＋") : null), h("main", { class: "stack", title: tip("machine.list") }, conn.machine ? h("p", { class: "intro" }, "Connected to ", machineChip(`${conn.machine}:`) ?? conn.machine, ` (${conn.machine}). Its daemon reaches the others.`) : null, body, full ? section("This machine on another", h("div", {}, h("p", { class: "sub" }, "Make a code here, then add it on the other machine. Or add this one there over ssh."), bar(h("button", { title: tip("machine.pair"), onclick: pairMachine }, "Make a code")))) : null, !full ? h("p", { class: "sub" }, "Adding and removing machines needs a device paired with ", h("code", {}, "ptyro pair --scope full"), ".") : null));
}
function machineMenu(m) {
    menu(m.name, [
        { title: tip("machine.retry"), label: "Try its link now", hint: m.state === "online" ? "It is online" : m.fix ?? "", run: async () => { await ask({ t: "machine_retry", name: m.name }, `Trying ${m.name}…`); } },
        ...m.workspaces.filter((w) => !m.hidden.includes(w)).map((w) => ({ title: tip("machine.hide"), label: `Hide ${w} here`, hint: "Nothing changes there", run: async () => { await ask({ t: "machine_hide", name: m.name, workspace: w, hidden: true }, `${m.name} / ${w} is hidden here`); } })),
        ...m.hidden.map((w) => ({ title: tip("machine.unhide"), label: `Show ${w} again`, run: async () => { await ask({ t: "machine_hide", name: m.name, workspace: w, hidden: false }, `${m.name} / ${w} is shown again`); } })),
        { title: tip("machine.remove"), label: "Stop showing it here", danger: true, hint: "Nothing changes on it", run: () => confirm(`Stop showing ${m.name}?`, "Its workspaces leave this list. Nothing changes on it; revoke this machine's token there too.", "Stop", async () => { await ask({ t: "machine_remove", name: m.name }, `${m.name} is no longer shown here`); }) },
    ]);
}
function addMachine() {
    form("Add a machine", [
        { key: "target", label: "ssh://HOST, or the code `ptyro machine pair` printed on it", required: true, hint: "Over ssh it needs a key or an agent, no password prompt. A code is claimed once, within 10 minutes." },
        { key: "name", label: "Name here (optional)", slug: "name", hint: "Default: what it calls itself. A space or other odd character becomes _" },
    ], "Add", async (v) => {
        try {
            const r = await host("machine.add", { target: v.target.trim(), name: v.name?.trim() || null });
            ui.toast(r?.state === "online" || r?.state === "connecting" ? `Added ${r.name}` : `Added ${r?.name ?? "it"}: ${r?.fix ?? r?.state ?? ""}`);
        }
        catch (e) {
            ui.toast(errText(e));
        }
    });
}
async function pairMachine() {
    try {
        const r = await host("machine.pair", {});
        const code = r.code;
        sheet("Add this machine on another", [
            h("p", { class: "intro" }, "On the other machine, within 10 minutes, once:"),
            h("pre", { class: "block" }, `ptyro machine add '${code}'`),
            h("p", { class: "sub" }, "It gets full access to this machine's sessions; it's listed under Devices as \"machine\", where you can revoke it."),
        ], [h("button", { class: "primary", onclick: () => void navigator.clipboard?.writeText(`ptyro machine add '${code}'`).then(() => ui.toast("Copied")) }, "Copy")]);
    }
    catch (e) {
        ui.toast(errText(e));
    }
}
views.set("machines", () => machinesView());
