// A real terminal in the web (ROADMAP WEB1.8). The daemon already emulates the program's
// terminal and sends row deltas, so this draws them (only the rows that changed) and adds what a
// viewer lacked: every key goes to the session as it is pressed (the daemon encodes it against
// the program's own modes), paste, IME and soft keyboards, a key row with sticky Ctrl and Alt,
// the mouse when the program asks for it, scrollback, selection and copy, OSC 52, and a size
// that follows this screen when the person asks. No dependencies (SPEC §16.4, §20).

import { css, F } from "./color.js";
import { READABLE_PX, cellAt, clampPx, docOf, findIn, fitCells, fitPx, fitted, lastUsed, maxOffset, pinchZoom, selText, span, splitPan, urlAt, windowOf, wordAt } from "./grid.js";
import type { Doc, Pos, Sel, Zoom } from "./grid.js";
import { ordered } from "./grid.js";
import { MOD, MOUSE, Sticky, inputKeys, keyInput, keyOf, mouseInput, pasteInput } from "./keys.js";
import type { KeyCode, KeyEvent } from "./keys.js";
import { actions, form, h, menu, sheet } from "./kit.js";
import type { Item, Sheet } from "./kit.js";
import type { Cell, Cursor, Screen, Style } from "./screen.js";

const ACTIONS = {
  "term.type": "Type straight into the session: every key goes to it as you press it",
  "term.keys": "Keys a phone's keyboard lacks: Esc, Tab, Ctrl and Alt (tap once for the next key, twice to hold), arrows, F-keys",
  "term.scroll": "Scroll back through the session's history: the wheel, or drag",
  "term.select": "Select text: long-press and drag, or drag with a mouse; double-click a word, triple-click a line",
  "term.copy": "Copy the selection",
  "term.paste": "Paste this device's clipboard into the session",
  "term.mouse": "Send clicks, drags and the wheel to the program, when it asks for the mouse (hold Shift to select instead)",
  "term.resize": "Make the session this screen's size (for everyone attached; the last to type decides)",
  "term.options": "Terminal options: text size, fit, follow the cursor, the keyboard",
  "term.zoom": "Zoom the text: pinch (or Ctrl and the wheel); tap with two fingers to switch between fitting the width and full size",
  "term.find": "Find text in the session's history and screen; ↑ and ↓ go to the previous and next",
  "term.link": "Open the link under the pointer (Ctrl or Cmd and click, or select it and tap Open)",
} as const;
export const tip = actions(ACTIONS);

const FAMILY = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace";
const store = {
  get: (k: string, d: string): string => { try { return localStorage.getItem(`ptyro.term.${k}`) ?? d; } catch { return d; } },
  set: (k: string, v: string): void => { try { localStorage.setItem(`ptyro.term.${k}`, v); } catch { /* private window */ } },
};

/// What a terminal needs from the page around it.
export interface Host {
  id(): string;
  screen(): Screen | undefined;
  /// This device may type into the session (scope `control` or more, and it is running).
  canType(): boolean;
  send(msg: object): void;
  request(req: object): Promise<any>;
  toast(text: string): void;
  /// The page's own lines changed (the pill, the reply box): draw it again.
  changed(): void;
}

interface View { rows: Cell[][]; styles: Map<number, Style>; cursor: Cursor | null; top: number }

export class Term {
  readonly el: HTMLElement;
  readonly wrap: HTMLElement;
  readonly canvas: HTMLCanvasElement;
  readonly input: HTMLTextAreaElement;
  readonly keybar: HTMLElement;
  private live: HTMLButtonElement;
  private bar: HTMLElement;
  private badge: HTMLElement;
  readonly sticky = new Sticky();
  private stickyBtns = new Map<number, HTMLElement>();

  /// Automatic, fitted to the width, or a size of its own: this device's choice (SPEC §16.4).
  zoom: Zoom = zoomOf(store.get("zoom", ""), store.get("fit", ""));
  size = clampPx(+store.get("px", "13") || 13);
  follow = store.get("follow", "1") === "1";
  mouseApp = store.get("mouse", "1") === "1";
  match = false;
  keyboard = store.get("keyboard", "1") === "1";
  typing = false;
  private origSize: { cols: number; rows: number } | null = null;

  /// Scrolled back: the frozen document and how many rows up from the bottom the window is.
  doc: Doc | null = null;
  offset = 0;
  private fetching: Promise<Doc | null> | null = null;
  sel: Sel | null = null;
  /// The last thing searched for: the selection bar offers ↑ and ↓ while it is on.
  private query = "";
  /// New output arrived while the view was frozen or scrolled back.
  private stale = false;

  // drawing state
  private drawn: (Cell[] | undefined)[] = [];
  private drawnSel: string[] = [];
  private geo = { cw: 0, ch: 0, px: 0, cols: 0, rows: 0, dpr: 0, bg: "", fg: "" };
  private raf = 0;
  private full = true;
  /// What painting costs, for the spike (SPEC §20) and anyone measuring on their own phone:
  /// `localStorage["ptyro.debug"] = "1"` then `ptyroTerm.stats` in the console.
  readonly stats = { paints: 0, rows: 0, ms: 0, worst: 0, full: 0 };
  private composing = false;
  private fling = 0;

  constructor(private host: Host) {
    this.canvas = h("canvas", { class: "term", title: tip("term.type") });
    this.wrap = h("div", { class: "term-wrap" }, this.canvas);
    this.input = h("textarea", { class: "term-input", rows: 1, autocapitalize: "off", autocomplete: "off", autocorrect: "off", spellcheck: "false", "aria-label": "Type into the session", enterkeyhint: "enter" });
    this.live = h("button", { class: "term-live", title: tip("term.scroll"), onclick: () => this.goLive() }, "↓ Live");
    this.live.hidden = true;
    this.bar = h("div", { class: "term-sel" });
    this.bar.hidden = true;
    this.badge = h("div", { class: "term-badge" });
    this.badge.hidden = true;
    this.el = h("div", { class: "term-box" }, this.wrap, this.input, this.live, this.bar, this.badge);
    if (store.get("debug", "") || (() => { try { return localStorage.getItem("ptyro.debug"); } catch { return null; } })()) (window as unknown as { ptyroTerm?: Term }).ptyroTerm = this;
    this.keybar = this.buildKeybar();
    this.selectionBar();
    this.wire();
    this.setKeyboard();
  }

  /// Listeners to take off when this terminal is done with (its session was left for another).
  private offs: (() => void)[] = [];
  private clip: { sheet: Sheet | null; text: string; quietUntil: number } = { sheet: null, text: "", quietUntil: 0 };

  /// This terminal is no longer shown: release the session and everything it hooked up.
  dispose(): void {
    this.release();
    for (const off of this.offs.splice(0)) off();
    this.clip.sheet?.close();
    this.el.remove();
  }

  // ── what is shown ────────────────────────────────────────────────────────────────────────

  private view(): View | null {
    const s = this.host.screen();
    if (!s) return null;
    if (this.doc) {
      const d = this.doc;
      const rows = s.size.rows;
      const top = Math.max(0, d.rows.length - rows - this.offset);
      const at = this.offset === 0 ? d.history + d.cursor.y - top : -1;
      return { rows: windowOf(d.rows, top, rows), styles: d.styles, cursor: at >= 0 && at < rows ? { ...d.cursor, y: at } : null, top };
    }
    return { rows: s.rows, styles: s.styles, cursor: s.cursor.visible ? s.cursor : null, top: 0 };
  }

