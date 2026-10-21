# Scripting ptyro

Everything the TUI does, a script can do with `ptyro` commands, and the TUI shows it live
while the script runs. The building blocks:

| Command | What it's for |
|---|---|
| `ptyro new -d --agent A [--new-wt NAME] --prompt TEXT` | start an agent detached (in a fresh worktree), prompt typed once it's ready |
| `ptyro send S… TEXT [--wait-ready] [--wait]` | type into sessions; `--wait` reports how the turn ended |
| `ptyro wait S… --for idle\|blocked\|done\|errored\|exit\|working\|any-attention` | block until they get there (`--any`, `--timeout`, `--stall`) |
| `ptyro wait S --match REGEX [--scrollback]` | block until text appears |
| `ptyro read S [--scrollback [N]] [--json]` | the screen as text |
| `ptyro events --follow` | every change, one JSON line each |
| `ptyro ls --json --status blocked` | who needs you |
| `ptyro answer S allow\|always\|deny` | answer a permission prompt |
| `ptyro broadcast S… TEXT` | one prompt into several sessions |
| `ptyro stop S…` / `ptyro kill S…` | stop gracefully (kept listed) / stop and remove |

Sessions can be selectors: `@working`, `@blocked`, `@idle`, `@live`, `build-*`, `a,b`.

