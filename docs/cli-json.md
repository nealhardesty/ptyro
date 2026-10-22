# `--json` output

Every listing and query command takes `--json`. Its output is an interface: field names and types
change only with a note in the release, and `crates/ptyro/tests/m5.rs` pins the shape of each
one with a snapshot (`crates/ptyro/tests/snapshots/m5__cli-json.snap`). `make snapshots`
accepts an intended change.

Conventions:

- **Times** are milliseconds since the Unix epoch (`*_ms`).
- **Optional fields** are `null`, or missing where noted. New fields may be added at any time,
  so ignore the ones you don't know.
- **Ids:**
  - `id` is the full session id;
  - `handle` is the shortest unambiguous suffix at the time of the call;
  - `name` is what people type.
- **Pretty-printed vs. streaming:** `--json` prints one pretty-printed document. The exceptions
  are `events --json` and `events --follow`, which print one compact JSON value per line
  (NDJSON).
- **Exit codes** don't change with `--json` (see the table at the end).

## Sessions

### `ptyro ls --json` → `[Session]`; `ptyro status S --json` → `{session: Session, explain: [ExplainLine]}`

`Session`:

| Field | Type | Notes |
|---|---|---|
| `id`, `handle`, `name` | string | |
| `kind` | string | `shell`, `command`, `agent`, `script` |
| `argv` | [string] | what runs |
| `cwd`, `live_cwd` | string | `live_cwd`: reported by the shell (OSC 7) or probed |
| `state` | `{t, …}` | `t` is `starting`, `running`, `exited` (`code`, `signal`), `failed` (`error`) or `dormant` |
| `size` | `{cols, rows}` | |
| `pid` | number? | |
| `created_ms`, `ended_ms` | number, number? | |
| `viewers` | number | clients showing it |
| `controller` | string? | client id that sizes it |
| `title`, `foreground` | string? | window title, foreground process name |
| `profile` | string | fixed for its lifetime |
| `worktree`, `project`, `workspace` | string? | worktree id, project id, home workspace name |
| `agent` | string? | configured agent name |
| `script` | string? | `setup`, `archive`, `after_turn`, `run:<name>` for script sessions |
| `ports` | [number] | ports its processes listen on |
| `status` | Status? | the status engine's view (below) |
| `attention` | `{kind, text, since_ms}`? | set when it wants you; `kind` is `blocked`, `errored`, `done`, `idle`, `exited` or `notification` |
| `activity` | string? | `"Bash: cargo test"` |
| `progress` | `{state, percent}`? | OSC 9;4 / 7501 progress |
| `program_status` | `{state, app, at_ms, …}`? | the raw OSC 7501 report |
| `last_command_exit` | number? | shell integration (OSC 133 D) |
| `conversation` | string? | the agent's own conversation id (resume) |
| `children` | [`{id, kind, running}`] | subagents and teammates |
| `dangerous` | bool | started with permission-skipping arguments |
| `prompt_pending` | bool | a `--prompt` waits to be typed |
| `last_prompt_ms` | number? | when ptyro last typed a prompt into it |
| `turns` | number | completed turns (working → idle/done) |
| `last_output_ms` | number? | when its screen last changed |
| `held` | bool | its permission prompt is held for an exact answer from any client (M9.5) |
| `structured` | string? | a structured session's protocol: `stream-json`, `app-server`, `acp` or `opencode` (M10) |

`Status`: `{status, kind?, detail?, confidence, source, since_ms, progress?}`.
- `status` is `unknown`, `starting`, `working`, `blocked`, `idle`, `done` or `error`.
- `kind` is missing when it has no value. For `blocked` it is `permission`, `question` or `auth`;
  for `error` it is `rate_limit`, `auth`, ….
- `confidence` is `high`, `medium` or `low`.
- `source` is `process`, `hook`, `sidecar`, `osc7501`, `report`, `title`, `screen`,
  `notification`, `heuristic` or `protocol` (structured sessions, M10).