  /// A frame arrived for this session.
  onFrame(): void {
    if (this.doc) { this.stale = true; this.updateChrome(); return; }
    this.schedule();
  }

  schedule(full = false): void {
    if (full) this.full = true;
    if (this.raf) return;
    this.raf = requestAnimationFrame(() => { this.raf = 0; this.paint(); });
  }

  /// Draw what changed: a row is drawn again when its cells are not the ones drawn last, or its
  /// selection changed; everything, when the size, the colours or the font changed.
  paint(): void {
    const s = this.host.screen();
    const v = this.view();
    if (!s || !v || !this.canvas.isConnected) return;
    const ctx = this.canvas.getContext("2d");
    if (!ctx) return;
    const t0 = performance.now();
    const cs = getComputedStyle(document.documentElement);
    const fg = cs.getPropertyValue("--term-fg").trim() || "#e6edf3";
    const bg = cs.getPropertyValue("--term-bg").trim() || "#0d1117";
    const dpr = window.devicePixelRatio || 1;
    // Fit the session's width to the screen; otherwise a size you can read, and a sideways scroll.
    const ratio = charRatio();
    const fp = this.fitPx();
    const px = fitted(this.zoom, fp) ? fp : this.size;
    const cw = px * ratio, ch = Math.round(px * 1.25);
    const g = this.geo;
    const rows = s.size.rows;
    if (this.full || g.cw !== cw || g.ch !== ch || g.cols !== s.size.cols || g.rows !== rows || g.dpr !== dpr || g.bg !== bg || g.fg !== fg) {
      const w = Math.ceil(cw * s.size.cols), hgt = ch * rows;
      this.canvas.style.width = `${w}px`;
      this.canvas.style.height = `${hgt}px`;
      const bw = Math.ceil(w * dpr), bh = Math.ceil(hgt * dpr);
      if (this.canvas.width !== bw) this.canvas.width = bw;
      if (this.canvas.height !== bh) this.canvas.height = bh;
      Object.assign(g, { cw, ch, px, cols: s.size.cols, rows, dpr, bg, fg });
      this.drawn = [];
      this.drawnSel = [];
      this.full = false;
      this.hint(fp);
    }
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.textBaseline = "top";
    const cur = v.cursor && v.cursor.y < rows ? v.cursor.y : -1;
    const curKey = cur >= 0 ? `${cur}:${v.cursor!.x}:${this.typing}` : "";
    let drawnRows = 0;
    for (let y = 0; y < rows; y++) {
      const row = v.rows[y] ?? [];
      const sp = this.sel ? span(this.sel, v.top + y, s.size.cols) : null;
      const sk = (sp ? sp.join(",") : "") + (y === cur ? `|${curKey}` : "");
      if (this.drawn[y] === row && this.drawnSel[y] === sk) continue;
      this.drawn[y] = row;
      this.drawnSel[y] = sk;
      drawnRows++;
      this.drawRow(ctx, v, y, row, sp, y === cur ? v.cursor : null);
    }
    this.followCursor(v, cw, ch);
    this.updateChrome();
    const took = performance.now() - t0;
    this.stats.paints++;
    this.stats.ms += took;
    this.stats.worst = Math.max(this.stats.worst, took);
    if (drawnRows === rows) this.stats.full++;
    this.stats.rows += drawnRows;
  }

  private drawRow(ctx: CanvasRenderingContext2D, v: View, y: number, row: Cell[], sp: [number, number] | null, cursor: Cursor | null): void {
    const { cw, ch, px, cols, fg: FG, bg: BG } = this.geo;
    ctx.globalAlpha = 1;
    ctx.fillStyle = BG;
    ctx.fillRect(0, y * ch, Math.ceil(cw * cols) + 1, ch);
    for (let x = 0; x < row.length; x++) {
      const c = row[x];
      if (c.spacer) continue;
      const st = v.styles.get(c.s) ?? {};
      const flags = st.flags ?? 0;
      let fg = css(st.fg, FG), bg = css(st.bg, BG);
      if (flags & F.INVERSE) [fg, bg] = [bg, fg];
      const width = c.wide ? 2 : 1;
      if (bg !== BG) { ctx.fillStyle = bg; ctx.fillRect(x * cw, y * ch, cw * width + 0.5, ch); }
      if (!c.t || c.t === " " || flags & F.HIDDEN) continue;
      ctx.font = `${flags & F.ITALIC ? "italic " : ""}${flags & F.BOLD ? "bold " : ""}${px}px ${FAMILY}`;
      ctx.globalAlpha = flags & F.DIM ? 0.6 : 1;
      ctx.fillStyle = fg;
      ctx.fillText(c.t, x * cw, y * ch + (ch - px) / 2);
      if (flags & F.UNDERLINE) ctx.fillRect(x * cw, y * ch + ch - 2, cw * width, 1);
      if (flags & F.STRIKE) ctx.fillRect(x * cw, y * ch + ch / 2, cw * width, 1);
      ctx.globalAlpha = 1;
    }
    if (sp) {
      ctx.globalAlpha = 0.38;
      ctx.fillStyle = css({ t: "indexed", i: 12 }, "#58a6ff");
      ctx.fillRect(sp[0] * cw, y * ch, Math.max(0, Math.min(sp[1], cols) - sp[0]) * cw, ch);
      ctx.globalAlpha = 1;
    }
    if (cursor) {
      ctx.fillStyle = FG;
      const under = cursor.shape === "underline", bar = cursor.shape === "bar";
      const w = bar ? 2 : cw, hh = under ? 2 : ch, yy = y * ch + (under ? ch - 2 : 0);
      // A cursor that is yours is solid; one you aren't typing at is an outline.
      if (this.typing || this.doc) { ctx.globalAlpha = 0.6; ctx.fillRect(cursor.x * cw, yy, w, hh); }
      else { ctx.globalAlpha = 0.9; ctx.strokeStyle = FG; ctx.lineWidth = 1; ctx.strokeRect(cursor.x * cw + 0.5, yy + 0.5, w - 1, hh - 1); }
      ctx.globalAlpha = 1;
    }
  }

  /// In 1:1 the screen can be wider or taller than the box: keep the cursor in sight.
  private followCursor(v: View, cw: number, ch: number): void {
    if (!this.follow || this.fit || !v.cursor || this.sel || this.doc || this.looking || this.pinch) return;
    const x = v.cursor.x * cw, y = v.cursor.y * ch;
    const w = this.wrap;
    if (x < w.scrollLeft + 10) w.scrollLeft = Math.max(0, x - 40);
    else if (x + cw > w.scrollLeft + w.clientWidth - 10) w.scrollLeft = x + cw - w.clientWidth + 40;
    if (y < w.scrollTop) w.scrollTop = y;
    else if (y + ch > w.scrollTop + w.clientHeight - 8) w.scrollTop = y + ch - w.clientHeight + 8;
  }

  private updateChrome(): void {
    this.live.hidden = !this.doc;
    this.live.textContent = this.stale ? "↓ Live · new output" : "↓ Live";
    const parts: string[] = [];
    if (this.zoomNote) parts.push(this.zoomNote);
    if (this.doc && this.offset > 0) parts.push(`${this.offset} rows back`);
    if (this.match) parts.push("resizing the session");
    this.badge.hidden = !parts.length;
    this.badge.textContent = parts.join(" · ");
    const sel = this.sel && !this.dragging;
    if (sel && this.bar.hidden) this.selectionBar();
    this.bar.hidden = !sel;
    this.el.classList.toggle("typing", this.typing);
    this.el.classList.toggle("fit", this.fit);
  }

