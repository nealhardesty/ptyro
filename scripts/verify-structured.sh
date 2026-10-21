#!/bin/sh
# Verify structured mode (M10) against the real agents installed here: Claude Code over
# stream-json (one tiny turn on your default Claude account, a few tenths of a cent), Codex's
# app-server and Gemini's ACP handshake (no turn: they report a session, or why they can't).
# Agents that aren't installed are skipped. Uses a throwaway ptyro home under
# target/verify-structured; your agents' logins and settings are used as they are.
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
bin="$root/target/debug/ptyro"
dir="$root/target/verify-structured"
export PTYRO_HOME="$dir/ptyro"
unset PTYRO_SOCKET PTYRO_SESSION_ID PTYRO_CONFIG CLAUDECODE
"$bin" daemon stop --force >/dev/null 2>&1
rm -rf "$dir"
mkdir -p "$dir/proj" "$PTYRO_HOME/config"
(cd "$dir/proj" && git init -q .)
: > "$PTYRO_HOME/config/config.toml"
fails=0
pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fails=$((fails + 1)); }
skip() { printf '  \033[33mSKIP\033[0m %s\n' "$1"; }
field() { "$bin" status "$1" --json 2>/dev/null | python3 -c "import json,sys; s=json.load(sys.stdin)['session']; print($2)" 2>/dev/null; }
wait_field() { # session expr value seconds
  i=0
  while [ $i -lt $(($4 * 4)) ]; do
    [ "$(field "$1" "$2")" = "$3" ] && return 0
    sleep 0.25
    i=$((i + 1))
  done
  return 1
}

echo "claude (stream-json):"
if command -v claude >/dev/null; then
  printf '[agents.claude]\nargs = ["--model", "haiku"]\n' >> "$PTYRO_HOME/config/config.toml"
  "$bin" new -d -n vc --agent claude --structured -C "$dir/proj" >/dev/null
  if "$bin" send vc "Reply with exactly the single word: pong" --wait-ready 60s --wait --timeout 90s | grep -q idle; then
    pass "a turn ended idle"
  else
    fail "the turn didn't end idle"
  fi
  "$bin" read vc --chat | grep -q '"text":"pong"' && pass "the reply is in the transcript" || fail "no 'pong' in the transcript"
  "$bin" read vc --chat | grep -q '"cost_micro_usd"' && pass "the turn's cost is reported" || fail "no cost reported"
  [ "$(field vc "s['status']['source']")" = "protocol" ] && pass "status comes from the protocol" || fail "status source"
  conv=$(field vc "s['conversation'] or ''")
  [ -n "$conv" ] && pass "the turn has a conversation id ($conv)" || fail "no conversation id: later checks would pass vacuously"
  "$bin" convert vc --to tui >/dev/null 2>&1 && wait_field vc "s.get('structured')" None 20 && pass "opened in its own TUI ($conv)" || fail "convert to the TUI"
  [ -n "$conv" ] && [ "$(field vc "s['conversation']")" = "$conv" ] && pass "the same conversation" || fail "conversation changed"
else
  skip "claude isn't installed"
fi

echo "codex (app-server):"
if command -v codex >/dev/null; then
  "$bin" new -d -n vx --agent codex --structured -C "$dir/proj" >/dev/null
  if wait_field vx "s['conversation'] is not None" True 30; then
    pass "a thread started ($(field vx "s['conversation']"))"
  else
    fail "no thread (see: ptyro read vx --chat)"
  fi
else
  skip "codex isn't installed"
fi

echo "gemini (acp):"
if command -v gemini >/dev/null; then
  "$bin" new -d -n vg --agent gemini --structured -C "$dir/proj" >/dev/null
  if wait_field vg "s['conversation'] is not None" True 60; then
    pass "an ACP session started ($(field vg "s['conversation']"))"
  else
    fail "no ACP session (see: ptyro read vg --chat)"
  fi
else
  skip "gemini isn't installed (an npx alias doesn't count: ptyro needs a command)"
fi

"$bin" daemon stop --force >/dev/null 2>&1
if [ $fails -gt 0 ]; then
  echo "$fails check(s) failed; the transcripts are in $PTYRO_HOME"
  exit 1
fi
echo "all good"
