# ptyro — Specification

> **ptyro** (pronounced *TIE-roh*, from **PTY**) is a terminal-native agent development environment:
> projects, isolated worktrees, many coding agents in parallel and orchestration, delivered as a fast
> Rust TUI on top of its **own** terminal multiplexer. No tmux, no Electron, no mandatory GUI. It is
> an independent design; the tools that inspired parts of it are credited in §20.6.

This document is the source of truth for *how ptyro works*: architecture, protocol, formats, file
layout, configuration, CLI surface, and every hard-won fact we learn along the way. What does **not**
exist yet is tracked in `ROADMAP.md`. How to install and use what *does* exist is in `README.md`.
What other products do, and the candidate ideas that come from them, is in `RESEARCH.md`. That file
is the intake funnel for `ROADMAP.md`; nothing in it is a commitment until promoted to a task id.

Status markers used below:

- **[decided]** — settled; change only with a new ADR entry (§19).
- **[proposed]** — the current plan; may change during implementation without ceremony.
- **[spike]** — must be validated by an experiment before we build on it (see ROADMAP M0).
- **[verified YYYY-MM-DD]** / **[unverified]** — for external facts (§20).

---

## Table of contents

1. Goals, non-goals, principles
2. Glossary and domain model
3. Architecture overview
4. Process and threading model (daemon internals)
5. Terminal layer: PTYs, emulation, input, queries
6. Client/daemon protocol
7. Sessions: lifecycle, persistence, hibernation, resume
8. Agent adapters
9. Status engine and attention
10. Projects, worktrees, setup scripts, ports, profiles
11. The TUI
12. The CLI
13. Configuration
14. Storage: files, directories, database
15. Orchestration
16. Remote access
17. Security model
18. Observability, testing, performance budgets
19. Architecture decision records (ADRs)
20. Facts, gotchas, and external references
21. Open questions

---

## 1. Goals, non-goals, principles

### 1.1 Goals

1. **An agent workspace that lives in the terminal.** Projects, isolated git worktrees, many agent
   sessions in parallel, status at a glance, review, and orchestration — all keyboard-driven, inside
   the terminal you already use.
2. **Detach/reattach like tmux.** Sessions are owned by a background daemon and survive the client
   going away (closed terminal, dropped SSH, laptop lid). Reattach from anywhere and get the exact
   screen back.
3. **Agents are first-class.** Claude Code is the primary agent; Codex CLI, Gemini CLI and OpenCode
   are supported from early on. ptyro *knows* when an agent is working, blocked on you, idle, or dead,
   without scraping the screen.
4. **Speed.** Native Rust. Keystroke-to-child overhead is imperceptible; idle CPU is ~zero; startup is
   instant.
5. **Everything is scriptable.** Anything the TUI can do, the CLI can do, with `--json`. Agent sessions
   are addressable resources (`ptyro send`, `ptyro wait --for blocked`, `ptyro events`).
6. **Remote-ready architecture.** The protocol is designed from day one so a web/phone client or a
   remote CLI can attach to the same sessions later, without re-architecting.

### 1.2 Non-goals (for now)

- Being a general-purpose tmux replacement. We implement what an agent workspace needs: persistent
  sessions, splits, scrollback, copy, navigation. Not tmux's full command language.
- Being a terminal emulator *application*. ptyro runs inside your terminal (Ghostty, kitty, WezTerm,
  Alacritty, iTerm2, foot, …), possibly inside tmux.
- Being an agent. ptyro hosts agents; it never calls an LLM API itself (this may change in the future).
- Built-in editor, task tracker (**back-burnered**; see ROADMAP Backlog).
- Windows native. WSL2 is expected to work like Linux. **[decided]**

### 1.3 Principles

- **The daemon owns the truth.** PTYs, processes, terminal state, status, metadata. Clients are thin.
- **Never scrape when you can be told.** Status comes from agent hooks and terminal protocols
  (OSC 7501); heuristics are a labelled fallback with explicit low confidence.
- **Never lose work.** No automatic deletion of a worktree with uncommitted or unpushed changes, ever.
- **Agent-native, not agent-specific.** Adapters declare capabilities; the core never hard-codes Claude.
- **The TUI is optional.** CLI/API parity is a requirement, not a nice-to-have.
- **Boring persistence, sharp terminal.** SQLite + plain files for state; all the cleverness goes
  into terminal fidelity and the attach experience.
- **Small surface, deep quality.** Fewer features, each one polished, before breadth.

---

## 2. Glossary and domain model

### 2.1 Terms **[decided]**

| Term | Meaning |
|---|---|
| **Daemon** | `ptyro daemon` — the long-lived background process. One per user per machine. Owns everything below. |
| **Client** | Anything connected to the daemon socket: the TUI, a CLI invocation, a hook relay, later web/phone. |
| **Workspace** | A named, user-defined grouping of projects plus a saved view (sidebar state, layout, focused session), an attention policy (§9.6) and a **profile** (§10.10). The closest analogue to a tmux *session*. A default workspace named `main` (profile `default`) always exists. |
| **Profile** | A named identity bundle — agent accounts/config dirs, git author and SSH key, `gh` config, env and secrets — defined in user config. Every session runs under exactly one profile, fixed for its lifetime (§10.10). |
| **Project** | A registered git repository (its main checkout path). A project may appear in several workspaces. |
| **Worktree** | An isolated `git worktree` checkout of a project on its own branch, created and tracked by ptyro. The project's main checkout is also represented as a (special, non-deletable) worktree named `@root`. |
| **Session** | One process running in one ptyro-owned PTY, with its own terminal state — an agent (`claude`, `codex`, …) or a plain shell/command. Sessions are what you attach to. |
| **Agent** | A configured session kind backed by an adapter (`claude`, `codex`, `gemini`, `opencode`, `shell`, or a user-defined generic command). |
| **Conversation** | The agent's own notion of a chat (Claude `session_id`, Codex `thread_id`, …). A ptyro session *has* zero or one current conversation id; resuming a dormant session reuses it. Never confuse the two ids. |
| **Pane** | A rectangle in a client's layout that displays one session. Panes are client-side view state; sessions are daemon-side. |
| **Status** | The agent-level state of a running session (`working`, `blocked`, `idle`, …) — §9. |
| **Attention** | A per-session flag meaning "something happened you haven't looked at" (§9.4). |
| **Run** | (Orchestration) a durable, multi-session job: fan-out, recipe execution, or a lead-agent mailbox (§15). |

### 2.2 Entity relationships

```text
Workspace *──* Project 1──* Worktree 1──* Session 0..1── Conversation
    │ *          │ 0..1                │ 1        │ 1
    └─1 Profile ─┴─ (pin)    Profile 1─┘          └─1 Profile, 1 home Workspace
                     (sessions may also be ad-hoc: no project, any cwd)
Run 1──* Session  (orchestration membership, optional)
```

### 2.3 Identifiers **[proposed]**

- Every entity has a **ULID** (`01JB…`), sortable by creation time, stored in full.
- Humans see a short, unique **handle**: `s:7f3k` (sessions), `w:…` (worktrees), `p:…` (projects),
  computed as the shortest unambiguous lowercase Crockford-base32 suffix (min 4 chars).
- Sessions and worktrees also get a **friendly name**, generated by default from a corvid-themed
  word list (`brisk-rook`, `quiet-jackdaw`, `sly-magpie`) and editable. Names are unique per project.
- **Address syntax** used across CLI and protocol (resolved by the daemon):

  | Form | Example | Resolves to |
  |---|---|---|
  | handle | `7f3k`, `s:7f3k` | session by short id |
  | name | `brisk-rook` | session by name (error if ambiguous across projects) |
  | qualified | `myrepo/brisk-rook` | session by project + name |
  | `@focused` | | the session focused in the most recently active TUI |
  | `@last` | | most recently created session |
  | `@attention` | | oldest session with attention set |
  | `@blocked` / `@idle` / `@working` | | all sessions in that status (multi) |
  | `@all` | | all running sessions (multi) |
  | `@run:<id>` | | all sessions of a run (multi) |

**[built M2]** Implemented now: handles (`7f3k`, `s:7f3k`, full id), names, `@last`, `@focused`
(the session most recently focused in any TUI, reported with `ClientMsg::Focus`), and the multi
selectors `@all`, `@live` (alias `@running`), `@exited`, `@working`/`@blocked`/`@idle`/`@done`/`@error`
(by program status), name globs (`build-*`, `a?c`), and `,` to join several (`a,b,@blocked`).
Results follow the session list's order without duplicates. Single-target requests accept a
selector only if it matches exactly one session (otherwise `ambiguous`). The CLI expands
selectors with `Request::Resolve`, so `ptyro kill @exited`, `ptyro send 'agent-*' …` and
`ptyro rm @all` work. `myrepo/name`, `@attention` and `@run:` arrive with projects, attention and
runs.

**[built M3]** `project/name` resolves a session by project and name. Projects are addressed by
slug, id suffix (≥ 4) or any path inside one of their checkouts. Worktrees are addressed by
`project/name` (`myrepo/@root` is the main checkout), a unique name, an id suffix, or a path
inside them, so CLI commands default to the worktree you're standing in. Ids are lowercase ULIDs
(`ptyro_core::model::new_id`). Session names stay globally unique: agent sessions default to the
agent's name (`claude`, `claude-2`), and script sessions to `<worktree>.<script>`.

---

## 3. Architecture overview

### 3.1 Big picture **[decided]**

```text
 ┌─────────────── clients ───────────────┐
 │  ptyro (TUI)   ptyro <cmd> (CLI)       │   later: web/PWA, phone, remote TUI over ssh
 │  ptyro hook (agent hook relay)         │
 └───────────────┬───────────────────────┘
                 │ Unix socket, length-delimited MessagePack (§6)
 ┌───────────────▼──────────────────────────────────────────────────────────┐
 │ ptyro daemon                                                             │
 │                                                                          │
 │  Supervisor (registry, addressing, commands)   Status engine (§9)        │
 │  Client hub (subscriptions, frame pacing)      Notifier (inbox, OSC 9…)  │
 │  Session actors ×N:  PTY ⇄ Emulator ⇄ row-versioned screen  ⇄ recorder   │
 │  Adapter layer: claude | codex | gemini | opencode | shell | generic     │
 │  Git/worktree service (git CLI)   Script runner (setup/run/archive)      │
 │  Store actor (SQLite)             Config loader (TOML, layered)          │
 └───────────────┬──────────────────────────────────────────────────────────┘
                 │ owns PTY masters; children are its direct child processes
     ┌───────────┼───────────┬─────────────┬─────────────┐
   claude      codex       gemini       opencode      $SHELL / nvim / …
 (each in its own PTY, own process group, cwd = its worktree)
```

### 3.2 Why this shape

- **The application must outlive the interface.** Close the terminal; agents keep working.
- **One binary.** `ptyro` contains the CLI, the TUI, the daemon (`ptyro daemon`) and the hook relay
  (`ptyro hook`). Client and daemon always come from the same build, which makes version skew a
  daemon-restart problem rather than a compatibility matrix.
- **State-based protocol.** The daemon runs the terminal emulator and ships *screen state* (rows of
  styled runs), not raw bytes. New clients get a keyframe instantly, slow clients skip frames instead
  of buffering forever, and a phone can render without a full VT emulator.

### 3.3 Cargo workspace layout **[proposed]**

```text
ptyro/
├── Cargo.toml                 # [workspace], shared deps & lints, edition 2024
├── crates/
│   ├── ptyro/                 # the binary: clap entry → cli | tui | daemon | hook
│   ├── ptyro-core/            # ids, domain types, addressing, status model, config schema
│   ├── ptyro-protocol/        # wire types, codec, version negotiation (no daemon deps)
│   ├── ptyro-client/          # async client library (used by tui, cli, hook, tests)
│   ├── ptyro-pty/             # spawn/resize/signal, process groups, cwd/fg-process probes
│   ├── ptyro-term/            # Emulator trait + alacritty backend, row versions, snapshots,
│   │                          #   query responder, input encoding, OSC side-channel parser
│   ├── ptyro-daemon/          # actors: supervisor, sessions, client hub, notifier, scripts
│   ├── ptyro-agents/          # adapters: claude, codex, gemini, opencode, shell, generic
│   ├── ptyro-git/             # worktree ops (git CLI), fast reads (gix, optional)
│   ├── ptyro-store/           # rusqlite schema, migrations, store actor
│   ├── ptyro-run/             # orchestration as data: recipes, templates, conditions, the
│   │                          #   event-sourced run state, the pure engine (M7, §15.6)
│   ├── ptyro-tui/             # ratatui client
│   └── ptyro-testkit/         # fake agent, temp-daemon harness, screen assertions
├── docs/                      # long-form design notes, recordings, compatibility matrix
├── Makefile                   # the automation entry point: every workflow is a target (AGENTS.md)
└── deny.toml                  # cargo-deny policy
```

Boundary rules **[decided]**:

- `ptyro-term` knows nothing about agents, projects or the network. Its only job is making a process
  believe it runs in a good terminal, and exposing that terminal's state.
- `ptyro-protocol` depends only on `serde`/`ptyro-core` types; a future WASM web client can reuse it.
  **[M7]** It also depends on `ptyro-run`, whose wire types it carries; `ptyro-run` itself does no
  I/O and depends only on `ptyro-core`.
- `ptyro-run` decides and the daemon executes: scheduling rules are pure functions with unit
  tests, never daemon code.
- Only `ptyro-agents` contains agent-specific knowledge. Adding an agent never touches the daemon core.
- Only `ptyro-store` talks SQL.

### 3.4 Platform support **[decided]**

| Platform | Status |
|---|---|
| Linux x86_64 / aarch64 | Tier 1 |
| macOS aarch64 / x86_64 | Tier 1 |
| WSL2 | Tier 2 (Linux build; tested occasionally) |
| Windows native | Not supported (ConPTY is a different world; revisit much later) |

MSRV: latest stable at project start, pinned in `rust-toolchain.toml`; edition 2024. **[proposed]**

### 3.5 Core dependencies **[proposed — versions checked on crates.io 2026-10-19]**

| Purpose | Crate | Notes |
|---|---|---|
| Async runtime | `tokio` 1.x | multi-thread runtime in daemon; current-thread in CLI |
| Framing | `tokio-util` 0.7 `LengthDelimitedCodec` | u32 BE length prefix |
| Wire encoding | `rmp-serde` 1.3 (named) | MessagePack maps; JSON for debug (§6.2) |
| PTY | `pty-process` 0.5 (tokio feature) | async `Pty`, `tokio::process::Child`; fallback `rustix-openpty` |
| Emulator | `alacritty_terminal` 0.26 (pinned `=`) | behind `Emulator` trait; libghostty-vt is the future upgrade path |
| Input encoding | `terminput` 0.5 + `terminput-crossterm` | key/mouse → xterm or kitty bytes |
| TUI | `ratatui` 0.30, `crossterm` 0.29 | |
| CLI | `clap` 4 (derive), `clap_complete` 4.6 | dynamic completion of session names |
| Storage | `rusqlite` 0.40 (`bundled`), `rusqlite_migration` 2.6 | one store thread |
| Paths | `etcetera` 0.11 | XDG layout on macOS too |
| Config | `toml`, `serde` | |
| Logging | `tracing`, `tracing-subscriber`, `tracing-appender` | |
| Fuzzy find | `nucleo` 0.5 | pickers |
| IDs | `ulid` | |
| Process info | `sysinfo` 0.39 (or `/proc` + `libproc` direct) | cwd, fg process, ports |
| Git reads | `gix` 0.89 (optional) | worktree add/remove always via git CLI |
| Files | `notify` 8 + debouncer; `reflink-copy` | CoW copies for shared dirs |
| Tests | `insta`, `proptest`, `assert_cmd` | |

Explicitly **not** used: `bincode` (unmaintained, RUSTSEC-2025-0141), `daemonize` (fork after tokio
start is unsound), `vt100`/`tui-term` for primary panes (no reflow, no kitty keyboard, no query
replies — see §5.3), `tarpc`/`jsonrpsee` (our traffic is server-push streams).

---

## 4. Process and threading model (daemon internals)

### 4.1 Startup **[proposed]**

1. Resolve paths (§14.1). Create runtime dir with mode `0700`.
2. Acquire an exclusive lock on `state/daemon.lock` (std `File::try_lock`); write pid. If locked,
   exit with "daemon already running (pid N)".
3. If a stale socket file exists: probe-connect; if nothing answers, unlink it.
4. Bind the socket under `umask(0o077)`; verify `sun_path` length (104 bytes on macOS).
5. Open/migrate SQLite. Load config. Reconcile (§7.5).
6. Signal readiness: write `ready` line to the inherited "ready pipe" fd if spawned by a client, and
   `sd_notify(READY=1)` when running under systemd.

