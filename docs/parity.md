<img src="logos/ptyro-icon.png" alt="ptyro" width="72" align="right">


# Every command, in the TUI and on the web

The CLI, the TUI and the web app are all first class (SPEC §1.3): what the CLI does, you can do
in the TUI and on the web. This page lists every command, how the
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
forms. A TUI `gap:` always fails: the CLI and the TUI are always in parity. A web gap fails too
(`WEB_STRICT=0` only lists them, while one is being worked on). When you add a command, add
its row: its TUI action and its web one, or `waived:` and why.

## Sessions

| Command | In the TUI | On the web |
|---|---|---|
| `new` | `session.new` (`n`): say what to do, ⏎ (or `C-o` to pick the agent, a shell or a command, and where); `worktree.new` (`N`) for a new worktree; `--again` is `session.again` (`C-n`): another of what the workspace last started, here, full sized | `session.new` (＋ in the session list): the task, the agent, and where: a new worktree (from the main branch, a PR, an issue, a branch or your changes), an existing one, or a shell or command; `--again` is `session.again`: the `+ claude` button on a project's and a worktree's page, and a session's ⋯ → Another … |
| `attach` | `session.find` (`/`), the sidebar (`Tab`, `⏎`), `1`–`9`, `j`/`k` | `session.open`: tap it in the list; its live screen, or its chat. The screen is a terminal: `term.type` (tap it and type: every key goes to the session as it is pressed, with `term.keys` for Esc, Ctrl, Alt, arrows and F-keys), `term.mouse`, `term.resize` |
| `list` | `focus.toggle`: the sidebar lists every session with its status; `session.find` (`/`) searches them; `--status running` / `active` is `sidebar.filter` (`f`, or click `running` / `active` under the sidebar title): only the live sessions, or only the busy ones, each machine, project and worktree with none hidden | `sessions`: every session with its status, who needs you first; `session.find` (`/`) filters them; `sidebar.filter`: the `all` / `running` / `active` chips |
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
| `shell` | `shell.drawer` (`C-t`): the worktree's quick terminal (one per worktree, only in a worktree) in a drawer under a pane of its project; `shell.pane` (`T`) full sized; `shell.here` (`⏎` on its `›_ quick shell` line) as `[ui] shell_style` says; again focuses it, then hides it (a panel goes back). `shell.new` (`!`) is another shell of its own, not the quick terminal. `--read` is `copy.enter` (`[`) in the drawer | `session.shell`: ⋯ → Its worktree's quick terminal (a terminal page, with keys and a reply box); `shell.here` and `shell.pane`: the Quick shell buttons on a project's and a worktree's page (the web has no split panes, so a shell is always its own page); `shell.new`: their New shell button |
| `summarize` | `session.summarize` (the row menu, the palette); with `[summaries] enabled` the sidebar shows one (`≈`) after every turn | `session.summarize`: the session's ⋯ menu |
| `status` | `session.explain` (`e` in the sidebar) says why; the sidebar and pane headers show the status | `sessions`: each card and the session's page say its status and why it waits |
| `answer` | `peek` (`p`): `y` allow, `a` always, `n` deny | `answer`: Allow, Always or Deny on its card and its page (exact decisions for a held Claude prompt) |
| `read` | `copy.enter` (`[`): the scrollback, searchable; the pane shows the screen | `session.open`: the live screen, or the chat; `term.scroll` (wheel or drag: its history), `term.select` and `term.copy` (long-press and drag, double-click a word), `term.find` (Aa → Find in the history), `term.link`; `session.scrollback` (⋯ → Scrollback) is all of it as text, searchable |
| `send` | `peek` (`p`): type a reply; or type into the pane | `session.reply`: Enter sends, Shift+Enter starts a new line; `term.type` types into the session key by key (`term.paste` pastes, `term.keys` for the keys a phone lacks); `session.keys` for a chat's keys |
| `broadcast` | `broadcast` (`b`): mark sessions with `Space` in the sidebar first | `session.broadcast`: ☑ in the session list, pick sessions, then type once |
| `wait` | waived: for scripts; the TUI shows status changes as they happen and notifies you | waived: for scripts; the web app shows status changes as they happen and notifies you |
| `events` | `inspector` (`D`), Events tab (`5`) | `session.events`: ⋯ → Events |
| `report` | waived: run by programs inside a session to report their own state | waived: run by programs inside a session to report their own state |

