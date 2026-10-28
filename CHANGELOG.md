# Changelog

Generated from git history by `make changelog` (scripts/changelog.sh); edit commit messages, not this file.

## v0.0.15 (2026-10-28)

### Fixes

- fix: a client transport without its pipes is an error, not a panic (the unwrap ratchet failed CI since MM1) (6dbd15a)

### Other

- chore: version 0.0.15 (4e89590)

## v0.0.14 (2026-10-27)

### Features

- feat: a new worktree's branch is named like the worktree, without the ptyro/ prefix (UX3.6) (9e81548)

### Fixes

- fix(H2.7): remote.public_url, notifications.callback.public_url and callback.listen are checked when the config loads; serve says which half of a certificate pair is missing instead of quietly using its own (44bca25)
- fix(H2.7): the Windows 'unsupported' message lives in ptyro-core, which every crate builds on, so it comes first (ddf94a5)
- fix(H2.7): PTYRO_TEST_* knobs and the fake-gh command exist only in debug builds; send --raw's escapes move to ptyro-core, so a release links only the fake agent the tour and agent check use (f4551a5)
- fix(H2.7): keys typed while the TUI asks the terminal for its colours are replayed, not lost (bcd71ea)
- fix(H2.7): raw bytes (send --raw) wait for a prompt being typed instead of landing between its text and its Enter (708b9fa)
- fix(H2.7): an editor the daemon starts on the computer is reaped when it exits, not left a zombie (7a7836d)
- fix(H2.7): serve.log moves aside to serve.log.1 at 5 MiB, so a long-lived web server can't fill the disk (a741e76)
- fix(H2.7): cs rm calls off a push or land waiting at a gate (its gate closes, cancelled) instead of refusing as busy for up to a day (d1d1173)
- fix(H2.7): a renamed workspace keeps its saved view, the TUI's last-workspace note and quick new's memory; a removed one's projects go to main instead of nowhere (2b97373)
- fix(H2.7): a live upgrade carries PTY masters, pipes and daemon.lock over a socket, so the lock is never let go and a child spawned meanwhile inherits none of them (the listener, which macOS can't carry, is made inheritable only at the exec) (b7068c7)
- fix(H2.7): a session's leader stays a zombie until its group is empty, so a late signal to the group can't reach a process given the recycled id (81d74ce)
- fix(H2.7): a session's scrollback is capped at 4 million cells, so a wide one keeps fewer lines (was lines × width, up to a billion cells) (c471dec)
- fix(H2.7): a browser's WebSocket messages are capped at 4.25 MiB, before login too (was 16 MiB); the web app says so instead of sending one over (f871187)

### Documentation

- docs(H2.7): ROADMAP marks it done; SPEC notes the carried descriptors, the zombie leader, the client message limit, serve.log rotation, the scrollback cap and raw sends waiting (e15fa40)
- docs: AGENTS.md project rules: no co-authored lines, CLI/TUI/web parity is required, bump on every feature (71db59e)
- docs: MM1.0 many machines decided: workspaces on other machines, symmetric, mirrored, same version (SPEC §16.4b) (1fd95bb)

### Other

- release: v0.0.14 (45d7078)
- chore: version 0.0.14 (3ce179e)
- MM1: other machines' workspaces beside yours, in one sidebar, one CLI and one phone app (v0.0.13) (6c3067e)

## v0.0.12 (2026-10-27)

### Features

- feat: UX3 daily flow: full-sized panels, splits only on request, quick new, created means shown (v0.0.12) (55b300d)

### Fixes

- fix: wait up to 5 s for a worker's reply before the lead's summary (flaky on loaded CI runners) (aab80eb)

### Other

- release: v0.0.12 (5f97f91)

## v0.0.11 (2026-10-26)

### Features

- feat: addresses printed in a TUI pane are clickable links (OSC 8), as in the web terminal (878b49b)
- feat: the burst-of-agents logo is the project's primary logo: README, docs, favicon, PWA (incl. maskable) and apple-touch icons (3668ce1)

### Documentation

- style: cargo fmt (fmt-check failed on master) (329c35c)

### Other

- chore: bump to v0.0.11 (6ea836c)
- chore: bump to v0.0.11 (979de5b)

## v0.0.10 (2026-10-26)

### Other

- release: v0.0.10 (4c44af1)
- refactor: one scratchpad again: the per-project pads are removed (v0.0.10) (f7e06d0)

## v0.0.9 (2026-10-26)

### Fixes

- fix: Homebrew packaging verified (make brew-check), release gate unblocked (zstd 0.14.3, crypto-bigint 0.7.10) (v0.0.9) (49a2189)

### Performance

- perf: make install builds in seconds: an incremental `local` profile instead of a from-scratch cargo install (v0.0.9) (50dadbc)

### Other

- release: v0.0.9 (da1b8eb)

## v0.0.8 (2026-10-26)

### Features

- feat: UX2.3 a shell opens where it belongs: a drawer when the project is open, its own panel when not (f36c794)
- feat: a config panel that makes sense, per-project scratchpad, About, pushed state, safer pairing (v0.0.8) (31b7252)

### Other

- release: v0.0.8 (3befb8c)

## v0.0.7 (2026-10-25)

### Features

- feat: H2.1-H2.3 locks that survive panics, a release pipeline with gates, provenance and an SBOM, an installer, ptyro uninstall and backup; docs tested against the code (f5e1ef7)
- feat: CFG1 one configuration panel: the settings schema, the TUI panel, ptyro config set|get|list…, and the web's Settings, from one table (fc42415)
- feat: CFG1.1/1.2 the settings panel in the TUI and ptyro config list|get|set|unset|add|rm|rename|schema (c118ff4)
- feat: CFG1.0 the settings schema: one description per config key, comment-keeping edits, and a view of every value with where it comes from (608d158)
- feat: WEB1.8 a real terminal in the web: type into the session key by key, a phone key row, mouse, scrollback, selection and copy, find, links, resize-to-screen (77af177)

### Fixes

- fix: two tests that were racy on Linux (a freed inode number reused for the next socket file; landing a set while its agents were still reading the restore) (d8e7b52)
- fix: the tour's agent can commit on a machine with no git identity (its own HOME with a .gitconfig); make test runs every test binary (1afa192)
- fix: p256 0.14 (RUSTSEC-2026-0387 in crypto-bigint 0.5), and a test that assumed a stopped daemon leaves no socket file (81bc5b6)
- fix: a review of the whole codebase: ~150 verified findings fixed (security, robustness, correctness) and the rest recorded as H2 in the ROADMAP (ac73e5b)
- fix: a managed `ptyro serve` leaves when its daemon does, so a crashed daemon's server can't hold the port from the next one (46af6c3)

### Other

- release: v0.0.7 (a166398)
- test: two Linux-runner flakes: a reset under a refusal's reply, and the shell's own directory standing in for the OSC 7 one (cdd6db2)
- test: wait a moment before the first tap on New task (2b80d01)
- test: the browser test taps New task again on a fresh page if the first tap came before the projects loaded (7ed4a18)
- test: the PR baseline comment exists before the first poll (a slow runner saw none); the browser test prints the page when a scenario fails (5168b6b)
- test: H2.4 the tests that didn't exist: scope table, store migration and downgrade, handoff fixture, upgrade race, sinks, landing, gates, web e2e in headless Chrome, five fuzz targets (98bd7a9)
- chore: bump the version to 0.0.4 for CFG1 (4711bff)

## v0.0.3 (2026-10-24)

### Features

- feat: WEB1.9 the web app's server is a setting you can find: ptyro web on|off|status, daemon start --serve, an init question, TUI actions, More → Web app (cca739c)
- feat: NOTE1.1-NOTE1.6 the scratchpad in the TUI and the web, MCP tools, doctor, docs (85537c6)
- feat: web polish: session search, keys, offline banner, waiting count in the title; request-shape tests; fixes found by driving every page (08edc2f)
- feat: WEB1.6 settings and health on the web, and WEB1 is done (c67a0ae)
- feat: NOTE1.0, NOTE1.4 the scratchpad file, PTYRO_SCRATCHPAD, ptyro scratchpad, merging writes and ScratchChanged (3f24daf)
- feat: WEB1.5 runs and change sets on the web: runs, gates, fan-out compare and pick, recipes, change sets with their contract, checks, checkpoints and merge train (35cf24e)
- feat: WEB1.4 review and ship on the web: the diff with line comments for the agent, commits, commit, integrate, the PR and its checks, the review queue (e3d9f12)
- feat: WEB1.3 projects and worktrees on the web: the Projects tab, a worktree's page, workspaces, clean-up, archives, leases, radar, checkpoints (078aec9)
- feat: WEB1.2 sessions on the web: new task, the session menu, scrollback, events, replay, export, broadcast (2aa673a)
- feat: WEB1.7 ptyro serve speaks HTTPS by default, with a certificate it makes (77213ae)
- feat: WEB1.0, WEB1.1 the parity check covers the web, and every command is audited for it (8b4e407)
- feat: UX1.22 the tour, done for real: a task to clean-up with a pretend agent (fecac74)
- feat: UX1.26 several lines in ptyro's own boxes; a paste stays in the box (c7a77a1)
- feat: make reset starts ptyro over, after a summary and a yes (4d4426e)
- feat: quit ptyro from the TUI (C-q C-d): every session and the daemon stop, after a yes (263ffcb)
- feat: UX1.3 opt-in summaries, a line from a small model after each turn (14c227b)
- feat: TUI prompts check input as it's typed instead of failing after ⏎ (cb13820)

### Fixes

- fix: a daemon never deletes another daemon's socket (95a1ea7)
- fix: a turn is the agent's own turn end, counted once, not every working → idle (94c4bf4)
- fix: status selectors match the status ptyro shows, not only self-reports (756c003)
- fix: Codex sessions show their task and last reply again (hooks trusted on Codex 0.164) (c504202)
- fix: a session started in a subdirectory of a project starts there, not at the worktree root (1335cb1)
- fix: resume a session that suspends itself on Ctrl-Z, and say so (d39aa8a)
- fix: removing a session closes the panes showing it instead of leaving them empty (8dcae2f)
- fix: a worker's turn summary is its own reply, not the screen's last line (92a8ef3)
- fix: changeset-demo waits for member turns to end before restoring the set checkpoint (346e6c0)
- fix: the collision radar no longer warns a finished fan-out's candidates about each other; deflake m10/m11 (deaeac2)

### Documentation

- docs: roadmap NOTE1 (the scratchpad), R-UX-32 (80e7b60)
- docs: roadmap CFG1 (config panel), WEB1.8 (web terminal), WEB1.9 (serve with the daemon) (0fb90ab)
- docs: regenerate CHANGELOG.md (cf57911)
- docs: several lines in one prompt, in every client; roadmap UX1.26 and TLS by default (WEB1.7) (79b7e08)
- docs: the CLI, the TUI and the web are all first class; roadmap WEB1, web parity (83d635a)
- docs: regenerate CHANGELOG.md (43dfeef)
- docs: j/k and the sidebar numbers skip quick terminals (1eb53e5)
- docs: roadmap UX1.25, a quick terminal per worktree that one key toggles (6a3a8a9)
- docs: bring keys, config, CLI and spec docs in line with the code (7178a8d)
- docs: regenerate CHANGELOG.md (0905e9f)
- docs: roadmap UX1, out of the box, and make the TUI first class (c3293c7)
- docs: roadmap an agent started inside a shell session (R-AGT-09) (ebf6f77)
- docs: roadmap registering a project from the TUI when there is none (8659085)
- docs: roadmap sidebar resizing by drag and by keys (e90d0ba)
- docs: README install points at the prebuilt release tarballs; the Homebrew tap is not published yet (14ccc66)

### Other

- release: v0.0.3 (20e64d1)
- test: UX1's "Done when" end to end, and the fixes closing UX1 out (2cd1ec8)
- UX1.25: a quick terminal per worktree, in a drawer one key toggles (25ede4d)
- UX1.21: every CLI command has a TUI path, and the check is strict (785c15c)
- UX1.20: a health badge with a fix for each finding (168adf6)
- UX1.19: a first run in the TUI, and home (baad19c)
- UX1.10, UX1.11: work from a PR, a branch or your changes, and a first worktree's setup (4a5f116)
- UX1.12, UX1.13: finish a worktree, archive it when done, and a shell beside its agent (b650c01)
- UX1.14, UX1.15, UX1.16, UX1.18: learn the keys by using them (d19e23b)
- UX1.17: messages offer the fix instead of a command to type (7950068)
- UX1.9, UX1.24: say what to do, and everything has a name and a title (f2622a0)
- UX1.4: an agent started by hand in a shell says what it's doing, and can be adopted (4660f2c)
- UX1.5, UX1.6: the repository you're in is a project, and "no project" asks for one (c39ab10)
- UX1.7, UX1.8: resize the sidebar by dragging its edge, or with < and > (e6ab1c4)
- UX1.1, UX1.2: sidebar rows say what each session is doing (da00a98)
- UX1.0: make tui-parity holds every CLI command to a TUI path (9f0ef3f)
- UX1.23: a removed or stopped session's pane shows the next one (08ecae1)
- test: verify-claude checks that Claude's Stop carries the turn's reply (62f2fb4)

## v0.0.2 (2026-10-22)

### Features

- feat: Ctrl-Q and Ctrl-Space are both prefixes by default (6e35691)
- feat: the default prefix is Ctrl-Q (7fab60a)

### Fixes

- fix: make changelog works on macOS (a51029a)
- fix: a late prompt hook no longer resets a pending verify bounce; tokio 1.54.0 (yanked) -> 1.54.1 (78ffdd0)
- fix: path addresses find their project or worktree on macOS too (f6dd8a3)
- fix: a session started inside a worktree finds it on macOS (/var is /private/var) (40a5909)
- fix: config hot reload works when the config dir is reached through a symlink (macOS /var) (86eb579)
- fix: a PTY with no foreground group on macOS no longer panics the daemon (f2789ba)
- fix(test): the pairing claim helper sends a Host the serve check accepts (92d98eb)
- fix(test): keep the restart test's job alive long enough to compare pids (d91deb8)
- fix: SPEC and ROADMAP for the security review (M1-M3) (92b2902)
- fix: ptyro pair prints a one-time code, not the token (security review M2) (2e46232)
- fix: agents can't answer for a person by other routes (security review M1) (f8d741d)
- fix: background git runs no repository code (security review M3) (45156d2)
- fix: notify-send gets -- before agent text; document desktop_command placeholders as untrusted (ce2fe53)
- fix: sandbox demo server binds 127.0.0.1; say the sandbox isolates state, not security (585587b)
- fix: write the user config 0600 (dirs 0700); ptyro doctor warns when it's readable by others (5efeb6d)
- fix: an ACP "allow" never picks allow_always when the agent offers no allow_once (0375c5f)
- fix: catch dangerous agent args in every spelling (pairs, =value, short forms, one-string commands) (d3fc575)
- fix: honour [clipboard] allow_write, cap OSC 52 writes at 1 MiB, keep them out of the paste buffer (d85990a)
- fix: contract patches and review patches use private tempfile scratch space (42c2747)
- fix: serve host check uses contains (clippy) (a7717ee)
- fix: approve/deny link listener follows serve's safe-address rule unless insecure (b91f163)
- fix: serve checks Host against expected names; trusts X-Forwarded-For only from a configured proxy (bbcedea)
- fix: parse push endpoints strictly (no userinfo, fragments, backslashes) before the host allow-list (6ffc6f1)
- fix: start a structured session's driver only once the session is registered (7a29a2a)

### Documentation

- docs: regenerate CHANGELOG.md (cc3d84b)
- docs: regenerate CHANGELOG.md (90b7fa8)
- docs: regenerate CHANGELOG.md (d735931)
- docs: regenerate CHANGELOG.md (30eb521)

### Other

- release: v0.0.2 (2830a04)
- build: make bump sets the release version (6f2a902)
- Ignore local git config file in .gitignore- Add /.gitconfig entry to .gitignore so a repo-local git identity file is not accidentally committedCo-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com> (f0d84af)
- Fix rust install (no homebrew, real install) (8c85e6c)
- build: make rust-install installs rustup and the pinned toolchain (Linux, macOS) (6a21d6d)
- ci: time out hung jobs (e17e8e0)
- ptyro: initial public commit (f1a18fb)

