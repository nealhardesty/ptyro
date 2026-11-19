// Predicted echo (ROADMAP WEB2.4): web/dist/predict.js's `Predictor` against a simulated world —
// a program behind a terminal (a shell that echoes, a password prompt that doesn't, one that
// echoes upper case or `*`, one that moves the cursor elsewhere, one with autosuggestions, a slow
// one), a daemon that answers keys with `Typed` and sends frames with a few in flight until they
// are acked, and a network with latency. Scenarios first, then a property test of thousands of
// random runs checking that nothing is drawn without proof, that nothing typed at a prompt that
// doesn't echo is ever drawn, that a prediction never outlives a contradicting frame or its
// expiry, that nothing is drawn on a fast link, and that everything settles.
//   node web/test/predict.mjs
import assert from "node:assert/strict";
import { Predictor, ON_MS, OFF_MS, PROOF, MAX_MS, narrow, predictable } from "../dist/predict.js";

// ── keys ───────────────────────────────────────────────────────────────────────────────────
const ch = (c, mods = 0) => ({ t: "key", key: { code: { t: "char", c }, mods } });
const named = (t, mods = 0) => ({ t: "key", key: { code: { t }, mods } });
const BS = named("backspace"), ENTER = named("enter"), LEFT = named("left"), TAB = named("tab");

// ── a random source that can be replayed ───────────────────────────────────────────────────
function rng(seed) {
  let s = seed >>> 0 || 1;
  const next = () => { s ^= s << 13; s >>>= 0; s ^= s >>> 17; s ^= s << 5; s >>>= 0; return s / 4294967296; };
  return { next, int: (a, b) => a + Math.floor(next() * (b - a + 1)), pick: (xs) => xs[Math.floor(next() * xs.length)], chance: (p) => next() < p };
}

