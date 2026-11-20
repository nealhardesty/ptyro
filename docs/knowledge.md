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

**Notes**, which you and your agents write: see [Notes](#notes). They are files; `kb.db`
holds their index.

**Your projects, and any folder you give it**: see [Projects and folders](#projects-and-folders).
Their documents, the names of their files, the comments in their code and their commit
messages; their code itself only if you ask.

**What your agents kept before ptyro did**, when you ask for it: `ptyro ingest --history`
(see [The agents' own history](#the-agents-own-history)).

**Your chats are searched beside it.** They live in their own file (`chat.db`) and are not
copied; one search answers from both.

Everything else is in `kb.db`, in ptyro's data folder (`ptyro about` names it), readable by you
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

Not built yet: shell commands, and pointers offered at a prompt (ROADMAP KB4, KB5).

## Projects and folders

**Every project ptyro knows is read by itself** (`[kb] projects`, on): when you add a project
it becomes searchable, with nothing to run. Only its own checkout is read, never its
worktrees, so a file is found once and not once per agent. `ptyro ingest PATH` reads any other
folder or file, and keeps reading it:

```sh
ptyro ingest ~/notes                 # read it, and say what was read
ptyro ingest docs --include '*.md' --name handbook
ptyro ingest ~/src/lib --code        # its source code too, found by its words
ptyro kb ls                          # what is read: each with what it holds and how it stands
ptyro kb sync                        # look again now, at everything (or: kb sync handbook)
ptyro kb rm handbook                 # stop reading it and drop what was kept (asks first)
ptyro kb ls --machine laptop2        # a linked machine's (ingest, kb sync and kb rm take it too)
```

In the Library it is the **Sources** tab (below): the same list, with how far a read is as it
happens, and a form for `ingest` that shows every option.

What is made of a file:

| | What | By default |
|---|---|---|
| Its path | the path, and the words in its name (`RateLimiter.ts` is found by `rate limiter`) | always |
| A document | `.md .mdx .markdown .txt .rst .adoc .org`, `README`, `CHANGELOG`, `NOTES`, `TODO`: its text, each piece under its heading | on |
| Its comments | a source file's comments, each with the line of code it stands on. Licence headers, tool directives (`eslint-disable`), switched-off code and trailing comments are left out; a comment that several files repeat is kept once | on (`[kb] comments`, `--no-comments`) |
| Its code | the source itself, in pieces of 60 lines, found by its words only | **off** (`[kb] code`, `--code`): it puts a copy of your code in `kb.db` |

And of a repository, **its commits** (`[kb] commits`, `--no-commits`): the message and the
names of the files each changed, the newest 5,000 of its main branch at first, then the new
ones.

**Which files.** In a git repository, what git tracks and what it would add: nothing that
`.gitignore` ignores. Elsewhere, everything but hidden folders and `node_modules`, `target`,
`dist`, `build`, `vendor`, `.venv`. Then `--include` and `--exclude` (globs like `docs/**` or
`*.md`). Never a file of keys (`.env*`, `*.pem`, `.ssh/`…; `[kb] exclude` adds to the list),
never one that isn't text, never one larger than `[kb] max_file` (1 MiB), never anything in
ptyro's own data folder. A root takes 50,000 files; one with more says so, and `--include`
narrows it. `ptyro kb` lists what was not kept, and why.

**It stays current.** A root is looked at again every hour (`[kb] sync_every`), when the
daemon starts, when an agent's turn ends in its project (so a document an agent just wrote is
found), and on `ptyro kb sync`. Only files whose time or size changed are read. A file that is
gone, or renamed, leaves. A folder that is gone is `missing`: what was read of it stays until
it is back or you remove it. `--once` reads now and never again.

**Whose it is.** A folder belongs to every profile unless you say `--profile work`; a project
to the profile it is pinned to, else to every profile. An agent of another profile never
finds it.

**A repository's own say**, in its `.ptyro.toml`:

```toml
[knowledge]
enabled = false          # nothing of this project
comments = false         # not its comments
commits = false          # not its commits
code = true              # its code too (the one thing a repository may switch on)
include = ["docs/**"]    # only these
exclude = ["fixtures/**"]
```

`ptyro kb rm` drops a folder's root and everything read of it; your files aren't touched, and
ingesting it again brings it back. A project's root goes when the project is removed from
ptyro, when its file keeps it out, or when `[kb] projects` is off.

**A folder that is gone** keeps what was read of it (it may come back) and is named until you
say what to do: in `ptyro kb`, `ptyro kb ls`, the Sources tab, `ptyro gc`, and `ptyro doctor`,
which is what the TUI's mend list (`M`) and the web's Health page show. `ptyro kb rm NAME`
drops it; `ptyro kb sync NAME` looks again once it is back.

## The agents' own history

Claude Code and Codex keep their own transcripts. `ptyro ingest --history` reads the ones
ptyro never saw: sessions from before you installed it, and agents you started by hand in a
shell.

```sh
ptyro ingest --history --dry-run     # how many there are, and read nothing
ptyro ingest --history               # read them (it says how many first)
ptyro ingest --history --agent codex --profile work --since 30d
```

It looks in each profile's Claude Code folder (`projects/`) and Codex folder (`sessions/`,
`archived_sessions/`). A conversation belongs to the profile whose folder held it, and to the
project its folder was in, if ptyro knows that project. It is read as a live session is: what
was typed, what was answered, a line a tool. It is read behind everything else, so a session's
own turn never waits for it.

Never read: a conversation that is kept already; one you forgot; **a private session's** (it
is marked as never to be kept as soon as ptyro knows its conversation); a chat's (its text is
in your chats); one older than `retain_days` or `--since`; an agent's with `kb = false`; one
that ran in a project that keeps itself out. Sub-agents' own files are not read.

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
ptyro kb show k:7f3k --open # its source: the session it happened in (resumed when it can be),
                            # a note or a file in your editor (a file at the piece's line),
                            # a commit as `git show`
