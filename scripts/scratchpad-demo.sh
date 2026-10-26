#!/bin/sh
# NOTE1 in the sandbox (see sandbox.sh): fills the scratchpad of a throwaway ptyro with a few
# items, shows the CLI side (todo, done, log, what `start` would tell an agent), then with
# `tui` opens the TUI on it: C-q ; shows the pad, C-q ' adds a line, C-q " lists the items,
# C-q g switches between the project's pad and the one for everywhere (NOTE1.7).
#
#   make scratchpad-demo            the CLI walkthrough
#   make scratchpad-demo ARGS=tui   then the TUI
#   make scratchpad-demo ARGS=clean remove the sandbox
set -eu

here=$(cd "$(dirname "$0")" && pwd)
# (The sandbox runs in its demo project, whose own pad the commands would use: the first part
# of the walkthrough is the pad for everywhere, so it says --global.)
pad() { "$here/sandbox.sh" scratchpad "$@"; }
gpad() { pad --global "$@"; }

if [ "${1:-}" = clean ]; then
  exec "$here/sandbox.sh" clean
fi

echo "== the file"
path=$(gpad path | tail -n 1)
echo "$path   (\$PTYRO_SCRATCHPAD in every session; the pad for everywhere)"

if ! grep -q 'login times out on safari' "$path"; then
  gpad add --section Bugs "login times out on safari
repro: sign in, wait 30 s, press a key" >/dev/null 2>&1
  gpad add --section Bugs "the retry loop never backs off" >/dev/null 2>&1
  gpad add --section Ideas "a dark theme for the diff view" >/dev/null 2>&1
  gpad add --note "ship the scratchpad before the weekend" >/dev/null 2>&1
fi

echo; echo "== ptyro scratchpad todo"
gpad todo
echo; echo "== ptyro scratchpad done retry   (words from one open item)"
gpad done retry
echo; echo "== ptyro scratchpad todo --all"
gpad todo --all
echo; echo "== what \`start safari\` would tell the agent"
gpad start --dry-run safari
echo; echo "== ptyro scratchpad log"
gpad log
echo; echo "the file itself:"; echo "----"; cat "$path"; echo "----"

# NOTE1.7: inside the sandbox's demo project the same commands use the project's own pad
# (<repo>/.ptyro/scratchpad.md, kept out of git); --global is the pad above.
demo="$here/../target/sandbox/demo"
echo; echo "== inside the demo project: its own pad (\$PTYRO_PROJECT_SCRATCHPAD)"
(
  cd "$demo"
  pad path
  pad add "the demo's setup script is slow" 2>&1
  pad todo
  echo "-- --global from the same directory:"
  pad --global todo | head -n 3
  echo "-- git status of the demo repository (the pad is not in it):"
  git status --short
)

if [ "${1:-}" = tui ]; then
  exec "$here/sandbox.sh"
fi
echo; echo "make scratchpad-demo ARGS=tui to open the TUI on it (C-q ; the pad, C-q ' add, C-q \" items, C-q g the other pad)"