  // ── scrollback ───────────────────────────────────────────────────────────────────────────

  /// The frozen document (history plus screen), fetched on the first scroll or selection.
  async freeze(): Promise<Doc | null> {
    if (this.doc) return this.doc;
    this.fetching ??= this.host.request({ t: "read_scrollback", addr: this.host.id(), lines: 5000 }).then(
      (r) => { this.doc = docOf(r.doc); this.stale = false; return this.doc; },
      (e) => { this.host.toast((e as Error).message); return null; },
    ).finally(() => { this.fetching = null; });
    const d = await this.fetching;
    this.schedule(true);
    return d;
  }

  async scrollBy(rows: number): Promise<void> {
    if (!rows) return;
    const s = this.host.screen();
    if (!s) return;
    if (s.modes.alt_screen && !this.doc) {
      // A full-screen program has no scrollback of its own to show: the wheel is its.
      if (s.modes.mouse && this.mouseApp && this.host.canType()) {
        const at = { col: Math.floor(s.size.cols / 2), row: Math.floor(s.size.rows / 2) };
        for (let i = 0; i < Math.abs(rows); i++) this.host.send({ t: "input", session: this.host.id(), event: mouseInput(rows < 0 ? MOUSE.scrollUp : MOUSE.scrollDown, at.col, at.row) });
      } else if (this.host.canType()) {
        for (let i = 0; i < Math.abs(rows); i++) this.key({ code: { t: rows < 0 ? "up" : "down" }, mods: 0 }, false);
      }
      return;
    }
    // Up is negative: the window moves back through history. There is nothing below the live screen.
    if (rows > 0 && !this.doc) return;
    const d = await this.freeze();
    if (!d) return;
    this.offset = Math.min(Math.max(this.offset - rows, 0), maxOffset(d.rows.length, s.size.rows));
    if (this.offset === 0 && !this.sel) { this.goLive(); return; }
    this.schedule();
    this.updateChrome();
  }

  goLive(): void {
    this.doc = null;
    this.offset = 0;
    this.stale = false;
    this.sel = null;
    this.schedule(true);
    this.updateChrome();
  }

  // ── selection and copy ───────────────────────────────────────────────────────────────────

  private dragging = false;

  private pos(e: { clientX: number; clientY: number }): Pos & { cell: { col: number; row: number } } {
    const r = this.canvas.getBoundingClientRect();
    const s = this.host.screen()!;
    const cell = cellAt(e.clientX - r.left, e.clientY - r.top, this.geo.cw, this.geo.ch, s.size.cols, s.size.rows);
    return { row: (this.view()?.top ?? 0) + cell.row, col: cell.col, cell };
  }

  private async startSelect(e: { clientX: number; clientY: number }, mode: "char" | "word" | "line" = "char"): Promise<void> {
    const d = await this.freeze();
    if (!d) return;
    const p = this.pos(e);
    const row = d.rows[p.row] ?? [];
    if (mode === "word") { const [a, b] = wordAt(row, p.col); this.sel = { a: { row: p.row, col: a }, b: { row: p.row, col: b } }; }
    else if (mode === "line") this.sel = { a: { row: p.row, col: 0 }, b: { row: p.row, col: Math.max(lastUsed(row), 0) } };
    else this.sel = { a: { row: p.row, col: p.col }, b: { row: p.row, col: p.col } };
    this.dragging = mode === "char";
    this.schedule();
    this.updateChrome();
  }

  private extendSelect(e: { clientX: number; clientY: number }): void {
    if (!this.sel) return;
    const p = this.pos(e);
    this.sel = { ...this.sel, b: { row: p.row, col: p.col } };
    this.schedule();
  }

  selectedText(): string {
    const d = this.doc;
    if (!d || !this.sel) return "";
    return selText(d.rows, d.wraps, this.sel, d.size.cols);
  }

  async copy(): Promise<void> {
    const t = this.selectedText();
    if (!t) { this.host.toast("Nothing selected"); return; }
    await this.toClipboard(t, `Copied ${t.length} character${t.length === 1 ? "" : "s"}`);
    this.clearSelect();
  }

  /// A program wrote to the clipboard (OSC 52). A page may only write it from a tap, so when
  /// the browser says no, the text is offered with a button that is one.
  fromProgram(text: string): void {
    // Anything printed to the terminal can ask (a `cat` of a hostile file), and the clipboard is
    // what you paste into a shell next: so it asks you, once, and not again for a while if you
    // say no. Requests that arrive while the question is open only change what it would copy.
    const c = this.clip;
    c.text = text;
    if (c.sheet || Date.now() < c.quietUntil) return;
    const shown = text.length > 400 ? `${text.slice(0, 400)}…` : text;
    const s = sheet("A program wants to copy this", [h("p", { class: "intro" }, `${text.length} characters, from the program in this session. It goes to your clipboard only if you copy it.`), h("pre", { class: "block" }, shown)], [
      h("button", { onclick: () => { c.quietUntil = Date.now() + 30_000; s.close(); } }, "Ignore"),
      h("button", { class: "primary", onclick: () => { void this.toClipboard(c.text, "Copied").then(() => s.close()); } }, "Copy"),
    ]);
    c.sheet = s;
    s.onClose(() => { c.sheet = null; });
  }

  /// Put text on the clipboard: the old way first, which works inside the tap that asked, then
  /// the clipboard API (which can wait for a permission nobody sees, so it gets a second or two).
  private async toClipboard(text: string, said: string): Promise<boolean> {
    let ok = false;
    try {
      const ta = h("textarea", { class: "term-copy" });
      ta.value = text;
      document.body.append(ta);
      ta.select();
      ok = document.execCommand("copy");
      ta.remove();
    } catch { /* not allowed here */ }
    if (!ok) {
      try {
        await Promise.race([navigator.clipboard.writeText(text), new Promise((_, no) => setTimeout(() => no(new Error("no answer")), 1500))]);
        ok = true;
      } catch { /* denied */ }
    }
    this.host.toast(ok ? said : "Couldn't copy: select the text and copy it yourself");
    if (this.typing) this.input.focus({ preventScroll: true });
    return ok;
  }

  clearSelect(): void {
    this.sel = null;
    this.dragging = false;
    // Frozen only for the selection: back to live unless scrolled on purpose.
    if (this.offset === 0) this.goLive(); else { this.schedule(true); this.updateChrome(); }
  }

  async paste(): Promise<void> {
    if (!this.host.canType()) return;
    try {
      const t = await navigator.clipboard.readText();
      if (t) this.sendInput(pasteInput(t));
    } catch { this.host.toast("This browser won't hand over the clipboard: paste with Ctrl+V or Cmd+V while the terminal has focus"); }
  }

  // ── typing ───────────────────────────────────────────────────────────────────────────────

  private sendInput(event: object): void {
    this.host.send({ t: "input", session: this.host.id(), event });
  }

  /// One key to the session, with the sticky modifiers on the key row spent on it.
  key(k: KeyEvent, fromRow = true): void {
    if (!this.host.canType()) return;
    if (this.doc) this.goLive();
    this.looking = false;
    const mods = k.mods | this.sticky.mods;
    this.sendInput(keyInput({ code: k.code, mods }));
    if (this.sticky.one) { this.sticky.spent(); this.paintSticky(); }
    if (fromRow) navigator.vibrate?.(4);
  }

