# Claude Code 2.1.329: real transcripts, sanitized

Written by `make verify-kb ARGS=--fixtures` (`scripts/verify-kb.py`) from tiny turns in a throwaway
folder, for the transcript reader of ROADMAP KB1.4 (SPEC §20.8, §25.6.1). Every line Claude Code
wrote is here, in its order. Reduced: `attachment` lines keep only their type, `api-request*` lines
only their ids (both held the system prompt, skill lists and the account's email), thinking
signatures, the account's ids; paths are rewritten to `/home/u/proj`.

- `plain.jsonl`: one turn, no tools. "Reply with only the word OK."
- `tools.jsonl`: one turn with tool calls: `Write` (a file edit), `Bash` (a shell command),
  `ToolSearch` (loading a deferred MCP tool) and `mcp__ptyro__recall`, some in one assistant message
  (several lines sharing a `message.id`), each result in a `user` line.
- `resumed.jsonl`: the same conversation after two `claude --resume <id>` turns: three turns in one
  file, the same `sessionId`. Its second turn holds a tool call that was refused (`is_error`).
  `tools.jsonl` is a byte prefix of the unreduced file, as a cursor would see it.
- `compacted.jsonl`: the same conversation after `/compact`: a `system`/`compact_boundary` line, the
  `user` line with `isCompactSummary`, then the slash command itself (an `isMeta` caveat, a
  `<command-name>` line, a `<local-command-stdout>` line). Its timestamps are not in file order.

A reader keeps: `user` lines whose content is a string and that are not `isMeta` (the user's words;
a slash command as `/name`), `text` blocks of `assistant` lines, a title per `tool_use`, and the
`isCompactSummary` line as a summary. It skips everything else, `queue-operation` and `last-prompt`
among it, which repeat the prompt. Not here: a rewind (it needs the TUI) and a sub-agent's file.
