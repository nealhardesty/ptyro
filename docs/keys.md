<img src="logos/ptyro-icon.png" alt="ptyro" width="72" align="right">


# Keys

Everything in the TUI starts with the **prefix**: `Ctrl-Q` or `Ctrl-Space`, whichever you like
(`[ui] prefix`). Press the prefix, release it, then the key. Press a prefix twice to send it to
the program. Hints on screen show the first one, `Ctrl-Q`.

There are two so that one always gets through (SPEC §21.2):
- **`Ctrl-Q`**: none of Claude Code, Codex, Gemini CLI or OpenCode binds it, and it sits in the
  same place on every keyboard layout. VS Code's terminal on Linux and Windows takes it for Quick
  Open, unless you add `-workbench.action.quickOpenView` to
  `terminal.integrated.commandsToSkipShell`. Zellij's default keys quit Zellij on it.
- **`Ctrl-Space`**: on macOS it switches input sources when you have more than one, unless you
  turn that shortcut off in Keyboard settings.

To use other keys, set one or a list: `prefix = "C-]"` or `prefix = ["C-]", "C-q"]`. A list you set
replaces both defaults. `C-]` needs AltGr on some non-US layouts.

Don't know the next key? Pause after the prefix: a menu of the keys that follow appears
(`[ui] which_key_delay`, 400 ms; `0` turns it off). `.` (after the prefix, or in the sidebar, or a
right-click on a row) lists everything you can do with that session, worktree or project.
`Ctrl-Q ?` lists every binding as it is configured right now, and `ptyro tour` (or
`help.tour` in the palette) has you do the whole loop for real, with a pretend agent in a ptyro
of its own. `Ctrl-Q :` (or `Ctrl-Q C-p`) opens the palette, which runs any
action by name.

## After the prefix

