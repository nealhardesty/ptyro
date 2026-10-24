# Every command, in the TUI and on the web

The CLI, the TUI and the web app are all first class (SPEC §1.3): what the CLI does, you can do
in the TUI and, once ROADMAP WEB1 is done, on the web. This page lists every command, how the
TUI does it and how the web app does, so it doubles as a "how do I … in the TUI (or on my
phone)" guide. TUI keys are the defaults, after the prefix (`Ctrl-Q` or `Ctrl-Space`); every TUI
action named in backticks is also in the palette (`Ctrl-Q :`), with its key. A web action in
backticks is a control in the web app (its tooltip says what it does; the list is `ACTIONS` in
`web/src/app.ts`).

A cell is one of:
- `` `action` …``: how it's done (the first backticked word is a real TUI or web action);
- `shown: …`: always on screen, nothing to run;
- `waived: …`: no path on purpose, and why (a command another program runs, or one that only
  makes sense on the computer itself), never "not built yet";
- `gap: TASK …`: not there yet, and the ROADMAP task that brings it.

`make parity` (`make tui-parity` is the same) checks this page against the real command tree,
the TUI's actions and the web app's, and CI runs it through `make test`. It fails when a command
has no row, a row names a command or an action that doesn't exist, or a cell is in none of these
forms. A TUI `gap:` always fails: the CLI and the TUI are always in parity. A web gap is listed
(`make parity` counts them by task) and fails with `WEB_STRICT=1`, which becomes the default once
WEB1 is done. When you add a command, add its row: its TUI action and its web one, or `waived:`
and why.

## Sessions

| Command | In the TUI | On the web |
|---|---|---|
| `new` | `session.new` (`n`): say what to do, ⏎ (or `C-o` to pick the agent, a shell or a command, and where); `worktree.new` (`N`) for a new worktree | `session.new` (＋ in the session list): the task, the agent, and where: a new worktree (from the main branch, a PR, an issue, a branch or your changes), an existing one, or a shell or command |
| `attach` | `session.find` (`/`), the sidebar (`Tab`, `⏎`), `1`–`9`, `j`/`k` | `session.open`: tap it in the list; its live screen, or its chat |
| `list` | `focus.toggle`: the sidebar lists every session with its status; `session.find` (`/`) searches them | `sessions`: every session with its status, who needs you first |
| `stop` | `session.stop` (`x`) | `session.stop`: the session's ⋯ menu |
| `kill` | `session.kill` (`X`) | `session.kill`: the session's ⋯ menu |
| `restart` | `session.restart` (`C-r`, or `⏎` on an exited pane) | `session.restart`: the session's ⋯ menu |
| `rm` | `session.stop` (`x`) on an exited session | `session.stop`: ⋯ on an exited session removes it |
| `rename` | `session.rename` (`,`) | `session.rename`: the session's ⋯ menu |
| `retitle` | `session.retitle` (`,` then `C-t`) | `session.retitle`: the session's ⋯ menu |
| `resume` | `session.resume` (or `⏎` on a dormant pane) | `session.resume`: the session's ⋯ menu |
| `hibernate` | `session.hibernate` | `session.hibernate`: the session's ⋯ menu |
| `convert` | `session.convert` (the row menu, the palette) | `session.convert`: ⋯ → Show as a chat / the terminal UI |
| `adopt` | `session.adopt` (`A` on a shell row running Claude) | `session.adopt`: ⋯ on a shell running Claude |
| `shell` | `shell.here` (`C-t`, `!`; `⏎` on its `›_ shell` line): the worktree's quick terminal in a drawer below the pane; again focuses it, then hides it. `--read` is `copy.enter` (`[`) in the drawer | `session.shell`: ⋯ → A shell in its worktree (a terminal page, with keys and a reply box); a worktree's page has it too |
| `summarize` | `session.summarize` (the row menu, the palette); with `[summaries] enabled` the sidebar shows one (`≈`) after every turn | `session.summarize`: the session's ⋯ menu |
| `status` | `session.explain` (`e` in the sidebar) says why; the sidebar and pane headers show the status | `sessions`: each card and the session's page say its status and why it waits |
| `answer` | `peek` (`p`): `y` allow, `a` always, `n` deny | `answer`: Allow, Always or Deny on its card and its page (exact decisions for a held Claude prompt) |
| `read` | `copy.enter` (`[`): the scrollback, searchable; the pane shows the screen | `session.open`: the live screen, or the chat; `session.scrollback` (⋯ → Scrollback) is its scrollback, searchable |
| `send` | `peek` (`p`): type a reply; or type into the pane | `session.reply`: Enter sends, Shift+Enter starts a new line; `session.keys` for keys |
| `broadcast` | `broadcast` (`b`): mark sessions with `Space` in the sidebar first | `session.broadcast`: ☑ in the session list, pick sessions, then type once |
| `wait` | waived: for scripts; the TUI shows status changes as they happen and notifies you | waived: for scripts; the web app shows status changes as they happen and notifies you |
| `events` | `inspector` (`D`), Events tab (`5`) | `session.events`: ⋯ → Events |
| `report` | waived: run by programs inside a session to report their own state | waived: run by programs inside a session to report their own state |

