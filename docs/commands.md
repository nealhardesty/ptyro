# Commands

Every TUI action has a command. `ptyro --help` and `ptyro <command> --help` are always the final
word; this page groups them so you can find the right one.

Conventions:

- `[x]` is optional, `A|B` is a choice, `…` repeats.
- Every command takes `--remote [ssh://][user@]host[:port]` (or `PTYRO_REMOTE`) to act on another
  machine's daemon over ssh.
- `--json` output shapes are in [`cli-json.md`](cli-json.md).
- Durations look like `30s`, `10m`, `2h`.

## Naming sessions and worktrees

- **`SESSION`** is a name (`brisk-rook`), `project/name`, a short handle from `ptyro ls`
  (`x7k2`), `@last`, or `@focused` (what your TUI shows).
- **`SESSIONS…`**: where several are allowed you can also use a glob (`'build-*'`), `@all`,
  `@live`, `@exited`, `@working` or `@blocked`, joined with commas (`ptyro rm @exited`).
- **`W`** (a worktree) is `project/name` (`myrepo/@root` is the main checkout), a unique name, or
  a path. It defaults to the worktree you're in.

## Sessions

| Command | Does |
|---|---|
| `ptyro` | attach to the most recent session (or start a shell session) |
| `ptyro "TASK…"` | the default agent on that task, in a new worktree named after it (here, outside a repository); then attach |
| `ptyro new [-d] [-n NAME] [-C DIR] [--size COLSxROWS] [--json] [-- CMD…]` | start a session; attaches unless `-d`. No `--agent` means your shell, or `CMD` |
| `ptyro new … --agent A[@PROFILE] [--profile P] [--override-pin] [-y]` | start an agent: `claude`, `codex`, `gemini`, `opencode`, or one from `[agents]` |
| `ptyro new … --wt W \| --new-wt [NAME] [--project P] \| --pr N\|URL \| --issue N\|URL \| --take-changes` | where: an existing worktree, a new one, one on a PR's branch, one for an issue, or one that takes the main checkout's uncommitted changes |
| `ptyro new … --prompt TEXT\|@SNIPPET \| --prompt-file F` · `--title T` | the first prompt, typed once the agent's input box is really there · its title (default: the prompt; the name comes from it too) |
| `ptyro new … --verify CHECK` · `--structured` · `--unqueued` | verified-done · drive it over its protocol (chat view) · skip the review queue's hold |
| `ptyro new … --lead [--max-workers N] [--lead-agent A]` | a lead agent with `ptyro mcp` (default 4 workers) |
| `ptyro attach [SESSION] [-w WS] [--raw] [--view-only]` | open the TUI on a session; `--raw`: that session only, no chrome |
| `ptyro list` (alias `ls`) `[-a] [--status S] [-p P] [-w WS] [--json]` | sessions: where they run, profile, ports; `-a` adds earlier daemon runs |
| `ptyro stop SESSIONS…` | stop gracefully, keeping them listed (agents finish their turn first) |
| `ptyro kill SESSIONS… [--keep] [-s SIGNAL]` | stop (HUP → TERM → KILL) and remove |
| `ptyro restart SESSIONS…` / `rm SESSIONS…` / `rename SESSION NAME` | run exited sessions again / forget them / rename (the address) |
| `ptyro retitle SESSION [TITLE…]` | change its title, what people read (none: clear it) |
| `ptyro resume SESSIONS…` / `hibernate SESSIONS…` | relaunch dormant agents / stop idle ones, keeping them resumable |
| `ptyro convert SESSION [--to tui\|structured] [--force]` | switch an agent between its own TUI and structured mode, same conversation |
| `ptyro adopt SESSION` | the Claude you started by hand in a shell session becomes a managed session (hooks, hibernation, mail), same conversation |

## Scripting