## Scratchpad

The scratchpad is one notes file (`$PTYRO_SCRATCHPAD`), the same in every session (ROADMAP NOTE1).

| Command | In the TUI | On the web |
|---|---|---|
| `scratchpad edit` | `scratchpad.toggle` (`C-q ;`): the pad in your editor, in a drawer under the pane (or a popup, or a pane: `[scratchpad] mode`); again focuses it, then hides it (the editor keeps running). `--pane` is the same session | `scratchpad`: the Notes view, edited in the page; it saves as you type and merges with other writers |
| `scratchpad path` | shown: its path heads the open-items list (`scratchpad.items`) and the history list | shown: its path heads the Notes view |
| `scratchpad cat` | `scratchpad.toggle`: you read it in the editor; `scratchpad.items` lists the open items | `scratchpad`: the Notes view, or its Preview |
| `scratchpad add` | `scratchpad.add` (`C-q '`): one line, stamped with where you are, without opening the editor; `scratchpad.note` (the row menu, `a` in copy mode, `n` in the inbox) adds the selected row's task, a selection or an inbox entry | `scratchpad.add`: the quick-add box on the session list and in the Notes view; the share sheet adds to it; `scratchpad.note` on a session card |
| `scratchpad todo` | `scratchpad.items` (`C-q "`): the open items, searchable | `scratchpad.items`: the open items beside the text |
| `scratchpad done` | `scratchpad.items` (`Ctrl-X` ticks, or unticks) | `scratchpad.tick`: tap a checkbox in the Preview |
| `scratchpad start` | `scratchpad.items` (`⏎` starts the item as a task in a new worktree) | `scratchpad.start`: Start on an open item opens the new-task box with its text |
| `scratchpad log` | `scratchpad.history` | `scratchpad.history`: saved copies |
| `scratchpad restore` | `scratchpad.history` (`⏎` puts one back) | `scratchpad.history` |

## Attention and notifications

| Command | In the TUI | On the web |
|---|---|---|
| `inbox` | `inbox` (`i`) | `inbox`; `inbox.read` marks it read |
| `mute` | `session.mute`; a whole project or workspace: `project.mute`, `workspace.mute` | `session.mute`: ⋯ → Mute notifications |
| `notify test` | `notify.test` | `notify.test`: More → Notifications → Send a test |
| `usage` | shown: the status bar and the sidebar title show the rate limits of the focused session's profile | `usage`: More → Usage: each account's limits as meters |
| `integrations ls` | `integrations` (full sized); the health badge says what's missing | `integrations`: More → Integrations |
| `integrations install` | `integrations.install` | `integrations`: More → Integrations: tap one → Install |
| `integrations uninstall` | `integrations.uninstall` | `integrations`: More → Integrations: tap one → Uninstall |

## Projects, worktrees, workspaces