`ExplainLine`: `{at_ms, source, signal, applied, reason}`.

`ls --all --json` returns history entries instead:
`{id, name, kind, argv, cwd, state, created_ms, ended_ms, current}`.

### `ptyro read S --json`

`{session, name, state, status?, turns, view, text}`.
- `view` is `screen` or `scrollback`.
- `state` is one word: the status, else `exited`, `dormant` or the process state.

### `ptyro read S --chat` (M10) → NDJSON `ChatItem`; with `--json`, one array

A structured session's transcript, oldest first. `ChatItem` is `{id, at_ms, rev, t, …}`, where
`t` selects the rest:

| `t` | Fields |
|---|---|
| `user` | `text` |
| `assistant` | `text`, `streaming` |
| `thinking` | `text` |
| `tool` | `name`, `title`, `status` (`pending`, `running`, `done`, `failed`, `declined`), `output?`, `diff?`, `files?` |
| `permission` | `request`, `tool`, `title`, `options: [{id, label, kind}]` (`kind`: `allow_once`, `allow_always`, `reject_once`, `reject_always`), `decided?` |
| `plan` | `steps: [{text, status}]` |
| `notice` | `level`, `text` |
| `turn_end` | `stop`, `cost_micro_usd?`, `duration_ms?`, `input_tokens?`, `output_tokens?` |

`rev` is the transcript revision that last changed the item (streamed text grows in place).

### `ptyro wait … --json` and `ptyro send … --wait --json` → `[Outcome]`

`{session, name, reached, state, detail?, turns, matched?, waited_ms}`.
- `state` is `idle`, `blocked`, `done`, `error`, `exited`, `removed`, `matched`, `stalled` or
  `timeout`.
- `matched` is the text the `--match` regex found.

### `ptyro events --json` (NDJSON, oldest first)

`{session, at_ms, kind, detail}`. The `kind` values and what each `detail` holds:

| `kind` | `detail` |
|---|---|
| `status` | `{from, to, kind, detail, source, confidence}` |
| `hook` | `{adapter, event, tool}` |
| `osc7501` | the report |
| `prompt` | `{chars}` |
| `reply` | `{chars}`: the agent's last message of a turn arrived (Claude `Stop`, Codex `notify`) |
| `answer` | `{decision}` |
| `process`, `rename`, `restart`, `resume` | |

### `ptyro events --follow` (NDJSON, live)

`{at_ms, event}`. `event` is a domain event tagged by `t`:
- sessions: `session_created`, `session_updated`, `session_exited`, `session_restarted` (each
  with `info: Session`) and `session_removed` (`id`);
- `controller_changed`, `bell`, `notification`, `notify`;
- projects and worktrees: `project_updated`/`removed`, `workspace_updated`/`removed`,
  `worktree_updated`/`removed`, `worktree_progress`;
- `inbox_added`, `usage_updated`, `config_reloaded`;
- `events_lost` (`count`): this stream fell behind and skipped events, so re-read state with
  `ls --json`.

## Agents

- `ptyro inbox --json` → `[{id, session, session_name, at_ms, kind, title, body, read_ms?, workspace?, profile?}]`
- `ptyro usage --json` → `[{profile, agent, windows: [{name, used_percent, resets_at_ms?}], cost_millis?, context_percent?, limited_until_ms?, at_ms}]`
- `ptyro integrations ls --json` → `[{agent, needs_install, active, detail}]`

## Projects, workspaces, worktrees, profiles

- `ptyro project ls --json` → `[{id, slug, root, default_branch?, remote_url?, pin?, created_ms, last_used_ms?}]`
- `ptyro project show P --json` → `{project, worktrees, scripts}`
- `ptyro project config P --json` → `{config, origin: {"<dotted key>": "<file>"}, imported: [{tool, file, scripts, notes}]}`
- `ptyro project discover --json` → `[path]`
- `ptyro ws ls --json` → `[{id, name, profile, notify, projects: [project id], created_ms}]`
- `ptyro wt ls --json`, `wt status W --json` → Worktree:
  - fields: `{id, project, project_slug, name, path, branch?, base?, is_root, state, locked, profile?, ports?: {base, count}, created_ms, last_used_ms?, removed_ms?, status?}`;
  - `status` is `{head, branch, upstream?, ahead, behind, base_ahead, base_behind, staged, modified, untracked, conflicts, insertions, deletions, subject, at_ms}`.
