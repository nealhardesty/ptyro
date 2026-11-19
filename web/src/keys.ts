// Keyboard input for the web terminal (ROADMAP WEB1.8), with no DOM in it so a test can feed it
// events and compare what a real session received. A browser key event becomes the daemon's
// abstract `KeyEvent` (crates/ptyro-core/src/input.rs); the daemon encodes it against the
// program's own modes (DECCKM, kitty flags…), which only it knows, so nothing here knows about
// escape sequences.

export const MOD = { SHIFT: 1, ALT: 2, CTRL: 4, SUPER: 8 } as const;

export type KeyCode =
  | { t: "char"; c: string }
  | { t: "f"; n: number }
  | { t: "enter" | "tab" | "back_tab" | "backspace" | "esc" | "up" | "down" | "left" | "right" | "home" | "end" | "page_up" | "page_down" | "insert" | "delete" | "menu" };
export interface KeyEvent { code: KeyCode; mods: number }

/// The fields of a DOM `KeyboardEvent` this reads.
export interface DomKey {
  key: string;
  code?: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
  isComposing?: boolean;
  keyCode?: number;
  /// `KeyboardEvent.getModifierState`: AltGraph says the layout made this character itself.
  getModifierState?: (k: string) => boolean;
}

const NAMED: Record<string, KeyCode> = {
  Enter: { t: "enter" }, Tab: { t: "tab" }, Backspace: { t: "backspace" }, Escape: { t: "esc" },
  ArrowUp: { t: "up" }, ArrowDown: { t: "down" }, ArrowLeft: { t: "left" }, ArrowRight: { t: "right" },
  Home: { t: "home" }, End: { t: "end" }, PageUp: { t: "page_up" }, PageDown: { t: "page_down" },
  Insert: { t: "insert" }, Delete: { t: "delete" }, ContextMenu: { t: "menu" },
};
const MODIFIER_KEYS = new Set(["Shift", "Control", "Alt", "Meta", "AltGraph", "CapsLock", "NumLock", "ScrollLock", "Fn", "FnLock", "Hyper", "Super", "OS"]);

/// A keydown that is part of composing text (an IME, a soft keyboard): Safari says so only with
/// keyCode 229, not `isComposing`. Enter then picks the composition; it sends nothing.
export const composing = (e: DomKey): boolean => !!e.isComposing || e.keyCode === 229;

/// What a keydown means for the terminal: a key to send, or `null` (leave it to the browser: a
/// modifier alone, Cmd shortcuts, a soft keyboard's composed text, which `inputKeys` handles).
export function keyOf(e: DomKey, sticky = 0): KeyEvent | null {
  if (composing(e)) return null;
  if (MODIFIER_KEYS.has(e.key)) return null;
  // Cmd (macOS) is the browser's: copy, paste, tabs. A sticky Super from the key row isn't.
  if (e.metaKey) return null;
  // AltGr (Windows reports it as Ctrl+Alt) and the Option key on a Mac with a layout that puts
  // `@ [ ] { } | \` under it make a character: the program gets that character, not Ctrl+Alt+letter.
  const altGr = e.getModifierState?.("AltGraph") === true;
  const typed = e.key.length === 1 && e.key >= " " && e.key <= "~";
  let mods = sticky | (e.ctrlKey && !altGr ? MOD.CTRL : 0) | (e.altKey && !altGr ? MOD.ALT : 0);
  const named = NAMED[e.key];
  if (named) {
    // Shift+Tab is its own key; every other named key carries Shift as a modifier.
    if (e.key === "Tab" && (e.shiftKey || sticky & MOD.SHIFT)) return { code: { t: "back_tab" }, mods: mods & ~MOD.SHIFT };
    return { code: named, mods: mods | (e.shiftKey ? MOD.SHIFT : 0) };
  }
  const f = /^F(\d{1,2})$/.exec(e.key);
  if (f && +f[1] >= 1 && +f[1] <= 24) return { code: { t: "f", n: +f[1] }, mods: mods | (e.shiftKey ? MOD.SHIFT : 0) };
  if (e.key === " " || [...e.key].length === 1) {
    let c = e.key;
    // Option on a Mac types å, ∂, ç…: the terminal wants Alt+a. The physical key says which.
    if (e.altKey && !altGr && e.code) {
      const m = /^Key([A-Z])$/.exec(e.code) ?? /^Digit(\d)$/.exec(e.code);
      if (m && typed && e.key.toLowerCase() !== m[1].toLowerCase()) mods &= ~MOD.ALT; // the layout's own `@`, `|`…: a typed character
      else if (m) c = e.shiftKey ? m[1].toUpperCase() : m[1].toLowerCase();
    }
    // Ctrl+Shift+letter is the letter with both modifiers, not an upper-case control code.
    if (mods & MOD.CTRL) { c = c.toLowerCase(); if (e.shiftKey) mods |= MOD.SHIFT; }
    if (sticky & MOD.SHIFT && c.toLowerCase() !== c.toUpperCase()) c = c.toUpperCase();
    return { code: { t: "char", c }, mods: mods & ~(mods & MOD.CTRL ? 0 : MOD.SHIFT) };
  }
  return null;
}

