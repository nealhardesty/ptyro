<img src="logos/ptyro-icon.png" alt="ptyro" width="72" align="right">


# Commands

Every TUI action has a command. `ptyro --help` and `ptyro <command> --help` are always the final
word; this page groups them so you can find the right one.

Conventions:

- `[x]` is optional, `A|B` is a choice, `…` repeats.
- Every command takes `--remote [ssh://][user@]host[:port]` (or `PTYRO_REMOTE`) to act on another
  machine's daemon over ssh, or `--remote wsl://[distro]` for the daemon in a WSL distro (from
  Windows, or from another distro). Without either, `client.default_remote` is used if set.
- Listing and query commands take `--json`; the shapes are in [`cli-json.md`](cli-json.md).
- Aliases: `a` (attach), `ls` (list), `runs`, `recipes`, `changeset`, `gates`, `leases`,
  `checkpoints` / `ckpt`, `projects`, `workspace`, `worktree`, `profiles`, `report-agent`.
- Durations look like `30s`, `10m`, `2h`.
- A flag's value goes after a space or an `=`: `--project app` and `--project=app` are the same,
  for every flag.
- A command that starts a session you'd work in (`new`, `wt new --agent`, `shells new`,
  `shells split`, `chat task`) attaches to it in a terminal, unless `-d`/`--detach`. In a script,
  or inside a ptyro session (whose TUI shows the new one), it starts detached and prints it.
- A name you may leave out is `-n NAME`; a name the command needs is its first word
  (`ptyro rename S NAME`, `ptyro ws new NAME`).

### Short flags

A letter means the same flag in every command that takes it (owner, 2026-11-09; a test,
`a_short_flag_means_one_thing_everywhere`, keeps it so):

