// Settings and health (ROADMAP WEB1.6): the More tab: this device, the doctor's findings with
// their fixes, usage, devices and pairing another, profiles and their logins, the config file,
// agent integrations, the daemon, first-run setup and the tour.

import { conn, host, store, token } from "./conn.js";
import { aboutDoc, aboutText, data } from "./data.js";
import { fixLineEndings } from "./projects.js";
import { actions, ago, ask, badge, bar, Cache, confirm, download, enc, errText, facts, form, go, h, menu, pending, poll, row, section, sheet, textSheet, ui, views, webHref } from "./kit.js";
import type { Item } from "./kit.js";
import { qrMatrix } from "./qr.js";
import { adminSteps, bannerWants, isHere, moveOffer, movesOf, moveWords, noAgent, retryOf, runningAgent, selfUpdate, setupOffer, SetupLog, updateOffer, viaWords, waitsForAdmin, webSteps } from "./setupkit.js";
import type { MoveOffer } from "./setupkit.js";
import type { UpdateOffer } from "./setupkit.js";
import { allSessions } from "./sessions.js";

const ACTIONS = {
  "more": "Everything beyond the lists: health, devices, profiles, the config, the daemon",
  "health": "What's wrong or worth knowing, from ptyro's checks, each with its fix",
  "usage": "How much of each account's limits is used",
  "notify.test": "Send a test notification through every channel you set up",
  "devices.list": "Devices paired with ptyro, what each may do, when it was last here",
  "devices.revoke": "Unpair a device: its token stops working at once",
  "pair.device": "Make a pairing link for another device",
  "pair.open": "Open the web app in the computer's own browser, paired (inside WSL: the Windows browser)",
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
  "app.about": "About ptyro: its version, repository and licence, where its files live, the daemon and this app's server, and a block to copy into a bug report",
  "daemon.status": "The daemon: version, uptime, sessions, health",
  "daemon.logs": "The daemon's log, the latest lines",
  "daemon.upgrade": "Switch the daemon to the ptyro on disk, keeping every session",
  "daemon.restart": "Stop and start the daemon (sessions are stopped; agents hibernate)",
  "daemon.stop": "Stop every session and the daemon",
  "backup.create": "Save ptyro's own state (config, database, scratchpad) to a private .tar.gz in your home directory on the computer",
  "uninstall.plan": "What uninstalling ptyro would remove and what stays, and the command to run on the computer (it can't be done from here)",
  "daemon.service": "Start ptyro at login (a systemd or launchd service, or a Windows logon task)",
  "daemon.unservice": "Remove the login service",
  "web.status": "The web app's server: whether it is up, its address, and who started it",
  "web.on": "Have the daemon run the web app's server, now and whenever it starts",
  "web.off": "Stop the daemon running the web app's server (paired devices stay paired)",
  "web.logs": "What the web app's server has said, the latest lines",
  "first.run": "First-run setup: your agents, accounts and repositories",
  "setup.plan": "The setup checklist (Settings → Agents and tools, or More → Set up ptyro): Agents, Tools and Already here, each with what it is, how it installs and what comes with it",
  "setup.banner": "No coding agent on the computer yet: the checklist installs one, with what it needs, then logs you in",
  "setup.admin": "A step that needs administrator rights runs only on the computer itself (the owner's exception): the command to run there, and the TUI's checklist that does it",
  "setup.running": "One setup run at a time: while one is going, from here or elsewhere, this page shows its progress instead of starting another",
  "setup.install": "Install the ticked agents and tools on the computer, with what they need first; a step that needs administrator rights shows the command to run there",
  "setup.where": "On Windows with WSL: where each agent or tool goes, into WSL's distro, onto Windows, or both (tick either column)",
  "setup.login": "Log in to each agent and the GitHub CLI, one account each: the tool's own sign-in in a session here, confirmed by its own status command (ptyro never sees the credentials)",
  "updates.update": "Update an agent that has a newer version out, on the computer, the way it was installed: the command it runs is shown first, and sessions running it keep the old version until they restart",
  "updates.retry": "An update Windows (or the system) refused because the agent is running: end the sessions running it, then try again from here",
  "updates.self": "Update ptyro itself on the computer: the release is downloaded, checked against its SHA256SUMS and put in place, then the daemon is upgraded live (every session carries on); on Windows with WSL both halves. A binary only an administrator can replace shows the command to run there",
  "setup.move": "Move an agent installed another way (npm, Homebrew, WinGet) to the vendor's recommended installer: the new one is installed and checked first, then the old copy is removed; settings and logins stay. Tick it in the checklist, or from its Health finding",
  "setup.keep": "Keep an agent installed another way as it is: the move isn't offered again, and it updates the way it's installed (Settings → Updates → keep_installer)",
  "setup.offer": "An agent or tool that isn't installed (a chat, a new task, a Health finding) offers setup: the checklist opens with it ticked, to install on the computer",
  "updates.check": "Look for newer versions of your agents now (ptyro looks once a day by itself; Settings → Updates turns that off or skips one)",
  "help.tour": "A tour: ptyro's daily loop with a pretend agent, in a throwaway ptyro of its own",
} as const;
const tip = actions(ACTIONS);

const LEVEL: Record<string, [string, string]> = { ok: ["✓", "ok"], note: ["·", ""], warn: ["!", "warn"], problem: ["✗", "bad"] };
const SCOPES: [string, string, string][] = [
  ["read", "Read", "Watch sessions, screens and the inbox"],
  ["approve", "Approve", "Also answer prompts and gates"],
  ["control", "Control", "Also type into sessions and start configured agents"],
  ["full", "Full", "Everything: any command, the config, this page"],
];

const hostFail = (e: unknown): void => ui.toast(errText(e));
/// A host operation, with its failure in a toast.
async function doHost(op: string, args: Record<string, unknown> = {}, ok?: string): Promise<any | undefined> {
  try { const r = await host(op, args); if (ok) ui.toast(ok); return r; } catch (e) { hostFail(e); return undefined; }
}
const showNotes = (title: string, notes: string[]): void => { if (notes.length <= 1) ui.toast(notes[0] ?? title); else textSheet(title, notes.join("\n")); };

// ── More ────────────────────────────────────────────────────────────────────────────────────

function moreView(): HTMLElement {
  const scope = SCOPES.find(([k]) => k === conn.scope);
  const items: [string, string, string, string?][] = [
    ["Review queue", "Worktrees waiting for you", "#/review"],
    ["Inbox", "What happened while you were away", "#/inbox"],
    ["Health", "What's wrong, with its fixes", "#/health"],
    ["Usage", "Your accounts' limits", "#/usage"],
    ["Devices", "Phones and browsers paired with ptyro", "#/devices"],
    ["Machines", "Other machines whose workspaces show here", "#/machines"],
    ["Profiles", "Accounts agents run under", "#/profiles"],
    ["Settings", "Every option, searchable, with what it does", "#/settings"],
    ["Integrations", "Agents that need installing once", "#/integrations"],
    ["Web app", "The server your phone opens: up, its address, on or off", "#/web"],
    ["Daemon", "Version, logs, restart, login service", "#/daemon"],
    ["About", "Version, where to report a problem, where things live", "#/about"],
    ["Set up ptyro", "Install agents and tools; your accounts and repositories", "#/setup"],
    ["Take the tour", "ptyro's daily loop with a pretend agent", "#/tour"],
  ];
  return h("div", {}, ui.header("More", false), h("main", { class: "stack", title: tip("more") },
    section("This device", facts([
      ["Access", scope ? h("span", {}, badge(scope[1], conn.scope === "full" ? "ok" : ""), " ", scope[2]) : conn.scope],
      ["Connection", conn.state],
      ...(conn.machine ? [["Machine", conn.machine] as [string, string]] : []),
    ]), bar(
      h("button", { onclick: () => menu("Notifications", [
        { label: "Turn them on for this device", hint: "A test is sent", run: () => ui.push() },
        { title: tip("notify.test"), label: "Send a test through every channel", hint: "This device, desktop notifications, anything in your config", run: async () => { const r = await ask({ t: "notify_test" }); if (r) textSheet("Notification test", (r.results as string[]).join("\n") || "Nothing is set up to notify."); } },
      ]) }, "🔔 Notifications"),
      h("button", { class: "deny", onclick: () => confirm("Forget this device?", "It stops showing your sessions until you pair it again. `ptyro devices revoke` ends the pairing for good.", "Forget", () => ui.forget()) }, "Forget this device"))),
    conn.scope !== "full" ? h("p", { class: "intro" }, "Some pages here change the machine and need a device paired with ", h("code", {}, "ptyro pair --scope full"), ".") : null,
    h("div", { class: "rows" }, ...items.map(([t, s, href]) => row(t, s, "›", { href }))),
    // The version, quietly, where people look first (UX2.1).
    h("p", { class: "sub more-version" }, "ptyro ", status.get("")?.version ?? "", " · ", h("a", { href: "#/about", title: tip("app.about") }, "About"))));
}

// ── health ──────────────────────────────────────────────────────────────────────────────────

const health = new Cache<any[]>(() => host("doctor", { quick: false }).then((r) => r.checks));

