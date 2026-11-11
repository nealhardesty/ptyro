# ptyro review — missing features and what the future could hold

Reviewer lens: missing features (agent orchestration proper excluded). Brainstorm only; nothing was
built, run or changed. Checkout read: `.claude/worktrees/review-2026-11` at v0.0.68, 2026-11-11.

## 0. How this was produced, and what I did not get to

Read: `README.md` (feature sections), `docs/commands.md` (the whole CLI surface), `docs/compat.md`,
`SPEC.md` table of contents and §1, `ROADMAP.md` overview + every open (`[ ]`/`[~]`) task + the
Backlog, `RESEARCH.md` Part III in full (III.1, III.2, III.3). Maturity notes come from module
header comments, file sizes, grep counts for each capability and the README's own "Status" caveat.

Not done:
- **No web search.** Adjacent-tool expectations come from RESEARCH Part I/III and my own knowledge
  (cutoff mid-2026). Anything about a competitor's current state is unverified [U].
- **ROADMAP's ~295 done tasks were not read line by line**, only by milestone. An idea I mark
  **new** could be a sub-bullet of a done task; the R- ids and Backlog were checked for every idea.
- **SPEC §2–§24 not read** (10k lines). Where I say "the renderers never show X", it is from a
  code comment or a grep, cited.
- **Nothing was executed.** "Solid/thin/stub" is a reading judgement, not a test result.
- RESEARCH Parts I, II and the deep dives were not read; only the funnel.
- `docs/parity.md` waivers were not audited.

Key to status: **exists** · **planned** (ROADMAP id) · **proposed** (R- id, status `new`/partly) ·
**backlog** (ROADMAP Backlog bullet, no id) · **rejected** (III.3 or a row) · **new** (found in none).

---

## 1. Capability map (what a user can do today)

