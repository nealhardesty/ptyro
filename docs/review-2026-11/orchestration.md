# ptyro review — agent orchestration (2026-11-11)

Reviewer lens: orchestration as implemented today, and how to make it much better.
Checkout read: `the review checkout` (branch `docs/review-2026-11`, e3a6cf3). Nothing was built, run or modified.
All `path:line` references are relative to that checkout.

**How to read the evidence labels**

- **[read]** — I (or a sub-reviewer I briefed) read the code at the cited lines and the claim follows from it.
- **[suspected]** — follows from reading, but depends on timing or on behaviour I could not confirm without running it.
- **[spec]** — taken from SPEC/ROADMAP/RESEARCH text, not checked against code.
- **[not verified]** — stated for completeness; I did not get to the code.

---

## 0. The honest one-page verdict

**Orchestration in ptyro is wide, not deep.** The caller's framing ("in its infancy") is right about maturity and wrong about surface area. In code today there is:

- an event-sourced run engine with a pure planner (`crates/ptyro-run/src/engine.rs:68`), seven step kinds plus `loop`, four join modes, retries, three timeouts, three budgets, typed outputs, gates, forks and restart recovery;
- fan-out with an evidence-ranked compare (tests on base and candidate, repro, cross-tests, clusters, advisory judge), pick by merge, cherry-pick, PR or hunk;
- a mailbox with group addresses and daemon-observed delivery states, file leases, a collision radar;
- lead agents driven through a stdio MCP server with a daemon-enforced policy;
- multi-repo change sets with a contract, write fences, overlays, set checkpoints and a merge train;
- verified-done (a Stop-hook bounce), per-turn checkpoints, a review queue.

That is more primitives than most competitors in RESEARCH.md Part I ship. The infancy shows elsewhere:

1. **It has never run against real agents.** ROADMAP M7 closed on 2026-10-20 and M11 on 2026-10-21; every test uses fake adapters (`ROADMAP.md:751`, `:767-769` "Needs you: real agents"). The only real-agent evidence in the repository is the hand experiment M11.0 (`SPEC.md:5427-5454`), and it found two blocking defects in eight minutes (the trust dialog; 13 permission prompts).
2. **ROADMAP has no open orchestration task at all.** Every M7 and M11 box is ticked; the 20+ `new` rows in RESEARCH Part III R-ORC are not promoted. On paper the area is finished, which is the opposite of true.
3. **The happy path is built and the unattended path is not.** Nothing supervises a run: there is no stuck or loop detection, no handling of a step whose agent sits on a permission prompt, no cost signal, no concurrency limit, and several timing bugs below make the flagship recipes fail in exactly the overnight case they are for (O-01, O-02, O-03).
4. **Authority is uneven.** A lead agent is tightly fenced; other agents are held to far fewer rules (O-07; detail withheld from this public copy).
5. **The user cannot see it.** A run is a list of step statuses. There is no timeline, no "why is this waiting", no cost, and no cross-machine view (runs never leave their machine, O-21).

The differentiator ptyro already owns, and no single-vendor tool can copy, is: **cross-vendor, reboot-surviving, evidence-first, with a human gate reachable from a phone.** The path in §4 is about making that true in practice before adding any new primitive.

---

## 1. What exists, precisely

### 1.1 The model: who is the orchestrator?

Three orchestrators exist side by side. SPEC §15 states the principle: every level is a deterministic engine; "AI planning" is something an agent does with ptyro's tools (`SPEC.md:4810-4812`).

| Orchestrator | What drives it | Run kind | Where |
|---|---|---|---|
| **The user with shell primitives** | `ptyro new/send/wait/read/events` in a script | none | SPEC §15.1 |
| **The daemon's deterministic engine** | a TOML recipe (or a fan-out, which is a generated recipe) | `recipe`, `fanout` | `crates/ptyro-run/src/engine.rs`, `crates/ptyro-daemon/src/runs.rs` |
| **A lead agent** | an LLM calling `ptyro mcp` tools under a daemon policy | `lead` | `crates/ptyro/src/mcp.rs`, `crates/ptyro-daemon/src/mail.rs:1221` |
| **A change set** (a hybrid) | a lead agent for the contract, a deterministic actor for everything else, the human for shipping | `changeset` | `crates/ptyro-daemon/src/changesets.rs`, `crates/ptyro-run/src/changeset.rs` |

There is no supervisor agent inside the engine and no LLM routing; RESEARCH III.3 rejects both on purpose (`RESEARCH.md:3135`).

### 1.2 Spawning, naming, addressing

- **A step's agent** is an ordinary ptyro session started by `Daemon::spawn` with `SpawnSpec { agent, cwd, worktree, profile, prompt, name, env, unqueued: true, structured }` (`runs.rs:1109-1123`). It gets `PTYRO_RUN_ID`, `PTYRO_STEP_ID`, `PTYRO_RUN_DIR`, `PTYRO_STEP_OUT` (`runs.rs:1067-1076`).
- **Names**: recipe worktrees are `<run-id>-<step name>`; fan-out worktrees are `<prefix>-<agent>` where the prefix is the prompt's first three words (`runs.rs:971-980`, `:2430-2437`). Lead workers are steps `w1, w2…` (`mail.rs:1359-1360`).
- **Addresses**: a session name or id; `@user`, `@lead`, `@all`, `@idle`, `@working`, `@blocked`, `@<agent>`, `@run:<id>` — expanded into one row per recipient at send time (`mail.rs:364-435`). Runs are `r-xxxxxx` or `@last` (`runs.rs:218-243`); gates are `g-xxxx` (`runs.rs:1709-1714`).
- **Who is calling** is decided by the kernel, not by what the client says: the daemon walks the peer pid's ancestry to a session (`mail.rs:119-141`).

### 1.3 Delegation

- **Engine → agent**: the prompt is typed into the agent's TUI once it is ready; the step ends when the turn that prompt started ends (`runs.rs:1375-1472`). A follow-up is a `send` step, typed at idle (`runs.rs:1474-1534`).
- **Lead → worker**: `spawn_session` (policy-checked), then `send`, which is mail of kind `dispatch` (`mail.rs:1416-1434`). The worker's prompt is suffixed with "report back with `ptyro msg send @lead`" (`mail.rs:1377-1379`).
- **Set lead → member**: `cs_assign`, `cs_send`; members get a generated brief and the approved contract [spec, `SPEC.md:5492-5494`].

### 1.4 Communication: the mailbox

