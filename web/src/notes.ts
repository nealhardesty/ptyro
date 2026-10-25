// The scratchpad on the web (ROADMAP NOTE1.5): the Notes view, the quick-add box, the offline
// queue and the share target. Self-contained: app.ts gives it a few hooks (`initNotes`) and
// mounts what it returns; the Markdown reader and the queue are in pad.ts.

import { sheet } from "./kit.js";
import { newTask } from "./sessions.js";
import type { Session } from "./screen.js";
import { Queue, noteFor, parseMarkdown, shareLine } from "./pad.js";
import type { Block, Inline, Op } from "./pad.js";

/// What notes.ts needs from the app around it.
export interface NotesHost {
  request(req: object): Promise<any>;
  connected(): boolean;
  toast(text: string): void;
  render(): void;
  /// A page header with a back arrow.
  header(title: string, back: boolean): HTMLElement;
  /// The tooltip of a web action (`ACTIONS` in app.ts).
  tip(action: any): string;
}
let ctx!: NotesHost;
export function initNotes(host: NotesHost): void { ctx = host; }

// ── a little DOM ────────────────────────────────────────────────────────────────────────────

type Child = Node | string | null | undefined | false;
function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, any> = {}, ...kids: Child[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k.startsWith("on") && typeof v === "function") el.addEventListener(k.slice(2), v);
    else if (k === "class") el.className = v;
    else if (v === true) el.setAttribute(k, "");
    else if (v !== false && v != null) el.setAttribute(k, String(v));
  }
  for (const c of kids) if (c) el.append(c);
  return el;
}

/// Make `kids` the children of `parent`, in order, without moving nodes already there in that
/// order (a moved node loses focus and composition).
function patch(parent: HTMLElement, kids: Node[]): void {
  const keep = new Set(kids);
  for (const c of [...parent.childNodes]) if (!keep.has(c)) c.remove();
  let at: ChildNode | null = parent.firstChild;
  for (const k of kids) {
    if (k === at) { at = at.nextSibling; continue; }
    parent.insertBefore(k, at);
  }
}