| Short | Long | Means |
|---|---|---|
| `-a` | `--agent` | the agent (`claude`, `codex@work`); where several go, comma-separated or repeated |
| `-C` | `--cwd` | the directory to start in |
| `-d` | `--detach` | start it, don't attach |
| `-e` | `--editor` | the editor to open in |
| `-f` | `--follow` | keep printing what comes |
| `-l` | `--limit` | at most N (lists, events) |
| `-m` | `--message` | a commit or contract message |
| `-n` | `--name` | its name |
| `-o` | `--output` | the file to write |
| `-p` | `--project` | the project (a name or a path; `.` is the one you're in) |
| `-s` | `--signal` | the signal to send |
| `-u` | `--unread` | only unread ones |
| `-w` | `--workspace` | the workspace |
| `-y` | `--yes` | don't ask first |

Everything else is long only: `--prompt`, `--all`, `--force`, `--session`, `--wt`, `--json`.

## Naming sessions and worktrees

- **`SESSION`** is a name (`brisk-rook`), `project/name`, a short handle from `ptyro ls`
  (`x7k2`), `@last`, or `@focused` (what your TUI shows). Names are unique per project, so when
  two projects both have a `build`, say which: `myrepo/build`.
- **`SESSIONS…`**: where several are allowed you can also use a glob (`'build-*'`) or a
  selector, joined with commas (`ptyro rm @exited`, `ptyro send 'build-*,@idle' …`):
  - `@all`: every session, exited and dormant ones too (so `ptyro rm @all` forgets hibernated
    agents as well).
  - `@last`, `@focused`: as for `SESSION`.
  - `@live` (or `@running`): running now. `@active`: busy now (working, waiting for you, or
    running a command).
  - `@exited`: ended and can't resume (exited or failed). Never a hibernated agent.
  - `@dormant`: hibernated agents, which resume their conversation.
  - `@working`, `@blocked`, `@idle`, `@done`, `@error` (or `@errored`): what the agent is doing.

  `ptyro ls --status` takes the same words without the `@` (`ptyro ls --status dormant`), and
  `ptyro wait --for` the status ones (`working` to `error`).
- **`W`** (a worktree) is `project/name` (`myrepo/@root` is the main checkout), a unique name, or
  a path. It defaults to the worktree you're in.
- **A name you give** (a session, worktree, workspace, profile, change set, `fanout --prefix`,
  machine or device) is `a-z 0-9 . _ -`, 1–64 characters. It is kept exactly as you wrote it
  (`cs new my_fix` makes `my_fix`). A bad one is refused, and the error offers the fix:
  `invalid name "My Feature": … try "my-feature"`.
- **Everything else you name** (projects, workspaces, profiles, chats, runs, change sets, devices,
  archives, machines) resolves the same way: the exact name (a project's slug), then
  `project/name` where there is one, then the full id, then the end of the id, **at least 4
  characters** (`7f3k`, `c:7f3k`, `r-k3x9`). Case doesn't matter. When several match, ptyro lists
  them and exits 4; say which. A chat also takes a word of its title, as the last resort.

## Sessions

| Command | Does |
|---|---|
| `ptyro` | attach to the most recent session (or start a shell session) |
| `ptyro "TASK…"` | the default agent on that task, in a new worktree named after it (here, outside a repository); then attach |
| `ptyro new [-d] [-n NAME] [-C DIR] [--size COLSxROWS] [--json] [-- CMD…]` | start a session; attaches in a terminal unless `-d` (`--json` prints it instead). No `--agent` means your shell, or `CMD` |
| `ptyro new … -a A[@PROFILE] [--prompt TEXT \| --prompt-file F] [--profile P] [--override-pin] [-y]` | start an agent: `claude`, `codex`, `gemini`, `opencode`, or one from `[agents]` |
| `ptyro new --again [--wt W] [-d]` | quick new: another of the agent this worktree last started (with its profile and arguments); the project's `default_agent`, else `[agents] default` (claude), when none has |
| `ptyro new … --wt W \| --new-wt [NAME] [-p P] \| --pr N\|URL \| --issue N\|URL \| --take-changes` | where: an existing worktree, a new one, one on a PR's branch, one for an issue, or one that takes the main checkout's uncommitted changes |
| `ptyro new … --prompt TEXT\|@SNIPPET \| --prompt-file F` · `--title T` | the first prompt, typed once the agent's input box is really there · its title (default: the prompt; the name comes from it too) |
| `ptyro new … --verify CHECK` · `--structured` · `--unqueued` | verified-done · drive it over its protocol (chat view) · skip the review queue's hold |
| `ptyro new … --lead [--max-workers N] [--lead-agent A]` | a lead agent with `ptyro mcp` (default 4 workers) |
| `ptyro attach [SESSION] [-w WS] [--raw] [--view-only] [--tab\|--pane]` | open the TUI on a session; `--raw`: that session only, no chrome; `--tab`/`--pane`: in a new tab or pane of this terminal instead (tmux, Windows Terminal) |
| `ptyro list` (alias `ls`) `[--all] [--status S] [-p P] [-w WS] [--json]` | sessions: where they run, profile, ports; `--all` adds earlier daemon runs; `--status running` is what is alive, `--status active` what is busy now (working, blocked, or running a command) |
| `ptyro stop SESSIONS…` | stop gracefully, keeping them listed (agents finish their turn first) |
| `ptyro kill SESSIONS… [--keep] [-s N]` (a signal number, e.g. `-s 9`) | stop (HUP → TERM → KILL) and remove |
| `ptyro restart SESSIONS…` / `rm SESSIONS…` / `rename SESSION NAME` | run exited sessions again / forget them / rename (the address) |
| `ptyro retitle SESSION [TITLE…]` | change its title, what people read (none: clear it; `wt retitle` and `chat retitle` alike) |
| `ptyro resume SESSIONS…` / `hibernate SESSIONS…` | relaunch dormant agents / stop idle ones, keeping them resumable |
| `ptyro convert SESSION [--to tui\|structured] [--force]` | switch an agent between its own TUI and structured mode, same conversation |
| `ptyro summarize SESSION [--json]` | what it's doing, in a line, from a small model (`[summaries] command`, on its profile's account); works with summaries off |
| `ptyro shell [WORKTREE] [--json] [--read [--scrollback [N]]]` | the worktree's quick terminal (one shell per worktree, or per directory outside one): attach with it in the TUI's drawer, print it as JSON, or print its screen (`--read`, which never starts one; an agent can read your terminal this way) |
| `ptyro shells [ls] [-w WS] [--all] [--json]` | your workspace's SHELLS (SPEC §23): numbered terminals of any command that belong to the workspace, not a worktree, by number (`Alt-1…9` in the TUI); a window's later panes are listed under it (`└.2`) |
| `ptyro shells new [-- CMD…] [-C DIR] [--slot N] [--scratch] [-n NAME] [--title T] [-w WS] [-d] [--json]` | start one in SHELLS, here (or `-C DIR`), with the lowest free number or `--slot N`, and attach (unless `-d`); `--scratch`: in a new, empty directory that goes with it |
| `ptyro shells go N [-w WS]` | attach with shell N shown (the same as `ptyro attach %N`); one lost in a restart starts again |
| `ptyro shells move N M [--workspace WS]` | give shell N the number M; a shell already there swaps with it |
| `ptyro shells split [S] [--down] [-d] [--json]` | split shell S's window (default: the one you're in) with a copy of it: a shell in the same directory, under the same profile, as a pane of the same number (`%3.2`), beside it or `--down` below it; at most 4 panes (SPEC §23.17); attaches unless `-d` (or inside a session) |
| `ptyro shells break S` | move a pane out of its window to the lowest free number (tmux's `break-pane`) |
| `ptyro shells join S N [--right]` | move shell S into window N as a pane, below that window's focused pane (`--right`: beside it; tmux's `join-pane`) |
| `ptyro shells loose S` | move a worktree's session into its workspace's SHELLS: it outlives its worktree |
| `ptyro shells adopt S [WORKTREE]` | move a shell from SHELLS into a worktree (default: the one its directory is in; refused across profiles) |
| `ptyro shells watch S [--done\|--activity\|--silence TIME\|--match PATTERN\|--off]` | tell you when its command ends, it prints, it goes quiet, or a line matches (`done` and `match` fire once) |
| `ptyro adopt SESSION` | the Claude you started by hand in a shell session becomes a managed session (hooks, hibernation, mail), same conversation |
| `ptyro scratchpad [edit] [--pane [--json]]` | the scratchpad (alias `pad`): one Markdown notes file for ideas and bugs, `$PTYRO_SCRATCHPAD` in every session. No subcommand: open it in your editor, here. `--pane`: the editor as a session of its own (what `C-q ;` shows), attached |
| `ptyro scratchpad path\|cat [--tail N] [--json]` | where the file is / its text (`--json`: `{path, rev, text, open, done}`) |
| `ptyro scratchpad add [--section HEADING] [--note] [--no-stamp] [--json] TEXT…` | add a line, stamped with the time and the worktree or directory you are in (and the agent's name); `-` or a pipe reads standard input; makes a `- [ ]` item unless `--note` |
| `ptyro scratchpad todo [--all] [--json]` | the open `- [ ]` items with line numbers (`--all`: ticked ones too) |
| `ptyro scratchpad done [--undo] ITEM` | tick (or untick) an item: its line number, or words from exactly one open item |
| `ptyro scratchpad start [-a A] [--dry-run] ITEM` | start the item as a task in a new worktree (like `ptyro "…"`); the item is tagged with the worktree and ticked once its PR merges or the worktree is removed |
| `ptyro scratchpad log [--json]` / `restore REV` | saved copies of the file, newest first / put one back (the current text is saved first) |
| `ptyro chat [new] [PROMPT…] [-a A[@P]] [--preset P] [--folder DIR]… [-w WS] [--title T] [--no-wait] [--json]` | a new chat with your agent (SPEC §22): the answer streams here and the chat is kept (the TUI's and web's CHATS show it). No prompt, at a terminal: the chat view. `-` or a pipe reads the prompt from standard input. `--folder` gives it a folder to read (with no `--preset`, the `read` preset) |
| `ptyro chat ask [--keep] [--agent A] [--preset P] [--timeout S] QUESTION…` | one question, the answer on standard output; standard input is context (`git diff \| ptyro chat ask "review this"`); nothing is kept unless `--keep` |
| `ptyro chat ls [-w WS] [--machine M] [--pinned] [--archived] [-l N] [--json]` | chats, pinned first then the newest; every linked machine's too (`laptop2:c:7f3k`), greyed while it is offline; `--machine here` for this one's |
| `ptyro chat show CHAT [--all] [--format text\|md\|json]` / `open CHAT` | print a chat's shown branch (`--all`: every branch) / open it in the TUI's chat view. CHAT: `c:7f3k`, `7f3k`, a title's words, `@last` |
| `ptyro chat send CHAT TEXT… [--no-wait] [--timeout S]` | a message; the answer streams here |
| `ptyro chat answer CHAT allow\|always\|deny` / `stop CHAT` | answer its permission request / stop the answer being written |
| `ptyro chat retitle CHAT [TITLE… \| --auto]` / `pin` / `unpin` / `archive` / `unarchive` / `rm CHATS…` | title it (none: clear it; `--auto`: a new automatic one) / keep at the top / put away / delete |
| `ptyro chat set CHAT [--agent A] [--preset P] [--folder DIR]… [--no-folders]` | switch its agent, its tools or the folders it reads; the next message carries the conversation over |
| `ptyro chat presets [--json]` | the tool presets, each with what it lets a chat use (⚠ for a shell) |
| `ptyro chat task CHAT [-p P] [-a A] [-n NAME] [-y] [-d] [--json]` | make it a task: an agent session in a new worktree whose first prompt is the chat's conversation and last answer, attached to in a terminal (unless `-d`) (not an incognito chat; asks first when the task's profile isn't the chat's, `--yes` agrees) |
| `ptyro chat export CHAT [--format md\|json] [-o FILE]` | write it out |
| `ptyro chat retry CHAT [NODE] [--no-wait]` / `edit CHAT NODE TEXT…` | ask again, beside the old answer / say a message differently, beside it; the old ones stay as branches (`show --ids` gives the NODEs) |
| `ptyro chat branch CHAT --to NODE` | show the branch through NODE (`show --branches` lists where there are others) |
| `ptyro chat fork CHAT [--at NODE] [--agent A] [--side [--keep]] [--json] [PROMPT…]` | a new chat from the end or a message, on its agent or another (natively for Claude from the end, else seeded); `--side`: a side chat that reads it and never changes it, gone after its answer unless `--keep` |
| `ptyro chat keep CHAT` | keep a side chat as a chat of its own |
| `ptyro chat search QUERY… [--chat C] [-w WS] [-a A] [--pinned] [--archived] [--here] [--meaning\|--words] [-l N] [--json]` | search every chat's messages and titles, on every linked machine (`--here`: this one): words, `"a phrase"`, `prefix*` (3 letters), `-word`, `agent:codex`, `since:7d`, `by:meaning`/`by:words`; each hit's citation (`c:7f3k/node`) and snippet. With `[chat] meaning` on, messages near in meaning are merged in (`≈`); `--meaning` only those (off: offers to switch it on), `--words` none of them |
| `ptyro chat find [--agent A] QUESTION…` | ask your chats: an agent with only the history tools searches them and answers, citing; nothing of it is kept |
| `ptyro chat compare CHAT [-a A,B…] [--yes] [--no-wait] [--json] [TEXT…]` | ask several agents one question (TEXT, or the last message again of those that haven't answered it; default every installed one; a bare name is under the chat's profile, another profile's `agent@p` asks first, `--yes` agrees): each answer is a branch of the chat labelled with its agent (`‹ 2/3 codex ›`), printed under its agent once all are in; `chat new --compare A,B QUESTION` asks a new chat's first message so |
| `ptyro chat pick CHAT [NODE]` | keep an answer: show its branch and switch the chat to the agent that wrote it (the next message goes there) |
| `ptyro chat import [--agent claude\|codex\|gemini\|all] [--profile P] [--since 30d] [--dry-run] [--json]` | import your agents' own histories (every profile's Claude Code, Codex and Gemini sessions) as read-only chats (`⇣`), searchable; again: only what is new. A message to one continues it as a fork. `[chat] import = true` keeps doing it every 10 minutes |
| `ptyro chat ls --imported` | only the imported chats |
| `ptyro chat new --incognito …` / `chat ask` | keep nothing: not saved, not searchable, gone with its agent (`ask` is always incognito unless `--keep`) |

