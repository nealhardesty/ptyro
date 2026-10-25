# ptyro — Roadmap

This file is the source of truth for **what does not exist yet**. The design behind every item is in
`SPEC.md` (section references like §5.6 point there). When a task ships, tick it here, make sure
`SPEC.md` reflects reality, and make sure `README.md` tells users how to use it.

Conventions:

- Task IDs (`M1.4`) are stable; reference them in commits and PRs (`M1.4: query responder`).
- Every milestone has a **Done when** — a demo-able acceptance test. A milestone is not done until
  that demo works on Linux *and* macOS.
- `[ ]` todo · `[~]` in progress · `[x]` done · `(spike)` = experiment whose result is written back
  into SPEC §20 / §21 before dependent work starts.
- **The CLI, the TUI and the web are all first class** (SPEC §1.3): a task is done when it
  works completely in all three, and its **Done when** demo runs in the TUI. The CLI and the TUI
  are always in parity; the web part lands in the same change, and a missing one is a defect.
- Estimates are rough focused-effort sizes: **S** ≤ 1 day, **M** 2–4 days, **L** 1–2 weeks.

## Overview

| Milestone | Theme | Outcome |
|---|---|---|
| **M0** | Foundations & spikes | Repo skeleton, CI, and the risky assumptions proven |
| **M1** | One session, rock solid | Daemon + PTY + emulator + attach/detach for a single Claude session |
| **M2** | Multiplexer core | Many sessions, TUI shell, scrollback/copy, splits, config |
| **M3** | Projects & worktrees | The workspace model: projects, safe worktrees, scripts, ports, per-workspace profiles (accounts) |
| **M4** | Agent intelligence | Adapters for Claude/Codex/Gemini/OpenCode, status engine, attention, notifications, resume |
| **M5** | Scriptable control plane | CLI/API parity, `--json`, `wait`/`send`/`events`, onboarding, doctor |
| **M6** | Review & ship | Diff inspector, comments → agent, commit/PR/checks via `gh`, integrate |
| **M7** | Orchestration | Fan-out & compare, recipes, gates, `ptyro mcp` for lead agents |
| **M8** | Hardening | Live upgrade, recorder/replay, perf, fuzzing, crash safety |
| **M9** | Remote | SSH remote TUI, WebSocket server, PWA/phone client, remote approvals |
| **M10** | Structured sessions | ACP / stream-json / app-server sessions with a native chat view (done) |
| **M11** | Change sets | One coordinated change across several repos: lead + member agents, contract, integration overlay, set checkpoints, merge train (done) |
| **H1** | Hardening review | ~120 findings of a full code review fixed (security, crashes, correctness, build) (done) |
| **UX1** | Out of the box | From `cd repo && ptyro` to a shipped, cleaned-up worktree by keys alone; sidebar rows that say what's going on (done) |
| **WEB1** | Web parity | The web client does what the CLI and TUI do, and the parity check covers it (done) |
| **CFG1** | One configuration panel | Every option editable in the TUI, on the web and with `ptyro config set`, from one schema; new options must join it (done) |
| **NOTE1** | The scratchpad | One persistent notepad file: a key opens it in your editor, `PTYRO_SCRATCHPAD` finds it, items become tasks, the web edits it (done) |
| **H2** | Second hardening review | A review of the whole codebase by nine independent readers: ~150 findings verified and fixed; locks, the release pipeline and uninstall done; the rest listed here |
| **Backlog** | Later / maybe | Editor, tasks, jj, libghostty, relay, … |

Critical path: **M0 → M1 → M2 → M3 → M4** gives a daily-driver. M5 can interleave with M3/M4.
M6–M10 are largely independent of each other after M5. M11 builds on M6 (PRs) and M7 (runs,
mailbox, `ptyro mcp`); its experiment M11.0 can run as soon as M5 exists. UX1, the owner's top
priority from 2026-10-22, is done (2026-10-23), and so is WEB1 (2026-10-24).

---

## M0 — Foundations & spikes

Goal: de-risk the hard parts before writing the real thing. The spikes lived in `spikes/` and were
promoted into the M1 crates (the `pty-pane` spike became `ptyro-term`, `ptyro-pty` and `ptyro-headless`).

- [x] **M0.1** Cargo workspace skeleton per SPEC §3.3; `rust-toolchain.toml`; shared lints
      (`clippy::pedantic` selectively, `unsafe_code = "deny"` except `ptyro-pty`); `cargo deny`
      config; `LICENSE` (MIT, §21.1) exists, add `license = "MIT"` to each crate. **S**
      *Done 2026-10-20:* 12 crates (stubs except `ptyro --version`), toolchain 1.99.1, `clippy::all`
      (pedantic deferred until there is real code to tune it on), `deny.toml` green, root `Makefile`.
- [~] **M0.2 (spike)** *PTY → alacritty_terminal → ratatui* pipeline: spawn `claude` in a
      `pty-process` PTY, feed `alacritty_terminal`, paint a ratatui pane, forward keys via
      `terminput`. Evaluate: rendering fidelity (Claude, nvim, htop), resize/reflow, kitty keyboard
      (Shift+Enter in Claude), synchronized output, latency feel. Record findings in SPEC §20. **M**
      *[~] 2026-10-20:* the spike worked headless on Linux with Claude, Codex, Gemini, vim
      and htop (docs/compat.md), and with its own TUI dogfooded inside the emulator; findings are in
      SPEC §5.6 and §20.5. **Left:** a human checks how it feels in real terminals
      (now with the real product: `make try CMD=claude`), plus macOS.
- [x] **M0.3 (spike)** Claude hook injection: verify `--plugin-dir` with a generated plugin
      `hooks/hooks.json` is additive to user hooks; compare `--settings` inline hooks merge
      behaviour. Capture real payloads of `SessionStart`, `UserPromptSubmit`, `PermissionRequest`,
      `Notification`, `Stop`, `StopFailure`, `SessionEnd` as fixtures. **S**
      *Done 2026-10-20:* both mechanisms are additive, `--plugin-dir` chosen (SPEC §8.2).
      `--session-id` and resume verified. Fixtures are in `crates/ptyro-agents/tests/fixtures/claude-2.1.304/hooks/`;
      `StopFailure` is not captured yet (it needs a real API failure).
- [x] **M0.4 (spike)** Codex hook/notify injection without touching `~/.codex`: try `-c` inline
      hooks, `--profile` layer, `CODEX_HOME` overlay (+ `auth.json` symlink); confirm hook trust
      behaviour; capture payload fixtures; determine graceful-quit sequence. **S**
      *Done 2026-10-20:* inline `-c` hooks plus inline `hooks.state` trust hashes, which ptyro
      computes (verified bit-for-bit), and `-c projects={…}` to skip the worktree trust dialog
      (SPEC §8.4). `SessionStart`/`UserPromptSubmit` captured. **Follow-up M0.4b:** capture
      `Stop`/`PermissionRequest`/`notify` after `codex login`.
- [x] **M0.5 (spike)** Gemini hook injection via `GEMINI_CLI_SYSTEM_SETTINGS_PATH` merge file;
      confirm `--session-id` + `--resume <uuid>`; capture title strings for Ready/Action
      Required/Working. **S** *Done 2026-10-20:* the system-settings route is impossible (needs
      root-owned dirs); an opt-in linked **extension** works instead (SPEC §8.5). `--session-id`
      verified via hooks; `GEMINI_CLI_HOME` verified. **Follow-up M0.5b:** turn hooks and the
      Action Required/Working titles, once a working API key or Google login is available.
- [x] **M0.6 (spike)** OSC 7501: implement probe reply in the M0.2 spike; confirm Claude reports
      `working/blocked/idle` with `kind`; measure latency vs hooks; confirm ordering rule (7501
      reply before DA1). **S** *Done 2026-10-20:* every state seen; 7501 runs 50–80 ms ahead of
      hooks. This led to a status-fusion fix: most recent wins among high-confidence sources
      (SPEC §9.2).
- [x] **M0.7** CI: GitHub Actions matrix (ubuntu-latest, macos-latest): fmt, clippy, test, deny;
      cache; nightly-free. **S** *Written* (`.github/workflows/ci.yml` → `make check` + cargo-deny).
      *Done 2026-10-23:* `ci` runs green on master on both platforms.
- [x] **M0.8** `docs/compat.md` skeleton (the manual compatibility matrix from SPEC §18.2) and
      `docs/fixtures.md` (how to record `.ptylog` fixtures). **S**
