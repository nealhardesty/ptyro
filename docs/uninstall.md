# Backing up and uninstalling

`ptyro backup` saves ptyro's own state; `ptyro uninstall` takes ptyro off the computer. Both say
first what they do. Neither touches your repositories.

## `ptyro backup`

```sh
ptyro backup                       # ~/ptyro-backup-20261025-143007.tar.gz
ptyro backup -o /safe/place/p.tar.gz
ptyro backup --with-recordings     # every session's recording too (big; it holds what was on screen)
ptyro backup --with-ca             # the web app's certificate authority, with its private key
```

A private (mode 600) `.tar.gz` that never replaces a file that is already there. In it:

| Path in the archive | What |
|---|---|
| `config/config.toml` | your config, comments and all |
| `data/ptyro.db` | a consistent copy of the database (SQLite `VACUUM INTO`, safe while the daemon runs): sessions' history, projects, worktree records, paired devices (as token hashes), saved views, trust |
| `data/scratchpad.md`, `data/scratchpad.history/` | the scratchpad and its saved copies |
| `README.txt`, `manifest.json` | what this is, what is in it and what was left out, and how to put it back |

Left out, and said so when it is: the agents' login directories under ptyro-managed profiles (log
in again after a restore), the recordings (`--with-recordings`) and the certificate authority
(`--with-ca`).

**Putting it back** is by hand, with ptyro stopped (`ptyro daemon stop`): unpack it somewhere and
copy `config/config.toml` to where `ptyro config path` says, and `data/ptyro.db`,
`data/scratchpad.md` and `data/scratchpad.history/` into ptyro's data directory (usually
`~/.local/share/ptyro`). Worktrees are not in a backup: they are in your repositories, and
`ptyro wt restore` brings archived ones back from the snapshots there.

## `ptyro uninstall`

```sh
ptyro uninstall --dry-run          # the plan, nothing removed
ptyro uninstall                    # the plan, then [y/N]
ptyro uninstall --backup ~/ptyro.tar.gz   # a backup first; nothing is removed if it fails
```

It stops the daemon and every session, then removes:

- **ptyro's directories**: config, data (history, scratchpad, managed profiles and the agent
  logins in them), state, and the runtime directory, wherever `ptyro daemon status` says they
  are (`PTYRO_HOME`, `XDG_*`), with their sizes;
- **its worktrees**, through `git worktree remove` so the repository forgets them (the branches
  stay). Each is checked with git, not the daemon's last look: uncommitted changes are listed and
  **will be lost**; commits not pushed anywhere are noted (they are on the branch, which stays);
- **the login service** (a launchd agent or systemd user unit) and the **agent integrations**
  ptyro installed (`integrations install gemini|antigravity`);
- **the lines ptyro added to your shell's rc files**: only the exact lines `ptyro init` and the
  mend list write (`source <(ptyro completions zsh)`, `eval "$(ptyro shell-init zsh)"`, their
  bash and fish forms) with the comment ptyro put above one. Anything else that mentions ptyro
  stays and is listed;
- **a Tailscale mapping** (`tailscale serve` on :443 to ptyro's port) when `[remote] tailscale`
  made one;
- **the binary**, when it was put there by hand (`~/.local/bin`, `/usr/local/bin`, a path you
  chose). A Homebrew install says `brew uninstall ptyro`; a package manager's file says to use it;
  `~/.cargo/bin` runs `cargo uninstall ptyro`; a build in a `target/` directory is never removed.

**What stays, always:** your repositories, their branches, `.ptyro.toml` and `.ptyro.local.toml`,
the lines ptyro added to `.git/info/exclude`, and, unless you say `--purge-refs`, the snapshots
under `refs/ptyro/` in your repositories (archived and taken-over work). They stay as ordinary git refs
(`git for-each-ref refs/ptyro`), so the work can be recovered with git, but ptyro's own list of
archives goes with its database: `ptyro wt restore` can't find them after an uninstall, unless
you restore a backup.

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
| `--backup FILE` | make a backup first |
| `--purge-refs` | also delete `refs/ptyro/*` in your repositories |
| `--json` | the plan and each step's result, for scripts |

It runs its steps in an order that is safe to run again after one fails: a failing step is
reported (`✗`), the rest go on where that is safe (nothing is deleted under a daemon that is still
running), and the exit status is 1.

### With `PTYRO_HOME` set

A sandbox or a test: only that home is reset. The login service, rc files, integrations,
Tailscale mapping and the binary belong to the machine and are left alone. `make reset` is exactly
`ptyro uninstall --keep-binary` (`YES=1` adds `--yes`).

### On the TUI and the web

- **TUI:** `app.backup` and `app.uninstall` in the palette (`C-q :`) run the commands in a split,
  where the question is asked. An uninstall ends the TUI with the daemon.
- **Web:** More → Daemon → *Back up ptyro's state* writes the file on the computer and shows what
  is in it. *Uninstall…* shows the plan and the command to type on the computer; it can't do it
  itself, because it would end the server the page comes from.

### Reinstalling

`scripts/install.sh` (and the release tarballs) put the binary back; `ptyro` then starts from
the first run. Restore a backup by hand first if you want your history.