| Key | Does |
|---|---|
| `?` | help: every key |
| `:` or `C-p` | command palette |
| `d` or `q` | detach (sessions keep running) |
| `C-d` | quit ptyro: stop every session and the daemon, after a yes (agents hibernate and resume next time) |
| `n` | new task: say what to do, ⏎ starts the default agent on it in a new worktree named after it (Tab edits the name, `C-o` picks the agent and where, an empty ⏎ is a random name) |
| `C-n` | quick new: another of what this workspace last started (`claude`, `claude@work`, a shell…; the default agent at first) in this worktree, no questions. The which-key menu says what it starts: `+ claude` |
| `N` | new worktree of this project (+ an agent), after its setup script |
| `w` | go to a workspace: every workspace (yours, then other machines') is in the sidebar, and this jumps to its header, unfolded; `+ new workspace` asks which machine when there are others |
| `o` | open the worktree in an editor (`[editors]`; nvim opens inside ptyro) |
| `r` / `R` | run the project's default run script / pick a lifecycle script |
| `a` | jump to the session that needs you most (blocked first) |
| `p` | peek at the session that needs you most (else this one) and answer it without switching |
| `i` | the inbox: everything that wanted you, per workspace |
| `b` | broadcast one prompt into every session marked with `Space` in the sidebar |
| `D` | the inspector: diff, comments, commit, PR, checks, ports, events |
| `Q` | the review queue |
| `O` | runs: fan-outs and recipes, their steps and gates, the comparison |
| `S` | change sets: members as swimlanes, gates, issues, the ledger |
| `t` | structured session: chat view ⇄ terminal view |
| `j` / `k` (or `Down` / `Up`) | show the next / previous session of the workspace here, in sidebar order, across projects (quick terminals are skipped) |
| `1` … `9` | show the session numbered N in the sidebar here |
| `/` | find a session (fuzzy) |
| `` ` `` | back: the split layout a full-sized panel replaced (whole, as it was), else the previously shown session |
| `v` / `s` | split: a clone of this pane's session beside / below it (the same agent and profile in the same worktree, a fresh conversation; a shell gets a shell in its directory). Only a split puts anything in a split |
| `h` / `l` | previous / next pane |
| `Shift`+arrows | the pane in that direction |
| `z` | zoom this pane (toggle) |
| `c` | close this pane (the session keeps running) |
| `<` `>` `+` `-` | narrower / wider / taller / shorter |
| `Tab` | move focus between the sidebar and the pane |
| `\` | show or hide the sidebar |
| `u` | do what the last message offers: undo it, or fix what it reports |
| `!` | a new shell of its own in this worktree, full sized (every press starts another; the quick terminal is `C-t` / `T`) |
| `C-t` | this worktree's quick terminal (one per worktree) in a drawer below a pane of its project (VS Code's toggle). Again focuses it, and once more hides it (the shell keeps running). When none of the project is open it is a panel of its own, and a note says so. With no worktree here it asks for a project first |
| `T` | this worktree's quick terminal full sized. Again goes back |
| `;` | the scratchpad: your notes file in the editor, shown as `[scratchpad] mode` says (below). Again focuses it, and once more hides it (the editor keeps running) |
| `'` | add a line to the scratchpad without opening it (stamped with the time and where you are) |
| `"` | the scratchpad's open items: type to search, `⏎` starts one as a task, `Ctrl-X` ticks it, `Ctrl-A` shows ticked ones, `Ctrl-E` opens the editor |
| `H` | home: who needs you, PRs, recent projects and repositories found; ⏎ starts a task there |
| `M` | mend: what the health badge (`⚕` in the status bar) found, each with its fix |
| `I` | about ptyro: its version and protocol, the repository and where to report a problem (links: `⏎` opens, `y` copies), where config, data, logs and the socket live, whether the daemon and the web app run (`U` switches a daemon of another version to this one), the tools found, and `c` copies all of it for a bug report |
| `C` | settings: every option, searchable, edited in place (`/` search · `m` only what you changed · `u` undo · `r` reset · `n` add · `d` remove · `R` rename · `s` save a repository's settings to the shared file · `!` restart the daemon when a setting needs it) |
| `.` | what you can do with the focused session (its menu) |
| `P` | add a project (pick from the repositories ptyro found) |
| `Z` | everything at once: the sidebar fills the screen with every row open (`⏎` opens one, `Z` or `Esc` goes back) |
| `f` | the sidebar's filter: all → running → active → all (also a click on a chip under its title; `sidebar.show_all`, `sidebar.show_running`, `sidebar.show_active` in the palette) |
| `[` / `PageUp` | copy mode / copy mode, one page up |
| `]` | paste what you last copied |
| `x` / `X` | stop the session (asks first unless `[ui] confirm_kill = false`; an exited one is removed; a shell that exits is removed by itself, `[ui] shell_exit = "keep"` leaves it) / kill it now. The pane moves on to the nearest other session (`[ui] on_stop`); `` ` `` brings the stopped one back |
| `C-r` | run an exited session's command again |
| `,` | rename the session (its address); `C-t` in the dialog edits its title instead |
| `=` | take control: size the session to your pane (when another client resized it) |
| `C-l` | redraw everything |

`Ctrl-Z` (no prefix) goes to the program. An agent run directly in a session has no shell to
`fg` it back, so when it suspends itself (Claude does), ptyro resumes it straight away and
says so.

In a pane whose session has exited, `⏎` runs it again; in a dormant one, `⏎` resumes it.
`Ctrl-Q D`, `O` and `S` close their view again when it is already open.

**Full-sized panels, splits only on request.** Only `v` / `s` put something in a split, and what
they put there is a clone of the session they split. Everything else opens full sized: a new
session (`n`, `C-n`, `!`, the wizard), the quick terminal's panel, a tool (an editor, a command's
output, `config edit`), and a session you pick that isn't on screen. A split layout it replaces is
kept whole: `` Ctrl-Q ` `` brings it back as it was, and so does picking one of its sessions in the
sidebar. An editor you opened from ptyro gives its screen back on its own when you quit it. A
session you pick that is already on screen is focused where it is.

## In the sidebar

Focus the sidebar with `Ctrl-Q Tab`. These keys need no prefix there.

| Key | Does |
|---|---|
| `j` / `k`, `g` / `G` | next / previous, first / last |
| `⏎` | open the session or change set, or fold and unfold a project or worktree |
| `h` / `l` (or `Left` / `Right`) | fold / unfold |
| `Space` | mark a session for broadcast; on a project or worktree, fold or unfold |
| `b` | broadcast one prompt into the marked sessions |
| `n` / `C-n` / `N` | new task here / quick new (another of what was last started, here) / new worktree |
| `a` `p` `i` `e` | next attention / peek / inbox / why this status |
| `x` / `X` | stop a session (`x` again removes it) or remove a worktree (refuses to lose work, and offers archiving instead; a merged one is archived) / kill. Its pane shows the next session |
| `R` | restart an exited session |
| `,` | rename |
| `D` `O` `S` | inspector / runs / change sets |
| `z` | everything at once: the sidebar full-screen, every row open |
| `f` | filter the list: all → running → active (a machine, project or worktree with nothing running under it is hidden) |
| `<` / `>` | the sidebar a column narrower / wider (or drag its edge) |
| `u` / `P` | do what the last message offers / add a project |
| `.` | everything you can do with this row (a session, worktree or project), with the keys |
| `;` / `'` | the scratchpad / add a line to it |
| `F` / `!` | finish this worktree (commit, then a PR or integrate) / a new shell in it, full sized |
| `C-t` / `T` | its quick terminal in a drawer / full sized (`⏎` on its `›_ quick shell` line shows or hides it too) |
| `H` / `M` / `I` | home / mend what the health badge found / about ptyro |
| `A` | adopt the Claude started by hand in this shell: a managed session, same conversation |
| `w` `o` `r` | workspace / editor / run script |
| `v` `s` | split |
| `1` … `9`, `/` | jump to a session / find one |
| `?` `:` `\` | help / palette / hide the sidebar |
| `Tab` or `Esc` | back to the pane |

## Peek and permission prompts

In peek (`Ctrl-Q p`), with nothing typed and the session blocked, `y` allows a permission
prompt, `a` always allows, `n` denies. Or type a reply and press `⏎`. `⏎` with nothing typed
switches to the session; `Esc` closes peek.

In a structured session's chat view, type into the pane and press `⏎` to send; `Shift-⏎` or
`Alt-⏎` adds a line, `Esc` clears the draft, `PageUp` / `PageDown` scroll. On a permission card,
with nothing typed: `a`, `y`, `1` or `⏎` allows, `A` or `2` always allows, `d`, `n`, `3` or `Esc`
denies. `Ctrl-C` clears the draft, or interrupts the turn when the draft is empty.

## The inbox (`Ctrl-Q i`)

`j` / `k` move, `Tab` cycles this workspace, the others and All. `⏎` opens the entry's session
(or the run, for a gate or run entry), `p` peeks at it, `n` notes it to the scratchpad (a line you
can edit, with a link back to the session), `r` marks it read, `R` marks all read, `q` or `Esc`
closes.

## The inspector (`Ctrl-Q D`)

Tabs `1`–`5` (or `Tab` / `Shift-Tab`): Diff, Commits, Checks, Ports, Events.

| Key | Does |
|---|---|
| `j` / `k`, `d` / `u` (`PageDown` / `PageUp`) | move a line / 20 lines |
| `n` / `p` (`]` / `[`, `J` / `K`, `Right` / `Left`) | next / previous file |
| `a` | what to diff against: base, unpushed, last turn, uncommitted |
| `s` | side by side |
| `c` or `⏎` / `x` | comment on a line / drop a comment |
| `S` | send every comment to the agent as one prompt (once its turn is over) |
| `Space` / `C` | mark files / commit them (or everything) as the worktree's profile |
| `E` / `A` | commit in your editor / ask the agent to commit |
| `P` | push and open a PR (a draft while `[github] draft = true`, the default) |
| `I` | integrate: rebase onto main and fast-forward it |
| `f` / `m` / `w` | Checks tab: send failing logs to the agent / auto-merge / watch |
| `r` / `q` | refresh / close |

## Runs (`Ctrl-Q O`) and change sets (`Ctrl-Q S`)

- **Runs:** `⏎` opens a run, `c` opens the comparison, `x` cancels, `F` starts a fan-out.
  - In a run: `1`–`9` answer a gate (`n` with a note), `⏎` shows the step's session, `D`
    inspects its worktree, `r` retries a step, `x` cancels.
  - In the comparison: `m` merges, `C` cherry-picks, `p` opens a PR (each asks first), `d`
    discards one, `X` discards the rest, `i` / `I` sends a follow-up to one / all, `R` re-runs
    the evidence, `D` inspects, `⏎` shows the candidate's session.
- **Change sets:** `⏎` shows a member's agent, `m` sends a message, `1`–`9` answer the gate,
  `c` runs checks, `p` takes a checkpoint, `r` refreshes, `Esc` goes back.

## Several lines in one prompt

Enter sends a prompt. To start a new line instead:

| Where | New line | Notes |
|---|---|---|
| An agent's pane (Claude Code, Codex) | `Shift+Enter`, `Alt+Enter`, `Ctrl+J`; in Claude also `\` then Enter | All four verified through ptyro (Claude Code 2.1.309, Codex 0.164, 2026-10-23). |
| ptyro's own boxes: the task box, peek's reply, a broadcast, a review comment, a commit message | `Shift+Enter`, `Alt+Enter`, `Ctrl+J` | The box grows as you type (up to a third of the screen), and a paste keeps its new lines. Names, titles and command lines stay on one line. The hint line names the key your terminal can send. |
| `ptyro send` | a real new line in the text: `ptyro send s $'line 1\nline 2'`, or a quoted string over two lines | Multi-line text is pasted, bracketed when the program asked for it, so nothing is sent early. Enter is pressed once afterwards (not with `--no-enter`). |
| The web app | `Shift+Enter` | Enter (or Send) sends. |

`Shift+Enter` needs a terminal that tells it apart from Enter, which the TUI asks for with the
kitty keyboard protocol. That works in Ghostty, kitty, WezTerm, foot, iTerm2 3.5+ and Alacritty
0.13+; `Ctrl-Q ?` shows whether it's on. Elsewhere (Terminal.app, older terminals) `Shift+Enter`
arrives as a plain Enter and sends. Use `Ctrl+J`, which works everywhere, or `Alt+Enter` (on
macOS, turn on "Use Option as Meta key" in Terminal.app, or "Esc+" for the left Option key in
iTerm2). Inside tmux, `set -g extended-keys on` lets `Shift+Enter` through. VS Code's terminal
needs Claude's `/terminal-setup` (it binds `Shift+Enter` to send `\` then Enter, which works
through ptyro too).

## Copy mode

`Ctrl-Q [` freezes the scrollback so you can read it while output continues.

| Key | Does |
|---|---|
| `h j k l w b e 0 ^ $`, arrows, `Home` / `End` | move (a count works: `5j`) |
| `H` / `M` / `L` | top / middle / bottom of the screen |
| `gg` / `G` | top / bottom (`gg` at the top loads older history from the recording) |
| `C-u` / `C-d`, `C-b` / `C-f` (`PageUp` / `PageDown`), `C-y` / `C-e` | half page / page / line |
| `/` / `?`, then `n` / `N` | search forward / backward, next / previous match |
| `[` / `]` (or `{` / `}`) | previous / next shell prompt (needs `ptyro shell-init`) |
| `v` / `V` / `C-v` | select characters / lines / a block (`o` jumps to the other end) |
| `y` or `⏎` / `Y` | copy the selection / the line to your terminal's clipboard (works over SSH and in tmux) |
| `a` | add the selection to the scratchpad, quoted, as a note |
| `q`, `Esc` or `C-c` | leave (`Esc` first clears a selection) |

## The scratchpad (`Ctrl-Q ;`)

One notes file for ideas and bugs, the same in every session (`$PTYRO_SCRATCHPAD`; `ptyro
scratchpad` on the command line). Its key opens it in your editor (`[scratchpad] editor`, else
`$VISUAL` / `$EDITOR`, else nvim, vim, nano or vi) as one session that keeps running, so your
cursor and undo are where you left them. `[scratchpad] mode` says where it shows:

- `drawer` (the default): below the focused pane, like the quick terminal (they share the one
  drawer; `+` / `-` resize it);
- `popup`: a window over everything, about 80% by 70% of the screen. The editor has the keyboard;
  only the prefix reaches ptyro, so `Ctrl-Q ;` closes it;
- `pane`: a pane beside the focused one. The key closes it.

Closing hides it; quitting the editor (`:q`) ends it, and the next press starts a new one. The
status bar shows `✎ 7` when seven `- [ ]` items are open. `Ctrl-Q '` adds a line without opening
the pad, and the row menu's "note this" (`.`, then `scratchpad.note`) starts that line from a
session's task. The palette also has `scratchpad.history`: saved copies, `⏎` puts one back.

## Mouse

- Wheel up to scroll back.
- Drag to select (copied on release). Hold `Shift` for the terminal's own selection, or set
  `[ui] mouse = false`.
- Click to focus. Clicking a project or worktree row folds it. Right-click a row for its menu.
- Drag a divider to resize panes, or the sidebar's edge to resize the sidebar. Either is saved
  with the workspace's layout.

## The palette

`Ctrl-Q :` runs any action by name, goes to any session, worktree or project, and runs the
current project's scripts; each line shows its key, so it teaches them. Actions with no default
key include: `project.add`
(pick from repos under `[discover] roots`), `project.trust`, `worktree.remove`,
`session.command` (a session running a command line), `session.resume`, `session.hibernate`,
`session.mute`, `session.summarize` (a `≈` line from a small model), `notify.test`,
`theme.next`, `config.edit`, `help.tour`, `scratchpad.history`, `scratchpad.note`.

## Other overlays

- **Review queue** (`Ctrl-Q Q`): `⏎` opens the inspector, `d` marks it reviewed.
- **Tour** (`ptyro tour`, `help.tour`): a coach in the corner says what to do next, with your keys;
  it never takes keys itself. Leave with `Ctrl-Q d`; everything the tour made is deleted.
  `help.tour` again starts it over.
- **Finish** (`F` on a worktree): `p` a pull request, `i` integrate. On a worktree whose work is
  in main already (`in main` or `merged` on its row), `F` and `x` clean it up (archive).
- **Trust** (a project's scripts): `y` trusts, `n` goes on without them, `j` / `k` scroll.

## Rebinding

**Every key can be changed from Settings** (`Ctrl-Q C`, then Key bindings; on the web, More →
Settings → Key bindings). It lists every key, the defaults too, in three groups (after the
prefix, in the sidebar, anywhere), each with what it does in plain words and where it stands:
`default`, `yours` (a key of your own), `changed` (a default that runs something else) or
`unbound`. Press `⏎` on one for the choices: **change its key** (press the new one; if it is
taken you are told what runs there and asked before the two swap), **change what it runs** (a
list you can search by name or by what the action does), **unbind** it (it stays listed, so
you can bring it back) or **back to the default**. `d` unbinds, `r` resets, `u` undoes, and
`/` finds a key by what it does (`detach`, `settings`). `+ Add a key…` makes a new one. The
nine digit keys that show sessions 1 to 9 are one row; bind a key of your own to
`session.focus` and a number to add more.

From a script: `ptyro config list keys` shows every key (`--changed`: only the ones that
differ from the defaults), `ptyro config get keys.d` what one runs,
`ptyro config set keys.y pane.zoom` binds, `ptyro config set keys.d none` unbinds a default and
`ptyro config unset keys.d` puts it back. In the file, only what differs from the defaults is
kept:

```toml
[keys]
"y" = "pane.zoom"                    # after the prefix
"global:M-1" = "session.focus 1"     # no prefix needed
"nav:J" = "nav.down"                 # in the sidebar
```

`"none"` unbinds a key, so a default can be switched off. The action names are the ones `Ctrl-Q ?` shows (the palette leaves out `nav.*` and
`session.focus N`). Every CLI command has one ([`parity.md`](parity.md) lists them); the
ones that run a `ptyro` command do so in a split below, asking for what they need first.
