// The web app's shared helpers (web/dist/kit.js and conn.js) that need no browser: route decoding,
// the link rule, the one-at-a-time guard, the cache that drops a reply from before a change, and
// the connection's timeouts and keepalive. A few globals stand in for the page.
//   node web/test/kit.mjs
import assert from "node:assert/strict";

const listeners = [];
globalThis.window = { addEventListener: (...a) => listeners.push(a), open: () => {}, setTimeout, clearTimeout };
globalThis.document = { addEventListener: () => {}, visibilityState: "visible", getElementById: () => null };
globalThis.localStorage = { getItem: () => { throw new Error("blocked"); }, setItem: () => { throw new Error("blocked"); }, removeItem: () => {} };
globalThis.location = { origin: "http://x", hash: "" };
globalThis.WebSocket = class { static OPEN = 1; readyState = 1; send() {} close() {} };

const { safeDecode, webHref, once, Cache } = await import("../dist/kit.js");
const { conn, store, token } = await import("../dist/conn.js");

// A malformed escape in a route is kept as typed instead of throwing.
assert.equal(safeDecode("a%20b"), "a b");
assert.equal(safeDecode("%E0%A4%A"), "%E0%A4%A");
// Only http(s) is ever followed from data.
assert.equal(webHref("https://example.com/a?b=1"), "https://example.com/a?b=1");
assert.equal(webHref("http://example.com/"), "http://example.com/");
for (const bad of ["javascript:alert(1)", "data:text/html,x", "ptyro://x", "/relative", "", null, undefined, 5]) assert.equal(webHref(bad), null, String(bad));
// localStorage that throws is not a crash.
assert.equal(token(), null);
store.set("k", "v");

// once(): a second call while the first is running does nothing.
{
  let calls = 0;
  const slow = once(async () => { calls++; await new Promise((r) => setTimeout(r, 20)); return "done"; });
  const [a, b] = await Promise.all([slow(), slow()]);
  assert.deepEqual([a, b, calls], ["done", undefined, 1]);
  assert.equal(await slow(), "done", "free again afterwards");
}

// Cache: a reply to a fetch that began before a reload doesn't put the old data back.
{
  conn.state = "open";
  let n = 0;
  const gates = [];
  const c = new Cache(() => new Promise((ok) => { const i = ++n; gates.push(() => ok(`v${i}`)); }));
  assert.equal(c.get("k"), undefined); // fetch 1 starts
  c.reload("k"); // the data changed meanwhile
  assert.equal(c.get("k"), undefined); // fetch 2 starts
  gates[0](); // the old reply lands late
  await new Promise((r) => setTimeout(r, 5));
  assert.equal(c.get("k"), undefined, "the stale reply was dropped");
  gates[1]();
  await new Promise((r) => setTimeout(r, 5));
  assert.equal(c.get("k"), "v2");
}

// Connection: a request with no answer times out and says why; a dropped send is reported.
{
  conn.state = "open";
  conn.ws = new WebSocket();
  let dropped = 0;
  conn.onDropped = () => dropped++;
  const id = conn.nextId;
  const err = await conn.request({ t: "status" }, 30).catch((e) => e);
  assert.equal(err.code, "timeout");
  assert.ok(!conn.pending.has(id), "forgotten");
  conn.pending.clear(); // (the timeout asked the connection a question of its own)
  conn.ws.readyState = 3;
  assert.equal(conn.send({ t: "input" }), false);
  assert.equal(dropped, 1);
  // A request that says how long it will wait isn't cut short.
  conn.ws.readyState = 1;
  const lid = conn.nextId;
  const long = conn.request({ t: "send_text", wait_ready_ms: 1000 }, 10);
  await new Promise((r) => setTimeout(r, 40));
  assert.ok(conn.pending.has(lid), "still waiting");
  conn.pending.get(lid)({ t: "ok", reply: "r" });
  assert.equal(await long, "r");
  // reset() fails what waits and starts over.
  const waiting = conn.request({ t: "x" }).catch((e) => e.code);
  conn.reset();
  assert.equal(await waiting, "daemon_unavailable");
  assert.equal(conn.state, "closed");
}
console.log("ok");
process.exit(0);
