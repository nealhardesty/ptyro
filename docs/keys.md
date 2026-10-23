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

`Ctrl-Q ?` lists every binding as it is configured right now, and `ptyro tour` is a
60-second walkthrough. `Ctrl-Q :` (or `Ctrl-Q C-p`) opens the palette, which runs any
action by name.

## After the prefix

| Key | Does |
|---|---|
| `?` | help: every key |
| `:` or `C-p` | command palette |
| `d` or `q` | detach (sessions keep running) |
| `n` | new task: say what to do, ⏎ starts the default agent on it in a new worktree named after it (Tab edits the name, `C-o` picks the agent and where, an empty ⏎ is a random name) |
| `N` | new worktree of this project (+ an agent), after its setup script |
| `w` | switch workspace |
| `o` | open the worktree in an editor (`[editors]`; nvim opens inside ptyro) |
| `r` / `R` | run the project's default run script / pick a lifecycle script |
| `a` | jump to the session that needs you most (blocked first) |
| `p` | peek at a session and answer it without switching |
| `i` | the inbox: everything that wanted you, per workspace |
| `b` | broadcast one prompt into every session marked with `Space` in the sidebar |
| `D` | the inspector: diff, comments, commit, PR, checks, ports, events |
| `Q` | the review queue |
| `O` | runs: fan-outs and recipes, their steps and gates, the comparison |
| `S` | change sets: members as swimlanes, gates, issues, the ledger |
| `t` | structured session: chat view ⇄ terminal view |
| `j` / `k` (or `Down` / `Up`) | next / previous session in this pane |
| `1` … `9` | the Nth session in this pane |
| `/` | find a session (fuzzy) |
| `` ` `` | the previously shown session |
| `v` / `s` | split: a session beside / below this pane |
| `h` / `l` | previous / next pane |
| `Shift`+arrows | the pane in that direction |
| `z` | zoom this pane (toggle) |
| `c` | close this pane (the session keeps running) |
| `<` `>` `+` `-` | narrower / wider / taller / shorter |
| `Tab` | move focus between the sidebar and the pane |
| `\` | show or hide the sidebar |
| `u` | do what the last message offers: undo it, or fix what it reports |
| `P` | add a project (pick from the repositories ptyro found) |
| `Z` | everything at once: the sidebar fills the screen with every row open (`⏎` opens one, `Z` or `Esc` goes back) |
| `[` or `PageUp` | copy mode |
| `]` | paste what you last copied |
| `x` / `X` | stop the session (with confirmation) / kill it now. The pane moves on to the nearest other session (`[ui] on_stop`); `` ` `` brings the stopped one back |
| `C-r` | run an exited session's command again |
| `,` | rename the session (its address); `C-t` in the dialog edits its title instead |
| `=` | take control: size the session to your pane (when another client resized it) |
| `C-l` | redraw everything |

`Ctrl-Z` (no prefix) goes to the program. An agent run directly in a session has no shell to
`fg` it back, so when it suspends itself (Claude does), ptyro resumes it straight away and
says so.

## In the sidebar

Focus the sidebar with `Ctrl-Q Tab`. These keys need no prefix there.

| Key | Does |
|---|---|
| `j` / `k`, `g` / `G` | next / previous, first / last |
| `⏎` | open the session, or fold and unfold a project or worktree |
| `h` / `l` (or `Left` / `Right`) | fold / unfold |
| `Space` | mark a session for broadcast (`b`) |
| `n` / `N` | new task here / new worktree |
| `a` `p` `i` `e` | next attention / peek / inbox / why this status |
| `x` / `X` | stop a session (`x` again removes it) or remove a worktree (refuses to lose work) / kill. Its pane shows the next session |
| `R` | restart an exited session |
| `,` | rename |
| `D` `O` `S` | inspector / runs / change sets |
| `z` | everything at once: the sidebar full-screen, every row open |
| `<` / `>` | the sidebar a column narrower / wider (or drag its edge) |
| `u` / `P` | do what the last message offers / add a project |
| `A` | adopt the Claude started by hand in this shell: a managed session, same conversation |
| `w` `o` `r` | workspace / editor / run script |
| `v` `s` | split |
| `1` … `9`, `/` | jump to a session / find one |
| `Tab` or `Esc` | back to the pane |

## Peek and permission prompts

In peek (`Ctrl-Q p`), `y` allows a permission prompt, `a` always allows, `n` denies. Or type
a reply and press `⏎`.

In a structured session's chat view, type into the pane and press `⏎` to send. On a permission
card, `a` allows, `A` always allows, `d` denies. `Ctrl-C` interrupts the turn.

## The inspector (`Ctrl-Q D`)

Tabs `1`–`5` (or `Tab`): Diff, Commits, Checks, Ports, Events.

| Key | Does |
|---|---|
| `j` / `k`, `n` / `p` | move / next and previous file |
| `a` | what to diff against: base, unpushed, last turn, uncommitted |
| `s` | side by side |
| `c` / `x` | comment on a line / drop a comment |
| `S` | send every comment to the agent as one prompt (once its turn is over) |
| `Space` / `C` | mark files / commit them (or everything) as the worktree's profile |
| `E` / `A` | commit in your editor / ask the agent to commit |
| `P` | push and open a draft PR |
| `I` | integrate: rebase onto main and fast-forward it |
| `f` / `m` / `w` | Checks tab: send failing logs to the agent / auto-merge / watch |
| `r` / `q` | refresh / close |

## Runs (`Ctrl-Q O`) and change sets (`Ctrl-Q S`)

- **Runs:** `⏎` opens a run, `c` opens the comparison (`m` merge, `p` PR, `d` discard,
  `i` iterate), `1`–`9` answer a gate (`n` with a note), `r` retries a step, `x` cancels,
  `F` starts a fan-out.
- **Change sets:** `⏎` shows a member's agent, `m` sends a message, `1`–`9` answer the gate,
  `c` runs checks, `p` takes a checkpoint, `r` refreshes, `Esc` goes back.

## Copy mode

`Ctrl-Q [` freezes the scrollback so you can read it while output continues.

| Key | Does |
|---|---|
| `h j k l w b e 0 ^ $` | move |
| `gg` / `G` | top / bottom (`gg` at the top loads older history from the recording) |
| `C-u` / `C-d`, `C-b` / `C-f` | half page / page |
| `/` / `?`, then `n` / `N` | search forward / backward, next / previous match |
| `[` / `]` | previous / next shell prompt (needs `ptyro shell-init`) |
| `v` / `V` / `C-v` | select characters / lines / a block |
| `y` or `⏎` | copy to your terminal's clipboard (works over SSH and in tmux) |
| `q` or `Esc` | leave |

## Mouse

- Wheel up to scroll back.
- Drag to select (copied on release). Hold `Shift` for the terminal's own selection, or set
  `[ui] mouse = false`.
- Click to focus. Clicking a project or worktree row folds it.
- Drag a divider to resize panes, or the sidebar's edge to resize the sidebar. Either is saved
  with the workspace's layout.

## The palette

`Ctrl-Q :` runs any action by name, including ones with no default key: `project.add`
(pick from repos under `[discover] roots`), `project.trust`, `worktree.remove`,
`session.command` (a session running a command line), `session.resume`, `session.hibernate`,
`session.mute`, `notify.test`, `theme.next`, `config.edit`.

## Rebinding

```toml
[keys]
"y" = "pane.zoom"                    # after the prefix
"global:M-1" = "session.focus 1"     # no prefix needed
"nav:J" = "nav.down"                 # in the sidebar
```

The action names are the ones the palette and `Ctrl-Q ?` show.
