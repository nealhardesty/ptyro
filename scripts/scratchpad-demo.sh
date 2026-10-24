#!/bin/sh
# NOTE1 in the sandbox (see sandbox.sh): fills the scratchpad of a throwaway ptyro with a few
# items, shows the CLI side (todo, done, log, what `start` would tell an agent), then with
# `tui` opens the TUI on it: C-q ; shows the pad, C-q ' adds a line, C-q " lists the items.
#
#   make scratchpad-demo            the CLI walkthrough
#   make scratchpad-demo ARGS=tui   then the TUI
#   make scratchpad-demo ARGS=clean remove the sandbox
set -eu

here=$(cd "$(dirname "$0")" && pwd)
pad() { "$here/sandbox.sh" scratchpad "$@"; }

if [ "${1:-}" = clean ]; then
  exec "$here/sandbox.sh" clean
fi

echo "== the file"
path=$(pad path | tail -n 1)
echo "$path   (\$PTYRO_SCRATCHPAD in every session)"

if ! grep -q 'login times out on safari' "$path"; then
  pad add --section Bugs "login times out on safari
repro: sign in, wait 30 s, press a key" >/dev/null 2>&1
  pad add --section Bugs "the retry loop never backs off" >/dev/null 2>&1
  pad add --section Ideas "a dark theme for the diff view" >/dev/null 2>&1
  pad add --note "ship the scratchpad before the weekend" >/dev/null 2>&1
fi

echo; echo "== ptyro scratchpad todo"
pad todo
echo; echo "== ptyro scratchpad done retry   (words from one open item)"
pad done retry
echo; echo "== ptyro scratchpad todo --all"
pad todo --all
echo; echo "== what \`start safari\` would tell the agent"
pad start --dry-run safari
echo; echo "== ptyro scratchpad log"
pad log
echo; echo "the file itself:"; echo "----"; cat "$path"; echo "----"

if [ "${1:-}" = tui ]; then
  exec "$here/sandbox.sh"
fi
echo; echo "make scratchpad-demo ARGS=tui to open the TUI on it (C-q ; the pad, C-q ' add, C-q " items)"
