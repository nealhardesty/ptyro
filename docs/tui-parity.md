# Every command in the TUI

The TUI is first class (SPEC §1.3): anything the CLI does, you can do in the TUI. This page lists
every command and how the TUI does it, so it doubles as a "how do I … in the TUI" guide.
Keys are the defaults, after the prefix (`Ctrl-Q` or `Ctrl-Space`). Every action named in
backticks is also in the palette (`Ctrl-Q :`), with its key.

A few commands have no TUI path on purpose, each with its reason (`waived:`). Every other
command has one: the TUI is first class (SPEC §1.3).

`make tui-parity` checks this page against the real command tree, and CI runs the same check
through `make test`. It fails when a command has no row, a row names a command or an action that
doesn't exist, or a row is a `gap:` (a TUI path still missing). When you add a command, add its
row here: its TUI action, or `waived:` and why.

## Sessions

| Command | In the TUI |
|---|---|
| `new` | `session.new` (`n`): say what to do, ⏎ (or `C-o` to pick the agent, a shell or a command, and where); `worktree.new` (`N`) for a new worktree |
| `attach` | `session.find` (`/`), the sidebar (`Tab`, `⏎`), `1`–`9`, `j`/`k` |
| `list` | `focus.toggle`: the sidebar lists every session with its status; `session.find` (`/`) searches them |
| `stop` | `session.stop` (`x`) |
| `kill` | `session.kill` (`X`) |
| `restart` | `session.restart` (`C-r`, or `⏎` on an exited pane) |
| `rm` | `session.stop` (`x`) on an exited session |
| `rename` | `session.rename` (`,`) |
| `retitle` | `session.retitle` (`,` then `C-t`) |
| `resume` | `session.resume` (or `⏎` on a dormant pane) |
| `hibernate` | `session.hibernate` |
| `convert` | `session.convert` (the row menu, the palette) |
| `adopt` | `session.adopt` (`A` on a shell row running Claude) |
| `shell` | `shell.here` (`C-t`, `!`; `⏎` on its `›_ shell` line): the worktree's quick terminal in a drawer below the pane; again focuses it, then hides it. `--read` is `copy.enter` (`[`) in the drawer |
| `summarize` | `session.summarize` (the row menu, the palette); with `[summaries] enabled` the sidebar shows one (`≈`) after every turn |
| `status` | `session.explain` (`e` in the sidebar) says why; the sidebar and pane headers show the status |
| `answer` | `peek` (`p`): `y` allow, `a` always, `n` deny |
| `read` | `copy.enter` (`[`): the scrollback, searchable; the pane shows the screen |
| `send` | `peek` (`p`): type a reply; or type into the pane |
| `broadcast` | `broadcast` (`b`): mark sessions with `Space` in the sidebar first |
| `wait` | waived: for scripts; the TUI shows status changes as they happen and notifies you |
| `events` | `inspector` (`D`), Events tab (`5`) |
| `report` | waived: run by programs inside a session to report their own state |

## Attention and notifications

| Command | In the TUI |
|---|---|
| `inbox` | `inbox` (`i`) |
| `mute` | `session.mute`; a whole project or workspace: `project.mute`, `workspace.mute` |
| `notify test` | `notify.test` |
| `usage` | shown: the status bar and the sidebar title show the rate limits of the focused session's profile |
| `integrations ls` | `integrations` (in a split); the health badge says what's missing |
| `integrations install` | `integrations.install` |
| `integrations uninstall` | `integrations.uninstall` |

## Projects, worktrees, workspaces

