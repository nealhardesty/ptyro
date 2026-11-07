// The setup checklist's logic (web/dist/setupkit.js, BOOT3), no browser: the first-run banner's
// rule, what an administrator step says to run on the computer instead, following this page's
// own run, and joining a run that was going already (from a plan's `running`), with nothing
// missed or shown twice.
//   node web/test/setup.mjs
import assert from "node:assert/strict";
import { adminSteps, bannerWants, isHere, leftForComputer, moveOffer, movesOf, moveWords, noAgent, retryOf, runningAgent, selfUpdate, setupLabel, setupOffer, SetupLog, updateOffer, viaWords, waitsForAdmin, webSteps } from "../dist/setupkit.js";

const entry = (id, kind, state, more = {}) => ({ id, name: id[0].toUpperCase() + id.slice(1), kind, state, side: "local", ticked: false, picked: false, ...more });

// The banner: no agent here on any side; an installed (or other-installed) one hides it; an
// empty or missing plan never shows it.
{
  const bare = { entries: [entry("claude", "agent", "missing"), entry("git", "tool", "installed")] };
  assert.equal(noAgent(bare), true);
  assert.equal(noAgent({ entries: [entry("claude", "agent", "other"), entry("git", "tool", "missing")] }), false);
  assert.equal(noAgent({ entries: [entry("codex", "agent", "installed", { side: "wsl" }), entry("codex", "agent", "missing", { side: "windows" })] }), false);
  assert.equal(noAgent({ entries: [entry("claude", "agent", "outdated")] }), false, "an outdated agent is still an agent");
  assert.equal(noAgent(undefined), false);
  assert.equal(noAgent({ entries: [] }), false);
  // It asks (a survey) only on a device that can install, before "Not now", and until this
  // device has seen an agent there.
  assert.equal(bannerWants({ scope: "full", later: false, seen: false }), true);
  for (const scope of ["read", "approve", "control"]) assert.equal(bannerWants({ scope, later: false, seen: false }), false, scope);
  assert.equal(bannerWants({ scope: "full", later: true, seen: false }), false);
  assert.equal(bannerWants({ scope: "full", later: false, seen: true }), false);
}

// Administrator steps: never run from the web. What waits for one (Codex after Node.js from
// apt) is left for the computer too, in the same command; the button counts only the rest.
{
  const apt = { via: "apt", command: "sudo apt-get install -y git", admin: true, after: [] };
  const plan = { entries: [
    entry("claude", "agent", "missing", { ticked: true, picked: true, action: { via: "vendor", admin: false, after: [] } }),
    entry("codex", "agent", "missing", { ticked: true, picked: true, action: { via: "npm", admin: false, after: ["node"] } }),
    entry("git", "tool", "missing", { ticked: true, picked: true, action: apt }),
    entry("node", "tool", "missing", { name: "Node.js", ticked: true, action: { ...apt, via: "apt" }, needed_by: ["codex"] }),
    entry("gh", "tool", "missing", { action: apt }),
  ] };
  assert.deepEqual(leftForComputer(plan).map((e) => e.id), ["codex", "git", "node"]);
  assert.deepEqual(adminSteps(plan), [{ side: "local", names: ["Codex", "Git", "Node.js"], command: "ptyro install codex git node" }]);
  assert.deepEqual(waitsForAdmin(plan), [["Codex", "Node.js"]]);
  assert.deepEqual(webSteps(plan).map((e) => e.id), ["claude"], "Install 1, not 2: Codex can't run from here");
  const onlyAdmin = { entries: [entry("git", "tool", "missing", { ticked: true, picked: true, action: apt })] };
  assert.deepEqual(webSteps(onlyAdmin), [], "nothing to install from here");
  const two = { entries: [
    entry("git", "tool", "missing", { side: "windows", ticked: true, picked: true, action: { via: "winget", admin: true } }),
    entry("git", "tool", "missing", { side: "wsl", ticked: true, picked: true, action: apt }),
    entry("claude", "agent", "missing", { side: "wsl", ticked: true, picked: true, action: { via: "vendor", admin: false, after: ["git"] } }),
    entry("codex", "agent", "missing", { side: "windows", ticked: true, picked: true, action: { via: "vendor", admin: false, after: [] } }),
  ] };
  assert.deepEqual(adminSteps(two).map((a) => a.command), ["ptyro install git --where windows", "ptyro install git claude --where wsl"]);
  assert.deepEqual(webSteps(two).map((e) => `${e.side}:${e.id}`), ["windows:codex"], "waiting is per side");
  assert.deepEqual(adminSteps({ entries: [entry("claude", "agent", "missing", { ticked: true, action: { admin: false } })] }), []);
}

