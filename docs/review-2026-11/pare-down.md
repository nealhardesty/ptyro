# Pare-down decisions (2026-11-17)

An interview with the owner, feature by feature, after the 2026-11 review
([`../review-2026-11.md`](../review-2026-11.md)). Principle: **deep, not wide**. The CLI, the TUI
and the web stay first class; polishing the TUI and the web is a priority.

Status words: **drop** (remove the code, tests, docs, settings and parity rows), **keep**,
**keep + invest**, **merge**, **trim**, **freeze** (keep green, no new work), **hide** (keep, out of
the default help).

## Decisions by area

### Sessions and terminals

| Feature | Decision | Note |
|---|---|---|
| Sessions: `new`, `attach`, `list`, `kill`, `stop`, `restart`, `rm`, `rename` | keep | |
| Scripting: `send`, `wait`, `read`, `events`, `status`, `answer` | keep | |
| `broadcast`, `hibernate`/`resume` | keep | |
| Terminals: quick terminal (`shell`), SHELLS (`shells`, all of it), plain shell sessions | keep | "A required core feature" |
| Recorder | keep, internal | Deep scrollback and a dormant session's last screen |
| `replay`, `export`, the web replay page | drop | |
| Checkpoints (`checkpoint`) | keep | A native TUI picker instead of the CLI-in-a-split |
| `retitle` (sessions, worktrees, chats) | merge into `rename` | One rename that offers the name and the title |
| `new --again` | drop | |
| `--take-changes` (`new`, `wt new`) | drop | |
| Bare `ptyro "task"` | keep, quoted only | One quoted argument is a task; several unknown words are an unknown command with a did-you-mean (fixes U-11) |
| `adopt` | drop | The shell-wrapper auto-manage route was offered and declined |
| Structured sessions (`--structured`, `convert`) | drop | The user-facing feature only: chat runs its agents as structured sessions (`chats.rs:2168`), so `ptyro-drive` and `structured.rs` stay |
| Summaries (`summarize`, `[summaries]`) | drop | |
| Verified-done (`verify`, `--verify`, `done_when`) | drop | |
| `report` (herdr-compatible) | drop | |
| `shell-init` | drop | ptyro's own shells stay integrated automatically |

### Agents

| Feature | Decision | Note |
|---|---|---|
| Claude, Codex, generic adapters | keep | |
| OpenCode adapter | keep + invest | |
| Antigravity adapter | drop | "For now" |
| Gemini adapter | drop | "For now" |
| Lead agents (`new --lead`, `mcp` for leads) | drop | First kept and reshaped, then dropped (2026-11-17), so the run engine (`ptyro-run`) can go too |

### Orchestration (all of it)

| Feature | Decision | Note |
|---|---|---|
| Change sets (`cs`) | drop | Never used. Daemon, run kind, TUI view, web tab, docs |
| Mail (`msg`) | drop | Never used |
| Recipes (`recipe`) and runs/gates (`run`, `gate`) | drop | Takes the dead `[policy]`, `max_diff_lines`, `on_trip`, `builtin:subtasks`, and O-01 to O-03 with it |
| Fan-out (`fanout`, `compare`, `pick`, `discard`, `iterate`) | drop | "For now." Takes `evidence.rs` and the judge with it |
| Leases (`lease`), radar (`radar`) | drop | |

### Git, worktrees and projects

| Feature | Decision | Note |
|---|---|---|
| Projects, worktrees (`wt new`, `ls`, `rm`, `rename`, `status`, `run`, `open`) | keep | `wt rm` keeps refusing when work would be lost |
| Landing (`wt finish`, `integrate`, `commit`, `log`) | drop | ptyro makes and removes worktrees; git is yours |
| Diff inspector (TUI `D`, web diff, `wt diff`) | drop | |
| PRs (`pr *`, `watch-pr`) | drop | All of it, including the PR-event poller |
| Review (line comments, review queue) | drop | And the `review.max_unreviewed` hold (with it `new --unqueued`) |
| Start from an issue or a PR (`--issue`, `--pr`) | keep | `gh` stays an optional dependency for this only |
| Archives (`wt archive`, `restore`, `archives`, `archive-rm`) | drop | |
| Pair mode (`wt pair`/`unpair`) | drop | |
| Lifecycle scripts | trim | Keep `setup` and `run` scripts, trust by fingerprint, `wt run`, `.worktreeinclude` and `.env` copying. Drop `after_turn` and `archive` scripts |
| `project suggest`, `project line-endings`, `project discover` | drop | Check that auto-registration doesn't feed from `discover.roots` |
| Port allocation and listening-port detection | drop | |
| `wt lock`/`unlock`, `wt rename-branch` | drop | |
| `gc` | keep | |
| Profiles, workspaces | keep | |