/// What a soft keyboard's `beforeinput` means: its text as keys, a backspace, an enter. (A
/// keydown that says `Unidentified` or 229 is how those keyboards report everything.)
export function inputKeys(inputType: string, data: string | null, sticky = 0): KeyEvent[] {
  switch (inputType) {
    case "insertText":
    case "insertCompositionText":
    case "insertReplacementText":
      // A sticky Ctrl or Alt is for the first character only: Ctrl-pasting "abc" is Ctrl+a, then b and c.
      return [...(data ?? "")].map((c, i) => c === "\n" ? { code: { t: "enter" } as KeyCode, mods: 0 } : { code: { t: "char", c } as KeyCode, mods: i === 0 ? sticky : 0 });
    case "insertLineBreak":
    case "insertParagraph":
      return [{ code: { t: "enter" }, mods: 0 }];
    case "deleteContentBackward":
      return [{ code: { t: "backspace" }, mods: 0 }];
    case "deleteWordBackward":
      return [{ code: { t: "backspace" }, mods: MOD.ALT }];
    case "deleteContentForward":
      return [{ code: { t: "delete" }, mods: 0 }];
    default:
      return [];
  }
}

export const keyInput = (key: KeyEvent): object => ({ t: "key", key });
export const pasteInput = (text: string): object => ({ t: "paste", text: text.replace(/\r\n?/g, "\n") });
export const mouseInput = (kind: object, x: number, y: number, mods = 0): object => ({ t: "mouse", mouse: { kind, x, y, mods } });
export const MOUSE = {
  down: (button: "left" | "middle" | "right") => ({ t: "down", button }),
  up: (button: "left" | "middle" | "right") => ({ t: "up", button }),
  drag: (button: "left" | "middle" | "right") => ({ t: "drag", button }),
  moved: { t: "moved" },
  scrollUp: { t: "scroll_up" },
  scrollDown: { t: "scroll_down" },
};

/// The sticky modifiers on a key row: tap once and the next key carries it; tap twice and it
/// stays until tapped again.
export class Sticky {
  one = 0;
  lock = 0;
  get mods(): number { return this.one | this.lock; }
  tap(m: number): void {
    if (this.lock & m) { this.lock &= ~m; this.one &= ~m; }
    else if (this.one & m) { this.one &= ~m; this.lock |= m; }
    else this.one |= m;
  }
  /// A key was sent: the one-shot modifiers are spent.
  spent(): void { this.one = 0; }
  has(m: number): boolean { return !!(this.mods & m); }
  locked(m: number): boolean { return !!(this.lock & m); }
}

