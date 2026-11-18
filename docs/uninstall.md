<img src="logos/ptyro-icon.png" alt="ptyro" width="72" align="right">


# Uninstalling

`ptyro uninstall` takes ptyro off the computer. It says first what it does, and it never touches
your repositories.

## `ptyro uninstall`

```sh
ptyro uninstall --dry-run          # the plan, nothing removed
ptyro uninstall                    # the plan, then [y/N]
```

The install scripts hand over to it, passing their remaining arguments along:
`curl -fsSL https://github.com/nealhardesty/ptyro/releases/latest/download/install.sh | sh -s -- --uninstall`
If the installed binary won't run, it removes only what it installed (the binary and its
completions), and `--dry-run` only says so.

It stops the daemon and every session, then removes:

- **ptyro's directories**: config, data (history, chats, managed profiles and the agent
  logins in them; notes left from the old scratchpad, `scratchpad.md`, are named in the
  plan), state, and the runtime directory, wherever `ptyro daemon status` says they are
  (`PTYRO_HOME`, `XDG_*`), with their sizes;
- **its worktrees**, through `git worktree remove` so the repository forgets them (the branches
  stay). Each is checked with git, not the daemon's last look: uncommitted changes are listed and
  **will be lost**; commits not pushed anywhere are noted (they are on the branch, which stays);
- **the login service** (a launchd agent or systemd user unit);
- **the lines ptyro added to your shell's rc files**: only the exact lines `ptyro setup` and the
  mend list write (`source <(ptyro completions zsh)`, `eval "$(ptyro shell-init zsh)"`, their
  bash, fish and PowerShell forms, the last in `$PROFILE`) with the comment ptyro put above one. Anything else that mentions ptyro
  stays and is listed. When `ptyro setup` installed tools into ptyro's tools folder, the
  `shell-init` line was what put them on `PATH`: a plain `PATH` line for that folder takes its
  place (the plan shows it), so they keep working;
- **a Tailscale mapping** (`tailscale serve` on :443 to ptyro's port) when `[remote] tailscale`
  made one;
- **the binary**, when it was put there by hand (`~/.local/bin`, `/usr/local/bin`, a path you
  chose). A Homebrew install says `brew uninstall ptyro`; a package manager's file says to use it;
  `~/.cargo/bin` runs `cargo uninstall ptyro`; a build in a `target/` directory is never removed.

**What stays, always:** the agents and tools `ptyro setup` installed, its tools folder
(`~/.local/share/ptyro-tools`); your repositories, their branches, `.ptyro.toml` and `.ptyro.local.toml`,
the lines ptyro added to `.git/info/exclude`, and, unless you say `--purge-refs`, the snapshots
under `refs/ptyro/` in your repositories (checkpoints, and work an older ptyro archived or took
over). They stay as ordinary git refs (`git for-each-ref refs/ptyro`), so the work can be
recovered with git. So does what an older ptyro installed into Gemini CLI's and Antigravity's own
folders (ptyro's Gemini extension, its entries in Antigravity's `hooks.json`; both removed
2026-11-17): the plan lists each one found, with the command that takes it out.

### Confirmation

It asks `Uninstall ptyro? [y/N]`. If any worktree has uncommitted work it asks you to **type
`delete`** instead, and `--yes` does not stand in for that: a script that really means it passes
`--delete-work`. Not in a terminal and without `--yes`, it prints the plan and refuses. Inside a
ptyro session it refuses too: stopping the daemon would stop the uninstall. Detach (`Ctrl-Q d`)
and run it from a plain terminal.

### Flags

| Flag | Does |
|---|---|
| `--dry-run` | the plan only (also `--json`) |
| `--yes`, `--delete-work` | skip `[y/N]`; also skip typing `delete` |
| `--keep-config` / `--keep-data` | keep the config / the data directory (and with it the worktrees) |
| `--keep-binary` | leave the program |
| `--purge-refs` | also delete `refs/ptyro/*` in your repositories |
| `--json` | the plan and each step's result, for scripts |

It runs its steps in an order that is safe to run again after one fails: a failing step is
reported (`✗`), the rest go on where that is safe (nothing is deleted under a daemon that is still
running), and the exit status is 1.

### With `PTYRO_HOME` set

A sandbox or a test: only that home is reset. The login service, rc files,
Tailscale mapping and the binary belong to the machine and are left alone. `make reset` is exactly
`ptyro uninstall --keep-binary` (`YES=1` adds `--yes`).

### On the TUI and the web

- **TUI:** `app.uninstall` in the palette (`C-q :`) runs the command in a split, where the
  question is asked. An uninstall ends the TUI with the daemon.
- **Web:** More → Daemon → *Uninstall…* shows the plan and the command to type on the computer; it can't do it
  itself, because it would end the server the page comes from.

### Reinstalling

The install script (`curl -fsSL https://github.com/nealhardesty/ptyro/releases/latest/download/install.sh | sh`)
and the release tarballs put the binary back; `ptyro` then starts from
the first run. To keep your history, copy ptyro's config and data directories (`ptyro about`
says where they are) somewhere first, with the daemon stopped, and put them back before you start
it again.
