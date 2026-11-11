# ptyro back-end code quality review (2026-11-11)

Scope: `ptyro-daemon`, `ptyro-core`, `ptyro-store`, `ptyro-protocol`, `ptyro-pty`, `ptyro-term`,
`ptyro-run`, `ptyro-git`, `ptyro-agents`, `ptyro-meaning`, `ptyro-drive`, `ptyro-client`
(about 107k lines with tests, about 78k without). Checkout:
`the review checkout` at `e3a6cf3`. Read-only: nothing was built,
run or changed. All paths below are relative to that checkout.

## How to read this

- Severity is about the daemon staying correct and up for a week with many sessions: **high** = can
  lose data or wedge the daemon; **med** = a real defect or a risk that grows with load; **low** =
  smell, cost, or hygiene.
- "Verified" means I read the code path end to end. "Unverified" means the evidence is a grep or a
  partial read and the conclusion could be wrong.
- I found no high-severity defect. The hot paths (PTY reader, frame pacing, store actor, protocol
  codec) are careful. The weight of the findings is in a handful of medium risks and in structure.

## What I read, and what I did not

Read in full: `ptyro-daemon/src/session.rs`, `conn.rs`, `lib.rs` (serve loop), `ptyro-store/src/lib.rs`
and `migrate.rs`, `ptyro-core/src/sync.rs`.
Read in part (the sections named in findings): `daemon.rs` (struct, constructor, lookup, dispatch
head, `save_kv`, `forget_session`), `agents.rs` (feed processor, ticks, hibernation), `devices.rs`
(scope table), `ptyro-store/src/chat.rs` (open, threading), `ptyro-term/src/frame.rs` and `record.rs`,
`ptyro-pty/src/unix.rs`, `probe.rs`, `lib.rs`, `ptyro-git/src/lib.rs` (command runner),
`ptyro-protocol/src/lib.rs` (versioning, codec, enum sizes), `ptyro-client/src/lib.rs` (read loop),
`ptyro-core/src/settings.rs` and `config.rs` (schema, `validate`).
Greps and scripts only (not read): `chats.rs`, `changesets.rs`, `runs.rs`, `mail.rs`, `machines.rs`,
`worktrees.rs`, `shells.rs`, `registry.rs`, `notifier.rs`, `structured.rs`, `handoff.rs`, `push.rs`,
`ptyro-run`, `ptyro-drive`, `ptyro-meaning`, `ptyro-agents`, `ptyro-pty/src/windows.rs` and the other
Windows files, `ptyro-term/src/emulator.rs`. Those five largest daemon modules (about 14k lines) are
the biggest gap: findings about them are structural, not line-level.

---

## Findings

### Correctness and robustness

**Q-01 (med) Session events sent from the PTY reader carry an empty handle and `held: false`.**
Verified in the daemon; client impact partly verified.
`crates/ptyro-daemon/src/session.rs:1017`, `:1043`, `:1052`:
```rust
DomainEvent::SessionUpdated { info: self.info(String::new()) }
```
`Session::info(handle)` puts the argument in `SessionInfo.handle` and hard-codes `held: false`
(`session.rs:626`). Every other emitter goes through `Daemon::info` (`daemon.rs:539`), which computes
the short handle and `is_held`. These three sites fire on OSC 7501 status, OSC 7 cwd and OSC 9;4
progress, straight from the reader task. The TUI replaces its whole row with what arrives
(`crates/ptyro-tui/src/app.rs:1777` `Some(s) => *s = info`), so after a `cd` in an integrated shell or
a progress update the client holds a session with no handle and a cleared "held" flag until some
later daemon-level update repairs it. A status change usually follows quickly through the feed; a cwd
or progress change has no such follow-up. Direction: the session should not build wire objects it
cannot fill; have the reader emit a "changed" hint that the daemon turns into a full `SessionInfo`,
or give the session what it needs to fill both fields.

**Q-02 (med) An exited session whose process group still has a member is polled for ever, with a full
`/proc` scan every 250 ms on Linux.** Verified.
`crates/ptyro-pty/src/lib.rs:241`:
```rust
pub async fn until_group_empty(pgid: u32) {
    while others_in_group(pgid) > 0 {
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}
```
called from the waiter (`session.rs:1155`) after the leader exits and is kept as a zombie. On Linux
`others_in_group` (`ptyro-pty/src/probe.rs:281`) reads `/proc/*/stat` for every process on the
machine, synchronously, on a runtime worker. A command that leaves a background child in its group
(`sh -c 'server &'`, a build that leaks a watcher) keeps one task, one zombie and four whole-machine
process scans per second for as long as the stray lives; there is no bound and no backoff. Several
such sessions multiply it. Direction: back off (seconds, not 250 ms), run the scan off the runtime,
and cap how long the zombie is held before reaping anyway.

**Q-03 (med) One task applies every session's signals, and it awaits slow work inline.** Verified.
`crates/ptyro-daemon/src/agents.rs:126`:
```rust
tokio::spawn(async move {
    while let Some(f) = rx.recv().await {
        me.run_feed(f).await;
    }
});
```
`run_feed` reaches `apply_signals`, `on_transition`, `raise` (store round trip, notification
delivery), `after_first_prompt` → `rename_branch_to` (git), and `after_turn`, which loads the
project's config from disk and awaits `self.restart(..)` or `self.run_script(..)`
(`agents.rs:1020-1038`). While any of that runs, no other session's status moves, and a hook waiting
on `Feed::Apply`'s oneshot (`session.rs:206`) waits too. The channel in front of it is unbounded
(`daemon.rs:192`), and nothing restarts the task: a panic inside it (the panic hook only writes a
report) leaves status frozen for the daemon's life while the queue grows. Direction: keep the
single-writer ordering per session, but move the slow follow-ups (scripts, git, notifications) out of
the loop into spawned work, and supervise the loop.

**Q-04 (med) `chat.db` is migrated without the copy-aside that `ptyro.db` gets.** Verified.
`crates/ptyro-store/src/lib.rs:341-370` (`prepare`) copies an older `ptyro.db` to
`<name>.bak-v<N>` with `VACUUM INTO` before migrating. `crates/ptyro-store/src/chat.rs:309-340`
(`ChatDb::open`/`start`) checks for a newer schema and then calls `migrate::to_latest` directly; there
is no `bak-v` anywhere in `chat.rs`. `chat.db` has six migrations so far and is the one database that
holds things a person wrote and cannot regenerate, while `ptyro.db` is metadata. The migration is
transactional, so a failing one rolls back; what is unprotected is a migration that succeeds and is
wrong. Direction: share `prepare` between the two stores.

**Q-05 (med) git runs synchronously with no time limit, and not always off the runtime.** Verified
for the runner; call sites sampled.
`crates/ptyro-git/src/lib.rs:285-300`: every operation is `Command::output()`; `GIT_TERMINAL_PROMPT=0`
and a null stdin stop prompts, but a fetch against a dead remote, a repository on a stalled network
mount, or a held lock blocks for as long as git does. Many callers wrap it in
`worktrees::blocking` (`spawn_blocking`), so the runtime survives, but the request never ends and
cannot be cancelled. Some call it directly from async code: `changesets.rs:773` and `:3158`
(`ptyro_git::Git::new().rev(&wt.path, "HEAD")` inside `cs_new`/`cs_add`), `changesets.rs:3731`,
`:3781`. Two other modules each wrote their own "run with a deadline, drain pipes on threads, poll
`try_wait`" (`launch.rs:255-290`, `github.rs:92-118`); git has neither. A script over the daemon
found 55 blocking `std::fs`/`Command` calls inside `async fn` bodies with no `blocking(` nearby;
most are small file operations, but `remove_dir_all` of a change set's root (`changesets.rs:3317`) or
a chat's folders (`chats.rs:2726`) can take seconds on a large tree. Direction: one subprocess runner
(deadline, kill of the group, output cap) used by git, gh and secrets, and a rule that git is only
reached through it.

