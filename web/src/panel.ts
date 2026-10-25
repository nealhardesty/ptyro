// The settings panel (ROADMAP CFG1.3): every configuration option, by section or by search,
// with the value in effect, its default and where it comes from, edited in place by what kind of
// value it is. It renders the schema `ptyro` describes its settings with (`settings.schema`),
// reads their values (`settings.view`) and changes them one at a time (`settings.apply`), which
// checks each against the same parser the daemon loads with and keeps the file's comments. Needs
// a device paired with full scope, like every call that changes the machine.
//
// The page is built once per route and updated in place, so typing in a field survives the
// app's re-renders (a session event, the clock).

import { conn, host } from "./conn.js";
import { APPLY, ORIGIN, article, fieldsOf, fromLines, hexOf, keyLabel, labelOf, needed, relevant, score, summary, toLines } from "./cfgkit.js";
import type { Collection, Entry, Schema, Section, Setting, View } from "./cfgkit.js";
import { data } from "./data.js";
import { actions, badge, bar, confirm, enc, errText, form, go, h, row, sheet, ui, views } from "./kit.js";
import type { Child } from "./kit.js";
import { cfgChanged, editConfigAt } from "./settings.js";

const ACTIONS = {
  "settings": "Settings: every option in sections, with what it does, its default and where its value comes from",
  "settings.set": "Change a setting: it is checked first and applies at once, or says when it will",
  "settings.reset": "Put a setting back to its default (remove it from the file)",
  "settings.undo": "Undo the last settings change, putting the file back as it was",
  "settings.search": "Find a setting by its name, what it does or its value",
  "settings.add": "Add an agent, theme, editor, key binding, notification service, run script or prompt",
  "settings.remove": "Remove one of those, and everything set in it",
  "settings.rename": "Give one of those another name",
  "settings.keys": "Press the keys you want instead of typing their names",
  "settings.project": "Choose the repository whose own settings (.ptyro.toml) you edit here",
  "settings.target": "Save a repository's settings to your own file or the committed, shared one",
  "settings.restart": "Restart the daemon for the settings that need it",
  "settings.file": "Edit config.toml itself, at the setting",
} as const;
const tip = actions(ACTIONS);

type Scope = "user" | "project";
interface Undo { scope: Scope; shared: boolean; project: string; text: string; after: string; label: string }
interface Schemas { user: Schema; project: Schema }

const st = {
  schema: null as Schemas | null,
  user: null as View | null,
  project: null as View | null,
  /// The repository whose settings show (a project id), "" for none.
  projectId: ((): string => { try { return localStorage.getItem("ptyro.cfg.project") ?? ""; } catch { return ""; } })(),
  shared: false,
  query: "",
  modified: false,
  advanced: ((): boolean => { try { return localStorage.getItem("ptyro.cfg.advanced") === "1"; } catch { return false; } })(),
  undo: [] as Undo[],
  restart: new Set<string>(),
  error: "",
  loading: false,
  focusKey: "",
  /// Answers older than the newest request for the same file are not shown.
  gen: { user: 0, project: 0 } as Record<Scope, number>,
  undoing: false,
};

const remember = (k: string, v: string): void => { try { localStorage.setItem(k, v); } catch { /* private window */ } };

const sectionsOf = (): Section[] => (st.schema ? [...st.schema.user.sections, ...st.schema.project.sections] : []);
const scopeOf = (sectionId: string): Scope => sectionsOf().find((s) => s.id === sectionId)?.scope ?? "user";
const viewOf = (scope: Scope): View | null => (scope === "user" ? st.user : st.project);
const settingOf = (e: Entry): Setting | undefined => {
  const s = st.schema; if (!s) return undefined;
  return s.user.settings.find((x) => x.key === e.setting) ?? s.project.settings.find((x) => x.key === e.setting);
};
const collectionsOf = (): Collection[] => (st.schema ? [...st.schema.user.collections, ...st.schema.project.collections] : []);
const projectName = (): string => data.projects.get(st.projectId)?.slug ?? "";

// ── loading and changing ────────────────────────────────────────────────────────────────────

const projectArgs = (scope: Scope, shared = st.shared, project = st.projectId): Record<string, unknown> => (scope === "project" ? { scope, project, shared } : { scope });