## Attention and notifications

| Command | In the TUI | On the web |
|---|---|---|
| `inbox` | `inbox` (`i`) | `inbox`; `inbox.read` marks it read |
| `mute` | `session.mute`; a whole project or workspace: `project.mute`, `workspace.mute` | `session.mute`: ⋯ → Mute notifications |
| `notify test` | `notify.test` | gap: WEB1.6 (settings and health) |
| `usage` | shown: the status bar and the sidebar title show the rate limits of the focused session's profile | gap: WEB1.6 (settings and health) |
| `integrations ls` | `integrations` (in a split); the health badge says what's missing | gap: WEB1.6 (settings and health) |
| `integrations install` | `integrations.install` | gap: WEB1.6 (settings and health) |
| `integrations uninstall` | `integrations.uninstall` | gap: WEB1.6 (settings and health) |

## Projects, worktrees, workspaces

| Command | In the TUI | On the web |
|---|---|---|
| `project add` | `project.add`: pick from discovered repositories, or "here" | gap: WEB1.3 (worktrees and projects) |
| `project discover` | `project.add` (it discovers) | gap: WEB1.3 (worktrees and projects) |
| `project suggest` | `project.suggest`; a project's first worktree offers it on its own | gap: WEB1.3 (worktrees and projects) |
| `project ls` | shown: the sidebar lists the workspace's projects | gap: WEB1.3 (worktrees and projects) |
| `project show` | `project.show` (in a split) | gap: WEB1.3 (worktrees and projects) |
| `project rm` | `project.remove` (asks first) | gap: WEB1.3 (worktrees and projects) |
| `project trust` | `project.trust` | gap: WEB1.3 (worktrees and projects) |
| `project untrust` | `project.untrust` | gap: WEB1.3 (worktrees and projects) |
| `project config` | `project.config` (with where each value comes from, in a split) | gap: WEB1.3 (worktrees and projects) |
| `project pin` | `project.pin`: pick the profile | gap: WEB1.3 (worktrees and projects) |
| `project unpin` | `project.unpin` | gap: WEB1.3 (worktrees and projects) |
| `wt new` | `worktree.new` (`N`), or `session.new` (`n`) → "+ new worktree" | gap: WEB1.3 (worktrees and projects) |
| `wt ls` | shown: the sidebar lists every worktree under its project | gap: WEB1.3 (worktrees and projects) |
| `wt rm` | `worktree.remove` (`x` on a worktree row) | gap: WEB1.3 (worktrees and projects) |
| `wt status` | shown: the sidebar's diffstat and dirty marker; `inspector` (`D`) for the details | gap: WEB1.3 (worktrees and projects) |
| `wt diff` | `inspector` (`D`), Diff tab; `a` picks what to diff against | gap: WEB1.4 (review and ship) |
| `wt log` | `inspector` (`D`), Commits tab | gap: WEB1.4 (review and ship) |
| `wt commit` | `inspector` (`D`): `C` commits, `A` asks the agent | gap: WEB1.4 (review and ship) |
| `wt integrate` | `inspector` (`D`): `I` | gap: WEB1.4 (review and ship) |
| `wt pair` | `worktree.pair` | gap: WEB1.3 (worktrees and projects) |
| `wt unpair` | `worktree.unpair` | gap: WEB1.3 (worktrees and projects) |
| `wt run` | `script.run` (`r`), `script.pick` (`R`) | gap: WEB1.3 (worktrees and projects) |
| `wt open` | `worktree.open` (`o`) | gap: WEB1.3 (worktrees and projects) |
| `wt lock` | `worktree.lock` | gap: WEB1.3 (worktrees and projects) |
| `wt unlock` | `worktree.unlock` | gap: WEB1.3 (worktrees and projects) |
| `wt rename-branch` | `worktree.rename_branch` | gap: WEB1.3 (worktrees and projects) |
| `wt retitle` | `worktree.retitle` | gap: WEB1.3 (worktrees and projects) |
| `wt finish` | `worktree.finish` (`F` in the sidebar): a PR or integrate | gap: WEB1.3 (worktrees and projects) |
| `wt archive` | `worktree.archive` (or `x` on a merged worktree) | gap: WEB1.3 (worktrees and projects) |
| `wt restore` | `worktree.restore`: pick from the archived ones | gap: WEB1.3 (worktrees and projects) |
| `wt archives` | `worktree.restore` lists them | gap: WEB1.3 (worktrees and projects) |
| `gc` | `worktree.gc`: stale worktrees, ⏎ archives one; a merged worktree's `x` archives it | gap: WEB1.3 (worktrees and projects) |
| `ws ls` | `workspace.switch` (`w`) | gap: WEB1.3 (worktrees and projects) |
| `ws new` | `workspace.switch` (`w`) → "+ new workspace" | gap: WEB1.3 (worktrees and projects) |
| `ws set` | `workspace.profile` and `workspace.notify` | gap: WEB1.3 (worktrees and projects) |
| `ws rm` | `workspace.remove` (asks first) | gap: WEB1.3 (worktrees and projects) |
| `ws rename` | `workspace.rename` | gap: WEB1.3 (worktrees and projects) |
| `ws add` | `project.add` (adds to the current workspace) | gap: WEB1.3 (worktrees and projects) |
| `ws remove` | `project.leave`: takes the selected project out of this workspace | gap: WEB1.3 (worktrees and projects) |