const ev = (run, seq, event, side = "local") => ({ run, seq, side, event });
const line = (text) => ({ event: "line", text });
const finished = (done = [], skipped = []) => ({ event: "finished", report: { done, failed: [], skipped, blocked: [] } });

// This page's own run: events before its reply wait, then count; the sides come from the plan
// the daemon runs; it ends when each side has finished.
{
  const log = new SetupLog();
  log.start();
  assert.equal(log.event(ev("r1", 1, { event: "started", name: "Claude Code", command: "curl … | bash" })), "");
  log.started("r1", { entries: [{ id: "claude", side: "windows", ticked: true, action: {} }, { id: "git", name: "git", side: "wsl", ticked: true, action: { admin: true } }] });
  assert.equal(log.sides, 2);
  assert.equal(log.running, true);
  assert.equal(log.theirs, false);
  assert.deepEqual(log.lines, ["▸ Windows: Claude Code: curl … | bash"]);
  log.event(ev("r1", 2, line("downloading")));
  log.event(ev("r1", 2, line("downloading")));
  assert.equal(log.lines.length, 2, "an event seen twice shows once");
  assert.equal(log.event(ev("r1", 3, finished(["claude"]), "windows")), "");
  log.event(ev("r1", 4, { event: "skipped", id: "git", reason: "it needs administrator rights", fix: "on the computer itself: `ptyro install git`" }, "wsl"));
  assert.equal(log.lines.at(-1), "– WSL: git: left for the computer: on the computer itself: `ptyro install git`");
  assert.equal(log.event(ev("r1", 5, finished([], ["git"]), "wsl")), "done");
  assert.equal(log.running, false);
  assert.equal(log.summary(), "1 installed. git is left for the computer (administrator rights): run ptyro install git --where wsl there.");
}

// An older daemon's events are unnumbered (seq 0): every one counts.
{
  const log = new SetupLog();
  log.start();
  log.started("r0", { entries: [] });
  log.event(ev("r0", 0, line("a")));
  log.event(ev("r0", 0, line("a")));
  assert.equal(log.lines.length, 2);
}

// A run going already (another device, or before a reload): the first event of an unknown run
// says "new" (the page asks the daemon); the plan's `running` holds what happened so far, and
// what arrived meanwhile is added once, in order. Its sides come from its own plan.
{
  const log = new SetupLog();
  assert.equal(log.event(ev("other", 3, line("third"))), "new");
  assert.equal(log.event(ev("other", 4, line("fourth"))), "", "asked once");
  assert.equal(log.running, false);
  const joined = log.sync({ run: "other", plan: { entries: [{ side: "local", ticked: true, action: {} }] },
    events: [{ seq: 1, side: "local", event: { event: "started", name: "Codex", command: "npm i -g @openai/codex" } }, { seq: 2, side: "local", event: line("second") }, { seq: 3, side: "local", event: line("third") }] });
  assert.equal(joined, "joined");
  assert.equal(log.theirs, true);
  assert.equal(log.running, true);
  assert.deepEqual(log.lines, ["▸ Codex: npm i -g @openai/codex", "  │ second", "  │ third", "  │ fourth"]);
  assert.equal(log.sync({ run: "other", plan: {}, events: [] }, "other"), "merged", "followed already: nothing new");
  log.event(ev("other", 4, line("fourth")));
  assert.equal(log.lines.length, 4);
  assert.equal(log.event(ev("other", 5, finished(["codex"], ["opencode"]))), "done");
  assert.equal(log.summary(), "1 installed, 1 not run.", "a skip that isn't the computer's is not run");
  // A finished run's plan says nothing is going: nothing to join.
  assert.equal(log.sync(null, "other"), "");
}

// The finish can arrive before the plan that says the run is going: joining still ends it.
{
  const log = new SetupLog();
  log.event(ev("late", 2, finished(["gh"])));
  log.sync({ run: "late", plan: { entries: [{ side: "local", ticked: true, action: {} }] }, events: [{ seq: 1, side: "local", event: line("one") }] });
  assert.equal(log.running, false);
  assert.equal(log.summary(), "1 installed.");
}

