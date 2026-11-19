// Soft keyboards, compositions and dictation (ROADMAP WEB2.1): web/dist/keys.js's `Typist`, fed
// the events real keyboards send, against a line editor that takes what the page would send.
// Run from web/test/term.mjs (units), so `make web-test` runs it.
//
// The traces are written from the event orders the browsers document and that are seen with
// each keyboard (Chromium's Android IME adapter: a keydown of 229 before each IME operation, a
// composition per word, commits as compositionend; WebKit: real keydowns from the iPhone's
// keyboard, marked text for dictation and input methods). A trace is a list of steps:
//   ["down", key, {keyCode, ...}]  a keydown
//   ["cs"] / ["cu", text] / ["ce", text?]   composition start / the composition now reads `text` /
//                                  end (committing `text`, when given, first)
//   ["sce", text]                  Safari's end: compositionend, then the commit (insertFromComposition)
//   ["bi", inputType, data, {cancelable}]   a beforeinput; when the page lets it through, the
//                                  field does what it says and an input event follows
//   ["repl", value]                the keyboard rewrites the field (insertReplacementText)
//   ["set", value]                 the field's text set with only an input event (accessibility)
//   ["paste", text]                Cmd/Ctrl-V: the paste event
//   ["row", key]                   a key from the key row
//   ["wait", ms]                   time passes; timers due run
//   ["blur"]
// Between steps no time passes and no timer runs unless a wait says so (the same task).
import assert from "node:assert/strict";
import { MOD, Typist, SETTLE_MS, LIVE_MAX, dictated, straighten, multiword, hasCJK, strayKey, hardKey, ownKeyboard, autoFocus, shellDigit, keyOf } from "../dist/keys.js";

const dom = (key, o = {}) => ({ key, ctrlKey: false, altKey: false, shiftKey: false, metaKey: false, getModifierState: () => false, ...o });

/// A field, a keyboard's view of it, and the session: what it was sent, as a line editor.
export function rig({ soft = true, bracketed = true, sticky = 0, prompt = "$ " } = {}) {
  const r = { value: "", now: 1000, due: null, out: [], previews: [], violations: [], composing: false, compFrom: 0, held: null, editor: [...prompt], prompt: [...prompt].length, enters: 0 };
  const apply = (o) => {
    if (o.paste !== undefined) { r.editor.push(...o.paste); return; }
    if (o === "<backspace>") {
      if (r.editor.length <= r.prompt) r.violations.push("a backspace into the prompt");
      else r.editor.pop();
      return;
    }
    if (o === "<enter>") { r.enters++; r.editor.push("\n"); return; }
    if (o === "<tab>") { r.editor.push("\t"); return; }
    if (typeof o === "string" && o.startsWith("<")) return; // Esc, arrows: no text
    r.editor.push(o);
  };
  const host = {
    value: () => r.value,
    clear: () => { if (r.composing) r.violations.push("the field emptied mid-composition"); r.value = ""; },
    key: (k, st) => {
      const mods = k.mods | (st ? sticky : 0);
      const o = k.code.t === "char" ? (mods & (MOD.CTRL | MOD.ALT) ? `<${mods}+${k.code.c}>` : k.code.c) : `<${k.code.t}${mods ? `:${mods}` : ""}>`;
      r.out.push(o);
      apply(o);
    },
    paste: (text) => { r.out.push({ paste: text }); apply({ paste: text }); },
    preview: (text, why) => { r.held = text; if (text) r.previews.push([text, why]); },
    sticky: () => sticky,
    bracketed: () => bracketed,
    soft: () => soft,
    now: () => r.now,
    later: (ms) => { r.due = r.now + ms; },
  };
  const t = new Typist(host);
  const input = (composing) => t.input(composing);
  const fire = () => { while (r.due !== null && r.due <= r.now) { r.due = null; t.tick(); } };
  const setComp = (text) => {
    t.beforeinput("insertCompositionText", text, true, false);
    r.value = r.value.slice(0, r.compFrom) + text;
    input(true);
  };
  const step = (s) => {
    const [op, a, b] = s;
    switch (op) {
      case "down": return t.keydown(dom(a, b));
      case "cs": r.composing = true; r.compFrom = r.value.length; t.compositionstart(); return;
      case "cu": setComp(a); return;
      case "ce":
        if (a !== undefined && r.value.slice(r.compFrom) !== a) setComp(a);
        r.composing = false;
        t.compositionend();
        return;
      case "sce": // Safari: compositionend first, then the commit's own input
        r.composing = false;
        t.compositionend();
        if (!t.beforeinput("insertFromComposition", a, false, true)) { r.value = r.value.slice(0, r.compFrom) + a; input(false); }
        return;
      case "bi": {
        const handled = t.beforeinput(a, s[2] ?? null, false, s[3]?.cancelable ?? true);
        if (handled) return;
        if (a === "insertText" || a === "insertReplacementText") r.value += s[2] ?? "";
        else if (a === "insertLineBreak" || a === "insertParagraph") r.value += "\n";
        else if (a === "deleteContentBackward") r.value = [...r.value].slice(0, -1).join("");
        input(false);
        return;
      }
      case "repl": t.beforeinput("insertReplacementText", a, false, true) || ((r.value = a), input(false)); return;
      case "set": r.value = a; input(false); return;
      case "paste": t.interject(); r.out.push({ paste: a }); apply({ paste: a }); return;
      case "row": t.interject(); host.key({ code: a.length === 1 ? { t: "char", c: a } : { t: a }, mods: 0 }, true); return;
      case "wait": r.now += a; fire(); return;
      case "blur": t.blur(); return;
      default: throw new Error(`step ${op}`);
    }
  };
  return { r, t, step, run: (steps) => { for (const s of steps) step(s); return r; }, text: () => r.editor.slice(r.prompt).join("") };
}

