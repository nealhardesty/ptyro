---
name: releaseme
description: Ship the working tree as a ptyro release through the normal rules. Bump the version if needed, then branch, commit, push and open a PR. Wait for the PR's CI and fix anything the change broke until it is green, then merge, tag vX.Y.Z, and watch master's CI and the Release workflow, fixing new breakage the same way. Only the user starts it, by typing /releaseme, optionally with patch|minor|major|X.Y.Z and a commit subject.
disable-model-invocation: true
context: fork
allowed-tools: Bash(git *) Bash(gh *) Bash(make *) Read Edit Write Grep Glob
---

<!-- One skill for every agent: it lives in `.agents/skills/` (the shared Agent Skills location;
Codex and others read it), and `.claude/skills/releaseme` is a symlink to it. `context: fork`,
`allowed-tools` and `disable-model-invocation` are Claude Code's keys (run it as a background
subagent, pre-allow git/gh/make, and only start it when the user asks); other agents ignore them. -->

# /releaseme: commit → PR → green CI → merge → tag → watch

The user's arguments, if any: `$ARGUMENTS`. A bump level (`patch`, `minor`, `major` or `X.Y.Z`;
default `patch`) and/or a one-line summary to use as the commit subject.

The user typed `/releaseme`, which authorizes every step below. Run them without stopping to
ask. **Follow the branch rules: never `--admin`, never merge a red or pending PR, never push
to `master` directly, never move or delete a tag.** Stop and report when a rule below says
to, or when a step fails in a way this file doesn't cover.

**Other agents may be committing at the same time.** master can move under you at any point,
and another agent may bump the version or push the tag you were about to use. Rebasing onto
the latest master is routine here, not an exception: do it whenever §R says to, without asking.

**Every push names its remote and branch** (`git push origin <branch>`, `git push origin
refs/tags/vX.Y.Z`), never a bare `git push`. With `push.default = matching` in someone's git
config, a bare push also sends every other local branch. One bare `--force-with-lease` once
moved master back to a stale local copy and dropped two merged PRs (2026-11-24). Never push
`master` at all: it changes only through merged PRs.

Everything that waits (CI, a run to appear) runs as a **background** command, so you're told
when it ends: in Claude Code, Bash with `run_in_background: true`; elsewhere your agent's
background or async shell, or `cmd &` then `wait`. Never poll in a tight loop.

## 0. Preflight

- `git status --short`: no changes and nothing ahead of `origin/master` means nothing to
  release. Say so and stop.
- `gh auth status` must pass.
- **Baseline:** note master's latest CI result before touching anything:
  `gh run list --workflow ci.yml --branch master --limit 1 --json conclusion,headSha,databaseId`.
  If it failed, keep its failing jobs (`gh run view <id> --json jobs`). Those failures are
  *pre-existing*, not ours (see §4).
- Read the version: `sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1`. If it equals
  the latest tag (`git describe --tags --abbrev=0`), run `make bump V=<level>` (AGENTS.md:
  every feature bumps). If it's already newer, keep it. Call the version `X.Y.Z` from here on.
- `git rev-parse -q --verify refs/tags/vX.Y.Z` must find nothing, else stop.
- Run `make fmt` and `make lint` locally first. Each failure caught here saves a CI round trip.
  Fix what they report, in what this change touched.

## R. Rebase onto the latest master (whenever master moved)

Run this before the first push, whenever `gh pr view <pr> --json mergeStateStatus` says
`BEHIND` or `DIRTY`, whenever a merge is refused because the branch is out of date, and before
every merge attempt:

1. `git fetch origin`. If `git merge-base --is-ancestor origin/master HEAD` succeeds, you're up
   to date: skip the rest.
2. `git rebase origin/master`.
3. **Conflicts:** resolve each one by keeping both intents: theirs is already on master and must
   survive, and yours goes on top. Then `git add` and `git rebase --continue`.
   - **Version (`Cargo.toml`, `Cargo.lock`):** take master's version, then bump again past it
     (`make bump V=patch`, or the asked level if master's version is still below it). A
     release that lands after another must have a higher version. Recompute `X.Y.Z`, and
     reword the commit subject's `; vX.Y.Z` if it named one (`git commit --amend` on that
     commit, during the rebase).
   - **Docs and lists** (SPEC, README, docs/parity.md, ROADMAP): keep both entries.
   - **Code:** if you can't tell how the two changes combine, `git rebase --abort`, leave the PR
     as it is, and report the conflicting files. Never resolve by dropping someone else's work.