function fixesFor(c: any): Item[] {
  const out: Item[] = [];
  const prof = String(c.section).match(/^profile (.+)$/);
  if (prof && ["claude", "codex", "gemini", "opencode", "gh"].includes(c.name) && c.level !== "ok") out.push({ label: `Log in to ${c.name} as ${prof[1]}`, run: () => profileLogin(prof[1], c.name) });
  if (c.section === "ptyro" && c.name === "config mode") out.push({ label: "Make it private (mode 600)", hint: "Saves it again as it is", run: async () => { const g = await doHost("config.get"); if (g && await doHost("config.save", { text: g.text }, "Saved: only you can read it")) health.reload(""); } });
  if (c.section === "ptyro" && c.name.startsWith("config")) out.push({ label: "Edit the config", run: () => go("#/config") });
  if (c.section === "remote" || /restart/.test(c.text)) out.push({ label: "Restart ptyro", run: () => restartDaemon() });
  // An upgrade that was called off is fixed by a restart (agents hibernate and resume), not by
  // trying the same upgrade again.
  if (c.section === "daemon" && /called off/i.test(c.text)) out.push({ label: "Restart the daemon", hint: "Agents resume their conversations; shells and commands start again", run: () => restartDaemon() });
  else if (c.section === "daemon" && /binary|upgrade|older|newer/i.test(c.text)) out.push({ label: "Upgrade the daemon", run: () => upgradeDaemon() });
  if (c.section === "wsl" && c.name === "survival" && /hold_wsl is off/.test(c.text)) out.push({ label: "Keep WSL running while ptyro runs", hint: "Settings → daemon → hold_wsl", run: () => go("#/settings/daemon") });
  if (c.section === "terminal" && c.name === "windows terminal" && c.level !== "ok") out.push({ label: "Change ptyro's prefix", hint: "Or remove the binding in Windows Terminal", run: () => go("#/settings/interface") });
  if (c.section === "wsl" && c.name === "network") out.push({ label: "Change who can reach it", hint: "Tailscale reaches a phone through WSL", run: () => go("#/settings/remote") });
  if (c.section === "integrations") out.push({ label: "Integrations", run: () => go("#/integrations") });
  if (c.section === "scripts" && /line-endings/.test(c.text)) {
    const p = [...data.projects.values()].find((x) => x.slug === c.name);
    if (p) out.push({ label: "Rewrite them with LF", hint: "In every checkout", run: async () => { if (await fixLineEndings(p.id, p.slug)) health.reload(""); } });
  }
  if (c.section === "worktrees" && c.name === "stale" && c.level !== "ok") out.push({ label: "Clean up", hint: "Archive, remove or keep each", run: () => go("#/gc") });
  if (c.section === "worktrees" && c.name === "archived") out.push({ label: "Archived", run: () => go("#/archives") });
  // Newer versions (BOOT5): the update, with its command; and looking again now.
  const upd = updateOffer(c);
  const own = upd && selfUpdate(upd);
  if (upd && own) out.push({ title: tip("updates.self"), label: `Update ptyro to ${upd.latest}`, hint: own.kind === "run" ? "Then the daemon upgrades live" : own.kind === "command" ? "On the computer, as administrator" : `Through ${own.how}`, run: () => askSelfUpdate(upd) });
  else if (upd) out.push({ title: tip("updates.update"), label: `Update ${upd.name} to ${upd.latest}`, hint: upd.command ? `Runs ${upd.command}` : "Its vendor's update", run: () => askUpdate(upd) });
  // Installed another way (BOOT7): the move to the recommended installer, or keep it as it is.
  const mv = moveOffer(c);
  if (mv) {
    out.push({ title: tip("setup.move"), label: `Move ${mv.name} to ${viaWords(mv.to)}`, hint: mv.admin ? "On the computer: removing the old copy needs administrator rights" : "The new one first, then the old removed", run: () => askMove(mv) });
    out.push({ title: tip("setup.keep"), label: `Keep ${mv.name} with ${viaWords(mv.from)}`, hint: "Not offered again", run: () => void keepInstaller(mv.program, mv.name, mv.from) });
  }
  // An agent or tool that isn't installed (BOOT9): the checklist, with it ticked.
  const picks = ["agents", "git"].includes(c.section) ? setupOffer(c.text) : null;
  if (picks) out.push({ title: tip("setup.offer"), label: picks.length ? `Set up ${picks.join(", ")}` : "Set up agents and tools", hint: "The checklist, to install on the computer", run: () => openSetup(picks) });
  if (c.section === "updates") {
    out.push({ title: tip("updates.check"), label: "Look for newer versions now", run: () => void checkUpdates() });
    out.push({ label: "Update settings", hint: "Turn the daily look off, or skip an agent or a version", run: () => go("#/settings/updates") });
  }
  return out;
}

/// Ask before updating (BOOT5): what runs, and which sessions keep the old version until they
/// restart. An update that needs administrator rights shows its command instead (the owner's
/// exception: a password never goes through the web).
function askUpdate(o: UpdateOffer): void {
  const running = runningAgent(allSessions().values(), o.id);
  const n = running.length;
  confirm(`Update ${o.name}?`, h("span", {},
    `${o.name} ${o.version} → ${o.latest}. On the computer it runs `, h("code", {}, o.command ?? "the vendor's update"),
    o.admin ? ", which asks for administrator rights: the next page gives the command to run there." : ".",
    n ? ` ${n === 1 ? "1 session runs" : `${n} sessions run`} ${o.name} (${running.join(", ")}): ${n === 1 ? "it keeps" : "they keep"} ${o.version} until restarted.` : ""),
  // Installed another way: updated that way, never moved by an update (BOOT7: a run moves only
  // what it names in `moves`).
  "Update", () => startUpdate(o.program), false);
}

/// The move to the recommended installer (BOOT7), asked first. One whose old copy only an
/// administrator can remove shows the command to run on the computer (the owner's exception).
function askMove(m: MoveOffer): void {
  if (m.admin) {
    const cmd = `ptyro install ${m.program}`;
    sheet(`Move ${m.name} on the computer`, [
      h("p", { class: "intro" }, moveWords(m), " Removing the old copy needs administrator rights, and a password never goes through this page. On the computer, run:"),
      h("div", { class: "setup-cmd" }, h("code", {}, cmd), copyButton(cmd)),
      h("p", { class: "sub" }, "Or in ptyro there: press the prefix, then U, and tick it."),
    ], []);
    return;
  }
  confirm(`Move ${m.name}?`, h("span", {}, moveWords(m)), "Move", () => { installKey = ""; go("#/setup"); void startInstall([m.program], null, [m.program]); }, false);
}

/// Keep an agent installed another way as it is (BOOT7): the computer's `updates.keep_installer`.
async function keepInstaller(program: string, name: string, from: string): Promise<void> {
  if (!await doHost("updates.keep", { name: program }, `${name} stays installed with ${viaWords(from)}: it updates that way, and the move isn't offered again`)) return;
  health.reload("");
  setupPlan.reload();
}

/// ptyro's own update (BOOT6): asked first, then `ptyro update ptyro --yes` in a session on the
/// computer, which this page opens (its progress live; the daemon's live upgrade at its end keeps
/// the session, and this page reconnects). For an administrator's binary, the command to run
/// there; for a package manager's, how it updates.
function askSelfUpdate(o: UpdateOffer): void {
  const how = selfUpdate(o);
  if (!how) return;
  if (how.kind === "managed") {
    textSheet("Update ptyro", `ptyro ${o.version} → ${o.latest}: it was installed with ${how.how}, so update it that way. ptyro never replaces a binary a package manager owns.`);
    return;
  }
  if (how.kind === "command") {
    sheet("Update ptyro on the computer", [
      h("p", { class: "intro" }, `ptyro ${o.version} → ${o.latest}. ${o.path ?? "Its binary"} can only be replaced as administrator, and a password never goes through this page. On the computer, run:`),
      how.command ? h("div", { class: "setup-cmd" }, h("code", {}, how.command), copyButton(how.command)) : null,
      h("p", { class: "sub" }, "Or in a terminal there: ", h("code", {}, "ptyro update ptyro"), " (it asks for the password), or in ptyro press the prefix, then u."),
    ], []);
    return;
  }
  confirm(`Update ptyro to ${o.latest}?`, h("span", {},
    `ptyro ${o.version} → ${o.latest} on the computer: it ${how.what}, then upgrades the daemon live. Every session carries on, and this page reconnects in a moment. On Windows with WSL, the other half is brought to ${o.latest} too.`),
  "Update", () => void startSelfUpdate(), false);
}

async function startSelfUpdate(): Promise<void> {
  const r = await doHost("updates.self");
  if (!r) return;
  ui.toast("Updating ptyro on the computer: its progress is here; Health looks again when you go back");
  health.reload("");
  go(`#/s/${enc(r.session)}`);
}

/// Run the update on the computer (the daemon's `SetupRun` of that agent: an outdated one runs its
/// update) and show its progress on the setup page, which follows the run.
function startUpdate(program: string): void {
  installKey = "";
  go("#/setup");
  void startInstall([program], null);
}

/// Look for newer versions now (`ptyro update --check`), then show what's out of date.
async function checkUpdates(): Promise<void> {
  ui.toast("Looking for newer versions…");
  const r = await doHost("updates.check");
  if (!r) return;
  const out = (r.outdated as any[] ?? []).filter((o) => !(r.ignored as string[] ?? []).includes(o.id));
  const failed = Object.keys(r.failed ?? {}).length;
  ui.toast(out.length ? `${out.map((o) => `${o.name} ${o.latest}`).join(", ")} ${out.length === 1 ? "is" : "are"} out: Health offers ${out.length === 1 ? "it" : "each"}` : failed ? "Couldn't look for every newer version (offline?): ptyro looks again tomorrow" : "Your agents are up to date");
  health.reload("");
  setupPlan.reload();
}

function healthView(): HTMLElement {
  const cs = health.get("");
  const body = pending(health, "", "Checking everything (a few seconds)…") ?? (() => {
    const bad = cs!.filter((c) => c.level === "problem").length, warn = cs!.filter((c) => c.level === "warn").length;
    const sections = [...new Set(cs!.map((c) => c.section as string))];
    return h("div", { class: "stack-in" },
      h("p", { class: "pr-head" }, bad || warn ? `${bad} problem${bad === 1 ? "" : "s"}, ${warn} warning${warn === 1 ? "" : "s"}` : "All good"),
      ...sections.map((sec) => section(sec, h("div", { class: "rows" }, ...cs!.filter((c) => c.section === sec).map((c) => {
        const [mark, kind] = LEVEL[c.level] ?? ["·", ""];
        const fixes = fixesFor(c);
        return row(h("span", {}, h("span", { class: `lvl ${kind}` }, mark), " ", c.name), c.text, fixes.length ? "⋯" : null,
          { cls: "wrap", onclick: fixes.length ? () => menu(`${c.name}`, fixes) : undefined });
      })))));
  })();
  return h("div", {}, ui.header("Health", "#/more", h("button", { class: "icon", title: tip("health"), "aria-label": "Check again", onclick: () => health.reload("") }, "↻")),
    h("main", { class: "stack", title: tip("health") }, conn.scope !== "full" ? h("p", { class: "empty" }, "Checking the machine needs a device paired with --scope full.") : body));
}

// ── usage ───────────────────────────────────────────────────────────────────────────────────

const usage = new Cache<any[]>(() => conn.request({ t: "usage" }).then((r) => r.usage));

