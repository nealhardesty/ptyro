# ptyro

**A terminal-native home for your coding agents.**

ptyro runs Claude Code, Codex, Gemini CLI and OpenCode side by side, each in its own git
worktree, inside the terminal you already use. A background daemon owns every session, so you
can detach, close the terminal or lose SSH, and the agents keep working. ptyro knows which agent
is working, which is blocked on a permission prompt and which is done, and one key takes you to
the one that needs you. When a task is too big for one agent, it can fan the work out, run
recipes, or coordinate one change across several repositories.

## What you get

- **Sessions that survive.** Detach and reattach to the exact screen; a crash or reboot brings
  agents back, ready to resume the same conversation.
- **One worktree per task.** A new branch and checkout, `.env` copied, setup script run, ports
  allocated, agent started. Removing a worktree refuses to lose unpushed work.
- **Real agent status.** Working, blocked, idle or done, from the agents' own hooks, not screen
  scraping. An inbox and notifications tell you who needs you.
- **Work and personal, never crossed.** Profiles keep accounts, git author, SSH keys and secrets
  apart, per repository.
- **Review and ship without leaving.** Diff, line comments for the agent, commit, PR, CI fixes.
- **Many agents at once.** Fan one task out to several agents and pick the best, run recipe
  workflows, or let a lead agent manage workers.
- **Your phone.** A web app shows every agent and answers its permission prompts.
- **Scriptable.** Every action is a command, most with `--json`.

## Status

Pre-1.0 (version 0.0.1), public for early testing; there is no tagged release yet. Every
feature milestone (M1–M11) is built and covered by tests, most of them against scripted fake
agents. Day-to-day use so far is mainly Claude Code on Linux. Some features have not been run
against the real thing yet: the phone app on a real phone, OpenCode, Gemini and Codex structured
turns, a real merge train, and macOS by hand. [`docs/compat.md`](docs/compat.md) records what has
been tried, and [`ROADMAP.md`](ROADMAP.md) what's next. Bug reports are very welcome.

ptyro is an independent project. It is not affiliated with or endorsed by Anthropic, OpenAI,
Google, or any other tool named here; their names and trademarks belong to their owners.

## Requirements