4. Re-run `make fmt` and `make lint` (other agents' code plus yours can break together), and
   fix what's ours.
5. A branch you already pushed: `git push --force-with-lease=<branch>:<sha it had> origin
   <branch>`, naming the branch twice: in the lease and in the refspec. That is the **only**
   force-push allowed, only on this PR's own branch, and the lease refuses if someone else
   pushed to it.
   Then CI runs again on the new head: back to §2. Earlier green results don't count after a
   rebase.

## 1. Commit and open the PR

- Branch: `git checkout -b releaseme/vX.Y.Z` (or keep the current branch if it isn't `master`).
- Commit subject: the user's summary if given; otherwise write one in the repo's style
  (`feat: …; vX.Y.Z` / `fix: …; vX.Y.Z`, see `git log --oneline -10`) from the diff. End the
  message with whatever attribution your agent's own instructions ask for in commits (AGENTS.md
  forbids a co-authored line only in *code comments*).
- Rebase onto the latest master (§R) first, then `git push -u origin <branch>`.
- `gh pr create --base master --title "<subject>" --body "<what changed, why, how it was
  tested>"`, the body ending with the attribution your agent's instructions ask for in PRs, if
  any.
- **Put this computer on the code just pushed:** `make daemon-upgrade` (in the background, while
  CI starts). It runs `make install`, then `ptyro daemon upgrade`, so the running daemon moves
  onto the new binary live and every session keeps running. Note the commit it built
  (`git rev-parse HEAD`). If it fails, report the error and carry on: the release doesn't
  depend on it. Never `ptyro daemon stop` or `restart` instead, because that ends sessions,
  maybe this one.

## 2. Wait for the PR's CI to go green

- In the background: `gh pr checks <pr> --watch --fail-fast`. Right after a push the checks may
  not be listed yet; if it says there are none, wait about 20 s in the background and run it
  again.
- **All pass:** go to §3.
- **Any fail:** go to §4 with the PR branch, then come back here. Each fix is a new commit on
  the same branch (no force-push, except after a rebase, §R), so the PR's history shows what
  CI made us change.
- **Give up after 3 fix rounds** on the PR, or if a failure isn't ours and isn't a flake (§4).
  Leave the PR open, and report what fails and why. The user decides.

## 3. Merge, tag

- `gh pr view <pr> --json mergeStateStatus,reviewDecision`. Merge only when the checks are
  green and nothing else blocks it. If a review is required and missing, stop and say so: the
  PR is ready and waiting for a reviewer.
- Rebase onto the latest master (§R). If that changed anything, back to §2: CI has to pass on
  what will actually merge.
- `gh pr merge <pr> --merge --delete-branch` (no `--admin`). If it's refused because master
  moved in the meantime, §R again, then §2. Try at most 3 times, then report that master is
  moving too fast.
- `git checkout master && git pull --ff-only`.
- **Tag the merge commit, not whatever master is now:** `sha=$(gh pr view <pr> --json
  mergeCommit -q .mergeCommit.oid)`. Check that `Cargo.toml` at `$sha` says `X.Y.Z`
  (`git show $sha:Cargo.toml`).
- If `vX.Y.Z` now exists on the remote (`git ls-remote --tags origin vX.Y.Z`), another agent
  took it. Never move it. Report the clash: your change is merged, but it needs a new version,
  which is a follow-up PR that bumps it (§1 to §3).
- `git tag vX.Y.Z $sha && git push origin refs/tags/vX.Y.Z`. `release.yml` runs on `v*` tags. If the push
  is refused because the tag appeared in the meantime, handle it as in the clash above.

## 4. Fixing what broke

For each failed run: `gh run view <id> --log-failed`, and find the real cause before changing
anything.

Decide whose it is:
- **Newly broken (ours):** it fails now but passed in the baseline, or it's in code or tests
  this change touched. Fix it.
- **Pre-existing:** it failed the same way in master's baseline run. Don't fix it as part of
  this release. Note it in the report, and don't let it block the merge decision on its own:
  if every failing job is pre-existing, stop and ask the user whether to merge anyway, since
  that would be the only reason to need `--admin`.
- **Flake:** it has nothing to do with the change, `target/flakes.txt` or the log names a flake,
  or it's a timeout or network error. Re-run it once: `gh run rerun <id> --failed`. Twice in a
  row means it's real; classify it again.

To fix:
1. Reproduce it locally with the matching make target (`make fmt-check`, `make lint`,
   `make test`, `make web-check`, `make parity`, `make deny`…), never raw cargo. CI calls the
   same targets.
2. Make the smallest fix that keeps the change's intent. Never weaken, skip or delete a test,
   or loosen a lint, to get green. If the test is right and the change is wrong, fix the change.
3. Run the target again until it passes locally. Also run `make fmt`, plus `make lint` if Rust
   changed.
4. Commit (`fix: <what CI caught>`) and push to the PR branch, then back to §2.

## 5. After the merge: master and the release

Start both watches in the background, at the same time:
- **master CI:** `gh run list --workflow ci.yml --commit $sha --json databaseId` (the merge
  commit: other agents' later commits have runs of their own), then `gh run watch <id> --exit-status`.
- **Release:** `gh run list --workflow release.yml --json databaseId,headBranch` (headBranch
  is `vX.Y.Z`), then `gh run watch <id> --exit-status`.

If a run isn't listed yet, wait about 20 s in the background and look again.

- **master CI breaks, but the PR was green** (a merge interaction, or a platform job): classify
  it as in §4. If it's newly broken, fix it on a new branch `fix/<what>` and take that through
  §1, §2 and §4 as its own PR (merge it once green; no new tag unless the shipped binary is
  affected, in which case bump the patch version and tag that).
- **Release fails:** never delete or move `vX.Y.Z`. Find the cause. If it needs a code fix, take
  that fix through a PR as above with the next patch version, and tag it once it's merged
  green. Report both versions.

## 6. Report

Keep it short:
- the PR URL, and how many fix rounds it took (what CI caught, if anything)
- rebases onto a moving master, any conflicts resolved, and a version re-bumped because of them
- PR checks, master CI and Release: each ✓ or ✗, with the run URL
- the release URL (`gh release view vX.Y.Z --json url -q .url`)
- pre-existing failures you left alone, and anything still broken, with the exact error
- which commit the local daemon runs. If fixes or rebases changed the code after the
  `make daemon-upgrade` in §1, run it once more on master after the merge (at `$sha`, before
  the report), so this computer runs what shipped
