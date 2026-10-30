// Chats (SPEC §22.12, ROADMAP CHAT1.8): plain conversations with your agents, kept by ptyro.
// `#/chats` lists them (pinned first, then by day, searchable, one workspace's or all), `#/c/<id>`
// is one chat (its messages, tool calls and permission cards as they stream, a composer), and
// `#/c/new` starts one. Every control is an action below, as in the TUI and the CLI.

import { conn, host } from "./conn.js";
import { data, machineOf, wsLabel } from "./data.js";
import { machineChip } from "./machines.js";
import { actions, ago, ask, confirm, download, errText, form, h, menu, patch, ui, views } from "./kit.js";
import type { Child } from "./kit.js";

const ACTIONS = {
  "chats": "Your chats: pinned first, then the newest; search them, or start one",
  "chat.new": "A new chat with your agent: type, then send (it can search the web, and it is kept)",
  "chat.open": "Open a chat",
  "chat.send": "Send the message; the answer streams in (Enter sends, Shift+Enter starts a new line)",
  "chat.answer": "Answer what the chat's agent asks: allow once, always, or deny",
  "chat.stop": "Stop the answer being written",
  "chat.retitle": "Change the chat's title (empty: a new automatic one)",
  "chat.pin": "Pin the chat to the top, or unpin it",
  "chat.archive": "Archive the chat (Archived shows them; again brings it back)",
  "chat.delete": "Delete the chat",
  "chat.agent": "Switch the chat to another agent: the next message carries the conversation over",
  "chat.preset": "What the chat may use: web search, read folders, a shell, MCP servers, or no tools",
  "chat.folder": "Folders the chat may read (on the computer it runs on), with the read preset",
  "chat.task": "Make it a task: an agent in a new worktree carries out the chat's last answer",
  "chat.export": "Download the chat as Markdown",
  "chat.search": "Search every chat: titles at once, and from 3 letters every message on every machine (\"phrases\", prefix*, -word, agent:codex, since:7d, by:meaning; ≈ marks a message found by meaning)",
  "chat.meaning": "Switch meaning search on: also find messages by what they are about, not only their words (downloads a 31 MB model once)",
  "chat.archived": "Show archived chats instead",
  "chat.retry": "Ask again: a new answer beside this one, which stays (‹ › flips between them)",
  "chat.edit": "Say this message differently: answered afresh, the old one stays as a branch",
  "chat.branch": "Show another answer (or message) where a retry or an edit made one",
  "chat.fork": "A new chat from here (it carries the conversation; the agent can change)",
  "chat.side": "A side chat: ask about this chat without changing it; it goes when you leave it, unless kept",
  "chat.keep": "Keep this side chat as a chat of its own",
  "chat.incognito": "Incognito: nothing of this chat is kept or searchable, and it ends with its agent",
  "chat.find": "Ask your chats a question: an agent searches them and answers, citing where",
  "chat.find_in": "Find in this chat",
  "chat.cite": "Open the message this citation points to",
  "chat.compare": "Ask several agents: each answers as a branch beside the others, labelled with its agent (‹ 2/3 codex ›)",
  "chat.pick": "Keep this answer: the chat switches to the agent that wrote it, which takes the next message",
  "chat.import": "Import your agents' own histories (Claude Code, Codex, Gemini) as read-only, searchable chats: every agent's and profile's, or one, only recent sessions, or just count them first",
  "chat.imported": "Show only the chats imported from your agents' own histories (⇣), or every chat again",
} as const;
const tip = actions(ACTIONS);

export interface Chat {
  id: string; handle: string; workspace: string; machine?: string | null; title?: string | null; title_src: string;
  agent: string; profile: string; tools: string; pinned: boolean; archived: boolean; incognito: boolean; forked: boolean;
  leaf?: string | null; nodes: number; unread: number; created_ms: number; last_ms: number; cost_micro_usd: number;
  status?: string | null; attention?: { kind: string; text: string; since_ms: number } | null; engine?: string | null;
  side_of?: string | null; folders?: string[]; best_effort?: boolean; shell?: boolean;
  /// A compare under way: each agent's state (SPEC §22.7).
  comparing?: { node: string; agents: { agent: string; state: string; note?: string }[] } | null;
  /// Imported from an agent's own history (`claude:<id>`), and the folder it ran in.
  source?: string | null; source_dir?: string | null;
}
/// A tool preset, with what it lets a chat use (`chat_presets`).
interface Preset { name: string; about: string; builtin: boolean; read: boolean; shell: boolean }
interface Siblings { node: string; at: number; of: number; ids: string[]; agents?: string[] }

/// The shown sibling's agent, when the siblings are not all one agent's (a compare).
function sibLabel(s: Siblings): string {
  const a = s.agents ?? [];
  if (!a.length || a.every((x) => x === a[0])) return "";
  return a[s.at - 1] ?? "";
}

/// `comparing: claude ✓ · codex … · gemini ✗ why`, as the daemon's `ChatCompare::line`.
function compareLine(c: NonNullable<Chat["comparing"]>): string {
  const mark = (st: string): string => st === "done" ? "✓" : st === "failed" ? "✗" : st === "stopped" ? "⊘" : "…";
  const busy = c.agents.some((a) => a.state === "asking");
  return `${busy ? "comparing" : "compared"}: ${c.agents.map((a) => `${a.agent} ${mark(a.state)}${a.note ? ` ${a.note}` : ""}`).join(" · ")}`;
}

/// `imported from claude · ~/repo`.
function importedFrom(c: Chat): string {
  if (!c.source) return "";
  const agent = c.source.split(":")[0];
  return c.source_dir ? `imported from ${agent} · ${c.source_dir}` : `imported from ${agent}`;
}
interface Hit { chat: Chat; node?: string | null; kind: string; snippet: string; at_ms: number; rank: number; how?: string }
interface Item { id: string; t: string; [k: string]: any }

const state = {
  list: new Map<string, Chat>(),
  archived: null as Chat[] | null,
  loaded: false,
  /// `[chat] enabled = false`: the daemon says why, and the Chats tab goes.
  off: "",
  query: "",
  workspace: "",
  /// The chat followed now, and its items.
  following: null as string | null,
  items: [] as Item[],
  /// Where its branch has others (SPEC §22.7).
  siblings: [] as Siblings[],
  /// Show this message once it arrives (a search hit, a citation).
  focus: null as string | null,
  /// Find in the chat, and which of its matches is shown.
  findIn: "",
  findAt: 0,
  /// Message hits of the list's search, and the query they answer.
  hits: [] as Hit[],
  hitsFor: "",
  /// `by:meaning` while meaning search is off: why, with the switch to turn it on.
  meaningOff: "",
  /// Only chats imported from the agents' own histories (`chat ls --imported`).
  imported: false,
};

const label = (c: Chat | undefined): string => c?.title?.trim() || "new chat";
const addr = (c: Chat): string => `c:${c.handle}`;
const working = (c: Chat): boolean => c.status === "working" || c.status === "starting";
const glyph = (c: Chat): string => c.attention || c.status === "blocked" ? "⚠" : working(c) ? "●" : c.status === "errored" ? "✗" : c.incognito ? "◌" : c.pinned ? "★" : c.side_of ? "↳" : c.source ? "⇣" : "·";

/// A snippet with its matches marked (control characters from the daemon) as <mark>.
function snippet(text: string): HTMLElement {
  const el = h("span", { class: "snippet" });
  let on = false;
  for (const part of text.replace(/\n/g, " ").split(/[\u0002\u0003]/)) {
    if (part) el.append(on ? h("mark", {}, part) : document.createTextNode(part));
    on = !on;
  }
  return el;
}