// ── the world: a terminal program, a daemon, a network, a page ─────────────────────────────
/// `up`/`down`: one-way latency (ms, or a function); `echo`: the program's delay; `window`: frames
/// in flight; `mode`: what the program does with keys (each Enter may switch it: `w.phase(mode)`).
function world({ cols = 24, rows = 6, up = 100, down = 100, echo = 2, window = 1, mode = "echo", seed = 1, prompt = "$ " } = {}) {
  const r = rng(seed);
  const lat = (v) => (typeof v === "function" ? v(r) : v);
  const w = {
    now: 0, r, events: [], cols, rows, mode, nextMode: [], prompt,
    // the program's terminal (the daemon's emulator)
    grid: [], cur: { x: 0, y: 0, visible: true }, dirty: true, inflight: 0, lastSend: -1e9,
    // the page
    screen: null, sentAt: new Map(), modeOf: new Map(), n: 0, timer: null, typedUpto: 0,
    toServerAt: 0, toClientAt: 0, programAt: 0, seq: 0, log: [], shownChain: 0, shownAny: 0,
  };
  for (let y = 0; y < rows; y++) w.grid.push(Array(cols).fill(" "));
  const write = (s) => { for (const c of s) { if (w.cur.x < cols) w.grid[w.cur.y][w.cur.x] = c; w.cur.x = Math.min(w.cur.x + 1, cols - 1); } };
  const newline = () => {
    w.cur.y++;
    if (w.cur.y >= rows) { w.grid.shift(); w.grid.push(Array(cols).fill(" ")); w.cur.y = rows - 1; }
    w.cur.x = 0;
    write(w.prompt);
  };
  write(prompt);
  w.at = (t, f) => { w.events.push({ t, f, i: w.seq++ }); };
  // The page's view of the screen: the last frame it got.
  const look = () => {
    const s = w.screen;
    if (!s) return null;
    return { cols, rows, cursor: s.cur, alt: false, cell: (x, y) => s.grid[y]?.[x] ?? " " };
  };
  w.look = look;
  w.p = new Predictor({ now: () => w.now, later: (ms) => { w.timer = w.now + ms; } });
  w.p.mode = "auto";

  // ── the program, as the daemon's pty feeds it ──
  const program = (ev, n) => {
    if (process.env.PREDICT_TRACE) console.log(`${w.now} PROGRAM #${n} ${w.modeOf.get(n)} ${JSON.stringify(ev.key?.code)}`);
    const m = w.modeOf.get(n);
    const k = ev.key?.code;
    if (k?.t === "enter") { newline(); return; }
    if (k?.t === "backspace") {
      if (m === "echo" || m === "suggest" || m === "slow" || m === "upper") {
        if (w.cur.x > w.prompt.length) { w.cur.x--; w.grid[w.cur.y][w.cur.x] = " "; if (m === "suggest") for (let x = w.cur.x; x < cols; x++) w.grid[w.cur.y][x] = " "; }
      }
      return;
    }
    if (k?.t === "left") { if (w.cur.x > w.prompt.length) w.cur.x--; return; }
    if (k?.t !== "char") return;
    const c = k.c;
    switch (m) {
      case "noecho": return;
      case "upper": write(c.toUpperCase()); return;
      case "star": write("*"); return;
      case "jump": w.grid[r.int(0, rows - 1)][r.int(0, cols - 1)] = c; w.cur.x = r.int(0, cols - 1); w.cur.y = r.int(0, rows - 1); return;
      case "suggest": {
        write(c);
        // zsh-autosuggestions: the rest of a remembered command, after the cursor.
        const x0 = w.cur.x;
        const s = "uggestion";
        for (let i = 0; i < s.length && x0 + i < cols - 1; i++) w.grid[w.cur.y][x0 + i] = s[i];
        return;
      }
      default: write(c);
    }
  };

  // ── the daemon: Typed at once, frames as the screen changes, `window` in flight ──
  const toClient = (f) => { w.toClientAt = Math.max(w.toClientAt, w.now + lat(down)); w.at(w.toClientAt, f); };
  const toServer = (f) => { w.toServerAt = Math.max(w.toServerAt, w.now + lat(up)); w.at(w.toServerAt, f); };
  const pump = () => {
    if (!w.dirty || w.inflight >= window) return;
    if (w.now - w.lastSend < 8) { w.at(w.lastSend + 8, pump); return; }
    w.dirty = false;
    w.inflight++;
    w.lastSend = w.now;
    const snap = { grid: w.grid.map((row) => row.slice()), cur: { ...w.cur } };
    toClient(() => {
      w.screen = snap;
      w.p.frame(look());
      w.check("frame", snap);
      toServer(() => { w.inflight--; pump(); });
    });
  };
  const changed = () => { w.dirty = true; pump(); };
  w.changed = changed;

  // ── the page types ──
  w.type = (ev) => {
    const n = ++w.n;
    if (process.env.PREDICT_TRACE) console.log(`${w.now} TYPE #${n} ${JSON.stringify(ev.key?.code)}`);
    w.sentAt.set(n, w.now);
    // An Enter switches the program to the next phase's mode for the keys after it.
    if (ev.key?.code?.t === "enter" && w.nextMode.length) w.mode = w.nextMode.shift();
    w.modeOf.set(n, ev.key?.code?.t === "enter" ? "enter" : w.mode);
    const before = w.p.overlay();
    w.p.key(ev, n, look(), false);
    w.check("key");
    void before;
    toServer(() => {
      toClient(() => { w.typedUpto = Math.max(w.typedUpto, n); w.p.acked(n); w.check("typed"); });
      const d = w.modeOf.get(n) === "slow" ? r.int(0, 400) : lat(echo);
      // (A pty is in order: a key is read after the ones before it.)
      w.programAt = Math.max(w.programAt, w.now + d);
      w.at(w.programAt, () => { program(ev, n); changed(); });
    });
  };
  w.text = (s) => { for (const c of s) w.type(ch(c)); };
  w.phase = (m) => { w.nextMode.push(m); };

  /// Run the world until `t` (ms from now).
  w.run = (ms) => {
    const end = w.now + ms;
    for (;;) {
      w.events.sort((a, b) => a.t - b.t || a.i - b.i);
      const nextEv = w.events[0];
      const nextT = Math.min(nextEv ? nextEv.t : Infinity, w.timer ?? Infinity);
      if (nextT > end) break;
      w.now = Math.max(w.now, nextT);
      if (w.timer !== null && w.timer <= w.now && (!nextEv || w.timer <= nextEv.t)) { w.timer = null; w.p.tick(look()); w.check("tick"); continue; }
      w.events.shift();
      nextEv.f();
    }
    w.now = end;
  };
  w.overlay = () => w.p.overlay();
  w.serverText = (y) => w.grid[y].join("").trimEnd();

  // ── what must always hold ──
  w.violations = [];
  w.check = (what, snap) => {
    const o = w.p.overlay();
    if (process.env.PREDICT_TRACE) console.log(`${w.now} ${what} srtt=${Math.round(w.p.srtt)} mode=${w.mode} proof=${w.p.proof} ep=${w.p.epoch} cur=${JSON.stringify(w.screen?.cur)} row=${JSON.stringify(w.screen?.grid[w.screen.cur.y].join(""))} preds=${JSON.stringify(w.p.preds.map((p) => [p.n, w.modeOf.get(p.n), p.x, p.y, p.ch, p.from, p.free ? "F" : ""]))} shown=${JSON.stringify(o.cells)}`);
    const chainCells = o.cells.filter((c) => w.p.chain().some((p) => p.x === c.x && p.y === c.y));
    if (chainCells.length) w.shownChain++;
    if (o.cells.length) w.shownAny++;
    const bad = (m) => w.violations.push(`${what} @${w.now}: ${m}`);
    // Nothing drawn without proof in this epoch, unless freed by a barrier while it was drawn.
    if (chainCells.length && w.p.proof < PROOF) bad(`chain drawn with proof ${w.p.proof}`);
    // Nothing lives past its expiry.
    for (const p of w.p.preds) if (w.now - p.at > MAX_MS + 1) bad(`a prediction ${w.now - p.at} ms old`);
    // Nothing typed where the program doesn't echo is ever drawn.
    for (const c of o.cells) {
      const p = [...w.p.preds].reverse().find((q) => q.x === c.x && q.y === c.y);
      if (p && w.modeOf.get(p.n) === "noecho" && c.t !== " ") bad(`drew '${c.t}' typed at a prompt that doesn't echo`);
    }
    // After a frame: predictions still tied to the cursor wait for an echo that hasn't begun —
    // the cursor is still where the first of them starts. Anything else (it moved past, back,
    // or to another row) contradicted or confirmed them, and they are gone.
    const chain = w.p.chain();
    if (what === "frame" && chain.length && (snap.cur.y !== chain[0].y || snap.cur.x !== chain[0].from)) {
      bad(`predictions outlived a frame with the cursor at ${snap.cur.x},${snap.cur.y}: ${JSON.stringify(chain)} row ${JSON.stringify(snap.grid[chain[0].y]?.join(""))}`);
    }
  };
  // The first frame: the page subscribes.
  changed();
  return w;
}

