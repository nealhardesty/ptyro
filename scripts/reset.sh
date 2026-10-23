#!/bin/sh
# Start ptyro over, as if it had never run: stop the daemon and every session, remove its
# worktrees (through git, so your repositories stay tidy), the login service, opt-in agent
# integrations, and its config, data, state and runtime directories (SPEC §14.1). Shows what it
# will remove and asks first. Your repositories, their branches and their .ptyro.toml stay.
#
#   make reset          summary, then [y/N]
#   make reset YES=1    no question, unless a worktree has uncommitted work (then type `delete`)
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
unset PTYRO_SESSION_ID

# Where ptyro keeps things (ptyro_core::paths).
if [ -n "${PTYRO_HOME:-}" ]; then
  dirs="$PTYRO_HOME"
else
  if [ -n "${XDG_RUNTIME_DIR:-}" ]; then run="$XDG_RUNTIME_DIR/ptyro"; else run="${TMPDIR:-/tmp}"; run="${run%/}/ptyro-$(id -u)"; fi
  dirs="${XDG_CONFIG_HOME:-$HOME/.config}/ptyro
${XDG_DATA_HOME:-$HOME/.local/share}/ptyro
${XDG_STATE_HOME:-$HOME/.local/state}/ptyro
$run"
fi

tilde() { case "$1" in "$HOME"/*) echo "~${1#"$HOME"}" ;; *) echo "$1" ;; esac; }

# What the daemon knows: sessions, worktrees (with their repositories), integrations. Asking
# starts the daemon if it isn't running; it is stopped below either way.
live=$("$bin" ls --json 2>/dev/null | python3 -I -c 'import json,sys
try: print(sum(1 for s in json.load(sys.stdin) if s["state"]["t"] in ("starting","running")))
except Exception: print(0)')
projects=$("$bin" project ls --json 2>/dev/null || echo '[]')
worktrees=$("$bin" wt ls --json 2>/dev/null || echo '[]')
# path<TAB>repository<TAB>branch<TAB>uncommitted changes<TAB>locked, for every worktree but @root.
# Uncommitted changes come from git now, not the daemon's last look.
wts=$(printf '%s\n%s\n' "$projects" "$worktrees" | python3 -I -c 'import json,subprocess,sys
text = sys.stdin.read()
dec = json.JSONDecoder()
projects, i = dec.raw_decode(text)
worktrees, _ = dec.raw_decode(text[i:].lstrip())
roots = {p["id"]: p["root"] for p in projects}
for w in worktrees:
    if w.get("is_root"):
        continue
    st = subprocess.run(["git", "-C", w["path"], "status", "--porcelain"], capture_output=True, text=True)
    dirty = len(st.stdout.splitlines()) if st.returncode == 0 else 0
    print("\t".join([w["path"], roots.get(w["project"], ""), w.get("branch") or "", str(dirty), "locked" if w.get("locked") else ""]))' 2>/dev/null || true)
integrations=$("$bin" integrations ls --json 2>/dev/null | python3 -I -c 'import json,sys
try: print(" ".join(e["agent"] for e in json.load(sys.stdin) if e.get("active") and e.get("needs_install")))
except Exception: pass' || true)
service=""
for f in "$HOME/Library/LaunchAgents/dev.ptyro.daemon.plist" "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/ptyro.service"; do
  [ -f "$f" ] && service="$f"
done
[ -n "${PTYRO_HOME:-}" ] && service=""

echo "ptyro reset removes everything ptyro keeps:"
if [ "$live" -gt 0 ]; then
  echo "  sessions      $live running: all of them stop (agents too: their conversations stay in the agent's own history)"
fi
echo "$dirs" | while IFS= read -r d; do
  [ -e "$d" ] || continue
  size=$(du -sh "$d" 2>/dev/null | cut -f1)
  echo "  directory     $(tilde "$d") ($size)"
done
echo "$dirs" | while IFS= read -r d; do
  if [ -d "$d/profiles" ] || [ -d "$d/data/profiles" ]; then
    echo "  logins        agent logins in ptyro-managed profiles (log in again afterwards)"
    break
  fi
done
lost=0
n=$(printf '%s' "$wts" | grep -c . || true)
if [ "$n" -gt 0 ]; then
  echo "  worktrees     $n (their branches stay in the repositories):"
  # A here-document, not a pipe: `lost` must survive the loop.
  while IFS="$(printf '\t')" read -r path repo branch dirty locked; do
    [ -n "$path" ] || continue
    note=""
    if [ "$dirty" -gt 0 ]; then note="  ⚠ $dirty uncommitted change(s) will be lost"; lost=$((lost + 1)); fi
    [ -n "$locked" ] && note="$note  (locked)"
    echo "                  $branch  $(tilde "$path")$note"
  done <<EOF
$wts
EOF
fi
[ -n "$service" ] && echo "  service       $(tilde "$service") (starts the daemon at login)"
[ -n "$integrations" ] && echo "  integrations  $integrations"
echo "Kept: your repositories, their branches and .ptyro.toml, and any \`ptyro shell-init\` line in your shell's rc file."

if [ "$lost" -gt 0 ]; then
  printf 'Uncommitted work in %s worktree(s) will be lost. Type delete to go on: ' "$lost"
  read -r answer
  [ "$answer" = delete ] || { echo "nothing removed"; exit 1; }
elif [ "${YES:-}" != 1 ]; then
  printf 'Reset ptyro? [y/N] '
  read -r answer
  case "$answer" in y | Y | yes) ;; *) echo "nothing removed"; exit 1 ;; esac
fi

for a in $integrations; do
  "$bin" integrations uninstall "$a" >/dev/null 2>&1 && echo "removed the $a integration"
done
if [ -n "$service" ]; then
  "$bin" daemon uninstall-service >/dev/null 2>&1 || rm -f "$service"
  echo "removed the login service"
fi
"$bin" daemon stop --force >/dev/null 2>&1 || true
# Never delete files under a daemon that's still running (one this client couldn't reach).
echo "$dirs" | while IFS= read -r d; do
  for lock in "$d/daemon.lock" "$d/state/daemon.lock"; do
    pid=$(cat "$lock" 2>/dev/null || true)
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && ps -p "$pid" -o command= | grep -q ptyro; then
      echo "the daemon (pid $pid) is still running and didn't answer; stop it (kill $pid) and run this again"
      exit 1
    fi
  done
done || exit 1
echo "stopped the daemon"
while IFS="$(printf '\t')" read -r path repo branch dirty locked; do
  [ -n "$path" ] || continue
  if [ -n "$repo" ] && [ -d "$repo" ]; then
    git -C "$repo" worktree remove --force --force "$path" 2>/dev/null || rm -rf "$path"
    git -C "$repo" worktree prune 2>/dev/null || true
  else
    rm -rf "$path"
  fi
done <<EOF
$wts
EOF
[ "$n" -gt 0 ] && echo "removed $n worktree(s)"
echo "$dirs" | while IFS= read -r d; do
  [ -e "$d" ] || continue
  rm -rf "$d"
  echo "removed $(tilde "$d")"
done
echo "ptyro is reset: \`ptyro\` starts from the first run."