/// An answer's text, its citations (`c:7f3k/01jb…`) links to the message they name.
function cited(text: string): (Node | string)[] {
  const out: (Node | string)[] = [];
  const re = /((?:[a-z0-9._-]+:)?c:[a-z0-9]{4,})\/([a-z0-9]{6,})/gi;
  let at = 0;
  for (const m of text.matchAll(re)) {
    const i = m.index ?? 0;
    out.push(text.slice(at, i));
    const handle = m[1].replace(/^.*c:/, "");
    const c = [...state.list.values()].find((x) => x.handle === handle || x.id.endsWith(handle));
    out.push(c ? h("a", { href: `#/c/${encodeURIComponent(c.id)}/${encodeURIComponent(m[2])}`, title: tip("chat.cite") }, m[0]) : m[0]);
    at = i + m[0].length;
  }
  out.push(text.slice(at));
  return out;
}

/// Whether the Chats tab shows (chats are on, as far as the daemon has said).
export function chatsShown(): boolean {
  return !state.off;
}

export async function loadChats(): Promise<void> {
  try {
    const r = await conn.request({ t: "chat_list", all: true });
    state.list = new Map((r.chats as Chat[]).map((c) => [c.id, c]));
    state.off = "";
    // Every preset with what it means (yours too); the built-in three until the daemon says.
    const p = await conn.request({ t: "chat_presets" }).catch(() => null);
    if (p) { presets = p.presets as Preset[]; presetsKnown = true; incognitoDefault = !!p.incognito; }
  } catch (e) {
    state.off = errText(e);
  }
  state.loaded = true;
  ui.render();
}

function sorted(list: Iterable<Chat>): Chat[] {
  return [...list].sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.last_ms - a.last_ms);
}

function dayOf(ms: number): string {
  const day = 86_400_000;
  const today = new Date(); today.setHours(0, 0, 0, 0);
  const t = today.getTime();
  if (ms >= t) return "Today";
  if (ms >= t - day) return "Yesterday";
  if (ms >= t - 6 * day) return "This week";
  return "Earlier";
}

// ── the list ────────────────────────────────────────────────────────────────────────────────

const findBox = h("input", { class: "search", type: "search", placeholder: "Search chats", "aria-label": "Search chats", autocapitalize: "off", autocomplete: "off", title: tip("chat.search") });
let searchTimer = 0;
findBox.addEventListener("input", () => {
  state.query = findBox.value.trim().toLowerCase();
  const l = document.querySelector<HTMLElement>("main.chats .rows"); if (l) patch(l, rows());
  // Every message, on every machine, from 3 letters on (a shorter prefix matches everything).
  clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => void searchMessages(findBox.value.trim()), 250);
});

async function searchMessages(q: string): Promise<void> {
  if ([...q].length < 3) { state.hits = []; state.hitsFor = ""; ui.render(); return; }
  try {
    const r = await conn.request({ t: "chat_search", query: q, workspace: state.workspace || null, archived: !!state.archived, limit: 30, machines: true });
    if (findBox.value.trim() !== q) return;
    state.hits = r.hits as Hit[];
    state.hitsFor = q;
    state.meaningOff = "";
  } catch (e) {
    state.hits = []; state.hitsFor = q;
    state.meaningOff = (e as { code?: string }).code === "needs_confirmation" ? errText(e) : "";
  }
  ui.render();
}

function hitRows(): Node[] {
  if (!state.query || [...state.query].length < 3) return [];
  if (state.meaningOff) return [h("p", { class: "sub" }, `${state.meaningOff}. `, h("button", { class: "small", title: tip("chat.meaning"), onclick: () => void meaningOn() }, "Switch it on"))];
  if (!state.hits.length) return state.hitsFor ? [h("p", { class: "sub" }, "No message matches.")] : [];
  return [h("h3", { class: "day" }, `Messages (${state.hits.length})`), ...state.hits.map((x) =>
    h("a", { class: "chat-row hit", href: `#/c/${encodeURIComponent(x.chat.id)}${x.node ? `/${encodeURIComponent(x.node)}` : ""}`, title: tip("chat.open") },
      h("span", { class: "main" }, h("span", { class: "title" }, label(x.chat)), x.chat.machine ? machineChip(x.chat.workspace) : null, h("small", {}, ` ${x.chat.agent} · ${ago(x.at_ms)}`)),
      x.how === "meaning" ? h("span", { class: "snippet", title: "Found by meaning: none of the words is in it" }, "≈ ", snippet(x.snippet)) : snippet(x.snippet)))];
}

/// Meaning search on, in your config (as the settings panel would): the daemon downloads its model.
async function meaningOn(): Promise<void> {
  try {
    await host("settings.apply", { scope: "user", change: { t: "set", key: "chat.meaning", value: "true" } });
    state.meaningOff = "";
    ui.toast("Meaning search is on: its model is downloading, then your chats are read (ptyro doctor shows how far)");
    ui.render();
  } catch (e) { ui.toast(errText(e)); }
}

/// The search box as a title filter: the search's operators (`by:meaning`, `since:7d`,
/// `workspace:w`, `-word`) are the message search's, not title text; `agent:x` narrows by agent.
function titleQuery(q: string): { text: string; agent: string } {
  let agent = "";
  const words: string[] = [];
  for (const w of q.split(/\s+/).filter(Boolean)) {
    const [k, v] = w.includes(":") ? [w.slice(0, w.indexOf(":")), w.slice(w.indexOf(":") + 1)] : ["", ""];
    if (k === "agent") agent = v;
    else if (["by", "since", "workspace", "ws"].includes(k) || (w.startsWith("-") && w.length > 1)) continue;
    else words.push(w.replace(/^"|"$/g, "").replace(/\*$/, ""));
  }
  return { text: words.join(" "), agent };
}

function rows(): Node[] {
  const source = state.archived ?? sorted(state.list.values());
  const { text: q, agent } = titleQuery(state.query);
  // Side chats sit with their chat; incognito ones are never "every chat" (SPEC §22.12).
  const shown = source.filter((c) => !c.side_of && !c.incognito && (!state.workspace || c.workspace === state.workspace) && (!state.imported || !!c.source) && (!agent || c.agent.includes(agent)) && (!q || label(c).toLowerCase().includes(q) || c.agent.includes(q) || importedFrom(c).toLowerCase().includes(q)));
  if (!shown.length && state.query) return [h("p", { class: "sub" }, q ? `No chat titled “${q}”.` : "No chat matches."), ...hitRows()];
  if (!shown.length) {
    return [h("div", { class: "empty" },
      h("p", {}, q ? `Nothing matches “${q}”.` : state.archived ? "No archived chats." : state.imported ? "No imported chats: ⇣ imports your agents' own histories." : "No chats yet. Ask anything: it can search the web, and it is kept here."),
      q || state.archived || state.imported || !canWrite() ? null : h("button", { class: "primary", title: tip("chat.new"), onclick: () => newChat() }, "Start a chat"))];
  }
  const out: Node[] = [];
  let group = "";
  for (const c of shown) {
    const g = c.pinned && !state.archived ? "Pinned" : dayOf(c.last_ms);
    if (g !== group) { out.push(h("h3", { class: "day" }, g)); group = g; }
    // Another machine's that is down: listed as it was, greyed (SPEC §22.11).
    const down = !!c.machine && machineOf(c.workspace)?.state !== "online";
    out.push(h("div", { class: `chat-row${c.unread ? " unread" : ""}${c.attention ? " blocked" : ""}${down ? " offline" : ""}` },
      h("a", { class: "main", href: `#/c/${encodeURIComponent(c.id)}`, title: tip("chat.open") },
        h("span", { class: "glyph" }, glyph(c)),
        h("span", { class: "title" }, label(c)),
        h("small", {}, ` ${c.agent} · ${ago(c.last_ms)}${state.workspace ? "" : ` · ${wsLabel(c.workspace)}`}`),
        // A shell preset: marked as a `dangerous` session is.
        c.shell ? h("b", { class: "warn", title: "This chat's agent has a shell: it runs commands as you" }, " ⚠") : null,
        c.machine ? machineChip(c.workspace) : null,
        c.unread ? h("b", { class: "unread-dot", "aria-label": "unread" }, "●") : null),
      h("button", { class: "icon", "aria-label": "More", onclick: () => chatMenu(c) }, "⋯")));
  }
  return [...out, ...hitRows()];
}

