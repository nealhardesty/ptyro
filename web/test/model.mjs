// A model's id shortened for the cards and the session header. No browser.
//   node web/test/model.mjs
// The same table as `model_label_tests` in crates/ptyro-core/src/session.rs: change both together.
import assert from "node:assert/strict";
import { modelLabel } from "../dist/screen.js";

for (const [id, label] of [
  ["claude-opus-5-5", "opus 5.5"],
  ["claude-sonnet-4-5-20250929", "sonnet 4.5"],
  ["claude-3-5-sonnet-20241022", "sonnet 3.5"],
  ["claude-opus-4-1", "opus 4.1"],
  ["claude-haiku-5-5", "haiku 5.5"],
  ["claude-opus-5-5[1m]", "opus 5.5 1m"],
  ["anthropic/claude-sonnet-5-5", "sonnet 5.5"],
  ["opus", "opus"],
  ["gpt-5.6-sol", "gpt-5.6-sol"],
  ["openai/gpt-5.5", "gpt-5.5"],
  ["gemini-2.5-pro", "gemini-2.5-pro"],
  ["claude-fable-5-1", "claude-fable-5-1"],
  ["  ", ""],
]) assert.equal(modelLabel(id), label, id);
console.log("model: ok");