- `ptyro wt new … --json` → `{info: Worktree, setup?: Session, agent?: Session, agent_pending, untrusted?, notes}`
- `ptyro gc --json` → `[{worktree: Worktree, reasons: [string]}]`
- `ptyro profile ls --json` → `[{name, label, color?, defined, isolate, dirs: {agent: path}, git_email?, matches: {remotes, paths}, vars}]`
- `ptyro profile env P --json` → `{VAR: value}` (secrets as `<secret:NAME>`)

## Review and ship (M6)

- `ptyro wt diff W --json` → DiffDoc `{against, from, truncated, files: [DiffFile]}`:
  - `against` is `base`, `upstream`, `last_turn` or `head`;
  - DiffFile is `{path, old_path?, status, added, removed, binary, hunks: [Hunk]}`, where
    `status` is `added`, `deleted`, `modified`, `renamed` or `untracked`;
  - Hunk is `{header, old_start, new_start, lines: [{kind, old?, new?, text}]}`, where `kind`
    is `context`, `added` or `removed`.
- `ptyro wt log W --json` → `[{sha, subject, author, at_ms, added, removed, files}]`, newest
  first.
- `ptyro pr status W --json` → PrInfo or `null`:
  - fields: `{number, url, title, state, draft, review?, checks: [CheckRun], automerge, ours, at_ms}`;
  - `state` is `OPEN`, `CLOSED` or `MERGED`; `ours` means ptyro opened it and watches it.
  - Worktrees carry the same object as `pr` (`wt ls --json`).
- `ptyro pr checks W --json` → `[CheckRun{name, bucket, link?, workflow?}]`, where `bucket` is
  `pass`, `fail`, `pending`, `skipping` or `cancel`.
- `ptyro review ls W --json` → `[{id, path, line, code?, text, at_ms}]`.
- `ptyro review queue --json` → `[{worktree: Worktree, session?, score, reasons, last_message?, waiting_ms}]`,
  best first.

## Orchestration (M7)

Pinned by `crates/ptyro/tests/m7.rs` (`snapshots/m7__m7-cli-json.snap`).

- `ptyro run ls --json` → `[RunInfo]`, newest first; `run show R --json`, `run wait R --json`,
  `run fork … --json` → `RunInfo`:
  - `RunInfo` is `{state: RunState, order: [step id], dir, children: [run id]}`;
  - `RunState` is `{id, kind, name, recipe_sha, vars, project?, base?, status, created_ms,
    ended_ms?, error?, steps: {id: StepState}, sessions, lead?, forked_from?: [run, step],
    picked: [[step, mode]], discarded, seq}`. `kind` is `recipe`, `fanout` or `lead`; `status`
    is `running`, `waiting`, `succeeded`, `failed` or `cancelled`;
  - `StepState` is `{status, attempts: [Attempt], outputs: {…}, gate?: Gate, started_ms?,
    ended_ms?, reason?, decided_by?}`. Its `status` is `pending`, `running`, `waiting`,
    `succeeded`, `failed`, `crashed`, `skipped` or `cancelled`;
  - `Attempt` is `{n, started_ms, ended_ms?, session?, conversation?, worktree?, start_ref?,
    end_ref?, outcome?, error?, bounces}`;
  - `Gate` is `{id, ask, options, opened_ms, deadline_ms?, choice?, note?, by?, resolved_ms?,
    key?}`.
