// The Library's logic with no DOM in it (web/test/library.mjs runs it): what the list asks the
// daemon for, the days of the history, the words a hit matched and where they are in a text,
// an address's parts, the daemon's question before it forgets, the notes (what their list
// asks, how they are grouped, what a new one sends), and the sources: what is read, how each
// root stands, what the ingest form and the agents' history ask, and what is kept of shells.

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

// ── sources (SPEC §25.6.3, §25.10) ──────────────────────────────────────────────────────────

/// A root as `kb_roots` lists it: a project, or a folder or file given to be read.
export interface Root {
  id: string; name: string; kind: string; path: string; state: string; error?: string | null;
  project_name?: string | null; profile?: string | null; added_ms?: number | null; synced_ms?: number | null;
  options?: { include?: string[]; exclude?: string[]; comments?: boolean | null; code?: boolean | null; commits?: boolean | null; once?: boolean };
  files: number; commits: number; chunks: number; progress?: [number, number] | null; removing?: boolean;
}

/// The machine a pick names, as a request carries it: none for this computer.
const far = (machine: string | undefined): { machine?: string } => (machine && machine !== "here" ? { machine } : {});

/// What the Sources tab asks: this computer's roots, or a linked machine's.
export const rootsRequest = (machine = ""): Record<string, unknown> => ({ t: "kb_roots", ...far(machine) });

/// Something of it is being read or removed: the list is asked for again until it is done.
export const rootBusy = (r: Root): boolean => !!r.progress || !!r.removing;

/// How a root stands, in a word or two. `since`: how long ago, in words (`2m`).
export function rootState(r: Root, since: (ms: number) => string): string {
  if (r.removing) return "being removed";
  if (r.progress) return r.progress[1] > 0 ? `reading ${r.progress[0]} of ${r.progress[1]}` : "waiting to be read";
  if (r.state === "missing") return "missing";
  if (r.state === "error") return "failing";
  if (r.state === "syncing") return "being read";
  return r.synced_ms ? `read ${since(r.synced_ms)} ago` : "not read yet";
}

/// It is missing or could not be read.
export const rootBad = (r: Root): boolean => r.state === "missing" || r.state === "error";

/// What a root holds: `12 files, 40 commits, 300 pieces`.
export const rootHolds = (r: Root): string => [plural(r.files, "file"), r.commits ? plural(r.commits, "commit") : "", plural(r.chunks, "piece")].filter(Boolean).join(", ");

/// What is read of a root's files where it says so itself (else the settings decide).
export function rootReads(r: Root): string {
  const o = r.options ?? {};
  const out: string[] = [];
  for (const [what, v] of [["comments", o.comments], ["code", o.code], ["commits", o.commits]] as const) {
    if (v === true) out.push(what);
    if (v === false) out.push(`no ${what}`);
  }
  if (o.include?.length) out.push(`only ${o.include.join(", ")}`);
  if (o.exclude?.length) out.push(`not ${o.exclude.join(", ")}`);
  if (o.once) out.push("read once");
  return out.join(" · ");
}

/// What a root's read will take, as the command says it (`KbPlan::line`).
export function planLine(p: { files: number; documents: number; sources: number; comments: boolean; code: boolean; commits: boolean; existing?: boolean }): string {
  const parts: string[] = [];
  if (p.documents) parts.push(plural(p.documents, "document"));
  if (p.sources) parts.push(`${plural(p.sources, "source file")} (${p.comments && p.code ? "comments and code" : p.comments ? "comments" : p.code ? "code" : "names only"})`);
  if (p.commits) parts.push("its commits");
  return plural(p.files, "file") + (parts.length ? `: ${parts.join(", ")}` : "") + (p.existing ? " (read already: looked at again)" : "");
}

/// How far back the agents' history may be read: what the pick offers.
export const HISTORY_SINCE: [string, string][] = [["7d", "The last week"], ["30d", "The last 30 days"], ["90d", "The last 90 days"], ["365d", "The last year"]];