function listView(): HTMLElement {
  if (!state.loaded) void loadChats();
  if (state.off) {
    return h("div", {}, ui.header("Chats", false), h("main", { class: "center" }, h("p", {}, state.off)));
  }
  if (findBox.value !== state.query && document.activeElement !== findBox) findBox.value = state.query;
  const wsSelect = h("select", { "aria-label": "Workspace", onchange: (e: Event) => { state.workspace = (e.target as HTMLSelectElement).value; ui.render(); } },
    h("option", { value: "" }, "Every workspace"),
    ...[...data.workspaces.values()].map((w) => h("option", { value: w.name }, wsLabel(w.name))));
  wsSelect.value = state.workspace;
  const archived = h("button", { class: `small${state.archived ? " on" : ""}`, title: tip("chat.archived"), onclick: () => void toggleArchived() }, state.archived ? "Back to chats" : "Archived");
  // Shown once there are imported chats (or while it filters).
  const anyImported = state.imported || [...state.list.values()].some((c) => !!c.source);
  const imported = anyImported ? h("button", { class: `small${state.imported ? " on" : ""}`, title: tip("chat.imported"), "aria-pressed": String(state.imported), onclick: () => { state.imported = !state.imported; ui.render(); } }, "Imported") : null;
  return h("div", {},
    ui.header("Chats", false,
      canWrite() ? h("button", { class: "icon", title: tip("chat.find"), "aria-label": "Ask your chats", onclick: () => findChats() }, "？") : null,
      conn.scope === "full" ? h("button", { class: "icon", title: tip("chat.import"), "aria-label": "Import agent histories", onclick: () => importHistories() }, "⇣") : null,
      canWrite() ? h("button", { class: "icon plus", title: tip("chat.new"), "aria-label": "New chat", onclick: () => newChat() }, "＋") : null),
    h("div", { class: "searchbar" }, findBox, wsSelect, archived, imported),
    h("main", { class: "list chats", title: tip("chats") }, h("div", { class: "rows" }, ...rows())));
}

/// `chat import`: the agents' own histories as read-only chats (asked first: it copies them),
/// narrowed as the CLI's flags do.
function importHistories(): void {
  const profiles = [...new Set([...data.workspaces.values()].map((w) => w.profile).concat([...state.list.values()].map((c) => c.profile)).filter(Boolean))].sort();
  form("Import agent histories", [
    { key: "agent", label: "Whose", type: "select", options: [["all", "Every agent's"], ["claude", "Claude Code"], ["codex", "Codex"], ["gemini", "Gemini"]] },
    { key: "profile", label: "Profile", type: "select", options: [["", "Every profile's"], ...profiles.map((p): [string, string] => [p, p])] },
    { key: "since", label: "Only sessions touched in (optional)", placeholder: "30d, 12h" },
    { key: "dry", label: "Only count them (import nothing)", type: "check" },
  ], "Import", async (v) => {
    const dry = !!v.dry;
    ui.toast(dry ? "Counting what your agents' histories would bring…" : "Importing your agents' histories…");
    try {
      // A first import can take minutes: no two-minute timeout for this one.
      const r = await conn.request({ t: "chat_import", agent: v.agent || "all", profile: v.profile || null, since: (v.since as string | undefined)?.trim() || null, dry_run: dry }, 60 * 60_000) as { report: { dry_run: boolean; sources: { new: number; grown: number; messages: number }[]; notes?: string[] } };
      const n = r.report.sources.reduce((a, s) => [a[0] + s.new, a[1] + s.grown, a[2] + s.messages], [0, 0, 0]);
      const what = dry ? ["Would import", "would grow"] : ["Imported", "grew"];
      ui.toast(n[0] + n[1] ? `${what[0]} ${n[0]} new chat${n[0] === 1 ? "" : "s"}, ${n[1]} ${what[1]} (${n[2]} messages)` : dry ? "Nothing new to import" : "Nothing new: every session is imported");
    } catch (e) { ui.toast(errText(e)); }
    // Whatever happened, what did come is listed.
    await loadChats();
  }, { intro: "Your Claude Code, Codex and Gemini sessions become read-only chats here: their text is copied into chat.db, where search finds it. Sessions already imported only gain what is new. (Settings → Chat → import keeps doing it.)" });
}

/// Send `req`; when the daemon asks first (the chat's conversation would go to another
/// profile's account, SPEC §22.7), show its question and send it again, agreed to.
async function agreed<T = any>(req: Record<string, unknown>): Promise<T> {
  try {
    return await conn.request(req) as T;
  } catch (e) {
    if ((e as { code?: string }).code !== "needs_confirmation") throw e;
    const yes = await new Promise<boolean>((ok) => {
      let said = false;
      const s = confirm("Another profile?", errText(e), "Go ahead", () => { said = true; ok(true); }, false);
      s.onClose(() => setTimeout(() => { if (!said) ok(false); }, 0));
    });
    if (!yes) throw new Error("cancelled");
    return await conn.request({ ...req, confirmed: true }) as T;
  }
}

/// `ask`, through `agreed`.
async function askAgreed<T = any>(req: Record<string, unknown>, ok?: string): Promise<T | undefined> {
  try {
    const r = await agreed<T>(req);
    if (ok) ui.toast(ok);
    return r;
  } catch (e) {
    if ((e as Error).message !== "cancelled") ui.toast(errText(e));
    return undefined;
  }
}

/// `chat compare`: which agents answer the chat's last message again (or a new question).
function compareAgents(c: Chat, question = ""): void {
  if (c.source) { ui.toast("An imported chat stays as its agent wrote it: send it a message (it continues as a fork), then compare there"); return; }
  const names = agentNames();
  form("Ask several agents", [
    { key: "agents", label: "Agents (each answers as a branch beside the others)", value: names.join(" "), placeholder: "claude codex gemini" },
    { key: "q", label: "A new question (empty: the last message again)", type: "textarea", rows: 2, value: question },
  ], "Ask them", async (v) => {
    const agents = String(v.agents ?? "").split(/[\s,]+/).filter(Boolean);
    await askAgreed({ t: "chat_compare", chat: c.id, agents, text: String(v.q ?? "").trim() || null }, "Asked: each answer is a branch (‹ › flips between them; Keep this answer switches to its agent)");
  }, { intro: "Every agent answers the same message; the chat's own carries on in its conversation, the others are told it so far." });
}

