// The Library's logic (web/dist/libkit.js): what the list asks the daemon for under the box and
// the filters, the kinds on offer, the days of the history, a snippet's matches and where they
// are in a text, an address's parts, what forgetting names, and the daemon's question.
//   node web/test/library.mjs
import assert from "node:assert/strict";
import { PAGE, byProject, clock, conversation, day, forgetAddr, found, historyRequest, historyText, ingestRequest, kindsOffered, lit, matched, noFilters, noteRequest, notesRequest, planLine, question, request, rootBad, rootBusy, rootHolds, rootReads, rootState, rootsRequest, searching, snippetParts, split, tagsOf, when, wroteBy } from "../dist/libkit.js";

const DAY = 86_400_000;
const f = (o = {}) => ({ ...noFilters(), ...o });

// ── what is asked ───────────────────────────────────────────────────────────────────────────
assert.ok(!searching("re") && !searching("  re  ") && searching("ret") && searching("日本語"), "a search starts at 3 characters");
// Nothing typed, no filter: the history, a page of it; fewer than 3 letters are not a search.
assert.deepEqual(request(f(), 0), { t: "kb_recent", kinds: [], project: null, limit: PAGE });
assert.equal(request(f({ query: "re" }), 0).t, "kb_recent");
// The history under the filters: a hit's kind is its item's, since is a time, and a row's
// history is that session's or worktree's alone.
assert.deepEqual(
  request(f({ kind: "comment", project: "p1", since: "7d", of: { t: "worktree", id: "w1", name: "feeder/fix" } }), 10 * DAY),
  { t: "kb_recent", kinds: ["file"], project: "p1", limit: PAGE, worktree: "w1", since_ms: 3 * DAY });
assert.equal(request(f({ of: { t: "session", id: "s1", name: "brisk-rook" } }), 0).session, "s1");
// The next page: older than what is shown.
assert.equal(request(f(), 0, 1234).before_ms, 1234);
// A linked machine's history is asked of it; this computer's own needs no name.
assert.equal(request(f({ machine: "laptop2" }), 0).machine, "laptop2");
assert.ok(!("machine" in request(f({ machine: "here" }), 0)));
// Chats have no history here.
assert.equal(request(f({ kind: "chat" }), 0), null);
// A search: the box's words, `since:` in them, the kind and the project beside them; every
// linked machine too unless one is picked; more within a row's history.
assert.deepEqual(request(f({ query: "  retry policy ", kind: "comment", project: "p1", since: "7d" }), 0),
  { t: "kb_search", query: "retry policy since:7d", project: "p1", kinds: ["comment"], limit: 30, machines: true });
assert.deepEqual(request(f({ query: "retry", machine: "laptop2" }), 0),
  { t: "kb_search", query: "retry", project: null, kinds: [], limit: 30, machines: false, machine: "laptop2" });
assert.equal(request(f({ query: "retry", machine: "here" }), 0).machine, "here");
assert.equal(request(f({ query: "retry", kind: "chat" }), 0).t, "kb_search", "chats are searched");
assert.equal(request(f({ query: "retry", of: { t: "session", id: "s1", name: "x" } }), 0).limit, 100);
// Who wrote it: the history asks for that writer's; a search says it in the query's own word.
assert.equal(request(f({ wrote: "you" }), 0).writer, "user");
assert.equal(request(f({ wrote: "agent" }), 0).writer, "agent");
assert.ok(!("writer" in request(f(), 0)));
assert.equal(request(f({ query: "retry", since: "7d", wrote: "agent" }), 0).query, "retry since:7d wrote:agent");

// ── notes ───────────────────────────────────────────────────────────────────────────────────
assert.deepEqual(notesRequest(f()), { t: "kb_notes" });
assert.deepEqual(notesRequest(f({ project: "p1", wrote: "you" })), { t: "kb_notes", project: "p1", writer: "user" });
{
  // By what they are about: the ones about nothing first, then each project's, as they came.
  const n = (title, project_name) => ({ title, project_name });
  const groups = byProject([n("a", "web"), n("b", null), n("c", "feeder"), n("d", "web"), n("e")]);
  assert.deepEqual(groups.map(([name, list]) => [name, list.map((i) => i.title).join("")]),
    [["Global (about no project)", "be"], ["feeder", "c"], ["web", "ad"]]);
  assert.deepEqual(byProject([]), []);
}
assert.deepEqual(tagsOf(" db, decision  #api,, "), ["db", "decision", "api"]);
assert.deepEqual(tagsOf(""), []);
// A new note sends its text, and only what was filled in.
assert.deepEqual(noteRequest({ text: "we chose X" }), { t: "kb_note", text: "we chose X" });
assert.deepEqual(noteRequest({ text: "x", title: "X", project: "p1", profile: "work", tags: "db decision" }),
  { t: "kb_note", text: "x", title: "X", project: "p1", profile: "work", tags: ["db", "decision"] });
