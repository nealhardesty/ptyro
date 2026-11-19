// Every key exactly once (ROADMAP WEB2.3): web/dist/outbox.js's `Outbox` against a daemon that
// follows the same rules as crates/ptyro-daemon/src/typed.rs, over a network that drops, cuts
// and delays. Scenarios first, then a property test: thousands of random runs of typing,
// connections lost mid-burst, answers lost, the session changing while offline, and a check
// that every key is typed at most once, in order, never after keys that were said not to be
// sent, never said "not sent" when it was, and that nothing is left waiting unsaid.
//   node web/test/outbox.mjs        (SEED=n: one random run, step by step)
import assert from "node:assert/strict";
import { Outbox, REPLAY_WINDOW_MS, LATE_LIMIT_MS, RESEND_MS, GIVE_UP_MS, WAIT_MS, MAX_SEQ_KEYS, MAX_BYTES, describe, lostTitle, mask } from "../dist/outbox.js";

// ── the daemon (typed.rs, in miniature) ────────────────────────────────────────────────────
const SLACK_MS = 1000;
const MAX_REFUSALS = 8;
const utf8 = new TextEncoder();
const validStream = (s) => typeof s === "string" && /^[A-Za-z0-9_-]{1,64}$/.test(s);
class Daemon {
  constructor() {
    // stream → { high, refused (the latest MAX_REFUSALS), bar }
    this.recs = new Map();
    /// Nothing before this is known (daemon clock): a new session's table knows everything.
    this.floor = 1;
    this.token = "t0"; this.typed = []; this.numbered = true; this.plain = [];
    /// The daemon's clock is not the page's: only `typed_ms` (reckoned from frames) links them.
    this.offset = 1_700_000_000_000;
    /// Odds a key finds the session's input queue full (L3), and the dice.
    this.overflow = 0; this.dice = Math.random;
    /// What the page did that it must never do (malformed messages).
    this.violations = [];
  }
  clock(now) { return now + this.offset; }
  /// Forget every stream (an image without a table): nothing before now is vouched for.
  forget(now) { this.recs.clear(); this.floor = this.clock(now); }
  remember(rec, r, own) {
    if (own) rec.bar = Math.max(rec.bar, r.to);
    const prev = rec.refused[rec.refused.length - 1];
    if (prev && prev.to + 1 === r.from) prev.to = r.to;
    else rec.refused.push({ ...r });
    if (rec.refused.length > MAX_REFUSALS) for (const gone of rec.refused.splice(0, rec.refused.length - MAX_REFUSALS)) rec.forgot = Math.max(rec.forgot, gone.to);
    return r;
  }
  /// A `keys` message → its answer (`typed`), as conn.rs `keys` and `Streams::decide` give it.
  keys(m, now) {
    const keys = m.keys;
    const answer = (upto, refused) => ({ t: "typed", session: m.session, stream: m.stream, upto, refused });
    const rec0 = this.recs.get(m.stream);
    const high = rec0?.high ?? 0;
    const rising = keys.every((k, i) => i === 0 || keys[i - 1].n < k.n);
    if (!validStream(m.stream) || keys.length > MAX_SEQ_KEYS || !rising || keys[0]?.n === 0) {
      // Nothing recorded (a garbage number must not settle a stream for good). The page never
      // sends one: noted, and the test fails.
      this.violations.push(`malformed: ${JSON.stringify(keys.map((k) => k.n))}`);
      return answer(0, keys.length ? [{ from: keys[0].n, to: keys[keys.length - 1].n, why: "invalid", reason: "malformed" }] : []);
    }
    // Earlier refusals of keys this brings again: said again; at or below one forgotten, unsure.
    const olds = (rec0?.refused ?? []).filter((r) => keys.some((k) => k.n <= high && k.n >= r.from && k.n <= r.to));
    const unsure = rec0 ? keys.map((k) => k.n).filter((n) => n <= Math.min(high, rec0.forgot) && !rec0.refused.some((r) => n >= r.from && n <= r.to)) : [];
    if (unsure.length) olds.push({ from: unsure[0], to: unsure[unsure.length - 1], why: "unknown", reason: "no longer remembered" });
    olds.sort((a, b) => a.from - b.from);
    const from = keys.findIndex((k) => k.n > high);
    if (from < 0) return answer(keys[keys.length - 1].n, olds);
    const last = keys[keys.length - 1].n;
    const rest = keys.slice(from);
    const dnow = this.clock(now);
    let before = null;
    if (m.replay && olds.length) before = olds[olds.length - 1];
    else if (rec0 && (rest[0].heard ?? 0) < rec0.bar) before = rec0.refused.findLast((x) => x.to === rec0.bar) ?? rec0.refused[rec0.refused.length - 1];
    const no = before ? { why: before.why, reason: `keys before them weren't typed: ${before.reason}` } : this.judge(!!rec0, rest, !!m.replay, dnow);
    const rec = rec0 ?? { high: 0, refused: [], bar: 0, forgot: 0 };
    this.recs.set(m.stream, rec);
    rec.high = Math.max(high, last);
    if (no) return answer(last, [...olds, this.remember(rec, { from: rest[0].n, to: last, ...no }, !before)]);
    for (const k of rest) {
      if (this.overflow && this.dice() < this.overflow) {
        return answer(last, [...olds, this.remember(rec, { from: k.n, to: last, why: "overflow", reason: "the input queue is full" }, true)]);
      }
      this.typed.push({ n: k.n, event: k.event, at: now, replay: !!m.replay, token: this.token });
    }
    return answer(last, olds);
  }
  judge(known, rest, replay, dnow) {
    const first = rest[0];
    const late = typeof first.typed_ms === "number" ? Math.max(0, dnow - first.typed_ms) : 0;
    const changed = rest.some((k) => k.token !== this.token);
    if (!replay) {
      if (late > LATE_LIMIT_MS) return { why: "stale", reason: "arrived late" };
      if (late > REPLAY_WINDOW_MS && changed) return { why: "changed", reason: "arrived late, and changed" };
      return null;
    }
    const typedAt = Math.min(dnow - first.age_ms, typeof first.typed_ms === "number" ? first.typed_ms : Infinity);
    if (!known && this.floor + SLACK_MS >= typedAt) return { why: "unknown", reason: "can't tell" };
    if (late > LATE_LIMIT_MS) return { why: "stale", reason: "arrived late" };
    if (first.age_ms > REPLAY_WINDOW_MS) return { why: "stale", reason: "too old" };
    if (changed) return { why: "changed", reason: "changed" };
    return null;
  }
}

