# Fixtures

Real captures from real programs keep the emulator, status engine and adapters honest (SPEC §18.2).
There are two kinds of fixture.

## 1. Terminal output (`.ptylog`)

Raw bytes a program wrote to its PTY, replayed into the emulator by golden tests.

Record one into `crates/ptyro-term/tests/fixtures/<name>.<COLS>x<ROWS>.ptylog`, using timed input
to drive the program:

```bash
make fixture NAME=claude-turn SIZE=110x32 CMD=claude \
  ARGS='--timeout 30 --send "3:Reply with only the word: pong" --send "3.5:\r"'
make snapshots          # golden test: replays every fixture, writes/updates its .snap
git diff crates/ptyro-term/tests/snapshots   # review what changed
```

The golden test also replays each fixture in tiny chunks and requires the identical screen,
so parsing never depends on where reads happened to split.

`make headless CMD=… ARGS=…` runs the same tool without recording:

- `--send SECONDS:TEXT` accepts `\r \n \t \e \xHH` escapes. Shift+Enter under kitty is
  `\e[13;2u`; unfocus is `\e[O`.
- `--snapshot SECONDS` prints the screen mid-run, and `--resize SECONDS:COLSxROWS` resizes.
- `--expect TEXT` makes the run fail when TEXT is missing from the final screen.

The format is raw bytes only; the size lives in the file name. Any session ptyro recorded can
become one: `ptyro export <session> --format ptylog -o crates/ptyro-term/tests/fixtures/<name>.<COLS>x<ROWS>.ptylog`
writes its output bytes (the recording itself, `pty.log` in the session's data directory, also
keeps timing and resizes; SPEC §7.6). **Review a capture before committing it.** Agent
screens show account names, plan names, paths and prompt text, so record in a scratch directory
with a throwaway prompt. Gemini's API-key dialog pre-fills the key in clear text (SPEC §8.5),
so never record that screen. Sanitize paths with **same-length** replacements, or the layout
will change; replace path components one by one, because agents wrap long paths across lines.

## 2. Hook payloads (JSON)

Captured by running the agent with a logging hook. For Claude, see
`crates/ptyro-agents/tests/fixtures/claude-2.1.304/hooks/`: one sanitized payload per event, captured during
ROADMAP M0.3. The sanitizing replaced home directories with `/home/user` and scratch paths with
`/tmp/ptyro-spike`. Session ids are random UUIDs and harmless. One exception keeps its recorded
path: `codex-0.143.0/trust-hashes.json`, because Codex computed those hashes over that exact
string.

Name directories `<agent>-<version>/`, so a new agent version gets new fixtures next to the old
ones. A diff between them is the changelog for our adapter.
