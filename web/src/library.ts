// The Library (SPEC §25.10, ROADMAP KB1.9): what the knowledge base kept of your work, searched
// and read on one page (More → Library), as the TUI's Library does it. A search box and a row of
// filters (kind, project, since, and the machine when others are linked); the hits, or with
// nothing typed the history by day; a tap opens the reader (`#/library/k/<address>`): the piece
// with what was said around it, then all of it, with a find box, the way to the session it
// happened in, copying and forgetting. On a phone the list and the reader are two steps.
//
// Two tabs (KB2): Search, and Notes (`#/library/notes`): the notes by project, New note (every
// option the command takes), a tap edits one in the page (`#/library/note/<address>`: its file
// as it is, Save, Delete…). Sources comes with KB3.
//
// What it shows is asked of the daemon (`kb_search`, `kb_recent`, `kb_read`, `kb_status`), and
// asked again when the daemon says the knowledge base changed (`kb_updated`).

import { conn, host } from "./conn.js";
import { data, can } from "./data.js";
import { Cache, actions, ago, ask, badge, bar, confirm, enc, errText, facts, form, go, h, menu, pending, profileChoices, row, safeDecode, sheet, tipOf, ui, views } from "./kit.js";
import type { Child } from "./kit.js";
import { PAGE, SINCE, WROTE, byProject, bytes, clock, conversation, day, forgetAddr, found, kindsOffered, lit, matched, noFilters, noteRequest, notesRequest, plural, question, request, searching, snippetParts, split, when, wroteBy } from "./libkit.js";
import type { Filters, Of, Part } from "./libkit.js";

const ACTIONS = {
  "kb.open": "The Library: everything ptyro kept of your work, searchable in one place (More → Library)",
  "kb.search": "Search what was kept, and your chats: words (all must match), \"a phrase\", prefix*, -word, kind:session, project:NAME, since:7d, before:30d, agent:codex, machine:NAME, by:meaning. The filters under the box narrow it: kind, project, since, and which machine",
  "kb.recent": "The history: what was kept, newest first, by day (shown while nothing is typed); Older… brings more",
  "kb.history": "The history of this one alone: the Library with only what was kept of this session, worktree or project",
  "kb.show": "Open it in the reader: the piece that matched with what was said around it, then all of it; find in it, go to the session it happened in (resumed when it is hibernated), copy its address or its text",
  "kb.forget": "Forget it: its text is deleted for good, and nothing of it is kept again (the computer says what will go, and asks first)",
  "kb.status": "How the knowledge base stands: on or off, what it holds, its size, how long sessions are kept, what was not kept and why",
  "kb.enable": "Switch the knowledge base on: agent sessions are kept from then on",
  "kb.notes": "The notes: what you and your agents wrote down, by project. A tap opens one to edit; ⋯ deletes it. They are Markdown files in ptyro's notes folder on the computer",
  "kb.note": "Write a note (a decision, a fact, a how-to): its text, its title, the project it is about, whose agents may see it, its tags. Every search and every agent's recall finds notes first",
  "kb.sync": "Read the notes folder again now: what was written, changed or deleted there by hand is found (the computer also watches the folder)",
} as const;
const tip = actions(ACTIONS);

interface Hit { addr: string; item: string; node?: string | null; kind: string; part: string; title?: string | null; snippet: string; at_ms: number; project_name?: string | null; agent?: string | null; machine?: string | null; how: string }
interface Item { id: string; uri: string; kind: string; title?: string | null; rel?: string | null; agent?: string | null; writer?: string | null; session?: string | null; chunks: number; started_ms?: number | null; ended_ms?: number | null; ingested_ms?: number | null }
interface Listed { addr: string; item: Item; project_name?: string | null }
interface Chunk { idx: number; kind: string; heading?: string | null; text: string }
interface Doc { item: Listed; chunks: Chunk[]; at?: number | null; stale?: boolean }
interface Status { enabled: boolean; sessions: boolean; retain_days: number; error?: string | null; path: string; notes?: string; bytes: number; kinds: { kind: string; items: number; chunks: number }[]; vectors: number; meaning?: string | null; forgotten: number; queued: number; skipped: { uri: string; why: string; at_ms: number }[] }
/// What the list holds: hits, the history, or what the daemon said instead of answering.
type Rows =
  | { t: "hits"; hits: Hit[]; wordsOnly: string; leftOut: string[] }
  | { t: "items"; items: Listed[] }
  | { t: "said"; code: string; text: string };

