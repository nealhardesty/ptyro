#!/usr/bin/env bash
# ROADMAP WIN0.1 (R-WIN-05): does WSL keep the ptyro daemon alive after the last terminal closes?
# Run inside a WSL2 distro, from a checkout on the distro's own filesystem.
#
#   make spike-wsl ARGS="start [label]"   start three heartbeats, then close every WSL terminal
#   make spike-wsl ARGS=check             (an hour later, in a new terminal) how long each lived
#   make spike-wsl ARGS=clean             stop everything the spike started (reports are kept)
#   make spike-wsl                        what's running and the reports so far
#
# Three things beat every 10 s into their own file, each line "<epoch> <boot id>":
#   control  a plain background loop (setsid + nohup): does the distro keep any process?
#   bare     a ptyro session in a daemon started on demand, as a user gets by just running ptyro
#   service  a ptyro session in a daemon run by a systemd user unit (only when systemd is PID 1)
# Each daemon has its own PTYRO_HOME, so your real ptyro is never touched. The last beat says when
# each one stopped, even though opening a terminal to run `check` starts the distro again; a new
# boot id means the whole VM restarted, a new PID 1 start time means only the distro did.
#
# The matrix (WIN0.1): run start/check once per setting, labelled, changing one thing at a time:
#   - nothing set (the default WSL behaviour)
#   - /etc/wsl.conf [boot] systemd=true          (then `wsl --shutdown` from Windows)
#   - loginctl enable-linger "$USER"             (systemd user services outlive the login)
#   - %USERPROFILE%\.wslconfig [wsl2] vmIdleTimeout=-1, and instanceIdleTimeout=-1 where the WSL
#     release knows it (then `wsl --shutdown`)
# Every report records all of these as found, so the label is only a reminder.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
bin="${PTYRO_BIN:-$root/target/debug/ptyro}"
state="${SPIKE_WSL_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/ptyro-spike-wsl}"
run="$state/run"
unit=ptyro-spike-wsl.service
unit_file="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/$unit"
beat_every=${SPIKE_WSL_BEAT:-10}
unset PTYRO_SOCKET PTYRO_SESSION_ID PTYRO_CONFIG
export PTYRO_ALLOW_NESTED=1

say() { printf '%s\n' "$*"; }
die() { printf 'spike-wsl: %s\n' "$*" >&2; exit 1; }

in_wsl() {
  [ -n "${WSL_DISTRO_NAME:-}" ] || grep -qi microsoft /proc/sys/kernel/osrelease 2>/dev/null
}
boot_id() { cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo none; }
pid1_start() { ps -o lstart= -p 1 2>/dev/null | sed 's/  */ /g; s/^ //; s/ $//' || echo unknown; }
systemd_on() { [ "$(ps -o comm= -p 1 2>/dev/null)" = systemd ]; }
# Windows commands through WSL interop; empty when interop is off.
win() { "$@" 2>/dev/null | tr -d '\0\r' || true; }
win_home() {
  local p
  p=$(win cmd.exe /c 'echo %USERPROFILE%')
  [ -n "$p" ] && wslpath "$p" 2>/dev/null || true
}
fmt_time() { date -d "@$1" '+%Y-%m-%d %H:%M:%S' 2>/dev/null || date -r "$1" '+%Y-%m-%d %H:%M:%S'; }

# The loop every heartbeat runs: `sh -c "$beat" <file>`.
beat='while :; do printf "%s %s\n" "$(date +%s)" "$(cat /proc/sys/kernel/random/boot_id 2>/dev/null || echo none)" >> "$1"; sleep '"$beat_every"'; done'

environment() {
  say "## Environment"
  say
  say "- recorded: $(date '+%Y-%m-%d %H:%M:%S %Z')"
  say "- distro: ${WSL_DISTRO_NAME:-not WSL} ($(. /etc/os-release 2>/dev/null; echo "${PRETTY_NAME:-unknown}"))"
  say "- kernel: $(uname -r)"
  say "- boot id: $(boot_id); PID 1: $(ps -o comm= -p 1 2>/dev/null) started $(pid1_start)"
  say "- systemd as PID 1: $(systemd_on && echo yes || echo no)"
  say "- linger for $USER: $(loginctl show-user "$USER" -p Linger --value 2>/dev/null || echo unknown)"
  say "- ptyro: $("$bin" --version 2>/dev/null || echo "not built: $bin")"
  say
  say '`wsl.exe --version`:'
  say '```'
  win wsl.exe --version || true
  say '```'
  say '`/etc/wsl.conf`:'
  say '```'
  cat /etc/wsl.conf 2>/dev/null || say "(none)"
  say '```'
  local wh
  wh=$(win_home)
  say '`%USERPROFILE%\.wslconfig`:'
  say '```'
  if [ -n "$wh" ] && [ -f "$wh/.wslconfig" ]; then tr -d '\r' < "$wh/.wslconfig"
  elif [ -n "$wh" ]; then say "(none)"
  else say "(not readable: WSL interop is off)"; fi
  say '```'
}

