# ptyro — Research

> A running record of **products in the same space as ptyro** and the **ideas they suggest**. Its
> job is to feed the funnel of candidate work into `ROADMAP.md`.

This file is the source of truth for **what others are doing and what we might learn from it**.

| Question | Where it lives |
|---|---|
| What ptyro *is* and how it works | `SPEC.md` |
| What ptyro will build | `ROADMAP.md` |
| How to use what exists | `README.md` |

Nothing here is a commitment. An idea becomes work only when it is promoted into `ROADMAP.md` with a task id.

**Last full sweep:** 2026-10-20 (ten parallel research passes, over 100 products and sources).
**Next sweep due:** 2027-01 (see §IV.4 for the cadence).

---

## Contents

- [How to use and maintain this document](#how-to-use-and-maintain-this-document)
- [Executive summary](#executive-summary-2026-10-20) · [Landscape map](#landscape-map)
- **Part I — Product catalogue:** A. TUI/tmux managers · B. GUI workspaces · C. Terminals & multiplexers
  (+ protocol landscape) · D. Cloud/async agents · E. Remote & mobile · F. Orchestration products ·
  G. Agent CLIs & adjacent tooling
- **Deep dives — herdr and tuios:** DD1 herdr · DD2 tuios (identity, architecture, status model, OSC 7501 interop, head-to-head, ideas)
- **Part II — Orchestration deep dive:** practitioners · research evidence · workflow engines ·
  coordination protocols · synthesis (recipe primitives, `ptyro mcp`, mailbox, delivery, M10)
- **Part III — Idea funnel:** Top 15 · full catalogue (R-ids) · not recommended
- **Part IV — Watchlist, re-checks, SPEC §20 candidates, cadence, positioning** · Changelog

---

## How to use and maintain this document

**Purpose.**
1. Know the field: who else hosts, multiplexes, isolates and orchestrates coding agents, and how.
2. Be honest about where others are ahead. Each entry's **Gaps** are the things a product does
   better than ptyro or that ptyro hasn't planned, written generously.
3. Turn that into **numbered idea candidates** (Part III) that the owner can promote, park or reject.

**Status markers** (same spirit as SPEC §20):
- **[V]** verified against the product's own docs, repo or a local binary on the date given.
- **[V-local]** checked on this machine (`--help`, generated schemas, binary strings).
- **[S]** secondary sources only (blogs, aggregators, comparison pages).
- **[U]** uncertain: sources conflict, or it comes from memory.
- Star counts and versions are snapshots from the date checked.

**Entry template** (keep entries in this shape):
```
#### <Segment letter><n>. <Product>
- **URL:** · **Class:** · license/pricing · stack
- **Maturity [V|S|U YYYY-MM-DD]:** stars, last release, activity
- **What it is:** one paragraph
- **Special:** what makes it stand out
- **Gaps (positive):** what it does better than ptyro or that ptyro lacks; note "planned Mx" where ptyro already plans it
- **Weaknesses vs ptyro:** brief
- **Ideas:** → R-XXX-nn ids in Part III
```

**Idea lifecycle.**
1. New ideas get an id `R-<AREA>-<nn>` in Part III with status `new`.
2. When the owner promotes one, add the ROADMAP task id to its row (`→ M7.12`) and reference the R-id in the ROADMAP task text. When it ships, SPEC and README take over as usual.
3. If it is `parked` or `rejected`, keep the row and say why, so the idea isn't re-proposed blind.
4. Ids are stable: never reuse one.

**Rules.**
- **Ideas, never code.** Read other projects to learn; never copy their code. Licenses here range
  from MIT to AGPL, BSL, Elastic and Commons Clause.
- Describe ptyro on its own terms. Products that shaped ptyro are credited in SPEC §20.6 and the
  README "Inspirations". This file compares; it does not imply ptyro is derived from anything.
- A hard technical fact discovered here (a hook name, a flag, an escape sequence) belongs in
  **SPEC §20** once it is verified. Part IV.3 lists candidates waiting to graduate.
- Date every claim. The field moves weekly. In October 2026 alone: OSC 7501 went public, two
  products shut down, and ACP v2 shipped an alpha.

---

## Executive summary (2026-10-20)

1. **The core idea is no longer rare.**
   - "A daemon that owns agent PTYs, one worktree per task, a status sidebar" now ships in **herdr**
     (Rust, ≈45k★), **tuios**, **Claude Code's own agent view**, and every major GUI vendor (Codex
     app, Cursor 3, Copilot app, Antigravity 2.0, JetBrains Air, VS Code, Zed).
   - ptyro wins on the **combination**:
     - terminal-native over plain SSH;
     - exact screen restore and reboot survival;
     - **identity profiles** (account + git author + SSH key + gh login);
     - **hook + OSC status with explicit precedence**;
     - **CLI/`--json` parity**;
     - **cross-vendor orchestration with daemon-enforced policy**;
     - **multi-repo change sets** (nobody else has them; thurbox is the closest).
   - **The deep dives sharpen this.** tuios already ships much of M4–M10, and herdr has funding plus a plugin ecosystem covering M3/M6/M7/M9 gaps. The lasting moats are profiles, the project/worktree lifecycle, exact restore with durable history, the declarative orchestration engine, and change sets (see *Deep dives*).
2. **Claude Code now natively covers single-vendor orchestration.**
   - It has background supervisor sessions, agent teams (mailbox + task list), cross-session
     messaging over a per-session socket, and resumable JS workflows.
   - ptyro should **consume** these surfaces (socket, hooks, `claude agents --json`, `WorktreeCreate`)
     rather than duplicate them.
   - ptyro should differentiate on what they lack: other vendors, reboot survival, **mid-run human
     gates**, verified-done, compare-and-ship, and multi-repo work.
3. **Orchestration's binding constraint is human review attention, not agent count.**
   - Practitioners run **2–6** agents.
   - Research shows that **picking the winner**, not generating candidates, is where results are lost.
   - Parallel *writers* on shared code fail; parallel attempts, readers, reviewers and verifiers work.
   - The most valuable orchestration features are therefore **verified-done**, a **ranked review
     queue**, and an **evidence-ranked compare**.
4. **The post-PR loop is the biggest workflow gap.**
   - Cloud agents (Claude Auto-fix, Jules CI Fixer, Amp, Cursor, Copilot) and orchestrators
     (Composio) route CI failures and review comments back to the agent automatically.
   - ptyro's plan stops at opening the PR.
5. **The protocols matured fast. Each change below needs a spike before we build on it:**
   - **OSC 7501** is Mitchell Hashimoto's public Program Status Protocol (spec revs 0.2 → 0.4 in four days).
   - Ghostty 1.4 parses it, Rex and tuios implement it, and Claude Code and Crush emit it.
   - **ACP** has a registry of ~42 agents, and v2 adds `state_update`.
   - **MCP 2026-07-28** went stateless; Tasks are now an extension that no coding CLI implements.
   - **Codex** can run its TUI against an app-server that ptyro owns.
   - **Gemini CLI** was replaced by **Antigravity CLI** for consumer accounts on 2026-06-18.
6. **The market is consolidating.** These shut down or were archived: BloopAI (Vibe Kanban),
   Roo Code, Terragon, Overstory (now Warren), Crystal (now Nimbalyst), and Omnara's mobile product
   (pivoted). The durable niches are **vendor-neutral, self-hosted, terminal-native, multi-repo**,
   which is where ptyro sits. Being open source with no service to shut down is a selling point.

**Top 15 idea candidates** are in **§III.1**. The orchestration deep dive the owner asked for is
**Part II**.

---

## Landscape map

| Segment | What it is | Leaders (2026-10) | Threat to ptyro | Where ptyro wins |
|---|---|---|---|---|
| **A. TUI / tmux agent managers** | Terminal session managers for parallel agents | herdr, tuios, Claude agent view, claude-squad, AoE, agent-deck, workmux | **High.** herdr and tuios own PTYs too | Worktree lifecycle, profiles, review/PR, orchestration, `--json` parity |
| **B. GUI agent workspaces** | Electron/Tauri/native apps: worktree per task, many agents | Conductor, Orca, Superset, Emdash, Codex app, Cursor 3, Claude desktop, Copilot app, Air | Medium (different users) | Terminal-native, detachable daemon, SSH, identity, no vendor lock-in |
| **C. Terminals and multiplexers** | Emulators and muxes adding agent awareness | cmux, Rex, iTerm2 3.7, Zellij 0.44, tmux 3.8, Ghostty, Warp | Medium. "tmux + plugin" covers basic status | Agent model, worktrees, orchestration; processes survive (cmux's don't) |
| **D. Cloud / async agents** | "Fire and forget, get a PR" | Codex cloud, Claude cloud, Jules, Copilot cloud agent, Cursor cloud, Devin, Amp, Factory | Medium. They own the post-PR loop | Local, any agent, real terminal, your subscriptions and keys |
| **E. Remote / mobile companions** | Phone control of local agents | Claude Remote Control, Happy, Paseo, Happier, CloudCLI, CC Pocket | Medium. They ship today; ptyro's M9 is planned | Terminal fidelity for any program, vendor-neutral, profiles |
| **F. Orchestration** | Lead/worker, swarms, recipes, specs, task boards | Claude teams/workflows, Gas Town + beads, Ruflo, Composio AO, Goose, Spec Kit, Kiro | **High** for M7's novelty | Cross-vendor, policy, gates, verified-done, compare, multi-repo |
| **G. Agent CLIs and adjacent tools** | The agents ptyro hosts; usage, sandbox, recording | Claude Code, Codex, Antigravity, OpenCode, Copilot, Droid; ccusage, sbx, srt, asciinema 3 | Integration surface, not competitors | — |

---

## Part I — Product catalogue

### A. Terminal/TUI and tmux-based agent session managers

> **Headline (2026-10-20):** "own multiplexer, no tmux" is **no longer unique**. herdr (Rust,
> ≈44.9k★, the segment leader), tuios (Go, 5.5k★) and initech already ship a daemon that owns
> PTYs with detach/reattach. ptyro must win on the *combination*: projects + worktree lifecycle +
> profiles + hook/OSC status + CLI/`--json` parity + review → PR + orchestration + multi-repo.
> Segment table stakes now: web/PWA approvals, Docker/Podman sandbox, fork-into-new-worktree, cost
> & quota meters, agent↔agent messaging, SSH hosts merged into one agent list.
>
> **OSC 7501 note.** tuios also implements the **OSC 7501 Program Status Protocol** — the same
> protocol Claude Code speaks and ptyro answers (SPEC §5.6, §20.1). This is *interop*, not a
> collision: programs written for tuios report to ptyro unchanged. tuios uses the state names
> `none/working/needs_input/idle/done/errored/unknown` with `kind=auth` for login prompts; ptyro's
> probe reply advertises `idle,working,done,blocked,error` and kinds `permission,question,auth`. The
> mapping should be written down (R-STA-01).

#### A1. herdr
> **Full deep dive: DD1.** Corrections from it: the license is resolved (AGPL → dual → **Apache-2.0** since 2026-07-22). herdr *does* manage git worktrees. Claude Code status comes from **screen manifests, not hooks**. herdr has **no OSC 7501** (users are requesting it) and no `error` state. It raised **$6M (Bessemer, YC F26)**.

- **URL:** https://github.com/herdrdev/herdr · https://herdr.dev
- **Class:** agent-aware terminal multiplexer/runtime, own PTYs · Apache-2.0 per GitHub API [V] (a July 2026 source said AGPL + commercial — relicensing unverified) · "Herdr Cloud" waitlist
- **Stack:** Rust single binary (~10 MB); repo contains `crates/ghostty-vt` (likely libghostty-based VT, inferred)
- **Maturity [V 2026-10-20]:** 44,935★; v0.9.5 (2026-10-14); 380 open issues; #1 GitHub Trending ~2026-06-30 [S]; pre-1.0, largely solo [S]; site claims 1,789 community plugins.
- **What it is:** background server keeps terminals alive across client exit and SSH drops; after reboot restores layout and resumes supported agent sessions (processes don't survive). tmux-style prefix + mouse; sidebar of workspaces → tabs → panes, each agent labelled working/blocked/idle/done; detects 22 agent CLIs; CLI + socket API so agents split panes, start and prompt other agents, wait on state; remote machines over SSH (`herdr machine add`, `herdr --remote ssh://…`) merged into one list; macOS, Linux, Windows (beta).
- **Special:**
  - Layered state detection [V]: remotely updated per-agent **detection manifests** with local overrides (`~/.config/herdr/agent-detection/<agent>.toml`) and hot reload; integration hooks (`herdr integration install claude|codex|…`); **self-report** via `herdr pane report-agent --state` / `report-metadata`.
  - **Strict "blocked"** — only when known approval UI is visible; unknown screens fall back to idle *with a reason label*; **`herdr agent explain <target>`** prints the matching rule.
  - Precise automation CLI [V]: `agent start` waits for ready; `agent prompt --wait --until` with errors `agent_blocked`, `agent_prompt_stalled` (no activity within 5 s); `agent read` refuses history reads while working; `pane wait-output --regex`; `pane read --format ansi`; JSON under `.result`; exit 1 runtime / 2 syntax; `completion_seq` turn counter; **"done" = idle but not yet seen**, cleared only by focus.
  - Ecosystem: ccgram (Telegram bridge), herdr-reviewr, Agentchute (inter-agent messaging), native support in the Moshi iOS app [S]; ships an agent skill + AGENTS.md so agents can drive it.
- **Gaps (positive):** detection manifests + `explain`; prompt-stall and already-blocked errors on `send`; done-vs-seen; multi-machine sidebar now (ptyro M9); plugin marketplace (ptyro backlog WASM); Windows beta; ecosystem & mindshare.
- **Weaknesses vs ptyro:** no worktree lifecycle, project model, setup scripts, ports or `.env` copy found in docs; no identity profiles; no review/PR flow; screen heuristics are the default; bus factor one; reports of slow rendering at 10+ panes [S].
- **Ideas:** R-STA-02 (`ptyro agent explain`), R-STA-03 (`send --wait` stall/blocked errors, turn counter), R-STA-04 (done-unseen), R-STA-01 (accept herdr `report-agent` + OSC 7501 inputs), R-ORC-18 (ship a ptyro agent skill/AGENTS.md before `ptyro mcp`).

#### A2. Claude Code native — Agent View, `--bg`, `--worktree`, Agent Teams
- **URL:** https://code.claude.com/docs/en/agent-view · `/worktrees` · `/agent-teams`
- **Class:** built into ptyro's primary agent · proprietary, subscription quota
- **Maturity [V]:** Agent View launched 2026-05-11 (v2.1.139), research preview; Agent Teams experimental.
- **What it is:** `claude agents` is one table of every background session; a per-user **supervisor** hosts sessions that survive supervisor restarts, auto-updates and sleep. Groups Pinned / Ready for review / Needs input / Working / Completed; **Haiku-generated one-line row summaries**; linked PR status coloured by PR/CI state; **Peek** (Space) shows the pending question; **inline reply**, numbered choices by number; attach/detach with →/←; dispatch bar with `@subagent`, `@repo`, `/skill`, `! cmd`, `#PR`. Background sessions move into `.claude/worktrees/<id>` before writing; commit, push, draft PR, never main or force-push. `claude --bg`, `--exec` (shell job, no model), `attach`, `logs`, `stop`, `respawn --all`, `rm` (refused with unpushed commits), `claude agents --json`, `daemon status|logs|stop`. **Auto-shelving**: idle unattached sessions stop after ~1 h and resume on attach. `claude --worktree`: **`.worktreeinclude`** (gitignore syntax), `worktree.baseRef`, **`--worktree "#1234"`** from a PR/MR, `git worktree lock` while running, periodic stale sweep, `WorktreeCreate`/`WorktreeRemove` hooks for other VCS, edits to the main checkout blocked.
- **Special:** first-party knowledge of Claude's state (permission prompts, MCP input requests); PR/CI-driven "Ready for review" grouping; summaries; peek/reply; careful worktree safety (unpushed refusal, locks, sweep, submodule/LFS, symlink refusal); documented state under `~/.claude/jobs/`.
- **Gaps:** AI row summaries; PR/CI-aware grouping; peek + inline reply; auto-shelve; worktree from PR; `.worktreeinclude`; worktree locking.
- **Weaknesses:** Claude-only; local only; research preview; one account per supervisor (`CLAUDE_CONFIG_DIR`); no ports/setup scripts beyond hooks; doesn't multiplex non-Claude programs; worktree deleted with the session.
- **Ideas:** R-AGT-02 (ingest `claude agents --json`/adopt), R-WT-01 (`.worktreeinclude` + `WorktreeCreate`-style hooks), R-WT-02 (`ptyro new --pr 1234`), R-UX-01 (opt-in summaries), R-RES-01 (auto-shelve), R-UX-02 (peek & reply).

#### A3. tuios
> **Full deep dive: DD2.** Corrections from it: tuios now ships review notes, checkpoints, `ship pr`, `fan compare/verify/keep`, an MCP server, ACP/Codex workers, held approvals with risk rules, and multi-host plus phone pairing. Hooks cover **11** events, not 10. The precedence ladder also has a `transcript` rank, and in the code `detect` ranks above `stall`. It implements OSC 7501 **rev 0.2** with a bare probe reply.

- **URL:** https://github.com/Gaurav-Gosain/tuios · tuios.dev · MIT
- **Stack:** Go, Charm (Bubble Tea v2, Lipgloss v2)
- **Maturity [V]:** 5,529★; v0.10.1 (2026-10-17); very active.
- **What it is:** terminal window manager + multiplexer with its own daemon (survives detach, daemon restart, reboot with layout/cwd restore); BSP / master-stack / scrolling tiling; 9 workspaces; vim-style modal keys; palette; zoom. `tuios integration install` wires 19 harnesses; detects 24 agent CLIs. **Inbox (Prefix+i)** — approvals, questions, mail, errors, finished turns across sessions *and machines*, answerable inline. `tuios ask-human`, `tuios send-agent-message`. **`tuios fan`** — one prompt to several agents, each in its own worktree; `tuios worktree pull` from remote machines. SSH server mode; Tailscale attach; **tape DSL** to record/replay/run headless; JSON verb protocol; hooks on 10 events; `tuios-web`.
- **Special:** OSC 7501 implementation with a formal **precedence ladder** — report > program > osc > screen > stall > detect; a source may only overwrite claims at or below its rank; a losing report returns `"applied": false`; a **visible-blocker override** lets a screen rule briefly beat a stale higher-ranked claim (claim ≥2 s old and output printed since). Also consumes herdr's pane state protocol.
- **Gaps:** formally specified precedence with `applied:false`; actionable cross-machine inbox; `kind=auth`; tape record/replay (ptyro M8 recorder); fan-out shipped (ptyro M7).
- **Weaknesses:** a window manager first — no project model, ports, profiles, review/PR.
- **Ideas:** R-STA-01 (publish ptyro's precedence table, `applied:false`, state-name mapping), R-STA-05 (`errored` + auth-blocked feeding profile health), R-UX-02 (actionable inbox), `ptyro fan` alias for `fanout`.

#### A4. Claude Squad (smtg-ai)
- **URL:** https://github.com/smtg-ai/claude-squad · **AGPL-3.0** · Go, needs tmux + gh
- **Maturity [V]:** 8,632★; v1.0.20 (2026-08-20), activity slowed.
- **What it is:** each agent (Claude, Codex, OpenCode, Amp, Gemini, Aider) in its own tmux session + worktree; preview & diff tabs; `c` commit-and-pause, `s` commit-and-push; experimental `--autoyes`; named program profiles; `cs`.
- **Special:** simplest mental model; **"pause"** commits WIP and releases the worktree so the branch can be checked out in the main repo.
- **Gaps:** pause/resume; opt-in auto-yes (not on ptyro roadmap).
- **Weaknesses:** tmux; coarse status; no API/JSON/profiles/ports/setup; AGPL; slowing.
- **Ideas:** `ptyro pause` / `resume` (R-WT-05); per-profile opt-in auto-approve policy with audit (R-SEC-03).

#### A5. Agent of Empires (AoE)
- **URL:** https://github.com/agent-of-empires/agent-of-empires · MIT (one file Apache-2.0 from herdr) · Rust
- **Maturity [V]:** 3,370★; v1.20.0 (2026-10-15); 169 open issues.
- **What it is:** each agent in a tmux session; 16 agents auto-detected (deprecated CLIs flagged); worktrees; **multi-repo workspaces**; Docker, Podman or **Apple Containers** sandboxes with shared auth; web dashboard with mobile-friendly **"Structured View"** and `aoe acp doctor` (likely ACP; details [U]); diff view; resume and fork; profiles; repo hooks in `.agent-of-empires/config.toml`; MCP; "tool sessions", **"scratch sessions"**; push notifications; `t` toggles the paired shell.
- **Gaps:** structured mobile view now (ptyro M9+M10); multi-repo (M11); containers incl. Apple Containers (backlog); HTTP API; scratch sessions; paired shell; ACP doctor.
- **Weaknesses:** tmux; undocumented status detection; no ports or identity profiles found.
- **Ideas:** companion-shell key per task with the same profile env (R-UX-03); scratch sessions (no project, temp dir); `ptyro doctor` covering hooks, per-profile `gh auth`, ACP, OSC support (M5); a read-only status web view before full M9 (R-REM-04).

#### A6. Agent Deck (asheshgoplani)
- **URL:** https://github.com/asheshgoplani/agent-deck · MIT · Go/Bubble Tea, own tmux socket
- **Maturity [V]:** 1,127★ but **4,354 commits**; v1.16.30 (2026-10-18); very active; seeking co-maintainers.
- **What it is:** ~12 agents; running/waiting/idle/error via "smart polling" plus hooks for Codex (`codex-hooks install`) and pi. **MCP manager** — toggle MCP servers per session; **MCP pooling** over Unix sockets (claims 85–90% less MCP memory) with reconnecting proxy. Native **fork into a new worktree** carrying uncommitted changes and Docker settings. **Conductor** — persistent orchestrator session that auto-answers routine questions and escalates the rest, Telegram/Slack bridge, 15-min heartbeat; **watchers** route webhooks, GitHub events and ntfy into it. Docker sandbox; web UI with push (token auth off-loopback); SSH remotes with self-install and `remote drain`. **Cost dashboard** from Claude transcript hooks; daily/weekly/monthly **budgets with enforcement**; `agent-deck usage` shows Claude's **5-hour and 7-day quota windows**. Profiles `[profiles.work.claude] config_dir`; **`agent-deck session switch-account <session> <account>`** moves a session — conversation included — to another account.
- **Special:** closest overlap with ptyro **profiles**; a working lead-agent ("conductor"); cost budgets.
- **Gaps:** session move between accounts; MCP pooling/toggling; budgets & quota windows (ptyro backlog); chat-app escalation and event watchers; remote self-install + drain; Docker.
- **Weaknesses:** tmux + polling; huge surface with "untested" features; no ports; profiles cover only Claude config dir (git author/SSH/gh not documented).
- **Ideas:** `ptyro session move --profile <p>` (R-PRF-01); **quota-aware profiles** showing 5 h/7 d windows and suggesting a switch (R-USE-01); MCP pooling behind the daemon (R-AGT-05); webhook/ntfy watchers → ptyro events (R-ORC-08); per-profile/project budgets with opt-in hard stop (R-USE-02).

#### A7. workmux (raine)
- **URL:** https://github.com/raine/workmux · MIT · Rust
- **Maturity [V]:** 2,922★; v0.1.276 (2026-10-14), very frequent releases.
- **What it is:** `workmux add` → worktree + tmux window; from a PR, a fork branch, **moved uncommitted changes**, or a **branch name generated from the prompt**; `workmux merge` merges and removes worktree/window/branch; status icons in tmux window names (`workmux setup` installs hooks); TUI dashboard with diffs, **patch staging**, send-input; `.workmux.yaml` with pane layouts, `post_create` / **`pre_merge`** / `pre_remove` hooks, **copy and symlink rules** (`.env`, `node_modules`); Kitty/WezTerm/Zellij backends; containers or **Lima VMs**; ~9 agents.
- **Gaps:** symlink rules; pre-merge/pre-remove hooks; hunk staging (ptyro M6); take-changes; Lima.
- **Weaknesses:** needs tmux or a terminal backend; no daemon, profiles, ports or event stream.
- **Ideas:** `symlink = [...]` next to copy rules (R-WT-04); `ptyro new --take-changes` (R-WT-05); `pre_merge`/`pre_remove` hooks; hunk staging in M6.

#### A8. dmux (standardagents, ex-formkit)
- **URL:** https://github.com/standardagents/dmux · MIT · Node/npm
- **Maturity [V]:** 1,804★; v5.11.1 (2026-08-16), slowed.
- **What it is:** one tmux pane + worktree + branch per task; **AI-generated branch names, commit messages and pane names** (optional OpenRouter key); one-step commit-merge-cleanup or PR; **multi-select one prompt to several agents**; multiple projects; create/pre-merge/post-merge hooks; macOS alert when a pane settles; file browser; ~12 agents.
- **Gaps:** AI naming (not on ptyro roadmap); fan-out (M7); file browser (backlog).
- **Ideas:** `ptyro new "<prompt>"` derives a slug + display name — deterministic by default, cheap model opt-in via the profile's account (R-UX-01); AI commit messages at merge in M6.

#### A9. CCManager (kbwo)
- **URL:** https://github.com/kbwo/ccmanager · MIT · TypeScript · **no tmux**
- **Maturity [V]:** 1,265★; v4.4.9 (2026-10-18).
- **What it is:** sessions for ~10 agents across worktrees/projects; configurable per-CLI state detection; post-create hooks; **`.worktreeinclude`**; **devcontainer split** (manager on host, agents in container: `--devc-up-command`, `--devc-exec-command`); multi-project discovery under a root; **copies Claude session data between worktrees** to carry context; crash restore; experimental **auto-approval where Claude Haiku judges whether a prompt is safe**, falling back to manual; status-change hooks; presets.
- **Gaps:** LLM-judged auto-approval; devcontainer split; conversation carry into a new worktree; project auto-discovery; status-change hooks.
- **Ideas:** `ptyro project scan ~/dev` (R-UX-04); `on_blocked = "…"` config hooks (R-STA-06); tiered auto-approve policy — allowlists first, opt-in LLM judge second, all logged (R-SEC-03); `ptyro fork <task>` = new worktree from HEAD + native conversation fork (R-AGT-04).

#### A10. Sidecar (marcus)
- **URL:** https://github.com/marcus/sidecar · MIT, no telemetry · Go (tmux-backed terminals)
- **Maturity [V]:** 1,091★; v1.17.1 (2026-10-08); 3,569 commits.
- **What it is:** companion TUI: worktree workspaces, agent launch, merge flow; sessions grouped by status; git staging and diffs; **TD Monitor** for its `td` agent task manager; file browser; 21 themes; `sidecar open|layout`; **`sidecar notify post`** (toasts with jump targets); **`sidecar agent`** lets one agent start and prompt a secondary agent (e.g. a Codex reviewer); **"terminal resource provider protocol"** — external executables (sidecar-jira, sidecar-github) feed read-only panes over JSON stdio.
- **Gaps:** stdio JSON provider plugins (lighter than WASM); issue panes; notify with jump target; agent-summoned reviewer (M7).
- **Ideas:** stdio-JSON "provider" plugin type before WASM (R-EXT-01); `ptyro notify --target <session>:<file>:<line>`; "review with <agent>" action spawning a read-only reviewer in the same worktree (R-ORC-04).

#### A11. amux (mixpeek)
- **URL:** https://github.com/mixpeek/amux · README badge "MIT + Commons Clause" (API: NOASSERTION) · Rust + SQLite, tmux 3.2+ · optional amux Cloud
- **Maturity [V]:** 544★, 8,210 commits; v0.9.108 (2026-07-13).
- **What it is:** control plane for unattended Claude swarms: shared board with **atomic task claiming**; agents ask/hand off/unblock each other; **self-healing** restart of stuck sessions; **schedules** ("daily at 9am", "every 15m"); per-worker model switch (Claude/Codex/Gemini/OpenCode/Ollama); dashboard on :8824 installable as PWA; Mac menu-bar app with Dock badge; iPhone/iPad/Android apps.
- **Gaps:** scheduling; watchdog; task board; native apps and menu-bar badge.
- **Ideas:** `ptyro schedule "<cron>" recipe:<name>` (R-RUN-02); stall watchdog → `stalled` status, notify, optional native-resume restart (R-ORC-09).

#### A12. amux (andyrewlee)
- **URL:** https://github.com/andyrewlee/amux · MIT · Go · also maintains *awesome-agent-orchestrators*
- **Maturity [V]:** 163★; v0.0.20 (2026-07-17).
- **What it is:** workspaces are worktrees; **imports worktrees created elsewhere**; Changes sidebar, hunk-navigable diff; commit but **never push**; `merge --no-ff`; **shelve** (remove worktree, keep branch + metadata); per-workspace `AMUX_PORT`; lifecycle hooks with **trust approval for repo-supplied scripts**; **minisign-verified self-update**; 14 agents.
- **Gaps:** shelve; adopt existing worktrees; signed self-update. (Trust-gated repo scripts: ptyro already has this, SPEC §10.5.)
- **Ideas:** `ptyro shelve`/`unshelve` (R-WT-06); `ptyro adopt <path>` (R-WT-07); minisign/cosign-signed releases (R-SEC-04).

#### A13. ccmux (epilande)
- **URL:** https://github.com/epilande/ccmux · MIT · TS/Bun
- **Maturity [V]:** 230★; v1.4.4 (2026-10-15).
- **What it is:** tmux picker with a pinned "needs attention" band; background **daemon streams state over SSE**; hooked agents write **PID marker files** mapped to tmux panes; Gemini by process/screen; **desktop notifications with Approve / Deny / inline Reply buttons**; `ccmux last` prints a session's last response; **`ccmux handoff`** sends it to another (or new) session with a **provenance header**, typed only into an idle composer; `spawn --worktree`; prune panel limited to finished work.
- **Gaps:** actionable notifications; human-driven handoff primitive (earlier than M7); SSE stream (relevant to M9).
- **Ideas:** `ptyro last <session>` and `ptyro handoff <from> <to>` delivering only when idle with provenance (R-ORC-19); actionable libnotify / macOS notifications (R-NOT-02).

#### A14. Uzi (devflowinc) — stale
- **URL:** https://github.com/devflowinc/uzi · MIT · Go · 582★, last push 2025-06-04 [V] — no activity since
- **What it was:** worktree + tmux per agent; `uzi.yaml` with `devCommand` and **`portRange`**; `uzi ls -w`; `uzi auto` presses Enter on trust prompts; **`uzi broadcast`**; `uzi checkpoint` commits and rebases into the current branch.
- **Lesson / ideas:** demand for ports + dev servers. ptyro allocates ports — it should also **run and surface the dev server** (URL in sidebar; R-WT-08). `ptyro broadcast "<msg>" --filter` (planned M5/M7).

#### A15. Worktree-only CLIs — git-worktree-runner (`git gtr`) and gwq
- **git-worktree-runner** (https://github.com/coderabbitai/git-worktree-runner · Apache-2.0 · Bash · 1,794★ · v2.11.1 2026-09-14 [V]): `new`, **`pr`**, `go`, `run`, `list`, `mv`, `rm`, `copy`; **`clean --merged` / `--closed`** by PR state; `gtr.copy.include`; `postCreate`, `postCd`; `--editor`, `--ai`; **`--porcelain`**; config in git config + committed `.gtrconfig` with **`git gtr trust`**; `doctor`.
- **gwq** (https://github.com/d-kuro/gwq · Apache-2.0 · Go · 475★ · v0.1.2 2026-10-02 [V]): global `host/owner/repo/branch` layout under one base dir; fuzzy finder; `gwq status` across all worktrees with watch + **JSON/CSV**; `gwq tmux`; filesystem scan instead of a registry.
- **Ideas:** `ptyro gc --merged|--closed` once M6 lands (R-WT-09); `ptyro open --editor`; `--porcelain` alias.

#### A16. tsk (tsk-tsk)
- **URL:** https://github.com/dtormoen/tsk-tsk · crate `tsk-ai` · MIT · Rust · 171★ · v0.10.9 (2026-07-29) [V]
- **What it is:** delegates tasks to Claude Code or Codex in **Docker/Podman with network isolation via a proxy**, YOLO mode inside, returns **branches for review**; `tsk add` queues; `tsk server start --workers 4` worker pool; **task chaining** (child starts from parent's result); `tsk shell`.
- **Gaps:** task queue with a concurrency cap; chaining; network-restricted sandbox.
- **Ideas:** `ptyro queue add "<prompt>"` with `max_parallel` per project/profile — doubles as quota management (R-RUN-01).

#### A17. Native features in other agent CLIs (brief; full detail in segment G)
- **Codex CLI** (130k★, v0.163.0, 2026-10-15 [V]): `codex resume` (picker, `--last`, id), `/fork`, `/side`, Goals (multi-session objectives), subagents (`agents.max_threads` default 6), `/ps`, named/pinned threads [S]; rollouts JSONL under `~/.codex/sessions/`; **no native `--worktree` in the CLI** (open issue) — ptyro fills that.
- **OpenCode** (218k★, v1.19.3 [V]): `--fork`, parent/child sessions, JSON export/import, server/client architecture; share links reportedly retired ~2026-10-15 [U].
- **Copilot CLI:** Sessions sidebar (2026-08) [U].

#### A18. Smaller projects worth tracking (mostly via *awesome-agent-orchestrators*, [U] beyond one-liners)
- **initech** (Go, MIT, own PTYs): `initech send` returns an explicit error rather than dropping input; 13 **role templates** (pm, arch, eng, qa, shipper, sec, ops); states include **"stalled"** and **"idle-with-work-waiting"**; headless `serve`; `peers`.
- **thurbox** (Rust core + Lua UI panes, MIT, v2.64.4): SSH hosts in `hosts.toml`; agent-neutral mailbox; **one session spanning several repos, each on its own worktree of a shared branch**, Ctrl-S to sync with base — **the closest thing found to M11 change sets**. Watch closely. (2026-10-21 [V] for ptyro: M11 shipped what thurbox lacks: a contract with versioned revisions and an approval gate, fenced per-repo roles, readiness wake-ups, set checkpoints, overlays, and a dependency-ordered merge train.)
- **tmuxai** (Go, Apache-2.0, 1.9k★): AI pair reading tmux panes; adjacent only.
- **agent-manager**, **agent-session-manager:** tmux TUIs that already ship in-terminal **diff review with comments sent back to the agent** (ptyro M6).
- **YYLO:** risk-based merge queue (relevant to M11 merge train). **repomon** (Rust): supervises agents across many repos with phone approval. **pappardelle:** description → issue + worktree + PR + Claude session. Kage, Rove, tmux-ide, Cyclops, openkanban.

#### A — capability matrix (others ship today vs ptyro)

| Capability | Shipped by | ptyro |
|---|---|---|
| Own PTY daemon (no tmux) | herdr, tuios, initech, ccmanager (partly) | **Has** |
| Hook-based status | Claude native, ccmux, herdr/tuios integrations, workmux, agent-deck | **Has** |
| OSC 7501 status | Claude Code (emitter), tuios; herdr own report API | **Has** — write down state mapping |
| Answer from inbox/notification | tuios, Claude peek/reply, ccmux, agent-deck web | Attention yes; reply-from-inbox unspecified |
| Per-profile accounts | agent-deck (config_dir, switch-account) | **Has**, wider (git, SSH, gh); no session move |
| Cost & quota meters, budgets | agent-deck | Backlog |
| Fork session into new worktree | agent-deck, AoE, Claude, Codex, OpenCode | Not planned |
| Fan-out one prompt → N agents | tuios `fan`, dmux | M7 |
| Diff review with comments → agent | agent-manager, agent-session-manager, sidecar, workmux | M6 |
| PR-aware grouping / cleanup on merge | Claude Agent View, git-gtr | M6 partly |
| Lead agent / orchestrator | agent-deck conductor, herdr API, Claude teams, mixpeek/amux | M7 |
| Agent↔agent mailbox / handoff | herdr, tuios, thurbox, ccmux, Claude | M7 |
| Web / PWA / mobile | AoE, agent-deck, mixpeek/amux, tuios-web, herdr (Moshi) | M9 |
| SSH hosts in one list | herdr, agent-deck, tuios, thurbox | M9 |
| Structured chat (ACP) | AoE Structured View (likely ACP) | M10 |
| Multi-repo sessions | AoE, thurbox | M11 |
| Container/VM sandbox | AoE, agent-deck, workmux (Lima), ccmanager, tsk | Backlog |
| Trust gating for repo scripts | andyrewlee/amux, git-gtr, agent-deck | **Has** (SPEC §10.5) |
| Scheduling / task queue | mixpeek/amux, tsk | Not planned |
| AI naming & summaries | dmux, Claude | Not planned |
| Recorder / replay | tuios tape | **Has** (M8.2: replay, asciicast v2/text export) |
| Plugins | herdr marketplace, sidecar stdio providers | Backlog (WASM) |

---

### B. Desktop/GUI and web-local agent workspaces

> **Headline (2026-10-20):** **every platform vendor now ships one** — Codex app (Feb 2026), Cursor
> 3 Agents Window (Apr), Claude Code desktop Code tab, GitHub Copilot app (Jun, preview), JetBrains
> Air (Mar, preview), Antigravity 2.0 (May), VS Code Agents window, Zed Parallel Agents + Terminal
> Threads. Independents (Orca, Superset, Emdash, Conductor) compete on breadth: 15–30 agents,
> mobile, SSH/remote hosts, cloud, issue intake, automations. **Consolidation:** BloopAI closed
> (Vibe Kanban community-maintained); Crystal deprecated for Nimbalyst.
>
> **Table stakes:** worktree per task, setup script, `.env` / `.worktreeinclude` copy, port per
> workspace, diff review with line comments to the agent, PR + CI status, "fix failing checks",
> blocked/done notifications, a mobile or remote view.
> **Differentiators:** in-app browser / "design mode", scheduled & event automations, always-on
> cloud sandboxes, multiplayer, account switching + usage meters, cross-session agent messaging,
> an MCP server/SDK for the app itself, container isolation.
>
> **ptyro's edge here:** terminal-native (no Electron); daemon-owned PTYs survive closing the UI
> (most GUIs only "restore"); per-workspace identity profiles; hook + OSC precedence engine; CLI
> `--json` parity; multi-repo change sets. **Behind:** browser/design mode, cloud/always-on hosts,
> automations, issue intake, usage meters/account switching, multiplayer.

#### B1. Conductor (Melty Labs)
- **URL:** https://www.conductor.build · docs `/docs` · LLM index `/llms-full.txt`
- **Class:** Mac desktop app + Conductor Cloud · closed · Free (local, BYO subscription) / Pro $50/mo (cloud hours, multiplayer ≤5, API, mobile) / Teams $60/user / Enterprise [V]
- **Maturity [V]:** YC S24, $24M raised, "10× since January"; users include Linear, Vercel, Ramp, Notion, Stripe; SOC 2 Type II.
- **What it is:** workspaces from a branch, PR, GitHub issue or **Linear issue**; each = branch + worktree + run env + shared **`.context` folder**; Claude Code / Codex / Cursor / OpenCode chats; several agents per workspace (implementer + reviewer); diff viewer and **Checks tab** (git status, CI, deployments, comments, todos); PR → merge → archive. Cloud runs in Vercel sandboxes (8 vCPU/16 GB) that keep going after the app closes.
- **Special:** layered TOML settings **with a published JSON schema** (`~/.conductor/settings.toml`, `.conductor/settings.toml`, `.conductor/settings.local.toml`, managed org settings); setup/run scripts with `CONDUCTOR_PORT` range and concurrent/exclusive run mode; env vars `CONDUCTOR_ROOT_PATH`, `CONDUCTOR_WORKSPACE_PATH`, `CONDUCTOR_IS_LOCAL`, `CONDUCTOR_API_URL`, `CONDUCTOR_API_TOKEN`, `CONDUCTOR_SESSION_ID` (ptyro plans aliases, M3.11); **Spotlight testing** (sync worktree into repo root for apps that can't run from a worktree); Codex "goals"; Conductor API (REST: workspaces, sessions, transcripts; webhook "routines"); hosted MCP at `api.conductor.build/mcp`; multiplayer; Slack.
- **Gaps:** closed-laptop cloud; Linear intake (ptyro: issue URL M6.6, Linear backlog); Spotlight (partly M6.7 pair mode); combined Checks tab (M6.5 is CI only); webhook routines; multiplayer; shared `.context`; JSON schema for config.
- **Weaknesses vs ptyro:** Mac-only; closed; cloud stores chats; app-owned terminals (no detach); no identity profiles; no documented hook status engine.
- **Ideas:** publish a **JSON Schema for `.ptyro.toml`** (R-UX-05); `.ptyro/context/` hand-off folder, git-excluded, linked into every session (R-ORC-20); "promote" command beyond M6.7; webhook-triggered recipes (R-RUN-03); keep the M3.11 env aliases.

#### B2. Orca (Stably AI) — credited inspiration (SPEC §20.6)
- **URL:** https://www.onorca.dev · https://github.com/stablyai/orca · MIT, free
- **Stack:** Electron/Vite/TS [U]; WebGL terminal
- **Maturity:** ≈99k★ on GitHub (one directory said ≈55k — [U]); ships daily; macOS/Windows/Linux; iOS + Android companion.
- **What it is:** a worktree per task; ~27 preconfigured CLI agents; one prompt to several agents, compare, merge the winner; WebGL terminal with splits and restored scrollback; VS Code-style editor; GitHub & Linear boards; SSH worktrees; diff annotations to the agent; Chromium **Design Mode**.
- **Special:** Design Mode (click an element → HTML, CSS, cropped screenshot to the agent); **account switcher with usage tracking** (Claude & Codex 5 h / 7 d windows, reset times, hot-swap Codex accounts without re-login, also from the phone); SSH worktrees with reconnect, port forwarding, passphrase caching; agent-facing CLI (`orca worktree create`, `snapshot`, `click`, `fill`) and agent-updated **"worktree comments"**; cross-search.
- **Gaps:** design mode/browser; usage meters + account hot-swap (ptyro: backlog); mobile apps (M9); SSH worktrees with port forwarding; boards; agent-written per-worktree notes.
- **Weaknesses vs ptyro:** Electron; not a multiplexer (no detach, no plain-SSH story); no identity isolation (git author/SSH/gh); undocumented status semantics.
- **Ideas:** per-profile usage meter in the status bar (R-USE-01); agent-written one-line note per session via `ptyro note` or OSC 7501 `msg` (R-UX-06); out-of-process `ptyro browser` helper (R-EXT-02).

#### B3. Superset (superset.sh)
- **URL:** https://superset.sh · https://github.com/superset-sh/superset · **Elastic License 2.0** (source-available) · desktop free; mobile needs Pro (~$20/seat [U])
- **Stack [V]:** Electron, React, Bun, Turborepo, Drizzle, tRPC, xterm.js, node-pty
- **Maturity [V]:** ≈15.4k★, 5,035 commits; YC; macOS primary, Linux experimental; iOS app; SOC 2 Type II.
- **What it is:** ~25 CLI agents in worktrees; sidebar working/blocked/waiting with chimes and dock badges; editable diff viewer; in-app browser with per-workspace port detection; **terminal presets**; persistent sessions; rich prompt editor; **"any machine as a host"**; **Automations** on schedule or event (daily triage, Sunday changelog, weekly dep upgrades, PR review on review-request — each produces a PR); `superset-mcp`; TS SDK; `.superset/config.json` (setup/teardown/run). README honestly states worktrees don't sandbox processes or prevent conflicts.
- **Gaps:** schedules & event automations; remote hosts; iOS; browser; editing in diff; presets/layouts (ptyro backlog: session templates).
- **Weaknesses:** Electron; macOS-first; not OSI; no precedence model; no profiles.
- **Ideas:** recipe `schedule` (cron) and `trigger` (gh review-request polling) (R-RUN-02/03); promote session templates (`[layouts.dev]` = agent + shell + dev server) to M3/M4 (R-UX-07); document the same honest limits.

#### B4. Emdash (General Action)
- **URL:** https://emdash.com · https://github.com/generalaction/emdash · Apache-2.0 [V] · YC W26
- **Maturity:** ≈6.0k★; docs (2026-10-17) mention "Emdash 2.0" early access [U].
- **What it is:** worktree + branch per task; auto-detects 22–28 agent CLIs; side-by-side diffs across agents; PR, CI, merge; **issue intake from ~10 trackers** (Linear, GitHub, Jira, GitLab, Asana, Featurebase, Monday, Forgejo, Plain, YouTrack, Notion); SSH/SFTP remote projects with credentials in the OS keychain; marker-tagged hooks written into user-level agent config that are inert outside Emdash [V]; local SQLite.
- **Gaps:** multi-tracker intake; OS keychain credentials.
- **Weaknesses:** Electron; writes into users' agent config (ptyro deliberately doesn't — SPEC M4.1 materialises integrations under `data/integrations/`); no multiplexer; no profiles.
- **Ideas:** pluggable issue sources starting with `gh issue`, then Linear (R-GIT-02); `keyring` backend for profile secrets next to `command = [...]` (R-PRF-03).

#### B5. Vibe Kanban (BloopAI) — company shut down 2026-04-10
- **URL:** https://github.com/BloopAI/vibe-kanban · https://vibekanban.com/blog/shutdown · Apache-2.0 · ≈28.4k★ · README: "sunsetting" [V]
- **Stack:** Rust backend (sqlx) + TS web UI — architecturally closest to ptyro.
- **What it is:** moving a card to In Progress creates a worktree and starts an agent (~10 agents); each workspace has branch, terminal, dev server; inline review comments → agent; preview browser with inspect mode and device emulation; PR with AI-written description; MCP server.
- **Lesson:** "no business model" — most users were on free plans. Agents-as-async-workers on a board is a compelling view.
- **Ideas:** a **board view** in the sidebar — columns Backlog / Working / Blocked / Review / Done driven by status engine + PR state (R-UX-08); `ptyro pr create --describe` asks the session's agent for the body (R-GIT-06); a "Coming from Vibe Kanban" doc — ptyro has no service to shut down.

#### B6. Crystal → Nimbalyst (Stravu)
- **URL:** https://nimbalyst.com · MIT (vendor claim) · Crystal's last release v0.3.5 (2026-02-26), deprecated
- **What it is:** parallel sessions with optional worktrees; session kanban; WYSIWYG markdown/mockups/diagrams/data models; tasks; **agent-written wiki** (also an MCP plugin for any agent); iOS approvals; Claude + Codex first-class, others alpha via ACP.
- **Gaps:** shared agent-written knowledge base / decision log (ptyro: M11 ledger is a narrow version).
- **Ideas:** per-project `decisions` log fed via `ptyro mcp` `log_decision`, surfaced in the inbox — generalise the M11 ledger to single repos (R-ORC-20).

#### B7. Sculptor (Imbue)
- **URL:** https://imbue.com/sculptor · closed · free in beta [U]
- **Maturity:** rebuilt 2025-09-26; Apple Silicon + Linux; Codex added 2025-12; cached dev-container images cut start-up from minutes to seconds (2026-04 blog).
- **What it is:** each agent in its own Docker container with its own repo copy and branch; **Pairing Mode** two-way syncs a container with your local checkout (cited by ptyro M6.7); **flags potential merge conflicts between agents** and hands them back; "Suggestions" beta reviewer; full history saved; fork-from-history planned.
- **Gaps:** containers (backlog); **conflict prediction across worktrees**; fork from history; background reviewer.
- **Ideas:** **`ptyro conflicts` / sidebar conflict radar** via `git merge-tree --write-tree` between sibling worktrees (R-ORC-11); `[sandbox] = "devcontainer"` with a per-project cached image (R-SEC-05); `ptyro fork` (R-AGT-04).

#### B8. Jean (coollabs, makers of Coolify)
- **URL:** https://jean.build · https://github.com/coollabsio/jean · Apache-2.0 [V] · Tauri v2 + Rust + React · ≈1.3k★
- **What it is:** many projects with worktrees (create/archive/restore/labels); Plan / Build / Yolo modes with plan approval; "magic" prompts (investigate, review, commit message, merge conflicts, release notes); PRs as worktrees, **auto-archived on merge**; **headless `jean-server`** (systemd/Docker) serving the full UI over HTTP+WS with token auth; **desktop connects to several servers** at once; **"Mr. Robot"** (beta) creates a worktree per open issue and executes approved plans; RTK/"Caveman" token-saving plugins.
- **Gaps:** one client ↔ many daemons; issue → plan → approval batch flow; auto-archive on merge.
- **Ideas:** `gc.archive_on_merge = true` running the safe-removal matrix (R-WT-09); multi-daemon sidebar as M9 polish (R-REM-01); built-in `triage-issues` recipe — one worktree per labelled issue in plan mode behind a gate (R-ORC-06).

#### B9. Claudia / opcode
- **URL:** https://github.com/winfunc/opcode · AGPL-3.0 · Tauri · ≈20k★, slowing
- **What it is:** browse Claude projects & sessions, **session timeline with restorable checkpoints**, custom agents, usage analytics, MCP manager.
- **Ideas:** single-worktree checkpoints at every turn end via the M11.8 ref mechanism, `ptyro restore <session> --turn N` (R-ORC-03).

#### B10. Claude Code desktop — Code tab (Anthropic)
- **URL:** https://code.claude.com/docs/en/desktop · proprietary, in Claude plans · macOS, Windows, Linux beta
- **What it is [V]:** session sidebar (group, filter, side-by-side); optional worktree per session under `.claude/worktrees/` with `.worktreeinclude`, cleaned ~30 days after archive; draggable panes (chat, diff, browser, terminal, file, plan, tasks, subagent, iOS Simulator); click a diff line to comment, ⌘↵ sends all; `/code-review` findings cards ("Fix this one", "Apply fixes"); PR monitoring with **Auto-fix** (failing checks + review comments from trusted authors) and **Merge when ready**; auto-archive on PR merge/close; Browser pane verifying against dev servers from **`.claude/launch.json`** (`autoPort`); side chats; **cross-session work** — Claude lists, reads, messages, renames, archives your other sessions and suggests "task chips" for new worktree sessions; SSH sessions; cloud handoff; Dispatch; usage ring.
- **Gaps:** merge-when-ready + review-comment auto-fix (M6.5 has only "fix failing checks"); `.claude/launch.json` import; findings-card UX; cross-session messaging is now first-party (ptyro's M7.7 must be **cross-agent** to matter).
- **Weaknesses:** Claude-only; no multi-account UI; not usable over plain SSH/tmux; closed.
- **Ideas:** import `.claude/launch.json` as `[scripts.run]` with a port from the block (R-WT-03); `ptyro pr automerge` (wraps `gh pr merge --auto`) and "address review comments" (R-GIT-01); headline M7.7 as cross-agent.

#### B11. Codex app (OpenAI)
- **URL:** https://learn.chatgpt.com/docs/app · proprietary · macOS (2026-02-02), Windows, Linux preview [S]
- **What it is:** threads local or in a worktree under `$CODEX_HOME/worktrees`, **handoff of a live thread between local checkout and worktree**; committable `.codex/` "local environments" with setup scripts and project **actions** (buttons); **Automations** on schedule, webhook or manually with a review queue for their output; 2026-04 overhaul added background computer use, in-app browser, 90+ plugins.
- **Gaps:** session handoff checkout ↔ worktree; actions palette; automations.
- **Ideas:** `ptyro wt adopt <session>` — promote a main-checkout session into a new worktree carrying uncommitted changes, resume the agent there (R-WT-05); `:` actions palette listing `[scripts.*]` and recipes (R-UX-09); import `.codex/` setup scripts (R-WT-03).

#### B12. GitHub Copilot app (technical preview, ~2026-06-17)
- **URL:** https://infoq.com/news/2026/06/github-copilot-app
- **What it is:** "My Work" view unifying sessions, issues, PRs and automations across repos; worktree per session; **Canvases** (plans, PRs, terminals, deployments) shared by humans and agents; **Agent Merge** drives review → checks → merge incl. CI fixes and feedback; **local sandboxes with filesystem + network policy**; ephemeral cloud VMs; Anthropic/OpenAI/Google models.
- **Ideas:** optional **`sandbox = "landlock"`** spawn mode on Linux (writes limited to the worktree + caches; network allowlist) — cheap vs containers and unique for a TUI (R-SEC-05); a "My Work" sidebar section (gh PRs awaiting you + assigned issues → "start worktree") (R-GIT-02).

#### B13. Cursor 3 Agents Window
- **URL:** https://cursor.com/changelog/3-0 · Cursor 3.0 released 2026-04-02 [V]
- **What it is:** many agents across local / worktree / cloud / SSH; sidebar includes agents started from Slack, GitHub, Linear, mobile, web; `/worktree`, `/best-of-n` (one task, several models, each in a worktree, compared); grid view of agent tabs; **Await tool** — agents wait for background shells/subagents or for output like "Ready"/"Error"; Design Mode; push a local agent to the cloud.
- **Ideas:** **`ptyro wait --match 'Ready|Error'`**, also via MCP (R-ORC-22); tile view (prefix T) — auto-grid every session in a status (R-UX-10).

#### B14. Warp — ADE + Oz (see also C-segment)
- **What it is (GUI side):** third-party CLI agents first-class (Claude Code, Codex, Gemini, OpenCode); vertical tabs with branch/worktree/PR metadata; **Tab Configs** (dir, startup commands, theme, worktree in one schema); notification centre fed by a Warp plugin for Claude (needs `jq`); review comments to a running third-party agent; voice; saved /prompts; publish a session to the cloud for phone steering; **Oz** cloud agents with shareable URL per agent, governance, audit logs, cross-harness subagents (launched 2026-02-10; multi-harness beta 2026-05).
- **Ideas:** session-scoped share tokens `ptyro share <session> --read-only --ttl 1h` (R-REM-09); prompt library as `@snippet` expansion in `ptyro send` and the TUI (R-UX-11).

#### B15. Google Antigravity 2.0 — Agent Manager
- **Maturity:** 1.0 Nov 2025; 2.0 at I/O 2026-05-19 [S]; Antigravity CLI replaced Gemini CLI for consumers on 2026-06-18 (confirmed in segment G).
- **What it is:** parallel agents, dynamic subagents, "Agent Teams" in sandboxes; pause/redirect/kill; **Artifacts** — task lists, implementation plans, diffs, walkthroughs, screenshots, browser recordings — that you comment on and the agent revises without restarting.
- **Ideas:** **plan review** — when a session is blocked on plan approval (Claude `ExitPlanMode`), open the plan in the M6 inspector with line comments, then approve or send comments (R-GIT-07); retarget M4.8 to the Antigravity CLI (R-AGT-01).

#### B16. JetBrains Air (+ Junie CLI)
- **URL:** https://www.jetbrains.com/help/air/ · free download; JetBrains AI or BYO
- **Maturity:** preview from 2026-03 (macOS), Windows 2026-06, July added Copilot, OpenCode, Pi, Cline, **any ACP Registry agent**, local models [V].
- **What it is:** tasks run concurrently in one of three modes — local, **Docker container**, or **git worktree**; cloud preview; IDE code intelligence in agent context.
- **Ideas:** ACP Registry discovery (R-ACP-01); choose execution target per task in the new-worktree wizard (worktree / container / remote host) so the architecture leaves room now (R-SEC-05).

#### B17. VS Code Agents window / Agent Sessions
- **URL:** https://code.visualstudio.com/docs/agents/run/sessions/manage-sessions (2026-10-14)
- **What it is:** sessions across workspaces with status, type, file changes, pins, groups; worktrees for Copilot sessions; **archiving commits uncommitted worktree changes to the session branch before removing the worktree** — restorable; "Worktree Cleanup" by inactivity; auto-done when PR merges; **app badge counts unread, waiting-for-input and failing-CI**; an "Agent Host Protocol" (~1.129).
- **Ideas:** **`ptyro wt archive`** = snapshot to `refs/ptyro/archive/<wt>`, remove worktree, keep branch; `ptyro wt restore` — a safe alternative to refusing removal (R-WT-06); `ci_failed` attention event + outer-terminal title badge (R-NOT-03).

#### B18. Zed — Parallel Agents & Terminal Threads
- **URL:** https://zed.dev/docs/ai/parallel-agents.html · https://zed.dev/blog/terminal-threads (2026-05-20) [V]
- **What it is:** Threads Sidebar grouped by project mixing Zed Agent threads, external **ACP** threads (Claude Code, Codex…), and **Terminal Threads** — a raw `claude`/`amp`/`codex` terminal becomes a sidebar entry with attention notifications. Amp uses `AMP_FORCE_BEL=1`.
- **Special:** the only editor mixing structured ACP threads and raw terminal agents in one list — exactly ptyro's M10 vision.
- **Ideas:** document per-agent "force notification" env vars in generic launch plans; ensure the generic adapter maps BEL / OSC 9 / 777 to attention.

#### B19. Kiro (AWS)
- **URL:** https://kiro.dev · Pro / Pro+ / Power
- **What it is:** prompt → requirements → design → **task graph**; independent tasks run in parallel batches of 3; the graph is immutable while running [V]; the autonomous agent example opens 15 PRs across 15 microservices.
- **Ideas:** built-in `spec-to-tasks` recipe (R-ORC-06). The 15-service example overlaps M11 — ptyro's contract + merge train are more rigorous.

#### Other 2026 entrants
Parallel Code (MIT, Electron/SolidJS), FleetCode, ChatML — confirm commoditisation; nothing new.

---

### C. Terminal emulators, multiplexers and session-persistence tools

> **Headline (2026-10-20):**
> 1. **OSC 7501 is now a public spec by Mitchell Hashimoto** — "Program Status Protocol",
>    launched 2026-10-06 (https://mitchellh.com/writing/program-status-osc7501), spec at
>    https://www.superlogical.com/rex/docs/build/program-status. Researchers saw **rev 0.2
>    (2026-10-06)** and **rev 0.4 (2026-10-10)** — it is moving weekly. libghostty-vt parsing merged
>    for Ghostty 1.4 (PR #14560, the GUI ignores reports for now) [V]; Rex is the reference
>    implementation; tuios implements it; emitters include Claude Code (2.1.295+), Crush v0.98,
>    opentui, plus PoCs for Codex/Terraform/Homebrew. ptyro already answers the probe and consumes
>    reports (SPEC §5.6, §9.2) — **audit against the latest revision** (R-STA-01).
> 2. **Rex (Superlogical, beta)** — a client/server multiplexer from the spec's author with native
>    7501, Lua events, an agent skill, and a planned mobile push + "unified priority inbox". The
>    closest new competitor to ptyro's core.
> 3. **tmux 3.7/3.8 is a quiet threat:** OSC 9;4 into formats, floating panes (3.7); OSC 133 → hook
>    events, key/value hooks, `wait-for -E`, monitor hooks, `pane_last_output_time`, line timestamps
>    (3.8, 2026-09-09). "tmux + a plugin" now covers much of a status story.
> 4. **iTerm2 3.7** ships first-class Claude Code status via its own `OSC 21337`, a Cockpit panel,
>    snooze, and an iOS companion.
> 5. **cmux** (~28k★) leads agent terminals but does **not** keep processes across relaunch — it
>    delegates detach to tmux/Zellij. That is ptyro's clearest structural advantage.
> 6. **Zellij 0.44** already ships a production web client + PWA + mobile UI with read-only tokens
>    — roughly M9.2/M9.3.
> 7. **Contour 0.7** added experimental daemon sessions over TCP+TLS and tmux control mode both ways.
> 8. **OSC 9;4 progress** is near-universal (Ghostty, kitty 0.47+, WezTerm nightly, Windows Terminal,
>    tmux 3.7+).

#### C1. cmux (manaflow)
- **URL:** https://github.com/manaflow-ai/cmux · https://cmux.com · GPL-3.0+ app; server parts BSL 1.1 [V]
- **Stack:** Swift/AppKit on libghostty (reads Ghostty config) · **Maturity [V]:** 28.3k★, nightly builds, YC
- **What it is:** macOS terminal with vertical workspace tabs for parallel agents; blue ring on panes needing attention; notification panel; ⌘⇧U to newest unread.
- **Special:** *every action* via CLI + Unix socket (workspaces, splits, send input, read screen, **screenshots**); `cmux hooks setup` for ~8 agents; **scriptable in-app browser pane** (accessibility snapshot, click, fill, eval JS, console/network logs); sidebar git branch + ports; `cmux ssh` (routes browser panes via the remote; drag-image upload via scp); iOS companion on TestFlight.
- **Gaps:** browser pane; phone app; one-command hooks across agents; screenshot API; image drag over SSH; polish.
- **Weaknesses vs ptyro:** macOS-only GUI; **processes don't survive relaunch** (layout + best-effort scrollback, auto-resume by session id); no worktree model [U]; BSL server.
- **Ideas:** `ptyro hooks setup` auto-detect + idempotent install (fold into `ptyro init`, R-AGT-06); "jump newest unread" and "cycle unread without marking read" (R-UX-02); sidebar metadata row (branch, ports, last line); `ptyro screenshot <session>` → PNG/SVG (R-UX-12).

#### C2. Rex (Superlogical) — new, highly relevant
- **URL:** https://www.superlogical.com/rex · license/pricing not stated · macOS app + Linux server · Lua 5.1 config · `rex` CLI · self-documenting server API · beta
- **Special:** OSC 7501 reference implementation; session-picker indicators for completed/blocked; spinners on unfocused tabs; Lua `terminal.program_status_changed` event; built-in **agent skill** so Claude/Codex can open tabs, split, read screens; per-user login server mode; no telemetry. Planned: mobile push, **unified priority inbox**.
- **Gaps:** scriptable event hooks; agent skill for driving the mux (ptyro plans MCP in M7.6); multi-user server; it sets the protocol's direction.
- **Weaknesses:** beta; macOS client; no evidence of worktrees, adapters, review or orchestration [U].
- **Ideas:** filterable `ptyro events --follow` (M5); ship a **Claude Code skill/plugin** teaching `ptyro send/read/split` before MCP (R-ORC-18); track the spec and file feedback (multiplexer passthrough semantics are unspecified).

#### C3. Warp (terminal side; see B14)
- **URL:** https://github.com/warpdotdev/warp · **AGPL-3.0** (open-sourced 2026-04-28; `warpui*` crates MIT) [V] · Rust GPU UI · 65.6k★
- **Special:** blocks UI; code review panel; vertical tabs; modes from pure terminal to full ADE; agent-editable settings file; Oz cloud agents.
- **Weaknesses:** heavy GUI, no plain-SSH use, no daemon-owned PTYs [U, high confidence]; account-oriented; third-party agents second-class.
- **Ideas:** a `--ui minimal|full` ladder so ptyro can be "just a multiplexer" for sceptics (R-UX-13); documented, agent-editable config with JSON Schema (R-UX-05).

#### C4. Ghostty + libghostty
- **URL:** https://ghostty.org · MIT · Zig · 1.3.0 (2026-03-09), 1.4 slipped into October with "1000+ changes" [S]
- **1.3 [V]:** scrollback search; `notify-on-command-finish` (never/unfocused/always, after N s) via OSC 133; fuller OSC 133 incl. `click_events` / `cl=line` click-to-move cursor; AppleScript control (preview); fixed a leak Claude Code triggered. OSC 9;4 renders a per-split progress bar with ~15 s timeout.
- **libghostty-vt:** zero-dependency VT parser/state with Zig + C APIs (macOS, Linux, Windows, WASM); **no tagged release, API unstable**, no Rust bindings found; used by zmx, gpui-ghostty, herdr (likely). OSC 7501 parsing landed for 1.4.
- **Ideas:** OSC 133-driven command-finish notifications for plain shell panes (R-NOT-04); support OSC 133 `cl=line` click-to-move inside panes; keep the libghostty backend in backlog (OSC 7501 now native there); forward OSC 9;4 outward when the outer terminal is Ghostty (R-STA-07).

#### C5. iTerm2 3.7 (2026-09)
- **URL:** https://iterm2.com · GPL-2.0 [U]
- **Special [V]:** **Session Status** via `OSC 21337;status=Working;indicator=#ffa500;status-color=…;detail=… ST`, `it2 session set-status`, or triggers; Session Status list sorted waiting → working → idle with **snooze** (dims and sinks until the next status change); **Cockpit** floating panel across windows; Claude Code hook installer writing `cc-status` hooks into `~/.claude/settings.json`, treats Claude background tasks as working, detail line shows the tool needing permission, **repair prompt if Claude drops the hook**; Claude Code Workgroup (Chat, Diff, Code Review; findings back via "Clippings"); orchestration AI chat seeing all sessions; session archive; **iTerm2 Buddy** iOS (Noise-encrypted, relayed, QR pairing).
- **Ideas:** accept **OSC 21337** as a low-confidence status source (R-STA-01); **snooze** in the inbox (R-UX-02); hook health check + auto-repair banner (fold into `doctor`, R-AGT-06); treat "background tasks still running" as working; QR pairing for M9.

#### C6. kitty
- **URL:** https://sw.kovidgoyal.net/kitty · GPL-3.0 · 0.49.2 (2026-10-01) [V]
- **2026 [V]:** 0.47 OSC 9;4 progress, drag-and-drop kitten (over SSH), `kitten @ ls` session name + last-focused; 0.48 vertical tabs, `get-text` on alt screen; 0.49 `kitten @ screenshot`, session layouts; 0.49.2 `detect_url_regex` for clickable file references. Owner of kitty keyboard, graphics and **OSC 99** notifications (actions, icons, close reports). A kitty PR references OSC 7501 [V mention].
- **Ideas:** clickable `path:line:col` in agent output → `$EDITOR` or M6 diff (R-UX-14); **emit OSC 99 with action buttons** ("Approve", "Open") when the outer terminal supports it — remote approvals without a phone (R-NOT-02); `last_focused` in `ptyro ls --json`.

#### C7. WezTerm
- **URL:** https://wezterm.org · MIT · Rust/Lua · last stable **20240203** — nightlies only [V]
- **Special:** real mux domains (local/SSH/TLS); `tmux -CC` client "now very usable" [V nightly]; OSC 9;4 via `pane:get_progress()`; Lua everything.
- **Weaknesses:** stale stable, slowed solo maintainer; no agent features.
- **Ideas:** a **tmux control-mode (`-CC`) facade** so iTerm2/WezTerm/Contour render ptyro sessions as native tabs — large, strategic, unplanned (R-EXT-03); `progress` field in `ls --json`.

#### C8. Zellij
- **URL:** https://zellij.dev · MIT · Rust, WASM plugins (wasmtime → wasmi in 0.44) · 0.44.0 (2026-03-23) → 0.44.3 (2026-05-13) [V]
- **Special [V]:** **web client** (opt-in; `zellij-no-web` build), hashed login tokens, **read-only tokens**, TLS, **bookmarkable URLs that start/attach/resurrect a session**, **mobile UI** (touch scroll, pane/tab pickers, on-screen keyboard, single-pane fullscreen), **PWA install**, `base_url` for proxies; **remote `zellij attach https://…`** with `--token`/`--remember`; resurrection with `post_command_discovery_hook`; 0.44 native Windows, CLI `list-panes`, `send-keys`, **`subscribe`**, `detach`, blocking `run`, plugin APIs (read scrollback, highlight, recolor), protobuf client/server contract; 0.44.1 OSC 99 forwarding; 0.44.2 CSI 2031. Community agent plugins: zjbar, cc-zjstatus, cc-zellij-tab-status.
- **Gaps:** M9.2/9.3 already shipped; read-only tokens; plugins; Windows; `subscribe`.
- **Weaknesses:** no native agent status (community plugins); no worktrees/orchestration; resurrection re-runs commands.
- **Keys [S, 2026-10-22]:** the default preset quits Zellij on `Ctrl+q`, which is ptyro's default prefix since 2026-10-22. Inside Zellij, use its unlock-first preset or change `[ui] prefix` (SPEC §21.2).
- **Ideas:** **read-only device tokens**; bookmarkable `/<session>` attach-or-resume URLs; mobile key row (Esc, Ctrl, arrows, **y/n for approvals**) (R-REM-03); `ptyro subscribe`; zjbar-compatible status feed = backlog `ptyro status-line` (R-EXT-04).

#### C9. tmux 3.6–3.8 and its agent plugin ecosystem
- **URL:** https://github.com/tmux/tmux · ISC · **3.8 (2026-09-09)** [V]
- **Changes [V]:** 3.6 scrollbars, mode 2031, OSC 52 detection; 3.7 floating panes, OSC 9;4 → formats; 3.8 OSC 133 → `pane-command-started/finished`, `pane-shell-prompt`; hooks as key/value events; `wait-for -E`; `set-hook -B` monitor hooks; new client/pane-activity/prompt-change/window/zoom hooks; `pane_last_output_time`, `pane_output_generation`; `capture-pane -I` line timestamps; JSON-subset layouts; built-in themes; mouse on by default.
- **Ecosystem [S]:** tmux-agent-sidebar (Claude, Codex, OpenCode; prompts, tool calls, subagent tree), agent-manager, tmux-ide, ATM (dashboard with cost), clorch, agent-status-tmux.
- **Ideas:** per-line arrival timestamps in scrollback (`read --timestamps`) for M8 (R-OBS-03); `output_generation` + `last_output_time` in `ls --json`; a monitor-hook pattern `ptyro on <event> --when '<expr>' -- cmd` as a light alternative to plugins (R-STA-06); **`ptyro import tmux`** migration story (R-UX-15).

#### C10. Wave Terminal
- **URL:** https://github.com/wavetermdev/waveterm · Apache-2.0 · Electron/TS/Go · 22.5k★ · v0.14.5 (2026-04-16) [V]
- **Special [V]:** **`wsh badge`** — block badges with colour + priority that roll up to the tab bar; **PID-linked badges auto-clear on process exit** ("Great for use with Claude Code hooks"); Claude icon in block header; durable SSH sessions; remote file editing; BYO-key AI.
- **Ideas:** **`ptyro badge`** CLI (set/clear with priority, optional PID lifetime) so any script raises attention without OSC 7501 (R-NOT-05); priority roll-up pane → workspace → project.

#### C11. Contour 0.7 (2026-08-23) [S]
- **URL:** https://github.com/contour-terminal/contour · C++/Qt
- **Special:** experimental `contour daemon` sessions (TCP + TLS + pre-shared token); **tmux control mode both directions**; kitty graphics, iTerm2 images, ReGIS, Good Image Protocol; accessibility (caret exposure, screen-reader labels).
- **Ideas:** control-mode facade (R-EXT-03); **accessibility pass** — screen-reader-friendly status announcements, a linear "plain" mode (R-UX-16).

#### C12. Windows Terminal
- **URL:** https://github.com/microsoft/terminal · MIT · 1.24 stable / 1.25 preview (2026-03) [V]
- **Special:** mode 2026 synchronized output (1.24); **kitty keyboard protocol** in 1.25 preview (Shift+Enter for agents); taskbar OSC 9;4 [U].
- **Idea:** forward aggregate progress via OSC 9;4 when `WT_SESSION` is set (WSL users).

#### C13. Rio and Tabby (minor)
- **Rio** (MIT, Rust+wgpu, WASM build `librio-wasm`, ≈7.6k★): no agent/persistence features; its WASM path could inform an M9 canvas renderer.
- **Tabby** (≈70.9k★, Electron, last push 2026-05 [S]): SSH/serial profiles, plugins; no agent story — a reminder that stars without an agent story stagnate.

#### C14. zmx
- **URL:** https://github.com/neurosnap/zmx · MIT · **Zig + libghostty-vt** · ≈2.2k★ · v0.8.1 [V]
- **What it is:** daemon + client per session over a Unix socket; libghostty-vt mirrors state, snapshot on reattach; **native terminal scrollback**; multi-client; deliberately no windows/splits.
- **Special:** agent-oriented CLI — `run -d`, `send`, **`print`** (inject display text), `write <file>`, `history --vt|--html|--screen`, `wait`, `tail`, labels; `ZMX_SESSION_PREFIX` grouping; README warns agents about **recursive attach** via `ZMX_SESSION`.
- **Ideas:** `ptyro read --format html|vt|text` (HTML transcript sharing) (R-OBS-03); **guard recursive attach** when `PTYRO_SESSION` is set (R-UX-17); `wait --prefix/--project` group waits; `ptyro print <session> <text>` annotation lines ("ptyro: approved by phone").

#### C15. shpool
- **URL:** https://github.com/shell-pool/shpool · Apache-2.0 · Rust (`shpool_vt100`) · ≈2.0k★
- **Special:** named persistent shells with **native scrollback**; templated names + `shpool var set`; prompt prefix; `list --json`; `--ttl`.
- **Ideas:** `--ttl` for throwaway sessions (complements hibernation); optional prompt hint showing session + worktree.

#### C16. abduco / dtach
- Minimal detach/attach, frozen; no state restore. **Idea:** advertise exact screen restore in README; `ptyro attach --readonly`.

#### C17. mosh / Eternal Terminal
- mosh stuck at 1.4.0 (2022) in 2026 distros [S]; ET 6.2.8 [S].
- **Gap:** roaming + **predictive echo**. **Ideas:** make M9.1 reconnect-transparent (sequence-numbered stream, resend frame diff) (R-REM-10); optional predictive echo for the remote input line; document mosh/ET as a transport fallback.

#### C18. Browser/terminal sharing — ttyd, tmate, sshx, upterm
- **ttyd** (MIT, ≈12.5k★, C/libuv + xterm.js WebGL2; sixel, ZMODEM, basic auth, TLS) [V].
- **tmate** — dormant [U].
- **sshx** (MIT, ≈7.7k★, Rust): E2E (Argon2 + AES), server blind; **infinite canvas** of terminals; live cursors and chat; predictive echo; **no self-hosting** [V].
- **upterm** (Apache-2.0, ≈1.3k★, Go): share over SSH/WebSocket; **authorise by forge username** (`github:user`, GitLab, Codeberg…); forced command; self-hostable `uptermd`; `action-upterm` for CI debugging [V].
- **Ideas:** **`ptyro share <session> [--readonly] [--allow github:alice]`** on the M9 server + relay with upterm-style forge-key auth (R-REM-09); sshx canvas as inspiration for a PWA multi-agent grid; status-aware sharing (show 7501 state + inbox, not just pixels).

#### C — Terminal protocol landscape 2026

| Sequence | Purpose | Support | ptyro status / suggestion |
|---|---|---|---|
| **OSC 7501** Program Status Protocol | Per-terminal status records: `state=idle\|working\|done\|blocked\|error\|clear`, `kind` (`permission\|question\|auth`), `progress`, hierarchical `id` (≤8 levels), `app`, base64 `title`/`msg`; probe `OSC 7501;?` → `OSC 7501;?:states=…:kinds=…` then DA1 | Spec rev 0.4 (2026-10-10) [V]; libghostty-vt (Ghostty 1.4, parse only) [V]; Rex [V]; tuios [V]; Claude Code 2.1.295+ [V]; Crush 0.98 [V]; opentui [U]; Codex/Terraform/Homebrew PoCs; kitty PR [V mention] | **Implemented.** Audit vs latest rev: ids & subtrees; LRU eviction at 256 records; reject control chars in decoded base64; `done`/`error` survive OSC 133 A and exit while `working`/`blocked` drop; RIS clears, DECSTR doesn't; stop mapping OSC 9;4 onto the root record once 7501 seen; rate-limit notifications. **Re-emit outward** for the focused/aggregate session. |
| **OSC 21337** (iTerm2) | Presentational status: `status`, `indicator`, `status-color`, `detail` | iTerm2 3.7 [V] | Accept as low-confidence input; the 7501 spec calls it misaligned. |
| **OSC 9;4** (ConEmu progress) | `9;4;st;pct` — 0 off, 1 progress, 2 error, 3 indeterminate, 4 paused | Ghostty, kitty 0.47+, WezTerm nightly, tmux 3.7+, Windows Terminal [S]; Claude Code emits (flicker bug during tool calls [S]) | Parsed. Add a timeout like Ghostty's; forward an aggregate outward. |
| **OSC 9** / **OSC 777;notify** | Desktop notifications | Ghostty, iTerm2, WezTerm, cmux, WT (partly) | Parsed; notifier M4.6. |
| **OSC 99** (kitty) | Rich notifications with ids, **actions**, close/activation reports | kitty, cmux, Zellij forwarding (0.44.1) [V]; Ghostty [U] | Parsed. **Emit outward with action buttons** where supported. |
| **OSC 133** (semantic prompts) | Prompt/command/exit marks; Ghostty `click_events`, `cl=line` | Ghostty 1.3, tmux 3.8 events [V]; WezTerm, kitty, iTerm2, WT [U]; fish 4.1+, nushell 0.111+ [V] | Parsed; prompt jumps M2. Add command-finish notifications for shell panes; OSC 133 A now matters for 7501 record lifetimes. |
| **OSC 3008** (systemd/UAPI context) | Context push/pop: shell, sudo, container, ssh | Emitted by systemd profile scripts (e.g. newer Ubuntu) [S]; no mainstream consumer | **Swallow silently** (verify alacritty_terminal drops it); later: "elevated" tint under sudo. |
| **OSC 7** / **OSC 52** | cwd / clipboard | Universal | Parsed; OSC 52 read denied (good). |
| **Mode 2026** synchronized output | Flicker-free redraws | WT 1.24 [V]; most others [U]; Claude Code uses it | Ensure frames aren't split mid-update. |
| **Mode/CSI 2031** | Dark/light theme notifications | tmux 3.6, Zellij 0.44.2 [V] | Forward/answer for inner apps. |
| **Kitty keyboard protocol** | Unambiguous keys (Shift+Enter) | kitty, Ghostty, WezTerm, foot, WT 1.25 preview [V] | Implemented (M1). |
| **Kitty graphics / sixel / iTerm2 images** | Inline images | kitty, Ghostty, WezTerm, Contour, iTerm2 | Backlog passthrough. |

No ratified OSC registry exists yet (the freedesktop terminal-wg issue is open [S]). OSC 7501 is the first agent-specific proposal with real momentum; iTerm2's 21337 competes.

---

### D. Cloud / async / background agents

> **Headline (2026-10-20):** cloud agents now commonly run the *whole* loop — task from an issue
> tracker or chat, isolated environment, PR, and then **PR comments and CI failures routed back to
> the agent automatically** (Claude Auto-fix, Jules CI Fixer, Amp "PR comment follow-ups", Cursor
> `@cursor` on PRs). ptyro has no post-PR loop planned; it is the biggest workflow gap in this
> segment. **Hybrid local/cloud** is the 2026 shape: your machine as a "runner" (Amp), Claude Remote
> Control server mode with `--spawn worktree`, Cursor Self-Hosted Machines.

#### D1. OpenAI Codex (cloud, web, CLI, integrations)
- **URL:** https://chatgpt.com/codex · https://learn.chatgpt.com/docs/cloud
- **Class:** first-party cloud agent · proprietary, ChatGPT plans (tier details [U])
- **Maturity:** reusable cloud *environments* with internet allowlists, secrets, OIDC [V]; Linear (assign or mention → cloud task) [V]; Slack [V]. DevDay Oct 2026: closed-laptop cloud sessions, CLI `/agents` view with colour-coded running/waiting/complete/interrupted, `/fork`, Code Review as a GitHub/GitLab reviewer, scheduled security scans that open fix PRs [S]. Best-of-N `--attempts 1–4` [U].
- **Special:** one account across CLI, desktop, web, mobile, Slack, Linear; environments as a first-class reusable object; "request Codex as reviewer".
- **Gaps:** issue-tracker triggers (ptyro: backlog); closed-laptop execution (backlog: cloud/SSH hosts); reviewer-request-triggered review; scheduled scans → PRs.
- **Weaknesses vs ptyro:** OpenAI-only; no local multiplexer; `/agents` covers Codex only.
- **Ideas:** named, versioned **environment objects** (setup, network allowlist, secrets) extending profiles; add **`interrupted`** as a distinct status; review on `ptyro-review` PR label (R-GIT-04).

#### D2. Claude Code cloud sessions, Projects, Routines, Auto-fix (Anthropic)
- **URL:** https://code.claude.com/docs/en/claude-code-on-the-web
- **Class:** first-party cloud agent · in Pro/Max/Team/Enterprise usage, no VM charge [V]
- **Maturity [V]:** `claude --cloud "task"`; `claude -p "msg" --cloud <id> --output-format json` queues a follow-up into a running cloud session from anywhere; `--teleport` / `/teleport` / `/tasks` pull a cloud session plus branch into the terminal; repos without a GitHub remote are bundled and uploaded (credential-named files skipped); self-hosted environments; diff view with inline comments to Claude; **Auto-fix** subscribes to PR CI failures and review comments, pushes fixes, asks when a comment is ambiguous; "Projects" coordinate many cloud sessions; Routines (scheduled or GitHub-event runs); Ultrareview.
- **Special:** moving work between surfaces (terminal → cloud → terminal → phone); GitHub credentials never enter the VM (proxy attaches them); Auto-fix replies on review threads as Claude.
- **Gaps:** **post-PR auto-fix loop** (M6 has PR + checks, no follow-up loop); teleport; fire-and-forget follow-ups from scripts; scheduled/event runs; coordinator over many sessions.
- **Weaknesses:** Claude-only, GitHub-centric; no API-key/Bedrock/Vertex/ZDR; environments expire and lose background work; not a real terminal.
- **Ideas:** `ptyro send … --json` to any session without attaching (already planned M5 — keep it remote-capable); `ptyro watch-pr` (R-GIT-01); session-id marker in PR bodies so `ptyro pr open <url>` finds the session ("teleport by PR", R-GIT-03); skip credential-pattern files when copying worktrees to remote hosts.

#### D3. Google Jules
- **URL:** https://jules.google · proprietary, Google AI Pro/Ultra; quotas 15/100/300 per day [S]
- **Maturity [V changelog]:** **CI Fixer** for Jules' own PRs and selectable commit authorship (Jules / you / co-authored) (2026-02-19); MCP (2026-02-02); "Planning Critic" reviewing auto-approved plans, repoless REST sessions, editable scheduled tasks (2026-01-26). "Jules V2 / Project Jitro" [U].
- **Special:** plan-first async with a critic pass; commit authorship as a setting; scheduled tasks.
- **Gaps:** scheduled/recurring tasks (not on ptyro roadmap); CI fixer loop; plan-review gate before autonomous execution.
- **Weaknesses:** Gemini-only, cloud VM only, GitHub only.
- **Ideas:** per-profile **commit attribution policy** (R-PRF-02); `ptyro schedule "<cron>" <task>` (R-RUN-02); "plan gate" — start in plan mode, approve before execute (R-ORC-06).

#### D4. GitHub Copilot cloud agent + Agent HQ
- **URL:** https://docs.github.com/en/copilot/concepts/agents/coding-agent/about-coding-agent
- **Maturity [V]:** renamed "Copilot cloud agent"; start from agents panel, chat, issues & PRs, MS Teams, Slack, security campaigns; environments via `copilot-setup-steps.yml`; Automations in the repo's Agents tab; one repo / branch / PR per task; **59-minute hard timeout**; Claude and Codex as agents inside Copilot (Feb 2026) with Local/Cloud/Background session types; GA June 2026.
- **Special:** "assign the issue to Copilot" is the default trigger; enterprise governance (designated branches, firewalled sandbox); multi-vendor control plane; GitHub Mobile.
- **Gaps:** issue assignment as trigger; org governance & audit; mobile via an app users already have.
- **Weaknesses:** GitHub-only; time cap; no local terminal; your own Claude subscription doesn't apply.
- **Ideas:** `ptyro issue start <url>` (R-GIT-02); honour **repo-native bootstrap files** (`copilot-setup-steps.yml`, `.cursor/environment.json`, Claude setup scripts) as worktree setup hooks (R-WT-03); per-session wall-clock budget.

#### D5. Cursor Cloud Agents
- **URL:** https://cursor.com/docs/cloud-agent · paid plan, API pricing, spend limit set on first use [V]
- **Maturity [V]:** launch from iOS, web (Android PWA), desktop, Slack/Linear, **`@cursor` on GitHub/Bitbucket PRs and issues**, API; environments via agent-led setup, snapshot, or Dockerfile in `.cursor/environment.json`; Tailscale private networking; **multi-repo environments**; computer use with screenshot/video/log **artifacts** and remote-desktop takeover; cloud-run `.cursor/hooks.json`; Self-Hosted Machines. "Projects" coordinator (2026-09) [S].
- **Gaps:** verification artifacts attached to PRs; multi-repo tasks (ptyro: M11); shipping PWA; per-run spend limit (ptyro: backlog meter only).
- **Weaknesses:** Cursor's own agent; no multiplexer; teammates read-only by default.
- **Ideas:** per-session/profile **spend cap** with pause + notify (R-USE-02); per-worktree `artifacts/` folder that M6 attaches to PR bodies (R-GIT-05).

#### D6. Devin (Cognition) + Windsurf / "Devin Desktop"
- **URL:** https://devin.ai · https://docs.devin.ai · pricing [U]
- **Maturity:** web app with embedded IDE, shell, browser [V]; Slack & MS Teams [V]; **Devin CLI `/handoff`** sends a local task to cloud Devin [V]; Linear/Jira [V]; Windsurf 2.0 "Agent Command Center", rebrand to Devin Desktop (2026-06) [S].
- **Gaps:** handoff of a running local session to remote execution; Teams & Jira triggers.
- **Ideas:** `ptyro handoff <session> --host <ssh>` — commit/stash, push, recreate worktree remotely, resume conversation, keep session id stable (R-REM-05).

#### D7. Factory (Droids)
- **URL:** https://factory.ai · https://docs.factory.com
- **Maturity [V]:** Droid CLI, desktop, web, mobile; cloud session sync; **"Droid Computers"** (persistent, API-addressable remote machines); headless `droid exec`; "Software Factory" automations (triage, review, QA, docs, incidents); "Missions"; analytics API.
- **Gaps:** persistent remote machines as targets; analytics API.
- **Ideas:** `ptyro stats --json` — time blocked vs working, sessions per agent/project, PRs opened/merged (R-OBS-02).

#### D8. Amp (Amp Inc., spun out of Sourcegraph)
- **URL:** https://ampcode.com/news
- **Pricing [V]:** free with your own compute + model keys (2026-09-13); Claude Pro/Max usable (2026-10-10); ChatGPT linkable.
- **Maturity [V, dated news]:** **orbs** (remote VMs, 2026-06-30), event-driven orbs; **agents schedule their own wake-ups** (07-21); agent-to-agent messaging and file exchange across threads (07-17); **runners** — one runner many repos (09-17), **creates worktrees from the composer** (09-22), **shared runners `--share`** (09-24); iOS + macOS apps (08-28); multiplayer threads; **PR comment follow-ups** (10-12); **assign Linear issues to Amp** (10-19).
- **Special:** most complete hybrid — your machine as a runner that teammates and your phone dispatch work to.
- **Gaps:** shared/team runner; self-scheduled wake-ups; inter-agent messaging (ptyro: M7.7); shipping mobile; PR/Linear triggers; live-preview "portals".
- **Weaknesses:** Amp's own harness; closed; no raw terminal multiplexing.
- **Ideas:** agent-callable **`ptyro wake --in 30m --session X "check CI"`** (R-RUN-02); port **portals** via the M9 relay (R-REM-06; `ptyro-pty/src/ports.rs` exists); later, a shared daemon with per-user auth.

#### D9. Ona (formerly Gitpod) — "joining OpenAI"
- **URL:** https://ona.com · enterprise · announced joining OpenAI 2026-06-11 [V]; close status [U]
- **What it is:** background agents task → PR; Automations triggered by PRs, schedules, webhooks; runs in customer VPC with kernel-level policy, audit trails, scoped credentials.
- **Gaps:** sandboxed sessions (backlog); webhook triggers; audit trail.
- **Ideas:** daemon **audit log** `ptyro log --json` of every approval/permission decision (R-SEC-02); `POST /tasks` webhook on the M9 server.

#### D10. OpenHands — Cloud, Agent Canvas, SDK
- **URL:** https://openhands.dev · MIT core, **91.4k★**, pushed 2026-10-20 [V]
- **Maturity [V]:** Agent Canvas (browser control centre; local, self-host, cloud); CLI in maintenance mode; Software Agent SDK; Agent Server (REST + WebSocket); Sandbox Server; **Automation Server** (cron & events); legacy Docker GUI deprecated.
- **Gaps:** automation server; documented REST+WS agent server.
- **Ideas:** publish the M9 WebSocket protocol as a **stable, versioned API with small TS/Python clients** (R-REM-07).

#### Dead / pivoted (A)
- **Terragon Labs** — shut down; `terragon-labs/terragon-oss` last push 2026-02-10 [V]. It validated "run Claude Code CLIs in the cloud".
- **Sweep** — pivoted to a JetBrains assistant [V]. **Charlie Labs, Replit Agent** — not researched.

---

### E. Remote control & mobile companions for local agents

> **Headline:** crowded and open-source. Baseline in 2026: QR pairing, push for "needs permission"
> and "turn finished", approve/deny from the lock screen, E2E with a self-hostable relay, voice,
> multi-machine. ptyro's M9 matches the baseline; everything here *ships today* while ptyro's is
> planned. ptyro's edge must be **terminal fidelity for any program, vendor-neutrality (API keys,
> gateways, Bedrock), per-workspace identities, and CLI/`--json` parity.**

#### E1. Claude Code Remote Control (+ Cowork Dispatch)
- **URL:** https://code.claude.com/docs/en/remote-control · Pro/Max/Team/Enterprise; **no API keys or custom `ANTHROPIC_BASE_URL`** [V]
- **Maturity [V]:** launched 2026-02-25 [S]; `claude remote-control` (server mode), `--remote-control`, `/rc`; `remoteControlAtStartup`; QR code; server flags `--spawn same-dir|worktree|session`, `--capacity N` (default 32), `--permission-mode`, `--continue`/`--session-id`; phone **starts new sessions** on registered machines; diff pane; model/effort control; subagents & workflows visible/stoppable; phone images/files become `@` references; **Trusted Devices (beta)** — per-device passkeys, biometric re-prompt after 18 h, revocable. Outbound HTTPS only; **transcripts stored on Anthropic servers (not E2E)**.
- **Special:** zero setup for subscribers; worktree-per-remote-session server mode; device-bound auth.
- **Gaps:** ships today; trusted-device model; file/photo upload into a session; remote model switching.
- **Weaknesses:** Claude-only; not E2E; no gateways; not a terminal (other programs invisible); no cross-agent board.
- **Ideas:** **WebAuthn/passkey device enrolment** with biometric step-up for approvals (R-REM-02); phone → file in worktree → referenced in prompt (R-REM-08); `ptyro serve --capacity N --spawn worktree`; interop flag to run Claude sessions with `--remote-control` and detect "taken over elsewhere".

#### E2. Happy (slopus/happy)
- **URL:** https://github.com/slopus/happy · MIT · **24.2k★**, pushed 2026-10-19 [V]
- **What it is:** `happy claude` / `happy codex` wrapper; iOS, Android, desktop apps; E2E (relay stores encrypted blobs); self-hostable `happy-server`; team server and **multiplayer sessions**; push for permission/errors, voice, QR [S]; switches local ↔ remote by restarting the session.
- **Gaps:** shipping apps; multiplayer steering; team server.
- **Weaknesses:** mode switch restarts the session (no real PTY multiplexing); Node; weak worktree model.
- **Ideas:** mirror the "server sees ciphertext + routing only" model; one-command self-host relay (`ptyro relay install`); read-only vs steer roles on attach (R-REM-09).

#### E3. Happier (happier.dev)
- **URL:** https://happier.dev · MIT, free [V site]; relation to Happy [U]; repo not located
- **What it is [V]:** **13 agents** plus **custom ACP backends**, per-machine CLI detection; E2E; push for turn finished / permission / question with a "Needs attention" group; **four permission modes** (Default / Read-only / Safe YOLO / YOLO) switchable mid-session; `happier setup` (relay, machine registration, service, QR); self-host relay; **multi-machine with session moves**; worktree per session; **voice approvals**.
- **Gaps:** generic ACP backend; cross-agent permission presets switchable from the phone; session migration; voice.
- **Ideas:** generic ACP agent profile (R-ACP-01); **cross-agent permission-mode abstraction** mapped to each agent's flags (R-SEC-03); "Needs attention" inbox sorted by time blocked.

#### E4. Paseo (getpaseo/paseo) — nearest architectural twin
- **URL:** https://github.com/getpaseo/paseo · Apache-2.0 per README · **21.5k★**, pushed 2026-10-20 [V]
- **What it is [V]:** **local daemon**; clients on Electron desktop, iOS, Android, web, and **CLI** (`paseo run/ls/attach/send`, `--host` for remote daemons); TS SDK `@getpaseo/client`; agents Claude Code, Codex, Copilot, OpenCode, Pi, Antigravity, Muse Code; optional E2E relay (Elixir) or TCP/Tailscale; worktree per agent; split terminals, editor, diffs, PRs, built-in browser; voice; plugins that can answer permission requests; no telemetry.
- **Gaps:** ships mobile/desktop; third-party SDK; permission-answering plugins; `--host` targeting.
- **Weaknesses:** Electron/TS; terminal fidelity likely lower [U]; no per-workspace identity profiles found.
- **Ideas:** **`ptyro --host <target>` for every verb** (R-REM-01); **permission-policy hook** (stdin JSON → allow/deny/ask) before a human is asked (R-SEC-03); README positioning vs Paseo.

#### E5. CloudCLI / Claude Code UI (siteboon/claudecodeui)
- **URL:** https://github.com/siteboon/claudecodeui · **AGPL-3.0** · 14.2k★ [V]; hosted from €7/month
- **What it is:** web UI for Claude Code, Cursor CLI, Codex (OpenCode); file tree + editing; git UI; shell; MCP config synced with `~/.claude`; **auto-discovers existing `~/.claude` sessions**; plugins.
- **Ideas:** **`ptyro adopt`** — scan `~/.claude/projects` / Codex history and resume a session started outside ptyro as a managed session (R-AGT-03).

#### E6. VibeTunnel (amantus-ai/vibetunnel)
- **URL:** https://github.com/amantus-ai/vibetunnel · MIT · 4.7k★, last push 2026-08-05 (slowing) [V]
- **What it is:** macOS menu-bar app + Node server + xterm.js front end; `vt` wraps any command into a browser terminal; ngrok/Tailscale.
- **Ideas:** the PWA should offer a **true xterm.js view of the daemon PTY** (fed by ptyro's own frame diffs) next to the agent-card view — fidelity no chat companion has (R-REM-03); `ptyro serve --tailscale` as the zero-relay step.

#### E7. CC Pocket (K9i-0/ccpocket)
- **URL:** https://github.com/K9i-0/ccpocket · MIT · 1.1k★, created 2026-02-06 [V]
- **What it is:** mobile chat client ↔ Bridge Server (launchd/systemd); Claude + Codex; approvals; worktrees; pairing via **QR, mDNS discovery**, saved hosts, `ws://`/`wss://`.
- **Ideas:** **LAN pairing via mDNS + QR** carrying a one-time secret and the daemon key fingerprint — no account, no relay — as the first M9 step (R-REM-02).

#### E8. Omnara (pivoted to managed-agent platform)
- **URL:** https://github.com/omnara-ai/omnara · Apache-2.0 · 2.9k★ [V]
- **Now:** self-hostable alternative to Claude Managed Agents; `omnarad` outbound-only daemon; sandboxes from 9 providers; **approvals in Slack/Discord threads**; cron. Old mobile product not visible [U].
- **Ideas:** **notification-sink abstraction** — ntfy, Pushover, Slack/Discord webhooks, Telegram, later Web Push — with **signed approve/deny callback links**; can ship long before the PWA (R-NOT-01). (See also *claude-push*: PermissionRequest hook + ntfy.sh Allow/Deny [S].)

#### Also seen (B) [S, mostly via an Agentastic comparison updated 2026-10-05]
Agentastic Companion (iPhone + Mac host, E2E, TestFlight) · Nimbalyst Mobile (E2E sync) · Superset Mobile (relay, $20/seat) · Orca desktop companion · SSH + tmux via Blink / Termius / Moshi (the baseline ptyro's remote TUI should beat outright) · Kibitz (fringe) · Conductor (no mobile app found).

---

### F. Multi-agent orchestration products

> **Headline (2026-10-20):** Claude Code now ships natively much of what M7 Levels 1–3 plan — a
> per-user supervisor with background sessions (`claude agents --json`), agent teams with a mailbox
> and dependency-aware task list, cross-session messaging over per-session Unix sockets with
> idle-notify, and scripted "dynamic workflows" with deterministic resume. ptyro's edge is no longer
> "has orchestration"; it is **agent-neutral, owns the PTY, status-aware across vendors, and
> multi-repo (M11)**. No surveyed tool ships M11-style change sets.

#### F1. Claude Code native multi-agent (subagents, agent view, agent teams, cross-session messaging, dynamic workflows)
- **URL:** https://code.claude.com/docs/en/agents (+ `/agent-teams`, `/agent-view`, `/cross-session-messaging`, `/workflows`)
- **Class:** first-party agent with built-in orchestration · proprietary, in Claude plans
- **Maturity [V 2026-10-20]:** subagents GA; agent view research preview (2026-05-11); agent teams experimental (`CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS=1`); cross-session messaging v2.1.224+, on by default; dynamic workflows on paid plans.
- **What it is [V]:**
  - *Agent view:* per-user supervisor; states working / needs input / idle / completed / failed / stopped; `claude --bg`, `claude attach|logs|stop|respawn|rm`; `claude agents --json` (`state`, `waitingFor`). Background sessions move into `.claude/worktrees/` before editing; on finish auto-commit, push, maybe draft PR, never push main. Supervisor restarts crashed sessions, survives auto-update.
  - *Agent teams:* lead + teammates; mailbox = JSON files `~/.claude/teams/{team}/inboxes/{agent}.json`; shared task list with dependencies and file-locked self-claim; plan-approval & shutdown protocol messages; quality-gate hooks `TeammateIdle`, `TaskCreated`, `TaskCompleted` (exit 2 = block with feedback). Limits: one team per session, no nesting, no teammate resume, **no worktree isolation for teammates**.
  - *Cross-session messaging:* `ListAgents` / `SendMessage` over per-session Unix sockets (`CLAUDE_CODE_MESSAGING_SOCKET` + token); delivered between tool calls or starts a turn when idle; `notify_when_idle` subscriptions (12 h); `crossSessionInbound = accept|hold|refuse`; **a peer message can never approve a permission or change config**; rate/loop throttles.
  - *Dynamic workflows:* JS scripts with `agent()`, `pipeline()`, `parallel()`, `phase()`, JSON-schema structured outputs; deterministic replay for resume (`Date.now()`/`Math.random()` throw); 16 concurrent / 1000 per run; stalled agents auto-restart ≤5×; saved under `.claude/workflows/`. **No mid-run human input.**
  - `/batch`: splits one change across 5–30 worktree subagents.
- **Special:** zero-install, deep semantics (structured mailbox, task DAG, plan approval) and a real security model for agent↔agent messages.
- **Gaps (positive, vs ptyro):** structured per-step outputs + replay-on-resume (M7.4 lacks both); verify/vote idioms; message trust model (M7.7 doesn't specify one); auto commit/push/draft-PR on finish.
- **Weaknesses vs ptyro:** Claude-only workers; no multiplexer (can't watch five live TUIs except via tmux split); no cross-harness best-of-N compare; no multi-repo, merge train, set checkpoints; no human gates mid-workflow.
- **Ideas:** → R-ORC-01, R-ORC-02, R-ORC-05, R-ORC-06, R-ORC-07.

#### F2. Gas Town + Beads (Steve Yegge)
- **URL:** https://github.com/gastownhall/gastown · https://github.com/gastownhall/beads
- **Class:** self-hosted orchestrator (Go, tmux) + git/Dolt-backed issue graph · MIT
- **Maturity:** Gas Town ≈18.4k★, Beads ≈28.0k★ [V 2026-10-20]; Gas Town released ~2026-01-02, built in ~3 weeks, largely AI-generated; author warns novices off [S]. "Wasteland" federation reported [S].
- **What it is [V]:** roles — *Mayor* (coordinator), *Polecats* (workers: persistent identity, ephemeral sessions), *Refinery* (per-rig Bors-style **bisecting batch merge queue**; polecats never push main), *Witness* (stuck-agent detection & recovery), *Deacon* (cross-rig patrol). *Convoys* bundle beads; *Formulas* (TOML templates) instantiate as *molecules* with checkpointed steps. Beads: `bd ready` (unblocked work), `bd update --claim` (atomic claim), dependency kinds blocks/related/parent-child/supersedes/replies-to, message issues with threads, `bd remember`/`bd prime` memory, compaction ("memory decay"). Presets for ~11 agents.
- **Special:** work state outlives sessions (identity ≠ session); CI-grade landing; runs 20–50 agents.
- **Gaps:** bisecting batch merge (M11.9 is dependency-ordered cross-repo only); an *acting* supervisor ("Witness") — ptyro detects status but doesn't recover; durable external work graph with memory.
- **Weaknesses:** whimsical vocabulary, steep curve; tmux-based, status inferred not hooked; single repo per rig.
- **Ideas:** → R-ORC-09 (watchdog), R-ORC-12 (merge queue), R-ORC-13 (identity ≠ session), R-ORC-15 (tracker adapters).

#### F3. Ruflo (formerly Claude Flow, ruvnet)
- **URL:** https://github.com/ruvnet/ruflo · MIT
- **Maturity:** ≈77.6k★, 9.7k commits [V]; renamed at v3.5 (2026-02-27) [S].
- **What it is:** swarm topologies (hierarchical/mesh/adaptive), "consensus", ~314 MCP tools, 27 hooks, HNSW vector memory, cross-machine federation; the repo itself flags its counts and benchmarks as inconsistent/self-reported.
- **Special:** enormous mindshare; MCP-first packaging; reusable prompt/skill library.
- **Gaps:** persistent cross-session memory; daemon↔daemon federation (M9 is client↔daemon).
- **Weaknesses:** the repo itself flags its counts and benchmarks as inconsistent/self-reported; lives inside one Claude session (no process isolation, no terminal ownership); opaque status.
- **Ideas:** name recipe shapes plainly (fan-out, pipeline, lead/worker, review-loop) — topology vocabulary sells; small `ptyro note` project memory readable via `ptyro mcp` (R-ORC-20). Do **not** chase a vector DB.

#### F4. Overstory → Warren (Jaymin West)
- **URL:** https://github.com/jayminwest/overstory (archived) · https://github.com/jayminwest/warren · MIT
- **Maturity:** Overstory ≈1.3k★, **archived**, moved to Warren (≈391★, TS; local sandbox / Docker / Kubernetes runs) [V/S].
- **What it is [V]:** worktree per worker in tmux; **SQLite-WAL mail with 8 typed protocol messages and group addresses `@all`, `@builders`**; FIFO merge queue with 4-tier conflict resolution; 3-tier watchdog (mechanical liveness → AI triage → monitor agent); 11 runtime adapters; `ov serve` web UI; a candid `STEELMAN.md` saying compounding errors and merge conflicts are the normal case.
- **Special:** the closest design to ptyro M7.7; honest risk documentation.
- **Gaps:** tiered automatic conflict resolution; tiered watchdog; Warren signals the move to **container-per-run**.
- **Weaknesses:** the project has already moved once (Overstory archived for Warren); tmux; single repo.
- **Ideas:** write ptyro's own steelman / failure-mode doc (fits SPEC §15.5.11); conflict ladder for pick/merge (R-ORC-11); optional container-per-worktree (R-ADJ sandbox).

#### F5. ccswarm (nwiizo)
- **URL:** https://github.com/nwiizo/ccswarm · Rust · license unverified
- **Maturity [S]:** ≈137★, last push 2026-03-05, experimental.
- **Special:** Rust + worktrees + role-specialised agents (frontend/backend/devops/QA).
- **Ideas:** ship **role briefs** as presets usable in recipes and change sets (R-ORC-21).

#### F6. Claude Code Agent Farm (Dicklesworthstone)
- **URL:** https://github.com/Dicklesworthstone/claude_code_agent_farm · MIT + "OpenAI/Anthropic rider"
- **Maturity [V]:** ≈922★, 50 commits.
- **What it is:** Python/tmux farm for 20–50 Claude agents; lock files in `/coordination/` (2 h stale) — enforced only by *prompt*; dashboard with context usage, heartbeat age, cycle count; `--auto-restart` with exponential backoff (10 s → 5 min); JSON state; HTML run reports; Ctrl-R broadcasts `/clear`.
- **Gaps:** context-window % per agent; broadcast; restart-with-backoff policy; HTML run reports.
- **Weaknesses:** coordination by prompt only; Claude-only; narrow (lint-fix-at-scale).
- **Ideas:** context % column (R-ORC-10); `restart = { on=["crash","stall"], backoff="10s..5m", max=5 }` (R-ORC-09); `ptyro run report --html`.

#### F7. claude-code-hooks-multi-agent-observability (disler)
- **URL:** https://github.com/disler/claude-code-hooks-multi-agent-observability · license not stated
- **Maturity [V]:** ≈1.6k★, reference implementation.
- **What it is:** hooks → HTTP → Bun → SQLite → WebSocket → Vue dashboard; 12 hook event types incl. SubagentStart/Stop, PreCompact; traces team TaskCreate/TaskUpdate/SendMessage.
- **Special:** proof that hooks are the right telemetry source — validates ptyro §9.
- **Gaps:** cross-agent **swimlane timeline** of tool events (ptyro plans swimlanes only for change sets, M11.11).
- **Ideas:** a run timeline view for every run (R-ORC-14); ingest Claude team/task hook events into the ledger.

#### F8. Conductor — checkpoints (see also B-segment entry)
- **Gaps:** **automatic per-turn checkpoints** in ordinary single-repo sessions [S]. ptyro's checkpoints are change-set-scoped (M11.8).
- **Ideas:** generalise M11.8 down to every session (R-ORC-03).

#### F9. OpenAI Codex — subagents & cloud best-of-N
- **URL:** https://learn.chatgpt.com/docs/agent-configuration/subagents
- **Maturity:** subagents on by default, spawn on request [V]; `codex cloud exec --attempts 1..4` [S]; Agents API public beta 2026-09-10 exposing the Codex harness [S].
- **What it is [V]:** custom agents in `.codex/agents/*.toml` (`name`, `description`, `developer_instructions`, `model`, effort); built-ins default/worker/explorer; `agents.max_concurrent_threads_per_session`; subagents inherit sandbox & approvals; approvals from inactive threads surface with their source.
- **Gaps:** remote/cloud attempts (ptyro fan-out is local only).
- **Weaknesses:** best-of-N within Codex models only — no cross-harness compare.
- **Ideas:** `ptyro fanout --agents codex-cloud` pulling cloud diffs into local worktrees for the compare view (R-ORC-17).

#### F10. Cursor 3 — `/best-of-n` and Agents Window
- **URL:** https://forum.cursor.com/t/cursor-3-worktrees-best-of-n/156507
- **Maturity [S]:** Cursor 3 (2026-04) with `/worktree`, `/best-of-n`; early bug reports; token-heavy.
- **What it is:** same task across models, each in a worktree; **a parent agent critiques the results** so you pick one or *merge parts of several*; agents local / worktree / cloud / SSH.
- **Gaps:** **AI judge** + **hunk-level combine across candidates** — neither in M7.3.
- **Weaknesses:** IDE-locked; compares *models* inside one harness, not harness-vs-harness.
- **Ideas:** `judge` step (R-ORC-04); hunk-level "take from B" (R-ORC-04); market harness-vs-harness compare.

#### F11. Composio Agent Orchestrator (2026 newcomer)
- **URL:** https://github.com/ComposioHQ/agent-orchestrator · MIT [S]
- **Maturity [S]:** ≈7k★; TS rewrite; `@aoagents/ao-cli` v0.3.0. (Not to be confused with AgentWrapper/agent-orchestrator.)
- **What it is:** worktree + branch + PR per agent (tmux); YAML **reactions** — CI failure → back to the agent with logs; review changes requested → agent; escalate to human after 30 min.
- **Special:** closes the loop *after* the PR opens.
- **Gaps:** event-driven routing of CI failures / review comments to the owning session. ptyro's plan ends at landing.
- **Ideas:** recipe triggers / reactions (R-ORC-08).

#### F12. Goose (Block / AAIF) — recipes, subrecipes, subagents
- **URL:** https://github.com/block/goose · Apache-2.0 · ≈50k★ [S]; in ACP registry [V]
- **What it is [S]:** YAML recipes with parameters; `sub_recipes` each in its own session; same subrecipe with different params runs **in parallel by default** (`sequential_when_repeated` to serialise); no nesting; ad-hoc subagents.
- **Gaps:** **matrix fan-out** (1 agent × N inputs; M7.2 is N agents × 1 prompt); shareable recipe links/hub.
- **Ideas:** `matrix =` on steps (R-ORC-05); `ptyro recipe import <url>` from trusted hubs (shares trust code with M11.12).

#### F13. Kilo Code / Roo Code orchestrator ("Boomerang")
- **URL:** https://kilocode.ai/docs/basic-usage/orchestrator-mode
- **Maturity [S]:** **Roo Code shut down** (announced 2026-04-21, archived 2026-05-15). Kilo rebuilt on the OpenCode server (GA 2026-04-02); orchestrator mode superseded by native subagent delegation.
- **Lesson:** mode-specialised delegation with summary-only return got absorbed into generic subagents — don't build a mode system. OpenCode `serve` now powers Kilo too → raises the value of an OpenCode structured driver (M10).

#### F14. Agent Client Protocol registry (see Part II §P3 for protocol detail)
- **URL:** https://agentclientprotocol.com/registry
- **Maturity [V]:** registry live since 2026-01, **44 agents** (Claude Agent, Codex, Gemini, Cursor, Copilot, Devin, goose, Kilo, OpenCode, Junie, Amp, Droid, Qwen, Kimi, Mistral Vibe, pi…); ACP v2 docs; open RFDs: session fork, proxy chains, MCP-over-ACP, **Subagent Sessions** (rev. 2026-10-13, `unstable_subagents`), compaction, usage/cost.
- **Gaps:** one client integration reaches ~44 agents. M10.2 treats ACP as "for Gemini/OpenCode" — it should be the primary structured path.
- **Ideas:** registry-aware `ptyro agents add`; render subagent trees; ptyro as an **ACP proxy** and/or exposing daemon sessions **as** an ACP agent to Zed/JetBrains (R-ORC-16).

#### F15. A2A and MCP as orchestration substrate (see Part II §P3)
- A2A v1.0 (2026 H1, LF, 150+ orgs) [S] — **no coding-agent CLI adoption found**; enterprise cross-vendor only. Low priority.
- MCP **Tasks** extension (`io.modelcontextprotocol/tasks`): poll-based, `working`/`completed`/`failed`/`cancelled`/**`input_required`** [S]. That is exactly ptyro's `blocked`.
- **Ideas:** model `ptyro mcp` spawn/wait as MCP Tasks (R-ORC-07).

#### F16. Spec-driven tools — GitHub Spec Kit, Kiro specs, BMAD Method, Tessl
- **Spec Kit** (https://github.com/github/spec-kit, MIT): 1.0.0 on 2026-08-21 [S]; primitives integrations / extensions / presets / workflows / steps; 38 agent integrations [S].
- **Kiro** (https://kiro.dev, AWS): `.kiro/specs/<name>/requirements.md|design.md|tasks.md` (EARS) [V]; CLI `/spec run` builds a dependency graph from `tasks.md` and runs independent tasks in concurrent **waves** [S]; pre/post task hooks [S].
- **BMAD Method** (MIT, ≈54k★, v6.12 stable, v7 alpha) [V]: role personas, "right-sized process", BMad Loop module.
- **Tessl** (https://tessl.io): registry of versioned, *evaluated* skill/spec "tiles" [S].
- **Gaps:** nothing in ptyro compiles a spec/tasks file into a fan-out; no "approve this file" gate.
- **Ideas:** `ptyro recipe from-tasks tasks.md` → wave DAG; `gate = { approve = "specs/x/design.md" }`; built-in `spec-pipeline` recipe (R-ORC-06).

#### F17. Agent task trackers — Backlog.md, Task Master AI, Vibe Kanban
- **Backlog.md** (MIT, ≈7.1k★) [V]: markdown tasks in-repo with deps, acceptance criteria, per-project Definition of Done; MCP; TUI + web board; `task list --json --watch`.
- **Task Master AI** (MIT + **Commons Clause**, ≈28k★) [V]: PRD → JSON task graph, `next`, complexity analysis; MCP with selectable tool sets.
- **Vibe Kanban** (Apache-2.0, ≈28k★): README says **sunsetting** [V]; BloopAI reportedly closed 2026-04-10 [S]. Best-in-class **inline review comment → agent** loop, browser preview, PR create/merge.
- **Gaps:** no tracker integration; no line-anchored comment-to-agent in compare (planned only in M6 diff inspector); no DoD gate.
- **Ideas:** `--task backlog:<id>|taskmaster:<id>|bead:<id>` (R-ORC-15); DoD gate; "Coming from Vibe Kanban" README section.

#### F18. Others, briefly
- **Aider architect mode** (Apache-2.0; development appears slowed): planner model + cheap editor → `architect-editor` recipe preset.
- **OpenHands V1 SDK** (MIT): task tool + concurrent delegate; markdown subagents; can host ACP agents; sandboxed Docker/K8s runtimes (ptyro has none).
- Watch: Superset, Parallel Code, Sculptor, Claude Code "projects".

---

### G. The agent CLIs themselves, and adjacent tooling

> **Headline (2026-10-20):**
> 1. **Claude Code ships its own supervisor daemon + agent view** (`claude agents`) — the closest
>    direct overlap with ptyro. For Claude-only users much of ptyro's single-vendor value is now
>    native. ptyro wins on multi-vendor, multi-account, a real multiplexer, and reboot survival.
> 2. **Gemini CLI was replaced by Antigravity CLI (`agy`) for unpaid / Google One accounts on
>    2026-06-18** (banner on geminicli.com [V]); still available to enterprise / API-key users [U].
>    This changes adapter priority (R-AGT-01).
> 3. **Documented live usage feeds now exist:** Claude's statusline JSON `rate_limits` and Codex
>    app-server `account/rateLimits/*`. Usage meters should be built on these (R-USE-01).
>
> This segment is mostly *integration surface*: exact hooks, flags and protocols. Facts here should
> graduate into SPEC §8 / §20 once spiked.

#### G1. Claude Code (Anthropic) — primary agent
- **URL:** https://code.claude.com/docs · changelog in `anthropics/claude-code` · **2.1.304** (2026-10-20) [V]
- **Overlap (biggest risk) [V]:** `claude agents` agent view under an auto-started **supervisor**: `claude daemon status|logs|stop --any --keep-workers`; state in `~/.claude/daemon/roster.json` and `~/.claude/jobs/<id>/state.json`; states Working / Needs input / Idle / Completed / Failed / Stopped; `claude --bg`, `attach`, `logs`, `stop`, `respawn`, `rm`, `claude agents --json`, `--cwd`; peek + reply; `/bg`, `/fork`; auto-isolation into `.claude/worktrees/` (disable with `worktree.bgIsolation: "none"`); research preview.
- **Where ptyro wins:** Claude-only; no splits/layouts/shells/dev servers; each `CLAUDE_CONFIG_DIR` is a separate supervisor (no cross-account view — ptyro profiles fix that); deletes worktrees with sessions; doesn't survive shutdown.
- **Integration surface [V]:**
  - **33 hook events:** `SessionStart` (matchers `startup|resume|clear|compact|fork`), `Setup`, `UserPromptSubmit`, `UserPromptExpansion`, `PreToolUse`, `PermissionRequest`, `PermissionDenied`, `PostToolUse`, `PostToolUseFailure`, `PostToolBatch`, `Notification`, `MessageDisplay`, `Stop`, `StopFailure`, `SubagentStart`, `SubagentStop`, `TaskCreated`, `TaskCompleted`, `TeammateIdle`, `InstructionsLoaded`, `ConfigChange`, `CwdChanged`, `DirectoryAdded`, `FileChanged`, `WorktreeCreate`, `WorktreeRemove`, `PreCompact`, `PostCompact`, `PreModelSwitch`, `PostModelSwitch`, `Elicitation`, `ElicitationResult`, `SessionEnd`.
  - **Notification matchers:** `permission_prompt`, `idle_prompt`, `auth_success`, `elicitation_*`, `agent_needs_input`, `agent_completed`, `quota_auto_resume_fired|stale|disabled`.
  - **Hook types:** `command`, **`http`** (POST, same JSON reply), `mcp_tool`, `prompt`, `agent`; `onFailure: "block"` (2.1.295).
  - **Decision control:** `decision:"block"` + `reason` (Stop, PostToolUse, UserPromptSubmit, PreCompact…); `continue:false` + `stopReason`; `hookSpecificOutput.additionalContext`; exit 2 blocks with stderr as reason.
  - **`WorktreeCreate`** replaces git's behaviour: a command hook prints the path; an HTTP hook returns `hookSpecificOutput.worktreePath`. Fires for `--worktree`, `isolation:"worktree"` and background sessions.
  - **OSC 7501** since 2.1.295; OSC 9;4 progress.
  - **Statusline stdin JSON:** `rate_limits.five_hour` / `.seven_day` → `used_percentage`, `resets_at` (epoch s); `rate_limits.spend_limit.{used_usd,limit_usd,period}` (gateway only); `cost.total_cost_usd`; `context_window.used_percentage`, `context_window_size`; `session_id`, `transcript_path`, `worktree.{name,path,branch,original_cwd}`. `rate_limits` only for Pro/Max after the first API response.
  - **Headless:** `-p --output-format json --json-schema '<schema>'` → `structured_output` (shape-validated, not truth-validated; occasional `null` with `subtype: success` [S]); `--input-format stream-json`.
  - **Cross-session messaging:** per-session Unix socket `CLAUDE_CODE_MESSAGING_SOCKET` + `CLAUDE_CODE_MESSAGING_TOKEN`; optional `{"type":"auth","token":…}` first line, line-oriented plain text, ~1M char cap; **full schema undocumented → spike first**.
  - **OTel:** `CLAUDE_CODE_ENABLE_TELEMETRY=1`, `OTEL_*` exporters; metrics `claude_code.session.count`, `.cost.usage`, `.token.usage`, `.lines_of_code.count`, `.commit.count`, `.pull_request.count`, `.active_time.total`…; events `claude_code.user_prompt`, `.api_request`, `.tool_result`, `.tool_decision`…; GenAI-semconv span attributes in 2.1.300.
- **Ideas:**
  - **Statusline relay as usage tap:** set `statusLine.command` to `ptyro statusline-relay`, forward JSON over the hook socket and chain to the user's existing statusline command (R-USE-01).
  - **`WorktreeCreate` hook returning a ptyro-managed worktree**, so Claude's own `--worktree` and background dispatches land in ptyro's model (and jj support comes free) (R-WT-01).
  - Import `claude agents --json` as foreign rows (R-AGT-02).
  - Map `quota_auto_resume_*` + `StopFailure` to a **"rate-limited, resumes at T"** status (R-STA-05).
  - Use **`type:"http"` hooks** to a daemon localhost endpoint to remove `ptyro hook` process spawns on hot events like PostToolUse (R-AGT-07).

#### G2. Codex CLI (OpenAI)
- **URL:** https://github.com/openai/codex · Apache-2.0 · **0.163.0** (2026-10-15), 0.164.0-alpha.7 (2026-10-20) [V] · docs moved to learn.chatgpt.com
- **Overlap:** 0.163 `/daemon` ("restarts a managed background server") [V, sparse]; desktop app worktrees [U]; **CLI still lacks native `--worktree`** [U] — room for ptyro.
- **Integration surface:**
  - **Hooks [V]:** `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `UserPromptSubmit`, `SubagentStop`, `Stop`, `Interrupt`, `SessionStart`, `SubagentStart`, `SessionEnd`; config in `~/.codex/hooks.json`, `<repo>/.codex/hooks.json`, inline `[hooks]`, plugin `hooks/hooks.json`, managed `requirements.toml`; on by default. **Trust is hash-pinned** — untrusted or changed hooks are skipped; `--dangerously-bypass-hook-trust` per invocation. Fields `session_id`, `transcript_path` (nullable, "not a stable interface"), `cwd`, `hook_event_name`, `model`, `turn_id`, `permission_mode`.
  - **`notify`** [U]: argv JSON on `agent-turn-complete`; `[tui] notifications`; `notification_method = auto|osc9|bel`.
  - **app-server [V]:** `--listen stdio://|ws://IP:PORT|unix://PATH`; JSON-RPC 2.0 without the header; `initialize`/`initialized`; `thread/start|resume|fork` (`lastTurnId`, `ephemeral`); `turn/start|interrupt|steer`; `item/permissions/requestApproval`, `tool/requestUserInput`, `serverRequest/resolved`; `thread/status/changed`, `thread/tokenUsage/updated`; **`account/rateLimits/read`**, **`account/rateLimits/updated`**, `account/usage/read`; `optOutNotificationMethods`; `codex app-server generate-ts|generate-json-schema`. Labelled "experimental and unsupported".
- **Ideas:** a side app-server connection per profile (`CODEX_HOME`) subscribed to `account/rateLimits/updated` for true countdowns (R-USE-01); handle **hook trust** up front (generate into a managed layer or detect "untrusted hook skipped" in `doctor`) (R-AGT-06); `thread/fork` backs "fork into new worktree" in M10 (R-AGT-04).

#### G3. Gemini CLI (Google) — retired for consumers
- **URL:** https://geminicli.com/docs · Apache-2.0 · banner [V]: replaced by Antigravity CLI on 2026-06-18 for unpaid/Google One users; "not optimized for Gemini 4 Argon".
- **Hooks [V]:** `BeforeTool`, `AfterTool`, `BeforeAgent`, `AfterAgent`, `BeforeModel`, `BeforeToolSelection`, `AfterModel`, `SessionStart`, `SessionEnd`, `Notification` (`notification_type: "ToolPermission"`, observe only), `PreCompress`; `type: "command"` only; exit 2 blocks (AfterAgent exit 2 = retry).
- **Idea:** keep the M4.8 adapter thin; `doctor` warns consumer accounts it is retired; retarget effort to Antigravity (R-AGT-01).

#### G4. Antigravity CLI (`agy`, Google) — Gemini CLI's successor
- **URL:** https://antigravity.google/docs/getting-started?tab=cli · hooks `/docs/hooks` · proprietary · GA ~I/O 2026-05 [V docs]
- **What it is:** Go binary sharing a harness with the Antigravity 2.0 app [U]; Ctrl+R review, Y approve; `/goal`, `/schedule`, `/browser`, `/hooks`; Python SDK `google-antigravity` [V].
- **Hooks [V]:** `SessionStart`, `PreToolUse`, `PostToolUse`, `PreInvocation`, `PostInvocation`, `Stop`; config `.agents/hooks.json`, `~/.gemini/config/hooks.json`, `~/.gemini/antigravity-cli/settings.json`, plugin `hooks.json`; camelCase stdin `conversationId`, `workspacePaths`, `transcriptPath`, `artifactDirectoryPath`, `modelName`; Stop adds `terminationReason`, `fullyIdle`, `error`; outputs `decision` (PreToolUse), `terminationBehavior` (PostInvocation). Migration: `agy plugin import gemini`.
- **Missing:** **no Notification/permission hook** — "blocked" must come from OSC 7501 (unknown) or heuristics.
- **Idea:** move this adapter ahead of Aider/Goose/Cursor/Copilot; map `Stop.fullyIdle=true` → idle, `PreInvocation` → working; resume on `conversationId`; M0-style spike for OSC 7501 and blocked detection (R-AGT-01).

#### G5. OpenCode (anomalyco, formerly sst)
- **URL:** https://opencode.ai/docs · MIT · ≈218k★, v1.19.3 [V]
- **Integration surface [V]:** `opencode serve` (`--port` 4096, `--hostname`, `--mdns`, `--cors`); `GET/POST /session`, `/session/:id`, `POST /session/:id/message`, `POST /session/:id/prompt_async` (204), `/abort`, `GET /event` SSE (first `server.connected`), `GET /global/event`; OpenAPI at `/doc`; `OPENCODE_SERVER_PASSWORD/USERNAME`. Plugin events `session.created|compacted|deleted|diff|error|idle|status|updated`, `permission.asked|replied`, `tool.execute.before|after`, `tui.toast.show`, `tui.prompt.append`, `file.edited`, `command.executed`. `opencode acp` [U]; opentui emits OSC 7501 [U].
- **Idea:** M4.9 already matches; add `session.diff` → live diff badge and `/global/event` for one SSE stream per server. **Kilo now runs on the OpenCode server** — the driver gains value.

#### G6. Kilo CLI (Kilo Code) — OpenCode fork [V]
- `kilo run --auto` (exit 0 / 124 timeout / 1), `kilo serve`, `kilo acp`, `kilo session|export|import|stats`; modes Architect/Ask/Debug/Orchestrator; **`/resume-claude` and `/resume-codex` import Claude/Codex transcripts** [V].
- **Idea:** parameterise the OpenCode adapter by binary + config dir to cover Kilo; consider cross-agent conversation handoff (export transcript, seed another agent) (R-AGT-08).

#### G7. Crush (Charm)
- **URL:** https://github.com/charmbracelet/crush · FSL-1.1-MIT [U] · v0.98.1 (2026-10-09) [V] — v0.98.0 added **OSC 7501**; v0.95 Plan Mode; headless `crush run`.
- **Idea:** the generic adapter should promote *any* OSC 7501 emitter to confident status with no bespoke adapter (R-STA-01).

#### G8. GitHub Copilot CLI
- **URL:** https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-hooks-reference · ACP public preview since 2026-01-28 [V]
- **Hooks [V]:** `sessionStart`, `sessionEnd`, `userPromptSubmitted`, `userPromptTransformed`, `preToolUse`, `postToolUse`, `postToolUseFailure`, `permissionRequest`, `agentStop`, `subagentStart`, `subagentStop`, `errorOccurred`, `preCompact`, `notification` (camelCase + PascalCase aliases); fields `sessionId`, `timestamp` (ms), `cwd`, `transcriptPath`, `source`, `agentId`, `parentAgentId`; locations `~/.copilot/hooks/*.json` (`$COPILOT_HOME`), `.github/hooks/*.json`, `.github/copilot/settings.json` — and **it also reads `.claude/settings.json` / `.claude/settings.local.json` hooks**.
- **Gotcha:** ptyro must keep Claude hooks out of repo `.claude/settings.json` (it already does — `data/integrations/`).
- **Idea:** cheap adapter: `COPILOT_HOME` per profile, inject `hooks/*.json`, map `permissionRequest`/`notification` → blocked, `agentStop` → idle (R-AGT-01).

#### G9. Cursor CLI (`agent`)
- **URL:** https://cursor.com/docs/cli/overview [V]: `agent -p`, `--model`, `--mode` Agent/Plan/Ask, `--plan`, `--sandbox`, `--continue`, `--resume="<chat-id>"`, `agent ls`, `agent resume`; prefixing a prompt with `&` hands it to a cloud agent.
- **Hooks [U, forum]:** CLI fires `beforeShellExecution`, `afterShellExecution`, `afterFileEdit`, `postToolUse`, `stop`, `sessionStart`; not `afterAgentResponse`/`afterAgentThought`; `--output-format stream-json` result event has token usage.
- **Idea:** `stop` → idle, `beforeShellExecution` → working; heuristic blocked; record `&` cloud handoff as an event.

#### G10. Droid (Factory)
- **URL:** https://docs.factory.com/reference/hooks-reference [V]: **Claude-compatible hooks** (`PreToolUse`, `PostToolUse`, `UserPromptSubmit`, `Notification`, `Stop`, `SubagentStop`, `PreCompact`, `SessionStart`, `SessionEnd`), same field names, `~/.factory/hooks.json`, `.factory/hooks.json`, accepts `${CLAUDE_PLUGIN_ROOT}`; `droid exec --output-format text|json|stream-json|stream-jsonrpc`.
- **Idea:** near-zero-cost adapter reusing the Claude payload parser (R-AGT-01).

#### G11. Amp · G12. Kimi Code CLI · G13. Qwen Code · G14. Goose · G15. Aider · G16. Warp Agent CLI (brief, mostly [U])
- **Amp:** `amp --execute --stream-json`; thread continuation; server-side threads; no confirmed status hooks → generic adapter + thread-id capture.
- **Kimi Code CLI:** `--wire` protocol; `kimi acp` → cover via the M10 ACP client.
- **Qwen Code:** Gemini CLI fork with Gemini-style hooks (+ HTTP/function/async types); free OAuth tier ended 2026-04-15 → parameterised Gemini adapter.
- **Goose** (Apache-2.0, AAIF): ACP server; hooks with PreToolUse denial (v1.36); `goose://resume` deep links; **recipes**; can itself use Claude Code / Codex as ACP providers. Session templates could borrow the recipe concept.
- **Aider:** maintenance mode (v0.86.x, single maintainer); no hooks → **downgrade to generic + prompt-regex profile**.
- **Warp Agent CLI** (`warp`, runs in any terminal [V]): persistent conversations, cloud handoff, multi-agent orchestration, credit tracking → generic adapter.

#### G17. ACP (Agent Client Protocol) — cross-cutting
- Registry launched 2026-01-28; ~25–50 agents (one segment counted 44); Zed 1.0 (2026-04-29) headline feature; Microsoft "Intelligent Terminal" 0.1 (2026-06-02, a Windows Terminal fork with an ACP agent pane). Claude adapter package name [U]: `@agentclientprotocol/claude-agent-acp` (cited in OpenHands docs) vs older `claude-code-acp`. → R-ACP-01.

#### Adjacent tooling

| Tool | URL | What it is | Integration / idea |
|---|---|---|---|
| **ccusage** | github.com/ryoppippi/ccusage (MIT, very active) | `daily\|weekly\|monthly\|session\|blocks\|statusline`, `--json`, `--offline`; parses local logs of ~18 agents (Claude, Codex, OpenCode, Amp, Droid, Goose, Kilo, Kimi, Qwen, Copilot, Gemini, Antigravity…) | **Don't reimplement log parsing:** shell out to `ccusage … --json` per profile config dir for history; live numbers from statusline / app-server (R-USE-01). |
| **Claude-Code-Usage-Monitor** | github.com/Maciek-roboblog/Claude-Code-Usage-Monitor (MIT, v4.0.0) | Live TUI; custom plan derives a **P90 limit** from the last 192 h; burn rate; predicted expiry; `--data-paths` per Claude dir; `--statusline` reads official `rate_limits` | Steal P90 soft limits and "at this burn you hit the 5 h cap at 14:20"; per-profile windows. |
| **ccstatusline** | github.com/sirmalloc/ccstatusline (MIT, v2.2.32) | Widget vocabulary: block/reset timers, cost/hour, context bars, weekly per-model usage, powerline | Borrow vocabulary for `ptyro status-line`; the relay must **chain** to it, not replace it. |
| **Claude Code sandbox / `@anthropic-ai/sandbox-runtime` (`srt`)** | code.claude.com/docs/en/sandboxing | macOS Seatbelt; Linux/WSL2 **bubblewrap + socat** (+ seccomp); Bash-only scope; network proxy with empty allowlist; AppArmor profile needed on Ubuntu 24.04+ | Wrap the **whole agent PTY** in `srt`/bwrap — a light sandbox tier for any agent; `doctor` checks bwrap/socat/AppArmor (R-SEC-05). |
| **Docker Sandboxes (`sbx`)** | docs.docker.com/ai/sandboxes | microVM per agent (Claude, Codex, Copilot, Cursor, Devin, Droid, Gemini, Kiro, OpenCode, Shell); `docker sandbox` deprecated for `sbx`; Linux support unconfirmed | `sandbox = "sbx"` backend: spawn `sbx run <agent>` in the PTY, OSC 7501 passes through (R-SEC-05). |
| **container-use (Dagger)** | github.com/dagger/container-use (Apache-2.0, experimental) | MCP server: container + git branch per environment; `container-use stdio` | Light "MCP-isolated" workspace option; weaker than PTY-in-container. |
| **asciinema 3** | blog.asciinema.org/post/three-point-o (2025-09-15) | Rust rewrite; **asciicast v3** (relative intervals, `term` group, `exit` event, `#` comments); `asciinema stream -s/-r`; headless rec | `ptyro export --asciicast` should target **v3** with status changes as markers; `ptyro stream` speaking asciinema's live protocol = free view-only sharing (R-OBS-03). |
| **Claude Code OTel** | code.claude.com/docs/en/monitoring-usage | see G1 | **Local OTLP/HTTP receiver in the daemon**; inject `OTEL_*` with `OTEL_RESOURCE_ATTRIBUTES=ptyro.session=<id>,ptyro.profile=<p>`; chain to the user's endpoint (R-OBS-01). |
| **CCS** | github.com/kaitranntt/ccs (MIT, ≈2.9k★) | Multi-account + multi-provider manager for Claude Code/Droid/Codex; CLIProxy routing to GLM, OpenRouter, Ollama, llama.cpp | Overlaps ptyro profiles; `profile adopt` could import CCS; **"provider via proxy" profiles** (`ANTHROPIC_BASE_URL`) as a first-class profile field (R-PRF-04). |

**Watch-outs:** Codex hook trust is hash-pinned · Copilot reads `.claude/settings.json` hooks · each `CLAUDE_CONFIG_DIR` runs its own supervisor · Codex app-server is experimental.

---

## Deep dives — herdr and tuios

These are the two products closest to ptyro's core: daemons that own their PTYs and know which agent is doing what. The owner asked for a full picture of both. Researched 2026-10-20 from a shallow clone of each repo (read only), the GitHub API, and the projects' docs and posts.

**What the deep dives change in the picture:**

1. **tuios is much further along than its A3 entry suggested.** Since v0.8.0 (2026-09-27) it has shipped a large share of ptyro's M4–M10 plans:
   - an actionable inbox with held approvals and risk rules;
   - review notes, turn checkpoints and `ship pr`;
   - `fan` / `compare` / `verify` / `keep`;
   - an MCP server, ACP and Codex app-server workers;
   - multi-host federation, Tailscale, phone pairing and a web client.

   It runs at about 80 commits a day, mostly from one author, with agent help.
2. **herdr is the best-funded and the most adopted.** It has a $6M seed, YC F26, 45k★ and about 1.6M downloads. Its core is small on purpose: no profiles, setup scripts, ports, review, orchestration, MCP, ACP or OSC 7501. Community plugins are filling those gaps (herdr-projects, herdr-reviewr, collie). Agent vendors already report status to herdr's `report-agent` API.
3. **ptyro's moats, re-stated.** Neither product has any of these:
   - per-workspace **identity profiles**;
   - a **project/worktree lifecycle** with setup scripts, a trust fingerprint, `.env` copy and port blocks;
   - an **exact live screen** after reattach plus durable SQLite history;
   - a **declarative, persisted orchestration engine** (recipes, gates, verified-done);
   - **multi-repo change sets**.

   Every other part of ptyro's roadmap now has a shipping competitor, so **speed to M6–M7 matters**.
4. **Interop is cheap and worth it.** tuios already emulates herdr's socket, so agents built for herdr report into tuios without changes. ptyro can do the same and accept every OSC 7501 emitter. The new ideas are filed in §III.2 "From the herdr and tuios deep dives".

### DD1. herdr — deep dive (herdrdev/herdr, herdr.dev)

**Snapshot:** 2026-10-20, repo `master` as of 2026-10-16. Sources: a shallow clone of the repo
(read only, nothing built or run), the GitHub REST API and herdr.dev pages. Tags: **[V]** primary
source, **[S]** secondary, **[U]** uncertain or inferred.

**Headline.** herdr is ptyro's closest and strongest competitor.

- **What it is.** A Rust daemon plus clients that owns the PTYs and emulates terminals with
  **libghostty-vt**, a Zig library reached through a bindgen FFI. It detects 22+ agent CLIs using
  **TOML screen manifests that are updated remotely**. Agents drive it through a JSON socket API and a CLI.
- **Growth.**
  - Went from v0.1.0 to v0.9.5 between 2026-03-27 and 2026-10-14.
  - Has **44,940★** and about **1.6M release downloads**.
  - Raised a **$6M seed (Bessemer, YC F26)**.
  - Has a plugin ecosystem of about **1,867 tagged repos**.
- **What it leaves out.** It keeps its core small on purpose. It has none of the following: identity
  profiles, setup scripts, `.env` copy, port blocks, a built-in review→PR flow, fan-out, compare,
  recipes, MCP, ACP, an `error` state, OSC 7501 or jj.
- **The threat.** Community plugins are filling many of those gaps: herdr-reviewr, herdr-projects,
  collie and herdr-plus.

#### DD1.1 Identity

| Item | Finding | Tag |
|---|---|---|
| Author | Founder-led: one main author. | [V] |
| Org | Created 2026-03-27 as `ogulcancelik/herdr`, moved to the **`herdrdev`** org around v0.8.0 (2026-08-03). Site footer reads "© 2026 Herdr, Inc." | [V] |
| **License history (resolved)** | **AGPL-3.0** at launch, then **AGPL-3.0-or-later plus a commercial dual license** from 2026-05-26 (commit `cfffe659`), then **Apache-2.0** from 2026-07-22 (commit `cd5ea1be`, first shipped in v0.8.0). The relicense followed issue #1340: the author collected consent from every contributor and rewrote code from anyone who couldn't be reached. Releases before the switch stay AGPL. **It is Apache-2.0 today.** | [V] |
| Funding | "Herdr is joining Y Combinator. The runtime stays open." (2026-08-06, **YC F26**). "Herdr raised a $6M seed" (2026-09-08): **Bessemer** led, with YC, e2vc and angels Tobi Lütke, Dane Knecht and Görkem Yurtseven. Hiring ran "through the terminal, over SSH". | [V] |
| Herdr Cloud | `/cloud/` is a waitlist: "Your machines. One Herdr. No SSH setup." The blog post "Connecting the machines" (2026-09-07) describes an E2E-encrypted middleman that "connects them; it doesn't host the agents". There's no price or date. The code already reserves cloud shell endpoints. On HN, people objected to needing an account, feared enshittification, saw overlap with Tailscale, and asked for self-hosting. | [V] / [S] |
| Traction | **44,940★, 3,580 forks**, 397 open issues, **91 contributors**, 2,612 issues and 1,124 PRs in total. Stars were ~13.6k on 2026-07-08 [S], #1 on GitHub Trending around 2026-06-30 [S], and 25k (with 340k downloads) on 2026-08-06 [V]. | [V] |
| Cadence | 1,865 commits in about 7 months. Per month: Mar 67, Apr 249, May 356, Jun 302, Jul 302, Aug 215, Sep 299, Oct 75 (to the 16th). | [V] |
| Releases | 98 releases (61 stable plus previews), **1,609,008 asset downloads** (API sum). See the milestone list below the table. | [V] |
| Platforms and install | Linux, macOS, **Windows GA since 0.8.2** (ConPTY via a patched portable-pty, named pipes), x86_64 and aarch64. Installs via `curl …/install.sh \| sh`, brew, mise, a Nix flake, PowerShell and `install.cmd`. `herdr update` self-updates with SHA-256 checks on `stable` or `preview` channels. **`herdr --remote host` installs herdr on the remote machine.** | [V] |
| Bus factor and process | Low but rising: one main author wrote most commits, with a second regular contributor active recently. **Unsolicited PRs are closed automatically**: only people listed in `.github/APPROVED_CONTRIBUTORS` may open PRs, and features go to Discussions. | [V] |

Release milestones:
- 0.6 (05-20): persistence and handoff.
- 0.8.0 (08-03): Apache-2.0 and the agent skill.
- 0.8.2 (08-19): Windows GA.
- **0.9.0 (09-07): multi-machine, multi-client and a client-side UI.**
- 0.9.1: `--machine` CLI forwarding.
- 0.9.2: resume commands reported by the agent.
- **0.9.5 (10-14).**

#### DD1.2 Architecture [V]

- **Process model.** A background **server** owns the panes; N **clients** connect to it.
  - Since **0.9.0 the TUI runs in each client**. The server streams pane *surfaces*, and the client draws the sidebar, menus, themes and copy mode itself.
  - Clients can view **different workspaces and tabs independently**. When two share a tab, the last one to interact sets its size.
  - With no client attached, panes get a virtual 120×40 size.
- **Two protocols.**
  1. **The public JSON API.**
     - NDJSON requests `{"id","method","params"}` get back `{"id","result"}` or `{"id","error":{"code","message"}}`.
     - Transport: a Unix socket (`~/.config/herdr/herdr.sock`, or `…/sessions/<name>/herdr.sock` for named sessions) or a named pipe on Windows.
     - About 128 methods and events, with a published **JSON Schema** (`herdr api schema --json`, about 120 KB). Clients are told to ignore unknown fields.
  2. **A private binary protocol** (bincode 2, length-prefixed frames, `PROTOCOL_VERSION = 22`), used for same-install attach and handoff.
     - A stable **"endpoint generation 1"** contract negotiates codecs (`shell.snapshot.v1`, `shell.surface.v1`, `shell.input.semantic.v1`, `shell.blob.v1`) and capabilities.
     - As a result, **clients and servers of different versions work together**. A missing method only disables that one action.
- **Emulation.**
  - `crates/ghostty-vt` is herdr's own binding to a **vendored libghostty-vt** (Ghostty commit `44f2a44d`, "1.3.2-HEAD"). `build.rs` builds it with **Zig** (ReleaseFast with SIMD) and generates the Rust bindings with bindgen 0.72.1.
  - Local patches each document when they can be removed (for example, `0002 expose modifyOtherKeys mode`).
  - **Kitty graphics** render natively (on by default since 0.9.0).
  - PTYs come from a vendored, patched `portable-pty` 0.9.0.
- **Persistence.**
  - **On detach:** processes stay alive.
  - **On server restart:** layout, cwd and focus come back from `session.json`. Agents resume through native session IDs (18 agents, via integrations) or an **argv the agent reported**, started one at a time 100 ms apart.
  - **Screen history replay** is off by default (`[experimental] pane_history`).
  - **Live handoff** (`herdr update --handoff`) passes PTY file descriptors to the new server. It is experimental, and batched beyond 64 panes.
  - **Crash safety:**
    - atomic writes;
    - 48 rotating snapshots, at most one every 15 minutes;
    - 3 recovery copies kept when a load fails;
    - on Linux, a **systemd-logind delay inhibitor** (via zbus) delays shutdown so state can be saved;
    - on macOS and Windows, state is saved at shutdown;
    - since 0.9.4 the server ignores SIGHUP.
- **Rendering.** ratatui 0.30 and crossterm 0.29. Remote links send row-level surface deltas and ask SSH for compression. Synchronized output prevents tearing.
- **Size and dependencies.**
  - About **290k lines of Rust**: client 67k, app 38k, server 30k, platform 17k, pane 13k, integration 12k, cli 11k, api 9.5k, terminal 9k, protocol 9k, config 8k, remote 7.5k, detect 5.6k, persist 5.2k.
  - Main dependencies: tokio, interprocess, bincode, serde_json, jsonc-parser, toml, schemars, regex, clap and zbus.
- **Performance.**
  - The blog post "Ten agents, three clients, **95% less CPU**" (0.8.0) explains the gain: sidebar spinners became static glyphs, hidden panes aren't drawn, and mouse movement no longer triggers repaints.
  - Complaints, all open issues unless noted:
    - **#1134 "60Hz limit"** (32 reactions, the most of any issue);
    - #3217, lag and fan noise with many agents on a Mac;
    - #5194, idle CPU grows when two clients subscribe to events;
    - #4555, a redraw loop when nested in tmux under WSL2;
    - occasional input lag, and WezTerm selections being cleared, reported on HN [S].

#### DD1.3 Agent model [V]

- **Detected agents (22 bundled manifests).**
  - **Supported by herdr with integrations:** Claude Code, Codex, Copilot CLI, Cursor Agent, OpenCode, Pi, OMP, Droid, Devin, Kimi, Kilo, Hermes, Qoder, Qwen, Letta, MastraCode, Grok and Antigravity (`agy`).
  - **Detected from the screen only:** Amp, Kiro, Maki, Gemini CLI, Cline. Muse is also detected.
  - **"Supported by the agent"** (the agent self-reports): Command Code, **Crush**, Kiro CLI, Muse Code and Prime Agent.
  - Wrappers can name the agent with `HERDR_AGENT=claude fence -- claude`.
  - Linux runtimes without a foreground process group can opt in with `HERDR_PROCESS_DETECTION=child-groups`.
- **Detection pipeline.** herdr identifies the agent from the pane's foreground process. It then applies that agent's manifest to the **live bottom of the screen** (not the scrolled view), plus the OSC title and OSC 9;4 progress.
- **Manifest format.**
  - One TOML file per agent in `src/detect/manifests/*.toml`, 1,798 lines in total (`claude.toml` alone is 242).
  - Header fields: `id`, `version = "2026.10.11.1"`, `min_engine_version = 2`, `updated_at`, `aliases`.
  - Each `[[rules]]` entry has:
    - `id` and `state` (working / blocked / idle / unknown);
    - `priority`;
    - `region`: `osc_title`, `bottom_non_empty_lines(12)`, `after_last_horizontal_rule`, `last_non_empty_above_prompt_box` or `whole_recent`;
    - matchers: `regex`, `line_regex`, `contains`, `any`, `not`, which can be nested;
    - flags: `visible_working`, `visible_blocker`, `skip_state_update`.

  Example rule:
  ```toml
  [[rules]]
  id = "live_blocked_form"
  state = "blocked"
  priority = 980
  region = "after_last_horizontal_rule"
  visible_blocker = true
  contains = ["esc to cancel"]
  any = [ { contains = ["enter to confirm"] }, { contains = ["enter to select"], any = [ { contains = ["↑↓ to navigate"] } ] } ]
  ```
- **Remote manifest updates.**
  - herdr fetches `https://herdr.dev/agent-detection/index.toml`, caches the result and **hot-reloads without a restart**. It uses whichever compatible copy is newer, the cached one or the bundled one.
  - Remote updates can only patch agents herdr already knows; a new agent needs a new binary.
  - Opt out with `[update] manifest_check = false`.
  - **Local overrides** in `~/.config/herdr/agent-detection/<agent>.toml` always win. An invalid override logs a warning and falls back.
  - Commands: `herdr server agent-manifests | update-agent-manifests | reload-agent-manifests`.
- **Integrations (`herdr integration install <x>`).**
  - Most integrations only report the **session identity, so the session can be resumed**. Example: the Claude hook script fires only on SessionStart, for the sources startup, resume, clear, compact and fork.
  - **Only Pi, OMP, Kimi, OpenCode, Kilo and MastraCode report lifecycle state.**
  - **Claude Code's state comes from screen manifests, not hooks.**
  - Codex turn hooks were removed in 0.9.5 after a problem with clamping the Interrupt hook timeout.
  - Integrations are versioned (resume needs Claude v6+ and Codex v5+). `herdr integration status [--outdated-only]` checks them.
- **Self-report API.**
  - The main call:
    ```
    pane report-agent <pane> --source ID --agent LABEL --state idle|working|blocked|unknown
        [--message] [--seq N] [--agent-session-id] [--agent-session-path] [-- RESUME_ARGV…]
    ```
  - Related calls:
    - `pane report-agent-session` reports identity only.
    - `pane release-agent` is sent on exit.
    - `pane report-metadata` is display-only: `--title`, `--display-agent`, `--state-label STATUS=TEXT`, `--token k=v`, `--ttl-ms`, `--applies-to-source`.
    - `workspace report-metadata` does the same per workspace.
  - Rules:
    - `seq` must strictly increase per source. The `herdr:` source prefix is reserved.
    - Resume argv: the first word must be a plain command on PATH, with no apostrophes, at most 64 arguments and 8 KiB in total. Violations return `invalid_resume_argv`. A resume reported before the source holds the pane returns `resume_not_accepted`.
    - Agents find herdr through `HERDR_ENV=1`, `HERDR_PANE_ID`, `HERDR_BIN_PATH` and `HERDR_SOCKET_PATH`.
    - Safety net: if an agent exits without releasing, the pane is cleared once it is back at an idle shell, usually within 1–2 s.
- **States.**
  - The states are `working`, `blocked`, `done` (idle but not yet seen), `idle` (seen) and `unknown`. **There is no `error` state.**
  - **Seen-state:** the TUI tracks it per client. The CLI and API use the server's copy, which only `pane focus` and `agent focus` clear; reading a pane doesn't.
  - **`completion_seq`** equals the `state_change_seq` of a real completed turn. Startup, restore and session switches don't count as turns.
  - **Roll-ups:** blocked propagates to pane, tab and workspace; working marks the workspace "active"; done stays until someone looks.
  - **"Blocked" is deliberately strict.** It's set only when a known approval or question UI is on screen. Otherwise a known agent falls back to idle, labelled **`default_known_agent_idle_fallback`**. The docs warn: "idle status is not authorization to approve an action."
  - Antigravity and Grok show idle while background subagents are still running.
  - Ctrl+Z followed by `fg` keeps the agent's identity.
- **`herdr agent explain <target> [--json|--verbose]`.** Also works offline with `--file screen.txt --agent codex`. The running server checks against its active manifest cache and prints:
  - the final state;
  - `screen_detection_skip_reason`, when a hook holds full authority over the lifecycle;
  - the manifest source and version, the cached remote version, any local override that shadows it, and the remote update status;
  - the matched rule, visible-evidence flags, and each rule's matcher and region evidence;
  - why a transcript viewer was skipped;
  - the reason for an idle fallback.

#### DD1.4 CLI and socket API [V]

- **Command groups:**
  - launch and status: `herdr`, `--remote`, `--session`, `--machine`, `--skill`, `update [--handoff]`, `status`, `api schema|snapshot`, `config check`;
  - management: `server`, `session`, `machine`, `notification show`;
  - layout: `workspace`, `worktree`, `tab`, `pane`;
  - agents and terminals: `agent`, `terminal`;
  - extensions: `integration`, `plugin`.
- **`pane` commands:**
  - Layout and navigation: list, current, get, layout, process-info, neighbor, edges, focus, resize, zoom, rename, input, split, swap, move, close.
  - `read --source visible|recent|recent-unwrapped|detection [--lines] [--ansi]`.
  - Input: `send-text`, `send-keys` (key-combo syntax, `C-c` alias), and `run` (bracketed paste plus Enter, atomic).
  - `wait-output (--match|--regex) [--source] [--lines] [--timeout]`.
- **`agent` commands:**
  - `agent start <name> --kind K --pane ID [--timeout] [-- args]` needs an existing shell pane at a prompt and never creates layout. It returns once the agent is detected and **ready**, or `agent_not_ready` if the agent shows blocked during startup.
  - `agent prompt <t> <text> [--wait] [--until S]… [--timeout]` refuses a blocked agent with **`agent_blocked`** and sends nothing. If the agent wasn't already working, it must see working or blocked **within 5 s**, or it returns **`agent_prompt_stalled`**. The docs warn that a timeout or stall "does not prove no input was sent".
  - `agent wait` waits for idle, done or blocked by default.
  - `agent read` reads the transcript of an idle full-screen agent by **auto-scrolling its alternate screen**; for an agent that isn't idle it returns `agent_not_idle`.
  - Also: `send-keys`, `rename`, `focus`, `attach [--takeover]`, `explain`, `get`, `list`.
- **`terminal` commands:**
  - `terminal attach <id>`.
  - `terminal session control <target>`: NDJSON over stdin and stdout. Input messages are `terminal.input|resize|scroll|mouse|release`; output messages are `terminal.frame` (base64 ANSI) and `terminal.closed`.
  - `terminal session observe`: read-only, with any number of observers.
- **Socket-only methods:**
  - `layout.export` / `layout.apply`: a declarative BSP tree with label, cwd, argv and env.
  - `session.snapshot`.
  - `events.subscribe` / `events.wait`.
  - `agent.view.set|clear`: a declarative filter and sort that reshapes the Agents sidebar. Ops: `all|any|not|eq|in|exists`. Fields include `status`, `seen`, `state_change_seq` and `{"token":…}`.
- **Events:**
  - `workspace.*` and `tab.*`;
  - `pane.created|updated|closed|focused|moved|exited|agent_detected|output_matched|agent_status_changed|scroll_changed`;
  - `layout.updated`;
  - `worktree.created|opened|removed`.

  Subscriptions **don't replay**. A slow reader gets **`events_lost`** and must resubscribe and re-read the snapshot; the docs include a reconcile recipe.
- **Conventions:**
  - Output is JSON by default; worktree commands are JSON-only.
  - IDs are qualified by workspace (`w1:p2`). The docs say to read them from responses rather than predict them.
  - `--current` resolves through `HERDR_PANE_ID`.
  - Exit code **1** means a server error or timeout (with JSON on stderr); exit code **2** means bad syntax.
- **Error codes:** `pane_not_found`, `agent_not_ready`, `agent_not_running`, `agent_not_idle`, `agent_blocked`, `agent_prompt_stalled`, `timeout`, `events_lost`, `server_not_running`, `invalid_resume_argv`, `resume_not_accepted`, `worktree_busy`, `not_git_worktree`, `confirmation_required`, `plugin_disabled`, `platform_unsupported`, `invalid_regex`, `unknown_method`, `endpoint_busy`, and others.

#### DD1.5 UX [V]

- **Model.**
  - Session → **Workspace** ("Space": one per repo or task) → **Tab** → **Pane** (BSP splits).
  - A **sidebar** with two sections:
    - **Spaces**: worktree groups nested under their parent repo, with git branch and tokens.
    - **Agents**: sorted by `spaces` or `priority`.
  - **Go To** (`prefix+g`) lists every agent and terminal; Navigate mode is `prefix+w`.
  - A **mobile single-column layout** kicks in at 64 columns or fewer, for SSH clients on phones.
- **Keys.** Prefix `ctrl+b` (several prefixes allowed since 0.9.2).
  - Panes and tabs: `c` new tab, `v`/`-` split, `hjkl` focus, `shift+hjkl` swap, `x` close, `z` zoom, `r` resize mode.
  - Text: `[` copy mode (vi-like), `e` opens the scrollback in `$EDITOR`.
  - Workspaces: `shift+n` new workspace, `shift+g` new worktree.
  - Other: `b` toggles the sidebar, `q` detaches, `o` jumps to the notification target, `s` opens settings, `?` help with a filter.
  - Custom `[[keys.command]]` entries, including `type="popup"` (for example lazygit).
- **Mouse is first-class:** click, drag borders, right-click menus, Ctrl-hover links, double-click to select a word, copy on select.
- **Notifications:** `ui.toast.delivery = off|herdr|terminal|system` (default off), a 1 s delay, suppressed for the active tab. Sounds can be overridden per agent with custom mp3s, and BEL is passed through. Clicking a native notification focuses the pane.
- **Config.**
  - TOML in `~/.config/herdr/config.toml`. Hot reload via `prefix+shift+r` or `server reload-config`; validate with `config check`.
  - Themes: catppuccin is the default, light/dark switch automatically, and `status_indicators = dots|symbols`.
  - Sidebar rows are built from tokens with style rules based on their values.
  - Notable keys: `worktrees.directory`, `advanced.scrollback_limit_bytes`, `ui.tab_bar_right` (can show async command output), `session.resume_agents_on_restore`, `remote.manage_ssh_config`.

#### DD1.6 Remote, plugins, ecosystem

- **Remote [V].**
  - `herdr --remote host|ssh://u@h:p` draws the UI locally, with *local* keybindings, and installs or updates herdr on the remote after you approve.
  - `herdr machine add|list|status|reconnect|rename|enable|disable|remove` gives one window for the local machine plus N SSH machines:
    - a combined agent list;
    - independent reconnects;
    - cached state shown dimmed while a machine is offline;
    - ControlMaster reuse (`ControlPersist 600`);
    - MFA support via `machine reconnect`;
    - compression and health checks.
  - `herdr --machine <label> agent prompt …` forwards CLI and API calls over SSH.
  - Agent IDs are scoped to one server; there's no cross-machine agent CLI yet.
  - Mosh support is requested (25 votes) but not built.
- **Plugins [V].**
  - A plugin is a `herdr-plugin.toml` (with `min_herdr_version` and `platforms`) plus **out-of-process commands**. It can declare:
    - `[[build]]`;
    - `[[actions]]`: keybindable;
    - `[[events]]`: hooks on named events such as `worktree.created`;
    - `[[panes]]`: terminal UIs shown as an overlay, popup, split, tab or zoomed pane;
    - `[[link_handlers]]`: a URL regex mapped to an action;
    - `[[startup]]`.
  - Plugins get `HERDR_PLUGIN_{ID,ROOT,CONFIG_DIR,STATE_DIR,CONTEXT_JSON,EVENT_JSON}`. There's no storage API, and v1 has no native panes.
  - `plugin install owner/repo[/subdir] [--ref]` shows a trust preview before installing.
  - The **marketplace** indexes any repo with the GitHub topic `herdr-plugin`, without review. A topic search finds **1,867 repos**; the site says 1,789. Many are only loosely related, such as dotfiles.
- **Ecosystem** (stars as of 2026-10-20):

  | Kind | Project | Stars | Notes |
  |---|---|---|---|
  | Review / projects | **herdr-reviewr** (persiyanov) | 916 | Diff review in a pane; line comments go to Claude, Codex, OpenCode or Pi |
  | | **herdr-projects** (eliasstravik) | 834 | "Mission control… run dozens of coding agents from one board", with a second agent reviewing |
  | | **herdr-plus** | 361 | Projects and Quick Actions |
  | | herdr-annotate | 682 | |
  | | herdr-browser (by Can) | — | Chromium over CDP in a pane |
  | Mobile / web | **collie** | 1,414 | Self-hosted **PWA** with push, for herdr, tmux and zellij |
  | | **herdr-web-ui** | 1,257 | Chat plus a live terminal per pane, with web push |
  | | **Heeler** | 617 | Native iOS client over SSH |
  | | roamgate | 311 | |
  | Bridges | **ccgram** | 279 | Telegram ↔ tmux / herdr / agterm |
  | | **Agentchute** | 17 | Markdown shared inboxes for agents |
  | | **Moshi** | — | Closed-source iOS/Android mosh terminal that treats herdr as a peer of tmux via `moshi-hook` [S] |
- **Agent skill.**
  - `skills/herdr/SKILL.md` (214 lines) is built into the binary (`herdr --skill`). It can also be installed with `npx skills add herdrdev/herdr --skill herdr -g`.
  - Its guardrail: act only if `HERDR_ENV=1`, and only when the user mentions herdr. Discover syntax with `herdr <group>`, and read IDs from the JSON.
  - The repo's AGENTS.md (329 lines) and CONTRIBUTING.md contain rules aimed at agents.

#### DD1.7 What herdr lacks, and its plans

| Capability | Status in herdr | Tag |
|---|---|---|
| Git worktrees | **Has them**: create/open/list/remove, grouped under the parent repo, configurable directory, `--trust-repository`, removal safety for submodules and dirty trees | [V] |
| Setup scripts, `.env` copy, port allocation | **None in core**; plugins do it through `worktree.created` hooks | [V] |
| Projects (multi-repo registry) | None (herdr-plus and herdr-projects plugins) | [V] |
| **Identity profiles** (agent account, git author, SSH key, gh) | **None** | [V] |
| Review → commit → PR | None in core; the site says its worktree and diff review flow "pairs with" one. herdr-reviewr covers it | [V] |
| Fan-out / compare / recipes / gates | Primitives only; no run model (herdr-projects plugin) | [V] |
| MCP / ACP / structured chat | **None** | [V] |
| `error` state | None | [V] |
| **OSC 7501** | **Not implemented.** Requested in Discussion **#5073** (20 votes, 2026-10-08) and **#5106** ("Support Claude Code's Program Status Protocol") | [V] |
| jj | Not supported (two Discussions, about 34 votes each) | [V] |
| Exact screen after a *server* restart | Only through opt-in `pane_history` or the experimental handoff | [V] |
| **Stated roadmap** | Herdr Cloud; the CLI and agents working across machines; any machine type (sandboxes, VMs); further out, moving an agent session between machines; more clients; core stays small, extensions do the rest | [V] blog posts 08-06 and 09-07 |

#### DD1.8 Community sentiment [S]

- **HN, "Herdr: One terminal to rule them all"** (item 48756578, about July 2026, **404 points, 178 comments**).
  - Praise:
    - the best visibility into which agent needs attention;
    - "worked out of the box, has actually good, working mouse support";
    - worktree management called "the killer feature";
    - clickable areas work on mobile via Moshi.
  - Complaints:
    - "what over tmux?"; for some, zmx is enough;
    - occasional typing lag;
    - WezTerm selections get cleared;
    - an agent waiting on a shell command shows as "idle";
    - no jj;
    - **"Vibecoded. Nope."**;
    - the logo wall called "lawyer-bait".
- **HN, "Connecting the machines"** (item 49612818, 93 points): pushback on accounts, VC funding and enshittification; Tailscale overlap; requests for self-hosting.
- **Blogs** (Flavio Copes, Bitdoze, tecmint, GIGAZINE and others) are mostly positive and call it "tmux for agents". Caveats they raise: retraining muscle memory, a smaller ecosystem than tmux, and advice to keep tmux + workmux if you want battle-tested tools.
- **Issue clusters [V]:**
  - keyboard and IME edge cases, the largest group (Kitty keyboard protocol, WezTerm, Windows, CJK);
  - Windows parity;
  - CPU with many clients;
  - **manifests churning with every agent CLI release**;
  - Codex 0.157's shared daemon broke per-pane sessions (#4649, open).

#### DD1.9 Head-to-head

| Capability | herdr | ptyro today | ptyro plan |
|---|---|---|---|
| Daemon owns PTYs; detach and reattach | ✅ mature | ✅ (M1/M2) | — |
| Emulator | libghostty-vt (Zig FFI) + Kitty graphics | alacritty_terminal | Backlog: libghostty |
| Restore after daemon restart | Layout + native resume for 18 agents + agent-reported argv; history opt-in | — | M4.11 |
| Live upgrade keeping processes | ✅ experimental (fd passing) | ✅ M8.1 (re-exec in place, so children stay ours; rollback) [V 2026-10-21] | — |
| Multi-client independent views | ✅ (0.9.0) | ? | — |
| Projects registry | ❌ (plugins) | ✅ M3.2 | — |
| Worktrees | ✅ with removal safety | ✅ M3.4–M3.6 (lock, safety matrix, `.worktreeinclude`, share) | — |
| Setup / run / archive scripts + trust | ❌ (event-hook plugins) | ✅ M3.8–M3.9 | — |
| Port blocks + listening-port detection | ❌ | ✅ M3.10 | — |
| **Identity profiles** | ❌ | ✅ M3.17–M3.21 | — |
| Agent coverage | **22 manifests + 5 self-reporters + 18 integrations** | Claude / Codex / Gemini / OpenCode / Antigravity planned | M4 |
| Status engine | Strict screen manifests + hook authority + `seq`; no error state | Hooks + **OSC 7501** with precedence; error state | M4.4, M4.20 |
| `agent explain` / done-unseen / stall errors | ✅ | — | R-STA-02/03/04 |
| JSON API + JSON Schema + `events_lost` | ✅ | partial | M5 |
| Review → PR | ❌ (herdr-reviewr) | — | M6 |
| Fan-out / recipes / gates / MCP | ❌ (herdr-projects) | — | M7 |
| Multi-machine in one window | ✅ | — | M9.1 |
| PWA / phone / push | ❌ in core (collie, herdr-web-ui, Heeler, Moshi) | — | M9 |
| ACP / multi-repo change sets | ❌ / ❌ | — | M10 / M11 |
| Plugins | ✅ out-of-process TOML, ~1.9k repos | — | Backlog |
| Windows | ✅ GA | — | Backlog |
| License | Apache-2.0 | MIT | — |

**Where herdr is clearly ahead:**
- **Maturity:** 61 stable releases, about 290k lines of Rust, Windows GA.
- **Detection:** breadth, plus maintenance through remote manifests, hot reload and `explain`.
- **Resume:** native resume for 18 agents.
- **Federation:** multiple machines and independent clients.
- **Mouse and Kitty graphics.**
- **Live handoff.**
- **Distribution:** brew, mise, nix, self-update, and self-install over SSH.
- **Momentum:** 45k★, about 1.6M downloads, $6M raised.
- **Agent vendors already target its `report-agent` API:** Crush, Kiro, Command Code, Muse and Prime Agent.

**Where ptyro is ahead:**
- **The workspace model:** projects, safe worktrees with scripts, a trust fingerprint and ports, and **per-workspace identity profiles**. Nobody else has these.
- **Status:** an OSC 7501-first engine with a formal precedence and an `error` state, and herdr users are asking for 7501 right now.
- **Integration:** review→PR, an orchestration run model, `ptyro mcp`, ACP and change sets are *built in*, not left to third-party plugins.

**Threats:**
1. **The plugin ecosystem is eating ptyro's differentiators.** herdr-projects, herdr-reviewr, herdr-plus, collie and herdr-web-ui already cover parts of M3, M6, M7 and M9.
2. **`report-agent` is becoming the de facto pane-state API** that agent vendors target.
3. **Herdr Cloud** removes the SSH-setup pain that M9 targets.
4. Funding and hiring will speed up the core: expect a cross-machine agent CLI and more clients.
5. The small-core stance means herdr is unlikely to add profiles, scripts or review. That keeps ptyro's niche open, but makes "herdr + plugins" a credible bundle.

#### DD1.10 Idea candidates

*Already captured and confirmed: R-STA-02 `agent explain`, R-STA-03 stall/blocked errors + `completion_seq`, R-STA-04 done-unseen, R-STA-01 accept `report-agent`, R-ORC-18 ship a skill.* The new ideas below are filed in Part III under §III.2 "From the herdr and tuios deep dives".

1. **herdr compatibility shim.** In ptyro sessions:
   - export `HERDR_ENV=1`, `HERDR_PANE_ID` and `HERDR_BIN_PATH=<ptyro-herdr-shim>`;
   - accept `pane report-agent | report-agent-session | release-agent | report-metadata`, with the same flags, `--seq`, and `-- RESUME_ARGV`;
   - map them onto the status engine and the resume store.

   Crush, Kiro, Command Code, Muse and Prime Agent would then get confident status with no ptyro adapter. **Risk:** agents that know the herdr skill will see `HERDR_ENV=1` and try other `herdr` commands. Make the shim opt-in per profile, and reject unknown subcommands with a clear error. tuios already ships this pattern (DD2.4g).
2. **Agent-reported resume argv** (a plain command on PATH, no apostrophes, at most 64 arguments and 8 KiB, accepted only once the source holds the session). A generic resume path for M4.11 that needs no per-agent code.
3. **Detection manifests as data.** Versioned TOML with region, priority, `any`/`not` and `visible_blocker`. Local overrides in `~/.config/ptyro/agent-detection/`, an optional signed remote update, and hot reload. herdr's files are Apache-2.0 and could seed fixtures if their license headers and a NOTICE are kept.
4. **Strict "blocked" and a labelled fallback reason** shown in `explain` and as a low-confidence idle glyph. Document that "idle is not authorization".
5. **`read --source detection` and transcript reads from the alternate screen.** herdr scrolls an idle full-screen agent to collect its history, then restores the viewport. Without this, M5's `read` is weak for Claude and OpenCode.
6. **`events_lost` semantics.** `ptyro events` should report a full buffer as an explicit error, and the docs should give a subscribe → snapshot → reconcile recipe (`ptyro snapshot --json`).
7. **A JSON Schema for the control plane** (`ptyro api schema --json`, generated with schemars), with a rule that clients ignore unknown fields.
8. **Declarative layouts:** `ptyro layout export|apply`, with label, cwd, argv and env.
9. **`terminal observe/control` NDJSON streams.** The cheapest path to ccgram, Moshi and web clients before the M9.2 WebSocket.
10. **Shutdown safety on the host:** a logind delay inhibitor, atomic session writes, rotating snapshots, and recovery copies kept when a load fails (M8).
11. **An agent-view projection API** (a declarative filter and sort over the attention list).
12. **Display-only metadata tokens** (`--token summary=… --ttl-ms`, capped at 80 characters), kept separate from semantic state.
13. **Performance lessons:**
    - static status glyphs instead of spinners;
    - don't render hidden panes;
    - don't repaint on mouse movement;
    - cap redraws at the display rate rather than a fixed 60 Hz (herdr's top complaint).
14. **Staggered agent restore** (100 ms apart), resumed only after a client has supplied size and theme.
15. **Environment hygiene for new sessions.** Strip outer-terminal markers (iTerm2, WezTerm, Kitty, tmux, Zellij) and Claude/Codex session variables, and set `TERM_PROGRAM=ptyro`. herdr hit this class of bug (#4104).
16. **`ptyro --remote host` self-install**, after approval and with a checksum check, plus ControlMaster reuse and an MFA-friendly reconnect.
17. **Lead with OSC 7501 publicly** (herdr #5073, #5106). Consider a tiny `report-agent` → OSC 7501 bridge.
18. **Interop targets:** a ptyro backend for ccgram; being attachable from Moshi, collie and herdr-web-ui; Agentchute-style Markdown inboxes as an M7.7 mailbox transport.
19. **Contribution policy.** herdr auto-closes unsolicited PRs and uses an issue agent, because of the flood of agent-generated PRs. Decide ptyro's policy early and put it in AGENTS.md and CONTRIBUTING.md.
20. **Positioning.** herdr calls manager apps "clients without a runtime" and says review flows "pair with it". ptyro's answer: **runtime + workspace model + review/orchestration layer, integrated** — profiles, scripts and PRs that herdr won't put in its core.

**DD1 sources.**
- **Primary:**
  - The herdrdev/herdr repo: README, CHANGELOG, Cargo.toml, `crates/ghostty-vt`, `vendor/*.patches.md`, `src/detect/manifests/*.toml`, `skills/herdr/SKILL.md`, CONTRIBUTING, docs.
  - License commits `cfffe659` and `cd5ea1be`, and issue #1340.
  - The GitHub API: repo, releases, contributors, issue searches, `topic:herdr-plugin`.
  - herdr.dev pages: /, /compare/, /plugins/, /cloud/.
  - herdr.dev blog posts: joining Y Combinator, the seed round, "connecting the machines", "95% less CPU", "coding agents are becoming runtimes".
  - ycombinator.com/companies/herdr.
  - Discussions #5073 and #5106.
- **Secondary:**
  - HN items 48756578 and 49612818.
  - runtimewire.com.
  - flaviocopes.com/herdr-alternatives.
  - bitdoze.com.
  - The dev.to post on CCGram v4, and github.com/alexei-led/ccgram.
  - github.com/agentchute/agentchute.
  - getmoshi.app.
  - zeli.app.

---

### DD2. tuios — deep dive (Gaurav-Gosain/tuios, tuios.dev)

**Snapshot:** 2026-10-20. Repo HEAD `4c9ad09` (2026-10-20 18:10 +0400), latest release v0.10.1 (2026-10-17). Sources are a shallow clone of the repo (docs and source read only, nothing built or run), the GitHub REST API, the Program Status Protocol spec page, and HN Algolia.

**Tags:** [V] I checked it in a primary source (repo, docs, code, API, spec). [S] secondary source or my inference. [U] not verified.

**Headline:** tuios is no longer "a tmux alternative with agent badges". Since v0.8.0 (2026-09-27, about 2,400 commits after v0.7.0) it ships almost all of ptyro's planned M4, M6, M7, M9 and part of M10:
- review notes, turn checkpoints, `ship pr`
- `fan compare` / `verify` / `keep --merge`
- ACP and Codex app-server headless agents
- MCP server
- multi-machine hosts, Tailscale, a phone pairing flow, tuios-web
- approvals answered from the Inbox, with risk rules

What it does **not** have: first-class projects, worktree setup scripts / `.env` copy / port allocation, per-workspace identity profiles, multi-repo change sets, and an exact live screen after a daemon restart. The existing RESEARCH.md entry for tuios undersells it and needs updating. Two small corrections: hooks cover **11** events, not 10, and the precedence ladder includes `transcript`.

---

#### DD2.1. Identity

| Item | Finding |
|---|---|
| Author | One individual maintainer (`Gaurav-Gosain` on GitHub), who also maintains the related `sip` (web serving), `tuiffects`, `tuitest` and `youterm` [V go.mod/README] |
| License | MIT (main repo, `tuios.dev/vt`, `tuios.dev/progstatus`). `vt/` started as a copy of `charmbracelet/x/vt`. The harness manifests carry an Apache-2.0 `LICENSE-herdr`, meaning they derive from herdr's manifests [V] |
| Stars / forks / watchers | 5,531 / 257 / 14 (2026-10-20). Created 2025-09-06 [V API] |
| Contributors | 51 in the API listing (including anonymous); the author wrote about 95% of commits [V] |
| Issues / PRs | about 940 numbers used. 211 issues in total, 37 open. 154 PRs from people other than the author [V API] |
| Commit cadence | 2026-03: 141, 04: 37, (May–Jun: none in the fetched window), 07: 284, 08: 1,075, 09: 1,294, **10 (to the 20th): 1,650, about 80 a day** [V git]. Some commits carry `Co-Authored-By: Claude Sonnet 5.5` [V]. The pace strongly suggests agent-driven development [S] |
| Release timeline | 51 releases. v0.0.5 (2025-09-06), v0.1.0 (2025-10-31), v0.4.0 (2025-12-02), v0.5.0 (2025-12-25), v0.6.0 (2026-01-25), v0.7.0 (2026-03-28), then a 6-month gap, then **v0.8.0 (2026-09-27), the agents pivot**, v0.9.0 (10-08), v0.10.0 (10-16, `tuios.dev/vt` split out, Omarchy theme follow), v0.10.1 (10-17) [V API] |
| Platforms | Linux, macOS, Windows (native ConPTY; there are PowerShell bugs such as #491), FreeBSD, OpenBSD. Docker image `ghcr.io/gaurav-gosain/tuios`. A Termux crash is reported (#52, open). WASM build for the tuios.dev/learn tour [V] |
| Install | `brew install tuios`, `gaurav-gosain/tap/tuios-ghostty` (libghostty build), AUR `tuios-bin`, Nix (in nixpkgs too), `curl … install.sh`, `go install`, release archives with `checksums.txt`, `tuios update` self-update [V README] |
| Bus factor | **1.** About 95% of commits are by the author [V] |
| Funding / company | Ko-fi only (`FUNDING.yml`). No company, no VC, no paid tier found [V/S]. A phone app is "in private testing" for Android [V SESSIONS.md] |
| Distribution channels | Terminal Trove "Tool of the Week" [V README]. First-class Omarchy integration (v0.10.0). The GitHub topics target `claude-code`, `codex`, `tmux-alternative`, `zellij-alternative` [V] |
| Size | About 860k lines of Go including tests, and about 34k lines of docs in `docs/` [V wc] |

#### DD2.2. Architecture

- **Daemon and client [V SESSIONS.md].** `tuios` with no arguments attaches to a daemon session and auto-starts the daemon (`startup.daemon` defaults on). `--standalone` runs without the daemon. The daemon owns the PTYs and runs an emulator for each pane. Clients are viewers that resync screen and scrollback on attach. Several clients can attach with full control each; there is no per-client permission tier. Clients reach the daemon through:
  - the local Unix socket (`$XDG_RUNTIME_DIR/tuios/tuios.sock` or `/tmp/tuios-<uid>/`, mode 0700)
  - SSH (`tuios ssh`, built on wish)
  - the browser (`tuios-web`)

  One daemon per user.
- **Socket and protocol [V docs/protocol.md].** The client's binary frames and the **JSON verb protocol** share one socket. The daemon classifies a connection by its first byte: `{` means JSON, `0x00`/`0x01` is a length-prefixed binary frame.
  - Requests are one JSON line each, `{"id":1,"verb":"list-windows","params":{…}}`. Results are `{"id","result":{"type":…}}`. Errors are `{"id","error":{"code","message","hint":{verb,command,param,accepted,did_you_mean,available,detail}}}`.
  - `hello` negotiates `protocol` / `min_protocol` and reports `link_policy`, `pane_grants` and `os`.
  - `list-verbs` is authoritative. There are about 100 documented verb sections (`### ` headings) and roughly 400 distinct verb names in the source [S, rough grep].
  - The event stream is `subscribe` (resumable with `last_seq`/`boot_id`, with a slow-subscriber policy) and `wait-for`.
  - There is no application-level auth token; file permissions guard the socket.
- **Terminal emulation [V].** It has its own emulator, `tuios.dev/vt`: a fork of `charmbracelet/x/vt`, with the parser a modified copy of `charmbracelet/x/ansi`'s. It is pure Go, with no cgo, and became its own module in v0.10.0. The build tag `-tags ghostty` swaps in **libghostty-vt** through `go.mitchellh.com/libghostty` (cgo), shipped as the `tuios-ghostty_*` archives. A differential test harness feeds both emulators the same bytes. Features: reflow, kitty graphics (including animation `a=f` damage patches and shm), sixel, kitty keyboard, modifyOtherKeys, OSC 133, OSC 52, OSC 9;4, OSC 7501, mode 2026 and 2031. It does not use vt10x.
- **Rendering [V README].** Bubble Tea v2 / Lipgloss v2 / ultraviolet MVU. Rendering is event-driven: PTY goroutines signal a channel and nothing ticks, so idle CPU is near zero. v0.10.1 says "no timer runs while nothing changes". Other techniques: viewport culling, LRU style cache, pooled buffers, and the emulator's own `Render()` for unfocused panes. `docs/perf.md` is 4.2k lines.
- **Persistence [V SESSIONS.md "What Survives"].**
  - Detach / client crash: everything survives (processes, screen, scrollback).
  - Daemon restart, crash or reboot is "resurrection". The JSON state (windows, geometry, BSP tree, layout mode, workspace, names, cwd from `/proc/<pid>/cwd` or libproc, space) comes back, plus **pane history**: up to 1,000 lines by default, styled, compressed, ≤2 MiB per pane and ≤16 MiB per session, under `$XDG_STATE_HOME/tuios/sessions/scrollback/`. It is shown *as text above a divider*: `-- tuios: restored from Sep 30 14:02, fresh shell in … --`.
  - Shells are new (`TUIOS_RESTORED=1`). Running programs and their live screens are lost; for a full-screen app it saves the shell's screen, not the app's.
  - Agents: the conversation id is persisted, and the Inbox offers `claude --resume <id>` and similar (`daemon.resume_agents = ask|auto|off`).
  - Save timing: structure within about 2 s of a change, cwd every 30 s, history at most once per 30 s per pane. A crash loses up to 30 s.
  - `restart-server` re-execs a new build and clients auto-reattach within 15 s. Open PR #876 hands panes to a new image in place, i.e. live upgrade without losing processes [V issue list].

#### DD2.3. Window management [V README, LAYOUT_MODES.md, CONFIGURATION.md]

- **Tiling:** BSP (spiral, preselection, equalize, opt-in aspect-aware smart split), **master-stack**, **scrolling** (niri-style infinite horizontal columns), or floating with drag and resize. Shared tmux-style borders, zoom (95% by default or full screen), drag a pane onto another pane's edge to split (#684).
- **Grouping:** 9 numbered workspaces per session. Sessions can be filed into "spaces" (v0.10.x). The aggregate view searches every window. Multifocus broadcasts input and includes a multi copy mode.
- **Modes:** vim-like Window Management mode and Terminal mode. Prefix is `Ctrl+B`. `Prefix+Esc` or `Alt+Esc` returns to WM mode. There is a which-key popup on prefix hold, keybinding presets for tmux, screen and zellij (v0.10.1), and `tuios keybinds doctor|explain`.
- **Palette and launcher:** `Ctrl+P` command palette. `Alt+Space` launcher over `$PATH` plus desktop apps, frecency-ranked.
- **Rail:** right-hand sidebar with sessions, terminals, files, git, agents, and custom sections fed by commands. Dock components.
- **Config:** TOML at `~/.config/tuios/config.toml`, with 23 `[keybindings]` sections. `tuios set-config`, `tuios list-options`, an in-app settings page (`Prefix+,`), hot reload, `tuios look` to apply a shared setup, and JSON themes.

#### DD2.4. Agent state and OSC 7501

**4a. tuios's internal state model [V AGENT_STATE.md §States]**

The states are `none` (default, never persisted), `working`, `needs_input` (with `blocked_by` = `approval|question|auth|""`), `idle`, `done` (a finished turn, tracked as unread per client), `errored` and `unknown` ("an agent is present and nothing says what it does"; written by the silence timer instead of `idle`).

Fields derived from the state:
- `needs_you` is true for `needs_input` and `errored`.
- `ready` (whether `ask-agent` would type now) is true for `idle`, `done`, `errored` and `none`. It is false for `needs_input` and `unknown`.
- Also reported: `completion_seq`, `finished_unread`, `evidence_age_ms`, `identity` (report, manifest, list or hint) and `confidence` (certain, strong or none).

**4b. The OSC 7501 implementation [V docs/PROGRAM_STATUS.md, progstatus/progstatus.go]**

- **Revision:** tuios states it follows **rev 0.2 (2026-10-06)**. The live spec is **rev 0.4 (2026-10-10)** [V spec page]. The changes since 0.2:
  - 0.3: absent `progress` means indeterminate, and programs may report without probing first.
  - 0.4: the probe reply MAY list `states=` and `kinds=`, and terminfo was removed.

  tuios's behaviour already matches 0.3 (`NoProgress = -1`).
- **Probe reply:** tuios answers only the bare `ESC ] 7501 ; ? ST` (`QueryReply = "\x1b]7501;?"`), with the terminator the query used. That is valid under 0.4, because absent lists mean the default sets. ptyro replies `OSC 7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth ST`, which is also valid.
- **Example report:** `printf '\e]7501;state=blocked:kind=permission:app=terraform:msg=QXBwbHk…\e\\'`
- **Keys:** `state` (`idle|working|done|blocked|error|clear`), `kind` (`permission|question|auth`; ignored with any other state or value), `progress` (0–100, only with working or blocked), `app` (≤32 bytes; invalid characters make it absent, but more than 32 bytes **discards the report**), `title` / `msg` (strict base64 UTF-8), `id` (path, ≤128 bytes, 8 levels, 32 bytes per segment). The value charset is `A-Z a-z 0-9 _ . , + / = -`. The last repeated key wins.
- **Discard rules:**
  - An invalid `id` discards the whole report and never falls back to the root record.
  - Control characters or non-UTF-8 in the decoded `msg`/`title` discard the report.
  - Base64 padding is optional, but padding that is present must be correct and unused bits must be zero.
  - The whole sequence is limited to 4,096 bytes.
  - Inside the sequence, CAN and SUB cancel it, a stray ESC terminates it, and other C0 controls are ignored.
- **Mapping into tuios:**

  | OSC 7501 | tuios state | `blocked_by` |
  |---|---|---|
  | `working` | `working` | |
  | `blocked`, `kind=permission` | `needs_input` | `approval` |
  | `blocked`, `kind=question` | `needs_input` | `question` |
  | `blocked`, `kind=auth` | `needs_input` | `auth` |
  | `blocked`, no kind | `needs_input` | `""` (never guessed from `msg`) |
  | `done` | `done` (unread) | |
  | `error` | `errored` | |
  | `idle` | `idle` | |

  A pane holding any record becomes an "agent pane" with `source: "program"`. The records appear as `program_status` in `get-agent-state` / `list-agents`.
- **Multiple records:** the pane shows the most urgent record, in the order blocked > error > done > working > idle. Ties go to the root record, then to the most recently changed. `app` is inherited from the nearest ancestor. At most 256 records per pane, with LRU eviction.
- **Record lifetimes (tuios's choices):**

  | Event | working / blocked / idle | done / error |
  |---|---|---|
  | OSC 133 A (prompt) | removed | kept |
  | Foreground reporter exits | removed within 2 s | kept |
  | Reporter was a background job or the pane's own process | kept | kept |
  | **User types in the pane** | kept | **removed** |
  | RIS | removed | removed |
  | DECSTR, alternate-screen switch | kept | kept |

  - Exit detection records the **foreground process group** when the report's bytes are read, and the detector polls every 2 s.
  - Known gap: a `printf …; exit` that reports and exits in the same instant can be misread as a background job's report.
  - Keys sent with `send-keys` / `send-text` do not count as user typing.
- **OSC 9;4:** read as agent state only on known agent panes. It stops for a pane after its first 7501 report, until RIS. It is never mapped to the root record.
- **Security:** markup is never interpreted; bidi overrides, zero-width and other invisible characters are stripped. Every display names the pane id, which the program cannot set. `app` is a label only and never grants harness approval privileges. Alerts from a self-reported state go out at most once per 30 s per pane, holding back the newest state in between.
- **tuios as a reporter to its host terminal (nesting) [V internal/app/host_program_status.go]:**
  - Each local or SSH client probes its terminal with `\x1b]7501;?\x1b\\`. It accepts any reply that *starts with* `\x1b]7501;?` ("anything after the ? is for a later revision, and is ignored"). ptyro's longer reply therefore enables it.
  - It emits one record per pane: `id=<session≤32>/<first 8 chars of pane id>` (session `local` when unnamed), `title` = pane name, `app` = harness or the pane's own app, `msg` = agent message, plus progress.
  - **It sends no root record.**
  - State mapping: needs_input → blocked plus kind; a seen `done` → `idle`; `unknown` and `none` → no record.
  - It sends only changes, batched at most every 250 ms, and keeps ≤64 records. On detach or exit it sends `state=clear:id=…` for each record, and a local client also sends RIS. Off switch: `[agents] host_program_status = "off"`. tuios-web does not probe.
- **`tuios status` CLI:** `tuios status working --app build --msg 'Compiling' --progress 40`, plus `--kind`, `--title`, `--id`, `--clear`, `--stdout`. It writes to `/dev/tty`, needs no daemon, and refuses reports a terminal would discard.

**4c. The `tuios.dev/progstatus` package [V progstatus/]**

- Module `tuios.dev/progstatus` v0.1.0, stdlib only, MIT; tags use the prefix `progstatus/v0.1.0`.
- Constants: `Command=7501`, `MaxSequence=4096`, `MaxKey=16`, `MaxMsgEncoded=2732`, `MaxMsgDecoded=2048`, `MaxTitleEncoded=256`, `MaxTitleDecoded=192`, `MaxApp=32`, `MaxID=128`, `MaxSegment=32`, `MaxDepth=8`, `MaxRecords=256`, `NoProgress=-1`.
- Types: `State` (`Idle`, `Working`, `Done`, `Blocked`, `Error`, `Clear`), `Kind` (Permission, Question, Auth), `Report{State,ID,Kind,Progress,App,Title,Msg}`, `Event{Report,Reset}`, `Record`, `Store`.
- Functions: `IsQuery(body)`, `QueryReply`, `SequenceLen(payloadLen,bel)`, `Parse(body,seqLen)(Report,bool)`, `ValidID`, `ValidApp`, `IsControl`, `Urgency(State)`, `InheritApp`, `Summary(recs)(Record,bool)`, `Encode(Report)`, `Sequence(Report)`, `FitText`, `FitSegment`.
- `Store` methods: `Apply`, `ApplyFrom(r,now,group)`, `Handle`, `HandleFrom`, `GroupOf`, `Groups`, `DropEnded(func(group)bool)`, `DropTransient` (on OSC 133 A or exit), `DropFinished` (on typing), `Reset` (RIS), `Seen`, `Len`, `HasFinished`, `Records`.
- The API "can change between minor versions" (v0).

**4d. Precedence ladder [V internal/session/agent_source.go `rank()`]**

The ranks are **report 40 > program 38 > transcript 35 > osc 30 > screen 20 > detect 10 > stall 0**. The docs table lists stall before detect, but the code ranks detect above stall.
- A source may overwrite a claim of equal or lower rank, and may always update its own claim.
- A losing write returns `"applied": false` plus the state that stands, not an error.
- Only the emulator sets `program`. `set-agent-state` accepts only `report|osc|screen|stall`, and `source` defaults to `report`.
- `yieldAgentClaim` lets a source say "I have nothing further to say" without clearing the state.

**4e. Visible-blocker override [V AGENT_STATE.md]**

A screen rule may overwrite a higher-ranked stale claim only if *all* of these hold:
1. The rule's state is `needs_input`.
2. The claim is not already `needs_input`.
3. The pane has printed output since the claim was stamped.
4. The claim has gone unrefreshed for ≥2 s.

Read-backs of an identical claim do not restamp it; this stops a spinner title from keeping the claim fresh forever. The override is **a loan**: it records the displaced claim and restores it exactly when no rule matches any more. `source` shows `screen` while the loan stands.

**4f. Stall heuristic [V]**

- Only `working` goes to `unknown`; it becomes `idle` only when the daemon has no screen tier at all.
- Before demoting, the screen tier gets a last look.
- The silence clock is the later of the last output and the time `working` was set. The default is 30 s (`TUIOS_AGENT_STALL_SECONDS`, 0 disables).
- It never touches `needs_input`, `done` or `errored`, and never promotes a pane into `working`.
- **Gap:** a hooked agent that hangs with a spinner going never stalls (issue #898, 2026-10-18). The author plans an `agents.stall_after` for report-only heartbeats that ignores output.

**4g. herdr interop [V AGENT_STATE.md]**

- Every pane gets `HERDR_ENV=1`, `HERDR_SOCKET_PATH=<daemon sock>.herdr`, `HERDR_PANE_ID=w<session12hex>:p<window12hex>`, `HERDR_TAB_ID`, `HERDR_WORKSPACE_ID`, and `HERDR_BIN_PATH` (a symlink named `herdr` pointing to tuios, which parses herdr 0.9.5's CLI).
- Wire format: one JSON line per connection, `{"id","method","params"}` → `{"id","result":{"type":"ok"}}`. `ping` returns `{"type":"pong","version":"0.9.5+tuios","protocol":22,"server":"tuios"}`.
- Method mapping:
  - `pane.report_agent` working → `working`.
  - blocked → `needs_input`. For Crush, a message starting with `Permission` (or no message) is `approval`; otherwise `question`.
  - idle → `done` if the pane was working or needs_input, else `idle`.
  - unknown → no change.
  - `pane.release_agent` → `none`.
  - `pane.report_agent_session` → session id. `pane.report_metadata` → metadata. `notification.show` → notification rules.
- `seq` must increase per source. If herdr's own hook scripts and tuios's integration both report, tuios's integration wins.
- The full herdr socket API (snapshot, lists, `pane.read`, `events.subscribe`) is emulated and gated by grants, so herdr plugins, Collie and bar widgets work against tuios.
- Outbound mapping: needs_input → blocked; done and errored → done; unknown and none → unknown.
- Crush, Kiro, Command Code, Muse Code and Prime Agent report this way with nothing installed.

**4h. `tuios integration install` (19 harnesses) [V AGENT_STATE.md §Harness integrations]**

- **Twelve report state:**

  | Harness | What it writes |
  |---|---|
  | Claude Code | `hooks` in `~/.claude/settings.json` (integration v3, including SubagentStart/Stop) |
  | Codex | `~/.codex/hooks.json` (not `notify`) |
  | Gemini CLI | `~/.gemini/settings.json` |
  | opencode | `plugins/tuios-agent-state.js` (V1 server hooks; V2 TUI `tui.js`) |
  | Kilo | the opencode plugin under Kilo's id |
  | Amp | `plugins/tuios-agent-state.ts` |
  | Kimi Code CLI | marker-delimited `[[hooks]]` in `~/.kimi-code/config.toml` |
  | Pi | `extensions/tuios-agent-state.ts` |
  | oh-my-pi | extension, needs omp ≥18.3.2 |
  | GitHub Copilot CLI | `~/.copilot/hooks/tuios.json` |
  | Cursor Agent | `~/.cursor/hooks.json` |
  | Qwen Code | `~/.qwen/settings.json` (timeouts ≥1000) |

- **Seven report only the session id:** Antigravity, Crush (`PreToolUse` hook; state comes over herdr), Devin, Droid, Grok, Hermes (plugin plus `config.yaml` edit), Qoder. A hook that misses interrupts would pin the pane at `working` above the screen rules, so these deliberately send no state.
- Every entry runs `tuios agent-hook <harness> --integration <version>`.
- File handling:
  - Edits are atomic and keep the user's formatting and order.
  - On the first rewrite it writes `<file>.tuios.bak`, and never touches that copy again.
  - Symlinked dotfiles are preserved.
  - It refuses to write when the file is unparsable or the config directory is missing.
  - Files tuios owns carry a version marker. A file at that path that tuios did not write is refused, never overwritten.
- Related commands: `integration status|uninstall`, `doctor agents`, `--mcp`/`--mcp-write` (writes an MCP server entry for Claude, Codex, Gemini and opencode).
- On `SessionStart` (Claude and Codex), the hook injects into the agent's context: `You run in tuios pane 3a42ab8f in session "work". Run \`tuios --skill\` to learn how to drive tuios.`
- Claude event map:
  - `UserPromptSubmit` / `PreToolUse` → working
  - `PermissionRequest` → needs_input/approval, message `approve <tool>: <cmd>` (or `plan: <title>` for ExitPlanMode)
  - `PostToolUse` / `PostToolUseFailure` / **`PermissionDenied`** / `ElicitationResult` → working, but only if the pane is needs_input
  - `Notification permission_prompt` → approval; `elicitation_dialog` / `agent_needs_input` → question; `idle_prompt` → idle
  - `Stop` → done, with the first line of `last_assistant_message`
  - `StopFailure` → errored `stopped on <error_type>`
  - `SessionEnd` → none

**4i. Detection of 24 agents [V `internal/harness/manifests/*.toml`]**

- The manifests are aider, amp, antigravity, claude-code, cline, codex, copilot, crush, cursor-agent, devin, droid, gemini-cli, goose, grok, hermes, kilo, kimi, kiro, maki, omp, opencode, pi, qoder, qwen. They are TOML, derived from herdr's (Apache-2.0).
- **Method:**
  - Every 2 s the daemon finds the pane's foreground process group and reads `comm`, `argv` and `exe`.
  - Matchers: `comm`, `argv0`, `exe_glob` (component globs, `**`), and `argv_path`. `argv_path` reads only the first non-flag token, only for known interpreters, and only after `node_modules`/`site-packages`/an npm scope.
  - `[detect.require]` is mandatory for names under 5 characters (e.g. `pi` must have a node exe).
  - Wrappers (`sh -c`, `timeout`, `npx`, `uvx`, `mise exec`, `nix develop -c`) are walked within the same process group, up to 24 processes and 4 levels deep.
  - Fallback: `TUIOS_AGENT=<id>` in the process environment (identity `hint`).
  - A pane loses its agent when its shell regains the foreground (immediately) or after 6 ticks of some other program in the foreground.
  - `tuios explain-agent-detect` prints the verdict in plain words plus every ignored word.
- **Screen rules:** each manifest carries a `[screen]` block with all/any substring rules, case-folded, and an answers block. The Claude Code permission rule is `all=["Do you want"]`, `any=["1. Yes","❯ 1."]`, with answers approve=`yes`, approve_always=`yes, `, deny=`esc`.

#### DD2.5. Inbox, ask-human, mail, fan, worktree pull, status

- **Inbox / agents view (`Prefix+i`, `Prefix+o` = oldest) [V].** It is daemon-owned, so every client sees the same list. It is also available as `list-attention` and as `attention` events on `subscribe`.
  - **Item types:** approvals, questions (blocked prompts plus `ask-human`), mail to `human`, errored agents, Resume rows after a restart, unread finished turns. There is also snooze, undo and unread.
  - **Scope:** every session on every linked host.
  - **Answering inline:** a digit, `a` or `d` on a screen-read prompt (`peek-prompt` / `respond`). The daemon checks the screen before pressing anything.
  - **Answering held approvals:** `1`, `2` or `3` (allow once / always / deny) when the hook holds the request (`[agents.approvals] enabled=["claude-code","opencode",…]`, `hold_seconds=120`).
    - Only "whole-line" calls are held: Claude `Bash` (not with `dangerouslyDisableSandbox`), `Read`, `Glob`, `Grep` without a path, `WebFetch`, `WebSearch`. `Write` and `Edit` are never held.
    - Risk rules (`rm -rf` and similar, parsing `;`, `&&`, `|`, `$()` and `sh -c`) require a second press within 3 s. The daemon enforces a `risk_ack`.
  - `r` replies to mail. Answers carry `verified_human` only when they come from an attached client's nonce.
- **`ask-human`.** Example: `tuios ask-human 'Deploy?' -o yes -o no -o later`.
  - The question goes to the Inbox and pops up when you are looking at the asking pane, with a key-drop guard against stray typing.
  - The call blocks for 2 minutes by default, then returns `pending`; a later answer arrives as verified mail.
  - Questions are ≤160 bytes and answers ≤60 bytes. One question per pane; questions do not survive a restart.
  - An agent calling `answer-ask` gets `not_human`.
- **Agent mail.**
  - `send-agent-message` drops a message in a pane's inbox (or `human`'s) without touching the keyboard, with `--reply-to` threading and `--attach` / stash.
  - `read-agent-messages`; `wait-for agent-message`.
  - `ask-agent` types the question at an idle agent and returns what it prints. It refuses `needs_input` panes (`agent_blocked`) and cycles of asks, and is rate limited.
  - Message bodies are fenced as untrusted data. Up to 256 messages per session, in memory only.
  - Mail to an offline host is queued.
  - A `tuios-courier` PR (#312, HTTPS mail between machines) was merged, but I found no courier code in the current tree [U].
- **`tuios fan` [V CLI_REFERENCE].**
  - Syntax: `tuios fan N --agent 'claude,codex --model o5' 'PROMPT'`, or repeated `--prompt`.
  - It creates N git worktrees with branches `fan/<first words>`, `-2`, `-3`, under `$XDG_DATA_HOME/tuios/worktrees/<repo>/<branch>`, starts the agent in each, and types the prompt as one paste plus CR once the agent is ready.
  - Per-session prompt status: `pending | held` (not ready for 30 s → becomes an Inbox question) `| sent | not sent | stalled` (no working or needs_input within 5 s).
  - Other flags: `--env`, `--grants`, `--host build`, `--wait`.
  - **Compare: yes.** `fan compare` shows, per attempt, the agent, state, files and ± lines against the fan base (read through a temporary index), and the last check (`fan verify` result, or the last OSC 133 command's exit). Example row: `api-fan-retry  claude  done  4 files  +120 -31  go test ./... passed 14m ago`.
  - `fan verify -- CMD` runs the check in a no-grant `verify` window in each attempt.
  - `fan diff A B`.
  - `fan keep S [--merge --squash|--ff-only]` removes the siblings. A sibling with uncommitted work is left in place unless `--stash` or `--force` is given.
  - The TUI compare view (`w`) has `V` verify and `K` keep.
- **`tuios worktree`.**
  - `new <branch> [--base --agent --host --clone --detach]`, `ls` (with `MERGED into <ref>` detection that counts squash and rebase merges), `rm` (refuses dirty worktrees unless `--stash` or `--force`; never deletes the branch), `diff`.
  - **`worktree pull HOST:SESSION`** carries the commits as a git bundle into a new branch here, plus uncommitted and untracked work as a patch applied uncommitted. It refuses a detached HEAD, an existing branch, or a different origin, rolls back on mismatch, and never changes the remote side.
  - There are **no setup scripts, no `.env` copy, no port allocation, and no identity/profile handling** [V grep of docs and source; `checkpoint.go` sets `GIT_AUTHOR_NAME=tuios` only for checkpoint commits, and `ship` uses the user's ambient git identity].
  - `list-ports` only **discovers** listening ports per pane.
- **`tuios status`:** see §4b.

#### DD2.6. Remote

- **SSH server:** `tuios ssh --host --port 2222 --default-session --ephemeral`, using its own `~/.config/tuios/authorized_keys` and refusing to start without one unless `--no-auth`. Connections attach to the daemon [V].
- **Hosts federation:**
  - `tuios hosts add NAME ADDR [--repos-root]`: the daemon keeps an SSH link.
  - Addressing: `-s HOST:SESSION`, `-w HOST:SESSION:WIN`; `attach --host`; `new-window --host` (one pane's process on another machine); `new --global` (one session holding panes from several machines).
  - Agents: `list-agents --all-hosts`, Inbox across hosts, `fan`/`worktree`/`start-agent --host`, `hosts sync` to keep versions aligned, `cp` / `transfers` for files.
  - A per-machine `[hosts]` link policy (`list`, `mail`, `open`, `write`, …) controls what each machine may do; answering prompts is off by default.
  - Reconnect after laptop wake was added in v0.10.0 [V].
- **Tailscale:** `tuios hosts tailnet` lists tailnet machines through tailscaled's local API, configured by dns/name/ip, OS filters and globs, and handles Tailscale SSH check mode [V SESSIONS.md].
- **Phone:** `tuios pair --name phone` shows a QR code plus check code, writes `[hosts.phone]` (default `list,mail`), and adds a key restricted to `tuios stdio-proxy --as phone`. The app is Android private beta. Push notifications (`[notify]`, `register-push`) link to tuios-web [V].
- **tuios-web:** a separate binary for isolation, built on the author's `sip` library. Supports `--auto-tls`, `--password-file` / `--random-password`, user `tuios`, Host-header checks, read-only mode and touch support. It is a WebSocket terminal of the full TUI, not a separate PWA UI [V WEB.md].

#### DD2.7. Automation

- **Tape DSL [V docs/TAPE_SCRIPTING.md, examples/actions_and_waits.tape]:**
  ```
  DisableAnimations
  EnableTiling
  RenameWindow "editor"
  Run "echo ready-$((20+22))"
  WaitFor text "ready-42" 15s
  Split vertical
  WaitFor panes 2
  Action equalize_splits
  FocusWindow editor
  Expect focus "editor"
  Press "ctrl+b ?"
  Sleep 1s
  ```
  - `WaitFor`/`Expect` conditions: `text "re" [in "pane"]`, `pane`, `gone`, `focus`, `panes N`, `agent "state" [in]`, `workspace N`, `mode terminal|window`. `Source "file.tape"` includes another tape.
  - Commands: `tuios tape play|validate|exec` (exec runs headless against a live daemon session and exits non-zero with file:line:col), `tape list|show|delete`.
  - Recording is in-app only (`Ctrl+B T r` / `s`). **It records keybinding actions, not PTY bytes**, so it is a macro recorder, not asciinema.
  - `.tuios.tape` project autorun uses a direnv-style trust model: trust is keyed on realpath plus SHA-256, and the bytes that were reviewed are the bytes that run.
  - `tuios layout export` turns a layout into a tape.
- **JSON verb protocol:** see §2. Plus `tuios call`, `subscribe`, `wait-for`, `run` (run a line and wait), `capture-pane`, `screenshot` (PNG/SVG/ANSI/HTML/text), `mermaid`, the tmux shim (Claude Code agent teams open tuios panes), the MCP server (read-only by default, `--write` adds send/ask/respond/fan; enforced by the daemon through `restrict-connection`), and `--skill` (an embedded SKILL.md for agents).
- **Hooks: 11 events, not 10 [V docs/HOOKS.md].**
  - Run by the daemon (fire with nobody attached): `after-new-window`, `after-close-window`, `after-focus-change`, `after-workspace-switch`, `after-agent-state`, `after-command-finished` (OSC 133; `TUIOS_COMMAND` / `EXIT_CODE` / `DURATION_MS`, secrets masked).
  - Run by the client: `after-attach`, `after-detach`, `after-resize`, `after-layout-change`, `after-theme-change` (local client only).
  - Configured in a `[hooks]` table, run with `sh -c`, with `TUIOS_*` environment variables. `after-agent-state` respects `[notifications.agent]`: settle time, quiet hours, suppress-if-focused, and host focus.

#### DD2.8. What it lacks compared with ptyro, and its plans

Verified absences [V, by grepping docs and source]:
- **Projects:** there is no project entity. Only repository grouping of worktree sessions on the rail, "spaces", and `.tuios.tape` autorun per directory.
- **Worktree lifecycle safety:** good dirty-tree refusals, stash and no-branch-delete. But no setup scripts or post-create hooks, no `.env` / untracked-file copy, no port allocation (only `list-ports` discovery), and no per-worktree environment.
- **Identity profiles:** none. git uses the daemon's ambient environment; `ship` says "tuios sets no identity".
- **Review / PR:** **it has this**: `review` with line notes re-anchored within ±50 lines, `send-review` through the delivery queue, `checkpoint`, `ship commit|merge|push|pr|status` (through `gh`, with a hash-token confirm, an Inbox yes required for pane callers, and PR checks on the rail polled once a minute only while attached). Review on remote hosts is not supported.
- **Orchestration engine:** fan, compare, verify, keep, selectors (`group:fan/retry needs:you`), `start-agent`, `queue`, mail and ask-agent exist. There are **no declarative recipes or workflow DAGs, no task specs, no multi-repo change sets, and no persisted SQLite event store** (mail and questions are memory-only).
- **Byte-level recorder / replay:** absent. Tapes are action-level.
- **Exact live screen after a daemon restart:** absent; only history text above a divider.

There is no ROADMAP file. Plans appear in issues: #876 in-place image handover (live upgrade), #898 hook-heartbeat stall, #921 per-pane quiet mode, #681 spaces and moving panes between sessions, the phone app, Draw-kitty-as-blocks #586 [V].

#### DD2.9. Community sentiment and notable issues

- The audience is GitHub-native: 5.5k stars and 154 outside PRs, but **almost no HN or Reddit footprint**. The only HN story is "TUIOS is an experimental terminal tiling multiplexer", 2025-10-20, 5 points, 0 comments [V Algolia]. Reddit search failed [U]. Coverage comes from Terminal Trove, LinuxLinks, newreleases.io and SkillsLLM [S].
- Many of the most-discussed threads are fixes and features: vim copy mode (#9), panics (#19), clipboard fallback (#133), Nix packaging (#131), the PowerShell cwd bug (#491), the Claude Code input box overflow (#649), the £-key bug (#746), reconnect after sleep (#720).
- Recent requests come from users building **orchestration toolkits on top of tuios**:
  - #921: a "discussion room" of 2–8 Pi agents flooding the Inbox.
  - #898: a hung hooked agent shows `working` forever.
  - #896: cycle between hosts.

  People are already using it as the substrate for orchestration, which is ptyro's M7 territory [V].
- The author answers quickly and in depth (#898 response cites `agent_state.go:920`) [V].
- Risks: one maintainer and a very fast release pace, and protocol breaks (v0.8.0 required stopping the v0.7 daemon) [V/S].

#### DD2.10. Head-to-head

| Capability | tuios (2026-10-20) | ptyro | ptyro milestone |
|---|---|---|---|
| Own PTY daemon, detach/reattach | Yes [V] | Yes | done |
| Screen after daemon restart / reboot | Layout, cwd, ≤1k lines of history as text; new shells [V] | Exact restore (per brief) | done / partial (check SPEC) |
| Emulator | own Go `tuios.dev/vt` or libghostty-vt [V] | alacritty_terminal | done |
| Tiling WM (BSP, master-stack, scrolling, floating), 9 workspaces, palette, launcher | Yes [V] | Splits plus multi-session TUI | partial |
| Projects as an entity | No [V] | Yes | M3 done |
| Worktrees: setup scripts, .env copy, ports | No / No / discovery only [V] | Yes | M3 done |
| Identity profiles per workspace | No [V] | Yes | M3 done |
| Hook integrations | 19 harnesses, auto-install with backups [V] | Claude plus others | M4 |
| Process / screen detection | 24 manifests, wrapper walk, explain [V] | Planned / partial | M4 |
| OSC 7501 | rev 0.2, bare probe reply, host re-emit [V] | rev 0.4 with lists | M4.20 |
| Precedence | Strict rank, `applied:false`, blocker loan [V] | Peers, latest wins, confidence markers | M4 |
| Actionable inbox, approvals with risk rules | Yes [V] | Attention exists; actionable is R-UX-02 | M4 |
| ask-human, mail, ask-agent | Yes (memory-only) [V] | Planned | M7 |
| Review notes, checkpoints, ship PR | Yes [V] | Planned | M6 |
| fan, compare, verify, keep | Yes [V] | Planned | M7 |
| Recipes, workflow engine, `mcp` | MCP yes; recipes no [V] | Planned | M7 |
| Recorder / replay | Action tapes, headless exec [V] | Planned byte-level | M8 |
| Remote: SSH hosts, Tailscale, web, phone | Yes, plus a beta app [V] | Planned | M9 |
| ACP / Codex app-server | Yes (`start-agent --protocol`) [V] | Planned | M10 (M4.x spike) |
| Multi-repo change sets | No [V] | Planned | M11 |
| CLI `--json` parity | Broad `--json`, `list-verbs`, hint objects [V] | Core principle | ongoing |
| herdr protocol | Full socket emulation [V] | Planned `report-agent` | M4.20 |
| Windows | Native [V] | ? | — |

**Where tuios is ahead:** breadth of harness coverage, the inbox and approval UX, review/ship/checkpoints, fleets, multi-machine, MCP and skill, herdr compatibility, polish (themes, Omarchy, the WASM tour), distribution (brew, nixpkgs, AUR), velocity.

**Where ptyro is ahead or differentiated:**
- **Project and workspace lifecycle:** setup scripts, env, ports.
- **Identity profiles:** which `gh` / git / ssh account each workspace uses. There is no tuios equivalent, and `ship pr` uses ambient `gh`.
- **Exact screen restore.**
- **The peer-fusion status model**, which tuios's strict rank cannot express (see §11).
- **Durable event history:** a SQLite `session_events` timeline (tuios mail, questions and notes are memory or JSON only).
- Multi-repo change sets, a byte-level recorder, a declarative orchestration and recipe engine, and the Rust / alacritty performance and footprint story [S].

**Threats:**
1. tuios already occupies ptyro's M6–M10 roadmap story and is shipping roughly 80 commits a day.
2. It is becoming a *substrate*: herdr compatibility plus the JSON verbs plus MCP, with orchestration toolkits built on it (#921).
3. `tuios.dev/progstatus` and `tuios status` could become the de facto OSC 7501 reference implementation.
4. Omarchy default integration is a distribution channel.

**Mitigating:** bus factor 1, protocol churn, memory-only state, and nothing for teams or identities.

#### DD2.11. OSC 7501 interop mapping and conformance notes

| Spec (rev 0.4) | tuios internal | tuios emits to its host | ptyro `AgentStatus` (proposed) |
|---|---|---|---|
| `idle` | `idle` | `idle` (also a *seen* `done`) | `Idle` |
| `working` (+progress) | `working` | `working` (+progress) | `Working{activity: msg}`, progress |
| `blocked:kind=permission` | `needs_input` / `approval` | `blocked:kind=permission` | `Blocked{Permission}` |
| `blocked:kind=question` | `needs_input` / `question` | `blocked:kind=question` | `Blocked{Question}` |
| `blocked:kind=auth` | `needs_input` / `auth` | `blocked:kind=auth` | `Blocked{Auth}` |
| `blocked` (no or unknown kind) | `needs_input` / `""` | `blocked` | `Blocked{Unspecified}`. **ptyro's BlockKind needs an unspecified variant; do not guess from `msg` (the spec forbids it)** |
| `done` | `done`, unread | `done` (unseen only) | Spec meaning is "finished, unseen results". **ptyro's `Done` = orchestration complete is a semantic mismatch**. Treat inbound `done` as `Idle` + attention (or a "finished-unread" flag) unless inside a run step |
| `error` | `errored` | `error` | `Errored{reason: Unknown, detail: msg}` |
| `clear[:id]` | remove subtree | sent on detach / exit | remove records; revert to lower sources |
| — | `none` | no record | no status |
| — | `unknown` | no record | `Unknown` → emit nothing |
| — | — | — | `Starting` → emit nothing, or `working` with no progress |

Conformance and interop notes for ptyro:
1. **Probe:** tuios replies with the bare `ESC]7501;? ST` (rev 0.2 style); ptyro's longer rev 0.4 reply is fine for tuios, which prefix-matches (`host_program_status.go`). A program ptyro writes must accept both reply forms; the spec says to ignore unknown pairs.
2. **tuios inside ptyro:**
   - ptyro will receive **child-only records** with ids `<session>/<pane8>` and no root, up to 64 of them, batched every 250 ms.
   - ptyro must summarise across children by urgency (blocked > error > done > working > idle) and should show titles as "tuios ▸ pane name".
   - It must handle `state=clear:id=…` bursts plus RIS on exit.
   - Do not map `app` to harness identity or privileges; tuios deliberately does not.
3. **ptyro inside tuios** (when ptyro re-emits outward):
   - tuios shows records per pane, but a pane's displayed state is the most urgent record.
   - Use distinct ids per ptyro session, keep `app=ptyro` or the harness name, and send `clear` on detach.
   - tuios drops `done`/`error` when the user types in that pane. Re-asserting after a keystroke is wrong; let them go.
4. **Precedence difference:**
   - tuios ranks hook `report` (40) above `program` (38) strictly.
   - ptyro (SPEC §9.2, decided 2026-10-20) treats hooks and 7501 as peers, latest wins, because denying a Claude permission prompt fires no hook and only 7501 reports the return to idle.
   - tuios papers over this with its `PermissionDenied` / `PostToolUse` "→ working only if needs_input" hooks and with the screen-blocker loan. **Verify whether Claude Code's `PermissionDenied` hook actually fires on an interactive user denial** [U]. If it does, ptyro could use it as a corroborating signal; if not, ptyro's peer model is strictly more correct and worth stating in SPEC.
5. **Lifetime choices that differ from the bare spec:**
   - tuios removes `done`/`error` when the user types (an extension; the spec says "until replaced or cleared").
   - It drops `idle` at prompt and exit (allowed by MAY).
   - It scopes exit detection to the reporting **process group** and polls it every 2 s.
   - ptyro should decide each of these explicitly in SPEC.
6. **Discard strictness to match:**
   - An `app` over 32 bytes discards the report; an `app` with bad characters is treated as absent.
   - An invalid `id` discards the report and never lands on the root record.
   - Strict base64: correct padding when present, zero unused bits.
   - Non-UTF-8 or control characters in `msg`/`title` discard the report.
   - CAN and SUB cancel the sequence; a stray ESC ends it.
   - A repeated key: the last value wins, but any bad `id` value discards.
7. **OSC 9;4:** stop deriving state from it per pane after the first 7501 report, until RIS.
8. **Rate limits:** tuios limits external alerts to 30 s per pane, holding the newest state until the window ends, and host emission to 250 ms. ptyro's notification limiter should be comparable so mixed setups do not double-alert.
9. **`send-keys` input is not user input:** tuios does not let injected keys clear `done`. ptyro should keep the same rule for input from orchestration.
10. **Test against `tuios.dev/progstatus`:** it is a stdlib-only reference parser. ptyro's conformance fixtures (M4.20) could port `vt/conform_program_status_test.go` cases (MIT) for cross-implementation agreement.

#### DD2.12. Idea candidates for ptyro

Already captured, now confirmed by tuios: precedence table with `applied:false`; auth-blocked kind; actionable inbox; `fan` alias; tape recorder.

New or sharper ideas:
1. **Turn checkpoints (M6):** at each working→rest transition, snapshot the worktree as a commit under `refs/ptyro/checkpoints/<session>/<n>` through a temporary index, so HEAD, the index and stash are untouched. Add `checkpoint list|diff|restore`, where restore first takes a safety checkpoint. Keep 50 per session. About 100 ms on a 50k-file tree [V tuios numbers].
2. **Review notes that re-anchor (M6):** `FILE:LINE` notes keep the quoted line text and are re-found within ±50 lines, else marked `outdated`. `send-review` composes a numbered message ("Address each note, then say which you changed") delivered **only when the agent is at rest, never over a prompt**.
3. **Delivery queue primitive (M7):** `queue` / "type when idle", with a 5 s prompt-taken check (→ `stalled`) and `held` after 30 s on first-run dialogs (→ inbox question). This is the reliability core under fan and recipes.
4. **Ship with confirm tokens (M6):** push and PR return `confirm_required` plus a token hashing branch, commit, remote and PR, and proceed only with the current token. Pane-originated pushes need a human yes from the inbox. PR checks are polled once a minute only while a client is attached. ptyro's twist: run `gh` and git **under the workspace's identity profile**, which tuios cannot do.
5. **Hook-held approvals with a whole-line rule (M4):** hold Claude `PermissionRequest` only when one line shows the whole effect (Bash without sandbox override, Read, Grep, WebFetch). Add daemon-enforced risk rules with a `risk_ack` and a double press.
6. **Visible-blocker loan:** let a screen-detected `blocked` borrow over a stale high-confidence `working` (output since stamp, ≥2 s unrefreshed), and restore the displaced claim when the prompt disappears. This fits ptyro's peer model as a "low-confidence override with automatic return".
7. **Honest `unknown` and a `ready` bit:** the silence timer goes to `unknown`, not `idle`. Expose `needs_you`, `ready` and `evidence_age_ms` in `--json` so orchestrators never type into a menu.
8. **Hook-heartbeat stall (ship before tuios, #898):** a `stall_after` that counts only hook reports and ignores spinner output, giving a `working (stalled?)` marker plus an alert.
9. **Per-session quiet flag (#921):** suppress inbox items for orchestrated or background sessions while keeping state and events. Make it the default for M7 recipe workers.
10. **Manifest-driven detection and `explain`:** TOML manifests (`comm`/`argv0`/`exe_glob`/`argv_path`, `require` for short names, a bounded wrapper walk, a `PTYRO_AGENT=` hint). `ptyro status --explain` should print "words seen and not counted". tuios's manifests are MIT and herdr-Apache-2.0 TOML data, so the screen-rule strings could be reused with attribution. Check licence notices first.
11. **herdr socket shim:** export `HERDR_ENV` / `HERDR_SOCKET_PATH` / `HERDR_PANE_ID` and answer `pane.report_agent` / `release_agent` / `report_agent_session`, giving Crush, Kiro, Command Code and others zero-install status. tuios has proved the pattern. Keep `seq` monotonic per source.
12. **Outward 7501 re-emission done as tuios does:** probe once per client, `<session>/<pane>` ids, ≤64 records, a 250 ms batch, `clear` on detach, never pass through a pane's raw sequence. Then ptyro inside Rex or Ghostty, or inside tuios, shows per-session blocked state.
13. **`ptyro --skill` plus SessionStart context injection:** an embedded agent guide matching the binary, and the hook prints "You run in ptyro session X (project Y, workspace Z). Run `ptyro --skill`." This is cheap and high leverage for `ptyro mcp` (M7).
14. **MCP held to grants by the daemon:** `restrict-connection` on every MCP call (read-only by default, scope = own session or fan group), with the pane identified from the kernel's pid record. Add pane grants (`read|write|fan|respond|admin`) for agent-spawned helpers.
15. **Error `hint` objects:** add `did_you_mean`, `command`, `accepted` and `available` to every `--json` error, so agents self-correct without a second call.
16. **Resume offers after a daemon restart:** persist the conversation id per session and put a "Resume" item in the inbox (`ask|auto|off`). This complements ptyro's exact restore when processes die (reboot).
17. **`fan compare` with a "last check" column from OSC 133:** reuse the last finished command's exit status as a free verification signal. `fan verify` runs a user-supplied command (never read from the repo) in no-grant windows.
18. **`worktree pull` across hosts (M9):** commits as a git bundle plus uncommitted work as a patch into a new local worktree, refusing on origin mismatch. It also fits M11 change sets.
19. **Port observation alongside allocation:** ptyro allocates ports; add `/proc/net/tcp` discovery to show "allocated 5173, actually listening on 5174" drift per workspace.
20. **tmux shim:** a `tmux` binary shim so Claude Code agent teams open ptyro sessions instead of tmux panes.

**Interop to-dos:**
- (a) Fixtures for tuios-in-ptyro and ptyro-in-tuios 7501 round trips.
- (b) A SPEC §9 table mapping tuios and spec state names, from §11.
- (c) Consider using `tuios.dev/progstatus` test vectors.
- (d) Document the semantics of ptyro's `Done` against the spec's `done`.
- (e) Update the RESEARCH.md A3 entry: 11 hooks; the ladder includes `transcript` and detect ranks above stall in code; review, ship and checkpoints exist; there are no projects, profiles or setup scripts.

**DD2 sources:**
- Repo clone: README, `docs/AGENT_STATE.md`, `PROGRAM_STATUS.md`, `SESSIONS.md`, `CLI_REFERENCE.md`, `protocol.md`, `HOOKS.md`, `WEB.md`, `TAPE_*.md`, `PROJECT_TAPES.md`, `ghostty-vt.md`, release notes v0.8.0, v0.10.0, v0.10.1.
- Code: `progstatus/progstatus.go`, `internal/app/host_program_status.go`, `internal/session/agent_source.go`, `internal/harness/manifests/`.
- GitHub API: repo metadata, releases, contributors, issues #898 and #921, user profile.
- [Program Status Protocol spec rev 0.4](https://www.superlogical.com/rex/docs/build/program-status)
- [HN Algolia search for "tuios"](https://hn.algolia.com/api/v1/search?query=tuios)
- [LinuxLinks listing](https://www.linuxlinks.com/tuios-terminal-ui-operating-system)
- [newreleases.io v0.8.0](https://newreleases.io/project/github/Gaurav-Gosain/tuios/release/v0.8.0)
---

## Part II — Orchestration deep dive

The owner expects multi-agent orchestration to become a compelling **daily** feature, so this part
goes beyond the product catalogue (segment F). It asks four questions:

1. how practitioners actually run many agents (§II.1);
2. what research says about picking and verifying among parallel attempts (§II.2);
3. which primitives mature workflow engines offer that ptyro's recipe engine should take (§II.3);
4. how agents talk to each other and to the human (§II.4).

It ends with a synthesis: what orchestration in ptyro should be (§II.5).

### II.0 The strategic picture in five sentences

1. **Claude Code now ships, natively and for Claude only, much of what M7 planned.** That covers background supervisor sessions, agent teams with a mailbox and task list, cross-session messaging over sockets, and resumable JS workflows. ptyro should **consume** these surfaces, not duplicate them.
2. **The binding constraint is human review attention, not agent count.** Orchestration that adds agents without compressing review makes things worse (Willison, Ronacher, Osmani; Every reports about 80% of effort goes on plan and review).
3. **Parallel *writers* on shared code remain the weak spot. Parallel *independent attempts*, *readers*, *reviewers* and *verifiers* work.** Cognition (2026) says "writes stay single-threaded". MAST names verification as one of its three failure classes. Google measured multi-agent setups at **−70%** on sequential planning tasks.
4. **Verification is the multiplier.** Unattended agents are only as good as the check that gates "done". About 1 in 5 SWE-bench patches that pass tests are semantically wrong (SWE-ABS).
5. **ptyro's defensible orchestration ground:**
   - cross-vendor runs (Claude + Codex + Gemini + OpenCode in one run);
   - an owned PTY that survives reboot;
   - **daemon-enforced deterministic policy**;
   - **mid-run human gates** (Claude workflows have none);
   - verified-done;
   - compare-and-ship;
   - multi-repo change sets.

### II.1 How practitioners actually run multi-agent coding (2025–2026)

#### Sources (annotated)

| # | Source | Date | Why it matters |
|---|---|---|---|
| 1 | Anthropic, *Building effective agents* — anthropic.com/engineering/building-effective-agents | 2024-12-19 [V] | Canonical patterns: chaining, routing, sectioning/voting, orchestrator-workers, evaluator-optimizer. "Simplest solution possible." |
| 2 | Anthropic, *How we built our multi-agent research system* — /engineering/multi-agent-research-system | 2025-06-13 [V] | About **15×** tokens vs chat; "most coding tasks involve fewer truly parallelizable tasks than research"; vague delegation leads to duplicate work and over-spawning; pass artifacts by reference. |
| 3 | Cognition, *Don't Build Multi-Agents* — cognition.com/blog/dont-build-multi-agents | 2025-06-12 [V] | "Actions carry implicit decisions, and conflicting decisions carry bad results." |
| 4 | Cognition, *Multi-Agents: What's Actually Working* — /blog/multi-agents-working | 2026-04-22 [V] | **Works:** review loop (Devin Review catches about 2 bugs per PR, about 58% of them severe), "smart friend", cross-frontier router, manager-child over MCP, map-reduce-and-manage, read-only subagents. **Fails:** fragmented decisions, review looping, missing upward communication, false shared state, over-prescriptive managers. |
| 5 | Anthropic, *Effective harnesses for long-running agents* — /engineering/effective-harnesses-for-long-running-agents | 2025-11-26 [V] | Initializer agent plus incremental worker. A feature-list JSON where agents may only flip `passes`. "It is unacceptable to remove or edit tests." `claude-progress.txt`, `init.sh`. |
| 6 | Anthropic (Rajasekaran), *Harness design for long-running application development* — /engineering/harness-design-long-running-apps | 2026-03-24 [V] | Planner, generator, evaluator. **Sprint contracts** negotiated in files before coding. Playwright evaluator with hard per-criterion thresholds. Solo run: 20 min, $9, broken. Harness: 6 h, $200. |
| 7 | Anthropic (Carlini), *Building a C compiler with a team of parallel Claudes* — /engineering/building-c-compiler | 2026-02-05 [V] | 16 agents, no orchestrator, lock files in `current_tasks/` synced via git. "Merge conflicts are frequent." All 16 hit the same bug until **GCC was used as an oracle** to partition the work. About 2,000 sessions, about $20k. |
| 8 | Claude Code docs: best practices, agent teams, agent view, cross-session messaging, workflows — code.claude.com/docs | live, 2026-10 [V] | Verify-or-don't-ship; writer/reviewer; the warning that **reviewers always find something**, which drives over-engineering. Teams: "start with 3–5"; the lead stops early or implements itself. Workflows: **no mid-run user input**. |
| 9 | Simon Willison, *Embracing the parallel coding agent lifestyle* — simonwillison.net/2025/Oct/5/parallel-coding-agents/ | 2025-10-05 [V] | Tasks that parallelize: research/POC, explanations, small maintenance, well-specified work, **"send out a scout"**. Later posts: six repos upgraded from one prompt, "parallel agent psychosis" (lost work), StrongDM holdout scenarios, red/green TDD. |
| 10 | Cursor (Wilson Lin), *Scaling long-running autonomous coding* — cursor.com/blog/scaling-agents | 2026-01-14 [V] | Equal-status agents with locks were **brittle**. Flat teams were risk-averse and churned. **Planners + workers + a judge** with fresh cycles worked. A dedicated integrator role *hurt*. "The prompts matter more." |
| 11 | Geoffrey Huntley, *Ralph Wiggum as a "software engineer"* — ghuntley.com/ralph/ | 2025-07-14 [V] | `while :; do cat PROMPT.md \| claude; done`: fresh context per loop, "only one thing" per loop, `fix_plan.md`, backpressure. "No way in heck" for existing codebases. |
| 12 | Steve Yegge, *Revenge of the junior developer* + Gas Town / beads READMEs | 2025-03-22 / 2026 [V] | Waves of agents, then clusters, then fleets; "a lot of agent babysitting". Witness/Deacon stuck detection; Refinery **bisecting** merge queue; "Kubernetes asks 'Is it running?' Gas Town asks 'Is it done?'" [S]. |
| 13 | Mitchell Hashimoto, *My AI Adoption Journey* — mitchellh.com/writing/my-ai-adoption-journey | 2026-02-05 [V] | Runs **1** background agent. **Turned notifications off** so he decides when to check in. "Harness engineering": fix each mistake so it never recurs. |
| 14 | Armin Ronacher, *Agentic Coding Recommendations* — lucumr.pocoo.org/2025/6/12/agentic-coding/ | 2025-06-12 [V] | Shared state (fs, DB, Redis) is the obstacle to parallelism. Pidfile-wrapped `make dev`. "Crashes are tolerable; hangs are problematic." |
| 15 | Peter Steinberger, *Just Talk To It* — steipete.me/posts/just-talk-to-it | ~2025-10 [V] | Runs 3–8 agents **in one folder**, each committing only the files it edited. Tried worktrees and reverted (one dev server, OAuth callbacks). Prefers CLIs over MCP for "zero context tax". Of orchestrators: "**None stick**". |
| 16 | Jesse Vincent, *Superpowers* — blog.fsck.com/2025/10/09/superpowers/ | 2025-10-09 [V] | Brainstorm, plan, worktree, subagent per task with review between tasks, RED/GREEN TDD. |
| 17 | Every (Shipper/Klaassen), *Compound Engineering* — every.to/chain-of-thought/compound-engineering-how-every-codes-with-agents | 2025-12-11 [V] | Plan, Work, Review, Compound. 12 parallel reviewer subagents synthesized for the human. Lessons stored as in-repo prompts. |
| 18 | Harper Reed, *My LLM codegen workflow atm* — harper.blog/2025/02/16/… | 2025-02-16 [V] | spec.md, prompt_plan.md, todo.md. "Largely a solo endeavor", and he wants team tooling. |
| 19 | Addy Osmani, *Conductors to Orchestrators* — addyo.substack.com | 2025-11-01 [V] | Conductor (sync, one agent) vs orchestrator (async, many, PR-based). The review loop "is going to need work". |
| 20 | Gergely Orosz, *Programming by kicking off parallel AI agents* — blog.pragmaticengineer.com | 2025-10-30 [V] | Only senior+ engineers reported success. 2–5 workstreams. Cognitive load and lost flow. |

**How many agents people really run:**

| Who | Agents |
|---|---|
| Hashimoto | 1 |
| Willison | "several" |
| Orosz's seniors | 2–5 |
| Steinberger | 3–8 |
| Claude teams docs | start with 3–5 |
| Gas Town | "4–10 agents become chaotic" without coordination |
| Research demos (Carlini; Cursor) | 16; hundreds |

**The daily sweet spot is 2–6.**

#### Pattern catalogue

Each pattern lists who uses it, the evidence, how it fails, and how ptyro supports it. *Existing plan* means the pattern is already covered by a milestone task; *New* means it needs new work.

| ID | Pattern | Who / evidence | Fails when | ptyro support |
|---|---|---|---|---|
| P1 | **Side-task parallelism**: one main change plus low-attention side tasks | Willison, Hashimoto, Steinberger, Orosz [V] | Side tasks need review too; work gets lost across checkouts | Core M2–M4 (status, next-attention, inbox). **New:** foreground/background priority with batched notifications; `ptyro where` work ledger. |
| P2 | **Scout run**: throwaway attempt to learn the terrain | Bleecher Snyder via Willison [V] | Scout output merged by accident; cost | **New:** `ptyro scout` = disposable worktree, read-only intent, findings file, auto-discard, findings fed into the real run. |
| P3 | **Writer / fresh-context reviewer** | Claude docs, Cognition, Every, Superpowers [V]. The strongest-evidenced pattern | Reviewer always finds something (over-engineering); review looping; reviewer lacks intent | `implement-and-review` (M7.8). **New:** a **cross-vendor reviewer by default**, a round cap, a severity filter, and a writer triage step; the reviewer gets *diff + task + test results*, not the transcript. |
| P4 | **Planner → generator → evaluator with sprint contracts** | Anthropic Labs 2026-03; Cursor [V] | Lenient evaluator; over-scoping; 20× cost | Recipes + gates. **New:** a single-repo `contract` step (generalises M11.5) plus per-criterion hard-threshold checks. |
| P5 | **Initializer + incremental worker, immutable feature list, progress file** | Anthropic 2025-11 [V] | Agents edit the list or claim passes without proof | **New:** seeded `features.json`/progress conventions; **daemon-enforced write fence** (only `passes` may change; reuses M11.4 PreToolUse fences); a baseline check at session start. |
| P6 | **Ralph loop**: fresh context per iteration, one item, backpressure | Huntley, Carlini, many imitators [V] | Reimplementation; placeholders; waking up to a broken tree; time blindness | **New, high value:** a `loop` step with a fresh session per iteration, stop on check-pass / N rounds without progress / budget, **a checkpoint per iteration**, and a morning digest. Owned PTYs plus reboot survival make it robust in a way a bash loop isn't. |
| P7 | **Spec → plan → todo → execute (interview first)** | Reed, Superpowers, Claude docs, Willison [V] | Spec drift | Recipe chain: interview session → human gate on SPEC → **fresh** implementer. The value is the fresh-session handoff plus gating the spec. |
| P8 | **Best-of-N / voting** | Building effective agents; Cursor `/best-of-n`; Codex `--attempts` | N diffs to review defeats the purpose | M7.2/7.3. **The compare view must do the triage** (§II.2). Read-only fan-out (hypotheses, plans, research) is cheaper and better evidenced. |
| P9 | **Swarm with task claiming** | Carlini, Claude teams, beads, Cursor v1 [V]. Mixed results | Brittle locks; everyone piles onto one bug | **New:** a daemon-owned task board where the daemon is the lock authority. Leases tied to session liveness; `ready` = dependencies satisfied; beads import. |
| P10 | **Oracle partitioning** | Carlini (GCC as oracle) [V] | — | Generalises to **partition by ownership**: disjoint owned paths enforced by write fences. |
| P11 | **Same-folder agents, atomic per-agent commits** | Steinberger [V] | Mixed commits; blast radius | Support it explicitly (worktrees are not mandatory). Track per-session touched files from PreToolUse Edit/Write; "commit only my files" helper. |
| P12 | **Manager + read-only children** (map-reduce-and-manage) | Cognition, Anthropic research, Claude subagents [V]. Strong | Children don't report upward; false shared state | M7.6. **New policy:** `lead_may_write = false` default; `worker_done` must carry `discoveries`; workers inherit the lead's brief and contract. |
| P13 | **Smart friend / model per role** | Cognition, Cursor [V] | Weak model fails to escalate | `agent`/`profile`/`model` per step; MCP `consult(agent, question, include_context)`. |
| P14 | **Harness / compound engineering**: codify every mistake | Hashimoto, Every, Huntley ("signs") [V] | — | ptyro detects and offers: after repeated corrections on a theme, a gated "promote to AGENTS.md / a hook" prompt. |
| P15 | **Test-first agents** (red/green; first run the tests) | Superpowers, Willison, Claude docs [V] | Implementer edits the tests | Test-writer and implementer as separate sessions; a fence stops the implementer from editing test files. |
| P16 | **Holdout scenarios** (hidden acceptance tests) | StrongDM via Willison [V] | — | **New:** a `check` running scenarios stored *outside* the worktree in ptyro data; only pass/fail plus a redacted summary goes back. |
| P17 | **End-of-day / overnight batch** | Hashimoto, Huntley, Carlini [V] | Silent overnight breakage | An `overnight` recipe with budgets plus a morning digest. Reboot survival beats Claude agent view here. |
| P18 | **Same prompt across many repos** | Willison (six repos) [V] | — | `ptyro fanout --projects a,b,c --agent codex`: independent repos, no contract. A lighter sibling of change sets that should ship before M11. |
| P19 | **Merge queue with bisect** | Gas Town Refinery [V README] | — | A single-repo merge train in M6/M7, reusing the M11.9 machinery. |
| P20 | **Stuck detection → nudge / handoff / escalate** | Gas Town Witness/Deacon, Claude workflows, Carlini [V] | — | Derived states `stalled`, `spinning` (same command repeated), `zombie`; actions nudge, handoff (fresh session plus brief), escalate. |

#### Failure modes the daemon can enforce against

| # | Failure | Evidence | Daemon-side mitigation |
|---|---|---|---|
| F1 | **Review bottleneck** | Willison, Ronacher, Osmani, Every [V] | A ranked **review queue** with an evidence bundle; a `max_unreviewed` policy that throttles spawns; digest notifications. |
| F2 | **Premature / false "done"**; the lead stops early | Anthropic harness, Claude teams, Cursor [V] | Separate **claimed-done** from **verified-done**: `done` only after the run's check passes in that worktree, with evidence stored. A Stop-hook gate returns exit 2 with the failures. |
| F3 | **Test tampering** (deleting tests, `assert true`, editing the feature list) | Anthropic, StrongDM [V] | PreToolUse write fences on protected globs; a diff lint flags deleted or weakened tests in compare and review; holdout checks. |
| F4 | **Merge conflicts between parallel writers** | Carlini, Claude teams, Cognition [V] | **Overlap radar**: a live file-overlap matrix plus `git merge-tree` previews; warn or block a second writer on an owned path. |
| F5 | **Conflicting implicit decisions** | Cognition [V] | A shared brief/contract injected into every worker; contract-revision propagation (M11.5 generalised); single-writer default for leads. |
| F6 | **Duplicate work** | Carlini, Anthropic, Huntley [V] | Daemon-owned claims with leases; a "same failing test across N sessions" detector. |
| F7 | **Runaway cost and quota** | 15× tokens; about $20k; $1k/engineer/day [V] | Budgets per session, run and day (turns, wall clock, tokens/$ where exposed); a pause/hibernate gate when exceeded; a max-concurrency scheduler per profile; a pre-run estimate. |
| F8 | **Time blindness / spinning / hangs** | Carlini, Ronacher [V] | Stalled and spinning detection from PreToolUse activity and output/diff deltas; per-step and per-tool timeouts; surface "45 min in `cargo test`". |
| F9 | **Context rot, correction loops** | Claude docs, Huntley [V] | Count corrections per session; suggest a **handoff** (fresh session plus a generated progress brief). |
| F10 | **Destructive actions** (`pkill -9 bash`, push to main, force-push) | Carlini, Claude agent view [V] | Deny destructive git (already in §15.4); process-group guard; container option. |
| F11 | **Shared runtime collisions** (ports, dev servers, DBs, OAuth) | Ronacher, Steinberger [V] | Port allocation (§10.7) plus per-worktree env namespacing (DB name, Redis prefix); one-dev-server guard; pair mode. |
| F12 | **Lost work across environments** | Willison [V] | `ptyro where`; refuse removal that would lose work; automatic checkpoint refs. |
| F13 | **Lock brittleness** | Cursor [V] | Leases owned by the daemon, released on process exit or staleness. |
| F14 | **Flat-team churn** | Cursor [V] | Recipes encode hierarchy (planner/worker/judge); a judge step decides continue or stop. |
| F15 | **No upward communication / false shared state** | Cognition [V] | Typed `worker_done{summary, discoveries, evidence}`; required heartbeat; automatic status digests to the lead. |
| F16 | **Reviewer-driven over-engineering** | Claude docs, Cognition [V] | Round cap; severity filter; a diff-growth alarm across rounds. |
| F17 | **Message loops and bursts** | Claude cross-session throttles [V] | Per-sender rate limits, identical-message dedupe, a loop detector that opens a gate. |
| F18 | **Permission-prompt floods** | Claude teams [V] | Inbox groups blocked items by kind with one-key approve; per-run policy pre-approvals. |
| F19 | **The lead does the work itself** | Claude teams [V] | `lead_may_write = false` fence. |
| F20 | **Orphans** (sessions, panes, worktrees) | Claude teams [V] | The daemon owns the PTYs; run teardown removes clean worktrees and keeps dirty ones. |

**What practitioners ask for:**
- runtime isolation per agent;
- faster, evidence-first review;
- knowing where all the work is;
- attention on their own terms (digests, not pings);
- **context-cheap control surfaces** (lean MCP, agent-friendly CLI);
- team workflows;
- stuck/health dashboards and merge queues;
- verification harness tooling;
- cost visibility;
- orchestrators that actually stick.

On the last point: **ptyro must win as a daily multiplexer first, with orchestration as an opt-in layer.**

### II.2 What the research says about selecting and verifying parallel attempts

#### Annotated bibliography (abridged; full tags kept)

**Multi-agent software-engineering systems and failure analyses**
- **MAST — *Why Do Multi-Agent LLM Systems Fail?*** (Cemri et al., Berkeley, 2025-03) arxiv.org/abs/2503.13657 [V]: 14 failure modes in 3 classes (system design, inter-agent misalignment, **task verification**), drawn from 1,600+ traces across 7 frameworks. Gains over a single agent are often minimal.
- ***Towards a Science of Scaling Agent Systems*** (Google + academia, 2025-12) arxiv.org/abs/2512.08296 [V]: 260 configurations. Multi-agent vs single agent ranges from **+80.8%** on decomposable tasks to **−70.0%** on sequential planning. Gains shrink as the single-agent baseline gets stronger. Architectures without centralised verification amplify errors.
- **Anthropic multi-agent research system** (2025-06) [V]: **+90.2%** over single-agent Opus 4 on research; about **15×** chat tokens; token usage explains 80% of BrowseComp variance; a single rubric judge beat a panel of judges.
- **Cognition 2025/2026** [V]: writes single-threaded; reviewers and advisors work.
- **AgentCoder** (2023-12) arxiv.org/abs/2312.13010 [V]: programmer + test designer + test executor; 96.3% HumanEval with fewer tokens than role-play systems.
- **MASAI** (2024-06) arxiv.org/abs/2406.11638 [V]: modular sub-agents; 28.33% SWE-bench Lite.
- **SWE-Search** (2024-10) arxiv.org/abs/2410.20285 [V]: MCTS plus a value agent plus a discriminator debate; +23% relative.
- **mini-swe-agent** (github.com/SWE-agent/mini-swe-agent) [V]: about 100 lines of bash-only Python, ">74%" Verified per its README. **Scaffold complexity is not where the gains are.**
- MetaGPT, ChatDev, MapCoder, SWE-agent, OpenHands, AutoCodeRover, CodeR [U]: function-level role-play gains mostly vanish at repository scale.

**Best-of-N and scaling compute at inference time**
- **Large Language Monkeys** (Stanford, 2024-07) arxiv.org/abs/2407.21787 [V]: SWE-bench Lite **15.9% → 56%** from 1 to 250 samples. Coverage grows log-linearly. Without verifiers, **voting and reward models plateau** after a few hundred samples.
- **CodeMonkeys** (Stanford, 2025-01) arxiv.org/abs/2501.14723 [V]:
  - 57.4% Verified for about $2,300; selection is only **5.8%** of cost.
  - Oracle coverage is 69.8%, so **selection recovers only about half the gap**.
  - **Barrel of Monkeys**, an ensemble of the top-4 systems plus CodeMonkeys, reaches **66.2%** vs 62.8% for the best member, with oracle coverage of 80.8%.
- **Trae Agent** (ByteDance, 2025) arxiv.org/abs/2507.23370 + blog [V]:
  - Candidates from **3 different models**: about 62% for the best single model → **70.6%** for the ensemble.
  - Pipeline: regression filter → Tree-sitter dedupe and clustering → selector voting.
  - **The LLM selector peaked at 5–6 candidates; at 8–9 it did worse than at 3–4.**
- **Claude 4 launch** (2025-05) [V]: "parallel test-time compute" = sample N, reject candidates that break visible regression tests, pick with an internal scorer. Opus 4 79.4%, Sonnet 4 80.2%.
- **Agentless** (2024-07) arxiv.org/abs/2407.01489 [V2]: about 40 candidates; **reproduction tests kept only if they fail on the original code**; regression filter plus majority vote. The template most later systems copy.
- **SWE-Gym** [V2]: trained verifier Best@k from 20.6% (k=1) to 32.0% (k=16), log-linear.
- **CWM** (Meta, 2025-09) [V2]: plateaus around k=16; majority vote 58.4% vs pass@40 of 80.4%.
- **DEI** (Salesforce, 2024-08) arxiv.org/abs/2408.07060 [V]: choosing among *different* frameworks gives **+25% relative**.
- **SWE-Replay** (2026-01) arxiv.org/abs/2601.22129 [V]: branching attempts from saved trajectories costs up to **17.4% less** at equal or better resolve rates.
- **S\*** (Berkeley, 2025-02) arxiv.org/abs/2502.14382 [V]: selection by **generating inputs that make candidates behave differently** (differential testing).
- **Kimi-Dev** (2025-09) [V2]: patch-writer / test-writer self-play.

**Verification and judging**
- **PatchDiff** (ICSE 2026) arxiv.org/abs/2503.15223 [V2]: 7.8% of "correct" patches fail the full suite; **29.6%** of plausible patches diverge from the reference.
- **SWE-ABS** (ICML 2026) arxiv.org/abs/2603.00520 [V]: about **1 in 5** passing patches from the top-30 agents are semantically wrong. The top agent drops from 78.80% to 62.20%.
- **TestPrune** (IBM, 2025-10) arxiv.org/abs/2510.18270 [V]: trimming the regression suite to a relevant subset gives **+8–13% relative resolution** for $0.02–0.05 per instance.
- **SWE-RM** (2025-12) arxiv.org/abs/2512.21919 [V]: execution-free reward model, +7–10 points. **Calibration ≠ selection accuracy.**
- **SWE-Lego** (2026-01) [V2]: generative (yes/no) verifiers beat regression-head verifiers.
- **Human-in-the-loop patch judging** (Google, 2025-11) arxiv.org/abs/2511.10865 [V2]: rubric-based LLM judges reach κ 0.75 on clear cases but **0.57 overall (precision 0.65)**. High recall, middling precision.

**Benchmarks and human factors**
- **SWE-Bench Pro** (Scale, 2025-09) [V2]: <25% Pass@1 on hours-to-days tasks.
- **SWE-Lancer** (OpenAI, 2025-02) [V]: $1M of Upwork tasks. Its **manager tasks are a selection benchmark**, which is directly relevant to the compare view.
- **METR time horizons** (2025-03) [V]: doubling about every 7 months, faster in Time Horizon 1.1 [V2].
- **Terminal-Bench** (tbench.ai) [V]: now at 4.0.
- **METR developer RCT** (2025-07) [V]: experienced developers were **19% slower** with AI but believed they were 20% faster. **METR's 2026 redesign** [V2]: time-per-task tracking breaks down for developers running several agents at once. Even METR can't measure multi-agent productivity, so ptyro can capture that data locally.

#### Findings

| # | Finding | Evidence |
|---|---|---|
| F1 | **Sampling raises the ceiling; selection is where results are lost.** Selection recovers only about half the random-to-oracle gap. ptyro's value is less "run N agents" than **"help pick the winner well".** | Monkeys, CodeMonkeys [V] |
| F2 | **Returns flatten fast. N ≈ 3–6 is the useful range** for interactive work, and lower when the selector is a human. | Trae [V], CWM/SWE-Gym [V2] |
| F3 | **Different models beat repeated runs of one model.** No controlled equal-cost study exists, so the size of the effect is uncertain. | Trae, Barrel, DEI [V] |
| F4 | **Ranking order: execution first, then agreement, then a judge.** Regression filter → reproduction test (must fail on base) → rank by tests → cluster equivalent patches (cluster size as votes) → LLM selector on a short list only. | Agentless, Claude 4, Trae, CodeMonkeys |
| F5 | **Passing tests is not proof.** Differential testing *between candidates* is the cheapest way to surface disagreement. | SWE-ABS, PatchDiff, S* |
| F6 | **LLM judges have high recall and middling precision.** Use one judge with a rubric as an **advisory column**, never as the final gate. | Anthropic, Shi et al., SWE-RM |
| F7 | **Parallel writers on one codebase fail; independent attempts and read-only helpers work.** | Cognition, MAST, Google |
| F8 | **Reviewers pay off with a clean context** (diff + task + tests, not the author's transcript). | Devin Review, AgentCoder |
| F9 | **Cost multiplies predictably and goes mostly to generation.** Branching from shared exploration saves up to 17%. | Anthropic, CodeMonkeys, SWE-Replay |
| F10 | **People misjudge their speedup, and human review is the bottleneck.** Show **differences between candidates**, not N full diffs. | METR |

#### What the fanout compare view should compute

All of these are deterministic; judging is delegated to an agent.

| Column | How | Why |
|---|---|---|
| **Regression tests** (newly broken) | Run the test command (or a relevant subset) before and after; show *newly broken* tests | First filter in every top system |
| **Reproduction test** (fails on base, passes after) | `repro_cmd` from the user, a recipe step, or an agent; ptyro checks it **fails on the base commit** | Strongest single signal |
| **Cross-test matrix** | Run every candidate's own tests against every other candidate (N×N) | Cheap CodeMonkeys-style voting |
| **Equivalence cluster** | Tree-sitter-normalised diffs, grouped; cluster size = votes | Agreement signal; less to read |
| **Behavioural divergence** | Run differing or generated tests on pairs of passing candidates; flag where outputs differ | Catches "passes but wrong" |
| **Diff size and spread** | Lines, files, files outside the implicated area, new dependencies | Reviewability; scope creep |
| **Judge verdict** (advisory) | A *different* model reads task + diff + tests and returns rubric score 0–1, pass/fail and rationale | Recall-oriented flag |
| **Cost and time** | Tokens/$ where reported, wall clock, turns | Cost-aware choice |
| **Lint / types / build** | The project's own commands | Cheap disqualifier |

- **Auto-rank lexicographically, not by a weighted sum:** builds → no new failures → reproduction passes → cross-test score → cluster size → judge → smaller diff.
- Show *why* each candidate is where it is.
- If none pass, keep all of them.
- Flag ties and divergence among the top candidates for the human.
- **The default view is the top cluster's representative plus how the runner-up differs.**
- **Defaults:** N = 3, warn above 6; **mixed models by default** when profiles allow; label the mode so users learn what pays off.
- **Warn against parallelism** when the task is sequential or exploratory, when there is no automated signal (suggest writing a reproduction step first), or when writers would touch the same files.

### II.3 Workflow engines: primitives to take for the recipe and run engine

**Framing.** None of these engines hosts non-deterministic interactive CLIs in PTYs. All of them assume an in-process model loop they can journal or replay. **ptyro cannot replay a Claude Code TUI.** What it controls are the **boundaries**: prompt in; hook-derived status out; git snapshot; structured verdict file; a check command's exit code.

| Engine | Primitive to take | How it looks in ptyro | Don't copy |
|---|---|---|---|
| **LangGraph** (checkpointer, `interrupt`/`Command(resume=)`, time-travel replay vs fork) [V] | **Fork from a step**; typed gate resume value | `ptyro run fork r42 --from review [--set task=…]`; `ptyro gate resolve r42/gate --value '{"choice":"iterate"}'` | Replay that silently re-executes nodes (re-running an agent costs money and gives a different result). **Only fork, never replay.** |
| **CrewAI Flows** (`@human_feedback` with `emit=` outcomes, revision loop `or_()`) [V] | Revision loop; append-only feedback history | Gate answers stored as `run_message`; `{{gates.review.history}}` in prompts | LLM-interpreted routing; a hierarchical manager LLM. Gate options are explicit buttons. |
| **Microsoft Agent Framework 1.0** (GA 2026-04-03; superstep checkpoints incl. pending HITL; rehydration needs the same topology) [V] | **Recipe identity pinning**; HITL answers as checkpointed events; graph export | `run.recipe_src` + `recipe_sha`; `ptyro run resume r42 [--upgrade]`; `ptyro recipe graph --format mermaid\|dot` | GroupChat/Magentic in the engine; pickled state. |
| **OpenAI Agents SDK** (guardrail tripwires, `needs_approval`, `RunState`, `reject(rejection_message=)`) [V] | **Guards / tripwires** running alongside a step; rejection text goes back into the session | `guard = { max_diff_lines = 1500, deny_paths = ["migrations/**"], deny_cmds = ["git push"], on_trip = "pause" }` (`deny_cmds` enforced via PreToolUse) | In-process handoffs; ptyro's "handoff" is a new session with templated outputs. |
| **Claude Agent SDK / Claude Code hooks** (33 events, `decision:block`, `--json-schema`, `fork_session`) [V] | **`done_when` Stop-hook bounce** (biggest idea); `WorktreeCreate` adoption; capture the native session id; headless verdict steps | `done_when = { run = "scripts.run.test", max_bounces = 3 }`; `spawn = { …, mode = "headless" }` + `output = { schema = "schemas/verdict.json" }` | — |
| **Google ADK** (Sequential/Parallel/Loop agents; `escalate`; `output_key`) [S] | Bounded loop with an explicit escape; named outputs | `loop = { steps = ["fix","test"], max = 4, until = "steps.test.exit_code == 0" }` | A parallel construct with no join (ADK's `ParallelAgent` has none). |
| **Mastra** (`.foreach(step,{concurrency})`, `suspend`/`resume`, per-index resume) [V/S] | `foreach` with concurrency and per-iteration retry | `foreach = "{{vars.crates}}"`, `concurrency = 3`; `ptyro run retry r42 --step port --index 3` | Code-first builder DSL. |
| **Inngest** (`step.waitForEvent`, keyed `concurrency`, `throttle`, `cancelOn`) [K] | **Keyed global concurrency**; wait-for-event with match and timeout; cancel-on | `[limits.concurrency] "agent:claude" = 4`; `wait_event = { kind = "pr.checks", match = "…", timeout = "2h" }`; `cancel_on = "session.exited:…"` | HTTP-serve/replay model; LLM router. |
| **Temporal** (activity timeouts, heartbeats, `RetryPolicy` non-retryable, Updates/Queries) [V/K] | **Timeout taxonomy** (total/start/**stall** = hook-driven heartbeat); retry classes; validated Updates | `timeout = { total = "45m", start = "2m", stall = "10m" }`; `retry = { max = 2, on = ["timeout","stall"], not_on = ["guard_tripped"] }`; `ptyro run update r42 set-var …` | Deterministic code replay; versioning pain; Continue-As-New. |
| **Restate** (awakeables, durable promises, single-writer virtual objects) [V] | **Gates as awakeables**: stable ids resolvable from anywhere; one per-run actor owns transitions | `ptyro gate resolve --id g_7f3a approve` (from TUI, CLI, phone, lead agent, webhook) | — |
| **DBOS Transact** (workflows as rows in your own DB, SQLite backend in 2026, `fork`, lineage) [S] | **The architectural sibling**: step rows written transactionally; fork from step; `parent_run_id`/`forked_from` | `ptyro run ls --tree` | Postgres-centric distribution. |
| **Dagger / container-use** (typed `Env` I/O) [V] | Typed step inputs/outputs validated by the engine | The `output` contract (below); `sandbox = "container"` later | A container DAG engine. |
| **GitHub Agentic Workflows ("gh aw")** (tech preview 2026-02-13; read-only agent job; **safe-outputs** applied by a separate job) [V] | **Safe-outputs**: agents never push/PR/merge; a deterministic step applies after policy | `apply = { from = "impl", actions = ["push","pr"] }`; `concurrency = { group = "fix:{{vars.issue}}", cancel_in_progress = true }`; `uses = "recipe:test-fixer"`; `on = ["session.errored","cron:0 2 * * *"]` | Expression-language sprawl; natural language compiled into YAML. |
| **Prefect** (Crashed vs Failed; `wait_for_input`; transactions) [K] | Separate `failed` (agent or check said no) from `crashed`/`interrupted` (infrastructure); typed gate input; `on_fail` compensation (v2) | `gate = { ask = "Merge?", options = ["merge","iterate","discard"], input = { note = "string?" } }` | — |
| **pydantic-graph** (`join` reducers, `ReduceFirstValue` cancels siblings) [V] | **Join policies with sibling cancellation**, the core of best-of-N | `join = { policy = "first_success", cancel_siblings = "interrupt" }` (`all\|any\|first_success\|quorum:2\|all_settled`) | — |
| **Apache Burr** (state machine, local tracking UI, fork) [V] | A local run-timeline UI | `ptyro run trace r42` | — |
| **Letta** (shared memory blocks, sleep-time agents) [V] | A shared **run notebook**; opt-in post-run lessons summariser | `$PTYRO_RUN_DIR/notes.md` templated into prompts | A memory server. |
| **Buildkite** (dynamic `pipeline upload`, `block`, `soft_fail`, `retry.automatic`) [V] | **Bounded dynamic expand**: a planner agent emits *data*; ptyro validates against policy and splices steps in | `expand = { schema = "builtin:subtasks", template = "subtask", max = 5 }` | Unbounded dynamic pipelines. |
| **Nx / Turborepo / Bazel** (content-addressed caches) | Cache `run` steps by git tree hash | `cache = "tree"` | Hermeticity, remote execution. |
| **Merge queues** (GitHub, Mergify, Graphite, Aviator: speculative prefixes, batching, bisect) | **Local merge train** for fan-out picks and multi-worktree landing | `ptyro land @run:r42 --train --onto main [--batch 3] [--bisect] [--check …] [--on-eject=send]` | — |

**Biggest complexity traps:**
- deterministic code replay;
- LLM routing or a manager LLM inside the engine;
- shared-state reducers and supersteps;
- a general-purpose expression language;
- unbounded dynamic pipelines;
- pickle or opaque snapshots;
- a container DAG engine;
- promising cost budgets before there is a reliable cost signal per agent.

**Durability semantics for PTY-hosted non-deterministic agents:**
1. **Event-sourced, not code-replayed.**
   - An append-only `run_event` table (run_id, seq, ts, kind, step_id, attempt, payload JSON).
   - `run` / `run_step` / `step_attempt` projections updated in the **same SQLite transaction**.
   - The engine is a pure function `(recipe snapshot, events) → next actions`.
2. **Pin the recipe** (`recipe_src`, `recipe_sha`). Changing it needs an explicit `--upgrade` with compatibility checks.
3. **The attempt is the unit of retry.** Each attempt records the ptyro session id, **the agent-native session id from SessionStart**, the worktree, and `start_ref`/`end_ref` under `refs/ptyro/runs/<run>/<step>/<n>/{start,end}` (via `git stash create` / `commit-tree`; no user-visible commits).
4. **On daemon restart, by step kind:**
   - completed steps never re-run;
   - an in-flight script → `crashed`, retried if `retry.on` includes it;
   - an in-flight agent → `on_restart = "native_resume" | "fresh" | "hold"` (default `native_resume` when the adapter declares one, else `hold`);
   - gates and timers are absolute deadlines;
   - `apply` (push/PR/merge) writes an **intent** event before acting and an **outcome** event after. On restart with an intent but no outcome, **reconcile** by checking the remote ref or PR before retrying. This is the at-most-once boundary.
5. **Completion is evidence, not belief.** Success means status reached the target, **and** the output validated, **and** `done_when` passed. Record which piece decided it.
6. **Time travel means fork.** `run fork --from <step> [--set var=…]` creates a new run with `forked_from` / `fork_point`, copies completed steps, materialises a worktree at the predecessor's `end_ref`, and optionally native-forks the agent session. There is no in-place rewind.
7. **Messages:** at-least-once with acks. Gate resolutions and Updates carry idempotency keys, so a double-tap on the phone doesn't resolve a gate twice.
8. **Trust:** JSON only, never deserialised code. Repo recipes keep the existing trust prompt.

**Observability:**
- **Spans:** `invoke_workflow <recipe>` → `step <id>` → attempt → `invoke_agent <agent>`.
  - Hook-observed tools become `execute_tool <name>` spans from PreToolUse/PostToolUse pairs. ptyro sees tool calls without being inside the model loop, which is unusual.
  - Status transitions are span events; gates are spans whose duration is human wait time.
- The OTel GenAI semconv is still **"Development"** status; pin a version.
- Every `events --json` record carries `run_id`, `step_id`, `attempt`, `trace_id` and `span_id`. Exporting `TRACEPARENT` into agents is experimental (whether agents honour it is unverified).
- Storage is local-first: a SQLite `span` table and a TUI waterfall. OTLP export is optional, and prompt/tool content is opt-in.

### II.4 The coordination substrate: how agents talk, delegate and get supervised

**Five findings that change ptyro's plans.** None of these is in SPEC §20 yet.

1. **Every Claude Code session has a local inbox socket that ptyro can post into.**
   - Requires v2.1.224+ (2026-08-07); on by default.
   - Exported to hooks and Bash as `CLAUDE_CODE_MESSAGING_SOCKET` + `CLAUDE_CODE_MESSAGING_TOKEN`.
   - Wire format is NDJSON [V-local, from strings in the binary]: an optional first line `{"type":"auth","token":"…"}`, then `{"type":"user","message":{"role":"user","content":"hello"}}`. A connection with no complete line within 30 s is closed.
   - Mid-turn, the message is read **between tool calls**. In an idle session it **starts a new turn**.
   - The receiving Claude is told the text came from another session. It **can't approve a prompt, change config or run slash commands**.
   - `crossSessionInbound = accept|hold|refuse`. Holds expire after `dialogExpiry` (5 min). Messages from the session's own children are trusted.
   - Limits: ~1M chars per message; ≤50 queued; burst refusal; identical repeats dropped.
   - `notify_when_idle` (v2.1.236+, 12 h). `--bare` sessions have no socket.
2. **Codex can run its own TUI against an app-server that ptyro owns** [V-local 0.143]: `codex app-server --listen unix://PATH` plus `codex --remote unix://PATH`.
   - ptyro keeps the real TUI in a PTY *and* attaches as a second JSON-RPC client.
   - Exact status from `thread/status/changed`: `notLoaded | idle | systemError | active{activeFlags: waitingOnApproval | waitingOnUserInput}`.
   - Injection via `turn/steer` (with an `expectedTurnId` precondition), `turn/start`, `turn/interrupt` and `thread/inject_items`.
3. **ACP v2** (2.0.0-alpha.9, 2026-10-16):
   - Adds `state_update` = `running | idle{stopReason} | requires_action | unknown`.
   - A `session/prompt` response now only means "inserted", so you can prompt while the agent is running.
   - `mcp/message` (unstable) tunnels MCP over ACP.
   - v2 drops the client-side `fs/*` and `terminal/*` methods.
4. **MCP 2026-07-28 is stateless.**
   - No `initialize`, no sessions (SEP-2575, SEP-2567). State lives in server-minted handles passed as tool arguments.
   - Server-to-client requests are replaced by **Multi Round-Trip Requests** (`resultType:"input_required"` + `requestState`, SEP-2322).
   - Tasks moved to the `io.modelcontextprotocol/tasks` extension (SEP-2663). Sampling, Roots and Logging are deprecated.
   - **The official client matrix lists no coding-agent CLI with Tasks support.**
   - Conflict between researchers: segment F suggested modelling `ptyro mcp` spawn/wait as MCP Tasks. **Resolution:** offer Tasks semantics as an *optional* layer; never depend on them.
5. **Claude Code backgrounds long MCP calls.**
   - A call still running after **2 min** becomes a background task (`CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS`).
   - The stdio idle timeout is 30 min and is reset by progress notifications. `MCP_TOOL_TIMEOUT` defaults to ~28 h.
   - So blocking `wait_for` / `check_messages(wait)` tools are viable if they emit progress.

**Other surfaces worth knowing:**
- **Claude Channels** (research preview, v2.1.80+):
  - Declared with `capabilities.experimental['claude/channel']`; push via `notifications/claude/channel {content, meta}`.
  - A **permission relay** pairs `…/permission_request{request_id,…}` with `…/permission{request_id, behavior}`; the first answer wins.
  - Needs `--dangerously-load-development-channels`.
  - **Gotcha:** a channel server that negotiates MCP 2026-07-28 is *not* registered as a channel.
- **Claude hooks:** `async:true` + **`asyncRewake:true`** runs a background hook whose exit code 2 wakes Claude, with stderr as a reminder. A candidate for a "mail waiter" (spike).
- **Claude stream-json control subtypes** [V-local, undocumented]: `initialize`, `interrupt`, `can_use_tool`, `set_permission_mode`, `set_model`, `mcp_message`, `session_state_changed`, `hook_callback`, `stop_task`, `get_context_usage`, `turn_starting`, `turn_preempted`. Feature-detect them via the `system/init.capabilities` array.
- **OpenCode TUI control:** `POST /tui/append-prompt`, `/tui/submit-prompt`, `/tui/show-toast`, `GET /tui/control/next`, and `POST /session/:id/permissions/:permissionID`.
- **A2A 1.0** (frozen ~2026-03; now under the Agentic AI Foundation):
  - Methods: `SendMessage`, `SendStreamingMessage`, `GetTask`, `ListTasks`, `CancelTask`, `SubscribeToTask`.
  - States SUBMITTED / WORKING / **INPUT_REQUIRED / AUTH_REQUIRED** / COMPLETED / FAILED / CANCELED / REJECTED.
  - **Local coding CLIs don't speak it**; its value to ptyro is as a facade (backlog).
- **mcp_agent_mail** (github.com/Dicklesworthstone/mcp_agent_mail):
  - Git + SQLite mail with **advisory file reservations** (`path|glob`, `ttl_seconds`, `exclusive`, `reason`). Conflicts are *reported*, not refused. Optional pre-commit guard.
  - `HumanOverseer` messages bypass policy and carry a "pause and prioritise" preamble.

**Agent × coordination feature matrix:**

| Feature | Claude Code 2.1.304 | Codex 0.143 | Gemini 0.66n | OpenCode |
|---|---|---|---|---|
| MCP via flag | ✓ `--mcp-config` (+`--strict-mcp-config`) | ✓ | ✓ settings | ✓ config |
| MCP elicitation | ✓ form + url | ✓ (~v0.119) [3P] | ? | ? |
| MCP Tasks extension | ✗ | ✗? | ✗? | ✗? |
| Push into a live session | ✓ inbox socket, channels (preview) | ✓ `turn/steer`, `inject_items` | ✗ (PTY only) | ✓ `prompt_async`, `/tui/*` |
| Native peer messaging | ✓ SendMessage / ListAgents / teams | ✗ | ✗ | ✗ |
| Precise idle/busy | Stop hook, `session_state_changed`, `claude agents --json`, OSC 7501 | `thread/status/changed` | hooks + title | SSE `session.status` |
| Approval interception | PermissionRequest hook, channel relay, `can_use_tool` | app-server `*/requestApproval` | ACP | HTTP permissions |
| ACP | adapter `claude-agent-acp` | adapter `codex-acp` | native `--acp` | native |
| Structured mode **and** native TUI at once | ✗ (`-p` *or* TUI; plus socket) | ✓ `--remote unix://` | ✗ | ✓ (TUI embeds the server) |

**Injecting into a running TUI agent — rules:**
- **Never paste into a `blocked` session.** Modal dialogs eat keys.
- Wait for `idle`: the Stop hook, not `idle_prompt`, which is delayed 60 s.
- Check that the prompt box is empty.
- Expect large pastes to collapse to `[Pasted text #n]` and an Enter right after the paste to be swallowed.
- Prefer native channels: the Claude socket, Codex `turn/steer`/`turn/start`, OpenCode `prompt_async`, ACP v2 `session/prompt`.
- **Provenance matters:** text typed into the PTY reads as *the user*. Socket and channel messages read as *another session* and can't grant consent. So human-originated text goes through the PTY or a user turn, and agent-originated mail goes through the peer path.

**Approval sources to route into one queue:**

| Agent / surface | Request | Response |
|---|---|---|
| Claude `PermissionRequest` hook | — | `hookSpecificOutput.decision{behavior, updatedInput}` |
| Claude channel relay | `permission_request` | `permission` |
| Claude stream-json | `can_use_tool` | control response |
| Codex app-server | `item/commandExecution\|fileChange\|permissions/requestApproval` | response; `serverRequest/resolved` notifies other clients |
| OpenCode | SSE `permission.asked` | `POST …/permissions/:pid` |
| ACP | `session/request_permission` | `{outcome: selected, optionId}`, kinds `allow_once\|allow_always\|reject_once\|reject_always` |
| MCP | elicitation / MRTR `input_required` | — |

Rules others have settled on:
- Agent messages never count as consent.
- Sanitise previews: strip bidi and invisible characters, fold whitespace, keep head and tail, mask secrets, never mask a path or command.
- Use short, phone-typeable request ids.
- The local answer wins a race.
- `requiresUserInteraction`-class tools can't be one-tap approved remotely.

### II.5 Synthesis: what orchestration in ptyro should be

**Positioning.** ptyro orchestration is the **cross-vendor, reboot-surviving, policy-enforcing, evidence-first** layer above agents that each now ship their own single-vendor multi-agent features. It should:
1. **consume** native surfaces (Claude socket, teams and hooks; Codex app-server; OpenCode server; ACP) rather than re-implement them;
2. make **verification and review** the product, not agent count;
3. keep **writes single-threaded per path**, with parallelism for attempts, readers and reviewers;
4. stay **deterministic**: agents plan by emitting *data* that the engine validates and executes.

**Daily-driver ladder.** Each rung is useful on its own, and each is how a single-agent user gets pulled up to the next:

| Rung | What the user does | Built from |
|---|---|---|
| 0 | One agent, but `done` means *verified* and review is ranked | R-ORC-23, R-ORC-24 |
| 1 | Writer + cross-vendor reviewer on every change | R-ORC-04, M7.8 |
| 2 | Side tasks and scouts while landing the main change | R-ORC-28, R-UX-02, R-NOT-06 |
| 3 | Fan-out with an evidence-ranked compare and a merge train | M7.2/7.3, R-ORC-04, R-ORC-12 |
| 4 | Overnight loops and recipes with gates, budgets and a morning digest | R-ORC-25, R-ORC-27, R-ORC-31 |
| 5 | A lead agent over `ptyro mcp` with leases, a mailbox and policy | R-ORC-07, R-ORC-30, R-ORC-34 |
| 6 | Multi-repo change sets | M11 |

**Proposed recipe primitive set.**

*v1 (M7.4), small and fully deterministic:*

| # | Primitive | Shape |
|---|---|---|
| 1 | Step kinds | `spawn`+`prompt` · `send` (follow-up into an existing step's session) · `run` (script) · `gate` · **`apply`** (safe-outputs: commit, push, PR, merge) · `sleep` |
| 2 | Dependencies | `needs`, plus `join = all (default) \| any \| first_success \| all_settled`, with `cancel_siblings = "interrupt" \| "keep"` |
| 3 | Conditions | `if = "<expr>"` over `vars.*`, `steps.<id>.status \| exit_code \| outputs.*`, `gates.<id>.choice`. A tiny non-Turing-complete language; evaluate CEL crates first. |
| 4 | Outputs and artifacts | Built-ins `session`, `worktree`, `branch`, `base`, `head`, `diffstat`, `last_message`, `duration`, `snapshot_ref`. Custom outputs via **`output = { schema = … }`**, written to `PTYRO_STEP_OUT` or produced by headless `--json-schema`; one repair re-prompt on invalid output. Artifacts live under `$PTYRO_RUN_DIR/<step>/`. |
| 5 | Templates | `{{vars.x}}`, `{{steps.id.outputs.y}}`, `{{item}}`. Missing references fail at `ptyro recipe validate`. |
| 6 | Wait | `wait = "idle" \| "done" \| "exited"`, plus **`done_when = { run = …, max_bounces = N }`**: a Stop-hook `decision:block` bounce for Claude, a follow-up send for the others. |
| 7 | Timeouts | `timeout = { total, start, stall }`, where `stall` is a heartbeat driven by hook events |
| 8 | Retries | `retry = { max, backoff, on = [...], not_on = [...], mode = "same_session" \| "fresh", feedback = "…{{attempt.error}}" }` |
| 9 | Gates | `gate = { ask, options = [...], input = {...}, timeout, on_timeout = "reject" \| "approve" \| "escalate:<addr>", notify = [...] }`, with stable ids resolvable from anywhere and idempotent |
| 10 | Budgets | `[budget] wall, sessions, attempts` (add `usd`/`tokens` only once the signal is trustworthy) |
| 11 | Guards | `deny_cmds`, `deny_paths`, `max_diff_lines`, `on_trip` |
| 12 | Bounded loop | `loop = { steps = [...], max = N, until = "<expr>" }`, always capped, with a fresh session per iteration and a checkpoint per iteration |
| 13 | Agent per step | `agent`, `profile` and `model` on each step (planner / worker / judge roles) |
| 14 | `verify` step | `tests`, `repro` (must fail on base), `cross_tests`, `judge` (a different agent; clean context; rubric), `diff_budget`, `max_iterations` |

*v2 (later):*
- `foreach` / `matrix` with `concurrency` and per-index retry
- `quorum:k` joins and `pick = { by = "tests" | "judge" | "human" }`
- bounded dynamic `expand`
- `uses = "recipe:…"` sub-recipes with lineage
- `wait_event` with `match`, and `cancel_on`
- triggers: `on = [...]`, cron, webhooks
- keyed concurrency and `concurrency = { group, cancel_in_progress }`
- `cache = "tree"` on run steps
- a shared run notebook
- `on_fail` compensation
- `sandbox = "container"`

Example, the `implement-and-review` recipe rewritten with v1 primitives:

```toml
name = "implement-and-review"
vars = { task = { required = true } }
[budget]
wall = "3h"
sessions = 4

[[step]]
id = "impl"
spawn = { agent = "claude", worktree = "new", name = "impl" }
prompt = "Implement: {{vars.task}}. Commit when tests pass."
done_when = { run = "scripts.run.test", max_bounces = 3 }    # verified-done, same session
timeout = { total = "45m", start = "2m", stall = "10m" }
guard = { deny_paths = ["tests/fixtures/holdout/**"], deny_cmds = ["git push"], on_trip = "pause" }

[[step]]
id = "review"
needs = ["impl"]
spawn = { agent = "codex", worktree = "impl", mode = "headless" }   # cross-vendor, clean context
prompt = "Review {{steps.impl.outputs.base}}..{{steps.impl.outputs.head}} against: {{vars.task}}. Correctness and requirements only."
output = { schema = "builtin:verdict" }                          # {decision: approve|changes, issues: [...]}

[[step]]
id = "fix"
needs = ["review"]
if = "steps.review.outputs.decision == 'changes'"
send = { to = "impl", text = "Reviewer found:\n{{steps.review.outputs.issues}}\nTriage: fix what is right, push back on what isn't." }
done_when = { run = "scripts.run.test", max_bounces = 2 }

[[step]]
id = "gate"
needs = ["review", "fix"]
join = "all_settled"
gate = { ask = "Ship impl?", options = ["pr", "iterate", "discard"], timeout = "8h", on_timeout = "escalate:@phone" }

[[step]]
id = "ship"
needs = ["gate"]
if = "gates.gate.choice == 'pr'"
apply = { from = "impl", actions = ["push", "pr"] }               # only ptyro pushes; intent/outcome reconciled
```

**`ptyro mcp` tool surface (M7.6).** Stateless-friendly: every create takes an `idempotency_key`, and every wait returns a server-minted handle.

| Tool | Notes |
|---|---|
| `list_sessions(filter)` | Status, `blocked_on`, last-turn summary, worktree |
| `spawn_session(agent, worktree \| new_worktree{base,name}, prompt, profile?, idempotency_key)` | Policy-checked. Over-budget → a gate or `anthropic/requiresUserInteraction`. |
| `send(session, text, mode: queue \| interrupt \| steer)` | Uses the per-agent delivery backend below; returns `delivery_id` |
| `read_screen(session, mode: screen \| last_turn \| transcript_tail)` | Prefer `last_turn` |
| `wait_for(session(s), until: idle \| blocked \| exited \| any_change, timeout)` | Progress notifications every ~20 s; returns `wait_handle` on timeout |
| `subscribe(session, event, once)` | The notice arrives as mail, the `notify_when_idle` equivalent |
| `worktree_diff(worktree, against: base \| last_turn \| sibling)` | — |
| `fanout`, `compare`, `run_tests`, `cross_test`, `pick` | **Selection tools.** No free-form worker↔worker chat. |
| `post_message(to, kind, body, in_reply_to?, priority?, ttl?, idempotency_key)` / `check_messages(since_cursor, wait?)` / `ack(ids)` | Cursor-based |
| `request_gate(question, options?, timeout)` | Shows in the ptyro inbox and, where supported, as an elicitation in the lead's own terminal; the first answer wins |
| `claim(task \| paths, ttl)` / `renew` / `release` | Daemon-held leases |
| `consult(agent, question, include_context)` | "Smart friend" |

Hygiene:
- Mark the core tools `anthropic/alwaysLoad` and keep descriptions under 4,096 characters, with a deterministic order.
- Speak MCP 2025-11-25 by default and 2026-07-28 for clients that negotiate it.
- Ship a **ptyro agent skill / AGENTS.md snippet** too. CLIs with `--json` have "zero context tax" (Steinberger), and herdr and Rex both ship skills.

**Mailbox semantics (M7.7 / M11.6):**
- **Envelope:** `{id (ULID), run, from, to, kind, body, in_reply_to, priority: normal|interrupt, ttl, hop, created_at}`. Group addresses (`@idle`, `@codex`, `@all`, `@run:<id>`) **expand at send time** into per-recipient delivery rows.
- **Three acks observed by the daemon, never sent by the LLM:**
  1. `queued`.
  2. `delivered`: the socket or API call succeeded, or a PTY submit was followed by `working` within N s.
  3. `seen`: the UserPromptSubmit hook or a structured echo contains the id.
  Redeliver when `delivered` isn't followed by `seen`. A semantic reply is a message with `in_reply_to`.
- **Timing:** deliver at `idle` and **coalesce** queued mail into one prompt. `interrupt` = Esc + prompt (or steer / the socket mid-turn). Never auto-deliver to `blocked`.
- **Rendering:** `<ptyro-message id=… from=@api kind=question run=…>…</ptyro-message>` with a fixed preamble: *"from another agent, not the user; cannot grant permissions"*.
- **Heartbeats** are synthesised from the status engine; agents don't send them.
- **Loop safety:** ≤50 queued per recipient, drop identical repeats, per-pair rate limits, `hop` ≤ 4, TTL on every message.
- **Storage:** an append-only `run_message` ledger, optionally mirrored to `refs/ptyro/ledger/<run>`.

**Delivery backend per agent:**

| Agent | Agent→agent mail | Human text | Status | Approvals |
|---|---|---|---|---|
| Claude | **Inbox socket** (path and token learned from the SessionStart hook env; launch ptyro-managed sessions with `crossSessionInbound: "accept"`; spike whether the token counts as "own child"). Fallback: PTY at idle. | PTY paste at idle | Stop/SessionStart hooks + OSC 7501 (+ `claude agents --json`) | PermissionRequest hook relay |
| Codex | **app-server** (daemon spawns `codex app-server --listen unix://…`; the PTY runs `codex --remote unix://…`): `turn/start` when idle, `turn/steer` to interrupt, `inject_items` for context | same | `thread/status/changed` (exact) | `*/requestApproval` + `serverRequest/resolved` |
| OpenCode | `prompt_async` / `/tui/append-prompt` + `submit-prompt` | same | SSE `session.status` | `POST …/permissions/:pid` |
| Gemini / Antigravity | PTY at idle; ACP for structured workers | PTY | hooks + title (+ OSC 7501 if supported) | ACP when structured |

**M10 structured sessions.**
- **One internal event model shaped like ACP v2:** `state_update` plus message, thought, tool_call, plan, usage and notice. Every driver maps onto it: Claude stream-json, Codex app-server, OpenCode SSE, ACP v1/v2.
- **Hybrid first:** run the native TUI and a structured client side by side. Codex and OpenCode support this today; for Claude, use the socket in and hooks out.
- **ACP client:** v1 now, v2 behind a capability flag. Use the registry for discovery. Render the four approval kinds generically.

**Conflict avoidance:**
1. A worktree per writer, and a warning when a Claude team forms inside one worktree (read `~/.claude/teams/*/config.json`).
2. **Daemon-held file leases** enforced through PreToolUse/BeforeTool denials ("leased by @api until 14:05; post_message them").
3. A **collision radar** (`git merge-tree --write-tree` between live siblings) that auto-messages both agents with the overlapping hunks.
4. **Task claims with leases** (beads semantics), with heartbeats taken from the status engine.

**Supervision.**
- One approval queue across every source, with a policy engine: rules on agent, profile, tool, path or command prefix decide `allow | deny | ask | escalate(after T)`. Scopes are once, run or worktree. Batch approval is supported.
- **By default the lead may not approve its workers' prompts.** Delegation must be explicit and is scoped by the daemon.
- **Attention on your own terms:** digest and batch non-blocking events, and break through only for `blocked` (Hashimoto turned notifications off).

---

## Part III — The idea funnel

Every idea from Parts I–II, deduplicated, with a stable ID. **This is the feed for `ROADMAP.md`.**

- **Pri:** ★★★ high leverage, do soon · ★★ valuable · ★ opportunistic.
- **Size:** S ≤ 1 day · M 2–4 days · L 1–2 weeks, as in ROADMAP.
- **Fit:** the suggested milestone, or *Backlog*.
- **Status:**
  - `new`: not yet triaged;
  - `→ M7.x`: promoted, with the ROADMAP task id (`, built` once it shipped);
  - `merged`: folded into an existing roadmap task;
  - `parked`;
  - `rejected (why)`.
- **Seen in:** the products or sources that inspired it. These are ideas only, never code.

### III.1 Top 15

Ranked by evidence, how much they raise ptyro's daily value, and how clearly they set ptyro apart from native single-vendor features. **All 15 were promoted into `ROADMAP.md` on 2026-10-20.** The Roadmap column gives each task id.

| # | ID | Idea | Why now | Roadmap |
|---|---|---|---|---|
| 1 | R-ORC-23 | **Verified-done**: `done` = check passed with evidence; Stop-hook `done_when` bounce | The most-cited failure in the field (false "done"), and it works across all four adapters | M7.9 |
| 2 | R-ORC-24 | **Review queue + evidence bundle + `max_unreviewed`** | Human review is *the* bottleneck; nobody ranks review work | M6.9 |
| 3 | R-GIT-01 | **PR-event loop**: CI failures and review comments go back to the owning session; automerge | Every cloud agent ships this; ptyro has nothing planned after the PR | M6.10 |
| 4 | R-USE-01 | **Usage and quota meters per profile from official feeds** | Cheap now (statusline `rate_limits`, Codex `account/rateLimits/*`), and profiles make it unique | M4.17 |
| 5 | R-ORC-01 | **Native Claude bridges**: inbox-socket delivery, team/task hooks, `claude agents --json` | Consume Claude's new surfaces instead of fighting them; correct provenance | M4.18 |
| 6 | R-ORC-35 | **Codex hybrid**: daemon-owned app-server + `codex --remote` in the PTY | Exact status, injection and approvals while keeping the native TUI | M4.19 |
| 7 | R-STA-01 | **OSC 7501 conformance + interop** (latest rev; OSC 21337; herdr; tuios mapping) | The protocol has gone public and is moving weekly; ptyro is an early implementer | M4.20 |
| 8 | R-ORC-04 | **Evidence-ranked compare + `verify`/judge step** | The research shows selection, not sampling, is where results are lost | M7.10 |
| 9 | R-ORC-03 | **Per-turn / per-iteration checkpoints for every session** | Big trust win; shares code with M11.8 | M7.11 |
| 10 | R-ORC-25 | **`loop` step (Ralph-native) + overnight digest** | Widely used; ptyro's reboot survival makes it robust | M7.12 |
| 11 | R-AGT-01 | **Adapter re-prioritisation**: Antigravity CLI (Gemini CLI retired for consumers), Droid, Copilot | M4.8 is aimed at a retiring product | M4.21 (+ Backlog order) |
| 12 | R-ORC-11 | **Collision radar + daemon file leases** | Cheap; nobody ships it in a TUI; prevents the #1 swarm failure | M7.13 |
| 13 | R-WT-03 | **Read competitors' committed configs** (`.claude/launch.json`, `.conductor/`, `.superset/`, `.codex/`, `copilot-setup-steps.yml`, `.cursor/environment.json`) | Drop-in for teams that already committed those files | M5.10 |
| 14 | R-NOT-01 | **Notification sinks + signed approve/deny links** (ntfy, Pushover, Slack/Discord, Web Push) | Remote approvals long before the M9 PWA | M4.22 |
| 15 | R-ORC-33 | **Event-sourced run model**: pinned recipe, attempts with snapshot refs, `on_restart`, `run fork` | The foundation every orchestration feature above stands on | M7.1 (expanded) |

### III.2 Full catalogue

#### Status engine and attention (R-STA)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-STA-01 | Audit OSC 7501 against the latest spec rev:<br>• ids/subtrees, LRU eviction at 256 records, base64 control-char rejection<br>• done/error survive OSC 133 A, RIS clears<br>• suppress OSC 9;4 → root mapping once 7501 is seen; rate-limit notifications<br>Accept OSC 21337 (low confidence) and herdr `report-agent`. Publish ptyro's precedence table with `applied:false` semantics and the tuios state-name mapping. Any 7501 emitter (e.g. Crush) gets confident status with no adapter. Conformance tests in the fake agent. | Rex, Ghostty, tuios, iTerm2, herdr, Crush | M4 | S | ★★★ | **built M4.20** (2026-10-20) |
| R-STA-02 | `ptyro agent explain <session>`: which source (hook / OSC / fallback) set the state and why | herdr | M5 | S | ★★ | **built M4** as `ptyro status --explain` (2026-10-20) |
| R-STA-03 | `send --wait` returns `stalled` (no activity in N s) or `blocked`; turn counter (`completion_seq`) in `events` | herdr, initech | M5 | S | ★★ | **built M5.2** (2026-10-20): `send --wait` reports idle/blocked/stalled/exited; `turns` in `--json` |
| R-STA-04 | A **done-but-unseen** state, cleared only by focus | herdr, Claude agent view | M4 | S | ★★ | new |
| R-STA-05 | Sub-states: `errored`, **auth-blocked** (feeds profile health: "work needs `gh auth login`"), **rate-limited until T** (Claude `quota_auto_resume_*`, `StopFailure`), `interrupted` | tuios, Claude, Codex `/agents` | M4 | S | ★★ | partly built M4: `errored` with kind, **rate-limited until T**, auth-blocked from screen dialogs (2026-10-20) |
| R-STA-06 | User hooks on state transitions: `on_blocked = "…"` and `ptyro on <event> --when '<expr>' -- cmd` | ccmanager, tmux 3.8 monitor hooks | M5 | S | ★ | new |
| R-STA-07 | **Re-emit status outward**: OSC 7501 / 9;4 / 99 / 21337 for the focused or aggregate session, so Ghostty, kitty, iTerm2 and Rex light up their native UI | Ghostty, iTerm2, Rex | M4 | S | ★★ | new |

#### Agent adapters and integration (R-AGT)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-AGT-01 | **Re-prioritise adapters:**<br>• Antigravity CLI (`agy`) first: `Stop.fullyIdle` → idle, `PreInvocation` → working, resume on `conversationId`; spike OSC 7501 and blocked detection<br>• Droid: Claude-compatible hooks, near-zero cost<br>• Copilot CLI: `COPILOT_HOME`, `permissionRequest` → blocked<br>• Cursor CLI<br>• Kilo and Qwen through parameterised OpenCode and Gemini adapters<br>• Aider downgraded to the generic adapter | G-segment | M4 / Backlog | M | ★★★ | Antigravity **built M4.21** from docs [U]; the rest open (2026-10-20) |
| R-AGT-02 | Ingest `claude agents --json` (and team configs) as foreign rows or child nodes; offer "adopt into ptyro" | Claude agent view | M4 | S | ★★ | partly built M4.18: `claude agents --json` as a status cross-check; foreign rows open (2026-10-20) |
| R-AGT-03 | `ptyro adopt`: resume a session started outside ptyro (`~/.claude/projects`, Codex history) as a managed session | CloudCLI | M5 | M | ★ | new |
| R-AGT-04 | `ptyro fork <session>`: new worktree from HEAD plus the agent's native conversation fork (Claude `--fork-session`, Codex `thread/fork`, OpenCode `--fork`) | agent-deck, Sculptor, Claude, Codex | M7 | M | ★★ | new |
| R-AGT-05 | MCP process pooling and per-session MCP toggles, behind the daemon | agent-deck | Backlog | M | ★ | new |
| R-AGT-06 | `ptyro hooks setup` + `doctor --repair`: detect agents on PATH, install idempotently, detect dropped or untrusted hooks (Codex hash-pinned trust), TUI banner | cmux, iTerm2, Codex | M5 | S | ★★ | partly built M5.6 (2026-10-20): `doctor` self-tests hooks and reports Codex hooks held for review; `--repair` and a TUI banner open |
| R-AGT-07 | Claude `type:"http"` hooks to a daemon localhost endpoint, removing process spawns on hot events (spike vs §8.3 relay) | Claude Code | M8 (spike) | S | ★ | new |
| R-AGT-08 | Cross-agent conversation handoff: export transcript, seed another agent | Kilo `/resume-claude` | Backlog | M | ★ | new |
| R-AGT-09 | **Agent detected inside a shell session**: when a known agent is the PTY's foreground process, label the row `shell › agent` and use that adapter's hook-free status (titles, screen, OSC 7501) instead of OSC 133, which reports the whole run as working [V 2026-10-22, ptyro code]; offer to adopt it as a managed session (R-AGT-03) | herdr (foreground-process detection) | Backlog | M | ★★ | → UX1.4, built 2026-10-22 (adopt: Claude only) |

#### Worktrees and projects (R-WT)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-WT-01 | `.worktreeinclude` (gitignore syntax) + answer Claude `WorktreeCreate`/`WorktreeRemove` hooks with ptyro-managed worktrees, so Claude's own `--worktree` and background dispatches land in ptyro | Claude, ccmanager | M3 / M4 | M | ★★★ | `.worktreeinclude` done (M3.6); `WorktreeCreate` adoption still new |
| R-WT-02 | `ptyro new --pr 1234`: worktree from a PR head | Claude `--worktree "#1234"`, git-gtr, Jean, workmux | M6 | S | ★★ | **built M6.6** (2026-10-20): `new --pr N|URL` |
| R-WT-03 | **Import competitors' committed config** as setup/run sources, shown in the trust dialog: `.claude/launch.json` (`autoPort`), `.conductor/settings.toml`, `.superset/config.json`, `.codex/` environments, `copilot-setup-steps.yml`, `.cursor/environment.json` | Claude desktop, Conductor, Superset, Codex, Copilot, Cursor | M3 | M | ★★★ | **built M5.10** (2026-10-20) |
| R-WT-04 | `symlink = [...]` next to copy rules (`node_modules`, caches) | workmux | M3 | S | ★★ | new |
| R-WT-05 | Move work between checkout and worktree: `ptyro new --take-changes`; `ptyro wt adopt <session>` promotes a main-checkout session into a worktree; `ptyro pause`/`resume` frees the branch | workmux, Codex app, claude-squad | M3 / M6 | M | ★★ | `--take-changes` → UX1.10 (2026-10-22); `wt adopt`, pause/resume still new |
| R-WT-06 | **Safe archive / shelve**: snapshot to `refs/ptyro/archive/<wt>`, remove the worktree, keep the branch; `ptyro wt restore`. A third option beside "refuse removal" | VS Code, andyrewlee/amux | M3 | S | ★★ | → UX1.12 (2026-10-22) |
| R-WT-07 | `ptyro adopt <path>` for worktrees created outside ptyro | andyrewlee/amux | M3 | S | ★ | new |
| R-WT-08 | Run and surface the dev server per worktree (URL in sidebar) on the allocated port | uzi, Superset, cmux | M3 / M4 | S | ★★ | new |
| R-WT-09 | `ptyro gc --merged/--closed` and `gc.archive_on_merge = true`, running the safe-removal matrix | git-gtr, Jean, Claude, VS Code | M6 | S | ★★ | → UX1.12 (2026-10-22) |

#### TUI and UX (R-UX)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-UX-01 | Derived names and summaries: deterministic slug from the prompt by default; optional cheap-model branch names, row summaries and commit messages using the session's own profile account | dmux, Claude agent view | M4+ | M | ★ | slugs → UX1.9 and UX1.24, LLM summaries → UX1.3 (opt-in, owner 2026-10-22); branch names and commit messages still new |
| R-UX-02 | **Actionable inbox**: peek the pending question, reply inline, numbered choice by digit, **snooze** until next transition, jump to newest unread, cycle unread without marking read | Claude agent view, tuios, iTerm2, cmux | M4 | M | ★★★ | partly built M4.5: peek, inline reply, y/a/n answers, newest-first inbox per workspace; snooze open (2026-10-20) |
| R-UX-03 | Companion-shell key per task (same worktree and profile env); **scratch sessions** (no project, temp dir) | AoE | M3 | S | ★★ | companion shell → UX1.13 (2026-10-22); scratch sessions still new |
| R-UX-04 | `ptyro project scan ~/dev` bulk registration | ccmanager | M3 | S | ★ | new |
| R-UX-05 | Publish a **JSON Schema** for `config.toml` and `.ptyro.toml`; document config as agent-editable | Conductor, Warp | M5 | S | ★★ | new |
| R-UX-06 | One-line agent-written note per session (`ptyro note`, or OSC 7501 `msg`) shown in the sidebar | Orca worktree comments | M4 | S | ★ | new |
| R-UX-07 | Promote **session templates / layouts** (`[layouts.dev]` = agent + shell + dev server) | Superset presets, Warp Tab Configs | M3 / M4 | M | ★★ | merged → Backlog "Session templates" (promote) |
| R-UX-08 | **Board view**: Backlog / Working / Blocked / Review / Done columns from status + PR state | Vibe Kanban, Nimbalyst | M6 | M | ★★ | new |
| R-UX-09 | `:` actions palette listing `[scripts.*]` and recipes per project | Codex app actions | M5 | S | ★ | → UX1.18 (2026-10-22) |
| R-UX-10 | Tile view: auto-grid every session in a status | Cursor 3 grid | M2 polish | S | ★ | new |
| R-UX-11 | Prompt library as `@snippet` expansion in `send` and the TUI | Warp, Superset | M5 | S | ★ | merged → Backlog "Prompt library" (promote) |
| R-UX-12 | `ptyro screenshot <session>` → PNG/SVG of the current frame | cmux, kitty 0.49 | M5 | S | ★ | new |
| R-UX-13 | `--ui minimal\|full` ladder: plain multiplexer mode for skeptics | Warp modes | M5 | S | ★ | new |
| R-UX-14 | Clickable `path:line:col` in agent output → `$EDITOR` or the M6 inspector | kitty 0.49.2 | M6 | S | ★ | new |
| R-UX-15 | `ptyro import tmux` migration and a "coming from tmux / Vibe Kanban / claude-squad" guide | tmux ecosystem, Vibe Kanban shutdown | M5 | S | ★★ | new |
| R-UX-16 | Accessibility pass: screen-reader status announcements, linear "plain" mode | Contour | M8 | M | ★ | new |
| R-UX-17 | **Recursive-attach guard** when `PTYRO_SESSION` is set (check SPEC coverage) | zmx | M2 fix | S | ★★ | new |
| R-UX-18 | **Which-key**: pause after the prefix and a compact menu of the next keys appears | which-key.nvim, Helix, Zellij's key hints | UX1 | S | ★★★ | → UX1.14 (2026-10-22) |
| R-UX-19 | **Row menu**: one key (or right-click) on a sidebar row lists every action that applies to that row | owner (2026-10-22) | UX1 | S | ★★★ | → UX1.15 (2026-10-22) |
| R-UX-20 | **Multi-line sidebar rows**: the real program name (Claude's process is named after its version, `2.1.309` [V 2026-10-22, `ps` on macOS]), then state-dependent lines (what a blocked agent wants, the current step, the task and last reply, a shell's last command and exit) | owner (2026-10-22) | UX1 | M | ★★★ | → UX1.1, UX1.2 (2026-10-22) |
| R-UX-21 | **TUI-first parity check**: `make tui-parity` fails CI when a CLI command has no TUI path and no recorded waiver | owner (2026-10-22) | UX1 | S | ★★★ | → UX1.0 (2026-10-22) |
| R-UX-22 | **Auto-register the repo you're in**, in the current workspace, with an undo toast instead of a question | owner (2026-10-22) | UX1 | S | ★★★ | → UX1.5 (2026-10-22) |
| R-UX-23 | **Prompt-first new task**: `n` is one box ("what should it do?"); ⏎ = worktree + branch named from the prompt + default agent + the prompt delivered; `ptyro "task"` from the shell | owner (2026-10-22); Claude agent view, dmux (prompt-named tasks) | UX1 | M | ★★★ | → UX1.9 (2026-10-22) |
| R-UX-24 | **Health badge**: the doctor's findings in the status bar, each with a key that fixes it (shell-init line, tmux options, daemon upgrade) | owner (2026-10-22) | UX1 | M | ★★ | → UX1.20 (2026-10-22) |
| R-UX-25 | **First run and home screen in the TUI**: `init` as a checklist screen; outside a repo, recent and discovered repos instead of a bare shell | owner (2026-10-22) | UX1 | M | ★★ | → UX1.19 (2026-10-22) |
| R-UX-26 | **Interactive tour**: do the steps with a fake agent instead of reading pages | owner (2026-10-22) | UX1 | S | ★ | → UX1.22 (2026-10-22) |
| R-UX-27 | **Errors offer the fix**: no toast names a command to type; it carries a key that does it | owner (2026-10-22) | UX1 | S | ★★★ | → UX1.17 (2026-10-22) |
| R-UX-28 | **Context hints**: the status bar's hints follow the selection (blocked → answer, diff → review, merged → clean up) | owner (2026-10-22) | UX1 | S | ★★ | → UX1.16 (2026-10-22) |
| R-UX-29 | **Name and title**: every create dialog shows a suggested name (from the prompt, PR or branch) that ⏎ accepts; a fixed, slugged name for addresses and a separate free-text title for people | owner (2026-10-22); dmux, Claude agent view (prompt-named tasks) | UX1 | M | ★★★ | → UX1.24 (2026-10-22) |
| R-WT-10 | **Setup suggestions on first use**: lockfiles suggest a setup script, ignored `.env*` files suggest a `.worktreeinclude`, written to local config after a yes | owner (2026-10-22); Conductor, Superset (setup scripts) | UX1 | S | ★★ | → UX1.11 (2026-10-22) |

#### Orchestration (R-ORC)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-ORC-01 | **Native Claude bridges**:<br>• deliver mail through `CLAUDE_CODE_MESSAGING_SOCKET` (NDJSON; spike the token / own-child rule)<br>• ingest `TeammateIdle`/`TaskCompleted`/Notification `agent_needs_input`/`agent_completed`<br>• render Claude teams as children; warn when a team shares one worktree | Claude Code | spike → M4.3 / M7.7 | M | ★★★ | **M4.18 done** (2026-10-20): hooks, Notification types, roster, child rows, shared-worktree warning; socket delivery not adopted (protocol undocumented) |
| R-ORC-02 | Recipe **output contracts**: `output = { schema }`, `PTYRO_STEP_OUT`, one repair re-prompt; headless `--json-schema` verdict steps | Claude workflows, Dagger, Claude `-p` | M7.4 | M | ★★★ | new |
| R-ORC-03 | **Checkpoints for every session** at turn end and loop iteration (generalise M11.8 refs); `ptyro checkpoint ls\|diff\|restore`, `restore --turn N` | Conductor, Claudia, Claude `/rewind` | M7 | M | ★★★ | **→ M7.11, built** (2026-10-20) |
| R-ORC-04 | **Evidence-ranked compare** (see the table in §II.2):<br>• columns: newly broken tests, repro fails-on-base, cross-test matrix, Tree-sitter clusters, divergence probe, diff spread, advisory judge, cost<br>• lexicographic rank with reasons; "differences between top candidates" view<br>• hunk-level "take from B"; `verify` step type; cross-vendor reviewer default with a round cap | CodeMonkeys, Trae, Agentless, Cursor `/best-of-n` | M7.3 / M7.4 | L | ★★★ | **→ M7.10, built** (2026-10-20) |
| R-ORC-05 | Recipe primitives v1 (§II.5): joins with sibling cancel, retries with feedback (`same_session\|fresh`), timeouts `total/start/stall`, guards, `if`, `sleep`. v2: `foreach`/`matrix`, quorum, bounded `expand`, sub-recipes, `cache = "tree"` | pydantic-graph, Temporal, OpenAI guardrails, Mastra, Goose, Buildkite | M7.4 / M7+ | L | ★★★ | new |
| R-ORC-06 | **Spec pipeline**:<br>• `ptyro recipe from-tasks tasks.md` (Kiro / Spec Kit / BMAD → wave DAG)<br>• `gate = { approve = "specs/x/design.md" }`<br>• plan gate (start in plan mode, approve before execute)<br>• built-in `spec-pipeline` and `triage-issues` recipes | Kiro, Spec Kit, Jules, Jean "Mr. Robot" | M7.8 | M | ★★ | new |
| R-ORC-07 | **`ptyro mcp` stateless design** (§II.5 table):<br>• idempotency keys, server-minted wait handles, progress during waits<br>• `subscribe`; selection tools; `claim`/`consult`; `alwaysLoad`<br>• elicitation-backed gates; optional (never required) MCP Tasks layer | MCP 2026-07-28, Claude MCP behaviour | M7.6 | L | ★★★ | new |
| R-ORC-08 | **Reactions / triggers**: `ci.failed` and `pr.review_changes_requested` go to the owning session with a log tail; `escalate_after`; webhook and ntfy watchers → events | Composio AO, agent-deck watchers | M6 / M7 | M | ★★★ | merged → M6.10 (with R-GIT-01) |
| R-ORC-09 | **Watchdog**: derived `stalled` / `spinning` / `zombie`; nudge → handoff → escalate; `restart = { on = ["crash","stall"], backoff = "10s..5m", max = 5 }` | Gas Town Witness, Agent Farm, Overstory, amux | M7 | M | ★★★ | new |
| R-ORC-10 | Context-window % column (Claude statusline `context_window.used_percentage`) | Agent Farm | M4 | S | ★★ | new |
| R-ORC-11 | **Collision radar + file leases + conflict ladder**:<br>• live overlap matrix and `git merge-tree` predictions between siblings, auto-message both agents<br>• daemon leases enforced via PreToolUse/BeforeTool<br>• merge ladder: clean → rebase → rerere → agent in temp worktree → human | Sculptor, mcp_agent_mail, Overstory | M6 / M7 | M | ★★★ | **→ M7.13, built** (2026-10-20) |
| R-ORC-12 | **Single-repo merge train**: `ptyro land --train --batch --bisect --on-eject=send`, speculative prefixes; reuses M11.9 | Gas Town Refinery, GitHub merge queue, Mergify, Graphite | M6 / M7 | L | ★★ | **enabled by M11.9** (2026-10-21): the change-set train (gates, CI waits, resumable stop report) is the building block; a single-repo `ptyro land --train` remains |
| R-ORC-13 | Separate **role/identity from session** in the run model (a respawned session inherits role and brief); `ptyro handoff <session>` (fresh session + generated progress brief) | Gas Town polecats, Anthropic resets | M7.1 | M | ★★ | new |
| R-ORC-14 | **Run timeline / trace view** for every run (not just change sets): span waterfall, hook-derived tool spans, gate wait times | disler observability, Burr, OTel GenAI | M7 / M8 | M | ★★ | **partly built M11.11** (2026-10-21): the change-set view draws the ledger as a timeline with messages as arrows between lanes; a timeline for every run remains |
| R-ORC-15 | **Daemon-owned task board**:<br>• leases tied to liveness, `ready` = deps satisfied<br>• adapters: beads (`bd ready`/`--claim`), Backlog.md (`--json --watch`), Task Master<br>• `--task <src>:<id>`, Definition-of-Done gate | beads, Backlog.md, Task Master, Claude teams | M7 | L | ★★ | merged → Backlog "Tasks" (promote) |
| R-ORC-16 | ptyro as an **ACP proxy** and/or exposing daemon sessions *as* ACP agents to Zed/JetBrains | ACP proxy-chains RFD | Backlog | L | ★ | new |
| R-ORC-17 | Cloud attempts in fan-out (`--agents codex-cloud`), pulling diffs into local worktrees for compare | Codex cloud | Backlog | M | ★ | new |
| R-ORC-18 | Ship a **ptyro agent skill / AGENTS.md snippet** teaching `ptyro send/read/wait --json`, before `ptyro mcp` | herdr, Rex, Orca | M5 | S | ★★ | **built M5.9** (2026-10-20): the AGENTS.md/skill snippet in `docs/scripting.md` |
| R-ORC-19 | Human-driven `ptyro last <session>` and `ptyro handoff <from> <to>`: deliver only when idle, with a provenance header | ccmux | M5 | S | ★★ | new |
| R-ORC-20 | Shared context: `.ptyro/context/` hand-off folder (git-excluded), run notebook `$PTYRO_RUN_DIR/notes.md`, project `decisions` log via `ptyro mcp log_decision`, opt-in post-run lessons summariser | Conductor `.context`, Nimbalyst wiki, Letta | M7+ | M | ★ | new |
| R-ORC-21 | Role briefs as presets (pm, arch, eng, qa, reviewer, frontend/backend) for recipes and change sets; name recipe shapes plainly (fan-out, pipeline, lead/worker, review-loop) | ccswarm, initech, BMAD | M7.8 | S | ★ | new |
| R-ORC-22 | `ptyro wait --match '<regex>'` on screen or scrollback, also via MCP | Cursor Await tool, herdr `wait-output` | M5 | S | ★★ | **built M5.2** (2026-10-20): `wait --match REGEX [--scrollback]` (MCP waits for M7) |
| R-ORC-23 | **Verified-done**: status splits into `done(claimed)` and `done(verified)`, with stored evidence (command, exit code, output tail); `done_when` Stop-hook `decision:block` bounce for Claude, follow-up send for the others | Anthropic harness posts, Claude hooks, StrongDM | M7 (hooks M4) | M | ★★★ | **→ M7.9, built** (2026-10-20) |
| R-ORC-24 | **Review queue**: one ranked "ready for review" lane (tests green, small diff, evidence attached, cost, touched files); `max_unreviewed` policy throttles spawns; digest notifications | Willison, Osmani, Every, METR | M6 / M7 | M | ★★★ | **built M6.9** (2026-10-20); verified-done waits for M7.9 |
| R-ORC-25 | **`loop` step (Ralph-native)**: fresh session per iteration; stop on check-pass / no progress in N / budget; checkpoint per iteration; morning digest | Huntley, Carlini | M7.4 | M | ★★★ | **→ M7.12, built** (2026-10-20) |
| R-ORC-26 | **Anti-gaming**: protected-path write fences (tests, feature list) for implementer roles; **holdout checks** stored outside the worktree (pass/fail + redacted summary only); a diff lint flags deleted or weakened tests | Anthropic harness, StrongDM, SWE-ABS | M7 | M | ★★ | new |
| R-ORC-27 | **Budgets + keyed concurrency scheduler**: per session/run/day (turns, wall clock, tokens/$ when trustworthy); `[limits.concurrency] "agent:claude" = 4`; exceeding a budget opens a gate | Gas Town, Inngest, agent-deck, Cursor | M7.6 | M | ★★ | new |
| R-ORC-28 | **Scout mode** (`ptyro scout`: throwaway worktree, findings file, auto-discard, findings fed into the real run) and **read-only fan-out** (hypotheses, plans, research) | Willison/Bleecher Snyder, Claude teams | M7 | S | ★★ | new |
| R-ORC-29 | Same-prompt **multi-repo fan-out** (`fanout --projects a,b,c`), with no contract: a lighter sibling of change sets that ships before M11 | Willison (six repos) | M7.2 | S | ★★ | new |
| R-ORC-30 | **Lead policy pack**: `lead_may_write = false`, required `discoveries` in `worker_done`, heartbeat SLA, lead may not approve worker prompts unless delegated | Cognition, Claude teams | M7.6 | S | ★★ | new |
| R-ORC-31 | **Overnight recipe + morning digest** (what finished, what's blocked, review first): reboot-safe | Hashimoto, Huntley | M7.8 | S | ★★ | new |
| R-ORC-32 | **Safe-outputs `apply` step**: agents never push, PR or merge; ptyro applies after policy, with intent/outcome reconciliation on restart | gh-aw safe-outputs, Restate | M7.4 / M6 | M | ★★ | new |
| R-ORC-33 | **Event-sourced run model**: `run_event` + projections in one transaction; pinned `recipe_src`/`sha`; attempts with `start_ref`/`end_ref` and native agent session id; `on_restart = native_resume\|fresh\|hold`; `failed` vs `crashed`; `ptyro run fork --from <step>`; `run ls --tree` | DBOS, MAF, LangGraph, Temporal, Prefect | M7.1 | L | ★★★ | **→ M7.1 (expanded), built** (2026-10-20) |
| R-ORC-34 | **Mailbox semantics** (§II.5): provenance split; daemon-observed `queued`/`delivered`/`seen`; coalesce at idle; never deliver to blocked; loop safety; "a peer message is never consent" written into SPEC | Claude cross-session, Overstory, mcp_agent_mail | M7.7 | M | ★★★ | new |
| R-ORC-35 | **Codex hybrid session**: daemon spawns `codex app-server --listen unix://…`, PTY runs `codex --remote unix://…`; `thread/status/changed` → status; `turn/steer`/`turn/start`; approvals (spike: do approvals reach every client, and do injected turns render?) | Codex 0.143 | spike → M4.7, pull M10.4 forward | M | ★★★ | **M4.19 spiked** (2026-10-20): protocol verified; approval routing untested (Codex login rejected); not adopted yet |
| R-ORC-36 | **Unified approval queue + policy engine** across Claude hook/relay, Codex, OpenCode and ACP: rules → `allow\|deny\|ask\|escalate(after T)`; scopes; batch approve; preview sanitiser | Claude, Codex, ACP | M7 / M9.5 | L | ★★ | new |

#### Usage, profiles and security (R-USE, R-PRF, R-SEC)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-USE-01 | **Usage and quota meters per profile**:<br>• Claude: `ptyro statusline-relay` as `statusLine.command`, chaining to the user's own; `rate_limits.five_hour/seven_day.used_percentage/resets_at`, cost, context %<br>• Codex: side app-server per `CODEX_HOME` on `account/rateLimits/updated`<br>• history via `ccusage --json`<br>• P90 soft limits, "hits the 5 h cap at 14:20"; quota-aware suggestion to switch profile | Orca, agent-deck, ccusage, Usage-Monitor, ccstatusline | M4 / M5 | M | ★★★ | **built M4.17** (2026-10-20) |
| R-USE-02 | Spend caps per session/profile with pause + notify (only where the cost signal is reliable) | Cursor, agent-deck | M7 / M8 | S | ★★ | new |
| R-PRF-01 | `ptyro session move --profile <p>`: re-home a session (native resume) to another account, e.g. when one hits quota | agent-deck `switch-account`, Orca | M7 | M | ★★ | new |
| R-PRF-02 | Commit-attribution policy per profile (agent / you / `Co-Authored-By`) | Jules | M3 | S | ★ | new |
| R-PRF-03 | `keyring` backend for profile secrets beside `command = [...]` | Emdash | M3 | S | ★ | new |
| R-PRF-04 | "Provider via proxy" profiles (`ANTHROPIC_BASE_URL` to GLM/OpenRouter/local), plus importing CCS profiles | CCS | M3 | S | ★ | new |
| R-SEC-02 | **Audit log** `ptyro log --json` of every approval, permission decision and remote input | Ona, Warp Oz | M5 / M9 | S | ★★ | **partly built M9.2** (2026-10-21): remote actions are `remote` events in each session's log; a `ptyro log` command remains |
| R-SEC-03 | **Cross-agent permission modes** (read-only / safe-auto / full-auto) mapped to each agent's flags and switchable live; a policy-script hook (stdin JSON → allow/deny/ask); tiered opt-in auto-approve (allowlist first, LLM judge second, all logged) | Happier, Paseo, ccmanager, claude-squad | M7 | M | ★★ | new |
| R-SEC-04 | Signed releases (minisign/cosign) and verified self-update | andyrewlee/amux | M8 | S | ★ | new |
| R-SEC-05 | **Sandbox tiers**:<br>• Linux: Landlock, or `srt`/bwrap around the whole agent PTY<br>• macOS: Seatbelt<br>• `sandbox = "sbx"` (Docker Sandboxes)<br>• devcontainer with cached image; Podman<br>• execution target chosen per task in the wizard<br>• `doctor` checks bwrap/socat/AppArmor | Copilot app, Claude sandbox, Docker sbx, Sculptor, AoE, JetBrains Air | M8 / Backlog | L | ★★ | merged → Backlog "Container/sandbox sessions" (concretise) |

#### Remote and mobile (R-REM)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-REM-01 | `ptyro --host <ssh\|ws target>` for **every** CLI verb; multi-daemon sidebar | Paseo, Jean, herdr | M9.1 | M | ★★ | **built M9.1** (2026-10-21): `--remote` for every verb and the TUI, over ssh; ws targets and a multi-daemon sidebar remain |
| R-REM-02 | **Pairing ladder**: mDNS + QR (one-time secret + daemon key fingerprint) → Tailscale → E2E relay; WebAuthn/passkey device enrolment with biometric step-up for approvals; revocation; read-only device tokens | CC Pocket, Claude Trusted Devices, Zellij, iTerm2 Buddy | M9.2 | L | ★★ | **partly built M9.2** (2026-10-21): QR pairing, read/approve/control/full tokens, revocation; mDNS, passkeys and the E2E relay remain |
| R-REM-03 | PWA design:<br>• **true xterm.js view of the daemon PTY** next to agent cards<br>• mobile key row (Esc, Ctrl, arrows, **y/n**)<br>• bookmarkable `/<session>` attach-or-resume URLs<br>• single-pane fullscreen | VibeTunnel, Zellij 0.44, sshx | M9.3 | L | ★★ | **partly built M9.3** (2026-10-21): a canvas view of daemon frames (not xterm.js), a key row with y/n, `#/s/<id>` URLs, a chat view |
| R-REM-04 | A minimal **read-only status web view** before full M9 | AoE, agent-deck | M5 / M9 | S | ★ | **superseded by M9.3** (2026-10-21): the full web app, with read-only device scopes |
| R-REM-05 | `ptyro handoff <session> --host <ssh>`: commit/stash, push, recreate the worktree remotely, resume the conversation, stable session id | Devin `/handoff`, Claude `--teleport`, Happier | Backlog "Cloud/SSH hosts" | L | ★ | merged |
| R-REM-06 | Port "portals": forward detected dev-server ports through the relay to the phone | Amp, cmux | M9 | M | ★ | new |
| R-REM-07 | Publish the WebSocket protocol as a **stable, versioned API** with small TS/Python clients | Paseo SDK, OpenHands Agent Server | M9.2 | M | ★★ | new |
| R-REM-08 | Phone → file/photo into the worktree, referenced in the prompt | Claude Remote Control | M9 | S | ★ | new |
| R-REM-09 | **Share links**: `ptyro share <session> [--readonly\|--steer] [--ttl 1h] [--allow github:alice]` (upterm-style forge-key auth); status-aware sharing | upterm, sshx, Warp Oz, Happy multiplayer | M9 | M | ★★ | new |
| R-REM-10 | Reconnect-transparent remote TUI (sequence-numbered stream, resend frame diff); optional predictive echo; document mosh/ET as transport | mosh, ET, sshx | M9.1 | M | ★★ | new |

#### Notifications (R-NOT)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-NOT-01 | **Notification sinks**: ntfy, Pushover, Slack/Discord webhooks, Telegram, later Web Push, with **signed approve/deny callback links** | Omnara, claude-push, Happier, agent-deck | M4 / M5 | M | ★★★ | **built M4.22** (2026-10-20) |
| R-NOT-02 | **Actionable desktop notifications** (Approve / Deny / Reply via libnotify/macOS), plus emitting OSC 99 with action buttons to kitty-class outer terminals | ccmux, kitty | M4 | M | ★★ | new |
| R-NOT-03 | `ci_failed` attention category; outer-terminal title badge counts (waiting / unread / failing CI) | VS Code Agents window | M6 | S | ★★ | new |
| R-NOT-04 | Command-finish notifications for plain shell panes via OSC 133 C/D (unfocused only, after N s) | Ghostty 1.3 | M4 | S | ★ | new |
| R-NOT-05 | `ptyro badge` CLI: set/clear a badge with priority, optionally tied to a PID's lifetime; roll-up pane → workspace → project | Wave `wsh badge` | M5 | S | ★ | new |
| R-NOT-06 | **Digest mode / attention windows**: batch non-blocking events and break through only for `blocked`; foreground/background session priority | Hashimoto, Willison | M4 | S | ★★ | new |

#### Review, PRs and issues (R-GIT)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-GIT-01 | **PR-event loop**:<br>• `ptyro watch-pr` polls `gh pr checks` and review threads, injects them into the owning session<br>• replies labelled "via ptyro"; asks when a comment is ambiguous<br>• `ptyro pr automerge` (`gh pr merge --auto`) | Claude Auto-fix, Jules CI Fixer, Amp, Cursor, Composio | M6 | M | ★★★ | **built M6.10** (2026-10-20) |
| R-GIT-02 | **Issue intake**: `ptyro issue start <url>` (gh first, then Linear, Jira…) → worktree + issue-named branch + seeded prompt + link-back comment; "My Work" sidebar (PRs awaiting you, issues assigned) | Copilot, Codex/Linear, Amp, Emdash, Conductor | M6.6 / M7 | M | ★★★ | partly built M6.6 (2026-10-20): `--issue N|URL` via gh (branch, seeded prompt); link-back comment and "My Work" open |
| R-GIT-03 | Session-id marker in PR bodies so `ptyro pr open <url>` finds or reattaches the session ("teleport by PR") | Claude cloud | M6 | S | ★ | **built M6.5** (2026-10-20): PR body marker + `pr open N|URL` |
| R-GIT-04 | Review on demand via a `ptyro-review` PR label | Codex reviewer | M6+ | S | ★ | new |
| R-GIT-05 | Per-worktree `artifacts/` (screenshots, test logs) attached to the PR body | Cursor cloud, Antigravity | M6 | S | ★ | new |
| R-GIT-06 | `ptyro pr create --describe`: the session's agent drafts the PR body | Vibe Kanban, Jean | M6 | S | ★ | new |
| R-GIT-07 | **Plan review**: when blocked on plan approval (`ExitPlanMode`), open the plan in the inspector with line comments → approve or send comments | Antigravity Artifacts | M6 | M | ★★ | new |

#### Runs, scheduling and triggers (R-RUN)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-RUN-01 | `ptyro queue add "<prompt>"` with `max_parallel` per project/profile (also a quota tool); chain tasks off a parent's branch | tsk | M7 | M | ★ | new |
| R-RUN-02 | **Schedules and self-wake**: `ptyro schedule "<cron>" recipe:<name>`; agent-callable `ptyro wake --in 30m --session X "check CI"` | Superset, Codex app, Jules, Amp, amux, Claude Routines | M7 | M | ★★ | new |
| R-RUN-03 | **Event-triggered recipes**: `on = ["session.errored","cron:…","gh:review_requested"]`; webhook `POST /tasks` on the M9 server | Superset, Ona, gh-aw, Conductor routines | M7+ / M9 | M | ★★ | new |

#### Observability (R-OBS)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-OBS-01 | **Local OTLP/HTTP receiver** in the daemon; inject `OTEL_*` with `ptyro.session`/`ptyro.profile` resource attributes; chain to the user's endpoint; exact per-session tokens, cost and tool timeline | Claude Code OTel | M8 | M | ★★ | new |
| R-OBS-02 | `ptyro stats --json` and local analytics: time blocked vs working, PRs opened/merged, **selection log** (which signal picked the winner; win rate by agent) | Factory, research §II.2 | M5 / M7 | S | ★★ | new |
| R-OBS-03 | Exports: **asciicast v3** with status markers, `read --format html\|vt\|text`, per-line timestamps, `ptyro stream` in asciinema's live protocol | asciinema 3, zmx, tmux 3.8 | M8 | M | ★ | **partly built M8.2** (2026-10-21): `export --format asciicast` (v2) / `text` / `ptylog` and `replay`. Open: v3 with status markers, html/vt, `ptyro stream` |

#### Extensibility (R-EXT), protocols (R-ACP) and resources (R-RES)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-EXT-01 | **stdio JSON "provider" plugins** (external binaries feed read-only panes) before WASM | sidecar | Backlog | M | ★ | new |
| R-EXT-02 | Out-of-process **browser bridge** (Playwright per worktree port; pick an element → agent) | Orca, Cursor, Superset, Claude desktop | Backlog | L | ★ | new |
| R-EXT-03 | **tmux control-mode (`-CC`) facade** so iTerm2/WezTerm/Contour render ptyro sessions as native tabs | WezTerm, Contour | Backlog | L | ★ | new |
| R-EXT-04 | zjbar/tmux-compatible status feed | Zellij plugins | Backlog | S | ★ | merged → Backlog "`ptyro status-line`" |
| R-ACP-01 | **ACP-first M10**:<br>• internal event model shaped like ACP v2; v1 client now (1.26), v2 behind a flag<br>• registry discovery (`ptyro agents add <id>`), generic ACP profile<br>• subagent-tree rendering, `mcp/message` tunnelling of ptyro tools | ACP registry, JetBrains Air, Zed, Happier | M10 (earlier) | L | ★★ | **partly built M10** (2026-10-21): an ACP v1 client (`protocol = "acp"`); v2, registry discovery and subagent trees remain |
| R-RES-01 | **Auto-shelve** idle unattached agent sessions after N minutes; native resume on attach | Claude agent view | M4 / M8 (hibernation) | S | ★★ | merged → M4 hibernation |


#### From the herdr and tuios deep dives (R-DD)

| ID | Idea | Seen in | Fit | Size | Pri | Status |
|---|---|---|---|---|---|---|
| R-DD-01 | **herdr-compatible socket shim**: export `HERDR_ENV`/`HERDR_SOCKET_PATH`/`HERDR_PANE_ID`/`HERDR_BIN_PATH` and accept `pane.report_agent`/`release_agent`/`report_agent_session`/`report_metadata` (monotonic `seq` per source). Agents that already report to herdr (Crush, Kiro, Command Code, Muse, Prime Agent) then get confident status with no adapter. Opt-in per profile; unknown subcommands rejected clearly. | tuios (ships it), herdr | M4 (with M4.20) | M | ★★ | new |
| R-DD-02 | **OSC 7501 `done` semantics**: in the spec, `done` means *finished, results unseen*. Treat an inbound `done` as idle plus attention (finished-unread), *not* ptyro's orchestration `Done`. Add a `Blocked{Unspecified}` kind, and never guess the kind from `msg`. Port tuios's MIT conformance cases. | DD2.11 | fold into M4.20 | S | ★★★ | new |
| R-DD-03 | **Nested-terminal 7501**: show tuios-in-ptyro child-only records (`<session>/<pane8>`, ≤64, 250 ms batches) summarised by urgency. When ptyro re-emits outward, mirror tuios: probe once per client, one id per session, `clear` on detach, and accept a prefix-matched probe reply. | DD2.4b, DD2.11 | M4 | S | ★★ | new |
| R-DD-04 | **Honest status fields**: the silence timer goes to `unknown`, not `idle`. Expose `ready`, `needs_you` and `evidence_age_ms` in `--json`. Add a **hook-heartbeat stall** that ignores spinner output (tuios #898 is still open). Add a **visible-blocker loan**: screen-detected blocked borrows over a stale `working` and gives it back when the prompt disappears. | tuios, herdr | M4.4 | M | ★★ | new |
| R-DD-05 | **Detection manifests as data**: versioned TOML (`region`, `priority`, `any`/`not`, `visible_blocker`), local overrides, optional signed remote update, hot reload, `explain` that prints "words seen and not counted", a bounded wrapper walk, and a `PTYRO_AGENT=` hint. Seed from the Apache-2.0 manifests, keeping attribution. | herdr, tuios | M4.10 | M | ★★ | new |
| R-DD-06 | **Agent-reported resume argv** (plain command on PATH, ≤64 args / 8 KiB, accepted only once the source holds the session), plus a "Resume" inbox item after a reboot (`ask\|auto\|off`) and staggered restore (100 ms apart, after a client supplies size). | herdr, tuios | M4.11 | S | ★★ | new |
| R-DD-07 | **Environment hygiene**: strip outer-terminal session markers (iTerm2, WezTerm, Kitty, tmux, Zellij) and Claude/Codex session variables from new sessions; set `TERM_PROGRAM=ptyro`. | herdr #4104 | M2 fix | S | ★★ | new |
| R-DD-08 | **Delivery queue primitive** ("type when idle"): a 5 s check that the prompt was taken (→ `stalled`) and `held` after 30 s on first-run dialogs (→ inbox question). The reliability core under `send --wait`, fan-out and recipes. | tuios `queue`, herdr `agent prompt` | M5.2 | S | ★★★ | new |
| R-DD-09 | **Held approvals with a whole-line rule**: hold a Claude `PermissionRequest` only when one line shows the full effect (Bash without a sandbox override, Read, Grep, WebFetch). Daemon-enforced risk rules (`rm -rf`, parsing `;`/`&&`/`\|`/`$()`) need a double press, and answers from agents are refused (`not_human`). | tuios | M4.5 | M | ★★ | new |
| R-DD-10 | **Review notes that re-anchor** (keep the quoted line text, re-find it within ±50 lines, else mark `outdated`), delivered only when the agent is at rest. **Ship with confirm tokens** (a hash of branch, commit, remote and PR; pushes started from a pane need a human yes), run **under the workspace's identity profile**, which tuios cannot do. | tuios | M6.3 / M6.5 | M | ★★ | new |
| R-DD-11 | **Control-plane ergonomics**: a JSON Schema (`ptyro api schema --json`); `hint` objects on every error (`did_you_mean`, `accepted`, `available`); `events_lost` plus a snapshot/reconcile recipe; `list-verbs`. | herdr, tuios | M5.1 | M | ★★ | new |
| R-DD-12 | **`ptyro --skill`** built into the binary, plus SessionStart context injection ("You run in ptyro session X…") and a guardrail (act only when `PTYRO_SESSION` is set). | herdr, tuios | M4.3 / M5 | S | ★★ | merged → R-ORC-18 |
| R-DD-13 | **`terminal observe/control` NDJSON streams** (base64 ANSI frames; one controller, many observers) and declarative `layout export\|apply`. Cheap paths for ccgram, Moshi-style and web clients, and for recipes. | herdr | M5 / M7 | M | ★ | new |
| R-DD-14 | **tmux shim**: a `tmux` binary on PATH inside sessions, so Claude Code agent teams open ptyro sessions instead of tmux panes. | tuios | M4 / M7 | M | ★★ | new |
| R-DD-15 | **Per-session quiet flag**: suppress inbox items for orchestrated or background workers while keeping state and events. Default on for M7 recipe workers. | tuios #921 | M7 | S | ★★ | new |
| R-DD-16 | **Cross-host `worktree pull`**: commits as a git bundle and uncommitted work as a patch into a new local worktree; refuses if the origin differs; never changes the remote. | tuios | M9 / M11 | M | ★ | new |
| R-DD-17 | **Ops lessons**: logind delay inhibitor, rotating session snapshots, recovery copies on failed loads (M8); static status glyphs, no hidden-pane rendering, redraws capped at the display rate (perf budget). | herdr | M8 | S | ★ | new |
| R-DD-18 | **Interop targets**: a ptyro backend for ccgram; attachable from Moshi, collie and herdr-web-ui (mobile layout at ≤64 columns); Agentchute-style Markdown inboxes as a mailbox transport. | herdr ecosystem | M9 / Backlog | M | ★ | new |
| R-DD-19 | **Contribution policy for agent-generated PRs** (herdr auto-closes unsolicited PRs and runs an issue agent): decide early, and document it in AGENTS.md and a CONTRIBUTING.md. | herdr | now (docs) | S | ★ | new |

### III.3 Explicitly *not* recommended (with reasons)

- **A vector-DB memory layer** (Ruflo): its own figures are self-reported; a lightweight notes file covers the need.
- **Peer-to-peer free-form worker chat:** MAST and Cognition locate failures there. Prefer typed messages and selection tools.
- **An LLM router or manager inside the engine:** breaks ptyro's "deterministic engine" principle (SPEC §15). Agents plan by emitting data.
- **Deterministic code replay** (Temporal-style) of agent steps: PTY agents can't be replayed. Fork instead.
- **Mode systems** ("architect/code/debug" modes): commoditised into generic subagents. Roo Code shut down.
- **A2A as a primary protocol:** no coding CLI speaks it. At most a facade later.
- **Depending on MCP Tasks:** no client supports it yet.

---

## Part IV — Watchlist, open checks and positioning

### IV.1 Watch closely (re-check monthly)

| Product | Why |
|---|---|
| **herdr** | Same architecture, huge mindshare, $6M seed, a plugin ecosystem filling its gaps, Herdr Cloud (DD1). Watch Discussions #5073/#5106 for OSC 7501 |
| **Rex** (Superlogical) | From OSC 7501's author; planned priority inbox and mobile push |
| **Claude Code** agent view, teams, workflows, cross-session messaging | Direct overlap; each release can obsolete or enable a ptyro feature |
| **thurbox** | One session across several repos on a shared branch, the closest thing to M11 |
| **Paseo** | Nearest twin of ptyro's daemon + CLI + mobile + relay shape |
| **tuios** | OSC 7501, precedence ladder, inbox, `fan`. Ships ~80 commits/day and already covers much of M4–M10 (DD2) |
| **Codex app-server / `--remote`** | Enables the hybrid session (R-ORC-35) |
| **ACP v2** | M10 design target |
| **OSC 7501 spec** | Conformance (R-STA-01) |

### IV.2 Claims to re-verify before relying on them

- Codex `cloud exec --attempts 1..4` (best-of-N): blog-only.
- Conductor per-turn checkpoints: secondary only.
- OSC 7501 current revision: 0.2 vs 0.4 were both seen within a week.
- Gemini CLI availability for enterprise and API-key users after 2026-06-18.
- ~~herdr license~~: resolved in DD1.1 (Apache-2.0 since 2026-07-22).
- Orca star count: about 99k vs about 55k.
- Claude `crossSessionInbound` own-child verification by token on Linux. **Spike** (R-ORC-01).
- Whether Codex app-server approvals reach every client, and whether turns injected over `--remote` render in the TUI. **Spike** (R-ORC-35).
- Claude `asyncRewake` hook as a mail waiter. **Spike**.
- Antigravity CLI: is there a blocked/permission signal or OSC 7501? **Spike** (R-AGT-01).
- Docker `sbx` on Linux.
- Kiro "waves" of parallel tasks; Spec Kit 1.0 date; Composio AO repo identity.
- METR 2026 follow-up percentages; Sonnet 4.5 parallel-compute score.
- Not covered in this sweep, so add them next time: HN and Latent Space threads; agent-driven
  merge-conflict and patch-synthesis research; SWE-bench Live/Multimodal; an equal-cost study of
  same-model vs mixed-model attempts; empirical limits on how many agents one human can supervise.

### IV.3 Facts waiting to graduate into SPEC §20

These are verified or locally checked. Each moves to SPEC §20 once a spike or implementation confirms it.

- **§20.1 Claude Code:**
  - Cross-session socket: `CLAUDE_CODE_MESSAGING_SOCKET` / `_TOKEN`, NDJSON `{"type":"user",…}`, `crossSessionInbound`, caps, no socket under `--bare`.
  - MCP auto-background after 2 min (`CLAUDE_CODE_MCP_AUTO_BACKGROUND_MS`); stdio idle timeout of 30 min, reset by progress.
  - Channels capability and its v2-runtime gotcha.
  - Agent-team paths: `~/.claude/teams/{team}/inboxes/*.json`, `~/.claude/tasks/{team}/`.
  - `claude agents --json`; supervisor state in `~/.claude/daemon/roster.json` and `~/.claude/jobs/<id>/state.json`.
  - The 33 hook events and Notification matchers (`agent_needs_input`, `agent_completed`, `quota_auto_resume_*`).
  - `type:"http"` hooks; `asyncRewake`.
  - Statusline `rate_limits` JSON.
  - `WorktreeCreate` output contract.
  - OSC 7501 since 2.1.295.
- **§20.2 Codex:**
  - `--remote unix://PATH`.
  - app-server `ThreadStatus` enum; `turn/steer` needs `expectedTurnId`; `thread/inject_items`; `account/rateLimits/*`.
  - Hook trust is hash-pinned (`--dangerously-bypass-hook-trust`).
  - Latest release 0.163.0 (we verified 0.143.0 locally).
- **§20.3 Gemini / Antigravity:**
  - Gemini CLI replaced for consumer accounts on 2026-06-18.
  - `--experimental-acp` deprecated in favour of `--acp`.
  - Antigravity hook names and camelCase fields (G4).
- **§20.4 OpenCode:**
  - `/tui/append-prompt|submit-prompt|show-toast`, `GET /tui/control/next`.
  - `POST /session/:id/permissions/:permissionID`; `/global/event`.
  - Kilo is a fork.
- **§20.5 Terminal:**
  - OSC 7501 spec URL and revision; OSC 21337 (iTerm2) format.
  - OSC 3008 (systemd context) must be swallowed.
  - tmux 3.8 OSC 133 hook events.
  - Copilot CLI reads `.claude/settings.json` hooks, which is another reason to keep integrations out of repo config.

### IV.4 Sweep cadence and method

- **Quarterly full sweep:** re-run the seven segment passes plus the orchestration deep dives.
  Update maturity lines, add new entrants, and mark shutdowns. The last sweep used ten parallel
  research agents.
- **Monthly:** re-check the §IV.1 watchlist and change the status of anything promoted.
- **Ad hoc:** when a launch lands (HN front page, a major vendor post), add an entry or a changelog line here first.

### IV.5 Positioning, in one paragraph

ptyro is the **terminal-native, vendor-neutral control plane for coding agents**. A daemon owns
every agent's real terminal, so sessions survive closed terminals, dropped SSH and reboots. Each
task gets a safe worktree. Each workspace carries its own identity: account, git author, SSH key
and gh login. ptyro knows when any agent is working, blocked or done without scraping the screen.
On top of that, ptyro runs **evidence-first orchestration** across Claude, Codex, Gemini/Antigravity
and OpenCode:
- verified-done;
- ranked review;
- fan-out with honest comparison;
- loops and recipes with human gates and daemon-enforced policy;
- one change across many repos.

Everything is scriptable with `--json`. Single-vendor tools can't cross vendors, GUI tools can't
follow you over SSH, and tmux plugins can't enforce policy. ptyro does all three.

---

## Changelog

- **2026-10-20:** First full sweep. Segments A–G (over 100 products), the orchestration deep
  dive (practitioners, research evidence, workflow engines, coordination protocols), and the idea
  funnel (122 ids). Purpose added to `AGENTS.md`, `SPEC.md` and `README.md`.
- **2026-10-20 (later):** All 15 top ideas promoted into `ROADMAP.md` (M4.17–M4.22, M5.10, M6.9–M6.10, M7.1 expanded, M7.9–M7.13). Deep dives on **herdr** (DD1) and **tuios** (DD2) added. A1/A3 and the executive summary corrected. 19 new ideas filed (R-DD-01…19, 141 ids in all).
- **2026-10-20 (M7):** R-ORC-03, R-ORC-04, R-ORC-11, R-ORC-23, R-ORC-25 and R-ORC-33 are
  built (M7.1, M7.9–M7.13). Left out of R-ORC-04 and filed as follow-ups: Tree-sitter
  equivalence clusters (normalized-diff clusters stand in), the divergence probe, and cost as a
  ranking signal. The verified facts (Stop-hook `decision:block`, PreToolUse
  `permissionDecision: deny`, `--mcp-config` inline JSON, Gemini `BeforeTool` deny) moved to
  SPEC §20.1–20.3 with their verification level.
- **2026-10-21 (M8):** R-OBS-03 is partly built: the recorder exports asciicast v2, text and
  `.ptylog`, and `replay` plays recordings back. asciicast v3 with status markers, html/vt
  export and `ptyro stream` stay open. Live upgrade is now built (M8.1, by re-exec rather than
  herdr's fd passing), so the DD1 comparison row is updated. alacritty_terminal facts found by
  the M8 compatibility sweep (tab cells, DECCOLM, DSR 6 under DECOM) are in SPEC §20.5.
- **2026-10-21 (M9, M10):** remote and structured sessions are built.
  - R-REM-01 is built (`--remote` over ssh). R-REM-02 is in part (QR pairing, scoped and
    revocable tokens; not mDNS, passkeys or the relay). R-REM-03 is in part (a canvas view of
    the daemon's frames, the key row, session URLs). R-REM-04 is superseded by the app.
  - R-SEC-02 is in part: remote actions are in each session's event log.
  - R-ACP-01 is in part: an ACP v1 client, usable by any agent with `protocol = "acp"`.
  - Verified facts moved to SPEC §20.1–20.3: Claude's PermissionRequest hook runs alongside its
    dialog; Claude stream-json with `--permission-prompt-tool stdio`; the Codex 0.143
    app-server handshake and schema; Gemini 0.64 ACP.
- **2026-10-21 (M11):** change sets are built (M11.0–M11.13).
  - The M11.0 experiment with real Claude agents on three repos is in SPEC §15.7.1: the
    side-by-side layout doubles as an overlay; leads write per-member sections without
    `affects:` lines (titles now stand in); a lead committed for its members (now flagged).
  - R-ORC-14 is in part (the set view's ledger timeline); R-ORC-12 is enabled (the train is
    reusable for a single-repo merge queue). R-ORC-29 (same-prompt multi-repo fan-out) stays
    open as the lighter sibling.
  - The competitive note holds: no surveyed tool ships contracts, set checkpoints or a
    multi-repo merge train; thurbox remains the closest (shared-branch multi-repo sessions).
  - A verified fact moved to SPEC §20.1: Claude 2.1.306's folder-trust dialog has no number
    keys (Down, Enter).
- **2026-10-22 (UX1):** The owner made the out-of-the-box experience the top priority and the
  TUI the first-class interface. R-UX-18…28 and R-WT-10 are new and promoted straight into the
  new ROADMAP milestone UX1, with R-AGT-09, R-UX-01 (summaries, opt-in LLM approved), R-UX-03,
  R-UX-09, R-WT-05 (`--take-changes`), R-WT-06 and R-WT-09.
- **2026-10-22 (naming):** The owner split a session's label in two: a fixed name to type and a
  changeable title to read, with a suggested name in every create dialog. R-UX-29, promoted to
  UX1.24. R-UX-01's prompt slugs feed the suggestion.