const state = {
  /// The box and the filters; `f.query` is asked a moment after the typing stops, `typed` is
  /// what the box holds meanwhile.
  f: noFilters(), typed: "",
  /// More of the history ("Older…"), for the list `olderFor` names; `more`: there may be more yet.
  older: [] as Listed[], olderFor: "", more: true,
  /// The words that found the hit the reader shows: marked in it.
  lit: [] as string[],
  /// The reader: all of the item (not only the piece and its neighbours), what `find` looks for,
  /// which of the pieces that hold it is shown, and the piece last scrolled to.
  all: false, find: "", foundAt: 0, scrolled: "",
};

/// How the knowledge base stands: asked for once the connection is open (a request made before
/// that is refused at once), and again when it changes.
const home = new Cache<Status>(async () => (await conn.request({ t: "kb_status" })).status as Status);

/// The list, by what was asked (the request, as text). What the daemon says of a query (it is
/// still being typed, meaning search is off) is an answer; a lost connection is a failure, with
/// Retry.
const lists = new Cache<Rows>(async (key) => {
  try {
    const r = await conn.request(JSON.parse(key));
    if (r.t === "kb_items") return { t: "items", items: r.items as Listed[] };
    return { t: "hits", hits: r.hits as Hit[], wordsOnly: (r.words_only as string | undefined) ?? "", leftOut: (r.left_out as string[] | undefined) ?? [] };
  } catch (e) {
    const code = (e as { code?: string }).code ?? "";
    if (!["invalid", "needs_confirmation", "unsupported", "not_found", "permission_denied"].includes(code)) throw e;
    return { t: "said", code, text: errText(e) };
  }
});

/// An item or a piece of one, by what was asked.
const docs = new Cache<Doc>(async (key) => await conn.request(JSON.parse(key)) as Doc);

/// The notes, by what was asked; and a note's file, by its address.
const notes = new Cache<Listed[]>(async (key) => (await conn.request(JSON.parse(key))).items as Listed[]);
interface NoteFile { item: Listed; text: string; path: string }
const files = new Cache<NoteFile>(async (addr) => await conn.request({ t: "kb_note_read", addr }) as NoteFile);

const about = (...parts: (string | null | undefined)[]): string => parts.filter(Boolean).join(" · ");
const titleOf = (i: Listed): string => (i.item.title ?? i.item.rel ?? i.item.uri).replace(/\n/g, " ");

/// Text with its matches as <mark>.
function marks(parts: Part[]): Node[] {
  return parts.map((p) => (p.on ? h("mark", {}, p.text) : document.createTextNode(p.text)));
}

// ── the list ────────────────────────────────────────────────────────────────────────────────

let timer = 0;
const findBox = h("input", { class: "search", type: "search", placeholder: "Search what was kept…", "aria-label": "Search the Library", title: tip("kb.search"), autocomplete: "off", spellcheck: false });
findBox.addEventListener("input", () => {
  state.typed = findBox.value.trim();
  clearTimeout(timer);
  timer = window.setTimeout(() => { state.f.query = state.typed; ui.render(); }, 250);
});

/// What a list under `f` asks, as its cache key; `null`: chats, which have no history here.
function keyFor(f: Filters): string | null {
  // (The history's `since` is a moment: to the minute, so the same list is the same ask.)
  const req = request(f, Math.floor(Date.now() / 60_000) * 60_000);
  return req ? JSON.stringify(req) : null;
}
const listKey = (): string | null => keyFor(state.f);

/// A filter as a pick: `any` first.
function pick(label: string, value: string, any: string, options: [string, string][], set: (v: string) => void): HTMLSelectElement {
  const el = h("select", { "aria-label": label, onchange: (e: Event) => { set((e.target as HTMLSelectElement).value); ui.render(); } },
    h("option", { value: "" }, any), ...options.map(([v, text]) => h("option", { value: v }, text)));
  el.value = value;
  return el;
}