/// `chat pick`: keep the answer shown (or the branch through `node`); says who takes the next message.
async function pickAnswer(chat: string, node?: string): Promise<void> {
  const r = await ask<{ info: Chat }>({ t: "chat_pick", chat, node: node ?? null });
  if (!r) return;
  state.list.set(r.info.id, r.info);
  const who = r.info.profile && r.info.profile !== "default" ? `${r.info.agent}@${r.info.profile}` : r.info.agent;
  ui.toast(`Kept: ${who} takes the next message`);
  ui.render();
}

/// `chat find`: ask the chats; the answer is an incognito chat of its own, opened.
function findChats(): void {
  form("Ask your chats", [{ key: "q", label: "What do you want to know?", required: true, placeholder: "what did we decide about the retry policy?" }], "Ask", async (v) => {
    const r = await conn.request({ t: "chat_find", question: v.q });
    const info = r.info as Chat;
    state.list.set(info.id, info);
    location.hash = `#/c/${encodeURIComponent(info.id)}`;
  }, { intro: "An agent searches your chats and answers, citing the messages (tap one to open it). Nothing of this is kept." });
}

async function toggleArchived(): Promise<void> {
  if (state.archived) { state.archived = null; ui.render(); return; }
  const r = await ask<{ chats: Chat[] }>({ t: "chat_list", all: true, archived: true });
  state.archived = r ? sorted(r.chats) : [];
  ui.render();
}

function newChat(): void {
  location.hash = `#/c/new${state.workspace ? `/${encodeURIComponent(state.workspace)}` : ""}`;
}

function update(c: Chat, change: object, ok: string): Promise<unknown> {
  return ask({ t: "chat_update", chat: c.id, ...change }, ok);
}

/// Keep a side chat: at once here too, so leaving before the daemon's word doesn't drop it.
function keepSide(c: Chat): void {
  c.side_of = null;
  ui.render();
  void update(c, { keep: true }, "Kept");
}

function chatMenu(c: Chat): void {
  const archived = c.archived || !!state.archived?.some((x) => x.id === c.id);
  const open = { title: tip("chat.open"), label: "Open", run: () => { location.hash = `#/c/${encodeURIComponent(c.id)}`; } };
  const md = { title: tip("chat.export"), label: "Download as Markdown", run: () => void exportChat(c) };
  // A device that may only read gets what reads (SPEC §22.11).
  const find = { title: tip("chat.find_in"), label: "Find in this chat", run: () => findInChat(c) };
  if (!canWrite()) { menu(label(c), [open, find, md]); return; }
  const side = !!c.side_of;
  menu(label(c), [
    open,
    side && { title: tip("chat.keep"), label: "Keep this side chat", hint: "it becomes a chat of its own", run: () => keepSide(c) },
    !c.incognito && { title: tip("chat.fork"), label: "Fork", hint: "a new chat from the end", run: () => forkAt(c, null) },
    !c.incognito && { title: tip("chat.side"), label: "Side chat", hint: "ask about it without changing it", run: () => void sideChat(c) },
    find,
    { title: tip("chat.retitle"), label: "Rename", run: () => retitle(c) },
    { title: tip("chat.pin"), label: c.pinned ? "Unpin" : "Pin", run: () => update(c, { pinned: !c.pinned }, c.pinned ? "Unpinned" : "Pinned") },
    { title: tip("chat.agent"), label: "Switch agent", hint: `now ${c.agent}`, run: () => pickAgent(c) },
    { title: tip("chat.preset"), label: "Tools", hint: `now ${c.tools}`, run: () => pickPreset(c) },
    { title: tip("chat.folder"), label: "Folders", hint: c.folders?.length ? c.folders.join(", ") : "none", run: () => editFolders(c) },
    !c.incognito && !c.source && { title: tip("chat.task"), label: "Make it a task", hint: "an agent carries it out in a new worktree", run: () => makeTask(c) },
    !c.incognito && !c.source && { title: tip("chat.compare"), label: "Ask several agents", hint: "each answers as a branch", run: () => compareAgents(c) },
    state.following === c.id && state.siblings.some((s) => sibLabel(s)) && { title: tip("chat.pick"), label: "Keep this answer", hint: "its agent takes the next message", run: () => void pickAnswer(c.id) },
    md,
    { title: tip("chat.archive"), label: archived ? "Bring back from the archive" : "Archive", run: async () => {
      await update(c, { archived: !archived }, archived ? "Back from the archive" : "Archived");
      if (state.archived) state.archived = state.archived.filter((x) => x.id !== c.id);
      if (state.following === c.id && !archived) location.hash = "#/chats";
      ui.render();
    } },
    { title: tip("chat.delete"), label: "Delete", danger: true, run: () => confirm(`Delete “${label(c)}”?`, "It goes from every list, on every device.", "Delete", async () => {
      await ask({ t: "chat_delete", chat: c.id }, "Deleted");
      if (state.archived) state.archived = state.archived.filter((x) => x.id !== c.id);
      if (state.following === c.id) location.hash = "#/chats";
    }) },
  ]);
}

function forkAt(c: Chat, node: string | null): void {
  menu(node ? "Fork from this message" : "Fork this chat", [
    { title: tip("chat.fork"), label: `On ${c.agent}`, hint: "the same agent", run: () => void fork(c, node, null) },
    ...agentNames().filter((a) => a !== c.agent).map((a) => ({ title: tip("chat.fork"), label: `On ${a}`, hint: "the conversation carries over", run: () => void fork(c, node, a) })),
  ]);
}

async function fork(c: Chat, node: string | null, agent: string | null): Promise<void> {
  const r = await ask<{ info: Chat }>({ t: "chat_fork", chat: c.id, node, agent, side: false }, "Forked");
  if (r) { state.list.set(r.info.id, r.info); location.hash = `#/c/${encodeURIComponent(r.info.id)}`; }
}

/// A side chat; with a question (`/side QUESTION`), asked in it at once.
async function sideChat(c: Chat, question = ""): Promise<void> {
  const r = await ask<{ info: Chat }>({ t: "chat_fork", chat: c.id, side: true });
  if (!r) return;
  state.list.set(r.info.id, r.info);
  location.hash = `#/c/${encodeURIComponent(r.info.id)}`;
  if (question) void ask({ t: "chat_send", chat: r.info.id, text: question });
}

function findInChat(c: Chat): void {
  if (state.following !== c.id) location.hash = `#/c/${encodeURIComponent(c.id)}`;
  // The same text again: its next match.
  form("Find in this chat", [{ key: "q", label: "Text (the same again: the next match)", required: true, value: state.findIn }], "Find", (v) => {
    const q = (v.q as string) ?? "";
    const hits = state.items.filter((x) => typeof x.text === "string" && x.text.toLowerCase().includes(q.toLowerCase()));
    state.findAt = q === state.findIn && hits.length ? (state.findAt + 1) % hits.length : 0;
    state.findIn = q;
    if (hits.length) { state.focus = hits[state.findAt].id; ui.toast(`${state.findAt + 1}/${hits.length}`); }
    else ui.toast("Not in this chat (the Chats page searches them all)");
    ui.render();
  });
}

function retitle(c: Chat): void {
  form("Rename the chat", [{ key: "title", label: "Title (empty: a new automatic one)", value: c.title ?? "" }], "Save", async (v) => {
    await conn.request({ t: "chat_update", chat: c.id, title: (v.title as string) ?? "" });
    ui.toast("Renamed");
  });
}

function agentNames(): string[] {
  const names = ["claude", "codex", "gemini", "opencode"];
  for (const a of data.agents) if (!names.includes(a.name)) names.push(a.name);
  // Antigravity has no structured mode, so it can't chat (SPEC §22.5).
  return names.filter((n) => n !== "antigravity" && (data.agents.length === 0 || data.agents.some((a) => a.name === n && a.installed)));
}

