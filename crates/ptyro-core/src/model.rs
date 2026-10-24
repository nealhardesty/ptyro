//! Projects, worktrees, workspaces and profiles as the daemon reports them (SPEC §2.1, §10, §14.2).
//! Plain data, shared by the store, the wire and the clients.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::config::MatchRules;

/// A new entity id: a lowercase ULID, like session ids.
pub fn new_id() -> String {
    ulid::Ulid::generate().to_string().to_ascii_lowercase()
}

/// Shortest suffix of `id` (≥ 4 characters) that no other id in `all` shares.
pub fn short_suffix(id: &str, all: &[&str]) -> String {
    for len in crate::ids::MIN_HANDLE..id.len() {
        let suffix = &id[id.len() - len..];
        if !all.iter().any(|o| *o != id && o.ends_with(suffix)) {
            return suffix.to_owned();
        }
    }
    id.to_owned()
}

/// `My Repo.git` → `my-repo`: lowercase `[a-z0-9-]`, at most 40 characters.
pub fn slugify(s: &str) -> String {
    let s = s.strip_suffix(".git").unwrap_or(s);
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(40).collect();
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() { "repo".into() } else { out }
}

const BRANCH_VARS: &[&str] = &["name", "user", "date", "issue", "slug"];

/// Placeholders in a branch template must be known ones.
pub fn check_branch_template(t: &str) -> Result<(), String> {
    let mut rest = t;
    while let Some(start) = rest.find('{') {
        let end = rest[start..].find('}').ok_or_else(|| format!("unclosed {{ in {t:?}"))?;
        let var = &rest[start + 1..start + end];
        if !BRANCH_VARS.contains(&var) {
            return Err(format!("unknown placeholder {{{var}}} (use {BRANCH_VARS:?})"));
        }
        rest = &rest[start + end + 1..];
    }
    if t.trim().is_empty() {
        return Err("must not be empty".into());
    }
    Ok(())
}

/// Fill a branch template (SPEC §10.3). Missing variables become empty and the doubled
/// separators they leave are tidied up.
pub fn render_branch(t: &str, vars: &BTreeMap<&str, String>) -> String {
    let mut out = t.to_owned();
    for v in BRANCH_VARS {
        out = out.replace(&format!("{{{v}}}"), vars.get(v).map_or("", String::as_str));
    }
    while out.contains("//") || out.contains("--") || out.contains("/-") || out.contains("-/") {
        out = out.replace("//", "/").replace("--", "-").replace("/-", "/").replace("-/", "/");
    }
    out.trim_matches(['/', '-']).to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectInfo {
    pub id: String,
    pub slug: String,
    /// The main checkout.
    pub root: PathBuf,
    #[serde(default)]
    pub default_branch: Option<String>,
    #[serde(default)]
    pub remote_url: Option<String>,
    /// The profile this project is pinned to (SPEC §10.10).
    #[serde(default)]
    pub pin: Option<String>,
    pub created_ms: u64,
    #[serde(default)]
    pub last_used_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeState {
    Creating,
    Ready,
    /// The directory is gone (never pruned automatically).
    Missing,
    Removed,
    Failed,
}

impl WorktreeState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Creating => "creating",
            Self::Ready => "ready",
            Self::Missing => "missing",
            Self::Removed => "removed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBlock {
    pub base: u16,
    pub count: u16,
}

impl PortBlock {
    pub fn contains(&self, p: u16) -> bool {
        p >= self.base && u32::from(p) < u32::from(self.base) + u32::from(self.count)
    }
}

/// Live repository state of a worktree (SPEC §10.9).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoStatus {
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub upstream: Option<String>,
    /// Against the upstream.
    pub ahead: u32,
    pub behind: u32,
    /// Against the project base (e.g. `origin/main`).
    pub base_ahead: u32,
    pub base_behind: u32,
    pub staged: u32,
    pub modified: u32,
    pub untracked: u32,
    pub conflicts: u32,
    /// Lines added and removed since the merge base with the project base, working tree included.
    pub insertions: u32,
    pub deletions: u32,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    pub at_ms: u64,
}

