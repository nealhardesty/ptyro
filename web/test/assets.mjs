// The web app's file lists must agree: what `ptyro serve` embeds (`js!("…")` and the others in
// crates/ptyro/src/serve.rs), what the service worker keeps for offline starts (FILES in
// web/src/sw.ts), and what is on disk in web/dist. A module in one and not another works online
// and breaks offline (or the other way round), with no error to say why.
//   node web/test/assets.mjs
import assert from "node:assert/strict";
import { readFileSync, readdirSync } from "node:fs";

const read = (p) => readFileSync(new URL(`../../${p}`, import.meta.url), "utf8");
const serve = read("crates/ptyro/src/serve.rs");
const sw = read("web/src/sw.ts");

const served = new Set([...serve.matchAll(/js!\("([a-z]+)"\)/g)].map((m) => `/${m[1]}.js`));
for (const m of serve.matchAll(/\(\s*"(\/[^"]*)",\s*(?:"[^"]*",\s*)?include_bytes!/g)) if (m[1] !== "/index.html" && m[1] !== "/sw.js") served.add(m[1]);

const files = sw.match(/const FILES = \[([\s\S]*?)\n\];/)[1];
const cached = new Set([...files.matchAll(/"(\/[^"]*)"/g)].map((m) => m[1]));
for (const m of files.matchAll(/\[((?:"[a-z]+",?\s*)+)\]\.map/g)) for (const n of m[1].matchAll(/"([a-z]+)"/g)) cached.add(`/${n[1]}.js`);

assert.deepEqual([...cached].sort(), [...served].sort(), "sw.ts FILES and serve.rs's assets list the same files (sw.js itself aside)");
// Every module on disk is served.
const onDisk = readdirSync(new URL("../dist/", import.meta.url)).filter((f) => f.endsWith(".js") && f !== "sw.js").map((f) => `/${f}`);
assert.deepEqual(onDisk.sort(), [...served].filter((f) => f.endsWith(".js")).sort(), "web/dist's modules are the ones served");
// Every import in a module names a module that is served.
for (const f of onDisk) {
  const src = read(`web/dist${f}`);
  for (const m of src.matchAll(/from "\.\/([a-z]+)\.js"|import "\.\/([a-z]+)\.js"/g)) assert.ok(served.has(`/${m[1] ?? m[2]}.js`), `${f} imports ${m[1] ?? m[2]}.js, which isn't served`);
}
console.log("ok");
