# Changelog

Generated from git history by `make changelog` (scripts/changelog.sh); edit commit messages, not this file.

## Unreleased

### Features

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

- docs: regenerate CHANGELOG.md (d735931)
- docs: regenerate CHANGELOG.md (30eb521)

### Other

- build: make bump sets the release version (6f2a902)
- Ignore local git config file in .gitignore- Add /.gitconfig entry to .gitignore so a repo-local git identity file is not accidentally committedCo-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com> (f0d84af)
- Fix rust install (no homebrew, real install) (8c85e6c)
- build: make rust-install installs rustup and the pinned toolchain (Linux, macOS) (6a21d6d)
- ci: time out hung jobs (e17e8e0)
- ptyro: initial public commit (f1a18fb)

