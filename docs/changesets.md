<img src="logos/ptyro-icon.png" alt="ptyro" width="72" align="right">


# Change sets

One coordinated change across several repositories: an API field added in `api`, used in
`sdk`, shown in `web`. A change set gives that work one name, one place, one status and one
safe way to ship, while staying honest that git, PRs and CI are per repository (SPEC §15.5,
§15.7; ROADMAP M11).

```bash
ptyro cs new add-user-tier --projects api,sdk,web -p "users get a tier: free or pro"
ptyro cs contract add-user-tier approve      # once the lead's contract looks right
ptyro cs status add-user-tier                # where it stands, and what to do about it
ptyro cs commit add-user-tier -m "Add user tier"
ptyro cs land add-user-tier                  # merge in dependency order, through gates
```

Try it with fake agents first: `make changeset-demo` (then `ARGS=tui` to look at it in the TUI,
`ARGS=clean` to remove it).

## The shape

```text
change set "add-user-tier"                      ~/.local/share/ptyro/changesets/add-user-tier/
├─ lead   claude  · the root · reads every member ├─ CHANGESET.md   the brief (generated)
├─ api    codex   · ./api   ptyro/add-user-tier   ├─ CONTRACT.md    the seams (the lead's, versioned)
├─ sdk    claude  · ./sdk   depends on api        ├─ api/ sdk/ web/ the members' worktrees
└─ web    claude  · ./web   depends on api, sdk   └─ .ptyro/        overlay record
```

- **Members** are projects (`ptyro project add`) of one workspace, all under **one profile**
  (a set mixing accounts is refused). Each gets an ordinary ptyro worktree on the shared branch
  name, **side by side in the set's root**, so a relative dependency like Go's `replace
  example.com/api => ../api` already builds against the sibling's in-progress code.
- **The order** comes from `--depends sdk:api --depends web:api+sdk`, a template, or the
  manifests (`go.mod`, `package.json`, `Cargo.toml`, `pyproject.toml`: B depends on A when B
  requires what A provides). Cycles are refused.
- **Layouts:** `lead` (the default: a lead agent plus one agent per member, each member's agent
  from `--agent api=codex`, the template, or the project's `default_agent`), `solo` (one agent
  for every member) and `manual` (no agents: the linked worktrees and the lockstep git).

## The contract

Multi-repo changes fail at the seams, so the lead's first job is `CONTRACT.md`: endpoints and
payloads, types, events, versions, flags, and the merge order. Each `##` section can name the
members it concerns:

```markdown
## sdk interface
affects: sdk

IsPro(user) bool
```

- Every save is a **revision** (r1, r2, …) with its diff, recorded when the lead's turn ends,
  or with `ptyro cs contract SET submit`.
- **You approve r1** (`ptyro cs contract SET approve`, `reject "why"`, the inbox, or your phone)
  before any member starts. Then each member agent starts with its part and the contract.
- **Revisions propagate:** members a changed section's `affects:` line names hear at once (their
  turn is interrupted); the rest hear at their next idle. Without an `affects:` line, the
  members the section's title names count; a section for "everyone" concerns all.
- `ptyro cs contract SET show|history|diff [--a N --b M]|edit|revise FILE [--diff] [--affects …]`.
  `diff FILE` compares the latest revision with a file (say, a regenerated OpenAPI spec) and
  exits 1 on drift: a good contract check.

## Who may do what

| Role | Reads | Writes | Ships |
|---|---|---|---|
| Lead | every member, the ledger | the root's files (`CONTRACT.md`, notes); never member code | no |
| Member | its worktree and its siblings | only its own worktree | no (it commits locally) |
| You | everything | everything | `cs commit`, `push`, `pr`, `land`, `restore`, `rm` |

Writes outside a role's fence are **denied** for agents with hooks (Claude, Codex), with the
reason and the command to ask instead. For anything hooks can't see (a lead committing in a
member with `git -C`), `cs status` flags the member as changed out of fence. Shipping and
structure (`commit`, `push`, `pr`, `land`, `restore`, `add`, `drop`, `rm`, `overlay`) are refused
from any agent.

## Talking: the ledger

