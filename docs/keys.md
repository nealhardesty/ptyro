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
| `n` | new session wizard: what to run, then where |
| `N` | new worktree of this project (+ an agent), after its setup script |
| `w` | switch workspace |
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
| `j` / `k` (or `Down` / `Up`) | show the next / previous session of the workspace here, in sidebar order, across projects |
| `1` … `9` | show the session numbered N in the sidebar here |
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
| `[` / `PageUp` | copy mode / copy mode, one page up |
| `]` | paste what you last copied |
| `x` / `X` | stop the session (asks first unless `[ui] confirm_kill = false`; an exited one is removed) / kill it now |
| `C-r` | run an exited session's command again |
| `,` | rename the session |
| `=` | take control: size the session to your pane (when another client resized it) |
| `C-l` | redraw everything |

`Ctrl-Z` (no prefix) goes to the program. An agent run directly in a session has no shell to
`fg` it back, so when it suspends itself (Claude does), ptyro resumes it straight away and
says so.

In a pane whose session has exited, `⏎` runs it again; in a dormant one, `⏎` resumes it.
`Ctrl-Q D`, `O` and `S` close their view again when it is already open.

## In the sidebar

Focus the sidebar with `Ctrl-Q Tab`. These keys need no prefix there.

| Key | Does |
|---|---|
| `j` / `k`, `g` / `G` | next / previous, first / last |
| `⏎` | open the session or change set, or fold and unfold a project or worktree |
| `h` / `l` (or `Left` / `Right`) | fold / unfold |
| `Space` | mark a session for broadcast; on a project or worktree, fold or unfold |
| `b` | broadcast one prompt into the marked sessions |
| `n` / `N` | new session here / new worktree |
| `a` `p` `i` `e` | next attention / peek / inbox / why this status |
| `x` / `X` | stop a session or remove a worktree (refuses to lose work) / kill |
| `R` | restart an exited session |
| `,` | rename |
| `D` `O` `S` | inspector / runs / change sets |
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
(or the run, for a gate or run entry), `p` peeks at it, `r` marks it read, `R` marks all read,
`q` or `Esc` closes.

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
| `q`, `Esc` or `C-c` | leave (`Esc` first clears a selection) |

## Mouse

- Wheel up to scroll back.
- Drag to select (copied on release). Hold `Shift` for the terminal's own selection, or set
  `[ui] mouse = false`.
- Click to focus. Clicking a project or worktree row folds it.
- Drag a divider to resize.

## The palette

`Ctrl-Q :` runs any action by name, including ones with no default key: `project.add`
(pick from repos under `[discover] roots`), `project.trust`, `worktree.remove`,
`session.command` (a session running a command line), `session.resume`, `session.hibernate`,
`session.mute`, `notify.test`, `theme.next`, `config.edit`, `help.tour`.

## Other overlays

- **Review queue** (`Ctrl-Q Q`): `⏎` opens the inspector, `d` marks it reviewed.
- **Tour** (`ptyro tour`, `help.tour`): `→` / `⏎` next, `←` / `h` / `Backspace` back, `q` / `Esc` leave.
- **Trust** (a project's scripts): `y` trusts, `n` goes on without them, `j` / `k` scroll.

## Rebinding

```toml
[keys]
"y" = "pane.zoom"                    # after the prefix
"global:M-1" = "session.focus 1"     # no prefix needed
"nav:J" = "nav.down"                 # in the sidebar
```

The action names are the ones `Ctrl-Q ?` shows (the palette leaves out `nav.*` and `session.focus N`).
