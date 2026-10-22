# Configuration

ptyro reads two kinds of file. Both are optional.

| File | Whose | What goes in it |
|---|---|---|
| `~/.config/ptyro/config.toml` | yours | the UI, agents, profiles, notifications, the daemon, remote access |
| `.ptyro.toml` in a repository | the project's (committed) | worktree setup, scripts, ports, prompt snippets, verified-done |
| `.ptyro.local.toml` beside it | yours (not committed) | overrides for `.ptyro.toml` |

`ptyro config edit` creates a commented `config.toml` and validates it when you save. Changes
apply live. `ptyro config check` validates the file, `ptyro config show` prints the effective
configuration with defaults filled in, and `ptyro config path` prints where it is.

The full schema is in [`SPEC.md`](../SPEC.md). This page covers the settings people change most.

## Your config: `~/.config/ptyro/config.toml`

```toml
[ui]
prefix = "C-]"            # one key or a list; default ["C-q", "C-Space"]
theme = "ptyro-light"     # ptyro-dark (default), ptyro-light, high-contrast, or your own
tint_borders = true       # colour pane dividers with the session's profile
# mouse = false           # let the terminal own the mouse (native selection)

[worktrees]
location = "sibling"      # data (default: ptyro's data dir) | sibling (<repo>.worktrees/) | in-repo
branch_template = "{user}/{name}"   # {name} {user} {date} {slug}; default ptyro/{name}

[ports]
range = "20000-29999"     # each worktree gets `count` ports from here
count = 10

[discover]
roots = ["~/dev", "~/src"]   # where `project.add` in the palette looks for repositories

[editors]
default = "nvim"
code = { command = ["code", "{path}"] }

[agents.claude]
command = ["claude", "--model", "opus"]   # how to start an agent
hibernate_after = "2h"                    # stop when idle and unwatched this long; resumes on focus

[agents.claude-work]
adapter = "claude"                        # any name, any wrapper: still gets Claude's integration
command = "claude"
env = { CLAUDE_CODE_MAX_OUTPUT_TOKENS = "64000" }

[agents.my-acp-agent]
command = ["my-acp-agent"]
protocol = "acp"                          # `--structured` speaks the Agent Client Protocol to it

[attention]
scope = "current-first"                   # where `a` looks: current-first | current | global

[github]
draft = true                              # new PRs are drafts
merge_method = "squash"                   # for `ptyro pr automerge`
escalate_after = "2h"                     # a PR still red after this asks for you

[review]
max_unreviewed = 3                        # hold new agents while 3 worktrees wait for review
digest = "30m"                            # "ready for review" digest, at most this often

[orchestration]
checkpoints = true                        # a checkpoint after every agent turn that changed files
lease_ttl = "30m"                         # file leases last this long unless renewed
radar = true                              # tell agents when their worktrees start to collide
message_ttl = "24h"                       # undelivered messages expire

[daemon]
scrollback_lines = 5000                   # kept in memory per session; older lines come from the recording
# restore_on_start = "resume"             # relaunch every dormant agent when the daemon starts

[recorder]
enabled = true                            # record every session's output (per agent: record = false)
max_bytes = "16MiB"                       # per session, as a ring

[remote]
serve = true                              # the daemon runs `ptyro serve` (docs/remote.md)
listen = "127.0.0.1:7091"
approval_wait = "10m"                     # held Claude permission prompts wait this long for a phone
# push_hosts = ["ntfy.example.com"]       # self-hosted push services (UnifiedPush, ntfy)
# trusted_proxy = true                   # a reverse proxy on this machine fronts serve

[clipboard]
# allow_write = false                     # programs' OSC 52 copies don't reach your clipboard

[keys]
"y" = "pane.zoom"         # after the prefix; "global:M-1" = "session.focus 1" needs no prefix
```

Key bindings and the action names you can bind are in [`keys.md`](keys.md).

## Profiles: one identity per context

A profile bundles the accounts and identity a session runs under: agent config dirs, git author
and SSH key, `gh` login and secrets.

```toml
[profiles.work]
label = "work"
color = "blue"
claude = "~/.claude-work"                  # CLAUDE_CONFIG_DIR for this profile
codex  = "~/.codex-work"                   # CODEX_HOME
git    = { name = "You", email = "you@acme.example", ssh_key = "~/.ssh/id_work" }
gh     = "~/.config/gh-work"               # GH_CONFIG_DIR
secrets = { NPM_TOKEN = { command = ["op", "read", "op://Work/npm/token"] } }
match  = { remotes = ["github.com:acme/*"], paths = ["~/work/**"] }   # repos that are "work"

[profiles.personal]
label = "me"
claude = "~/.claude"
match  = { remotes = ["github.com:you/*"] }
```

How a session's profile is chosen (the first rule that applies):

1. an explicit choice (`--profile`, or `--agent claude@work`);
2. the worktree's profile (fixed when it was created);
3. the project's pin (`ptyro project pin`, or automatic from `match`);
4. the workspace's profile;
5. `default`.

Other rules:

- **Guards.** Spawning in a repository whose `match` rules belong to another profile asks first
  (`guard = "confirm"`, the default). `guard = "refuse"` refuses, and `"warn"` just logs.