const tests = [];
const test = (name, f) => tests.push([name, f]);

// ── the pieces ─────────────────────────────────────────────────────────────────────────────
test("only narrow printable characters and Backspace are predictable", () => {
  for (const c of ["a", "Z", "~", " ", "é", "ß", "ж", "Ω"]) assert.ok(narrow(c), c);
  for (const c of ["中", "😀", "é", "́", "­", "ab", "", "\t"]) assert.ok(!narrow(c), JSON.stringify(c));
  assert.deepEqual(predictable(ch("a")), { t: "char", c: "a" });
  assert.deepEqual(predictable(ch("A", 1)), { t: "char", c: "A" }, "Shift is fine");
  assert.equal(predictable(ch("c", 4)), null, "Ctrl-C isn't");
  assert.equal(predictable(ch("x", 2)), null, "Alt-x isn't");
  assert.deepEqual(predictable(BS), { t: "erase" });
  for (const e of [ENTER, LEFT, TAB, named("backspace", 4), { t: "paste", text: "a" }, { t: "mouse" }, { t: "focus", gained: true }]) assert.equal(predictable(e), null);
});

// ── scenarios ──────────────────────────────────────────────────────────────────────────────
test("on a slow link, an echoing shell's letters show at once after two are confirmed", () => {
  const w = world({ up: 100, down: 100 });
  w.run(300);
  w.text("ab");
  assert.equal(w.overlay().cells.length, 0, "tentative: nothing proven yet");
  w.run(600);
  assert.equal(w.p.proof, 2);
  assert.ok(w.p.slow, `srtt ${w.p.srtt}`);
  w.text("cd");
  const o = w.overlay();
  assert.deepEqual(o.cells.map((c) => [c.x, c.y, c.t]), [[4, 0, "c"], [5, 0, "d"]], "drawn where the cursor will be");
  assert.deepEqual(o.cursor, { x: 6, y: 0 }, "the cursor after them");
  w.run(600);
  assert.equal(w.p.preds.length, 0, "confirmed by the echo");
  assert.equal(w.serverText(0), "$ abcd");
  assert.equal(w.p.stats.misses, 0);
  assert.deepEqual(w.violations, []);
});

