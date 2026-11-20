# The knowledge base

One memory of your work, for you and your agents to search. ptyro keeps it by itself, in one
file on this computer, and you can bound it, look into it and delete from it at any time.

This page is the long form. The short one is in the [README](../README.md); every command is
in [`commands.md`](commands.md#the-knowledge-base), every key in
[`keys.md`](keys.md#the-library-ctrl-q-l), every setting in [`config.md`](config.md).

## What is kept

**Your agent sessions, as they happen.** At the end of each turn ptyro reads what Claude Code
or Codex wrote in its own transcript and keeps, under one entry a conversation:

- what you typed,
- what the agent answered,
- one line for each tool it used (`Bash: cargo test`, `Edit src/feeder.rs`),
- a summary the agent wrote when it compacted the conversation,
- a short **card**: the task, the outcome, the files changed, the agent, the project, when.

There is nothing to switch on and nothing to run. A session that is resumed goes on in the
same entry. An agent with no transcript ptyro can read (OpenCode) gets its card only.

**Your chats are searched beside it.** They live in their own file (`chat.db`) and are not
copied; one search answers from both.

Everything is in `kb.db`, in ptyro's data folder (`ptyro about` names it), readable by you
only. Nothing leaves this computer. Each linked machine keeps its own (see
[Linked machines](#linked-machines)).

## What is never kept

- An agent's **thinking**, and **what a tool printed** (a command's output, a file's
  contents). Only the line that says which tool ran on what.
- **A private session** (below): nothing at all, not even its card.
- **Secrets**: keys, tokens, passwords and private keys are taken out before anything is
  written, and so are the values of your profile's secret settings. `[kb] redact` adds your
  own patterns.
- **Files that hold secrets**: `.env*`, `*.pem`, `*.key`, `.ssh/`, `.aws/`, `.gnupg/` and
  their kind are never read. `[kb] exclude` adds paths.
- **What you forgot** (below), and anything of an agent or a repository you kept out.
- Terminal contents. ptyro's database (`ptyro.db`) holds no text of your work at all.

Not built yet: notes you or an agent write down, folders of documents and code (`ptyro
ingest`), an agent's history from before ptyro kept it, and shell commands. They are planned
(ROADMAP KB2 to KB5); today the knowledge base holds agent sessions.

## Private sessions

A private session leaves nothing in the knowledge base: no transcript is read and no card is
kept. The agent still keeps its own history, as it always does.

- Start one with `ptyro new --private` (also `wt new --agent … --private` and `shells new
  --private`), or tick **Private** in the TUI's form (`C-o` in the task box) or the web's New
  task sheet.
- `[kb] private = true` makes it the default; `--no-private` keeps one session then.
- It is marked `◌` on its row, in its pane's header, in `ptyro ls` and on the web.
- It still has the tools to search with: it can read the memory, it leaves no trace in it.
- **A running session can be made private**: `ptyro kb forget --session NAME`, the TUI's
  `session.private` (the row's menu), the web's ⋯ → Make it private…. Everything kept of it is
  forgotten and nothing more is kept. There is no way back for that session.

## Searching

```sh
ptyro recall retry policy
ptyro recall flaky test kind:session since:30d -p myrepo
ptyro recall "exactly this phrase" agent:codex --words
```

Words must all match; `"a phrase"` matches as it is; `prefix*` needs three letters; `-word`
leaves out. In the query: `kind:session` (or `chat`), `project:NAME`, `since:7d`,
`before:30d`, `agent:codex`, `workspace:work`, `file:PATH`, `wrote:you|agent`,
`machine:NAME`, `by:meaning`, `by:words`. What is of the project you are in ranks higher.

Each hit has an **address**: `k:7f3k/12` is piece 12 of the item `k:7f3k`; a chat's message is
`c:7f3k/…`; another machine's has the machine in front (`laptop2:k:7f3k/12`).

```sh
ptyro kb show k:7f3k/12     # that piece with what was said around it
ptyro kb show k:7f3k        # the whole item
ptyro kb show k:7f3k --open # the session it happened in, resumed when it can be
ptyro kb recent             # the history, by day
```

**Meaning search** is opt-in (`[kb] meaning = true`): a small model is downloaded once (31 MB)
and what is kept is read in the background, on this computer only. Searches then also find
what is *about* your words, marked `≈`. Without it a search says it was by words alone.

## The Library

The same, to look at.

- **TUI**: `Ctrl-Q L` (`L` in the sidebar). The history by day when nothing is typed; `/`
  searches as you type; `Tab` reaches the filters (kind, project, since, and the machine when
  others are linked); `⏎` opens the reader; `r` goes to the session it happened in; `y` / `Y`
  copy; `d` forgets; `s` shows the status.
- **Web**: More → Library, built for a phone. The search box, the same filters as picks, the
  history by day with **Older…** for more; a tap opens the reader (the piece with what was said
  around it, then **All of it**, a box to find in it, **Its session**, and ⋯ for copying and
  Forget…). It follows what is kept as it happens.
- A session's, a worktree's and a project's menu has **History**: the Library with only that
  one's.
- Switched off, both say so and offer to switch it on.

## What your agents get

Every Claude Code and Codex session ptyro starts is given two tools:

- `recall`: search. It answers pointers (an address, what it is, when, a snippet).
- `recall_read`: open an address, with what was said around it.

They come with the session, for that session only (ptyro writes nothing into `~/.claude` or
`~/.codex`), and are there again after a resume. Chats have them too. What they answer is
marked as retrieved history, not instructions, and says who wrote each piece.

**The wall between profiles.** An agent sees only what happened under its own profile (and
what belongs to no profile), never the session it is in, and it can't forget anything. An
agent that runs `ptyro recall` in its terminal is behind the same wall. You see everything.
`[kb] scope` moves the wall: `profile` (the default), `workspace` (narrower), `all`.

`[kb] tools = false` gives no agent the tools; `[agents.codex] kb = false` gives that agent
none and keeps its sessions out.

## The switches

Settings → Knowledge in the TUI and on the web; `[kb]` in the config.

| Setting | What it does |
|---|---|
| `enabled` | off: nothing is read, kept or searched. What was kept stays in the file |
| `sessions` | off: agent sessions are no longer kept |
| `private` | new sessions start private |
| `tools` | off: no agent is given `recall` and `recall_read` |
| `scope` | what an agent's search may see: `profile`, `workspace`, `all` |
| `meaning` | search by meaning too (downloads a model once) |
| `retain_days` | delete agent sessions older than this, once a day; `0` keeps them |
| `redact` | more patterns to take out before anything is kept |
| `exclude` | more paths that are never read |
| `max_file` | the largest transcript or file that is read |
| `[agents.NAME] kb` | off: nothing of this agent's sessions, and no tools for it |
| `[knowledge] enabled` in a repository's `.ptyro.toml` | off: nothing of this project. A repository can keep itself out, never in |

`ptyro kb` says how it stands: on or off, what it holds, its size, how long sessions are kept,
what was forgotten, and what was not kept and why.

## Retention

`retain_days = 90` deletes agent sessions older than 90 days. It acts when you set it, at the
daemon's start and once a day. A session deleted this way may be kept again if it goes on.

## Forgetting

```sh
ptyro kb forget k:7f3k              # one item
ptyro kb forget --query "acme"      # everything a search finds (it lists them, then asks)
ptyro kb forget --session NAME      # a session, which is private from then on
```

It says what will go and asks first. Then the text is really deleted: overwritten in the
file, gone from the search index, and **never kept again** (a forgotten session's later turns
are not read). Your files and the agents' own transcripts are not touched. In the Library it
is `d` (TUI) or ⋯ → Forget… (web; a device paired with `--scope full`).

`ptyro uninstall` lists `kb.db` and removes it with the rest of ptyro's data.

## Linked machines

With other machines linked (`ptyro machine add`), each keeps its own `kb.db`; nothing is
copied between them.

- **A search asks every linked machine that is up**, and the answers are mixed, each hit with
  its machine. `ptyro recall --here` (or `machine:here`) is this computer only; `--machine
  laptop2` is that one only. The Library has a machine filter.
- **A machine that is down** (or runs another version of ptyro) is named under the answer;
  the search still answers.
- **A hit there is read there**: `ptyro kb show laptop2:k:7f3k/12`. `kb recent --machine
  laptop2` is its history, and `kb forget laptop2:k:7f3k` forgets it there.
- **The wall holds across the link.** An agent's `recall` asks other machines only when it
  says so, and each answers with what its own profile of the same name may see. An agent
  whose scope is its workspace is never answered from another machine.
