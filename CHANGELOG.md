# Changelog

Generated from git history by `make changelog` (scripts/changelog.sh); edit commit messages, not this file.

## Unreleased

### Milestones

- M11: change sets — one change across several repos: lead + contract, fenced members, ledger, overlays, set checkpoints, lockstep git and the merge train (c0869f5)
  - M11.0 experiment with real agents (Claude Code 2.1.306; Sonnet lead, Haiku members) on three repos (Go api, Go sdk with `replace ../api`, Node web): 7m47s, all green, nothing pushed. Findings in SPEC §15.7.1 decided the design: members side by side in the set's root (relative-path deps build against siblings), `affects` falls back to section titles, the lead must not commit for members (now flagged), clean member exits aren't failures. Found and fixed a bug: `ptyro answer` couldn't pass Claude's folder-trust dialog (no number keys); adapters now answer what's on screen (`answer_screen`).
  - M11.1 pure model `ptyro-run::changeset` (ChangeSet, Member, ContractRev, SetCheckpoint, Train, CsEvent, derived Report with failure states and actions, topo order with cycle reporting, manifest parsing for go.mod/package.json/Cargo.toml/pyproject.toml, dependency and overlay suggestions, templates with all-at-once validation, CHANGESET.md brief, prompts, sibling-PR table, trailers, overlay file generators). A set is a run of kind `changeset` (RunState.changeset, RunEvent::Cs/LeadSet): event-sourced, survives restarts. One profile and workspace for all members, refused otherwise.
  - M11.2 member worktrees at <root>/<member> (WorktreeSpec.path), all or nothing with rollback; per-member bases; `cs rm` checks every member first (--keep, --force); `cs add`/`cs drop`.
  - M11.3 layouts lead/solo/manual; the lead in the root with --add-dir per member and `ptyro mcp` (SpawnSpec.mcp); members start when r1 is approved, with their part and the contract, and $PTYRO_CS/$PTYRO_CS_ROOT.
  - M11.4 write fences in PreToolUse (Claude and Codex verified): lead → root files only, members → own worktree only, with reasons and the command to ask; advisory out_of_fence for member changes during the lead's turn.
  - M11.5 contract revisions (sha, diff, author, affects), gate contract-r1 (approve/revise/reject), propagation by affects (interrupt vs next idle), `cs contract show|history|diff [FILE]|edit|submit|revise [--diff]|approve|reject`.
  - M11.6 typed ledger messages, readiness and blocked_on wake-ups, interrupts that stop a working agent's turn (Esc) then deliver, mail resuming exited members, `cs msg|ready|blocked|assign|ask|log --follow`.
  - M11.7 overlays (go.work, cargo [patch], node_modules links with the installed copy set aside, venv .pth, git-excluded compose override; recorded and reversible), integration and contract checks (`cs check`).
  - M11.8 set checkpoints refs/ptyro/cs/<set>/<n> (HEAD + uncommitted), automatic before revisions and land; `cs restore` with loss checks (--force checkpoints the present first).
  - M11.9 `cs commit` with Change-Set/Change-Set-Members trailers, resumable dependency-ordered `cs push` with a precise stop report, `cs pr` with a live sibling table, gated `cs land` train (PR mode waits for CI and merges with gh; local mode rebases/ff/pushes), stopping with "Landed: …; not landed: …" and resuming.
  - M11.10 lead MCP tools cs_status/assign/send/contract_get/contract_submit/contract_revise/member_diff/integration_check/checkpoint/request_gate/log; no shipping tools; the daemon refuses shipping/structure ops from any agent.
  - M11.11 TUI: set node atop the sidebar with its lead (amber when it wants you), set view (prefix S): swimlanes, gate answers, issues, the ledger as a timeline with message arrows between lanes.
  - M11.12 templates from ~/.config/ptyro/changesets and trusted hub repos (scope changeset:<name>, --trust).
  - M11.13 failure states with actions: land_stopped, member_conflict, member_failed, out_of_order, missing_worktree, integration_failed, check_failed, stale_contract, out_of_fence, no_remote, blocked_on.
  - Protocol 1.9 (Request::Cs/Reply::Cs). fake-gh gains per-repo state, real merges and `pr edit --body`. New: `ptyro cs`, `make changeset-demo`, docs/changesets.md.
  - tests/m11.rs (6): the Done-when locally and with PRs (injected CI failure in sdk stops the train, fixed CI resumes), every failure mode, the lead's tools + restart, the TUI set view. `make check` green (297 tests), check-macos passes.
  - Docs: README (status, section, commands, keys, dev targets, tests), SPEC (§15.5 built, new §15.7, protocol 1.9, store note, ADR 18, §20.1 trust-dialog facts), ROADMAP (M11 ticked with notes + status; backlog: pre-trusted Claude worktrees), RESEARCH (R-ORC-14 partly built, R-ORC-12 enabled, thurbox note, changelog), docs/cli-json.md, CHANGELOG.
