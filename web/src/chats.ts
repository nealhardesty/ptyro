// Chats (SPEC §22.12, ROADMAP CHAT1.8): plain conversations with your agents, kept by ptyro.
// `#/chats` lists them (pinned first, then by day, searchable, one workspace's or all), `#/c/<id>`
// is one chat (its messages, tool calls and permission cards as they stream, a composer), and
// `#/c/new` starts one. Every control is an action below, as in the TUI and the CLI.

import { conn, host } from "./conn.js";
import { data, machineOf, wsLabel } from "./data.js";
import { machineChip } from "./machines.js";
import { actions, NAME_HINT, ago, ask, confirm, confirmed, download, errText, form, go, h, menu, patch, statusWord, tipOf, ui, views } from "./kit.js";
import type { Child } from "./kit.js";
import { atInsert, atQuery, chatsOff, folderList, itemKey, label, recentChats, shownChats, slashMessage, sorted, onMachine } from "./chatkit.js";
import { composing } from "./keys.js";

const ACTIONS = {
  "chats": "Your chats: pinned first, then the newest; search them, or start one",
  "chat.new": "A new chat with your agent: type, then send (it can search the web, and it is kept)",
  "chat.open": "Open a chat",
  "chat.send": "Send the message; the answer streams in (Enter sends, Shift+Enter starts a new line)",
  "chat.answer": "Answer what the chat's agent asks: allow once, always, or deny",
  "chat.stop": "Stop the answer being written",
  "chat.rename": "Rename the chat: its title, what people read (empty clears it)",
  "chat.autotitle": "A new automatic title for the chat, made from the conversation",
  "chat.pin": "Pin the chat to the top, or unpin it",
  "chat.archive": "Archive the chat (Archived shows them; again brings it back)",
  "chat.delete": "Delete the chat",
  "chat.agent": "Switch the chat to another agent: the next message carries the conversation over",
  "chat.folder": "Folders the chat may read (on the computer it runs on), or none",
  "chat.task": "Make it a task: an agent in a new worktree carries out the chat's last answer",
  "chat.export": "Download the chat as Markdown, or as JSON (every message, with its ids)",
  "chat.machine": "Only one machine's chats (`chat ls --machine`)",
  "chat.search": "Search every chat: titles at once, and from 3 letters every message on every machine (\"phrases\", prefix*, -word, agent:codex, since:7d, by:meaning; ≈ marks a message found by meaning)",
  "chat.meaning": "Switch meaning search on: also find messages by what they are about, not only their words (downloads a 31 MB model once)",
  "chat.archived": "Show archived chats instead",
  "chat.retry": "Ask again: a new answer replaces the last one",
  "chat.edit": "Say your last message differently: it and its answer are replaced",
  "chat.incognito": "Incognito: nothing of this chat is kept or searchable, and it ends with its agent",
  "chat.find": "Ask your chats a question: an agent searches them and answers, citing where",
  "chat.find_in": "Find in this chat",
  "chat.cite": "Open the message this citation points to",
  "chat.pinned": "Show only the pinned chats, or every chat again",
  "chat.on": "Switch chats on (`[chat] enabled`, in your config, as Settings → Chat would)",
  "chat.recent": "Your pinned chats and the newest (Settings → Chat → recent says how many)",
  "chat.folder_at": "@ in the box: the chat's folders, to name one (Tab or Enter puts it in)",
} as const;
const tip = actions(ACTIONS);

export interface Chat {
  id: string; handle: string; workspace: string; machine?: string | null; title?: string | null; title_src: string;
  agent: string; profile: string; tools: string; pinned: boolean; archived: boolean; incognito: boolean;
  leaf?: string | null; nodes: number; unread: number; created_ms: number; last_ms: number; cost_micro_usd: number;
  status?: string | null; attention?: { kind: string; text: string; since_ms: number } | null; engine?: string | null;
  folders?: string[]; best_effort?: boolean;
}

interface Hit { chat: Chat; node?: string | null; kind: string; snippet: string; at_ms: number; rank: number; how?: string }
interface Item { id: string; t: string; [k: string]: any }

const state = {
  list: new Map<string, Chat>(),
  archived: null as Chat[] | null,
  loaded: false,
  /// `[chat] enabled = false`: the daemon says why, and the Chats tab goes.
  off: "",
  /// The search box's text, as typed.
  query: "",
  workspace: "",
  /// The chat followed now, and its items.
  following: null as string | null,
  items: [] as Item[],
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
  /// Only pinned chats (`chat ls --pinned`).
  pinned: false,
  /// Only one machine's (`chat ls --machine`): "" every one, "." this one.
  machine: "",
  /// Chats opened that the list doesn't hold (archived, from a link or a citation), by id.
  seen: new Map<string, Chat>(),
  /// The chat shown was deleted (elsewhere, or incognito and its agent ended).
  gone: "",
  /// `[chat] recent`: how many unpinned chats the compact CHATS section shows.
  recent: 5,
};

