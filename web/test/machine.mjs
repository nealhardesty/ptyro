// The machine name on the page: the tab title, the stable colour, the favicon dot. No browser.
//   node web/test/machine.mjs
import assert from "node:assert/strict";
import { accent, faviconUrl, hue, titleFor } from "../dist/machine.js";

assert.equal(titleFor("studio", 0), "studio · ptyro");
assert.equal(titleFor("studio", 3), "(3) studio · ptyro", "the waiting count stays, in front");
assert.equal(titleFor("", 0), "ptyro", "no name yet (before the first welcome, or an older daemon)");
assert.equal(titleFor("", 2), "(2) ptyro");
// The hue is stable, ignores case, differs between machines, and matches the Rust side
// (`ptyro_core::machine::hue`: FNV-1a over lowercase bytes, mod 360).
assert.equal(hue("Studio"), hue("studio"));
assert.ok(hue("studio") >= 0 && hue("studio") < 360);
assert.notEqual(hue("studio"), hue("laptop"));
assert.equal(hue(""), 0x811c9dc5 % 360);
assert.equal(hue("a"), Number((BigInt.asUintN(32, (0x811c9dc5n ^ 97n) * 0x01000193n)) % 360n));
// Light pages get a darker colour than dark ones; the name is always shown beside it.
assert.notEqual(accent("studio", true), accent("studio", false));
assert.match(accent("studio", true), /^hsl\(\d+ 55% 62%\)$/);
const icon = decodeURIComponent(faviconUrl("studio", true));
assert.ok(icon.startsWith("data:image/svg+xml,<svg") && icon.includes(accent("studio", true)));
console.log("machine: ok");
