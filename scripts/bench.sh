#!/bin/sh
# Throughput and idle-CPU baseline for the daemon (SPEC §18.3). Run via `make bench`.
# Uses a throwaway PTYRO_HOME, so it never touches your sessions. Linux only (reads /proc).
set -eu
P=${1:-target/release/ptyro}
PTYRO_HOME=$(mktemp -d)
export PTYRO_HOME PTYRO_BIN="$P"
unset PTYRO_SOCKET PTYRO_SESSION_ID
trap '"$P" daemon stop --force >/dev/null 2>&1 || true; rm -rf "$PTYRO_HOME"' EXIT

run() { # name size command
  "$P" new -d -n "$1" --size "$2" -- sh -c "$3" >/dev/null 2>&1
  until "$P" ls --json | grep -q "\"name\": \"$1\"" && "$P" read "$1" >/dev/null && \
        "$P" ls | awk -v n="$1" '$2 == n && $3 == "exited" {f=1} END {exit !f}'; do sleep 0.2; done
}
run short 200x50 'seq 1 3000000'
run long  200x50 'head -c 30000000 /dev/zero | tr "\0" x | fold -w 180'
"$P" ls --json | python3 -c '
import sys, json
mb = {"short": 22.9, "long": 30.0 * 181 / 180}
for s in json.load(sys.stdin):
    t = (s["ended_ms"] - s["created_ms"]) / 1000
    n = s["name"]
    print(f"{n:6} {mb[n]:5.1f} MB in {t:5.2f}s = {mb[n] / t:4.0f} MB/s")
' 2>/dev/null || "$P" ls

for i in 1 2 3 4 5 6 7 8 9 10; do "$P" new -d -- sh -c "sleep 600" >/dev/null 2>&1; done
pid=$("$P" daemon status --json | sed -n 's/.*"pid": \([0-9]*\).*/\1/p')
sleep 2
t1=$(awk '{print $14+$15}' "/proc/$pid/stat"); sleep 10; t2=$(awk '{print $14+$15}' "/proc/$pid/stat")
rss=$(awk '/VmRSS/ {print $2}' "/proc/$pid/status")
echo "idle: 10 sessions, $(( (t2 - t1) ))0 ms CPU over 10 s, RSS ${rss} KiB"
