# Bootstrap: install the agents and tools, and keep them current

Status: design, approved for planning once the owner reads it (2026-11-06). ROADMAP: **BOOT**.
RESEARCH: R-BOOT-01.

## Intent

Install ptyro from its binary on a new machine, and ptyro sets up everything else: the coding
agents, the tools they and the user need, and the logins. The owner's words: "the goal is to
enable us to just install ptyro from binary, then bootstrap everything else on a new
machine". The same choices stay available afterwards ("add agents"). Once installed, ptyro keeps
the agents and itself current: it notices a newer version and offers the update in one step.

Why: today a missing agent is a dead end with a command to copy ("claude isn't installed:
`irm https://claude.ai/install.ps1 | iex`"), prerequisites like Node.js are the user's problem,
logins are separate chores, and nothing says an agent is out of date. ptyro's rule is "out of
the box comes first … errors that offer the fix instead of naming a command to type"
(AGENTS.md); setup is where that matters most.

## Goals

1. **A setup checklist on first run** (owner: one checklist, not a step-by-step wizard). One
   screen, three groups:
   - **Agents:** Claude Code, Codex, Antigravity (`agy`), OpenCode, and Gemini CLI labelled
     "for Gemini API-key and enterprise accounts (others: Antigravity)". The owner keeps Gemini
     CLI in the menu with that label. [V 2026-11-06: Google moved free, Google One, AI Pro and
     Ultra users from Gemini CLI to Antigravity CLI on 2026-06-18. Gemini CLI still serves
     paid Gemini API keys, and some enterprise licences. Google's pages disagree on exactly who
     kept it.]
   - **Tools:** git and the GitHub CLI (owner: yes, with their logins), and each agent's
     prerequisites (Node.js for the npm agents), ticked automatically when something picked
     needs them.
   - **Already here:** what is installed, shown as done.
   One action installs everything ticked, then walks through the logins.
2. **The same checklist any time later**: to add agents or tools ("and anytime further on, to
   install additional agents"), in the CLI, the TUI and the web (SPEC §1.3, AGENTS.md: every
   feature in all three).
3. **Prerequisites installed too, after asking** (owner). Admin rights or a system package
   manager are used where that is the normal way on the platform, **only after asking**
   (owner).
4. **Windows: WSL, Windows, or both.** Each agent can be installed into the WSL distro, onto
   Windows itself, or both (owner: "offer the option for either (or both)").
   - **WSL is the user's to bring** (owner: "WSL is pretty complicated, leave that to the
     user"). Setup never runs `wsl --install`, installs a distro, or puts ptyro inside one.
   - **On a machine without a usable WSL** (no WSL, no distro, or no ptyro in the distro),
     setup offers the Windows side only.
   - **The WSL side can be added later:** the same checklist, opened again once WSL and its
     ptyro are there, offers the agents and tools inside the distro.
5. **Logins, one account each** (owner): each installed agent and `gh` is logged in once, to
   the default profile. Setup ends by saying how to add more accounts later (profiles, each
   with its own login: `ptyro profile login`).
6. **Out of date is noticed and offered** (owner): at start-up and in `doctor`, an installed
   agent with a newer version is reported with a one-step update, and the command shown. It
   is never updated silently.
7. **ptyro keeps itself current** (owner): when a newer ptyro release exists, ptyro downloads it,
   verifies it, and offers to install it and live-upgrade the daemon (`daemon upgrade`: sessions
   carry on).
8. **Version checks at most daily** (owner): the newest versions are fetched at most once a
   day, remembered between, and fetched afresh by `doctor`.
9. **Agents installed another way are offered a move** (owner): an agent installed some way
   other than its vendor's recommended installer (Claude Code through npm or Homebrew) is
   offered a move to the recommended one.

## Non-goals

- Updating git, the GitHub CLI or Node.js (owner: the daily check covers agents and ptyro
  only). They stay on their own updaters. A missing one is still installable from the checklist.
- More than one account per agent during setup (owner). Profiles are how more get added,
  afterwards.
- Picking or configuring models, plans or API keys for the user.
- Installing agents on another linked machine (MM1) from this one. Each machine runs its own
  setup.

## Anti-goals (a failure even if it works)

- **Anything silent with admin rights or the network:** no elevation, no system package
  manager, no install and no update without the user's yes.
- **Credentials passing through ptyro.** Logins are each tool's own flow (browser or device
  code). ptyro starts them and watches for success; it never reads, stores or forwards a token.