// Reading a folder: its path, and only what was changed from how the form stood.
assert.deepEqual(ingestRequest({ path: " ~/notes ", comments: true, code: false, commits: true, once: false }),
  { t: "kb_ingest", spec: { path: "~/notes", options: {} } });
assert.deepEqual(ingestRequest({ path: "/src/docs", name: "docs", project: "feeder", profile: "work", include: "*.md, guides/**", exclude: "drafts", comments: false, code: true, commits: false, once: true }),
  { t: "kb_ingest", spec: { path: "/src/docs", name: "docs", project: "feeder", profile: "work", options: { include: ["*.md", "guides/**"], exclude: ["drafts"], comments: false, code: true, commits: false, once: true } } });
// On a linked machine: named in the request; this computer needs no name.
assert.equal(ingestRequest({ path: "/x", machine: "laptop2" }).machine, "laptop2");
assert.ok(!("machine" in ingestRequest({ path: "/x", machine: "" })));

// ── sources ─────────────────────────────────────────────────────────────────────────────────
assert.deepEqual(rootsRequest(), { t: "kb_roots" });
assert.deepEqual(rootsRequest("here"), { t: "kb_roots" });
assert.deepEqual(rootsRequest("laptop2"), { t: "kb_roots", machine: "laptop2" });
{
  const root = (o = {}) => ({ id: "R1", name: "docs", kind: "folder", path: "/src/docs", state: "ok", files: 12, commits: 3, chunks: 40, ...o });
  const since = () => "2m";
  // How it stands: a read under way says how far, before anything else.
  assert.equal(rootState(root({ synced_ms: 5 }), since), "read 2m ago");
  assert.equal(rootState(root(), since), "not read yet");
  assert.equal(rootState(root({ state: "syncing" }), since), "being read");
  assert.equal(rootState(root({ state: "syncing", progress: [3, 10] }), since), "reading 3 of 10");
  assert.equal(rootState(root({ progress: [0, 0] }), since), "waiting to be read");
  assert.equal(rootState(root({ state: "missing", progress: [0, 0], removing: true }), since), "being removed");
  assert.equal(rootState(root({ state: "missing" }), since), "missing");
  assert.equal(rootState(root({ state: "error" }), since), "failing");
  assert.ok(rootBad(root({ state: "missing" })) && rootBad(root({ state: "error" })) && !rootBad(root({ state: "syncing" })));
  assert.ok(rootBusy(root({ progress: [0, 0] })) && rootBusy(root({ removing: true })) && !rootBusy(root()));
  assert.equal(rootHolds(root()), "12 files, 3 commits, 40 pieces");
  assert.equal(rootHolds(root({ files: 1, commits: 0, chunks: 1 })), "1 file, 1 piece");
  // What it says of its own reading; nothing where the settings decide.
  assert.equal(rootReads(root()), "");
  assert.equal(rootReads(root({ options: { include: ["*.md"], exclude: ["drafts", "old"], comments: false, code: true, once: true } })),
    "no comments · code · only *.md · not drafts, old · read once");
}
// What a read will take, as the command says it.
assert.equal(planLine({ files: 6, documents: 3, sources: 1, comments: true, code: false, commits: true }), "6 files: 3 documents, 1 source file (comments), its commits");
assert.equal(planLine({ files: 1, documents: 0, sources: 0, comments: true, code: false, commits: false, existing: true }), "1 file (read already: looked at again)");
assert.equal(planLine({ files: 2, documents: 0, sources: 2, comments: false, code: false, commits: false }), "2 files: 2 source files (names only)");
// The agents' history: counted first (`dry_run`), with only what was picked.
assert.deepEqual(historyRequest({ agent: "", profile: "", since: "", dry: false, machine: "" }, 0, true), { t: "kb_history", dry_run: true });
assert.deepEqual(historyRequest({ agent: "codex", profile: "work", since: "30d", machine: "laptop2" }, 40 * DAY, false),
  { t: "kb_history", dry_run: false, agent: "codex", profile: "work", since_ms: 10 * DAY, machine: "laptop2" });
assert.equal(historyText({ found: 0, known: 0, old: 0, chats: 0, new: 0 }), "No conversations in your agents' folders");
assert.equal(historyText({ found: 12, known: 3, old: 1, chats: 0, new: 8 }), "12 conversations in your agents' folders: 3 kept already (or forgotten, or private), 1 too old to keep, 8 to read");
assert.equal(wroteBy({ writer: "user", agent: null }), "you");
assert.equal(wroteBy({ writer: "agent", agent: "claude@work" }), "claude@work");
assert.equal(wroteBy({ writer: "agent" }), "agent");