  focus(): void {
    if (!this.host.canType()) return;
    this.input.focus({ preventScroll: true });
  }

  private setKeyboard(): void {
    // `none` keeps the keys (a hardware keyboard) without the on-screen one in the way.
    this.input.setAttribute("inputmode", this.keyboard ? "text" : "none");
  }

  private wire(): void {
    const inp = this.input;
    inp.addEventListener("focus", () => {
      this.typing = true;
      const s = this.host.screen();
      if (s?.modes.focus_events) this.sendInput({ t: "focus", gained: true });
      document.body.classList.add("typing");
      this.fitViewport();
      this.schedule(true);
      this.host.changed();
    });
    inp.addEventListener("blur", () => {
      this.typing = false;
      const s = this.host.screen();
      if (s?.modes.focus_events) this.sendInput({ t: "focus", gained: false });
      document.body.classList.remove("typing");
      document.documentElement.style.removeProperty("--vvh");
      this.schedule(true);
      this.host.changed();
    });
    inp.addEventListener("keydown", (e) => this.onKeydown(e));
    inp.addEventListener("beforeinput", (e) => {
      // Soft keyboards and IMEs report text here; physical keys were handled in keydown.
      if (this.composing || e.isComposing) return;
      const keys = inputKeys(e.inputType, e.data ?? inp.value, this.sticky.mods);
      if (e.inputType === "insertFromPaste" || e.inputType === "insertFromDrop") return;
      e.preventDefault();
      for (const k of keys) this.key(k, false);
      inp.value = "";
    });
    inp.addEventListener("compositionstart", () => { this.composing = true; });
    inp.addEventListener("compositionend", (e) => {
      this.composing = false;
      for (const k of inputKeys("insertText", e.data, this.sticky.mods)) this.key(k, false);
      inp.value = "";
    });
    inp.addEventListener("paste", (e) => {
      e.preventDefault();
      const t = e.clipboardData?.getData("text/plain") ?? "";
      if (t && this.host.canType()) this.sendInput(pasteInput(t));
    });
    inp.addEventListener("copy", (e) => {
      const t = this.selectedText();
      if (!t) return;
      e.preventDefault();
      e.clipboardData?.setData("text/plain", t);
      this.host.toast(`Copied ${t.length} characters`);
    });
    // Listeners on the window and an observer outlive this terminal unless `dispose` takes them off.
    const onVv = () => { if (this.typing) { this.fitViewport(); this.followCursorNow(); } this.resizeSession(); };
    const onWin = () => { this.schedule(true); this.resizeSession(); };
    window.visualViewport?.addEventListener("resize", onVv);
    window.addEventListener("resize", onWin);
    const ro = new ResizeObserver(onWin);
    ro.observe(this.wrap);
    this.offs.push(() => { window.visualViewport?.removeEventListener("resize", onVv); window.removeEventListener("resize", onWin); ro.disconnect(); });

    // pointer: select, scroll, pan, pinch, click. On the box, not only the canvas, so that a finger
    // on the space around a small screen pans and zooms it too (and never zooms the whole page).
    const c = this.canvas, w = this.wrap;
    w.addEventListener("pointerdown", (e) => this.onDown(e));
    w.addEventListener("pointermove", (e) => this.onMove(e));
    w.addEventListener("pointerup", (e) => this.onUp(e));
    w.addEventListener("pointercancel", (e) => this.onUp(e, true));
    // Safari: a trackpad pinch on a Mac comes as gesture events (and on an iPhone they would zoom
    // the page; the pointers above are the pinch there).
    type Gesture = Event & { scale: number; clientX: number; clientY: number };
    let g0 = 0, gAt = { x: 0, y: 0 };
    w.addEventListener("gesturestart", (e) => { e.preventDefault(); if (this.fingers.size) return; g0 = this.geo.px || this.size; this.looking = true; });
    w.addEventListener("gesturechange", (e) => {
      e.preventDefault();
      if (this.fingers.size || !g0) return;
      const g = e as Gesture;
      gAt = { x: g.clientX, y: g.clientY };
      const z = pinchZoom(g0, 1, g.scale, this.fitPx());
      this.zoomTo(z.zoom, z.px, gAt);
    });
    w.addEventListener("gestureend", (e) => { e.preventDefault(); if (this.fingers.size || !g0) return; g0 = 0; this.saveZoom(); });
    c.addEventListener("contextmenu", (e) => {
      if (this.appMouse(e)) { e.preventDefault(); return; }
      e.preventDefault();
      menu("The terminal", [
        this.sel && { label: "Copy", hint: "The selection", title: tip("term.copy"), run: () => this.copy() },
        this.host.canType() && { label: "Paste", title: tip("term.paste"), run: () => this.paste() },
        { label: "Select all", title: tip("term.select"), run: () => this.selectAll() },
        { label: "Find in the history…", title: tip("term.find"), run: () => { form("Find", [{ key: "q", label: "Text", value: this.query, required: true }], "Find", async (v) => { await this.find(String(v.q), -1); }); } },
        { label: "More…", title: tip("term.options"), run: () => this.options() },
      ]);
    });
    c.addEventListener("pointermove", (e) => { if (e.pointerType === "mouse" && !e.buttons) c.style.cursor = (e.ctrlKey || e.metaKey) && this.link(e) ? "pointer" : ""; });
    c.addEventListener("dblclick", (e) => { if (!this.touch && !this.appMouse(e)) void this.startSelect(e, "word"); });
    c.addEventListener("click", (e) => { if (e.detail === 3 && !this.appMouse(e)) void this.startSelect(e, "line"); });
    c.addEventListener("wheel", (e) => {
      const s = this.host.screen();
      if (!s) return;
      e.preventDefault();
      // Ctrl and the wheel is zoom, as it is everywhere else; so is a trackpad's pinch in Chrome,
      // which comes as Ctrl and a wheel whose delta is -100·ln(scale). A mouse's notch is a step.
      if (e.ctrlKey) {
        const dy = e.deltaMode === 0 ? e.deltaY : Math.sign(e.deltaY) * 10;
        const z = pinchZoom(this.geo.px || this.size, 1, Math.exp(-Math.max(-10, Math.min(10, dy)) / 100), this.fitPx());
        this.looking = true;
        this.zoomTo(z.zoom, z.px, { x: e.clientX, y: e.clientY });
        clearTimeout(this.wheelSave);
        this.wheelSave = window.setTimeout(() => this.saveZoom(), 300);
        return;
      }
      const rows = Math.round((e.deltaMode === 1 ? e.deltaY : e.deltaY / this.geo.ch) || Math.sign(e.deltaY));
      if (this.appMouse(e) || (s.modes.mouse && this.mouseApp && this.host.canType() && !e.shiftKey)) {
        const p = this.pos(e).cell;
        for (let i = 0; i < Math.abs(rows); i++) this.sendInput(mouseInput(rows < 0 ? MOUSE.scrollUp : MOUSE.scrollDown, p.col, p.row));
      } else void this.scrollBy(rows);
    }, { passive: false });
  }

  private followCursorNow(): void {
    const v = this.view();
    if (v) this.followCursor(v, this.geo.cw, this.geo.ch);
  }