**Q-06 (med) The recorder writes to disk on every PTY read, on the reader task, under the terminal
lock.** Verified.
`session.rs:915-928`: the reader takes `term`, then `self.record(&buf[..n], term)` →
`Recorder::output` (`ptyro-term/src/record.rs:182`) → `File::write_all` (and, when the ring rotates,
a rename, a new file and a full-screen keyframe built from the emulator). Recording is on by default
(`ptyro-core/src/config.rs:1244`, `enabled: true`). On a healthy local disk this is a page-cache
copy; on a slow or full disk, or a network home directory, each write stalls a runtime worker and
every viewer of that session (`stream_frames` needs the same lock, `conn.rs:713`). Direction: hand
bytes to a recorder thread through a bounded queue that drops (and says so) when the disk cannot keep
up, as the input writer already does.

**Q-07 (med) Requests are spawned without a limit and are not cancelled when their client goes.**
Verified.
`conn.rs:316-335`: each `ClientMsg::Request` becomes two spawned tasks; nothing counts them per
connection, and `serve`'s teardown (`conn.rs:280-288`) aborts the event forwarder and waits two
seconds for the writer but leaves request tasks running. A long request (a gate, a `wait`, a git
operation from Q-05) outlives its client and holds its clone of `out`. A paired device with a valid
token can also queue requests as fast as it can send 16 MiB frames. Direction: a per-connection
`JoinSet` (aborted on close, with a small concurrency cap for remote clients).

**Q-08 (low) The connection's read loop awaits sends and dials, so a client that stops reading is
not cut off "at once".** Verified.
`conn.rs:237-279`: `conn.handle(msg).await` sits in one arm of the `select!`; inside it are
`self.out.send(..).await` on a 64-slot channel (`conn.rs:408`, `:416`, `:560`) and
`self.daemon.dial(..)` for another machine (`conn.rs:609`, up to `CHECK_TIMEOUT` = 20 s,
`machines.rs:45`). While that awaits, the revocation, shutdown and goodbye arms are not polled, and
the revocation arm itself ends in another `out.send(..).await` (`conn.rs:242`). A revoked device that
has stopped reading keeps its socket open until the writer errors. Direction: `try_send` or a short
timeout for the goodbye, and dial outside the loop.

**Q-09 (low) Elapsed time is measured on the wall clock almost everywhere.** Verified.
`ptyro_core::now_ms()` is `SystemTime` (`ptyro-core/src/lib.rs:48`); the daemon calls it 200 times
against 57 uses of `Instant`. Idle time for hibernation (`agents.rs:1531`), the status engine's stale
and tie windows, lease and message TTLs and nonce expiry all subtract wall-clock stamps. A step
forward (resume from sleep, NTP, the WSL2 drift already met in `changesets.rs:3045`) reads as hours
of idleness; a step back reads as zero. The project fixed one case (ROADMAP line 3020, `cs land`);
the rest follow the same pattern. Direction: keep wall-clock stamps for what is shown and stored,
and a monotonic clock for "how long since".

**Q-10 (low) Fire-and-forget persistence: memory is the truth, and a failed write is a log line.**
Verified.
`Store::spawn` (`ptyro-store/src/lib.rs:436`) logs and drops errors; `save_session`,
`save_workspace`, `save_project`, `save_worktree`, `save_trust` and `event` all use it. The queue
behind it is `std::sync::mpsc::channel` (`lib.rs:409`), which is unbounded, and
`session.rs:1008` adds a row per OSC 7501 report, so a program that emits status in a loop grows it.
If a job panics, the store thread ends and every later write is silently dropped
(`let _ = self.tx.send(..)`): the daemon runs on, forgetting everything, with nothing in `doctor`.
Smaller instances: `kv_set` results discarded in eight places (`notifier.rs:460`, `:540` for the
used approve/deny link nonces, `again.rs:78`, `registry.rs:715-723`, `suggest.rs:82`), and
`serde_json::to_value(..).unwrap_or_default()` before a save (`shells.rs:1029`, `machines.rs:379`,
`autoreg.rs:38`), which would store `null` over good state if serializing ever failed. Three store
methods run several statements with no transaction (`forget_sessions_in` `lib.rs:525`,
`close_orphans` `:549`, `prune` `:722`); each is safe to repeat, so this is tidiness. Direction:
count store failures and a dead store thread into `daemon.warn`, and rate-limit `osc7501` events.

**Q-11 (low) An unreadable push from the daemon is dropped without a word, and a dropped screen
frame freezes its stream.** Verified by reading; not reproduced.
`crates/ptyro-client/src/lib.rs:329-345`: a frame that fails to decode is skipped with `continue`
unless it is a response. `stream_frames` (`conn.rs:737`) then waits for an ack of a frame the client
never saw, with no timeout. Version skew is the realistic trigger: the handshake checks only `major`
(`conn.rs:93`), `minor` (now 39) is printed and never consulted, and the wire enums have no
catch-all variant, so an older client meeting a newer daemon's `SessionInfo` or event shape drops it
silently. Direction: log the drop once per kind, and either a catch-all variant or a resync on a
stalled ack.

**Q-12 (low) `write_private` exists five times, and the daemon's copies are not atomic.** Verified.
`ptyro-core/src/config.rs:1009` writes a temp file, syncs and renames. `ptyro-daemon/src/push.rs:75`
(the VAPID private key) and `ptyro-daemon/src/chats.rs:3352` truncate and write in place; two more
live in the CLI crate. A crash mid-write of the VAPID key invalidates every push subscription.
Direction: export the core one.

### Design

**Q-13 (med) `Daemon` is one struct with 78 fields and 46 mutexes, extended by 42 `impl Daemon`
blocks across the crate.** Verified (counts by script).
`crates/ptyro-daemon/src/daemon.rs:29-171`. Every feature (mail, leases, runs, change sets, review,
devices, push, chats, sidecars, usage, the scratchpad, WSL) adds fields here and methods in its own
file, so every module can reach every lock. There is no written lock order; I found no inversion (a
scripted scan of the session locks was clean, and comments at `session.rs:579`, `:767`, `:1467` show
the ordering is known locally), but nothing would stop one. Per-session state lives in daemon-level
maps keyed by `SessionId` (`sidecars`, `sidecar_live`, `held`, `last_notified`, `viewing`) and is
cleaned up in different places; `forget_session` (`daemon.rs:775-796`) clears only `last_notified`.
`radar_seen` (`mail.rs:1083`) is inserted into and, as far as grep shows, never trimmed (unverified).
Direction: subsystem structs that own their state and expose a narrow surface, as `Chats`
(`chats.rs:63`) already does; the daemon then holds a dozen subsystems, not 78 fields.

**Q-14 (med) Whole-state clones on common paths.** Verified.
- `Daemon::config()` (`daemon.rs:527`) deep-clones `Config` (five `BTreeMap`s of themes, profiles,
  keys, agents, editors, plus every section); it is called about 115 times, including inside loops
  (`daemon.rs:457`, per restored session) and to read one field (`machine()`, `daemon.rs:1148`, used
  in every `Welcome`).
- `Daemon::session(id)` (`daemon.rs:535`) clones the whole `Vec<Arc<Session>>` and scans it; it runs
  per keystroke (`conn.rs:361`, again in `placed_session`).
- `Daemon::info` (`daemon.rs:539`) clones the vector and collects every id to compute one handle, so
  a listing is quadratic.
- `Session::info` (`session.rs:565-644`) holds `meta` while it locks `term`, calls `tcgetpgrp` and
  reads the foreground process's name and cwd (`ptyro-pty/src/lib.rs:248`; on Windows a whole
  process snapshot, `windows.rs:1055`), then clones about 60 fields. It is called from 89 places,
  including per terminal event in the reader, and each `SessionUpdated` is cloned once per connected
  client by the broadcast channel.