- [x] **M0.9** Decide §21.1 (license: MIT) and §21.2 (prefix: `Ctrl-Space`, configurable; changed to `Ctrl-Q` plus `Ctrl-Space` on 2026-10-22) with the owner. **S**
- [ ] **M0.10 (spike, deferred to M3 by the owner)** Multi-account isolation (SPEC §10.10), for each installed agent with two
      real logins side by side on Linux *and* macOS:
      - does the isolation variable (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`) fully
        separate auth, settings and conversation storage?
      - macOS Keychain entry naming for Claude, and Codex keyring storage: are they per-dir?
      - does Claude's `.claude.json` move into the dir?
      - does ptyro's hook injection (M0.3/M0.4) still work in a non-default dir?
      - which command reports the logged-in account?
      Record the results in SPEC §20. **S**

**Done when:** the M0.2 spike runs Claude Code interactively inside a ratatui pane well enough to
complete a real task, and spike results for M0.3–M0.6 and M0.10 are written into SPEC §20 with "verified" tags.

---

## M1 — One session, rock solid

Goal: the core loop of a tmux replacement, for exactly one session, with no shortcuts that would
have to be undone later.

**ptyro-pty**
- [x] **M1.1** Spawn with env policy (SPEC §5.1 table, strip `TMUX` & friends), own process group,
      resize, signal-group, wait for exit with status. Unit tests with `/bin/sh -c`. **M**
      *Done:* `ptyro-pty`; env policy also strips parent-Claude markers, `COLUMNS`/`LINES`; tests cover size, env, group kill.
- [x] **M1.2** Foreground-process + cwd probes (Linux `/proc`, macOS `libproc`). **S**
      *Done:* Linux `/proc`, macOS `libproc` (type-checked for aarch64-apple-darwin; runtime-tested on Linux only).

**ptyro-term**
- [x] **M1.3** `Emulator` trait + `AlacrittyEmulator` (feed, resize with reflow, damage, rows,
      cursor, modes, title, drain replies/events). **M**
      *Done:* `snapshot()` for live upgrade is deferred to M8.1.
- [x] **M1.4** Query responder (SPEC §5.6 table) incl. OSC 7501 probe reply and ordering rule. **M**
      *Done:* VT220 DA1, XTVERSION, `CSI 16 t`, XTGETTCAP (TN/Co/RGB), OSC 10/11 with the viewer's real colours.
      Port the spike's sniffer-before-emulator ordering. Add what alacritty lacks (XTVERSION,
      `CSI 16 t`, XTGETTCAP) and override its VT102 DA1 reply.
- [x] **M1.5** Side-channel sniffer: OSC 7501 reports, OSC 9/777/99, OSC 0/2, OSC 7, OSC 133,
      OSC 52, OSC 9;4, BEL — with length caps. Fuzz target. **M**
      *Done:* plus OSC 7 cwd, OSC 133 marks, OSC 9;4 progress (distinguished from OSC 9 notifications). "Fuzz target" is a proptest never-panic test; a `cargo fuzz` target can follow.
- [x] **M1.6** Row-version table + `ScreenFrame` builder (keyframe & delta, style table, wide runs). **M**
      *Done:* content-hashed row versions; property test (20k cases) proves deltas == keyframes.
- [x] **M1.7** Input encoding: keys (xterm + kitty flags + DECCKM/DECKPAM), mouse (SGR/UTF-8/X10
      per mode), bracketed paste with `ESC[201~` stripping, focus events. Table tests. **M**
      *Done:* kitty flags by name, DECCKM, SGR + X10 mouse, paste defence, focus. Not yet: DECKPAM keypad, UTF-8 (1005) mouse.
- [x] **M1.8** Golden-test harness: `.ptylog` fixtures → screen snapshots (`insta`). First fixtures:
      Claude startup, Claude permission prompt, nvim open file, htop, `ls --color`. **M**
      *Done:* fixtures for Claude (fullscreen main screen, permission dialog), Codex, vim, `ls --color`; chunking-independence check. htop skipped (its screen lists your processes).

**ptyro-protocol / ptyro-client**
- [x] **M1.9** Framing + msgpack/json codec, `Hello/Welcome/Reject` handshake, request/response
      correlation, push messages; round-trip tests. **M**
      *Done:* frames self-identify msgpack vs JSON; forward-compatible decoding tested.
- [x] **M1.10** Async client library: connect (with auto-spawn of the daemon and ready-pipe),
      request helpers, frame stream, reconnect with backoff. **M**
      *Done:* readiness by polling the socket instead of a ready pipe (SPEC §4.1); reconnect with backoff lives in the TUI.

**ptyro-daemon**
- [x] **M1.11** Daemon bootstrap: paths (§14.1, `PTYRO_HOME`), lock file, stale-socket probe,
      `0700`/`0600` perms, peer-uid check, `sun_path` length check, logging to file. **M**
      *Done:* also refuses a socket dir owned by another uid; `/tmp` fallback for long paths.
- [x] **M1.12** Session actor: PTY read loop always draining into the emulator, replies written
      back, frame-ready notifications via `watch`. **M**
      *Done.*
- [x] **M1.13** Client hub: subscriptions, per-client pacing (tick + one-unacked-frame), controller
      tracking, sizing policy (§5.8), sync-output holding. **M**
      *Done:* sync-output holding comes free from alacritty's processor (frames never show half a 2026 batch).
- [x] **M1.14** `daemon start|stop|status`, SIGTERM handling, graceful child shutdown (§4.4). **S**
      *Done:* SIGTERM stops sessions gracefully until hibernation exists (M4.12).

**ptyro (binary) / ptyro-tui (minimal)**
- [x] **M1.15** `ptyro new -- <cmd>` (single session), `ptyro attach`, `ptyro attach --raw`,
      `ptyro ls`, `ptyro kill`. **M**
      *Done:* plus `rm`, `rename`, `read`, `send`.
- [x] **M1.16** Minimal TUI: one full-screen pane + status line, prefix key, `prefix d` detach,
      outer-terminal setup/teardown (alt screen, raw mode, kitty enhancement push/pop, mouse,
      bracketed paste, focus reporting) with panic-safe restore. Query the outer terminal's real
      OSC 10/11 colours at start and pass them to the daemon; Codex and Gemini theme from them. **M**
      *Done:* prefix fixed at Ctrl-Space until the key-map engine (M2.5) reads config; OSC 52 forwarding, bell, cursor shape, reconnect.
- [x] **M1.17** Testkit v0: temp daemon harness + `ptyro-fake-agent` (prints colours, alt screen,
      asks a fake y/n prompt, emits OSC 7501). Integration test: spawn → attach → type → detach →
      reattach → screen identical. **M**
      *Done:* `crates/ptyro/tests/e2e.rs`, 12 tests (screen restore, responder, status, env, SIGPIPE, slow client, sizing, addressing, protocol reject, single instance + stale socket, CLI exit codes, SIGTERM, side channels).

*Status 2026-10-20:* every task is done. The demo is automated: attach, type, detach, then reattach
from another terminal at another size with the screen restored, run via `ptyro-headless` and
`e2e.rs`. **Left for the owner:** the same by hand in real terminals, plus macOS CI.

**Done when:** `ptyro new -- claude` starts Claude; you use it normally (incl. Shift+Enter, paste,
resize); `prefix d` detaches; close the terminal; from a *different* terminal emulator,
`ptyro attach` restores the exact screen instantly and Claude never noticed. Same for `nvim` and
`htop`. The fake-agent integration test passes in CI on both OSes.

---

## M2 — Multiplexer core

Goal: a multi-session terminal workspace you could already use instead of tmux for agent work.

- [x] **M2.1** Session registry & addressing (§2.3): ULIDs, short handles, friendly corvid names,
      `@last`, `@focused`, multi-target selectors. **M**
      *Done:* `@focused`, `@all/@live/@exited`, `@working/@blocked/…`, globs, `,` unions; CLI multi-target via `Resolve`. Project-qualified names, `@attention` and `@run:` come with M3, M4 and M7.
- [x] **M2.2** Store v1 (rusqlite + migrations, store actor): sessions, session events, kv. **M**
      *Done:* `ptyro-store` (session, session_event, kv), orphan sessions from earlier runs closed, retention pruning, `ls --all`. The daemon degrades gracefully without the database.
- [x] **M2.3** Config loader: user `config.toml`, validation with precise errors, hot reload,
      `ptyro config path|edit|check|show`. **M**
      *Done:* `[ui] [daemon] [keys] [themes.*]`, precise errors, reserved future sections accepted, directory watch with live reload in daemon and TUI, `config path|check|show|edit`.
- [x] **M2.4** TUI shell: sidebar (sessions list for now), pane header, status bar, focus model
      (terminal vs navigation mode), help overlay generated from the key map. **L**
      *Done:* sidebar, pane headers, status bar with mode badges and clock, sidebar/pane focus, help generated from the live key map (filterable).
- [x] **M2.5** Key map engine: prefix, after-prefix bindings, `global:` bindings, config
      overrides, `prefix prefix` literal. **M**
      *Done:* prefix/global/nav tables, overrides and unbinding, a configurable prefix, prefix-prefix literal; the prefix also escapes any overlay.
- [x] **M2.6** Command palette (`:` / `Ctrl-p`) with fuzzy search (`nucleo`) over all commands. **M**
      *Done:* nucleo-matcher palette: actions (with their keys), sessions and themes.
- [x] **M2.7** Session switching: `j/k`, `1-9`, `/` fuzzy finder, `Tab`. **S**
      *Done*, plus `` ` `` for the previous session in the pane.
- [x] **M2.8** Scrollback: `ReadScrollback` ranges, client cache, mouse wheel, `scrollback_epoch`. **M**
      *Done, design changed:* frozen `ScrollbackDoc` snapshots instead of ranges + epoch (SPEC §5.4); mouse wheel enters copy mode; `read --scrollback`.
- [x] **M2.9** Copy mode: vi motions, search, selection, yank via OSC 52 to the outer terminal
      (tmux-passthrough aware), prompt jumps via OSC 133. **L**
      *Done:* vi motions with counts, words, `/ ? n N` smartcase search, char/line/block selection, `y`/`Y`, OSC 52 (tmux-wrapped), paste buffer (`prefix ]`), prompt jumps `[ ]` via in-grid hyperlink marks (SPEC §5.7), mouse-drag selection.
- [x] **M2.10** Splits: two-to-four panes in the main area (h/v), focus movement, zoom,
      drag-resize; layout saved per workspace view. **L**
      *Done:* up to four panes, `v`/`s` with a picker, `h/l` and Shift-arrows, `z`, `c`, keyboard and drag resize, view persisted in the store.
- [x] **M2.11** Shell sessions (`kind = shell`), command sessions, `ptyro shell-init` snippets for
      OSC 7/133. **M**
      *Done:* shell (`$SHELL -l`) and command sessions; `shell-init bash|zsh|fish` (OSC 7 + 133, inert outside ptyro).
- [x] **M2.12** Exited/dormant session presentation: final screen greyed, exit code, `restart`. **S**
      *Done:* greyed final screen, exit banner, ⏎ or `prefix C-r` restarts in place (same id), `x` removes. Dormant arrives with M4.
- [x] **M2.13** Multiple simultaneous clients: view-only, controller handover, letterbox/crop with
      size badge (§5.8). **M**
      *Done:* `attach --view-only`, take-control (`=`), crop with badge and auto-pan to the cursor.
- [x] **M2.14** Themes: `ptyro-dark`, `ptyro-light`, `high-contrast`; `NO_COLOR`; colour
      downsampling for 256/16-colour outer terminals. **S**
      *Done:* three built-in themes plus user themes, `NO_COLOR`, truecolor/256/16 downsampling of chrome and content, `theme.next` and palette switching.
- [x] **M2.15** Mouse: click-to-focus, wheel scroll, split-border drag, passthrough when the child
      grabbed the mouse. **M**
      *Done:* click-to-focus (sidebar and panes), wheel, divider drag, drag-select, passthrough to mouse-reporting programs.

*Status 2026-10-20:* every task is done. `crates/ptyro/tests/m2.rs` drives the real TUI inside the
headless emulator: splits restored on reattach, copy-mode search and yank and paste, palette,
finder, sidebar, restart, view-only, custom prefix. The daemon side is tested too: selectors,
restart, scrollback documents with prompt marks, views and history across a daemon restart, and
config reload with errors. **Left for the owner:** the demo by hand with real agents (Codex needs
`codex login`), and macOS CI.

**Done when:** you can run 5+ sessions (2 Claude, 1 Codex, nvim, a shell) in one TUI, switch with
keys and the palette, split two side by side, scroll and copy from scrollback, attach a second
terminal as a viewer, detach, and reattach later with layout intact.

---

## M3 — Projects, workspaces & worktrees

Goal: the core workflow — "new idea → new isolated worktree with an agent in it" in two keystrokes,
with zero risk of losing work.

- [x] **M3.1** Store v2: workspace, project, workspace_project, worktree, trust tables, incl.
      profile/notify columns (§14.2). **S**
      *Done:* migration 2 (workspace, project, workspace_project, worktree with a partial unique name index, trust; session gains profile/worktree/project/workspace/agent/script/launch). An in-memory `Registry` writes through, so the store stays optional.
- [x] **M3.2** Projects: `project add|ls|rm|show`, default branch & remote detection, discovery
      scan of `discover.roots` with cache. **M**
      *Done:* `project add|ls|rm|show|config|discover`. The main checkout is found from inside linked worktrees; default branch from `origin/HEAD`; scan of `discover.roots`, cached for an hour; TUI `project.add` picker.
- [x] **M3.3** Workspaces: default `main`, `ws new|ls|rm|rename|add|remove`, saved views,
      workspace switcher in TUI. **M**
      *Done:* `main` always exists; `ws ls|new|set|rm|rename|add|remove`; views saved per workspace with the last one remembered; `C-q w` switcher.
- [x] **M3.4** `ptyro-git`: worktree add (fetch base, new/existing branch, `pull/<n>/head`), lock
      while running, ptyro marker file, `worktree list --porcelain` reconciliation, symlink refusal. **L**
      *Done:* `ptyro-git` (the git CLI, `GIT_OPTIONAL_LOCKS=0`): fetch the base (non-fatal offline), new/existing/remote branch, `pull/N/head`, `--no-track`; lock while sessions run; `ptyro.json` marker; reconciliation (missing, re-adopted); symlink refusal on ptyro-owned path parts.
- [x] **M3.5** Worktree removal safety matrix (§10.8): running sessions, uncommitted, untracked,
      unpushed, stashes; branch keep/delete rules; precise refusal reports. Exhaustive tests. **M**
      *Done:* refuses on running sessions, uncommitted, untracked, unpushed-and-unmerged commits and stashes, with a precise report. `--force` asks; the branch is deleted only if merged or identical to its upstream. Covered by unit tests against real repos plus e2e and TUI tests.
- [x] **M3.6** `.worktreeinclude` (gitignore semantics, matched-and-ignored), `worktree.copy`,
      `worktree.share` with CoW clone / symlink / copy fallbacks. **M**
      *Done:* matching is done by git itself (`ls-files --others --ignored --exclude`, `check-ignore`); `share` clones with CoW, falling back to symlink or copy, and excludes the shared entry so a symlink never counts as untracked work.
- [x] **M3.7** Project config `.ptyro.toml` + `.ptyro.local.toml` layering; agent-executable
      restriction (§13.1). **M**
      *Done:* `.ptyro.toml` deep-merged with `.ptyro.local.toml` (main checkout and worktree); `[agents]`, `[profiles]`, `[ui]`, `[daemon]` and `[keys]` in a repository are errors that name the rule.
- [x] **M3.8** Script runner: setup/run/archive in `script` sessions, process-group stop,
      `run_mode = exclusive`, `after_turn` hook point (wired in M4). **M**
      *Done:* setup/run/archive/after_turn in read-only `script` sessions (`<wt>.<script>`), stopped with TERM then KILL after 5 s; `run_mode = exclusive`; `start_agents = after_setup`; `after_turn` runs via `wt run W after_turn` (automatic in M4).
- [x] **M3.9** Trust flow: fingerprint of `[scripts]`, TUI dialog showing exact script text, CLI
      `project trust`, re-prompt on change. **M**
      *Done:* SHA-256 of the merged `[scripts]`; the TUI dialog shows the full text *before* creating the worktree; `project trust [--yes]`; any change asks again.
- [x] **M3.10** Port blocks (§10.7) + listening-port detection per session shown in sidebar. **M**
      *Done:* first-fit blocks, bind-tested; a listening-port scan every 3 s over each session's process tree (`/proc` on Linux, `lsof` on macOS), shown in the sidebar, header and `ls`.
- [x] **M3.11** Env injection for worktree sessions/scripts (§10.6) incl. optional Conductor
      aliases. **S**
      *Done:* `PTYRO_PROJECT/ROOT_PATH/WORKTREE_PATH/WORKTREE_NAME/BRANCH/PORT/PORT_COUNT`, `[compat] conductor_env`; inherited copies are stripped.
- [x] **M3.12** Sidebar becomes the full tree: workspace → project → worktree → sessions, with
      branch, diffstat, dirty marker, ports. Live repo status via `notify` (debounced). **L**
      *Done:* the tree with folding, branch, `+/−`, dirty marker, state and ports; `notify` per worktree (500 ms) plus 10 s/60 s refresh; pinned profile badges.
- [x] **M3.13** New-session / new-worktree wizard (`n`, `N`), fast path `n ⏎ ⏎`. **M**
      *Done:* `n` (agent, then target; `n ⏎ ⏎` = default agent in a new worktree), `N` (name, then agent). The profile is shown with its reason; the pane follows the agent once setup ends.
- [x] **M3.14** `wt open` editors (§13.2), `nvim` "inside" mode opens as a ptyro session. **S**
      *Done:* `[editors]` plus built-ins (nvim/vim/hx inside, code/zed/cursor outside); `wt open`, `C-q o`.
- [x] **M3.15** `ptyro gc` (stale worktrees report, interactive cleanup). **S**
      *Done:* `gc [--dry-run]`: gone, merged, unused for `stale_days`; asks for each, never forces.
- [x] **M3.16** Branch name templates + optional auto-rename-from-first-prompt (`wt rename-branch`). **S**
      *Done:* `{name} {user} {date} {slug}` (`{issue}` empty until issues exist); `wt rename-branch` refuses once pushed and can name the branch after the first commit. Naming from the first *prompt* waits for prompt delivery (M4).
- [x] **M3.17** Profiles core (§10.10): `[profiles.*]` schema (user config only), resolution order,
      env construction (managed-var scrub always, credential scrub when isolated; `default` isolated
      once any profile exists), `PTYRO_PROFILE`,
      `agent@profile` syntax, managed dirs under `data/profiles/`. Resume bound to the profile.
      Table-driven tests for every spawn path (TUI, CLI, resume, scripts, MCP). **M**
      *Done:* the schema, resolution order, env construction (`ptyro-agents::profile`), `PTYRO_PROFILE`, `agent@profile`, managed dirs. Restart is bound to its profile. Table-driven tests plus e2e for the CLI, TUI, restart and script paths; MCP arrives in M7.
- [x] **M3.18** Profile secrets: `{ command = [...] }` resolution per spawn, memory-only TTL cache,
      `<secret:NAME>` in the DB, redaction in logs, `profile env` and protocol replies. **S**
      *Done:* `{ command = [...] }` per spawn with a TTL cache, `<secret:NAME>` in the launch record (the test greps the DB and WAL), never logged; `profile env` never resolves secrets.
- [x] **M3.19** Profile CLI: `profile ls|show|new [--adopt]|rm|rename|env|login|sync`,
      `ws new|set --profile`, `project pin|unpin`, `new --profile/--override-pin`. **M**
      *Done:* everything listed, plus `profile detect [--apply]` (the owner's aliases) and `profile doctor`. Config edits use `toml_edit` (comments kept), validate, then `ReloadConfig`.
- [x] **M3.20** Guardrails: `match` rules → pin suggestion on `project add`; spawn-time guard
      (`confirm|refuse|warn`); pin-override confirmation; `share` symlinks that refuse credential
      files. **M**
      *Done:* auto-pin on add when exactly one profile matches (the TUI asks to keep it); guards (`confirm` asks, `refuse`, `warn`); pin-override confirmation (`needs_confirmation`, CLI `--yes`/`--override-pin`); `share` never links credential files.
- [x] **M3.21** Identity in the TUI: profile label and colour in the workspace header, sidebar,
      session header and status bar; optional border tint; the wizard shows the resolved profile and
      why. **S**
      *Done:* `[label]` badges in their colour on the workspace title, project rows, pane headers and status bar; `tint_borders`; the wizard title shows the resolved profile and why.

*Status 2026-10-20:* every task is done. `crates/ptyro/tests/m3.rs` (10 tests) checks against a
real daemon, real git (a bare "remote") and a fake `$HOME`:
- the worktree lifecycle: includes, share, ports, untrusted-then-trusted setup, the §10.6 env,
  locks, re-asking after a script change, every refusal, force with the kept branch, archive,
  and merged removal with branch deletion;
- scripts, exclusive run mode, live status and branch renames;
- listening ports;
- profiles: auto-pin, environment, secrets, guards, pin override, restart, the commit author, the
  SSH key, and secrets kept out of the DB;
- the profile CLI's config edits;
- workspaces, reconciliation and gc;
- editors;
- the TUI: the `n ⏎ ⏎` fast path through trust and setup to the followed agent, the `N` wizard,
  folding, refused removal and the workspace switcher.

**Left for the owner:**
- the Done-when with real Claude accounts: log in with `ptyro profile login work claude`, then
  check `/status` inside;
- a real `git push` with each profile's SSH key;
- macOS (FSEvents watching, the `lsof` port scan, APFS clones).

`make sandbox` gives a throwaway ptyro with a demo repository to try it in.


**Done when:** from a fresh repo you register it, press `N`, and get a new worktree on
`ptyro/<name>` with `.env` copied, setup script run (after trusting it), a port block assigned, and
Claude running in it; you can try to remove a worktree with uncommitted work and ptyro refuses with a
clear explanation; removing a clean merged one deletes it and its branch. With `work` and
`personal` profiles defined, a repo matching `github.com:acme/*` is pinned to `work` on add; a
Claude session there runs under the work account and commits with the work author; a shell in
`personal` pushes with the personal key; spawning `claude@personal` in the work repo asks for
confirmation first.

---

## M4 — Agent intelligence

Goal: ptyro *knows* what every agent is doing and tells you exactly when you're needed.

- [x] **M4.1** `Adapter` trait, capabilities, `LaunchPlan`, integration-file materialization under
      `data/integrations/` (never in user config or worktree). **M**
      *Done:* the synchronous `Adapter` trait (SPEC §8.1) with `Caps`, `LaunchPlan {argv, env, files, conversation, sidecar}`, `on_hook/on_title/on_screen/ready/answer/stop`; files land in `data/integrations/<agent>/<content hash>/`; `adapter = "…"` lets any agent name use any adapter; dangerous args need `dangerous = true` (⚠ badge).
- [x] **M4.2** `ptyro hook` relay: blocking socket client, 500 ms budget, silent-success failure
      mode, no-op without `PTYRO_SESSION_ID`. **S**
      *Done:* `ptyro hook <adapter> <event>` runs before any runtime: blocking `UnixStream`, 500 ms budget, always exit 0 and no stdout, no-op without `PTYRO_SESSION_ID`; Codex `notify` JSON as the last argument with `--chain` to the user's own notify.
- [x] **M4.3** Claude adapter: `--session-id` pre-assign, `-n`, hooks injection (per M0.3 result),
      OSC 7501 mapping, prompt delivery, graceful stop, resume with re-passed flags, multi-account
      via the session's profile (`CLAUDE_CONFIG_DIR`, M3.17). **L**
      *Done:* plugin-dir hooks, `--session-id`, `-n`, the statusline relay, OSC 7501, folder-trust and login recognisers, prompt delivery only into the real input box (Enter as an encoded key event: Claude ignores a bare `\r`), answers `1`/`2`/Esc, stop via Esc → `/exit` → HUP, resume with re-passed flags, per-profile `CLAUDE_CONFIG_DIR`. Checked against real Claude 2.1.304 with `make verify-claude` (trust dialog, delivery, working/blocked/idle from hooks, `ptyro answer`, hibernate, dormant across a daemon restart, resume into the same conversation).
- [x] **M4.4** Status engine (§9): reducer, precedence & staleness rules, persisted transitions,
      `status --explain`. Fixture-driven tests from M0.3–M0.5 payloads. **M**
      *Done:* `engine.rs`: high sources are peers (newest wins; a weaker one within 250 ms is deferred, not dropped), medium after `stale_after`, low last; strong details survive weak refreshes; a 64-entry explain ring (`ptyro status S --explain`, TUI `e`); every transition persisted with its previous confidence and work time. Tests replay the M0 fixtures.
- [x] **M4.5** (R-UX-02) Attention model + "next attention" (`a`) + inbox (`i`) + peek (`p`) with inline
      reply. **L**
      *Done:* attention raised unless the session is watched (focused pane *and* focused terminal, mode 1004); `a` with `attention.scope`; `i` inbox with per-workspace tabs; `p` peek with `y`/`a`/`n` answers and a typed reply; `ptyro answer`.
- [x] **M4.6** Notifier: channels (inbox, bell, OSC 9/777 via controller with tmux passthrough,
      desktop via `notify-rust` feature), per-category/project/session mute, quiet hours,
      coalescing, contextful messages. **M**
      *Done:* inbox always; then gated by `on` kinds, mutes (`ptyro mute` session/project/workspace, `--for`), workspace mode + breakthrough, quiet hours and coalescing (blocked always passes). Channels: OSC 777/9 and bell through each TUI (tmux passthrough), desktop via `notify-send`/`osascript` or `desktop_command` (not `notify-rust`: no new dependency), titles carry the profile label and say what is needed; `ptyro notify test`.
- [x] **M4.7** Codex adapter (per M0.4): notify chaining, hooks, id learning, title fallback,
      resume. **M**
      *Done:* `-c` hooks + `hooks.state` trust (hash golden-tested vs 0.143.0), chained `notify`, worktree trust when the project is trusted, id from `SessionStart`, braille-spinner title, update/trust/"hooks need review" recognisers, answers `y`/`a`/Esc, `/quit`, `codex resume`. Real turns are unverified (the Codex login returns 401), so it stays on hooks rather than M4.19's app-server.
- [x] **M4.8** Gemini adapter (per M0.5): `--session-id`, system-settings hook merge, title
      fallback, resume. **M**
      *Done:* the system-settings route was rejected in M0.5; instead an opt-in linked extension (`ptyro integrations install gemini`), `--session-id`/`--resume`, title fallback; `profile doctor` warns about retired consumer Gemini. Turn hooks unverified (API key 403).
- [x] **M4.9** OpenCode adapter: allocated `--port`, per-session password, SSE subscription,
      `session.status`/`permission.asked` mapping, resume. **M**
      *Done:* `--port`/`--hostname`, random `OPENCODE_SERVER_PASSWORD`, SSE `/event` follower (reconnecting), `session.status`/`permission.*`/`session.error` mapping, answers through `/session/:id/permissions/:id`, `--session` resume. Tested against the fake agent's OpenCode server only (not installed here).
- [x] **M4.10** Generic adapter + activity heuristic with low-confidence display (`◌`). **S**
      *Done:* `generic` trusts OSC 7501 from any program and otherwise uses titles, notifications and the heuristic (only screen-changing output counts), drawn `◌`; `shell` gets working/idle from OSC 133.
- [x] **M4.11** Dormant sessions & resume: reconciliation on daemon start (§7.5),
      `restore_on_start`, focus-to-resume, `ptyro resume`. **M**
      *Done:* `close_orphans` → dormant, `restore_on_start = dormant|resume|none` (resume staggers 3 at a time), `⏎`/focus resumes (`resume_on_focus`), `ptyro resume`; `daemon stop` hibernates resumable agents first.
- [x] **M4.12** Hibernation (`hibernate_after`), guarded by idle + not visible + no child
      processes. **M**
      *Done:* `hibernate_after` tick every 2 s: idle that long, no viewers, conversation known, and the PTY's foreground process is the agent itself; `ptyro hibernate` on demand.
- [x] **M4.13** Pane header "current activity" (`Bash: cargo test`, `Editing src/x.rs`) from
      `PreToolUse`/equivalents; progress bar from OSC 9;4 / OSC 7501 `progress`. **S**
      *Done:* header activity from hooks/7501 `msg`/`PreToolUse` tool summaries and a progress bar from 7501 `progress` / OSC 9;4.
- [x] **M4.14** `after_turn` script execution on turn end (opt-in). **S**
      *Done:* a trusted project's `[scripts] after_turn` runs on Working → Idle with the worktree env; `[agents.<name>] after_turn = false` opts out (trusting the script is the opt-in). Also `worktrees.auto_rename_branch`: an unnamed worktree's branch is renamed from the first prompt.
- [x] **M4.15** `ptyro-fake-agent` gains hook-calling modes for every adapter's payload shape, so
      CI covers all four adapters without real agents. **M**
      *Done:* `fake-agent --as claude|codex|gemini|opencode|antigravity` reads the integration ptyro actually generated (plugin hooks.json, `-c` hooks and notify) and calls it with each adapter's payload shapes; `--as opencode` serves the SSE/permissions API; plus `conformance` for 7501.
- [x] **M4.16** Workspace-scoped attention (§9.6): home workspace per session, `notify =
      all|badge|mute` with `breakthrough`, `attention.scope`, per-workspace inbox tabs, other
      workspaces as status-bar badges, profile label in notification titles. **M**
      *Done:* home workspace per session, `notify = all|badge|mute` with `breakthrough`, `attention.scope`, inbox tabs, `⚑N` badges for other workspaces, profile label in titles.

*Added 2026-10-20 from `RESEARCH.md` (R-ids point to its idea funnel):*

- [x] **M4.17** Usage and quota meters per profile (R-USE-01, promoted from Backlog):
      - Claude: a `ptyro statusline-relay` set as `statusLine.command`. It chains to the user's own
        statusline and forwards `rate_limits.five_hour` / `.seven_day` (`used_percentage`,
        `resets_at`), cost and context % over the hook socket.
      - Codex: a side app-server connection per `CODEX_HOME` on `account/rateLimits/updated`.
      - History via `ccusage --json` when installed.
      - Shown per profile in the status bar and sidebar, with a "rate-limited until T" state. **M**
      *Done:* the statusline relay (chains to the user's `statusLine`), opt-in Codex side app-server (`[agents.codex] usage = true`), `StopFailure(rate_limit)` → "limited until T", `ptyro usage` (+ `ccusage` when installed), sidebar and status-bar meters. Codex meters unverified (401).
- [x] **M4.18** Claude native bridges (R-ORC-01, R-AGT-02). **Spike first.**
      - Learn `CLAUDE_CODE_MESSAGING_SOCKET`/`_TOKEN` from the hook env and deliver NDJSON over the
        socket. Verify the token / own-child rule and `crossSessionInbound`.
      - Ingest the Notification types `agent_needs_input` / `agent_completed`, plus `TeammateIdle`
        and `TaskCompleted`.
      - Read `claude agents --json`.
      - Show Claude agent teams as child rows, and warn when a team shares one worktree.
      - The spike result goes to SPEC §20.1. **M**
      *Done:* Notification types `agent_needs_input`/`agent_completed`/`elicitation_dialog`, `TeammateIdle`, `TaskCompleted` and subagents as `+N` child rows; `claude agents --json` as a medium-confidence cross-check every 15 s when hooks are quiet (fields recorded in SPEC §20.1). **Not done:** socket delivery. The messaging protocol is undocumented and ptyro won't reverse-engineer its token handling, so prompts stay on the PTY until there's a documented interface or your decision. Two or more teammates in one worktree raise a "their edits can collide" notification (corrected 2026-10-20 in M5: this note first said the warning was missing).
- [x] **M4.19** Codex hybrid session (R-ORC-35). **Spike first.**
      - A daemon-owned `codex app-server --listen unix://…`, with `codex --remote unix://…` in the PTY.
      - `thread/status/changed` feeds the status engine; prompts go via `turn/start`/`turn/steer`;
        approvals are answered from the inbox.
      - The spike answers two questions: do approvals reach every client, and do injected turns render
        in the TUI?
      - If it holds, M4.7 uses it and M10.4 shrinks. **M**
      *Done:* spike recorded in SPEC §20.2: the app-server schema and rate-limit/approval methods are verified, but turns and approval routing couldn't be tested (401 despite "Logged in"). Not adopted; M4.7 stays on hooks. Re-run after `codex login`.
- [x] **M4.20** OSC 7501 conformance and interop (R-STA-01).
      - Audit against the current Program Status Protocol revision: ids and subtrees, the 256-record
        LRU, rejecting control characters in base64 fields, record lifetimes across OSC 133 `A` and
        process exit, RIS, suppressing OSC 9;4 on the root record, and notification rate limits.
      - Accept iTerm2 OSC 21337 (low confidence) and herdr `report-agent`.
      - Document precedence (including `applied:false`) and the tuios state-name mapping in SPEC §9.
      - Any 7501 emitter gets confident status in the generic adapter.
      - Conformance fixtures in `ptyro-fake-agent`.
      - An inbound `done` means "finished, results unseen" (idle plus attention), not orchestration
        `Done`. Add `Blocked{Unspecified}` (R-DD-02). **S**
      *Done:* `parse_psp` follows rev 0.4 (whole-report discard, ids/subtrees, `app` inheritance, 256-record LRU, `clear`, record lifetimes across OSC 133 A, exit and RIS, OSC 9;4 suppressed once 7501 is seen, most-urgent effective record); OSC 21337 (low) and `ptyro report` (alias `report-agent`, herdr/tuios state names); precedence and `applied:false` documented in SPEC §9.7; `fake-agent conformance`. Agents' `done` is idle + attention; generic tools' `done` is `Done`, which raises attention too. Blocked without a kind is "has a question".
- [x] **M4.21** Antigravity CLI adapter (R-AGT-01).
      - Gemini CLI was replaced by `agy` for consumer accounts on 2026-06-18, so M4.8 stays thin and
        the effort moves here.
      - Map hooks: `SessionStart`, `PreInvocation` → working, `Stop` with `fullyIdle` → idle. Resume on
        `conversationId`.
      - Spike a blocked signal (no permission hook exists; check OSC 7501).
      - `doctor` warns consumer Gemini users. **M**
      *Done:* `agy` adapter written from its docs (SessionStart, PreInvocation, PreToolUse, Stop with `fullyIdle`), opt-in `ptyro integrations install antigravity`, blocked only via OSC 7501; `doctor` warns consumer Gemini users. Unverified: agy isn't installed here.
- [x] **M4.22** External notification sinks for M4.6 (R-NOT-01): ntfy, Pushover, Slack/Discord
      webhooks, Telegram.
      - Approve/deny action links use signed, single-use, expiring tokens.
      - They are answered by a minimal callback listener (off by default; loopback or tailnet bind),
        which M9.2 later reuses. **M**
      *Done:* ntfy, Pushover, Slack, Discord, Telegram and `command` sinks (secrets via `curl --config -`, never argv), approve/deny links signed with HMAC-SHA256, single-use and expiring, served by the opt-in callback listener (GET shows a page, only POST acts).

*Status 2026-10-20:* every task is done except M4.18's socket delivery (needs your decision) and
M4.19 (spiked, waiting on a Codex login). `crates/ptyro/tests/m4.rs` (8 tests) runs a real daemon
with fake agents speaking each adapter's real integration:
- Claude: plugin hooks, prompt delivery, attention, answers, usage and the explain log (the
  real trust dialog is covered by `make verify-claude`);
- Codex, Gemini, Antigravity and OpenCode, including the OpenCode SSE and permissions API;
- generic, shell and `ptyro report`;
- dormant, resume and restart;
- notifications, sinks and signed links;
- `after_turn` with the branch auto-rename;
- the TUI's `a`, `p` and `i`;
- 7501 conformance and auto-hibernation.

`make verify-claude` checks the Claude adapter against the real Claude Code installed here, with
one tiny turn.

**Left for the owner:**
- `codex login` (the stored token returns 401), then re-check Codex turns, M4.19 and the missing
  fixtures;
- a working Gemini API key;
- installing OpenCode and agy to verify their adapters;
- the M4.18 messaging-socket decision;
- macOS desktop notifications;
- the Done-when with real agents.

**Done when:** with 2 Claude, 1 Codex, 1 Gemini session working in parallel, ptyro shows accurate
working/blocked/idle states within ~1 s of reality, `a` always jumps to the session that needs you,
notifications say *what* is needed, and after a reboot every agent session reappears as dormant and
resumes its conversation on focus.

---

## M5 — Scriptable control plane

Goal: everything the TUI can do, scripts and other agents can do too.

- [x] **M5.1** CLI parity pass against SPEC §12.2; every list/query command has `--json` with
      documented schemas (`docs/cli-json.md`) and snapshot tests. **L**
      *Done:* the §12.2 tree is reconciled both ways (SPEC §12.2 [built M5] lists what was added and what waits for M7–M9). `--json` is on every listing and query, the shapes are documented in `docs/cli-json.md`, and a snapshot of every shape lives in `crates/ptyro/tests/snapshots/m5__cli-json.snap` (`make snapshots`). Protocol 1.4 adds `events_lost`.
- [x] **M5.2** `send` (`--wait-ready`, `--raw`, multi-target), `read --screen|--scrollback`
      (`--plain|--ansi`), `wait --for … --timeout` (`--any|--all`), `events --follow` (NDJSON). **M**
      *Done:* `send S… TEXT` takes several sessions and `--wait-ready`; `--wait` reports how the turn ended (idle, blocked, stalled, exited) and covers R-STA-03, along with the new `turns` counter. `read --screen|--scrollback --plain|--ansi --json`. `wait --for … --timeout --any|--all`, plus `--match REGEX` (R-ORC-22) and `--stall`. `idle` means "after the prompt ptyro last typed", so `new --prompt` + `wait` is safe. `events` shows the persisted history, and `--follow` streams NDJSON. Also `broadcast`, `stop`, and `ls --status/--project/--workspace`.
- [x] **M5.3** Stable exit codes (§12.4). **S**
      *Done:* the codes of SPEC §12.4, plus `wait` → 5 when a session exits first and 7 on timeout or stall, and `doctor` → 1 on a problem. Tested in `m5.rs`, documented in `docs/cli-json.md`.
- [x] **M5.4** Dynamic shell completions (live session/worktree/project names). **S**
      *Done:* `ptyro completions bash|zsh|fish|elvish|powershell` uses clap_complete's dynamic engine (pinned `=4.6.11`). Session, worktree, project and workspace names come live from the daemon (400 ms budget, never starts one); agents, `agent@profile` and profiles come from the config.
- [x] **M5.5** `ptyro init` onboarding wizard (§12.3), including detection and adoption of
      existing multi-account setups per §10.10: alias/function parsing for config-dir variables
      (the owner's setup; golden tests over real-world rc snippets), suffix → profile naming,
      `includeIf` identities, orphan `~/.claude-*` dirs. **M**
      *Done:* `ptyro init [--yes] [--dry-run]`: terminal and agent checks, account detection, preferences, a config written by adding only missing keys, repositories to register, then completions, the service and the tour (offered; `--yes` never edits rc files or installs anything). Detection now handles multi-line functions, fish `set -x`, `${HOME}`, and suffix-less aliases (named after the account dir), with golden tests over real-world rc snippets (`crates/ptyro-agents/tests/fixtures/rc/`).
- [x] **M5.6** `ptyro doctor` + `profile doctor`: agents/versions/auth per profile (logged in as
      whom), leaked managed vars in the daemon env, pins contradicting `match`, terminal caps,
      tmux settings advice, socket perms, git version, hook-integration self-test (spawns fake agent). **M**
      *Done:* `ptyro doctor [--quick] [--json]` and `profile doctor`:
        - logins via each tool's own status command (`claude auth status --json`, `codex login status`, `gh auth status`), never by reading credentials;
        - versions against those verified;
        - account variables leaked into the daemon, and agents missing from the daemon's PATH;
        - pins that contradict `match`; the terminal and kitty keys; tmux advice;
        - socket permissions and git version;
        - the hook self-test, which runs the fake agent through the real Claude, Codex and OpenCode integrations.

        The Codex "hooks need review" screen is now recognised and reported here (part of R-AGT-06; `--repair` is open).
- [x] **M5.7** `daemon install-service` / `uninstall-service` (systemd user unit, launchd agent). **S**
      *Done:* `daemon install-service` / `uninstall-service [--dry-run]`:
        - systemd: the user unit carries this shell's PATH and XDG/PTYRO_HOME, with `KillMode=mixed` (agents hibernate on stop) and `RestartPreventExitStatus=5`;
        - launchd: a plist with `KeepAlive{SuccessfulExit=false}`;
        - nothing is started or stopped behind your back.

        Tested with a fake `systemctl`; the launchd path is unverified (no macOS here).
- [x] **M5.8** `broadcast` to multiple sessions (TUI multi-select with `space` + `b`). **S**
      *Done:* `ptyro broadcast S… TEXT` sends to each session once it's ready, in parallel. In the TUI, Space marks sessions in the sidebar (`◆`) and `b` sends one prompt to all of them.
- [x] **M5.9** `docs/scripting.md` cookbook (shell recipes: overnight batch, "wait for all then
      test", tmux status-line integration via `ptyro ls --json`). **S**
      *Done:* `docs/scripting.md` covers one task start to finish, an overnight batch, wait-for-all-then-test, turn-by-turn `send --wait`, reacting to `events --follow`, a tmux status line and waiting for a dev server. It also has the AGENTS.md/skill snippet that teaches agents the commands (R-ORC-18).
- [x] **M5.10** Read other tools' committed config (R-WT-03, from `RESEARCH.md`).
      - Import as setup/run sources: `.claude/launch.json` (dev servers, `autoPort`),
        `.conductor/settings.toml`, `.superset/config.json`, `.codex/` environments,
        `copilot-setup-steps.yml`, `.cursor/environment.json`.
      - All of it shows in, and is fingerprinted by, the M3.9 trust dialog.
      - `project config --explain` shows where each value came from. **M**
      *Done:* Six readers in `ptyro_core::imports`, one each for Conductor (`.conductor/settings.toml`, `conductor.json`), Superset, Codex environments, Claude's `launch.json` (ports from the worktree's block, `--port` rewritten), Cursor's `environment.json` and Copilot's setup steps (via yaml-rust2).
        - They fill only what `.ptyro.toml` leaves unset.
        - The trust dialog names each script's source file, and the scripts are fingerprinted like ptyro's own.
        - Scripts get the tool's variables (`CONDUCTOR_*`, `SUPERSET_*`, `CODEX_*`).
        - `project config --explain|--json` shows where every value came from; `[project] import = false` turns it off.

        Formats are in SPEC §20.7.

*Status 2026-10-20:* every task is done. `crates/ptyro/tests/m5.rs` (9 tests):
- the Done-when as a real `sh` script, with `events --follow` watching;
- `send`, `wait` (including `--match`, `--any` and `--stall`), `read`, `events`, `broadcast` and `stop`;
- exit codes;
- the `--json` shape snapshot;
- live completions;
- `doctor` with its self-test, and `install-service`;
- `init` against a fake home;
- other tools' config, from trust to the imported setup running with `CONDUCTOR_*`;
- the TUI's broadcast and tour.

Golden rc-file tests and importer unit tests live in their crates.

**Left for the owner:**
- the launchd service on macOS;
- `init` on your real machine (it found no multi-account setup here);
- the imported formats against real projects that use those tools.

**Done when:** a shell script can create a worktree, start Claude with a prompt, wait for it to go
idle, read its final screen, run tests and remove the worktree — using only `ptyro` commands — and
the TUI shows all of it live.

---

## M6 — Review & ship

Goal: close the loop from "agent finished" to "merged", without leaving ptyro.

- [x] **M6.1** Inspector pane framework (`D`), tabs: Diff, Commits, Checks, Ports, Events. **M**
      *Done:* `D` (after the prefix, or in the sidebar) opens the inspector over the panes for the selected or focused worktree, with five tabs (`1`–`5`/`Tab`) and the PR's check summary in its title. `Q` opens the review queue. Sidebar worktree rows show `#7 ✓` / `✗2` / `◌` / `merged`.
- [x] **M6.2** Diff view: file list, unified/side-by-side hunks, syntax highlighting (optional
      `syntect`), against base / upstream / last turn. **L**
      *Done:* the daemon diffs the working tree with `-M` against the base (merge base), the upstream, the last turn (a `git stash create` snapshot at turn start, which touches nothing) or HEAD; untracked files are included and the diff is capped at 20k lines. The view has a file list, unified hunks with old/new numbers, and side by side (`s`). `wt diff --against/--stat/--json` gives the same. **Not done:** syntax highlighting, which was optional; syntect was left out for binary size.
- [x] **M6.3** Line comments on the diff → batched into a prompt sent to the session's agent
      (annotate-the-diff review). **M**
      *Done:* `c` comments on the cursor line (the line's text is kept) and `S` sends all comments to the agent as one numbered `file:line` prompt, delivered once its turn is over. The comments live in the daemon (`review add/ls/rm/send`), so a TUI restart keeps them.
- [x] **M6.4** Commit helper (stage all / pick files, message editor, or "ask the agent to commit"). **M**
      *Done:* `Space` marks files and `C` commits them (or everything) with a message, as the worktree's profile identity. `E` commits in your `$EDITOR` inside ptyro (`git commit --verbose` in a session), and `A` asks the agent to commit. From the CLI: `wt commit -m/--all/-- FILES/--ask-agent` and `wt log`.
- [x] **M6.5** `gh` integration: create (draft) PR, show PR number + check status per worktree in
      sidebar, "fix failing checks" sends failing logs to the agent. **L**
      *Done:* `pr create` pushes under the profile and opens a draft PR (`--ready`); the body is the commit list plus a worktree/session marker (R-GIT-03). The poller (`github.poll`, under the profile's gh account) gives the sidebar its PR number and checks. `pr status/checks` are there too, and `pr fix` (inspector `f`) sends the failing checks' log tails to the agent. Tested against `fake-gh`; gh isn't installed here.
- [x] **M6.6** Create worktree from PR (`--pr 123`) or issue URL (branch name derived). **M**
      *Done:* `wt new --pr N|URL`, `new --pr N|URL` (R-WT-02) and `--issue N|URL`. An issue gives the worktree `issue-N`, the branch `issue-N-<slug>`, and the agent a first prompt from the issue. Also `pr open N|URL`, which finds the worktree from a PR and attaches to its agent.
- [x] **M6.7** Integrate / checkpoint: rebase onto base and fast-forward into a target branch, or
      cherry-pick; "pair mode" checks out the agent branch in the main checkout for IDE/dev-server
      use and restores afterwards (with stash safety). **L**
      *Done:* `wt integrate W [--into B] [--cherry-pick] [--push]`: it refuses while dirty or while an agent is working, rebases onto the target (aborts on conflict), then fast-forwards the target, with `merge --ff-only` where it's checked out or by moving the branch forward. `wt pair W` / `wt unpair W`:
        - **pair:** stash the main checkout's own work, check out the worktree's commit detached, then apply its uncommitted changes and copy its new files;
        - **while paired:** edits made in the main checkout are carried back to the worktree on `--refresh`/`unpair`;
        - **unpair:** restore the original branch and apply and drop exactly that stash.

        Nothing changes if a patch doesn't apply. ("Checkpoint" is read as integrating progress into the target.)
- [x] **M6.8** Events tab: per-session status timeline from `session_event`. **S**
      *Done:* the inspector's Events tab: the worktree's sessions' persisted events as one timeline, with status transitions coloured.
- [x] **M6.9** Review queue (R-ORC-24, from `RESEARCH.md`).
      - One ranked "ready for review" lane across sessions, using verified-done (M7.9), check results,
        diff size, touched files, time/cost and the last message.
      - The evidence bundle opens in the inspector.
      - A `max_unreviewed` policy queues new spawns and fan-outs past the limit.
      - Digest notifications. **M**
      *Done:* `review queue` (TUI `Q`) ranks worktrees whose agent finished a turn you haven't reviewed. The score weighs checks, diff size, files, errors and time waiting; each item has reasons and the agent's last line. The evidence bundle is the inspector. `review done` (or sending comments) counts as reviewed. `review.max_unreviewed` holds new agent spawns in a queue (`Queued` reply; `--unqueued` skips it) and starts them as room appears. A digest notification goes out at most every `review.digest`. Since M7, verified-done counts too: +25 "verified", −25 "claims done; its check fails".
- [x] **M6.10** PR-event loop (R-GIT-01, R-ORC-08). Builds on M6.5.
      - `ptyro watch-pr`, automatic for PRs ptyro opened, polls `gh pr checks` and review threads.
      - New CI failures (with a log tail) and review comments go to the owning session. Delivery is
        only when the agent is idle, and tagged with where it came from.
      - Replies on review threads are marked "via ptyro". Ambiguous comments raise attention instead.
      - `ptyro pr automerge` wraps `gh pr merge --auto`.
      - Escalate to the human after a configurable time. **L**
      *Done:* the poller runs the PR-event loop for watched PRs (ptyro's own, or `pr watch`/`watch-pr`):
        - **baseline:** the first poll is a baseline, so only news is sent;
        - **news:** newly failing checks (with a log tail) and new line comments go to the agent once its turn is over, tagged `[via ptyro: …]`;
        - **questions** raise attention instead;
        - **replies:** `pr reply` ends with "— via ptyro";
        - **merging:** `pr automerge` wraps `gh pr merge --auto`;
        - **escalation** after `github.escalate_after`.

        State persists in the store.

*Status 2026-10-20:* every task is done. Syntax highlighting (optional in M6.2) is left out.
`crates/ptyro/tests/m6.rs` (7 tests):
- the Done-when in the TUI: review in the inspector, two line comments become one prompt, the
  "fix" is committed, a draft PR opens, and its checks go pending → passing in the sidebar;
- diffs (base, head, last turn), commits as the profile identity, integrate and cherry-pick;
- pair mode round-tripping the user's stash and edits;
- PRs and the PR-event loop through `fake-gh`: baseline, failing CI with a log, a line comment
  delivered, a question that raises attention, reply, auto-merge, `pr open`;
- worktrees from issues;
- the review queue: ranking, the `max_unreviewed` hold, the digest;
- the inspector's tabs.

**Left for the owner:**
- install `gh` and check the PR features against a real repository (they follow gh's docs and
  ran only against `fake-gh`);
- pair mode in your IDE with a dev server running.

**Done when:** you review an agent's changes in the inspector, leave two line comments that become
a follow-up prompt, the agent fixes them, you open a draft PR and watch its checks in the sidebar.

---

## M7 — Orchestration

Goal: run many agents on one problem deliberately, and let a lead agent coordinate workers safely.

- [x] **M7.1** Run model + persistence (`run`, `run_message`), `ptyro run ls|show|cancel`.
      Event-sourced (R-ORC-33):
      - An append-only `run_event` table, with projections updated in the same transaction.
      - The recipe source and sha are pinned per run.
      - Attempts record `start_ref`/`end_ref` snapshot refs and the agent's native session id.
      - `on_restart = native_resume|fresh|hold`; `failed` is kept distinct from `crashed`.
      - `ptyro run fork --from <step>` with lineage (`run ls --tree`).
      - Design notes: `RESEARCH.md` §II.3. **L**
      *Done:* runs (`r-7f3a2c`) are event-sourced: an append-only `run_event` log plus the `run` projection, written in one transaction; `RunState::replay` rebuilds the projection from the log. The recipe source and sha are pinned per run. Attempts record the session, the agent's conversation id, the worktree and `start_ref`/`end_ref` (`refs/ptyro/runs/…`, checkpoint commits). `on_restart = native_resume|fresh|hold`; `failed` and `crashed` are distinct. CLI: `run ls [--active] [--tree]`, `show`, `events`, `wait [--gate]`, `cancel`, `retry`, `fork --from STEP [--set]`; forks record lineage. The mail ledger is `run_message` (M7.7). SPEC §15.6.1.
- [x] **M7.2** `ptyro fanout` (§15.2): N worktrees from one base, prompt delivery on ready, wait,
      optional test script per worktree. **M**
      *Done:* `ptyro fanout --agents a,b,c | -n N` builds a recipe with one candidate per agent in `<prefix>-<agent>` worktrees from one base. The prompt goes in once each agent is ready, `--test` ranks them, and `--verify`, `--judge`, `--timeout` and `--wait` are there too. The TUI starts one with `F` in the runs view. SPEC §15.6.4.
- [x] **M7.3** Compare view: side-by-side diffstat/tests/time/last message; pick (merge/cherry-pick/
      PR), discard (safe removal), iterate (follow-up to one/all). **L**
      *Done:* `ptyro compare` and the TUI's compare view (`O` → `c`) show, per candidate: diffstat, tests, time, last message and reasons. **Pick** merges (rebase + fast-forward), cherry-picks, opens a PR, or applies chosen files or hunks; uncommitted work is committed as the profile first. **Discard** goes through the safe removal (`--force` for work that would be lost). **Iterate** sends a follow-up to one or all.
- [x] **M7.4** Recipes (§15.3): TOML schema, validation, DAG executor with `needs`, `wait`, `run`,
      `gate`, timeouts, per-step retry, survival across daemon restart; repo recipes require trust. **L**
      *Done:* the recipe schema v1 (SPEC §15.6.2):
        - step kinds `spawn`, `send`, `run`, `gate`, `apply`, `sleep`, `verify`, plus `loop`;
        - `needs` with `all|any|first_success|all_settled` joins and `cancel_siblings`;
        - `if` conditions (a small non-Turing-complete language) and `{{templates}}`;
        - typed outputs; `timeout {total,start,stall}`; `retry {max,on,not_on,mode,feedback}`; `budget {wall,sessions,attempts}`; `guard {deny_paths,deny_cmds}`.

        `recipe validate` reports every mistake with where. The pure engine (`plan`) is unit-tested. Runs survive a daemon restart, and repo recipes (`.ptyro/recipes/`) need trust. Also `recipe graph` (Mermaid or dot). **Not done:** `guard.max_diff_lines`/`on_trip` are parsed but not enforced; `spawn.mode = "headless"`; the v2 primitives (`foreach`, `expand`, `wait_event`, triggers…).
- [x] **M7.5** Gates: resolve from TUI (inbox entry), CLI, later phone. **S**
      *Done:* gates have stable short ids (`g-7f3a`) and absolute deadlines that survive restarts. They are answered from the CLI (`gate ls/resolve`, idempotent per key), the TUI runs view (`1`–`9`, a note with `n`) and the inbox (opens the run), with notifications and toasts. `on_timeout` is reject, approve, escalate or an option. Agents can never answer one. **Not done:** phone links.
- [x] **M7.6** `ptyro mcp` server (§15.4) with policy limits enforced by the daemon; injection into
      a "lead" session via `--mcp-config`. **L**
      *Done:* `ptyro mcp`, a stdio JSON-RPC MCP server with 16 tools, progress notifications and idempotency keys, injected into `--lead` sessions with `--mcp-config` (Codex: `-c mcp_servers.*`). The daemon enforces the policy (`max_sessions`, `max_spawns`, `agents`, `worktrees`, `destructive`) on every op, and identifies the caller by its process ancestry. Agents shelling out to `ptyro` therefore get the same limits, and no agent answers gates or permission prompts. SPEC §15.6.8.
- [x] **M7.7** Mailbox messaging (typed messages, acks, group addresses `@idle` `@codex` `@all`). **M**
      *Done:* typed messages and group addresses (`@lead @user @all @idle @<agent> @run:<id>`), which expand when sent. The daemon observes the acks itself: queued, delivered (typed into an idle agent, coalesced) and seen (the prompt hook carried the id). Messages are framed as not-from-the-user. Loop safety: ≤50 queued, repeats dropped, ≤4 hops, TTLs. Interrupt mail to a working Claude uses its messaging socket (unverified). Workers' turn ends reach their lead as `worker_done` messages. CLI: `msg send/check/ls/ack`.
- [x] **M7.8** Built-in recipes: `implement-and-review`, `best-of-n`, `test-fixer`, `doc-writer`. **S**
      *Done:* `implement-and-review`, `best-of-n`, `test-fixer`, `doc-writer`, plus `overnight` (M7.12). Each is valid by test, and `implement-and-review` runs end to end with fake agents (the writer, a reviewer from another vendor asking for changes, the follow-up, the gate, integrate).

*Added 2026-10-20 from `RESEARCH.md` Part II (the orchestration deep dive):*

- [x] **M7.9** Verified-done (R-ORC-23).
      - Status splits into `done(claimed)` and `done(verified)`.
      - A `check` is defined per project or per run. The evidence is stored: command, exit code,
        output tail, tree hash.
      - Recipe `done_when = { run, max_bounces }`. For Claude, the Stop hook returns `decision:block`
        with the failures as the reason. For other agents, ptyro sends a follow-up prompt.
      - Needs M4.4. **M**
      *Done:* `new --verify`, `verify S CHECK`, `done_when`, `fanout --verify` and `.ptyro.toml [verify] auto` set a check on a session. Claude is bounced from its Stop hook (`decision:block`), so the turn never ends with failing tests; other agents get a follow-up prompt. Evidence (`command, exit_code, tail, tree`) is kept. The states are pending, checking, passed (done, verified), bounced and failed (done, claimed), shown as sidebar badges `✔ ◌ ↩N ✘`.
- [x] **M7.10** Evidence-ranked compare and a `verify` step (R-ORC-04), extending M7.3.
      - Signals: newly broken tests; a repro test that fails on base and passes after; a cross-test
        matrix; Tree-sitter equivalence clusters; a divergence probe; diff spread; cost and time.
      - An advisory judge: a different agent with a clean context, returning rubric JSON.
      - Lexicographic ranking that shows its reasons, and a "differences between top candidates" view.
      - Pick by hunk.
      - Defaults: N=3, warn above 6, mixed agents. **L**
      *Done:* evidence is gathered per candidate and cached on the run:
        - tests, against the base in a throwaway worktree (so "broke tests" means something);
        - a repro that must fail on the base;
        - a cross-test matrix (one candidate's test files on another's tree);
        - clusters of identical changes, comments and whitespace aside;
        - diff spread, time and verified-done;
        - an advisory judge (`--judge`; a clean directory of diffs, scores 0–10).

        The ranking is lexicographic and shows its reasons. The view warns above 6 candidates and when every candidate is the same agent, and shows the diff between the top two. Pick by hunk with `pick --hunk F:N` and `--path`. **Not done:** Tree-sitter equivalence (the normalized-diff clusters stand in), the divergence probe, and cost as a signal (no trustworthy per-agent cost yet).
- [x] **M7.11** Checkpoints for every session (R-ORC-03).
      - At each turn end and loop iteration, write `refs/ptyro/ckpt/<session>/<n>`: HEAD plus a
        snapshot of uncommitted work, with no user-visible commits.
      - `ptyro checkpoint ls|diff|restore` and `restore --turn N`.
      - Shares code with M11.8. **M**
      *Done:* every agent turn that changed its worktree leaves `refs/ptyro/ckpt/<session>/<n>`: HEAD plus all uncommitted work, written through a private index, so no visible commit and the user's index untouched. 200 are kept per session. `checkpoint ls/diff/restore [--turn N]`; a restore checkpoints the present first and refuses while an agent works. `[orchestration] checkpoints` turns them off.
- [x] **M7.12** `loop` recipe step (R-ORC-25).
      - A fresh session per iteration, with a checkpoint per iteration (M7.11).
      - Stops when the check passes, after N iterations without progress, or when the budget runs out.
      - A built-in `overnight` recipe shows a morning digest on attach. **M**
      *Done:* `loop = { until, max, no_progress }`: a fresh session per iteration in one worktree, with a checkpoint each time. It stops when the check passes, after `no_progress` unchanged iterations, at `max`, or when the budget runs out. The built-in `overnight` recipe ends in a morning gate, and the TUI shows a digest on attach (run updates and waiting gates).
- [x] **M7.13** Collision radar and file leases (R-ORC-11).
      - A live file-overlap matrix across worktrees, plus `git merge-tree --write-tree` predicted
        conflicts. Both agents get a message with the overlapping hunks.
      - Daemon-held leases (`path|glob`, exclusive or shared, ttl, holder), enforced through
        PreToolUse/BeforeTool denials.
      - Generalises the M11.4 fences. **M**
      *Done:* `ptyro radar` lists each live worktree's changes (uncommitted included), the overlapping pairs and the files `git merge-tree --write-tree` predicts would conflict. After each turn, new overlaps message both agents once, with the other side's hunks; fan-out siblings are skipped. Daemon-held leases (`lease claim/release/ls`; path or glob, exclusive or shared, ttl, holder, reason) are enforced through PreToolUse/BeforeTool denials that name the holder and how to ask them. Guards' `deny_paths`/`deny_cmds` use the same path.

**Done when:** `ptyro fanout --agents claude,codex,gemini --prompt-file task.md` produces three
worktrees, you compare them in one view and merge the best; and a lead Claude session using
`ptyro mcp` can spawn two worker sessions, wait for them, and report back — without being able to
exceed its policy limits.


*Status 2026-10-20:* every task is done, with the gaps named above: guard diff limits, headless
spawns and the v2 primitives (M7.4); phone links for gates (M7.5); Tree-sitter equivalence, the
divergence probe and cost (M7.10). `crates/ptyro/tests/m7.rs` (8 tests, every agent a fake):
- the Done-when, part one: a claude/codex/gemini fan-out ranked by evidence (only one passes the
  tests; the base doesn't); the best is merged, the rest discarded only with `--force`, then a
  fork re-ranks;
- the Done-when, part two: a lead Claude using `ptyro mcp` through `--mcp-config` starts two
  workers, waits for them and reports back. A third worker, touching a non-worker, shelling out
  to `ptyro new` and a worker answering a gate are all refused;
- a recipe with a Stop-hook bounce, a check, a skipped step, a gate and `apply`
  (commit + integrate); a guard denies `git push`; checkpoints exist;
- a run surviving a daemon restart mid-gate, and a rejected gate retried;
- leases denying an edit, the radar predicting a conflict and telling both agents, mail
  delivered and seen, a checkpoint restore, and a follow-up bounce for Codex;
- the built-in `implement-and-review` with a Codex reviewer asking for changes;
- a judged fan-out;
- the M7 `--json` shapes (`snapshots/m7__m7-cli-json.snap`).

`make orchestrate-demo` runs a fan-out, a gated recipe and a lead with fake agents. **Needs you:**
real agents. Watch Claude's Stop-hook bounce and the PreToolUse denial on 2.1.x, run Codex with
`-c mcp_servers` as a lead, and try the messaging socket.

---

## M8 — Hardening

Goal: trust it with a week of uninterrupted work.

- [x] **M8.1** Live upgrade via re-exec handoff (§4.5): handoff image format (versioned),
      fd-inheritance, emulator state serialization, client auto-reconnect, rollback path. **L**
      *Done:* `ptyro daemon upgrade [--binary P]`. The daemon parks every PTY reader, flushes,
      writes a versioned image (sessions, masters, status with engine clocks, run identity,
      sidecars, counters) to a memfd, and re-executes itself in place. Each emulator is saved
      whole: both grids, scrollback, cursors, modes, the kitty stack and the title, as compact
      msgpack + zstd, encoded in parallel (10 × 12k lines: 0.25 s + 0.25 s). After the exec, a
      `waitpid` reaper collects children tokio didn't spawn, and a SIGWINCH nudge makes programs
      repaint. A refusing binary rolls back to `state/daemon.prev`. The TUI, `wait`,
      `send --wait` (never typing twice), `run wait` and `events --follow` reconnect; runs carry
      on. Two bugs the soak found are fixed: a request on a dead connection hung forever, and a
      hook's `blocked` was lost to heuristics after the upgrade.
- [x] **M8.2** (R-OBS-03) Recorder (§7.6): ring-buffered `pty.log`, scrollback for exited/dormant sessions
      after restart, `ptyro replay`, `ptyro export --asciicast|--text`. **M**
      *Done:* a framed format with output, resize, keyframe and start records, in a two-file
      ring (`[recorder] max_bytes`, 16 MiB; `0600`). Dormant agents show their last screen after
      a restart. `read --scrollback N` and copy mode's `gg` reach past memory. `replay
      [--speed --max-idle --from]` and `export --format asciicast|text|ptylog`. Recording can be
      turned off globally or per agent (`record = false`). The in-memory default dropped to 5000
      lines.
- [x] **M8.3** Performance pass against SPEC §18.3 budgets; criterion benches (parse throughput,
      frame build, paint); memory profiling of scrollback. **L** Baseline from M1 is in §18.3
      (`make bench`). Open items: parse is at 140–200 MB/s, below the 200 MB/s target, and the
      limit is alacritty itself; a full 200-column 10k-line scrollback costs ≈ 48 MB, so
      consider a compact or compressed history, or a lower default.
      *Done:* `make criterion` (parse, frames, scrollback, handoff image, TUI paint) and `make
      latency` (`ptyro-latency`: key → echo → frame through a real daemon: p50 0.69 ms, p99
      1.0 ms). Paint takes 0.23 ms, and a 200×60 keyframe is 58 KB built in 0.4 ms. Memory: a
      full 10k-line 80-column session is ≈ 19.5 MB, over budget, so the default is now 5000
      lines with the recorder behind it (§7.6). **Still open:** parse throughput is alacritty's
      (no change).
- [x] **M8.4** Fuzzing: side-channel sniffer, protocol decoder, input encoder (`cargo fuzz`, CI
      smoke run). **M**
      *Done:* six targets (sniff, emulator with resizes and image round trips, protocol with
      zstd, input's paste safety, recipes, recordings), shared by `ptyro-fuzz` (stable, CI 20 s
      each, `make fuzz-smoke`) and a cargo-fuzz crate (`make fuzz`, nightly). First long run:
      ≈ 1.05 billion runs, no crash.
- [x] **M8.5** Session actor panic isolation + crash reports (`state/crashes/`), daemon watchdog
      metrics in `daemon status`. **M**
      *Done:* a panic hook writes a report for every panic (newest 50 kept). An emulator that
      panics on some output is replaced by a fresh one; the child keeps running, and you're
      notified, with the bytes in the report. `daemon status` shows RSS, fds, tasks, PTY
      readers, event-loop lag (max and p99 over a minute), bytes, frames, crashes, per-session
      memory and upgrades; the counters carry across upgrades.
- [x] **M8.6** Compatibility sweep: `vttest` sections, full `docs/compat.md` matrix across 6 outer
      terminals + inside tmux. Fix or document every gap. **L**
      *Done (headless half):* `make compat` checks vim, less, htop, top, tmux, zsh, bash, the
      python REPL, man, git's pager and ten vttest screens (cursor, autowrap, VT102 insert and
      delete, DSR, DA1/DA2). All pass. It found and fixed tab cells painting a raw tab into the
      outer terminal. Gaps are documented: DECCOLM, DSR 6 under DECOM, DA3 and DECREQTPARM, all
      alacritty's. **Needs a human:** the outer-terminal grid (Ghostty, kitty, WezTerm,
      Alacritty, iTerm2, Terminal.app, inside tmux).
- [x] **M8.7** Optional `zstd` frame compression (feature-negotiated). **S**
      *Done:* the `"zstd"` feature in Hello/Welcome (`PTYRO_COMPRESS=1`). Frames of 512 bytes or
      more go as zstd frames, which the decoder tells apart by magic, capped at the frame limit.
      Off by default locally; it's for M9.
- [x] **M8.8** Release engineering: tagged builds for linux-x86_64/aarch64, macos-aarch64/x86_64;
      `cargo install`; Homebrew tap; checksums; changelog automation. **M**
      *Done:* `make dist` (a tarball with completions, README, LICENSE and CHANGELOG, plus
      `.sha256`; refuses a tag that doesn't match the version), `make changelog`
      (`CHANGELOG.md` from tags and commit prefixes, milestone bullets kept), `make
      homebrew-formula`, and `release.yml` on `v*` tags (four platforms, `SHA256SUMS`, notes,
      `gh release`, a tap push with `HOMEBREW_TAP_TOKEN`). `cargo install --git` is in the
      README. **Needs you:** create `nealhardesty/homebrew-ptyro` and the token, then push the
      first tag.

**Done when:** a daemon upgrade during active agent work is invisible to the agents, and a 7-day
soak test (fake agents generating output + real daily use) shows no leaks, no stuck sessions, and
budgets met.

*Status 2026-10-21:* every task is done, except the outer-terminal half of M8.6, which needs a
person at each terminal. `crates/ptyro/tests/m8.rs` (7 tests):
- **The Done-when, part one.** An upgrade in the middle of a fake Claude's turn keeps the pid,
  every session id and screen, and the daemon's identity. The turn's Stop hook lands on the new
  daemon. A `ptyro wait` blocked across it returns `idle`. A ticker never stops. A child exiting
  afterwards is reaped with its code. A second upgrade works too.
- **Rollback.** A refused handoff rolls back, with sessions intact and exit 1.
- **send across an upgrade.** A `send --wait-ready --wait` waiting inside the daemon (the agent
  blocked on a question) is typed exactly once afterwards, and the hook's `blocked` survives.
- **The recorder.** Deep scrollback, text and asciicast export, replay, `0600`/`0700`, and a
  dormant agent's screen after a restart. Recording can be turned off.
- **zstd and health.** A zstd client, and the health metrics.
- **Dead connections.** A request on a dead connection fails at once.

`make soak` passed a 4-minute run: 5 upgrades, 424 turns, flat RSS, fds and tasks, lag p99 4 ms.
Stress-running the suites in parallel found and fixed three older races:
- a fast agent's first hook arriving before its session was registered (signal lost);
- a run's end re-marking a just-succeeded step as cancelled;
- recorder segments starting within the same millisecond going back in time.
**Needs you:**
- the 7-day soak (`make soak DURATION=7d`) and real daily use, which is the second half of the
  Done-when;
- the outer-terminal grid;
- the first tagged release;
- a macOS run of `make check` and an upgrade (CI covers the build and tests).

---

## M9 — Remote

Goal: check on and steer your agents from another machine or your phone, safely.

- [x] **M9.1** (R-REM-01) `ptyro proxy` + `ptyro --remote ssh://host` / `attach --remote` (local TUI, remote
      daemon over SSH stdio). **M**
      *Done:* `--remote [ssh://][user@]host[:port]` (or `PTYRO_REMOTE`) for every command and
      the TUI. ssh runs with BatchMode, finds ptyro in the usual install dirs, and is
      replaceable (`PTYRO_SSH`). Frames are zstd-compressed, the Hello says `via ssh://host`,
      and failures quote ssh's stderr. `ptyro proxy` bridges stdio to the daemon socket. A
      failed remote `ls` is an error, not "no sessions".
- [x] **M9.2** (R-REM-02, R-SEC-02) WebSocket transport (`ptyro serve`), device tokens (`ptyro pair` with terminal QR
      code, `ptyro devices ls|revoke`), hashed storage, audit events for remote input. **L**
      *Done:*
      - `ptyro serve` is a separate process (hyper + tungstenite) with a strict CSP;
        `--tailscale` puts Tailscale HTTPS in front; loopback/Tailscale only unless
        `--insecure`; the daemon can supervise it (`[remote] serve = true`), through upgrades.
      - The browser's Hello is rewritten (kind `web`, `via`); 10 failed logins lock an address
        out.
      - Tokens: 256 bits, SHA-256 stored, constant-time checks. Scopes read < approve <
        control < full are enforced on every request and keystroke; devices never
        resize/control.
      - Revocation closes live connections. Pairing is local only, by QR code (`#pair=`
        fragment). `remote` events record each action on a session.
      - `POST /api/request` serves notification actions.
- [x] **M9.3** (R-REM-03) Web client/PWA (TypeScript): session list + status, inbox, canvas terminal view from
      `ScreenFrame`s, quick reply, mobile layout, install as PWA. **L**
      *Done:* `web/src` (TS, no dependencies) → `web/dist` (committed, embedded;
      `make web`/`web-check`).
      - Session list (blocked first, Allow/Deny on the card), inbox, live canvas at the
        session's size (fit/1:1), reply box, key bar, permission card.
      - A chat view for structured sessions (M10).
      - Manifest, icons, a service worker (offline shell, push, notification actions).
      - Its screen model is checked against `ptyro read` on live frames.
- [x] **M9.4** Web Push notifications for attention events. **M**
      *Done:* VAPID (ES256) plus RFC 8291 encryption, matching the RFC's example byte for
      byte. Delivery goes through curl and follows the notifier's filters; 404/410 drop the
      subscription. `ptyro devices push-test`. An e2e test decrypts delivered pushes like a
      browser.
- [x] **M9.5** Remote approvals (§16.6) for Claude `PermissionRequest` (bounded wait; local key
      always wins). **M**
      *Done:* first verified on Claude 2.1.305 that the dialog shows while the hook runs, that
      a hook decision closes it, and that a local answer wins. With a device paired, the hook is
      held up to `[remote] approval_wait`. Any client's allow/deny lands as an exact decision;
      answering in the dialog releases it. `SessionInfo.held` shows "exact".
- [x] **M9.6** `docs/remote.md`: Tailscale recipe, reverse-proxy TLS recipe, threat model. **S**

**Done when:** from a phone on Tailscale you see that an agent is blocked on a permission prompt,
read the context, approve it, and watch it continue — and no unauthenticated request can reach the
daemon.

*Status 2026-10-21:* every task is done. `crates/ptyro/tests/m9.rs` (6 tests), with the "phone" a
WebSocket client speaking the web app's JSON:
- **The Done-when.** A blocked fake Claude, held for an exact answer, its context and dialog
  read through `serve`, approved, and continuing. Hellos with no token, a wrong token, or
  posing as a TUI are rejected; `/api/request` without a token is 401.
- **Scopes:** read can't type; control can't spawn commands; nobody remote pairs.
- **Revocation:** a goodbye, then rejection. **Lockout:** 429 after 10 failures.
- **`--remote`** through a stand-in ssh, and ssh's error relayed.
- **Web Push:** received, VAPID-signed, decrypted; 410 drops the subscription.
- **The managed `serve`** coming back after an upgrade.
- **The web app's TypeScript screen model** matching `ptyro read` (with Node).

**Needs you:** a real phone over Tailscale (install the app, notifications on iOS/Android), a
reverse-proxy setup, and a real Claude prompt answered from the phone.

---

## M10 — Structured sessions

Goal: agents driven over structured protocols, for orchestration-heavy and mobile-first use.

- [x] **M10.1** `structured` session kind and chat-view widget (messages, tool calls, diffs,
      permission requests as interactive cards). **L**
      *Done:* `new|fanout --structured`, recipe `spawn.structured`.
      - The transcript model is `ptyro_core::chat`; drivers are in `ptyro-drive`; the daemon
        glue is `structured.rs`, with status from the protocol, the transcript also rendered
        into the emulator, and a line editor for keys.
      - `ServerMsg::Chat` streams, and `read --chat` dumps the transcript.
      - TUI chat view: cards, diffs, permission card with a/A/d, composer, PageUp/Down, `C-q
        t` for the terminal view. The web app has one too.
      - Live upgrades carry it mid-turn.
- [x] **M10.2** ACP client (stdio JSON-RPC) — Gemini `--acp`, OpenCode, and other ACP agents. **L**
      *Done:* initialize/session/new/load, prompt, updates (messages, thoughts, tool calls with
      content and diffs, plans), request_permission by option kind, cancel. Any agent with
      `protocol = "acp"`. Gemini 0.64 handshake verified live.
- [x] **M10.3** Claude `-p --input-format stream-json --output-format stream-json` driver. **M**
      *Done:* verified on 2.1.305 with a real turn (fixture): partial text, tool_use/result
      with Edit diffs, `can_use_tool` over stdio (`--permission-prompt-tool stdio`), interrupt,
      exact cost per turn.
- [x] **M10.4** Codex `app-server` driver (JSON-RPC, approval requests). **M**
      *Done:* written against the generated 0.143 schema; handshake/thread/start verified live;
      commands, file changes with diffs, plans, approvals (accept/acceptForSession/decline),
      interrupt; a logged-out error shown once, as `auth`.
- [x] **M10.5** OpenCode `serve` HTTP driver. **M**
      *Done:* from the docs (not installed here): sessions, prompt_async, SSE parts, permissions,
      abort, cost. Tested against the fake's server.
- [x] **M10.6** Convert a structured session to interactive (resume the same conversation in the
      agent's TUI) and back where supported. **M**
      *Done:* `ptyro convert S [--to tui|structured] [--force]`: needs the conversation id,
      refuses mid-turn without `--force`, hibernates and resumes in the other mode. The mode
      persists through resume, restart and dormant restore.

**Done when:** fan-out workers can run as structured sessions with full tool-call visibility in the
compare view, and any of them can be "opened" into the native TUI to take over by hand.

*Status 2026-10-21:* every task is done. `crates/ptyro/tests/m10.rs` (5 tests, fakes for all four
protocols):
- **The Done-when.** A structured claude/codex/gemini fan-out; compare shows each candidate's
  tool calls and commands, and Claude's cost; the best is ranked by its test. A candidate is
  converted to its TUI (same conversation), worked by hand, and converted back.
- **Every protocol:** a turn plus an allowed and a denied permission.
- **The chat stream:** streaming text, the draft, keys answering a card, a late subscriber's
  reset.
- **A live upgrade mid-turn** (same pid, transcript whole).
- **Hibernate/resume** staying structured, an agent failing at start with its stderr shown, and
  a read-scope device reading the transcript.

`make verify-structured` passes against the real Claude (a turn, cost, takeover), Codex
(thread) and Gemini (ACP session). **Not yet:** leases and guards for structured sessions (they
ride on hooks); a structured transcript surviving a full daemon restart (not an upgrade).
**Needs you:** OpenCode against a real install, and a real Gemini/Codex turn when logged in.

---

## M11 — Change sets (multi-repo)

Goal: one coordinated change across several repositories, with a lead agent, one agent per repo,
a contract at the seams, the ability to build against each other's in-progress code, multi-repo
undo, and a ship train that's honest about partial states (SPEC §15.5).

- [x] **M11.0 (experiment)** Run the workflow by hand on two or three real repos of the owner's:
      linked worktrees on one branch, a lead Claude with `--add-dir` per member, member agents
      prompted by the lead through `ptyro send`/`wait`/`read`, a shared `CONTRACT.md`. Record what
      the lead needed, where it went wrong, which messages mattered and how long it took, in SPEC
      §15.5 and §20. Decides the default layout and the message types. **M**
      *Done 2026-10-21:* three toy repos (Go api + Go sdk with `replace ../api` + Node web),
      Sonnet lead, Haiku members, 7 min 47 s, all green, nothing pushed (SPEC §15.7.1). It
      decided: members live side by side in the root (relative-path deps then build against
      siblings); `affects` falls back to section titles; the lead must not commit for members;
      clean member exits aren't failures. It found a bug, fixed: `ptyro answer` couldn't pass
      Claude's folder-trust dialog (§20.1).
- [x] **M11.1** Model: Run kind `changeset`, `changeset_member` table, `CHANGESET.md` generation,
      creation from projects or a template, same-profile check, `depends_on` cycle check, manifest-based
      dependency suggestions. **M**
      *Done:* `ptyro-run/src/changeset.rs` (pure) + `ptyro-daemon/src/changesets.rs`. Members
      live in the run's event-sourced projection instead of a table (one source of truth).
      Manifests read: go.mod, package.json, Cargo.toml, pyproject.toml.
- [x] **M11.2** Linked worktrees: create/remove all members atomically from the user's view
      (rollback on partial failure), per-member base branches, all-or-nothing removal safety. **M**
      *Done:* `<root>/<member>` (`WorktreeSpec.path`), `--base m=ref`; `cs rm` checks every
      member first (`--keep`, `--force`).
- [x] **M11.3** Layouts `lead` / `solo` / `manual`; spawning a lead (cwd = root, read access to
      members) and per-member agents (each project's `default_agent`) with their briefs. **M**
      *Done:* the lead gets `--add-dir` per member and `ptyro mcp` with the set's tools; members
      start when r1 is approved, with the contract in their brief and `$PTYRO_CS`.
- [x] **M11.4** Write fences: `PreToolUse` denials for edits outside a role's allowed paths
      (Claude, Codex), advisory flagging elsewhere, denial reasons fed back to the agent. **M**
      *Done:* verified on the fake Claude and Codex; advisory: member changes made during the
      lead's turn are flagged `out_of_fence`.
- [x] **M11.5** Contract: versioned `CONTRACT.md` revisions in the ledger, the contract gate,
      `contract_revised` propagation by `affects:`, and `cs contract show|edit|diff|history`. **M**
      *Done:* plus `submit`, `revise` (text or unified diff), `approve`/`reject`; affected
      members get an interrupt, the rest a normal message; `diff FILE` exits 1 on drift.
- [x] **M11.6** Status-aware ledger: typed messages, delivery at idle, interrupts, `blocked_on`
      wake-ups, `cs msg` and `cs log --follow`. **L**
      *Done:* plus `cs ready`/`cs blocked`/`cs assign`/`cs ask`; interrupts stop a working
      agent's turn (Esc) and deliver at idle; mail for an exited member resumes it.
- [x] **M11.7** Integration overlay generators (Go, Rust, Node/pnpm, Python, compose), each opt-in
      per member and never committed, plus integration checks and contract checks. **L**
      *Done:* go.work and cargo `[patch]` in the root, node_modules symlinks (installed copy set
      aside), venv `.pth`, git-excluded compose override; `cs overlay … off` restores.
- [x] **M11.8** Set checkpoints: `refs/ptyro/cs/<set>/<n>` per member (HEAD plus snapshot of
      uncommitted work), automatic checkpoints before revisions and land, and `cs restore` with
      loss checks. **M**
- [x] **M11.9** Lockstep git: `cs commit` with `Change-Set` trailers, dependency-ordered resumable
      `cs push`, linked PRs with a live sibling table, and the gated `cs land` merge train. **L**
      *Done:* PR mode waits for CI and merges with `gh`; repos without PRs rebase, fast-forward
      and push; `Change-Set-Members` names the members committed before each one.
- [x] **M11.10** Lead MCP tools (`cs_*`, SPEC §15.5.9), with no push/merge tools by design. **M**
      *Done:* plus `cs_contract_submit` and `cs_log`; the daemon refuses shipping ops from any
      agent.
- [x] **M11.11** (R-ORC-14) TUI: set node in the sidebar, set view with swimlanes (`prefix S`), attention
      roll-up, ledger timeline. **L**
- [x] **M11.12** Templates (`~/.config/ptyro/changesets/*.toml`, trusted hub repos) and
      `cs new --template`. **S**
      *Done:* hub templates are trusted per template (`--trust FP`, or asked on a terminal).
- [x] **M11.13** Failure-mode states and suggested actions (SPEC §15.5.11), with a scripted test
      for each using fake agents and temp repos with bare remotes. **M**
      *Done:* `land_stopped`, `member_conflict`, `member_failed`, `out_of_order`,
      `missing_worktree`, `integration_failed`, `check_failed`, `stale_contract`,
      `out_of_fence`, `no_remote`, `blocked_on`, each with an action.

**Done when:** `ptyro cs new add-user-tier --projects api,sdk,web` starts a lead that drafts a
contract you approve. Three member agents (one of them Codex) implement it, and `sdk` and `web`
build against the unpushed `api` through the overlay. A contract revision reaches exactly the
affected members. You restore a checkpoint after a bad turn, then `cs land` merges the three PRs
in dependency order. Injecting a CI failure in `sdk` stops the train with a precise partial-state
report.

*Status 2026-10-21:* every task is done. `crates/ptyro/tests/m11.rs` (6 tests, fake Claude and
Codex agents, a fake `gh`, temp repos with bare remotes):
- **The Done-when, local.** A three-member set (one member on Codex) with manifest-derived
  order and the node overlay; the lead's contract approved; fences for both agents; `blocked`
  and `ready` waking a member; a revision reaching only the member it affects (as an
  interrupt); a set checkpoint restored across two repos; `cs commit` trailers; `cs land`
  merging all three in order.
- **The Done-when, PRs.** One PR per member with the live sibling table; gates answered from
  the CLI; a CI failure injected in `sdk` stops the train ("Landed: api; not landed: sdk,
  web"); fixed CI resumes it.
- **Failure modes:** a failing check, no remote (resumable push), a failed member agent and its
  recovery, a stale contract, an out-of-order merge, a rebase conflict, creation failing
  halfway (nothing left behind), cycles, hub-template trust.
- **The lead's tools** through `ptyro mcp`, and the set surviving a daemon restart.
- **The TUI:** the sidebar node and the set view's swimlanes, gate and ledger.

**Not yet:** `--from-issue` reads GitHub only; integration checks don't block a `ready` report;
PR tables refresh on `cs pr` and landings, not from the PR poller. **Needs you:** a real
multi-repo set of yours end to end (the experiment used toy repos), and a real `gh` merge train.

---

## H1 — Hardening from the full code review (2026-10-21)

Goal: everything a full, eight-way code review of the whole tree found (about 120 findings: the
daemon core, orchestration, agents/remote/git, the CLI and `ptyro serve`, the TUI, the emulator
and protocol, the core libraries, the web app and the build) is fixed, or here with a reason.
Each fix has a focused test where one could be written.

- [x] **H1.1 Security.** *Done:*
      - child output can no longer type into its own shell through an XTGETTCAP reply (non-hex
        requests aren't echoed);
      - bracketed paste drops every ESC/CSI (nesting beat the old removal);
      - recipe values in command fields are shell-quoted (`render_shell`);
      - a policy-bound agent gets an allow-list, the hook exemption is by request (a client
        claiming `kind: hook` got none of the policy before), and a session in two runs gets the
        stricter policy;
      - sessions read only their own mail;
      - fences, guards and leases match real paths (`..`, symlinks; the node overlay's links
        reached siblings);
      - answers name their prompt (`Answer.expect`, links, push payloads, held hooks with
        generations), so a stale Allow never approves a newer request;
      - PR comments instruct agents only from owners, members and collaborators;
      - `ptyro serve` keys its lockout on the proxy-added address, caps logins in flight and
        connections, and prunes its failure map;
      - push subscriptions only for the browsers' push services; devices unsubscribe only their
        own; revoke-by-name refuses ambiguity; verify/lead spawns need `full`; `Focus` is ignored
        from devices;
      - ssh targets can't be options, and the remote binary can't escape its quoting;
      - the untracked-file preview doesn't follow symlinks; worktree file copies never write
        through links;
      - the callback key is created 0600 and never from a weak random fallback;
      - git revisions from config are passed after `--end-of-options`.
- [x] **H1.2 Crashes and hangs.** *Done:*
      - a resize to 0 columns or 65535×65535 no longer panics or OOMs the daemon;
      - frames stay under the frame limit (combining marks capped, a byte budget, bounded
        scrollback documents), and style tables are bounded;
      - the meta/term lock-order deadlock on the emulator-panic path is fixed;
      - a failed live upgrade restores everything (fds private, accept loop back, goodbye
        cleared);
      - the exponential glob is now linear;
      - pipe deadlocks fixed (secret commands, `git check-ignore`), and sinks and the roster no
        longer run inline with status;
      - a request or response that can't be decoded is answered instead of hanging;
      - the TUI no longer panics on hint truncation, and its panic hook restores the terminal
        fully;
      - the key-encoding overflow found by the new overflow-checked fuzz profile is fixed;
      - `ptyro mcp` stops with its client;
      - replay speed and recording arithmetic are bounded.
- [x] **H1.3 Correctness.** *Done:*
      - **Daemon and orchestration:**
        - restarts can't orphan a process; names are unique under concurrency; nothing is
          spawned during an upgrade;
        - run and step endings are final (no double "ended"), recovered retries run, and
          gates are never re-asked, time out to a real option, and stop with their run;
        - `cs rm` waits for a train;
        - every signal (hooks, reports and ticks too) goes through the one feed queue, in
          order; hooks wait for theirs;
        - snapshots are written newest-only;
        - review comments stay until delivered;
        - workspace moves are saved and announced; project adds are serialized;
        - checks kill their whole process group, and scratch worktrees are always removed.
      - **Git:**
        - checkpoint restore removes files deleted at the checkpoint and keeps the old HEAD
          reachable;
        - a paused rebase or cherry-pick is never aborted;
        - quoted diff paths, scp remotes with digits and `**/` profile rules are handled;
        - a fresh checkpoint copies the index (no full re-hash).
      - **CLI:**
        - `new --new-wt/--pr/--issue` honours every flag and starts a shell without `--agent`;
        - `send --raw` sends bytes (`SendBytes`);
        - `send S --raw TEXT` parses;
        - `pr checks --json` exit codes, `daemon stop` timeouts, the launchd restart loop and
          `events --session` selectors are fixed.
      - **Web app:** it resubscribes after reconnects and keeps the draft, caret and scroll
        across updates.
      - **TUI:** the selection follows its row, stale replies are dropped, and scrolling, ages
        and the copy-mode anchor are right.
- [x] **H1.4 Build and tests.** *Done:*
      - `make fuzz-smoke` builds with overflow checks and debug assertions (`[profile.fuzz]`);
      - the release workflow uses a supported macOS runner, checks the web build, and re-runs
        cleanly;
      - icon checks compare pixels;
      - the verify scripts can't pass vacuously;
      - test CLIs never autospawn a daemon (`PTYRO_NO_AUTOSPAWN`).
- [x] **H1.5 The rest** (2026-10-21).
      - [x] The grid itself is bounded: a filter in front of alacritty's parser lets at most 16
        zero-width characters through per printed base character. It follows escape state, so an
        escape sequence's bytes neither count nor reset the allowance, and it carries split
        UTF-8 across reads. ASCII and Latin-1 skip the work. Parse throughput stays in the SPEC
        §18 range (141–183 MB/s plain, 83 MB/s SGR-heavy); fuzzed with overflow checks.
      - [x] Parallel permission requests in structured sessions: every driver (Claude, Codex,
        ACP, OpenCode) keeps each open request and answers them in turn, newest first (the one
        the status shows, which becomes blocked again on the next). A cancel, an interrupt or
        an answer given in the agent's own UI removes exactly that request; none is dropped,
        so the agent never hangs on one nobody can answer.
      - [x] Push to self-hosted services: `[remote] push_hosts` (done in H1).
- [x] **H1.6 Pre-release security review: agents, pairing, repository code** (2026-10-21).
      - [x] **M1, agents answering for a person.** Policy or not, an agent can't type into a
        session blocked on a person (`send`, `send --raw` or a client's keystrokes, which now
        pass the same guard), pair or revoke devices, subscribe pushes, trust scripts, recipes
        or templates, or upgrade the daemon to another binary. Callers are placed by ancestry,
        then POSIX session (`getsid`), then (Linux) the exec-time `PTYRO_SESSION_ID`; what
        leaves all three is treated as a plain shell, by choice (SPEC §17). SPEC §17 and
        docs/remote.md now say plainly that this stops mistakes and casual prompt injection,
        not code running as the user.
      - [x] **M2, the pairing link.** `ptyro pair` prints a single-use code that works for 10
        minutes; the web app trades it for the token through `POST /api/pair` (protocol 1.10:
        `Pairing`, `DeviceClaim`). The printed text is worthless once claimed or expired.
        `pair` waits and reports who claimed it; the default scope is `approve`.
      - [x] **M3, repository code outside the sandbox.** Background git runs with
        `core.fsmonitor=false` and no hooks (hooks only for the commit, push, merge, rebase,
        cherry-pick and new worktree the user asks for); diffs skip external diff and
        textconv. SPEC §10.5/§15.6.5 and docs/config.md say that trust pins a command's text,
        not the files it runs, and that scripts, `after_turn` and checks run as the user
        outside any sandbox. Not sandboxed.

---

## UX1 — Out of the box (2026-10-22, top priority)

Goal: someone who has learned nothing gets from `cd myrepo && ptyro` to an agent working in its
own worktree, sees at a glance what every session is doing, and ships and cleans up the work,
without typing a ptyro command or opening the docs. **The TUI is first class** (SPEC §1.3):
every workflow must be complete and discoverable there; the CLI stays complete for scripts and
agents. Ideas come from RESEARCH R-UX-18…28 and R-WT-10, plus the funnel ids named per task.

**Done when:** on a fresh machine with only `claude` installed and no config:
1. `cd myrepo && ptyro` registers the repo (an undo toast says so).
2. `Ctrl-Q n`, type a task, `⏎`: Claude is working in a new worktree named after the task.
3. The sidebar row shows `claude`, the task and the current step. While Claude is blocked, the
   row shows what it wants and the keys that answer it.
4. The worktree's finish flow commits and opens a PR, and once it merges, one key cleans up.

All of it works by keys alone, and `make tui-parity` passes. Linux and macOS.

**Done (2026-10-23).** `tests/ux1.rs` runs the four steps above as written, by keys in a
headless terminal (`ux1_done_when_from_a_fresh_machine_to_a_merged_and_cleaned_up_pr`, ~9 s): a
home with no config and only `claude` on `PATH` (the pretend agent), a repository with a
remote, a fake `gh`; the first run registers the repository, the task box starts the agent in
`fix-typo-greeting`, its blocked row says what it wants and `y/a/n answers`, peek answers it,
`F` → a pull request commits (the agent writes the message) and opens it, the PR merges, and
`F` (the hint's key) cleans up. `make tui-parity` passes. Checked on macOS; CI runs it on Linux.
Closing it out also fixed: an agent session's row names its agent (a wrapper or launcher was
shown by its process name); the auto-registration sweep places a session in a project added
after it looked, and retries a registration that failed (both flaked under load); two archives
of one worktree at once (a PR poll and a refresh both seeing it merge) could list it twice; the
UX1 tests wait for what's on screen, not a clock.

### First class, and seeing what's going on

- [x] **UX1.0 TUI first, enforced** (R-UX-21) — S
  - SPEC §1.3, `AGENTS.md`, README and these conventions say the TUI is first class (done
    2026-10-22 with this milestone).
  - `make tui-parity`, run in CI: it lists the CLI command tree (from clap) and the palette's
    actions and fails on a command with no TUI path unless `docs/tui-parity.md` waives it with a
    reason (e.g. `ptyro hook`, `ptyro proxy`).
  - *Done when:* CI fails on a new CLI command without a TUI action or a waiver.
  - *Done (2026-10-22):* `crates/ptyro/src/parity.rs` walks the clap tree and checks it against
    the table in `docs/tui-parity.md` (an action, `shown:`, `waived:` or `gap: UX1.x`). It runs
    in `make test`; `make tui-parity` lists the 76 gaps left and `STRICT=1` fails on them. UX1
    is finished when `STRICT=1` passes.
- [x] **UX1.23 A removed session's pane shows the next one** (owner, 2026-10-22; high priority)
  — S.
  - **Today:** when the shown session is removed (`x` on an exited session, `X`, `ptyro kill` /
    `rm`), a split closes, but the last pane takes the first not-shown session in tree order,
    or none, and then shows "(empty pane)" (`SessionRemoved` in `crates/ptyro-tui/src/app.rs`).
  - **Instead:** the pane shows the session that came *after* the removed one in the sidebar,
    else the one before. Live sessions come first, nearest first: same worktree, then project,
    then workspace. The sidebar selection and focus follow it.
  - A pane is empty only when the workspace has no sessions left, and then it shows the empty
    state (`n` to start one; the home screen once UX1.19 lands), not a blank.
  - Stopping a live session with `x` leaves it listed as exited. With `[ui] on_stop = "next"`
    (the default) the pane moves on too, and `C-r` / the sidebar bring the exited one back.
  - *Done when:* TUI tests remove the middle, the last and the only session, in one pane and
    in splits, and the pane shows the expected neighbour each time.
  - *Done (2026-10-22):* `neighbour()` and `App::move_on` in `crates/ptyro-tui/src/app.rs`, with
    tests for the ranking, removal, splits and `on_stop`; the empty pane says what to do next
    (SPEC §11.2h).
- [x] **UX1.1 Real program names** (R-UX-20) — S. Claude Code's process is named after its
  version, so the foreground probe shows `2.1.309` (SPEC §20 when verified on Linux). When the
  name looks like a version, or is a known launcher, use argv[0] and the known-agent table, so
  the row says `claude`. `ptyro ls`'s COMMAND column has the same bug. *Done when:* a session
  running Claude shows `claude`, not a version, in the TUI and in `ptyro ls`.
  *Done (2026-10-22):* `ptyro_pty::process_name` reads argv[0] when the kernel's name looks like
  a version (verified against Claude 2.1.291 and 2.1.309 on macOS; SPEC §20.5).
- [x] **UX1.2 Multi-line sidebar rows** (R-UX-20) — M. Line 1 is unchanged: number, status
  glyph, program, and the time in this state. Up to two more lines follow, and only when they
  add something:
  - blocked: what it wants, plus the answering keys (`y allow · n deny`);
  - working: the current step (hook tool summary, turn number);
  - idle or done: the task (the first prompt, trimmed) and the last turn's reply (Claude's
    Stop carries it);
  - a shell: the last command, its exit code and duration (OSC 133).

  `[ui] sidebar_detail = "auto" | "compact" | "full"`. With `auto`, the selected row and blocked
  rows open up and the rest stay compact. Numbering, `j`/`k` and mouse hit-testing follow the
  taller rows. A zoomed sidebar with every row open is the "everything at once" summary view.
  *Done when:* golden screens for each state at each detail level.
  *Done (2026-10-22):* `SessionInfo` gains `task`, `last_reply` and `command` (protocol 1.12);
  `ptyro shell-init` sends the command line with OSC 133 `C` (bash gains `C` at all, from 4.4);
  `ui::sidebar_body` lays out rows for drawing and clicks alike. TUI tests cover each state,
  the three detail levels and clicking a detail line; `tests/ux1.rs` checks the daemon end to
  end (SPEC §11.2h).
- [x] **UX1.3 LLM summaries, opt-in** (R-UX-01; owner approved 2026-10-22) — M, spike first.
  `[summaries] enabled = true` replaces the task and last-reply lines with a short summary of
  the current step.
  - It uses the cheapest model of the session's own agent and **the session's own profile
    account**, so work and personal never cross. Off by default, labelled as a summary, and
    the rule-based lines stay as the fallback.
  - It runs once per finished turn or status change, debounced, cached by turn, and it has a
    per-profile daily cap. The input is the turn's reply and tool summary (or the screen tail
    for generic sessions), with secrets redacted as in the recorder export.
  - **(spike) Stateless call or a resident summariser.** Option A: a one-shot call per turn
    (`claude -p --model <cheapest>`, the Codex/Gemini equivalents) under the profile's env.
    Option B (the owner's idea): a long-lived, cheap, structured "concierge" session per
    profile (M10), fed turn digests, that summarises and can later answer "what's going on?"
    or act as the default lead. Measure the latency, the cost per summary and what context
    gains are worth. Write the result to SPEC §20/§21. Build both behind one `Summarizer` in
    the daemon so B can replace A.
  - *Done when:* with a fake agent and a fake summariser, rows show summaries that update per
    turn. `make verify-claude` checks one real summary.
  - *Done (2026-10-23):* option A, `crates/ptyro-daemon/src/summary.rs`: `[summaries] enabled`,
    `daily_cap` and `command` (default `claude -p --model haiku`). It runs under the session's
    profile, after each finished turn and each new block, one at a time per session, with the
    prompt redacted. `≈` replaces `last` on the row, and the task stays. `session.summarize` and
    `ptyro summarize` make one on demand. Tests: `tests/ux1.rs` (fake agent, fake summariser,
    redaction) and the TUI row test. `make verify-claude` makes a real one: $0.00065, 1–2.5 s
    (SPEC §20.1). **Differs from the plan:**
    - One configurable command, not a `Summarizer` trait. Option B would be a new command or
      mode, and is left open as SPEC §21 question 9.
    - Codex and Gemini sessions are summarised with the same command (Claude's Haiku on that
      profile's account), not with their own agent's cheapest model.
- [x] **UX1.4 Agents started inside a shell session** (R-AGT-09) — M. Typing `claude` in a
  `shell` session leaves it a shell. The sidebar shows the foreground process. Status still
  comes from OSC 133, which reports the whole Claude run as one long **working** command, even
  while Claude waits for you. Proposed fix:
  - When the foreground process (≤ 1 Hz probe, §5) is a known agent, show `zsh › claude ◌` in
    the sidebar. Don't rename the session.
  - While the agent is in the foreground, take status from that adapter's hook-free sources
    instead of OSC 133: titles, screen recognisers and OSC 7501. Mark it low confidence.
  - Go back to OSC 133 when the agent exits.
  - Optionally offer "adopt": restart the session as a managed agent session with `--resume <id>`
    (R-AGT-03), which brings back hooks, hibernation and mail.
  - Looking through `node`/`npx` launchers belongs with detection manifests (R-DD-05).
  - *Done when:* a shell session running a fake Claude shows idle and blocked, not a solid
    working.
  - *Done (2026-10-22):* `inner_agent_tick` in `crates/ptyro-daemon/src/agents.rs`,
    `Adapter::screen_status` (Claude, Codex), `Engine::forget_sources`; adopt for Claude
    (`ptyro adopt`, `A` in the sidebar), resuming the newest transcript. `tests/ux1.rs` runs a
    process really named `claude` through idle, blocked and working, and adopts one (SPEC §8.7).
    Looking through `node`/`npx` launchers is still R-DD-05's.

### Projects and worktrees without setup

- [x] **UX1.5 Auto-register the repo you're in** (R-UX-22; owner chose auto with undo,
  2026-10-22) — S. Running `ptyro` inside a git repository that isn't a project yet just opens a
  shell session, and then `Ctrl-Q N` says "no project here yet". Instead, register it in the
  *current* workspace and toast "registered `myrepo` · `u` undo". A profile pin that `match`
  rules suggest still asks, as today. `ptyro project add` without `--workspace` uses the
  workspace you last used, not always `main`.
  - **The project is always in the sidebar.** Today the tree shows only registered projects,
    so sessions running in an unregistered repo fall under "other sessions", or under nothing
    (seen 2026-10-22: six sessions in `~/dev/ptyro`, no project registered). Any session whose
    cwd is in an unregistered git repo registers that repo too: at spawn, on a live-cwd change
    (OSC 7) and, once, for existing sessions when the daemon starts. A project you undid or
    `project rm`'d is remembered and not registered again.
  - *Done when:* an end-to-end test opens ptyro in a fresh repo and `N` works at once, and a
    session that `cd`s into another repo puts that repo in the sidebar. Undo forgets the
    project.
  - *Done (2026-10-22):* `crates/ptyro-daemon/src/autoreg.rs`; `ProjectRemove{release}` and
    `ProjectAutoAdded`; the TUI's undo through `toast.act` (`C-q u`). `tests/ux1.rs` covers
    spawn, undo, declining, the last-shown workspace and a shell moving into a repo (SPEC
    §10.1). A session already in a project that moves to another repo stays where it is; only
    the new repo is registered.
- [x] **UX1.6 "No project" opens the picker** — S. When `worktree.new` (or anything else that
  needs a project) finds none, open the `project.add` discovery picker directly instead of
  showing a toast that names the palette command. Once you pick a project, carry on with the
  action that asked for it. *Done (2026-10-22):* `N`, `o`, `r`, `R` and `project.trust`, and
  the wizard's "+ add a project…", via `AfterAdd`; `project.add` is bound to `P` (SPEC §11.2h).
- [x] **UX1.9 Prompt-first new task** (R-UX-23, R-UX-01 slugs) — M. `Ctrl-Q n` opens one box:
  "what should it do?".
  - `⏎` creates a worktree, with the name and branch slugged from the prompt (deterministic),
    starts the default agent, and delivers the prompt.
  - `Tab` opens the full wizard (agent, target, name), which gains a prompt step too.
  - An empty `⏎` is today's fast path.
  - The default agent is the configured one, else the only one installed, else the one last
    used in this project.
  - From the shell: `ptyro "fix the flaky test"` does the same and attaches.
  - *Done when:* end-to-end test from keys to a fake agent receiving the prompt.
  - *Done (2026-10-22, with UX1.24):* `Overlay::Task` (preview, Tab name, `C-o` wizard);
    `ptyro "TASK…"`, which registers the repository on the way; `tests/ux1.rs` drives it from
    keys to the agent's prompt (SPEC §11.2h, §12.1). Tab edits the name (UX1.24) and `C-o`
    opens the wizard, instead of Tab opening the wizard.
- [x] **UX1.24 Every new thing has a name and a title** (R-UX-29; owner, 2026-10-22) — M.
  Today agent sessions are named `claude`, `claude-2`…, which says nothing in the sidebar, and
  the name is the only label a session has.
  - **Two fields** (SPEC §2): the **name** is the address (slugged, fixed at creation, changed
    only by rename); the **title** is free text people read (the task, a PR title, later a
    UX1.3 summary) and may change freely. Sessions and worktrees get both; `SessionInfo.title`
    already exists and is unused, and the protocol and store gain the worktree's.
  - **Every create dialog shows the name**: new session, new worktree, workspace and project.
    It is never blank and never required. It is filled with a dimmed suggestion that ⏎ accepts;
    typing replaces it, and the as-typed checks and Tab fix apply as today. In UX1.9's one box
    it is a preview line under the prompt (`→ fix-flaky-test · claude · new worktree`), and Tab
    edits it, so the fast path gains no step.
  - **The suggestion** comes from the best thing known: a slug of the prompt › the PR or issue
    title › the branch › the worktree's name › `<agent>-<corvid>` (`claude-rook`, not
    `claude-2`). It follows the prompt as you type until you edit the name, then stops. The
    title defaults to the prompt (trimmed), else the PR or issue title, else nothing.
  - The sidebar and header show the title when there is one and the name otherwise, with the
    name still visible for typing. UX1.2's detail lines use the title. `ptyro rename` changes
    the name; `ptyro retitle` (and `,` offering both) changes the title. `ptyro new`, `run` and
    `wt new` take `--title`.
  - *Done when:* TUI tests create a session and a worktree from a prompt, from a PR and from
    nothing, and check the suggested name, that ⏎ accepts it, that an edited name stops
    following the prompt, and that the title is shown. `make tui-parity` covers `retitle`.
  - *Done (2026-10-22):* `names::from_text` and `names::corvid`; store v5 (`title` on sessions
    and worktrees); `SessionInfo.title` now the title people read, `window_title` the program's;
    `Retitle{addr, title, worktree}`, `ptyro retitle`, `ptyro wt retitle`, `--title` on `new`
    and `wt new` (`run` takes none: its sessions are scripts); PR and issue worktrees take their
    title. TUI and end-to-end tests cover prompts, nothing typed, suggestions, an edited name,
    retitling and a restart (SPEC §2.1, §11.2h, §14.2).
- [x] **UX1.10 Where work comes from** (R-WT-05) — M. The wizard's target step offers:
  - a pasted GitHub PR or issue URL (→ `--pr` / `--issue`);
  - an existing branch (`--existing`);
  - "take my uncommitted changes": they move from `@root` into the new worktree
    (`ptyro new --take-changes`), stashed safely if anything fails.
  - *Done (2026-10-22):* `WorktreeSpec.take_changes` (a copy at `refs/ptyro/taken/<name>`;
    the main checkout is cleaned only once the changes are in), `Branches`, `work_source` in
    the task box and the wizard. `tests/ux1.rs` moves changes and lists branches (SPEC §10.3).
- [x] **UX1.11 Setup suggestions on first use** (R-WT-10) — S. On a project's first worktree,
  ptyro suggests what it detects:
  - lockfiles suggest a setup script (`pnpm-lock.yaml` → `pnpm install --frozen-lockfile`,
    `uv.lock` → `uv sync`, …);
  - ignored `.env*` files at the root suggest a `.worktreeinclude`.

  The suggestions are shown in the trust dialog and written to the local (uncommitted) project
  config only after a yes. Never on later worktrees. *Done (2026-10-22):*
  `crates/ptyro-daemon/src/suggest.rs`, `ptyro project suggest`, the TUI's dialog before the first
  worktree and `project.suggest`; the CLI asks on a terminal. Written to `.ptyro.local.toml`
  and trusted when they're the only scripts; asked once (SPEC §10.4).
- [x] **UX1.12 Finish and clean up** (R-WT-06, R-WT-09) — M. A worktree's **finish** action
  (row menu and inspector):
  - commit, with the agent asked for the message;
  - then PR or integrate, your choice.

  When its PR merges, the row shows `merged · x clean up`. `x` archives: the branch is
  snapshotted to `refs/ptyro/archive/<wt>` and the worktree removed (`ptyro wt archive` /
  `restore`), instead of only refusing. `gc.archive_on_merge` makes it automatic. *Done when:*
  an end-to-end test goes from finish to merged (fake `gh`) to archived and restored.
  *Done (2026-10-22):* `crates/ptyro-daemon/src/finish.rs`; `ptyro wt finish|archive|restore|
  archives`; `gc` offers archiving; the TUI's `F`, merged `x`, "a: archive instead" and
  `worktree.gc`. The setting is `[worktrees] archive_on_merge` (there's no `[gc]` section).
  `tests/m6.rs` goes from finish to a PR, merged, archived and restored; `tests/ux1.rs` keeps
  uncommitted and untracked work through archive and restore (SPEC §10.8).
- [x] **UX1.13 Companion shell** (R-UX-03) — S. One key opens a shell in the selected
  worktree, with the same profile environment, split beside the agent. *Done (2026-10-22):*
  `shell.here`, `!` after the prefix or in the sidebar, and in the row menu.
- [x] **UX1.25 Quick terminal** (R-UX-30; owner, 2026-10-23) — M. This changes UX1.13's
  companion shell. Today `!` starts a **new** shell, split to the right, every time it's
  pressed, so a few presses leave several shells. Instead:
  - **One per worktree.** `shell.here` brings up the worktree's quick terminal, and starts it
    only if there isn't one. Sessions in the same worktree share it. Outside a worktree (an
    empty pane, the home screen, no project), it belongs to the current directory instead.
    It runs under the worktree's profile, as the companion shell does today.
  - **A drawer at the bottom** of the focused pane, about 30% of its height. `+` / `-` resize
    it, and the size is kept with the view. In a zoomed pane it opens inside the zoom.
  - **One key, three steps** (as VS Code's terminal panel works): hidden → show it and focus
    it; shown but not focused → focus it; focused → hide it, and focus goes back to the pane
    above. Hiding never stops the shell. The daemon owns it, so it outlives a detach.
  - **Keys:** `C-q C-t` and `!` (prefix and sidebar). Ctrl-T alone stays with the program in
    the pane: all four agents bind it (SPEC §20.5), and so do readline and fzf. Anyone who
    wants a global key can bind one in `[keys]` (`"global:…" = "shell.here"`).
  - **In the sidebar** it is a child line of its worktree (`›_ shell`, with the last command
    from UX1.2's OSC 133 line). It has no number, never raises attention and never lands in
    the inbox (R-DD-15's quiet flag, scoped to it). Clicking or ⏎ on that line toggles it the
    same way.
  - **Lifecycle:** after `exit`, the next press starts a fresh one. Archiving or removing the
    worktree stops it with the worktree's sessions. Removing an agent session leaves it alone.
  - **CLI:** `ptyro shell [WORKTREE]` reuses the quick terminal (or starts one) and attaches;
    `--json` prints its address without attaching. `ptyro read` works on it as on any
    session, so an agent can be told "look at the error in my terminal".
  - *Done when:* TUI tests press the key three times (show, hide, show again) and get one
    shell, with the right focus each time. Two sessions in one worktree share the shell, and an
    empty pane gets one in the current directory. It survives a detach and a reattach, a new one
    starts after `exit`, and its row stays quiet while its commands run. `make tui-parity`
    covers `ptyro shell`.
  - *Done (2026-10-23):* `QuickShell` and `SessionInfo.quick` (protocol 1.13),
    `crates/ptyro-daemon/src/quick.rs` and `crates/ptyro-tui/src/quick.rs` (`Drawer`, `Row::Quick`).
    TUI tests cover the three steps, sharing, the no-worktree case, `C-q C-t`, the sidebar line,
    `exit`, zoom, closing the host and the saved view. `tests/ux1.rs` covers finding versus
    starting, quiet attention, removal while busy and at the prompt, `exit`, and `ptyro shell
    --json`/`--read` (SPEC §11.2h, §6.4). **Differs from the plan, or goes further:**
    - `exit` forgets the shell outright (no exited row), so its name is free again and the
      drawer just closes.
    - An agent started in it that blocks still raises attention: quiet, but never silent about
      a question.
    - `ptyro shell --read` prints its screen and never starts one; `--json` prints it.
    - A quick terminal at its prompt doesn't hold up removing, archiving or `archive_on_merge`;
      one running a command does (`Session::busy`).
    - The header names the key that hides it.

### Learn nothing up front

- [x] **UX1.26 Several lines in ptyro's own boxes** (owner, 2026-10-23) — S. In the task box
  (UX1.9), peek's reply and the other text prompts, Enter sends and nothing makes a new line,
  while the agents' panes, `ptyro send` and the web all take several lines (docs/keys.md).
  `Shift+Enter`, `Alt+Enter` and `Ctrl+J` insert a new line. The box grows up to a third of the
  screen and wraps, and the hint line says which key makes a new line. Pasting keeps its new
  lines. Name prompts (rename, a new worktree's name) stay single-line. *Done when:* TUI tests
  type a two-line task with each key, and the agent receives one prompt with both lines.
  *Done (2026-10-23):* the task box, peek's reply and free-text prompts (broadcast, review
  comment, commit message) take a new line from each key, wrap and grow (`ui::edit_rows`), and
  keep a paste's new lines. A paste with a box open used to reach the session behind it. A TUI
  test types with each key; `tests/ux1.rs` types a two-line task and the fake agent
  (`PTYRO_FAKE_PASTE=1`: bracketed paste, as Claude and Codex) gets one prompt. The task box
  test now waits for what's on screen, not a clock (`send_when`), and runs in 3.6 s.
- [x] **UX1.14 Which-key** (R-UX-18) — S. If nothing follows the prefix within
  `[ui] which_key_delay` (400 ms; `0` turns it off), a compact menu of the next keys appears,
  grouped and as bound. Keys typed before it appears work as today. *Done (2026-10-22):*
  `draw_which_key`; the event loop wakes at the deadline (SPEC §11.2h).
- [x] **UX1.15 Row menu** (R-UX-19) — S. `.` in the sidebar, or a right-click, opens every
  action that applies to that row. For a worktree: open a session here, shell, diff, finish,
  PR, pair, run script, open in editor, archive, remove. For a session: peek, rename, restart,
  hibernate, explain, mute. It is the palette filtered to the row, and it shows each key.
  *Done (2026-10-22):* `App::row_actions` (state-aware), right-click; actions of tasks still to
  come (finish, archive, a shell here) join as they land.
- [x] **UX1.16 Context hints** (R-UX-28) — S. The status bar's hints follow the selection:
  blocked → `p answer`, a diff → `D review`, merged → `x clean up`, an exited session →
  `C-r restart`. *Done (2026-10-22):* `App::context_hint`, keys as they work where the focus
  is.
- [x] **UX1.17 Errors offer the fix** (R-UX-27) — S. Audit the ~50 `toast(` calls in
  `crates/ptyro-tui/src/app.rs`: none names a command to type. Each either does the obvious
  thing (UX1.6) or carries a key that does it ("not in a worktree · `N` new one").
  *Done (2026-10-22):* `toast_offer` with `Then::{Run, Send, Edit}` behind `C-q u`; the daemon's
  CLI hints map to actions (`SUGGESTED`); a test fails on any toast that names a command (SPEC
  §11.2h). The copy-mode "set up `ptyro shell-init`" hint becomes UX1.20's offer.
- [x] **UX1.18 The palette searches everything** (R-UX-09) — S. Actions, sessions, worktrees,
  projects, `[scripts.*]` and recipes in one fuzzy list, each with its key, so the palette
  teaches the keys. *Done (2026-10-22):* worktrees, projects and the context worktree's scripts
  (fetched as it opens); recipes join with UX1.21's recipe runner.
- [x] **UX1.19 First run and home in the TUI** (R-UX-25) — M.
  - No config yet: `ptyro` opens `init` as a screen. Found agents, accounts and repos are
    checkboxes, and ⏎ writes the config. CLI `ptyro init` stays as the scripted form.
  - Outside any repo with no sessions: a home screen with recent and discovered repos (⏎ opens
    one, auto-registering it), who needs you, and PRs waiting.
  - *Done (2026-10-22):* `crates/ptyro-tui/src/onboard.rs`; the CLI marks the TUI it opens on
    a first session (`Options::welcome`); `H` and `first.run` any time. A TUI test ticks and
    writes; `tests/ux1.rs` opens a fresh `ptyro` on it once (SPEC §11.2h). The terminal checks
    and shell extras of `ptyro init` are the health badge's (UX1.20).
- [x] **UX1.20 Health badge** (R-UX-24) — M. The doctor's findings appear as a status-bar
  badge, and a key opens them, each with a fix: add the `shell-init` line (after a yes), tmux
  options, an agent not logged in (`profile login`), broken hooks, or a newer installed binary
  ("upgrade the daemon live? `u`"). *Done (2026-10-22):* `crates/ptyro-tui/src/health.rs`: the
  doctor in the background, the TUI's own shell-init check, `M` with a fix per finding, and
  `run_cli` for the daemon's and integrations' commands (SPEC §11.2h).
- [x] **UX1.21 TUI parity gaps** — L, split as UX1.0 lists them. Today (2026-10-22) the TUI
  cannot:
  - run a recipe, or create a change set;
  - run `gc` or archive;
  - restore a checkpoint;
  - `wt pair` / `unpair`;
  - manage profiles or log in;
  - pair a phone (render the QR in an overlay);
  - start `serve`.

  Each gets a palette action and, where it is frequent, a key or a row-menu entry.
  *Done (2026-10-22):* the frequent ones native; the long tail through
  `crates/ptyro-tui/src/cli_actions.rs` (a `ptyro …` template per action, its words from the
  selection, a pick or a question, run in a split). `docs/tui-parity.md` has no gaps, and the
  check is strict by default now (SPEC §11.2h).
- [x] **UX1.22 Interactive tour** (R-UX-26) — S. `ptyro tour` (and the first run's last step)
  does the steps with a fake agent in a throwaway repo: new task, answer a prompt, review the
  diff, finish. It replaces the pages.
  *Done (2026-10-23):* `crates/ptyro/src/tour.rs` runs a ptyro of its own (its own home, your
  `[ui]` and `[keys]`, a demo repository, the fake agent in tour mode) and deletes it after;
  `crates/ptyro-tui/src/tour.rs` is the coach, five steps that move on when done (task, answer,
  review, finish into main, clean up). From a TUI, `help.tour` steps aside for the tour and
  comes back. It needed, and got: integrated worktrees count as shipped (`in main`, `F`/`x`
  clean up; before, only a merged PR did, and `F` on one reopened the finish picker); one-key
  choices in the finish picker; a fake agent that commits when finish asks. `tests/ux1.rs` takes
  the whole tour in a headless terminal in about 6 s, and a stale tour is swept by the next.

### Sidebar

- [x] **UX1.7 Drag to resize the sidebar** — S. SPEC §11 says the sidebar is resizable, but
  today only `[ui] sidebar_width` sets it. Divider drag (M2.15) covers splits between panes
  only. Dragging the sidebar's right edge sets the width, kept within 12–80 and saved with the
  workspace's view like the rest of the layout.
- [x] **UX1.8 Sidebar width keys** — S. With the sidebar focused, `<` / `>` narrow and widen
  it a column at a time. These match the pane-resize keys after the prefix, and both are free
  in the sidebar. They share the 12–80 clamp and the saved view with the drag above.
  *Done (2026-10-22, with UX1.7):* `App::set_sidebar_width` (12–80, the panes keep 21 columns),
  `Drag::Sidebar`, `sidebar.wider` / `sidebar.narrower`; one TUI test covers the drag, the
  clamps and the keys (SPEC §11.2h).

Suggested order: UX1.23 → UX1.0 → UX1.1–1.2 → UX1.4–1.8 → UX1.9, UX1.24, UX1.17 → UX1.14–1.15 → UX1.12 →
UX1.19 → UX1.3 (after its spike) → the rest. Of what's open: UX1.25, UX1.26, then UX1.22 (the
tour can then show the quick terminal).

---

## WEB1 — Web parity (2026-10-23)

Goal: the web client (`ptyro serve`'s PWA, `web/src`) is a first-class client beside the CLI
and the TUI (SPEC §1.3, owner 2026-10-23). Today (M9.3) it lists sessions with their status,
shows a live terminal, sends replies and keys, answers permission prompts, shows the inbox and
structured chats, and takes push notifications. It can't start, stop or restart anything, or
touch worktrees, projects, review and PRs, runs, change sets, profiles or settings. R-UX-31.

**Done (2026-10-24): all of WEB1.** 0 web gaps in `docs/parity.md`, and `make parity` fails on
one. The device needs `--scope full` for the whole app (`control`: answer, type, stop; the pages
that change the machine say so). *Not checked here:* the app on a real phone.

**Done when:** from a phone, with only `ptyro serve` and a paired device, you can do the UX1
demo (start a task in a new worktree, answer it, review its diff, finish it into a PR, clean
up), and `make parity` fails CI on a CLI command with neither a web path nor a waiver.

- [x] **WEB1.0 The parity check covers the web** — S. `docs/tui-parity.md` becomes
  `docs/parity.md` with a web column (`web: <view or button>`, `waived:`, or `gap: WEB1.x`), and
  `make parity` (`make tui-parity` stays as an alias) checks both columns against the command
  tree. Web gaps fail only with `STRICT=1` until WEB1 is done, then always, like UX1.0 did.
  *Done when:* CI lists every web gap, and fails on a new command with no web row.
  *Done (2026-10-23):* `docs/parity.md` (moved from `docs/tui-parity.md`) has a web column; a
  web action is a control registered in `ACTIONS` in `web/src/app.ts` (its tooltip), which
  `crates/ptyro/src/parity.rs` reads, so a cell can't name a control that isn't there. `make
  parity` (in `make test`, so in CI; `make tui-parity` is the same) fails on a command with no
  web cell or a bad one, and prints the web gaps counted by task; `WEB_STRICT=1` fails on them.
- [x] **WEB1.1 Audit** — S. Fill the web column for every command: what the web does today,
  else a gap with the WEB1 task that closes it. Write the result to the table and to this
  milestone.
  *Done (2026-10-23):* 183 commands. The web does 10: `attach` and `read` (a session's live
  screen or chat; its scrollback is a WEB1.2 gap), `list` and `status` (the session list),
  `answer` (Allow, Always, Deny), `send` (the reply box, the key bar), `inbox` and `msg ls`,
  `pair` (this device's half) and `devices push-test` (turning notifications on). 22 are waived
  (another program runs them, or they only make sense on the computer: `serve`, `proxy`,
  `completions`, `shell-init`, `daemon start`, and the TUI's waivers). 151 are gaps, by task
  below. Found on the way: the empty session list says "Start one with `ptyro new` on your
  computer" (WEB1.2 gives it the action instead).
- [x] **WEB1.2 Sessions** — M. New task (the UX1.9 box), new session, stop, kill, restart,
  resume, hibernate, rename, retitle, adopt, summarize, explain, mute; the row menu's actions as
  a session's menu. From the audit (20): `new`, `stop`, `kill`, `restart`, `rm`, `rename`,
  `retitle`, `resume`, `hibernate`, `convert`, `adopt`, `shell`, `summarize`, `broadcast`,
  `events`, `mute`, `msg send`, `verify`, `replay`, `export`; and `read`'s scrollback.
  *Done (2026-10-24):* the web app got a kit (sheets that slide up on a phone, menus, forms,
  caches that re-render as data arrives), a router with a tab bar, and a data model fed by the
  daemon's events. **＋** on the Sessions tab is the new-task sheet (the task, the agent, and
  where: a new worktree from the main branch, a PR, an issue, a branch or your changes; an
  existing worktree; a shell or a command), asking about the project's scripts and a profile
  guard like the TUI does. A session's **⋯** is the row menu: stop, kill, restart, resume,
  hibernate, convert, adopt, rename, title, summarize, why this status, mute, message, verify,
  a shell in its worktree, scrollback (coloured, searchable), events, replay (a canvas player,
  scrubber, 1–8×) and saving the recording; ☑ marks several sessions to type into at once;
  *Earlier sessions* lists what ended. New daemon requests `Agents`, `Replay`, `Export`; the
  welcome carries the device's scope (protocol 1.14, SPEC §6.4). *Found on the way:* `Kill` and
  `Remove` were `full` though the scope's own description said `control`.
- [x] **WEB1.3 Worktrees and projects** — M. The sidebar's tree, a new worktree (from a PR,
  an issue, a branch, your changes), finish, archive, restore, remove, scripts, ports; projects
  add, trust, pin. From the audit (40): `project` (add, discover, suggest, ls, show, rm,
  trust, untrust, config, pin, unpin), `wt` (new, ls, rm, status, pair, unpair, run, open,
  lock, unlock, rename-branch, retitle, finish, archive, restore, archives), `gc`, `ws` (ls,
  new, set, rm, rename, add, remove), `lease ls`, `radar`, `checkpoint` (ls, diff, restore).
  *Done (2026-10-24):* the Projects tab is the sidebar's tree (workspace chips, each project's
  worktrees with changes, PR and sessions, ＋ Task); a project's page (path, remote, pin,
  scripts with a review-then-trust, configuration with where each value comes from,
  suggestions, workspaces, forget); a worktree's page (sessions, status, ⋯: editor, run script,
  pair mode, lock, title, rename branch, archive, remove, which offers to archive instead when
  it holds unsaved work; Finish: a PR or merge into main; Checkpoints), Archived, 🧹 clean-up,
  Workspaces, Leases and the collision Radar. New daemon requests `Editors`, `WorktreeOpen`
  (control), `ProjectConfig`.
- [x] **WEB1.4 Review and ship** — L. The diff inspector (comments to the agent), commit, PR
  and checks, integrate, the review queue. From the audit (19): `wt diff`, `wt log`,
  `wt commit`, `wt integrate`, `review` (add, ls, rm, send, queue, done), `pr` (create,
  status, checks, fix, automerge, watch, reply, open), `watch-pr`.
  *Done (2026-10-24):* a worktree's **Changes** tab is the diff inspector (what to diff against;
  per file and hunk; tap a line to leave a comment that shows under it; one bar to send them to
  the agent as a prompt or clear them; Commit… with the files to pick, or ask the agent to);
  **Commits** (and Merge into main: rebase or cherry-pick, push); **Checks** (the PR, its
  checks with links, open one, fix the failing, auto-merge, watch, reply); More → Review queue,
  with ⎘ to open a PR's worktree or make one on it.
- [x] **WEB1.5 Runs and change sets** — M. Recipes, fan-out compare and pick, gates, change
  sets and their merge train. From the audit (42): `fanout`, `compare`, `pick`, `discard`,
  `iterate`, `run` (ls, show, events, cancel, retry, fork, lead), `recipe` (ls, show, run),
  `gate` (ls, resolve), and every `cs` command but the agents' own (23).
  *Done (2026-10-24):* the Runs tab (open gates on top with their options and a note; runs;
  ＋ a fan-out, a recipe or a lead; 📜 recipes with their variables and source), a run's page
  (steps and why they ended, retry, fork, events, cancel), the compare view (each candidate
  ranked with its tests, judge and reasons; take it by merge, cherry-pick, PR or some files;
  follow up; discard) and the Sets tab (a set's members with what to do about each issue, its
  gates, the contract with its history, diff and an editor that records a revision, approve or
  send back, ledger, checks, checkpoints and restore, overlay, commit, push, PRs, land, add,
  drop, remove). Both poll while open.
- [x] **WEB1.7 TLS by default for `ptyro serve`** (R-REM-11; owner, 2026-10-23) — M. Today
  `serve` speaks plain HTTP on loopback or Tailscale and relies on Tailscale HTTPS or a reverse
  proxy for TLS; any other address needs `--insecure` (tokens in clear).
  - **Default: HTTPS with a certificate ptyro makes.** On first use, a small local CA and a
    server certificate for `localhost`, the loopback addresses, the host name, its LAN
    addresses and its Tailscale name. Kept under `<data>/serve/tls/` (0600) and re-issued when
    the addresses change. `--cert`/`--key` (and `[remote] tls_cert`/`tls_key`) use your own.
  - **Phones trust it once.** Browsers reject a self-signed certificate for PWAs, service
    workers and push. `ptyro pair` offers the CA (a QR code, and a download at `/ca.crt`), and
    `docs/remote.md` covers installing it on iOS and Android. The pairing QR carries the
    certificate's fingerprint, so the page can warn on a mismatch.
  - **Plain HTTP only where TLS ends earlier, or on `--insecure`.** `--tailscale` and
    `trusted_proxy = true` put TLS in front and talk HTTP to `127.0.0.1`, as today.
    `--insecure` is plain HTTP anywhere, with the warning it has now.
  - **`--local`: the local network without Tailscale.** Listen on every interface, but accept
    only peers on loopback, private (RFC 1918, IPv6 ULA) and link-local addresses, over TLS.
    Today that takes `--insecure`. *(Decided 2026-10-23: LAN peers only, loopback and tailnets
    included, always over TLS; plain HTTP stays `--insecure`'s.)*
  - The health badge and `ptyro doctor` flag a certificate that is about to expire, and a
    `serve` running on plain HTTP off loopback.
  - *Done when:* `ptyro serve --local` on a LAN, with the CA installed on a phone, installs the
    PWA and gets push with no browser warning. Tests cover certificate making and renewal, the
    peer filter, and HTTP being refused off loopback without `--insecure`.
  - *Done (2026-10-23):* `crates/ptyro/src/tls.rs` and `serve`: HTTPS by default with a local
    CA (made once, `ca.pem` never changes) and a server certificate it signs (397 days, renewed
    with < 30 days left or a missing name, at start and every 12 h while running), `/ca.crt`,
    `--local` (every interface, local peers only, over TLS), `--cert`/`--key` and `[remote]
    tls_cert`/`tls_key` for your own (re-read every 12 h). Plain HTTP only behind `--tailscale`,
    a detected `tailscale serve` or `trusted_proxy` (and then on loopback only), or with
    `--insecure`. `ptyro pair` points at the running `serve`'s `https://` address
    (`state/serve.json`) and says how to trust the CA, with its SHA-256; `docs/remote.md` has
    the iPhone, Android and desktop steps. `ptyro doctor` and the health badge warn on plain
    HTTP off loopback and on a certificate that wasn't renewed. Tests: certificate making,
    renewal, a lost CA, key modes, the peer filter, names; `tests/m9.rs` speaks real TLS
    (trusted for `localhost` and `127.0.0.1`, refused for another name, `/ca.crt`, plain HTTP
    unanswered, the WebSocket over `wss://`, `pair`'s `https://` link) and serves your own
    certificate. `curl` over this machine's LAN address with `serve --local`, trusting only the
    CA, gets in. **Not checked here:** a real phone installing the PWA and getting push; that's
    the owner's to try. **Dropped:** the pairing QR carrying the fingerprint for the page to
    check, since a page can't see the certificate its own connection uses; `pair` prints the
    fingerprint for you to compare in the phone's certificate details instead.
- [x] **WEB1.6 Settings and health** — M. Profiles and logins, workspaces, the health badge and
  its fixes, the config (validated as `ptyro config edit` does), quit (`app.quit`). From the
  audit (30): `notify test`, `usage`, `integrations` (ls, install, uninstall), `profile` (ls,
  show, new, rm, rename, login, sync, detect, doctor), `devices ls`, `devices revoke`, `pair`
  for another device, `init`, `doctor`, `tour`, `config` (edit, check, show, path), `daemon`
  (stop, restart, status, logs, upgrade, install-service, uninstall-service).
  *Done (2026-10-24):* the More tab: this device (its scope, notifications and a test), Health
  (every `ptyro doctor` check with its fix: log in, edit the config, restart…), Usage meters,
  Devices (list, revoke, pair another with a link and a QR code drawn by `web/src/qr.ts`),
  Profiles (new, find my accounts, log in, sync, check, rename, remove), Configuration (an editor
  checked as `config check` does, applied at once), Integrations, the Daemon (status, logs,
  upgrade, restart, stop, the login service), Set up (the first run: agents, accounts,
  repositories) and the Tour. What a browser can't do itself goes through `POST /api/host`
  (`crates/ptyro/src/host.rs`; `full` devices only), which shares the CLI's own functions
  (`profiles.rs`, `doctor.rs`, `config.rs`). The tour is the real one: a throwaway ptyro with its
  own `serve` on a free local port, opened in a new tab with a coach that follows the same five
  steps and ends by deleting it all; it was walked through for real (task, Allow, diff, merge,
  archive, end). A `full` device may now pair and revoke devices. `ptyro pair` says what the scope
  it made may do. `make parity` now fails on web gaps (`WEB_STRICT=0` only lists them).

- [x] **WEB1.8 A real terminal in the web** — L. (owner 2026-10-24; done 2026-10-24) Today the web draws the
  daemon's screen frames to a canvas (M9.3), so it *shows* a session live, but it can't *drive*
  one: the only input is the reply box (`SendText` + Enter, a line at a time) and a key bar. Make
  the session view a terminal you type in: every key goes to the session as it is pressed
  (the daemon's encoder, so DECCKM, kitty flags and bracketed paste are right), paste, IME, the
  phone's soft keyboard with a modifier row (Esc, Ctrl, Alt, Tab, arrows), mouse (SGR, when
  the app asks), scrollback with touch/wheel scrolling, selection and copy (OSC 52 included),
  a follow-the-cursor/fit mode, and resize that follows the viewport when this device has
  `control` scope. Starts with a spike:
  - *(spike)* **Consume or build?** Candidates: xterm.js (MIT; wants a raw byte stream, which
    the protocol doesn't send the web, and would be a second emulator beside the daemon's),
    a libghostty-vt WASM build (a second emulator, and Zig in the build), and our own: the
    daemon already emulates and sends row deltas, so what's missing is input, scrollback,
    selection and a faster draw loop, not parsing. Lean: **our own**, extending
    `web/src/screen.ts` and the canvas drawer (no dependencies, in keeping with §16.4's embedded,
    zero-third-party app). The spike measures a deltas-only renderer against xterm.js on a phone
    (latency per keystroke, `htop`/`nvim`/Claude redraw, battery) and writes the verdict into SPEC
    §16.4 / §20. Check the idea funnel (RESEARCH.md Part III) first, and add the options there.
  - *Done when:* from a phone, `nvim` and `claude` are usable by keystroke, scrollback and
    selection work, and the web test (`web/test`) types into a real session and compares it with
    `ptyro read`.
  *Done:* **our own**, in `web/src/term.ts` (+ DOM-free `keys.ts`, `grid.ts`): every key as pressed
  through a hidden textarea (soft keyboards, IME, Option-as-Alt, Cmd stays the browser's), a key
  row with sticky Ctrl/Alt/Shift and held-to-repeat arrows, paste, focus events, mouse when the
  program asks (Shift selects), the wheel as arrows in full-screen programs, scrollback with
  momentum over a frozen `ReadScrollback` document, selection (word, line, drag, long-press, edge
  scroll) and copy (wrapped lines joined), OSC 52, find, links, bell, resize-to-screen (devices
  with `control` scope may now take control and size a session, protocol 1.17), only changed rows
  painted. Spike verdict and numbers in SPEC §20.5 (xterm.js and libghostty-vt WASM rejected: a
  second emulator each). Tests: `web/test/term.mjs` (keys, soft keyboards, words, selection, links,
  find) and m9's `the_web_terminal_types_into_a_real_session` (the keys the page sends reach a raw
  session byte for byte; a read-only device can't type; select-all equals `read --scrollback`),
  `a_control_device_may_size_a_session_and_a_watching_one_may_not`. *Not checked:* a real phone's
  soft keyboard and touch gestures (the touch paths are written to the pointer-events and
  `beforeinput` standards and were exercised with a mouse, synthetic wheel events and the
  `type` action, not a finger), xterm.js side by side, battery.
- [x] **WEB1.9 The web server starts with the daemon, as a setting** — S. (owner 2026-10-24; done 2026-10-24)
  `[remote] serve = true` already makes the daemon supervise `ptyro serve --managed` (M9.2, SPEC
  §16.3), but nobody finds it: it is a config key, off by default, with no UI. Make it a setting
  you can turn on (the config panel, CFG1; `ptyro init`'s question; `ptyro daemon start --serve`;
  the web's own settings, WEB1.6), show in `ptyro daemon status` and the health badge whether it
  is up and at what URL, and say what turning it on changes (a listening port; `pair` next).
  *Decision to record:* keep it a supervised child process rather than build it into the
  daemon. The daemon owns every PTY and is local-socket-only by design; the network-facing
  HTTP/TLS code belongs in a process that can crash, restart and be killed for a live upgrade
  without touching a session. The user-visible result is the same ("the web is up when the
  daemon is"). *Done when:* a fresh install answers "yes" to one question and the phone can
  pair after a daemon restart, with no `ptyro serve` typed.
  *Done:* `ptyro web on|off|status` (`--via tailscale|lan|here`, `--pair`), `ptyro daemon start
  --serve`, the `ptyro init` question (never answered by `--yes`), the TUI's `remote.web*` actions
  and the web app's More → Web app all flip `[remote] serve` through one function
  (`config::set_serve`) or the daemon's `Request::Web` (protocol 1.16). The supervisor wakes at
  once; `daemon status`, the doctor and the web page say whether it is up, where, since when, how
  it last ended and whether a by-hand `ptyro serve` holds the port. New key `[remote] tailscale`.
  Tests: `the_web_app_is_a_setting_the_daemon_keeps`, `only_a_full_device_changes_the_web_apps_server`,
  `serving_the_web_app_is_one_setting…`. *Since:* `remote.serve` and `remote.tailscale` are in the CFG1 panel. *Not done:* a QR code on
  the TUI's `remote.web_on` (it prints `ptyro pair` as the next step).

Suggested order: WEB1.0 → WEB1.1 → WEB1.7 (phones need it) → WEB1.2 → WEB1.3 → WEB1.4 → the
rest (WEB1.8's spike can run any time; WEB1.9 is small and can go first). From WEB1.0 on, every
new feature in any milestone ships with its web part.

---

## CFG1 — One configuration panel (2026-10-24)

Goal: every configuration option can be found and changed from inside ptyro, with no
hand-editing of TOML (owner 2026-10-24). Today `ptyro config edit` opens the file in an editor
(commented, validated on save); the panel is the front door for people who don't want to know
the keys.

**The rule going forward:** a new configuration option isn't done until it is in the panel
(and the web's), like the CLI/TUI/web parity rule (SPEC §1.3). The panel reads the schema, so
the cost of that rule is a description and a type on the key, not UI code.

- [x] **CFG1.0 A schema the panel reads** — M. *Done 2026-10-24.* One description per key in
  `ptyro-core`'s `settings.rs`: section, kind (bool, number with a range, text, path, one of,
  several of, a list, a command, a duration, a size, a time window, a colour, an address, a
  secret, a table), one line of help, when a change applies (`live`, `next`, `restart`),
  advanced or not, and which sink type a field is for. Defaults come from `Config::default()`,
  not a second copy. `ptyro config edit`'s first file is generated from it (every ordinary
  setting, commented out, with its help). A test walks every config struct and fails when a
  field has no entry or an entry has no field; others check each choice loads, each default is a
  choice, and every default typed back gives the same config. Edits are text to text
  (`set|unset|add|remove|rename`), keeping the file's comments and order, validated whole before
  anything is written. SPEC §13.7. Agent commands, env and profile secrets stay in the file
  (`opaque`): the panel opens it at their table.
  - Made true on the way: `daemon.log_level` is read (it never was), live, with `PTYRO_LOG`
    winning; a `[remote]` change starts the supervised web app again with it; `daemon.log_level`
    is validated.
- [x] **CFG1.1 The TUI panel** — L. *Done 2026-10-24.* `C-q C` (not `,`, which renames) and a
  `setting <key>` entry in the palette for every setting. Sections on the left with how many you
  changed, settings on the right with the value, its default, where it comes from (and the line)
  and when a change applies. `/` searches keys, help and values; `m` shows only what you
  changed; `A` the advanced ones. Edit by kind: ⏎ or space toggles a switch; a list of choices
  (the theme shows as you move over it, `Esc` puts it back; the action list for a key binding
  is searchable); a set toggles each choice; numbers step with `+`/`-`; a box checks the value
  against the real parser as you type and stays open with the reason; keys are captured by
  pressing them (`ui.prefix`, a new binding). Agents, themes, editors, key bindings, notification
  services and a repository's run scripts and prompts are added (a service asks for what its
  type needs, and is written whole), renamed and removed; profiles run `profile.new|rename|
  remove` (they set up account folders). `r` resets, `u` undoes. A repository's own sections
  follow yours, saving to `.ptyro.local.toml` or, with `s`, the committed `.ptyro.toml`. A
  setting that needs a daemon restart says so and `!` does it. A config that doesn't load
  still opens, on defaults, with the reason. *Tests:* 22 panel tests, and an end-to-end one that
  drives the real TUI in a terminal: a switch, a choice, a number and a list change, the file
  keeps its comments, and the running daemon uses the new `discover.roots`.
- [x] **CFG1.2 CLI parity** — S. *Done 2026-10-24.* `ptyro config list [section] [--changed]
  [--json]`, `get [--explain]`, `set`, `unset`, `add`, `rm`, `rename`, `schema` (all with
  `--project [name]` and `--shared`), `edit [key]` opens the file at that setting; keys
  complete in the shell; a wrong name says what you probably meant. `parity.md` rows for each.
- [x] **CFG1.3 The web panel** — M. *Done 2026-10-24.* More → Settings: sections (two panes on
  a wide screen), search, Changed and Advanced filters, a repository picker with Mine/Shared,
  and a control for each kind (a switch, a select with what each choice means, a number, a box
  checked as you type, a list one per line, colours with a picker, press-the-keys, sets as
  checkboxes). Add, rename and remove with forms that ask for what a new thing needs; an Undo on
  every change; a banner with Restart now for settings that need it; the raw file one tap away
  and openable at a setting. Host calls `settings.schema|view|apply|restore` (full scope).
  Built once per route and updated in place, so typing survives the app's re-renders. Driven
  in Chrome against a sandbox: a switch, a refused number with its reason, undo, search, an
  agent added, a repository's script written to `.ptyro.local.toml`.

*Not done:* the web panel and the TUI panel weren't tried on a phone or in other terminals than
the test one; secrets typed into the panel are stored as written (use `!program args` to keep
them out of the file).

---

## NOTE1 — The scratchpad (2026-10-24)

Goal: one always-available notepad for ideas and bugs you're working, that survives every
session and restart, opens in your editor in one key, and is plain text that shells and agents
can find (owner 2026-10-24, R-UX-32). The CLI, the TUI and the web are all first class (SPEC §1.3).

**Design.**
- **One file, plain text.** `<data>/scratchpad.md` (`[scratchpad] path` moves it, e.g. into a
  synced folder or a git repo). Markdown, so checkboxes and headings work, but nothing needs
  them. ptyro never owns the only copy: it is a file you can `cat`, `grep` and back up.
- **`PTYRO_SCRATCHPAD`** is set in every session ptyro starts (agents, shells, the quick
  terminal, recipes, scripts), next to `PTYRO_SESSION_ID` (SPEC §5.1 env table). Only `[scratchpad] path`
  moves the file; the variable is never read as an override (a daemon started inside another ptyro
  would adopt that one's pad). Agents are told
  nothing by default; an agent that finds the variable can read the pad, and `ptyro scratchpad
  add` is the polite way to write to it.
- **Never lose a word.** Writes from ptyro are atomic appends or whole-file replaces with a
  revision check; a snapshot is kept before every change from the web or the CLI, and daily
  (`<data>/scratchpad.history/`, 30 days, `[scratchpad] keep`), so `ptyro scratchpad restore`
  undoes a bad merge or an agent's mistake.
- **Editing is the editor's job.** The pad opens in the default editor (`[scratchpad] editor`,
  else the `[editors]` default, else `$VISUAL`/`$EDITOR`, else `nvim`/`vim`/`nano`, whichever is
  found). nvim and vim start with `autoread` so an append from elsewhere appears without a prompt.

- [x] **NOTE1.0 The file, the variable and the CLI** — S. The path, creation on first use with a
  short header explaining itself (`# Scratchpad`, how to add to it), `PTYRO_SCRATCHPAD` in every
  session's environment (and `PTYRO_SCRATCHPAD` honoured as an override of the path), and
  `ptyro scratchpad`:
  - no arguments: open it in the editor (waits, like `git commit`; from a ptyro pane it opens
    the pane, NOTE1.1);
  - `path`, `cat` (`--tail N`), `edit`;
  - `add <text…>` (or stdin: `git diff --stat | ptyro scratchpad add -`): appends a stamped bullet
    — `- [ ] 2026-10-24 14:02 · ptyro/bugs-roadmap-requests · <text>` — with the time, and the
    worktree and session it came from when run inside one (`--no-stamp`, `--section Bugs` to
    append under a heading, created if missing);
  - `todo [--json]`: the open `- [ ]` items, with line numbers; `done <n|text>` ticks one;
  - `log` and `restore [rev]` for the history; `--json` on all of them.
  *Done when:* a test starts a session, finds `PTYRO_SCRATCHPAD` in its environment, appends from
  it, and `cat`s the line back; the file survives a daemon restart and a live upgrade.
- [x] **NOTE1.1 The pane** — M. A key (chosen with `docs/keys.md` in hand; also the palette and
  the home screen) brings up the scratchpad in the editor, **in its own pane like the quick
  terminal** (UX1.25): one editor session for the pad, started once and kept running, so the
  cursor, scroll and undo are exactly where you left them. The same three steps as the quick
  terminal (hidden → show and focus; shown → focus; focused → hide), the same drawer
  (`+` / `-` resize), and it follows you across worktrees and projects, since there is only one.
  `[scratchpad] mode = "drawer" | "popup" | "pane"`: a centred popup over everything (a
  floating window about 80% by 70%, closed with the same key), or a normal pane in the sidebar's
  list. The session is kept out of the sidebar's counts and numbering as quick terminals are.
  If the editor dies, the next press starts it again; if no editor is found, `init`'s
  "found tools" line and the error offer to pick one.
  *Done when:* a TUI test types into the pad, hides it, switches worktree, shows it, and the text
  and cursor are there; killing the editor and pressing the key brings it back.
- [x] **NOTE1.2 Capture from anywhere** — M. The point of a scratchpad is that adding to it takes
  no thought:
  - a one-line **quick add** box (`Ctrl-Q` + the pad key with Shift, or the row menu's "note this")
    that appends without opening the editor, stamped with where you were;
  - in copy mode, a key sends the selection to the pad (quoted, with its session and time);
  - on a session row, "note this" adds a line pre-filled with the task or the agent's last message;
  - when a gate, a failed check or a blocked agent needs a thought you can't give now, "note for
    later" adds it as an item that links back (`ptyro://session/<handle>`).
  *Done when:* each of these is a tested key path, listed in `docs/keys.md` and `docs/parity.md`.
- [x] **NOTE1.3 Items become work** — M. A `- [ ]` line is a task waiting to happen.
  - `ptyro scratchpad start [n|text]` (TUI: `⏎` on an item in the pad's **picker**, a fuzzy list of
    open items over the editor) starts a UX1.9 task with the item as the prompt, in a new worktree
    on the current project; the item is tagged `[wt:<name>]` so it's traceable.
  - When that worktree's PR merges or the worktree is cleaned up, the item is ticked (`- [x]`, with
    the PR number) — `[scratchpad] tick_on_finish`, default on.
  - The sidebar's footer and the home screen show the open count (`✎ 7`); the health badge doesn't.
  - `ptyro mcp` exposes `scratchpad_read`, `scratchpad_add` and `scratchpad_todo` to a lead agent
    (M7.x), writes stamped with the agent, and shown dimmed in the pad's picker so you can tell
    your notes from theirs.
  *Done when:* a test adds a bug as an item, starts it as a task, finishes the worktree, and sees the
  item ticked with the PR.
- [x] **NOTE1.4 Protocol and live sync** — S. `scratchpad.get`, `scratchpad.put` (with the revision
  it was based on; a stale one is refused with the current text), `scratchpad.append`,
  `scratchpad.history`, `scratchpad.restore`, and an event `scratchpad.changed` from a file watcher
  (`notify`, so an edit from any program reaches every client). Scopes: `read` reads, `control`
  writes. Writes from the web and agents never silently overwrite: three-way merge on the last
  revision seen, and on a conflict both texts are kept with markers.
  *Done when:* a test edits the file from outside and a connected client sees the event.
- [x] **NOTE1.5 The web** — M. A **Notes** view (WEB1.x, same change): the pad as an editable
  document that autosaves and merges as NOTE1.4 says, with a Markdown preview toggle in which
  checkboxes tick with a tap, a find box, and a history sheet to restore from. On the session list
  a **quick-add** box and the open-item count. The PWA is a **Web Share Target**
  (`share_target` in the manifest): sharing text, a link or a screenshot's text from the phone's
  share sheet appends it to the pad. Works offline: edits queue in IndexedDB and merge on reconnect.
  *Done when:* from a phone, share a link into the pad, tick an item, and see both on the
  desktop's open pane without reloading.
- [x] **NOTE1.6 Configuration, docs and tests** — S. `[scratchpad]` (`path`, `mode`, `editor`,
  `keep`, `tick_on_finish`) in the schema and the panel (CFG1), a README section, `docs/keys.md`,
  `docs/config.md`, `docs/scripting.md` (the variable and `add` for hooks and scripts), the
  parity rows, a `make` target if any fixture needs one, and SPEC §5.1 (the variable) and §16 updated.
- [ ] **NOTE1.7 (later) Pads that follow context** — M. A per-project pad
  (`<repo>/.ptyro/scratchpad.md`, git-excluded) beside the global one with `PTYRO_PROJECT_SCRATCHPAD`,
  `ptyro scratchpad --project`, and a key that cycles global → project → worktree. Only if the single
  pad turns out to be too noisy in use.

**Done when:** you press one key anywhere in ptyro and are typing in your pad; from a shell
inside a worktree `echo "login times out on safari" | ptyro scratchpad add -` lands as a stamped
item; `⏎` on it starts an agent on it; the PR merging ticks it; and on your phone you share a
link into it and see it in the desktop pane.

**Done (2026-10-24): NOTE1.0–NOTE1.6** (NOTE1.7 stays a maybe). SPEC §13.6 has the design; the
CLI is `ptyro scratchpad` (`pad`), `make scratchpad-demo` walks through it, and `docs/parity.md`
has its rows. How it differs from the plan above:
- **Keys:** `C-q ;` toggles the pad, `C-q '` adds a line, `C-q "` lists the open items (`+` and `=`
  were taken). In copy mode `a` sends the selection; in the inbox `n` notes an entry. In the items
  list, tick, show-ticked and editor are `Ctrl-X`, `Ctrl-A`, `Ctrl-E` (plain letters search).
- **Stamp format:** after the checkbox: the time, then the worktree in backticks, then `by` and the
  agent in backticks, then the words (`2026-10-24 14:02 · … · words`), so it parses without
  guessing; tags (`[wt:project/name]`, `(#42)`) go at the end.
- **Watching** is a 700 ms poll of the file's mtime and size, not `notify`: the same on Linux and
  macOS and on synced folders. History keeps every state for an hour, then thins out.
- **`start` on the web** was the one gap, and is closed: Start on an open item opens the new-task
  box with its text (`scratchpad.start`); the `✎ N` count is in the status bar (`[ui] status_bar`) and on the home screen, not a sidebar
  footer; agent-added items show `by <agent>` rather than dimmed.
- **Not done:** an item started where there is no project isn't tagged `[wt:…]`; the popup swallows
  mouse clicks rather than closing on a click outside; the web's Notes page and share target were
  type-checked and unit-tested but not tried on a real phone.
- **Also:** `ptyro doctor` checks the file and the editor; a lead's `ptyro mcp` has
  `scratchpad_read`, `scratchpad_todo` and `scratchpad_add`.

---

## H2 — Second hardening review (2026-10-24)

Nine reviewers each read a slice of the code (daemon runtime, daemon features, TUI, the rest of
the TUI with the new settings code, the `ptyro` binary and web server, core/protocol/store/client,
orchestration/agents/git/pty/term, the web app) and a tenth audited the project as a whole
(docs against code, tests, CI, release, operations). Each finding was re-read in the code before
a fix; six fixers then changed what held up, with tests. *Done:* ~150 items, among them: a reply
over 16 MiB no longer wedges a connection; the client checks who owns the daemon's socket; a YAML
alias bomb, a symlinked or endless `.ptyro.toml` and symlink escapes through `worktree.share`,
pair mode, change-set overlays and `record export` are refused; generated cargo/go/compose
config can't be injected; `{{…}}` in quotes in a recipe's shell command is refused; a resume id
can't be a flag; an agent can't report or hook for another session; `Compare`, `always` and
land/push gates need `control`; a late or double gate answer can't revive a finished step;
`land` waits for CI to exist and merges only the commit it checked; archive stops sessions first
and keeps small ignored files; a restore never moves a branch backwards; a cross-site page can't
lock you out of your own web app, bodies time out, connections are capped; the tour's directory
is private; the settings editor keeps the comments above a key, refuses to undo over a file
changed since, locks against concurrent writers and never sends a token to a client; the TUI no
longer switches workspace before the daemon agrees, answers a question with letters instead of
"deny", and ignores mouse and paste behind full-screen views; the web app's undo can't write
into another repository, deep links to Settings load, dialogs keep their size, OSC 52 asks first.

*Open, ranked* (each is a task when picked up):
- [x] **H2.1 Mutex poisoning cascades.** *Done 2026-10-25.* 360-odd `lock().expect(..)` were the
  daemon's weakest habit: one panic while a lock was held failed every later request touching it.
  `ptyro_core::sync` (`locked`, `reading`, `writing`) takes the guard whether or not the lock is
  poisoned, and every call across the workspace uses it; a request that panics still gets an
  error reply (conn.rs). A ratchet (`make ratchet`, `scripts/unwrap-ratchet.py`, baseline in
  `scripts/unwrap-baseline.txt`, part of `make check`) fails when a crate gains `unwrap()` or
  `expect()` in non-test code; the daemon went from ~540 to 6 and the whole workspace is at 37
  (the baseline file), which may only go down. SPEC §18.2.
- [x] **H2.2 Release and CI.** *Done 2026-10-25.* A release now needs the tag on `master` at the
  crate's version; `make check` on x86_64 Linux, aarch64 Linux and aarch64 macOS (Node required,
  so the web tests can't be skipped); `deny`, `web-check`, `msrv` and `fuzz-smoke` on that
  commit; and each platform's tarball installing through `scripts/install.sh` (a tampered one
  refused). It publishes the tarballs, `SHA256SUMS`, a CycloneDX bill of materials (`make sbom`)
  with signed build provenance (`gh attestation verify`). CI runs the same on every push plus
  `doc` (warnings fail), `lock-check` and an MSRV job, and calls `make deny` instead of an
  action. `scripts/install.sh` is the way to install a release (checksum, provenance when `gh`
  is there, no macOS quarantine). *Not done:* macOS notarization (needs an Apple developer
  account), `x86_64-apple-darwin` is built and install-checked at release time but not in the CI
  matrix, nothing verifies the Homebrew formula's install.
- [x] **H2.3 The binary install's lifecycle (uninstall and backup).** *Done 2026-10-25.* `ptyro
  uninstall` (plan, confirm, stop, remove; keeps repositories, branches and snapshots refs;
  `--backup`, `--purge-refs`, `--dry-run`, `--json`), `ptyro backup`, `scripts/install.sh`, and
  `make reset` as a wrapper; TUI actions and web (More → Daemon). SPEC §14.5, `docs/uninstall.md`.
  *Still open here:* `ptyro update` (download, verify, replace, `daemon upgrade`; the installer
  is the manual version), `ptyro restore`, a Homebrew tap, and a "newer version exists" notice.
- [ ] **H2.4 Tests that don't exist yet.** A table test of every `Request` variant's scope
  (exhaustive `match`, so a new one forces a decision); store migration and downgrade fixtures;
  a handoff image from the previous release; the live-upgrade race with an in-flight launch;
  `land_pr` and gate-closing; notification sinks failing (timeout, 500, DNS) without blocking the
  status engine; a web end-to-end (jsdom or Playwright) for new-session, answer and settings.
  Fuzz targets for the host API, settings edits, imports and the handoff decoder.
- [ ] **H2.5 Profile isolation is not a boundary.** `HOME` is shared, so a profile with no
  `git.ssh_key` still reaches the default Keychain credential helper, `~/.ssh` keys, `~/.aws` and
  the client's `SSH_AUTH_SOCK`. Isolation removes credential-shaped *variables* (many more now),
  not files. Decide (a managed `HOME`? an empty `GIT_CONFIG_GLOBAL` and no agent socket by
  default?) or say so loudly in the profile docs. Related: finish M0.10.
- [ ] **H2.6 The local CA is trusted for every site.** It has no name constraints, because adding
  them would break phones that already trust it. Documented, with a rotation recipe
  (docs/remote.md); a constrained CA with an explicit migration is the fix.
- [ ] **H2.7 Smaller, confirmed, not done:** the first WebSocket message may be 16 MiB before
  login (the library can't change the limit after the handshake); scrollback memory grows with
  terminal width × history (cap total cells); a pid can be recycled between a group's death and
  a late `SIGKILL`; `daemon.lock` is released across the live-upgrade `exec`; the fd-inheritance
  window of a live upgrade should exclude other spawns; saved views are keyed by workspace name
  (a rename orphans one); removing a workspace leaves its projects in none; a gate-held change-set
  job blocks `cs rm` for up to a day; `serve.log` isn't rotated; editors started by the daemon
  are never `wait`ed; raw `SendBytes` bypasses the typing lock; keystrokes typed during the
  terminal-colour query at startup are lost; `ptyro-testkit` and its `PTYRO_TEST_*` knobs ship
  in release builds; the Windows "unsupported" message only shows after dependencies fail (it
  belongs in `ptyro-core`); a few config values still have no format check (`remote.public_url`,
  `callback.listen`, a certificate without its key).
- [ ] **H2.8 Accessibility.** The web terminal is a canvas with no text view for a screen reader
  (a "text view" mode, like the chat view, would do); the TUI has `NO_COLOR` and a
  high-contrast theme and nothing else. SPEC doesn't say whether this is a goal.
- [ ] **H2.9 Unverified on hardware:** Android Gboard backspace in the web terminal and its
  composition, iOS `visualViewport` panning, push on a real phone, Tailscale mode, a real
  Claude/Codex/Gemini turn per adapter (see "Left for the owner" above).

---

## Backlog (unscheduled; ideas welcome)

Back-burnered by the owner:

- **Built-in editor** — a minimal modal editor pane for quick edits (or tight `nvim --embed`
  integration instead of writing an editor).
- **Tasks** — local task list per project; import from GitHub/Linear/Jira issues; "start task" =
  new worktree + agent + prompt from the issue.

Other ideas:

- **Pre-trusted Claude worktrees** (M11.0): every new worktree shows Claude's folder-trust dialog
  (ptyro can answer it now). Codex worktrees inherit their project's trust (§20.2); Claude has no
  flag, so this would mean writing `hasTrustDialogAccepted` into the profile's `.claude.json`,
  only for projects the user trusted already.
- **Editor integrations**: VS Code/Cursor/Zed "open in", plus a VS Code extension that
  shows ptyro sessions for the current folder.
- **Jujutsu (jj) workspaces** via a pluggable VCS backend (Claude's `WorktreeCreate` hook pattern).
- **libghostty-vt emulator backend** (§5.2) for grapheme/image fidelity once its API is stable.
- **Kitty graphics / sixel** passthrough for single-client sessions.
- **E2E relay** (§16.5) with self-hostable server.
- **Droid** (Claude-compatible hooks), **Copilot CLI** (`COPILOT_HOME` hooks), **Cursor CLI**,
  **Goose** adapters, in that order (R-AGT-01). Antigravity was promoted to M4.21. **Aider** stays on the generic
  adapter (maintenance mode). The generic adapter covers all of these meanwhile.
- **Usage & budget meters** (meters promoted to M4.17; budgets remain here): per-session tokens/cost where agents expose it (Claude `result`
  `total_cost_usd`, Codex usage), daily totals, rate-limit countdowns.
- **Per-profile usage meters**: usage and limits per account (builds on the usage meters above).
- **Sandbox per profile**: separate OS user or container per profile for real isolation (§10.10).
- **Container/sandbox sessions** (Docker/Podman) per worktree for untrusted repos.
- **Cloud/SSH hosts as worktree targets**: run agents on a remote box, UI local (overlaps M9.1).
- **Session templates**: "Claude + shell + dev server" trio spawned together per worktree.
- **Timeline scrubber**: replay a session's recording synced with its status events and git commits.
- **Prompt library** with per-project snippets and variables (seeded by `[prompts]`).
- **`ptyro status-line`** output for tmux/starship/zellij bars.
- **Plugin system** (WASM) for custom panes/adapters — only if real demand appears.
- **Windows (ConPTY)** support.