function pickAgent(c: Chat): void {
  // Each with where ptyro found it (an npx cache, nvm…), so a surprising copy is visible.
  const where = (a: string) => data.agents.find((x) => x.name === a)?.found ?? "";
  menu("Which agent? The conversation carries over", agentNames().map((a) => ({ title: tip("chat.agent"), label: a, hint: [a === c.agent ? "now" : "", where(a)].filter(Boolean).join(" · "), run: () => update(c, { agent: a }, `The next message goes to ${a}`) })));
}

/// Whether `presets` is the daemon's list (yours too), not the built-in three it starts as.
let presetsKnown = false;
/// `[chat] incognito`: a new chat starts incognito (its box ticked; unticked, it is kept).
let incognitoDefault = false;
let presets: Preset[] = [
  { name: "chat", about: "web search", builtin: true, read: false, shell: false },
  { name: "read", about: "web search · reads the chat's folders", builtin: true, read: true, shell: false },
  { name: "none", about: "no tools", builtin: true, read: false, shell: false },
];

function pickPreset(c: Chat): void {
  menu("What may the chat use?", presets.map((p) => ({ title: tip("chat.preset"), label: p.name, hint: `${p.name === c.tools ? "now · " : ""}${p.about}${p.builtin ? "" : " (yours)"}`, run: () => update(c, { preset: p.name }, `Tools: ${p.name}`) })));
}

/// Folders, one a line, as full paths on the computer the chat runs on (`~` works).
function folderList(text: string): string[] {
  return text.split(/[\n,]/).map((s) => s.trim()).filter(Boolean);
}

function editFolders(c: Chat): void {
  const reads = presets.find((p) => p.name === c.tools)?.read;
  form("Folders this chat may read", [{ key: "f", label: "One a line (empty: none)", type: "textarea", rows: 3, placeholder: "~/notes", value: (c.folders ?? []).join("\n") }], "Save", async (v) => {
    await conn.request({ t: "chat_update", chat: c.id, folders: folderList(v.f as string) });
    ui.toast(reads || !folderList(v.f as string).length ? "Saved: the next message starts it with them" : "Saved; the preset reads no files: Tools → read lets it");
  }, { intro: "On the computer the chat runs on. It reads them with the read preset (or one of yours that reads), and nothing else." });
}

/// `chat.task` (SPEC §22.7): the project (the chat's workspace's, when it has one), the agent
/// that works on it (the chat's own by default) and the worktree's name (from the title).
/// `/task [PROJECT] [--agent A] [--name N]` goes at once.
function makeTask(c: Chat, args = ""): void {
  const here = [...data.projects.values()].filter((p) => !(p as any).machine);
  if (!here.length) { ui.toast("A task needs a project: add one first"); return; }
  const ws = [...data.workspaces.values()].find((w) => w.name === c.workspace);
  const mine = here.filter((p) => ws?.projects.includes(p.id));
  const guess = mine.length === 1 ? mine[0].slug : !mine.length && here.length === 1 ? here[0].slug : "";
  const go = async (slug: string, agent: string | null, name: string | null): Promise<void> => {
    const r = await askAgreed<{ agent?: { id: string } | null }>({ t: "chat_task", chat: c.id, project: slug, agent, name }, "A task now: the chat says where it went");
    if (r?.agent) location.hash = `#/s/${encodeURIComponent(r.agent.id)}`;
  };
  if (args) {
    const t = taskArgs(args);
    if (typeof t === "string") { ui.toast(t); return; }
    const slug = t.project ?? guess;
    if (slug) { void go(slug, t.agent, t.name); return; }
  }
  form("Make it a task", [
    { key: "project", label: "The project it works in", type: "select", options: here.map((p): [string, string] => [p.slug, p.slug]), value: guess || here[0].slug },
    { key: "agent", label: "Who works on it", type: "select", options: [["", `${c.agent}@${c.profile} (the chat's)`], ...agentNames().filter((a) => a !== c.agent).map((a): [string, string] => [a, a])] },
    { key: "name", label: "The worktree's name (optional)", placeholder: "from the chat's title", slug: "name" },
  ], "Make it", (v) => go(String(v.project), (v.agent as string) || null, (v.name as string)?.trim() || null),
  { intro: "An agent in a new worktree of the project carries out the chat's last answer, starting from its conversation." });
}

/// `/task`'s words, as `chat task`'s flags: `[PROJECT] [--agent A] [--name N]`; a string: what's wrong.
function taskArgs(text: string): { project: string | null; agent: string | null; name: string | null } | string {
  const out = { project: null as string | null, agent: null as string | null, name: null as string | null };
  const words = text.split(/\s+/).filter(Boolean);
  const usage = "/task [PROJECT] [--agent A] [--name N]";
  while (words.length) {
    const w = words.shift()!;
    const m = /^--(agent|name|project)(?:=(.*))?$/.exec(w) ?? /^(agent|name):(.*)$/.exec(w);
    if (m) {
      const key = m[1] as "agent" | "name" | "project";
      const value = m[2] ?? words.shift() ?? "";
      if (!value) return `--${key} needs a value: ${usage}`;
      out[key] = value;
    } else if (w.startsWith("--") || out.project) return `${w}: ${usage}`;
    else out.project = w;
  }
  return out;
}

async function exportChat(c: Chat): Promise<void> {
  const r = await ask<{ text: string }>({ t: "chat_export", chat: c.id, format: "md" });
  if (r) download(`${label(c).replace(/[^\w.-]+/g, "-").slice(0, 60) || c.handle}.md`, r.text, "text/markdown");
}

// ── one chat ────────────────────────────────────────────────────────────────────────────────

/// The page's composer and transcript stay in the DOM across renders, so a draft, the caret
/// and the scroll position survive the events that re-render everything around them.
interface Page { id: string; el: HTMLElement; box: HTMLElement; reply: HTMLTextAreaElement; els: Map<string, { key: string; el: HTMLElement }>; agent: HTMLSelectElement; preset: HTMLSelectElement; incognito: HTMLInputElement; folders: HTMLInputElement; compare: HTMLElement; sending: boolean }
let page: Page | null = null;
const drafts = new Map<string, string>();

function canWrite(): boolean {
  return conn.scope === "control" || conn.scope === "full";
}