test("on a fast link nothing is drawn (the echo is as quick)", () => {
  const w = world({ up: 3, down: 3 });
  w.run(50);
  for (const c of "hello world") { w.type(ch(c)); assert.equal(w.overlay().cells.length, 0); w.run(30); }
  assert.ok(w.p.srtt < OFF_MS, `srtt ${w.p.srtt}`);
  assert.ok(!w.p.slow);
  assert.ok(w.p.stats.confirmed >= 10, "still checked, to measure");
  w.p.force = true;
  w.text("xy");
  assert.equal(w.overlay().cells.length, 2, "forced, it shows");
  assert.deepEqual(w.violations, []);
});

test("the round trip has hysteresis", () => {
  const p = new Predictor({ now: () => 0, later: () => {} });
  const feed = (ms) => p["sample"](ms);
  feed(60);
  assert.ok(p.slow);
  for (let i = 0; i < 3; i++) feed(40);
  assert.ok(p.srtt < ON_MS && p.srtt >= OFF_MS && p.slow, "between the two: stays on");
  for (let i = 0; i < 20; i++) feed(5);
  assert.ok(!p.slow);
});

test("a password prompt (no echo) never shows what is typed, and the miss is counted", () => {
  const w = world();
  w.run(300);
  w.text("sudo ls");
  w.run(800);
  assert.ok(w.p.proof >= 2);
  w.phase("noecho");
  w.type(ENTER);
  w.run(50);
  // Typed at once, before the screen has caught up with the Enter: not even predicted.
  w.text("pa");
  assert.equal(w.p.preds.length, 0);
  w.run(800);
  for (const c of "hunter2") { w.type(ch(c)); assert.equal(w.overlay().cells.filter((x) => x.t !== " " && x.y === 1).length, 0, "nothing drawn"); w.run(70); }
  w.run(2000);
  assert.equal(w.p.preds.length, 0, "gone by their expiry");
  assert.ok(w.p.stats.misses >= 1);
  assert.deepEqual(w.violations, []);
});

test("upper case echo: drawn guesses vanish with the first frame that disagrees", () => {
  const w = world();
  w.run(300);
  w.text("12");
  w.run(800);
  w.phase("upper");
  w.type(ENTER);
  w.run(800);
  w.text("34"); // digits echo as typed: proven
  w.run(800);
  assert.ok(w.p.proof >= 2);
  w.text("ab");
  assert.equal(w.overlay().cells.length, 2, "drawn");
  const misses = w.p.stats.misses;
  // Run until the first frame with the echo arrives.
  for (let i = 0; w.screen.grid[1][4] === " " && i < 1000; i++) w.run(5);
  assert.equal(w.overlay().cells.length, 0, "gone with that frame");
  assert.equal(w.p.stats.misses, misses + 1);
  assert.deepEqual(w.violations, []);
});

test("`*` for each letter (a masked password) is a contradiction", () => {
  const w = world();
  w.run(300);
  w.text("pw");
  w.run(800);
  w.phase("star");
  w.type(ENTER);
  w.run(800);
  for (const c of "secret") { w.type(ch(c)); w.run(60); }
  assert.equal(w.shownChain, 0, "never drawn in the starred epoch");
  w.run(2000);
  assert.ok(w.p.stats.misses >= 1);
  assert.deepEqual(w.violations, []);
});

test("Enter: drawn letters stay until the echo, judged by their cells; a new epoch starts", () => {
  const w = world();
  w.run(300);
  w.text("ec");
  w.run(800);
  w.text("ho");
  assert.equal(w.overlay().cells.length, 2);
  const epoch = w.p.epoch;
  w.type(ENTER);
  assert.ok(w.p.epoch > epoch, "a new epoch");
  assert.equal(w.overlay().cells.length, 2, "what was drawn stays, no flicker");
  assert.equal(w.overlay().cursor, null, "the cursor is the screen's again");
  w.text("ls");
  assert.equal(w.overlay().cells.length, 2, "nothing new drawn after Enter until proven");
  w.run(1500);
  assert.equal(w.p.preds.length, 0);
  assert.equal(w.serverText(0), "$ echo");
  assert.equal(w.serverText(1), "$ ls");
  assert.deepEqual(w.violations, []);
});

test("Enter at the bottom (the screen scrolls): freed guesses go by the strict check, never linger", () => {
  const w = world({ rows: 2 });
  w.run(300);
  w.type(ENTER);
  w.run(500);
  w.text("ab");
  w.run(800);
  w.text("cd");
  assert.equal(w.overlay().cells.length, 2);
  w.type(ENTER);
  w.run(400);
  assert.equal(w.p.preds.length, 0, "gone (confirmed or wiped), not lingering on the new prompt line");
  assert.equal(w.serverText(0), "$ abcd");
  assert.deepEqual(w.violations, []);
});