| Command | In the TUI | On the web |
|---|---|---|
| `project add` | `project.add`: pick from discovered repositories, or "here" | `project.add`: ＋ on the Projects tab: pick a repository found on the computer, or give a path |
| `project discover` | `project.add` (it discovers) | `project.add`: it lists the repositories it found |
| `project suggest` | `project.suggest`; a project's first worktree offers it on its own | `project.suggest`: a project's ⋯ menu |
| `project ls` | shown: the sidebar lists the workspace's projects | `projects`: the Projects tab lists every project with its worktrees; its workspace chips filter them (`workspace.switch`) |
| `project show` | `project.show` (full sized) | `project.show`: tap a project's name |
| `project rm` | `project.remove` (asks first) | `project.remove`: a project's ⋯ menu (asks first) |
| `project trust` | `project.trust` | `project.trust`: a project's page: review the scripts, then trust |
| `project untrust` | `project.untrust` | `project.untrust`: a project's page |
| `project config` | `project.config` (with where each value comes from, full sized) | `project.config`: a project's ⋯ menu, with where each value comes from |
| `project pin` | `project.pin`: pick the profile | `project.pin`: a project's ⋯ menu: pick the profile |
| `project unpin` | `project.unpin` | `project.pin`: a project's ⋯ menu → Unpin |
| `wt new` | `worktree.new` (`N`), or `session.new` (`n`) → "+ new worktree" | `worktree.new`: ＋ Task on a project, or `session.new` with "in a new worktree" |
| `wt ls` | shown: the sidebar lists every worktree under its project, each with what is left to do: `↑2` unpushed, `not pushed ↑2` (no upstream yet), `● 3` uncommitted, a quiet `✓ pushed` when it is all done (UX2.2) | `projects`: every worktree under its project, with its changes, PR and sessions |
| `wt rm` | `worktree.remove` (`x` on a worktree row) | `worktree.remove`: a worktree's ⋯ menu (offers to archive when it has unsaved work) |
| `wt status` | shown: the sidebar's and the pane header's `↑N`, `●`, `✓ pushed` and `+N −N` (while something is left); `inspector` (`D`) for the details | `worktree.status`: a worktree's page, and its row on the Projects tab |
| `wt diff` | `inspector` (`D`), Diff tab; `a` picks what to diff against | `review.diff`: a worktree's Changes tab; `review.against` picks what to diff against |
| `wt log` | `inspector` (`D`), Commits tab | `worktree.log`: a worktree's Commits tab |
| `wt commit` | `inspector` (`D`): `C` commits, `A` asks the agent | `worktree.commit`: Commit… on the Changes tab (pick the files, write the message); `worktree.ask_commit` asks the agent |
| `wt integrate` | `inspector` (`D`): `I` | `worktree.integrate`: Merge into main… on the Commits tab |
| `wt pair` | `worktree.pair` | `worktree.pair`: a worktree's ⋯ menu |
| `wt unpair` | `worktree.unpair` | `worktree.unpair`: a worktree's ⋯ menu |
| `wt run` | `script.run` (`r`), `script.pick` (`R`) | `worktree.run`: a worktree's ⋯ menu: pick a script; it opens its session |
| `wt open` | `worktree.open` (`o`) | `worktree.open`: a worktree's ⋯ menu: pick the editor (a terminal editor opens as a session) |
| `wt lock` | `worktree.lock` | `worktree.lock`: a worktree's ⋯ menu |
| `wt unlock` | `worktree.unlock` | `worktree.unlock`: a worktree's ⋯ menu |
| `wt rename-branch` | `worktree.rename_branch` | `worktree.rename_branch`: a worktree's ⋯ menu |
| `wt retitle` | `worktree.retitle` | `worktree.retitle`: a worktree's ⋯ menu |
| `wt finish` | `worktree.finish` (`F` in the sidebar): a PR or integrate | `worktree.finish`: Finish on a worktree's page: a PR, or merge into the main branch |
| `wt archive` | `worktree.archive` (or `x` on a merged worktree) | `worktree.archive`: a worktree's ⋯ menu |
| `wt restore` | `worktree.restore`: pick from the archived ones | `worktree.restore`: Archived (Projects tab): tap one |
| `wt archives` | `worktree.restore` lists them | `worktree.restore`: Archived lists them |
| `gc` | `worktree.gc`: stale worktrees, ⏎ archives one; a merged worktree's `x` archives it | `worktree.gc`: 🧹 on the Projects tab: stale worktrees, archive one |
| `ws ls` | `workspace.switch` (`w`) | `workspace.switch`: chips on the Projects tab; Workspaces lists them |
| `ws new` | `workspace.switch` (`w`) → "+ new workspace" | `workspace.new`: ＋ on Workspaces |
| `ws set` | `workspace.profile` and `workspace.notify` | `workspace.set`: a workspace's menu: its profile and notifications |
| `ws rm` | `workspace.remove` (asks first) | `workspace.remove`: a workspace's menu (asks first) |
| `ws rename` | `workspace.rename` | `workspace.rename`: a workspace's menu |
| `ws add` | `project.add` (adds to the current workspace) | `workspace.member`: a project's ⋯ menu → Workspaces |
| `ws remove` | `project.leave`: takes the selected project out of this workspace | `workspace.member`: a project's ⋯ menu → Workspaces |

