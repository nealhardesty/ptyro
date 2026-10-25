// The web terminal's keyboard and selection against a real daemon (ROADMAP WEB1.8). Run by
// crates/ptyro/tests/m9.rs when Node is there; also by hand:
//   node web/test/term.mjs units                       keys, soft keyboards, words, selections
//   node web/test/term.mjs plan                        {"count":N,"hex":"…"}: the bytes the cases make
//   node web/test/term.mjs type  WS_URL TOKEN SESSION  press the cases' keys in the session
//   node web/test/term.mjs scroll WS_URL TOKEN SESSION fetch the scrollback; print what a select copies
// Keys go through web/dist/keys.js exactly as the page sends them; the daemon encodes them against
// the program's modes. The Rust test compares what the program received (`od` of its raw input)
// with `plan`, and a selection's text with `ptyro read --scrollback`.
import assert from "node:assert/strict";
import { MOD, Sticky, inputKeys, keyInput, keyOf, pasteInput } from "../dist/keys.js";
import { docOf, selText, wordAt, lastUsed, ordered, span, urlAt, findIn } from "../dist/grid.js";

const dom = (key, o = {}) => ({ key, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, getModifierState: (k) => k === "AltGraph" && !!o.altGraph, ...o });
const hex = (...b) => b.map((x) => x.toString(16).padStart(2, "0"));
const utf8 = (s) => [...new TextEncoder().encode(s)];

// What each case sends, and the bytes the program must see (DECCKM off, bracketed paste on).
const sticky = new Sticky();
const cases = [
  ["h", () => [keyOf(dom("h"))], utf8("h")],
  ["i", () => [keyOf(dom("i"))], utf8("i")],
  ["shift+I", () => [keyOf(dom("I", { shiftKey: true }))], utf8("I")],
  ["é", () => [keyOf(dom("é"))], utf8("é")],
  ["Enter", () => [keyOf(dom("Enter"))], [0x0d]],
  ["Tab", () => [keyOf(dom("Tab"))], [0x09]],
  ["Shift+Tab", () => [keyOf(dom("Tab", { shiftKey: true }))], [0x1b, 0x5b, 0x5a]],
  ["Backspace", () => [keyOf(dom("Backspace"))], [0x7f]],
  ["Escape", () => [keyOf(dom("Escape"))], [0x1b]],
  ["ArrowUp", () => [keyOf(dom("ArrowUp"))], [0x1b, 0x5b, 0x41]],
  ["ArrowLeft", () => [keyOf(dom("ArrowLeft"))], [0x1b, 0x5b, 0x44]],
  ["Home", () => [keyOf(dom("Home"))], [0x1b, 0x5b, 0x48]],
  ["Delete", () => [keyOf(dom("Delete"))], [0x1b, 0x5b, 0x33, 0x7e]],
  ["Ctrl+C", () => [keyOf(dom("c", { ctrlKey: true }))], [0x03]],
  ["Ctrl+A", () => [keyOf(dom("a", { ctrlKey: true }))], [0x01]],
  ["Alt+x", () => [keyOf(dom("x", { altKey: true }))], [0x1b, 0x78]],
  ["Option+a on a Mac (å)", () => [keyOf(dom("å", { altKey: true, code: "KeyA" }))], [0x1b, 0x61]],
  ["F1", () => [keyOf(dom("F1"))], [0x1b, 0x4f, 0x50]],
  ["sticky Ctrl then d", () => { sticky.tap(MOD.CTRL); const k = keyOf(dom("d"), sticky.mods); sticky.spent(); return [k]; }, [0x04]],
  ["soft keyboard types ok", () => inputKeys("insertText", "ok"), utf8("ok")],
  ["soft keyboard backspace", () => inputKeys("deleteContentBackward", null), [0x7f]],
  ["Android keydown 229 is left to beforeinput", () => [keyOf(dom("Unidentified", { keyCode: 229 }))], []],
  ["Cmd+C is the browser's", () => [keyOf(dom("c", { metaKey: true }))], []],
  ["Shift alone is no key", () => [keyOf(dom("Shift", { shiftKey: true }))], []],
];
const pasteBytes = [0x1b, 0x5b, 0x32, 0x30, 0x30, 0x7e, ...utf8("xy"), 0x1b, 0x5b, 0x32, 0x30, 0x31, 0x7e];