const addr = (c: Chat): string => `c:${c.handle}`;
const working = (c: Chat): boolean => c.status === "working" || c.status === "starting";
/// A chat by id: listed, archived, or opened from a link.
const chatOf = (id: string | null | undefined): Chat | undefined => (id ? state.list.get(id) ?? state.seen.get(id) ?? state.archived?.find((x) => x.id === id) : undefined);
const glyph = (c: Chat): string => c.attention || c.status === "blocked" ? "⚠" : working(c) ? "●" : c.status === "errored" ? "✗" : c.incognito ? "◌" : c.pinned ? "★" : "·";

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
    // One the list doesn't hold (archived, another machine's): by its address, read when opened.
    out.push(h("a", { href: `#/c/${encodeURIComponent(c?.id ?? m[1])}/${encodeURIComponent(m[2])}`, title: tip("chat.cite") }, m[0]));
    at = i + m[0].length;
  }
  out.push(text.slice(at));
  return out;
}

/// Whether the Chats tab shows (chats are on, as far as the daemon has said).
export function chatsShown(): boolean {
  return !state.off;
}

let retryTimer = 0;
export async function loadChats(): Promise<void> {
  clearTimeout(retryTimer);
  try {
    const r = await conn.request({ t: "chat_list", all: true });
    state.list = new Map((r.chats as Chat[]).map((c) => [c.id, c]));
    state.off = "";
    // `[chat] incognito`: whether a new chat starts incognito.
    incognitoDefault = !!r.incognito;
    // `[chat] recent`, where this device may read the config.
    if (conn.scope === "full") {
      const v = await host("settings.view", { scope: "user" }).catch(() => null);
      const n = Number(v?.entries?.find((x: { key: string }) => x.key === "chat.recent")?.value);
      if (Number.isFinite(n) && n >= 0) state.recent = n;
    }
  } catch (e) {
    // Only chats switched off hide the tab; anything else (a slow daemon, a dropped connection)
    // is tried again.
    if (chatsOff(e)) state.off = "Chats are switched off.";
    else retryTimer = window.setTimeout(() => { if (conn.state === "open") void loadChats(); }, 3000);
  }
  state.loaded = true;
  ui.render();
}

/// `[chat] enabled = true`, in your config (as the settings panel would); the daemon's reload
/// brings the tab back.
async function chatsOn(): Promise<void> {
  try {
    await host("settings.apply", { scope: "user", change: { t: "set", key: "chat.enabled", value: "true" } });
    ui.toast("Chats are on");
    await loadChats();
  } catch (e) { ui.toast(errText(e)); }
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
  state.query = findBox.value.trim();
  const l = document.querySelector<HTMLElement>("main.chats .rows"); if (l) patch(l, rows());
  // Every message, on every machine, from 3 letters on (a shorter prefix matches everything).
  clearTimeout(searchTimer);
  searchTimer = window.setTimeout(() => void searchMessages(findBox.value.trim()), 250);
});

