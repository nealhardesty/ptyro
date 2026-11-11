# ptyro review: inconsistency and CLI/TUI/web parity

Reviewed 2026-11-11 against the clean checkout at `the review checkout` (branch `docs/review-2026-11`). Read-only: nothing was built, run or changed. Paths are relative to that checkout unless absolute. Each finding is marked VERIFIED (both sides of the mismatch were read) or SUSPECTED.

One correction to the brief: the clap definitions are not in `crates/ptyro/src/cli.rs`. The top-level `Cmd` enum is in `crates/ptyro/src/main.rs:71`, and each group's enum is in its module (`chat.rs:64`, `cs.rs:33`, `orch.rs:28`, `projects.rs:20/122/176`, `profiles.rs:20`, `shells.rs:34`, `ship.rs:16/87`, `pad.rs:17`, `agents.rs:15/21`).

## Summary

- The ledger is complete at the level it is checked: all 257 visible leaf commands have a row, every first-named action exists, and there are no `gap:` cells. The drift is underneath that level.
- `make parity` proves that an action name exists. It does not look at flags, and most findings are a TUI or web path that hard-codes a request's optional fields.
- The TUI is the weaker surface for PR controls, runs, mute and integrate, which breaks "the CLI and the TUI are always in parity". The web is weaker for the inbox, key-binding settings and live updates.
- 63 commands reach the TUI as the CLI run in a split, and seven of them ask the user to type an id that is already on screen.
- "Creating shows every option" fails in four places: the TUI session form (lead, agent arguments), the TUI new chat (title, workspace), the web fan-out (profile), and `ptyro new` versus `ptyro wt new` in the CLI itself.
- `msg ls` (the mail ledger) and `run events` (TUI) have ledger rows that point at a different feature.

## 1. Command × surface matrix

Columns: the command; its visible flags; the TUI path the ledger names; the web action it names. Every action below was checked to exist in `crates/ptyro-tui/src/keymap.rs` `ACTIONS` (251 ids) or a `const ACTIONS` in `web/src/*.ts` (287 ids).

- `[cli-split]`: the TUI action runs the `ptyro` CLI in a split (`crates/ptyro-tui/src/cli_actions.rs:21-101`, `app.rs:3452-3468`, or a `Make::Cli` form in `form.rs`). It is not native TUI UI.
- `[hub+keys]`: the named action opens a screen, and the command itself is a hard-coded key inside it. The checker cannot see those keys.
- `waived` / `shown`: the ledger's own words.

Where a flag is not reachable from a surface, the findings and section 3 say so; the matrix does not repeat it per row.