## Scripting

| Command | Does |
|---|---|
| `ptyro send SESSIONS… TEXT [--no-enter] [--raw] [--wait-ready[=D]] [--wait [--timeout D] [--stall D]] [--json]` | type into sessions. `--wait-ready` waits for the input box (2m, or D). `--wait` reports how the turn ended. `--raw` sends keys as written (`\e`, `\r`, `\t`, `\xHH`) |
| `ptyro broadcast SESSIONS… TEXT [--no-enter] [--timeout D]` | one prompt to several, each once it's ready |
| `ptyro wait SESSIONS… [--for working\|blocked\|idle\|done\|error\|exit\|any-attention] [--match RE [--scrollback]] [--timeout D] [--any] [--stall D] [--json]` | block until they get there (exit 0), time out (7) or exit first (5). Default `--for idle` |
| `ptyro read SESSION [--scrollback [N]] [--ansi] [--chat] [--json]` | the screen, scrollback, or a structured session's transcript |
| `ptyro events [-f] [--session S] [-l N] [--json]` | what happened; `--follow` streams every change as NDJSON |
| `ptyro status [SESSION] [--explain] [--json]` | what every agent is doing; `--explain`: why ptyro thinks so |
| `ptyro answer SESSION allow\|always\|deny` | answer a permission prompt without opening the session |
| `ptyro report --state S [--kind K] [--msg M] [--progress N] [--session S]` | report your own program's state from inside a session |
| `ptyro completions bash\|zsh\|fish\|elvish\|powershell` | the completion script (with live names) |
| `ptyro shell-init bash\|zsh\|fish\|pwsh` | shell integration for a shell ptyro didn't start (its own get it automatically): prompt marks, status, live cwd |

