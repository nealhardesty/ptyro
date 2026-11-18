# The drop list (2026-11-17)

Everything the pare-down removes, by feature, on each surface, from the decisions in
[`pare-down.md`](pare-down.md). Built from `ptyro --help`, the TUI's `ACTIONS`
(`crates/ptyro-tui/src/keymap.rs`), the web routes, the settings schema and module headers.
**It is an inventory to work from, not a proof:** each batch starts by grepping for what is
listed, and anything shared with a kept feature stays (see "Shared, so it stays").

Each batch is one branch and PR with its own `make bump`. In every batch: remove the CLI command,
the TUI action and key, the web action and page, the protocol variants, the daemon code, the
settings schema entries and config fields, the tests, the `docs/parity.md` rows, and the doc
sections (README, SPEC, `docs/commands.md`, `keys.md`, `cli-json.md`, `config.md`). Record the
removal in ROADMAP and mark the affected R- rows in RESEARCH.

## Batch 1: change sets

| Surface | What goes |
|---|---|
| CLI | `cs` and all 24 subcommands (alias `changeset`): `crates/ptyro/src/cs.rs` |
| TUI | `sets`, `sets.new`, `sets.templates`, every `set.*` (checkpoints, restore, contract, overlay, commit, push, pr, land, add, drop, remove); `S`; `crates/ptyro-tui/src/sets.rs` |
| Web | the Sets tab, `#/sets`, `g e`, every `set.*` action in `runs.ts` |
| Daemon | `changesets.rs` (3.9k lines), the `changeset` run kind, the cs interrupt path in `mail.rs`, `--add-dir` handling for set members |
| Crates | `ptyro-run/src/changeset.rs` (1.8k lines) |
| Store | the change-set tables and refs `refs/ptyro/cs/*` (a migration drops the tables) |
| Tests | `m11.rs`, `land.rs` |

## Batch 2: orchestration (mail, leads, recipes, runs, gates, fan-out, leases, radar)

| Surface | What goes |
|---|---|
| CLI | `msg`, `lease`, `radar`, `run` (ls/show/events/wait/cancel/retry/fork/lead), `recipe` (ls/show/validate/graph/run), `gate` (ls/resolve), `fanout`, `compare`, `pick`, `discard`, `iterate`; `new --lead`, `--max-workers`, `--lead-agent`; `ptyro mcp` without `--chat`: `crates/ptyro/src/orch.rs` and the lead part of `mcp.rs` |
| TUI | `runs` (`O`) and its views (steps, compare, gates), `recipe.list/show/run`, `run.fork`, `session.lead`, `session.message`, `leases`, `radar`; `crates/ptyro-tui/src/runs.rs`; the run and gate counts in the status bar |
| Web | the Runs tab, `#/runs`, `#/recipes`, `#/leases`, `#/radar`, `g r`, the fan-out sheet, the compare view, gate cards, "Make it a lead" on the new-task sheet, `session.message` |
| Daemon | `runs.rs` (2.6k), `mail.rs` (1.9k), `evidence.rs`, the run-kind parts of `daemon.rs`, the lead policy in `guard_caller`, gate push links (notifier) |
| Crates | `ptyro-run`: all of it except what structured sessions and the protocol still import (wire types such as tool calls and cost; check `structured.rs` and `ptyro-protocol`). Ideally the survivors move into `ptyro-drive` or `ptyro-protocol` and the crate goes |
| Store | `run`, `run_event`, `run_message`, the lease tables; refs `refs/ptyro/runs/*` |
| Settings | `orchestration.*` (4) |
| Tests | `m7.rs`, `gates.rs`; the fan-out parts of `m10.rs`; the lead and mail parts of others |

## Batch 3: PRs, review, the inspector, landing, verify

| Surface | What goes |
|---|---|
| CLI | `pr` (create/status/checks/fix/automerge/watch/reply/open), `watch-pr`, `review` (add/ls/rm/send/queue/done), `verify`, `wt finish`, `wt integrate`, `wt commit`, `wt log`, `wt diff`; `new --verify`, `new --unqueued`: `crates/ptyro/src/ship.rs` |
| TUI | the inspector (`D`) and all its tabs: `crates/ptyro-tui/src/inspector.rs`; `review.queue`, `worktree.finish` (`F`), `pr.reply`, `pr.open`, `session.verify`; the verify field on the session form |
| Web | `#/review`, `review.ts`, every `pr.*`, `review.*`, `worktree.finish/commit/integrate/log`, `session.verify` |
| Daemon | `github.rs` except issue/PR lookup for `--issue`/`--pr`, `review.rs`, `finish.rs`, the PR-event poller, the verified-done half of `verify.rs` (the Stop-hook bounce, `done_when`), the `max_unreviewed` hold in `daemon.rs` |
| Settings | `github.*` (6: draft, escalate_after, log_lines, merge_method, poll, watch), `review.*` (2), `verify.*` (3) |
| Tests | `m6.rs` (review and ship), the verify parts of `m10.rs` |