function usageView(): HTMLElement {
  poll("usage", () => usage.refresh(""), 15000);
  const list = usage.get("");
  const body = pending(usage, "") ?? (list!.length ? h("div", { class: "stack-in" }, ...list!.map((u) => h("section", { class: "meter" },
    h("div", { class: "meter-head" }, h("b", {}, `${u.agent}`), " ", h("span", { class: "sub" }, u.profile), u.limited_until_ms ? badge(`limited until ${new Date(u.limited_until_ms).toLocaleTimeString()}`, "bad") : null),
    ...(u.windows as any[]).map((w) => h("div", { class: "meter-row" }, h("span", {}, w.name), h("div", { class: "bar-track" }, h("div", { class: `bar-fill ${w.used_percent >= 90 ? "bad" : w.used_percent >= 70 ? "warn" : ""}` }, "")), h("span", {}, `${w.used_percent}%`), w.resets_at_ms ? h("span", { class: "sub" }, `resets ${new Date(w.resets_at_ms).toLocaleTimeString()}`) : null)),
    u.context_percent != null ? h("div", { class: "sub" }, `context ${u.context_percent}%`) : null,
    u.cost_millis != null ? h("div", { class: "sub" }, `$${(u.cost_millis / 1000).toFixed(2)} this session`) : null)))
    : h("p", { class: "empty" }, "No usage yet: it shows once an agent reports its limits."));
  // Set the bars' widths (an inline style attribute would break the CSP; the CSSOM is fine).
  requestAnimationFrame(() => document.querySelectorAll<HTMLElement>(".meter-row").forEach((r, i) => { void i; const pct = parseInt(r.children[2]?.textContent ?? "0", 10); const f = r.querySelector<HTMLElement>(".bar-fill"); if (f) f.style.width = `${pct}%`; }));
  return h("div", {}, ui.header("Usage", "#/more"), h("main", { class: "stack", title: tip("usage") }, body));
}

// ── devices ─────────────────────────────────────────────────────────────────────────────────

const devices = new Cache<any[]>(() => conn.request({ t: "device_list" }).then((r) => r.devices));

/// What the default scope means, in plain words (the same text `ptyro pair` prints).
const FULL_WARNING = "This device gets FULL control: it can run commands as you, change settings, and pair or revoke other devices. Pair only your own phone or browser. Pick Approve or Control below for less.";

function pairAnother(): void {
  form("Pair another device", [
    { key: "name", label: "Name", value: "phone", required: true, hint: "A name that exists is replaced: its old link stops working and it takes the access chosen here." },
    { key: "scope", label: "May", type: "select", options: SCOPES.map(([k, l, d]): [string, string] => [k, `${l}: ${d}`]), value: "full" },
  ], "Make a link", async (v) => {
    const r = await conn.request({ t: "device_pair", name: v.name, scope: v.scope });
    const link = `${location.origin}/#pair=${r.code}`;
    const old = r.replaces as { name: string; scope: string } | undefined;
    const s = sheet(`Pair ${v.name}${conn.machine ? ` with ${conn.machine}` : ""}`, [
      h("p", { class: "intro" }, `Open this on the device (or scan the code). It works once, for ${Math.round((r.expires_ms - Date.now()) / 60000)} minutes.`),
      old ? h("p", { class: "intro" }, `Replaces the existing device ${old.name} (was ${old.scope}, now ${v.scope}) when this link is opened: its old link and token stop working.`) : null,
      v.scope === "full" ? h("p", { class: "form-err" }, FULL_WARNING) : null,
      /^(localhost|127\.|\[::1\])/.test(location.hostname) ? h("p", { class: "form-err" }, "This address only reaches this computer. Open this page by its network address (the one `ptyro pair` prints) to make a link another device can use.") : null,
      qrBox(link),
      h("pre", { class: "block" }, link),
    ], [
      h("button", { onclick: () => void navigator.clipboard?.writeText(link).then(() => ui.toast("Copied"), () => ui.toast("Couldn't copy")) }, "Copy"),
      "share" in navigator ? h("button", { onclick: () => void navigator.share({ title: "ptyro", url: link }).catch(() => {}) }, "Share…") : null,
      h("button", { class: "primary", onclick: () => { s.close(); devices.reload(""); } }, "Done"),
    ]);
    devices.reload("");
  }, { intro: h("span", { class: "form-err" }, FULL_WARNING) });
}

/// `ptyro pair --open` on the computer: its own browser (inside WSL, Windows') opens the web app,
/// paired as "browser".
async function openOnComputer(): Promise<void> {
  const r = await doHost("pair.open", {}, "Opening…");
  if (!r) return;
  const said = r.opened ? "Opened in the computer's browser." : `Nothing on the computer opens a browser; open ${r.url} there.`;
  showNotes("Open on the computer", [said, ...((r.notes as string[] | undefined) ?? [])]);
  devices.reload("");
}

function devicesView(): HTMLElement {
  poll("devices", () => devices.refresh(""), 10000);
  const list = devices.get("");
  return h("div", {}, ui.header("Devices", "#/more", h("button", { class: "icon plus", title: tip("pair.device"), "aria-label": "Pair another device", onclick: pairAnother }, "＋")),
    h("main", { class: "stack", title: tip("devices.list") }, conn.scope !== "full" ? h("p", { class: "empty" }, "Listing devices needs a device paired with --scope full.") :
      pending(devices, "") ?? (list!.length ? h("div", { class: "rows" }, ...list!.map((d) => row(
        h("span", {}, d.name, " ", badge(d.scope, d.scope === "full" ? "warn" : ""), d.pending ? badge("waiting to pair", "info") : null),
        `${d.last_seen_ms ? `seen ${ago(d.last_seen_ms)} ago` : "never seen"}${d.last_via ? ` · ${d.last_via}` : ""}${d.push ? ` · ${d.push} notification${d.push === 1 ? "" : "s"}` : ""}`, null,
        { onclick: () => menu(d.name, [{ title: tip("devices.revoke"), label: "Revoke", danger: true, hint: "Its token stops working at once", run: () => confirm(`Revoke ${d.name}?`, "It's unpaired now. If it's this device, you'll be signed out.", "Revoke", async () => { await ask({ t: "device_revoke", id: d.id }, "Revoked"); devices.reload(""); }) }]) }))) : h("p", { class: "empty" }, "No devices."))));
}

// a QR code, drawn here (qr.ts, no library)
function qrBox(text: string): HTMLElement {
  const m = qrMatrix(text);
  if (!m) return h("p", { class: "sub" }, "(too long for a code: use the link)");
  const n = m.length, px = 4, quiet = 4;
  const c = h("canvas", { class: "qr", "aria-label": "QR code of the link" });
  const size = (n + quiet * 2) * px;
  c.width = size; c.height = size;
  const ctx = c.getContext("2d");
  if (ctx) {
    ctx.fillStyle = "#fff"; ctx.fillRect(0, 0, size, size);
    ctx.fillStyle = "#000";
    for (let y = 0; y < n; y++) for (let x = 0; x < n; x++) if (m[y][x]) ctx.fillRect((x + quiet) * px, (y + quiet) * px, px, px);
  }
  return c;
}

// ── profiles ────────────────────────────────────────────────────────────────────────────────

const profiles = new Cache<any[]>(() => conn.request({ t: "profile_list" }).then((r) => r.profiles));
const AGENT_DIRS = ["claude", "codex", "gemini", "opencode", "gh"];

async function profileLogin(name: string, target: string): Promise<void> {
  const r = await doHost("profile.login", { name, target });
  if (r) { ui.toast(`Logging in to ${target} as ${name}: the tool's own flow; ptyro never sees the credentials`); go(`#/s/${enc(r.session)}`); }
}

function profileForm(): void {
  form("New profile", [
    { key: "name", label: "Name", required: true, slug: "name", placeholder: "work", hint: "A space or other odd character becomes _ (a-z 0-9 . _ -)" },
    { key: "label", label: "Label", placeholder: "shown in the sidebar" },
    { key: "color", label: "Colour", placeholder: "e.g. orange or #ff8800" },
    ...AGENT_DIRS.map((a) => ({ key: `dir_${a}`, label: `${a} directory`, placeholder: `adopt one you have, e.g. ~/.${a}-work`, hint: undefined }) as const),
    { key: "git_name", label: "Git name" }, { key: "git_email", label: "Git email" }, { key: "ssh_key", label: "SSH key", placeholder: "~/.ssh/id_work" },
    { key: "matches", label: "Matches remotes", placeholder: "github.com:acme/*", hint: "Pins matching projects to it; comma separated" },
  ], "Create", async (v) => {
    const adopt: Record<string, string> = {};
    for (const a of AGENT_DIRS) if (v[`dir_${a}`]) adopt[a] = String(v[`dir_${a}`]);
    const r = await host("profile.new", { name: v.name, label: v.label, color: v.color, adopt, git_name: v.git_name, git_email: v.git_email, ssh_key: v.ssh_key, matches: String(v.matches ?? "").split(",").map((s) => s.trim()).filter(Boolean) });
    profiles.reload("");
    showNotes("Profile added", r.notes ?? []);
  });
}

async function detectProfiles(): Promise<void> {
  const r = await doHost("profile.detect");
  if (!r) return;
  const found = (r.found as any[]);
  if (!found.length) { ui.toast("No multi-account setup found (aliases, ~/.claude-* dirs, git includes)"); return; }
  const picks = new Map<string, HTMLInputElement>();
  const s = sheet("Accounts found", [
    h("p", { class: "intro" }, "Already set up on this machine. Tick the ones to add as profiles; your rc files aren't touched."),
    ...found.map((f) => { const c = h("input", { type: "checkbox" }); c.checked = !f.known; c.disabled = f.known; picks.set(f.profile, c);
      return h("label", { class: "field check" }, c, h("span", {}, `${f.profile}${f.known ? " (already defined)" : ""}`), h("small", {}, `from ${(f.sources as string[]).join(", ")}`)); }),
  ], [h("button", { onclick: () => s.close() }, "Cancel"), h("button", { class: "primary", onclick: async () => {
    const chosen = [...picks].filter(([, c]) => c.checked && !c.disabled).map(([p]) => p);
    if (!chosen.length) { s.close(); return; }
    s.close();
    const x = await doHost("profile.detect_apply", { profiles: chosen });
    if (x) { profiles.reload(""); showNotes("Added", x.notes ?? []); }
  } }, "Add them")]);
}