| Area | What exists | Maturity | Evidence |
|---|---|---|---|
| **Sessions / terminal** | Daemon-owned PTYs, attach/detach, exact-screen reattach, reboot survival, hibernate/resume, live daemon upgrade, recorder ring + `replay`/`export` (asciicast v2, text), copy mode with search, splits (binary tree, max panes), SHELLS (numbered workspace terminals, windows of up to 4 panes, watches for done/silence/match), quick terminal, OSC 133 shell integration auto-injected for bash/zsh/fish/pwsh | **Solid** on the core; emulator is `alacritty_terminal`, so **no images, no rendered hyperlinks, no ligature concerns (outer terminal's job)** | `ptyro-term/src/{emulator,record,sniff}.rs`, `ptyro-tui/src/{copy,layout}.rs`, `ptyro-daemon/src/shells.rs` (1.3k lines), `docs/compat.md` headless sweep. Emulator comment: "Our renderers never show hyperlinks" (`emulator.rs:521`). The outer-terminal matrix in `docs/compat.md` is entirely "—": untested by hand |
| **Agents** | Adapters for Claude (primary), Codex, Gemini, OpenCode, Antigravity, generic; hook relay; OSC 7501; status with `--explain`; turns; model in header (Claude, Codex); usage meters per profile; adopt a hand-started Claude; structured sessions (ACP / stream-json / app-server) with `convert`; `setup`/`install`/`update`/`login` bootstrap | **Solid for Claude on Linux**; **thin** for the rest (README: OpenCode, Gemini, Codex structured turns never run against the real thing; Antigravity built from docs) | `ptyro-agents/src/*`, `ptyro-drive/src/*`, `ptyro-daemon/src/{agents,usage,structured}.rs` |
| **Chats** | Own durable record in `chat.db`, tree with branches/forks/side chats/incognito, switch agent mid-chat, tool presets, FTS5 search + opt-in meaning search (model2vec, pure Rust), `chat find` (agent searches history), compare across agents, import of Claude/Codex/Gemini histories, export md/json, chat → task | **Solid and unusually deep** (the largest subsystem after the TUI: `ptyro-daemon/src/chats.rs` 4k, `ptyro-store/src/chat.rs` 2.2k, `ptyro-tui/src/chats.rs` 3.1k). No attachments (no image/file in a chat message: no hit for upload/attach in `web/src/chats.ts`). Cross-machine merge is CHAT6, open |
| **Review** | Diff inspector, line comments batched to the agent, commit as profile, PR via `gh`, checks, PR-event loop (CI failures and review comments back to the agent), automerge, review queue with evidence, checkpoints per turn, radar, leases | **Solid for GitHub only**. No GitLab/Forgejo/Bitbucket (`glab`/gitlab: zero hits in the daemon). Tested against `fake-gh`, "not the real `gh`" (SPEC §20.6b heading) | `ptyro-daemon/src/{github,review,evidence}.rs`, `ptyro-tui/src/inspector.rs`, `web/src/review.ts` |
| **Projects / git** | Auto-registered projects, workspaces, safe worktrees (archive/restore/gc, never lose work), `.worktreeinclude`, lifecycle scripts with trust, port allocation + listening-port detection, profiles (accounts, git author, ssh, secrets), competitor config import, change sets across repos | **Solid**. Ports are **detected and listed, never forwarded or opened** (`ptyro-pty/src/ports.rs`, 181 lines; no `forward`/`portal`/`preview` anywhere). Profile isolation is "not a boundary" (ROADMAP H2.5, open) |
| **Remote / mobile** | `--remote ssh://` for every verb, `serve` over HTTPS with own CA, QR pairing with scoped device tokens, PWA with a real canvas terminal (keys, mouse, selection, find, pinch zoom), web replay with slider, Web Push with Allow/Deny buttons from the notification, many machines federated in one sidebar (MM1.B), Tailscale cert | **Solid in tests, thin in the field** (README: "the phone app on a real phone" not yet run; H2.9 lists unverified hardware). No E2E relay, no mDNS, no passkeys, no share links, no presence | `ptyro/src/serve.rs`, `ptyro-daemon/src/{push,devices,machines}.rs`, `web/src/{term,sw}.ts` |
| **Notifications** | Inbox, mute, quiet hours, desktop, OSC 9/777, sinks (ntfy, Pushover, Slack, Discord, Telegram, command) with signed single-use approve/deny links, Web Push | **Solid, one-way**. No digest, no snooze, no two-way chat-ops, no actionable desktop notifications | `ptyro-daemon/src/notifier.rs` (1.1k) |
| **Config** | One schema, every option editable in TUI, web and `config set`; layering with `--explain`; `config schema` as JSON; themes; key bindings | **Solid** (`ptyro-core/src/settings.rs` 2k, `ptyro-tui/src/settings.rs` 2.9k, `web/src/settings.ts`) |
| **Scripting / API** | `send`/`wait`/`read`/`events -f`/`status`/`answer`/`report`, selectors and globs, `--json` shapes documented, `ptyro mcp` for leads, WebSocket protocol + `POST /api/request` | **Solid CLI; the wire protocol is not a public API** (no published schema, no SDK, no inbound webhooks, no user event hooks) | `ptyro/src/script.rs`, `docs/cli-json.md`, `ptyro-protocol/src/lib.rs` (2.7k) |
| **AI of ptyro's own** | Opt-in one-line session summaries from a small model CLI with secret redaction and a daily cap; chat titles; meaning search | **Thin by design** (SPEC §1.2: ptyro never calls an LLM API itself) | `ptyro-daemon/src/summary.rs` (327), `ptyro-meaning` |
| **Platform** | Linux and macOS; Windows 11 native daemon on ConPTY + named pipes, WSL2 bridged; installers (`install.sh`, `install.ps1`), Homebrew formula, SBOM, verified self-update, `uninstall`, `backup` | **Linux solid; macOS "not by hand"; Windows built, hand checks open** (WIN0/2/3/8/12/14 unticked). `backup` has **no `restore` command** (docs/uninstall.md: "Restore a backup by hand") | `packaging/homebrew`, `scripts/`, ROADMAP WIN |
| **Onboarding** | `init`, `setup` checklist, `tour` with a fake agent, health badge with one-key fixes, which-key, row menu, creation forms | **Solid** |
| **Accessibility / i18n** | Some `aria-`/`role=` in 13 web files; web terminal is a canvas with no text view (H2.8 open). No i18n | **Stub** |

---

## 2. Gap analysis against adjacent tools

### 2.1 tmux / zellij / screen

| Expected | Status | Note |
|---|---|---|
| Panes and splits | exists | Splits are deliberately "a clone on request" (UX3.1); SHELLS windows cap at 4 panes |
| Named layouts / session templates | backlog ("Session templates"), proposed R-UX-07 | Promote: see M-06 |
| Copy mode, search in one pane's scrollback | exists | TUI `copy.rs`; web "Find in the history" |
| **Search across all sessions** | **new** | Only chats are searchable. See M-01 |
| Session resurrect | exists | Stronger than tmux-resurrect (native agent resume) |
| Synchronized panes | partly exists | `broadcast` is a one-shot CLI send; no live "type into all" mode. **new** as a TUI mode (M-08) |
| Plugins | backlog (WASM, "only if real demand"), proposed R-EXT-01 | |
| Status-line feed for other bars | backlog (`ptyro status-line`), R-EXT-04 merged | Cheap and unbuilt; see M-11 |
| tmux key preset, `import tmux` | proposed R-UX-42, R-UX-15 | |
| tmux `-CC` control mode | proposed R-EXT-03 | Moonshot |
| Popups, command language, `run-shell` hooks | non-goal (SPEC §1.2) / hooks proposed R-STA-06 | |
| Activity/silence monitors | exists (SH.5, SHELLS only) | Worktree shells "could follow" |
| Zoom a pane | exists (full-sized panels, UX3) | |

### 2.2 Modern terminals (Ghostty, WezTerm, kitty, Warp)

| Expected | Status | Note |
|---|---|---|
| Kitty graphics / sixel / iTerm images | backlog ("passthrough for single-client sessions"; libghostty-vt backend) | Web canvas would need its own image path: **new** part |
| OSC 8 hyperlinks | **new** | The emulator stores them and even uses the field for prompt marks; no renderer shows or opens them (`emulator.rs:519-522`) |
| Clickable `path:line:col` | proposed R-UX-14 | |
| OSC 133 semantic prompts | exists | Auto-injected; drives status and SH.5 |
| **Command blocks** (jump between commands, copy one command's output, rerun, share) | **new** | The marks are already in the grid. See M-02 |
| Kitty keyboard protocol | exists | Inner side answers; outer side used when present |
| Ligatures, fonts, GPU | n/a (not a terminal application, SPEC §1.2); the web terminal is the exception and has no font or ligature options: **new**, minor |
| Synchronized output (mode 2026) | exists (emulator) | |
| Quick terminal, command palette | exists | |
| Notifications on command finish | exists (SH.5); R-NOT-04 for worktree shells still `new` | |
| Warp-style "AI command" / explain error | **new** | Must go through registered agents (III.3 rejects raw provider keys). See M-47 |
| Themes / colour scheme import | partly exists (TUI themes, editable on web) | Import of iTerm/Ghostty/base16 schemes: **new** |

### 2.3 Remote and sharing tools

| Expected | Status | Note |
|---|---|---|
| Survive roaming / flaky links (mosh, ET) | proposed R-REM-10 | The daemon already owns state; the SSH transport is the weak part |
| Zero-config reach from anywhere (VS Code tunnels, Claude Remote Control) | backlog (E2E relay, SPEC §16.5 "much later") | Today: Tailscale or your own exposure. Biggest remote gap |
| Certificates nobody installs | planned TS1.2 | |
| Share a session read-only / multiplayer (sshx, upterm, tmate) | proposed R-REM-09 | Presence indicators: **new** |
| Port forwarding (VS Code "Ports" tab, tailscale serve) | proposed R-REM-06, R-WT-08 | Detection exists; forwarding does not |
| File transfer (scp, drag-drop in VS Code remote) | proposed R-REM-08 (phone → worktree only) | Download and general browse: **new** |
| Passkeys / biometric step-up | proposed R-REM-02 (remainder) | |
| mDNS discovery | proposed R-REM-02 (remainder) | |
| Multi-machine in one client | exists (MM1.B); planned MM1.A (alternative, not chosen) | |
| Native mobile app, widgets, live activities, watch | **new** | PWA only today |
| Voice input | **new** | |

### 2.4 Agent session managers (adjacent only)

| Expected | Status |
|---|---|
| Board / kanban view | proposed R-UX-08 |
| Tile / grid of all agents | proposed R-UX-10 |
| Cost and token dashboards | backlog (budgets), proposed R-OBS-01, R-USE-02, R-ORC-10 |
| Schedules, cron, self-wake | proposed R-RUN-02 |
| Issue intake beyond GitHub; "My Work" | proposed R-GIT-02 (partly built for gh), backlog "Tasks" |
| Morning digest | proposed R-NOT-06, R-ORC-31 |
| Browser preview / element picker | proposed R-EXT-02 |
| Sandboxed execution | backlog, proposed R-SEC-05 |
| GitLab / other forges | **new** |

### 2.5 Dev-environment tools

| Expected | Status | Note |
|---|---|---|
| devcontainers / containers per worktree | backlog; proposed R-SEC-05 | |
| direnv / mise / nix env per worktree | **new** | `setup` knows mise only as an installer for Node; sessions do not load `.envrc` or `mise.toml` unless the user's shell does. Agents launched directly (not through a shell) miss them |
| File browser and preview | **new** | Backlog has "Built-in editor" (back-burnered); read-only browse is a different, smaller thing |
| Diff viewer | exists | Unified only (no side-by-side seen; not verified in depth), no image diff |
| Log viewer for dev servers / run scripts | **new** | `wt run --attach` gives the raw terminal only |
| Editor integration (VS Code extension, open-in) | backlog; `wt open -e` exists | |
| Secrets manager integration (1Password, keychain, vault) | proposed R-PRF-03 (keyring); `command = [...]` exists | Brokering short-lived credentials: **new** |
| Jujutsu | backlog | |

---

## 3. Brainstorm: 74 future features

Each entry: **story** · surfaces (CLI / TUI / web) · size (S ≤ 1 day, M 2–4 days, L 1–2 weeks,
XL more) · builds on · risks · status.

### Theme A — The terminal knows more (search, blocks, time)

**M-01 Search everything on screen, ever.**
Story: "Which session printed that stack trace yesterday?" answered in one query across every
session's screen, scrollback and recording.
CLI `ptyro grep PATTERN [SESSIONS…] [--since 2d] [--json]` · TUI a "search all sessions" picker
whose hits jump into copy mode at the line · web a search box on the sessions page.
Size M. Builds on `ptyro-term/src/record.rs` (the recorder already serves as "scrollback deeper than
memory"), `ptyro-tui/src/copy.rs` search, the `read --scrollback N` path.
Risks: the owner's rule that terminal contents never enter a database rules out an FTS index, so
this is a streaming scan of the recording rings rendered through the emulator; needs a time budget
and a per-session cap. Federated machines must answer for themselves.
Status: **new** (chat search exists; nothing for terminals).

**M-02 Command blocks.**
Story: in any shell I jump command to command, copy just the last command's output, rerun it, or
hand "this command and its output" to an agent.
CLI `ptyro read S --block -1 [--json]` (command, exit code, duration, output), `ptyro blocks S` ·
TUI `[`/`]` in copy mode jump between prompts, `y` on a block copies its output, row menu
"send block to agent…" · web tap a prompt mark for the same menu.
Size M. Builds on the OSC 133 marks already kept in the grid (`emulator.rs` `MARK_URI`),
`ptyro-core/src/shell.rs`, SH.5's exit-code tracking.
Risks: marks "shift once history is full" (code comment); full-screen programs have no blocks;
agents' TUIs are not shells. Keep it to shell sessions.
Status: **new** (R-NOT-04 and SH.5 cover only "command finished").

**M-03 Timeline scrubber with markers.**
Story: I scrub a session's recording and see turn boundaries, status changes, permission prompts,
checkpoints and commits as ticks, and can restore the worktree to the checkpoint under the cursor.
CLI `ptyro replay S --at turn:7`, `ptyro export S --format asciicast3` with markers · TUI a replay
view with a marker bar · web: the existing `#/replay/` slider gains ticks and a "restore here" button.
Size M. Builds on `web/src/sessions.ts` replay, `ptyro/src/record.rs`, session events, checkpoints.
Risks: clock alignment between the recording's `at_us` and event timestamps; ring loses its start.
Status: backlog ("Timeline scrubber"); R-OBS-03 (asciicast v3 markers) proposed.

**M-04 Links you can open.**
Story: I click or press a key on a URL, an OSC 8 link or `src/foo.rs:42:7` in agent output and it
opens in the browser, my editor, or the inspector.
CLI `ptyro read S --links --json` · TUI hint mode (letters over every link, like kitty hints) and
mouse click · web tap (URL detection `urlAt` exists in `web/src/grid.ts`; extend to OSC 8 and paths).
Size S–M. Builds on alacritty's stored hyperlinks, `ptyro-core/src/open.rs`, `wt open -e`.
Risks: path resolution relative to the session's cwd (OSC 7 gives it); the prompt-mark trick uses
the hyperlink field and "ends a program's own hyperlink open at that moment".
Status: R-UX-14 proposed for paths; OSC 8 rendering and hint mode **new**.

**M-05 Inline images.**
Story: an agent's screenshot, a plot, or `chafa`/`icat` output shows in the pane and in the web terminal.
CLI `ptyro read S --images` lists and saves them · TUI passthrough to an outer terminal that
supports kitty graphics or sixel, placeholder otherwise · web draws them on the canvas.
Size L. Builds on `sniff.rs` (already sees bytes before the emulator), frame protocol.
Risks: multi-client sessions with mixed capability; scroll and resize bookkeeping; recording size;
alacritty_terminal has no image model (the libghostty-vt backend is the cleaner route).
Status: backlog (kitty graphics/sixel passthrough, libghostty-vt); the web half is **new**.

**M-06 Session templates and named layouts.**
Story: "dev" gives me agent + dev server + test watcher in my arrangement, per worktree, in one key.
CLI `ptyro layout apply dev [--wt W]`, `layout save NAME`, `layout ls` · TUI in the creation form
as a pick; `C-q` palette "save this layout" · web a "Start from layout" pick on the worktree page.
Size M. Builds on `ptyro-tui/src/layout.rs`, SHELLS windows (daemon-kept layouts), `[scripts]`.
Risks: overlaps recipes; keep it declarative (no logic). Settings schema entry required (CFG1).
Status: backlog ("Session templates"), R-UX-07 merged, R-DD-13 (`layout export|apply`) proposed.

**M-07 A shareable replay file.**
Story: I send a colleague one self-contained HTML file that plays a session back, no ptyro needed.
CLI `ptyro export S --format html [-o F] [--from D --to D]` · TUI row menu "Export…" form gains the
format · web "Download replay" on the replay page.
Size S–M. Builds on the web's `Screen` renderer and `record.rs`.
Risks: secrets in recordings (pair with M-53 redaction); file size.
Status: R-OBS-03 (html) proposed.

**M-08 Type into several panes at once.**
Story: I toggle "sync" and what I type goes to every pane in the window (tmux `synchronize-panes`).
CLI exists as `broadcast` (one-shot) · TUI a toggle with a loud header badge · web the same toggle
on a SHELLS window.
Size S. Builds on `layout.rs`, input fan-out in `ptyro-daemon/src/session.rs`.
Risks: dangerous by nature; never include agent sessions waiting on a permission prompt.
Status: **new** as a live mode.

**M-09 Screenshot a session.**
Story: I paste a crisp SVG/PNG of a pane into a PR or a bug report.
CLI `ptyro screenshot S [-o F] [--format svg|png|html|ansi]` · TUI row menu · web "Save image".
Size S. Builds on `frame.rs`. Risks: fonts in PNG (SVG first).
Status: R-UX-12 proposed.

**M-10 Bookmarks and notes on the timeline.**
Story: I mark "here it went wrong" in a session and later jump back to it, or cite it in the scratchpad.
CLI `ptyro mark S [TEXT]`, `marks S` · TUI a key in copy mode · web long-press in the history.
Size S. Builds on session events, scratchpad stamps (`ptyro-core/src/scratchpad.rs`).
Risks: one more noun; keep marks as events, not a new store.
Status: **new** (R-UX-06 is a one-line agent note, a cousin).

### Theme B — Files, ports and the dev environment

**M-11 Status feed for other bars.**
Story: my tmux/starship/Waybar/SwiftBar shows "2 blocked · 5 working" and the focused agent's state.
CLI `ptyro status-line [--format tmux|starship|json|waybar]` · TUI: a waiver is honest here (it is
another program's tool), plus an About line that shows the snippet · web: Health page shows the
snippet to copy.
Size S. Builds on `status`, `events -f`. Also re-emit OSC 9;4 / 7501 outward (R-STA-07).
Risks: polling cost; give it a cached file or a `--follow` mode.
Status: backlog (`ptyro status-line`), R-EXT-04/R-WIN-20 merged, R-STA-07 partly built.

**M-12 Port portals: open a worktree's dev server from anywhere.**
Story: on my phone I tap the port a worktree's dev server listens on and see the app, through
ptyro's own HTTPS and device token.
CLI `ptyro ports [W]`, `ptyro ports open W 5173`, `ports share … --ttl` · TUI inspector Ports tab
(exists) gains "open" and "forward" · web a Preview button per port, in an iframe or new tab.
Size L. Builds on `ptyro-pty/src/ports.rs`, port allocation (SPEC §10.7), `serve.rs`, device scopes.
Risks: the largest security surface in this list: a reverse proxy to arbitrary local services
behind a device token; cookies and origins (needs a per-port subdomain or path rewriting, which
breaks many dev servers); WebSocket/HMR passthrough; federation hop for other machines. Needs its
own scope (`preview`), default off.
Status: R-REM-06 and R-WT-08 proposed.

**M-13 Read-only file browser and preview.**
Story: from the phone I look at the README the agent just wrote, or an image it produced, without
attaching a shell.
CLI `ptyro files W [PATH] [--json]`, `ptyro cat W PATH` · TUI an inspector "Files" tab (changed
files first) with a pager · web a Files tab with Markdown, image and syntax-coloured preview.
Size M. Builds on `inspector.rs`, `web/src/review.ts`, `ptyro-git`.
Risks: drifts toward the back-burnered editor (SPEC §1.2). Hold the line: read-only, "open in
editor" for writes. Respect `.gitignore` and never serve outside the worktree.
Status: **new**.

**M-14 Drop a file or photo into a worktree.**
Story: I photograph a whiteboard on my phone and the agent gets its path in the prompt.
CLI `ptyro put W FILE [--prompt]` (also over `--remote`) · TUI drag-and-drop path / paste detection
in the task box · web a share target (one exists for notes in `web/src/notes.ts`) and an attach button.
Size M. Builds on the web share target, `.ptyro/` conventions, `send`.
Risks: size limits, where files land (git-excluded inbox dir), device scope (`control` at least).
Status: R-REM-08 proposed.

**M-15 Paste an image into a remote agent.**
Story: Ctrl-V of a screenshot works in Claude over ssh, the web and federated machines, where the
agent's own clipboard read fails.
CLI `ptyro paste S --image FILE` · TUI intercept image paste where the outer terminal exposes it
(kitty clipboard protocol / OSC 5522), else a key that takes a file · web paste handler on the
terminal uploads and types the path.
Size M. Builds on M-14's upload path, adapter knowledge of how each agent takes an image.
Risks: per-agent behaviour differs; terminals expose image clipboards unevenly.
Status: **new**.

**M-16 Download what the agent made.**
Story: I pull a build artifact, a log or a generated PDF from a worktree to my phone or laptop.
CLI `ptyro get W PATH [-o F]` · TUI Files tab "save to…" · web a Download button; attach to the PR
body optionally.
Size S (after M-13). Builds on M-13.
Risks: same boundary rules as M-13.
Status: R-GIT-05 (artifacts on the PR) proposed; download **new**.

**M-17 Worktree environments: direnv, mise, nix, `.env`.**
Story: every session and script in a worktree gets the environment its `.envrc`/`mise.toml`/flake
defines, including agents that ptyro launches without a shell.
CLI `ptyro wt env W [--explain]`, `project config --explain` shows the source · TUI a line in the
worktree overview and a health finding ("this repo has .envrc, not allowed yet: press a") · web
the same finding with a button.
Size M. Builds on SPEC §10.6 environment, the trust dialog (`project trust`), `suggest.rs`.
Risks: direnv is code execution: route it through the existing trust model. Caching and staleness.
Status: **new**.

**M-18 Container or sandbox per session.**
Story: I run an untrusted repo's agent in a container with only its worktree mounted.
CLI `ptyro new --sandbox docker|bwrap|seatbelt|devcontainer` · TUI and web: an "Runs in" pick in
the creation form · settings: `sandbox.default`, per project.
Size L–XL. Builds on `SpawnSpec`, profiles, `doctor`.
Risks: agent logins and hooks inside the box; macOS file-sharing speed; port mapping; Windows.
Status: backlog ("Container/sandbox sessions", "Sandbox per profile"), R-SEC-05 merged, H2.5 open.

**M-19 A log viewer for dev servers and scripts.**
Story: a run script's output is filterable by level, errors are collected, and "send the last
error to the agent" is one key.
CLI `ptyro logs W [SCRIPT] [--errors] [-f] [--json]` · TUI a Logs tab in the inspector · web the same.
Size M. Builds on `wt run`, recorder, SHELLS watches (`--match`).
Risks: log format guessing; keep heuristics labelled low confidence (SPEC principle).
Status: **new**.

**M-20 Editor companions.**
Story: VS Code/Zed/JetBrains show the ptyro sessions for the folder I have open, with status, and
"open worktree here".
CLI exists (`wt open -e`, `ls --json`) · TUI exists · web deep links `vscode://`.
Size M (one extension), then per editor. Builds on the CLI JSON only.
Risks: a second codebase and marketplace to maintain.
Status: backlog ("Editor integrations").

**M-21 Other forges: GitLab, Forgejo/Gitea, Bitbucket.**
Story: `pr create`, checks, the PR-event loop and `--issue` work on my GitLab merge requests.
CLI unchanged verbs, a `forge` detected from the remote · TUI and web unchanged, labels say "MR".
Size L. Builds on `ptyro-daemon/src/github.rs` (921 lines, gh-only) behind a small trait.
Risks: `glab`'s JSON is thinner than `gh`'s; testing needs a `fake-glab`. The abstraction is
justified only once a second forge exists.
Status: **new**.

**M-22 Jujutsu workspaces.**
Story: my jj repo gets workspaces instead of git worktrees. Size L. Status: backlog. (Listed for
completeness; low priority while git covers the owner's use.)

### Theme C — Reach: remote, phone, sharing

**M-23 The E2E relay.**
Story: I pair my phone once and reach my agents from any network with no Tailscale, no port
forward and no certificate to install; the relay sees only ciphertext.
CLI `ptyro relay on|off|status`, `pair --relay` · TUI a row in Health and the pairing form · web
connects through the relay transparently.
Size XL. Builds on device tokens, the framed protocol, `push.rs` crypto.
Risks: it is a hosted service (cost, abuse, uptime) unless self-hosted only; the owner already
flagged "means running a service: the owner decides" for TS1.2's name service.
Status: backlog ("E2E relay", SPEC §16.5), R-REM-02 remainder.

**M-24 Share a session.**
Story: I give a colleague a link that shows one session read-only for an hour, or lets them type.
CLI `ptyro share S [--readonly|--steer] [--ttl 1h]`, `share ls`, `share revoke` · TUI row menu
"Share…" form with a QR · web Share button, and the guest's stripped single-session page.
Size M. Builds on `--view-only` attach, scoped device tokens (`devices.rs`), `#/s/<id>` URLs.
Risks: needs reachability (M-23 or Tailscale Funnel); secrets on screen; a guest must never reach
other sessions or the API.
Status: R-REM-09 proposed.

**M-25 Presence.**
Story: I can see that my phone and my desktop are both attached to this session, who is typing,
and which client's size is in force.
CLI `ptyro clients [S] [--json]`, `clients detach ID` · TUI a small "2 viewers" chip in the pane
header · web the same chip, tap to list.
Size S. Builds on `conn.rs`, sizing across clients (SPEC §5.8), devices.
Risks: none much; it also explains "why is my pane small".
Status: **new**.

**M-26 Passkeys and step-up.**
Story: approving a risky command from my phone asks for Face ID; pairing a browser uses a passkey.
CLI `devices` shows which have one; `[remote] step_up = risky|always|never` · TUI settings · web
WebAuthn prompts.
Size M. Risks: WebAuthn needs a stable origin (rpId), awkward with IPs and the local CA.
Status: R-REM-02 remainder.

**M-27 Find my daemon on the LAN.**
Story: the PWA and `machine add` list ptyro daemons on this network; I pick one instead of typing an address.
Size S. Risks: mDNS in browsers is not available; this helps the CLI/TUI and native shells only.
Status: R-REM-02 remainder.

**M-28 Roaming-proof remote TUI.**
Story: my laptop sleeps, changes network, wakes, and the remote TUI is simply still there.
CLI/TUI `--remote` reconnects with sequence numbers and a frame diff; optional local echo · web
already reconnects.
Size M. Builds on `ptyro-client/src/remote.rs`, row versioning (SPEC §5.4).
Status: R-REM-10 proposed.

**M-29 Voice.**
Story: walking the dog, I hold a button and say "tell the fixer to rebase and rerun the tests".
CLI `ptyro send S --stdin` already takes text from any dictation tool; document it · TUI: a
`voice.command` setting that runs a local transcriber (whisper.cpp) and pastes · web a push-to-talk
button on the key row and the chat box.
Size S (web, OS dictation / Web Speech) to M (local transcriber command).
Risks: Web Speech sends audio to the browser vendor; say so, and prefer the OS keyboard's dictation,
which needs nothing. Never call a speech API from the daemon (SPEC §1.2 spirit).
Status: **new**.

**M-30 Native phone shells.**
Story: my lock screen shows a live activity "3 working · 1 needs you", my watch taps when an agent
is blocked, a widget lists the inbox.
CLI n/a · TUI n/a · web is the content; the shell is a thin wrapper (Capacitor/Tauri) adding APNs/
FCM, widgets, live activities, a watch complication.
Size XL. Builds on the PWA and `push.rs`.
Risks: app-store accounts and review; APNs needs a relay server holding keys (conflicts with "no
service"); two more platforms. A waiver-shaped feature: the CLI and TUI have nothing to mirror.
Status: **new**.

**M-31 Offline-first web.**
Story: on the train with no signal, the app opens with the last known state, I can read finished
answers and the diff, and what I queue (notes, replies, approvals that are still valid) sends on reconnect.
Size M. Builds on `sw.ts` generations and the notes queue in IndexedDB (`web/src/notes.ts`).
Risks: stale approvals (the `since_ms` guard exists for push answers; reuse it); storing terminal
text on the device.
Status: **new** (notes already do it).

**M-32 Many machines without a hub.**
Status: **planned MM1.A** (the alternative not chosen). Worth doing only for the "no always-on
daemon" case. Size M.

**M-33 Move a session to another machine.**
Story: I leave the desk; the task continues on the server, same conversation, same id.
CLI `ptyro handoff S --to MACHINE` · TUI row menu "Move to…" (a pick of linked machines) · web the same.
Size L. Builds on `machines.rs` federation, `handoff.rs`, archives.
Status: R-REM-05 merged into backlog "Cloud/SSH hosts"; R-DD-16 (`worktree pull`) proposed.

**M-34 Restore and migrate.**
Story: on a new laptop, `ptyro restore backup.tgz` (or `ptyro migrate --from old-laptop`) brings
config, profiles' metadata, projects, the scratchpad and chats.
CLI `ptyro restore FILE [--dry-run]`, `migrate --from M` · TUI first-run screen offers "restore a
backup / copy from a linked machine" · web the same on `#/setup`.
Size S–M. Builds on `ptyro/src/backup.rs`, `machines.rs`.
Risks: path differences between machines; never import credentials (logins are per machine).
Status: **new** (`backup` exists; restoring is "by hand" in docs/uninstall.md). CHAT6 covers chats.

**M-35 Kiosk / wall view.**
Story: a spare monitor shows every agent as a tile with its state and last line, read-only, no login prompts.
CLI `ptyro pair --scope read --kiosk` · TUI `ptyro attach --wall` (auto-grid of sessions in a
status) · web `#/wall`, large type, auto-rotate through blocked sessions.
Size M. Builds on the read scope, `layout.rs`, frames.
Status: R-UX-10 (tile view) proposed; kiosk mode **new**.

### Theme D — Attention that respects you

**M-36 The digest.**
Story: at 8:30 I get one message: what finished overnight, what is blocked, what to review first,
what it cost.
CLI `ptyro digest [--since 12h] [--json]` · TUI a "While you were away" screen on first attach
after an absence · web the same card on the inbox, and it goes to the sinks on a schedule.
Size M. Builds on events, `review queue`, `usage.rs`, `summary.rs` (optional prose), `notifier.rs`.
Risks: prose needs the opt-in summariser; the deterministic version (counts and lists) must stand alone.
Status: R-NOT-06 and R-ORC-31 proposed.

**M-37 Snooze, and done-but-unseen.**
Story: I snooze a question for 30 minutes; a finished session stays marked until I actually look at it.
CLI `ptyro inbox snooze ID 30m` · TUI `z` in the inbox · web swipe.
Size S. Status: R-UX-02 remainder, R-STA-04 proposed.

**M-38 Focus-aware quiet.**
Story: ptyro stays quiet while my OS is in Do Not Disturb or my calendar says I am in a meeting,
and catches me up after.
CLI `ptyro mute --until-free`; settings `notifications.respect_focus`, `notifications.calendar`
(an ICS URL or a command) · TUI and web: the settings, and a "quiet until 14:00 (meeting)" chip.
Size S (OS focus) to M (calendar). Builds on `notifications.quiet_hours` (`settings.rs:719`).
Risks: an ICS URL is a secret; a command source keeps ptyro out of OAuth.
Status: **new**.

**M-39 Actionable desktop notifications.**
Story: the macOS/Linux/Windows notification has Allow / Deny / Reply. Size M.
Status: R-NOT-02 proposed (Web Push already has the buttons in `sw.ts`; Windows toasts built in WIN10).

**M-40 Two-way chat-ops.**
Story: I answer an agent's question by replying in the Telegram/Slack/Discord thread the alert came in.
CLI `ptyro notify bridge ls|test` · TUI and web: a sink's settings gain "accept replies".
Size M–L. Builds on `notifier.rs` sinks and signed links.
Risks: inbound needs polling (Telegram long-poll and Slack socket mode need no public endpoint;
webhooks do). A reply is remote input: authenticate the sender id, log it, scope it.
Status: **new** (sinks are one-way today); R-DD-18 names ccgram interop.

**M-41 Your own hooks.**
Story: when any session goes blocked, run my script; when a PR merges, post to my channel.
CLI `ptyro on EVENT [--when EXPR] -- CMD`, `hooks ls|rm`; `[hooks]` in config · TUI and web: a
Hooks collection in Configuration with a test button.
Size M. Builds on the event stream, `ptyro-run/src/expr.rs` for `--when`, the `command` sink.
Risks: scripts in committed `.ptyro.toml` need the trust dialog.
Status: R-STA-06 proposed.

**M-42 Webhooks in and out.**
Story: CI or a cron elsewhere POSTs to ptyro to start a task; ptyro POSTs signed events to my endpoint.
Size M. Builds on `serve.rs`, HMAC from `notifier.rs`.
Status: R-RUN-03 proposed (orchestration-adjacent; the transport is mine, the triggers are not).

**M-43 Badges.**
Story: a long build sets a badge on its row that clears when the process exits. Size S.
Status: R-NOT-05 proposed.

### Theme E — Insight and ptyro's own (borrowed) intelligence

**M-44 Cost, tokens and context at a glance.**
Story: I see today's spend per profile, per project and per session, context-window fill per
agent, and when I will hit the 5-hour cap.
CLI `ptyro usage --by session|project|day [--json]` · TUI a Usage screen (About shows limits
today) and a context % column · web `#/usage` (exists) grows charts.
Size M. Builds on `usage.rs` (already parses `cost.total_cost_usd` and `context_window`), chat
`tokens` fields in `ptyro-core/src/chat.rs`.
Risks: cost is trustworthy only where the agent reports it; label the rest "unknown", never estimate silently.
Status: backlog (budgets, per-profile meters), R-ORC-10, R-USE-02, R-OBS-01 proposed.

**M-45 How are my agents doing?**
Story: over a month: time working vs waiting on me, median time I took to answer, PRs merged per
agent, how often verified-done bounced, which agent wins fan-outs.
CLI `ptyro stats [--since 30d] [--json]` · TUI a Stats screen · web `#/stats`.
Size M. Builds on events and run history (metadata, so it may live in `ptyro.db`).
Risks: vanity metrics; lead with "time you were the bottleneck".
Status: R-OBS-02 proposed.

**M-46 Catch me up.**
Story: I open a session I left two hours ago and see, above the screen, what changed since I last
looked: turns, files touched, questions asked and answered, the diff stat.
CLI `ptyro status S --since-seen` · TUI a dismissible strip on focus · web the same strip.
Size M. Builds on turns (SPEC §9.3b), checkpoints, events, "last focused" per client, `summary.rs`.
Risks: "seen" is per person, not per client; decide which.
Status: **new** (nearest: R-STA-04, UX1.16 context hints).

**M-47 Ask about what is on screen.**
Story: I select an error, press a key, and a side chat explains it, with the last command and the
worktree as context; "fix it" turns the answer into a prompt for the session's agent.
CLI `ptyro shell --read | ptyro chat ask "why did this fail"` works today; add `ptyro explain S
[--block -1]` · TUI a key in copy mode and on a shell row whose command failed · web a button on a
selection.
Size S. Builds on `chat ask` (incognito, stdin as context), M-02 blocks, SH.5 exit codes.
Risks: sends terminal text to a model: use the session's profile and the redactor from `summary.rs`.
Stays inside SPEC §1.2 because the registered agent answers, not ptyro.
Status: **new**.

**M-48 Names, commit messages and PR bodies written for you.**
Story: branch names, commit messages and PR descriptions come drafted, on the profile's own account.
CLI `wt commit --draft`, `pr create --describe` · TUI the commit/PR forms arrive pre-filled · web the same.
Size S. Builds on `summary.rs`'s runner, `--ask-agent`.
Status: R-UX-01 remainder, R-GIT-06 proposed.

**M-49 A local model for the small jobs.**
Story: summaries, titles and "explain this" run on Ollama/llama.cpp on my machine, free and private.
CLI `ptyro config set summaries.command …` works today for any command; add presets (`ollama`,
`llm`) and detection · TUI and web: a pick in Configuration with "found on this computer".
Size S. Builds on `[summaries] command`.
Risks: quality of small local models on terminal text; keep Haiku the documented default.
Status: **new** (R-PRF-04 covers proxy profiles for the agents themselves).

**M-50 Redaction everywhere.**
Story: tokens and profile secrets never appear in exports, shared sessions, screenshots, replays
or notifications.
CLI `export --redact`, on by default for `share` and `export --format html` · TUI and web: a
setting, and a "redacted" chip on shared views.
Size S–M. Builds on the redactor in `summary.rs` ("the profile's secrets and anything that looks
like a token replaced").
Risks: false confidence; call it best effort in the UI.
Status: **new**.

**M-51 Semantic status of a shell.**
Story: a shell row says "tests: 3 failed" or "server: listening on 5173" rather than "running".
CLI `status --json` gains a `gist` · TUI/web the row's second line.
Size M. Builds on manifests-as-data (R-DD-05), SHELLS watches, ports.
Risks: scraping; SPEC says heuristics are a labelled fallback. Do it as opt-in matchers per
`[scripts.*]`, not guesses.
Status: **new** (R-DD-05 is the mechanism).

**M-52 Workspace memory.**
Status: R-CHAT-16 proposed; III.3 rejects a vector-DB memory layer. Keep to a notes file.

### Theme F — Credentials, safety, audit

**M-53 Keychain and secret-manager backends.**
Story: profile secrets come from the OS keychain, 1Password, `pass` or Vault, never from a file.
CLI `profile secret set|ls|rm`, `--from keychain|op|pass` · TUI/web: a pick on each secret in the
profile form, with a "test" button.
Size S–M. Builds on profiles' `command = [...]` secrets.
Status: R-PRF-03 proposed (keyring).

**M-54 Credential broker.**
Story: an agent gets a GitHub token scoped to one repository that expires when the session ends;
no long-lived token sits in its environment.
CLI `profile broker …`; sessions see a credential helper, not a token · TUI/web: profile form.
Size L. Builds on profiles, `ptyro hook` relay pattern (a git credential helper and an `SSH_AUTH_SOCK`
proxy that asks the daemon), approvals.
Risks: every forge mints tokens differently (GitHub Apps vs fine-grained PATs); easy to get subtly
wrong. Start with an approval-gated ssh-agent proxy and git credential helper only.
Status: **new**.

**M-55 Profiles as a real boundary.**
Status: **planned H2.5** (open), backlog "Sandbox per profile". Size L. The honest prerequisite for
anyone using profiles to separate an employer's account from a personal one.

**M-56 The audit log.**
Story: "what did I approve from my phone last Tuesday, and what typed into that session?"
CLI `ptyro log [--since] [--device D] [--json]` · TUI an Audit tab in the inbox · web `#/log`.
Size S. Builds on the `remote` events already in each session's log.
Status: R-SEC-02 remainder.

**M-57 One permission dial across agents.**
Story: read-only / ask / auto as one setting that maps to each agent's flags, switchable live.
Size M. Status: R-SEC-03, R-ORC-36 proposed (orchestration-adjacent; noted, not mine).

### Theme G — Extensibility and API

**M-58 A published, versioned API with small SDKs.**
Story: I write a 20-line TypeScript or Python script that subscribes to events and answers prompts.
CLI `ptyro api schema --json`, `api token new --scope` · TUI/web: tokens appear with devices.
Size M. Builds on `ptyro-protocol` (2.7k lines, already versioned by `PROTO`), `/api/request`.
Risks: freezing a protocol at 0.0.x is a tax on every later change. Publish the schema as
"unstable, versioned" first; promise stability at 1.0.
Status: R-REM-07, R-DD-11 proposed.

**M-59 Provider plugins (stdio JSON).**
Story: a small program of mine feeds a read-only panel (deploy status, a queue depth) into the
sidebar and the web.
CLI `ptyro plugin add|ls|rm` · TUI a pane kind · web a card.
Size M. Risks: every plugin surface must exist in three clients; that triples the contract.
Status: R-EXT-01 proposed; WASM in backlog "only if real demand appears".

**M-60 JSON Schema for the config files.**
Story: my editor completes and validates `config.toml` and `.ptyro.toml`. Size S.
Builds on `config schema` (exists, ptyro's own shape). Status: R-UX-05 proposed.

**M-61 Browser bridge.**
Story: I click an element in the previewed app and the agent gets its selector and a screenshot.
Size L. Builds on M-12. Status: R-EXT-02 proposed.

**M-62 tmux control-mode facade.**
Story: iTerm2 or WezTerm shows ptyro sessions as native tabs and splits. Size L.
Status: R-EXT-03 proposed.

### Theme H — Platform, packaging, polish

**M-63 Everywhere installable.**
Story: `nix run`, `apt install`, `winget install`, an AUR package, and `docker run` for a headless
daemon on a server.
CLI/TUI/web unaffected; `about` names the installer so `update` uses the right one.
Size M. Builds on `scripts/dist.sh`, `packaging/homebrew`, BOOT6 self-update.
Status: WIN12 planned (winget, scoop, signing); nix, apt/rpm, AUR, docker **new**.

**M-64 Update channels.**
Story: I opt into `beta` on one machine and stay on `stable` on the server; federated machines
that "must run the same version" warn before I split them.
CLI `config set updates.channel beta` · TUI/web: a pick in Configuration → Updates.
Size S. Builds on `ptyro-agents/src/{own,updates}.rs`.
Risks: MM1's same-version rule (`docs/commands.md`: "they must run the same version") makes
channels and federation fight; relaxing it to "same protocol minor" is the real work.
Status: **new**.

**M-65 A text view for screen readers, and a plain mode.**
Story: with VoiceOver I hear "fixer is blocked: wants to run npm test" and can read the screen as text.
CLI `--plain` linear output exists in spirit (`read`, `status`); TUI a linear mode without the
sidebar chrome · web an off-screen live region mirroring the canvas, and status announcements.
Size M. Status: **planned H2.8**, R-UX-16 proposed.

**M-66 Colour-scheme import and web terminal fonts.**
Story: ptyro's panes and the web terminal use my Ghostty/iTerm/base16 scheme and my font.
CLI `config add themes NAME --from ghostty|iterm|base16 FILE` · TUI/web: "Import…" in Themes.
Size S. Builds on `theme.rs`, `web/src/color.ts`. Status: **new**.

**M-67 Plain multiplexer mode.**
Story: a sceptical tmux user gets sessions and splits and nothing else until they ask.
Size S. Status: R-UX-13 proposed; R-UX-15/R-UX-42 (tmux import and keys) alongside.

**M-68 Thousands of sessions.**
Story: a build farm of 2,000 hibernated and 200 live sessions lists instantly and idles near zero.
CLI `ls` paging and server-side filters · TUI/web virtualised lists.
Size M (measure) to L (fix). Builds on `scripts/bench.sh`, `soak.sh`, SPEC §18.3 budgets.
Status: **new** as a stated target; R-DD-17 has related ops lessons.

**M-69 Menu-bar / tray presence.**
Story: a menu-bar dot shows blocked count; clicking lists who needs me and opens the session.
Size S via M-11 (ship SwiftBar/xbar/Waybar/polybar/GNOME snippets); M for a real tray binary.
Status: **new** (depends on backlog `status-line`).

**M-70 Launcher actions.**
Story: Raycast/Alfred/rofi: "ptyro: new task…", "jump to blocked". Size S (scripts over the CLI). Status: **new**.

**M-71 Translations.**
Story: the UI in my language. Size XL (three clients, every string, every help text). Status:
**new**; see "never" list: not while strings change weekly.

**M-72 Chat attachments.**
Story: I drop an image or a file into a chat message.
CLI `chat send C --attach FILE` · TUI path paste · web attach/paste/drag.
Size M. Builds on M-14, drivers in `ptyro-drive`. Risks: `chat.db` holds text; files need a store
of their own and the "database holds metadata" rule needs a ruling.
Status: **new**.

**M-73 Recordings you can trust to be there.**
Story: I choose how much history each session keeps, pin a recording so the ring never drops it,
and see what recordings cost on disk.
CLI `ptyro recordings ls|pin|rm`, settings for the budget · TUI/web: a Recordings page, and `gc`
includes them.
Size S. Builds on `record.rs` ring. Status: **new** (the budget exists; managing it does not, as far as I read).

**M-74 The outer-terminal matrix, filled in by the tool.**
Story: `ptyro doctor --terminal` runs the hand checks (keys, OSC 52, mouse, kitty protocol, Alt
keys) interactively and writes my terminal's row of `docs/compat.md`.
Size S. Builds on `verify-shells`, `doctor.rs`, `outer.rs`.
Status: **new** as a feature (SH.0 and the empty compat grid are the open work it would close).

---

## 4. Ranking

### 4.1 Top 15 by user value × fit ÷ cost

| # | Id | Why |
|---|---|---|
| 1 | **M-36 Digest** | The stated product is "many agents, you are the bottleneck". One message that orders your morning is the highest-value thing ptyro does not do, and every input exists. M |
| 2 | **M-02 Command blocks** | The marks are already in the grid; this turns shell integration from a status source into a daily tool, and feeds M-47 and M-01. M |
| 3 | **M-01 Search every session** | Chats are searchable and terminals are not; with dozens of sessions, "where was that" is constant. M |
| 4 | **M-44 Cost and context dashboard** | Data already parsed in `usage.rs`; users of parallel agents ask about cost first. M |
| 5 | **M-14 + M-15 Files and images into a worktree** | The most common remote failure (pasting a screenshot) and the most natural phone action. M each |
| 6 | **M-11 Status feed (+ M-69 snippets)** | A day of work; makes ptyro visible when its TUI is not on screen. S |
| 7 | **M-47 Ask about what is on screen** | Nearly free on top of `chat ask`; the one "AI terminal" feature that respects SPEC §1.2. S |
| 8 | **M-04 Links you can open** | Table stakes in every terminal ptyro sits inside; currently swallowed. S–M |
| 9 | **M-34 Restore and migrate** | `backup` without `restore` is half a feature; multi-machine users need it. S–M |
| 10 | **M-25 Presence** | Small; explains sizing surprises and is the base of sharing. S |
| 11 | **M-37 Snooze / done-unseen** | Small inbox quality that compounds daily. S |
| 12 | **M-06 Session templates** | Already in the backlog, already asked for by the layout machinery; removes repeated setup. M |
| 13 | **M-46 Catch me up** | Makes context switching between many agents cheap; the meaning layer's best use. M |
| 14 | **M-12 Port portals** | Very high value for web developers on a phone; ranked lower only for its security cost. L |
| 15 | **M-50 Redaction everywhere** | A prerequisite for M-07, M-24 and anything shared; small. S–M |

Just outside: M-03 scrubber markers, M-41 hooks, M-56 audit log, M-13 file browser, M-21 other forges
(high value for a GitLab user, zero for others).

### 4.2 Five moonshots

1. **M-23 E2E relay** (with M-24 sharing on top): ptyro reachable from anywhere with nothing to set up.
   Changes who can use the phone app at all.
2. **M-30 Native phone shells**: live activities, watch taps, widgets. The agent fleet on your wrist.
3. **Burst to cloud** (backlog "Cloud/SSH hosts", R-ORC-17, M-33): `ptyro new --on cloud` starts an
   ephemeral VM or container running a ptyro daemon that federates in through MM1 and is torn down
   when its worktree is pushed. Federation makes this more plausible here than elsewhere.
4. **M-18 + M-54 The safe box**: every session in a sandbox with brokered, expiring credentials,
   so "full auto" is something you can leave running overnight without a second thought.
5. **Time-travel workspace** (M-03 + M-01 + checkpoints): scrub any session to any moment, see the
   screen, the diff and the conversation as they were, and fork a new worktree from there.

### 4.3 Five things ptyro should deliberately never do

1. **Call model APIs itself or hold provider keys.** SPEC §1.2 ("it never calls an LLM API itself")
   and III.3 ("a chat client for raw provider APIs and keys … would be a second product"). Every AI
   feature above runs a registered agent or a user-named command on the profile's own account.
2. **Become a terminal emulator application or a full tmux.** SPEC §1.2, both. No GPU renderer, no
   font stack in a native window, no tmux command language. The web terminal is a remote view, not
   a product; resist ligature and font feature work there beyond a font pick.
3. **Grow an editor or an IDE.** Back-burnered in SPEC §1.2 and the Backlog. The file browser (M-13)
   must stay read-only; the moment it saves a file it is competing with the editors users already love.
4. **Put terminal contents in a database or a cloud.** The owner's rule (AGENTS.md, 2026-10-29) and
   the "no hub" choices in MM1. Search scans recordings; a relay carries ciphertext; no hosted
   account system, no telemetry.
5. **Become a team server with roles, SSO and shared daemons.** Everything assumes one person: the
   user-only socket and pipe ACLs, profiles as that person's accounts, "a peer message is never
   consent". Sharing a session with a guest (M-24) is fine; multi-tenant is a different product with
   a different threat model. Related: no i18n (M-71) until the strings stop moving, and no LLM
   router inside the engine (III.3).

---

## 5. Strategic observations

**The sharpest wedge.** ptyro is the only thing in its own research that combines (a) a daemon that
owns real PTYs with exact reattach and reboot survival, (b) status told by the agents rather than
scraped, across vendors, and (c) profiles that keep accounts apart. Put together: *the place where
agents from any vendor, on any of your machines and accounts, keep running and tell you truthfully
when they need you.* The features that sharpen that are the ones about **attention and return**:
digest, catch-me-up, snooze, presence, search, cost. They are mostly small, mostly unbuilt, and
mostly absent from the R- funnel, which leans heavily toward orchestration (36 R-ORC rows) and
creation flows.

**What the funnel under-weights.** Reading Part III as a whole: orchestration and UX-of-creating
are deep; "what happened while I was away", "find it again" and "get things in and out of a remote
worktree" are thin or missing. Of the top fifteen above, eight are **new** and three more sit in the Backlog without a task id.

**What dilutes it.**
- *Chat.* It is excellent and it is the largest non-TUI subsystem, but a general chat client with
  meaning search, imports and compare is a second product growing inside the first. Its strongest
  justification is as the front door to tasks (`chat task`) and as the answer engine for M-47; more
  chat-only depth (attachments, memory, cross-machine merge CHAT6) is where I would be slowest.
- *The three-clients rule at full strength.* It is a real differentiator, and it roughly triples
  the cost of each idea here. Several good ideas are naturally one-surface (voice, native shells,
  status-line, launcher scripts). The existing waiver test ("only commands that another program
  runs") is too narrow for them; a second waiver class, "the surface is the device", would let them
  exist without pretending a TUI has a microphone.
- *Windows depth before field proof elsewhere.* A native ConPTY daemon is built while the README
  says macOS has not been run by hand, the phone app has not been on a real phone, and three of
  five agents have not met the real thing. The cheapest "feature" available is M-74 plus a week of
  real-device use: it converts "built" into "works".
- *Change sets and the merge train* are impressive and far from the wedge for a single user; not
  my lens, noted only because they compete for the same hours as the small attention features.

**One positioning thought.** Vendors are shipping their own agent views, background sessions and
remote control (RESEARCH A2, E1). ptyro cannot out-ship each vendor inside that vendor's product.
Its durable ground is what a vendor will not build: cross-vendor, cross-account, cross-machine,
local-first, scriptable. Prioritise features by asking "would Anthropic, OpenAI or Google ship this
for their own agent only?" If yes (a nicer chat, a single-agent phone view), ptyro's version must be
the cross-vendor one or it is not worth building. If no (profiles, federation, a truthful digest
across all of them, search across all of them), it is the roadmap.