/// A word typed on Gboard (and most Android keyboards): a 229 keydown, then the composition grows.
const gboard = (word, from = "") => {
  const out = [["down", "Unidentified", { keyCode: 229 }], ["cs"]];
  let cur = from;
  for (const c of word.slice(from.length)) { cur += c; out.push(["down", "Unidentified", { keyCode: 229 }], ["cu", cur]); }
  return out;
};
const space229 = [["down", "Unidentified", { keyCode: 229 }], ["bi", "insertText", " "]];
const finish = [["blur"], ["wait", 5000]];

// ── Recorded keyboards ──────────────────────────────────────────────────────────────────────
// [name, options, steps, the session's text after, (the exact keys and pastes sent)]
const P = (t) => ({ paste: t });
export const traces = [
  ["Gboard: letters arrive while the word is composed", {}, [...gboard("ls"), ["ce", "ls"], ...space229, ["down", "Unidentified", { keyCode: 229 }], ["bi", "insertText", "-"], ...gboard("la"), ["ce"], ...finish],
    "ls -la", ["l", "s", " ", "-", "l", "a"]],
  ["Gboard: autocorrect on space is a correction", {}, [...gboard("teh"), ["ce", "the"], ...space229, ...finish],
    "the ", ["t", "e", "h", "<backspace>", "<backspace>", "h", "e", " "]],
  ["Gboard: a suggestion tapped while composing", {}, [...gboard("hel"), ["cu", "hello"], ["ce"], ...finish], "hello", ["h", "e", "l", "l", "o"]],
  ["Gboard: backspace inside the composing word", {}, [...gboard("hel"), ["down", "Unidentified", { keyCode: 229 }], ["cu", "he"], ["cu", "hex"], ["ce"], ...finish],
    "hex", ["h", "e", "l", "<backspace>", "x"]],
  ["Gboard: the composing word removed entirely", {}, [...gboard("ab"), ["cu", "a"], ["cu", ""], ["ce", ""], ...finish],
    "", ["a", "b", "<backspace>", "<backspace>"], "$"],
  ["Gboard: compositionend and the next compositionstart in one task (no timer between)", {}, [...gboard("hi"), ["ce", "hi"], ["cs"], ["cu", " "], ["cu", " yo"], ["ce"], ["wait", 0], ...finish],
    "hi yo"],
  ["Swipe typing: a whole word at once, then a revision", {}, [["cs"], ["cu", "hello"], ["ce", "hello"], ["wait", 10], ...space229, ["cs"], ["cu", "world"], ["cu", "would"], ["ce"], ...finish],
    "hello would", ["h", "e", "l", "l", "o", " ", "w", "o", "r", "l", "d", "<backspace>", "<backspace>", "<backspace>", "u", "l", "d"]],
  ["Samsung: the composition carries its space", {}, [...gboard("hi"), ["cu", "hi "], ["ce", "hi "], ["wait", 0], ...gboard("there"), ["ce"], ...finish], "hi there"],
  ["SwiftKey: commits through insertText that can't be cancelled", {}, [["bi", "insertText", "o", { cancelable: false }], ["bi", "insertText", "k", { cancelable: false }], ...finish], "ok", ["o", "k"]],
  ["iPhone: real keydowns, a predictive word, smart quotes", {}, [["down", "i"], ["down", "t"], ["down", "’"], ["down", "s"], ["down", " "], ["bi", "insertText", "fine "], ["bi", "insertText", "“"], ["down", "”"], ...finish],
    "it's fine \"\""],
  ["iPhone: -- autoformatted to an em dash is --", {}, [["down", "a"], ["down", "-"], ["bi", "deleteContentBackward", null], ["bi", "insertText", "—"], ["down", "b"], ...finish], "a--b"],
  ["iPhone: -- made a dash with no backspace first", {}, [["down", "-"], ["bi", "insertText", "–"], ...finish], "--"],
  ["iPhone: an em dash chosen from the keyboard stays", {}, [["down", "a"], ["down", " "], ["bi", "insertText", "—"], ...finish], "a —"],
  ["iPhone: the double-space full stop is two spaces", {}, [["down", "o"], ["down", "k"], ["down", " "], ["bi", "deleteContentBackward", null], ["bi", "insertText", ". "], ["down", "x"], ...finish], "ok  x"],
  ["Gboard: the double-space full stop is two spaces", {}, [...gboard("ok"), ["ce"], ...space229, ["down", "Unidentified", { keyCode: 229 }], ["bi", "deleteContentBackward", null], ["bi", "insertText", ". "], ...finish], "ok  "],
  ["A typed . then space stays a full stop", {}, [["down", "a"], ["down", "."], ["down", " "], ...finish], "a. "],
  ["iPhone dictation (marked text): previewed, then one paste; the spoken new line presses nothing", {}, [["cs"], ["cu", "Hello"], ["cu", "Hello world"], ["cu", "Hello world new"], ["cu", "Hello, world.\nNext"], ["ce"], ["wait", 0], ...finish],
    "Hello, world.\nNext", ["H", "e", "l", "l", "o", P(", world.\nNext")]],
  ["iPhone dictation revising its first word", {}, [["cs"], ["cu", "their"], ["cu", "there is"], ["cu", "There is a cat"], ["ce"], ["wait", 0], ...finish],
    "There is a cat", ["t", "h", "e", "i", "r", "<backspace>", "<backspace>", "<backspace>", "<backspace>", "<backspace>", P("There is a cat")]],
  ["Gboard voice typing", {}, [["cs"], ["cu", "what time"], ["cu", "what time is it"], ["ce", "what time is it"], ["wait", 0], ...finish], "what time is it", [P("what time is it")]],
  ["Dictation's newline without an Enter key is text, not Enter", {}, [["bi", "insertLineBreak", null], ["wait", SETTLE_MS + 10], ...finish], "\n", [P("\n")]],
  ["Without bracketed paste, a dictated newline is a space", { bracketed: false }, [["cs"], ["cu", "one two"], ["cu", "one two\nthree\n"], ["ce"], ["wait", 0], ...finish], "one two three", [P("one two three")]],
  ["An Enter key whose keydown the keyboard kept (229) still presses Enter", {}, [["down", "Enter", { keyCode: 229 }], ["bi", "insertLineBreak", null], ...finish], "\n", ["<enter>"]],
  ["Superwhisper's iOS keyboard: one insert of the whole text", {}, [["bi", "insertText", "Run the tests and then\ncommit"], ["wait", 100], ["bi", "insertText", " please", {}], ["wait", SETTLE_MS + 10], ...finish],
    "Run the tests and then\ncommit please", [P("Run the tests and then\ncommit please")]],
  ["iOS dictation as inserts the keyboard keeps revising", {}, [["bi", "insertText", "I scream"], ["wait", 200], ["repl", "ice cream"], ["wait", 200], ["repl", "Ice cream."], ["wait", SETTLE_MS + 10], ...finish],
    "Ice cream.", [P("Ice cream.")]],
  ["A dictated phrase then a typed letter: the phrase goes first", {}, [["bi", "insertText", "hello there"], ["wait", 50], ["down", "x"], ...finish], "hello therex", [P("hello there"), "x"]],
  ["Superwhisper paste mode: Cmd-V", {}, [["down", "Meta", { metaKey: true }], ["down", "v", { metaKey: true }], ["paste", "fix the bug\nnow"], ...finish], "fix the bug\nnow", [P("fix the bug\nnow")]],
  ["Superwhisper type mode: keystrokes, Enter included", { soft: false }, [...[..."hi"].map((c) => ["down", c]), ["down", "Enter", { keyCode: 13 }], ...finish], "hi\n", ["h", "i", "<enter>"]],
  ["Wispr Flow / accessibility: text set with only an input event", { soft: false }, [["set", "make it so"], ["wait", SETTLE_MS - 100], ["set", "Make it so."], ["wait", SETTLE_MS + 10], ...finish],
    "Make it so.", [P("Make it so.")]],
  ["A single word set by accessibility is pasted too (no key events said it was typed)", {}, [["set", "ok"], ["wait", SETTLE_MS + 10]], "ok", [P("ok")]],
  ["Emoji from the keyboard's panel", {}, [["bi", "insertText", "\u{1F44D}"], ["bi", "insertText", "\u{1F469}‍\u{1F4BB}"], ...finish], "\u{1F44D}\u{1F469}‍\u{1F4BB}"],
  ["Japanese on a computer: the conversion is sent, never the romaji", { soft: false }, [["cs"], ["cu", "n"], ["cu", "に"], ["cu", "にh"], ["cu", "にほ"], ["cu", "にほn"], ["cu", "にほん"], ["cu", "日本"], ["ce", "日本"], ["wait", 0], ...finish],
    "日本", ["日", "本"]],
  ["Pinyin on a Mac (spaces in the marked text)", { soft: false }, [["cs"], ["cu", "ni"], ["cu", "ni hao"], ["ce", "你好"], ["wait", 0], ...finish], "你好", ["你", "好"]],
  ["Japanese flick on a phone: kana compose, the conversion is sent", {}, [["cs"], ["cu", "に"], ["cu", "にほん"], ["cu", "日本"], ["ce"], ["wait", 0], ...finish], "日本", ["日", "本"]],
  ["Korean on a phone: a syllable at a time", {}, [["cs"], ["cu", "ㅎ"], ["cu", "하"], ["cu", "한"], ["ce"], ["wait", 0], ["cs"], ["cu", "글"], ["ce"], ["wait", 0], ...finish], "한글", ["한", "글"]],
  ["Pinyin on a phone: the first word's letters are corrected, then the keyboard is known", {}, [...gboard("nihao"), ["cu", "你好"], ["ce"], ["wait", 0], ...gboard("shi"), ["cu", "是"], ["ce"], ["wait", 0], ...finish],
    "你好是", ["n", "i", "h", "a", "o", "<backspace>", "<backspace>", "<backspace>", "<backspace>", "<backspace>", "你", "好", "是"]],
  ["A dead key on a computer: ´ then e is é", { soft: false }, [["cs"], ["cu", "´"], ["ce", "é"], ["wait", 0], ...finish], "é", ["é"]],
  ["macOS dictation in Chrome: one composition, one paste", { soft: false }, [["cs"], ["cu", "hello"], ["cu", "hello there"], ["ce", "Hello there."], ["wait", 0], ...finish], "Hello there.", [P("Hello there.")]],
  ["Safari: the commit's input after compositionend", {}, [...gboard("ok"), ["ce"], ["bi", "insertFromComposition", "ok"], ["wait", 0], ...finish], "ok", ["o", "k"]],
  ["A key-row key mid-word: the letters before it are never taken back", {}, [...gboard("ab"), ["row", "esc"], ["cu", "abc"], ["cu", ""], ["ce", ""], ["wait", 0], ...finish], "ab", ["a", "b", "<esc>", "c", "<backspace>"]],
  ["A key-row key while an input method composes: the composition isn't cut short", { soft: false }, [["cs"], ["cu", "ni"], ["row", "esc"], ["cu", "\u4f60"], ["ce"], ["wait", 0], ...finish], "\u4f60", ["<esc>", "\u4f60"]],
  ["An input that can't be cancelled is read from the field, which is emptied at once", {}, [["bi", "insertText", "a", { cancelable: false }]], "a", ["a"]],
  ["A sticky Ctrl on the first composed letter: Ctrl+c, never taken back", { sticky: MOD.CTRL }, [["cs"], ["cu", "c"], ["cu", ""], ["ce", ""], ["wait", 0], ...finish], "", [`<${MOD.CTRL}+c>`]],
  ["The ✕ drops held dictation", {}, [["bi", "insertText", "never mind this"], ["discard"], ["wait", SETTLE_MS + 10], ...finish], "", []],
  ["A long single word (over LIVE_MAX) is a paste", {}, [["bi", "insertText", "x".repeat(LIVE_MAX + 1)], ["wait", SETTLE_MS + 10]], "x".repeat(LIVE_MAX + 1), [P("x".repeat(LIVE_MAX + 1))]],
];