function profilesView(): HTMLElement {
  const list = profiles.get("");
  return h("div", {}, ui.header("Profiles", "#/more",
    h("button", { class: "icon", title: tip("profile.detect"), "aria-label": "Find my accounts", onclick: () => void detectProfiles() }, "🔍"),
    h("button", { class: "icon plus", title: tip("profile.new"), "aria-label": "New profile", onclick: profileForm }, "＋")),
    h("main", { class: "stack", title: tip("profile.list") }, pending(profiles, "") ?? h("div", { class: "rows" }, ...list!.map((p) => row(
      h("span", {}, p.label || p.name, p.label && p.label !== p.name ? h("small", {}, ` ${p.name}`) : null, p.defined ? null : badge("implicit")),
      [p.git_email, p.isolate ? "isolated" : ""].filter(Boolean).join(" · ") || Object.keys(p.dirs).join(", "), "›", { href: `#/profile/${enc(p.name)}` })))));
}

function profileView(rest: string): HTMLElement {
  const name = decodeURIComponent(rest);
  const p = profiles.get("")?.find((x) => x.name === name);
  const body = pending(profiles, "") ?? (!p ? h("p", { class: "empty" }, "No such profile.") : h("div", { class: "stack-in", title: tip("profile.show") },
    facts([["Label", p.label], ["Colour", p.color], ["Isolated", p.isolate ? "yes: credentials it inherits are removed" : "no: it inherits the daemon's"], ["Git", p.git_email],
      ["Matches remotes", (p.matches?.remotes ?? []).join(", ")], ["Matches paths", (p.matches?.paths ?? []).join(", ")], ["Variables", (p.vars as string[]).join(" ")]]),
    section("Directories", facts(Object.entries(p.dirs).map(([a, d]) => [a, h("code", {}, String(d))] as [string, Node]))),
    section("Log in", h("div", { class: "bar", title: tip("profile.login") }, ...AGENT_DIRS.map((a) => h("button", { onclick: () => void profileLogin(name, a) }, a)))),
    bar(
      h("button", { title: tip("profile.doctor"), onclick: async () => { const r = await doHost("profile.doctor", { name }); if (r) textSheet(`${name}: checks`, (r.checks as any[]).map((c) => `${(LEVEL[c.level] ?? ["·"])[0]} ${c.name}: ${c.text}`).join("\n") || "Nothing to check."); } }, "Check"),
      h("button", { title: tip("profile.sync"), onclick: async () => { const r = await doHost("profile.sync", { name }); if (r) textSheet(`${name}: shared files`, (r.notes as string[]).join("\n")); } }, "Sync shared files"),
      name !== "default" ? h("button", { title: tip("profile.rename"), onclick: () => form("Rename the profile", [{ key: "new", label: "New name", value: name, required: true, slug: "name", hint: "A space or other odd character becomes _ (a-z 0-9 . _ -)" }], "Rename", async (v) => { const r = await host("profile.rename", { old: name, new: v.new }); profiles.reload(""); go(`#/profile/${enc(String(v.new))}`); showNotes("Renamed", r.notes ?? []); }) }, "Rename") : null,
      name !== "default" ? h("button", { class: "deny", title: tip("profile.remove"), onclick: () => removeProfile(name) }, "Remove") : null)));
  return h("div", {}, ui.header(p?.label || name, "#/profiles"), h("main", { class: "stack" }, body));
}

function removeProfile(name: string, force = false): void {
  confirm(`Remove ${name}?`, force ? "Whatever uses it falls back to the default profile." : "It leaves your config; its directories aren't touched.", force ? "Remove anyway" : "Remove", async () => {
    try { const r = await host("profile.rm", { name, force }); profiles.reload(""); go("#/profiles"); showNotes("Removed", r.notes ?? []); } catch (e) {
      if ((e as { code?: string }).code === "conflict" && !force) confirm("It's in use", errText(e), "Remove anyway", () => removeProfile(name, true)); else hostFail(e);
    }
  });
}

// ── the config ──────────────────────────────────────────────────────────────────────────────

const cfg = new Cache<any>(() => host("config.get"));
let draft: string | null = null;
/// Edits typed in the editor and not saved.
let draftDirty = false;
/// The settings panel (or anything else) changed the file since the editor read it.
let cfgStale = false;

/// The file was changed elsewhere: the editor reads it again next time (and warns when it has
/// edits of its own that would be saved over it).
export function cfgChanged(): void {
  cfgStale = true;
  if (!draftDirty) { draft = null; cfg.reload(""); cfgStale = false; }
}
/// A line of the file to put the cursor on when the editor opens (the settings panel's "in the file").
let atLine: number | null = null;

/// Open the config file's editor at `line` (1-based; 0: the top).
export function editConfigAt(line: number): void {
  atLine = line > 0 ? line : null;
  go("#/config");
}

function configView(): HTMLElement {
  const c = cfg.get("");
  const body = conn.scope !== "full" ? h("p", { class: "empty" }, "Editing the config needs a device paired with --scope full.") : pending(cfg, "") ?? (() => {
    if (draft === null) draft = c.text;
    const behind = cfgStale && draftDirty;
    const ta = h("textarea", { class: "code", rows: 22, spellcheck: "false", autocapitalize: "off", "aria-label": "config.toml" });
    ta.value = draft ?? "";
    const status = h("p", { class: "form-err" });
    ta.addEventListener("input", () => { draft = ta.value; draftDirty = true; status.textContent = ""; });
    if (atLine) {
      const line = atLine;
      atLine = null;
      requestAnimationFrame(() => {
        const at = ta.value.split("\n").slice(0, line - 1).reduce((n, l) => n + l.length + 1, 0);
        ta.focus();
        ta.setSelectionRange(at, at);
        ta.scrollTop = Math.max(0, (line - 4) * 19);
      });
    }
    const check = async (save: boolean) => {
      status.className = "form-err";
      try {
        // `base` is the file as this editor read it: if it has changed since, the save is refused.
        const r = await host(save ? "config.save" : "config.check", save && c.exists ? { text: ta.value, base: c.text } : { text: ta.value });
        status.className = "ok-note"; status.textContent = `${save ? "Saved: it applies at once." : "Looks right."}${(r.notes as string[]).length ? `\n${(r.notes as string[]).join("\n")}` : ""}`;
        if (save) { cfg.set("", { ...c, text: ta.value, exists: true }); draft = null; draftDirty = false; cfgStale = false; }
      } catch (e) {
        status.textContent = (e as { code?: string }).code === "conflict" ? `${errText(e)}\nThe file changed since you opened this editor (from Settings, or by hand): reload it, then make your edit again.` : errText(e);
        if ((e as { code?: string }).code === "conflict") cfgStale = true;
      }
    };
    return h("div", { class: "stack-in" },
      h("div", { class: "sub", title: tip("config.path") }, c.exists ? c.path : `${c.path} (not there yet: saving creates it)`),
      behind ? h("div", { class: "banner warn" }, "The file changed since you started editing (from Settings, or by hand). Saving this replaces those changes. ", h("button", { class: "small", onclick: () => { draft = null; draftDirty = false; cfgStale = false; cfg.reload(""); } }, "Discard my edits and reload")) : null,
      ta, status,
      bar(h("button", { title: tip("config.check"), onclick: () => void check(false) }, "Check"), h("button", { class: "primary", title: tip("config.edit"), onclick: () => void check(true) }, "Save"),
        h("button", { title: tip("config.show"), onclick: async () => { const r = await doHost("config.show"); if (r) textSheet("The effective config", r.text); } }, "Show effective"),
        h("button", { onclick: () => download("config.toml", ta.value) }, "Download")));
  })();
  return h("div", {}, ui.header("config.toml", "#/settings"), h("main", { class: "stack", title: tip("config.edit") }, body));
}

// ── integrations ────────────────────────────────────────────────────────────────────────────

const integrations = new Cache<any[]>(() => host("integrations.ls").then((r) => r.integrations));

function integrationsView(): HTMLElement {
  const list = integrations.get("");
  return h("div", {}, ui.header("Integrations", "#/more"), h("main", { class: "stack", title: tip("integrations") },
    conn.scope !== "full" ? h("p", { class: "empty" }, "This needs a device paired with --scope full.") :
    pending(integrations, "") ?? h("div", { class: "rows" }, ...list!.map((i) => row(h("span", {}, i.agent, " ", badge(i.active ? "active" : "not installed", i.active ? "ok" : "warn")), i.detail, null,
      i.needs_install ? { onclick: () => menu(i.agent, [
        !i.active && { label: "Install", run: async () => { const r = await doHost("integrations.install", { agent: i.agent }); if (r) { integrations.reload(""); showNotes("Installed", r.notes ?? []); } } },
        i.active && { label: "Uninstall", danger: true, run: async () => { const r = await doHost("integrations.uninstall", { agent: i.agent }); if (r) { integrations.reload(""); showNotes("Removed", r.notes ?? []); } } },
      ]) } : {})))));
}

// ── the daemon ──────────────────────────────────────────────────────────────────────────────

const status = new Cache<any>(() => conn.request({ t: "status" }).then((r) => r.status));

// ── about (UX2.1) ───────────────────────────────────────────────────────────────────────────

/// Both asked together: the machine's own document (a `full` device may), and the daemon's status.
const about = new Cache<any>(async () => {
  const st = await conn.request({ t: "status" }).then((r) => r.status).catch(() => null);
  const doc = conn.scope === "full" ? await host("about").catch(() => null) : null;
  return aboutDoc(doc, st);
});

const copyText = (text: string, what = "Copied"): void => void navigator.clipboard?.writeText(text).then(() => ui.toast(what), () => ui.toast("Couldn't copy"));

/// An address as a link, with a button to copy it; a path (nothing to open from a browser) as text with the button.
function aboutValue(v: string, link = false): HTMLElement {
  const href = link ? webHref(v) : null;
  return h("span", { class: "about-v" }, href ? h("a", { href, target: "_blank", rel: "noreferrer" }, v) : h("code", {}, v), " ", h("button", { class: "small", title: "Copy", onclick: () => copyText(v) }, "Copy"));
}

