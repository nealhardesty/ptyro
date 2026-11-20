<img src="logos/ptyro-icon.png" alt="ptyro" width="72" align="right">


# Commands

Every TUI action has a command. `ptyro --help` and `ptyro <command> --help` are always the final
word; this page groups them so you can find the right one.

Conventions:

- `[x]` is optional, `A|B` is a choice, `…` repeats.
- Every command takes `--remote [ssh://][user@]host[:port]` (or `PTYRO_REMOTE`) to act on another
  machine's daemon over ssh. Without it, `client.default_remote` is used if set.
- Listing and query commands take `--json`; the shapes are in [`cli-json.md`](cli-json.md).
- Aliases: `a` (attach), `ls` (list),
  `checkpoints` / `ckpt`, `projects`, `workspace`, `worktree`, `profiles`.
- Durations look like `30s`, `10m`, `2h`.
- Removing asks first, saying what goes and what stays (the TUI and the web ask in the same
  words): `chat rm`, `project rm`, `wt rm`, `ws rm`, `profile rm`, `devices revoke`,
  `machine rm`, `config rm`. `-y`/`--yes` doesn't ask; with no
  terminal to ask and no `--yes`, the command refuses (exit 5) and changes nothing. They act on an
  exact address only: a chat's handle, id or `@last`, never words from its title.
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

A long flag is a switch in every command or takes a value in every command, never one here and
the other there (owner, 2026-11-10; `a_flag_is_a_switch_everywhere_or_takes_a_value_everywhere`).
So `--pr N` is a PR number (`new`, `wt new`); `--keep` keeps a chat; `--chat` reads a transcript
and `chat search --in CHAT` narrows a search; `--timeout D` is how long to wait. An optional value reads the same
everywhere: `--scrollback [N]` is the screen and up to N lines above it in `read` and `wait`
(bare: the scrollback kept in memory; N can reach back into the recording).

## Naming sessions and worktrees

- **`SESSION`** is a name (`brisk-rook`), `project/name`, a short handle from `ptyro ls`
  (`x7k2`), `@last`, or `@focused` (what your TUI shows). Names are unique per project, so when
  two projects both have a `build`, say which: `myrepo/build`.
- **`SESSIONS…`**: where several are allowed you can also use a glob (`'build-*'`) or a
  selector, joined with commas (`ptyro rm @exited`, `ptyro send 'build-*,@idle' …`):
  - `@all`: every session, exited and dormant ones too (so `ptyro rm @all` forgets hibernated
    agents as well, after asking).
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
- **A name you give** (a session, worktree, workspace, profile,
  machine or device) is `a-z 0-9 . _ -`, 1–64 characters. It is kept exactly as you wrote it
  (`ws new my_fix` makes `my_fix`). A bad one is refused, and the error offers the fix:
  `invalid name "My Feature": … try "my-feature"`.