// ── Typing: soft keyboards, compositions and dictation (ROADMAP WEB2.1) ──────────────────────
//
// A phone's keyboard composes the word being typed (Gboard, SwiftKey, Samsung, iOS predictive):
// the text sits in the hidden field as a composition the keyboard rewrites at will. `Typist`
// keeps a mirror of what the session has been sent for the field's text and, at every change,
// sends the difference: backspaces for what went and the new letters, so letters arrive as they
// are typed and an autocorrect becomes a correction. Dictation is different: it revises whole
// phrases as you speak, so it is held (shown as a preview) and sent once, settled, as one paste,
// and a spoken "new line" can never press Enter. No DOM here: the page hands it the events and
// the field's text, and a test hands it recorded keyboards' events (web/test/term.mjs).

/// Longer single inserts (code points) than this are dictation or a keyboard's clipboard chip.
export const LIVE_MAX = 32;
/// Held text that hasn't changed for this long (ms) is sent (dictation outside a composition).
export const SETTLE_MS = 700;
/// A newline that comes this soon (ms) after an Enter keydown is that Enter.
const TAP_MS = 150;
/// Smart punctuation rewrites what was typed this recently (ms): `--` → `—`, two spaces → `. `.
const SMART_MS = 1500;

/// Why text is held instead of sent: dictation (sent as one paste), a CJK input method or a
/// computer's composition (its final text goes as keys, as a terminal's IME commit does), text
/// put in the field without any input event to say so (accessibility, a dictation app: a paste).
export type Hold = "dictation" | "ime" | "inserted";

/// What a `Typist` needs from the page.
export interface TypingHost {
  /// The hidden field's text now.
  value(): string;
  /// Empty the field (only ever called outside a composition: emptying it mid-composition
  /// breaks keyboards).
  clear(): void;
  /// Send a key; `sticky`: the key row's sticky modifiers apply (typed text, not corrections).
  key(k: KeyEvent, sticky: boolean): void;
  paste(text: string): void;
  /// Held text to show (dim, at the cursor), or `null` to hide it.
  preview(text: string | null, why: Hold | null): void;
  /// The key row's sticky modifiers now.
  sticky(): number;
  /// The program asked for bracketed paste (a pasted newline then doesn't run anything).
  bracketed(): boolean;
  /// An on-screen keyboard: its compositions are predictive typing, streamed; a computer's are
  /// input methods and dead keys, held until they end.
  soft(): boolean;
  now(): number;
  /// Call `tick` after `ms` (replacing any earlier request).
  later(ms: number): void;
}

const cps = (s: string): string[] => [...s];
/// Two or more words: `\S\s+\S` (a trailing space after one word is still one word).
export const multiword = (s: string): boolean => /\S\s+\S/u.test(s);
const NEWLINE = /[\r\n\u2028\u2029]/;
/// Han, kana, Hangul, Bopomofo, CJK punctuation and full-width forms: an input method's text.
export const hasCJK = (s: string): boolean => /[\p{Script=Han}\p{Script=Hiragana}\p{Script=Katakana}\p{Script=Hangul}\p{Script=Bopomofo}\u3000-\u303f\uff00-\uffef]/u.test(s);
/// iOS smart quotes back to what was typed. (Primes, guillemets and CJK brackets are left.)
export const straighten = (s: string): string => s.replace(/[\u201c\u201d\u201e]/g, '"').replace(/[\u2018\u2019\u201a]/g, "'");
/// What a paste of dictated text becomes: without bracketed paste a newline would press Enter,
/// so newlines become spaces and trailing ones go.
export const dictated = (text: string, bracketed: boolean): string =>
  bracketed ? text.replace(/\r\n?|[\u2028\u2029]/g, "\n") : text.replace(/\s*[\r\n\u2028\u2029][\s]*$/u, "").replace(/[ \t]*(?:\r\n?|[\n\u2028\u2029])[ \t\r\n\u2028\u2029]*/g, " ");