impl RepoStatus {
    pub fn dirty(&self) -> u32 {
        self.staged + self.modified + self.untracked + self.conflicts
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub id: String,
    /// Project id.
    pub project: String,
    pub project_slug: String,
    /// Friendly name; `@root` for the main checkout.
    pub name: String,
    pub path: PathBuf,
    #[serde(default)]
    pub branch: Option<String>,
    #[serde(default)]
    pub base: Option<String>,
    pub is_root: bool,
    /// Fixed when the worktree is created; `None` for `@root` (SPEC §10.10).
    #[serde(default)]
    pub profile: Option<String>,
    pub state: WorktreeState,
    #[serde(default)]
    pub ports: Option<PortBlock>,
    pub created_ms: u64,
    #[serde(default)]
    pub removed_ms: Option<u64>,
    #[serde(default)]
    pub last_used_ms: Option<u64>,
    #[serde(default)]
    pub status: Option<RepoStatus>,
    /// `git worktree lock`ed by ptyro while sessions run in it.
    #[serde(default)]
    pub locked: bool,
    /// Its pull request, when `gh` found one (M6.5).
    #[serde(default)]
    pub pr: Option<PrInfo>,
    /// What it's for, for people to read (UX1.24): the prompt it was made for, a PR's or an
    /// issue's title, or `--title`. The `name` is its address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The commit ptyro integrated into the main branch (`wt integrate`, finish): its work is
    /// in main for as long as the worktree stays there, clean.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrated: Option<String>,
}

impl WorktreeInfo {
    /// `myrepo/brisk-rook`, `myrepo/@root`.
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.project_slug, self.name)
    }

    /// Its work has shipped, so cleaning it up loses nothing: its PR merged, or ptyro
    /// integrated it into the main branch and nothing changed since.
    pub fn shipped(&self) -> bool {
        if self.is_root {
            return false;
        }
        if self.pr.as_ref().is_some_and(|p| p.state == "MERGED") {
            return true;
        }
        match (&self.integrated, &self.status) {
            (Some(at), Some(s)) => {
                s.dirty() == 0
                    && s.head.as_deref().is_some_and(|h| !h.is_empty() && at.starts_with(h))
            }
            _ => false,
        }
    }
}

pub const ROOT_WORKTREE: &str = "@root";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    pub name: String,
    pub profile: String,
    /// `all | badge | mute` (SPEC §9.6, used from M4).
    pub notify: String,
    /// Project ids, in sidebar order.
    pub projects: Vec<String>,
    pub created_ms: u64,
}

pub const MAIN_WORKSPACE: &str = "main";
pub const DEFAULT_PROFILE: &str = "default";

/// A profile as clients see it: never any secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub color: Option<String>,
    /// Defined in the config (only `default` can be implicit).
    pub defined: bool,
    pub isolate: bool,
    /// Agent (or `gh`) → the config directory sessions get.
    pub dirs: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub git_email: Option<String>,
    #[serde(default)]
    pub matches: MatchRules,
    /// Variable names a session gets (values never leave the daemon for secrets).
    pub vars: Vec<String>,
}

/// Why a session got its profile (SPEC §10.10), shown by the wizard and `ptyro new`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileReason {
    Explicit,
    Worktree,
    Pinned,
    Workspace,
    Default,
}

impl ProfileReason {
    pub fn label(self) -> &'static str {
        match self {
            Self::Explicit => "chosen",
            Self::Worktree => "the worktree's profile",
            Self::Pinned => "pinned by project",
            Self::Workspace => "the workspace's profile",
            Self::Default => "default",
        }
    }
}