/// Every agent's known limits, per profile, one line each (the meters are on the Usage page).
function usageFacts(): HTMLElement {
  const wait = pending(usage, "", "Looking…");
  if (wait) return wait;
  const list = usage.get("")!;
  const now = Date.now();
  const until = (t: number): string => { const m = Math.floor(Math.max(0, t - now) / 60000); return m >= 60 ? `${Math.floor(m / 60)}h${String(m % 60).padStart(2, "0")}m` : `${m}m`; };
  const line = (u: any): string => [
    u.limited_until_ms > now ? `RATE-LIMITED for ${until(u.limited_until_ms)}` : "",
    ...(u.windows as any[]).map((w) => `${w.name} ${w.used_percent}%${w.resets_at_ms > now ? ` (resets in ${until(w.resets_at_ms)})` : ""}`),
    u.context_percent != null ? `context ${u.context_percent}%` : "",
    u.cost_millis != null ? `session $${(u.cost_millis / 1000).toFixed(2)}` : "",
  ].filter(Boolean).join(" · ") || "no figures reported yet";
  return list.length
    ? h("div", { title: tip("usage") }, facts(list.map((u) => [`${u.agent}@${u.profile}`, line(u)] as [string, string])), h("a", { href: "#/usage" }, "The meters →"))
    : h("p", { class: "sub" }, "None reported yet: Claude reports its limits with a session's first reply; Codex with [agents.codex] usage = true.");
}

function aboutView(): HTMLElement {
  poll("about", () => about.refresh(""), 10_000);
  poll("usage", () => usage.refresh(""), 15000);
  const doc = about.get("");
  const body = pending(about, "", "Looking…") ?? (() => {
    const b = doc.build ?? {}, d = doc.daemon ?? {}, w = doc.web;
    const build = [b.commit && `commit ${b.commit}`, b.date && `of ${b.date}`, b.target].filter(Boolean).join(" ");
    return h("div", { class: "stack-in", title: tip("app.about") },
      bar(h("button", { class: "primary", onclick: () => copyText(aboutText(doc), "Copied: paste it into your bug report") }, "Copy all, for a bug report")),
      d.same === false ? h("p", { class: "banner warn" }, `The daemon is ${d.version} (protocol ${d.protocol}), not ${doc.version}. ${conn.scope === "full" ? "Switch it to this version: every session keeps running." : "Pair this device with --scope full to switch it, or run `ptyro daemon upgrade`."}`, " ",
        conn.scope === "full" ? h("button", { class: "small", title: tip("daemon.upgrade"), onclick: upgradeDaemon }, "Upgrade the daemon") : null) : null,
      section("ptyro", facts([
        ["Version", `${doc.version} (protocol ${doc.protocol})`], ["Build", build], ["System", doc.system],
        ["Source", aboutValue(doc.repository, true)], ["Report a problem", aboutValue(doc.issues, true)], ["Licence", doc.license]])),
      section("Running", facts([
        ["Daemon", d.running ? `running · ${d.version} (protocol ${d.protocol}) · pid ${d.pid} · up ${ago(d.started_ms)}` : "not running (it starts on demand)"],
        ["Holds WSL", d.holds_wsl ?? null],
        ["Web app", w ? (w.text ?? []).join(" · ") : null]])),
      section("Where things live", facts((doc.places ?? []).map((p: any) => [p.name[0].toUpperCase() + p.name.slice(1), aboutValue(p.path)] as [string, HTMLElement]))),
      (doc.tools ?? []).length ? section("Tools", facts((doc.tools as any[]).map((t) => [t.name, t.text] as [string, string]))) : null,
      section("Usage limits", usageFacts()),
      doc.limited ? h("p", { class: "sub" }, "This device can't ask the computer for more (build, the config and data folders, git and the agents). Pair it with ", h("code", {}, "ptyro pair --scope full"), " to see them.") : null);
  })();
  return h("div", {}, ui.header("About", "#/more", h("button", { class: "icon", title: "Check again", "aria-label": "Check again", onclick: () => about.reload("") }, "↻")), h("main", { class: "stack" }, body));
}

function upgradeDaemon(): void {
  confirm("Switch the daemon to the ptyro on disk?", "Every session keeps running. The app reconnects in a moment.", "Upgrade", async () => {
    try { const r = await conn.request({ t: "upgrade", binary: null }); ui.toast(`Upgrading ${r.from} → ${r.to}`); setTimeout(() => status.reload(""), 3000); } catch (e) { hostFail(e); }
  }, false);
}

function restartDaemon(): void {
  confirm("Restart the daemon?", "Running sessions are stopped (agents hibernate and can be resumed). The app reconnects when it's back.", "Restart", async () => { if (await doHost("daemon.restart", {}, "Restarting…")) setTimeout(() => status.reload(""), 4000); });
}

function daemonView(): HTMLElement {
  poll("daemon", () => status.refresh(""), 5000);
  const s = status.get("");
  const body = pending(status, "") ?? h("div", { class: "stack-in", title: tip("daemon.status") },
    facts([["Version", `${s.version} (protocol ${s.proto.major}.${s.proto.minor})`], ["Process", String(s.pid)], ["Up for", ago(s.started_ms)], ["Sessions", `${s.sessions} (${s.live_sessions} running)`], ["Clients", String(s.clients)],
      ["Upgrades", s.generation ? `${s.generation}${s.rolled_back ? " (the last was rolled back)" : ""}` : null],
      ["Holds WSL", s.wsl_hold ? aboutDoc(null, s).daemon.holds_wsl : null],
      ["Memory", s.metrics ? `${Math.round(s.metrics.rss_kib / 1024)} MB · ${s.metrics.open_fds} fds · lag ${s.metrics.loop_lag_ms_max} ms` : null], ["Socket", s.socket], ["Logs", s.log_dir]]),
    conn.scope === "full" ? h("div", { class: "stack-in" },
      bar(
        h("button", { title: tip("daemon.logs"), onclick: () => showLogs() }, "Logs"),
        h("button", { title: tip("daemon.upgrade"), onclick: upgradeDaemon }, "Upgrade"),
        h("button", { title: tip("daemon.restart"), onclick: restartDaemon }, "Restart"),
        h("button", { class: "deny", title: tip("daemon.stop"), onclick: () => confirm("Stop the daemon?", "Every session is stopped and ptyro goes away, this app with it, until you run ptyro again on the computer.", "Stop it", async () => { if (await doHost("daemon.stop", {}, "Stopping…")) ui.toast("Stopped: run ptyro to start it again"); }) }, "Stop")),
      section("Start at login", h("p", { class: "sub" }, "A systemd or launchd service, or on Windows a logon task, starts the daemon (and this server) when you log in."),
        bar(h("button", { title: tip("daemon.service"), onclick: async () => { const r = await doHost("service.install"); if (r) showNotes("Login service", r.notes ?? []); } }, "Install the service"),
          h("button", { title: tip("daemon.unservice"), onclick: async () => { const r = await doHost("service.uninstall"); if (r) showNotes("Login service", r.notes ?? []); } }, "Remove it"))),

      section("Back up or leave", h("p", { class: "sub" }, "A backup holds your config, history, scratchpad and settings, in a private file on the computer. Uninstalling removes ptyro; your repositories stay."),
        bar(h("button", { title: tip("backup.create"), onclick: backupNow }, "Back up ptyro's state"),
          h("button", { class: "deny", title: tip("uninstall.plan"), onclick: showUninstall }, "Uninstall…")))) : h("p", { class: "intro" }, "Logs, upgrading and the service need a device paired with --scope full."));
  return h("div", {}, ui.header("Daemon", "#/more"), h("main", { class: "stack" }, body));
}

/// A backup of ptyro's state, written on the computer (the page can't hold a file that size, and
/// the file belongs where the person looks for it).
async function backupNow(): Promise<void> {
  const r = await doHost("backup.create");
  if (!r) return;
  const mb = (r.bytes / (1024 * 1024)).toFixed(1);
  textSheet("Backed up", `${r.path} (${mb} MB)\n\nIn it:\n${(r.items as string[]).map((i) => `  + ${i}`).join("\n")}\n\nNot in it:\n${(r.left_out as string[]).map((i) => `  - ${i}`).join("\n") || "  (nothing left out)"}\n\nIt is private to you (mode 600). Its README.txt says how to put it back.`);
}

/// What uninstalling would do. It is not done from here: it ends the server this page comes from
/// (and the daemon), and asks to be typed at, so the page shows the plan and the command.
async function showUninstall(): Promise<void> {
  const r = await doHost("uninstall.plan");
  if (!r) return;
  const cmd = String(r.command);
  sheet("Uninstall ptyro", [
    h("p", { class: "intro" }, "This is what it would do. It isn't done from the web app: it stops the daemon and the server this page comes from, and asks you to confirm in a terminal on the computer."),
    h("pre", { class: "block" }, String(r.text)),
    h("p", { class: "sub" }, "To do it, run this on the computer (add --backup FILE to save a backup first; --dry-run shows this again):"),
    h("pre", { class: "block" }, cmd),
  ], [h("button", { onclick: () => void navigator.clipboard?.writeText(cmd).then(() => ui.toast("Copied"), () => ui.toast("Couldn't copy")) }, "Copy the command")]);
}

async function showLogs(): Promise<void> {
  const r = await doHost("daemon.logs", { lines: 300 });
  if (!r) return;
  const s = textSheet(String(r.path).split("/").pop() ?? "daemon log", r.text);
  requestAnimationFrame(() => { const pre = s.el.querySelector("pre"); if (pre) pre.scrollTop = pre.scrollHeight; });
}

// ── the web app's own server (WEB1.9) ───────────────────────────────────────────────────────

const ACCESS: [string, string, string][] = [
  ["tailscale", "My Tailscale network", "This computer and your tailnet (your phone, signed in to Tailscale), not the Wi-Fi"],
  ["open", "Every network", "The Wi-Fi or LAN too, and anything else that reaches this computer. Devices still need a pairing token"],
  ["localhost", "This computer only", "Nothing else can reach it"],
];

async function setWeb(on: boolean, via?: string): Promise<void> {
  try {
    const r = await conn.request({ t: "web", on, via: via || null });
    status.reload("");
    const notes: string[] = r.notes ?? [];
    if (on && r.web.up) ui.toast(`Up at ${r.web.url}`);
    else if (!on) ui.toast("Stopped. Paired devices stay paired.");
    if (notes.length > (on && r.web.up ? 0 : 1)) showNotes(on ? "The web app" : "Stopped", notes);
    else if (notes.length && !(on && r.web.up)) ui.toast(notes[0]);
  } catch (e) { hostFail(e); }
}

/// Who can reach it (`remote.access`): the current one is marked, and picking another applies at once.
function pickAccess(current: string | null, title: string): void {
  const items = ACCESS.map(([v, label, hint]): Item => ({ label: v === current ? `${label} (now)` : label, hint, run: () => void setWeb(true, v) }));
  menu(title, current ? items : [{ label: "Pick for me", hint: "Tailscale if it is up here, else every network", run: () => void setWeb(true) }, ...items]);
}