const prefix = (a: string[], b: string[]): number => {
  let p = 0;
  while (p < a.length && p < b.length && a[p] === b[p]) p++;
  return p;
};
const BS: KeyEvent = { code: { t: "backspace" }, mods: 0 };
const ch = (c: string): KeyEvent => (c === "\t" ? { code: { t: "tab" }, mods: 0 } : { code: { t: "char", c }, mods: 0 });
/// A character a line editor would take as typed text (no controls; a tab is the Tab key).
const printable = (c: string): boolean => c === "\t" || !/^[\u0000-\u001f\u007f]$/u.test(c);

export class Typist {
  /// The field's text as the session has it (code points), since the field was last emptied.
  private sent: string[] = [];
  /// How many of `sent` can't be taken back: something else went to the session after them (a
  /// key from the row, a paste) or they went changed (a sticky modifier). Backspaces stop here.
  private floor = 0;
  private hold: { why: Hold; discard: boolean } | null = null;
  /// When the held text last changed.
  private changed = 0;
  composing = false;
  /// The field's length when the composition started: its text is what came after.
  private compStart = 0;
  /// A composition ended; its final text is read once the browser has finished with the field.
  private ending = false;
  /// A `beforeinput` announced the next change (an `input` without one is an insertion).
  private announced = false;
  private lastDown = { t: -1e9, enter: false };
  /// What was typed outside the field lately, for undoing smart punctuation (`\b`: a backspace,
  /// `synth`: from the keyboard's own editing, not a Backspace key).
  private recent: { c: string; t: number; synth?: boolean }[] = [];
  /// This keyboard is an input method (it has composed CJK text): its compositions are held.
  cjk = false;

  constructor(private h: TypingHost) {}

  /// Something is waiting: held text, or a composition's end to read.
  get busy(): boolean { return !!this.hold || this.ending; }

  /// A keydown. True when it was sent as a key (the page then cancels it).
  keydown(e: DomKey): boolean {
    const now = this.h.now();
    this.lastDown = { t: now, enter: e.key === "Enter" || e.keyCode === 13 };
    const st = this.h.sticky();
    const k = keyOf(e, st);
    if (!k) return false;
    this.settle();
    const mods = k.mods & ~st;
    if (k.code.t === "char" && !((k.mods | st) & (MOD.CTRL | MOD.ALT | MOD.SUPER))) {
      // The physical key typed it (an iPhone's keyboard sends real keydowns too).
      for (const c of cps(this.smart(k.code.c))) this.h.key({ code: { t: "char", c }, mods }, true);
      this.note(k.code.c);
    } else {
      this.h.key({ code: k.code, mods }, true);
      if (k.code.t === "backspace") this.note("\b"); else this.recent = [];
    }
    return true;
  }

  /// A `beforeinput`. True when it was handled here (the page then cancels it). A change let
  /// through reaches the field, and `input` reads it from there.
  beforeinput(type: string, data: string | null, isComposing: boolean, cancelable = true): boolean {
    this.announced = true;
    if (isComposing || this.composing) return false;
    // Safari's composition commits, pastes (the paste event has them) and anything that can't
    // be cancelled: let the field have them and read the difference.
    if (!cancelable || /Composition|^insertFrom/.test(type)) return false;
    // Safari puts a replacement's text in its dataTransfer: when even that is missing, the field has it.
    if (data === null && (type === "insertText" || type === "insertReplacementText")) return false;
    // The composition that just ended is settled before what comes after it.
    if (this.ending) this.finalize();
    const now = this.h.now();
    const d = data ?? "";
    switch (type) {
      case "insertText":
      case "insertReplacementText":
      case "insertLineBreak":
      case "insertParagraph": {
        const text = type === "insertText" || type === "insertReplacementText" ? d : "\n";
        if (!text) return true;
        if (/^(\r\n?|\n)$/.test(text) && this.lastDown.enter && now - this.lastDown.t < TAP_MS && !this.hold) {
          this.settle();
          this.h.key({ code: { t: "enter" }, mods: 0 }, true);
          this.recent = [];
          return true;
        }
        // Dictation, a keyboard extension's insert, the clipboard chip, a newline no Enter key
        // made: into the field, where the keyboard can still revise it, held, then one paste.
        if (NEWLINE.test(text) || multiword(text) || cps(text).length > LIVE_MAX) {
          if (!this.hold) this.settle();
          return false;
        }
        // More dictation while dictation is held, unless a key was pressed for it.
        if (this.hold && !this.composing && now - this.lastDown.t > TAP_MS) return false;
        this.settle();
        const out = this.smart(text);
        for (const c of cps(out)) if (printable(c)) this.h.key(ch(c), true);
        for (const c of cps(text)) this.note(c);
        return true;
      }
      case "deleteContentBackward":
      case "deleteWordBackward":
      case "deleteContentForward":
        this.settle();
        this.h.key(inputKeys(type, null)[0], true);
        if (type === "deleteContentBackward") this.note("\b", true); else this.recent = [];
        return true;
      default:
        // Formatting, undo and the like mean nothing to a terminal.
        return true;
    }
  }

