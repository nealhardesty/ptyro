# ptyro usability review (end-user lens)

Date: 2026-11-11. Checkout read: `the review checkout` (paths below are relative to it).
Review and brainstorm only; nothing was changed, built or run except `--help` on the prebuilt binary.

## How this was done, and what it does not cover

- **Read by me:** `README.md` (all), `docs/commands.md` (all), `docs/keys.md` (lines 1-165), `docs/remote.md` (1-320), `docs/uninstall.md` (all), `SPEC.md` §1.3 and §2.1-2.2, `ROADMAP.md` headings, open items and backlog, `crates/ptyro/src/main.rs` (the clap tree: lines 40-370, 640-1120, 1380-1500), parts of `cli.rs`, `init.rs`, `doctor.rs`, `setup.rs`, `crates/ptyro-client/src/lib.rs` (connect, handshake), `crates/ptyro-daemon/src/conn.rs` (handshake, bad frames), `crates/ptyro-core/src/settings.rs` (the whole schema table, lines 1-905), `crates/ptyro-core/src/lookup.rs` (messages), `web/src/conn.ts`, and the key tables in `crates/ptyro-tui/src/keymap.rs`.
- **Help text run:** `ptyro --help`, `-h`, and `--help` of every top-level command and several subcommands, from `target/debug/ptyro` with `PTYRO_SOCKET` unset. I also ran `ptyro stauts`, `ptyro wt lst` and `ptyro completions klingon` to see the error text; all three fail at argument handling.
- **Web (W- findings):** from a delegated code read of `web/src/*.ts` and `web/dist/{index.html,style.css,manifest.webmanifest}`. I re-read the cited lines for W-01, W-02, W-06, W-07, W-09, W-12, W-14, W-15 and W-17 myself; the rest are the delegate's reading and are marked so.
- **TUI:** see the note at the top of the TUI section for what was and was not read.
- **Not covered:** nothing was executed, so there are no measured sizes, timings or real-terminal behaviour. I did not read `docs/config.md` beyond its headings and starter-file comments, `docs/cli-json.md`, `docs/scripting.md`, `docs/changesets.md`, `docs/parity.md`, `RESEARCH.md`, SPEC §11 in full, the Windows paths (`winclient.rs`, `winapp.rs`), `uninstall.rs`, `serve.rs` or the chat CLI code. Update and uninstall flows are judged from their docs and help text only.

Severity is about how much an end user is hurt; confidence is about how sure I am the evidence means what I say.

---

## The five things that are genuinely good

1. **Errors are built to carry their fix.** A refusal has a structured `fix` with the flag that gets past it (`crates/ptyro/src/cli.rs:898-910`, "again with `--force`: …"). Lookup failures list what exists ("no device matches "phone"; the devices are laptop and tablet", `crates/ptyro-core/src/lookup.rs:112,253`). Setup prints a `fix:` line under every failed step (`crates/ptyro/src/setup.rs:1052-1061`). Bad names offer the corrected one (`docs/commands.md:87-88`).
2. **"Never lose work" is real and consistent.** `wt rm` refuses and lists what would be lost, archiving is offered instead, uninstall makes you type `delete` when worktrees are dirty and `--yes` does not stand in for it (`docs/uninstall.md:83-88`), and every removal asks in the same words in all three clients (`docs/commands.md:19-23`).
3. **Discoverability machinery exists in the TUI.** A which-key menu after a 400 ms pause, `?` for every binding as configured, `.` for a row's menu, a palette, a status-bar hint for the next likely key, and `ptyro tour` with a pretend agent in a throwaway home (`docs/keys.md:24-30`, `README.md:406-410`).
4. **The settings schema is one source for CLI, TUI, web and docs,** with plain-language help, search synonyms (`also`), an "applies at once / to new sessions / needs a restart" label, and a test that fails when a config field has no entry (`crates/ptyro-core/src/settings.rs:1-13, 47-53`). Most help lines read well ("Let ptyro take the mouse: click to focus, wheel to scroll, drag borders. Off leaves it to your terminal…").
5. **CLI conventions are enforced, not hoped for.** One meaning per short flag with a test behind it (`docs/commands.md:32-57`), `--json` on every list command I probed (31 of 31), documented exit codes (`docs/commands.md:309-322`), dynamic shell completion of live names, and a parity check that fails the build when a command has no TUI or web path.

Also worth saying: the web client's reconnect logic is careful (dead-socket detection on wake, `web/src/conn.ts:101-112, 153-156`), its phone terminal input is unusually thorough, and the README's Status section is honest about what has not been tried on real hardware.

---

## The first 10 minutes (a newcomer's friction, in order)

1. **Minute 0, the README.** I want "install, run, see an agent". The page gives a pitch, then Requirements whose second bullet is "Rust 1.99 or later to build" (`README.md:55`) although the prebuilt installer needs no Rust. Install leads with `git clone … make install` (`README.md:76-79`); the one-line curl installer is second.
2. **Minute 1.** After install the README says "Then let ptyro look around" and lists three commands, `ptyro setup`, `ptyro init`, `ptyro doctor` (`README.md:126-130`). I run them. `init` asks "prefix keys, comma-separated (C-q, C-], C-Space)" and "where worktrees go (data, sibling, in-repo)" (`crates/ptyro/src/init.rs:181-190`). I don't know what a prefix or a worktree location is yet, so I press Enter and hope.
3. **Minute 3.** I scroll for the quick start. It begins at line 387 of 1,101, after 250 lines on setup internals, update internals and Windows. It then says the first `ptyro` shows the same checklist anyway (`README.md:396-398`), so two of the three commands I just ran were optional.
4. **Minute 4.** `ptyro` opens "a shell session to start with" (`README.md:393`). I wanted an agent. I have to read on to step 3 to learn `Ctrl-Q n`.
5. **Minute 5.** Step 1's prose introduces prefix, project, worktree, home, the health badge and `Ctrl-Q u` (undo the auto-registration) in three paragraphs. `u` turns out to mean "do what the last message offers", which is a different thing each time (`docs/keys.md:72`).
6. **Minute 6.** I try a guess at the CLI: `ptyro sessions list`. It is not an error. Two or more unknown words are a task, so ptyro creates a worktree and starts my default agent with the prompt "sessions list" (U-11).
7. **Minute 7.** `ptyro --help` prints 77 commands in one unsorted list, several with 300-character descriptions (U-09, U-10). `-h` is no shorter.
8. **Minute 8.** Step 4 of the quick start is one 80-line bullet list that covers SHELLS, tmux-style panes, Mac Option-key settings, the quick terminal, "open in…" and split rules (`README.md:462-516`). None of it is needed for a first task.
9. **Minute 9.** I want it on my phone. `ptyro web on` picks "every network" because Tailscale isn't running (U-22), `ptyro pair` gives the phone full control by default, and the phone then needs a root CA installed through five iOS settings screens (U-23).
10. **Minute 10.** I still haven't met: workspace, profile, run, recipe, gate, change set, lease, radar, checkpoint, chat preset, device scope, machine. There is no glossary outside SPEC §2.1 (U-06).