function filters(s: Status): HTMLElement {
  const f = state.f;
  const projects = [...data.projects.values()].filter((p) => !(p as { machine?: string | null }).machine).map((p): [string, string] => [p.id, p.slug]);
  const machines = [...data.machines.keys()].sort();
  return h("div", { class: "kb-filters", title: tip("kb.search") },
    pick("Kind", f.kind, "Every kind", kindsOffered(s.kinds).map((k): [string, string] => [k, k]), (v) => { f.kind = v; }),
    pick("Project", f.project, "Every project", projects, (v) => { f.project = v; }),
    pick("Since", f.since, "Any time", SINCE, (v) => { f.since = v; }),
    pick("Wrote", f.wrote, "Anyone wrote", WROTE, (v) => { f.wrote = v; }),
    machines.length || f.machine ? pick("Machine", f.machine, "Every machine", [["here", "This computer"], ...machines.map((m): [string, string] => [m, m])], (v) => { f.machine = v; }) : null,
    f.of ? h("button", { class: "small on", title: tip("kb.history"), onclick: () => { f.of = null; ui.render(); } }, `Only ${f.of.t} ${f.of.name} ✕`) : null);
}

/// The reader, at `addr`; `words`: what found it, marked there.
function open(addr: string, words: string[] = []): void {
  state.lit = words;
  [state.all, state.find, state.foundAt, state.scrolled] = [false, "", 0, ""];
  go(`#/library/k/${enc(addr)}`);
}

function hitRows(r: Extract<Rows, { t: "hits" }>, ofItems: Set<string> | null): Child[] {
  // A row's history: of what was found, only that session's or worktree's.
  const hits = ofItems ? r.hits.filter((x) => ofItems.has(x.item)).slice(0, 30) : r.hits;
  const notes = [
    r.wordsOnly ? h("p", { class: "sub" }, `(${r.wordsOnly})`) : null,
    ...r.leftOut.map((m) => h("p", { class: "sub" }, `(not searched: ${m})`)),
  ];
  if (!hits.length) return [h("p", { class: "empty" }, "Nothing matches."), ...notes];
  return [...hits.map((x) => {
    const sub = h("span", { class: "snippet" }, x.how === "meaning" ? h("span", { title: "Found by meaning: none of the words is in it" }, "≈ ") : null, ...marks(snippetParts(x.snippet)));
    const title = h("span", {}, badge(x.kind), " ", (x.title ?? x.addr).replace(/\n/g, " "), h("small", {}, ` ${about(x.machine, x.project_name, x.agent, `${ago(x.at_ms)} ago`)}`));
    // A chat's hit opens the chat at its message; anything else, the reader at its piece.
    return x.kind === "chat"
      ? row(title, sub, null, { href: `#/c/${enc(x.item)}${x.node ? `/${enc(x.node)}` : ""}`, cls: "wrap" })
      : h("div", { title: tip("kb.show") }, row(title, sub, null, { onclick: () => open(x.addr, matched(x.snippet)), cls: "wrap" }));
  }), ...notes];
}

function historyRows(items: Listed[], s: Status): Child[] {
  const f = state.f;
  if (!items.length) {
    const none = f.of ? `Nothing is kept of ${f.of.t} ${f.of.name}.`
      : f.kind || f.project || f.since || f.wrote || f.machine ? "Nothing kept matches these filters."
      : "Nothing is kept yet. Agent sessions are, as they happen: what you asked, what the agent answered and what it ran.";
    return [h("p", { class: "empty" }, none), s.sessions ? null : h("p", { class: "sub" }, "Agent sessions are not kept (Settings → Knowledge → sessions is off).")];
  }
  const out: Child[] = [];
  const now = Date.now();
  let last = "";
  for (const i of items) {
    const at = when(i.item);
    const d = day(at, now);
    if (d !== last) { out.push(h("h3", { class: "day" }, d)); last = d; }
    out.push(h("div", { title: tip("kb.show") }, row(h("span", {}, badge(i.item.kind), " ", titleOf(i)),
      about(clock(at), i.project_name, i.item.kind === "note" ? wroteBy(i.item) : i.item.agent, plural(i.item.chunks, "piece")), null, { onclick: () => open(i.addr) })));
  }
  return out;
}