function turnWebOn(): void {
  pickAccess(null, "Run the web app with the daemon: who can reach it?");
}

function turnWebOff(): void {
  const here = location.origin;
  confirm("Stop the web app's server?", `The daemon stops serving it and won't start it again. If this page is served from ${here}, it goes away with it: turn it back on with \`ptyro web on\` at the computer. Paired devices stay paired.`, "Stop it", () => void setWeb(false), false);
}

async function showWebLogs(): Promise<void> {
  const r = await doHost("web.logs", { lines: 300 });
  if (!r) return;
  const s = textSheet("The web server's log", r.text || "(nothing yet)");
  requestAnimationFrame(() => { const pre = s.el.querySelector("pre"); if (pre) pre.scrollTop = pre.scrollHeight; });
}

function webView(): HTMLElement {
  poll("web", () => status.refresh(""), 4000);
  const st = status.get("");
  const w = st?.web;
  const full = conn.scope === "full";
  const body = pending(status, "") ?? (!w ? h("p", { class: "empty" }, "This daemon is older than this app and can't say. Upgrade it (More → Daemon).") : (() => {
    const state = w.up ? badge("up", "ok") : w.wanted ? badge(w.last_exit ? "failing" : "starting", w.last_exit ? "bad" : "info") : badge("off", "");
    const byHand = w.by_hand && !w.wanted;
    const lines: string[] = [];
    if (w.by_hand && w.wanted) lines.push(`A \`ptyro serve\` you started by hand (pid ${w.by_hand}) holds the port. Stop it (Ctrl-C) and the daemon's takes over.`);
    if (byHand) lines.push(`This page comes from a \`ptyro serve\` you started by hand (pid ${w.by_hand}): it ends with that terminal, and it won't be back after a restart. Have the daemon run it instead.`);
    return h("div", { class: "stack-in", title: tip("web.status") },
      h("p", { class: "pr-head" }, state, " ", w.up ? "The web app is running" : w.wanted ? "The daemon is starting the web app" : "The daemon isn't serving the web app"),
      facts([
        ["Address", w.url ? h("span", {}, h("code", {}, w.url), " ", h("button", { class: "small", onclick: () => void navigator.clipboard?.writeText(w.url).then(() => ui.toast("Copied"), () => ui.toast("Couldn't copy")) }, "Copy")) : null],
        ["Who can reach it", w.access ? h("span", {}, ACCESS.find((a) => a[0] === w.access)?.[1] ?? w.access, " ", h("button", { class: "small", title: "remote.access", onclick: () => pickAccess(w.access, "Who can reach the web app?") }, "Change")) : null],
        ["Port", w.port ? String(w.port) : w.listen],
        ["Run by", w.pid ? `the daemon (pid ${w.pid})` : byHand ? "you, by hand" : null],
        ["Up for", w.since_ms ? ago(w.since_ms) : null],
        ["Restarted", w.restarts ? `${w.restarts} time${w.restarts === 1 ? "" : "s"} since the daemon started` : null],
        ["Last stop", w.wanted && !w.up ? w.last_exit : null],
        ["Certificate", w.cert === "tailscale" && !w.cert_note ? h("span", { class: "sub" }, "Tailscale's, for this computer's tailnet name: nothing to install on a phone that opens it") : w.cert === "own" ? h("span", { class: "sub" }, "yours (tls_cert)") : w.ca_fingerprint ? h("span", { class: "sub" }, `ptyro's own; SHA-256 ${w.ca_fingerprint}`) : null],
        ["Tailscale certificate", w.cert_note ? h("span", { class: "sub" }, w.cert_note) : null],
      ]),
      ...lines.map((l) => h("p", { class: "form-err" }, l)),
      !w.wanted ? h("p", { class: "intro" }, "Turning it on makes the daemon run a web server: it listens on a port and comes back whenever the daemon does. Nothing opens to anyone until you pair a device, and each can be revoked.") : null,
      w.wanted && !w.up && w.last_exit ? h("p", { class: "intro" }, "It keeps trying again, a little slower each time. The log says why; ", h("a", { href: "#/health" }, "Health"), " has the fix.") : null,
      full ? bar(
        w.wanted ? h("button", { class: "deny", title: tip("web.off"), onclick: turnWebOff }, "Stop serving") : h("button", { class: "primary", title: tip("web.on"), onclick: turnWebOn }, "Run it with the daemon"),
        h("button", { title: tip("pair.device"), onclick: pairAnother }, "Pair a device…"),
        w.up ? h("button", { title: tip("pair.open"), onclick: openOnComputer }, "Open on the computer") : null,
        h("button", { title: tip("web.logs"), onclick: () => void showWebLogs() }, "Log")) : h("p", { class: "intro" }, "Turning it on or off needs a device paired with ", h("code", {}, "ptyro pair --scope full"), "."));
  })());
  return h("div", {}, ui.header("Web app", "#/more"), h("main", { class: "stack" }, body));
}

// ── first-run setup ─────────────────────────────────────────────────────────────────────────

const discovered = new Cache<string[]>(() => conn.request({ t: "discover", refresh: false }).then((r) => r.paths));
let setupPicks = new Set<string>();

/// `#/setup` (from More, the first-run banner and the Sessions tab) or `#/setup/settings` (from
/// Settings): the checklist first, then the logins, then the rest of the first run.
function setupView(rest: string): HTMLElement {
  const repos = (discovered.get("") ?? []).filter((p) => ![...data.projects.values()].some((x) => x.root === p));
  return h("div", {}, ui.header("Set up ptyro", rest === "settings" ? "#/settings" : "#/more"), h("main", { class: "stack", title: tip("first.run") },
    h("p", { class: "intro" }, "Nothing changes until you tap a button."),
    installSection(),
    loginsSection(),
    section("Your accounts", h("p", { class: "sub" }, "Several logins on one machine (aliases, ~/.claude-* directories, git identities) can become profiles."),
      bar(h("button", { onclick: () => void detectProfiles(), disabled: conn.scope !== "full" }, "Find my accounts"))),
    section("Your repositories", pending(discovered, "", "Looking…") ?? (repos.length ? h("div", { class: "stack-in" },
      ...repos.map((p) => { const c = h("input", { type: "checkbox" }); c.checked = setupPicks.has(p); c.addEventListener("change", () => { if (c.checked) setupPicks.add(p); else setupPicks.delete(p); }); return h("label", { class: "field check" }, c, h("span", {}, p.split("/").pop() ?? p), h("small", {}, p)); }),
      bar(h("button", { class: "primary", onclick: async () => { const n = setupPicks.size; for (const p of setupPicks) await ask({ t: "project_add", path: p, workspace: null }); setupPicks = new Set(); discovered.reload(""); ui.toast(`Added ${n}`); } }, "Add the ticked ones")))
      : h("p", { class: "sub" }, data.projects.size ? "Everything found is added already." : "None found under your discover roots: add one by its path on the Projects tab."))),
    bar(h("button", { class: "primary", onclick: () => go("#/") }, "Done"))));
}

// ── the checklist (BOOT1's engine in the daemon, SPEC §24.2; BOOT3's page, §24.6) ────────────

/// The checklist for some picks. The key: this computer's ids joined by commas ("" is the
/// defaults, "none" nothing), then on Windows with WSL `|` and WSL's (BOOT8). The reply is
/// `SetupPlan`: the plan, each entry's line in the TUI's words, the default agent, and the run
/// going now, which this page then follows.
const ids = (k: string): string[] | null => k === "" ? null : k === "none" ? [] : k.split(",");
const setupPlan = new Cache<any>((key) => {
  const [mine, wsl] = key.split("|");
  // What this page followed when it asked, and how many asks came before: a plan that says
  // nothing is running means the run ended only if it was asked for after the run was known.
  const asked = installRun.id;
  const at = asks;
  return conn.request({ t: "setup_plan", picks: ids(mine), wsl_picks: wsl === undefined ? null : ids(wsl) ?? [] }).then((r) => {
    const said = installRun.sync(r.running, asked);
    // The checklist and the logins as they are after a run that ended out of this page's sight
    // (or that a join found finished): this survey may be from before its end.
    if (said === "ended" || said === "done" || (said === "joined" && installRun.report)) {
      setupLogins.reload("");
      setTimeout(() => setupPlan.refresh(installKey), 0);
    }
    if (said) ui.render();
    if (noAgent(r.plan)) store.remove(AGENT_SEEN); else if (r.plan?.entries?.length) store.set(AGENT_SEEN, "1");
    // Asked again while this one was out (a run's first event, a reconnect): ask once more, as
    // this survey may be from before it.
    if (asks > at) setTimeout(askPlan, 0);
    // Someone else's install (a terminal's, the TUI's) sends no events here: look again in a
    // while, as long as the page is open, to offer Install once it's done.
    if (r.held_by && !r.running && !heldTimer) heldTimer = window.setTimeout(() => { heldTimer = 0; if (location.hash.startsWith("#/setup")) askPlan(); }, 10_000);
    return r;
  });
});
let installKey = "";
let asks = 0;
let heldTimer = 0;
/// Ask the daemon for the plan again (and what is running), keeping what is shown meanwhile.
function askPlan(): void { asks++; setupPlan.refresh(installKey); }
/// The run this page follows: its own, or one going already (BOOT3: one at a time, everywhere).
const installRun = new SetupLog();

/// A setup run's progress (`setup_progress` events).
export function setupEvent(e: any): void {
  const said = installRun.event(e);
  // A run this page didn't start, or lines it missed: the plan says what is going and what it
  // has done so far.
  if (said === "new" || said === "gap") askPlan();
  if (said === "done") { setupPlan.reload(); setupLogins.reload(""); }
  ui.render();
}

/// A fresh connection (a reconnect: a phone woke up): events may have been missed meanwhile, so
/// a run this page follows, or the checklist on screen, asks the daemon again.
export function setupConnected(): void {
  if (installRun.running || location.hash.startsWith("#/setup")) askPlan();
}

/// This device saw a coding agent on the computer: the banner stops asking (a survey each time).
const AGENT_SEEN = "ptyro.setup.agent";
const bannerRetry = { wait: 5_000, timer: 0 };

/// The checklist with `picks` ticked (none: its defaults): a message's setup offer (BOOT9).
export function openSetup(picks: string[]): void {
  installKey = picks.join(",");
  go("#/setup");
  askPlan();
}

