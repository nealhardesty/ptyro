// Settings and health (ROADMAP WEB1.6): the More tab: this device, the doctor's findings with
// their fixes, usage, devices and pairing another, profiles and their logins, the config file,
// agent integrations, the daemon, first-run setup and the tour.
import { conn, host, token } from "./conn.js";
import { data } from "./data.js";
import { actions, ago, ask, badge, bar, Cache, confirm, download, enc, errText, facts, form, go, h, menu, pending, poll, row, section, sheet, textSheet, ui, views } from "./kit.js";
import { qrMatrix } from "./qr.js";
import { allSessions } from "./sessions.js";
const ACTIONS = {
    "more": "Everything beyond the lists: health, devices, profiles, the config, the daemon",
    "health": "What's wrong or worth knowing, from ptyro's checks, each with its fix",
    "usage": "How much of each account's limits is used",
    "notify.test": "Send a test notification through every channel you set up",
    "devices.list": "Devices paired with ptyro, what each may do, when it was last here",
    "devices.revoke": "Unpair a device: its token stops working at once",
    "pair.device": "Make a pairing link for another device",
    "profile.list": "Profiles: accounts agents run under, each with its logins",
    "profile.show": "A profile: its directories, git identity and variables",
    "profile.new": "Add a profile, adopting directories you already have",
    "profile.remove": "Remove a profile from your config (refused while it's in use)",
    "profile.rename": "Rename a profile everywhere it's used",
    "profile.login": "Log in to an agent or gh under the profile, in its own session",
    "profile.sync": "Link the files a profile shares (CLAUDE.md, skills) from the agents' defaults",
    "profile.detect": "Find accounts already set up here (aliases, ~/.claude-* dirs, git includes)",
    "profile.doctor": "Check a profile: directories, logins, identity",
    "integrations": "Agent integrations that need installing once",
    "config.edit": "Edit the config; it's checked as you save and applies at once",
    "config.check": "Check the config without saving it",
    "config.show": "The effective config, with defaults filled in",
    "config.path": "Where the config file is",
    "daemon.status": "The daemon: version, uptime, sessions, health",
    "daemon.logs": "The daemon's log, the latest lines",
    "daemon.upgrade": "Switch the daemon to the ptyro on disk, keeping every session",
    "daemon.restart": "Stop and start the daemon (sessions are stopped; agents hibernate)",
    "daemon.stop": "Stop every session and the daemon",
    "daemon.service": "Start ptyro at login (a systemd or launchd service)",
    "daemon.unservice": "Remove the login service",
    "first.run": "First-run setup: your agents, accounts and repositories",
    "help.tour": "A tour: ptyro's daily loop with a pretend agent, in a throwaway ptyro of its own",
};
const tip = actions(ACTIONS);
const LEVEL = { ok: ["✓", "ok"], note: ["·", ""], warn: ["!", "warn"], problem: ["✗", "bad"] };
const SCOPES = [
    ["read", "Read", "Watch sessions, screens and the inbox"],
    ["approve", "Approve", "Also answer prompts and gates"],
    ["control", "Control", "Also type into sessions and start configured agents"],
    ["full", "Full", "Everything: any command, the config, this page"],
];
const hostFail = (e) => ui.toast(errText(e));
/// A host operation, with its failure in a toast.
async function doHost(op, args = {}, ok) {
    try {
        const r = await host(op, args);
        if (ok)
            ui.toast(ok);
        return r;
    }
    catch (e) {
        hostFail(e);
        return undefined;
    }
}
const showNotes = (title, notes) => { if (notes.length <= 1)
    ui.toast(notes[0] ?? title);
else
    textSheet(title, notes.join("\n")); };