/// A new worktree (SPEC §10.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSpec {
    /// Project address (slug, id suffix or a path inside it).
    pub project: String,
    /// Default: a friendly random name.
    #[serde(default)]
    pub name: Option<String>,
    /// Default: the branch template (`ptyro/{name}`).
    #[serde(default)]
    pub branch: Option<String>,
    /// Check out the existing branch `branch` instead of creating one.
    #[serde(default)]
    pub existing: bool,
    /// Check out GitHub pull request N.
    #[serde(default)]
    pub pr: Option<u32>,
    /// Start from a GitHub issue (number or URL): branch and first prompt come from it (M6.6).
    #[serde(default)]
    pub issue: Option<String>,
    /// Default: the project's base (`.ptyro.toml`, else `origin/<default branch>`).
    #[serde(default)]
    pub base: Option<String>,
    /// `None` follows `worktrees.fetch`.
    #[serde(default)]
    pub fetch: Option<bool>,
    /// Run the setup script (when trusted).
    #[serde(default = "yes")]
    pub setup: bool,
    /// The scripts fingerprint the user just approved (the TUI's trust dialog).
    #[serde(default)]
    pub trust: Option<String>,
    /// Profile for the worktree (and its sessions); `None` resolves (SPEC §10.10).
    #[serde(default)]
    pub profile: Option<String>,
    #[serde(default)]
    pub override_pin: bool,
    #[serde(default)]
    pub confirmed: bool,
    /// Start this agent in it (`claude`, `codex@work`), after setup per `start_agents`.
    #[serde(default)]
    pub agent: Option<String>,
    /// Its first prompt.
    #[serde(default)]
    pub prompt: Option<String>,
    /// Size for its sessions.
    #[serde(default)]
    pub size: Option<crate::TermSize>,
    /// Started from the scratchpad item on this line (NOTE1.3): the item is tagged with the
    /// worktree, and ticked once it is merged or cleaned up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pad_item: Option<u32>,
    /// The client's SSH agent and display variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub workspace: Option<String>,
    /// For its agent: verified-done (M7.9) and lead (M7.6), as in [`crate::session::SpawnSpec`].
    #[serde(default)]
    pub verify: Option<String>,
    #[serde(default)]
    pub lead: Option<serde_json::Value>,
    /// Start the agent in structured mode (M10).
    #[serde(default)]
    pub structured: bool,
    /// Put the worktree here instead of `worktrees.location` (a change set's member, M11).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<std::path::PathBuf>,
    /// Its title (UX1.24); default: the prompt, else the PR's or issue's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Move the main checkout's uncommitted changes into it (UX1.10): it starts at the main
    /// checkout's HEAD, the changes go with it, and a copy stays at `refs/ptyro/taken/<name>`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub take_changes: bool,
}

/// What ptyro found a project needs, for its first worktree (UX1.11).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSuggestions {
    pub project: String,
    /// A setup script, from a lockfile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    /// The file that suggested it (`pnpm-lock.yaml`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// Ignored files at the root to copy into each worktree (`.env`).
    #[serde(default)]
    pub copy: Vec<String>,
    /// Offered already (taken or declined): never again on its own.
    #[serde(default)]
    pub offered: bool,
}

impl SetupSuggestions {
    pub fn is_empty(&self) -> bool {
        self.setup.is_none() && self.copy.is_empty()
    }
}

fn yes() -> bool {
    true
}

/// Lifecycle scripts as shown before trusting them (SPEC §10.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptsInfo {
    pub project: String,
    /// The exact text that would run.
    pub text: String,
    pub fingerprint: String,
    pub trusted: bool,
    /// Script names: `setup`, `archive`, `after_turn`, `run:<name>` (default run script first).
    pub names: Vec<String>,
}

/// `ptyro wt rm` options (SPEC §10.8).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveOpts {
    /// Remove even with uncommitted, untracked, unpushed or stashed work (after confirmation).
    #[serde(default)]
    pub force: bool,
    /// Stop sessions running in it first.
    #[serde(default)]
    pub stop_sessions: bool,
    #[serde(default)]
    pub keep_branch: bool,
}

/// A `ptyro gc` candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GcEntry {
    pub worktree: WorktreeInfo,
    pub reasons: Vec<String>,
}

/// An archived worktree (UX1.12): its branch kept, its files (committed or not) in a snapshot
/// commit under `refs/ptyro/archive/`, so `wt restore` brings it back as it was.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArchiveEntry {
    pub project: String,
    pub project_slug: String,
    pub name: String,
    pub branch: String,
    /// The ref holding the snapshot (`refs/ptyro/archive/<name>`).
    pub snapshot: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub archived_ms: u64,
}

/// What a diff is taken against (SPEC §11.5 inspector, M6.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffAgainst {
    /// The merge base with the project base (`origin/main`): everything this worktree changed,
    /// committed or not.
    #[default]
    Base,
    /// The branch's upstream: what isn't pushed yet (and uncommitted work).
    Upstream,
    /// The state when the agent's last turn started: what that turn did.
    LastTurn,
    /// `HEAD`: uncommitted work only.
    Head,
}