/// The next page of the history, under what is shown.
async function older(key: string, shown: Listed[]): Promise<void> {
  const before = Math.min(...shown.map((i) => when(i.item)));
  const req = request(state.f, Date.now(), before);
  if (!req) return;
  const r = await ask<{ items: Listed[] }>(req);
  if (!r || listKey() !== key) return;
  const have = new Set(shown.map((i) => i.item.id));
  const fresh = r.items.filter((i) => !have.has(i.item.id));
  state.older = [...(state.olderFor === key ? state.older : []), ...fresh];
  [state.olderFor, state.more] = [key, r.items.length >= PAGE && fresh.length > 0];
  ui.render();
}

/// Meaning search on, in your config (as the settings panel would): the daemon downloads its model.
async function meaningOn(): Promise<void> {
  try {
    await host("settings.apply", { scope: "user", change: { t: "set", key: "kb.meaning", value: "true" } });
    ui.toast("Meaning search is on: its model is downloading, then what is kept is read (the status shows how far)");
    lists.reload();
  } catch (e) { ui.toast(errText(e)); }
}

function listRows(s: Status): Child[] {
  const f = state.f;
  const key = listKey();
  if (key === null) return [h("p", { class: "empty" }, "Chats are on the Chats page: type to search them here.")];
  const wait = pending(lists, key, searching(f.query) ? "Searching…" : "Loading…");
  const r = lists.get(key);
  if (wait || !r) return [wait];
  if (r.t === "said") {
    // `by:meaning` while it is off: offer to switch it on.
    if (r.code === "needs_confirmation") return [h("p", { class: "sub" }, `${r.text}. `, can("full") ? h("button", { class: "small", title: tipOf("chat.meaning"), onclick: () => void meaningOn() }, "Switch it on") : null)];
    return [h("p", { class: "sub" }, r.text)];
  }
  if (r.t === "hits") {
    // (Which items a row's history holds: the history asked for before the typing began.)
    const histKey = f.of ? keyFor({ ...f, query: "" }) : null;
    const hist = histKey ? lists.get(histKey) : undefined;
    return hitRows(r, f.of ? new Set(hist?.t === "items" ? hist.items.map((i) => i.item.id) : []) : null);
  }
  const items = [...r.items, ...(state.olderFor === key ? state.older : [])];
  const more = r.items.length >= PAGE && (state.olderFor !== key || state.more);
  return [...historyRows(items, s), more ? h("button", { class: "small", title: tip("kb.recent"), onclick: () => void older(key, items) }, "Older…") : null];
}

function showStatus(s: Status): void {
  const pieces = s.kinds.reduce((n, k) => n + k.chunks, 0);
  sheet("The knowledge base", [facts([
    ["It is", s.enabled ? "on" : "off"],
    ["Kept", s.kinds.length ? s.kinds.map((k) => `${plural(k.items, k.kind)} (${plural(k.chunks, "piece")})`).join(", ") : "nothing yet"],
    ["In", `${s.path} (${bytes(s.bytes)})`],
    ["Notes", s.notes ? `Markdown files in ${s.notes}` : null],
    ["By meaning", s.vectors ? `${s.vectors} of ${plural(pieces, "piece")}` : null],
    ["Meaning search", s.meaning ?? null],
    ["Agent sessions", s.sessions ? null : "not kept (Knowledge → sessions is off)"],
    ["Sessions are kept", s.retain_days ? `${plural(s.retain_days, "day")}, then deleted` : "for good"],
    ["Forgotten", s.forgotten ? `${s.forgotten}: never kept again` : null],
    ["Waiting to be read", s.queued ? String(s.queued) : null],
  ]),
  s.skipped.length ? h("h3", {}, `Not kept (${s.skipped.length})`) : null,
  ...[...s.skipped].reverse().map((k) => h("p", { class: "sub" }, `${k.uri}: ${k.why} (${ago(k.at_ms)} ago)`))]);
}

async function enable(): Promise<void> {
  try {
    await host("settings.apply", { scope: "user", change: { t: "set", key: "kb.enabled", value: "true" } });
    ui.toast("The knowledge base is on: agent sessions are kept from here on");
    home.reload("");
  } catch (e) { ui.toast(errText(e)); }
}