| Command | In the TUI |
|---|---|
| `project add` | `project.add`: pick from discovered repositories, or "here" |
| `project discover` | `project.add` (it discovers) |
| `project suggest` | `project.suggest`; a project's first worktree offers it on its own |
| `project ls` | shown: the sidebar lists the workspace's projects |
| `project show` | `project.show` (in a split) |
| `project rm` | `project.remove` (asks first) |
| `project trust` | `project.trust` |
| `project untrust` | `project.untrust` |
| `project config` | `project.config` (with where each value comes from, in a split) |
| `project pin` | `project.pin`: pick the profile |
| `project unpin` | `project.unpin` |
| `wt new` | `worktree.new` (`N`), or `session.new` (`n`) → "+ new worktree" |
| `wt ls` | shown: the sidebar lists every worktree under its project |
| `wt rm` | `worktree.remove` (`x` on a worktree row) |
| `wt status` | shown: the sidebar's diffstat and dirty marker; `inspector` (`D`) for the details |
| `wt diff` | `inspector` (`D`), Diff tab; `a` picks what to diff against |
| `wt log` | `inspector` (`D`), Commits tab |
| `wt commit` | `inspector` (`D`): `C` commits, `A` asks the agent |
| `wt integrate` | `inspector` (`D`): `I` |
| `wt pair` | `worktree.pair` |
| `wt unpair` | `worktree.unpair` |
| `wt run` | `script.run` (`r`), `script.pick` (`R`) |
| `wt open` | `worktree.open` (`o`) |
| `wt lock` | `worktree.lock` |
| `wt unlock` | `worktree.unlock` |
| `wt rename-branch` | `worktree.rename_branch` |
| `wt retitle` | `worktree.retitle` |
| `wt finish` | `worktree.finish` (`F` in the sidebar): a PR or integrate |
| `wt archive` | `worktree.archive` (or `x` on a merged worktree) |
| `wt restore` | `worktree.restore`: pick from the archived ones |
| `wt archives` | `worktree.restore` lists them |
| `gc` | `worktree.gc`: stale worktrees, ⏎ archives one; a merged worktree's `x` archives it |
| `ws ls` | `workspace.switch` (`w`) |
| `ws new` | `workspace.switch` (`w`) → "+ new workspace" |
| `ws set` | `workspace.profile` and `workspace.notify` |
| `ws rm` | `workspace.remove` (asks first) |
| `ws rename` | `workspace.rename` |
| `ws add` | `project.add` (adds to the current workspace) |
| `ws remove` | `project.leave`: takes the selected project out of this workspace |

## Profiles

| Command | In the TUI |
|---|---|
| `profile ls` | `profile.list` (in a split) |
| `profile show` | `profile.show`: pick it |
| `profile new` | `profile.new` |
| `profile rm` | `profile.remove`: pick it |
| `profile rename` | `profile.rename` |
| `profile env` | waived: prints `export` lines for your shell to `eval`; sessions get the environment already |
| `profile login` | `profile.login`: pick the profile and the agent; it logs in, in a split (a health finding offers it too) |
| `profile sync` | `profile.sync` |
| `profile detect` | `first.run` finds them, as accounts to tick |
| `profile doctor` | `health`: the badge's findings include each profile's logins |

## Review and ship

| Command | In the TUI |
|---|---|
| `review add` | `inspector` (`D`): `c` on a line |
| `review ls` | `inspector` (`D`): comments show on their lines |
| `review rm` | `inspector` (`D`): `x` on a comment |
| `review send` | `inspector` (`D`): `S` |
| `review queue` | `review.queue` (`Q`) |
| `review done` | `review.queue` (`Q`): `d` |
| `pr create` | `inspector` (`D`): `P` |
| `pr status` | `inspector` (`D`), Checks tab; the worktree row shows `#N` and its checks |
| `pr checks` | `inspector` (`D`), Checks tab |
| `pr fix` | `inspector` (`D`), Checks tab: `f` |
| `pr automerge` | `inspector` (`D`), Checks tab: `m` |
| `pr watch` | `inspector` (`D`), Checks tab: `w` |
| `watch-pr` | `inspector` (`D`), Checks tab: `w` |
| `pr reply` | `pr.reply` |
| `pr open` | `pr.open`: its worktree and session, or a new worktree on it |

## Orchestration

| Command | In the TUI |
|---|---|
| `fanout` | `runs` (`O`): `F`, then the agents and the prompt |
| `compare` | `runs` (`O`): `c` on a fan-out |
| `pick` | `runs` (`O`), compare: `m` merge, `C` cherry-pick, `p` PR |
| `discard` | `runs` (`O`), compare: `d` one, `X` the rest |
| `iterate` | `runs` (`O`), compare: `i` one, `I` all |
| `run ls` | `runs` (`O`) |
| `run show` | `runs` (`O`): `⏎` on a run |
| `run events` | `runs` (`O`): `⏎` on a run shows its steps and why they ended |
| `run wait` | waived: for scripts; gates and finished runs reach the inbox and notifications |
| `run cancel` | `runs` (`O`): `x` |
| `run retry` | `runs` (`O`): `r` on a step |
| `run fork` | `run.fork` |
| `run lead` | `session.lead` |
| `recipe ls` | `recipe.list` (in a split) |
| `recipe show` | `recipe.show` |
| `recipe run` | `recipe.run`: its name and variables; then the runs view (`O`) |
| `recipe validate` | waived: for recipe authors, in their editor or CI |
| `recipe graph` | waived: draws a recipe as Mermaid text for documents |
| `gate ls` | `runs` (`O`): open gates show on their run; the status bar counts them |
| `gate resolve` | `runs` (`O`): `1`–`9` answer, `n` with a note |
| `msg send` | `session.message`; to a change set's members, `sets` `m` |
| `msg check` | waived: an agent's own mailbox; your messages reach the inbox |
| `msg ls` | `inbox` (`i`): messages to you |
| `msg ack` | waived: an agent acknowledging its own mail |
| `lease claim` | waived: agents claim the files they edit |
| `lease release` | waived: agents release their own claims |
| `lease ls` | `leases` (in a split) |
| `radar` | `radar` |
| `checkpoint ls` | `checkpoint.list` |
| `checkpoint diff` | `checkpoint.diff` |
| `checkpoint restore` | `checkpoint.restore` (it asks first, in a split) |
| `verify` | `session.verify` |
| `mcp` | waived: the MCP server a lead agent talks to; ptyro starts it for the agent |

