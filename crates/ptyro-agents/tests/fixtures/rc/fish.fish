# ~/.config/fish/config.fish
set -gx GH_CONFIG_DIR ~/.config/gh-main
alias claude-me 'CLAUDE_CONFIG_DIR=~/.claude-me claude'
function claude-work
    set -lx CLAUDE_CONFIG_DIR ~/.claude-work
    claude $argv
end
