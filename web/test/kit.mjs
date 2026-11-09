// The web app's shared helpers (web/dist/kit.js and conn.js) that need no browser: route decoding,
// the link rule, the one-at-a-time guard, the cache that drops a reply from before a change, and
// the connection's timeouts and keepalive. A few globals stand in for the page.
//   node web/test/kit.mjs
import assert from "node:assert/strict";

const listeners = [];
globalThis.window = { addEventListener: (...a) => listeners.push(a), open: () => {}, setTimeout, clearTimeout };
globalThis.document = { addEventListener: () => {}, visibilityState: "visible", getElementById: () => null };
globalThis.localStorage = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); }, removeItem: () => {} };
globalThis.location = { origin: "http://x", hash: "" };
globalThis.WebSocket = class { static OPEN = 1; readyState = 1; send() {} close() {} };

const { safeDecode, webHref, once, Cache, slugify, NAME_HINT } = await import("../dist/kit.js");
const { conn, store, token } = await import("../dist/conn.js");

// A malformed escape in a route is kept as typed instead of throwing.
assert.equal(safeDecode("a%20b"), "a b");
assert.equal(safeDecode("%E0%A4%A"), "%E0%A4%A");
// Only http(s) is ever followed from data.
assert.equal(webHref("https://example.com/a?b=1"), "https://example.com/a?b=1");
assert.equal(webHref("http://example.com/"), "http://example.com/");
for (const bad of ["javascript:alert(1)", "data:text/html,x", "ptyro://x", "/relative", "", null, undefined, 5]) assert.equal(webHref(bad), null, String(bad));
// A name field swaps what a name can't hold for - as it is typed, and drops what it can't start
// with (the same rule as ptyro-core's `names::typed`, whose tests use the same cases).
assert.equal(slugify("name", "My Feat/2"), "my-feat-2");
assert.equal(slugify("name", "-x"), "x");
assert.equal(slugify("name", ".hidden"), "hidden");
assert.equal(slugify("name", " x"), "x");
assert.equal(slugify("name", "_x"), "_x");
assert.equal(slugify("name", "x..y"), "x.-y");
assert.equal(slugify("name", "héllo"), "h-llo");
assert.equal(slugify("name", "my_fix"), "my_fix");
assert.equal(slugify("name", "brisk-rook.v1"), "brisk-rook.v1");
assert.equal(slugify("branch", "fix this/Now: ok"), "fix-this/Now--ok");
assert.equal(slugify("branch", "-x"), "x");
assert.equal(NAME_HINT, "A space or other odd character becomes - (a-z 0-9 . _ -)");
// localStorage that throws is not a crash.
assert.equal(token(), null);
store.set("k", "v");

// once(): a second call while the first is running does nothing.
{
  let calls = 0;
  const slow = once(async () => { calls++; await new Promise((r) => setTimeout(r, 20)); return "done"; });
  const [a, b] = await Promise.all([slow(), slow()]);
  assert.deepEqual([a, b, calls], ["done", undefined, 1]);
  assert.equal(await slow(), "done", "free again afterwards");
}

// Cache: a reply to a fetch that began before a reload doesn't put the old data back.
{
  conn.state = "open";
  let n = 0;
  const gates = [];
  const c = new Cache(() => new Promise((ok) => { const i = ++n; gates.push(() => ok(`v${i}`)); }));
  assert.equal(c.get("k"), undefined); // fetch 1 starts
  c.reload("k"); // the data changed meanwhile
  assert.equal(c.get("k"), undefined); // fetch 2 starts
  gates[0](); // the old reply lands late
  await new Promise((r) => setTimeout(r, 5));
  assert.equal(c.get("k"), undefined, "the stale reply was dropped");
  gates[1]();
  await new Promise((r) => setTimeout(r, 5));
  assert.equal(c.get("k"), "v2");
}

// Connection: a request with no answer times out and says why; a dropped send is reported.
{
  conn.state = "open";
  conn.ws = new WebSocket();
  let dropped = 0;
  conn.onDropped = () => dropped++;
  const id = conn.nextId;
  const err = await conn.request({ t: "status" }, 30).catch((e) => e);
  assert.equal(err.code, "timeout");
  assert.ok(!conn.pending.has(id), "forgotten");
  conn.pending.clear(); // (the timeout asked the connection a question of its own)
  conn.ws.readyState = 3;
  assert.equal(conn.send({ t: "input" }), false);
  assert.equal(dropped, 1);
  // A request that says how long it will wait isn't cut short.
  conn.ws.readyState = 1;
  const lid = conn.nextId;
  const long = conn.request({ t: "send_text", wait_ready_ms: 1000 }, 10);
  await new Promise((r) => setTimeout(r, 40));
  assert.ok(conn.pending.has(lid), "still waiting");
  conn.pending.get(lid)({ t: "ok", reply: "r" });
  assert.equal(await long, "r");
  // reset() fails what waits and starts over.
  const waiting = conn.request({ t: "x" }).catch((e) => e.code);
  conn.reset();
  assert.equal(await waiting, "daemon_unavailable");
  assert.equal(conn.state, "closed");
}

