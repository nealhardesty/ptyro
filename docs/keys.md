# Keys

Everything in the TUI starts with the **prefix**, `Ctrl-Space` by default (`[ui] prefix`). Press
the prefix, release it, then the key. Press the prefix twice to send a real `Ctrl-Space` to the
program.

`Ctrl-Space ?` lists every binding as it is configured right now, and `ptyro tour` is a
60-second walkthrough. `Ctrl-Space :` (or `Ctrl-Space C-p`) opens the palette, which runs any
action by name.

## After the prefix

| Key | Does |
|---|---|
| `?` | help: every key |
| `:` or `C-p` | command palette |
| `d` or `q` | detach (sessions keep running) |
| `n` | new session wizard: what to run, then where |
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
| `[` or `PageUp` | copy mode |
| `]` | paste what you last copied |
| `x` / `X` | stop the session (with confirmation) / kill it now |
| `C-r` | run an exited session's command again |
| `,` | rename the session |
| `=` | take control: size the session to your pane (when another client resized it) |
| `C-l` | redraw everything |

## In the sidebar

Focus the sidebar with `Ctrl-Space Tab`. These keys need no prefix there.

| Key | Does |
|---|---|
| `j` / `k`, `g` / `G` | next / previous, first / last |
| `⏎` | open the session, or fold and unfold a project or worktree |
| `h` / `l` (or `Left` / `Right`) | fold / unfold |
| `Space` | mark a session for broadcast (`b`) |
| `n` / `N` | new session here / new worktree |
| `a` `p` `i` `e` | next attention / peek / inbox / why this status |
| `x` / `X` | stop a session or remove a worktree (refuses to lose work) / kill |
| `R` | restart an exited session |
| `,` | rename |
| `D` `O` `S` | inspector / runs / change sets |
| `w` `o` `r` | workspace / editor / run script |
| `v` `s` | split |
| `1` … `9`, `/` | jump to a session / find one |
| `Tab` or `Esc` | back to the pane |

## Peek and permission prompts

In peek (`Ctrl-Space p`), `y` allows a permission prompt, `a` always allows, `n` denies. Or type
a reply and press `⏎`.

In a structured session's chat view, type into the pane and press `⏎` to send. On a permission
card, `a` allows, `A` always allows, `d` denies. `Ctrl-C` interrupts the turn.

## The inspector (`Ctrl-Space D`)

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

## Runs (`Ctrl-Space O`) and change sets (`Ctrl-Space S`)

- **Runs:** `⏎` opens a run, `c` opens the comparison (`m` merge, `p` PR, `d` discard,
  `i` iterate), `1`–`9` answer a gate (`n` with a note), `r` retries a step, `x` cancels,
  `F` starts a fan-out.
- **Change sets:** `⏎` shows a member's agent, `m` sends a message, `1`–`9` answer the gate,
  `c` runs checks, `p` takes a checkpoint, `r` refreshes, `Esc` goes back.

## Copy mode

`Ctrl-Space [` freezes the scrollback so you can read it while output continues.

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
- Drag a divider to resize.

## The palette

`Ctrl-Space :` runs any action by name, including ones with no default key: `project.add`
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

The action names are the ones the palette and `Ctrl-Space ?` show.