stop_all() {
  [ -d "$run" ] || return 0
  if [ -f "$run/control.pid" ]; then
    kill "$(cat "$run/control.pid")" 2>/dev/null || true
  fi
  if [ -d "$run/bare" ]; then
    PTYRO_HOME="$run/bare" "$bin" daemon stop --force >/dev/null 2>&1 || true
  fi
  if [ -f "$unit_file" ]; then
    systemctl --user disable --now "$unit" >/dev/null 2>&1 || true
    rm -f "$unit_file"
    systemctl --user daemon-reload 2>/dev/null || true
  fi
  if [ -d "$run/service" ]; then
    PTYRO_HOME="$run/service" "$bin" daemon stop --force >/dev/null 2>&1 || true
  fi
  rm -rf "$run"
}

# A ptyro session that beats into $run/<name>.beats, in the daemon under PTYRO_HOME=$run/<name>.
ptyro_beat() {
  local name=$1
  PTYRO_HOME="$run/$name" "$bin" new -d --name heartbeat --cwd "$run" -- \
    sh -c "$beat" sh "$run/$name.beats" >/dev/null
  PTYRO_HOME="$run/$name" "$bin" daemon status --json > "$run/$name.status.json"
}

wait_daemon() {
  local i
  for i in $(seq 1 100); do
    PTYRO_HOME="$1" "$bin" daemon status --json >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  return 1
}

