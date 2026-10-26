// The web app's scratchpad logic (web/dist/pad.js and the wire shapes app.js uses), run by
// crates/ptyro/tests/m9.rs when Node is available: the Markdown reader never turns text into
// markup, the offline queue keeps order and coalesces edits, and against a real daemon through
// `ptyro serve`: add, a stale put that merges, ticking by line, the change event, and a
// read-only device being refused.
//   node web/test/pad.mjs ws://127.0.0.1:PORT/ws CONTROL_TOKEN READ_TOKEN
import assert from "node:assert/strict";
import { Queue, noteFor, padId, padSel, parseMarkdown, safeHref, shareLine } from "../dist/pad.js";

// ── Markdown ───────────────────────────────────────────────────────────────────────────────
{
  const blocks = parseMarkdown("# Title\n\n- [ ] open <b>x</b>\n- [x] done\n  more words\n- plain\n\n`code` and **bold** and *it*\n");
  assert.deepEqual(blocks[0], { k: "h", level: 1, inl: [{ t: "text", text: "Title" }] });
  const li = blocks.filter((b) => b.k === "li");
  assert.equal(li.length, 3);
  assert.deepEqual([li[0].line, li[0].check, li[1].line, li[1].check, li[2].check], [3, false, 4, true, null]);
  // Markup in the text stays text: the renderer only ever makes text nodes.
  assert.deepEqual(li[0].inl, [{ t: "text", text: "open <b>x</b>" }]);
  // A continuation line joins the item above it.
  assert.ok(li[1].inl.map((i) => i.text).join("").includes("more words"));
  const p = blocks.at(-1);
  assert.deepEqual(p.inl.map((i) => i.t), ["code", "text", "b", "text", "i"]);
  // Links: only safe schemes become links.
  assert.equal(safeHref("javascript:alert(1)"), null);
  assert.equal(safeHref("data:text/html,x"), null);
  assert.equal(safeHref("https://example.com/a?b=1"), "https://example.com/a?b=1");
  assert.equal(safeHref("ptyro://session/abc"), "ptyro://session/abc");
  const links = parseMarkdown("[bad](javascript:alert(1)) [good](https://e.com) see https://x.org/y and (ptyro://session/s1)")[0].inl;
  assert.deepEqual(links.filter((i) => i.t === "a").map((i) => i.href), ["https://e.com", "https://x.org/y", "ptyro://session/s1"]);
  assert.ok(links.some((i) => i.t === "text" && i.text.includes("javascript:")), "the unsafe link is kept as text");
  assert.equal(parseMarkdown("```\n- [ ] not an item\n```")[0].k, "code");
}

// ── share and note text ────────────────────────────────────────────────────────────────────
{
  assert.equal(shareLine(new URLSearchParams("title=Cool&text=read+this&url=https%3A%2F%2Fe.com")), "Cool — read this — https://e.com");
  assert.equal(shareLine(new URLSearchParams("text=look+https%3A%2F%2Fe.com&url=https%3A%2F%2Fe.com")), "look https://e.com");
  assert.equal(shareLine(new URLSearchParams("title=&text=")), null);
  assert.equal(noteFor({ name: "n", handle: "ab12", task: "fix the thing\nmore" }), "fix the thing (ptyro://session/ab12)");
}

// ── the queue ──────────────────────────────────────────────────────────────────────────────
{
  let saved = null;
  const store = { load: async () => saved, save: async (v) => { saved = v; } };
  const q = new Queue(store);
  await q.push({ k: "add", text: "one" });
  await q.push({ k: "put", base: "r1", text: "edit 1" });
  await q.push({ k: "add", text: "two" });
  await q.push({ k: "put", base: "r2", text: "edit 2" });
  assert.deepEqual(q.ops, [
    { k: "add", text: "one" },
    { k: "put", base: "r1", text: "edit 2" },
    { k: "add", text: "two" },
  ], "edits coalesce: the oldest base, the newest text; the order of the rest holds");
  assert.ok(saved, "persisted");
  const again = new Queue(store);
  await again.load();
  assert.equal(again.length, 3, "it survives a reload");
  // A failure keeps that op and everything after it.
  const seen = [];
  const n = await q.flush(async (op) => { if (op.k === "put") throw new Error("down"); seen.push(op.text); });
  assert.deepEqual([n, seen, q.length], [1, ["one"], 2]);
  const sent = [];
  assert.equal(await q.flush(async (op) => { sent.push(op.k); }), 2);
  assert.deepEqual(sent, ["put", "add"]);
  assert.equal(saved, null, "an empty queue is forgotten");
  // Storage that throws never breaks it.
  const broken = new Queue({ load: async () => { throw new Error("x"); }, save: async () => { throw new Error("x"); } });
  await broken.load();
  await broken.push({ k: "add", text: "still works" });
  assert.equal(broken.length, 1);
}