function agoOf(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

const onNotes = (): boolean => location.hash === "#/notes";

// ── storage: the queue lives in IndexedDB (the app's own "ptyro" database) ─────────────────────

const db = (): Promise<IDBDatabase> =>
  new Promise((ok, err) => {
    const r = indexedDB.open("ptyro", 1);
    r.onupgradeneeded = () => r.result.createObjectStore("kv");
    r.onsuccess = () => ok(r.result);
    r.onerror = () => err(r.error);
  });
async function idbSet(key: string, value: string | null): Promise<void> {
  const d = await db();
  await new Promise<void>((ok, err) => {
    const tx = d.transaction("kv", "readwrite");
    if (value === null) tx.objectStore("kv").delete(key);
    else tx.objectStore("kv").put(value, key);
    tx.oncomplete = () => ok();
    tx.onerror = () => err(tx.error);
  });
}
async function idbGet(key: string): Promise<string | null> {
  const d = await db();
  return new Promise<string | null>((ok, err) => {
    const r = d.transaction("kv").objectStore("kv").get(key);
    r.onsuccess = () => ok(typeof r.result === "string" ? r.result : null);
    r.onerror = () => err(r.error);
  });
}

interface PadDoc { path: string; rev: string; text: string; open: number; done: number }
interface PadItem { line: number; done: boolean; text: string; section?: string; at?: string; whence?: string; by?: string; worktree?: string; pr?: number }
interface PadSnap { rev: string; at_ms: number; bytes: number; lines: number }

interface PadShell {
  el: HTMLElement;
  ta: HTMLTextAreaElement;
  preview: HTMLElement;
  quick: HTMLElement;
  find: HTMLInputElement;
}

const pad = {
  doc: null as PadDoc | null,
  /// The revision the text in the box descends from: the daemon merges an edit from it.
  base: "",
  dirty: false,
  saving: false,
  /// Saves refused one after another: each waits longer, and after a few it stops until you type again.
  fails: 0,
  failure: "",
  timer: 0,
  readOnly: false,
  conflict: false,
  preview: false,
  find: "",
  items: [] as PadItem[],
  history: null as PadSnap[] | null,
  /// The saved copy the next tap restores.
  confirm: "",
  open: 0,
  queued: 0,
  quickText: "",
  focusQuick: false,
  shell: null as PadShell | null,
  listEl: null as HTMLElement | null,
  quickEl: null as HTMLElement | null,
};

/// The queue lives in IndexedDB, else localStorage, else only until the page closes.
const queue = new Queue({
  async load() {
    try { const v = await idbGet("padq"); if (v) return v; } catch { /* fall through */ }
    try { return localStorage.getItem("ptyro.padq"); } catch { return null; }
  },
  async save(v) {
    try { await idbSet("padq", v); return; } catch { /* fall through */ }
    try { if (v === null) localStorage.removeItem("ptyro.padq"); else localStorage.setItem("ptyro.padq", v); } catch { /* none */ }
  },
});

const denied = (e: unknown): boolean => (e as { code?: string }).code === "permission_denied";

function padRefused(): void {
  pad.readOnly = true;
  if (pad.shell) pad.shell.ta.readOnly = true;
  ctx.toast("This device is paired read-only: pair it again with `ptyro pair --scope control` to write.");
}

/// Replace the box's text, keeping the caret and scroll where they were.
function setPadText(ta: HTMLTextAreaElement, text: string): void {
  if (ta.value === text) return;
  const [a, b, top] = [ta.selectionStart, ta.selectionEnd, ta.scrollTop];
  ta.value = text;
  ta.setSelectionRange(Math.min(a, text.length), Math.min(b, text.length));
  ta.scrollTop = top;
}

function applyDoc(doc: PadDoc): void {
  pad.doc = doc;
  pad.open = doc.open;
  if (pad.shell && !pad.dirty && !pad.saving) {
    setPadText(pad.shell.ta, doc.text);
    pad.base = doc.rev;
  } else if (!pad.shell) {
    pad.base = doc.rev;
  }
}

async function padLoad(): Promise<void> {
  try {
    const r = await ctx.request({ t: "scratch_get" });
    applyDoc(r.doc);
  } catch { /* reconnecting */ }
  if (onNotes()) void padItems();
  ctx.render();
}

async function padItems(): Promise<void> {
  try {
    const r = await ctx.request({ t: "scratch_todo", all: false });
    pad.items = r.items;
    ctx.render();
  } catch { /* reconnecting */ }
}

function padSaveSoon(ms = 800): void {
  clearTimeout(pad.timer);
  pad.timer = window.setTimeout(() => void padSave(), ms);
}

function queued(): void {
  pad.queued = queue.length;
  ctx.render();
}

/// Save what's in the box: a Put from the revision it descends from, which the daemon merges
/// with whatever was written meanwhile. Offline, it waits in the queue.
async function padSave(): Promise<void> {
  clearTimeout(pad.timer);
  const sh = pad.shell;
  if (!sh || !pad.dirty || pad.readOnly) return;
  if (pad.saving) { padSaveSoon(200); return; }
  const text = sh.ta.value;
  const base = pad.base;
  pad.dirty = false;
  if (!ctx.connected()) {
    await queue.push({ k: "put", base, text });
    queued();
    return;
  }
  pad.saving = true;
  try {
    const r = await ctx.request({ t: "scratch_put", base, text });
    pad.fails = 0;
    pad.failure = "";
    afterPut(r.doc, text, r.conflicts);
  } catch (e) {
    if (denied(e)) padRefused();
    else if (!ctx.connected()) { await queue.push({ k: "put", base, text }); queued(); }
    else {
      // The daemon said no: trying again at once would only repeat it. Back off, then stop and say so.
      pad.dirty = true;
      pad.fails++;
      pad.failure = (e as Error).message;
      if (pad.fails <= 1) ctx.toast(pad.failure);
    }
  } finally {
    pad.saving = false;
    if (pad.dirty && !pad.readOnly && pad.fails < 6) padSaveSoon(Math.min(30_000, 200 * 2 ** pad.fails));
    ctx.render();
  }
}

/// The daemon wrote `doc` for the edit `sent`. If nothing was typed since, the box takes the
/// merged text; if there was, the box keeps what it has and the next save merges again from the
/// same ancestor, so neither side's lines are lost.
function afterPut(doc: PadDoc, sent: string, conflicts: number): void {
  pad.doc = doc;
  pad.open = doc.open;
  pad.conflict = conflicts > 0;
  const ta = pad.shell?.ta;
  const typed = !!ta && ta.value !== sent;
  if (ta && !typed) { setPadText(ta, doc.text); pad.base = doc.rev; }
  else if (doc.text === sent) pad.base = doc.rev;
  if (!ta) pad.base = doc.rev;
  if (onNotes()) void padItems();
}

/// Send what waited offline, in order.
async function padFlush(): Promise<void> {
  if (!queue.length) return;
  let dropped = 0;
  const n = await queue.flush(async (op: Op) => {
    try {
      if (op.k === "add") await ctx.request({ t: "scratch_add", text: op.text, section: null, note: !!op.note, stamp: true, session: null, cwd: null });
      else {
        const r = await ctx.request({ t: "scratch_put", base: op.base, text: op.text });
        const typed = !!pad.shell && pad.shell.ta.value !== op.text;
        if (!typed) afterPut(r.doc, op.text, r.conflicts);
        else { pad.doc = r.doc; pad.conflict = r.conflicts > 0; }
      }
    } catch (e) {
      if (!denied(e)) throw e;
      dropped++;
      padRefused();
    }
  });
  pad.queued = queue.length;
  if (n && !dropped) ctx.toast(`Sent ${n} queued ${n === 1 ? "change" : "changes"} to the scratchpad`);
  void padLoad();
}

/// A line for the pad (`ptyro scratchpad add`), stamped by the daemon; queued while offline.
async function padAdd(text: string): Promise<boolean> {
  if (!text.trim()) return false;
  if (!ctx.connected()) {
    await queue.push({ k: "add", text });
    ctx.toast("Queued: it will be added when ptyro is reachable");
    queued();
    return true;
  }
  try {
    const r = await ctx.request({ t: "scratch_add", text, section: null, note: false, stamp: true, session: null, cwd: null });
    applyDoc(r.doc);
    ctx.toast("Added to the scratchpad");
    if (onNotes()) void padItems();
    return true;
  } catch (e) {
    if (denied(e)) padRefused(); else ctx.toast((e as Error).message);
    return false;
  }
}

async function padTick(line: number, done: boolean, expect = ""): Promise<void> {
  await padSave();
  try {
    // The line number is from what this page last saw: if the pad changed since (an agent, another
    // device), the same number may be another item. Look first.
    if (expect) {
      const r = await ctx.request({ t: "scratch_get" });
      const at = String(r.doc.text ?? "").split("\n")[line - 1] ?? "";
      if (!at.includes(expect.slice(0, 40))) { ctx.toast("The pad changed meanwhile: it is up to date now. Tap again."); await padLoad(); return; }
    }
    await ctx.request({ t: "scratch_mark", sel: String(line), mark: done ? { done: { pr: null } } : "open" });
    await padLoad();
  } catch (e) {
    if (denied(e)) padRefused(); else ctx.toast((e as Error).message);
  }
}

async function padHistory(): Promise<void> {
  if (pad.history) { pad.history = null; pad.confirm = ""; ctx.render(); return; }
  try {
    const r = await ctx.request({ t: "scratch_history" });
    pad.history = r.snapshots;
  } catch (e) { ctx.toast((e as Error).message); }
  ctx.render();
}

async function padRestore(rev: string): Promise<void> {
  if (pad.confirm !== rev) { pad.confirm = rev; ctx.render(); return; }
  try {
    const r = await ctx.request({ t: "scratch_restore", rev });
    pad.dirty = false;
    clearTimeout(pad.timer);
    pad.doc = r.doc;
    pad.open = r.doc.open;
    if (pad.shell) setPadText(pad.shell.ta, r.doc.text);
    pad.base = r.doc.rev;
    pad.history = null;
    pad.confirm = "";
    ctx.toast("Put the saved copy back (the one it replaced is saved too)");
    void padItems();
  } catch (e) {
    if (denied(e)) padRefused(); else ctx.toast((e as Error).message);
  }
  ctx.render();
}

/// Move the box's selection to the next (or previous) match of the find text.
function padFindNext(dir: 1 | -1): void {
  const sh = pad.shell;
  const q = pad.find.toLowerCase();
  if (!sh || !q) return;
  if (pad.preview) { pad.preview = false; ctx.render(); }
  const hay = sh.ta.value.toLowerCase();
  let at = dir === 1 ? hay.indexOf(q, sh.ta.selectionEnd) : hay.lastIndexOf(q, sh.ta.selectionStart - 1);
  if (at < 0) at = dir === 1 ? hay.indexOf(q) : hay.lastIndexOf(q);
  if (at < 0) { ctx.toast("Not found"); return; }
  sh.ta.focus();
  sh.ta.setSelectionRange(at, at + q.length);
  const lineNo = sh.ta.value.slice(0, at).split("\n").length - 1;
  const lh = parseFloat(getComputedStyle(sh.ta).lineHeight) || 18;
  sh.ta.scrollTop = Math.max(0, lineNo * lh - sh.ta.clientHeight / 3);
}

function inlineEls(parts: Inline[]): Node[] {
  return parts.map((p) => {
    if (p.t === "b") return h("strong", {}, p.text);
    if (p.t === "i") return h("em", {}, p.text);
    if (p.t === "code") return h("code", {}, p.text);
    if (p.t === "a") return h("a", { href: p.href, target: "_blank", rel: "noopener noreferrer" }, p.text);
    return document.createTextNode(p.text);
  });
}

/// Markdown as elements built from text nodes: nothing from the file is ever parsed as HTML.
function previewEls(src: string): HTMLElement[] {
  return parseMarkdown(src).map((b: Block) => {
    switch (b.k) {
      case "h": return h(`h${Math.min(6, b.level + 1)}` as "h2", { class: "md-h" }, ...inlineEls(b.inl));
      case "p": return h("p", {}, ...inlineEls(b.inl));
      case "quote": return h("blockquote", {}, ...inlineEls(b.inl));
      case "code": return h("pre", {}, h("code", {}, b.text));
      case "hr": return h("hr");
      case "li": {
        const box = b.check === null ? null : h("input", { type: "checkbox", title: ctx.tip("scratchpad.tick"), checked: b.check, disabled: pad.readOnly || null });
        if (box) box.addEventListener("change", () => void padTick(b.line, box.checked, b.inl.map((x: { text?: string }) => x.text ?? "").join("")));
        return h("div", { class: `md-li d${Math.min(b.indent, 4)} ${b.check ? "done" : ""}` }, box ?? "• ", h("span", {}, ...inlineEls(b.inl)));
      }
    }
  });
}

function padTags(i: PadItem): string {
  return [i.section, i.by ? `by ${i.by}` : "", i.worktree ? `→ ${i.worktree}` : "", i.whence && !i.worktree ? i.whence : "", i.at].filter(Boolean).join(" · ");
}

function shellOf(): PadShell {
  if (pad.shell) return pad.shell;
  const ta = h("textarea", { class: "pad-text", spellcheck: "false", autocapitalize: "off", autocomplete: "off", title: ctx.tip("scratchpad"), "aria-label": "Scratchpad" });
  ta.value = pad.doc?.text ?? "";
  ta.readOnly = pad.readOnly;
  ta.addEventListener("input", () => {
    pad.dirty = true;
    pad.fails = 0;
    padSaveSoon();
  });
  ta.addEventListener("blur", () => void padSave());
  const find = h("input", { class: "pad-find", type: "search", placeholder: "Find", autocomplete: "off", "aria-label": "Find in the scratchpad" });
  find.addEventListener("input", () => { pad.find = find.value; });
  find.addEventListener("keydown", (e) => { if (e.key === "Enter") { e.preventDefault(); padFindNext(e.shiftKey ? -1 : 1); } });
  pad.shell = { el: h("div", { class: "notes" }), ta, preview: h("div", { class: "pad-preview" }), quick: quickAdd(), find };
  return pad.shell;
}

function notesPage(): HTMLElement {
  const sh = shellOf();
  const find = sh.find;
  const items = pad.items;
  const history = pad.history;
  const parts: Node[] = [
    ctx.header("Notes", true),
    h("div", { class: "pad-head" },
      h("span", { class: "pad-path", title: pad.doc?.path ?? "" }, pad.doc?.path ?? "…"),
      h("span", {}, `${pad.open} open`),
      pad.queued ? h("span", { class: "tag", title: "waiting for ptyro to be reachable" }, `queued (${pad.queued})`) : null,
      pad.dirty || pad.saving ? h("span", { class: "muted" }, "saving…") : null),
    h("div", { class: "pad-bar" },
      h("button", { class: pad.preview ? "" : "on", onclick: () => { pad.preview = false; ctx.render(); } }, "Edit"),
      h("button", { class: pad.preview ? "on" : "", onclick: () => { void padSave(); pad.preview = true; ctx.render(); } }, "Preview"),
      find,
      h("button", { onclick: () => padFindNext(-1), title: "Previous match" }, "‹"),
      h("button", { onclick: () => padFindNext(1), title: "Next match" }, "›"),
      h("button", { class: history ? "on" : "", title: ctx.tip("scratchpad.history"), onclick: () => void padHistory() }, "History")),
  ];
  if (pad.readOnly) parts.push(h("div", { class: "banner" }, "Read-only: this device is paired without control. Pair it again with `ptyro pair --scope control` to write."));
  if (pad.fails >= 6) parts.push(h("div", { class: "banner bad" }, `This page can't save the scratchpad: ${pad.failure}`,
    h("button", { class: "small", onclick: () => { pad.fails = 0; void padSave(); } }, "Try again")));
  if (pad.conflict) parts.push(h("div", { class: "banner bad" }, "Another change clashed with yours: both versions are kept between <<<<<<< markers. Fix the text and it saves.",
    h("button", { class: "small", onclick: () => { pad.conflict = false; ctx.render(); } }, "Got it")));
  if (history) {
    parts.push(h("section", { class: "pad-history", title: ctx.tip("scratchpad.history") },
      h("b", {}, "Saved copies"),
      ...(history.length ? history.map((s) => h("button", { class: `snap ${pad.confirm === s.rev ? "on" : ""}`, onclick: () => void padRestore(s.rev) },
        `${agoOf(s.at_ms)} ago · ${s.lines} lines`, h("small", {}, pad.confirm === s.rev ? "tap again to put it back" : s.rev))) : [h("p", { class: "empty" }, "No saved copies yet.")])));
  }
  if (pad.preview) {
    sh.preview.replaceChildren(...previewEls(sh.ta.value));
    parts.push(sh.preview);
  } else parts.push(sh.ta);
  parts.push(h("details", { class: "pad-items", title: ctx.tip("scratchpad.items"), open: items.length > 0 && items.length <= 8 || null },
    h("summary", {}, `Open items (${items.length})`),
    ...(items.length ? items.map((i) => h("div", { class: "pad-item" },
      h("button", { class: "small", title: ctx.tip("scratchpad.tick"), disabled: pad.readOnly || null, onclick: () => void padTick(i.line, true, i.text) }, "☐"),
      h("div", { class: "main" }, h("div", {}, i.text), h("small", {}, padTags(i))),
      h("button", { class: "small", title: ctx.tip("scratchpad.start"), onclick: () => newTask(undefined, i.text) }, "Start"))) : [h("p", { class: "empty" }, "Nothing open. Add a line below.")]),
  ));
  parts.push(sh.quick);
  patch(sh.el, parts);
  return sh.el;
}

/// The one-line add box. One element, kept across renders so typing isn't interrupted.
function quickAdd(): HTMLElement {
  if (pad.quickEl) return pad.quickEl;
  const input = h("input", { class: "quick-input", placeholder: "Add to the scratchpad…", enterkeyhint: "send", autocomplete: "off", title: ctx.tip("scratchpad.add") });
  const go = async () => {
    const text = input.value;
    if (await padAdd(text)) { input.value = ""; pad.quickText = ""; }
  };
  input.addEventListener("input", () => { pad.quickText = input.value; });
  input.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.isComposing) { e.preventDefault(); void go(); } });
  pad.quickEl = h("div", { class: "quick-add" }, input, h("button", { onclick: () => void go(), title: ctx.tip("scratchpad.add") }, "Add"));
  return pad.quickEl;
}

