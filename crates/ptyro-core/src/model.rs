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

/// `My Repo.git` → `my-repo`: [`crate::names::fold`] to lowercase `[a-z0-9-]`, at most 40
/// characters, `""` when nothing is left (the caller says what stands in). Narrower than a
/// name on purpose: a project's slug is derived once, stored, and is the folder its worktrees
/// live under (`<data>/worktrees/<slug>/`); keeping this fold as it always was
/// means re-adding a repository finds them again. Words taken from text (a branch from a
/// prompt) use it too. A name someone gives never goes through it (SPEC §2.3).
pub fn slugify(s: &str) -> String {
    let s = s.strip_suffix(".git").unwrap_or(s);
    crate::names::fold(s, |c| c.is_ascii_alphanumeric(), 40)
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
    /// The machine it lives on, when that is another one this daemon mirrors (MM1.B, SPEC
    /// §16.4b); `None`: here. Its names then carry the machine (`laptop2:work`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
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
    /// Its main checkout is gone, and the same repository was found here: it seems to have moved
    /// (1.53, SPEC §10.1). `ProjectMove` takes it there; never stored, the daemon looks again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<PathBuf>,
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
    /// Lines added and removed since the merge base with the project base, working tree included;
    /// untracked text files' lines count as added.
    pub insertions: u32,
    pub deletions: u32,
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    /// Commits on no remote (and not in the base), counted for a branch with no upstream; `None`
    /// when git wasn't asked or the repository has no remote at all (UX2.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpushed: Option<u32>,
    /// It has work of its own, all of it committed and on a remote (UX2.2).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub pushed: bool,
    pub at_ms: u64,
}

/// What is left to do with a worktree's work, the same everywhere it is shown (UX2.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Left {
    /// Changed files not committed yet.
    pub uncommitted: u32,
    /// Commits not pushed: ahead of the upstream, or on no remote.
    pub unpushed: u32,
    /// There are commits, and the branch has no upstream yet.
    pub not_pushed: bool,
    /// Work of its own, committed and pushed: nothing left to do.
    pub pushed: bool,
}

impl RepoStatus {
    pub fn dirty(&self) -> u32 {
        self.staged + self.modified + self.untracked + self.conflicts
    }

    /// What is left to do: uncommitted changes, unpushed commits, or nothing (pushed).
    pub fn left(&self) -> Left {
        let unpushed =
            if self.upstream.is_some() { self.ahead } else { self.unpushed.unwrap_or(0) };
        Left {
            uncommitted: self.dirty(),
            unpushed,
            not_pushed: self.upstream.is_none() && unpushed > 0,
            pushed: self.pushed,
        }
    }

    /// Fill `pushed` once the counts are in: clean, nothing unpushed, commits beyond the base, and
    /// somewhere to push them (an upstream, or a remote that was counted against).
    pub fn settle(&mut self) {
        let l = self.left();
        let remote = self.upstream.is_some() || self.unpushed.is_some();
        self.pushed = remote && l.uncommitted == 0 && l.unpushed == 0 && self.base_ahead > 0;
    }
}

impl Left {
    /// Nothing to show for the worktree's own work (no commits, nothing changed).
    pub fn quiet(&self) -> bool {
        self.uncommitted == 0 && self.unpushed == 0 && !self.pushed
    }

    /// The line counts matter only while there is something left to do.
    pub fn open(&self) -> bool {
        self.uncommitted > 0 || self.unpushed > 0
    }