async function load(): Promise<void> {
  if (conn.scope !== "full") return;
  st.loading = true;
  const project = st.projectId;
  const gu = ++st.gen.user, gp = ++st.gen.project;
  try {
    if (!st.schema) st.schema = await host("settings.schema");
    const user = await host("settings.view", projectArgs("user"));
    if (gu === st.gen.user) st.user = user;
    if (project) {
      const view = await host("settings.view", projectArgs("project", st.shared, project)).catch((e) => {
        // A repository that is gone: forget it instead of failing for ever.
        if (/no such|not found|isn't registered/i.test(errText(e))) { st.projectId = ""; remember("ptyro.cfg.project", ""); } else st.error = errText(e);
        return null;
      });
      if (gp === st.gen.project && project === st.projectId) st.project = view;
    } else st.project = null;
    st.error = st.error && st.project ? "" : st.error;
  } catch (e) {
    st.error = errText(e);
  }
  st.loading = false;
  page?.paint();
}

/// Apply one change. Resolves whether it was accepted; a refusal shows under `at`.
async function change(scope: Scope, op: Record<string, unknown>, label: string, opts: { shared?: boolean; at?: (msg: string) => void; quiet?: boolean } = {}): Promise<boolean> {
  const shared = opts.shared ?? st.shared;
  const project = st.projectId;
  const gen = ++st.gen[scope];
  try {
    const r = await host("settings.apply", { ...projectArgs(scope, shared, project), change: op });
    // The newest answer for a file wins; a repository switched away from meanwhile isn't shown.
    if (gen === st.gen[scope] && (scope === "user" || project === st.projectId)) {
      if (scope === "user") st.user = r.view; else st.project = r.view;
    }
    opts.at?.("");
    if (r.changed) {
      st.undo.push({ scope, shared, project, text: r.before, after: r.after ?? "", label });
      if (st.undo.length > 40) st.undo.shift();
      // The raw file editor reads the file again next time.
      cfgChanged();
      const key = String(op.key ?? op.path ?? "");
      if (r.apply === "restart") st.restart.add(key);
    }
    if (!opts.quiet) page?.snack(r.changed ? `${label} — ${(r.notes as string[]).join(" · ")}` : (r.notes as string[]).join(" · "), r.changed);
    return true;
  } catch (e) {
    const msg = errText(e);
    if (opts.at) opts.at(msg); else page?.snack(msg, false, true);
    return false;
  }
}

/// A change to something a repository's two files could hold (a prompt, a run script): in the
/// file being saved to, and when that doesn't have it, in the other one.
async function changeHeld(scope: Scope, op: Record<string, unknown>, label: string, opts: { at?: (msg: string) => void } = {}): Promise<boolean> {
  if (scope === "user") return change(scope, op, label, opts);
  let first = "";
  if (await change(scope, op, label, { at: (m) => { first = m; } })) { opts.at?.(""); return true; }
  if (/isn't set|no .* called|there is no/i.test(first) && await change(scope, op, label, { shared: !st.shared, at: () => {} })) { opts.at?.(""); return true; }
  opts.at?.(first);
  return false;
}

/// Whether a value would be accepted, without writing it (typing in a box shows the reason).
async function would(scope: Scope, op: Record<string, unknown>): Promise<string> {
  try { await host("settings.apply", { ...projectArgs(scope), change: op, dry: true }); return ""; } catch (e) { return errText(e); }
}

async function undo(): Promise<void> {
  if (st.undoing) return;
  const u = st.undo[st.undo.length - 1];
  if (!u) { page?.snack("Nothing to undo", false); return; }
  st.undoing = true;
  try {
    // Only over the file as that change left it: anything done since is not lost.
    const r = await host("settings.restore", { ...projectArgs(u.scope, u.shared, u.project), text: u.text, expect: u.after || undefined });
    st.undo.pop();
    st.gen[u.scope]++;
    if (u.scope === "user") st.user = r.view; else if (u.project === st.projectId) st.project = r.view;
    cfgChanged();
    page?.snack(`Undone: ${u.label}`, false);
    page?.paintBody();
  } catch (e) { page?.snack(`Can't undo: ${errText(e)}`, false, true); } finally { st.undoing = false; }
}

// ── one setting ─────────────────────────────────────────────────────────────────────────────

interface Ctl { el: HTMLElement; focus?: HTMLElement }

/// The layer a value is reset in: the file that has it.
const resetShared = (e: Entry): boolean => e.origin === "project";

/// A route part, decoded; a malformed one stays as it is instead of throwing in render.
const safeDecode = (s: string): string => { try { return decodeURIComponent(s); } catch { return s; } };

let cardIds = 0;
function card(e: Entry, s: Setting, scope: Scope, view: View): HTMLElement {
  const id = `cfg-${++cardIds}`;
  const err = h("p", { class: "form-err", role: "alert" });
  const showErr = (m: string): void => { err.textContent = m; };
  const shownText = (text: string): string => (s.kind.t === "secret" ? "••••" : text.length > 40 ? `${text.slice(0, 40)}…` : text);
  const set = (text: string): Promise<boolean> => change(scope, { t: "set", key: e.key, value: text }, `${e.key} = ${shownText(text)}`, { at: showErr });
  const reset = (): Promise<boolean> => change(scope, { t: "unset", key: e.key }, `${e.key} reset`, { shared: resetShared(e), at: showErr });
  const refresh = (): void => page?.repaint(e.key);
  const ctl = control(e, s, scope, view, id, async (text) => { if (await set(text)) refresh(); }, async () => { if (await reset()) refresh(); }, showErr);
  const from = `${ORIGIN[e.origin]}${e.line ? `, line ${e.line}` : ""}`;
  const head = h("div", { class: "set-head" },
    h("label", { for: id }, labelOf(e.key, !!e.member)),
    e.changed ? badge("changed", "info") : null,
    s.apply !== "live" ? badge(APPLY[s.apply], "warn") : null);
  return h("div", { class: `set${e.changed ? " changed" : ""}`, "data-key": e.key },
    head,
    h("p", { class: "set-help" }, s.help),
    ctl.el,
    err,
    h("div", { class: "set-foot" },
      h("span", {}, `default ${e.default_shown} · from ${from}`),
      e.origin !== "default" ? h("button", { class: "small", title: tip("settings.reset"), onclick: async () => { if (await reset()) refresh(); } }, "Reset") : null,
      scope === "user" && e.line && e.origin !== "default" ? h("button", { class: "small", title: tip("settings.file"), onclick: () => editConfigAt(e.line!) }, "In the file") : null));
}

function control(e: Entry, s: Setting, scope: Scope, view: View, id: string, commit: (text: string) => Promise<void>, reset: () => Promise<void>, showErr: (m: string) => void): Ctl {
  const k = s.kind;
  const sel = (options: [string, string][], current: string, extra?: [string, string]): HTMLSelectElement => {
    const el = h("select", { id, title: tip("settings.set") }, ...(extra ? [h("option", { value: extra[0] }, extra[1])] : []), ...options.map(([v, l]) => h("option", { value: v }, l)));
    el.value = options.some(([v]) => v === current) || extra?.[0] === current ? current : (extra ? extra[0] : options[0]?.[0] ?? "");
    return el;
  };
  // Typed values: checked as you type, saved when you leave the box or press Enter.
  const typed = (el: HTMLInputElement | HTMLTextAreaElement, toText: (raw: string) => string = (r) => r): void => {
    let timer = 0;
    el.addEventListener("input", () => {
      clearTimeout(timer);
      // (A token isn't sent on every pause: it is checked when it is saved.)
      if (s.kind.t === "secret") return;
      timer = window.setTimeout(async () => showErr(await would(scope, { t: "set", key: e.key, value: toText(el.value) })), 250);
    });
    el.addEventListener("change", () => { if (el.value.trim() === "" && !(el instanceof HTMLTextAreaElement)) { if (e.origin !== "default") void reset(); return; } void commit(toText(el.value)); });
    if (el instanceof HTMLInputElement) el.addEventListener("keydown", (ev) => { if (ev.key === "Enter" && !ev.isComposing) el.blur(); });
  };
  const cur = typeof e.value === "string" ? e.value : "";
  switch (k.t) {
    case "bool": {
      if (e.default === null) {
        const el = sel([["on", "on"], ["off", "off"]], e.value === true ? "on" : e.value === false ? "off" : "", ["", `default (${s.otherwise || "off"})`]);
        el.addEventListener("change", () => void (el.value === "" ? reset() : commit(el.value)));
        return { el };
      }
      const box = h("input", { type: "checkbox", id, role: "switch", title: tip("settings.set") });
      box.checked = e.value === true;
      box.addEventListener("change", () => void commit(box.checked ? "on" : "off"));
      return { el: h("label", { class: "switch" }, box, h("span", { class: "track" }), h("span", { class: "state" }, e.value === true ? "on" : "off")), focus: box };
    }
    case "int": {
      const el = h("input", { type: "number", id, min: k.min, max: k.max, step: 1, inputmode: "numeric", title: tip("settings.set") });
      el.value = e.value == null ? "" : String(e.value);
      typed(el);
      return { el: h("div", { class: "ctl" }, el, h("small", {}, `${k.min} to ${k.max}`)), focus: el };
    }
    case "choice": {
      const el = sel(k.choices.map((c) => [c.value, c.value]), cur);
      const help = h("small", {}, k.choices.find((c) => c.value === cur)?.help ?? "");
      el.addEventListener("change", () => { help.textContent = k.choices.find((c) => c.value === el.value)?.help ?? ""; void commit(el.value); });
      return { el: h("div", { class: "ctl" }, el, help), focus: el };
    }
    case "dynamic": {
      const opts = view.options.find((o) => o.source === k.source)?.options ?? [];
      const all = opts.some(([v]) => v === cur) || !cur ? opts : [[cur, "in use"] as [string, string], ...opts];
      const el = sel(all.map(([v, d]) => [v, d ? `${v} — ${d}` : v]), cur, e.default === null ? ["", `none${s.otherwise ? ` (${s.otherwise})` : ""}`] : undefined);
      el.addEventListener("change", () => void (el.value === "" ? reset() : commit(el.value)));
      return { el, focus: el };
    }
    case "set": {
      const on = new Set(Array.isArray(e.value) ? (e.value as string[]) : []);
      const boxes = k.choices.map((c) => {
        const b = h("input", { type: "checkbox", value: c.value });
        b.checked = on.has(c.value);
        b.addEventListener("change", () => { void commit(JSON.stringify(boxes.filter((x) => x.checked).map((x) => x.value))); });
        return b;
      });
      return { el: h("div", { class: "checks", id }, ...k.choices.map((c, i) => h("label", { title: c.help }, boxes[i], h("span", {}, c.value)))) };
    }
    case "list": {
      const el = h("textarea", { id, rows: Math.max(2, Math.min(8, (Array.isArray(e.value) ? e.value.length : 0) + 1)), spellcheck: "false", autocapitalize: "off", placeholder: "one per line", title: tip("settings.set") });
      el.value = toLines(e.value);
      typed(el, fromLines);
      return { el, focus: el };
    }
    case "script": {
      const el = h("textarea", { id, rows: Math.max(2, Math.min(10, cur.split("\n").length + 1)), spellcheck: "false", autocapitalize: "off", class: "code", title: tip("settings.set") });
      el.value = cur;
      typed(el);
      return { el, focus: el };
    }
    case "keys": {
      const el = h("input", { type: "text", id, spellcheck: "false", autocapitalize: "off", title: tip("settings.set") });
      el.value = Array.isArray(e.value) ? (e.value as string[]).join(", ") : cur;
      typed(el);
      const pressed = h("button", { class: "small", title: tip("settings.keys"), onclick: () => captureKeys((labels) => { el.value = labels.join(", "); el.dispatchEvent(new Event("input")); el.dispatchEvent(new Event("change")); }) }, "Press keys…");
      return { el: h("div", { class: "ctl row-ctl" }, el, pressed), focus: el };
    }
    case "color": {
      const el = h("input", { type: "text", id, spellcheck: "false", autocapitalize: "off", placeholder: "#rrggbb, 0–255 or a name", title: tip("settings.set") });
      el.value = cur;
      typed(el);
      const pick = h("input", { type: "color", class: "swatch", "aria-label": "Pick a colour", value: hexOf(cur) ?? "#808080" });
      pick.addEventListener("change", () => void commit(pick.value));
      return { el: h("div", { class: "ctl row-ctl" }, el, pick), focus: el };
    }
    case "opaque":
      return { el: h("div", { class: "ctl" }, h("button", { class: "small", title: tip("settings.file"), onclick: () => editConfigAt(e.line ?? 0) }, "Edit in the file"), h("small", {}, typeof e.value === "object" && e.value ? `${Object.keys(e.value as object).length} set` : "none")) };
    case "secret": {
      const set = e.shown !== "—" && e.shown !== "";
      const el = h("input", { type: "password", id, autocomplete: "off", spellcheck: "false", placeholder: set ? `${e.shown} (type to replace)` : "the token, or !program args to run one", title: tip("settings.set") });
      typed(el);
      return { el, focus: el };
    }
    default: {
      // text, path, address, duration, bytes, window, argv
      const hints: Record<string, string> = { duration: "30s, 10m, 2h, 1d or off", bytes: "16MiB, 512KiB, 2GB", window: "22:00-07:00", address: "host:port", argv: "a program and its arguments", path: "a path; ~ is your home" };
      const el = h("input", { type: "text", id, spellcheck: "false", autocapitalize: "off", autocomplete: "off", placeholder: hints[k.t] ?? s.example ?? "", title: tip("settings.set") });
      el.value = e.value == null || e.value === "" ? "" : (e.shown.startsWith("(") ? "" : e.shown);
      typed(el);
      return { el: h("div", { class: "ctl" }, el, hints[k.t] ? h("small", {}, hints[k.t]) : null), focus: el };
    }
  }
}

/// Press keys: the next key presses become the answer, spelled as the config spells them.
function captureKeys(done: (labels: string[]) => void): void {
  const labels: string[] = [];
  const shown = h("div", { class: "captured" }, "…");
  const s = sheet("Press the keys you want", [h("p", { class: "intro" }, "Each key you press is added. Backspace takes the last back."), shown], [
    h("button", { onclick: () => s.close() }, "Cancel"),
    h("button", { class: "primary", onclick: () => { s.close(); if (labels.length) done(labels); } }, "Done"),
  ]);
  const on = (ev: KeyboardEvent): void => {
    // Escape, and Tab or Enter on their own, are the dialog's: how a keyboard gets to its buttons.
    if (ev.key === "Escape" || ((ev.key === "Tab" || ev.key === "Enter") && !ev.ctrlKey && !ev.altKey && !ev.metaKey)) return;
    ev.preventDefault();
    ev.stopPropagation();
    if (ev.key === "Backspace" && !ev.ctrlKey && !ev.altKey) labels.pop();
    else { const l = keyLabel(ev); if (l && !labels.includes(l)) labels.push(l); }
    shown.textContent = labels.length ? labels.join("   ") : "…";
  };
  document.addEventListener("keydown", on, true);
  const watch = new MutationObserver(() => { if (!s.el.isConnected) { document.removeEventListener("keydown", on, true); watch.disconnect(); } });
  watch.observe(document.body, { childList: true });
}

// ── collections ─────────────────────────────────────────────────────────────────────────────

function actionOptions(view: View): HTMLElement {
  const dl = h("datalist", { id: "cfg-actions" });
  for (const [v, d] of view.options.find((o) => o.source === "actions")?.options ?? []) dl.append(h("option", { value: v }, d));
  return dl;
}

function mapRows(c: Collection, view: View, scope: Scope): HTMLElement {
  const items = view.maps.find((m) => m.path === c.path)?.items ?? [];
  const isKeys = c.path === "keys";
  const rows = items.map(([name, value]) => {
    const err = h("p", { class: "form-err" });
    const input = isKeys
      ? h("input", { type: "text", list: "cfg-actions", spellcheck: "false", autocapitalize: "off", "aria-label": `what ${name} runs` })
      : h("textarea", { rows: 2, class: "code", "aria-label": `${name}'s text` });
    input.value = value;
    input.addEventListener("change", async () => {
      // (What was typed is on screen already: nothing is rebuilt under the cursor.)
      await changeHeld(scope, { t: "set", key: `${c.path}.${name}`, value: input.value }, `${name} → ${input.value.length > 40 ? `${input.value.slice(0, 40)}…` : input.value}`, { at: (m) => (err.textContent = m) });
    });
    return h("div", { class: "set map" },
      h("div", { class: "set-head" }, h("code", {}, name), h("span", { class: "grow" }),
        h("button", { class: "small deny", title: tip("settings.remove"), onclick: async () => { if (await changeHeld(scope, { t: "unset", key: `${c.path}.${name}` }, `${name} removed`)) page?.paintBody(); } }, isKeys ? "Unbind" : "Remove")),
      input, err);
  });
  return h("div", { class: "stack-in" }, isKeys ? actionOptions(view) : null, ...(rows.length ? rows : [h("p", { class: "empty" }, `No ${c.noun}s yet.`)]));
}

function memberRows(c: Collection, view: View, scope: Scope): HTMLElement {
  const names = view.members.find((m) => m.path === c.path)?.names ?? [];
  const rows = names.map((n) => {
    const fields = fieldsOf(view, c.path, n);
    const changed = fields.filter((f) => f.changed).length;
    return row(h("span", {}, c.layout === "array" ? `${n}` : n, changed ? badge(`${changed} changed`, "info") : null), summary(c.path, n, fields), "›", { href: `#/settings/${c.path}/${enc(n)}` });
  });
  return h("div", { class: "rows" }, ...(rows.length ? rows : [h("p", { class: "empty" }, `No ${c.noun}s yet.`)]), h("button", { class: "add-row", title: tip("settings.add"), onclick: () => addMember(c, scope) }, `＋ Add ${article(c.noun)} ${c.noun}`));
}

/// The sheet that makes a member: its name and what it needs, so it's valid the moment it exists.
function addMember(c: Collection, scope: Scope): void {
  if (c.path === "profiles") { go("#/profiles"); return; }
  const view = viewOf(scope);
  if (!view) return;
  // Resolves when it was added; a refusal is thrown, so the sheet that asked shows why.
  const finish = async (name: string, fields: Record<string, string>): Promise<void> => {
    let why = "";
    if (!(await change(scope, { t: "add", path: c.path, name, fields }, `added ${c.noun} ${name}`.trim(), { at: (m) => { why = m; } }))) throw new Error(why);
    if (c.layout === "tables") go(`#/settings/${c.path}/${enc(name)}`); else page?.paintBody();
  };
  if (c.path === "keys") { bindingSheet(view, finish); return; }
  if (c.layout === "array") {
    const types = (st.schema?.user.settings.find((x) => x.key === "notifications.sinks.*.type")?.kind as { choices: { value: string; help: string }[] } | undefined)?.choices ?? [];
    sinkSheet(types, finish);
    return;
  }
  if (typeof c.layout === "object") {
    form(`New ${c.noun}`, [{ key: "name", label: "Name", required: true }, { key: "value", label: "Text", type: "textarea", required: true }], "Add", async (v) => finish(String(v.name), { value: String(v.value) }));
    return;
  }
  const extra = needed(c.path, "");
  const fields = [{ key: "name", label: "Name", required: true, hint: "letters, digits, - and _" }, ...extra.map((f) => ({ key: f, label: f, required: true })), ...(c.path === "agents" ? [{ key: "command", label: "Command", hint: "optional: the program and its arguments; default, its own name" }] : [])];
  form(`New ${c.noun}`, fields, "Add", async (v) => {
    const { name, ...rest } = v;
    await finish(String(name), Object.fromEntries(Object.entries(rest).filter(([, x]) => typeof x === "string" && x !== "")) as Record<string, string>);
  });
}

function bindingSheet(view: View, finish: (name: string, fields: Record<string, string>) => Promise<void>): void {
  let busy = false;
  const key = h("input", { type: "text", spellcheck: "false", autocapitalize: "off", placeholder: "x, C-a, global:M-1", "aria-label": "Key" });
  const act = h("input", { type: "text", list: "cfg-actions", spellcheck: "false", autocapitalize: "off", placeholder: "session.next", "aria-label": "Runs" });
  const err = h("p", { class: "form-err" });
  const s = sheet("New key binding", [
    actionOptions(view),
    h("p", { class: "intro" }, "The key you press after the prefix. Put global: in front for one that works without it."),
    h("label", { class: "field" }, h("span", {}, "Key"), h("div", { class: "ctl row-ctl" }, key, h("button", { class: "small", onclick: () => captureKeys((l) => { key.value = l[0] ?? ""; }) }, "Press a key…"))),
    h("label", { class: "field" }, h("span", {}, "Runs"), act),
    err,
  ], [h("button", { onclick: () => s.close() }, "Cancel"), h("button", { class: "primary", onclick: async () => {
    if (busy) return;
    busy = true;
    try { await finish(key.value.trim(), { value: act.value.trim() }); s.close(); } catch (e) { err.textContent = errText(e); } finally { busy = false; }
  } }, "Add")]);
}

function sinkSheet(types: { value: string; help: string }[], finish: (name: string, fields: Record<string, string>) => Promise<void>): void {
  let busy = false;
  const type = h("select", { "aria-label": "Where" }, ...types.map((t) => h("option", { value: t.value }, `${t.value} — ${t.help}`)));
  const fieldsBox = h("div", { class: "stack-in" });
  const inputs = new Map<string, HTMLInputElement>();
  const err = h("p", { class: "form-err" });
  const redraw = (): void => {
    inputs.clear();
    fieldsBox.replaceChildren(...needed("notifications.sinks", type.value).map((f) => {
      const secret = ["token", "user", "webhook"].includes(f);
      const i = h("input", { type: secret ? "password" : "text", autocomplete: "off", spellcheck: "false", autocapitalize: "off", placeholder: secret ? "the value, or !program args to run one" : f === "url" ? "https://ntfy.sh/my-topic" : "" });
      inputs.set(f, i);
      return h("label", { class: "field" }, h("span", {}, f.replace("_", " ")), i);
    }));
  };
  type.addEventListener("change", redraw);
  redraw();
  const s = sheet("New notification service", [h("label", { class: "field" }, h("span", {}, "Where"), type), fieldsBox, err], [
    h("button", { onclick: () => s.close() }, "Cancel"),
    h("button", { class: "primary", onclick: async () => {
      const f: Record<string, string> = { type: type.value };
      for (const [k, i] of inputs) if (i.value.trim()) f[k] = i.value.trim();
      if (busy) return;
      busy = true;
      try { await finish("", f); s.close(); } catch (e) { err.textContent = errText(e); } finally { busy = false; }
    } }, "Add"),
  ]);
}

// ── the page ────────────────────────────────────────────────────────────────────────────────

interface Page { rest: string; el: HTMLElement; paint: () => void; paintBody: () => void; repaint: (key: string) => void; snack: (text: string, undoable: boolean, err?: boolean) => void }
let page: Page | null = null;

function build(rest: string): Page {
  const [first, ...more] = rest.split("/");
  const member = more.length ? safeDecode(more.join("/")) : "";
  const body = h("div", { class: "cfg-body" });
  const nav = h("nav", { class: "cfg-nav", "aria-label": "Sections" });
  const banners = h("div", { class: "cfg-banners" });
  const snackEl = h("div", { class: "snack", role: "status", hidden: true });
  let snackTimer = 0;
  const search = h("input", { class: "search", type: "search", placeholder: "Search every setting…", "aria-label": "Search settings", title: tip("settings.search"), autocomplete: "off", spellcheck: "false" });
  search.value = st.query;
  search.addEventListener("input", () => { st.query = search.value; paintBody(); });
  search.addEventListener("keydown", (e) => { if (e.key === "Escape") { search.value = ""; st.query = ""; paintBody(); } });

  const toggles = (): HTMLElement => h("div", { class: "chips cfg-chips" },
    h("button", { class: st.modified ? "on" : "", title: "Only the settings that differ from their default", onclick: () => { st.modified = !st.modified; paintBody(); paintToggles(); } }, "Changed"),
    h("button", { class: st.advanced ? "on" : "", title: "Settings most people never touch", onclick: () => { st.advanced = !st.advanced; remember("ptyro.cfg.advanced", st.advanced ? "1" : "0"); paintBody(); paintToggles(); } }, "Advanced"),
    st.undo.length ? h("button", { title: tip("settings.undo"), onclick: () => void undo() }, "Undo") : null);
  const togglesBox = h("div", {});
  const paintToggles = (): void => togglesBox.replaceChildren(toggles());

  function snack(text: string, undoable: boolean, err = false): void {
    snackEl.hidden = false;
    snackEl.className = `snack${err ? " bad" : ""}`;
    snackEl.replaceChildren(h("span", {}, text));
    if (undoable) snackEl.append(h("button", { class: "small", title: tip("settings.undo"), onclick: () => { snackEl.hidden = true; void undo(); } }, "Undo"));
    clearTimeout(snackTimer);
    snackTimer = window.setTimeout(() => { snackEl.hidden = true; }, err ? 9000 : 7000);
    paintToggles();
    paintBanners();
  }

  function paintBanners(): void {
    const kids: Child[] = [];
    if (conn.scope !== "full") kids.push(h("p", { class: "empty" }, "Settings need a device paired with ", h("code", {}, "ptyro pair --scope full"), "."));
    if (st.user?.problem) kids.push(h("div", { class: "banner bad" }, h("b", {}, "Your config doesn't load, so defaults show: "), st.user.problem, " ", h("button", { class: "small", onclick: () => editConfigAt(0) }, "Fix it in the file")));
    if (st.project?.problem) kids.push(h("div", { class: "banner bad" }, h("b", {}, `${projectName()}'s config doesn't load: `), st.project.problem));
    if (st.error) kids.push(h("div", { class: "banner bad" }, st.error, " ", h("button", { class: "small", onclick: () => { st.error = ""; void load(); } }, "Retry")));
    if (st.restart.size) kids.push(h("div", { class: "banner warn" }, `⟳ ${[...st.restart].join(", ")} needs the daemon restarted. `, h("button", { class: "small", title: tip("settings.restart"), onclick: () => confirm("Restart the daemon?", "Sessions are stopped; agents hibernate and come back resumable. This page reconnects.", "Restart", async () => { try { await host("daemon.restart"); st.restart.clear(); paintBanners(); ui.toast("Restarting…"); } catch (e) { ui.toast(errText(e)); } }) }, "Restart now")));
    banners.replaceChildren(...(kids.filter(Boolean) as Node[]));
  }

  function projectBar(): HTMLElement {
    const sel = h("select", { "aria-label": "Repository", title: tip("settings.project") }, h("option", { value: "" }, "No repository"), ...[...data.projects.values()].sort((a, b) => a.slug.localeCompare(b.slug)).map((p) => h("option", { value: p.id }, p.slug)));
    sel.value = data.projects.has(st.projectId) ? st.projectId : "";
    sel.addEventListener("change", () => { st.projectId = sel.value; remember("ptyro.cfg.project", st.projectId); st.project = null; void load().then(() => paint()); });
    const target = h("div", { class: "seg", role: "group", "aria-label": "Save to", title: tip("settings.target") },
      h("button", { class: st.shared ? "" : "on", onclick: () => { st.shared = false; paint(); } }, "Mine"),
      h("button", { class: st.shared ? "on" : "", onclick: () => { st.shared = true; paint(); } }, "Shared"));
    return h("div", { class: "cfg-project" }, h("span", {}, "Repository"), sel, st.projectId ? target : null, st.projectId ? h("small", {}, st.shared ? ".ptyro.toml — committed, for everyone" : ".ptyro.local.toml — yours alone") : null);
  }

  function paintNav(): void {
    const s = st.schema;
    if (!s) { nav.replaceChildren(); return; }
    const count = (sec: Section): number => (viewOf(sec.scope)?.entries ?? []).filter((e) => e.setting.split(".")[0] !== "" && settingSection(e) === sec.id && e.changed).length;
    const link = (sec: Section): HTMLElement => {
      const n = count(sec);
      return h("a", { class: `cfg-link${sec.id === first ? " on" : ""}`, href: `#/settings/${sec.id}` }, h("span", {}, sec.title), n ? badge(String(n), "info") : null);
    };
    nav.replaceChildren(
      ...s.user.sections.map(link),
      h("div", { class: "cfg-sep" }, projectName() ? `Repository: ${projectName()}` : "Repository"),
      ...s.project.sections.map(link),
      h("a", { class: "cfg-link file", href: "#/config", title: tip("settings.file") }, h("span", {}, "Edit config.toml directly")));
  }

  const settingSection = (e: Entry): string => settingOf(e)?.section ?? "";

  /// The cards for entries of a view that pass the filters, in the schema's order.
  function cards(entries: Entry[], view: View, scope: Scope, opts: { all?: boolean } = {}): HTMLElement[] {
    return entries
      .map((e) => ({ e, s: settingOf(e) }))
      .filter((x): x is { e: Entry; s: Setting } => !!x.s)
      .filter(({ e, s }) => relevant(e, s, view.entries) && (opts.all || ((!st.modified || e.changed) && (st.advanced || !s.advanced || e.changed))))
      .map(({ e, s }) => card(e, s, scope, view));
  }

  function homeBody(): Child[] {
    const s = st.schema;
    if (!s) return [h("p", { class: "empty" }, st.loading ? "Loading…" : "")];
    const rows = [...s.user.sections, ...s.project.sections].map((sec) => {
      const view = viewOf(sec.scope);
      const n = (view?.entries ?? []).filter((e) => settingSection(e) === sec.id && e.changed).length;
      const off = sec.scope === "project" && !st.projectId;
      return row(h("span", {}, sec.title, n ? badge(`${n} changed`, "info") : null), off ? "Choose a repository above" : sec.blurb, "›", { href: `#/settings/${sec.id}`, cls: off ? "dim" : "" });
    });
    return [h("div", { class: "rows" }, ...rows)];
  }

  function sectionBody(sec: Section): Child[] {
    const scope = sec.scope;
    const view = viewOf(scope);
    if (scope === "project" && !st.projectId) return [h("p", { class: "empty" }, "These are a repository's own settings (", h("code", {}, ".ptyro.toml"), "). Choose one above.")];
    if (!view) return [h("p", { class: "empty" }, st.loading ? "Loading…" : "Nothing loaded.")];
    const out: Child[] = [h("p", { class: "intro" }, sec.blurb)];
    out.push(...cards(view.entries.filter((e) => !e.member && settingOf(e)?.section === sec.id), view, scope));
    for (const c of collectionsOf().filter((x) => x.section === sec.id)) {
      out.push(h("h3", { class: "cfg-h" }, c.path === "keys" ? "Your key bindings" : c.path === "notifications.sinks" ? "Notification services" : c.path === "scripts.run" ? "Run scripts" : c.path[0].toUpperCase() + c.path.slice(1)));
      out.push(typeof c.layout === "object" ? mapRows(c, view, scope) : memberRows(c, view, scope));
      if (typeof c.layout === "object") out.push(h("button", { class: "add-row", title: tip("settings.add"), onclick: () => addMember(c, scope) }, `＋ Add ${article(c.noun)} ${c.noun}`));
    }
    return out;
  }

  function memberBody(path: string, name: string): Child[] {
    const c = collectionsOf().find((x) => x.path === path);
    if (!c) return [h("p", { class: "empty" }, "No such group.")];
    const scope = scopeOf(c.section);
    const view = viewOf(scope);
    if (!view) return [h("p", { class: "empty" }, st.loading ? "Loading…" : "Nothing loaded.")];
    const fields = fieldsOf(view, path, name);
    if (!fields.length) return [h("p", { class: "empty" }, `There is no ${c.noun} ${name}.`)];
    const actionsBar = c.path === "profiles"
      ? bar(h("a", { class: "button", href: `#/profile/${enc(name)}` }, "Rename or remove (profile page)"))
      : bar(
        c.layout !== "array" ? h("button", { title: tip("settings.rename"), onclick: () => form(`Rename ${name}`, [{ key: "to", label: "New name", value: name, required: true }], "Rename", async (v) => {
          let why = "";
          if (!(await changeHeld(scope, { t: "rename", path, from: name, to: String(v.to) }, `${name} is now ${v.to}`, { at: (m) => { why = m; } }))) throw new Error(why);
          go(`#/settings/${path}/${enc(String(v.to))}`);
        }) }, "Rename") : null,
        h("button", { class: "deny", title: tip("settings.remove"), onclick: () => confirm(`Remove ${c.noun} ${name}?`, "Everything set in it goes too.", "Remove", async () => { if (await changeHeld(scope, { t: "remove", path, name }, `removed ${c.noun} ${name}`)) go(`#/settings/${c.section}`); }) }, "Remove"));
    return [actionsBar, ...cards(fields, view, scope)];
  }

  function searchBody(): Child[] {
    const hits: { e: Entry; s: Setting; scope: Scope; n: number }[] = [];
    for (const scope of ["user", "project"] as Scope[]) {
      const view = viewOf(scope);
      if (!view) continue;
      for (const e of view.entries) {
        const s = settingOf(e);
        if (!s || !relevant(e, s, view.entries)) continue;
        const n = score(st.query, e.key, s.help, e.shown);
        if (n >= 0) hits.push({ e, s, scope, n });
      }
    }
    hits.sort((a, b) => b.n - a.n || a.e.key.localeCompare(b.e.key));
    if (!hits.length) return [h("p", { class: "empty" }, `No setting matches “${st.query}”.`)];
    return hits.slice(0, 40).map(({ e, s, scope }) => {
      const view = viewOf(scope)!;
      const c = card(e, s, scope, view);
      c.prepend(h("a", { class: "set-where", href: `#/settings/${e.member ? `${e.member[0]}/${enc(e.member[1])}` : s.section}` }, `${sectionsOf().find((x) => x.id === s.section)?.title ?? s.section} › ${e.key}`));
      return c;
    });
  }

  function paintBody(): void {
    const sec = sectionsOf().find((x) => x.id === first);
    let kids: Child[];
    if (st.query.trim()) kids = searchBody();
    else if (member && first) kids = memberBody(first, member);
    else if (sec) kids = sectionBody(sec);
    else kids = homeBody();
    body.replaceChildren(...(kids.filter(Boolean) as Node[]).map((k) => (typeof k === "string" ? document.createTextNode(k) : k)));
    paintNav();
    if (st.focusKey) {
      const el = body.querySelector(`[data-key="${CSS.escape(st.focusKey)}"]`);
      st.focusKey = "";
      el?.scrollIntoView({ block: "center" });
      el?.classList.add("flash");
    }
  }

  function paint(): void {
    paintBanners();
    paintToggles();
    paintBody();
    projectHost.replaceChildren(projectBar());
  }

  /// One setting again, after it changed, without touching the fields around it.
  function repaint(key: string): void {
    const old = body.querySelector(`[data-key="${CSS.escape(key)}"]`);
    if (!old) { paintBody(); return; }
    const e = [st.user, st.project].flatMap((v) => v?.entries ?? []).find((x) => x.key === key);
    const s = e && settingOf(e);
    const scope: Scope = s ? scopeOf(s.section) : "user";
    const view = viewOf(scope);
    if (!e || !s || !view) { paintBody(); return; }
    // A sink's type decides which of its other fields show; a changed filter hides this one.
    // A sink's type decides which of its other fields show.
    if (e.setting.endsWith(".type") && !st.query.trim()) { paintBody(); return; }
    old.replaceWith(card(e, s, scope, view));
    paintNav();
    paintBanners();
  }

  const projectHost = h("div", {});
  const sec = sectionsOf().find((x) => x.id === first);
  const title = member ? `${member}` : sec ? sec.title : "Settings";
  const back = member ? `#/settings/${collectionsOf().find((c) => c.path === first)?.section ?? ""}` : first ? "#/settings" : "#/more";
  const el = h("div", { class: `cfg${first ? " inner" : " home"}`, title: tip("settings") },
    ui.header(title, back),
    h("main", { class: "cfg-main" }, h("div", { class: "cfg-top" }, search, togglesBox, projectHost), banners, h("div", { class: "cfg-cols" }, nav, body)),
    snackEl);
  const p: Page = { rest, el, paint, paintBody, repaint, snack };
  // Once the first paint has what it needs.
  queueMicrotask(() => { paintNav(); paint(); });
  return p;
}

/// `#/settings`, `#/settings/<section>`, `#/settings/<collection>/<member>`.
let seenScope = "";
let seenProjects = -1;
function settingsView(rest: string): HTMLElement {
  if (!page || page.rest !== rest) {
    page = build(rest);
    seenScope = conn.scope;
    seenProjects = data.projects.size;
    if (conn.scope === "full") void load();
  } else {
    // The connection said what this device may do after the page was built (a reload on this
    // route), or the repositories arrived: show what they allow.
    if (conn.scope !== seenScope) {
      seenScope = conn.scope;
      if (conn.scope === "full" && !st.user && !st.loading) void load();
      page.paint();
    }
    if (data.projects.size !== seenProjects) {
      seenProjects = data.projects.size;
      page.paint();
    }
  }
  for (const stale of page.el.querySelectorAll(":scope > .offline")) stale.remove();
  return page.el;
}

/// Open the panel on `key` (a search hit, a link from elsewhere).
export function openSetting(key: string): void {
  const all = [st.user, st.project].flatMap((v) => v?.entries ?? []);
  const e = all.find((x) => x.key === key);
  const s = e && settingOf(e);
  st.query = "";
  st.focusKey = key;
  go(`#/settings/${s ? (e.member ? `${e.member[0]}/${enc(e.member[1])}` : s.section) : ""}`);
}

views.set("settings", (rest) => settingsView(rest));