| Command | Flags | TUI path | Web action |
|---|---|---|---|
| `new` | --detach --name --cwd --size --json --agent --profile --again --wt --new-wt --project --override-pin --yes --prompt --prompt-file --title --take-changes --pr --issue --unqueued --verify --lead --max-workers --lead-agent --structured | session.new | session.new |
| `attach` | --workspace --raw --view-only --tab --pane | session.find | session.open |
| `list` | --json --all --status --project --workspace --chats | focus.toggle | sessions |
| `kill` | --signal | session.kill | session.kill |
| `stop` | — | session.stop | session.stop |
| `restart` | — | session.restart | session.restart |
| `rm` | --stop --yes | session.remove | session.remove |
| `rename` | — | session.rename | session.rename |
| `read` | --screen --scrollback --ansi --plain --json --chat | copy.enter | session.open |
| `send` | --no-enter --raw --wait-ready --wait --timeout --stall --json | peek [hub+keys] | session.reply |
| `broadcast` | --no-enter --timeout | broadcast | session.broadcast |
| `wait` | --for --match --scrollback --timeout --any --all --stall --json | waived | waived |
| `events` | --follow --session --limit --json | inspector [hub+keys] | session.events |
| `pr create` | --draft --ready --title --body --base | inspector [hub+keys] | pr.create |
| `pr status` | --refresh --json | inspector [hub+keys] | pr.checks |
| `pr checks` | --json | inspector [hub+keys] | pr.checks |
| `pr fix` | — | inspector [hub+keys] | pr.fix |
| `pr automerge` | --method --off | inspector [hub+keys] | pr.automerge |
| `pr watch` | --off | inspector [hub+keys] | pr.watch |
| `pr reply` | --wt --comment | pr.reply [cli-split] | pr.reply |
| `pr open` | — | pr.open | pr.open |
| `watch-pr` | --off | inspector [hub+keys] | pr.watch |
| `review add` | — | inspector [hub+keys] | review.add |
| `review ls` | --json | inspector [hub+keys] | review.list |
| `review rm` | --all --yes | inspector [hub+keys] | review.rm |
| `review send` | --to --note | inspector [hub+keys] | review.send |
| `review queue` | --json | review.queue [hub+keys] | review.queue |
| `review done` | — | review.queue [hub+keys] | review.done |
| `run ls` | --active --tree --json | runs [hub+keys] | runs |
| `run show` | --json | runs [hub+keys] | run.show |
| `run events` | --json | runs [hub+keys] | run.events |
| `run wait` | --timeout --gate --json | waived | waived |
| `run cancel` | — | runs [hub+keys] | run.cancel |
| `run retry` | — | runs [hub+keys] | run.retry |
| `run fork` | --from --set --json | run.fork [cli-split] | run.fork |
| `run lead` | --max-workers --agent | session.lead [cli-split] | run.lead |
| `recipe ls` | --project --json | recipe.list [cli-split] | recipe.list |
| `recipe show` | --project | recipe.show [cli-split] | recipe.show |
| `recipe validate` | — | waived | waived |
| `recipe graph` | --format --project | waived | waived |
| `recipe run` | --var --project --base --trust --wait --json | recipe.run [cli-split] | recipe.run |
| `fanout` | --agent --count --prompt --prompt-file --project --base --prefix --test --verify --judge --timeout --structured --wait --json | runs [hub+keys] | fanout |
| `compare` | --refresh --json | runs [hub+keys] | compare |
| `pick` | --cherry-pick --open-pr --apply --into --path --hunk | runs [hub+keys] | pick |
| `discard` | --force --yes | runs [hub+keys] | discard |
| `iterate` | — | runs [hub+keys] | iterate |
| `cs new` | --projects --template --lead-as --layout --prompt --prompt-file --from-issue --depends --agent --base --gates --no-gates --integration --overlay --workspace --trust --json | sets.new [cli-split] | set.new |
| `cs ls` | --json | sets [hub+keys] | sets |
| `cs show` | --json | sets [hub+keys] | set.show |
| `cs log` | --follow --json | sets [hub+keys] | set.log |
| `cs msg` | --kind --interrupt | sets [hub+keys] | set.message |
| `cs ready` | --member | waived | waived |
| `cs blocked` | --member | waived | waived |
| `cs assign` | — | waived | waived |
| `cs ask` | --option | waived | waived |
| `cs contract show` | --rev | set.contract [cli-split] | set.contract |
| `cs contract edit` | — | set.contract [cli-split] | set.contract_edit |
| `cs contract diff` | --a --b | set.contract [cli-split] | set.contract |
| `cs contract history` | — | set.contract [cli-split] | set.contract |
| `cs contract submit` | --message | waived | waived |
| `cs contract revise` | --diff --affects --message | waived | waived |
| `cs contract approve` | --note | sets [hub+keys] | set.contract_approve |
| `cs contract reject` | — | sets [hub+keys] | set.contract_reject |
| `cs diff` | — | sets [hub+keys] | set.diff |
| `cs check` | --json | sets [hub+keys] | set.check |
| `cs checkpoint` | — | sets [hub+keys] | set.checkpoint |
| `cs checkpoints` | --json | set.checkpoints [cli-split] | set.checkpoints |
| `cs restore` | --force | set.restore [cli-split] | set.restore |
| `cs overlay` | --member | set.overlay [cli-split] | set.overlay |
| `cs commit` | --message | set.commit [cli-split] | set.commit |
| `cs push` | --yes | set.push [cli-split] | set.push |
| `cs pr` | --draft | set.pr [cli-split] | set.pr |
| `cs land` | --yes --no-wait | set.land [cli-split] | set.land |
| `cs add` | --agent --depends-on | set.add [cli-split] | set.add |
| `cs drop` | --force --ignore-dependents --yes | set.drop [cli-split] | set.drop |
| `cs rm` | --keep-member --force --yes | set.remove [cli-split] | set.remove |
| `cs templates` | --json | sets.templates [cli-split] | set.templates |
| `gate ls` | --all --json | runs [hub+keys] | gate.list |
| `gate resolve` | --note --key | runs [hub+keys] | gate.resolve |
| `msg send` | --kind --run --reply-to --interrupt --key --json | session.message [cli-split] | session.message |
| `msg check` | --me --since --timeout --json | waived | waived |
| `msg ls` | --run --limit --json | inbox [hub+keys] | inbox |
| `msg ack` | --me | waived | waived |
| `lease claim` | --exclusive --ttl --reason --as | waived | waived |
| `lease release` | --as | waived | waived |
| `lease ls` | --json | leases [cli-split] | leases |
| `radar` | --project --json | radar [cli-split] | radar |
| `checkpoint ls` | --json | checkpoint.list [cli-split] | checkpoint.list |
| `checkpoint diff` | --stat | checkpoint.diff [cli-split] | checkpoint.diff |
| `checkpoint restore` | --turn --yes | checkpoint.restore [cli-split] | checkpoint.restore |
| `verify` | --off --max-bounces | session.verify [cli-split] | session.verify |
| `mcp` | --chat | waived | waived |
| `proxy` | — | waived | waived |
| `convert` | --to --force | session.convert | session.convert |
| `retitle` | — | session.retitle | session.retitle |
| `summarize` | --json | session.summarize | session.summarize |
| `adopt` | — | session.adopt | session.adopt |
| `shells ls` | --workspace --all --json | shells.menu | shells |
| `shells new` | --cwd --slot --name --title --scratch --workspace --detach --json | shells.new | shells.new |
| `shells go` | --workspace | shells.go | shells.go |
| `shells move` | --workspace | shells.move | shells.move |
| `shells split` | --down --detach --json | pane.split_right | shells.split |
| `shells break` | — | shells.break | shells.break |
| `shells join` | --right | shells.join | shells.join |
| `shells loose` | — | shells.loose | shells.loose |
| `shells adopt` | — | shells.adopt | shells.adopt |
| `shells watch` | --done --activity --silence --match --off | shells.watch | shells.watch |
| `shell` | --json --read --scrollback | shell.drawer | session.shell |
| `chat new` | --agent --preset --workspace --title --incognito --keep --folder --compare --yes --no-wait --timeout --json | chat.new | chat.new |
| `chat ask` | --keep --agent --preset --timeout --json --no-stdin | chat.new | chat.new |
| `chat ls` | --workspace --pinned --archived --machine --limit --imported --json | chat.list [hub+keys] | chats |
| `chat show` | --all --ids --branches --format | chat.open [hub+keys] | chat.open |
| `chat retry` | --no-wait --timeout --json | chat.retry | chat.retry |
| `chat edit` | --no-wait --timeout --json | chat.edit | chat.edit |
| `chat branch` | --to --json | chat.branch | chat.branch |
| `chat fork` | --at --agent --side --keep --yes --no-wait --timeout --json | chat.fork | chat.fork |
| `chat keep` | --json | chat.keep | chat.keep |
| `chat search` | --in --workspace --agent --pinned --archived --here --meaning --words --limit --json | chat.search | chat.search |
| `chat find` | --agent --timeout --json | chat.find | chat.find |
| `chat open` | — | chat.open [hub+keys] | chat.open |
| `chat send` | --no-wait --timeout --json | chat.open [hub+keys] | chat.send |
| `chat answer` | --node | chat.open [hub+keys] | chat.answer |
| `chat stop` | — | chat.stop | chat.stop |
| `chat retitle` | --auto --json | chat.retitle | chat.retitle |
| `chat pin` | --json | chat.pin | chat.pin |
| `chat unpin` | --json | chat.pin | chat.pin |
| `chat archive` | --json | chat.archive | chat.archive |
| `chat unarchive` | --json | chat.archive | chat.archive |
| `chat set` | --agent --preset --folder --no-folders --yes --json | chat.agent | chat.agent |
| `chat presets` | --json | chat.preset | chat.preset |
| `chat task` | --project --agent --name --yes --detach --json | chat.task | chat.task |
| `chat rm` | --yes | chat.delete | chat.delete |
| `chat compare` | --agent --yes --no-wait --timeout --json | chat.compare | chat.compare |
| `chat pick` | --json | chat.pick | chat.pick |
| `chat import` | --agent --profile --since --dry-run --json | chat.import | chat.import |
| `chat export` | --format --output | chat.export | chat.export |
| `scratchpad edit` | — | scratchpad.toggle | scratchpad |
| `scratchpad path` | — | shown | shown |
| `scratchpad cat` | --tail --json | scratchpad.toggle | scratchpad |
| `scratchpad add` | --section --bullet --no-stamp --json | scratchpad.add | scratchpad.add |
| `scratchpad todo` | --all --json | scratchpad.items [hub+keys] | scratchpad.items |
| `scratchpad done` | --undo | scratchpad.items [hub+keys] | scratchpad.tick |
| `scratchpad start` | --agent --dry-run | scratchpad.items [hub+keys] | scratchpad.start |
| `scratchpad log` | --json | scratchpad.history [hub+keys] | scratchpad.history |
| `scratchpad restore` | — | scratchpad.history [hub+keys] | scratchpad.history |
| `serve` | --access --port --cert --key | remote.serve [cli-split] | waived |
| `web` | --access --pair --json | remote.web [cli-split] | web.status |
| `pair` | --name --scope --url --json --open | remote.pair [cli-split] | pair |
| `devices ls` | --json | devices.list [cli-split] | devices.list |
| `devices revoke` | --yes | devices.revoke [cli-split] | devices.revoke |
| `devices push-test` | — | devices.push_test [cli-split] | notifications |
| `machine ls` | --json | machine.list [cli-split] | machine.list |
| `machine add` | --name --force --json | machine.add [cli-split] | machine.add |
| `machine rm` | --yes | machine.remove | machine.remove |
| `machine hide` | — | machine.hide | machine.hide |
| `machine unhide` | — | machine.unhide | machine.unhide |
| `machine retry` | — | machine.retry | machine.retry |
| `machine pair` | --url --json | machine.pair [cli-split] | machine.pair |
| `replay` | --speed --max-idle --from | session.replay [cli-split] | session.replay |
| `export` | --format --output | session.export [cli-split] | session.export |
| `project add` | --workspace | project.add | project.add |
| `project ls` | --json | shown | projects |
| `project rm` | --release --yes | project.remove | project.remove |
| `project show` | --json | project.show [cli-split] | project.show |
| `project trust` | --yes | project.trust | project.trust |
| `project untrust` | — | project.untrust | project.untrust |
| `project line-endings` | — | project.line_endings | project.line_endings |
| `project config` | --explain --json | project.config [cli-split] | project.config |
| `project rename` | — | project.rename | project.rename |
| `project pin` | — | project.pin | project.pin |
| `project unpin` | — | project.unpin | project.pin |
| `project discover` | --refresh --json | project.add | project.add |
| `project suggest` | --apply --decline --json | project.suggest | project.suggest |
| `ws ls` | --json | workspace.switch | workspace.switch |
| `ws new` | --profile --notify | workspace.switch | workspace.new |
| `ws set` | --profile --notify | workspace.profile | workspace.set |
| `ws rm` | --yes | workspace.remove | workspace.remove |
| `ws rename` | — | workspace.rename | workspace.rename |
| `ws add-project` | — | project.workspaces [hub+keys] | workspace.member |
| `ws remove-project` | — | project.workspaces [hub+keys] | workspace.member |
| `wt finish` | --integrate | worktree.finish | worktree.finish |
| `wt archive` | --stop-sessions | worktree.archive | worktree.archive |
| `wt restore` | — | worktree.restore [hub+keys] | worktree.restore |
| `wt archive-rm` | --yes | worktree.archive_delete | worktree.archive_delete |
| `wt archives` | --project --json | worktree.restore [hub+keys] | worktree.restore |
| `wt retitle` | — | worktree.retitle | worktree.retitle |
| `wt ls` | --project --json | shown | projects |
| `wt new` | --name --project --base --branch --existing --pr --issue --prompt --title --take-changes --no-setup --no-fetch --agent --profile --override-pin --yes --detach --json | worktree.new | worktree.new |
| `wt rm` | --force --yes --stop-sessions --keep-branch | worktree.remove | worktree.remove |
| `wt status` | --json | shown | worktree.status |
| `wt diff` | --against --stat --json | inspector [hub+keys] | review.diff |
| `wt log` | --json | inspector [hub+keys] | worktree.log |
| `wt commit` | --message --all --ask-agent | inspector [hub+keys] | worktree.commit |
| `wt integrate` | --into --cherry-pick --push | inspector [hub+keys] | worktree.integrate |
| `wt pair` | --refresh | worktree.pair | worktree.pair |
| `wt unpair` | — | worktree.unpair | worktree.unpair |
| `wt run` | --attach | script.run | worktree.run |
| `wt open` | --editor | worktree.open | worktree.open |
| `wt lock` | — | worktree.lock | worktree.lock |
| `wt unlock` | — | worktree.unlock | worktree.unlock |
| `wt rename` | — | worktree.rename | worktree.rename |
| `wt rename-branch` | — | worktree.rename_branch | worktree.rename_branch |
| `profile ls` | --json | profile.list [cli-split] | profile.list |
| `profile show` | --json | profile.show [cli-split] | profile.show |
| `profile new` | --adopt --label --color --git-name --git-email --ssh-key --match | profile.new [cli-split] | profile.new |
| `profile rm` | --force --yes | profile.remove [cli-split] | profile.remove |
| `profile rename` | — | profile.rename [cli-split] | profile.rename |
| `profile env` | --json | waived | waived |
| `profile login` | — | profile.login [cli-split] | profile.login |
| `profile sync` | — | profile.sync [cli-split] | profile.sync |
| `profile detect` | --apply | first.run [hub+keys] | profile.detect |
| `profile doctor` | --json | health [hub+keys] | profile.doctor |
| `gc` | --dry-run --json --scratch | worktree.gc | worktree.gc |
| `config path` | — | config.edit | config.path |
| `config check` | --json | config.edit | config.check |
| `config show` | --effective --json | config.show [cli-split] | config.show |
| `config edit` | — | config.edit | config.edit |
| `config list` | --changed --project --json | config.panel [hub+keys] | settings |
| `config get` | --explain --project --json | config.panel [hub+keys] | settings |
| `config set` | --project --shared | config.panel [hub+keys] | settings.set |
| `config unset` | --project --shared | config.panel [hub+keys] | settings.reset |
| `config add` | --project --shared | config.panel [hub+keys] | settings.add |
| `config rm` | --project --shared --yes | config.panel [hub+keys] | settings.remove |
| `config rename` | --project --shared | config.panel [hub+keys] | settings.rename |
| `config schema` | --project | waived | waived |
| `completions` | — | waived | waived |
| `init` | --yes --dry-run | first.run [hub+keys] | first.run |
| `setup` | --yes --agents --tools --dry-run --json --where --move | app.setup [cli-split] | setup.plan |
| `install` | --yes --dry-run --json --where --move | app.install [cli-split] | setup.install |
| `update` | --check --yes --json | app.update [cli-split] | updates.update |
| `login` | --check --json --where | app.login [cli-split] | setup.login |
| `about` | --json | app.about | app.about |
| `doctor` | --quick --json | health [hub+keys] | health |
| `tour` | — | help.tour | help.tour |
| `uninstall` | --dry-run --yes --delete-work --keep-config --keep-data --keep-binary --backup --purge-refs --json | app.uninstall [cli-split] | uninstall.plan |
| `backup` | --output --with-recordings --with-ca --no-chats --json | app.backup [cli-split] | backup.create |
| `shell-init` | — | health [hub+keys] | waived |
| `daemon start` | --foreground --serve | waived | waived |
| `daemon stop` | --force --hibernate | app.quit | daemon.stop |
| `daemon restart` | --force --hibernate | daemon.restart [cli-split] | daemon.restart |
| `daemon install-service` | --dry-run | daemon.service [cli-split] | daemon.service |
| `daemon uninstall-service` | --dry-run | daemon.unservice [cli-split] | daemon.unservice |
| `daemon status` | --json | daemon.status [cli-split] | daemon.status |
| `daemon upgrade` | --binary --json | daemon.upgrade [cli-split] | daemon.upgrade |
| `daemon logs` | --lines --follow | daemon.logs [cli-split] | daemon.logs |
| `status` | --explain --json | session.explain | sessions |
| `resume` | — | session.resume | session.resume |
| `hibernate` | — | session.hibernate | session.hibernate |
| `answer` | — | peek [hub+keys] | answer |
| `inbox` | --workspace --unread --read --json | inbox [hub+keys] | inbox |
| `mute` | --for --off | session.mute | session.mute |
| `usage` | --json | shown | usage |
| `notify test` | — | notify.test | notify.test |
| `integrations ls` | --json | integrations [cli-split] | integrations |
| `integrations install` | --profile | integrations.install | integrations |
| `integrations uninstall` | --profile | integrations.uninstall | integrations |
| `report` | --state --kind --msg --progress --session | waived | waived |