    /// `✓ pushed`, `↑2`, `● 3`, `↑2 ● 3`, `not pushed ↑2`: the words and marks, in the order shown.
    pub fn words(&self) -> String {
        let mut v: Vec<String> = Vec::new();
        if self.unpushed > 0 {
            v.push(if self.not_pushed {
                format!("not pushed ↑{}", self.unpushed)
            } else {
                format!("↑{}", self.unpushed)
            });
        }
        if self.uncommitted > 0 {
            v.push(format!("● {}", self.uncommitted));
        }
        if self.pushed {
            v.push("✓ pushed".into());
        }
        v.join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    pub id: String,
    /// The machine it lives on, when that is another one this daemon mirrors (MM1.B, SPEC
    /// §16.4b); `None`: here. Its names then carry the machine (`laptop2:work`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
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
    /// What it's for, for people to read (UX1.24): the prompt it was made for, a PR's or an
    /// issue's title, or `--title`. The `name` is its address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

impl WorktreeInfo {
    /// Its branch is still the one it was made with by default: its name (since v0.0.13), or
    /// `ptyro/<name>` (before). Such a branch says nothing the name doesn't.
    pub fn default_branch(&self) -> bool {
        self.branch
            .as_deref()
            .is_some_and(|b| b == self.name || b == format!("ptyro/{}", self.name))
    }

    /// `myrepo/brisk-rook`, `myrepo/@root`.
    pub fn qualified(&self) -> String {
        format!("{}/{}", self.project_slug, self.name)
    }
}

pub const ROOT_WORKTREE: &str = "@root";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceInfo {
    pub id: String,
    /// The machine it lives on, when that is another one this daemon mirrors (MM1.B, SPEC
    /// §16.4b); `None`: here. Its names then carry the machine (`laptop2:work`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
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

// ── many machines (MM1.B, SPEC §16.4b) ─────────────────────────────────────

/// How this daemon reaches another machine's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum MachineTransport {
    /// `ssh host ptyro proxy` (SPEC §16.2): keys or an agent, no ports, nothing to pair.
    Ssh { target: String },
    /// The other machine's `ptyro serve`, with a token it paired to this one (`machine pair`).
    /// `fingerprint`: the SHA-256 of its certificate authority, pinned (its own HTTPS).
    Token {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fingerprint: Option<String>,
    },
}

impl MachineTransport {
    /// For people: `ssh://me@laptop2`, `https://laptop2.tail.ts.net:7091`.
    pub fn label(&self) -> String {
        match self {
            Self::Ssh { target } if target.starts_with("ssh://") => target.clone(),
            Self::Ssh { target } => format!("ssh://{target}"),
            Self::Token { url, .. } => url.clone(),
        }
    }

    /// The same machine reached the same way (`https://Turkey.ts.net:7091/` is
    /// `https://turkey.ts.net:7091`): one machine is added once.
    pub fn same_place(&self, other: &Self) -> bool {
        let key = |t: &Self| t.label().trim_end_matches('/').to_ascii_lowercase();
        key(self) == key(other)
    }
}

/// Where a link to another machine stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MachineState {
    /// Trying (at start, after a drop, after the backoff).
    #[default]
    Connecting,
    Online,
    /// Not reachable; its workspaces show as they were last seen.
    Offline,
    /// Reachable, but it runs another version of ptyro: the link stays down until they match.
    Mismatch,
}

impl MachineState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Online => "online",
            Self::Offline => "offline",
            Self::Mismatch => "other version",
        }
    }
}

/// Another machine this daemon mirrors (MM1.B).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineInfo {
    /// What its names carry here: `laptop2` in `laptop2:work`.
    pub name: String,
    pub transport: MachineTransport,
    pub state: MachineState,
    /// Since when it is in `state`.
    #[serde(default)]
    pub since_ms: Option<u64>,
    /// Its ptyro version, once heard.
    #[serde(default)]
    pub version: Option<String>,
    /// What it calls itself (`DaemonStatus.machine`).
    #[serde(default)]
    pub calls_itself: Option<String>,
    /// Its workspaces, by their own names, shown or not.
    #[serde(default)]
    pub workspaces: Vec<String>,
    /// Those not shown here.
    #[serde(default)]
    pub hidden: Vec<String>,
    /// Why it isn't online, in words.
    #[serde(default)]
    pub error: Option<String>,
    /// What to do about it, and where (`run ptyro daemon upgrade on laptop2`).
    #[serde(default)]
    pub fix: Option<String>,
    pub added_ms: u64,
}

/// `laptop2:work` → (`laptop2`, `work`); a name without a machine is `None`.
pub fn split_machine(name: &str) -> Option<(&str, &str)> {
    let (m, rest) = name.split_once(':')?;
    (!m.is_empty() && !rest.is_empty() && crate::names::validate(m).is_ok()).then_some((m, rest))
}

/// A profile as clients see it: never any secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    /// The machine it lives on, when that is another one this daemon mirrors (MM1.B, SPEC
    /// §16.4b); `None`: here. Its names then carry the machine (`laptop2:work`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
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
    /// Default: the branch template (`{name}`).
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
    /// The client's SSH agent and display variables.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub workspace: Option<String>,
    /// Its title (UX1.24); default: the prompt, else the PR's or issue's title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Its agent's session is private (1.55, SPEC §25.7a); `None`: as `kb.private` says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private: Option<bool>,
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
    /// Script names: `setup`, `run:<name>` (default run script first).
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
    /// What its directory takes on disk (build output included), when measured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    /// Measuring took too long and stopped: it takes at least `bytes`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bytes_partial: bool,
}

