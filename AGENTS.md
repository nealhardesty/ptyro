# AGENTS - Base rules for coding agents.


**Core Directive**: Go Above and Beyond.  You have creative freedom here; use it.  

Everything a user needs to setup and use this project will be found in `README.md`.
Keep instructions maintained in there.

Everything *about* the project — architecture, protocol, formats,
environment, accounts, commands, and every hard-won fact — is in **`SPEC.md`**. Read it before
changing anything. 

**`ROADMAP.md`** is the source of truth for what does not exist yet; read it
before proposing new work.

**`RESEARCH.md`** is the running record of competitor products and the ideas
they suggest. It feeds candidate work into `ROADMAP.md`.
- Check its idea funnel (Part III) before proposing a feature. The idea may
  already have an `R-` id, a status, or a recorded reason it was rejected.
- When you learn about a relevant product or technique, add or update its entry.
  Date every claim and mark it [V]/[S]/[U].
- When an idea is promoted, put the ROADMAP task id on its row and mention the
  `R-` id in the ROADMAP task.
- Take ideas, never code. Verified technical facts move to SPEC §20.

**The TUI is first class** (SPEC §1.3). Every feature must work completely in the
TUI, and be discoverable there, before it counts as done. The CLI must be complete
too, but it follows the TUI.
- A new CLI command needs its TUI path (a key, the palette, the row menu) or a
  waiver with a reason in `docs/tui-parity.md`. Once ROADMAP UX1.0 lands,
  `make tui-parity` checks this in CI.
- Out of the box comes first: sensible defaults, no setup the user must know
  about, and errors that offer the fix instead of naming a command to type.

**Automation: everything has a `make` target.** The owner drives the project
through the root `Makefile`.
- Anything we implement that someone would run gets a target: build, test, lint,
  run, spikes, fixtures, releases, services.
- Each target carries a `## description` comment so that `make` (which runs
  `make help`) lists it.
- CI calls make targets, never raw `cargo` commands, so local runs and CI behave
  the same.
- Use make instead of `cargo xtask` or loose scripts. When a task needs real
  logic, write it as a script or binary and wrap it in a target.
- Mention new targets in the Development section of `README.md`.


---

## Coding Behavioral Guidelines 

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 1. Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Before implementing:
- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 2. Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 3. Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:
- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:
- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 4. Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:
- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:
```
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria lets you loop independently. Weak criteria ("make it work") require constant clarification.

---

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.