## Attention and notifications

| Command | Does |
|---|---|
| `ptyro inbox [-u] [-w WS] [--read] [--json]` | what wanted you, newest first; `--read` marks all read |
| `ptyro mute SESSION\|project:P\|workspace:W [--for D] [--off]` | silence notifications (the inbox still gets them) |
| `ptyro notify test` | send a test through every channel and sink |
| `ptyro usage [--json]` | rate limits per profile (Claude, Codex) |
| `ptyro integrations ls\|install\|uninstall [gemini\|antigravity] [--profile P]` | the one-time agent integrations that need installing |

## Projects, worktrees, workspaces, profiles

| Command | Does |
|---|---|
| `ptyro project add [PATH] [-w WS]` / `ls` / `show [P]` / `rm P` | register (to the workspace the TUI showed last, unless `--workspace`) / list / inspect / forget repositories. The repository a session runs in registers by itself; `rm` keeps it from doing so again |
| `ptyro project rename P NEW` | rename a project: its slug, the address of its worktrees and sessions (`NEW/name`). Its worktrees' directories stay where they are; new ones go under the new name |
| `ptyro project trust [P] [-y]` / `untrust [P]` | review and trust a project's scripts |
| `ptyro project line-endings [P]` | rewrite the project's bash scripts that have Windows (CRLF) line endings with LF, in every checkout |
| `ptyro project config [P] [--explain] [--json]` | the effective project config, and where each value came from |
| `ptyro project discover [--refresh]` · `pin P PROFILE` · `unpin P` | find repos under `[discover] roots` · pin a project to a profile |
| `ptyro project suggest [P] [--apply\|--decline] [--json]` | what the project needs (a setup script from its lockfile, `.env` files to copy); `--apply` writes it to `.ptyro.local.toml`. A first worktree asks on its own |
| `ptyro wt new [-n NAME] [-p P] [--base REF] [--branch B [--existing]] [--pr N\|URL \| --issue N\|URL \| --take-changes] [-a A [--prompt P]] [--title T] [--profile P] [--no-setup] [--no-fetch] [-d] [--json]` | create a worktree (optionally with an agent, attached to in a terminal unless `-d`); `--take-changes` moves the main checkout's uncommitted work into it |
| `ptyro wt ls [-p P]` / `status [W]` / `log [W]` | list worktrees / repository status / commits since the base |
| `ptyro wt diff [W] [--against base\|upstream\|last-turn\|head] [--stat]` | a worktree's changes |
| `ptyro wt commit [W] -m MSG [--all] [-- FILES…]` / `--ask-agent` | commit as the worktree's profile / ask its agent to |
| `ptyro wt integrate W [--into B] [--cherry-pick] [--push]` | rebase onto main and fast-forward it (or cherry-pick) |
| `ptyro wt pair W [--refresh]` / `unpair W` | show W's work in the main checkout / give the checkout back, your edits carried to W |
| `ptyro wt run W [SCRIPT] [--attach]` / `open [W] [-e EDITOR]` | run a lifecycle script / open in an editor |
| `ptyro wt rm W [--force [-y]] [--stop-sessions] [--keep-branch]` | remove safely: refuses, listing what would be lost |
| `ptyro wt rename W NEW` | rename a worktree: its name, its address `project/NEW`, unique in its project. Its branch and its directory stay, so what runs in it carries on. The main checkout is always `@root` |
| `ptyro wt lock\|unlock W` / `rename-branch W [NEW]` | manual lock / rename the branch (never once pushed) |
| `ptyro wt retitle W [TITLE…]` | change a worktree's title (`wt new --title`, else its prompt, PR or issue) |
| `ptyro wt finish [W] [--integrate]` | commit what's left (its agent asked to; else its title is the message), then a PR, or integrate |
| `ptyro wt archive [W] [--stop-sessions]` / `restore [P/]NAME\|HANDLE` / `archives [-p P] [--json]` | keep its branch and a snapshot of its files, remove the checkout (never loses work) / bring it back as it was / list them, each with the handle that tells apart archives of one name |
| `ptyro wt archive-rm [P/]NAME\|HANDLE… [-y]` | delete archives for good: the saved files, and each branch if merged or pushed (asks unless `-y`); `[worktrees] archive_purge_days` does it on its own |
| `ptyro gc [--dry-run] [--json] [--scratch]` | stale worktrees (gone, merged, unused; locked ones are kept), biggest on disk first; asks for each: archive, remove, keep (lock) or skip. Then scratch shells' directories left behind with files in them: `d` deletes one. `--json` lists the worktrees; `--scratch` only the scratch leftovers (`--scratch --json`: those as JSON) |
| `ptyro ws ls\|new\|set\|rm\|rename\|add-project\|remove-project …` | workspaces: `new NAME [--profile P] [--notify all\|badge\|mute]`; `rm WS` removes a workspace; `add-project WS PROJECT` / `remove-project WS PROJECT` put a project in one or take it out (it stays registered) |
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
| `ptyro pr reply TEXT [--wt W] [--comment ID]` / `open PR` | reply on the PR or a review thread, marked "via ptyro" / back to a PR's session |