### Chat

| Feature | Decision | Note |
|---|---|---|
| Chat | keep + invest | More functionality later, on a smaller base |
| Meaning search, word search, incognito, `chat find` (`mcp --chat`), chat → task | keep | |
| Compare/pick across agents, import, presets | drop | |
| Branches, forks, side chats | drop | Retry and edit become linear: they replace the last turn |

### Attention

| Feature | Decision | Note |
|---|---|---|
| Inbox (`inbox`, TUI `i`, web bell list) | drop | "Needs you" lives on session rows and in notifications |
| Notifications (desktop, OSC 9/777, quiet hours, `mute`, Web Push, sinks, signed links), `notify test` | keep | The whole attention layer, now that the inbox is gone |
| `usage` | keep | |

### Remote and web

| Feature | Decision | Note |
|---|---|---|
| `--remote ssh://` (+ `proxy`), linked machines (`machine`) | keep | Both core. The version-skew findings (exact-version links, Q-36) still want fixing |
| Web server | consolidate | `web on|off|status` is the one way in; `serve` hidden as the developer form; `daemon start --serve` and `[remote] serve` reviewed. Pairing, devices and push stay. Tailscale first, own CA as the fallback; `web on` defaults to localhost |
| Scratchpad (`scratchpad`/`pad`, web Notes, share target) | drop | All of it |

### Setup and housekeeping

| Feature | Decision | Note |
|---|---|---|
| `setup`, `install`, `update`, `login`, `doctor`, `about`, `uninstall`, `config`, `completions`, `daemon *` | keep | |
| `init` | merge into `setup` | One first-run command; bare `ptyro` runs it the first time |
| `tour`, `integrations`, `backup` | drop | |
| Windows (native ConPTY, WSL2) | freeze | Keep it building and green; no new Windows work |

### TUI

| Feature | Decision | Note |
|---|---|---|
| Which-key, mouse mode, palette, row menu, copy mode, splits | keep, polish | |
| The kept split (`screens.rs`: `` ` `` brings back a split layout that something opened over) | drop | Saved views per workspace (`SaveView`/`LoadView`) stay |
| Themes | trim | Keep the built-ins `ptyro-dark`, `ptyro-light` and `high-contrast`, and `NO_COLOR`. Drop user themes (`[themes.*]`), theme editing in both panels, `theme.next` and the preview |
| Sidebar `task` line | drop (done) | The pane header already shows the title; the sidebar is to be compressed |
| Sidebar `last` line | one line (done) | Even when the row is opened up |

## Follow-ups the decisions create

- **`ptyro-run`.** With leads gone it can go, except the wire types that structured sessions
  (chat's engines) and the protocol import.
- **The complete list,** batch by batch: [`drop-list.md`](drop-list.md).
- **Gemini and Antigravity** go from setup, install, login, doctor, the status engine and the
  adapter table.
- **Settings.** Every dropped feature takes its schema entries, config fields and both
  configuration panels' rows with it (`settings.rs`).
- **Docs.** README, SPEC, `docs/commands.md`, `docs/keys.md`, `docs/parity.md`,
  `docs/cli-json.md`, `docs/remote.md` and `docs/config.md` lose the dropped sections. ROADMAP
  records the removals, and the RESEARCH idea funnel marks the affected R- rows.
- **Store.** A migration drops the tables the removed features used (change sets, mail, runs,
  review), with the copy-aside that `ptyro.db` already gets.
- **Protocol.** The removed `Request`/`Reply`/`DomainEvent` variants go; bump the protocol major
  if old clients would misread what is left.