- **Everything else you name** (projects, workspaces, profiles, chats, devices,
  machines) resolves the same way: the exact name (a project's slug), then
  `project/name` where there is one, then the full id, then the end of the id, **at least 4
  characters** (`7f3k`, `c:7f3k`, `r-k3x9`). Case doesn't matter. When several match, ptyro lists
  them and exits 4; say which. A chat also takes a word of its title, as the last resort.

## Sessions

| Command | Does |
|---|---|
| `ptyro` | attach to the most recent session (or start a shell session) |
| `ptyro "TASK"` | the default agent on that task, in a new worktree named after it (here, outside a repository); then attach. Quote it: several words unquoted are an unknown command, with a did-you-mean |
| `ptyro new [-d] [-n NAME] [-C DIR] [--size COLSxROWS] [--json] [-- CMD…]` | start a session; attaches in a terminal unless `-d` (`--json` prints it instead). No `--agent` means your shell, or `CMD` |
| `ptyro new … -a A[@PROFILE] [--prompt TEXT \| --prompt-file F] [--profile P] [--override-pin] [-y]` | start an agent: `claude`, `codex`, `opencode`, or one from `[agents]` |
| `ptyro new --again [--wt W] [-d]` | quick new: another of the agent this worktree last started (with its profile and arguments); the project's `default_agent`, else `[agents] default` (claude), when none has |
| `ptyro new … --wt W \| --new-wt [NAME] [-p P] \| --pr N\|URL \| --issue N\|URL` | where: an existing worktree, a new one, one on a PR's branch, or one for an issue. `-p/--project` picks the new worktree's project, so it needs one of those (it once meant `--prompt`; alone it is refused, never ignored) |
| `ptyro new … --prompt TEXT\|@SNIPPET \| --prompt-file F` · `--title T` | the first prompt, typed once the agent's input box is really there · its title (default: the prompt; the name comes from it too) |
| `ptyro attach [SESSION] [-w WS] [--raw] [--view-only] [--tab\|--pane]` | open the TUI on a session; `--raw`: that session only, no chrome; `--tab`/`--pane`: in a new tab or pane of this terminal instead (tmux, Windows Terminal) |
| `ptyro list` (alias `ls`) `[--all] [--status S] [-p P] [-w WS] [--json]` | sessions: where they run, profile; `--all` adds earlier daemon runs; `--status running` is what is alive, `--status active` what is busy now (working, blocked, or running a command) |
| `ptyro stop SESSIONS…` | stop, keeping them listed: an agent's turn is interrupted and it gets a moment to exit, then the process gets HUP, TERM and KILL |
| `ptyro kill SESSIONS… [-s N]` | kill now (SIGKILL, no chance to clean up), keeping them listed; `-s N` sends signal N instead |
| `ptyro rm SESSIONS… [--stop] [-y]` | forget ended sessions (they leave the list); a dormant agent asks first, its conversation forgotten (`-y` doesn't ask); a running one is refused, and `--stop` stops it first. Each is tried: a refusal is said, the rest go on, and the exit code is the first refusal's |
| `ptyro restart SESSIONS…` | run exited sessions again |
| `ptyro rename SESSION [NAME] [--title T]` | its name, the address you type to find it, and/or its title, what people read (`--title ""` clears it; it then shows what its program titles its window) |
| `ptyro resume SESSIONS…` / `hibernate SESSIONS…` | relaunch dormant agents / stop idle ones, keeping them resumable |
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
| `ptyro chat [new] [PROMPT…] [-a A[@P]] [--folder DIR]… [-w WS] [--title T] [--no-wait] [--json]` | a new chat with your agent (SPEC §22): the answer streams here and the chat is kept (the TUI's and web's CHATS show it). No prompt, at a terminal: the chat view. `-` or a pipe reads the prompt from standard input. A chat searches the web; `--folder` gives it a folder to read |
| `ptyro chat ask [--keep] [--agent A] [--timeout S] QUESTION…` | one question, the answer on standard output; standard input is context (`git diff \| ptyro chat ask "review this"`); nothing is kept unless `--keep` |
| `ptyro chat ls [-w WS] [--machine M] [--pinned] [--archived] [-l N] [--json]` | chats, pinned first then the newest; every linked machine's too (`laptop2:c:7f3k`), greyed while it is offline; `--machine here` for this one's |
| `ptyro chat show CHAT [--ids] [--format text\|md\|json]` / `open CHAT` | print a chat (`--ids`: each message's id) / open it in the TUI's chat view. CHAT: `c:7f3k`, `7f3k`, a title's words, `@last` |
| `ptyro chat send CHAT TEXT… [--no-wait] [--timeout S]` | a message; the answer streams here |
| `ptyro chat answer CHAT allow\|always\|deny` / `stop CHAT` | answer its permission request / stop the answer being written |
| `ptyro chat rename CHAT TITLE… \| --title T \| --auto` / `pin` / `unpin` / `archive` / `unarchive` / `rm CHATS… [-y]` | its title, what people read: a chat's name (it's found by its handle; `--title ""` clears it, `--auto` asks for a new automatic one) / keep at the top / put away / delete for good (by handle, id or `@last`, never a title; asks unless `-y`) |
| `ptyro chat set CHAT [--agent A] [--folder DIR]… [--no-folders]` | switch its agent or the folders it reads; the next message carries the conversation over |
| `ptyro chat task CHAT [-p P] [-a A] [-n NAME] [-y] [-d] [--json]` | make it a task: an agent session in a new worktree whose first prompt is the chat's conversation and last answer, attached to in a terminal (unless `-d`) (not an incognito chat; asks first when the task's profile isn't the chat's, `--yes` agrees) |
| `ptyro chat export CHAT [--format md\|json] [-o FILE]` | write it out |
| `ptyro chat retry CHAT [NODE] [--no-wait]` / `edit CHAT NODE TEXT…` | ask again: a new answer replaces the last one / say your last message differently: it and its answer are replaced. Only the last turn (`show --ids` gives the NODEs); to go back further, send a new message |
| `ptyro chat search QUERY… [--in CHAT] [-w WS] [-a A] [--pinned] [--archived] [--here] [--meaning\|--words] [-l N] [--json]` | search every chat's messages and titles, on every linked machine (`--here`: this one): words, `"a phrase"`, `prefix*` (3 letters), `-word`, `agent:codex`, `since:7d`, `by:meaning`/`by:words`; each hit's citation (`c:7f3k/node`) and snippet. With `[kb] meaning` on, messages near in meaning are merged in (`≈`); `--meaning` only those (off: offers to switch it on), `--words` none of them |
| `ptyro chat find [--agent A] QUESTION…` | ask your chats: an agent with only the history tools searches them and answers, citing; nothing of it is kept |
| `ptyro chat new --incognito …` / `chat ask` | keep nothing: not saved, not searchable, gone with its agent (`ask` is always incognito unless `--keep`) |

## Scripting

| Command | Does |
|---|---|
| `ptyro send SESSIONS… TEXT [--no-enter] [--raw] [--wait-ready[=D]] [--wait [--timeout D] [--stall D]] [--json]` | type into sessions. `--wait-ready` waits for the input box (2m, or D). `--wait` reports how the turn ended. `--raw` sends keys as written (`\e`, `\r`, `\t`, `\xHH`) |
| `ptyro broadcast SESSIONS… TEXT [--no-enter] [--timeout D]` | one prompt to several, each once it's ready |
| `ptyro wait SESSIONS… [--for working\|blocked\|idle\|done\|error\|exit\|any-attention] [--match RE [--scrollback [N]]] [--timeout D] [--any] [--stall D] [--json]` | block until they get there (exit 0), time out (7) or exit first (5). Default `--for idle` |
| `ptyro read SESSION [--scrollback [N]] [--ansi] [--json]` | the screen or its scrollback (beyond memory, from the recording) |
| `ptyro events [-f] [--session S] [-l N] [--json]` | what happened; `--follow` streams every change as NDJSON |
| `ptyro status [SESSION] [--explain] [--json]` | what every agent is doing; `--explain`: why ptyro thinks so |
| `ptyro answer SESSION allow\|always\|deny` | answer a permission prompt without opening the session |
| `ptyro completions bash\|zsh\|fish\|elvish\|powershell` | the completion script (with live names) |

## Attention and notifications

| Command | Does |
|---|---|
| `ptyro mute SESSION\|project:P\|workspace:W [--for D] [--off]` | silence notifications (its row still shows when it wants you) |
| `ptyro notify test` | send a test through every channel and sink |
| `ptyro usage [--json]` | rate limits per profile (Claude, Codex) |

## Projects, worktrees, workspaces, profiles

| Command | Does |
|---|---|
| `ptyro project add [PATH] [-w WS]` / `ls` / `show [P]` / `rm P [--release] [-y]` | register (to the workspace the TUI showed last, unless `--workspace`) / list / inspect / forget repositories. The repository a session runs in registers by itself; `rm` keeps it from doing so again, and its worktrees stay on disk. Sessions running in it block `rm` (at a terminal it offers to let them go); `--release` lets them go on, outside any project |
| `ptyro project rename P NEW` | rename a project: its slug, the address of its worktrees and sessions (`NEW/name`). Its worktrees' directories stay where they are; new ones go under the new name. `NEW` is a slug: `a-z 0-9 -`, at most 40 |
| `ptyro project trust [P] [-y]` / `untrust [P]` | review and trust a project's scripts |
| `ptyro project config [P] [--explain] [--json]` | the effective project config, and where each value came from |
| `ptyro project pin P PROFILE` · `unpin P` | pin a project to a profile |
| `ptyro wt new [-n NAME] [-p P] [--base REF] [--branch B [--existing]] [--pr N\|URL \| --issue N\|URL] [-a A [--prompt P]] [--title T] [--profile P] [--no-setup] [--no-fetch] [-d] [--json]` | create a worktree (optionally with an agent, attached to in a terminal unless `-d`) |
| `ptyro wt ls [-p P]` / `status [W]` | list worktrees / repository status |
| `ptyro wt run W [SCRIPT] [--attach]` / `open [W] [-e EDITOR]` | run a lifecycle script (`setup`, or a run script by name) / open in an editor |
| `ptyro wt rm W [--force] [-y] [--stop-sessions] [--keep-branch]` | remove safely: refuses, listing what would be lost (commit and push it first, or `--force` to discard it). Its directory is deleted, and its branch too if it is merged or the same as its pushed copy, otherwise kept (`--keep-branch`: always kept). Asks unless `-y` (`--force` asks again, after saying what would be lost) |
| `ptyro wt rename W [NEW] [--title T]` | its name, its address `project/NEW`, unique in its project, and/or its title, what people read (`wt new --title`, else its prompt, PR or issue; `--title ""` clears it). Its branch and its directory stay, so what runs in it carries on. The main checkout is always `@root`: only its title changes |
| `ptyro gc [--dry-run] [--json] [--scratch] [--keep-wt WT]…` | stale worktrees (gone, merged or squash-merged, unused; one with sessions running is left alone), biggest on disk first; asks for each: `r` removes it (refusing to lose work, as `wt rm` does), `k` keeps it (not offered again until it is unused for another `worktrees.stale_days`, or its branch moves on), ⏎ not now. `--keep-wt WT` keeps those and asks nothing. Then scratch shells' directories left behind with files in them: `d` deletes one. `--json` lists the worktrees; `--scratch` only the scratch leftovers (`--scratch --json`: those as JSON) |
| `ptyro ws ls\|new\|set\|rm\|rename\|add-project\|remove-project …` | workspaces: `new NAME [--profile P] [--notify all\|badge\|mute]`; `rm WS [-y]`: its sessions keep running and move to main, with its chats and the projects no other workspace has; `add-project WS PROJECT` / `remove-project WS PROJECT` put a project in one or take it out (it stays registered) |
| `ptyro profile ls\|show\|new\|rm\|rename\|env\|login\|sync\|detect\|doctor …` | profiles; `detect [--apply]` finds your existing multi-account setups; edits keep your config's comments |

## Checkpoints

| Command | Does |
|---|---|
| `ptyro checkpoint ls [S\|W]` / `diff S N [M] [--stat]` / `restore S\|W [N] [--turn T] [-y]` | the worktree after every agent turn |
| `ptyro mcp` | the MCP server with the chat history tools a chat's agent gets (started for it) |

## Remote and phone

See [`remote.md`](remote.md).

| Command | Does |
|---|---|
| `ptyro pair [-n NAME] [--scope read\|approve\|control\|full] [--url URL] [--json] [--open]` | pair a phone or browser (turns the web app on first when it is off): a QR code with a one-time code (10 minutes; scope `full` by default, with a warning; a name that exists is replaced), then waits for the device; `--open` pairs this computer's own browser instead (inside WSL, the Windows one; it offers to have Windows trust ptyro's certificate first) |
| `ptyro devices [ls \| revoke ID [-y] \| push-test]` | paired devices |
| `ptyro machine [ls \| add (ssh://HOST \| CODE) [-n NAME] \| rm M [-y] \| hide M WS \| unhide M WS \| retry M \| update [M…] [--all] [-y] \| pair]` | other machines whose workspaces show here beside yours (`laptop2:work`, `laptop2:fix-flaky-test`): add one over ssh or with the code `machine pair` printed on it; they must run the same version, and `update` has those named (none: every one behind this machine's version) update their own ptyro, each checking the release there and upgrading its daemon live, even while its link is down for its version |
| `ptyro web [on\|off\|status] [--access tailscale\|localhost\|open] [--pair] [--json]` | the one switch for the web app: the daemon runs its server (always HTTPS, with a certificate ptyro makes or Tailscale's; phones trust ptyro's CA once, from `/ca.crt`), now and whenever it starts; on, off, or where it is and what to do next. `on` picks Tailscale if it is running here, else this computer only, unless `--access` or `[remote] access` says |
| `ptyro --remote HOST …` / `ptyro proxy` | any command against another machine's daemon / the bridge it runs there (`client.default_remote` keeps one) |

## Setup, config and the daemon

| Command | Does |
|---|---|
| `ptyro setup [-y] [--agents A,B] [--tools T,U] [--dry-run] [--json]` | set ptyro up: the checklist to install coding agents (Claude Code, Codex, OpenCode) and tools (git, GitHub CLI, Node.js) with what they need first, their logins, then (at a terminal) your accounts, preferences and repositories, with only what your config lacks written; `-y` asks nothing (the defaults, or what `--agents`/`--tools` name) and reports admin steps it can't do without a prompt |
| `ptyro install NAME… [-y] [--dry-run] [--json]` | install agents or tools by name (`codex`, `opencode gh`), with their prerequisites |
| `ptyro update [NAME…] [--check] [-y] [--json] [--here]` | update the agents that have a newer version out (or those named), each the way it was installed (`claude update`, `npm install -g …@latest`), after showing the commands and asking; sessions running an updated agent keep the old version until they restart, and are named. `--check` looks for newer versions now and says what's out of date (`--json`: as JSON). ptyro also looks once a day by itself (`updates.check`; skip one with `updates.ignore`) and offers each at start-up, in the health badge, on the web's Health page and in `ptyro doctor` (a new ptyro also once as a desktop notification, `updates.notify`). Updating ptyro upgrades the daemon live, every open TUI restarts into the new ptyro with its view kept, and the machines linked here on an older ptyro are offered too, each updating itself (`--here`: this machine only) |
| `ptyro login [NAME…] [--check] [--json]` | log in to the agents and the GitHub CLI, one account each: each not logged in yet runs its own sign-in here (a browser page, or a link and a code), confirmed by its own status command; ptyro never sees a credential. `--check` only says where each stands (exit 1 when one is logged out), `--json` as JSON. Setup ends with these logins. More accounts: `ptyro profile login` |
| `ptyro about [--json]` | which ptyro this is: version and protocol, the repository, licence, build, where config, data, logs and the socket live, the daemon and web app, the tools found, and a block to paste into a bug report |
| `ptyro doctor [--quick] [--json] [--fix]` | diagnose agents, logins, hooks, terminal, tmux, daemon, git, and settings ptyro no longer reads (retired keys, keys bound to actions it no longer has, a default agent it no longer has), each with its fix. `--fix` makes the fixes that change a config file, each file copied aside first (`config.toml.bak-YYYYMMDD-HHMMSS`, private; the newest five kept), and says where; the health badge and the web's Health page do the same |
| `ptyro config path\|check\|show\|edit [KEY]` | your config; `edit` opens `$EDITOR` (at that setting) and validates |
| `ptyro config list [SECTION] [--changed] [-p P] [--json]` | every setting with its value and where it comes from (default, your config, `.ptyro.toml`, `.ptyro.local.toml`); `SECTION` is `ui`, `remote`… or a key prefix |
| `ptyro config get KEY [--explain] [-p P] [--json]` | one value, bare for scripts; `--explain`: what it does, its default, where it is set, when a change applies |
| `ptyro config set KEY VALUE… [-p P [--shared]]` | change a setting: checked first, the file's comments kept, live where it can be; words make a list or a command (after `--` if one starts with a dash); `-p P` (a project's name or path, `.` for the one you're in) writes your `.ptyro.local.toml`, `--shared` the committed `.ptyro.toml` |
| `ptyro config unset KEY… [-p P [--shared]]` | back to the default |
| `ptyro config add COLLECTION [NAME] [FIELD=VALUE…]` / `rm COLLECTION NAME [-y]` / `rename COLLECTION FROM TO` (each with `[-p P [--shared]]`) | an agent, editor, key binding, notification service (`notifications.sinks`, `type=ntfy url=…`), run script or prompt |
| `ptyro config schema [-p P]` | every setting's description as JSON, for tools that build forms |
| `ptyro daemon start [--foreground]` / `status [--json]` / `logs [--lines N] [-f]` | the background daemon (it starts by itself); `status` includes its health |
| `ptyro daemon stop\|restart [--hibernate] [--force]` | stop it; `--hibernate` brings resumable agents back dormant |
| `ptyro daemon upgrade [--binary PATH] [--json]` | switch the running daemon to a new binary without stopping sessions; exits 1 if it rolled back |
| `ptyro daemon install-service\|uninstall-service [--dry-run]` | start the daemon at login (systemd user unit / launchd agent) |
| `ptyro uninstall [--dry-run] [--yes] [--delete-work] [--keep-config] [--keep-data] [--keep-binary] [--purge-refs] [--json]` | take ptyro off this computer: shows what goes and what stays, asks (typing `delete` when worktrees have uncommitted work), stops the daemon, removes worktrees, directories, the login service, shell lines and the binary installed by hand ([`docs/uninstall.md`](uninstall.md)) |

## Exit codes

| Code | Meaning |
|---|---|
| `0` | ok (`wait`: got there) |
| `1` | error (`doctor`: found a problem; `daemon upgrade`: rolled back) |
| `2` | invalid arguments |
| `3` | not found |
| `4` | ambiguous: a name, handle or selector matched several, or a removal was given words from a title instead of an address (the message lists the candidates) |
| `5` | conflict: unsaved work, a question needing `--yes` (a removal, a profile) / `--override-pin`, or (`wait`) the session exited first |
| `6` | untrusted scripts |
| `7` | timeout (`wait`, `send --wait-ready`), or `--stall` saw no output |
| `8` | no daemon |
| `9` | the daemon speaks another major protocol version: run `ptyro daemon restart --hibernate` (within a major version, `ptyro daemon upgrade` switches live) |