function runTrace([name, opts, steps, want, keys, prompt]) {
  const g = rig({ ...opts, prompt: prompt ?? "$ " });
  for (const s of steps) {
    if (s[0] === "discard") g.t.discard(); else g.step(s);
  }
  assert.deepEqual(g.r.violations, [], `${name}: ${g.r.violations.join(", ")}`);
  if (prompt) assert.equal(g.r.editor.join(""), prompt, `${name}: the prompt untouched`);
  else assert.equal(g.text(), want, `${name}: the session's text`);
  if (keys) assert.deepEqual(g.r.out, keys, `${name}: what was sent`);
  assert.equal(g.r.value, "", `${name}: nothing left in the field`);
  assert.equal(g.r.held, null, `${name}: nothing still held`);
  return g;
}

// ── A property: any mix of typing, corrections, dictation and keys ──────────────────────────
// A seeded generator plays a person on a keyboard: words composed letter by letter (with
// autocorrect, backspaces inside the word, suggestions), spaces and punctuation committed
// directly, dictation (growing and revised compositions, big inserts, text set into the field),
// Enter, Backspace, key-row keys, emoji and waits that do or don't let timers run. It keeps what
// a text field would hold (`truth`). The session must end with exactly that, never take a
// character back from before what was typed, and press Enter only when Enter was pressed.
function rng(seed) {
  let x = seed >>> 0 || 1;
  return () => { x ^= x << 13; x >>>= 0; x ^= x >>> 17; x ^= x << 5; x >>>= 0; return x / 4294967296; };
}
const WORDS = ["ls", "git", "status", "the", "cat", "hello", "world", "make", "test", "x", "a", "commit", "été", "naïve", "straße"];
const CJK = ["你好", "日本", "한", "是"];