  compositionstart(): void {
    // (Composing first: the last composition's end is read, but the field isn't emptied under
    // the keyboard that is starting a new one.)
    const was = this.composing;
    this.composing = true;
    if (this.ending) this.finalize();
    if (!was) {
      this.sync(true);
      if (this.hold) this.flush();
      this.compStart = this.sent.length;
    }
  }

  compositionend(): void {
    this.composing = false;
    this.ending = true;
    this.h.later(0);
  }

  /// An `input`: the field changed. Read it and send (or hold) the difference.
  input(isComposing = false): void {
    if (isComposing && !this.composing) { this.composing = true; this.compStart = this.sent.length; }
    const announced = this.announced;
    this.announced = false;
    this.sync(announced);
    if (!this.composing && !this.ending && !this.hold) this.reset();
  }

  /// A paste, a key from the row: whatever is waiting goes first, then this.
  interject(): void {
    this.settle();
    this.recent = [];
  }

  /// Before a key that doesn't go through the field: a composition's end is read, held text is
  /// sent (not an input method's, mid-composition: it is still composing), and what the field
  /// holds can't be taken back after it.
  private settle(): void {
    if (this.ending) this.finalize();
    this.sync(true);
    if (!(this.composing && this.hold?.why === "ime")) this.flush();
    if (this.composing) this.floor = this.sent.length;
    else this.reset();
  }

  /// The field lost focus: send what waits.
  blur(): void {
    if (this.ending) this.finalize();
    this.flush();
    if (!this.composing) this.reset();
  }

  /// Don't send the held text (the preview's ✕).
  discard(): void {
    if (!this.hold) return;
    this.hold.discard = true;
    this.h.preview(null, null);
    if (!this.composing && !this.ending) { this.flush(); this.reset(); }
  }

  /// A timer asked for with `later`.
  tick(): void {
    if (this.ending) this.finalize();
    if (this.hold && !this.composing && !this.ending) {
      const left = this.changed + SETTLE_MS - this.h.now();
      if (left > 0) { this.h.later(left); return; }
      this.flush();
      this.reset();
    }
  }

  /// A composition's final text: its difference goes, held text is sent, the field emptied.
  private finalize(): void {
    this.ending = false;
    this.sync(true, true);
    if (this.hold) {
      // A computer's input method commits: what it composed was CJK, or it wasn't.
      if (this.hold.why === "ime") {
        const t = this.h.value();
        this.cjk = hasCJK(t) || (this.cjk && !/\p{Script=Latin}/u.test(t));
      }
      this.flush();
    }
    if (!this.composing) this.reset();
  }

  /// The difference between what the session has and the field: `from`, where they part (never
  /// below the floor), what to take back and what to add.
  private diff(v: string[]): { p: number; dels: number; ins: string[] } {
    const p = Math.max(prefix(this.sent, v), this.floor);
    return { p, dels: this.sent.length - p, ins: v.slice(p) };
  }