## Profiles

| Command | In the TUI | On the web |
|---|---|---|
| `profile ls` | `profile.list` (full sized) | `profile.list`: More → Profiles |
| `profile show` | `profile.show`: pick it | `profile.show`: tap a profile |
| `profile new` | `profile.new` | `profile.new`: ＋ on Profiles, adopting directories you have |
| `profile rm` | `profile.remove`: pick it | `profile.remove`: a profile's page (asks; offers to force when it's in use) |
| `profile rename` | `profile.rename` | `profile.rename`: a profile's page |
| `profile env` | waived: prints `export` lines for your shell to `eval`; sessions get the environment already | waived: prints `export` lines for a shell on the computer to `eval` |
| `profile login` | `profile.login`: pick the profile and the agent; it logs in, full sized (a health finding offers it too) | `profile.login`: a profile's page, or a health finding's fix: the tool's own login, in a session |
| `profile sync` | `profile.sync` | `profile.sync`: a profile's page |
| `profile detect` | `first.run` finds them, as accounts to tick | `profile.detect`: 🔍 on Profiles: the accounts found, to tick |
| `profile doctor` | `health`: the badge's findings include each profile's logins | `profile.doctor`: Check on a profile's page |

## Review and ship

| Command | In the TUI | On the web |
|---|---|---|
| `review add` | `inspector` (`D`): `c` on a line | `review.add`: tap a line on the Changes tab |
| `review ls` | `inspector` (`D`): comments show on their lines | `review.list`: comments show under their lines, counted in a bar |
| `review rm` | `inspector` (`D`): `x` on a comment | `review.rm`: ✕ on a comment, or Clear |
| `review send` | `inspector` (`D`): `S` | `review.send`: Send to agent, on the comments bar |
| `review queue` | `review.queue` (`Q`) | `review.queue`: More → Review queue |
| `review done` | `review.queue` (`Q`): `d` | `review.done`: a queue item's menu → Mark reviewed |
| `pr create` | `inspector` (`D`): `P` | `pr.create`: the Checks tab, with no PR yet |
| `pr status` | `inspector` (`D`), Checks tab; the worktree row shows `#N` and its checks | `pr.checks`: a worktree's Checks tab; its row shows `#N` and its checks |
| `pr checks` | `inspector` (`D`), Checks tab | `pr.checks`: a worktree's Checks tab |
| `pr fix` | `inspector` (`D`), Checks tab: `f` | `pr.fix`: the Checks tab: Fix N failing |
| `pr automerge` | `inspector` (`D`), Checks tab: `m` | `pr.automerge`: the Checks tab |
| `pr watch` | `inspector` (`D`), Checks tab: `w` | `pr.watch`: the Checks tab |
| `watch-pr` | `inspector` (`D`), Checks tab: `w` | `pr.watch`: the Checks tab |
| `pr reply` | `pr.reply` | `pr.reply`: the Checks tab: Reply… |
| `pr open` | `pr.open`: its worktree and session, or a new worktree on it | `pr.open`: ⎘ on the Review queue: its worktree and session, or a new worktree on it |

## Orchestration