/// `scratchpad.note`: the session's words and a link back, ready to send.
function noteSession(s: Session): void {
  pad.quickText = noteFor(s);
  pad.focusQuick = true;
  if (location.hash === "#/") ctx.render(); else location.hash = "#/";
}

/// What the phone's share sheet sent us arrives as `/?title=&text=&url=`.
function takeShare(): void {
  const q = new URLSearchParams(location.search);
  if (!["title", "text", "url"].some((k) => q.has(k))) return;
  const line = shareLine(q);
  history.replaceState(null, "", "/" + (location.hash || "#/"));
  // A link anyone can make: nothing goes into the pad (which agents read) without being seen first.
  if (line) {
    const s = sheet("Add this to the scratchpad?", [h("pre", { class: "block" }, line)], [
      h("button", { onclick: () => s.close() }, "No"),
      h("button", { class: "primary", onclick: () => { s.close(); void padAdd(line); } }, "Add"),
    ]);
  }
}


// ── what app.ts mounts ─────────────────────────────────────────────────────────────────────

/// The Notes page (`#/notes`).
export const notesView = (): HTMLElement => notesPage();

/// The header's ✎ link, with the open count.
export function notesLink(): HTMLElement {
  return h("a", { class: "inbox-link", href: "#/notes", title: ctx.tip("scratchpad") }, "✎", pad.open ? h("b", { class: "open" }, String(pad.open)) : null);
}