Agents in a set get `$PTYRO_CS` (the set's name):

```bash
ptyro cs msg "$PTYRO_CS" @lead "does tier ever come back null?" --kind question
ptyro cs blocked "$PTYRO_CS" api "I need the Tier type first"
ptyro cs ready "$PTYRO_CS" "tier is in, tests pass"
```

- Messages are delivered between turns, never into the middle of one. `--interrupt` stops a
  working agent's turn first.
- `blocked` shows the member as blocked on api, tells the lead, and puts the set in your inbox.
  When api says `ready`, every member blocked on it is woken.
- A member whose agent exited (the lead stopped it, say) is resumed when mail arrives for it.
- `ptyro cs log SET [--follow]` is the set's memory: revisions, approvals, readiness, failures,
  checkpoints, commits, landings, and every message.

## Building against siblings: the overlay

`ptyro cs new … --overlay` (or `ptyro cs overlay SET [auto|go|rust|node|python|compose|off]
[--member M]`) wires members to their siblings' in-progress code. Nothing is ever committed:

| Ecosystem | What ptyro writes |
|---|---|
| Go | `go.work` in the root, using every Go member |
| Rust | `.cargo/config.toml` in the root with `[patch]` path overrides for sibling crates |
| Node | `node_modules/<pkg>` → the sibling (the installed copy is set aside and comes back on `off`) |
| Python | a `.pth` in the member's `.venv` putting the sibling on the path |
| Compose | a git-excluded `compose.override.yml` building sibling services from their worktrees |

**Checks:** a template's `integration = ["docker compose up -d", "pnpm -C web e2e"]` run in the
root, and `contract_checks = { api = "make openapi && ptyro cs contract add-user-tier diff
openapi.yaml" }` run in a member. `ptyro cs check SET` runs them all (exit 1 on a failure); they
also run when every member is ready, and before landing.

## Undo across repositories

```bash
ptyro cs checkpoint SET "before the refactor"
ptyro cs checkpoints SET
ptyro cs restore SET 3            # every member back to #3, at once
```

A set checkpoint snapshots every member (HEAD plus uncommitted work, untracked included) under
`refs/ptyro/cs/<set>/<n>`. One is taken before every contract revision and before landing.
`restore` refuses while an agent is working, and refuses if a member changed since the last
checkpoint (it names them); `--force` checkpoints the present first, so nothing is lost.

## Shipping

- **`ptyro cs commit SET -m MSG`** commits every member with changes, in dependency order, with
  trailers linking the repositories:

  ```text
  Add user tier

  Change-Set: add-user-tier
  Change-Set-Members: api@a1b2c3d sdk@d4e5f6a
  ```

  (each commit names the sibling commits it was built against.)
- **`ptyro cs push SET`** pushes in dependency order. A failure stops it and says exactly who
  pushed and who didn't; run it again to resume.
- **`ptyro cs pr SET [--draft]`** opens one PR per member, each with a table of its siblings,
  their CI and the merge order, kept current as members land.
- **`ptyro cs land SET [--yes]`** is the merge train: the integration checks, a checkpoint, then
  each member in order through a gate (asked here on a terminal; otherwise in the inbox or on
  your phone), waiting for its PR's CI (or running its contract check when it has no PR), then
  merging. A failure stops the train with a precise report, e.g.:

  > the merge train stopped at sdk: CI failed on PR #12: build. Landed: api; not landed: sdk, web

  Fix it and run `cs land` again: it resumes where it stopped.
- **`ptyro cs rm SET [--keep M] [--force]`** removes every member's worktree safely: one with
  unshipped work blocks the whole set unless you keep it. `cs add SET PROJECT` and `cs drop SET
  MEMBER` change membership.

## When things go wrong

`ptyro cs status` never says "done" unless every member landed. Each failure is a state with a
suggested action:

| State | Means | What to do |
|---|---|---|
| `land_stopped` | the train stopped at a member | fix it, `cs land` again |
| `member_conflict` | rebasing a member onto its base conflicts | resolve, commit, `cs land` |
| `member_failed` | a member's agent errored or crashed | look at it; it clears on the next good turn |
| `out_of_order` | someone merged a member before its dependency | land the dependencies, check the base |
| `stale_contract` | a member said ready against a revision a later one affects | ask it to re-check |
| `check_failed` / `integration_failed` | a contract or integration check failed | fix, `cs check` |
| `missing_worktree` | a member's directory is gone | `cs drop`, then `cs add` |
| `out_of_fence` | a member changed while its agent was idle (another agent wrote there) | revert or move the change |
| `no_remote` | a member's repository has no remote | add one, `cs push` |
| `blocked_on` | a member waits for a sibling | it wakes when that one is ready |

## Templates

Recurring sets get a template in `~/.config/ptyro/changesets/NAME.toml`, or in a "hub"
repository's `.ptyro/changesets/` (trusted per template, like repo recipes: `cs new --trust FP`,
or the CLI asks):

```toml
name = "api-sdk-web"
description = "the usual trio"
members = [
  { project = "api", agent = "codex" },
  { project = "sdk", depends_on = ["api"], overlay = "node" },
  { project = "web", depends_on = ["api", "sdk"], overlay = "node" },
]
lead = { agent = "claude" }
gates = ["contract", "push", "land"]     # where you must approve (default: all three)
integration = ["docker compose up -d", "pnpm -C web e2e"]
contract_checks = { api = "make openapi && ptyro cs contract api-sdk-web diff openapi.yaml" }
```

`ptyro cs templates` lists them (with mistakes, all at once); `ptyro cs new NAME --template
api-sdk-web` uses one.

## The lead's tools

A set's lead gets `ptyro mcp` with `cs_status`, `cs_assign`, `cs_send`, `cs_contract_get`,
`cs_contract_submit`, `cs_contract_revise`, `cs_member_diff`, `cs_integration_check`,
`cs_checkpoint`, `cs_request_gate` and `cs_log`, plus the mailbox. There is deliberately no push
or merge tool: shipping is your train.

## In the TUI

A set is a node at the top of the sidebar (`◆ add-user-tier working 2/3 ready · r2`, amber when
it wants you), with its lead under it. `Ctrl-Q S` (or `⏎` on the node) opens the set view:
one swimlane per member and the lead, the open gate (`1`–`9` answer it), the issues with their
actions, and the ledger as a timeline where messages are arrows between lanes. `m` messages the
selected lane, `⏎` shows its agent, `c` runs the checks, `p` takes a checkpoint.

## Tips from the first real run

- Every new worktree is a new folder to Claude, so each member's first start shows Claude's
  folder-trust question. It shows up as blocked in ptyro; answer it with `ptyro answer
  SET-MEMBER allow` (or `a` in peek) and the member carries on.
- Agents ask for permission a lot in a fresh set (13 prompts in 8 minutes in the experiment):
  pair your phone (`docs/remote.md`) or give the agents an allow-list.
- Small changes rarely need messages: a precise contract carried a three-repo change on its own.