- M9 + M10: remote (proxy, serve, devices, PWA, Web Push, remote approvals) and structured sessions (chat view, ACP, stream-json, app-server, OpenCode, convert) (34c788c)
  - M9.1 `--remote host` / PTYRO_REMOTE for every verb and the TUI: ssh -T BatchMode + `ptyro proxy` bridging stdio to the remote daemon socket (auto-start, zstd frames, PTYRO_SSH / PTYRO_REMOTE_BIN); local-file commands stay local; a remote `ls` failure is an error.
  - M9.2 `ptyro serve` (hyper 1.12 + tokio-tungstenite): static web app + /ws, a separate process the daemon can supervise (`[remote] serve = true`, restart with backoff, killed before live upgrade, state/serve.log), `--tailscale` runs `tailscale serve`; refuses non-loopback/non-Tailscale listens without --insecure. Device tokens (`ptd_` + 256 bits, SHA-256 at rest, constant-time compare), `ptyro pair NAME [--scope]` with a QR code (token in the URL fragment), `ptyro devices [ls|revoke]`; scopes read < approve < control < full enforced in the daemon per request and keystroke; devices never take control or resize; revocation closes live connections (epoch watch); Hello rewritten to kind=web with the real origin; 10 failures / 10 min per IP → 429; remote actions logged to session events.
  - M9.3 PWA in TypeScript (tsc 5.9.3, dist committed, embedded): session list with blocked-first Allow/Deny cards, a canvas screen view from daemon frames at the session's size, reply box + key row, chat view for structured sessions, strict CSP, installable manifest + icons.
  - M9.4 Web Push: VAPID ES256 (RFC 8292) + aes128gcm (RFC 8291) via p256/hkdf/aes-gcm, sent with curl; subscribe/unsubscribe/test; 404/410 prunes; notification Allow/Deny actions answer without opening the app.
  - M9.5 remote approvals: while a device is paired, Claude's PermissionRequest hook is held (approval_wait) for an exact decision from any client; the local dialog stays live and wins if answered first (verified on Claude Code 2.1.305).
  - M9.6 docs/remote.md: setup, Tailscale/Caddy, scopes, threat model.
  - Protocol 1.8: ClientInfo token/via, Scope, DeviceInfo, PushSub, DevicePair/List/Revoke, Push*, Convert, ChatGet/Transcript, ServerMsg::Chat; SessionInfo held/structured.
  - M10.1 transcript model (ptyro_core::chat: messages, thinking, tools with output/diffs, permissions, plans, notices, turn ends with cost) with per-item revisions; SignalSource::Protocol.
  - M10.2-10.5 new crate ptyro-drive (Driver trait, effects) with drivers for Claude stream-json (control protocol, --permission-prompt-tool stdio), the Codex app-server (0.143 handshake, error classification/dedupe), ACP v1 (verified handshake on Gemini 0.64) and OpenCode serve (HTTP + SSE); `[agents.X] protocol`; daemon structured sessions: piped child in its own process group, append-only render into the emulator, line editor, status from the protocol, live-upgrade handoff of driver state and fds.
  - M10.6 `ptyro convert S --to tui|structured` keeping the conversation; `new`/`fanout --structured`; `read --chat`; TUI chat view (prefix t, PageUp/Down); run steps report tool use and cost, shown in compare.
  - Testkit: fake agents for all four protocols (paced deltas), concurrent PermissionRequest hooks. tests/m9.rs (6) and tests/m10.rs (5); `make check` green (280 tests), deny and check-macos pass.
  - Make targets: web, web-check (CI job), verify-structured (real agents: 9 checks passing). changelog.sh no longer aborts under set -e when a group ends with a non-milestone commit.
  - Docs: README, SPEC (§7.1, §13, §16, §17, §20, ADRs 16-17), ROADMAP (M9/M10 ticked), RESEARCH (R-REM-01..04, R-SEC-02, R-ACP-01 marked), docs/cli-json.md, CHANGELOG.