| Command | Does |
|---|---|
| `ptyro send SESSIONS… TEXT [--no-enter] [--raw] [--wait-ready[=D]] [--wait [--timeout D] [--stall D]] [--json]` | type into sessions. `--wait-ready` waits for the input box (2m, or D). `--wait` reports how the turn ended. `--raw` sends keys as written (`\e`, `\r`, `\t`, `\xHH`) |
| `ptyro broadcast SESSIONS… TEXT [--no-enter] [--timeout D]` | one prompt to several, each once it's ready |
| `ptyro wait SESSIONS… [--for working\|blocked\|idle\|done\|errored\|exit\|any-attention] [--match RE [--scrollback]] [--timeout D] [--any] [--stall D] [--json]` | block until they get there (exit 0), time out (7) or exit first (5). Default `--for idle` |
| `ptyro read SESSION [--scrollback [N]] [--ansi] [--chat] [--json]` | the screen, scrollback, or a structured session's transcript |
| `ptyro events [-f] [-s SESSION] [-n N] [--json]` | what happened; `--follow` streams every change as NDJSON |
| `ptyro status [SESSION] [--explain] [--json]` | what every agent is doing; `--explain`: why ptyro thinks so |
| `ptyro answer SESSION allow\|always\|deny` | answer a permission prompt without opening the session |
| `ptyro report --state S [--kind K] [--msg M] [--progress N] [--session S]` | report your own program's state from inside a session |
| `ptyro completions bash\|zsh\|fish\|elvish\|powershell` | the completion script (with live names) |
| `ptyro shell-init bash\|zsh\|fish` | shell integration: prompt marks and live cwd |

## Attention and notifications

| Command | Does |
|---|---|
| `ptyro inbox [-u] [--workspace W] [--read] [--json]` | what wanted you, newest first; `--read` marks all read |
| `ptyro mute SESSION\|project:P\|workspace:W [--for D] [--off]` | silence notifications (the inbox still gets them) |
| `ptyro notify test` | send a test through every channel and sink |
| `ptyro usage [--json]` | rate limits per profile (Claude, Codex) |
| `ptyro integrations ls\|install\|uninstall [gemini\|antigravity] [--profile P]` | the one-time agent integrations that need installing |

## Projects, worktrees, workspaces, profiles

| Command | Does |
|---|---|
| `ptyro project add [PATH] [--workspace W]` / `ls` / `show [P]` / `rm P` | register (to the workspace the TUI showed last, unless `--workspace`) / list / inspect / forget repositories. The repository a session runs in registers by itself; `rm` keeps it from doing so again |
| `ptyro project trust [P] [-y]` / `untrust [P]` | review and trust a project's scripts |
| `ptyro project config [P] [--explain] [--json]` | the effective project config, and where each value came from |
| `ptyro project discover [--refresh]` · `pin P PROFILE` · `unpin P` | find repos under `[discover] roots` · pin a project to a profile |
| `ptyro project suggest [P] [--apply\|--decline] [--json]` | what the project needs (a setup script from its lockfile, `.env` files to copy); `--apply` writes it to `.ptyro.local.toml`. A first worktree asks on its own |
| `ptyro wt new [NAME] [-p P] [--base REF] [--branch B [--existing]] [--pr N\|URL \| --issue N\|URL \| --take-changes] [-a A [--prompt P]] [--title T] [--profile P] [--no-setup] [--no-fetch] [--attach]` | create a worktree (optionally with an agent); `--take-changes` moves the main checkout's uncommitted work into it |
| `ptyro wt ls [--project P]` / `status [W]` / `log [W]` | list worktrees / repository status / commits since the base |
| `ptyro wt diff [W] [--against base\|upstream\|last-turn\|head] [--stat]` | a worktree's changes |
| `ptyro wt commit [W] -m MSG [-a] [-- FILES…]` / `--ask-agent` | commit as the worktree's profile / ask its agent to |
| `ptyro wt integrate W [--into B] [--cherry-pick] [--push]` | rebase onto main and fast-forward it (or cherry-pick) |
| `ptyro wt pair W [--refresh]` / `unpair W` | show W's work in the main checkout / give the checkout back, your edits carried to W |
| `ptyro wt run W [SCRIPT] [--attach]` / `open [W] [-e EDITOR]` | run a lifecycle script / open in an editor |
| `ptyro wt rm W [--force [-y]] [--stop-sessions] [--keep-branch]` | remove safely: refuses, listing what would be lost |
| `ptyro wt lock\|unlock W` / `rename-branch W [NEW]` | manual lock / rename the branch (never once pushed) |
| `ptyro wt retitle W [TITLE…]` | change a worktree's title (`wt new --title`, else its prompt, PR or issue) |
| `ptyro wt finish [W] [--integrate]` | commit what's left (its agent asked to; else its title is the message), then a PR, or integrate |
| `ptyro wt archive [W] [--stop-sessions]` / `restore [P/]NAME` / `archives [-p P] [--json]` | keep its branch and a snapshot of its files, remove the checkout (never loses work) / bring it back as it was / list them |
| `ptyro gc [--dry-run] [--json]` | stale worktrees (gone, merged, unused); asks for each: archive, remove or skip |
| `ptyro ws ls\|new\|set\|rm\|rename\|add\|remove …` | workspaces: `new NAME [--profile P] [--notify all\|badge\|mute]`, `add WS PROJECT` |
| `ptyro profile ls\|show\|new\|rm\|rename\|env\|login\|sync\|detect\|doctor …` | profiles; `detect [--apply]` finds your existing multi-account setups; edits keep your config's comments |