// ── which pad (NOTE1.7) ────────────────────────────────────────────────────────────────────
{
  // The wire shapes: "global", or { project: id }; a reply without one is the global pad.
  assert.equal(padSel("global"), "global");
  assert.deepEqual(padSel("p1"), { project: "p1" });
  assert.equal(padId(undefined), "global");
  assert.equal(padId("global"), "global");
  assert.equal(padId({ project: "p1" }), "p1");
  assert.equal(padId({ near: "/x" }), "global");
  // Edits coalesce per pad; an op queued before pads existed (no pad) is the global pad's.
  const q = new Queue({ load: async () => null, save: async () => {} });
  await q.push({ k: "put", base: "a", text: "g1" });
  await q.push({ k: "put", base: "b", text: "p1", pad: "proj" });
  await q.push({ k: "put", base: "a", text: "g2", pad: "global" });
  await q.push({ k: "put", base: "c", text: "p2", pad: "proj" });
  assert.deepEqual(q.ops, [
    { k: "put", base: "a", text: "g2", pad: "global" },
    { k: "put", base: "b", text: "p2", pad: "proj" },
  ]);
}

// ── a real daemon ──────────────────────────────────────────────────────────────────────────
const [url, control, read] = process.argv.slice(2);
if (url) {
  const timer = setTimeout(() => { console.error("timed out"); process.exit(2); }, 30000);
  const open = (token) => new Promise((ok, fail) => {
    const ws = new WebSocket(url);
    let id = 0;
    const pending = new Map();
    const events = [];
    ws.onopen = () => ws.send(JSON.stringify({ t: "hello", proto: { major: 1, minor: 8 }, client: { kind: "web", version: "test", pid: 0, token }, features: [] }));
    ws.onmessage = (ev) => {
      const m = JSON.parse(ev.data);
      if (m.t === "welcome") {
        ws.send(JSON.stringify({ t: "watch_events" }));
        ok({
          events,
          close: () => ws.close(),
          req: (req) => new Promise((res, rej) => {
            pending.set(++id, (o) => (o.t === "ok" ? res(o.reply) : rej(Object.assign(new Error(o.error.message), { code: o.error.code }))));
            ws.send(JSON.stringify({ t: "request", id, req }));
          }),
        });
      } else if (m.t === "reject") fail(new Error(m.reason));
      else if (m.t === "response") pending.get(m.id)?.(m.outcome);
      else if (m.t === "event") events.push(m.event);
    };
  });
  const until = async (pred) => { for (let i = 0; i < 200; i++) { const v = pred(); if (v) return v; await new Promise((r) => setTimeout(r, 50)); } throw new Error("never happened"); };

  const c = await open(control);
  const start = (await c.req({ t: "scratch_get" })).doc;
  assert.ok(start.text.startsWith("# Scratchpad"), start.text);

  // Add, as the quick-add box sends it.
  const added = (await c.req({ t: "scratch_add", text: "from the web", section: null, note: false, stamp: true, session: null, cwd: null })).doc;
  assert.match(added.text, /- \[ \] \d{4}-\d{2}-\d{2} \d{2}:\d{2} · .*from the web\n$/);
  assert.equal(added.open, start.open + 1);
  await until(() => c.events.find((e) => e.t === "scratch_changed" && e.rev === added.rev));

  // A stale edit (from `start`'s revision) merges with the add, and says so.
  const edited = start.text.replace("# Scratchpad", "# My notes");
  const put = await c.req({ t: "scratch_put", base: start.rev, text: edited });
  assert.equal(put.conflicts, 0);
  assert.ok(put.doc.text.startsWith("# My notes") && put.doc.text.includes("from the web"), put.doc.text);
  assert.notEqual(put.doc.text, edited, "the merged text differs from what was sent");

  // Tick by line, the way the Preview's checkbox does; untick.
  const line = put.doc.text.split("\n").findIndex((l) => l.includes("from the web")) + 1;
  let marked = await c.req({ t: "scratch_mark", sel: String(line), mark: { done: { pr: null } } });
  assert.equal(marked.items[0].done, true);
  assert.equal((await c.req({ t: "scratch_get" })).doc.done, 1);
  marked = await c.req({ t: "scratch_mark", sel: String(line), mark: "open" });
  assert.equal(marked.items[0].done, false);
  const todo = await c.req({ t: "scratch_todo", all: false });
  assert.deepEqual(todo.items.map((i) => i.text), ["from the web"]);

  // History lists saved copies and a restore puts one back.
  const log = (await c.req({ t: "scratch_history" })).snapshots;
  assert.ok(log.length > 0 && log[0].rev && log[0].at_ms);
  const back = (await c.req({ t: "scratch_restore", rev: log.at(-1).rev })).doc;
  assert.ok(back.text.startsWith("# Scratchpad"));

  // A device paired without control reads, and its writes are refused with a code the app understands.
  const r = await open(read);
  assert.ok((await r.req({ t: "scratch_get" })).doc.path);
  for (const req of [
    { t: "scratch_add", text: "no", section: null, note: false, stamp: true, session: null, cwd: null },
    { t: "scratch_put", base: back.rev, text: "no" },
    { t: "scratch_mark", sel: "1", mark: "open" },
  ]) {
    await assert.rejects(r.req(req), (e) => e.code === "permission_denied");
  }
  c.close(); r.close();
  clearTimeout(timer);
}
console.log("ok");
process.exit(0);