- **Isolation.** Once any profile exists, sessions stop inheriting API keys from the daemon's
  environment (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GH_TOKEN`, AWS keys…), `default` included.
  Put keys in a profile instead.
- **Secrets.** `{ command = [...] }` secrets are resolved at each spawn, held only in memory, and
  shown as `<secret:NAME>` everywhere else (`ptyro profile env work`).
- **Account dirs.** A profile without a dir for some agent gets a managed one under ptyro's data
  dir, never the agent's default. `share = { claude = ["CLAUDE.md", "skills"] }` plus
  `ptyro profile sync` links shared Claude files into each account. Credentials are never shared.
- **Not a sandbox.** Profiles prevent accidents (the wrong bill, author or org). Every profile
  still runs as your OS user (SPEC §10.10).

## Notifications

```toml
[notifications]
channels = ["inbox", "osc", "bell", "desktop", "sinks"]
on = ["blocked", "errored", "done", "idle"]
quiet_hours = "22:00-07:00"          # inbox only (blocked still breaks through)
# desktop_command = ["my-notifier", "--title", "{title}", "{body}"]   # instead of notify-send

[[notifications.sinks]]
type = "ntfy"                        # also pushover, slack, discord, telegram, command
url = "https://ntfy.sh/my-secret-topic"
on = ["blocked"]

[notifications.callback]             # optional: approve/deny buttons in your notifications
listen = "127.0.0.1:7591"            # or your tailnet address
# insecure = true                    # needed for any other address: the links are plain HTTP
```

- Approve/deny links are signed, single-use, and expire after 15 minutes. Opening one only shows
  a page; the button on it acts, so link previews can't approve anything.
- `{title}` and `{body}` in `desktop_command` are text from the agent (a prompt, a tool's
  description), so treat them as untrusted. The command runs without a shell, each `{…}` inside
  one argument; never wrap it in `sh -c "… {body} …"`, where that text would become shell
  syntax.
- Tokens and webhooks can be `{ command = ["pass", "ntfy"] }` instead of literals. If you do keep
  literals, the file stays private: ptyro writes it `0600`, and `ptyro doctor` warns when it isn't.
- `ptyro ws set personal --notify badge` keeps a workspace to inbox and badges; `mute` silences it.
- `ptyro notify test` sends a test through everything you configured.
- Codex usage meters are opt-in: `[agents.codex] usage = true`.

## The project's config: `.ptyro.toml`

```toml
[project]
base = "origin/main"                 # what new worktrees start from
# import = false                     # don't read other tools' setup files (see below)

[worktree]
copy = ["config/local.yml"]          # untracked files to copy into new worktrees
share = ["node_modules"]             # shared, not copied (copy-on-write clone, else a symlink)

[scripts]
setup = "pnpm install --frozen-lockfile"
archive = "docker compose down -v"   # before a worktree is removed
run_mode = "exclusive"               # starting a run script stops it in other worktrees
# after_turn = "cargo clippy -q"     # after every agent turn

[scripts.run.dev]
command = "pnpm dev --port $PTYRO_PORT"
default = true

[scripts.run.test]
command = "cargo test"

[verify]
check = "test"                       # "done" means this run script passes (recipe steps)
auto = true                          # ...for every agent session in this project, too
```

- **Files git doesn't carry.** Anything listed in `.worktreeinclude` (gitignore syntax) that git
  ignores, such as `.env`, is copied into each new worktree, plus `[worktree] copy`.
- **Scripts run only once you trust them.** ptyro shows the exact script text first, and asks
  again whenever it changes (`ptyro project trust`).
- **Trusting a command trusts the files it runs.** Trust pins the text (`cargo test`,
  `./scripts/lint.sh`), not what those commands read, and agents edit the checkout. Scripts,
  `after_turn` and `[verify]` checks run as you, with your full environment and network,
  outside any sandbox an agent runs in, right after an agent's turn. Trust a project only if
  you'd run its build and tests yourself after any agent's edit. (ptyro's own background `git`
  calls run no hooks and no `core.fsmonitor`; a commit, push, merge or new worktree you ask for
  runs your hooks as git would.)
- **Other tools' config is read too.** Conductor (`.conductor/settings.toml`), Superset
  (`.superset/config.json`), the Codex app (`.codex/environments/`), Claude Desktop
  (`.claude/launch.json`), Cursor (`.cursor/environment.json`) and Copilot
  (`copilot-setup-steps.yml`) fill only what `.ptyro.toml` leaves unset.
  `ptyro project config --explain` shows where every value came from.
- **A repository can never define agent commands or profiles.** Those live only in your config.

Scripts and sessions in a worktree see `PTYRO_PROJECT`, `PTYRO_ROOT_PATH`, `PTYRO_WORKTREE_PATH`,
`PTYRO_WORKTREE_NAME`, `PTYRO_BRANCH` and `PTYRO_PORT`. `[compat] conductor_env = true` (in your
config) adds the `CONDUCTOR_*` names, so Conductor setup scripts run unchanged.

## Where files live

ptyro follows the XDG directories:

| Path | Holds |
|---|---|
| `~/.config/ptyro/` | `config.toml`, `recipes/`, `changesets/` (templates) |
| `~/.local/share/ptyro/` | `ptyro.db`, `worktrees/`, `sessions/<id>/pty.log` (recordings) |
| `~/.local/state/ptyro/` | logs, `crashes/` |
| `$XDG_RUNTIME_DIR/ptyro/` | the daemon's socket |

Set `PTYRO_HOME` to keep everything under one directory instead. `make sandbox` does exactly that.

Other environment variables: `PTYRO_REMOTE` (like `--remote`), `PTYRO_LOG=debug` (a chatty
daemon log), and `NO_COLOR`.
