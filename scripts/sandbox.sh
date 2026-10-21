#!/bin/sh
# A throwaway ptyro (its own daemon, store, config and worktrees under target/sandbox) with a
# demo repository: try projects, worktrees, scripts, ports and profiles without touching your real
# ptyro state. Agents keep your real $HOME, so `claude` etc. stay logged in.
#
#   make sandbox            open the TUI on the sandbox
#   make sandbox-clean      stop its daemon and delete it
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
dir="$root/target/sandbox"
bin="$root/target/debug/ptyro"
export PTYRO_HOME="$dir/ptyro"
unset PTYRO_SOCKET PTYRO_SESSION_ID PTYRO_CONFIG
export PTYRO_ALLOW_NESTED=1

if [ "${1:-}" = clean ]; then
  "$bin" daemon stop --force >/dev/null 2>&1 || true
  rm -rf "$dir"
  echo "sandbox removed"
  exit 0
fi

if [ ! -d "$dir/demo" ]; then
  mkdir -p "$dir" "$PTYRO_HOME/config"
  git init -q --bare "$dir/demo.git"
  git clone -q "$dir/demo.git" "$dir/demo" 2>/dev/null
  cd "$dir/demo"
  cat > README.md <<'MD'
# demo

A scratch repository for trying ptyro's worktrees. Edit freely.
MD
  printf '.env\nnode_modules/\n' > .gitignore
  printf '.env\n' > .worktreeinclude
  printf 'GREETING=hello-from-.env\n' > .env
  cat > .ptyro.toml <<'TOML'
[scripts]
setup = """
echo "setting up $PTYRO_WORKTREE_NAME on port $PTYRO_PORT"
sleep 1
echo "copied: $(cat .env)"
"""
archive = "echo archived $PTYRO_WORKTREE_NAME"

[scripts.run.serve]
command = "python3 -m http.server $PTYRO_PORT"
default = true
TOML
  git add -A
  git -c user.name=ptyro -c user.email=sandbox@example.invalid commit -qm "demo project"
  git push -q origin HEAD
  git remote set-head origin --auto >/dev/null 2>&1 || true
  cat > "$PTYRO_HOME/config/config.toml" <<'TOML'
# The sandbox's ptyro config: `ptyro config edit` (with PTYRO_HOME set) or edit this file.
[ports]
range = "28000-28999"
TOML
  "$bin" project add "$dir/demo" >/dev/null
  echo "sandbox ready: $dir (project 'demo')"
  echo "try: C-Space n ⏎ ⏎ (an agent in a new worktree), C-Space N, C-Space r (serve), C-Space w"
fi
cd "$dir/demo"
exec "$bin" "$@"
