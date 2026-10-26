# Configuration

ptyro reads two kinds of file. Both are optional.

| File | Whose | What goes in it |
|---|---|---|
| `~/.config/ptyro/config.toml` | yours | the UI, agents, profiles, notifications, the daemon, remote access |
| `.ptyro.toml` in a repository | the project's (committed) | worktree setup, scripts, ports, prompt snippets, verified-done |
| `.ptyro.local.toml` beside it | yours (not committed) | overrides for `.ptyro.toml` |

**You don't have to edit these files.** Settings (`C-q C` in the TUI, More → Settings on the
web) shows every option below with what it does, its default and where its value comes from,
and changes it in place, keeping the file's comments; `ptyro config list|get|set|unset|add|rm|rename`
do the same from a script (`--project` for a repository's own file, or `--project=NAME` for
another one; `--shared` for the committed one). Each setting says when a change shows: at once, for sessions started afterwards, or after
a daemon restart (which the panel offers). A new option isn't done until it has its entry in
`ptyro-core`'s settings table (SPEC §13.7); a test fails without one.

`ptyro config edit` creates a commented `config.toml` (every ordinary setting, with its default
and what it does, all commented out) and validates it when you save. Changes
apply live. `ptyro config check` validates the file, `ptyro config show` prints the effective
configuration with defaults filled in, and `ptyro config path` prints where it is.

The full schema is in [`SPEC.md`](../SPEC.md) §13. This page covers the settings people change
most; `ptyro config show` lists every key with the value in effect.

## Your config: `~/.config/ptyro/config.toml`

```toml
[ui]
prefix = "C-]"            # one key or a list; default ["C-q", "C-Space"]
theme = "ptyro-light"     # ptyro-dark (default), ptyro-light, high-contrast, or your own (Settings → Themes picks it; a new one starts as a copy of one you choose)
tint_borders = true       # colour pane dividers with the session's profile
# on_stop = "stay"        # after you stop a pane's session, keep showing it (default: the next one)
# sidebar_detail = "full" # lines under each row (task, now, last…): auto (default) | compact | full
# shell_style = "pane"    # what ! shows: a drawer when the project is open, else a panel (auto, default) | drawer | pane
# which_key_delay = 0     # ms after the prefix before the menu of next keys shows; 0: never (default 400)
# mouse = false           # let the terminal own the mouse (native selection)
# confirm_kill = false    # x stops a session without asking
# sidebar = false         # start with the sidebar hidden (`\` toggles it)
# sidebar_width = 30      # at least 12
# status_bar = false
# clock = false           # no clock in the status bar
# show_ports = false      # hide listening ports in the sidebar and pane headers

[worktrees]
location = "sibling"      # data (default: ptyro's data dir) | sibling (<repo>.worktrees/) | in-repo
branch_template = "{user}/{name}"   # {name} {user} {date} {issue} {slug}; default ptyro/{name}
archive_on_merge = true   # archive a worktree once its PR merges (default false); `wt restore` brings it back

[ports]
range = "20000-29999"     # each worktree gets `count` ports from here
count = 10

[discover]
roots = ["~/dev", "~/src"]   # where `project.add` (`Ctrl-Q P`) looks for repositories
# auto_register = false      # don't make the repository a session runs in a project by itself

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
# machine_name = "studio"                 # what the web page's title and the TUI call this computer (default: its host name)

[summaries]
enabled = true                            # a `≈` line after each agent turn, from a small model (default off)
# daily_cap = 200                         # per profile, per day
# command = ["claude", "-p", "--model", "haiku", "--output-format", "json", "--strict-mcp-config", "--tools", ""]
                                          # any command: the prompt on stdin, the summary on stdout;
                                          # it runs on the session's profile account, with secrets redacted

[scratchpad]
# mode = "drawer"                         # how C-q ; shows it: drawer | popup | pane
# editor = "nvim"                         # a command line; default: [editors], $VISUAL, $EDITOR, nvim/vim/nano/vi
# path = "~/Sync/pad.md"                  # the pad for everywhere (default: scratchpad.md in the data directory); a project's is .ptyro/scratchpad.md in its repository
# keep = 30                               # days of saved copies (the newest 20 always stay)
# tick_on_finish = false                  # leave an item open when its worktree is merged or removed

[recorder]
enabled = true                            # record every session's output (per agent: record = false)
max_bytes = "16MiB"                       # per session, as a ring

[remote]
serve = true                              # the daemon runs `ptyro serve` (docs/remote.md)
listen = "127.0.0.1:7091"
# local = true                            # every interface, local addresses only (your LAN, tailnet)
# tls_cert = "~/certs/fullchain.pem"      # your own certificate instead of the one ptyro makes
# tls_key = "~/certs/privkey.pem"         #   and its key
approval_wait = "10m"                     # held Claude permission prompts wait this long for a phone
# push_hosts = ["ntfy.example.com"]       # self-hosted push services (UnifiedPush, ntfy)
# tailscale = true                        # run `tailscale serve` in front: the phone gets Tailscale's HTTPS name
# trusted_proxy = true                   # a reverse proxy on this machine fronts serve (plain HTTP on loopback)
# insecure = true                        # plain HTTP instead of HTTPS, any address (tokens in clear)

[clipboard]
# allow_write = false                     # programs' OSC 52 copies don't reach your clipboard

[keys]
"y" = "pane.zoom"         # after the prefix; "global:M-1" = "session.focus 1" needs no prefix
```

