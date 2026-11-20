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
export const KINDS = {
    session: "session", chat: null, note: "note", doc: "file", comment: "file", code: "file", path: "file", commit: "commit", command: "shell",
};
/// How far back `since` may look: what the pick offers.
export const SINCE = [["1d", "The last day"], ["7d", "The last week"], ["30d", "The last 30 days"], ["90d", "The last 90 days"], ["365d", "The last year"]];
export const noFilters = () => ({ query: "", kind: "", project: "", since: "", wrote: "", machine: "", of: null });
/// Who wrote it, as the pick offers it.
export const WROTE = [["you", "You"], ["agent", "Your agents"]];
/// `wrote`'s word as the writer a request names.
const writer = (wrote) => (wrote ? { writer: wrote === "you" ? "user" : "agent" } : {});
/// The query is one to search for (else the list is the history).
export const searching = (q) => [...q.trim()].length >= MIN_QUERY;
/// What the list asks the daemon for: the search, or (nothing typed) the history, under the
/// filters; `before` is the next page of the history. `null`: the history of chats, which the
/// knowledge base has none of.
export function request(f, now, before) {
    const project = f.project || null;
    if (searching(f.query)) {
        const query = f.query.trim() + (f.since ? ` since:${f.since}` : "") + (f.wrote ? ` wrote:${f.wrote}` : "");
        // A search asks every linked machine that is up too, unless one is picked. Within a row's
        // history it asks for that session's or worktree's alone (the computer filters: what is
        // found is the row's, however much else holds the words).
        return { t: "kb_search", query, project, kinds: f.kind ? [f.kind] : [], limit: 30, machines: !f.machine, ...(f.machine ? { machine: f.machine } : {}), ...(f.of ? { [f.of.t]: f.of.id } : {}) };
    }
    if (f.kind && KINDS[f.kind] === null)
        return null;
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
export function notesRequest(f) {
    return { t: "kb_notes", ...(f.project ? { project: f.project } : {}), ...writer(f.wrote) };
}
/// The notes under what they are about: the ones about no project first, then each project's by
/// its name; within one, as they came (newest first).
export function byProject(items) {
    const groups = new Map();
    for (const i of items) {
        const name = i.project_name ?? "";
        groups.set(name, [...(groups.get(name) ?? []), i]);
    }
    return [...groups.entries()].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)).map(([name, list]) => [name || "Global (about no project)", list]);
}
/// Tags as they are typed: `db, decision` or `#db #decision`.
export const tagsOf = (text) => text.split(/[,\s]+/).map((t) => t.replace(/^#/, "")).filter(Boolean);
/// A new note, from the form's values: its text, and only what was filled in.
export function noteRequest(v) {
    const tags = tagsOf(v.tags ?? "");
    return { t: "kb_note", text: v.text, ...(v.title ? { title: v.title } : {}), ...(v.project ? { project: v.project } : {}), ...(v.profile ? { profile: v.profile } : {}), ...(tags.length ? { tags } : {}) };
}
export const noDraft = () => ({ of: "", text: "", base: "", from: "" });
/// The draft for the note at `addr`, now that its file is `file`. A draft of another note
/// starts over from the file. The same note's file changed on the computer since the draft
/// was started (an editor there, another page): a draft with nothing typed in it follows the
/// file; one with edits is kept, and `stale` says the file moved under it (saving would
/// write over what changed, so the computer refuses until the reader chooses).
export function noteDraft(d, addr, file) {
    const fresh = { of: addr, text: file.text, base: file.version ?? "", from: file.text };
    if (d.of !== addr)
        return { draft: fresh, stale: false };
    if (file.text === d.from)
        return { draft: d, stale: false };
    return d.text === d.from ? { draft: fresh, stale: false } : { draft: d, stale: true };
}
/// The draft once the reader chose to keep it over the file as it is now: saved from that
/// file's version, knowingly over it.
export const keepDraft = (d, file) => ({ ...d, base: file.version ?? "", from: file.text });
/// `kb_note` for a note written again: the whole file, and the version it was read as (none
/// from a computer that says none: it writes as it always did).
export const saveRequest = (id, d) => ({ t: "kb_note", addr: id, text: d.text, ...(d.base ? { base: d.base } : {}) });
/// Who wrote an item, in a word: `you`, or the agent.
export const wroteBy = (i) => (i.writer === "user" ? "you" : i.agent ?? i.writer ?? "");
/// The kinds the kind filter offers: sessions and chats, and whatever else is kept.
export function kindsOffered(held) {
    const kept = new Set(held.filter((k) => k.items > 0).map((k) => k.kind));
    return Object.keys(KINDS).filter((k) => k === "session" || KINDS[k] === null || kept.has(KINDS[k]));
}
/// When an item happened: its end, else its start, else when it was kept.
export const when = (i) => i.ended_ms ?? i.started_ms ?? i.ingested_ms ?? 0;
const pad = (n) => String(n).padStart(2, "0");
/// The day an item is listed under, in this device's time: `Today`, `Yesterday`, else
/// `Tue 2026-11-17`.
export function day(ms, now) {
    const d = new Date(ms), n = new Date(now);
    const midnight = (x) => new Date(x.getFullYear(), x.getMonth(), x.getDate()).getTime();
    const days = Math.round((midnight(n) - midnight(d)) / 86_400_000);
    if (days === 0)
        return "Today";
    if (days === 1)
        return "Yesterday";
    return `${["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][d.getDay()]} ${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}`;
}
/// `14:05`, in this device's time.
export const clock = (ms) => { const d = new Date(ms); return `${pad(d.getHours())}:${pad(d.getMinutes())}`; };
/// A snippet as the daemon sends it (each match between the control characters 2 and 3), in
/// parts.
export function snippetParts(snippet) {
    const out = [];
    let on = false;
    for (const text of snippet.replace(/\n/g, " ").split(/[\u0002\u0003]/)) {
        if (text)
            out.push({ text, on });
        on = !on;
    }
    return out;
}
/// The words a snippet marks as matched, lowercased, each once.
export function matched(snippet) {
    const out = [];
    for (const p of snippetParts(snippet)) {
        const w = p.text.trim().toLowerCase();
        if (p.on && w && !out.includes(w))
            out.push(w);
    }
    return out;
}
/// `text` in parts, every one of `words` in it a match (whatever its case).
export function lit(text, words) {
    const ws = words.map((w) => w.toLowerCase()).filter(Boolean);
    const low = text.toLowerCase();
    // (Lowercasing may change a character's length: then the text is shown plain.)
    if (!ws.length || low.length !== text.length)
        return text ? [{ text, on: false }] : [];
    const out = [];
    let from = 0;
    while (from < text.length) {
        let at = -1, len = 0;
        for (const w of ws) {
            const i = low.indexOf(w, from);
            if (i >= 0 && (at < 0 || i < at)) {
                at = i;
                len = w.length;
            }
        }
        if (at < 0)
            break;
        if (at > from)
            out.push({ text: text.slice(from, at), on: false });
        out.push({ text: text.slice(at, at + len), on: true });
        from = at + len;
    }
    if (from < text.length)
        out.push({ text: text.slice(from), on: false });
    return out;
}
/// An address in parts: `laptop2:k:7f3k/12` is the item `laptop2:k:7f3k` and its piece 12; an
/// item's own address has no piece.
export function split(addr) {
    const i = addr.lastIndexOf("/");
    const n = i >= 0 ? Number(addr.slice(i + 1)) : NaN;
    return i >= 0 && Number.isInteger(n) ? { item: addr.slice(0, i), piece: n } : { item: addr, piece: null };
}
/// What `KbForget` takes for a listed item: its id here (exact, whatever is kept meanwhile); a
/// linked machine's by its address there (ids are each machine's own).
export const forgetAddr = (i) => (i.addr.startsWith("k:") ? i.item.id : i.addr);
/// The daemon's question before it forgets (`Forget 1 item: session “x”? The text is deleted…`)
/// as a dialog's title and what it says (which has no question in it, so a title that has one
/// stays whole); `null` when it isn't one.
export function question(message) {
    const i = message.lastIndexOf("? ");
    return i > 0 ? [message.slice(0, i + 1), message.slice(i + 2)] : null;
}
/// The pieces that hold `text` (whatever its case), by their index in the item.
export function found(chunks, text) {
    const t = text.trim().toLowerCase();
    return t ? chunks.filter((c) => c.text.toLowerCase().includes(t) || (c.heading ?? "").toLowerCase().includes(t)).map((c) => c.idx) : [];
}
/// The conversation an agent session's item is of, by the agent's own id (the end of its URI).
export const conversation = (uri) => uri.slice(uri.lastIndexOf("/") + 1);
export const plural = (n, word) => `${n} ${word}${n === 1 ? "" : "s"}`;
export const bytes = (n) => (n >= 1 << 20 ? `${(n / (1 << 20)).toFixed(1)} MB` : `${Math.max(1, Math.round(n / 1024))} KB`);
/// The machine a pick names, as a request carries it: none for this computer.
const far = (machine) => (machine && machine !== "here" ? { machine } : {});
/// What the Sources tab asks: this computer's roots, or a linked machine's.
export const rootsRequest = (machine = "") => ({ t: "kb_roots", ...far(machine) });
/// Something of it is being read or removed: the list is asked for again until it is done.
export const rootBusy = (r) => !!r.progress || !!r.removing;
/// How a root stands, in a word or two. `since`: how long ago, in words (`2m`).
export function rootState(r, since) {
    if (r.removing)
        return "being removed";
    if (r.progress)
        return r.progress[1] > 0 ? `reading ${r.progress[0]} of ${r.progress[1]}` : "waiting to be read";
    if (r.state === "missing")
        return "missing";
    if (r.state === "error")
        return "failing";
    if (r.state === "syncing")
        return "being read";
    return r.synced_ms ? `read ${since(r.synced_ms)} ago` : "not read yet";
}
/// It is missing or could not be read.
export const rootBad = (r) => r.state === "missing" || r.state === "error";
/// What a root holds: `12 files, 40 commits, 300 pieces`.
export const rootHolds = (r) => [plural(r.files, "file"), r.commits ? plural(r.commits, "commit") : "", plural(r.chunks, "piece")].filter(Boolean).join(", ");
/// What is read of a root's files where it says so itself (else the settings decide).
export function rootReads(r) {
    const o = r.options ?? {};
    const out = [];
    for (const [what, v] of [["comments", o.comments], ["code", o.code], ["commits", o.commits]]) {
        if (v === true)
            out.push(what);
        if (v === false)
            out.push(`no ${what}`);
    }
    if (o.include?.length)
        out.push(`only ${o.include.join(", ")}`);
    if (o.exclude?.length)
        out.push(`not ${o.exclude.join(", ")}`);
    if (o.once)
        out.push("read once");
    return out.join(" · ");
}
/// What a root's read will take, as the command says it (`KbPlan::line`).
export function planLine(p) {
    const parts = [];
    if (p.documents)
        parts.push(plural(p.documents, "document"));
    if (p.sources)
        parts.push(`${plural(p.sources, "source file")} (${p.comments && p.code ? "comments and code" : p.comments ? "comments" : p.code ? "code" : "names only"})`);
    if (p.commits)
        parts.push("its commits");
    return plural(p.files, "file") + (parts.length ? `: ${parts.join(", ")}` : "") + (p.existing ? " (read already: looked at again)" : "");
}
/// How far back the agents' history may be read: what the pick offers.
export const HISTORY_SINCE = [["7d", "The last week"], ["30d", "The last 30 days"], ["90d", "The last 90 days"], ["365d", "The last year"]];
/// `kb_history`, from the form's values: what `ptyro ingest --history` takes. `dryRun`: count,
/// and read nothing (always asked first).
export function historyRequest(v, now, dryRun) {
    const text = (k) => (typeof v[k] === "string" ? v[k].trim() : "");
    return {
        t: "kb_history", dry_run: dryRun,
        ...(text("agent") ? { agent: text("agent") } : {}),
        ...(text("profile") ? { profile: text("profile") } : {}),
        ...(text("since") ? { since_ms: now - parseInt(text("since"), 10) * 86_400_000 } : {}),
        ...far(text("machine")),
    };
}
/// The count, in the words every client uses (`KbHistoryPlan::text`'s first line).
export function historyText(p) {
    if (!p.found)
        return "No conversations in your agents' folders";
    const parts = [[p.known, "kept already (or forgotten, or private)"], [p.chats, "a chat's (its text is in your chats)"], [p.old, "too old to keep"], [p.new, "to read"]];
    return `${plural(p.found, "conversation")} in your agents' folders: ${parts.filter(([n]) => n > 0).map(([n, what]) => `${n} ${what}`).join(", ")}`;
}
/// The built-in settings: where the switches start when the computer's can't be read.
export const TIERS = { comments: true, code: false, commits: true };
/// The tiers from the settings as `settings.view` lists them (each key's value in effect);
/// `null` when they can't be read (this device may not read the config).
export function tiersOf(entries) {
    if (!entries)
        return null;
    const on = (key) => {
        const e = entries.find((x) => x.key === `kb.${key}`);
        return e ? String(e.value) === "true" : TIERS[key];
    };
    return { comments: on("comments"), code: on("code"), commits: on("commits") };
}
/// `kb_ingest`, from the form's values: what `ptyro ingest PATH` takes. A switch (comments,
/// code, commits) says something only when it was moved from where the settings had it
/// (`start`): one left as it stood leaves the settings deciding. Where the settings are not
/// known (`start` is `null`: this device can't read them), or are another computer's (a
/// machine is picked), every switch says what it shows.
export function ingestRequest(v, start = null) {
    const text = (k) => (typeof v[k] === "string" && v[k].trim() ? v[k].trim() : undefined);
    const globs = (k) => (text(k) ?? "").split(",").map((g) => g.trim()).filter(Boolean);
    const options = {};
    if (globs("include").length)
        options.include = globs("include");
    if (globs("exclude").length)
        options.exclude = globs("exclude");
    const known = far(text("machine")).machine ? null : start;
    for (const k of ["comments", "code", "commits"]) {
        if (typeof v[k] === "boolean" && v[k] !== known?.[k])
            options[k] = v[k];
    }
    if (v.once === true)
        options.once = true;
    const spec = { path: text("path") ?? "", options };
    for (const k of ["name", "project", "profile"])
        if (text(k))
            spec[k] = text(k);
    return { t: "kb_ingest", spec, ...far(text("machine")) };
}
/// What agents are offered at a prompt, for the status: the sentence that says whether they
/// are (`null`: they are not, and never were since the daemon started), and each offer as
/// who was given it and what each pointer is called (its address opens it).
export function offered(inject, offers) {
    const rows = (offers ?? []).map((o) => ({
        to: o.name,
        pointers: o.pointers.map((p) => ({ addr: p.addr, label: p.title ? `${p.kind}, ${p.title}` : p.kind })),
    }));
    const says = inject ? "Agents may be given up to three pointers with a prompt (Knowledge → inject)"
        : rows.length ? "Off now (Knowledge → inject); what was offered before is below" : null;
    return { says, rows };
}
/// What is asked before `kb.shell` becomes `output`, wherever the page switches it (the
/// Library's Sources, and Settings → Knowledge → shell): the one setting that stores what is
/// on your terminal is never switched on by one tap.
export const SHELL_OUTPUT_ASK = ["Keep what your commands print?",
    "What each command prints in a shell is then stored in the knowledge base beside its command line: the first 2 KiB and the last 6 KiB of it, with tokens and passwords redacted, for 30 days unless the settings say otherwise. This is the one place where what is on your terminal is stored. Never kept: a private shell, a command typed with a space before it, what a full-screen program (an editor, a pager) shows, and anything a command shows that you type into while it runs (a password prompt, a REPL, ssh).",
    "Keep it"];
/// Whether setting `key` to `value` is that switch: asked first, unless it is `output` already.
export const asksFirst = (key, value, current) => key === "kb.shell" && value === "output" && current !== "output";
/// What is kept of your shells (`kb.shell`: off, commands, output), in a sentence, and the
/// switches that change it: the commands first, what they print second and only where the
/// commands are kept. `null`: a daemon from before shells were kept.
export function shellsKept(shell) {
    const off = { label: "", to: "off", said: "Nothing more of your shells is kept (what was kept stays until you forget it)" };
    if (shell === "off")
        return {
            says: "Nothing of your shells is kept.",
            switches: [{ label: "Keep shell commands", to: "commands", said: "Shell commands are kept from here on: the lines you run, not what they print (a private shell keeps nothing)" }],
        };
    if (shell === "commands")
        return {
            says: "Shell commands are kept, not what they print.",
            switches: [{ ...off, label: "Stop keeping commands" }, {
                    label: "Keep what they print too…", to: "output", said: "What shell commands print is kept from here on",
                    ask: SHELL_OUTPUT_ASK,
                }],
        };
    if (shell === "output")
        return {
            says: "Shell commands are kept, and what they print.",
            switches: [{ label: "Stop keeping what they print", to: "commands", said: "What shell commands print is no longer kept (the commands still are; what was kept stays)" }, { ...off, label: "Stop keeping both" }],
        };
    return null;
}
