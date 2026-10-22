#!/bin/sh
# M7 orchestration, end to end, with no real agents: a throwaway ptyro (target/orchestrate-demo)
# whose claude, codex and gemini are `ptyro fake-agent`. It runs a three-agent fan-out and its
# evidence-ranked comparison, merges the best, runs a recipe with a verified-done bounce and a
# gate, and lets a lead agent start workers through `ptyro mcp` within its policy.
#
#   make orchestrate-demo           run it (about 20 s), then leave the daemon up to explore
#   make orchestrate-demo ARGS=tui  …and open the TUI on it (Ctrl-Q O: runs)
#   make orchestrate-demo ARGS=clean
set -eu

root=$(cd "$(dirname "$0")/.." && pwd)
dir="$root/target/orchestrate-demo"
bin="$root/target/debug/ptyro"
export PTYRO_HOME="$dir/ptyro" PTYRO_BIN="$bin" HOME="$dir/home" GIT_CONFIG_NOSYSTEM=1 SHELL=/bin/sh
unset PTYRO_SOCKET PTYRO_SESSION_ID PTYRO_CONFIG
export PTYRO_ALLOW_NESTED=1
p() { "$bin" "$@"; }
say() { printf '\n\033[1;36m▸ %s\033[0m\n' "$*"; }

if [ "${1:-}" = clean ]; then
  p daemon stop --force >/dev/null 2>&1 || true
  rm -rf "$dir"
  echo "orchestrate-demo removed"
  exit 0
fi
if [ "${1:-}" = tui ] && [ -d "$dir/app" ]; then
  cd "$dir/app" && exec "$bin"
fi

p daemon stop --force >/dev/null 2>&1 || true
rm -rf "$dir"
mkdir -p "$HOME" "$PTYRO_HOME/config"
printf '[user]\n\tname = Demo\n\temail = demo@example.invalid\n[init]\n\tdefaultBranch = main\n' > "$HOME/.gitconfig"
{
  printf '[attention]\nmin_work = "1s"\n[worktrees]\nfetch = false\n'
  for a in claude codex gemini; do
    printf '[agents.%s]\ncommand = ["%s", "fake-agent", "--as", "%s"]\n' "$a" "$bin" "$a"
  done
} > "$PTYRO_HOME/config/config.toml"
git init -q --bare "$dir/app.git"
git clone -q "$dir/app.git" "$dir/app" 2>/dev/null
cd "$dir/app"
echo "hello" > README.md
git add -A && git commit -qm init && git push -q origin main
p project add . >/dev/null

wait_run() {
  if [ "${2:-}" = gate ]; then p run wait "$1" --gate --timeout 60s; else p run wait "$1" --timeout 120s; fi
}

say "fan-out: the same task to claude, codex and gemini, each in its own worktree"
run=$(p fanout --agents claude,codex,gemini -p 'sh echo $PTYRO_STEP_ID > answer.txt' --name answer --test 'grep -q claude answer.txt' 2>/dev/null)
wait_run "$run"
say "ptyro compare $run   (ranked by evidence: tests, the base, diff, time)"
p compare "$run"
say "ptyro pick $run claude   (commit its work, integrate into main)"
p pick "$run" claude
echo "main now has: $(git show main:answer.txt)"

say "a recipe: implement with verified-done (bounced once), check, gate, ship"
cat > "$dir/ship.toml" <<'TOML'
name = "ship-it"
vars = { task = { required = true } }
[[step]]
id = "impl"
spawn = { agent = "claude", worktree = "new", name = "impl" }
prompt = "{{vars.task}}"
done_when = { run = "test -f .second || (touch .second; exit 1)", max_bounces = 2 }
[[step]]
id = "gate"
needs = ["impl"]
gate = { ask = "Ship impl ({{steps.impl.outputs.diffstat}})?", options = ["integrate", "discard"] }
[[step]]
id = "ship"
needs = ["gate"]
if = "gates.gate.choice == 'integrate'"
apply = { from = "impl", actions = ["commit", "integrate"] }
TOML
p recipe graph "$dir/ship.toml"
r2=$(p recipe run "$dir/ship.toml" --var 'task=edit feature.txt shipped' 2>/dev/null)
wait_run "$r2" gate
p gate ls
gate=$(p gate ls --json | sed -n 's/.*"id": "\(g-[a-z0-9]*\)".*/\1/p' | head -1)
p gate resolve "$gate" integrate
wait_run "$r2"
p run show "$r2"

say "a lead agent with ptyro mcp: two workers, a third refused by its policy"
p new -d --agent claude --lead --max-workers 2 -n lead >/dev/null
sleep 1
p send lead 'mcp spawn_session {"agent":"claude","name":"w1","prompt":"work 0.3 one"}'
p send lead 'mcp spawn_session {"agent":"codex","name":"w2","prompt":"work 0.3 two"}'
p send lead 'mcp spawn_session {"agent":"gemini","name":"w3","prompt":"work 0.3 three"}'
p send lead 'mcp wait_for {"sessions":["w1","w2"],"timeout_s":20}'
sleep 4
p read lead --scrollback 200 | grep -E 'mcp |policy:|"reached"' || true
say "done. Explore: PTYRO_HOME=$PTYRO_HOME $bin run ls · or: make orchestrate-demo ARGS=tui"