  private sync(announced: boolean, ended = false): void {
    const v = cps(this.h.value());
    if (v.length === this.sent.length && prefix(this.sent, v) === v.length) return;
    const { dels, ins, p } = this.diff(v);
    const inComp = this.composing || this.ending || ended;
    if (!this.hold) {
      const comp = inComp ? v.slice(Math.min(this.compStart, v.length)).join("") : null;
      const why = this.classify(this.sent.slice(p).join(""), ins.join(""), comp, announced || inComp);
      if (why) this.hold = { why, discard: false };
    }
    if (this.hold) {
      this.changed = this.h.now();
      if (!this.hold.discard) this.h.preview(ins.join("") || null, this.hold.why);
      if (!inComp) this.h.later(SETTLE_MS);
      return;
    }
    for (let i = 0; i < dels; i++) this.h.key(BS, false);
    const st = this.h.sticky();
    const smart = !(inComp && this.cjk);
    for (const c of ins) if (printable(c)) this.h.key(ch(smart ? straighten(c) : c), true);
    this.sent = v;
    if (v.length < this.floor) this.floor = v.length;
    // A sticky modifier changed what was sent: those characters can't be corrected.
    if (st && ins.length) this.floor = v.length;
  }

  private classify(removed: string, added: string, comp: string | null, announced: boolean): Hold | null {
    if (comp !== null) {
      if (!this.h.soft() || this.cjk || hasCJK(comp)) return "ime";
      if (multiword(comp) || NEWLINE.test(comp)) return "dictation";
    }
    if (NEWLINE.test(added) || multiword(added) || multiword(removed) || cps(added).length > LIVE_MAX) return "dictation";
    if (!announced) return "inserted";
    return null;
  }

  /// Send the held text: backspaces for what it revised, then one paste (an input method's
  /// short commit goes as keys).
  private flush(): void {
    const hold = this.hold;
    if (!hold) return;
    this.hold = null;
    this.h.preview(null, null);
    const v = cps(this.h.value());
    if (!hold.discard) {
      const { dels, ins } = this.diff(v);
      const text = ins.join("");
      for (let i = 0; i < dels; i++) this.h.key(BS, false);
      if (hold.why === "ime" && !NEWLINE.test(text) && !multiword(text) && ins.length <= LIVE_MAX) {
        for (const c of ins) if (printable(c)) this.h.key(ch(c), true);
        this.sent = v;
        if (this.h.sticky() && ins.length) this.floor = v.length;
        return;
      }
      const out = dictated(text, this.h.bracketed());
      if (out) this.h.paste(out);
    }
    this.sent = v;
    this.floor = v.length;
  }

  /// Empty the field: what is in it has been sent, and the next text starts afresh.
  private reset(): void {
    if (this.h.value() !== "") this.h.clear();
    this.sent = [];
    this.floor = 0;
    this.compStart = 0;
  }

  private note(c: string, synth = false): void {
    this.recent.push({ c, t: this.h.now(), synth });
    if (this.recent.length > 4) this.recent.shift();
  }

  /// What a keyboard's smart punctuation typed, as typed: `—` or `–` right after a `-` is the
  /// second `-` (`--`); `. ` right after a space is the second space; curly quotes are straight.
  /// The keyboard may first take back what it replaces (a backspace of its own, `synth`), or
  /// not (the field it saw was empty): both end as what was typed.
  private smart(d: string): string {
    const now = this.h.now();
    const r = this.recent.filter((x) => now - x.t < SMART_MS);
    const last = r[r.length - 1], prev = r[r.length - 2];
    const undid = (c: string) => last?.c === "\b" && last.synth && prev?.c === c;
    if (d === "\u2014" || d === "\u2013") {
      if (undid("-")) return "--";
      if (last?.c === "-") return "-";
    }
    if (d === ". ") {
      if (undid(" ")) return "  ";
      if (last?.c === " ") return " ";
    }
    return straighten(d);
  }
}