// ── a page, a network and a clock ──────────────────────────────────────────────────────────
function world({ numbered = true, maxBytes = MAX_BYTES } = {}) {
  const d = new Daemon();
  d.numbered = numbered;
  const w = {
    d, now: 0, up: false, toServer: [], toClient: [], held: [], timer: null, checks: 0, frame: null,
    limit: maxBytes, oversize: [], gaps: [], failNext: 0, io: null, o: null,
  };
  w.io = {
    send: (m) => {
      if (Number(process.env.SEED ?? 0)) console.log(w.now, "send", w.up, m.t, m.replay ? "replay" : "", JSON.stringify(m.keys?.map((k) => k.n)));
      if (!w.up) return false;
      if (w.failNext > 0) { w.failNext--; return false; }
      const bytes = utf8.encode(JSON.stringify(m)).length;
      // Over the server's limit: conn.ts doesn't send it (and says so). The outbox never asks.
      if (bytes > w.limit) { w.oversize.push(bytes); return false; }
      // Live keys go only behind keys that went ahead of them on this connection (M1).
      if (m.t === "keys" && !m.replay) {
        const behind = w.o.queue.filter((k) => k.n < m.keys[0].n && k.sent === null);
        if (behind.length || w.o.replayTo) w.gaps.push(`live ${m.keys[0].n} past ${behind.map((k) => k.n)} (replay to ${w.o.replayTo})`);
      }
      w.toServer.push(m);
      return true;
    },
    now: () => w.now,
    seen: () => (w.frame ? { token: w.frame.token, clock: w.frame.at + (w.now - w.frame.rx) } : { token: null, clock: null }),
    changed: () => {},
    check: () => { w.checks++; },
    later: (ms) => { w.timer = w.now + ms; },
  };
  w.o = new Outbox("s1", w.io, "stream1", maxBytes);
  /// A frame arrives: the session as it is now, on the daemon's clock.
  w.draw = () => { w.frame = { token: d.token, at: d.clock(w.now), rx: w.now }; };
  /// Deliver what is in flight (both ways), in order.
  w.pump = () => {
    if (w.blackhole) { w.toServer = []; return; }
    while (w.toServer.length || w.toClient.length) {
      const m = w.toServer.shift();
      if (m) {
        if (Number(process.env.SEED ?? 0)) console.log(w.now, "deliver", m.replay ? "replay" : "", JSON.stringify(m.keys?.map((k) => k.n)));
        if (m.t === "keys") w.toClient.push(d.keys(m, w.now));
        else if (m.t === "input") d.plain.push(m.event);
      }
      const a = w.toClient.shift();
      if (a && Number(process.env.SEED ?? 0)) console.log(w.now, "answer", JSON.stringify(a));
      if (a) w.o.typed(a);
    }
    w.draw();
  };
  w.connect = () => { w.up = true; w.draw(); w.o.connected(d.numbered); };
  /// The connection goes: the first `keep` messages to the daemon still arrive (`late` ms from
  /// now: a dying connection holding them), the rest and every answer on its way are lost.
  w.drop = (keep = 0, late = 0) => {
    const arrive = w.toServer.splice(0, keep);
    w.toServer = [];
    if (late > 0) for (const m of arrive) w.held.push({ at: w.now + late, m });
    else for (const m of arrive) if (m.t === "keys") d.keys(m, w.now); // answered into a dead socket
    w.toClient = [];
    w.up = false;
    w.o.disconnected();
  };
  /// Held messages whose time has come reach the daemon (their answers go nowhere).
  const arrive = (until) => {
    w.held.sort((a, b) => a.at - b.at);
    while (w.held.length && w.held[0].at <= until) {
      const { at, m } = w.held.shift();
      const was = w.now;
      w.now = Math.max(w.now, at);
      if (m.t === "keys") d.keys(m, w.now);
      w.now = was;
    }
  };
  w.wait = (ms) => {
    const end = w.now + ms;
    for (;;) {
      const t = w.timer !== null && w.timer <= end ? w.timer : null;
      const h = w.held.length ? Math.min(...w.held.map((x) => x.at)) : null;
      if (h !== null && h <= end && (t === null || h <= t)) { arrive(h); w.now = Math.max(w.now, h); continue; }
      if (t === null) break;
      w.now = Math.max(w.now, t); w.timer = null; w.o.tick(); if (w.up) w.pump();
    }
    w.now = end;
  };
  w.text = () => describe(d.typed.map((k) => k.event));
  return w;
}
const key = (c) => ({ t: "key", key: { code: c === "\n" ? { t: "enter" } : { t: "char", c }, mods: 0 } });
const typeText = (w, s) => { for (const c of s) w.o.type(key(c)); };
/// No message over the limit, no live key past one waiting, nothing malformed.
const clean = (w, ctx = "") => {
  assert.deepEqual(w.oversize, [], `a message over the limit ${ctx}`);
  assert.deepEqual(w.gaps, [], `a live key past one waiting ${ctx}`);
  assert.deepEqual(w.d.violations, [], `malformed ${ctx}`);
};

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

