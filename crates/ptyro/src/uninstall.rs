//! `ptyro uninstall` (ROADMAP H2.3): take ptyro off a computer, tidily, and say first what goes and
//! what stays. It stops the daemon and every session, removes ptyro's own worktrees (through git,
//! so your repositories stay clean; their branches stay), its config, data, state and runtime
//! directories, the login service, the opt-in agent integrations, the lines `ptyro init` and the
//! mend list put in your shell's rc files, a Tailscale mapping it made, and the binary when it is
//! the one that was installed by hand.
//!
//! What it never touches: your repositories, their branches, `.ptyro.toml` and
//! `.ptyro.local.toml`, `.git/info/exclude` lines, and (unless `--purge-refs`) the snapshots under
//! `refs/ptyro/` in your repositories, which hold archived and taken-over work.
//!
//! The plan is built without starting anything (`--dry-run` stops there). Work that would be lost
//! (uncommitted changes in a worktree) has to be confirmed by typing `delete`. With `PTYRO_HOME`
//! set (a sandbox, a test) it resets that home only: the service, rc files, integrations and the
//! binary are the machine's, not the sandbox's, and are left alone.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};

use ptyro_core::Error;
use ptyro_core::config::Config;
use ptyro_core::paths::Paths;
use ptyro_protocol::{Reply, Request};
use serde::Serialize;

use crate::cli::Ctx;

#[derive(Debug, Clone, Default)]
pub struct Opts {
    pub dry_run: bool,
    pub yes: bool,
    pub delete_work: bool,
    pub keep_config: bool,
    pub keep_data: bool,
    pub keep_binary: bool,
    pub backup: Option<PathBuf>,
    pub purge_refs: bool,
}

/// What the process's surroundings say, gathered once so the planning can be tested.
#[derive(Debug, Clone)]
pub struct Env {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    /// `PTYRO_HOME`: this is a sandbox, not the install.
    pub ptyro_home: Option<PathBuf>,
    pub exe: PathBuf,
    /// This process runs inside a ptyro session.
    pub in_session: bool,
}

impl Env {
    pub fn current() -> Self {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        Self {
            home: var("HOME"),
            xdg_config_home: var("XDG_CONFIG_HOME"),
            ptyro_home: var("PTYRO_HOME"),
            exe: std::env::current_exe().unwrap_or_else(|_| PathBuf::from("ptyro")),
            in_session: var("PTYRO_SESSION_ID").is_some(),
        }
    }
}

