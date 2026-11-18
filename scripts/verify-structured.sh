#!/bin/sh
# Verify structured mode (M10), what chats run on, against the real agents installed here: one
# tiny chat with each (Claude Code over stream-json with Haiku, Codex's app-server, Gemini's
# ACP; a few tenths of a cent each on your default accounts). Agents that aren't installed are
# skipped. Uses a throwaway ptyro home under target/verify-structured; your agents' logins and
# settings are used as they are.
set -u

root=$(cd "$(dirname "$0")/.." && pwd)
bin="$root/target/debug/ptyro"
dir="$root/target/verify-structured"
export PTYRO_HOME="$dir/ptyro"
unset PTYRO_SOCKET PTYRO_SESSION_ID PTYRO_CONFIG CLAUDECODE
"$bin" daemon stop --force >/dev/null 2>&1
rm -rf "$dir"
mkdir -p "$PTYRO_HOME/config"
printf '[chat]\ntitle = "off"\n' > "$PTYRO_HOME/config/config.toml"
fails=0
pass() { printf '  \033[32mPASS\033[0m %s\n' "$1"; }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$1"; fails=$((fails + 1)); }
skip() { printf '  \033[33mSKIP\033[0m %s\n' "$1"; }
# A field of the chat's engine (the hidden structured session it runs on).
engine() { "$bin" ls --chats --json 2>/dev/null | python3 -c "import json,sys; s=[x for x in json.load(sys.stdin) if x.get('agent')=='$1'][-1]; print($2)" 2>/dev/null; }
# One tiny chat with agent $1, protocol $2.
check() {
  out=$("$bin" chat new --agent "$1" --timeout 120 --json "Reply with exactly the single word: pong" 2>&1)
  echo "$out" | grep -qi 'pong' && pass "it answered" || fail "no answer: $out"
  [ "$(engine "$1" "s.get('structured')")" = "$2" ] && pass "its engine speaks $2" || fail "its engine's protocol"
  [ "$(engine "$1" "(s.get('status') or {}).get('source')")" = "protocol" ] && pass "status comes from the protocol" || fail "status source"
  conv=$(engine "$1" "s.get('conversation') or ''")
  [ -n "$conv" ] && pass "a conversation id ($conv)" || fail "no conversation id"
}

echo "claude (stream-json):"
if command -v claude >/dev/null; then
  printf '[agents.claude]\nargs = ["--model", "haiku"]\n' >> "$PTYRO_HOME/config/config.toml"
  check claude stream-json
else
  skip "claude isn't installed"
fi

echo "codex (app-server):"
if command -v codex >/dev/null; then
  check codex app-server
else
  skip "codex isn't installed"
fi

echo "gemini (acp):"
if command -v gemini >/dev/null; then
  check gemini acp
else
  skip "gemini isn't installed (an npx alias doesn't count: ptyro needs a command)"
fi

"$bin" daemon stop --force >/dev/null 2>&1
if [ $fails -gt 0 ]; then
  echo "$fails check(s) failed; the chats are in $PTYRO_HOME"
  exit 1
fi
echo "all good"
