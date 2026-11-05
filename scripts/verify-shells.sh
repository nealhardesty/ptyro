#!/usr/bin/env bash
# SH.0 (SPEC §23.6, §23.16): what this terminal sends for Alt-1, so you know whether SHELLS'
# `Alt-1…9` reach ptyro here, or whether to use `C-q e N` (or turn on "Option as Meta").
# Interactive: run it in each terminal you use, press Alt-1 (Option-1 on a Mac), read the verdict.
set -euo pipefail
if [ ! -t 0 ]; then
  echo "verify-shells: run it in a terminal (it reads one key press)" >&2
  exit 2
fi
term="${TERM_PROGRAM:-${TERM:-unknown}}"
echo "Terminal: $term${TMUX:+ (inside tmux)}"
echo "Press Alt-1 (Option-1 on a Mac) within 10 seconds…"
old=$(stty -g)
trap 'stty "$old"' EXIT
stty raw -echo min 0 time 100
bytes=$(dd bs=16 count=1 2>/dev/null | od -An -tx1 | tr -s ' ' | sed 's/^ //;s/ $//')
stty "$old"
trap - EXIT
echo "Sent: ${bytes:-nothing}"
case "$bytes" in
  "1b 31") echo "OK: Alt arrives as ESC 1. SHELLS' Alt-1…9 work here." ;;
  "1b 5b 34 39 3b 33 75"*) echo "OK: Alt arrives through the kitty keyboard protocol. Alt-1…9 work here." ;;
  "c2 a1") echo "No Alt: Option types ¡. Turn on Option as Meta (Terminal.app: Settings → Profiles → Keyboard; iTerm2: Profiles → Keys → Left Option key: Esc+), or use C-q e 1…9." ;;
  "") echo "Nothing arrived: try again, or use C-q e 1…9." ;;
  *) echo "Something else arrived; Alt-1…9 may not work here. C-q e 1…9 always does." ;;
esac