Key bindings and the action names you can bind are in [`keys.md`](keys.md). Settings lists
every key, defaults included, to change, unbind or reset from the panel; in the file only your
changes are kept (`"none"` unbinds a default).

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
on = ["blocked", "errored", "done", "idle", "gate", "run"]   # the default
quiet_hours = "22:00-07:00"          # inbox only, except [attention] breakthrough (blocked, gate)
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
`PTYRO_WORKTREE_NAME`, `PTYRO_BRANCH`, `PTYRO_PORT` and `PTYRO_PORT_COUNT`. Every session also
gets `PTYRO_SESSION_ID` and `PTYRO_SOCKET`. `[compat] conductor_env = true` (in your
config) adds the `CONDUCTOR_*` names, so Conductor setup scripts run unchanged.

## Where files live

ptyro follows the XDG directories:

| Path | Holds |
|---|---|
| `~/.config/ptyro/` | `config.toml`, `recipes/`, `changesets/` (templates) |
| `~/.local/share/ptyro/` | `ptyro.db`, `worktrees/`, `sessions/<id>/pty.log` (recordings), `scratchpad.md` and `scratchpad.history/` (the pad for everywhere; each project's is `.ptyro/scratchpad.md` in its repository, kept out of git) |
| `~/.local/state/ptyro/` | logs, `crashes/` |
| `$XDG_RUNTIME_DIR/ptyro/` | the daemon's socket (without `XDG_RUNTIME_DIR`, as on macOS: `$TMPDIR/ptyro-<uid>/`; a path too long for a socket falls back to `/tmp/ptyro-<uid>-<hash>/`) |

Set `PTYRO_HOME` to keep everything under one directory instead. `make sandbox` does exactly that.

Other environment variables:

| Variable | Does |
|---|---|
| `PTYRO_CONFIG` | use this config file instead of `config.toml` |
| `PTYRO_SOCKET` | use this socket path |
| `PTYRO_REMOTE` | like `--remote` |
| `PTYRO_SSH`, `PTYRO_REMOTE_BIN` | the ssh command and the remote ptyro ([remote.md](remote.md)) |
| `PTYRO_LOG` | the daemon's log filter, e.g. `debug` |
| `PTYRO_COMPRESS=1` | compress frames on a local connection too (always on with `--remote`) |
| `PTYRO_NO_AUTOSPAWN=1` | don't start the daemon when none is running |
| `PTYRO_ALLOW_NESTED=1` | allow the TUI inside a ptyro session |
| `PTYRO_SCRATCHPAD`, `PTYRO_PROJECT_SCRATCHPAD` | *set* in every session, not read: the files of the scratchpad for everywhere, and of the session's project (only in sessions that belong to one) |
| `NO_COLOR` | no colour in CLI output |
