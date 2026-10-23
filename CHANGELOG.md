# Changelog

Generated from git history by `make changelog` (scripts/changelog.sh); edit commit messages, not this file.

## Unreleased

### Features

- feat: UX1.3 opt-in summaries, a line from a small model after each turn (14c227b)
- feat: TUI prompts check input as it's typed instead of failing after ⏎ (cb13820)

### Fixes

- fix: status selectors match the status ptyro shows, not only self-reports (756c003)
- fix: Codex sessions show their task and last reply again (hooks trusted on Codex 0.164) (c504202)
- fix: a session started in a subdirectory of a project starts there, not at the worktree root (1335cb1)
- fix: resume a session that suspends itself on Ctrl-Z, and say so (d39aa8a)
- fix: removing a session closes the panes showing it instead of leaving them empty (8dcae2f)
- fix: a worker's turn summary is its own reply, not the screen's last line (92a8ef3)
- fix: changeset-demo waits for member turns to end before restoring the set checkpoint (346e6c0)
- fix: the collision radar no longer warns a finished fan-out's candidates about each other; deflake m10/m11 (deaeac2)

### Documentation

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

