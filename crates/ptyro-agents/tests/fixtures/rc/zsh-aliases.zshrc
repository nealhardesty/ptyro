# oh-my-zsh style ~/.zshrc
export ZSH="$HOME/.oh-my-zsh"
plugins=(git)
alias ll='ls -lah'
# work and personal Claude accounts
alias claude-work='CLAUDE_CONFIG_DIR=~/.claude-work claude'
alias claude-personal="CLAUDE_CONFIG_DIR=${HOME}/.claude-personal claude"
alias codex-work='env CODEX_HOME=$HOME/.codex-work codex'
# alias claude-old='CLAUDE_CONFIG_DIR=~/.claude-old claude'
