// Chats (SPEC §22.12, ROADMAP CHAT1.8): plain conversations with your agents, kept by ptyro.
// `#/chats` lists them (pinned first, then by day, searchable, one workspace's or all), `#/c/<id>`
// is one chat (its messages, tool calls and permission cards as they stream, a composer), and
// `#/c/new` starts one. Every control is an action below, as in the TUI and the CLI.

import { conn } from "./conn.js";
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
  "chat.preset": "What the chat may use: web search, read folders, or no tools",
  "chat.export": "Download the chat as Markdown",
  "chat.find": "Find a chat by its title or agent",
  "chat.archived": "Show archived chats instead",
} as const;
const tip = actions(ACTIONS);

export interface Chat {
  id: string; handle: string; workspace: string; machine?: string | null; title?: string | null; title_src: string;
  agent: string; profile: string; tools: string; pinned: boolean; archived: boolean; incognito: boolean; forked: boolean;
  leaf?: string | null; nodes: number; unread: number; created_ms: number; last_ms: number; cost_micro_usd: number;
  status?: string | null; attention?: { kind: string; text: string; since_ms: number } | null; engine?: string | null;
}
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
};

const label = (c: Chat | undefined): string => c?.title?.trim() || "new chat";
const addr = (c: Chat): string => `c:${c.handle}`;
const working = (c: Chat): boolean => c.status === "working" || c.status === "starting";
const glyph = (c: Chat): string => c.attention || c.status === "blocked" ? "⚠" : working(c) ? "●" : c.status === "errored" ? "✗" : c.pinned ? "★" : "·";

/// Whether the Chats tab shows (chats are on, as far as the daemon has said).
export function chatsShown(): boolean {
  return !state.off;
}