  /// The soft keyboard covers the bottom of the layout on a phone: fit the page above it.
  private fitViewport(): void {
    const vv = window.visualViewport;
    if (vv) document.documentElement.style.setProperty("--vvh", `${Math.round(vv.height)}px`);
  }

  private onKeydown(e: KeyboardEvent): void {
    if (!this.host.canType()) return;
    // Copy and paste, the way each platform does them.
    if (this.sel && ((e.metaKey && e.key === "c") || (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "c"))) { e.preventDefault(); void this.copy(); return; }
    if ((e.metaKey && e.key === "v") || (e.ctrlKey && e.shiftKey && e.key.toLowerCase() === "v")) return; // the paste event
    if (e.key === "Escape" && this.sel) { e.preventDefault(); this.clearSelect(); return; }
    if (e.key === "PageUp" && e.shiftKey) { e.preventDefault(); void this.scrollBy(-Math.max(1, (this.host.screen()?.size.rows ?? 24) - 2)); return; }
    if (e.key === "PageDown" && e.shiftKey) { e.preventDefault(); void this.scrollBy(Math.max(1, (this.host.screen()?.size.rows ?? 24) - 2)); return; }
    const k = keyOf(e, this.sticky.mods);
    if (!k) return;
    e.preventDefault();
    this.key({ code: k.code, mods: k.mods & ~this.sticky.mods }, false);
  }

  // ── the pointer ──────────────────────────────────────────────────────────────────────────

  private touch: {
    id: number; x: number; y: number; t: number; mode: "pending" | "scroll" | "pan" | "pinch" | "select" | "app";
    timer: number; carry: number; vx: number; vy: number; lastX: number; lastY: number; lastT: number; type: string; cell: { col: number; row: number }; btn: "left" | "middle" | "right";
  } | null = null;

  /// The program has the mouse: not while selecting (Shift), not when this device can't type.
  private appMouse(e: { shiftKey?: boolean }): boolean {
    const s = this.host.screen();
    return !!s && s.modes.mouse && this.mouseApp && this.host.canType() && !e.shiftKey && !this.doc;
  }

  private onDown(e: PointerEvent): void {
    // A mouse on the box around the canvas is on its scrollbars.
    if (e.pointerType === "mouse" && e.target !== this.canvas) return;
    if (e.pointerType === "touch") {
      // The first finger of a new touch: whatever an earlier one left behind is over.
      if (e.isPrimary) { this.fingers.clear(); if (this.touch?.type === "touch") this.touch = null; }
      this.fingers.set(e.pointerId, { x: e.clientX, y: e.clientY });
      if (this.fingers.size === 2 && this.touch?.type === "touch" && !this.pinch) {
        try { this.wrap.setPointerCapture(e.pointerId); } catch { /* gone */ }
        this.startPinch();
        return;
      }
      if (this.fingers.size > 1) return;
    }
    if (this.touch) return;
    if (e.pointerType === "mouse" && e.button === 0 && (e.ctrlKey || e.metaKey)) {
      const l = this.link(e);
      if (l) { e.preventDefault(); window.open(l.url, "_blank", "noopener,noreferrer"); return; }
    }
    this.fling && cancelAnimationFrame(this.fling);
    const s = this.host.screen();
    if (!s) return;
    const p = this.pos(e);
    const btn = e.button === 2 ? "right" : e.button === 1 ? "middle" : "left";
    this.touch = { id: e.pointerId, x: e.clientX, y: e.clientY, t: Date.now(), mode: "pending", timer: 0, carry: 0, vx: 0, vy: 0, lastX: e.clientX, lastY: e.clientY, lastT: Date.now(), type: e.pointerType, cell: p.cell, btn };
    this.wrap.setPointerCapture(e.pointerId);
    const t = this.touch;
    if (e.pointerType === "mouse") {
      if (this.appMouse(e)) {
        t.mode = "app";
        this.sendInput(mouseInput(MOUSE.down(btn), p.cell.col, p.cell.row, modsOf(e)));
        this.focus();
        e.preventDefault();
        return;
      }
      if (e.button === 0) {
        // Press and drag selects; a click without a drag only focuses.
        const hadSel = !!this.sel;
        if (hadSel) this.clearSelect();
        t.mode = "pending";
        t.timer = window.setTimeout(() => {}, 0);
        this.focus();
        e.preventDefault();
      }
      return;
    }
    // A finger: hold to select, drag to scroll or pan, two to zoom, tap to type (or click, for a
    // program that asks).
    t.timer = window.setTimeout(() => {
      if (this.touch !== t || t.mode !== "pending") return;
      t.mode = "select";
      navigator.vibrate?.(12);
      // The selection arrives with the scrollback it is in. A finger lifted before then has
      // already let go: the selection stays, with its Copy bar, and isn't a drag in progress.
      void this.startSelect({ clientX: t.x, clientY: t.y }, "word").then(() => {
        if (this.touch === t) { this.dragging = true; return; }
        this.dragging = false;
        this.schedule();
        this.updateChrome();
        this.placeBar();
      });
    }, 420);
  }

  private onMove(e: PointerEvent): void {
    if (this.fingers.has(e.pointerId)) this.fingers.set(e.pointerId, { x: e.clientX, y: e.clientY });
    if (this.pinch) { this.movePinch(); return; }
    const t = this.touch;
    if (!t || t.id !== e.pointerId || t.mode === "pinch") return;
    const dx = e.clientX - t.x, dy = e.clientY - t.y;
    if (t.mode === "app") {
      const p = this.pos(e).cell;
      if (p.col !== t.cell.col || p.row !== t.cell.row) { t.cell = p; this.sendInput(mouseInput(MOUSE.drag(t.btn), p.col, p.row, modsOf(e))); }
      return;
    }
    if (t.mode === "select") {
      if (this.dragging) { this.extendSelect(e); this.edgeScroll(e); }
      return;
    }
    if (t.type === "mouse") {
      if (t.mode === "pending" && Math.hypot(dx, dy) > 4) { t.mode = "select"; void this.startSelect({ clientX: t.x, clientY: t.y }).then(() => { this.dragging = true; this.extendSelect(e); }); }
      return;
    }
    if (t.mode === "pending") {
      // The first way the finger moves decides: up and down scrolls, sideways pans.
      if (Math.abs(dy) > 8 && Math.abs(dy) > Math.abs(dx)) { clearTimeout(t.timer); t.mode = "scroll"; t.lastY = e.clientY; t.lastT = Date.now(); }
      else if (Math.abs(dx) > 10) { clearTimeout(t.timer); t.mode = "pan"; t.lastX = e.clientX; t.lastT = Date.now(); }
      return;
    }
    const now = Date.now();
    if (t.mode === "scroll") {
      const step = e.clientY - t.lastY;
      t.vy = step / Math.max(1, now - t.lastT);
      t.lastY = e.clientY;
      t.lastT = now;
      this.panY(step, t);
    } else if (t.mode === "pan") {
      const step = e.clientX - t.lastX;
      t.vx = step / Math.max(1, now - t.lastT);
      t.lastX = e.clientX;
      t.lastT = now;
      this.panX(step);
    }
  }

  /// A finger moved `dx` pixels sideways: the box scrolls under it.
  private panX(dx: number): void {
    if (!dx) return;
    this.wrap.scrollLeft -= dx;
    this.looking = true;
  }