// While this page asks for its own run, a plan's `running` (its own, about to be named) isn't
// joined as someone else's.
{
  const log = new SetupLog();
  log.start();
  assert.equal(log.sync({ run: "mine", plan: {}, events: [] }, ""), "");
  log.started("mine", { entries: [] });
  assert.equal(log.theirs, false);
}

// What is left for the computer is told apart by side: git skipped on Windows for another
// reason is "not run"; git left on WSL is the computer's.
{
  const log = new SetupLog();
  log.start();
  log.started("rs", { entries: [
    { id: "git", name: "git", side: "windows", ticked: true, action: { admin: false, after: [] } },
    { id: "git", name: "git", side: "wsl", ticked: true, action: { admin: true, after: [] } },
  ] });
  log.event(ev("rs", 1, finished([], ["git"]), "windows"));
  log.event(ev("rs", 2, finished([], ["git"]), "wsl"));
  assert.equal(log.summary(), "0 installed, 1 not run. git is left for the computer (administrator rights): run ptyro install git --where wsl there.");
}

// Away and back (a phone's socket dropped): the run ended meanwhile. A plan asked for after the
// run was known, saying nothing is going, ends it with a note; one asked for before it (a race
// with this page's own start) changes nothing.
{
  const log = new SetupLog();
  log.start();
  log.started("r9", { entries: [{ id: "agy", side: "local", ticked: true, action: {} }] });
  log.event(ev("r9", 1, line("one")));
  assert.equal(log.sync(null, ""), "", "asked before the run was known");
  assert.equal(log.running, true);
  assert.equal(log.sync(null, "r9"), "ended");
  assert.equal(log.running, false);
  assert.equal(log.ended, true);
  assert.match(log.lines.at(-1), /finished while this page was away/);
  assert.equal(log.summary(), "", "no report to show");
}

// Away and back, still going: the lines missed are noticed by `seq` (a gap), held, and filled
// from the plan's `running`, once and in order.
{
  const log = new SetupLog();
  log.start();
  log.started("r7", { entries: [{ id: "agy", side: "local", ticked: true, action: {} }] });
  log.event(ev("r7", 1, line("one")));
  assert.equal(log.event(ev("r7", 4, line("four"))), "gap");
  assert.equal(log.event(ev("r7", 5, line("five"))), "", "asked once");
  assert.deepEqual(log.lines, ["  │ one"]);
  const events = [1, 2, 3, 4].map((n) => ({ seq: n, side: "local", event: line(["one", "two", "three", "four"][n - 1]) }));
  assert.equal(log.sync({ run: "r7", plan: {}, events }, "r7"), "merged");
  assert.deepEqual(log.lines, ["one", "two", "three", "four", "five"].map((t) => `  │ ${t}`));
  // The daemon kept only the latest: what's gone is said.
  assert.equal(log.event(ev("r7", 9, line("nine"))), "gap");
  assert.equal(log.sync({ run: "r7", plan: {}, events: [{ seq: 8, side: "local", event: line("eight") }] }, "r7"), "merged");
  assert.deepEqual(log.lines.slice(-3), ["… 2 earlier line(s) from while this page was away aren't shown", "  │ eight", "  │ nine"]);
  // Held events that hold the finish end it even when the plan says nothing is going.
  log.event(ev("r7", 11, finished(["antigravity"])));
  assert.equal(log.sync(null, "r7"), "done");
  assert.match(log.lines.find((l) => l.includes("aren't shown") && l.includes("1 earlier")) ?? "", /1 earlier/);
}