/// Switched off, or `kb.db` can't be opened: what that means and the way out. `null`: it is on.
function offBody(s: Status): Child[] | null {
  if (!s.enabled) return [h("div", { class: "empty" },
    h("p", {}, "The knowledge base is off: nothing is kept and nothing is searched.", s.bytes > 0 ? ` What was kept before stays on the computer (${bytes(s.bytes)}).` : ""),
    can("full") ? h("button", { class: "primary", title: tip("kb.enable"), onclick: () => void enable() }, "Switch it on")
      : h("p", { class: "sub" }, "Switching it on needs a device paired with ", h("code", {}, "ptyro pair --scope full"), " (Settings → Knowledge on the computer)."),
    h("p", { class: "sub" }, "Agent sessions are kept from then on, searchable here and by your agents. A private session is never kept."))];
  if (s.error) return [h("p", { class: "banner warn" }, `The knowledge base can't be read. ${s.error}`)];
  return null;
}

function listView(): HTMLElement {
  const wait = pending(home, "");
  const s = home.get("");
  let body: Child[];
  if (wait || !s) body = [wait];
  else {
    if (findBox.value.trim() !== state.typed && document.activeElement !== findBox) findBox.value = state.typed;
    const kept = s.kinds.length ? s.kinds.map((k) => plural(k.items, k.kind)).join(", ") : "nothing kept yet";
    body = offBody(s) ?? [
      tabs("search"),
      findBox,
      filters(s),
      h("p", { class: "sub" }, h("button", { class: "small", title: tip("kb.status"), onclick: () => showStatus(s) }, about(kept, bytes(s.bytes), s.queued ? `${s.queued} waiting to be read` : null))),
      h("div", { class: "rows", title: tip(searching(state.f.query) ? "kb.search" : "kb.recent") }, ...listRows(s)),
    ];
  }
  return h("div", {}, ui.header("Library", "#/more"), h("main", { class: "stack library", title: tip("kb.open") }, ...body));
}

/// The tabs, under the header.
function tabs(on: "search" | "notes"): HTMLElement {
  const tab = (id: string, label: string, hash: string, title: string): HTMLElement =>
    h("button", { class: `small${on === id ? " on" : ""}`, title, "aria-pressed": String(on === id), onclick: () => go(hash) }, label);
  return h("div", { class: "kb-tabs" }, tab("search", "Search", "#/library", tip("kb.search")), tab("notes", "Notes", "#/library/notes", tip("kb.notes")));
}

// ── notes ───────────────────────────────────────────────────────────────────────────────────

const localProjects = (): [string, string][] => [...data.projects.values()].filter((p) => !(p as { machine?: string | null }).machine).map((p): [string, string] => [p.id, p.slug]);

/// What changed in the notes is asked for again, wherever it shows.
function notesChanged(): void {
  home.refresh(""); notes.reload(); files.reload(); lists.reload(); docs.reload();
}

/// `kb.note`: everything `ptyro kb note` takes. `project`: the one to start on.
async function newNote(project = ""): Promise<void> {
  let list: any[] = [];
  try { list = (await conn.request({ t: "profile_list" })).profiles ?? []; } catch { /* `default` is always there */ }
  const profiles = profileChoices(list, null);
  if (!profiles.some(([v]) => v === "default")) profiles.unshift(["default", "default"]);
  form("New note", [
    { key: "text", label: "Note", type: "textarea", rows: 6, required: true, placeholder: "We chose X because Y", hint: "A decision and its reason, a fact, a how-to. Markdown" },
    { key: "title", label: "Title", placeholder: "its first line", hint: "Its heading, and what lists call it" },
    { key: "project", label: "About", type: "select", options: [["", "No project (a global note)"], ...localProjects()], value: project, hint: "The project it is about" },
    { key: "profile", label: "Seen by", type: "select", options: [["", "Every profile's agents"], ...profiles], value: "", hint: "Whose agents may find it with recall (you always see every note)" },
    { key: "tags", label: "Tags", placeholder: "db, decision", hint: "Words to find it by" },
  ], "Keep", async (v) => {
    const r = await conn.request(noteRequest(v as { text: string }));
    ui.toast(`Noted: ${titleOf(r.item as Listed)}`);
    notesChanged();
    if (location.hash !== "#/library/notes") go("#/library/notes");
    ui.render();
  });
}

async function syncNotes(): Promise<void> {
  const r = await ask<{ notes: number; changed: number; removed: number }>({ t: "kb_sync" });
  if (!r) return;
  ui.toast(`${plural(r.notes, "note")} in the folder: ${r.changed} read again, ${r.removed} gone`);
  notesChanged();
}