function property(seed, soft) {
  const rand = rng(seed);
  const pick = (a) => a[Math.floor(rand() * a.length)];
  const g = rig({ soft });
  const { r } = g;
  const step = (s) => g.step(s);
  let truth = [];
  let enters = 0;
  const keydown229 = () => step(["down", "Unidentified", { keyCode: 229 }]);
  const gap = () => { if (rand() < 0.5) step(["wait", Math.floor(rand() * 60)]); };
  const word = () => {
    const w = pick(WORDS);
    keydown229(); step(["cs"]);
    let cur = "";
    for (const c of w) { cur += c; keydown229(); step(["cu", cur]); gap(); }
    const roll = rand();
    if (roll < 0.2) { cur = cur.slice(0, -1) || cur; step(["cu", cur]); }                 // a backspace inside
    else if (roll < 0.35) { cur = pick(WORDS); step(["cu", cur]); }                        // autocorrect / suggestion
    else if (roll < 0.42) { cur = ""; step(["cu", ""]); }                                  // the word wiped
    else if (roll < 0.5) { cur = pick(CJK); step(["cu", cur]); }                           // an input method converts
    if (rand() < 0.2) { const fin = rand() < 0.5 ? cur : pick(WORDS); step(["sce", fin]); cur = fin; }
    else step(["ce"]);
    truth.push(...cur);
  };
  const voice = () => {
    step(["cs"]);
    const words = [];
    let cur = "";
    const n = 1 + Math.floor(rand() * 5);
    for (let i = 0; i < n; i++) {
      words.push(pick(WORDS));
      if (rand() < 0.3) words[Math.floor(rand() * words.length)] = pick(WORDS);              // revised
      cur = words.join(rand() < 0.15 ? "\n" : " ");
      step(["cu", cur]); gap();
    }
    step(["ce"]);
    truth.push(...cur);
  };
  // TYPING_DEBUG=1 prints the steps before a failure.
  const log = [];
  const plain = g.step;
  g.step = (s) => { log.push(JSON.stringify(s)); if (log.length > 60) log.shift(); return plain(s); };
  const fail = (what) => { if (process.env.TYPING_DEBUG) console.error(log.join("\n")); return what; };
  for (let i = 0; i < 60; i++) {
    // Whenever nothing is held or composing, the session already has exactly what was typed.
    if (!g.t.busy && !r.composing) assert.equal(g.text(), truth.join(""), fail(`seed ${seed} (${soft ? "phone" : "computer"}): before action ${i}`));
    const a = rand();
    if (a < 0.3) word();
    else if (a < 0.37) { keydown229(); step(["bi", "insertText", " "]); truth.push(" "); }
    else if (a < 0.4) { const c = pick(["k", " ", "q"]); step(["bi", "insertText", c, { cancelable: false }]); truth.push(c); }  // can't be cancelled
    else if (a < 0.45) { const c = pick([".", ",", "-", "?", "/"]); keydown229(); step(["bi", "insertText", c]); truth.push(c); }
    else if (a < 0.5) { const w = `${pick(WORDS)} `; step(["bi", "insertText", w]); truth.push(...w); }            // a suggestion
    else if (a < 0.57) voice();
    else if (a < 0.62) {                                                                                         // an extension's insert, revised
      const t = `${pick(WORDS)} ${pick(WORDS)}`;
      step(["bi", "insertText", t]);
      let v = t;
      if (rand() < 0.5) {
        // The keyboard rewrites what it inserted (the field may hold earlier dictation before it).
        step(["wait", 100]);
        v = `${pick(WORDS)} ${pick(WORDS)} ${pick(WORDS)}`;
        step(["repl", [...r.value].slice(0, r.value.length ? [...r.value].length - [...t].length : 0).join("") + v]);
      }
      truth.push(...v);
    } else if (a < 0.65) { const t = `${pick(WORDS)} ${pick(WORDS)}`; step(["set", r.value + t]); truth.push(...t); } // accessibility: inserted at the caret
    else if (a < 0.72) { if (truth.length) { step(["down", "Backspace", { keyCode: 8 }]); truth.pop(); } }
    else if (a < 0.76) { if (truth.length) { keydown229(); step(["bi", "deleteContentBackward", null]); truth.pop(); } }
    else if (a < 0.8) { step(["down", "Enter", { keyCode: 13 }]); truth.push("\n"); enters++; }
    else if (a < 0.83) step(["row", pick(["esc", "up", "left"])]);
    else if (a < 0.86) { const e = pick(["\u{1F44D}", "\u{1F469}‍\u{1F4BB}", "❤️"]); step(["bi", "insertText", e]); truth.push(...e); }
    else if (a < 0.9) { const c = pick(["a", "b", "Z", "1"]); step(["down", c]); truth.push(c); }               // a hardware key
    else if (a < 0.95) step(["wait", Math.floor(rand() * (SETTLE_MS * 2))]);
    else step(["blur"]);
  }
  step(["blur"]); step(["wait", SETTLE_MS * 3]);
  const ctx = `seed ${seed} (${soft ? "phone" : "computer"})`;
  assert.deepEqual(r.violations, [], fail(`${ctx}: ${r.violations.join(", ")}`));
  assert.equal(g.text(), truth.join(""), fail(`${ctx}: the session's text`));
  assert.equal(r.enters, enters, `${ctx}: Enter only when pressed`);
  assert.equal(r.value, "", `${ctx}: the field is empty`);
}