## Orchestration

| Command | Does |
|---|---|
| `ptyro fanout (-a A,B,C \| --count N) (--prompt P \| --prompt-file F) [--test [CHECK]] [--verify CHECK] [--judge A] [--timeout D] [--prefix PREFIX] [--base REF] [-p P] [--structured] [--wait] [--json]` | one task, several agents, one worktree each |
| `ptyro compare R [--refresh] [--json]` | rank candidates by evidence: tests, diff, time |
| `ptyro pick R C [--cherry-pick \| --pr \| --apply [--path F]… [--hunk F:N]…] [--into T]` | take a candidate (default: merge) |
| `ptyro discard R [C] [--force]` / `iterate R [C] TEXT` | remove the others / a follow-up prompt |
| `ptyro recipe ls\|show NAME\|validate FILE\|graph R [--format mermaid\|dot]` (each with `[-p P]`) | recipes: list, read, check, draw |
| `ptyro recipe run R [--var k=v]… [-p P] [--base REF] [--trust FP] [--wait] [--json]` | start a recipe |
| `ptyro run ls [--active] [--tree]` / `show R` / `events R` / `wait R [--gate] [--timeout D]` | runs and their steps (`wait`: exit 0 succeeded, 1 failed, 5 cancelled, 7 timeout) |
| `ptyro run cancel R` / `retry R STEP` / `fork R --from STEP [--set k=v]` / `lead SESSION [--max-workers N] [--agent A]…` | control runs; make an existing agent a lead |
| `ptyro gate ls [--all]` / `resolve ID CHOICE [--note N] [--key K]` | what runs wait on you for / answer |
| `ptyro msg send TO TEXT [--kind K] [--interrupt] [--run R] [--reply-to ID] [--key K]` / `check [--me S] [--since ID] [--wait D]` / `ls [--run R] [-l N]` / `ack [IDS] [--me S]` | the mailbox between agents and you (`TO`: `@lead`, `@user`, `@all`, `@idle`, `@<agent>`, a name) |
| `ptyro lease claim PATTERN… [--exclusive] [--ttl D] [--reason TEXT] [--as S]` / `release [PATTERN…] [--as S]` / `ls` | file leases (`claim` exits 5 when refused) |
| `ptyro radar [-p P]` | overlapping worktrees and predicted conflicts (exit 1 when it predicts one) |
| `ptyro checkpoint ls [S\|W]` / `diff S N [M] [--stat]` / `restore S\|W [N] [--turn T] [-y]` | the worktree after every agent turn |
| `ptyro verify SESSION [CHECK] [--max-bounces N]` / `--off` | verified-done for a session |
| `ptyro mcp` | the MCP server a lead agent gets (started for it) |