## Profiles

| Command | In the TUI | On the web |
|---|---|---|
| `profile ls` | `profile.list` (in a split) | gap: WEB1.6 (settings and health) |
| `profile show` | `profile.show`: pick it | gap: WEB1.6 (settings and health) |
| `profile new` | `profile.new` | gap: WEB1.6 (settings and health) |
| `profile rm` | `profile.remove`: pick it | gap: WEB1.6 (settings and health) |
| `profile rename` | `profile.rename` | gap: WEB1.6 (settings and health) |
| `profile env` | waived: prints `export` lines for your shell to `eval`; sessions get the environment already | waived: prints `export` lines for a shell on the computer to `eval` |
| `profile login` | `profile.login`: pick the profile and the agent; it logs in, in a split (a health finding offers it too) | gap: WEB1.6 (settings and health) |
| `profile sync` | `profile.sync` | gap: WEB1.6 (settings and health) |
| `profile detect` | `first.run` finds them, as accounts to tick | gap: WEB1.6 (settings and health) |
| `profile doctor` | `health`: the badge's findings include each profile's logins | gap: WEB1.6 (settings and health) |

## Review and ship

| Command | In the TUI | On the web |
|---|---|---|
| `review add` | `inspector` (`D`): `c` on a line | gap: WEB1.4 (review and ship) |
| `review ls` | `inspector` (`D`): comments show on their lines | gap: WEB1.4 (review and ship) |
| `review rm` | `inspector` (`D`): `x` on a comment | gap: WEB1.4 (review and ship) |
| `review send` | `inspector` (`D`): `S` | gap: WEB1.4 (review and ship) |
| `review queue` | `review.queue` (`Q`) | gap: WEB1.4 (review and ship) |
| `review done` | `review.queue` (`Q`): `d` | gap: WEB1.4 (review and ship) |
| `pr create` | `inspector` (`D`): `P` | gap: WEB1.4 (review and ship) |
| `pr status` | `inspector` (`D`), Checks tab; the worktree row shows `#N` and its checks | gap: WEB1.4 (review and ship) |
| `pr checks` | `inspector` (`D`), Checks tab | gap: WEB1.4 (review and ship) |
| `pr fix` | `inspector` (`D`), Checks tab: `f` | gap: WEB1.4 (review and ship) |
| `pr automerge` | `inspector` (`D`), Checks tab: `m` | gap: WEB1.4 (review and ship) |
| `pr watch` | `inspector` (`D`), Checks tab: `w` | gap: WEB1.4 (review and ship) |
| `watch-pr` | `inspector` (`D`), Checks tab: `w` | gap: WEB1.4 (review and ship) |
| `pr reply` | `pr.reply` | gap: WEB1.4 (review and ship) |
| `pr open` | `pr.open`: its worktree and session, or a new worktree on it | gap: WEB1.4 (review and ship) |