// ── More ────────────────────────────────────────────────────────────────────────────────────
function moreView() {
    const scope = SCOPES.find(([k]) => k === conn.scope);
    const items = [
        ["Review queue", "Worktrees waiting for you", "#/review"],
        ["Inbox", "What happened while you were away", "#/inbox"],
        ["Health", "What's wrong, with its fixes", "#/health"],
        ["Usage", "Your accounts' limits", "#/usage"],
        ["Devices", "Phones and browsers paired with ptyro", "#/devices"],
        ["Profiles", "Accounts agents run under", "#/profiles"],
        ["Configuration", "Your config file", "#/config"],
        ["Integrations", "Agents that need installing once", "#/integrations"],
        ["Daemon", "Version, logs, restart, login service", "#/daemon"],
        ["Set up ptyro", "Your agents, accounts and repositories", "#/setup"],
        ["Take the tour", "ptyro's daily loop with a pretend agent", "#/tour"],
    ];
    return h("div", {}, ui.header("More", false), h("main", { class: "stack", title: tip("more") }, section("This device", facts([
        ["Access", scope ? h("span", {}, badge(scope[1], conn.scope === "full" ? "ok" : ""), " ", scope[2]) : conn.scope],
        ["Connection", conn.state],
    ]), bar(h("button", { onclick: () => menu("Notifications", [
            { label: "Turn them on for this device", hint: "A test is sent", run: () => ui.push() },
            { title: tip("notify.test"), label: "Send a test through every channel", hint: "This device, desktop notifications, anything in your config", run: async () => { const r = await ask({ t: "notify_test" }); if (r)
                    textSheet("Notification test", r.results.join("\n") || "Nothing is set up to notify."); } },
        ]) }, "🔔 Notifications"), h("button", { class: "deny", onclick: () => confirm("Forget this device?", "It stops showing your sessions until you pair it again. `ptyro devices revoke` ends the pairing for good.", "Forget", () => ui.forget()) }, "Forget this device"))), conn.scope !== "full" ? h("p", { class: "intro" }, "Some pages here change the machine and need a device paired with ", h("code", {}, "ptyro pair --scope full"), ".") : null, h("div", { class: "rows" }, ...items.map(([t, s, href]) => row(t, s, "›", { href })))));
}
// ── health ──────────────────────────────────────────────────────────────────────────────────
const health = new Cache(() => host("doctor", { quick: false }).then((r) => r.checks));
function fixesFor(c) {
    const out = [];
    const prof = String(c.section).match(/^profile (.+)$/);
    if (prof && ["claude", "codex", "gemini", "opencode", "gh"].includes(c.name) && c.level !== "ok")
        out.push({ label: `Log in to ${c.name} as ${prof[1]}`, run: () => profileLogin(prof[1], c.name) });
    if (c.section === "ptyro" && c.name.startsWith("config"))
        out.push({ label: "Edit the config", run: () => go("#/config") });
    if (c.section === "remote" || /restart/.test(c.text))
        out.push({ label: "Restart ptyro", run: () => restartDaemon() });
    if (c.section === "daemon" && /binary|upgrade|older|newer/i.test(c.text))
        out.push({ label: "Upgrade the daemon", run: () => upgradeDaemon() });
    if (c.section === "integrations")
        out.push({ label: "Integrations", run: () => go("#/integrations") });
    return out;
}
function healthView() {
    const cs = health.get("");
    const body = pending(health, "", "Checking everything (a few seconds)…") ?? (() => {
        const bad = cs.filter((c) => c.level === "problem").length, warn = cs.filter((c) => c.level === "warn").length;
        const sections = [...new Set(cs.map((c) => c.section))];
        return h("div", { class: "stack-in" }, h("p", { class: "pr-head" }, bad || warn ? `${bad} problem${bad === 1 ? "" : "s"}, ${warn} warning${warn === 1 ? "" : "s"}` : "All good"), ...sections.map((sec) => section(sec, h("div", { class: "rows" }, ...cs.filter((c) => c.section === sec).map((c) => {
            const [mark, kind] = LEVEL[c.level] ?? ["·", ""];
            const fixes = fixesFor(c);
            return row(h("span", {}, h("span", { class: `lvl ${kind}` }, mark), " ", c.name), c.text, fixes.length ? "⋯" : null, { cls: "wrap", onclick: fixes.length ? () => menu(`${c.name}`, fixes) : undefined });
        })))));
    })();
    return h("div", {}, ui.header("Health", "#/more", h("button", { class: "icon", title: tip("health"), "aria-label": "Check again", onclick: () => health.reload("") }, "↻")), h("main", { class: "stack", title: tip("health") }, conn.scope !== "full" ? h("p", { class: "empty" }, "Checking the machine needs a device paired with --scope full.") : body));
}
// ── usage ───────────────────────────────────────────────────────────────────────────────────
const usage = new Cache(() => conn.request({ t: "usage" }).then((r) => r.usage));
function usageView() {
    poll("usage", () => usage.refresh(""), 15000);
    const list = usage.get("");
    const body = pending(usage, "") ?? (list.length ? h("div", { class: "stack-in" }, ...list.map((u) => h("section", { class: "meter" }, h("div", { class: "meter-head" }, h("b", {}, `${u.agent}`), " ", h("span", { class: "sub" }, u.profile), u.limited_until_ms ? badge(`limited until ${new Date(u.limited_until_ms).toLocaleTimeString()}`, "bad") : null), ...u.windows.map((w) => h("div", { class: "meter-row" }, h("span", {}, w.name), h("div", { class: "bar-track" }, h("div", { class: `bar-fill ${w.used_percent >= 90 ? "bad" : w.used_percent >= 70 ? "warn" : ""}` }, "")), h("span", {}, `${w.used_percent}%`), w.resets_at_ms ? h("span", { class: "sub" }, `resets ${new Date(w.resets_at_ms).toLocaleTimeString()}`) : null)), u.context_percent != null ? h("div", { class: "sub" }, `context ${u.context_percent}%`) : null, u.cost_millis != null ? h("div", { class: "sub" }, `$${(u.cost_millis / 1000).toFixed(2)} this session`) : null)))
        : h("p", { class: "empty" }, "No usage yet: it shows once an agent reports its limits."));
    // Set the bars' widths (an inline style attribute would break the CSP; the CSSOM is fine).
    requestAnimationFrame(() => document.querySelectorAll(".meter-row").forEach((r, i) => { void i; const pct = parseInt(r.children[2]?.textContent ?? "0", 10); const f = r.querySelector(".bar-fill"); if (f)
        f.style.width = `${pct}%`; }));
    return h("div", {}, ui.header("Usage", "#/more"), h("main", { class: "stack", title: tip("usage") }, body));
}
// ── devices ─────────────────────────────────────────────────────────────────────────────────
const devices = new Cache(() => conn.request({ t: "device_list" }).then((r) => r.devices));
function pairAnother() {
    form("Pair another device", [
        { key: "name", label: "Name", value: "phone", required: true },
        { key: "scope", label: "May", type: "select", options: SCOPES.map(([k, l, d]) => [k, `${l}: ${d}`]), value: "control" },
    ], "Make a link", async (v) => {
        const r = await conn.request({ t: "device_pair", name: v.name, scope: v.scope });
        const link = `${location.origin}/#pair=${r.code}`;
        const s = sheet(`Pair ${v.name}`, [
            h("p", { class: "intro" }, `Open this on the device (or scan the code). It works once, for ${Math.round((r.expires_ms - Date.now()) / 60000)} minutes.`),
            qrBox(link),
            h("pre", { class: "block" }, link),
        ], [
            h("button", { onclick: () => void navigator.clipboard?.writeText(link).then(() => ui.toast("Copied"), () => ui.toast("Couldn't copy")) }, "Copy"),
            "share" in navigator ? h("button", { onclick: () => void navigator.share({ title: "ptyro", url: link }).catch(() => { }) }, "Share…") : null,
            h("button", { class: "primary", onclick: () => { s.close(); devices.reload(""); } }, "Done"),
        ]);
        devices.reload("");
    });
}
function devicesView() {
    poll("devices", () => devices.refresh(""), 10000);
    const list = devices.get("");
    return h("div", {}, ui.header("Devices", "#/more", h("button", { class: "icon plus", title: tip("pair.device"), "aria-label": "Pair another device", onclick: pairAnother }, "＋")), h("main", { class: "stack", title: tip("devices.list") }, conn.scope !== "full" ? h("p", { class: "empty" }, "Listing devices needs a device paired with --scope full.") :
        pending(devices, "") ?? (list.length ? h("div", { class: "rows" }, ...list.map((d) => row(h("span", {}, d.name, " ", badge(d.scope, d.scope === "full" ? "warn" : ""), d.pending ? badge("waiting to pair", "info") : null), `${d.last_seen_ms ? `seen ${ago(d.last_seen_ms)} ago` : "never seen"}${d.last_via ? ` · ${d.last_via}` : ""}${d.push ? ` · ${d.push} notification${d.push === 1 ? "" : "s"}` : ""}`, null, { onclick: () => menu(d.name, [{ title: tip("devices.revoke"), label: "Revoke", danger: true, hint: "Its token stops working at once", run: () => confirm(`Revoke ${d.name}?`, "It's unpaired now. If it's this device, you'll be signed out.", "Revoke", async () => { await ask({ t: "device_revoke", id: d.id }, "Revoked"); devices.reload(""); }) }]) }))) : h("p", { class: "empty" }, "No devices."))));
}
// a QR code, drawn here (qr.ts, no library)
function qrBox(text) {
    const m = qrMatrix(text);
    if (!m)
        return h("p", { class: "sub" }, "(too long for a code: use the link)");
    const n = m.length, px = 4, quiet = 4;
    const c = h("canvas", { class: "qr", "aria-label": "QR code of the link" });
    const size = (n + quiet * 2) * px;
    c.width = size;
    c.height = size;
    const ctx = c.getContext("2d");
    if (ctx) {
        ctx.fillStyle = "#fff";
        ctx.fillRect(0, 0, size, size);
        ctx.fillStyle = "#000";
        for (let y = 0; y < n; y++)
            for (let x = 0; x < n; x++)
                if (m[y][x])
                    ctx.fillRect((x + quiet) * px, (y + quiet) * px, px, px);
    }
    return c;
}
// ── profiles ────────────────────────────────────────────────────────────────────────────────
const profiles = new Cache(() => conn.request({ t: "profile_list" }).then((r) => r.profiles));
const AGENT_DIRS = ["claude", "codex", "gemini", "opencode", "gh"];
async function profileLogin(name, target) {
    const r = await doHost("profile.login", { name, target });
    if (r) {
        ui.toast(`Logging in to ${target} as ${name}: the tool's own flow; ptyro never sees the credentials`);
        go(`#/s/${enc(r.session)}`);
    }
}
function profileForm() {
    form("New profile", [
        { key: "name", label: "Name", required: true, placeholder: "work" },
        { key: "label", label: "Label", placeholder: "shown in the sidebar" },
        { key: "color", label: "Colour", placeholder: "e.g. orange or #ff8800" },
        ...AGENT_DIRS.map((a) => ({ key: `dir_${a}`, label: `${a} directory`, placeholder: `adopt one you have, e.g. ~/.${a}-work`, hint: undefined })),
        { key: "git_name", label: "Git name" }, { key: "git_email", label: "Git email" }, { key: "ssh_key", label: "SSH key", placeholder: "~/.ssh/id_work" },
        { key: "matches", label: "Matches remotes", placeholder: "github.com:acme/*", hint: "Pins matching projects to it; comma separated" },
    ], "Create", async (v) => {
        const adopt = {};
        for (const a of AGENT_DIRS)
            if (v[`dir_${a}`])
                adopt[a] = String(v[`dir_${a}`]);
        const r = await host("profile.new", { name: v.name, label: v.label, color: v.color, adopt, git_name: v.git_name, git_email: v.git_email, ssh_key: v.ssh_key, matches: String(v.matches ?? "").split(",").map((s) => s.trim()).filter(Boolean) });
        profiles.reload("");
        showNotes("Profile added", r.notes ?? []);
    });
}
async function detectProfiles() {
    const r = await doHost("profile.detect");
    if (!r)
        return;
    const found = r.found;
    if (!found.length) {
        ui.toast("No multi-account setup found (aliases, ~/.claude-* dirs, git includes)");
        return;
    }
    const picks = new Map();
    const s = sheet("Accounts found", [
        h("p", { class: "intro" }, "Already set up on this machine. Tick the ones to add as profiles; your rc files aren't touched."),
        ...found.map((f) => {
            const c = h("input", { type: "checkbox" });
            c.checked = !f.known;
            c.disabled = f.known;
            picks.set(f.profile, c);
            return h("label", { class: "field check" }, c, h("span", {}, `${f.profile}${f.known ? " (already defined)" : ""}`), h("small", {}, `from ${f.sources.join(", ")}`));
        }),
    ], [h("button", { onclick: () => s.close() }, "Cancel"), h("button", { class: "primary", onclick: async () => {
                const chosen = [...picks].filter(([, c]) => c.checked && !c.disabled).map(([p]) => p);
                if (!chosen.length) {
                    s.close();
                    return;
                }
                s.close();
                const x = await doHost("profile.detect_apply", { profiles: chosen });
                if (x) {
                    profiles.reload("");
                    showNotes("Added", x.notes ?? []);
                }
            } }, "Add them")]);
}
function profilesView() {
    const list = profiles.get("");
    return h("div", {}, ui.header("Profiles", "#/more", h("button", { class: "icon", title: tip("profile.detect"), "aria-label": "Find my accounts", onclick: () => void detectProfiles() }, "🔍"), h("button", { class: "icon plus", title: tip("profile.new"), "aria-label": "New profile", onclick: profileForm }, "＋")), h("main", { class: "stack", title: tip("profile.list") }, pending(profiles, "") ?? h("div", { class: "rows" }, ...list.map((p) => row(h("span", {}, p.label || p.name, p.label && p.label !== p.name ? h("small", {}, ` ${p.name}`) : null, p.defined ? null : badge("implicit")), [p.git_email, p.isolate ? "isolated" : ""].filter(Boolean).join(" · ") || Object.keys(p.dirs).join(", "), "›", { href: `#/profile/${enc(p.name)}` })))));
}
function profileView(rest) {
    const name = decodeURIComponent(rest);
    const p = profiles.get("")?.find((x) => x.name === name);
    const body = pending(profiles, "") ?? (!p ? h("p", { class: "empty" }, "No such profile.") : h("div", { class: "stack-in", title: tip("profile.show") }, facts([["Label", p.label], ["Colour", p.color], ["Isolated", p.isolate ? "yes: credentials it inherits are removed" : "no: it inherits the daemon's"], ["Git", p.git_email],
        ["Matches remotes", (p.matches?.remotes ?? []).join(", ")], ["Matches paths", (p.matches?.paths ?? []).join(", ")], ["Variables", p.vars.join(" ")]]), section("Directories", facts(Object.entries(p.dirs).map(([a, d]) => [a, h("code", {}, String(d))]))), section("Log in", h("div", { class: "bar", title: tip("profile.login") }, ...AGENT_DIRS.map((a) => h("button", { onclick: () => void profileLogin(name, a) }, a)))), bar(h("button", { title: tip("profile.doctor"), onclick: async () => { const r = await doHost("profile.doctor", { name }); if (r)
            textSheet(`${name}: checks`, r.checks.map((c) => `${(LEVEL[c.level] ?? ["·"])[0]} ${c.name}: ${c.text}`).join("\n") || "Nothing to check."); } }, "Check"), h("button", { title: tip("profile.sync"), onclick: async () => { const r = await doHost("profile.sync", { name }); if (r)
            textSheet(`${name}: shared files`, r.notes.join("\n")); } }, "Sync shared files"), name !== "default" ? h("button", { title: tip("profile.rename"), onclick: () => form("Rename the profile", [{ key: "new", label: "New name", value: name, required: true }], "Rename", async (v) => { const r = await host("profile.rename", { old: name, new: v.new }); profiles.reload(""); go(`#/profile/${enc(String(v.new))}`); showNotes("Renamed", r.notes ?? []); }) }, "Rename") : null, name !== "default" ? h("button", { class: "deny", title: tip("profile.remove"), onclick: () => removeProfile(name) }, "Remove") : null)));
    return h("div", {}, ui.header(p?.label || name, "#/profiles"), h("main", { class: "stack" }, body));
}
function removeProfile(name, force = false) {
    confirm(`Remove ${name}?`, force ? "Whatever uses it falls back to the default profile." : "It leaves your config; its directories aren't touched.", force ? "Remove anyway" : "Remove", async () => {
        try {
            const r = await host("profile.rm", { name, force });
            profiles.reload("");
            go("#/profiles");
            showNotes("Removed", r.notes ?? []);
        }
        catch (e) {
            if (e.code === "conflict" && !force)
                confirm("It's in use", errText(e), "Remove anyway", () => removeProfile(name, true));
            else
                hostFail(e);
        }
    });
}
// ── the config ──────────────────────────────────────────────────────────────────────────────
const cfg = new Cache(() => host("config.get"));
let draft = null;
function configView() {
    const c = cfg.get("");
    const body = conn.scope !== "full" ? h("p", { class: "empty" }, "Editing the config needs a device paired with --scope full.") : pending(cfg, "") ?? (() => {
        if (draft === null)
            draft = c.text;
        const ta = h("textarea", { class: "code", rows: 22, spellcheck: "false", autocapitalize: "off", "aria-label": "config.toml" });
        ta.value = draft ?? "";
        const status = h("p", { class: "form-err" });
        ta.addEventListener("input", () => { draft = ta.value; status.textContent = ""; });
        const check = async (save) => {
            status.className = "form-err";
            try {
                const r = await host(save ? "config.save" : "config.check", { text: ta.value });
                status.className = "ok-note";
                status.textContent = `${save ? "Saved: it applies at once." : "Looks right."}${r.notes.length ? `\n${r.notes.join("\n")}` : ""}`;
                if (save) {
                    cfg.set("", { ...c, text: ta.value, exists: true });
                    draft = null;
                }
            }
            catch (e) {
                status.textContent = errText(e);
            }
        };
        return h("div", { class: "stack-in" }, h("div", { class: "sub", title: tip("config.path") }, c.exists ? c.path : `${c.path} (not there yet: saving creates it)`), ta, status, bar(h("button", { title: tip("config.check"), onclick: () => void check(false) }, "Check"), h("button", { class: "primary", title: tip("config.edit"), onclick: () => void check(true) }, "Save"), h("button", { title: tip("config.show"), onclick: async () => { const r = await doHost("config.show"); if (r)
                textSheet("The effective config", r.text); } }, "Show effective"), h("button", { onclick: () => download("config.toml", ta.value) }, "Download")));
    })();
    return h("div", {}, ui.header("Configuration", "#/more"), h("main", { class: "stack", title: tip("config.edit") }, body));
}
// ── integrations ────────────────────────────────────────────────────────────────────────────
const integrations = new Cache(() => host("integrations.ls").then((r) => r.integrations));
function integrationsView() {
    const list = integrations.get("");
    return h("div", {}, ui.header("Integrations", "#/more"), h("main", { class: "stack", title: tip("integrations") }, conn.scope !== "full" ? h("p", { class: "empty" }, "This needs a device paired with --scope full.") :
        pending(integrations, "") ?? h("div", { class: "rows" }, ...list.map((i) => row(h("span", {}, i.agent, " ", badge(i.active ? "active" : "not installed", i.active ? "ok" : "warn")), i.detail, null, i.needs_install ? { onclick: () => menu(i.agent, [
                !i.active && { label: "Install", run: async () => { const r = await doHost("integrations.install", { agent: i.agent }); if (r) {
                        integrations.reload("");
                        showNotes("Installed", r.notes ?? []);
                    } } },
                i.active && { label: "Uninstall", danger: true, run: async () => { const r = await doHost("integrations.uninstall", { agent: i.agent }); if (r) {
                        integrations.reload("");
                        showNotes("Removed", r.notes ?? []);
                    } } },
            ]) } : {})))));
}
// ── the daemon ──────────────────────────────────────────────────────────────────────────────
const status = new Cache(() => conn.request({ t: "status" }).then((r) => r.status));
function upgradeDaemon() {
    confirm("Switch the daemon to the ptyro on disk?", "Every session keeps running. The app reconnects in a moment.", "Upgrade", async () => {
        try {
            const r = await conn.request({ t: "upgrade", binary: null });
            ui.toast(`Upgrading ${r.from} → ${r.to}`);
            setTimeout(() => status.reload(""), 3000);
        }
        catch (e) {
            hostFail(e);
        }
    }, false);
}
function restartDaemon() {
    confirm("Restart the daemon?", "Running sessions are stopped (agents hibernate and can be resumed). The app reconnects when it's back.", "Restart", async () => { if (await doHost("daemon.restart", {}, "Restarting…"))
        setTimeout(() => status.reload(""), 4000); });
}
function daemonView() {
    poll("daemon", () => status.refresh(""), 5000);
    const s = status.get("");
    const body = pending(status, "") ?? h("div", { class: "stack-in", title: tip("daemon.status") }, facts([["Version", `${s.version} (protocol ${s.proto.major}.${s.proto.minor})`], ["Process", String(s.pid)], ["Up for", ago(s.started_ms)], ["Sessions", `${s.sessions} (${s.live_sessions} running)`], ["Clients", String(s.clients)],
        ["Upgrades", s.generation ? `${s.generation}${s.rolled_back ? " (the last was rolled back)" : ""}` : null],
        ["Memory", s.metrics ? `${Math.round(s.metrics.rss_kib / 1024)} MB · ${s.metrics.open_fds} fds · lag ${s.metrics.loop_lag_ms_max} ms` : null], ["Socket", s.socket], ["Logs", s.log_dir]]), conn.scope === "full" ? h("div", { class: "stack-in" }, bar(h("button", { title: tip("daemon.logs"), onclick: () => showLogs() }, "Logs"), h("button", { title: tip("daemon.upgrade"), onclick: upgradeDaemon }, "Upgrade"), h("button", { title: tip("daemon.restart"), onclick: restartDaemon }, "Restart"), h("button", { class: "deny", title: tip("daemon.stop"), onclick: () => confirm("Stop the daemon?", "Every session is stopped and ptyro goes away, this app with it, until you run ptyro again on the computer.", "Stop it", async () => { if (await doHost("daemon.stop", {}, "Stopping…"))
            ui.toast("Stopped: run ptyro to start it again"); }) }, "Stop")), section("Start at login", h("p", { class: "sub" }, "A systemd or launchd service starts the daemon (and this server) when you log in."), bar(h("button", { title: tip("daemon.service"), onclick: async () => { const r = await doHost("service.install"); if (r)
            showNotes("Login service", r.notes ?? []); } }, "Install the service"), h("button", { title: tip("daemon.unservice"), onclick: async () => { const r = await doHost("service.uninstall"); if (r)
            showNotes("Login service", r.notes ?? []); } }, "Remove it")))) : h("p", { class: "intro" }, "Logs, upgrading and the service need a device paired with --scope full."));
    return h("div", {}, ui.header("Daemon", "#/more"), h("main", { class: "stack" }, body));
}
async function showLogs() {
    const r = await doHost("daemon.logs", { lines: 300 });
    if (!r)
        return;
    const s = textSheet(String(r.path).split("/").pop() ?? "daemon log", r.text);
    requestAnimationFrame(() => { const pre = s.el.querySelector("pre"); if (pre)
        pre.scrollTop = pre.scrollHeight; });
}
// ── first-run setup ─────────────────────────────────────────────────────────────────────────
const setupChecks = new Cache(() => host("doctor", { quick: true }).then((r) => r.checks));
const discovered = new Cache(() => conn.request({ t: "discover", refresh: false }).then((r) => r.paths));
let setupPicks = new Set();
function setupView() {
    const agents = (setupChecks.get("") ?? []).filter((c) => c.section === "agents");
    const repos = (discovered.get("") ?? []).filter((p) => ![...data.projects.values()].some((x) => x.root === p));
    return h("div", {}, ui.header("Set up ptyro", "#/more"), h("main", { class: "stack", title: tip("first.run") }, h("p", { class: "intro" }, "Nothing changes until you tap a button."), section("Your agents", conn.scope !== "full" ? h("p", { class: "sub" }, "Needs a device paired with --scope full.") : pending(setupChecks, "", "Looking…") ?? (agents.length ? h("div", { class: "rows" }, ...agents.map((c) => row(h("span", {}, h("span", { class: `lvl ${(LEVEL[c.level] ?? ["", ""])[1]}` }, (LEVEL[c.level] ?? ["·"])[0]), " ", c.name), c.text))) : h("p", { class: "sub" }, "No agent CLIs found: install claude, codex, gemini or opencode."))), section("Your accounts", h("p", { class: "sub" }, "Several logins on one machine (aliases, ~/.claude-* directories, git identities) can become profiles."), bar(h("button", { onclick: () => void detectProfiles(), disabled: conn.scope !== "full" }, "Find my accounts"))), section("Your repositories", pending(discovered, "", "Looking…") ?? (repos.length ? h("div", { class: "stack-in" }, ...repos.map((p) => { const c = h("input", { type: "checkbox" }); c.checked = setupPicks.has(p); c.addEventListener("change", () => { if (c.checked)
        setupPicks.add(p);
    else
        setupPicks.delete(p); }); return h("label", { class: "field check" }, c, h("span", {}, p.split("/").pop() ?? p), h("small", {}, p)); }), bar(h("button", { class: "primary", onclick: async () => { const n = setupPicks.size; for (const p of setupPicks)
            await ask({ t: "project_add", path: p, workspace: null }); setupPicks = new Set(); discovered.reload(""); ui.toast(`Added ${n}`); } }, "Add the ticked ones")))
        : h("p", { class: "sub" }, data.projects.size ? "Everything found is added already." : "None found under your discover roots: add one by its path on the Projects tab."))), bar(h("button", { class: "primary", onclick: () => go("#/") }, "Done"))));
}
// ── the tour ────────────────────────────────────────────────────────────────────────────────
function tourView() {
    return h("div", {}, ui.header("Tour", "#/more"), h("main", { class: "stack", title: tip("help.tour") }, h("p", { class: "intro" }, "ptyro's daily loop, done for real: start a task, answer the agent, review what it changed, finish it, clean up. It runs in a ptyro of its own, with a demo repository and a pretend agent: your sessions, projects and config are never touched, and it all goes away when you end the tour."), conn.scope !== "full" ? h("p", { class: "empty" }, "Starting a tour needs a device paired with --scope full.") : bar(h("button", { class: "primary", onclick: startTour }, "Start the tour")), h("p", { class: "sub" }, "It opens in a new tab served from this computer (localhost), so open this page from the computer that runs ptyro. The tour's own page has a coach that says what to do next.")));
}
async function startTour() {
    const b = sheet("Starting the tour…", [h("p", { class: "intro" }, "⏳ Making a ptyro of its own, a demo repository and a pretend agent.")]);
    try {
        const r = await host("tour.start");
        b.close();
        if (location.hostname !== "localhost" && location.hostname !== "127.0.0.1" && !location.hostname.startsWith("[")) {
            textSheet("The tour is ready", `It listens on this computer only. Open this on the computer that runs ptyro:\n\n${r.url}`);
        }
        else
            window.open(r.url, "_blank");
    }
    catch (e) {
        b.close();
        hostFail(e);
    }
}
// the coach, in a tour's own page (`ptyro.tour` is set when its pairing link says so)
const STEPS = ["Start a task", "Answer the agent", "Review its change", "Finish it", "Clean up"];
const tour = { step: 0, worktree: "", agent: "", blocked: false };
function advanceTour() {
    for (;;) {
        const before = tour.step;
        const sessions = [...allSessions().values()];
        const agent = sessions.find((s) => s.id === tour.agent);
        if (tour.step === 0) {
            const s = sessions.filter((x) => x.kind === "agent" && x.task && x.worktree && !data.worktrees.get(x.worktree)?.is_root).sort((a, b) => b.created_ms - a.created_ms)[0];
            if (s) {
                tour.agent = s.id;
                tour.worktree = s.worktree;
                tour.step = 1;
            }
        }
        else if (tour.step === 1) {
            const st = agent?.status?.status;
            tour.blocked ||= st === "blocked";
            if ((tour.blocked && (st === "working" || st === "idle" || st === "done")) || ((st === "idle" || st === "done") && (agent?.turns ?? 0) > 0))
                tour.step = 2;
        }
        else if (tour.step === 2) {
            if (location.hash.startsWith(`#/wt/${enc(tour.worktree)}/changes`))
                tour.step = 3;
        }
        else if (tour.step === 3) {
            const w = data.worktrees.get(tour.worktree);
            const st = w?.status;
            if (w && ((w.pr?.state === "MERGED") || (w.integrated && st && (st.staged ?? 0) + (st.modified ?? 0) + (st.untracked ?? 0) === 0 && st.head && w.integrated.startsWith(st.head))))
                tour.step = 4;
        }
        else if (tour.step === 4) {
            if (!data.worktrees.has(tour.worktree))
                tour.step = 5;
        }
        if (tour.step === before)
            break;
    }
}
const COACH = [
    ["Tap ＋ (top right) and say what to do:", "fix the typo in greeting.txt", "Pick “demo · in a new worktree” and Start. It gets a worktree and a branch of its own."],
    ["It wants to run a command and asks first.", "Open its session (Sessions tab) and tap Allow."],
    ["See what it changed: Projects tab → its worktree → Changes.", "Tap a line to leave a comment for the agent."],
    ["Ship it: on the worktree's page tap Finish → Merge into the main branch."],
    ["Clean up: on the worktree's page, ⋯ → Archive (or Remove)."],
    ["That's the loop. Everything you did was real, in a ptyro of its own."],
];
let coachEl = null;
/// Called after every render: the coach shows on a tour's own pages and says what's next.
export function coach() {
    if (localStorage.getItem("ptyro.tour") !== "1" || !token()) {
        coachEl?.remove();
        coachEl = null;
        return;
    }
    advanceTour();
    const lines = COACH[tour.step];
    const el = h("aside", { class: "coach", "aria-live": "polite" }, h("b", {}, tour.step < 5 ? `Tour ${tour.step + 1}/5 · ${STEPS[tour.step]}` : "Tour done"), ...lines.map((l, i) => h("div", { class: i === 1 && tour.step === 0 ? "say" : "" }, l)), h("button", { class: tour.step === 5 ? "primary small" : "small", onclick: () => confirm("End the tour?", "Its ptyro, repository and agent are deleted. Your own ptyro is untouched.", "End the tour", async () => { if (await doHost("tour.end")) {
            localStorage.removeItem("ptyro.tour");
            ui.toast("The tour is over");
            setTimeout(() => window.close(), 800);
        } }, false) }, "End the tour"));
    if (coachEl)
        coachEl.replaceWith(el);
    else
        document.body.append(el);
    coachEl = el;
}
views.set("more", () => moreView());
views.set("health", () => healthView());
views.set("usage", () => usageView());
views.set("devices", () => devicesView());
views.set("profiles", () => profilesView());
views.set("profile", profileView);
views.set("config", () => configView());
views.set("integrations", () => integrationsView());
views.set("daemon", () => daemonView());
views.set("setup", () => setupView());
views.set("tour", () => tourView());