What works in those ten minutes: the bare `ptyro "fix the flaky test"` form is a good idea, the tour exists, setup installs agents for me, and nothing I did could lose work.

---

## Findings

### A. First run and documentation

**U-01 (high) README is a reference manual in the place of a front door.**
Evidence: 1,101 lines, 12,594 words. Quick start is at `README.md:387`; before it come 100 lines on `ptyro setup` (136-237), 57 on `ptyro update` (239-294) and 84 on Windows (303-385). Requirements lead with the Rust toolchain (`README.md:55-60`).
Hurt: a newcomer cannot find the three lines they need; the project's own rule is "out of the box comes first".
Direction: README = pitch, one install line, a five-step quick start, a screenshot, links. Move setup/update/Windows/feature detail to `docs/`. Put the prebuilt installer first and mark Rust as "only to build from source".
Confidence: high. Not on ROADMAP that I found.

**U-02 (med) Four overlapping ways to get set up, and the README sends you through all of them.**
Evidence: `ptyro setup`, `ptyro init`, `ptyro doctor`, `ptyro install`, `ptyro login`, `ptyro integrations install`, plus the first-run screen of plain `ptyro`. `README.md:126-130` lists three to run; `README.md:396-398` says the first `ptyro` does the same; `init` itself offers setup (`crates/ptyro/src/init.rs:133`).
Hurt: unclear which is required, in what order, and whether running one makes another redundant.
Direction: one sentence in the README: "run `ptyro`; it sets itself up". Present the others as "run again later" tools. Consider folding `init` into `setup`.
Confidence: high.

**U-03 (med) `ptyro init` asks questions a new user cannot answer.**
Evidence: `crates/ptyro/src/init.rs:181-190`: "prefix keys, comma-separated (C-q, C-], C-Space)", "where worktrees go (data, sibling, in-repo)".
Hurt: jargon before the product has been seen; a wrong guess at worktree location is awkward to undo later.
Direction: don't ask; use the defaults and say where to change them. If asked, explain each choice in a line (the settings schema already has the words: `settings.rs:584-588`).
Confidence: high.