## Orchestration

| Command | In the TUI | On the web |
|---|---|---|
| `fanout` | `runs` (`O`): `F`, then the agents and the prompt | gap: WEB1.5 (runs and change sets) |
| `compare` | `runs` (`O`): `c` on a fan-out | gap: WEB1.5 (runs and change sets) |
| `pick` | `runs` (`O`), compare: `m` merge, `C` cherry-pick, `p` PR | gap: WEB1.5 (runs and change sets) |
| `discard` | `runs` (`O`), compare: `d` one, `X` the rest | gap: WEB1.5 (runs and change sets) |
| `iterate` | `runs` (`O`), compare: `i` one, `I` all | gap: WEB1.5 (runs and change sets) |
| `run ls` | `runs` (`O`) | gap: WEB1.5 (runs and change sets) |
| `run show` | `runs` (`O`): `⏎` on a run | gap: WEB1.5 (runs and change sets) |
| `run events` | `runs` (`O`): `⏎` on a run shows its steps and why they ended | gap: WEB1.5 (runs and change sets) |
| `run wait` | waived: for scripts; gates and finished runs reach the inbox and notifications | waived: for scripts; gates and finished runs reach the inbox and notifications |
| `run cancel` | `runs` (`O`): `x` | gap: WEB1.5 (runs and change sets) |
| `run retry` | `runs` (`O`): `r` on a step | gap: WEB1.5 (runs and change sets) |
| `run fork` | `run.fork` | gap: WEB1.5 (runs and change sets) |
| `run lead` | `session.lead` | gap: WEB1.5 (runs and change sets) |
| `recipe ls` | `recipe.list` (in a split) | gap: WEB1.5 (runs and change sets) |
| `recipe show` | `recipe.show` | gap: WEB1.5 (runs and change sets) |
| `recipe run` | `recipe.run`: its name and variables; then the runs view (`O`) | gap: WEB1.5 (runs and change sets) |
| `recipe validate` | waived: for recipe authors, in their editor or CI | waived: for recipe authors, in their editor or CI |
| `recipe graph` | waived: draws a recipe as Mermaid text for documents | waived: draws a recipe as Mermaid text for documents |
| `gate ls` | `runs` (`O`): open gates show on their run; the status bar counts them | gap: WEB1.5 (runs and change sets) |
| `gate resolve` | `runs` (`O`): `1`–`9` answer, `n` with a note | gap: WEB1.5 (runs and change sets) |
| `msg send` | `session.message`; to a change set's members, `sets` `m` | `session.message`: ⋯ → Message |
| `msg check` | waived: an agent's own mailbox; your messages reach the inbox | waived: an agent's own mailbox; your messages reach the inbox |
| `msg ls` | `inbox` (`i`): messages to you | `inbox`: messages to you |
| `msg ack` | waived: an agent acknowledging its own mail | waived: an agent acknowledging its own mail |
| `lease claim` | waived: agents claim the files they edit | waived: agents claim the files they edit |
| `lease release` | waived: agents release their own claims | waived: agents release their own claims |
| `lease ls` | `leases` (in a split) | gap: WEB1.3 (worktrees and projects) |
| `radar` | `radar` | gap: WEB1.3 (worktrees and projects) |
| `checkpoint ls` | `checkpoint.list` | gap: WEB1.3 (worktrees and projects) |
| `checkpoint diff` | `checkpoint.diff` | gap: WEB1.3 (worktrees and projects) |
| `checkpoint restore` | `checkpoint.restore` (it asks first, in a split) | gap: WEB1.3 (worktrees and projects) |
| `verify` | `session.verify` | `session.verify`: ⋯ → Verify when done |
| `mcp` | waived: the MCP server a lead agent talks to; ptyro starts it for the agent | waived: the MCP server a lead agent talks to; ptyro starts it for the agent |

