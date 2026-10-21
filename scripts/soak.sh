#!/bin/sh
# Soak test (ROADMAP M8 Done-when): fake agents taking turns nonstop, shells printing, a live
# daemon upgrade every UPGRADE_EVERY, and the daemon's health sampled every SAMPLE seconds.
# Fails on a stuck turn, a failed or rolled-back upgrade, a crash, a dead daemon, a leak trend
# (RSS, fds or tasks still growing in the second half) or a blown lag budget.
# Run via `make soak` (DURATION=10m by default; the release soak is DURATION=7d).
# Uses a throwaway PTYRO_HOME, so it never touches your sessions. Samples land in $OUT/samples.csv.
set -eu
P=$(cd "$(dirname "${1:-target/release/ptyro}")" && pwd)/$(basename "${1:-target/release/ptyro}")
DURATION=${DURATION:-10m}
UPGRADE_EVERY=${UPGRADE_EVERY:-120}
SAMPLE=${SAMPLE:-10}
AGENTS=${AGENTS:-4}
SHELLS=${SHELLS:-3}
secs() { case $1 in *d) echo $(( ${1%d} * 86400 ));; *h) echo $(( ${1%h} * 3600 ));; *m) echo $(( ${1%m} * 60 ));; *s) echo "${1%s}";; *) echo "$1";; esac; }
TOTAL=$(secs "$DURATION")

PTYRO_HOME=$(mktemp -d)
OUT=${OUT:-$PTYRO_HOME/soak}
mkdir -p "$OUT" "$PTYRO_HOME/config"
export PTYRO_HOME PTYRO_BIN="$P" SHELL=/bin/sh
unset PTYRO_SOCKET PTYRO_SESSION_ID
cleanup() { kill $(jobs -p) 2>/dev/null || true; "$P" daemon stop --force >/dev/null 2>&1 || true; }
trap cleanup EXIT INT TERM
cat > "$PTYRO_HOME/config/config.toml" <<EOF
[attention]
min_work = "1s"
[agents.claude]
command = ["$P", "fake-agent", "--as", "claude"]
EOF
fail() { echo "FAIL $*" | tee -a "$OUT/failures"; }

echo "soak: $DURATION, $AGENTS agents, $SHELLS shells, upgrade every ${UPGRADE_EVERY}s; home $PTYRO_HOME"

i=0
while [ $i -lt "$AGENTS" ]; do
  i=$((i + 1))
  "$P" new -d -n "agent$i" --agent claude >/dev/null 2>&1
  ( n=0
    while :; do
      n=$((n + 1))
      # A turn of 1–3 s of "work"; a turn that doesn't end within 60 s is stuck.
      if ! "$P" send "agent$i" "work $(( n % 3 + 1 )) soak turn $n" --wait-ready 60s --wait --timeout 60s >/dev/null 2>&1; then
        "$P" daemon status >/dev/null 2>&1 && fail "agent$i: turn $n did not finish"
        sleep 1
      fi
      echo "$n" > "$OUT/agent$i.turns"
    done ) &
done
i=0
while [ $i -lt "$SHELLS" ]; do
  i=$((i + 1))
  # Bursty output with colour and long lines, wrapping the scrollback many times over.
  "$P" new -d -n "shell$i" -- sh -c 'n=0; while :; do n=$((n+1)); seq 1 2000 | sed "s/^/\x1b[3$((n%8))mline $n /"; printf "\033[0m"; sleep 1; done' >/dev/null 2>&1
done

echo "t_s,rss_kib,open_fds,tasks,lag_p99_ms,lag_max_ms,crashes,generation,sessions,bytes_read" > "$OUT/samples.csv"
start=$(date +%s)
next_up=$((start + UPGRADE_EVERY))
upgrades=0
while :; do
  now=$(date +%s)
  t=$((now - start))
  [ $t -ge "$TOTAL" ] && break
  if ! "$P" daemon status --json > "$OUT/status.json" 2>/dev/null; then
    fail "daemon not answering at ${t}s"; break
  fi
  python3 - "$OUT/status.json" "$t" >> "$OUT/samples.csv" <<'PY'
import json, sys
s = json.load(open(sys.argv[1])); m = s.get("metrics") or {}
print(",".join(str(x) for x in [sys.argv[2], m.get("rss_kib", 0), m.get("open_fds", 0), m.get("tasks_alive", 0),
      m.get("loop_lag_ms_p99", 0), m.get("loop_lag_ms_max", 0), m.get("crashes", 0), s.get("generation", 0),
      s.get("sessions", 0), m.get("bytes_read", 0)]))
PY
  if [ "$now" -ge "$next_up" ]; then
    if "$P" daemon upgrade --json > "$OUT/upgrade.json" 2>>"$OUT/upgrade.log"; then
      upgrades=$((upgrades + 1))
    else
      fail "upgrade at ${t}s: $(cat "$OUT/upgrade.json")"
    fi
    next_up=$((now + UPGRADE_EVERY))
  fi
  sleep "$SAMPLE"
done

turns=0
for f in "$OUT"/agent*.turns; do [ -f "$f" ] && turns=$((turns + $(cat "$f"))); done
echo "soak: ran ${TOTAL}s, $upgrades upgrades, $turns agent turns"
python3 - "$OUT/samples.csv" "$OUT/failures" <<'PY'
import csv, sys
rows = [{k: float(v) for k, v in r.items()} for r in csv.DictReader(open(sys.argv[1]))]
bad = []
if len(rows) < 6:
    print("soak: too few samples for a trend"); sys.exit(0)
# Ignore the warm-up (first quarter: scrollback filling up), then compare the two halves of the rest.
rest = rows[len(rows) // 4:]
a, b = rest[: len(rest) // 2], rest[len(rest) // 2:]
avg = lambda rs, k: sum(r[k] for r in rs) / len(rs)
for k, slack in [("rss_kib", 1.10), ("open_fds", 1.05), ("tasks", 1.10)]:
    x, y = avg(a, k), avg(b, k)
    print(f"  {k:9} {x:10.0f} → {y:10.0f}")
    if y > x * slack + 4:
        bad.append(f"{k} still growing ({x:.0f} → {y:.0f})")
p99 = max(r["lag_p99_ms"] for r in rows)
print(f"  lag p99  max {p99:.0f} ms; crashes {rows[-1]['crashes']:.0f}; generation {rows[-1]['generation']:.0f}")
if p99 > 50: bad.append(f"event-loop lag p99 {p99:.0f} ms > 50 ms")
if rows[-1]["crashes"] > 0: bad.append(f"{rows[-1]['crashes']:.0f} crash report(s) in state/crashes")
for m in bad:
    print("FAIL " + m); open(sys.argv[2], "a").write("FAIL " + m + "\n")
PY
if [ -s "$OUT/failures" ]; then
  echo "soak: FAILED (samples and status in $OUT)"; exit 1
fi
echo "soak: ok"
