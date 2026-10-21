# ~/.bashrc: one account exported for everything, another via an env alias
export CLAUDE_CONFIG_DIR="$HOME/.claude-main"
[ -f ~/.bash_aliases ] && . ~/.bash_aliases
alias cw='env CLAUDE_CONFIG_DIR=~/.claude-work claude --model opus'