export async function loadChats(): Promise<void> {
  try {
    const r = await conn.request({ t: "chat_list", all: true });
    state.list = new Map((r.chats as Chat[]).map((c) => [c.id, c]));
    state.off = "";
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

const findBox = h("input", { class: "search", type: "search", placeholder: "Find a chat", "aria-label": "Find a chat", autocapitalize: "off", autocomplete: "off", title: tip("chat.find") });
findBox.addEventListener("input", () => { state.query = findBox.value.trim().toLowerCase(); const l = document.querySelector<HTMLElement>("main.chats .rows"); if (l) patch(l, rows()); });

function rows(): Node[] {
  const source = state.archived ?? sorted(state.list.values());
  const q = state.query;
  const shown = source.filter((c) => (!state.workspace || c.workspace === state.workspace) && (!q || label(c).toLowerCase().includes(q) || c.agent.includes(q)));
  if (!shown.length) {
    return [h("div", { class: "empty" },
      h("p", {}, q ? `Nothing matches “${q}”.` : state.archived ? "No archived chats." : "No chats yet. Ask anything: it can search the web, and it is kept here."),
      q || state.archived || !canWrite() ? null : h("button", { class: "primary", title: tip("chat.new"), onclick: () => newChat() }, "Start a chat"))];
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
  return out;
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
  return h("div", {},
    ui.header("Chats", false, canWrite() ? h("button", { class: "icon plus", title: tip("chat.new"), "aria-label": "New chat", onclick: () => newChat() }, "＋") : null),
    h("div", { class: "searchbar" }, findBox, wsSelect, archived),
    h("main", { class: "list chats", title: tip("chats") }, h("div", { class: "rows" }, ...rows())));
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

function chatMenu(c: Chat): void {
  const archived = c.archived || !!state.archived?.some((x) => x.id === c.id);
  const open = { title: tip("chat.open"), label: "Open", run: () => { location.hash = `#/c/${encodeURIComponent(c.id)}`; } };
  const md = { title: tip("chat.export"), label: "Download as Markdown", run: () => void exportChat(c) };
  // A device that may only read gets what reads (SPEC §22.11).
  if (!canWrite()) { menu(label(c), [open, md]); return; }
  menu(label(c), [
    open,
    { title: tip("chat.retitle"), label: "Rename", run: () => retitle(c) },
    { title: tip("chat.pin"), label: c.pinned ? "Unpin" : "Pin", run: () => update(c, { pinned: !c.pinned }, c.pinned ? "Unpinned" : "Pinned") },
    { title: tip("chat.agent"), label: "Switch agent", hint: `now ${c.agent}`, run: () => pickAgent(c) },
    { title: tip("chat.preset"), label: "Tools", hint: `now ${c.tools}`, run: () => pickPreset(c) },
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
  menu("Which agent? The conversation carries over", agentNames().map((a) => ({ title: tip("chat.agent"), label: a, hint: a === c.agent ? "now" : "", run: () => update(c, { agent: a }, `The next message goes to ${a}`) })));
}

const PRESETS: [string, string][] = [["chat", "web search"], ["read", "web search, and reads its folders"], ["none", "no tools"]];

function pickPreset(c: Chat): void {
  menu("What may the chat use?", [
    ...PRESETS.map(([p, what]) => ({ title: tip("chat.preset"), label: p, hint: p === c.tools ? `now · ${what}` : what, run: () => update(c, { preset: p }, `Tools: ${p}`) })),
    // Your own `[chat.tools.NAME]` presets, by name.
    { title: tip("chat.preset"), label: "One of your own…", hint: "a [chat.tools] preset", run: () => form("Tools: your preset", [{ key: "p", label: "Preset name", required: true, value: PRESETS.some(([p]) => p === c.tools) ? "" : c.tools }], "Use it", async (v) => { await conn.request({ t: "chat_update", chat: c.id, preset: v.p }); ui.toast(`Tools: ${v.p}`); }) },
  ]);
}

async function exportChat(c: Chat): Promise<void> {
  const r = await ask<{ text: string }>({ t: "chat_export", chat: c.id, format: "md" });
  if (r) download(`${label(c).replace(/[^\w.-]+/g, "-").slice(0, 60) || c.handle}.md`, r.text, "text/markdown");
}

// ── one chat ────────────────────────────────────────────────────────────────────────────────

/// The page's composer and transcript stay in the DOM across renders, so a draft, the caret
/// and the scroll position survive the events that re-render everything around them.
interface Page { id: string; el: HTMLElement; box: HTMLElement; reply: HTMLTextAreaElement; els: Map<string, { key: string; el: HTMLElement }>; agent: HTMLSelectElement; preset: HTMLSelectElement; sending: boolean }
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
  if (id) conn.send({ t: "chat_subscribe", chat: id });
}
window.addEventListener("hashchange", () => { if (!/^#\/c\//.test(location.hash)) follow(null); });

function pageFor(id: string): Page {
  if (page?.id === id) return page;
  const reply = h("textarea", { rows: 2, enterkeyhint: "send", placeholder: "Ask anything…", title: tip("chat.send") });
  reply.value = drafts.get(id) ?? "";
  reply.addEventListener("input", () => drafts.set(id, reply.value));
  reply.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey && !e.isComposing) { e.preventDefault(); void send(); } });
  const agent = h("select", { "aria-label": "Agent", title: tip("chat.agent") }, h("option", { value: "" }, "default agent"), ...agentNames().map((a) => h("option", { value: a }, a)));
  // The default is `[chat] preset`'s, whatever it is (a preset of your own too).
  const preset = h("select", { "aria-label": "Tools", title: tip("chat.preset") }, h("option", { value: "" }, "default tools"), ...PRESETS.map(([p, what]) => h("option", { value: p }, `${p}: ${what}`)));
  page = { id, el: h("div", { class: "session chat-page" }), box: h("div", { class: "chat" }), reply, els: new Map(), agent, preset, sending: false };
  // A new chat's page starts empty (not with what a failed try left).
  if (id.startsWith("new")) state.items = [];
  return page;
}

async function send(): Promise<void> {
  const p = page;
  if (!p) return;
  const text = p.reply.value;
  if (!text.trim()) return;
  // One at a time: a second Send while the first is on its way would send it twice.
  if (p.sending) { ui.toast("One moment: the last message is on its way"); return; }
  p.sending = true;
  try {
    if (p.id.startsWith("new")) {
      const ws = decodeURIComponent(p.id.split("/")[1] ?? "") || undefined;
      state.items = [{ id: "first", t: "user", text }];
      ui.render();
      const r = await conn.request({ t: "chat_new", workspace: ws, agent: p.agent.value || null, preset: p.preset.value || null, prompt: text });
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
  } catch (e) { ui.toast(errText(e)); }
  p.sending = false;
}

function chatItem(id: string, it: Item): HTMLElement | null {
  switch (it.t) {
    case "user": return h("div", { class: "msg user" }, it.text);
    case "assistant": return h("div", { class: "msg assistant" }, it.text, it.streaming ? h("span", { class: "caret" }, "▍") : null);
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
    const key = JSON.stringify(it);
    const old = p.els.get(it.id);
    const el = old?.key === key ? old.el : chatItem(p.id, it);
    if (!el) continue;
    if (old && old.el !== el && old.el instanceof HTMLDetailsElement && el instanceof HTMLDetailsElement) el.open = old.el.open;
    els.set(it.id, { key, el });
    kids.push(el);
  }
  p.els = els;
  patch(box, kids);
  if (atBottom) requestAnimationFrame(() => { box.scrollTop = box.scrollHeight; });
  else box.scrollTop = top;
}

function chatView(rest: string): HTMLElement {
  if (!state.loaded) void loadChats();
  const isNew = rest === "new" || rest.startsWith("new/");
  const id = isNew ? `new${rest.slice(3)}` : decodeURIComponent(rest);
  const p = pageFor(id);
  // A new chat follows nothing yet (and shows nothing of the one left).
  follow(isNew ? null : id);
  const c = isNew ? undefined : state.list.get(id) ?? state.archived?.find((x) => x.id === id);
  const pill: Child[] = c
    ? [`${c.agent}@${c.profile}`, ` · tools: ${c.tools}`, c.status ? ` · ${c.status}` : "", c.cost_micro_usd ? ` · $${(c.cost_micro_usd / 1e6).toFixed(3)}` : "", ` · ${addr(c)}`]
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
    empty ?? p.box,
    ...(isNew ? [h("div", { class: "bar chat-pick" }, p.agent, p.preset)] : []),
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
