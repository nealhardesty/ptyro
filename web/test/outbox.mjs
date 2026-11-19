// Every key exactly once (ROADMAP WEB2.3): web/dist/outbox.js's `Outbox` against a daemon that
// follows the same rules as crates/ptyro-daemon/src/typed.rs, over a network that drops, cuts
// and delays. Scenarios first, then a property test: thousands of random runs of typing,
// connections lost mid-burst, answers lost, the session changing while offline, and a check
// that every key is typed at most once, in order, never after keys that were said not to be
// sent, never said "not sent" when it was, and that nothing is left waiting unsaid.
//   node web/test/outbox.mjs
import assert from "node:assert/strict";
import { Outbox, REPLAY_WINDOW_MS, RESEND_MS, GIVE_UP_MS, WAIT_MS, describe, lostTitle } from "../dist/outbox.js";

// ── the daemon (typed.rs, in miniature) ────────────────────────────────────────────────────
class Daemon {
  constructor() { this.high = new Map(); this.refusals = []; this.token = "t0"; this.typed = []; this.numbered = true; this.plain = []; }
  /// A `keys` message → its answer (`typed`), as conn.rs `keys` and `Streams::decide` give it.
  keys(m, now) {
    const keys = m.keys;
    const last = keys.length ? keys[keys.length - 1].n : 0;
    const high = this.high.get(m.stream) ?? 0;
    // Earlier refusals of keys this brings again: said again.
    const olds = this.refusals.filter((r) => keys.some((k) => k.n <= high && k.n >= r.from && k.n <= r.to));
    const from = keys.findIndex((k) => k.n > high);
    const answer = (upto, refused) => ({ t: "typed", session: m.session, stream: m.stream, upto, refused });
    if (from < 0) return answer(high, olds);
    let no = null;
    if (m.replay) {
      const rest = keys.slice(from);
      if (olds.length) no = { why: olds[olds.length - 1].why, reason: "keys before them weren't typed" };
      else if (rest[0].age_ms > REPLAY_WINDOW_MS) no = { why: "stale", reason: "too old" };
      else if (rest.some((k) => k.token !== this.token)) no = { why: "changed", reason: "changed" };
    }
    this.high.set(m.stream, Math.max(high, last));
    if (no) {
      const r = { from: keys[from].n, to: last, ...no };
      this.refusals.push(r);
      return answer(last, [...olds, r]);
    }
    for (const k of keys.slice(from)) this.typed.push({ n: k.n, event: k.event, at: now, replay: !!m.replay, token: this.token });
    return answer(last, olds);
  }
}

// ── a page, a network and a clock ──────────────────────────────────────────────────────────
function world({ numbered = true } = {}) {
  const d = new Daemon();
  d.numbered = numbered;
  const w = {
    d, now: 0, up: false, toServer: [], toClient: [], timer: null, checks: 0, seen: d.token,
    io: null, o: null,
  };
  w.io = {
    send: (m) => { if (!w.up) return false; w.toServer.push(m); return true; },
    now: () => w.now,
    token: () => w.seen,
    changed: () => {},
    check: () => { w.checks++; },
    later: (ms) => { w.timer = w.now + ms; },
  };
  w.o = new Outbox("s1", w.io, "stream1");
  /// Deliver what is in flight (both ways), in order.
  w.pump = () => {
    if (w.blackhole) { w.toServer = []; return; }
    while (w.toServer.length || w.toClient.length) {
      const m = w.toServer.shift();
      if (m) {
        if (m.t === "keys") w.toClient.push(d.keys(m, w.now));
        else if (m.t === "input") d.plain.push(m.event);
      }
      const a = w.toClient.shift();
      if (a) w.o.typed(a);
    }
    w.seen = d.token;
  };
  w.connect = () => { w.up = true; w.seen = d.token; w.o.connected(d.numbered); };
  /// The connection goes: the first `keep` messages to the daemon still arrive, the rest and
  /// every answer on its way are lost.
  w.drop = (keep = 0) => {
    const arrive = w.toServer.splice(0, keep);
    w.toServer = [];
    for (const m of arrive) if (m.t === "keys") d.keys(m, w.now); // answered into a dead socket
    w.toClient = [];
    w.up = false;
    w.o.disconnected();
  };
  w.wait = (ms) => {
    const end = w.now + ms;
    while (w.timer !== null && w.timer <= end) { w.now = Math.max(w.now, w.timer); w.timer = null; w.o.tick(); if (w.up) w.pump(); }
    w.now = end;
  };
  w.text = () => describe(d.typed.map((k) => k.event));
  return w;
}
const key = (c) => ({ t: "key", key: { code: c === "\n" ? { t: "enter" } : { t: "char", c }, mods: 0 } });
const typeText = (w, s) => { for (const c of s) w.o.type(key(c)); };

