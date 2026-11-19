# Codex 0.167.1: real rollouts, sanitized

Written by `make verify-kb ARGS=--fixtures` (`scripts/verify-kb.py`) from tiny `codex exec` turns in a
throwaway folder, for the transcript reader of ROADMAP KB1.4 (SPEC §20.8, §25.6.1). Every line Codex
wrote is here, in its order. Reduced: `session_meta` (its instructions and the account's ids),
`world_state`, the tool list of `additional_tools`, `rate_limits`, the text of Codex's own context messages
(`developer`, and `user` ones that are not `user.text`: only their opening tag is kept), and long
strings in tool outputs and events; paths are rewritten to `/home/u/proj`.

- `plain.jsonl`: one turn, no tools. "Reply with only the word OK."
- `tools.jsonl`: one turn with a file edit (`apply_patch`) and a shell command (`cat`). This model
  calls both through the `exec` custom tool, whose `input` is JavaScript (`tools.apply_patch(…)`,
  `tools.exec_command({cmd: …})`); the `event_msg` `item_completed` lines name what really ran.
- `resumed.jsonl`: the same thread after `codex exec resume <id>`: two turns in one file, one
  `session_meta`. `tools.jsonl` is a byte prefix of the unreduced file.
- `mcp.jsonl`: one turn that looks a deferred MCP tool up (`ALL_TOOLS`) and calls
  `tools.mcp__ptyro__recall(…)`, again through `exec`.

A reader keeps: `response_item` messages of role `user` whose `content_item_kinds` include
`user.text`, `assistant` messages' `output_text`, and a title per `function_call` and
`custom_tool_call`. It skips `developer` messages, other `user` messages, every `*_output`,
`event_msg`, `world_state`, `turn_context`, `token_usage_record` and `additional_tools`. Not here: a
compaction (`exec` has no `/compact`) and `function_call` items (this model made none).