/// The quick-add box with its badge, for the top of the session list. One element, patched in
/// place, so typing isn't interrupted by the renders around it.
export function notesQuickAdd(): HTMLElement {
  const quick = quickAdd();
  const input = quick.firstChild as HTMLInputElement;
  if (pad.quickText !== input.value) input.value = pad.quickText;
  pad.listEl ??= h("div", { class: "quick-wrap" });
  patch(pad.listEl, [
    quick,
    pad.open ? h("a", { class: "badge", href: "#/notes", title: ctx.tip("scratchpad") }, `✎ ${pad.open} open`) : null,
    pad.queued ? h("span", { class: "tag", title: "waiting for ptyro to be reachable" }, `queued (${pad.queued})`) : null,
  ].filter((n): n is HTMLElement => !!n));
  if (pad.focusQuick) { pad.focusQuick = false; requestAnimationFrame(() => input.focus()); }
  return pad.listEl;
}

/// A "note this session" button (`scratchpad.note`): `inCard` stops the card's link opening.
export function notesNoteButton(s: Session, inCard: boolean): HTMLElement {
  return h("button", {
    class: inCard ? "icon note" : "",
    title: ctx.tip("scratchpad.note"),
    onclick: (e: Event) => { if (inCard) { e.preventDefault(); e.stopPropagation(); } noteSession(s); },
  }, inCard ? "✎" : "✎ Note");
}

/// The socket (re)connected: send what waited, then read the pad.
export function notesConnected(): void { void padFlush().then(() => padLoad()); }

/// A `scratch_changed` event.
export function notesEvent(e: { rev: string; open: number }): void {
  pad.open = e.open;
  if (pad.doc?.rev !== e.rev) void (pad.dirty ? padSave() : Promise.resolve()).then(() => padLoad());
  else ctx.render();
}

/// The page changed to `#/notes`.
export function notesShown(): void { if (onNotes() && ctx.connected()) void padItems(); }

/// Whether render should load the pad for the first time.
export function notesNeedsLoad(): boolean { return onNotes() && pad.doc === null && ctx.connected(); }
export { padLoad as notesLoad };

/// Read the queue left by an earlier visit, then take what the share sheet sent.
export function notesStart(): void {
  void queue.load().then(() => { pad.queued = queue.length; takeShare(); ctx.render(); });
}