function notesView(): HTMLElement {
  const wait = pending(home, "");
  const s = home.get("");
  let body: Child[];
  if (wait || !s) body = [wait];
  else {
    const f = state.f;
    const key = JSON.stringify(notesRequest(f));
    const waitList = pending(notes, key);
    const list = notes.get(key);
    const rows: Child[] = waitList || !list ? [waitList]
      : !list.length ? [h("p", { class: "empty" }, f.project || f.wrote ? "No note matches these filters." : "No notes yet. Write one here; your agents write them with their remember tool. Every search finds notes first.")]
      : byProject(list).flatMap(([name, group]): Child[] => [h("h3", { class: "day" }, name),
        ...group.map((i) => row(h("span", {}, badge("note"), " ", titleOf(i)), about(wroteBy(i.item), `${ago(when(i.item))} ago`), null, { href: `#/library/note/${enc(i.addr)}` }))]);
    body = offBody(s) ?? [
      tabs("notes"),
      h("div", { class: "kb-filters", title: tip("kb.notes") },
        pick("Project", f.project, "Every project", localProjects(), (v) => { f.project = v; }),
        pick("Wrote", f.wrote, "Anyone wrote", WROTE, (v) => { f.wrote = v; })),
      bar(can("control") ? h("button", { class: "primary", title: tip("kb.note"), onclick: () => void newNote(f.project) }, "New note") : null,
        can("control") ? h("button", { class: "small", title: tip("kb.sync"), onclick: () => void syncNotes() }, "Read the folder again") : null),
      h("div", { class: "rows", title: tip("kb.notes") }, ...rows),
    ];
  }
  return h("div", {}, ui.header("Library", "#/more"), h("main", { class: "stack library", title: tip("kb.notes") }, ...body));
}

/// The text being edited, and the note it is of (kept while the page redraws).
const draft = { of: "", text: "" };

/// A note, edited in the page: its file as it is (its header too), Save, and ⋯.
function noteView(addr: string): HTMLElement {
  const wait = pending(files, addr);
  const n = files.get(addr);
  if (wait || !n) return h("div", {}, ui.header("Note", "#/library/notes"), h("main", { class: "stack library" }, wait));
  const i = n.item;
  if (draft.of !== addr) [draft.of, draft.text] = [addr, n.text];
  const box = h("textarea", { class: "kb-note", rows: 16, "aria-label": "The note", spellcheck: "false", autocapitalize: "off", title: tip("kb.notes") });
  box.value = draft.text;
  box.readOnly = !can("control");
  box.addEventListener("input", () => { draft.text = box.value; });
  const save = async (): Promise<void> => {
    if (!box.value.trim()) { ui.toast("An empty note is not saved: ⋯ → Delete… removes it"); return; }
    const r = await ask({ t: "kb_note", addr: i.item.id, text: box.value }, "Saved");
    if (!r) return;
    draft.of = "";
    notesChanged();
  };
  const more = (): void => void menu(titleOf(i), [
    { title: tip("kb.show"), label: "Read it", hint: "As it is kept and found", run: () => open(i.addr) },
    { title: tip("kb.show"), label: "Copy its address", hint: i.addr, run: () => copy(i.addr, "its address") },
    can("full") && { title: tip("kb.forget"), label: "Delete…", danger: true, hint: "Its file is deleted", run: () => forget(i, "#/library/notes") },
  ]);
  return h("div", {},
    ui.header(titleOf(i), "#/library/notes", h("button", { class: "icon", "aria-label": "More", title: tip("kb.notes"), onclick: more }, "⋯")),
    h("main", { class: "stack library", title: tip("kb.notes") },
      h("p", { class: "sub" }, about("note", i.project_name ?? "global", `written by ${wroteBy(i.item)}`, new Date(when(i.item)).toLocaleString(), i.addr)),
      box,
      bar(can("control") ? h("button", { class: "primary", title: tip("kb.notes"), onclick: () => void save() }, "Save") : h("span", { class: "sub" }, "read only"),
        h("span", { class: "sub" }, n.path))));
}

/// `kb.history`: the Library with only what was kept of a session or a worktree (`of`), or of
/// a project; `machine`: the linked machine it is on.
export function historyOf(of: Of | null, project = "", machine = ""): void {
  [state.f, state.typed] = [{ ...noFilters(), of, project, machine }, ""];
  findBox.value = "";
  go("#/library");
  ui.render();
}

