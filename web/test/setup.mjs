// The setup checklist's logic (web/dist/setupkit.js, BOOT3), no browser: the first-run banner's
// rule, what an administrator step says to run on the computer instead, following this page's
// own run, and joining a run that was going already (from a plan's `running`), with nothing
// missed or shown twice.
//   node web/test/setup.mjs
import assert from "node:assert/strict";
import { adminSteps, bannerWants, leftForComputer, noAgent, SetupLog, waitsForAdmin, webSteps } from "../dist/setupkit.js";

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

console.log("setup: ok");