None of this hurts at ten sessions. Direction: `Arc<Config>` swapped on reload, a map for sessions,
handles computed when the set changes, and the foreground probe cached by the tick that already runs.

**Q-15 (med) The wire protocol's types live in implementation crates.** Verified.
`ptyro-protocol` depends on `ptyro-agents` and `ptyro-run` (`crates/ptyro-protocol/Cargo.toml`), and
its messages embed `ptyro_agents::setup::{Plan, Event, Side}`, `ptyro_agents::updates::Outdated`,
`ptyro_run::Policy` and `ptyro_run::wire::*` (`lib.rs:1189`, `:2028-2056`, `:2306-2332`).
`ptyro-agents` in turn depends on `ptyro-git` and holds installers and host probing. So a change to
how setup works changes the wire format with no edit in the protocol crate, and every client links
the agent adapters. The protocol itself is one 2,725-line file whose `Request` has about 187
variants, `Reply` about 82 and `DomainEvent` 31. Compatibility rests on three things that do work:
a major-version check, `#[serde(default)]` on new fields, and both ends answering a request they
cannot read (`conn.rs:253-263`, `ptyro-client/src/lib.rs:333`). There is no record of which minor
added what beyond comments. Direction: wire types defined in (or re-exported through one module of)
the protocol crate, with the dependency arrow reversed; split the file by domain.

**Q-16 (low) "The database holds metadata, never contents" is true of terminal contents, and looser
than AGENTS.md says.** Verified.
What holds: no screen or scrollback reaches either database; recordings are files; session events
record sizes, not text (`agents.rs:365` `"chars": text.chars().count()`, likewise `:801`,
`github.rs:615`); a chat engine's session is never saved (`session.rs:660`) and its events are
filtered in SQL (`ptyro-store/src/lib.rs:487`). What is in `ptyro.db` and is content by a plain
reading: the first 120 characters of the first prompt as the session title (`agents.rs:306`, saved at
`session.rs:683`); notification text from programs in `inbox.body`; agent-to-agent message bodies in
`run_message.body`; review comments with the quoted code line under kv `review_comments`
(`review.rs:365`, `ptyro-core/src/model.rs:825`); the program-written `msg` of each OSC 7501 report
(`session.rs:1008`); and `client_env` in the kept-shells snapshot (`shells.rs:1077`, unverified what
it may contain). SPEC §14.2 says "never terminal contents", AGENTS.md says "never contents".
Direction: state the rule once, in SPEC's wording, with the list of user-written text that is
allowed in `ptyro.db`.

**Q-17 (low) Modes as booleans, states as strings.** Verified (counts by script).
`SpawnSpec` (`ptyro-core/src/session.rs`) has 31 fields, 13 of them `bool` (`quick`, `pad`, `tool`,
`loose`, `scratch`, `structured`, `mcp`, `again`, `unqueued`, `unrecorded`, `confirmed`,
`override_pin`, `name_or_next`), many mutually exclusive. `Place` repeats `quick`/`pad`/`unrecorded`
beside `SessionKind`. `Meta` (`session.rs:72-155`) has 44 fields spanning identity, process state,
viewers, turn tracking, orchestration and shells. About 35 public fields named `state`, `status`,
`kind`, `priority`, `source` are `String` across core, run, store and protocol (for example
`ptyro-core/src/model.rs:313` `notify: String`, `ptyro-run/src/wire.rs:109`, `:192-198`), and OSC
7501 states are matched as string literals (`session.rs:958-967`). Durations are stored as strings
in `Config` and parsed where used (32 `parse_duration` calls outside tests, each with its own
fallback). Direction: an enum for what kind of session is being started; typed durations parsed once
at load.

**Q-18 (low) Settings and config are one source for defaults, two for limits.** Verified.
`settings.rs` takes defaults from `Config::default()` and a test fails when a field has no schema
entry, which is the right design. Numeric bounds are written twice: `settings.rs:810`
`int(1, 1000)` and `config.rs:1888` `(1..=1000).contains(..)`; `settings.rs:809` and
`config.rs:1930` likewise. `Config::validate` is 202 lines of hand-written checks. Direction: derive
range checks from the schema.

**Q-19 (low) God files and functions.** See the table. The request path is a 489-line `match`
(`daemon.rs:1226` `handle_here`) that hands leftovers to sub-dispatchers ending in
`_ => Err(Error::internal("not a chat request"))` (`chats.rs:1212`, `machines.rs:1291`,
`shells.rs:710`): a request routed to the wrong group is a runtime error, not a compile error. The
device scope table (`devices.rs:133`) shows the better pattern (exhaustive, no catch-all).

**Q-20 (low) Two copies of the store actor.** `Store` (`ptyro-store/src/lib.rs:250-443`) and
`ChatDb` (`chat.rs:309-372`) each define the job type, the thread, `call` and the pragmas. Q-04 is a
consequence. Statements are prepared afresh on every call (`prepare`, one `prepare_cached` in the
whole crate).

**Q-21 (low) Hygiene.**
- 187 spellings of `.lock().unwrap_or_else(std::sync::PoisonError::into_inner)` remain in the daemon
  beside 557 uses of the `locked()` helper that replaced it (`ptyro-core/src/sync.rs`).
- 116 lines over 160 columns in the daemon and 72 in core (two and four over 400), against
  `max_width = 100`: rustfmt gives up on a statement containing a long string and leaves the whole
  expression unformatted (`devices.rs:270` is one 2,000-character match arm).
- No lint enforces the no-`unwrap` discipline the code keeps by hand (`clippy::unwrap_used` is not in
  `[workspace.lints]`).
- Startup runs about 30 `start_*`/`load_*` steps in sequence before the first `accept`
  (`lib.rs:456-510`), including replaying each dormant session's recording inline
  (`daemon.rs:497`); clients wait in the backlog meanwhile.
- No cap on sessions, or on how long an exited session keeps its emulator and scrollback in memory
  (unverified: I found no retention setting).
- A structured session's transcript and a followed chat's branch are held whole in memory with no
  cap that I found (unverified).

---

## Tables

### Largest files (back-end crates, lines)

| File | Lines |
|---|---|
| `ptyro-daemon/src/chats.rs` | 3,961 |
| `ptyro-daemon/src/changesets.rs` | 3,889 |
| `ptyro-core/src/config.rs` | 3,334 |
| `ptyro-protocol/src/lib.rs` | 2,725 |
| `ptyro-daemon/src/runs.rs` | 2,621 |
| `ptyro-store/src/chat.rs` | 2,204 |
| `ptyro-core/src/settings.rs` | 2,007 |
| `ptyro-daemon/src/daemon.rs` | 1,961 |
| `ptyro-daemon/src/mail.rs` | 1,917 |
| `ptyro-daemon/src/agents.rs` | 1,897 |
| `ptyro-run/src/changeset.rs` | 1,806 |
| `ptyro-daemon/src/session.rs` | 1,765 |

The daemon crate is 42,221 lines in 47 files, a quarter of it in three files.

### Longest functions (lines, by indentation-matching script; approximate)