// BOOT5: the doctor's update finding, read into its parts; the sessions an update leaves on the
// old version; and a run that updated says so.
{
  const data = { id: "antigravity", name: "Antigravity", program: "agy", version: "1.3.2", latest: "1.4.0", command: "agy update", admin: false };
  const c = { section: "updates", name: "antigravity", level: "warn", text: "whatever the words say", data };
  assert.deepEqual(updateOffer(c), { id: "antigravity", name: "Antigravity", latest: "1.4.0", version: "1.3.2", program: "agy", command: "agy update", admin: false, path: null, managed: null, via: null });
  assert.equal(updateOffer({ ...c, data: { ...data, admin: true } }).admin, true);
  assert.equal(updateOffer({ ...c, level: "note" }), null, "a skipped one (updates.ignore) offers no update");
  assert.equal(updateOffer({ ...c, data: undefined }), null, "no data, no button (never parsed from the words)");
  assert.equal(updateOffer({ section: "agents", level: "warn", data }), null);
  const s = (name, agent, t = "running", machine = null) => ({ name, agent, state: { t }, machine });
  assert.deepEqual(runningAgent([s("a", "antigravity"), s("b", "claude"), s("c", "antigravity", "exited"), s("d", "antigravity", "running", "laptop2")], "antigravity"), ["a"]);
  const log = new SetupLog();
  log.start();
  log.started("u1", { entries: [{ id: "claude", side: "local", ticked: true, action: { update: true } }, { id: "gh", side: "local", ticked: true, action: {} }] });
  log.event(ev("u1", 1, finished(["claude"])));
  assert.equal(log.summary(), "1 updated.");
  const both = new SetupLog();
  both.start();
  both.started("u2", { entries: [{ id: "claude", side: "local", ticked: true, action: { update: true } }, { id: "gh", side: "local", ticked: true, action: {} }] });
  both.event(ev("u2", 1, finished(["claude", "gh"])));
  assert.equal(both.summary(), "1 installed, 1 updated.");
  // An update refused because the program runs (in_use): the page offers to try it again; a
  // plain failure doesn't; a new run starts with none.
  const busy = new SetupLog();
  busy.start();
  busy.started("u3", { entries: [{ id: "claude", side: "windows", ticked: true, action: { update: true } }, { id: "codex", side: "windows", ticked: true, action: { update: true } }] });
  busy.event(ev("u3", 1, { event: "failed", id: "claude", error: "it exited with 1: being used by another process", fix: "end the sessions", in_use: true }, "windows"));
  busy.event(ev("u3", 2, { event: "failed", id: "codex", error: "no network", fix: "try again" }, "windows"));
  busy.event(ev("u3", 3, { event: "finished", report: { done: [], failed: ["claude", "codex"], skipped: [], blocked: [] } }, "windows"));
  assert.deepEqual(busy.retry, [{ id: "claude", side: "windows" }]);
  // Its button: the label, and the side it runs on (the daemon's picks, or WSL's).
  assert.deepEqual(retryOf(busy.retry[0], "Claude Code"), { label: "Try updating Claude Code again", picks: ["claude"], wslPicks: null });
  assert.deepEqual(retryOf({ id: "codex", side: "wsl" }, "Codex"), { label: "Try updating Codex again", picks: [], wslPicks: ["codex"] });
  assert.deepEqual(retryOf({ id: "codex", side: "local" }, "Codex").picks, ["codex"]);
  busy.start();
  assert.deepEqual(busy.retry, []);
}

console.log("setup: ok");

// ptyro itself (BOOT6): run on the computer; an administrator's binary shows its command; a
// package manager's says how; an agent isn't this.
{
  const data = { id: "ptyro", name: "ptyro", program: "ptyro", version: "0.0.63", latest: "0.0.64", command: null, admin: false, path: "/home/me/.local/bin/ptyro" };
  const c = { section: "updates", level: "warn", data };
  const o = updateOffer(c);
  assert.equal(o.path, "/home/me/.local/bin/ptyro");
  const run = selfUpdate(o);
  assert.equal(run.kind, "run");
  assert.match(run.what, /SHA256SUMS/);
  assert.match(run.what, /replaces \/home\/me\/\.local\/bin\/ptyro/);
  assert.deepEqual(selfUpdate(updateOffer({ ...c, data: { ...data, command: "brew upgrade ptyro", via: "brew" } })), { kind: "run", what: "runs brew upgrade ptyro" });
  const admin = updateOffer({ ...c, data: { ...data, admin: true, command: "curl -fsSL x/install.sh | sudo env PTYRO_PREFIX=/usr/local PTYRO_VERSION=0.0.64 sh" } });
  assert.deepEqual(selfUpdate(admin), { kind: "command", command: admin.command }, "a password never goes through the web");
  assert.deepEqual(selfUpdate(updateOffer({ ...c, data: { ...data, managed: "cargo (`cargo install`)" } })), { kind: "managed", how: "cargo (`cargo install`)" });
  assert.equal(selfUpdate(updateOffer({ section: "updates", level: "warn", data: { id: "codex", program: "codex", latest: "1", version: "0" } })), null);
}