## Change sets

| Command | In the TUI |
|---|---|
| `cs new` | `sets.new`: its name, projects and goal |
| `cs templates` | `sets.templates` |
| `cs ls` | `sets` (`S`) |
| `cs show` | `sets` (`S`): `⏎` on a set |
| `cs log` | `sets` (`S`): the ledger, as a timeline |
| `cs msg` | `sets` (`S`): `m` |
| `cs check` | `sets` (`S`): `c` |
| `cs checkpoint` | `sets` (`S`): `p` |
| `cs checkpoints` | `set.checkpoints` |
| `cs restore` | `set.restore` |
| `cs diff` | `sets` (`S`): `⏎` on a member, then `inspector` (`D`) on its worktree |
| `cs contract show` | `set.contract` → show |
| `cs contract edit` | `set.contract` → edit (your editor, in a split) |
| `cs contract diff` | `set.contract` → diff |
| `cs contract history` | `set.contract` → history |
| `cs contract approve` | `sets` (`S`): `1`–`9` answer the contract gate |
| `cs contract reject` | `sets` (`S`): answer the contract gate with a note |
| `cs contract submit` | waived: the lead agent submits the contract |
| `cs contract revise` | waived: the lead agent revises the contract |
| `cs ready` | waived: run by a member agent inside the set |
| `cs blocked` | waived: run by a member agent inside the set |
| `cs assign` | waived: run by the lead agent inside the set |
| `cs ask` | waived: run by an agent inside the set; the question reaches you as a gate |
| `cs overlay` | `set.overlay` |
| `cs commit` | `set.commit` |
| `cs push` | `set.push` (or the push gate in `sets`) |
| `cs pr` | `set.pr` |
| `cs land` | `set.land` (or the land gate in `sets`) |
| `cs add` | `set.add` |
| `cs drop` | `set.drop` |
| `cs rm` | `set.remove` |

## Remote, phone and recordings

| Command | In the TUI |
|---|---|
| `serve` | `remote.serve` (in a split) |
| `pair` | `remote.pair`: the QR code, in a split, to scan |
| `devices ls` | `devices.list` |
| `devices revoke` | `devices.revoke` |
| `devices push-test` | `devices.push_test` |
| `proxy` | waived: the bridge `--remote` runs over ssh on the other machine |
| `replay` | `session.replay` (plays in a split) |
| `export` | `session.export`: the format and the file |

## Setup, config and the daemon

| Command | In the TUI |
|---|---|
| `init` | `first.run`: the first run as a screen (it opens on its own the first time); terminal checks and shell extras are `doctor`'s (UX1.20) |
| `doctor` | `health` (`M`): the badge's findings, each with its fix; `doctor` runs the full one in a split |
| `tour` | `help.tour` |
| `config edit` | `config.edit` (validated when you save; changes apply live) |
| `config check` | `config.edit`: saving checks it, and a bad config shows as an error |
| `config show` | `config.show` (in a split) |
| `config path` | `config.edit` (opens it) |
| `completions` | waived: shell setup, for typing commands in a shell |
| `shell-init` | `health` (`M`): when it's missing, a finding whose fix adds the line to your rc file |
| `daemon start` | waived: the TUI starts the daemon when it isn't running |
| `daemon stop` | `app.quit` (`C-d` after the prefix): asks, then stops every session and the daemon and exits |
| `daemon restart` | `daemon.restart` (in a split; this TUI reconnects) |
| `daemon status` | `daemon.status`; the health badge has its warnings |
| `daemon logs` | `daemon.logs` (followed, in a split) |
| `daemon upgrade` | `daemon.upgrade`; a daemon running another binary is a health finding whose fix this is |
| `daemon install-service` | `daemon.service` |
| `daemon uninstall-service` | `daemon.unservice` |