export function units() {
  assert.equal(multiword("hello "), false);
  assert.equal(multiword("hello there"), true);
  assert.equal(hasCJK("日本"), true);
  assert.equal(hasCJK("naïve"), false);
  assert.equal(straighten("“it’s”"), "\"it's\"");
  assert.equal(dictated("a\nb\n", false), "a b");
  assert.equal(dictated("a \n\n b", false), "a b");
  assert.equal(dictated("a\r\nb", true), "a\nb");
  for (const t of traces) runTrace(t);
  // Letters arrive while composing: after the second letter, both have gone, the word unfinished.
  const g = rig();
  g.run(gboard("he"));
  assert.deepEqual(g.r.out, ["h", "e"]);
  // Dictation is shown while it is held, then sent once.
  const d = rig();
  d.run([["cs"], ["cu", "one two three"]]);
  assert.equal(d.r.held, "one two three");
  assert.deepEqual(d.r.out, []);
  d.run([["ce"], ["wait", 0]]);
  assert.deepEqual(d.r.out, [{ paste: "one two three" }]);
  assert.equal(d.r.held, null);
  // An input method's composition on a computer is shown (the field is hidden) and sends nothing yet.
  const j = rig({ soft: false });
  j.run([["cs"], ["cu", "にほ"]]);
  assert.deepEqual(j.r.out, []);
  assert.deepEqual(j.r.previews.at(-1), ["にほ", "ime"]);
  terminalFirst();
  altDigits();
  let runs = 0;
  for (let seed = 1; seed <= 400; seed++) { property(seed, true); property(seed * 7919, false); runs += 2; }
  return { traces: traces.length, runs };
}

