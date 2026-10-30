// The Chats page's logic with no DOM in it (web/test/chats.mjs runs it): what the list shows and
// in what order, the composer's `/` lines and `@` folders, and what makes a message redrawn.

/// What of a chat the list reads (`ChatInfo`, SPEC §22.12).
export interface ChatLike {
  id: string; title?: string | null; agent: string; workspace: string; pinned: boolean; last_ms: number;
  incognito: boolean; side_of?: string | null; source?: string | null; source_dir?: string | null;
}

export const label = (c: ChatLike | undefined): string => c?.title?.trim() || "new chat";

/// `imported from claude · ~/repo`.
export function importedFrom(c: ChatLike): string {
  if (!c.source) return "";
  const agent = c.source.split(":")[0];
  return c.source_dir ? `imported from ${agent} · ${c.source_dir}` : `imported from ${agent}`;
}

/// Pinned first, then the newest.
export function sorted<T extends ChatLike>(list: Iterable<T>): T[] {
  return [...list].sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.last_ms - a.last_ms);
}

/// The search box as a title filter, lowercased: the search's operators (`by:meaning`, `since:7d`,
/// `workspace:w`, `-word`) are the message search's, not title text; `agent:x` narrows by agent.
export function titleQuery(q: string): { text: string; agent: string } {
  let agent = "";
  const words: string[] = [];
  for (const w of q.toLowerCase().split(/\s+/).filter(Boolean)) {
    const [k, v] = w.includes(":") ? [w.slice(0, w.indexOf(":")), w.slice(w.indexOf(":") + 1)] : ["", ""];
    if (k === "agent") agent = v;
    else if (["by", "since", "workspace", "ws"].includes(k) || (w.startsWith("-") && w.length > 1)) continue;
    else words.push(w.replace(/^"|"$/g, "").replace(/\*$/, ""));
  }
  return { text: words.join(" "), agent };
}

export interface ListFilter { query: string; workspace: string; imported: boolean; pinned: boolean }

/// The list's rows: the chats `source` holds that the filter keeps, each followed by its side
/// chats (newest first, as the TUI's sidebar has them under it). A side chat goes with its chat;
/// incognito ones are never "every chat" (SPEC §22.12).
export function shownChats<T extends ChatLike>(source: T[], f: ListFilter): { chat: T; side: boolean }[] {
  const { text: q, agent } = titleQuery(f.query);
  const keeps = (c: T): boolean => !c.incognito && (!f.workspace || c.workspace === f.workspace) && (!f.imported || !!c.source) && (!f.pinned || c.pinned)
    && (!agent || c.agent.toLowerCase().includes(agent))
    && (!q || label(c).toLowerCase().includes(q) || c.agent.toLowerCase().includes(q) || importedFrom(c).toLowerCase().includes(q));
  const sides = new Map<string, T[]>();
  for (const c of source) if (c.side_of && !c.incognito) sides.set(c.side_of, [...(sides.get(c.side_of) ?? []), c]);
  const out: { chat: T; side: boolean }[] = [];
  for (const c of source) {
    if (c.side_of || !keeps(c)) continue;
    out.push({ chat: c, side: false });
    for (const s of [...(sides.get(c.id) ?? [])].sort((a, b) => b.last_ms - a.last_ms)) out.push({ chat: s, side: true });
  }
  return out;
}

/// The compact CHATS section: every pinned chat, then the newest `recent` others (`[chat] recent`).
export function recentChats<T extends ChatLike>(list: Iterable<T>, recent: number): T[] {
  const top = sorted([...list].filter((c) => !c.side_of && !c.incognito));
  return [...top.filter((c) => c.pinned), ...top.filter((c) => !c.pinned).slice(0, Math.max(0, recent))];
}

/// A line that starts with `/` but is a message (as `chats::slash_message`): `//` sends the rest
/// with one `/`, and a path (`/etc/hosts — what is it?`) is a message as it is. `null`: a command.
export function slashMessage(text: string): string | null {
  const t = text.trimStart();
  if (!t.startsWith("/")) return null;
  const rest = t.slice(1);
  if (rest.startsWith("/")) return rest;
  return (rest.split(/\s/)[0] ?? "").includes("/") ? text : null;
}

/// Folders, one a line or comma-separated, as full paths on the computer the chat runs on (`~` works).
export function folderList(text: string): string[] {
  return text.split(/[\n,]/).map((s) => s.trim()).filter(Boolean);
}

/// The `@` being typed before the caret (`look in @no`), with the chat's folders it may be: null
/// when the caret isn't after one, or the chat has no folders.
export function atQuery(text: string, caret: number, folders: string[]): { start: number; matches: string[] } | null {
  const m = /(^|\s)@(\S*)$/.exec(text.slice(0, caret));
  if (!m || !folders.length) return null;
  const q = m[2].toLowerCase();
  return { start: caret - m[2].length - 1, matches: folders.filter((f) => f.toLowerCase().includes(q)) };
}

/// The text with the `@…` at `start` (up to the caret) replaced by `folder`, and where the caret goes.
export function atInsert(text: string, caret: number, start: number, folder: string): { text: string; caret: number } {
  const put = /^\s/.test(text.slice(caret)) ? folder : `${folder} `;
  return { text: text.slice(0, start) + put + text.slice(caret), caret: start + put.length };
}

/// What makes a transcript item drawn again: what it shows changing, not the whole of it (a long
/// answer isn't turned into a string on every render); `extra`: what around it changes its
/// buttons and links.
export function itemKey(it: { id: string; t: string; rev?: number; [k: string]: any }, sib: { at: number; of: number } | undefined, findIn: string, extra = ""): string {
  const len = (x: unknown): number => (typeof x === "string" ? x.length : 0);
  return [it.id, it.t, it.rev ?? "", len(it.text), len(it.output), len(it.title), it.streaming ? 1 : 0, it.status ?? "", it.decided ?? "", it.stop ?? "", sib ? `${sib.at}/${sib.of}` : "", findIn, extra].join("\u0001");
}

/// The daemon's "chats are off" (`[chat] enabled = false`), not some other failure to list them.
export function chatsOff(e: unknown): boolean {
  const x = e as { code?: string; message?: string };
  return x?.code === "unsupported" && /^chats are off/.test(x.message ?? "");
}