  /// A finger moved `dy` pixels up or down: the box scrolls first when the screen is taller than
  /// it, then the history (or a full-screen program's own scrolling).
  private panY(dy: number, t: { carry: number }): void {
    const w = this.wrap;
    const { top, rest } = splitPan(dy, w.scrollTop, w.scrollHeight - w.clientHeight, !!this.doc && this.offset > 0);
    if (top !== w.scrollTop) { w.scrollTop = top; this.looking = true; }
    t.carry += rest / (this.geo.ch || 16);
    const rows = Math.trunc(t.carry);
    if (rows) { t.carry -= rows; void this.scrollBy(-rows); }
  }

  private onUp(e: PointerEvent, cancelled = false): void {
    this.fingers.delete(e.pointerId);
    if (this.pinch && this.fingers.size < 2) this.endPinch(cancelled);
    const t = this.touch;
    if (t?.mode === "pinch") {
      // The pinch is over; the fingers still down do nothing until they lift.
      try { this.wrap.releasePointerCapture(e.pointerId); } catch { /* gone */ }
      if (!this.fingers.size) this.touch = null;
      return;
    }
    if (!t || t.id !== e.pointerId) return;
    clearTimeout(t.timer);
    this.touch = null;
    try { this.wrap.releasePointerCapture(e.pointerId); } catch { /* gone */ }
    if (t.mode === "app") { const p = this.pos(e).cell; this.sendInput(mouseInput(MOUSE.up(t.btn), p.col, p.row, modsOf(e))); return; }
    if (t.mode === "select") { this.dragging = false; if (this.sel) { this.schedule(); this.updateChrome(); this.placeBar(); } return; }
    if (cancelled) return;
    if (t.mode === "scroll" || t.mode === "pan") {
      // A flick keeps scrolling (or panning) for a moment: pixels a frame, slowing down.
      let vx = t.mode === "pan" ? t.vx * 16 : 0, vy = t.mode === "scroll" ? t.vy * 16 : 0;
      if (Math.abs(vx) + Math.abs(vy) > 2) {
        const step = () => {
          if (vy) this.panY(vy, t);
          if (vx) this.panX(vx);
          vx *= 0.94;
          vy *= 0.94;
          this.fling = Math.abs(vx) + Math.abs(vy) > 0.5 ? requestAnimationFrame(step) : 0;
        };
        this.fling = requestAnimationFrame(step);
      }
      return;
    }
    if (t.mode === "pending" && Date.now() - t.t < 400) {
      // A tap. A program that asked for the mouse gets a click; otherwise the keyboard comes up.
      if (this.sel) { this.clearSelect(); return; }
      this.looking = false;
      if (t.type !== "mouse" && this.appMouse(e)) {
        this.sendInput(mouseInput(MOUSE.down("left"), t.cell.col, t.cell.row));
        this.sendInput(mouseInput(MOUSE.up("left"), t.cell.col, t.cell.row));
      }
      this.focus();
    }
  }

  /// Dragging a selection past the top or bottom edge scrolls the history.
  private edgeScroll(e: PointerEvent): void {
    const r = this.wrap.getBoundingClientRect();
    if (e.clientY < r.top + 14) void this.scrollBy(-1).then(() => this.extendSelect(e));
    else if (e.clientY > r.bottom - 14) void this.scrollBy(1).then(() => this.extendSelect(e));
  }

  private placeBar(): void {
    // The selection's own bar sits at the top of the terminal, out of the way of the fingers.
    this.bar.hidden = !this.sel;
  }

  // ── zoom ─────────────────────────────────────────────────────────────────────────────────

  /// The person moved the view themselves (a pan, a pinch): the cursor isn't followed again
  /// until they type or tap the terminal.
  private looking = false;
  /// Touch points down on the box, for the pinch.
  private fingers = new Map<number, { x: number; y: number }>();
  /// A pinch in progress: where it started, the size then, and the cell (fractional) under the
  /// fingers, which stays under them.
  private pinch: { d0: number; px0: number; col: number; row: number; t0: number; at: { x: number; y: number }; moved: boolean } | null = null;
  private zoomNote = "";
  private zoomNoteTimer = 0;
  private wheelSave = 0;

  /// Drawn fitted to the width (by choice, or automatically because that is readable).
  get fit(): boolean { return fitted(this.zoom, this.fitPx()); }

  /// The text size that fits the session's width in this box.
  private fitPx(): number {
    return fitPx(this.wrap.clientWidth - 8, this.host.screen()?.size.cols ?? 80, charRatio());
  }

  /// Show the text at `zoom` (`px` for a size of its own), keeping what is under `at` (a point on
  /// the page; the middle of the box if none) under it. Not kept for next time: `saveZoom`.
  private zoomTo(zoom: Zoom, px: number, at: { x: number; y: number } | null, anchor?: { col: number; row: number }): void {
    const g = this.geo;
    const box = this.wrap.getBoundingClientRect();
    const pt = at ?? { x: box.left + box.width / 2, y: box.top + box.height / 2 };
    const r = this.canvas.getBoundingClientRect();
    const a = anchor ?? { col: (pt.x - r.left) / (g.cw || 1), row: (pt.y - r.top) / (g.ch || 1) };
    this.zoom = zoom;
    if (zoom === "size") this.size = clampPx(px);
    // Drawn now, so the scroll below is measured against the new size.
    this.full = true;
    this.paint();
    const r2 = this.canvas.getBoundingClientRect();
    this.wrap.scrollLeft += r2.left + a.col * this.geo.cw - pt.x;
    this.wrap.scrollTop += r2.top + a.row * this.geo.ch - pt.y;
    this.noteZoom();
  }

  /// Keep this device's zoom for next time, and resize the session when it follows this screen.
  private saveZoom(): void {
    store.set("zoom", this.zoom);
    store.set("px", String(+this.size.toFixed(2)));
    this.resizeSession();
    this.host.changed();
  }

  setZoom(zoom: Zoom, px = this.size, at: { x: number; y: number } | null = null): void {
    this.zoomTo(zoom, px, at);
    this.saveZoom();
  }

  /// A pixel bigger or smaller than what is shown; smaller than fitting the width is fitted.
  private stepZoom(by: 1 | -1): void {
    const z = pinchZoom(Math.round(this.geo.px || this.size) + by, 1, 1, this.fitPx());
    this.setZoom(z.zoom, z.px);
  }

  /// The size, in the corner for a moment after it changes.
  private noteZoom(): void {
    const px = Math.round(this.geo.px);
    this.zoomNote = `${this.zoom === "auto" ? "Auto · " : ""}${this.fit ? `Fit · ${px}px` : `${px}px`}`;
    clearTimeout(this.zoomNoteTimer);
    this.zoomNoteTimer = window.setTimeout(() => { this.zoomNote = ""; this.updateChrome(); }, 1200);
    this.updateChrome();
  }

  private startPinch(): void {
    const [a, b] = [...this.fingers.values()];
    const t = this.touch;
    if (t) { clearTimeout(t.timer); t.mode = "pinch"; }
    // A selection being dragged stays as it is; a flick stops.
    this.dragging = false;
    this.fling && cancelAnimationFrame(this.fling);
    this.fling = 0;
    this.looking = true;
    const r = this.canvas.getBoundingClientRect();
    const m = mid(a, b);
    this.pinch = { d0: dist(a, b), px0: this.geo.px || this.size, col: (m.x - r.left) / (this.geo.cw || 1), row: (m.y - r.top) / (this.geo.ch || 1), t0: Date.now(), at: m, moved: false };
    this.updateChrome();
  }

