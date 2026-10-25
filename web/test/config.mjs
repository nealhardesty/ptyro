// The web app's settings panel (web/dist/cfgkit.js and the wire shapes panel.js uses), run by
// crates/ptyro/tests/m9.rs when Node is available:
//   node web/test/config.mjs units                    search, labels, lists, key names, what shows
//   node web/test/config.mjs host http://127.0.0.1:PORT FULL_TOKEN   against a real `ptyro serve`
import assert from "node:assert/strict";
import { fieldsOf, fromLines, hexOf, keyLabel, labelOf, needed, relevant, score, summary, toLines } from "../dist/cfgkit.js";

const mode = process.argv[2];

if (mode === "units") {
  // Labels read as words.
  assert.equal(labelOf("ui.sidebar_width"), "sidebar width");
  assert.equal(labelOf("agents.aider.command", true), "command");
  assert.equal(labelOf("notifications.callback.listen"), "callback.listen");
  // Search: every word, any order; the key ranks over the help; no match is -1.
  assert.ok(score("sidebar width", "ui.sidebar_width", "How wide the sidebar is, in columns.", "30") > score("wide", "ui.sidebar", "How wide the sidebar is.", "on") - 100);
  assert.ok(score("sidebar", "ui.sidebar_width", "x", "30") > score("sidebar", "ui.theme", "the sidebar colour", "dark"));
  assert.equal(score("tailscale phone", "remote.tailscale", "Put Tailscale's HTTPS in front for your phone.", "off") > 0, true);
  assert.equal(score("zzz", "ui.theme", "The colour scheme.", "dark"), -1);
  assert.equal(score("", "ui.theme", "", ""), 0);
  assert.ok(score("dark", "ui.theme", "The colour scheme.", "ptyro-dark") > 0, "a value counts");
  // Lists are lines in a box and JSON on the wire.
  assert.equal(toLines(["~/a", "~/b"]), "~/a\n~/b");
  assert.equal(toLines(null), "");
  assert.equal(fromLines(" ~/a \n\n~/b\n"), '["~/a","~/b"]');
  assert.equal(fromLines(""), "[]");
  // Key presses are named as the config names them.
  const k = (key, o = {}) => keyLabel({ key, ctrlKey: false, altKey: false, shiftKey: false, ...o });
  assert.equal(k("a"), "a");
  assert.equal(k("a", { ctrlKey: true }), "C-a");
  assert.equal(k("A", { ctrlKey: true, shiftKey: true }), "C-S-a");
  assert.equal(k("1", { altKey: true }), "M-1");
  assert.equal(k(" "), "Space");
  assert.equal(k(" ", { ctrlKey: true }), "C-Space");
  assert.equal(k("ArrowUp"), "Up");
  assert.equal(k("ArrowUp", { shiftKey: true }), "S-Up");
  assert.equal(k("Tab", { shiftKey: true }), "S-Tab");
  assert.equal(k("A"), "A", "a capital says Shift itself");
  assert.equal(k("?"), "?");
  assert.equal(k("F5"), "F5");
  assert.equal(k("Shift", { shiftKey: true }), null, "a modifier alone is no key");
  // Colours.
  assert.equal(hexOf("#A0b1C2"), "#a0b1c2");
  assert.equal(hexOf("bright-blue"), null);
  assert.equal(hexOf(75), null);
  // A sink's fields show for its type only.
  const entries = [{ key: "notifications.sinks.0.type", value: "ntfy", member: ["notifications.sinks", "0"] }, { key: "notifications.sinks.0.url", value: "x", member: ["notifications.sinks", "0"] }, { key: "notifications.sinks.0.webhook", value: null, member: ["notifications.sinks", "0"] }];
  const url = { when: { key: "type", any: ["ntfy"] } }, hook = { when: { key: "type", any: ["slack", "discord"] } };
  assert.equal(relevant(entries[1], url, entries), true);
  assert.equal(relevant(entries[2], hook, entries), false);
  assert.equal(relevant({ key: "ui.theme", member: null }, { when: null }, entries), true);
  // Members: their fields, their one line, and what a new one needs.
  const view = { entries: [{ key: "agents.aider.command", shown: "aider --model sonnet", member: ["agents", "aider"], changed: true }, { key: "agents.aider.adapter", shown: "(from name)", member: ["agents", "aider"] }, { key: "agents.pair.command", shown: "x", member: ["agents", "pair"] }] };
  assert.deepEqual(fieldsOf(view, "agents", "aider").map((e) => e.key), ["agents.aider.command", "agents.aider.adapter"]);
  assert.equal(summary("agents", "aider", fieldsOf(view, "agents", "aider")), "aider --model sonnet");
  assert.equal(summary("agents", "x", []), "runs as its name");
  assert.deepEqual(needed("notifications.sinks", "pushover"), ["token", "user"]);
  assert.deepEqual(needed("editors", ""), ["command"]);
  assert.deepEqual(needed("agents", ""), []);
  console.log("ok");
} else if (mode === "host") {
  const [base, token] = process.argv.slice(3);
  const call = async (op, args = {}) => {
    const r = await fetch(`${base}/api/host`, { method: "POST", headers: { "Content-Type": "application/json", Authorization: `Bearer ${token}` }, body: JSON.stringify({ op, ...args }) });
    const o = await r.json();
    if (o.t === "err") throw Object.assign(new Error(o.error.message), { code: o.error.code });
    return o.reply;
  };
  const schema = await call("settings.schema");
  assert.ok(schema.user.settings.length > 100 && schema.project.settings.length >= 12);
  assert.ok(schema.user.sections.some((s) => s.id === "interface") && schema.project.sections.some((s) => s.id === "scripts"));
  const theme = schema.user.settings.find((s) => s.key === "ui.theme");
  assert.deepEqual(theme.kind, { t: "dynamic", source: "themes" });
  assert.equal(schema.user.settings.find((s) => s.key === "ui.on_stop").kind.choices[1].value, "stay");
  assert.equal(schema.user.settings.find((s) => s.key === "ui.sidebar_width").kind.min, 12);
  assert.equal(schema.user.collections.find((c) => c.path === "keys").layout.map.t, "dynamic");

  let view = await call("settings.view", { scope: "user" });
  const entry = (k) => view.entries.find((e) => e.key === k);
  assert.equal(entry("ui.theme").origin, "default");
  assert.equal(entry("ui.theme").shown, "ptyro-dark");
  assert.ok(view.options.find((o) => o.source === "themes").options.some(([n]) => n === "ptyro-light"));
  assert.ok(view.options.find((o) => o.source === "actions").options.length > 100);
  assert.equal(view.layer.kind, "user");

  // A wrong value says why, and a dry run checks without writing.
  await assert.rejects(call("settings.apply", { scope: "user", change: { t: "set", key: "ui.sidebar_width", value: "3" } }), /outside 12 to 120/);
  await assert.rejects(call("settings.apply", { scope: "user", dry: true, change: { t: "set", key: "ui.theme", value: "nonesuch" } }), /unknown theme/);
  assert.deepEqual(await call("settings.apply", { scope: "user", dry: true, change: { t: "set", key: "ui.theme", value: "ptyro-light" } }), { ok: true });
  assert.equal((await call("settings.view", { scope: "user" })).entries.find((e) => e.key === "ui.theme").origin, "default", "the dry run wrote nothing");

  // A change: the new view comes back, and so does the file as it was, to undo with.
  let r = await call("settings.apply", { scope: "user", change: { t: "set", key: "ui.theme", value: "ptyro-light" } });
  assert.equal(r.changed, true);
  assert.equal(r.apply, "live");
  assert.equal(r.view.entries.find((e) => e.key === "ui.theme").origin, "user");
  r = await call("settings.apply", { scope: "user", change: { t: "set", key: "notifications.callback.listen", value: "127.0.0.1:7591" } });
  const before = r.before;
  assert.equal(r.apply, "restart");
  assert.ok(r.notes[0].includes("daemon restart"));
  // Lists, sets, members and bindings.
  await call("settings.apply", { scope: "user", change: { t: "set", key: "discover.roots", value: '["~/a","~/b"]' } });
  await call("settings.apply", { scope: "user", change: { t: "set", key: "notifications.on", value: '["blocked","done"]' } });
  r = await call("settings.apply", { scope: "user", change: { t: "add", path: "agents", name: "aider", fields: { command: "aider --model sonnet" } } });
  assert.ok(r.view.members.find((m) => m.path === "agents").names.includes("aider"));
  assert.equal(r.view.entries.find((e) => e.key === "agents.aider.command").shown, "aider --model sonnet");
  r = await call("settings.apply", { scope: "user", change: { t: "add", path: "keys", name: "x", fields: { value: "session.next" } } });
  assert.deepEqual(r.view.maps.find((m) => m.path === "keys").items, [["x", "session.next"]]);
  await assert.rejects(call("settings.apply", { scope: "user", change: { t: "add", path: "notifications.sinks", fields: { type: "slack" } } }), /needs its webhook/);
  r = await call("settings.apply", { scope: "user", change: { t: "add", path: "notifications.sinks", fields: { type: "ntfy", url: "https://ntfy.sh/me" } } });
  assert.equal(r.view.entries.find((e) => e.key === "notifications.sinks.0.url").value, "https://ntfy.sh/me");
  r = await call("settings.apply", { scope: "user", change: { t: "rename", path: "agents", from: "aider", to: "pair" } });
  const names = r.view.members.find((m) => m.path === "agents").names;
  assert.ok(names.includes("pair") && !names.includes("aider"));
  r = await call("settings.apply", { scope: "user", change: { t: "remove", path: "agents", name: "pair" } });
  assert.ok(!r.view.members.find((m) => m.path === "agents").names.includes("pair"));
  await call("settings.apply", { scope: "user", change: { t: "remove", path: "notifications.sinks", name: "0" } });
  await call("settings.apply", { scope: "user", change: { t: "unset", key: "keys.x" } });
  // Undo puts the file back as it was before the second change.
  r = await call("settings.restore", { scope: "user", text: before });
  assert.equal(r.view.entries.find((e) => e.key === "ui.theme").origin, "user");
  assert.equal(r.view.entries.find((e) => e.key === "notifications.callback.listen").origin, "default");
  await assert.rejects(call("settings.restore", { scope: "user", text: "[ui\n" }), /./);
  // Reset.
  r = await call("settings.apply", { scope: "user", change: { t: "unset", key: "ui.theme" } });
  assert.equal(r.view.entries.find((e) => e.key === "ui.theme").origin, "default");
  await assert.rejects(call("settings.apply", { scope: "user", change: { t: "bogus" } }), /no such change/);
  // A repository's settings need a repository.
  await assert.rejects(call("settings.view", { scope: "project" }), /project is needed/);
  console.log("ok");
} else {
  console.error("usage: config.mjs units|host");
  process.exit(64);
}