| Function | Location | Lines |
|---|---|---|
| `lead_op` | `ptyro-daemon/src/mail.rs:1221` | 513 |
| `handle_chat` | `ptyro-daemon/src/chats.rs:703` | 512 |
| `handle_here` | `ptyro-daemon/src/daemon.rs:1226` | 489 |
| `create_worktree` | `ptyro-daemon/src/worktrees.rs:228` | 375 |
| `cs_new` | `ptyro-daemon/src/changesets.rs:483` | 348 |
| `launch` | `ptyro-daemon/src/launch.rs:428` | 294 |
| `cs_tick` | `ptyro-daemon/src/changesets.rs:1740` | 279 |
| `follow_engine` | `ptyro-daemon/src/chats.rs:2349` | 276 |
| `chat_import` | `ptyro-daemon/src/chats/import.rs:879` | 274 |
| `handle_runs` | `ptyro-daemon/src/runs.rs:2147` | 251 |
| `handle_devices` | `ptyro-daemon/src/devices.rs:410` | 251 |
| `spawn` | `ptyro-daemon/src/daemon.rs:800` | 242 |
| `validate` | `ptyro-run/src/recipe.rs:548` | 228 |
| `validate` | `ptyro-core/src/config.rs:1779` | 202 |

40 functions exceed 140 lines; 36 of them are in the daemon.

### Inventories per crate (non-test code; test modules cut at `#[cfg(test)]`/`#[cfg(all(test…`)

| Crate | Lines | `unwrap()` | `expect(` | `let _ =` | `.ok();` | `#[allow]` | `unsafe` | TODO/FIXME | `sleep(` | unbounded channels |
|---|---|---|---|---|---|---|---|---|---|---|
| ptyro-daemon | 35,875 | 0 | 6 | 326 | 13 | 9 | 0 | 0 | 79 | 7 |
| ptyro-core | 13,997 | 0 | 2 | 21 | 1 | 0 (+1 `unsafe_code`) | 2 | 0 | 0 | 0 |
| ptyro-store | 3,239 | 0 | 0 | 10 | 0 | 1 | 0 | 0 | 0 | 2 (std mpsc) |
| ptyro-protocol | 2,524 | 0 | 0 | 0 | 0 | 4 | 0 | 0 | 0 | 0 |
| ptyro-pty | 3,996 | 0 | 0 | 11 | 2 | 22 | about 120 | 0 | 3 | 0 |
| ptyro-term | 2,386 | 0 | 3 | 2 | 0 | 0 | 0 | 0 | 0 | 0 |
| ptyro-run | 4,673 | 0 | 0 | 0 | 0 | 0 | 0 | 0 (1 in generated text) | 0 | 0 |
| ptyro-git | 1,671 | 0 | 1 | 10 | 0 | 0 | 0 | 0 | 0 | 0 |
| ptyro-agents | 7,693 | 0 | 2 | 58 | 1 | 0 | 0 | 0 | 6 | 0 |
| ptyro-meaning | 725 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| ptyro-drive | 2,025 | 0 | 2 | 0 | 0 | 0 | 0 | 0 | 0 | 0 |
| ptyro-client | 1,286 | 0 | 1 | 15 | 0 | 0 | 0 | 0 | 2 | 2 |

Notes on the numbers:
- The daemon's six `expect`s are all `.expect("infinite")` on an endless name iterator
  (`daemon.rs:729`, `finish.rs:169`, `registry.rs:375`, `runs.rs:1041`, `:2449`, `worktrees.rs:370`).
- `let _ =` in the daemon by what is discarded: about 80 broadcast sends (`events`, `mirrored`: no
  receiver is normal), about 30 best-effort file removals, 8 watch sends, about 10 `kv_set`/`flush`
  results and 7 `std::fs::write` (the last two groups are the ones in Q-10).
- `#[allow]` in the daemon: 6 `too_many_arguments`, 5 `type_complexity`, 1 `unnecessary_wraps`;
  protocol: 4 `large_enum_variant`. No `dead_code` allows except three `cfg_attr` ones for
  platform-only code. All 22 in `ptyro-pty` are `unsafe_code`.
- Unbounded channels: the signal feed and the status-refresh queue (`daemon.rs:191-192`), three in
  `structured.rs` (`:230`, `:528`, `:529`), the config watcher, the Windows listener, the client's
  outgoing and push queues (`ptyro-client/src/lib.rs:295`, `:324`), and both store job queues.
- Other daemon counts: 105 `as` integer casts (mostly `u64`↔`i64` milliseconds for SQLite, which
  cannot overflow in practice), 274 `unwrap_or_default()`, 60 `Error::internal(e.to_string())`-style
  conversions, 34 `spawn_blocking`/`block_in_place`, about 100 `tokio::spawn` with no handle kept.

### Unsafe

`unsafe_code = "deny"` for the workspace (`Cargo.toml`), allowed per item or per file in `ptyro-pty`
and once in `ptyro-core/src/os.rs`. By file (blocks / `SAFETY` comments): `windows.rs` 53/43,
`winproc.rs` 20/19, `probe.rs` 11/8, `winreg.rs` 10/10, `handoff.rs` 7/7, `console.rs` 6/5,
`acl.rs` 5/3, `pipe.rs` 5/5, `winread.rs` 3/3, `unix.rs` 2/2, `core/os.rs` 2/1. Unix unsafe is three
kinds of thing: one `pre_exec` `prctl`, `tcgetpgrp` (with the reason rustix is not used,
`unix.rs:109`), and fd adoption for live upgrade. I did not audit the Windows blocks; about 18 of
them have no `SAFETY` line.

### Platform conditionals

`cfg(windows)`-side / `cfg(unix)`-side attributes: daemon 37/34, core 22/41, pty 24/16 (plus 37
per-OS), agents 10/14, client 4/7. `ptyro-run`, `ptyro-drive`, `ptyro-meaning`, `ptyro-protocol`
have none. In the daemon the split is mostly paired functions (`inherit_pty`, `carried_lock`,
`stop_signal`) rather than scattered lines; `session.rs:836-899` is the densest. I did not check the
Windows side for behavior drift.

---

## Five things the code does well

1. **Panic discipline.** Zero `unwrap()` outside tests in twelve crates; the six `expect`s are
   provably unreachable. Poisoned locks are survived on purpose (`ptyro-core/src/sync.rs`), a
   panicking request still answers its caller (`conn.rs:322-333`), and the emulator is fed inside
   `catch_unwind` with a crash report and a fresh emulator (`session.rs:760-788`).
2. **The PTY-to-client path.** The reader never waits for the writer (bounded queue, `try_send`,
   with the deadlock it avoids written down, `session.rs:232-235`); frames are damage-based deltas,
   one in flight per subscriber, paced and acknowledged (`conn.rs:701-747`), so a slow client gets
   fewer, larger frames and the session never waits. Oversized replies are answered, not fatal
   (`conn.rs:162-203`).
3. **Process lifecycle on Unix.** The exit is observed without reaping so a later group signal
   cannot reach a recycled pid (`session.rs:1134-1158`, `ptyro-pty/src/unix.rs:87-104`); Ctrl-Z of a
   session leader is detected and resumed (`session.rs:1216-1244`); SIGHUP is ignored for a stated
   reason (`lib.rs:322-338`).
4. **The store.** One thread owns the connection; WAL, a busy timeout set before the pragmas,
   foreign keys on; migrations in one transaction with the version; a newer schema refused
   untouched; an older one copied aside first; one unreadable row does not hide the rest
   (`ptyro-store/src/lib.rs:341-420`, `:568-585`). Tests migrate a database from every earlier
   version.
5. **Compile-time coverage where it matters.** The device scope table is an exhaustive match with
   no catch-all (`devices.rs:129-270`); the settings schema is checked against the config structs by
   a test and takes its defaults from `Config::default()` (`settings.rs:1-13`); unsafe is denied
   workspace-wide and confined to one crate; git runs with hooks, fsmonitor and repository-named
   drivers switched off for everything ptyro does by itself (`ptyro-git/src/lib.rs:231-250`).

## Five refactors that would pay off most

1. **Break `Daemon` into subsystems that own their state** (Q-13). Mail, runs, change sets, review,
   devices and shells each become a struct with its own locks and a narrow interface, as `Chats`
   is. This is what makes lock order, cleanup on session removal and the 42 scattered `impl Daemon`
   blocks tractable, and it is the precondition for splitting `chats.rs`, `changesets.rs` and
   `runs.rs`.