Ledger totals: 257 leaf commands, 258 rows. 234 have a path in both, 21 are waived in both, 2 are waived on the web only (`serve`, `shell-init`). The extra row is `open-url` (hidden).

Actions the ledger names after the first one that are not actions (unchecked by `make parity`): `remote.access`, `remote.port` (`docs/parity.md:298`, config keys), `updates.keep_installer` (`:320`, a config key), `shell.integration` (`:340`, a config key), and TUI ids `app.install` / `app.setup` in a web cell (`:321`). None is wrong, but backticks mean two things in one cell.

## 2. Flag-level parity on creating commands

"Creating shows every option" (AGENTS.md; SPEC §1.3, §11.5a). Fields were read from `crates/ptyro-tui/src/form.rs` and the web forms. Flags that only make sense in a shell (`--json`, `--detach`, `--yes`, `--size`, `--prompt-file`, `--no-wait`, `--timeout`) are left out.

| Command | CLI flags | Missing in the TUI form | Missing in the web form |
|---|---|---|---|
| `new` | name, cwd, agent, profile, again, wt, new-wt, project, override-pin, prompt, title, take-changes, pr, issue, unqueued, verify, lead, max-workers, lead-agent, structured, `-- argv` | `--lead`, `--max-workers`, `--lead-agent`; agent arguments (`-- argv`); `--unqueued` (`form.rs:1772-1905`) | `--unqueued`; `--cwd` outside a project (`sessions.ts:76-80` refuses with no project) |
| `wt new` | name, project, base, branch, existing, pr, issue, prompt, title, take-changes, no-setup, no-fetch, agent, profile, override-pin | none (the session form) | none (the new-task sheet) |
| `shells new` | command, cwd, slot, name, title, scratch, workspace | none (`form.rs:1453-1476`) | none per the ledger; not re-read |
| `chat new` | agent, preset, workspace, title, incognito, keep, folder, compare | title, workspace (`chats.rs:2459`: agent, preset, folder, incognito only) | none found |
| `chat fork` | at, agent, side, keep, prompt | `--agent` (`chats.rs:1021,1351,1508` send `agent: None`) | none found |
| `chat import` | agent, profile, since, dry-run | none | none |
| `fanout` | agent, count, prompt, project, base, prefix, test, verify, judge, timeout, structured | none (it adds a profile field) | profile per agent (`runs.ts:100-115`; the TUI builds `agent@profile`, `form.rs:1476`) |
| `run lead` | max-workers, agent | none | none |
| `recipe run` | var, project, base, trust | none | none (trust is asked on refusal, `runs.ts:306`) |
| `cs new` | projects, template, lead-as, layout, prompt, from-issue, depends, agent, base, gates, no-gates, integration, overlay, workspace, trust | `--trust` is not a field; the CLI in the split asks | none (trust asked on refusal) |
| `cs add` | agent, depends-on | none | none |
| `pr create` | draft, ready, title, body, base | `--ready` | `--ready` |
| `project add` | path, workspace | none | none |
| `ws new` | profile, notify | none | none (adds a machine pick) |
| `profile new` | adopt, label, color, git-name, git-email, ssh-key, match | none | none; colour is free text where the TUI has a pick (`settings.ts:380`) |
| `pair` | name, scope, url | none | `--url` (`settings.ts:315` uses the page's origin) |
| `machine add` | name, force | none | none |
| `machine pair` | url | none | none |
| `scratchpad add` | section, bullet, no-stamp | section, no-stamp (`pad.rs:176-185`) | section, no-stamp (`notes.ts:283`) |
| `msg send` | kind, run, reply-to, interrupt, key | all of them (`cli_actions.rs:42`) | kind, run, reply-to (`sessions.ts:273`) |
| `review send` | to, note | to, note (`inspector.rs:398-405`) | to (`review.ts:66`) |

The CLI is inconsistent with itself here. `ptyro new --new-wt` has no `--base`, `--branch`, `--existing`, `--no-setup` or `--no-fetch` (`main.rs:73-157`); `ptyro wt new --agent` has no `--verify`, `--structured`, `--lead`, `--unqueued` or agent arguments (`projects.rs:228-290`). The TUI and web forms offer the union in one step, so the forms can do what no single CLI command can.

## 3. Depth parity

Y = yes, N = no, P = partial. Evidence is in the findings.

### Sessions, inbox, mail

| Capability | CLI | TUI | Web |
|---|---|---|---|
| list: all / running / active | Y | Y | Y |
| list `--status` for the other states | Y | N as a filter | P (the find box matches the status word, `app.ts:139`) |
| broadcast ready timeout | 2m, `--timeout` | 120s fixed (`app.rs:6445`) | 5s fixed (`sessions.ts:441`) |
| events: global log, `--follow`, `--limit` | Y | N: per session, 200, snapshot | N: per session, 300, manual refresh |
| mute: any duration, or until unmuted | Y | N: always 1h | P: 1h, 8h, until unmuted |
| mute `--off` | Y | N | Y |
| mute `project:` / `workspace:` | Y | Y | N |
| export formats | asciicast, text, ptylog | all three | no ptylog |
| replay speed / max-idle / from | Y | N | P (speeds and a scrubber) |
| inbox size | default | 500 | 50 |
| inbox per workspace | `-w` | Y | N |
| inbox unread only | `-u` | N | N |
| inbox: mark one read | N | Y | N |
| msg send `--interrupt` | Y | N | Y |
| msg ls (the mail ledger) | Y | N | N |

### Chats

| Capability | CLI | TUI | Web |
|---|---|---|---|
| ls pinned / archived / imported / workspace | Y | Y | Y |
| ls `--machine` | Y | N | Y |
| list filters: needing you / working / unread | N | Y | N |
| search `--pinned` | Y | N | Y |
| search `--in CHAT`, `--here`, `--limit` | Y | N (30 fixed) | N (30 fixed) |
| export JSON | Y | N (Markdown, fixed path) | Y |
| fork `--agent` | Y | N | Y |
| rm several at once | Y | N | N |

### Runs, gates, recipes

| Capability | CLI | TUI | Web |
|---|---|---|---|
| pick merge / cherry-pick / PR | Y | Y | Y |
| pick `--apply --path` | Y | N | Y (typed paths) |
| pick `--hunk` | Y | N | N |
| pick `--into` for merge / cherry-pick | Y | N | N |
| discard `--force` | Y | N | N |
| run ls `--active` / `--tree` | Y | N | N |
| run events | Y | N | Y (snapshot) |
| run fork from what is selected, with `--set` | Y | N: typed run id and step | Y |
| gate: any choice with a note | Y | N: the first option only | Y |
| gate ls `--all` | Y | N | N |
| live updates | n/a | push | poll, 4-6s |

### Change sets, review, PR, worktrees

| Capability | CLI | TUI | Web |
|---|---|---|---|
| cs msg `--interrupt` | Y | N | Y |
| cs pr `--draft` | Y | N | N |
| cs rm `--keep-member`; cs drop / restore `--force` | Y | N | N |
| review send `--note` | Y | N | Y |
| pr automerge `--method` / `--off` | Y | N | Y |
| pr watch `--off` | Y | N | Y |
| pr reply `--comment` | Y | N | Y |
| wt integrate `--into` / `--cherry-pick` / `--push` | Y | N | Y |
| wt rm `--keep-branch` | Y | N | N |

### Scratchpad, devices, daemon

| Capability | CLI | TUI | Web |
|---|---|---|---|
| scratchpad start links the item to its worktree | Y | Y | N |
| scratchpad todo `--all` | Y | Y | N as a list |
| devices revoke by picking | by id | N: types the id | Y |
| devices push-test | Y | Y (CLI split) | P: only as a side effect of turning notifications on |
| backup `-o`, `--with-recordings`, `--no-chats`, `--with-ca` | Y | N | N |
| daemon stop / restart `--hibernate` | Y | N (always `--force`) | N |
| doctor `--quick` | Y | P | N |
| usage for every account | Y | N: the focused session's profile, in the status bar | Y |

### Settings panels

| Feature | TUI | Web |
|---|---|---|
| Key-binding tables listed | 5 | 3 (no chat, no chat list) |
| Multi-line script or prompt | N | Y |
| Changed advanced field on a member page | hidden until `A` | shown |
| Undo depth | 50 | 40, lost on reload |
| Open the file at a setting | any layer | the user config only |
| Repository whose settings are edited | fixed when the panel opens | a picker |
| Theme preview while picking | Y | N |

## 4. Findings

### Parity: TUI behind the CLI (the "always in parity" rule)

**P-01 (high, VERIFIED). The TUI cannot unmute, or mute for anything but one hour.**
- Evidence: `crates/ptyro-tui/src/app.rs:3181` and `:4517` always send `minutes: Some(60), unmute: false`; `unmute: true` appears nowhere in `crates/ptyro-tui/src`. CLI: `mute --for`, `--off`, default until unmuted (`crates/ptyro/src/main.rs:1027-1035`). Web: 1h, 8h, until unmuted, unmute (`web/src/sessions.ts:283-286`).
- Reverse side: the web can only mute a session (`sessions.ts:280`); `project:` and `workspace:` targets exist in the CLI and TUI only.
- Tracked: no. `docs/parity.md:129` presents all three as done.
- Direction: one mute picker in the TUI (the web's four choices); project and workspace mute in the web's menus.

**P-02 (high, VERIFIED). TUI PR controls only switch on.**
- Evidence: `crates/ptyro-tui/src/inspector.rs:298-306` hard-codes `method: None, off: false` for automerge and `off: false` for watch. CLI: `pr automerge --method/--off`, `pr watch --off` (`ship.rs:56-72`). Web: squash/merge/rebase, Turn off, Stop watching (`web/src/review.ts:219-224`).
- Tracked: no (`docs/parity.md:213-215`).
- Direction: `m` and `w` open a small menu, as the web does.

**P-03 (high, VERIFIED). TUI `wt integrate` takes no options.**
- Evidence: `crates/ptyro-tui/src/app.rs:5920-5925` sends `into: None, cherry_pick: false, push: false`. CLI: `--into`, `--cherry-pick`, `--push` (`projects.rs:353-365`). Web form has all three (`web/src/review.ts:166`).
- Direction: a three-field form before the confirm.

**P-04 (high, VERIFIED). A TUI gate note can only go with the first option.**
- Evidence: `crates/ptyro-tui/src/runs.rs:326-338`: `n` takes `g.options.first()`. "Reject, because …" cannot be said. CLI: `gate resolve ID CHOICE --note` (`orch.rs:150-158`). Web: any choice with a note (`web/src/runs.ts:67`).
- Direction: `n`, then a digit.

**P-05 (med, VERIFIED). TUI `pick` cannot apply files, and nothing but the CLI has `--hunk` or `--into` for a merge.**
- Evidence: `crates/ptyro-tui/src/runs.rs:410-437` sends `into: None, paths: vec![], hunks: vec![]` for merge, cherry-pick and PR only. Web has apply with typed paths and a target (`web/src/runs.ts:234-239`), `hunks: []` always.
- The ledger says so by omission: the web cell mentions "some of its files", the TUI cell does not (`docs/parity.md:225`).

**P-06 (med, VERIFIED). Options the web has and the TUI lacks.**
- `review send --note`: `inspector.rs:403` (`note: None`) vs `review.ts:66`.
- `cs msg --interrupt`: `crates/ptyro-tui/src/sets.rs:162` vs `runs.ts:464`.
- `msg send --interrupt`: `cli_actions.rs:42` vs `sessions.ts:273`.
- `pr reply --comment`: `cli_actions.rs:52` vs `review.ts:200`.
- `chat fork --agent`: `chats.rs:1021,1351,1508` vs `chats.ts:490`.
- `chat export --format json`: `chats.rs:994` vs `chats.ts:612` (the ledger admits this one, `docs/parity.md:112`).
- `chat ls --machine`, `chat search --pinned`: `chats.rs:1665` vs `chats.ts:227,312`.
- `run fork --set`: `cli_actions.rs:63` vs `runs.ts:171-178`.

**P-07 (med, VERIFIED). Seven TUI actions ask for a typed id that is already on screen.**
- Evidence: `crates/ptyro-tui/src/cli_actions.rs`: `run.fork` (`:63`, run id and step), `devices.revoke` (`:91`, device id), `set.drop` (`:83`, member), `set.restore` (`:68`, number), `checkpoint.diff` (`:45`) and `checkpoint.restore` (`:49`, number), `recipe.show` (`:62`, name). The web picks each from a list or a row menu.
- Rule broken: "as a pick wherever the answers are known".
- Direction: add `{device}`, `{member}`, `{checkpoint}`, `{recipe}`, `{run}`, `{step}` template words that open a picker, as `{profile}` and `{agent}` already do.

**P-08 (med, VERIFIED). `run events` has no TUI view.**
- Evidence: `docs/parity.md:230` cites `runs`, ⏎ on a run, which is the steps view (`run show`). No TUI code requests the run's event log. Web: `run.events` (`web/src/runs.ts:206-212`).

**P-09 (med, VERIFIED). TUI `usage` is one profile in the status bar.**
- Evidence: `docs/parity.md:131` is a `shown:` cell for "the focused session's profile". CLI `usage` and the web's `usage` page list every account. The TUI fetches the full reply (`crates/ptyro-tui/src/app.rs:1133`) but has no view of it.
- This is a gap written as `shown:`.

### Parity: missing from both TUI and web

**P-10 (high, VERIFIED). `msg ls`, the mail ledger, exists only in the CLI.**
- Evidence: `Request::MsgList` is sent only from `crates/ptyro/src/orch.rs:1059` (and `mcp.rs`); no TUI file and no `web/src` file sends it. CLI help: "The ledger: every message (of a run) with how far it got" (`orch.rs:197`). `docs/parity.md:245` maps it to `inbox`, "messages to you", which is a different list.
- The same row's sender is narrower too: the UIs address one session; the CLI takes `@lead`, `@user`, `@all`, `@idle`, `@<agent>`, `@run:<id>` (`orch.rs:164`).
- Tracked: ROADMAP counts `msg ls` as done on the web (ROADMAP.md:1643).
- Direction: a Messages section on the run page in both, or correct the row to a gap.

**P-11 (med, VERIFIED). Backup options are offered nowhere but the CLI, and the ledger says otherwise.**
- Evidence: TUI template is `["backup"]` (`cli_actions.rs:94`); web calls `doHost("backup.create")` with no arguments (`web/src/settings.ts:659`). CLI: `-o`, `--with-recordings`, `--with-ca`, `--no-chats` (`main.rs:958-973`). `docs/parity.md:349` says "in your home directory unless you give one"; neither surface asks for one. The host API already accepts the options (`crates/ptyro/src/host.rs:401-407`).

**P-12 (med, VERIFIED). Flags reachable from neither UI.**
- `wt rm --keep-branch` (`app.rs:5956`; no `keep_branch` in `web/src`), `cs pr --draft` (`cli_actions.rs:81`; `runs.ts:494`), `cs rm --keep-member`, `cs drop --force/--ignore-dependents`, `cs restore --force` (`runs.ts:455,526,586`), `discard --force` (`runs.rs:458,478`; `runs.ts:260,271`), `review send --to`, `scratchpad add --section/--no-stamp`, `msg send --kind/--run/--reply-to`, `daemon stop|restart --hibernate` (`app.rs:5911`; `host.rs:412,416`), `gate ls --all`, `run ls --active/--tree`, `inbox --unread`, the global `events` log with `--follow`, `recipe ls --project`, `kill --signal`, `chat search --in/--here/--limit`.
- Not checked: whether the web's fix-offer on a refused request covers the `--force` cases.

**P-13 (low, VERIFIED). Waivers that read as "not needed" or "not built" rather than "another program runs it".**
- `recipe graph` (`docs/parity.md:240`): a drawing of a recipe is something a person looks at, and a UI is the natural place for it.
- `recipe validate` (`:239`): people run it; the UIs do show a recipe's error badge (`web/src/runs.ts:315`), so `shown:` would be truer.
- `lease release` (`:248`): `--as HOLDER` lets a person free a stuck lease (`orch.rs:232-235`); the `leases` views are read-only (`web/src/projects.ts:553`).
- The other 18 waivers are commands a program or a shell runs and look right.

### Parity: web behind

**P-14 (high, VERIFIED). The web's key-binding settings know three of five tables.**
- Evidence: core accepts `chat:` and `chatlist:` (`crates/ptyro-core/src/config.rs:1934`); the TUI lists five groups (`crates/ptyro-tui/src/settings.rs:673-682`); the web's `KEY_GROUPS` has prefix, nav and global (`web/src/panel.ts:341-345`), `Binding.table` is typed with three values (`web/src/cfgkit.ts:45`), and `slotOf` strips only `global:` and `nav:` (`cfgkit.ts:180`), so a `chat:x` binding is treated as a prefix key and its taken-key check is wrong.

**P-15 (med, VERIFIED). The web inbox is much thinner than the TUI's.**
- Evidence: 50 entries (`web/src/app.ts:177`) vs 500 (`app.rs:1106`); no workspace tabs (`app.rs:7262`); mark-all only (`app.ts:322`) vs one entry (`app.rs:7303`); every entry links to a session page (`app.ts:317`) where the TUI opens the run for a gate (`app.rs:7282`).
- SUSPECTED: a gate entry with no session is a dead link on the web.

**P-16 (med, VERIFIED). The web polls where the daemon pushes.**
- Evidence: the web handles no `run_updated`, `usage_updated` or `events_lost` (`web/src/app.ts:644-670`, `data.ts:84-100`); runs, gates, sets and the ledger poll every 3-6s (`runs.ts:141,183,265,574`). The TUI takes `RunUpdated` as a push (`app.rs:1913`).
- SUSPECTED: with no `events_lost` handling a dropped event is not repaired until reconnect (`conn.ts` not read).

**P-17 (med, VERIFIED). Web scratchpad Start does not link the item to its worktree.**
- Evidence: CLI and TUI pass `pad_item` (`crates/ptyro/src/cli.rs:1013`, `app.rs:7433`), which the daemon uses to annotate the item (`crates/ptyro-daemon/src/worktrees.rs:586`); the web only pre-fills the text (`web/src/notes.ts:447`), and `pad_item` appears nowhere in `web/src`.

**P-18 (low, VERIFIED). Smaller web shortfalls.**
- Broadcast waits 5s for readiness (`sessions.ts:441`) against 120s in the TUI and 2m in the CLI, so busy agents count as failures on the web.
- No `ptylog` export (`sessions.ts:425-426`).
- `devices push-test` exists only inside turning notifications on, which re-subscribes first (`app.ts:594-597`); `docs/parity.md:302` names the action `notifications`, not a test.
- Pairing cannot set the link's address (`settings.ts:315`).
- Fan-out has no profile (section 2).

### Creating shows every option

**P-19 (med, VERIFIED). The TUI session form cannot start a lead or pass agent arguments.**
- Evidence: fields at `crates/ptyro-tui/src/form.rs:1772-1905` are prompt, agent, command, profile, where, from, which, base, name, title, verify, structured, setup, fetch. The web sheet adds "Agent arguments", "Make it a lead", "Workers at once", "Agents it may start" (`web/src/sessions.ts:113-125`). CLI: `--lead`, `--max-workers`, `--lead-agent`, `-- argv` (`main.rs:144-157`).
- The ledger's TUI cell lists the fields accurately (`docs/parity.md:32`); the omission is in the form.

**P-20 (med, VERIFIED). A new TUI chat has no title and no workspace pick.**
- Evidence: `NEW_CHAT_ACTIONS` is agent, preset, folder, incognito (`crates/ptyro-tui/src/chats.rs:2459`); the web has title and workspace beside them (`docs/parity.md:95`). CLI: `--title`, `--workspace` (`chat.rs:21-60`).

**P-21 (med, VERIFIED). `ptyro new` and `ptyro wt new` each lack half of the other's options.** See the note under the table in section 2.
- Direction: one shared `Args` struct for "a new worktree" flattened into both.

### Config

**P-22 (med, VERIFIED). Six kinds of key can only be edited in the file.**
- `agents.*.env` (`config.rs:315`), `profiles.*.env`, `.secrets`, `.share` (`config.rs:1710-1716`) are `Kind::Opaque` (`crates/ptyro-core/src/settings.rs:837,877-882`); `profiles.*.{claude,codex,gemini,opencode,gh}.{dir,api_key,token}` (`config.rs:1648-1654`) are not in the schema at all. Both panels send the user to the file (`crates/ptyro-tui/src/settings.rs:1639-1651`, `web/src/panel.ts:292`).
- The completeness test passes because `Opaque` satisfies it and `AccountDef` is never walked (`crates/ptyro-core/src/settings/tests.rs:32-38,54-93`).
- Rule broken: "a setting you can only change by editing the file isn't finished".

**P-23 (med, VERIFIED). The schema's ranges are narrower than the parser's.**

| Key | Parser accepts | Schema accepts |
|---|---|---|
| `ui.sidebar_width` | 12..=500 (`config.rs:1868`) | 12..120 (`settings.rs:693`) |
| `discover.depth` | 0..=32 (`config.rs:1871`) | 1..10 (`settings.rs:747`) |
| `github.log_lines` | 0..=5000 (`config.rs:1874`) | 1..2000 (`settings.rs:756`) |
| `ports.count` | any u16 ≥ 1 (`config.rs:1947`) | 1..1000 (`settings.rs:745`) |
| `worktrees.stale_days` | any u32 (`config.rs:1500`) | 1..3650 (`settings.rs:740`) |
| `review.max_unreviewed` | any u32 (`config.rs:1221`) | 0..100 (`settings.rs:757`) |

A value the file accepts cannot be typed into a panel or `config set` (`settings.rs:1206-1212`).

**P-24 (med, VERIFIED with one caveat). The TUI cannot enter a multi-line script or prompt.**
- Evidence: Enter submits (`crates/ptyro-tui/src/settings.rs:1866`) and paste turns newlines into spaces (`:2167-2168`); the web uses textareas (`web/src/panel.ts:271-276,428`). The schema says a Script "may run over several lines" (`settings.rs:95-96`).
- Caveat: not every branch of `LineEdit::key` was read.

**P-25 (med, VERIFIED). Smaller panel differences.**
- A changed advanced field on a member page is hidden in the TUI (`settings.rs:571` lacks the `|| e.changed()` that `:583` has); the web shows it (`panel.ts:624,675`).
- Prompts cannot be renamed in either panel (TUI `R` acts on `Row::Member` only, `settings.rs:1406-1410`; web `mapRows` has Remove only, `panel.ts:434-437`); core supports it (`settings.rs:1738-1783`). The CLI wiring past `crates/ptyro/src/config.rs:259-260` was not traced.
- The TUI repeats the sink types (`settings.rs:1724-1731`) that the web reads from the schema (`panel.ts:467`).
- Neither panel shows that an environment variable overrides a value (`Origin` has no such case, `settings.rs:1785`).

**P-26 (low, VERIFIED). Config docs drift.**
- `docs/parity.md:335` lists different collection sets for the TUI and the web under `config add`; both drive every collection from `COLLECTIONS` (`settings.rs:408-499`), so the two are equal and both cells are short.
- `docs/config.md:11` says `.ptyro.toml` holds "ports"; `ProjectConfig` has no such section and denies unknown fields (`crates/ptyro-core/src/project.rs:317-325`).
- `ui.shell_style`: `docs/config.md:46` and `SPEC.md:4348` describe ⏎ on the quick terminal's line; `settings.rs:705` and `config.rs:44` describe the `!` key.
- `SPEC.md:4264` says `daemon.log_level` is "accepted but not read yet"; `crates/ptyro-daemon/src/lib.rs:138` applies it.
- `docs/config.md:139` omits `local` for `client.default_remote` (`config.rs:657-660`).
- `PTYRO_WSL_HOLD` overrides `daemon.hold_wsl` (`crates/ptyro-daemon/src/wsl_hold.rs:159,175`) and is in no document.
- No default in `docs/config.md` was found to differ from `Config::default()`; the schema takes its defaults from there (`settings.rs:7-8`), so those two cannot drift.

### Behaviour

**P-27 (med, VERIFIED). Confirmation differs by surface for the same action.**
- `pick` (merge, cherry-pick, PR): the TUI asks `[y/N]` (`runs.rs:422-438`); the web sends it from the menu with no question (`web/src/runs.ts:222-232`); the CLI has no `--yes` and does not ask (`main.rs:434-455`).
- The full table is under Behaviour.

**P-28 (low, VERIFIED). A message's `kind` depends on where it was sent from.**
- CLI default `note` (`orch.rs:168`), web `info` (`web/src/sessions.ts:273`), daemon default `note` (`crates/ptyro-daemon/src/mail.rs:1751`). The ledger shows the kind (`sets.rs:524`).

**P-29 (low, VERIFIED). Confirmation wording is shared in Rust and copied by hand in the web.**
- `crates/ptyro-core/src/removal.rs` holds the sentences; the TUI uses the constants (`runs.rs:246,449`); the web repeats the text as literals (`web/src/runs.ts:260,271`, `sessions.ts:355-356`). `removal.rs:79` has a `DIALOGS` table that suggests a test pins some of them; which ones was not checked.

### Keys

What holds: every key in `docs/keys.md` is bound to the documented action in `keymap.rs:552-722`; the help overlay is generated from the keymap (`crates/ptyro-tui/src/ui.rs:2299-2340`); about 30 key cells sampled from `docs/parity.md` match the defaults.

Action × screen (TUI):

| Action | Prefix | Sidebar | Chat list | Inspector | Runs | Sets | Settings | Inbox |
|---|---|---|---|---|---|---|---|---|
| remove / delete | `x` stop, `X` kill | `x`, `X` | `x` archive, `X` delete | `x` one comment, `X` all | `x` cancel; compare `d`, `X` | none | `d`; `r` reset | none |
| rename / title | `,` | `,` | `,` | none | none | none | `R` | none |
| search | `/` | `/` | `/` (in a chat `C-f`) | none | none | none | `/` | none |
| refresh | `C-l` redraw | none | none | `r` | `r` is retry | `r` | none | `r` is mark read |
| close | n/a | `Tab`/`Esc` | `Esc`/`q` | `q`/`Esc` | `q`/`Esc`, back one level | `Esc` back, `q` closes all | `q`/`Esc` | `q`/`Esc` |

TUI against web shortcuts (`web/src/app.ts:690-717`; `web/src/keys.ts` only translates terminal keys):

| Action | TUI | Web |
|---|---|---|
| new task | `n` | `n` |
| new chat | `m` | `m` |
| find a session | `/` | `/` (chat search on the Chats page) |
| search all chats | `C-q C-g` | `Ctrl-G` |
| digits `1`-`9` | session N (`keymap.rs:783-792`) | shell N of SHELLS (`app.ts:704`); the TUI uses `Alt-N` for that |
| chat list | `g` | `g c` (`g` is a go-to prefix) |
| runs / sets / inbox | `O` / `S` / `i` | `g r` / `g e` / `g i` |
| settings, inspector, stop, rename, palette | `C`, `D`, `x`, `,`, `:` | no key |

**P-30 (med, VERIFIED). "Every key can be changed" holds for five tables only.**
- Claims: `docs/keys.md:347` and `:27` ("lists every binding").
- Hard-coded, not rebindable, and absent from `?`: the inspector (`inspector.rs:229-427`), runs (`runs.rs:216-470`), sets (`sets.rs:174-283`), inbox (`app.rs:7241-7314`), review queue (`app.rs:5528-5541`), settings panel (`settings.rs:1211-1425`), about (`about.rs:46-82`), first run (`onboard.rs:147-183`), scratchpad items and the gc and restore pickers (`pad.rs:248-272`, `app.rs:5636,5655`), copy mode (`copy.rs:319+`).
- These are the same screens that carry the `[hub+keys]` rows of the matrix, so the keys the ledger cites for 69 commands are neither checked nor configurable.
- Direction: per-screen tables in the keymap; at least list them in `?`.

**P-31 (med, VERIFIED). Destructive keys change meaning by context.**
- `x`: stop a session (`keymap.rs:583,658`), remove or archive a worktree (`app.rs:3382-3390`), archive a chat (`keymap.rs:711`), drop a review comment (`inspector.rs:367`), cancel a run (`runs.rs:239,365`).
- `X`: SIGKILL a session (`keymap.rs:584`), delete a chat (`keymap.rs:712`), remove every comment (`inspector.rs:382`), discard the other candidates (`runs.rs:466`).
- `Ctrl-X`: ticks a scratchpad item (`pad.rs:248`) but permanently deletes an archive in the restore list (`app.rs:5655`).
- `d`: detach, deny a permission, page down in the inspector, mark reviewed, discard a candidate, remove in settings.
- `f` on a chat row forks the chat inside the `sidebar.filter` action (`app.rs:3525-3531`), so rebinding the filter moves fork with it.
- All confirm first except the single-comment drop and the scratchpad tick.

**P-32 (med, VERIFIED). Digits mean different things in the TUI and the web.** See the table. Web keys are documented only in the web's own `?` sheet, are not rebindable, and `docs/keys.md` has no web section.

**P-33 (low, VERIFIED). `docs/keys.md` omissions.**
- It names three key groups (`:348-349`); there are five (`settings.rs:673-678`).
- Bound but not listed: sidebar `C` (`keymap.rs:685`), chat list `n` (`:709`) and `q` (`chats.rs:2328`), inspector `X`, `c` in a run (`runs.rs:350`), settings `A`, `e`, `o`, `[`/`]` (`settings.rs:1258-1425`), about `o` and `Y` (`about.rs:62,78`), inspector `E` (commit in your editor, `inspector.rs:408`).
- The help overlay closes on `Esc` or `C-c`; `q` types into its filter (`app.rs:5567`), unlike every other screen.

### Docs drift

What holds: every command `docs/cli-json.md` documents has `--json`; the short-flag table (`docs/commands.md:38-51`) matches; every listed alias exists; about 75 README examples and the code blocks of `docs/scripting.md` and `docs/remote.md` parse against the clap definitions; 16 SPEC claims sampled match the code (ui, daemon, recorder, remote, notification, chat, ports and discover defaults; protocol 1.39 at `SPEC.md:6129` vs `crates/ptyro-protocol/src/lib.rs:43`; pairing TTL; status names; session kinds).

**P-34 (med, VERIFIED). `--timeout` is a duration in some commands and bare seconds in others.**
- Duration string for `wait`, `send`, `broadcast`, `fanout` (`main.rs:290,306,325,413`); `u64` seconds, default 600, for every `chat` subcommand (`chat.rs:55-57,79-81,142,157`). `ptyro chat ask --timeout 10m` is a parse error.
- `docs/commands.md:54-58` states that a flag means the same everywhere and gives `--timeout D` as the rule.
- Direction: accept durations in chat, a bare number still meaning seconds.

**P-35 (med, VERIFIED). `docs/cli-json.md` leaves out whole families that have `--json`.**
- No `chat` (36 `json` fields in `chat.rs`), no `machine ls/add/pair` (`main.rs:1243,1256,1289`), no `shells` or `shell`; also `backup`, `uninstall`, `update`, `login`, `install`, `setup`, `web`, `fanout`, `summarize`.
- `docs/cli-json.md:6` calls these shapes a pinned interface, and says every listing and query command takes `--json`; `chat show` uses `--format text|md|json` instead (`chat.rs:127-129`).

**P-36 (low, VERIFIED). Flags missing from their `docs/commands.md` rows.**
`list --chats` (`main.rs:200`), `read --screen/--plain`, `wait --all`, `chat ask --json/--no-stdin`, `chat new --compare` (`chat.rs:48`), `machine add --force`, `setup`/`install --move`, `backup --no-chats`, `wt new --override-pin/-y` (`projects.rs:276`). `commands.md:235` says every `recipe` subcommand takes `-p`; `recipe validate` does not (`orch.rs:108`). The alias list (`commands.md:17-18`) omits `pad`, `chat list`, `chat delete`, `ws add`, `cs status`.

**P-37 (low, VERIFIED). Two exit-code tables disagree.** `docs/cli-json.md:307-316` is the stale one against `docs/commands.md:312-322`. Keep one. `cli-json.md:58` lists six `attention.kind` values; the enum has `Gate` and `Run` as well (`crates/ptyro-core/src/status.rs:209-220`).

**P-38 (low, VERIFIED). SPEC §11.3 is stale.**
- `SPEC.md:3729-3730` marks `Alt-1…9` and `C-c`/`e` as "[proposed SH]"; they are built (`keymap.rs:614-615,741-744,794`).
- `SPEC.md:3698` says `e` has no key after the prefix; it is `shells.menu` (`keymap.rs:615`).
- `SPEC.md:3708` calls `n` the "new session wizard"; the action is "New task" (`keymap.rs:41-42`).
- Three wordings for `s`/`v` splits (`SPEC.md:3716`, `docs/keys.md:63`, `keymap.rs:266-267`); which is right was not traced (SUSPECTED).

### Vocabulary and naming

Same thing, different names:

| Concept | CLI | TUI | Web |
|---|---|---|---|
| Notes file | `scratchpad`, alias `pad` (`main.rs:608`) | `scratchpad.*`, "your notes file" | page "Notes", route `#/notes`, module `notes.ts` (`web/src/notes.ts:413`); actions `scratchpad.*` |
| Health | `doctor` | action `health`, described "Mend", key `M` (`keymap.rs:216`, `docs/keys.md:82`); a separate `doctor` action | "Health" (`web/src/settings.ts:94`) |
| Settings | `config …` | `config.panel`, labelled "Settings" | `settings.*`, plus `config.edit/check/show/path` |
| Change set | `cs` (alias `changeset`) | `sets`, `sets.new`, then `set.*` | tab "Sets", heading "Change sets" (`runs.ts:435`), `set.*` |
| Other computers | `machine` (singular) | `machine.*` | "Machines" |
| Paired phones | `devices` (plural), `pair` | `devices.*`, `remote.pair` | `devices.*`, `pair.device`, `pair`, `unpair` |
| Web server | `web on/off/status`, `serve` | `remote.web`, `remote.web_on`, `remote.serve` | `web.status`, `web.on`, `web.logs` |
| Hibernated session | `hibernate` / `resume`; state `dormant` | "dormant", `☾` | "Hibernated" (`app.ts:405`); key `dormant` |
| Errored status | `--status error`, label `error` | `error` | key `errored`, shown "error" (`kit.ts:444`) |
| Quick terminal vs numbered shells | `shell` and `shells`, two commands | `shell.*` and `shells.*` | `session.shell`, `shell.*`, `shells.*` |

CLI verbs:

| Idea | Spellings |
|---|---|
| List | top-level `list` (alias `ls`, `main.rs:180`); `config list` (no `ls`); every other group `ls`, and only `chat ls` has a `list` alias (`chat.rs:91`). Also plural nouns: `wt archives`, `cs checkpoints` (but `checkpoint ls`), `cs templates`, `chat presets` |
| Bare group | `devices`, `machine`, `shells`, `scratchpad`, `web` run bare; `project`, `ws`, `wt`, `profile`, `cs`, `run`, `checkpoint` need a subcommand; bare `chat` starts a chat |
| Remove | `rm` mostly; only `chat rm` has a `delete` alias (`chat.rs:385`). Also `kill`, `stop`, `discard`, `cs drop`, `devices revoke`, `wt archive-rm`, `ws remove-project`, `config unset`, `integrations uninstall`, `lease release`, `run cancel` |
| Undo / off | `un-` commands (`unpin`, `unlock`, `unpair`, `unhide`, `unarchive`, `untrust`); `--off` flags (`mute`, `verify`, `pr watch`, `pr automerge`, `shells watch`); positional `web on|off`; `scratchpad done --undo` |
| Create | `new` (session, wt, ws, profile, cs, shells, chat); `add` (project, machine, cs member, config, review, scratchpad); `ws add-project`; `pr create` |
| Inspect | `show` (project, profile, cs, run, recipe, chat, config); `status` (session, wt, pr, daemon, web); `config get`; `scratchpad cat`; `read`. No `wt show` or `ws show` |
| Group aliases | plural or long aliases for `run`, `recipe`, `gate`, `lease`, `checkpoint`, `project`, `profile`, `ws`, `wt`, `cs` (`main.rs:369-743`); none for `machine`, `devices`, `chat`, `integrations`, `msg` |

CLI flags. Short flags are consistent everywhere (`-p` project, `-w` workspace, `-a` agent, `-n` name, `-y` yes, `-d` detach, `-l` limit, `-f` follow, `-m` message, `-o` output, `-C` cwd); no collision was found.

| Issue | Where |
|---|---|
| `--force` means six things | stop sessions (`daemon stop/restart`); lose work (`wt rm`, `cs rm/drop`, `discard`); in use (`profile rm`); interrupt a turn (`convert`); unreachable (`machine add`); overwrite newer work (`cs restore`) |
| Stopping sessions has three flags | `rm --stop`; `wt rm`/`wt archive --stop-sessions`; `daemon stop --force` |
| `--all` means several things | `list --all` (history), `shells --all` (every workspace), `gate ls`, `scratchpad todo`, `chat show`, `review rm`, `wt commit`, `wait --all` |
| Look only | `--dry-run` (init, setup, install, gc, uninstall, chat import, scratchpad start) vs `--check` (`update`, `login`) |
| Waiting default flips | `--no-wait` on chat commands and `cs land`; `--wait` on `fanout`, `recipe run`, `send` |
| Worktree target | positional mostly; `--wt` on `new` and `pr reply` |
| Output format | `--json`; `--format` on `chat show`, `chat export`, `export`, `recipe graph` |
| `--project` | `-p/--project` everywhere; `cs new --projects` |
| JSON from a mutation | yes for `chat pin/archive/keep/retitle`, `machine add`, `new`, `wt new`, `shells new`; no for `profile new`, `ws new`, `project add`, `chat rm`, `run cancel/retry`, `pick` |

Action ids, TUI → web, for the same thing: `sets.new` → `set.new`; `sets.templates` → `set.templates`; `chat.allow/always/deny` → `chat.answer`; `chat.list_archived` → `chat.archived`; `app.backup` → `backup.create`; `app.uninstall` → `uninstall.plan`; `app.setup/install/login` → `setup.plan/install/login`; `app.update` → `updates.update`; `remote.web*` → `web.*`; `remote.pair` → `pair.device`; `config.panel` → `settings`; `broadcast` → `session.broadcast`; `workspace.profile/notify` → `workspace.set`; `project.workspaces` → `workspace.member`; `script.run` → `worktree.run`.

**P-39 (med, VERIFIED in the parser, not run). A mistyped group name can start an agent.**
- Evidence: unknown words fall through to `task: Vec<String>` (`crates/ptyro/src/main.rs:64-66`), which starts the default agent in a new worktree (`crates/ptyro/src/cli.rs:273`). One unknown word is caught as "unknown command" (`cli.rs:969`); two or more are a task. `machines`, `device`, `chats` are not aliases, while `projects`, `profiles`, `runs`, `gates` are, so `ptyro machines ls` reads as a task.
- Direction: add the missing plural and singular aliases; refuse a task whose first word is close to a command name unless it is quoted.

**P-40 (med, VERIFIED). `ls`/`list` and `rm`/`delete` aliases are uneven.** See the verb table. Direction: every `Ls` gets `list`, `config list` gets `ls`, every `Rm` gets the aliases `chat rm` has.

**P-41 (low, VERIFIED). Action ids and user-facing nouns differ between the TUI and the web.** See the tables above. Inside each table the schemes are mixed too: `*.remove` beside `chat.delete`, `review.rm`, `devices.revoke`; `machine.list` beside bare `runs`, `leases`; `devices.*` plural beside `machine.*`; `remote.pair.browser` beside `remote.web_on`. Direction: one id registry (section 5), and one noun each for Notes/scratchpad and Health/Mend/doctor.

### Behaviour

Status glyphs and colours:

| Status | CLI `list` | TUI (`ui.rs:22-58`, `theme.rs:46-51`) | Web (`app.ts:114`, style.css) |
|---|---|---|---|
| working | `working` | `●` pulsing, orange | `●`, blue |
| blocked | `blocked: …` | `◉`, red | `⚠`, red |
| idle | `idle` | `○`, blue | `○`, green |
| done | `done` | `✓`, green | `✓`, green |
| errored | `error` | `✗`, a second red | `✗`, the same red as blocked |
| starting | `starting` | `•`, dim | `◌` |
| low-confidence guess | suffix ` (guess)` (`agents.rs:50`) | `◌` in the status colour | no marker |
| exited / dormant | words | `■` / `☾` | `·`, dimmed |

Default session order:

| Surface | Order |
|---|---|
| CLI `list` | the daemon's order, no client sort (`cli.rs:951-956`) |
| TUI sidebar | workspace → project → worktree; worktrees root first, then by creation (`tree.rs:193`) |
| Web | by urgency: blocked, errored, working, starting, done, idle, shell, dormant, exited; then name (`app.ts:285-286`) |

Confirmations for destructive actions:

| Action | CLI | TUI | Web |
|---|---|---|---|
| stop / kill a session | never asks, no `--yes` (`main.rs:204-219`) | asks, under `ui.confirm_kill` (`app.rs:3900-3912`) | asks, under `confirm_kill` (`sessions.ts:343-364`) |
| `rm --stop` on a running session | no question (`cli.rs:508-530`) | asks | asks |
| rm a dormant session | asks / `--yes` | asks | asks |
| run cancel | no question, no `--yes` (`orch.rs:598`) | asks (`runs.rs:247`) | asks (`runs.ts:195`) |
| pick (merge, cherry-pick, PR) | no question | asks (`runs.rs:422`) | no question (`runs.ts:222-232`) |
| cs restore | no question; `--force` only (`cs.rs:750`) | the CLI in a split, so none | asks (`runs.ts:586`) |
| checkpoint restore | its own prompt (`orch.rs:1229-1241`) | the CLI in a split | asks (`projects.ts:586`) |
| wt archive | no question | asks (`app.rs:3249`) | asks (`projects.ts:344`) |
| daemon restart / stop | refused while sessions run unless `--force` | restart asks (`app.rs:3455`); stop is `app.quit` | both ask (`settings.ts:629,645`) |
| daemon upgrade | no question | the CLI, no question (`app.rs:3464`) | asks (`settings.ts:623`) |
| gc | interactive; archive / remove / keep (`projects.rs:1483`) | archive / keep | archive / keep / open (`projects.ts:510-513`) |
| wt rm, project rm, ws rm, chat rm, profile rm, devices revoke, machine rm, discard, cs rm/drop, review rm --all, config rm, wt archive-rm | asks / `--yes` | asks | asks |

**P-42 (high, VERIFIED by reading, not run). The TUI's scratchpad history shows the wrong age on every row.**
- Evidence: `crates/ptyro-tui/src/pad.rs:312` calls `crate::ui::ago(now.saturating_sub(s.at_ms))`. `ago` is `ptyro_core::ago` (`ui.rs:2405`), which takes a timestamp (`crates/ptyro-core/src/lib.rs:56-58`), so it formats the epoch time itself: about "20770d ago". The CLI is right: `crates/ptyro/src/pad.rs:320` passes `s.at_ms`.
- Direction: `ago(s.at_ms)`.

**P-43 (med, VERIFIED). The same status is `errored` and `error` in `--json`, and the doc names the wrong one.**
- Evidence: `AgentStatus` serialises as `errored` (`crates/ptyro-core/src/status.rs:6-21`), which the web keys on (`app.ts:114`); `docs/cli-json.md:73` documents `error`; `wait --json` emits `error` through `label()` (`crates/ptyro/src/script.rs:55`). Same pattern for `source`: `osc` on the wire (`status.rs:73`), `osc7501` in the doc (`cli-json.md:77`).

**P-44 (med, VERIFIED). The CLI ends sessions, cancels runs and restores checkpoints without the question the TUI and web ask.** See the confirmation table.
- `checkpoint restore` does not use `ask_first`: with no TTY and no `--yes` it restores silently (`orch.rs:1229`), where `ask_first` would refuse (`cli.rs:1162`). `cs restore` never asks (`cs.rs:750`).
- `removal::RUN_CANCEL` exists and the CLI does not use it.
- Direction: route these through `ask_first`, or state in SPEC that the CLI never asks for stop and kill.

**P-45 (med, VERIFIED). Colours and glyphs mean different things in the TUI and the web.** See the status table: blue is idle in the TUI and working on the web; the web gives idle and done one green and blocked and errored one red; `◌` is a guess in the TUI and "starting" on the web, and the web has no guess marker. Direction: one status table in core, tested as `removal.rs` is.

**P-46 (low, VERIFIED). JSON conventions drift.**
- `docs/cli-json.md:20` says only `events` is line-delimited; `run events --json` (`orch.rs:572`), `cs log --json`, `read --chat` and `update --json` are too.
- Bare `status --json` has a different, undocumented shape from `ls --json` (`agents.rs:134`).
- `daemon status` has `proto: {major, minor}` and `about` has `protocol`; `cost_millis` beside `cost_micro_usd`; `expires_ms` beside `resets_at_ms`; scratchpad `at` is a formatted string.
- Errors in JSON mode are plain text on stderr (`cli.rs:152-156`), which the doc does not say.

**P-47 (low, VERIFIED). Smaller behaviour differences.**
- Event times: CLI `events` prints UTC `HH:MM:SS` (`script.rs:558`); `run events` and `cs log` print relative ages (`orch.rs:582`, `cs.rs:428`); the web prints local clock time (`sessions.ts:524`, `runs.ts:213,577`).
- Daemon down: "the daemon is not running (so there are no sessions)" (`cli.rs:929`) is used by `run`, `cs` and `shells` too; chat, pad and profile commands show the client's "no daemon at <socket>" (`crates/ptyro-client/src/lib.rs:433`). `list` exits 0 with no daemon (`cli.rs:1566-1574`); `status` exits 8.
- `gc`: the web tooltip says "archive or remove" (`projects.ts:44`) and its menu has no Remove; the CLI has no `--yes`, so it cannot be scripted.

### Platform

No clap command or flag is `cfg`-gated, so help is the same on every platform and the ledger is right to list commands once.

**P-48 (med, VERIFIED). `docs/compat.md` never mentions Windows.** Its terminal table has Linux and macOS columns only (`docs/compat.md:71-79`), while the README makes `ptyro.exe` in Windows Terminal a main path (`README.md:303-383`) and settings exist only for it (`ui.progress_*`, `settings.rs:706-708`).

**P-49 (low, VERIFIED). `kill -s N` does nothing for most signals on a native Windows daemon.** `crates/ptyro-pty/src/windows.rs:965-995` maps KILL, HUP, TERM and INT; anything else returns Ok. Help (`main.rs:207`) has no caveat. Direction: return `Unsupported`.

**P-50 (low, VERIFIED on the schema side). Platform-specific settings show everywhere.** `daemon.hold_wsl` ("Does nothing outside WSL", `settings.rs:813`), `ui.progress_*`, and the `cmd` shell choice ("Windows only", `settings.rs:652`), which errors elsewhere (`crates/ptyro-core/src/shell.rs:300`). The schema has no platform field. Whether a panel filters them was not checked.

**P-51 (low, SUSPECTED). Mail to Claude's inbox socket fails on Windows** with an Unsupported error (`crates/ptyro-daemon/src/mail.rs:1880-1883`); whether another path delivers it was not traced.

## 5. The parity checker

`make parity` runs `cargo test -p ptyro --bin ptyro parity` (`Makefile:120-121`), which is `crates/ptyro/src/parity.rs`.

What it verifies:
- every visible leaf command of the clap tree has a row in `docs/parity.md`, and no row names a command that is gone (`parity.rs:174-199`);
- each cell starts with `` `action` ``, `shown:`, `waived:` or `gap: TASK` (`parity.rs:137-165`);
- the first backticked word is a TUI action (`keymap::spec`) or a key of a `const ACTIONS` in eleven web files (`parity.rs:26-38,92-107`);
- a `gap:` fails unless a strictness variable is `0`;
- every command is on a line of `docs/commands.md`, and every `make` target is in the README (`parity.rs:252-337`).

What it cannot catch:

| Class of drift | Why | Example found |
|---|---|---|
| Flags and options | It never looks below the command name | P-01 to P-06, P-11, P-12 |
| Hub actions | `inspector`, `runs`, `sets`, `config.panel`, `health` each satisfy many rows; the real path is a hard-coded key in prose | 69 `[hub+keys]` rows; P-04 |
| An action that exists but does something else | Existence is the whole test | `msg ls` → `inbox` (P-10); `run events` → `runs` (P-08) |
| Later backticked words | Only the first is checked | config keys and TUI ids in cells (section 1) |
| `shown:` and `waived:` | Any text over three characters passes | `usage` (P-09); `recipe graph` (P-13) |
| Web actions with no control | A key in `ACTIONS` passes whether or not anything uses it | `settings.keychange`, `settings.keyreset`, `settings.keyunbind`, `setup.where` are registered but never passed to `tip()` or `tipOf()`; whether their controls exist under another tooltip was not traced (SUSPECTED) |
| Reverse gaps | Rows start from CLI commands | 102 TUI-only and 138 web-only action ids have no row to live in |
| Create forms | No comparison of flags to fields | P-19 to P-21 |
| Malformed rows | A row whose first cell is not exactly `` `cmd` `` is skipped silently (`parity.rs:81`) | the `open-url (hidden)` row at `docs/parity.md:311` is never read |
| New web modules | The source list is fixed at eleven files (`parity.rs:26-38`) | `notes.ts` and `pad.ts` hold scratchpad UI and register nothing |
| Platform | One command tree, one ledger | section on platform |

Ideas to make parity structural:

1. **One operation registry.** Most UI paths already end in the same `Request` variant the CLI sends. Declare each operation once (id, request type, its optional fields with label, kind and default) in `ptyro-protocol` or `ptyro-core`, as `settings.rs` already does for config. The clap `Args`, the TUI form and the web form are generated from it or checked against it. A new optional field then fails the build until each surface shows it or carries a waiver naming the field.
2. **A field-level check as a first step.** Cheaper than a registry: for each `Request` variant, list the fields a TUI or web call site sets to a literal `None`, `false` or `[]` on every path. That list is this report's P-01 to P-12, found by grep; a test could produce it, with waivers per field.
3. **Generate the web's request types from the protocol.** The web builds requests as untyped object literals (`conn.request({ t: "pick", … })`). Generated TypeScript types would at least make a new field a compile error at every call site.
4. **Rows name a handler, not an action.** Replace the hub cells with `inspector:pr.automerge` style ids that the screen's key table must contain, which also makes those keys rebindable (P-30).
5. **Check every backticked id in a cell,** and require a marker (for example `key:`) for config keys, so backticks mean one thing.
6. **Check that a web action is used:** each `ACTIONS` key must appear in a `tip("…")` call.
7. **Give reverse gaps a home:** a second table for TUI and web actions with no command, each `inherent:` (splits, zoom, touch) or pointing at a CLI flag.
8. **Fail on a table row that does not parse,** rather than skipping it.
9. **Per-surface e2e over the registry.** With operations declared once, one fixture can drive each through the CLI, a headless TUI and the web test harness and compare the request the daemon receives.

## 6. Already tracked?

- `docs/parity.md` has no `gap:` cell, so none of the findings is recorded there as a gap. It admits two by wording: TUI chat export is Markdown only (`:112`), and the web's uninstall and admin-rights installs show a command instead of running it (`:320-322`, `:350`, waived by the owner on 2026-11-06).
- ROADMAP was keyword-searched, not read in full. It counts `msg ls` as done on the web (`ROADMAP.md:1643`) and describes TUI gates as answered with notes (`ROADMAP.md:689`) without the first-option limit. Nothing else here turned up.

## 7. Not covered

- Nothing was run: every finding is from reading. P-42 and P-39 in particular should be confirmed by running.
- The matrix's TUI and web columns verify that the named action exists; the per-flag checks cover the commands listed in sections 2 and 3, not all 257.
- Web forms for `shells new`, and the TUI and web paths for `attach --view-only/--raw`, `read` flags, `list --project/--workspace`.
- Setup, install, update and login flag by flag; `crates/ptyro-tui/src/health.rs` and `web/src/setupkit.ts` were not read.
- Machines beyond pairing and devices; shells and profiles in depth.
- TUI settings draw code (`settings.rs:2339-2800`), `settings/ops.rs`, and the host API handlers behind the web panel.
- Chat and run status glyphs (session statuses only); session order inside a TUI worktree.
- "Agent not installed" wording across the three surfaces.
- JSON shapes in `docs/cli-json.md` against their serde structs.
- Most `cfg` sites in `ptyro-core/src/os.rs`, `ptyro-pty` and `ptyro-daemon/src/session.rs`; macOS-only and Linux-only features.
- Whether the web's fix-offer on a refused request covers the `--force` cases of P-12.
- The debug binary used for help text is from a newer branch; anything cited by `path:line` was read in this checkout.