function follow(id: string | null): void {
  if (state.following === id) return;
  if (state.following) conn.send({ t: "chat_unsubscribe", chat: state.following });
  state.following = id;
  state.items = [];
  state.siblings = [];
  if (id) conn.send({ t: "chat_subscribe", chat: id });
}
window.addEventListener("hashchange", () => {
  // Leaving a side chat that wasn't kept: it goes (SPEC §22.7).
  const left = state.following ? state.list.get(state.following) : undefined;
  if (left?.side_of && !location.hash.startsWith(`#/c/${encodeURIComponent(left.id)}`)) void conn.request({ t: "chat_delete", chat: left.id }).catch(() => {});
  if (!/^#\/c\//.test(location.hash)) follow(null);
  if (location.hash !== `#/c/${atRoute}`) atRoute = "";
  state.findIn = "";
});

function pageFor(id: string): Page {
  if (page?.id === id) return page;
  const reply = h("textarea", { rows: 2, enterkeyhint: "send", placeholder: "Ask anything…", title: tip("chat.send") });
  reply.value = drafts.get(id) ?? "";
  reply.addEventListener("input", () => drafts.set(id, reply.value));
  reply.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); void send(); } });
  const agent = h("select", { "aria-label": "Agent", title: tip("chat.agent") }, h("option", { value: "" }, "default agent"), ...agentNames().map((a) => h("option", { value: a }, a)));
  // The default is `[chat] preset`'s, whatever it is (a preset of your own too).
  const preset = h("select", { "aria-label": "Tools", title: tip("chat.preset") }, h("option", { value: "" }, "default tools"), ...presets.map((p) => h("option", { value: p.name }, `${p.name}: ${p.about}`)));
  const incognito = h("input", { type: "checkbox", title: tip("chat.incognito") });
  incognito.checked = incognitoDefault;
  const folders = h("input", { type: "text", placeholder: "Folders it may read (optional), e.g. ~/notes", "aria-label": "Folders", title: tip("chat.folder"), autocapitalize: "off", spellcheck: "false" });
  // Compare: the first message goes to each agent checked (two or more), each answer a branch.
  const compare = h("div", { class: "bar chat-pick compare", title: tip("chat.compare") }, h("span", { class: "sub" }, "Ask several:"),
    ...agentNames().map((a) => h("label", { class: "check" }, h("input", { type: "checkbox", value: a }), ` ${a}`)));
  page = { id, el: h("div", { class: "session chat-page" }), box: h("div", { class: "chat" }), reply, els: new Map(), agent, preset, incognito, folders, compare, sending: false };
  // A new chat's page starts empty (not with what a failed try left).
  if (id.startsWith("new")) state.items = [];
  return page;
}

async function send(): Promise<void> {
  const p = page;
  if (!p) return;
  const text = p.reply.value;
  if (!text.trim()) return;
  if (text.trim().startsWith("/")) { p.reply.value = ""; drafts.delete(p.id); slash(p.id, text.trim()); return; }
  // One at a time: a second Send while the first is on its way would send it twice.
  if (p.sending) { ui.toast("One moment: the last message is on its way"); return; }
  p.sending = true;
  try {
    if (p.id.startsWith("new")) {
      const ws = decodeURIComponent(p.id.split("/")[1] ?? "") || undefined;
      state.items = [{ id: "first", t: "user", text }];
      ui.render();
      const compare = [...p.compare.querySelectorAll<HTMLInputElement>("input:checked")].map((x) => x.value);
      const r = await agreed({ t: "chat_new", workspace: ws, agent: p.agent.value || null, preset: p.preset.value || null, prompt: text, incognito: p.incognito.checked, folders: folderList(p.folders.value), compare: compare.length > 1 ? compare : [] });
      p.reply.value = ""; drafts.delete(p.id);
      p.sending = false;
      const info = r.info as Chat;
      state.list.set(info.id, info);
      // The same page, now the chat's own.
      location.replace(`#/c/${encodeURIComponent(info.id)}`);
    } else {
      const r = await conn.request({ t: "chat_send", chat: p.id, text });
      if (p.reply.value === text) { p.reply.value = ""; drafts.delete(p.id); }
      // An imported chat carries on as a fork of it: go there.
      const info = r?.info as Chat | undefined;
      if (info && info.id !== p.id) {
        state.list.set(info.id, info);
        ui.toast("Continued as a fork: the imported chat stays as it was");
        location.hash = `#/c/${encodeURIComponent(info.id)}`;
      }
    }
  } catch (e) { ui.toast(errText(e)); }
  p.sending = false;
}

/// The composer's slash commands (SPEC §22.12), as in the TUI.
function slash(id: string, text: string): void {
  const [name, ...rest] = text.slice(1).split(" ");
  const arg = rest.join(" ").trim();
  const c = state.list.get(id);
  const lastUser = [...state.items].reverse().find((x) => x.t === "user");
  const needsChat = ["retry", "edit", "fork", "side", "keep", "agent", "title", "export", "branch", "cite", "task", "pick"];
  if (!c && needsChat.includes(name)) { ui.toast(`/${name} works in a chat: send a first message`); return; }
  if (name === "edit" && !lastUser) { ui.toast("No message to edit yet"); return; }
  switch (name) {
    case "retry": if (c) void ask({ t: "chat_retry", chat: c.id }); return;
    case "edit": if (c && lastUser) { if (arg) void ask({ t: "chat_edit", chat: c.id, node: lastUser.id, text: arg }); else editMessage(c, lastUser); } return;
    case "fork": if (c) forkAt(c, null); return;
    case "side": if (c) void sideChat(c, arg); return;
    case "keep": if (c) keepSide(c); return;
    case "agent": if (c) { if (arg) void update(c, { agent: arg }, `The next message goes to ${arg}`); else pickAgent(c); } return;
    case "tools":
      if (c) { if (arg) void update(c, { preset: arg }, `Tools: ${arg}`); else pickPreset(c); }
      else if (page) {
        if (!arg) { ui.toast(presets.map((p) => `${p.name}: ${p.about}`).join(" · ")); return; }
        const known = presets.some((p) => p.name === arg) || [...page.preset.options].some((o) => o.value === arg);
        // The daemon's list is in and doesn't have it: say so, rather than send the default.
        if (!known && presetsKnown) { ui.toast(`No preset named “${arg}”: ${presets.map((p) => p.name).join(", ")} (Settings → Chat adds yours)`); return; }
        if (!known) page.preset.append(h("option", { value: arg }, arg));
        page.preset.value = arg;
        ui.toast(`Tools: ${arg}`);
      }
      return;
    case "folder": case "folders": {
      const none = arg === "none" || arg === "-";
      if (!c) { if (page) { page.folders.value = none ? "" : [page.folders.value, arg].filter(Boolean).join(", "); ui.toast(none ? "No folders" : `It may read ${arg}`); } return; }
      if (!arg) { editFolders(c); return; }
      const next = none ? [] : [...new Set([...(c.folders ?? []), arg])];
      void update(c, { folders: next }, none ? "No folders" : `It may read ${arg} from the next message on`);
      return;
    }
    case "task": if (c) makeTask(c, arg); return;
    case "compare": {
      // The words that name agents (`codex`, `codex@work`), then the question.
      const words = arg.split(/\s+/).filter(Boolean);
      const agents: string[] = [];
      for (let named = agentWords(words[0] ?? ""); words.length && named; named = agentWords(words[0] ?? "")) { agents.push(...named); words.shift(); }
      const question = words.join(" ");
      if (!c) {
        if (!page) return;
        const unique = [...new Set(agents)];
        if (unique.length < 2) { ui.toast("/compare AGENT AGENT…: the first message goes to each of two or more (e.g. /compare claude codex@work)"); return; }
        // `codex@work` too: a box of its own, checked, beside the bare names.
        for (const a of unique) {
          if (![...page.compare.querySelectorAll<HTMLInputElement>("input")].some((b) => b.value === a)) page.compare.append(h("label", { class: "check" }, h("input", { type: "checkbox", value: a }), ` ${a}`));
        }
        for (const box of page.compare.querySelectorAll<HTMLInputElement>("input")) box.checked = unique.includes(box.value);
        if (question) { page.reply.value = question; drafts.set(page.id, question); }
        ui.toast(`The first message goes to ${unique.join(", ")} (each answer a branch)`);
        return;
      }
      if (!agents.length && !question) { compareAgents(c); return; }
      void askAgreed({ t: "chat_compare", chat: c.id, agents, text: question || null }, `Asked ${agents.length ? agents.join(", ") : "every installed agent"}: each answer is a branch`);
      return;
    }
    case "pick": if (c) void pickAnswer(c.id); return;
    case "title": if (c) { if (arg) void update(c, { title: arg }, "Renamed"); else retitle(c); } return;
    case "export": if (c) void exportChat(c); return;
    case "search": location.hash = "#/chats"; findBox.value = arg; findBox.dispatchEvent(new Event("input")); return;
    case "find": if (arg) void ask<{ info: Chat }>({ t: "chat_find", question: arg }).then((r) => { if (r) location.hash = `#/c/${encodeURIComponent(r.info.id)}`; }); else findChats(); return;
    case "branch": pickBranch(id); return;
    case "cite": pickCitation(); return;
    case "incognito": {
      location.hash = "#/c/new";
      // Once the new chat's page is the one shown.
      let tries = 0;
      const tick = (): void => {
        if (page?.id.startsWith("new")) { page.incognito.checked = true; ui.render(); }
        else if (++tries < 60) requestAnimationFrame(tick);
      };
      requestAnimationFrame(tick);
      return;
    }
    default: ui.toast("/retry /edit [text] /compare [AGENTS…] [question] /pick /fork /side [question] /keep /agent NAME /tools NAME /folder DIR|none /task [PROJECT] [--agent A] [--name N] /title TEXT /search WORDS /find QUESTION /export /incognito /branch /cite");
  }
}

