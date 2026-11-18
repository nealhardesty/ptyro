// The Chats page's logic (web/dist/chatkit.js): what the list shows and in what order, the
// compact CHATS section, the composer's `/` lines and `@` folders, the
// folders box, what redraws a message, and which error means chats are off.
//   node web/test/chats.mjs
import assert from "node:assert/strict";
import { atInsert, atQuery, chatsOff, folderList, itemKey, recentChats, shownChats, slashMessage, sorted, titleQuery } from "../dist/chatkit.js";

const chat = (id, o = {}) => ({ id, title: id, agent: "claude", workspace: "main", pinned: false, last_ms: 0, incognito: false, ...o });
const none = { query: "", workspace: "", pinned: false };
const ids = (rows) => rows.map((c) => c.id);

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
    chat("m", { title: "Mail", last_ms: 3 }),
  ]);
  assert.deepEqual(ids(shownChats(list, none)), ["p", "b", "a", "m"], "pinned first, then the newest; never incognito");
  assert.deepEqual(ids(shownChats(list, { ...none, query: "RUST" })), ["a"], "titles match whatever the case");
  assert.deepEqual(ids(shownChats(list, { ...none, query: "agent:codex" })), ["b"]);
  assert.deepEqual(ids(shownChats(list, { ...none, query: "dinner since:7d by:meaning -x" })), ["b"], "the search's operators aren't title words");
  assert.deepEqual(ids(shownChats(list, { ...none, workspace: "work" })), ["b"]);
  assert.deepEqual(ids(shownChats(list, { ...none, pinned: true })), ["p"]);
  const far = sorted([...list, chat("r", { title: "Remote", machine: "laptop2", last_ms: 4 })]);
  assert.deepEqual(ids(shownChats(far, { ...none, machine: "laptop2" })), ["r"], "one machine's");
  assert.ok(!ids(shownChats(far, { ...none, machine: "." })).includes("r"), "this machine's");
  assert.ok(ids(shownChats(far, none)).includes("r"), "every machine's");
  assert.deepEqual(titleQuery("Agent:Codex \"Retry\" redi*"), { text: "retry redi", agent: "codex" });
}

// ── the compact CHATS section ───────────────────────────────────────────────────────────────
{
  const list = [chat("p", { pinned: true, last_ms: 1 }), chat("a", { last_ms: 5 }), chat("b", { last_ms: 9 }), chat("c", { last_ms: 7 }), chat("i", { incognito: true, last_ms: 50 })];
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
  const k = itemKey(it, "");
  assert.ok(k.length < 100, "a key, not the text");
  assert.equal(itemKey({ ...it }, ""), k, "the same item: the same key");
  assert.notEqual(itemKey({ ...it, text: it.text + "more" }, ""), k, "more text");
  assert.notEqual(itemKey({ ...it, streaming: false }, ""), k, "done streaming");
  assert.notEqual(itemKey({ ...it, rev: 4 }, ""), k, "a new revision");
  assert.notEqual(itemKey(it, "find"), k, "find in the chat");
  assert.notEqual(itemKey(it, "", "x"), k, "what around it changed");
  const card = { id: "p1", t: "permission", tool: "Bash", title: "ls" };
  assert.notEqual(itemKey({ ...card, decided: "allow" }, ""), itemKey(card, ""), "a permission answered");
}

// ── chats off, or another failure ───────────────────────────────────────────────────────────
assert.ok(chatsOff({ code: "unsupported", message: "chats are off: set `[chat] enabled = true` (ptyro config set chat.enabled true)" }));
assert.ok(!chatsOff({ code: "timeout", message: "no answer: the connection may be down" }), "a slow daemon isn't chats off");
assert.ok(!chatsOff({ code: "daemon_unavailable", message: "connection lost" }));
assert.ok(!chatsOff({ code: "unsupported", message: "unknown request" }), "an older daemon isn't chats off");

console.log("ok");