| Command | In the TUI | On the web |
|---|---|---|
| `fanout` | `runs` (`O`): `F`, then the agents and the prompt | `fanout`: ＋ on the Runs tab → A fan-out: the prompt, the agents, a test and a judge |
| `compare` | `runs` (`O`): `c` on a fan-out | `compare`: a run's Compare button |
| `pick` | `runs` (`O`), compare: `m` merge, `C` cherry-pick, `p` PR | `pick`: Take on a candidate: merge, cherry-pick, a PR, or some of its files |
| `discard` | `runs` (`O`), compare: `d` one, `X` the rest | `discard`: a candidate's Discard, or Discard the rest |
| `iterate` | `runs` (`O`), compare: `i` one, `I` all | `iterate`: Follow up on a candidate, or on all of them |
| `run ls` | `runs` (`O`) | `runs`: the Runs tab, with the open gates on top |
| `run show` | `runs` (`O`): `⏎` on a run | `run.show`: tap a run: its steps, why they ended, its forks |
| `run events` | `runs` (`O`): `⏎` on a run shows its steps and why they ended | `run.events`: a run's Events |
| `run wait` | waived: for scripts; gates and finished runs reach the inbox and notifications | waived: for scripts; gates and finished runs reach the inbox and notifications |
| `run cancel` | `runs` (`O`): `x` | `run.cancel`: a run's Cancel |
| `run retry` | `runs` (`O`): `r` on a step | `run.retry`: a step's menu → Run it again |
| `run fork` | `run.fork` | `run.fork`: a step's menu → Fork the run from here |
| `run lead` | `session.lead` | `run.lead`: ＋ on the Runs tab → A lead |
| `recipe ls` | `recipe.list` (full sized) | `recipe.list`: 📜 on the Runs tab |
| `recipe show` | `recipe.show` | `recipe.show`: a recipe's menu |
| `recipe run` | `recipe.run`: its name and variables; then the runs view (`O`) | `recipe.run`: a recipe's menu: its variables, then the run |
| `recipe validate` | waived: for recipe authors, in their editor or CI | waived: for recipe authors, in their editor or CI |
| `recipe graph` | waived: draws a recipe as Mermaid text for documents | waived: draws a recipe as Mermaid text for documents |
| `gate ls` | `runs` (`O`): open gates show on their run; the status bar counts them | `gate.list`: open gates sit on top of the Runs and Sets tabs, and on their run |
| `gate resolve` | `runs` (`O`): `1`–`9` answer, `n` with a note | `gate.resolve`: answer on the gate's card, with a note if you like |
| `msg send` | `session.message`; to a change set's members, `sets` `m` | `session.message`: ⋯ → Message |
| `msg check` | waived: an agent's own mailbox; your messages reach the inbox | waived: an agent's own mailbox; your messages reach the inbox |
| `msg ls` | `inbox` (`i`): messages to you | `inbox`: messages to you |
| `msg ack` | waived: an agent acknowledging its own mail | waived: an agent acknowledging its own mail |
| `lease claim` | waived: agents claim the files they edit | waived: agents claim the files they edit |
| `lease release` | waived: agents release their own claims | waived: agents release their own claims |
| `lease ls` | `leases` (full sized) | `leases`: Leases on the Projects tab |
| `radar` | `radar` | `radar`: Radar on the Projects tab, or a project's ⋯ menu |
| `checkpoint ls` | `checkpoint.list` | `checkpoint.list`: a worktree's Checkpoints tab |
| `checkpoint diff` | `checkpoint.diff` | `checkpoint.diff`: a checkpoint's menu |
| `checkpoint restore` | `checkpoint.restore` (it asks first, full sized) | `checkpoint.restore`: a checkpoint's menu (asks first; the state now is checkpointed) |
| `verify` | `session.verify` | `session.verify`: ⋯ → Verify when done |
| `mcp` | waived: the MCP server a lead agent talks to; ptyro starts it for the agent | waived: the MCP server a lead agent talks to; ptyro starts it for the agent |

## Change sets

