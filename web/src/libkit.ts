// The Library's logic with no DOM in it (web/test/library.mjs runs it): what the list asks the
// daemon for, the days of the history, the words a hit matched and where they are in a text,
// an address's parts, the daemon's question before it forgets, and the notes: what their list
// asks, how they are grouped, and what a new one sends.

/// A search starts at this many characters (a shorter prefix matches everything), as the TUI's
/// Library and the chat list do; fewer is the history.
export const MIN_QUERY = 3;
/// How many items of the history one ask brings ("Older…" brings the next).
export const PAGE = 50;

/// `kind:`'s words, and the kind of item each is kept as (`null`: a chat, which is no item).
export const KINDS: Record<string, string | null> = {
  session: "session", chat: null, note: "note", doc: "file", comment: "file", code: "file", path: "file", commit: "commit", command: "shell",
};
/// How far back `since` may look: what the pick offers.
export const SINCE: [string, string][] = [["1d", "The last day"], ["7d", "The last week"], ["30d", "The last 30 days"], ["90d", "The last 90 days"], ["365d", "The last year"]];

/// A row's history: only what was kept of this session or worktree.
export interface Of { t: "session" | "worktree"; id: string; name: string }
/// The search box and the filters. `machine`: "" every machine, "here" this computer, else a
/// linked machine's name. `wrote`: "" anyone, "you", or "agent".
export interface Filters { query: string; kind: string; project: string; since: string; wrote: string; machine: string; of: Of | null }

export const noFilters = (): Filters => ({ query: "", kind: "", project: "", since: "", wrote: "", machine: "", of: null });

/// Who wrote it, as the pick offers it.
export const WROTE: [string, string][] = [["you", "You"], ["agent", "Your agents"]];
/// `wrote`'s word as the writer a request names.
const writer = (wrote: string): { writer?: string } => (wrote ? { writer: wrote === "you" ? "user" : "agent" } : {});

/// The query is one to search for (else the list is the history).
export const searching = (q: string): boolean => [...q.trim()].length >= MIN_QUERY;

/// What the list asks the daemon for: the search, or (nothing typed) the history, under the
/// filters; `before` is the next page of the history. `null`: the history of chats, which the
/// knowledge base has none of.
export function request(f: Filters, now: number, before?: number): Record<string, unknown> | null {
  const project = f.project || null;
  if (searching(f.query)) {
    const query = f.query.trim() + (f.since ? ` since:${f.since}` : "") + (f.wrote ? ` wrote:${f.wrote}` : "");
    // A search asks every linked machine that is up too, unless one is picked. Within a row's
    // history it asks for more: only that row's are kept of what is found.
    return { t: "kb_search", query, project, kinds: f.kind ? [f.kind] : [], limit: f.of ? 100 : 30, machines: !f.machine, ...(f.machine ? { machine: f.machine } : {}) };
  }
  if (f.kind && KINDS[f.kind] === null) return null;
  const far = f.machine && f.machine !== "here" ? { machine: f.machine } : {};
  return {
    t: "kb_recent", kinds: f.kind ? [KINDS[f.kind]] : [], project, limit: PAGE,
    ...(f.of ? { [f.of.t]: f.of.id } : {}),
    ...(f.since ? { since_ms: now - parseInt(f.since, 10) * 86_400_000 } : {}),
    ...(before ? { before_ms: before } : {}),
    ...far,
    ...writer(f.wrote),
  };
}

// ── notes (SPEC §25.6.4) ────────────────────────────────────────────────────────────────────

/// What the notes' list asks: all of them, or a project's, or one writer's.
export function notesRequest(f: { project: string; wrote: string }): Record<string, unknown> {
  return { t: "kb_notes", ...(f.project ? { project: f.project } : {}), ...writer(f.wrote) };
}

/// The notes under what they are about: the ones about no project first, then each project's by
/// its name; within one, as they came (newest first).
export function byProject<T extends { project_name?: string | null }>(items: T[]): [string, T[]][] {
  const groups = new Map<string, T[]>();
  for (const i of items) {
    const name = i.project_name ?? "";
    groups.set(name, [...(groups.get(name) ?? []), i]);
  }
  return [...groups.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([name, list]): [string, T[]] => [name || "Global (about no project)", list]);
}