const events = () => {
  const out = [];
  for (const [, make] of cases) for (const k of make()) if (k) out.push(keyInput(k));
  out.push(pasteInput("xy"));
  return out;
};
const expected = cases.flatMap(([, , b]) => b).concat(pasteBytes);

const mode = process.argv[2];
if (mode === "units") {
  // Keys.
  for (const [name, make, bytes] of cases) assert.equal(make().filter(Boolean).length > 0, bytes.length > 0, name);
  assert.deepEqual(keyOf(dom("c", { ctrlKey: true, shiftKey: true })), { code: { t: "char", c: "c" }, mods: MOD.CTRL | MOD.SHIFT });
  assert.deepEqual(keyOf(dom("ArrowUp", { shiftKey: true })), { code: { t: "up" }, mods: MOD.SHIFT });
  assert.deepEqual(keyOf(dom("F12")), { code: { t: "f", n: 12 }, mods: 0 });
  assert.deepEqual(pasteInput("a\r\nb\rc"), { t: "paste", text: "a\nb\nc" });
  // AltGr and a layout's Option characters are typed characters, not Ctrl+Alt+letter / Alt+letter.
  assert.deepEqual(keyOf(dom("@", { ctrlKey: true, altKey: true, altGraph: true, code: "KeyQ" })), { code: { t: "char", c: "@" }, mods: 0 });
  assert.deepEqual(keyOf(dom("|", { altKey: true, code: "Digit7" })), { code: { t: "char", c: "|" }, mods: 0 });
  assert.deepEqual(keyOf(dom("x", { altKey: true, code: "KeyX" })), { code: { t: "char", c: "x" }, mods: MOD.ALT }, "Alt+x on a US layout is still Alt+x");
  // A sticky modifier is for the first character of an inserted run.
  assert.deepEqual(inputKeys("insertText", "ab", MOD.CTRL).map((k) => k.mods), [MOD.CTRL, 0]);
  assert.deepEqual(inputKeys("insertText", "a\n").map((k) => k.code.t), ["char", "enter"]);
  // Sticky modifiers: once, then held, then off.
  const s = new Sticky();
  s.tap(MOD.CTRL); assert.ok(s.has(MOD.CTRL) && !s.locked(MOD.CTRL));
  s.spent(); assert.ok(!s.has(MOD.CTRL));
  s.tap(MOD.ALT); s.tap(MOD.ALT); s.spent(); assert.ok(s.locked(MOD.ALT) && s.has(MOD.ALT));
  s.tap(MOD.ALT); assert.ok(!s.has(MOD.ALT));
  // Words and selections over a document.
  const cells = (t) => [...t].map((c) => ({ t: c, s: 0 }));
  const row = cells("see /usr/bin/env, then ok");
  assert.deepEqual(wordAt(row, 6), [4, 15]);
  assert.deepEqual(wordAt(row, 3), [3, 3]);
  assert.equal(lastUsed(cells("abc   ")), 2);
  const rows = ["hello world", "second line", "wrapped li", "ne here"].map(cells);
  const wraps = [false, false, true, false];
  assert.equal(selText(rows, wraps, { a: { row: 0, col: 6 }, b: { row: 1, col: 5 } }), "world\nsecond");
  assert.equal(selText(rows, wraps, { a: { row: 2, col: 0 }, b: { row: 3, col: 6 } }), "wrapped line here");
  assert.equal(selText(rows, wraps, { a: { row: 1, col: 4 }, b: { row: 0, col: 0 } }), "hello world\nsecon", "either order");
  assert.equal(selText(rows, wraps, { a: { row: 0, col: 2 }, b: { row: 1, col: 4 }, block: true }), "llo\ncon");
  assert.deepEqual(span({ a: { row: 0, col: 3 }, b: { row: 2, col: 1 } }, 1, 11), [0, 11]);
  assert.deepEqual(ordered({ a: { row: 2, col: 1 }, b: { row: 0, col: 3 } })[0], { row: 0, col: 3 });
  // A wide character and its spacer are one cell to select and one to copy.
  const wide = [{ t: "漢", s: 0, wide: true }, { t: "", s: 0, spacer: true }, { t: "x", s: 0 }];
  assert.equal(selText([wide], [false], { a: { row: 0, col: 0 }, b: { row: 0, col: 2 } }), "漢x");
  assert.deepEqual(wordAt(wide, 1), [0, 1]);
  // Links: not the full stop after one, and the cells they take.
  const link = cells("see https://example.com/a?b=1, or (http://x.org/y). ok");
  assert.deepEqual(urlAt(link, 12), { from: 4, to: 28, url: "https://example.com/a?b=1" });
  assert.equal(urlAt(link, 1), null);
  assert.equal(urlAt(link, link.map((c) => c.t).join("").indexOf("x.org")).url, "http://x.org/y");
  // Find: forward, backward, round the end, and the cell span of the match.
  const doc = ["one fish", "two fish", "red fish", "blue"].map(cells);
  assert.deepEqual(findIn(doc, "fish", { row: 0, col: 0 }, 1), { a: { row: 0, col: 4 }, b: { row: 0, col: 7 } });
  assert.deepEqual(findIn(doc, "FISH", { row: 0, col: 4 }, 1).a, { row: 1, col: 4 });
  assert.deepEqual(findIn(doc, "fish", { row: 2, col: 4 }, 1).a, { row: 0, col: 4 }, "wraps round");
  assert.deepEqual(findIn(doc, "fish", { row: 1, col: 4 }, -1).a, { row: 0, col: 4 });
  assert.equal(findIn(doc, "cat", { row: 0, col: 0 }, 1), null);
  console.log("ok");
} else if (mode === "plan") {
  console.log(JSON.stringify({ count: expected.length, hex: hex(...expected).join(" ") }));
} else if (mode === "type" || mode === "scroll") {
  const [url, token, name] = process.argv.slice(3);
  const ws = new WebSocket(url);
  let id = 0, session = null;
  const timer = setTimeout(() => { console.error("timed out"); process.exit(2); }, 20000);
  ws.onopen = () => ws.send(JSON.stringify({ t: "hello", proto: { major: 1, minor: 8 }, client: { kind: "web", version: "test", pid: 0, token }, features: [] }));
  ws.onmessage = async (ev) => {
    const m = JSON.parse(ev.data);
    if (m.t === "welcome") ws.send(JSON.stringify({ t: "request", id: ++id, req: { t: "list" } }));
    else if (m.t === "reject") { console.error(m.reason); process.exit(3); }
    else if (m.t === "response" && id === 1) {
      session = m.outcome.reply.sessions.find((x) => x.name === name).id;
      ws.send(JSON.stringify({ t: "subscribe", session, size: null, control: false }));
      if (mode === "type") {
        // One message per event, as the page sends them.
        for (const event of events()) ws.send(JSON.stringify({ t: "input", session, event }));
        setTimeout(() => process.exit(0), 600);
      } else {
        ws.send(JSON.stringify({ t: "request", id: ++id, req: { t: "read_scrollback", addr: session, lines: 5000 } }));
      }
    } else if (m.t === "response" && id === 2) {
      const doc = docOf(m.outcome.reply.doc);
      const last = doc.rows.length - 1;
      // From the first line of the numbers to the last: every line, in order.
      const all = selText(doc.rows, doc.wraps, { a: { row: 0, col: 0 }, b: { row: last, col: doc.size.cols - 1 } }, doc.size.cols);
      clearTimeout(timer);
      process.stdout.write(JSON.stringify({ history: doc.history, rows: doc.rows.length, text: all }) + "\n");
      process.exit(0);
    }
  };
} else {
  console.error("usage: term.mjs units|plan|type|scroll");
  process.exit(64);
}
