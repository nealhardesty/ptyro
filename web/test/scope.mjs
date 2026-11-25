// The machine and profile tabs' rule (web/dist/machine.js), case for case as `ptyro_core::scope`
// asserts it (crates/ptyro-core/src/scope.rs, its tests): labels, order, what a tab keeps, the
// counts, and when no strip is drawn. No browser.
//   node web/test/scope.mjs
import assert from "node:assert/strict";
import { bare, count, drawn, follow, keepsSession, keepsWorkspace, profileName, resolve, sameScope, scopeNew, scopeOf, scopeOfWorkspace, scopeParse, scopeText, startIn, tabProfile, tabs } from "../dist/machine.js";

const ws = (machine, name, profile) => ({ id: name, machine, name, profile, notify: "all", projects: [], created_ms: 0 });
const session = (machine, name, profile, extra = {}) => ({ id: name, machine, name, profile, kind: "agent", state: { t: "running" }, created_ms: 0, ...extra });
const as = (s, status) => ({ ...s, status: { status } });
const labels = (t) => t.map((x) => x.label);
const scope = (machine, profile) => ({ machine, profile });

// One machine, one profile: no strip, and a saved tab falls back to `all`.
{
  const t = tabs([ws(null, "main", "default")], [session(null, "a", "default")], "laptop1");
  assert.equal(t.length, 1);
  assert.ok(!drawn(t));
  assert.equal(resolve(scope(null, "default"), t), null);
}

// Two profiles on one machine name only the profile.
{
  const t = tabs([ws(null, "main", "default"), ws(null, "job", "work")], [], "laptop1");
  assert.deepEqual(labels(t), ["[default]", "[work]"]);
  assert.ok(drawn(t));
}

// A linked machine names the machine on every tab.
{
  const t = tabs([ws("laptop2", "laptop2:main", "laptop2:default"), ws(null, "main", "work"), ws(null, "home", "me")], [], "laptop1");
  assert.deepEqual(labels(t), ["laptop1 [me]", "laptop1 [work]", "laptop2"]);
  assert.deepEqual(t[2].scope, scope("laptop2", "laptop2:default"));
  assert.equal(profileName(t[2].scope), "default");
}

// This machine first, then the others by name; `default` leads its machine.
{
  const t = tabs([
    ws("zed", "zed:main", "zed:default"), ws("alpha", "alpha:main", "alpha:work"), ws("alpha", "alpha:x", "alpha:default"),
    ws(null, "b", "beta"), ws(null, "main", "default"), ws(null, "a", "acme"),
  ], [], "here1");
  assert.deepEqual(labels(t), ["here1 [default]", "here1 [acme]", "here1 [beta]", "alpha [default]", "alpha [work]", "zed"]);
}

// A `me` session in a `work` workspace shows its workspace under both tabs; each keeps its own.
{
  const job = ws(null, "job", "work");
  const mine = session(null, "side", "me");
  const theirs = session(null, "build", "work");
  const s = [mine, theirs];
  const t = tabs([job], s, "laptop1");
  assert.deepEqual(labels(t), ["[me]", "[work]"], "`me` is in use only through the session");
  const [me, work] = [t[0].scope, t[1].scope];
  assert.ok(keepsWorkspace(me, job, s) && keepsWorkspace(work, job, s));
  assert.ok(keepsSession(me, mine) && !keepsSession(me, theirs));
  assert.ok(keepsSession(work, theirs) && !keepsSession(work, mine));
  assert.ok(!keepsWorkspace(me, job, [theirs]), "with nothing of `me` under it, `[me]` doesn't show it");
}

// Another machine never shows under this one's tab.
{
  const there = ws("laptop2", "laptop2:main", "laptop2:default");
  const s = session("laptop2", "laptop2:a", "laptop2:default");
  const here = scope(null, "default");
  assert.ok(!keepsWorkspace(here, there, [s]));
  assert.ok(!keepsSession(here, s));
  assert.deepEqual(scopeOf(s), scope("laptop2", "laptop2:default"));
  assert.deepEqual(scopeOfWorkspace(there), scopeOf(s));
  assert.ok(keepsWorkspace(scopeOf(s), there, []));
  // This machine's sessions and workspaces say no machine at all (the field left out).
  assert.ok(keepsSession(here, { name: "x", profile: "default", state: { t: "running" } }));
}

// A profile is named with or without its machine.
{
  const s = session("laptop2", "laptop2:a", "laptop2:work");
  assert.ok(keepsSession(scopeNew("laptop2", "work"), s));
  assert.ok(keepsSession(scopeNew("laptop2", "laptop2:work"), s));
  assert.ok(!keepsSession(scopeNew(null, "work"), s));
  assert.deepEqual(scopeNew(null, "work"), scope(null, "work"));
  assert.equal(bare("laptop2:work", "laptop2"), "work");
  assert.equal(bare("work", null), "work");
}