impl GcEntry {
    /// `4.1 GiB`, `≥ 12.0 GiB` when measuring stopped early; `None` when not measured.
    pub fn size_label(&self) -> Option<String> {
        let b = self.bytes?;
        Some(format!("{}{}", if self.bytes_partial { "≥ " } else { "" }, human_bytes(b)))
    }
}

/// A scratch shell's directory left behind (SPEC §23.8): its shell went while files were still
/// in it (they're never deleted unasked), or the daemon stopped before it could tidy up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScratchLeftover {
    pub path: PathBuf,
    /// Files and directories in it, all the way down.
    pub files: u64,
    pub bytes: u64,
    /// When something in it last changed.
    pub modified_ms: u64,
}

/// `512 B`, `3.4 MiB`, `12.0 GiB`.
pub fn human_bytes(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let (mut v, mut i) = (b as f64, 0);
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
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
    /// The commit compared from (a checkpoint's).
    #[serde(default)]
    pub from: Option<String>,
    pub files: Vec<DiffFile>,
    /// Files or hunks left out because the diff was too big.
    #[serde(default)]
    pub truncated: bool,
    /// Always `"base"`, never read: a ptyro from before 2026-11-17 requires the field (what the
    /// diff was against, when there were several kinds) to decode the diff at all.
    #[serde(default, skip_deserializing, serialize_with = "against_base")]
    pub against: (),
}

fn against_base<S: serde::Serializer>(_: &(), s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str("base")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ptyro from before 2026-11-17 decodes a checkpoint diff: it requires `against`.
    #[test]
    fn an_older_client_decodes_a_diff() {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum OldAgainst {
            Base,
        }
        #[derive(Deserialize)]
        struct OldDiffDoc {
            against: OldAgainst,
            files: Vec<DiffFile>,
        }
        let doc = DiffDoc { from: Some("abc".into()), ..DiffDoc::default() };
        let json: OldDiffDoc = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert!(matches!(json.against, OldAgainst::Base) && json.files.is_empty());
        let back: DiffDoc = serde_json::from_str(&serde_json::to_string(&doc).unwrap()).unwrap();
        assert_eq!(back, doc);
    }

    #[test]
    fn slugs() {
        assert_eq!(slugify("My Repo.git"), "my-repo");
        assert_eq!(slugify("--x__y--"), "x-y");
        assert_eq!(slugify("..."), "", "no made-up stand-in: the caller picks one");
        assert_eq!(slugify("my_fix"), "my-fix");
        assert_eq!(slugify(&"abc ".repeat(30)).len(), 40 - 1, "40 at most, no - at the end");
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
    fn what_is_left_to_do() {
        let st = |f: &dyn Fn(&mut RepoStatus)| {
            let mut s = RepoStatus { base_ahead: 1, ..RepoStatus::default() };
            f(&mut s);
            s.settle();
            s.left()
        };
        let l = st(&|s| s.unpushed = Some(0));
        assert_eq!(l.words(), "✓ pushed", "commits, nothing left to push: done");
        assert!(st(&|_| {}).quiet(), "no remote to push to: nothing to say");
        // Upstream: the ahead count is what is unpushed.
        let l = st(&|s| {
            s.upstream = Some("origin/x".into());
            s.ahead = 2;
            s.modified = 1;
            s.untracked = 2;
        });
        assert_eq!(l.words(), "↑2 ● 3");
        assert!(!l.pushed && l.open() && !l.not_pushed);
        // No upstream and commits: not pushed.
        let l = st(&|s| s.unpushed = Some(3));
        assert!(l.not_pushed && l.words() == "not pushed ↑3");
        // Nothing of its own (no commits beyond the base): quiet, not "pushed".
        let mut s = RepoStatus::default();
        s.settle();
        assert!(s.left().quiet() && s.left().words().is_empty());
        // Old daemons' JSON has none of the new fields.
        let old: RepoStatus = serde_json::from_str(
            r#"{"ahead":0,"behind":0,"base_ahead":1,"base_behind":0,"staged":0,"modified":0,
            "untracked":0,"conflicts":0,"insertions":0,"deletions":0,"at_ms":1}"#,
        )
        .unwrap();
        assert_eq!((old.unpushed, old.pushed), (None, false));
        let json = serde_json::to_value(RepoStatus { pushed: true, ..old }).unwrap();
        assert_eq!(json["pushed"], true);
        assert!(json.get("unpushed").is_none());
    }
}