- M8: hardening — live upgrade, recorder, perf, fuzzing, crash safety, compat, releases (0600a85)
  - M8.1 live upgrade: `ptyro daemon upgrade` parks every PTY reader, writes a versioned image (sessions, PTY masters, emulator grids + scrollback + modes as compact msgpack+zstd encoded in parallel, status with engine clocks, run identity, counters) to a memfd and re-execs in place: same pid, children stay ours (waitpid reaper), SIGWINCH redraw nudge, rollback to state/daemon.prev. TUI, wait, send --wait (never typing twice), run wait and events --follow reconnect; runs carry on. Protocol 1.7 (Upgrade/Upgrading, DaemonStatus generation/metrics).
  - M8.2 recorder: framed pty.log ring with resize/keyframe/start records; dormant agents show their last screen; read --scrollback / copy-mode gg reach past memory; `ptyro replay`, `ptyro export --format asciicast|text|ptylog`; [recorder] config; scrollback default 5000.
  - M8.3 perf: criterion benches (parse, frames, scrollback, handoff, paint), ptyro-latency (key→echo→frame p50 0.69 ms, p99 1.0 ms), memory measured; SPEC §18.3 updated.
  - M8.4 fuzzing: six targets shared by a stable runner (make fuzz-smoke, CI) and cargo-fuzz; ~1.05 billion runs, no crash.
  - M8.5 crash reports for every panic, emulator panics isolated per session, watchdog metrics (RSS, fds, tasks, event-loop lag, counters) in daemon status, carried across upgrades.
  - M8.6 make compat: headless sweep (vim, less, htop, top, tmux, zsh, bash, python, man, git, ten vttest screens), all passing; fixed tab cells painting raw tabs; gaps documented.
  - M8.7 negotiated zstd frames (PTYRO_COMPRESS=1).
  - M8.8 make dist / changelog / homebrew-formula, release.yml on v* tags (4 platforms, SHA256SUMS, notes, gh release, tap push), cargo install --git, CHANGELOG.md.
  - make soak (agents + shells + upgrades, leak/stuck/lag checks): 4-minute run clean.
  - Fixes found by the soak and stress runs: requests on a dead connection hung forever; hook status lost to heuristics after an upgrade; first hooks racing session registration; a run's end re-marking a succeeded step cancelled; recorder segment times going backwards; flaky e2e/m4/m7 tests made deterministic.
  - check-macos now type-checks the whole workspace (stand-in C compiler for cargo check).
  - tests/m8.rs (7 tests); SPEC (§4.5, §6, §7.6, §13, §18, §19, §20), ROADMAP, README, RESEARCH, docs/compat.md, cli-json.md and fixtures.md updated.