| Command | In the TUI | On the web |
|---|---|---|
| `cs new` | `sets.new`: its name, projects and goal | `set.new`: ＋ on the Sets tab: its name, projects or template, goal and layout |
| `cs templates` | `sets.templates` | `set.templates`: 📋 on the Sets tab |
| `cs ls` | `sets` (`S`) | `sets`: the Sets tab |
| `cs show` | `sets` (`S`): `⏎` on a set | `set.show`: tap a set: members, gates, issues with what to do |
| `cs log` | `sets` (`S`): the ledger, as a timeline | `set.log`: a set's ⋯ → Ledger |
| `cs msg` | `sets` (`S`): `m` | `set.message`: Message on a set, or on a member |
| `cs check` | `sets` (`S`): `c` | `set.check`: a set's Check |
| `cs checkpoint` | `sets` (`S`): `p` | `set.checkpoint`: a set's ⋯ → Checkpoint now |
| `cs checkpoints` | `set.checkpoints` | `set.checkpoints`: a set's ⋯ → Checkpoints |
| `cs restore` | `set.restore` | `set.restore`: a checkpoint's menu (asks first) |
| `cs diff` | `sets` (`S`): `⏎` on a member, then `inspector` (`D`) on its worktree | `set.diff`: a member's menu → What it changed |
| `cs contract show` | `set.contract` → show | `set.contract`: a set's Contract page |
| `cs contract edit` | `set.contract` → edit (your editor, full sized) | `set.contract_edit`: the Contract page: Edit saves a revision |
| `cs contract diff` | `set.contract` → diff | `set.contract`: the Contract page: Diff |
| `cs contract history` | `set.contract` → history | `set.contract`: the Contract page: History, tap a revision |
| `cs contract approve` | `sets` (`S`): `1`–`9` answer the contract gate | `set.contract_approve`: Approve on the Contract page, or on the gate's card |
| `cs contract reject` | `sets` (`S`): answer the contract gate with a note | `set.contract_reject`: Send back… on the Contract page, with a note |
| `cs contract submit` | waived: the lead agent submits the contract | waived: the lead agent submits the contract |
| `cs contract revise` | waived: the lead agent revises the contract | waived: the lead agent revises the contract |
| `cs ready` | waived: run by a member agent inside the set | waived: run by a member agent inside the set |
| `cs blocked` | waived: run by a member agent inside the set | waived: run by a member agent inside the set |
| `cs assign` | waived: run by the lead agent inside the set | waived: run by the lead agent inside the set |
| `cs ask` | waived: run by an agent inside the set; the question reaches you as a gate | waived: run by an agent inside the set; the question reaches you as a gate |
| `cs overlay` | `set.overlay` | `set.overlay`: a set's ⋯ menu, or a member's |
| `cs commit` | `set.commit` | `set.commit`: a set's Ship… → Commit |
| `cs push` | `set.push` (or the push gate in `sets`) | `set.push`: a set's Ship… → Push (or the push gate's card) |
| `cs pr` | `set.pr` | `set.pr`: a set's Ship… → Open the pull requests |
| `cs land` | `set.land` (or the land gate in `sets`) | `set.land`: a set's Ship… → Land (or the land gate's card) |
| `cs add` | `set.add` | `set.add`: a set's ⋯ → Add a project |
| `cs drop` | `set.drop` | `set.drop`: a member's menu → Take it out of the set |
| `cs rm` | `set.remove` | `set.remove`: a set's ⋯ → Remove the set |

## Remote, phone and recordings