// ── the kinds on offer ──────────────────────────────────────────────────────────────────────
assert.deepEqual(kindsOffered([]), ["session", "chat"], "nothing that isn't kept yet");
assert.deepEqual(kindsOffered([{ kind: "session", items: 3 }, { kind: "note", items: 0 }]), ["session", "chat"]);
assert.deepEqual(kindsOffered([{ kind: "file", items: 2 }, { kind: "shell", items: 1 }]), ["session", "chat", "doc", "comment", "code", "path", "command"]);

// ── days ────────────────────────────────────────────────────────────────────────────────────
{
  // (In this device's time, whatever its zone: built from local parts.)
  const at = (y, m, d, hh = 12, mm = 0) => new Date(y, m - 1, d, hh, mm).getTime();
  const now = at(2026, 11, 19, 20);
  assert.equal(day(at(2026, 11, 19, 0, 5), now), "Today");
  assert.equal(day(at(2026, 11, 18, 23, 55), now), "Yesterday", "by the calendar, not by 24 hours");
  assert.equal(day(at(2026, 11, 17), now), "Tue 2026-11-17");
  assert.equal(day(at(2026, 1, 4), now), "Sun 2026-01-04");
  assert.equal(clock(at(2026, 11, 19, 9, 5)), "09:05");
  assert.equal(when({ ended_ms: 3, started_ms: 2, ingested_ms: 1 }), 3);
  assert.equal(when({ started_ms: 2, ingested_ms: 1 }), 2);
  assert.equal(when({ ingested_ms: 1 }), 1);
  assert.equal(when({}), 0);
}

// ── matches ─────────────────────────────────────────────────────────────────────────────────
{
  const s = "a \u0002Retry\u0003 and\na \u0002retry\u0003 \u0002policy\u0003";
  assert.deepEqual(snippetParts(s).filter((p) => p.on).map((p) => p.text), ["Retry", "retry", "policy"]);
  assert.equal(snippetParts(s).map((p) => p.text).join(""), "a Retry and a retry policy", "the marks are never shown; a newline is a space");
  assert.deepEqual(matched(s), ["retry", "policy"]);
  assert.deepEqual(matched("nothing marked"), []);
  assert.deepEqual(lit("The Retry policy retries", ["retry", "policy"]), [
    { text: "The ", on: false }, { text: "Retry", on: true }, { text: " ", on: false }, { text: "policy", on: true }, { text: " retries", on: false },
  ]);
  assert.deepEqual(lit("plain", []), [{ text: "plain", on: false }]);
  assert.deepEqual(lit("", ["x"]), []);
  // A character whose lowercase is longer: shown plain, never cut in the wrong place.
  assert.deepEqual(lit("İstanbul retry", ["retry"]), [{ text: "İstanbul retry", on: false }]);
  const chunks = [{ idx: 0, text: "Task: the Feeder" }, { idx: 1, text: "nothing" }, { idx: 4, text: "x", heading: "SPEC › the feeder" }];
  assert.deepEqual(found(chunks, " feeder "), [0, 4]);
  assert.deepEqual(found(chunks, "  "), []);
}

// ── addresses ───────────────────────────────────────────────────────────────────────────────
assert.deepEqual(split("k:7f3k/12"), { item: "k:7f3k", piece: 12 });
assert.deepEqual(split("k:7f3k"), { item: "k:7f3k", piece: null });
assert.deepEqual(split("laptop2:k:7f3k/0"), { item: "laptop2:k:7f3k", piece: 0 });
assert.deepEqual(split("c:aaaa/01jbnode"), { item: "c:aaaa/01jbnode", piece: null }, "a chat's message is no piece");
assert.equal(forgetAddr({ addr: "k:7f3k", item: { id: "01JB7F3K" } }), "01JB7F3K", "an item here, by its id");
assert.equal(forgetAddr({ addr: "laptop2:k:7f3k", item: { id: "01JB7F3K" } }), "laptop2:k:7f3k", "another machine's, by its address there");
assert.equal(conversation("session://claude/1f3d-7b16"), "1f3d-7b16");

// ── the daemon's question ───────────────────────────────────────────────────────────────────
assert.deepEqual(question("Forget 1 item: session “why? because”? The text is deleted for good. Really."),
  ["Forget 1 item: session “why? because”?", "The text is deleted for good. Really."], "a title with a question in it stays whole");
assert.deepEqual(question("Forget 2 items: session “a”, session “b”? The text is deleted."), ["Forget 2 items: session “a”, session “b”?", "The text is deleted."]);
assert.equal(question("no question here"), null);

console.log("ok");