  private movePinch(): void {
    const p = this.pinch;
    const [a, b] = [...this.fingers.values()];
    if (!p || !b) return;
    const d = dist(a, b), m = mid(a, b);
    // Still, until the fingers have clearly spread, closed or moved together: a tap stays a tap.
    if (!p.moved && Math.abs(d / p.d0 - 1) < 0.05 && Math.hypot(m.x - p.at.x, m.y - p.at.y) < 10) return;
    p.moved = true;
    p.at = m;
    const z = pinchZoom(p.px0, p.d0, d, this.fitPx());
    // Both fingers moving together pan: the cell that was under them follows them.
    this.zoomTo(z.zoom, z.px, m, p);
  }

  private endPinch(cancelled: boolean): void {
    const p = this.pinch;
    this.pinch = null;
    if (!p) return;
    if (p.moved) this.saveZoom();
    // Two fingers tapped together: fitted ⇄ full size.
    else if (!cancelled && Date.now() - p.t0 < 350) this.setZoom(this.fit ? "size" : "fit", this.size, p.at);
    this.schedule();
  }

  /// Once on a phone, the first time the text size isn't obvious: how to change it.
  private hint(fp: number): void {
    // Not while zooming: the person has found it.
    if (this.looking || this.pinch || store.get("zoomHint", "") || !matchMedia("(pointer: coarse)").matches) return;
    const s = this.host.screen();
    if (!s) return;
    const wide = !this.fit && this.size * charRatio() * s.size.cols > this.wrap.clientWidth;
    const small = this.fit && fp < READABLE_PX;
    if (!wide && !small) return;
    store.set("zoomHint", "1");
    this.host.toast(wide ? "Shown at a size you can read: drag sideways for the rest, pinch in to see it all" : "Small text? Pinch out to zoom in");
  }

  // ── a size that follows this screen ──────────────────────────────────────────────────────

  private resizeTimer = 0;
  resizeSession(): void {
    if (!this.match || !this.host.canType()) return;
    clearTimeout(this.resizeTimer);
    this.resizeTimer = window.setTimeout(() => {
      const s = this.host.screen();
      if (!s) return;
      const n = this.cells();
      if (n.cols !== s.size.cols || n.rows !== s.size.rows) this.host.send({ t: "resize", session: this.host.id(), size: n });
    }, 120);
  }

  /// The connection came back: take the session's size again.
  reassert(): void {
    const s = this.host.screen();
    if (!this.match || !s) return;
    this.host.send({ t: "subscribe", session: this.host.id(), size: this.cells(), control: true });
  }

  /// How many cells fit above the soft keyboard, at the current text size.
  private cells(): { cols: number; rows: number } {
    const m = measure(this.size);
    const vv = window.visualViewport;
    const avail = Math.min(this.wrap.clientHeight, (vv ? vv.height : window.innerHeight) - this.wrap.getBoundingClientRect().top - 2);
    return fitCells(this.wrap.clientWidth - 8, avail, m.cw, m.ch);
  }

  setMatch(on: boolean): void {
    const s = this.host.screen();
    if (!s || !this.host.canType()) return;
    if (on && !this.match) {
      this.origSize = { ...s.size };
      this.match = true;
      // Taking control with this screen's size; later changes are `resize` messages.
      this.zoom = "size";
      store.set("zoom", "size");
      this.host.send({ t: "subscribe", session: this.host.id(), size: this.cells(), control: true });
    } else if (!on && this.match) {
      this.match = false;
      if (this.origSize) this.host.send({ t: "resize", session: this.host.id(), size: this.origSize });
      this.origSize = null;
    }
    this.schedule(true);
    this.updateChrome();
    this.host.changed();
  }

  /// This terminal is going away (another session, another page): hand the size back.
  release(): void {
    if (this.match) this.setMatch(false);
    this.fling && cancelAnimationFrame(this.fling);
    this.input.blur();
  }

  // ── the key row and the menu ─────────────────────────────────────────────────────────────

  private paintSticky(): void {
    for (const [m, b] of this.stickyBtns) {
      b.classList.toggle("on", this.sticky.has(m));
      b.classList.toggle("lock", this.sticky.locked(m));
    }
  }

  private buildKeybar(): HTMLElement {
    // A key that doesn't take the keyboard's focus from the terminal.
    const stay = (e: Event) => e.preventDefault();
    const btn = (label: string, run: () => void, title = tip("term.keys"), cls = ""): HTMLButtonElement =>
      h("button", { class: cls, title, onpointerdown: stay, onmousedown: stay, onclick: run }, label);
    /// A key that repeats while it is held, like the arrows on a keyboard.
    const key = (label: string, code: KeyCode, mods = 0, repeat = false, cls = ""): HTMLButtonElement => {
      const b = btn(label, () => {}, tip("term.keys"), cls);
      let timer = 0, again = 0, fired = false;
      const stop = () => { clearTimeout(timer); clearInterval(again); };
      b.onclick = null;
      b.addEventListener("click", (e) => { if (e.detail === 0 || !fired) this.key({ code, mods }); fired = false; });
      if (repeat) {
        b.addEventListener("pointerdown", () => {
          fired = false;
          timer = window.setTimeout(() => { fired = true; this.key({ code, mods }); again = window.setInterval(() => this.key({ code, mods }), 70); }, 380);
        });
        for (const ev of ["pointerup", "pointerleave", "pointercancel"]) b.addEventListener(ev, stop);
      }
      return b;
    };
    const mod = (label: string, m: number): HTMLButtonElement => {
      const b = btn(label, () => { this.sticky.tap(m); this.paintSticky(); this.focus(); }, tip("term.keys"), "mod");
      this.stickyBtns.set(m, b);
      return b;
    };
    const ch = (c: string, mods = 0): HTMLButtonElement => key(c, { t: "char", c }, mods);
    const ctrl = (c: string): HTMLButtonElement => key(`^${c.toUpperCase()}`, { t: "char", c }, MOD.CTRL);
    return h("div", { class: "keys term-keys", title: tip("term.keys") },
      key("Esc", { t: "esc" }), key("Tab", { t: "tab" }), mod("Ctrl", MOD.CTRL), mod("Alt", MOD.ALT), mod("⇧", MOD.SHIFT),
      key("←", { t: "left" }, 0, true), key("↓", { t: "down" }, 0, true), key("↑", { t: "up" }, 0, true), key("→", { t: "right" }, 0, true),
      key("⏎", { t: "enter" }),
      key("Home", { t: "home" }), key("End", { t: "end" }), key("PgUp", { t: "page_up" }, 0, true), key("PgDn", { t: "page_down" }, 0, true), key("Del", { t: "delete" }, 0, true),
      ctrl("c"), ctrl("d"), ctrl("z"), ctrl("l"), ctrl("r"), ctrl("a"), ctrl("e"), ctrl("w"),
      ch("|"), ch("~"), ch("/"), ch("-"), ch("_"), ch("\\"), ch("`"), ch("{"), ch("}"), ch("["), ch("]"), ch("<"), ch(">"), ch("&"), ch("*"),
      btn("F▾", () => this.fkeys(), tip("term.keys")),
      btn("Paste", () => void this.paste(), tip("term.paste")),
      btn("Aa", () => this.options(), tip("term.options")),
    );
  }