test("the cursor moved elsewhere by the program: every prediction goes at once", () => {
  const w = world({ seed: 7 });
  w.run(300);
  w.text("ab");
  w.run(800);
  w.phase("jump");
  w.type(ENTER);
  w.run(800);
  for (let i = 0; i < 12; i++) { w.type(ch("x")); w.run(40); }
  w.run(2000);
  assert.equal(w.p.preds.length, 0);
  assert.deepEqual(w.violations, []);
});

test("autosuggestions after the cursor: letters along the line are still confirmed", () => {
  const w = world();
  w.run(300);
  w.text("x");
  w.run(500);
  w.phase("suggest");
  w.type(ENTER);
  w.run(800);
  for (const c of "git status") { w.type(ch(c)); w.run(150); }
  w.run(800);
  assert.equal(w.p.preds.length, 0);
  assert.ok(w.p.stats.confirmed >= 10, `confirmed ${w.p.stats.confirmed}`);
  assert.deepEqual(w.violations, []);
});

test("the right margin is never predicted (the terminal waits to wrap)", () => {
  const w = world({ cols: 8 });
  w.run(300);
  w.text("ab");
  w.run(800);
  w.text("cdef"); // columns 4..7: 7 is the last
  const o = w.overlay();
  assert.deepEqual(o.cells.map((c) => c.x), [4, 5, 6], "not the last column");
  assert.equal(w.p.chain().length, 0, "the barrier freed them");
  w.run(1500);
  assert.deepEqual(w.violations, []);
});

test("Backspace erases what was typed in this epoch, never the prompt", () => {
  const w = world();
  w.run(300);
  w.text("abc");
  w.run(800);
  w.type(BS);
  assert.deepEqual(w.overlay().cells.map((c) => [c.x, c.t]), [[4, " "]], "erases the c");
  assert.deepEqual(w.overlay().cursor, { x: 4, y: 0 });
  w.text("d");
  assert.deepEqual(w.overlay().cells.map((c) => [c.x, c.t]), [[4, "d"]], "a letter over the erased cell");
  w.type(BS);
  w.type(BS);
  w.type(BS);
  assert.deepEqual(w.overlay().cursor, { x: 2, y: 0 }, "back to the prompt's end");
  const before = w.p.stats.barriers;
  w.type(BS); // into the prompt: not predicted
  assert.equal(w.p.stats.barriers, before + 1, "a barrier, not a prediction");
  w.run(1500);
  assert.equal(w.serverText(0), "$");
  assert.deepEqual(w.violations, []);
});

test("autocorrect (two Backspaces and new letters) draws the correction at once", () => {
  const w = world();
  w.run(300);
  w.text("OK ");
  w.run(800);
  w.text("teh");
  w.type(BS); w.type(BS);
  w.text("he");
  assert.deepEqual(w.overlay().cells.map((c) => c.t).join(""), "the");
  w.run(1500);
  assert.equal(w.serverText(0), "$ OK the");
  assert.equal(w.p.stats.misses, 0);
  assert.deepEqual(w.violations, []);
});

test("typing over existing text (the middle of a line) is checked but not drawn", () => {
  const w = world();
  w.run(300);
  w.text("abcdef");
  w.run(800);
  for (let i = 0; i < 4; i++) w.type(LEFT);
  w.run(800);
  w.text("xy");
  w.run(800);
  w.text("zz");
  assert.equal(w.overlay().cells.length, 0, "over text that is there: not drawn");
  w.run(1500);
  assert.deepEqual(w.violations, []);
});

test("a wide character is a barrier", () => {
  const w = world();
  w.run(300);
  w.text("ab");
  w.run(800);
  const e = w.p.epoch;
  w.type(ch("中"));
  assert.ok(w.p.epoch > e);
  w.text("c");
  assert.equal(w.overlay().cells.length, 0);
});

test("a slow program's late echo: expired guesses go, and come back once it proves itself", () => {
  const w = world({ echo: 900 });
  w.run(1500);
  w.text("abcd");
  w.run(3000);
  assert.ok(w.p.stats.misses >= 1, "too late: missed");
  assert.equal(w.p.preds.length, 0);
  assert.deepEqual(w.violations, []);
});

test("a hidden cursor (a program drawing its own): nothing is predicted", () => {
  const w = world();
  w.run(300);
  w.text("ab");
  w.run(800);
  w.cur.visible = false;
  w.changed();
  w.run(300);
  w.text("cd");
  assert.equal(w.overlay().cells.length, 0);
  assert.equal(w.p.chain().length, 0);
});