**Auto-spawn.** Any client that cannot connect starts the daemon itself: `ptyro daemon start
--spawned` (binary: `$PTYRO_BIN`, else the client's own executable), stdio → `/dev/null`. The
daemon calls `setsid()` first thing, before any runtime exists, so it never forks an initialized
tokio process. **[built M1]** Readiness is "the socket accepts": the client polls `connect` every
20 ms for ≤ 5 s, and if the child exits early it points at the log directory. That is simpler than
the planned ready pipe and equally robust (a second client racing the first simply finds the
socket). Users may instead run the
daemon under a systemd user unit or a launchd agent (`ptyro daemon install-service`).

### 4.2 Actors **[proposed]**

Everything is message passing over `tokio::sync::mpsc`; no shared mutable state across actors except
`Arc<watch>` snapshots for read-mostly data.

**[built M1]** M1 ships the session, client-hub and store-less parts. Two pragmatic choices:
- The supervisor is a registry behind one `RwLock`, which serializes mutations just as an actor
  would; it becomes an actor with the store in M3.
- A session's emulator sits behind a short `Mutex` that subscription tasks lock to build frames,
  rather than requesting them by message. Lock order is always `meta → term`. The PTY writer queue
  is unbounded on purpose: the reader must never wait on the writer, or a child that stops reading
  input while it writes output would deadlock both.

| Actor | Count | Owns | Notes |
|---|---|---|---|
| **Supervisor** | 1 | registry of workspaces/projects/worktrees/sessions, address resolution, config | single task; all mutations serialize through it |
| **Session** | 1 per session | PTY master, child handle, `Emulator`, row-version table, recorder, adapter instance | reads PTY on a task; never blocks on clients |
| **Client hub** | 1 | connections, subscriptions, per-client frame pacing & acks | one writer task per connection |
| **Status engine** | 1 | signal fusion per session (§9) | pure reducer; easy to unit-test |
| **Notifier** | 1 | inbox, desktop/OSC notifications, mute rules | |
| **Store** | 1 OS thread | the `rusqlite::Connection` (WAL, `busy_timeout`) | async callers send closures + oneshot |
| **Git service** | pool (≤4) | `git` subprocess invocations | serialized per project to avoid lock contention |
| **Script runner** | per script | setup/run/archive processes (each in its own PTY session, kind `script`) | |
| **Ingress** | per transport | hook events, socket accept, later WebSocket | |

### 4.3 The session hot path

```text
PTY master ──read (async, 64 KiB buf)──► OSC side-channel sniffer ─► Emulator.feed(bytes)
                                              │                         │
                               (OSC 7501, OSC 9/777, OSC 133,     damage → bump row versions,
                                OSC 7, OSC 0/2 titles, BEL)        cursor/mode/title changes
                                              │                         │
                                       Status engine/Notifier     notify client hub (watch<seq>)
Emulator replies (DA, CPR, kitty ?u, OSC 7501 ?) ──► PTY master write
Recorder: append raw bytes + timestamps to sessions/<id>/pty.log (ring, §7.6)
```

Invariants **[decided]**:

- The PTY is **always drained** into the emulator, regardless of clients. A stalled client must never
  stall a child (the #1 failure mode of naive multiplexers).
- Terminal **query replies are generated by the daemon** even when no client is attached — Claude
  Code, Neovim and others probe at startup and stall or degrade if unanswered.
- One session's panic is caught at the actor boundary (`JoinHandle` + `catch_unwind` around feed);
  the session is marked `failed` and its child is left running if possible.

### 4.4 Shutdown

- `ptyro daemon stop` → refuse if sessions are running unless `--force` (or `--hibernate`, which
  hibernates resumable agents first, §7.4). Then SIGHUP each session's process group, wait 3 s,
  SIGKILL, flush store, remove socket, release lock.
- SIGTERM to the daemon = `stop --hibernate`. SIGINT in foreground mode = same.

### 4.5 Live upgrade (re-exec handoff) **[built M8]**

Goal: replace the daemon binary **without killing sessions**.

1. `ptyro daemon upgrade` (or the TUI noticing a newer binary on disk) asks the daemon to hand off.
2. Daemon quiesces: stop reading PTYs (kernel buffers hold output briefly), flush store.
3. Serialize a **handoff image** (versioned, separate from the wire protocol): sessions, their fds,
   child pids, emulator state (grid via `alacritty_terminal`'s `serde` feature, plus explicit
   `TermMode`, cursor, title stack), row versions, adapter state. Written to a `memfd` (Linux) or an
   unlinked temp file (macOS).
4. Clear `FD_CLOEXEC` on PTY masters, the listening socket and the image fd; `execve` the new binary
   with `PTYRO_HANDOFF_FD=<n>`.
5. Same pid ⇒ children are still *our* children; `waitpid` and exit codes keep working (this is why
   we re-exec instead of passing fds to a new process via `SCM_RIGHTS`, which would orphan children).
6. New daemon restores, resumes reading, clients reconnect automatically (they see a socket EOF and
   retry with backoff) and receive keyframes.

Fallback if handoff fails validation: the new binary refuses to start sessions-in-flight and the old
image is used to re-exec the *old* binary (kept at `state/daemon.prev`).

**[built M8]** As designed, with these specifics:
- **Asking.** `ptyro daemon upgrade [--binary P] [--json]` sends `Upgrade{binary}` (default: the
  binary the daemon was started from, `/proc/self/exe` minus " (deleted)", so "install, then
  upgrade" works). The daemon first runs `P daemon handoff-version` and refuses unless the list
  it prints contains `IMAGE_VERSION` (1). It copies its own executable to `state/daemon.prev`,
  answers `Upgrading{from, to, binary, sessions}`, and starts the handoff 150 ms later. Agent
  sessions can't ask (`guard_caller`). The CLI then polls `Status` until `generation` goes up,
  and exits 1 if `rolled_back`.
- **Quiescing.**
  - Every client gets `Goodbye{reason: "upgrading"}`.
  - The accept loop stops. New connections wait in the listen backlog, which the new image
    inherits with the socket.
  - Every PTY reader parks on a `watch` pause channel, and the handoff waits until all of them
    have (`readers == parked`), so no read is half-fed to an emulator. Output waits in the kernel.
  - The store is flushed.
- **The image** is `rmp_serde` named maps, versioned, in a memfd (Linux) or an unlinked `0600`
  file in `state/` (macOS). It holds:
  - the daemon's `run_id`, `started_ms` and `generation`, the listener fd, foreground or not,
    the focused session, OpenCode sidecars and the watchdog's running totals;
  - per session: `Meta` (minus client links), the status record and the status engine's clocks
    (so a hook's `blocked` still outranks heuristics afterwards), activity, output counters,
    the PTY master fd, and the emulator.
  - **The emulator** (`EmuImage`):
    - the primary grid with its scrollback, and the alternate grid when it's active, via
      alacritty's serde;
    - the cursor and saved cursor, cursor template and style (DECSCUSR), title and colours;
    - the modes, re-fed as their escape sequences, and the kitty keyboard flag stack.
    - It's compact MessagePack, then zstd level 1: a 10k-line history shrinks about 50×, and
      three sessions went from 210 MB of named maps to 194 KB.
    - Sessions are encoded and decoded in parallel threads: 10 sessions × 12k lines take 0.25 s
      to write and 0.25 s to restore (release build).
- **exec.** `FD_CLOEXEC` is cleared on the masters, the listener and the image; then `execve`
  `P daemon start --foreground|--spawned` with `PTYRO_HANDOFF_FD`. The new image marks
  everything close-on-exec again.
- **Adoption.**
  - Masters come back through `Pty::from_fd`.
  - A reaper collects children this runtime didn't spawn: `waitpid(pid, WNOHANG)` on SIGCHLD
    and every 5 s. Exit codes and signals arrive as before.
  - Recorders reopen at their position, and sidecars restart.
  - Each live session gets a SIGWINCH nudge. Its state is already exact; this is for programs
    that track their own damage.
  - Runs (§15) in flight continue on the new image without re-sending prompts (`handed_off`).
- **Rollback.** A new binary that refuses the image (unknown version, a decode error, or
  `PTYRO_TEST_REFUSE_HANDOFF` in tests) execs `state/daemon.prev` with the same fd and
  `PTYRO_HANDOFF_ROLLBACK=1`, and that image never refuses. `daemon status` then says
  `rolled_back`.
- **Clients.** The TUI reconnects and gets keyframes. `wait`, `send --wait`, `run wait` and
  `events --follow` reconnect and continue with what's left of their timeout.
  - A `send` whose `SendText` was cut off mid-wait is sent again only if the session's
    `last_prompt_ms` shows it wasn't typed, so nothing is typed twice.
  - A request on a connection that is already gone fails at once. It used to wait forever; the
    soak found that.
- **Status.** `DaemonStatus` gains `generation` (upgrades since start), `upgraded_ms` and
  `rolled_back`.
- **Limits.**
  - The first upgrade *to* M8 needs a restart, because older daemons don't know `Upgrade`.
  - A major protocol bump can't be upgraded into. The handshake is rejected before `Upgrade`
    can be sent.
  - Something typed while the old image is between its reply and the exec is lost with that
    connection; the client sees the error.

---

## 5. Terminal layer: PTYs, emulation, input, queries

### 5.1 PTY management (`ptyro-pty`) **[proposed]**

- Open with `pty-process` (`Pty::new()`, `pts()`, `Command::spawn(pts)`): child becomes session
  leader with the PTY as controlling terminal (`setsid` + `TIOCSCTTY`).
- Each child gets its **own process group**; signals go to the group (`killpg`) so tool subprocesses
  (dev servers, test runners) die with the session. Stop sequence: `SIGHUP` → 3 s → `SIGTERM` → 2 s →
  `SIGKILL` (configurable per agent; Claude prefers `SIGINT` to end a turn cleanly).
- **Resize** via `TIOCSWINSZ` on the master; the kernel delivers SIGWINCH to the foreground group.
- **Inspection** (polled ≤ 1 Hz, only for visible or recently-active sessions):
  - foreground process group: `tcgetpgrp(master)`; name via `/proc/<pid>/comm` (Linux) or
    `proc_name` (macOS);
  - cwd: `/proc/<pid>/cwd` (Linux) / `proc_pidinfo(PROC_PIDVNODEPATHINFO)` (macOS), preferring
    OSC 7 reports from the shell when present;
  - listening TCP ports of the process tree (Linux `/proc/net/tcp*` + fd inodes; macOS `libproc`) —
    shown in the sidebar (§11) and used for port-conflict warnings.
- **Child environment** (§8.2 adds adapter-specific variables):

  | Variable | Value |
  |---|---|
  | `TERM` | `xterm-256color` (configurable; we do not ship a terminfo entry initially) |
  | `COLORTERM` | `truecolor` |
  | `TERM_PROGRAM` / `TERM_PROGRAM_VERSION` | `ptyro` / crate version |
  | `PTYRO` | `1` |
  | `PTYRO_SESSION_ID` | session ULID |
  | `PTYRO_PROFILE` | the session's profile name (§10.10) |
  | `PTYRO_SOCKET` | daemon socket path (for `ptyro hook` and nested `ptyro` CLI use) |
  | `PTYRO_PROJECT`, `PTYRO_WORKTREE_PATH`, `PTYRO_WORKTREE_NAME`, `PTYRO_BRANCH`, `PTYRO_PORT` | when in a worktree (§10.6) |
  | removed | `TMUX`, `TMUX_PANE`, `STY`, `ZELLIJ*`, `WEZTERM_PANE`, `KITTY_WINDOW_ID`, `ITERM_SESSION_ID`, `TERM_SESSION_ID` — the child must see *ptyro* as its terminal, not the outer one |
  | removed | `COLUMNS`, `LINES` (stale from the outer terminal), `WEZTERM_UNIX_SOCKET`, `KITTY_LISTEN_ON`, `GHOSTTY_BIN_DIR`, `WT_SESSION`, and an enclosing ptyro session's `PTYRO_SESSION_ID` (re-set for the new session) |
  | from the client | `SSH_AUTH_SOCK`, `SSH_CONNECTION`, `DISPLAY`, `WAYLAND_DISPLAY`, `XAUTHORITY`: the *spawning* client's values win over the daemon's, like tmux's `update-environment` |
  | removed | markers of a *parent* Claude Code session: `CLAUDECODE`, `CLAUDE_CODE_CHILD_SESSION`, `CLAUDE_CODE_ENTRYPOINT`, `CLAUDE_CODE_EXECPATH`, `CLAUDE_CODE_MESSAGING_SOCKET`, `CLAUDE_CODE_MESSAGING_TOKEN`, `CLAUDE_CODE_SESSION_ATTENDED`, `CLAUDE_CODE_SESSION_ID`, `CLAUDE_EFFORT`, `CLAUDE_PID` (§20.1: inherited, they turn off the child's transcript saving and leak a messaging token) |

  Profile-managed variables and, for isolated profiles, inherited credentials are removed and
  re-applied per §10.10.

  Removing `TMUX` matters concretely: with `$TMUX` set, Claude Code clamps to 256 colours and probes
  tmux instead of its terminal for OSC 7501 (§20).

### 5.2 The `Emulator` trait (`ptyro-term`) **[proposed]**

```rust
pub trait Emulator: Send {
    fn feed(&mut self, bytes: &[u8]);                 // parse child output
    fn resize(&mut self, size: TermSize);             // reflows when the backend supports it
    fn take_damage(&mut self) -> Damage;              // rows changed since last call
    fn row(&self, line: Line) -> RowView<'_>;         // styled cells, wide/grapheme aware
    fn cursor(&self) -> CursorState;                  // pos, shape, visible, blinking
    fn modes(&self) -> TermModes;                     // app cursor/keypad, mouse modes+encoding,
                                                      //   bracketed paste, focus events, kitty kbd flags,
                                                      //   alt screen, sync-output pending
    fn title(&self) -> Option<&str>;
    fn scrollback_len(&self) -> usize;
    fn drain_replies(&mut self) -> Vec<Vec<u8>>;      // DA/DSR/CPR/... answers to write back to the PTY
    fn drain_events(&mut self) -> Vec<TermEvent>;     // Bell, ClipboardStore, Title, Hyperlink, ...
    fn snapshot(&self) -> TermImage;                  // serializable full state (handoff, tests)
}
```

Backends:

1. **`AlacrittyEmulator`** (default) — `alacritty_terminal::Term` + `vte::ansi::Processor`. Gives
   reflow on resize, scrollback, damage tracking, kitty keyboard mode flags, SGR mouse, focus
   (1004), bracketed paste, synchronized output (2026, buffered in the processor), OSC 8 hyperlinks
   per cell, OSC 52 as events, true colour, zero-width combining chars. Pinned with `=` because its
   API is tailored to Alacritty and breaks across minor versions.
2. **`GhosttyEmulator`** (future, feature-gated) — `libghostty-vt` bindings: higher fidelity
   (graphemes, kitty graphics, OSC 133, built-in VT/HTML formatter) but needs Zig at build time and
   its types are `!Send` (would run on a dedicated thread per session). Revisit when its API is stable.
3. **`NullEmulator`** (tests) — records bytes only.

### 5.3 Why not vt100 / tui-term **[decided]**

`vt100` has excellent ANSI re-emission but no reflow, no kitty keyboard protocol, no synchronized
output, no query replies, limited graphemes, and heavy memory with large scrollback (shpool is
rewriting away from its vt100 fork for these reasons). `tui-term` is a thin vt100→ratatui widget;
painting styled runs into a ratatui `Buffer` ourselves is ~100 lines and avoids coupling.

### 5.4 Row versioning (how deltas are computed) **[proposed]**

- The session keeps `seq: u64`, incremented once per *batch* of output processed (not per byte).
- Each visible row `r` has `row_ver[r] = seq` of its last modification (from emulator damage).
- Cursor, modes, title, size, and scroll position each carry their own version.
- A client that has acknowledged `acked_seq` needs exactly the rows with `row_ver > acked_seq`, plus
  any changed scalar state. No per-client diff buffers, no history: O(rows) per frame.
- A full-screen scroll marks every row dirty; optimization (a `Scroll{n}` op) is deferred until
  measured.
- **[built M1]** Emulator damage only *nominates* rows: each nominated row's content is hashed
  and its version bumps only if the hash changed. alacritty reports the cursor row damaged on
  every batch, which would otherwise resend it constantly. It also **under-reports** in one known
  case: overwriting a wide character that wrapped clears the *leading spacer* left at the end of
  the previous row, but only the current row is marked. The row above each damaged row is
  therefore nominated too. Found by `ptyro-term/tests/props.rs`, which checks that incremental
  deltas always equal a fresh keyframe (20k random cases in `make proptest-deep`).
- **Scrollback** is never pushed. **[built M2, design change]** Instead of paging ranges with a
  `scrollback_epoch` (unworkable: once history is full, alacritty evicts lines without any counter,
  so ranges drift under the reader), a client asks for a **frozen document**:
  `ReadScrollback { addr, lines }` returns the last `lines` history lines plus the screen, as rows
  with their own style table and cursor (`ScrollbackDoc`). Copy mode, mouse-wheel scrolling and
  `ptyro read --scrollback` work on that snapshot while the live screen keeps updating, just as
  tmux's copy mode freezes. 10 000 lines of 200 columns is about 1 MB of msgpack, built in a few
  milliseconds.

### 5.5 Input (client → child) **[proposed]**

Clients do **not** send raw bytes for keys; they send abstract events and the daemon encodes them
against the **child's current modes** (only the daemon knows them):

- `Key { code, mods, kind: press|repeat|release, text }` → `terminput` encoding:
  - if the child pushed kitty keyboard flags → kitty `CSI … u` per flags;
  - else xterm encoding honouring DECCKM (app cursor) and DECKPAM (app keypad);
  - release/repeat events dropped unless the child asked for them.
- `Mouse { kind, button, x, y, mods }` → only forwarded if the child enabled a mouse mode; encoded in
  the child's chosen encoding (SGR 1006 preferred). Otherwise the client uses the mouse locally
  (selection, scrollback, pane focus).
- `Paste { text }` → wrapped in `ESC[200~ … ESC[201~` iff bracketed paste is on; any embedded
  `ESC[201~` is stripped (paste-injection defence). Newlines normalized to `\r` when not bracketed.
- `Focus { gained }` → `CSI I` / `CSI O` iff mode 1004 is on. Focus is sent honestly: a session is
  focused when it is the focused pane of the *controlling* client and that client's outer terminal
  is focused. Codex/Gemini use focus to decide whether to notify (§20).
- `RawBytes { bytes }` — escape hatch for `ptyro send --raw` and the `--raw` attach mode.

The TUI asks the *outer* terminal for kitty keyboard enhancement
(`PushKeyboardEnhancementFlags`) when supported, so it can distinguish `Ctrl-I`/`Tab`,
`Shift-Enter`, etc., and therefore re-encode them faithfully for Claude Code.

### 5.6 Query responder **[proposed]**

The daemon answers, on behalf of the (possibly absent) outer terminal:

| Query | Reply |
|---|---|
| DA1 `CSI c` | `CSI ? 62 ; 22 c` (VT220 + ANSI colour) |
| DA2 `CSI > c` | `CSI > 0 ; <version> ; 0 c` |
| XTVERSION `CSI > q` | `DCS > \| ptyro <version> ST` |
| DSR `CSI 5 n` / CPR `CSI 6 n` | OK / cursor position |
| DECRQM `CSI ? <mode> $ p` | mode state (incl. 2026 synchronized output = supported) |
| Kitty keyboard query `CSI ? u` | current flags |
| OSC 10/11/12 colour queries | the attached controller's real colours if known (queried once from the outer terminal at client start), else theme defaults |
| **OSC 7501 probe** `OSC 7501 ; ? ST` | `OSC 7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth ST` — no spaces (the spec's grammar has none; its prose example does) (§9.2) |
| XTGETTCAP | minimal set (`TN`=ptyro, `Co`=256, `RGB`) |
| Window ops (`CSI 14/16/18 t`) | pixel/cell sizes from the controller, else a sane default. alacritty answers 14 and 18 but **not 16** (cell size), which Claude Code asks for at startup → ours |

Ordering rule: the OSC 7501 probe is followed by `CSI c`; programs treat "DA arrives first" as
"unsupported", so the 7501 reply must be written **before** the DA reply when both arrive in the same
read batch. **[verified M0 spike]** Implementation: the side-channel scanner sees each read batch
*before* the emulator, so replies it owns (7501, XTVERSION, `CSI 16 t`) are queued ahead of the
emulator's `PtyWrite` replies; Claude Code then sends 7501 reports.

Who answers what in the alacritty backend **[verified 0.26.0]**: alacritty answers DA1 (as
`CSI ? 6 c`, VT102 — ours should become `CSI ? 62 ; 22 c` in M1.4), DA2, DSR/CPR, DECRQM (2026 →
`2` "reset" = supported), kitty `CSI ? u` (only with `Config::kitty_keyboard = true`), OSC 10/11/12
and OSC 4 via `ColorRequest`, `CSI 14/18 t` via `TextAreaSizeRequest`. ptyro must answer: OSC 7501,
XTVERSION (`DCS > | ptyro <ver> ST`), `CSI 16 t`, XTGETTCAP.

### 5.7 Side channels we sniff (before/alongside the emulator)

| Sequence | Use |
|---|---|
| OSC 7501 status reports | agent status (§9.2) |
| OSC 9 / OSC 777 `notify` / OSC 99 (kitty) | notifications → inbox (§9.5) |
| BEL | attention hint (configurable) |
| OSC 0/2 title | pane title; Gemini's dynamic title encodes Ready/Action Required/Working (§8.6) |
| OSC 7 cwd | session cwd without polling |
| OSC 133 A/B/C/D (shell integration) | prompt marks → jump-to-prompt in scrollback, command exit status for shell sessions. **[built M2]** Marks are stored *inside the grid*: at 133;A the daemon splits the batch and injects an invisible OSC 8 hyperlink (`ptyro:prompt`) that it closes at B/C/D or at the next newline. alacritty keeps hyperlinks per cell through scrolling and reflow, so the mark travels with its line into history, where line numbers can't follow. Rows carry `prompt: true` in frames and scrollback documents. Caveat: a program's own hyperlink that is open at that moment ends there. `ptyro shell-init bash\|zsh\|fish` prints the snippets; they do nothing outside ptyro. |
| OSC 52 clipboard | forwarded to the controlling client if `clipboard.allow_write` (default on); writes over 1 MiB dropped by the emulator; reads always denied. The TUI writes it to its own terminal's clipboard only, never to its paste buffer (`]` pastes only your own yanks) |
| OSC 8 hyperlinks | preserved per cell; clients render as links |
| OSC 9;4 progress (ConEmu) | progress bar in pane border (Claude `terminalProgressBarEnabled`) |

### 5.8 Sizing across multiple clients **[decided]**

A PTY has exactly one size. Policy: **the controlling client wins** (tmux's `latest`). The
controller of a session is the client that most recently sent input to it or focused it. Other
viewers render the session at its true size: letterboxed (padding) if their pane is larger, cropped
with a "viewing at 120×40" badge and pan keys if smaller. A `--view-only` client never becomes the
controller.

---

## 6. Client/daemon protocol

### 6.1 Transport and framing **[decided]**

- Local: Unix stream socket at `runtime/ptyro.sock` (§14.1), `0600`, peer uid checked via
  `SO_PEERCRED` / `getpeereid` on every accept.
- Framing: `u32` big-endian length prefix + payload, max frame 16 MiB.
- Remote (later): the same payloads as WebSocket binary messages (one message = one frame); or the
  same framing tunnelled over `ssh host ptyro proxy` (stdio ⇄ socket bridge).

### 6.2 Encoding **[decided]**

- Payloads are **MessagePack with named fields** (`rmp_serde::to_vec_named`), enums internally
  tagged (`#[serde(tag = "t")]`), new fields always `#[serde(default)]`. Self-describing ⇒ additive
  evolution without breaking old clients; decodable in browsers (`@msgpack/msgpack`) and Swift/Kotlin.
- `ptyro --json-wire` (debug) switches a connection to JSON payloads. **[built M1]** No
  negotiation is needed: every message is a map, so a JSON payload starts with `{` and a
  MessagePack map never does. Each frame identifies its own encoding, and the daemon answers in
  the encoding of the client's `Hello`.

### 6.3 Handshake and versioning

```rust
// first frame from client
Hello { proto: Version { major: u16, minor: u16 }, client: ClientInfo, features: Vec<String>,
        encoding: Encoding /* msgpack | json */ }
// ClientInfo { kind: tui|cli|hook|web|mobile|proxy, version, pid, tty: Option<String>,
//              outer_term: Option<OuterTermCaps> }

// daemon reply
Welcome { proto: Version, daemon_version: String, client_id: ClientId,
          features: Vec<String>, daemon_started_at: Timestamp }
| Reject  { reason: String, daemon_version: String, upgrade_hint: Option<String> }
```

- Different `major` ⇒ `Reject`. The CLI then offers `ptyro daemon restart` (or `upgrade` once §4.5
  exists) and explains that running sessions will be hibernated/lost.
- `minor` and `features` gate optional behaviour (e.g. `"scroll-op"`, `"acp-sessions"`).

### 6.4 Message catalogue (protocol v1, **[proposed]**)

Requests carry `id: u32`; the daemon answers with `Response { id, result: Ok(..) | Err(Error) }`.
Push messages have no id.

**Client → daemon**

| Message | Purpose |
|---|---|
| `Request { id, req }` | see request list below |
| `Subscribe { session, pane_size }` | start receiving `Screen` frames for a session |
| `Unsubscribe { session }` | |
| `Ack { session, seq }` | client finished painting up to `seq` (flow control) |
| `Input { session, event }` | `Key`/`Mouse`/`Paste`/`Focus`/`RawBytes` (§5.5) |
| `Resize { session, size }` | only honoured from the controller |
| `WatchEvents { filter }` | subscribe to the domain event stream |
| `Ping` | keepalive (remote transports) |

**Requests** (each also exposed in the CLI, §12):

- Sessions: `Spawn(SpawnSpec)`, `List(filter)`, `Get(addr)`, `Rename`, `Stop{grace}`, `Kill{signal}`,
  `Restart`, `Resume`, `Hibernate`, `SendText{text, enter, bracketed}`, `ReadScreen{format: plain|ansi|cells}`,
  `ReadScrollback{range, format}`, `Wait{addr, until: StatusPredicate, timeout}`, `ClearAttention`,
  `SetTag`, `Export{asciicast|text}`.
- Workspaces: `WorkspaceCreate/List/Rename/Delete/AddProject/RemoveProject/Set` (profile, notify), `SaveView`, `LoadView`.
- Profiles: `ProfileList`, `ProfileShow` (resolved, secrets redacted), `ProfileResolve` (which profile a spawn would get, and why), `ProfileLogin`, `ProfileSync`. Profiles are defined only in user config, so there is no create/update over the socket.
- Projects: `ProjectAdd{path}`, `ProjectList`, `ProjectRemove`, `ProjectTrust{fingerprint}`,
  `ProjectConfig` (effective merged config).
- Worktrees: `WorktreeCreate(WorktreeSpec)`, `WorktreeList`, `WorktreeRemove{force}`,
  `WorktreeStatus` (dirty/ahead/behind/diffstat), `WorktreeDiff{format}`, `WorktreeOpen{editor}`,
  `RunScript{name}`.
- Orchestration: `RunCreate(RunSpec)`, `RunList`, `RunCancel`, `Broadcast{targets, text}`,
  `GateResolve{run, gate, decision}`.
- Hooks: `HookEvent{session, agent, event, payload: Value}` — from `ptyro hook` (§8.3).
- Daemon: `Status`, `Reload` (config), `Shutdown{mode}`, `Upgrade`, `Doctor`.

**Daemon → client**

| Message | Purpose |
|---|---|
| `Response { id, result }` | |
| `Screen(ScreenFrame)` | keyframe or delta for a subscribed session |
| `Event(DomainEvent)` | `SessionCreated/Updated/StatusChanged/Exited/Removed`, `AttentionChanged`, `Notification`, `WorktreeChanged`, `ProjectChanged`, `RunChanged`, `ConfigReloaded`, `ControllerChanged`, `Bell`, `Clipboard{text}` (controller only) |
| `Goodbye { reason }` | daemon shutting down / upgrading (clients reconnect) |

**[built M2] Protocol 1.1** (minor bump, additive):
- Requests: `Resolve`, `Restart`, `ReadScrollback`, `SaveView`, `LoadView`, `History`.
- Replies: `Scrollback`, `View`, `History`.
- Client message: `Focus { session }`.
- Events: `SessionRestarted` and `ConfigReloaded` (the latter goes to every client).
- `RowUpdate.prompt`.

**[built M3] Protocol 1.2** (additive):
- Requests:
  - `ProjectAdd/List/Get/Remove/Pin/Scripts/Trust{fingerprint}/Untrust` and `Discover{refresh}`;
  - `WorkspaceList/New/Set/Rename/Remove/Member{remove}`;
  - `WorktreeList/Get/New(WorktreeSpec)/Remove{opts}/Refresh/Run{script}/Lock/RenameBranch` and
    `Gc`;
  - `ProfileList/Env/Rename/Resolve(SpawnSpec)`;
  - `ReloadConfig`, which the CLI sends after it edits the config file.
- Replies:
  - `Project{notes}`, `Projects`, `Scripts`, `Paths`, `Workspace(s)`, `Worktree(s)`;
  - `WorktreeCreated{setup, agent, agent_pending, untrusted, notes}`, `Removed{notes}`, `Gc`;
  - `Profiles`, `Env`, `Resolved{profile, reason, question}`.
- Events go to every `WatchEvents` client: `ProjectUpdated/Removed`, `WorkspaceUpdated/Removed`,
  `WorktreeUpdated/Removed` (including live repository status and the lock flag) and
  `WorktreeProgress{step}`.
- `SpawnSpec` gains `agent`, `worktree`, `profile`, `workspace`, `override_pin` and
  `confirmed`.
- `SessionInfo` gains `profile`, `worktree`, `project`, `workspace`, `agent`, `script` and
  `ports`.
- New error code `needs_confirmation` (exit code 5): the request would cross a profile guard or a
  pin, and the client resends it with consent (the CLI asks on a TTY; the TUI shows a dialog).

**[built M4] Protocol 1.3** (additive):
- Requests:
  - from the relays: `Hook{adapter, event, payload}`, `StatusLine{session, payload}` and
    `Report{session, state, kind, msg, progress}` (`ptyro report`);
  - `StatusExplain`, `Resume`, `Hibernate`, `ClearAttention`, `Answer{decision}`;
  - `Inbox{workspace, unread}`, `InboxMark`, `Mute{target, until}`, `Usage`, `NotifyTest`.
- Replies: `Explain{lines: [ExplainLine{at_ms, source, signal, applied, reason}]}`, `Inbox`,
  `Usage`, `Notified{channels}`.
- Events: `InboxAdded`, `Notify{title, body}` (the TUI writes OSC 777/9 to its own terminal) and
  `UsageUpdated`. Status changes ride on `SessionUpdated`.
- `ClientMsg::Focus` gains `terminal_focused` (serde default `true`, so 1.2 clients still
  work): a session counts as *watched* only in the focused pane of a client whose terminal has
  focus (mode 1004).
- `SessionInfo` gains `status` (a `StatusRecord`), `attention`, `activity`, `conversation`,
  `children` and `dangerous`. `ProcessState` gains `Dormant`. `SpawnSpec` and `WorktreeSpec` gain
  `prompt`.

**[built M5] Protocol 1.4** (additive):
- `SendText` gains `wait_ready_ms`: the daemon waits (up to that long) until the session is
  ready for input, using the same readiness test as prompt delivery (§8.2), then types and
  replies; `timeout` otherwise.
- New requests and replies:
  - `Events{session, limit}` → `Events{events: [EventRecord{session, at_ms, kind, detail}]}`
    (persisted `session_event`s, oldest first);
  - `Doctor` → `Doctor{report: DaemonDoctor{leaked_vars, path, warnings, config_error}}`;
  - `SelfTest{adapter}` → `SelfTest{result: SelfTestResult{adapter, ok, seen, detail}}`.
- A new event, `EventsLost{count}`, goes only to a client whose event queue overflowed. The TUI
  re-reads the session list on it, and `events --follow` passes it on.
- `SessionInfo` gains `prompt_pending`, `last_prompt_ms`, `turns` and `last_output_ms`, which
  `wait` needs (§12.2).

**[built M6] Protocol 1.5** (additive):
- **Requests:**
  - `WorktreeDiff{against: base|upstream|last_turn|head}` → `Diff{doc: DiffDoc}`;
  - `WorktreeLog` → `Commits`;
  - `WorktreeCommit{message, paths, all}` → `Committed{sha}`;
  - `WorktreeIntegrate{into, cherry_pick, push}` and `WorktreePair{off, refresh}` → `Notes`;
  - `PrGet{refresh}`, `PrCreate{draft, title, body, base}` and `PrAutomerge{method, off}` →
    `Pr{info}`;
  - `PrFix`, `PrWatch{off}`, `PrReply{comment, text}`;
  - `ReviewAdd{path, line, code, text}`, `ReviewList`, `ReviewRemove{id}` → `Comments`;
  - `ReviewSend{session, note}` → `Session`;
  - `ReviewQueue` → `ReviewQueue{items}`;
  - `ReviewDone`.
- **`Spawn`** of an agent can answer `Queued{position, unreviewed}` instead of `Session`
  (`review.max_unreviewed`). `SpawnSpec.unqueued` skips that gate.
- **Model changes:** `WorktreeInfo` gains `pr: PrInfo{number, url, title, state, draft, review,
  checks: [CheckRun{name, bucket, link, workflow}], automerge, ours, at_ms}`. `WorktreeSpec`
  gains `issue`.

**[built M7] Protocol 1.6** (additive; the wire types live in `ptyro-run::wire`):
- **Runs:** `RunList{active, limit}` → `Runs`; `RunGet`, `RunCancel`, `RunRetry{step}`,
  `RunFork{from, set}` → `Run{run: RunInfo}`; `RunEvents` → `RunEvents{events: [(seq, at, event)]}`.
- **Recipes:** `RecipeList{project}` → `Recipes`; `RecipeShow{name, project}` → `Recipe{info, src}`;
  `RecipeRun{name|src, vars, project, base, trust}` → `Run`. `Fanout{spec: FanoutSpec}` → `Run`.
- **Gates:** `GateList{all}` and `GateResolve{id, choice, note, key, by}` → `Gates`.
- **Compare:** `Compare{run, refresh}` → `Compare{doc: CompareDoc}`; `Pick{run, candidate, mode,
  into, paths, hunks}`, `Discard{run, candidate, force}` and `Iterate{run, candidate, text}` →
  `Notes`.
- **Mailbox:** `MsgPost{from, to, kind, body, run, in_reply_to, priority, ttl_ms, key}` →
  `Posted{ids, to}`; `MsgCheck{me, since, wait_ms}` and `MsgList{run, limit}` → `Messages`;
  `MsgAck{me, ids}`.
- **Leases, radar, checkpoints:** `LeaseClaim{holder, patterns, exclusive, ttl_ms, reason}`,
  `LeaseRelease{holder, patterns}` and `LeaseList` → `Leases{leases, refused}`;
  `Radar{project}` → `Radar`; `CheckpointList{addr}` → `Checkpoints`; `CheckpointDiff{addr, n,
  to}` → `Diff`; `CheckpointRestore{addr, n, turn}` → `Notes`.
- **Agents:** `SetVerify{addr, check, max_bounces}` → `Session`; `LeadStart{addr, policy}` → `Run`;
  `Lead{lead, op: LeadOp}` → `LeadResult{value}` (the `ptyro mcp` surface, §15.6).
- **Hooks** may now be answered `HookOutput{stdout}`: the relay prints it for the agent (§8.3).
- **Event:** `RunUpdated{run}` to every watcher.
- **Model changes:** `SessionInfo` gains `verify` (`{check, max_bounces, bounces, state, last:
  Evidence}`) and `run` (`r-7f3a2c/impl`), both omitted when unset. `SpawnSpec` and
  `WorktreeSpec` gain `verify` and `lead`. `AttentionKind` gains `gate` and `run`.
- **Who asks:** the daemon takes the connecting process's pid from the socket's peer credentials
  and walks its ancestry to the session it runs in. Policy and "agents don't answer gates"
  (§15.6) are decided on that, not on anything the client says.

**[built M8] Protocol 1.7** (additive):
- **Upgrade:** `Upgrade{binary}` → `Upgrading{from, to, binary, sessions}` (§4.5); agents can't
  send it.
- **Status:** `DaemonStatus` gains `generation`, `upgraded_ms`, `rolled_back` and `metrics:
  Metrics{rss_kib, open_fds, tasks_alive, loop_lag_ms_max, loop_lag_ms_p99, pty_readers,
  bytes_read, frames_sent, crashes, scrollback_bytes, sessions: [(name, bytes)]}` (§18.1).
- **History:** `ReadScrollback{lines: Some(n)}` may reach past memory into the recording
  (§7.6).
- **zstd frames (M8.7):**
  - A client that lists the feature `"zstd"` in `Hello` (the CLI and TUI do with
    `PTYRO_COMPRESS=1`) gets it back in `Welcome`, and from then on the daemon may send frames
    of 512 bytes or more as a zstd frame instead of MessagePack.
  - The two are told apart by the first four bytes (zstd's magic `28 b5 2f fd` can't start a
    MessagePack map), so a decoder takes either, always.
  - Decompression is capped at the 16 MiB frame limit.
  - It's off by default: on a local socket it costs a little CPU and saves nothing. It exists
    for M9's remote links. `ptyro-latency --compress` measures about the same latency.

**[built M9/M10] Protocol 1.8** (additive):
- **Hello:** `ClientInfo` gains `token` (a paired device's) and `via` (`ssh://host`, or
  `ws:<peer>` set by `ptyro serve`). `web`/`mobile` clients, or any client with a token, must
  present a valid token or are rejected before `Welcome`.
- **Devices:** `DevicePair{name, scope}` → `Paired{device, token}` (from this machine only);
  `DeviceList` → `Devices`; `DeviceRevoke{id}`.
- **Push:** `PushKey` → `PushKey{key}` (VAPID public key); `PushSubscribe{sub}`;
  `PushUnsubscribe{endpoint}`; `PushTest` → `Notes`.
- **Structured sessions:** `Convert{addr, structured, force}` → `Session`;
  `ChatGet{addr}` → `Transcript{items}`; `ServerMsg::Chat{session, rev, reset, items, draft}`.
  `SessionInfo` gains `structured` (the protocol) and `held` (a permission prompt held for an
  exact answer). `SpawnSpec`, `WorktreeSpec`, `FanoutSpec` and recipe `spawn` gain
  `structured`. Compare `Candidate` gains `tools`.
- **Errors** keep their shape: `{t: "err", error: {code, message}}` (and so does `ptyro serve`'s
  `POST /api/request`).

**[built M11] Protocol 1.9** (additive):
- **Change sets:** `Cs{op}` → `Cs{value}` (JSON). `op` is one of `new{spec}`, `list`,
  `show{set}`, `log{set, since}`, `whoami`, `msg{set, to, text, kind, interrupt}`,
  `ready{set, member, note}`, `blocked{set, on, member, note}`, `assign{set, member, task}`,
  `contract{set, action}` (`show`, `history`, `diff`, `submit`, `revise`), `diff{set, member}`,
  `check`, `checkpoint`, `restore`, `overlay`, `commit`, `push`, `pr`, `land`, `add`, `drop`,
  `rm`, `gate{set, question, options}` and `templates`. Values are `ChangeSetInfo`,
  `[LedgerEntry]`, `[TemplateInfo]` or small objects (§15.7, `docs/cli-json.md`).
- **Runs:** `RunKind::Changeset`; `RunState` gains `changeset`; `RunEvent` gains `lead_set` and
  `cs` (a `CsEvent`, tagged `c`).
- `SpawnSpec` gains `mcp` (give the agent `ptyro mcp`); `WorktreeSpec` gains `path` (put the
  worktree there instead of `worktrees.location`).

**[built] Protocol 1.10** (security review, M2; *not* additive for pairing):
- **Pairing:** `DevicePair{name, scope}` now answers `Pairing{device, code, expires_ms}`, a
  single-use code (`ptp_…`) that works for 10 minutes, instead of the token.
  `DeviceClaim{code, via}` trades it for `Paired{device, token}`, once; `ptyro serve` sends it
  for `POST /api/pair`. Devices can't send either. `DeviceInfo` gains `pending` (not claimed
  yet).

### 6.5 Screen frames

```rust
struct ScreenFrame {
    session: SessionId,
    seq: u64,                    // this frame brings the client up to `seq`
    keyframe: bool,              // true: client must discard its cache first
    size: Option<TermSize>,      // cols, rows (present on keyframe or change)
    styles: Vec<StyleDef>,       // new style-table entries for this connection
    rows: Vec<RowUpdate>,        // dirty rows only (all rows on keyframe)
    cursor: Option<CursorState>,
    modes: Option<ClientVisibleModes>, // mouse mode on/off, alt screen, bracketed paste (for client UX)
    title: Option<String>,
    scrollback: Option<ScrollbackInfo>, // len, epoch
}
struct RowUpdate { y: u16, runs: Vec<Run>, wrapped: bool }
struct Run { style: u32, text: String, wide: bool,   // `wide`: every cell in the run is 2 columns
             clusters: Option<Vec<u8>> }           // chars per cell, only when a cell has combining chars
struct StyleDef { id: u32, fg: Color, bg: Color, ul: Option<Color>,
                  flags: StyleFlags /* bold dim italic underline(kind) blink inverse hidden strike */,
                  link: Option<u32> /* hyperlink table id */ }
```

- **Style table** is per subscription, append-only, cleared on keyframe; id 0 is always the default
  style and is never sent; typical screens use < 50 styles. Trailing default blanks are not sent;
  the client pads. The client never measures character widths itself (`wide` + `clusters`), so it
  can't drift from the daemon's emulator.
- **Pacing:** the hub sends at most one frame per session per client per tick (default 8 ms,
  configurable), and holds at most **one unacked frame** per session per client. A slow client
  simply receives fewer, larger deltas — never an unbounded queue.
- **Synchronized output:** while the child has an open DEC 2026 batch, frames are held (bounded by
  the processor's timeout) so clients never paint half-drawn Claude/Neovim screens.
- Optional `zstd` compression for frames > 8 KiB on remote transports (feature `"zstd"`).

### 6.6 Errors

`Error { code: ErrorCode, message: String, details: Value }` with stable codes:
`not_found`, `ambiguous`, `invalid`, `conflict`, `dirty_worktree`, `untrusted_project`,
`permission_denied`, `unsupported`, `timeout`, `busy`, `internal`. CLI exit codes map from these
(§12.4).

---

## 7. Sessions: lifecycle, persistence, hibernation, resume

### 7.1 Session kinds

| Kind | Description |
|---|---|
| `agent` | Adapter-backed coding agent in interactive (PTY) mode. The default. |
| `shell` | `$SHELL -l` (or configured) in a worktree/cwd. |
| `command` | Arbitrary command (`nvim`, `lazygit`, `htop`, a dev server). |
| `script` | ptyro-managed lifecycle script (setup/run/archive) — read-only by default. |
| `structured` | **[built M10]** an agent driven over its structured protocol (stream-json / app-server / ACP / OpenCode's server); rendered as a chat view. On the wire it is an `agent` session with `structured: <protocol>`. |

**[built M10] Structured sessions.**
- **Starting one.** `new --agent A --structured`, `fanout --structured`, a recipe step's
  `spawn = { …, structured = true }`, or `WorktreeSpec.structured`. The protocol comes from
  `[agents.A] protocol`, else from the adapter:

  | Adapter | Protocol | Launch |
  |---|---|---|
  | claude | stream-json | `-p --input-format/--output-format stream-json --verbose --include-partial-messages --permission-prompt-tool stdio` |
  | codex | app-server | `codex app-server` |
  | gemini | acp | `gemini --acp` |
  | opencode | opencode | `opencode serve --port P --hostname 127.0.0.1` |

  Other adapters have none, unless the agent sets `protocol = "acp"`. `--structured` needs an
  agent.
- **The process.** The agent runs with pipes, in its own process group, with the profile's
  environment and no hook integration.
- **The driver.** The `ptyro-drive` crate is pure protocol logic: lines in, effects out. The
  daemon's `structured.rs` does the I/O:
  - transcript items (`ptyro_core::chat`): user, assistant (streamed in place), thinking, tool
    (status, output, diff, files), permission (options, decision), plan, notice, turn end (stop
    reason, cost in micro-dollars, duration, tokens);
  - status signals from source `protocol` (high);
  - lines to the agent's stdin, or HTTP to OpenCode (whose `GET /event` stream is followed).
- **The terminal view.** The transcript is also rendered, append-only, into the session's
  emulator. `read`, `wait --match`, the recorder, `--scrollback` and the web canvas therefore
  all work.
- **Input.** Keys typed into the pane go through a line editor in the daemon: Enter sends,
  Shift/Alt+Enter is a newline, `a`/`A`/`d` (and `1`/`2`/`3`, Enter, Esc) answer an open
  permission request while the draft is empty, and Ctrl-C clears the draft or interrupts the
  turn. `SendText` prompts directly; `answer` answers through the protocol. An answer never
  widens itself: "allow" is one-time only (ACP: an `allow_once` option; if the agent offers
  none, the request stays open with a notice asking for an explicit "always" or a deny), while
  "always" may fall back to one-time and "deny" to a standing reject.
- **Readiness** comes only from the protocol's status.
- **Stopping** interrupts a running turn, waits 3 s, then stops the process group.
- **Clients.** A subscription to a structured session also streams `ServerMsg::Chat` (all items
  on subscribe, then what changed, coalesced at 30 ms, plus the draft). `ChatGet` returns the
  whole transcript (`read --chat`).
- **Converting** (`Convert`, `ptyro convert S --to tui|structured`) needs the conversation id.
  It refuses mid-turn without `force`, stops the agent as hibernation does, then resumes the
  same conversation in the other mode: Claude `--resume`, Codex `thread/resume` / `codex
  resume`, ACP `session/load`, OpenCode the same `ses_` id. The protocol is kept in `Meta` and
  the stored launch, so resume, restart and dormant restore stay structured.
- **Runs.** Steps record `tools` (a tool summary) and the last assistant message in their
  outputs. Compare candidates carry `tools: {calls, by_tool, failed, files, commands,
  cost_micro_usd}`. Verified-done uses follow-ups (no Stop hook).
- **Live upgrade.** The driver parks between lines like a PTY reader. Its state (serde), unread
  bytes, transcript, draft and rendering state, plus the stdin/stdout/stderr fds, cross the exec.
- **Not yet:** file leases and recipe guards are enforced through PreToolUse hooks, which
  structured sessions don't install. After a full daemon restart (not an upgrade) a dormant
  structured session's transcript is gone; its recording and conversation remain.

**[built M3]** `agent` sessions start `[agents.<name>] command` (a string or argv) plus `args`,
else the built-in CLI of that name (`claude`, `codex`, `gemini`, `opencode`). `script` sessions
are read-only: the daemon drops input to them. They stop with SIGTERM, then SIGKILL after 5 s.

### 7.2 SpawnSpec

```rust
struct SpawnSpec {
    kind: SessionKind,
    agent: Option<String>,            // adapter id or user-defined agent name
    target: SpawnTarget,              // Worktree(addr) | NewWorktree(WorktreeSpec) | ProjectRoot(p) | Cwd(path)
    name: Option<String>,
    command: Option<Vec<String>>,     // for kind=command; agents use config
    args: Vec<String>,                // extra args appended to the agent's configured args
    env: BTreeMap<String, String>,
    profile: Option<String>,          // None → resolution order of §10.10
    workspace: Option<WorkspaceAddr>, // home workspace (§9.6)
    prompt: Option<String>,           // initial prompt (delivered per §8.4)
    size: Option<TermSize>,           // default: requesting client's pane size, else 120x40
    tags: Vec<String>,
    run: Option<RunId>,
    attach: bool,                     // TUI convenience
}
```

### 7.3 Process lifecycle state machine **[decided]**

```text
          spawn ok                      child exits
 Starting ─────────► Running ────────────────────────► Exited{code|signal}
    │                  │  ▲                                  │
    │ spawn fails      │  │ resume (adapter supports it)     │ resume
    ▼                  │  └──────────────── Dormant ◄────────┘ (if conversation id known)
  Failed               │  hibernate / daemon stop --hibernate / reboot
                       └────────────────────────► Dormant
```

- `Running` sessions additionally carry an **agent status** (§9).
- `Exited` sessions stay visible (greyed, with exit code and final screen) until removed; their
  recorder log remains readable (`ptyro read --scrollback`).
- `Dormant` = process not running but resumable; the last screen is shown dimmed with a
  "⏎ resume" hint.

### 7.4 Hibernation **[proposed]**

Long-idle agents cost memory and sometimes subscription slots. Optional per agent
(`hibernate_after = "2h"`, default off):

1. Only when status is `idle` (turn finished, no pending permission) and the session is not visible
   on any client and has no running tool subprocesses in its process group.
2. Adapter-specific graceful stop (Claude: `SIGINT` then wait for exit; Codex: send `/quit`?
   **[spike]**; generic: `SIGHUP`).
3. State → `Dormant`. Focusing it (or `ptyro resume`) relaunches with the adapter's resume command
   and the **same ptyro session id, name, worktree and conversation id**.

**[built M4]**
- `[agents.<name>] hibernate_after = "2h"`. A tick every 2 s hibernates an agent that has been
  `Idle` that long, has no viewers, has a known conversation id, and whose PTY's foreground
  process is still the agent itself (so a running tool or a shell escape keeps it alive).
- Stops follow the adapter's `stop()` steps (§8.1). Claude: Esc if working, then Ctrl-U `/exit`
  and wait 5 s. Codex: Esc, then `/quit`. Everything else: SIGHUP. Then the process group is
  killed.
- `ptyro hibernate S…` and the palette's `session.hibernate` do the same on demand.
  `ptyro daemon stop` hibernates every resumable agent first, so a restart or upgrade loses no
  conversation.
- Resume (`ptyro resume`, `⏎` on a dormant pane, or focusing it while
  `attention.resume_on_focus` is on) relaunches through the adapter's `resume()` with the same
  session id, name, place, profile and integrations. A profile that has since been deleted is
  refused, because its conversation lives in that account.

### 7.5 Reconciliation (daemon start) and reboot survival

- On start, every session that was `Starting`/`Running` in the DB but has no live process (daemon
  crashed or machine rebooted) becomes:
  - `Dormant` if its adapter can resume and a conversation id is known;
  - `Exited{lost}` otherwise.
- `restore_on_start = "dormant" | "resume" | "none"` (default `dormant`): with `resume`, ptyro
  relaunches all resumable sessions automatically (staggered, max 3 concurrent).
- Worktrees are reconciled against `git worktree list --porcelain`: missing directories are marked
  `missing` (never auto-pruned); unknown ptyro-marked worktrees are re-adopted.

**[built M4]** `restore_on_start` is implemented as specified. At start, `close_orphans` turns
orphaned agent sessions whose adapter can resume and whose conversation id is known into
`Dormant` (others become `Exited{lost}`). Dormant sessions come back in the session list with
their name (deduplicated), place and conversation; `resume` relaunches them three at a time.
Without a recorder (M8) a dormant pane shows a "☾ dormant: ⏎ resumes" card, not the last screen.
**[built M8]** With one, the dormant pane shows the agent's last screen, dimmed, with the "⏎ resume" bar.

### 7.6 Recorder **[built M8]**

- Raw PTY output is appended to `data/sessions/<id>/pty.log` in a simple framed format
  (`[u32 len][u64 µs-since-start][bytes]`), ring-truncated at `recorder.max_bytes` (default 16 MiB).
- Uses: (1) scrollback for `Exited`/`Dormant` sessions after daemon restart; (2) `ptyro replay`
  (time-faithful playback in a pane); (3) `ptyro export --asciicast` (asciinema v2); (4) debugging
  emulator bugs from real-world byte streams (attach the log to an issue).
- Per-agent opt-out (`record = false`) and global `recorder.enabled`. Logs may contain secrets that
  scrolled through the terminal: files are `0600`, and `ptyro gc` prunes logs of removed sessions.

**[built M8]** The format grew resize and keyframe records:
- **Header and records.** The file starts `PTYROREC\x01`. Each record is
  `[kind u8][len u32 BE][at_µs u64 BE][payload]`. Kinds:
  - 0: output bytes;
  - 1: resize (`cols`, `rows` as u16);
  - 2: keyframe, the whole screen as escape sequences;
  - 3: start (`cols`, `rows`, unix ms).
- **The ring.** It's `pty.log` plus `pty.log.1`, each up to `max_bytes / 2`. Every file starts
  with a start record and a keyframe, so either half replays on its own. Files are `0600` and
  directories `0700`. Readers skip a torn last record. The `record` fuzz target checks they
  never panic.
- **Uses.**
  - `read --scrollback N` past the in-memory history replays the recording into a scratch
    emulator with N lines of scrollback (`ReadScrollback{lines: Some(N)}`). In copy mode, `gg`
    at the top asks for 1M lines and extends the document in place.
  - A dormant agent restored at daemon start shows its last screen, rebuilt from its
    recording.
  - `ptyro replay S [--speed X] [--max-idle D] [--from D]` plays the recording in your
    terminal, idle gaps capped.
  - `ptyro export S --format asciicast|text|ptylog [-o F]` writes asciicast v2 (with resize
    events as `"r"`), the final text of every line, or the raw bytes as a `.ptylog` fixture.
- **Memory.** The in-memory scrollback default dropped to **5000 lines**: a full 10k-line,
  80-column session measured ≈ 19.5 MB RSS, over §18.3's 15 MB, and the recording now serves
  everything older.
- **Pruning.** At daemon start, the recordings of sessions the store no longer knows (removed
  with `rm`, or aged out of the history) are deleted.

---

## 8. Agent adapters

### 8.1 Adapter trait (`ptyro-agents`) **[proposed]**

```rust
#[async_trait]
pub trait Adapter: Send + Sync {
    fn id(&self) -> &'static str;                          // "claude"
    fn capabilities(&self) -> Capabilities;
    fn detect(&self) -> Detection;                         // installed? version? auth hints?
    /// Build the launch: command, args, env, integration files to materialize.
    fn launch(&self, ctx: &LaunchCtx) -> Result<LaunchPlan>;
    /// Build a resume launch for a dormant session.
    fn resume(&self, ctx: &LaunchCtx, conversation: &str) -> Result<LaunchPlan>;
    /// Translate a hook payload into normalized signals.
    fn on_hook(&self, event: &str, payload: &serde_json::Value) -> Vec<Signal>;
    /// Translate side-channel sequences (OSC 7501, titles, OSC 9) into signals.
    fn on_side_channel(&self, ev: &SideChannelEvent) -> Vec<Signal>;
    /// How to deliver a prompt into the running TUI.
    fn prompt_delivery(&self) -> PromptDelivery;            // BracketedPasteThenEnter | Keys | Arg
    fn graceful_stop(&self) -> StopPlan;
}

bitflags! { pub struct Capabilities: u32 {
    const PRESET_CONVERSATION_ID = 1;  // we choose the id at launch
    const LEARN_CONVERSATION_ID  = 1<<1;  // we learn it from hooks/events
    const RESUME                 = 1<<2;
    const HOOKS                  = 1<<3;
    const OSC_STATUS             = 1<<4;  // OSC 7501
    const TITLE_STATUS           = 1<<5;  // status encoded in OSC 0/2
    const PERMISSION_SIGNAL      = 1<<6;  // reliable "blocked on permission"
    const TURN_END_SIGNAL        = 1<<7;
    const ERROR_SIGNAL           = 1<<8;
    const STRUCTURED_MODE        = 1<<9;  // headless/ACP/app-server
    const REMOTE_APPROVAL        = 1<<10; // hook can return a decision (§16.6)
}}
```

Integration files (hook configs, plugin dirs) are materialized under
`data/integrations/<adapter>/<hash>/` and **never** written into the user's global agent config or
into the worktree, unless the user explicitly opts in. **[decided]**

**[built M4]** The trait is synchronous and pure: the daemon does all the I/O. `launch`/`resume`
return a `LaunchPlan { argv, env, files, conversation, sidecar }`. `files` are integration files
written under `data/integrations/<agent>/<content hash>/`, so identical content is shared and
never rewritten. `sidecar` is a side channel the daemon runs (OpenCode's server).
- Then `on_hook`, `on_title`, `on_screen` (dialogs that appear before any hook; medium
  confidence, consulted only while no hook or 7501 has spoken) and `ready(screen)` (the input
  box is showing). These are followed by `answer(Decision) → keys`, `stop() → [StopStep]` and
  `done_means_idle`.
- `[agents.<name>] adapter = "…"` picks the adapter (default: the name, if it's a known one,
  else `generic`), so `claude-work` or a wrapper script still gets Claude's integration.
- Built-ins: `claude`, `codex`, `gemini`, `opencode`, `antigravity` (CLI `agy`), `generic`; plus
  the `shell` session kind.
- Args that skip permission prompts (`--dangerously-skip-permissions`, `--yolo`,
  `--dangerously-bypass-approvals-and-sandbox`, …) are refused unless the agent sets
  `dangerous = true`. Those sessions carry a ⚠ badge everywhere. The check
  (`config::dangerous_arg`) covers the flags `--dangerously-skip-permissions`,
  `--allow-dangerously-skip-permissions`, `--dangerously-bypass-approvals-and-sandbox`,
  `--full-auto`, `--yolo`, `-y`, and the values `--permission-mode bypassPermissions`,
  `--approval-mode yolo`, `--sandbox`/`-s danger-full-access`, `--ask-for-approval`/`-a never`,
  `--config`/`-c approval_policy=never|sandbox_mode=danger-full-access`. Values are matched as
  the next argument, `--flag=value` or `-svalue`, ignoring case and quotes. A one-string
  `command` is checked word by word. It runs on the config and on every launch's full argv.

### 8.2 Claude Code (primary) **[spike-validated items marked]**

Verified against Claude Code **2.1.304** (§20).

- **Launch:** `claude --session-id <uuid> -n <ptyro-name> [configured args] [--settings <json> | --plugin-dir <dir>]`.
  - `--session-id` lets ptyro **pre-assign** the conversation id (a UUIDv4 generated per session).
  - `-n/--name` sets the display name (prompt box, `/resume` picker, terminal title).
- **Hooks injection** **[decided 2026-10-20, verified M0.3]**: `--plugin-dir
  data/integrations/claude/<hash>/` containing `.claude-plugin/plugin.json` and `hooks/hooks.json`.
  In one run, hooks from `--plugin-dir`, from inline `--settings '<json>'` *and* from the project's
  `.claude/settings.json` **all fired** for every event, so both mechanisms add to the user's hooks.
  The plugin wins on tidiness: a stable directory instead of a large argv, and nothing that looks
  like a settings override. `--settings` is the fallback.
- **Hooked events** → `ptyro hook claude <Event>` (command hook, async where allowed):
  `SessionStart`, `UserPromptSubmit`, `PreToolUse` (sampled, for "current tool" display),
  `PermissionRequest`, `Notification`, `Stop`, `StopFailure`, `SubagentStart/Stop`, `SessionEnd`,
  `PreCompact`, `CwdChanged`, `WorktreeCreate`/`WorktreeRemove` (observe only).
  **[verified M0.3]** observed semantics (fixtures in `crates/ptyro-agents/tests/fixtures/claude-2.1.304/hooks/`):
  - `SessionStart.source` = `startup` / `resume`; `SessionEnd.reason` = `prompt_input_exit` on `/exit`.
  - `PermissionRequest` fires ~0.1 s after `PreToolUse` and is the authoritative *blocked* signal.
  - `Notification(permission_prompt)` fires **only when the terminal is unfocused** (`CSI O` sent
    via mode 1004), ~6 s after the prompt appears. It is never the primary signal.
  - `Stop` fires on a completed turn but **not** when the user interrupts or denies (Esc). Only
    OSC 7501 reports the return to `idle` there, so the status engine must not wait for `Stop`
    (§9.2).
  - Every payload carries `session_id` = the pre-assigned `--session-id`, plus `prompt_id`,
    `permission_mode` (`--permission-mode manual` reports `default`), `effort`, and `scratchpad_dir`.
- **OSC 7501**: Claude probes on start; ptyro replies (§5.6) and then receives
  `state=working|blocked|done|idle|error` with optional `kind=permission|question|auth`,
  `progress`, `title` (base64), `msg` (base64). Lowest-latency signal; hooks remain authoritative.
  Disabled if `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` or `--bg`.
  **[verified M0.6, 2.1.304]** observed lifecycle, always with `app=claude-code`:
  - `idle` once the prompt is ready (~0.6 s after spawn, or after the trust dialog is accepted);
  - `working` ~0.1–0.2 s after the prompt is submitted, repeated with `msg` = the activity
    ("Listing files in current directory", "Creating empty marker file");
  - `blocked` `kind=permission` `msg="approve Bash: touch spike-marker.txt"`;
  - `done` at the end of a completed turn (Claude stays in `done` until the next input, so ptyro
    maps it to `Idle` + attention);
  - `idle` after an interrupt or denial;
  - three `state=clear` reports on `/exit`.
  - 7501 arrives 50–80 ms *before* the equivalent hook (hook process spawn overhead) and at the
    same moment as `Stop`.
- **Env:** `CLAUDE_CODE_FORCE_SYNC_OUTPUT=1` (we support DEC 2026), `FORCE_HYPERLINK=1` (we support
  OSC 8). Never set `TMUX`. Do not set telemetry vars on the user's behalf (their choice; documented).
- **Resume:** `claude --resume <uuid> -n <name> …` — **must re-pass** `--settings`/`--plugin-dir`,
  `--add-dir`, `--mcp-config` (not restored by Claude). **[verified M0.3]** resuming a
  pre-assigned id restores the conversation on screen, fires `SessionStart source=resume` with the
  same `session_id`, and shows the `-n` name on the prompt-box border. On exit Claude prints
  `Resume this session with: claude --resume <uuid>`, a last-resort way to learn the id.
- **Transcript:** `$CLAUDE_CONFIG_DIR/projects/<cwd with non-alphanumerics → '-'>/<uuid>.jsonl` (default `~/.claude`) (format is
  internal; ptyro only reads it opportunistically for "last message" previews, never depends on it).
- **Prompt delivery:** bracketed paste of the prompt text, then `\r`, only once status is `idle`
  (after `SessionStart` + first OSC 7501 `idle`, or 1.5 s quiet as fallback).
- **Graceful stop:** if `working`, `SIGINT` (ends the turn), then `/exit` + `\r` once idle,
  then process-group SIGHUP after timeout.
- **Accounts:** via the session's profile (§10.10), which sets `CLAUDE_CONFIG_DIR`. ptyro's hook
  injection is per-launch (`--plugin-dir`/`--settings`), so it works with any config dir.
- **Remote approval** (later): `PermissionRequest` hook can *return* an allow/deny decision; ptyro's
  hook relay may block (bounded) waiting for a decision from a phone/web client (§16.6).

**[built M4, verified against 2.1.304 with `make verify-claude`]**
- **Launch:** `claude --session-id <uuid> -n <name> --plugin-dir <integration dir> --settings
  '{"statusLine":{"type":"command","command":"<ptyro> statusline-relay"}}' [args]`. The plugin
  hooks `SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `Notification`,
  `Stop`, `StopFailure`, `SubagentStart/Stop` and `SessionEnd`.
- **Hook mapping:**
  - `UserPromptSubmit` → working;
  - `PreToolUse` → working with the tool summary (`Bash: cargo test`);
  - `PermissionRequest` → blocked(permission) with the tool;
  - `Stop` → idle;
  - `StopFailure` → errored with its kind, plus a profile-wide "rate-limited until" for
    `rate_limit`;
  - `SubagentStart/Stop` → child rows;
  - `Notification` → a status only for `permission_prompt` (blocked, *no detail*: the message is
    the generic "Claude needs your permission", which would overwrite the real tool) and
    `idle_prompt`. Other notifications go to the inbox.
- **Statusline relay:** `ptyro statusline-relay` forwards Claude's statusline JSON to the daemon
  (usage meters, §9.7) and runs the user's own `statusLine` command from the profile's
  `settings.json`, printing its output, so the user's statusline is unchanged.
- **Prompt delivery (changed from the proposal):**
  - Wait for a reliable `Idle` (hook or 7501), or for `ready(screen)` (the `❯` input box with
    no dialog on screen). Never type while `Blocked`; give up after 120 s with a "couldn't
    deliver your prompt" attention.
  - Then paste the text, wait 60 ms, and send Enter as an **encoded key event**. Claude speaks
    the kitty keyboard protocol and ignores a bare `\r` written straight after the text: a burst
    ending in `\r` reads as pasted (§20.1).
- **Folder trust:** `on_screen` recognises the trust dialog and reports blocked(question)
  "trust this folder?". `ready()` is false while it shows, so ptyro never types into it.
- **Answers** (`ptyro answer`, peek `y/a/n`, approve links): `1` (yes), `2` (yes, and don't ask
  again for this), Esc (no). On the folder-trust question the keys are Down, Enter (yes) and Esc
  (no): the adapter picks them from what is on screen (`answer_screen`, M11.0).
- **Roster cross-check:** every 15 s, for Claude sessions whose hooks have been quiet for a
  while, `claude agents --json` (per profile) gives a medium-confidence status by `sessionId`.

### 8.3 The hook relay: `ptyro hook <adapter> <event>` **[decided]**

- Reads the JSON payload on stdin, reads `PTYRO_SESSION_ID` and `PTYRO_SOCKET` from env (inherited
  through the agent from its PTY environment), sends `HookEvent` over the socket, exits.
- **Must never hurt the agent:** total budget 500 ms (except remote-approval mode); on any error
  (daemon down, unknown session) it exits `0` silently with no stdout. Unknown hook events are
  forwarded verbatim (the adapter decides).
- Written to be tiny and fast: no tokio runtime (blocking `UnixStream`), no config loading.
- If `PTYRO_SESSION_ID` is missing (the user ran the agent outside ptyro but our integration leaked
  into its config), it is a no-op.

**[built M4]** `ptyro hook <adapter> <event> [args…]` (hidden), blocking `UnixStream`, 500 ms
budget, always exit 0 with no stdout. The payload is stdin, or the last argument for Codex's
`notify`. `--chain <argv…>` runs the user's original notify program with the same payload, after
the relay and never blocked by it.

**[built M7]** The daemon can answer a hook for the agent (`HookOutput`), and the relay prints
that on stdout (nothing otherwise):
- **Stop** (Claude) with verified-done: `{"decision":"block","reason":…}` until the check passes
  (§15.6). A Stop waits up to 900 s (the check runs first); the Claude plugin's Stop hook timeout
  is 900 s to match, every other hook keeps 5 s.
- **PreToolUse / BeforeTool**: a denial when a file is leased by another session or the step's
  guard forbids the path or command. Claude (and Codex, which follows Claude's format):
  `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":…}}`;
  Gemini/Antigravity: `{"decision":"deny","reason":…}`.

The relay also forwards `CLAUDE_CODE_MESSAGING_TOKEN` with the socket path.

### 8.4 Codex CLI **[verified 0.143.0 where noted; several items spike]**

- **Launch:** `codex [configured args] -c 'notify=["ptyro","hook","codex","notify"]' …`.
  If the user already has a `notify` program, ptyro chains it: `ptyro hook codex notify --chain <orig…>`.
- **Hooks:** Codex has `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `PreToolUse`,
  `PermissionRequest`, `PostToolUse`, `Stop`, `SubagentStart/Stop`, `PreCompact`, `PostCompact`,
  `Interrupt`, but **no `Notification` event**. Non-managed hooks need trust; trust is recorded per
  hook as `hooks.state."<source>:<event>:<group>:<handler>".trusted_hash`.
- **Hook injection** **[decided 2026-10-20, verified M0.4 on 0.143.0]**: entirely through `-c`,
  nothing written to `~/.codex`:
  1. One `-c 'hooks.<Event>=[{hooks=[{type="command",command="ptyro hook codex <event>"}]}]'` per
     event. These become the *session-flags* layer (key source `/<session-flags>/config.toml`).
  2. One `-c 'hooks.state={"/<session-flags>/config.toml:<event_snake>:0:0"={trusted_hash="sha256:<h>"}, …}'`.
     Codex reads hook trust from the user layer **and the session-flags layer**
     (`hook_states_from_stack`), so this pre-trusts *exactly our* hooks for this invocation. It does
     not bypass trust for repo or user hooks, which `--dangerously-bypass-hook-trust` would.
  3. `<h>` = SHA-256 hex of the compact, key-sorted JSON of
     `{"event_name": "<event_snake>", "hooks": [{"type":"command","command":"<cmd>","timeout":600,"async":false}]}`.
     `timeout` is normalized to 600, or 1 for `session_end`; `matcher` is omitted when absent.
     Reproduced bit-for-bit for 6 events (`crates/ptyro-agents/tests/fixtures/codex-0.143.0/trust-hashes.json`). The
     identity is Codex-internal and may change between versions, so the adapter golden-tests it per
     supported version. When a version mismatches, Codex shows "hooks need review" instead of
     running them: ptyro detects that screen text and falls back to `notify` + title parsing,
     flagging `ptyro doctor`.
  - **Gotcha:** `-c` splits keys on every `.` and ignores TOML quoting, so a dotted key containing
    `"…config.toml…"` or a path silently becomes a different key. Always put such keys inside an
    inline-table *value* (`hooks.state={…}`, `projects={"<path>"={trust_level="trusted"}}`).
  - The `CODEX_HOME` overlay idea is **rejected**. Symlinking `auth.json` into another home is
    unsafe, because a token refresh may replace the file and strand the original.
- **Directory trust for worktrees:** each new worktree is a new directory, so Codex would show its
  trust dialog every time. `-c 'projects={"<worktree>"={trust_level="trusted"}}'` skips it
  **[verified]**. ptyro passes this only when the user has trusted that project in Codex already
  (main checkout `trust_level = "trusted"` in the profile's config) or set
  `agents.codex.trust_worktrees = true`. Trusting loads the repo's `.codex/` config and hooks, so it
  stays the user's decision.
- **Startup interstitials:** the update prompt ("Update available! … 1. Update now / 2. Skip")
  appears before the trust dialog and swallows early keystrokes. Prompt delivery must wait for the
  real input box (`› ` prompt line) or a `SessionStart` hook, never a fixed delay.
  `check_for_update_on_startup = false` via `-c` is the likely fix **[unverified]**.
- **Conversation id:** cannot be pre-assigned; learned from `SessionStart.session_id` **[verified:
  present, UUIDv7-style, plus `transcript_path`]**, `notify` `thread-id`, or `thread.started` in exec
  mode. Fixtures: `crates/ptyro-agents/tests/fixtures/codex-0.143.0/hooks/`. `Stop`, `PermissionRequest` and `notify`
  payloads are **not yet captured** (no working Codex login when recorded; re-test after
  `codex login`).
- **Status signals:** `UserPromptSubmit` → working; `PermissionRequest` → blocked(permission);
  `Stop`/`notify agent-turn-complete` → idle; process exit → exited. Codex's OSC 0 title
  (`tui.terminal_title`, spinner) is a fallback signal.
- **Resume:** `codex resume <uuid>`. Transcripts: `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`.
- **Alt screen:** `tui.alternate_screen = auto` — fine under ptyro (we fully support 1049).
- **Structured:** `codex app-server --listen unix://…` (JSON-RPC with approval requests) for
  structured sessions (M10); `codex exec --json` for one-shot fan-out workers.

**[built M4]** As designed. One `-c hooks.<Event>` per event in `SessionStart`,
`UserPromptSubmit`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `Stop`, `Interrupt` and
`SubagentStart/Stop`, plus a matching `hooks.state` with trust hashes golden-tested against the
0.143.0 fixture, and a `notify` that chains the user's own.
- `projects={…trusted}` is passed for a worktree only when the project's main checkout is trusted
  in that Codex home, or `trust_worktrees = true`.
- `on_title` reads the braille spinner (working). `on_screen` recognises the update prompt, the
  trust dialog and "hooks need review" (which also logs a doctor warning). `ready()` is the `›`
  input line.
- Answers: `y` / `a` / Esc. Stop: Esc, then `/quit`. The conversation id is learned from
  `SessionStart`.
- **[unverified]** `Stop`, `PermissionRequest` and `notify` payloads are still uncaptured (Codex
  login, §20.2), so their mapping follows the docs and the fake agent.

### 8.5 Gemini CLI **[verified 0.64.0 where noted]**

- **Launch:** `gemini --session-id <uuid> [configured args]` (flag present in 0.64 local help).
- **Hooks:** settings-based (`SessionStart`, `BeforeAgent`, `AfterAgent`, `BeforeTool`, `AfterTool`,
  `Notification{notification_type: ToolPermission}`, `SessionEnd`, …). No `--settings` flag. Hook
  arrays merge with strategy `concat` across layers, so they add rather than replace.
- **Hook injection** **[decided 2026-10-20, verified M0.5 on 0.64.0]**: a **ptyro Gemini extension**
  (`gemini-extension.json` + `hooks/hooks.json`; template in
  `crates/ptyro-agents/tests/fixtures/gemini-0.64.0/extension/`), materialized at `data/integrations/gemini/ptyro/`
  and **linked once** with `gemini extensions link <dir> --consent`. Linking writes to the user's
  (or profile's) Gemini home, so it is **opt-in**: `ptyro init` / `ptyro integrations install
  gemini`, listed by `ptyro doctor`. Its hooks call `ptyro hook gemini <event>`, which exits 0
  silently when `PTYRO_SESSION_ID` is unset, so it is inert outside ptyro. Verified: `SessionStart`
  and `SessionEnd` fire from a linked extension, carrying the pre-assigned `--session-id`.
  - The planned `GEMINI_CLI_SYSTEM_SETTINGS_PATH` / `GEMINI_CLI_SYSTEM_DEFAULTS_PATH` route is
    **rejected**. Gemini skips system layers whose file or parent directory is not owned by root
    ("Security Warning: Skipping system defaults file … not owned by root").
  - Without the extension, status comes from the title (medium confidence) and the activity
    heuristic.
- **Title fallback:** **[verified M0 spike, 0.64.0]** the idle title is `◇  Ready (<folder>)` (glyph, two spaces, state, folder). `ui.dynamicWindowTitle` (default on) puts Ready / Action Required / Working
  in OSC 0 — parsed by the adapter with low-medium confidence.
- **Resume:** `gemini --resume <uuid>`; storage `$GEMINI_CLI_HOME/.gemini/tmp/<project>/chats/session-<ts>-<id8>.jsonl`.
- **`GEMINI_CLI_HOME`** **[verified]** replaces the *home directory*: Gemini uses
  `$GEMINI_CLI_HOME/.gemini/` for everything (settings, extensions, trust, chats, credentials), which
  confirms the profile mechanism for Gemini (§10.10).
- **Turn-level hooks** (`BeforeAgent`/`AfterAgent`/`Notification`) are **not yet captured**: the
  API key available when recording belonged to a project with the Gemini API disabled (HTTP 403).
  Re-test with a working key.
- **Gotcha (secrets on screen):** the auth dialog pre-fills `GEMINI_API_KEY` **in clear text**.
  Recordings and screenshots of agent startup can contain secrets: the recorder (§7.6) and fixture
  capture (docs/fixtures.md) must assume screens are sensitive.
- **Structured:** `gemini --acp` (Agent Client Protocol over stdio) for M10.
- **Note:** Google moved free-tier users to "Antigravity CLI" (2026-06); an `antigravity` adapter is
  in the backlog once researched.

**[built M4]**
- The extension is opt-in: `ptyro integrations install gemini` writes it under
  `data/integrations/gemini/ptyro/` and links it into the session profile's Gemini home;
  `integrations ls` / `uninstall` manage it.
- Launch: `--session-id <uuid>`; resume: `--resume <uuid>`.
- Hooks: `BeforeAgent` → working, `BeforeTool` → working(tool),
  `Notification(ToolPermission)` → blocked, `AfterAgent` → idle.
- `on_title` maps `◇ Ready` / `✋ Action Required` / working titles at medium confidence.
- `ptyro profile doctor` warns when a profile uses consumer Gemini login: the free tier moved to
  Antigravity.

### 8.6 OpenCode **[from docs/source; not yet installed locally]**

- **Launch:** `opencode --port <allocated> --hostname 127.0.0.1` with `OPENCODE_SERVER_PASSWORD`
  set to a random per-session secret. The TUI embeds an HTTP server; the adapter subscribes to SSE
  `GET /event` for `session.status` (`idle|busy|retry`), `permission.asked`/`replied`,
  `session.error`.
- **Conversation id:** server-assigned (`ses_…`), learned from events.
- **Resume:** `opencode --session <id>` (or `-c`).
- **Structured:** `opencode serve` + HTTP API (`POST /session/:id/prompt_async`,
  `/permissions/:id`) — the cleanest structured integration of the four.

**[built M4, not run against a real OpenCode yet]** Launch adds `--port <free
port> --hostname 127.0.0.1` and `OPENCODE_SERVER_PASSWORD=<random>`. The daemon follows `GET
/event` with basic auth (user `opencode`), reconnecting while the session lives:
- `session.status` busy/retry/idle → working/working/idle;
- `permission.asked`/`updated` → blocked, remembering the permission id;
- `permission.replied` → working;
- `session.error` → errored.

`ptyro answer` posts `once`/`always`/`reject` to `/session/:id/permissions/:permissionID`. The
fake agent's `--as opencode` serves this API for the tests.

### 8.6b Antigravity CLI (`agy`) **[built M4.21 from its docs; unverified, not installed]**

Google's successor to Gemini CLI for consumer accounts. Its hooks are `SessionStart`,
`PreInvocation`, `PreToolUse`, `PostToolUse`, `PostInvocation` and `Stop{fullyIdle,
terminationReason, error}` with camelCase fields, configured in `~/.gemini/config/hooks.json`.
`ptyro integrations install antigravity` writes that file (opt-in, inert outside ptyro).
- Mapping: `PreInvocation` → working; `PreToolUse` → working(tool); `Stop` → idle when
  `fullyIdle`, errored on `error`.
- There is no permission hook, so *blocked* comes only from OSC 7501, if agy emits it.

### 8.7 Shell and generic adapters

- `shell`: `$SHELL -l`; OSC 133/OSC 7 support if the user's shell emits them (ptyro ships optional
  shell-integration snippets for bash/zsh/fish, `ptyro shell-init <shell>`). Status: `idle` at
  prompt, `working` while a command runs (from OSC 133), else unknown.
- `generic`: any user-defined agent command. Status from OSC 7501 (if the program speaks it),
  OSC 9 notifications, BEL, and the activity heuristic (§9.3).

**[built M4]** `shell` sessions get working/idle from OSC 133 (C → working; D → idle with the exit code). OSC 133 A also ends any 7501 working/blocked records. For
`generic`, OSC 7501 from any program is trusted like any other emitter. 7501 `done` maps to
`Done` (task complete), not `Idle`, because `done_means_idle` is false for generic tools.

---

## 9. Status engine and attention

### 9.1 Status model **[decided]**

```rust
enum AgentStatus {
    Unknown,                         // no signal yet / adapter has none
    Starting,                        // spawned, not yet ready for input
    Working { activity: Option<String> },        // "Bash: cargo test", "Editing src/x.rs"
    Blocked { kind: BlockKind, detail: Option<String> }, // permission | question | auth | error-prompt
    Idle,                            // turn finished, waiting for the next prompt
    Done,                            // orchestration: task reported complete (OSC 7501 done / run step)
    Errored { reason: ErrorReason, detail: Option<String> }, // rate_limit, auth, overloaded, ...
}
struct StatusRecord { status: AgentStatus, confidence: Confidence /* high|medium|low */,
                      source: SignalSource, since: Timestamp, progress: Option<u8> }
```

Process state (§7.3) dominates: when the process exits, status is cleared.

### 9.2 Signal sources and precedence

| Priority | Source | Confidence | Examples |
|---|---|---|---|
| 1 | Process events | high | exit, spawn failure |
| 2 | Hooks (authoritative, carry conversation id) | high | Claude `PermissionRequest`, `Stop`, `StopFailure`; Codex `PermissionRequest`, `Stop`; Gemini `Notification(ToolPermission)`, `AfterAgent`; OpenCode SSE |
| 3 | OSC 7501 | high | Claude `state=blocked:kind=permission` |
| 4 | Title parsing | medium | Gemini dynamic title, Codex spinner title |
| 5 | Notifications (OSC 9/777/BEL) | low→medium | "waiting for input" |
| 6 | Activity heuristic | low | output in last 2 s ⇒ "active?"; quiet for 30 s after activity ⇒ "quiet" |

Fusion rules:

- **High-confidence sources (1–3) are peers: the most recent event wins.** Priority only breaks
  ties within 250 ms. **[decided 2026-10-20 from M0.3]** A strict "hooks beat 7501" rule is
  wrong: denying a permission prompt fires no hook at all (no `Stop`), and only OSC 7501 reports
  the return to `idle`, so a strict ranking would leave the session `blocked` until it went stale.
- Lower-confidence sources (4–6) only apply when no high-confidence source has spoken for
  `stale_after` (default 5 min), so a missed hook can't freeze the state forever.
- `UserPromptSubmit`/`working` clears `Blocked` and `Idle`.
- Every transition is persisted (`session_events`, §14.3) — powers timelines, metrics, and debugging
  of adapters ("why did it say blocked?" → `ptyro status --explain`).
- Heuristic-only sessions show their status with a visible low-confidence marker (e.g. `◌`), never
  pretend certainty. **[decided]**

### 9.3 Activity heuristic (fallback only)

Counts bytes written to the PTY by the child per 250 ms bucket, ignoring cursor-blink-only and
spinner-only redraws (rows whose text didn't change). Derives `active` / `quiet`; never `blocked`.

### 9.4 Attention

- A session gains **attention** when it transitions to `Blocked`, `Idle` (after `Working` for
  ≥ `min_work` = 10 s), `Errored`, `Done`, or `Exited`, **unless** it is currently displayed in the
  focused pane of any controller client.
- Cleared when the session is focused, or via `ClearAttention`.
- Ordering for "jump to next attention": `Blocked` > `Errored` > `Done` > `Idle` > `Exited`, oldest
  first within a class.

### 9.5 Notifications

- Every attention-raising transition and every OSC 9/777/99 message from a child creates an
  **inbox entry** (session, kind, title, message, time, read/unread).
- Delivery channels (per category, configurable, with per-project and per-session mute and quiet
  hours): TUI inbox badge (always), bell, OSC 9 / OSC 777 to the controlling client's outer terminal
  (wrapped in tmux DCS passthrough when the client runs inside tmux), native desktop notification
  (`notify-rust`, optional feature), and later push to paired phones (§16).
- Messages carry real context: `brisk-rook (myrepo) wants permission: Bash(npm install)` rather than
  "waiting for input".
- Rate limit: coalesce per session per 10 s.
- Notification titles carry the session's profile label (`[work] brisk-rook wants permission…`).

### 9.6 Workspace-scoped attention **[proposed]**

Every session has a **home workspace**: the one it was created from in the TUI. For CLI spawns it is
`--workspace`, else the only workspace containing its project, else the controlling TUI's current
workspace, else `main`.

- Per workspace, `notify = "all" | "badge" | "mute"`, plus optional `quiet_hours`:
  - `all`: the channels of §9.5;
  - `badge`: an inbox entry and a status-bar counter only; no bell, OSC, desktop or push,
    except kinds listed in `breakthrough` (default `["blocked"]`, since a blocked agent is wasting
    time);
  - `mute`: inbox only, no breakthrough.
- `attention.next` (`a`) uses `attention.scope`:
  - `"current-first"` (default): exhaust the current workspace, then continue into others, switching
    workspace with a toast (`→ personal`);
  - `"current"`: never leave the current workspace;
  - `"global"`: §9.4 ordering across everything.
- The status bar shows the current workspace's counts in full and every other workspace as a
  compact badge (`me ⚠1`). The inbox has one tab per workspace plus `All`.

### 9.7 What M4 built **[built M4]**

**Engine** (`ptyro-daemon/src/engine.rs`, one per session; all signals go through one feed task):
- **Sources:** process (high), hook (high), sidecar (OpenCode SSE: high; Claude roster: medium),
  OSC 7501 (high), `ptyro report` (high), title (medium), screen recognisers (medium), OSC
  9/777/BEL (low), heuristic (low).
- **High sources are peers: the newest wins.** A weaker high source that arrives within 250 ms
  of a stronger one is **deferred, not dropped**. It is applied 270 ms later unless a newer high
  signal superseded it. Dropping it lost real progressions (agy's `Stop` right after a 7501
  `working`).
- **Medium sources** apply only after high ones have been quiet for `attention.stale_after`
  (default 5 m), and **low** only when neither has spoken. The heuristic is not even evaluated
  while a high signal is fresh, which keeps the explain log readable.
- **Same-status refreshes** from a weaker source never overwrite a stronger source's detail
  (Claude's generic `Notification` vs. the tool from `PermissionRequest`).
- **Ring:** every decision lands in a 64-entry ring that `ptyro status S --explain` prints. Each
  line has the source, the signal, `applied` and the reason ("tie: hook beat osc", "stale-gated:
  high spoke 3s ago", "deferred", "superseded").
- **Persistence:** every transition is persisted as a `status` session event, with the previous
  status, its confidence and how long it worked.

**OSC 7501 rev 0.4** (`ptyro-term` `parse_psp`, conformance-tested through `fake-agent
conformance`):
- **Validation:** any limit breach, bad base64, control character, invalid id, or missing or
  unknown state discards the whole report. Malformed pairs are skipped, the last repeated key
  wins and whitespace is trimmed.
- **Fields:** `kind` only with blocked; `progress` only with working/blocked. Ids are dotted
  segments `[A-Za-z0-9_.+-]{1,32}`, at most 8 levels and 128 bytes, and `app` is inherited from
  the nearest ancestor.
- **Record lifetime:** records live in a 256-entry LRU. `clear` removes a subtree; OSC 133 A and
  process exit drop working/blocked records; RIS (`ESC c`) clears everything; OSC 9;4 progress
  stops being mapped once 7501 has been seen.
- **Effective status** is the most urgent record (blocked > error > working > done > idle; the
  newest among equals). Child records become `+N` child rows.
- OSC 21337 (iTerm2's foreign status) is accepted at low confidence. `AgentStatus::parse`
  also maps tuios's and herdr's state names (`needs_input`, `errored`, `busy`, …), which is what
  `ptyro report` (alias `report-agent`) accepts.

**Attention** (§9.4) as specified, plus:
- A session is *watched* only in a focused pane of a client whose outer terminal has focus.
  Switching away from the terminal lets a blocked agent notify.
- `Idle` raises attention only after a *reliable* `Working` (confidence above low) that lasted
  `min_work`, so a heuristic blip never pings.
- Attention clears on focus (which also marks that session's inbox entries read), on `Working`,
  and on `ptyro answer`.
- Prompt delivery that gave up raises a `notification` attention.

**Notifier** (`notifier.rs`):
- Every attention goes to the inbox (in memory and in the store). The other channels are gated
  by `notifications.on`, mutes (`ptyro mute S|project:P|workspace:W [--for 2h] [--off]`), the
  home workspace's `notify` mode with `attention.breakthrough`, quiet hours (wrapping midnight)
  and per-session coalescing (`coalesce = "10s"`; `Blocked` always passes).
- **Channels:**
  - `osc`/`bell`: a `Notify` event; each TUI writes OSC 777 (and OSC 9) to its own terminal, in
    tmux DCS passthrough when inside tmux;
  - `desktop`: `notify-send` (with `--` before the agent-controlled title and body) /
    `osascript`, or `desktop_command` with `{title}`/`{body}` (an argv run without a shell; the
    values are agent-controlled, so a `sh -c` wrapper around them is unsafe);
  - `sinks`: ntfy, Pushover, Slack, Discord, Telegram (through `curl --config -`, so tokens and
    webhook URLs never appear in a process listing), or `command` (JSON on stdin). Secrets are
    `Secret`s (`{command = [...]}` preferred).
- **Approve links:** with `[notifications.callback] listen` set, a blocked notification carries
  approve/deny URLs. Links are HMAC-SHA256-signed with `data/callback.key` (0600), single-use and
  expiring after `link_ttl` (15 m). A GET renders a confirmation page; only its POST answers, so
  link unfurlers can't approve anything. Plain HTTP, so `listen` must be loopback or Tailscale
  (`config::safe_address`, the rule `ptyro serve` uses) unless `insecure = true`; otherwise the
  listener is not started and the daemon log says why.
- `ptyro notify test` sends one notification through every channel and sink and reports which
  delivered.

**Usage meters** (R-USE-01):
- **Claude:** the statusline relay gives the 5 h / 7 d `used_percentage` and `resets_at`, and
  the cost.
- **Codex:** opt-in (`[agents.codex] usage = true`). A side `codex app-server` per Codex home
  answers `account/rateLimits/read` and pushes `account/rateLimits/updated`.
- A `StopFailure(rate_limit)` marks the profile rate-limited until the window resets.
- Shown per profile in the sidebar title, the status bar and `ptyro usage`; `ptyro usage` adds
  `ccusage` daily totals when it's installed.

**`after_turn`:** a trusted project's `[scripts] after_turn` runs after each completed turn
(Working → Idle), with the worktree env (§10.6), unless `[agents.<name>] after_turn = false`.
It runs as the user outside the agent's sandbox, and runs the files the agent just edited
(§10.5, "What trust covers").

**Workspace scope** (§9.6) is implemented: `notify`, `breakthrough`, the `attention.scope`
modes, other-workspace badges and the per-workspace inbox tabs.

---

## 10. Projects, worktrees, setup scripts, ports, profiles

### 10.1 Projects

- `ptyro project add [path]` registers the git repo containing `path` (default cwd); stores its main
  checkout path, default branch (from `origin/HEAD`, else `main`/`master`), remotes.
- Projects may be added to workspaces; adding from inside the TUI uses a fuzzy directory picker
  seeded from `~/src`, `~/dev`, `~/code`, `~/projects` and configured `discover_roots` (scan depth
  3, `.git` detection, cached).

**[built M3]**
- The main checkout is found through `--git-common-dir`, so `project add` from inside a linked
  worktree registers the real repository.
- The slug comes from `.ptyro.toml [project] name`, else the directory name; `-2`, `-3` suffixes
  keep it unique.
- `remote_url` is `origin`'s URL, else the first remote's.
- Every project gets an `@root` worktree row for its main checkout. It is never locked or
  removable.
- Discovery walks `[discover] roots` (depth 3). It stops at repositories and skips hidden dirs,
  `node_modules`, `target`, `vendor` and `venv`, and caches the result for an hour in kv
  `discover:cache`. The CLI command is `project discover`; in the TUI it's the palette's
  `project.add`.

### 10.2 Worktree layout **[decided]**

- Default root: `data/worktrees/<project-slug>/<worktree-name>` (outside the repo: no watcher noise,
  no `.gitignore` edits, no IDE confusion).
- Alternatives via config: `worktrees.location = "sibling"` →
  `<repo>/../<repo>.worktrees/<name>`; `"in-repo"` → `<repo>/.ptyro/worktrees/<name>` (ptyro adds
  it to `.git/info/exclude`, never to `.gitignore`).
- Symlinked worktree roots are refused.

**[built M3]** The symlink check covers the part of the path ptyro owns:
- `<data>/worktrees/<slug>/<name>`;
- `<repo>.worktrees/<name>`;
- `.ptyro/worktrees/<name>`.

Symlinks higher up, like a macOS `/var` → `/private/var` or a symlinked home, are fine. An
existing target path is refused.

### 10.3 WorktreeSpec and creation

```rust
struct WorktreeSpec {
    project: ProjectAddr,
    name: Option<String>,         // default: friendly random name
    branch: Option<String>,       // default: template, e.g. "ptyro/{name}"
    base: Option<String>,         // default: project base (origin/<default>), fetched first
    from: Option<WorktreeSource>, // ExistingBranch(b) | PullRequest(n) | Issue(url) | Commit(sha)
    setup: SetupMode,             // run | skip | inherit (from project config)
}
```

Creation steps (async; progress streamed as events; each step retryable):

1. `git fetch <remote> <base>` (skippable offline; `--no-fetch`).
2. `git worktree add -b <branch> <path> <base>` (or check out existing branch / `pull/<n>/head`).
3. `git worktree lock --reason "ptyro:<worktree-ulid>" <path>` while any session runs in it.
4. Write marker `<git-common-dir>/worktrees/<name>/ptyro.json` (`{id, created_at, project}`).
5. Copy includes (§10.4). 6. Allocate ports (§10.6). 7. Run `setup` script (§10.5) in a `script`
   session — the user can watch it in a pane; agents may be started immediately or after setup
   (`start_agents = "after_setup" | "immediately"`).

Branch-name templates: `{name}`, `{user}`, `{date}`, `{issue}`, `{slug}`; optional **auto-rename**
(optional): after the first agent turn, offer a branch name derived from the first prompt
(`ptyro wt rename-branch`; never automatic once pushed).

**[built M3]**
- **Profile:** the worktree's profile is resolved like a session's, against the project's
  `@root`, and stored on the worktree; `default` is stored as such. Git runs with that
  profile's environment, so a fetch uses its SSH key.
- **Fetch:** failures are non-fatal ("fetch failed, using the local origin/main"), for offline
  work. `worktrees.fetch = false` or `--no-fetch` skips it.
- **New branches** are created with `--no-track`, so `ptyro/x` doesn't claim `origin/main` as its
  upstream. `--existing --branch B` checks out a local branch, or a remote one tracked. `--pr N`
  fetches `pull/N/head` into `pr/N`.
- **Marker:** `ptyro.json` (`{id, project, name, created_at}`) goes in the worktree's own git dir.
- **Agents:** `start_agents` is `[worktree] start_agents` in `.ptyro.toml`, else
  `[worktrees] start_agents`. With `after_setup` the agent is spawned when the setup session
  exits, and the TUI's pane follows it.
- **Rename:** `wt rename-branch` refuses once the branch has an upstream or exists on a remote.
  Without a name it uses the template with `{name}` set to a slug of the first commit's subject.
  Deriving the name from the agent's first prompt comes with prompt delivery in M4.

### 10.4 Files that git doesn't carry

- **`.worktreeinclude`** at repo root, gitignore syntax: a file is copied iff it matches **and** is
  git-ignored (compatible with Claude Code / Conductor semantics).
- Project config `worktree.copy = [...]` (globs) and `worktree.share = [...]` for heavy directories
  (`node_modules`, `.venv`, `target`): CoW clone (`reflink-copy`; APFS clonefile, btrfs/xfs reflink)
  with fallback to `symlink` or `copy` per `worktree.share_mode`. Default `share = []` — sharing
  breaks isolation when agents add dependencies, so it is opt-in.

**[built M3]**
- **Matching:** git itself matches, so the semantics are exactly gitignore's.
  `git ls-files --others --ignored --exclude=<pattern>` lists untracked files matching a
  pattern, and `.worktreeinclude` keeps only those `git check-ignore` confirms. `copy` takes
  matching untracked files whether ignored or not; tracked files are never copied, and nothing is
  overwritten.
- **`share_mode`:**
  - `clone` (default): `cp -c` (APFS) or `cp --reflink=always`, falling back to a symlink;
  - `symlink`;
  - `copy`: a CoW clone, else a full copy.
- **Ignoring shared entries:** an entry the main checkout ignores is also added as `/<entry>` to
  `.git/info/exclude`. A symlinked `node_modules` is a *file* to git, which `node_modules/` doesn't
  match, so without this every removal would see "untracked work".

### 10.5 Lifecycle scripts and trust **[decided]**

Scripts are defined in the project config (§13.3): `setup`, `run.<name>` (named run scripts with
`default`), `archive` (before removal), `after_turn` (optional, after each agent turn — lint/format).

- Run via `$SHELL -c` in the worktree, in their own PTY session (kind `script`), own process group,
  with the env of §10.6.
- **Trust:** scripts come from the repository and are arbitrary code. Before the first run — and
  after any change — ptyro shows the exact script text and asks to trust it. Trust is recorded as a
  SHA-256 fingerprint of the canonicalized `[scripts]` table per project. `ptyro project trust`.
  Untrusted scripts never run; worktree creation still works without them.
- **What trust covers [security review M3]:** the fingerprint pins the *text* of the commands,
  not the files they call. `cargo test`, `make check` or `./scripts/lint.sh` run whatever the
  checkout holds at that moment, and agents edit the checkout. Trusted scripts, `after_turn` and
  verify checks (§15.6.5) run as the user, with the daemon's full environment and network,
  outside any sandbox an agent runs in. So trusting a command trusts every file it reads, now
  and after any agent's edit. Sandboxing them is not attempted.
- `run_mode = "concurrent" | "exclusive"`: `exclusive` stops the run script in other worktrees of the
  same project before starting (avoids port/DB collisions).
- Stop: SIGTERM to the process group, SIGKILL after 5 s.

**[built M3]**
- **Fingerprint:** SHA-256 of the canonical JSON of the *merged* `[scripts]` table (`.ptyro.toml`
  plus both `.ptyro.local.toml`), one approval per project.
  - The CLI (`project trust`) and the TUI show `Scripts::describe()` (every script, labelled) and
    send back the fingerprint the user saw.
  - The TUI's wizard asks *before* creating the worktree, so `after_setup` still holds. A worktree
    whose checkout has different scripts than the one approved is reported as untrusted again.
- **Script sessions** are named `<worktree>.<script>` and run `$SHELL -c` in the worktree with
  the §10.6 environment. They run under the worktree's profile; a guard that refuses still
  refuses, since a script can't answer a question.
- **`after_turn`** runs with `wt run W after_turn`. The status engine calls it after each turn
  from M4.

**[built M5] Other tools' committed config** (M5.10, R-WT-03):
- **What fills in:** when `.ptyro.toml` leaves a script unset, it comes from another tool's file
  in the checkout, read in this order:
  1. `.conductor/settings.toml` (else the legacy `conductor.json`);
  2. `.superset/config.json` (+ `config.local.json`);
  3. `.codex/environments/*.toml`;
  4. `.claude/launch.json`;
  5. `.cursor/environment.json`;
  6. `.github/workflows/copilot-setup-steps.yml`.
- **Precedence:** ptyro's own files always win, then the first tool in that list. What is
  skipped is listed in the notes.
- **Trust:** imported scripts become part of the `[scripts]` table, so they are fingerprinted
  and trusted like ptyro's own. The trust text names the file each one came from
  (`── run.web (from .conductor/settings.toml) ──`).
- **Explain:** `project config --explain` lists the origin of every key, plus notes such as
  "setup not used: .conductor/settings.toml defines it", "cloud-only: skipped" and "skipped
  `uses: actions/setup-node`".
- **Mappings** (the formats themselves are in §20.7):
  - Conductor: `setup`/`archive`/`run` (a string or `[scripts.run.<id>]` with `args` and
    `options.cwd`); `nonconcurrent` → `exclusive`; `[environment_variables]` are exported first.
  - Superset: array entries become one line each after `set -e`; the local layer's
    `{before, after}` wrap; a missing key falls back to `.superset/<key>.sh`.
  - Codex: `[setup] script` with a `[setup.<platform>]` override, `[[actions]]` as run scripts
    (`Run` is the default), `[cleanup]` as archive.
  - Claude Desktop: each configuration is a run script. With `autoPort` (the default) the
    server gets `PORT` from the worktree's port block (`$PTYRO_PORT`, then `+1`…) and
    `--port`/`-p` arguments are rewritten to match, as Claude does.
  - Cursor: `install` is setup; `start` and each of `terminals[]` are run scripts.
  - Copilot: the `copilot-setup-steps` job's `run:` steps become setup. `uses:` steps and steps
    with `${{ }}` expressions are skipped and noted.
- **Environment aliases:** scripts from a tool get that tool's variables:
  - `CONDUCTOR_ROOT_PATH/WORKSPACE_PATH/WORKSPACE_NAME/PORT/DEFAULT_BRANCH/IS_LOCAL`;
  - `SUPERSET_ROOT_PATH/WORKSPACE_PATH/WORKSPACE_NAME`;
  - `CODEX_SOURCE_TREE_PATH`, `CODEX_WORKTREE_PATH`.
- **Opt-out:** `[project] import = false` turns all of it off.

### 10.6 Environment for scripts and sessions in a worktree

| Variable | Meaning |
|---|---|
| `PTYRO_PROJECT` | project slug |
| `PTYRO_ROOT_PATH` | project main checkout |
| `PTYRO_WORKTREE_PATH` | this worktree |
| `PTYRO_WORKTREE_NAME` | friendly name |
| `PTYRO_BRANCH` | branch |
| `PTYRO_PORT` | first port of this worktree's block (`PTYRO_PORT`..`PTYRO_PORT+count-1`) |
| `PTYRO_SESSION_ID` | (sessions only) |
| `CONDUCTOR_ROOT_PATH`, `CONDUCTOR_WORKSPACE_PATH`, `CONDUCTOR_WORKSPACE_NAME`, `CONDUCTOR_PORT` | only if `compat.conductor_env = true`, so existing setup scripts run unchanged |

**[built M3]** Also `PTYRO_PORT_COUNT`. These variables, and `PTYRO_PROFILE`, are on the
child-env strip list, so a daemon started inside a ptyro session can't leak its worktree into
others.

### 10.7 Port allocation

- Each worktree gets a block of `ports.count` (default 10) ports from `ports.range` (default
  `20000-29999`), persisted in the DB, released on worktree removal.
- Blocks are verified free at allocation (bind test) and skipped if occupied.
- The sidebar shows *actually listening* ports per session (§5.1), so "which worktree is on :3000?"
  is answered at a glance.

**[built M3]**
- **Allocation** is first fit, in steps of `count`: a block that overlaps no other worktree's and
  whose every port binds on 127.0.0.1 right now. If none is free, the worktree is created without
  ports and says so.
- **Listening ports** come from one scan every 3 s for every live session, covering the leader
  and all its descendants:
  - Linux: `/proc/*/stat` for the process tree, `/proc/<pid>/fd` socket inodes,
    `/proc/net/tcp{,6}` LISTEN rows;
  - macOS: `ps -A -o pid=,ppid=` and `lsof -iTCP -sTCP:LISTEN -p …`.
- Changes are announced as `SessionUpdated` and shown in the sidebar, pane header and `ls`.

### 10.8 Removal and cleanup **[decided — safety-critical]**

`ptyro wt rm <wt>`:

1. Refuse if any session is running in it (unless `--stop-sessions`).
2. Compute state: uncommitted changes, untracked non-ignored files, commits not on any remote,
   stashes. If any: **refuse** with a precise report, unless `--force` *and* interactive confirmation
   (or `--force --yes` in scripts).
3. Run `archive` script (if trusted). 4. `git worktree unlock`, `git worktree remove`.
5. Branch: deleted only if fully merged into base or pushed and identical to its remote; otherwise
   kept and reported ("kept branch ptyro/brisk-rook: 3 unpushed commits").
6. Release ports; mark worktree `removed` in DB (history kept).

`ptyro gc` lists (never deletes without confirmation) stale worktrees: removed-on-disk, merged
branches, no sessions for N days.

**[built M3]**
- **"Unpushed"** means commits on HEAD that are on no remote and not in the base. A local-only
  repository's merged work is therefore not "at risk".
- **The report** lists each kind with up to five items. `--force` (the CLI asks unless `--yes`)
  discards the work, and the result notes what was discarded.
- **Archive** runs only if the scripts are trusted, and is stopped after 120 s.
- **Sessions:** `--stop-sessions` stops running sessions. Exited ones in the worktree are
  forgotten along with it.
- **A missing directory** is just forgotten (`git worktree prune`).
- **gc** reports directories that are gone, branches merged into the base, and worktrees without
  sessions for `worktrees.stale_days` (default 14). It asks before each removal and never forces.

### 10.9 Repository status

Per worktree, refreshed on `notify` file events (debounced 500 ms) and after every agent turn:
branch, ahead/behind base and upstream, dirty file count, diffstat (+/−), last commit, PR number and
CI check status (via `gh`, when available, polled ≤ 1/min, M6). `gh` runs under the worktree's
profile (§10.10).

**[built M3]**
- **Watching:** each worktree is watched recursively with `notify` (500 ms debounce; churn in
  `.git/objects`, `.git/logs` and `*.lock` is ignored). Watches are registered on a blocking
  thread, because big trees take a while.
- **Polling** covers whatever the watches miss, and inotify limits: worktrees with live sessions
  are refreshed every 10 s, all of them every 60 s.
- **Status** is `git status --porcelain=v2 --branch` plus `rev-list --left-right` against the
  base and `diff --numstat <merge-base>`. That's the worktree's own change: committed and
  uncommitted, never the base's progress.
- **Locks:** every git call runs with `GIT_OPTIONAL_LOCKS=0`. Without it, the background
  `git status` takes `index.lock`, and a user's or agent's concurrent `git add` fails (seen in
  the tests).
- **No repository code [security review M3]:** every git call runs with `-c
  core.fsmonitor=false`, and every one but those the user asked for also with `-c
  core.hooksPath=/dev/null`. A repository's config (`.git/config`, which agents in its
  worktrees can write) can name code in both, and these calls run as the user, unasked,
  outside any sandbox. Status, diffs (also `--no-ext-diff --no-textconv`), logs, checkpoints,
  conflict checks, scratch worktrees and ref updates run no hooks. Hooks do run for what the user
  asks ptyro to do and would expect them on: `commit`, `push`, `merge`, `rebase`,
  `cherry-pick` and the `worktree add` of a new worktree (`Git::run_hooked`). Agents can edit
  those hooks too; that is the same trust as running git there yourself. Clean/smudge filters
  are left alone (LFS needs them).
- **A vanished directory** turns the worktree `missing`; it goes back to `ready` when the
  directory returns.

**[built M6] PRs and checks:**
- **The poller:** `gh` runs in the worktree under its profile's environment (so `GH_CONFIG_DIR`
  or `GH_TOKEN` pick the account), with prompts disabled. Every 10 s the poller asks about
  worktrees that are due: their branch is pushed (an upstream), they have a PR, or they're
  watched. Each is asked at most once per `github.poll` (default 60 s, floor 30 s).
- **Lookup:** `gh pr view <branch> --json number,url,title,state,isDraft,reviewDecision,autoMergeRequest`,
  then `gh pr checks <n> --json name,bucket,link,workflow`. Its JSON is read even when it exits
  non-zero (it exits 8 while checks are pending).
- **No `gh`:** the poller is idle (checked once a minute), and PR commands say what's missing.
- The sidebar shows `#7 ✓` / `#7 ✗2` / `#7 ◌` / `merged` on worktree rows.

**[built M6] The PR-event loop** (M6.10, R-GIT-01) runs for watched PRs: the ones ptyro opened
(`github.watch`), or `pr watch`.
- **Baseline:** the first poll records the existing comments and failures; only what changes
  after that is news.
- **New failures:** a check that newly fails sends the agent "[via ptyro: CI on PR #N is
  failing]", the check names and links, and the last `github.log_lines` lines of
  `gh run view --log-failed`.
- **Comments:** a new review (line) comment is sent as "[via ptyro: review comment from X on
  PR #N on path:line (comment ID)]". Conversation comments that look like questions (a short
  text with `?`, "why …", "should we", "wdyt") **raise attention instead**: the human decides.
  Bots' comments and ptyro's own replies are skipped.
- **Delivery** happens only when the agent's turn is over (it isn't working and the input box
  is ready). Several messages are joined into one prompt.
- **Replies** made with `pr reply` end with "— via ptyro".
- **Escalation:** a PR that stays failing, or keeps messages waiting, for
  `github.escalate_after` raises an `errored` attention once ("PR #N needs you: …").
- **Persistence:** state per worktree (watched, seen ids, failing checks, pending messages) lives
  in `kv` (`pr_watches`), so a daemon restart doesn't re-deliver anything.

### 10.10 Profiles: one identity per workspace **[decided 2026-10-20: concept · details proposed]**

Owner requirement: **several accounts (e.g. work and personal Claude subscriptions) used side by
side, and never crossed.** A **profile** is a named identity bundle defined in user config. Every
workspace points at one profile, and every session is launched under exactly one profile, which is
recorded on the session and never changes for its lifetime. The built-in profile `default` sets
nothing, so the agents use their usual config dirs; users who never define a profile see no
difference.

**What a profile controls:**

| Concern | Mechanism | Status |
|---|---|---|
| Claude account, settings, transcripts | `CLAUDE_CONFIG_DIR=<dir>` | variable present in the 2.1.304 binary; macOS Keychain separation is spike M0.10 |
| Claude by token or key instead | `CLAUDE_CODE_OAUTH_TOKEN` (from `claude setup-token`), `ANTHROPIC_API_KEY`, or `apiKeyHelper` in that dir's settings, supplied via `secrets` | variables present in the 2.1.304 binary |
| Codex account, config, rollouts | `CODEX_HOME=<dir>` (`auth.json`, `config.toml`, `sessions/`); or `CODEX_API_KEY` / `OPENAI_API_KEY` | present in the 0.143.0 binary; keyring storage (`cli_auth_credentials_store`) is spike M0.10 |
| Gemini | `GEMINI_CLI_HOME=<dir>`; or `GEMINI_API_KEY`, or `GOOGLE_CLOUD_PROJECT` + `GOOGLE_APPLICATION_CREDENTIALS` | docs; spike M0.10 |
| OpenCode | `OPENCODE_CONFIG_DIR`, plus the auth file under `$XDG_DATA_HOME/opencode/` | unverified (not installed); spike once installed |
| git identity | `GIT_AUTHOR_*` / `GIT_COMMITTER_*`, or a whole `GIT_CONFIG_GLOBAL=<file>` (signing key, `url.*.insteadOf`); `GIT_SSH_COMMAND="ssh -i <key> -o IdentitiesOnly=yes"` | git docs |
| GitHub CLI (M6 PRs/checks) | `GH_CONFIG_DIR=<dir>`, or `GH_TOKEN` via `secrets` | gh docs |
| Anything else | `env`, `env_file`, `secrets` (`AWS_PROFILE`, `KUBECONFIG`, npm tokens, …) | — |

Profiles apply to **every** session kind — agents, shells, scripts, editors — so `git push` typed
into a shell pane in the `work` workspace uses the work key and author.

**Example** (user config only; repo config can never define or select a profile, ADR 15):

```toml
[profiles.work]
label = "work"
color = "blue"
claude = "~/.claude-work"              # → CLAUDE_CONFIG_DIR
codex  = "~/.codex-work"               # → CODEX_HOME
gemini = { api_key = { command = ["op", "read", "op://Work/gemini/key"] } }
git    = { name = "Your Name", email = "you@acme.example", ssh_key = "~/.ssh/id_work" }
gh     = "~/.config/gh-work"           # → GH_CONFIG_DIR
env    = { AWS_PROFILE = "acme" }
match  = { remotes = ["github.com:acme/*", "github.com/acme/*"], paths = ["~/work/**"] }
share  = { claude = ["CLAUDE.md", "skills", "agents", "commands"] }   # symlinked from ~/.claude

[profiles.personal]
label = "me"
color = "green"
claude = "~/.claude"                   # adopting the default dirs is fine
match  = { remotes = ["github.com:you/*"] }
```

Omitted agents fall back to the managed directory `data/profiles/<name>/<agent>/` (created on first
`profile login`), never silently to the agent's default dir — that would cross accounts.

**Which profile a session gets** (first match wins; the wizard and `ptyro new` print the result and
its reason):

1. explicit choice: `--profile work`, `--agent claude@work`, or the wizard;
2. the worktree's profile (fixed when the worktree was created; a worktree's commits have one author);
3. the project's **pin** (`ptyro project pin myrepo work`; set automatically on `project add` when
   exactly one profile's `match` fits, confirmed in the TUI);
4. the session's workspace's profile;
5. `default`.

An explicit choice that contradicts 2 or 3 needs confirmation (TTY) or `--override-pin` (scripts).
**Guard:** at every spawn, if the target repo's remotes or path match a *different* profile's
`match` rules, ptyro applies `guard = "confirm" | "refuse" | "warn"` (default `confirm`; a CLI
spawn without a TTY counts as refuse unless `--yes`).

**Resume is bound to the profile.** Conversations live inside the profile's dirs
(`$CLAUDE_CONFIG_DIR/projects/…`, `$CODEX_HOME/sessions/…`), so a dormant session always resumes
under the profile it started with. If that profile was deleted, the session goes `Failed` with an
explicit error. `ptyro profile rename` rewrites every reference.

**Environment construction** at spawn (extends §5.1):

1. Start from the daemon's base env, with every profile-managed variable **always** removed
   (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`, `OPENCODE_CONFIG_DIR`, `GH_CONFIG_DIR`,
   `GIT_CONFIG_GLOBAL`, `GIT_AUTHOR_*`, `GIT_COMMITTER_*`, `GIT_SSH_COMMAND`). A daemon started
   from a shell with `CLAUDE_CONFIG_DIR` exported must not leak that account into `default`
   sessions; `ptyro doctor` reports any it found.
2. If the profile has `isolate = true`, also remove inherited credentials. `isolate` defaults to
   `true` for every user-defined profile, **and for `default` too as soon as any profile is defined**
   (decided 2026-10-20). Only a user with no profiles at all gets the fully inherited env. Set
   `[profiles.default] isolate = false` to opt out. The variables removed are: `ANTHROPIC_API_KEY`, `ANTHROPIC_AUTH_TOKEN`, `ANTHROPIC_BASE_URL`,
   `CLAUDE_CODE_OAUTH_TOKEN`, `CLAUDE_CODE_USE_BEDROCK`, `CLAUDE_CODE_USE_VERTEX`,
   `OPENAI_API_KEY`, `CODEX_API_KEY`, `GEMINI_API_KEY`, `GOOGLE_API_KEY`,
   `GOOGLE_APPLICATION_CREDENTIALS`, `GOOGLE_CLOUD_PROJECT`, `GH_TOKEN`, `GITHUB_TOKEN`,
   `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_SESSION_TOKEN`, `AWS_PROFILE`, plus
   `scrub_extra`. An exported `ANTHROPIC_API_KEY` otherwise silently overrides a subscription login.
   `SSH_AUTH_SOCK` is kept; use `ssh_key` (which sets `IdentitiesOnly=yes`) to pin a key.
3. Apply the profile: agent dirs, git/gh settings, `env_file`, `env`, resolved `secrets`.
4. Apply adapter variables (§8) and ptyro variables (§5.1), including `PTYRO_PROFILE=<name>`.
5. An agent definition's own `env` (e.g. a legacy `[agents.claude-work]` that sets
   `CLAUDE_CONFIG_DIR`) still wins, and `ptyro doctor` warns about it; profiles replace that pattern.

**Secrets:** `{ command = [...] }` values (1Password `op`, `pass`, `security find-generic-password`,
…) are resolved at each spawn and resume, held only in daemon memory (optional `secret_ttl`
cache), never written to the DB (the stored `launch` env diff records `<secret:NAME>`), never sent
to clients, and redacted in logs and `ptyro profile env`.

**Shared config without shared accounts:** `share` symlinks the listed entries from the agent's
default dir into the profile dir, so one `CLAUDE.md`, skill set and command set serves every
account. `ptyro profile sync` creates missing links. It never overwrites an existing file and
refuses to share credentials (`.credentials.json`, `.claude.json`, `auth.json`, `oauth_creds.json`).
Off unless configured.

**Login and onboarding:**
- `ptyro profile new <name> [--adopt claude=~/.claude-work …]` creates a profile, adopting existing
  dirs or creating managed ones.
- `ptyro profile login <name> <agent|gh>` opens a session under that profile that runs the tool's
  own login flow (Claude `/login`, `codex login`, Gemini's auth dialog, `gh auth login`). ptyro never
  sees the credentials.
- `ptyro init` detects existing multi-account setups and offers to adopt each one. The owner's
  setup — **aliases that point an agent at a separate config dir** — is the first-class case:
  - parse `alias NAME='VAR=dir … agent …'` and `NAME() { VAR=dir agent "$@"; }` forms for `VAR` in
    `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`, `GH_CONFIG_DIR`, in `~/.zshrc`,
    `~/.zshenv`, `~/.zprofile`, `~/.bashrc`, `~/.bash_aliases`, `~/.profile`,
    `~/.config/fish/config.fish`, and files they `source` (one level, no evaluation — text only);
  - the profile name is suggested from the alias suffix (`claude-work` → `work`); aliases for
    different agents with the same suffix merge into one profile;
  - the agent's plain default dir becomes the `default` profile, or a named one if the user picks;
  - also detected: extra `~/.claude-*`/`~/.codex-*` dirs with no alias, `export VAR=…` lines, and
    git `includeIf "gitdir:…"` identities, which seed `match.paths` and `git` settings.
  - ptyro never edits rc files; aliases keep working outside ptyro.
- `ptyro profile doctor [<name>]` checks each agent per profile: logged in (and as whom, where the
  CLI exposes it; spike M0.10), where credentials are stored, the git identity, and `gh auth status`.
  It also lists projects whose pin contradicts `match`.

**Visibility:** each profile has a `label` and `color`. It appears in the workspace header, on the
sidebar workspace row, in every session's header line (`● brisk-rook · claude · [work]`), and in the
status bar, and `tint_borders = true` also tints the pane borders. Desktop and push notifications
are prefixed with the label. The new-session wizard shows the resolved profile and why
(`work — pinned by project`).

**Orchestration:** sessions spawned by a lead agent through `ptyro mcp` inherit the lead's profile,
and cross-profile spawns are denied. Fan-out may mix profiles only when the user asks explicitly
(`--agents claude@work,codex@work`).

**Not a security boundary.** All profiles run as the same OS user, so an agent in `personal` can
still read `~/.claude-work` if it goes looking. Profiles prevent *accidents*, such as billing the
wrong account, the wrong commit author or the wrong `gh` org. True isolation needs separate OS users
or containers (backlog: sandbox per profile).

**[built M3]**
- **Schema.** `claude`, `codex`, `gemini`, `opencode` and `gh` each take a dir string or
  `{ dir, api_key, token }`:
  - `api_key` → `ANTHROPIC_API_KEY` (claude), `OPENAI_API_KEY` + `CODEX_API_KEY` (codex),
    `GEMINI_API_KEY` (gemini);
  - `token` → `CLAUDE_CODE_OAUTH_TOKEN` (claude), `GH_TOKEN` (gh).

  `git = { name, email, ssh_key, config }`. `[profiles.default]` may exist, to set `isolate =
  false` or give `default` real settings.
- **Managed dirs.** A named profile without a dir for an agent (or `gh`) gets
  `<data>/profiles/<name>/<agent>`, created `0700` at spawn.
- **Guards** apply only when the profile came from an explicit choice, the workspace, or
  `default`. A pin or a worktree profile is the user's decision already.
  - The guard used is the first claimant's.
  - `override_pin` also satisfies `confirm`, so one consent covers both questions; `refuse` always
    refuses.
  - Questions travel as `needs_confirmation` errors.
- **Spawn paths.** CLI, TUI, restart, scripts and editors all go through `Daemon::launch`:
  resolve, plan, strip, set.
  - Restart re-plans under the session's recorded profile, and fails with an explicit error if
    that profile was deleted.
  - Secrets are cached per label for `secret_ttl`, and the cache is cleared on config reload. A
    failing secret command's error shows its exit status and the first stderr line, never stdout.
  - The stored `launch` JSON is `{argv, cwd, profile, env}` with secrets as `<secret:NAME>`; a
    test checks the database and WAL for the value.
- **CLI.** `profile new/rm/rename/detect --apply` edit `config.toml` with `toml_edit`, keeping
  comments and layout. They validate the result before an atomic save, then send
  `ReloadConfig`. `rm` refuses while workspaces, pins, worktrees or live sessions use the
  profile. `rename` also rewrites the store (workspace, project, worktree and session rows) and
  live sessions.
- **Detection.** The alias detection described above is `ptyro profile detect [--apply]`; `ptyro
  init` (M5) will call it.
  - It reads `alias`/function/`export` lines, follows one level of `source`, and expands `$HOME`.
  - It also finds unreferenced `~/.claude-*`/`~/.codex-*`/`~/.gemini-*` dirs and `[includeIf
    "gitdir:…"]` identities.
- **Doctor.** `profile doctor` checks:
  - each agent's dir and its credential file (`.credentials.json`, `auth.json`,
    `oauth_creds.json`, `hosts.yml`; on macOS Claude may use the Keychain);
  - the git identity, and whether `ssh_key` exists;
  - pins that contradict `match`, and managed variables exported in your shell.
- **`profile login`** runs `claude` (a fresh dir starts its login), `codex login`, `gemini`,
  `opencode auth login` or `gh auth login` under the profile.

### 10.11 Change sets

A **change set** bundles linked worktrees of several projects (one per repo, same branch) into
one coordinated change with a lead agent, a contract and a dependency-ordered ship train. Its
worktrees are ordinary §10 worktrees. The orchestration design is in §15.5.

---

## 11. The TUI

### 11.1 Layout **[proposed]**

```text
┌ ptyro ─ main ───────────────────────────────────────────── 3 working · 1 blocked · ✉ 2 ┐
│ WORKSPACE main            │ ● brisk-rook · claude · ptyro/brisk-rook · +42 −7 · :3001   │
│ ▾ myrepo           +2 ⚠1  │┌───────────────────────────────────────────────────────────┐│
│   ▾ @root (main)          ││                                                           ││
│     ○ shell        idle   ││   (live Claude Code session, full fidelity)               ││
│   ▾ brisk-rook     +42 −7 ││                                                           ││
│     ● claude    working   ││                                                           ││
│     ○ nvim                ││                                                           ││
│   ▾ quiet-jackdaw         ││                                                           ││
│     ◉ codex  ⚠ permission ││                                                           ││
│ ▸ dotfiles                │└───────────────────────────────────────────────────────────┘│
│                           │                                                             │
│ INBOX (2)                 │                                                             │
│ ◉ quiet-jackdaw: Bash(...)│                                                             │
├───────────────────────────┴─────────────────────────────────────────────────────────────┤
│ C-Space ? help · n new · a next attention · p peek · d detach        TERMINAL   12:41  │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Sidebar** (collapsible, resizable): workspace → project → worktree → session tree with status
  glyphs, diffstat, ports, attention badges; inbox below.
- **Main area**: one or more panes (each showing one session), with a header line: status, agent,
  branch, diffstat, ports, controller/size badge.
- **Status bar**: global counts, mode indicator, key hints, clock.
- Status glyphs: `●` working (animated pulse), `◉` blocked (colour: warning), `○` idle, `✓` done,
  `✗` errored, `■` exited, `☾` dormant, `◌` low-confidence. Colours are theme tokens; respects
  `NO_COLOR`.

### 11.2 Modes and focus

- **Terminal mode** (default when a pane is focused): every key goes to the session, except the
  **prefix** key (default `Ctrl-Space`, configurable; chosen because Claude Code, readline, Vim and
  tmux's `C-b` don't use it). Prefix twice sends a literal prefix.
- **Navigation mode** (focus in sidebar or after prefix): single-key commands.
- **Zoom**: pane fills the screen (sidebar hidden). **Raw attach** (`ptyro attach --raw`): no chrome
  at all; ideal over flaky SSH or on a phone terminal app; detach with `prefix d`.

### 11.2b What M2 built **[built M2]**

- **Layout:** sidebar (sessions with 1–9 numbers, status glyphs, the foreground process, blocked
  kind), up to **four panes** in a split tree with 1-cell dividers, a one-row header per pane, and
  the status bar. The status bar shows the mode badge (PREFIX / COPY / SIDEBAR / VIEW ONLY /
  RECONNECTING), toasts, counts (live · working · blocked), hints and a clock.
- **Splits:** `v` side by side, `s` stacked (vim's sense), each through a picker that offers
  existing sessions or "+ new shell". Panes are focused with `h`/`l` (cyclic) or Shift-arrows
  (geometric), resized with `< > + -` or by dragging a divider, zoomed with `z`, closed with `c`.
  Every visible session is subscribed and **sized to its own pane**; hidden ones are unsubscribed.
- **The view persists in the daemon** (`SaveView`/`LoadView "main"`, stored in `kv`, debounced
  500 ms and on detach): the layout, what each pane shows, focus, zoom and the sidebar. A plain
  `ptyro` brings it back, even after a daemon restart. Panes whose session is gone come back
  empty; empty panes are filled from unshown sessions.
- **Overlays:** help (generated from the live key map, filterable, also showing the terminal's
  kitty-keyboard and colour support), the command palette (actions with their keys, sessions,
  themes), the session finder, a text prompt (new session: run through `$SHELL -c` so pipes and
  quotes work; rename), and a confirmation for stop.
- **Copy mode** (`prefix [` or `PageUp`, the mouse wheel, or a drag): §11.3 below.
- **Exited sessions** stay on screen, greyed, with a banner "exited N — ⏎ restart · x remove".
- **Multiple clients:** `--view-only` never sends input, resize or focus; `=` takes control.
  A session larger than the pane is cropped with a "viewing at C×R" badge, and the view
  **auto-pans to keep the cursor visible**.
- **Mouse** (when `ui.mouse`): click a sidebar row or pane to focus it, wheel scrolls the
  sidebar or enters copy mode, drag a divider to resize, drag in a pane to select (copied on
  release). Mouse events pass through to programs that enabled mouse reporting. Shift-drag
  keeps the outer terminal's own selection in most terminals.
- **Themes** `ptyro-dark`, `ptyro-light`, `high-contrast`, plus `[themes.<name>]` (any subset of
  16 tokens on a `base`). `NO_COLOR` makes chrome monochrome, with emphasis by attributes. Every
  colour, chrome and session content alike, is **downsampled** to what the outer terminal shows
  (truecolor / 256 / 16, detected from `COLORTERM`, `TERM` and `TERM_PROGRAM`).
- **Reconnect:** if the daemon goes away, the TUI shows RECONNECTING, retries every 250 ms for
  15 s, and restores the view when it's back.

### 11.2c What M3 added **[built M3]**

- **The sidebar tree** shows the workspace's projects, each with `@root` first and then the other
  worktrees by creation, each with its sessions. Below them, under "other sessions", are the
  sessions whose home is this workspace but that aren't under one of its projects.
  - Rows show the pin's profile label (in its colour), the branch when it isn't the default
    `ptyro/<name>`, `+N −M` lines changed, a `●` dirty marker, the worktree state when not ready,
    and listening ports.
  - Folding: `⏎`/`Space`, `h`/`l`, or a click. `1`–`9` number the visible session rows, and
    `j`/`k` cycle every session of the workspace in tree order.
- **Workspaces:** `w` opens the switcher (each workspace's profile, project and session counts, "+
  new workspace"). Views are saved per workspace (`view:<name>`), and the last workspace is
  stored under `_tui`. Opening on a session from another workspace switches to that workspace. A
  plain `ptyro` reopens the last one.
- **The wizard** (`n`):
  1. **Agent:** the default agent first, then the other agents, `shell`, `command…`, and
     `<default>@<profile>` for each defined profile.
  2. **Target:** "+ new worktree in <project>" first, then the context worktree, its project's
     worktrees, the others, and "here" (the focused session's directory).

  The title shows `ProfileResolve`'s answer ("work — pinned by project · will ask first"). `N`
  asks for a name, then an agent ("no agent" too). The *context* is the selected sidebar row,
  else the focused session's worktree, else the workspace's first project.
- **Dialogs:**
  - Trust shows the scripts in full: `y` trusts and continues, `n` creates the worktree without
    them.
  - A `needs_confirmation` reply becomes a `[y/N]` dialog that resends with consent.
  - A refused removal shows the safety report and offers to force.
  - After `project.add` (the palette) pins a project, "keep the pin? [Y/n]".
- **Identity:**
  - the `[label]` badge in its colour on the sidebar's workspace title, project rows and every
    pane header (with branch, diffstat and ports);
  - the focused session's profile and the workspace name in the status bar;
  - a session in a worktree pinned to another profile shows its own badge;
  - `ui.tint_borders` colours the dividers and a header marker with the focused session's profile
    colour.
- **More keys:** `o` opens an editor picker: `inside` editors spawn as a session in the worktree,
  others are launched detached. `r` runs the default run script and `R` picks one; an untrusted
  project gets the trust dialog first. Palette-only actions: `project.add`, `project.trust`,
  `worktree.remove` and `session.command`.

### 11.2d What M4 added **[built M4]**

- **Status glyphs** on sidebar rows and pane headers:
  - working `●`, pulsing with `◍`; blocked `⚠` with its kind; idle `○`; done `✓`; errored `✗`;
  - dormant `☾`;
  - a low-confidence status is drawn `◌`, never as certain.
- **Row badges:** `⚑` attention, `+N` child agents, `⚠` for `dangerous` agents. The header shows
  the current activity (`Bash: cargo test`) and a progress bar when one was reported.
- **Status bar:** the focused profile's usage (`5h 42% · 7d 12%`, or `limited until 14:05`),
  compact `⚑N` badges for other workspaces, and `✉N` unread.
- **`a`** jumps to the next attention. Order: blocked > errored > done > idle > exited >
  notification, oldest first, within `attention.scope`.
- **`p` (peek)** pops up the selected session's screen and its pending question without
  switching: `y`/`a`/`n` answer a permission; typing writes a reply that `⏎` sends; `⏎` with
  nothing typed opens the session.
- **`i` (inbox):** one tab per workspace plus All (`Tab` switches), newest first, unread marked.
  `j`/`k` move; `⏎` opens the session, which marks its entries read.
- **`e`** explains the status (the engine's ring).
- **Palette:** `session.resume`, `session.hibernate`, `session.mute`, `notify.test`.
- **Dormant panes** show a resume card; `⏎` (or focus, with `resume_on_focus`) resumes.
- The TUI reports its terminal's focus (mode 1004) with `Focus`, so watching only counts while
  you're actually looking.

### 11.2e What M5 added **[built M5]**

- **Broadcast:** `Space` in the sidebar marks a session (`◆` replaces its number), and on a
  project or worktree row it still folds. `b` (in the sidebar, or after the prefix) opens one
  prompt for every marked session. Each gets it once it's ready (`wait_ready_ms`, never typed
  into a dialog), and the marks clear.
- **Tour:** `ptyro tour` (palette `help.tour`, and offered at the end of `ptyro init`) is five
  pages of keys, generated from the user's own key map:
  1. welcome;
  2. sessions;
  3. who needs you;
  4. panes;
  5. from scripts.

  `→`/`⏎` next, `←` back, `q` leaves. With no sessions it starts a shell to try them in.
- `ptyro attach --workspace W` opens a workspace's own view.

### 11.2f What M6 added **[built M6]**

- **The inspector** (`D` after the prefix, or in the sidebar) covers the panes for one worktree
  (the selected row's, else the focused session's). Five tabs (`1`–`5`, `Tab`):
  - **Diff:**
    - a file list (marks: `◆` staged for commit, `💬N` comments) next to the hunks;
    - unified with old/new line numbers, or side by side (`s`);
    - against the base, the upstream, the last turn or HEAD (`a` cycles);
    - keys: `j/k` lines, `n/p` files, `c`/`⏎` comment on the cursor line, `x` drop it, `S` send
      all comments to the agent, `Space` mark a file, `C` commit the marked files (or
      everything) with a message, `A` ask the agent to commit, `P` push and open a (draft) PR,
      `I` integrate (asks first).
  - **Commits** since the base.
  - **Checks:** the PR, its review state and each check. `f` sends the failing logs to the
    agent, `m` turns on auto-merge, `w` watches.
  - **Ports:** the port block and what listens, as URLs.
  - **Events:** the worktree's sessions' persisted events as one timeline (status transitions
    coloured, hooks, prompts, deliveries).

  Syntax highlighting (`syntect`) is not built: optional in the proposal, and left out for
  binary size.
- **The review queue** (`Q` after the prefix): the ranked list. `⏎` opens the inspector on it,
  `d` marks it reviewed.
- **Sidebar:** PR badges on worktree rows (§10.9).

### 11.2g What M7 added **[built M7]**

- **The runs view** (`O` after the prefix, or in the sidebar) covers the panes, like the
  inspector:
  - **Runs:** every run, newest first: kind, name, status, step counts, open gates (`⏸`), the
    error. `⏎` opens one, `c` compares, `x` cancels (asks), `F` starts a fan-out (agents, then the
    prompt) in the focused session's project.
  - **A run:** its steps in recipe order with status, attempts, bounces (`↩`), the session, the
    diffstat, exit codes and why it ended; open gates with numbered answers. `1`–`9` answer a
    gate, `n` answers with a note, `⏎` shows the step's session, `D` inspects its worktree,
    `r` retries a step, `x` cancels.
  - **Compare:** the candidates ranked, each with its evidence and reasons, the agent's last
    message, the base's test result, warnings, and the diff between the top two. `m` merges,
    `C` cherry-picks, `p` opens a PR (each asks first), `d` discards one, `X` discards the
    rest, `i`/`I` sends a follow-up to one/all, `R` re-runs the evidence, `D` inspects.
- **Gates** announce themselves (a toast, the inbox, notifications). An inbox entry for a gate or
  a run opens that run. The status bar shows `⏸ N gate(s)`.
- **Verified-done** in the sidebar: `✔` verified, `◌` checking, `↩N` sent back N times, `✘`
  claimed only (the check still fails).
- **The morning digest:** on attach, a toast sums up run updates and waiting gates since you
  last read the inbox.
- The tour has a "many agents" page.

### 11.3 Default key map (after prefix) **[built M2; differs from the proposal where noted]**

**[built M2]** The real table is generated: `C-Space ?` lists it and the palette shows each
action's key. Differences from the proposal below:
- `c` closes a pane (`x` stops a session).
- `C-r` restarts a session (`r`/`R` stay free for run scripts in M3).
- `C-l` redraws.
- `,` renames, `=` takes control, `` ` `` shows the previous session in this pane, `\` toggles
  the sidebar, `]` pastes the last yank.
- In the sidebar: `j/k/g/G` move, `⏎` opens, `x/X/R/,` act on the selected session.
- `[keys]` in config rebinds keys (`"key" = "action [arg]"`, `"global:key"`, `"nav:key"`, `"none"`
  unbinds); unknown actions or keys are reported by `ptyro config check` and as a toast.

**[built M3]**
- `n` is the new-session wizard (§11.2c); a typed command line is the palette's
  `session.command`.
- `N`, `w`, `o`, `r` and `R` work as listed.
- In the sidebar, `⏎`/`Space` fold or open, `h`/`l` fold and unfold, and `x` on a worktree row
  removes it (with confirmation and the safety checks).

**[built M6]** `D` opens the inspector (§11.2f), in the sidebar too; `Q` is the review queue.

**[built M7]** `O` opens the runs view (§11.2g), in the sidebar too.

**[built M4]** `a`, `i` and `p` work as listed. `e` explains the focused session's status. In the
sidebar, `a`/`i`/`p`/`e` work without the prefix.

| Key | Action |
|---|---|
| `?` | help overlay (searchable) |
| `:` / `Ctrl-p` | command palette (every command, fuzzy) |
| `n` | new session (picker: agent → target worktree/new worktree → name → optional prompt) |
| `N` | new worktree (+ agent) wizard |
| `j` / `k`, `↑` / `↓` | next / previous session in sidebar order |
| `1`…`9` | focus session by sidebar index |
| `a` | jump to next session needing attention |
| `p` | **peek**: popup with the last screen + pending question of the selected session; reply inline without switching |
| `i` | inbox |
| `/` | fuzzy find session/worktree/project |
| `Tab` | toggle focus sidebar ↔ pane |
| `s` / `v` | split horizontal / vertical with a picked session |
| `h`/`l` (in split) | move focus between panes |
| `z` | zoom pane |
| `[` | copy/scroll mode (vi keys, `/` search, `y` yank via OSC 52, `]` / `[` prompt jumps via OSC 133) |
| `D` | diff inspector for the focused session's worktree (M6) |
| `o` | open worktree in editor (picker of configured editors; `nvim` opens *inside ptyro* as a session) |
| `r` | run default run script; `R` picker |
| `x` | stop session (confirm); `X` kill |
| `b` | broadcast prompt to selected sessions (multi-select with `space` in sidebar) |
| `w` | workspace switcher (shows each workspace's profile label and attention badges) |
| `S` | change-set view (§15.5.10), when the focused session belongs to a set |
| `d` | detach (TUI exits, everything keeps running) |
| `q` | quit TUI (same as detach; never kills sessions) |

Mouse: click to focus panes/sidebar items, wheel scrolls scrollback (unless the child grabbed the
mouse), drag to select (copy mode), drag split borders.

### 11.4 Rendering

- Client keeps a per-session cell cache from `ScreenFrame`s and paints runs into the ratatui
  `Buffer` for the pane's rect; letterboxes/crops per §5.8.
- The TUI redraws only on frame arrival / UI event; no polling loop. Target ≤ 1 ms per frame paint
  for a 200×60 pane.
- Wide chars/graphemes: the cache stores clusters; the painter uses `unicode-width` consistent with
  the daemon's emulator to avoid drift.
- Colours: truecolor passthrough; downsampled to 256/16 when the outer terminal lacks support.
- Themes: built-in `ptyro-dark`, `ptyro-light`, `high-contrast`; user themes in config.

### 11.5 Overlays and views

- **Peek** (above), **Inbox**, **Command palette**, **Help**.
- **New session/worktree wizard** (fast path: `n`, `Enter`, `Enter` = Claude in a new worktree of
  the current project with a random name).
- **Inspector** (M6): Diff (file list + side-by-side/unified hunks, syntax highlighted via
  `syntect` optional), Commits, Checks, Ports, Events (status timeline — "why is it blocked?").
- **Fan-out compare** (M7): N worktrees side by side: diffstat, test result, agent summary, pick.

---

## 12. The CLI

### 12.1 Conventions **[decided]**

- `ptyro` with no args: attach the TUI (auto-start the daemon). **[built M1]** It attaches to the
  newest live session, else the newest one, else starts a shell session.
- **[built M1]** Attaching from inside a ptyro session is refused unless `PTYRO_ALLOW_NESTED=1`
  (a nested TUI fights over the prefix key).
- All listing/query commands support `--json` (stable schemas, documented in `docs/cli-json.md`)
  and `--format` templates later.
- Addresses per §2.3. Multi-target commands accept `@blocked`, `@all`, etc.
- Destructive commands ask for confirmation on a TTY; `--yes` for scripts.

### 12.2 Command tree **[proposed]**

```text
ptyro                                  # TUI
ptyro attach [<session>] [--raw] [--view-only] [--workspace <ws>]
ptyro new [--agent <a>[@<profile>]] [--profile <pr>] [--project <p>] [--wt <name>|--new-wt [<name>]|--here|--cwd <dir>]
          [--name <n>] [--prompt <text>|--prompt-file <f>] [--attach] [--override-pin] [-- <args…>]
ptyro ls [--status <s>] [--project <p>] [--all] [--json]
ptyro status [<session>] [--explain] [--json]
ptyro send <session…> <text> [--no-enter] [--raw] [--wait-ready]
ptyro read <session> [--screen|--scrollback [N]] [--plain|--ansi] [--json]
ptyro wait <session…> --for working|blocked|idle|done|exit|any-attention [--timeout 10m] [--any|--all]
ptyro events [--follow] [--session <s>] [--json]       # NDJSON stream of domain events
ptyro stop|kill|restart|resume|hibernate <session…>
ptyro rename <session> <name>
ptyro replay <session> [--speed 2]      ;  ptyro export <session> --asciicast|--text [-o file]

ptyro ws ls|new <name> [--profile <pr>] [--notify all|badge|mute]|rm <name>|rename <a> <b>
         |add <project>|remove <project>|set <name> [--profile <pr>] [--notify …]
ptyro profile ls|show <pr>|new <pr> [--adopt <agent>=<dir>…]|rm <pr>|rename <a> <b>
              |login <pr> <agent|gh>|env <pr> [--agent <a>]|sync <pr>|doctor [<pr>]
ptyro project add [<path>]|ls|rm <p>|show <p>|trust <p>|config <p>|pin <p> <profile>|unpin <p>
ptyro wt ls [--project <p>]|new [<name>] [--base <ref>] [--branch <b>] [--pr <n>] [--setup run|skip]
         |rm <wt> [--force] [--keep-branch]|status <wt>|diff <wt>|open <wt> [--editor <e>]
         |run <wt> [<script>]|lock <wt>|unlock <wt>|rename-branch <wt> [<new>]
ptyro fanout --agents claude,codex --prompt <p> [--base <ref>] [--name <prefix>] [--wait]
ptyro broadcast <session…> <text>
ptyro run ls|show <run>|cancel <run>|gate <run> <gate> approve|reject
ptyro recipe ls|run <recipe> [--var k=v]

ptyro init                             # first-run onboarding (§12.3)
ptyro doctor                           # diagnose: agents installed/authenticated, terminal caps,
                                       #   socket perms, git version, hook integration health
ptyro config path|edit|check|show [--effective]
ptyro daemon start [--foreground]|stop [--force|--hibernate]|restart|status|logs [-f]
             |upgrade|install-service|uninstall-service
ptyro gc [--dry-run]
ptyro completions bash|zsh|fish        # dynamic: completes live session/worktree names
ptyro shell-init bash|zsh|fish         # optional OSC 7/133 snippets
ptyro hook <adapter> <event>           # internal; used by agent integrations
ptyro proxy                            # internal; stdio⇄socket bridge for ssh remote (§16.2)
ptyro mcp                              # MCP server exposing ptyro to agents (§15.4)
```

**[built M2]** Added: `attach --view-only`, `restart S…`, `ls --all` (history from the store,
including earlier daemon runs), `read S --scrollback [N]`, multi-target selectors for
`kill`/`rm`/`restart`/`send`, `config path|check|show|edit` (`check` also validates key bindings,
action names and the theme; `edit` starts from a commented template and re-opens on errors), and
`shell-init bash|zsh|fish`.

**[built M1]** `new [-d] [-n NAME] [-C DIR] [--size CxR] [--json] [-- CMD…]`, `attach [S] [--raw]`,
`ls [--json]`, `kill S… [--signal N] [--keep]`, `rm S…`, `rename S NAME`, `read S [--ansi]`,
`send S TEXT [--no-enter]`, `daemon start [--foreground]|stop [--force]|status [--json]|logs [-n N]`,
the hidden `fake-agent` (§18.2), and the global `--json-wire`. `kill` waits for the graceful stop
and then removes the session unless `--keep`. Still to come from this tree: `--view-only`,
`status`, `wait`, `events`, and everything project- or worktree-related.

**[built M3]**
- **Commands:**
  - `project add|ls|rm|show|trust|untrust|config|pin|unpin|discover`;
  - `ws ls|new|set|rm|rename|add|remove`;
  - `wt ls|new|rm|status|diff|run|open|lock|unlock|rename-branch`;
  - `profile ls|show|new|rm|rename|env|login|sync|detect|doctor`;
  - `gc [--dry-run]`.
- **`new`** gains `--agent A[@P]`, `--profile`, `--wt`, `--new-wt [NAME]`, `--project`,
  `--override-pin` and `--yes`.
- **`ls`** shows WHERE (the worktree, else `~`-shortened cwd), PROFILE (when any session isn't
  `default`) and ports.
- **Addresses** for projects and worktrees default to the one containing the current directory.
- **`wt new`** on a TTY offers to trust untrusted scripts and run setup. `wt rm` on a refusal with
  `--force` shows the report and asks (unless `--yes`).

**[built M4]**
- **Commands:**
  - `status [S] [--explain] [--json]`;
  - `answer S allow|always|deny`;
  - `resume S…` and `hibernate S…`;
  - `inbox [--workspace W] [-u] [--read] [--json]`;
  - `mute TARGET [--for D] [--off]`;
  - `usage [--json]`;
  - `notify test`;
  - `integrations ls|install|uninstall gemini|antigravity`;
  - `report --state … [--kind] [--msg] [--progress]` (alias `report-agent`).
- **Hidden:** `hook` and `statusline-relay`. They run before any tokio runtime starts.
- **`new`** gains `--prompt TEXT|@snippet` and `--prompt-file F`. The prompt is delivered once
  the agent is ready (§8.2), and `@name` comes from the project's `[prompts]`.
- **`send --raw`** writes bytes as-is (no paste brackets, no Enter).
- **`daemon stop`** returns only once the daemon's lock is released (its pid can linger as a
  zombie).

**[built M5]** The parity pass against the tree above:
- **Scripting:**
  - `send S… TEXT [--no-enter] [--raw] [--wait-ready[=2m]] [--wait [--timeout] [--stall]] [--json]`;
  - `broadcast S… TEXT [--timeout]`;
  - `read S [--screen|--scrollback [N]] [--plain|--ansi] [--json]`;
  - `wait S… [--for C] [--match RE [--scrollback]] [--timeout] [--any|--all] [--stall] [--json]`;
  - `events [--follow] [-s S] [-n N] [--json]`;
  - `stop S…` (graceful, kept listed; `kill` also removes).
- **Listing:** `ls --status/--project/--workspace`; `attach --workspace`.
- **Daemon:** `daemon stop --hibernate`, `daemon restart`, `daemon logs -f`,
  `daemon install-service|uninstall-service [--dry-run]`.
- **Setup and help:** `completions`, `init [--yes] [--dry-run]`, `doctor [--quick] [--json]`,
  `profile doctor [P] [--json]`, `tour`.
- **More `--json`:** `config show|check`, `gc`, `profile env`, `integrations ls`,
  `project config [--explain]`. Every shape is documented in `docs/cli-json.md` and pinned by a
  snapshot.
- **`wait` conditions:**
  - `idle` (idle or done) and `done` require a status *newer than the last prompt ptyro typed*,
    and no prompt waiting to be typed. That is why `new --prompt` followed by `wait --for idle`
    works.
  - `blocked` counts at any time.
  - `any-attention` is blocked, or an attention raised after the last prompt.
  - `exit` is "not running".
  - `--stall D` reports `stalled` when a working session's screen hasn't changed for `D`.
  - `send --wait` waits for idle, blocked, errored or exit, and prints how the turn ended.
  - Events wake the wait at once; a 250 ms tick covers `--match` and stalls.
- **Completions** use clap_complete's dynamic engine (`=4.6.11`, feature `unstable-dynamic`,
  pinned exactly):
  - `ptyro completions SHELL` prints a registration script that calls `PTYRO_COMPLETE=<shell>
    ptyro -- …` back for every completion;
  - session, worktree, project and workspace names come live from the daemon over a blocking
    socket with a 400 ms budget, so a completion never starts a daemon;
  - agents, `agent@profile` and profiles come from the config.
- **`doctor`** checks:
  - config validity;
  - the daemon's version, protocol, socket-dir permissions and leaked account variables;
  - agents found on your PATH but not on the daemon's (a service manager's PATH);
  - warnings the daemon collected (a Codex "hooks need review" screen, a config that failed to
    load);
  - the **hook self-test**: `fake-agent --as claude|codex|opencode` runs through the real
    per-launch integration. It passes only if the turn's `working` came from hooks or the
    sidecar; for OpenCode it waits for the event stream first.
  - terminal (TERM, truecolor, kitty keyboard via a query);
  - tmux `allow-passthrough`/`extended-keys`/`set-clipboard`;
  - git (warns below 2.40, the documented requirement) and gh;
  - agent versions against the ones verified;
  - per profile: logins by each tool's own command (`claude auth status --json`, `codex login
    status`, `gh auth status`; Gemini by its credential file), git identity, SSH key; then pins
    that contradict `match`.

  A `problem` exits 1.
- **`install-service`:**
  - **systemd:** writes `~/.config/systemd/user/ptyro.service` with
    - `ExecStart=<ptyro> daemon start --foreground`;
    - this shell's `PATH`, `PTYRO_HOME` and XDG variables;
    - `KillMode=mixed`, so stop is a SIGTERM to the daemon alone and agents hibernate;
    - `Restart=on-failure` with `RestartPreventExitStatus=5` (5 = "already running").

    Then `daemon-reload` and `enable`, plus `start` only if no daemon runs.
  - **launchd:** `~/Library/LaunchAgents/dev.ptyro.daemon.plist` with `RunAtLoad` and
    `KeepAlive{SuccessfulExit=false}`, loaded by `launchctl bootstrap gui/<uid>`.
  - **Uninstall** never stops a running daemon.
- **Not built (later milestones):**
  - `replay`/`export` (recorder, M8, now built: §7.6);
  - `fanout`, `run`, `recipe`, `mcp` (M7, now built: see below);
  - `proxy` (M9);
  - `daemon upgrade` (M8, now built: §4.5).

  `new --here`/`--attach` were dropped: `new` attaches unless `-d`, and runs in the current
  directory unless `--wt`/`--new-wt`.

**[built M6]**
- **Worktree commands:**
  - `wt diff [W] [--against base|upstream|last-turn|head] [--stat] [--json]` (now from the
    daemon: untracked files included, renames detected);
  - `wt log [W] [--json]`;
  - `wt commit [W] -m MSG [--all] [-- FILES…] | --ask-agent`, which commits as the worktree's
    profile (its git identity);
  - `wt integrate W [--into B] [--cherry-pick] [--push]`;
  - `wt pair W [--refresh]` and `wt unpair W`;
  - `wt new --issue N|URL` and `--prompt`. `--pr` takes a URL too.
- **`new`:** `new --pr N|URL` and `new --issue N|URL` imply a new worktree. `--unqueued` skips
  the review gate.
- **`pr`:**
  - `pr create [W] [--draft|--ready] [--title] [--body] [--base]`;
  - `pr status [W] [--refresh] [--json]`;
  - `pr checks [W] [--json]` (exit 1 when a check fails, 7 while any runs);
  - `pr fix [W]`;
  - `pr automerge [W] [--method] [--off]`;
  - `pr watch [W] [--off]` (also `watch-pr`);
  - `pr reply [-w W] [--comment ID] TEXT`;
  - `pr open N|URL`.
- **`review`:** `review add W FILE:LINE TEXT`, `review ls [W] [--json]`,
  `review rm W ID|--all`, `review send [W] [--to S] [--note]`, `review queue [--json]`,
  `review done W`.

**[built M7]** (shapes in `docs/cli-json.md`; `@last` names the newest run):
- **Runs:** `run ls [--active] [--tree] [--json]`, `run show R [--json]`, `run events R [--json]`,
  `run wait R [--gate] [--timeout] [--json]` (exit 0 succeeded, 1 failed, 5 cancelled, 7 timeout),
  `run cancel R`, `run retry R STEP`, `run fork R --from STEP [--set k=v]…`,
  `run lead S [--max-workers N] [--agent A]…`.
- **Recipes:** `recipe ls [--project] [--json]`, `recipe show NAME`, `recipe validate FILE`
  (every mistake, exit 2), `recipe graph NAME|FILE [--format mermaid|dot]`,
  `recipe run NAME|FILE [--var k=v]… [--project] [--base] [--trust FP] [--wait] [--json]`.
  On a terminal an untrusted repo recipe asks first.
- **Fan-out:** `fanout --agents a,b,c | -n N [--agents a] (--prompt P | --prompt-file F)
  [--project] [--base] [--name PREFIX] [--test [CHECK]] [--verify CHECK] [--judge AGENT]
  [--timeout D] [--wait] [--json]`; `compare R [--refresh] [--json]`;
  `pick R CANDIDATE [--cherry-pick|--pr|--apply] [--into B|W] [--path F]… [--hunk F:N]…`;
  `discard R [CANDIDATE] [--force]`; `iterate R [CANDIDATE] TEXT`.
- **Gates:** `gate ls [--all] [--json]`, `gate resolve ID CHOICE [--note] [--key]`.
- **Mailbox:** `msg send TO TEXT [--kind] [--run] [--reply-to] [--interrupt] [--key]`,
  `msg check [--me S] [--since ID] [--wait D] [--json]`, `msg ls [--run] [-n] [--json]`,
  `msg ack ID…`. Inside a session the sender is that session.
- **Leases and radar:** `lease claim PATTERN… [--exclusive] [--ttl] [--reason] [--as S]`
  (exit 5 when refused), `lease release [PATTERN…] [--as S]`, `lease ls [--json]`,
  `radar [--project] [--json]` (exit 1 when a merge would conflict).
- **Checkpoints:** `checkpoint ls [S|W] [--json]`, `checkpoint diff S N [M] [--stat]`,
  `checkpoint restore S|W [N] [--turn T] [--yes]` (alias `ckpt`).
- **Agents:** `verify S CHECK [--max-bounces N] | --off`; `new … --verify CHECK`,
  `new … --lead [--max-workers N] [--lead-agent A]…`; `mcp` (stdio MCP server, §15.6).
- **`pr create`:**
  - pushes with `--set-upstream` under the profile (its SSH key);
  - takes the title from a single commit's subject, else the worktree name;
  - builds the body from the commit list, plus a marker `<!-- ptyro:worktree=<id>
    session=<id> -->` (R-GIT-03: `pr open` finds the worktree from a PR);
  - refuses a branch with no commits beyond its base.
- **`--issue`:** `gh issue view N --json number,title,body,url`, then name `issue-N`, branch
  `issue-N-<title slug>`, and (for an agent with no prompt) a first prompt carrying the issue,
  ending with "commit with `Fixes #N`".
- **Diffs** come from `git diff -M --unified=3 <from>` against the working tree, plus untracked
  files (`ls-files --others --exclude-standard`, ≤ 256 KiB each, binary detected), capped at
  20 000 lines (`truncated`). `from` depends on `--against`:
  - `base`: the merge base with the worktree's base;
  - `upstream`: `@{upstream}`;
  - `head`: `HEAD`;
  - `last_turn`: a `git stash create` snapshot taken when the agent's turn started (no index,
    stash-list or file is touched), keeping only untracked files written during the turn.
- **Line comments** are kept by the daemon per worktree (in `kv`, so a TUI restart keeps them),
  with the line's text.
  - `review send` batches them into one prompt: "[via ptyro: review of your changes in W]",
    then a numbered `file:line` list with the code and the comment.
  - It is delivered only once the agent's turn is over (not working, input box ready), never
    into a dialog. Then the comments clear and the worktree counts as reviewed.
- **Integrate** refuses while an agent in the worktree is working, or when the worktree is
  dirty (exit 5).
  - Rebase onto the local target branch (aborted and reported on a conflict).
  - Then fast-forward the target: with `merge --ff-only` where it's checked out (the main
    checkout), else by moving the branch, only forward.
  - `--cherry-pick` applies `merge-base..branch` where the target is checked out; that checkout
    must be clean.
  - `--push` pushes the target.
- **Pair mode** (with stash safety):
  - **`pair`:**
    1. stashes the main checkout's own uncommitted work (`stash push --include-untracked`,
       remembered by commit id);
    2. remembers its branch;
    3. checks out the worktree's commit (detached);
    4. applies the worktree's uncommitted changes as a binary patch;
    5. copies its new files.
  - **`refresh`** first carries the user's edits from the main checkout to the worktree: a
    patch against the synced snapshot, new files, and copied files that changed. Then it
    re-syncs.
  - **`unpair`:**
    1. carries edits back the same way;
    2. resets, removing only the files ptyro copied and the ones carried over;
    3. checks out the original branch;
    4. applies the stash and drops exactly that stash entry.

    If the edits don't apply to the worktree, nothing changes and pair mode stays. If the stash
    doesn't apply, it stays in `git stash list` and ptyro says so.
  - One worktree per project at a time; the state lives in `kv` (`pairs`).

### 12.3 `ptyro init` (onboarding, quick setup)

1. Detect terminal capabilities (truecolor, kitty keyboard, OSC 52, running inside tmux → suggest
   `allow-passthrough on` and `extended-keys on`).
2. Detect agent CLIs (`claude`, `codex`, `gemini`, `opencode`) and versions; flag unsupported
   versions; check authentication hints (without reading secrets).
3. Detect existing multi-account setups (`CLAUDE_CONFIG_DIR`/`CODEX_HOME` exports and aliases in
   shell rc files, extra `~/.claude-*`/`~/.codex-*` dirs, git `includeIf` identities) and offer to
   turn each into a profile and workspace (§10.10).
4. Discover git repos under common roots; multi-select to register as projects; suggest a pin for
   each from the profiles' `match` rules.
5. Choose prefix key, worktree location, notifications.
6. Write `~/.config/ptyro/config.toml` with comments; offer to install shell completions and the
   systemd/launchd service.
7. Offer a 60-second guided tour in the TUI.

**[built M5]**
1. **Terminal:** the doctor's terminal and tmux checks.
2. **Agents:** the doctor's version checks.
3. **Accounts:** `detect()` suggestions not already defined, each confirmed.
   - Detection now joins multi-line function bodies (`name() {…}`, `function name … end`) and
     reads fish `set -x`/`-gx`/`-lx`.
   - `${HOME}` is normalised before parsing.
   - A suffix-less alias (`cw`) is named after its account dir (`~/.claude-work` → `work`).
   - Golden tests: `crates/ptyro-agents/tests/fixtures/rc/*` (zsh aliases, zsh functions, bash
     exports, fish).
4. **Preferences:** prefix, worktree location, desktop notifications.
5. **The config:** only keys you don't have are added, via `toml_edit` (comments kept). The
   additions are listed, then written on a yes. `--dry-run` prints the result and stops.
6. **Projects:** discovered repos are listed by number (`1,3-5`, `all`, `none`).
7. **Extras:**
   - completions: offered for your shell's rc file; appended only on an explicit yes;
   - the service: offered;
   - the tour: offered.

`--yes` accepts the suggestions but never registers repositories, edits rc files or installs the
service. Without a terminal and without `--yes`, `init` refuses (exit 2).

### 12.4 Exit codes

`0` ok · `1` generic error · `2` usage · `3` not found · `4` ambiguous · `5` conflict/dirty ·
`6` untrusted · `7` timeout (`wait`) · `8` daemon unavailable · `9` protocol mismatch.

**[built M5]** As above. `wait` exits 5 when a session exits (or is removed) before reaching the
state, and 7 on a timeout or `--stall`. `doctor` exits 1 on a problem. Tested in
`crates/ptyro/tests/m5.rs`.

---

## 13. Configuration

### 13.1 Layering **[decided]**

1. Built-in defaults.
2. User: `~/.config/ptyro/config.toml` (`$PTYRO_CONFIG` overrides).
3. Project (committed): `<repo>/.ptyro.toml`.
4. Project local (uncommitted, add to `.git/info/exclude`): `<repo>/.ptyro.local.toml`.
5. CLI flags / env.

Security rule: **agent commands, args and env, and profiles, can only be defined in user config.** Repo configs may
choose *which* configured agent is default and may set prompts/scripts/worktree settings, never
executables for agents (prevents a cloned repo from silently running arbitrary binaries as "claude").

Config is validated on load with precise errors (`ptyro config check`); the daemon hot-reloads on
change (debounced) and emits `ConfigReloaded`.

**[built M2]** The schema has `[ui]`, `[daemon]`, `[keys]` and `[themes.*]`.
- Unknown keys inside them are errors with line and column (`deny_unknown_fields`).
- Sections for later milestones (`[agents]`, `[profiles]`, `[worktrees]`, `[ports]`,
  `[discover]`, `[notifications]`, `[attention]`, `[recorder]`, `[editors]`, `[remote]`) are
  accepted and reported as "not used until Mn", so a config written from this SPEC never breaks.
- The daemon watches the config *directory* with `notify` (debounced 200 ms), so atomic-rename
  saves and a file created later both count.
- On a change it re-reads the file and broadcasts `ConfigReloaded { error }`. On error it keeps
  the old config.
- TUIs re-read the file and apply key bindings and theme live. New sessions use the new
  `scrollback_lines`, new subscriptions the new `frame_interval_ms`. `log_level` takes effect at
  the next daemon start.
- A broken config never stops the daemon: it starts on defaults and logs the error.

**[built M3]**
- **Typed sections now:** `[profiles.*]`, `[worktrees]`, `[ports]`, `[discover]`, `[editors]` and
  `[compat]`, plus `ui.show_ports` and `ui.tint_borders`.
- **`[agents.<name>]`** reads `command`, `args` and `default` now; the rest is still reserved for
  M4, as are `[notifications]`, `[attention]`, `[recorder]` and `[remote]`.
- **Project config** (`ptyro_core::project`): `.ptyro.toml` with `[project]`, `[worktree]`,
  `[scripts]` and `[prompts]`, deep-merged with the local files. `[agents]`, `[profiles]`, `[ui]`,
  `[daemon]` and `[keys]` in a repository are errors naming the rule.
- **Reload loop fix:** the daemon's config watch reloads only when the file's *bytes* changed.
  inotify reports opens, so the daemon reading the file, and every TUI re-reading it on
  `ConfigReloaded`, would otherwise ping-pong reloads forever (found in M3 testing; latent since
  M2).

**[built M4]**
- `[agents.<name>]` is fully typed: `adapter`, `command`, `args`, `env` (wins over the
  profile's), `default`, `hibernate_after`, `dangerous`, `trust_worktrees`, `hooks` (default
  true), `usage`, `after_turn`.
- `[attention]`: `scope`, `breakthrough`, `stale_after`, `min_work`, `resume_on_focus`.
- `[notifications]`:
  - `channels` (default `inbox, osc, bell`);
  - `on` (default `blocked, errored, done, idle`);
  - `quiet_hours = "22:00-07:00"`, `coalesce`, `desktop_command`;
  - `[[notifications.sinks]]` with `type = ntfy|pushover|slack|discord|telegram|command` and a
    per-sink `on`;
  - `[notifications.callback] listen/public_url/link_ttl/insecure`.
- `[daemon] restore_on_start`, `[worktrees] auto_rename_branch` (a branch without a name is
  renamed from the first prompt), and `[prompts]` snippets.
- Only `[recorder]` (M8) and `[remote]` (M9) remain reserved.
  **[built M8]** `[recorder]` is typed now (`enabled = true`, `max_bytes = "16MiB"`, at least
  256 KiB), and agents take `record = false`. Only `[remote]` is still reserved. The default
  `[daemon] scrollback_lines` is 5000 (§7.6).
  **[built M9]** `[remote]` is typed: `listen = "127.0.0.1:7091"`, `public_url`, `serve = false`,
  `insecure = false`, `approval_wait = "10m"`, `push = true`, `push_hosts = []` (push services a
  device may subscribe with besides the browsers': FCM, Mozilla, Apple, Windows). `trusted_proxy
  = false` (believe a loopback peer's `X-Forwarded-For`; on by itself under `tailscale serve`). No section is
  reserved any more.
  `[clipboard] allow_write = true` (OSC 52 writes from programs reach the TUI's terminal;
  read by the TUI, so it follows that machine's config).
  **[built M10]** `[agents.x] protocol` (`stream-json`, `app-server`, `acp`, `opencode`) makes
  `--structured` speak that protocol to the agent.

**[built M5]** `.ptyro.toml [project] import = false` turns off other tools' config (§10.5).
`ptyro init` writes `ui.prefix`, `worktrees.location`, `notifications.channels`,
`discover.roots` and `[profiles.*]`, and only when they're absent.

**[built M6]**
- **`[github]`:**
  - `poll = "60s"`;
  - `draft = true`;
  - `merge_method = "squash"` (`squash`, `merge` or `rebase`);
  - `watch = true` (PRs ptyro opens are watched);
  - `escalate_after = "2h"`;
  - `log_lines = 80`.
- **`[review]`:** `max_unreviewed = 0` (off) and `digest = "30m"` (`"off"` for never).

**[built M7]**
- **`[orchestration]`:** `checkpoints = true` (a checkpoint per agent turn that changed its
  worktree), `lease_ttl = "30m"`, `radar = true` (collision messages), `message_ttl = "24h"`.
- **`notifications.on`** gains `gate` and `run` by default; `attention.breakthrough` gains
  `gate`.
- **Recipes:** your own in `~/.config/ptyro/recipes/*.toml`; a repository's in
  `.ptyro/recipes/*.toml`, which need trust (scope `recipe:<name>`, fingerprint = the recipe's
  sha).
- **`.ptyro.toml [verify]`:** `check = "<run script name>"`, `max_bounces = 3`, `auto = false`
  (verify every agent in the project's worktrees). `check` names a run script, so it's covered by
  the scripts' trust, which pins the command text only (§10.5).

### 13.2 User config (example, all keys optional)

```toml
[ui]
prefix = "C-Space"
theme = "ptyro-dark"
sidebar_width = 30
show_ports = true
confirm_quit = false

[daemon]
scrollback_lines = 5000          # in memory; older lines come from the recording (§7.6)
restore_on_start = "dormant"     # dormant | resume | none
frame_interval_ms = 8
log_level = "info"

[recorder]
enabled = true
max_bytes = "16MiB"

[notifications]
channels = ["inbox", "osc", "bell"]      # + "desktop", later "push"
on = ["blocked", "errored", "done", "idle"]
quiet_hours = "22:00-07:00"
min_work = "10s"

[worktrees]
location = "data"                         # data | sibling | in-repo
branch_template = "ptyro/{name}"
names = "corvid"                          # word list for friendly names

[ports]
range = "20000-29999"
count = 10

[discover]
roots = ["~/dev", "~/src"]

[agents.claude]
adapter = "claude"
command = "claude"
args = []
env = {}
default = true
hibernate_after = "off"
# dangerous = true is required to allow args like --dangerously-skip-permissions;
# sessions of such agents show a ⚠ badge.

[agents.codex]
adapter = "codex"

[agents.gemini]
adapter = "gemini"

[agents.opencode]
adapter = "opencode"

[agents.aider]                             # generic: any CLI
adapter = "generic"
command = "aider"

[profiles.work]                            # full reference: §10.10
label = "work"
color = "blue"
claude = "~/.claude-work"
codex = "~/.codex-work"
git = { email = "you@acme.example", ssh_key = "~/.ssh/id_work" }
match = { remotes = ["github.com:acme/*"] }
guard = "confirm"                          # confirm | refuse | warn

[attention]
scope = "current-first"                    # current-first | current | global (§9.6)

[editors]
default = "nvim"
nvim = { command = ["nvim", "{path}"], inside = true }   # opens as a ptyro session
code = { command = ["code", "{path}"] }
zed = { command = ["zed", "{path}"] }

[keys]                                     # overrides, after-prefix unless "global:"
"a" = "attention.next"
"global:M-1" = "session.focus 1"

[remote]
enabled = false
```

### 13.3 Project config `.ptyro.toml` (example)

```toml
[project]
name = "myrepo"
default_agent = "claude"
base = "origin/main"

[worktree]
copy = [".env", "config/local.yml"]
share = ["node_modules"]
share_mode = "clone"        # clone | symlink | copy

[scripts]
setup = """
pnpm install --frozen-lockfile
cp -n .env.example .env
"""
archive = "docker compose down -v"
run_mode = "exclusive"
after_turn = "pnpm -s lint --fix"   # optional

[scripts.run.dev]
command = "pnpm dev --port $PTYRO_PORT"
default = true

[scripts.run.test]
command = "pnpm test"

[prompts]
# snippets offered in the new-session wizard and `ptyro new --prompt @name`
review = "Review the diff against origin/main and list risks."
```

---

## 14. Storage: files, directories, database

### 14.1 Paths **[decided]** (XDG on both Linux and macOS via `etcetera`; all overridable by
`PTYRO_HOME`, which puts everything under one directory — used by tests)

| Kind | Path | Contents |
|---|---|---|
| config | `~/.config/ptyro/` | `config.toml`, `themes/`, `recipes/` |
| data | `~/.local/share/ptyro/` | `ptyro.db`, `worktrees/`, `sessions/<id>/pty.log`, `integrations/`, `profiles/<name>/<agent>/` (managed profile dirs, `0700`) |
| state | `~/.local/state/ptyro/` | `daemon.lock`, `daemon.log*`, `daemon.prev` |
| runtime | Linux `$XDG_RUNTIME_DIR/ptyro/`; macOS `$TMPDIR/ptyro-<uid>/`; `PTYRO_HOME/run/` | `ptyro.sock` |

**[built M1]** If the socket path would exceed 100 bytes (macOS `sun_path` is 104; deep test
dirs), it falls back to `/tmp/ptyro-<uid>-<fnv32 of the original path>/ptyro.sock`, which is
deterministic, so every client finds it. The daemon refuses a socket directory owned by another
uid and tightens it to `0700` (a shared `/tmp` could otherwise be pre-seeded). `PTYRO_SOCKET`
overrides the socket path alone.

### 14.2 Database

SQLite (bundled), WAL, `foreign_keys=ON`, migrations embedded with `rusqlite_migration`
(`user_version`). Only metadata — never terminal contents.

```sql
CREATE TABLE workspace (id TEXT PRIMARY KEY, name TEXT UNIQUE NOT NULL, view JSON,
                        profile TEXT NOT NULL DEFAULT 'default',   -- name in user config
                        notify TEXT NOT NULL DEFAULT 'all',        -- all|badge|mute (§9.6)
                        quiet_hours TEXT, created_at INTEGER, updated_at INTEGER);
CREATE TABLE project (id TEXT PRIMARY KEY, slug TEXT UNIQUE NOT NULL, root_path TEXT UNIQUE NOT NULL,
                      default_branch TEXT, remote_url TEXT, profile TEXT,   -- pin (§10.10), nullable
                      created_at INTEGER, last_used_at INTEGER);
CREATE TABLE workspace_project (workspace_id TEXT REFERENCES workspace ON DELETE CASCADE,
                                project_id TEXT REFERENCES project ON DELETE CASCADE, position INTEGER,
                                PRIMARY KEY (workspace_id, project_id));
CREATE TABLE worktree (id TEXT PRIMARY KEY, project_id TEXT REFERENCES project, name TEXT NOT NULL,
                       path TEXT NOT NULL, branch TEXT, base TEXT, is_root INTEGER NOT NULL DEFAULT 0,
                       profile TEXT,                     -- fixed at creation; NULL for @root
                       state TEXT NOT NULL,              -- creating|ready|missing|removed|failed
                       port_base INTEGER, port_count INTEGER, created_at INTEGER, removed_at INTEGER,
                       UNIQUE (project_id, name));
CREATE TABLE session (id TEXT PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL, agent TEXT,
                      worktree_id TEXT REFERENCES worktree, cwd TEXT NOT NULL,
                      profile TEXT NOT NULL, workspace_id TEXT REFERENCES workspace,  -- §10.10, §9.6
                      launch JSON NOT NULL,                -- resolved LaunchPlan (argv, env diff; secrets as <secret:NAME>)
                      conversation_id TEXT, process_state TEXT NOT NULL, exit_code INTEGER,
                      pid INTEGER, run_id TEXT REFERENCES run, tags JSON,
                      created_at INTEGER, started_at INTEGER, ended_at INTEGER);
CREATE TABLE session_event (id INTEGER PRIMARY KEY, session_id TEXT REFERENCES session ON DELETE CASCADE,
                            at INTEGER NOT NULL, kind TEXT NOT NULL,   -- status|process|hook|attention
                            status TEXT, confidence TEXT, source TEXT, detail JSON);
CREATE INDEX session_event_by_session ON session_event(session_id, at);
CREATE TABLE inbox (id INTEGER PRIMARY KEY, session_id TEXT, at INTEGER, kind TEXT, title TEXT,
                    body TEXT, read_at INTEGER);
CREATE TABLE trust (project_id TEXT REFERENCES project ON DELETE CASCADE, scope TEXT NOT NULL,
                    fingerprint TEXT NOT NULL, approved_at INTEGER, PRIMARY KEY (project_id, scope));
CREATE TABLE run (id TEXT PRIMARY KEY, kind TEXT, spec JSON, state TEXT, created_at INTEGER, ended_at INTEGER);
CREATE TABLE changeset_member (run_id TEXT REFERENCES run ON DELETE CASCADE,   -- §15.5 (kind = changeset)
                               project_id TEXT REFERENCES project, worktree_id TEXT REFERENCES worktree,
                               role TEXT NOT NULL,            -- lead | member
                               agent TEXT, session_id TEXT, depends_on JSON, overlay TEXT,
                               state TEXT, PRIMARY KEY (run_id, project_id));
CREATE TABLE changeset_checkpoint (run_id TEXT REFERENCES run ON DELETE CASCADE, n INTEGER,
                                   label TEXT, refs JSON,   -- {project: {head, snapshot}}
                                   created_at INTEGER, PRIMARY KEY (run_id, n));
CREATE TABLE run_message (id INTEGER PRIMARY KEY, run_id TEXT REFERENCES run ON DELETE CASCADE,
                          at INTEGER, sender TEXT, recipient TEXT, kind TEXT, body JSON, acked_at INTEGER);
CREATE TABLE kv (key TEXT PRIMARY KEY, value JSON);
```

Retention: `session_event` pruned after 90 days; `inbox` read entries after 30 days.

**[built M2] Store v1** (`ptyro-store`) ships the subset M2 needs, as migration 1 of `ptyro.db`:
- `session (id, name, kind, argv, cwd, state, pid, daemon_run, created_at, ended_at)`;
- `session_event (session_id, at, kind, detail)` for `process`, `status`, `rename` and `restart`;
- `kv (key, value, updated_at)`, holding `view:<name>`.

Each daemon run has an id. At start the daemon marks sessions left open by earlier runs as
`failed: daemon stopped`, because they really ended (resume is M4), and prunes: events after 90
days, ended sessions after 30. Writes are fire-and-forget on the store thread; reads await it. If
the database can't be opened, the daemon runs without history and saved views (it logs why), and
views fall back to memory. Later milestones add tables with new migrations; the full schema above
is the target.

**[built M3] Store v2** (migration 2) adds:
- `workspace`;
- `project` (`profile` = the pin);
- `workspace_project` (ordered membership);
- `worktree`: removed worktrees stay as history, so name uniqueness is a partial index `WHERE
  state != 'removed'`, and a `last_used_at` column feeds gc;
- `trust`;
- on `session`: `profile`, `worktree_id`, `project_id`, `workspace`, `agent`, `script` and
  `launch`.

The daemon keeps a `Registry` in memory and writes through, so the store stays optional. At
start it creates `main`, reconciles every project with `git worktree list --porcelain`
(re-adopting marked worktrees the store forgot), and starts the watchers.

**[built M4] Store v3** (migration 3) adds `session.conversation_id`, plus `inbox` (id, session,
workspace, kind, title, body, at, read), pruned to the newest 1000 entries. `close_orphans` now
makes resumable agents `dormant` (§7.5), and mutes persist in `kv`.

**[built M7] Store v4** (migration 4):
- `run` (id, kind, name, `recipe_src` + `recipe_sha` pinned, status, `state` = the JSON
  projection with the policy, project, lead, `forked_from`/`fork_step`, times);
- `run_event` (run, seq, at, kind, step, payload = a `RunEvent`), appended **in the same
  transaction** that updates `run.state`: the log is the truth, the projection a cache
  (`RunState::replay` rebuilds it);
- `run_message`, the mail ledger: one row per recipient with `queued`/`delivered`/`seen`/`acked`
  times, `hop`, `priority` and `expires_at`;
- `lease` (holder, pattern, project, exclusive, reason, expiry).

No `changeset_*` tables: a change set (M11) is a run whose projection carries its members, contract and train (§15.7.2).

---

## 15. Orchestration

Orchestration grows in three deliberate levels. All levels are **deterministic engines** driven by
explicit specs; "AI planning" is something an *agent* does using ptyro's tools, not something ptyro
does itself. **[decided]**

**[built M7]** §15.1–15.4 are the original design; what was built, with its schema and the
places it differs, is §15.6.

### 15.1 Level 1 — primitives (M5/M7)

`send`, `wait --for`, `broadcast`, `events`, `read`. Already enough for shell-script orchestration:

```bash
s=$(ptyro new --agent claude --new-wt --prompt-file task.md --json | jq -r .id)
ptyro wait "$s" --for idle --timeout 30m && ptyro wt diff "$s"
```

### 15.2 Level 2 — fan-out and compare (M7)

`ptyro fanout --agents claude,codex,gemini --prompt-file task.md`:

1. Creates a run; N worktrees from the same base (`<prefix>-claude`, `<prefix>-codex`, …).
2. Spawns each agent, delivers the prompt when ready.
3. Waits for each to reach `idle`/`done`/`errored`; optionally runs `scripts.run.test` per worktree.
4. Compare view: diffstat, tests pass/fail, time, agent's last message; actions: **pick** (merge or
   cherry-pick into a target branch, or open PR), **discard** (safe removal), **iterate** (send a
   follow-up to one or all).

### 15.3 Level 2b — recipes (M7)

Declarative TOML workflows in `~/.config/ptyro/recipes/` or `<repo>/.ptyro/recipes/`
(repo recipes require trust like scripts):

```toml
name = "implement-and-review"
vars = { task = { required = true } }

[[step]]
id = "impl"
spawn = { agent = "claude", worktree = "new", name = "impl" }
prompt = "Implement: {{task}}. Commit when tests pass."
wait = "idle"
timeout = "45m"

[[step]]
id = "test"
needs = ["impl"]
run = "scripts.run.test"
in = "impl"

[[step]]
id = "review"
needs = ["test"]
spawn = { agent = "codex", worktree = "impl" }   # same worktree, read-only intent
prompt = "Review the last commit on this branch. Reply with APPROVE or a list of issues."
wait = "idle"

[[step]]
id = "gate"
needs = ["review"]
gate = "Human: merge impl?"            # blocks until resolved in TUI/CLI/phone
```

The engine persists step state in `run`/`run_message`, so runs survive daemon restarts; a failed
step can be retried individually.

### 15.4 Level 3 — agents orchestrating agents (M7+)

`ptyro mcp` is an MCP server (stdio) that an agent session can be given (ptyro can inject it into a
"lead" session via `--mcp-config`). Tools:

`list_sessions`, `spawn_session(agent, worktree, prompt)`, `send(session, text)`,
`read_screen(session)`, `wait_for(session, status, timeout)`, `worktree_diff(worktree)`,
`post_message(run, to, kind, body)`, `check_messages(run, wait)`, `request_gate(run, question)`.

Mailbox semantics (an idea several orchestrators share): typed messages (`dispatch`, `status`,
`question`, `escalation`, `worker_done`, `heartbeat`), at-least-once delivery with acks, group
addresses (`@idle`, `@codex`, `@all`). **Policy limits** (max sessions per run, allowed agents,
allowed worktree targets, no destructive git ops) are enforced by the daemon, not trusted to the
agent.

### 15.5 Change sets — one change across several repos **[built M11; what was built, and where it differs, is §15.7]**

The owner regularly needs **one coordinated change across several repositories**: an API field
added in `api`, consumed in `web`, exposed in `sdk`, documented in `docs`. Agents can coordinate
that by hand, but nothing gives the work one name, one place, one status and one safe way to ship.
Most worktree tools stop at a single repo, for real reasons: git, branches, PRs and CI are
per-repo, an agent starts in one directory, and cross-repo commits can't be atomic. A change set
doesn't pretend otherwise. It gives the multi-repo change a lead, a contract, a shared timeline,
safe checkpoints and an explicit ship order, and is honest about partial states.

#### 15.5.1 Shape

```text
change set "add-user-tier"   (workspace: work · profile: work)
├─ lead       claude   · cwd = change-set root · sees every member, read-only by policy
├─ api        codex    · worktree api/   branch ptyro/add-user-tier   depends_on: —
├─ sdk        claude   · worktree sdk/   branch ptyro/add-user-tier   depends_on: api
└─ web        claude   · worktree web/   branch ptyro/add-user-tier   depends_on: api, sdk
root: data/changesets/add-user-tier/
      ├─ CHANGESET.md      generated brief: goal, members, roles, order, conventions
      ├─ CONTRACT.md       the cross-repo contract (owned by the lead, versioned by ptyro)
      ├─ api/ sdk/ web/    the members' worktrees (ordinary ptyro worktrees, §10)
      └─ .ptyro/           ledger, checkpoints, generated build overlay
```

- A change set is a **Run** of kind `changeset` (§15.3 storage, mailbox and gates reused). It
  lives in one workspace and therefore runs under one profile (§10.10). Every member must resolve
  to that same profile, or creation is refused: no accidental cross-account sets.
- **Members** are projects of that workspace. Each gets an ordinary worktree on the shared branch
  name, so every single-repo feature and safety rule (§10.4–§10.8) applies unchanged. Members
  may have different base branches (`api` off `main`, `web` off `develop`).
- The **default layout** is decided by the owner: **one lead plus one agent per member**. Each
  member agent's adapter comes from its project (`default_agent`), so `api` may run Codex while
  `web` runs Claude. A `solo` layout (one agent across all members) and `manual` (no agents; just
  the linked worktrees and lockstep git) are options.

#### 15.5.2 Roles and the rules the daemon enforces

| Role | Sees | May change | Talks to |
|---|---|---|---|
| **Lead** | every member (read), the ledger, CONTRACT.md | CONTRACT.md, CHANGESET.md; never member code | members (assign/answer/revise), the human (gates) |
| **Member** | its own worktree (rw) and its siblings (read) | only its own worktree | the lead (questions, ready, blocked) |
| **Human** | everything | everything | anyone; resolves gates |

Enforced, not just requested:
- **Write fences.** ptyro's injected `PreToolUse` hook (Claude, Codex) denies edit/write tools
  whose target resolves outside the agent's allowed paths. Denials are reported to the agent with
  the reason ("web may not edit api/; ask the lead"). Where an agent has no hook,
  the fence is advisory, and `cs status` flags any out-of-fence diff.
- **No silent shipping.** Agents may commit inside their worktree. Push, PR, merge and rebase of
  the *set* go through ptyro (`cs push|pr|land`), and by default through a human gate.
- **Policy limits** (§15.4) apply per set: max member agents, allowed adapters, turn and time budgets.

#### 15.5.3 The contract (the novel part)

Multi-repo changes fail at the seams, not inside repos. So the lead's first job is to write a
**contract** before any member writes code: the interfaces between members (endpoints and
payloads, schema migrations, event names, exported types, version bumps, feature flags), plus the
merge and deploy order.

- `CONTRACT.md` is versioned by ptyro: each save is a **revision** (r1, r2, …), stored in the
  ledger with its diff.
- **Gate 1: the human approves r1** before members are started. This is cheap to review and
  prevents most wasted work.
- Members get the approved contract as part of their brief and work against it.
- **Contract changes propagate.** When the lead (or a member, via a `contract_change` request to
  the lead) revises the contract, ptyro sends every affected member a `contract_revised` message
  with the diff. Members listed in the changed section's `affects:` get it as an interrupt; others
  get it at their next idle.
- **Contract checks (optional, per member):** commands that turn the contract into something
  verifiable. For example, `api` regenerates its OpenAPI spec and ptyro diffs it against the
  contract's schema block; `sdk` type-checks against the generated client. Failures are attached
  to the member's status (`⚠ contract drift`).

#### 15.5.4 Status-aware messaging (the ledger)

The mailbox from §15.4, made aware of agent status, since ptyro knows when each agent is working,
blocked or idle (§9):

- Typed messages: `assign`, `question`, `answer`, `contract_revised`, `ready_for_integration`,
  `blocked_on{member}`, `handoff`, `heartbeat`, `gate_request`.
- **Delivery waits for the right moment.** A message to a `working` member is queued and delivered
  as a prompt when it goes `idle`. `interrupt` messages are delivered at once (Esc, then the
  prompt), and `blocked` members are never auto-answered. Agents never get a message pasted into
  the middle of a turn.
- **Dependency-aware waiting:** a member that sends `blocked_on: api` is shown blocked *on api*,
  and the lead is told. When `api` posts `ready_for_integration`, the blocked member is woken
  automatically.
- Everything lands in an append-only **ledger** (`run_message`), which is the set's memory: who
  decided what, when, and why. `cs log` prints it; the TUI renders it as a timeline.

#### 15.5.5 The integration overlay ("build against my siblings")

What makes a change set more than N worktrees: members can **build and test against each other's
in-progress code** before anything is pushed. ptyro knows every member's path, so it generates the
wiring per ecosystem, opt-in per member, and never committed:

| Ecosystem | Overlay |
|---|---|
| Go | `go.work` in the root listing member modules |
| Rust | `.cargo/config.toml` `[patch]` path overrides in the consuming worktree (git-excluded) |
| Node | `pnpm.overrides` / npm `overrides` via a generated `.npmrc`/`pnpm-workspace.yaml` in the root, or `link:` rewrites in a git-excluded copy |
| Python | editable installs of sibling members into the consumer's venv |
| Containers | `compose.override.yml` mapping sibling worktrees as build contexts |

**Integration checks** are commands that run in the root against the overlay (`docker compose up
-d && pnpm e2e`), defined in the set template. Their results gate `ready_for_integration` and
`cs land`.

#### 15.5.6 Set checkpoints (multi-repo undo)

`cs checkpoint [label]` records a **consistent snapshot of the whole set**: each member's HEAD
plus a stash-like commit of its uncommitted work, under `refs/ptyro/cs/<set>/<n>` in each repo.
Checkpoints are also taken automatically before contract revisions and before `land`.
`cs restore <n>` returns *every* member to that snapshot, after checking that nothing newer would
be lost. One command to undo a multi-repo wrong turn.

#### 15.5.7 Shipping: lockstep git and the merge train

- **Commits** may be per member (agents) or set-wide (`cs commit -m`: one message, plus the
  trailers `Change-Set: add-user-tier` and `Change-Set-Members: api@a1b2c3 sdk@d4e5f6 web@0a9b8c`
  so the history links the repos together).
- **Push in dependency order** (`depends_on`, topologically sorted; cycles are refused when the
  set is created). A failure stops the train and reports exactly which members pushed and which
  didn't; `cs push` resumes. ptyro never pretends a partial push is atomic.
- **Linked PRs** (`cs pr`, M6 plumbing): one PR per member; each body carries a generated "change
  set" table with the sibling PRs, their CI states and the merge order. ptyro refreshes it as
  siblings change.
- **`cs land`, the merge train:** merges in order, waits for each member's checks (and the
  integration checks) before the next, and stops on failure with a precise partial-state report.
  Every step goes through a gate unless `--yes`.
- **Removal** (`cs rm`) applies §10.8 to *every* member; one dirty member blocks removal of the
  whole set (or `--keep <member>`).

#### 15.5.8 Templates

Recurring sets get a template in user config (`~/.config/ptyro/changesets/*.toml`) or in a
trusted "hub" repo:

```toml
name = "api-sdk-web"
members = [
  { project = "api", agent = "codex",  depends_on = [] },
  { project = "sdk", agent = "claude", depends_on = ["api"], overlay = "node" },
  { project = "web", agent = "claude", depends_on = ["api", "sdk"], overlay = "node" },
]
lead = { agent = "claude" }
gates = ["contract", "push", "land"]          # where the human must approve
integration = ["docker compose up -d", "pnpm -C web e2e"]
contract_checks = { api = "make openapi && ptyro cs contract diff openapi.yaml" }
```

`depends_on` can be suggested on creation by reading manifests (a sibling's package or module
name in another member's dependencies).

#### 15.5.9 Lead tools (via `ptyro mcp`, §15.4)

`cs_status`, `cs_assign(member, task)`, `cs_send(member, msg, interrupt?)`, `cs_contract_get`,
`cs_contract_revise(diff, affects)`, `cs_member_diff(member)`, `cs_integration_check`,
`cs_checkpoint(label)`, `cs_request_gate(question)`. There is deliberately **no**
`push`/`merge` tool: shipping is the human's train.

#### 15.5.10 CLI and TUI

```text
ptyro cs new <name> --projects api,sdk,web [--template t] [--lead claude] [--layout lead|solo|manual]
             [--prompt <goal> | --from-issue <url>]
ptyro cs ls | show <set> | status <set> [--json] | log <set> [--follow]
ptyro cs msg <set> <member|@lead|@all> <text> [--interrupt]
ptyro cs contract <set> [show|edit|diff <file>|history]
ptyro cs check <set>                       # integration + contract checks
ptyro cs checkpoint <set> [label] | restore <set> <n>
ptyro cs commit|push|pr|land <set>        # lockstep, dependency-ordered, gated
ptyro cs add <set> <project> | drop <set> <member> | rm <set> [--keep <member>]
```

- **Sidebar:** a change set is a group node (`◆ add-user-tier 3/4 ready · r3`) above its members'
  worktrees and sessions.
- **Set view** (`prefix S`): swimlanes, one per member plus the lead, showing status, commits,
  contract revisions, ledger messages drawn as arrows between lanes, and check results. Live air
  traffic control for the change.
- **Attention** rolls up: one gate or one blocked member puts the set in the inbox.

#### 15.5.11 Failure modes we design for

A member's rebase conflicts; a member agent errors out; a contract revision after some members
are done; a sibling's PR merged out of order by a human; a member repo without a remote; differing
CI systems. Each maps to an explicit set state (`member_conflict`, `member_failed`, `stale_contract`,
`out_of_order`, …) with a suggested action, never a silent "done".

#### 15.5.12 Experiment first

Before building the machinery, ROADMAP M11.0 runs the workflow by hand: linked worktrees, a lead
Claude with `--add-dir` per member, member agents prompted by the lead, using only ptyro's
existing primitives (`new`, `send`, `wait`, `read`) and a shared `CONTRACT.md`. Its findings
decide the default layout, the message types and which parts are worth automating.

---

### 15.6 What M7 built **[built M7]**

The engine is data plus a pure function. `ptyro-run` holds the recipe schema and its validation,
the `{{template}}` and `if` languages, the event-sourced `RunState`, and
`plan(recipe, state) → actions`. The daemon's `runs.rs` carries actions out: one actor per run,
step tasks that report back only through events. Nothing in a recipe is code.

#### 15.6.1 Runs, events and durability

- **A run** (`r-7f3a2c`) is a recipe, a fan-out or a lead: kind, name, the pinned recipe source
  and sha, vars, project, base, steps, the sessions it started, picks and discards, lineage
  (`forked_from`). Run status: `running`, `waiting` (a gate or a held step), `succeeded`,
  `failed`, `cancelled`.
- **Every change is a `RunEvent`**, applied to the state and appended to the store with the new
  projection in one transaction. The event kinds: `created`, `step_added`, `step_copied`,
  `attempt_started`, `attempt_info`, `bounced`, `gate_opened`, `gate_resolved`, `outputs`,
  `attempt_ended`, `step_held`, `step_ended`, `step_reset`, `intent`, `outcome`,
  `session_added`, `picked`, `discarded`, `note`, `ended`. `ptyro run events` prints them.
- **Steps:** `pending`, `running`, `waiting`, `succeeded`, `failed` (the agent or a check said
  no), `crashed` (the infrastructure gave way), `skipped`, `cancelled`.
- **Attempts** are the unit of retry. Each one records:
  - the session and the agent's own conversation id;
  - the worktree;
  - `start_ref`/`end_ref` (`refs/ptyro/runs/<run>/<step>/<n>/{start,end}`: checkpoint commits of
    the worktree, never visible commits);
  - the outcome and error, and its bounces.
- **On daemon start** (`start_runs`, after dormant sessions are restored), each running step is
  handled by kind:
  - finished steps never re-run;
  - `run` steps become `crashed`, then follow `retry`;
  - agent steps follow `on_restart` (default `native_resume` when the adapter resumes, else
    `hold`). `native_resume` resumes the same session and nudges it to carry on. `fresh` starts
    a new attempt. `hold` waits for `ptyro run retry`;
  - gates keep their id and absolute deadline; `sleep` keeps its deadline;
  - `apply` re-runs only the actions without an `outcome` event, and each action is idempotent
    (commit if dirty, push, an existing PR is found). Intents are written before acting and
    outcomes after: that is the at-most-once boundary.
- **Time travel is a fork:** `run fork R --from STEP [--set k=v]`. The new run copies every
  finished step that STEP doesn't lead to, then runs from STEP. `run ls --tree` shows the
  lineage. There is no replay.
- **Run directories:** `<data>/runs/<run>/<step>/` hold check logs, `out.json` and the judge's
  files. Step sessions get `PTYRO_RUN_ID`, `PTYRO_STEP_ID`, `PTYRO_RUN_DIR` and `PTYRO_STEP_OUT`
  (run variables pass the client-env filter).

#### 15.6.2 Recipe schema v1

Top level:
- `name`, `description`;
- `vars = { k = { required, default, description } }`;
- `[budget] wall, sessions, attempts`;
- `[policy]` (for leads);
- `[[step]]`.

Each step has exactly one kind:

| Kind | Fields | Ends when |
|---|---|---|
| `spawn` + `prompt` | `spawn = { agent, worktree = "new" \| "root" \| <step> \| <address>, name, profile, base }` | the turn the prompt starts ends (`wait = "idle" \| "done" \| "exited"`), verified if `done_when` |
| `send` | `send = { to = <step>, text }` | the follow-up's turn ends (verified if `done_when`) |
| `run` | `run = "scripts.run.<name>" \| "<command>"`, `in = <step>` | the command exits; success = exit 0 (`outputs.exit_code`, `tail`, `log`) |
| `gate` | `gate = "ask"` or `{ ask, options, timeout, on_timeout = reject \| approve \| escalate \| <option>, fail_on }` | answered; the default `fail_on = ["reject"]` fails the step |
| `apply` | `apply = { from = <step>, actions = [commit, push, pr, integrate, automerge], message, draft, into }` | every action has an outcome |
| `sleep` | `sleep = "10m"` | the deadline |
| `verify` | `verify = { candidates, tests, repro, cross_tests, judge }` | evidence gathered; fails if no candidate succeeded (`outputs.best`, `ranking`, `evidence`) |

A `loop = { until, max = 5, no_progress = 2 }` rides on a spawn. Each iteration gets a fresh
session in the same worktree and a checkpoint (`refs/ptyro/runs/<run>/<step>/loop/<i>`). The loop
ends when `until` passes, after `no_progress` iterations in a row that didn't change the tree,
after `max` iterations, or when the budget runs out. `{{loop.iteration}}` and
`{{loop.last_check}}` are available.

Every step can also have:
- `needs`, `join = all | any | first_success | all_settled`,
  `cancel_siblings = "interrupt" | "keep"` (with `first_success`);
- `if = "<expr>"`;
- `timeout = "45m"` or `{ total, start, stall }`;
- `retry = { max, on, not_on, mode = fresh | same_session, feedback }`;
- `done_when = { run, max_bounces = 3 }`;
- `guard = { deny_paths, deny_cmds, max_diff_lines, on_trip }`;
- `output = { schema = "builtin:verdict" | "builtin:subtasks" | <inline JSON {"required": […]}> }`;
- `on_restart`.

- **Templates:** `{{vars.x}}`, `{{steps.<id>.status|exit_code|error|session|worktree|attempts|outputs.<k>}}`,
  `{{gates.<id>.choice|note|by}}`, `{{run.id|dir|base|project|name}}`,
  `{{attempt.n|error}}`, and `{{loop.*}}` in loops. Arrays render one `- item` per line.
- **Conditions:** `==`, `!=`, `<`, `<=`, `>`, `>=`, `contains`, `&&`, `||`, `!`, parentheses,
  strings, numbers, `true`/`false`/`null` and paths. A missing path is `null`. Numeric strings
  compare as numbers. It is not Turing-complete, and was written in-crate rather than taking a
  CEL dependency.
- **`ptyro recipe validate`** reports every mistake at once:
  - one kind per step;
  - known needs, and no cycles (the cycle is printed);
  - every template and condition only reads variables that exist and steps the step (transitively)
    needs;
  - borrowed worktrees come from needed steps;
  - durations, apply actions, outcomes and schemas are valid;
  - unknown fields are refused.
- **Scheduling** (`plan`):
  - `all` skips a step when a need didn't succeed;
  - `any` runs once any need is over;
  - `first_success` runs on the first success, cancels running siblings with `interrupt`, and
    fails when every need failed;
  - `all_settled` waits for every need;
  - a false `if` skips the step.
  - The run succeeds when every step is over and no failure is left unabsorbed. A failure is
    absorbed when a step with a non-`all` join depends on it.
- **Retries:** `max` counts retries (max + 1 attempts). By default it retries on `failed`,
  `timeout`, `stall` and `crashed`, and never on cancelled or rejected. A fresh retry appends
  `feedback` (default: why the last attempt failed) to the prompt; `same_session` sends only
  the feedback.
- **Typed outputs:** the step's prompt gets instructions to write JSON to `$PTYRO_STEP_OUT`.
  After the turn (and a 15 s grace for the file), the JSON is checked: `builtin:verdict` is
  `{decision: approve|changes, issues, summary}`, `builtin:subtasks` is
  `{subtasks: [{title, prompt}]}`. One repair prompt follows an invalid answer, then the step
  fails. The answer lands in `outputs.output`.
- **Built-in outputs** of agent steps: `session`, `session_name`, `worktree`, `worktree_name`,
  `branch`, `base`, `head`, `diffstat`, `files`, `last_message`, `duration_ms`, `turns`,
  `verified`. `decided_by` says what decided success: `status`, `done_when`, `output`,
  `exit_code`, `gate`, `apply`, `evidence`, `loop.until`.
- **Budgets:** `wall` fails the run when the clock runs out, `sessions` caps the sessions it
  starts, and `attempts` caps attempts across steps. Cost budgets wait for a trustworthy per-agent
  cost signal.
- **Guards:** `deny_cmds` and `deny_paths` deny the tool call through PreToolUse (§8.3).
  `max_diff_lines`/`on_trip` are parsed but not enforced yet.
- **Where recipes come from:** built-in, then `~/.config/ptyro/recipes/`, then the project's
  `.ptyro/recipes/`; a later file with the same name wins. Repo recipes need trust: the scope is
  `recipe:<name>` and the fingerprint is the source's sha. `recipe run` refuses with exit 6 and
  the fingerprint, and on a terminal it asks.
- **Built-ins** (`ptyro-run/src/builtin.rs`):
  - `implement-and-review`: a writer verified by `done_when`, a reviewer from another vendor in
    the same worktree with a `builtin:verdict`, a conditional follow-up, a gate
    (`pr`/`integrate`/`discard`), then apply;
  - `best-of-n`: three agents, a `verify` rank, and a gate to pick in the compare view;
  - `test-fixer`: run the tests; if they fail, an agent fixes the code until they pass; gate;
    PR;
  - `doc-writer`: a writer, then a checker with a verdict;
  - `overnight`: a `loop` until the check passes, then a morning gate.

#### 15.6.3 Gates

A gate has a stable, phone-typeable id (`g-7f3a`). Opening it adds an inbox entry (kind `gate`)
and notifications (`gate` is a breakthrough kind, so quiet hours don't hold it); the TUI toasts
it. It is answered with `ptyro gate resolve ID CHOICE [--note] [--key]`, from the runs view
(`1`–`9`, `n` with a note), or the inbox (opens the run).

- **Idempotent:** the same key or the same choice again is a no-op; a different answer to a
  closed gate is a conflict.
- **Choices** are matched case-insensitively against the options.
- **Agents never answer:** a `GateResolve` (or `Answer`) from inside an agent session is refused,
  whatever its policy.
- **`on_timeout`:** `escalate` notifies again and keeps waiting; anything else answers for you.
- **Leads:** a lead's `request_gate` opens an ad-hoc gate step in its run.
- **Not built:** phone links for gates (the notifier's approve/deny links cover permission
  prompts only).

#### 15.6.4 Fan-out, compare and evidence (M7.2, M7.3, M7.10)

**`ptyro fanout`** builds a recipe:
- one `spawn` step per agent (ids are the agent names, numbered on repeats: `claude`,
  `claude-2`);
- each in a new worktree `<prefix>-<id>` from one base (the prefix defaults to the prompt's
  first three words);
- the prompt delivered when the agent is ready;
- optional `done_when` (`--verify`) and timeout;
- then a `rank` step (`join = "all_settled"`, `verify = {candidates, tests, judge}`).

`-n N` cycles through `--agents`.

**Evidence** (cached on the verify step; `compare --refresh` gathers it again):
- **tests** in each candidate, and on the base in a throwaway detached worktree. `broke_tests`
  means the base passes and the candidate doesn't;
- **repro:** must fail on the base (a note if it passes there) and pass in the candidate;
- **cross-tests** (`cross_tests = "<glob>"`): candidate X's changed test files are checked out
  onto Y's tree in a throwaway worktree, and Y's tests run: `(passed, of)`;
- **clusters:** identical changes once whitespace and comment-only lines are ignored. This
  stands in for Tree-sitter equivalence;
- **diff spread** (files, hunks, ±lines), duration, the last message, verified-done;
- **an advisory judge** (`--judge AGENT` / `verify.judge`): another agent in a clean directory
  holding the candidates' diffs and the task writes `{scores, notes}` (0–10). It is advice, and
  ranks after hard evidence.

**Ranking** is lexicographic and shows its reasons, in this order: succeeded; tests (pass >
not run > fail); didn't break tests; repro; cross-test pass rate; verified; changed something;
judge; smaller diff; faster. The view warns above 6 candidates, when every candidate is the same
agent, and when no tests ran. It shows `git diff --stat` between the top two.

**Acting on a candidate:**
- **`pick`** commits the candidate's uncommitted work as its profile, then does one of:
  - merges (rebase + fast-forward, `wt integrate`; the default);
  - cherry-picks;
  - opens a PR;
  - `--apply --path F`/`--hunk F:N` copies only those files or hunks into a worktree (default:
    the main checkout), uncommitted.
- **`discard`** removes non-picked candidates (or one) through the safe removal (§10.8): work
  that would be lost keeps the worktree unless `--force`.
- **`iterate`** sends a follow-up to one or all candidates (resuming exited ones).

Not built: the divergence probe and cost as a signal.

#### 15.6.5 Verified-done (M7.9)

A session can have a check: `ptyro new --verify CHECK`, `ptyro verify S CHECK`, a recipe's
`done_when`, `fanout --verify`, or `[verify] auto` in `.ptyro.toml`. CHECK is
`scripts.run.<name>`, a run script's name, or a command; scripts need trust.

The check runs `$SHELL -c` in the worktree as the user, with the daemon's environment and full
network, outside the agent's sandbox. It is how agent-written code (tests, build scripts) gets
run by something other than the agent; trust pins only the command's text (§10.5).

- **Claude:** the Stop hook runs the check before the turn may end. If it fails and bounces are
  left, the hook answers `{"decision":"block","reason": "[via ptyro: verify] Not done yet: … exits
  N … output tail …"}`. The status stays working, so nothing downstream sees a false "done".
- **Other agents:** after the turn the check runs; a failure becomes a follow-up prompt (the
  same text) once the agent is idle.
- **Evidence** `{command, exit_code, tail, tree, at_ms, duration_ms}` sits in
  `SessionInfo.verify.last` and a `verify` session event.
- **States:** `pending`, `checking`, `passed` (done, verified), `bounced`, `failed` (bounces ran
  out: done, claimed). Bounces reset on a prompt that isn't a bounce.
- A step waits for `passed` (or fails on `failed`); the review queue and the compare view use it.

#### 15.6.6 Checkpoints (M7.11)

After every agent turn that changed its worktree, the turn is checkpointed:
- **The ref:** `refs/ptyro/ckpt/<session>/<n>`, a commit of HEAD plus everything uncommitted
  (untracked included, ignored not) with HEAD as its parent;
- **How:** written through a private `GIT_INDEX_FILE` with a `ptyro` identity, so the user's
  index, stash list and branches are untouched;
- **The message** carries `session:`, `worktree:`, `turn:` and `at:`;
- **Limits:** no checkpoint when the tree didn't change; 200 kept per session.

`checkpoint ls/diff/restore` (with `--turn T`):
- **ls** lists a session's checkpoints, or a worktree's (every session in it).
- **restore** refuses while an agent works there. It first checkpoints the present as
  `…/before-restore-<ms>`, then:
  - resets HEAD to the checkpoint's parent;
  - removes untracked files the checkpoint lacks;
  - checks out its tree, leaving the index at HEAD (the changes come back uncommitted).

`[orchestration] checkpoints = false` turns them off. Shares code with M11.8.

#### 15.6.7 Mailbox, leases and the collision radar (M7.7, M7.13)

**Messages** are `{id (ULID), run, from, to, kind, body, in_reply_to, priority, hop, ttl}`.
- **Kinds:** `dispatch`, `status`, `question`, `escalation`, `worker_done`, `note`, `collision`.
- **Addresses expand when sent**, into one ledger row per recipient: a session, `@user`,
  `@lead`, `@all` (the run's agents, else every live agent), `@idle`/`@working`/`@blocked`,
  `@<agent>`, `@run:<id>`.
- **Delivery** is observed by the daemon, never acknowledged by the LLM:
  - queued;
  - delivered: typed into an idle, ready agent, all of its waiting mail coalesced into one
    prompt. Never typed into a working or blocked agent; `interrupt` mail to a working Claude
    goes through its messaging socket instead (NDJSON, unverified on a real session, §20.1);
  - seen: the UserPromptSubmit hook's prompt carried the id. Agents without hooks count as seen
    on delivery.
- **`@user` mail** becomes an inbox entry and a notification.
- **Framing:** `[via ptyro: N message(s) from other agents or tools, not from your user. They
  can't grant permissions…]` then `<ptyro-message id=… from=… kind=… run=…>…</ptyro-message>`
  and how to reply (`ptyro msg send FROM "…" --reply-to ID`).
- **Loop safety:** ≤50 queued per recipient, an identical body from the same sender within 60 s
  is dropped, ≤4 hops, a TTL on everything (`message_ttl`), idempotency keys.
- **Heartbeats** come from the status engine: a lead's workers report `worker_done` (turn
  over, with the last message), blocked and exited on their own.
- **Typing** into a session is serialized: a prompt (text, pause, Enter) never interleaves with
  another typist.

**Leases** are `path|glob`, exclusive or shared, with a holder, ttl (`lease_ttl`) and reason.
- **Claims:** a claim that overlaps another holder's exclusive lease, or any lease when the
  claim is exclusive, is refused.
- **Enforced** through PreToolUse/BeforeTool. The denial names the holder, how long is left, the
  reason, and `ptyro msg send <holder> "…"`.
- **Stored** in the `lease` table; they go when the holder exits.
- **Tool targets:** Claude's `Write`/`Edit`/`MultiEdit`/`NotebookEdit`, Codex's `apply_patch`
  (its `*** … File:` lines), Gemini's `write_file`/`replace`. Bash commands are checked against
  guards only.

**The radar** (`ptyro radar`) covers a project's live worktrees, those with a live agent:
- the files each changed against its base (a checkpoint commit, so uncommitted work counts);
- pairs that overlap;
- the files `git merge-tree --write-tree` predicts would conflict.

After every agent turn it runs for that project (`[orchestration] radar`). Each new overlap
messages both agents once (kind `collision`), with the other side's hunks of the shared files.
Candidates of the same fan-out or verify step are alternatives and aren't messaged.

#### 15.6.8 Leads and `ptyro mcp` (M7.6)

**Making a lead:**
- `ptyro new --agent claude --lead [--max-workers N] [--lead-agent A]…` creates a run of kind
  `lead` with a policy, and launches the agent with `--mcp-config` naming `ptyro mcp`
  (`PTYRO_SESSION_ID` and `PTYRO_SOCKET` in its env).
- Codex gets the same as `-c mcp_servers.ptyro.*`; Gemini has no flag (`run lead` still gives it
  `ptyro msg`).
- The injection survives resume and restart. `ptyro run lead S` makes a running agent a lead.

**`ptyro mcp`** is stdio, newline-delimited JSON-RPC:
- **Protocol:** `initialize` (speaks 2026-07-28, 2025-11-25, 2025-06-18, 2025-03-26 and
  2024-11-05; the newest the client asks for), `ping` and `tools/list`.
- **Calls:** `tools/call` runs concurrently. A call with a progress token gets
  `notifications/progress` every 20 s, so long waits survive Claude's backgrounding.
- **Errors:** a tool error is a result with `isError: true`, carrying the policy's explanation.
- **The tools:** `list_sessions`, `spawn_session` (with `idempotency_key`), `send` (as
  `dispatch` mail), `wait_for` (until idle/blocked/exited/any_change), `read_screen`,
  `worktree_diff`, `stop_session`, `post_message`, `check_messages` (a cursor, can wait), `ack`,
  `request_gate`, `claim`/`release`, `fanout`, `compare` and `policy`.
- `anthropic/alwaysLoad` is set on the core ones; descriptions stay under 4 KiB.

**The policy** is `{max_sessions = 4 (live), max_spawns, agents = [], worktrees = ["new",
"own"], destructive = false}`. The daemon checks every lead op:
- the agent must be allowed;
- live workers stay under the limit, and spawns under `max_spawns`;
- workers go only to new worktrees, this run's own (`own`), or `root` if listed;
- the lead may read, wait for, send to and stop only its own workers.

Workers are steps `w1, w2…` of the lead's run; a worker's step ends with its session. The lead
run ends when the lead exits.

**Not just MCP.** A request is attributed to the session whose process tree it comes from (§6.4,
peer pid). Inside a lead or worker session with a policy, the CLI is restricted the same way:
- spawning, new worktrees, fan-outs, recipes, trust, shutdown and the like are refused;
- removal, integrate, commit through ptyro, PRs, pick/discard and checkpoint restore need
  `destructive`;
- kill, restart and send work only on the run's sessions.

Any agent session is refused `gate resolve` and `answer`.

### 15.7 What M11 built **[built M11]**

The pure model is `ptyro-run/src/changeset.rs`; the daemon's half is
`ptyro-daemon/src/changesets.rs`; the CLI is `ptyro cs` (`crates/ptyro/src/cs.rs`); the lead's
tools live in `ptyro mcp`; the TUI's set view is `ptyro-tui/src/sets.rs`. User guide:
`docs/changesets.md`.

#### 15.7.1 The experiment (M11.0, 2026-10-21)

Three small repositories (`api` and `sdk` Go modules, `sdk` with `replace example.com/api =>
../api`; `web` in Node), worktrees laid out by hand side by side in one root on one branch, a
hand-written brief, a lead Claude (Sonnet 5.5) in the root with `--add-dir` per member, and
member Claudes (Haiku) started by the lead with `ptyro new … --prompt`. The lead coordinated with
`ptyro wait --for idle` and `git -C`. Done in 7 min 47 s of lead time; all three committed with
tests green; nothing pushed. What it decided:
- **Members live side by side in the set's root.** `sdk` built against `api`'s *uncommitted*
  code through its relative `replace ../api`: the layout is itself an overlay for every
  relative-path dependency (`replace`, `path = "../x"`, `file:../x`). So member worktrees are
  created at `<root>/<member>` (`WorktreeSpec.path`), not under `worktrees.location`.
- **The lead writes sections per member, unprompted** (`## api (Go…)`, `## Wire format (api ->
  everyone)`, `## Merge order`) and no `affects:` lines. So a section without an `affects:` line
  concerns the members its *title* names, or everyone when it says `everyone`/`all`.
- **Dependency waves emerged:** the lead started `api` and `web` together and `sdk` after `api`.
  Members needed no messages for a change this size; the contract carried it. ptyro starts
  every member at once after approval and gives them `blocked_on` for the waves.
- **The lead committed for the members** (`git -C api commit -am …`), a write no edit fence
  sees. So briefs say commits are the members' (or `cs commit`'s), and the actor flags member
  changes made during the lead's turn while the member's own agent was idle (§15.7.4).
- **The lead stopped the members when done.** A member that exits cleanly isn't failed; mail
  waiting for it resumes it.
- **Every member's first start showed Claude's folder-trust dialog**, and `ptyro answer allow`
  sent `1`, which that dialog ignores: members sat there. Fixed: adapters answer whatever is on
  screen (§20.1).
- 13 real permission prompts in 8 minutes: unattended sets need the inbox/phone approvals (M9)
  or an allow-list.

#### 15.7.2 Model and storage

- **A set is a run** of kind `changeset` named after the set; its `RunState.changeset` is the
  `ChangeSet` projection, changed only by `RunEvent::Cs(CsEvent)` in the run's log. The spec's
  `changeset_member` table was not added: members live in that projection, so there is one
  source of truth and restarts need nothing new.
- **`CsEvent`s:** `created`, `member_added`, `member_dropped`, `member_info` (worktree, path,
  branch, start commit, session, overlay), `contract`, `contract_approved`, `delivered`, `ready`,
  `unready`, `blocked_on`, `unblocked`, `failed`, `conflict`, `checked`, `checkpoint`,
  `committed`, `pushed`, `no_remote`, `pr`, `landed`, `out_of_order`, `phase`, `train`.
- **Phases:** `drafting` → `working` (the contract approved; at once for `solo`/`manual`) →
  `landing` → `landed`; `removed`. Finer states are derived (§15.7.7).
- **Steps:** the lead is step `lead` (or `solo`), each member agent is a step named after the
  member, and gates are steps (`contract-r1`, `push-<ms>`, `land-<member>`, `ask-<ms>`), so the
  runs view and `run show` show a set like any run.
- **Creation** (`cs new`): projects (and/or a template) → one workspace and **one profile for
  every member** (refused otherwise, naming both); `depends_on` from `--depends`, else the
  template, else **manifests** (`go.mod` requires, `package.json` dependencies, `Cargo.toml`
  dependencies by package name, `pyproject.toml` dependencies; B depends on A when B requires a
  name A provides); cycles refused with the cycle printed. Each member's agent: `--agent m=a`,
  else the template's, else the project's `default_agent`, else the default.
- **Linked worktrees** (M11.2): one per member at `<root>/<member>`, on the project's branch
  template (`ptyro/<set>`), base from `--base m=ref`/template/project. Creation is all or
  nothing: a failure removes the worktrees already made and the root, and cancels the run.
- **The root** (`<data>/changesets/<set>/`): `CHANGESET.md` (the generated brief, rewritten when
  membership changes), `CONTRACT.md`, the members, `.ptyro/` (overlay record), and overlay
  files.

#### 15.7.3 Agents and the contract

- **Layouts:** `lead` (default) starts the lead at once and the members when r1 is approved;
  `solo` starts one agent in the root that may edit every member (no contract gate); `manual`
  starts none.
- **The lead** runs in the root with `--add-dir` per member (Claude) and `ptyro mcp`
  (`SpawnSpec.mcp`), which detects a set's lead (`Cs whoami`) and offers the set's tools instead
  of the worker ones. Its run carries a policy that allows no ad-hoc workers and no shipping.
- **Members** run in their worktrees with `--add-dir` for siblings (Claude), `PTYRO_CS`,
  `PTYRO_CS_ROOT`, `PTYRO_RUN_ID` and `PTYRO_STEP_ID`, and a brief: their part, the approved
  contract, the rules and the exact commands (`ptyro cs ready|blocked|msg "$PTYRO_CS"`).
- **Revisions:** `CONTRACT.md` becomes a revision when it changed and its editor is idle (the
  actor checks every 1.5 s and on every run event; r1 also waits for the lead's first turn to
  end), or on `cs contract submit|revise|edit` (`revise` takes a whole text or a unified diff,
  applied with `git apply`). Each revision stores its text, sha, diff (`git diff --no-index`),
  author, summary and `affects`.
- **Gate 1:** r1 opens gate `contract-r1` (`approve` / `revise` / `reject`). `approve` starts
  the members; anything else goes back to the lead with the note; its next save opens a new gate.
  Without the `contract` gate, r1 is approved by `auto`.
- **Propagation:** later revisions are checkpointed first, then each member gets a
  `contract_revised` message with the diff: as an **interrupt** when the revision names it in
  `affects`, normal (next idle) otherwise. `affects` comes from the changed sections' `affects:`
  lines, else their titles (§15.7.1), else everyone; `--affects` overrides.
- **Contract checks** (`contract_checks` in a template) run per member in `cs check` and before
  that member lands; failures go to the lead and the set's state (`check_failed`).
  `cs contract diff FILE` compares the latest revision with any file (a regenerated OpenAPI spec)
  and exits 1 on drift.

#### 15.7.4 Fences and the ledger

- **Write fences** (M11.4) are checked in PreToolUse before leases: the lead may write the root's
  files (`CONTRACT.md`, notes) but no member's; a member only its own worktree (not siblings,
  not the root, not the contract: "ask the lead … --kind contract_change"); `solo` anything in
  the root. Paths resolve against the session's directory (`..` folded lexically). Scratch files
  in the temp directory are allowed unless the set itself lives there. The reason names the
  owner and the command to ask.
- **Advisory fences:** at each lead turn's start and end the actor fingerprints every member
  (HEAD plus `git status --porcelain`); a member that changed while its own agent was idle is
  flagged `out_of_fence` ("changed during the lead's turn").
- **Messages** are the mailbox (§15.6.7) with run = the set and typed kinds: `assign`,
  `question`, `answer`, `contract_revised`, `contract_change` (also notifies you),
  `ready_for_integration`, `blocked_on`, `handoff`, `heartbeat`, `gate_request`. Addresses: a
  member, `@lead`, `@all`. Delivery waits for idle; an **interrupt** to a working agent without
  an inbox socket gets the adapter's interrupt keys once (Esc; at most every 15 s) and is typed
  when the agent is idle.
- **Readiness:** `cs ready` records the member ready against the current revision, tells the
  lead, wakes every member blocked on it, and runs the integration checks once everyone is
  ready. `cs blocked SET ON` marks it blocked on that member, tells the lead and puts the set in
  your inbox; when ON is ready already, it's woken at once.
- **The actor also** records failures (the agent errored or exited non-zero → `member_failed`,
  inbox, the lead hears; cleared by the next good turn), resumes an exited member that has mail
  waiting (at most once a minute), and every 5 s notices members merged outside the train (their
  branch gained commits since its recorded start commit and they're all in the base, or the PR
  is `MERGED`): landed, and `out_of_order` when a dependency hadn't landed.
- **The ledger** (`cs log`, `--follow`) is the run's events described in one line each plus the
  set's messages, by time.

#### 15.7.5 Overlays, checks and checkpoints

- **Overlays** (M11.7) are opt-in per member (`overlay = "…"`, `cs new --overlay` takes the
  manifests' suggestion, `cs overlay SET [auto|go|rust|node|python|compose|off] [--member]`).
  Files are recorded in `.ptyro/overlay.json` and taken back on `off`:
  - **go:** `go.work` in the root listing every Go member (Go finds it from inside each member);
  - **rust:** the root's `.cargo/config.toml` with `[patch.crates-io]` (or `[patch."<git url>"]`)
    path overrides for sibling crates the consumer depends on (cargo reads it from every member);
  - **node:** `node_modules/<pkg>` symlinked to the sibling (what `npm link` does), the installed
    copy set aside and put back; `/node_modules/` git-excluded;
  - **python:** a `ptyro-cs-<sibling>.pth` in the consumer's `.venv`/`venv` site-packages (an
    editable install of the sibling's `src/` or root);
  - **compose:** a git-excluded `compose.override.yml` in the consumer making siblings' worktrees
    the build contexts of services named like them (never overwriting one that isn't ours).
- **Integration checks** (`integration = [...]`, `cs new --integration`) run with `sh -c` in the
  root under the set's profile (30-minute limit, 40-line tail kept); `cs check` runs them plus
  every contract check and exits 1 on any failure.
- **Set checkpoints** (M11.8): `refs/ptyro/cs/<set>/<n>` in each member (the M7.11 checkpoint
  commit: HEAD plus uncommitted work), recorded with each member's HEAD. Taken by `cs
  checkpoint`, the lead's `cs_checkpoint`, before every contract revision after r1, and before
  `land`. `cs restore SET N` refuses while any member's agent works, and refuses when a member's
  present matches no recorded checkpoint (naming them) unless `--force`, which checkpoints the
  present first; members that joined later are left alone; agents are told.

#### 15.7.6 Lockstep git and the train

- **`cs commit -m`** commits every member with changes (`add --all`) in dependency order under
  each worktree's profile. Trailers: `Change-Set: <set>` and `Change-Set-Members:` listing the
  members committed before it with their short sha (a commit can't name its own sha, so each
  commit names the sibling commits it was built against, dependencies first). Clean members
  count with their HEAD.
- **`cs push`** pushes every member in order (`--set-upstream`), skipping members whose remote
  branch already equals HEAD, so running it again resumes. A failure stops it with exactly who
  pushed, who was up to date and who wasn't; a member without a remote is `no_remote`. The
  `push` gate applies unless `--yes`; on a terminal the CLI asks first, otherwise a gate waits
  in the inbox and the push runs when it's answered.
- **`cs pr`** opens one PR per member (M6 plumbing, which pushes), dependencies first, each body
  with the goal and a sibling table between `<!-- ptyro:changeset -->` markers: order, member,
  PR link, CI (`passing`/`failing`/`pending`/`none`), merged/open. Tables are rewritten after
  `cs pr` and after every landing.
- **`cs land`** (a background job; one push or land per set at a time): the integration checks
  first (any failure stops before anything merges), a checkpoint, then each member not landed
  in order: its dependencies must have landed; its gate `land-<member>` unless `--yes` (the CLI
  answers it on a terminal, else it waits in the inbox/phone); then
  - **with a PR:** wait for its checks (polling; an hour at most), stop on a failure, then
    `gh pr merge --<github.merge_method>`;
  - **without:** its contract check, then rebase onto its base branch, fast-forward and push
    (M6's integrate); a rebase conflict is `member_conflict`.
  A failure stops the train with `land_stopped` and the report "the merge train stopped at sdk:
  CI failed on PR #12: build. Landed: api; not landed: sdk, web"; running `cs land` again
  resumes from the member that stopped it. When every member has landed the set is `landed` and
  its run succeeds.
- **`cs rm`** checks every member's safety first (§10.8); one with work that would be lost blocks
  the whole set unless `--keep` it or `--force`. Then the set's agents stop and the worktrees go
  (branches kept for members that didn't land). `cs drop` removes one member (refused while
  others depend on it, unless `--force`); `cs add` adds one (same profile, no cycles) and starts
  its agent when the set is working.

#### 15.7.7 States, actions, tools and the TUI

- **The report** (`ChangeSet::report`, pure): a state word, every issue with a suggested action,
  and per-member words (`working`, `idle`, `ready`, `stale`, `blocked_on:api`, `failed`,
  `conflict`, `landed`, `missing`). Issues, most pressing first: `land_stopped`,
  `member_conflict`, `member_failed`, `out_of_order`, `missing_worktree`,
  `integration_failed`, `check_failed`, `stale_contract` (ready against an older revision that a
  later one affects), `out_of_fence`, `no_remote`, `blocked_on`. The state is the most pressing
  issue's, except that `blocked_on`, `no_remote` and `out_of_fence` are things to know rather
  than the set's state; without one it's `drafting`, `contract_review`, `working`, `ready`,
  `landing`, `landed` or `removed`. Nothing reads "done" unless every member landed. Differing CI
  systems need nothing special: a PR without checks merges after its gate (CI `none`).
- **Templates** (M11.12): `~/.config/ptyro/changesets/*.toml`, then every registered project's
  `.ptyro/changesets/*.toml` ("hub" repositories, trusted per template: scope
  `changeset:<name>`, fingerprint the source's sha; `cs new --trust FP`, or the CLI asks on a
  terminal). Validated with every mistake at once (unknown fields, unknown members, cycles, bad
  overlays and gates).
- **The lead's tools** (M11.10): `cs_status`, `cs_assign`, `cs_send`, `cs_contract_get`,
  `cs_contract_submit`, `cs_contract_revise`, `cs_member_diff`, `cs_integration_check`,
  `cs_checkpoint`, `cs_request_gate` (the answer returns as a message), `cs_log`, plus
  `check_messages`, `ack` and `post_message`. No push, PR, merge, land or restore tool exists,
  and the daemon refuses those ops (and `new`, `add`, `drop`, `rm`, `overlay`, `commit`) from any
  agent; `submit`, `revise`, `assign`, `checkpoint` and questions are the lead's (or yours).
- **The TUI** (M11.11): a sidebar node per set still going (`▾ ◆ tier working 2/3 ready · r2`,
  amber while a gate or an issue wants you) with its lead under it; `prefix S` (or `S` in the
  sidebar, `⏎` on the node) opens the set view: one swimlane per lead and member (status word,
  branch, commits, uncommitted, PR, landed), the open gates (`1`–`9` answer), the issues with
  their actions, and the ledger as a timeline with one column per lane, where messages are
  arrows (`●──▶`) and set events marks (`◆`) in their member's lane. `m` messages the selected
  lane, `⏎` shows its agent, `c` runs the checks, `p` takes a checkpoint. Attention rolls up
  through the inbox: gates, blocked members, failures and a stopped train each add an entry.
- **Not built:** `cs new --from-issue` reads only GitHub issues; integration checks inform
  readiness but don't block a `ready` report; the overlay doesn't rewrite `link:`/`overrides` in
  a copy (node uses symlinks); no `cs pr` refresh from the PR poller (tables refresh on `cs pr`
  and landings).

## 16. Remote access **[built M9, except §16.5]**

### 16.1 Principles **[decided]**

- Off by default. Local Unix socket only until the user enables a remote transport.
- Sending input to an agent session is equivalent to running commands as the user. Remote access is
  therefore authenticated, encrypted, and auditable (every remote input logged in `session_event`).

### 16.2 Remote TUI over SSH (first remote feature)

`ptyro --remote ssh://host` (or `ptyro attach --remote host`) runs `ssh host ptyro proxy`, which
bridges stdio to the remote daemon socket. The local TUI renders the remote sessions natively — no
ports, no new auth, works through jump hosts. Latency-friendly because frames are state deltas.

**[built M9]** `--remote` (or `PTYRO_REMOTE`) takes `[ssh://][user@]host[:port]`. It runs `ssh -T
-o BatchMode=yes -o ServerAliveInterval=15 [-p port] -- host "sh -c 'PATH=…common install
dirs…:$PATH exec \"$0\" proxy' <bin>"`; `PTYRO_SSH` replaces the ssh command and
`PTYRO_REMOTE_BIN` the remote binary (passed as `$0`, single-quoted, so nothing in it becomes
script). A target starting with `-` is refused (it would be an ssh option). Frames are zstd-compressed on this path, the Hello carries `via: ssh://host`, and a
failed handshake reports ssh's last stderr lines. `ptyro proxy` connects to (or starts) the
local daemon and copies bytes both ways. Commands that read local files still read this
machine's (docs/remote.md).

### 16.3 WebSocket server

`ptyro serve --listen 127.0.0.1:7091` (or a Tailscale address) exposes the protocol over WebSocket
(TLS optional, recommended via Tailscale/`tailscale serve` or a reverse proxy). Auth: long random
device tokens (`ptyro pair` prints a one-time code the device trades for one), stored hashed;
revocable (`ptyro devices ls|revoke`).

**[built M9]** `ptyro serve` is its own process (hyper + tokio-tungstenite), so the daemon stays a
local socket. With `[remote] serve = true` the daemon supervises it (backoff, `state/serve.log`,
killed before a live upgrade, restarted by the next image).
- **Routes:** `/` and the embedded app; `/ws`; `POST /api/request` (`Authorization: Bearer`,
  one request, JSON outcome); `POST /api/pair` (`{code}` → `{token, device}`: a `DeviceClaim`
  over a local connection; a refused code counts as a failed login); `/healthz`.
- **Headers:** a strict CSP (`default-src 'none'`, `script-src 'self'`…), `X-Frame-Options:
  DENY`, `nosniff`, `no-referrer`.
- **The WebSocket:** one WebSocket message per protocol frame (JSON text or MessagePack binary).
  The first message must be a Hello. `serve` rewrites it to `kind: web`, `via: ws:<peer>` (or
  `X-Forwarded-For` and `Tailscale-User-Login` from a loopback peer when a proxy is trusted:
  `[remote] trusted_proxy`, `--tailscale`, or `tailscale serve` already fronting the port),
  `pid: 0`, and drops a requested zstd. A `Reject` counts as a failed login: 10 from one address in 10 min answer 429.
  Pings every 25 s; messages up to `MAX_FRAME`.
- **Host check (DNS rebinding):** a request whose `Host` isn't an IP address, `localhost`
  (`*.localhost`), `public_url`'s host or this machine's MagicDNS name (full and short, from
  `tailscale status --json`) gets 421. No `Host` passes (not a browser).
- **Addresses:** loopback and Tailscale (100.64.0.0/10, fd7a:115c:a1e0::/48) only, unless
  `--insecure` or `[remote] insecure`. `--tailscale` runs `tailscale serve --bg --https=443
  http://127.0.0.1:<port>`.
- **Devices:** kept in the store's `kv` (`devices`): id, name, scope, SHA-256 of the token,
  created/last seen/via, push subscriptions, and, until claimed, the pairing code's SHA-256 and
  expiry. Tokens are `ptd_` + 32 bytes from `/dev/urandom` (no fallback), checked in constant
  time against every record; an unclaimed record has no token and never authenticates.
- **Scopes:** `read` < `approve` < `control` < `full`, enforced in `handle_as`
  (`devices::needed`; unknown requests need `full`) and on `Input` (control). A device never
  takes control or resizes a session. Revocation bumps an epoch, and the device's live
  connections get `Goodbye{"this device was revoked"}`. Approve-or-higher actions on a session
  are recorded as `remote` events with the device's name.

### 16.4 Web client / PWA

A small TypeScript client served by `ptyro serve` (assets embedded with `rust-embed`): session list
with status, attention inbox, a terminal view rendering `ScreenFrame`s to a canvas (no xterm.js
needed, because the daemon already emulates), quick-reply box (sends `Paste` + Enter), approve/deny
buttons for permission prompts, push notifications via Web Push. Installable on phones as a PWA.

**[built M9]** The app is TypeScript (`web/src`) compiled by `tsc` 5.9.3 into `web/dist`, which is
committed, so `cargo install` needs no Node; `make web-check` (CI) keeps it in sync. It is
embedded with `include_bytes!`, with no dependencies or third-party requests.
- **Screens:** session list (blocked first, Allow/Deny on the card), inbox, session view. The
  session view is a canvas drawn from `ScreenFrame`s by `web/src/screen.ts`, a port of
  `ScreenCache`; `tests/m9.rs` checks its text equals `ptyro read` against live frames. Fit or
  1:1, at the session's own size.
- **Input:** a reply box (`SendText`), keys, a permission card (with "exact" when the prompt is
  held), and a chat view for structured sessions.
- **PWA:** a manifest, icons (rendered by `scripts/icons.py`), and a service worker with an
  offline shell, push display, and Allow/Deny notification actions answered through
  `/api/request` with the token from IndexedDB.
- **Pairing:** `ptyro pair` (scope `approve` unless `--scope`) prints a QR code (`qrcode`,
  Dense1x2) for `<base>/#pair=<code>`. The base is `--url`, `[remote] public_url`, the Tailscale
  HTTPS name when `tailscale serve` proxies to our port, or `http://<listen>`.
  - **Why a code [security review M2]:** what is printed stays in scrollback and in the session
    recording (on by default, kept 30 days), which `read` devices and agents can read. So it is
    a pairing code (`ptp_` + 32 random bytes, stored as SHA-256), not the token. The app
    `POST`s it to `/api/pair` and gets the token once. The code is then cleared; unclaimed, it
    is pruned after 10 minutes (`PTYRO_TEST_PAIRING_TTL_MS` shortens that for tests, never
    lengthens it). Either way the printed text is worthless afterwards.
  - **Residual risk:** inside those 10 minutes the code is a credential; whoever claims it first
    gets the device. `ptyro pair` waits and prints who claimed it and from where (`devices`
    shows `unclaimed` until then), and in a ptyro pane it says the pane is recorded.

**[built M9] Web Push.** The daemon is the application server.
- **Keys:** a VAPID P-256 key in `state/push-vapid.key` (0600); an ES256 JWT with `aud` = the
  endpoint's origin, `exp` +12 h, `sub` `mailto:ptyro@localhost`.
- **Encryption:** RFC 8291 `aes128gcm`, one record, a new ephemeral key and salt per message. A
  unit test reproduces the RFC's published example byte for byte, and `tests/m9.rs` decrypts a
  delivered push as a browser would.
- **Delivery:** curl with a config on stdin and the body from a 0600 temp file. `TTL: 86400`;
  `Urgency: high` for blocked, errored and gate.
- **Housekeeping:** a 404/410 drops the subscription. Pushes follow the notifier's filters (kind
  in `notifications.on`, mutes, quiet hours, coalescing) and run in the background.

### 16.5 E2E relay (much later)

For access without exposing ports: an outbound-only connection from the daemon to a relay that sees
only ciphertext (key hierarchy: secret held by paired devices; per-session data keys;
pairing by QR). Self-hostable relay. Deliberately last: high security bar.

### 16.6 Remote approvals

For adapters with `REMOTE_APPROVAL` (Claude `PermissionRequest`), the hook relay can wait (bounded,
e.g. 10 min) for a decision from any controller (TUI, phone). Pressing a key in the local TUI always
takes precedence and resolves the prompt in the agent's own UI.

**[built M9]** While at least one device is paired, the daemon holds Claude's `PermissionRequest`
hook for up to `[remote] approval_wait`. The plugin's hook timeout is 3600 s and so is the
relay's budget.
- **Answering:** `answer` allow/deny (from any client) releases the hook with
  `{hookSpecificOutput: {hookEventName: "PermissionRequest", decision: {behavior}}}`. "Always"
  falls back to the dialog's keys.
- **Releasing:** a status transition out of `blocked` (someone answered in Claude's dialog)
  releases the hook with no decision, and so does a newer prompt.
- **Visibility:** `SessionInfo.held` shows the prompt is held.
- **Verified on 2.1.305 (§20.1):** the dialog is shown while the hook runs, a hook decision
  closes it, and a local answer wins over a later hook decision.

---

## 17. Security model

**The boundary is the OS user.** Agents run as you. ptyro's rules for them (they never answer
for a person, never pair devices or grant trust, stay within a lead's policy) defend against an
agent's *mistake*, or a prompt injection talking it into casual misuse of ptyro's own commands.
They are not a boundary against code running as your user, which can read ptyro's files, edit
a trusted script, or reach the socket from a process ptyro can't place. Run an agent you don't
trust under another user, or in a sandbox without the ptyro socket. Also, an agent that can
reach the socket can start commands outside its own sandbox (a shell session, a verify
command): ptyro doesn't confine that.

| Asset / threat | Mitigation |
|---|---|
| Socket hijack by other local users | dir `0700`, socket `0600`, peer-uid check on accept |
| Cloned repo runs arbitrary code | scripts/recipes require fingerprinted trust; agent executables only from user config |
| Code ptyro runs for a repository: scripts, `after_turn`, verify checks, git (security review M3) | trust pins a script's *text*, not the files it calls, and agents edit those files: trusting `cargo test` trusts the tests. Scripts, `after_turn` and checks run as the user, with the daemon's environment and network, outside any agent sandbox; this is documented, not sandboxed (§10.5, §15.6.5, docs/config.md). Background git runs no repository code: `core.fsmonitor=false` always, `core.hooksPath=/dev/null` except for the operations the user asked for (commit, push, merge, rebase, cherry-pick, a new worktree), diffs with `--no-ext-diff --no-textconv` (§10.9) |
| Agents' own permission prompts bypassed | ptyro never auto-answers permission prompts unless the user configured an explicit policy; "dangerous" agent args require `dangerous = true` and show a ⚠ badge |
| Paste injection (`ESC[201~` in pasted text) | every ESC and C1 CSI is removed from bracketed-paste payloads, so nothing can close the bracket (a one-pass removal of `ESC[201~` was beatable by nesting) |
| Malicious escape sequences from child output | emulator is the only consumer; clients render cells, not raw bytes; OSC 52 read denied; title length capped; OSC 7501 messages length-capped (4096); OSC 9/777/99 notifications stripped of controls and capped (1024); query replies never echo what the program sent (XTGETTCAP answers only hex names: echoing a non-hex request typed child output into the shell); sizes from the wire clamped (2×1 … 1000×500); combining marks capped per cell (16) both in what alacritty stores (a filter before its parser, escape-aware, split-UTF-8-safe) and in what frames report, frames budgeted (8 MiB), style tables bounded |
| Recipe values in commands | `run`, `done_when.run`, `loop.until` and verify `tests`/`repro` render every substituted value as one quoted shell word (`render_shell`): an agent-written output can't become shell syntax |
| Parallel permission requests (structured sessions) | every driver keeps each open request: answered newest first (what the status shows), then the next becomes the blocked prompt; cancels and answers from the agent's own UI remove exactly theirs |
| Stale answers | an answer names its prompt (`Answer.expect` = the blocked status's `since_ms`, which changes with every new prompt); approve/deny links and push notifications carry it; a held hook has a generation. A changed prompt refuses the answer |
| Clipboard exfiltration | OSC 52 write only to the controlling client, off with `[clipboard] allow_write = false`, capped at 1 MiB, never into ptyro's paste buffer; read never |
| Secrets in recordings/logs | files `0600`; recorder opt-out; logs never include PTY content above `trace` |
| Secrets in the config file (literal tokens, webhooks) | everything ptyro writes there (`config edit`, `init`, profile and pin commands) is 0600 in directories it creates 0700, replaced atomically; `ptyro doctor` warns when group or others can read or write it |
| Data loss in worktrees | §10.8 rules; locks while running; marker files |
| Remote access | **[built M9]** off by default; the daemon's clients use a local socket only (its one TCP port is the opt-in approve/deny link listener, loopback/Tailscale unless `insecure`). `ptyro serve` listens on loopback/Tailscale unless `--insecure`. Devices: 256-bit tokens stored hashed, compared in constant time, scopes enforced per request and keystroke, a browser's Hello rewritten to `web`, a lockout after 10 failures, revocation closes live connections, pairing only locally and through a one-time code (single use, 10 minutes, so the printed link is worthless afterwards; the token itself is never printed), `approve` by default, remote actions in the event log. The app ships a strict CSP and no third-party code. Web Push is end-to-end encrypted. Over ssh: the ssh login. E2E relay later (docs/remote.md has the threat model). |
| Credentials | ptyro stores none for agents; uses the agents' own auth; remote tokens hashed; profile `secrets` resolved per spawn, memory-only, redacted everywhere |
| An agent answering for a person, or widening what it may do (security review M1) | refused to any agent, with or without a policy, whichever client it uses: `answer` and `gate resolve`; typing (`send`, `send --raw`, a client's keystrokes) into a session that is blocked on a person (Claude's dialog preselects "Yes", so one Enter approves); pairing or revoking devices and push subscriptions; trusting scripts, recipes or templates (`project trust`, the `trust` fingerprint on new worktrees, `recipe run`, `cs new`); upgrading the daemon to another binary. Typing into an *idle* agent, mail, reading and spawning configured agents stay allowed: a lead or a script orchestrating other agents needs them (§15) |
| Telling an agent's request from yours | the connecting process (its pid from the kernel, never from the Hello) is placed in a session by: (1) its ancestry, the nearest ancestor that is a session's process; (2) its POSIX session (`getsid`): every session's child leads its own, so a double-forked, `nohup`ed or `disown`ed process is still placed; (3) on Linux, the `PTYRO_SESSION_ID` it was exec'd with (`/proc/<pid>/environ`), which `setsid` keeps. A client's keystrokes are placed once per connection. The daemon carries no session id (the client that starts it strips it), so neither do the checks it runs. Paired devices are people and held to their scope instead. **It fails open:** a process that leaves all three (`setsid env -u PTYRO_SESSION_ID`, `systemd-run`, `ssh localhost`; on macOS, any `setsid`) looks exactly like you at a plain shell, and is treated like you. Failing closed would refuse the CLI from every terminal that isn't a ptyro session; given the boundary above, ptyro accepts the gap |
| An agent escaping its orchestration limits | the daemon enforces a lead's policy on every request, identifying the caller as above, so shelling out to `ptyro` gets the same limits as MCP. Under a policy an agent gets an *allow-list* (reading, mail, leases, its own run's sessions; shipping only with `destructive`; everything else refused, including forks, retries and cancels of other runs). The hook exemption is by request (`Hook`, `StatusLine`, `Report`), never by the `kind` a client claims. A session in two runs gets the stricter policy. Sessions read only their own mail. Lead spawns are serialized and every spawn counts. Fences and guards match real paths (`..` folded, symlinks resolved); agents never answer gates or permission prompts; agent mail is framed as not-from-the-user and can't grant consent; only ptyro pushes/PRs/merges in recipes (`apply`), and guards deny `git push` and paths through PreToolUse |
| Wrong account / identity (work ↔ personal) | profiles (§10.10): managed vars always scrubbed, credentials scrubbed for isolated profiles, resolution order shown, `match` guard, pins, resume bound to profile, labels everywhere; *not* an OS-level boundary |

---

## 18. Observability, testing, performance budgets

### 18.1 Logging and diagnostics

- `tracing` everywhere; daemon logs to `state/daemon.log` (daily rotation, 7 files). `PTYRO_LOG`
  (env-filter syntax) overrides level.
- `ptyro daemon status`: uptime, sessions, clients, memory per session, frame stats.
  **[built M8]** It shows the watchdog's health:
  - RSS, open fds, live tokio tasks, PTY readers;
  - event-loop lag (max and p99 of a 1 s tick over the last minute);
  - bytes read, frames sent, crashes;
  - terminal state per session, largest first;
  - upgrades.

  The totals carry across live upgrades. With `--json` they're in `metrics`. Every panic, on any
  thread, writes `state/crashes/<ms>-<pid>.txt` (message, location, backtrace; the newest 50
  are kept). A panic inside a session's emulator is caught: the report includes the bytes it
  choked on, and the session gets a fresh emulator, a notification and a redraw nudge, while the
  child keeps running.
- `ptyro status --explain <s>`: the last 20 status signals and which one won.
- `ptyro doctor` (§12.2).

### 18.2 Testing strategy **[decided]**

| Layer | How |
|---|---|
| `ptyro-term` | golden tests: feed recorded byte streams (`tests/fixtures/*.ptylog`, real Claude/Codex/nvim captures) → assert screen snapshots with `insta`; property tests for resize/reflow; fuzz the side-channel parser (`cargo fuzz`) |
| Input encoding | table tests per mode (xterm/app-cursor/kitty flags) |
| Protocol | round-trip tests msgpack/json; compatibility test decoding frames from the previous minor version (fixtures) |
| Status engine | pure reducer tests: signal sequences → expected states; regression fixtures from real hook payloads |
| Daemon integration | `ptyro-testkit` spawns a daemon with `PTYRO_HOME=tmpdir`, uses `ptyro-fake-agent` — a scriptable fake agent binary that emits OSC 7501, calls `ptyro hook`, asks fake permission prompts, prints colours, uses alt screen — so status/attach tests never need real agents or network |
| Git | temp repos; worktree create/remove safety matrix (dirty, untracked, unpushed, stash, locked) |
| End-to-end | scripted TUI runs in a headless PTY (the testkit attaches a client, sends keys, asserts on cell cache). **Built in M1:** `ptyro-headless` (dev tool in `ptyro-testkit`) runs a program, even the ptyro TUI itself, inside the emulator with timed `--send`/`--resize`/`--snapshot`/`--expect`/`--record`; `make smoke` runs it in CI. `crates/ptyro/tests/e2e.rs` drives a real daemon in a temp `PTYRO_HOME` with the built-in fake agent. |
| Manual compatibility matrix | `docs/compat.md`: Claude Code, Codex, Gemini, OpenCode, nvim, vim, htop/btop, less, fzf, lazygit, tig, bash/zsh/fish readline, `vttest` sections — checked per release in Ghostty, kitty, WezTerm, Alacritty, iTerm2, Terminal.app, and inside tmux |

CI: GitHub Actions on Linux + macOS runs `make check` (fmt-check, clippy `-D warnings`, tests,
spike smoke) plus cargo-deny (`deny.toml`: licenses, advisories, bans, sources); release artifacts
are built on tags later. CI only ever calls make targets.

**[built M8]**
- **Fuzzing.** Six targets live in `ptyro-testkit::fuzz`:
  - `sniff`: the same findings however the reads are split;
  - `emulator`: random bytes, resizes and image round trips;
  - `protocol`: the decoder, with or without zstd;
  - `input`: encoding never lets a paste close bracketed-paste mode;
  - `recipe`: the parser and validator;
  - `record`: torn and garbled recordings.

  They're shared by two front ends. `ptyro-fuzz`, a stable-Rust mutation runner with seeds,
  replay and saved crashes, runs 20 s per target in CI (`make fuzz-smoke`). `fuzz/` is a
  cargo-fuzz crate (`make fuzz`, nightly). First long run (2026-10-21): 240 s per target, about
  1.05 billion runs, no crash.
- **Benchmarks.** `make criterion` covers parse, frame build, scrollback, handoff image and TUI
  paint. `make latency` drives a real daemon (`ptyro-latency`), and `make bench` runs it too.
- **Soak.** `make soak` (`scripts/soak.sh`) runs, in a throwaway home:
  - fake agents taking turns nonstop;
  - shells printing coloured bursts;
  - a live upgrade every `UPGRADE_EVERY` seconds;
  - a health sample every `SAMPLE` seconds.

  It fails on a stuck turn, a failed upgrade, a crash, or RSS, fds or tasks still growing after
  warm-up. The release soak is `DURATION=7d`.
- **Compatibility.** `make compat` (`scripts/compat.sh`) is the headless sweep behind
  `docs/compat.md`.
- **Releases.** `.github/workflows/release.yml` runs on `v*` tags. It does `make check`, then
  `make changelog dist` on four platforms (x86_64/aarch64 Linux, aarch64/x86_64 macOS), then
  `SHA256SUMS`, release notes from `scripts/changelog.sh --notes`, the formula from `make
  homebrew-formula`, `gh release`, and a push to the tap when `HOMEBREW_TAP_TOKEN` is set.
  `make dist` refuses a tag that doesn't match the crate version.

### 18.3 Performance budgets (targets, measured in CI benches where feasible) **[proposed]**

| Metric | Target |
|---|---|
| Key → child write (daemon overhead) | < 1 ms p99 locally |
| Child output → painted (local, idle system) | < 10 ms p99 |
| Daemon idle CPU, 20 idle sessions | < 0.5 % of one core |
| Daemon RSS | < 30 MB + ~15 MB per session at 10k-line scrollback (80-col typical) |
| TUI cold start to first paint (daemon running) | < 50 ms |
| Attach keyframe (200×60) | < 64 KiB on the wire, < 5 ms to build |
| Throughput (`cat` a 100 MB file) | daemon stays responsive; ≥ 200 MB/s parse |

**Measured baseline, 2026-10-20 (M1, Linux x86_64, release, `make bench`):**

| Metric | Measured | Notes |
|---|---|---|
| Emulator parse (in-process, 200×50) | 140–200 MB/s plain lines; 80–100 MB/s SGR-heavy | Almost all of it is alacritty's parser; our side-channel sniffer skips to the next ESC byte and runs at ≈ 4.8 GB/s (it was 0.5 GB/s scanning byte by byte, about 20% of the cost). |
| End-to-end through the PTY and daemon | 70–85 MB/s long lines; 20–28 MB/s 8-byte lines (≈ 3.6 M lines/s) | Line-heavy output is bound by per-line scrolling. A viewer barely changes it (rows are hashed only when a frame is built, SPEC §5.4). |
| Idle CPU, 10 idle sessions | 0 ms per 10 s | Purely event-driven, no timers while idle. |
| RSS | ≈ 24 B per cell of *used* scrollback (≈ 48 MB for 10k lines × 200 columns) | Idle short sessions are small; full 200-column scrollback is heavy. Compact scrollback is in M8.3. |

**Measured, 2026-10-21 (M8, Linux x86_64, release):**

| Metric | Measured | Budget | Notes |
|---|---|---|---|
| Key → echo → frame, through the daemon (`make latency`) | p50 0.69 ms, p99 1.0 ms (400 keys, 20–100 ms apart) | key → child < 1 ms; output → painted < 10 ms | Covers the client, the socket, the PTY, `cat`'s echo, parsing, frame build and the frame back. With keys under 8 ms apart it measures frame pacing (`frame_interval_ms`) instead: about 6 ms. On a loaded machine p99 was up to 7 ms. zstd makes no measurable difference. |
| TUI paint, full 200×60 colourful screen (`make criterion`) | 0.23 ms | ≤ 1 ms per frame paint (§11) | So output → painted is ≈ 1.3 ms at p99. |
| Attach keyframe 200×60 | 58 KB, 0.38 ms to build (0.6 ms under load) | < 64 KiB, < 5 ms | |
| Delta of one changed row | 14–20 µs | — | |
| Scrollback document, 10k lines | 13–19 ms | — | `read --scrollback`, copy mode. |
| Handoff image of one 10k-line session | 81 ms write, 126 ms restore (single thread, under load) | — | 10 sessions × 12k lines in parallel: 0.25 s + 0.25 s. |
| Daemon RSS | 20 MB empty; ≈ 19.5 MB per full 10k-line 80-column session | < 30 MB + ~15 MB per session | Over budget at 10k lines, so the default is 5000 lines (≈ 10 MB), with the recording behind it (§7.6). 7 busy sessions in the soak: 87 MB. |
| Emulator parse (criterion, `parse/*`) | 37–67 MiB/s *measured under load* (load 8: another process held 9 cores) | ≥ 200 MB/s | The quiet-machine M1 figure above (140–200 MB/s) is the reference. Still alacritty-bound; not fixed in M8. |
| Soak, 4 min, 4 fake agents + 3 noisy shells, upgrade every 40 s (`make soak`) | 5 upgrades, 424 turns, 0 failures, 0 crashes; RSS 87 → 88 MB, fds 45 → 45, tasks 49 → 48 after warm-up; lag p99 ≤ 4 ms | no leaks, no stuck sessions | The 7-day run is the release gate (`DURATION=7d`). |

---

## 19. Architecture decision records (ADRs)

| # | Decision | Rationale | Rejected alternatives |
|---|---|---|---|
| 1 | **Own multiplexer; no tmux/Zellij dependency** | full control over attach UX, agent metadata, remote protocol; tmux scraping is the top source of bugs in claude-squad et al. | tmux backend (fast MVP, fragile control), Zellij plugin |
| 2 | **Daemon owns PTYs; single daemon per user** | sessions survive clients; one place for state | one holder process per session (dtach/zmx style) — more robust to daemon crashes, but N processes and a frozen forever-API; reconsider if daemon crashes become a real problem |
| 3 | **Dashboard UX** (agents in a sidebar, embedded terminals), not a pane-tree multiplexer | user preference; agents are the unit, not windows | tmux-like window/pane tree |
| 4 | **alacritty_terminal emulator behind a trait** | pure Rust, Apache-2.0, reflow, kitty kbd, sync output, damage tracking | vt100 (fidelity gaps), libghostty-vt (Zig build, unstable API; future), wezterm-term (not on crates.io) |
| 5 | **State-based screen protocol with row versions** | instant reattach, slow-client safety, phone-friendly | raw byte stream with client-side emulation |
| 6 | **MessagePack named, length-delimited; JSON debug** | additive evolution, browser support | bincode (unmaintained), postcard (not self-describing), protobuf (schema tooling overhead) |
| 7 | **pty-process (tokio-native)** | async without reader threads, maintained, small | portable-pty (blocking, old deps, Windows baggage) |
| 8 | **rusqlite + single store thread** | sync API is simpler; bundled sqlite | sqlx (async adds nothing for local SQLite) |
| 9 | **git CLI for mutations** | matches user expectations and hooks; worktree porcelain | gix for writes |
| 10 | **Hooks + OSC 7501 first; heuristics last, labelled** | accuracy; lessons from every competitor | screen scraping |
| 11 | **Controlling client decides PTY size** | least surprise; tmux `latest` | smallest-client-wins |
| 12 | **Single binary** (CLI, TUI, daemon, hook relay) | no version skew between parts | separate `ptyrod` |
| 13 | **Agent executables only from user config** | supply-chain safety for cloned repos | repo-defined agents |
| 14 | **Re-exec live upgrade** (built M8) | keeps children as our children; the emulator image is compact msgpack + zstd, encoded in parallel, so it's fast enough to do mid-turn | SCM_RIGHTS fd handoff to a new process (orphans children); replaying each session's recording into a fresh emulator instead of an image (slow for deep history, and loses modes the bytes set long ago) |
| 15 | **Profiles: identity bundles referenced by workspaces, user-config only, fixed per session** | owner uses multiple accounts; one place to set agent dirs + git + gh + secrets; a cloned repo must never pick your credentials | per-agent variants like `[agents.claude-work]` (doesn't cover git/gh/shells, easy to pick the wrong one), identity stored on the workspace itself (can't share one identity across workspaces), separate daemon per account (duplicate everything) |
| 16 | **`ptyro serve` as its own process; devices authenticated by the daemon** (M9) | the daemon stays a local socket; a crash or bug in HTTP/TLS code can't take sessions down; the daemon is the single authority for tokens and scopes, and `serve` only transports (rewriting the Hello so browsers can't pose as local clients) | the daemon listening on TCP itself; `serve` checking tokens (two authorities); cookie sessions (CSWSH, CSRF) |
| 17 | **Structured sessions render their transcript into the session's emulator** (M10) | `read`, `wait --match`, the recorder, live upgrades and every terminal client work unchanged; the chat view is an enhancement on top (`ServerMsg::Chat`) | a separate session type with no terminal (every existing path would need a structured variant); clients rendering transcripts only |
| 18 | **A change set is a run, and its members live side by side in the set's root** (M11) | runs already give durability, gates, the mailbox, policies and the runs view; the side-by-side layout makes relative-path dependencies build against siblings with no overlay at all (M11.0), and lets `go.work` and `.cargo/config.toml` in the root reach every member | a separate changeset table and engine (two sources of truth); member worktrees under `worktrees.location` with symlinks in the root (tools resolve symlinks differently; fences and `--add-dir` get confusing) |

---

## 20. Facts, gotchas, and external references

Hard-won facts go here, with the date and version they were verified. Add to this list whenever you
learn something the hard way.

### 20.1 Claude Code

- **[verified 2026-10-19, 2.1.304]** Speaks the **OSC 7501 Program Status Protocol**: binary contains
  `probe: OSC 7501 reply` / `probe: no reply to OSC 7501 ; ?` and the changelog line "Added Program
  Status Protocol (OSC 7501) support". Probe is followed by DA1; DA-first ⇒ unsupported. Also probes
  `DECRPM 2026` (synchronized output). Spec: https://superlogical.com/rex/docs/build/program-status
  (rev 0.4); docs: https://code.claude.com/docs/en/terminal-config
- **[verified 2026-10-19, 2.1.304 `--help`]** Flags: `--session-id <uuid>`, `-r/--resume`,
  `-c/--continue`, `--fork-session`, `-n/--name`, `--settings <file-or-json>`, `--plugin-dir`,
  `--setting-sources`, `--permission-mode` (acceptEdits|auto|bypassPermissions|manual|dontAsk|plan),
  `--add-dir`, `--mcp-config`, `--bg`, `--tmux` (requires `--worktree`), `-p` with
  `--output-format stream-json --input-format stream-json --include-partial-messages
  --include-hook-events --replay-user-messages`.
- **[docs]** Hook events include `PermissionRequest`, `Notification` (`notification_type`:
  `permission_prompt`, `idle_prompt`, `elicitation_dialog`, `agent_needs_input`, …), `Stop`,
  `StopFailure` (`error`: rate_limit, overloaded, authentication_failed, billing_error, …),
  `SessionStart` (`source`: startup|resume|clear|compact|fork), `SessionEnd`. Common fields:
  `session_id`, `transcript_path`, `cwd`, `permission_mode`, `hook_event_name`.
  https://code.claude.com/docs/en/hooks
- **[verified in binary]** `idle_prompt` notifications are delayed by `messageIdleNotifThresholdMs`
  (default 60 000 ms) ⇒ use `Stop` for "turn ended", not `idle_prompt`.
- **[docs]** `--resume` does not restore `--settings`, `--mcp-config`, `--plugin-dir`, `--add-dir`.
- **[docs]** With `$TMUX` set: 256-colour clamp; OSC 7501 probes tmux; notifications need tmux
  `allow-passthrough`; Shift+Enter needs `extended-keys`. ⇒ ptyro strips `TMUX` from child env.
- **[docs]** `CLAUDE_CODE_FORCE_SYNC_OUTPUT=1`, `CLAUDE_CODE_NO_FLICKER=1`,
  `CLAUDE_CODE_DISABLE_TERMINAL_TITLE=1` (also disables OSC 7501), `FORCE_HYPERLINK`.
  `DISABLE_TELEMETRY="0"` still counts as *set* (truthiness gotcha).
- **[docs]** Transcripts: `~/.claude/projects/<sanitized-cwd>/<session-id>.jsonl`; 30-day cleanup.
- **[verified 2026-10-20, 2.1.304, golden fixture `claude-main`]** Claude's default UI is a
  **fullscreen renderer**: alternate screen (1049), SGR mouse with all-motion tracking
  (1003+1006), kitty flags 5, and the prompt pinned to the bottom. Scrolling back through
  Claude's output therefore happens *inside Claude* (mouse wheel or keys are forwarded), not in
  ptyro's scrollback.
- **[verified 2026-10-20, 2.1.304]** Killing Claude while its fullscreen renderer is starting makes
  the **next** launch fall back to the classic renderer ("fullscreen renderer didn't finish
  starting last time…"; it retries on the launch after that). That is one more reason ptyro's stop
  is graceful (`/exit` when idle, then HUP), never an immediate KILL.
- **[unverified]** Whether `--settings` hooks merge with or replace user hooks → spike M0.3.
- **[verified 2026-10-20, 2.1.304 binary strings]** Recognises `CLAUDE_CONFIG_DIR`,
  `CLAUDE_CODE_OAUTH_TOKEN`, `ANTHROPIC_API_KEY`, `apiKeyHelper`; stores `.credentials.json`
  (Linux). **[unverified]** On macOS credentials go to the Keychain; whether the entry is
  distinct per `CLAUDE_CONFIG_DIR`, and whether `.claude.json` moves into the dir → spike M0.10.

- **[verified 2026-10-20, 2.1.304, M4]** In a folder it hasn't seen, Claude first shows "Is this a
  project you created or one you trust?" with **"❯ No, exit" preselected**, *before* any hook
  fires (OSC 7501 is silent too). Typing a prompt plus Enter there exits Claude. ptyro
  recognises the dialog on screen and never types into it.
- **[verified 2026-10-21, 2.1.306, M11.0]** The trust dialog has no number keys: `1` does
  nothing, so `ptyro answer S allow` used to leave it waiting. Down then Enter selects "Yes, I
  trust this folder"; Esc leaves (and Claude exits). Every new worktree is a new folder, so each
  change-set member's first start shows it (unlike Codex, Claude has no flag to pre-trust a
  path). The `--add-dir` directories don't need their own trust.
- **[verified 2026-10-21, 2.1.306, M11.0]** A lead blocking in `ptyro wait … --for idle --timeout
  15m` inside one Bash tool call works (the call ran for minutes); Claude backgrounds nothing.
- **[verified 2026-10-20, 2.1.304, M4]** The permission dialog's options are `1` Yes, `2` Yes
  and don't ask again for this (in this directory), `3` switch to auto mode, `4` No, Esc cancels.
  `Notification(permission_prompt)` carries only the generic message "Claude needs your
  permission".
- **[verified 2026-10-20, 2.1.304, M4]** Text followed immediately by `\r` in one write is
  treated as a paste and not submitted, because Claude enables the kitty keyboard protocol. Enter
  must come as its own key event after a short pause (ptyro waits 60 ms and sends the encoded
  key).
- **[verified 2026-10-20, 2.1.304, M4]** `claude agents --json` lists every interactive session
  of the config dir: `pid`, `cwd`, `kind`, `startedAt`, `sessionId`, `name`, `status`.
- **[verified 2026-10-20, 2.1.304, M4]** The statusline JSON carries `rate_limits.five_hour` /
  `seven_day` (`used_percentage`, `resets_at` in epoch seconds), `cost.total_cost_usd` and
  `context_window.used_percentage`.
- **[unverified, M4.18 spike]** Claude sets `CLAUDE_CODE_MESSAGING_SOCKET`/`_TOKEN` in its
  children's environment (ptyro scrubs both from sessions, §5.1); the socket might deliver prompts
  without typing. Its protocol is
  undocumented, and ptyro does not reverse-engineer credentials handling, so delivery stays on
  the PTY. Adopting it needs a documented interface or the owner's decision.

- **[verified 2026-10-20, 2.1.304, M5]** `claude auth status --json` (with `CLAUDE_CONFIG_DIR`
  for a non-default account) prints `loggedIn`, `email`, `orgId`, `orgName`,
  `subscriptionType`, `authMethod`, `apiProvider`, `configDirectory`, `projectsDirectory` and
  `analyticsDisabled`; `--text` is the human form. ptyro's doctor uses it instead of reading any
  file.
- **[verified 2026-10-20, 0.143.0, M5]** `codex login status` prints one line ("Logged in using
  ChatGPT") and exits non-zero when logged out.

- **[docs; M7]** Hook output ptyro relies on: a **Stop** hook printing
  `{"decision":"block","reason":"…"}` keeps Claude working with the reason as its next
  instruction (`stop_hook_active` is true on the next Stop, so a loop is the hook's job to cap:
  `max_bounces`). **PreToolUse** printing
  `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"…"}}`
  denies the call and shows the reason to the model. Both were exercised against the fake agent
  only, not a real 2.1.x session yet.
- **[docs; M7]** `--mcp-config '<json>'` takes `{"mcpServers":{name:{type:"stdio",command,args,env}}}`
  inline; a stdio server's `env` is merged into its environment.
- **[verified 2026-10-21, 2.1.305]** A `PermissionRequest` hook runs *alongside* the permission
  dialog: the dialog renders while the hook sleeps, and stays if the hook returns nothing. A
  hook printing `{"hookSpecificOutput":{"hookEventName":"PermissionRequest","decision":{"behavior":"allow"}}}`
  closes the dialog and the tool runs. Answering "No" in the dialog first wins: a later hook
  "allow" is ignored. This is the basis of M9.5. (`permissions.ask: ["Bash(touch:*)"]` in
  `--settings` forces a prompt even where the user's settings would allow.)
- **[verified 2026-10-21, 2.1.305; fixture `ptyro-drive/tests/fixtures/claude-2.1.305.jsonl`]**
  Structured mode: `claude -p --input-format stream-json --output-format stream-json --verbose
  --permission-prompt-tool stdio [--include-partial-messages]`. In: `{"type":"user","message":
  {"role":"user","content":TEXT}}` lines. Out:
  - `system/init` (`session_id`, `model`, `cwd`, `tools`…);
  - `stream_event` (partial text, with `--include-partial-messages`);
  - `assistant` (`text`, `thinking`, `tool_use`);
  - `user` (`tool_result`);
  - `result` (`subtype`, `is_error`, `result`, `total_cost_usd`, `duration_ms`, `usage`,
    `stop_reason`);
  - `control_request {request_id, request: {subtype: "can_use_tool", tool_name, display_name,
    input, tool_use_id, description, permission_suggestions?}}`, answered `{"type":
    "control_response","response":{"subtype":"success","request_id":…,"response":{"behavior":
    "allow","updatedInput":…}}}` (or `{behavior: deny, message}`).

  `control_request {subtype: interrupt}` ends a turn. `--resume <id>` works in both modes, so a
  session converts between them. `--permission-prompts host|none` exists too.
- **[spike; unverified]** The messaging socket (RESEARCH §II.4): ptyro sends `interrupt` mail as
  one NDJSON line `{"type":"user","message":{"role":"user","content":…}}`, without the optional
  auth line. Not tried against a real session.

### 20.2 Codex CLI

- **[verified 2026-10-19, 0.143.0 `--help`]** `-c key=value` (TOML, dotted paths), `--enable/--disable
  <feature>`, `-p/--profile`, `resume [SESSION_ID] [--last] [--all]`, `fork`, `exec --json`,
  `exec resume`, `app-server --listen stdio://|unix://PATH|ws://IP:PORT`, `--remote <ws|unix>` for
  the TUI, `remote-control`.
- **[docs]** No flag to pre-assign a session id; no `Notification` hook event; non-managed hooks need
  trust review. `notify = [...]` receives JSON `{type:"agent-turn-complete", thread-id, turn-id, cwd,
  last-assistant-message, …}`. https://learn.chatgpt.com/docs/hooks
- **[observed]** Sessions stored at `~/.codex/sessions/YYYY/MM/DD/rollout-<ts>-<uuid>.jsonl`, plus
  `state_*.sqlite`.
- **[verified 2026-10-20, 0.143.0 + source]** Hook trust and session-flag trust are covered in
  §8.4. Every interactive run increments a NUX counter (`[tui.model_availability_nux]`) in
  `config.toml`; that's normal Codex behaviour, not ours. Directory trust is written to
  `config.toml` as `[projects."<path>"] trust_level`.
- **[verified 2026-10-20, 0.143.0 binary strings]** Recognises `CODEX_HOME`, `CODEX_API_KEY`,
  `OPENAI_API_KEY`, `auth.json`, and the `cli_auth_credentials_store` setting (file vs keyring:
  keyring entries may not be separated per `CODEX_HOME` → spike M0.10).

- **[verified 2026-10-20, 0.143.0, M4.19 spike]** `codex app-server generate-json-schema --out
  DIR` works without auth and documents the JSON-RPC surface. It includes
  `account/rateLimits/read` → `{rateLimits: {primary, secondary}}` with `{usedPercent, resetsAt,
  windowDurationMins}`, the `account/rateLimits/updated` notification, and the approval requests
  `item/commandExecution/requestApproval` and `item/fileChange/requestApproval`.
- **[verified 2026-10-20, 0.143.0]** With a stale login, `account/rateLimits/read` returns 401
  "Could not parse your authentication token" even though `codex login status` says "Logged in".
  Turns, approval routing through a side app-server, and the missing hook fixtures (§8.4) all
  wait for a fresh `codex login`.

- **[verified 2026-10-21, 0.143.0]** `codex app-server` speaks JSON-RPC lines *without* the
  `jsonrpc` field. `initialize {clientInfo}` → `{userAgent, codexHome, platformFamily,
  platformOs}`; then the `initialized` notification; `thread/start {cwd, approvalPolicy:
  "on-request", sandbox: "workspace-write"}` → `{thread: {id, …}}` + `thread/started`. All were
  run live. Turns, items and approvals follow the generated schema (`codex app-server
  generate-json-schema --out DIR`: `item/started`/`completed`, `item/agentMessage/delta`,
  `item/commandExecution/outputDelta`, `turn/completed {turn: {status}}`, server requests
  `item/commandExecution/requestApproval` / `item/fileChange/requestApproval` answered
  `{decision: accept|acceptForSession|decline|cancel}`). Logged out, a turn sends an `error`
  notification and then `turn/completed` with status `failed` and the same message ("Your
  access token could not be refreshed…"); ptyro shows it once, as `errored`/`auth`.
- **[unverified; M7]** Codex's hooks are treated as Claude-compatible for output: ptyro answers a
  PreToolUse with Claude's `permissionDecision: deny` JSON, and bounces verified-done with a
  follow-up prompt rather than a Stop decision. MCP servers go in as
  `-c mcp_servers.<name>.command=…`, `.args=[…]`, `.env={…}`.

### 20.3 Gemini CLI

- **[verified 2026-10-19, 0.64.0 `--help`]** `--session-id <uuid>`, `-r/--resume`, `--list-sessions`,
  `-o text|json|stream-json`, `--acp`, `--approval-mode default|auto_edit|yolo|plan`, `-w/--worktree`,
  `gemini hooks migrate` (from Claude Code).
- **[docs]** Hook events: `SessionStart`, `BeforeAgent`, `AfterAgent`, `BeforeTool`, `AfterTool`,
  `Notification{notification_type: "ToolPermission"}`, `SessionEnd`, …; no `--settings` flag;
  `GEMINI_CLI_SYSTEM_SETTINGS_PATH` and `GEMINI_CLI_HOME` exist. `ui.dynamicWindowTitle` shows
  Ready/Action Required/Working in OSC 0. Honours `NO_COLOR`. Emits kitty keyboard push, `?2004h`,
  `?1004h`, `?2026h`. https://geminicli.com/docs/hooks/
- **[docs]** Free-tier users were moved to "Antigravity CLI" on 2026-06-18.
- **[verified 2026-10-20, 0.64.0 bundle]** `getSystemSettingsPath()` honours
  `GEMINI_CLI_SYSTEM_SETTINGS_PATH`, and `GEMINI_CLI_SYSTEM_DEFAULTS_PATH` overrides
  `system-defaults.json`. Both are dropped unless the file and its parent are root-owned and not
  group/world-writable. Settings merge order: schema defaults < system-defaults < user < workspace
  (only if the folder is trusted) < system. `SessionEnd` fires 3× on `/quit`. Hook payloads carry
  `timestamp`. Sensitive files in the Gemini home: `oauth_creds.json`, `google_accounts.json`,
  `gemini-credentials.json`, `mcp-oauth-tokens.json`, `trusted_hooks.json`, `trustedFolders.json`.

- **[docs; unverified M7]** A `BeforeTool` hook printing `{"decision":"deny","reason":"…"}`
  denies the tool call. ptyro uses it for leases and guards; verified-done uses a follow-up
  prompt.

- **[verified 2026-10-21, 0.64.0]** `gemini --acp` (ACP, JSON-RPC 2.0 lines):
  - `initialize {protocolVersion: 1, clientCapabilities: {fs: {readTextFile: false,
    writeTextFile: false}, terminal: false}}` → `{protocolVersion: 1, authMethods: [oauth-personal,
    gemini-api-key, vertex-ai, gateway], agentInfo, agentCapabilities: {loadSession: true, …}}`;
  - `session/new {cwd, mcpServers: []}` → `{sessionId, modes, models}`, then a `session/update`
    `available_commands_update`.

  Run live, also through ptyro (`make verify-structured`). Prompts, updates and
  `session/request_permission` follow the ACP spec; not yet run against a real Gemini turn.

### 20.4 OpenCode

- **[docs/source, not installed locally]** TUI embeds an HTTP server (`--port`, `--hostname`);
  `opencode serve`; SSE `GET /event`; `session.status` `{idle|busy|retry}`; `permission.asked`;
  basic auth via `OPENCODE_SERVER_PASSWORD`. https://opencode.ai/docs/server/

- **[docs, 2026-10-20, M4]** Permission replies: `POST /session/:id/permissions/:permissionID`
  with `{"response": "once"|"always"|"reject"}`. Event names used: `session.status`,
  `permission.asked`/`permission.updated`, `permission.replied`, `session.error`. Not yet run
  against a real OpenCode.

- **[docs, unverified: not installed]** `opencode serve` structured mode (M10.5): `POST /session`,
  `POST /session/:id/prompt_async {parts}`, `message.part.updated` (text, reasoning, tool parts
  with `state {status, input, output, title}`), `message.updated {info {cost, tokens}}`,
  `session.idle`, `POST /session/:id/abort`. Tested only against the fake.

### 20.5 Terminal & Rust ecosystem

- **[verified crates.io 2026-10-19]** versions in §3.5. `bincode` 3.0.0 is a tombstone release
  (RUSTSEC-2025-0141). `wezterm-term` is not published on crates.io.
- **[verified 2026-10-20, terminput 0.5.15]** Two gaps we must cover:
  - terminput's `KittyFlags` bits are shifted one place from the kitty spec
    (`DISAMBIGUATE_ESCAPE_CODES = 1<<1`). Map flags from alacritty's `TermMode` *by name*, never
    by bits.
  - Its xterm encoder ignores DECCKM. In application-cursor mode, unmodified arrows/Home/End must
    become `ESC O x`. `ptyro-term` patches this, unit-tested in `crates/ptyro-term/src/input.rs`.
- **[verified M0 spike]** Startup queries by program:
  - Claude Code 2.1.304: XTVERSION, OSC 7501 probe, kitty `?u`, DA1, DECRQM 2026, `CSI 16 t`,
    DECRQM 1016. It sets 2004, 1004, 2031 (palette-change notifications) and kitty flags 5, and
    uses 2026 sync for every frame (~50 pairs in a short session).
  - Codex 0.143.0: CPR, OSC 10/11, kitty `?u`, DA1, kitty push **7**, which includes
    REPORT_EVENT_TYPES, so key *releases* must be forwarded.
  - Gemini 0.64.0: XTVERSION, OSC 11, kitty `?u`, DA1, kitty push 1.
  - vim 9: CPR, DA2, OSC 10/11, DECRQM 12, XTWINOPS 22/23 (title stack).
  - Codex and Gemini pick their theme from OSC 10/11, so answering with the outer terminal's
    *real* colours (§5.6) matters.
- **[verified M0 spike]** pty-process 0.5.4: `open()` → `(Pty, Pts)`, `Command::spawn(pts)` does
  setsid + controlling-tty; `Pty::into_split()` gives tokio read/write halves, and the write half can
  `resize`. Linux reports EIO at child exit, so treat any read error as EOF.
- `alacritty_terminal` answers queries via `Event::PtyWrite`; synchronized output is handled in
  `vte::ansi::Processor` (buffer until ESU or timeout); OSC 8 per cell; OSC 52 via
  `Event::ClipboardStore/Load`. OSC 133 is not parsed **[unverified]** → we sniff it ourselves.
- macOS `sun_path` is 104 bytes (Linux 108); macOS `$TMPDIR` is long — check before bind.
- **[verified M2]** `cargo check --target aarch64-apple-darwin` from Linux fails at the build
  script of `libsqlite3-sys` (bundled SQLite is C; Apple's SDK isn't there). `make check-macos`
  therefore checks the crates below the store; CI builds everything natively on macOS.
- **[verified M2]** A watch on a *file* misses editors that save by renaming a new file into
  place; watch the directory and filter by path.
- **[verified M1, e2e test]** Children of the daemon get the *default* SIGPIPE disposition, even
  though Rust ignores SIGPIPE in its own process: std's `Command`, and therefore tokio's and
  pty-process's, resets it before exec. Without that, `yes | head -1` inside a session would spin
  forever.
- **[verified M1]** A split `Framed` socket closes only when *both* halves are dropped. A client
  whose reader task outlives the `Client` keeps the connection, and the viewer count, alive, so
  `Client` aborts its reader on drop.
- **[verified M1]** Inside the daemon, every watch guard in a `tokio::select!` branch must be dropped
  before awaiting (`watch::Ref` is not `Send`). Wrap such futures in `async { … .await; }`.
- Never `fork()` after starting a tokio runtime; spawn the daemon as a fresh process instead.
- Reattach redraw trick (shpool, dtach): if the size didn't change, resize to rows−1 and back to
  force full-screen apps to repaint. With daemon-side emulation we rarely need it, but keep it as a
  `ptyro redraw <session>` escape hatch.
- tmux passthrough for OSC to the outer terminal: `ESC P tmux; <seq with ESC doubled> ESC \`,
  requires `set -g allow-passthrough on`.
- **[verified M3]** `tokio::sync::watch::Sender::send` *drops the value* when there are no
  receivers. A session that exited before anyone called `wait_exit()` then never looked exited.
  Use `send_replace` for state that late subscribers must see.
- **[verified M3]** inotify (via `notify`) reports file *opens* as events, so watching a file you
  also read is a feedback loop. Compare contents before acting.
- **[verified M3]** Background `git status` takes `index.lock` for its opportunistic index
  refresh and breaks concurrent `git add`/`commit`; `GIT_OPTIONAL_LOCKS=0` (or
  `--no-optional-locks`) prevents it.
- **[verified 2026-10-21, git 2.53]** `git status` runs the program in `core.fsmonitor`, and
  `update-ref` (so every checkpoint) runs the `reference-transaction` hook. `-c
  core.fsmonitor=false -c core.hooksPath=/dev/null` stops both (`ptyro-git`'s
  `background_calls_run_no_repository_code`).
- **[verified M3]** A gitignore pattern with a trailing slash (`node_modules/`) matches
  directories only. A *symlink* to a directory is a file to git and shows as untracked.
- **[verified M3, git 2.53]** `git worktree add -b B path origin/main` sets `origin/main` as B's
  upstream (branch.autoSetupMerge); pass `--no-track`. `git stash list --format='%gd %gs'` gives
  `On <branch>:` / `WIP on <branch>:`, which identifies a worktree's stashes. A commit subject
  joins its first paragraph's lines with spaces.
- **[verified M8, alacritty_terminal 0.26.0]** A tab leaves a literal `\t` in the cell it
  jumped from, so that copying keeps tabs. The cell's text reaches our frames as is. Painting it
  sends a raw tab to the *outer* terminal, which jumps to the outer tab stop: the wrong column
  whenever the pane doesn't start at column 0. The TUI paints control characters as spaces;
  copy mode keeps the tab.
- **[verified M8, alacritty_terminal 0.26.0]** DECCOLM (80/132 columns) is ignored, as in most
  modern terminals, so vttest's 132-column screens are garbled. DSR 6 (cursor position)
  reports absolute rows even under DECOM (origin mode); xterm reports rows relative to the
  scroll region. DA3 and DECREQTPARM aren't answered. Everything else in vttest's cursor,
  VT102 and report menus is correct (`docs/compat.md`).
- **[verified M8, alacritty_terminal 0.26.0]** With the default `serde` feature, `Grid<Cell>`
  serializes (scrollback included), `Cursor`'s fields and `Term::swap_alt` are public, and
  `mark_fully_damaged` isn't. An image therefore saves the alternate grid by swapping, cloning
  and swapping back. Restoring feeds `\e[?1049h` after putting the cursor where the *primary*
  screen's saved cursor was, because 1049 saves the cursor at that moment. Named msgpack maps of
  a 10k-line grid run to ~70 MB; compact `rmp_serde::to_vec` plus zstd level 1 is ~50× smaller.
- **[verified M8, Linux 7.0]** After the binary on disk is replaced, `readlink /proc/self/exe`
  ends in `" (deleted)"`. Strip it to find the new binary at the same path. `execve` keeps the
  pid, so children stay ours and `waitpid` works, but tokio's child handles don't survive: reap
  adopted pids with `waitpid(pid, WNOHANG)` on SIGCHLD. A listening socket with `FD_CLOEXEC`
  cleared keeps its backlog across the exec, so clients connecting meanwhile simply wait.
- **[verified M8]** A future awaiting a oneshot reply whose sender sits in a map that the
  connection's reader already cleared (connection gone) waits forever, with no timer to wake
  the runtime. Mark the map closed (an `Option`) so later requests fail at once. The soak found
  this as a `send` stuck for good across an upgrade.
- **[verified M8]** A fast child can run its first hook (`SessionStart`) before the daemon has
  registered the session it just spawned (between the fork and the insert). The hook used to be
  answered NotFound and its signal lost, leaving a Codex session that was never "ready". A
  hook for an unknown id now waits up to 2 s for the session to appear. Found by stress-running
  `m5` 18 times in parallel.
- **[verified M9, hyper 1.12]** `http1::Builder::header_read_timeout` panics at the first
  connection ("timeout set, but no timer set") unless the builder also gets `.timer(TokioTimer)`.
- **[verified M10, clap 4.6]** `Command::debug_assert()` on the full ptyro CLI overflows a test
  thread's 2 MiB stack in a debug build. The test runs it on a 32 MiB thread; the binary's main
  thread (8 MiB) is fine.
- **[verified M10]** `std::sync::Mutex` guards created inside a struct literal live until the end
  of the statement, so a later field that locks the same mutex deadlocks (`Session::image`'s
  `meta` field, then `is_live()`). This is the M7 double-lock again. Compute such fields first.

### 20.6b GitHub CLI (`gh`), as ptyro uses it **[docs; tested against `fake-gh`, not the real `gh`]**

- **Status and checks:**
  - `gh pr view <branch|number> --json number,url,title,state,isDraft,reviewDecision,autoMergeRequest`;
    without a PR it exits non-zero with "no pull requests found for branch".
  - `gh pr checks <n> --json name,bucket,link,workflow`. `bucket` is `pass`, `fail`,
    `pending`, `skipping` or `cancel`; the exit code is 8 while any is pending.
  - `gh run view <run-id> --log-failed` (the run id is in the check link,
    `/actions/runs/<id>/job/<job>`).
- **Comments:**
  - `gh api repos/{owner}/{repo}/pulls/<n>/comments`: review (line) comments with `id`, `path`,
    `line`/`original_line`, `user.login`, `in_reply_to_id`. `gh api` fills in
    `{owner}/{repo}` from the checkout.
  - `gh api --method POST repos/{owner}/{repo}/pulls/<n>/comments/<id>/replies -f body=…`
    replies on a thread.
  - `gh pr view <n> --json comments` gives the conversation comments (node ids);
    `gh pr comment <n> --body`.
- **Changes:** `gh pr create --head B --base X --title T --body … [--draft]` prints the URL;
  `gh pr merge <n> --auto --squash|--merge|--rebase` and `--disable-auto`;
  `gh issue view <n> --json number,title,body,url`.
- `GH_PROMPT_DISABLED=1` keeps gh from asking anything; `GH_CONFIG_DIR`/`GH_TOKEN` pick the
  account (§10.10).

### 20.6 Prior art and inspirations (ideas, never code)

ptyro is its own design and codebase. These projects shaped our thinking, and we credit them
gladly. A dated survey of the wider field, and the ideas it suggests, is kept in `RESEARCH.md`.
Verified facts found there graduate into §20.1–§20.5 (`RESEARCH.md` §IV.3 lists the candidates).

Orca (https://www.onorca.dev, https://github.com/stablyai/orca) — managed hooks,
notifications, worktree UX, orchestration mailbox · Conductor (scripts, ports,
`.worktreeinclude`) · Claude Code agent view & worktrees (locks, markers, cleanup rules) ·
claude-squad / agent-deck / workmux (tmux-based; status scraping pain) · cmux (OSC 9/99/777
notifications, ports in sidebar) · rook (`rookctl` socket API) · murmur (in-agent status reporting) ·
Happy (E2E mobile relay) · Zed ACP (structured agent protocol) · Zellij & WezTerm (mux
architecture) · shpool, dtach, abduco, zmx (session persistence; restore strategies).

---

### 20.7 Other tools' repository config (formats read by M5.10)

Checked against each tool's documentation on 2026-10-20. Marks: [V] official docs or source,
[S] secondary sources, [U] unverified.

- **Claude Code desktop `.claude/launch.json`** [V] (code.claude.com/docs/en/desktop, Preview):
  - Shape: JSON with comments, `{version, configurations[], autoVerify}`.
  - A configuration is `{name, runtimeExecutable, runtimeArgs[] | program, args[], cwd, env,
    port (default 3000), autoPort, url}`.
  - `autoPort: true` passes a free port in `PORT` and rewrites `--port`/`-p` in `runtimeArgs`;
    `false` fails if the port is busy. `url`-only entries attach to a running server.
  - Whether commands run through a shell: [U].
- **Conductor `.conductor/settings.toml`** [V] (conductor.build/docs/reference):
  - Layers: `settings.local.toml` on top; unknown keys are rejected. `conductor.json` is the
    legacy form, ignored once settings.toml exists.
  - `[scripts]`:
    - `setup` and `archive` are shell strings;
    - `run` is a string or `[scripts.run.<id>]` with `{command, args, options.cwd, default,
      icon, hide, available_in}`;
    - `run_mode` is `concurrent` or `nonconcurrent`.
  - Variables: `[environment_variables]` (+ `.local`/`.cloud`); `CONDUCTOR_PORT` is the first
    of 10 consecutive ports; `CONDUCTOR_ROOT_PATH`, `WORKSPACE_PATH`, `WORKSPACE_NAME`,
    `DEFAULT_BRANCH`, `IS_LOCAL`.
  - Run scripts stop with TERM, then KILL after 5 s.
- **Superset `.superset/config.json`** [V] (docs.superset.sh; source
  `packages/host-service/src/runtime/setup/config.ts`):
  - `setup`, `teardown`, `run`, `start` must be arrays of shell commands (a string makes the
    file invalid); `cwd` is relative to the worktree.
  - `config.local.json` replaces a key, or wraps it with `{before, after}`; a missing key falls
    back to `.superset/<key>.sh`.
  - No port allocation. Variables: `SUPERSET_ROOT_PATH`, `WORKSPACE_PATH`, `WORKSPACE_NAME`.
- **Codex app `.codex/environments/*.toml`** [S] (the docs describe behaviour, not keys; keys
  from an OpenAI-authored example):
  - Usually `environment.toml`, app-generated (`version = 1`, `name`).
  - `[setup] script` with platform overrides (`[setup.win32]` seen; `darwin`/`linux` [U]),
    `[[actions]] {name, icon, command, platform?}`, and `[cleanup] script` [S].
  - Variables: `CODEX_WORKTREE_PATH`, `CODEX_SOURCE_TREE_PATH` [S/U].
- **Copilot coding agent `.github/workflows/copilot-setup-steps.yml`** [V] (docs.github.com):
  - It needs one job named `copilot-setup-steps`, read from the default branch.
  - Honoured keys: `steps`, `permissions`, `runs-on`, `services`, `snapshot`, `timeout-minutes`
    (≤ 59).
  - Only `run:` steps can run outside Actions. `uses:` and `${{ }}` can't. A failing step
    skips the rest.
- **Cursor cloud agents `.cursor/environment.json`** [V] (cursor.com/docs/cloud-agent/setup;
  schema at cursor.com/schemas/environment.schema.json):
  - JSON with comments; unknown keys are rejected.
  - `install` (idempotent, cached) is the setup; `start` runs on every boot;
    `terminals[] {command, name?, description?}` (may nest).
  - Image fields: `build {dockerfile, context}`, `snapshot`, `image`.
  - `ports[] {port, name?}` is declarative; no environment map.

## 21. Open questions

1. ~~**License**~~ — **decided 2026-10-20: MIT** ("for now"; `LICENSE` at repo root, `license =
   "MIT"` in every crate). MIT is compatible with alacritty_terminal's Apache-2.0. Relicensing to
   dual MIT OR Apache-2.0 later stays easy while the owner is the only contributor.
2. ~~**Default prefix key**~~ — **decided 2026-10-20: `Ctrl-Space`, configurable** via
   `[ui] prefix` (§13.2). Known caveat: macOS binds Ctrl-Space to "select previous input source"
   when more than one input source is enabled; `ptyro doctor` and `ptyro init` should detect a
   prefix that never arrives and suggest `C-a`/`C-]` or disabling the macOS shortcut.
3. **Codex & Gemini hook injection** — which mechanism works without touching user config (M0.4/M0.5).
4. ~~**Claude hooks injection**~~ — **decided 2026-10-20:** `--plugin-dir`; both mechanisms are additive (§8.2, M0.3).
5. **Should the TUI offer a raw-passthrough fast path** when it is the only client and sizes match
   (enables image protocols like kitty graphics)? Deferred until someone needs images.
6. ~~**Workspace semantics**~~ — **decided 2026-10-20:** workspaces carry a view, an attention
   policy (§9.6) *and* a profile (§10.10); the owner runs several accounts side by side, switching
   today via shell aliases with separate config dirs (→ `ptyro init` adoption, §10.10). `default`
   becomes `isolate = true` once any profile is defined (decided 2026-10-20).
7. **Antigravity CLI** adapter — research once Gemini users migrate.
8. **Change sets (§15.5)** — **direction decided 2026-10-20:** one lead plus one agent per member
   repo, validated by the hands-on experiment M11.0 before the machinery is built. Still open:
   should the lead be allowed to edit member code in `solo` layout only, or never; do the contract
   checks belong in the template or in each repo's `.ptyro.toml`; and how deep should the merge
   train integrate with forge-native merge queues?