// BOOT7: an agent installed another way and offered the move is a row, not "already here"; its
// Health finding's `data.move` is read into the offer; a move's run says it moved; an
// administrator's move is left for the computer.
{
  const move = { from: "npm", to: "vendor", uninstall: "npm uninstall -g @openai/codex", admin: false, keeps: ["~/.codex"] };
  assert.equal(isHere({ state: "other" }), true, "kept (no offer): here already");
  assert.equal(isHere({ state: "other", moves: move }), false, "offered: a row to tick");
  assert.equal(isHere({ state: "installed" }), true);
  const c = { section: "updates", name: "codex", level: "note", data: { move: { id: "codex", name: "Codex", program: "codex", version: "0.162.1", ...move } } };
  const m = moveOffer(c);
  assert.deepEqual(m, { id: "codex", program: "codex", name: "Codex", from: "npm", to: "vendor", uninstall: "npm uninstall -g @openai/codex", admin: false, keeps: ["~/.codex"] });
  assert.equal(moveWords(m), "Codex is installed with npm. Moving it installs it through the vendor's installer, the recommended way, checks the new one runs, and only then removes npm's copy (npm uninstall -g @openai/codex); your settings and logins (~/.codex) stay. If the new install fails, the old one stays as it was.");
  assert.equal(moveOffer({ section: "updates", data: { id: "codex" } }), null, "an update's data alone offers no move");
  assert.equal(viaWords("brew"), "Homebrew");
  const log = new SetupLog();
  log.start();
  log.started("m1", { entries: [{ id: "codex", side: "local", ticked: true, action: { moving: true } }, { id: "gh", side: "local", ticked: true, action: {} }] });
  log.event(ev("m1", 1, finished(["codex", "gh"])));
  assert.equal(log.summary(), "1 installed, 1 moved to the recommended installer.");
  const plan = { entries: [{ id: "claude", name: "Claude Code", side: "local", ticked: true, action: { moving: true, admin: true }, moves: { ...move, admin: true } }] };
  assert.deepEqual(adminSteps(plan), [{ side: "local", names: ["Claude Code"], command: "ptyro install claude" }]);
  assert.equal(webSteps(plan).length, 0);
  // A run asks to move only what was ticked where offered.
  assert.deepEqual(movesOf({ entries: [{ id: "codex", picked: true, moves: move }, { id: "claude", picked: false, moves: move }, { id: "gh", picked: true }, { id: "codex", side: "wsl", picked: true, moves: move }] }), ["codex"]);
}

// BOOT9: a message that an agent or tool isn't installed offers setup (the daemon's words,
// `ptyro_agents::locate`): the checklist's defaults for "no agent", that one ticked for a missing
// one; nothing for a lock held by `ptyro install`, a move, or an update.
{
  const noAgent = "No coding agent is installed: `ptyro setup` installs one (Claude Code, Codex, or Antigravity for a Google account; Gemini CLI is for Gemini API keys and enterprise accounts)";
  assert.deepEqual(setupOffer(noAgent), []);
  assert.deepEqual(setupOffer("No agent that can chat is installed: `ptyro setup` installs one (Claude Code, Codex or OpenCode; Gemini CLI with a Gemini API key; Antigravity, for a Google account, runs in sessions but can't chat)"), []);
  assert.deepEqual(setupOffer("codex isn't installed (codex is nowhere ptyro looks): `ptyro install codex` installs it"), ["codex"]);
  assert.deepEqual(setupOffer("gh not found: pull requests (`wt new --pr`) need it; `ptyro install gh` installs it"), ["gh"]);
  assert.equal(setupOffer("setup is running already: `ptyro install` (process 42)"), null);
  assert.equal(setupOffer("`ptyro install codex` moves it to the vendor's installer"), null);
  assert.equal(setupOffer("Codex 0.170.0 is out: `ptyro update codex` updates it"), null);
  assert.equal(setupOffer("mine isn't installed (/opt/mine is nowhere ptyro looks): install it, or set [agents.mine] command to its full path"), null);
}

// A setup offer's button names what the checklist calls it: Antigravity, never its program
// `agy` (BOOT9 review); the agent's name while no plan is known; the defaults with no picks.
{
  const agents = [{ name: "antigravity", program: "agy" }, { name: "codex", program: "codex" }];
  const entries = [{ id: "antigravity", name: "Antigravity" }, { id: "gh", name: "GitHub CLI" }];
  assert.equal(setupLabel(["agy"], agents, entries), "Set up Antigravity");
  assert.equal(setupLabel(["agy", "gh"], agents, entries), "Set up Antigravity, GitHub CLI");
  assert.equal(setupLabel(["agy"], agents, []), "Set up antigravity");
  assert.equal(setupLabel(["codex"], agents, entries), "Set up codex");
  assert.equal(setupLabel([], agents, entries), "Set up agents and tools");
}