{
  // M1: two pastes too big for one message together, typed offline: sent again as two messages
  // (by size, as by count), both typed, in order; a key typed meanwhile waits behind them and
  // goes after, never past them.
  const w = world({ maxBytes: 4096 });
  w.connect();
  w.drop();
  const big = (c) => ({ t: "paste", text: c.repeat(2500) });
  w.o.type(big("a"));
  w.o.type(big("b"));
  w.wait(300);
  w.connect();
  assert.equal(w.toServer.length, 1, "one message's worth at a time");
  assert.equal(w.toServer[0].keys.length, 1);
  typeText(w, "x"); // typed on the new connection, behind both
  assert.equal(w.toServer.length, 1, "it waits behind the replay");
  w.pump();
  assert.equal(w.text(), `${"a".repeat(2500)}${"b".repeat(2500)}x`);
  assert.equal(w.o.waiting, 0);
  assert.equal(w.o.lost.length, 0);
  clean(w);
  // At the server's real limit too: two 2.5 MiB pastes go as two messages.
  const v = world();
  v.connect();
  v.drop();
  v.o.type({ t: "paste", text: "p".repeat(2_500_000) });
  v.o.type({ t: "paste", text: "q".repeat(2_500_000) });
  v.connect();
  v.pump();
  assert.equal(v.d.typed.length, 2);
  clean(v);
}
{
  // M1: a send that didn't leave on an open connection isn't stuck: it goes again, and keys
  // typed meanwhile queue behind it (never as a live gap).
  const w = world();
  w.connect();
  w.failNext = 1;
  typeText(w, "a");
  typeText(w, "b");
  assert.deepEqual(w.toServer.map((m) => m.keys.map((k) => k.n)), [[1, 2]], "b goes with a, behind it, never alone");
  w.pump();
  assert.equal(w.text(), "ab");
  const v = world();
  v.connect();
  v.failNext = 2;
  typeText(v, "a");
  typeText(v, "b");
  assert.equal(v.toServer.length, 0);
  v.wait(WAIT_MS + 50); // (the world delivers what a tick sends)
  assert.equal(v.text(), "ab");
  assert.equal(v.o.waiting, 0);
  clean(w);
  clean(v);
  // A paste too large for any message: said at once, never kept, and what follows goes.
  const u = world({ maxBytes: 4096 });
  u.connect();
  u.o.type({ t: "paste", text: "z".repeat(5000) });
  assert.equal(u.o.waiting, 0);
  assert.equal(u.o.lost[0].why, "other");
  assert.match(lostTitle(u.o.lost[0]), /^A paste wasn't sent: it is too large to send from here/);
  typeText(u, "k");
  u.pump();
  assert.equal(u.text(), "k");
}
{
  // Keys carry what the page last drew: its frame's token, and when they were typed on the
  // daemon's clock (the frame's time plus the time since it came), live or replayed.
  const w = world();
  w.connect();
  w.now += 700;
  typeText(w, "a");
  const k = w.toServer[0].keys[0];
  assert.equal(k.token, "t0");
  assert.equal(k.typed_ms, w.d.clock(w.now));
  assert.equal(w.toServer[0].replay, undefined);
  // A list's newer token isn't the page's: only a frame moves it.
  w.d.token = "t1";
  typeText(w, "b");
  assert.equal(w.toServer[1].keys[0].token, "t0");
}
{
  // M2: live keys held by a dying connection (a phone in a tunnel) arrive 20 s later, into a
  // session that asks something new by then: not typed. The page, back, asks about them and is
  // told they weren't typed.
  const w = world();
  w.connect();
  typeText(w, "y\n");
  w.drop(2, 20_000);
  w.d.token = "t1"; // a permission prompt
  w.wait(25_000);
  assert.equal(w.d.typed.length, 0, "late keys into a changed session aren't typed");
  w.connect();
  w.pump();
  assert.equal(w.o.waiting, 0);
  assert.equal(w.o.lost[0].why, "stale");
  // Held 7 s into a session that didn't change (a slow link): typed.
  const v = world();
  v.connect();
  typeText(v, "ok");
  v.drop(2, 7_000);
  v.wait(8_000);
  assert.equal(v.text(), "ok");
  // Held 7 s into one that changed: refused.
  const u = world();
  u.connect();
  typeText(u, "ok");
  u.drop(2, 7_000);
  u.d.token = "t2";
  u.wait(8_000);
  assert.equal(u.d.typed.length, 0);
}
{
  // M2: fast typing is never refused, whatever moves the token: Enter starts a turn (the token
  // changes at once) and the next keys, typed before the page saw the new one, go.
  const w = world();
  w.connect();
  typeText(w, "go\n");
  w.pump();
  w.d.token = "t-working"; // the turn began; no frame yet
  typeText(w, "and more");
  w.now += 80;
  w.pump();
  assert.equal(w.text(), "go⏎\nand more");
  assert.equal(w.o.lost.length, 0);
}
{
  // A refused live key with more on their way behind it: those aren't typed either (the page
  // says they weren't sent); a key typed once the page heard of it goes. Nothing cascades.
  const w = world();
  w.connect();
  typeText(w, "a");
  w.drop(1, 7_000); // `a` held 7 s
  w.d.token = "t1";
  w.wait(100);
  w.connect();
  // (The replay of `a` waits in flight; the held copy arrives first, late, and is refused.)
  w.wait(7_000);
  w.pump();
  assert.equal(w.d.typed.length, 0);
  assert.equal(w.o.lost.length, 1);
  typeText(w, "b");
  typeText(w, "c");
  w.pump();
  assert.equal(w.text(), "bc");
  // Overflow: a key the session's input queue drops is refused, and one in flight behind it too.
  const v = world();
  v.connect();
  let n = 0;
  v.d.overflow = 1; v.d.dice = () => (n++ === 0 ? 0 : 1); // only the first key overflows
  typeText(v, "xy");
  v.pump();
  assert.equal(v.d.typed.length, 0, "y was typed before the page heard x was dropped");
  assert.equal(v.o.lost[0].why, "overflow");
  assert.match(lostTitle(v.o.lost[0]), /isn't reading/);
  assert.equal(describe(v.o.lost.flatMap((l) => l.events)), "xy");
  typeText(v, "z");
  v.pump();
  assert.equal(v.text(), "z");
}
{
  // ✎ compose waits for the keys typed before it: settled, or not (then it isn't sent).
  const w = world();
  w.connect();
  let got = null;
  w.o.settled().then((ok) => { got = ok; });
  await null;
  assert.equal(got, true, "nothing waiting: at once");
  typeText(w, "ab");
  got = null;
  w.o.settled().then((ok) => { got = ok; });
  await null;
  assert.equal(got, null, "keys on their way: it waits");
  w.pump();
  await null;
  assert.equal(got, true);
  w.drop();
  typeText(w, "cd");
  w.d.token = "t1";
  got = null;
  w.o.settled().then((ok) => { got = ok; });
  w.wait(300);
  w.connect();
  w.pump();
  await null;
  assert.equal(got, false, "a key before it was lost");
}
{
  // Lost keys are hidden until revealed (a password typed at a prompt that doesn't echo).
  assert.equal(mask("hunter2⏎\nls"), "••••••••\n••");
}

// ── property test ──────────────────────────────────────────────────────────────────────────
// A seeded generator, so a failure names its run.
function rng(seed) { let s = seed >>> 0; return () => { s = (s * 1664525 + 1013904223) >>> 0; return s / 2 ** 32; }; }
let runs = 0;
const only = Number(process.env.SEED ?? 0);
const log = (...a) => { if (only) console.log(...a); };
for (let seed = only || 1; seed <= (only || 10000); seed++) {
  const r = rng(seed);
  // A small limit, so pastes need messages of their own and replays split.
  const w = world({ maxBytes: 1200 });
  if (r() < 0.3) { w.d.overflow = 0.02; w.d.dice = r; }
  const typedAt = new Map(); // n → when the person typed it
  const seenAt = new Map(); // n → the token its page had drawn
  const lostAt = new Map(); // n → when it was said not sent
  let n = 0;
  const watchLost = () => {
    for (const l of w.o.lost) for (const e of l.events) if (!lostAt.has(e.n)) lostAt.set(e.n, { at: w.now, why: l.why });
  };
  const arrivals = []; // [n, when it reached the daemon] for keys typed
  if (r() < 0.8) w.connect();
  for (let step = 0; step < 60; step++) {
    const x = r();
    log(w.now, "step", x.toFixed(3), "up", w.up, "typed", JSON.stringify(w.d.typed.map((k) => k.event.n)));
    if (x < 0.45) {
      n++;
      typedAt.set(n, w.now);
      seenAt.set(n, w.io.seen().token);
      const big = r() < 0.06;
      w.o.type(big ? { t: "paste", text: "p".repeat(Math.floor(r() * 1300)), n } : { t: "key", key: { code: { t: "char", c: "k" }, mods: 0 }, n });
    } else if (x < 0.55 && w.up) {
      // Lost, or held by a dying connection for up to 25 s.
      w.drop(Math.floor(r() * (w.toServer.length + 1)), r() < 0.4 ? Math.floor(r() * 25_000) : 0);
      // The daemon starts again from an image without the table (its connections, and what
      // they held, gone with the old one).
      if (r() < 0.05) { w.d.forget(w.now); w.held = []; }
    }
    else if (x < 0.65 && !w.up) w.connect();
    else if (x < 0.7) w.d.token = `t${Math.floor(r() * 3)}`;
    else if (x < 0.8 && w.up) w.pump();
    else if (x < 0.85 && w.up) { w.toClient.splice(0, Math.floor(r() * (w.toClient.length + 1))); } // answers lost, socket open
    else w.wait(Math.floor(r() * 2500));
    watchLost();
  }
  // Settle: back online, time passes, everything delivered.
  if (!w.up) w.connect();
  for (let i = 0; i < 30; i++) { w.pump(); w.wait(1000); }
  w.pump();
  watchLost();
  const typed = w.d.typed.filter((k) => k.event.n !== undefined).map((k) => ({ ...k, id: k.event.n }));
  const ids = typed.map((k) => k.id);
  const ctx = `seed ${seed}: typed ${JSON.stringify(ids)} lost ${JSON.stringify([...lostAt.entries()].map(([k, v]) => [k, v.why]))}`;
  clean(w, ctx);
  // At most once, in order.
  assert.deepEqual(ids, [...new Set(ids)].sort((a, b) => a - b), `duplicates or out of order, ${ctx}`);
  for (const k of typed) {
    const l = lostAt.get(k.id);
    // Never said "not sent" when it was typed ("may not have been sent", and "couldn't tell
    // whether they were typed already", excepted).
    assert.ok(!l || l.why === "silent" || l.why === "unknown", `key ${k.id} typed but said ${l?.why}, ${ctx}`);
    const late = k.at - typedAt.get(k.id);
    // A replayed key, or one that arrived late, was typed only if the session still waited for
    // what its page had drawn when it was typed; none arriving later than the limit.
    if (k.replay || late > REPLAY_WINDOW_MS) assert.equal(k.token, seenAt.get(k.id), `key ${k.id} (${k.replay ? "replayed" : `${late} ms late`}) typed into a session that changed, ${ctx}`);
    assert.ok(late <= LATE_LIMIT_MS, `key ${k.id} typed ${late} ms after it was typed, ${ctx}`);
  }
  for (const [lost, l] of lostAt) {
    if (l.why === "silent" || l.why === "unknown") continue;
    // Nothing typed before it was said lost may come after it: no fragment of a command.
    for (const k of typed) if (k.id > lost) assert.ok(typedAt.get(k.id) >= l.at, `key ${k.id} typed after lost key ${lost} though typed before the loss was known, ${ctx}`);
  }
  // Nothing left waiting, nothing unaccounted for.
  assert.equal(w.o.waiting, 0, `left waiting, ${ctx}`);
  for (let i = 1; i <= n; i++) assert.ok(ids.includes(i) || lostAt.has(i), `key ${i} neither typed nor said, ${ctx}`);
  runs++;
}
console.log(`ok (outbox: scenarios and ${runs} random runs)`);
