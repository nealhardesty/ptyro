// The Chats page's logic (web/dist/chatkit.js): what the list shows and in what order (side chats
// under their chat), the compact CHATS section, the composer's `/` lines and `@` folders, the
// folders box, what redraws a message, and which error means chats are off.
//   node web/test/chats.mjs
import assert from "node:assert/strict";
import { atInsert, atQuery, chatsOff, folderList, itemKey, recentChats, shownChats, slashMessage, sorted, titleQuery } from "../dist/chatkit.js";

const chat = (id, o = {}) => ({ id, title: id, agent: "claude", workspace: "main", pinned: false, last_ms: 0, incognito: false, ...o });
const none = { query: "", workspace: "", imported: false, pinned: false };
const ids = (rows) => rows.map((r) => `${r.side ? "↳" : ""}${r.chat.id}`);

// ── `/` lines ───────────────────────────────────────────────────────────────────────────────
assert.equal(slashMessage("hello"), null, "not a slash line at all");
assert.equal(slashMessage("/retry"), null, "a command");
assert.equal(slashMessage("/edit say it again"), null);
assert.equal(slashMessage("//etc is odd"), "/etc is odd", "// sends the rest with one /");
assert.equal(slashMessage("/etc/hosts — what is it?"), "/etc/hosts — what is it?", "a path is a message as it is");
assert.equal(slashMessage("  /usr/bin"), "  /usr/bin");

// ── the folders box ─────────────────────────────────────────────────────────────────────────
assert.deepEqual(folderList("~/notes, /tmp\n\n  ~/src  \n,"), ["~/notes", "/tmp", "~/src"]);
assert.deepEqual(folderList(""), [], "emptied: no folders");

// ── the list: filters ───────────────────────────────────────────────────────────────────────
{
  const list = sorted([
    chat("a", { title: "Rust lifetimes", last_ms: 5 }),
    chat("b", { title: "Dinner", agent: "codex", last_ms: 9, workspace: "work" }),
    chat("p", { title: "Plan", pinned: true, last_ms: 1 }),
    chat("i", { title: "Secret", incognito: true, last_ms: 20 }),
    chat("m", { title: "Imported", source: "claude:x", source_dir: "~/repo", last_ms: 3 }),
  ]);
  assert.deepEqual(ids(shownChats(list, none)), ["p", "b", "a", "m"], "pinned first, then the newest; never incognito");
  assert.deepEqual(ids(shownChats(list, { ...none, query: "RUST" })), ["a"], "titles match whatever the case");
  assert.deepEqual(ids(shownChats(list, { ...none, query: "agent:codex" })), ["b"]);
  assert.deepEqual(ids(shownChats(list, { ...none, query: "dinner since:7d by:meaning -x" })), ["b"], "the search's operators aren't title words");
  assert.deepEqual(ids(shownChats(list, { ...none, workspace: "work" })), ["b"]);
  assert.deepEqual(ids(shownChats(list, { ...none, pinned: true })), ["p"]);
  assert.deepEqual(ids(shownChats(list, { ...none, imported: true })), ["m"]);
  const far = sorted([...list, chat("r", { title: "Remote", machine: "laptop2", last_ms: 4 })]);
  assert.deepEqual(ids(shownChats(far, { ...none, machine: "laptop2" })), ["r"], "one machine's");
  assert.ok(!ids(shownChats(far, { ...none, machine: "." })).includes("r"), "this machine's");
  assert.ok(ids(shownChats(far, none)).includes("r"), "every machine's");
  assert.deepEqual(ids(shownChats(list, { ...none, query: "~/repo" })), ["m"], "an imported chat by where it ran");
  assert.deepEqual(titleQuery("Agent:Codex \"Retry\" redi*"), { text: "retry redi", agent: "codex" });
}