- M7: orchestration — runs, recipes, fan-out + compare, gates, verified-done, ptyro mcp (3ef55a5)
  - M7.1 event-sourced runs: a run_event log + projection in one transaction (store v4), pinned recipe src/sha, attempts with snapshot refs and native ids, on_restart, failed vs crashed, `run ls/show/events/wait/cancel/retry/fork` with lineage (R-ORC-33).
  - New pure crate ptyro-run: recipe schema v1 with validation, {{templates}}, a small `if` language, RunState/RunEvent, plan(recipe, state) → actions, built-in recipes.
  - M7.2/M7.3/M7.10 `ptyro fanout` + an evidence-ranked compare (tests vs the base, repro, cross-tests, change clusters, diff spread, verified-done, an advisory judge), reasons, top-two diff; pick (merge/cherry-pick/PR/by file or hunk), safe discard, iterate (R-ORC-04).
  - M7.4 recipes: spawn/send/run/gate/apply/sleep/verify + loop, joins, if, timeouts, retries, budgets, guards, typed outputs; survive daemon restarts; repo recipes need trust.
  - M7.5 gates: short stable ids, idempotent resolve from CLI, TUI and inbox; agents can't.
  - M7.6 `ptyro mcp` (stdio MCP, 16 tools, progress, idempotency) injected into `--lead` sessions; the daemon enforces the lead's policy on MCP and CLI alike (peer-pid ancestry).
  - M7.7 mailbox: group addresses, coalesced delivery at idle, daemon-observed acks, loop safety.
  - M7.8 implement-and-review, best-of-n, test-fixer, doc-writer (+ overnight).
  - M7.9 verified-done: Claude bounced from its Stop hook, others by follow-up; evidence kept; sidebar badges; the review queue scores it (R-ORC-23).
  - M7.11 a checkpoint per agent turn (private index), `checkpoint ls/diff/restore` (R-ORC-03).
  - M7.12 loop step + overnight recipe + morning digest (R-ORC-25).
  - M7.13 collision radar (merge-tree predictions, both agents told) and file leases enforced through PreToolUse/BeforeTool denials (R-ORC-11).
  - TUI runs view (O): runs, steps, gates, compare/pick/discard/iterate, fan-out wizard.
  - Fixes found on the way: prompts typed without their trailing newline and timed from when typing starts; one typist per session; concurrent checkpoints no longer share an index.
  - Protocol 1.6; [orchestration] and [verify] config; fake agent honours Stop bounces, denials and MCP; tests/m7.rs (8 tests) + JSON shape snapshot; `make orchestrate-demo`; SPEC §15.6, ROADMAP, README, RESEARCH, docs updated.
- M6: review & ship — inspector, line comments, commits, PRs, PR-event loop, pair mode (49bd473)
  - M6.1/M6.2/M6.8 the inspector (`D`): Diff (file list, unified/side by side, against base, upstream, last turn or HEAD), Commits, Checks, Ports, Events tabs; PR badges in the sidebar.
  - M6.3 line comments kept by the daemon, batched into one prompt for the agent, typed once its turn is over (`review add/ls/rm/send`).
  - M6.4 commit as the worktree's profile (`wt commit`, inspector C/E/A), `wt log`.
  - M6.5 gh integration under the profile: `pr create` (draft, session marker), status/checks in the sidebar via a poller, `pr fix` with failing log tails, `pr open` (R-GIT-03).
  - M6.6 `--pr N|URL` and `--issue N|URL` for `new` and `wt new` (R-WT-02).
  - M6.7 `wt integrate` (rebase + fast-forward, or cherry-pick) and `wt pair/unpair` with stash safety and edits carried back.
  - M6.9 review queue (R-ORC-24) with `max_unreviewed` holding agent spawns and a digest.
  - M6.10 PR-event loop (R-GIT-01): baselined, new CI failures and review comments delivered when idle, questions raise attention, replies "via ptyro", automerge, escalation.
  - Protocol 1.5; [github]/[review] config; ptyro fake-gh; tests/m6.rs (7 tests); a clap debug_assert test; SPEC/ROADMAP/README/RESEARCH/docs updated.