/// The agents a word of `/compare` names (`codex`, `codex@work`, `claude,codex@work,`), or null
/// when any part of it isn't one: then it is the question's.
function agentWords(word: string): string[] | null {
  const known = agentNames();
  const parts = word.split(",").filter(Boolean);
  return parts.length && parts.every((a) => known.includes(a.split("@")[0])) ? parts : null;
}

/// `/branch`: every choice at every point where the shown chat has others.
function pickBranch(id: string): void {
  if (!state.siblings.length) { ui.toast("One branch only: retry an answer or edit a message to make another"); return; }
  menu("Which branch?", state.siblings.flatMap((s) => s.ids.map((to, i) => ({
    title: tip("chat.branch"),
    label: `${i + 1}/${s.of}${sibLabel(s) && s.agents?.[i] ? ` (${s.agents[i]})` : ""} at message ${s.node.slice(-8)}`,
    hint: i + 1 === s.at ? "shown" : "show this branch",
    run: () => void ask({ t: "chat_branch", chat: id, to }),
  }))));
}

/// `/cite`: the citations in the shown chat's answers, to open one.
function pickCitation(): void {
  const re = /((?:[a-z0-9._-]+:)?c:[a-z0-9]{4,})\/([a-z0-9]{6,})/gi;
  const seen = new Map<string, string>();
  for (const it of state.items) {
    if (it.t !== "assistant" || typeof it.text !== "string") continue;
    for (const m of it.text.matchAll(re)) {
      const handle = m[1].replace(/^.*c:/, "");
      const c = [...state.list.values()].find((x) => x.handle === handle || x.id.endsWith(handle));
      if (c) seen.set(m[0], `#/c/${encodeURIComponent(c.id)}/${encodeURIComponent(m[2])}`);
    }
  }
  if (!seen.size) { ui.toast("No citations (c:7f3k/…) in this chat"); return; }
  menu("Open a citation", [...seen].map(([text, href]) => ({ title: tip("chat.cite"), label: text, run: () => { location.hash = href; } })));
}

function editMessage(c: Chat, it: Item): void {
  form("Say it differently", [{ key: "text", label: "The message (the old one and its answer stay as a branch)", type: "textarea", value: it.text as string, required: true }], "Send", async (v) => {
    await conn.request({ t: "chat_edit", chat: c.id, node: it.id, text: v.text });
  });
}

/// `‹ 2/3 ›` before a message that has others beside it.
function siblingBar(id: string, s: Siblings): HTMLElement {
  const go = (d: number) => () => void ask({ t: "chat_branch", chat: id, to: s.ids[(s.at - 1 + d + s.of) % s.of] });
  // A compare's answers are named by their agent; keeping one switches the chat to it.
  const who = sibLabel(s);
  return h("div", { class: "siblings", title: tip("chat.branch") },
    h("button", { class: "small", "aria-label": "Previous", onclick: go(-1) }, "‹"),
    h("span", {}, ` ${s.at}/${s.of}${who ? ` ${who}` : ""} `),
    h("button", { class: "small", "aria-label": "Next", onclick: go(1) }, "›"),
    who && canWrite() ? h("button", { class: "link", title: tip("chat.pick"), onclick: () => void pickAnswer(id, s.node) }, "Keep this answer") : null);
}

function chatItem(id: string, it: Item): HTMLElement | null {
  const c = state.list.get(id);
  const may = canWrite() && !!c;
  // An imported chat stays as its agent wrote it: forks only (a message makes one).
  const mine = may && !c!.source;
  switch (it.t) {
    case "user": return h("div", { class: "msg-wrap user" },
      h("div", { class: "msg user" }, it.text),
      may ? h("div", { class: "msg-tools" },
        mine ? h("button", { class: "link", title: tip("chat.edit"), onclick: () => editMessage(c!, it) }, "Edit") : null,
        !c!.incognito ? h("button", { class: "link", title: tip("chat.fork"), onclick: () => forkAt(c!, it.id) }, "Fork from here") : null) : null);
    case "assistant": return h("div", { class: "msg-wrap" },
      h("div", { class: "msg assistant" }, ...cited(it.text as string), it.streaming ? h("span", { class: "caret" }, "▍") : null),
      may && !it.streaming ? h("div", { class: "msg-tools" },
        mine ? h("button", { class: "link", title: tip("chat.retry"), onclick: () => void ask({ t: "chat_retry", chat: id, node: it.id }) }, "Retry") : null,
        !c!.incognito ? h("button", { class: "link", title: tip("chat.fork"), onclick: () => forkAt(c!, it.id) }, "Fork from here") : null) : null);
    case "thinking": return h("div", { class: "msg thinking" }, "· ", (it.text as string).split("\n")[0]);
    case "tool": {
      const status = it.status === "done" ? "done" : it.status === "failed" ? "failed" : it.status === "declined" ? "declined" : "running";
      const det = h("details", { class: `tool ${status}` }, h("summary", {}, `${it.name} · ${it.title}`));
      if (it.output) det.append(h("pre", {}, it.output));
      return det;
    }
    case "permission":
      if (it.decided) return h("div", { class: "turn-end" }, `${(it.title as string).split("\n")[0] || it.tool} → ${it.decided}`);
      return h("section", { class: "prompt" },
        h("b", {}, `Wants permission: ${it.tool}`),
        h("pre", {}, it.title),
        canWrite() ? h("div", { class: "actions", title: tip("chat.answer") },
          ...(["allow", "always", "deny"] as const).map((d) => h("button", { class: d, onclick: () => void ask({ t: "chat_answer", chat: id, decision: d }, d === "deny" ? "Denied" : "Allowed") }, d[0].toUpperCase() + d.slice(1)))) : null);
    case "notice": return h("div", { class: `msg thinking ${it.level}` }, "! ", it.text);
    case "turn_end": {
      const parts = [it.stop];
      if (it.cost_micro_usd != null) parts.push(`$${(it.cost_micro_usd / 1e6).toFixed(4)}`);
      if (it.duration_ms != null) parts.push(`${(it.duration_ms / 1000).toFixed(1)}s`);
      return h("div", { class: "turn-end" }, `── ${parts.join(" · ")} ──`);
    }
  }
  return null;
}

