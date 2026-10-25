#!/bin/sh
# Start ptyro over, as if it had never run: `ptyro uninstall --keep-binary` (it stops the daemon and
# every session, removes its worktrees through git, the login service, opt-in agent integrations,
# the lines it added to your shell's rc files, and its config, data, state and runtime
# directories, SPEC §14.1). It shows what it will remove and asks first. Your repositories, their
# branches, their .ptyro.toml and the snapshot refs under refs/ptyro/ stay. The ptyro program
# itself stays too (`ptyro uninstall` without --keep-binary removes it).
#
#   make reset          summary, then [y/N] (and `delete` to confirm when a worktree has uncommitted work)
#   make reset YES=1    no [y/N] question; uncommitted work is still asked about
#
# With PTYRO_HOME set it resets that home only (a sandbox, a test).
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
bin="${PTYRO_BIN:-$root/target/debug/ptyro}"
unset PTYRO_SOCKET PTYRO_CONFIG

if [ -n "${PTYRO_SESSION_ID:-}" ] && [ -z "${PTYRO_HOME:-}" ]; then
  echo "This shell runs inside a ptyro session: stopping the daemon would stop this reset too."
  echo "Detach (Ctrl-Q d) and run it from a plain terminal."
  exit 1
fi

if [ "${YES:-}" = 1 ]; then
  exec "$bin" uninstall --keep-binary --yes
fi
exec "$bin" uninstall --keep-binary