## Change sets

See [`changesets.md`](changesets.md) for the walkthrough.

| Command | Does |
|---|---|
| `ptyro cs new NAME --projects A,B… [--template T] [--layout lead\|solo\|manual] [--lead AGENT] [--agent M=A]… [--depends M:D+E]… [--base M=REF]… [--prompt GOAL \| --prompt-file F \| --from-issue N] [--gates contract,push,land \| --no-gates] [--integration CMD]… [--overlay] [-w WS] [--trust FP] [--json]` | start a change set (`--trust`: the fingerprint of a hub template you reviewed) |
| `ptyro cs ls` / `show SET` (alias `status`) / `log SET [-f]` | change sets / one set with its issues and actions / its ledger |
| `ptyro cs msg SET TO TEXT [--kind K] [--interrupt]` | a message on the set (`TO`: a member, `@lead`, `@all`) |
| `ptyro cs ready SET [NOTE]` / `blocked SET ON [NOTE]` (both `[--member M]`) / `assign SET M TASK` / `ask SET QUESTION [--option O]…` | from inside the set: readiness, blockers, a member's task, a question for you |
| `ptyro cs contract SET [show [--rev N] \| edit \| diff [FILE] [--a REV] [--b REV] \| history \| submit [-m MSG] \| revise FILE [--diff] [--affects …] [-m MSG] \| approve [--note N] \| reject NOTE]` | the contract and its revisions |
| `ptyro cs diff SET M` / `check SET` / `overlay SET [auto\|go\|rust\|node\|python\|compose\|off] [--member M]` | a member's changes / integration and contract checks / build against siblings |
| `ptyro cs checkpoint SET [LABEL]` / `checkpoints SET` / `restore SET N [--force]` | undo across every repo |
| `ptyro cs commit SET -m MSG` / `push SET [-y]` / `pr SET [--draft]` / `land SET [-y] [--no-wait]` | lockstep git and the merge train |
| `ptyro cs add SET PROJECT [-a A] [--depends-on M]` / `drop SET M [--force]` / `rm SET [--keep M] [--force]` / `templates` | membership, removal, templates |