/// WEB2.2: which keys pressed outside the terminal it takes, and when it takes the focus itself.
function terminalFirst() {
  // From the page itself: letters, Enter, Esc, arrows, Ctrl combinations; not Tab (the way out),
  // a modifier alone or Cmd (the browser's).
  for (const k of ["a", "Z", "1", " ", "Enter", "Escape", "ArrowUp", "Backspace", "F5", "é", "Dead", "Unidentified"]) assert.equal(strayKey(dom(k), "none"), true, k);
  assert.equal(strayKey(dom("c", { ctrlKey: true }), "none"), true, "Ctrl-C interrupts");
  for (const k of ["Tab", "Shift", "Control", "Alt", "Meta", "CapsLock"]) assert.equal(strayKey(dom(k), "none"), false, k);
  assert.equal(strayKey(dom("Tab", { shiftKey: true }), "none"), false);
  assert.equal(strayKey(dom("c", { metaKey: true }), "none"), false, "Cmd-C copies");
  assert.equal(strayKey(dom("r", { metaKey: true }), "control"), false, "Cmd-R reloads");
  // From a focused button: it keeps the keys that press it; letters still go.
  for (const k of ["Enter", " ", "Escape", "Tab"]) assert.equal(strayKey(dom(k), "control"), false, k);
  for (const k of ["a", "ArrowLeft", "Backspace"]) assert.equal(strayKey(dom(k), "control"), true, k);
  // A field keeps everything.
  for (const k of ["a", "Enter", "Escape"]) assert.equal(strayKey(dom(k), "field"), false, k);
  // Text selected on the page: Ctrl-C and Alt combinations are about it; letters still type.
  assert.equal(strayKey(dom("c", { ctrlKey: true }), "none", true), false);
  assert.equal(strayKey(dom("b", { altKey: true }), "none", true), false);
  assert.equal(strayKey(dom("x"), "none", true), true);
  // A key outside any field is a keyboard of the device's own; a soft keyboard's are in a field.
  assert.equal(hardKey(dom("a"), "none"), true);
  assert.equal(hardKey(dom("a"), "control"), true);
  assert.equal(hardKey(dom("a"), "field"), false);
  assert.equal(hardKey(dom("Unidentified", { keyCode: 229 }), "none"), false);
  assert.equal(hardKey(dom("Shift"), "none"), false);
  assert.equal(ownKeyboard(false, false), true, "a computer");
  assert.equal(ownKeyboard(true, false), false, "a phone: its keyboard would pop up");
  assert.equal(ownKeyboard(true, true), true, "a tablet with a keyboard");
  const free = { canType: true, keyboard: true, focus: "none", dialog: false, selected: false };
  assert.equal(autoFocus(free), true);
  assert.equal(autoFocus({ ...free, focus: "control" }), true, "from a button the page focused");
  assert.equal(autoFocus({ ...free, focus: "field" }), false, "never from a field");
  assert.equal(autoFocus({ ...free, dialog: true }), false, "never under a dialog");
  assert.equal(autoFocus({ ...free, selected: true }), false, "never from a selection");
  assert.equal(autoFocus({ ...free, keyboard: false }), false, "never pops a phone's keyboard");
  assert.equal(autoFocus({ ...free, canType: false }), false, "a viewer, an ended session");
}