// ── the list: side chats under their chat ───────────────────────────────────────────────────
{
  const list = sorted([
    chat("a", { last_ms: 10 }),
    chat("s1", { side_of: "a", last_ms: 11 }),
    chat("s2", { side_of: "a", last_ms: 12 }),
    chat("b", { last_ms: 5 }),
    chat("s3", { side_of: "b", last_ms: 30 }),
    chat("orphan", { side_of: "gone", last_ms: 40 }),
  ]);
  assert.deepEqual(ids(shownChats(list, none)), ["a", "↳s2", "↳s1", "b", "↳s3"], "each under its chat, newest first; one whose chat is gone isn't listed");
  assert.deepEqual(ids(shownChats(list, { ...none, query: "b" })), ["b", "↳s3"], "a side chat goes with its chat");
}

// ── the compact CHATS section ───────────────────────────────────────────────────────────────
{
  const list = [chat("p", { pinned: true, last_ms: 1 }), chat("a", { last_ms: 5 }), chat("b", { last_ms: 9 }), chat("c", { last_ms: 7 }), chat("s", { side_of: "a", last_ms: 99 }), chat("i", { incognito: true, last_ms: 50 })];
  assert.deepEqual(recentChats(list, 2).map((c) => c.id), ["p", "b", "c"], "every pinned one, then the newest `recent`");
  assert.deepEqual(recentChats(list, 0).map((c) => c.id), ["p"]);
}

// ── `@` folders ─────────────────────────────────────────────────────────────────────────────
{
  const folders = ["~/notes", "~/src/ptyro", "/tmp"];
  assert.equal(atQuery("hello", 5, folders), null);
  assert.equal(atQuery("look in @", 9, []), null, "no folders: nothing to list");
  assert.deepEqual(atQuery("look in @", 9, folders), { start: 8, matches: folders });
  assert.deepEqual(atQuery("look in @PT", 11, folders), { start: 8, matches: ["~/src/ptyro"] });
  assert.equal(atQuery("mail me@home", 12, folders), null, "an @ inside a word isn't one");
  assert.deepEqual(atQuery("@no and more", 3, folders), { start: 0, matches: ["~/notes"] }, "before the caret only");
  assert.deepEqual(atInsert("look in @no please", 11, 8, "~/notes"), { text: "look in ~/notes please", caret: 15 });
  assert.deepEqual(atInsert("look in @no", 11, 8, "~/notes"), { text: "look in ~/notes ", caret: 16 });
}

// ── what redraws a message ──────────────────────────────────────────────────────────────────
{
  const it = { id: "m1", t: "assistant", rev: 3, text: "x".repeat(10_000), streaming: true };
  const k = itemKey(it, undefined, "");
  assert.ok(k.length < 100, "a key, not the text");
  assert.equal(itemKey({ ...it }, undefined, ""), k, "the same item: the same key");
  assert.notEqual(itemKey({ ...it, text: it.text + "more" }, undefined, ""), k, "more text");
  assert.notEqual(itemKey({ ...it, streaming: false }, undefined, ""), k, "done streaming");
  assert.notEqual(itemKey({ ...it, rev: 4 }, undefined, ""), k, "a new revision");
  assert.notEqual(itemKey(it, { at: 1, of: 2 }, ""), k, "a branch beside it");
  assert.notEqual(itemKey(it, undefined, "find"), k, "find in the chat");
  assert.notEqual(itemKey(it, undefined, "", "x"), k, "what around it changed");
  const card = { id: "p1", t: "permission", tool: "Bash", title: "ls" };
  assert.notEqual(itemKey({ ...card, decided: "allow" }, undefined, ""), itemKey(card, undefined, ""), "a permission answered");
}

// ── chats off, or another failure ───────────────────────────────────────────────────────────
assert.ok(chatsOff({ code: "unsupported", message: "chats are off: set `[chat] enabled = true` (ptyro config set chat.enabled true)" }));
assert.ok(!chatsOff({ code: "timeout", message: "no answer: the connection may be down" }), "a slow daemon isn't chats off");
assert.ok(!chatsOff({ code: "daemon_unavailable", message: "connection lost" }));
assert.ok(!chatsOff({ code: "unsupported", message: "unknown request" }), "an older daemon isn't chats off");

console.log("ok");