test("off: nothing is predicted or drawn", () => {
  const w = world();
  w.p.mode = "off";
  w.run(300);
  w.text("abcdef");
  w.run(800);
  w.text("gh");
  assert.equal(w.p.preds.length, 0);
  assert.equal(w.overlay().cells.length, 0);
});

test("blocked (keys waiting, scrolled back, dictation): all go, and nothing is drawn", () => {
  const w = world();
  w.run(300);
  w.text("ab");
  w.run(800);
  w.text("cd");
  assert.equal(w.overlay().cells.length, 2);
  assert.equal(w.p.overlay(true).cells.length, 0, "not drawn while blocked");
  w.p.block();
  assert.equal(w.p.preds.length, 0);
  assert.equal(w.p.proof, 0, "proven again from scratch");
  w.p.key(ch("e"), ++w.n, w.look(), true);
  assert.equal(w.p.preds.length, 0, "a key while blocked isn't predicted");
});

test("predicting never sends anything: it has no way to", () => {
  const p = new Predictor({ now: () => 0, later: () => {} });
  assert.ok(!Object.getOwnPropertyNames(Predictor.prototype).some((m) => /send|input|type/i.test(m)), "no sending method");
  assert.equal(p.key(ch("a"), 1, null, false), false);
});

// ── the property test ──────────────────────────────────────────────────────────────────────
test("random worlds: never drawn without proof, never a password, never past a contradiction or expiry", () => {
  const RUNS = +(process.env.PREDICT_RUNS ?? 5000);
  let drawn = 0, misses = 0, confirmed = 0;
  for (let seed = 1; seed <= RUNS; seed++) {
    if (process.env.PREDICT_SEED && seed !== +process.env.PREDICT_SEED) continue;
    const r = rng(seed * 7919);
    const slow = r.chance(0.7);
    const w = world({
      seed,
      cols: r.int(10, 40),
      rows: r.int(2, 8),
      up: slow ? (q) => q.int(20, 200) : (q) => q.int(0, 8),
      down: slow ? (q) => q.int(20, 200) : (q) => q.int(0, 8),
      // (A slow program makes the echo slow too, and is worth predicting: only on slow links here.)
      echo: slow ? (q) => (q.chance(0.1) ? q.int(0, 300) : q.int(0, 20)) : (q) => q.int(0, 10),
      window: r.int(1, 3),
      mode: r.pick(["echo", "echo", "noecho", "upper", "suggest"]),
    });
    w.run(r.int(0, 400));
    const steps = r.int(10, 80);
    for (let i = 0; i < steps; i++) {
      const x = r.next();
      if (x < 0.06) { w.phase(r.pick(["echo", "echo", "noecho", "upper", "star", "jump", "suggest", ...(slow ? ["slow"] : [])])); w.type(ENTER); }
      else if (x < 0.14) w.type(BS);
      else if (x < 0.16) w.type(LEFT);
      else if (x < 0.17) w.type(ch("c", 4));
      else if (x < 0.18) w.type(ch("中"));
      else w.type(ch(r.pick("abcdefxyz 0123-ABé".split(""))));
      w.run(r.chance(0.3) ? r.int(0, 20) : r.int(20, 400));
    }
    w.run(4000);
    if (w.violations.length) assert.fail(`seed ${seed}: ${w.violations.slice(0, 5).join("\n")}`);
    assert.equal(w.p.preds.length, 0, `seed ${seed}: everything settles`);
    assert.equal(w.overlay().cells.length, 0, `seed ${seed}: nothing left drawn`);
    if (!slow && w.shownAny) assert.fail(`seed ${seed}: drawn on a fast link (srtt ${w.p.srtt})`);
    drawn += w.shownChain;
    misses += w.p.stats.misses;
    confirmed += w.p.stats.confirmed;
  }
  if (!process.env.PREDICT_SEED) assert.ok(drawn > RUNS, `predictions were drawn (${drawn} times): the test tests something`);
  console.log(`  ${RUNS} random worlds: ${confirmed} confirmed, ${misses} misses, drawn at ${drawn} moments`);
});

let failed = 0;
for (const [name, f] of tests) {
  try { f(); console.log(`ok ${name}`); }
  catch (e) { failed++; console.log(`FAIL ${name}\n${e.stack}`); }
}
if (failed) { console.log(`${failed} failed`); process.exit(1); }
console.log(`all ${tests.length} passed`);