function drawItems(p: Page): void {
  const box = p.box;
  const atBottom = box.scrollHeight - box.scrollTop - box.clientHeight < 32;
  const top = box.scrollTop;
  const els = new Map<string, { key: string; el: HTMLElement }>();
  const kids: HTMLElement[] = [];
  for (const it of state.items) {
    const sib = state.siblings.find((s) => s.node === it.id);
    const key = JSON.stringify([it, sib, state.findIn]);
    const old = p.els.get(it.id);
    let el = old?.key === key ? old.el : chatItem(p.id, it);
    if (!el) continue;
    if (old?.key !== key && sib) el = h("div", {}, siblingBar(p.id, sib), el);
    if (old?.key !== key && state.findIn && typeof it.text === "string" && it.text.toLowerCase().includes(state.findIn.toLowerCase())) el.classList.add("found");
    if (old && old.el !== el && old.el instanceof HTMLDetailsElement && el instanceof HTMLDetailsElement) el.open = old.el.open;
    els.set(it.id, { key, el });
    kids.push(el);
  }
  p.els = els;
  patch(box, kids);
  // A message asked for (a search hit, a citation, a find): brought into view once.
  const focus = state.focus ? els.get(state.focus)?.el : null;
  if (focus) { state.focus = null; requestAnimationFrame(() => { focus.scrollIntoView({ block: "center" }); focus.classList.add("found"); }); }
  else if (atBottom) requestAnimationFrame(() => { box.scrollTop = box.scrollHeight; });
  else box.scrollTop = top;
}

/// The `#/c/<id>/<node>` address last acted on, so a render doesn't branch and scroll again.
let atRoute = "";

function chatView(rest: string): HTMLElement {
  if (!state.loaded) void loadChats();
  const isNew = rest === "new" || rest.startsWith("new/");
  // `#/c/<id>/<node>`: the chat, at that message (its branch shown first).
  const [rawId, rawNode] = isNew ? [rest, ""] : rest.split("/");
  const id = isNew ? `new${rest.slice(3)}` : decodeURIComponent(rawId);
  // Once per visit of that address (the chat may be open already: it still moves there).
  if (!isNew && rawNode && atRoute !== rest) {
    atRoute = rest;
    state.focus = decodeURIComponent(rawNode);
    void conn.request({ t: "chat_branch", chat: id, to: state.focus }).catch(() => {});
  }
  const p = pageFor(id);
  // A new chat follows nothing yet (and shows nothing of the one left).
  follow(isNew ? null : id);
  const c = isNew ? undefined : state.list.get(id) ?? state.archived?.find((x) => x.id === id);
  const pill: Child[] = c
    ? [`${c.agent}@${c.profile}`, ` · tools: ${c.tools}`,
      c.best_effort ? h("span", { class: "warn", title: "This agent can't enforce the preset by itself: ptyro's answer to each permission request stands behind it" }, " · best effort") : "",
      c.shell ? h("b", { class: "warn", title: "This chat's agent has a shell: it runs commands as you" }, " · ⚠ shell") : "",
      c.folders?.length ? h("span", { title: c.folders.join("\n") }, ` · ${c.folders.length} folder${c.folders.length === 1 ? "" : "s"}`) : "",
      c.status ? ` · ${c.status}` : "", c.cost_micro_usd ? ` · $${(c.cost_micro_usd / 1e6).toFixed(3)}` : "", ` · ${addr(c)}`]
    : ["A new chat: it can search the web, and it is kept."];
  const empty = isNew && !state.items.length
    ? h("div", { class: "empty" }, h("p", {}, h("b", {}, "Ask anything.")), h("p", { class: "sub" }, "Pick the agent and what it may use below, or keep the defaults."))
    : null;
  if (!isNew || state.items.length) drawItems(p);
  const stop = c && working(c) && canWrite() ? h("button", { class: "small", title: tip("chat.stop"), onclick: () => void ask({ t: "chat_stop", chat: c.id }, "Stopped") }, "Stop") : null;
  patch(p.el, [
    ui.header(c ? label(c) : "New chat", "#/chats", c ? h("button", { class: "icon", "aria-label": "More", onclick: () => chatMenu(c) }, "⋯") : null),
    h("div", { class: `pill ${c?.status ?? ""}` }, ...pill, c?.machine ? " · " : null, c?.machine ? machineChip(c.workspace) : null),
    ...(c?.attention ? [h("p", { class: "banner warn" }, c.attention.text)] : []),
    ...(c?.incognito ? [h("p", { class: "banner" }, "◌ Incognito: nothing of this chat is kept, and it ends with its agent.")] : []),
    ...(c?.source ? [h("p", { class: "banner" }, `⇣ ${importedFrom(c)}. It stays as ${c.source.split(":")[0]} wrote it: a message continues it as a fork.`)] : []),
    ...(c?.comparing ? [h("p", { class: "banner" }, compareLine(c.comparing), canWrite() ? h("button", { class: "small", title: tip("chat.stop"), onclick: () => void ask({ t: "chat_stop", chat: c.id }, "Stopped") }, " Stop") : null)] : []),
    ...(c?.side_of ? [h("p", { class: "banner" }, `↳ A side chat of “${label(state.list.get(c.side_of))}”: it never changes it, and goes when you leave, unless kept. `, canWrite() ? h("button", { class: "small", title: tip("chat.keep"), onclick: () => keepSide(c) }, "Keep it") : null)] : []),
    empty ?? p.box,
    ...(isNew ? [h("div", { class: "bar chat-pick" }, p.agent, p.preset, h("label", { class: "check", title: tip("chat.incognito") }, p.incognito, " incognito")), h("div", { class: "bar chat-pick" }, p.folders), p.compare] : []),
    canWrite() ? h("div", { class: "reply" }, p.reply, stop, h("button", { class: "send", title: tip("chat.send"), onclick: () => void send() }, "Send")) : h("p", { class: "sub" }, "This device may read chats, not write in them."),
  ].filter((x): x is HTMLElement => !!x));
  if (isNew) requestAnimationFrame(() => { if (document.activeElement !== p.reply) p.reply.focus(); });
  return p.el;
}

// ── wiring ──────────────────────────────────────────────────────────────────────────────────

conn.listeners.add((msg: any) => {
  if (msg.t === "connected") {
    void loadChats();
    // A fresh connection follows nothing: follow the open chat again.
    if (state.following) { const f = state.following; state.following = null; follow(f); }
    return;
  }
  if (msg.t === "chat_live") {
    if (msg.chat !== state.following) return;
    if (msg.reset) { state.items = []; state.siblings = (msg.siblings ?? []) as Siblings[]; }
    for (const it of msg.items as Item[]) {
      const i = state.items.findIndex((x) => x.id === it.id);
      if (i >= 0) state.items[i] = it; else state.items.push(it);
    }
    ui.render();
    return;
  }
  if (msg.t === "event") {
    const e = msg.event;
    if (e.t === "chat_updated") {
      const c = e.info as Chat;
      if (c.archived) state.list.delete(c.id); else state.list.set(c.id, c);
      ui.render();
    } else if (e.t === "chat_removed") {
      state.list.delete(e.id);
      ui.render();
    } else if (e.t === "machine_updated" && state.following) {
      // The open chat's machine came back: follow it again; went away: say so.
      const c = state.list.get(state.following);
      if (c?.machine === e.info.name) {
        if (e.info.state === "online") { const f = state.following; state.following = null; follow(f); }
        else if (e.info.state === "offline") { state.items.push({ id: `offline-${Date.now()}`, t: "notice", level: "warn", text: `${e.info.name} went offline; this chat carries on when it is back` }); ui.render(); }
      }
    }
  }
});

views.set("chats", () => listView());
views.set("c", (rest) => chatView(rest));