**U-04 (med) The first thing a new user sees is a shell, not an agent.**
Evidence: `README.md:393` "open the TUI (a shell session to start with)"; `main.rs:51` "Run with no arguments to attach to the most recent session (or start a shell session)".
Hurt: the product is "a home for your coding agents"; the first screen doesn't show one, and the key to start one (`Ctrl-Q n`) is two steps further down.
Direction: on a first run inside a repository, land on the new-task box ("What should Claude do here?") with the shell one key away.
Confidence: medium (the TUI's first-run screen may already lead there; see the TUI section).

**U-05 (med) Gemini and Antigravity need a manual one-time step that nothing in setup appears to do.**
Evidence: `README.md:593-594` "Gemini and Antigravity need a one-time `ptyro integrations install`". A grep for `integrations` in `crates/ptyro/src/setup.rs`, `init.rs` and `crates/ptyro-tui/src/onboard.rs` finds nothing; doctor's self-test covers only claude, codex and opencode (`crates/ptyro/src/doctor.rs:945`).
Hurt: a user who ticks Gemini in the checklist gets an agent with degraded status and no hint why.
Direction: have setup install the integration when it installs the agent (it already asks before changing anything), or make the health badge offer it.
Confidence: medium-low: the health badge may flag it through a path I didn't read.

**U-06 (high) Concept load is very high and there is no user-facing glossary.**
Evidence: SPEC §2.1 (`SPEC.md:125-144`) is the only glossary and defines 17 terms. The README and help use at least these nouns: session, shell session, SHELLS shell, quick terminal, pane, window, drawer, workspace, project, worktree, `@root`, profile, agent, conversation, chat, side chat, preset, run, recipe, fan-out, candidate, lead, worker, gate, change set, member, contract, ledger, overlay, merge train, checkpoint, archive, lease, radar, mailbox, inbox, attention, device, scope, machine, scratchpad, dormant/hibernated, structured session, verified-done, handle, selector, slot, run script, lifecycle script, prompt snippet, sink, channel, health badge. That is 50+.
Hurt: every screen and help line assumes words the user hasn't been taught.
Direction: a one-page `docs/concepts.md` with a picture (workspace > project > worktree > session) and three tiers: "day one" (session, worktree, project, agent), "when you have two accounts" (profile, workspace), "when you orchestrate" (the rest). Link it from `--help` and the TUI's `?`.
Confidence: high.

**U-07 (med) Several things have two or three names, and several names mean two or three things.**
Evidence:
- One thing, many names: scratchpad / `pad` / "Notes view" on the web (`README.md:743-758`); `doctor` / health badge / "mend" / Health page (`docs/keys.md:82`); "Settings" / "Configuration" / `config` (`README.md:469` "Configuration → Agents → Default agent" against `README.md:878` "Settings (`C-q C`)"; `docs/keys.md:108` "Configuration → Shells"); checkpoints are "save points" and leases "file locks" in the settings blurb (`settings.rs:312`).
- One name, many things: **pair** is a phone (`ptyro pair`), a machine (`ptyro machine pair`) and a worktree mode (`ptyro wt pair`); **remote** is the ssh daemon (`--remote`, `client.default_remote`) and the web app (`[remote] serve`, `remote.access`); **shell** is a shell session, `ptyro shell` (the quick terminal), `ptyro shells` (SHELLS) and `shell-init`; **checkpoint** is per-session, per-change-set, and close kin to an archive's snapshot; **status** is `ptyro status`, `wt status`, `cs status`, `pr status`, `daemon status`.
Hurt: search and memory both fail; "pair my laptop" has two plausible commands.
Direction: pick one user-facing word per thing and use it in all three clients; rename the collisions (`wt pair` → "mirror" or "preview", `[remote]` → `[web]`).
Confidence: high.

**U-08 (low) Internal ids leak into user-facing text.**
Evidence: `ptyro --help`: "A task for your default agent… (UX1.9)" (`main.rs:65`); `ptyro wt --help`: "refusing if it would lose work (SPEC §10.8)" (`projects.rs:289`); `docs/commands.md:119,123,137` cite SPEC §23, §23.17, §22; `README.md:54,69` cite "ROADMAP WIN, track D" and "SPEC §24.1".
Hurt: noise, and it reads as unfinished.
Direction: strip them from help and README; keep them in code comments.
Confidence: high.

**U-09 (med) Reference docs are tables whose cells are paragraphs.**
Evidence: `docs/keys.md:57` (the `g` row) is one cell of about 100 words listing 12 sub-keys; `docs/commands.md:253` is a single synopsis with 14 optional groups; `README.md:472-487` is one bullet of 16 lines.
Hurt: unscannable; the answer is there but can't be found.
Direction: one line per key or command in the table, details in a section below it. Generate the tables from the keymap and clap tree, which already hold the one-liners.
Confidence: high.

**U-10 (low) Small contradictions between docs, help and code.**
Evidence:
- `ui.shell_style` is described three ways: "How the shell key (`!`) shows a worktree's terminal" (`settings.rs:705`), "`!` is not it: that starts another shell" (`crates/ptyro-tui/src/quick.rs:4`), "what ⏎ on the quick terminal's line shows" (`docs/config.md:46`).
- `remote.access`: "localhost (the default)" (`docs/remote.md:103`) against `ptyro web on`, which writes `open` when Tailscale isn't running (`crates/ptyro-core/src/config.rs:936-940`).
- Protocol mismatch: the client says "Restart it with `ptyro daemon stop` (sessions will end)" (`crates/ptyro-client/src/lib.rs:288`); the docs say "run `ptyro daemon restart --hibernate`" (`docs/commands.md:322`).
- `ptyro backup` has `--no-chats` (`main.rs:968-970`); `docs/commands.md:306` and the "In it" table in `docs/uninstall.md:20-25` mention neither the flag nor `chat.db`.
- `docs/remote.md:285-287` describes the session page as "a reply box… keys (Esc, Tab, arrows, ⏎, ^C)" and "your phone never resizes it", while lines 296-305 describe a full terminal with a "Resize the session to this screen" action.
Direction: fix each; generate what can be generated.
Confidence: high for each quote.

### B. CLI

**U-11 (high) Unknown multi-word commands silently start an agent in a new worktree.**
Evidence: the root command takes a positional `TASK` (`main.rs:65-67`). `start_task` rejects only a single word (`cli.rs:968-973`); anything with whitespace becomes a prompt, and in a repository that means a new worktree, a new branch and an agent session (`cli.rs:975-994`). The test `words_alone_are_a_task_and_commands_stay_commands` confirms `ptyro fix it now` parses as a task (`main.rs:1479-1482`). So `ptyro sessions list`, `ptyro session new`, `ptyro agents ls` or `ptyro worktrees ls` all spend tokens on a nonsense prompt.
A second cost: for one mistyped word the user gets ptyro's own message, not clap's suggestion. I ran both: `ptyro stauts` → "unknown command "stauts" (`ptyro --help` lists them; …)"; `ptyro wt lst` → "tip: a similar subcommand exists: 'ls'".
Hurt: a guess at the CLI costs money and leaves a worktree to clean up.
Direction: treat it as a task only when quoted as one argument, or when the first word is not within edit distance of a command or common noun; otherwise ask "start an agent on "sessions list"? [y/N]" on a terminal. Add did-you-mean to the single-word error.
Confidence: high.

**U-12 (high) 77 top-level commands in one flat, unsorted help list; 252 leaf commands.**
Evidence: counted from `ptyro --help` (77) and each group's `--help` (chat 28, cs 24, wt 22, project 13, config 12, profile 10, shells 10…). No `help_heading` or grouping in `main.rs` (grep finds none). Plumbing sits beside daily commands: `mcp`, `proxy`, `report`, `shell-init`, `lease`, `msg`. `docs/commands.md` groups them into ten sections, but `--help` does not. SPEC §1.3 states "Small surface, deep quality. Fewer features, each one polished, before breadth" (`SPEC.md:119`).
Hurt: the first `--help` is a wall; a user cannot tell the eight commands they need from the sixty-nine they don't.
Direction: group `--help` under the headings `docs/commands.md` already uses; hide agent-only plumbing (`mcp`, `proxy`, `report`, `lease`, `msg`) behind `ptyro help --all`; show a ten-line "common commands" help by default.
Confidence: high.

**U-13 (med) `-h` is as long as `--help`, with lines up to 324 characters.**
Evidence: `ptyro -h` is 88 non-empty lines with a longest line of 324 characters (measured). Doc comments are single paragraphs, so clap's short help has no shorter summary: e.g. `shells`, `chat`, `install`, `uninstall` (`ptyro --help` output). The global `--remote` flag carries a 300-character description that is repeated in every subcommand's help, in the middle of that command's own options (`main.rs:58-62`; seen in `ptyro new -h`, `list -h`, `attach -h`, `wt -h`).
Hurt: help wraps into unreadable blocks on an 80-column terminal.
Direction: first line of each doc comment at most 60 characters, the rest after a blank line; move `--remote` under a "Global options" heading with a one-line description.
Confidence: high.

**U-14 (med) Singular and plural forms are sometimes aliases and sometimes different commands.**
Evidence: `runs` = `run`, `recipes` = `recipe`, `gates` = `gate`, `checkpoints` = `checkpoint` (`main.rs:369,375,481,505`); but `shell` (quick terminal) ≠ `shells` (SHELLS) (`main.rs` help text), `cs checkpoint` (make one) ≠ `cs checkpoints` (list) (`docs/commands.md:259`), `wt archive` (do it) ≠ `wt archives` (list) (`docs/commands.md:208`).
Hurt: `ptyro shell new` and `ptyro shells new` do different things; muscle memory from one group is wrong in another.
Direction: one rule. Either plural is always an alias and lists are `ls`, or never. Rename `ptyro shell` to something that isn't one letter from `shells` (`ptyro term`, `ptyro quick`).
Confidence: high.

**U-15 (med) Several ways to do one thing, and one family split across the top level.**
Evidence: the web server is `ptyro serve`, `ptyro web on`, `ptyro daemon start --serve` and `[remote] serve = true` (`docs/remote.md:176-177`); `ptyro watch-pr` and `ptyro pr watch` (`docs/commands.md:224`); `rename` and `retitle` for sessions, worktrees and chats; fan-out is `fanout`, `compare`, `pick`, `discard`, `iterate` at the top level while `chat compare` and `chat pick` mean something else (`docs/commands.md:154-155, 231-234`).
Hurt: more surface to learn; `ptyro pick` reads as general but is fan-out only.
Direction: `ptyro fanout compare|pick|discard|iterate` with the old names as hidden aliases; drop `watch-pr`; document `serve` as the developer form only.
Confidence: high.

**U-16 (low) On/off is spelled six ways.**
Evidence: `mute … --off`, `watch-pr --off`, `verify --off`, `shells watch --off`, `pr automerge --off`; `web on|off`; `pin`/`unpin`, `lock`/`unlock`, `hide`/`unhide`, `pair`/`unpair`, `archive`/`unarchive`, `trust`/`untrust` (`docs/commands.md`).
Direction: pick `X` / `unX` or `X on|off` and apply it to all; keep the rest as aliases.
Confidence: high.

**U-17 (low) Naming and prompt arguments follow a rule the user has to be told.**
Evidence: `ptyro new -n NAME`, `wt new -n NAME`, but `ws new NAME`, `cs new NAME`, `profile new NAME`; the rule is stated at `docs/commands.md:29-30`. The prompt is positional for bare `ptyro`, `chat` and `iterate` but `--prompt` for `new` and `fanout`; `-p` is `--project` and "once meant `--prompt`" (`docs/commands.md:104`). `send` and `broadcast` take `SESSION… TEXT` as one variadic argument (`main.rs:273`).
Direction: accept a trailing positional prompt on `new` and `fanout`; accept a positional name wherever `-n` is taken.
Confidence: high.

**U-18 (low) Missing or broken help strings.**
Evidence: `--json` has no description on `list`, `wt ls`, `send`, `wait`, `events`, `about`, `doctor`, `status`, `inbox`, `usage`, `uninstall`, `backup` (12 of 12 sampled; e.g. `main.rs:182-183`); `attach [SESSION]` and `wt unlock` have none; `send --no-enter` has none (`main.rs:275-276`); `ptyro wt --help` prints "`open` Open the worktree in an editor (\[editors\]; …" with the backslashes showing.
Direction: a test that every visible argument and subcommand has help.
Confidence: high.

**U-19 (med) Some errors leak internals or name no fix.**
Evidence: 79 call sites return `internal("unexpected reply")` and the shared helper prints the Rust debug form, `unexpected reply: {r:?}` (`cli.rs:936-938`); six daemon handlers answer `unsupported request {other:?}` (e.g. `crates/ptyro-daemon/src/review.rs:213`); "bad session id", "session has exited" (3 sites), "where am I? {e}" and "no daemon at <socket path>" (`crates/ptyro-client/src/lib.rs:433`) offer nothing to do next.
Hurt: these are what a user sees on version skew or a stale handle, the moments they most need a next step.
Direction: map "unexpected reply" and "unsupported request" to "this ptyro and its daemon disagree (client X, daemon Y): run `ptyro daemon upgrade`"; add the fix to the others ("session has exited: `ptyro restart NAME`").
Confidence: medium on how often users hit them; high on the text.

**U-20 (high) Version skew between client and daemon is explained badly on the CLI.**
Evidence:
- Within a major version, a request the older daemon can't decode is answered "the daemon couldn't read this request: {serde error}" (`crates/ptyro-daemon/src/conn.rs:261`). The client knows the daemon's version from the welcome (`crates/ptyro-client/src/lib.rs:124, 282-285`), but a grep for `daemon_version` in `crates/ptyro/src` and `crates/ptyro-tui/src` finds no use of it.
- Across a major version the message is "daemon {v} rejected this client: protocol X.Y is not supported… Restart it with `ptyro daemon stop` (sessions will end)" (`lib.rs:286-289`, `conn.rs:93-99`), which recommends the destructive path although `daemon restart --hibernate` exists and is what the docs recommend.
- This is the normal state after `brew upgrade ptyro` or a rerun of the install script, both of which the README says need a separate `ptyro daemon upgrade` (`README.md:112, 133-134`).
Hurt: the user upgrades, runs a command, and gets a serde error.
Direction: on any decode failure or "unexpected reply", compare versions and say "the daemon is still 0.0.67; `ptyro daemon upgrade` switches it without stopping sessions" (or do it after a yes). Have the package paths run the upgrade themselves.
Confidence: high on the text; medium on whether the TUI's health badge catches it first (it does flag "not this ptyro", `crates/ptyro-tui/src/app.rs:9831`).

**U-21 (low) `completions` and `shell-init` name shells differently.**
Evidence: `completions` takes free text "bash, zsh, fish, elvish or powershell" (`main.rs:766-769`); `shell-init` takes an enum `bash|zsh|fish|pwsh` with `powershell` as an alias (`cli.rs:1489`).
Direction: same enum, same names.
Confidence: high.

### C. Remote, phone, update, recovery

**U-22 (med) `ptyro web on` opens the web app to every network when Tailscale isn't running, and pairing defaults to full control.**
Evidence: `crates/ptyro-core/src/config.rs:936-940`: no `access` set and no Tailscale → `Access::Open` (0.0.0.0, `crates/ptyro/src/serve.rs:258-266`). `README.md:769-770` says so; `docs/remote.md:103` calls `localhost` "the default". `ptyro pair` gives scope `full` by default, "with a warning" (`main.rs:666-668`).
Hurt: the easiest path is the widest one. A token is still required, so this is exposure, not an open door, but the user did not choose it.
Direction: default to `localhost` and have `web on` ask "who should reach it?" on a terminal, with the three choices and their one-line meanings. Default pairing to `control` and let the web app ask to raise it when a page needs `full`.
Confidence: high on behaviour.

**U-23 (med) A phone without Tailscale must install a root CA through five settings screens.**
Evidence: `docs/remote.md:154-160` (iOS: download, VPN & Device Management, Install, About, Certificate Trust Settings). That CA is then trusted for every site (`docs/remote.md:165-171`).
Hurt: most users stop here.
Direction: lead with Tailscale in the pairing flow and say so in `ptyro pair`'s output when no trusted certificate is in use.
Known: ROADMAP H2.6 (CA trusted for every site) and TS1.2 (more ways to a certificate nobody installs).
Confidence: high.

**U-24 (low) Linked machines must run exactly the same version.**
Evidence: `docs/remote.md:79-80` "Linked machines must run the same ptyro. A link to another version stays down and says which machine to update".
Hurt: at 0.0.x cadence (v0.0.68 today) the link breaks every time one machine updates.
Direction: accept the same protocol major, or offer "update laptop2 from here".
Confidence: medium (I read the doc, not the check).

**U-25 (low) `ptyro backup` has no `ptyro restore`.**
Evidence: `docs/uninstall.md:31-35` "Putting it back is by hand, with ptyro stopped": unpack, copy three paths. No restore subcommand in `main.rs`.
Direction: `ptyro backup restore FILE` that stops the daemon, checks the schema version and puts the files back.
Confidence: high.

**U-26 (low) "Quit ptyro" is one modifier away from "detach".**
Evidence: `Ctrl-Q d` detaches; `Ctrl-Q Ctrl-D` stops every session and the daemon (`crates/ptyro-tui/src/keymap.rs:558`, `docs/keys.md:38-40`). Holding Ctrl a beat too long is the difference. It asks first, which is the saving grace.
Direction: move quit to the palette or a capital letter; keep `C-d` unbound.
Confidence: high.

### D. Configuration

**U-27 (med) About 200 settings in 20 sections, with section and key names that don't line up.**
Evidence: 207 schema entries (`settings.rs`), 28 marked advanced. Per section: chat 23, profiles 21, interface 19, themes 18, notifications 17, worktrees 15, agents 14. Mismatches a user will trip on:
- `worktrees.*` is your config, `worktree.*` is the project's (`settings.rs:735` and `:892`), one letter apart.
- `[remote]` is the web app, but `--remote` and `client.default_remote` (filed in the same "Web app and remote" section, `settings.rs:818`) are the ssh daemon.
- Shell settings are split between `shell.*` (section Shells) and `ui.shell_exit`, `ui.shell_style` (section Interface).
- "Attention" and "Notifications" are separate sections for one question: when and how am I told?
- `discover.*`, `ports.*` and `compat.*` live under "Worktrees and ports".
Hurt: `ptyro config set worktree.location …` fails for a reason that is hard to see.
Direction: when a key isn't found, suggest the near one ("did you mean `worktrees.location`?"); merge Attention into Notifications; rename `[remote]` to `[web]` with the old name accepted.
Confidence: high.

**U-28 (low) Jargon in settings a "reasonable user" has to edit.**
Evidence: the notification channel value `sinks`; `notifications.coalesce`; `attention.breakthrough`; "your terminal's own notification (OSC 9/777)" (`settings.rs:517`); "Let programs in sessions copy to your clipboard (OSC 52)" (`:709`); `profiles.*.guard`: "What to do when a spawn targets another profile's repository" (`:885`); `isolate`, `scrub_extra`, `lease_ttl`, `radar`.
Direction: keep the key, change the label and help ("services", "group repeats", "always tell me about").
Confidence: high.

**U-29 (low) Time values are spelled three ways.**
Evidence: `ui.which_key_delay` is an integer of milliseconds (`settings.rs:703`), `daemon.frame_interval_ms` carries the unit in its name (`:810`), `worktrees.stale_days` and `*.archive_purge_days` are integers of days, and everything else is a duration string (`"30s"`, `"2h"`).
Direction: accept a duration everywhere.
Confidence: high.

**U-30 (low) Defaults worth a second look.**
Evidence and reasoning:
- `ui.mouse = true` (`crates/ptyro-core/src/config.rs:143`): a newcomer's first drag-to-copy does nothing useful until they learn Shift-drag (`README.md:930`).
- `recorder.enabled = true` (`config.rs:1244`): every session's output is written to disk, and the README itself warns it "can hold secrets that scrolled past" (`README.md:957-958`).
- `daemon.scrollback_lines = 5000` (`config.rs:177`): small for agent sessions that print long diffs, though the recorder backs it.
Direction: say the Shift-drag hint in the status bar the first time a drag is captured; ask about recording during first run.
Confidence: medium (judgement calls).

**U-31 (low) Legacy options stay visible.**
Evidence: `agents.*.default`: "The older way to pick the default agent" (`settings.rs:838`); `web --via` shown as a visible alias with "(`--via` is the old name; here = localhost, lan = open)" in help (`main.rs:649-650`).
Direction: at 0.0.x, delete them or hide them.
Confidence: high.

### E. TUI

Source: a delegated read of `crates/ptyro-tui/src` (keymap.rs, form.rs and theme.rs in full; ui.rs, onboard.rs, inspector.rs, runs.rs in large part; the key, overlay, toast, mouse and palette parts of app.rs; key handling in chats.rs and sets.rs). Not read: tree.rs, copy.rs, health.rs, about.rs, shells.rs, quick.rs, pad.rs, chat.rs (the structured view), layout.rs. I re-read the cited lines for T-01, T-02, T-04, T-05, T-07, T-08, T-10, T-11, T-12, T-16, T-17 and T-20; the rest are the delegate's reading. Paths are under `crates/ptyro-tui/src/`. Nothing was run, so narrow-terminal behaviour is worked out from layout code.

Size of the surface: 78 prefix bindings, 61 sidebar bindings, 12 chat, 15 chat-list (counted in keymap.rs), about 250 named actions, plus per-view keys outside the keymap (copy mode about 35, inspector about 27, settings about 30, runs about 20). About 30 modal states; the `Overlay` enum alone has 14 variants (`app.rs:78-151`). Mode badges exist only for RECONNECTING, PREFIX, COPY, SIDEBAR and VIEW ONLY (`ui.rs:1551-1563`).

**T-01 (high) The which-key menu cuts off its last column, which holds detach, quit and "all keys", below 97 columns.**
Evidence: `ui.rs:325-326`: `col_w = 19`, width = `19 * columns + 2`, clamped to the terminal. Five groups need 97 columns. The fifth group, "this", holds `app.detach`, `app.quit`, `help` ("all keys"), `toast.act` (`ui.rs:290-303`). Checked.
Hurt: on a standard 80-column terminal the menu that teaches the keys hides how to leave and how to get help.
Direction: wrap groups onto a second row; always keep detach and help visible.
Confidence: high on the arithmetic; not run. ROADMAP UX1.14 (which-key) is marked done.

**T-02 (high) With `NO_COLOR`, mode badges are black text on the default background, and selection in most lists has no marker.**
Evidence: `theme.rs:102-123` sets every theme colour to `Color::Reset`; the badge style is `Style::new().black().bg(fit(c))` (`ui.rs:1545-1549`), so PREFIX / SIDEBAR / COPY are black on the terminal's own background. Checked. Per the delegate, selection is background-only in runs (`runs.rs:564`), change sets (`sets.rs:337`), inbox (`ui.rs:2068-2070`), first run (`onboard.rs:630-632`), inspector tabs (`inspector.rs:461`) and form choices (`form.rs:502-507`); only the sidebar, palette, settings and copy mode switch to reverse video. Focused and unfocused pane headers differ only by colour (`ui.rs:1348-1352`).
Hurt: a `NO_COLOR` user on a dark terminal can't see which mode they are in or which row ⏎ will act on.
Direction: reverse video for badges and every selected row in monochrome; a marker glyph on the selected row always.
Known: ROADMAP H2.8 says only "the TUI has `NO_COLOR` and a high-contrast theme and nothing else".
Confidence: high.

**T-03 (low) Some states are colour-only, and there is no ASCII glyph option.**
Evidence (delegate): the health badge is `⚕ N` in red or amber with the same glyph (`ui.rs:1631-1637`); load and usage thresholds are colour-only (`ui.rs:1607-1624`). Glyphs such as ☾ ◉ ◍ ⚕ ✎ ✉ ⚑ ＋ have no fallback (`ui.rs:23-60`); no ascii or glyph setting exists.
Confidence: high that no setting exists; rendering not tested.

**T-04 (high) `a` means "allow once" in a chat and "always allow" in peek, and in peek the first letter of a reply is taken as the answer.**
Evidence: peek: `'y' => "allow", 'a' => "always", _ => "deny"` when the reply is empty (`app.rs:7163-7168`); chat: `a` = `chat.allow`, `A` = `chat.always`, `d` = `chat.deny` (`keymap.rs:699-701`). Checked. `docs/keys.md:166-173` lists a third set for the structured view.
Hurt: the same key grants a lasting permission in one place and a single one in another; starting a typed reply with "a", "y" or "n" answers the prompt instead.
Direction: one permission key table for all three places; anything lasting needs Shift; never read the first letter of a reply as an answer (use a modifier, or a separate focus).
Confidence: high.

**T-05 (high) Messages have one slot, 3 to 10 seconds, and no history.**
Evidence: `toast` 3 s, `toast_err` 6 s, `toast_offer` 10 s, each replacing the one before (`app.rs:1656-1687`); `toast.act` (`C-q u`) acts only on the current one. Checked. No message log exists (grep).
Hurt: an error is gone in six seconds; an "undo" offer is wiped by the next message, such as "copied N characters".
Direction: a message log (palette and a key), with offers that stay valid; a "+2" count in the status bar.
Confidence: high.

**T-06 (med) Status-bar messages are cut at the end, where the offered key is.**
Evidence (delegate): `toast_offer` appends "· {key} {label}" last (`app.rs:1681`, checked); the left side of the bar gets what the right leaves, minimum 24 columns, with no ellipsis (`ui.rs:1678-1686`). The first-run message ending "C-q u take the tour" (`onboard.rs:455-463`) loses its key at 80 columns.
Direction: put the key first.
Confidence: medium-high.

**T-07 (med) `[ui] status_bar = false` removes every message, error and mode badge.**
Evidence: the status line is drawn only `if app.config.ui.status_bar && area.height > 2` (`ui.rs:134-136`, checked); messages render only there.
Direction: show messages as a transient line when the bar is off.
Confidence: high.

**T-08 (med) A mouse click anywhere closes the open overlay and discards what was typed.**
Evidence: `app.rs:6592-6597`: with an overlay open, any button-down sets `self.overlay = None`. Checked. Clicking a palette row closes the palette; clicking inside the task box loses the prompt. The mouse does nothing in the inspector, runs, change sets or first run (`app.rs:6598-6600`).
Direction: clicks pick; a click outside closes only when nothing has been typed; the wheel scrolls diffs.
Confidence: high.

**T-09 (med) Typing into the sidebar by mistake runs single-letter commands.**
Evidence (delegate): any bound nav key runs, the rest are dropped silently (`app.rs:2951-2955`); `s` and `v` clone the agent into a split, `n`, `c`, `!`, `r` each start something (`keymap.rs:691-692`). The only cue is the SIDEBAR badge.
Hurt: typing `ls -la` with the sidebar focused can start an agent session.
Direction: a burst of printable keys in the sidebar gets "you're in the sidebar: Esc goes back" and is swallowed.
Confidence: medium-high.

**T-10 (med) The task box, prompts, peek reply and form fields can only append and backspace.**
Evidence: the task box handles `push`, `pop` and `C-u` only (`app.rs:4814-4836`, checked); the same in prompts (`app.rs:5689-5706`), peek (`app.rs:7200-7204`) and forms (`form.rs:373-384`). The chat composer has a full line editor (`chats.rs:2219-2225`).
Hurt: the product's main input, the task prompt, can't be edited mid-line.
Direction: reuse the composer's editor everywhere.
Confidence: high.

**T-11 (med) Esc throws away a chat draft, and the whole prompt in the task box.**
Evidence: `chats.rs:2155-2157`: Esc with a non-empty draft clears it. Checked. Task box: Esc returns (`app.rs:4780`, delegate).
Direction: keep the draft and let `C-q u` restore it.
Confidence: high.

**T-12 (med) `m` in the inspector's Checks tab arms auto-merge with no question.**
Evidence: `inspector.rs:298-301` sends `PrAutomerge { off: false }` on `m`. Checked. The compare view's `m` does ask (`runs.rs:410-441`, delegate). Gate answers by digit are also immediate, and in change sets act on the first gate whatever is selected (`sets.rs:232-240`, `runs.rs:307-336`, delegate).
Direction: confirm anything that merges or resolves a gate.
Confidence: high for auto-merge.

**T-13 (med) Footers of full-screen views are one unwrapped line of 150 to 170 characters.**
Evidence (delegate): inspector Diff footer (`inspector.rs:491, 498`), runs compare footer (`runs.rs:549`), first-run footer (`onboard.rs:659`). "q close" is past column 120. `C-q ?` lists keymap tables and copy mode only, not these per-view keys (`ui.rs:2299-2340`).
Direction: wrap, lead with close, and give `?` a per-view page.
Confidence: medium-high.

**T-14 (med) First run: Esc skips setup for good, and the key line can scroll off.**
Evidence (delegate): Esc writes an empty config "so this screen doesn't come back" (`onboard.rs:191-198`); the scroll keeps only the selected row in view, so the footer falls below a long list. The tour is offered once, in a 10-second message that needs `C-q u` (`onboard.rs:456-463`); `help.tour` has no key and is not in the which-key menu.
Direction: Esc asks "skip for now, or don't show again?"; pin the footer; put the tour in the which-key menu and the empty pane.
Confidence: medium-high.

**T-15 (med) Forms don't scroll and don't mark required fields.**
Evidence (delegate): the form's box is clamped to the screen height with no scroll offset (`form.rs:484-492`, `ui.rs:1741`); "X is needed" appears only after ⏎ (`form.rs:306-328`). A 13-field session form on a short terminal hides its last fields and its key line.
Confidence: medium (not run).

**T-16 (med) The same letter does different things after the prefix and in the sidebar.**
Evidence (keymap.rs, checked): `z` zoom pane (616) / sidebar full-screen (670); `Z` sidebar full-screen (593) / fold everything (671); `c` close pane (613) / new SHELLS shell (677); `e` shells menu (615) / explain status (666); `R` pick a script (565) / restart (660); `g` chat list / top of list; `h` `l` panes / fold. The README has to explain it: "`C-z` fold the whole sidebar (`Z` in the sidebar)" (`README.md:866`).
Direction: the consistency test in idea 9; move the sidebar's own actions to keys that are free in both tables.
Confidence: high.

**T-17 (med) Holding Ctrl a moment too long after the prefix changes about ten keys.**
Evidence (keymap.rs, checked for the first four): `d` detach / `C-d` quit everything (558); `c` / `C-c`; `r` / `C-r` (565, 585); `z` / `C-z` (616, 594); also `p`/`C-p`, `n`/`C-n`, `t`/`C-t`, `g`/`C-g`, `l`/`C-l`. `docs/keys.md:7-8` teaches holding Ctrl for `Ctrl-Q Ctrl-Q`.
Direction: where both exist, make them the same action or keep the Ctrl form unbound. See U-26 for quit.
Confidence: high.

**T-18 (low) `x` / `X` change meaning by row.**
Evidence (delegate): `x` stops a session, removes an ended one, archives or removes a worktree, archives a chat; `X` kills a session or deletes a chat (`app.rs:3359-3393, 3921`). The hint says only "x stop" (`ui.rs:557`). Removals ask first.

**T-19 (low) Close keys differ by view.**
Evidence (delegate): help closes on Esc or C-c only, and `q` or `?` type into its filter (`app.rs:5565-5581`); inbox, explain, review queue, inspector, runs and sets close on `q`.

**T-20 (low) Key-config errors at start-up are discarded.**
Evidence: `let (keymap, _) = Keymap::new(&config);` (`app.rs:938`, checked). A mistyped `[keys]` entry does nothing and says nothing until a reload, which shows only the first error (`app.rs:2020-2021`, delegate).

**T-21 (low) Copy always reports success.**
Evidence (delegate): OSC 52 is written blind and `copied {n} characters` always shows (`lib.rs:406-416`, `app.rs:2555-2561`). In tmux without clipboard passthrough the clipboard stays empty.
Direction: say "sent to your terminal's clipboard" and link the tmux fix when inside tmux.

**T-22 (low) Tab in a new chat switches it to incognito.**
Evidence (delegate): `keymap.rs:697-698`. Tab is a completion reflex; incognito means nothing is kept.

**T-23 (low) Settings: Backspace resets a value; the restart warning can be cut off.**
Evidence (delegate): `settings.rs:1390-1394`, `:2618-2624`. Undo exists.

**T-24 (low) `docs/keys.md` and SPEC §11 are behind the keymap.**
Evidence (delegate): keys.md:349 says three key groups, there are five tables; the sidebar table omits `C`; the inspector table omits `X`. SPEC.md:3697 says "`e` has no key after the prefix" (it is `shells.menu`, `keymap.rs:615`); SPEC.md:3717 says `s`/`v` split "with a picked session" (they clone).

What the TUI does well, per the same read: every hint is built from the live keymap, so rebinds show (`ui.rs:1694-1717`); an empty pane lists next steps with your keys (`ui.rs:1306-1336`); an over-filtered sidebar says why and which key fixes it (`ui.rs:529-549`); quit names what hibernates and what ends and offers detach instead (`app.rs:5312-5338`); name fields check as you type and Tab applies the fix (`ui.rs:1962-2005`); the palette finds actions, sessions, worktrees, chats, themes and settings in one box.

### F. Web client

Source: a delegated read of `web/src` and `web/dist`. "Checked" means I re-read the cited lines. Paths are under `web/`.

**W-01 (high) The config editor's textarea is rebuilt on every app render, so focus and caret are lost while typing.**
Evidence: `src/settings.ts:500-505` creates a new `<textarea>` on each call; `src/app.ts:512-514` rebuilds the view and swaps the root on every render; renders fire on session events and on a 30-second timer (`src/app.ts:727`). Checked.
Hurt: editing `config.toml` from a phone on a busy machine keeps dropping the keyboard.
Direction: keep the element across renders, as the reply box, scratchpad and settings panel already do.
Confidence: medium-high (not run).

**W-02 (high) Expanded diff files collapse on every re-render.**
Evidence: `src/review.ts:82` builds `<details open={…}>` from a fixed default (`:145`: open only when there are three files or fewer, or it is the first); no open state is kept. The chat transcript does keep it (`src/app.ts:440`). Checked.
Hurt: reviewing a large diff on a phone, files snap shut whenever any session updates.
Direction: remember open state per worktree and file.
Confidence: medium-high.

**W-03 (high, accessibility) Many rows and all diff lines are click-only `div`s.**
Evidence: `src/kit.ts:467` renders rows as `div.row` with `onclick` and no role, tabindex or key handler; used for Health fixes, Devices, Archives, Workspaces, Checkpoints, run steps, recipes, the review queue. Diff lines likewise (`src/review.ts:92`). Not re-checked.
Hurt: keyboard and switch users can't revoke a device, apply a fix or comment on a line.
Direction: render tappable rows as buttons.
Confidence: high per the delegate.

**W-04 (high) Explanations live in `title` tooltips, which a phone never shows.**
Evidence: `src/app.ts:28-29` states each action's description is "its control's tooltip". The sticky/locked modifier behaviour of the key row is explained only there (`src/term.ts:20`); so is two-finger tap (`src/term.ts:28`) and what the "exact" tag means (`src/app.ts:398`). What "Always" allows is explained nowhere visible. Not re-checked.
Hurt: the phone-first client hides its help from phones. This is also how the parity check counts a web action as "discoverable".
Direction: on touch, show the description on long-press or behind an ⓘ; add a visible help page built from the same action list.
Confidence: high per the delegate.

**W-05 (high, accessibility) White text on mid-tone fills fails contrast in the dark theme.**
Evidence: `dist/style.css:4` defines `--accent #58a6ff`, `--ok #3fb950`, `--warn #d29922`, `--bad #f85149`; `#fff` text sits on them for Allow (`:64`), primary buttons (`:126`), user chat bubbles (`:110`), the offline banner (`:274`) and badges (`:145-146`). The delegate computed about 2.5:1 for the first three and 3.3:1 for the last; AA needs 4.5:1. Not re-checked or measured.
Direction: dark text on those fills, or darker fills.
Confidence: medium (hand-computed).

**W-06 (med) Cold start flashes "Offline", then "Nothing is running", then the data.**
Evidence: `src/app.ts:725-726` renders before connecting; the initial state is `"closed"`, which the banner reads as offline; lists render their empty states before the first load (`src/app.ts:290-295`, `src/chats.ts:273-276`, `src/projects.ts:127`). A deep link shows "No such session." while loading (`src/app.ts:385`). Lines 725-727 checked.
Hurt: every launch, and every notification tap, briefly tells the user something false.
Direction: a `loaded` flag and skeleton rows.
Confidence: high.

**W-07 (med) Input typed while disconnected is dropped.**
Evidence: `src/conn.ts:116-125` returns false and calls `onDropped`; the app shows "Not sent: offline. It reconnects by itself." Requests fail with the bare text "not connected" (`src/conn.ts:131`). Checked. The scratchpad has a real offline queue (`src/notes.ts:130-271`), so the pattern exists.
Direction: hold keystrokes and replies for a few seconds across a reconnect, or keep them in the box and offer to send.
Confidence: high.

**W-08 (med) An open tab never learns the web app was upgraded, and a protocol mismatch offers "Forget this pairing".**
Evidence: on `goodbye` the client only shortens its backoff (`src/conn.ts:68-70`); the hello sends `version: "web"` (`:47`); the rejected state shows the reason and one button, "Forget this pairing" (`src/app.ts:502-507`), and stops reconnecting (`src/conn.ts:95`). Lines 502-507 and conn.ts checked.
Hurt: after a daemon upgrade a stale page keeps running old code; on a major mismatch the only button unpairs the device when a reload was the fix. An installed iOS app has no reload control.
Direction: compare a build stamp on reconnect and offer "Reload"; make the rejected screen offer Reload first.
Confidence: medium-high.

**W-09 (med) The header's back arrow goes to the Sessions list, not back.**
Evidence: `src/app.ts:226`: `href: typeof back === "string" ? back : "#/"`. Checked.
Hurt: opening a session from the review queue or a worktree and tapping ‹ lands somewhere else, and adds a history entry that the system back button then loops through.
Direction: `history.back()` when the previous entry is in the app.
Confidence: high.

**W-10 (med) Sheets are not in browser history, so Android's back gesture leaves the page.**
Evidence: no `pushState` anywhere in `src/` (grep, checked); sheets close on `hashchange` (`src/kit.ts:225`).
Hurt: back with the New task form open discards the form and navigates away.
Direction: push a state when a sheet opens.
Confidence: high.

**W-11 (med) A failed "new task" loses what was typed.**
Evidence: `src/sessions.ts:218` and `:233` call `s.close()` before awaiting `startSession` / `createWorktree`; the error goes to a toast. Line 218 checked.
Direction: close on success only.
Confidence: high.

**W-12 (med) A stray digit key starts a shell.**
Evidence: `src/app.ts:704-708` maps 1-9 on the list page to `goShell`; `src/shells.ts:83` starts a new shell in that slot when none exists. Checked. `/` navigates to the list from any page and Ctrl+G is taken from the browser (`src/app.ts:695, 716`, not re-checked).
Hurt: a single unmodified key with a side effect, and no way to turn it off.
Direction: digits only go to existing shells; creating one needs a confirm or a modifier.
Confidence: high.

**W-13 (med) Allow / Deny give no pending state and show for devices that can't use them.**
Evidence: `src/app.ts:189-200` and the buttons at `:255-256`, `:400-403` neither disable nor show progress; a request can wait 120 s on a half-dead socket (`src/conn.ts:129-139`); there is no scope check before showing them. The chat permission card does disable on tap (`src/chats.ts:928`). Not re-checked.
Direction: disable and show "sending…"; hide for `read` scope.
Confidence: high per the delegate.

**W-14 (med) Leaving a side chat deletes it without asking.**
Evidence: `src/chats.ts:637-640`: on `hashchange`, a side chat that wasn't kept is sent `chat_delete … confirmed: true`. Checked. A banner states the rule (`src/chats.ts:1018`).
Hurt: tapping a citation, the inbox icon or the back gesture destroys it, with no undo.
Direction: keep it for a few minutes with an "Undo" toast.
Confidence: high.

**W-15 (med, accessibility) No reduced-motion support; the terminal is a canvas with no text alternative.**
Evidence: no `prefers-reduced-motion` in `dist/style.css` or `src/` (grep, checked); infinite pulse on working cards (`dist/style.css:56`). The terminal is a `<canvas>` (`src/term.ts:100`).
Known: ROADMAP H2.8 covers the canvas and says SPEC doesn't state whether accessibility is a goal.
Confidence: high.

**W-16 (med) Small tap targets and a crowded header.**
Evidence (sizes estimated from CSS, not measured): header ✎/✉ links about 26 px (`dist/style.css:35`), header icon buttons 36 px (`:271`), pane strip buttons about 23 px tall (`:463`), chat Edit/Retry/Fork links about 21 px (`:414`), `button.small` about 27 px (`:69`). The header carries up to nine controls on every page (`src/app.ts:225-234`). Not re-checked.
Direction: 44 px minimum; move the bell and the pad link into More.
Confidence: medium.

**W-17 (low) Toasts last 3.5 s, hold one message, and aren't announced.**
Evidence: `src/app.ts:183-187`, `:519`; the chat `/help` output is one roughly 300-character toast (`src/chats.ts:841`). Not re-checked.
Direction: errors stay until dismissed; add `role="status"`; a message log.

**W-18 (low) Pairing screen: bad input does nothing; failures show the raw response.**
Evidence: `src/app.ts:563-566`, `:93`. Not re-checked.

**W-19 (low) The bell shows no state and re-subscribes on every tap; there is no install prompt.**
Evidence: `src/app.ts:233`, `:582-600`; `beforeinstallprompt` is never handled. Not re-checked.

**W-20 (low) Inconsistent words.**
Evidence: "dormant" on cards and "Hibernated" on the session page (`src/app.ts:246`, `:405`); "Forget this pairing" / "Forget this device" / "unpair" / "Revoke"; "Quick shell" and "quick terminal" (`src/projects.ts:241`, `src/sessions.ts:32`); tab "Sets" for "Change sets" (`src/app.ts:162`); "＋ Task", "New task" and "New session" open the same sheet. Not re-checked.

**W-21 (low) iOS standalone: sticky headers likely slide under the status bar.**
Evidence: `black-translucent` status bar (`dist/index.html:9`) with sticky `top: 0` (`dist/style.css:22`, `:322`). Not re-checked, not run.
Confidence: medium.

**W-22 (low) Reconnect banner has no "retry now", countdown or "as of" time on the stale data.**
Evidence: `src/conn.ts:98-99`, `src/machine.ts:65`.

---

## Creative ideas

1. **`ptyro` as the only command a newcomer types.** No `setup`, `init` or `doctor` in the README. First run does the checklist, registers the repository, then asks one question: "What should Claude do here?"
2. **A ten-command default help.** `ptyro --help` shows `ptyro "task"`, `ls`, `attach`, `status`, `answer`, `stop`, `wt finish`, `web on`, `doctor`, `update`, then "`ptyro help all` for the other 240, `ptyro help <topic>` for worktrees, review, orchestration, phone".
3. **`ptyro help <word>` as a concept lookup.** `ptyro help worktree` prints the two-line definition, the three commands that matter, and the TUI key. One glossary feeds the CLI, the TUI's `?` and the web.
4. **Progressive disclosure by level.** A setting `ui.level = basic | full`: basic hides orchestration, change sets, leases, radar, machines and SHELLS panes from help, the palette, the which-key menu and the web tabs until the user first uses one or switches level. The surface a new user meets drops by about two thirds without removing anything.
5. **Guess-safe CLI.** Unknown words never start an agent without a yes; the question shows what it would cost ("a new worktree `sessions-list` and a Claude session"). Add `ptyro do "…"` as the explicit spelling.
6. **Self-healing version skew.** Any client that sees an older daemon offers the live upgrade on the spot, and the web shows "ptyro was upgraded: Reload".
7. **`ptyro explain` for the last error.** Every error gets a short code; `ptyro explain` (or `u` in the TUI) prints what happened, why, and runs the fix after a yes. The structured `fix` field is already there for refusals; extend it to every error.
8. **A "what can I press" strip.** Make the which-key menu permanent at one line for the first week (dismissable), showing the six keys that matter in the current focus.
9. **One-letter consistency audit as a test.** A test like the short-flag one: a letter that is bound in both the prefix table and the sidebar table must run the same action, or carry a waiver. It would have caught `z`/`Z`, `e`, `c` and `R`.
10. **Phone pairing wizard in the terminal.** `ptyro phone` asks who should reach it, turns the server on, detects Tailscale, shows the QR code, waits, and ends with "paired: iPhone, control scope" and a test notification. It replaces `web on`, `config set remote.access`, `pair` and `devices`.
11. **Scoped pairing by default with in-app elevation.** Pair at `control`; when a page needs `full`, the phone asks and the computer (TUI toast or desktop notification) approves.
12. **Session "story" line.** For each agent session, one plain sentence that needs no vocabulary: "Claude is fixing the flaky test in a copy of myrepo; it has changed 3 files and is waiting for you to allow `npm i`." Use it on the phone's card, in notifications, and for a future screen-reader text view.
13. **Undo everywhere on the web.** Replace confirm sheets for reversible actions (archive, stop, side-chat delete, mark read) with do-then-Undo toasts that stay 8 seconds, as the settings panel already does.
14. **A text view of any terminal.** One action, in the TUI and on the web, that shows the last screen as selectable, screen-readable text. It serves accessibility (H2.8), copying on a phone, and "paste this into a bug report".
15. **README as a generated artefact.** Key tables from the keymap, command tables from clap, settings from the schema, each limited to one line per row; a doc test fails when a cell passes 100 characters.
16. **"Why is this here?" on every sidebar row.** The `e` (explain status) idea extended to worktrees and projects: why it is stale, why it is pinned to a profile, why it was auto-registered, each with the one action that changes it.