/// Tags as they are typed: `db, decision` or `#db #decision`.
export const tagsOf = (text: string): string[] => text.split(/[,\s]+/).map((t) => t.replace(/^#/, "")).filter(Boolean);

/// A new note, from the form's values: its text, and only what was filled in.
export function noteRequest(v: { text: string; title?: string; project?: string; profile?: string; tags?: string }): Record<string, unknown> {
  const tags = tagsOf(v.tags ?? "");
  return { t: "kb_note", text: v.text, ...(v.title ? { title: v.title } : {}), ...(v.project ? { project: v.project } : {}), ...(v.profile ? { profile: v.profile } : {}), ...(tags.length ? { tags } : {}) };
}

/// Who wrote an item, in a word: `you`, or the agent.
export const wroteBy = (i: { writer?: string | null; agent?: string | null }): string => (i.writer === "user" ? "you" : i.agent ?? i.writer ?? "");

/// The kinds the kind filter offers: sessions and chats, and whatever else is kept.
export function kindsOffered(held: { kind: string; items: number }[]): string[] {
  const kept = new Set(held.filter((k) => k.items > 0).map((k) => k.kind));
  return Object.keys(KINDS).filter((k) => k === "session" || KINDS[k] === null || kept.has(KINDS[k] as string));
}

/// When an item happened: its end, else its start, else when it was kept.
export const when = (i: { ended_ms?: number | null; started_ms?: number | null; ingested_ms?: number | null }): number => i.ended_ms ?? i.started_ms ?? i.ingested_ms ?? 0;

const pad = (n: number): string => String(n).padStart(2, "0");
/// The day an item is listed under, in this device's time: `Today`, `Yesterday`, else
/// `Tue 2026-11-17`.
export function day(ms: number, now: number): string {
  const d = new Date(ms), n = new Date(now);
  const midnight = (x: Date): number => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
  const days = Math.round((midnight(n) - midnight(d)) / 86_400_000);
  if (days === 0) return "Today";
  if (days === 1) return "Yesterday";
  return `${["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][d.getDay()]} ${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}
/// `14:05`, in this device's time.
export const clock = (ms: number): string => { const d = new Date(ms); return `${pad(d.getHours())}:${pad(d.getMinutes())}`; };

/// A piece of text, and whether it is a match.
export interface Part { text: string; on: boolean }

/// A snippet as the daemon sends it (each match between the control characters 2 and 3), in
/// parts.
export function snippetParts(snippet: string): Part[] {
  const out: Part[] = [];
  let on = false;
  for (const text of snippet.replace(/\n/g, " ").split(/[\u0002\u0003]/)) {
    if (text) out.push({ text, on });
    on = !on;
  }
  return out;
}

/// The words a snippet marks as matched, lowercased, each once.
export function matched(snippet: string): string[] {
  const out: string[] = [];
  for (const p of snippetParts(snippet)) {
    const w = p.text.trim().toLowerCase();
    if (p.on && w && !out.includes(w)) out.push(w);
  }
  return out;
}

/// `text` in parts, every one of `words` in it a match (whatever its case).
export function lit(text: string, words: string[]): Part[] {
  const ws = words.map((w) => w.toLowerCase()).filter(Boolean);
  const low = text.toLowerCase();
  // (Lowercasing may change a character's length: then the text is shown plain.)
  if (!ws.length || low.length !== text.length) return text ? [{ text, on: false }] : [];
  const out: Part[] = [];
  let from = 0;
  while (from < text.length) {
    let at = -1, len = 0;
    for (const w of ws) {
      const i = low.indexOf(w, from);
      if (i >= 0 && (at < 0 || i < at)) { at = i; len = w.length; }
    }
    if (at < 0) break;
    if (at > from) out.push({ text: text.slice(from, at), on: false });
    out.push({ text: text.slice(at, at + len), on: true });
    from = at + len;
  }
  if (from < text.length) out.push({ text: text.slice(from), on: false });
  return out;
}

/// An address in parts: `laptop2:k:7f3k/12` is the item `laptop2:k:7f3k` and its piece 12; an
/// item's own address has no piece.
export function split(addr: string): { item: string; piece: number | null } {
  const i = addr.lastIndexOf("/");
  const n = i >= 0 ? Number(addr.slice(i + 1)) : NaN;
  return i >= 0 && Number.isInteger(n) ? { item: addr.slice(0, i), piece: n } : { item: addr, piece: null };
}

/// What `KbForget` takes for a listed item: its id here (exact, whatever is kept meanwhile); a
/// linked machine's by its address there (ids are each machine's own).
export const forgetAddr = (i: { addr: string; item: { id: string } }): string => (i.addr.startsWith("k:") ? i.item.id : i.addr);

/// The daemon's question before it forgets (`Forget 1 item: session “x”? The text is deleted…`)
/// as a dialog's title and what it says (which has no question in it, so a title that has one
/// stays whole); `null` when it isn't one.
export function question(message: string): [string, string] | null {
  const i = message.lastIndexOf("? ");
  return i > 0 ? [message.slice(0, i + 1), message.slice(i + 2)] : null;
}

/// The pieces that hold `text` (whatever its case), by their index in the item.
export function found(chunks: { idx: number; text: string; heading?: string | null }[], text: string): number[] {
  const t = text.trim().toLowerCase();
  return t ? chunks.filter((c) => c.text.toLowerCase().includes(t) || (c.heading ?? "").toLowerCase().includes(t)).map((c) => c.idx) : [];
}

/// The conversation an agent session's item is of, by the agent's own id (the end of its URI).
export const conversation = (uri: string): string => uri.slice(uri.lastIndexOf("/") + 1);

export const plural = (n: number, word: string): string => `${n} ${word}${n === 1 ? "" : "s"}`;
export const bytes = (n: number): string => (n >= 1 << 20 ? `${(n / (1 << 20)).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);

/// `kb_ingest`, from the form's values: what `ptyro ingest PATH` takes. A switch left as it
/// stands by default is not sent (the computer's own settings then decide).
export function ingestRequest(v: Record<string, unknown>): object {
  const text = (k: string): string | undefined => (typeof v[k] === "string" && (v[k] as string).trim() ? (v[k] as string).trim() : undefined);
  const globs = (k: string): string[] => (text(k) ?? "").split(",").map((g) => g.trim()).filter(Boolean);
  const options: Record<string, unknown> = {};
  if (globs("include").length) options.include = globs("include");
  if (globs("exclude").length) options.exclude = globs("exclude");
  if (v.comments === false) options.comments = false;
  if (v.code === true) options.code = true;
  if (v.commits === false) options.commits = false;
  if (v.once === true) options.once = true;
  const spec: Record<string, unknown> = { path: text("path") ?? "", options };
  for (const k of ["name", "project", "profile"]) if (text(k)) spec[k] = text(k);
  return { t: "kb_ingest", spec };
}
