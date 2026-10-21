# Compatibility matrix

How well programs run inside a ptyro pane (SPEC §18.2). Update it whenever you test something, with
the version and the date. Headless rows were produced with `make headless` (originally the M0 spike) and need no human;
the outer-terminal grid at the bottom needs a person at a real terminal.

Legend: ✅ works · ⚠️ works with caveats · ❌ broken · — not tested yet

## Programs (inner side), Linux, 2026-10-20 (M0 spike, re-checked with the M1 crates)

| Program | Version | Renders | Keys | Resize | Queries answered | Notes |
|---|---|---|---|---|---|---|
| Claude Code | 2.1.304 | ✅ | ✅ | ✅ | ✅ all | Trust dialog, prompt, tool output tables, permission dialog. Shift+Enter via kitty `CSI 13;2u` inserts a newline; Esc Esc clears; OSC 7501 idle/working/blocked/done/clear all observed. |
| Codex CLI | 0.143.0 | ✅ | — | — | ✅ all | Startup trust dialog only. Pushes kitty flags 7 (key releases wanted). |
| Gemini CLI | 0.64.0 | ✅ | — | — | ✅ all | Startup trust dialog; title `◇  Ready (<folder>)`. |
| vim | 9.1 | ✅ | ✅ | ✅ | ✅ | Edit + `:wq` verified by file content. A lone Esc waits for `ttimeout` with `-u NONE` (vim behaviour, not ours). |
| htop | 3.4.1 | ✅ | ✅ | — | ✅ | Starts and quits with `q`. |
| bash / sh | 5.3.9 | ✅ | ✅ | — | — | `make smoke`: colour, wide CJK and emoji, line input. |
| ptyro's own TUI | 0.0.1 | ✅ | ✅ | ✅ | ✅ | Dogfood: `ptyro new`/`attach` inside the headless emulator: attach, type, detach, reattach at another size. |
| nvim, fzf, lazygit, tig, fish | — | — | — | — | — | Not tried yet. |
| OpenCode | — | — | — | — | — | Not tried yet. |

## Headless sweep, Linux, 2026-10-21 (M8.6, `make compat`)

`make compat` runs each program in ptyro's PTY + emulator with scripted keys and checks the
screen. It needs no human, so run it after any emulator change; it prints this table.

| Check | Version | Result | Notes |
|---|---|---|---|
| vim: edit, `:wq` | 9.1 | ✅ | The file has the typed text. |
| less: `G`, `?search` | 668 | ✅ | |
| htop | 3.4.1 | ✅ | Header and function-key bar. |
| top | procps-ng | ✅ | |
| tmux inside ptyro | 3.6 | ✅ | Its own status bar; a private server (`-L`). |
| zsh: prompt, colour | 5.9 | ✅ | `zsh -f -i`, `print -P` colours. |
| bash: line editing | 5.3.9 | ✅ | readline Ctrl-B moves and inserts mid-line. |
| python3 REPL | 3.14.4 | ✅ | The new pyrepl (colour, its own key handling). |
| man (pager) | man-db 2.13.1 | ✅ | |
| git log (pager) | 2.53.0 | ✅ | |
| vttest: cursor box | 2.7 (20251205) | ✅ | Unbroken `*`/`+` border with the centred `E` frame. |
| vttest: autowrap | 2.7 | ✅ | Margins `I…Z` / `i…z` in order. |
| vttest: controls inside ESC sequences | 2.7 | ✅ | Four identical lines. |
| vttest: leading zeros | 2.7 | ✅ | "This is a correct sentence". |
| vttest: insert/delete line (accordion) | 2.7 | ✅ | |
| vttest: insert mode | 2.7 | ✅ | `A***…***B`. |
| vttest: delete character | 2.7 | ✅ | `AB`. |
| vttest: DSR 5 and 6 | 2.7 | ✅ | `TERMINAL OK`; position reports are right. See the DECOM caveat below. |
| vttest: DA1, DA2 | 2.7 | ✅ | `ESC[?62;22c` (VT200 family, colour); `ESC[>0;2600;1c`. |

**Known gaps:** all of these are alacritty_terminal's behaviour, also seen in Alacritty itself
(SPEC §20.5).
- **DECCOLM** (80/132 columns) is ignored, so vttest's 132-column variant of each cursor screen
  is garbled at 80 columns. Most modern terminals ignore it too.
- **DSR 6 under DECOM** (origin mode) reports absolute rows, where xterm reports rows relative to
  the scroll region. Real programs don't rely on it.
- **DA3** (unit id) and **DECREQTPARM** aren't answered: VT420 and VT100-only queries.

**Fixed by the sweep:** vttest's autowrap screen showed that a tab leaves a literal `\t` in the
cell it jumped from. Painting that cell moved the outer terminal's cursor to *its* tab stop. It
now paints as a space, and copy mode still copies the tab.

## Outer terminals (the pane UI shown in a real terminal)

Needs a human: run `make try CMD=claude` and check that colours, Shift+Enter, paste, mouse
wheel (Claude's fullscreen UI scrolls itself), copy (OSC 52), resize and detach/reattach all work,
and note how fast it feels.

| Terminal | Linux | macOS | Notes |
|---|---|---|---|
| Ghostty | — | — | |
| kitty | — | — | |
| WezTerm | — | — | |
| Alacritty | — | — | |
| iTerm2 | n/a | — | |
| Terminal.app | n/a | — | no kitty keyboard protocol: Shift+Enter falls back |
| inside tmux | — | — | needs `extended-keys on`, `allow-passthrough on` |