start() {
  local label=${1:-unlabelled}
  in_wsl || [ -n "${SPIKE_WSL_ANY:-}" ] ||
    die "not in WSL (set SPIKE_WSL_ANY=1 to try the mechanics elsewhere)"
  [ -x "$bin" ] || die "no ptyro at $bin: run \`make build\` (or set PTYRO_BIN)"
  case "$root" in /mnt/[a-z]/*) say "note: this checkout is on a Windows drive ($root); fine for the spike, slow for building";; esac
  stop_all
  mkdir -p "$run"
  printf '%s\n' "$label" > "$run/label"
  date +%s > "$run/started"
  boot_id > "$run/boot_id"
  pid1_start > "$run/pid1_start"
  environment > "$run/environment.md"

  # control
  local detach=nohup
  command -v setsid >/dev/null && detach=setsid
  "$detach" nohup sh -c "$beat" sh "$run/control.beats" >/dev/null 2>&1 < /dev/null &
  echo $! > "$run/control.pid"

  # bare: the daemon `ptyro new` starts on demand
  mkdir -p "$run/bare"
  ptyro_beat bare

  # service: a systemd user unit, like `ptyro service install` writes, with its own home
  if systemd_on && systemctl --user show-environment >/dev/null 2>&1; then
    mkdir -p "$run/service" "$(dirname "$unit_file")"
    cat > "$unit_file" <<UNIT
# Written by scripts/spike-wsl.sh (ROADMAP WIN0.1); removed by \`make spike-wsl ARGS=clean\`.
[Unit]
Description=ptyro WSL survival spike

[Service]
Type=simple
ExecStart="$bin" daemon start --foreground
Environment="PTYRO_HOME=$run/service" "PTYRO_ALLOW_NESTED=1"
KillMode=mixed
TimeoutStopSec=45
Restart=on-failure
RestartPreventExitStatus=5

[Install]
WantedBy=default.target
UNIT
    systemctl --user daemon-reload
    systemctl --user enable --now "$unit" >/dev/null 2>&1
    wait_daemon "$run/service" || die "the systemd unit's daemon didn't answer: journalctl --user -u $unit"
    ptyro_beat service
  else
    say "service: skipped (systemd isn't PID 1, or there's no user instance); see the matrix at the top of $0"
  fi

  sleep 1
  say
  say "spike-wsl started ($label) at $(fmt_time "$(cat "$run/started")")."
  for v in control bare service; do
    [ -f "$run/$v.beats" ] && say "  $v: beating into $run/$v.beats"
  done
  say
  say "Now close EVERY WSL terminal (Windows Terminal tabs, VS Code Remote-WSL windows, anything"
  say "running wsl.exe), and don't open one or run wsl.exe for at least an hour. Then:"
  say "  cd $root && make spike-wsl ARGS=check"
}

# How long one heartbeat lived: "<name>|<first>|<last>|<beats>|<alive>|<vm restarted>".
lived() {
  local name=$1 f="$run/$1.beats" now first last last_boot alive restarted
  [ -s "$f" ] || { echo "$name|||0|no beats|"; return; }
  now=$(date +%s)
  first=$(head -n1 "$f" | cut -d' ' -f1)
  last=$(tail -n1 "$f" | cut -d' ' -f1)
  last_boot=$(tail -n1 "$f" | cut -d' ' -f2)
  alive=no
  [ $((now - last)) -le $((beat_every * 3)) ] && alive=yes
  restarted=no
  [ "$last_boot" != "$(boot_id)" ] && restarted=yes
  echo "$name|$first|$last|$(wc -l < "$f" | tr -d ' ')|$alive|$restarted"
}

check() {
  [ -f "$run/started" ] || die "nothing running: make spike-wsl ARGS=\"start [label]\""
  local started label now report
  started=$(cat "$run/started")
  label=$(cat "$run/label")
  now=$(date +%s)
  report="$state/report-$(date +%Y%m%d-%H%M%S)-${label//[^A-Za-z0-9_-]/_}.md"
  {
    say "# WSL survival spike: $label"
    say
    say "Started $(fmt_time "$started"), checked $(fmt_time "$now") ($(( (now - started) / 60 )) min later)."
    say
    say "| heartbeat | beats | last beat | lived | still running |"
    say "|---|---|---|---|---|"
    local v line name first last beats alive vm
    for v in control bare service; do
      [ -f "$run/$v.beats" ] || continue
      line=$(lived "$v")
      IFS='|' read -r name first last beats alive vm <<< "$line"
      if [ "$beats" = 0 ]; then
        say "| $name | 0 | — | never beat | no |"
      else
        say "| $name | $beats | $(fmt_time "$last") | $(( (last - started) / 60 )) min | $alive |"
      fi
    done
    say
    local vm_now distro_now
    vm_now=$([ "$(cat "$run/boot_id")" = "$(boot_id)" ] && echo "no" || echo "**yes** (boot id changed)")
    distro_now=$([ "$(cat "$run/pid1_start")" = "$(pid1_start)" ] && echo "no" || echo "**yes** (PID 1 started $(pid1_start))")
    say "- the WSL VM restarted since start: $vm_now"
    say "- the distro restarted since start: $distro_now"
    if [ -f "$unit_file" ]; then
      say
      say "\`journalctl --user -u $unit\` (last 15 lines):"
      say '```'
      journalctl --user -u "$unit" --since "@$started" --no-pager -o short-iso 2>/dev/null | tail -n 15 || true
      say '```'
    fi
    if command -v journalctl >/dev/null && journalctl --list-boots >/dev/null 2>&1; then
      say
      say "\`journalctl --list-boots\` (last 3; a persistent journal shows how the previous boot ended):"
      say '```'
      journalctl --list-boots --no-pager 2>/dev/null | tail -n 3 || true
      say '```'
    fi
    say
    say "As started:"
    say
    sed 's/^## /### /' "$run/environment.md"
    say
    say "Now (if a setting changed since start, the run measured the old one):"
    say
    environment | sed '1,2d'
  } > "$report"
  cat "$report"
  say
  say "Report: $report"
  say "Write the finding into SPEC §20 with [V] and the WSL version, then run the next setting"
  say "(make spike-wsl ARGS=\"start <label>\") or make spike-wsl ARGS=clean."
}

status() {
  if [ -f "$run/started" ]; then
    say "running since $(fmt_time "$(cat "$run/started")") ($(cat "$run/label")):"
    local v line name first last beats alive vm
    for v in control bare service; do
      [ -f "$run/$v.beats" ] || continue
      IFS='|' read -r name first last beats alive vm <<< "$(lived "$v")"
      say "  $name: $beats beats, still running: $alive"
    done
  else
    say "nothing running. make spike-wsl ARGS=\"start [label]\" to begin (see the top of $0)."
  fi
  local r
  for r in "$state"/report-*.md; do [ -f "$r" ] && say "report: $r"; done
  return 0
}

case "${1:-}" in
  start) shift; start "${1:-}" ;;
  check) check ;;
  clean) stop_all; say "spike-wsl stopped; reports kept in $state" ;;
  ""|status) status ;;
  *) die "unknown '$1': start [label] | check | clean | status" ;;
esac