/// A refused request that says an agent or tool isn't installed (BOOT9: "`ptyro install codex`
/// installs it", "`ptyro setup` installs one"): a sheet with its words and Set up, which opens
/// the checklist with it ticked. False for any other message (the caller toasts it).
export function offerSetup(text: string): boolean {
  const picks = setupOffer(text);
  if (!picks) return false;
  const s = sheet(picks.length ? "Not installed" : "No agent installed", [h("p", { class: "intro" }, text)], [
    h("button", { onclick: () => s.close() }, "Not now"),
    h("button", { class: "primary", title: tip("setup.offer"), onclick: () => { s.close(); openSetup(picks); } }, picks.length ? `Set up ${picks.join(", ")}` : "Set up agents and tools"),
  ]);
  return true;
}

/// The first-run banner on the Sessions tab: no coding agent on the computer yet (BOOT3). Only
/// for a device that can install (`full`), and never again once this device has seen an agent.
export function setupBanner(): HTMLElement | null {
  if (conn.state !== "open" || !bannerWants({ scope: conn.scope, later: store.get("ptyro.setup.later") === "1", seen: store.get(AGENT_SEEN) === "1" })) return null;
  // A failed ask is tried again, waiting longer each time (up to five minutes).
  if (setupPlan.error("") && !bannerRetry.timer) {
    bannerRetry.timer = window.setTimeout(() => { bannerRetry.timer = 0; setupPlan.reload(""); }, bannerRetry.wait);
    bannerRetry.wait = Math.min(bannerRetry.wait * 2, 300_000);
  }
  const plan = setupPlan.get("")?.plan;
  if (plan) bannerRetry.wait = 5_000;
  if (!noAgent(plan)) return null;
  return h("div", { class: "banner warn setup-banner", title: tip("setup.banner") },
    h("b", {}, "No coding agent is installed on this computer yet. "),
    "Tick Claude Code, Codex or another, and ptyro installs it with what it needs, then logs you in.",
    bar(h("a", { class: "button primary", href: "#/setup" }, "Set up agents and tools"),
      h("button", { class: "small", onclick: () => { store.set("ptyro.setup.later", "1"); ui.render(); } }, "Not now")));
}

/// What WSL is, for the page (the CLI's words for it).
function wslText(w: any): string | null {
  if (w?.state === "unusable") return `WSL: ${w.reason}, so this sets up Windows only. For WSL too: ${w.fix}`;
  if (w?.state === "inside") return `This is WSL${w.distro ? ` (${w.distro})` : ""}: setup here installs into it. For Windows itself, run \`ptyro setup --where windows\` in a Windows terminal.`;
  return null;
}

const copyButton = (text: string): HTMLElement => h("button", { class: "small", onclick: () => void navigator.clipboard?.writeText(text).then(() => ui.toast("Copied"), () => ui.toast("Couldn't copy")) }, "Copy");

function installSection(): HTMLElement {
  const title = "Agents and tools";
  if (conn.scope !== "full") return section(title, h("p", { class: "sub" }, "Needs a device paired with --scope full."));
  const reply = setupPlan.get(installKey);
  const waiting = pending(setupPlan, installKey, "Looking at what's installed…");
  if (waiting || !reply) return section(title, waiting ?? h("p", { class: "sub" }, "Looking…"));
  const plan = reply.plan;
  const lines: any[] = reply.lines ?? [];
  const lineOf = (e: any): any => lines.find((l) => l.id === e.id && l.side === e.side) ?? {};
  const two = plan.wsl?.state === "usable";
  const mineSide = plan.entries[0]?.side ?? "local";
  const sides: string[] = two ? [mineSide, "wsl"] : [mineSide];
  const rows = (side: string): any[] => plan.entries.filter((e: any) => e.side === side);
  const picked = (side: string): string[] => rows(side).filter((e: any) => e.picked).map((e: any) => e.id);
  const keyOf = (mine: string[], wsl: string[]): string => {
    const k = (l: string[]): string => l.length ? l.join(",") : "none";
    return two ? `${k(mine)}|${wsl.join(",")}` : k(mine);
  };
  const toggle = (side: string, id: string, on: boolean): void => {
    const next = (l: string[]): string[] => on ? [...l, id] : l.filter((p) => p !== id);
    const mine = picked(mineSide), wsl = picked("wsl");
    installKey = side === "wsl" ? keyOf(mine, next(wsl)) : keyOf(next(mine), wsl);
    ui.render();
  };
  // Setup running on the computer, but not as the daemon's run (a terminal's, the TUI's).
  const held: string | null = !installRun.running && !installRun.starting ? reply.held_by ?? null : null;
  const running = installRun.running || installRun.starting || !!held;
  const all = plan.entries;
  const entryOn = (id: string, side: string): any => all.find((y: any) => y.id === id && y.side === side);
  const box = (e: any): HTMLElement => {
    const c = h("input", { type: "checkbox", "aria-label": two ? `${e.name} on ${sideName(e.side)}` : e.name }) as HTMLInputElement;
    c.checked = e.ticked;
    // A prerequisite something ticked needs stays ticked (untick that first).
    c.disabled = running || (e.ticked && !e.picked);
    c.addEventListener("change", () => toggle(e.side, e.id, c.checked));
    return c;
  };
  const sideName = (s: string): string => s === "wsl" ? `WSL (${plan.wsl.distro})` : s === "windows" ? "Windows" : "this computer";
  // An item is open while some side doesn't have it (the engine's `rows`); here on every side,
  // it is under Already here (`already`).
  const open = rows(mineSide).filter((e: any) => sides.some((s) => { const x = entryOn(e.id, s); return x && !isHere(x); }));
  const here = rows(mineSide).filter((e: any) => !open.includes(e));
  // Installed another way and offered the move (BOOT7): Keep stops offering it (this computer's
  // side; WSL keeps its own, in its config).
  const keepButton = (e: any): HTMLElement | null => {
    const x = entryOn(e.id, mineSide);
    return x?.moves ? h("button", { class: "small setup-keep", title: tip("setup.keep"), disabled: running, onclick: (ev: Event) => { ev.preventDefault(); void keepInstaller(e.id, e.name, x.moves.from); } }, `Keep ${viaWords(x.moves.from)}`) : null;
  };
  const item = (e: any): HTMLElement => {
    const about = lineOf(e).about ?? "";
    if (!two) return h("div", { class: "setup-item-row" }, h("label", { class: "field check setup-item", title: e.moves ? tip("setup.move") : undefined }, box(e), h("span", {}, h("b", {}, e.name)), about ? h("small", {}, about) : null), keepButton(e));
    const cells = sides.map((s) => {
      const x = entryOn(e.id, s);
      if (!x) return null;
      const l = lineOf(x);
      if (isHere(x)) return h("div", { class: "setup-cell" }, h("small", {}, `${sideName(s)}: ${l.how ?? "✓"}`));
      return h("label", { class: "field check setup-cell" }, box(x), h("span", {}, sideName(s), l.how ? h("small", {}, ` ${l.how}`) : null),
        (l.why ?? []).length ? h("small", {}, l.why.join("; ")) : null);
    });
    return h("div", { class: "setup-item" }, h("b", {}, e.name), about ? h("small", {}, about) : null, h("div", { class: "setup-cells" }, ...cells), keepButton(e));
  };
  const group = (name: string, kind: string): HTMLElement | null => {
    const these = open.filter((e: any) => e.kind === kind);
    return these.length ? h("div", { class: "setup-group" }, h("h4", {}, name), ...these.map(item)) : null;
  };
  const hereLine = (e: any): HTMLElement => {
    const each = sides.map((s) => entryOn(e.id, s)).filter(Boolean).map((x: any) => {
      const at = x.path ? ` (${x.path})` : "";
      const words = lineOf(x).state ?? x.state;
      return two ? `${sideName(x.side)} ${words}${at}` : `${words}${at}`;
    });
    // Out of date, installed another way (npm, Homebrew): it updates that way (BOOT5).
    const stale = sides.map((s) => entryOn(e.id, s)).find((x: any) => x?.state === "other" && x.latest);
    return h("div", { class: "setup-here" }, h("span", { class: "lvl ok" }, "✓"), " ", h("b", {}, e.name), h("small", {}, ` ${each.join(" · ")}${e.id === reply.default_agent ? " · your default agent" : ""}`),
      stale ? h("button", { class: "small", title: tip("updates.update"), disabled: running, onclick: () => void startInstall([e.id], null) }, `Update to ${stale.latest}`) : null);
  };
  const steps = webSteps(plan);
  const admin = adminSteps(plan);
  const waits = waitsForAdmin(plan);
  const wsl = wslText(plan.wsl);
  const who = held
    ? `Setup is already running on the computer (from ${held}): Install waits until it's done, and this page looks again on its own.`
    : installRun.theirs
    ? "Setup is already running on the computer (started from another window, device or before this page opened): its progress is below. The checklist opens again once it's done."
    : "Installing on the computer: its progress is below.";
  return section(title,
    running ? h("p", { class: "banner setup-running", title: tip("setup.running") }, `⏳ ${who}`) : null,
    h("div", { title: tip(two ? "setup.where" : "setup.plan") },
      two ? h("p", { class: "sub" }, `WSL's ${plan.wsl.distro} has ${plan.wsl.version}: tick an agent or tool under Windows, WSL, or both.`) : null,
      group("Agents", "agent"),
      group("Tools", "tool"),
      open.length ? null : h("p", { class: "sub" }, "Every agent and tool ptyro sets up is here already."),
      here.length ? h("div", { class: "setup-group" }, h("h4", {}, "Already here"), ...here.map(hereLine)) : null,
      wsl ? h("p", { class: "sub" }, wsl) : null),
    admin.length ? h("div", { class: "banner warn setup-admin", title: tip("setup.admin") },
      h("b", {}, "Administrator rights are asked for only on the computer itself"),
      h("p", { class: "sub" }, "A password or a Windows prompt never goes through this page, so these aren't installed from here. On the computer, run:"),
      ...admin.map((a) => h("div", { class: "setup-cmd" }, h("code", {}, a.command), copyButton(a.command), h("small", {}, ` ${a.names.join(", ")}${two ? ` on ${sideName(a.side)}` : ""}: left for the computer`))),
      h("p", { class: "sub" }, "Or in ptyro there: press the prefix (C-q unless you changed it), then U, for this checklist, which asks for the password once. ", h("code", {}, "ptyro setup"), " in a terminal does the same."),
      ...waits.map(([what, first]) => h("p", { class: "sub" }, `${what} needs ${first} first, so it's in that command too.`))) : null,
    bar(h("button", { class: "primary", title: tip("setup.install"), disabled: running || !steps.length, onclick: () => void startInstall(picked(mineSide), two ? picked("wsl") : null, movesOf(plan)) },
      held ? "Waiting for the other setup" : running ? "Installing…" : steps.length ? `Install ${steps.length}` : "Nothing to install from here")),
    installRun.lines.length ? h("pre", { class: "log" }, installRun.lines.join("\n")) : null,
    installRun.report ? h("p", { class: "sub setup-summary" }, installRun.summary()) : null,
    installRun.report ? retrySection(plan) : null);
}

