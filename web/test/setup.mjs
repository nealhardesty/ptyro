// The setup checklist's logic (web/dist/setupkit.js, BOOT3), no browser: the first-run banner's
// rule, what an administrator step says to run on the computer instead, following this page's
// own run, and joining a run that was going already (from a plan's `running`), with nothing
// missed or shown twice.
//   node web/test/setup.mjs
import assert from "node:assert/strict";
import { adminSteps, noAgent, SetupLog, waitsForAdmin, webSteps } from "../dist/setupkit.js";

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
}

// Administrator steps: never run from the web; each side's command for the computer, with
// `--where` when there are two sides. What waits for one is said; the web runs the rest.
{
  const apt = { via: "apt", command: "sudo apt-get install -y git", admin: true, after: [] };
  const plan = { entries: [
    entry("claude", "agent", "missing", { ticked: true, picked: true, action: { via: "vendor", admin: false, after: [] } }),
    entry("codex", "agent", "missing", { ticked: true, picked: true, action: { via: "npm", admin: false, after: ["node"] } }),
    entry("git", "tool", "missing", { ticked: true, picked: true, action: apt }),
    entry("node", "tool", "missing", { name: "Node.js", ticked: true, action: { ...apt, via: "apt" }, needed_by: ["codex"] }),
    entry("gh", "tool", "missing", { action: apt }),
  ] };
  assert.deepEqual(adminSteps(plan), [{ side: "local", names: ["Git", "Node.js"], command: "ptyro install git node" }]);
  assert.deepEqual(waitsForAdmin(plan), [["Codex", "Node.js"]]);
  assert.deepEqual(webSteps(plan).map((e) => e.id), ["claude", "codex"]);
  const two = { entries: [
    entry("git", "tool", "missing", { side: "windows", ticked: true, picked: true, action: { via: "winget", admin: true } }),
    entry("git", "tool", "missing", { side: "wsl", ticked: true, picked: true, action: apt }),
    entry("claude", "agent", "missing", { side: "wsl", ticked: true, picked: true, action: { via: "vendor", admin: false } }),
  ] };
  assert.deepEqual(adminSteps(two).map((a) => a.command), ["ptyro install git --where windows", "ptyro install git --where wsl"]);
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
  log.started("r1", { entries: [{ side: "windows", ticked: true, action: {} }, { side: "wsl", ticked: true, action: {} }] });
  assert.equal(log.sides, 2);
  assert.equal(log.running, true);
  assert.equal(log.theirs, false);
  assert.deepEqual(log.lines, ["▸ Windows: Claude Code: curl … | bash"]);
  log.event(ev("r1", 2, line("downloading")));
  log.event(ev("r1", 2, line("downloading")));
  assert.equal(log.lines.length, 2, "an event seen twice shows once");
  assert.equal(log.event(ev("r1", 3, finished(["claude"]), "windows")), "");
  assert.equal(log.event(ev("r1", 4, finished([], ["git"]), "wsl")), "done");
  assert.equal(log.running, false);
  assert.equal(log.summary(), "1 installed, 0 failed, 1 not run.");
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
  const joined = log.adopt({ run: "other", plan: { entries: [{ side: "local", ticked: true, action: {} }] },
    events: [{ seq: 1, side: "local", event: { event: "started", name: "Codex", command: "npm i -g @openai/codex" } }, { seq: 2, side: "local", event: line("second") }, { seq: 3, side: "local", event: line("third") }] });
  assert.equal(joined, true);
  assert.equal(log.theirs, true);
  assert.equal(log.running, true);
  assert.deepEqual(log.lines, ["▸ Codex: npm i -g @openai/codex", "  │ second", "  │ third", "  │ fourth"]);
  assert.equal(log.adopt({ run: "other", plan: {}, events: [] }), false, "followed already");
  log.event(ev("other", 4, line("fourth")));
  assert.equal(log.lines.length, 4);
  assert.equal(log.event(ev("other", 5, finished(["codex"]))), "done");
  assert.equal(log.summary(), "1 installed, 0 failed, 0 not run.");
  // A finished run's plan says nothing is going: nothing to join.
  assert.equal(log.adopt(null), false);
}

// The finish can arrive before the plan that says the run is going: joining still ends it.
{
  const log = new SetupLog();
  log.event(ev("late", 2, finished(["gh"])));
  log.adopt({ run: "late", plan: { entries: [{ side: "local", ticked: true, action: {} }] }, events: [{ seq: 1, side: "local", event: line("one") }] });
  assert.equal(log.running, false);
  assert.equal(log.summary(), "1 installed, 0 failed, 0 not run.");
}

// While this page asks for its own run, a plan's `running` (its own, about to be named) isn't
// joined as someone else's.
{
  const log = new SetupLog();
  log.start();
  assert.equal(log.adopt({ run: "mine", plan: {}, events: [] }), false);
  log.started("mine", { entries: [] });
  assert.equal(log.theirs, false);
}

console.log("setup: ok");