| Command | In the TUI | On the web |
|---|---|---|
| `serve` | `remote.serve` (full sized) | waived: it serves the web app; the web app can't start the thing it runs on |
| `web` | `remote.web` (is it up, where, who can reach it, what next), `remote.web_on`, `remote.web_off` (full sized); `remote.access` and `remote.port` in Settings → remote (a dropdown and a number; search "tailscale" or "wifi"); `remote.web` says which certificate is in use; the health badge has its findings | `web.status`: More → Web app: up or not, the address (copy it), who can reach it with a Change button, the port, who runs it, the log (`web.logs`); `web.on` (asks who can reach it: my Tailscale network, every network, this computer only) and `web.off`; the certificate in use is on the page (Certificate, Tailscale certificate); the same settings are in Settings → remote |
| `pair` | `remote.pair`: the QR code, full sized, to scan (full scope by default, the warning shows above the code) | `pair`: this device's half (the link or QR code); `pair.device` makes a link and a QR code for another device (More → Devices → ＋; `full` preselected, the warning in the form and the sheet; a name that exists is replaced) |
| `devices ls` | `devices.list` | `devices.list`: More → Devices |
| `devices revoke` | `devices.revoke` | `devices.revoke`: a device's menu |
| `devices push-test` | `devices.push_test` | `notifications`: turning them on sends this device a test |
| `machine ls` | `machine.list` (full sized); the sidebar shows every machine's workspaces, each header with its machine and how its link stands | `machine.list`: More → Machines |
| `machine add` | `machine.add`: `ssh://HOST` or the code, then `machine add` full sized (it claims the code from this machine) | `machine.add`: More → Machines → ＋ (the claim runs on this machine, `full` scope) |
| `machine rm` | `machine.remove`: the row menu of one of its workspaces, or the palette (asks first) | `machine.remove`: a machine's menu → Stop showing it here |
| `machine hide` | `machine.hide`: the row menu of one of its workspaces | `machine.hide`: a machine's menu → Hide WORKSPACE here |
| `machine unhide` | `machine.unhide`: a picker of the hidden workspaces | `machine.unhide`: a machine's menu → Show WORKSPACE again |
| `machine retry` | `machine.retry`: first in the row menu of its workspace while it's down | `machine.retry`: a machine's menu → Try its link now |
| `machine pair` | `machine.pair` (the code, full sized) | `machine.pair`: More → Machines → Make a code |
| `proxy` | waived: the bridge `--remote` runs over ssh on the other machine | waived: the bridge `--remote` runs over ssh between two computers |
| `replay` | `session.replay` (plays full sized) | `session.replay`: ⋯ → Replay: its recording as a player with a scrubber and speeds; `session.history` lists earlier sessions |
| `export` | `session.export`: the format and the file | `session.export`: ⋯ → Save its recording (text or asciicast, as a download) |

## Setup, config and the daemon