// ── the reader ──────────────────────────────────────────────────────────────────────────────

const inBox = h("input", { class: "search", type: "search", placeholder: "Find in it…", "aria-label": "Find in it", autocomplete: "off", spellcheck: false });
inBox.addEventListener("input", () => {
  // (Looked for in all of it, not only the pieces shown first.)
  [state.find, state.foundAt, state.all, state.scrolled] = [inBox.value.trim(), 0, state.all || !!inBox.value.trim(), ""];
  ui.render();
});

/// What the reader asks for: the piece with its neighbours, an item's start, or all of it.
function docKey(addr: string): string {
  const { item, piece } = split(addr);
  if (state.all) return JSON.stringify({ t: "kb_read", addr: item });
  return JSON.stringify(piece === null ? { t: "kb_read", addr, after: 30 } : { t: "kb_read", addr, before: 3, after: 3 });
}

const copy = (text: string, what: string): void => void navigator.clipboard?.writeText(text).then(() => ui.toast(`Copied ${what}`), () => ui.toast("Couldn't copy"));

/// The session an item happened in: shown, resumed first when it is hibernated.
async function toSession(i: Listed): Promise<void> {
  if (i.item.kind !== "session") { ui.toast(`A ${i.item.kind} has no session to go to: this is what is kept of it`); return; }
  const gone = (): void => ui.toast(`The session it happened in is no longer in ptyro: this is what was kept of it (its agent may still carry it on by its own id, ${conversation(i.item.uri)})`);
  if (!i.item.session) { gone(); return; }
  try {
    const s = (await conn.request({ t: "get", addr: i.item.session })).info;
    if (s.state?.t === "dormant" && can("control")) await conn.request({ t: "resume", addr: s.id });
    go(`#/s/${enc(s.id)}`);
  } catch (e) {
    if ((e as { code?: string }).code === "not_found") gone(); else ui.fail(errText(e));
  }
}

/// Forget an item: the daemon says what will go, in the words every client uses, and asks.
async function forget(i: Listed, then = "#/library"): Promise<void> {
  const req = { t: "kb_forget", addrs: [forgetAddr(i)], confirmed: false };
  const gone = (r: { items: number; chunks: number }): void => {
    ui.toast(`Forgot ${plural(r.items, "item")} (${plural(r.chunks, "piece")}): deleted for good, and never kept again`);
    notesChanged();
    go(then);
  };
  try {
    gone(await conn.request(req));
  } catch (e) {
    if ((e as { code?: string }).code !== "needs_confirmation") { ui.fail(errText(e)); return; }
    const q = question(errText(e));
    confirm(q?.[0] ?? "Forget 1 item?", q?.[1] ?? "The text is deleted from the knowledge base for good, and nothing of it is kept again. Your files and the agents' own transcripts aren't touched.", "Forget", async () => { const r = await ask({ ...req, confirmed: true }); if (r) gone(r); });
  }
}

/// Who a piece is by, for its label.
const by = (c: Chunk, i: Item): string => (c.kind === "user" ? "you" : c.kind === "assistant" ? i.agent ?? "the agent" : c.kind);