## Change sets

| Command | In the TUI | On the web |
|---|---|---|
| `cs new` | `sets.new`: its name, projects and goal | gap: WEB1.5 (runs and change sets) |
| `cs templates` | `sets.templates` | gap: WEB1.5 (runs and change sets) |
| `cs ls` | `sets` (`S`) | gap: WEB1.5 (runs and change sets) |
| `cs show` | `sets` (`S`): `⏎` on a set | gap: WEB1.5 (runs and change sets) |
| `cs log` | `sets` (`S`): the ledger, as a timeline | gap: WEB1.5 (runs and change sets) |
| `cs msg` | `sets` (`S`): `m` | gap: WEB1.5 (runs and change sets) |
| `cs check` | `sets` (`S`): `c` | gap: WEB1.5 (runs and change sets) |
| `cs checkpoint` | `sets` (`S`): `p` | gap: WEB1.5 (runs and change sets) |
| `cs checkpoints` | `set.checkpoints` | gap: WEB1.5 (runs and change sets) |
| `cs restore` | `set.restore` | gap: WEB1.5 (runs and change sets) |
| `cs diff` | `sets` (`S`): `⏎` on a member, then `inspector` (`D`) on its worktree | gap: WEB1.5 (runs and change sets) |
| `cs contract show` | `set.contract` → show | gap: WEB1.5 (runs and change sets) |
| `cs contract edit` | `set.contract` → edit (your editor, in a split) | gap: WEB1.5 (runs and change sets) |
| `cs contract diff` | `set.contract` → diff | gap: WEB1.5 (runs and change sets) |
| `cs contract history` | `set.contract` → history | gap: WEB1.5 (runs and change sets) |
| `cs contract approve` | `sets` (`S`): `1`–`9` answer the contract gate | gap: WEB1.5 (runs and change sets) |
| `cs contract reject` | `sets` (`S`): answer the contract gate with a note | gap: WEB1.5 (runs and change sets) |
| `cs contract submit` | waived: the lead agent submits the contract | waived: the lead agent submits the contract |
| `cs contract revise` | waived: the lead agent revises the contract | waived: the lead agent revises the contract |
| `cs ready` | waived: run by a member agent inside the set | waived: run by a member agent inside the set |
| `cs blocked` | waived: run by a member agent inside the set | waived: run by a member agent inside the set |
| `cs assign` | waived: run by the lead agent inside the set | waived: run by the lead agent inside the set |
| `cs ask` | waived: run by an agent inside the set; the question reaches you as a gate | waived: run by an agent inside the set; the question reaches you as a gate |
| `cs overlay` | `set.overlay` | gap: WEB1.5 (runs and change sets) |
| `cs commit` | `set.commit` | gap: WEB1.5 (runs and change sets) |
| `cs push` | `set.push` (or the push gate in `sets`) | gap: WEB1.5 (runs and change sets) |
| `cs pr` | `set.pr` | gap: WEB1.5 (runs and change sets) |
| `cs land` | `set.land` (or the land gate in `sets`) | gap: WEB1.5 (runs and change sets) |
| `cs add` | `set.add` | gap: WEB1.5 (runs and change sets) |
| `cs drop` | `set.drop` | gap: WEB1.5 (runs and change sets) |
| `cs rm` | `set.remove` | gap: WEB1.5 (runs and change sets) |