2. **One subprocess runner** (Q-05) with a deadline, group kill, output cap and off-runtime
   execution, used for git, gh, secret commands and notifiers. It replaces two hand-written copies
   and gives git the timeout it lacks.
3. **Move slow work off the single-consumer loops and bound their queues** (Q-03, Q-06, Q-10): the
   signal feed, the recorder write and the store job queue. Supervise the loops so a panic is a
   restart and a warning, not a silent stop.
4. **Make the protocol crate the owner of wire types and split it by domain** (Q-15, Q-11). Reverse
   the dependency on `ptyro-agents` and `ptyro-run`, add a catch-all for unknown pushes, and keep a
   per-minor changelog next to `PROTO`.
5. **Share state instead of cloning it** (Q-14, Q-01): `Arc<Config>`, a session map, handles kept
   with the set, and one place that builds `SessionInfo`. That removes the quadratic listing and the
   half-filled events at the same time.

Also cheap and worth doing alongside: share `prepare` and the actor between the two databases
(Q-04, Q-20), and give `until_group_empty` a backoff and a bound (Q-02).

## Tracked already?

ROADMAP has two earlier hardening passes (H1, H2). H2.1 is the lock-poisoning change and H2.7 the
unreaped-zombie design; the wall-clock problem was fixed for one case (line 3020). I found no
ROADMAP entry for Q-01 through Q-08, Q-13 or Q-15 by keyword search; I did not read H1/H2 line by
line, so some may be recorded under other words.

---

## Second pass


Continues `quality-backend.md`
(Q-01 to Q-21). This file holds Q-22 to Q-38: a line-level read of the daemon files the first pass
only grepped. Checkout: `the review checkout`. Read-only: nothing
was built or run, so every finding is from reading and none was reproduced. Paths are relative to
`crates/` in that checkout unless they start with `ptyro-`.

Severity and "verified" mean what they do in the first file: **med** = a real defect or a risk that
grows with load; **low** = smell, cost or hygiene; "verified" = I read the code path; "unverified" =
partial read or inference.

No high-severity defect. Eight medium findings, nine low.

## What was read this time

- `ptyro-daemon/src/changesets.rs`: all of the non-test code (lines 1-3800), except the middle of
  `overlay_files` (about 3723-3775: the python and compose branches), which I skimmed.
- `ptyro-daemon/src/chats.rs`: lines 196-535 (live map, start-up, records, delete), 1389-1456
  (`chat_send`), 1667-2020 (busy check, retry, edit, branch, fork, task), 2116-2835 (`start_engine`,
  `follow_engine`, turn end, stop, file removal, follow/unfollow).
- `ptyro-daemon/src/chats/import.rs`: outline and I/O points only (bounds, `spawn_blocking`, awaits).
- `ptyro-daemon/src/runs.rs`: 155-925 (emit, create, restart recovery, actor, finish, cancel, step
  execution, attempts) and 1661-2052 (run, gate, apply, sleep steps).
- `ptyro-daemon/src/machines.rs`: 330-460, 500-920, 1296-1385 (records, link loop, mirror, follow,
  add).
- `ptyro-daemon/src/worktrees.rs`: 228-845 (`create_worktree`, `run_script`, `remove_worktree`).
- `ptyro-store/src/chat.rs`: 300-372, 540-835, 976-1026, 1497-1695 (open, delete, compact, node
  writes, interrupted turns, tip, copy, branch, siblings).
- `ptyro-daemon/src/verify.rs`: 612-682 (`run_to_log`, `GroupGuard`), because run steps use it.
- `ptyro-pty/src/windows.rs`: about 150 lines sampled (handle wrapper, spawn tail, cross-process
  duplication, pipe creation). Not an audit.

## Still unread

- `chats.rs`: `handle_chat` (703-1215, 512 lines), `chat_new`, search (1460-1660),
  `update_incognito`, titles, `preset_launch` (3384-3617) and the helpers after it.
- `chats/import.rs`: the three parsers (`parse_claude`, `parse_codex`, `parse_gemini`) and the body
  of `chat_import`.
- `runs.rs`: spawn, send and loop steps and `wait_turn` (930-1660), `handle_runs`, `fanout`.
- `machines.rs`: qualification and routing helpers (1-330, 920-1290).
- `worktrees.rs`: everything outside create, run_script and remove (file bringing, rename, refresh,
  gc).
- `ptyro-store/src/chat.rs`: list and search SQL, `put_branch`, `import_batch`, conv helpers, and
  `chat/vec.rs`.
- `ptyro-pty/src/windows.rs`: about 1,150 of 1,328 lines; all of `winproc.rs`, `winread.rs`,
  `console.rs`, `acl.rs`, `winreg.rs`.
- Still grep-only from the first pass: `mail.rs`, `shells.rs`, `registry.rs`, `notifier.rs`,
  `structured.rs`, `handoff.rs`, `review.rs`, `evidence.rs`, `github.rs`, `push.rs`, and the crates
  `ptyro-run`, `ptyro-drive`, `ptyro-meaning`, `ptyro-agents`, `ptyro-term/src/emulator.rs`.

---

## Change sets (`ptyro-daemon/src/changesets.rs`)

### Q-22 (med) A push or land in progress does not survive a daemon restart or live upgrade, and leaves the set looking busy

Verified by reading; the upgrade half rests on the absence of any check in `handoff.rs`.

Evidence. `cs_land_request` records the phase and a train in the run's log, then runs the train in a
spawned task (`changesets.rs:2882-2887`):
```rust
self.cs_emit(
    run,
    vec![CsEvent::Train { train: Some(train) }, CsEvent::Phase { phase: Phase::Landing }],
);
let (me, run2, name) = (Arc::clone(self), run.to_owned(), set.name.clone());
tokio::spawn(async move {
    let r = me.cs_land(&run2, yes).await;
```
`cs_push_request` opens a `push-…` gate and spawns a task that waits on it (`:2622-2627`); the wait
gives up after a day (`:1660`). The job slot, the waiter and the train are in `CsLive`, which says of
itself (`:59`): "not persisted: it is re-derived after a restart". Nothing re-derives them. After a
restart the actor's tick acts only on two kinds of gate (`:1772`, `:1820`):
```rust
if !step.starts_with("contract-r")
...
let (Some(choice), true) = (&g.choice, step.starts_with("ask-")) else { continue };
```
`Phase::Landing` is set at `:2884` and reset only by the task itself (`:2898`). `handoff.rs` waits
for launches in flight (`handoff.rs:393-404`) and has no check for a set's job.

Why it matters. After a restart or upgrade mid-train the set shows `Landing` with a train that is
neither stopped nor running, and an open `push-…` or `land-<member>` gate can still be answered, to
no effect. Running `cs land` again does recover (it builds a new train from who has landed), but
nothing tells the person to.

Direction. At start, a set found in `Landing` with no stop recorded gets its train marked stopped
("the daemon restarted") and its orphan gates closed, as recipe runs get `recover`.

### Q-23 (med) Creating a set is "all or nothing" only for a failure before `git worktree add`

Verified.

Evidence. `cs_new` rolls back the members made so far when one member fails (`changesets.rs:778-801`):
```rust
Err(e) => {
    for w in made.iter().rev() {
        let _ = self.remove_worktree(&w.id, RemoveOpts { force: true, stop_sessions: true, keep_branch: false }).await;
    }
    let _ = std::fs::remove_dir_all(&root);
    ...
    "couldn't create {}'s worktree, so nothing was created: {}",
```
The failing member is not in `made`. `create_worktree` can return an error after its worktree exists
on disk and in the registry (`put_worktree` at `worktrees.rs:406` and `:514`):
`self.scripts_info(&p, &wt)?` (`worktrees.rs:518`), `self.run_script(&wt, Some("setup"), ..).await?`
(`:531`), `self.spawn(spawn).await?` (`:579`). After the worktree loop, `cs_new` has more `?` exits
that roll nothing back: `std::fs::write(&contract, ..).map_err(cs_err)?` (`changesets.rs:809`),
`self.spawn_lead(&run).await?` (`:820`, `:824`); `self.start_cs_actor(&run)` is the last line
(`:828`). `cs_add` has the same shape: the member is added (`:3147-3161`), then
`self.spawn_member(run, &m.name).await?` (`:3165`).