## Remote and phone

See [`remote.md`](remote.md).

| Command | Does |
|---|---|
| `ptyro serve [--access localhost\|tailscale\|open] [--port N] [--cert PEM --key PEM]` | the web app and the protocol over WebSocket for paired devices, always HTTPS with a certificate ptyro makes (phones trust its CA once, from `/ca.crt`); `--access` and `--port` default to `[remote] access` and `port`; `--cert`/`--key` use your own certificate |
| `ptyro pair [-n NAME] [--scope read\|approve\|control\|full] [--url URL] [--json] [--open]` | pair a phone or browser: a QR code with a one-time code (10 minutes; scope `full` by default, with a warning; a name that exists is replaced), then waits for the device; `--open` pairs this computer's own browser instead (inside WSL, the Windows one; it offers to have Windows trust ptyro's certificate first) |
| `ptyro devices [ls \| revoke ID \| push-test]` | paired devices |
| `ptyro machine [ls \| add (ssh://HOST \| CODE) [-n NAME] \| rm M \| hide M WS \| unhide M WS \| retry M \| pair]` | other machines whose workspaces show here beside yours (`laptop2:work`, `laptop2:fix-flaky-test`): add one over ssh or with the code `machine pair` printed on it; they must run the same version |
| `ptyro web [on\|off\|status] [--access localhost\|tailscale\|open] [--pair] [--json]` | have the daemon run the web app's server (`ptyro daemon start --serve` too): on, off, or where it is and what to do next |
| `ptyro --remote HOST …` / `ptyro proxy` | any command against another machine's daemon / the bridge it runs there; `--remote wsl://[distro]` is the daemon in a WSL distro (what `ptyro.exe` on Windows uses; `client.default_remote` keeps one) |

## Recordings

| Command | Does |
|---|---|
| `ptyro replay SESSION [--speed X] [--max-idle D] [--from D]` | play a session's recording back in your terminal |
| `ptyro export SESSION [--format asciicast\|text\|ptylog] [-o FILE]` | the recording as asciicast v2, plain text, or a test fixture |

## Setup, config and the daemon