/// An update refused because the agent is running (BOOT5): which sessions run it, and Try again
/// once they've ended (the CLI and the TUI ask "Try again now?" the same way).
function retrySection(plan: any): HTMLElement | null {
  if (!installRun.retry.length) return null;
  return h("div", { class: "banner warn setup-retry", title: tip("updates.retry") },
    ...installRun.retry.map(({ id, side }) => {
      const name = plan?.entries?.find((e: any) => e.id === id)?.name ?? id;
      const again = retryOf({ id, side }, name);
      const running = runningAgent(allSessions().values(), id);
      return h("div", { class: "stack-in" },
        h("p", {}, h("b", {}, `${name} is still running`), running.length ? ` (${running.join(", ")})` : "", ", so its update couldn't replace it. End those sessions (or quit it wherever else it runs), then try again."),
        bar(h("button", { class: "primary", title: tip("updates.retry"), onclick: () => void startInstall(again.picks, again.wslPicks) }, again.label)));
    }));
}

/// Run picks on the computer. `moves`: the agents installed another way to move to the
/// recommended installer (BOOT7: ticked where offered, or Move on Health); every other one stays
/// as it is.
async function startInstall(picks: string[], wslPicks: string[] | null, moves: string[] = []): Promise<void> {
  installRun.start();
  ui.render();
  try {
    const r = await conn.request({ t: "setup_run", picks, wsl_picks: wslPicks, moves });
    // The plan the daemon runs (it looked at WSL again), not the page's: its sides each report.
    installRun.started(r.run, r.plan);
  } catch (e) {
    installRun.failed();
    hostFail(e);
    // Refused because a run is going already (another device): the plan says which, and this
    // page follows it.
    askPlan();
  }
  ui.render();
}

// ── logins (BOOT4: each tool's own sign-in, confirmed by its status command, SPEC §24.4) ─────

const setupLogins = new Cache<any>(() => host("login.list"));
const LOGIN_STATE: Record<string, string> = {
  logged_in: "✓ logged in",
  logged_out: "not logged in",
  unconfirmed: "can't be checked (no status command): it signs in when it starts",
  missing: "not installed",
};

function loginsSection(): HTMLElement {
  const title = "Log in";
  if (conn.scope !== "full") return section(title, h("p", { class: "sub" }, "Needs a device paired with --scope full."));
  const r = setupLogins.get("");
  const waiting = pending(setupLogins, "", "Checking each login…");
  if (waiting || !r) return section(title, waiting ?? h("p", { class: "sub" }, "Checking…"));
  const rows = (r.logins as any[]).filter((l) => l.state !== "missing");
  const plan = setupPlan.get(installKey)?.plan;
  const wsl = plan?.wsl?.state === "usable" ? plan.wsl.distro : null;
  return section(title,
    h("p", { class: "sub", title: tip("setup.login") }, "One account each, through each tool's own sign-in. Log in runs it in a session here: follow the link it shows and enter its code (at the computer, it may open the browser there). ptyro never sees the credentials; each tool's own status command says whether it worked."),
    rows.length
      ? h("div", { class: "rows" }, ...rows.map((l) => row(l.name, `${LOGIN_STATE[l.state] ?? l.state}${l.state === "logged_in" ? "" : ` · runs ${l.command}`}`,
          l.state === "logged_in" ? null : h("button", { class: "small", title: tip("setup.login"), onclick: () => void startLogin(l.id, l.name) }, "Log in"))))
      : h("p", { class: "sub" }, "No agent or GitHub CLI is installed yet: install them above, then log in here."),
    wsl ? h("p", { class: "sub" }, "For WSL's (", wsl, "): run ", h("code", {}, "ptyro login --where wsl"), " on the computer.") : null,
    bar(h("button", { onclick: () => setupLogins.reload("") }, "Check again")),
    h("p", { class: "sub" }, r.profiles));
}

async function startLogin(id: string, name: string): Promise<void> {
  const r = await doHost("login.start", { id });
  if (r) { ui.toast(`Logging in to ${name}: its own sign-in; come back and Check again when it's done`); go(`#/s/${enc(r.session)}`); }
}

// ── the tour ────────────────────────────────────────────────────────────────────────────────

function tourView(): HTMLElement {
  return h("div", {}, ui.header("Tour", "#/more"), h("main", { class: "stack", title: tip("help.tour") },
    h("p", { class: "intro" }, "ptyro's daily loop, done for real: start a task, answer the agent, review what it changed, finish it, clean up. It runs in a ptyro of its own, with a demo repository and a pretend agent: your sessions, projects and config are never touched, and it all goes away when you end the tour."),
    conn.scope !== "full" ? h("p", { class: "empty" }, "Starting a tour needs a device paired with --scope full.") : bar(h("button", { class: "primary", onclick: startTour }, "Start the tour")),
    h("p", { class: "sub" }, "It opens in a new tab served from this computer (localhost), so open this page from the computer that runs ptyro. The tour's own page has a coach that says what to do next.")));
}

async function startTour(): Promise<void> {
  // Open the tab now, inside the tap, or a popup blocker refuses it once the tour is ready.
  const tab = window.open("about:blank", "_blank");
  const b = sheet("Starting the tour…", [h("p", { class: "intro" }, "⏳ Making a ptyro of its own, a demo repository and a pretend agent.")]);
  try {
    const r = await host("tour.start");
    b.close();
    const local = ["localhost", "127.0.0.1"].includes(location.hostname) || location.hostname.startsWith("[");
    if (local && tab) { tab.location.href = r.url; return; }
    tab?.close();
    textSheet("The tour is ready", `It listens on this computer only, so open this link in a browser on the computer that runs ptyro:\n\n${r.url}`);
  } catch (e) { b.close(); tab?.close(); hostFail(e); }
}

// the coach, in a tour's own page (`ptyro.tour` is set when its pairing link says so)
const STEPS = ["Start a task", "Answer the agent", "Review its change", "Finish it", "Clean up", "Keep a shell"] as const;
const tour = { step: 0, worktree: "", agent: "", blocked: false };

function advanceTour(): void {
  for (;;) {
    const before = tour.step;
    const sessions = [...allSessions().values()];
    const agent = sessions.find((s) => s.id === tour.agent);
    if (tour.step === 0) {
      const s = sessions.filter((x) => x.kind === "agent" && x.task && x.worktree && !data.worktrees.get(x.worktree)?.is_root).sort((a, b) => b.created_ms - a.created_ms)[0];
      if (s) { tour.agent = s.id; tour.worktree = s.worktree!; tour.step = 1; }
    } else if (tour.step === 1) {
      const st = agent?.status?.status;
      tour.blocked ||= st === "blocked";
      if ((tour.blocked && (st === "working" || st === "idle" || st === "done")) || ((st === "idle" || st === "done") && (agent?.turns ?? 0) > 0)) tour.step = 2;
    } else if (tour.step === 2) {
      if (location.hash.startsWith(`#/wt/${enc(tour.worktree)}/changes`)) tour.step = 3;
    } else if (tour.step === 3) {
      const w = data.worktrees.get(tour.worktree);
      const st = w?.status;
      if (w && ((w.pr?.state === "MERGED") || (w.integrated && st && (st.staged ?? 0) + (st.modified ?? 0) + (st.untracked ?? 0) === 0 && st.head && w.integrated.startsWith(st.head)))) tour.step = 4;
    } else if (tour.step === 4) {
      if (!data.worktrees.has(tour.worktree)) tour.step = 5;
    } else if (tour.step === 5) {
      // A shell of your own in SHELLS (SPEC §23): numbered, in no worktree.
      if (sessions.some((s) => s.loose && s.slot != null)) tour.step = 6;
    }
    if (tour.step === before) break;
  }
}

const COACH: string[][] = [
  ["Tap ＋ (top right) and say what to do:", "fix the typo in greeting.txt", "Pick “demo · in a new worktree” and Start. It gets a worktree and a branch of its own."],
  ["It wants to run a command and asks first.", "Open its session (Sessions tab) and tap Allow."],
  ["See what it changed: Projects tab → its worktree → Changes.", "Tap a line to leave a comment for the agent."],
  ["Ship it: on the worktree's page tap Finish → Merge into the main branch."],
  ["Clean up: on the worktree's page, ⋯ → Archive (or Remove)."],
  ["Keep a shell of your own: on the Sessions tab, tap ＋ beside SHELLS → A shell.", "It's numbered and in no worktree: tap its number on a shell's page to come back to it."],
  ["That's the loop, and a shell of your own. Everything you did was real, in a ptyro of its own."],
];

let coachEl: HTMLElement | null = null;
/// Called after every render: the coach shows on a tour's own pages and says what's next.
export function coach(): void {
  if (store.get("ptyro.tour") !== "1" || !token()) { coachEl?.remove(); coachEl = null; return; }
  advanceTour();
  const lines = COACH[tour.step];
  const el = h("aside", { class: "coach", "aria-live": "polite" },
    h("b", {}, tour.step < STEPS.length ? `Tour ${tour.step + 1}/${STEPS.length} · ${STEPS[tour.step]}` : "Tour done"),
    ...lines.map((l, i) => h("div", { class: i === 1 && tour.step === 0 ? "say" : "" }, l)),
    h("button", { class: tour.step === STEPS.length ? "primary small" : "small", onclick: () => confirm("End the tour?", "Its ptyro, repository and agent are deleted. Your own ptyro is untouched.", "End the tour", async () => { if (await doHost("tour.end")) { store.remove("ptyro.tour"); ui.toast("The tour is over"); setTimeout(() => window.close(), 800); } }, false) }, "End the tour"));
  if (coachEl) coachEl.replaceWith(el); else document.body.append(el);
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
views.set("about", () => aboutView());
views.set("web", () => webView());
views.set("setup", (rest) => setupView(rest));
views.set("tour", () => tourView());
