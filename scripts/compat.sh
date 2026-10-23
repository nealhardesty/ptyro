#!/bin/sh
# Headless compatibility sweep (ROADMAP M8.6, docs/compat.md): each installed program runs in
# ptyro's PTY + emulator with scripted keys, and its screen is checked for what a working
# terminal shows. Prints a Markdown table to paste into docs/compat.md; exits 1 if a check fails.
# Programs that aren't installed are skipped. vttest comes from $VTTEST or the PATH.
# Run via `make compat`.
set -u
H=${1:-target/debug/ptyro-headless}
VTTEST=${VTTEST:-$(command -v vttest || true)}
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT
export HOME="$WORK" TERM=xterm-256color LANG=${LANG:-C.UTF-8}
printf 'line %s\n' $(seq 1 500) > "$WORK/lines.txt"
failed=0

# check NAME PROGRAM VERSION-CMD -- headless args (…--expect TEXT…) -- CMD...
check() {
  name=$1 prog=$2 vcmd=$3
  shift 3
  if ! command -v "$prog" >/dev/null 2>&1; then
    printf '| %s | — | — | not installed |\n' "$name"
    return
  fi
  ver=$(sh -c "$vcmd" 2>/dev/null | head -1 | tr -d '|' | cut -c1-40)
  if out=$("$H" --size 100x30 --timeout 6 "$@" 2>&1); then
    printf '| %s | %s | ✅ | |\n' "$name" "$ver"
  else
    failed=1
    why=$(printf '%s' "$out" | grep -m1 'expected text' | cut -c1-80)
    printf '| %s | %s | ❌ | %s |\n' "$name" "$ver" "${why:-exit/timeout}"
    printf '%s\n' "$out" | tail -40 >&2
  fi
}

echo "| Check | Version | Result | Notes |"
echo "|---|---|---|---|"
check "vim: edit, :wq" vim "vim --version" \
  --send '1.0:ihello from vim\e' --send '1.5::wq\r' --expect 'saved: hello from vim' \
  -- sh -c 'vim -u NONE -N "$HOME/v.txt"; echo "saved: $(cat "$HOME/v.txt")"; sleep 1'
check "less: G, ?search" less "less --version" \
  --send '1.0:G' --send '2.0:?line 250\r' --expect 'line 250' --expect 'line 251' \
  -- less "$WORK/lines.txt"
check "htop" htop "htop --version" --send '2.0:\e[B' --expect 'PID' --expect 'F10Quit' -- htop
check "top" top "top -v | sed -n 's/.*procps-ng/procps-ng/p'" --expect 'load average' --expect 'PID USER' -- top
check "tmux in ptyro" tmux "tmux -V" \
  --expect 'inside tmux' --expect '[pc] 0:' \
  -- tmux -L "ptyro-compat-$$" -f /dev/null new-session -s pc 'echo inside tmux; sleep 8'
check "zsh: prompt, colour" zsh "zsh --version" \
  --send '1.0:print -P "%F{red}zsh-$((6*7))%f"\r' --send '2.0:exit\r' --expect 'zsh-42' -- zsh -f -i
check "bash: line editing" bash "bash --version" \
  --send '1.0:echo bash-$((6*7))\r' --send '1.5:echo abc\x02\x02X\r' --send '2.0:exit\r' \
  --expect 'bash-42' --expect 'aXbc' -- bash --norc --noprofile -i
check "python3 REPL" python3 "python3 --version" \
  --send '1.5:print(6*7)\r' --send '2.5:exit()\r' --expect '42' -- python3 -q
check "man (pager)" man "man --version" --send '2.0: ' --expect 'Manual page ls(1)' -- man ls
check "git log (pager)" git "git --version" --send '2.0:q' --expect 'Author:' \
  -- sh -c 'cd "$HOME" && git init -q r && cd r && git -c user.name=T -c user.email=t@e commit -q --allow-empty -m first && git log'
if [ -n "$VTTEST" ]; then
  # vttest assumes 80×24 and draws slowly; one step per second.
  vt1() { name=$1; shift; check "vttest: $name" "$VTTEST" "$VTTEST -V" --size 80x24 --timeout 9 "$@" -- "$VTTEST"; }
  vt1 "cursor box" --send '1:1\r' --expect '*+        EEEEEEEEEEEEEEEEEEEE'
  vt1 "autowrap" --send '1:1\r' --send '2:\r' --send '3:\r' \
    --expect 'Z                                                                              z'
  # Each cursor screen comes twice, at 80 and at 132 columns (DECCOLM, which ptyro ignores).
  vt1 "controls in ESC sequences" --send '1:1\r' --send '2:\r' --send '3:\r' --send '4:\r' --send '5:\r' \
    --expect 'A B C D E F G H I'
  vt1 "leading zeros" --send '1:1\r' --send '2:\r' --send '3:\r' --send '4:\r' --send '5:\r' --send '6:\r' \
    --expect 'This is a correct sentence'
  vt1 "insert/delete line" --send '1:8\r' --send '2:\r' --expect 'Top line: A'
  vt1 "insert mode" --send '1:8\r' --send '2:\r' --send '3:\r' \
    --expect 'A******************************************************************************B'
  vt1 "delete character" --send '1:8\r' --send '2:\r' --send '3:\r' --send '4:\r' --expect 'AB


Test of'
  vt1 "DSR 5/6" --send '1:6\r' --send '2:3\r' --expect 'TERMINAL OK' --expect '[ 5 ; 1 R  -- OK'
  vt1 "DA1" --send '1:6\r' --send '2:4\r' --expect 'Report is: <27> [ ?'
  vt1 "DA2" --send '1:6\r' --send '2:5\r' --expect '<27> [ >'
else
  echo "| vttest | — | — | not installed (set VTTEST=/path/to/vttest) |"
fi
exit $failed