// ── the plan ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Dir {
    pub name: &'static str,
    pub path: PathBuf,
    pub bytes: u64,
    /// Kept: `--keep-config` or `--keep-data` said so.
    pub keep: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Worktree {
    pub path: PathBuf,
    pub repo: Option<PathBuf>,
    pub branch: Option<String>,
    /// Changed or untracked files: gone with the worktree.
    pub uncommitted: usize,
    /// Commits on no remote (the branch stays in the repository, so they are not lost).
    pub unpushed: usize,
    pub locked: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RcEdit {
    pub file: PathBuf,
    /// The lines that go.
    pub lines: Vec<String>,
    /// Other lines that mention ptyro's commands: left for you to look at.
    pub mentions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Refs {
    pub repo: PathBuf,
    pub archive: usize,
    pub taken: usize,
    pub other: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct Integration {
    pub agent: String,
    pub profile: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum BinaryAction {
    /// Deleted.
    Remove,
    /// `cargo uninstall ptyro`, which keeps cargo's own record right.
    ViaCargo,
    /// Someone else's to remove, and the command that does it.
    Manual { command: String },
    /// Left where it is.
    Keep { reason: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct Binary {
    pub path: PathBuf,
    #[serde(flatten)]
    pub action: BinaryAction,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    /// `PTYRO_HOME`, when this is a sandbox.
    pub sandbox: Option<PathBuf>,
    pub daemon_running: bool,
    /// Sessions that are running now (`None`: no daemon to ask).
    pub sessions_live: Option<usize>,
    pub dirs: Vec<Dir>,
    /// Agent logins in ptyro-managed profile directories go with the data directory.
    pub logins: bool,
    /// The size of `chat.db`, your chats' text, which goes with the data directory (SPEC §22.4).
    #[serde(default)]
    pub chats: Option<u64>,
    pub worktrees: Vec<Worktree>,
    pub uncommitted_total: usize,
    pub service: Option<PathBuf>,
    pub integrations: Vec<Integration>,
    pub rc: Vec<RcEdit>,
    pub tailscale: Option<String>,
    pub refs: Vec<Refs>,
    pub purge_refs: bool,
    pub binary: Binary,
    pub kept: Vec<String>,
    /// What to type on the computer to do it.
    pub command: String,
}

/// How big a directory is, links not followed.
pub fn dir_size(p: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(p) else { return 0 };
    if meta.is_file() {
        return meta.len();
    }
    if !meta.is_dir() {
        return 0;
    }
    std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| dir_size(&e.path())).sum()).unwrap_or(0)
}

pub fn human(b: u64) -> String {
    const U: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let (mut v, mut i) = (b as f64, 0);
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{b} B") } else { format!("{v:.1} {}", U[i]) }
}

fn tilde(p: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

/// The directories ptyro keeps, from where it keeps them (never from a guess about `~`).
pub fn dirs_of(paths: &Paths, opts: &Opts) -> Vec<Dir> {
    let mut list: Vec<(&'static str, PathBuf, bool)> = vec![
        ("config", paths.config.clone(), opts.keep_config),
        ("data", paths.data.clone(), opts.keep_data),
        ("state", paths.state.clone(), false),
        ("runtime", paths.runtime.clone(), false),
    ];
    // A long path puts the socket in /tmp/ptyro-<uid>-<hash>/: that directory is ptyro's too.
    if let Some(dir) = paths.socket.parent()
        && !dir.starts_with(&paths.runtime)
        && dir.file_name().is_some_and(|n| n.to_string_lossy().starts_with("ptyro-"))
    {
        list.push(("socket", dir.to_path_buf(), false));
    }
    let mut out: Vec<Dir> = Vec::new();
    for (name, path, keep) in list {
        // (State may be the same directory as data on a machine with no state dir.)
        if out.iter().any(|d| d.path == path) {
            continue;
        }
        let bytes = dir_size(&path);
        out.push(Dir {
            name,
            path,
            bytes,
            keep: keep || (name == "state" && opts.keep_data && paths.state == paths.data),
        });
    }
    out
}

/// What to do about the binary at `exe`.
pub fn classify_binary(exe: &Path, env: &Env, keep: bool) -> BinaryAction {
    let s = exe.to_string_lossy();
    if keep {
        return BinaryAction::Keep { reason: "--keep-binary".into() };
    }
    if env.ptyro_home.is_some() {
        return BinaryAction::Keep {
            reason: "PTYRO_HOME is set: this resets one ptyro home, not the install".into(),
        };
    }
    if s.contains("/target/") {
        return BinaryAction::Keep { reason: "it is a build in a cargo target directory".into() };
    }
    let brew = ["/Cellar/", "/Homebrew/", "/homebrew/", "/linuxbrew/", "/opt/homebrew/"];
    if brew.iter().any(|b| s.contains(b)) {
        return BinaryAction::Manual { command: "brew uninstall ptyro".into() };
    }
    if s.contains("/.cargo/registry/") {
        return BinaryAction::Manual { command: "cargo uninstall ptyro".into() };
    }
    if let Some(home) = &env.home
        && exe.starts_with(home.join(".cargo/bin"))
    {
        return BinaryAction::ViaCargo;
    }
    // Where a package manager puts things: its own command removes them.
    for dir in ["/usr/bin/", "/bin/", "/usr/sbin/", "/sbin/", "/snap/"] {
        if s.starts_with(dir) {
            return BinaryAction::Manual {
                command:
                    "remove it with the package manager that installed it (apt, dnf, pacman, snap…)"
                        .into(),
            };
        }
    }
    BinaryAction::Remove
}

/// The shell rc files ptyro may have added lines to.
pub fn rc_files(env: &Env) -> Vec<PathBuf> {
    let Some(home) = &env.home else { return Vec::new() };
    let conf = env.xdg_config_home.clone().unwrap_or_else(|| home.join(".config"));
    vec![
        home.join(".zshrc"),
        home.join(".bashrc"),
        home.join(".bash_profile"),
        home.join(".profile"),
        conf.join("fish/config.fish"),
    ]
}

fn rc_line_re() -> regex::Regex {
    regex::Regex::new(
        r#"^\s*(eval "\$\(ptyro shell-init (zsh|bash)\)"|ptyro shell-init fish \| source|source <\(ptyro completions (zsh|bash)\)|ptyro completions fish \| source)\s*$"#,
    )
    .expect("a fixed pattern")
}

/// The file without the lines ptyro put in it (the exact forms `ptyro init` and the mend list
/// write, and the comment ptyro put above one, and the blank line before that), and the lines
/// that went. Anything else that merely mentions ptyro stays.
pub fn strip_rc(text: &str) -> (String, Vec<String>) {
    let re = rc_line_re();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut gone = vec![false; lines.len()];
    for i in 0..lines.len() {
        if !re.is_match(lines[i]) {
            continue;
        }
        gone[i] = true;
        let mut first = i;
        if i > 0 && lines[i - 1].trim_start().starts_with("# ptyro ") {
            gone[i - 1] = true;
            first = i - 1;
        }
        if first > 0 && lines[first - 1].trim().is_empty() && !gone[first - 1] {
            gone[first - 1] = true;
        }
    }
    let removed: Vec<String> =
        lines.iter().zip(&gone).filter(|(_, g)| **g).map(|(l, _)| (*l).to_owned()).collect();
    let kept: Vec<&str> = lines.iter().zip(&gone).filter(|(_, g)| !**g).map(|(l, _)| *l).collect();
    (kept.join("\n"), removed)
}

/// Lines that mention ptyro's shell commands but aren't the forms [`strip_rc`] takes out.
pub fn rc_mentions(text: &str) -> Vec<String> {
    let re = rc_line_re();
    text.lines()
        .filter(|l| {
            !re.is_match(l)
                && !l.trim_start().starts_with('#')
                && (l.contains("ptyro shell-init") || l.contains("ptyro completions"))
        })
        .map(|l| l.trim().to_owned())
        .collect()
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-C"])
        .arg(dir)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A worktree as git sees it now: not the daemon's last look.
fn look(path: &Path, repo: Option<&Path>, branch: Option<String>, locked: bool) -> Worktree {
    let uncommitted = git(path, &["status", "--porcelain"])
        .map_or(0, |s| s.lines().filter(|l| !l.is_empty()).count());
    let has_remote = git(path, &["remote"]).is_some_and(|s| !s.trim().is_empty());
    let unpushed = if has_remote {
        git(path, &["rev-list", "--count", "HEAD", "--not", "--remotes"])
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(0)
    } else {
        0
    };
    Worktree {
        path: path.to_path_buf(),
        repo: repo.map(Path::to_path_buf),
        branch,
        uncommitted,
        unpushed,
        locked,
    }
}

fn ref_counts(repo: &Path) -> Option<Refs> {
    let out = git(repo, &["for-each-ref", "--format=%(refname)", "refs/ptyro"])?;
    let (mut archive, mut taken, mut other) = (0, 0, 0);
    for r in out.lines().filter(|l| !l.is_empty()) {
        if r.starts_with("refs/ptyro/archive/") {
            archive += 1;
        } else if r.starts_with("refs/ptyro/taken/") {
            taken += 1;
        } else {
            other += 1;
        }
    }
    (archive + taken + other > 0).then(|| Refs { repo: repo.to_path_buf(), archive, taken, other })
}

fn daemon_running(paths: &Paths) -> bool {
    std::fs::File::open(paths.lock_file()).is_ok_and(|f| f.try_lock_shared().is_err())
}

/// What the daemon, or failing that the database, knows.
struct Known {
    sessions_live: Option<usize>,
    /// `(worktree path, repository root, branch, locked)`, every one but a project's main checkout.
    worktrees: Vec<(PathBuf, Option<PathBuf>, Option<String>, bool)>,
    repos: Vec<PathBuf>,
}

async fn known(ctx: &Ctx) -> Known {
    let mut k = Known { sessions_live: None, worktrees: Vec::new(), repos: Vec::new() };
    if let Ok(c) = ctx.connect(false).await {
        if let Ok(Reply::Sessions { sessions }) = c.request(Request::List).await {
            k.sessions_live = Some(sessions.iter().filter(|s| s.state.is_live()).count());
        }
        let projects = match c.request(Request::ProjectList).await {
            Ok(Reply::Projects { projects }) => projects,
            _ => Vec::new(),
        };
        let wts = match c.request(Request::WorktreeList { project: None }).await {
            Ok(Reply::Worktrees { worktrees }) => worktrees,
            _ => Vec::new(),
        };
        fill(&mut k, &projects, &wts);
        return k;
    }
    let db = ctx.paths.data.join("ptyro.db");
    if db.exists()
        && let Ok(store) = ptyro_store::Store::open(&db)
        && let Ok(reg) = store.load_registry().await
    {
        fill(&mut k, &reg.projects, &reg.worktrees);
    }
    k
}

fn fill(
    k: &mut Known,
    projects: &[ptyro_core::model::ProjectInfo],
    wts: &[ptyro_core::model::WorktreeInfo],
) {
    for p in projects {
        if p.root.exists() && !k.repos.contains(&p.root) {
            k.repos.push(p.root.clone());
        }
    }
    for w in wts.iter().filter(|w| !w.is_root && w.path.exists()) {
        let repo = projects.iter().find(|p| p.id == w.project).map(|p| p.root.clone());
        k.worktrees.push((w.path.clone(), repo, w.branch.clone(), w.locked));
    }
}

/// The agent integrations ptyro installed (opt-in ones): for the default account and each
/// profile with an account directory of its own.
fn integrations_found(config: &Config, env: &Env) -> Vec<Integration> {
    let mut homes: Vec<(Option<String>, PathBuf)> = Vec::new();
    if let Some(h) = &env.home {
        homes.push((None, h.join(".gemini")));
    }
    for (name, p) in &config.profiles {
        if let Some(dir) = p.gemini.as_ref().and_then(|a| a.dir()) {
            homes.push((Some(name.clone()), ptyro_core::config::expand_home(dir).join(".gemini")));
        }
    }
    let mut out = Vec::new();
    for (profile, g) in homes {
        let linked = g.join("extensions").join("ptyro").exists()
            || std::fs::read_to_string(g.join("extensions/extension-enablement.json"))
                .is_ok_and(|t| t.contains("ptyro"));
        if linked {
            out.push(Integration { agent: "gemini".into(), profile: profile.clone() });
        }
        if std::fs::read_to_string(g.join("config/hooks.json"))
            .is_ok_and(|t| t.contains("hook antigravity"))
        {
            out.push(Integration { agent: "antigravity".into(), profile });
        }
    }
    out
}

/// A `tailscale serve` mapping to our port: an older ptyro made it (`[remote] tailscale`); this
/// one no longer does.
fn tailscale_mapping(config: &Config) -> Option<String> {
    let port = config.remote.port.to_string();
    let port = port.as_str();
    let out = std::process::Command::new("tailscale")
        .args(["serve", "status", "--json"])
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    crate::remote::proxies_to(&v, port)
        .then(|| format!("tailscale serve (https :443 → 127.0.0.1:{port})"))
}

/// Build the plan. Reads; starts nothing.
pub async fn plan(ctx: &Ctx, env: &Env, opts: &Opts) -> Result<Plan, Error> {
    if ctx.remote.is_some() {
        return Err(Error::invalid("uninstall works on this computer, not over --remote"));
    }
    let paths = &ctx.paths;
    let config = Config::load(&Config::path(paths)).unwrap_or_default();
    let sandbox = env.ptyro_home.clone();
    let running = daemon_running(paths);
    let k = known(ctx).await;
    let dirs: Vec<Dir> = dirs_of(paths, opts).into_iter().filter(|d| d.path.exists()).collect();
    let logins = paths.data.join("profiles").is_dir();
    let chats = std::fs::metadata(paths.data.join("chat.db")).ok().map(|m| m.len());

    // Worktrees and snapshot refs: asked of git, off the async threads.
    let wts_in: Vec<_> = if opts.keep_data { Vec::new() } else { k.worktrees.clone() };
    let repos = k.repos.clone();
    let (worktrees, refs) = tokio::task::spawn_blocking(move || {
        let w: Vec<Worktree> =
            wts_in.iter().map(|(p, repo, b, l)| look(p, repo.as_deref(), b.clone(), *l)).collect();
        let r: Vec<Refs> = repos.iter().filter_map(|r| ref_counts(r)).collect();
        (w, r)
    })
    .await
    .map_err(|e| Error::internal(e.to_string()))?;
    let uncommitted_total = worktrees.iter().filter(|w| w.uncommitted > 0).count();

    // The machine's own files: only when this is the install, not a sandbox.
    let machine = sandbox.is_none();
    let service = (machine && crate::service::installed()).then(crate::service::file);
    let integrations = if machine { integrations_found(&config, env) } else { Vec::new() };
    let mut rc = Vec::new();
    if machine {
        for file in rc_files(env) {
            let Ok(text) = std::fs::read_to_string(&file) else { continue };
            let (_, lines) = strip_rc(&text);
            let mentions = rc_mentions(&text);
            if !lines.is_empty() || !mentions.is_empty() {
                rc.push(RcEdit { file, lines, mentions });
            }
        }
    }
    let tailscale = if machine { tailscale_mapping(&config) } else { None };
    let binary =
        Binary { path: env.exe.clone(), action: classify_binary(&env.exe, env, opts.keep_binary) };

    let mut kept = vec![
        "your repositories, their branches, .ptyro.toml and .ptyro.local.toml, and the lines ptyro added to .git/info/exclude".to_owned(),
    ];
    if !opts.purge_refs && !refs.is_empty() {
        let n: usize = refs.iter().map(|r| r.archive + r.taken + r.other).sum();
        kept.push(format!(
            "{n} snapshot ref(s) under refs/ptyro/ in {} repositor{} (archived and taken-over work; --purge-refs deletes them)",
            refs.len(),
            if refs.len() == 1 { "y" } else { "ies" }
        ));
    }
    if rc.iter().any(|r| !r.mentions.is_empty()) {
        kept.push(
            "lines in your rc files that mention ptyro but aren't the ones ptyro wrote".to_owned(),
        );
    }
    for d in dirs.iter().filter(|d| d.keep) {
        kept.push(format!("{} ({})", d.path.display(), d.name));
    }
    if opts.keep_data && !k.worktrees.is_empty() {
        kept.push(format!("{} worktree(s), which live with the data", k.worktrees.len()));
    }
    if let BinaryAction::Keep { reason } = &binary.action {
        kept.push(format!("the ptyro binary ({reason})"));
    }
    Ok(Plan {
        sandbox,
        daemon_running: running,
        sessions_live: k.sessions_live,
        dirs,
        logins,
        chats,
        worktrees,
        uncommitted_total,
        service,
        integrations,
        rc,
        tailscale,
        refs,
        purge_refs: opts.purge_refs,
        binary,
        kept,
        command: "ptyro uninstall".into(),
    })
}

/// The plan as a person reads it.
pub fn render(plan: &Plan, env: &Env) -> String {
    use std::fmt::Write as _;
    let home = env.home.as_deref();
    let mut s = String::new();
    match &plan.sandbox {
        Some(h) => {
            let _ = writeln!(
                s,
                "ptyro uninstall (PTYRO_HOME is set: resets {} only) removes:",
                h.display()
            );
        }
        None => s.push_str("ptyro uninstall removes:\n"),
    }
    match (plan.daemon_running, plan.sessions_live) {
        (true, Some(n)) => {
            let _ = writeln!(
                s,
                "  sessions      {n} running: they all stop (agents too; their conversations stay in each agent's own history)"
            );
        }
        (true, None) => s.push_str(
            "  the daemon    is running (it didn't answer): it stops, and its sessions\n",
        ),
        (false, _) => s.push_str("  the daemon    is not running\n"),
    }
    for d in plan.dirs.iter().filter(|d| !d.keep) {
        let _ = writeln!(s, "  {:<13} {} ({})", d.name, tilde(&d.path, home), human(d.bytes));
    }
    if let Some(bytes) =
        plan.chats.filter(|_| !plan.dirs.iter().any(|d| d.name == "data" && d.keep))
    {
        let _ = writeln!(s, "  chats         every chat you kept, chat.db ({})", human(bytes));
    }
    if plan.logins && !plan.dirs.iter().any(|d| d.name == "data" && d.keep) {
        s.push_str(
            "  logins        agent logins in ptyro-managed profiles (they log in again after)\n",
        );
    }
    if !plan.worktrees.is_empty() {
        let _ = writeln!(
            s,
            "  worktrees     {} (their branches stay in the repositories):",
            plan.worktrees.len()
        );
        for w in &plan.worktrees {
            let mut note = String::new();
            if w.uncommitted > 0 {
                let _ = write!(note, "  ⚠ {} uncommitted change(s) will be lost", w.uncommitted);
            }
            if w.unpushed > 0 {
                let _ = write!(
                    note,
                    "  ({} commit(s) not pushed: on the branch, which stays)",
                    w.unpushed
                );
            }
            if w.locked {
                note.push_str("  (locked)");
            }
            let _ = writeln!(
                s,
                "                  {}  {}{note}",
                w.branch.as_deref().unwrap_or("(detached)"),
                tilde(&w.path, home)
            );
        }
    }
    if let Some(f) = &plan.service {
        let _ = writeln!(s, "  service       {} (starts the daemon at login)", tilde(f, home));
    }
    if !plan.integrations.is_empty() {
        let names: Vec<String> = plan
            .integrations
            .iter()
            .map(|i| match &i.profile {
                Some(p) => format!("{} ({p})", i.agent),
                None => i.agent.clone(),
            })
            .collect();
        let _ = writeln!(s, "  integrations  {}", names.join(", "));
    }
    for r in plan.rc.iter().filter(|r| !r.lines.is_empty()) {
        let _ = writeln!(
            s,
            "  shell setup   {} line(s) from {}:",
            r.lines.iter().filter(|l| !l.trim().is_empty()).count(),
            tilde(&r.file, home)
        );
        for l in r.lines.iter().filter(|l| !l.trim().is_empty()) {
            let _ = writeln!(s, "                  {}", l.trim());
        }
    }
    if let Some(t) = &plan.tailscale {
        let _ = writeln!(s, "  tailscale     {t}, switched off");
    }
    if plan.purge_refs && !plan.refs.is_empty() {
        let n: usize = plan.refs.iter().map(|r| r.archive + r.taken + r.other).sum();
        let _ = writeln!(
            s,
            "  snapshots     {n} ref(s) under refs/ptyro/ in {} repositor(ies): archived and taken-over work",
            plan.refs.len()
        );
    }
    match &plan.binary.action {
        BinaryAction::Remove => {
            let _ = writeln!(s, "  the binary    {}", tilde(&plan.binary.path, home));
        }
        BinaryAction::ViaCargo => {
            let _ = writeln!(
                s,
                "  the binary    {} (by `cargo uninstall ptyro`)",
                tilde(&plan.binary.path, home)
            );
        }
        BinaryAction::Manual { command } => {
            let _ = writeln!(s, "  the binary    stays: remove it yourself with `{command}`");
        }
        BinaryAction::Keep { .. } => {}
    }
    s.push_str("Kept:\n");
    for k in &plan.kept {
        let _ = writeln!(s, "  {k}");
    }
    s
}

// ── doing it ──────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct Step {
    pub step: String,
    pub ok: bool,
    pub detail: String,
}

fn step(out: &mut Vec<Step>, json: bool, name: &str, r: Result<String, String>) {
    let (ok, detail) = match r {
        Ok(d) => (true, d),
        Err(d) => (false, d),
    };
    if !json {
        println!(
            "{} {name}{}",
            if ok { "✓" } else { "✗" },
            if detail.is_empty() { String::new() } else { format!(": {detail}") }
        );
    }
    out.push(Step { step: name.to_owned(), ok, detail });
}

fn remove_tree(p: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(p),
        Ok(_) => std::fs::remove_file(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Take a worktree away through git (so the repository forgets it), or by hand when git can't.
fn remove_worktree(w: &Worktree) -> Result<String, String> {
    if let Some(repo) = w.repo.as_deref().filter(|r| r.exists()) {
        let ok = std::process::Command::new("git")
            .args(["-c", "core.hooksPath=/dev/null", "-C"])
            .arg(repo)
            .args(["worktree", "remove", "--force", "--force"])
            .arg(&w.path)
            .stdin(std::process::Stdio::null())
            .output()
            .is_ok_and(|o| o.status.success());
        if !ok {
            remove_tree(&w.path).map_err(|e| format!("{}: {e}", w.path.display()))?;
        }
        let _ = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["worktree", "prune"])
            .output();
        return Ok(w.path.display().to_string());
    }
    remove_tree(&w.path).map_err(|e| format!("{}: {e}", w.path.display()))?;
    Ok(w.path.display().to_string())
}

fn purge_refs_of(repo: &Path) -> Result<usize, String> {
    let out = git(repo, &["for-each-ref", "--format=%(refname)", "refs/ptyro"])
        .ok_or_else(|| format!("{}: git couldn't list refs", repo.display()))?;
    let mut n = 0;
    for r in out.lines().filter(|l| !l.is_empty()) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(["update-ref", "-d", r])
            .output()
            .is_ok_and(|o| o.status.success());
        if ok {
            n += 1;
        }
    }
    Ok(n)
}

fn write_rc(file: &Path, text: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(file).map_or(0o644, |m| m.permissions().mode() & 0o777);
    crate::config::write_atomic(file, text, mode).map_err(|e| e.message)
}

/// Run the uninstall. Returns whether everything it set out to do was done.
async fn execute(ctx: &Ctx, env: &Env, plan: &Plan, opts: &Opts, json: bool) -> (Vec<Step>, bool) {
    let mut steps = Vec::new();
    let exe = env.exe.clone();
    let sub = |args: Vec<String>| {
        let exe = exe.clone();
        async move {
            let out = tokio::process::Command::new(exe)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .output()
                .await;
            match out {
                Ok(o) if o.status.success() => Ok(String::new()),
                Ok(o) => Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
                Err(e) => Err(e.to_string()),
            }
        }
    };

    // A backup first: nothing below is undone by it, but nothing is lost unseen.
    if let Some(file) = &opts.backup {
        let r = crate::backup::create(
            ctx,
            &crate::backup::Opts { output: Some(file.clone()), ..Default::default() },
        )
        .await;
        let ok = r.is_ok();
        step(
            &mut steps,
            json,
            "backup",
            r.map(|m| m.path.display().to_string()).map_err(|e| e.message),
        );
        if !ok {
            // Without the backup that was asked for, nothing is removed.
            return (steps, false);
        }
    }
    for i in &plan.integrations {
        let mut args = vec!["integrations".to_owned(), "uninstall".into(), i.agent.clone()];
        if let Some(p) = &i.profile {
            args.extend(["--profile".to_owned(), p.clone()]);
        }
        step(&mut steps, json, &format!("remove the {} integration", i.agent), sub(args).await);
    }
    if plan.service.is_some() {
        let r = crate::service::uninstall(ctx, false).map(|_| String::new()).map_err(|e| e.message);
        step(&mut steps, json, "remove the login service", r);
    }
    // The daemon, and what it holds: nothing is deleted under one that still runs.
    let stop = crate::cli::stop_daemon(ctx, true, false).await;
    let stopped = stop.is_ok() && !daemon_running(&ctx.paths);
    step(
        &mut steps,
        json,
        "stop the daemon",
        match (&stop, stopped) {
            (_, true) => Ok(String::new()),
            (Err(e), _) => Err(e.message.clone()),
            _ => {
                Err("it is still running: stop it (`ptyro daemon stop`) and run this again"
                    .to_owned())
            }
        },
    );
    if stopped {
        if !plan.worktrees.is_empty() {
            let ws = plan.worktrees.clone();
            let results = tokio::task::spawn_blocking(move || {
                ws.iter().map(remove_worktree).collect::<Vec<_>>()
            })
            .await
            .unwrap_or_default();
            let failed: Vec<String> =
                results.iter().filter_map(|r| r.as_ref().err().cloned()).collect();
            step(
                &mut steps,
                json,
                &format!("remove {} worktree(s)", plan.worktrees.len()),
                if failed.is_empty() { Ok(String::new()) } else { Err(failed.join("; ")) },
            );
        }
        if opts.purge_refs {
            for r in &plan.refs {
                let repo = r.repo.clone();
                let res = tokio::task::spawn_blocking(move || purge_refs_of(&repo)).await;
                step(
                    &mut steps,
                    json,
                    &format!("delete snapshot refs in {}", r.repo.display()),
                    match res {
                        Ok(Ok(n)) => Ok(format!("{n} ref(s)")),
                        Ok(Err(e)) => Err(e),
                        Err(e) => Err(e.to_string()),
                    },
                );
            }
        }
        for d in plan.dirs.iter().filter(|d| !d.keep) {
            step(
                &mut steps,
                json,
                &format!("remove {}", d.path.display()),
                remove_tree(&d.path).map(|()| String::new()).map_err(|e| e.to_string()),
            );
        }
        // A sandbox's home directory itself, once it is empty.
        if let Some(h) = &plan.sandbox
            && std::fs::read_dir(h).is_ok_and(|mut d| d.next().is_none())
        {
            let _ = std::fs::remove_dir(h);
        }
    }
    for r in plan.rc.iter().filter(|r| !r.lines.is_empty()) {
        let res = std::fs::read_to_string(&r.file)
            .map_err(|e| e.to_string())
            .and_then(|t| write_rc(&r.file, &strip_rc(&t).0));
        step(
            &mut steps,
            json,
            &format!("take ptyro out of {}", r.file.display()),
            res.map(|()| String::new()),
        );
    }
    if plan.tailscale.is_some() {
        let ok = std::process::Command::new("tailscale")
            .args(["serve", "--https=443", "off"])
            .output()
            .is_ok_and(|o| o.status.success());
        step(
            &mut steps,
            json,
            "switch off the tailscale mapping",
            if ok {
                Ok(String::new())
            } else {
                Err("`tailscale serve --https=443 off` failed: run it yourself".into())
            },
        );
    }
    match &plan.binary.action {
        BinaryAction::Remove => {
            let r = std::fs::remove_file(&plan.binary.path).map(|()| String::new()).map_err(|e| {
                format!(
                    "{e}: remove it yourself (`sudo rm {}` if it needs it)",
                    plan.binary.path.display()
                )
            });
            step(&mut steps, json, &format!("remove {}", plan.binary.path.display()), r);
        }
        BinaryAction::ViaCargo => {
            let ok = std::process::Command::new("cargo")
                .args(["uninstall", "ptyro"])
                .output()
                .is_ok_and(|o| o.status.success());
            let r = if ok {
                Ok(String::new())
            } else {
                // No cargo on the PATH: the file is what matters.
                std::fs::remove_file(&plan.binary.path)
                    .map(|()| String::new())
                    .map_err(|e| e.to_string())
            };
            step(&mut steps, json, "remove the binary (cargo uninstall ptyro)", r);
        }
        BinaryAction::Manual { command } => {
            if !json {
                println!("• the binary stays: remove it with `{command}`");
            }
        }
        BinaryAction::Keep { .. } => {}
    }
    let ok = steps.iter().all(|s| s.ok);
    (steps, ok)
}

fn answer() -> String {
    let mut l = String::new();
    let _ = std::io::stdin().lock().read_line(&mut l);
    l.trim().to_owned()
}

/// `delete`, typed: the one answer `--yes` never gives for the person.
fn asked_delete(n: usize) -> bool {
    print!("Uncommitted work in {n} worktree(s) will be lost. Type delete to go on: ");
    let _ = std::io::stdout().flush();
    answer() == "delete"
}

fn asked_yes() -> bool {
    print!("Uninstall ptyro? [y/N] ");
    let _ = std::io::stdout().flush();
    matches!(answer().to_lowercase().as_str(), "y" | "yes")
}

/// `ptyro uninstall`.
pub async fn run(ctx: &Ctx, opts: Opts, json: bool) -> Result<i32, Error> {
    let env = Env::current();
    if env.in_session && env.ptyro_home.is_none() && !opts.dry_run {
        return Err(Error::invalid(
            "this shell runs inside a ptyro session: stopping the daemon would stop the uninstall too. Detach (Ctrl-Q d) and run it from a plain terminal",
        ));
    }
    let plan = plan(ctx, &env, &opts).await?;
    if opts.dry_run {
        if json {
            println!("{}", serde_json::to_string_pretty(&plan).unwrap_or_default());
        } else {
            print!("{}", render(&plan, &env));
            println!("(a dry run: nothing was removed)");
        }
        return Ok(0);
    }
    if !json {
        print!("{}", render(&plan, &env));
    }
    let interactive = std::io::stdin().is_terminal();
    // Work that would be lost is confirmed by typing `delete`, or by a script saying
    // `--delete-work`; `--yes` is only the plain question.
    let needs_delete = plan.uncommitted_total > 0 && !opts.delete_work;
    if needs_delete {
        if !interactive {
            return Err(Error::invalid(
                "uncommitted work would be lost: run it in a terminal and type `delete`, or pass --delete-work",
            ));
        }
        if !asked_delete(plan.uncommitted_total) {
            println!("nothing removed");
            return Ok(1);
        }
    } else if !opts.yes {
        if !interactive {
            return Err(Error::invalid(
                "not a terminal: pass --yes to go ahead (--dry-run shows the plan)",
            ));
        }
        if !asked_yes() {
            println!("nothing removed");
            return Ok(1);
        }
    }
    let (steps, ok) = execute(ctx, &env, &plan, &opts, json).await;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(
                &serde_json::json!({ "plan": plan, "steps": steps, "ok": ok })
            )
            .unwrap_or_default()
        );
    } else if ok {
        println!("ptyro is uninstalled. A kept or reinstalled `ptyro` starts from the first run.");
    } else {
        println!(
            "some steps failed (✗ above); what could be done was. Run it again after fixing them."
        );
    }
    Ok(i32::from(!ok))
}

#[cfg(test)]
mod tests;