- **Linux or macOS.** WSL2 should work; native Windows is not supported.
- **Rust 1.99 or later** to build. A checkout pins 1.99.1 in `rust-toolchain.toml`, and
  [rustup](https://rustup.rs) fetches it for you.
- **git 2.40 or later**, and **GNU make** for building from a checkout.
- **At least one agent CLI:** [Claude Code](https://code.claude.com) (the best supported),
  [Codex CLI](https://github.com/openai/codex), [Gemini CLI](https://github.com/google-gemini/gemini-cli)
  or [OpenCode](https://opencode.ai). Any other program runs too, with less status detail.
- **A modern terminal:** Ghostty, kitty, WezTerm, Alacritty, iTerm2, foot… Inside tmux works too.
- **Optional:** the [GitHub CLI](https://cli.github.com) (`gh`) for PRs and checks;
  [Tailscale](https://tailscale.com) for the phone app.

## Install

```bash
git clone https://github.com/nealhardesty/ptyro.git && cd ptyro
make install            # builds and installs ~/.cargo/bin/ptyro
```

Or, without a checkout:

```bash
cargo install --locked --git https://github.com/nealhardesty/ptyro ptyro
```

Make sure `~/.cargo/bin` is on your `PATH`. Once a version is tagged, each GitHub release will
also have prebuilt tarballs (Linux and macOS, x86_64 and aarch64) and a Homebrew tap
(`brew install nealhardesty/ptyro/ptyro`). Then let ptyro look around:

```bash
ptyro init       # finds your agents, accounts and repositories; asks a few questions
ptyro doctor     # checks agents, logins, integrations, terminal and tmux
```

**Upgrading** while agents run: install the new binary, then `ptyro daemon upgrade`. Every
session keeps running.

## Quick start

**1. Open ptyro in a repository.**

```bash
cd ~/dev/myrepo
ptyro project add       # once per repository
ptyro                   # open the TUI (a shell session to start with)
```

Every key starts with the **prefix**, `Ctrl-Space`. `Ctrl-Space ?` lists the keys; `ptyro tour`
walks you through them.

**2. Detach and come back.** Press `Ctrl-Space d`. Everything keeps running in the daemon. Run
`ptyro` again and you're back on the same screen.

**3. An agent in its own worktree.** Press `Ctrl-Space n`, then `⏎` `⏎`: Claude in a fresh
worktree on its own branch. Or from the shell, with a first prompt:

```bash
ptyro new -d --agent claude --new-wt --prompt "fix the flaky test in tests/api.rs"
```

**4. See who needs you.** The sidebar shows each session's status. `⚑` means it wants you.

```text
 WORKSPACE main [work]        │ ● claude · claude · [work] · ptyro/brisk-rook +42 −7 · :20001
 ▾ myrepo [work]              │ (Claude, full fidelity, in its own worktree)
   ▾ @root (main)             │
     1 • shell zsh            │
   ▾ brisk-rook +42 −7 ●      │
     2 ⚠ claude permission ⚑ │
     3 ● codex  Bash: cargo t │
 ptyro                main [work] 3 live · 1 working · 1 blocked  ✉2  C-Space ? help 14:02
```

- `Ctrl-Space a` jumps to the next session that needs you (blocked first).
- `Ctrl-Space p` peeks without switching: `y` allows a permission prompt, `n` denies, or type a
  reply.
- `Ctrl-Space i` opens the inbox. From the shell: `ptyro status`.

## Features

### Projects and worktrees

A **project** is a git repository you registered. Each task gets a **worktree**: a separate
checkout on its own branch (`ptyro/<name>`), so agents can't trample each other and your main
checkout stays untouched. A new worktree gets the ignored files you list in `.worktreeinclude`
(such as `.env`), a block of ports in `$PTYRO_PORT`, and the project's setup script, which runs
only after you've reviewed and trusted it.

```toml
# .ptyro.toml in your repository (all optional)
[worktree]
share = ["node_modules"]                  # shared, not copied

[scripts]
setup = "pnpm install --frozen-lockfile"

[scripts.run.dev]
command = "pnpm dev --port $PTYRO_PORT"   # Ctrl-Space r runs it
default = true
```

`ptyro wt rm` (or `x` in the sidebar) refuses to remove a worktree with running sessions,
uncommitted or unpushed work, or stashes, and lists exactly what would be lost. ptyro also reads
setup config written for Conductor, Superset, the Codex app, Claude Desktop, Cursor and Copilot.
Details: [`docs/config.md`](docs/config.md).

### Profiles: separate accounts

A profile is one identity: agent accounts, git author, SSH key, `gh` login and secrets. Repos
matching a profile's rules are pinned to it, and ptyro asks before anything would cross over.

```toml
# ~/.config/ptyro/config.toml
[profiles.work]
claude = "~/.claude-work"                  # CLAUDE_CONFIG_DIR
git    = { name = "You", email = "you@acme.example", ssh_key = "~/.ssh/id_work" }
match  = { remotes = ["github.com:acme/*"], paths = ["~/work/**"] }
```

`ptyro profile detect --apply` finds the multi-account setup you already have (aliases,
`~/.claude-*` dirs). Profiles prevent accidents; they are not a security sandbox. Details:
[`docs/config.md`](docs/config.md#profiles-one-identity-per-context).

### Agent status and notifications

ptyro adds its hooks to each agent *for that launch only*, so your agent config is never
changed. Gemini and Antigravity need a one-time `ptyro integrations install`.

| Glyph | Means |
|---|---|
| `●` | working (the header says on what: `Bash: cargo test`) |
| `⚠` | blocked: a permission prompt or a question |
| `○` / `✓` / `✗` | idle / done / errored |
| `☾` | dormant: stopped, resumes into the same conversation |
| `⚑` | needs your attention |

Notifications always go to the inbox, and by default to your terminal and its bell. Add desktop
notifications or a push service such as ntfy:

```toml
[notifications]
channels = ["inbox", "osc", "bell", "desktop", "sinks"]

[[notifications.sinks]]
type = "ntfy"
url = "https://ntfy.sh/my-secret-topic"
on = ["blocked"]
```

`ptyro notify test` checks your setup, and `ptyro usage` shows each profile's rate limits.

### Review and ship

`Ctrl-Space D` opens the **inspector** on a worktree: the diff (against the base, unpushed, or
just the agent's last turn), line comments sent to the agent as one prompt, commit, PR and
checks. `Ctrl-Space Q` lists worktrees waiting for your review.

```bash
ptyro wt diff myrepo/brisk-rook --against last-turn   # what the agent's last turn changed
ptyro review add myrepo/brisk-rook src/auth.rs:42 "this leaks the handle"
ptyro review send myrepo/brisk-rook        # every comment, as one prompt
ptyro pr create myrepo/brisk-rook          # push + PR; CI failures go back to the agent
ptyro wt integrate myrepo/brisk-rook       # rebase onto main and fast-forward it
```

PRs need `gh`. `ptyro new --issue URL` starts from a GitHub issue, and `ptyro wt pair` shows an
agent's work in your main checkout, for your IDE and dev server.

### Orchestration: fan-out, recipes, leads

Give one task to several agents, each in its own worktree, then compare them by evidence (tests,
diff size, time) and take the best:

```bash
ptyro fanout --agents claude,codex,gemini --prompt-file task.md --test
ptyro compare @last                        # ranked candidates, with reasons
ptyro pick @last claude                    # merge it; `ptyro discard @last` removes the rest
ptyro recipe run implement-and-review --var task="add --json to ls"
ptyro new --agent claude --lead --max-workers 3   # an agent that manages workers
```

**Recipes** are TOML workflows with gates that wait for you. Built-ins include
`implement-and-review` (a writer and a reviewer from another vendor), `best-of-n`, `test-fixer`
and `overnight`. **Verified-done** (`--verify "cargo test"`) means an agent's turn only counts as
done when the check passes. Agents can message each other, claim files, and every turn leaves a
checkpoint you can restore. `Ctrl-Space O` shows every run. Try it with fake agents:
`make orchestrate-demo`.

### Change sets: one change across several repos

A change set puts a worktree of each repo side by side on one branch. A lead agent writes the
contract between them; once you approve it, one agent per repo implements its part, building
against the others' unpushed code. Checkpoints cover every repo, and a merge train lands the PRs
in dependency order.

```bash
ptyro cs new add-user-tier --projects api,sdk,web --overlay -p "users get a tier: free or pro"
ptyro cs contract add-user-tier approve    # after reading the lead's CONTRACT.md
ptyro cs status add-user-tier              # members, gates, and what needs doing
ptyro cs pr add-user-tier && ptyro cs land add-user-tier
```

`Ctrl-Space S` shows the set as swimlanes. `make changeset-demo` runs one with fake agents. The
guide: [`docs/changesets.md`](docs/changesets.md).

### Your phone and other machines

```bash
ptyro serve --tailscale        # the web app over Tailscale HTTPS
ptyro pair phone               # a one-time QR code (10 min): scan it and the phone is signed in
ptyro devices revoke ID        # cut a device off at once
ptyro --remote me@workbox      # the TUI (or any command) on another machine, over ssh
```

The web app lists your agents (blocked ones on top, with Allow / Deny), the inbox, and a live
view of any session with a reply box. Push notifications carry Allow / Deny where the platform
shows actions. Each device gets a scope (`read`, `approve` by default, `control` or `full`). Setup and the threat model:
[`docs/remote.md`](docs/remote.md).

### Structured sessions

Instead of the agent's terminal UI, ptyro can drive it over its own protocol (Claude stream-json,
Codex app-server, Gemini ACP, OpenCode's server, or any ACP agent). You get a chat view, in the
TUI and on the phone, with every tool call, its output and diff, permission cards, and each
turn's cost.

```bash
ptyro new -n fixer --agent claude --structured
ptyro read fixer --chat                    # the transcript, one JSON item per line
ptyro convert fixer                        # take over in Claude's own TUI, same conversation
ptyro convert fixer --to structured        # and back
```

`Ctrl-Space t` switches between the chat and terminal views.

### Scripting

```bash
ptyro new -d -n fixer --agent claude --new-wt fix-login --prompt "Fix the login bug"
ptyro wait fixer --for idle --timeout 30m      # 0 when the turn is over; 7 on timeout
ptyro read fixer | tail -20
ptyro send fixer "now add a test" --wait       # prints how the turn ended
ptyro events --follow | jq .                   # every change, one JSON line each
```

`wait --for idle` waits for the turn ptyro last typed, so `new --prompt` then `wait` is safe.
More recipes, including one that teaches *your agents* to drive ptyro, are in
[`docs/scripting.md`](docs/scripting.md). Every command is in
[`docs/commands.md`](docs/commands.md), and every `--json` shape in
[`docs/cli-json.md`](docs/cli-json.md).

## Essential keys

All after the prefix, `Ctrl-Space`. The full list is in [`docs/keys.md`](docs/keys.md).

| Key | Does |
|---|---|
| `?` / `:` | every key / the command palette |
| `d` | detach (everything keeps running) |
| `n` / `N` | new session / new worktree |
| `a` / `p` / `i` | next session that needs you / peek and answer / inbox |
| `j` `k` / `1`…`9` / `/` | next / previous / Nth session / find one |
| `v` / `s` | split side by side / stacked |
| `h` `l` / `z` / `c` | move between panes / zoom / close pane |
| `Tab` | focus the sidebar (`⏎` open, `x` stop or remove, `e` why this status) |
| `[` | copy mode: scroll back, search (`/`), select (`v`), copy (`y`) |
| `D` / `Q` | inspector / review queue |
| `O` / `S` | runs / change sets |
| `w` / `o` / `r` | switch workspace / open in editor / run the default run script |

## Configuration

Your config is `~/.config/ptyro/config.toml`; it's optional. `ptyro config edit` creates a
commented one and validates it when you save. Changes apply live.

```toml
[ui]
prefix = "C-a"                  # default C-Space
theme = "ptyro-light"           # ptyro-dark (default), ptyro-light, high-contrast

[agents.claude]
command = ["claude", "--model", "opus"]
hibernate_after = "2h"          # stop idle, unwatched agents; they resume on focus
```

Each repository can have a `.ptyro.toml` (shown above). The full reference, including
notifications, profiles and where ptyro keeps its files, is [`docs/config.md`](docs/config.md).

## Tips and troubleshooting

- **Inside tmux:** add `set -g allow-passthrough on` and `set -g extended-keys on` to
  `~/.tmux.conf` so Shift+Enter and clipboard copies reach ptyro.
- **macOS:** free `Ctrl-Space` from the input-source shortcut, or pick another prefix.
- **Shift+Enter** reaches Claude when your terminal speaks the kitty keyboard protocol (Ghostty,
  kitty, WezTerm, foot, recent iTerm2 and Alacritty). `Ctrl-Space ?` shows whether it's on.
- **Native mouse selection:** hold Shift while dragging, or set `[ui] mouse = false`.
- **Prompt jumps and live cwd** need one line in your shell's rc file:
  `eval "$(ptyro shell-init zsh)"` (or `bash`; fish: `ptyro shell-init fish | source`).
- **Completions:** `source <(ptyro completions zsh)` (or bash, fish…). **Start at login:**
  `ptyro daemon install-service` (systemd or launchd).
- **Something odd?** Run `ptyro doctor`. `ptyro daemon logs` shows the daemon's log, and
  `PTYRO_LOG=debug` makes it chatty. `Ctrl-Space C-l` redraws the screen.
- **Recordings** of each session (for `ptyro replay` and `ptyro export`) can hold secrets that
  scrolled past. Turn them off with `[recorder] enabled = false`.

## Documentation

| File | What's in it |
|---|---|
| [`docs/commands.md`](docs/commands.md) | Every command, how to name sessions, and the exit codes |
| [`docs/keys.md`](docs/keys.md) | Every key: prefix, sidebar, inspector, copy mode, mouse, rebinding |
| [`docs/config.md`](docs/config.md) | Configuration: your config, profiles, notifications, `.ptyro.toml`, file locations |
| [`docs/scripting.md`](docs/scripting.md) | Scripting recipes, and a snippet that teaches agents to use ptyro |
| [`docs/cli-json.md`](docs/cli-json.md) | Every `--json` output and the exit codes |
| [`docs/remote.md`](docs/remote.md) | Phones and other machines: setup, scopes, remote approvals, threat model |
| [`docs/changesets.md`](docs/changesets.md) | Change sets: one change across several repos, from contract to merge train |
| [`docs/compat.md`](docs/compat.md) | Which programs and terminals work inside ptyro, and how well |
| [`docs/fixtures.md`](docs/fixtures.md) | Recording and sanitising test fixtures |
| [`SPEC.md`](SPEC.md) | Architecture, protocol, formats, configuration, CLI, security, verified facts |
| [`ROADMAP.md`](ROADMAP.md) | Milestones and tasks: everything that doesn't exist yet |
| [`RESEARCH.md`](RESEARCH.md) | Competitor landscape and the idea funnel that feeds the roadmap |
| [`CHANGELOG.md`](CHANGELOG.md) | What changed, generated from git history |
| [`AGENTS.md`](AGENTS.md) | Rules for AI coding agents working on this repo |

## Development

Everything runs through `make`; `make` on its own lists the targets. CI calls the same targets
(`make check`, `make fuzz-smoke`, `make web-check`), never raw `cargo`, so local runs and CI
behave the same. (License and advisory checks run there through the cargo-deny action.)

| Target | Does |
|---|---|
| `make help` | list the targets (the default) |
| `make build` / `release` / `install` | debug build / optimized `target/release/ptyro` / install into `~/.cargo/bin` |
| `make run ARGS="ls"` / `make try CMD=claude` | run the dev build with arguments / start `CMD` in a new session and attach |
| `make test` / `make smoke` | all tests / quick end-to-end checks of the PTY and emulator |
| `make fmt` / `fmt-check` / `lint` | format / fail if unformatted / clippy with warnings as errors |
| `make check` / `make ci` | what CI runs except `deny` (fmt-check, lint, test, smoke) / everything, `deny` included |
| `make tools` / `make deny` | install cargo-deny / license, advisory and source checks |
| `make doc` / `make clean` | API docs / remove build output |
| `make sandbox` / `make sandbox-clean` | a throwaway ptyro (own daemon, store, worktrees) with a demo repo / delete it. It isolates ptyro's state, not a security sandbox: agents still run as you |
| `make orchestrate-demo` (`ARGS=tui\|clean`) | fan-out, a gated recipe and a lead over MCP, with fake agents |
| `make changeset-demo` (`ARGS=tui\|clean`) | a change set across three repos, contract to merge train, with fake agents |
| `make verify-claude` | check the Claude adapter against the real Claude Code on your machine (one tiny turn) |
| `make verify-structured` | check structured mode against the real agents here |
| `make headless CMD=… ARGS=…` | run a program in ptyro's emulator without a terminal; prints screen and status |
| `make fixture NAME=… SIZE=… CMD=…` / `make snapshots` | record a golden-test fixture (sanitise it!) / accept golden snapshots (screens, `--json` shapes, rc-file detection) |
| `make fuzz-smoke` (`FUZZ_SECONDS=5`) / `make fuzz` (`TARGET=… SECONDS=…`) | mutation fuzzing on stable (CI) / coverage-guided `cargo fuzz` (nightly) |
| `make proptest-deep` | 20k-case property test of the frame protocol |
| `make compat` (`VTTEST=…`) | headless compatibility sweep → a table for `docs/compat.md` |
| `make bench` / `make criterion` | emulator parse speed, daemon throughput, idle CPU, key latency (Linux) / micro-benchmarks (`target/criterion`) |
| `make latency` (`ARGS=…`) | key → echo → frame round trip through a real daemon, p50/p99 |
| `make soak` (`DURATION=10m`) | fake agents and shells with live upgrades; fails on leaks, stuck turns or lag |
| `make web` / `make web-check` | build the web app (needs Node) / check the committed `web/dist` matches (CI) |
| `make dist` (`TRIPLE=…`) / `changelog` / `homebrew-formula` | release tarball + `.sha256` in `dist/` / regenerate `CHANGELOG.md` / render `dist/ptyro.rb` from the checksums |
| `make check-macos` | type-check the workspace for macOS from Linux |

Pushing a `vX.Y.Z` tag runs `dist`, `changelog` and `homebrew-formula` in CI and publishes the
release. Only the final `publish` job gets a write token. It runs in the `release` environment,
which holds the `HOMEBREW_TAP_TOKEN` secret (a fine-grained token that can write only to
`nealhardesty/homebrew-ptyro`); restrict that environment to `v*` tags. Actions are pinned by
commit SHA, and Dependabot keeps them and the crates current.

Found a security problem? See [`SECURITY.md`](SECURITY.md).

The code is a Cargo workspace in `crates/` (SPEC §3.3). End-to-end tests in `crates/ptyro/tests`
run a real daemon against real git repos, with built-in fake agents and a fake `gh`. Ground rules:

- Read `SPEC.md` before changing anything; record new hard-won facts in SPEC §20.
- Reference roadmap task IDs in commits (e.g. `M1.4: query responder`).
- Tick roadmap items when they ship, and keep this README current.
- `make check` must pass on Linux and macOS (CI runs both).
- Anything new that someone would run gets a `make` target ([`AGENTS.md`](AGENTS.md)).

## License

[MIT](LICENSE).

## Inspirations

ptyro is an independent project with its own design and code. Ideas from these tools helped shape
it, with thanks:

- [tmux](https://github.com/tmux/tmux), [Zellij](https://zellij.dev) and
  [WezTerm](https://wezterm.org): terminal multiplexing and detach/reattach
- [Orca](https://www.onorca.dev): an agent workspace with worktrees and parallel agents
- [Conductor](https://conductor.build): worktree setup scripts and ports
- [Happy](https://happy.engineering): end-to-end encrypted remote control

The full list is in SPEC §20.6.