- M5: scriptable control plane — send/wait/read/events, doctor, init, completions, imports (e0c0c26)
  - M5.1/M5.3 CLI parity with SPEC §12.2: every listing has --json (docs/cli-json.md, shapes pinned by a snapshot), stable exit codes (wait: 5 exited first, 7 timeout/stall), protocol 1.4 (SendText wait_ready_ms, Events, Doctor, SelfTest, EventsLost).
  - M5.2 send S… TEXT --wait-ready --wait (R-STA-03), wait --for/--match (R-ORC-22)/--any/--stall, read --json, events (persisted + --follow NDJSON), stop, ls filters; "idle" means after the last prompt ptyro typed (prompt_pending, last_prompt_ms, turns).
  - M5.4 dynamic completions (clap_complete, live names from the daemon).
  - M5.5 ptyro init: detection now handles multi-line functions, fish set -x, ${HOME}, suffix-less aliases; golden rc-snippet tests; adds only missing config keys; tour offered.
  - M5.6 ptyro doctor / profile doctor: logins via the tools' own status commands, daemon PATH and leaked vars, tmux/terminal, git, versions, hook self-test through the real integrations; Codex "hooks need review" recognised.
  - M5.7 daemon install-service/uninstall-service (systemd user unit, launchd), daemon restart, stop --hibernate, logs -f.
  - M5.8 broadcast (CLI; TUI Space marks + b); ptyro tour.
  - M5.9 docs/scripting.md cookbook with an agent skill snippet (R-ORC-18).
  - M5.10 other tools' committed config as script sources (Conductor, Superset, Codex, Claude launch.json, Cursor, Copilot) with provenance in trust and project config --explain.
  - tests/m5.rs incl. the Done-when as a real sh script; SPEC/ROADMAP/README/RESEARCH updated; corrected M4 notes about the shared-worktree warning.
- M4: agent intelligence — adapters, status engine, attention, notifications, resume (be81164)
  - Adapters (M4.1–M4.3, M4.7–M4.10, M4.21): Claude (plugin hooks, statusline relay, trust-dialog-safe prompt delivery, answers, graceful stop, resume), Codex (-c hooks with pre-trusted hashes, chained notify), Gemini (opt-in extension), OpenCode (SSE sidecar and permissions API), Antigravity (from docs), generic and shell.
  - `ptyro hook` relay (M4.2) and status engine (M4.4): high sources as peers with deferred tie-losers, stale gating, `status --explain`; OSC 7501 rev 0.4 conformance (M4.20).
  - Attention, `a`/`p`/`i`/`e`, inbox, peek answers (M4.5, M4.16); notifier with mutes, quiet hours, coalescing, desktop and ntfy/Pushover/Slack/Discord/Telegram sinks with signed single-use approve links (M4.6, M4.22).
  - Dormant sessions, restore_on_start, hibernation, resume (M4.11, M4.12); activity and progress (M4.13); after_turn and branch naming from the first prompt (M4.14).
  - Usage meters per profile (M4.17); Claude roster and child rows (M4.18, socket delivery not adopted); Codex app-server spike recorded (M4.19).
  - fake-agent --as <adapter> (M4.15); tests/m4.rs; `make verify-claude` (real Claude 2.1.304).
  - Protocol 1.3, store v3; SPEC, ROADMAP, README and RESEARCH funnel updated.
- M3: projects, workspaces, worktrees, scripts, ports and profiles (a333ce4)
  - ptyro-git: worktree add/lock/remove/list via the git CLI, repository status, the removal safety matrix (uncommitted, untracked, unpushed-and-unmerged, stashes) and branch fate rules; GIT_OPTIONAL_LOCKS=0 so background status never breaks a user's git add.
  - ptyro-agents: agent commands, profile resolution (explicit > worktree > pin > workspace > default), env plans (managed-var scrub, credential isolation, secrets as <secret:NAME>), share sync that never links credentials, and detection of alias-based multi-account setups.
  - Store v2 and protocol 1.2: workspaces, projects, worktrees, trust; needs_confirmation for profile guards and pin overrides.
  - Daemon: worktree creation (.worktreeinclude, copy, CoW/symlink share, port blocks, trusted setup, agent after setup), safe removal with archive scripts, read-only script sessions with exclusive run mode, locks while sessions run, notify-driven status, listening-port scan, reconciliation and gc. Fixes a config reload loop (inotify open events) and a lost exit notification (watch::send with no receivers).
  - CLI: project, ws, wt, profile (toml_edit config edits), gc; new --agent/--profile/--wt/--new-wt.
  - TUI: workspace → project → worktree → session tree, workspace switcher, new-session and new-worktree wizards (n ⏎ ⏎), trust dialog, editors, scripts, profile badges and tint.
  - make sandbox / sandbox-clean; SPEC, ROADMAP and README updated; m3.rs e2e + TUI tests.