/// SHELLS' Alt+1…9, Alt+0: shell N, decided by the physical key, before the terminal sees it.
function altDigits() {
  const term = { focus: "field", terminal: true, dialog: false, on: true };
  const alt = (key, code, o = {}) => dom(key, { altKey: true, code, ...o });
  // Windows and Linux: Alt+1 says "1"; a Mac's Option+1 says "¡", Option+0 "º".
  assert.equal(shellDigit(alt("1", "Digit1"), term), 1);
  assert.equal(shellDigit(alt("¡", "Digit1"), term), 1, "Option+1 on a Mac");
  assert.equal(shellDigit(alt("ª", "Digit9"), term), 9);
  assert.equal(shellDigit(alt("º", "Digit0"), term), 10, "Alt+0 is shell 10");
  // From the page, a button, the terminal: all of them. Not another field, a dialog, or switched off.
  for (const focus of ["none", "control"]) assert.equal(shellDigit(alt("2", "Digit2"), { ...term, focus, terminal: false }), 2, focus);
  assert.equal(shellDigit(alt("¡", "Digit1"), { ...term, terminal: false }), null, "a chat's box types ¡");
  assert.equal(shellDigit(alt("1", "Digit1"), { ...term, dialog: true }), null);
  assert.equal(shellDigit(alt("1", "Digit1"), { ...term, on: false }), null, "[shell] alt_digits off: the program's");
  // The layout's own characters type (German Mac Option+5 `[`, British Option+3 `#`), as keyOf sends them.
  assert.equal(shellDigit(alt("[", "Digit5"), term), null);
  assert.equal(shellDigit(alt("#", "Digit3"), term), null);
  assert.deepEqual(keyOf(alt("[", "Digit5")), { code: { t: "char", c: "[" }, mods: 0 });
  // Other modifiers, other keys, a plain digit: not a shell.
  assert.equal(shellDigit(alt("1", "Digit1", { ctrlKey: true }), term), null, "AltGr on Windows is Ctrl+Alt");
  assert.equal(shellDigit(alt("!", "Digit1", { shiftKey: true }), term), null);
  assert.equal(shellDigit(alt("1", "Digit1", { metaKey: true }), term), null);
  assert.equal(shellDigit(alt("¹", "Digit1", { getModifierState: (k) => k === "AltGraph" }), term), null);
  assert.equal(shellDigit(alt("1", "Numpad1"), term), null);
  assert.equal(shellDigit(alt("å", "KeyA"), term), null);
  assert.equal(shellDigit(dom("1", { code: "Digit1" }), term), null, "a plain digit is typed");
  assert.equal(shellDigit(alt("1", "Digit1", { isComposing: true }), term), null);
}