// ── scenarios ──────────────────────────────────────────────────────────────────────────────
{
  // Online: each key goes as it is typed and is settled by its answer; nothing shows.
  const w = world();
  w.connect();
  typeText(w, "ls\n");
  assert.equal(w.toServer.length, 3);
  assert.equal(w.o.waiting, 3);
  assert.equal(w.o.shown, false, "a key on its way isn't news");
  w.pump();
  assert.equal(w.text(), "ls⏎\n");
  assert.equal(w.o.waiting, 0);
}
{
  // Offline: kept and shown; back within the window with nothing changed: typed, once.
  const w = world();
  w.connect();
  typeText(w, "ab");
  w.pump();
  w.drop();
  typeText(w, "cd");
  assert.equal(w.o.waiting, 2);
  assert.equal(w.o.shown, true);
  assert.ok(w.checks > 0, "it asks to reconnect soon");
  w.wait(1500);
  w.connect();
  assert.equal(w.toServer.length, 1, "one replay");
  assert.equal(w.toServer[0].replay, true);
  assert.deepEqual(w.toServer[0].keys.map((k) => [k.n, k.token, k.age_ms]), [[3, "t0", 1500], [4, "t0", 1500]]);
  w.pump();
  assert.equal(w.text(), "abcd");
  assert.equal(w.o.lost.length, 0);
}
{
  // The connection drops mid-burst: some keys arrived, their answers didn't. Sent again with the
  // same numbers, the copies are dropped: every key once.
  const w = world();
  w.connect();
  typeText(w, "hello");
  w.drop(3); // h e l arrived; l o didn't
  w.wait(300);
  w.connect();
  w.pump();
  assert.equal(w.text(), "hello");
  assert.deepEqual(w.d.typed.map((k) => k.n), [1, 2, 3, 4, 5]);
}
{
  // The session asked something new while offline: the replay is refused, said plainly, the
  // keys can be seen; and what was typed behind it on the new connection isn't sent either.
  const w = world();
  w.connect();
  w.drop();
  typeText(w, "y\n");
  w.d.token = "t1"; // a permission prompt appeared
  w.wait(800);
  w.connect();
  typeText(w, "more"); // typed at once, behind the replay: held until it is answered
  assert.equal(w.toServer.length, 1, "keys behind an unanswered replay wait");
  w.pump();
  assert.equal(w.d.typed.length, 0, "nothing typed");
  assert.equal(w.o.waiting, 0);
  // One notice: the refused keys and the ones held behind them.
  assert.equal(w.o.lost.length, 1);
  assert.equal(w.o.lost[0].why, "changed");
  assert.equal(describe(w.o.lost[0].events), "y⏎\nmore");
  assert.match(lostTitle(w.o.lost[0]), /^6 keys weren't sent: the session changed while you were away/);
  // Typed afresh, now: goes.
  typeText(w, "n");
  w.pump();
  assert.equal(w.text(), "n");
  w.o.dismiss();
  assert.equal(w.o.lost.length, 0);
}
{
  // Accepted replay, then what was held behind it goes live.
  const w = world();
  w.connect();
  w.drop();
  typeText(w, "ab");
  w.wait(200);
  w.connect();
  typeText(w, "cd");
  w.pump();
  assert.equal(w.text(), "abcd");
  assert.equal(w.toServer.length, 0);
}
{
  // Offline longer than the window: what was never sent is said at once (it can't be typed),
  // and what is typed after, while still offline, is lost with it (no half command later).
  const w = world();
  w.connect();
  w.drop();
  typeText(w, "rm -rf");
  w.wait(REPLAY_WINDOW_MS + 600);
  assert.equal(w.o.waiting, 0);
  assert.equal(w.o.lost[0].why, "stale");
  typeText(w, " /\n");
  assert.equal(w.o.waiting, 0);
  assert.equal(describe(w.o.lost.flatMap((l) => l.events)), "rm -rf /⏎\n");
  w.connect();
  w.pump();
  assert.equal(w.d.typed.length, 0, "no fragment typed after a loss");
  typeText(w, "x");
  w.pump();
  assert.equal(w.text(), "x", "typing works again once back");
}
{
  // Keys that went and may have been typed are asked about, never assumed lost: the daemon has
  // them, so it says so (a copy), however long the wait was.
  const w = world();
  w.connect();
  typeText(w, "ok");
  w.drop(2); // both arrived and were typed; the answers were lost
  w.wait(REPLAY_WINDOW_MS + 3000);
  assert.equal(w.o.waiting, 2, "kept: they may have been typed");
  w.connect();
  w.pump();
  assert.equal(w.text(), "ok");
  assert.equal(w.o.lost.length, 0, "not said to be lost: they were typed");
  assert.equal(w.o.waiting, 0);
}
{
  // A socket that looks open and carries nothing: unanswered keys show, the connection is asked
  // about, and they are sent again (dropped by the daemon if they had arrived).
  const w = world();
  w.connect();
  typeText(w, "q");
  w.toServer = []; // swallowed
  w.wait(WAIT_MS + 10);
  assert.equal(w.o.shown, true);
  assert.ok(w.checks > 0);
  w.wait(RESEND_MS); // (the world delivers what a tick sends)
  assert.equal(w.text(), "q");
  assert.equal(w.d.typed[0].replay, true, "sent again, as a replay");
  assert.equal(w.o.waiting, 0);
}
{
  // A daemon that never answers: given up on after GIVE_UP_MS and said, honestly ("may not").
  const w = world();
  w.connect();
  typeText(w, "z");
  w.blackhole = true;
  w.wait(GIVE_UP_MS + RESEND_MS * 2);
  assert.equal(w.o.waiting, 0);
  assert.equal(w.o.lost[0].why, "silent");
  assert.match(lostTitle(w.o.lost[0]), /may not have been sent/);
}
{
  // Keys from long ago, asked about on a new connection that doesn't answer at once: the wait
  // counts from the connection, not from when they were typed; the answer, when it comes, says.
  const w = world();
  w.connect();
  typeText(w, "w");
  w.drop(0); // it went (so it may have been typed): kept, not assumed lost
  w.wait(20_000);
  assert.equal(w.o.waiting, 1);
  w.blackhole = true;
  w.connect();
  w.wait(RESEND_MS * 2);
  assert.equal(w.o.waiting, 1, "not given up: it was only just asked about");
  w.blackhole = false;
  w.wait(RESEND_MS + 100);
  assert.equal(w.o.waiting, 0);
  assert.equal(w.o.lost[0].why, "stale");
}
{
  // An older daemon (no `input_seq`): plain Input as before, nothing waits or shows.
  const w = world({ numbered: false });
  w.connect();
  typeText(w, "ab");
  assert.equal(w.o.mode, "plain");
  assert.equal(w.o.waiting, 0);
  w.pump();
  assert.equal(describe(w.d.plain), "ab");
  // Keys typed before the first welcome, then the daemon turns out older: not sent, and said.
  const v = world({ numbered: false });
  typeText(v, "x");
  assert.equal(v.o.waiting, 1);
  v.connect();
  assert.equal(v.o.lost[0].why, "old");
  v.pump();
  assert.equal(v.d.plain.length, 0);
}
{
  // Clicks are aimed at a screen as it was: never kept for later; sent live when online.
  const w = world();
  const click = { t: "mouse", mouse: { kind: { t: "down", button: "left" }, x: 1, y: 1, mods: 0 } };
  w.o.type(click);
  assert.equal(w.o.waiting, 0);
  w.connect();
  w.o.type(click);
  w.pump();
  assert.equal(w.d.typed.length, 1);
}
{
  // A refusal for keys the daemon couldn't type (it ended, this device only watches) is said.
  const w = world();
  w.connect();
  typeText(w, "a");
  w.toServer = [];
  w.o.typed({ stream: "stream1", upto: 1, refused: [{ from: 1, to: 1, why: "scope", reason: "watch only" }] });
  assert.equal(w.o.lost[0].why, "scope");
  assert.match(lostTitle(w.o.lost[0]), /^A key wasn't sent: this device may only watch\.$/);
  // Another stream's answer is not ours.
  typeText(w, "b");
  w.o.typed({ stream: "other", upto: 99 });
  assert.equal(w.o.waiting, 1);
}
{
  // describe: keys as a person reads them.
  const ev = [key("a"), { t: "key", key: { code: { t: "char", c: "c" }, mods: 4 } }, { t: "key", key: { code: { t: "backspace" }, mods: 0 } }, { t: "paste", text: "hi there" }, { t: "key", key: { code: { t: "f", n: 5 }, mods: 2 } }];
  assert.equal(describe(ev), "a^C⌫hi thereM-F5");
  assert.equal(lostTitle({ events: [{ t: "paste", text: "x" }], why: "stale", reason: "" }), "A paste wasn't sent: you were offline too long.");
}

// ── property test ──────────────────────────────────────────────────────────────────────────
// A seeded generator, so a failure names its run.
function rng(seed) { let s = seed >>> 0; return () => { s = (s * 1664525 + 1013904223) >>> 0; return s / 2 ** 32; }; }
let runs = 0;
for (let seed = 1; seed <= 10000; seed++) {
  const r = rng(seed);
  const w = world();
  const typedAt = new Map(); // n → when the person typed it
  const lostAt = new Map(); // n → when it was said not sent
  let n = 0;
  const watchLost = () => {
    for (const l of w.o.lost) for (const e of l.events) if (!lostAt.has(e.n)) lostAt.set(e.n, { at: w.now, why: l.why });
  };
  if (r() < 0.8) w.connect();
  for (let step = 0; step < 60; step++) {
    const x = r();
    if (x < 0.45) { n++; typedAt.set(n, w.now); w.o.type({ t: "key", key: { code: { t: "char", c: "k" }, mods: 0 }, n }); }
    else if (x < 0.55 && w.up) w.drop(Math.floor(r() * (w.toServer.length + 1)));
    else if (x < 0.65 && !w.up) w.connect();
    else if (x < 0.7) w.d.token = `t${Math.floor(r() * 3)}`;
    else if (x < 0.8 && w.up) w.pump();
    else if (x < 0.85 && w.up) { w.toClient.splice(0, Math.floor(r() * (w.toClient.length + 1))); } // answers lost, socket open
    else w.wait(Math.floor(r() * 2500));
    watchLost();
  }
  // Settle: back online, time passes, everything delivered.
  if (!w.up) w.connect();
  for (let i = 0; i < 20; i++) { w.pump(); w.wait(1000); }
  w.pump();
  watchLost();
  const typed = w.d.typed.filter((k) => k.event.n !== undefined).map((k) => ({ ...k, id: k.event.n }));
  const ids = typed.map((k) => k.id);
  const ctx = `seed ${seed}: typed ${JSON.stringify(ids)} lost ${JSON.stringify([...lostAt.keys()])}`;
  // At most once, in order.
  assert.deepEqual(ids, [...new Set(ids)].sort((a, b) => a - b), `duplicates or out of order, ${ctx}`);
  for (const k of typed) {
    const l = lostAt.get(k.id);
    // Never said "not sent" when it was typed ("may not have been sent" excepted).
    assert.ok(!l || l.why === "silent", `key ${k.id} typed but said ${l?.why}, ${ctx}`);
  }
  for (const [lost, l] of lostAt) {
    if (l.why === "silent") continue;
    // Nothing typed before it was said lost may come after it: no fragment of a command.
    for (const k of typed) if (k.id > lost) assert.ok(typedAt.get(k.id) >= l.at, `key ${k.id} typed after lost key ${lost} though typed before the loss was known, ${ctx}`);
  }
  // A replayed key was typed only if the session still waited for what it did when it was typed.
  // Nothing left waiting, nothing unaccounted for.
  assert.equal(w.o.waiting, 0, `left waiting, ${ctx}`);
  for (let i = 1; i <= n; i++) assert.ok(ids.includes(i) || lostAt.has(i), `key ${i} neither typed nor said, ${ctx}`);
  runs++;
}
console.log(`ok (outbox: scenarios and ${runs} random runs)`);