- M2: multiplexer core — multi-session TUI, store, config, copy mode, splits (7e4c432)
- M1.5, M1.8, M1.17: side channels, golden fixtures, e2e tests; retire spike (faf6104)
  - Side channels: OSC 7 cwd, OSC 133 prompt marks/exit status, OSC 9;4 progress (no longer mistaken for OSC 9 notifications), surfaced in SessionInfo (live_cwd, progress, last_command_exit).
  - Golden tests: real recordings of Claude (fullscreen main screen and permission dialog), Codex, vim and ls --color, sanitized with same-length replacements; each also replayed in tiny chunks to prove parsing is independent of read boundaries.
  - e2e: 12 tests against a real daemon in a temp PTYRO_HOME with the built-in fake agent. Found and fixed: dropping a Client didn't close its connection (the reader task held the read half).
  - ptyro-headless (testkit) replaces the M0 spike; agent hook fixtures move to crates/ptyro-agents/tests/fixtures; Makefile gains run, try, headless, smoke, fixture, snapshots, proptest-deep, check-macos.
  - Docs: README now documents real usage; SPEC records what M1 built and the facts learned (Claude's fullscreen renderer, SIGPIPE in children, damage under-reporting); ROADMAP ticks M1.1-M1.17.
- M1: daemon, protocol, emulator, PTYs, client, TUI and CLI (ce0d219)
- M0.4, M0.5: Codex and Gemini hook injection spikes (c776234)
- M0.7: run CI on every branch push (b197059)
- M0.3, M0.6, M0.8: spike findings, Claude hook fixtures, compat docs (30e6b26)
  - OSC 7501 reports idle/working(+msg)/blocked(kind=permission)/done/ clear, 50-80 ms ahead of the equivalent hooks.
  - --plugin-dir and --settings hooks both add to project hooks; --plugin-dir chosen. --session-id pre-assignment and --resume work.
  - Denying a permission prompt fires no Stop hook, so status fusion now means most-recent-wins among high-confidence sources (SPEC §9.2).
  - Notification(permission_prompt) only fires when unfocused.
  - Inherited parent-Claude env vars disable the child's transcripts and leak a messaging token; they are now stripped (SPEC §5.1).
- M0.1, M0.2, M0.6, M0.7: workspace skeleton, make targets, CI, pane spike (19da0d2)
  - Cargo workspace per SPEC §3.3: 12 crates (stubs; `ptyro --version`), toolchain pinned to 1.99.1, shared lints, MIT license per crate.
  - Makefile is the automation entry point (AGENTS.md now says every workflow gets a target); CI runs `make check` on Linux and macOS plus cargo-deny (deny.toml: licenses, advisories, bans, sources).
  - spikes/pty-pane: PTY (pty-process) -> alacritty_terminal -> ratatui, with a side-channel sniffer that answers OSC 7501 / XTVERSION / CSI 16 t ahead of the emulator's replies, mode-aware key/paste/ mouse/focus encoding (kitty flags mapped by name, DECCKM fix), interactive mode (Ctrl-Space prefix, live status line) and a headless mode (--send/--resize/--snapshot/--expect/--record) that verifies everything without a human.

### Performance

- perf: hash rows lazily at frame build, ESC fast path in sniffer; make bench (6daa4e1)

### Research

- research: herdr and tuios deep dives; promote the top 15 ideas into the roadmap (d69638e)

### Documentation

- style: rustfmt the parse benchmarks (ca22e32)
- docs: change sets — lead + per-repo agents across several repos (SPEC §15.5, M11) (2f84275)
- docs: brainstorm change sets (one change across several repos) (e919ddf)
- docs: credit Orca as an inspiration, not the basis of ptyro (7530f8c)
- docs: design spec, roadmap, README, MIT license (13591e0)

### Other

- H1: fix the findings of a full eight-way code review (security, crashes, correctness, build) (6aedfe7)
- Initial commit (e7be285)

