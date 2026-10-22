# Every command in the TUI

The TUI is first class (SPEC §1.3): anything the CLI does, you can do in the TUI. This page lists
every command and how the TUI does it, so it doubles as a "how do I … in the TUI" guide.
Keys are the defaults, after the prefix (`Ctrl-Q` or `Ctrl-Space`). Every action named in
backticks is also in the palette (`Ctrl-Q :`), with its key.

A few commands have no TUI path on purpose, each with its reason (`waived:`). The rest that
don't have one yet are marked `gap:`, with the roadmap task that closes it.

`make tui-parity` checks this page against the real command tree. It fails when a command has
no row, or a row names a command or action that doesn't exist, and (strictly) on any gap. CI runs
the same check through `make test`. When you add a command, add its row here.

## Sessions

| Command | In the TUI |
|---|---|
| `new` | `session.new` (`n`): an agent, a shell or a command line, then where it runs; `worktree.new` (`N`) for a new worktree |
| `attach` | `session.find` (`/`), the sidebar (`Tab`, `⏎`), `1`–`9`, `j`/`k` |
| `list` | `focus.toggle`: the sidebar lists every session with its status; `session.find` (`/`) searches them |
| `stop` | `session.stop` (`x`) |
| `kill` | `session.kill` (`X`) |
| `restart` | `session.restart` (`C-r`, or `⏎` on an exited pane) |
| `rm` | `session.stop` (`x`) on an exited session |
| `rename` | `session.rename` (`,`) |
| `resume` | `session.resume` (or `⏎` on a dormant pane) |
| `hibernate` | `session.hibernate` |
| `convert` | gap: UX1.21 (switch between the agent's own TUI and structured mode) |
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
| `mute` | `session.mute`; a project or workspace: gap: UX1.21 |
| `notify test` | `notify.test` |
| `usage` | shown: the status bar and the sidebar title show the rate limits of the focused session's profile |
| `integrations ls` | gap: UX1.20 (the health badge lists what's missing) |
| `integrations install` | gap: UX1.20 |
| `integrations uninstall` | gap: UX1.20 |

## Projects, worktrees, workspaces

| Command | In the TUI |
|---|---|
| `project add` | `project.add`: pick from discovered repositories, or "here" |
| `project discover` | `project.add` (it discovers) |
| `project ls` | shown: the sidebar lists the workspace's projects |
| `project show` | gap: UX1.21 |
| `project rm` | gap: UX1.21 |
| `project trust` | `project.trust` |
| `project untrust` | gap: UX1.21 |
| `project config` | gap: UX1.21 |
| `project pin` | gap: UX1.21 (today only "keep the pin? [Y/n]" after adding) |
| `project unpin` | gap: UX1.21 |
| `wt new` | `worktree.new` (`N`), or `session.new` (`n`) → "+ new worktree" |
| `wt ls` | shown: the sidebar lists every worktree under its project |
| `wt rm` | `worktree.remove` (`x` on a worktree row) |
| `wt status` | shown: the sidebar's diffstat and dirty marker; `inspector` (`D`) for the details |
| `wt diff` | `inspector` (`D`), Diff tab; `a` picks what to diff against |
| `wt log` | `inspector` (`D`), Commits tab |
| `wt commit` | `inspector` (`D`): `C` commits, `A` asks the agent |
| `wt integrate` | `inspector` (`D`): `I` |
| `wt pair` | gap: UX1.21 |
| `wt unpair` | gap: UX1.21 |
| `wt run` | `script.run` (`r`), `script.pick` (`R`) |
| `wt open` | `worktree.open` (`o`) |
| `wt lock` | gap: UX1.21 |
| `wt unlock` | gap: UX1.21 |
| `wt rename-branch` | gap: UX1.21 |
| `gc` | gap: UX1.12 (clean up merged worktrees) |
| `ws ls` | `workspace.switch` (`w`) |
| `ws new` | `workspace.switch` (`w`) → "+ new workspace" |
| `ws set` | gap: UX1.21 |
| `ws rm` | gap: UX1.21 |
| `ws rename` | gap: UX1.21 |
| `ws add` | `project.add` (adds to the current workspace) |
| `ws remove` | gap: UX1.21 |

## Profiles

| Command | In the TUI |
|---|---|
| `profile ls` | gap: UX1.21 |
| `profile show` | gap: UX1.21 |
| `profile new` | gap: UX1.21 |
| `profile rm` | gap: UX1.21 |
| `profile rename` | gap: UX1.21 |
| `profile env` | waived: prints `export` lines for your shell to `eval`; sessions get the environment already |
| `profile login` | gap: UX1.21 |
| `profile sync` | gap: UX1.21 |
| `profile detect` | gap: UX1.19 (the first run finds them) |
| `profile doctor` | gap: UX1.20 |

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
| `pr reply` | gap: UX1.21 |
| `pr open` | gap: UX1.21 |

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
| `run fork` | gap: UX1.21 |
| `run lead` | gap: UX1.21 |
| `recipe ls` | gap: UX1.21 |
| `recipe show` | gap: UX1.21 |
| `recipe run` | gap: UX1.21 |
| `recipe validate` | waived: for recipe authors, in their editor or CI |
| `recipe graph` | waived: draws a recipe as Mermaid text for documents |
| `gate ls` | `runs` (`O`): open gates show on their run; the status bar counts them |
| `gate resolve` | `runs` (`O`): `1`–`9` answer, `n` with a note |
| `msg send` | gap: UX1.21 (today only to a change set's members, `sets` `m`) |
| `msg check` | waived: an agent's own mailbox; your messages reach the inbox |
| `msg ls` | `inbox` (`i`): messages to you |
| `msg ack` | waived: an agent acknowledging its own mail |
| `lease claim` | waived: agents claim the files they edit |
| `lease release` | waived: agents release their own claims |
| `lease ls` | gap: UX1.21 |
| `radar` | gap: UX1.21 |
| `checkpoint ls` | gap: UX1.21 |
| `checkpoint diff` | gap: UX1.21 |
| `checkpoint restore` | gap: UX1.21 |
| `verify` | gap: UX1.21 |
| `mcp` | waived: the MCP server a lead agent talks to; ptyro starts it for the agent |

## Change sets

| Command | In the TUI |
|---|---|
| `cs new` | gap: UX1.21 |
| `cs templates` | gap: UX1.21 (with `cs new`) |
| `cs ls` | `sets` (`S`) |
| `cs show` | `sets` (`S`): `⏎` on a set |
| `cs log` | `sets` (`S`): the ledger, as a timeline |
| `cs msg` | `sets` (`S`): `m` |
| `cs check` | `sets` (`S`): `c` |
| `cs checkpoint` | `sets` (`S`): `p` |
| `cs checkpoints` | gap: UX1.21 |
| `cs restore` | gap: UX1.21 |
| `cs diff` | `sets` (`S`): `⏎` on a member, then `inspector` (`D`) on its worktree |
| `cs contract show` | gap: UX1.21 |
| `cs contract edit` | gap: UX1.21 |
| `cs contract diff` | gap: UX1.21 |
| `cs contract history` | gap: UX1.21 |
| `cs contract approve` | `sets` (`S`): `1`–`9` answer the contract gate |
| `cs contract reject` | `sets` (`S`): answer the contract gate with a note |
| `cs contract submit` | waived: the lead agent submits the contract |
| `cs contract revise` | waived: the lead agent revises the contract |
| `cs ready` | waived: run by a member agent inside the set |
| `cs blocked` | waived: run by a member agent inside the set |
| `cs assign` | waived: run by the lead agent inside the set |
| `cs ask` | waived: run by an agent inside the set; the question reaches you as a gate |
| `cs overlay` | gap: UX1.21 |
| `cs commit` | gap: UX1.21 |
| `cs push` | gap: UX1.21 (the push gate answers in `sets`) |
| `cs pr` | gap: UX1.21 |
| `cs land` | gap: UX1.21 (the land gate answers in `sets`) |
| `cs add` | gap: UX1.21 |
| `cs drop` | gap: UX1.21 |
| `cs rm` | gap: UX1.21 |

## Remote, phone and recordings

| Command | In the TUI |
|---|---|
| `serve` | gap: UX1.21 |
| `pair` | gap: UX1.21 (the QR code in an overlay) |
| `devices ls` | gap: UX1.21 |
| `devices revoke` | gap: UX1.21 |
| `devices push-test` | gap: UX1.21 |
| `proxy` | waived: the bridge `--remote` runs over ssh on the other machine |
| `replay` | gap: UX1.21 |
| `export` | gap: UX1.21 |

## Setup, config and the daemon

| Command | In the TUI |
|---|---|
| `init` | gap: UX1.19 (the first run, as a screen) |
| `doctor` | gap: UX1.20 (the health badge) |
| `tour` | `help.tour` |
| `config edit` | `config.edit` (validated when you save; changes apply live) |
| `config check` | `config.edit`: saving checks it, and a bad config shows as an error |
| `config show` | gap: UX1.21 |
| `config path` | `config.edit` (opens it) |
| `completions` | waived: shell setup, for typing commands in a shell |
| `shell-init` | gap: UX1.20 (offers to add the line to your rc file) |
| `daemon start` | waived: the TUI starts the daemon when it isn't running |
| `daemon stop` | waived: stopping the daemon from its own client ends the TUI; sessions with resume come back with `daemon restart --hibernate` |
| `daemon restart` | gap: UX1.20 |
| `daemon status` | gap: UX1.20 |
| `daemon logs` | gap: UX1.20 |
| `daemon upgrade` | gap: UX1.20 (a newer binary: "upgrade the daemon live? `u`") |
| `daemon install-service` | gap: UX1.20 |
| `daemon uninstall-service` | gap: UX1.20 |
