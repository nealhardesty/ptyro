// What every view shares: the DOM helper, sheets (the web's dialogs and menus), forms, and the
// hooks the app wires up (re-render, toast, header). No dependencies.

import { conn } from "./conn.js";

// ── tiny DOM helper ─────────────────────────────────────────────────────────────────────────

export type Child = Node | string | null | undefined | false;
export function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, any> = {}, ...kids: Child[]): HTMLElementTagNameMap[K] {
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

export function ago(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 60) return `${s}s`;
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

/// Make `kids` the children of `parent`, in order, without moving nodes already there in that
/// order (a moved node loses focus and composition).
export function patch(parent: HTMLElement, kids: Node[]): void {
  const keep = new Set(kids);
  for (const c of [...parent.childNodes]) if (!keep.has(c)) c.remove();
  let at: ChildNode | null = parent.firstChild;
  for (const k of kids) {
    if (k === at) { at = at.nextSibling; continue; }
    parent.insertBefore(k, at);
  }
}

// ── actions: the controls, by name, and their tooltips ────────────────────────────────────

const tips: Record<string, string> = {};
/// Register a module's `ACTIONS` and get its typed `tip`. The Web column of docs/parity.md names
/// these, and `make parity` checks every name it uses is registered in some module's `ACTIONS`.
export function actions<T extends Record<string, string>>(a: T): (k: keyof T & string) => string {
  Object.assign(tips, a);
  return (k) => a[k];
}
/// Another module's action, by name.
export const tipOf = (k: string): string => tips[k] ?? "";

// ── hooks the app sets ──────────────────────────────────────────────────────────────────────

export const ui = {
  render: () => {},
  toast: (_text: string) => {},
  /// A page's top bar: the title, a back arrow, and extra controls on its right.
  header: (title: string, _back: boolean | string, ..._extra: Child[]): HTMLElement => h("header", {}, h("h1", {}, title)),
};

/// A view: `#/<name>/<rest>`. Views are built on every render, so they read what's loaded and
/// ask `load` for what isn't (a `Cache` below).
export type View = (rest: string) => HTMLElement;
export const views = new Map<string, View>();

// ── asking the daemon ───────────────────────────────────────────────────────────────────────

export const errText = (e: unknown): string => (e as Error)?.message || String(e);

/// Run a request; on failure toast why and resolve `undefined`.
export async function ask<T = any>(req: object, ok?: string): Promise<T | undefined> {
  try {
    const r = await conn.request(req);
    if (ok) ui.toast(ok);
    return r as T;
  } catch (e) {
    ui.toast(errText(e));
    return undefined;
  }
}

/// A keyed cache of loaded data for views, which render synchronously: `get` returns what's
/// loaded (and starts loading what isn't, re-rendering when it arrives); `reload` forgets it.
export class Cache<T> {
  private data = new Map<string, T>();
  private loading = new Set<string>();
  private errors = new Map<string, string>();
  constructor(private fetcher: (key: string) => Promise<T>) {}
  get(key: string): T | undefined {
    if (!this.data.has(key) && !this.loading.has(key) && !this.errors.has(key) && conn.state === "open") {
      this.loading.add(key);
      this.fetcher(key).then((v) => this.data.set(key, v), (e) => this.errors.set(key, errText(e)))
        .finally(() => { this.loading.delete(key); ui.render(); });
    }
    return this.data.get(key);
  }
  error(key: string): string | undefined { return this.errors.get(key); }
  set(key: string, v: T): void { this.data.set(key, v); }
  /// Forget `key` (everything without one) and load again.
  reload(key?: string): void {
    if (key === undefined) { this.data.clear(); this.errors.clear(); } else { this.data.delete(key); this.errors.delete(key); }
    ui.render();
  }
}

/// What a cached value shows while it loads, or when it failed.
export function pending<T>(c: Cache<T>, key: string, what = "Loading…"): HTMLElement | null {
  const err = c.error(key);
  if (err) return h("p", { class: "empty err" }, err, " ", h("button", { class: "small", onclick: () => c.reload(key) }, "Retry"));
  return c.get(key) === undefined ? h("p", { class: "empty" }, what) : null;
}

// ── sheets ──────────────────────────────────────────────────────────────────────────────────

export interface Sheet { el: HTMLElement; close: () => void }
let openSheets: Sheet[] = [];

/// A dialog that slides up on a phone and sits in the middle on a desktop. It lives outside the
/// page (so re-renders leave it alone) and closes on Esc or a tap outside it.
export function sheet(title: string, body: Child[], foot: Child[] = []): Sheet {
  const scrim = h("div", { class: "scrim" });
  const el = h("div", { class: "sheet", role: "dialog", "aria-label": title },
    h("div", { class: "sheet-head" }, h("h2", {}, title), h("button", { class: "icon", "aria-label": "Close", onclick: () => s.close() }, "✕")),
    h("div", { class: "sheet-body" }, ...body),
    foot.length ? h("div", { class: "sheet-foot" }, ...foot) : null);
  scrim.append(el);
  const onKey = (e: KeyboardEvent) => { if (e.key === "Escape" && openSheets[openSheets.length - 1] === s) s.close(); };
  const s: Sheet = {
    el: scrim,
    close: () => { scrim.remove(); document.removeEventListener("keydown", onKey); openSheets = openSheets.filter((x) => x !== s); },
  };
  scrim.addEventListener("mousedown", (e) => { if (e.target === scrim) s.close(); });
  document.addEventListener("keydown", onKey);
  document.body.append(scrim);
  openSheets.push(s);
  return s;
}

export interface Item { label: string; hint?: string; danger?: boolean; run: () => unknown; disabled?: boolean; title?: string }

/// A list of things to do; choosing one closes the sheet and runs it. Falsy entries are skipped,
/// so a menu can say `cond && {…}`.
export function menu(title: string, items: (Item | false | null | undefined | "")[]): Sheet {
  const list = items.filter(Boolean) as Item[];
  const s: Sheet = sheet(title, [h("div", { class: "menu" },
    ...(list.length ? list.map((it) => h("button", {
      class: it.danger ? "danger" : "", disabled: it.disabled, title: it.title,
      onclick: () => { s.close(); void it.run(); },
    }, h("span", {}, it.label), it.hint ? h("small", {}, it.hint) : null)) : [h("p", { class: "empty" }, "Nothing to do here.")]))]);
  return s;
}

/// Ask before doing something that can't be undone.
export function confirm(title: string, message: Child, label: string, run: () => unknown, danger = true): Sheet {
  const s: Sheet = sheet(title, [h("p", { class: "confirm" }, message)], [
    h("button", { onclick: () => s.close() }, "Cancel"),
    h("button", { class: danger ? "deny" : "primary", onclick: () => { s.close(); void run(); } }, label),
  ]);
  return s;
}

/// Show text (a command's output, a file) in a sheet, with a copy button.
export function textSheet(title: string, text: string): Sheet {
  return sheet(title, [h("pre", { class: "block" }, text || "(empty)")], [
    h("button", { onclick: () => void navigator.clipboard?.writeText(text).then(() => ui.toast("Copied"), () => ui.toast("Couldn't copy")) }, "Copy"),
  ]);
}

// ── forms ───────────────────────────────────────────────────────────────────────────────────

export type Field =
  | { key: string; label: string; type?: "text" | "password" | "number"; value?: string | number | null; placeholder?: string; hint?: string; required?: boolean }
  | { key: string; label: string; type: "textarea"; value?: string | null; placeholder?: string; hint?: string; rows?: number; required?: boolean }
  | { key: string; label: string; type: "select"; options: ([string, string] | string)[]; value?: string | null; hint?: string }
  | { key: string; label: string; type: "check"; value?: boolean; hint?: string };

export type Values = Record<string, string | boolean>;

/// A form in a sheet. `go` gets the values; if it throws, the message shows in the form and the
/// sheet stays; if it returns, the sheet closes. Empty text fields are left out of the values.
export function form(title: string, fields: Field[], label: string, go: (v: Values) => Promise<unknown> | unknown, opts: { danger?: boolean; intro?: Child } = {}): Sheet {
  const inputs = new Map<string, HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement>();
  const rows = fields.map((f) => {
    let input: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement;
    if (f.type === "textarea") {
      input = h("textarea", { rows: f.rows ?? 4, placeholder: f.placeholder, autocapitalize: "off" });
      input.value = f.value ?? "";
    } else if (f.type === "select") {
      input = h("select", {}, ...f.options.map((o) => { const [v, l] = typeof o === "string" ? [o, o] : o; return h("option", { value: v }, l); }));
      input.value = f.value ?? (typeof f.options[0] === "string" ? f.options[0] : f.options[0][0]);
    } else if (f.type === "check") {
      input = h("input", { type: "checkbox" });
      input.checked = !!f.value;
    } else {
      input = h("input", { type: f.type ?? "text", placeholder: f.placeholder, autocapitalize: "off", autocomplete: "off", spellcheck: "false", inputmode: f.type === "number" ? "numeric" : undefined });
      input.value = f.value == null ? "" : String(f.value);
    }
    inputs.set(f.key, input);
    return f.type === "check"
      ? h("label", { class: "field check" }, input, h("span", {}, f.label), f.hint ? h("small", {}, f.hint) : null)
      : h("label", { class: "field" }, h("span", {}, f.label), input, f.hint ? h("small", {}, f.hint) : null);
  });
  const err = h("p", { class: "form-err" });
  const submit = h("button", { class: opts.danger ? "deny" : "primary" }, label);
  const run = async () => {
    const v: Values = {};
    for (const f of fields) {
      const i = inputs.get(f.key)!;
      if (f.type === "check") v[f.key] = (i as HTMLInputElement).checked;
      else if (i.value.trim() !== "" || f.type === "select") v[f.key] = i.value.trim();
      else if ("required" in f && f.required) { err.textContent = `${f.label} is needed.`; i.focus(); return; }
    }
    err.textContent = "";
    submit.disabled = true;
    try {
      await go(v);
      s.close();
    } catch (e) {
      err.textContent = errText(e);
      submit.disabled = false;
    }
  };
  submit.addEventListener("click", () => void run());
  const s = sheet(title, [opts.intro ? h("p", { class: "intro" }, opts.intro) : null, ...rows, err], [h("button", { onclick: () => s.close() }, "Cancel"), submit]);
  // Enter in a single-line field submits.
  s.el.addEventListener("keydown", (e) => {
    const t = e.target as HTMLElement;
    if (e.key === "Enter" && t instanceof HTMLInputElement && t.type !== "checkbox" && !e.isComposing) { e.preventDefault(); void run(); }
  });
  requestAnimationFrame(() => (s.el.querySelector("input:not([type=checkbox]), textarea") as HTMLElement | null)?.focus());
  return s;
}

// ── small parts ─────────────────────────────────────────────────────────────────────────────

export const badge = (text: string, kind = ""): HTMLElement => h("span", { class: `badge ${kind}` }, text);

/// A titled block on a detail page.
export function section(title: string, ...kids: Child[]): HTMLElement {
  return h("section", { class: "block-sec" }, h("h3", {}, title), ...kids);
}

/// A row of buttons.
export const bar = (...kids: Child[]): HTMLElement => h("div", { class: "bar" }, ...kids);

/// `label: value` lines.
export function facts(rows: [string, Child][]): HTMLElement {
  return h("dl", { class: "facts" }, ...rows.filter(([, v]) => v != null && v !== false && v !== "").flatMap(([k, v]) => [h("dt", {}, k), h("dd", {}, v as Child)]));
}

/// A tappable row: a title, a line under it, something on the right.
export function row(title: Child, sub?: Child, right?: Child, opts: { href?: string; onclick?: () => void; cls?: string } = {}): HTMLElement {
  const kids = [h("div", { class: "main" }, h("div", { class: "name" }, title), sub ? h("div", { class: "sub" }, sub) : null), right ? h("div", { class: "right" }, right) : null];
  if (opts.href) return h("a", { class: `row ${opts.cls ?? ""}`, href: opts.href }, ...kids);
  return h("div", { class: `row ${opts.cls ?? ""} ${opts.onclick ? "tap" : ""}`, onclick: opts.onclick }, ...kids);
}

/// Save text as a file on this device.
export function download(name: string, text: string, type = "text/plain"): void {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = h("a", { href: url, download: name });
  document.body.append(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10_000);
}

/// Go to a route.
export const go = (hash: string): void => { location.hash = hash; };
export const enc = encodeURIComponent;
