// Home's logic (web/dist/homekit.js): the headline, the line of what happened while you were
// away, a week's marks and a duration, said as `ptyro_core::home` says them (the expected
// strings here are the ones its tests assert: crates/ptyro-core/src/home/tests.rs).
//   node web/test/home.mjs
import assert from "node:assert/strict";
import { CAPS, HANGING, SECTIONS, TITLES, VERBS, awayLine, headline, span, spark } from "../dist/homekit.js";

const MIN = 60_000;
const thing = (next) => ({ next, text: "", at_ms: 0, key: next, stamp: "" });
const row = (section, nexts, working = 0) => ({ key: section, name: section, section, things: nexts.map(thing), live: working, working, last_ms: 0 });
const view = (rows) => ({ at_ms: 0, rows, meters: [], memory: "on" });

// Things are counted, not rows; `working` counts agents at work.
assert.equal(headline(view([row("needs", ["answer", "review"]), row("needs", ["failed"]), row("hanging", ["asked"]), row("working", ["working"], 1), row("quiet", ["nothing"])])), "3 need you · 1 left hanging · 1 working");
assert.equal(headline(view([row("needs", ["look", "commit"])])), "1 needs you · 1 left hanging");
assert.equal(headline(view([row("quiet", ["left_off"]), row("working", ["open"])])), "all quiet");
assert.equal(headline(view([])), "all quiet");

// Every `next` the daemon sends has its word, and what is left hanging is what can be put away.
assert.deepEqual(Object.keys(VERBS), ["answer", "failed", "review", "look", "asked", "cut_off", "planned", "commit", "push", "stale", "working", "limit", "open", "left_off", "nothing"]);
assert.deepEqual([VERBS.asked, VERBS.cut_off, VERBS.planned, VERBS.stale, VERBS.open, VERBS.nothing], ["it asked", "cut off", "next", "idle", "your turn", ""]);
assert.deepEqual(HANGING, ["asked", "cut_off", "planned", "commit", "push", "stale"]);
assert.deepEqual(SECTIONS.map((s) => TITLES[s].toUpperCase()), ["NEEDS YOU", "LEFT HANGING", "WORKING", "QUIET"]);
assert.deepEqual(CAPS, { hanging: 8, quiet: 6 });

assert.deepEqual([45 * MIN, 120 * MIN, 130 * MIN, 3 * 1440 * MIN, 0].map(span), ["45m", "2h", "2h10m", "3d", "0m"]);
assert.equal(awayLine({ since_ms: 0, back_ms: 130 * MIN, finished: 3, failed: 1, asked: 0, commits: 2 }), "away 2h10m: 3 finished, 1 failed, 2 commits");
assert.equal(awayLine({ since_ms: 0, back_ms: 60 * MIN, finished: 0, failed: 0, asked: 0, commits: 0 }), "away 1h: nothing happened");
assert.equal(awayLine({ since_ms: 0, back_ms: 25 * MIN, finished: 0, failed: 0, asked: 1, commits: 1 }), "away 25m: 1 asks you, 1 commit");

assert.equal(spark([0, 0, 3, 9, 0, 0, 1]), "··▃█··▁");
assert.equal(spark([0, 0, 0, 0, 0, 0, 0]), "");
assert.equal(spark(undefined), "");
console.log("ok");