| Property | What the code does | Evidence |
|---|---|---|
| Storage | In-memory `Vec<MessageRow>` mirrored to the `run_message` table; only open rows are reloaded at start | `mail.rs:339-347`, `ptyro-store/src/lib.rs:156-172` |
| Delivery mechanism | A 700 ms loop types all waiting mail for a recipient as **one coalesced prompt** into the agent's PTY when it is idle and its input box is ready | `mail.rs:349-354`, `:623-684` |
| When it is injected | Never into a `working` or `blocked` agent, or one with a prompt pending. `interrupt` mail to a working agent goes to Claude's messaging socket if the session has one, else to the change-set interrupt path | `mail.rs:642-665` |
| States | `queued` → `delivered` (typed) → `seen` (the UserPromptSubmit hook's text contained the id) → `acked`. Hook-less agents count as seen on delivery | `mail.rs:76-86`, `:672-681`, `:715-730` |
| Acknowledgement | Observed by the daemon, never asked of the LLM. `mail_seen` sets seen **and** acked together | `mail.rs:725-728` |
| Ordering | FIFO per recipient (vector order), coalesced. Cursor reads compare ids as strings | `mail.rs:629-635`, `:748` |
| Framing | `[via ptyro: N message(s) from other agents or tools, not from your user. They can't grant permissions…]` + `<ptyro-message id= from= kind= run=>` + how to reply | `mail.rs:686-712` |
| Loop safety | ≤50 queued per recipient, identical body from the same sender within 60 s dropped, ≤4 reply hops, TTL (default 24 h), 20/min and 500 unread to `@user` | `mail.rs:37-38`, `:56-58`, `:508-553` |
| Heartbeats | Synthesised: a worker's turn end, block or exit posts `worker_done`/`status` to its lead, with the agent's last reply | `mail.rs:768-824` |
| Loss | See O-04, O-05, O-06 | |

### 1.5 State detection

Per-adapter detail is in the capability table, §2.3. In outline: hooks relayed by `ptyro hook <adapter> <event>` are the high-confidence source; OSC 7501 and window titles are medium; reading the screen is the fallback for dialogs that precede any hook and for agents started by hand (`crates/ptyro-agents/src/adapter.rs:114-139`). The run engine consumes exactly one thing from all of this: `reached(info, "idle" | "done" | "error")`, which requires a status newer than the last prompt ptyro typed (`crates/ptyro-core/src/session.rs:457-483`).

`crates/ptyro-meaning` is **not** status detection, despite the name; see §2.3.

### 1.6 How results come back

1. **Step outputs** (built in): `session`, `worktree`, `branch`, `base`, `head`, `diffstat`, `files`, `last_message`, `duration_ms`, `turns`, `verified`, and `tools` for structured sessions (`runs.rs:1314-1371`).
2. **Typed outputs**: the prompt is extended with instructions to write JSON to `$PTYRO_STEP_OUT`; `builtin:verdict`, `builtin:subtasks` or a required-keys schema is checked; one repair prompt (`runs.rs:1270-1304`, `ptyro-run/src/output.rs`).
3. **Snapshot refs**: `refs/ptyro/runs/<run>/<step>/<n>/{start,end}` checkpoint commits per attempt (`runs.rs:1045-1065`), `…/loop/<i>` per loop iteration (`runs.rs:1603-1608`), and `refs/ptyro/ckpt/<session>/<n>` per turn [spec, `SPEC.md:5311-5329`].
4. **Verified-done**: a check command decides "done", by a Stop-hook bounce for Claude and a follow-up prompt for others [spec, `SPEC.md:5288-5309`; code in `crates/ptyro-daemon/src/verify.rs`].
5. **Shipping** is an `apply` step — `commit`, `push`, `pr`, `integrate`, `automerge` — with an intent event before and an outcome after each action, so a restart re-runs only what has no outcome (`runs.rs:1883-2026`). Agents do not push; the built-ins add `guard = { deny_cmds = ["git push"] }`.
6. **Review**: the review queue ranks worktrees whose agent finished an unreviewed turn (`crates/ptyro-daemon/src/review.rs`).

### 1.7 Isolation and selection of parallel attempts

- One worktree per writer by default (`worktree = "new"`); a reviewer may borrow the writer's worktree by naming its step (`runs.rs:1023-1030`).
- A retry and each loop iteration reuse the step's worktree (`runs.rs:953-961`).
- **Selection is always the human's.** The `verify` step gathers evidence and ranks lexicographically — succeeded; tests; did not break tests; repro; cross-test pass rate; verified; changed something; judge; smaller diff; faster [spec, `SPEC.md:5270-5273`] — and publishes `outputs.best`, but nothing picks: `best-of-n` ends in a gate that tells the user to open the compare view (`crates/ptyro-run/src/builtin.rs:88-91`).
- Conflict avoidance between writers: daemon-held leases enforced in PreToolUse (`mail.rs:840-922`), and the radar, which after each turn computes file overlap and `git merge-tree` conflicts between live worktrees and mails both agents once per new overlap (`mail.rs:973-1129`).

### 1.8 Runs and recipes: the primitive inventory

A **run** is an append-only event log plus a JSON projection written in one transaction (`runs.rs:162-206`, `ptyro-store/src/runs.rs:109`). A **recipe** is TOML, pinned by source and sha per run.

| Primitive | Present? | Notes and evidence |
|---|---|---|
| Steps: `spawn`+`prompt`, `send`, `run`, `gate`, `apply`, `sleep`, `verify` | yes | `runs.rs:913-923` |
| Dependencies and joins `all / any / first_success / all_settled`; `cancel_siblings` | yes | `engine.rs:39-66`, `:101-117` |
| Fan-out | static only | one step per agent, generated as recipe text (`runs.rs:2455-2479`). No `foreach`, `matrix` or dynamic expansion |
| Conditions | yes | `if`, a small expression language (`ptyro-run/src/expr.rs`) |
| Loops | one shape | `loop = { until, max, no_progress }` on a single spawn step; fresh session per iteration (`runs.rs:1538-1657`). No loop over several steps (a review loop cannot be expressed) |
| Retries | yes | `max`, `on`, `not_on`, `mode = fresh / same_session`, `feedback`. **No backoff** (`engine.rs:152-160`, `runs.rs:846-848`) |
| Timeouts | yes | `total`, `start`, `stall` (`runs.rs:869-888`, `:1443-1469`). No default: a step without one waits for ever |
| Gates (human approval) | yes | options, deadline, `on_timeout = reject / approve / escalate / <option>`; idempotent by key (`runs.rs:1699-1881`) |
| Budgets | wall, sessions, attempts | `runs.rs:515-528`, `:1089-1093`, `:535-549`. No cost, token or turn budget |
| Guards | partly | `deny_paths`, `deny_cmds` through PreToolUse; `max_diff_lines` and `on_trip` are parsed and never read (`ptyro-run/src/recipe.rs:422-425`; no other reference in the tree) |
| Typed outputs | yes | `builtin:verdict`, `builtin:subtasks`, required keys |
| Concurrency limits | **no** | every ready step starts at once (`engine.rs:74-100`) |
| Sub-recipes, triggers, cron, events, cache, compensation | **no** | listed as v2 in `RESEARCH.md:2678-2689` |
| Model per step | **no** | `SpawnDef` has `agent`, `worktree`, `name`, `profile`, `base`, `structured` (`recipe.rs:182-198`); model only by defining another agent name |
| Headless agent steps | **no** | "Not done: `spawn.mode = "headless"`" (`ROADMAP.md:687`) |

Built-in recipes: `implement-and-review`, `best-of-n`, `test-fixer`, `doc-writer`, `overnight` (`ptyro-run/src/builtin.rs`).

### 1.9 Persistence and restart

On daemon start (`runs.rs:318-406`), up to 500 unfinished runs are reloaded from the projection (or replayed from the log), sessions are re-tied to their steps, and `recover` (`runs.rs:409-494`) handles each in-flight step:

| Step kind | After a restart |
|---|---|
| `run` | attempt marked `crashed`, then the retry rule applies |
| `spawn` / `send` | `on_restart`: `native_resume` (default when the adapter resumes) resumes the same session and types a "carry on" nudge; `fresh` starts a new attempt; `hold` waits for `ptyro run retry` |
| `gate` | same id, same absolute deadline (`runs.rs:1702-1707`) |
| `sleep` | same deadline (`runs.rs:2028-2052`) |
| `apply` | only actions with no recorded outcome (`runs.rs:1894-1905`) |
| `verify` | re-run [not verified] |
| lead run | no actor; the lead's MCP injection survives resume [spec, `SPEC.md:5387`] |
| change set | its own actor restarts (`runs.rs:377-379`) |

Mail and leases reload (`mail.rs:339-347`). What is **not** persisted: step tasks' timers (a `total` timeout restarts from zero, `runs.rs:869-888`), `radar_seen` (so overlaps are re-announced after a restart, `mail.rs:1083`), and mail delivery attempts.

### 1.10 Data flow

```
                    YOU (CLI · TUI · web/phone)
       recipe run / fanout / cs new        gate resolve · pick · answer
                 │                                   ▲
                 ▼                                   │  inbox entry + notification
┌──────────────────────────── ptyrod ────────────────┼──────────────────────────────┐
│                                                    │                              │
│  Request ─► guard_caller (peer pid → session → policy)        mail.rs:144         │
│                 │                                                                 │
│                 ▼                                                                 │
│   create_run ─► RunEvent::Created ─► emit ─► [run + run_event] one txn (SQLite)   │
│                                        │                         ▲                │
│                                        ▼ wake                    │ every change   │
│   ┌──────────── run actor (one per run, 2 s tick) ───────────┐   │ is an event    │
│   │ plan(recipe, state) → Start / Skip / Fail / Cancel / Finish  │                │
│   └───────┬───────────────────────────────────────────────────┘ │                │
│           ▼ spawn_step (one task per step)                       │                │
│   ┌─ spawn ─► create_worktree ─► setup script ─► Daemon::spawn ──┤                │
│   │            (git worktree)                      │             │                │
│   │                                                ▼             │                │
│   │                                     PTY ◄─ type_prompt ─ agent TUI            │
│   │                                      │   (Claude · Codex · Gemini · …)        │
│   │   status engine ◄─ hooks (ptyro hook …) / OSC 7501 / title / screen           │
│   │        │                                                                      │
│   │        ├─► wait_turn: reached(idle|done|error) + verify state ─► outputs ─────┤
│   │        ├─► Stop hook ─► done_when check ─► bounce or pass                     │
│   │        ├─► PreToolUse ─► guards · leases · change-set fences ─► allow / deny  │
│   │        ├─► turn end ─► checkpoint ref · radar ─► "collision" mail             │
│   │        └─► worker turn end ─► "worker_done" mail to the lead                  │
│   ├─ run    ─► $SHELL -c in the worktree ─► exit code, log, tail                  │
│   ├─ gate   ─► GateOpened ─► inbox + notifier ─► waits for GateResolved ──────────┘
│   ├─ verify ─► tests on base + candidates · repro · cross-tests · judge ─► ranking
│   └─ apply  ─► Intent ─► commit / push / PR / integrate ─► Outcome
│                                                                                   │
│   mailbox (700 ms loop): queued ─► typed at idle, coalesced ─► seen (prompt hook) │
│        ▲ post                                                                     │
│   lead agent ─► ptyro mcp (stdio JSON-RPC) ─► Request::Lead ─► lead_op + Policy   │
│   change-set actor ─► contract revisions · readiness · train ─► CsEvents          │
└───────────────────────────────────────────────────────────────────────────────────┘
       compare view ◄─ evidence        review queue ◄─ finished, unreviewed turns
```

### 1.11 What the user sees

See the surface table in §2.4 (from a sub-review of the CLI, TUI and web code).

---
## 2. Gaps and weaknesses

Findings from the run engine, mailbox and lead code are my own reading. Findings marked **(sub-review)** come from three read-only sub-reviews I briefed (adapters; change sets, worktrees and evidence; the three front ends); I spot-checked some of their citations but not all, so treat their line numbers as "read by a reviewer, not re-read by me".

Abbreviations: `runs.rs`, `mail.rs`, `changesets.rs`, `agents.rs`, `verify.rs`, `evidence.rs`, `machines.rs` are in `crates/ptyro-daemon/src/`; `engine.rs`, `recipe.rs`, `builtin.rs`, `state.rs` in `crates/ptyro-run/src/`.

### 2.1 Correctness

#### O-01 — The wall budget counts time spent waiting for you, so the flagship recipes fail overnight [read] — high
- The actor checks `now − created_ms > budget.wall` on every pass, whatever the run is doing (`runs.rs:515-528`).
- `overnight` has `wall = "8h"` and ends in a gate with no timeout (`builtin.rs:174-176`, `:190`). A loop that finishes at 01:00 for a user who looks at 09:30 has been failed with "budget: the run's 8h wall clock ran out", and its gate step cancelled (`runs.rs:618-630`).
- `implement-and-review` has `wall = "3h"` and a gate with `timeout = "8h", on_timeout = "escalate"` (`builtin.rs:9-11`, `:41`). The gate can never wait eight hours.
- Fix direction: stop the clock while the run is `waiting`, or count working time (OI-05).

#### O-02 — Three paths wait for a turn that has not started [suspected] — high
- `wait_turn` returns as soon as the session is idle with a status newer than the last prompt ptyro typed (`crates/ptyro-core/src/session.rs:457-460`, `runs.rs:1402-1405`).
- `exec_send` knows the hazard and waits until the follow-up has really been typed: "Wait for delivery, so the idle before it doesn't count" (`runs.rs:1516-1532`).
- Three other call sites type through `deliver_when_idle`, which only spawns a task (`crates/ptyro-daemon/src/github.rs:599-604`), and then call `wait_turn` at once:
  1. **Resume after a restart** (`runs.rs:1203-1204`): the resumed agent is idle before the "carry on" nudge lands, so the step can succeed with whatever was there.
  2. **`retry.mode = "same_session"`** (`runs.rs:1230-1233`): the session is idle from the failed attempt, so the retry can succeed instantly without the agent seeing the feedback.
  3. **The typed-output repair prompt** (`runs.rs:1289-1297`): `wait_turn` returns at once, the invalid file is still there, and the step fails; the one repair SPEC §15.6.2 promises has no time to happen.
- A step with `done_when` is protected in cases 1 and 2, because `reset_verify`/`set_verify` leave the verify state pending. Steps without it are not.
- `crates/ptyro/tests/m7.rs` has no test that mentions the repair prompt (grep for "repair" and "isn't valid": no hits).
- Not run; the timing could hide it when the agent takes the prompt within one 250 ms poll.

#### O-03 — A step whose agent waits on a permission prompt is unbounded and looks like work [read] — high
- `wait_turn` has no branch for `blocked`. The `stall` timeout applies only while the status is `Working` (`runs.rs:1453-1469`), and `start` only before the agent first worked.
- A step without `timeout.total` therefore waits for ever; one with it is killed after the full total with "still not done", which hides the cause.
- The step's status stays `running`; `Waiting` means only a gate or a held step (`state.rs:44`, `:74`). No front end shows the difference (sub-review: `crates/ptyro-tui/src/runs.rs:677`, `web/src/runs.ts:200`).
- The M11.0 experiment met 13 permission prompts in 8 minutes (`SPEC.md:5453`). This is the normal case, not an edge.

#### O-04 — "Delivered" means "typed", and nothing is ever delivered twice [read] — high
- Mail is marked delivered straight after `type_prompt` returns (`mail.rs:670-681`).
- `type_prompt` confirms that a prompt was taken only for single-line text (`agents.rs:803`); mail is always several lines (`mail.rs:686-712`).
- There is no redelivery anywhere in the tree (grep "redeliver": none). RESEARCH II.5 specified "Redeliver when `delivered` isn't followed by `seen`" (`RESEARCH.md:2763`).
- So mail typed into an agent whose input box did not take it (a dialog, a dropped Enter, which the code knows Codex 0.166 does) is `delivered` for ever and never `seen`.
- Mid-turn delivery to Claude writes to an undocumented socket and marks the mail delivered on a successful write (`mail.rs:651-665`, `:1885-1898`). ROADMAP calls socket delivery both "not adopted" (`RESEARCH.md:2930`) and built-but-unverified (`ROADMAP.md:694`).

#### O-05 — Mail is dropped silently in four ways, and the sender is told it worked [read] — medium
- Recipient's queue is full: `continue` (`mail.rs:524-528`).
- Same body from the same sender within 60 s: `continue` (`mail.rs:545-553`). A second honest "done" or "ok" disappears.
- `@user` over 20 a minute or 500 unread: `continue` (`mail.rs:531-544`).
- TTL expiry: removed from memory with no notice to anyone (`mail.rs:628`).
- In each case `post` returns `Ok` with fewer or no ids (`mail.rs:606-609`).
- Mail to an exited or dormant recipient waits until the TTL (`mail.rs:639-641`); only change sets wake the recipient (`SPEC.md:5534`).

#### O-06 — The reply-depth limit does not work for agents with hooks [read] — medium
- The hop count is the parent's hop plus one, and the parent is looked up in the in-memory open mail (`mail.rs:496-499`).
- When a hook agent's prompt hook sees a message it is marked seen **and acked** (`mail.rs:725-728`), and acked rows are dropped from memory on the next pass (`mail.rs:628`).
- So a reply to a message that was already read finds no parent and starts at hop 0. Two Claude agents replying to each other never reach `MAX_HOPS`.
- An agent that does not pass `--reply-to` is never counted either. The remaining brake is the identical-body rule. RESEARCH asked for per-pair rate limits (`RESEARCH.md:2767`); there are none.

#### O-07 — Only leads are fenced [read] — high
*Withheld from this public copy: the detail is security-relevant and is in the private security paper (see the note at the top of `docs/review-2026-11.md`).*

#### O-08 — A run that fails or times out leaves its agents running [read] — medium
- `finish_run` aborts the step tasks and emits `StepEnded(Cancelled)` but does not stop sessions (`runs.rs:608-631`). Only an explicit `run cancel` goes through `cancel_step`, which does (`runs.rs:721-735`, `:2194-2196`).
- A wall-budget failure (O-01) therefore leaves N agents working with nothing watching them, their done-when checks and guards still set.

#### O-09 — No back-pressure [read] — medium
- `plan` starts every ready step at once (`engine.rs:74-100`). A 10-way fan-out starts ten agents and ten setup scripts together.
- The one throttle ptyro has, `review.max_unreviewed`, is bypassed by every orchestrated spawn: `unqueued: true` (`runs.rs:1119`, `mail.rs:1384`; the hold is at `crates/ptyro-daemon/src/daemon.rs:1300-1302`).
- Nothing consults the usage meters before starting agents, though the daemon knows a profile's rate-limit window (`crates/ptyro-daemon/src/usage.rs:1-10`).

#### O-13 — Timers and retries [read] — low to medium
- No step has a default timeout (`runs.rs:870-887`).
- The `total` timeout is a tokio timer around the attempt; after a daemon restart it starts again from zero, unlike gates and sleeps, which keep an absolute deadline.
- Retries start immediately with no backoff (`runs.rs:846-848`); RESEARCH's primitive 8 had `backoff` (`RESEARCH.md:2670`). A rate-limited agent burns its retries in seconds.
- The worktree setup wait is 20 minutes, after which the step fails and the worktree and setup session stay (`runs.rs:998-1015`).

#### O-16 — Nothing is cleaned up (sub-review, [read]) — medium
- No code removes worktrees made by recipe steps, lead workers, or failed and cancelled runs; fan-out candidates go only through `discard`. `gc` lists and never deletes (`crates/ptyro-daemon/src/worktrees.rs:1021-1085`).
- `refs/ptyro/runs/*`, `refs/ptyro/cs/*` and before-restore refs are never deleted; the 200-checkpoint cap is pruned only when that same session checkpoints again (`verify.rs:325-338`).
- `<data>/runs/<run>/` is never pruned.
- Evidence scratch worktrees survive a daemon crash and `reconcile` skips them (`worktrees.rs:972`).
- RESEARCH F20 promised "run teardown removes clean worktrees and keeps dirty ones" (`RESEARCH.md:2379`).

#### O-20 — Change sets (sub-review, [read] unless marked) — several high
1. **A hung git operation holds the set's job for ever.** `cs push` and integrate call git with no timeout (`crates/ptyro-git/src/lib.rs:301-302`, `crates/ptyro-git/src/review.rs:671-691` as cited); afterwards `cs push`, `land` and `rm` answer "busy" until the daemon restarts.
2. **"No silent shipping" is not enforced.** Detail withheld from this public copy (security-relevant).
3. **A lead's write access is wider than its role needs.** Detail withheld from this public copy (security-relevant).
4. **Crash windows.** A daemon stop between closing the `contract-rN` gate and `approve_contract` leaves the set in Drafting for good (`changesets.rs:1779-1792`, `:1773`, `:1417`). A stop after `ContractApproved` and before members are spawned leaves members with no session and no code path that starts them (`:1502-1517`, `:1858`).
5. **Restart during push or land.** The job, the gate waiters and `waiting_at` are in memory only (`changesets.rs:59-82`). A `push-<ms>` gate open at restart stays open and answering it does nothing.
6. **`cs land` does not look at readiness.** It never checks `ready`, `failed`, `stale_contract` or `out_of_fence`, and the `unready` event is never emitted (`changesets.rs:2859-3041`; `crates/ptyro-run/src/changeset.rs:485`).
7. **Restore is not atomic and does not claim the job** (`changesets.rs:2380-2463`); set checkpoint refs are overwritten by a later set of the same name (`:2359`, `:2369`).
8. **Interrupts repeat every 15 s** while an agent stays working, and any member may interrupt the lead or `@all` (`changesets.rs:2035`, `:1216`).
9. **Stuck states with no exit**: a lead that never writes the contract; a member that never says ready; a member blocked on a dependency that failed; a failed lead is not reported at all (`changesets.rs:1857`).

#### O-24 — Conflicts have no ladder (sub-review, [read]) — medium
- Integrate rebases and aborts on failure; pick commits the candidate's work, then integrates, and on conflict returns the error (`evidence.rs:784-803`). No agent is asked to resolve, there is no rerere, and no preview before pick.
- No lock serialises two integrates into one target [suspected].
- R-ORC-11's "merge ladder" is in the row that is marked built (`RESEARCH.md:2940`); only the radar and leases were.

#### O-25 — Evidence and the judge (sub-review, [read]) — medium
*Withheld from this public copy: the detail is security-relevant and is in the private security paper (see the note at the top of `docs/review-2026-11.md`).*

### 2.2 Missing or hollow primitives

#### O-10 — A recipe's `[policy]` is parsed and never used [read]
`Recipe.policy` (`recipe.rs:24-26`) has no reader: the only `.policy` references in the daemon are `RunEntry.policy` (`runs.rs:122`, `:2251`, `:2556-2558`), and recipe runs are created with `None`. SPEC §15.6.2 lists `[policy]` as part of the schema (`SPEC.md:5139`).

#### O-11 — Guards and leases are weaker than they read
*Withheld from this public copy: the detail is security-relevant and is in the private security paper (see the note at the top of `docs/review-2026-11.md`).*

#### O-12 — `builtin:subtasks` has no consumer [read]
The schema exists (`crates/ptyro-run/src/output.rs:12-13`, `:41-43`) and no step kind can turn its result into work: there is no `foreach`, `expand` or sub-recipe. A planner step is a dead end; plan → fan-out cannot be written as a recipe today, only done by a lead agent.

Also absent (all acknowledged as v2 in `RESEARCH.md:2678-2689`): loops over more than one step (so a bounded review loop cannot be expressed), concurrency keys, triggers and schedules, model per step, headless agent steps, compensation, quorum joins, automatic pick.

#### O-14 — No stuck or loop detection [read]
Nothing in the daemon derives `stalled`, `spinning` or `zombie` (grep: no hits). The `stall` timeout is the only signal, it is opt-in per step, and it ends the attempt instead of nudging. R-ORC-09 is `new`.

#### O-15 — No cost signal for a run [read + sub-review]
- Budgets are wall, sessions and attempts (`recipe.rs:42-55`).
- Exact cost exists only for structured Claude sessions and appears only in the compare view (`crates/ptyro-run/src/wire.rs:92-94`).
- (sub-review) Usage is stored per (profile, agent), last writer wins, so per-session cost and context % overwrite each other across sessions (`crates/ptyro-daemon/src/usage.rs:79-94`).
- Context-window % is forwarded by Claude's statusline and not shown on a step (R-ORC-10 `new`).

#### O-18 — Built-in defaults [read]
`best-of-n` defaults its third candidate to `gemini` (`builtin.rs:58`), which RESEARCH records as retired for consumers (`RESEARCH.md:982`); its candidates have no `done_when`, so "idle" is "done". Mixed-vendor defaults are right in principle; the default should come from what is installed.

### 2.3 Adapters: who is first class

From the adapter sub-review ([read] by it; cells cite its evidence). Short form: **Claude is first class; Codex is close but unverified on real turns; everything else can be started and watched but cannot be safely orchestrated.**

| Capability | Claude | Codex | Gemini | OpenCode | Antigravity | Generic | Structured |
|---|---|---|---|---|---|---|---|
| Status source | hooks (15 events) + screen for early dialogs + roster cross-check | hooks (9) + `notify`; title says only "working" | hooks only via an opt-in extension; title | SSE sidecar | opt-in hooks file, written from docs, unverified | OSC 7501 + activity heuristic (2 s output = working, 30 s quiet = idle) | the protocol |
| Turn end | Stop hook | Stop / `notify` | AfterAgent | `session.idle` | Stop `fullyIdle` | heuristic | driver event |
| Blocked detected | yes | yes (payload unverified) | yes | yes | **no** | OSC 7501 only | yes |
| Prompt answerable | keys, or exact via held hook | keys | keys (unverified) | API | **no** | **no** | protocol |
| Tool denial (guards, leases, fences) | yes | yes | BeforeTool | **no** | unverified | **no** | **no** |
| Verified-done | true bounce in the Stop hook | follow-up prompt | follow-up | follow-up | follow-up | follow-up after heuristic idle | follow-up |
| Lead via MCP | yes | yes | **silently none** | **silently none** | **silently none** | **silently none** | Claude stream-json only |
| `--add-dir` in change sets | only if the agent is named exactly `claude` | no | no | no | no | no | — |
| Mail | typed at idle; socket mid-turn (unverified) | typed at idle | typed at idle | typed at idle | typed at idle | typed at idle | `prompt()` |
| Resume | preset id | learned id | preset id | learned id | learned id | **none** | yes |
| Usage / cost | 5 h / 7 d windows, cost, context % | rate limits, opt-in, unverified | none | none | none | none | Claude: tokens and cost; others none |

#### O-19 — What the table hides (sub-review, [read])
1. **Verified-done timeouts disagree.** The check may run 30 minutes (`verify.rs:34`); Claude's Stop hook and the relay give up at 900 s (`crates/ptyro-agents/src/claude.rs:48`, `relay.rs:18`). A check longer than 15 minutes lets Claude end the turn unverified, and the status can stay on `working` (`agents.rs:671-675`).
2. **A stale high-confidence `working` never decays.** The heuristic may only say idle over a low-confidence record (`agents.rs:1349`, `:1370`). After one missed Stop hook, Codex, Gemini and Antigravity stay `working` until another hook speaks; the run step then waits to its timeout.
3. **One adapter's tool denials are not wired correctly,** and its tests pass because the fake agent differs from the real one. Detail withheld from this public copy.
4. **Gemini and Antigravity hooks are opt-in, and the daemon assumes them** (`agents.rs:389-403`). Without them no turn end arrives: no checkpoints, no verify, no lead notices, and no error.
5. **A lead on an agent without MCP injection gets no tools and no message saying so** (`crates/ptyro-daemon/src/launch.rs:539`, `others.rs:65-70`).
6. **Codex blocked is missed when its hooks are held for review** (`crates/ptyro-agents/src/codex.rs:281-304`).
7. **There is no auto-approve, allow-list or approval policy** for tool prompts; the only lever is `dangerous = true` (`launch.rs:474-483`).
8. **Only fakes have exercised any of this.** The fakes call `ptyro hook` directly, have no messaging socket, and never bounce a Codex Stop.

**`ptyro-meaning` is not status detection.** It is a small static embedding model (model2vec, WordPiece) used only for opt-in chat meaning search (`crates/ptyro-meaning/src/lib.rs:1-27`; referenced only from `crates/ptyro-daemon/src/chats.rs:71`). The brief's "screen-scraping meaning" does not exist under that name; screen reading lives in each adapter's `on_screen` / `screen_status`.

### 2.4 What the user can see and do

From the front-end sub-review ([read] by it). `docs/parity.md` rows 223–291 claim near-complete parity; several cells are overstated.

| Capability | CLI | TUI | Web |
|---|---|---|---|
| Start a recipe / fan-out / lead | yes | yes (picker and form) | recipe **partial**: repository recipes are never listed; fan-out and lead yes |
| Run list and steps | yes (`--tree`) | yes | yes; cards show no gate count |
| Run event log | yes | **no** | raw JSON |
| Step logs / check output | 6 lines of reason | 4 lines | reason + JSON dump |
| Retry, cancel | yes | yes | yes |
| Fork | yes, `--set` | CLI text in a split, type the ids | yes |
| Answer a gate | yes | `1`–`9`, `n` note | cards |
| Compare with evidence | yes | yes (+ tool calls, cost) | yes (+ judge) |
| Pick | merge, cherry-pick, PR, paths, hunks | merge, cherry-pick, PR | + files, no hunks |
| Mail: send / ledger | yes / yes | send / **no** | send / **no** |
| Leases | list, claim, release | list as CLI text | list |
| Radar, checkpoints | yes | CLI text in a split | yes |
| A lead's policy | **no** | **no** | **no** |
| Change set swimlanes, ledger | yes | yes (lanes, arrows) | flat list; ledger polled |
| Reject a contract with a note | yes | **no** | yes |
| Review queue | yes | yes | yes |
| Recipe validate / graph / author | validate, graph (Mermaid/dot text) | waived | waived |

#### O-21 — Orchestration never leaves its machine [read]
`RunList`, `RunGet`, `RecipeRun`, `Fanout`, `GateList`, `GateResolve`, `Msg*`, `Lease*`, `Radar`, `Lead`, `Cs`, `Inbox` and `ReviewQueue` are all in `here_only`, "never routed, whatever they name" (`machines.rs:239-316`). MM1 mirrors another machine's workspaces and sessions; its runs and gates are invisible, and no step can run on another machine.

#### O-22 — A user cannot tell what N agents are doing or why one waits [read + sub-review]
- A step row has status, attempt count, bounces, session, elapsed time and diffstat. It never has the agent's live status, its last message, what it is blocked on, tokens, context % or cost.
- There is no timeline, Gantt or live graph for a run. The only timeline is the change-set ledger in the TUI (`crates/ptyro-tui/src/sets.rs:475`). R-ORC-14 says "a timeline for every run remains".
- There is no single list of what waits on the person. Gates are on the runs views and a status-bar count; permission prompts are on session rows; review-ready work is its own queue; `@user` mail is an inbox entry. The inbox is a history of attention events, not a queue of open questions.
- A lead's policy and spawn count are visible only to the lead (MCP `policy`); `RunState` has no policy field.

#### O-23 — Front-end defects (sub-review, [read])
1. The web cannot list or run a repository's recipes: both requests send `project: null` (`web/src/runs.ts:280`, `:319`).
2. A gate notification, and a gate's inbox entry on the web, open a session (the lead or the last one), or `run:<id>` which is not a session, never the run (`runs.rs:659-662`; `web/src/app.ts:317`). The TUI inbox does open the run.
3. Notification action buttons exist only for permission prompts (`web/src/sw.ts:86`); gates have none. SPEC says so (`SPEC.md:5241`).
4. Recipe variable descriptions are dropped before they reach a form (`crates/ptyro-run/src/wire.rs:34-36`), so the forms cannot say what a variable is for — against the "creating shows every option" rule.
5. `claim` is exclusive by default over MCP and shared by default in the CLI (`crates/ptyro/src/mcp.rs:723` against `crates/ptyro/src/orch.rs:221`).
6. Many TUI "views" are CLI output in a split where the user types ids (`crates/ptyro-tui/src/cli_actions.rs:43-71`): fork, checkpoints, recipe show, leases, radar, contract.
7. The TUI answers only the first open gate of a set with `1`–`9` (`crates/ptyro-tui/src/sets.rs:233`).
8. There is no recipe dry-run and no authoring help anywhere; `validate` reports no line numbers beyond TOML parse errors.

### 2.5 Verification, context and safety in one table

| Area | Today | Gap |
|---|---|---|
| Tests as a gate | `done_when`, `run` steps, `verify.tests` on base and candidate | no holdout checks, no test-weakening lint (R-ORC-26 `new`); timeouts disagree (O-19.1) |
| Reviewer agents | `implement-and-review`: one cross-vendor review, one fix, then a gate | no bounded loop, no severity filter, the reviewer shares the writer's worktree with only "don't edit files" in the prompt (`builtin.rs:24-25`) |
| Judge | advisory, after hard evidence | O-25 |
| Comparing attempts | strong: evidence table, clusters, top-two diff | selection is always manual; no cost column |
| Merge | rebase + fast-forward, cherry-pick, PR, hunks | O-24; no single-repo queue (R-ORC-12) |
| Shared context | the contract in change sets; the scratchpad tools for leads (`scratchpad_read/add/todo`) | no run notebook, no handoff brief, no decisions log (R-ORC-20, R-ORC-13 `new`) |
| Handoff | `loop` gives a fresh session per iteration with the last check output | nothing carries what the last iteration learned except the worktree |
| Authority | leads: strong allow-list by kernel-attested caller | O-07, O-11, O-20.2 (detail withheld) |
| Runaway spawn | lead: `max_sessions`, `max_spawns` | none for anyone else; no global cap |
| Budgets | wall, sessions, attempts per recipe | O-01; none for lead runs or change sets; no cost |
| Human escalation | gates with `on_timeout = escalate`; inbox; notifications | O-03, O-22, O-23.2–3 |

### 2.6 SPEC and ROADMAP against the code

#### O-26 — Drift
| Says | Where | Code |
|---|---|---|
| Recipe schema has `[policy]` (for leads) | `SPEC.md:5139` | never read (O-10) |
| "One repair prompt follows an invalid answer" | `SPEC.md:5202` | races (O-02) [suspected] |
| `wall` "fails the run when the clock runs out" | `SPEC.md:5208` | true, including while waiting on a person (O-01) — the text does not warn |
| Delivery: "Never typed into a working … agent"; change sets: "never pasted mid-turn" | `SPEC.md:5341`, `:4973` | socket delivery mid-turn (`mail.rs:651-665`) |
| Loop safety "≤4 hops" | `SPEC.md:5350` | ineffective for hook agents (O-06) |
| "No silent shipping"; per-set turn and time budgets; max member agents | `SPEC.md:4939-4941` | not enforced (O-20.2; sub-review) |
| Fence: where an agent has no hook, `cs status` flags any out-of-fence diff | `SPEC.md:4937-4938` | only changes during the lead's turn are flagged (`changesets.rs:1966-1995`) |
| Train "waits for … the integration checks before the next" member | `SPEC.md:5018-5019` | integration runs once, before the train (`changesets.rs:2933`) |
| Interrupt keys "once" | `SPEC.md:5527` | every 15 s while working |
| `CsEvent` `unready` | `SPEC.md:5464` | never emitted |
| R-ORC-11 "built", including the merge ladder | `RESEARCH.md:2940` | ladder not built |
| Socket delivery "not adopted" | `RESEARCH.md:2930` | adopted in code, unverified (`ROADMAP.md:694`) |
| "The database holds metadata, never contents"; terminal contents never in any database | `AGENTS.md` project rules | `run_message.body` (up to 100 000 chars: diff hunks, contract diffs) and run event payloads (`last_message` read off the screen, test output tails, contract revisions) are in `ptyro.db` (`crates/ptyro-store/src/lib.rs:147-172`; `crates/ptyro-daemon/src/review.rs:810-818`; `mail.rs:58`). Either the rule needs a second named exception or these belong in files |

#### O-27 — The roadmap says this area is finished [read]
Every M7 and M11 task is ticked, and no open `- [ ]` task in `ROADMAP.md` concerns orchestration. About twenty R-ORC rows are still `new` (R-ORC-02, -05, -07, -32 and -34 are marked `new` although largely built, which also hides what is left). The owner reads ROADMAP as the source of truth for what does not exist; for orchestration it currently says "nothing".

---

## 3. Brainstorm

Each idea: what the user experiences in the three front ends, what it needs underneath, a size (S ≤ 1 day, M 2–4 days, L 1–2 weeks, as in ROADMAP), what it depends on, and whether RESEARCH.md or ROADMAP.md already has it. "New" means I found no row for it in RESEARCH Part III or ROADMAP.

Ideas that RESEARCH III.3 rejected are not re-proposed: an LLM router or manager inside the engine, a vector-DB memory, free-form worker-to-worker chat, deterministic replay of agent steps, A2A as a primary protocol, a dependency on MCP Tasks. Where an idea comes near one of them, the entry says how it stays on the right side.

### 3.A Make what exists trustworthy

#### OI-01 — Canary runs against real agents
- **Experience.** CLI: `make orchestrate-canary` / `ptyro doctor --orchestration` runs three tiny recipes on a throwaway repository with each installed agent on its cheapest model and prints a matrix: prompt taken, turn end seen, Stop-hook bounce worked, PreToolUse denial worked, mail seen, permission prompt detected and answered, resume after restart. TUI and web: the same matrix as a panel under Health, each red cell with its fix.
- **Under the hood.** A fixture repository, three recipes, a result schema, a `doctor` section; a nightly CI job that needs logged-in agents is optional. The matrix becomes the adapter capability table in SPEC §8, generated instead of asserted.
- **Size** M. **Depends on** nothing. **Status** new (ROADMAP only says "Needs you: real agents", `ROADMAP.md:767`).
- **Why first.** Every other idea rests on signals nobody has seen from a real agent.

#### OI-02 — Delivery receipts that mean it
- **Experience.** `ptyro msg ls` shows `queued → typed → taken → seen` with ages, and `undelivered (recipient exited)`, `dropped (queue full)`, `expired`. The sender (agent or person) gets a `bounce` message when mail cannot be delivered. TUI and web: the mail ledger marks undelivered mail amber.
- **Under the hood.** `delivered` only after the agent went `working` or the hook saw the id within N seconds; one redelivery, then a dead letter; a reply to the sender on drop, expiry and dead recipient; keep acked rows addressable so the hop count works (O-06). The same "type when idle and confirm it was taken" queue should sit under recipe prompts, `send` steps, retries and repair prompts (O-02).
- **Size** M. **Depends on** nothing. **Status** R-ORC-34 (`new`, though half built) and R-DD-08 (delivery queue primitive).

#### OI-03 — Every agent has a policy
- **Experience.** `ptyro policy show <session>` says what the agent may do and why. An agent that tries to start a fourth agent reads "policy: this session may start 3 agents (2 levels deep); ask @user". Settings: "Agents may start other agents: never / ask me / up to N", in both configuration panels.
- **Under the hood.** A default `Policy` for every agent session, not only leads; `PTYRO_DEPTH` carried through spawns; a global live-agent cap; recipe and fan-out workers get a restrictive policy by default; a recipe's `[policy]` is finally read (O-10). Over-limit opens a gate instead of refusing, when a person is reachable.
- **Size** M. **Depends on** OI-08 for the gate path. **Status** extends R-ORC-30 (lead policy pack, `new`); the default-for-everyone part is new.

#### OI-04 — Run teardown and garbage collection
- **Experience.** When a run ends: "3 worktrees: 1 picked, 2 clean (removed), 0 with unsaved work (kept)". `ptyro run clean [--older 7d]`, a "Clean up" action on a finished run in the TUI and the web, and a line in `doctor` that counts orphaned worktrees, refs and run directories.
- **Under the hood.** On every ending (success, failure, budget, cancel, lead exit): stop the run's sessions unless `keep`, remove clean worktrees through §10.8, keep dirty ones and say so; sweep `refs/ptyro/runs/*`, `<data>/runs/<run>/` and finished run rows by age.
- **Size** S–M. **Depends on** nothing. **Status** RESEARCH F20 names the mitigation; no R- row or task. New as a task.

#### OI-05 — Sensible defaults for unattended steps
- **Experience.** A recipe step with no `timeout` gets one (say `total = 2h`, `stall = 20m`, `start = 3m`) and `recipe validate` says which defaults applied. A wall budget counts working time, not time spent waiting on a person.
- **Under the hood.** Default timeouts in the schema, a paused clock while a run is `waiting`, backoff on retries, persisted attempt deadlines (O-01, O-13).
- **Size** S. **Depends on** nothing. **Status** new (corrections to M7.4).

### 3.B Supervision and the human

#### OI-06 — "Waiting on you": one queue
- **Experience.** One list, everywhere, of everything that cannot proceed without a person: recipe gates, permission prompts, held steps, blocked change-set members, prompts that were not taken, `@user` questions, and review-ready work — from every machine in MM1. Each row says who asks, for which run, how long it has waited and what happens if it is ignored. CLI: `ptyro waiting [--json]`, `ptyro answer <id> <choice>` for any kind. TUI: the inbox grouped by kind with one-key answers and batch approve. Web and phone: a "Waiting" tab as the home screen, a push notification per breakthrough item with signed action links, a badge count on the PWA icon.
- **Under the hood.** One `Waiting` projection over inbox, gates, session status and mail, with a stable id per item; signed links for gates (today only permission prompts have them, `runs.rs:715-716`); routing over the machines link (O-21).
- **Size** M–L. **Depends on** nothing; better with OI-07. **Status** R-UX-02 (partly built M4.5), R-ORC-36 (`new`), SPEC §15.6.3 "Not built: phone links for gates". The cross-machine, all-kinds unification is new.

#### OI-07 — Approval policy rules
- **Experience.** "Always allow `cargo test` in this run", "never allow `git push`", "ask me for anything outside the worktree", "if I do not answer in 10 minutes, deny and tell the lead". Offered at the moment of answering ("allow once / for this run / for this worktree"). Shown and edited in both configuration panels; `ptyro approvals ls`.
- **Under the hood.** A rule table (agent, profile, tool, path, command prefix → allow / deny / ask / escalate-after), evaluated in the PermissionRequest and PreToolUse relays; scopes once, run, worktree; an audit event for each automatic decision. Recipes may carry pre-approvals, shown to the user before the run starts.
- **Size** L. **Depends on** OI-06. **Status** R-ORC-36, R-SEC-03, R-DD-09 (all `new`).

#### OI-08 — Supervisor ladder (deterministic)
- **Experience.** A step shows `stalled 14m (no tool call, no output)`, `spinning (ran "npm test" 6 times with the same failure)`, `blocked 9m on a permission prompt` or `context 92%`. The ladder is configured per recipe or globally: nudge → handoff to a fresh session with a brief → escalate to "Waiting on you". Every rung is an event in the run's log.
- **Under the hood.** Derived states from signals ptyro already has (hook timestamps, PreToolUse command hashes, output and diff deltas, Claude's `context_window.used_percentage`), a small rule engine, and the handoff of OI-16. No model is asked whether an agent is stuck.
- **Size** M. **Depends on** OI-16 for the handoff rung. **Status** R-ORC-09 (`new`), RESEARCH P20/F8, R-ORC-10 (context %, `new`), R-DD-04 (hook-heartbeat stall).

#### OI-09 — Pause, resume and quota-aware runs
- **Experience.** `ptyro run pause R` hibernates the run's agents and stops its clocks; `resume` brings them back. When a profile hits its rate limit, the run shows "paused until 14:20 (Claude 5-hour limit)" and resumes by itself, or offers "move to profile work-2".
- **Under the hood.** A `paused` run status, hibernation of step sessions, the `rate-limited until T` signal that M4.17 already records (`usage.rs`), and an optional profile move.
- **Size** M. **Depends on** OI-05. **Status** R-PRF-01 (session move, `new`), R-STA-05 (partly built); run pause is new.

#### OI-10 — Quiet workers, loud runs
- **Experience.** Ten workers do not make ten notifications. A run speaks with one voice: started, needs you, finished. Workers appear under their run and stay out of the inbox unless they need a person.
- **Under the hood.** A per-session quiet flag, on by default for run workers; attention rolls up to the run.
- **Size** S. **Depends on** nothing. **Status** R-DD-15 (`new`), R-NOT-06 (digest mode, `new`).

### 3.C Seeing what happens

#### OI-11 — `run why` and a timeline for every run
- **Experience.** CLI: `ptyro run why R` prints one sentence per unfinished step ("review waits for impl; impl is blocked 6m on a permission prompt: Bash `rm -rf target`"). TUI: a waterfall per run — a row per step, bars for attempts, marks for gates, mail, bounces, checkpoints and retries, with the wait time on each gate. Web: the same as a scrollable chart; tap a bar to open that moment of the session.
- **Under the hood.** Everything is already in `run_event` plus session events; it needs a joined read model, a "reason" function over step and session state, and the renderer the change-set view already has (`crates/ptyro-tui/src/sets.rs`) generalised.
- **Size** M. **Depends on** nothing. **Status** R-ORC-14 (partly built for change sets in M11.11; "a timeline for every run remains").

#### OI-12 — Cost and budget meters
- **Experience.** Each step and run shows turns, wall time, tool calls, and tokens and dollars where the number is exact, with "≈" where it is estimated and "—" where it is unknown. `[budget] usd = 5` pauses the run at a gate. A pre-run line: "starts up to 4 agents, about 40–90 minutes; Claude 5-hour window is 61% used".
- **Under the hood.** Exact cost exists today only for structured Claude sessions (`crates/ptyro-run/src/wire.rs:92-94`); add per-session attribution from the statusline relay (cost and context are already forwarded, `usage.rs:3-6`), an OTLP receiver for Claude and Codex, and turn budgets for everyone else.
- **Size** M (turn and wall meters, exact where available) to L (OTLP). **Depends on** nothing. **Status** R-USE-02, R-ORC-27, R-OBS-01 (all `new`); Backlog "Usage & budget meters".

#### OI-13 — Audit log and run export
- **Experience.** `ptyro log --run R --json` lists every decision: who approved what, which rule auto-approved, which guard denied, which message was delivered when. `ptyro run export R` writes one bundle (recipe, events, mail, check logs, diffs by ref) for a bug report or a post-mortem. Web: a "History" tab per run.
- **Under the hood.** Mostly a query over tables that exist, plus events for permission decisions.
- **Size** S–M. **Depends on** nothing. **Status** R-SEC-02 (partly built M9.2); Backlog "Timeline scrubber"; R-OBS-03.

#### OI-14 — Run digest in plain words
- **Experience.** Attach in the morning and read: "3 runs overnight. `fix-flaky` passed on iteration 4 and waits for you to ship. `add-tier` stopped at 02:10: codex hit its limit. `docs` has a PR open." One paragraph per run, with the action beside it. Also as one notification.
- **Under the hood.** Deterministic facts from run state, with the optional one-line summariser that already exists (`crates/ptyro-daemon/src/summary.rs`) for the "what it did" clause.
- **Size** S. **Depends on** OI-11's reason function. **Status** R-ORC-31 (`new`); the TUI shows a simpler digest on attach today (`ROADMAP.md:734`).

### 3.D Pipelines

#### OI-15 — Plan → expand → work → verify
- **Experience.** `ptyro recipe run plan-and-build --var task="…"`: a planner (read-only) writes a subtask list; you see it as a gate with the list, edit or approve it; one worker per subtask starts, at most K at a time; each is verified; a final integration check runs. TUI and web show the plan as a checklist that fills in.
- **Under the hood.** A bounded `expand` step that turns a `builtin:subtasks` output into steps (the schema exists and nothing consumes it, O-12), `foreach` with `concurrency`, a plan gate that shows a file, planned-file hints for OI-21. The engine stays deterministic: the agent emits data, the engine validates and schedules it.
- **Size** L. **Depends on** OI-03 (caps), OI-17 (concurrency). **Status** R-ORC-05 v2 (`foreach`, bounded `expand`), R-ORC-06 (spec pipeline, plan gate) — both `new`.

#### OI-16 — Structured handoff
- **Experience.** `ptyro handoff <session> [--to codex]`: the agent is asked for a brief in a fixed shape (goal, done, not done, decisions, files touched, open questions, how to verify), a fresh session starts in the same worktree with that brief, and the old one is hibernated. The brief is shown first, and can be edited. Offered automatically at high context use or after repeated corrections.
- **Under the hood.** A `builtin:handoff` output schema, a role record separate from the session in the run model so a replacement inherits the step, and the checkpoint that already exists at each turn.
- **Size** M. **Depends on** nothing. **Status** R-ORC-13, R-ORC-19 (`new`), R-AGT-08 (cross-agent handoff, `new`), RESEARCH F9.

#### OI-17 — Concurrency and admission scheduler
- **Experience.** "Queued (2 ahead): at most 3 Claude agents on profile `work`". `ptyro queue` shows what waits and why; limits are settings.
- **Under the hood.** Keyed limits (`agent:claude`, `profile:work`, `project:x`, global) consulted by every spawn, including recipe and lead workers, which bypass the review hold today (`runs.rs:1119`, `mail.rs:1384`).
- **Size** M. **Depends on** nothing. **Status** R-ORC-27, R-RUN-01 (`new`).

#### OI-18 — Tournament with automatic selection
- **Experience.** `ptyro fanout -n 4 --test … --pick auto`: when exactly one candidate passes the tests and the repro and did not break anything, ptyro picks it and tells you; when it is close, it asks, showing only the difference between the top two. A staged form: four cheap attempts, the best two continue on a stronger model. Losers stop as soon as a winner is verified.
- **Under the hood.** `pick = { by = "tests" | "judge" | "human", when = "decisive" }` on the verify step, early cancel of siblings, an explicit "decisive" rule (hard evidence only; the judge never decides alone), a second round.
- **Size** M. **Depends on** OI-12 for cost as a signal. **Status** R-ORC-05 v2 names `pick` and `quorum:k`; automatic selection on decisive evidence and the staged form are new.

#### OI-19 — Review loop as a primitive
- **Experience.** `review = { reviewer = "codex", max_rounds = 2, severity = "high" }` on any writing step. The runs view shows "round 2 of 2: 1 issue left (high)". A warning appears when the diff grows from round to round.
- **Under the hood.** A loop over several steps (`loop.steps`), which RESEARCH II.5 proposed and M7.12 did not build; a verdict schema with severities; a diff-growth alarm; the reviewer gets diff, task and test results, never the transcript.
- **Size** M. **Depends on** nothing. **Status** RESEARCH II.5 primitive 12 and F16; no R- row for the multi-step loop. Partly new.

#### OI-20 — Checks an agent cannot game
- **Experience.** `holdout = "scripts.run.acceptance"`: the agent never sees those tests, only "3 of 12 scenarios fail: checkout, refunds, …". The compare and review views flag "deleted 2 tests, weakened 1 assertion". Test files are read-only for implementer roles.
- **Under the hood.** Checks stored outside the worktree and run by the daemon with a redacted result; a diff lint for tests; protected-path fences reusing the change-set fence code.
- **Size** M. **Depends on** nothing. **Status** R-ORC-26 (`new`).

#### OI-21 — Conflict prediction before and after
- **Experience.** Before a fan-out of subtasks starts: "tasks 2 and 4 both plan to touch `src/auth/*`; run them one after the other?". In the compare view: "picking B conflicts with `main` in 2 files". On a conflict at integrate: a ladder — rebase, rerere, a resolver agent in a temporary worktree, then you.
- **Under the hood.** Planned-file hints from the planner's subtasks, `git merge-tree` against the target before pick, and a resolver recipe.
- **Size** M. **Depends on** OI-15 for the planned files. **Status** R-ORC-11 is built for the radar and leases; its "merge ladder" is not. The before-start prediction is new.

#### OI-22 — Single-repo merge queue
- **Experience.** `ptyro land --train`: finished work queues, each entry is rebased on the one before, tested and merged; a failure ejects that entry and tells its agent with the log. TUI and web: a queue lane with positions and states.
- **Under the hood.** The M11.9 train with one repository, speculative prefixes and bisect on batch failure.
- **Size** L. **Depends on** OI-21. **Status** R-ORC-12 ("enabled by M11.9; a single-repo `ptyro land --train` remains").

#### OI-23 — Scouts and read-only fan-out
- **Experience.** `ptyro scout "how does auth refresh work?" -a claude,codex`: disposable worktrees, a findings file each, shown side by side; nothing to merge, and the findings can seed the real run's prompt.
- **Under the hood.** A read-only fence, a findings output schema, automatic discard.
- **Size** S. **Depends on** nothing. **Status** R-ORC-28 (`new`).

#### OI-24 — Same prompt across many repositories
- **Experience.** `ptyro fanout --projects a,b,c "bump the SDK to 4.2"`: one row per repository, each with its own check, one screen to review them.
- **Under the hood.** A fan-out over projects instead of agents; no contract.
- **Size** S. **Depends on** nothing. **Status** R-ORC-29 (`new`).

### 3.E Context

#### OI-25 — A run notebook (the blackboard)
- **Experience.** Every run has `notes.md` and a decisions list. Agents add to it with `ptyro note "…"` or an MCP tool; each new session in the run starts with the current notes; you can read and edit it in the TUI and the web beside the run. It looks and works like the scratchpad that already exists.
- **Under the hood.** A file in the run directory with the scratchpad's atomic write and merge (`crates/ptyro-daemon/src/scratchpad.rs`), size-capped, injected at spawn and on handoff; decisions are typed mail of kind `decision` so they land in the ledger. A file, not a vector store.
- **Size** S–M. **Depends on** nothing. **Status** R-ORC-20 (`new`, ★).

#### OI-26 — Model strategy per step
- **Experience.** `spawn = { agent = "claude", model = "haiku" }`; a `cascade = ["haiku", "sonnet", "opus"]` that moves up only when the check fails; "cheap model drafts, strong model reviews" as a built-in recipe; a `consult` tool by which a worker asks a stronger model one question.
- **Under the hood.** A `model` field on `SpawnDef` mapped to each adapter's flag (UX4.1 already reads the model), retry with `mode = "fresh"` on the next model, a consult tool that starts a short headless session.
- **Size** M. **Depends on** OI-12 to show the saving. **Status** RESEARCH II.5 primitive 13 and P13 (`consult`); not built, no R- row of its own. Partly new.

#### OI-27 — Routing suggestions from past outcomes
- **Experience.** In the creation form: "for test-fixing in this repository, codex won 7 of 9 fan-outs and took half the time". `ptyro stats` shows win rate by agent, model and recipe, time blocked against time working, and which signal picked each winner.
- **Under the hood.** A selection log (candidate, evidence, picked or not, by what) and plain counting. It suggests; it never routes. That keeps it clear of the rejected "LLM router in the engine".
- **Size** M. **Depends on** OI-18 for enough data. **Status** R-OBS-02 (selection log and win rate, `new`); the suggestion in the form is new.

### 3.F Reach

#### OI-28 — Triggers and schedules
- **Experience.** `ptyro schedule "0 2 * * *" recipe:overnight --var task=…`; `on = ["ci.failed", "pr.review_requested", "issue.labeled:agent", "push:main"]` in a recipe; an agent can ask to be woken ("check CI in 30 minutes"). TUI and web: a Schedules list with next run and last result.
- **Under the hood.** A schedule table and a tick; the PR poller that exists (`crates/ptyro-daemon/src/github.rs:636`) as the event source; a webhook on `serve`; a concurrency group per trigger so a storm does not start ten runs.
- **Size** M. **Depends on** OI-03, OI-17. **Status** R-RUN-02, R-RUN-03 (`new`); R-ORC-08 merged into M6.10.

#### OI-29 — Orchestration across machines
- **Experience.** `spawn = { agent = "codex", machine = "studio" }`; a fan-out that puts candidates on three machines; the runs view and "Waiting on you" show every machine; a gate raised on the build box is answered from the laptop or phone.
- **Under the hood.** Runs, gates, mail, leases and change sets are local-only today (`crates/ptyro-daemon/src/machines.rs:239-316`). Stage 1: route reads and gate answers over the link. Stage 2: remote steps, with the diff brought back as a git bundle for compare.
- **Size** L. **Depends on** OI-06. **Status** R-DD-16 (cross-host worktree pull), R-REM-05, R-ORC-17 (cloud attempts) — all `new` or merged to Backlog; run routing is new.

#### OI-30 — ptyro's tools for every agent
- **Experience.** Any agent in ptyro can ask the human a question that lands in "Waiting on you", report progress, claim files, add a note or request a handoff — through a small MCP tool set or the CLI. A worker reads "you have: ask_human, report, claim, note".
- **Under the hood.** A "worker" profile of `ptyro mcp` (five tools, always loaded), injected for every agent that supports it, with the per-session policy of OI-03 behind it; elicitation where the client supports it. The skill text built into the binary for agents without MCP.
- **Size** M. **Depends on** OI-03. **Status** R-ORC-07 (`new`, covers leads), R-DD-12 (merged into R-ORC-18). Tools for non-lead agents are new.

#### OI-31 — ACP in both directions
- **Experience.** Zed or a JetBrains IDE lists "ptyro" as an agent: starting a thread there starts a ptyro session in a worktree, with ptyro's gates and checkpoints. In the other direction, any ACP agent from the registry runs as a structured ptyro worker.
- **Under the hood.** An ACP server face over structured sessions (`crates/ptyro-drive/src/acp.rs` is a v1 client today); registry discovery.
- **Size** L. **Depends on** nothing. **Status** R-ORC-16, R-ACP-01 (partly built M10). A2A stays rejected as a primary protocol; at most an Agent Card facade over this later.

### 3.G Recipes as a product

#### OI-32 — Recipe ergonomics
- **Experience.** `ptyro recipe new` asks five questions and writes a valid file; `ptyro recipe run X --dry-run` plays the recipe with fake agents and prints the schedule, the prompts as rendered and what each gate would ask; the TUI and web forms list every variable with its default (the "creating shows every option" rule); the web draws the graph and highlights the live step; errors point at a line.
- **Under the hood.** A wizard over the schema, the fake adapters as a dry-run backend (`crates/ptyro-testkit/src/fake_adapters.rs`), `uses = "recipe:…"` sub-recipes, role briefs as presets.
- **Size** M. **Depends on** nothing. **Status** sub-recipes are R-ORC-05 v2; role briefs R-ORC-21 (`new`); the wizard and dry-run are new.

#### OI-33 — A recipe library
- **Experience.** `ptyro recipe browse` and a gallery in the TUI and web: bug-fix-with-repro, dependency bump, flaky-test hunt, release notes, migration, security-review, spec-pipeline. Each shows what it starts, what it costs in agents and time, and what it will ask you. Install is copy-and-trust, as repository recipes already are.
- **Under the hood.** A recipes directory in the repository (or a small index file), the existing trust-by-fingerprint.
- **Size** M (mostly writing and testing recipes). **Depends on** OI-01, OI-32. **Status** R-ORC-06 names `spec-pipeline` and `triage-issues`; a browsable library is new.

#### OI-34 — An evaluation harness for recipes
- **Experience.** `ptyro eval recipes/fix.toml --tasks evals/` runs a recipe over a set of tasks (repository at a commit, a prompt, a check) and reports pass rate, time, turns and cost per agent and model; `--compare other.toml` puts two recipes side by side. The result is a table in the CLI and a page in the web.
- **Under the hood.** A task-set format, runs in throwaway clones, the selection log of OI-27 as storage. It is also how OI-01's canary grows up.
- **Size** L. **Depends on** OI-01, OI-12. **Status** new.

#### OI-35 — A task board on top
- **Experience.** A board per project: Backlog, Working, Blocked, Review, Done. A card is a task with a source (the scratchpad, a GitHub issue), a recipe, and its run. Dragging to Working starts the recipe. `ptyro task add|start|ls`.
- **Under the hood.** A daemon-owned task table with claims tied to session liveness and `ready` = dependencies done; importers for issues; the scratchpad's "start a worktree from an item" as the first source.
- **Size** L. **Depends on** OI-06, OI-11, OI-17. **Status** R-ORC-15 (merged → Backlog "Tasks"), R-UX-08 (board view, `new`). Back-burnered by the owner (`ROADMAP.md:3495-3500`).

### 3.H Coverage check against the brief

| Asked for | Ideas |
|---|---|
| Task graph / board | OI-15, OI-35 |
| Plan → fan-out → judge → merge | OI-15, OI-18, OI-22 |
| Tournament with automatic verification | OI-18, OI-20 |
| Reviewer / critic agents | OI-19 |
| Supervisor agent vs deterministic engine | OI-08 (deterministic; an LLM supervisor is rejected in III.3 and I agree) |
| Human-in-the-loop inbox, phone | OI-06, OI-07 |
| Structured handoffs | OI-16 |
| Shared blackboard | OI-25 |
| Budgets and cost meters | OI-12, OI-09 |
| Stuck detection | OI-08 |
| Replay / timeline / audit | OI-11, OI-13 |
| Cross-machine | OI-29 |
| Scheduling, cron, triggers | OI-28 |
| Protocol interop (MCP, ACP, A2A) | OI-30, OI-31 |
| Recipe ergonomics and library | OI-32, OI-33 |
| Heterogeneous models | OI-26 |
| Merge queue | OI-22 |
| Conflict prediction | OI-21 |
| Learned routing | OI-27 |
| Evaluation harness | OI-34 |

---

## 4. A staged path

The order follows one rule: **a run must be safe to leave alone before it is worth making smarter.**

### Increment 1 — "It does what it says" (about two weeks)

Goal: the five built-in recipes complete unattended with real Claude and Codex, and a failed one says why.

1. OI-01 canary runs against real agents; fix what they find first.
2. Fix the correctness findings that break the flagship flows: O-01 (wall budget against gate time), O-02 (the turn-wait race), O-03 (a blocked step is invisible and unbounded), O-04 to O-06 (mail), O-08 (sessions outlive a failed run).
3. OI-05 default timeouts, OI-04 teardown and GC, OI-03 a policy for every agent.
4. OI-11, the small half: `ptyro run why` and the reason line on every step in all three front ends.
5. Re-open the area in ROADMAP: an "ORCH" milestone with the R-ORC rows that are still `new` triaged into it, so the document stops saying orchestration is finished.

Done when: `overnight` and `implement-and-review` run to their gate on a real repository with the laptop closed and reopened, and the morning view explains each run in one sentence.

### Increment 2 — "Leave it alone, answer from anywhere" (three to four weeks)

Goal: ten agents cost the user less attention than two do today.

1. OI-06 "Waiting on you" with phone links for gates, then OI-07 approval rules (the M11.0 experiment counted 13 permission prompts in 8 minutes; this is the gate to any unattended use).
2. OI-08 the supervisor ladder with OI-16 handoff as its second rung.
3. OI-10 quiet workers, OI-14 the digest.
4. OI-11 the full timeline; OI-12 meters for turns, wall time and exact cost where it exists; OI-17 the concurrency scheduler.
5. OI-02 delivery receipts, OI-25 the run notebook.

Done when: a five-step recipe with a fan-out runs for three hours, asks the phone twice, and the timeline shows where every minute went.

### Increment 3 — "The evidence pipeline" (four to six weeks)

Goal: the thing no single-vendor tool has — cross-vendor attempts, hard evidence, automatic selection when it is decisive, and a safe landing.

1. OI-15 plan → expand → work → verify, with OI-21 conflict prediction on the plan.
2. OI-18 tournament with decisive automatic selection, OI-19 the review loop, OI-20 holdout checks, OI-26 model strategy per step.
3. OI-22 the single-repo merge queue.
4. OI-28 triggers and schedules (so the pipeline starts from a CI failure or an issue label).
5. OI-27 the selection log, which starts collecting from the first day of this increment.

Then, in whatever order demand suggests: OI-29 cross-machine, OI-30 worker tools, OI-32 to OI-34 recipes as a product, OI-31 ACP, OI-35 the board.

### What not to build

- **A supervisor or router LLM inside the engine.** RESEARCH III.3 rejects it and the code's best property is that `plan` is a pure function. Keep agents emitting data and the engine validating it (OI-15, OI-08).
- **More recipe language.** No general loops, functions or expressions beyond what `expr.rs` has. If a recipe needs logic, it is a `run` step. The gap is bounded `expand`, `foreach`, multi-step `loop` and sub-recipes, nothing else.
- **A visual workflow builder.** A wizard and a graph view (OI-32) cover it; a canvas editor is a product of its own.
- **Free-form worker-to-worker chat, and more message kinds.** Typed mail to the lead, the notebook and selection tools are enough; RESEARCH III.3 gives the evidence.
- **A vector or embedding memory for agents.** A notes file (OI-25). `ptyro-meaning` should stay a chat-search feature.
- **A2A, and MCP Tasks.** No coding CLI speaks the first; no client supports the second (RESEARCH III.3).
- **More adapters before the conformance matrix exists.** Droid, Copilot and Cursor adapters (Backlog) add columns to a table nobody has measured; OI-01 first.
- **Hosting agents in the cloud, or ptyro's own sandbox.** Cloud attempts as fan-out candidates (R-ORC-17) can come later through the vendors' own surfaces.
- **More change-set machinery.** M11 is already the largest file in the daemon (3.9k lines) for the rarest workflow. Fix its findings below; do not extend it until single-repo runs are solid.
- **Automatic merging without a human by default.** Automatic *selection* on decisive evidence (OI-18) is different from automatic *shipping*; the second should stay behind a gate unless the user sets a rule (OI-07).

---

## 5. What I did not get to, and what is not verified

- **Nothing was run.** Every finding is from reading. O-02 depends on timing and is marked suspected; the concurrent-integrate race in O-24 is suspected.
- **Sub-review findings** (O-16, O-19, O-20, O-23, O-24, O-25, the two tables in §2.3 and §2.4, and the sub-review bullets in O-11 and O-15) were read by reviewers I briefed. I did not re-read every cited line.
- **Not read by anyone:** `crates/ptyro-daemon/src/chats.rs` (chat compare and chat tools; only its use of `ptyro-meaning` was checked), `notifier.rs` beyond confirming it has no gate links, `registry.rs`, `crates/ptyro-agents/src/setup/` and `updates.rs` (installation, not orchestration), the overlay code in `changesets.rs:3590+`, `crates/ptyro-run/src/expr.rs` and `template.rs` (shell quoting of agent-set outputs in `render_shell` deserves its own look), the TUI sidebar rendering of runs, and `crates/ptyro/tests/m7.rs` / `m11.rs` beyond two greps.
- **Not checked against real agents:** whether Codex, Gemini or Antigravity honour the denial JSON; whether Claude's Stop-hook bounce and messaging socket behave as coded; whether a run step resumes correctly after a restart when sessions come back dormant.
- **RESEARCH.md** was read for Part II.1 (patterns and failure modes), II.5 and the R-ORC, R-USE, R-REM, R-NOT, R-GIT, R-RUN, R-OBS, R-EXT and R-DD rows. II.2–II.4 and the product catalogue were not read; an idea I call "new" may have a cousin there.
- **No web research**, by brief.
