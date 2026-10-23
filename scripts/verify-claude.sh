#!/bin/sh
# Verify the Claude adapter against the real Claude Code on this machine (M4.3): hooks through
# ptyro's plugin, the folder-trust dialog, prompt delivery, a permission prompt answered by ptyro,
# resume and one summary (UX1.3). Spends one tiny turn and one Haiku call on your default Claude
# account. Uses a throwaway ptyro home under target/verify-claude; your Claude login and settings
# are used as they are.
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
bin="$root/target/debug/ptyro"
dir="$root/target/verify-claude"
export PTYRO_HOME="$dir/ptyro"
unset PTYRO_SOCKET PTYRO_SESSION_ID PTYRO_CONFIG CLAUDECODE
"$bin" daemon stop --force >/dev/null 2>&1
rm -rf "$dir"
mkdir -p "$dir/proj" "$PTYRO_HOME/config"
printf '[attention]\nmin_work = "1s"\n[agents.claude]\nargs = ["--permission-mode", "default"]\n' > "$PTYRO_HOME/config/config.toml"
fails=0
pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fails=$((fails + 1)); }
status() { "$bin" status v 2>/dev/null | head -1; }
wait_for() { # pattern seconds
  i=0
  while [ $i -lt $(($2 * 4)) ]; do
    status | grep -q "$1" && return 0
    sleep 0.25
    i=$((i + 1))
  done
  return 1
}

echo "Claude $(claude --version 2>/dev/null | head -1) under ptyro $("$bin" --version)"
"$bin" new -d -n v --agent claude -C "$dir/proj" --size 120x40 \
  --prompt "Use the Bash tool to run exactly: touch verified.txt — then reply with just the word ok." >/dev/null
if wait_for "trust this folder" 20; then
  pass "the folder-trust dialog shows as blocked (question)"
  "$bin" read v | grep -q "touch verified" && fail "the prompt was typed into the dialog" || pass "nothing typed into the dialog"
  "$bin" send v --raw '\e[B' && sleep 0.3 && "$bin" send v --raw '\r'
else
  echo "  (no trust dialog: this folder was trusted already)"
fi
# Working is brief before the permission prompt: either proves the prompt went in.
wait_for "working\|blocked" 30 && pass "the prompt was delivered and hooks report it" || fail "the prompt never went in: $(status)"
if wait_for "blocked (permission)" 60; then
  pass "permission prompt: $(status | sed 's/^v *//')"
  "$bin" answer v allow && pass "answered allow through ptyro"
else
  fail "never blocked on the permission: $(status)"
fi
wait_for "idle" 60 && pass "back to idle after the turn" || fail "not idle: $(status)"
"$bin" events -s v --json | grep '"reply"' | grep -qv '"chars":0' \
  && pass "Stop carried the turn's reply (last_assistant_message)" \
  || fail "no reply from Stop: summaries fall back to the screen (SPEC §20.1)"
[ -f "$dir/proj/verified.txt" ] && pass "the approved command ran" || fail "verified.txt missing"
# UX1.3: one real summary through the default `[summaries] command` (Haiku, about $0.0007).
sum=$("$bin" summarize v 2>&1)
echo "$sum" | grep -q '^v: ..*' && pass "summary: ${sum#v: }" || fail "no summary: $sum"
"$bin" inbox | grep -q "wants permission" && pass "the inbox has the permission entry" || fail "inbox"
conv=$("$bin" status v | sed -n 's/.*conversation //p')
[ -n "$conv" ] && pass "the session has a conversation id ($conv)" || fail "no conversation id: the resume check would pass vacuously"
"$bin" hibernate v >/dev/null && sleep 1 && "$bin" ls | grep -q "dormant" && pass "hibernated: dormant" || fail "hibernate"
"$bin" resume v >/dev/null
wait_for "idle" 30 && [ -n "$conv" ] && "$bin" status v | grep -qF "$conv" && pass "resumed into conversation $conv" || fail "resume: $(status)"
"$bin" daemon stop --force >/dev/null 2>&1
[ $fails -eq 0 ] && echo "all good" || echo "$fails check(s) failed — see SPEC §8.2 / §20.1"
exit $fails