// What is left to do with a worktree (UX2.2): the same rule as ptyro-core's `RepoStatus::left`.
{
  const { repoLeft, leftWords, shipped } = await import("../dist/data.js");
  const st = (o) => ({ ahead: 0, behind: 0, staged: 0, modified: 0, untracked: 0, conflicts: 0, base_ahead: 1, ...o });
  assert.equal(leftWords(repoLeft(st({ upstream: "origin/x", pushed: true }))), "✓ pushed");
  assert.equal(leftWords(repoLeft(st({ upstream: "origin/x", ahead: 2, modified: 1, untracked: 2 }))), "↑2 ● 3");
  assert.equal(leftWords(repoLeft(st({ unpushed: 3 }))), "not pushed ↑3");
  assert.equal(leftWords(repoLeft(st({ unpushed: 3, staged: 1 }))), "not pushed ↑3 ● 1");
  assert.equal(leftWords(repoLeft(null)), "", "nothing known, nothing said");
  assert.equal(leftWords(repoLeft(st({}))), "", "no commits of its own, nothing to say");
  // Old daemons' status has none of the new fields.
  assert.equal(repoLeft({ ahead: 0, behind: 0, staged: 0 }).pushed, false);
  const wt = (o) => ({ is_root: false, status: st({ head: "abc123def" }), ...o });
  assert.equal(shipped(wt({ pr: { state: "MERGED" } })), true);
  assert.equal(shipped(wt({ pr: { state: "OPEN" } })), false);
  assert.equal(shipped(wt({ integrated: "abc123def456" })), true);
  assert.equal(shipped(wt({ integrated: "abc123def456", status: st({ head: "abc123def", modified: 1 }) })), false);
  assert.equal(shipped(wt({ is_root: true, pr: { state: "MERGED" } })), false);
}

// About (UX2.1): the constants match Cargo.toml, and a device that can't ask the machine still
// gets the daemon's facts and a block to copy.
{
  const { REPOSITORY, ISSUES, LICENSE, aboutDoc, aboutText } = await import("../dist/data.js");
  const { readFileSync } = await import("node:fs");
  const cargo = readFileSync(new URL("../../Cargo.toml", import.meta.url), "utf8");
  assert.equal(cargo.match(/^repository = "(.+)"$/m)[1], REPOSITORY);
  assert.equal(cargo.match(/^license = "(.+)"$/m)[1], LICENSE);
  assert.equal(ISSUES, `${REPOSITORY}/issues`);
  const status = { version: "1.2.3", proto: { major: 1, minor: 18 }, pid: 7, started_ms: 1, socket: "/run/p.sock", log_dir: "/state", web: { wanted: true, up: true, url: "http://h:1" } };
  const doc = aboutDoc(null, status);
  assert.equal(doc.limited, true);
  assert.equal(doc.protocol, "1.18");
  const text = aboutText(doc);
  for (const want of ["ptyro", "1.2.3 (protocol 1.18)", REPOSITORY, ISSUES, "MIT", "up at http://h:1", "/run/p.sock", "/state"]) assert.ok(text.includes(want), `${want} in:\n${text}`);
  // The machine's own document wins, and its report is what is copied.
  assert.equal(aboutText(aboutDoc({ report: "R", version: "x" }, status)), "R");
  assert.equal(aboutDoc(undefined, null).daemon.running, false);
}
{
  // Setup's sides (BOOT8): the summary waits for one report per side the daemon runs.
  const { setupSides } = await import("../dist/kit.js");
  const e = (side, ticked, action, blocked) => ({ side, ticked, action, blocked });
  assert.equal(setupSides(null), 1);
  assert.equal(setupSides({ entries: [] }), 1);
  assert.equal(setupSides({ entries: [e("windows", true, {}), e("wsl", true, {})] }), 2);
  // WSL went away between the page's plan and the run: the daemon's plan has Windows only.
  assert.equal(setupSides({ entries: [e("windows", true, {}), e("windows", true, null, "no way")] }), 1);
  assert.equal(setupSides({ entries: [e("windows", true, {}), e("wsl", false, {}), e("wsl", true, null, null)] }), 1);
}
{
  // Creation forms' pickers: fan-out candidates cycle through the ticked agents (`fanout -n`),
  // a profile select lists the machine's own profiles by their bare names, a new workspace
  // starts with the profile named like it, and agent arguments split as a shell would.
  const { cycle, profileChoices, profileFor, argWords } = await import("../dist/kit.js");
  assert.deepEqual(cycle(["claude"], 2), ["claude", "claude"]);
  assert.deepEqual(cycle(["claude", "codex"], 3), ["claude", "codex", "claude"]);
  assert.deepEqual(cycle(["claude", "codex"], 0), ["claude", "codex"]);
  assert.deepEqual(cycle([], 3), []);
  const ps = [{ name: "default", label: "default" }, { name: "work", label: "Acme" }, { name: "lap:home", label: "home", machine: "lap" }];
  assert.deepEqual(profileChoices(ps, null), [["default", "default"], ["work", "work (Acme)"]]);
  assert.deepEqual(profileChoices(ps, "lap"), [["home", "home"]]);
  assert.equal(profileFor(["default", "work"], "work"), "work");
  assert.equal(profileFor(["default", "work"], "play"), "default");
  assert.deepEqual(argWords(`--model opus  --name "a b" 'c d'`), ["--model", "opus", "--name", "a b", "c d"]);
  assert.deepEqual(argWords("   "), []);
}
console.log("ok");
process.exit(0);
