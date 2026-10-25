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

/// What a keydown means for the terminal: a key to send, or `null` (leave it to the browser: a
/// modifier alone, Cmd shortcuts, a soft keyboard's composed text, which `inputKeys` handles).
export function keyOf(e: DomKey, sticky = 0): KeyEvent | null {
  if (e.isComposing || e.keyCode === 229) return null;
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