- `ptyro run events R --json` → NDJSON `{seq, at_ms, event}`; `event.e` is the kind
  (`created`, `attempt_started`, `bounced`, `gate_opened`, `gate_resolved`, `outputs`,
  `attempt_ended`, `step_ended`, `intent`, `outcome`, `picked`, `ended`, …).
- `ptyro compare R --json` → `{run, base?, base_tests?, candidates: [Candidate], top_difference?,
  warnings}`. `Candidate` is `{step, agent?, worktree?, worktree_name?, session?, status, files,
  added, removed, hunks, duration_ms, last_message?, tools?, tests?, broke_tests, repro?, cross?: [passed,
  of], cluster, verified?, judge?, judge_note?, rank, reasons, picked, discarded}`, best first.
  `tools` (structured steps, M10) is `{calls, by_tool: {name: n}, failed, files, commands,
  cost_micro_usd?}`.
- `ptyro gate ls --json` → `[{run, step, gate: Gate}]`, oldest first.
- `ptyro recipe ls --json` → `[{name, description?, source, path?, vars, steps, untrusted?,
  error?}]`. `source` is `builtin`, `user` or `repo:<project>`; `untrusted` is the fingerprint
  to pass as `--trust`.
- `ptyro msg ls --json` / `msg check --json` → `[{id, run?, at_ms, from, to, kind, body,
  in_reply_to?, priority, state}]`, where `state` is `queued`, `delivered`, `seen` or `acked`.
  `msg send --json` → `{ids, to}`.
- `ptyro lease ls --json` → `[{id, holder, holder_name, pattern, exclusive, reason?,
  expires_ms}]`.
- `ptyro radar --json` → `{project, worktrees: [[worktree, [file]]], overlaps: [{a, b, files,
  conflicts}]}`.
- `ptyro checkpoint ls --json` → `[{n, session, at_ms, turn, sha, ref, head, files}]`.
- `ptyro status S --json` sessions gain `verify` (`{check, max_bounces, bounces, state, last?:
  {command, exit_code, tail, tree?, at_ms, duration_ms}}`; `state` is `pending`, `checking`,
  `passed`, `bounced` or `failed`) and `run` (`r-7f3a2c/impl`), when set.

## Change sets (M11)

Pinned by `crates/ptyro/tests/m11.rs`.

- `ptyro cs new … --json`, `ptyro cs show SET --json` → `ChangeSetInfo`; `ptyro cs ls --json` →
  `[ChangeSetInfo]`, newest first:
  - `ChangeSetInfo` is `{run, status, set: ChangeSet, report: Report, live: {member:
    MemberLive}, lead?, sessions: {member: session name}, gates: [GateInfo], job?}`. `status` is
    the run's; `job` is `push` or `land` while one runs.
  - `ChangeSet` is `{name, root, layout, workspace?, profile, goal?, template?, lead_agent?,
    gates, integration, members: [Member], contract: [ContractRev], checkpoints:
    [SetCheckpoint], checks: [CheckResult], phase, train?}`. `layout` is `lead`, `solo` or
    `manual`; `phase` is `drafting`, `working`, `landing`, `landed` or `removed`.
  - `Member` is `{name, project, agent?, depends_on, overlay?, base?, contract_check?,
    worktree?, path, branch, start?, session?, contract_rev, ready? (ms), ready_rev, blocked_on,
    failed?, conflict?, check?: CheckResult, commit?, pushed?, pr?, pr_url?, landed?,
    out_of_order?, no_remote}`.
  - `ContractRev` is `{n, at_ms, by, sha, text, diff, affects, summary?, approved?}` (`affects`
    empty: everyone).
  - `SetCheckpoint` is `{n, label?, at_ms, reason, members: {member: [head, snapshot]}}`;
    `reason` is `manual`, `contract`, `land` or `restore`.
  - `CheckResult` is `{command, ok, exit_code?, tail, at_ms, duration_ms}`.
  - `Train` is `{order, done, stopped?: [member, why], started_ms}`.
  - `Report` is `{state, issues: [{state, member?, detail, action}], members: {member: word},
    ready: [ready, total]}`. Issue states: `land_stopped`, `member_conflict`, `member_failed`,
    `out_of_order`, `missing_worktree`, `integration_failed`, `check_failed`, `stale_contract`,
    `out_of_fence`, `no_remote`, `blocked_on`. Without one, `state` is `drafting`,
    `contract_review`, `working`, `ready`, `landing`, `landed` or `removed`.
  - `MemberLive` is `{status?, dirty, ahead, out_of_fence, missing}`.