Why it matters. A late failure in one member's `create_worktree` removes the earlier members,
deletes the set's root (where the failed member's directory is) and reports "nothing was created",
leaving a registry entry, git worktree metadata and a branch for a directory that is gone. A failure
to start the lead returns an error for a set that exists with every worktree made and no actor.

Direction. Have `create_worktree` either undo what it made or say in its error that a worktree was
left; treat "set created, agent didn't start" as a created set with the failure recorded on it.

### Q-24 (med) `cs rm` checks for unsaved work before it stops the agents, then removes with `force`

Verified.

Evidence. `cs_rm_inner` in order: the safety check per member (`changesets.rs:3241-3273`,
`ptyro_git::Git::new().safety(..)` at `:3252`); then "The agents first." (`:3274-3288`); then
(`:3293-3300`):
```rust
.remove_worktree(
    w,
    RemoveOpts {
        force: true,
        stop_sessions: true,
        keep_branch: m.landed.is_none() && !force,
    },
)
```
A failed removal becomes a note and the function carries on (`:3304`), to (`:3316-3318`):
```rust
if keep.is_empty() {
    let _ = std::fs::remove_dir_all(&set.root);
}
```

Why it matters. An agent still working can write between the check and its stop, and that work is
discarded without the refusal the check exists to give. When a member's removal fails, its directory
is deleted anyway with the root while its registry entry stays.

Direction. Stop the agents, then check, then remove; keep the root when any member's removal failed.

### Q-25 (med) Check-then-act across awaits, with no per-set lock

Verified.

Evidence.
- Contract revisions. `record_revision` reads the set (`changesets.rs:1415`), awaits a diff
  (`:1427`) and a checkpoint (`:1433-1439`), then numbers the revision from the stale read (`:1442`):
  `n: set.contract.len() as u32 + 1`. The actor's tick calls it (`:1760`) and so do the `Submit` and
  `Revise` requests (`:1578`, `:1591`).
- Checkpoints. `cs_checkpoint` picks `let n = set.checkpoints.iter().map(|c| c.n).max().unwrap_or(0) + 1;`
  (`:2359`), then runs git per member and writes `refs/ptyro/cs/{setname}/{n}` (`:2369`). Callers: a
  manual checkpoint (`:421`), before a revision (`:1434`), before a restore (`:2438`), before a land
  (`:2950`). A failure at the second member returns via `.await??` (`:2372`) with the first member's
  ref written and no event.
- Set names. `set_named(&name)` is checked at `:492`; `create_run` is at `:747`, after a `gh issue
  view` (`:682-693`) and manifest reads (`:588`).
- Worktree names. `taken(&n)` at `worktrees.rs:255`; `self.put_worktree(wt.clone())` at `:406`, after
  several awaits. The store's unique index (`worktree_live_name`, `ptyro-store/src/lib.rs:95`) then
  rejects the second row in a fire-and-forget write (Q-10).
- Machines. `already_added` is checked at `machines.rs:1352`; `open_link` inserts over an existing
  entry without aborting its task (`:420-424`).

Why it matters. Two revisions can get the same number; two checkpoints can share `n` and overwrite
each other's refs, so a recorded checkpoint no longer matches its ref; two sets, worktrees or links
can share a name, with memory and disk disagreeing. The job slot (`claim_job`, `changesets.rs:2635`)
covers push, land and rm only.

Direction. One async lock per set for anything that derives a number or a name from the set's state;
the same for worktree names per project.

### Q-26 (low) `cs restore` and the overlay are not failure-atomic

Verified.

Evidence. `cs_restore` restores members one at a time and returns on the first error
(`changesets.rs:2441-2449`):
```rust
blocking(move || ptyro_git::Git::new().restore(&path, &snap))
    .await?
    .map_err(crate::registry::git_err)?;
```
The note (`:2454-2455`) and the agents' messages (`:2457-2460`) come after the loop. The "before
restoring" checkpoint is taken only when something was unrecorded (`:2425-2440`).
`overlay_files` undoes the previous overlay (`:3601-3619`), writes the new one, and records what it
wrote last (`:3789`). In the node branch (`:3718-3722`):
```rust
let _ = std::fs::remove_dir_all(&aside);
std::fs::rename(&link, &aside).map_err(cs_err)?;
}
let how = ptyro_core::os::symlink(std::path::Path::new(&p.path), &link)
    .map_err(cs_err)?;
```

Why it matters. A restore that fails part-way says nothing about which members were already
restored. An overlay that fails between the rename and the link leaves a package moved aside to
`.<name>.ptyro-aside` with no record of it; the `remove_dir_all` deletes any earlier set-aside copy
first.

Direction. Report per-member results from restore; write the overlay record as each file is made.

### Q-27 (low) Smaller items in `changesets.rs`

Verified.

- `out_of_fence` is only ever added to (`:1985`) and read (`:1004`, `:1027`); it is never cleared
  while the set lives, and it is memory only. A member flagged once stays flagged until a restart,
  then is silently unflagged.
- `record_revision` ignores a failed write of the contract (`:1452`,
  `let _ = std::fs::write(Self::contract_path(&set), &rev.text);`). The tick then sees a file that
  differs from the head (`:1750`) and records the old text as a new revision.
- `text_diff` names its two temp files by the millisecond (`:1385-1387`); two diffs in one
  millisecond share them.
- `wait_gate_inner` polls every 300 ms for up to a day on the wall clock (`:1660-1700`), though the
  run has a `wake` notifier that `exec_gate` uses (`runs.rs:1731`, `:1794`).
- `cs_commit` reports a partial commit as a count (`:2582-2585`, "members before it are committed:
  {}") and writes no ledger note; `cs_push` names them and does (`:2719-2731`).
- `cs_pr` emits `CsEvent::Pushed { name: name.clone(), sha: String::new() }` as a flag (`:2787`), and
  `describe` has a special arm for the empty sha (`:3488`).
- `run.unwrap_or_default()` appears 20 times in `cs_op` (`:353-445`) for a value that is always
  present on those arms.

## Chats (`ptyro-daemon/src/chats.rs`, `ptyro-store/src/chat.rs`)

### Q-28 (med) An engine launched for a chat can be left running with nobody following it

Verified.

Evidence. In `start_engine` the session is launched at `chats.rs:2219-2248` (`self.launch(Launch {
.. }).await?`). Then (`:2278-2292`):
```rust
if let (Some(how), false) = (how, info.incognito) {
    db.add_conv(Conv { .. })
    .await
    .map_err(store_err)?;
}
*live.engine.locked() = Some(Engine { session: Arc::clone(&session) });
```
`follow_engine`, the only thing that stops and forgets an engine, starts at `:2314`. The session has
`Place.chat` set (`:2232`), so it is hidden from the session tree and never saved to `ptyro.db`
(`session.rs:660-663`). In the same function the previous conversation is ended before the launch is
known to work (`:2149-2151`, `db.end_conv(old.id.clone());`).

Why it matters. If the `chat.db` write fails after the launch, an agent process runs hidden until
the daemon restarts. A failed launch also costs the chat its native resume.

Direction. Record first and launch second, or stop the session on every error path after the launch.

### Q-29 (med) An answer's text is written fire-and-forget, and a fork is several separate writes

Verified.

Evidence. The person's message is awaited (`chats.rs:1398`, "The record first", and
`self.rec_put_now(id, user.clone()).await?` at `:1411`). The answer is not (`:2474-2476`):
```rust
if keep {
    db.put_node(chat.clone(), node, Some(conv.clone()), index_tools);
}
```
and again at `:2557`. `put_node` is `self.spawn(..)` (`ptyro-store/src/chat.rs:643-646`), which only
logs a failure. `chat_fork` calls `db.create(..)` (`chats.rs:1846`), `db.copy_branch(..)` (`:1863`)
and `db.add_conv(..)` (`:1878`), each its own call; `copy_branch` calls `put_node` once per node
(`chat.rs:1009-1022`), and `put_node` opens its own transaction (`chat.rs:1550`). `delete` runs two
updates with no transaction (`chat.rs:565-579`), as does the purge loop in `compact` (`:613-620`,
before the `transaction()` at `:623`).