ptyro kb recent             # the history, by day
```

**Meaning search** is opt-in (`[kb] meaning = true`): a small model is downloaded once (31 MB)
and what is kept is read in the background, on this computer only. Searches then also find
what is *about* your words, marked `≈`. Without it a search says it was by words alone.

## Notes

A note is a small Markdown file about one thing: a decision and its reason, a fact about a
project, how something is done, a gotcha. Notes are the only part of the knowledge base that
someone *writes*, so a search lists them first.

```sh
ptyro kb note "We chose SQLite because there is no server to run" --title Storage --tag decision
ptyro kb note                        # no text: your editor opens
ptyro kb note --project myrepo -     # about a project, the text from stdin
ptyro kb notes                       # the notes, by project
ptyro kb show k:7f3k --open          # edit one in your editor
ptyro kb forget k:7f3k               # delete one (its file goes)
```

- **They are files, and the files are the truth**:
  `<data>/kb/notes/<profile>/<project>/<name>.md` (`ptyro about` and `ptyro kb` name the
  folder). The folder says whose a note is and what it is about: `_all` is every profile's,
  `_global` is about no project. Edit, add, rename or delete them with anything: ptyro watches
  the folder and finds the change in a moment (`ptyro kb sync` reads it again at once).
- **A note may start with a header** between `---` lines, which ptyro writes when it writes
  the note: `by` (`you`, or the agent), `session`, `tags`, `created`, `replaces`. A file with
  no header is as good as any other: it is yours. The title is the first heading, else the
  first line.
- **Your agents write them with `remember`**, a third tool beside `recall` (below). An agent
  told "remember that we chose X because Y" writes a note; the next session's `recall` finds
  it first. Who wrote it is ptyro's word, never the agent's: the note is stamped with the
  agent and its session, lands under that session's profile (and its project, unless it says
  otherwise), and is redacted before it becomes a file.
- **What an agent can't do**: write more than 8 KiB in a note, write more than 20 notes in a
  session in a day, edit or delete a note (it writes a new one that *replaces* the old, which
  is moved to `_old/` beside it and out of search), or see another profile's notes.
  `[kb] remember = false` takes the tool away and leaves the reading ones.
- **Whose agents see a note you write**: every profile's, unless you say `--profile work`
  (the forms have the pick). You see every note.
- **A private session may still `remember`**: a note is a deliberate act. The note then says
  "a private session" and never which.
- **In the Library**: the Notes tab (below).
- **Uninstalling ptyro leaves the notes folder where it is** (`ptyro uninstall --delete-notes`
  removes it too): notes are your writing.

## The Library

The same, to look at.

- **TUI**: `Ctrl-Q L` (`L` in the sidebar). The history by day when nothing is typed; `/`
  searches as you type; `Tab` reaches the filters (kind, project, since, and the machine when
  others are linked, and who wrote it); `⏎` opens the reader (a note or a file: your editor,
  a file at the piece's line; a commit: `git show`); `r` goes
  to the session it happened in; `y` / `Y` copy; `d` forgets; `s` shows the status. `2` is
  the **Notes** tab: the notes by project, `n` writes one (a form with every option), `⏎`
  edits one in your editor, `x` deletes it; `1` is the search again. `3` is the **Sources**
  tab: what is read, each with what it holds, how it stands and how far a read of it is; `i`
  reads a folder (a form with every option of `ptyro ingest`), `s` looks at one again and
  `S` at all, `x` stops reading one (it asks first), `b` reads your agents' own history
  (it counts first), `t` switches the keeping of agent sessions.
- **Web**: More → Library, built for a phone. The search box, the same filters as picks, the
  history by day with **Older…** for more; a tap opens the reader (the piece with what was said
  around it, then **All of it**, a box to find in it, **Its session**, and ⋯ for copying and
  Forget…). It follows what is kept as it happens. **Notes** is its second tab: the notes by
  project, **New note**, a tap to edit one in the page (Save), ⋯ → Delete…. **Sources** is its
  third: what is read and how far a read is, **Read a folder…** (every option), **Look at all
  again**, **Agents' history…**, and a tap on a row for all of it, **Look at it again** and
  **Stop reading it…**. A file's and a commit's hit show what was kept of them: a phone has
  no editor to open them in.
- A session's, a worktree's and a project's menu has **History**: the Library with only that
  one's.
- Switched off, both say so and offer to switch it on.

## What your agents get

Every Claude Code and Codex session ptyro starts is given three tools:

- `recall`: search. It answers pointers (an address, what it is, when, a snippet).
- `recall_read`: open an address, with what was said around it.
- `remember`: write a [note](#notes) for later sessions.

They come with the session, for that session only (ptyro writes nothing into `~/.claude` or
`~/.codex`), and are there again after a resume. Chats have them too. What they answer is
marked as retrieved history, not instructions, and says who wrote each piece.

**The wall between profiles.** An agent sees only what happened under its own profile (and
what belongs to no profile), never the session it is in, and it can't forget anything. An
agent that runs `ptyro recall` in its terminal is behind the same wall. You see everything.
`[kb] scope` moves the wall: `profile` (the default), `workspace` (narrower), `all`.

`[kb] tools = false` gives no agent the tools; `[kb] remember = false` gives them the two that
read and not the one that writes; `[agents.codex] kb = false` gives that agent none and keeps
its sessions out.

## The switches

Settings → Knowledge in the TUI and on the web; `[kb]` in the config.

| Setting | What it does |
|---|---|
| `enabled` | off: nothing is read, kept or searched. What was kept stays in the file |
| `sessions` | off: agent sessions are no longer kept |
| `private` | new sessions start private |
| `tools` | off: no agent is given `recall`, `recall_read` and `remember` |
| `remember` | off: agents don't write notes (you still do) |
| `scope` | what an agent's search may see: `profile`, `workspace`, `all` |
| `meaning` | search by meaning too (downloads a model once) |
| `retain_days` | delete agent sessions older than this, once a day; `0` keeps them |
| `redact` | more patterns to take out before anything is kept |
| `exclude` | more paths that are never read |
| `max_file` | the largest file that is read |
| `projects` | off: projects are not read by themselves (only what `ptyro ingest` was given) |
| `comments` | off: source comments are not read |
| `code` | on: source code is read too, found by its words. It makes `kb.db` several times larger |
| `commits` | off: commit messages are not read |
| `sync_every` | how often every project and folder is looked at again (`1h`) |
| `[agents.NAME] kb` | off: nothing of this agent's sessions, and no tools for it |
| `[knowledge] enabled` in a repository's `.ptyro.toml` | off: nothing of this project. A repository can keep itself out, never in |
| `[knowledge] comments`, `commits`, `include`, `exclude` there | narrow what is read of this project; `code = true` reads its code too |

`ptyro kb` says how it stands: on or off, what it holds, its size, how long sessions are kept,
what was forgotten, and what was not kept and why.

## Retention

Files, commits and notes are never aged out: they are current by being read again.

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

**Forgetting a note deletes its file** (the question says so), and that is all of it: a note
you write later under the same name is a new note.

`ptyro uninstall` lists `kb.db` and removes it with the rest of ptyro's data; it leaves your
notes folder in place unless you say `--delete-notes`.

## Linked machines

With other machines linked (`ptyro machine add`), each keeps its own `kb.db`; nothing is
copied between them.

- **A search asks every linked machine that is up**, and the answers are mixed, each hit with
  its machine. `ptyro recall --here` (or `machine:here`) is this computer only; `--machine
  laptop2` is that one only. The Library has a machine filter.
- **What a machine reads is its own.** `ptyro ingest PATH --machine laptop2` reads a folder
  on laptop2 into laptop2's knowledge base (the path is laptop2's); `kb ls`, `kb sync` and
  `kb rm` take `--machine` too, and so does `ingest --history`. In the Library's Sources tab
  the machine filter says whose are listed, and the forms have the pick.
- **A machine that is down** (or runs another version of ptyro) is named under the answer;
  the search still answers.
- **A hit there is read there**: `ptyro kb show laptop2:k:7f3k/12`. `kb recent --machine
  laptop2` is its history, and `kb forget laptop2:k:7f3k` forgets it there.
- **The wall holds across the link.** An agent's `recall` asks other machines only when it
  says so, and each answers with what its own profile of the same name may see. An agent
  whose scope is its workspace is never answered from another machine.