- **A broken machine on failure.** A failed install leaves the rest installed and says what went
  wrong with the fix offered; a failed update leaves the old version working.
- **Fighting the user's package manager.** ptyro installed by Homebrew is updated through
  Homebrew (my call, below), and a move between installers (goal 9) is offered, never done
  unasked.
- **Needing the network to run.** Offline, every check fails quietly and is tried again the
  next day; nothing blocks start-up.
- **Guessing versions or install methods.** Each agent's install, update, version and latest
  version come from a table checked against the vendor's documentation, with dates
  (RESEARCH segment G). An agent whose way isn't verified says so and offers the vendor's page.

## Constraints

- **Parity** (AGENTS.md, SPEC §1.3): the checklist and the update offers exist in the CLI, the
  TUI and the web, with rows in `docs/parity.md`. `make parity` must pass.
- **The one parity exception** (owner, 2026-11-06: "this is setup, and I'm fine with an
  exception"): installs that need admin rights run only from the TUI or the CLI, on the machine
  itself. A sudo password or a UAC prompt never goes through the web, or over the network.
  - The web still shows the checklist, installs what needs no admin, and offers every update.
  - For an admin step, the web shows the command to run on the machine, and the TUI and CLI
    paths that do it.
  - `docs/parity.md` records this as a waiver with that reason, in the web cell of those rows.
- **Settings** (AGENTS.md): every new option gets its schema entry in
  `crates/ptyro-core/src/settings.rs` (section, kind, default, help) and appears in both
  settings screens.
- **Database rule** (AGENTS.md): metadata only. The remembered "latest versions" are a small
  state file (`<state>/versions.json`), not database content.
- **Make targets** for anything someone runs (a spike, a check), in the README.
- **Platforms:** Linux, macOS, WSL, Windows (x64, ARM64). Windows commands run without a
  window flashing (SPEC §20.12). Smart App Control may refuse an unsigned ptyro; installers
  ptyro starts are the vendors' own (signed).
- **ptyro's own update path** reuses the release's verification: `SHA256SUMS` and the signed
  build provenance (`gh attestation verify` where `gh` is present), as `install.sh` and
  `install.ps1` already do (SPEC §18.2 "Releases").

## Decided

These are the owner's decisions:

| Topic | Decision |
|---|---|
| Shape | One checklist screen (option A), sensible picks ticked |
| When | First run of a fresh install; any time later |
| Agents | Claude Code, Codex, Antigravity, OpenCode, Gemini CLI (labelled for API-key/enterprise) |
| Tools | git, GitHub CLI, agents' prerequisites (Node.js) |
| Prerequisites | Installed too, after asking |
| Admin / system package managers | Allowed, after asking |
| Windows | Per agent: WSL, Windows, or both |
| Logins | One account each (agents and `gh`); remind how to add profiles with logins |
| Out of date | Reported at start-up and in `doctor`, with a one-step update and the command; never silent |
| Version checks | At most once a day, remembered; `doctor` checks afresh |
| Checked | Agents and ptyro itself (not git, gh, Node.js) |
| ptyro's own update | Download and verify, then offer to install and live-upgrade |
| Other installs | Offer to move to the vendor's recommended installer |

## My calls (the owner can overrule)

- **Default ticks:** Claude Code, Codex, git, GitHub CLI (and Node.js, because Codex needs it).
  Nothing ticked that's already installed.
- **Install methods**, one per agent and platform, the vendor's recommended one (verified when
  built, RESEARCH G):
  - Claude Code: its native installer.
  - Codex, OpenCode, Gemini CLI: npm global, into a user-owned prefix (no `sudo npm`).
  - Antigravity: its own installer.
  - git, GitHub CLI, Node.js: the platform's package manager (winget on Windows, Homebrew on
    macOS, apt or dnf on Linux and WSL), or the vendor's installer where there is none.
- **Where the checklist lives:**
  - It is the first part of the onboarding screen that already opens on a fresh install
    (`crates/ptyro-tui/src/onboard.rs`, UX1.19), not a second screen beside it. `ptyro init`,
    `ptyro setup` and the palette's `first.run` reopen it; the health badge's fixes do too.
  - The web has it under Settings, and as a first-run banner when no agent is installed.
  - The CLI has `ptyro setup` (the checklist, interactive) and `ptyro install <name>…`.
  - **One plan engine** in the daemon or a shared crate serves all three, so they can't drift.