/// `kb_history`, from the form's values: what `ptyro ingest --history` takes. `dryRun`: count,
/// and read nothing (always asked first).
export function historyRequest(v: Record<string, unknown>, now: number, dryRun: boolean): Record<string, unknown> {
  const text = (k: string): string => (typeof v[k] === "string" ? (v[k] as string).trim() : "");
  return {
    t: "kb_history", dry_run: dryRun,
    ...(text("agent") ? { agent: text("agent") } : {}),
    ...(text("profile") ? { profile: text("profile") } : {}),
    ...(text("since") ? { since_ms: now - parseInt(text("since"), 10) * 86_400_000 } : {}),
    ...far(text("machine")),
  };
}

/// The count, in the words every client uses (`KbHistoryPlan::text`'s first line).
export function historyText(p: { found: number; known: number; old: number; chats: number; new: number }): string {
  if (!p.found) return "No conversations in your agents' folders";
  const parts = [[p.known, "kept already (or forgotten, or private)"], [p.chats, "a chat's (its text is in your chats)"], [p.old, "too old to keep"], [p.new, "to read"]] as const;
  return `${plural(p.found, "conversation")} in your agents' folders: ${parts.filter(([n]) => n > 0).map(([n, what]) => `${n} ${what}`).join(", ")}`;
}

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
  return { t: "kb_ingest", spec, ...far(text("machine")) };
}

// ── shells (SPEC §25.6.5) ───────────────────────────────────────────────────────────────────

/// A switch of what is kept of your shells: its button, what `kb.shell` becomes, what is said
/// once it is, and (for the one that stores what is on your terminal) what is asked first.
export interface ShellSwitch { label: string; to: string; said: string; ask?: [title: string, text: string, yes: string] }
/// What one prompt was offered (`kb.inject`, SPEC §25.9): the session that was given it, and
/// the pointers.
export interface Offer { at_ms: number; session: string; name: string; pointers: { addr: string; kind: string; title?: string | null }[] }

/// What agents are offered at a prompt, for the status: the sentence that says whether they
/// are (`null`: they are not, and never were since the daemon started), and each offer as
/// who was given it and what each pointer is called (its address opens it).
export function offered(inject: boolean | undefined, offers: Offer[] | undefined): { says: string | null; rows: { to: string; pointers: { addr: string; label: string }[] }[] } {
  const rows = (offers ?? []).map((o) => ({
    to: o.name,
    pointers: o.pointers.map((p) => ({ addr: p.addr, label: p.title ? `${p.kind}, ${p.title}` : p.kind })),
  }));
  const says = inject ? "Agents may be given up to three pointers with a prompt (Knowledge → inject)"
    : rows.length ? "Off now (Knowledge → inject); what was offered before is below" : null;
  return { says, rows };
}

/// What is kept of your shells (`kb.shell`: off, commands, output), in a sentence, and the
/// switches that change it: the commands first, what they print second and only where the
/// commands are kept. `null`: a daemon from before shells were kept.
export function shellsKept(shell: string | null | undefined): { says: string; switches: ShellSwitch[] } | null {
  const off: ShellSwitch = { label: "", to: "off", said: "Nothing more of your shells is kept (what was kept stays until you forget it)" };
  if (shell === "off") return {
    says: "Nothing of your shells is kept.",
    switches: [{ label: "Keep shell commands", to: "commands", said: "Shell commands are kept from here on: the lines you run, not what they print (a private shell keeps nothing)" }],
  };
  if (shell === "commands") return {
    says: "Shell commands are kept, not what they print.",
    switches: [{ ...off, label: "Stop keeping commands" }, {
      label: "Keep what they print too…", to: "output", said: "What shell commands print is kept from here on",
      ask: ["Keep what your commands print?",
        "What each command prints in a shell is then stored in the knowledge base beside its command line: the first 2 KiB and the last 6 KiB of it, with tokens and passwords redacted, for 30 days unless the settings say otherwise. This is the one place where what is on your terminal is stored. Never kept: a private shell, a command typed with a space before it, what a full-screen program (an editor, a pager) shows, and anything a command shows that you type into while it runs (a password prompt, a REPL, ssh).",
        "Keep it"],
    }],
  };
  if (shell === "output") return {
    says: "Shell commands are kept, and what they print.",
    switches: [{ label: "Stop keeping what they print", to: "commands", said: "What shell commands print is no longer kept (the commands still are; what was kept stays)" }, { ...off, label: "Stop keeping both" }],
  };
  return null;
}