## Batch 4: worktree and project extras

| Surface | What goes |
|---|---|
| CLI | `wt archive`, `wt restore`, `wt archive-rm`, `wt archives`, `wt pair`, `wt unpair`, `wt lock`, `wt unlock`, `wt rename-branch`, `wt retitle` (into `wt rename`), `project suggest`, `project line-endings`, `project discover`; `--take-changes` on `new` and `wt new`; `new --again` |
| TUI | `worktree.archive`, `worktree.restore`, `worktree.archive_delete`, `worktree.pair`, `worktree.unpair`, `worktree.lock`, `worktree.unlock`, `worktree.rename_branch`, `worktree.retitle`, `project.suggest`, `project.line_endings`, `session.again` (`C-q C-n`), the restore picker |
| Web | `#/archives`, `worktree.archive/restore/archive_delete/pair/unpair/lock/unlock/rename_branch/retitle`, `project.suggest`, `project.line_endings`, the "+ claude" quick-new button |
| Daemon | archive and restore in `worktrees.rs`, `suggest.rs`, pair mode, `again.rs`, port allocation and listening-port detection (`background.rs` ports part, `ptyro-pty/src/ports.rs`), the `after_turn` and `archive` lifecycle scripts, taking changes (`reset --hard`/`clean -fd` on the user's checkout) |
| Settings | `ports.*` (2), `worktrees.archive_on_merge`, `worktrees.archive_purge_days`, `worktrees.auto_rename_branch`, `worktree.share`/`share_mode` (pair mode; verify), `discover.roots`/`depth` (only if auto-registration doesn't use them) |
| Tests | the archive, pair, lock, ports and suggest parts of `m3.rs` and `ux1.rs` |

## Batch 5: session extras

| Surface | What goes |
|---|---|
| CLI | `convert`, `--structured` on `new`, `summarize`, `adopt`, `report`, `replay`, `export`, `shell-init`, `retitle` (into `rename`); bare `ptyro` with several unquoted words stops being a task (U-11) |
| TUI | `session.convert`, `session.summarize`, `session.adopt` (`A`, the "A adopts it" detail line), `session.replay`, `session.export`, `session.retitle` |
| Web | `#/history` (replay), `session.convert`, `session.summarize`, `session.adopt`, `session.replay`, `session.export`, `session.retitle` |
| Daemon | `summary.rs`, the adopt path in `agents.rs`, the `report` handler, user-facing structured sessions (structured *machinery* stays for chat), `crates/ptyro/src/record.rs` (replay/export) |
| Settings | `summaries.*` (3) |
| Tests | `m10.rs` (structured sessions, keep what chat needs), the summary, adopt, replay and export parts of `ux1.rs` and `m2.rs` |

## Batch 6: the scratchpad and the inbox

| Surface | What goes |
|---|---|
| CLI | `scratchpad` / `pad` (edit/path/cat/add/todo/done/start/log/restore), `inbox` |
| TUI | `scratchpad.toggle/add/items/note/history`, the pad popup (`crates/ptyro-tui/src/pad.rs`), `inbox` (`i`), `attention.next` (verify: may stay as "next session wanting you") |
| Web | the Notes page (`#/notes`, `notes.ts`, `pad.ts`), the share target and its offline queue, `#/inbox` and the bell's list, `g i` |
| Daemon | `scratchpad.rs`, the inbox table and its requests (notifications keep firing) |
| Settings | `scratchpad.*` (5); `attention.*` reviewed (kept only where it drives notifications) |
| Tests | `note1.rs`; the inbox parts of `m4.rs`/`m9.rs` |

## Batch 7: chat trims

| Surface | What goes |
|---|---|
| CLI | `chat compare`, `chat pick`, `chat import`, `chat presets`, `chat set --preset`, `--preset` on `chat new`/`ask`, `chat branch`, `chat fork`, `chat keep` (side chats), `chat new --compare`; retry and edit become linear |
| TUI | `chat.compare`, `chat.pick`, `chat.import`, `chat.preset`, `chat.fork`, `chat.side`, `chat.branch`, `chat.branch_prev`, `chat.branch_next`, `chat.keep`, `chat.list_imported` |
| Web | the same actions in `chats.ts`; side-chat delete-on-leave goes away (fixes W-14) |
| Daemon | `chats/compare.rs`, `chats/import.rs`, presets, the node tree (branches) in `ptyro-store/src/chat.rs`: a migration flattens each chat to its current branch |
| Settings | `chat.preset`, `chat.tools.*` (the presets' tools; verify what an unpreset chat needs), `chat.import` |
| Tests | `chat_compare.rs`, `chat_import.rs`, the branch/fork/side parts of `chat_more.rs` and `chat_tools.rs` |

## Batch 8: agents and setup

| Surface | What goes |
|---|---|
| Agents | Gemini and Antigravity: adapters (`ptyro-agents/src/others.rs` parts), catalog entries (`catalog.toml`), install, login, update, doctor checks, `ptyro-drive`'s ACP mapping for Gemini, the status-engine paths for both |
| CLI | `integrations` (ls/install/uninstall), `tour`, `backup`, `init` (into `setup`) |
| TUI | `integrations`, `integrations.install`, `integrations.uninstall`, `help.tour` and the tour coach (`tour.rs`), `app.backup`; the first-run `init` questions fold into setup |
| Web | `#/integrations`, `#/tour`, `backup.create`, `#/setup`'s init step |
| Daemon / CLI crate | `crates/ptyro/src/tour.rs`, `backup.rs`, `init.rs` (merged), the integrations module |
| Tests | the Gemini and Antigravity modes of the fake agent in `m4.rs`; the backup parts of `uninstall.rs`; tour tests in `app.rs` |

## Batch 9: TUI and web compaction

| Surface | What goes |
|---|---|
| TUI | the kept split (`crates/ptyro-tui/src/screens.rs`: `` ` `` and the layout kept under a full-sized panel; saved views per workspace stay); user themes, `theme.next`, the theme preview and theme editing (the built-ins `ptyro-dark`, `ptyro-light`, `high-contrast` and `NO_COLOR` stay); **done already:** the sidebar's `task` line is gone (the pane header shows the title), `last` takes one line, and a blocked row is one line (its key shows on the row you're on, or beside the request when the sidebar has room) |
| Web | theme editing; `#/web` and the four ways to start the server fold into `web on|off|status` (`serve` hidden); the Sets, Runs and Notes tabs are already gone by now |
| Settings | `themes.*` (17); `ui.theme` stays as a pick of the three built-ins |

## Shared, so it stays

- **`ptyro-drive` and `structured.rs`.** Chat runs its agents as structured sessions
  (`chats.rs:2168`), so only the user-facing `--structured`/`convert` goes.
- **Checkpoints.** They live in `verify.rs` beside verified-done; split the file, keep the
  checkpoint half (`refs/ptyro/ckpt/*`).
- **`ptyro mcp --chat`.** It serves `chat find`.
- **`approvals.rs`.** Remote approval of permission prompts from the phone (`answer`) stays.
- **`sidecar.rs`.** OpenCode's event stream.
- **`gh`.** Only for `--issue`/`--pr` on new worktrees.
- **The recorder.** It stays as internal storage; only `replay`/`export` go.

## What stays (for the record)

Sessions and the scripting verbs; `broadcast`, `hibernate`/`resume`; the quick terminal, SHELLS
and shell sessions; checkpoints; Claude, Codex, OpenCode and the generic adapter; projects,
worktrees (new/ls/rm/rename/status/run/open), trimmed lifecycle scripts, `gc`; profiles and
workspaces; chat (linear, word and meaning search, incognito, `chat find`, chat → task); notifications,
`mute`, `notify test`, `usage`, `answer`; `--remote` and `proxy`, linked machines; the web app with
pairing, devices and push; `setup`, `install`, `update`, `login`, `doctor`, `about`, `uninstall`,
`config`, `completions`, `daemon`; Windows, frozen.

## Order

1 and 2 first (the most self-contained, and what the owner was surest of), then 3, 6, 4, 5, 7, 8,
9. Batch 2 must leave `ptyro-run`'s survivors in place for structured sessions (chat). The protocol
major goes up once, at the end of batch 2, if an older client would misread what is left.