Why it matters. On a full disk an answer shows in the chat and is not kept, with no notice in the
chat. A failure or crash during a fork leaves a listed fork with part of its branch. A crash between
`delete`'s two updates leaves a tombstoned chat whose text is still in the search index until the
next compaction.

Direction. A `put_node` that takes a transaction, so fork, delete and interrupted-turn repair are
each one; surface a failed answer write as a notice in the chat.

### Q-30 (low) `follow_engine` is a 276-line state machine made of booleans

Verified.

Evidence. `chats.rs:2349-2624`. Its flags: `replied`, `failed_resume`, `stopped`, `said_error`,
`gave_up` (`:2373-2381`), `resumed` and `keep`, `ours` (`:2404`), `starting_since`, `changed`, plus
`live.quiet` and `live.stopping`. The end-of-task logic reads six of them (`:2565-2622`), for example
(`:2588`): `if was_ours && resumed && !replied && !gave_up {`. Every half second (`:2387`) it calls
`me.info(&session)` (`:2498`), the probe from Q-14. Related: per-chat send locks are cleaned up by
reference count against a holder count the caller supplies (`:256-261`,
`if sending.get(id).is_some_and(|l| Arc::strong_count(l) <= held)`; called with `2` at `:246`,
`:482`, `:509`). `busy_check` has two identical arms (`:1680-1693`). A streaming answer is rewritten
whole every two seconds (`FLUSH`, `:54`; `UPDATE node SET body = ?2, text = ?3`, `chat.rs:1552`).

Why it matters. The combinations are hard to check by reading, and this is the code that decides
whether an agent process is stopped, retried or left. The lock cleanup is correct as written and
breaks quietly if anything else ever clones a lock.

Direction. An explicit enum for how the engine ended; a lock map that removes entries by an owned
guard.

### Q-31 (low) `branch` has no guard against a parent cycle

Verified.

Evidence. `ptyro-store/src/chat.rs:1685-1693`:
```sql
WITH RECURSIVE up(id, parent, body, at_ms, depth) AS (
  SELECT id, parent, body, at_ms, 0 FROM node WHERE id = ?1 AND chat = ?2
  UNION ALL
  SELECT n.id, n.parent, n.body, n.at_ms, up.depth + 1 FROM node n JOIN up ON n.id = up.parent
)
```
`tip` is bounded (`:979`, `for _ in 0..100_000`) and so is `agent_of` (`:1643`, `for _ in 0..64`).
Node parents also come from imported files (`import_batch`, not read).

Why it matters. A cycle (a damaged database, a bad import) would spin the one `chat.db` thread for
ever, and with it every chat operation. Unverified whether an import can produce one.

Direction. A depth limit in the query.

Import (`chats/import.rs`), from its outline only: reads are bounded (`MAX_LINE` 4 MiB at `:36`,
`MAX_WHOLE` 64 MiB at `:40`), parsing runs in `spawn_blocking` (`:950`, `:1008`), and one import runs
at a time (`:912`). `chat_import` is 274 lines.

## Runs (`ptyro-daemon/src/runs.rs`), engine only

What holds: events and the state projection are written in one transaction
(`ptyro-store/src/runs.rs:114-145`); endings are final (`runs.rs:172-187`); a missing projection is
rebuilt from the log (`:328-340`); gates are reused after a restart (`:1702-1707`); a sleep step
persists its deadline (`:2030-2048`); a command step runs in its own process group with
`kill_on_drop` and a guard that kills the group when the step is aborted or times out
(`verify.rs:628-634`, `:674-682`); answering a gate is serialized (`runs.rs:1842`).

### Q-32 (med) After a live upgrade, a command step that is still running is marked crashed and may be started again beside itself

Verified by reading; the upgrade path was not run.