| Command | Does |
|---|---|
| `ptyro init [-y] [--dry-run]` | detect agents, accounts and repositories, then write your config |
| `ptyro setup [-y] [--agents A,B] [--tools T,U] [--where wsl\|windows\|both] [--dry-run] [--json]` | the setup checklist: install coding agents (Claude Code, Codex, Antigravity, OpenCode, Gemini CLI) and tools (git, GitHub CLI, Node.js) with what they need first; `-y` asks nothing (the defaults, or what `--agents`/`--tools` name) and reports admin steps it can't do without a prompt; on Windows `--where` picks WSL's distro, Windows, or both (default: the side your daemon runs on) |
| `ptyro install NAME… [-y] [--where wsl\|windows\|both] [--dry-run] [--json]` | install agents or tools by name (`agy`, `gemini gh`), with their prerequisites; on Windows into WSL, onto Windows, or both |
| `ptyro update [NAME…] [--check] [-y] [--json]` | update the agents that have a newer version out (or those named), each the way it was installed (`claude update`, `npm install -g …@latest`), after showing the commands and asking; sessions running an updated agent keep the old version until they restart, and are named. `--check` looks for newer versions now and says what's out of date (`--json`: as JSON). ptyro also looks once a day by itself (`updates.check`; skip one with `updates.ignore`) and offers each at start-up, in the health badge, on the web's Health page and in `ptyro doctor` |
| `ptyro login [NAME…] [--check] [--json] [--where wsl\|windows\|both]` | log in to the agents and the GitHub CLI, one account each: each not logged in yet runs its own sign-in here (a browser page, or a link and a code), confirmed by its own status command; ptyro never sees a credential. `--check` only says where each stands (exit 1 when one is logged out), `--json` as JSON. Setup ends with these logins. More accounts: `ptyro profile login` |
| `ptyro about [--json]` | which ptyro this is: version and protocol, the repository, licence, build, where config, data, logs and the socket live, the daemon and web app, the tools found, and a block to paste into a bug report |
| `ptyro doctor [--quick] [--json]` | diagnose agents, logins, integrations, terminal, tmux, daemon, git |
| `ptyro tour` | the daily loop for real (task, answer, review, finish, clean up) with a pretend agent in a ptyro of its own, deleted afterwards |
| `ptyro config path\|check\|show\|edit [KEY]` | your config; `edit` opens `$EDITOR` (at that setting) and validates |
| `ptyro config list [SECTION] [--changed] [-p P] [--json]` | every setting with its value and where it comes from (default, your config, `.ptyro.toml`, `.ptyro.local.toml`); `SECTION` is `ui`, `remote`… or a key prefix |
| `ptyro config get KEY [--explain] [-p P] [--json]` | one value, bare for scripts; `--explain`: what it does, its default, where it is set, when a change applies |
| `ptyro config set KEY VALUE… [-p P [--shared]]` | change a setting: checked first, the file's comments kept, live where it can be; words make a list or a command (after `--` if one starts with a dash); `-p P` (a project's name or path, `.` for the one you're in) writes your `.ptyro.local.toml`, `--shared` the committed `.ptyro.toml` |
| `ptyro config unset KEY… [-p P [--shared]]` | back to the default |
| `ptyro config add COLLECTION [NAME] [FIELD=VALUE…]` / `rm COLLECTION NAME` / `rename COLLECTION FROM TO` (each with `[-p P [--shared]]`) | an agent, theme, editor, key binding, notification service (`notifications.sinks`, `type=ntfy url=…`), run script or prompt |
| `ptyro config schema [-p P]` | every setting's description as JSON, for tools that build forms |
| `ptyro daemon start [--foreground]` / `status [--json]` / `logs [--lines N] [-f]` | the background daemon (it starts by itself); `status` includes its health |
| `ptyro daemon stop\|restart [--hibernate] [--force]` | stop it; `--hibernate` brings resumable agents back dormant |
| `ptyro daemon upgrade [--binary PATH] [--json]` | switch the running daemon to a new binary without stopping sessions; exits 1 if it rolled back |
| `ptyro daemon install-service\|uninstall-service [--dry-run]` | start the daemon at login (systemd user unit / launchd agent) |
| `ptyro backup [-o FILE] [--with-recordings] [--with-ca] [--json]` | a private `.tar.gz` of ptyro's own state (config, a consistent copy of the database, scratchpad); never replaces a file ([`docs/uninstall.md`](uninstall.md)) |
| `ptyro uninstall [--dry-run] [--yes] [--delete-work] [--keep-config] [--keep-data] [--keep-binary] [--backup FILE] [--purge-refs] [--json]` | take ptyro off this computer: shows what goes and what stays, asks (typing `delete` when worktrees have uncommitted work), stops the daemon, removes worktrees, directories, the login service, integrations, shell lines and the binary installed by hand ([`docs/uninstall.md`](uninstall.md)) |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | ok (`wait`: got there) |
| `1` | error (`doctor`: found a problem; `pr checks`: a check fails; `run wait`: the run failed; `radar`: a predicted conflict; `cs check`: not ok; `daemon upgrade`: rolled back) |
| `2` | invalid arguments |
| `3` | not found |
| `4` | ambiguous: a name, handle or selector matched several (the message lists them) |
| `5` | conflict: unsaved work, a profile question needing `--yes` / `--override-pin`, (`wait`) the session exited first, (`run wait`) the run was cancelled, or (`lease claim`) refused |
| `6` | untrusted scripts |
| `7` | timeout (`wait`, `send --wait-ready`), `--stall` saw no output, or (`pr checks`) checks still running |
| `8` | no daemon |
| `9` | the daemon speaks another major protocol version: run `ptyro daemon restart --hibernate` (within a major version, `ptyro daemon upgrade` switches live) |