`idle` means **the turn ptyro last typed has ended**. ptyro remembers when it typed your prompt,
and an idle from before that doesn't count, so `new --prompt …` then `wait --for idle` is safe.
Exit codes are stable and documented in [`cli-json.md`](cli-json.md#exit-codes): `0` reached,
`7` timeout or stalled, `5` the session exited first, `8` no daemon.

## Recipes

### One task, start to finish

The milestone's acceptance test does exactly this (`crates/ptyro/tests/m5.rs`):

```sh
#!/bin/sh
set -eu
ptyro wt new fix-login --project app
ptyro new -d -n fixer --agent claude --wt app/fix-login \
  --prompt "The login form rejects valid emails. Fix it and add a test."
ptyro wait fixer --for idle --timeout 30m      # exit 7 if it's still going
ptyro read fixer | tail -20                    # what it said at the end
ptyro wt run app/fix-login test                # your [scripts.run.test]
ptyro wait fix-login.test --for exit
ptyro read fix-login.test | tail -5
ptyro wt diff app/fix-login
```

If the agent stops to ask for permission, `wait --for idle` keeps waiting. Use
`--for idle` together with a check for `blocked`, or `send … --wait` (below), which returns on
either.

### Overnight batch

One worktree and agent per task, all in parallel. In the morning, see who finished and who got
stuck:

```sh
#!/bin/sh
set -eu
while read -r name prompt; do
  ptyro new -d -n "$name" --agent claude --new-wt "$name" --prompt "$prompt"
done <<'EOF'
deps    Upgrade all dependencies to their latest minor versions and fix what breaks.
lint    Fix every clippy warning in crates/ without changing behaviour.
docs    Make every public item in crates/core documented.
EOF
ptyro wait deps lint docs --for idle --timeout 8h --stall 30m || true
ptyro ls --status blocked     # who's waiting for you
ptyro inbox -u                # what each one wanted
```

### Wait for all of them, then test once

```sh
ptyro wait @working --for idle --timeout 1h && make test
```

`--any` returns when the first one is done: `ptyro wait a b c --any --for idle`.

### Turn-by-turn: send, then see how it ended

```sh
case "$(ptyro send fixer "Now run the tests and fix failures" --wait --timeout 20m)" in
  *" idle")    echo "turn done" ;;
  *" blocked") ptyro read fixer | tail -15; ptyro answer fixer allow ;;
  *)           echo "it stopped or timed out" ;;
esac
```

`--json` gives the same as data: `ptyro send … --wait --json | jq -r '.[0].state'`.

### React to events

```sh
ptyro events --follow | jq --unbuffered -r '
  select(.event.t == "session_updated" and .event.info.status.status == "blocked")
  | "\(.event.info.name) needs you: \(.event.info.status.detail // "?")"' |
while read -r line; do notify-send "ptyro" "$line"; done
```

The stream says `events_lost` if your reader falls behind. Re-read state with `ptyro ls --json`
when it does.

### tmux status line

```tmux
set -g status-right '#(ptyro ls --json 2>/dev/null | jq -r "[.[] | select(.status.status == \"blocked\")] | length | if . > 0 then \"⚠ \(.)\" else \"\" end") %H:%M'
set -g status-interval 5
```

### Review, then ship

```sh
ptyro wait fixer --for idle
ptyro wt diff app/fix-login --stat
ptyro review add app/fix-login src/login.rs:42 "handle the empty email case"
ptyro review add app/fix-login src/login.rs:57 "this needs a test"
ptyro review send app/fix-login            # one prompt; typed once the agent's turn is over
ptyro wait fixer --for idle --timeout 30m
ptyro wt commit app/fix-login -m "Fix login validation" --all
ptyro pr create app/fix-login              # draft; ptyro feeds CI failures and review comments back
until ptyro pr checks app/fix-login >/dev/null; do
  [ $? -eq 7 ] || { ptyro pr fix app/fix-login; ptyro wait fixer --for idle; }
  sleep 60
done
ptyro pr automerge app/fix-login
```

`pr checks` exits 0 when everything passed, 1 when something failed and 7 while checks still
run.

### Wait for a dev server

```sh
ptyro wt run app/fix-login dev
ptyro wait fix-login.dev --match 'ready on|listening' --timeout 2m
```

## Teach your agents ptyro (an AGENTS.md / skill snippet)

Agents running inside ptyro can drive other sessions with the same commands. Paste this into a
project's `AGENTS.md` or `CLAUDE.md`, or save it as a skill:

```markdown
## ptyro (you are running inside it)

You run in a ptyro session (`$PTYRO_SESSION_ID` is set). Other agents and shells run next to you.
Use these commands; they print JSON with `--json`:

- `ptyro ls --json`: every session, its `status.status` (working/blocked/idle/done/error) and `activity`.
- `ptyro new -d -n NAME --agent claude --new-wt NAME --prompt "TASK"`: start a helper in its own git worktree.
- `ptyro wait NAME --for idle --timeout 30m`: wait for its turn to end (exit 7 on timeout, 5 if it exited).
- `ptyro read NAME --scrollback 200`: read what it printed.
- `ptyro send NAME "TEXT" --wait-ready --wait`: give it the next instruction and wait for the turn.
- `ptyro report --state working --msg "what you're doing"`: tell the human what you're doing.
- `ptyro msg send @lead "…"` / `ptyro msg send NAME "…"` / `ptyro msg check --wait 5m`: talk to
  the other agents (messages reach them when they're idle; `@user` reaches the human).
- `ptyro lease claim 'src/api/**' --exclusive --reason "…"`: keep others out of files you're
  changing; `ptyro lease release` when done. `ptyro radar` shows who else is changing what.
- `ptyro checkpoint ls`: your worktree after each of your turns (the human can restore one).

Rules: never `ptyro kill` or `ptyro wt rm` sessions or worktrees you didn't create; never answer
another session's permission prompt (`ptyro answer`) or a gate; the human does that (ptyro
refuses agents both). If you're a lead (started with `--lead`), use the `ptyro` MCP tools for
workers; your policy limits how many and where.
```

## Runs from scripts (M7)

Recipes and fan-outs are runs, which a script can start, wait for and read:

```sh
#!/bin/sh
run=$(ptyro fanout --agents claude,codex -p "Fix the flaky test in src/net" --test)
ptyro run wait "$run" --timeout 1h || exit 1          # 0 succeeded, 1 failed, 7 timeout
best=$(ptyro compare "$run" --json | jq -r '.candidates[0].step')
ptyro pick "$run" "$best" --pr                        # push + PR from the winner
ptyro discard "$run" --force
```

```sh
run=$(ptyro recipe run test-fixer --var test=scripts.run.test)
ptyro run wait "$run" --gate                          # returns when a gate needs you
ptyro gate ls --json | jq -r '.[0].gate.id' | xargs -I{} ptyro gate resolve {} pr
```
