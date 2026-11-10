<img src="docs/logos/ptyro-icon.png" alt="ptyro" width="72" align="right">


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
22. Chat: general-purpose conversations
23. Shells: a workspace's numbered terminals
24. Bootstrap: agents, tools and updates

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
- Being an agent. ptyro hosts agents; it never calls an LLM API itself. The one exception is
  opt-in session summaries (**[built UX1.3]**, approved 2026-10-22, §11.2h). They run a small
  model's CLI (Claude's Haiku by default) on the session's own profile account, and they are off
  by default.
- Built-in editor, task tracker (**back-burnered**; see ROADMAP Backlog).
- ~~Windows native.~~ Reversed 2026-10-31 (owner): native Windows 11 and WSL2 are both Tier 1 targets (§3.4, ROADMAP WIN). Until WIN ships, WSL2 is expected to work like Linux.

### 1.3 Principles

- **The daemon owns the truth.** PTYs, processes, terminal state, status, metadata. Clients are thin.
- **Never scrape when you can be told.** Status comes from agent hooks and terminal protocols
  (OSC 7501); heuristics are a labelled fallback with explicit low confidence.
- **Never lose work.** No automatic deletion of a worktree with uncommitted or unpushed changes, ever.
- **Agent-native, not agent-specific.** Adapters declare capabilities; the core never hard-codes Claude.
- **Three first-class clients: the CLI, the TUI and the web.** *(owner, 2026-10-23; widens
  "the TUI is first class" of 2026-10-22, which replaced "the TUI is optional")* Every workflow
  must be complete, discoverable and polished in each, and must work there without the user
  learning anything up front: defaults set up, errors that offer the fix, keys (or buttons)
  shown where they apply.
  - **The CLI and the TUI are always in parity.** A feature isn't done until both have it.
    `docs/parity.md` maps every command to its TUI path and its web path, and `make parity`
    (also part of `make test`; `make tui-parity` is the same) fails when a command has no TUI
    path and no waiver.
  - **The web ships with them.** A feature is built for the web client (`ptyro serve`, the
    PWA, §16) in the same change; its web path is a control registered in `ACTIONS`
    (`web/src/app.ts`, each its tooltip), which the check reads. A web gap is a defect to fix,
    not a later milestone: a web gap fails `make parity`, like a TUI one (`WEB_STRICT=0` only
    lists them, while one is being worked on). ROADMAP WEB1 closed them all.
  - Waivers are for commands that are another program's tool (agents' own mail, hooks, shell
    completions), never for "not built yet".
  - **Creating shows every option.** *(owner, 2026-11-09)* Whatever a client creates (a
    workspace, profile, session, worktree, project, shell, change set, fan-out, recipe run,
    PR, device, machine…), its form shows every option the CLI's command takes, each with its
    default visible and, wherever the answers are known (profiles, agents, projects,
    workspaces, recipes), as a pick rather than free text. The quick path stays quick: ⏎ (or
    the button) takes the defaults. In the TUI that is the creation form (§11.5a).
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
| **Name / title** | **[built UX1.24]** Sessions and worktrees have both. The **name** is the address (`[a-z0-9._-]`, fixed at creation, changed only by `rename`): `ptyro send fix-flaky-test …`. The **title** is free text people read (the task, a PR's title) and changes freely (`retitle`). By default a name is made from the title or prompt (`fix-flaky-test`), else `<agent>-<corvid>` (`claude-rook`), else a friendly pair; the title is the prompt. |
| **Agent** | A configured session kind backed by an adapter (`claude`, `codex`, `gemini`, `opencode`, `shell`, or a user-defined generic command). |
| **Conversation** | The agent's own notion of a chat (Claude `session_id`, Codex `thread_id`, …). A ptyro session *has* zero or one current conversation id; resuming a dormant session reuses it. Never confuse the two ids. |
| **Chat** | **[decided 2026-10-29, §22]** A general-purpose conversation with an agent: no repository, no worktree. A durable tree of nodes in `chat.db`, owned by one workspace on one machine, served by a replaceable *engine* (a hidden structured session). Not a session, and not the agent's *conversation* (a chat may use several over its life). |
| **Shell (SHELLS)** | **[decided 2026-11-04, §23; proposed]** A session that belongs to a workspace instead of a worktree (`loose`), any command, listed in the workspace's SHELLS section under a **number** (its *slot*) that never changes on its own: `Alt-3`, `%3`. tmux's window. Not the quick terminal (one per worktree, §11.2h). |
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
Workspace 1──* loose Session, numbered 1, 2, 3…  (SHELLS, §23; proposed)
Run 1──* Session  (orchestration membership, optional)
Workspace 1──* Chat 1──* Node (a tree) ; Chat 1──* Conv ── its engine, a hidden structured Session  (§22)
```

### 2.3 Identifiers **[decided]**

- Every entity has a **ULID** (`01JB…`), sortable by creation time, stored in full.
- Humans see a short, unique **handle**: `s:7f3k` (sessions), `w:…` (worktrees), `p:…` (projects),
  computed as the shortest unambiguous lowercase Crockford-base32 suffix (min 4 chars).
- Sessions and worktrees also get a **name** and a **title** (**[built UX1.24]**):
  - The **name** is the address: `[a-z0-9._-]`, 1–64 characters, chosen at creation and changed
    only by an explicit rename. Session and worktree names are unique per project; sessions in
    no project share one scope (owner, 2026-11-09). A bare name two projects share is ambiguous:
    the error lists each `project/name`, except that mail from an agent means the session in
    the sender's own project. Defaults are below.
  - The renames (owner, 2026-11-09): `rename` (a session), `wt rename` (a worktree) and
    `project rename` (a project's slug, the `project` in `project/name`), in the CLI, the TUI
    (`session.rename`, `worktree.rename`, `project.rename`) and the web. A worktree keeps its
    branch (`wt rename-branch` changes that) and its directory, and a project's worktrees keep
    theirs: moving a directory under running sessions would break them. A worktree's name is
    unique among its project's worktrees; a project's slug is unique; `@root`, the main
    checkout, is never renamed. A running process keeps the `PTYRO_WORKTREE_NAME` and
    `PTYRO_PROJECT` it started with (§10.6).
  - The **title** is free text for people to read: the task, a PR title. It may change at any
    time (`retitle`), is never an address, and is shown in the sidebar and headers, not used in
    commands. Without one, the name stands in. A summary (UX1.3) is its own line, not the title.
  - The friendly-name generator draws from a corvid-themed word list (`brisk-rook`,
    `quiet-jackdaw`, `sly-magpie`; `ptyro_core::names`).
- **One name policy** (owner, 2026-11-09). Every name a person, an agent or a script gives
  follows the same rule, `names::validate`: `[a-z0-9._-]`, 1–64 characters, not all digits, not
  starting with `.` or `-`, no `..`, not ending in `.` or `.lock`. That covers sessions (and
  shells), worktrees, workspaces, profiles, change sets, a fan-out's name prefix, a lead's name
  for its worker, machines, paired devices, and the config panel's named members (agents,
  tools, …). A change set's name and a fan-out's prefix are at most 40 characters, kept short
  because member names are appended (`<set>-<member>`, `<prefix>-<agent>`); the fix offered
  then fits in 40 too (`names::validate_within`).
  - **Given names are refused, never rewritten.** `cs new my_fix` makes `my_fix`. A bad one is an
    error that offers the fix: `invalid name "My Feature": 'M' (uppercase) isn't allowed (only
    a-z 0-9 . _ -); try "my-feature"` (`names::tidy`). An empty name is refused, never replaced
    by a stand-in.
  - **`-` is the one replacement character.** The suggestion, generated names and the TUI's and
    web's live name fields all use it. As a name is typed (`names::typed`, the web's `slugify`)
    a character it can't hold becomes `-`, and what a name can't start with is dropped at the
    start. The fields say so: "A space or other odd character becomes - (a-z 0-9 . _ -)".
  - **Derived names may be tidied silently**, since they are no one's input: names made from a
    title, a prompt, a directory, a recipe's template (`names::derive`, always valid, or none so
    the caller picks its own fallback), a machine's name from what it calls itself, and names
    ptyro composes itself (`<set>-<member>`, a step's agent, `judge`). A taken name gets `-2`,
    `-3` (`names::numbered`), its base cut so the whole stays within 64.
  - **Project slugs** fold to the narrower `[a-z0-9-]`, at most 40 (`model::slugify`, built on the
    same `names::fold`). A slug is derived once and stored, and `<data>/worktrees/<slug>/` holds
    its worktrees and archives, so this fold stays as it was: re-adding a repository finds them
    again, and stored slugs keep working. A directory with nothing slug-like in its name gets
    `repo`; nothing else falls back to it.
- **Address syntax** used across CLI and protocol (resolved by the daemon):

  | Form | Example | Resolves to |
  |---|---|---|
  | handle | `7f3k`, `s:7f3k` | session by short id |
  | name | `brisk-rook` | session by name (error if ambiguous across projects) |
  | qualified | `myrepo/brisk-rook` | session by project + name |
  | `@focused` | | the session focused in the most recently active TUI |
  | `@last` | | most recently created session |
  | `@attention` | | oldest session with attention set |
  | `@all` | | every session, exited and dormant ones too (multi) |
  | `@live` (alias `@running`) | | the process runs: starting or running, never dormant (multi) |
  | `@active` | | busy now: an agent working or blocked, a shell running a command (multi) |
  | `@exited` | | ended and not resumable: exited or failed, never dormant (multi) |
  | `@dormant` | | hibernated agents, resumable (§7.4) (multi) |
  | `@working` / `@blocked` / `@idle` / `@done` / `@error` (alias `@errored`) | | all sessions in that status (multi) |
| `%N`, `ws%N` | `%3`, `work%3` | shell N of the context (or named) workspace's SHELLS (§23.5; proposed) |
  | `@run:<id>` | | all sessions of a run (multi) |

**One selector vocabulary** (owner, 2026-11-09). `ptyro_core::selector::SELECTORS` is the one
list of selectors: name, aliases, a line of help, and the state each picks. The daemon's
`resolve_many`, its "unknown selector; try …" hint, shell completion, `ptyro list --status`
(the state words without the `@`) and `ptyro wait --for` (the status words, plus `exit` and
`any-attention`) all read it, so a word means the same thing everywhere. The status words are
the status engine's own labels (`AgentStatus::label`): `error`, with `errored` accepted as an
alias in each of those places. The wire keeps `errored` (`AgentStatus`'s serde name), and the
web shows it as `error`. `@exited` never matches a dormant session, so `ptyro rm @exited` never
throws away a resumable conversation; `@all` does include dormant ones, so `ptyro rm @all`
forgets them too. A test fails when docs/commands.md's selector list drifts from `SELECTORS`.
`wait --for exit` is an event, not the `exited` state: it is reached when the process stops
running, hibernating included.

**[built M2]** Implemented now: handles (`7f3k`, `s:7f3k`, full id), names, `@last`, `@focused`
(the session most recently focused in any TUI, reported with `ClientMsg::Focus`), the multi
selectors in the table above (`@working`/`@blocked`/`@idle`/`@done`/`@error` are the status
`ptyro status` shows, whatever its source; until 2026-10-23 only self-reports counted, so
hook-driven agents never matched; `@dormant` and `@active` since 2026-11-09), name globs
(`build-*`, `a?c`), and `,` to join several (`a,b,@blocked`).
Results follow the session list's order without duplicates. Single-target requests accept a
selector only if it matches exactly one session (otherwise `ambiguous`). The CLI expands
selectors with `Request::Resolve`, so `ptyro stop @live`, `ptyro send 'agent-*' …` and
`ptyro rm @exited` work. `myrepo/name` arrived with projects (M3). `@attention` and `@run:<id>` are
**not built yet** (`@run:` works only as a mailbox recipient).

**[built M3]** `project/name` resolves a session by project and name. Projects are addressed by
slug, id suffix (≥ 4) or any path inside one of their checkouts. Worktrees are addressed by
`project/name` (`myrepo/@root` is the main checkout), a unique name, an id suffix, or a path
inside them, so CLI commands default to the worktree you're standing in. Ids are lowercase ULIDs
(`ptyro_core::model::new_id`). **[built UX1.24]** Default names: from the title or prompt
(`fix-flaky-test`, made unique), else for an agent session the agent and a corvid
(`claude-rook`), else a friendly pair. Script sessions get `<worktree>.<script>`. Create dialogs
show the suggestion before ⏎ (§11.2h).

**One resolution policy** (owner, 2026-11-09). Every kind of thing a user names resolves the same
way: sessions, worktrees, projects, workspaces, profiles, chats and their messages, runs, change
sets, devices, archived worktrees and machines. The code is `ptyro_core::lookup`; each resolver
uses it, so the rules can't drift apart.

1. **Order**: an exact name (a project's slug) → the qualified form where the kind has one
   (`project/name`) → the full id → a handle: the end of the id, **at least 4 characters**,
   with or without its prefix (`s:`, `w:`, `p:`, `c:`, `r-`). A shorter one never matches, and
   the not-found message says why. Runs match the end of their id, never a part of the middle.
2. **Case**: names and ids are lowercase, so the input is lowercased before names, ids and
   handles are compared (`Build` finds `build`; device and machine names likewise). Titles are
   not addresses. Chats still take a word of a title, as the last resort; a destructive request
   doesn't (`find_chat_by(addr, false)`).
3. **Ambiguity** is always `ambiguous` (CLI exit 4), and the message lists the candidates as
   they can be addressed, at most 5 and then "and N more": `2 sessions are named "build": say
   which, app/build or lib/build`; `2 runs match "k3x9": say which, r-ak3x9 (fanout) or …`.
   Several archived worktrees of one name are listed with their handles and ages. Each
   candidate reads "address (description)": `r-ef9999 (login)`, `a1b2c3d4e5f6 (Phone)`,
   `c:7f3k (Fix the build)`, `7f3k (myrepo/feat, archived 3d ago)`.
4. **Not found names the kind**: `no run matches "zz9q"`, `no device matches "phone"; the
   devices are laptop and tablet`. Where the set is small (devices, workspaces, profiles,
   machines) the message lists what there is.
5. **Archived worktrees** have an id (protocol 1.38; earlier ones get a stable one when the list
   is read) and resolve by name, `project/name` (the project by slug, id or path, as anywhere),
   or handle. `ptyro wt archives` shows the handle; the TUI and the web send the id.

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

### 3.3 Cargo workspace layout **[decided]**

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
│   ├── ptyro-drive/           # structured-session drivers: Claude stream-json, Codex
│   │                          #   app-server, ACP, OpenCode server (M10, §7.1)
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
| WSL2 | **Tier 1 [decided 2026-10-31, owner]** (Linux build). `make check` passes [V 2026-11-02]: WSL 2.6.2.0, kernel 6.6.87.2, Ubuntu 24.04.3, Windows 11 build 26200, systemd on, master at v0.0.46. All 73 test binaries pass, with bash 5.2 and zsh present but no fish or pwsh (`tests/win1.rs` skips those two), and no browser, so web-e2e didn't run. Survival after the last terminal (WIN0.1) is answered: WSL stops a distro about 15 s after the last Windows-side `wsl.exe` exits, systemd services and linger included, so the daemon holds WSL open itself while it runs (`[daemon] hold_wsl`, on by default; §20.10, WIN2). CI's `wsl` job runs `make test-wsl` on a hosted `windows-latest` runner, which has WSL2 (§20.10) |
| Windows 11 native (x64, ARM64) | **Tier 1 [decided 2026-10-31, owner]. The client is built (WIN3, 2026-11-03): `ptyro.exe` drives the daemon in WSL (`--remote wsl://`, §16.2), the TUI included. The native daemon runs, opt-in (WIN5, 2026-11-04: ConPTY, a named pipe; `client.default_remote = local`); WSL's stays the default until shells, processes and agents work natively (WIN6–WIN8)** (track D, RESEARCH DD3). It will bundle Microsoft's packaged ConPTY and upgrade live by handing pseudoconsoles over [decided 2026-11-04, owner; §20.13]. CI builds it and runs the client crates' tests on `windows-latest` (§20.11). Windows 10 is not supported |

Inside WSL, `ptyro_core::wsl` is the one rule (WIN2) [V 2026-11-02, WSL 2.6.2]: it is WSL if the kernel release (`/proc/sys/kernel/osrelease`) says `microsoft`, `WSL_DISTRO_NAME` is set, or interop is registered (`/proc/sys/fs/binfmt_misc/WSLInterop`). WSL1's release ends in `-Microsoft` and WSL2's says `microsoft-standard`. The distro comes from `WSL_DISTRO_NAME`, or from `wslpath -w /` (`\\wsl.localhost\<distro>\`) when that's missing: a systemd user unit, which is how `ptyro service install` runs the daemon, doesn't get the variable. About's `system` reads `linux x86_64, WSL2 (Ubuntu)`, its JSON has a `wsl` object, and `doctor` has a `wsl` section (WSL1 is a warning).

MSRV: latest stable at project start, pinned in `rust-toolchain.toml`; edition 2024. **[decided]**

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

### 4.1 Startup **[decided]**

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
daemon under a systemd user unit, a launchd agent, or on Windows a per-user logon task
(`ptyro daemon install-service`; §12 `install-service`, WIN10).

**`ptyro daemon start` typed by a user** (no `--foreground`, no `--spawned`) starts the daemon the
same way, detached, and returns once it accepts connections: `daemon started (pid N); logs in
<state dir>`, exit 0. When one already runs it says `daemon already running (pid N)` and exits 5
(conflict). `PTYRO_NO_AUTOSPAWN` does not stop it: the user asked by name. **[changed 2026-11-09]**
Before, it ran the daemon in the terminal with no output, so it looked hung, and Ctrl-C stopped it.
`--foreground` keeps that behaviour (plus logs on stderr) for service managers and debugging.

### 4.2 Actors **[decided]**

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
- **exec.** **[changed H2.7]** The image fd, `daemon.lock`, the PTY masters and the structured
  sessions' pipes are not left inheritable. They travel in a datagram socketpair by
  `SCM_RIGHTS`, in batches of 32, each batch carrying its old numbers
  (`ptyro_pty::handoff::carry`). Only the socketpair's receiving end is inheritable, named by
  `PTYRO_HANDOFF_SOCK`.
  - A child another task spawns while the handoff runs therefore inherits none of them.
  - `daemon.lock` is held throughout. Its old number is in `PTYRO_HANDOFF_LOCK`, and the new
    image takes the carried descriptor over (same device and inode) instead of locking again,
    so no second daemon can start in the gap.
  - The image records raw descriptor numbers. `adopt` looks them up among the carried ones
    first. A rollback puts them back at the old numbers (`restore_carried`).
  - **The listening socket is the exception.** On macOS a listener carried through a socket
    across `exec` breaks: every connection it accepts gets EOF after the first message. So it
    alone has `FD_CLOEXEC` cleared, at the last moment before `execve`.
  - Then `execve` `P daemon start --foreground|--spawned` with `PTYRO_HANDOFF_FD`. The new
    image marks everything close-on-exec again.
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

**[built WIN9, 2026-11-05] On Windows.** There is no `execve`, so the next daemon is another
process: the daemon's pid changes; every session's program, console, job, screen and status carry
on (WIN0.3 found a pseudoconsole outlives its creator when another process holds its handles; the
owner chose this over restart-and-resume, WIN0.5 d). `ptyro daemon upgrade [--binary P]`:
- **Which binary.** The one the daemon was started from, unless `--binary` names another. An
  install can't overwrite a running `ptyro.exe`, so `install.ps1` renames it to `ptyro.exe.old`
  and puts the new one at that path. The same `handoff-version` check refuses an image version the
  new binary can't read.
- **Quiescing** as on Unix (goodbye, the managed `ptyro serve` stopped, the store flushed), with
  one difference: each pipe is read on a thread of its own (`ptyro_pty::winread`), because a read
  on an anonymous pipe blocks a thread and a read that's under way when its future is dropped
  carries on, landing in a buffer the next owner never sees (WIN0.3's lost chunk). A pause asks
  the thread to stop and cancels its read (`CancelSynchronousIo`, asked again until the thread
  says it's idle: a read started just after a cancel isn't cancelled by it); what it read and
  nobody took goes into the emulator (or a structured agent's driver buffer) before the image is
  made. A reader that doesn't stop within 3 s isn't counted as parked, and the handover is called
  off rather than lose output.
- **Structured agents** get plain anonymous pipes on Windows (`ptyro_pty::anon_pipe`), not std's
  overlapped ones, which a blocking read can't drive: their output goes through the same reader.
- **The handover.** The old daemon starts `P daemon start --spawned|--foreground` detached, with
  `PTYRO_HANDOFF_IMAGE` (a file in `state/`) and `PTYRO_HANDOFF_PARENT` (its own pid). It copies
  every live session's handles into the new process (`DuplicateHandle`; nothing is made
  inheritable, so no other child can take one): a console session's two pipe ends, the
  `HPCON`'s three, the program and its job (`ptyro_pty::Handles`); a structured agent's three
  pipes, process and job. The image records the values the new process has (`win_handles`,
  `structured.fds`; left out when empty, so a Unix image is unchanged), and is written whole
  (temporary name, then a rename). The new daemon waits for it, adopts every session (its
  `HPCON` rebuilt from the three handles on the process heap; each handle checked to be open
  there, and a pipe where one is expected, before it's owned), and writes `handoff-<pid>.ack`.
  The old daemon then exits without closing anything.
- **The pipe and the lock.** The new daemon makes its pipe instances beside the old daemon's
  (`FILE_FLAG_FIRST_PIPE_INSTANCE` off), so the name never goes away: a client that finds no pipe
  starts a daemon of its own, which would have raced for the lock. Connections the old daemon
  receives meanwhile are dropped and retried. The new daemon takes `daemon.lock` once the old
  process is gone (a byte-range lock is released when its holder exits; retried up to 10 s).
- **Exits.** An adopted program's exit code comes from its process handle, which the new daemon
  holds; there is no reaping.
- **A handover that fails** (the new binary refuses the image or exits, or doesn't take over
  within 30 s, or a reader didn't stop): the new process is ended, its copies with it, and the old
  daemon carries on with every session. The status says so as a rollback (`generation` goes up,
  `rolled_back`), and a finding where people look says so with the fix first:
  `ptyro daemon restart --hibernate` (agents resume their conversations; shells and running
  commands start again). The TUI runs it, and the web's Health page offers "Restart the daemon",
  not another try at the same upgrade.
- **Seen on Windows 11** (2026-11-05, a release build, the packaged ConPTY): a ticker and a
  program waiting for input went through two upgrades in 572 ms and 537 ms; each program kept its
  pid, the ticker never stopped, typing reached the waiting one and its exit code (4) came back,
  the old daemon was gone, a second daemon was refused the lock, and a session started after
  worked. Upgrading to a ptyro from before WIN9 (it can't take a handover, finds the lock held
  and exits) left the daemon carrying on, said as a rollback. `tests/win9.rs` runs these in CI.
  [V]

---

## 5. Terminal layer: PTYs, emulation, input, queries

### 5.1 PTY management (`ptyro-pty`) **[decided]**

- Open with `pty-process` (`Pty::new()`, `pts()`, `Command::spawn(pts)`): child becomes session
  leader with the PTY as controlling terminal (`setsid` + `TIOCSCTTY`).
- **[built WIN5]** On Windows, `ptyro-pty`'s `windows.rs`: a pseudoconsole (the packaged ConPTY
  beside `ptyro.exe`, else Windows' own) and a Job Object per session; signals mapped (INT is a
  typed Ctrl+C, HUP/TERM close the console, KILL ends the job), the console closed when the job
  is empty (which ends the output, as a PTY's last holder does). §20.13 has the facts.
  **[built WIN7]** The probes there (`winproc.rs`): the foreground program is the newest child
  of the session's program in its job (through a `cmd.exe` running a batch shim), its name the
  image's stem, lowercase, or for an interpreter (node, bun, deno, python…) the script it runs
  (a package's generic `cli.js` gives the package's folder); its directory is read from its
  PEB; ports come from `GetExtendedTcpTable` with the Toolhelp32 tree; which session a process
  is in is the job that holds it (`IsProcessInJob`: a process can't leave a job, and pids,
  which Windows reuses, aren't followed up parents). `is_raw` is unknown: only Unix job control
  reads it.
- Each child gets its **own process group**; signals go to the group (`killpg`) so tool subprocesses
  (dev servers, test runners) die with the session. **[built H2.7]** The leader is left a zombie
  (seen with `waitid(WNOWAIT)`) until no other process is in its group, then reaped. Its pid, and
  so the group id, can't be given to a new process while a late `SIGKILL` to the group might
  still be sent. Stop sequence: `SIGHUP` → 3 s → `SIGTERM` → 2 s →
  `SIGKILL` (configurable per agent; Claude prefers `SIGINT` to end a turn cleanly).
- **Resize** via `TIOCSWINSZ` on the master; the kernel delivers SIGWINCH to the foreground group.
- **Ctrl-Z** **[built]**: the child leads its own session, so its process group is orphaned and the
  kernel drops the SIGTSTP it gets (§20.5). A program that suspends itself (Claude) then waits for
  a SIGCONT no shell will send. So when Ctrl-Z (a key, or `send --raw '\x1a'`) goes to the
  session's own process while it has the terminal in raw mode, the daemon watches for up to 1 s.
  If the terminal drops to cooked mode, the daemon sends SIGCONT to the group and announces
  `SessionResumed`, and the TUI shows a toast. A program that stays raw (Ctrl-Z as undo) is left
  alone, and so is a job inside a shell session: it has its own group, and its shell can `fg` it.
- **Inspection** (polled ≤ 1 Hz, only for visible or recently-active sessions):
  - foreground process group: `tcgetpgrp(master)`; name via `/proc/<pid>/comm` (Linux) or
    `proc_name` (macOS). **[built UX1.1]** A name that is a version number (`2.1.309`) gives way
    to argv[0]'s basename (`/proc/<pid>/cmdline`, macOS `KERN_PROCARGS2`), §20.5;
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
  | `PTYRO_SCRATCHPAD` | path of the scratchpad file (§13.6), in every session |
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

### 5.2 The `Emulator` trait (`ptyro-term`) **[decided]**

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

### 5.4 Row versioning (how deltas are computed) **[decided]**

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

### 5.5 Input (client → child) **[decided]**

Clients do **not** send raw bytes for keys; they send abstract events and the daemon encodes them
against the **child's current modes** (only the daemon knows them):

- `Key { code, mods, kind: press|repeat|release, text }` → `terminput` encoding:
  - if the child pushed kitty keyboard flags → kitty `CSI … u` per flags;
  - **[built WIN5]** else, when the console host asked for win32-input-mode (`CSI ? 9001 h`,
    what Windows' ConPTYs write first; the emulator follows it, alacritty doesn't), console
    records `CSI Vk;Sc;Uc;Kd;Cs;Rc _`, down then up: Shift+Enter and Ctrl+Space reach PSReadLine
    as themselves (§20.13);
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
`Shift-Enter`, etc., and therefore re-encode them faithfully for Claude Code. **[built WIN3]** On
Windows nothing is pushed: the console hands keys over as records with the virtual key and every
modifier, so those keys already arrive apart, and the TUI treats the terminal as one that
distinguishes them (§20.12).

### 5.6 Query responder **[decided]**

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
| OSC 133 A/B/C/D (shell integration) | prompt marks → jump-to-prompt in scrollback, command exit status for shell sessions. **[built M2]** Marks are stored *inside the grid*: at 133;A the daemon splits the batch and injects an invisible OSC 8 hyperlink (`ptyro:prompt`) that it closes at B/C/D or at the next newline. alacritty keeps hyperlinks per cell through scrolling and reflow, so the mark travels with its line into history, where line numbers can't follow. Rows carry `prompt: true` in frames and scrollback documents. Caveat: a program's own hyperlink that is open at that moment ends there. **[built WIN1]** Shell sessions get the snippets with no setup (§8.7); `ptyro shell-init bash\|zsh\|fish\|pwsh` prints them for other shells; they do nothing outside ptyro. |
| OSC 9;*n* (ConEmu's numbered commands) | **[built WIN1]** OSC 9 is two things: `9;<text>` is a notification (iTerm2), and `9;<digits>[;…]` is one of ConEmu's commands. `9;4` is progress; `9;9;<path>` is the directory (Windows Terminal uses it too, so a prompt set up for Windows Terminal reports it in ptyro unchanged): a POSIX path as it is, and in WSL a Windows path mapped (`C:\src` → `/mnt/c/src` under `[automount] root` from `/etc/wsl.conf`, `\\wsl.localhost\<distro>\p` → `/p`); elsewhere a Windows path is ignored. `9;12` is a prompt start (clink, cmd.exe), like 133;A. Every other number (sleep, message box, run a program…) is dropped, never shown as a notification. Before WIN1, `9;9` raised a notification at every prompt. |
| OSC 52 clipboard | forwarded to the controlling client if `clipboard.allow_write` (default on); writes over 1 MiB dropped by the emulator; reads always denied. The TUI writes it to its own terminal's clipboard only, never to its paste buffer (`]` pastes only your own yanks) |
| OSC 8 hyperlinks | preserved per cell; clients render as links |
| OSC 9;4 progress (ConEmu) | progress bar in pane border (Claude `terminalProgressBarEnabled`) |

### 5.8 Sizing across multiple clients **[decided]**

A PTY has exactly one size. Policy: **the controlling client wins** (tmux's `latest`). The
controller of a session is the client that most recently sent input to it or focused it. Other
viewers render the session at its true size: letterboxed (padding) if their pane is larger, cropped
with a "viewing at 120×40" badge and pan keys if smaller. A `--view-only` client never becomes the
controller.

**[built WEB1.8]** A paired device watches at the session's own size and never resizes it under
the person at the terminal, unless it has `control` scope and says so: typing from the web makes
it the controller (its OSC 52 clipboard writes then reach it), and `Subscribe{size, control:
true}` or `Resize` from it sizes the session like the TUI's own would. The web app does that only
when asked ("Resize the session to this screen"), remembers the size it saw and puts it back when
that is turned off or the page is left. Whoever types last decides, as everywhere else.

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

### 6.4 Message catalogue (protocol v1, **[decided]**; the current minor is `PROTO` in `ptyro-protocol`)

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

**[built] Protocol 1.11** (additive):
- A new event, `SessionResumed{session}`: the session's own process suspended itself on Ctrl-Z,
  and the daemon resumed it (§5.1). Older clients skip events they can't read.

**[built UX1] Protocol 1.12** (additive):
- `SessionInfo` gains `task` (the first prompt, at most 200 characters), `last_reply` (the
  agent's last message once its turn ended, at most 300) and `command`
  (`ShellCommand{text?, started_ms, ended_ms?, exit?}`: a shell's running or last command, from
  OSC 133 `C`/`D`). The TUI's sidebar rows show them (§11.2h).
- `SessionInfo` gains `inner_agent`, `SpawnSpec` gains `resume`, and a new request, `Adopt{addr}`,
  makes an agent started by hand in a shell a managed session (§8.7).
- Titles (§2.1): `SpawnSpec.title` and `WorktreeSpec.title`, `WorktreeInfo.title`, and
  `Retitle{addr, title, worktree}`. `SessionInfo.title` now means the title people read (its own,
  else its first prompt, else the window title), and the window title is `window_title`.
- `WorktreeArchive{addr, stop_sessions}`, `WorktreeRestore{addr}`, `WorktreeArchives{project}`,
  `WorktreeArchiveDelete{addr}` → `Notes`
  → `Archives{entries}`, and `WorktreeFinish{addr, integrate}` (§10.8).
- `SessionInfo` gains `summary` (`Summary{text, turn, at_ms}`), and a new request,
  `Summarize{addr}` (control scope), makes one now and replies `Session` (§11.2h).
- `ProjectRemove` gains `release` (let the project's running sessions go instead of refusing),
  and a new event, `ProjectAutoAdded{info, workspace, notes}`, says a repository registered on
  its own (§10.1).
- `WorktreeSpec.take_changes`, `Branches{project}` → `Branches{names}`, and
  `ProjectSuggest{addr, apply}` → `Suggestions{suggestions}` (§10.3, §10.4).

**[built UX1.25] Protocol 1.13** (additive): `SessionInfo.quick` and `SpawnSpec.quick`, and
`QuickShell{worktree?, cwd, create, env, size?, workspace?}` → `Session`: a worktree's (or
directory's) quick terminal, started when there's none and `create` is set (§11.2h).

**[built WEB1] Protocol 1.15** (additive), for the web client (§16.4): `Welcome.scope` (what a
paired device may do; absent for a local client); `Agents` → `Agents{agents:[{name, program,
installed}], default}`; `Editors` → `Editors{names, default}`; `WorktreeOpen{addr, editor?}`
(a terminal editor starts as a `Session`, any other on the daemon's machine: `Notes`; `control`);
`ProjectConfig{addr}` → `Text{text}` (the merged config with where each value comes from);
`Replay{addr}` → `Replay{replay:{size, duration_ms, steps:[{at_ms, keyframe, delta}], truncated}}`
(a recording as screen frames, sampled to about 300 steps, at most about 6 MB; the web player
applies them like a live screen); `Export{addr, format}` → `Text` (`asciicast` or `text`).
`Kill` and `Remove` (stopping what you started) are `control` scope, as the scope's description
always said. A device with `full` scope may `DevicePair` and `DeviceRevoke` (a code is still only
claimed by `ptyro serve`).

**[built WEB1.9] Protocol 1.16** (additive), the web app's server as a setting (§16.3):
`Web{on?, via?}` → `Web{web, notes}`. With `on` left out it asks how the server is; `Some(true)` or
`Some(false)` turns `[remote] serve` on or off (`full` scope): the daemon edits the config file
(`ptyro_core::config::set_serve`, comments and the rest kept), reloads it, starts or stops the
server at once and waits up to 12 s for it to say where it listens. `via` (`localhost`,
`tailscale`, `open`; `here` and `lan` are the old words) says who can reach it, else the config's
own choice stands, else Tailscale when it is running on this machine, else `open`.
`DaemonStatus.web` carries the same `WebServe{wanted, up, pid, url, listen, access, port,
ca_fingerprint, since_ms, restarts, last_exit, by_hand, log}` (`access` and `port` are ACC1's).

**[built WEB1.8] Protocol 1.17** (behaviour; no new message): a device with `control` scope may
`Subscribe` with `control` and a `size`, and `Resize`; its `Input` takes control of the session
(§5.8). Devices below `control` still watch at the session's size and their input is refused.

**Protocol 1.18** (tried in v0.0.8/0.0.9, removed in v0.0.10; the number stays, it is never reused
or lowered). It added an optional `pad` selector to every `Scratch*` request, a `ScratchPads`
request and reply, `pad`/`name` on `PadDoc`, `pad` on `ScratchChanged`, `pad_project` on
`SessionInfo`/`SpawnSpec` and `pad_sel` on `WorktreeSpec` (NOTE1.7, a pad per project). The owner
dropped the feature (§13.6), so v0.0.10 has none of them, and **there is one scratchpad again**.
How the wire stays compatible both ways, with nothing kept for the dead fields:

- None of the wire types uses `deny_unknown_fields`, so a field a peer does not know is ignored.
  A 0.0.8/0.0.9 client's `pad` on a request is dropped and the request acts on the one pad (a client
  that asked for a project's pad gets the global one, which is the whole point of the change); a
  `pad`/`name`/`pad_project`/`pad_sel` in a document, event, session or spec from such a daemon is
  ignored (this includes a handoff image, whose sessions are `serde(default)` for the new fields).
- A v0.0.10 peer sends none of them; a 0.0.8/0.0.9 one reads the missing ones as their defaults
  (every one was `#[serde(default)]`: the global pad, no project).
- The one removed message, `ScratchPads`, is, to a v0.0.10 daemon, a request it cannot read: it answers with an
  `invalid` error ("the daemon couldn't read this request") and the connection survives
  (`conn.rs`); a 0.0.9 TUI
  or `ptyro doctor` ignores that error (its pad count then comes from `ScratchChanged`).
- Not versioned further: removing fields is not additive, but it is invisible to a peer that
  ignores unknowns, and `PROTO` stays 1.18 so nobody sees a downgrade. The test
  `a_leftover_pad_selector_is_ignored_and_means_the_global_pad` pins it.
- One edge: a project pad's editor session still running in a 0.0.9 daemon that hands off to
  v0.0.10 becomes "the scratchpad's editor" and still shows the old project file; quit it
  (`:q`) and the next `C-q ;` starts the global one.

**[built, 2026-11-09] Protocol 1.39** (additive): `ChatDelete` gains `confirmed` (default
false). The daemon resolves its chat by an exact address only (a title is refused, exit 4, its
matches listed) and, unconfirmed, answers `NeedsConfirmation` with the question every client
asks ("Delete the chat “T”? It goes from every list…", §12.1). The TUI and the web ask first and
send it confirmed; `chat rm` asks through `with_confirmation_as`. An older client's unconfirmed delete
is asked about instead of done.

**[built 2026-11-09] Protocol 1.39** (additive): `Request::Remove` adds `stop` (absent: false): stop a
running session, then forget it (`rm --stop`, Stop and remove; §7.3). An older daemon ignores
it and refuses a running session as before. `Error` adds `fix` (absent: none): a
refusal's way through, `{fix: force|stop_sessions|stop|trust, label, fingerprint?}` (§6.6); an
older client ignores it.

**[built] Protocol 1.39**: `ChatUpdate.auto_title` asks for a new automatic title (§22.8). An
empty `title` now clears a chat's title (before 1.39 it asked for an automatic one), as `Retitle`
does for a session or a worktree. Clients send `auto_title` with an empty `title`, so a daemon older
than 1.39 (which ignores `auto_title`) still makes a new one; an older client's empty title now
clears it.

**[built 2026-11-09] Protocol 1.38** (additive): `WorktreeRename{addr, name}` → `Worktree{info}` and
`ProjectRename{addr, name}` → `Project{info}` (§2.3). Both are `full` scope, like
`WorkspaceRename`. Watchers hear `WorktreeUpdated` (for a project, one per worktree, carrying
the new `project_slug`) and `ProjectUpdated`. An older daemon answers "unsupported request".

**[built SH.14] Protocol 1.37** (additive): `DomainEvent::HostLoad{load?}` (every watcher,
when the 5 s sample changes; `None` once `[ui] load` is off) and `DaemonStatus.load?`:
`HostLoad{avg: [1, 5, 15 min in hundredths], cpus}`. Never mirrored from another machine. An older
client ignores both.

**Protocol 1.38** (additive): `ArchiveEntry.id`, so archived worktrees that share a name can be
told apart (§2.3). Clients send it to `WorktreeRestore`/`WorktreeArchiveDelete`; an older daemon's
entries have none (empty), and the clients then send `project/name` as before.

**[built BOOT7] Protocol 1.36** (additive): moves to the recommended installer (§24.9). A setup
entry installed another way carries `moves` (the offer: `from`, `to`, `uninstall`, `admin`,
`keeps`) unless `updates.keep_installer` keeps it, and its state `system` when its folder is an
administrator's (and `prefix`, npm's); a ticked one's `action.moving` is the move.
`Request::SetupRun` adds `moves`: the agents installed another way to move (ticked where offered,
or Move on Health). Every other one stays as it is, so a client that names none, or predates 1.36,
never moves anything (its update runs that installer's own). `updates::Outdated` adds `move_to`.
An older daemon sends no `moves` (nothing is offered) and ignores the field; an older client
ignores them.

**[built BOOT5] Protocol 1.35** (additive): out of date, offered (§24.7). A setup entry in the
`other` state (installed another way: npm, Homebrew) adds `latest` when a newer version is out,
and ticking it plans that installer's own update; `outdated` entries now arrive whenever the
daemon remembers a newer version (`<state>/versions.json`), and ticking one runs its update.
`Request::Updates{note}` → `Reply::Updates{offered, note}` (`read` scope): the agents out of date
on the daemon's machine, and with `note` the start-up note's, marked said (`note: true` needs
`full` scope: it uses up the owner's once-per-version note; `note: false` is `read`). `setup::Event::Failed`
adds `in_use` (an update refused because the program runs: a client offers to try again). An
older daemon sends no `latest` or `in_use` and refuses `Updates` (the TUI then offers nothing); an
older client ignores them.

**[built BOOT3] Protocol 1.34** (additive): the web's setup checklist (§24.6). `Reply::SetupPlan`
adds `lines` (each entry's words, `checklist::Line`: `about`, `how`, `why`, `state`, as the TUI
says them), `default_agent`, `running` (`SetupRunning{run, plan, events}`: the run going now
and its latest 500 events, each `SetupLogged{seq, side, event}`), and `held_by` (who holds the
machine's setup lock when it isn't the daemon's run: a terminal's or the TUI's install); `DomainEvent::SetupProgress`
adds `seq` (from 1 in its run). An older daemon sends none of them (no lines, nothing running,
`seq` 0: every event counts); an older client ignores them.

**[built BOOT4] Protocol 1.33** (additive): `SpawnSpec.unrecorded` (§7.6): a session that is never
recorded to disk, for logins (§24.4). An older daemon ignores the field and records as before.

**[built BOOT8] Protocol 1.32** (additive): setup's sides (§24.3). `SetupPlan` and `SetupRun`
take `wsl_picks?` (what to tick in WSL's distro, on a Windows daemon); `Plan.wsl` says what WSL
is (usable, unusable with why and the fix, or "this is WSL"); each entry's `side` is `windows`,
`wsl` or `local`; `SetupProgress` carries `side`, and each side ends with its own `finished`. An
older client sends neither and gets this computer's side only.

**[built BOOT1] Protocol 1.31** (additive): setup (§24.2). `SetupPlan{picks?}` → `SetupPlan{plan}`
(`read`), `SetupRun{picks}` → `SetupStarted{run, plan}` (`full`), and the event
`SetupProgress{run, event}` to every watcher. An older daemon refuses the new requests.

**[built SH.13] Protocol 1.30** (additive): SHELLS windows (§23.17). `ShellSplit{addr, dir,
env, size?}`, `ShellJoin{addr, into?, dir}`, `ShellLayout{addr, layout}` → `Session`.
`SessionInfo.pane/layout`. `%N.P` addresses a pane. An older daemon refuses the new requests.

**[built SH] Protocol 1.29** (additive): SHELLS (§23.12). `ShellMove{addr, slot}`,
`ShellPlace{addr, loose, worktree?}`, `ShellWatch{addr, watch?}` → `Session`. `SessionInfo.loose/slot/
watch/scratch/lost` and `SpawnSpec.loose/slot/scratch`. `%N` and `ws%N` resolve wherever a session
address goes.

**[built WIN4] Protocol 1.27** (additive): `SpawnSpec.command`, a command line as typed. The
daemon runs it through its own default shell (`$SHELL -c` on Unix; on Windows PowerShell, §3.4),
so a client no longer builds the shell's argv itself: the TUI's "run a command" used the
*client's* `$SHELL` and the web `sh -c`, which a Windows client would have got wrong for its WSL
daemon. It needs full scope from a paired device, like any command line (§16.4), and `again`
leaves it alone. An older daemon ignores the field and starts a shell.

**[built UX3.7] Protocol 1.26** (additive): quick new is per worktree and always an agent.
`LastLaunch` gains `worktree?` (an address) and answers what `again` starts there, always
`Some`: the agent that worktree last started (agent, profile, argv), else its project's
`default_agent`, else `[agents] default` (claude). `workspace` is no longer read. The daemon
remembers the last agent per worktree id in `kv` (`last_agent`), from `Spawn` and `WorktreeNew`,
never a shell, a command line, a tool, a quick terminal or the scratchpad's editor, and forgets a
removed worktree's. `again` therefore always names an agent. An older client asking by workspace
gets the default agent.

**[built CHAT1] Protocol 1.22** (additive): chats (§22.14 as built). Requests `ChatNew`, `ChatList`,
`ChatRead` (the draft's `ChatGet`: that name was a structured session's transcript already),
`ChatSend`, `ChatAnswer`, `ChatStop`, `ChatUpdate`, `ChatDelete`, `ChatExport`; replies `Chat`,
`Chats`, `ChatTranscript`; `ClientMsg::ChatSubscribe` / `ChatUnsubscribe` and `ServerMsg::ChatLive`;
events `ChatUpdated`, `ChatRemoved`; `SessionInfo.chat`; `DaemonDoctor.chats`.

**[built TS1] Protocol 1.21** (additive): Tailscale's certificate (§16.3). `WebServe` gains `cert`
(`tailscale`, `ptyro`, `own` or empty) and `cert_note` (why Tailscale's isn't in use, with the fix).
There is no setting and no question: it is automatic over Tailscale (owner, 2026-10-29).

**[built MM1.B] Protocol 1.20** (additive): many machines (§16.4b). `ClientKind::Machine`;
`machine` on `SessionInfo`, `WorkspaceInfo`, `ProjectInfo`, `WorktreeInfo`, `ProfileInfo`,
`InboxEntry` and `Usage` (absent here); `DevicePair.machine` and `DeviceInfo.machine`;
`MachineList` → `Machines{machines}`, `MachineAdd` / `MachineHide` / `MachineRetry` → `Machine{info}`,
`MachineRemove` → `Done`; events `MachineUpdated{info}`, `MachineRemoved{name}`. `MachineList` is
`read` scope, the rest `full`. A daemon of 1.19 ignores the new fields and doesn't know the
requests (answered as unreadable); a link needs the same version anyway.

**[built UX3.2] Protocol 1.19** (additive): quick new. `SpawnSpec` gains `again` (fill in the
agent, `argv` and `profile` from what its home workspace last started; the default agent when
nothing has) and `tool` (a client's tool, not remembered). `LastLaunch{workspace}` →
`LastLaunch{launch?: {agent?, profile?, argv?}}` (`read` scope). The daemon remembers the last
`Spawn` per workspace, and a `WorktreeNew`'s agent, in `kv` (`last_launch`), never a tool, a quick
terminal or the scratchpad's editor. `again` with no agent named is `full` scope (it may start a
command line); the web asks `LastLaunch` and names the agent instead, which is `control`.

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

**A refusal says what to do in words every client shares** (owner, 2026-11-09). The CLI, the TUI
and the web all show the daemon's message, so it never names a CLI flag or tells you to run a
CLI command: "stop them first", "archive it to keep it, or remove it anyway to discard it", not
"use --stop-sessions" or "use --force". Each client then offers the way through in its own idiom:
the CLI adds the flag, the TUI and the web a button or a confirmation. A test
(`refusals_name_no_cli_flag`) keeps flags out of the daemon's refusals.

**The way through is data, not words** (protocol 1.39). A refusal that has one carries
`Error.fix`: `{fix, label, fingerprint?}`, where `fix` says what the same request says the
second time: `force`, `stop_sessions`, `stop`, `trust` (with the `fingerprint` read), or
`release`. The daemon sets it exactly where that way applies (removing a running session: `stop`,
"Stop and remove"; a busy worktree's remove or archive: `stop_sessions`; unsaved work, a mid-turn
convert, a daemon stop with sessions, a change set's restore, drop or remove: `force`; an
untrusted recipe or template: `trust`; forgetting a project with running sessions: `release`,
"Let them go"), never for a refusal it doesn't undo (removing while it restarts, a restart while
it runs). The machine's own operations do the same (a profile in use: `force`, "Remove anyway"),
and the web's HTTP API passes `fix` through. Clients act on `fix`, never on an error code that
happens to go with it (owner, 2026-11-09). The CLI prints "(again with `--force`: remove anyway)"
(`cli::said`; `project rm` at a terminal asks "Let them go?" instead); the TUI turns it into a
toast button that resends the request (`Request::with_fix`); the web asks with a confirmation
(`kit.ts` `withFix`, in `ask()`), and `ask()` resolves with the retry's reply, so the caller goes
on (leaves a removed thing's page) as if the first request had worked. Left as words: a message to an agent (which
drives the CLI), one about another computer or the daemon process itself, and the ones that
suggest a different action, which the TUI's `SUGGESTED` table still turns into one (`` `ptyro
project trust` `` and the like).

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

**[built UX3.2]** The real struct also has `again` and `tool` (§6.4, protocol 1.19).
**[proposed SH, §23.12]** `loose`, `slot` and `scratch`: a loose spawn starts in the directory given but is never placed under the worktree that holds it.

**[built]** Where it starts: a worktree given by address (`Worktree`) starts at its root. A
directory (`Cwd`) starts exactly there, even inside a registered worktree; the innermost
worktree that contains it only places the session (project, profile, sidebar). Before
2026-10-23 a `Cwd` inside any worktree was moved to that worktree's root, and UX1.5's
auto-registration made every directory in a repository hit it (`ptyro new -C repo/sub` ran in
`repo`).

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
- **Stop, kill, remove** (owner, 2026-11-09): one meaning each, the same in the CLI, the TUI, the
  web, the help and the daemon's messages.
  - **stop** (`ptyro stop`, `x`, Stop; `Kill{signal: None}`): an agent's turn in progress is
    interrupted and it gets a moment to exit (the adapter's `stop()` steps, §7.4); then the
    process group gets HUP, TERM and KILL. It does not wait for the turn to finish. The session
    stays listed, `Exited`.
  - **kill** (`ptyro kill`, `X`, Kill; `Kill{signal: Some(9)}`): SIGKILL at once, no chance to
    clean up. The session stays listed. `ptyro kill -s N` sends signal N instead. (Until
    2026-11-09 `ptyro kill` was a stop followed by a remove; `--keep` is now a hidden no-op.)
  - **rm** (`ptyro rm`, `session.remove`, Remove; `Remove{stop: false}`): forget an ended
    session. A running one is refused ("NAME is still running: stop it first"); `rm --stop`,
    the TUI's and the web's Stop and remove (`Remove{stop: true}`, protocol 1.39) stop it, then
    forget it. A removed dormant session never comes back.
  - `[ui] confirm_kill` (default on) makes Stop, Kill, and Stop and remove ask first, in the TUI
    and the web alike (the web reads it where the device may read the config, else always asks).
    Removing an ended session never asks.
- `Exited` sessions stay visible (greyed, with exit code and final screen) until removed; their
  recorder log remains readable (`ptyro read --scrollback`).
- `Dormant` = process not running but resumable; the last screen is shown dimmed with a
  "⏎ resume" hint.

### 7.4 Hibernation **[decided]**

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
  (time-faithful playback in a pane); (3) `ptyro export --format asciicast` (asciinema v2); (4) debugging
  emulator bugs from real-world byte streams (attach the log to an issue).
- Per-agent opt-out (`record = false`) and global `recorder.enabled`. Logs may contain secrets that
  scrolled through the terminal: files are `0600`, and `ptyro gc` prunes logs of removed sessions.
- Per session, `SpawnSpec.unrecorded` (protocol 1.33, BOOT4): never recorded. Logins use it
  (`profile login`, the web's Log in, §24.4), so a code the sign-in prints or the person pastes
  never reaches disk. It is kept across `restart`, but not in the store: a login session
  restarted after the daemon itself restarted is recorded as usual.

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

### 8.1 Adapter trait (`ptyro-agents`) **[decided]**

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
  `settings.json`, printing its output, so the user's statusline is unchanged. It runs that
  command where Claude would (`shell::claude_command_argv`): `sh -c` on Unix; on Windows in Git
  Bash (`CLAUDE_CODE_GIT_BASH_PATH`, else where Git for Windows puts it), else in Windows
  PowerShell (WIN8). On Windows the daemon also starts Claude with `CLAUDE_CODE_GIT_BASH_PATH`
  set to the Git Bash it finds, unless the daemon's environment, the profile's `env` or the
  agent's `env` names one already, so Claude's Bash tool, its hooks, the relay and project
  scripts use one bash (a profile's `env` is where to choose another).
- **Prompt delivery (changed from the proposal):**
  - Wait for a reliable `Idle` (hook or 7501), or for `ready(screen)` (the `❯` input box with
    no dialog on screen). Never type while `Blocked`; give up after 120 s with a "couldn't
    deliver your prompt" attention.
  - Then paste the text, wait 60 ms, and send Enter as an **encoded key event**. Claude speaks
    the kitty keyboard protocol and ignores a bare `\r` written straight after the text: a burst
    ending in `\r` reads as pasted (§20.1).
  - **[WIN8, 2026-11-05]** For Claude a reliable idle isn't enough (`ready_needs_screen`): its
    `SessionStart` comes while a resumed one still loads its transcript, and an Enter then is
    encoded for keyboard modes it hasn't turned on (lost once in two tries on Windows). Its input
    box must be on screen too: the `❯` (or old `│ >`) line right under a rule, so the `❯`
    prompts of the conversation above it don't count, and a dialog counts only below the last box.
  - **[WIN8]** After typing, ptyro watches the input box (`pending_input`): while this very prompt
    still sits there unsent and the agent waits for input (idle, done, or not yet said), it
    presses Enter again, twice at most within ~10 s; never while working (that Enter would be lost
    too) or blocked (Enter would answer the dialog). Codex 0.166 says idle as it starts and is
    then busy for a moment: the first prompt's Enter landed then and was dropped, every time.
    It relies on an agent emptying its box as it takes a prompt, as Claude and Codex do; the
    fake agent does too since 2026-11-07 (§18.2: it didn't, and a second Enter answered a
    permission dialog "no").
  - **[2026-11-07] Enter is pressed again only where an Enter is known to be dropped.**
    Each adapter says whether it drops one (`Adapter::drops_enter`): only Codex does (0.166,
    above). For every other agent, Claude included (Enter on its dialogs is "Yes", and its box
    has never been seen to drop an Enter), a prompt that still sits in the box is never pressed
    again: it is reported. So a second Enter can't land on a Claude dialog.
  - For Codex, a press needs every sign that it never took the prompt; any doubt is no press:
    - the box shows this prompt, unsent: all of it, or its start when it wraps (the box's
      first line);
    - no hook or OSC 7501 report has come since the Enter;
    - the status is idle, done or not yet said, never blocked, and no dialog its adapter
      recognises (`on_screen`, `screen_status`) is on screen;
    - two looks 500 ms apart saw the same screen, with no output between them (an agent that
      took the prompt redraws; a dropped Enter leaves the screen still);
    - just before the key is written, no output and no report came since that second look, and
      the session still runs.

    What is left for Codex: a Codex that took the prompt, then froze its screen for over a
    second, then showed a dialog it doesn't recognise as one, could read that Enter. Its own
    dialogs are recognised, and none has been seen to come that way.
  - **Reported, never silently dropped.** Whenever the prompt still sits in the box and nothing
    is pressed (a dialog on screen while it still shows, said at once; at the end of the ~10 s
    watch, any agent but Codex, an agent that spoke since the Enter, and Codex when no press was
    safe; Codex after two presses), the session gets a `prompt_unconfirmed` event with why, and
    attention: "its prompt may not have been sent (…): look, and press Enter in it if not".
    **[2026-11-07]** A dialog is looked for before a report: it stays on screen until answered,
    so the report says "a dialog" however late the look. An agent that spoke (a hook, OSC 7501)
    is no longer reported at once: hooks come by socket and the box's redraw through the
    terminal, so a lagging screen can still show a prompt it took; it is watched to the end and
    reported only if the prompt is still there.
  - **Tests.** m4, with a Claude whose redraw lags (`PTYRO_FAKE_LAGGY_BOX`: the taken prompt
    stays in its box, its slow hooks leave the status idle):
    - `a_claude_prompt_left_in_its_box_is_reported_not_entered_again` (the dialog at once);
    - `a_dialog_drawn_between_two_looks_is_reported_not_answered` (the dialog 700 ms later);
    - `a_dialog_no_adapter_recognises_is_never_answered` (`PTYRO_FAKE_PLAIN_DIALOG`);
    - `a_wrapped_claude_prompt_left_in_its_box_is_reported`.

    For Codex: `a_prompt_whose_enter_was_lost_is_sent` and
    `a_wrapped_prompt_whose_enter_was_lost_is_sent`. The decision itself is `look_at_box`, with
    table tests. The four Claude tests and the wrapped Codex one fail against the guard of the
    first review round, which still pressed Enter for Claude, reported nothing when a dialog came
    and lost a wrapped prompt; `a_prompt_whose_enter_was_lost_is_sent` is older, and passes
    against it. The Claude tests assert the report and that nothing was pressed, not its
    reason: which look sees the prompt first moves with a runner's load.
- **A session starting doesn't end a turn [WIN8]:** a `SessionStart` hook that comes while the
  session is working moves nothing to idle (its conversation and model still count). Codex 0.166
  sends it only with the first prompt, after OSC 7501's working (verified live, 2026-11-05).
  Only a turn holds it off: a *reliable* working (above low confidence: a hook, OSC 7501, a
  title or the screen), or the output heuristic's guess made after a prompt was typed (the agent
  at work on it). A working guessed from the agent's start-up output, before any prompt, is no
  turn: `SessionStart`'s idle replaces it **[fixed 2026-11-07]**. When the first hook is slow
  (CI's Windows and WSL runners, a loaded laptop), the heuristic guessed working before it came,
  and the start's idle was dropped; the guess then stood until it went stale (5 m). An agent
  whose input box ptyro doesn't read (Codex) never got its prompt meanwhile, since delivery
  waits for a reliable idle (the m7 "fake Codex never gets its prompt" flake on Windows).
- **Folder trust:** `on_screen` recognises the trust dialog and reports blocked(question)
  "trust this folder?". `ready()` is false while it shows, so ptyro never types into it. So is
  the first-run "Claude in Chrome extension detected" question (2.1.318), blocked(question)
  "use your Chrome browser?"; its keys are Down, Enter (yes) and Enter (no, preselected).
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
  1. One `-c 'hooks.<Event>=[{hooks=[{type="command",command="ptyro hook codex <event>",timeout=<t>}]}]'`
     per event, the timeout written out (600, or 1 for `Interrupt`) so the hash below never rests
     on Codex's defaults. These become the *session-flags* layer (key source `/<session-flags>/config.toml`).
  2. One `-c 'hooks.state={"/<session-flags>/config.toml:<event_snake>:0:0"={trusted_hash="sha256:<h>"}, …}'`.
     Codex reads hook trust from the user layer **and the session-flags layer**
     (`hook_states_from_stack`), so this pre-trusts *exactly our* hooks for this invocation. It does
     not bypass trust for repo or user hooks, which `--dangerously-bypass-hook-trust` would.
  3. `<h>` = SHA-256 hex of the compact, key-sorted JSON of
     `{"event_name": "<event_snake>", "hooks": [{"type":"command","command":"<cmd>","timeout":600,"async":false}]}`.
     `timeout` is the resolved one: ptyro's explicit value, else Codex's default (600; 1 for
     `session_end` and, from 0.164, `interrupt`, which 0.164 also clamps to 1–3 s). `matcher` is
     omitted when absent. Reproduced bit-for-bit for 6 events on 0.143.0 and for all 9 ptyro
     installs on 0.164.0 (`crates/ptyro-agents/tests/fixtures/codex-<version>/trust-hashes.json`;
     0.164's from `codex app-server`'s `hooks/list`, which reports each hook's `currentHash`,
     `trustStatus` and `timeoutSec` without a login: the way to check a new version). The
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
  mode. Fixtures: `crates/ptyro-agents/tests/fixtures/codex-0.143.0/hooks/`, and
  `codex-0.164.0/hooks/Stop.json`. `PermissionRequest` is **not yet captured**.
- **Task and reply** **[verified 2026-10-23, 0.164.0]**: `UserPromptSubmit.prompt` is the task;
  `Stop.last_assistant_message` is the turn's reply (the sidebar's `task` and `last`, §11.2h).
  `notify` also carries `last-assistant-message`, but it fires for Codex's side threads too: after
  each first turn, a separate thread (its own `thread-id`, same `client: "codex-tui"`) writes the
  conversation's title and reports `{"title":"…"}` as its reply. Side threads run no hooks. The
  daemon drops a `notify` whose `thread-id` isn't the session's conversation, so a side thread can
  neither replace the reply, end a turn, nor take over the conversation id that resume uses.
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
0.143.0 and 0.164.0 fixtures, and a `notify` that chains the user's own. **[fixed 2026-10-23]**
Before, `Interrupt` was hashed with 600 s; 0.164 resolves it to 1 s, so every Codex session
stopped on "hooks need review" and, skipped, ran no hooks: no task, no reply, status from the
title alone.
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

### 8.6c Which model an agent runs **[built UX4.1]**

`SessionInfo.model` is the model's id as the agent names it (`claude-opus-5-5`, `gpt-5.6-sol`).
It is live state: kept in `Meta`, never in `ptyro.db`, so a dormant session shows none until its
agent speaks again (a resumed Claude sends `SessionStart` with it). The `Signal::Model` an
adapter returns sets it; the daemon keeps at most 80 characters and announces a change like any
other. Sources, by agent:

| Agent | Source | Status |
|---|---|---|
| Claude (terminal) | `SessionStart` hook payload `model`; on `Stop` the last assistant turn of `transcript_path` (`message.model`, the file's last 256 KiB; `<synthetic>` turns skipped), so a `/model` switch shows once the turn that used it ends | **[verified 2.1.304 fixture; a real transcript]** |
| Claude (structured) | `system/init` `model` in stream-json | **[verified 2.1.305 fixture]** |
| Codex | `model` in the payload of every hook (`SessionStart`, `UserPromptSubmit`, `Stop`); `notify` carries none | **[verified 0.143.0 and 0.164.0 fixtures; a real rollout's `turn_context`]** |
| Gemini | its hooks carry none (0.64.0 fixture); the chat file's shape is unconfirmed | **[spike UX4.2]** |
| OpenCode | `providerID`/`modelID` on a message from `GET /session/:id/message`, by docs and clients | **[spike UX4.2; not installed]** |

Display: `ptyro_core::session::model_label` (and `modelLabel` in web/src/screen.ts, same table of
tests) shortens `claude-opus-5-5` to `opus 5.5`, `claude-3-5-sonnet-20241022` to `sonnet 3.5` and
drops a provider prefix; a non-Claude id is shown whole. The TUI pane header of an agent session
says `name · claude opus 5.5`; `ptyro ls` gets a MODEL column once any session has one (`--json`
has the raw `model`); the web shows it in a session's card, its header pill and the search.

### 8.7 Shell and generic adapters

- `shell`: `[shell] command`, else `$SHELL -l`. A bare bash, zsh, fish or PowerShell (`ptyro new
  -- pwsh`, with at most login, interactive, no-banner or `-NoProfile` switches) is a shell
  session too, not a command (`ptyro_core::shell::interactive`). Status: `idle` at prompt,
  `working` while a command runs (from OSC 133), else unknown.

**[built WIN1] Shell integration with no setup** (`ptyro_core::shell`, `[shell] integration`,
default on). When a shell session starts, the daemon writes the snippets under
`<data>/integrations/shell/` and starts the shell so that it reads the user's own startup files
first and ptyro's snippet last. The session keeps the argv it was asked for (`Spawned.exec` is what
runs), so a restart integrates afresh:
  - **bash:** `bash --init-file <dir>/bash-init -i`. For a login shell (`-l`, with
    `PTYRO_BASH_LOGIN=1`) the init file reads `/etc/profile` and the first of `~/.bash_profile`,
    `~/.bash_login`, `~/.profile`, as bash would; otherwise `~/.bashrc`. `$0` is `bash`, not
    `-bash`.
  - **zsh:** `ZDOTDIR=<dir>/zsh` (the user's own in `PTYRO_ZDOTDIR`, or `PTYRO_ZDOTDIR_UNSET`).
    ptyro's `.zshenv`, `.zprofile` and `.zshrc` each source the user's file with `ZDOTDIR` set
    back to theirs (their `.zshenv` may move it), then point it at ptyro's again. `.zshrc`, the
    last one read from there, loads the snippet and leaves `ZDOTDIR` as the user had it (unset, if
    it was), so `.zlogin` and every child see theirs. macOS's `/etc/zshrc` sets `HISTFILE` from
    `ZDOTDIR` while it points at ptyro's; `.zshrc` puts it back.
  - **fish:** `--init-command 'source <dir>/ptyro.fish'`, which runs after fish's config.
  - **PowerShell:** `pwsh [-Login] [the user's switches] -NoLogo -NoExit -Command ". '<dir>/ptyro.ps1'"`,
    which runs after `$PROFILE`. The snippet wraps the prompt rather than replacing it
    (oh-my-posh, starship and the user's own keep drawing, and are handed `$?` and
    `$LASTEXITCODE` as the command left them). The exit status is 0 on success, else a native
    program's `$LASTEXITCODE`, else 1: a cmdlet failed, which shows as a new `$Error` entry with
    `$LASTEXITCODE` unchanged since the command started. 133;C with the command line comes from
    wrapping PSReadLine's `PSConsoleHostReadLine`. The directory is OSC 7 on macOS and Linux and
    OSC 9;9 on Windows, only for a FileSystem location. It also runs on Windows PowerShell 5.1:
    no `` `e ``, no `$IsWindows`.
    **[V] 2026-11-01, pwsh 7.6.9 / PSReadLine 2.4.5 on macOS:** PSReadLine drops the Enter of a
    line typed while it starts, after the prompt is drawn (about 0.5 s here, and longer on a
    loaded CI runner). The line's text survives on the prompt; only the Enter is lost. The same
    happens with ptyro's integration off, so it is PowerShell's, not ptyro's. Binding Ctrl+J to
    AcceptLine doesn't help, because the Enter isn't arriving as a line feed. Something that types
    into a new pwsh session must check that the line ran and press Enter again if it didn't
    (`tests/win1.rs` does).
  - **bash before 4.4** (macOS's 3.2) has no `PS0`. A DEBUG trap sends 133;C for the first
    command after a prompt, but only when no other DEBUG trap is set. Before WIN1, bash on a Mac
    never said `working`.

  Every snippet guards on `PTYRO` and `__ptyro_integrated`, so a `shell-init` line in an rc file
  and the automatic one never stack. When the files can't be written the shell starts as it is
  (logged). Off (`[shell] integration = false`), the TUI's health check offers the rc line again,
  and `ptyro doctor`'s "shell" section reports it as a note. That section also says which shell
  new sessions run, whether integration reaches it, and that PowerShell is installed when it
  isn't the default.
  The rc file is `~/.bashrc`, `~/.zshrc`, `<config>/fish/config.fish` or (off Windows)
  `<config>/powershell/Microsoft.PowerShell_profile.ps1`. `<config>` is `$XDG_CONFIG_HOME` when it
  is set to an absolute path, else `~/.config`, as fish and pwsh have it
  (`ptyro_core::config::config_home`). `init`'s completions line, profile detection (§10.10),
  `uninstall` and the systemd unit use the same rule [fixed 2026-11-01: they used `~/.config` regardless].
- `generic`: any user-defined agent command. Status from OSC 7501 (if the program speaks it),
  OSC 9 notifications, BEL, and the activity heuristic (§9.3).

**[built M4]** `shell` sessions get working/idle from OSC 133 (C → working; D → idle with the exit code). OSC 133 A also ends any 7501 working/blocked records. For
`generic`, OSC 7501 from any program is trusted like any other emitter. 7501 `done` maps to
`Done` (task complete), not `Idle`, because `done_means_idle` is false for generic tools.

**[built UX1.4] An agent started by hand in a shell session.** Once a second, a live `shell`
session's foreground process is matched against the agents (a configured agent's command, else
a built-in's program: `claude`, `codex`, `gemini`, `opencode`, `agy`). On Windows a name matches
without case or a runnable extension (`C:\Tools\MyAgent.EXE` is `myagent`), and an npm package's
folder names its command (`claude-code`, as WIN7 names `node …\claude-code\cli.js`; WIN8). While
one is in the foreground:
- `SessionInfo.inner_agent` names its adapter, and the TUI reads `zsh › claude ◌`; the session
  isn't renamed.
- What the shell said no longer holds (OSC 133 `C` was one long "working"): the engine forgets
  its sources and its status (`Engine::forget_sources`), and the shell's `C` marks set no status.
- The status comes from that adapter's hook-free sources, at **low** confidence: its titles,
  and `Adapter::screen_status`. Claude: its dialogs (trust, login), a permission dialog ("Do
  you want to proceed?" / "Do you want to make this…", named by its first line, `Bash
  command`) → blocked, "esc to interrupt" → working, else idle. Codex the same with its own
  words [unverified]; the others, titles only. A high-confidence source (an agent that speaks
  OSC 7501) still wins.
- Once it exits, the engine forgets again, and the shell's own marks take over.

**Adopt** (`Request::Adopt`, `ptyro adopt SESSION`, the TUI's `session.adopt`, `A` in the
sidebar), Claude only so far: refused while it's working; else the daemon finds the
conversation it's in (the newest transcript in `<config dir>/projects/<cwd, every
non-alphanumeric a dash>/`, under the shell's profile's `CLAUDE_CONFIG_DIR`, §20.1), types
`Ctrl-U /exit ⏎`, waits up to 10 s for the shell to come back, and spawns a managed agent session
in the same directory and profile with `SpawnSpec.resume`, so it gets hooks, hibernation and
mail, in the same conversation.

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
- Every transition is persisted (`session_events`, §14.2) — powers timelines, metrics, and debugging
  of adapters ("why did it say blocked?" → `ptyro status --explain`).
- Heuristic-only sessions show their status with a visible low-confidence marker (e.g. `◌`), never
  pretend certainty. **[decided]**

### 9.3 Activity heuristic (fallback only)

Counts bytes written to the PTY by the child per 250 ms bucket, ignoring cursor-blink-only and
spinner-only redraws (rows whose text didn't change). Derives `active` / `quiet`; never `blocked`.

### 9.3b Turns **[built 2026-10-23]**

A turn is what the agent did between a prompt and its answer. Its end runs the turn-end work:
`turns` + 1, the checkpoint (§15.6), verified-done (M7.9), the lead's `worker_done` notice,
the collision radar, run members, the summary (UX1.3) and the `after_turn` script.
- **Agents with a turn-end signal** (every built-in adapter whose hooks are on, and every
  structured session): the turn ends at the agent's own end, `Signal::TurnEnd` (Claude's and
  Codex's `Stop`, Codex's `notify`, Gemini's `AfterAgent`, OpenCode's `session.idle`, OSC 7501
  `done`, a driver's turn end). It counts once, and only if the session has worked since the
  last one, so a turn that several sources end (Codex sends `Stop`, `notify` and OSC 7501
  `done`) is one turn. Start-up noise is none: the output heuristic, or an agent's own
  "working" while it loads, followed by `SessionStart`'s idle. An end that arrives while the
  status still says working (OSC 7501 `done` before Claude's Stop hook has decided) counts when
  the status moves to idle, so a Stop hook that bounces the turn keeps it open. A turn you
  interrupt has no end and runs on into the next one.
- **Everything else** (a generic program, a shell, an agent with `hooks = false`): every
  working → idle or done is a turn, as before.

Before this rule, every working → idle counted. A Codex 0.164 session counted three turns for
its first prompt, and its lead heard "done" twice before any work was done.

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

### 9.6 Workspace-scoped attention **[decided]**

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
  and on `ptyro answer`. `Working` clears only what is about the agent's own state (blocked,
  errored, done, idle, exited); a notification, gate or run (a reviewer's question, a message for
  you) stays until you look [fixed 2026-11-01: typing a review comment into the agent used to wipe
  the question raised in the same poll].
- A new attention less urgent than one still pending leaves the session's attention as it is, but
  still goes to the inbox and the notifier [fixed 2026-11-01: it used to be dropped].
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
    `osascript` / a Windows toast, natively (WIN10) and inside WSL with interop (§20.10, §20.12), or `desktop_command` with `{title}`/`{body}` (an argv run without a shell; the
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
  keep it unique. `ptyro project rename P NEW` changes it (§2.3; `.ptyro.toml` only names a
  project when it is added). Sessions, workspaces, trust and pins refer to the project by id,
  so they follow on their own; its worktrees' `project/name` and its archives
  (`wt restore NEW/name`) follow it. A change set's member keeps the name it was added with:
  that is the member's directory in the set. A change-set template that names its members by
  slug doesn't follow either: after a rename, edit the template (or name the project by id).
  On another machine's project (MM1.B), the clients offer and check the name it has there
  (`app`, not `laptop2:app`), and the daemon takes the `laptop2:` off a rename's new name where it
  routes the request. Sessions and workspaces are renamed the same way.
  A worktree is renamed only while it is ready or missing; one being created is refused.
- `remote_url` is `origin`'s URL, else the first remote's.
- Every project gets an `@root` worktree row for its main checkout. It is never locked or
  removable.
- **[built UX1.5] The repository you're in is a project.** A session whose directory is in a git
  repository that isn't one registers it (`crates/ptyro-daemon/src/autoreg.rs`):
  - at spawn, in the session's workspace (`SpawnSpec.workspace`, else the one the TUI showed
    last, its `_tui` view, else `main`);
  - when a running session's directory moves into one (OSC 7, else the foreground process's
    cwd), from a sweep every 2 s that looks at each (session, directory) once. A session outside
    every project moves in; one already in a project stays there (its worktree lock and profile
    are its own), and only the new repository is registered;
  - once at start, for the sessions an earlier daemon or a live upgrade left running.

  Not registered: your home directory (a dotfiles repository would claim everything), anything
  under ptyro's data directory, and **declined** roots. `project rm` declines its root (kv
  `autoreg:declined`), and adding it by hand lifts that. `ProjectRemove{release: true}` (the
  TUI's undo, `project rm --release`, every client's "Let them go") lets the project's running sessions go first, instead of refusing; they keep
  running outside any project. Clients hear `ProjectAutoAdded{info, workspace, notes}`.
  `[discover] auto_register = false` turns it off. **[proposed SH, §23.2]** A loose session (one in
  a workspace's SHELLS) is never moved: its repository may be registered, the shell stays put. `project add` without `--workspace` uses the
  workspace the TUI showed last.
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
existing target path given with `WorktreeSpec.path` is refused.

**[built 2026-11-09]** The directory is named once, at creation; a rename never moves it (§2.3). A
renamed worktree stays in its old directory, and a renamed project's worktrees under its old
slug. New worktrees go under the new slug. When the directory a new worktree's name would get
is taken (a renamed worktree's, or one left behind), it gets the next free one: `name-2`,
`name-3`.

### 10.3 WorktreeSpec and creation

```rust
struct WorktreeSpec {
    project: ProjectAddr,
    name: Option<String>,         // default: friendly random name
    branch: Option<String>,       // default: template, `{name}` (was "ptyro/{name}" before v0.0.13)
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

Step 2's add, and every `worktree remove` and `prune`, take turns per repository (keyed by its
common dir; 2026-10-31). git writes a new worktree's `worktrees/<name>/commondir` in two steps
while a concurrent add reads every registered worktree, so a fan-out's parallel adds failed now
and then with "failed to read .git/worktrees/<name>/commondir" (seen in CI on Linux).

**On Windows (WIN8):** before step 2, ptyro sets `core.longpaths = true` in the repository's own
config unless it has a value already: Git for Windows checks out a file whose path passes 260
characters only with it, and a worktree's folder adds to every path. Long paths for other programs
are Windows' own (`LongPathsEnabled`, off by default; it needs an administrator), so `doctor` (full,
with a native daemon) warns when a tracked file in a worktree on a Windows drive reaches 230
characters while it's off, with the command that turns it on and the other fix, a shorter
`[worktrees] location`. It also says whether the worktrees' drive is a Dev Drive: ReFS, which a
volume's filesystem says without an administrator (`GetVolumeInformationW`; `fsutil devdrv query`
needs one), else how to make one and what a Defender exclusion for the folder trades (Defender's
exclusions can't be read without an administrator, so it only suggests). A WSL daemon's worktrees
live on ext4 and get neither.

Branch-name templates: `{name}`, `{user}`, `{date}`, `{issue}`, `{slug}`; optional **auto-rename**
(optional): after the first agent turn, offer a branch name derived from the first prompt
(`ptyro wt rename-branch`; never automatic once pushed).

**[built UX1.10] Where work comes from.**
- **`take_changes`** (`new --take-changes`, `wt new --take-changes`, the wizard's "+ with my
  uncommitted changes" when the main checkout is dirty): the main checkout's work is snapshot
  first (`Git::checkpoint`; refused when there's none), the new branch starts at the main
  checkout's HEAD, the snapshot is restored into the worktree (uncommitted, untracked
  untracked), a copy goes to `refs/ptyro/taken/<name>`, and only then is the main checkout
  reset and cleaned. If restoring fails the main checkout keeps its changes.
- **`Branches{project}`**: local branches without a worktree, newest first, then the main
  remote's without a local one: the wizard's "+ on an existing branch…".
- The task box and the wizard's "+ from a PR or an issue…" take a GitHub PR or issue URL, or
  `pr 12` / `issue 34` (`app::work_source`): `--pr` / `--issue`, a PR worktree named `pr-12`.

**[built M3]**
- **Profile:** the worktree's profile is resolved like a session's, against the project's
  `@root`, and stored on the worktree; `default` is stored as such. Git runs with that
  profile's environment, so a fetch uses its SSH key.
- **Fetch:** failures are non-fatal ("fetch failed, using the local origin/main"), for offline
  work. `worktrees.fetch = false` or `--no-fetch` skips it.
- **New branches** are created with `--no-track`, so `x` doesn't claim `origin/main` as its
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

**[built UX1.11] Setup suggestions** (`crates/ptyro-daemon/src/suggest.rs`,
`ProjectSuggest{addr, apply}`, `ptyro project suggest`): before a project's first worktree, a
lockfile implies a setup script (`pnpm-lock.yaml` → `pnpm install --frozen-lockfile`, `bun.lock`,
`yarn.lock`, `package-lock.json` → `npm ci`, `uv.lock` → `uv sync`, `poetry.lock`, `Pipfile.lock`,
`Gemfile.lock`, `composer.lock`, `mix.lock`, `go.sum`; the first found), and ignored `.env` /
`.env.*` files at the root (not `*.example`, `*.sample`, `*.template`) are offered as `[worktree]
copy`, unless the project has a setup script, a `.worktreeinclude` or `copy` already. A yes
writes them into `.ptyro.local.toml` (`toml_edit`: what's there and its comments stay; nothing is
overwritten) and excludes that file in `.git/info/exclude`; when they're then the project's only
scripts, they are trusted. Yes or no, kv `suggested:<project>` keeps it from asking again. The
TUI asks in a dialog before creating the first worktree (then the usual trust check); the CLI
asks on a terminal; `project.suggest` shows them any time.

### 10.5 Lifecycle scripts and trust **[decided]**

Scripts are defined in the project config (§13.3): `setup`, `run.<name>` (named run scripts with
`default`), `archive` (before removal), `after_turn` (optional, after each agent turn — lint/format).

- Run via `$SHELL -c` in the worktree, in their own PTY session (kind `script`), own process group,
  with the env of §10.6. **[built WIN6]** In the shell they're written for, `[scripts] shell =
  "auto" | "pwsh" | "bash" | "cmd" | "sh"` (`auto` by default; a project setting in the settings
  panel). `auto`: a line whose first word is a `.ps1` file runs in PowerShell
  (`-ExecutionPolicy Bypass`: Windows PowerShell's default refuses every script, and ptyro's
  trust is the gate); a `.sh` file in bash; a script imported from another tool's config
  (M5.10, written for bash) in bash; anything else in the default shell, `$SHELL -c` on Unix as
  before, PowerShell on Windows (§3.4). On Windows, bash and sh are Git for Windows' (under
  Program Files, `%LOCALAPPDATA%\Programs\Git`, or beside the `git` on `PATH`), never
  `System32\bash.exe` (WSL's launcher). One that's missing is said with its fix (`winget install
  --id Git.Git -e`), by the script's start and by `doctor` (`scripts <project> shell`). cmd's
  command text goes raw (after `/c`): it doesn't read Rust's `\"` quoting. A shell other than
  `auto` is part of the trust fingerprint (`auto` isn't, so scripts trusted before still are).
  Verify checks (§15.6.5) resolve the same way; a change set's checks keep `sh -c` on Unix.
- **[built WIN6] Line endings:** a bash script (a `.sh` file, or a `#!` line naming a POSIX
  shell) named by a project's scripts with Windows (CRLF) line endings is a `doctor` finding
  (`scripts <project>`, every platform: bash reads the `\r` as part of each command). `ptyro
  project line-endings [P]` (TUI `project.line_endings`, the finding's ⏎; web: the project's ⋯,
  and Health beside the finding; `Request::ProjectLineEndings`, protocol 1.28) rewrites them with
  LF in the main checkout and every worktree (a temporary file, then a rename; links never
  written through; `.bat`/`.cmd`, which want CRLF, and `.ps1` left alone), and says to commit
  `*.sh text eol=lf` to `.gitattributes`.
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
- **Script sessions** are named `<worktree>.<script>` and run `$SHELL -c` (WIN6: the shell
  above) in the worktree with
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

They are set when a process starts: a rename (§2.3) or `rename-branch` doesn't reach a process
already running, and its next restart gets the new values.

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

1. Refuse if any session is running in it (unless `--stop-sessions`). **[proposed SH, §23.8]** "In it"
   includes a loose session (SHELLS) whose live directory is inside the worktree.
2. Compute state: uncommitted changes, untracked non-ignored files, commits not on any remote,
   stashes. If any: **refuse** with a precise report, unless `--force` *and* interactive confirmation
   (or `--force --yes` in scripts).
3. Run `archive` script (if trusted). 4. `git worktree unlock`, `git worktree remove`.
5. Branch: deleted only if fully merged into base or pushed and identical to its remote; otherwise
   kept and reported ("kept branch brisk-rook: 3 unpushed commits").
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
- **[changed 2026-10-30] gc, measured and kept:**
  - *Merged* means the branch moved from where its reflog says it was made and its tip is in
    the base. The old test asked for commits outside the base that were also in it, so it never
    fired. A squash- or rebase-merged PR, or an integrate (`WorktreeInfo::shipped`), counts too
    ("its PR merged", "integrated into the main branch").
  - A **locked** worktree is kept on purpose: gc leaves it out unless its directory is gone. Every
    gc offers *keep* (CLI `k`, TUI `^l`, web "Keep it"), which locks it.
  - `GcEntry::bytes` (`bytes_partial`: it stopped early, "≥") is what the directory takes on disk:
    allocated blocks, links not followed, build output included, each walk stopping after 3 s,
    side by side, cached for 30 min (`Daemon::gc_sizes`, only for worktrees still stale).
    Entries come biggest first; the CLI prints a size column and the total.
- **The nudge (doctor `worktrees`):** `stale` is a warning when anything is stale: "3 stale
  worktrees, taking 4.0 GiB (2 merged, 1 unused for 14+ days): `ptyro gc` archives or removes
  them; lock one to keep it". So it is the TUI's `⚕` (once per attach, its own toast: "…: `C-q G`
  cleans up"; ⏎ in `M` opens `worktree.gc`, and a worktree removed or locked asks the doctor
  again) and the web's Health page ("Clean up"), where the Projects tab's 🧹 also counts them
  (`🧹3`). `archived` is only a note: how many, the oldest, and whether
  `archive_purge_days` deletes them.

**[built UX1.12] Archive, restore and finish** (`crates/ptyro-daemon/src/finish.rs`):
- **Archive** (`WorktreeArchive`, `ptyro wt archive`): a snapshot commit of the files as they are
  (`Git::checkpoint`: a private index, untracked included, ignored not, parent HEAD) goes to
  `refs/ptyro/archive/<name>` (`-2`, `-3` when taken), then the worktree is removed with the
  branch kept. Running sessions refuse unless `stop_sessions`, and they are stopped *before* the
  snapshot, so what they write on the way out is in it (a quick terminal at its prompt doesn't
  count, UX1.25). The snapshot leaves out ignored files, so small ones (a `.env`) are kept
  beside it in `data/archive-extras/` and put back on restore, and an ignored file too big to
  keep makes the archive refuse and name it, so nothing is lost silently. The archive list is
  kv `archive:list` (`ArchiveEntry`, newest first; read and written under a lock, errors
  propagated, the entry written after the removal; `WorktreeArchives`, `ptyro wt archives`).
- **Restore** (`WorktreeRestore`, `ptyro wt restore [project/]name|handle`): the branch checked
  out at its old path (when free), no setup, then `Git::restore_onto_tip` (it refuses when the
  branch has moved since) puts the snapshot back with the changes uncommitted again, untracked
  files untracked. The entry leaves the list. Several archives of one name are listed (handle
  and age), never picked between; the handle says which (§2.3, owner, 2026-11-09).
- **Finish** (`WorktreeFinish`, `ptyro wt finish [--integrate]`): when there are changes, the
  worktree's live agent (not mid-turn) is asked to commit and its turn awaited (up to 10 min,
  or 1 min if it never starts); what's left is committed as the worktree's profile with its
  title as the message (`Work on <name>` without one). Then a PR (`github.draft` decides draft)
  or integrating. Progress goes out as `WorktreeProgress`.
- **Archive on merge** (`[worktrees] archive_on_merge`, default off): a PR refresh that sees a PR
  it knew open now merged archives the worktree, when nothing runs in it. A first sighting
  doesn't count, so a worktree restored after its PR merged stays. Integrating it into the main
  branch counts too (`WorktreeInfo::shipped`). A worktree is archived by one archive at a time:
  a second one while it runs is refused (before, a PR poll and a refresh seeing the same merge
  could both snapshot it and both list it).
- `ptyro gc` asks per stale worktree: `a` archive (keeps the work), `r` remove, ⏎ skip.
- **Delete an archive** (`WorktreeArchiveDelete`, `ptyro wt archive-rm [project/]name|handle…`, asks
  unless `--yes`; 2026-10-30): the entry leaves the list first (an entry whose snapshot is gone
  couldn't be restored), then the snapshot ref and its `archive-extras/` copies go, and its branch
  by `wt rm`'s rule (deleted when merged into the base, kept in the archive as `ArchiveEntry::base`,
  or identical to its upstream; kept and said why otherwise, and kept while another archive or a
  worktree has it). **`[worktrees] archive_purge_days`** (default 0, never) does the same to
  archives older than that, once a day and a minute after the daemon starts.

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
  uncommitted, never the base's progress. A diff leaves untracked files out, so their lines are
  added to the insertions (`ls-files --others --exclude-standard`, only when status saw any):
  a new file is new work. Kept cheap for the refresh: files over 1 MiB, binaries (a NUL in the
  first 8000 bytes) and anything but a regular file count nothing, and at most 8 MiB is read per
  refresh.
- **What is left to do (UX2.2).** `RepoStatus` (crates/ptyro-core/src/model.rs) also carries
  `unpushed` (optional: commits on no remote and not in the base, counted only for a branch with
  no upstream, and only when the repository has a remote) and `pushed` (it has commits beyond the
  base, nothing uncommitted, nothing to push, and somewhere to push to). Both are optional on the
  wire, so an older daemon's status still parses. `RepoStatus::left()` is the one rule every
  surface uses: `uncommitted` = staged + modified + untracked + conflicts; `unpushed` = `ahead`
  when there is an upstream, else the counted commits; `not_pushed` = no upstream and commits;
  `pushed`. Shown as `↑N` / `not pushed ↑N` (unpushed), `● N` (uncommitted) and a quiet
  `✓ pushed`; the `+N −M` line counts only while uncommitted or unpushed work exists. A shipped
  worktree (PR merged, or integrated) shows nothing of it, and an open PR's badge stands in for
  `✓ pushed`. The TUI sidebar row and pane header (`diffstat` in ui.rs), `ptyro wt ls|info`
  (`status_line`) and the web's worktree rows and page (`summary`, `repoLeft` in web/src/data.ts)
  follow it; the web test keeps the TS mirror equal to the Rust one. Refreshing: the push moves
  `refs/remotes/<remote>/<branch>` in the main repository's git directory, outside a linked
  worktree, so each linked worktree's watcher also watches `.git/refs/remotes` there (the status
  is recomputed within the 500 ms debounce); the ship, finish and push paths refresh the status
  themselves, and the 10 s / 60 s poll covers the rest.
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
   `scrub_extra`. An isolated profile also loses every variable with a credential-shaped name:
   `AWS_*`, `AZURE_*`, `GOOGLE_*`, `GH_*`, `GITHUB_*`, `OPENAI_*`, `ANTHROPIC_*`, `XAI_*`,
   `MISTRAL_*`, `OPENROUTER_*`, `GIT_CONFIG_*`, and any ending `_API_KEY`, `_TOKEN`, `_SECRET`,
   `_PASSWORD` or `_ASKPASS` (the profile's own `env`, `secrets` and account tokens, applied
   next, put back what it wants). An exported `ANTHROPIC_API_KEY` otherwise silently overrides a subscription login.
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
    `<config>/fish/config.fish` (`$XDG_CONFIG_HOME` or `~/.config`, §8.7), and files they `source` (one level, no evaluation — text only);
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

### 11.1 Layout **[decided]**

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
│ C-q ? help · n new · a next attention · p peek · d detach        TERMINAL   12:41      │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

- **Sidebar** (collapsible, resizable): workspace → project → worktree → session tree with status
  glyphs, diffstat, ports, attention badges; inbox below.
- **Main area**: one or more panes (each showing one session), with a header line: status, agent,
  branch, diffstat, ports, controller/size badge.
- **Status bar**: global counts, mode indicator, key hints, clock. **[built SH.14]** The load
  average of the daemon's computer (`0.52 0.48 0.40`, from `/proc/loadavg` or `getloadavg`; none
  on Windows), first on the right so it is the first to go on a narrow terminal; amber once the
  1-minute load reaches the CPU count, red at twice it. `[ui] load = false` stops the daemon
  sampling it, for every client. The web shows it beside the machine's name in its header.
- Status glyphs: `●` working (animated pulse), `◉` blocked (colour: warning), `○` idle, `✓` done,
  `✗` errored, `■` exited, `☾` dormant, `◌` low-confidence. Colours are theme tokens; respects
  `NO_COLOR`.

### 11.2 Modes and focus

- **Terminal mode** (default when a pane is focused): every key goes to the session, except the
  **prefix** keys (default `Ctrl-Q` and `Ctrl-Space`, configurable as one key or a list; §21.2).
  Any of them starts a command, and hints show the first. A prefix pressed twice focuses the
  sidebar (`focus.toggle`, like `prefix Tab`; every configured prefix gets this, and `[keys]` can
  rebind it). `prefix q` (`prefix.send`) sends the prefix key that started the command to the
  program, so a ptyro inside ptyro still gets its prefix (owner, 2026-10-30: the quickest chord
  goes to the sidebar, the screen-style `C-a a` way, rather than tmux's `C-b C-b` send-prefix).
- **Navigation mode** (focus in sidebar or after prefix): single-key commands.
- **Zoom**: pane fills the screen (sidebar hidden). **Raw attach** (`ptyro attach --raw`): no chrome
  at all; ideal over flaky SSH or on a phone terminal app; detach with `prefix d`.

### 11.2b What M2 built **[built M2]**

- **Layout:** sidebar (sessions with 1–9 numbers, status glyphs, the foreground process, blocked
  kind), up to **four panes** in a split tree with 1-cell dividers, a one-row header per pane, and
  the status bar. The status bar shows the mode badge (PREFIX / COPY / SIDEBAR / VIEW ONLY /
  RECONNECTING), toasts, counts (live · working · blocked), hints and a clock.
- **Splits:** `v` side by side, `s` stacked (vim's sense). **[changed UX3.1]** A split starts a
  clone of the session it splits (§11.2i); before, a picker offered existing sessions or "+ new
  shell". Panes are focused with `h`/`l` (cyclic) or Shift-arrows
  (geometric), resized with `< > + -` or by dragging a divider, zoomed with `z`, closed with `c`.
  Every visible session is subscribed and **sized to its own pane**; hidden ones are unsubscribed.
- **The view persists in the daemon** (`SaveView`/`LoadView "main"`, stored in `kv`, debounced
  500 ms and on detach): the layout, what each pane shows, focus, zoom and the sidebar. A plain
  `ptyro` brings it back, even after a daemon restart. Panes whose session is gone come back
  empty; empty panes are filled from unshown sessions. While attached, removing a session closes
  the panes showing it (its sibling takes the space); the last pane is refilled from an unshown
  session instead. An exited session keeps its pane.
- **Overlays:** help (generated from the live key map, filterable, also showing the terminal's
  kitty-keyboard and colour support), the command palette (actions with their keys, sessions,
  themes), the session finder, a text prompt (new session: run through `$SHELL -c` so pipes and
  quotes work; rename), and a confirmation for stop. A prompt is **checked as it's typed**,
  mirroring the daemon's rules from what the TUI knows: name prompts (rename, new worktree, new
  workspace) say when the name is taken; the fan-out agent list flags unknown agents. While
  there's a problem ⏎ is refused and the text stays. Renaming a session to its own name does
  nothing.
- **Name fields swap, they don't refuse** (`names::typed`, `names::branch_typed`): as a name is
  typed or pasted, uppercase is lowercased and any other character outside `[a-z0-9._-]` (a
  space, `/`, an accent) becomes `-`, as does the second `.` of `..`; a leading `.`, `-` or
  space is dropped (`My Feat/2` → `my-feat-2`; owner, 2026-11-09: `-`, not `_`, the same
  character as the suggestion and generated names, §2.3). The dialog says so. What no one
  character fixes (all digits, ending in `.` or `.lock`, over 64) is still flagged with its
  reason, and Tab applies the suggested fix. A branch name keeps case and `/`, swaps
  whitespace and `~ ^ : ? * [ \` for `-`, and drops them or a `-` at the start. It applies to
  every interactive name field in the TUI and the web (`slug` on a form field): session rename,
  the task box's name, new session, shell and worktree, workspace new and rename, profile new
  and rename, a change set's and a fan-out's name, a paired device's, config-panel members,
  machine names, and the branch rename. The CLI
  and the daemon stay strict: a bad name given as an argument is an error, so a script never
  gets a name it didn't write.
- **Copy mode** (`prefix [` or `PageUp`, the mouse wheel, or a drag): §11.3 below.
- **Exited sessions** stay on screen, greyed, with a banner "<name> exited N — ⏎ restart · C-q x
  remove" ("⏎ resume" on a dormant one).
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
  worktrees by creation, each with its sessions. Below them, under "other sessions" (**[to be
  replaced by SHELLS, §23.3]**), are the
  sessions whose home is this workspace but that aren't under one of its projects.
  - Rows show the pin's profile label (in its colour), the branch when it isn't the default
    `<name>` (or `ptyro/<name>`, the default before v0.0.13), what is left to do (`↑N` unpushed, `not pushed ↑N`, `● N` uncommitted, a
    quiet `✓ pushed`; the `+N −M` lines only while something is left, §10.9), the worktree state
    when not ready, and listening ports.
  - Folding: `⏎`/`Space`, `h`/`l`, or a click. `1`–`9` number the visible session rows. In the sidebar
    `j`/`k` move the selection; after the prefix they cycle every session of the workspace in
    tree order, skipping quick terminals (§11.2h).
- **Workspaces:** **[changed MM1.B]** every workspace is in the sidebar at once and there is one
  layout (§11.2j); `w` jumps to one. Before: `w` opened the switcher (each workspace's profile,
  project and session counts, "+ new workspace"). Views are saved per workspace (`view:<name>`), and the last workspace is
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
- **`p` (peek)** pops up the screen and pending question of the session that needs you most
  (else the focused one; in the sidebar, the selected one) without switching: with nothing typed,
  `y`/`a`/`n` answer a permission; typing writes a reply that `⏎` sends; `⏎` with
  nothing typed opens the session.
- **`i` (inbox):** one tab per workspace plus All (`Tab` switches), newest first, unread marked.
  `j`/`k` move; `⏎` opens the session (or the run, for a gate or run entry) and marks the entry
  read; `p` peeks, `r` marks one read, `R` all.
- **`e`** (in the sidebar) explains the selected session's status (the engine's ring); elsewhere
  it is the palette's `session.explain`.
- **Palette:** `session.resume`, `session.hibernate`, `session.mute`, `notify.test`.
- **Dormant panes** show a resume card; `⏎` (or focus, with `resume_on_focus`) resumes.
- The TUI reports its terminal's focus (mode 1004) with `Focus`, so watching only counts while
  you're actually looking.

### 11.2e What M5 added **[built M5]**

- **Broadcast:** `Space` in the sidebar marks a session (`◆` replaces its number), and on a
  project or worktree row it still folds. `b` (in the sidebar, or after the prefix) opens one
  prompt for every marked session. Each gets it once it's ready (`wait_ready_ms`, never typed
  into a dialog), and the marks clear.
- **Tour:** `ptyro tour` (palette `help.tour`, and offered at the end of `ptyro init`). *(Was
  five pages of keys; UX1.22 replaced them with the hands-on tour in §11.2h.)*
- `ptyro attach --workspace W` opens a workspace's own view.

### 11.2f What M6 added **[built M6]**

- **The inspector** (`D` after the prefix, or in the sidebar) covers the panes for one worktree
  (the selected row's, else the focused session's). Five tabs (`1`–`5`, `Tab`):
  - **Diff:**
    - a file list (marks: `◆` staged for commit, `💬N` comments) next to the hunks;
    - unified with old/new line numbers, or side by side (`s`);
    - against the base, the upstream, the last turn or HEAD (`a` cycles);
    - keys: `j/k` lines, `d`/`u` 20 lines, `n/p` (or `]`/`[`) files, `c`/`⏎` comment on the
      cursor line, `x` drop it, `S` send all comments to the agent, `Space` mark a file, `C`
      commit the marked files (or everything) with a message, `E` commit in your editor, `A` ask
      the agent to commit, `P` push and open a PR (a draft while `github.draft`), `I` integrate
      (asks first), `r` refresh, `q` close.
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

### 11.2h What UX1 added **[built UX1]**

UX1 (ROADMAP) makes the TUI first class (§1.3): everything works, and is discoverable, without
learning anything up front.

- **A pane moves on when its session goes** (UX1.23). A removed session's panes, and the panes of
  one you stopped from this TUI (`[ui] on_stop = "next"`, the default; `"stay"` keeps its last
  screen), show its neighbour:
  - live sessions first; a dormant agent never, since showing it would wake it
    (`attention.resume_on_focus`);
  - then the nearest place: the same worktree, then the same project, then anywhere;
  - then the nearest row in the sidebar, the one after before the one before.

  Sessions shown in other panes are skipped. The focused pane picks first, and the sidebar
  selection follows it. A stopped session stays listed, and `` ` `` brings it back. With no
  neighbour, a removed session's split closes and the last pane shows the empty state: what to
  do next (`n`, `N` when the workspace has a project, `/` when other workspaces have sessions,
  `?`), with the keys as bound. In raw attach, only the attached session's removal ends it.
- **Programs are named** (UX1.1): the sidebar, headers and `ptyro ls` show `claude`, not the
  version its installer names the file after (§5.1, §20.5). An agent session's row names its
  agent as configured (`claude`, `claude-work`) whatever its process is called (a wrapper
  script, a launcher).
- **An agent started by hand in a shell** (UX1.4, §8.7) reads `zsh › claude` with the `◌`
  of a guess, and a detail line offers adopting it (`A` in the sidebar).
- **Rows say what's going on** (UX1.2). Under a session's first line (now with how long it has
  been working or blocked) come up to a few lines, each only when it has something to say:
  - blocked: `wants` what (the permission's tool, the question), then the keys that answer it;
  - working: `now` the current step (the hook's tool summary);
  - errored: why;
  - an agent: `task`, its first prompt, and once its turn ends, `last`, its reply;
  - a shell: `now` its running command and for how long, or `last` the one that finished, with
    its exit (`✓`, `✗101`), duration and age. `ptyro shell-init` now sends the command line
    with OSC 133 `C` (`cmdline=`; kitty's `cmdline_url=` is read too): zsh, fish and PowerShell
    always, bash from 4.4 (`PS0`) and, since WIN1, older bash through a DEBUG trap;
  - exited: its exit and age, `⏎ restart`; dormant: `⏎ resumes it`.

  `[ui] sidebar_detail`: `auto` (the default) opens every row while they all fit, else only
  the selected, focused, blocked and attention rows; `compact` keeps one line per row; `full`
  opens every row. The row in focus (and any that wants you) wraps its task and reply over two
  lines. Drawing, clicks and scrolling share one line layout, so a row is one target however
  many lines it takes.
- **The tour, done for real** (UX1.22; `ptyro tour`, palette `help.tour`, the first run's last
  step and `ptyro init`'s). It runs a ptyro of its own (`crates/ptyro/src/tour.rs`):
  - its `PTYRO_HOME` under the system temp dir (`ptyro-tour-<pid>-<ms>`), with none of the outer
    session's `PTYRO_SOCKET`/`PTYRO_SESSION_ID`;
  - a config with your `[ui]` and `[keys]` (so its keys are yours) and one agent, `claude`,
    which is the built-in fake in tour mode (`PTYRO_FAKE_TOUR`, `PTYRO_FAKE_PASTE`);
  - a demo repository whose `greeting.txt` has a typo.

  The TUI runs as a child (`ptyro tour --here`) in the demo repository. When it ends the
  tour's daemon is stopped and its directory deleted; a tour that couldn't clean up (killed, its
  terminal closed) is swept by the next one. From a TUI, `help.tour` exits into the tour
  (`AttachEnd::Tour`) and the TUI comes back after it.

  The coach (`crates/ptyro-tui/src/tour.rs`) sits bottom right and never takes keys. Each step
  names the keys as bound (or as they work where the focus is) and moves on when it's done:
  1. start a task (`n`): an agent with a task, in a worktree of its own;
  2. answer it (`p`, `y`): it blocked on a permission and got going again (or its turn ended);
  3. review (`D`): the inspector is open on its worktree;
  4. finish (sidebar `F`, integrate): its agent commits, ptyro integrates into main;
  5. clean up (`F` or `x`): the worktree is archived.

  The pretend agent asks before running `sed` on `greeting.txt`, fixes the typo once allowed
  (leaves it, told no), and commits when finish asks. `help.tour` inside the tour starts it over.
- **Shipped worktrees** (UX1.12, UX1.22): `WorktreeInfo::shipped()` is true when the PR merged,
  or when ptyro integrated it into the main branch (`WorktreeInfo.integrated`, the commit, kept
  in kv across restarts) and the worktree is still clean at that commit. A shipped worktree's
  row says `in main` (or `merged`), its hint is "clean up", `F` and `x` archive it, and
  `archive_on_merge` archives it on its own. Before, only a merged PR counted, and `F` on a
  merged worktree opened the finish picker again despite the hint.
- **Short choices pick by key** (`Palette::with_hotkeys`): in the finish picker, `p` (a PR) and
  `i` (integrate) pick their item while nothing is typed. In the palette, keys are only shown.
- **Several lines in ptyro's own boxes** (UX1.26): in the task box, peek's reply and the
  prompts for free text (a broadcast, a review comment, a commit message), `Shift+Enter`,
  `Alt+Enter` and `Ctrl+J` start a new line (`app::is_newline_key`; without the kitty keyboard
  protocol only the last two can be told from Enter, and the hint says `C-j`). The box wraps and
  grows to a third of the screen, keeping its end in view (`ui::edit_rows`). A paste goes into
  the open box, new lines kept where it takes several, and never on to the session behind it,
  which it used to. Names, titles and command lines stay single-line. The daemon types a
  multi-line prompt as one bracketed paste, so the agent gets one prompt.
- **Summaries, opt-in** (UX1.3, `crates/ptyro-daemon/src/summary.rs`). With `[summaries] enabled
  = true`, a one-line summary is made each time an agent's turn ends (an agent session, or one
  started by hand in a shell) and each time a session newly blocks. It shows as `≈ …` on the
  row. It replaces `last` while it describes the current turn, and is hidden while the session
  works. `session.summarize` (the row menu, the palette) and `ptyro summarize SESSION` make one
  now, enabled or not.
  - **How:** `[summaries] command` (default `claude -p --model haiku --output-format json
    --strict-mcp-config --tools ""`) reads the prompt on stdin and prints the summary, or
    Claude's JSON, whose `result` is used. It runs 1.5 s after the turn ends, so the reply lands
    first. It runs under the session's profile environment, so on that profile's account, in
    `<data>/summaries` (no project files, no tools), with a 60 s limit. Only one runs at a time
    per session; a turn that ended meanwhile gets its own afterwards.
  - **What it's sent:** the session's kind, title, task, status and what it waits on, its last
    reply (≤ 1500 characters), a shell's command, and the last 25 lines of its screen. The
    profile's secret values become `<secret:NAME>`. Anything shaped like a credential (`KEY=…`
    for token/secret/password keys, `Bearer …`, known token prefixes, long mixed runs of letters
    and digits) becomes `<redacted>`.
  - **Limits:** `daily_cap` (200) per profile per UTC day, counted in memory. On a failure (not
    logged in, over the cap) the rule-based lines stay; the error shows only when you asked for
    the summary directly.
  - **Spike result (2026-10-23):** option A, a stateless call per turn, is what's built. One
    summary costs $0.0006 and takes 1–2.5 s (§20.1). Option B, a resident per-profile
    "concierge" session, is not built: at this price and speed it would only pay off for
    answering questions across sessions, which stays open (§21, question 9).
- **`n` asks what to do** (UX1.9). One box, "What should it do?", with a preview under it:
  `→ fix-flaky-test · claude · new worktree in myrepo`. ⏎ starts the default agent on it in a
  new worktree named after it (here, with no project), the prompt also its title. Tab edits the
  name, which starts from the suggestion and stops following the prompt; a bad name says why and
  ⏎ waits. `C-o` opens the full wizard (agent, then where) with the prompt carried along. An
  empty ⏎ is the old `n ⏎ ⏎`: the default agent in a new worktree with a random name.
- **Names and titles** (UX1.24, §2.1). Create dialogs offer a name, dimmed, that ⏎ takes and
  typing replaces: `N`'s worktree name (a friendly pair) and a new workspace's (a profile without
  one, else `workspace-2`). Rows and headers show the title: the session's `task` line and its
  header, a worktree's ` — title` when its name doesn't say it all. An agent named after its
  worktree shows as its program under it. `,` renames (the address); `C-t` there edits the title
  instead (`session.retitle`); `worktree.retitle` titles a worktree.
- **Messages offer the next step** (`toast.act`, `u` after the prefix or in the sidebar): a
  message may carry one follow-up, shown with its key (`registered myrepo in main · C-q u
  undo`), kept up 10 s. **Errors offer the fix** (UX1.17): no message the TUI writes names a
  command to type (a test reads the sources to keep it so). Instead: an unbound key offers the
  key list; untrusted scripts, trusting them; a project without scripts, its `.ptyro.toml` in
  `$EDITOR` (`Then::Edit`); a hidden sidebar, showing it; "no session here", starting one; a
  chat view on a terminal agent, switching it to structured mode; nothing marked for broadcast,
  the sidebar; the runs view's gates and the away digest, the runs. A daemon error that
  suggests a CLI command offers its TUI action when there is one (`app::SUGGESTED`).
- **A repository registered on its own** (UX1.5, §10.1) says so with an undo; a profile pin its
  rules suggest still asks "keep the pin? [Y/n]". The sweep (every 2 s) places any session
  outside every project whose directory is in one now, a project added by hand since included,
  and tries a (session, directory) again when registering failed rather than said no (git
  failing inside a repository, the store busy); a directory that isn't a repository, your home
  or a declined one is settled.
- **No project? Pick one, then carry on** (UX1.6). `N`, `o`, `r`, `R` and `project.trust` with no
  project open the discovery picker ("No project here yet: pick one to add to main") and, once
  the project is added and its main checkout has arrived, run as asked there. The wizard's
  "where" step offers "+ add a project…" after "here" when there is none, then goes on to the
  new project's targets. `project.add` has a key: `P` (after the prefix, or in the sidebar), and
  the empty sidebar names it.
- **Which-key** (UX1.14): when no key follows the prefix within `[ui] which_key_delay` (400 ms;
  0: never), a menu of the next keys appears above the status bar, in five groups (start, go,
  panes, work, this: including settings, broadcast, home, last session, shell here, the
  scratchpad, `G` clean up and `f` the sidebar filter), as bound now; keys typed before it work as always. The event loop wakes at
  the deadline, not on its 250 ms tick.
- **Everything the CLI does** (UX1.21): `docs/parity.md`'s TUI column has no gaps, and
  `make tui-parity` (part of `make test`) fails on any. The frequent and visual ones are native:
  `session.convert`, `project.remove|untrust|pin|unpin|leave|mute`, `worktree.lock|unlock|pair|
  unpair|rename_branch`, `workspace.rename|remove|profile|notify|mute`, `pr.open` (the PR's
  worktree and session, else a new worktree on it). The long tail is
  `crates/ptyro-tui/src/cli_actions.rs`: a table of `ptyro …` templates whose words come from the
  selection (`{session}`, `{worktree}`, `{project}`, `{set}`), a pick (`{profile}`, `{agent}`,
  `{choose:…}`) or a question (`{ask:…}`, optional when left empty; `{vars}` → `--var k=v`), then
  run in a split: profiles (login included), recipes, checkpoints, verify, messages, PR replies,
  leases, the radar, every change-set command, serving, pairing (its QR code in the split),
  devices, replay and export, `config show`.
- **About** (UX2.1, `crates/ptyro/src/about.rs`, `crates/ptyro-tui/src/about.rs`,
  `aboutView` in `web/src/settings.ts`). `ptyro about [--json]` is one document: the version and
  protocol (`ptyro --version` prints the same version; **[UX3.5]** and every agent's known usage
  limits per profile, `usage: [{label, text, limited}]` from the daemon's `Usage` meters, one
  line each by `Usage::summary`), the repository (`Cargo.toml`'s
  `repository`) and its `/issues` as where to report a problem, the licence, the build (commit and
  its date from `build.rs`, the target triple, the binary, the OS), where the config file, data,
  logs and socket are (and whether they exist yet), the daemon (running, its version and
  protocol, pid, uptime; `same: false` plus the fix, `ptyro daemon upgrade`, when its version or
  protocol is not this ptyro's), the web app's server (`webapp::lines`), the tools (the doctor's own
  `git` and agents checks, and `gh`), and `report`: all of it as text for a bug report. The TUI
  (`app.about`, `I` after the prefix and in the sidebar) runs `ptyro about --json` like the health
  badge runs the doctor and draws it as an overlay: `j`/`k` pick a link or path, `⏎` opens it
  (`open` / `xdg-open`, inside WSL `explorer.exe` with the folder's Windows path; copied instead if
  nothing opens it), `y` copies it, `c` copies the report,
  `U` runs `daemon.upgrade` when the daemon differs. The addresses are OSC 8 hyperlinks: after
  each frame `link_cells` rewrites those cells with the hyperlink escape in the symbol, over the
  identical text, so the screen looks the same. Writing them moves the terminal's cursor, so
  `paint_links` brackets the write with DECSC/DECRC; without that the cursor was left on the last
  link, away from the focused program's prompt. The web app's More → About asks the `about` host
  operation (`full` devices), and falls back to the daemon's status plus the repository, issues and
  licence constants in `web/src/data.ts` (checked against `Cargo.toml` by `web/test/kit.mjs`) for
  a lower scope. The help overlay's footer and the web's More page show the version.
- **The health badge** (UX1.20, `crates/ptyro-tui/src/health.rs`): `ptyro doctor --quick --json`
  runs in the background 20 s after attaching and every 10 min; its warnings and problems, plus a
  missing `ptyro shell-init` line in your shell's rc file (the TUI's own check, only with
  `[shell] integration = false` since WIN1), show as `⚕ N` in
  the status bar (red with a problem), announced once when there are more. `health` (`M`)
  lists them, ⏎ fixes one: a tmux option set now and offered for `~/.tmux.conf`; `chmod 600`;
  a line appended to a file (asked first); or the `` `ptyro …` `` command the finding names
  (`<profile>` filled in from its section), run in a split; stale worktrees open the clean-up
  list (`Fix::Action`, §10.8). `run_cli` (a `ptyro` command in a
  split, its output kept) is also the TUI path of `doctor`, `daemon.status|logs|restart|
  upgrade|service|unservice` and `integrations[.install|.uninstall]`.
- **The first run and home** (UX1.19, `crates/ptyro-tui/src/onboard.rs`). When `ptyro` had to
  start the first session itself (`Options::welcome`):
  - with no config file (and `PTYRO_FIRST_RUN` not `0`): the first run, over everything. First
    the setup checklist (BOOT2, §24.5: agents and tools to install, what's here already with the
    default agent marked), then accounts `detect()` finds in your shell setup (ticked),
    repositories under `[discover] roots` (the one you're in ticked; it registers on its own
    anyway), desktop notifications (not ticked). Space ticks, ⏎ writes `config.toml` (0600,
    created only if absent, checked before writing) and registers the ticked repositories in
    the current workspace (and installs what the checklist ticks, §24.5), then offers the
    tour; `s` does the same without installing; Esc writes an empty config so it never comes
    back. `first.run` opens it again; `ptyro init` stays the terminal's version, with the
    terminal checks and shell extras (and the CLI's checklist offered);
  - else, when the workspace has no project: **home** (`home`, `H`), one list of who needs you,
    worktrees with PRs and their checks, projects (most recently used first; ⏎ starts a task
    there), repositories found (⏎ adds one, then the task box), and a shell here.
- **Finish and clean up** (UX1.12, §10.8): `worktree.finish` (`F` in the sidebar, the row menu)
  asks "a pull request" or "integrate", then runs it while messages say how far it got. A merged
  worktree's hint is "clean up", and `x` on it archives instead of removing. A removal refused for
  work it would lose offers `a`: archive instead. `worktree.archive`, `worktree.restore` (a picker
  of archived worktrees; `^x` deletes the selected one, asked first), `worktree.archive_delete`
  (the same picker, ⏎ deletes) and `worktree.gc` (`G`; stale ones; ⏎ archives) are in the
  palette. The web's Archived list offers Restore and Delete on each.
- **A companion shell** (UX1.13, `shell.here`: `!` after the prefix or in the sidebar): your
  shell in the selected worktree, under its profile. **[built UX1.25]** It is now the
  **quick terminal**, below.
- **The quick terminal** (UX1.25; **[changed UX3.4]** `!` is now `shell.new`, a new shell, and
  the quick terminal is a worktree's only, §11.2i; `shell.drawer`: `C-t`, `shell.pane`: `T`,
  `shell.here`: `⏎` on its line; `crates/ptyro-tui/src/quick.rs`, `crates/ptyro-daemon/src/quick.rs`):
  - **One per worktree**, shared by its sessions; outside any worktree (an empty pane, a
    session in a plain directory), one per directory. `QuickShell {worktree?, cwd, create, env,
    size?, workspace?}` finds it (by address, else the worktree `cwd` is in, else `cwd`) and,
    with `create`, starts it: `$SHELL -l` under the worktree's profile, named
    `<worktree>.shell` (`<project>.shell` for the main checkout, `<dir>.shell` outside one),
    `SessionInfo.quick` and `Place.quick` set. Finding is `read` scope for paired devices,
    starting `full`.
  - **Drawer or panel (UX2.3, 2026-10-26).** The same shell is shown one of two ways.
    `shell.here` picks by `[ui] shell_style` (`pane` by default since UX3.1): `auto` puts it in a drawer
    under a pane showing a session of the same project (the focused pane first, else the first
    in layout order; matched by project, or by directory outside any), and when no pane does
    (the focused pane is another project's, empty, or nothing is open) shows it as a **panel of
    its own**: it fills the focused pane as `session.new` does (`PaneState.last` keeps what it
    replaced; a drawer's focus first moves to its host). `drawer` and `pane` make `!` always
    one. `shell.drawer` (`C-t`) forces the drawer, under a pane of the project even when another
    is focused, and falls back to the panel with a toast when none is open; `shell.pane` (`T`)
    forces the panel. Toggling: in its drawer the key hides it; in its panel it switches back
    to the pane's previous session (or empties the pane); shown but not focused, it focuses;
    `shell.pane` on a shown drawer moves the shell to a panel. The web has no split panes, so a
    shell is always its own page there.
  - **A drawer** split off below the focused pane (`Drawer {host, pane}` in the saved view),
    30% of its height at first. It's one pane more than `MAX_PANES` allows for splits
    (`MAX_WITH_DRAWER`). `+`/`-` and dragging resize it, and hiding keeps its share
    (`drawer_share`). A zoomed pane keeps its drawer below it. Closing the host closes the drawer;
    closing the drawer hides it.
  - **One key, three steps** (as VS Code's terminal panel): hidden → shown and focused; shown
    but not focused → focused; focused → hidden, focus back on the host. Hiding only closes the
    pane: the daemon keeps the shell, so it outlives a detach, and the view brings the drawer
    back. When the shell is gone (a daemon restart: shells aren't restored), the restored drawer
    closes instead of showing another session. Its header shows `›_` and the key (`C-q C-t
    hide`, or `focus` while another pane has focus).
  - **In the sidebar** it's a line of its own, first under its worktree (`Row::Quick`): `›_ quick shell`
    and what runs in it (the command, else the foreground program, else the last command with
    `✓`/`✗`). It has no number, `j`/`k` skip it, and it never fills an empty pane. Its row menu
    shows or hides it, adopts, renames, explains and stops it.
  - **Quick means brief:** the drawer (and the scratchpad's popup) go away when another session
    or workspace shows: a pane that now shows a session it didn't (`App::tuck_away`, after every
    command and mouse event, compares what every pane shows); the shell and the editor keep
    running. **[changed 2026-10-30]** Moving focus between panes that were already showing
    (back up to the agent the drawer hangs from, say) leaves them open: they are used together.
    Before, focus landing on another pane closed the drawer. Focus in the sidebar doesn't count
    either, so you can pick the next thing. While its drawer is open the shell's sidebar line is
    tinted (`bar_bg`).
  - **Quiet:** `raise` drops its attention and notifications, except `blocked` (an agent started
    in it that waits for you). `exit` forgets it (no exited row; the name is free again), and the
    TUI closes its drawer.
  - **It doesn't hold up its worktree:** removing, archiving and `archive_on_merge` refuse only
    for sessions that are `busy()`. A quick terminal at its prompt (its shell is the PTY's
    foreground process group) isn't busy, and is stopped with the worktree; one running a
    command is.
  - **CLI:** `ptyro shell [WORKTREE]` attaches with it open in the drawer, `--json` prints it,
    and `--read [--scrollback [N]]` prints its screen without ever starting one. An agent asked
    to "look at the error in my terminal" can run `ptyro shell --read` from its worktree.
- **A row's menu** (UX1.15, `row.menu`: `.` after the prefix or in the sidebar, or a
  right-click): every action that applies to that row now, with its key. A session's (peek,
  resume or restart, adopt, title, rename, explain, hibernate, mute, its worktree's inspector,
  editor and scripts, split, stop or kill), a worktree's (a task there, a shell, the inspector,
  finish, editor, scripts, title, archive, remove), a project's. Picked, it runs with that row
  selected, so it works on that row. In a pane, `.` is the focused session's.
- **Hints follow the selection** (UX1.16): with no message up, the status bar says what you'd
  do next with the selected row (the focused session in a pane), with the key as it works
  there: `C-q p answer it` (blocked), `R restart · x remove` (exited, in the sidebar), `⏎
  resumes it` (dormant), adopting a hand-started Claude, `D review its changes` (a worktree
  with changes), clean up a merged one, `a who needs you`.
- **The palette finds everything** (UX1.18): besides actions, sessions, workspaces and themes,
  the workspace's worktrees (going to one shows its first session) and projects (selected in the
  sidebar), and the context worktree's scripts, which join once the daemon answers. Every line
  shows its key.
- **The sidebar resizes** (UX1.7, UX1.8): drag its edge (the divider column), or press `<` / `>`
  with the sidebar focused (`sidebar.narrower` / `sidebar.wider`, a column at a time). The width
  stays within 12–80 and leaves the panes at least 21 columns, and is saved with the workspace's
  view like the rest of the layout; `[ui] sidebar_width` is only the starting width.
- **Quit ptyro** (`app.quit`, `C-d` after the prefix; the which-key menu's "this" group and the
  help list it beside detach): a confirmation names what stops (agents that can resume
  hibernate, the rest end for good) and offers detaching instead; `y` sends
  `Shutdown{force: true}`, the same as `ptyro daemon stop --force`. Once it's confirmed, the
  daemon closing the connection ends the TUI (`AttachEnd::Stopped`, "[ptyro stopped…]")
  instead of reconnecting, which would start a new daemon.
- **Everything at once** (`Z`, `z` in the sidebar; `sidebar.zoom`): the sidebar fills the
  screen with every row open and wrapped. It is drawn over the panes, which keep their size, so
  no session is resized for a look. `⏎` opens a session and goes back; `C-q Z`, `z` or `Esc` go
  back (a bare `Z` there folds the sidebar, as anywhere in it).

### 11.2i What UX3 added **[built UX3]**

The daily loop (ROADMAP UX3, RESEARCH R-UX-34…37). `crates/ptyro-tui/src/screens.rs`.

- **Full-sized panels; only a split command fills a split** (UX3.1). `show(id, None)` places a
  session without a split (`place`): where it already shows (focused there); else in the kept
  layout that shows it (swapped back); else, in a single pane (a drawer aside), in that pane
  (`PaneState.last` remembers what it replaced); else full sized in a fresh one-pane layout, the
  split layout kept whole in `App.kept` (a `Screen`: layout, panes, focus, zoom, drawer; saved as
  `SavedView.kept`). `` ` `` (`session.last`) goes back and forth: from a chat (or the chat list)
  to the session under it, the chat put aside as it is (`App.chat_aside`: draft, scroll, a new
  chat's choices) and brought back by `` ` `` again; a session shown from a chat puts the chat
  aside the same way, one shown from the panes ends that. With no chat to go back to it swaps the
  kept layout and the screen, else shows the pane's previous session. Everything goes through it: new sessions and worktrees, the quick
  terminal's panel, ⏎ in the sidebar, `j`/`k`, `1`–`9`, the finder, attention.
  - Tools open full sized (`spawn_tool`, `SpawnSpec.tool`): `$EDITOR` on a file and
    `config edit` give the screen back when they exit (`App.tool_back`); `run_cli` (the CLI's long
    tail, health fixes) keeps its output until `` ` ``.
  - `v` / `s` (`split_clone`): the focused pane's session (the selected row's, in the sidebar)
    cloned beside or below it: an agent as the same agent and profile in the same worktree with a
    fresh conversation (`override_pin`/`confirmed`: its source already runs there); anything else
    as a shell in its directory; an empty pane a shell here. The pane count is checked first.
    **[changed SH.13]** A shell of SHELLS is split by the daemon instead (`ShellSplit`, §23.17):
    the copy is a pane of its window, never a session placed in the worktree it stands in.
  - A split's pane whose session is removed closes; it never takes another session (UX1.23 still
    applies to a single pane). A restored view closes a split's empty panes and fills only a
    single one. Sessions that go are dropped from the kept layout, which goes when it is empty.
  - Unchanged: the drawer (`C-t`) and the scratchpad's `mode = "pane"`, both asked for by name.
- **Quick new** (UX3.2, **[changed UX3.7]**; `session.again`: `C-n` after the prefix and in the
  sidebar, the row menus, which-key's `+ claude`): `SpawnSpec { again: true }` in the context's
  worktree (else here), full sized. It always starts an agent: the worktree's last one (with its
  profile and arguments), else the project's `default_agent`, else `[agents] default`; `!` is the
  shell key. The label comes from `LastLaunch { worktree }`, asked on attach, after each spawn,
  and from `tick` whenever the context moves to another worktree (`App.again_for`).
- **Created means shown** (UX3.3, `reveal`): `show` unfolds the session's workspace, project and
  worktree and selects its row; a row the tree can't place yet is retried on each event for 5 s
  (`App.revealing`). A new worktree with no session shown is selected (`reveal_worktree`). A new
  workspace is upserted from `WorkspaceNew`'s reply.
- **`!` is a new shell** (UX3.4, `shell.new`): another plain shell in the context's worktree,
  every press, full sized; it counts for quick new. The quick terminal is `C-t`
  (`shell.drawer`) / `T` (`shell.pane`), and `shell.here` (⏎ on its line) follows
  `[ui] shell_style`, now `pane` by default. It belongs to a worktree: with no context worktree
  the TUI opens the project picker and carries on there (`AfterAdd::Run`); `QuickShell` refuses
  to start one outside a worktree (one per directory from before is still found).
  **[amended 2026-11-04, owner; proposed SH.2, §23.6]** The project picker is for the quick
  terminal only. `!` with no worktree around (an empty pane, home, a chat, a loose shell or the
  SHELLS heading selected) starts a shell in the workspace's SHELLS instead. The quick terminal is
  unchanged.

- **The sidebar filter** (UX4.1, `sidebar.filter`: `f` after the prefix or in the sidebar; or click a
  chip): a strip under the sidebar's title, `all 14  running 6  active 2`, picks what the tree lists.
  `sidebar.show_all`, `sidebar.show_running` and `sidebar.show_active` set one (the palette); the
  click and `f` do the same. `Show` (`ptyro-core`, `session.rs`) is the one definition for the TUI,
  the CLI and the web:
  - **running**: a real session (not a quick terminal, not the scratchpad's editor) whose process
    is alive. Exited, failed and hibernated ones are out.
  - **active**: running and busy now: an agent that is `working`, `blocked` (it waits for you: you
    want to see it) or `starting`, or a shell with a command running (OSC 133). An agent between
    turns, a shell at its prompt and a command session with no status are running, not active.
  - A header (machine/workspace, project, worktree) is listed only while something under it is; a
    change set shows only while its lead does. Folds, numbering (`1`–`9`) and `j`/`k` follow the
    filter. The count in a chip is of every real session, not of what is listed.
  - Chats aren't sessions: while it filters, a workspace's CHATS section keeps only the chat last
    written to (`last_ms`), and none under a workspace that isn't listed.
  - Saved with the view (`SavedView.show`), so a reattach has it as it was. Filtered to nothing,
    the sidebar says `nothing active right now` and which key shows all.
  - The web's session list has the same three chips (`sidebar.filter`), kept in the browser.
  - CLI: `ptyro ls --status running|active`.

### 11.2j What MM1.B added **[built MM1.B]**

- **Every workspace at once** (`tree.rs`): a header per workspace (`Row::Workspace`), this
  machine's first in the daemon's order, then each other machine's (`● laptop2 / work`, the dot in
  the machine's colour, then `offline 14:02`, `other version` with its fix, or `connecting…`, then
  its profile badge), each folding (`ws:<name>` in `collapsed`), with its projects → worktrees →
  sessions and its other sessions under it. A project in several workspaces shows under each (since 2026-11-09, §11.5a), its
  sessions under the one they started in. A session whose workspace isn't shown sits under its machine's `main`. Rows of another
  machine's sessions show their names without the machine (the header says it).
- **The workspace you're in** is the selected row's in the sidebar, else the focused pane's
  session's (`enter_workspace`): `n`, `C-n`, `N`, the status bar, quick new's label and new
  workspace-wide things go by it. The sidebar title says this machine first: `● laptop1 · work`.
- **One layout for every workspace**, saved as `view:_all` (`ALL_VIEW`); the last workspace's own,
  from before, is taken over once. `w` jumps to a workspace's header, unfolded, and leaves the panes
  alone; its picker groups by machine and says which are down. `+ new workspace` asks which
  machine when there are others.
- **Machines:** `machine.add` (asks for `ssh://HOST` or the code, runs `machine add`),
  `machine.pair`, `machine.list` (full sized), `machine.retry`, `machine.hide`, `machine.unhide`,
  `machine.remove` (asks); a workspace header's row menu has them, `machine.retry` first while it's
  down, and its hint is the fix. A link going down, coming back or running another version is said
  once. A remote session's pane header has its machine's dot, and its state on the right when down.
- Profile badges count per machine: another machine's implicit `default` shows none.

### 11.3 Default key map (after prefix) **[built M2; differs from the proposal where noted]**

**[built M2]** The real table is generated: `C-q ?` lists it and the palette shows each
action's key. Differences from the proposal below:
- `c` closes a pane (`x` stops a session).
- `C-r` restarts a session (`r`/`R` stay free for run scripts in M3).
- `C-l` redraws.
- `,` renames, `=` takes control, `` ` `` shows the previous session in this pane, `\` toggles
  the sidebar, `]` pastes the last yank.
- In the sidebar: `j/k/g/G` move, `⏎` opens, `x/X/R/,` act on the selected session.
- `[keys]` in config rebinds keys (`"key" = "action [arg]"`, `"global:key"`, `"nav:key"`, `"none"`
  unbinds); unknown actions or keys are reported by `ptyro config check` and as a toast. Every
  default can be overridden or unbound; the settings panel lists them all (§13.7).

**[built M3]**
- `n` is the new-session wizard (§11.2c); a typed command line is the palette's
  `session.command`.
- `N`, `w`, `o`, `r` and `R` work as listed.
- In the sidebar, `⏎`/`Space` fold or open, `h`/`l` fold and unfold, and `x` on a worktree row
  removes it (with confirmation and the safety checks).
- **Fold everything** (`Z` in the sidebar, `C-z` after the prefix; `sidebar.fold_all`): every key
  the sidebar can fold (`Inputs::fold_keys`: change sets, `ws:<name>`, `chats:<name>`, projects,
  worktrees) goes into `collapsed`, so the sidebar is the change sets, then one line a workspace,
  and opening one by hand shows its levels folded too. The set folded before is kept
  (`folded_before`) until `Z` puts it back or you fold or open a row by hand (`fold_by_hand`; a
  workspace's header folds like a project's, by key or click): folding again after going to a
  session (which opens its workspace) keeps the first fold's. Pressed again while every foldable
  row *on screen* is folded (`Row::fold_key`), that set comes back; with none kept (folded by
  hand), only the rows on screen open and the folds under them stay. What arrived under a folded row meanwhile (a worktree, chats
  turned on) isn't on screen, so it doesn't count, and it opens with the rest. Going to a session
  unfolds its workspace to show it (`reveal`). The selection moves to the nearest row above it
  that still shows. TUI only: the CLI has no tree to fold, and the web's session list is flat
  (sorted by who needs you), so there is nothing there to fold.

**[built M6]** `D` opens the inspector (§11.2f), in the sidebar too; `Q` is the review queue.

**[built M7]** `O` opens the runs view (§11.2g), in the sidebar too.

**[built M4]** `a`, `i` and `p` work as listed (`p` after the prefix peeks at the session that
needs you most). In the sidebar, `a`/`i`/`p` work without the prefix, and `e` explains the
selected session's status; `e` has no key after the prefix (palette: `session.explain`).

**[built M10]** `t` toggles a structured session between the chat view and the terminal view.

The full sidebar table (no prefix) is in [`docs/keys.md`](docs/keys.md#in-the-sidebar).

| Key | Action |
|---|---|
| `?` | help overlay (searchable) |
| `:` / `Ctrl-p` | command palette (every command, fuzzy) |
| `n` | new session (wizard: agent → target worktree/new worktree) |
| `N` | new worktree (+ agent) wizard |
| `j` / `k`, `↑` / `↓` | next / previous session in sidebar order |
| `1`…`9` | focus session by sidebar index |
| `a` | jump to next session needing attention |
| `p` | **peek**: popup with the last screen + pending question of the session that needs you most (in the sidebar: the selected one); reply inline without switching |
| `i` | inbox |
| `/` | fuzzy find a session |
| `Tab` | toggle focus sidebar ↔ pane |
| prefix again (`C-q C-q`) | the same as `Tab` |
| `s` / `v` | split horizontal / vertical with a picked session |
| `h`/`l` (in split) | move focus between panes |
| `z` | zoom pane |
| `[` | copy/scroll mode (vi keys, `/` search, `y` yank via OSC 52, `]` / `[` prompt jumps via OSC 133) |
| `D` | diff inspector for the focused session's worktree (M6) |
| `o` | open worktree in editor (picker of configured editors; `nvim` opens *inside ptyro* as a session) |
| `r` | run default run script; `R` picker |
| `x` | stop session (confirm); `X` kill |
| `Alt-1`…`Alt-9`, `Alt-0` (no prefix) | **[proposed SH, §23.6]** show shell N of the workspace's SHELLS; an empty number starts one there |
| `C-c` / `e` | **[proposed SH]** a new shell in SHELLS (tmux's `c`) / the shells menu (a digit picks one) |
| `b` | broadcast prompt to selected sessions (multi-select with `space` in sidebar) |
| `w` | workspace switcher (shows each workspace's profile label and attention badges) |
| `S` | change sets (§15.5.10): the list; in the sidebar on a set row, that set |
| `d` | detach (TUI exits, everything keeps running) |
| `C-d` | quit ptyro (`app.quit`): after a yes, stop the daemon and every session, then exit |
| `q` | send the prefix key itself to the program (`prefix.send`) |

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
- **New session/worktree wizard** (fast path: `n`, `Enter`, `Enter` = the default agent in a new worktree of
  the current project with a random name).
- **Inspector** (M6): Diff (file list + side-by-side/unified hunks, syntax highlighted via
  `syntect` optional), Commits, Checks, Ports, Events (status timeline — "why is it blocked?").
- **Fan-out compare** (M7): N worktrees side by side: diffstat, test result, agent summary, pick.

### 11.5a The creation form **[decided 2026-11-09 (owner); built 2026-11-09]**

Everything the TUI creates opens one form (`crates/ptyro-tui/src/form.rs`, `Overlay::Form`)
holding every option of the matching CLI command (§1.3, "Creating shows every option"):

- **Fields.** Text (a name field holds itself to `[a-z0-9._-]` as it's typed, a dimmed
  placeholder says what empty means), a choice (`‹ value ›`, ←→ or a letter changes it), or
  ticks (space ticks, ←→ moves, `a` all). The selected field's hint is under the fields.
- **Keys.** ⏎ creates from any field, so typing a name and ⏎ is still the whole quick path;
  Tab/↑↓ move; `C-u` clears; Esc cancels. A field that is needed or wrong is selected and
  said in red; the form stays.
- **What it makes.** Native requests where the TUI had them (workspace, PR, fan-out, chat
  import, project, its workspaces, a SHELLS shell, a session or worktree), else the CLI's own
  command run full sized (`Make::Cli`: each field is a positional word, `--flag value`,
  repeated flags from a comma list, a switch, or a subset such as `--gates`/`--no-gates`).
- **Where.** `+ new workspace` (name, profile (the one named like it chosen) or
  `+ new profile…`, which makes it and comes back to this form, notifications);
  `profile.new` (name, label, colour, each agent's directory to adopt, git identity, SSH
  key, matching remotes, then a login); the task box's `C-o` and `N` (task, agent, shell or
  command, profile, where, start from, base, name, title, verify, structured, setup,
  fetch); `project.add` and `project.workspaces`; `shells.command`; `sets.new`, `set.add`,
  `recipe.run` (a field per variable), `session.lead`, fan-out (`F` in the runs view), `P`
  in the inspector, `remote.pair`, `machine.add`, `machine.pair`, `chat.import`.
- **Quick creates stay quick.** `n` ⏎ (the task box), `C-q C-c` (a shell, at once) and
  `Alt-N` keep their one-key paths; their form is one key away (`C-o`, `shells.command`).
- **A project in several workspaces** shows under each in the sidebar; a session is listed
  under one: its home workspace when that has the project, else the first that does
  (`tree::Inputs::home`). Selection keeps to the copy of a row nearest where it was.

---

## 12. The CLI

### 12.1 Conventions **[decided]**

- `ptyro` with no args: attach the TUI (auto-start the daemon). **[built M1]** It attaches to the
  newest live session, else the newest one, else starts a shell session.
- **[built UX1.9]** `ptyro "fix the flaky test"` (words, not a command): the default agent with
  that as its first prompt, in a new worktree named after it in the repository you're in
  (registered on the way, §10.1), or right here outside one; then it attaches. One word alone is
  refused as a likely mistyped command.
- **[built M1]** Attaching from inside a ptyro session is refused unless `PTYRO_ALLOW_NESTED=1`
  (a nested TUI fights over the prefix key).
- **[built] One CLI surface (owner, 2026-11-09):**
  - A short flag means one long flag in every command: `-a --agent`, `-C --cwd`, `-d --detach`,
    `-e --editor`, `-f --follow`, `-l --limit`, `-m --message`, `-n --name`, `-o --output`,
    `-p --project`, `-s --signal`, `-u --unread`, `-w --workspace`, `-y --yes`; and a long flag
    that has a letter has it everywhere it is taken. The rest is long only (`--prompt`, `--all`,
    `--force`, `--session`, `--wt`). The table is in `docs/commands.md`; the test
    `a_short_flag_means_one_thing_everywhere` walks the command tree and fails on a clash.
  - A value follows a flag after a space or an `=`, for every flag: none insists on `=`
    (`no_flag_insists_on_an_equals_sign`). `config … -p .` is the repository you're in.
  - A command that starts a session you'd work in (`new`, `wt new --agent`, `shells new`,
    `shells split`, `chat task`) attaches in a terminal unless `-d/--detach`; `--json` prints
    instead. Without a terminal, or inside a ptyro session (whose TUI shows the new one), it
    starts detached and prints it. A command whose session isn't one you'd type into (`wt run`,
    `fanout`'s candidates) attaches only when asked (`wt run --attach`).
  - A name you may leave out is `-n/--name`; a name a command needs is a positional word.
    `fanout`'s worktree-name start is `--prefix`.
  - `retitle X` with no title clears it, for a session, a worktree and a chat alike; a chat's
    `--auto` (the TUI's and web's `chat.autotitle`) asks for a new automatic one.
- All listing/query commands support `--json` (stable schemas, documented in `docs/cli-json.md`)
  and `--format` templates later.
- Addresses per §2.3. Multi-target commands accept `@blocked`, `@all`, etc.
- Destructive commands ask for confirmation on a TTY; `--yes` for scripts.
- **Removing is one rule in every client** *(owner, 2026-11-09)*. Every destructive command
  (`chat rm`, `project rm`, `wt rm`, `wt archive-rm`, `ws rm`, `profile rm`, `devices revoke`,
  `machine rm`, `cs drop`, `cs rm`, `discard`, `config rm`, `review rm --all`; `gc` asks per
  item) asks on a
  terminal unless `-y`/`--yes` (`cli::ask_first`); with no terminal and no `--yes` it refuses
  (exit 5, `… (no terminal to ask: pass --yes)`) and changes nothing. The TUI and the web always
  ask with a dialog. All three say the same thing, what goes and what stays, from one sentence
  per kind of removal in `ptyro_core::removal`. `removal::DIALOGS` pairs each question's start
  with its sentences, and a test checks that the web dialog asking that question (one
  `confirm(…)` line in `web/src`) says those sentences word for word. Stopping, killing and removing sessions have their own rule.
  - **Never by a fuzzy match.** A destructive command takes an exact address only: a name, an
    id, an id suffix (a handle) of 4 or more characters, or `@last`; never words from a title and
    never a substring. A chat's title words, `@pinned` or `@unread` are refused (exit 4) with the
    chats they match listed by handle (`ChatDelete` resolves with `find_chat_as(addr, exact)`).
    This holds for every kind: they all resolve by the one policy of §2.3 (runs by the end of
    their id, archives by name or handle, several of one name listed, never picked).
  - **Resolved, then asked.** The CLI resolves the address before it asks, so a typo is "not
    found" without a question, and the question names what was found (`wt rm`, `ws rm`,
    `project rm`, `cs drop`/`rm`, `discard`, `machine rm`, `devices revoke`, `review rm --all`;
    `chat rm`'s question comes from the daemon after it resolves). `profile rm`, `config rm` and
    `wt archive-rm` have no request that resolves first: they ask naming what was typed, and a
    name that matches nothing is refused after the question, with nothing changed.
  - **Say exactly what happens.** Removing a worktree deletes its directory, and its branch too
    when it is merged into its base or the same as its upstream (`BranchFate`), else keeps it;
    `--keep-branch` always keeps it. Removing a workspace moves its sessions (still running), its
    chats and the projects no other workspace has to main. Forgetting a project drops its trust,
    pin and place in workspaces; its worktrees stay on disk.
  - **Running sessions don't trap a project.** `project rm` with live sessions is refused with
    both ways through (stop them, or let them go). The CLI's `--release` lets them go (at a
    terminal it offers to), and the TUI and the web offer "Let them go"; they keep running,
    outside any project. The refusal carries that way through as `fix: release` (§6.6), which
    is what each client acts on. One sentence, `removal::PROJECT_RELEASE`, says so: in the daemon's
    refusal (which all three show) and in `project rm --release`'s question.

### 12.2 Command tree **[decided]**

The proposal as written. Built syntax differs in places (`export --format asciicast|text|ptylog`,
`gate resolve ID CHOICE`, `wt new --no-setup`, `profile env P [--json]`, `verify S [CHECK]`);
[`docs/commands.md`](docs/commands.md) and `ptyro <command> --help` are the reference.

```text
ptyro                                  # TUI
ptyro attach [<session>] [--raw] [--view-only] [--workspace <ws>]
ptyro new [--agent <a>[@<profile>]] [--profile <pr>] [--project <p>] [--wt <name>|--new-wt [<name>]|--here|--cwd <dir>]
          [--name <n>] [--prompt <text>|--prompt-file <f>] [--detach] [--override-pin] [-- <args…>]
ptyro ls [--status live|active|exited|dormant|<agent status>] [--project <p>] [--all] [--json]   # §2.3's words
ptyro status [<session>] [--explain] [--json]
ptyro send <session…> <text> [--no-enter] [--raw] [--wait-ready]
ptyro read <session> [--screen|--scrollback [N]] [--plain|--ansi] [--json]
ptyro wait <session…> --for working|blocked|idle|done|error|exit|any-attention [--timeout 10m] [--any|--all]
ptyro events [--follow] [--session <s>] [--json]       # NDJSON stream of domain events
ptyro stop|kill|restart|resume|hibernate <session…>
ptyro rename <session> <name>
ptyro replay <session> [--speed 2]      ;  ptyro export <session> --asciicast|--text [-o file]

ptyro ws ls|new <name> [--profile <pr>] [--notify all|badge|mute]|rm <name>|rename <a> <b>
         |add-project <ws> <project>|remove-project <ws> <project>|set <name> [--profile <pr>] [--notify …]
ptyro profile ls|show <pr>|new <pr> [--adopt <agent>=<dir>…]|rm <pr>|rename <a> <b>
              |login <pr> <agent|gh>|env <pr> [--agent <a>]|sync <pr>|doctor [<pr>]
ptyro project add [<path>]|ls|rm <p>|show <p>|rename <p> <new>|trust <p>|config <p>|pin <p> <profile>|unpin <p>
ptyro wt ls [--project <p>]|new [<name>] [--base <ref>] [--branch <b>] [--pr <n>] [--setup run|skip]
         |rm <wt> [--force] [--keep-branch]|status <wt>|diff <wt>|open <wt> [--editor <e>]
         |run <wt> [<script>]|lock <wt>|unlock <wt>|rename <wt> <new>|rename-branch <wt> [<new>]
ptyro fanout --agent claude,codex --prompt <p> [--base <ref>] [--prefix <prefix>] [--wait]
ptyro broadcast <session…> <text>
ptyro run ls|show <run>|cancel <run>|gate <run> <gate> approve|reject
ptyro recipe ls|run <recipe> [--var k=v]

ptyro init                             # first-run onboarding (§12.3)
ptyro doctor                           # diagnose: agents installed/authenticated, terminal caps,
                                       #   socket perms, git version, hook integration health
ptyro about [--json]                   # version, repository, licence, build, paths, daemon, tools
ptyro config path|edit|check|show [--effective]
ptyro daemon start [--foreground]|stop [--force|--hibernate]|restart|status|logs [-f]
             |upgrade|install-service|uninstall-service
ptyro gc [--dry-run]
ptyro completions bash|zsh|fish        # dynamic: completes live session/worktree names
ptyro shells [ls]|new [-- CMD…] [-C DIR] [--slot N] [--scratch]|go N|move N M|split [S] [--down]|break S|join S N|loose S|adopt S [WT]|watch S [--done|--activity|--silence D|--match RE|--off]   # SHELLS (§23; proposed)
ptyro shell-init bash|zsh|fish|pwsh    # OSC 7/133 snippets for shells ptyro didn't start (§8.7)
ptyro setup [-y] [--agents A,B] [--tools T,U] [--where W] [--dry-run] [--json]   # the setup checklist (§24.2, §24.3)
ptyro install NAME… [-y] [--where W] [--dry-run] [--json]             # install agents/tools by name; W: wsl|windows|both (Windows)
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
`send S TEXT [--no-enter]`, `daemon start [--foreground]|stop [--force]|status [--json]|logs [--lines N]`,
the hidden `fake-agent` (§18.2), and the global `--json-wire`. `kill` waits for the graceful stop
and then removes the session unless `--keep` (changed 2026-11-09: `kill` is SIGKILL and keeps it, §7.3). Still to come from this tree: `--view-only`,
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
  `default`), ports, and STATUS: an agent's status as `ptyro status` words it (`blocked:
  trust this folder?`, `(guess)` when low), else what the program reports of itself (OSC 7501).
  It showed only the latter, so an agent that reports nothing (Codex) read blank while blocked.
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
  - `events [--follow] [--session S] [-l N] [--json]`;
  - `stop S…` (graceful, kept listed; `kill` also removed until 2026-11-09, §7.3).
- **Listing:** `ls --status/--project/--workspace`; `attach --workspace`.
- **`attach --tab|--pane`** (WIN3, `ptyro_tui::tab`): open the session in a new tab or pane of
  the terminal instead, attached with this ptyro and its `--remote`. The innermost terminal that
  can be asked wins: tmux (`new-window` / `split-window -- <ptyro> attach <id>`), else Windows
  Terminal (`WT_SESSION`): `wt.exe -w 0 new-tab|split-pane -- <ptyro.exe> …` from Windows, or
  from WSL `wt.exe … -- wsl.exe -d <distro> --cd ~ -- <ptyro> attach <id>` (`wt.exe` from `PATH`,
  else the Windows user's `WindowsApps`). wt splits its command line at every `;`, even after
  `--`, so each argument's `;` goes as `\;` [V 2026-11-03]. Anywhere else the error names the
  command to run in a new tab. TUI: `session.tab`, `session.pane` (the palette, and a session's
  row menu when there is a terminal to ask). Web: a session's ⋯ → Open on the computer (the
  `session.tab` host op: inside WSL it asks Windows Terminal when `wt.exe` is there; the server
  has no `WT_SESSION` of its own).
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
  - `send --wait` waits for idle, blocked, error or exit, and prints how the turn ended.
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
  - inside WSL: the version; interop (off in `/etc/wsl.conf`, or its binfmt entry gone, each with
    its fix); **survival**, from the daemon's `wsl_hold` (§20.10): ok while it holds, a warning
    with the setting as its fix when `hold_wsl` is off, `ptyro daemon upgrade` for a daemon that
    predates it, the reason when it can't; NAT hiding an open web app; projects on a Windows
    drive; CRLF scripts;
  - terminal (TERM, truecolor, kitty keyboard via a query);
  - tmux `allow-passthrough`/`extended-keys`/`set-clipboard`;
  - git (warns below 2.40, the documented requirement) and gh;
  - agent versions against the ones verified;
  - per profile: logins by each tool's own command (`claude auth status --json`, `codex login
    status`, `gh auth status`; Gemini by its credential file), git identity, SSH key; then pins
    that contradict `match`.

  A `problem` exits 1.
- **`install-service`:**
  - **systemd:** writes `<config>/systemd/user/ptyro.service` (`$XDG_CONFIG_HOME` or `~/.config`) with
    - `ExecStart=<ptyro> daemon start --foreground`;
    - this shell's `PATH`, `PTYRO_HOME` and XDG variables;
    - `KillMode=mixed`, so stop is a SIGTERM to the daemon alone and agents hibernate;
    - `Restart=on-failure` with `RestartPreventExitStatus=5` (5 = "already running").

    Then `daemon-reload` and `enable`, plus `start` only if no daemon runs.
  - **Inside WSL with systemd off** (WIN2) it writes the unit, makes the
    `default.target.wants/ptyro.service` link `enable` would, and offers to turn systemd on
    (`crate::wsl_conf`): it explains, asks, then through sudo (`sudo -n` without a terminal, so
    it fails at once with the fix instead of waiting) copies `/etc/wsl.conf` to
    `/etc/wsl.conf.ptyro-backup` and writes it with `[boot] systemd=true` (an existing
    `systemd =` under `[boot]` is replaced, nothing else changes). Then it says how to restart
    WSL: `ptyro daemon stop --hibernate`, then `wsl --shutdown`. The service is for starting at
    login; what keeps the daemon running once the last terminal closes is its hold on WSL (§20.10).
  - **Windows** (WIN10) **[built 2026-11-05]**: a per-user logon task in Task Scheduler,
    `\ptyro\daemon`, never a Windows Service (a service runs in session 0, without the user's
    profile, credentials or desktop, all of which agents need). Its XML (`service::task_xml`):
    a `LogonTrigger` for this user (`USERDOMAIN\USERNAME`), principal `InteractiveToken` with
    `LeastPrivilege`, no time limit, on battery too, `IgnoreNew`; the action runs
    `ptyrow.exe daemon start --spawned`, or, when this computer's daemon is WSL's
    (`client.default_remote = "wsl://<distro>"`), `ptyrow.exe --remote wsl://<distro> daemon
    start`, which runs `ptyro daemon start` in that distro through `wsl.exe` and so boots WSL at
    logon (a systemd unit inside WSL only starts once something starts WSL; the daemon's hold
    keeps it running after, §20.10). `ptyrow.exe` is a GUI-subsystem trampoline shipped
    beside `ptyro.exe` that starts it with `CREATE_NO_WINDOW`: a console program started by Task
    Scheduler would keep a console (or Windows Terminal) window open for as long as it runs. A
    trigger for this user alone needs no administrator (`schtasks /SC ONLOGON` would). Registered
    with `schtasks /Create /TN \ptyro\daemon /XML <UTF-16 file> /F`, started at once with
    `schtasks /Run` when no daemon runs; `uninstall-service` deletes it, and the `\ptyro` folder
    when empty (Task Scheduler's COM API; `schtasks` can't). The task's daemon gets the user's
    own environment (the registry's), so a `PTYRO_HOME` or XDG variable set only in the shell is
    named, with `setx` as the fix. `doctor` says whether the task is there; `ptyro uninstall`
    removes it.
  - **launchd:** `~/Library/LaunchAgents/dev.ptyro.daemon.plist` with `RunAtLoad` and
    `KeepAlive{SuccessfulExit=false}`, loaded by `launchctl bootstrap gui/<uid>`.
  - **Uninstall** never stops a running daemon. Without systemd running it removes the link by
    hand and skips `daemon-reload`.
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
  - `pr reply [--wt W] [--comment ID] TEXT`;
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
- **Fan-out:** `fanout -a a,b,c | --count N [-a a] (--prompt P | --prompt-file F)
  [-p P] [--base] [--prefix PREFIX] [--test [CHECK]] [--verify CHECK] [--judge AGENT]
  [--timeout D] [--wait] [--json]`; `compare R [--refresh] [--json]`;
  `pick R CANDIDATE [--cherry-pick|--pr|--apply] [--into B|W] [--path F]… [--hunk F:N]…`;
  `discard R [CANDIDATE] [--force]`; `iterate R [CANDIDATE] TEXT`.
- **Gates:** `gate ls [--all] [--json]`, `gate resolve ID CHOICE [--note] [--key]`.
- **Mailbox:** `msg send TO TEXT [--kind] [--run] [--reply-to] [--interrupt] [--key]`,
  `msg check [--me S] [--since ID] [--wait D] [--json]`, `msg ls [--run] [-l N] [--json]`,
  `msg ack ID…`. Inside a session the sender is that session.
- **Leases and radar:** `lease claim PATTERN… [--exclusive] [--ttl] [--reason] [--as S]`
  (exit 5 when refused), `lease release [PATTERN…] [--as S]`, `lease ls [--json]`,
  `radar [--project] [--json]` (exit 1 when a merge would conflict).
- **Checkpoints:** `checkpoint ls [S|W] [--json]`, `checkpoint diff S N [M] [--stat]`,
  `checkpoint restore S|W [N] [--turn T] [--yes]` (alias `ckpt`).
- **Agents:** `verify S [CHECK] [--max-bounces N] | --off`; `new … --verify CHECK`,
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
5. Choose prefix keys, worktree location, notifications.
6. Write `~/.config/ptyro/config.toml` with comments; offer to install shell completions and the
   systemd/launchd service.
7. Offer the tour (`ptyro tour`, §11.2h).

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
  `scrollback_lines`, new subscriptions the new `frame_interval_ms`. `daemon.log_level` is
  accepted but not read yet; set the daemon's log level with `PTYRO_LOG` (env-filter syntax).
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
  - `on` (default `blocked, errored, done, idle, gate, run`);
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
  **[built M9]** `[remote]` is typed: `access = "localhost"` (`tailscale`, `open`; ACC1, §16.3),
  `port = 7091`, `public_url`, `serve = false`, `approval_wait = "10m"`, `push = true`,
  `push_hosts = []` (push services a device may subscribe with besides the browsers': FCM,
  Mozilla, Apple, Windows), `tls_cert`/`tls_key`. (`listen`, `tailscale`, `local`, `insecure`
  and `trusted_proxy` were replaced by `access` and `port`.) No section is
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
prefix = ["C-q", "C-Space"]  # or one key: "C-]"
theme = "ptyro-dark"
sidebar_width = 30
show_ports = true
confirm_kill = true              # Stop, Kill, and Stop and remove ask first (TUI and web)
shell_exit = "close"             # a shell that exits: close (disappears; agents stay) | keep
on_stop = "next"                 # after you stop a pane's session: next (its neighbour) | stay
sidebar_detail = "auto"          # lines under each session row: auto | compact | full
shell_style = "pane"             # what ⏎ on the quick terminal's line shows: pane (full sized) | auto (drawer if the project is open) | drawer
which_key_delay = 400            # ms after the prefix before the which-key menu; 0: never
progress_blocked = "yellow"      # Windows Terminal's tab/taskbar ring (OSC 9;4) while a session needs you
progress_errored = "red"         #   … while one has an error;  off | busy | green | yellow | red
progress_working = "busy"        #   … while one works. The first that applies and isn't off shows

[daemon]
scrollback_lines = 5000          # in memory, capped at 4M cells (fewer lines when wide); older lines come from the recording (§7.6)
restore_on_start = "dormant"     # dormant | resume | none
frame_interval_ms = 8

[recorder]
enabled = true
max_bytes = "16MiB"

[summaries]                       # opt-in one-line summaries from a small model (§11.2h)
enabled = false
daily_cap = 200                   # per profile, per UTC day
command = ["claude", "-p", "--model", "haiku", "--output-format", "json", "--strict-mcp-config", "--tools", ""]

[notifications]
channels = ["inbox", "osc", "bell"]      # + "desktop", later "push"
on = ["blocked", "errored", "done", "idle", "gate", "run"]
quiet_hours = "22:00-07:00"

[worktrees]
location = "data"                         # data | sibling | in-repo
branch_template = "{name}"
archive_on_merge = false                  # archive a worktree once its PR merges (UX1.12)
archive_purge_days = 0                    # delete archives this old (and merged branches); 0 = keep

[ports]
range = "20000-29999"
count = 10

[discover]
roots = ["~/dev", "~/src"]
auto_register = true             # the repository a session runs in becomes a project (§10.1)

[agents]
default = "claude"               # the default agent (UX3.7): `n`'s first choice, and what quick
                                 # new starts in a worktree that hasn't run one; a project's
                                 # default_agent wins in its worktrees. The older per-agent
                                 # `default = true` still counts when this is unset.

[agents.claude]
adapter = "claude"
command = "claude"
args = []
env = {}
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
min_work = "10s"

[editors]
default = "nvim"
nvim = { command = ["nvim", "{path}"], inside = true }   # opens as a ptyro session
code = { command = ["code", "{path}"] }
zed = { command = ["zed", "{path}"] }

[keys]                                     # overrides, after-prefix unless "global:"
"a" = "attention.next"
"global:M-1" = "session.focus 1"

[remote]
serve = false                              # the daemon runs `ptyro serve` itself
```

**Open in** (`wt open`, `worktree.open`). Only what is installed is offered, checked where it
runs (the TUI's and CLI's computer, or the daemon's for the web): a configured editor when its
`command[0]` is on `PATH` (or is an executable path); a built-in when its command is, and on
macOS `code`, `zed` and `cursor` also as `Visual Studio Code.app`, `Zed.app`, `Cursor.app` in
`/Applications` or `~/Applications`, opened with `open -a <app> {path}`. The built-in file
manager is named for the platform (`finder`: `open`; `files`: `xdg-open`, else `gio open`;
`explorer`) and answers to all three names, so a shared config keeps working. Inside WSL with
interop it is Windows' `explorer.exe` on `{winpath}` (the path as Windows names it,
`\\wsl.localhost\<distro>\…` or `C:\…`; any command may use it), and `code` and `cursor` are
also found where their Windows installers put them (under `%USERPROFILE%\AppData\Local\Programs`,
or `C:\Program Files`), run as `env WSL_DISTRO_NAME=<distro> <code> {path}` (§20.10). The default is
`editors.default` if installed, else `$VISUAL`/`$EDITOR` if it names an installed one, else the
first installed. Terminal apps come after the file manager, each where it is installed (macOS:
`terminal`, `iterm`, `ghostty` by `open -a`, `wezterm`, `kitty`, `alacritty`; Linux: those last four
and `ghostty`, `gnome-terminal`, `kgx`, `konsole`, `xfce4-terminal`, `foot`, by their directory
flags; §20.5). `shell` is not an editor: `wt open -e shell` (the picker's `!`, the web menu's
last entry) starts a shell session in the worktree through `Spawn`, so it needs the Full device
scope, as `!` does, while `WorktreeOpen` stays Control.

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

### 13.7 The settings schema and panel **[built CFG1]**

Every configuration key has one description, in `ptyro-core` (`settings.rs`): its **section**
(the panel's groups), its **kind** (bool, number with a range, text, path, one of a fixed few,
one of what exists (themes, agents, editors, actions), several of a fixed few, a list, a command,
a duration, a size, a time window, a colour, an address, a secret, or a table the panel leaves to
the file), one line of **help**, when a change **applies** (`live`: the daemon and every open
screen reload the file; `next`: sessions started from then on; `restart`: when the daemon
starts), whether it is **advanced**, and, for a notification service, which `type` it is for.
Defaults are not repeated: they come from `Config::default()`. Named things (agents, profiles,
themes, editors, key bindings, notification services, and a repository's run scripts and
prompts) are **collections**: their fields are the entries whose key reads `agents.*.command`.

- **A test fails when a field has no entry** (and when an entry has no field), so an option
  can't skip the panel. A new option needs its entry in the same change as its field.
- **Edits are text to text** with `toml_edit` (`settings::set|unset|add|remove|rename`): the
  file's comments, order and spelling stay, a value replaces in place keeping the comment on its
  line, and nothing is written unless the whole new file passes the parser the daemon loads with
  (plus, for your config, the TUI's own check of key bindings and themes). The refusal names the
  setting and why. `settings::preview` does the same without writing, for forms that check as
  you type; `settings::restore` puts a file back (undo).
- **Plain words, everywhere.** A setting may carry a `unit` (`columns`, `days`: shown after a
  number as `30 columns`; `config get` still prints the bare number) and `also` words a search
  finds ("notify", "sound", "shortcut"; every colour setting also answers to "color"). A
  collection has a `title` and an `empty` sentence (what it is for and what already works
  without one), shown by the TUI and the web alike. A theme just made becomes `ui.theme` so
  editing its colours shows at once; removing or renaming the theme in use moves `ui.theme` with
  it (`settings::apply` does this, so the CLI agrees). A new agent asks which program it runs
  (empty: its own name). `ui.theme` lives in Themes and `clipboard.allow_write` in Interface.
  A new theme first asks which built-in theme to copy (its `base`; the TUI picker, the web's
  "Start from" box). The `?` help ends with where to change any key (Settings → Key bindings).
- **Key bindings list every key, defaults too.** The TUI key map owns the defaults
  (`ptyro_tui::keymap::default_keys()`, named as `[keys]` names them: `d`, `nav:j`); a caller
  passes them in `Lookups.keys`, and `settings::bindings` merges them with `[keys]` into
  `Binding`s: `name`, `table` (`prefix`/`nav`/`global`), `key`, `action`, `default`, `standing`
  (`default`, `yours`, `changed`, `unbound`), `help` (the action's description, the default's
  while unbound), and the nine digit keys folded to one `fixed` row. They are in `View.bindings`
  (`bindings` in the web's `settings.view`; `config list keys --json`), sorted by table then by
  action. Keys compare as the key map does (`Q`, `S-q` and `shift-q` are one key:
  `settings::slot`), so a change finds the line already there whatever its spelling.
  `settings::owner` names what already runs on a key (the panels warn before taking it, and
  `config set keys.x` prints a note). **One change, `Op::Bind { changes }`** (`{t: "bind",
  changes: [[name, action | null]]}` on the web), sets several keys atomically, so a swap is one
  validation and one undo: `Some(action)` binds (`"none"` unbinds), `None` takes the line out so
  the default is back. A line that says what the default says is not written, and `"none"` on a
  key with no default is just removed, so the file keeps only what differs. The CLI's
  `config set|unset|add keys.<name>` go through it, so `config set keys.d none` unbinds a default
  and `config unset keys.d` restores it. No protocol change: this is the host API and the
  settings library, not the wire protocol.
- **Layers**: your `config.toml` (written private, 0600), and for a repository its
  `.ptyro.toml` (committed) or `.ptyro.local.toml` (yours; added to `.git/info/exclude`). Agents,
  profiles, keys and the rest are refused in a repository (§13.1). A value says where it comes
  from (`default`, your config, `.ptyro.toml`, `.ptyro.local.toml`) and the line.
- **Surfaces**: `ptyro config list|get|set|unset|add|rm|rename|schema` (the CLI; `--project` for
  a repository, `--shared` for the committed file); the TUI's panel, `C-q C` and the palette
  (every setting is a palette entry, `setting ui.theme`); the web's Settings page, over the host
  calls `settings.schema`, `settings.view`, `settings.apply` (`dry` only checks) and
  `settings.restore`, which need full scope. The first `config edit` starts from a commented
  file generated from the table (`settings::starter`).
- **What applies when** is true to the code: `daemon.log_level` follows the file while the daemon
  runs (`PTYRO_LOG` wins when set); a `[remote]` change starts a supervised web app again
  with it; `notifications.callback.*` binds when the daemon starts, so a change says so and
  offers the restart; scrollback, recording, ports, agents and profiles apply to what starts
  next.

### 13.6 The scratchpad **[built NOTE1; NOTE1.7 tried and dropped]**

One persistent notes file for ideas and bugs: plain Markdown, the same in every session and
workspace, found by `$PTYRO_SCRATCHPAD`. ptyro never owns the only copy: it is a file you can
`cat`, `grep`, sync and back up.

- **Where:** `<data>/scratchpad.md`, or `[scratchpad] path` (`~` expands; a symlink is written
  through, so the real file in a synced folder is replaced in place). The daemon makes it, with a
  short header, when it starts. `PTYRO_SCRATCHPAD` is set in the environment of every session
  (`launch.rs`, after the profile and worktree variables, and listed in `ptyro_pty`'s strip list so
  an enclosing ptyro's value never leaks into a nested one).
- **Items.** A line `- [ ]` / `- [x]` (or `* [ ]`) is an item. `ptyro scratchpad add` writes
  `- [ ] 2026-10-24 14:02 · `ptyro/bugs` · by `claude-rook` · the words`: the local time, the
  worktree (else the directory) it was run in, and for an agent the session's name; further lines
  are indented two spaces and belong to the item. Tags go at the end: `[wt:project/name]` (the
  worktree started from it) and `(#42)` (the pull request that finished it). Under `## Heading`
  with `--section`. `--no-stamp` leaves the stamp off, `--note` writes a plain bullet. An item is
  selected by its line number or by words in exactly one open item.
- **Writes.** `ptyro_core::scratchpad::Pad` is the only code that changes the file. An add is a
  single append (`O_APPEND`), so an editor with the file open sees only added bytes; anything else
  is a temp file and a rename. Every change first saves what was there to `<data>/scratchpad.history/
  <ms>-<rev>.md` (`rev` is the first 12 hex digits of the text's SHA-256). Copies thin out: all of
  them for an hour, one per ten minutes for a day, one per hour until `[scratchpad] keep` days
  (30); the newest 20 and a cap of 400 always hold. A change seen from outside (an editor saving as
  you type) is saved at most every 15 s.
- **Merging.** `ScratchPut{base, text}` writes `text` that was edited from revision `base`. If the
  file still is `base` it is written as is; otherwise the daemon three-way merges by lines
  (`similar` Myers diff against the text of `base`, kept in a ring of the last 64 revisions, else
  from the history): edits to different lines combine; two appends at the same place both stay (the
  file's first); the same lines changed two ways are both kept between `<<<<<<< on disk`,
  `=======` and `>>>>>>> your edit`. With no copy of `base` the whole of both is kept that way. The
  reply says whether the text came back different (`merged`) and how many regions clashed.
- **Watching.** The daemon looks at the file's mtime and size every 700 ms; a revision it hasn't
  announced becomes `ScratchChanged{rev, open, by}` to every client watching events (`by` is
  `file` for a change made outside the daemon). Polling, not inotify: it is the same on Linux and
  macOS, works on network and synced folders, and a pad is a few KB.
- **The pane.** `ScratchPane{create}` runs the editor as one session (`SessionInfo.pad`): `[scratchpad]
  editor`, else the `[editors]` default when it runs inside a terminal, else `$VISUAL`/`$EDITOR`,
  else the first of nvim, vim, nano, vi on `PATH`. vim and nvim start with `autoread` and a
  `checktime` autocommand so an append from elsewhere shows without a prompt. The session is quiet
  like a quick terminal (`Session::is_quick`) and forgotten when the editor exits; clients keep it
  out of the sidebar, numbering and counts. `[scratchpad] mode` (`drawer`, `popup`, `pane`) is how
  the TUI shows it.
- **Items become work.** `ptyro scratchpad start` (TUI: ⏎ in the items picker) starts a task like
  `ptyro "…"` with the item's text (and its indented lines) as the prompt, in a new worktree; the
  new worktree's spec carries `pad_item` (the line), and the daemon tags the item `[wt:…]` when it
  is created. When that worktree's PR is seen merged (the GitHub poller) or the worktree is
  removed, the daemon ticks the item with the PR number (`[scratchpad] tick_on_finish`, default on).
- **Agents.** A lead's `ptyro mcp` has `scratchpad_read`, `scratchpad_todo` and `scratchpad_add`
  (the add is stamped with the lead's name). Agents are not told about the pad by default; one
  that finds the variable can read it and should add with `ptyro scratchpad add`.
- **Scopes.** Reading (`ScratchGet`, `ScratchTodo`, `ScratchHistory`) needs `read`; writing
  (`ScratchPut`, `ScratchAdd`, `ScratchMark`, `ScratchRestore`) needs `control`; starting the
  editor session is `full`.

---
- **One pad (NOTE1.7, dropped 2026-10-26).** v0.0.8 and v0.0.9 also had a pad per project
  (`<repo>/.ptyro/scratchpad.md`, `$PTYRO_PROJECT_SCRATCHPAD`, a `pad` selector on the requests, a
  switch key). The owner found several pads too confusing, and v0.0.10 removed it: there is exactly
  one pad, the global one. Nothing in ptyro reads or writes a `.ptyro/scratchpad.*` file any more,
  and the files that exist are left alone (and the `/.ptyro/scratchpad.*` lines already in a
  repository's `.git/info/exclude` stay: they are harmless). The title of a new pad is `# Scratchpad`
  again; a file that 0.0.8 titled `# Scratchpad — everywhere` still reads as one pad (that title is
  not a section for the items). **Wire:** see §6.4, protocol 1.18.


## 14. Storage: files, directories, database

### 14.1 Paths **[decided]** (XDG on both Linux and macOS via `etcetera`; all overridable by
`PTYRO_HOME`, which puts everything under one directory — used by tests)

| Kind | Path | Contents |
|---|---|---|
| config | `~/.config/ptyro/` | `config.toml`, `themes/`, `recipes/` |
| data | `~/.local/share/ptyro/` | `ptyro.db`, `worktrees/`, `sessions/<id>/pty.log`, `scratchpad.md` + `scratchpad.history/` (§13.6), `integrations/`, `profiles/<name>/<agent>/` (managed profile dirs, `0700`) |
| state | `~/.local/state/ptyro/` | `daemon.lock`, `daemon.log*`, `daemon.prev` |
| runtime | Linux `$XDG_RUNTIME_DIR/ptyro/`; macOS `$TMPDIR/ptyro-<uid>/`; `PTYRO_HOME/run/` | `ptyro.sock` |

**[built WIN3]** On Windows (the client of a WSL daemon): config `%APPDATA%\ptyro\`, and data,
state and runtime under `%LOCALAPPDATA%\ptyro\` (`data\`, `state\`, `run\`); there is no
socket there. **[built WIN4]** An `XDG_CONFIG_HOME`, `XDG_DATA_HOME`, `XDG_STATE_HOME` or
`XDG_RUNTIME_DIR` that is set to an absolute path wins there too (`<it>\ptyro\`), as on Unix.
**[built WIN5]** The daemon's "socket" there is a named pipe, `\\.\pipe\ptyro-<8 hex>`: a hash of
the runtime folder, so one per user and per `PTYRO_HOME`. `PTYRO_SOCKET` must name a pipe
(`\\.\pipe\…`) on Windows. A file "only its owner reads" is mode 0600 on Unix; on Windows these folders'
inherited access list already keeps them to the user (`ptyro_core::os`). That config holds this
client's own settings (`client.default_remote`, the TUI's look and keys); the daemon's is in WSL,
as for any `--remote` client. Which is whose is `settings::read_by_client` (the client reads `ui.*`
but `ui.shell_exit`, `keys`, `themes`, `clipboard` and `client`; the daemon everything else). With a
remote daemon the TUI's settings panel tags the daemon's settings "daemon's", and a key that would
change one opens the daemon's own file there instead (`ptyro config edit <key>` in a tool session,
which runs on the daemon's computer); `ptyro config set`/`unset` of one says the daemon reads its own
file and names the command that changes it there. Tool sessions (`config.edit`, the CLI's long tail
from the TUI) run the daemon's computer's `ptyro` (`PTYRO_REMOTE_BIN`, else on its `PATH`), never
this client's path.

**[built M1]** If the socket path would exceed 100 bytes (macOS `sun_path` is 104; deep test
dirs), it falls back to `/tmp/ptyro-<uid>-<fnv32 of the original path>/ptyro.sock`, which is
deterministic, so every client finds it. The daemon refuses a socket directory owned by another
uid and tightens it to `0700` (a shared `/tmp` could otherwise be pre-seeded). `PTYRO_SOCKET`
overrides the socket path alone.

**[fixed 2026-11-09]** Under a `PTYRO_HOME`, `PTYRO_SOCKET` counts only when it is that home's:
inside the home, or the home's own fallback socket (a pipe on Windows: the home's own pipe).
Otherwise it is ignored. Every session's environment carries its daemon's `PTYRO_SOCKET`, so an
agent that sandboxed itself with `PTYRO_HOME=<scratch>` and ran `ptyro daemon stop --force` stopped
the daemon it was running in, and every session with it.

**[fixed 2026-10-23]** A daemon replaces an existing socket file only when connecting to it is
*refused* (nobody listens); any other failure leaves it alone and the daemon refuses to start,
saying why. On the way out it removes the socket only if the file is still the one it bound
(same device and inode). Before, any failed connection counted as stale. A daemon started
under a sandbox that forbids connecting (and with an outer session's `PTYRO_SOCKET`) deleted a
running daemon's socket, so new clients could no longer reach it.

### 14.2 Database

SQLite (bundled), WAL, `foreign_keys=ON`, migrations embedded with `rusqlite_migration`
(`user_version`). Only metadata — never terminal contents. **One exception, decided 2026-10-29:**
chat text, which lives in its own file, `chat.db` (§22.4), never in `ptyro.db`.

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

**[built UX1.24] Store v5** (migration 5): `session.title` and `worktree.title`. A session's
upsert also rewrites its `worktree_id` and `project_id`, since a session can now move into a
project registered after it started (§10.1).

---

### 14.5 Backup and uninstall **[built H2.3]**

`ptyro backup` writes a private (0600), never-overwriting `.tar.gz` of ptyro's own state: the
config, a consistent copy of the database (`Store::backup_to`, SQLite `VACUUM INTO`, safe with the
daemon running), the scratchpad and its saved copies, a `manifest.json` and a `README.txt` with
the manual restore. It leaves out the agents' login directories, the recordings (`--with-recordings`)
and the web app's CA private key (`--with-ca`). `ptyro uninstall` plans first (`--dry-run` stops
there; read-only, never starts a daemon): the running sessions, every directory it keeps (from
`Paths`, so `PTYRO_HOME` and `PTYRO_CONFIG` are honoured), worktrees with uncommitted changes
(from `git status`, not the daemon's cache), unpushed commits and locks, the login service,
integrations it installed, the shell-rc lines `ptyro init` and the mend list wrote (only those
exact lines), a `tailscale serve` mapping it made, and the binary. Typing `delete` is required
when a worktree has uncommitted work (`--yes` never skips it; `--delete-work` does); without a
terminal it refuses without `--yes`; inside a ptyro session it refuses (`PTYRO_HOME` set means
a sandbox). Order: backup, integrations, login service, stop the daemon and wait for its lock,
worktrees (through `git worktree remove`), directories, rc lines, the binary last. **[built WIN3]**
Uninstall is about this computer: it never follows `client.default_remote` or `PTYRO_REMOTE` (it
would stop that daemon and its sessions), and refuses `--remote`, naming where to run it. On
Windows there is no daemon to stop (the plan keeps "ptyro in WSL", with `wsl -- ptyro uninstall`);
a running `ptyro.exe` can't be deleted, so it is moved to the temp folder and a detached `cmd`
deletes it once the process ends; its folder goes when empty (and `…\ptyro\bin`'s parent), and
leaves the user's `PATH` where install.ps1 put it. Kept: the
user's repositories, branches, `.ptyro.toml`, `.ptyro.local.toml`, `.git/info/exclude` lines and
`refs/ptyro/*` snapshots (`--purge-refs` deletes the last). The binary is removed only from a
place the user put it (`~/.local/bin`, `/usr/local/bin`); `~/.cargo/bin` goes through `cargo
uninstall`; Homebrew and system paths get the package manager's command; a path under
`/target/` is never removed. In a sandbox (`PTYRO_HOME`) only that home is touched. Web:
`uninstall.plan` and `backup.create` host operations (full scope); the page shows the plan and
the command, because removing ptyro ends the server the page is served by. `make reset` is a
wrapper over it. Details: `docs/uninstall.md`.

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

`ptyro fanout -a claude,codex,gemini --prompt-file task.md`:

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
├─ api        codex    · worktree api/   branch add-user-tier   depends_on: —
├─ sdk        claude   · worktree sdk/   branch add-user-tier   depends_on: api
└─ web        claude   · worktree web/   branch add-user-tier   depends_on: api, sdk
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

`--count N` cycles through `--agent`.

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
  out: done, claimed). Bounces reset on a prompt that isn't a bounce, except while one is
  outstanding (`bounced`): hooks arrive on separate connections, so the turn's own prompt can
  be reported after the turn was checked.
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
  over, with the last message: the agent's own reply when its adapter reports one, else the
  screen's last line), blocked and exited on their own.
- **Typing** into a session is serialized: a prompt (text, pause, Enter) never interleaves with
  another typist. **[H2.7]** `send --raw` (`SendBytes`) waits its turn too. Stop keys and
  approval answers don't wait, and neither do keys typed live.

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
Candidates of the same fan-out or verify step are alternatives and aren't messaged, also after
the run has ended (a human taking one over). Mail typed into a structured session queues as a
turn of its own ahead of the next prompt, so a stray message also ends `send --wait` early.

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

**[built WIN11, 2026-11-05]** A Windows host has no `sh`: its OpenSSH server runs the command in
`cmd.exe` or PowerShell, which both run `ptyro proxy` as it is (a binary with a space goes in
double quotes, which cmd takes). The POSIX command goes first; when it fails and ssh itself got
through (not exit 255), the client asks the host `echo %OS% $env:OS` (`Windows_NT` from cmd or
PowerShell, whatever the locale) and, when the answer differs from what it tried, tries the
other command and remembers the host (`<state>/ssh-windows-hosts`, `[user@]host[:port]` a
line), so the next connection starts with the right one. A POSIX host pays nothing; a failing
one pays one probe. On a Windows computer whose daemon is WSL's (`client.default_remote =
"wsl://…"`), `ptyro proxy` hands its stdin and stdout to `wsl.exe` running the same bridge in
that distro, so ssh reaches the daemon the owner uses there (never inside WSL itself, where the
same config would send it round again). `ptyro serve` runs natively too: its CA and keys are
written private (narrowed to the user, SYSTEM and Administrators when their folder lets others
read), its certificate names `%COMPUTERNAME%`, the Tailscale CLI is found where its installer
puts it when it isn't on `PATH`, `pair --open` offers to trust ptyro's CA in Windows' user store
as it does from WSL, and a server the daemon started leaves when that daemon's process is gone
(Windows has no parent pid to watch). MM1 links a Windows machine either way: over ssh as
above, or over `machine-bridge` to its `ptyro serve` (`tests/win11.rs`).

**[built WIN3]** `--remote wsl://<distro>` (`wsl://` is the default distro) runs `wsl.exe [-d
<distro>] --exec sh -c 'PATH=… exec "$0" proxy' <bin>`, the same script as over ssh, with
`WSL_UTF8=1`. Distro names are `[A-Za-z0-9._-]`, so nothing option-like reaches `wsl.exe`. It
works from Windows and, through interop, from inside WSL (which is how it is tested). `wsl.exe`
writes "there is no distribution" on stdout, where the protocol was expected, so a failed
handshake asks `wsl.exe -l -q` (UTF-16, whatever `WSL_UTF8` says) and names the distros there
are. `PTYRO_WSL` replaces `wsl.exe`. Variables cross both hops when listed in `WSLENV`.
- **Where the daemon is:** `--remote`, else `PTYRO_REMOTE`, else `[client] default_remote`.
  **[built WIN5]** `local` there is this computer's own daemon, on Windows the native one (over
  its named pipe), and the only way a Windows client starts one by itself; with no remote, a
  Windows client uses a native daemon already running but starts none. On
  Windows with none of them, the first command that needs a daemon (not `config`,
  `completions`, `shell-init`, `init`, `uninstall`, `backup`, `daemon`, `proxy`) asks each
  distro (WSL's order; `docker-desktop`, `rancher-desktop`, `podman-machine` left out) for
  `ptyro --version`. The first that answers is offered (on a terminal "Ubuntu (WSL) runs ptyro
  0.0.48. Use it? [Y/n]", kept in `client.default_remote`; without one it is used and a note says
  how to keep it). None: an offer to run the README's install line in the default distro.
- **Paths:** the working directory a Windows client sends is the distro's (`C:\src` is
  `/mnt/c/src`, `\\wsl.localhost\Ubuntu\home\me` and `\\wsl$\…` are `/home/me`; `\\?\` is
  ignored; a network share is refused with the fix). For any remote client, "open in" (an editor,
  the file manager) lists the daemon's editors (`Request::Editors`) and has the daemon open them
  (`WorktreeOpen`), in the TUI and `wt open`, as the web already did: from WSL, VS Code opens
  through Remote-WSL and folders in Explorer (§20.10).

### 16.3 WebSocket server

`ptyro serve` exposes the protocol over WebSocket, always over HTTPS (**[built WEB1.7]**, below).
**[changed ACC1, 2026-10-28] Two settings say where, and nothing else does.** `[remote] access`
(`localhost` | `tailscale` | `open`, default `localhost`) says who can reach it; `[remote] port`
(default 7091, 1024–65535) says on which port. They replace `listen`, `tailscale`, `local`,
`insecure` and `trusted_proxy`: five settings that had to agree, where a contradictory pair
(`tailscale` and `local`) made the server exit with an error only the log showed (the owner's
bug report: "this is too dang confusing"). Principle of Obviousness (AGENTS.md): a person picks
from a dropdown of three words, each with a sentence; the combination that fails cannot be made. Auth: long random
device tokens (`ptyro pair` prints a one-time code the device trades for one), stored hashed;
revocable (`ptyro devices ls|revoke`).

**[built M9]** `ptyro serve` is its own process (hyper + tokio-tungstenite), so the daemon stays a
local socket. With `[remote] serve = true` the daemon supervises it (backoff, `state/serve.log`,
killed before a live upgrade, restarted by the next image). **[built H2.7]** At 5 MiB,
`serve.log` is copied to `serve.log.1` and emptied in place; the server writes with `O_APPEND`, so
it carries on at the new start.

**[built WEB1.9] The server is a setting you can find and turn on.** Decision (owner, 2026-10-24):
it stays a supervised child process, not code inside the daemon. The daemon owns every PTY and
is local-socket-only by design; the network-facing HTTP/TLS code belongs in a process that can
crash, restart and be killed for a live upgrade without touching a session. What the person sees
is "the web is up when the daemon is".
- **One switch, five doors.** `ptyro web on|off|status [--via tailscale|lan|here] [--pair]`;
  `ptyro daemon start --serve`; the question in `ptyro init`; the TUI's `remote.web`,
  `remote.web_on`, `remote.web_off`; and the web app's More → Web app. All of them are
  `Request::Web` (§6.4) or the same `set_serve` on the config file, so they cannot disagree.
- **It reacts at once.** The supervisor waits on a wake-up, not only its 3 s look, so a config
  reload (the file edited by hand, `ReloadConfig`) or the switch starts or stops the server
  immediately and resets the backoff.
- **It knows how it is.** The server writes `state/serve.json` (pid, URL, CA fingerprint) when it
  listens; the daemon starts the record afresh with each child it starts, and trusts it only when
  its pid is the child's. `DaemonStatus.web` has `up`, the URL, how long, how many restarts and
  how the last run ended (its exit status and its last line of `serve.log`, e.g. the port is
  taken). A `ptyro serve` someone started by hand (a live pid in the record that isn't the
  child) shows as `by_hand`: the daemon's can't share its port, and the hand one won't come back.
- **Where it shows.** `ptyro daemon status` (a `web app` line), `ptyro doctor` (`web app`: ok, a
  note when off or starting, a warning for a by-hand server, a problem with the fix when it fails,
  e.g. "another program holds 127.0.0.1:7091: pick another port"), and More → Web app, which
  also turns it on or off and pairs a device.
- **Reaching it.** On with nothing chosen, `set_serve` keeps the file's `[remote] access`, else
  picks `tailscale` when `tailscale status` says Running, else `open` (a phone is the point, so
  the default is one that works at once; the note and `ptyro doctor` say what `open` means).
  `ptyro web on --access localhost|tailscale|open` (or the words `here` and `lan`, the old
  `--via`, which still work) sets it; so do `ptyro config set remote.access …`, the settings
  panel's dropdown (`remote.access`, with `remote.port` beside it), and the web app's More → Web
  app ("Who can reach it", with a Change button; the page asks the same three questions when it
  turns the server on). `WebServe` carries `access` and `port`; `ptyro web status`, `daemon
  status` and the doctor say "who can reach it: …" and how to change it. Searching the panel for
  `tailscale`, `wifi`, `lan`, `listen` or `bind` finds `remote.access`. Nothing opens to anyone
  until a device is paired.
- **Older files.** A `[remote]` with the old keys is read as what they meant
  (`config::migrate_remote`: `local` or a wildcard `listen` is `open`; `tailscale` or a
  non-loopback `listen` is `tailscale`; else `localhost`; `listen`'s port is `port`;
  `access`/`port` already in the file win), and `set_serve` rewrites the file without them. Two
  things went with them by design (owner, 2026-10-28: "https is always on, I don't want http
  ever (fine with the self signed cert issues)"): ptyro no longer runs `tailscale serve` for
  you (the name is `https://<name>.<tailnet>.ts.net:<port>` with ptyro's certificate, trusted
  once; `ptyro uninstall` still switches off a `tailscale serve` mapping to our port that an
  older ptyro made), and there is no plain-HTTP or trusted-proxy mode (so no `X-Forwarded-For`
  handling). The one exception is not a setting: `ptyro serve --loopback-http` (or
  `PTYRO_LOOPBACK_HTTP=1`, which the daemon's child inherits) speaks plain HTTP on 127.0.0.1
  for the tour and the tests (a browser counts loopback as secure); it refuses any `access`
  but `localhost`.
- **No orphans.** A managed `ptyro serve` leaves within a second of its parent daemon going
  (`getppid` changes), however it went: a crash leaves the socket file behind, so the file alone
  can't say, and the next daemon's server would never get the port (`m9`: the daemon is killed
  with SIGKILL and its replacement's server comes up). **[fixed 2026-11-07]** The parent it
  compares with is the daemon's own pid (`PTYRO_DAEMON_PID`), not the parent it saw first: a
  daemon gone before the server first looked had already left it to another parent, so the
  change never came. And the daemon starts no server once it is stopping (checked under the lock
  its stop's kill takes): a server started just after that kill outlived the daemon and held the
  port from the next one (`m9` on WSL; tested by
  `a_managed_server_whose_daemon_is_already_gone_leaves`). Nor while it hands over to a new
  image: the exec keeps the daemon's pid, so a server started then would never notice (a
  handover called off starts it again). On Windows the daemon names its start time too
  (`PTYRO_DAEMON_STARTED`), and a process with its id that started at another time is another
  one: the daemon is gone.
- **After a reboot.** The daemon runs it, so it is up when the daemon is; `ptyro web on` and
  `ptyro init` say when the login service isn't installed (`ptyro daemon install-service`).
- **Routes:** `/` and the embedded app; `/ws`; `POST /api/request` (`Authorization: Bearer`,
  one request, JSON outcome); `POST /api/pair` (`{code}` → `{token, device}`: a `DeviceClaim`
  over a local connection; a refused code counts as a failed login); `/healthz`.
- **Headers:** a strict CSP (`default-src 'none'`, `script-src 'self'`…), `X-Frame-Options:
  DENY`, `nosniff`, `no-referrer`.
- **The WebSocket:** one WebSocket message per protocol frame (JSON text or MessagePack binary).
  The first message must be a Hello. `serve` rewrites it to `kind: web`, `via: ws:<peer>`,
  `pid: 0`, and drops a requested zstd. A `Reject` counts as a failed login: 10 from one address in 10 min answer 429.
  Pings every 25 s. **[changed H2.7]** A browser's message is at most `MAX_CLIENT_MESSAGE`
  (4.25 MiB: a whole scratchpad and room to spare), set at the handshake because the library
  can't change it after login. The web app checks the same limit before it sends and says so in
  a toast. Messages to the browser stay up to `MAX_FRAME`.
- **Host check (DNS rebinding):** a request whose `Host` isn't an IP address, `localhost`
  (`*.localhost`), `public_url`'s host or this machine's MagicDNS name (full and short, from
  `tailscale status --json`) gets 421. No `Host` passes (not a browser).
- **Addresses:** `localhost` binds 127.0.0.1; `tailscale` binds 127.0.0.1 and each of this
  machine's Tailscale addresses (`tailscale status --json` `Self.TailscaleIPs`: 100.64.0.0/10,
  fd7a:115c:a1e0::/48), and looks again every 5 s, so Tailscale starting after the server (or
  its address changing) needs no restart; `open` binds 0.0.0.0. A port taken is the one error
  that matters, and says which setting changes it. Each listener runs the same accept loop.
- **HTTPS** **[built WEB1.7]** (`crates/ptyro/src/tls.rs`): always (see ACC1 for the loopback
  exception). The certificates
  live in `<data>/serve/tls/`:
  - a CA (ECDSA P-256, ten years, `ptyro local CA (<host>)`), made once; `ca.pem` never changes,
    since it's what phones installed (rcgen can't read a certificate back, so `tls.json` keeps
    its name, dates and serial, and the CA is remade from those and its key to sign with);
  - a server certificate it signs (397 days, under Apple's 825-day limit for user-installed
    roots; `serverAuth`), for `localhost`, the loopback addresses, the host name and
    `<host>.local`, the addresses traffic to a LAN and a tailnet leaves from (a connected UDP
    socket, nothing sent), the bound addresses, this machine's Tailscale addresses and names and
    `public_url`'s host. Re-issued
    with fewer than 30 days left or a name missing (names seen before stay), at start and every
    12 h while it runs, swapped in for new connections.

  **[built TS1] Tailscale's certificate.** With `access = tailscale`, no certificate of your own and
  automatically (no setting: owner, 2026-10-29), `serve` runs `tailscale cert --min-validity 15d` for the
  machine's own `*.ts.net` name when `tailscale status --json` is `Running` and lists it in
  `CertDomains` (the tailnet's "HTTPS Certificates" are on). It is a Let's Encrypt certificate
  (DNS-01 done by Tailscale's coordination server; the key is made on this machine), so every
  phone trusts it and Android needs no CA install. Files: `<data>/serve/tls/tailscale/<name>.crt|.key`
  (directory `0700`, key `0600`); a certificate on disk is used at once on the next start if it
  names the machine and has more than a day left (`load_tailnet`, which reads the end date). The
  fetch runs in the background so the port is ready first: `serve` starts with its own
  certificate and swaps a rebuilt `ServerConfig` in when Tailscale's arrives, then asks again every 12 h
  (a no-op until 14 days from its end), after a failure every 5 to 10 min. The TLS config picks the
  certificate by SNI (`ByName`): the tailnet name gets Tailscale's, every other name, address or no
  name gets ptyro's own (a Let's Encrypt certificate can't hold an IP address, and a CA-pinned
  `machine add` by IP still works). Failure never stops serving: ptyro's own stays, `serve.json`
  carries `cert` (`own`, `tailscale`, `ptyro`), `tailscale_name` and `cert_note` (the fix:
  turn on HTTPS Certificates at login.tailscale.com/admin/dns; or `tailscale set --operator=$USER`),
  and `ptyro web`, `ptyro doctor` and the web app's Web app page show them. `ptyro pair` hands out no
  CA when its address is the tailnet name with Tailscale's certificate. `machine add` accepts a
  `*.ts.net` host's certificate checked against the public roots (`webpki-roots`) when the pinned
  CA is not in its chain; no other host may skip the pin. It is automatic
  whenever `access = tailscale`, with no setting and no question (owner, 2026-10-29); the name goes
  into public certificate logs (Certificate Transparency), which is the price of a certificate
  nothing needs installing for. A person who minds picks another `access` or sets `tls_cert`.
  `--cert`/`--key` (or `[remote] tls_cert`/`tls_key`) use your own certificate instead: no CA,
  no `/ca.crt`, the files read again every 12 h. It is made (or read) before `serve` binds, so a
  listening port means ready. `GET /ca.crt` serves the CA
  (`application/x-x509-ca-cert`). `serve` records its URL and the CA's SHA-256 in
  `state/serve.json`; `ptyro pair` uses them while that `serve` runs, so the QR code is its
  `https://` address, and says where to get the CA and how to trust it. `ptyro doctor` (so the
  health badge) notes `open`, warns when `tailscale` is chosen and Tailscale is down, and warns
  on a certificate that wasn't renewed.
- **Devices:** kept in the store's `kv` (`devices`): id, name, scope, SHA-256 of the token,
  created/last seen/via, push subscriptions, and, until claimed, the pairing code's SHA-256 and
  expiry. Tokens are `ptd_` + 32 bytes from `/dev/urandom` (no fallback), checked in constant
  time against every record; an unclaimed record has no token and never authenticates.
- **Scopes:** `read` < `approve` < `control` < `full`, enforced in `handle_as`
  (`devices::needed`; unknown requests need `full`) and on `Input` (control). A device never
  takes control or resizes a session. Revocation bumps an epoch, and the device's live
  connections get `Goodbye{"this device was revoked"}`. Approve-or-higher actions on a session
  are recorded as `remote` events with the device's name.
  - **What `control` means (hardening review, 2026-10-24):** it may *steer* sessions: type into
    them (a shell included: typing into a shell is running commands as you), start configured
    agents, stop things. It is not "can't run commands"; only `full` reaches the machine itself
    (config, host operations). `Compare` (it runs a project's checks and may start a judge
    agent), answering `always`, and the change-set `push-*` and `land-*` gates need `control`;
    `read` can see screens, scrollback, recordings and diffs, so a lost `read` device is a
    secrets leak too. A gate answered from a device is recorded under the device's name.
  - **Agents calling the daemon (`Hook`, `StatusLine`, `Report`) speak for their own session
    only:** a caller the daemon places in session X can't report on or feed hooks for Y. Under a
    run policy the allow-list applies. Verified-done's `deny_cmds`, leases and fences are
    tripwires for an agent that is behaving, not a sandbox (`bash -c`, `sed -i`, links get past
    them).

### 16.4 Web client / PWA

A small TypeScript client served by `ptyro serve` (assets embedded with `rust-embed`): session list
with status, attention inbox, a terminal you type into (below) rendering `ScreenFrame`s to a canvas (no
xterm.js needed, because the daemon already emulates), quick-reply box (`SendText`), approve/deny
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
- **PWA:** a manifest, icons (rendered from `docs/logos/ptyro-icon.svg` by `make icons`, including a separate maskable one), and a service worker with an
  offline shell, push display, and Allow/Deny notification actions answered through
  `/api/request` with the token from IndexedDB. `serve` ends `/sw.js` with
  `// certificate <stamp>` (16 hex digits of a SHA-256 over the leaf certificates it serves,
  `tls::Served`), so a new or renewed certificate makes the browser install a new worker
  (§20.9).
- **Pairing:** `ptyro pair` prints a QR code (`qrcode`, Dense1x2) for `<base>/#pair=<code>`. The
  base is `--url`, `[remote] public_url`, or the address `access` makes reachable (below).
  - **Default scope is `full`, with a warning.** With no `--scope`, `ptyro pair` (and the TUI's
    `remote.pair`, which runs it, and the web's Devices ＋ form, which preselects `full`) pairs
    with `full`, because the whole web app needs it, and says so in plain words: "This device gets
    FULL control: it can run commands as you, change settings, and pair or revoke other devices.
    Pair only your own phone or browser. Use --scope approve … for less." (stderr before the QR
    code; the `warning` field of `--json`; a red paragraph in the web form and in the link sheet).
    A scope named on the command line is the owner's choice: no warning, just the one-line "This
    device may …". The protocol's `Scope` default is `full` too (a `device_pair` without a scope).
    Existing devices keep their scopes. No config option: a default that can be configured away
    is a default nobody reads.
  - **The port is always in the link, and the address follows `access`.** `localhost` is
    `https://127.0.0.1:<port>`; `tailscale` is `https://<MagicDNS name>:<port>` (the first
    Tailscale address if there is no name); `open` is `https://<LAN address>:<port>`. IPv6 gets
    its brackets. `--url` and `public_url` are used as written (a trailing `/` trimmed): they are
    the owner's word, for a forwarded port or a name of their own. `ptyro pair` uses the address
    of the running `ptyro serve` (`state/serve.json`), else the config. With `localhost` and no
    `--url`, it adds a note: "that address only works on this computer", and the setting that
    changes it. `--json` carries the same URL and `notes`. `remote::base_url` is the one function.
  - **A name is one device.** Pairing a name that exists (case-insensitive) replaces that device:
    the new link carries the newly asked scope (up or down), and when it is opened the old
    token stops working and the old device's live connections are closed (as a revoke). Until
    then the old device is untouched (an abandoned pairing cuts nobody off), and an earlier
    unused link for the same name is dead at once. The CLI says "Replacing the existing device
    'phone' (was approve, now full)"; `--json` has `replaces`; the web form says the same and
    names the replacement in the sheet. The daemon replies `Pairing{replaces}` / `Paired{replaced}`.
    A different name from the same browser is a different device (nothing links them).
  - **Why a code [security review M2]:** what is printed stays in scrollback and in the session
    recording (on by default, kept 30 days), which `read` devices and agents can read. So it is
    a pairing code (`ptp_` + 32 random bytes, stored as SHA-256), not the token. The app
    `POST`s it to `/api/pair` and gets the token once. The code is then cleared; unclaimed, it
    is pruned after 10 minutes (`PTYRO_TEST_PAIRING_TTL_MS` shortens that for tests, never
    lengthens it). Either way the printed text is worthless afterwards.
  - **Residual risk:** inside those 10 minutes the code is a credential; whoever claims it first
    gets the device. `ptyro pair` waits and prints who claimed it and from where (`devices`
    shows `unclaimed` until then), and in a ptyro pane it says the pane is recorded.
  - **`--open`** (WIN2) pairs this computer's own browser instead: the link opens with what the
    computer uses for addresses (`ptyro_core::open`: inside WSL `explorer.exe`, so the Windows
    browser; `open` on macOS; `xdg-open`), the name defaults to `browser`, and no QR code is
    printed. It refuses before making a code when no server runs (the fix: `ptyro web on`). Inside
    WSL, with ptyro's own CA, it asks PowerShell whether `Cert:\CurrentUser\Root` holds it (by
    the SHA-256 ptyro prints); if not, it offers to add it (`Import-Certificate`, no
    administrator; Windows shows its own confirmation), else the page opens on a certificate
    warning, which a note says. Firefox keeps its own store. TUI: `remote.pair.browser`; web:
    More → Web app → Open on the computer (the `pair.open` host op, `--json`, which can't ask,
    so it notes the warning). [V 2026-11-03: the trust check and reading the PEM on Windows
    11; the import's dialog not clicked through yet]

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

**[built WEB1] The web client is first class** (§1.3): everything the CLI does, it does, each
control a named action in an `ACTIONS` table (its tooltip) that `docs/parity.md` and `make parity`
check. It speaks the protocol over its WebSocket, and a few things a browser can't do itself
(files, subprocesses) over `POST /api/host`.
- **Structure** (`web/src`): `conn.ts` (the connection, `host()`), `kit.ts` (the DOM helper,
  sheets, menus, forms, caches that re-render when data arrives, polling), `data.ts` (projects,
  worktrees, workspaces, agents: loaded at connect, kept by domain events), `flow.ts` (starting
  work: the project's scripts to trust, a profile guard's question, the review queue's wait),
  one module per kind of work (`sessions`, `projects`, `review`, `runs`, `settings`), `qr.ts`
  (a QR encoder, so pairing another device needs no library), `color.ts`, `app.ts` (the router,
  the tab bar, the session page, push).
- **Navigation:** a bottom tab bar (Sessions, Projects, Runs, Sets, More) and hash routes
  (`#/s/<id>`, `#/wt/<id>/<tab>`, `#/run/<id>`, `#/set/<name>/contract`, …). Dialogs are sheets
  that slide up on a phone and close with the page they belong to.
- **Scope-aware:** `Welcome.scope` says what the device may do; the daemon refuses the rest and
  its message names the `ptyro pair --scope` that would work. Pages that change the machine say
  so and need `full`.
- **Host operations** (`crates/ptyro/src/host.rs`, `POST /api/host` with `Authorization: Bearer
  <token>`; the token must be a `full` device's, which `serve` checks by asking the daemon for
  the device list, and `serve` must not be `--remote`): `config.get|check|save|show` (checked as
  `ptyro config check` does, written 0600 atomically, the daemon told to reload), `doctor`, `about`,
  `profile.new|rm|rename|sync|detect|detect_apply|login|doctor`, `integrations.ls|install|
  uninstall`, `service.install|uninstall`, `daemon.logs|restart|stop`, `tour.start|end`,
  `login.list|start` (BOOT4, §24.4). The work
  is the CLI's own (`profiles.rs`, `doctor.rs`, `config.rs` expose it as functions; the rest runs
  the CLI as a subprocess), so the two cannot drift.
- **The web tour:** `tour.start` makes what `ptyro tour` makes (a throwaway directory with its own
  ptyro home, a demo repository, the pretend agent) and also starts a `ptyro serve` of its own
  there on a free loopback port, paired with a `full` device, and returns its link (with
  `&tour=1`). That page shows a coach (`settings.ts`) that follows the same six steps as the
  TUI's and moves on by what it sees; "End the tour" runs `tour.end` there, which stops that
  daemon, deletes the directory and exits. Stale tours are swept at the next start. A tour listens
  on this computer only, so it is opened from a browser on it.
- **Replay and scrollback:** `Replay` (above) feeds a canvas player (scrubber, 1–8×);
  `ReadScrollback` renders as coloured, searchable text.
- **Diff and review:** `WorktreeDiff` renders per file and hunk; tapping a line adds a
  `ReviewComment` for the agent; the comments batch into one prompt (`ReviewSend`).

**[built WEB1.8] A real terminal in the web.** The session page's screen is a terminal
(`web/src/term.ts`; the DOM-free parts are `keys.ts` and `grid.ts`, tested by `web/test/term.mjs`).
Decision (spike, §20.5): **our own**, extending the canvas drawer; no xterm.js, no second
emulator, no WASM.
- **Typing.** Tap the screen (or click it) and every key goes to the session as it is pressed, as
  an abstract `KeyEvent` (§5.5) in `ClientMsg::Input`; the daemon encodes it against the
  program's own modes (DECCKM, kitty flags), which only it knows. A hidden `<textarea>` takes the
  keys, so soft keyboards and IME composition work (`beforeinput` and composition events carry
  what a phone's keyboard says; a keydown of 229 or `Unidentified` is left to them), hardware
  keyboards get Ctrl/Alt/Shift combinations, F-keys, Option-as-Alt on a Mac (the physical key
  decides), and Cmd stays the browser's (copy, paste). Paste is `Input::Paste` (bracketed by the
  daemon when the program asked); focus in and out is `Input::Focus` when it asked for that.
  Needs `control` scope and a running session; below it the page is a viewer (and says so).
- **The key row.** Esc, Tab, sticky Ctrl, Alt and Shift (tap: the next key; tap again: held),
  arrows that repeat while held, Home/End/PgUp/PgDn/Del, Ctrl-C/D/Z/L/R/A/E/W, the punctuation
  a phone keyboard buries, F1–F12, Paste, and Aa (the options). Buttons don't take focus from the
  terminal, so the keyboard stays up. The reply box (a whole line, dictation-friendly) stays for
  when the terminal isn't being typed into.
- **Mouse.** When the program turned the mouse on (`ScreenModes.mouse`), clicks, drags and the
  wheel go to it as `Input::Mouse` with cell coordinates (hold Shift for the browser's own
  selection); otherwise a drag selects. A full-screen program without mouse gets the wheel as
  arrow keys, as terminals do.
- **Scrollback.** Wheel, drag or flick (with momentum) scrolls back: the first scroll fetches
  `ReadScrollback` (5000 lines) and the window moves over that frozen document; a "↓ Live" button
  (and any key typed) returns, and says when output came meanwhile. Shift+PgUp/PgDn page.
- **Selection.** Mouse: drag, double-click a word, triple-click a line. Touch: long-press selects
  the word, drag extends (the edges scroll the history). Selecting freezes the document so output
  doesn't move it; the bar offers Copy, All, Paste, Open (a link), and ↑ ↓ after a find. Copy
  text joins wrapped lines (`RowUpdate.wrapped`) and trims trailing blanks, wide characters count
  once. Copy tries the synchronous `execCommand` inside the tap first, then the clipboard API
  with a timeout (a permission prompt nobody sees would hang it). A program's OSC 52 copy
  (`DomainEvent::Clipboard`, sent to the controller) is written the same way, or offered behind a
  Copy button when the browser wants a tap. Find (`Aa`, or the context menu) searches the whole
  document; Ctrl/Cmd-click opens a link; the bell flashes the frame and buzzes.
- **Size.** Fit (the whole screen shrunk to the width, 5–16 px) or 1:1 at a text size (6–32 px)
  with a scroll both ways that follows the cursor. The default is automatic: Fit while that is
  readable (9 px or more, `READABLE_PX`), else 1:1, so a 200-column session on a phone opens
  readable and the same session on a laptop opens whole; rotating or resizing re-decides. Zoom is
  on this device only (`localStorage` `ptyro.term.zoom` = `auto|fit|size`, `ptyro.term.px`; an
  older `ptyro.term.fit` is read as fit or size) and never changes the session, unless "Resize the
  session to this screen" is on. Ways to zoom: a pinch (two pointers on the box; continuous, the
  cell under the fingers stays under them, moving both pans, pinching in past the fitted size is
  Fit, except for a session so narrow it fits at 16 px, which can still shrink), a two-finger tap
  (Fit ⇄ 1:1), Ctrl+wheel (also a trackpad pinch in Chrome; `scale = exp(-deltaY/100)`, steps
  capped at 10 %), Safari's gesture events (a Mac trackpad), and Aa → Bigger/Smaller text, Fit or
  full size, Size it automatically. A size badge shows for a moment after each change. The box
  takes `touch-action: none` and does its own panning (sideways; up and down moves the box first,
  then the history — coming back from the history, it first), with momentum, so a pinch never
  zooms the page. After a pan or a zoom the cursor isn't followed until a key or a tap. Once per
  device, on a touch screen, a toast says how (the first time the screen is wider than the box,
  or fitted below 9 px). "Resize the session to this screen": the page computes columns and rows
  from its box above the soft keyboard (`visualViewport`) and sends them (§5.8), restoring the old
  size afterwards.
- **Drawing.** Only the rows whose cells changed are painted (a row's cell array is replaced by
  the frame that changes it, so identity says), in a `requestAnimationFrame`; the frame is acked
  after it is drawn, so a slow phone gets fewer, bigger deltas, and a hidden tab acks at once.
  `localStorage["ptyro.debug"]="1"` exposes `ptyroTerm.stats` (paints, rows drawn, ms).

### 16.4a Which machine (the page title, the TUI)

The daemon reports its machine's short name (`DaemonStatus.machine`, `Welcome.machine`;
`ptyro_core::machine`): `[daemon] machine_name` if set, else on macOS the ComputerName if it is
short and plain, else the host name up to its first dot (`.local`, `.lan`, domains dropped; an
IPv4-looking name stays whole), printable only, 24 characters at most, `computer` if nothing is
known. A client of a remote daemon (`--remote`, a page served by another computer) shows the
daemon's. Surfaces: the web page title `studio · ptyro` (`(2) studio · ptyro` with agents waiting),
a name chip and a thin top bar in a colour hashed from the name (`hue`: FNV-1a over the lowercase
name, mod 360, the same in Rust and TypeScript) on every header, a favicon with a dot in that
colour, the installed app's title (`apple-mobile-web-app-title`), More → This device → Machine;
the TUI's sidebar title (`WORKSPACE main ● studio`, dropped when it doesn't fit) and the
terminal's window title `ptyro · studio` (OSC 0, with xterm's title stack saved and restored on
exit); `ptyro daemon status` (`machine`), `ptyro pair` ("Pairing 'phone' with studio"). The
manifest is static and served before sign-in, so it keeps the name `ptyro` (the name is not given
to unauthenticated requests). The name is read at connect: a changed `machine_name` shows after
the next reconnect.

### 16.4b Many machines: workspaces on other machines **[decided 2026-10-27 (owner, ROADMAP MM1.0); built MM1.B]**

One sidebar, one phone app, every laptop's agents. A daemon **links** to the daemons of other
machines and **mirrors** their workspaces next to its own; every client of it (TUI, CLI, `serve`
and so the web app) sees one daemon. RESEARCH R-REM-13; the alternative (R-REM-12, the PWA
holding several machines) was not chosen.

**Decided by the owner:**
- **Symmetric.** Each machine links the others; the phone is paired with each. Whichever is awake
  shows everything it can reach. No hub.
- **Mirror.** A remote workspace lives on its machine with its projects, worktrees, profiles and
  logins; that machine is the authority. The local daemon shows it and forwards what is done in it.
- **A machine is added, not a workspace.** All its workspaces are followed (new ones appear,
  removed ones go); any can be hidden here (`machine hide laptop2 work`), which changes nothing
  there. A daemon offers its links only its **own** workspaces, never ones it mirrors, so two
  machines linking each other never echo.
- **Transport: both.** A paired token over Tailscale (`wss://`, the phone's pairing, a `machine`
  device with `full` scope) or `ssh://` (`ptyro proxy`, §16.2). Adding a machine asks which and
  checks the link before saving it.
- **Full scope** between linked machines; revoking the token (or removing the machine) cuts it.
- **Same version.** A link to a daemon of another version stays down and says what to do and
  where: `laptop2 runs ptyro 0.0.11, this is 0.0.12: run \`ptyro daemon upgrade\` on laptop2 (after
  updating ptyro there)` (the older side is named); the TUI and web offer it as the fix, a toast
  when the link is tried and a line on the machine's header.
- **Offline:** the workspaces of an unreachable machine stay listed, greyed, read-only, with the
  last sessions and screens known and "offline since 14:02"; anything that would change them is
  refused with that message; the link retries (backoff to 60 s) and comes back by itself.
- **Addresses:** `laptop2:fix-flaky-test` (sessions), `laptop2:myrepo/feat` (worktrees),
  `laptop2:work` (workspaces), in the CLI, the palette and completions. A local name needs no
  prefix; a bare name that only a remote has resolves to it, an ambiguous one is refused with
  both qualified names.
- **Alerts come through the daemon you are connected to:** a mirror's attention events are the
  local daemon's notifications (desktop, sinks, web push). A phone paired with both machines may
  hear the same alert twice; both use the same notification `tag` (`<session id>:<since_ms>`), so
  the phone shows one.
- **New workspace asks which machine** (local first). It is created there.
- **The sidebar shows every workspace stacked** (local first, then each machine's), each a
  header with its machine chip in the machine's colour (§16.4a): `laptop2 / work [profile]`, then
  project → worktree → session. **One pane layout for everything**: panes mix workspaces and
  machines; each pane header names workspace, machine and profile. "The current workspace" (for
  `n`, `C-n`, `N`, the status bar's profile, attention scope) is the selected row's, else the
  focused pane's. `w` jumps to a workspace's header (grouped by machine, offline greyed), unfolding
  it.

**[built MM1.B]** What was built (`crates/ptyro-daemon/src/machines.rs`, `conn.rs`,
`crates/ptyro/src/machines.rs`, `crates/ptyro-tui/src/tree.rs`, `web/src/machines.ts`):
- **Machines** are kept in `kv` (`machines`: name, transport, added_ms, hidden); a token is a file
  of its own, `data/machines/<name>.token` (0600, in a 0700 directory), never in the database or a
  reply. `ptyro machine add|ls|rm|hide|unhide|retry|pair`; `MachineList`, `MachineAdd{name?,
  transport, token?, force}` (it dials first: unreachable is refused unless `force`; a version that
  differs is kept and shown as such), `MachineRemove`, `MachineHide{name, workspace, hidden}`,
  `MachineRetry`; `MachineUpdated`/`MachineRemoved` events. Its name defaults to what it calls
  itself; this machine's own name is refused.
- **The link** is always a child process whose stdin and stdout carry the protocol, so the daemon
  needs no WebSocket or TLS of its own: `ssh host ptyro proxy` (§16.2, `PTYRO_SSH`) or `ptyro
  machine-bridge --url … [--fingerprint …]` (hidden), which speaks to the other machine's `ptyro
  serve` (`/ws`, one message per frame) over its HTTPS, accepting a server only if its chain holds
  the certificate authority with the paired fingerprint and that authority signed it for the name
  connected to (`ws://` is plain, for Tailscale only). Hello kind `Machine`; over a token, the
  device's `machine` flag (`DevicePair{machine: true}`, `ptyro machine pair`) marks it. The code
  `machine pair` prints is a link: `https://HOST:PORT/#machine=ptp_…&ca=FP&name=laptop2`;
  `machine add CODE` claims it once (`POST /api/pair`, the same pin) for the token.
- **Same version** is checked against `Welcome.daemon_version`; a mismatch leaves the link down
  (retried every 5 minutes, or on `retry`) with `error` and `fix` naming the older side.
- **The mirror** holds what the link heard, qualified on arrival: a JSON walk over replies and
  events gives sessions, workspaces, projects, worktrees, profiles, inbox entries and usage the
  `machine` field and their names the prefix (sessions: `name`, `workspace`, `profile`; workspaces:
  `name`, `profile`; projects: `slug`, `pin`; worktrees: `project_slug`, `profile`; profiles:
  `name`; inbox: `session_name`, `workspace`, `profile`; usage: `profile`). Ids stay as they are
  (unique everywhere). A client id there (a controller) becomes ours when it is our client's
  upstream, else one with the top bit set that matches no client here. The mirror is refreshed
  whole on (re)connect and on `EventsLost`; what went is announced as removed. Its events go out on
  a channel of their own (`Daemon.mirrored`), heard by clients only, never by this daemon's own
  machinery. A link pings with `Status` every 20 s and counts 15 s of silence as gone (a laptop
  that sleeps never closes its end).
- **Routing** (`handle_as`): unless the caller is a link, a request whose addresses (`addr`,
  `name`, `workspace`, `project`, `worktree`, `session`, `target`, and those in its `spec`) carry
  `laptop2:` or the id of something of laptop2's is checked against this daemon's scopes, sent over
  the link with the prefix taken off its addresses only (never off text), and its reply qualified.
  A list (`List`, `WorkspaceList`, `ProjectList`, `WorktreeList` of all, `ProfileList`, `Usage`,
  `Inbox`) gains every mirror's shown part. A request this machine answers `not found` is retried
  on the one mirror that has that bare name; two that have it: refused, naming both.
  A request naming a workspace of one machine and a worktree or project (by id) of another is
  refused (`…is on this machine, but the workspace is on laptop2: use a workspace here`), never
  sent to the wrong one. The TUI avoids it: a spawn, a profile check or a new worktree goes to
  the workspace that lists the worktree's project, whichever machine that is, not to the
  selected row's. Requests
  about this machine itself are never routed (devices, push, the scratchpad, views, hooks and
  status lines, runs, recipes, fan-outs, gates, compare, mail and leases, change sets, profiles'
  own operations, doctor, the web server, `Agents`, `Editors`, machines).
- **Screens and keys** (`conn.rs`): a client's `Subscribe`, `Unsubscribe`, `Ack`, `Input`,
  `Resize` and `Focus` for a mirrored session go over an upstream connection of that client's own
  to the machine (made on first use), so frames, acks, control and sizes work as they do here; it
  passes back only `Screen`, `Chat` and its own `Clipboard`. A device below `control` watches
  without typing or sizing, as here. Offline, these are dropped.
- **Alerts:** an inbox entry from a mirror is delivered by this daemon's channels (`deliver`:
  terminal, desktop, sinks, web push) under this machine's `notifications.on` and quiet hours; push
  tags are `ptyro-<session id>` on every machine, so a phone paired with both shows one.
- **The TUI** stacks every workspace (§11.2j); the web app labels workspaces and projects with
  their machine (`laptop2 / work`), gives other machines' sessions a chip in the machine's colour,
  asks which machine in the new-workspace form, and has More → Machines.
- **Deviations from the decision:** an offline machine's *sessions* stay listed as they were,
  but its screens are only what a client already had (none is kept by the daemon); marking a
  mirrored inbox entry read isn't routed (it is read-only here); the wizard offers this machine's
  agents and editors in a remote workspace too.

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
| Files only you should read (the config, shell integration) | mode `0600`/`0700`; **[built WIN7]** on Windows the access list (`ptyro_pty::acl`): one wider than the user, SYSTEM and Administrators is a `doctor` finding with its fix, a config save narrows it, and the daemon narrows a shell-integration folder others could change |
| Socket hijack by other local users | dir `0700`, socket `0600`, peer-uid check on accept. **[built WIN5]** Windows: a named pipe whose every instance admits only the user's SID (`D:P(A;;GA;;;<sid>)`) and rejects remote clients; pipe names are one namespace per computer, so the client refuses a pipe whose server process runs as anyone else (`ptyro_pty::pipe`) |
| Cloned repo runs arbitrary code | scripts/recipes require fingerprinted trust; agent executables only from user config |
| Code ptyro runs for a repository: scripts, `after_turn`, verify checks, git (security review M3) | trust pins a script's *text*, not the files it calls, and agents edit those files: trusting `cargo test` trusts the tests. Scripts, `after_turn` and checks run as the user, with the daemon's environment and network, outside any agent sandbox; this is documented, not sandboxed (§10.5, §15.6.5, docs/config.md). Background git runs no repository code: `core.fsmonitor=false` always, `core.hooksPath=/dev/null` except for the operations the user asked for (commit, push, merge, rebase, cherry-pick, a new worktree), diffs with `--no-ext-diff --no-textconv` (§10.9) |
| Session content sent to a model provider by summaries (UX1.3) | off by default; on, only a trimmed view goes (§11.2h), through the session's own profile, so its own account and provider. The profile's secret values and anything shaped like a credential are replaced before sending; the summariser runs with no tools, no MCP servers and no project directory |
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
| Remote access | **[built M9]** off by default; the daemon's clients use a local socket only (its one TCP port is the opt-in approve/deny link listener, loopback/Tailscale unless `insecure`). `ptyro serve` is always HTTPS and listens where `[remote] access` says (this computer, plus your Tailscale network, or every interface). Devices: 256-bit tokens stored hashed, compared in constant time, scopes enforced per request and keystroke, a browser's Hello rewritten to `web`, a lockout after 10 failures, revocation closes live connections, pairing only locally and through a one-time code (single use, 10 minutes, so the printed link is worthless afterwards; the token itself is never printed), `full` by default with a warning (`--scope` for less), remote actions in the event log. The app ships a strict CSP and no third-party code. Web Push is end-to-end encrypted. Over ssh: the ssh login. E2E relay later (docs/remote.md has the threat model). |
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

**The fake agent behaves as the real ones do where ptyro looks [2026-11-07, the flakes task]:**
- As Claude does, the fake Claude empties its input box when it takes a prompt (the prompt
  moves up into the conversation), and draws what it took itself: its terminal doesn't echo,
  as Claude's raw one doesn't, so a line typed ahead while it works can't land where the box is
  redrawn. It used to leave the prompt in the box, so ptyro's WIN8 check ("this very prompt
  still sits there unsent", §8.2) saw it unsent while a slow hook still left the status idle,
  and pressed Enter again; the permission dialog that came next took that Enter as "no". This
  was the `send --wait` flake of ROADMAP WIN13 (m5 on macOS: "a1 isn't waiting on a prompt",
  "a1 matched answer: no"). The product's guard against it is in §8.2.
- So the fake Claude never shows a typed-but-unsent draft: a dropped Enter on Claude's box
  isn't modelled (Codex's is, `PTYRO_FAKE_DROP_FIRST_ENTER`). A test that needs one would draw
  the draft from a raw-mode reader.
- `PTYRO_FAKE_LAGGY_BOX=1` is a Claude whose redraw lags: a prompt it took stays in its box,
  and `ask` shows its permission dialog at once, before its hooks.
- `PTYRO_FAKE_HOOK_DELAY_MS=N` makes each of its hooks take N ms to start, as on a loaded
  machine, so what ptyro does before a hook arrives is tested on every machine, not only on a
  slow CI runner (m5 `a_prompt_taken_with_slow_hooks_is_entered_once`, and the m4 tests above).
  `PTYRO_FAKE_START_GATE=<path>` keeps it loading, with output, until the file exists: m4
  `a_slow_session_start_replaces_a_guessed_working` waits for the heuristic's guess before it
  lets the first hook go. These, `PTYRO_FAKE_LAGGY_BOX` and `PTYRO_FAKE_PLAIN_DIALOG` are test
  knobs (`ptyro_core::test_knob`: a release build ignores them).
- A test that asserts which source a status came from first waits for the agent's first
  reliable word: before it, the output heuristic may guess "working" from start-up output (§9.3).

CI: GitHub Actions on Linux + macOS runs `make check` (fmt-check, clippy `-D warnings`, tests,
spike smoke) plus cargo-deny (`deny.toml`: licenses, advisories, bans, sources); release artifacts
are built on tags later. CI only ever calls make targets.

**[built WIN13]**
- **The flake budget.** `make test` runs every test binary through `ptyro-flaky` (a binary in
  `ptyro-testkit`): it builds the tests (`cargo test --no-run`, cargo's JSON names each binary
  and its package), runs each in its package's folder as cargo would, and runs any test that
  failed again, once, on its own (`--exact`). A test that passes then is a flake: named in the
  output, in `target/flakes.txt` and in the CI job's summary (`GITHUB_STEP_SUMMARY`). The run
  fails when a test fails twice, a binary fails without naming a test twice, doc tests fail
  (run last, once), or flakes exceed `FLAKE_BUDGET` (2). Retries can't hide a flaky suite, and
  a flake is never silent (owner, 2026-11-06: retry once, capped).
- **Windows.** CI's `windows` job runs `make test` on `windows-latest` (Git Bash; `make` from
  Chocolatey): every crate's tests natively, native daemons in ConPTY with the packaged ConPTY
  beside the test binaries (`PTYRO_TEST_PACKAGED_CONPTY=1`), two tests at a time per binary
  (`RUST_TEST_THREADS=2`, also on macOS's three-core runners: m7's nine at once took 144 s
  on Windows and 102 s on macOS and missed their windows, each alone 14 s) and scripted TUI runs at half speed (`PTYRO_TEST_TIME_SCALE=2`). A test that can't run there is
  `#[cfg(unix)]` with its reason on the line above (a Unix mode, a signal, a symbolic link
  without Developer Mode, a POSIX shell's job control, a stand-in that is a shell script by
  nature). Stand-in programs on `PATH` (`gh`, `claude`, `ssh`, `tailscale`, an editor) go
  through `ptyro_testkit::program`: a `#!/bin/sh` script on Unix; on Windows, where a script
  can't run by name and `Command` finds only `.exe` files, the debug `ptyro.exe` hard-linked
  under the program's name with what to run beside it in `<name>.shim` (JSON `argv` and `env`;
  `say:` prints and waits). A debug `ptyro.exe` started under another name with a shim runs it
  first thing; a release build never looks. `make check-windows` cross-checks every crate's
  tests from Linux.

**[built M8]**
- **Fuzzing.** Eleven targets live in `ptyro-testkit::fuzz`:
  - `sniff`: the same findings however the reads are split;
  - `emulator`: random bytes, resizes and image round trips;
  - `protocol`: the decoder, with or without zstd;
  - `input`: encoding never lets a paste close bracketed-paste mode;
  - `recipe`: the parser and validator;
  - `record`: torn and garbled recordings;
  - `settings` (H2.4): the settings editor's `set`/`unset`/`add`/`remove`/`rename`/`check` on arbitrary
    files, keys and values: never a panic, and what it saves still checks;
  - `imports`: the seven importers of other tools' files (YAML, JSON, TOML), strip and quote
    helpers, on hostile nesting and aliases;
  - `hostjson`: arbitrary JSON as `Request`, `Reply`, `ClientMsg` and `ServerMsg` and the host
    API's parsing helpers;
  - `handoff`: arbitrary bytes as a live-upgrade image and as one session's compressed emulator;
  - `parsers`: every config and project parser, `names`, and the scratchpad's merge, stamp and tick.

  They're shared by two front ends. `ptyro-fuzz`, a stable-Rust mutation runner with seeds,
  replay and saved crashes, runs 20 s per target in CI (`make fuzz-smoke`, about four minutes). `fuzz/` is a
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
- **Releases (H2.2).** `.github/workflows/release.yml` runs on `v*` tags and publishes nothing
  unless, for the tagged commit: the tag is an ancestor of `origin/master` and equals the crate
  version (`gate`); `make check` passes on x86_64 Linux, aarch64 Linux and aarch64 macOS, and
  `make test-windows` on Windows (`check`); `make tools deny web-check msrv fuzz-smoke` pass
  (`supply`); and each of the four tarballs (`make changelog dist`, x86_64/aarch64 Linux,
  aarch64/x86_64 macOS) installs through `scripts/install.sh` into a temporary prefix, runs, and
  is refused when tampered with (`make dist-check`, `scripts/installer-check.sh`), then is removed
  by `install.sh --uninstall` (fed on stdin, as `curl … | sh -s -- --uninstall` would): a
  `--dry-run` keeps it; otherwise it hands over to `ptyro uninstall` with the remaining arguments
  (from `/dev/tty` when stdin is the script, so its question can be answered), and a binary that
  won't run is removed by the script itself, with its completions. `install.ps1 -Uninstall` (or
  `PTYRO_UNINSTALL=1`, since `irm | iex` passes no arguments) does the same with `ptyro.exe`, and
  never calls `exit`, which would close an `iex` caller's PowerShell. Both installers are release
  assets (in `SHA256SUMS` and the provenance), so the documented URL is
  `https://github.com/nealhardesty/ptyro/releases/latest/download/install.sh` (`.ps1`), the
  installer of the release it installs. **[built WIN3]**
  So do the two Windows zips (`ptyro-<v>-{x86_64,aarch64}-pc-windows-msvc.zip`: `ptyro.exe`,
  PowerShell completions, README, LICENSE, CHANGELOG), built natively on `windows-latest` and
  `windows-11-arm` (rustup installed there by `scripts/rust-install.sh` when the image lacks it),
  through `scripts/install.ps1` (`irm …/install.ps1 | iex`; Windows PowerShell 5.1 and 7: the zip
  for the machine's own architecture, checked against `SHA256SUMS` and, with `gh`, its provenance,
  into `%LOCALAPPDATA%\Programs\ptyro\bin`, which goes on the user's `PATH`; the same
  `PTYRO_VERSION`, `PTYRO_PREFIX`, `PTYRO_RELEASE_URL`, `PTYRO_VERIFY` and
  `PTYRO_REQUIRE_ATTESTATION` as install.sh, plus `PTYRO_NO_MODIFY_PATH`). `ptyro.exe` isn't signed
  (WIN12; Smart App Control, §20.12). Run by hand with `dry_run`, the workflow rehearses all of it on
  any branch and publishes nothing (`gate`'s tag checks and `publish` are skipped). Only then `publish` (the one job with a write token,
  in the `release` environment) writes `SHA256SUMS`, the bill of materials (`make sbom`,
  `scripts/sbom.py`: CycloneDX 1.5 from `Cargo.lock`, deterministic), release notes from
  `scripts/changelog.sh --notes` and the formula from `make homebrew-formula`; signs build
  provenance for the tarballs, the SBOM and `SHA256SUMS` (`actions/attest-build-provenance`,
  verified with `gh attestation verify`); creates the release; and pushes to the tap when
  `HOMEBREW_TAP_TOKEN` is set. The tap is the public repo `nealhardesty/homebrew-ptyro`: a README and
  `Formula/ptyro.rb`, which each release overwrites with the formula rendered from the four
  tarball checksums (users: `brew install nealhardesty/ptyro/ptyro`). Set-up (owner, once): create
  that repo, create a fine-grained token with contents:write on that repo only, and store it as the
  secret `HOMEBREW_TAP_TOKEN` on the `release` environment, restricted to `v*` tags. The formula
  (`packaging/homebrew/ptyro.rb.in`) installs the prebuilt binary, generates the bash/zsh/fish
  completions from it, `uses_from_macos "git"`, has a GitHub `livecheck`, and its caveats say to
  `ptyro daemon upgrade` after `brew upgrade` and that `gh` is optional. `make brew-check`
  (`scripts/brew-check.sh`; the macOS build jobs and CI's macOS installer job run it with
  `PTYRO_REQUIRE_BREW=1`) renders it for the host platform (`scripts/homebrew.sh` with `BASE_URL`
  and `ONLY`) against the tarball in `dist/`, installs it from a throwaway local tap, and checks
  `brew test`, `brew audit --strict`, the version, the three completions and the caveats; a wrong
  checksum must be refused. It refuses to run if a Homebrew `ptyro` is already installed, and
  undoes everything on exit. Not covered: `brew audit --online` and installing from the real tap,
  which need a published release. A Homebrew install is recognised by its path: `ptyro
  uninstall` names `brew uninstall ptyro`. macOS binaries are not notarized (no Apple developer account in
  the pipeline): a browser download is quarantined and needs `xattr -d com.apple.quarantine`;
  `scripts/install.sh` downloads with curl, which leaves no mark. `make bump V=patch|minor|major|X.Y.Z`
  (`scripts/bump.sh`) sets the version, the first `version =` in the root `Cargo.toml`, and the
  workspace entries in `Cargo.lock`; it refuses a version whose tag exists and never commits or tags.
- **CI (`ci.yml`)** runs the same make targets on every push: `check` on the three platforms above
  (with Node installed and `PTYRO_REQUIRE_NODE=1`, so the web app's logic tests can't be skipped),
  `msrv` (the oldest compiler `rust-version` claims), `fuzz-smoke`, `web`, `deny` (`make tools
  deny`) and `installer`. `make check` is `fmt-check lint doc lock-check ratchet test smoke`:
  rustdoc warnings and an out-of-date `Cargo.lock` fail, and so does a crate gaining
  `unwrap()`/`expect()` in non-test code beyond `scripts/unwrap-baseline.txt` (`make ratchet`;
  the count only goes down).
- **Locks that survive panics (H2.1).** `ptyro_core::sync` (`MutexExt::locked`,
  `RwLockExt::reading/writing`) take a `std` lock whether or not another thread panicked holding
  it; the daemon uses them everywhere, so one buggy handler no longer poisons a session's state
  into failing every later request. The data may be half-updated (what the panicking code was
  doing); the daemon keeps serving the rest. A request that panics still gets an error reply.
- **What is tested, beyond the unit tests (H2.4).** `needed()` in the daemon's `devices.rs` is an
  exhaustive `match` on `Request`: a new request doesn't compile until someone decides its scope;
  `crates/ptyro/tests/scopes.rs` keeps a table of every request's scope (checked against the
  protocol's enum both ways) and proves it with real devices of each scope. The store is tested
  against a database from every earlier schema version, refuses one from a newer ptyro with a
  message, and backs an older one up before migrating (`<db>.bak-v<N>`, two kept). The
  live-upgrade image has a committed fixture (`PTYRO_UPDATE_FIXTURES=1` regenerates it, on
  purpose), and an upgrade begun during a slow launch waits for it (`tests/upgrade_race.rs`).
  Notification sinks that fail or hang don't hold up status and leak no secret into a log
  (`tests/sinks.rs`); `land` waits for CI to exist and merges only what it checked
  (`tests/land.rs`); gates are answered once (`tests/gates.rs`). The web app runs in a real
  headless Chrome driven from Node over the DevTools protocol, with no packages
  (`make web-e2e`, `web/test/e2e.mjs`, `tests/web_e2e.rs`; CI requires it).
- **Docs are tested against the code.** `make parity` (every command has a row in
  `docs/parity.md`), and tests that every command has a line in `docs/commands.md`, every `make`
  target is in the README, and every default key is in `docs/keys.md` fail the build when a doc
  falls behind.

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
| 19 | **A chat is ptyro's own durable record; the agent's conversation is a replaceable engine** (CHAT) | one truth for history, search, forks, switching agent and the machine mirror; agents' own histories differ, change format and vanish; native resume and fork stay as optimisations | indexing the agents' own history files (four unstable formats, no fork, no switch); a chat as a session kind (the transcript dies with the session, no tree) |
| 20 | **Chat text lives in its own `chat.db`** (CHAT) | sensitive and large; FTS churn and long writes must not touch the metadata store's thread; backup, retention, delete and uninstall treat it on its own (`--no-chats`) | new tables in `ptyro.db` (every backup and `VACUUM INTO` copies chats; contention); a JSONL file per chat (no search, no tree queries) |
| 21 | **Seed-replay is the contract; native resume and fork are optimisations** (CHAT) | works for every agent, an agent switch, a fork from the middle, a lost conversation; the cost is tokens, shown once | requiring native fork (only Codex forks at any turn; Claude only from the end); making switching agents impossible |
| 22 | **A chat belongs to a workspace on the machine that owns it; machines mirror, they don't sync** (CHAT, MM1) | chats then appear in every machine's workspaces with the machinery MM1 built (authority, routing, offline); one writer means no conflicts | replicating chats to every machine now (sync is CHAT6, needs its own design; the schema is ready for it) |

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
- **[verified 2026-10-22: 2.1.308 binary schema; live in 2.1.309 by `make verify-claude`]** `Stop`
  (and `StopFailure`, `SubagentStop`) carry an optional `last_assistant_message`: the text of the
  turn's last reply (session event `reply`). In the live run `Stop` came ~10 ms *before* 7501 `done`. ptyro maps it to
  `Signal::Reply`, the summary of a `worker_done`, a run's `last_message` and the review queue.
  OSC 7501 `done` can arrive before the `Stop` hook, so the lead's notice waits up to 1 s for it.
- **[verified 2026-10-23, 2.1.309; `make verify-claude`]** `claude -p --model haiku
  --output-format json --strict-mcp-config --tools ""`, with a ~400-character prompt on stdin,
  prints one JSON object with `is_error`, `result` (the text) and `total_cost_usd`. The alias
  resolved to `claude-haiku-5-5`. One summary cost $0.00065 (3.1k cache-creation tokens, most of
  them Claude Code's own system prompt) and took 1.1 s (`duration_ms`), or 2.4 s wall clock with
  startup. This is the default `[summaries] command` (§11.2h).
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
- **[verified 2026-10-23, 2.1.309, through ptyro]** In the prompt box a new line comes from
  `Ctrl+J` (LF), kitty `Shift+Enter` (`CSI 13;2u`), `Alt+Enter` (`ESC CR`) and `\` then Enter;
  a bare CR submits. Codex 0.164 takes `Ctrl+J`, kitty `Shift+Enter` and `Alt+Enter` the same
  way. User guidance: docs/keys.md "Several lines in one prompt".
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
  client, input-messages, last-assistant-message}` [verified 2026-10-23, 0.164.0], also for
  side threads (§8.4). https://learn.chatgpt.com/docs/hooks
- **[verified 2026-10-23, 0.164.0]** `codex app-server` `hooks/list {cwds}` returns every hook
  with `key`, `currentHash`, `trustStatus` (`managed|untrusted|trusted|modified`), `timeoutSec`
  and `source` (`user`, `sessionFlags`, …), with no login. `Interrupt`'s timeout defaults to 1
  and is clamped to at most 3. Codex 0.164 also emits OSC 7501 status reports [observed], which
  ptyro's generic OSC 7501 reader already takes as status.
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

- **[spike WEB1.8, 2026-10-24] Web terminal: consume or build?** Verdict: **build, on the
  daemon's frames**; no xterm.js (MIT; wants a raw byte stream that the protocol doesn't send the
  web, so it would be a second emulator to keep in step with the daemon's, and ~300 KB of
  third-party code in an app whose CSP is `script-src 'self'` and which has no dependencies), no
  libghostty-vt WASM (a second emulator again, Zig in the build, no stable API). What a viewer
  lacked was never parsing: it was input, scrollback, selection and a draw loop that paints less.
  *Measured here* (Chrome on macOS, a 186×42 session, loopback; not on a phone): key to the echo
  frame at the browser's socket p50 2.5 ms, p90 3.3 ms, max 10.6 ms (60 keys, `ptyro serve`
  included); a paint of the changed rows ~1 ms on average, 2.4 ms worst, while `seq 1 4000`
  scrolled by. A phone core is several times slower, which still leaves a frame's budget.
  *Not measured:* xterm.js beside it (it can't be installed from here), a real phone, battery.
  The numbers come from `ptyroTerm.stats` and a ten-line WebSocket script; a phone owner can read
  the first from the console.

- **[verified crates.io 2026-10-19]** versions in §3.5. `bincode` 3.0.0 is a tombstone release
  (RUSTSEC-2025-0141). `wezterm-term` is not published on crates.io.
- **[verified 2026-10-20, terminput 0.5.15]** Two gaps we must cover:
  - terminput's `KittyFlags` bits are shifted one place from the kitty spec
    (`DISAMBIGUATE_ESCAPE_CODES = 1<<1`). Map flags from alacritty's `TermMode` *by name*, never
    by bits.
  - Its xterm encoder ignores DECCKM. In application-cursor mode, unmodified arrows/Home/End must
    become `ESC O x`. `ptyro-term` patches this, unit-tested in `crates/ptyro-term/src/input.rs`.
- **[verified 2026-10-22, Claude Code 2.1.309]** Claude keeps each conversation's transcript at
  `<config dir>/projects/<cwd with every non-alphanumeric character a dash>/<conversation
  id>.jsonl` (`/Users/me/dev/easy_llm_wrapper` → `-Users-me-dev-easy-llm-wrapper`); the newest one
  there is the conversation a Claude started in that directory is in. Adopting one (UX1.4)
  resumes it.
- **[verified 2026-10-22, macOS 25.6, Claude Code 2.1.291–2.1.309]** Claude Code's native
  installer puts each release at `~/.local/share/claude/versions/<version>` and points
  `~/.local/bin/claude` at it, so the kernel names the process after the version (`ps` `ucomm`
  `2.1.309`, `/proc/<pid>/comm` on Linux) while argv[0] stays `claude`. The foreground probe uses
  argv[0] when the kernel's name looks like a version (UX1.1). A copy of a macOS system binary
  is killed at exec (code signing), so that end-to-end test runs on Linux only.
- **[verified 2026-10-22, macOS 25.6]** SIGTSTP sent to a PTY's session leader is discarded,
  because its process group is orphaned (POSIX; Linux does the same). After `killpg(TSTP)` the
  process is still `Ss+`, while SIGSTOP stops it (`Ts+`). Claude Code 2.1.309 on Ctrl-Z resets
  its terminal modes, leaves raw mode and signals itself. As a ptyro child it keeps running, and
  after SIGCONT it goes raw again and redraws (`crates/ptyro/tests/m2.rs`, ctrl_z tests).
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
  force full-screen apps to repaint. With daemon-side emulation we rarely need it; the escape
  hatch is the TUI's `C-l` (`view.redraw`), and there is no `ptyro redraw` command.
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
- **Prefix-key survey (2026-10-22), the agents' default Ctrl+letter bindings** (for §21.2):
  - **[verified docs]** Claude Code (code.claude.com, interactive mode) binds A B C D E G J K L N O
    P R S T U V W Y Z, plus `C-x C-k`, `C-x C-e`, `C-x C-s`, `C-_` and `C-[`.
  - **[verified docs]** Gemini CLI (geminicli.com, keyboard shortcuts) binds every letter but I, M
    and Q. I and M are Tab and Enter anyway.
  - **[verified docs]** OpenCode (opencode.ai/docs/keybinds) uses `ctrl+x` as its leader key and
    binds A B C D E F G J K N P R T U V W Z directly.
  - **[secondary]** Codex CLI: C, D, G (editor), L, O (copy), R, T (transcript); its keymap is
    configurable (`/keymap`).
  - So Q is the only Ctrl+letter free in all four. `C-]` and `C-\` are free too, but need AltGr on
    many non-US layouts.
  - **[secondary]** Outside the agents: VS Code on Linux and Windows binds Ctrl+Q to
    `workbench.action.quickOpenView`. Its terminal swallows the key unless
    `-workbench.action.quickOpenView` is added to `terminal.integrated.commandsToSkipShell`. Zellij's
    default preset quits Zellij on Ctrl+q. Ctrl-Q is XON, but raw mode clears `IXON`, so it arrives.
- **Opening a terminal app in a folder** ("open in", §13.2):
  - **[V 2026-11-02]** On macOS, Terminal 2.15 (`public.directory`), iTerm 3.7.3 (`public.folder`) and
    Ghostty 1.3.1 (`public.directory`) list folders under `CFBundleDocumentTypes` in their
    `Info.plist`, so `open -a <app> <folder>` opens a window there. Terminal.app lives in
    `/System/Applications/Utilities`, not `/Applications`.
  - **[S]** The documented flags, used on Linux (and for WezTerm, kitty and Alacritty on macOS, by
    their command or the program inside the app): `ghostty`, `gnome-terminal`, `kgx`,
    `xfce4-terminal`, `foot` `--working-directory=DIR`; `konsole --workdir DIR`;
    `wezterm start --cwd DIR`; `kitty --directory DIR`; `alacritty --working-directory DIR`. The
    `ghostty` command can't start a window on macOS, so there it is the app.

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

### 20.8 Chat engines: the CHAT0 spike **[verified 2026-10-29]**

Run again with `make spike-chat` (the agents) and `make spike-chat-fts` (SQLite); every line below
that says "verified" is a check in `scripts/spike-chat.py` or `crates/ptyro-store/examples/
chat_fts_spike.rs`. Versions: Claude Code 2.1.312, Codex 0.155.1, Gemini CLI 0.66.0, OpenCode
1.19.5, SQLite 3.53.2 (rusqlite 0.40.2 `bundled`), on macOS. Account: Claude and Codex on this machine's own
logins (Claude's is not an API key: `--bare` said "Not logged in"). **Not verified:** any account type but those two (`WebSearch` "on every
account type" stays open), Linux, a Gemini turn (no login here), an OpenCode model turn (its free
tier was at its limit, and no provider was logged in).

**Claude Code (stream-json, `claude -p … --permission-prompt-tool stdio`)**

- **The chat launch line, verified:** `--tools "WebSearch,WebFetch" --allowedTools WebSearch
  WebFetch --strict-mcp-config --safe-mode`. `init.tools` is then exactly `["WebFetch",
  "WebSearch"]`; `WebSearch` runs in `-p` stream-json with no card (its input has a `mode`,
  `standard` or `extended`; a search turn cost $0.015–0.04 on haiku). `--tools ""` plus the same two
  flags leaves no tool at all.
- **`--tools` limits built-in tools only.** With `--tools ""` alone, the user's MCP servers (here:
  claude.ai connectors, a plugin's AWS server, two database servers) are still loaded and offered.
  `--strict-mcp-config` removes every MCP server, claude.ai connectors included.
- **`--safe-mode` (new, found by this spike).** It turns off CLAUDE.md, skills, plugins, hooks, MCP,
  custom commands and agents, and keeps login, model choice and permissions. A hook given with
  `--settings` ran without it and did not run with it. Skills went from 56 to 17 and plugins from 13
  to 6 (the built-in ones). A chat should always get it: a user's hooks and instructions would
  otherwise run inside every chat, and the skill list costs tokens each turn. `--restricted` is also
  new (no shell or code tools, ignores settings files); `--safe-mode` is the one that fits.
- **`--safe-mode` drops even an explicit `--mcp-config`** (verified 2026-10-29, Claude 2.1.314, CHAT3,
  about $0.002): the history tools never appeared with it. The line that keeps our server and still
  keeps the user's setup out is `--setting-sources "" --disable-slash-commands --strict-mcp-config
  --mcp-config <json>` with `--system-prompt`: the `ptyro` server connected, no skills or slash
  commands, only the built-in plugins, and a CLAUDE.md planted in the scratch folder was ignored. A
  chat engine with the history tools gets that line; one without keeps `--safe-mode`.
- **`--bare` cannot be used:** on a subscription login it answers "Not logged in" (it takes
  `ANTHROPIC_API_KEY` or an `apiKeyHelper` only). RESEARCH IV.3 listed it as an option; it isn't one.
- **A bare `--allowedTools Read` is not a folder limit.** With `--tools Read,Glob,Grep --add-dir D
  --allowedTools Read Glob Grep` a file **outside** `D` was read with no card. Without
  `--allowedTools` for them, files under `D` (and the cwd) are read freely and any other path raises
  a `can_use_tool` request. A path-scoped rule `Read(D/**)` behaves the same. §22.5's `read`
  column therefore lists `--add-dir` and no `--allowedTools` for the read tools.
- **Permission path:** a tool in `--tools` and not allowed raises `control_request` `can_use_tool`
  (denials are listed in the `result`'s `permission_denials`); the driver's existing answer works.
  `--permission-prompts none` denies without asking (not needed).
- **Fork and resume:** `--resume ID --fork-session` gives a new `session_id`, carries the whole
  context, and leaves the original unchanged; `--resume ID` alone keeps the id. The session file
  lives under `~/.claude/projects/<cwd with / → ->/`, so **each chat's scratch directory makes its own
  project folder** in Claude's own history. No flag in `--help` forks at a message (2.1.312), so a fork from the middle is seeded, as §22.6 says.
- **No persistence:** `--no-session-persistence` writes no session file, and `--resume` of its id
  fails with "No conversation found with session ID …" on stderr.
- **No title.** The stream has no title event and the session file has no title record in print
  mode, so Claude's chats always use the one-shot title (§22.8).
- **System prompt:** `--system-prompt "…"` replaces Claude Code's (the model then answers as a
  general assistant) and a one-line turn cost $0.00017 against $0.00046 with the default, so the
  `chat` preset passes one. `--append-system-prompt` left the answer's identity as Claude Code.

**Codex (`codex app-server`, JSON-RPC)**

- **`thread/start` takes** `config` (a map of config keys: it worked for `web_search`, `features.*`
  and `mcp_servers.<name>.enabled`), `ephemeral`, `baseInstructions`, `developerInstructions`,
  `sandbox` (`read-only`, `workspace-write`, `danger-full-access`: these are the *mode* strings;
  `readOnly` is the *policy* object of `turn/start`) and `approvalPolicy`.
- **Web search:** `config {"web_search": "live"}` with `sandbox: "read-only"` produced a `webSearch`
  item and an answer; no approval was asked.
- **`readOnly` still reads.** In a `read-only` thread the model ran `cat` on a file **outside** the
  cwd and printed it; a write (`echo > file`) raised `item/commandExecution/requestApproval` and
  was refused when declined. So §22.5's "`chat` can't touch a file" is false for Codex as written.
- **No shell:** `config {"features": {"shell_tool": false, "unified_exec": false}}` leaves the thread
  without a shell (no `commandExecution` item) and web search still works. The model still lists
  `apply_patch` (a write, which the read-only sandbox refuses), image viewing, goals and
  sub-agents.
- **A turn can end without `turn/completed`.** When the model asked for the disabled shell, the
  thread went `active` → `idle` in about 3 s and **no `turn/completed` was sent**; the answer arrived
  as `agentMessage` items. The driver (§8.4) must treat `thread/status/changed → idle` after
  `turn/started` as the end of the turn. A normal turn sends both.
- **Your hooks and MCP servers run inside a chat thread.** `hook/started` fired on every turn, and
  `mcpServer/startupStatus/updated` showed the user's servers starting. `features.hooks = false`
  stopped the hooks, and `mcp_servers.<name>.enabled = false` for each server that `codex mcp list`
  shows stopped all of them (the table can't be emptied, only each entry switched off).
- **A folder-limited `read`:** `config {"default_permissions": "p", "permissions": {"p": {"extends":
  ":read-only", "filesystem": {"<HOME>": "deny", "<dir>": "read", …}}}}` and **no `sandbox`
  parameter** (it wins over the profile and nothing is confined) let a thread read `<dir>`, refused a
  sibling folder under `$HOME` ("Operation not permitted"), and still read `/etc/hosts`. The profile
  also has to re-allow what Codex needs to start: the directory of the `codex` binary, `~/.local/bin`
  (the launcher), `~/.codex`, and the thread's cwd, or the thread fails to start with "failed to load
  AGENTS.md … Operation not permitted". The built-in profile ids are `:read-only`, `:workspace`,
  `:danger-full-access`. An allow-list alone (`:minimal` plus folders) did not confine reads.
- **Fork:** `thread/fork {threadId, lastTurnId}` returns a thread with exactly the turns up to
  `lastTurnId` (1 of 3 in the check) and a model that knows only those; the original is unchanged.
- **Resume:** `thread/resume {threadId, cwd, …}` in a **new** app-server process continued the
  conversation (3 turns restored).
- **Ephemeral:** `thread/start {ephemeral: true}` has no `path`, leaves no rollout and is not in
  `thread/list`: Codex's own incognito.
- **Titles:** no `thread/name/updated` arrived over three turns (or for a fork), and `name` stayed
  null, so the one-shot title is needed. `thread/name/set` works, and `thread/list {searchTerm}` finds
  the thread by that name. `thread/delete` removes a thread's rollout.
- **Approvals** reached the client as server requests (`item/commandExecution/requestApproval`),
  as §8.4 says.

**Gemini CLI (`gemini --acp`, 0.66.0)**

- `initialize` answers `protocolVersion 1`, `authMethods` (`oauth-personal`, `gemini-api-key`,
  `vertex-ai`, `gateway`) and `agentCapabilities {loadSession: true, promptCapabilities {image,
  audio, embeddedContext}, mcpCapabilities {http, sse}}`. **There is no `sessionCapabilities`:
  `session/list`, `session/resume` and `session/fork` are not offered.** A Gemini chat continues with
  `session/load` or a seed. `session/new` without a login answers error -32000 ("Gemini API key is
  missing or not configured"); `GEMINI_CLI_HOME` keeps a spike out of your own home.
- **Tool restriction is the policy engine.** `--policy FILE` (also `--admin-policy`) loads TOML:
  `[[rule]] toolName = "*" | "name" | ["a", "b"]`, `decision = "allow" | "deny" | "ask_user"`,
  `priority` (higher wins; user-tier rules are above the built-in ones), optional `commandPrefix`,
  `argsPattern`, `mcpName`, `modes`, `denyMessage`. The shipped `plan.toml` uses a `toolName = "*"`
  `deny` catch-all. So the `none` preset is a catch-all deny, and `chat` is that plus an `allow` for
  `google_web_search`, both in a file ptyro writes and passes with `--policy`. `--allowed-tools` is
  deprecated for it (the help says so). **Not tried:** that a rule stops a call, because a turn needs
  a login, and there is none here. Also present: `--approval-mode plan` (read-only),
  `--allowed-mcp-server-names`, `--extensions`, `--session-id`, `--list-sessions`,
  `--delete-session`, `--resume`.
- Consumer-account availability after 2026-06-18 is still RESEARCH IV.2's open item.
- **Read from its source, 0.66.0 (2026-10-30, [S] unless marked [V]; the bundle's
  `packages/core/src/policy` and `packages/cli/src/acp`):**
  - *Policy tiers:* default 1 < extension 2 < workspace 3 < user 4 < admin 5; a rule's effective
    priority is `tier + priority/1000`. `--policy` paths **replace** the user's policy folder in the
    user tier; `--admin-policy` paths load in the admin tier (with the system folder,
    `/Library/Application Support/GeminiCli/policies` or `/etc/gemini-cli/policies`), **unless that
    system folder already holds a `.toml`**, when Gemini ignores `--admin-policy` with a warning.
    "Always allow" answers and settings' allow lists sit at tier 3–4.3, so an admin-tier deny wins
    over all of them. Extensions may not contribute `allow` rules.
  - *Rule schema:* `toolName` (string or array; required), `mcpName`, `argsPattern`,
    `commandPrefix`, `commandRegex`, `decision` (`allow`/`deny`/`ask_user`), `priority` 0–999,
    `modes`. `toolName = "*"` matches every tool, MCP ones included; `mcp_<server>_*` every tool of
    one server (a server name can't hold `_`). Tool names: `read_file`, `read_many_files`, `glob`,
    `grep_search`, `list_directory`, `run_shell_command`, `google_web_search`, `web_fetch`.
  - *File tools read only the workspace:* `validatePathAccess` allows the session's `cwd`, the
    `--include-directories` and Gemini's own temp folder for the project; anything else is "Path not
    in workspace".
  - *MCP:* `session/new` and `session/load` take `mcpServers` in ACP's stdio shape `{name, command,
    args, env: [{name, value}]}` (every field needed: `env` is iterated) and merge them over the
    settings' servers; `--allowed-mcp-server-names` then **replaces** the settings' allow list (and
    drops their exclude list); a lone empty name (`--allowed-mcp-server-names ""`) allows none.
  - *Extensions:* `--extensions none` (exactly that one word) turns every extension off.
  - *Where an ACP session is kept:* `<GEMINI_CLI_HOME or $HOME>/.gemini/tmp/<slug>/chats/`, with
    `history/<slug>/` beside it; `<slug>` comes from the folder's name (`projects.json` maps the
    folder to it) and each such folder holds `.project_root`, the folder it is for. **[V]**
    2026-10-30: a `gemini --acp` with ptyro's chat flags (`--admin-policy`, `--policy`, `--extensions
    none`, `--allowed-mcp-server-names ptyro`) and a `session/new` with one stdio server got as far
    as the login check (-32000) with no policy or argument error, and wrote `tmp/<slug>/.project_root`
    holding the folder's path.
  - *Titles:* the ACP schema has `session_info_update {title}`, but 0.66.0 never sends one, so the
    one-shot titler names Gemini chats.
  - *No incognito switch:* nothing keeps a session from being written; ptyro removes the folders
    above when the chat ends.

**OpenCode (`opencode serve`, 1.19.5; `/doc` is its OpenAPI)**

- **Per-session permissions are native.** `POST /session {permission: [{permission, pattern,
  action: allow|deny|ask}], title?, agent?, model?, parentID?}` stores the ruleset and returns it
  (`{"permission": "*", "pattern": "*", "action": "deny"}` then `websearch` allowed was accepted).
  `POST /session/:id/message` also takes `tools: {name: bool}`, `system`, `model`, `agent`, `noReply`.
  The registered tool ids are `bash, read, glob, grep, edit, write, task, webfetch, todowrite,
  websearch, skill, apply_patch, question`. **Not tried:** that a rule stops a call (no model turn).
- **Fork:** `POST /session/:id/fork {messageID?}` copies the messages **before** `messageID` (a
  session of user + assistant forked at the assistant message had 1), or all of them with no body,
  and titles it "… (fork #1)". **The fork has no `permission` ruleset**: set it again with
  `PATCH /session/:id {permission}` straight after every fork.
- **Titles:** `PATCH /session/:id {title}` sets one; OpenCode also writes its own (the sessions in
  the spike got generated titles), announced in `session.updated` events.
- **Free usage runs out.** The `opencode` provider's free models (`big-pickle`, …) work with no
  login until a limit; then `/session/status` reports `{type: "retry", message: "Free usage exceeded,
  subscribe to Go", action: {reason: "free_tier_limit", link}}` and a synchronous `POST …/message`
  does not return. The driver needs a `retry` status with an `action` shown as a notice, and must not
  use the synchronous call. (An unsecured server is a warning: `OPENCODE_SERVER_PASSWORD`.)
- It runs with `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_CACHE_HOME` set, which
  keeps its state out of the user's.

**ACP in general:** `session/load` (Gemini), and for others `session/list` and `session/resume` only
where `agentCapabilities.sessionCapabilities` says so; read it at `initialize` and don't assume.

**SQLite (`chat.db`)**

- **FTS5 is in the bundled build** (`ENABLE_FTS5`, SQLite 3.53.2; `dbstat` too) and the §22.4 schema
  creates with `tokenize='porter unicode61 remove_diacritics 2'`, the triggers included.
- **The tokenizer, verified:** `run` finds `running`; `cafe`, `CAFÉ` and `Café` match each other;
  `"retry policy"` phrases, `migrat*` prefixes, `borrow NOT migration` and AND work; `snippet()` marks
  the hit and `bm25()` ranks (lower is better); a tombstone, an edit, a delete and a chat's cascade
  each leave the index right, and `integrity-check` passes after them.
- **Limit: CJK.** `unicode61` makes a run of CJK characters one token, so `日本語` is found by
  itself and `日本` is not. Chinese, Japanese and Korean chats search by whole run only. The
  `trigram` tokenizer does substrings (at about three times the index) and could be a second index,
  added when someone asks (ROADMAP CHAT3).
- **Size and speed, 100,000 nodes (made-up prose, 589 characters a node on average, half short
  "user" nodes and half long "assistant" ones, in 2,500 chats; 59 MB of text, and the `body` JSON
  about doubles it):** `chat.db`
  200 MB, of which the FTS index 30 MB; loaded through the triggers in 6 s (16,000 nodes/s);
  top-20 by bm25: a rare word 4 ms, a middling word 7 ms, a phrase 2 ms, a very common word 46 ms
  (with `snippet()` 42 ms, with a profile and workspace join 81 ms), a two-letter prefix 104 ms
  (worst 200 ms); `optimize` 0.3 s, `rebuild` 1.2 s, `integrity_check` 0.4 s; a streaming node's 2 s
  flush (an `UPDATE` through the triggers) 0.9 ms. So a search box needs no index beyond FTS5 at this
  size, and a prefix of fewer than three characters should wait for the third keystroke.

**The agents' own histories, as CHAT5's import reads them** (2026-10-30; Claude Code and Codex
read from this Mac's files, read-only **[V]**; Gemini from 0.66.0's bundle **[S]**: it isn't used
here):
- **Claude Code** (2.1.x): `<config dir>/projects/<cwd with every non-alphanumeric as ->/<session
  id>.jsonl`, one JSON object a line. `type` `user` / `assistant` lines carry `uuid`,
  `parentUuid`, `sessionId`, `cwd`, `timestamp` (RFC 3339, ms), `isSidechain`, and `message.content`:
  a string, or blocks `text`, `thinking`, `tool_use {id, name, input}`, `tool_result {tool_use_id,
  content, is_error}` (results come in the next `user` line). `isMeta` lines are the caveats and
  command output; a typed slash command is a string wrapped in `<command-name>`, its output in
  `<local-command-stdout>`, a `!` command in `<bash-input>`/`<bash-stdout>`, and background tasks in
  `<task-notification>`. `isCompactSummary` marks a compaction's summary. Titles are lines of their
  own: `custom-title {customTitle}` (one you gave), `ai-title {aiTitle}` (newer versions) and
  `summary {summary}` (older); other lines (`attachment`, `api-request*`, `mode`, `last-prompt`,
  `file-history-*`, `cost-state`, `continued-in`…) carry no conversation. An API error is an
  `assistant` line with `isApiErrorMessage`. Sub-agents' transcripts sit in folders of their own,
  not beside the sessions. Each assistant line holds one block (several lines share a
  `message.id`).
- **Codex** (0.164 desktop, this Mac's 14 rollouts): `<CODEX_HOME>/sessions/YYYY/MM/DD/rollout-<time>-<thread id>.jsonl`
  (archived ones in `archived_sessions/`), lines `{timestamp, type, payload}`: `session_meta {id,
  cwd, originator, cli_version…}`, `response_item`s (`message` with `role` `user` / `assistant` /
  `developer` and `content` blocks `input_text` / `output_text`; `function_call` and
  `custom_tool_call {name, arguments|input, call_id}` and their `*_output {call_id, output}`;
  `reasoning`; `web_search_call`), and `event_msg`s that repeat them for the UI (`item_completed`,
  `token_count`, `task_*`). The user's own words are the `user` messages whose
  `internal_chat_message_metadata_passthrough.content_item_kinds` include `user.text`; Codex's
  context (`<environment_context>`, the AGENTS.md it read, plugin suggestions) is a `user` message
  too, of other kinds. Titles: `<CODEX_HOME>/session_index.jsonl` lines `{id, thread_name,
  updated_at}`.
- **Gemini CLI 0.66.0 [S]:** `<GEMINI_CLI_HOME or ~>/.gemini/tmp/<project slug>/chats/session-<time>-<id8>.jsonl`
  (a legacy `.json` is one object with `messages`), the folder's `.project_root` naming the project
  (§22.9). Lines: a metadata record (`sessionId`, `projectHash`, `startTime`, `lastUpdated`, `kind`,
  `summary`); message records (`id`, `timestamp`, `type` `user` / `gemini` / `info` / `error` /
  `warning`, `content` as a string or parts with `text`, and on `gemini` ones `toolCalls [{id, name,
  displayName, args, status, result, resultDisplay}]`, `thoughts`, `tokens`, `model`), a later record
  with the same id replacing the earlier; `{"$rewindTo": id}` drops that message and what follows;
  `{"$patch": {id, content?, toolCalls?, updates?, removeIds?, orderIds?}}`; `{"$set": {…}}`
  (metadata, or every message). Sub-agents' files are in `chats/<parent id>/`. ptyro applies all but
  `orderIds`.
- **OpenCode:** not installed here; its storage is not read (§22's "As built").

### 20.10 WSL2 **[verified 2026-11-02: WSL 2.6.2.0, kernel 6.6.87.2, Ubuntu 24.04.3, Windows 11 build 26200]**

- **A systemd user unit gets no `WSL_DISTRO_NAME`** (nor `WSL_INTEROP`). `wslpath -w /` still prints
  `\\wsl.localhost\<distro>\` there, so `ptyro_core::wsl` names the distro from it (§3.4). [V]
- **VS Code's `bin/code` script needs `WSL_DISTRO_NAME` on WSL2.** It decides it is inside WSL from
  that variable; its fallback reads a WSL1-style kernel release (`-<build>-Microsoft`) and fails on
  WSL2's. Without it, `code` starts Windows VS Code with a Linux path (read in the script of VS Code
  commit `2a59476`). So ptyro runs it as `env WSL_DISTRO_NAME=<distro> …`. [V]
- **A shell rc that sets `PATH` outright drops the Windows directories WSL appends**: `cmd.exe`,
  `explorer.exe`, `powershell.exe` and `code` stop being found by name. ptyro looks for Windows'
  programs under `<mount root>c/Windows` (`System32`, the folder itself,
  `System32/WindowsPowerShell/v1.0`) when `PATH` doesn't have them (`Wsl::windows_exe`).
  `%USERPROFILE%` comes from `cmd.exe /d /c echo %USERPROFILE%`, run from `/mnt/c` (from a Linux folder
  cmd.exe complains about UNC paths first). [V]
- **Toasts:** Windows PowerShell 5.1's WinRT toast API (`ToastNotificationManager`) shows a
  toast from WSL with no module installed. The title and body go as environment variables listed
  in `WSLENV` and become XML text nodes, so agent text never becomes script. Both test toasts
  appear in Windows' notification log (owner, 2026-11-02), the second with a 漢字 title [V].
  Since WIN10 the same script serves the native daemon too, and a toast comes from ptyro (its
  AppUserModelID, once `ptyro.exe` has registered it) and opens its session when clicked (§20.12
  "ptyro as a Windows app").
- **Survival after the last terminal (WIN0.1).** WSL 2.6.2.0, Windows 11 build 26200
  (2026-11-03), measured with throwaway distros (`wsl --import` of an Alpine and an Ubuntu base
  rootfs) beside the owner's Ubuntu, so no terminal had to close. "Stops" is `wsl -l --running`
  dropping the distro, or the last beat of a heartbeat inside it:

  | What runs once the last `wsl.exe` that started it exits | The distro |
  |---|---|
  | nothing | stops at ~15 s |
  | a `setsid nohup` process left by `wsl.exe -e …` (no console) | stops at ~15 s |
  | systemd as PID 1, a system service and a user service with linger | **stops at 13–14 s** |
  | a Windows program started from inside (interop) | stops (the program is left running on Windows) |
  | a process left by a `wsl.exe` that had a console window | **held** until that process exits, then stops ~15 s later |
  | `wsl.exe -d <itself> -e …` started from inside | **held** while it runs |
  | …after its inner command exited, its Linux-side proxy left over | **held** until the proxy is killed (then 17 s) |

  So systemd and linger don't keep a distro running (as microsoft/WSL#13416 reports for 2.6.1
  and 2.7.3). The 9 h 26 min overnight `make spike-wsl` run (2026-11-02/03, systemd and linger
  on) survived because its heartbeats were started from a terminal: that terminal's `wslhost.exe`
  (started 22:46:22, its `wsl.exe` long gone) was still alive in the morning. A daemon started
  by a systemd unit at boot would have had nothing holding it. [V]
  - **What ptyro does** (`[daemon] hold_wsl`, on by default; `crates/ptyro-daemon/src/wsl_hold.rs`):
    the daemon starts `wsl.exe -d <distro> -u <its user> --cd / --exec /bin/sh -c '<loop>'`
    as its child, where the loop (marked `: ptyro-wsl-hold <pid> <start ticks>;` in `ps`)
    sleeps while `/proc/<daemon pid>/stat` still shows the daemon's start time. Stopping the
    hold (shutdown, the setting going off, a live upgrade) kills the proxy and the loop; a
    killed daemon takes the proxy with it (`PR_SET_PDEATHSIG`, from a thread that waits on it)
    and the loop sees the pid gone. Checked: a clean stop, SIGKILL, the setting off and on, and
    a live upgrade (same pid, a new holder) each leave exactly the holders they should, inside
    the distro and on Windows. `tests/win2.rs` checks it in WSL. Interop works from a systemd
    user unit, so a daemon started by `install-service` holds too. [V]
  - **End to end, an hour** (2026-11-03, a throwaway Ubuntu-base distro with no systemd and no
    terminal): ptyro's daemon, started from a `wsl.exe -e` that exited at once (worse than any
    real start), with a session writing a heartbeat every 10 s. 350 beats over 3,700 s, the
    longest gap 13 s; after `ptyro daemon stop --force` the distro stopped 16 s later. Once in
    that run the first holder's proxy ended about 10 s after starting (the session that
    started the daemon went, its interop channel with it) and the supervisor started another;
    it now ends the first one's loop before it does. [V]
  - **Still open:** Windows sleep and hibernate (a held distro and the PC asleep: what happens
    to the agents?). [open]
- **Hosted GitHub runners do have WSL2** (2026-11-03): `windows-latest` (Windows Server 2025, build
  26100) has WSL 3.0.3.0 with kernel 6.18.54.1, default version 2 and no distro. With
  `Vampire/setup-wsl` (v7.2.0) an Ubuntu 24.04 boots as WSL2 with systemd as PID 1, 4 CPUs and
  8 GB. `windows-11-arm` can't: `WslRegisterDistribution` fails with 0x80370102 (no
  virtualization). So the [U] "hosted runners have only WSL1" (RESEARCH IV.3, WIN0.5 f) was
  wrong. [V]
- **Booting systemd in a second distro can remove interop for all of them.** WSL writes an
  override for each distro's `systemd-binfmt.service` (`/run/systemd/generator/…/override.conf`,
  turned off by `[boot] protectBinfmt=false`) that registers `WSLInterop` again. Still, booting
  and stopping a freshly imported Ubuntu base with systemd removed `WSLInterop` from
  `/proc/sys/fs/binfmt_misc` three times, and every distro in the VM then failed to start
  Windows programs (`exec format error`). `echo ':WSLInterop:M::MZ::/init:PF' >
  /proc/sys/fs/binfmt_misc/register` (as root) brings it back; so does `wsl --shutdown`.
  `doctor`'s interop check tells this case from interop turned off in `/etc/wsl.conf`. [V]
- **`ps -o lstart` drifts on WSL.** For one PID 1 in one boot (same boot id) it printed 12:22:48,
  12:26:40 and 12:27:03 over the evening, so comparing it says "the distro restarted" when it
  didn't. Field 22 of `/proc/1/stat` (clock ticks since boot) doesn't move; `spike-wsl` uses it. [V]
- **Networking:** `wslinfo --networking-mode` prints `nat` by default. Under NAT, a web app bound to
  every interface (`access = open`) reaches Windows' localhost but not the Wi-Fi, so a phone can't
  reach it. `networkingMode=mirrored` under `[wsl2]` in `%USERPROFILE%\.wslconfig` (or Tailscale)
  can. `doctor` warns. [V for the mode and the rule; mirrored not tried yet]
- **`wsl.exe --cd "~" -- bash -lc "cd ~/dev/ptyro && …"`** runs in the distro with its login PATH
  (rustup's included), which is how `make test-wsl` starts from Windows. [V, run from WSL's own interop]
- **Windows Terminal** sets `WT_SESSION`, and shows OSC 9;4 progress on the tab and the taskbar
  button: state 1 a green ring, 2 red, 3 a busy spinner, 4 yellow (1, 2 and 4 with a percent, sent
  as 100). The TUI sends the look `[ui] progress_blocked` (yellow), `progress_errored` (red) or
  `progress_working` (busy) names for the first of those states any live session is in, skipping one
  set to `off`, and state 0 when none applies and when it exits. [S: WT's documented OSC 9;4; not yet
  looked at on screen]

### 20.11 Cross-building for Windows (WIN0.4) **[verified 2026-11-03: cargo-xwin 0.23.1, clang/lld 18.1.3 on Ubuntu 24.04 in WSL, Rust 1.99.1]**

No Windows machine, Visual Studio or Windows Rust is needed: `cargo xwin check --target
{x86_64,aarch64}-pc-windows-msvc` from Linux (or macOS, with Homebrew's LLVM) downloads Microsoft's
CRT and SDK into `~/.cache/cargo-xwin` (about 1 GB, once, under Microsoft's licence:
`XWIN_ACCEPT_LICENSE=1`). `make check-windows` wraps it. The native route (Build Tools on
Windows) wasn't needed to get the list below: compile errors are the same either way. [V]

- **The C libraries ptyro bundles all build, for both targets:** ring 0.17.14, libsqlite3-sys
  0.38.2 (bundled SQLite) and zstd-sys 2.1.2. H2's worry (they fail with no Windows C compiler)
  is answered by clang-cl and the xwin sysroot. [V]
- **ring on ARM64** compiles with the GNU-style `clang` driver whatever `CC` says, but cargo-xwin's
  `CFLAGS` are clang-cl's (`/imsvc DIR`), which clang reads as a file. cargo-xwin's
  `--cross-compiler clang` fixes ring and breaks SQLite and zstd (its sysroot clashes with the
  MSVC `intrin.h` in clang 18: `__prefetch`; with clang 19 its headers aren't case-insensitive:
  `Windows.h` not found). What works is `scripts/xwin/clang` first on `PATH`: `/imsvc` becomes
  `-isystem` for plain `clang` calls. cargo-xwin links its `clang-cl` to the first `clang` on
  `PATH` on every run, so the wrapper, called by that name, passes everything through in cl mode,
  and finds the real clang through its own symlinks (`readlink -f`). [V]
- **What stops a build, in order** (a scratch tree with `ptyro-core`'s `compile_error!` lifted,
  each layer stood in to reach the next; x64 and ARM64 alike):

  | Crate | For Windows | What it needs |
  |---|---|---|
  | `ptyro-core` | 10 errors (lib), 31 (tests) | file modes (`OpenOptionsExt::mode`, `PermissionsExt`), `O_NOFOLLOW`/`O_NONBLOCK` (`imports.rs`), `getuid` (`paths.rs`); tests: `symlink` |
  | `ptyro-protocol`, `ptyro-git`, `ptyro-meaning`, `ptyro-run`, `ptyro-drive` | build | nothing (ptyro-git's tests: 2) |
  | `ptyro-term` | 6 | `record.rs`: mode 0600 and `read_exact_at` (Windows: `seek_read`) |
  | `ptyro-store` | 2 | permissions in `chat.rs` |
  | `ptyro-agents` | 1 | `from_mode` in `locate.rs`, a symlink in `profile.rs` |
  | `ptyro-client` | 1 (and what follows) | `tokio::net::UnixStream`: the transport (WIN3's is `wsl.exe … ptyro proxy` over stdio) |
  | `ptyro-tui` | 3, once off `ptyro-pty` | it uses `ptyro-pty` only for `CLIENT_ENV` and `local_time_hm` (both belong in `ptyro-core`); then `health.rs` (chmod 600), `onboard.rs` (a 0600 write) and `outer.rs` (polls stdin's fd for the OSC 10/11 replies; a Windows console needs another way) |
  | `pty-process` → `ptyro-pty` → `ptyro-daemon`, `ptyro-testkit` | 65 | Unix PTYs: the daemon side (track D) |
  | `ptyro` (the binary) | 41, in 15 files, once the daemon, `ptyro-pty` and `ptyro-testkit` are `cfg(unix)` dependencies | the daemon only in `daemon start` and `handoff-version` (3 uses); the rest are modes, uids, `setsid`, `process_group`, `ESRCH` |

  **Built (WIN3, 2026-11-03):** the `compile_error!` is the daemon's now; `ptyro_core::os` holds
  what differs (private files and folders, `no_follow`, `detach`, executables by `PATHEXT`, the
  uid, `CLIENT_ENV`); `ptyro-tui` takes `ptyro-pty` only on Unix (its clock uses the time
  crate's local offset on Windows); the binary takes the daemon, `ptyro-pty` and the testkit
  only on Unix, and what only a daemon host does says to run it in WSL. `/dev/urandom` gave way
  to the getrandom crate (`ProcessPrng` on Windows). `ptyro.exe` links with an 8 MiB main stack
  (`/STACK`): Windows' 1 MiB overflowed at once in a debug build. The outer terminal's colour
  query reads Windows Terminal's answers from crossterm's key events on Windows (§20.12). Run on Windows 11 (test binaries started from WSL through interop):
  ptyro-core 135, ptyro-tui 221, ptyro-term 41, ptyro-protocol 30, ptyro-store 23, ptyro-client 7
  and the rest pass; tests of Unix behaviour (shells, the daemon's emulator and recorder, modes,
  symlinks, `sh`) are `#[cfg(unix)]`. ptyro-agents and ptyro-git only compile there: they run in
  the daemon, and their tests assume Unix paths and Unix git. [V]

  **Built (WIN4, 2026-11-04):** `ptyro-pty` builds for Windows too (x64 and ARM64, clippy clean):
  pty-process, rustix and libc are its Unix dependencies, and `windows.rs` stands beside
  `unix.rs` with the same functions. Its PTY types (`Reader`, `Writer`, `Master`) are empty enums
  there, so code that holds one type-checks and can't be reached; `spawn` says sessions on Windows
  are WIN5's and to run the daemon in WSL. `Signal` keeps the POSIX numbers clients send. Local
  time there is the time crate's offset. `make check-windows` and `make test-windows` include it.
  Smart App Control refused ptyro-pty's unsigned debug test binary on this machine (Device Guard)
  while it ran ptyro-core's (139 tests passed, `shell::default_shell` picking pwsh first), so
  ptyro-pty's Windows tests run in CI's `windows` job (§20.12). [V]

  What this table said, before it was built: the client side (core, protocol, client, term, tui and the binary without the daemon) is
  about 65 small changes, nearly all "a file only its owner reads" and "who am I", plus two real
  designs: the client's transport, and reading the outer terminal's replies. The `ptyro-tui` row
  was checked for both targets; the rest for x64 with ARM64 matching at every layer it reached. [V]

### 20.12 The Windows client in Windows Terminal (WIN3) **[verified 2026-11-03: Windows 11 build 26200, Windows Terminal from the Store, crossterm 0.29.0]**

- **Windows Terminal answers OSC 10/11 and DA1, as key presses.** A program in raw mode that
  writes `OSC 10;? ST OSC 11;? ST CSI c` gets the answers through `ReadConsoleInputW` (crossterm's
  `event::read`) as one key event per character, kind Press, no modifiers, with every ESC dropped
  (an injected ESC has no virtual key, which crossterm skips): `]10;rgb:cccc/cccc/cccc\`
  `]11;rgb:0c0c/0c0c/0c0c\` `[?61;4;6;7;14;21;22;23;24;28;32;42;52c`, all within 12 ms (Campbell).
  `outer::console_replies` picks them out of the keys typed meanwhile. End to end, a WSL session
  under `ptyro.exe` in a tab using the Campbell PowerShell scheme was answered
  `rgb:0101/2424/5656`, that scheme's background. [V]
- **Keys from a console are already disambiguated.** crossterm's Windows reader builds each key
  from the record's virtual key code and its modifier flags: `VK_RETURN` with Shift is Shift+Enter,
  `VK_TAB` is Tab while Ctrl+I is `Char('i')` with Ctrl, and releases arrive as Release events
  (dropped unless the program asked for them, `ptyro-term` `input.rs`). Windows Terminal hands
  conpty full records (win32-input-mode). crossterm's `supports_keyboard_enhancement` is always
  false on Windows, and the kitty protocol isn't needed there. [V for crossterm's code, from its
  source; S for Windows Terminal's records, from its documentation]
- **Smart App Control blocks some unsigned programs and not others.** With it on, a freshly built
  unsigned `probe.exe` (debug and release) was refused ("An Application Control policy has blocked
  this file", CodeIntegrity event 3077). Every unsigned `ptyro.exe` that day ran, debug and
  release (the release build is what the zips ship). It judges a file by its contents, not its
  name or folder: a fresh hello-world ran, the refused probe stayed refused under another name,
  and `ptyro.exe` renamed still ran. The probe was small, put the console in raw mode and read
  keys, which may look like a keylogger (a guess). The ConPTY spike (§20.13) ran as a debug build
  and was refused as a release build. There's no per-file exception, and turning it off can't be
  undone without reinstalling Windows. Signing (WIN12) is the real answer; install.ps1 says what
  happened if `ptyro.exe` is refused. [V]
- **A detached daemon needs a console, just not a window.** Started with `DETACHED_PROCESS`
  (no console at all), every console program the daemon ran in the background (git, gh, cmd) got
  a console of its own, and so a window: with Windows Terminal as the default terminal, each git
  call flashed a Windows Terminal window open and shut (2026-11-04, the native daemon from WIN7
  polling git). `CREATE_NO_WINDOW` gives the daemon a console with no window, and its children
  inherit it. `ptyro_core::os::detach` uses it; its test runs itself detached and asks
  `GetConsoleProcessList` (no console under `DETACHED_PROCESS`, one with `CREATE_NO_WINDOW`) and
  `GetConsoleWindow`. It failed under `DETACHED_PROCESS` and passes with `CREATE_NO_WINDOW` on
  Windows 11 (2026-11-05; Smart App Control refused one rebuild of the test binary the day before,
  then let a later one run). [V]
- **No one quoting suits every Windows shell.** An agent runs a hook or status line `command` in
  a shell: Claude in Git Bash when it finds one, else PowerShell (its `shell` field, `"bash"` or
  `"powershell"`, picks; a `statusLine` has none) [S, Claude's hooks and status line docs,
  2026-11-04]. Run in Git Bash 2.x, Windows PowerShell 5.1 and cmd on Windows 11 build 26200:
  a path with nothing to quote, written with forward slashes (`C:/Users/me/…/ptyro.exe hook …`),
  runs in all three; with backslashes Git Bash drops them; a quoted path (`"C:/Program Files/…"
  …`) runs in Git Bash and cmd but PowerShell reads a string, not a command; PowerShell's
  `& '…' …` runs only there. So ptyro writes its path with forward slashes, unquoted; a path
  that needs quotes (a space in the user's name) becomes its 8.3 short name
  (`GetShortPathNameW`, kept on the system volume by default); and failing that, Claude's hooks
  name PowerShell (`"shell": "powershell"`, `& '…'`), which every Windows has, while the status
  line, Codex, Gemini and agy get double quotes (Git Bash and cmd). Claude's exec form (`args`,
  no shell, v2.1.139) was passed over: reports say plugin `hooks.json` ignores it, and that on
  Windows with Git Bash it still goes through bash [U]. [V for the three shells]
- **A detached child inherits the parent's standard handles.** std starts a Windows child with
  `bInheritHandles`, so it gets every inheritable handle, the parent's stdout among them, whatever
  its own `Stdio` says: a daemon the CLI started held the pipe a script (or WSL's interop) read
  the CLI's output from, and the reader waited for its end for as long as the daemon ran (seen:
  `ptyro doctor` from WSL hung until its timeout when it started the daemon). Before starting
  anything detached, ptyro clears the inherit flag on its own standard handles
  (`ptyro_pty::std_handles_private`); a child given them on purpose still gets std's copies.
  [V, 2026-11-05: with the fix, PowerShell's `$out = & ptyro.exe new …`, which starts the daemon,
  returns at once with the output.] **Not covered:** handles the CLI itself inherited from *its*
  parent that aren't its standard handles (a PowerShell started from WSL hands its interop pipes
  on) still reach the daemon, so the WSL side of such a chain waits. The whole fix is to start a
  daemon with no inheritance at all (`CreateProcessW` with `bInheritHandles` off; it wants
  nothing inherited), which std's `Command` can't do.
- **ConPTY holds a lone ESC.** Raw input reaches ConPTY as VT, where ESC may begin a sequence:
  a lone one (`send --raw '\e'`, an answer's "deny", a stop key) waited for the rest and never
  arrived (a Codex panel stayed open, 2026-11-04). For a program in win32-input-mode, a lone ESC
  (at the end, or before another) goes as the Escape key's record (`input::raw_input`); whole
  sequences (`\e[B`) and Alt+key (`\ex`) went through as they are. [V for the held ESC]
- **Codex 0.166 on Windows** (2026-11-05): its approval overlay says its keys, `y` proceed, `p`
  don't ask again, `esc` no (ptyro sent `a` for "always"; it sends `p` when offered); its trust
  question is "Trust this folder?" with "1. Trust and continue" preselected (Enter) and Esc to
  quit, and text typed into it can quit Codex; with no `[windows] sandbox` it runs each command as
  you ("Environment: local") and asks first, offering no setup. [V]
- **PowerShell's execution policy** (Restricted by default) refuses `install.ps1` run as a file,
  but not `irm … | iex`, which evaluates a string. [V]
- **`wt.exe` splits its command line at every `;`**, also inside a `powershell -Command` string
  after `--`: a script file avoids it (as `tab.rs` escapes `\;`). [V]
- **ptyro as a Windows app (WIN10)** [V, 2026-11-05, Windows 11 build 26200]:
  - An unpackaged app's toasts carry its name and icon when its AppUserModelID is registered
    under `HKCU\Software\Classes\AppUserModelId\<AUMID>` (`DisplayName`, `IconUri` a PNG
    path); no Start-menu shortcut is needed and no administrator. ptyro's is `dev.ptyro.app`,
    its icon the web app's, written to `<data>\ptyro.png`.
  - A toast whose `<toast>` has `launch="<uri>" activationType="protocol"` opens that URI when
    clicked, through the URI's registered handler (`HKCU\Software\Classes\ptyro`, with
    `URL Protocol` and `shell\open\command`), for any AppUserModelID, PowerShell's included.
    Buttons work the same way (`<action activationType="protocol" arguments="<url>">`): the
    Approve and Deny links open in the browser. A toast's `Tag` (≤ 64 characters) makes the
    next one for the same session replace it.
  - `ptyro.exe` registers both itself at start (the native daemon, and any client command but
    `uninstall`), writing only what differs, so an install that moved fixes itself; never from
    a sandbox (`PTYRO_HOME`, as tests run). `ptyro uninstall` removes them.
  - Links are `ptyro://attach/<session>[?remote=local|wsl://<distro>]`. A browser can hand
    Windows any `ptyro:` link (it asks first), so `open-url` follows only that shape: an address
    of `[A-Za-z0-9._-]` not starting with `-`, a remote of those two forms; anything else opens
    nothing. A click runs `ptyrow.exe open-url "%1"`, which opens a Windows Terminal tab
    (`wt.exe -w 0 new-tab -- ptyro.exe [--remote R] attach <session>`; `-w 0` makes a window
    when none is open), else a console window of its own. Seen: a WSL session opened in a tab
    from its link.
  - The toast script checks the registry itself (`Test-Path`), so a WSL daemon's toasts become
    ptyro's the moment `ptyro.exe` has run on that Windows, with nothing cached.
- **Task Scheduler** [V, 2026-11-05]: `schtasks /Create /XML` reads a UTF-16 file with its byte
  order mark. `schtasks /Query /XML` gives the principal's user as a SID and the trigger's as
  `DOMAIN\user`, and leaves out `RunLevel` when it is `LeastPrivilege` (the default). A
  per-user `LogonTrigger` with an `InteractiveToken` registers without elevation.
- **Windows hosts over ssh (WIN11)**: Windows' OpenSSH server hands a command to `cmd.exe` (its
  default) or to PowerShell (`DefaultShell`); neither has `sh`, both run `ptyro proxy` as it
  is (install.ps1 put its folder on the user's `PATH`, which an ssh session gets). Both answer
  `echo %OS% $env:OS` with `Windows_NT` in their output, a POSIX shell with neither, whatever the
  locale (the "not recognized" message is localized). [S, Microsoft's OpenSSH docs; the probe's
  answers V in cmd and PowerShell 5.1 on Windows 11]
- **What running every test on Windows found (WIN13)** [V, 2026-11-06, CI's `windows-latest`
  and Windows 11 build 26200]:
  - **Paths as patterns see `/`.** A tool's file comes as `src\api.rs` on Windows, and leases,
    guards, change-set fences and a profile's `match.paths` are written with `/`: none of them
    matched until both sides were spelled with `/` (`os::slash_path`).
  - **An address typed as `C:\…` is a path** (`os::looks_like_path`): `--project C:\src\app`
    was looked up as a project's name.
  - **Git Bash reports its folder as `/c/Users/…`** (OSC 7); the native daemon reads it as
    `C:\Users\…` (`os::reported_folder`).
  - **`std::fs::canonicalize` gives `\\?\C:\…`,** which git can't read a config at, nor an
    agent a folder: ptyro's own `os::canonicalize` drops the prefix, product code and tests.
  - **`curl -o /dev/null` fails on Windows after a good answer**: `NUL` (`os::DEV_NULL`).
  - **MSYS programs expand an unquoted `{…}` themselves**, and Rust leaves an argument without
    a space unquoted: compact JSON (`{"type":"x"}`) reached Git Bash's `sh` as an empty
    argument (reproduced with Git for Windows 2.x's `sh.exe`). ptyro quotes every argument of a
    program it chains on Windows (`os::quoted_args`).
  - **A console's input handle is signalled for every record** (focus, window size, a key let
    go), not only for a character: `input_ready` takes the others off and waits on.
  - **Windows keeps a file in use for a moment** after the process that had it exits (os error
    32): removing a folder right after the daemon stops is tried again for a few seconds.
  - **A link to a folder is removed with `remove_dir`** there, not `remove_file`.
  - **A cooked console's line editor** takes ESC as "clear the line": a bracketed paste typed
    into a program reading lines in cooked mode loses its markers and new lines. Programs that
    ask for bracketed paste read raw VT input (real agents do; so does the fake agent then).
  - **`daemon stop` without `--force` refuses while sessions run**: the testkit's stop on Windows
    (no SIGTERM there) is `--force`, else the kill after it lost unsaved state.
  - **A daemon's health** reads the working set (`K32GetProcessMemoryInfo`) and the handle count
    (`GetProcessHandleCount`): `ps` and `/proc` aren't there.
- **A named pipe is never `stat`ed.** Asking for a pipe's attributes (`Path::exists` on
  `\\.\pipe\…`) opens it, which can take an instance a client is waiting for; a server the
  daemon started watches the daemon's process instead (`PTYRO_DAEMON_PID`, `WaitForSingleObject`).
  [S]

### 20.13 ConPTY (WIN0.2, WIN0.3) **[verified 2026-11-03: Windows 11 build 26200.9711 (25H2), x64; inbox conhost 10.0.26100.1; conpty.dll and OpenConsole.exe 1.25.2609.30003 from the NuGet package Microsoft.Windows.Console.ConPTY 1.25.260930003 (MIT, Microsoft-signed)]**

`make spike-conpty` (`spikes/conpty`, `scripts/spike-conpty.sh`) plays what ptyro needs through
both ConPTYs: Windows' own (`CreatePseudoConsole` in kernel32, the conhost in System32) and the
newer one Microsoft ships as a package (`ConptyCreatePseudoConsole` in conpty.dll, which runs the
OpenConsole.exe beside it), each with and without `PSEUDOCONSOLE_PASSTHROUGH_MODE`. A host
process records what comes out; a child writes each case and logs what reaches it. The report
judges the output with ptyro's own sniffer and emulator; the full tables are in
`spikes/conpty/REPORT.md`. The results held across five runs. [V]

- **Two different machines.** The inbox conhost keeps its own screen buffer and paints it into
  the pipe. The packaged one passes the program's bytes through as they are: 2 MiB of coloured
  lines came out byte for byte. `PSEUDOCONSOLE_PASSTHROUGH_MODE` (`0x8`) changes nothing on
  either: the inbox conhost accepts the flag and goes on painting, and the packaged one passes
  bytes through without it. [V]
- **What each writes first.** Inbox: `\e[?9001h\e[?1004h\e[?25l\e[2J\e[m\e[H`. Packaged:
  `\e[1t\e[c\e[?1004h\e[?9001h` (it asks the terminal for DA1, but doesn't wait for the answer:
  the child's first output came 31 ms after the spawn, against 12 ms for the inbox one). Both ask
  for win32-input-mode (`?9001h`) and focus reports (`?1004h`) for themselves, and turn both off
  when the pseudoconsole closes. The inbox one also sets the title to the child's path (OSC 0). [V]
- **Sequences a program writes, inbox:** OSC 133 is understood and stays in place. Every other
  OSC ptyro reads (7501, 9, 777, 9;4, 9;9, 7, 52, 21337, 99), the mode switches (2026, 2004,
  1000/1006, 1004, kitty's `>1u`), DECSCUSR, the tmux DCS wrapper and APC pass through, but at
  once, ahead of the text written before them, which waits for conhost's next paint. OSC 8 gets
  an `id=`, OSC 2 comes out later as OSC 0, sixel is dropped, and SGR is re-encoded: it looks the
  same, but a foreground colour is left on blank cells. **Packaged:** every one kept, in place. [V]
- **Queries, inbox:** conhost answers DA1 (`\e[?61;6;7;21;22;23;24;28;32;42c`), DA2
  (`\e[>0;10;1c`), DSR, CPR, DECRQM, DECRQSS and `CSI 18 t` itself, so the terminal never sees
  them. It drops OSC 10/11 and XTGETTCAP, and passes on XTVERSION, kitty's `CSI ? u`, the OSC
  7501 probe and `CSI 14/16 t`. **Packaged:** every query is passed on, for the terminal (ptyro)
  to answer, as on Unix. After a resize it asks the terminal for the cursor position (`\e[6n`). [V]
- **Keys.** A child in VT input mode reads every key and report unchanged (text, Enter, cursor
  and function keys, Alt, Ctrl, UTF-8, bracketed paste, SGR mouse, focus, kitty CSI u), through
  both. A child reading console records with VT input off, as PowerShell's PSReadLine and cmd
  do, gets records with the right modifiers from plain VT (Ctrl+Up, Alt+x). But NUL becomes
  Ctrl+Shift+2, not Ctrl+Space. Kitty's CSI u is dropped by the inbox conhost and arrives as
  seven junk characters from the packaged one. Win32-input-mode (`CSI Vk;Sc;Uc;Kd;Cs;Rc _`) gives
  that child exact records (Shift+Enter, Ctrl+Space) through both. The other way round, it fails:
  win32-input-mode Shift+Enter reaches a VT child that pushed kitty flags as a plain `\r`, with
  the Shift lost. [V]
- **Ctrl+C and closing.** A typed `0x03` is CTRL_C_EVENT for a child with processed input, and
  the byte for one without. `ClosePseudoConsole` returns at once (0 ms). The child gets
  CTRL_CLOSE_EVENT and exits with `0xC000013A`. [V]
- **Resizing.** The inbox conhost repaints the screen after a resize (1–2 KB, erase and home), and
  when it does depends on timing. Replayed into ptyro's emulator, 60 lines and three resizes
  ended with 28 to 48 lines of history, against the program's own 37: lines added, or lost. The
  packaged one writes nothing but its cursor-position query, and the history is exactly 37. The
  alternate screen came through a resize intact in both. [V]
- **Recorded programs** (ptyro-term's fixtures: Claude Code twice, Codex, vim, ls): the same text
  in every row through both. Identical styles through the packaged one; through the inbox one,
  the same look (the foreground on blanks again). [V]
- **Speed.** Key to echo: at most 1 ms through either. Throughput of 2 MiB of coloured lines:
  about 6 MiB/s inbox, about 21 MiB/s packaged. [V]
- **WIN0.3: a pseudoconsole outlives the process that made it, if another holds it.** An `HPCON`
  is a pointer to three handles, `{ hSignal, hPtyReference, hConPtyProcess }` (microsoft/terminal
  `src/winconpty/winconpty.h`), in both ConPTYs; the third is conhost.exe's or OpenConsole.exe's
  process. The creator exited without `ClosePseudoConsole`. A process that had our two pipe ends,
  those three handles and the child's process handle kept the session going: it got them either
  inherited at its creation (`PROC_THREAD_ATTRIBUTE_HANDLE_LIST`) or copied into it while it ran
  (`DuplicateHandle`), which both worked. The child's output kept coming, typing reached it, and
  `ResizePseudoConsole` and `ClosePseudoConsole` worked on an `HPCON` rebuilt from the three
  handles (allocated on the process heap, which `ClosePseudoConsole` frees). Held by nobody, the
  child and its conhost died with the creator. **The one trap:** the creator's pending
  `ReadFile` on the output pipe swallowed a chunk as it exited, in 2 of 10 runs. A handover must
  cancel its reader (`CancelIoEx`) and pass on what it already read. The layout is internal, but
  it hasn't changed since Windows 10 1809, and bundling conpty.dll pins it. [V]
- **Built (WIN5, 2026-11-04)** [V on Windows 11 25H2, x64, unless marked]:
  - `ptyro-pty`'s tests run natively there: 18 with the packaged ConPTY beside the test binary,
    17 with Windows' own (the OSC-order test needs the packaged one). They cover a program's exit
    code, typing and resizing (`mode con` reports 100 columns), Ctrl+C reaching `ping`, a stop
    that ends a program and the one it started, a `.cmd` on the PATH given `50% & more`, and a
    missing program named in the error.
  - The pinned package (Microsoft.Windows.Console.ConPTY 1.25.260930003, SHA-256
    `02b07b34…ae7ee`): its four files are Microsoft-signed (`Get-AuthenticodeSignature`).
  - A release `ptyro.exe` with the packaged ConPTY beside it: `--remote local` started a daemon
    on `\\.\pipe\ptyro-<hash>`; `new`, `read`, `ls --json` (the Windows cwd) and `kill` worked,
    and a session saw `PTYRO=1`, `TERM=xterm-256color`.
  - An anonymous pipe can't be overlapped, so each session's pipes go through tokio's blocking
    pool (one waiting read per session). The console host keeps the output pipe open after the
    program exits: the console is closed once the job is empty, and that ends the output.
  - A process started with an empty `STARTUPINFO` std handle set (`STARTF_USESTDHANDLES`, none
    given) writes to its console even when the daemon's stdout is a pipe.
  - Smart App Control refused the unsigned debug `ptyro.exe` that carries the daemon, and ran
    the release build (§20.12).
- **Built (WIN7, 2026-11-04)** [V on Windows 11 25H2 with a release `ptyro.exe`]:
  - A shell session running `ping` reports `ping` as its foreground program, with its
    directory (read from the PEB at `ProcessParameters` 0x20, `CurrentDirectory` 0x38, the
    64-bit layout); a session's PowerShell listening on a port has it in `ls --json`.
  - Windows refuses symbolic links without Developer Mode (`ERROR_PRIVILEGE_NOT_HELD`, 1314):
    a folder is then linked as a junction (`mklink /J`, no privilege needed) and a file as a hard
    link, or copied across drives; the notes say which.
  - `canonicalize` gives `\\?\C:\…`; ptyro drops the prefix (the standard library adds it back
    for long paths) and compares Windows paths without case.
- **What it decides** (DD3.4; decided by the owner 2026-11-04, ROADMAP WIN0.5 d and g) **[decided]**:
  - **The PTY: (c).** Bundle the packaged conpty.dll and OpenConsole.exe: about 1.2 MB per
    architecture, MIT, signed by Microsoft. Use the inbox ConPTY only when they are missing. The
    packaged one is the only one that keeps ptyro's side channels in order, lets ptyro answer
    queries (OSC 10/11 for the outer colours, `CSI 16 t` for Claude Code), keeps sixel, leaves the
    scrollback alone on resize and is 3.5 times faster, and it pins the `HPCON` layout.
  - **Keys:** a Windows session sends kitty CSI u when its program has pushed kitty flags (ptyro's
    emulator sees the push pass through), and otherwise win32-input-mode, which conhost asks for.
    Plain VT works for everything but the keys only those two can tell apart. Sending CSI u to a
    program that reads records breaks it.
  - **Live upgrade:** possible without holder processes. The old daemon hands the new one, by
    inheritance as `handoff.rs` does on Unix, each session's pipe ends, its three `HPCON` handles,
    the child and its job object, and stops reading first. The owner chose this (WIN0.5 d,
    2026-11-04) over restart and resume, which stays as the fallback when a handover fails.

### 20.9 Chrome shows a service worker's certificate, not the connection's

**Seen 2026-10-29** on Android Chrome, after `serve` moved from ptyro's own certificate to
Tailscale's (TS1) for `bear.<tailnet>.ts.net:7091`: a normal tab's certificate viewer showed
ptyro's own (`ptyro serve (bear)`, issued by `ptyro local CA (bear)`, trusted because that phone
had the CA installed), while an Incognito tab on the same address showed Let's Encrypt. A packet
capture on this side showed that tab's fresh handshakes (no resumption) carrying SNI
`bear.<tailnet>.ts.net`, which `serve` answers with Tailscale's certificate (checked with
`openssl s_client`, also with Chrome's ALPN and post-quantum key share).

**Inferred, not verified in Chromium's source:** for a page its service worker controls,
Chrome's page info shows the certificate recorded when that worker's script was installed. A
worker is replaced only when its script's bytes change, so a new server certificate alone never
replaces the record. Hence `serve`'s certificate stamp at the end of `/sw.js` (§16.4, PWA).
Clearing the site's data in Chrome also clears it (and the pairing with it). **Not verified yet:**
that the stamp makes a phone that already has the old worker show the new certificate.

**Seen the same day, after the site's data was cleared and the phone paired again:** the
certificate details showed Let's Encrypt, but the page still said "Connection is not secure" and
Install said "This app cannot be installed" (Chrome installs only pages it rates secure). A fresh
headless Chrome on the Mac, over the same tailnet, rated the page `secure` with no installability
errors (DevTools protocol `Security.visibleSecurityStateChanged`, `Page.getInstallabilityErrors`),
and every asset is same-origin over HTTPS. **Force-stopping Chrome on the phone fixed both at
once** (Install offered straight away, the certificate right). So Chrome keeps some per-site
security state for as long as it runs, which deleting cookies and site data doesn't clear. Which
state it was is not known (a guess: content from an earlier certificate counted against the
host). The fix for users is in `docs/remote.md` (Tailscale's certificate).

### 20.14 tmux's windows, the model SHELLS borrows from **[V-local 2026-11-04: tmux 3.6b, a fresh server with `-f /dev/null`]**

- Prefix table: `c` new-window, `0`…`9` `select-window -t :=N`, `n`/`p` next/previous, `l` last
  window, `w` `choose-tree -Zw`, `,` rename-window, `&` kill-window (asks). `<` is a window menu
  (swap left/right, kill, respawn, rename, new after, new at end).
- Defaults: `base-index 0` (the first window is 0), `renumber-windows off` (closing window 2
  leaves 0, 1, 3), `automatic-rename on` (the name follows the running program),
  `monitor-activity off`, `monitor-silence 0` (off), `monitor-bell on`.
- `new-window [-c start-directory] … [shell-command]`: without `-c` a window starts in the
  session's directory, not the current pane's. Users who want the pane's bind
  `new-window -c "#{pane_current_path}"`.
- ptyro's choices against these (§23): numbers start at 1 (`Alt-1` is the key nearest the left
  hand, and `Alt-0` is the tenth); numbers stay (as `renumber-windows off`); labels follow the
  program but names stay (ptyro's name is an address, §2.3); the pane's directory by default;
  long-command notifications on by default, told by OSC 133 exit codes instead of output guesses.

### 20.15 Installers and newest-version feeds (BOOT0) **[V/S 2026-11-06: read from each vendor's docs and installer scripts; the version table §24.1 holds the per-value marks]**

- **Claude Code:** the native installer (`curl -fsSL https://claude.ai/install.sh | bash`;
  `irm https://claude.ai/install.ps1 | iex`) is the recommended way on every platform, x64 and
  ARM64, and needs no admin. It updates itself in the background; `claude update` applies one now.
  Its newest version is the plain-text body of
  `https://downloads.claude.ai/claude-code-releases/latest` (what `install.sh` reads; `stable` is
  beside it). `claude auth status` exits 1 when logged out. The npm package needs Node.js 22+.
- **Codex:** the docs now lead with a standalone installer (`curl -fsSL
  https://chatgpt.com/codex/install.sh | sh`; `irm https://chatgpt.com/codex/install.ps1 | iex`),
  which needs no Node.js and installs into `~/.local/bin` (`~/.codex/packages/standalone`) or
  `%LOCALAPPDATA%\Programs\OpenAI\Codex\bin`. Updating is running it again (`codex update` exists
  too). Its feed, `https://releases.openai.com/codex/channels/latest`, is GitHub-release JSON with
  `tag_name` = `rust-v<version>`. `codex login status` exits 1 when logged out.
- **Antigravity (`agy`):** its installer (`…/cli/install.sh | bash`, `…/cli/install.ps1 | iex`) puts
  `agy` in `~/.local/bin` or `%LOCALAPPDATA%\agy\bin` and it updates itself in the background
  (`AGY_CLI_DISABLE_AUTO_UPDATE=true` stops that). The installers read
  `https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/<os>_<arch>.json`
  (`linux_amd64`, `darwin_arm64`, `windows_arm64`, `linux_amd64_musl`…): `{version, url, sha512}`.
  Sign-in happens when `agy` starts; no status command is documented.
- **OpenCode:** OpenCode 2 (`@opencode/cli`, `opencode.ai/v2/install`, beta) replaced the server
  API ptyro's adapter speaks (§8.6) and shares the `opencode` command, so the table installs
  OpenCode 1: `curl -fsSL https://opencode.ai/install | bash` (into `~/.opencode/bin`), or
  `npm install -g opencode-ai` on Windows (the docs recommend WSL there). Its newest version is
  the npm registry's `opencode-ai/latest`; `opencode upgrade` updates it.
- **Gemini CLI:** `npm install -g @google/gemini-cli`, Node.js 20+ (its `engines`, and the docs).
  No login status command.
- **GitHub CLI:** the docs warn off Debian's and Ubuntu's own `gh` (2.45.x/2.46.x broken); its
  own apt and dnf repositories, Homebrew, and `winget install --id GitHub.cli --source winget`.
  `gh auth status` exits 1 when logged out.
- **Node.js:** `https://nodejs.org/dist/index.json` lists releases newest first, each with `lts`
  (`false` or the line's name); 26.13.0 was the newest LTS on 2026-11-06, above a 27.0.0 alpha.
  Builds are `node-v<v>-<linux|darwin|win>-<x64|arm64>.<tar.xz|zip>`, with `SHASUMS256.txt`.
  WinGet's `OpenJS.NodeJS.LTS` is in winget-pkgs; nodejs.org names no winget command.
- **Moving between installers keeps settings and logins (BOOT7, 2026-11-07):** each agent keeps
  them in the home, apart from the program: Claude Code `~/.claude` and `~/.claude.json` (its setup
  page removes them in a step of its own, after any uninstall; uninstalls are `npm uninstall -g
  @anthropic-ai/claude-code`, `brew uninstall --cask claude-code`, `winget uninstall
  Anthropic.ClaudeCode`) [V]; Codex `~/.codex/config.toml` [V config-basic], its `auth.json` in the
  same `CODEX_HOME` [S]; Gemini CLI `~/.gemini` [S]; OpenCode `~/.config/opencode` and
  `~/.local/share/opencode` [S]. Claude Code's npm package installs the same native binary as the
  native installer [V]. If `claude` still runs after an uninstall, the docs point at a second
  installation or a leftover alias [V]: the reason a move checks what runs afterwards (§24.9).

## 21. Open questions

1. ~~**License**~~ — **decided 2026-10-20: MIT** ("for now"; `LICENSE` at repo root, `license =
   "MIT"` in every crate). MIT is compatible with alacritty_terminal's Apache-2.0. Relicensing to
   dual MIT OR Apache-2.0 later stays easy while the owner is the only contributor.
2. ~~**Default prefix key**~~ — **decided 2026-10-20: `Ctrl-Space`; changed 2026-10-22 to
   `Ctrl-Q`**, configurable via `[ui] prefix` (§13.2). Why the change: on most Macs Ctrl-Space is
   "select previous input source", so the default never arrived. Why `Ctrl-Q`: it is the only
   Ctrl+letter that none of the four agents binds (survey in §20.5). It is also a letter, so it
   works on layouts where `]` and `\` need AltGr. Its usual meaning (XON) is moot, because raw mode
   turns flow control off. Readline and Vim only use it as a second key for `C-v`. Rejected:
   `C-b` (Claude and Gemini use it to background tasks), `C-a` (line start; OpenCode's model list),
   `C-x` (OpenCode's leader key), `C-]` / `C-\` (AltGr layouts; Vim tag jumps; SIGQUIT).
   Caveats: VS Code's terminal on Linux and Windows and Zellij's default keys take `Ctrl-Q`.
   **Same day, the owner asked for both:** `[ui] prefix` takes a list, and the default is
   `["C-q", "C-Space"]`. Each key covers the other's gaps: macOS takes Ctrl-Space, while VS Code
   and Zellij take Ctrl-Q. A list the user sets replaces the default. The user-facing wording is in
   `docs/keys.md`. `ptyro init` asks for comma-separated keys and warns only when `C-Space` is the
   sole prefix on macOS.
3. ~~**Codex & Gemini hook injection**~~ — **decided 2026-10-20:** Codex entirely through `-c` flags, nothing written to `~/.codex`; Gemini through a ptyro extension linked once, opt-in (§8.4, §8.5, M0.4/M0.5).
4. ~~**Claude hooks injection**~~ — **decided 2026-10-20:** `--plugin-dir`; both mechanisms are additive (§8.2, M0.3).
5. **Should the TUI offer a raw-passthrough fast path** when it is the only client and sizes match
   (enables image protocols like kitty graphics)? Deferred until someone needs images.
6. ~~**Workspace semantics**~~ — **decided 2026-10-20:** workspaces carry a view, an attention
   policy (§9.6) *and* a profile (§10.10); the owner runs several accounts side by side, switching
   today via shell aliases with separate config dirs (→ `ptyro init` adoption, §10.10). `default`
   becomes `isolate = true` once any profile is defined (decided 2026-10-20).
7. ~~**Antigravity CLI** adapter~~ — built from its docs as M4.21 (§8.6b); unverified against a real install.
8. **Change sets (§15.5)** — **direction decided 2026-10-20:** one lead plus one agent per member
   repo, validated by the hands-on experiment M11.0 before the machinery is built. Still open:
   should the lead be allowed to edit member code in `solo` layout only, or never; do the contract
   checks belong in the template or in each repo's `.ptyro.toml`; and how deep should the merge
   train integrate with forge-native merge queues?
9. **A resident summariser ("concierge")** — the UX1.3 spike's option B (owner's idea,
   2026-10-22): one long-lived, cheap structured session per profile, fed turn digests, that
   could answer "what's going on?" across sessions or act as the default lead. Per-turn
   summaries don't need it (§11.2h, §20.1); revisit when cross-session questions are wanted.
10. **Chat sync and merge across machines (CHAT6)** — the owner wants it, 2026-10-29; needs a
    brainstorm of its own. §22.15 lists what the schema already guarantees and what is open.
11. ~~**Chat meaning search (embeddings)**~~ — **built opt-in 2026-10-30** (CHAT5, §22.10 tier 3):
    model2vec `potion-base-8M` (31 MB, downloaded only when `[chat] meaning` is switched on). Still
    open: its quality on real chats, and the 0.3 cosine floor ([U] until tried).
12. **Chat tool restriction per agent** — answered by CHAT0 for Claude and Codex (§20.8, §22.5);
    open for Gemini and OpenCode, whose native rules are read but were not run (§22.17).


---

## 22. Chat: general-purpose conversations **[decided 2026-10-29 (owner, ROADMAP CHAT); CHAT1–CHAT3 built 2026-10-29, CHAT4 and CHAT5 built 2026-10-30, CHAT6 proposed]**

**As built (CHAT1, 2026-10-29), and where it differs from the design below:**
- **Store** (`ptyro-store/src/chat.rs`): the §22.4 schema exactly, its own thread, 0600, a newer
  schema refused untouched; tombstones compacted a day later (and `archive_purge_days`), a
  streaming node closed `interrupted` at start. So is a turn cut off before any of its answer
  arrived (the shown branch ends after the person's message with no answer and no end): a notice
  that says to retry, then the `interrupted` end; its running tools are closed failed and its
  open permission cards `cancelled`, as nothing can answer them now. `ptyro doctor` reports it, `ptyro backup` copies it
  (`--no-chats` leaves it out), `ptyro uninstall`'s plan names it. A removed workspace's chats go
  to `main`.
- **Engine** (`ptyro-daemon/src/chats.rs`): a structured session with `Place::chat`, in
  `<data>/chat/<id>/`, under the chat's profile, launched with the §22.5 line per agent (Claude
  `--tools`/`--allowedTools`/`--strict-mcp-config`/`--safe-mode`/`--system-prompt`; Codex
  `thread/start` fields through the driver's new `Opts.params`, every `codex mcp list` server off;
  a Gemini `--policy` file; OpenCode's permission ruleset). Engines are hidden by the clients
  (`SessionInfo.chat`), never restored as dormant, never summarized, never hibernated by
  `[agents]`: `[chat] hibernate_after` stops them. Native continue by conv; a conv whose resume
  produced no answer is ended and the message is re-sent seeded. Codex's driver ends a turn on
  `thread/status/changed → idle` (CHAT0). Attention follows `[chat] notify`.
- **Rules the review added (2026-10-29):** one answer at a time: `ChatSend` while the engine
  works or waits on a permission card is refused (`Busy`, the client keeps the text), so a message
  never interleaves with an answer. `ChatNew` with a prompt always returns the chat, even when its
  agent didn't start (the reason is an error notice in it); an engine that ends before it answers
  says so. A failed native resume writes nothing and raises nothing (`quiet`) before it is
  re-sent seeded; hibernation and the retry take the chat's send lock. Engines that crossed a live
  upgrade are stopped at start (their cut-off answer is closed `interrupted`). A Codex `read` chat
  always gets the folder-limited profile, re-allowing the profile's `CODEX_HOME` and the `codex`
  binary's folder; every `codex mcp list` server is switched off whatever its case, and when the
  list can't be read the chat says `tools: best effort`. `chat.db` and its copies are 0600 every
  time; a write after a delete is dropped; `archive_purge_days` removes the purged chats' files;
  `ptyro backup` reads `chat.db` read-only; doctor uses `quick_check`. `[chat] notify = "all"` adds
  only the chat's own "answered". A machine can't be named `c`; another machine's archived chats
  are routed there once listed.
- **Not built in CHAT1** (each is its later task): incognito, `history_tool`/`history_scope`
  (CHAT3), the presets' `shell` and `mcp` (CHAT4), agent-sent titles (Claude and Codex send none;
  OpenCode's and ACP's are not read yet: the titler is used), a `parent` on `ChatSend` and every
  branch key (CHAT2), `C-g`, `make verify-chat`, and the search fan-out of §22.10 (CHAT3).
- **Machines** (CHAT1.9, `machines.rs`, `conn.rs`): a link's mirror gains `chats` (non-archived
  `ChatInfo`, read with `ChatList` on (re)connect and kept by `ChatUpdated`/`ChatRemoved`, which
  reach clients with `machine` set); `ChatInfo`'s `workspace` and `profile` take the prefix, ids
  stay. `chat` is an address key, so `ChatSend`, `ChatRead`, `ChatUpdate`… about a mirrored chat
  (by `laptop2:c:7f3k`, its id, or a handle only it has) go over the link, and `ChatNew` in
  `laptop2:work` is made there. `ChatList` gains every mirror's shown chats; archived ones are
  asked of each online machine (they are not mirrored). `ChatSubscribe` to a mirrored chat goes
  over the client's own upstream and `ChatLive` comes back through it; an offline machine answers
  it with a `ChatLive` notice ("laptop2 is offline since 14:02; its chats open when it is back"),
  and every request with the link's offline message. Hiding a workspace hides its chats; removing a
  machine removes them. Alerts already arrive as relayed inbox entries. Clients grey an offline
  machine's chats (`chat ls` says "(offline)"; `--machine M` filters).
- **Branches and forks (CHAT2, 2026-10-29):** `put_node` moves `leaf` only when the new node
  extends the shown branch, so a retry or edit sets it explicitly. **Retry** (`ChatRetry{chat,
  node}`) re-asks the user message above `node` as a new answer sibling; **edit** is its own request
  (`ChatEdit{chat, node, text}`, not `ChatSend.parent`) that adds a user sibling. Both stop the
  engine and start it seeded with the branch above the message (a native conv can't rewind).
  `ChatBranch{chat, to}` shows the branch through `to` (a node, its tip followed down);
  `ChatTranscript` and `ChatLive` carry `siblings` (`{node, at, of, ids}`) for every message that has
  them, drawn as `‹ 2/3 ›`. **Fork** (`ChatFork{chat, node?, agent?, side}`) copies the branch to
  `node` with new ids; it is native (`--resume X --fork-session`, conv `fork_pending`) only for
  Claude, the same agent and profile, when the source conv ends exactly at the fork point; every
  other fork is seeded. **Side chats** carry `side_of` (schema v2, `ChatInfo.side_of`, `↳`), are
  never listed (they show under their parent while open) and are deleted when closed unkept;
  `ChatUpdate.keep` makes one a normal fork. **Incognito** (`ChatNew.incognito`, `[chat] incognito`,
  `Tab`/`C-i` in a new chat) keeps its record in the daemon's memory only; retry and edit there truncate instead of
  branching. No chat engine is recorded (`pty.log`) or saved as a session row in `ptyro.db`
  (CHAT1 made its first message the row's title); rows left from then are removed at start.
  A chat's agent is confined before any routing: it can't reach another machine's chats, and an
  ambiguous or out-of-scope name is simply not found. A native fork stays `fork_pending` until its
  first answer has its own conversation, so a restart before then forks the source again. Node ids are shown and taken by their last 8 characters (any unique suffix of 6+).
  Slash commands in the TUI and web composer: `/retry /edit /fork /side /keep /agent /tools /title
  /search /find /export /incognito /branch /cite /help`.
- **Search (CHAT3, 2026-10-29):** `ChatSearch{query, chat?, workspace?, agent?, pinned, archived,
  limit, machines}` → `ChatHits{hits}` (`ChatHit{chat, node, kind, snippet, at_ms, rank}`; `rank` is
  bm25×1000, user messages weighted ×1.3, titles matched too; marks are `\u{2}`/`\u{3}`, which each
  client turns into its own highlight). `core::chats::parse_query` quotes every word so FTS syntax
  can't leak: `"a phrase"`, `-word`, `prefix*`, `agent:`, `since:7d`, `workspace:`/`ws:`; a query
  of negatives only is refused. With no chat or workspace filter it fans out to every online machine
  and interleaves round robin. **Ask:** `ChatFind{question, agent?, workspace?}` returns the
  incognito chat it started (`Chat{info}`, not a `ChatAnswer`), whose engine has only the history
  tools; it shares the title cap (`chat-cap:<profile>`). The history tools are `ptyro mcp --chat`
  (`chat_search`, `chat_read`, `chat_list`, also in leads whose run policy has `chat = true`, through
  `PTYRO_CHAT_TOOLS=1`). The daemon knows a caller is an engine by its pid's session; an engine may
  only search, read and list, scoped by `history_scope`, never itself or an incognito chat. Claude
  engines get the line of §20.8 instead of `--safe-mode`; Codex gets `mcp_servers.ptyro` in the
  thread's config; Gemini engines get them in `session/new` since CHAT4; OpenCode engines get none yet. Not built: `--machine` on
  search (the fan-out and `--here` are), the trigram index for CJK.
- **Tools and tasks (CHAT4) and Gemini (2026-10-30):** `ChatTools` gains `shell` and `mcp`
  (schema entries; a built-in preset with `shell` is refused, an MCP name with `_` too).
  `ChatTools::describe` is each preset's one line, shown by `/tools`, every picker, the settings
  panel and `ptyro chat presets` (`ChatPresets`, 1.24). **Folders** are the chat's own:
  `ChatNew.folders`, `ChatUpdate.folders` (all of them; empty clears), `chat.db` v4 (`chat.folders`,
  JSON), `ChatInfo.folders`; the daemon makes them full paths that exist, a fork copies them, and a
  chat given folders with no preset asked for gets `read`. A preset that reads uses them with its
  `dirs`. `ChatInfo.best_effort` and `.shell` are worked out per agent when a row is sent (§22.5's
  table), and the clients show `tools: P · best effort · ⚠ shell` (a shell chat's row has `⚠`, as a
  `dangerous` session's). **Task:** `ChatTask{chat, project?, agent?, name?, confirmed}` →
  `WorktreeCreated`: `create_worktree` with the chat's agent (the project's profile), the title, and
  a first prompt of the shown branch's seed block, "The plan from that conversation (chat c:…, its
  last answer)", the answer and "Carry this out in this repository."; the chat gets a notice naming
  the worktree and the session. **What moves where:** a task is the person's explicit act of
  carrying the conversation into coding work, so its text goes where a session's does: typed into
  a terminal session (its scrollback and recording), the chat's title into the worktree's (in
  `ptyro.db`), and into the coding agent's own history (which `chat import` may later read back).
  So: an **incognito chat can't become a task** (refused: "`chat export` copies it out, or ask it
  again in a chat that isn't incognito"; the web hides the item), nor an imported one (the fork
  advice); and when the profile the task would run under (as `create_worktree` resolves it: the
  agent's `@profile`, the project's pin, the workspace's) isn't the chat's, the daemon asks first
  (`NeedsConfirmation`, "c:… is a chat of profile personal, and the task in alpha would run under
  work (pinned): carry the conversation over to that account?"); `confirmed` (CLI `--yes`, the
  TUI's `y`, the web's sheet) goes ahead, and also answers a profile guard's question (it no longer
  skips one). It waits its turn like a message: refused while the chat answers or compares. The project defaults to the only one of the chat's workspace (or the only one there is);
  otherwise the error lists them with `--project`, and the TUI and web ask with a picker. The link
  back is that notice and the worktree's title (the chat's): `SessionInfo` has no `forked_from_chat`
  (27 places build it, and `ptyro.db` would need a column). **Gemini:** a chat runs with
  `--admin-policy F --policy F --extensions none --allowed-mcp-server-names <ptyro and the preset's
  servers, or "">` (and `--include-directories` per folder when it reads); the history tools reach any
  ACP agent through `Opts.params.mcpServers`, which the driver merges into `session/new` and
  `session/load` (no new `Opts` field: `params` already is "more fields for the call that starts or
  resumes the conversation"). Another ACP agent gets no Gemini flags. Deleting a Gemini chat, or an
  incognito one ending, removes Gemini's own folders for its scratch directory (`.project_root`).
  Deleting any chat (or `archive_purge_days` purging it: `compact` returns each with its agent and
  profile) removes the copy of every agent that served it (each conv's agent and profile): Claude's
  project folder, Gemini's folders, and Codex's rollouts (`sessions/YYYY/MM/DD/rollout-*-<thread
  id>.jsonl` and `archived_sessions/`, by each conv's `conversation_id`, only when the file's first
  line names the chat's id, so no other conversation is touched).
  **Finding agents:** `ptyro_agents::locate` finds a program that isn't on `PATH` in the npm prefix,
  `~/.npm-global/bin`, `~/.local/bin`, `~/.volta/bin`, `~/.bun/bin`, `~/.claude/local`, Homebrew,
  nvm's node versions (newest first) and, for the npm-shipped built-ins, `npx`'s cache
  (`~/.npm/_npx/*/node_modules/<package>`, newest version; only a `package.json` whose `name` is
  that package, and a `bin` script inside it: relative, no `..`), running a Node script with the `node`
  beside it when none is on `PATH`; ptyro never downloads one. Sessions, chats, `doctor`
  (where, and how: `(nvm)`, `(npx cache)` with a note to `npm i -g`), `AgentChoice.found` (web and
  TUI agent pickers) and the first-run screen use it; no agent at all and an asked-for agent that
  isn't installed offer setup (`ptyro setup`, `ptyro install <program>`; BOOT9, §24.10). **On Windows (WIN8):** a program is found by
  `PATHEXT` (`claude.exe`, `codex.cmd`; never npm's extensionless `sh` shim beside the `.cmd`),
  with `%USERPROFILE%` as the home when `HOME` isn't set, in the npm prefix (the prefix itself, not
  `bin`), `%APPDATA%\npm`, `~\.local\bin` (Claude's native installer), WinGet's `Links`, Scoop's
  `shims`, `%NVM_SYMLINK%` (nvm-windows), `~\.bun\bin` and `npx`'s cache under
  `%LOCALAPPDATA%\npm-cache\_npx`, and `%LOCALAPPDATA%\agy\bin` (the Antigravity CLI's installer).
  An npm shim whose package is gone (a removed package, or an install that rolled back) runs
  nothing and isn't found. An npm `.cmd` shim is read and what it starts runs instead: its
  last quoted `"%dp0%\…"`, inside its folder only, a `.js` with `node` (the shim's own `node.exe`
  first, as cmd-shim picks), anything else as it is. Through the shim, `cmd.exe` would stand
  between ptyro and the agent, ask "Terminate batch job (Y/N)?" at Ctrl+C, and be the session's
  program. A shim that can't be read runs as itself (WIN5 wraps it in `cmd.exe /d /c`). Claude's
  install hint is `irm https://claude.ai/install.ps1 | iex` there (every hint is the version table's, §24.1). **Not built:** an `@` completion of the folders' files
  in the composer; agent-sent titles (no agent of the four sends one: §20.8); MCP for OpenCode chats
  (its server takes MCP servers per server instance, `POST /mcp`, and it isn't installed here to
  try; its permission rules for `<server>_*` are written [S]).
- **Meaning search (CHAT5, 2026-10-30):** opt-in `[chat] meaning`, model2vec `potion-base-8M` in
  pure Rust (`crates/ptyro-meaning`), int8 vectors in schema v3 `node_vec`, a background backfill,
  RRF with the keyword hits, `by:meaning`/`by:words`, `ChatHit.how` and `≈`. See §22.10 tier 3.
- **Compare (CHAT5, 2026-10-30; `ptyro-daemon/src/chats/compare.rs`):** `ChatCompare{chat,
  agents, text?, confirmed}` (1.24; `ChatNew.compare`/`confirmed` for a new chat's first message)
  asks each agent the same message, and each answer is a **sibling branch under it**. **Profiles:**
  a bare agent name is the **chat's own profile's** (`claude` in a `claude@personal` chat is
  `claude@personal`, wherever the chat's workspace points), so the branch never reaches another
  profile's account, nor do a helper's history tools search another profile's chats; a name with
  another profile (`codex@work`) is asked about first (`NeedsConfirmation`; CLI `--yes`, the TUI's
  `y`, the web's sheet). Agents are counted once each after resolving (`claude,claude` or
  `claude,claude@default` is one), and a new chat's `--compare` of fewer than two is refused before
  the chat is made. With `text` it is a new message
  on the shown branch; without, the last message of the person's again, for the agents that haven't
  answered it (a new message needs two agents or more). Default agents: the chat's own, then every
  installed one with a structured protocol. The chat's own agent (same agent and profile) answers
  in its own conversation, natively where it can (§22.6), as a message would; every other agent
  answers in a **helper**: an in-memory chat as an incognito one is (so `--no-session-persistence`
  / `ephemeral`, no agent-side copy kept; Gemini's folders removed), registered in
  `Chats.helpers`, which no list, `find_chat`, `ChatUpdated`/`ChatRemoved` or alert ever sees,
  seeded with the branch above the message. Its answer (words, tool calls, cards, turn end; not
  ptyro's own info notices) is copied under the message with new ids by `put_branch`, which
  leaves `leaf` alone, and attributed by a `conv` row (`how = compare`, inserted ended: it says who
  wrote it, it is not a conversation to continue). Then the helper's engine, scratch folder and
  agent copy go, before its agent counts as in. **Labels:** `ChatSiblings.agents` (`agent`, or
  `agent@profile` off the default profile), worked out per sibling as the first node at or below it
  that a conv produced; `ChatSiblings::label` is the shown one's when they differ, so a retry's
  siblings stay unlabelled and a compare's read `‹ 2/3 codex ›` in the TUI, the web and `chat show
  --branches`. **Progress:** `ChatInfo.comparing` (`ChatCompare{node, agents: [{agent, state,
  note}]}`, `line()` = `comparing: claude ✓ · codex … · gemini ✗ why`), a live item under the
  message while it runs (not a node), and at the end a notice on the shown branch (`compared: …`
  with the hint to `/pick`). With no answer of the chat's own agent, the first other answer in is
  shown. **Rules:** one answer at a time holds for the whole compare (`busy_check` refuses a send,
  retry, edit, branch or a second compare); `ChatStop` interrupts every helper too, and one that
  hasn't stopped 10 s later is stopped (so is the chat's own engine, compare or not, with a notice
  that says so; an engine still starting after 2 minutes is ended the same way, and isn't retried);
  a helper not done in 10 minutes is stopped as `✗ timed out`, and the chat's own answer too
  (what it said so far left out); deleting the chat stops its compare, and nothing is copied into a
  chat deleted meanwhile; at the daemon's start, scratch folders of chats that no longer exist (a
  crash's helpers and incognito chats) are removed, with Gemini's copies; a helper that fails (not installed, not logged in, ends
  without an answer) is `✗` with the reason and the others go on; a helper is never shown a
  permission card: what its preset doesn't allow is **denied** (the preset is ptyro's answer
  anyway; the progress says "a tool it asked for was denied"), while the chat's own agent asks as
  ever. **Pick:** `ChatPick{chat, node?}` shows the branch through `node` (default: the shown one)
  and sets the chat's agent to the one whose conv wrote its newest node (`branch_agent`); the next
  message is seeded on it (§22.6 rule 3). Like a message, it waits its turn (refused mid-answer or
  mid-compare). Losers stay as branches (CHAT2 deletes none).
- **Import (CHAT5, 2026-10-30; `ptyro-daemon/src/chats/import.rs`):** `ChatImport{agent?,
  profile?, since?, dry_run}` → `ChatImported{report}` (1.24, scope `full`: it copies coding history
  into `chat.db`, as the setting does) reads every profile's (or one's) Claude Code, Codex and
  Gemini history (§20.8 has the formats, verified 2026-10-30) **read-only** and makes each session a
  chat: `chat.db` v5 adds `chat.source` (`claude:<session id>`, unique), `chat.source_dir` (the
  folder it ran in) and `import(source, chat, path, size, mtime, offset, tail, at)`. Its user and
  assistant text become nodes, tool calls tool nodes (their output only with `index_tools`, 2 KB),
  with the session's own timestamps; thinking, side chains, meta lines, compaction summaries and the
  wrappers around slash commands and Codex's own context are left out. Node ids are made from the
  source's ids (`node_id`: SHA-256 to 26 Crockford characters), so an import is idempotent; an
  append-only file (Claude, Codex) is read on from `offset`, the end of the last line after which no
  tool call waited for its result, parented on `tail`; Gemini's file is read whole (its patches
  change earlier messages); a file unchanged in size and mtime isn't opened. Title: Claude's
  `custom-title`, else `ai-title`/`summary`; Codex's `session_index.jsonl` `thread_name`;
  Gemini's `summary`; else the first message's first line (`title_src = agent`, so a title you
  give wins). Workspace: the one whose project or worktree contains the session's folder, else
  `main`; profile: the profile whose folder it came from (two profiles sharing a folder: the first).
  **Skipped:** sessions that ran in ptyro's own chat engines' and summaries' folders
  (`<data>/chat`, `<data>/summaries`; sessions in its worktrees, `<data>/worktrees`, are history and
  imported), sessions with nothing said, and a session whose chat you deleted (the `import` row
  outlives the chat, so a delete is never undone); a file passed over isn't read again until its
  size or mtime changes (remembered in memory). Lines over 4 MB are passed over unread: a tool
  result among them closes its call ("its output was too large to import", found by its id in the
  line's first 64 KB), and a call still unanswered 1,000 lines on no longer holds `offset` back. A
  Gemini file over 64 MB is skipped with a note. A file that can't be read, or a parser that
  panics, is skipped with a note, never the whole import. A tool node's title (its command, path
  or URL) is redacted as summaries are (`Bearer …`, `TOKEN=…`, `sk-…`). The parse runs in a
  blocking task and each session is one transaction, so the request loop and other chats' writes
  wait at most one session. Imported chats are **read-only**: `ChatSend` forks one (the fork says
  "continued as a fork of the imported c:…", and the reply names the fork, which each client then
  shows); the next message to the import carries on in its newest fork (kept, not archived)
  rather than making another; `ChatRetry`, `ChatEdit`, `ChatCompare`, `ChatPick`, `ChatTask` and `ChatUpdate`'s
  agent, preset and folders are refused with that advice (title, pin and archive are the
  person's). A re-import moves the shown branch on only from where the last import left it
  (`chat.db` v6 `import.leaf`): a person who went to another branch stays there. They are searched (the FTS triggers, the meaning backfill on `ChatUpdated`),
  obey `history_scope` (their profile), and are listed like any chat, marked `⇣` with "imported
  from claude · ~/repo" in the header; `chat ls --imported` lists only them (`i` in the TUI's chat
  list, Imported on the web's Chats page). The TUI's `chat.import` asks first in a prompt that takes
  the CLI's flags (`codex --since 30d --profile work --dry-run`; ⏎ alone: everything), the web's in
  a form (whose, profile, since, Only count them), with no request timeout. The TUI refuses retry
  and edit of one up front, as the web hides them. `[chat] import =
  true` (default off: it copies coding history) imports at once and then every 10 minutes. On
  this Mac, 2026-10-30 (debug build): 104 sessions, 11,326 messages, 785 MB of Claude history
  imported in 7.5 s, again in 0.01 s. **OpenCode is not imported**: it isn't installed here to read
  its storage, and ptyro doesn't guess a format; the report says so. ACP `session/list` isn't used
  either (Gemini 0.66.0 doesn't offer it, §22.6).
- **Configuration renamed:** `[chat] tools = "chat"` became **`[chat] preset`**: TOML can't hold
  both that string and the `[chat.tools.<name>]` tables. Built keys: `enabled`, `recent`, `agent`,
  `preset`, `hibernate_after`, `seed_tokens`, `title`, `title_command`, `daily_cap`, `index_tools`,
  `notify`, `archive_purge_days`, and `chat.tools.*.{web, read, dirs, allow}`.
  CHAT2 and CHAT3 add `incognito`, `history_tool` (default on) and `history_scope` (`profile`,
  `workspace`, `all`); CHAT4 adds `chat.tools.*.{shell, mcp}`.
- **Seeds:** the "N earlier messages left out" line is used (no summary paragraph yet). The seed
  notice sits on the branch between the message and its answer.
- **Surfaces:** CLI `chat [new]|ask|ls|show|open|send|answer|stop|retitle|pin|unpin|archive|
  unarchive|set|rm|export` (`set` is the draft's `update --agent/--tools`); TUI: the CHATS section
  per workspace, `C-q m`, `C-q g`, the chat panel over the panes, sidebar keys reusing `,` `x` `X`
  and `*`; web: a Chats tab, `#/chats`, `#/c/<id>`, `#/c/new`.

A **chat** is a plain conversation with a registered agent: no repository, no worktree, no task.
It is what the Claude and ChatGPT desktop apps give you, built on the agents ptyro already runs,
with their accounts (profiles) and without API keys. RESEARCH R-CHAT-01…17 hold the ideas and
what other products do. The CLI, the TUI and the web are all first class (§1.3), and chats are
shown in every workspace, including those of linked machines (§16.4b).

### 22.1 Decided by the owner

- **On by default** (`[chat] enabled = true`), and switchable off. Off hides the sidebar section,
  the keys, the CLI group and the web page, and starts no engine. Existing chats stay on disk.
- **A separate database, `chat.db`.** The rule "the database holds metadata, never contents"
  (§14.2, `AGENTS.md`) has this one exception: chat text, because history and search are the
  feature. Terminal contents still never go into any database.
- **Seed-replay is an acceptable fallback** where an agent can't fork or resume natively (§22.6).
- **Chats are shown in the workspaces of connected machines** (§22.11), listed, opened and used
  like local ones.
- **Sync and merge across machines is a later phase, CHAT6.** It needs its own brainstorm, but the
  schema is built for it now (§22.4, §22.15).

### 22.2 What a chat is, and is not

- A chat is **ptyro's own durable record** (a tree of nodes in `chat.db`). The agent's
  conversation is an **engine** that serves it: replaceable, and rebuilt from the record when it is
  gone, can't fork, or is a different agent. ADR 19.
- It is **not a session**. Sessions are processes with terminals in worktrees. A chat's engine
  *is* a structured session (§7.1) with `chat = <id>`, so every driver, the permission cards and
  the live upgrade carry over, but:
  - engines are **hidden from the session tree** and from `ptyro ls` (`ls --chats` shows them);
  - a chat's row in the sidebar carries the engine's status glyph and its attention;
  - an engine lives in its own **scratch directory** `<data>/chat/<chat id>/`, empty, with no
    project files, no `.ptyro.toml` and no hooks (the way summaries run, §11.2h).
- It belongs to **one workspace** (the selected row's, else the focused pane's, as for `n`), runs
  under **that workspace's profile** (so its account), and lives on **the machine that owns the
  workspace**. A removed workspace's chats move to `main` of their machine.

### 22.3 Identifiers and addresses

- A chat and a node have a **ULID** (§2.3); the human handle of a chat is `c:7f3k`.
- A chat has a **title** (free text) and no name. It is addressed by handle (`7f3k`, `c:7f3k`),
  by `@last` (the chat most recently used), `@pinned` and `@unread` (the one pinned chat, or the
  one with answers not read; when several are, the error lists them, newest first), and, as the
  last resort (§2.3), by a title substring that matches one chat. A chat of another machine is `laptop2:c:7f3k`, as sessions are
  (§16.4b); a handle only one linked machine has is that machine's (requests and `ChatSubscribe`
  alike).
- A node is addressed `7f3k/01JB…` (chat, node id suffix ≥ 4, §2.3), for `fork --at` and search
  hits. The web's citation links look for 6 or more characters after the slash: that only finds
  citations in prose (fewer would catch ordinary words); the daemon resolves the link.

### 22.4 Storage: `chat.db`

`<data>/chat.db` (0600), SQLite (bundled), WAL, `foreign_keys=ON`, migrations embedded and counted
by `user_version`, **its own writer thread** apart from the metadata store's (a long message or an
index update never delays a status write). It is opened when the daemon starts and `[chat]
enabled`. **If it can't be opened** the daemon runs, chats say why in one line and offer the fix,
and nothing else is affected. Built (`StoreError::fix`, also for a write that fails later): in use
by another program (busy, locked; at start it names the program and its pid, as `lsof` tells), the disk full, the file not writable
(read-only, can't open, permission), damaged (move it aside, keep it for `ptyro doctor`), and
written by a newer ptyro; anything else gives SQLite's own words and "`ptyro doctor` says more".

```sql
CREATE TABLE meta (key TEXT PRIMARY KEY, value);        -- machine_id (a ULID made once), schema notes
CREATE TABLE chat (
  id TEXT PRIMARY KEY,                -- ULID
  workspace TEXT NOT NULL,            -- workspace id on this machine
  title TEXT, title_src TEXT,         -- auto | agent | user (a user's title is never replaced)
  pinned INTEGER NOT NULL DEFAULT 0, archived_at INTEGER, deleted_at INTEGER,   -- deleted = tombstone
  agent TEXT NOT NULL, profile TEXT NOT NULL, tools TEXT NOT NULL,              -- current; a preset name
  leaf TEXT,                          -- the shown branch's tip (a node id)
  forked_from_chat TEXT, forked_from_node TEXT,
  created_at INTEGER, updated_at INTEGER, last_at INTEGER,                      -- last_at: last message
  nodes INTEGER NOT NULL DEFAULT 0, unread INTEGER NOT NULL DEFAULT 0, cost_micro_usd INTEGER NOT NULL DEFAULT 0,
  origin TEXT NOT NULL);              -- meta.machine_id of the machine that made it
CREATE TABLE node (
  seq INTEGER PRIMARY KEY,            -- local only; the FTS rowid
  id TEXT NOT NULL UNIQUE,            -- ULID, made by the machine that wrote it
  chat TEXT NOT NULL REFERENCES chat(id) ON DELETE CASCADE,
  parent TEXT,                        -- node id; NULL for the first node
  kind TEXT NOT NULL,                 -- user|assistant|thinking|tool|permission|plan|notice|turn_end
  body JSON NOT NULL,                 -- ptyro_core::chat::ChatBody, as the chat view renders it
  text TEXT,                          -- plain text to search: user and assistant text, a tool's title
  at_ms INTEGER NOT NULL,
  conv TEXT,                          -- the conv that produced it, if an engine did
  origin TEXT NOT NULL, deleted_at INTEGER);
CREATE INDEX node_by_chat   ON node(chat, at_ms);
CREATE INDEX node_by_parent ON node(parent);
CREATE VIRTUAL TABLE node_fts USING fts5(text, content='node', content_rowid='seq',
                                         tokenize='porter unicode61 remove_diacritics 2');
-- insert/update/delete triggers keep node_fts in step; tombstoned nodes leave the index
CREATE TABLE conv (                   -- an agent conversation that served a chat (a chat has several over time)
  id TEXT PRIMARY KEY, chat TEXT NOT NULL REFERENCES chat(id) ON DELETE CASCADE,
  agent TEXT NOT NULL, profile TEXT NOT NULL,
  conversation_id TEXT,               -- the agent's own id (Claude session_id, Codex thread id, ACP session id)
  node TEXT,                          -- the last node that conversation has seen
  how TEXT NOT NULL,                  -- new | native_fork | seed
  started_at INTEGER, ended_at INTEGER);
```

- **Nodes are immutable once complete.** A streaming assistant node is written when it completes,
  and every 2 s while it streams, so a daemon crash keeps what had arrived; the next start closes
  the turn with a `turn_end` whose `stop` is `interrupted`. **Editing and regenerating add a sibling
  node (a new branch); deleting a message sets `deleted_at`.** Nothing is rewritten in place.
- **Every node has a `parent`.** One chat is a tree; `leaf` names the branch that is shown. A
  branch is the path from the root to a node.
- **What is indexed:** `text` of user and assistant nodes, tool *titles*, and chat titles (`[chat]
  index_tools = true` adds tool output, capped at 2 KB a node). Thinking and diffs are never indexed.
- **Incognito chats write nothing** (§22.9). **Retention:** forever by default. `[chat]
  archive_purge_days` (0 = never) deletes archived chats after that long; `ptyro chat rm` and the
  row menu delete one at once (a tombstone, then the rows and the index entries are removed at
  the next compaction, in a day, so a sync can learn of the delete).
- **Privacy:** a chat's content is sensitive and large. It is in `ptyro backup` (`--no-chats`
  leaves it out), in `ptyro uninstall`'s plan (the file and its size), and in `ptyro doctor` (opens,
  FTS5 present, size, `PRAGMA integrity_check`). Secrets the profile holds are never put in a
  chat by ptyro; text a person types is stored as typed.
- **Live upgrade:** the connection closes before the exec and opens after; engines cross as
  structured sessions do (§4.5).

### 22.5 The engine

- **One engine per chat, at most.** A chat with no engine costs nothing. Sending a message starts
  one (or resumes the chat's current `conv`, §22.6); it **hibernates** after `[chat] hibernate_after`
  (default `2m`) idle, as sessions do (§7.4).
- **Which agent.** `[chat] agent`, else the workspace's default agent, else the first of claude,
  codex, gemini, opencode that is installed and has a structured protocol (§7.1). With none: the new
  chat says no agent that can chat is installed and offers setup (BOOT9, §24.10: the TUI's
  checklist, the web's setup page, `ptyro setup`). An agent without a structured
  protocol is not offered (a `protocol = "acp"` entry makes it one). `agent@profile` works as it
  does for sessions. The agent of a chat can be changed at any time (§22.7).
- **Permissions.** Tools the chat's preset allows are approved at launch where the agent can
  say so (below), otherwise by ptyro's answer `allow_once` to exactly those tools; a request for
  any other tool shows as a normal permission card in the chat, the row says `⚠`, and the inbox gets
  the entry. An answer never widens itself (§7.1). Answers from `read`-scope devices are refused
  as for sessions.
- **What the presets mean per agent** (CHAT0, 2026-10-29: every cell marked **[V]** was tried through
  the real agent, §20.8; **[S]** was read from the agent's own files or schema and **not** run, because
  a turn needs a login or a model that wasn't available):

  | | `chat` (default) | `read` | `none` |
  |---|---|---|---|
  | **Claude** (stream-json) **[V]** | `--tools "WebSearch,WebFetch" --allowedTools WebSearch WebFetch --strict-mcp-config --safe-mode --system-prompt <chat prompt>` | `--tools "Read,Glob,Grep"` and `--add-dir` for each attached folder, **no** `--allowedTools` for them (a bare allow reads anywhere), plus the same flags | `--tools ""` plus `--strict-mcp-config --safe-mode` |
  | **Codex** (app-server) **[V]** | `thread/start {sandbox: "read-only", config: {web_search: "live", features: {shell_tool: false, unified_exec: false, hooks: false, …}, mcp_servers: {<each>: {enabled: false}}}}`. `read-only` alone is not enough: it still reads any file | a profile `extends ":read-only"` that denies `$HOME` and re-allows the attached folders and what Codex needs to start; **no** `sandbox` parameter; shell on. Files outside `$HOME` stay readable, so the header says `tools: best effort` | the `chat` config with `web_search: "disabled"` |
  | **Gemini** (ACP) **[S]** (2026-10-30, read from 0.66.0's source; §20.8) | a policy file ptyro writes, passed as `--admin-policy` (the tier above the user's rules and "always allow" answers) and `--policy` (which replaces the user's own; for when a system policy makes Gemini ignore `--admin-policy`): `toolName = "*"` `deny` at 900, `google_web_search` and `web_fetch` `allow` at 950, `mcp_ptyro_*` with the history tools; `--extensions none`; `--allowed-mcp-server-names ptyro` (or `""`: none). The history server comes in `session/new`. 0.66.0 has no `--safe-mode`; the user's hooks and skills in a chat stay **[U]** | the same, with `read_file`, `read_many_files`, `glob`, `grep_search`, `list_directory` allowed and `--include-directories` for each folder: its file tools read only the workspace (the scratch folder and those) | the catch-all `deny` only |
  | **OpenCode** (serve) **[S]** | `POST /session {permission: [{"*","*",deny}, {websearch,*,allow}, {webfetch,*,allow}]}`, set again after every fork; per-message `tools` as a second fence | `read`, `glob`, `grep` allowed for the attached folders' patterns | the catch-all `deny` only |

  **`shell` and `mcp` (CHAT4, a preset of your own only):** Claude `Bash` in `--tools` and
  `--allowedTools`, each named server's definition from `<config dir>/.claude.json` `mcpServers` in
  `--mcp-config <scratch>/mcp.json` (a `0600` file in the chat's `0700` scratch folder, never the
  JSON on the command line: a definition can carry its tokens, and argv is in `ps` and in every
  client's `SessionInfo`) with `mcp__<server>` allowed (any MCP server means `--setting-sources ""
  --disable-slash-commands` instead of `--safe-mode`); Codex `shell_tool` and `unified_exec` on, the
  `workspace-write` sandbox (commands write only the scratch folder; with `read`, its profile), the
  named servers left out of the switched-off list; Gemini `run_shell_command` and `mcp_<server>_*`
  allowed, the names in `--allowed-mcp-server-names`; OpenCode `bash` and `<server>_*` allowed [S].
  A name the agent's config lacks is left out, and the chat says so. A shell chat shows `⚠`.

  Every Claude engine also gets `--safe-mode`: without it the user's hooks, CLAUDE.md, skills and
  plugins run inside the chat (§20.8). A Codex thread's hooks and MCP servers are switched off by
  name for the same reason. A chat whose agent can't enforce its preset natively, or can only
  partly (the `read` preset on Codex, anything on Gemini and OpenCode until a login lets CHAT1.3
  test it), shows `tools: best effort` in its header, not silence, and ptyro's own answer to each
  permission request still stands behind it: any tool the preset doesn't name is denied. Web content
  is untrusted text: with no shell and no file tools in the default preset, a page can say what it
  likes and the chat can't act on it. That is why `chat` is the default, and why `read` and `work`
  are opt-in per chat.
- **Rules the engine takes from CHAT0** (CHAT1.3 builds them in; each is verified in §20.8):
  - Claude's chat launch line is the table's, always with `--safe-mode` and `--strict-mcp-config`, and
    never `--bare`. A chat's scratch directory is a project folder in the profile's Claude history
    (`<config dir>/projects/<cwd slug>/`): deleting a chat removes that folder with its scratch
    directory, so Claude's own `--resume` list doesn't fill with dead chats.
  - Codex: the end of a turn is `thread/status/changed → idle` after `turn/started`, not only
    `turn/completed`. The launch lists the user's MCP servers (`codex mcp list --json`) and switches
    each one off in the thread's `config`. A `read` chat sends a permission profile and **no**
    `sandbox`; every other preset sends `sandbox: "read-only"` and no profile. Incognito is
    `ephemeral: true`.
  - ACP: the driver reads `agentCapabilities.sessionCapabilities` at `initialize` and offers list,
    resume and fork only where it says so (§22.6). OpenCode: its permission ruleset is set at session
    creation and **again after every fork**; a `retry` status with an `action` (a usage limit) is shown
    as a notice with its link, and no call waits for the model synchronously.
- **Web search out of the box.** All four agents ship one (Claude `WebSearch` and `WebFetch`;
  Codex `web_search`; Gemini `google_web_search`; OpenCode `websearch` where Exa's hosted search is
  on **[S]**), so the `chat` preset only has to leave it on. No search MCP is installed. A keyless MCP
  (DuckDuckGo or a fetch server) is listed in `[chat.tools.chat] mcp` only as a fallback the user
  can enable for an agent that has none.

### 22.6 Continuing a conversation: native, or seeded

Each message goes to the chat's **current conv** (the last row of `conv` for the chat). The rule,
from the stored record (never from the agent's own files):

1. **Continue natively** when the conv's `node` is on the shown branch (it is the leaf, or an
   ancestor of it), the agent and profile are unchanged, and the agent can still resume the
   `conversation_id` (Claude `--resume`, Codex `thread/resume`, ACP `session/load`, OpenCode the
   same session). Nodes after the conv's `node` that the agent hasn't seen (added from another
   branch, or a message sent while it was down) are **folded into the prompt** as a seed block.
2. **Fork natively** when a new chat or branch starts at a node the conv already reached: Claude
   `--resume ID --fork-session` (from the conversation's end only), Codex `thread/fork` with
   `lastTurnId` (from any turn). A new `conv` row (`how = native_fork`).
3. **Otherwise seed**: a new conv (`how = seed`), the first prompt carrying the branch as a
   transcript. This covers an agent switch, a fork from the middle for Claude, an agent that can't
   resume, a lost conversation, and a different profile.

CHAT0 (§20.8) settled which agents can do what: **Claude** resumes (`--resume`, same id) and forks
at its end (`--fork-session`, new id, original untouched) but has no fork at a message; **Codex**
resumes in a new process (`thread/resume`) and forks at any turn (`thread/fork` with `lastTurnId`,
verified); **Gemini 0.66.0** offers `session/load` only (no `session/list`, `resume` or `fork`: read
`agentCapabilities.sessionCapabilities` at `initialize` and use what it lists); **OpenCode** forks at
a message (`POST /session/:id/fork`, which copies the messages *before* it and drops the session's
permission rules, so set them again) and resumes by id. Rule 2 therefore holds for Codex at any
turn, for Claude at the end, and for OpenCode at a message; every other fork is seeded.

Seed format (the block ahead of the user's message, in the one prompt, so it costs no extra turn):

```
The conversation so far, saved by ptyro (continue it; do not reply to this block):
<conversation>
[user] …
[assistant] …
[tool WebSearch "query"] → first 300 characters
</conversation>
```

Thinking and diffs are left out; tool output is cut to 500 characters; the block is built from
the newest message backwards within `[chat] seed_tokens` (default 30000, estimated at 4 characters a
token). When the branch is longer, the oldest part becomes one summary paragraph from the title
command (§22.10), and when that fails, "(N earlier messages left out)". The transcript gets a
`notice` ("continued with codex from the saved transcript, 14 messages") that the engine is never
sent. Seeding costs tokens; the header shows it once. Because the record is the truth, **the
agent's own history is a cache**: losing it, or an agent's updated format, loses nothing.

### 22.7 Branches, forks, side chats and agent switches

- **Branch** (in one chat): *edit* a user message, or *retry* an answer. Both add a **sibling**
  node, set `leaf` there and run the engine from that parent (§22.6). The chat view shows `‹ 2/3 ›`
  where a message has siblings; `Alt-←` / `Alt-→` (and the buttons on the web) move between them.
  Branches are never merged.
- **Fork** (a new chat): copies the path from the root to a node into a new chat
  (`forked_from_chat`, `forked_from_node`, new node ids, so the two are independent), with the
  title "fork of …" until renamed. From the end of a chat, or at any message (`fork --at`).
  Native where §22.6 allows, else seeded. The copy-fork model is Codex's, gptme's and LibreChat's;
  the in-chat tree is Open WebUI's and llm's (RESEARCH R-CHAT-02).
- **Side chat** (`fork --side`): a fork that reads the chat's context and **never writes back**:
  its engine gets the branch as a seed, it is not listed under CHATS (it shows under its parent while
  open), and it is dropped on close unless **kept** (then it is a normal fork). The client that
  closes it deletes it; the daemon deletes any nobody has followed for `hibernate_after` (one left
  by a quit, a crash or a reload; checked again at start), and a chat's side chats go with it.
  Claude Desktop, Codex and Claude Code's `/btw` have one (R-CHAT-04).
- **Switch agent** (`/agent codex`, `chat.agent`, `chat update --agent`): the next message starts a
  seeded conv on the other agent. A bare name is the chat's own profile's; another profile's
  (`codex@work`) asks first, as the conversation goes to that account. The chat is unchanged; no
  other chat app can do this.
- **Make it a task** (CHAT4): start a normal session in a worktree whose first prompt carries the
  branch as a seed block and the chat's last answer, linked back by `forked_from_chat`.

### 22.8 Titles

The first exchange gets a title, once: (1) the agent's own when it sends one (ACP
`session_info_update {title}`: Gemini 0.66.0 declares it and never sends it, read from its source
**[S]** 2026-10-30; OpenCode writes its own title into the session, read
from `session.updated` or `GET /session/:id` [V]; Codex sends no `thread/name/updated` and sets no
name in an app-server thread, and Claude's stream and session file carry no title: both **[V]**, so
for Claude and Codex this step never happens and (2) always runs); else (2) `[chat] title_command`, a no-tools, no-MCP one-shot as for summaries
(default `claude -p --model haiku --output-format json --strict-mcp-config --tools ""`, run in
`<data>/summaries` under the chat's profile, 60 s, counted against `daily_cap`), fed the first user
and assistant messages (redacted as summaries are); else (3) the first line of the first message.
A title the user typed (`title_src = user`) is never replaced; `chat.retitle` with an empty box
clears it (and none is made for it again), `chat.autotitle` (`chat retitle --auto`) makes a new
one (owner, 2026-11-09: retitle clears, for every kind). `[chat] title = "off"` uses (3) only.

### 22.9 Incognito

`[chat] incognito = true` (or `--incognito`, or `Tab`/`C-i` in the new-chat box: most terminals send Tab for `C-i`) starts chats that **write
nothing to `chat.db`**, are not indexed and not listed under "all chats", and end when the engine
ends of itself or goes idle (`hibernate_after`), or the chat is deleted (the sidebar shows them
with `◌` while open); one stopped for a retry, an edit or another agent carries on with the next
message. One that someone is reading (a client follows it) outlives its engine: the next message
seeds a new one from memory, and the chat ends 10 s after its last reader leaves with no engine
running (a reload or a reconnect comes back in time); `chat find`'s are the same. The transcript
lives in the daemon's memory, so it ends with a live upgrade or a restart too (its folders go then). Claude's engine gets `--no-session-persistence`
**[V]**, so it has no resume and leaves no file; Codex's gets `ephemeral: true` **[V]**, which leaves
no rollout and no list entry; Gemini has no such switch, but its copy is in folders marked with
the chat's scratch directory (`<home>/.gemini/{tmp,history}/<slug>/.project_root`, §20.8 [S/V]),
which ptyro removes when the chat ends, and the chat says so ("ptyro keeps nothing, and removes
gemini's own copy when the chat ends"); OpenCode has no switch **[U]**, so its own history may keep
it, which the chat says plainly ("ptyro keeps nothing; opencode may keep its own copy"). `chat ask` (below)
is incognito unless `--keep`.

### 22.10 History and search

Three tiers; each one is useful without the next.

1. **Keyword (CHAT3).** FTS5 over `node.text`, ranked by bm25 with a snippet and the match
   highlighted, user messages weighted above assistant text as `llm` does. Filters: `--chat`,
   `--workspace`, `--agent`, `--machine`, `--since 7d`, `--pinned`, `--archived`. A query is plain
   words (all must match), `"a phrase"`, `-word`, `prefix*`, or `agent:codex`, `since:7d`. A hit opens
   the chat at that node. In the chat, find-in-chat searches the shown branch.
2. **Ask (CHAT3).** `ptyro chat find "what did we decide about the retry policy?"` runs a one-shot
   engine with **only** the chat tools (below) and no others, and answers with citations (`7f3k/01JB…`)
   that open. The same tools are in every chat when `[chat] history_tool = true`, so a chat can look
   up its own past ("as we said last week…"). Counted against `daily_cap`.
   - `chat_search(query, filters)`, `chat_read(chat, node?, before?, after?)` and `chat_list(filters)`
     are tools of `ptyro mcp` (§15.4), appearing only in chat engines and in leads whose run policy
     says `chat = true`.
   - **Scope:** `[chat] history_scope = "profile"` (default): a chat sees chats of **its own
     profile** only, so a work account's history never reaches a personal account's model. `"workspace"`
     narrows, `"all"` widens. Incognito chats are never visible.
3. **Meaning (CHAT5, opt-in; built 2026-10-30).** A local embedding of each message, merged with
   the keyword hits by reciprocal-rank fusion. Off by default: `[chat] meaning = true` (the settings
   panel's Chat section, `ptyro config set chat.meaning true`, or yes where a search offers it).
   Nothing is downloaded or computed until then. model2vec was chosen over fastembed: 31 MB against
   ~130 MB, and no ONNX Runtime. As built:
   - **Model:** model2vec `minishlab/potion-base-8M` (MIT), pinned to commit
     `bf8b056651a2c21b8d2565580b8569da283cab23`. Its files, from the Hugging Face API on 2026-10-30
     [S]: `model.safetensors` 30,236,760 B (one F32 tensor `embeddings`, 29,528 × 256 by its size),
     SHA-256 `f65d0f325faadc1e121c319e2faa41170d3fa07d8c89abd48ca5358d9a223de2`;
     `tokenizer.json` 683,666 B; `vocab.txt` 219,690 B; `config.json` (`model_type: model2vec`,
     `hidden_dim: 256`, `normalize: true`). 31 MB in all.
   - **Download:** the daemon fetches the files with curl (the way notifications are sent; HTTPS
     only) into `<data>/models/potion-base-8M/`. Each file goes to `<file>.part`, which `-C -`
     resumes after an interruption. It is then checked and renamed into place: the matrix by its
     SHA-256, the small files by size and by loading them (a WordPiece vocabulary with a row for
     every token). Their hashes aren't published, and the commit pin plus HTTPS covers them. A
     transfer under 1 KB/s for a minute fails and is retried (curl `--speed-limit`), so a stall
     never hangs it. A safetensors header whose offsets or shape overflow is an error. A
     failed download says why in `ptyro doctor` and is tried again in 10 minutes, or at once when
     switched off and on. `PTYRO_MEANING_URL` replaces `https://huggingface.co` (for a mirror; the
     tests use a `file://` folder). Development and tests never download it: they use a synthetic
     model in the same layout (`ptyro_meaning::synthetic`).
   - **Inference** in pure Rust (`crates/ptyro-meaning`, no ONNX, no `tokenizers` crate), as
     model2vec's `StaticModel.encode` does it. It is BERT-uncased WordPiece from `tokenizer.json`
     (`vocab.txt` if that isn't WordPiece), with **no special tokens** (model2vec encodes with
     `add_special_tokens=False`, so there is no `[CLS]`/`[SEP]`). `[UNK]` is dropped, the first 512
     tokens are kept, and the result is the mean of their rows, L2-normalized. Two approximations:
     accents are stripped for Latin (U+00C0–U+024F and the combining marks) rather than by full NFD,
     and punctuation is ASCII's plus the Unicode punctuation blocks rather than every `P*`
     character. model2vec's behaviour comes from its source as remembered, and it has not been run
     against the Python reference: [U] until it is compared on the real model.
   - **Storage:** schema v3, `node_vec(node, chat, at_ms, model, vec)`. A vector is int8 with an
     f32 scale: 260 B a message, a quarter of f32's size. On unit vectors it gives f32's ranking
     except for near ties. `model` is `potion-base-8M@bf8b056/i8`, and vectors of any other model
     are dropped when meaning search is switched on. A message whose text changes, or that is
     deleted, loses its vector (a trigger), and a vector is stored only if the message's text is
     still the text embedded (one that changed while it was embedded is done by a later pass), and a deleted chat's vectors go with its rows. Covered
     are the messages keyword search covers except titles (user and assistant text, tool text with
     `index_tools`), once complete. Incognito chats are never in `chat.db`, so never embedded.
   - **Backfill:** a daemon task at low priority, in batches of 128 with a pause between. It goes
     newest first, walking down `node.seq`, so the table is read once and the recent past is
     searchable soonest. After that each pass reads only what is new (it wakes on `ChatUpdated`),
     with a full pass hourly. Switched off, it stops. What was downloaded and embedded stays, so
     switching it on again is instant. The model is loaded only while it is used and dropped after
     5 idle minutes (30 MB of memory).
   - **Search:** `by:meaning` or `by:words` in a query (CLI `--meaning`/`--words`) asks for one
     kind. With meaning on, the default merges the keyword hits with the nearest `limit` messages by
     RRF (k = 60), and the best `limit` of the merge are the answer. Nearest means a cosine of at least 0.3 ([U]: set on the synthetic model), found
     by a brute-force scan of `node_vec`. Every filter, `-word` and the engine's `history_scope`
     apply, so the history tools' `chat_search` gets meaning search too. `ChatHit.how` is `words`,
     `meaning` or `both`. A hit found only by meaning has the start of the message as its snippet
     and shows `≈` (CLI, TUI, web). `by:meaning` while meaning search is off returns
     `NeedsConfirmation`, and each client offers to switch it on: the CLI asks on a terminal, the
     TUI asks once a list (`Confirmed::MeaningOn`), and the web shows a button (`chat.meaning`).
     Each writes `chat.meaning = true` to the user config. While its model downloads, `by:meaning`
     answers `Busy` with the progress, and plain searches use words. The machine fan-out is
     unchanged: each machine searches its own `chat.db`, by its own config.
   - **Cost** (`make spike-chat-meaning`: 100k messages of 10–200 words and a synthetic model the
     real one's size, `--profile local`, this Mac, 2026-10-30): the model loads in 28 ms. Embedding takes 57 µs a message, and the whole backfill
     7.3 s. A search's scan takes 45 ms median and 52 ms worst, and the vectors are 26 MB (260 B a
     message). An earlier version took 62 s to backfill, because every batch re-read the table from
     the top, and 160 ms to search, because the scan joined `node` rows; the `seq` walk and
     `node_vec`'s own `chat`/`at_ms` fixed both.
   - `ptyro doctor` gives its state: off, downloading (N of 31 MB), N of M messages embedded,
     ready, or why not. It now prints the `chat.db` line too, which the daemon reported and the CLI
     didn't show.

**Every machine is searched** (§22.11): a search from the TUI, CLI or web goes to this daemon and
to every linked machine that is up, each searching its own `chat.db`; bm25 scores differ per
machine, so results are interleaved by each machine's rank (round robin), then date, each with its
machine chip. The ask tools search every machine too when asked (`chat_search`'s `machines`,
CHAT7): each linked machine is given the asking chat's profile by name (`ChatSearch.profile`), so
it answers with its own chats of that profile; a chat whose `history_scope` is its workspace (one
of this machine's) searches here only. `chat_read` of a hit there is read there, when the scope
sees that chat (its profile, as that machine names it; never an incognito one). A machine that is down or doesn't answer is left out of a search; the
reply has no field to say so, so the daemon logs it.

### 22.11 Many machines

The owner's rule: chats show in the workspaces of connected machines. They follow the mirror of
§16.4b, which is why a chat belongs to a workspace.

- **Authority.** A chat lives on its workspace's machine, with its engine, its agent logins and
  its profile; that machine is the only writer. The local daemon **mirrors** the chats of the
  workspaces it follows and forwards what is done in them. A chat made in a remote workspace (`+` on
  its CHATS row, `chat new --workspace laptop2:work`) is created **there**.
- **Mirror.** `ChatInfo` (never a transcript) joins the walk that qualifies names on arrival: it
  gains `machine`, and its `workspace` and `profile` take the machine prefix; chat and node ids stay
  as they are (unique everywhere). The list is refreshed whole on (re)connect and on `EventsLost`;
  `ChatUpdated` / `ChatRemoved` events go to clients only, on `Daemon.mirrored`.
- **Routing.** `chat` joins the address keys `handle_as` looks at (`addr`, `name`, `workspace`,
  …); a request about `laptop2:c:…` goes over the link with the prefix taken off its addresses only,
  and its reply is qualified. `ChatList` (and `Chats` in the TUI and web) gains every mirror's
  shown part; `ChatSearch` fans out (§22.10). Requests about this machine itself are not routed
  (`[chat]` settings, `chat.db` doctor and backup).
- **Open and use.** `ChatSubscribe` (the live stream, `ServerMsg::Chat` with nodes) goes over the
  client's own upstream connection to the machine, as screens do, so streaming, answering a
  permission card, sending, branching and forking work on a remote chat exactly as on a local one.
  The engine runs on the owning machine, with its agents.
- **Offline.** An unreachable machine's chats stay listed, greyed, from the last list heard, with
  "offline since 14:02"; opening one says "laptop2 is offline; its chats open when it is back"
  (transcripts are not cached here: that is what CHAT6 is for). Every change is refused with that
  message. The link retries as machines do.
- **Same version** is already required of a link (§16.4b), so chat messages need no negotiation
  beyond the protocol minor (§22.13).
- **Hidden workspaces** (`machine hide laptop2 work`) hide their chats too.
- **Scopes** (§16.3, checked on both ends): listing, reading, searching and exporting need
  `read` (a lost `read` token is a leak of chats, exactly as it is of screens); sending, creating,
  forking, branching, retitling, pinning, archiving and answering a card need `control`; deleting
  needs `control`; the `[chat]` settings need `full`. Links between machines are `full`.
- **Alerts.** A chat is not an inbox generator by default: only a **permission request** or an
  **error** raise attention (`[chat] notify = "blocked"`; `"all"` adds a finished reply while no
  client shows the chat; `"off"`). A mirror's alert is delivered by the local daemon's channels
  with the same push `tag` as sessions, so a phone paired with both hears one. A reply that arrives
  while no client is looking raises the chat's `unread` count (a `●` on its row), cleared when it
  is shown, on every machine's view.
- **Not here: cross-machine copies.** Forking a remote chat into a local one is a fork whose
  source is on another machine; CHAT6 may do it, and until then it is refused with that reason.

### 22.12 Surfaces

**Sidebar (TUI and web).** A **CHATS** section under every workspace's projects (each machine's
workspaces too), so a workspace with no chats shows one dim row `＋ new chat` (the section never
hides when `enabled`):

```
▾ ● laptop1 · work
  ▾ myrepo   …
  ▾ CHATS                      ＋
    ★ migration plan   claude        <- pinned
    ● rust lifetimes   codex         <- working
    ◉ release notes    claude ⚠      <- needs you (permission)
      dinner ideas     gemini  ●     <- unread
      all chats… (23)
▾ ● laptop2 / work
  ▾ CHATS                      ＋
      standup notes    claude
```

- The newest `[chat] recent` (default 5) unpinned chats show after the pinned ones; `all chats…`
  opens the **chat list** (full sized): every chat of the workspace (or `all workspaces`, `all
  machines`), grouped Today / Yesterday / This week / Earlier, with `/` search, pin, archive, delete,
  the state filters, and a preview. Rows reuse the status glyphs (§11.1) plus `★` pinned, `●`
  unread, `◌` incognito, `⎇` a fork, the agent, and the machine chip where it isn't this machine.
- **Chat view** = the existing structured-session chat view (§7.1: cards, diffs, permission
  card `a/A/d`, composer, PageUp/Down) plus: a header (title, `agent@profile`, tools preset, `⎇ branch
  2/3`, cost, machine chip); the branch navigator; a composer that takes **slash commands**
  (`/fork`, `/side`, `/agent`, `/tools`, `/title`, `/retry`, `/search`, `/export`, `/incognito`,
  `/task [PROJECT] [--agent A] [--name N]`, `/compare [AGENTS…] [question]` (agents space- or
  comma-separated, `codex@work` kept as typed), `/pick`), each also a palette action (`//` sends a
  message starting with `/`, a path like `/etc/hosts …` is a message as it is, and a name that
  isn't a command stays in the composer, never lost); an `@` completion for attached folders (TUI and web: the matching folders shown, `↑`/`↓` to
  choose, Tab or ⏎ to put one in; `chats::at_query`); and find-in-chat.
- **Keys** (prefix, then; proposed, `m` and `g` are free in `docs/keys.md` on 2026-10-29; CHAT1.7
  checks and the user can rebind): `m` **new chat** in the current workspace (the composer opens at
  once, ⏎ sends; `C-o` picks agent, tools and folder; `Tab`/`C-i` incognito), `g` the **chat list**
  (searchable), `C-g` find across all chats, `Alt-←/→` branches. In the sidebar on a chat row (no
  prefix): `⏎` open, `,` retitle, `*` pin, `x` archive, `X` delete (asks), `f` fork, `.` its menu; on
  the CHATS heading `⏎`/Space fold it as on a project (`h`/`l` close and open it), and a click on
  its `＋` starts a chat in that workspace. Opening a chat or the list gives it the keyboard; a
  slow reply (a fork, a side chat, `/find`) opens its chat only if what asked for it is still
  shown, else it is offered (`C-q u`). Each chat keeps its draft while it isn't shown; Esc goes
  back to the list a chat was opened from, a side chat to its chat. While the chat panel is shown
  the session keys `,` `x` `X` mean its chat, never the session under it. The composer edits like a shell line: `←/→`, `C-a`/`C-e`
  (Home/End) to the line's start and end, `C-u`/`C-k` delete to them, `C-w` the word before. The
  shared keys (`/` find, `.` menu, `j/k`, `Tab`, `z`, `t` where it applies) behave as for sessions.
  The view's own keys (`C-f`, `C-o`, `Tab`/`C-i`, `a`/`A`/`d` on a card, `C-c`, `Alt-←/→`,
  `PageUp/Down`) are the key map's `chat` table and the list's (`/ m * x X , f e s A i I Tab .`)
  its `chatlist` table: `[keys]` names them `chat:KEY` and `chatlist:KEY`. A `chat` key whose
  action doesn't apply now (a card letter with no card) is typed instead. Every key is in
  `[keys]` and the settings panel, and each action in the palette (`chat.*`).
- **CLI** (an address may be `laptop2:…`). `--json` on a verb that asks (`new`, `ask`, `send`,
  `retry`, `edit`, `fork`, `find`, `compare`) waits for the answer, unless `--no-wait`, and prints
  `{"chat", "message", "answers": [{"agent", "node", "text"}]}` (`compare`'s also has `agents`);
  on one that changes a chat (`set`, `retitle`, `keep`, `branch`, `pick`) it prints `{"chat"}`,
  and on `pin`/`unpin`/`archive`/`unarchive` `{"chats": […]}`; `ls`, `search`, `presets`,
  `task` and `import` print their lists and reports; `show` and `export` take `--format json`;
  `answer`, `stop`, `rm` and `open` print nothing to parse. A question whose first word names a
  verb (`ptyro chat show me how…`) needs `chat new` or quotes; `show`, `open` and `set` given
  more words than a chat say so.

  ```
  ptyro chat [new] [PROMPT] [--agent A[@P]] [--preset|--tools PRESET] [--folder DIR]… [--incognito|--keep]
                   [--workspace W] [--title T] [--compare A,B] [--no-wait] [--timeout S]   # new chat; with no PROMPT, opens it in a terminal
  ptyro chat ask QUESTION [--keep] [--agent A] [--preset|--tools PRESET] [--no-stdin] [--timeout S]   # one-shot answer on stdout; stdin is context
  ptyro chat ls [--workspace W] [--machine M] [--pinned] [--archived] [--imported] [-l N]   # every workspace's unless -w
  ptyro chat show CHAT [--all] [--ids] [--branches] [--format text|md|json]
  ptyro chat open CHAT                               # the chat view
  ptyro chat send CHAT TEXT… [--no-wait] [--timeout S]   # waits for the answer by default
  ptyro chat retry CHAT [NODE] · edit CHAT NODE TEXT · branch CHAT --to NODE
  ptyro chat fork CHAT [--at NODE] [--agent A] [--side] [--keep] [--yes] [PROMPT] [--no-wait] [--timeout S]
  ptyro chat retitle CHAT [TITLE|--auto] · pin|unpin|archive|unarchive|rm CHAT… · keep CHAT
  ptyro chat search QUERY [filters]  ·  ptyro chat find QUESTION   # no match is exit 0
  ptyro chat answer CHAT allow|always|deny  ·  ptyro chat stop CHAT
  ptyro chat export CHAT [--format md|json] [-o FILE]
  ptyro chat set CHAT [--agent A] [--preset P] [--folder DIR]… [--no-folders] [--yes]   # (built, CHAT4)
  ptyro chat presets [--json]  ·  ptyro chat task CHAT [-p P] [-a A] [-n NAME] [-d]
  ptyro chat compare CHAT [-a A,B…] [TEXT]  ·  ptyro chat pick CHAT [NODE]   # (built, CHAT5)
  ptyro chat import [--agent A|all] [--profile P] [--since 30d] [--dry-run]  ·  chat ls --imported
  ```

  `git diff | ptyro chat ask "review this"` works from any shell, even outside a ptyro session.
- **Web.** A **Chats** page beside Sessions (recent, pinned, a live search box), the chat view
  (with the same header, branch navigator, composer and slash commands; touch targets for
  fork/retry/edit; the machine chip), `#/c/<id>` URLs, a preset and an agent picker, incognito
  toggle, export, and the same sidebar section on wide screens (beside the sessions: the pinned
  chats and the newest `[chat] recent`, with `all chats…`). Side chats list under their chat; `m`
  starts a chat, `Ctrl-G` searches them all, and `@` in the composer lists the chat's folders. Each
  control is an entry in `ACTIONS` (`web/src/app.ts`, `web/src/chats.ts`) with its tooltip.
- **Parity.** One row in `docs/parity.md` for every `chat` verb, with its TUI action and its web
  action; none is waivable (`chat ask` is `chat.new` with `C-i`, and its answer is the view).
- **Settings panel.** Every `[chat]` key below has its schema entry (type, default, help, when it
  applies, `also` words like "history", "ai", "web search"), a **Chat** section in the panel (TUI
  and web), and the tool presets are a collection (`chat.tools.*`) with a `title` and an `empty`
  sentence.

### 22.13 Configuration

```toml
[chat]
enabled = true            # live: off hides the section, keys, CLI group and web page
recent = 5                # live: chats shown under CHATS in the sidebar
agent = ""                # next: "" = the workspace's default agent, else the first installed one
preset = "chat"           # next: preset for new chats: chat | read | none | your own ([chat.tools.X])
incognito = false         # next: new chats start incognito
hibernate_after = "2m"    # next: an idle engine stops; the next message resumes or seeds
seed_tokens = 30000       # next: the most a seed block carries (§22.6)
title = "auto"            # auto | off
title_command = []        # argv; [] = the summaries' default command (§22.8)
daily_cap = 200           # per profile per UTC day: titles and chat find, counted together
history_tool = true       # the chat tools of §22.10 in every chat
history_scope = "profile" # profile | workspace | all
index_tools = false       # also index tool output (2 KB a node)
meaning = false           # live: meaning search (§22.10 tier 3); on downloads a 31 MB model once
import = false            # live: import the agents' own histories every 10 min (CHAT5; copies them)
notify = "blocked"        # blocked | all | off (§22.11)
archive_purge_days = 0    # 0 = keep archived chats forever

[chat.tools.chat]         # `tools` is the preset map: built-in `chat`, `read`, `none`; yours add to them
                          # (`find` is `chat find`'s own, history tools only: a preset of that name is refused)
web = true                # the agent's own web search and fetch
read = false              # read files in `dirs`
dirs = []                 # folders every chat of this preset may read
shell = false             # never in a built-in preset; a user preset may, and shows ⚠ like `dangerous`
mcp = []                  # extra MCP servers by name (from the agent's own config or [mcp.*])
allow = []                # extra tool names approved without a card
```

Presets and the keys marked *next* apply to chats started or resumed after the change; `live`
ones to every screen at once. `[chat]` is refused in a repository's `.ptyro.toml` (§13.1), as
agents are. `ptyro init` asks nothing about chats: they work on first use.

### 22.14 Protocol (additive, the next minor after 1.20)

**As built** (1.22–1.25; `ptyro-protocol`'s doc comments are the reference). Requests:
`ChatNew{workspace?, agent?, preset?, title?, prompt?, incognito?, folders, compare, confirmed}` →
`Chat{info}`; `ChatList{workspace?, all, pinned, archived, limit?}` → `Chats{chats}` (other machines'
chats merged in from their mirrors, archived ones asked of each); `ChatRead{chat, all, from?,
limit?}` → `ChatTranscript{info, nodes, siblings}` (`from`/`limit`, 1.25: a part, oldest first, for a
chat too big for one reply, whose error says so); `ChatSubscribe{chat}` / `ChatUnsubscribe{chat}`
(the live `ServerMsg::ChatLive{chat, rev, reset, items, siblings}`, whose items carry node ids; a
reset too big for one frame comes as several, the first with `reset`; a subscription is kept by the
chat's full id, so any of its addresses ends it); `ChatSend{chat, text}`; `ChatRetry{chat, node?}`;
`ChatEdit{chat, node, text}`; `ChatBranch{chat, to}`; `ChatFork{chat, node?, agent?, side,
confirmed}`; `ChatUpdate{chat, title?, pinned?, archived?, agent?, preset?, keep?, folders?,
confirmed}` (a new agent, preset or folders is refused mid-answer, as a retry is); `ChatDelete{chat, confirmed}` (an exact address; asks unless confirmed, 1.39);
`ChatAnswer{chat, decision}`; `ChatStop{chat}`; `ChatSearch{query, chat?, workspace?, agent?,
pinned, archived, limit?, machines}` → `ChatHits{hits}`; `ChatFind{question, agent?, workspace?}` →
`Chat{info}` (an incognito chat that answers; the client follows it); `ChatExport{chat, format}` →
`Text{text}`; `ChatTask{chat, project?, agent?, name?, confirmed}` (answered as `WorktreeNew`);
`ChatPresets` → `ChatPresets{presets, incognito}`; `ChatCompare{chat, agents, text?, confirmed}`;
`ChatPick{chat, node?}`; `ChatImport{agent?, profile?, since?, dry_run}` → `ChatImported{report}`.
Events `ChatUpdated{info}`, `ChatRemoved{id, machine?}`. `ChatInfo` carries `id, handle, workspace,
machine, title, title_src, agent, profile, tools, pinned, archived, incognito, forked, side_of, leaf,
nodes, unread, created_ms, last_ms, cost_micro_usd, status, attention, engine` (1.24 adds `folders`,
`best_effort`, `shell`, `comparing`, `source`, `source_dir`; `ChatSiblings.agents`;
`AgentChoice.found`; 1.25: `ChatNew.incognito` is optional, none meaning `[chat] incognito` and
`false` a kept chat even when that is on, and `ChatPresets.incognito` says the default, which a new
chat's toggle starts from; `ChatUpdate.confirmed` and `ChatFork.confirmed`: a bare agent name is the
chat's own profile's, and another profile's asks first with `NeedsConfirmation`, as compare and task
do; `ChatAnswer.node`: the card answered, refused when another (or none) is waiting, so a double
tap or a card answered elsewhere never decides the next request; the TUI and the web send it,
`chat answer --node` too; `ChatSearch.profile`: only that profile's chats, what a chat's history
tools give another machine's search). `SessionInfo` gains `chat` (the chat id of an engine). Scopes as in §22.11, listed in
`devices::needed`.

**Not built** (in the first draft; nothing needs them yet): `ChatNew.profile` (it is
`agent@profile`); `ChatList.machine`, `cursor` and `next` (no paging: a list is metadata, and
`limit` caps it); `ChatGet{from, branches}` (it is `ChatRead`, whose `all` is every branch);
`ChatSend.parent` and `.agent` (a branch is `ChatEdit` or `ChatRetry`, an agent `ChatUpdate.agent`);
`ChatBranch.leaf` (it is `to`, any node, shown to its newest tip); `ChatFork.keep` (a side chat is
kept by `ChatUpdate.keep`); `ChatFind.machines` and a `ChatAnswer{text, cites}` reply (`ChatFind`
answers with the chat that answers, whose answer cites `c:HANDLE/NODE`; `ChatAnswer` is the
permission answer).

### 22.15 Built for sync (CHAT6) now, so it needs no migration later

CHAT6 merges chats across machines. It needs its own brainstorm, but these are in CHAT1's schema
so that it is a feature and not a rewrite:
- **Globally unique ids** (ULIDs, §22.4) for chats and nodes, minted on the writer; a stable
  `meta.machine_id` and an `origin` on every row.
- **Immutable, append-only nodes with parent links**: two machines' versions of a chat are two
  trees over the same ids, and a merge is the **union of nodes**, which has no conflicts (an id
  is never reused, and a node is never edited; edits are siblings).
- **Tombstones, not deletes** (`deleted_at`), kept until compaction and sync both know of them.
- **Chat fields** (`title`, `pinned`, `archived_at`, `leaf`, `tools`) are last-writer-wins by
  `updated_at` with `origin` as the tiebreak, except `title_src = user` beats `auto`.
- Open for CHAT6 (do not decide in CHAT1): where a synced chat *lives* (a replica per machine, or
  one owner and read replicas), what "continue a synced chat here" means for the engine (seed
  from the union, a new conv), how the workspace and profile of a chat map across machines (the
  owner's profile names differ), which machines may see which chats (by profile?), clock skew
  for `updated_at` (a hybrid logical clock), how large histories transfer (a node range per
  chat), whether the merge is automatic or offered, and what a conflict in `leaf` looks like.

### 22.16 Testing

CHAT builds on the fakes of M10 (a fake for each of the four protocols), so every test runs with no
agent installed:
- **Store:** migrations, the FTS triggers, a streaming node written in steps and a crash mid-turn
  (the next start closes it `interrupted`), tombstones and compaction, `chat.db` missing or locked
  (the daemon runs, the chat says why), backup with and without `--no-chats`.
- **Engine:** native continue, native fork, seed (agent switch, fork from the middle, lost
  conversation, other profile), the seed budget and the summary, hibernate and resume, the preset
  per agent (the launch arguments are asserted for each), a denied tool, an incognito chat leaving no
  row, a live upgrade mid-turn.
- **Search:** ranking, phrases, filters, scope by profile (a work chat is invisible to a personal
  one), incognito never found, the ask tools through `ptyro mcp`, the cap.
- **Machines** (as `tests/mm1.rs`, two linked daemons): chats listed in the remote workspace,
  created there, opened and streamed over the upstream, a permission card answered, a search that
  spans both, `read`-scope refusals, offline listing and its message, hidden workspaces, no echoes.
- **Surfaces:** the TUI in a headless emulator (the section, the list, a branch, the keys), the
  web (Chrome against a sandbox, as CFG1 did), `make parity`, the settings schema test (a key with
  no entry fails), and `make verify-chat` against a real Claude and Codex when logged in (a chat with
  a web search, a fork, a seeded switch).

### 22.17 Not here, and unverified

- **Not here:** a chat client for raw provider APIs (ptyro drives registered agents and their
  accounts), voice, images and files as attachments (a folder in the `read` preset covers files),
  artifacts, shared chats between people, cloud storage of chats.
- **Settled by CHAT0 (2026-10-29, §20.8):** the Claude flags through the stream-json driver
  (`--tools`, `--allowedTools`, `--strict-mcp-config`, `--safe-mode`, `--fork-session`,
  `--no-session-persistence`, `--system-prompt`); `WebSearch` in `-p` stream-json on a Claude
  subscription login; Codex's `thread/fork` with `lastTurnId`, `ephemeral`, `web_search = "live"` and
  `features.*` through the thread's `config`, `thread/name/set`; FTS5 and the `porter unicode61`
  tokenizer in the bundled SQLite, and the cost of a 100,000-node index. **Three claims of the first
  draft of §22.5 were wrong and are corrected there:** a bare `--allowedTools Read` is not a folder
  limit; Codex's `readOnly` sandbox still reads any file; and `--bare` is not a way to a clean Claude
  (it can't log in).
- **Still unverified, each a known gap and a task before the work that depends on it:**
  - that `WebSearch` works in `-p` on an API-key, Bedrock, Vertex or Team account (only a
    subscription login was tried);
  - that Gemini's policy rules stop a call (read from its source, §20.8; a turn needs a login),
    and whether its hooks and skills load in a chat;
  - that OpenCode's ruleset stops a call, and that its `retry`/`action` status is what a driver sees
    at a limit (a free tier at its limit, no provider logged in);
  - `session/resume` and `session/fork` of ACP on an agent that offers them (Gemini 0.66.0 doesn't);
  - Linux: every result above is from macOS (Codex's sandbox and permission profiles differ there);
  - the `read` preset on Codex outside `$HOME` (it stays readable).
  The first three fit `make verify-chat` (§22.16), which CHAT1.3 writes with a login for each.
  Facts found so far, dated and tagged, are in RESEARCH §IV.3.

---

## 23. Shells: a workspace's numbered terminals **[decided 2026-11-04 (owner, ROADMAP SH); built 2026-11-04 (SH.1–SH.8); SH.0's real-terminal check open]**

**As built (2026-11-04), and where it differs from the design below:**
- **Daemon** (`crates/ptyro-daemon/src/shells.rs`): a loose session carries `Meta.shelf`
  (`Shelf {slot, scratch, watch, lost}`), which crosses a live upgrade with the rest of `Meta`. Numbers
  are handed out under a reservation (`RESERVED`), and a new session's shelf waits in `PENDING` until
  `launch` puts it on the session, before `SessionCreated`. That way neither a client nor the
  auto-register sweep ever sees it unplaced. A restart keeps the number (`carried`). `place_session(s,
  None)` is where a session loses its place, and it now calls `shelve`. The sweep counts a loose
  session as placed, so its new repository registers and it stays put.
- **No new database columns** (§23.13 differs). What a restart offers back is a `kv` snapshot,
  `shells:kept`, that the one-second tick rewrites when it changes: id, name, title, kind, argv, last
  directory, workspace, profile, number, client env. One number per workspace is the daemon's rule
  (the reservation), not a unique index. Nothing goes into `ptyro.db` but metadata, as before.
- **Protocol 1.29**, not 1.28 (WIN6 took 1.28): `ShellMove`, `ShellPlace`, `ShellWatch`; `SessionInfo`
  and `SpawnSpec` gain `loose`, `slot`, `scratch`, and `SessionInfo` also `watch` and `lost`. No
  `ShellMoved` event: each moved session sends `SessionUpdated`. No `SlotTaken` error code: a taken
  number is a `Conflict` that names the shell there.
- **A loose spawn** whose directory doesn't exist on the daemon's machine (`~` from the web, a gone
  directory, another machine's path) starts at home. `~/x` is expanded when it exists.
- **Joining SHELLS** happens on a `ProjectRemove{release}`, at a spawn outside every project, and by
  hand. A project dropped from a workspace (`ws remove-project`) keeps its sessions placed, and they are
  listed unnumbered under SHELLS. So is any other session with no row (the old `other sessions`).
  The note when one joins is in the log, not sent to clients.
- **Put there, or landed there** (`Shelf.pinned`, the full suite's UX1.5 tests found it): a shell
  asked for in SHELLS (`loose`, `--slot`, `--scratch`), one set loose by hand, and one offered back
  after a restart are *pinned*: they never move, as §23.2 says. A session that only *landed* in
  SHELLS (started outside every project, or its project let go) still moves into the repository it
  `cd`s into once that is a project, and gives up its number. That keeps UX1.5's "the repository
  you're in is a project" for a plain `ptyro new` in `/tmp`.
- **The label** (`ptyro_core::session::shell_label`, shared by the TUI, the CLI and the web) is
  the title, else an agent's or a command's *name* (`claude-rook`, `dev-server`), else for a
  shell what runs in it and a short directory (`htop · ~/dl`, `…/tmp` outside home). §23.4's
  directory-only label read badly for agents. A shell's pane header keeps the session's name
  first, as every pane does, with the strip after it.
- **Removal safety:** a live loose session whose directory is inside the worktree holds it
  (`busy_in`). Like any of the worktree's own shells, it does so even at its prompt.
- **Scratch shells** never ask: an empty directory goes with its shell, and one with files stays,
  logged. `ptyro gc` doesn't list leftovers yet.
- **Watches:** `output` fires when output arrives while nobody looks and nothing is pending (so it
  doesn't repeat every second); `silence` once per quiet spell; `match` looks at the screen whenever
  output changed, then clears.
- **The TUI** (`crates/ptyro-tui/src/shells.rs`):
  - Keys: `shells.go N` (`M-1…9`, `M-0`), `shells.new` (`C-q C-c`, `c` in the sidebar),
    `shells.menu` (`C-q e`), `shells.command`, `shells.scratch`, `shells.move`, `shells.watch`,
    `shells.loose`, `shells.adopt`.
  - The shells menu is a hotkey picker: digits, `c`, `!` a command, `s`.
  - `[shell] alt_digits` (default on) binds or frees the ten Alt digits. It is a setting in
    Configuration → Shells rather than a switch in the Keys section, and the key list folds them
    into one `M-1…9, M-0` row.
  - The hints fall back to `C-q e 1…9` when `TERM_PROGRAM` is `Apple_Terminal` or `iTerm.app` and
    the kitty protocol is off. The health badge doesn't offer the terminal setting yet.
  - The header strip isn't clickable in the TUI yet. It is on the web.
- **The web** (`web/src/shells.ts`): the SHELLS group above the cards, the strip with tap and swipe,
  ⋯ items, and digits `1`–`9` on the list and on a shell's page while the terminal isn't focused. No
  chord reaches the page through a focused terminal, so none is bound there.
- **SH.0** checked what can be checked here. crossterm reports `ESC 1` and kitty's `CSI 49;3u` as
  Alt+`1`, and that reaches `shells.go 1` (`keys.rs` test). `¡` doesn't. The real-terminal matrix is
  `make verify-shells`, run by a person in each terminal. Its results are not recorded yet [U].
- **After the code review (2026-11-04):**
  - A bare `%N` from a command running inside a session (the CLI, an agent) means that
    session's workspace: `handle_as` rewrites it to `s:<id>%N`, found by the caller's pid
    (`session_of_pid`). Outside a session it is still the workspace the TUI shows.
  - Only a shell *asked* for in SHELLS falls back to home for a missing directory. Any other
    spawn there is refused (`not a directory`), as before. ptyro's own work in its data
    directory (a run's judge) never takes a number.
  - The restart list keeps rows that were offered back but not yet started, and dormant loose
    agents, whose numbers `restore_shells` gives back after `restore_dormant`.
  - When a workspace goes, only its shells are renumbered: main's keep theirs.
  - A move onto a number that a starting shell holds is refused.
  - `--stop-sessions` (remove, archive) also stops the loose shells standing in the worktree.
  - A `match` watch's baseline (the lines that already match) is taken when it is set.
  - `section = "never"` lists every session with no row under "other sessions", numbered ones
    included.
  - The web's digit keys act on one workspace (the one whose shell was opened last, else
    `main`), and the group splits by workspace.
- **The rest of SH (2026-11-04, SH.9–SH.12):**
  - **The strip is clickable in the TUI** (`App.strip_hits`, filled as each header is drawn and
    clipped to what shows): a number shows that shell (one lost in a restart starts again), and
    `＋` starts one in that workspace.
  - **Leftover scratch directories** (`Reply::Gc.scratch`, `ScratchLeftover {path, files, bytes,
    modified_ms}`): a directory in `<data>/scratch/` that no session uses. "Uses" means its shelf's
    scratch directory, where it started, or where it is now, so a scratch shell adopted into a
    worktree still holds it. A directory still being made for a starting shell (`STARTING`) is
    held too. An empty one is simply removed instead of listed. Sizes are measured at most every
    half hour, for three seconds at most, counting files only. `ShellScratchRemove{path}`
    (`full`) deletes one, only directly in `scratch/` and never one in use. They are shown by:
    - `ptyro gc`, which lists them after the worktrees and asks `d` for each (`--json` is still
      the worktrees; `--scratch [--json]` the leftovers alone);
    - the TUI's `G` list, where ⏎ deletes one after a yes;
    - the web's Clean up page ("Left behind by scratch shells", Delete asks);
    - `ptyro doctor`, which notes how many (`shells scratch`).
  - **Option-as-Meta:**
    - Alt with a digit proves Alt reaches ptyro (`App.alt_seen`). Hints then say `M-1…9`, and
      the finding goes. Any other Alt key proves nothing: Terminal.app sends Option-Left as
      Alt-b even without Option as Meta.
    - Option-digit arriving as `º¡™£¢∞§¶•ª` while Alt hasn't been seen is said once, as a toast
      naming the terminal's setting. The character still goes to the program.
    - The health badge's finding `shells alt keys` (Terminal.app and iTerm2 without the kitty
      protocol) says where the setting lives, and ⏎ opens `shell.alt_digits` for anyone who'd
      rather free the keys. ptyro never edits the terminal's own settings.
    - `ptyro doctor`'s `terminal alt keys` says the same from a shell. It names the shells menu as
      the keymap binds it, says nothing is wrong when `alt_digits` is off, and trusts the kitty
      keyboard protocol.
  - **The tour's sixth stop**, "keep a shell of your own" (TUI and web), moves on once a shell is
    in SHELLS. The headless tour test presses Alt-1 there.


What tmux's windows give you, and ptyro hasn't: terminals that belong to no repository, made with
one key and reached with one key and a number. Every workspace gets a **SHELLS** section beside
its CHATS. A shell there is an ordinary session (any command, not only a shell) with a **number**
(its *slot*) that stays the same, so `Alt-3` is always the same shell. RESEARCH R-UX-40…42.

### 23.1 Decided by the owner (2026-11-04)

- **Per workspace.** Each workspace has its own SHELLS and its own numbers, like a tmux session's
  windows (a workspace is ptyro's tmux session, §2.1). Switching workspaces switches the set.
- **A new shell starts in the pane's directory**: the focused pane's, or the selected row's
  (§23.4). tmux starts a new window in the session's directory unless you pass `-c` (§20.14).
- **Any command can be in SHELLS**: a shell, `htop`, `ssh prod`, `claude` in `~/Downloads`. The
  section is still called SHELLS.
- **UX3.4 is amended**: `!` with no worktree around starts a shell in SHELLS instead of first asking
  for a project (§23.6).
- **The quick terminal doesn't change.** It is still one per worktree (`C-t`/`T`, §11.2h, §11.2i)
  and never appears in SHELLS. SHELLS is a separate feature, not a new form of the quick terminal.

### 23.2 What a shell in SHELLS is, and is not

- **It is a session** (§7.1) of kind `shell` or `command` (an `agent` too, when started there),
  with `loose = true` and a `slot`. Everything a session has works on it unchanged: `send`,
  `read`, `wait`, rename and retitle, stop and kill, restart, copy mode, the recorder, splits
  (`v`/`s`: a copy of it, a shell in its directory, as a pane of its window, §23.17), broadcast (`b` to the rows
  marked with `Space`: tmux's `synchronize-panes`), peek, the inbox, the filter, the finder.
- **It belongs to its workspace, not to a worktree.** It has no `worktree` and no `project`, gets
  no worktree environment and no ports (§10.6, §10.7), and runs no lifecycle scripts. It runs under
  **the workspace's profile** (§10.10), so `git` and `gh` in it act as the workspace's identity.
- **It stays where it was put (`loose`).** The daemon never places it under a worktree, wherever its
  directory is or becomes:
  - a `Cwd` spawn inside a worktree normally places the session there (§7.2); a loose spawn
    is not placed;
  - auto-registration (§10.1) may register the repository a loose shell `cd`s into, but never moves
    the shell into it;
  - only an explicit **adopt** (§23.8) moves it.
- **It is not the quick terminal.** The quick terminal is quiet, unnumbered, one per worktree, and
  stopped with its worktree. A loose shell raises attention like any session, has a number, and
  outlives every worktree.
- **It is not a chat.** It is a real PTY session with a terminal, in a directory, with no
  `chat.db` record. Its contents never go into any database (§14.2).

### 23.3 Slots: numbers that stay

- **A loose session always has a slot**: a whole number from 1 up. When it is made it takes the
  lowest free number in its workspace, or the one asked for (`--slot N`, `Alt-N` on an empty
  slot). When it is removed, its number becomes free. Nothing is renumbered when another shell
  goes: shell 3 stays shell 3 (tmux's `renumber-windows` is off by default, §20.14).
- **Moving.** `shells.move` (CLI `shells move N M`) gives shell N the number M; when another shell
  already has M, the two swap.
- **Keys reach 1–10**: `Alt-1`…`Alt-9` are slots 1–9 and `Alt-0` is slot 10, as on a keyboard's
  number row. Higher numbers are reached by address (`%12`), the finder, the shells menu, a click
  and the web's strip.
- **Sessions that lose their place join SHELLS.** This replaces the sidebar's unnumbered `other
  sessions` group (`tree.rs`), which disappears. Such a session becomes loose, takes the lowest free
  slot in its home workspace, and the daemon raises a note ("`make dev` moved to SHELLS as 4"):
  - a session whose project is let go (`ProjectRemove{release}`);
  - a session whose project leaves its workspace (`ws remove-project`);
  - a session started outside every project (`ptyro new --cwd /tmp -- htop`);
  - a session left running when its worktree goes: removal can't stop a busy one, and one set
    loose by hand (§23.8) stays.

  The one exception: a session started inside a repository that auto-registration would add is
  placed in it, as today. The daemon registers the repository during the spawn rather than
  leaving it to the 2-second sweep, so `ptyro new -C ~/dev/newrepo` still lands in `newrepo`.
- **A removed workspace's shells move to `main`** (as chats do, §22.2), each keeping its number
  when that is free there, else taking the lowest free one.
- **Exited rows keep their number.** With `[ui] shell_exit = "keep"` an exited loose session holds
  its slot until it is removed. With `close` (the default) it goes and its number becomes free.
  `C-r` on a kept one starts it again in the same slot.

### 23.4 Where a new shell starts, and what runs

- **Directory** ("the pane's"), from the first of these that has one:
  1. The selected sidebar row, when the sidebar has focus: a session's live directory
     (`live_cwd`, else its `cwd`), a worktree's root, a project's main checkout, a loose shell's
     live directory.
  2. The focused pane's session: its live directory (OSC 7, else probed, §8.7), else its `cwd`.
  3. `$HOME`. This covers an empty pane, a chat, home, or a session of another machine whose
     directory doesn't exist here (a loose shell is made on the machine that owns the workspace,
     §23.10).

  The CLI uses its own working directory, or `-C DIR`. The web uses the directory of the session it
  is showing, else `$HOME`. A directory that no longer exists falls back to `$HOME`, with a note
  that says so.
- **Command:** the configured shell (`[shell] command`, else `$SHELL -l`; §8.7), with shell
  integration as for any shell (§8.7, WIN1). Or the command given (`shells new -- htop`, the
  dialog's command line). Shell integration supplies the last command and its exit code, which
  the row shows and §23.9 uses.
- **Name** (the address, §2.3): a friendly pair (`quiet-jackdaw`), as for any shell. The slot is
  the handy address (§23.5), so the name doesn't need to say anything.
- **Label** (what the row shows, not an address): the title if one is set (`,`, then `C-t`), else
  the foreground program and the directory (`htop`, `vim · ~/dl`), else the directory (`~/dl`).
  The label follows what runs, as tmux's `automatic-rename` does (on by default, §20.14), while
  the name stays put. A shell itself at its prompt isn't "what runs": its program's name is compared the way the
  process list names it (`session::program_name`, and `programName` in the web's `shellkit.ts`,
  one table for both), so a Windows daemon's `C:\…\pwsh.exe` at its prompt is `pwsh` and the label
  is its directory, which reads with `\` (`…\app`). `$HOME` here is the platform's home
  (`%USERPROFILE%` on Windows, which sets no `HOME`).

### 23.5 Addresses

`%N` is slot N of the **context workspace**. Every command that takes a session takes it:
`ptyro send %3 'make test'`, `ptyro read %3`, `ptyro wait %2 --for idle`, `ptyro attach %1`.

- The context workspace is `--workspace WS`, else the workspace of the session the command runs in
  (`PTYRO_SESSION`), else the one the TUI showed last (as `project add` uses, §10.1).
- `work%3` names a workspace; `laptop2:work%3` a workspace on another machine (§16.4b).
- `%` because a shell doesn't treat it specially in a word's first position (`#` would start a
  comment) and tmux uses it for pane ids. It never appears in a name (`[a-z0-9._-]`), so no name
  can be mistaken for it.
- `%3.2` is pane 2 of shell 3's window (§23.17), and `%3` its first pane.
- An empty slot is an error that names the way out: `no shell 4 in main: \`ptyro shells new --slot
  4\` starts one`.

### 23.6 The TUI

**The section.** Under each workspace's projects, before CHATS (shells are what you do,
chats are what you ask), like CHATS in every other way:

```
▾ main
  ▾ ptyro …
  ▾ SHELLS             M-1…9  ＋
    1  ~/dl                ✓ curl -O …      <- last command, its exit
    2  htop                ● running
    3  ssh prod            ◉ bell           <- wants you
    4  make dev · ~/web    ✗ 2  12s
  ▾ CHATS                       ＋
```

- The heading shows the jump key (`M-1…9`, or whatever is bound to `shells.go 1`) and `＋`, which a
  click turns into a new shell. A workspace with no shells shows one dim row, `＋ new shell`.
  `[shell] section = "when-used"` hides the empty section, and `never` hides it always (§23.11).
- `⏎` or Space on the heading folds it, and so do `h`/`l`. `Z` folds it with the rest. The fold
  key is `shells:<ws>`.
- **Rows** are ordered by slot. Each shows its number, its label (§23.4), and then its state as a
  quick terminal's line does (§11.2h): the command running, else the foreground program, else the
  last command with `✓`/`✗`. Status glyphs and attention as for any session (§11.1). An exited row
  dims, and a lost one says `↻ ⏎ starts it again` (§23.10).
- **One number per row, one meaning per number.** A SHELLS row's number is its slot, drawn in the
  heading's colour. The sidebar's own numbering (`C-q 1…9`, §11.3) skips SHELLS rows, as it skips
  quick terminals. `j`/`k` include them, as they are sessions.
- **The filter** (§11.2i UX4.1) treats them as sessions: `running` keeps the live ones, `active` the
  busy ones. The heading goes when nothing under it is left.

**Keys** (defaults; every one is in `[keys]`, the settings panel and the palette, with which-key
showing it):

| Key | Action | Does |
|---|---|---|
| `Alt-1` … `Alt-9`, `Alt-0` | `shells.go N` (global: no prefix) | show shell N of the context workspace, full sized as anything shown is (UX3.1); **an empty slot starts a new shell in it, here** (a note says "shell 7 started"), so a number is also a key that makes one |
| `C-q C-c` (and `c` in the sidebar) | `shells.new` | a new shell in SHELLS, in the pane's directory, full sized: tmux's `C-b c` |
| `C-q e` | `shells.menu` | the shells menu: every shell of the workspace with its number and label. A digit shows that shell, `j`/`k` ⏎ too; `c` makes a new one, `s` a scratch one (§23.8), `x` stops the selected one, `m` moves it to another number, `w` watches it (§23.9). Works without `Alt`: `C-q e 3` |
| `!` with no worktree around | `shell.new` | **amends UX3.4**: in an empty pane, home, a chat, or with a loose shell or the SHELLS heading selected, `!` starts a shell in SHELLS instead of asking for a project. In a worktree `!` still starts a shell in the worktree |
| `` ` `` | `session.last` | back and forth between the last two shown, as now (tmux's `last-window`) |
| `,` `x` `X` `C-r` `.` `b` | as for any session | rename (C-t: title), stop, kill, restart, its menu (with Move to…, Watch…, Adopt into a worktree…), broadcast |

- **The pane's header** for a loose session carries the strip, tmux's status line: `SHELLS  1 ~/dl
  [2 htop]  3 ssh ◉  4 make  ＋`. The shown one is in brackets, a shell that wants you has its
  glyph, and a click on a number shows that shell (on `＋`, makes one). The strip is drawn only on
  a loose session's pane, so no other pane gets busier.
- **Why `Alt` with no prefix**, and the cost. tmux's prefix-and-digit is two keys. Many tmux users [U]
  bind `M-1…9` themselves, and the point of SHELLS is one-key reach. `Alt`-digits then don't reach
  the program in the pane: readline's `digit-argument`, irssi and weechat's windows. Two fixes:
  unbind them (`[keys] "global:M-1" = "none"`, or one switch in the settings panel's Keys section
  that frees all ten), or press `C-q e N`, which always works. On macOS, Terminal.app and iTerm2
  type `¡™£` for Option-digit unless Option is set to act as Meta (`docs/keys.md`). Ghostty, kitty,
  WezTerm and Alacritty (with the kitty keyboard protocol) send `Alt`. When the TUI starts in a
  terminal that will type characters instead (`TERM_PROGRAM` is `Apple_Terminal`, or iTerm2 without
  the kitty protocol), the hints and the heading show `C-q e 1…9` instead, and the health badge
  (`M`) offers the terminal setting that turns them on. SH.0 checks this in each terminal before
  the default is fixed.

### 23.7 The CLI

`ptyro shell` (singular) stays the quick terminal (§11.2h). The plural group is SHELLS, and
`ptyro shell --help` points to it:

```text
ptyro shells [ls] [--workspace WS] [--all] [--json]   # the workspace's shells, by number (--all: every workspace)
ptyro shells new [-- CMD…] [-C DIR] [--slot N] [-n NAME] [--title T] [--scratch] [-w WS] [-d] [--json]
ptyro shells go N [--workspace WS]                    # attach with shell N shown (= ptyro attach %N)
ptyro shells move N M [--workspace WS]                # renumber; swaps when M is taken
ptyro shells loose <session>                          # set a worktree's session loose into SHELLS (§23.8)
ptyro shells adopt <session> [WORKTREE]               # move a loose one into a worktree (default: where its directory is)
ptyro shells watch <session> [--done|--activity|--silence DUR|--match RE|--off]   # §23.9
```

- `ls --json` prints `[{slot, id, name, label, kind, state, cwd, live_cwd, command, last_exit,
  watch, attention}]`. Text is one line a shell: `3  ssh prod  ◉ bell  ~`.
- `new` attaches in a terminal unless `-d` (§12.1); detached it prints the slot and name
  (`4 quiet-jackdaw`), `--json` the session. A script (no terminal) never attaches, so
  `ptyro shells new --slot 9 -- htop` works there as it is.
- `ls` and the session commands with `%N` work against other machines' workspaces as the session
  commands already do (§16.4b).

### 23.8 Moving between SHELLS and worktrees, and scratch shells

tmux has `break-pane` and `join-pane`; SHELLS has these:

- **Set loose** (`shells loose`, a session's menu: "Move to SHELLS"): a worktree's session becomes
  loose with the next free number. It keeps its process, its environment (including the
  worktree's variables and ports, which it started with) and its profile. Use it for a dev server
  or a long job that should outlive the worktree.
  - **Removal safety (§10.8) still counts it.** Removing or archiving a worktree treats every
    session whose live directory is inside that worktree as the worktree's own for the busy
    check, loose or not. Removing the directory out from under a running command is what §10.8
    exists to prevent.
- **Adopt** (`shells adopt`, a loose row's menu: "Adopt into a worktree…"): a loose session joins a
  worktree, by default the one its live directory is in, and gives up its slot.
  - **Refused when the profiles differ.** A session's profile is fixed (§10.10), so the error
    offers a new shell in that worktree instead.
  - It keeps the environment it started with. It gets no worktree variables or ports, and the
    menu says so before it moves.
- **Scratch shells** (`shells new --scratch`, `s` in the shells menu, the web's ＋ menu; R-UX-03):
  a shell in a new, empty directory `<data>/scratch/<id>/` (0700). Its row shows `◌`. When it is
  removed the directory goes with it. A directory that still holds files asks first ("keep
  `scratch/7f3k` (3 files)?"), and `ptyro gc` lists the ones left behind.

### 23.9 Watching a shell, and long commands

A loose session raises attention like any session (a bell, OSC 9/777, an agent in it that waits).
Two additions, built on the OSC 133 marks shell integration already supplies:

- **Long commands** (`[shell] notify_after`, default `30s`): a command in a loose shell that ran at
  least this long and ends while the shell isn't shown on any client raises attention:
  `done ✓ make test (4m12s)`, or `error ✗2 make test`. It goes through the notifier like any
  attention (§9.5: mutes, quiet hours, the inbox). `off` turns it off. This is what tmux users get
  from `monitor-activity` and `monitor-silence` (both off by default, §20.14), told by the exit
  code instead of guessed from output.
- **Watch** (per shell; `w` in the shells menu, the row menu, `shells watch`, the web's ⋯): one of
  - `done`: when the running command ends, however short it was;
  - `output`: when new output arrives while it isn't shown (tmux's `monitor-activity`);
  - `silence DUR`: when it has printed nothing for DUR (tmux's `monitor-silence`), such as a
    build that hung;
  - `match RE`: when a line matches, such as `ERROR|listening on`.

  A watch fires once and then clears (`done`, `match`), or stays until it is turned off (`output`,
  `silence`). The row shows `◎` while one is set. It is kept with the session, and lost with it.

### 23.10 Daemon restarts, upgrades and other machines

- **A live upgrade** (§4.5, the handover WIN0.5 decided for Windows) keeps loose sessions running,
  as it keeps every PTY. Slots, watches and labels cross over with the session.
- **A daemon restart or a reboot** ends the processes. tmux loses its windows too, and its users
  install tmux-resurrect. `[shell] restore` decides what comes back (default `offer`):
  - `offer`: each loose session comes back as a **lost** row in its slot, with its name, title,
    last directory (`live_cwd`) and command, and its last screen from the recorder (§7.6) when one
    was kept. `⏎`, `C-r` or `Alt-N` starts it again there, in that directory, with that command.
  - `start`: loose *shells* start again by themselves in their last directory. Loose *commands*
    (`ssh`, a dev server) are still only offered, since running them again unasked might do
    something.
  - `forget`: they're gone, as before.

  A lost row's screen is the recorder's file, never a database (§14.2).
- **Other machines** (§16.4b): a linked machine's workspace shows its own SHELLS, and its shells
  run there. `＋`, `Alt-N` and `%N` act on the context workspace's machine, so `Alt-2` on a
  `laptop2` workspace shows `laptop2`'s shell 2. Starting one needs `full` scope on a paired device,
  as any spawn does.

### 23.11 Configuration

Each option gets a schema entry in the **Shells** section (`crates/ptyro-core/src/settings.rs`,
beside `shell.command` and `shell.integration`) and is editable in the TUI's and the web's panel
and with `ptyro config set`:

| Key | Kind | Default | Help |
|---|---|---|---|
| `shell.section` | pick `always` / `when-used` / `never` | `always` | Show each workspace's SHELLS section in the sidebar: always (with `＋ new shell` when it is empty), only once it has a shell, or never (the keys still work). |
| `shell.notify_after` | duration or `off` | `30s` | A command in a SHELLS shell that ran at least this long tells you when it ends, if you weren't looking: done ✓ or error ✗ with its exit code. |
| `shell.restore` | pick `offer` / `start` / `forget` | `offer` | After ptyro restarts (not an upgrade), what happens to SHELLS: each comes back as a row that ⏎ starts again in its last directory, shells start again by themselves, or they're gone. |

Keys are `[keys]` entries like every other: `"global:M-1" = "shells.go 1"` … `"global:M-0" =
"shells.go 10"`, `"C-c" = "shells.new"`, `"e" = "shells.menu"`, `"nav:c" = "shells.new"`. The Keys
section gets one more switch, **Alt-digits jump to shells**, which binds or frees all ten at once.

### 23.12 Protocol (additive; built as 1.29)

- `SessionInfo`: `loose: bool`, `slot: Option<u32>`, `watch: Option<Watch>`, `scratch: bool`
  (all `serde(default)`, skipped when empty).
- `SpawnSpec`: `loose: bool`, `slot: Option<u32>`, `scratch: bool`. A loose spawn needs `workspace`
  (else the client's last one); with `slot` taken it is refused (`SlotTaken{slot, session}`), and
  the TUI then shows that shell instead.
- `ShellMove{session, slot}` → `Session` (swaps when taken).
- `ShellPlace{session, loose: true}` (set loose) | `ShellPlace{session, loose: false, worktree?}`
  (adopt) → `Session`. Refused with the reason (`profile differs`, `not loose`).
- `ShellWatch{session, watch: Option<Watch>}` → `Session`. `Watch = Done | Output | Silence{ms} |
  Match{re}`.
- `Resolve` learns `%N`, `ws%N` and `machine:ws%N` (§23.5). It takes the client's context workspace.
- Events: the usual `SessionChanged`, plus `ShellMoved{workspace, from, to}` so clients can redraw
  the strip without a full session list.
- Scope: `ls`/`read` are `read`; new, move, place and watch are `full`, as spawns are.

### 23.13 Storage

One migration of `ptyro.db` (metadata only; §14.2): `session.loose INTEGER NOT NULL DEFAULT 0`,
`session.slot INTEGER`, `session.watch TEXT` (JSON), `session.scratch TEXT` (its directory), and a
unique index `(workspace, slot) WHERE loose AND ended_at IS NULL`. The index makes "one shell per
number" the database's rule, not only the daemon's. The handoff image (§4.5) carries the same
fields (`Place.loose`, `Place.slot`, `serde(default)`).

### 23.14 The web

The web has no splits and no `Alt` it can count on: on macOS the browser types characters, and
`Cmd`-digit switches tabs. It gets the same things, shaped for a page and a phone:

- **The session list** gets a SHELLS group above its chats aside. It holds numbered cards with
  their label and state, a `＋` with a menu (new shell, scratch shell, a command), and the empty
  `＋ new shell`.
- **A shell's page** has the strip at the top (§23.6): `1 2 [3] 4 ＋`, which you tap to switch, and
  swipe left and right on a phone. Its ⋯ menu has rename, move to number…, watch…, adopt…, stop.
- **Keys on a desktop browser** while the page (not the terminal) has focus: `1`…`9` on the session
  list open that shell. In the terminal, keys belong to the session, so the strip is the way.
  Which chord, if any, reaches the page through a focused terminal canvas on Chrome, Safari and
  Firefox is checked in SH.0, not assumed.
- `ACTIONS` gains `shells`, `shells.new`, `shells.go`, `shells.move`, `shells.watch`,
  `shells.place`, `shells.scratch`, each in its `docs/parity.md` row.

### 23.15 Testing

- Daemon (`tests/shells.rs`):
  - lowest-free slots, `--slot` taken, swap on move;
  - removal frees a number and nothing renumbers;
  - a loose spawn inside a worktree isn't placed;
  - auto-registration doesn't move a loose shell that `cd`s into a new repository;
  - `release`, `ws remove-project` and a session started outside every project join SHELLS with a note;
  - worktree removal still refuses a busy loose shell whose directory is inside it;
  - adopt refuses another profile;
  - `restore = offer|start|forget` across a daemon restart;
  - a live upgrade keeps slots and watches;
  - `notify_after` fires once for a long command and not for a short one;
  - each watch kind;
  - `%N` resolution with and without `--workspace`;
  - a scratch directory goes, or asks.
- TUI (`tree.rs` unit tests and the `m9`-style screen tests):
  - the section, its fold and the empty row;
  - the numbering split from the sidebar's;
  - `Alt-N` on a full slot and on an empty one;
  - `C-q e N`;
  - `!` with no worktree;
  - the strip, and the fallback hints when Alt can't be sent.
- Web: in Chrome against a sandbox (the WEB1 recipe): the group, the strip, ＋, the ⋯ actions.
  `make parity` covers every new command.
- `make verify-shells` (SH.0's script): sends `Alt-1` from each installed terminal through the
  TUI's input decoder and records what arrived.

### 23.16 Not here, and open

- **Not here:**
  - the quick terminal in SHELLS, or SHELLS in a drawer (the owner kept them apart, §23.1);
  - shells shared between workspaces;
  - tmux's session groups;
  - `ptyro import tmux` (R-UX-15, still its own idea);
  - a **tmux key preset** (`[keys] preset = "tmux"`: `c`, `0–9`, `n`/`p`, `&`, `,`, `w`). It is
    R-UX-42, `new`, and not part of SH: ptyro's defaults already use `n`, `p` and `w`, and a second
    keymap is a second thing to document and test.
- **Open, settled in SH.0 before SH.2:**
  - which terminals deliver `Alt`-digits by default, and the exact detection for the fallback
    hints;
  - whether Windows Terminal leaves `Alt`-digits alone (it binds `Ctrl+Alt+digit` to tabs [U]);
  - the web chord, if any;
  - whether `C-q e` is the right key for the shells menu. `e` is free after the prefix today, and
    `W` (tmux's `w`, choose-window) is the other candidate.

### 23.17 Windows: a number's panes **[decided 2026-11-06 (owner); built 2026-11-06 (SH.13)]**

tmux's window holds panes, and so does a number in SHELLS. The owner asked for it after a split
of a SHELLS shell opened a session in a worktree. The cause: `split_clone` sent a plain spawn in the
shell's directory, so the daemon placed it under the innermost worktree there (§7.2). The owner's
choices (2026-11-06): the layout lives in the daemon, the web shows one pane at a time with a pane
strip, and a split is always a shell.

- **A split is a copy.** `v`/`s` on a shell of SHELLS (CLI `shells split`, the web's ⋯ → Split
  beside/below) asks the daemon for `ShellSplit{addr, dir}`. The daemon starts a shell, whatever
  the source runs (as tmux's `split-window`), with these from the source:
  - its live directory, else where it started;
  - its workspace and profile;
  - the client's environment, else the source's.

  It is loose, never placed under a worktree, and becomes a new **pane** of the source's number.
  Panes don't take numbers: the next new shell still gets the lowest free number.
- **Panes.** `Shelf.pane` (from 1; 0 from before windows reads as 1). The sessions that share a
  number are one window. Its first pane is the window's row, and `%3` names it. `%3.2` names pane
  2 (every session address takes it; `ptyro_core::window::parse_address`).
  - A new pane takes the lowest free pane number above the window's first pane, so a copy never
    becomes `%N` (the window's row) ahead of the shell it copies.
  - At most four panes (`window::MAX_PANES`, the TUI's split limit), counting panes still
    starting: the count and the claim are one step under `PENDING`. A fifth is refused and
    pointed to `shells break`.
  - A window's layout is read, changed and written back under one lock (`ARRANGING`), so two
    splits finishing together keep both.
  - `shells move` is refused while a pane of either window is starting.
- **The layout** (`window::WindowLayout`): a split tree whose leaves are pane numbers, each
  split's share in thousandths (not floats, so layouts compare exactly), the focused pane, and
  zoom.
  - Every pane's `Shelf.layout` holds the same copy, written by `Daemon::relayout` whenever the
    window's panes change. So it moves with the number (`shells move` moves every pane), crosses
    a live upgrade with `Meta`, and is in the restart list (`Kept.pane`, `Kept.layout`).
  - `relayout` fits the layout to the panes there are (`WindowLayout::fit`): a gone pane's
    sibling takes its space, and a pane it lacks goes beside the last.
  - A window of one pane has no layout.
  - `SessionInfo.pane` and `SessionInfo.layout` carry it to clients.
- **Break and join** (`ShellJoin{addr, into?, dir}`). With no `into`, the pane moves to the
  lowest free number as a window of its own (tmux's `break-pane`); a window of one pane is
  refused. With `into: N`, it becomes a new pane of window N, `dir` from N's focused pane (tmux's
  `join-pane`).
- **What changes the window:**
  - a pane that goes: `forget_session` relayouts, and the next pane becomes `%N`;
  - adoption into a worktree: `ShellPlace{loose: false}` relayouts the panes it leaves;
  - a restart: the panes come back together in the number their first pane gets, keyed by the
    workspace they were saved in, so a gone workspace's window 3 never merges with main's.
    Panes a live upgrade already brought back hold their window's number. `reslot` renumbers a
    moved workspace's windows whole;
  - a shell that only landed in SHELLS still moves into the repository it `cd`s into (UX1.5),
    unless it is a pane of a window of several: then it stays with its window.
- **The TUI** (`crates/ptyro-tui/src/windows.rs`):
  - Showing any pane (⏎ on its row, `Alt-N`, the strip, the finder) shows the whole window as
    the daemon has it: `place` builds a screen from the layout, its pane ids the pane numbers.
    The screen it replaces is kept, so `` ` `` goes back.
  - `Alt-N` opens the window on the pane that had focus there.
  - `sync_window` (each `tick`) compares the window on screen with `App.window_seen`, the layout
    last agreed with the daemon:
    - a resize, a focus move or zoom made here is sent (`ShellLayout`);
    - a change made elsewhere is shown (another client's split, a pane that went), whole, with
      the daemon's focused pane, so two TUIs on one window settle on one focus rather than each
      sending its own;
    - the TUI's own sends are remembered (`App.window_sent`), so their echoes, which can arrive
      after a newer change here, are not taken for news;
    - nothing is sent from a screen that mixes the window with other sessions or lacks some of
      its panes;
    - the quick terminal's drawer stays open under the same pane, and doesn't count when deciding
      whether the screen replaced is kept.
  - The sidebar hangs a window's later panes under its row as `└.2`, and the window's row shows
    `⊞N`. The strip and the shells menu list windows. A pane's header strip says `[3.2 …]`.
  - `shells.break` and `shells.join` are in a shell's row menu and the palette; join asks for the
    number.
- **The CLI:**
  - `shells split [S] [--down] [-d]`, where S defaults to the session it runs in (it attaches as
    `shells new` does, §12.1);
  - `shells break S`;
  - `shells join S N [--right]`;
  - `shells ls` lists panes under their window, and its `--json` rows carry `pane`.
- **The web:** a shell's page in a window of several has a **pane strip** under the shell
  strip: `.1 [.2] .3 ＋`. Tap shows a pane (`shells.pane`), and `＋` splits. A swipe on the
  shell strip goes to the next window, on the pane that had focus there. The ⋯ menu has Split
  beside, Split below, Break out to its own number, and Join another window…. The list shows a
  window's later panes as indented `.2` cards. The web still shows one pane at a time (§23.14).
- **Not here:** splitting a worktree's sessions into windows (their splits stay a TUI layout,
  §11.2i), a desktop split view on the web, and more than four panes.

## 24. Bootstrap: agents, tools and updates **[decided 2026-11-06 (owner, ROADMAP BOOT); BOOT0 and BOOT1 built 2026-11-06; BOOT8, BOOT4, BOOT2, BOOT3, BOOT5, BOOT6, BOOT7 and BOOT9 built 2026-11-07: the feature is complete]**

The design is `docs/superpowers/specs/2026-11-06-bootstrap-agents-design.md`: one setup checklist
that installs the coding agents, git, the GitHub CLI and their prerequisites, walks through the
logins, and later offers updates. Each phase below records what it built and what the builder
decided where the design left it open.

### 24.1 The version table (BOOT0) **[built 2026-11-06]**

**Where it lives:** `crates/ptyro-agents/src/catalog.toml`, embedded with `include_str!` and read
by `ptyro_agents::catalog` (typed, `serde`, `deny_unknown_fields`; a table that failed to load would be empty, so nothing breaks, and the tests load it). Data, not code, so a fact can be
corrected without touching the logic, and the tests check every row on every load. Every value is
from the vendor's documentation (or, where marked, the vendor's installer script or the program
itself) on the row's `checked` date, and RESEARCH segment G records the same, with sources.

**Rows:** the agents Claude Code (`claude`), Codex, Antigravity (`agy`), OpenCode and Gemini CLI
(labelled "for Gemini API-key and enterprise accounts (others: Antigravity)"), and the tools
git, the GitHub CLI (`gh`) and Node.js (`node`). An agent's row id is its built-in name (§8.1).

**Per row:** `doc` (the vendor page) and `checked` (the date); `version` (the argv that prints the
installed version); `login` (`start`, and `check`, which exits 0 when logged in, absent when the
vendor documents none); `latest` (the newest-version source); `prereqs` (an item, its minimum
version when one is named, required or only recommended, and on which platforms); `methods` (in
order of preference) and `others` (other ways it may already be installed, with how each updates:
for BOOT5's updates and BOOT7's moves).

**Per method:** `platforms` (`linux`, `macos`, `wsl`, `windows-x64`, `windows-arm64`), `via`
(`vendor`, `npm`, `brew`, `winget`, `apt`, `dnf`, `xcode`, `node-dist`), `shell` (`sh`, or `powershell`
on Windows), `needs` (a program that must be on `PATH`, such as `apt-get`), `install`, `update`,
`auto_update`, `admin`, and for `node-dist` the `url` and `sums` templates. `Item::method(platform,
has)` picks the first method whose `needs` is present, so a Mac without Homebrew gets nodejs.org's
build and Linux gets apt or dnf by what is there.

**Marks:** every method, login and source carries `V`, `S` or `U` (§20 style), and anything not `V`
carries a `note` saying what is second-hand or missing. A value nobody documents is left out:
Antigravity's, Gemini CLI's and OpenCode's logins have no `check` (OpenCode's `auth list` was one until BOOT4: it lists providers, and nothing documents that it fails when none is logged in), and the tests insist such a login says why.

**Newest versions, without the network:** `Latest::url(platform, arch)` names what to fetch and
`Latest::parse(body)` reads the reply. Sources: `npm` (`{"version"}`), `github-release`
(`tag_name` less a prefix: `v` for gh, `rust-v` for Codex's channel), `plain-text` (Claude's
`latest`), `agy-manifest` (`{"version"}`, per `<os>_<arch>`) and `node-dist` (the newest LTS in
`index.json`). The tests parse replies recorded on 2026-11-06 in
`crates/ptyro-agents/tests/fixtures/catalog/` (trimmed to the fields that matter; `make catalog-fixtures` records them again) and reject a
reply that isn't a version (an HTML error page, the wrong tag prefix). git has no source: the
design checks only agents and ptyro. `catalog::Version` reads `2.1.319 (Claude Code)`,
`codex-cli 0.162.1`, `v24.13.0`, `gh version 2.57.0 (…)` and Git for Windows' `git version 2.51.0.windows.1` (2.51.0), and orders them (a pre-release before
its release); `at_least` checks a minimum.

**Decided by the builder** (the design's "verified when built"):
- **Codex installs with its standalone installer, not npm:** the vendor's docs now lead with it
  on every platform. So Codex no longer needs Node.js, and the design's "Node.js, because Codex
  needs it" default tick no longer follows. npm and Homebrew are `others`.
- **OpenCode is OpenCode 1** (`opencode-ai`): OpenCode 2 is beta and changed the server API the
  adapter speaks. On Windows it installs with npm (the docs recommend WSL), so it needs Node.js
  there.
- **Antigravity and Claude Code update themselves**; their `update` is the manual form.
  Antigravity's `agy update` is [S] (third-party guides, not the docs).
- **Admin:** no agent's recommended method needs it. apt, dnf, Xcode's tools and WinGet's
  machine-wide Git, gh and Node.js installers do (`admin = true`); the tests require any `sudo`
  command to say so.
- **Not handled:** musl Linux for Antigravity's manifest (its installer picks `_musl` builds);
  Alpine generally.

**Uses:** `locate::install_hint` is the table's first method for the platform (`doctor`'s note
for an agent run from npx's cache); BOOT1 builds the plan engine on the rest. A missing agent's fix
was `install_hint`'s command to copy until BOOT9 made it `ptyro install <program>`
(`locate::install_fix`, §24.10), the table deciding which programs setup installs here.

### 24.2 The plan engine, `ptyro setup` and `ptyro install` (BOOT1) **[built 2026-11-06]**

**One engine:** `ptyro_agents::setup` (`crates/ptyro-agents/src/setup.rs`, with `setup/host.rs`,
`run.rs`, `fake.rs`). A library, so the CLI runs it with no daemon (a fresh machine has none),
the daemon runs it for the web, and the TUI shows its own checklist on it, then installs through the CLI in a split (§24.5). Three steps:

1. **`survey(host, latest)`** looks at every row of the table through a `Host`: the item's
   program found (`locate::find`, so npm's, nvm's and the vendors' folders count, with ptyro's
   tools folders first on `PATH`), its `version` argv run (15 s at most) and read with
   `catalog::Version`; the methods' `needs` (and `sudo`, `curl`) present; and the Node.js apt or
   dnf would install (`apt-cache policy nodejs`'s `Candidate:`, `dnf -q -C info nodejs`'s
   `Version`, cache only, never the network; an epoch and the distro's revision dropped).
2. **`plan(survey, picks)`** gives one `Entry` per row: its `State`, `ticked` (picked, or needed),
   `picked`, `needed_by`, its `Action` (`via`, the command as typed, `admin`, `update`, `after`)
   or why there's none here (`blocked`: the programs its methods need, and the vendor's page).
   `Plan::steps()` orders the steps so each comes after what it needs (else the table's order:
   agents, then tools); `Plan::admin()` lists those needing administrator rights.
3. **`run(plan, host, admin, on)`** runs the steps, each event to `on`; a failure never stops
   the rest, only what needs the failed item (`Skipped`, "it needs Node.js, which didn't install").

**States** (the design left the model to the builder): `missing`; `installed`; `outdated`
(installed older than a newest version passed in: BOOT5 passes them, the engine then runs the
method's `update`); `other` (installed through one of the row's `others`, by its folder:
`node_modules`/`npm`/`_npx`, Homebrew's `Cellar`/`Caskroom`, WinGet's links, read where the file
really is (a link in `~/.npm-global/bin` names npm's folder); offered the move to the recommended
installer unless kept, BOOT7, §24.9); `too_old` (older than a minimum something ticked needs:
Node.js 18 for Gemini CLI's 20). Each entry has a `side`, only `local` until BOOT8 adds WSL's and
Windows'.

**Prerequisites:** only *required* ones tick (Node.js for Gemini CLI everywhere and for OpenCode
on Windows); recommended ones (git for Codex's PR helpers, Git for Windows for Claude) don't, but
git is a default tick. A ticked prerequisite can't be unticked while what needs it is ticked.
`npm` counts as present when Node.js is installed or about to be, so Gemini CLI plans through npm
on a machine with no Node.js yet.

**Node.js, the design's rule** (and BOOT0's review's open note): methods in the table's order,
skipping those whose `needs` is absent. apt's and dnf's are used only when the candidate version
is known and meets the highest minimum of what needs it (and, for one too old already, is newer);
an unknown candidate is not trusted. Homebrew and WinGet ship the newest, so they are used as they
are (`update` for one too old). Else **nodejs.org's build** (`node-dist`): the newest LTS from
`index.json`, the archive for the platform and architecture, its SHA-256 checked against
`SHASUMS256.txt` (a mismatch is refused and deleted), unpacked with `tar` (Windows 10's
`tar.exe` reads the zip), and moved into place only once unpacked (the old one kept until then).
Downloads use `curl` (Windows 10 has `curl.exe`), so ptyro carries no HTTP client for this.

**ptyro's tools folder:** `Paths::tools()`, beside the data directory, not in it:
`~/.local/share/ptyro-tools` (XDG), `%LOCALAPPDATA%\ptyro-tools`, `$PTYRO_HOME/ptyro-tools`.
`ptyro uninstall` leaves it, because the tools are the user's (the design's "`ptyro uninstall`
leaves installed agents and tools alone"). In it: `node` (nodejs.org's build) and `npm` (the npm
prefix used when npm's own global prefix isn't writable, as with a distro's Node.js: never `sudo
npm`; on Windows npm's prefix is the user's `%APPDATA%\npm` already). `setup::path_dirs` names
their program folders (`bin` under each, except on Windows):
- **`ptyro shell-init`** prints `ptyro_core::shell::path_lines` first, for the folders that
  exist, before the snippet's `PTYRO` guard, so every shell gets them, not only ptyro's: each
  added at the front of `PATH` once (bash/zsh `case ":$PATH:"`, fish `contains`, PowerShell
  `-notcontains`). A machine that never needed them prints nothing new.
- **The daemon's environment:** `base_env`'s `PATH` gets them in front at start-up, existing or
  not, so a session finds a tool installed after the daemon started.
- **After a run that used the folder,** the CLI offers to add `ptyro shell-init`'s line to the
  shell's rc file (`ShellKind::rc_line`, with a `# ptyro …` comment above it: exactly the form
  `ptyro uninstall` takes out), or says it's there already. On Windows it offers to put the
  folders in the user's `PATH` (`[Environment]::SetEnvironmentVariable(…, 'User')`, each once).
  `--yes` only says what to add, as `ptyro init --yes` leaves rc files alone.

**Administrator rights:** asked for once, before the first step that needs them, never through
ptyro. `Admin::Ask` (the CLI at a terminal): sudo's own `sudo -v` in the real terminal, then each
step's `sudo` finds it remembered; on Windows nothing is asked ahead, since WinGet's installers
raise their own UAC prompt. `Admin::Silent` (`--yes`): only when no prompt is needed (root, sudo's
remembered password or a `NOPASSWD` rule: `sudo -n true`; an elevated Windows terminal: `net
session` succeeds); otherwise each admin step is `Skipped` with its command to run. `Admin::Never`
(the daemon, for the web): never, whatever it could do, and the fix names the computer: "on the
computer itself: `ptyro install gh`". As root with no sudo (a fresh container), a command's
`sudo` runs through `sudo() { "$@"; }`.

**Running a step:** the method's `shell` argv (`sh -c`, PowerShell with `-ExecutionPolicy
Bypass`), stdin empty, stdout and stderr read line by line as they come (a progress bar's `\r`
keeps its last state), with ptyro's tools folders first on `PATH`, and on Windows
`CREATE_NO_WINDOW` (§20.12). An `Err` is the exit code and the last three lines. Afterwards the
item is looked for again, and `Done` carries the version found (none: "a new terminal may be
needed").

**Events** (`setup::Event`, JSON-tagged `event`): `elevating{ids}`, `blocked{id, reason, fix}`,
`started{id, name, command, admin}`, `line{id, text}`, `done{id, version?}`, `failed{id, error,
fix}`, `skipped{id, reason, fix}`, `logins{ids}`, `finished{report}` (`done`, `failed`, `skipped`,
`blocked`, `tools`). **Fixes:** a failure offers `ptyro install <program>`
again, the command to run yourself and the vendor's page; nodejs.org's build, the connection and
the page. **Logins:** after the installs, `logins` names what was installed, or picked and here
already, that has a login (`setup::login::after`); the CLI then walks through them (BOOT4, §24.4).

**`ptyro setup [-y] [--agents A,B] [--tools T,U] [--dry-run] [--json]`:** the checklist in a
terminal (stderr): Agents and Tools numbered (`[x]` picked, `[+]` needed by something picked,
with the method and "as administrator"), then Already here (✓ with version and path). Typing
numbers (or names) ticks and unticks, Enter installs, `q` or end of input quits. Defaults: the
design's ticks less what is installed (Claude Code, Codex, git, GitHub CLI; Codex needs no Node.js
since BOOT0). With `--agents`/`--tools`, exactly those (`none` for none; an agent in `--tools` is
an error naming the right flag); with neither, the defaults. `--yes` asks nothing; without it and
without a terminal it refuses and says to pass `--yes`. `--dry-run` prints the checklist and each
step's command (`--json`: the `Plan`); `--json` on a run prints each event as a JSON line on
stdout. `--where` is BOOT8's (§24.3), `--move` BOOT7's (§24.9).
**`ptyro install NAME… [-y] [--dry-run] [--json]`:** names as people say them (`setup::resolve`:
the id, the program or the name, any case, without spaces or dots: `agy`, `github-cli`,
`"Claude Code"`, `nodejs`); an unknown one exits 3 listing the known. It shows the steps and asks
`Install? [Y/n]` (end of input is no). Already installed: "✓ GitHub CLI is here already". Both
exit 0 when everything ran, 1 when anything failed or was skipped, and never talk to a daemon (an
explicit `--remote` elsewhere is refused: setup is about this computer).

**TUI:** `app.install` (palette) runs `ptyro install` in a split (UX1.21's `cli_actions`), a
real terminal where sudo can ask. `app.setup` was the same with `ptyro setup` until BOOT2 made it
the checklist on screen (§24.5).
**Web:** More → Set up ptyro → "Install agents and tools" (`setup.plan`, `setup.install` in
`web/src/settings.ts`): the plan with checkboxes (re-planned on each tick, so prerequisites tick
themselves), Install, and the run's lines from `setup_progress` events; administrator steps are
shown with `ptyro install …` to run on the computer. BOOT3 gave it its full place (Settings, a
first-run banner, §24.6).

**Protocol 1.31** (§6.4): `SetupPlan{picks?}` → `SetupPlan{plan}` (`read` scope: it only looks,
but runs each agent's `--version`); `SetupRun{picks}` → `SetupStarted{run, plan}` (`full`), then
`DomainEvent::SetupProgress{run, event}` to every watcher. One run at a time (two package
managers fight over their locks): a second is a conflict. An unknown name is `not_found` before
anything runs.

**Tests, without the network or real installers:** the engine's logic in
`crates/ptyro-agents/src/setup/tests.rs` (an in-process `Host`: defaults, ordering, the Node.js
rule, too old, other and outdated, admin per mode, a failure's skips, names, the plan as JSON);
the CLI end to end in `crates/ptyro/tests/boot1.rs` through the **fake machine**:
`PTYRO_TEST_SETUP=<dir>` (debug builds, `ptyro_core::test_knob`) makes `setup::host` a fake that
reads `<dir>/world.json` (platform, programs, installed versions, other installers, apt's or dnf's
Node.js, `elevation` ready/prompt/unavailable, whether `elevate` succeeds, per-item outcomes
`ok`/`fail`/`slow`/`quiet`) and runs each step as `ptyro fake-installer` (hidden, debug builds), a
real child whose output streams like an installer's and which "installs" by writing
`<dir>/installed/<id>`; `<dir>/calls.log` records each step and each elevation asked. They cover
success, a failure with its fix, live output (lines 300 ms apart arrive 300 ms apart), admin asked
once / never with `--yes` / refused, the checklist's ticks and quitting, the Node.js rule with the
tools folder on `PATH` through a real `bash`, the rc line `ptyro uninstall` removes again, and the
daemon's `SetupPlan`/`SetupRun` with its events and its never-admin rule. With the knob set, the
checklist reads its answers from stdin even when it isn't a terminal.

**Review fixes (2026-11-07):**
- **Blocked items count.** A ticked item with no way here (Linux without apt or dnf for git)
  gets `Event::Blocked{id, reason, fix}` before the steps and is in `Report::blocked`. `ok()` is
  false, so `ptyro setup`/`install` exit 1. The CLI, `--json` and the web all show it, and a step
  that needs a blocked item is skipped.
- **Windows' PATH follows installers.** After each successful step, `RealHost` rebuilds its `PATH`
  from the stored machine and user `Path` (read raw through `ptyro_pty::winreg::get_raw`,
  `%VAR%` expanded) with `refreshed_path`: every entry it had, then the new ones, with the tools
  folders in front. WinGet's Node.js adds `C:\Program Files\nodejs` only to the registry, and the
  next step's `npm` then finds it. The fake models it with `path_adds`/`path_needs` and a
  `stored-path` file. **The daemon follows too [final fix wave, 2026-11-07]:** a daemon started
  before the install read `PATH` once, so on Windows Codex (its `install.ps1` adds its folder to
  the user's `Path`), git and gh (WinGet, the machine's) stayed invisible to it until it
  restarted: "codex isn't installed", and an offer to install it again. Now
  `Daemon::refresh_path` merges `setup::stored_path()` (the registry's `Path` on Windows; the
  fake's `stored-path` under `PTYRO_TEST_SETUP`; nothing on Unix, whose `PATH` is the shell's)
  into the daemon's base environment with `refreshed_path`: when its own setup run finishes
  (before `finished` is sent), when any session starts or ends (a split's `ptyro install`, the
  person's own `winget`), and before it looks for an agent (`daemon_path`: the agent list, a
  spawn's program, the "isn't installed" check) or for gh. On Windows it sets the daemon's own
  `PATH` too (`winreg::set_process_path`; `set_var` is documented safe there in a threaded
  program), for what runs with the process environment (git). Test:
  `boot1::an_agent_setup_installed_starts_without_a_daemon_restart` (the daemon lists codex as
  not installed and refuses its session; its `SetupRun` installs codex into a folder only the
  stored Path names; then codex is listed from that folder and its session starts).
- **The user's `Path` is written as stored.** `add_user_path` reads `HKCU\Environment\Path` raw,
  puts the tools folders in front with `prepend_entries` (nothing else changed or expanded), keeps
  REG_EXPAND_SZ (`set_raw`), then broadcasts `WM_SETTINGCHANGE`. PowerShell's
  `[Environment]::SetEnvironmentVariable` would have expanded `%VAR%` entries for good and written
  REG_SZ.
- **Git Bash's PATH form.** For bash, zsh and fish, `path_lines` writes a Windows folder as MSYS
  writes it (`shell::msys_form`: `C:\Users\x` → `/c/Users/x`). `PATH` there is `:`-separated, so
  the drive's colon would split it. PowerShell keeps the native form.
- **A step ends when its program does.** `pump` stops reading 1 s after the program exits, so a
  background process left holding stdout (a daemon an installer started) can't hang it.
  `capture` kills at its limit.
- **WinGet runs with no input.** The table's WinGet commands carry `--source winget
  --accept-source-agreements --accept-package-agreements` (BOOT0's tests require them). Claude
  Code's WinGet line gained `--id … -e` too, the same package written exactly.
- **The daemon's one-run guard** is a drop guard, so a panicking run frees it.
- **Uninstall keeps the tools working.** The plan lists `ptyro-tools` under Kept, and on Windows
  the user `Path` entries in it. When it removes `ptyro shell-init`'s line from an rc file while
  the tools folder exists, it writes `path_lines` for that shell in its place (`rewrite_rc`, shown
  in the plan). If the file can't be written, the error gives the exact lines.

**Decided by the builder** (the design's "Left to the builder"): the states above; elevation per
mode as above; the engine in `ptyro-agents` (beside the table it reads; the protocol crate now
depends on it for the wire types); the tools folder beside the data directory; `curl` and `tar`
for nodejs.org's build; required prerequisites tick, recommended ones don't; outdated ones update
through the recommended method's `update`.

### 24.3 Windows: WSL, Windows, or both (BOOT8) **[built 2026-11-07]**

The design's goal 4: on Windows each agent and tool can go into the WSL distro, onto Windows
itself, or both; WSL is the user's to bring.

**Sides.** `setup::Side` is `local` (this computer: Linux, macOS, and WSL seen from inside),
`windows` or `wsl`. `setup::plan` labels a Windows machine's rows `windows`. A two-sided `Plan`
holds Windows' rows, then WSL's (`Plan::merge`; `side()`, `entry_on()`, `sides()`), and
`Plan::steps()` orders each item after what it needs on its own side. `Plan.wsl` is a
`WslStatus`: `usable{distro, version}`, `unusable{reason, fix}`, or `inside{distro}` (the engine
runs in WSL: the Windows side is set up from Windows, and the plan says how).

**Usable WSL** (`ptyro_client::wsl_setup::probe`): `wsl.exe -l -q` answers, it lists a distro
(tools' own, `docker-desktop` and the like, and anything that isn't a distro's name are left
out, as is a line starting with `-`; first run's WSL search uses the same filter), and `ptyro --version` answers in it (`remote::version_in`, the bridge's `PATH`). The
distro of `client.default_remote` (`wsl://<distro>`) is asked first, then the others in WSL's
order. Setup never runs `wsl --install`, installs a distro, or puts ptyro in one. Without a
usable WSL only Windows' side is offered, and the plan says why with the fix: `wsl --install`
for no WSL or no distro ("WSL isn't installed or has no distro" when `wsl -l -q` fails, as
Windows 11's inbox `wsl.exe` does without WSL; "WSL has no Linux distro" when it lists only
tools' own); for no ptyro, `wsl -d <distro> -- sh -c "<README's install line>"` (double quotes:
cmd.exe doesn't quote with `'`)
(`wsl_setup::install_line`); for a ptyro too old to answer `setup --survey`, the same line to
update it. Setup run again afterwards offers WSL's side (the design's "added later").

**WSL's half is done by WSL's ptyro**, through `wsl.exe` with the bridge's `PATH` and the
arguments passed as `"$@"` (`wsl_setup::ptyro_args`), so the two engines can't drift:
- what is there: `ptyro setup --survey` (hidden) prints its `Survey` as JSON; Windows' ptyro plans
  WSL's side from it with its own engine, so the checklist re-plans on every tick without asking
  WSL again;
- a run: `ptyro install --json <ids>` with the hidden `--admin=ask` (the person said yes
  already: run, and let sudo ask in this terminal, `wsl.exe` passing the terminal through),
  `--admin=silent` (`--yes`: only what needs no prompt) or `--admin=never` (a daemon, for the
  web). Its JSON events come back line
  by line and go out as Windows' own (`wsl_setup::run`); if it ends without its report, each step
  it didn't get to is reported failed with why and the command to run there.
- `ptyro install --admin=…`, in each of the three modes, also keeps WSL's ptyro quiet about what
  Windows' says already ("Looking…", "here already").

**CLI.** `--where wsl|windows|both` on `setup` and `install`. The default is the side the user's
daemon runs on: WSL's when `client.default_remote` (or `PTYRO_REMOTE`) is `wsl://<distro>` and
that distro is the usable one (`wsl://`, the default distro: any), else Windows'. When the
daemon's distro can't be reached but another has ptyro, the default is Windows and a line says
so (`--where wsl` installs into that other one). `--where windows` doesn't look at WSL at all; `--where wsl` without a
usable WSL exits 1 with the reason and fix; `--where both` without one says so and sets up
Windows. On Linux and macOS only `--where local` is accepted (exit 2 otherwise, saying sides are
Windows'); inside WSL the message says to run `ptyro setup --where windows` in a Windows
terminal. `--remote wsl://…` on setup is refused with a pointer to `--where wsl`. The checklist
shows a column each (`Windows`, `WSL (<distro>)`: `[x] WinGet`, `[+] apt (admin)`, `✓ 2.43.0`),
with each side's notes under the row; a number ticks it on the default side(s), and `3:wsl`,
`3:windows`, `3:both` puts item 3 exactly there. A run does Windows' side, then WSL's, under a
heading each; with `--json` on Windows each line is a `setup::Sided` (the event with `side`).
The exit code covers both sides.

**TUI.** `app.setup` and `app.install` gain the `{where}` template word (`cli_actions`): on
Windows a picker (WSL, Windows, both; the daemon's side first). With this computer's daemon the
split runs `ptyro.exe … --where X`. With WSL's daemon, splits run inside WSL: `wsl` runs WSL's own
ptyro (its side natively, sudo in that terminal); `windows` runs this `ptyro.exe` through interop
(its path as WSL names it, `/mnt/c/…`) with `--where windows --tty`, the hidden `--tty` saying a
person answers although the console is a pipe there (Windows' admin prompts are UAC dialogs, not
the terminal); `both` opens the two splits.

**Web, daemon.** Protocol 1.32 (additive, §6.4): `SetupPlan` and `SetupRun` carry `wsl_picks`
beside `picks`, and each entry its `side`. A daemon on Windows probes WSL like the CLI (the result kept a minute, as the
page re-plans on every tick; forgotten after a run) and plans `wsl_picks` beside its own; its
run does both sides with `Admin::Never` on each, so WSL's admin steps show their command too. The
page shows a Windows and a WSL checkbox per item when WSL is usable (`setup.where`), the reason
and fix when it isn't, and with the WSL daemon most Windows users run, "this is WSL" with the
Windows command. Its log tags lines `WSL:`/`Windows:` and its summary waits for both reports.

**Tests, with no WSL:** `ptyro fake-wsl` (hidden, debug builds; `ptyro_testkit::fake_wsl`) stands
in for `wsl.exe` through `PTYRO_WSL` (a script on Unix, a `.shim` stand-in on Windows):
`PTYRO_FAKE_WSL=<dir>` holds `distros`, and per distro `ptyro/` (present: ptyro is installed),
`fake/` (its own fake machine), `old` (a pre-setup ptyro); it runs this same ptyro as the
distro's and logs each call. `crates/ptyro/tests/boot8.rs` covers no WSL / no distro / no ptyro
/ an old ptyro (Windows only, with the fix, never installed), the default side following
`client.default_remote`, one plan of both sides, both sides installing with sided JSON events and
a failure in WSL, sudo asked in the terminal through `wsl.exe` (and not with `--yes`), the
two-column checklist's `N:side` ticks, sides refused on Linux and inside WSL, a Windows
daemon's both-sided run with no admin, and one test with no platform faked that checks the real
sides on Windows (CI's `windows` job) and the refusal elsewhere. `boot1.rs` points `PTYRO_WSL` at
nothing so a faked Windows never reaches a real WSL.

**Decided by the builder:** WSL's side is planned from its survey by Windows' engine (re-planning
without a `wsl.exe` round trip per tick), then run by WSL's own `ptyro install`; the run order is
Windows then WSL; each side reports its own `finished`; a Windows daemon keeps the WSL probe a
minute; a daemon inside WSL doesn't reach Windows' side (it has no Windows ptyro it can rely on):
the page names the command instead.

### 24.4 Logins (BOOT4) **[built 2026-11-07]**

The design's goal 5: each installed agent and the GitHub CLI logged in once, to the default
profile, through the tool's own flow; setup ends by saying how to add more accounts.

**The engine** (`ptyro_agents::setup::login`): `status(host, item)` is `logged_in`, `logged_out`,
`unconfirmed` (the table names no `check`: Antigravity, Gemini CLI, OpenCode) or `missing`;
`rows(host, ids)` lists them (`LoginRow`: id, name, side, state, the `start` command as typed,
the `check`, the table's note); `log_in(host, item)` runs `start` in the person's terminal, then
asks `check` again; `after(plan, report)` is what a run walks through. Two `Host` methods:
`status(argv)` (the exit code, at most 20 s, then killed) and `interact(argv)` (stdin, stdout
and stderr the person's own, to its end). Programs are found as the survey finds them
(`locate::find`: npm's, nvm's and the vendors' folders).

**Credentials never pass through ptyro.** "Logged in" is the `check` command's exit code alone:
its stdout and stderr go to the null device, unread (`gh auth status` prints the account and a
masked token; none of it reaches ptyro). The login itself is the tool's, in a terminal ptyro
doesn't read. No pattern matching on output: no row needs it, and not reading is the stronger
promise. A row's `check` must exit 0 only when logged in; OpenCode's `auth list` couldn't be shown
to, so it was taken out of the table (§24.1) and OpenCode is `unconfirmed`.

**Where a login runs** (the design's "where the user sees a browser or device code"):
- **The CLI:** in the terminal it was typed in, with no daemon (a fresh machine has none). On
  Windows the child shares the console (no `CREATE_NO_WINDOW`, which would take the console
  away; no window opens either). While it runs, Ctrl+C is the login's (an agent quits with it, a
  flow gives up), not ptyro's: a handler (a thread with its own Tokio runtime, from the first
  login) ignores it during a login and exits 130 otherwise, so Ctrl+C at any other moment still
  ends ptyro.
- **The TUI:** `app.login` runs `ptyro login {where}` in a split, a real terminal.
- **The web:** More → Set up ptyro → Log in. `login.list` (`POST /api/host`, `full` scope) is
  `ptyro login --check --json --where local`; Log in (`login.start`) spawns a session on the
  daemon running `ptyro login --where local <name>` under the default profile, in the home
  folder, unrecorded (§7.6: what the sign-in prints, or a code pasted into it, never reaches
  disk; `profile login`'s session is unrecorded too), and opens it. `--where local` keeps both
  on the daemon machine's side, with no WSL probe. The tool's
  link and code show in that terminal. A browser the tool opens opens on the computer running
  ptyro, which helps only someone sitting there, so the page says to follow the link and enter
  the code. Check again asks the status commands. No new daemon request; the one protocol change
  is 1.33's `SpawnSpec.unrecorded`. **[final fix wave, 2026-11-07]** Both ops take `where`
  (`local`, the default, or `wsl`): on a Windows daemon with a usable WSL the page lists WSL's
  logins under their own heading (`--where wsl`, WSL's own ptyro through `wsl.exe`), and its Log
  in runs `ptyro login --where wsl <name>` in the session, so WSL's side is logged in from the
  page too. The session runs this ptyro's installed name (`own::installed_name`, as the
  self-update's session does): after `ptyro update ptyro`, `current_exe` can be `…/ptyro
  (deleted)` or `ptyro.exe.old`.
- **WSL's side** (from Windows): `wsl -d <distro> -- ptyro login <ids>` with the terminal passed
  through (`wsl_setup::interact`), so the login runs in that distro, by its own ptyro; `--check
  --json` there comes back as rows marked `side: wsl` (`wsl_setup::logins`). Windows' ptyro waits
  on it with Ctrl+C held off as for its own logins (Windows' console sends Ctrl+C to every
  process on it), so a Ctrl+C there skips that login and never ends ptyro.exe. Setup's walk there
  passes the hidden `--installed=<ids>` (setup's list, not names asked for: what it just
  installed) and `--tty` (a person is at this terminal).

**`ptyro login [NAME…] [--check] [--json] [--where wsl|windows|both]`:** no names, every item with
a login that is installed. Each logged in says ✓; each logged out runs its login ("▸ Logging in to
GitHub CLI: `gh auth login --web`", then a line saying the sign-in is the tool's and Ctrl+C skips
it) and ends ✓, or ✗ with the fix (`ptyro login gh` tries again, or the command to run yourself).
One that can't be checked runs only when named (`ptyro login agy` starts Antigravity: "sign in,
then quit it to come back here") and ends "?", saying it can't be confirmed; quit non-zero (Ctrl+C,
or however the agent quits) it says "skipped", not a failure. A login ending non-zero for a tool
that can be checked says how it ended, with the fix. A named one not
installed says `ptyro install …`; one without a login (git) is refused (exit 2); an unknown name
exits 3. It needs a terminal (the TUI's split and a web session are terminals); without one it
refuses and points at `--check`. `--check` only says where each stands (what isn't installed only
when named); `--json` (implies `--check`) prints the rows. Exit 1 when any ends logged out (or a
named one isn't installed). `--where` and its default are setup's (§24.3); `--remote` is refused,
as for setup: logins are this computer's.

**Setup's end:** after every side's run, `ptyro setup` and `ptyro install` walk through
`login::after` per side, under a heading each (WSL's in WSL, as above), asking nothing more: the
person chose these by ticking them, and Ctrl+C skips any one. One that can be checked runs when
logged out; one that can't runs only if this run just installed it (it can't have signed in yet),
so Antigravity already here isn't started on every setup. One just installed but not found from
this terminal yet says to open a new terminal, then `ptyro login <name>`. With `--yes` (no person) they only
say where each stands and that `ptyro login` logs them in; with `--json` nothing is said (the
`logins` event is in the stream); WSL's ptyro running for Windows' (`--admin=…`) leaves them to
Windows'. When there were logins, setup's last line is `login::PROFILES_HINT`: more accounts are
profiles, each with its own logins (`ptyro profile new work`, then `ptyro profile login work
claude`). The exit code stays the installs': a login not done is said with its fix, never a
failed setup.

**Tests, with no real login and no credential:** the fake machine's `world.json` gains
`logged_in` (ids logged in already) and `logins` (per id: `ok`, `fail` (exit 1), `nothing` (exit
0, still logged out), `slow` (two seconds before it logs in)). `interact` runs `ptyro fake-installer login ID` in the terminal, a real
child that prints a device code and writes `logged-in/<id>`; `status` answers for the rows'
`check` argv from those files; both log to `logins.log` (not `calls.log`, whose steps BOOT1's
tests count). `crates/ptyro/tests/boot4.rs` covers setup's walk after the installs with the
profiles line last; `--yes` running none; a failed and a "nothing done" login with their fixes;
the unconfirmed one only when named; `--check`/`--json` asking only the status commands; WSL's
side logged in in the distro through `ptyro fake-wsl`, and both sides' rows; the web's
`login.list`, and a `login.start` session showing the code, ending logged in, and leaving no
recording; the web's WSL side (`the_web_lists_and_runs_wsls_logins_too`: `where: "wsl"` lists
and logs in the distro's, never Windows'); an unconfirmed agent started only when just installed or named, and quitting it a
skip; a just-installed one not found yet pointing at a new terminal; the `--where` refusal's
words; and (Unix) SIGINT to ptyro during a login, its own and WSL's through `fake-wsl` (the
fake's `slow` login), leaving it running to the end. The engine's rules are in `setup/tests.rs`.

**Decided by the builder:** the exit code alone decides (no output read, so no patterns);
OpenCode's `check` dropped; the CLI runs logins in its own terminal and the web in a daemon
session running `ptyro login`, so both confirm the same way; setup walks without a question per
login; an unconfirmed login runs only when named or just installed (never because it was picked
and was here already), and quitting it is a skip; login sessions are unrecorded (protocol 1.33); the web shows the daemon's
side (`--where local`), naming WSL's command on a Windows daemon.

### 24.5 The TUI checklist (BOOT2) **[built 2026-11-07]**

The design's "where the checklist lives": the first part of the onboarding screen
(`crates/ptyro-tui/src/onboard.rs`, UX1.19), and the same screen with only that part any time
later. The owner chose one checklist screen (option A); the approved mockup's shape is kept:
Agents, Tools and Already here, a line per item, prerequisites ticked with the reason, and
"⏎ install N · Space toggle · s not now".

**One set of ticking rules.** The CLI's checklist and the TUI's share
`ptyro_agents::setup::checklist` (moved out of `crates/ptyro/src/setup.rs`): `Sides` (this
computer's survey, WSL's, and WSL's status), `Picks` (what each side has ticked), `rows` (what can
be ticked: not here on some side), `already` (here on every side), `toggle` (picked → unticked;
else ticked where it can be; a prerequisite something ticked needs stays, saying "untick that
first"; what's here can't be), `default_place` (BOOT8's default side) and the `mark`/`cell` words.
The plan is still `setup::plan`; the TUI has no plan or run logic of its own.

**Where it looks** (`crates/ptyro-tui/src/setup_list.rs`): in the TUI's own process, as `ptyro
setup` does: `setup::host` + `setup::survey` on a blocking thread, and on Windows
`wsl_setup::probe` and `survey` for the distro, the daemon's (`wsl://<distro>`, the TUI's
connection) asked first. It arrives as `AppMsg::SetupFound`; until then the part says "looking at
what's installed…". The default side is the daemon's (`checklist::default_place`), with the
reason when it isn't. A daemon on another computer (`--remote`, not this Windows' WSL) is set up
there: the first run says so, and `app.setup` runs that computer's `ptyro setup` in a split, as
before.

**On screen:** under Agents and Tools, each row is `[x]` picked, `[+]` ticked for something
picked, `[ ]` not, then its name and one line, parts joined by ` · ` and wrapped between parts:
the catalog's new `about` (one line per row, `catalog.toml`; Gemini CLI's label takes its place),
the method it would use (`through` the action's when ticked, else the table's first method whose
`needs` is present), "asks for admin", "needs Node.js" (required prerequisites), "ticked because
Gemini CLI needs it", an outdated or too-old state, "can't here: …" for a blocked one, and "then
log in" for an item with a login. On Windows with a usable WSL each row has a cell per side
(`Windows`, `WSL (Ubuntu)`: `[x] WinGet (admin)`, `[ ]`, `✓ 2.43.0`), the cursor's cell
reversed, and a line under the row per side for what a cell can't say (why it can't, what needs
it). Already here lists each item with its version and path (each side's, named, on two), the
default agent marked "your default agent". It fits 80 columns: a cell is 26 wide (an ellipsis
ends a longer one), and when the cells leave less than 30 columns beside them the row's line goes
under it, indented, wrapped between parts like the rest. The per-side lines under a row, WSL's
state, the default side's reason and what a tick said wrap between words (a word longer than a
line, a URL, breaks where the line ends, so all of it stays on screen). Then WSL's state when it is unusable or this is WSL, the
default side's reason, and what the last tick said.

**Keys:** `j`/`k` (arrows) move over the checklist's rows, then the first run's choices; `Space`
ticks the row (on the cursor's side) or the choice; `←`/`→` (`h`/`l`) pick the side; `⏎` installs
what's ticked and, in the first run, writes the config and registers the repositories as before;
`s` is "not now": no installs (the first run's other choices still count; alone, it closes and
says the key that brings it back); `Esc` skips the first run (an empty config) or closes the
checklist. The footer counts the steps: "⏎ install 5 and use these" (first run), "⏎ install 4 ·
Space toggle · j/k move · s not now" (alone; `· ←/→ side` with two sides).

**⏎ runs the CLI.** What the person picked (not what ticked itself: the engine ticks
prerequisites again) goes to `ptyro install <names…> --confirmed` in a split (`run_cli`), a real
terminal, so everything the CLI does happens there unchanged: the order, live output, ✓/✗ with
the fix, administrator rights asked once (sudo's prompt in that terminal, or each installer's
UAC), the tools-folder `PATH` offer, the logins walked one by one (Ctrl+C skips one), and
`PROFILES_HINT` last. `--confirmed` (hidden, `ptyro install` only) skips the `Install? [Y/n]` the
checklist already asked; nothing else changes. On Windows (`cli_actions::where_runs`): with this
computer's daemon, the same picks on both sides are one `--where both`, different picks a split per
side (`--where windows`, then `--where wsl`); with WSL's daemon, WSL's side runs WSL's own ptyro in
its split and Windows' this `ptyro.exe` through interop with `--where windows --tty`. **One split
at a time** (`setup_list::SetupRun`, `App::setup_tick`): the next side's split starts once the one
before it ends, so two package managers, sudo and UAC prompts, or two sets of sign-ins never
overlap. **While it runs, the checklist doesn't open again**: `U`, the palette's `app.setup` and
`first.run`, and a health fix show the running split instead and say "setup is still running in
<split>: the checklist opens again once it's done" (no second install beside the first). That
guard is this TUI client's (`App::setup_run`): another TUI attached at the same time, or this one
after a reattach, doesn't know of the split and opens the checklist; but the machine's setup lock
(§24.6) is under every install, so that checklist says setup is already running (and from where)
and ⏎ starts nothing, and the web's runs are refused the same way. A split
is running from its spawn's reply on (it is in the session list from then); a failed spawn frees
the checklist at once, and a split closed or ended (its session gone, or not live) frees it on the
next tick, then the next side's starts. When the last split ends, the health badge looks again
(`health_again`): now, or, with a doctor run already in flight (whose answer may predate the
install), as soon as that one is in, so it stops offering what was just installed. Nothing ticked, still looking, `s`
or Esc: nothing installs, and the first run's message (or, alone, the checklist's) ends with how
to open it again (`C-q U opens the setup checklist (agents and tools) any time`).

**Where it opens:**
- **The first run** (no config): at the top of the first-run screen (`first.run` reopens the whole
  screen).
- **`app.setup`**: `U` (after the prefix, and in the sidebar; in the prefix-pause menu as "set up
  agents") and the palette: the checklist alone (" Setup ", "Set up this computer: …"), which writes no config.
- **The health badge** (`M`): `health::Fix::Setup(names)` for a doctor line suggesting `` `ptyro
  setup` `` or `` `ptyro install NAME…` ``, an agent not found (`claude not found: …`, in
  `agents`), git not found, and no agent at all: ⏎ opens the checklist with those ticked (none:
  the defaults), on the side a suggested `--where` names; flags' values are never names
  (`--agents`/`--tools` lists are). With a daemon on another computer, the fix runs that
  computer's `ptyro install <names> [--where …]` in a split (`ptyro setup` when it names none).
  The doctor's lines offer setup in BOOT9's words (§24.10): "No coding agent is installed:
  `ptyro setup` installs one (…)", "codex not found: `ptyro install codex` installs it".
- **`ptyro init`** (the CLI): its agents step asks "open the setup checklist, to install agents
  and tools?" (yes by default when no agent is found) and runs `ptyro setup`'s checklist in that
  terminal, then carries on; never with `--yes` or `--dry-run`.

**Tests, with no real installer:** the ticking rules and Windows' commands in
`setup_list.rs` (defaults and `install … --confirmed`, a prerequisite ticked with its reason and
kept, two columns and `--where both` vs. a split per side with WSL's daemon, Windows' ptyro out of
WSL's reach, a daemon elsewhere); the screen in `app.rs`'s tests (the first run's sections, lines
and footer count, Space on Gemini CLI ticking Node.js, ⏎'s `install` words and the config; the
checklist alone and `s`); `health.rs` (which findings open it, with what ticked);
`crates/ptyro/tests/boot2.rs`, `ptyro` in a PTY (`ptyro_testkit::headless`, `send_when`/`until`)
on BOOT1's fake machine: the first run's ⏎ installing the defaults in a split with sudo asked once
and each login, then the profiles line and the config; `C-q U` once while that install runs,
saying setup is still running, each item installed once; `C-q U` later, `s` installing nothing, and
Space then ⏎ installing only Antigravity (and its sign-in, just installed); `ptyro init` offering
the checklist. UX1.19's tour test now leaves the first run with `s`, so it installs nothing, and
both UX1.19 tests point `PTYRO_TEST_SETUP` at an empty fake machine, so the checklist never looks
at the machine running the tests. The unit tests add: a running install refused a second, and the
next side started only once the first ends; a failed spawn, a closed split and a died one each
freeing it, and the badge looking again after a doctor run in flight; the 80-column screens (two
sides with a per-side line, and Windows without WSL with its reason, fix and the default side's
note); a fix's `--where`. The headless test of a running install asserts the "still running"
message, then (once the split finishes in the daemon) that one install split ran and each item
installed once.

**Decided by the builder:** the checklist surveys in the TUI's process like the CLI (a daemon may
not exist yet, and WSL's side needs Windows' ptyro), not through `SetupPlan`; installs run as the
CLI in a split rather than through `SetupRun`, which never runs admin steps (the owner's
exception keeps those to the TUI and the CLI on the computer); `--confirmed` for the question
already asked; `U` for `app.setup`; the catalog's `about` lines; `s` keeps the first run's other
choices; the health badge opens it for missing agents and git (BOOT9 then rewrote the doctor's
lines to offer setup, §24.10).

### 24.6 The web checklist (BOOT3) **[built 2026-11-07]**

The design's "the web has it under Settings, and as a first-run banner when no agent is
installed", with the owner's parity exception: a step that needs administrator rights never runs
from the web (a sudo password or a UAC prompt would go through it, over the network); the page
shows the command to run on the computer, and the TUI's and the CLI's ways that do it. Recorded
as a waiver in the web cells of `setup` and `install` in `docs/parity.md`.

**One page, reached three ways.** BOOT1's More → Set up ptyro became the checklist page
(`setupView` in `web/src/settings.ts`, `#/setup`), not a second one beside it:
- **Settings → Agents and tools**: the first link of the settings panel's sections (wide
  screens) and a card at the top of its home (narrow ones), `#/setup/settings` (its back arrow
  returns to Settings).
- **More → Set up ptyro**, as before.
- **The first-run banner** (`setupBanner`, `setup.banner`) at the top of the Sessions tab while no
  coding agent is installed on any side (`noAgent`: every agent's state `missing`; an outdated
  one counts as there): "Set up agents and tools" opens the page; "Not now" hides it on that
  device (`ptyro.setup.later` in its storage). Only a device paired with `full` scope sees it
  (`bannerWants`): another could only reach a page saying it can't install. It asks the daemon's
  `SetupPlan` (the defaults, the same cache the page uses: one survey) once per page load, a
  failed ask tried again after 5 s, then twice as long each time up to five minutes; once any
  plan has shown this device an agent (`ptyro.setup.agent` in its storage), the banner stops
  asking at all. A plan that shows no agent (the setup page's) clears that again.

**The page:** the checklist first, then Log in (BOOT4's, with the profiles line), then the first
run's accounts and repositories. The doctor's "Your agents" list went: Already here says the same,
with versions and paths. The checklist is the TUI's shape (§24.5): Agents, Tools and Already
here; each row a checkbox, the name, and the TUI's own line under it; a prerequisite ticked for
something is checked and locked (untick that first); on Windows with WSL a box per side with that
side's cell ("WinGet (admin)", "✓ 2.43.0") and what it can't say. Already here: each item's
state and path (each side's on two), the default agent marked. The button counts what the web
will run ("Install 2": ticked, with an action that needs no administrator rights), or says
"Nothing to install from here".

**The words come from the engine.** The page has no catalog, so the daemon sends each entry's
line with the plan (protocol 1.34's `lines`, `checklist::lines`): `about` (what the TUI shows after
the name: the catalog's `about` or the label, the method, "asks for admin", "needs Node.js",
"ticked because …", an outdated state, "then log in"; without the method on two sides), `how` (a
side's cell without its mark), `why` (what a cell can't say) and `state` (in words). `about` and
`why` moved from the TUI (`setup_list.rs`, `onboard.rs`) into `ptyro_agents::setup::checklist`,
which both use, so the two can't drift. The page reads the plan's own fields (ticked, picked,
side, action) only for its checkboxes and counts.

**Administrator steps** (`setup.admin`, `adminSteps` in `web/src/setupkit.ts`): what is left for
the computer (`leftForComputer`): each ticked step that needs administrator rights, and each
ticked item that waits for one on its side (`action.after`, transitively: Gemini CLI after Node.js
from apt). A box per side gives the command to run on the computer, `ptyro install <ids>`
(`--where windows|wsl` when there are two sides), with Copy, the items it covers ("left for the
computer"), why a waiting one is in it ("Gemini CLI needs Node.js first, so it's in that command
too"), and the TUI's way ("press the prefix … then U", `app.setup`, which asks for the password
once) or `ptyro setup`. The run still gets every pick (so its plan is the one shown, and a
prerequisite the plan re-ticks is skipped the same either way); the daemon skips what is left,
with its fix (`Admin::Never`, §24.2). The page words it so: Install counts only what will run
(`webSteps`: "Install 1", or "Nothing to install from here"); a skipped line of the log reads
"git: left for the computer: …"; and the report keeps those apart from what wasn't run for other
reasons: "1 installed. git and GitHub CLI are left for the computer (administrator rights): run
ptyro install git gh there."

**One install at a time on a machine, from any client** (the review's ruling, 2026-11-07). The
guard is where every path meets: `ptyro_agents::setup::lock`, an exclusive lock on
`<state>/setup.lock` (the operating system's, `File::try_lock`: `flock` on Unix, `LockFileEx` on
Windows, so it is let go however the holder ends). `ptyro install` and `ptyro setup` take it
first, before the checklist is walked or `Install? [Y/n]` is asked (holding it across those
questions), and keep it through their installs (`go` in `crates/ptyro/src/setup.rs`; the logins
after them run without it; `--dry-run` takes none); the TUI's splits, which run them, do too.
The daemon's `SetupRun` takes it with its own flag (`Running`). The holder writes who it is to `<state>/setup.who` (a file of its own, since
Windows' locks keep others from reading the locked one): "`ptyro install`", "`ptyro setup`",
"the TUI's setup checklist", "`ptyro install`, for Windows' ptyro", "the web app, through the
daemon", each with its process id. A second run on that machine side stops before installing
anything there: "setup is already running (from the web app, through the daemon (process 4242)):
wait for it to finish, then try again" (the CLI's error, exit 5, `conflict`; on the web a toast).
Each machine side has its own: Windows' ptyro and WSL's keep their state apart, as their package
managers are apart. So a run of both sides checks WSL's lock only when it gets to WSL's half (WSL's
own `ptyro install` takes it): with WSL's held, Windows' half installs and WSL's is reported failed
with that message. A filesystem that can't lock doesn't stop an install (it goes ahead unguarded,
as before).

**Said up front, everywhere.** Who holds it is asked before anyone chooses
(`Lock::held_by`: only the operating system's lock decides, by a non-blocking probe that takes
the lock *shared*, so two probes at once never see each other as a holder [fixed 2026-11-07: an
exclusive probe made a checklist say setup was running when another one was only looking]. The
probe opens the file for reading as well: where `flock` is emulated with POSIX locks (NFS), a
shared lock is a read lock, which a file open only for writing can't take; held,
`setup.who` names who; free, a `setup.who` a holder left behind (Ctrl-C, a closed window, power
lost) is removed while the probe holds the lock, whatever process now has its id; the holding
process itself answers from its own set of held locks without probing, since a POSIX lock (NFS's
`flock` emulation) belongs to the process and a probe would let go of it; BOOT5 review):
- **the CLI** takes the lock before its checklist or question, so it refuses at once;
- **the TUI's checklist** looks when the survey is in (`Setup::check_busy`): it says "setup is
  already running on this computer (from …): ⏎ installs nothing now; open the checklist again
  once it's done", ⏎ counts nothing, and ⏎ looks again before opening a split (a toast with the
  refusal instead);
- **the daemon's `SetupRun`** refuses at once when another holds the lock, and takes it off the
  runtime's workers (the take waits out a probe's instant);
- **the web**: `SetupPlan.held_by` (protocol 1.34) names a holder that isn't the daemon's own run
  (while `running` is empty). The page says "Setup is already running on the computer (from
  `ptyro install` (process 4242)): Install waits until it's done", disables the boxes and Install
  ("Waiting for the other setup"), and asks again every 10 s while it is open (such a run sends it
  no events), offering Install once it is free. When the daemon's own run is the holder, `running`
  names it and the page joins it (below); a refused `SetupRun` asks for the plan, so the page then
  shows either.

Asking can't make a run starting that instant be refused: `Lock::take` tries again for 400 ms
while the lock is held (a probe holds it for microseconds, a run for minutes) before it refuses.
For an instant after a holder locks and before it writes `setup.who`, a refusal may name "another
ptyro": cosmetic.

**Following a run on the web.** The daemon's guard already refused a second `SetupRun`; a page
that hadn't started the run didn't know of it. Now the daemon keeps the run
going (`CURRENT` in `crates/ptyro-daemon/src/setup.rs`: its id, the plan it runs, its latest 500
events), numbers each event (`seq`, from 1), and every `SetupPlan` carries it as `running`. The
page follows it (`SetupLog.adopt`, `web/src/setupkit.ts`): "Setup is already running on the
computer …: its progress is below" at the top, its lines so far, every box and Install disabled
("Installing…") until it ends, then its report. Events of a run the page doesn't follow are kept
(the latest 600) and the first one makes the page ask for the plan again, so a run started
elsewhere while the page is open is joined too; joining adds what came between, by `seq`, once. A
refused `SetupRun` (started elsewhere a moment before) asks for the plan too, and an ask made
while one is out is made again when it lands (its survey may be from before the run), so a run
started just as the page opens is joined as well. A join that finds the run finished already, or a
run ending while the page was away, refreshes the checklist and the logins.

**Away and back** (a phone's socket drops; `setupConnected` on every new connection): a page
following a run, or showing the checklist, asks for the plan again. The same run still going: what
the page missed is added from `running.events` (beyond the last `seq` it had); a jump in `seq`
while connected (`"gap"`) holds the later events and asks too; when the daemon no longer has the
earliest missed ones, the log says how many aren't shown. Nothing running, asked for after the run
was known: it ended while the page was away, so the page says "It finished while this page was
away: the checklist below is how the computer is now", frees Install and refreshes the checklist
(the report itself is gone). A plan asked for before this page's own run was known never ends it. Its sides are counted
from the run's own plan (`SetupStarted`'s, or `running.plan`), never a cached one. Recording an
event and sending it happen under one lock, so a plan holds an event or comes before it. `CURRENT`
is cleared when the run lets go of its place (`Running`'s `Drop`, a panic included). The TUI's
splits run the CLI, which takes the machine's lock, so they and the web's runs exclude each other
too (the TUI keeps its own guard as well, §24.5, so it doesn't open a second split).

**Tests, with no real installer:** the logic without a browser, `web/test/setup.mjs` (`make
web-test`): the banner's rule and who it asks for (`full` only, not after "Not now" or an agent
seen); what is left for the computer per side, its commands, and Install's count; the report's
"left for the computer"; a run that ended while away (and not one asked before the run was known);
a gap filled from `running`, and what the daemon no longer has said; a run's
own events (early ones, sides from its plan, a duplicate shown once, an older daemon's unnumbered
ones); joining a run going already from `running` with what came between, once and in order; a
finish that arrives before the plan that names the run; not joining its own run while starting
it. `checklist::lines` in `setup/tests.rs` (one side and two). The daemon, `boot1.rs`
(`the_daemon_says_a_run_is_going_with_its_progress`): the lines and default agent; a run going
(the fake machine's new `wait` outcome: an installer that waits for a file `go`) shows in every
plan with its events exactly as sent, numbered from 1; a second run refused, and `ptyro install`
on the same machine refused with "from the web app, through the daemon"; gone after the report.
`two_installs_at_once_the_second_is_refused` (`boot1.rs`): two CLI installs on the fake machine,
the second refused saying `ptyro install`, `ptyro setup` refused before its checklist, nothing of
them installed, the lock free afterwards; `the_daemon_says_and_refuses_while_a_terminal_installs`:
a terminal's install named in `SetupPlan.held_by` and refusing the daemon's `SetupRun` (whose own
flag is let go, so the next starts); the TUI's `the_setup_checklist_says_when_setup_runs_elsewhere`;
`setup::lock`'s own tests (a second holder refused and told who, then free again; a stale
`setup.who` naming a live, unrelated process never blocking and tidied away; a probe racing 200
takes never refusing one; asking in the holding process showing the holder and keeping the lock)
and boot5's `a_stale_note_of_who_holds_setup_blocks_nothing` (a real lock taken and let go, then
a stale note: `doctor`'s look and `ptyro update` both proceed). In a real browser (`make web-e2e`, `web/test/e2e.mjs`): the banner opening the page;
Settings → Agents and tools with the TUI's lines, the admin command and the TUI's way, Install 2's
report and Already here with the default agent; a page opened again while a run waits saying it is
already running, with its lines and no second Install, then its report and Already here; a
terminal's `ptyro install` named on the page with Install held back, then offered again once it
ends; then the server stopped mid-run, the run ending meanwhile, and the page, reconnected, saying it finished
away, with the checklist as it is now.

**Decided by the builder:** the words travel with the plan (not a catalog in the web, nor rules
re-written in TypeScript); the running run rides on `SetupPlan` rather than a request of its own,
since the page always asks for the plan first; the run's events are numbered so a page can join
mid-way without losing or repeating one; the banner hides per device; the button counts only what
the web runs.

### 24.7 Out of date, offered (BOOT5) **[built 2026-11-07]**

The design's goals 6 and 8 (the owner: "tell us how to update on startup when it's detected out
of date. on 'doctor' as well"; "once a day, remembered in between, or with doctor"): an installed
agent with a newer version is reported with a one-step update and the command shown, never
updated silently; newest versions are fetched at most once a day, remembered, and fetched afresh
by `doctor`. What is checked is the agents (Claude Code, Codex, Antigravity, OpenCode, Gemini
CLI): not git, the GitHub CLI or Node.js (the design's non-goal). ptyro itself is one more row (BOOT6, §24.8).

**The engine** (`ptyro_agents::updates`, beside the version table and the plan engine):
- `sources()`: each agent row with a `latest` source (§24.1); `looked_up()` adds ptyro's own
  (BOOT6, §24.8) as one more source and finding under the id `ptyro`.
- `lookup(host)`: every source at once (a scoped thread each), each reply through
  `Host::latest_reply(url)` (the real host: `curl -fsSL --connect-timeout 5 --max-time 10`, as the
  rest of setup downloads; the fake machine: offline), read by `Latest::parse`. A dead network
  costs one wait of a few seconds, not one per agent. Found versions, and per source why it
  couldn't be had.
- `check(host, state, now)`: lookup, then `setup::survey` with the newest versions (an older
  agent is `outdated`; one installed another way is `other` with `latest`), and remember it.
  A failed lookup keeps the version found before, so offline changes nothing but the time.
- `refresh(host, state, now)`: the survey again against what is remembered, no network, by the
  holder of the setup lock after an install or update. Between the daily looks the same is done
  beside the installs (`look_beside`, below), so an agent that updated itself (Claude Code and
  Antigravity do) stops being offered. Refreshes ask the agents only (`setup::survey_agents`: no
  git, GitHub CLI or Node.js versions, no package manager's candidate).
- `remember(host, state, now, since, looked, full)`: what a lookup found, written with the survey
  (all rows when it is to plan with, the agents otherwise); `since` is the install count read
  before the survey (below).
- `setup::other_update(item, platform, via)`: an agent installed another way updates through that
  installer's `others` row with an `update`; the findings and the plan both use it, so a row
  without one is neither offered nor planned.
- `outdated(survey)`: each out-of-date agent as `Outdated{id, name, program, version, latest,
  command, via, admin, doc}`: `command` is what the update runs, as typed (the recommended
  method's `update`, or for one installed another way that installer's own `update` from
  `others`: `npm install -g @openai/codex@latest`). Its `finding()` is the words every place
  shows: "Codex 0.170.0 is out (you have 0.162.1): `ptyro update codex` updates it (it runs
  `curl -fsSL https://chatgpt.com/codex/install.sh | sh`)".

**Remembered: `<state>/versions.json`** (state, not database content: the design's database
rule): `checked` (when the newest versions were last looked up, ms, whether that worked or not:
a failed look waits a day too), `latest` (by item id), `failed` (why each couldn't be had, said
only by `doctor`), `surveyed` and `outdated` (the last survey's findings), `noted` (per agent,
the newest version the start-up note said) and `generation` (below). It is changed only under
`<state>/versions.lock` (the OS's file lock), read whole, changed, written to a temporary file and
moved into place, so the daemon's look, a `ptyro update` and the start-up note never undo each
other. `due(v, now)`: never looked, a day (`DAY_MS`) since `checked`, or the clock went back.

**Who looks** (decided by the builder, as the coordinator asked): the daemon, in the background,
so the TUI's badge, the web's Health page and the start-up note all read one result.
`start_updates` (`crates/ptyro-daemon/src/setup.rs`) waits a minute after the daemon starts
(never on its start-up path), then every hour: with `updates.check = "daily"` and no install
running, it looks if a day has passed, else only refreshes. `ptyro doctor`, `ptyro update` and
`ptyro update --check` look afresh; `ptyro doctor --quick`, the web's Health page and the daemon's
`Updates` request refresh (installed versions asked again, newest versions as remembered).
Start-up never touches the network: the TUI asks its daemon, which answers from what the last
look remembered (asking each agent's version again, beside the installs). Each machine side checks its own: Windows' ptyro and WSL's
keep their own state (§24.6), so a finding belongs to its side's file, its side's daemon and its
side's `ptyro update`.

**A TUI shows its daemon's machine** (the coordinator's ruling, the review's I1 and I2): its
badge's `updates` findings and its start-up note come from the daemon it is attached to
(`Request::Updates`), whose machine runs the sessions, never from this computer's file when the
daemon is elsewhere; its own `doctor --quick`'s per-agent `updates` findings are dropped (its other `updates` warnings,
such as a typo in `updates.ignore`, stay). The fix runs there
too: `ptyro update` in a split the daemon starts (this computer's ptyro; WSL's own in WSL, as
setup's WSL side runs; or the remote computer's). With an ssh daemon each finding and the note say
where ("on box: Codex 0.170.0 is out …"); with WSL's daemon, "on WSL: …". Windows' own agents,
when WSL's daemon is the attached one, are `ptyro doctor`'s (and `ptyro update`'s) on Windows.

**A look beside an install** (the review lessons: no race where a second action starts while one
runs, no stale view after one): every install and update takes the machine's setup lock (§24.6),
and taking it bumps `generation` (`updates::started`). Every write of `versions.json` by someone
not holding the lock goes through `look_beside` (the daemon's hourly look, `doctor`, `ptyro update
--check`, the web's "look now", the `Updates` request): it reads `generation`, then whether the
lock is held (`held_by`'s probe); while held it writes nothing and answers from
memory; and its write keeps the newest versions it found but drops its view of what is installed
if `generation` moved meanwhile. An install or update refreshes while it still holds the lock. So
a look that began before an update never writes the old version back over it (unit-tested:
`a_look_that_began_before_an_update_never_writes_the_old_version_back`).

**Offline is quiet.** A failed lookup never prints at start-up, never blocks it (start-up never
touches the network: the daemon answers from what the last look remembered), and is tried again the next day. Only `doctor` says so, as a note: "couldn't look for
newer versions (offline?): ptyro looks again tomorrow" (or which agents' sources couldn't be
reached). `ptyro update` offline says the same and exits 0.

**Protocol 1.35** (additive, §6.4): `Request::Updates{note}` → `Reply::Updates{offered, note}`,
the daemon machine's `Outdated` findings (less what is skipped; none when off), and with `note`
those the start-up note says now, marked said.

**The four places a finding shows:**
1. **The start-up note**, once per new version: when the TUI starts (its first tick with no
   dialog or first run on screen to hide it), it asks its daemon `Updates{note: true}`, whose
   `updates::note` returns what is offered and not said yet and marks it said in `noted`, under
   the lock (said once, however many TUIs start).
   The TUI shows it as a message with an offer: "Codex 0.170.0 is out (you have 0.162.1) · C-q u
   updates it" (`toast.act` runs `app.update` for it); several: "2 agents have newer versions:
   Codex 0.170.0, Claude Code 2.1.400 · C-q u updates them". A newer version still is said again,
   once. With a daemon on another computer it says which ("on box: …").
2. **The TUI's health badge** (`M`): the daemon's `Updates` (asked with each doctor run),
   `Fix::Update(names)`:
   ⏎ runs `ptyro update <names>` in a split, tracked as the checklist's installs are (§24.5: one
   at a time, never beside a running install, and the badge looks again when the split ends).
3. **The web's Health page** (`#/health`): the same findings (each check carries the engine's
   `Outdated` as `data`, which the page reads instead of the words); a finding's menu offers
   "Update <name> to <version>" (`updates.update`), "Look for newer versions now" (`updates.check`) and
   Update settings (`#/settings/updates`).
4. **`ptyro doctor`**: section `updates`, looked up afresh: per agent out of date a warning with
   the words above, one skipped by `updates.ignore` a note, "up to date (looked 5 minutes ago)"
   when nothing is, "off (updates.check)" when off, and a warning for an `updates.ignore` entry
   that names no agent (a typo skips nothing).

**The update itself, `ptyro update [NAME…] [--check] [-y] [--json]`:** names as `ptyro install`
takes them, agents only (a tool is refused, exit 2: "ptyro updates the agents; git updates the way
you installed it"; an unknown name exits 3). Another run holding the machine's setup lock is said at once ("setup is already running (from …)",
exit 5). It looks afresh first (offline: what is remembered), without the lock (a slow network
never holds it), then takes the lock, writes what it found with a full survey, and plans each agent to update (those named, else every one out of date that
`updates.ignore` doesn't skip; named ones are updated even when skipped) with the plan engine:
an `outdated` entry runs its method's `update`, an `other` one with `latest` that installer's own
(`setup::plan`; `setup::run` finds the method among `others` for an update). It shows each "Codex
0.162.1 → 0.170.0: `…`", with the sessions running it, and asks `Update? [Y/n]` (`--yes` asks
nothing and runs only what needs no password; without a terminal and without `--yes` it refuses).
The steps run with live output, ✓ or ✗ with the fix ("`ptyro update codex` tries again; or run
it yourself to see why: …"); then it refreshes `versions.json` (still under the lock), says "N
updated", and what else is out of date. A named agent with nothing to do says so: "✓ Claude Code
2.1.400 is the newest", "isn't installed: `ptyro install …`", or that its newest version couldn't
be looked up. `--check` only looks (afresh, even with `updates.check = "off"`: the person asked)
and says where each installed agent stands; `--json` prints `{outdated, ignored, failed,
checked}`. `--remote` is refused, as for setup (the agents are this computer's; a `wsl://` one
says to run `wsl ptyro update`). The hidden `--tty` is setup's (the TUI's split through interop).

**An agent in use** (the design's call): the update goes ahead, and ptyro names the sessions that
still run the old version until they restart: before the question ("2 sessions still run Codex
0.162.1 until they restart: brisk-rook, calm-fox") and after the update. Sessions are this
computer's daemon's live ones whose agent is that item (or whose program is its command); with no
daemon running there are none. On the web the question names them from the page's session list.

**Windows won't replace a running program.** An update failing with Windows', npm's or an
installer's words for it ("being used by another process", `EBUSY`, "resource busy or locked",
"text file busy", os error 32, "sharing violation": `setup::in_use`) gets the fix "Codex is
running, and Windows won't replace a program while it runs: end the sessions running it (or quit
it wherever it runs), then `ptyro update codex` again" (on every client: the web's log shows it).
`ptyro update` at a terminal on Windows then names the sessions running it and asks "Try again
now? [Y/n]", and tries those again on a yes (as often as asked); one that updated itself meanwhile
counts as done ("✓ Claude Code is up to date now"); `--yes` and `--json` don't ask. The event
carries `in_use`, and the web's setup page shows "<name> is still running (sessions…)" with "Try
updating <name> again" (`updates.retry`), which runs it again. Elsewhere (Linux's and macOS's
"text file busy") the words don't blame Windows: "its file couldn't be replaced while it runs".

**The TUI** (`app.update`, the palette; the badge's fix; the note's offer): `ptyro update
[names]` in a split where the daemon is, as above, one at a time with setup's splits (`setup_run`,
`setup_tick`; a running one shows instead, saying "an update is still running in …" or "setup is
still running in …"); with this computer's daemon, the machine's lock held elsewhere is said
before any split opens. The setup checklist (TUI, CLI and web) plans
with the remembered newest versions (`updates::known`: less what is skipped, none when off), so
an outdated agent is a row there too ("2.1.300; 2.1.400 is out"), and ticking it updates it.

**The web:** Health's "Update <name> to <version>" asks first (the update's command; if it needs
administrator rights, that the next page gives the command to run on the computer; the sessions
that keep the old version), then runs the daemon's `SetupRun` of that agent (an outdated agent's
action is its update) and opens the setup page, which follows the run (§24.6) to its report, "1
updated." (`SetupLog.summary` tells updates from installs by the plan's `action.update`). The
setup page's Already here gives an agent installed another way and out of date its own "Update to
<version>" button. "Look for newer versions now" is the host operation `updates.check` (`ptyro
update --check --json` in `ptyro serve`), then Health and the checklist reload. The daemon's run
refreshes `versions.json` before it lets go of the lock. An update needing administrator rights is
the owner's parity exception (§24.6): the page shows the command to run on the computer.

**Settings** (`[updates]`, the panel's new Updates section, both screens; `settings.rs`):
`updates.check = "daily" | "off"` (default `daily`; the design's form) and `updates.ignore` (a
list: `codex` skips every Codex update, `codex@0.170.0` only that version; names as people say
them, any case). The config checks each entry's shape (`name` or `name@version`, no spaces).
With `off`, nothing is fetched (the daemon, `doctor`) and nothing offered (the note, the badge, the
checklists); only `ptyro update` and `--check`, asked for, still look.

**Tests, never the network:** the knob `PTYRO_TEST_LATEST=<dir>` (debug builds,
`ptyro_core::test_knob`) makes `lookup` read `<dir>/<id>` (each source's reply, in BOOT0's
fixtures' shapes; a missing one is offline) and log each to `<dir>/asked.log`; the testkit sets
it to an empty folder for every daemon and command it starts, so no `doctor` in a test reaches
the network. `PTYRO_TEST_NOW` sets the clock (ms, or `@file` so a test moves it under a running
daemon); `PTYRO_TEST_UPDATES_FIRST_MS` and `PTYRO_TEST_UPDATES_EVERY_MS` the daemon's waits. The
fake machine's new `busy` outcome fails once as Windows does. The engine in `setup/tests.rs`
(found, remembered, due, offline keeping the last versions; one installed through npm updating
through npm; `updates.ignore` by name and version, `off`; the note once per version; refresh asking
no source; Windows' refusal's fix); the CLI and daemon in `crates/ptyro/tests/boot5.rs` (doctor
afresh vs. `--quick` remembered, `--check --json`; offline quiet and `off` fetching nothing; the
question, `n`, the ignore list, named ones; the lock held refusing it; npm's update; Windows' retry
and its refusal; the daemon looking once a day by the moved clock, refreshing hourly, and the web's
update as a `SetupRun` with the daemon's hourly look running beside it never writing the old view
back); the TUI's note once and its fix's split one at a time (`app.rs`); the web's finding parsing,
running sessions and "1 updated." (`web/test/setup.mjs`); and in a real browser (`make web-e2e`):
Health → Look for newer versions now → Update → the setup page's "1 updated." → nothing offered.

**Decided by the builder:** the daemon looks (the badge, the note and the web share one result) and
the CLI note is the TUI's start; a TUI shows its daemon's machine (the coordinator's ruling); refreshes between looks and after every install keep findings true without
the network; `generation` instead of holding the setup lock while looking (a daily look must never
make `ptyro install` wait or be refused); `ptyro update --check` and `ptyro update` look even when
off (asked for); an agent installed another way updates through its own installer, and BOOT7
offers the move beside it (§24.9); protocol 1.35 (above).

### 24.8 ptyro updates itself (BOOT6) **[built 2026-11-07]**

The design's goal 7 (the owner: "download, and offer to install and upgrade"): install ptyro from
its binary once, and from then on ptyro keeps itself current the way it keeps the agents current.
It is **one more row of §24.7's machinery**, under the id `ptyro`, not a system beside it: the same
daily look and `versions.json`, the same `Outdated` finding in the same four places (the start-up
note, the health badge, the web's Health page, `doctor`), the same `ptyro update`, the same
machine setup lock (§24.6) and `updates.check` / `updates.ignore` (`ptyro`, `ptyro@0.0.70`).

**The engine** (`ptyro_agents::own`):
- **The newest version:** GitHub's latest release of `nealhardesty/ptyro`
  (`https://api.github.com/repos/nealhardesty/ptyro/releases/latest`, `tag_name` = `v<version>`,
  what `install.sh` reads), as a `github-release` source (`Latest::new`). `updates::looked_up()`
  adds it to the agents' sources, so it is fetched in the same concurrent look, with the same
  timeouts, and offline is just as quiet. A test's `PTYRO_TEST_LATEST/ptyro` holds the reply.
- **What is installed:** `Install::here()`: this ptyro's binary (`current_exe`, on Linux with a
  replaced file's ` (deleted)` dropped, on Windows an image an update renamed aside,
  `ptyro.exe.old[.<pid>]`, named as `ptyro.exe`, so a daemon whose upgrade didn't happen never
  writes under the aside name; a debug build's `PTYRO_TEST_SELF_EXE` names another), **resolved
  to the file itself** (macOS's `current_exe` is the path that was run: on an Intel Mac Homebrew's
  `/usr/local/bin/ptyro`, a link into its keg in a folder the user owns), and how it updates, by
  where that file is (`classify`, as `ptyro uninstall` tells; a link in Homebrew's `bin` counts as
  Homebrew's). An update replaces the file, never a link to it, so a link someone made keeps
  pointing at the new binary:
  - `Release`: a folder the user can write (`~/.local/bin`, `%LOCALAPPDATA%\Programs\ptyro\bin`,
    or anywhere copied by hand): downloaded, checked and replaced here.
  - `Admin`: the same in a folder only an administrator can write (`/usr/local/bin` owned by
    root): replaced with sudo, at a terminal only.
  - `Brew`: `/Cellar/`, `/homebrew/`, `/linuxbrew/`: `brew upgrade ptyro`, never overwritten.
  - `Managed`: cargo (`~/.cargo`), a system package (`/usr/bin`, `/bin`, `/snap`), WinGet, Scoop:
    said, with how it updates ("it was installed with cargo, so update it that way"), never
    replaced; `ptyro update` (all) passes over it, `ptyro update ptyro` says so (exit 1).
  - `Build`: a cargo `target/{debug,release}` (or `target/<triple>/…`): never offered, never
    looked up (so a developer's build and every test of another feature never reach the network
    for it).
  Its version is what the binary answers to `--version` (the file on disk, not the process: a
  daemon still running the old image after an install by hand isn't "outdated", it is `daemon
  upgrade`'s finding).
- **The finding:** `own::finding(install, latest)`: an `Outdated` with `id`/`program` `ptyro`,
  plus two optional fields (`path`: the binary it replaces; `managed`: who owns it instead; wire
  compatible, so no protocol bump). Its words: "ptyro 0.0.64 is out (you have 0.0.63): `ptyro
  update ptyro` updates it (it downloads the release, checks it against SHA256SUMS, replaces
  /home/me/.local/bin/ptyro and upgrades the daemon live)"; Homebrew's "(it runs `brew upgrade
  ptyro`, then upgrades the daemon live)"; an administrator's "`ptyro update ptyro` in a terminal
  on the computer updates it (it replaces /usr/local/bin/ptyro as administrator …)", whose
  `command` is the installer line to run there instead (`curl -fsSL
  …/releases/download/v<v>/install.sh | sudo env PTYRO_PREFIX='/usr/local' PTYRO_VERSION=<v> sh`; on
  Windows install.ps1 in an administrator PowerShell). The start-up note with ptyro among others
  says "2 newer versions are out: ptyro 0.0.64, Codex 0.170.0".
- **The update** (`own::update`, only after the person's yes, under the setup lock), reporting
  setup's events under the id `ptyro`, exactly as `install.sh` / `install.ps1` check a release. The
  version must be a plain release version (`1.2.3`, `1.2.3-rc.1`: `own::valid_version`) before it
  goes into a URL or a path; the finding isn't offered otherwise.
  1. `ptyro-<v>-<triple>.tar.gz` (Windows: `.zip`) and `SHA256SUMS` downloaded with curl from
     `https://github.com/nealhardesty/ptyro/releases/download/v<v>` (a debug build's
     `PTYRO_TEST_RELEASE_URL`, as `install.sh`'s `PTYRO_RELEASE_URL`) into a temporary folder of
     its own (`tempfile`: created exclusively, private; untrusted until checked; removed
     afterwards, whatever happens). curl is held to HTTPS (`--proto =https --proto-redir =https
     --tlsv1.2`; plain HTTP only for a debug build's test knob), 10 s to connect, and gives up on
     a download under 1 KB/s for a minute or past 30 minutes, so a stalled one ends with its error
     and lets go of the setup lock. The triple is this build's (`own::triple`).
  2. Its SHA-256 against its `SHA256SUMS` line (either mode). **A mismatch is refused** and
     nothing is replaced: "the download's checksum is …, but the release's SHA256SUMS says …: it
     was changed or broken on the way, so it isn't installed", fix "nothing was replaced; `ptyro
     update ptyro` tries again (if it keeps failing, don't install it by hand either: tell the
     maintainers …)". No line for the file is refused the same way.
  3. With `gh` on `PATH`, the signed build provenance: `gh attestation verify <file> --repo
     nealhardesty/ptyro`. As in the installers: not verifiable (not logged in, an old release) is a
     note, the checksum having matched; `PTYRO_VERIFY=0` skips it; `PTYRO_REQUIRE_ATTESTATION=1`
     refuses without it.
  4. Unpacked with `tar -xf` (Windows: System32's bsdtar, which reads a zip; Git's GNU tar can't),
     `ptyro-<v>-<triple>/ptyro[.exe]` required. Its list (`tar -tf`) is checked first (no
     absolute path, no `..`), and what was unpacked must be plain files and folders: a symbolic
     link, or on Unix a file linked twice (a hard link in the archive), is refused.
  5. **Swapped in atomically, the old kept until the new one runs** (`own::swap`): each new file
     copied beside its place (the same folder, so the move is atomic); the old one kept aside
     (elsewhere hard-linked to `.ptyro.old.<pid>`, so the name is never missing; on Windows
     renamed to `ptyro.exe.old.<pid>`, since a running exe can't be overwritten but can be
     renamed);
     the new one renamed into place; then **`<path> --version` must say `ptyro <v>`**. If it
     doesn't (it won't run, Smart App Control, the wrong version), every file is put back: "the
     new ptyro <v> didn't run (…), so the old one is back in place" (a file that couldn't be put
     back is said instead, with where the old one is). On success the old file is removed (on
     Windows, while the old image still runs — the daemon, a TUI — it can't be: every ptyro start
     sweeps install.ps1's `ptyro.exe.old` and an update's `ptyro.exe.old.<pid>` once that `<pid>`
     no longer runs, so an update still going keeps what its rollback needs; beside the file
     itself, canonical, as the update put them there even when ptyro is reached through a link;
     the same for the
     zip's `ptyrow.exe`, `conpty.dll`, `OpenConsole.exe`, which are swapped with it, as
     install.ps1 puts them). The release's shell completions are copied over `<prefix>/share/ptyro/completions` when
     that folder exists (as install.sh puts them).
  6. An administrator's binary (Unix, at a terminal: `Admin::Ask`): the same, with the place,
     put-back and removal done by `sudo sh -c SCRIPT sh PATHS…` (the paths are arguments, never
     in the script; sudo asks for the password in the terminal, never through ptyro). A put-back
     sudo refuses is said with where the old binary is and the `sudo mv` that restores it. With `--yes`, from the web, or on Windows, it isn't attempted: the
     command above is the fix.
  Homebrew's: `brew upgrade ptyro` live, then the version Homebrew's `bin/ptyro` link answers (the
  keg's folder changes with each version, so the daemon is upgraded onto the link). One still
  older than the release (the formula behind it, or Homebrew's auto-update off) isn't an update:
  "Homebrew's formula isn't at <v> yet", fix `brew update && brew upgrade ptyro`, and no daemon
  upgrade or other half follows.

**`ptyro update`** (§24.7's, extended): `ptyro update ptyro` names it (`ptyro update` covers it
with the agents, ptyro last; `--check` and `--json` include it). Whether it will be updated is
known from the lookup's newest release, else the remembered one (as `updates::remember` builds the
update list: GitHub's API may be out of requests while the download still works, and the halves
are decided on the same view), so on Windows with WSL the other half is looked at then, before the
setup lock is taken (a cold distro takes seconds; the lock is never held across a probe). The question shows it as "ptyro
0.0.63 → 0.0.64: download ptyro 0.0.64 (ptyro-0.0.64-x86_64-unknown-linux-gnu.tar.gz), check it
against SHA256SUMS, and replace /home/me/.local/bin/ptyro / then the daemon is upgraded live: every
session carries on". After the swap, still under the lock, `versions.json` is refreshed (the
finding goes); then, the lock released:
- **The daemon, live:** when this computer's daemon answers, `daemon upgrade --binary <the new
  binary>` (§4.5): every session carries on, in `daemon upgrade`'s own words ("upgraded
  in 41 ms: daemon … N session(s)"). No daemon: "the next one starts as ptyro 0.0.64". A failed
  upgrade leaves the new binary installed and says the fix (`ptyro daemon upgrade`).
- **On Windows with WSL, both halves** (the design's call): the two ptyros are kept at one version,
  so Windows never passes flags to an older WSL ptyro that rejects them. Each half updates
  itself; the half that updated asks the other, when it says another version, to run `ptyro update
  ptyro --yes`:
  - Windows → WSL: the distro of `client.default_remote` (else WSL's others in WSL's order) whose
    ptyro answers `--version` through `wsl.exe` (`ptyro_args`, as setup's WSL side): it updates its
    binary and live-upgrades WSL's daemon. No WSL, no distro, or no ptyro in any (the distro's
    shell can't find it: exit 127) is a normal state with no other half, and says nothing. A ptyro
    that is there but doesn't answer (another exit, or no answer in 20 s) is said on stderr before
    the question: "note: WSL's ptyro (Ubuntu) couldn't be checked (…), so it isn't updated with
    this one: `wsl -d Ubuntu -- ptyro update ptyro` once it answers", so one half is never updated
    without a word about the other.
  - WSL → Windows: Windows' `ptyro.exe` on `PATH` (WSL appends Windows' own), else where
    install.ps1 puts it (`%USERPROFILE%\AppData\Local\Programs\ptyro\bin`), through interop.
  It stops by itself: the other half, updated, sees this one at its version and doesn't ask back.
  The question names it ("and WSL's ptyro (Ubuntu) 0.0.62 → 0.0.64 too, so both halves agree"),
  the result is "✓ WSL's ptyro (Ubuntu) 0.0.64", and `ptyro update ptyro` when this half is the
  newest still brings the other along ("✓ … is 0.0.64 too"). A half too old to update itself (from
  before BOOT6, which doesn't know `ptyro update ptyro`) is "✗ … wasn't updated" with the fix: its
  installer (`wsl -d <distro> -- sh -c "curl … install.sh | sh"`, then `ptyro daemon upgrade`
  there; or install.ps1 in PowerShell). So the TUI on Windows attached to WSL's daemon, whose fix
  runs `ptyro update ptyro` in WSL, updates Windows' `ptyro.exe` too.

**The four places**, unchanged in shape: the TUI's start-up note (`C-q u`) and badge (⏎: `ptyro
update ptyro` in a split where the daemon is, tracked as §24.7's), and `app.update` in the palette;
`doctor`'s `updates` section; the web's Health page, whose finding for ptyro offers "Update ptyro
to <v>" (`updates.self`): asked first (what it does, that the daemon upgrades live and the page
reconnects, that on Windows with WSL the other half comes along), then the host operation
`updates.self` starts `ptyro update ptyro --yes` in a session on the daemon (refused at once while
setup runs; the setup lock is one-at-a-time) and the page opens it: its progress live, and the
daemon's live upgrade at its end keeps that session. Installing from the web replaces the daemon's
own binary on the computer, which isn't an administrator's step for a user-owned install. For one
only an administrator can replace (the owner's parity exception, §24.6), the page shows the command
to run on the computer, with Copy, and `ptyro update ptyro` / `C-q u` there; the host operation
refuses it too. A package manager's says how it updates.

**Decided by the builder:**
- **Nothing is downloaded before the yes.** The daily look fetches the release's metadata only (a
  small JSON, as for the agents); the binary (tens of MB) is fetched by the update the person said
  yes to. Downloading ahead would make the yes quicker by seconds but would spend the network
  unasked on every machine, every release, against the anti-goal (no network install or update
  without the user's yes).
- `ptyro update ptyro` rather than a separate `self-update` command: ptyro is a row like the
  agents, and `ptyro update` (everything) covers it.
- The version is the file's (`--version`), not the process's; a build in `target/` is never a row.
- The halves are kept in step by each updating itself, asked by the other only when the versions
  differ (no new hidden flag an older half would reject).
- After a swap the daemon is upgraded onto the binary just installed (`--binary`), so a daemon
  started from elsewhere (a build) still ends up on it.

**Tests, never the network** (`crates/ptyro/tests/boot6.rs`): a fake release server on 127.0.0.1
(a thread serving a folder, each request logged) with the archive (`tar`; on Windows System32's,
`-a` for the zip) and `SHA256SUMS`; the registry's `ptyro`; the installed binary is the test's own
(`PTYRO_TEST_SELF_EXE`), never the build. Offered in `--check --json` and `doctor` (with `data`),
the question answered no fetching nothing, `updates.ignore = ["ptyro"]` skipping it; **a tampered
download refused** (the sums say another file's: exit 1, nothing replaced, nothing left beside it);
**a new binary that doesn't run rolled back** (the old one answers again, still offered); a build
never looked up; a yes installing it and **the daemon upgraded live with a session kept** (its
generation moves, the session's process alive, the finding gone); the web's `updates.self` running
it in a session whose daemon is upgraded under it; on "Windows" (the fake machine's platform)
WSL's half brought along through `ptyro fake-wsl` (whose distro now has its own installed binary,
`<distro>/bin/ptyro`, which answers its `--version` and is what its update replaces); in "WSL"
Windows' half (`PTYRO_TEST_WINDOWS_PTYRO`, a stand-in) brought along, and one too old to know how
given its fix; Homebrew's through a stand-in `brew` with its keg never overwritten; cargo's said and
never replaced. The success paths use shell-script stand-ins (Unix); the refusals and the rollback
run on Windows too, with this build hard-linked as the installed one. Unit tests: `classify`,
`checksum_for`, the administrator's command, `swap` keeping and restoring (with an injected check,
on every platform), `sweep` removing only leftovers (`own.rs`); the TUI's finding and note
(`app.rs`); the web's `selfUpdate` (`web/test/setup.mjs`).

### 24.9 Moves to the recommended installer (BOOT7) **[built 2026-11-07]**

The design's goal 9 (the owner: "offer to move it to the recommended installer"): an agent
installed some way other than the version table's recommended one (Codex through npm, Claude Code
through npm, Homebrew or WinGet) is offered the move; the offer can be dismissed per agent, after
which it updates the way it is installed. In all three clients.

**Detected** as before (§24.2's `other` state, by where the program's file really is: Homebrew's
`Cellar`/`Caskroom` first, since Homebrew's Gemini CLI is an npm install inside its keg; then
`npm`'s `node_modules`; then `homebrew`, WinGet's links), now through the canonical path, so a
link in `~/.npm-global/bin` or `/usr/local/bin` counts as npm's. `Installed.system`: the file's
folder is neither the user's nor writable by everyone (a system npm prefix,
`/usr/lib/node_modules`), so removing it needs administrator rights. `Installed.prefix`: npm's
global prefix the copy is in (the folder above `lib/node_modules`; on Windows above
`node_modules`, chosen by the path's own shape: a drive letter or share is Windows'), `None` for another tool's store (`Installed.store`: Bun's
global folder, mise's `npm:` backend, pnpm's, an asdf plugin's), which npm doesn't manage: no move,
and `doctor` names that tool's command (`mise uninstall npm:@openai/codex`, `bun remove -g …`). A
Node.js that mise or asdf installed is a real npm prefix and moves as any. A prefix ptyro won't
quote (a `'` in it) offers no move, and `doctor` says that, with `npm uninstall -g` to run there. `Installed.file` and `Installed.real`: the bare file found and where it really is (links
followed).

**The vendor's copy is known by its path.** A vendor installer's method names its `places`, where
its copy really is: Claude Code `~/.local/share/claude` (the launcher `~/.local/bin/claude` links
into its `versions/`) [V 2026-11-07, the setup page], on Windows `~/.local/bin/claude.exe` and
`~/.local/share/claude` [V, its Windows uninstall]; Codex `~/.codex/packages/standalone` (install.sh
links `~/.local/bin/codex` into `…/current`; `$CODEX_HOME` first when set, as the installers
follow it; on Windows install.ps1's junctions, also `%CODEX_INSTALL_DIR%` or
`%LOCALAPPDATA%/Programs/OpenAI/Codex`) [S 2026-11-07, the vendor's install.sh and install.ps1];
OpenCode `~/.opencode` [V, its docs]. A place naming an unset variable is skipped; `~` is the home
the platform's installers use (`USERPROFILE` on Windows, whatever Git Bash's `HOME` says).
`setup::installed_by(copy, method)`: a package manager's copy
by its folder (`via`); a vendor's only when its file (or where it really is) is under one of those
places. A copy from an installer nothing classifies (Nix's store, Scoop's shims, mise or asdf
shims, pipx, a wrapper script in `~/bin`) never counts. A vendor method with no `places` (not
known) offers no move, and the table's tests require them wherever another way exists.

**The offer** (`setup::offer`, `setup::Moving`): for an `other` agent that `Survey.keep` doesn't
name, when the platform's recommended method is usable here (its `needs` present; Gemini CLI's npm
needs Node.js) and the old installer's `others` row has an `uninstall`. Each `others` row in the
table now carries `uninstall` (the vendor's documented one for Claude Code [V 2026-11-07, the setup
page's "Uninstall Claude Code"]; npm's and Homebrew's own forms for Codex, OpenCode and Gemini CLI
[S], marked in the row's note), never with `sudo` (the tests insist): ptyro adds `sudo ` for a
`system` copy on Unix, and the whole move is then an administrator's step. Never for Homebrew,
which refuses to run as root: a keg that isn't the user's isn't offered the move. npm's removal acts
on the old copy's own prefix, `npm uninstall -g --prefix '<prefix>' <package>` (npm's documented
`prefix` setting [S]), never npm's prefix of the moment (another nvm Node.js's, ptyro's own); a copy
with no npm prefix (Bun's) isn't offered the move. `Entry.moves` carries
the offer whether ticked or not; ticked, `Action{via: recommended, command: install + "\n" +
uninstall, admin: either, moving: true}`. Each agent row gains `keeps`, where its settings and
logins live, said in every offer.

**Nothing is lost** (the ruling's checks, recorded in RESEARCH G18):
- **Settings and logins stay.** They live in the home, which neither installer touches: Claude
  Code `~/.claude` and `~/.claude.json` [V 2026-11-07: the docs remove them in a step of their own,
  apart from every uninstall]; Codex `~/.codex` (`config.toml` [V 2026-11-07, config-basic];
  `auth.json` there [S: the binary, §10.10]); Gemini CLI `~/.gemini` [S]; OpenCode
  `~/.config/opencode` and `~/.local/share/opencode` [S]. Antigravity has no other way in the table,
  so it is never offered a move. The tests confirm the login holds across a move (the fake
  machine's `logged_in`: setup's login walk after the move says ✓).
- **The new one first** (`setup/moving.rs`): the recommended method's install runs; then
  `Host::copies` (every copy of the program, `PATH` first, then npm's, nvm's and the vendors'
  folders: `locate::find_all`) must show a copy the recommended method installed
  (`installed_by`: in the vendor's places; npm's for Gemini CLI) that answers `--version`. Any
  other copy (Homebrew's beside npm's, Nix's) never counts; then the install runs.
  A failed install, or one whose new copy can't be found answering, is `failed` with "nothing was
  removed: Codex through npm is still installed and works" and the fix; the old copy is untouched.
- **Then the old one goes**, with its installer's `uninstall` (`Host::uninstall`, the row's shell,
  `sudo` as above). A failure is `failed` with what works and where ("Codex 0.170.0 through the
  vendor's installer works (~/.local/bin/codex), but removing npm's copy … failed") and the fix
  "finish the move by removing it yourself: `npm uninstall -g --prefix … @openai/codex`". Tried
  again, a new copy already here is said and only the removal runs.
- **The old one must be gone.** Its copy still there after a removal that said it worked (npm
  acting elsewhere: "removed 0 packages") is `failed`, "npm's copy (…) is still there after `…`",
  with the fix to delete that file, ready to paste (`rm '<file>'`; on Windows `Remove-Item
  -LiteralPath`; an npm copy on Windows by its prefix's shims, `codex`, `codex.cmd`, `codex.ps1`,
  and its package folder, never the script inside it, which would leave the shim on `PATH`), never
  "run it again", which would do the same. Every message after the
  removal looks for the new copy again before saying it works (or says it no longer runs, with
  the install command).
- **The new one must still run.** A removal can take the new install's link with it (an npm prefix
  of `~/.local` shares `~/.local/bin` with the native installer): the recommended copy is looked
  for again, and if it is gone it is installed again at once ("the removal took the new copy too,
  installing it again"). If that fails, the move is `failed`: "npm's copy is removed, but the
  removal took the new codex with it … no Codex runs now", fix: `ptyro install codex` or the
  install command. A move never ends ✓ with nothing to run.
- **The new one must be what runs.** Afterwards `Host::installed` (what `locate::find` picks: `PATH`
  first) must be the recommended copy and answer `--version`. If another comes first (a second npm
  prefix, a third installer), the move is `failed` (the vendor's own copy not answering is
  `failed` with the install command to repair it, never a delete): "npm's codex (…) comes first on PATH, so it is the one that
  runs", fix: delete it (or uninstall it with the tool that put it there), or put the new one's
  folder ahead of it on `PATH`.
- Each half is a `Started` event of its own (`Codex: installing it through the vendor's
  installer`, `Codex: removing npm's copy`) with its command, so the CLI, the TUI's split and the
  web's log show both. A move holds the machine's setup lock like every install (§24.6), and what is
  remembered (`versions.json`) is refreshed afterwards even when it failed half way (it changed
  what is installed).

**Dismissed for good: `updates.keep_installer`** (a list of agent names, `["codex"]`; `[updates]`,
in both settings screens with its schema entry; the config checks each is a bare name). Kept
agents are `Survey.keep` (filled by `updates::survey` from each side's own config, so WSL's comes
from WSL's ptyro): no `moves`, the row is "Already here", and ticking an out-of-date one runs its
own installer's update (BOOT5). `ptyro update` never moves (it plans with every agent kept); the
move is `ptyro install`'s.

**Where it shows:**
- **The checklists** (CLI, TUI, web; `checklist::here_already` and `about`): an offered agent is a
  row, not Already here: "OpenAI's coding agent · installed with npm · move to the recommended
  installer?"; ticked, "… · moves to the vendor's installer, then removes npm's copy · your
  settings and logins (~/.codex) stay". The CLI's checklist note: "installed 0.162.1 through npm;
  move to the recommended installer?". A two-sided cell: `npm: move?`, ticked `move to the vendor's
  installer`.
- **`ptyro install codex`** (the CLI's form: ticking it by name is the move) shows "Codex: move
  from npm to the vendor's installer (path): install it through the vendor's installer, check it
  runs, then `npm uninstall -g @openai/codex`; your settings and logins (~/.codex) stay" and asks
  "Install? [Y/n, or k to keep Codex with npm and not be asked again]". `k` adds it to this
  computer's `updates.keep_installer` and runs nothing for it this time. **A move is offered, never
  done unasked** (the design): `--yes` alone leaves an agent installed another way as it is (its
  own update when out of date, else nothing) and says "Codex is installed with npm: moving it to the
  recommended installer needs a yes (`ptyro install codex` in a terminal, or --move); left as it
  is"; `--move` (with `--yes`) is the unattended opt-in, on `ptyro setup` as on `ptyro install`
  (`setup --yes` without it says the same for each ticked one it passes over). The TUI's
  `--confirmed` moves (its row was ticked). A move on WSL's side (`--where wsl`) runs in WSL's
  ptyro under the Windows question's yes, which passes the moves it covers (hidden `--moves`), so
  WSL's ptyro asks nothing; keeping one of WSL's as installed is done in WSL (its step line says so).
- **The TUI checklist**: `Space` ticks the row (⏎ runs `ptyro install codex --confirmed` in a
  split as for any install); `K` keeps it (this computer's side; on WSL's it says how to keep it
  there), written with the settings panel's own writer; the footer adds "K keep as installed" when
  any row is offered the move.
- **The web checklist**: the row's checkbox (`setup.move`), and "Keep npm" beside it (`setup.keep`,
  the host operation `updates.keep`, which writes the computer's `updates.keep_installer` with the
  CLI's own writer; the daemon reads the config live). An administrator's move is left for the
  computer like any administrator step (§24.6: `ptyro install codex` to run there).
- **The update findings** (§24.7's four places): `updates::Versions.movable` (each offered move,
  kept ones included) is remembered with the survey; `Outdated.move_to` names the recommended
  installer for one out of date, cleared by `updates::offered` (and `doctor`) when kept. Its words
  add "; or `ptyro install codex` moves it from npm to the vendor's installer, the recommended way
  (to keep npm, add codex to updates.keep_installer)". **`doctor`**: that warning, with `data.move`
  (the `Movable`); an agent up to date but installed another way is a **note**, never a warning (a
  normal state): "Codex is installed with npm: `ptyro install codex` moves it to the vendor's
  installer …"; an `updates.keep_installer` entry naming no agent is a warning. **The badge**: beside
  the update finding, `updates codex move` ("Codex is installed with npm: moving it to the vendor's
  installer, the recommended way, also brings it up to date"), whose ⏎ opens the setup checklist
  with it ticked (`Fix::Setup`). **The start-up note**: "Codex 0.170.0 is out (you have 0.162.1)
  (installed with npm; the move to the vendor's installer is offered too)". **The web's Health
  page**: a finding with `data.move` offers "Move Codex to the vendor's installer" (asked first, with
  what it installs, removes and keeps; then the daemon's `SetupRun` of it on the setup page, whose
  report says "1 moved to the recommended installer."; an administrator's shows `ptyro install codex`
  to run on the computer) and "Keep Codex with npm"; its Update names no `moves`, so it updates
  through npm and never moves. The checklist's Install sends `moves` for the rows ticked where the
  move is offered.

**Tests, never the network or a real installer:** the fake machine gains two copies for an `other`
item (`new/<id>`, written when a step installs it the recommended way; `removed/<id>`, written by
`ptyro fake-installer ID VIA uninstall ADMIN`), the old first on `PATH` until removed, and
`world.json`'s `system` (copies in an administrator's folder) and `uninstalls` (`ok`, `fail`,
`leftover`: exits 0 but the copy stays; `both`: takes the new copy too), `second` (another copy of
the old installer, ahead of the new one), `third` (a third installer's copy), `no_prefix` (npm's
copy in no prefix) and outcome `once` (installs once, then fails); each removal's command goes to
`uninstall.log`. `crates/ptyro/tests/boot7.rs`: a successful move (the question's words, both
commands in order, the removal against the old copy's prefix, the login still there, nothing
offered afterwards); a new install that fails leaving the old one (and still offered); a removal
that fails, its fix, and the retry running only the removal; a removal that leaves the old copy,
said with the exact fix and no loop; another old copy first on `PATH`; a removal that takes the new
copy, put back at once, or (put back failing) said plainly with no ✓; a third installer's copy not
counted; `--yes` alone and `setup --yes` moving nothing; no move offered without a prefix; `k`
written to the config, nothing run, never offered again, and the kept agent updating with npm; an
up-to-date one a note in `doctor` and `ptyro update` never moving; the daemon's plan offering it,
the Health page's update (no `moves`) running npm's update, and the checklist's tick moving it while
an administrator's move is left for the computer. Unit tests: the engine's offer, plan, run, a
failed install, a failed removal and its retry, kept, `system`'s sudo and `Admin::Never`, the
findings with and without keep (`setup/tests.rs`); the catalog's `uninstall` and `keeps`
(`catalog.rs`); the TUI's `K` and its tick (`setup_list.rs`) and the badge's move finding
(`app.rs`); the web's `isHere`, `moveOffer`, `moveWords` and the moved summary
(`web/test/setup.mjs`); in a real browser (`make web-e2e`'s `move`): Claude Code and Codex through
npm offered, Keep npm moving Claude Code to Already here (the host operation also asks the daemon to
`ReloadConfig`, so the plan asked for next already keeps it), and Codex ticked, "1 moved to the
recommended installer." with both installers' lines.

**Decided by the builder:** `ptyro install NAME` at a terminal offers the move in its question
(`--move` with `--yes` unattended), and `k` there is the CLI's dismissal; `updates.keep_installer` as the setting's name, beside `updates.ignore`; the new copy is
installed even when the old one shadows it and found through every copy (`locate::find_all`), so
the old is removed only after the new one answered; a move counts as failed when the old copy still
runs first (the goal is the new one running); `system` by the file's folder not being writable;
`ptyro update` never moves; protocol 1.36's `SetupRun.moves` (absent: nothing moves).
`PTYRO_NO_AUTOSPAWN=1` wins in the CLI's shared `Ctx::connect`, so no command (`doctor`'s check
included) starts a daemon when it is set and no test leaves one running; the tests of autospawn
itself (`e2e.rs`'s `cli_autospawns_and_maps_errors_to_exit_codes` and
`only_one_daemon_per_home_and_stale_sockets_are_recovered`, `hardening.rs`) clear it. `doctor` notes an agent installed another way that isn't offered the
move, and why (`updates::Versions.unmovable`: the vendor's place unknown, no npm prefix,
someone else's Homebrew).

### 24.10 The stale suggestions (BOOT9) **[built 2026-11-07]**

The design's last call: the "no agent found" and "isn't installed" messages, which put Gemini CLI
first with npm commands to copy, become offers of setup that name Antigravity for Google. Every
such message now offers the fix, in one set of words shared by the CLI, the TUI and the web.

**The words** (`ptyro_agents::locate`, so no client writes its own):
- `no_agent()`: "No coding agent is installed: `ptyro setup` installs one (Claude Code, Codex, or
  Antigravity for a Google account; Gemini CLI is for Gemini API keys and enterprise accounts)"
  (`WHICH_AGENT`; Gemini CLI kept, labelled as the checklist labels it, never first).
- `no_chat_agent()`: "No agent that can chat is installed: `ptyro setup` installs one (Claude
  Code, Codex or OpenCode; Gemini CLI with a Gemini API key; Antigravity, for a Google account,
  runs in sessions but can't chat)". Antigravity has no structured mode (§22.5), so a chat never
  offers it as the fix.
- `install_fix(program)`: "`ptyro install codex` installs it", for a program the version table
  knows and can install on this platform (`None` otherwise: an agent of your own keeps "install
  it, or set [agents.NAME] command to its full path").
- `not_installed(agent, program)`: "codex isn't installed (codex is nowhere ptyro looks): `ptyro
  install codex` installs it".
- `setup_offer(text)`: what a message's offer installs, read back by the clients that act on it:
  "`ptyro setup` installs" → the checklist's defaults, "`ptyro install codex agy` installs" →
  those names. Only that form: a lock held by "`ptyro install`" or a move ("`ptyro install codex`
  moves it") is something else. The web's `setupOffer` (`web/src/setupkit.ts`) is the same rule.

**Where they are said:**
- **`doctor`** (`agents` and `git`): no agent at all is `no_agent()` (Antigravity now counts as an
  agent: it runs sessions, and with only it installed "no agent" would be wrong); a configured
  agent not found, "claude not found: `ptyro install claude` installs it"; git, "git not found:
  projects and worktrees need it; `ptyro install git` installs it"; gh, "gh not found: pull
  requests (`wt new --pr`) need it; `ptyro install gh` installs it"; Gemini CLI without an API
  key, "… with a personal Google account, use Antigravity (`agy`): `ptyro install agy` installs
  it" when agy isn't installed.
- **The daemon:** a chat with no agent that can chat (`no_chat_agent()`) or with a named one not
  installed (`not_installed`, also a compare's note for that agent); a session whose agent's
  program isn't anywhere ptyro looks (`not_installed`, instead of "cannot start session: No such
  file or directory"; a missing working folder still gives the OS's words); `gh` missing for PR
  features and git missing for a project, each with `install_fix`.
- **The CLI:** the messages as they are: a command to run, which is the CLI's way. `ptyro update`
  with no agent says `no_agent()`. `ptyro init` now notices the no-agent line (it never did: the
  line made the report non-empty) and asks to open the checklist with yes as the default.
- **The TUI:** any refused request whose message carries an offer (a session's spawn, a new
  worktree's agent, a chat made or sent) shows it with `toast.act`: "… · C-q u set it up (the
  checklist)", which runs `app.setup` with that one ticked (`app.setup`'s argument: names), or
  "set up agents (the checklist)" for the defaults (`offer_for`, UX1.17's mechanism). The health
  badge's findings already opened the checklist (§24.5) and now read the same words. The chat's
  agent picker says "not installed: ⏎ sets it up" on an agent setup can install, and ⏎ there
  opens the checklist with it ticked (`app.setup <program>`, as the toast's offer; final fix
  wave).
- **The web:** a refused request (`ui.fail`, which `ask`, `withConfirm` and the chat composer
  use) whose message carries an offer opens a sheet with its words and "Set up codex" (or "Set up
  agents and tools"), which opens the checklist with it ticked (`offerSetup`, `openSetup`,
  `setup.offer`); any other is a toast as before. The button names each by the checklist's
  name ("Set up Antigravity", never its program `agy`: `setupLabel`, through the daemon's agent
  list and the plan's entries). Only a device paired with full scope is offered setup (the sheet,
  Health's item, the new-task link and the chat picker's); others get the message as a toast, and
  the new-task sheet says to run `ptyro setup` on the computer. Health's findings in `agents` and `git` with an
  offer get the same item in their menu. The new-task sheet links "Install another agent", or,
  with none installed, says "No coding agent is installed on the computer yet: Set up agents and
  tools (Claude Code, Codex, or Antigravity for a Google account)"; the chat's agent picker ends
  with "Install another agent…".

**Tests:** `locate.rs` (the words, Gemini CLI never first, Antigravity named, `setup_offer` and
what it refuses); `doctor.rs` (`fix_for`); the TUI's `a_missing_agent_offers_the_setup_checklist`
(a refused spawn and a chat not made, with the offer and its `app.setup` argument) and
`health.rs` (the doctor's new lines opening the checklist); `crates/ptyro/tests/chat_compare.rs`'s
`a_missing_agent_offers_setup_not_the_os_error` (a chat with no agent, and `ptyro new --agent`
of one that isn't there, against a real daemon); `web/test/setup.mjs` (`setupOffer`).

**Decided by the builder:** the daemon's errors carry the CLI's words (a command, which every
client can show), and the TUI and the web act on them through `setup_offer` rather than each
error growing a field; offers are recognised only in their "`…` installs" form, so a message
merely naming `ptyro install` never turns into a setup button; the chat's no-agent words name
Antigravity as what can't chat rather than as the fix.

### 24.11 The final fix wave **[built 2026-11-07]**

What the whole-feature review found, and the rulings carried into one change (PR #87):
- **The daemon's PATH follows installers on Windows** (§24.2, the review's I1).
- **`locate` searches the version table's vendor folders.** Each vendor method names its `bin`,
  the folders its installer puts the program in (`~/.local/bin` for Claude and Codex's
  install.sh, `~/.opencode/bin`, `%CODEX_INSTALL_DIR%` then `%LOCALAPPDATA%\Programs\OpenAI\Codex`,
  `%LOCALAPPDATA%\agy\bin`), and `locate::places` adds them for this platform (`vendor_bins`,
  expanded by `expand_place`; one unset variable skips that entry). OpenCode's
  `~/.opencode/bin` and Codex's Windows folder were missing, so a fresh install was "nowhere
  ptyro looks", and BOOT7's move of OpenCode from Homebrew never found its new copy (`fresh()`
  reads `find_all` over the same places; the copy in `~/.opencode/bin` is OpenCode's by its
  `places`). The catalog's test requires a `bin` of every vendor method with `places`, and
  `locate::vendor_tests::every_vendor_bin_in_the_table_is_searched` requires each to be
  searched, so the two can't drift.
- **The web's Log in** runs the installed name, and logs WSL's side in too (§24.4).
- **The administrator's update command quotes its prefix** in each shell's single quotes
  (`PTYRO_PREFIX='/Users/Jane Doe'`), a quote in it escaped.
- **A ptyro in Nix's or Guix's store is theirs** (`How::Managed`, "update it with Nix (`nix
  profile upgrade`, or your Nix configuration)"), never a sudo swap into a read-only store.
- **`wsl.exe` in the background has no window.** `ptyro_core::os::no_window` (`CREATE_NO_WINDOW`
  on Windows, nothing elsewhere) for every `wsl.exe` run whose output is read or is a pipe: the
  probe and its `-l -q`, `--version` in a distro, setup's survey and its daemon run, the logins'
  `--check --json`, the self-update's version questions and the client's proxy bridge
  (`remote::wsl`), through `remote::hidden_wsl`. `ptyro proxy`'s own bridge keeps ssh's console,
  so wsl.exe's errors reach ssh's terminal. Those that share the
  person's terminal keep its console (a login through `wsl_setup::interact`, setup's run that
  asks for sudo's password, first run's install in a distro, starting WSL's daemon from a
  terminal, the self-update of the other half). The holder that keeps a WSL distro open starts
  `wsl.exe` from inside WSL, through interop, where no Windows flag applies.
- **The setup lock is airtight on NFS.** `Lock::take` claims its path in this process before it
  opens the file (a second take here is refused without touching it: closing any descriptor of
  the file lets go of an fcntl-emulated lock), gives the claim up on every way out without the
  lock, and `Drop` closes the file before the claim goes. Tests:
  `a_second_take_here_never_opens_the_lock_file` (a folder where the lock file was: opening it
  would let the take through), `a_refused_take_leaves_no_claim_behind`.
- **The TUI's setup guard** stays while a spawn asks to be confirmed first (`NeedsConfirmation`:
  a `y` still starts the split), and goes when that is declined.
- **A once-only shell watch clears before its attention is raised.** `raise` sends the
  attention, then awaits the desktop notification (slow inside WSL, through interop); a client
  between the two saw a fired `match` or `done` watch still set (WSL CI's
  `shells::long_commands_and_watches_say_so`, twice in run 42409928325).
- **The prompt watch** looks for a dialog before anything else, and keeps watching an agent that
  spoke (§8.2).
- **Smaller:** the repair line after a move says "install it again" once; `sh_quote`'s doc
  comment is back on it; `docs/config.md`'s sample has `updates.keep_installer`; boot4's Ctrl+C
  test waits for the login's line and no longer sleeps after it (the handler is in place before
  the login starts); the web offers setup only to a full-scope device and names each item by the
  checklist's name (§24.10); the chat picker's missing agent sets itself up on ⏎; `ptyro init`'s
  default-yes is tested (`boot2::init_offers_the_setup_checklist`, Enter alone opens it).