- **Unattended form:** `ptyro setup --yes --agents claude,codex --tools git,gh [--where wsl|windows|both]`
  for scripts and fresh-VM provisioning. It asks nothing; elevation it can't do without a prompt
  is reported, with the command.
- **Install output** is shown live (a split in the TUI, a log pane in the web, the terminal in
  the CLI). Each item ends ✓ or ✗ with the fix; one failure doesn't stop the rest.
- **Node.js:**
  - If the platform's package manager has a new enough Node (the npm agents' minimum,
    recorded in the version table), use that.
  - If not (Ubuntu 24.04's `nodejs` is 18.x), install the official nodejs.org build into a
    ptyro-managed folder under the user's home. That needs no admin rights and no `sudo npm`,
    and the npm global prefix goes there too.
  - That folder goes on `PATH` through ptyro's existing shell-init line (`ptyro shell-init`),
    and for ptyro's own sessions through the daemon's environment.
- **A move between installers** (goal 9): Claude Code installed through npm or Homebrew is
  offered the move to its native installer, as the owner said. For other agents, the version
  table names the recommended method; any other is offered the move. The offer can be
  dismissed per agent (it then updates the way it was installed).
- **An agent in use when it updates:**
  - The update goes ahead, and ptyro says which sessions still run the old version until they
    restart.
  - On Windows, if a running program can't be replaced, it says so and offers to retry once
    those sessions end.
- **ptyro's own update with WSL:** on Windows with WSL's daemon, the update offers both halves,
  `ptyro.exe` and the ptyro inside the distro, and upgrades WSL's daemon live.
- **ptyro installed by a package manager** (Homebrew, a future winget/scoop): its update goes
  through that manager. The download-and-verify path is for release-binary installs
  (`install.sh` / `install.ps1` / a binary in `~/.local/bin` or `%LOCALAPPDATA%\Programs`).
- **Settings** (schema entries, both settings screens):
  - `updates.check = "daily" | "off"` (default `daily`), and whether ptyro checks for itself.
  - Per-agent update offers can be muted (`updates.ignore = ["codex@0.170.0"]`: one version
    skipped).
- **Where offers show up:** the TUI's health badge (the existing doctor findings with a fix
  key), a one-line note on `ptyro` start-up (once per new version, not every start), the web's
  Health page, and `doctor`.
- **`ptyro uninstall` leaves installed agents and tools alone** (they're the user's).
- **The stale suggestions** in `doctor.rs` and `chats.rs` that put Gemini CLI first become "open
  setup" offers that name Antigravity for Google.

## Left to the builder

- The exact plan model: an item's states (missing, installed, outdated, other-installer),
  dependencies (Node.js before npm agents), elevation batching (ask once for all admin steps on
  a platform where that's possible).
- How a login is driven and detected per tool:
  - `gh auth login --web`, `claude` first run or `claude /login`, `codex login`, `agy` login.
  - Logins run in a session (so a browser or device code shows where the user is) and are
    confirmed by each tool's own status command.
- Where the version table lives and how it is tested without the network:
  - Per agent: install, update, current version and latest version sources. Examples: the
    npm registry for npm packages, Claude's release manifest, Antigravity's release feed,
    GitHub releases for ptyro.
  - Recorded fixtures, as `fake-gh` does.
- The protocol additions (requests for the plan, run, progress events) and their version bump.
- WSL and Windows side by side: running the WSL half through `wsl.exe` from Windows (or the
  daemon in WSL), and reporting both halves on one checklist.
- **Where the plan engine runs:** on a fresh Windows machine there may be no daemon yet. The
  checklist must work from the CLI and TUI before any daemon exists, and from the daemon for
  the web.

## Done when

On a fresh Windows 11 machine and a fresh Ubuntu (and a macOS) with only ptyro:

1. The first `ptyro` shows the checklist. (On Windows without WSL, it offers the Windows side
   only.) Ticking Claude Code, Codex, git and GitHub CLI and
   pressing Enter installs them, Node.js included (one admin prompt where needed), then walks
   through the logins. Afterwards, a Claude and a Codex session start and work.
2. Later, `ptyro install agy` (or the TUI palette, or the web Settings) adds Antigravity the same
   way.
3. With an older Claude Code installed, `ptyro` start-up and `doctor` say a newer one is out,
   and the update fix updates it. With an older ptyro, the newer release is downloaded,
   verified, installed and the daemon upgraded live, sessions intact.
4. Offline, nothing above blocks or errors loudly. With `updates.check = "off"`, nothing is
   fetched.
5. `make parity` passes, and the new options are in both settings screens.