- `ptyro cs log SET --json` → NDJSON `{seq, at_ms, t, kind, from?, to?, text}`; `t` is `event`
  (a run event, `seq` > 0) or `message` (`kind` is the message's kind).
- `ptyro cs check SET --json` → `{ok, integration: [CheckResult], members: {member:
  CheckResult}}`; the exit code is 1 when `ok` is false.
- `ptyro cs checkpoints SET --json` → `[SetCheckpoint]`.
- `ptyro cs templates --json` → `[{name, description?, source, path, members, untrusted?,
  error?}]`; `source` is `user` or `hub:<project>`.
- `ptyro run show R --json` for a set's run carries the same `ChangeSet` as `state.changeset`.

## Remote (M9)

- `ptyro pair NAME --json` → `{device: DeviceInfo, url, expires_ms}`. `url` carries a
  one-time pairing code in its fragment (`#pair=ptp_…`), not the token: the web app trades it
  for the token (`POST /api/pair` on `ptyro serve`) once, before `expires_ms` (10 minutes).
  `device.pending` is true until then.
- `ptyro devices ls --json` → `[DeviceInfo]`: `{id, name, scope, created_ms, last_seen_ms?,
  last_via?, push, pending}`. `scope` is `read`, `approve`, `control` or `full`; `last_via` is
  the address it last connected from; `push` counts its Web Push subscriptions; `pending`: its
  pairing code hasn't been claimed yet.


- `ptyro doctor --json`, `ptyro profile doctor --json` → `[{section, name, level, text}]`.
  `level` is `ok`, `note`, `warn` or `problem`; any `problem` makes the exit code 1.
- `ptyro daemon status --json` → `{version, proto: {major, minor}, pid, started_ms, sessions, live_sessions, clients, socket, log_dir, generation, upgraded_ms, rolled_back, metrics}`
  - `generation`: live upgrades since the daemon started; `upgraded_ms`: when the last one happened (or `null`); `rolled_back`: the last upgrade was refused by the new binary and rolled back.
  - `metrics`: `{rss_kib, open_fds, tasks_alive, loop_lag_ms_max, loop_lag_ms_p99, pty_readers, bytes_read, frames_sent, crashes, scrollback_bytes, sessions: [[name, bytes]]}`. Event-loop lag covers the last minute; the totals carry across upgrades.
- `ptyro daemon upgrade --json` → `{from, to, pid, sessions, rolled_back, ms}` once the new image answers (exit 1 if `rolled_back`).
- `ptyro config check --json` → `{path, exists, ok, notes, error?}`
- `ptyro config show --json` → the effective configuration, defaults filled in.

## Exit codes

| Code | Meaning |
|---|---|
| `0` | ok (for `wait`: the condition was met) |
| `1` | error (for `doctor`: a problem was found; for `pr checks`: a check fails) |
| `2` | usage: bad arguments or values |
| `3` | not found |
| `4` | ambiguous: a handle or selector matched several |
| `5` | conflict: unsaved work, a profile question, or (`wait`) a session exited first |
| `6` | untrusted scripts |
| `7` | timeout (`wait`, `send --wait-ready`), `wait --stall` saw no output, or (`pr checks`) checks still running |
| `8` | no daemon |
| `9` | protocol mismatch: the daemon speaks another major protocol version (`ptyro daemon restart --hibernate`); within a major version, `ptyro daemon upgrade` switches it over without stopping sessions |