function readerView(addr: string): HTMLElement {
  const key = docKey(addr);
  const wait = pending(docs, key);
  const d = docs.get(key);
  if (wait || !d) return h("div", {}, ui.header("Library", "#/library"), h("main", { class: "stack library" }, wait));
  const i = d.item;
  const { piece } = split(addr);
  const at = d.stale ? null : piece ?? d.at ?? null;
  const hits = found(d.chunks, state.find);
  const shown = hits.length ? hits[state.foundAt % hits.length] : at;
  const words = state.find ? [state.find] : state.lit;
  if (inBox.value.trim() !== state.find && document.activeElement !== inBox) inBox.value = state.find;
  const step = (n: number): void => { [state.foundAt, state.scrolled] = [(state.foundAt + n + hits.length) % hits.length, ""]; ui.render(); };
  const whole = d.chunks.length >= i.item.chunks;
  const all = (label: string): HTMLElement => h("button", { class: "small", title: tip("kb.show"), onclick: () => { [state.all, state.scrolled] = [true, ""]; ui.render(); } }, label);
  // The piece asked for (or found) comes into view once it is drawn.
  const mark = `${key} ${shown ?? ""}`;
  if (state.scrolled !== mark) {
    state.scrolled = mark;
    requestAnimationFrame(() => document.querySelector(".kb-piece.at")?.scrollIntoView({ block: "center" }));
  }
  const more = (): void => void menu(titleOf(i), [
    i.item.kind === "session" && { title: tip("kb.show"), label: "Go to its session", hint: "Resumed when it is hibernated", run: () => toSession(i) },
    i.item.kind === "note" && i.addr.startsWith("k:") && { title: tip("kb.notes"), label: "Edit it", hint: "Its file, in the page", run: () => go(`#/library/note/${enc(i.addr)}`) },
    { title: tip("kb.show"), label: "Copy its address", hint: i.addr, run: () => copy(shown === null ? i.addr : `${i.addr}/${shown}`, "its address") },
    { title: tip("kb.show"), label: "Copy its text", hint: whole ? "All of it" : "The pieces shown", run: () => copy(d.chunks.map((c) => (c.kind === "user" ? `› ${c.text}` : c.text)).join("\n\n"), "its text") },
    can("full") && { title: tip("kb.forget"), label: "Forget…", danger: true, hint: "Deleted for good, and never kept again", run: () => forget(i) },
  ]);
  return h("div", {},
    ui.header(titleOf(i), "#/library", h("button", { class: "icon", "aria-label": "More", title: tip("kb.show"), onclick: more }, "⋯")),
    h("main", { class: "stack library reader", title: tip("kb.show") },
      h("p", { class: "sub" }, about(i.item.kind, i.project_name, i.item.kind === "note" ? `written by ${wroteBy(i.item)}` : i.item.agent, new Date(when(i.item)).toLocaleString(), plural(i.item.chunks, "piece"), i.addr), " · read only"),
      bar(inBox,
        hits.length ? h("span", { class: "sub" }, `${(state.foundAt % hits.length) + 1}/${hits.length}`) : state.find ? h("span", { class: "sub" }, "not in it") : null,
        hits.length > 1 ? h("button", { class: "small", "aria-label": "The one before", onclick: () => step(-1) }, "↑") : null,
        hits.length > 1 ? h("button", { class: "small", "aria-label": "The next", onclick: () => step(1) }, "↓") : null,
        i.item.kind === "session" ? h("button", { class: "small", title: tip("kb.show"), onclick: () => void toSession(i) }, "Its session") : null,
        i.item.kind === "note" && i.addr.startsWith("k:") ? h("button", { class: "small", title: tip("kb.notes"), onclick: () => go(`#/library/note/${enc(i.addr)}`) }, "Edit") : null),
      d.stale ? h("p", { class: "sub" }, "It was read again since: that piece is gone, so here it is from its start.") : null,
      !whole && (d.chunks[0]?.idx ?? 0) > 0 ? all("Earlier: all of it") : null,
      ...d.chunks.map((c) => h("div", { class: `kb-piece k-${c.kind}${c.idx === shown ? " at" : ""}` },
        h("small", {}, about(by(c, i.item), c.heading, `${i.addr}/${c.idx}`)),
        h("pre", { class: "block" }, ...marks(lit(c.text, words))))),
      !whole ? all(`All of it (${plural(i.item.chunks, "piece")})`) : null));
}

// What is kept changed (a turn was read, something was forgotten or expired), or the switch
// did: what is shown is asked for again.
conn.listeners.add((msg: any) => {
  if (msg.t !== "event" || !location.hash.startsWith("#/library")) return;
  if (msg.event.t !== "kb_updated" && msg.event.t !== "config_reloaded") return;
  home.refresh("");
  const key = listKey();
  if (key) lists.refresh(key);
  const m = /^#\/library\/k\/(.+)$/.exec(location.hash);
  if (m) docs.refresh(docKey(safeDecode(m[1])));
  if (location.hash === "#/library/notes") notes.refresh(JSON.stringify(notesRequest(state.f)));
});

views.set("library", (rest) => (rest.startsWith("k/") ? readerView(safeDecode(rest.slice(2)))
  : rest.startsWith("note/") ? noteView(safeDecode(rest.slice(5)))
  : rest === "notes" ? notesView() : listView()));