// The counts are running real sessions, blocked and working.
{
  const w = [ws(null, "main", "default"), ws(null, "job", "work")];
  const s = [
    as(session(null, "b1", "work"), "blocked"), as(session(null, "b2", "work"), "blocked"),
    as(session(null, "w1", "work"), "working"), as(session(null, "w2", "default"), "working"),
    as(session(null, "i", "default"), "idle"),
    as(session(null, "q", "work", { quick: true }), "blocked"),
    as(session(null, "c", "work", { chat: "c1" }), "working"),
    as(session(null, "g", "work", { state: { t: "exited" } }), "blocked"),
  ];
  const t = tabs(w, s, "laptop1");
  assert.deepEqual([t[0].blocked, t[0].working], [0, 1]);
  assert.deepEqual([t[1].blocked, t[1].working], [2, 1]);
  assert.deepEqual(count(s), [2, 2], "what `all` carries");
}

// A profile in use only by a session has a tab; a chat's engine makes none.
{
  const w = [ws(null, "main", "default")];
  assert.deepEqual(labels(tabs(w, [session(null, "c", "ghost", { chat: "c1" })], "laptop1")), ["[default]"]);
  assert.deepEqual(labels(tabs(w, [session(null, "x", "solo")], "laptop1")), ["[default]", "[solo]"]);
}

// A quick terminal makes no tab.
{
  const t = tabs([ws(null, "main", "default")], [session(null, "q", "ghost", { quick: true })], "laptop1");
  assert.deepEqual(labels(t), ["[default]"]);
  assert.ok(!drawn(t));
}

// A saved tab that is gone falls back to `all`.
{
  const t = tabs([ws(null, "main", "default"), ws(null, "job", "work")], [], "laptop1");
  assert.deepEqual(resolve(scope(null, "work"), t), scope(null, "work"));
  assert.equal(resolve(scope(null, "gone"), t), null);
  assert.equal(resolve(null, t), null);
}

// Kept in the browser as the Rust one serializes (this machine left out), and back.
{
  assert.equal(scopeText(scope(null, "work")), '{"profile":"work"}');
  assert.deepEqual(scopeParse('{"profile":"work"}'), scope(null, "work"));
  assert.deepEqual(scopeParse(scopeText(scope("laptop2", "me"))), scope("laptop2", "me"));
  assert.equal(scopeText(null), "");
  for (const junk of [null, "", "all", "{", '{"machine":"x"}', "42"]) assert.equal(scopeParse(junk), null, String(junk));
}

// Going to a session outside the tab switches to its tab; `all` and a kept one stay.
{
  const job = ws(null, "job", "work");
  const mine = session(null, "side", "me");
  const theirs = session(null, "build", "work");
  const t = tabs([job], [mine, theirs], "laptop1");
  const work = scope(null, "work");
  assert.deepEqual(follow(work, mine), scope(null, "me"));
  assert.ok(t.some((x) => sameScope(x.scope, follow(work, mine))), "its own tab is always there");
  assert.deepEqual(follow(work, theirs), work);
  assert.equal(follow(null, mine), null, "`all` has everything");
  // Its tab with no strip drawn (one machine, one profile) shows as `all`.
  const one = tabs([ws(null, "main", "default")], [session(null, "x", "default")], "laptop1");
  assert.equal(resolve(follow(scope(null, "work"), session(null, "x", "default")), one), null);
  assert.ok(sameScope(null, null) && !sameScope(null, work) && sameScope(work, scope(null, "work")));
}

// A quick terminal goes with its worktree: going to it never moves the tab.
{
  const work = scope(null, "work");
  assert.deepEqual(follow(work, session(null, "q", "me", { quick: true })), work);
  assert.equal(follow(null, session(null, "q", "me", { quick: true })), null);
}

// A form under a tab preselects its profile only on its machine, and only if it is there.
{
  const work = scope(null, "work");
  assert.equal(tabProfile(work, null, ["default", "work"]), "work");
  assert.equal(tabProfile(work, "laptop2", ["default", "work"]), null, "another machine: the form's own default");
  assert.equal(tabProfile(work, null, ["default"]), null, "not among the options: the form's own default");
  assert.equal(tabProfile(null, null, ["default", "work"]), null);
  assert.equal(tabProfile(scope("laptop2", "laptop2:work"), "laptop2", ["default", "work"]), "work");
}

// Something new under a tab lands in it: a workspace of its profile (the one shown first), else
// the profile asked for explicitly.
{
  const w = [ws(null, "main", "default"), ws(null, "job", "work"), ws(null, "job2", "work"), ws("laptop2", "laptop2:main", "laptop2:default"), ws("laptop2", "laptop2:side", "laptop2:work")];
  assert.deepEqual(startIn(scope(null, "work"), w), { workspace: "job" });
  assert.deepEqual(startIn(scope(null, "work"), w, "job2"), { workspace: "job2" });
  assert.deepEqual(startIn(scope(null, "work"), w, "main"), { workspace: "job" }, "the one shown isn't the tab's");
  assert.deepEqual(startIn(scope("laptop2", "laptop2:work"), w), { workspace: "laptop2:side" });
  assert.deepEqual(startIn(scope(null, "me"), w), { workspace: null, profile: "me" });
  assert.deepEqual(startIn(scope("laptop2", "laptop2:me"), w), { workspace: "laptop2:main", profile: "me" }, "reached through a workspace of its machine");
}

console.log("scope: ok");