  private fkeys(): void {
    menu("Function keys", Array.from({ length: 12 }, (_, i): Item => ({ label: `F${i + 1}`, run: () => { this.key({ code: { t: "f", n: i + 1 }, mods: 0 }); this.focus(); } })));
  }

  /// What the selection bar offers.
  selectionBar(): HTMLElement {
    const stay = (e: Event) => e.preventDefault();
    const url = this.selectedText().trim();
    this.bar.replaceChildren(
      h("button", { class: "primary", title: tip("term.copy"), onpointerdown: stay, onclick: () => void this.copy() }, "Copy"),
      ...(/^https?:\/\/\S+$/.test(url) ? [h("button", { title: tip("term.link"), onpointerdown: stay, onclick: () => { window.open(url, "_blank", "noopener,noreferrer"); this.clearSelect(); } }, "Open")] : []),
      ...(this.query ? [h("button", { title: tip("term.find"), onpointerdown: stay, onclick: () => void this.find(this.query, -1) }, "↑"), h("button", { title: tip("term.find"), onpointerdown: stay, onclick: () => void this.find(this.query, 1) }, "↓")] : []),
      h("button", { title: tip("term.select"), onpointerdown: stay, onclick: () => void this.selectAll() }, "All"),
      ...(this.host.canType() ? [h("button", { title: tip("term.paste"), onpointerdown: stay, onclick: () => { this.clearSelect(); void this.paste(); } }, "Paste")] : []),
      h("button", { onpointerdown: stay, onclick: () => this.clearSelect() }, "✕"),
    );
    return this.bar;
  }

  async selectAll(): Promise<void> {
    const d = await this.freeze();
    if (!d) return;
    this.sel = { a: { row: 0, col: 0 }, b: { row: d.rows.length - 1, col: Math.max(d.size.cols - 1, 0) } };
    this.schedule(true);
    this.updateChrome();
  }

  /// Find `query` in the history and screen, from the selection (or the bottom) in `dir`.
  async find(query: string, dir: 1 | -1 = 1): Promise<void> {
    const d = await this.freeze();
    const s = this.host.screen();
    if (!d || !s || !query) return;
    this.query = query;
    const n = d.rows.length;
    const from = this.sel ? ordered(this.sel)[dir === 1 ? 1 : 0] : dir === 1 ? { row: 0, col: -1 } : { row: n - 1, col: Infinity };
    const hit = findIn(d.rows, query, from, dir);
    if (!hit) { this.host.toast(`“${query}” isn't there`); return; }
    this.sel = hit;
    // Put the match in the middle of the window.
    const top = Math.max(0, hit.a.row - Math.floor(s.size.rows / 2));
    this.offset = Math.min(Math.max(d.rows.length - s.size.rows - top, 0), maxOffset(d.rows.length, s.size.rows));
    this.bar.hidden = true;
    this.schedule(true);
    this.updateChrome();
  }

  /// The program rang the bell: a flash, and a buzz where the phone has one.
  bell(): void {
    this.el.classList.add("bell");
    setTimeout(() => this.el.classList.remove("bell"), 160);
    if (this.typing) navigator.vibrate?.(60);
  }

  private link(e: { clientX: number; clientY: number }): { url: string } | null {
    const d = this.view();
    if (!d) return null;
    const p = this.pos(e);
    const row = d.rows[p.cell.row] ?? [];
    return urlAt(row, p.col);
  }

  options(): void {
    const on = (b: boolean) => (b ? "on" : "off");
    const shown = Math.round(this.geo.px || this.size);
    const can = this.host.canType();
    menu("The terminal", [
      { label: this.fit ? "Show at full size" : "Fit to the width", hint: this.fit ? "Readable text, scroll sideways" : "Shrink the whole screen into view", title: tip("term.zoom"),
        run: () => this.setZoom(this.fit ? "size" : "fit") },
      this.zoom !== "auto" && { label: "Size it automatically", hint: `Fit to the width while the text is ${READABLE_PX}px or more, else full size`, title: tip("term.zoom"), run: () => this.setZoom("auto") },
      { label: "Bigger text", hint: `${shown}px · or pinch out`, title: tip("term.zoom"), run: () => this.stepZoom(1) },
      { label: "Smaller text", hint: `${shown}px · or pinch in`, title: tip("term.zoom"), run: () => this.stepZoom(-1) },
      { label: `Follow the cursor: ${on(this.follow)}`, hint: "When the screen is wider than the box", run: () => { this.follow = !this.follow; store.set("follow", this.follow ? "1" : "0"); } },
      can && { label: `On-screen keyboard: ${on(this.keyboard)}`, hint: "Off keeps a hardware keyboard's keys without covering the screen", run: () => { this.keyboard = !this.keyboard; store.set("keyboard", this.keyboard ? "1" : "0"); this.setKeyboard(); if (this.typing) { this.input.blur(); this.focus(); } } },
      can && { label: `Mouse to the program: ${on(this.mouseApp)}`, hint: "Clicks and the wheel, when it asks for them", title: tip("term.mouse"), run: () => { this.mouseApp = !this.mouseApp; store.set("mouse", this.mouseApp ? "1" : "0"); } },
      can && { label: this.match ? "Stop resizing the session" : "Resize the session to this screen", hint: "Everyone attached sees the new size; the last to type decides", title: tip("term.resize"), run: () => this.setMatch(!this.match) },
      { label: "Find in the history…", hint: this.query ? `Last: ${this.query}` : "The text of the session and what scrolled off", title: tip("term.find"), run: () => { form("Find", [{ key: "q", label: "Text", value: this.query, required: true }], "Find", async (v) => { await this.find(String(v.q), -1); }); } },
      { label: "Select all and copy", hint: "The history and the screen", title: tip("term.copy"), run: async () => { await this.selectAll(); await this.copy(); } },
      { label: "Jump to the top of the history", title: tip("term.scroll"), run: async () => { const d = await this.freeze(); const s = this.host.screen(); if (d && s) { this.offset = maxOffset(d.rows.length, s.size.rows); this.schedule(); this.updateChrome(); } } },
    ]);
  }
}

const modsOf = (e: { shiftKey: boolean; altKey: boolean; ctrlKey: boolean }): number => (e.shiftKey ? MOD.SHIFT : 0) | (e.altKey ? MOD.ALT : 0) | (e.ctrlKey ? MOD.CTRL : 0);

type Point = { x: number; y: number };
const dist = (a: Point, b: Point): number => Math.max(1, Math.hypot(a.x - b.x, a.y - b.y));
const mid = (a: Point, b: Point): Point => ({ x: (a.x + b.x) / 2, y: (a.y + b.y) / 2 });

/// The zoom kept on this device; before there was one, a fit flag (which was the default).
function zoomOf(zoom: string, fit: string): Zoom {
  if (zoom === "auto" || zoom === "fit" || zoom === "size") return zoom;
  return fit === "1" ? "fit" : fit === "0" ? "size" : "auto";
}

/// A cell's width over the font size, for the font the terminal draws with.
let ratio = 0;
function charRatio(): number {
  if (!ratio) {
    const ctx = document.createElement("canvas").getContext("2d")!;
    ctx.font = `100px ${FAMILY}`;
    ratio = ctx.measureText("M").width / 100 || 0.6;
  }
  return ratio;
}

/// The size of a cell at this font size.
function measure(px: number): { cw: number; ch: number } {
  return { cw: charRatio() * px, ch: Math.round(px * 1.25) };
}
