# functions instead of aliases, so "$@" passes through
claude-work() {
  CLAUDE_CONFIG_DIR="$HOME/.claude-work" command claude "$@"
}

function codex_personal {
  export CODEX_HOME=~/.codex-me
  codex "$@"
}

gh-acme() { GH_CONFIG_DIR=~/.config/gh-acme gh "$@"; }
