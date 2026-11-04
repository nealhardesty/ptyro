# ConPTY spike report (WIN0.2, WIN0.3)

```
Windows build 26200.9711 (25H2; build 22000 and later is Windows 11)
conhost: 10.0.26100.1 (WinBuild.160101.0800)
conpty.dll (NuGet Microsoft.Windows.Console.ConPTY 1.25.260930003): 1.25.2609.30003
arch: AMD64
```

Variants: `inbox`, `inbox-passthrough`, `nuget`, `nuget-passthrough`.

Cases:

- `seqs`: each sequence ptyro reads, between text markers
- `query-da1`…: a query: who answers it
- `keys`: keys and reports typed into the pseudoconsole
- `records`: keys read as console records (PSReadLine, cmd)
- `latency`: key to echo: how long a typed letter takes to come back
- `flood`: 2 MiB of coloured lines: how fast they come through
- `ctrl-c`: Ctrl+C with processed input: a signal
- `ctrl-c-raw`: Ctrl+C with processed input off: a byte
- `close`: the pseudoconsole closed under a running child
- `resize`: 60 lines, then narrower, then wider
- `alt-resize`: the alternate screen, then a resize
- `fixture-claude-main`…: a recorded program's output

## What conhost writes first

The start of the `ctrl-c-raw` case's output (the child writes only `ready`).

| Variant | First bytes |
|---|---|
| inbox | `\e[?9001h\e[?1004h\e[?25l\e[2J\e[m\e[H` |
| inbox-passthrough | `\e[?9001h\e[?1004h\e[?25l\e[2J\e[m\e[H` |
| nuget | `\e[1t\e[c\e[?1004h\e[?9001h` |
| nuget-passthrough | `\e[1t\e[c\e[?1004h\e[?9001h` |

## Sequences a program writes (case `seqs`)

*kept*: the same sequence, between its markers. *moved*: the same, elsewhere. *changed*: something else came. *dropped*: nothing.

| Sequence | ptyro uses it for | inbox | inbox-passthrough | nuget | nuget-passthrough |
|---|---|---|---|---|---|
| `osc7501` | status reports (§9.2) | moved ahead of its text | moved ahead of its text | kept | kept |
| `osc7501-bel` | status, BEL-terminated | moved ahead of its text | moved ahead of its text | kept | kept |
| `osc133a` | prompt start | kept, plus `OSC 0;C:\Users\neal\AppData\Local\Temp\ptyro-spike\spike-conpty.exe CSI ?25h` | kept, plus `OSC 0;C:\Users\neal\AppData\Local\Temp\ptyro-spike\spike-conpty.exe CSI ?25h` | kept | kept |
| `osc133b` | prompt end | kept | kept | kept | kept |
| `osc133c` | command start, with its line | kept | kept | kept | kept |
| `osc133d` | command end, exit 0 | kept, plus `OSC 9;build done OSC 777;notify;Title;Body OSC 9;4;1;42 OSC 9;9;C:\src\app OSC 7;file://host/home/me/app OSC 52;c;aGVsbG8= OSC 21337;state=busy OSC 99;;kitty says hi CSI ?2026h CSI ?2026l CSI ?2004h CSI ?1000h CSI ?1006h CSI ?1004h CSI >1u CSI 5 q` | kept, plus `OSC 9;build done OSC 777;notify;Title;Body OSC 9;4;1;42 OSC 9;9;C:\src\app OSC 7;file://host/home/me/app OSC 52;c;aGVsbG8= OSC 21337;state=busy OSC 99;;kitty says hi CSI ?2026h CSI ?2026l CSI ?2004h CSI ?1000h CSI ?1006h CSI ?1004h CSI >1u CSI 5 q` | kept | kept |
| `osc9` | notification (iTerm2) | moved ahead of its text | moved ahead of its text | kept | kept |
| `osc777` | notification (urxvt) | moved ahead of its text | moved ahead of its text | kept | kept |
| `osc9;4` | progress (ConEmu) | moved ahead of its text | moved ahead of its text | kept | kept |
| `osc9;9` | working directory (ConEmu, WT) | moved ahead of its text | moved ahead of its text | kept | kept |
| `osc7` | working directory | moved ahead of its text | moved ahead of its text | kept | kept |
| `osc52` | copy to the clipboard | moved ahead of its text | moved ahead of its text | kept | kept |
| `osc8` | hyperlink | changed: `OSC 8;id=47216-1;https://example.com OSC 8;;` | changed: `OSC 8;id=7460-1;https://example.com OSC 8;;` | kept | kept |
| `osc2` | window title | became OSC 0, later | became OSC 0, later | kept | kept |
| `osc21337` | iTerm2 status (foreign) | moved ahead of its text | moved ahead of its text | kept | kept |
| `osc99` | notification (kitty) | moved ahead of its text | moved ahead of its text | kept | kept |
| `dcs-tmux` | tmux passthrough wrapper | kept | kept | kept | kept |
| `dcs-sixel` | sixel image | dropped | dropped | kept | kept |
| `apc` | kitty graphics (APC) | kept | kept | kept | kept |
| `sync` | synchronized output (2026) | moved ahead of its text | moved ahead of its text | kept | kept |
| `paste-mode` | bracketed paste on | moved ahead of its text | moved ahead of its text | kept | kept |
| `mouse-mode` | mouse reporting, SGR | moved ahead of its text | moved ahead of its text | kept | kept |
| `focus-mode` | focus reporting | moved ahead of its text | moved ahead of its text | kept | kept |
| `kitty-push` | kitty keyboard, push flags | moved ahead of its text | moved ahead of its text | kept | kept |
| `cursor-shape` | cursor style (DECSCUSR) | moved ahead of its text | moved ahead of its text | kept | kept |
| `truecolor` | 24-bit colour | changed: `CSI 38;2;255;100;0m CSI m` | changed: `CSI 38;2;255;100;0m CSI m` | kept | kept |
| `underline-curly` | curly, coloured underline | changed: `CSI 58:2::255:0:0m CSI 4:3m CSI m` | changed: `CSI 58:2::255:0:0m CSI 4:3m CSI m` | kept | kept |
| `wide` | wide, emoji, combining | kept | kept | kept | kept |