impl DiffAgainst {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "base" => Self::Base,
            "upstream" => Self::Upstream,
            "last-turn" | "last_turn" | "turn" => Self::LastTurn,
            "head" | "uncommitted" => Self::Head,
            _ => return None,
        })
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Base => "base",
            Self::Upstream => "upstream",
            Self::LastTurn => "last turn",
            Self::Head => "uncommitted",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Base => Self::Upstream,
            Self::Upstream => Self::LastTurn,
            Self::LastTurn => Self::Head,
            Self::Head => Self::Base,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LineKind {
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffLine {
    pub kind: LineKind,
    #[serde(default)]
    pub old: Option<u32>,
    #[serde(default)]
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hunk {
    /// `@@ -1,4 +1,6 @@ fn main()`
    pub header: String,
    pub old_start: u32,
    pub new_start: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffFile {
    pub path: String,
    /// For renames.
    #[serde(default)]
    pub old_path: Option<String>,
    /// `added`, `deleted`, `modified`, `renamed`, `untracked`.
    pub status: String,
    pub added: u32,
    pub removed: u32,
    #[serde(default)]
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffDoc {
    pub against: DiffAgainst,
    /// The commit compared with (the merge base, the upstream, …).
    #[serde(default)]
    pub from: Option<String>,
    pub files: Vec<DiffFile>,
    /// Files or hunks left out because the diff was too big.
    #[serde(default)]
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub sha: String,
    pub subject: String,
    pub author: String,
    pub at_ms: u64,
    pub added: u32,
    pub removed: u32,
    pub files: u32,
}

/// One CI check of a PR (`gh pr checks`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub name: String,
    /// `pass`, `fail`, `pending`, `skipping`, `cancel`.
    pub bucket: String,
    #[serde(default)]
    pub link: Option<String>,
    #[serde(default)]
    pub workflow: Option<String>,
}

/// A worktree's pull request (M6.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrInfo {
    pub number: u32,
    pub url: String,
    pub title: String,
    /// `OPEN`, `CLOSED`, `MERGED`.
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    /// `APPROVED`, `CHANGES_REQUESTED`, `REVIEW_REQUIRED`.
    #[serde(default)]
    pub review: Option<String>,
    #[serde(default)]
    pub checks: Vec<CheckRun>,
    /// Auto-merge is on.
    #[serde(default)]
    pub automerge: bool,
    /// ptyro opened it (its PR-event loop watches it).
    #[serde(default)]
    pub ours: bool,
    pub at_ms: u64,
}

impl PrInfo {
    /// (passed, failed, pending).
    pub fn tally(&self) -> (usize, usize, usize) {
        let n = |b: &[&str]| self.checks.iter().filter(|c| b.contains(&c.bucket.as_str())).count();
        (n(&["pass"]), n(&["fail", "cancel"]), n(&["pending"]))
    }
}

/// A line comment on a worktree's diff, waiting to be sent to its agent (M6.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: u32,
    pub path: String,
    /// The line in the new file (the old one for a removed line).
    pub line: u32,
    /// The line's text, so the agent sees what was meant even after edits.
    #[serde(default)]
    pub code: Option<String>,
    pub text: String,
    pub at_ms: u64,
}

/// A worktree waiting for review, ranked (M6.9, R-ORC-24).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewItem {
    pub worktree: WorktreeInfo,
    #[serde(default)]
    pub session: Option<String>,
    /// Higher first.
    pub score: i32,
    /// Why it ranks where it does ("checks pass", "small diff", "+12 −3 in 2 files").
    pub reasons: Vec<String>,
    /// What the agent last said, briefly.
    #[serde(default)]
    pub last_message: Option<String>,
    /// Finished this long ago.
    pub waiting_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("My Repo.git"), "my-repo");
        assert_eq!(slugify("--x__y--"), "x-y");
        assert_eq!(slugify("..."), "repo");
    }

    #[test]
    fn branch_templates() {
        assert!(check_branch_template("ptyro/{name}").is_ok());
        assert!(check_branch_template("{who}/x").is_err());
        assert!(check_branch_template("{name").is_err());
        let mut v = BTreeMap::new();
        v.insert("name", "brisk-rook".to_owned());
        assert_eq!(render_branch("ptyro/{name}", &v), "ptyro/brisk-rook");
        assert_eq!(render_branch("{user}/{issue}-{name}", &v), "brisk-rook");
        v.insert("user", "neal".to_owned());
        assert_eq!(render_branch("{user}/{issue}-{name}", &v), "neal/brisk-rook");
    }

    #[test]
    fn suffixes_are_unique() {
        let ids: Vec<String> = (0..50).map(|_| new_id()).collect();
        let all: Vec<&str> = ids.iter().map(String::as_str).collect();
        for id in &ids {
            let s = short_suffix(id, &all);
            assert_eq!(all.iter().filter(|o| o.ends_with(&s)).count(), 1);
        }
    }
}