## Remote, phone and recordings

| Command | In the TUI | On the web |
|---|---|---|
| `serve` | `remote.serve` (in a split) | waived: it serves the web app; the web app can't start the thing it runs on |
| `pair` | `remote.pair`: the QR code, in a split, to scan | `pair`: this device's half (the link or QR code); making a code for another device: gap WEB1.6 |
| `devices ls` | `devices.list` | gap: WEB1.6 (settings and health) |
| `devices revoke` | `devices.revoke` | gap: WEB1.6 (settings and health) |
| `devices push-test` | `devices.push_test` | `notifications`: turning them on sends this device a test |
| `proxy` | waived: the bridge `--remote` runs over ssh on the other machine | waived: the bridge `--remote` runs over ssh between two computers |
| `replay` | `session.replay` (plays in a split) | `session.replay`: ⋯ → Replay: its recording as a player with a scrubber and speeds; `session.history` lists earlier sessions |
| `export` | `session.export`: the format and the file | `session.export`: ⋯ → Save its recording (text or asciicast, as a download) |

## Setup, config and the daemon

| Command | In the TUI | On the web |
|---|---|---|
| `init` | `first.run`: the first run as a screen (it opens on its own the first time); terminal checks and shell extras are `doctor`'s (UX1.20) | gap: WEB1.6 (settings and health) |
| `doctor` | `health` (`M`): the badge's findings, each with its fix; `doctor` runs the full one in a split | gap: WEB1.6 (settings and health) |
| `tour` | `help.tour` (the TUI steps aside for the tour, then comes back) | gap: WEB1.6 (settings and health) |
| `config edit` | `config.edit` (validated when you save; changes apply live) | gap: WEB1.6 (settings and health) |
| `config check` | `config.edit`: saving checks it, and a bad config shows as an error | gap: WEB1.6 (settings and health) |
| `config show` | `config.show` (in a split) | gap: WEB1.6 (settings and health) |
| `config path` | `config.edit` (opens it) | gap: WEB1.6 (settings and health) |
| `completions` | waived: shell setup, for typing commands in a shell | waived: shell setup on the computer, for typing commands there |
| `shell-init` | `health` (`M`): when it's missing, a finding whose fix adds the line to your rc file | waived: a line in the computer's shell rc file, for its shells |
| `daemon start` | waived: the TUI starts the daemon when it isn't running | waived: `serve` runs with the daemon; with no daemon there is no web app to ask |
| `daemon stop` | `app.quit` (`C-d` after the prefix): asks, then stops every session and the daemon and exits | gap: WEB1.6 (settings and health) |
| `daemon restart` | `daemon.restart` (in a split; this TUI reconnects) | gap: WEB1.6 (settings and health) |
| `daemon status` | `daemon.status`; the health badge has its warnings | gap: WEB1.6 (settings and health) |
| `daemon logs` | `daemon.logs` (followed, in a split) | gap: WEB1.6 (settings and health) |
| `daemon upgrade` | `daemon.upgrade`; a daemon running another binary is a health finding whose fix this is | gap: WEB1.6 (settings and health) |
| `daemon install-service` | `daemon.service` | gap: WEB1.6 (settings and health) |
| `daemon uninstall-service` | `daemon.unservice` | gap: WEB1.6 (settings and health) |