## Queries (cases `query-*`)

*passed on*: the query came out of the pseudoconsole, for the terminal (ptyro) to answer. *answered*: conhost answered the child itself (what the child read).

| Sequence | ptyro uses it for | inbox | inbox-passthrough | nuget | nuget-passthrough |
|---|---|---|---|---|---|
| `da1` | device attributes | answered `\e[?61;6;7;21;22;23;24;28;32;42c` | answered `\e[?61;6;7;21;22;23;24;28;32;42c` | passed on | passed on |
| `da2` | secondary attributes | answered `\e[>0;10;1c` | answered `\e[>0;10;1c` | passed on | passed on |
| `xtversion` | terminal name and version | passed on | passed on | passed on | passed on |
| `dsr` | status report | answered `\e[0n` | answered `\e[0n` | passed on | passed on |
| `cpr` | cursor position | answered `\e[1;6R` | answered `\e[1;6R` | passed on | passed on |
| `decrqm2026` | is synchronized output supported | answered `\e[?2026;0$y` | answered `\e[?2026;0$y` | passed on | passed on |
| `kitty-query` | kitty keyboard flags | passed on | passed on | passed on | passed on |
| `osc10` | default foreground | dropped | dropped | passed on | passed on |
| `osc11` | default background | dropped | dropped | passed on | passed on |
| `osc7501q` | the status probe (§9.2) | passed on | passed on | passed on | passed on |
| `xtgettcap` | terminfo name | dropped | dropped | passed on | passed on |
| `decrqss` | current SGR | answered `\eP1$r0m\e\` | answered `\eP1$r0m\e\` | passed on | passed on |
| `csi14t` | text area in pixels | passed on | passed on | passed on | passed on |
| `csi16t` | cell size (Claude Code asks) | passed on | passed on | passed on | passed on |
| `csi18t` | text area in cells | answered `\e[8;24;80t` | answered `\e[8;24;80t` | passed on | passed on |

## Keys typed into the pseudoconsole (case `keys`)

What the child read for each, in VT input mode, after turning on bracketed paste, SGR mouse, focus and kitty keys. The host types one every 200 ms.

| Typed | | inbox | inbox-passthrough | nuget | nuget-passthrough |
|---|---|---|---|---|---|
| `ab` | plain text | same | same | same | same |
| `\r` | Enter | same | same | same | same |
| `\e[A` | cursor key | same | same | same | same |
| `\eOP` | F1 (SS3) | same | same | same | same |
| `\e[15~` | F5 | same | same | same | same |
| `\ex` | Alt+x | same | same | same | same |
| `\x01` | Ctrl+A | same | same | same | same |
| `\x7f` | Backspace | same | same | same | same |
| `é漢` | non-ASCII text | same | same | same | same |
| `\e[200~pasted\rline\e[201~` | bracketed paste | same | same | same | same |
| `\e[<0;10;5M` | SGR mouse press | same | same | same | same |
| `\e[<0;10;5m` | SGR mouse release | same | same | same | same |
| `\e[I` | focus in | same | same | same | same |
| `\e[13;2u` | Shift+Enter (kitty CSI u) | same | same | same | same |
| `\e[105;5u` | Ctrl+I (kitty CSI u) | same | same | same | same |
| `\e[13;28;13;1;16;1_\e[13;28;13;0;16;1_` | Shift+Enter, win32-input-mode | `\r` | `\r` | `\r` | `\r` |
| `\e[65;30;97;1;0;1_\e[65;30;97;0;0;1_` | a, win32-input-mode | `a` | `a` | `a` | `a` |

## Echo latency and throughput (cases `latency`, `flood`)

*Latency*: from the host writing a letter to that letter coming back out (the child echoes at once), over 20 letters. *Throughput*: 2 MiB of coloured lines from the child, from its start to `FLOOD-END` arriving, and how many bytes conhost wrote for them.

| Variant | Echo median | Echo worst | Flood time | Flood rate | Bytes out for 2 MiB in |
|---|---|---|---|---|---|
| inbox | 0 ms | 1 ms | 500 ms | 4.0 MiB/s | 2150441 |
| inbox-passthrough | 0 ms | 1 ms | 352 ms | 5.7 MiB/s | 2150455 |
| nuget | 0 ms | 1 ms | 102 ms | 19.6 MiB/s | 2097263 |
| nuget-passthrough | 0 ms | 1 ms | 78 ms | 25.6 MiB/s | 2097263 |

## Keys read as console records (case `records`)

A child with VT input off reads `INPUT_RECORD`s, as PowerShell's PSReadLine and cmd do. What each typed sequence became (key-down records: virtual key, character, modifier state; `0x10` is Shift, `0x8` left Ctrl, `0x2` left Alt).

| Typed | | inbox | inbox-passthrough | nuget | nuget-passthrough |
|---|---|---|---|---|---|
| `\r` | Enter, plain VT | `vk13 ch13 0x0` | `vk13 ch13 0x0` | `vk13 ch13 0x0` | `vk13 ch13 0x0` |
| `\e[13;2u` | Shift+Enter, kitty CSI u | `nothing` | `nothing` | `vk0 ch27 0x0, vk0 ch91 0x0, vk0 ch49 0x0, vk0 ch51 0x0, vk0 ch59 0x0, vk0 ch50 0x0, vk0 ch117 0x0` | `vk0 ch27 0x0, vk0 ch91 0x0, vk0 ch49 0x0, vk0 ch51 0x0, vk0 ch59 0x0, vk0 ch50 0x0, vk0 ch117 0x0` |
| `\e[13;28;13;1;16;1_\e[13;28;13;0;16;1_` | Shift+Enter, win32-input-mode | `vk13 ch13 0x10` | `vk13 ch13 0x10` | `vk13 ch13 0x10` | `vk13 ch13 0x10` |
| `\e[A` | Up, plain VT | `vk38 ch0 0x100` | `vk38 ch0 0x100` | `vk38 ch0 0x100` | `vk38 ch0 0x100` |
| `\e[1;5A` | Ctrl+Up, plain VT | `vk17 ch0 0x8, vk38 ch0 0x108` | `vk17 ch0 0x8, vk38 ch0 0x108` | `vk17 ch0 0x8, vk38 ch0 0x108` | `vk17 ch0 0x8, vk38 ch0 0x108` |
| `\ex` | Alt+x, plain VT | `vk18 ch0 0x2, vk88 ch120 0x2` | `vk18 ch0 0x2, vk88 ch120 0x2` | `vk18 ch0 0x2, vk88 ch120 0x2` | `vk18 ch0 0x2, vk88 ch120 0x2` |
| `\x00` | Ctrl+Space, plain VT (NUL) | `vk16 ch0 0x10, vk17 ch0 0x18, vk50 ch0 0x18` | `vk16 ch0 0x10, vk17 ch0 0x18, vk50 ch0 0x18` | `vk16 ch0 0x10, vk17 ch0 0x18, vk50 ch0 0x18` | `vk16 ch0 0x10, vk17 ch0 0x18, vk50 ch0 0x18` |
| `\e[32;57;0;1;8;1_\e[32;57;0;0;8;1_` | Ctrl+Space, win32-input-mode | `vk32 ch0 0x8` | `vk32 ch0 0x8` | `vk32 ch0 0x8` | `vk32 ch0 0x8` |

## Ctrl+C and closing

| Case | inbox | inbox-passthrough | nuget | nuget-passthrough |
|---|---|---|---|---|
| `ctrl-c` | ctrl 0; exit 7; close 0 ms | ctrl 0; exit 7; close 0 ms | ctrl 0; exit 7; close 0 ms | ctrl 0; exit 7; close 0 ms |
| `ctrl-c-raw` | in 03; exit 7; close 0 ms | in 03; exit 7; close 0 ms | in 03; exit 7; close 0 ms | in 03; exit 7; close 0 ms |
| `close` | ctrl 2; exit 3221225786; close 0 ms | ctrl 2; exit 3221225786; close 0 ms | ctrl 2; exit 3221225786; close 0 ms | ctrl 2; exit 3221225786; close 0 ms |

(`ctrl 0` is CTRL_C_EVENT, `ctrl 2` CTRL_CLOSE_EVENT; `in 03` is the byte.)

## Resizing (cases `resize`, `alt-resize`)

**resize**

| Variant | Bytes after each resize | Repaints | History lines (ptyro's emulator, at the end) |
|---|---|---|---|
| inbox | 2261, 0, 1140 | yes, —, yes | 48 |
| inbox-passthrough | 2261, 1124, 16 | yes, yes, partly | 42 |
| nuget | 0, 0, 16 | —, —, partly | 37 |
| nuget-passthrough | 0, 0, 16 | —, —, partly | 37 |

Without ConPTY (the program's own bytes, the same resizes): 37 history lines.

**alt-resize**

| Variant | Bytes after each resize | Repaints | History lines (ptyro's emulator, at the end) |
|---|---|---|---|
| inbox | 16 | partly | 0 |
| inbox-passthrough | 177 | yes | 0 |
| nuget | 16 | partly | 0 |
| nuget-passthrough | 16 | partly | 0 |

Without ConPTY (the program's own bytes, the same resizes): 0 history lines.

## Recorded programs (cases `fixture-*`)

Each recording replayed through the pseudoconsole, then both into ptyro's emulator: rows whose text differs, and whether the colours and attributes match. Focus reporting is left out of the modes: conhost turns it on for itself when it starts (`?1004h`) and off when it closes.

| Recording | Size | inbox | inbox-passthrough | nuget | nuget-passthrough |
|---|---|---|---|---|---|
| `claude-main` | 100×30 | 0 rows differ, same look (styles differ only on blanks) | 0 rows differ, same look (styles differ only on blanks) | 0 rows differ, same styles | 0 rows differ, same styles |
| `claude-permission` | 100×30 | 0 rows differ, same look (styles differ only on blanks) | 0 rows differ, same look (styles differ only on blanks) | 0 rows differ, same styles | 0 rows differ, same styles |
| `codex-update` | 100×28 | 0 rows differ, same styles | 0 rows differ, same styles | 0 rows differ, same styles | 0 rows differ, same styles |
| `ls-color` | 80×8 | 0 rows differ, same styles | 0 rows differ, same styles | 0 rows differ, same styles | 0 rows differ, same styles |
| `vim-edit` | 60×12 | 0 rows differ, same styles | 0 rows differ, same styles | 0 rows differ, same styles | 0 rows differ, same styles |

## WIN0.3: a second process takes the pseudoconsole over

The old process opens a pseudoconsole with an echoing child, then exits without closing it. *none*: it hands nothing on. *inherit*: it starts the new process with the six handles (our pipe ends, the HPCON's signal, reference and conhost handles, the child). *dup*: it copies them into a new process that is already running. The new one builds an HPCON from the three and drives it.

| ConPTY | Mode | Before | After | |
|---|---|---|---|---|
| inbox | none | conhost `conhost.exe` | child alive 2 s later: false; conhost alive: false | |
| inbox | inherit | old gone: true; child alive: true | heartbeats 6 (none lost), typed→echoed true, resize hr 0 → true, close 0 ms → child exited true (code 3221225786), output ended true | **works** |
| inbox | dup | old gone: true; child alive: true | heartbeats 6 (none lost), typed→echoed true, resize hr 0 → true, close 0 ms → child exited true (code 3221225786), output ended true | **works** |
| nuget | none | conhost `OpenConsole.exe` | child alive 2 s later: false; conhost alive: false | |
| nuget | inherit | old gone: true; child alive: true | heartbeats 6 (none lost), typed→echoed true, resize hr 0 → true, close 0 ms → child exited true (code 3221225786), output ended true | **works** |
| nuget | dup | old gone: true; child alive: true | heartbeats 6 (old saw ..4, new from 6), typed→echoed true, resize hr 0 → true, close 0 ms → child exited true (code 3221225786), output ended true | **works** |