## Review and ship

| Command | Does |
|---|---|
| `ptyro review add W FILE:LINE TEXT` / `ls [W]` / `rm W [ID\|--all]` | line comments for the agent |
| `ptyro review send [W] [--to SESSION] [--note TEXT]` | send every comment as one prompt, once the turn is over |
| `ptyro review queue` / `done W` | worktrees ready for review, best first / mark one reviewed (until its agent's next turn) |
| `ptyro pr create [W] [--draft\|--ready] [--title T] [--body B] [--base B]` | push and open a PR through `gh`; ptyro watches it |
| `ptyro pr status [W] [--refresh]` / `checks [W]` | the PR / its checks (exit 1 failing, 7 running) |
| `ptyro pr fix [W]` / `automerge [W] [--method squash\|merge\|rebase] [--off]` | failing logs → the agent / `gh pr merge --auto` |
| `ptyro pr watch [W] [--off]` (also `ptyro watch-pr`) | new CI failures and review comments go to the agent |
| `ptyro pr reply TEXT [-w W] [--comment ID]` / `open PR` | reply on the PR or a review thread, marked "via ptyro" / back to a PR's session |

## Orchestration

| Command | Does |
|---|---|
| `ptyro fanout (-a A,B,C \| -n N) (-p P \| --prompt-file F) [--test [CHECK]] [--verify CHECK] [--judge A] [--timeout D] [--name PREFIX] [--base REF] [--project P] [--structured] [--wait] [--json]` | one task, several agents, one worktree each |
| `ptyro compare R [--refresh] [--json]` | rank candidates by evidence: tests, diff, time |
| `ptyro pick R C [--cherry-pick \| --pr \| --apply [--path F]… [--hunk F:N]…] [--into T]` | take a candidate (default: merge) |
| `ptyro discard R [C] [--force]` / `iterate R [C] TEXT` | remove the others / a follow-up prompt |
| `ptyro recipe ls\|show NAME\|validate FILE\|graph R [--format mermaid]` | recipes: list, read, check, draw |
| `ptyro recipe run R [--var k=v]… [--base REF] [--trust FP] [--wait] [--json]` | start a recipe |
| `ptyro run ls [--active] [--tree]` / `show R` / `events R` / `wait R [--gate] [--timeout D]` | runs and their steps |
| `ptyro run cancel R` / `retry R STEP` / `fork R --from STEP [--set k=v]` / `lead SESSION` | control runs; make an existing agent a lead |
| `ptyro gate ls [--all]` / `resolve ID CHOICE [--note N] [--key K]` | what runs wait on you for / answer |
| `ptyro msg send TO TEXT [--kind K] [--interrupt]` / `check [--wait D]` / `ls` / `ack [IDS]` | the mailbox between agents and you (`TO`: `@lead`, `@user`, `@all`, `@idle`, `@<agent>`, a name) |
| `ptyro lease claim PATTERN… [--exclusive] [--ttl D] [--as S]` / `release` / `ls` | file leases |
| `ptyro radar [--project P]` | overlapping worktrees and predicted conflicts |
| `ptyro checkpoint ls [S\|W]` / `diff S N [M] [--stat]` / `restore S\|W [N] [--turn T] [-y]` | the worktree after every agent turn |
| `ptyro verify SESSION [CHECK] [--max-bounces N]` / `--off` | verified-done for a session |
| `ptyro mcp` | the MCP server a lead agent gets (started for it) |

## Change sets

See [`changesets.md`](changesets.md) for the walkthrough.

| Command | Does |
|---|---|
| `ptyro cs new NAME --projects A,B… [--template T] [--layout lead\|solo\|manual] [--lead AGENT] [--agent M=A]… [--depends M:D+E]… [--base M=REF]… [-p GOAL \| --prompt-file F \| --from-issue N] [--gates contract,push,land \| --no-gates] [--integration CMD]… [--overlay] [--json]` | start a change set |
| `ptyro cs ls` / `show SET` (alias `status`) / `log SET [-f]` | change sets / one set with its issues and actions / its ledger |
| `ptyro cs msg SET TO TEXT [--kind K] [--interrupt]` | a message on the set (`TO`: a member, `@lead`, `@all`) |
| `ptyro cs ready SET [NOTE]` / `blocked SET ON [NOTE]` / `assign SET M TASK` / `ask SET QUESTION [--option O]…` | from inside the set: readiness, blockers, a member's task, a question for you |
| `ptyro cs contract SET [show [--rev N] \| edit \| diff [FILE] \| history \| submit \| revise FILE [--diff] [--affects …] \| approve \| reject NOTE]` | the contract and its revisions |
| `ptyro cs diff SET M` / `check SET` / `overlay SET [auto\|go\|rust\|node\|python\|compose\|off] [--member M]` | a member's changes / integration and contract checks / build against siblings |
| `ptyro cs checkpoint SET [LABEL]` / `checkpoints SET` / `restore SET N [--force]` | undo across every repo |
| `ptyro cs commit SET -m MSG` / `push SET [-y]` / `pr SET [--draft]` / `land SET [-y] [--no-wait]` | lockstep git and the merge train |
| `ptyro cs add SET PROJECT [--agent A] [--depends-on M]` / `drop SET M [--force]` / `rm SET [--keep M] [--force]` / `templates` | membership, removal, templates |

## Remote and phone

See [`remote.md`](remote.md).

| Command | Does |
|---|---|
| `ptyro serve [--listen ADDR] [--tailscale] [--insecure]` | the web app and the protocol over WebSocket for paired devices |
| `ptyro pair [NAME] [--scope read\|approve\|control\|full] [--url URL] [--json]` | pair a phone or browser: a QR code with a one-time code (10 minutes; scope `approve` by default), then waits for the device |
| `ptyro devices [ls \| revoke ID \| push-test]` | paired devices |
| `ptyro --remote HOST …` / `ptyro proxy` | any command against another machine's daemon / the bridge it runs there |

## Recordings

| Command | Does |
|---|---|
| `ptyro replay SESSION [--speed X] [--max-idle D] [--from D]` | play a session's recording back in your terminal |
| `ptyro export SESSION [--format asciicast\|text\|ptylog] [-o FILE]` | the recording as asciicast v2, plain text, or a test fixture |

## Setup, config and the daemon

| Command | Does |
|---|---|
| `ptyro init [-y] [--dry-run]` | detect agents, accounts and repositories, then write your config |
| `ptyro doctor [--quick] [--json]` | diagnose agents, logins, integrations, terminal, tmux, daemon, git |
| `ptyro tour` | the 60-second tour of the TUI's keys |
| `ptyro config path\|check\|show\|edit` | your config; `edit` opens `$EDITOR` and validates |
| `ptyro daemon start [--foreground]` / `status [--json]` / `logs [-n N] [-f]` | the background daemon (it starts by itself); `status` includes its health |
| `ptyro daemon stop\|restart [--hibernate] [--force]` | stop it; `--hibernate` brings resumable agents back dormant |
| `ptyro daemon upgrade [--binary PATH] [--json]` | switch the running daemon to a new binary without stopping sessions; exits 1 if it rolled back |
| `ptyro daemon install-service\|uninstall-service [--dry-run]` | start the daemon at login (systemd user unit / launchd agent) |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | ok (`wait`: got there) |
| `1` | error (`doctor`: found a problem; `pr checks`: a check fails) |
| `2` | invalid arguments |
| `3` | not found |
| `4` | ambiguous: a handle or selector matched several |
| `5` | conflict: unsaved work, a profile question needing `--yes` / `--override-pin`, or (`wait`) the session exited first |
| `6` | untrusted scripts |
| `7` | timeout (`wait`, `send --wait-ready`), `--stall` saw no output, or (`pr checks`) checks still running |
| `8` | no daemon |
| `9` | the daemon speaks another major protocol version: run `ptyro daemon restart --hibernate` (within a major version, `ptyro daemon upgrade` switches live) |