Evidence. `recover` (`runs.rs:420-442`):
```rust
Some(Kind::Run) => {
    self.emit(id, vec![RunEvent::AttemptEnded {
        step: sid.clone(),
        outcome: "crashed".into(),
        error: Some("the daemon stopped while it ran".into()),
        ..
    }]);
    // A retry starts a new attempt: run it.
    if self.after_attempt(..).await {
        self.spawn_step(id, sid, false);
    }
}
```
The step's command is a child started in `run_to_log` with `.kill_on_drop(true)` and a `GroupGuard`
(`verify.rs:628-634`); both act only when the future is dropped. A live upgrade replaces the process
image (`handoff.rs:386`: "Nothing is closed on the way out: every console, program and job is the new
daemon's"), and `handoff.rs` has no check for run steps in flight.

Why it matters. That handling is right after a crash. After an upgrade the command is still this
process's child and still running; a retry then runs the same command a second time against the same
worktree and log, and nothing stops or reaps the first.

Direction. Refuse an upgrade while a command step runs, or kill those groups before the handover.

### Q-33 (low) `Intent` events are written and never read

Verified.

Evidence. `exec_apply` emits an intent before each action (`runs.rs:1906`) and an outcome after
(`:1997`). Resuming looks only at the `applied` output (`:1894-1905`, "after a restart: already done
(the outcome event says so)"). The state fold ignores it (`ptyro-run/src/state.rs:565`):
`RunEvent::Intent { .. } | RunEvent::Outcome { .. } | RunEvent::Note { .. } => {}`. `recover`
(`runs.rs:409-494`) does not look for an intent without an outcome.

Why it matters. A stop between an action taking effect and its outcome being stored repeats the
action: harmless for commit and push, a failed step for `pr` (the PR exists). The outcome write is
fire-and-forget (Q-10), which widens the window.

Direction. On recovery, an intent with no outcome means "check whether it happened" for that action.

### Q-34 (low) Every event rewrites and rebroadcasts the whole run

Verified.

Evidence. `emit` (`runs.rs:197-203`):
```rust
(Self::row(e), rows, self.info_of(e, &runs), Arc::clone(&e.wake))
...
store.run_append(row, rows);
let _ = self.events.send(DomainEvent::RunUpdated { run: info });
```
`run_append` stores `row.state.to_string()` each time (`ptyro-store/src/runs.rs:128`). `start_runs`
loads up to 500 active runs (`runs.rs:320`) and 200 finished ones (`:383`); nothing removes a run
from `self.runs`. The wall budget compares `now_ms()` with `created_ms` (`:515-516`).

Why it matters. For a change set the state carries every contract revision's text and diff and every
checkpoint, so the cost per event grows with the set's history. More than 500 active runs are
silently not resumed. The wall-clock budget counts time the daemon was down.

Direction. Write the projection on a timer or on step boundaries, send deltas, and log when the 500
limit cuts something off.

### Q-35 (low) `GroupGuard` signals a pid after it was reaped

Verified.

Evidence. `verify.rs:634` (`let _group = GroupGuard(child.id());`) and `:676-681`:
```rust
impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let _ = ptyro_pty::signal_group(pid, ptyro_pty::Signal::KILL);
        }
    }
}
```
On a normal exit `child.wait()` has already returned (`:639-641`) when the guard drops.

Why it matters. That is wanted when background children remain in the group. When none do, the group
id is free and could belong to a new process. Sessions avoid exactly this by not reaping the leader
first (H2.7, `ptyro-pty/src/unix.rs:87-104`); checks and command steps do not. The odds are small;
the inconsistency is the point.

Direction. Signal the group before reaping, as sessions do.

## Machines (`ptyro-daemon/src/machines.rs`)

### Q-36 (med) Two machines link only when their versions are identical strings

Verified.

Evidence. `run_link` (`machines.rs:548-555`):
```rust
if client.daemon_version != ours {
    let (error, fix) = version_fix(&name, &client.daemon_version, ours);
    self.set_link_state(&name, MachineState::Mismatch, Some(error), Some(fix));
    drop(client);
    // Until someone updates one of them: not worth trying often.
```
`add_machine` does the same (`:1373`). `ours` is `env!("CARGO_PKG_VERSION")` (`:503`). The
protocol's own version (`PROTO`, 1.39) plays no part.

Why it matters. The one place where two builds must interoperate has no compatibility at all: every
release breaks every link until both ends are updated. This may be the intended rule (I did not find
it stated as a decision in the parts of SPEC I read: unverified). It belongs with Q-11 and Q-15: the
protocol's version is not what decides compatibility anywhere.

Direction. Decide links on the protocol version, or record the exact-version rule in SPEC as a
decision.

### Q-37 (low) A link can hang while it looks online

Verified.

Evidence. `refresh_mirror` makes eight requests in a row with no timeout (`machines.rs:589-629`). It
is called at connect (`:559`) and inside one arm of `follow`'s `select!` (`:699-703`):
```rust
if matches!(event, DomainEvent::EventsLost { .. }) {
    if self.refresh_mirror(name, client).await.is_err() {
        return;
    }
```
The ping arm (`:712-717`), whose comment says "A machine that went to sleep never closes its end: a
request tells", is not polled while that awaits. `mirror_event` awaits notification delivery inline
(`:826`, `relay_alert`; `:860`, `self.deliver(&note, false).await`).

Why it matters. A machine that sleeps during a refresh leaves the link `Online` and stuck.

Direction. A timeout around the refresh; deliver alerts off the follow loop.

### Q-38 (low) Routing, state and error handling by text

Verified (counts by grep).

- `machine_for` serializes every request to JSON and walks it for strings that look like addresses
  (`machines.rs:901-903`: `let v = serde_json::to_value(req).unwrap_or_default();` …
  `let addrs = addresses(&v);`), for every request once any machine is linked.
- Errors are classified by substring: `e.message.contains("unauthorized")` (`machines.rs:527`),
  `e.message.contains("conflict")` (`changesets.rs:3016`); five such tests in the daemon.
- Adapter identity is compared as a string in 18 places (for example `chats.rs:2173`,
  `adapter == "codex" && mcp.is_none()`; `changesets.rs:879`, `aname == "claude"`), beside the
  adapter trait that exists to hold such differences.
- Step outcomes are strings (`runs.rs:801`, `res.outcome == "succeeded"`; `:850-853`); so are message
  kinds and priorities (`changesets.rs:1162-1166`, `:1216`), gate steps by prefix
  (`starts_with("contract-r")`, `"ask-"`, and `devices.rs:125-127`, where a device's permission to
  answer a gate depends on `step.starts_with("push-") || step.starts_with("land-")`), conversation
  origins (`"new"`, `"seed"`, `chats.rs:2152`) and `self.config().chat.notify == "all"`
  (`chats.rs:2648`).
- The ledger hides mailbox notes by what their text contains (`changesets.rs:1090-1095`:
  `text.contains(" → ") && text.contains("): ")`).

Why it matters. A renamed step, a reworded error or a new adapter changes behavior with no compile
error; one of these strings gates what a paired device may approve.

Direction. Enums for outcomes, kinds and gate purposes; error codes instead of message text;
addresses declared per request instead of found by walking JSON.

## Worktrees (`ptyro-daemon/src/worktrees.rs`): notes, no new ids

`create_worktree` is 375 lines and does eleven things in order (register an unknown repository, read
an issue, name, resolve the profile, snapshot changes to take, add the worktree, move the changes,
copy files, allocate ports, run setup, start or queue the agent). Its error contract is Q-23.
`remove_worktree` orders its steps safely for the non-forced case (git itself refuses a dirty
worktree).

- More synchronous git on async threads (first pass, Q-05): `Git::new().main_remote(&p.root)`
  (`worktrees.rs:318`) and `Git::new().status(&path, base.as_deref())` (`:501`);
  `ProjectConfig::load` reads files inline (`:247`, `:490`).
- Taking the main checkout's changes ends in `git reset -q --hard` and `git clean -q -fd` on the
  user's checkout (`:478-479`) after a re-check (`:470-477`). The window between the re-check and the
  reset is small and cannot be closed from outside git; a copy of the earlier snapshot is kept under
  `refs/ptyro/taken/<name>` (`:461`). Worth stating in SPEC as a known limit.
- Function-local `static` mutexes serialize port allocation (`:507`) and gate answers
  (`runs.rs:1842`): fine for one daemon per process, shared by every daemon a test starts in one
  process.
- Quiet-hours handling is written four times (`notifier.rs:129`, `runs.rs:697-705`,
  `machines.rs:840-847`, `review.rs:588`).

## Windows (`ptyro-pty/src/windows.rs`): sampled, no new ids

The ownership pattern is sound where I looked: `Owned` closes once (`windows.rs:83-95`); handles
given to a `File` are forgotten immediately after (`:555-559`, `:860-862`); a handle value read from
a handover image is checked to be open, and a pipe where it should be, before it is owned
(`image_handle`, from `:746`). One leak on an error path: `duplicate_to` copies seven handles into
the next daemon one at a time with `?` (`:713-724`), so a failure part-way leaves the earlier copies
open in that process with nobody holding their values; they go when that process exits, which is
what a failed handover leads to.

## Longest functions in the files read this pass

| Function | Location | Lines |
|---|---|---|
| `handle_chat` (not read) | `chats.rs:703` | 512 |
| `create_worktree` | `worktrees.rs:228` | 375 |
| `cs_new` | `changesets.rs:483` | 348 |
| `cs_tick` | `changesets.rs:1740` | 279 |
| `follow_engine` | `chats.rs:2349` | 276 |
| `chat_import` | `chats/import.rs:879` | 274 |
| `overlay_files` | `changesets.rs:3590` | 209 |
| `cs_op` | `changesets.rs:258` | 202 |
| `start_engine` | `chats.rs:2116` | 201 |
| `close_interrupted` | `ptyro-store/src/chat.rs:678` | 157 |
| `exec_apply` | `runs.rs:1883` | 144 |
| `remove_worktree` | `worktrees.rs:701` | 142 |

`cs_tick` is the clearest candidate to split: it records contract revisions, answers two kinds of
gate, watches members, resumes agents for mail, tracks the lead's turns, scans for outside merges and
finishes the run, in one function run every 1.5 seconds (`changesets.rs:1727`).

## How this changes the first pass's picture

The first pass found the hot paths careful and put the weight on structure. This pass finds the
orchestration layer (change sets, chat engines, upgrade recovery) to be where correctness is thinner:
multi-step operations report failure after part of the work is done (Q-23, Q-26, Q-29); state is
derived and written across awaits with no lock (Q-25); in-memory jobs are not recovered after a
restart or an upgrade (Q-22, Q-32). The run engine itself is the exception: its log, projection and
gate handling are sound.

A sixth refactor belongs on the first file's list: a per-set (and per-chat) operation lock, plus a
rule that a multi-step operation either records what it finished or undoes it.