async function searchMessages(q: string): Promise<void> {
  if ([...q].length < 3) { state.hits = []; state.hitsFor = ""; ui.render(); return; }
  try {
    const r = await conn.request({ t: "chat_search", query: q, workspace: state.workspace || null, archived: !!state.archived, pinned: state.pinned, limit: 30, machines: true });
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

/// The list's filters changed: the message search answers them too.
function filtersChanged(): void {
  if ([...state.query].length >= 3) void searchMessages(state.query);
  ui.render();
}

function hitRows(): Node[] {
  if (!state.query || [...state.query].length < 3) return [];
  if (state.meaningOff) return [h("p", { class: "sub" }, `${state.meaningOff}. `, h("button", { class: "small", title: tip("chat.meaning"), onclick: () => void meaningOn() }, "Switch it on"))];
  if (!state.hits.length) return state.hitsFor ? [h("p", { class: "sub" }, "No message matches.")] : [];
  const hits = state.hits.filter((x) => onMachine(x.chat, state.machine));
  if (!hits.length) return [h("p", { class: "sub" }, "No message matches.")];
  return [h("h3", { class: "day" }, `Messages (${hits.length})`), ...hits.map((x) =>
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

function rows(): Node[] {
  const source = state.archived ?? sorted(state.list.values());
  const q = state.query;
  const shown = shownChats(source, { query: q, workspace: state.workspace, pinned: state.pinned, machine: state.machine });
  if (!shown.length && q) return [h("p", { class: "sub" }, `No chat titled “${q}”.`), ...hitRows()];
  if (!shown.length) {
    return [h("div", { class: "empty" },
      h("p", {}, state.archived ? "No archived chats." : state.pinned ? "No pinned chats: ⋯ → Pin keeps one at the top." : "No chats yet. Ask anything: it can search the web, and it is kept here."),
      state.archived || state.pinned || !canWrite() ? null : h("button", { class: "primary", title: tip("chat.new"), onclick: () => newChat() }, "Start a chat"))];
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
        c.machine ? machineChip(c.workspace) : null,
        c.unread ? h("b", { class: "unread-dot", "aria-label": "unread" }, "●") : null),
      h("button", { class: "icon", "aria-label": "More", onclick: () => chatMenu(c) }, "⋯")));
  }
  return [...out, ...hitRows()];
}

function listView(): HTMLElement {
  if (!state.loaded) void loadChats();
  if (state.off) {
    return h("div", {}, ui.header("Chats", false), h("main", { class: "center" }, h("p", {}, state.off),
      conn.scope === "full" ? h("button", { class: "primary", title: tip("chat.on"), onclick: () => void chatsOn() }, "Switch chats on") : h("p", { class: "sub" }, "A device paired with full access (or Settings → Chat on the computer) can switch them on.")));
  }
  if (findBox.value.trim() !== state.query && document.activeElement !== findBox) findBox.value = state.query;
  const wsSelect = h("select", { "aria-label": "Workspace", onchange: (e: Event) => { state.workspace = (e.target as HTMLSelectElement).value; filtersChanged(); } },
    h("option", { value: "" }, "Every workspace"),
    ...[...data.workspaces.values()].map((w) => h("option", { value: w.name }, wsLabel(w.name))));
  wsSelect.value = state.workspace;
  // Which machine's, once chats of more than one are listed (`chat ls --machine`).
  const machines = [...new Set([...state.list.values()].map((c) => c.machine).filter((m): m is string => !!m))].sort();
  const machineSelect = machines.length || state.machine ? h("select", { "aria-label": "Machine", title: tip("chat.machine"), onchange: (e: Event) => { state.machine = (e.target as HTMLSelectElement).value; filtersChanged(); } },
    h("option", { value: "" }, "Every machine"), h("option", { value: "." }, "This machine"),
    ...machines.map((m) => h("option", { value: m }, m))) : null;
  if (machineSelect) (machineSelect as HTMLSelectElement).value = state.machine;
  const archived = h("button", { class: `small${state.archived ? " on" : ""}`, title: tip("chat.archived"), onclick: () => void toggleArchived() }, state.archived ? "Back to chats" : "Archived");
  const pinned = h("button", { class: `small${state.pinned ? " on" : ""}`, title: tip("chat.pinned"), "aria-pressed": String(state.pinned), onclick: () => { state.pinned = !state.pinned; filtersChanged(); } }, "★ Pinned");
  return h("div", {},
    ui.header("Chats", false,
      canWrite() ? h("button", { class: "icon", title: tip("chat.find"), "aria-label": "Ask your chats", onclick: () => findChats() }, "？") : null,
      canWrite() ? h("button", { class: "icon plus", title: tip("chat.new"), "aria-label": "New chat", onclick: () => newChat() }, "＋") : null),
    h("div", { class: "searchbar" }, findBox, wsSelect, machineSelect, pinned, archived),
    h("main", { class: "list chats", title: tip("chats") }, h("div", { class: "rows" }, ...rows())));
}

/// Send `req`; when the daemon asks first (the chat's conversation would go to another
/// profile's account, SPEC §22.7), show its question and send it again, agreed to.
async function agreed<T = any>(req: Record<string, unknown>): Promise<T> {
  try {
    return await conn.request(req) as T;
  } catch (e) {
    if ((e as { code?: string }).code !== "needs_confirmation") throw e;
    if (!(await confirmed("Another profile?", errText(e), "Go ahead", false))) throw new Error("cancelled");
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
    if ((e as Error).message !== "cancelled") ui.fail(errText(e));
    return undefined;
  }
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
  if (state.archived) { state.archived = null; filtersChanged(); return; }
  const r = await ask<{ chats: Chat[] }>({ t: "chat_list", all: true, archived: true });
  state.archived = r ? sorted(r.chats) : [];
  filtersChanged();
}

/// `C-g`: the search box over every chat.
export function findAllChats(): void {
  if (location.hash !== "#/chats") location.hash = "#/chats";
  setTimeout(() => findBox.focus(), 50);
}

/// `chat new` (`m`): a new chat, in the workspace the list shows.
export function newChat(): void {
  location.hash = `#/c/new${state.workspace ? `/${encodeURIComponent(state.workspace)}` : ""}`;
}

/// Whether a chat reads the folders it is given: every chat does, but one made before 2026-11-17
/// with no tools or a preset of the person's own (`Chat::tools_for`, SPEC §22.5).
const reads = (c: Chat): boolean => c.tools === "chat" || c.tools === "read";
const NOT_READ = "this chat was made without tools, so it reads no files: a new chat with the folder does";

function update(c: Chat, change: object, ok: string): Promise<unknown> {
  // Another profile's agent asks first (the conversation goes to that account).
  return askAgreed({ t: "chat_update", chat: c.id, ...change }, ok);
}

function chatMenu(c: Chat): void {
  const archived = c.archived || !!state.archived?.some((x) => x.id === c.id);
  const open = { title: tip("chat.open"), label: "Open", run: () => { location.hash = `#/c/${encodeURIComponent(c.id)}`; } };
  const md = { title: tip("chat.export"), label: "Download as Markdown", run: () => void exportChat(c) };
  const js = { title: tip("chat.export"), label: "Download as JSON", hint: "every message, with its ids", run: () => void exportChat(c, "json") };
  // A device that may only read gets what reads (SPEC §22.11).
  const find = { title: tip("chat.find_in"), label: "Find in this chat", run: () => findInChat(c) };
  if (!canWrite()) { menu(label(c), [open, find, md, js]); return; }
  menu(label(c), [
    open,
    find,
    { title: tip("chat.rename"), label: "Rename", run: () => retitle(c) },
    { title: tip("chat.autotitle"), label: "New automatic title", hint: "made from the conversation", run: () => update(c, { title: "", auto_title: true }, "A new title is on its way") },
    { title: tip("chat.pin"), label: c.pinned ? "Unpin" : "Pin", run: () => update(c, { pinned: !c.pinned }, c.pinned ? "Unpinned" : "Pinned") },
    { title: tip("chat.agent"), label: "Switch agent", hint: `now ${c.agent}`, run: () => pickAgent(c) },
    { title: tip("chat.folder"), label: "Folders", hint: c.folders?.length ? c.folders.join(", ") : "none", run: () => editFolders(c) },
    !c.incognito && { title: tip("chat.task"), label: "Make it a task", hint: "an agent carries it out in a new worktree", run: () => makeTask(c) },
    md,
    js,
    { title: tip("chat.archive"), label: archived ? "Bring back from the archive" : "Archive", run: async () => {
      await update(c, { archived: !archived }, archived ? "Back from the archive" : "Archived");
      if (state.archived) state.archived = state.archived.filter((x) => x.id !== c.id);
      if (state.following === c.id && !archived) location.hash = "#/chats";
      ui.render();
    } },
    { title: tip("chat.delete"), label: "Delete", danger: true, run: () => confirm(`Delete the chat “${label(c)}”?`, "It goes from every list, on every device. This can't be undone.", "Delete", async () => {
      await ask({ t: "chat_delete", chat: c.id, confirmed: true }, "Deleted");
      if (state.archived) state.archived = state.archived.filter((x) => x.id !== c.id);
      if (state.following === c.id) location.hash = "#/chats";
    }) },
  ]);
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
  form("Rename the chat", [{ key: "title", label: "Title", value: c.title ?? "", hint: "Empty clears it (New automatic title makes one)." }], "Save", async (v) => {
    await conn.request({ t: "chat_update", chat: c.id, title: (v.title as string) ?? "" });
    ui.toast("Renamed");
  });
}

function agentNames(): string[] {
  const names = ["claude", "codex", "opencode"];
  for (const a of data.agents) if (!names.includes(a.name)) names.push(a.name);
  return names.filter((n) => data.agents.length === 0 || data.agents.some((a) => a.name === n && a.installed));
}

function pickAgent(c: Chat): void {
  // Each with where ptyro found it (an npx cache, nvm…), so a surprising copy is visible.
  const where = (a: string) => data.agents.find((x) => x.name === a)?.found ?? "";
  menu("Which agent? The conversation carries over", [
    ...agentNames().map((a) => ({ title: tip("chat.agent"), label: a, hint: [a === c.agent ? "now" : "", where(a)].filter(Boolean).join(" · "), run: () => update(c, { agent: a }, `The next message goes to ${a}`) })),
    // Another one is a tap away (BOOT9), on a device that can install.
    ...(conn.scope === "full" ? [{ title: tipOf("setup.offer"), label: "Install another agent…", hint: "The setup checklist", run: () => go("#/setup") }] : []),
  ]);
}

/// `[chat] incognito`: a new chat starts incognito (its box ticked; unticked, it is kept).
let incognitoDefault = false;
function editFolders(c: Chat): void {
  form("Folders this chat may read", [{ key: "f", label: "One a line (empty: none)", type: "textarea", rows: 3, placeholder: "~/notes", value: (c.folders ?? []).join("\n") }], "Save", async (v) => {
    // Emptied, the box is left out of the values: no folders.
    const f = folderList(String(v.f ?? ""));
    await conn.request({ t: "chat_update", chat: c.id, folders: f });
    ui.toast(f.length && !reads(c) ? `Saved, but ${NOT_READ}` : "Saved: the next message starts it with them");
  }, { intro: "On the computer the chat runs on. It reads them, and no other files." });
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
    { key: "name", label: "The worktree's name (optional)", placeholder: "from the chat's title", slug: "name", hint: NAME_HINT },
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

async function exportChat(c: Chat, format: "md" | "json" = "md"): Promise<void> {
  const r = await ask<{ text: string }>({ t: "chat_export", chat: c.id, format });
  if (r) download(`${label(c).replace(/[^\w.-]+/g, "-").slice(0, 60) || c.handle}.${format}`, r.text, format === "json" ? "application/json" : "text/markdown");
}

// ── one chat ────────────────────────────────────────────────────────────────────────────────

/// The page's composer and transcript stay in the DOM across renders, so a draft, the caret
/// and the scroll position survive the events that re-render everything around them.
interface Page { id: string; el: HTMLElement; box: HTMLElement; reply: HTMLTextAreaElement; els: Map<string, { key: string; el: HTMLElement }>; agent: HTMLSelectElement; incognito: HTMLInputElement; folders: HTMLInputElement; title: HTMLInputElement; workspace: HTMLSelectElement; at: HTMLElement; sending: boolean }
let page: Page | null = null;
const drafts = new Map<string, string>();

function canWrite(): boolean {
  return conn.scope === "control" || conn.scope === "full";
}

function follow(id: string | null): void {
  if (state.following === id) return;
  state.gone = "";
  if (state.following) conn.send({ t: "chat_unsubscribe", chat: state.following });
  state.following = id;
  state.items = [];
  if (id) conn.send({ t: "chat_subscribe", chat: id });
}
window.addEventListener("hashchange", () => {
  if (!/^#\/c\//.test(location.hash)) follow(null);
  if (location.hash !== `#/c/${atRoute}`) atRoute = "";
  state.findIn = "";
});

function pageFor(id: string): Page {
  if (page?.id === id) return page;
  const reply = h("textarea", { rows: 2, enterkeyhint: "send", placeholder: "Ask anything…", title: tip("chat.send") });
  reply.value = drafts.get(id) ?? "";
  // `@`: the chat's folders, to name one in the message.
  const at = h("div", { class: "at-list", role: "listbox", title: tip("chat.folder_at") });
  const folderNames = (): string[] => (id.startsWith("new") ? folderList(page?.folders.value ?? "") : chatOf(id)?.folders ?? []);
  const atNow = () => atQuery(reply.value, reply.selectionStart ?? reply.value.length, folderNames());
  const putAt = (f: string): void => {
    const q = atNow();
    if (!q) return;
    const r = atInsert(reply.value, reply.selectionStart ?? reply.value.length, q.start, f);
    reply.value = r.text; reply.setSelectionRange(r.caret, r.caret); drafts.set(id, reply.value);
    showAt(); reply.focus();
  };
  const showAt = (): void => {
    const q = atNow();
    at.replaceChildren(...(q?.matches ?? []).map((f) => h("button", { class: "small", role: "option", title: tip("chat.folder_at"), onmousedown: (e: Event) => e.preventDefault(), onclick: () => putAt(f) }, f)));
    at.hidden = !q?.matches.length;
  };
  at.hidden = true;
  reply.addEventListener("input", () => { drafts.set(id, reply.value); showAt(); });
  reply.addEventListener("blur", () => { at.hidden = true; });
  reply.addEventListener("keydown", (e) => {
    // Tab or Enter puts the first folder in; Esc closes the list.
    const q = at.hidden ? null : atNow();
    if (q?.matches.length && (e.key === "Tab" || (e.key === "Enter" && !e.shiftKey)) && !composing(e)) { e.preventDefault(); putAt(q.matches[0]); return; }
    if (q && e.key === "Escape") { at.hidden = true; e.preventDefault(); return; }
    if (e.key === "Enter" && !e.shiftKey && !composing(e)) { e.preventDefault(); void send(); }
  });
  const agent = h("select", { "aria-label": "Agent", title: tip("chat.agent") }, h("option", { value: "" }, "default agent"), ...agentNames().map((a) => h("option", { value: a }, a)));
  const incognito = h("input", { type: "checkbox", title: tip("chat.incognito") });
  incognito.checked = incognitoDefault;
  const folders = h("input", { type: "text", placeholder: "Folders it may read (optional), e.g. ~/notes", "aria-label": "Folders", title: tip("chat.folder"), autocapitalize: "off", spellcheck: "false" });
  // `chat new --title`, and the workspace it belongs to (`--workspace`; the list's, or the address's).
  const title = h("input", { type: "text", placeholder: "Title (optional: one is made after the first answer)", "aria-label": "Title", title: tip("chat.rename") });
  const ws = decodeURIComponent(id.split("/")[1] ?? "") || state.workspace;
  const workspace = h("select", { "aria-label": "Workspace", title: "The workspace the chat belongs to" }, h("option", { value: "" }, "main workspace"),
    ...[...data.workspaces.values()].filter((w) => w.name !== "main").map((w) => h("option", { value: w.name }, wsLabel(w.name))));
  workspace.value = ws === "main" ? "" : ws;
  page = { id, el: h("div", { class: "session chat-page" }), box: h("div", { class: "chat" }), reply, els: new Map(), agent, incognito, folders, title, workspace, at, sending: false };
  // The keyboard goes to the composer once, as the page opens (not on every render, which
  // would pull it out of the Folders box or a sheet); on a phone only for a new chat, where
  // typing is the point, rather than popping the keyboard over a chat being read.
  requestAnimationFrame(() => {
    if (page?.reply === reply && reply.isConnected && (id.startsWith("new") || matchMedia("(pointer: fine)").matches)) reply.focus();
  });
  // A new chat's page starts empty (not with what a failed try left).
  if (id.startsWith("new")) state.items = [];
  return page;
}

async function send(): Promise<void> {
  const p = page;
  if (!p) return;
  let text = p.reply.value;
  if (!text.trim()) return;
  const literal = slashMessage(text);
  if (text.trim().startsWith("/") && literal === null) { p.reply.value = ""; drafts.delete(p.id); slash(p.id, text.trim()); return; }
  text = literal ?? text;
  // One at a time: a second Send while the first is on its way would send it twice.
  if (p.sending) { ui.toast("One moment: the last message is on its way"); return; }
  p.sending = true;
  try {
    if (p.id.startsWith("new")) {
      state.items = [{ id: "first", t: "user", text }];
      ui.render();
      let r;
      try {
        r = await conn.request({ t: "chat_new", workspace: p.workspace.value || undefined, agent: p.agent.value || null, title: p.title.value.trim() || null, prompt: text, incognito: p.incognito.checked, folders: folderList(p.folders.value) });
      } catch (e) {
        // Not made: the message goes back to being a draft, not a sent one.
        state.items = [];
        ui.render();
        throw e;
      }
      p.reply.value = ""; drafts.delete(p.id);
      p.sending = false;
      const info = r.info as Chat;
      state.list.set(info.id, info);
      // The same page, now the chat's own.
      location.replace(`#/c/${encodeURIComponent(info.id)}`);
    } else {
      await conn.request({ t: "chat_send", chat: p.id, text });
      if (p.reply.value === text) { p.reply.value = ""; drafts.delete(p.id); }
    }
  } catch (e) { ui.fail(errText(e)); }
  p.sending = false;
}

/// The composer's slash commands (SPEC §22.12), as in the TUI.
function slash(id: string, text: string): void {
  const [name, ...rest] = text.slice(1).split(" ");
  const arg = rest.join(" ").trim();
  const c = chatOf(id);
  const lastUser = [...state.items].reverse().find((x) => x.t === "user");
  const needsChat = ["retry", "edit", "agent", "title", "export", "cite", "task"];
  if (!c && needsChat.includes(name)) { ui.toast(`/${name} works in a chat: send a first message`); return; }
  if (name === "edit" && !lastUser) { ui.toast("No message to edit yet"); return; }
  switch (name) {
    case "retry": if (c) void ask({ t: "chat_retry", chat: c.id }); return;
    case "edit": if (c && lastUser) { if (arg) void ask({ t: "chat_edit", chat: c.id, node: lastUser.id, text: arg }); else editMessage(c, lastUser); } return;
    case "agent": if (c) { if (arg) void update(c, { agent: arg }, `The next message goes to ${arg}`); else pickAgent(c); } return;
    case "folder": case "folders": {
      const none = arg === "none" || arg === "-";
      if (!c) {
        if (!page) return;
        // No folder named: the box, to type them in.
        if (!arg) { page.folders.focus(); ui.toast(page.folders.value ? `It may read ${page.folders.value}` : "/folder DIR lets it read a folder (/folder none: none)"); return; }
        page.folders.value = none ? "" : [page.folders.value, arg].filter(Boolean).join(", ");
        ui.toast(none ? "No folders" : `It may read ${arg}`);
        return;
      }
      if (!arg) { editFolders(c); return; }
      const next = none ? [] : [...new Set([...(c.folders ?? []), arg])];
      void update(c, { folders: next }, none ? "No folders" : reads(c) ? `It may read ${arg} from the next message on` : `${arg} attached, but ${NOT_READ}`);
      return;
    }
    case "task": if (c) makeTask(c, arg); return;
    case "title": if (c) { if (arg) void update(c, { title: arg }, "Renamed"); else retitle(c); } return;
    case "export": if (c) void exportChat(c); return;
    case "search": location.hash = "#/chats"; findBox.value = arg; findBox.dispatchEvent(new Event("input")); return;
    case "find": if (arg) void ask<{ info: Chat }>({ t: "chat_find", question: arg }).then((r) => { if (r) location.hash = `#/c/${encodeURIComponent(r.info.id)}`; }); else findChats(); return;
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
    default:
      if (name !== "help" && name !== "?") {
        // Not lost: back in the composer, to fix or to send as a message.
        if (page?.id === id) { page.reply.value = text; drafts.set(id, text); }
        ui.toast(`/${name}: not a command (start with // to send it as a message). /help lists them`);
        return;
      }
      ui.toast("/retry /edit [text] /agent NAME /folder DIR|none /task [PROJECT] [--agent A] [--name N] /title TEXT /search WORDS /find QUESTION /export /incognito /cite");
  }
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
      seen.set(m[0], `#/c/${encodeURIComponent(c?.id ?? m[1])}/${encodeURIComponent(m[2])}`);
    }
  }
  if (!seen.size) { ui.toast("No citations (c:7f3k/…) in this chat"); return; }
  menu("Open a citation", [...seen].map(([text, href]) => ({ title: tip("chat.cite"), label: text, run: () => { location.hash = href; } })));
}

function editMessage(c: Chat, it: Item): void {
  form("Say it differently", [{ key: "text", label: "Your last message (it and its answer are replaced)", type: "textarea", value: it.text as string, required: true }], "Send", async (v) => {
    await conn.request({ t: "chat_edit", chat: c.id, node: it.id, text: v.text });
  });
}

/// `last`: your last message, or the answer to it: only the last turn is retried or edited.
function chatItem(id: string, it: Item, last: boolean): HTMLElement | null {
  const c = chatOf(id);
  const may = canWrite() && !!c && last;
  switch (it.t) {
    case "user": return h("div", { class: "msg-wrap user" },
      h("div", { class: "msg user" }, it.text),
      may ? h("div", { class: "msg-tools" },
        h("button", { class: "link", title: tip("chat.edit"), onclick: () => editMessage(c!, it) }, "Edit")) : null);
    case "assistant": return h("div", { class: "msg-wrap" },
      h("div", { class: "msg assistant" }, ...cited(it.text as string), it.streaming ? h("span", { class: "caret" }, "▍") : null),
      may && !it.streaming ? h("div", { class: "msg-tools" },
        h("button", { class: "link", title: tip("chat.retry"), onclick: () => void ask({ t: "chat_retry", chat: id }) }, "Retry")) : null);
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
          // This card, once: a second tap (or a card answered elsewhere) never decides the next one.
          ...(["allow", "always", "deny"] as const).map((d) => h("button", { class: d, onclick: (e: Event) => { for (const b of (e.currentTarget as HTMLElement).parentElement!.querySelectorAll("button")) b.disabled = true; void ask({ t: "chat_answer", chat: id, decision: d, node: it.id }, d === "deny" ? "Denied" : d === "always" ? "Always allowed" : "Allowed"); } }, d[0].toUpperCase() + d.slice(1)))) : null);
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
  // What around a message changes its buttons and links: the chat's own state, citations resolvable.
  const c = chatOf(p.id);
  const extra = c ? `${canWrite()}${c.incognito}${state.list.size}` : `${canWrite()}`;
  // The last turn: your last message, and the answer after it (Edit and Retry go under them).
  const u = state.items.map((x) => x.t).lastIndexOf("user");
  const lastUser = u >= 0 ? state.items[u].id : "";
  const a = state.items.map((x) => x.t).lastIndexOf("assistant");
  const lastAnswer = a > u ? state.items[a].id : "";
  for (const it of state.items) {
    const last = it.id === lastUser || it.id === lastAnswer;
    const key = itemKey(it, state.findIn, `${extra}${last ? "L" : ""}`);
    const old = p.els.get(it.id);
    const el = old?.key === key ? old.el : chatItem(p.id, it, last);
    if (!el) continue;
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

/// The `#/c/<id>/<node>` address last acted on, so a render doesn't scroll to it again.
let atRoute = "";

function chatView(rest: string): HTMLElement {
  if (!state.loaded) void loadChats();
  const isNew = rest === "new" || rest.startsWith("new/");
  // `#/c/<id>/<node>`: the chat, at that message.
  const [rawId, rawNode] = isNew ? [rest, ""] : rest.split("/");
  const id = isNew ? `new${rest.slice(3)}` : decodeURIComponent(rawId);
  // Once per visit of that address (the chat may be open already: it still moves there).
  if (!isNew && rawNode && atRoute !== rest) {
    atRoute = rest;
    state.focus = decodeURIComponent(rawNode);
  }
  const p = pageFor(id);
  // A new chat follows nothing yet (and shows nothing of the one left).
  follow(isNew ? null : id);
  const c = isNew ? undefined : chatOf(id);
  // Not in the list (archived, or opened from a link before the list came): asked for itself.
  if (!isNew && !c && !state.gone) void fetchChat(id);
  const gone = !isNew && state.gone === id;
  const pill: Child[] = c
    ? [`${c.agent}@${c.profile}`,
      c.best_effort ? h("span", { class: "warn", title: "This agent can't keep the chat to its tools by itself: ptyro's answer to each permission request stands behind it" }, " · best effort") : "",
      c.folders?.length ? h("span", { title: c.folders.join("\n") }, ` · ${c.folders.length} folder${c.folders.length === 1 ? "" : "s"}${reads(c) ? "" : " (not read: made without tools)"}`) : "",
      c.status ? ` · ${statusWord(c.status)}` : "", c.cost_micro_usd ? ` · $${(c.cost_micro_usd / 1e6).toFixed(3)}` : "", ` · ${addr(c)}`]
    : [isNew ? "A new chat: it can search the web, and it is kept." : gone ? "" : "Loading the chat…"];
  const empty = isNew && !state.items.length
    ? h("div", { class: "empty" }, h("p", {}, h("b", {}, "Ask anything.")), h("p", { class: "sub" }, "Pick the agent below, or keep the defaults."))
    : null;
  if (!isNew || state.items.length) drawItems(p);
  const stop = c && !gone && working(c) && canWrite() ? h("button", { class: "small", title: tip("chat.stop"), onclick: () => void ask({ t: "chat_stop", chat: c.id }, "Stopped") }, "Stop") : null;
  patch(p.el, [
    ui.header(c ? label(c) : isNew ? "New chat" : "Chat", "#/chats", c && !gone ? h("button", { class: "icon", "aria-label": "More", onclick: () => chatMenu(c) }, "⋯") : null),
    h("div", { class: `pill ${c?.status ?? ""}` }, ...pill, c?.machine ? " · " : null, c?.machine ? machineChip(c.workspace) : null),
    ...(c?.attention ? [h("p", { class: "banner warn" }, c.attention.text)] : []),
    ...(c?.incognito ? [h("p", { class: "banner" }, "◌ Incognito: nothing of this chat is kept, and it ends with its agent.")] : []),
    ...(gone ? [h("p", { class: "banner warn" }, "This chat was deleted (elsewhere, or it was incognito and its agent ended). What was here stays until you leave. ", h("a", { class: "small", href: "#/chats", title: tip("chats") }, "Back to the chats"))] : []),
    ...(c?.archived && !gone ? [h("p", { class: "banner" }, "Archived: it is kept, out of the list. ", canWrite() ? h("button", { class: "small", title: tip("chat.archive"), onclick: () => void update(c, { archived: false }, "Back from the archive") }, "Bring it back") : null)] : []),
    empty ?? p.box,
    ...(isNew ? [h("div", { class: "bar chat-pick" }, p.agent, h("label", { class: "check", title: tip("chat.incognito") }, p.incognito, " incognito")), h("div", { class: "bar chat-pick" }, p.title, p.workspace), h("div", { class: "bar chat-pick" }, p.folders)] : []),
    gone ? null : canWrite() ? h("div", { class: "reply" }, p.at, p.reply, stop, h("button", { class: "send", title: tip("chat.send"), onclick: () => void send() }, "Send")) : h("p", { class: "sub" }, "This device may read chats, not write in them."),
  ].filter((x): x is HTMLElement => !!x));
  return p.el;
}

/// A chat the list doesn't hold, read for itself (once at a time); not there at all: gone.
const fetching = new Set<string>();
async function fetchChat(id: string): Promise<void> {
  if (fetching.has(id) || conn.state !== "open") return;
  fetching.add(id);
  try {
    const r = await conn.request({ t: "chat_read", chat: id });
    const info = r.info as Chat;
    state.seen.set(info.id, info);
    // Opened by its address (`c:7f3k`, a citation): the page is the chat's own id's.
    if (info.id !== id) location.replace(location.hash.replace(`#/c/${encodeURIComponent(id)}`, `#/c/${encodeURIComponent(info.id)}`));
  } catch (e) {
    if ((e as { code?: string }).code === "not_found") state.gone = id;
    else { setTimeout(() => fetching.delete(id), 3000); ui.render(); return; }
  }
  fetching.delete(id);
  ui.render();
}

/// The compact CHATS section beside the sessions on a wide screen: the pinned chats and the
/// newest `[chat] recent`, as the TUI's sidebar shows them.
export function chatsAside(): HTMLElement | null {
  if (state.off || !state.loaded) { if (!state.loaded) void loadChats(); return null; }
  const list = recentChats(state.list.values(), state.recent);
  const all = [...state.list.values()].filter((c) => !c.incognito).length;
  return h("aside", { class: "chats-aside", title: tip("chat.recent") },
    h("h3", { class: "day" }, "Chats", canWrite() ? h("button", { class: "icon plus", title: tip("chat.new"), "aria-label": "New chat", onclick: () => newChat() }, "＋") : null),
    ...list.map((c) => h("a", { class: `chat-row${c.unread ? " unread" : ""}${c.attention ? " blocked" : ""}`, href: `#/c/${encodeURIComponent(c.id)}`, title: tip("chat.open") },
      h("span", { class: "main" }, h("span", { class: "glyph" }, glyph(c)), h("span", { class: "title" }, label(c)), h("small", {}, ` ${c.agent}`)))),
    list.length ? null : h("p", { class: "sub" }, "No chats yet."),
    h("a", { class: "foot-link", href: "#/chats", title: tip("chats") }, all > list.length ? `All chats… (${all})` : "All chats…"));
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
    if (msg.reset) state.items = [];
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
      // Archived: out of the list, still known to a page that shows it.
      if (c.archived) { state.list.delete(c.id); state.seen.set(c.id, c); } else { state.list.set(c.id, c); state.seen.delete(c.id); }
      if (state.archived?.some((x) => x.id === c.id)) state.archived = state.archived.map((x) => (x.id === c.id ? c : x));
      ui.render();
    } else if (e.t === "chat_removed") {
      state.list.delete(e.id);
      state.seen.delete(e.id);
      // The chat shown: say so, rather than a page that acts on nothing.
      if (e.id === state.following) state.gone = e.id;
      ui.render();
    } else if (e.t === "config_reloaded") {
      // `[chat] enabled`, `incognito`, `recent`: as the config now says.
      void loadChats();
    } else if (e.t === "machine_updated" && state.following) {
      // The open chat's machine came back: follow it again; went away: say so.
      const c = chatOf(state.following);
      if (c?.machine === e.info.name) {
        if (e.info.state === "online") { const f = state.following; state.following = null; follow(f); }
        else if (e.info.state === "offline") { state.items.push({ id: `offline-${Date.now()}`, t: "notice", level: "warn", text: `${e.info.name} went offline; this chat carries on when it is back` }); ui.render(); }
      }
    }
  }
});

views.set("chats", () => listView());
views.set("c", (rest) => chatView(rest));