| Command | In the TUI | On the web |
|---|---|---|
| `init` | `first.run`: the first run as a screen (it opens on its own the first time); terminal checks and shell extras are `doctor`'s (UX1.20) | `first.run`: More → Set up ptyro (the Sessions tab offers it with no project): your agents, accounts and repositories (the web app for a phone is `web.on`, More → Web app) |
| `about` | `app.about` (`I`, the palette, the sidebar's `I`): version, the repository and issues as links (`⏎` opens, `y` copies), where config, data, logs and the socket live, the daemon and web app (and `U` to switch a daemon of another version), the tools found; `c` copies all of it for a bug report | `app.about`: More → About (and the version at the foot of More): the same facts, the addresses as links with Copy, "Copy all, for a bug report", and Upgrade the daemon when it is another version |
| `doctor` | `health` (`M`): the badge's findings, each with its fix; `doctor` runs the full one full sized | `health`: More → Health: every check with its fix (log in, edit the config, restart…) |
| `tour` | `help.tour` (the TUI steps aside for the tour, then comes back) | `help.tour`: More → Take the tour: a ptyro of its own on a free local port, opened in a new tab with a coach |
| `config edit` | `config.edit` (validated when you save; changes apply live); `config.panel` has `e` for the file and `o` to open it at a setting | `config.edit`: More → Settings → Edit config.toml directly: an editor, checked as you save, applied at once; `settings.file` opens it at a setting |
| `config check` | `config.edit`: saving checks it, and a bad config shows as an error | `config.check`: Check on the editor, without saving |
| `config show` | `config.show` (full sized) | `config.show`: Show effective on the editor |
| `config path` | `config.edit` (opens it) | `config.path`: the editor says where the file is |
| `config list` | `config.panel` (`C`): every setting by section, with its value, default and where it comes from (Key bindings lists every key, defaults too, grouped by when it works); `m` shows only what you changed; the palette lists each as `setting <key>` | `settings`: More → Settings: sections, with a count of what you changed; `settings.search` finds one by name, what it does or its value; Key bindings lists every key, defaults too, with its standing (default, yours, changed, unbound) |
| `config get` | `config.panel`: the row's detail says its default, where it comes from and when a change applies | `settings`: each setting says its default, its file and line, and when a change applies |
| `config set` | `config.panel`: ⏎ or space changes the setting by its kind (a switch, a list of choices, a number with `+`/`-`, a box that says why a value is wrong as you type); the daemon and every open screen pick it up; on a key, ⏎ is a menu: change its key (press it; a taken key asks before the two swap), change what it runs (searchable), unbind (`d`, a default stays listed) | `settings.set`: a switch, a list, a box checked as you type; `settings.keys` presses keys instead of typing their names; `settings.keychange` presses a new key for a binding (a taken key asks before swapping), `settings.keyaction` picks what it runs, `settings.keyunbind` switches a key off; `settings.target` saves a repository's to your file or the shared one |
| `config unset` | `config.panel`: `r` puts a setting back to its default; `u` undoes the last change; on a key it brings the default back | `settings.reset`: Reset on a setting; `settings.keyreset`: Reset on a key; `settings.undo`: Undo on the message after any change |
| `config add` | `config.panel`: ⏎ on `+ Add …` in Agents, Themes, Key bindings (one under each group: press the key, then pick what it runs; a taken key asks first), Notifications, Scratchpad and editors; Profiles run `profile.new` | `settings.add`: ＋ Add … under an agent, theme, editor, key binding, notification service, run script or prompt |
| `config rm` | `config.panel`: `d` on a member, asking first | `settings.remove`: Remove on a member's page, asking first |
| `config rename` | `config.panel`: `R` on a member | `settings.rename`: Rename on a member's page |
| `config schema` | waived: for programs that build forms of their own (the web app is one); people use the panel | waived: for programs that build forms of their own (the web app is one) |
| `completions` | waived: shell setup, for typing commands in a shell | waived: shell setup on the computer, for typing commands there |
| `shell-init` | `health` (`M`): when it's missing, a finding whose fix adds the line to your rc file | waived: a line in the computer's shell rc file, for its shells |
| `daemon start` | waived: the TUI starts the daemon when it isn't running | waived: `serve` runs with the daemon; with no daemon there is no web app to ask |
| `daemon stop` | `app.quit` (`C-d` after the prefix): asks, then stops every session and the daemon and exits | `daemon.stop`: More → Daemon → Stop (asks first; the app goes offline with it) |
| `daemon restart` | `daemon.restart` (full sized; this TUI reconnects) | `daemon.restart`: More → Daemon → Restart (asks first; the app reconnects) |
| `daemon status` | `daemon.status` (its `machine` line shows the computer's name; the sidebar title and the terminal title carry it too); `remote.web` is the web app's line; the health badge has its warnings | `daemon.status`: More → Daemon (the machine is in the page title, the header and More); More → Web app (`web.status`) is the web app's line |
| `daemon logs` | `daemon.logs` (followed, full sized) | `daemon.logs`: More → Daemon → Logs |
| `daemon upgrade` | `daemon.upgrade`; a daemon running another binary is a health finding whose fix this is | `daemon.upgrade`: More → Daemon → Upgrade, or a health finding's fix |
| `daemon install-service` | `daemon.service` | `daemon.service`: More → Daemon → Install the service |
| `daemon uninstall-service` | `daemon.unservice` | `daemon.unservice`: More → Daemon → Remove it |
| `backup` | `app.backup` (full sized; the file is private, in your home directory unless you give one) | `backup.create`: More → Daemon → Back up ptyro's state: written on the computer, with what is in it and what is left out |
| `uninstall` | `app.uninstall` (full sized: the plan, then the question; it ends this TUI and the daemon) | `uninstall.plan`: More → Daemon → Uninstall…: the plan, and the command to type on the computer (it ends the server this page comes from) |
