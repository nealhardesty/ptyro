//! Profiles (SPEC §10.10): which one a session gets, and the environment that gives it.
//!
//! Everything here is pure except secret resolution, which the caller supplies, and
//! [`sync_shares`], which makes symlinks. The daemon calls [`plan`] for every spawn path (TUI,
//! CLI, restart, scripts), so they can't drift apart.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use ptyro_core::config::{CREDENTIAL_FILES, Config, ProfileDef, Secret, expand_home};
use ptyro_core::model::{DEFAULT_PROFILE, ProfileInfo, ProfileReason};

/// Removed from every session's environment, whatever its profile: a daemon started from a shell
/// with `CLAUDE_CONFIG_DIR` exported must not leak that account into `default` sessions.
pub const MANAGED_VARS: &[&str] = &[
    "CLAUDE_CONFIG_DIR",
    "CODEX_HOME",
    "GEMINI_CLI_HOME",
    "OPENCODE_CONFIG_DIR",
    "GH_CONFIG_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_SSH_COMMAND",
    "PTYRO_PROFILE",
];

/// Prefixes removed the same way (`GIT_AUTHOR_NAME`, `GIT_COMMITTER_DATE`, …).
pub const MANAGED_PREFIXES: &[&str] = &["GIT_AUTHOR_", "GIT_COMMITTER_"];

/// Inherited credentials, removed when a profile isolates (SPEC §10.10 step 2).
pub const CREDENTIAL_VARS: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "CLAUDE_CODE_OAUTH_TOKEN",
    "CLAUDE_CODE_USE_BEDROCK",
    "CLAUDE_CODE_USE_VERTEX",
    "OPENAI_API_KEY",
    "CODEX_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "AWS_SESSION_TOKEN",
    "AWS_PROFILE",
];

/// More inherited credentials and endpoints, by shape, removed with [`CREDENTIAL_VARS`] when a
/// profile isolates: the list above cannot name every provider's variables. A profile's own
/// `env`, `secrets` and account tokens are set after, so they survive.
pub const CREDENTIAL_PREFIXES: &[&str] = &[
    "AWS_",
    "AZURE_",
    "GOOGLE_",
    "GH_",
    "GITHUB_",
    "OPENAI_",
    "ANTHROPIC_",
    "XAI_",
    "MISTRAL_",
    "OPENROUTER_",
    "GIT_CONFIG_",
];

/// Variable names ending like a secret.
pub const CREDENTIAL_SUFFIXES: &[&str] =
    &["_API_KEY", "_TOKEN", "_SECRET", "_SECRET_KEY", "_PASSWORD", "_ASKPASS"];

/// Other exact names that carry a credential or point git or ssh at somebody's own config.
pub const CREDENTIAL_EXTRA: &[&str] =
    &["GIT_ASKPASS", "SSH_ASKPASS", "GIT_SSH", "GIT_CONFIG_SYSTEM", "NETRC"];

/// Agents (and `gh`) whose config dir a profile sets, in a stable order.
const ACCOUNTS: &[&str] = &["claude", "codex", "gemini", "opencode", "gh"];

/// The profile's definition, if any. `default` may be implicit.
pub fn def<'a>(config: &'a Config, name: &str) -> Option<&'a ProfileDef> {
    config.profiles.get(name)
}

pub fn exists(config: &Config, name: &str) -> bool {
    name == DEFAULT_PROFILE || config.profiles.contains_key(name)
}

/// Every profile: `default` first (defined or not), then the configured ones.
pub fn names(config: &Config) -> Vec<String> {
    let mut v = vec![DEFAULT_PROFILE.to_owned()];
    v.extend(config.profiles.keys().filter(|k| *k != DEFAULT_PROFILE).cloned());
    v
}

/// Whether `name` removes inherited credentials: user profiles always do, and `default` does as
/// soon as any profile is defined (decided 2026-10-20), unless `isolate = false`.
pub fn isolates(config: &Config, name: &str) -> bool {
    let any = config.profiles.keys().any(|k| k != DEFAULT_PROFILE);
    def(config, name).and_then(|d| d.isolate).unwrap_or(name != DEFAULT_PROFILE || any)
}

/// The inputs of SPEC §10.10's resolution order.
#[derive(Debug, Clone, Copy, Default)]
pub struct Inputs<'a> {
    pub explicit: Option<&'a str>,
    pub worktree: Option<&'a str>,
    pub pin: Option<&'a str>,
    pub workspace: Option<&'a str>,
}

/// First match wins: explicit, the worktree's, the project's pin, the workspace's, `default`.
pub fn resolve(i: Inputs<'_>) -> (String, ProfileReason) {
    let pick = [
        (i.explicit, ProfileReason::Explicit),
        (i.worktree, ProfileReason::Worktree),
        (i.pin, ProfileReason::Pinned),
        (i.workspace, ProfileReason::Workspace),
    ];
    pick.into_iter()
        .find_map(|(p, r)| p.map(|p| (p.to_owned(), r)))
        .unwrap_or_else(|| (DEFAULT_PROFILE.into(), ProfileReason::Default))
}

/// Whether an explicit choice contradicts a worktree's profile or a pin (needs confirmation).
pub fn contradicts(i: Inputs<'_>) -> Option<(String, ProfileReason)> {
    let explicit = i.explicit?;
    [(i.worktree, ProfileReason::Worktree), (i.pin, ProfileReason::Pinned)]
        .into_iter()
        .find_map(|(p, r)| p.filter(|p| *p != explicit).map(|p| (p.to_owned(), r)))
}

/// Whether a repo with these remotes and path matches `def`'s `match` rules.
pub fn matches(def: &ProfileDef, remotes: &[String], path: &Path) -> bool {
    let norm: Vec<String> = remotes.iter().map(|r| ptyro_git::normalize_remote(r)).collect();
    let remote_hit = def.matches.remotes.iter().any(|pat| {
        let pat = ptyro_git::normalize_remote(pat);
        norm.iter().any(|r| crate::glob::matches(&pat, r))
    });
    let path_s = path.to_string_lossy();
    let path_hit = def
        .matches
        .paths
        .iter()
        .any(|pat| crate::glob::matches(&expand_home(pat).to_string_lossy(), &path_s));
    remote_hit || path_hit
}

/// Profiles whose `match` rules claim this repo.
pub fn claimants(config: &Config, remotes: &[String], path: &Path) -> Vec<String> {
    config
        .profiles
        .iter()
        .filter(|(_, d)| !d.matches.is_empty() && matches(d, remotes, path))
        .map(|(n, _)| n.clone())
        .collect()
}

/// The environment changes for a session under one profile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub profile: String,
    /// Exact names removed from the base environment.
    pub remove: Vec<String>,
    pub remove_prefixes: Vec<String>,
    /// Names ending like these are removed too (`_TOKEN`, `_API_KEY`).
    pub remove_suffixes: Vec<String>,
    /// Real values, applied in order.
    pub set: Vec<(String, String)>,
    /// The same, with secrets shown as `<secret:NAME>`: safe to store, log and print.
    pub shown: Vec<(String, String)>,
    /// Managed profile dirs to create (`0700`) before the session starts.
    pub create_dirs: Vec<PathBuf>,
}

impl Plan {
    /// The base environment (the daemon's) without what this profile removes.
    pub fn strip(
        &self,
        base: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Vec<(OsString, OsString)> {
        base.into_iter()
            .filter(|(k, _)| {
                let k = k.to_string_lossy();
                !self.remove.iter().any(|r| *r == k)
                    && !self.remove_prefixes.iter().any(|p| k.starts_with(p.as_str()))
                    && !self.remove_suffixes.iter().any(|s| k.ends_with(s.as_str()))
            })
            .collect()
    }

    /// Apply to a base environment: [`strip`](Self::strip), then set.
    pub fn apply(
        &self,
        base: impl IntoIterator<Item = (OsString, OsString)>,
    ) -> Vec<(OsString, OsString)> {
        let mut env = self.strip(base);
        for (k, v) in &self.set {
            env.retain(|(ek, _)| ek != k.as_str());
            env.push((k.into(), v.into()));
        }
        env
    }

    pub fn shown_map(&self) -> BTreeMap<String, String> {
        self.shown.iter().cloned().collect()
    }
}

/// Resolves a secret to its value; the label names it in errors (`profiles.work.secrets.X`).
pub type SecretFn<'a> = dyn FnMut(&str, &Secret) -> Result<String, String> + 'a;

/// Build the environment plan for `name` (SPEC §10.10 steps 1–4; the caller adds ptyro's own
/// variables). `managed_root` is `<data>/profiles`.
pub fn plan(
    config: &Config,
    name: &str,
    managed_root: &Path,
    secret: &mut SecretFn<'_>,
) -> Result<Plan, String> {
    if !exists(config, name) {
        return Err(format!(
            "no profile named {name:?} (defined profiles: {})",
            names(config).join(", ")
        ));
    }
    let d = def(config, name).cloned().unwrap_or_default();
    let mut p = Plan {
        profile: name.to_owned(),
        remove: MANAGED_VARS.iter().map(|s| (*s).to_owned()).collect(),
        remove_prefixes: MANAGED_PREFIXES.iter().map(|s| (*s).to_owned()).collect(),
        ..Plan::default()
    };
    if isolates(config, name) {
        p.remove.extend(CREDENTIAL_VARS.iter().chain(CREDENTIAL_EXTRA).map(|s| (*s).to_owned()));
        p.remove_prefixes.extend(CREDENTIAL_PREFIXES.iter().map(|s| (*s).to_owned()));
        p.remove_suffixes.extend(CREDENTIAL_SUFFIXES.iter().map(|s| (*s).to_owned()));
        p.remove.extend(d.scrub_extra.iter().cloned());
    }
    let put = |p: &mut Plan, k: &str, v: String, shown: Option<String>| {
        p.shown.retain(|(sk, _)| sk != k);
        p.set.retain(|(sk, _)| sk != k);
        p.shown.push((k.to_owned(), shown.unwrap_or_else(|| v.clone())));
        p.set.push((k.to_owned(), v));
    };
    let put_secret = |p: &mut Plan,
                      k: &str,
                      label: String,
                      s: &Secret,
                      f: &mut SecretFn<'_>|
     -> Result<(), String> {
        let v = f(&label, s)?;
        put(p, k, v, Some(format!("<secret:{k}>")));
        Ok(())
    };
    for agent in ACCOUNTS {
        let var = crate::dir_var(agent).expect("known account");
        let account = d.account(agent);
        let dir = match account.and_then(|a| a.dir()) {
            Some(dir) => Some(expand_home(dir)),
            // Never fall back to the agent's default dir for a named profile: that would cross
            // accounts. `default` keeps the agent's usual dir.
            None if name != DEFAULT_PROFILE => {
                let dir = managed_root.join(name).join(agent);
                p.create_dirs.push(dir.clone());
                Some(dir)
            }
            None => None,
        };
        if let Some(dir) = dir {
            put(&mut p, var, dir.to_string_lossy().into_owned(), None);
        }
        let Some(def) = account.and_then(|a| a.def()) else { continue };
        let keys: &[&str] = match *agent {
            "claude" => &["ANTHROPIC_API_KEY"],
            "codex" => &["OPENAI_API_KEY", "CODEX_API_KEY"],
            "gemini" => &["GEMINI_API_KEY"],
            _ => &[],
        };
        if let Some(s) = &def.api_key {
            for k in keys {
                put_secret(&mut p, k, format!("profiles.{name}.{agent}.api_key"), s, secret)?;
            }
        }
        if let Some(s) = &def.token {
            let k = match *agent {
                "claude" => "CLAUDE_CODE_OAUTH_TOKEN",
                "gh" => "GH_TOKEN",
                other => {
                    return Err(format!(
                        "profiles.{name}.{other}.token: only claude and gh take a token"
                    ));
                }
            };
            put_secret(&mut p, k, format!("profiles.{name}.{agent}.token"), s, secret)?;
        }
    }
    if let Some(g) = &d.git {
        if let Some(n) = &g.name {
            put(&mut p, "GIT_AUTHOR_NAME", n.clone(), None);
            put(&mut p, "GIT_COMMITTER_NAME", n.clone(), None);
        }
        if let Some(e) = &g.email {
            put(&mut p, "GIT_AUTHOR_EMAIL", e.clone(), None);
            put(&mut p, "GIT_COMMITTER_EMAIL", e.clone(), None);
        }
        if let Some(k) = &g.ssh_key {
            let key = expand_home(k);
            put(
                &mut p,
                "GIT_SSH_COMMAND",
                format!("ssh -i {} -o IdentitiesOnly=yes", shell_quote(&key.to_string_lossy())),
                None,
            );
        }
        if let Some(c) = &g.config {
            put(&mut p, "GIT_CONFIG_GLOBAL", expand_home(c).to_string_lossy().into_owned(), None);
        }
    }
    if let Some(f) = &d.env_file {
        let path = expand_home(f);
        let text = std::fs::read_to_string(&path)
            .map_err(|e| format!("profiles.{name}.env_file: {}: {e}", path.display()))?;
        for (k, v) in parse_env_file(&text) {
            put(&mut p, &k, v, None);
        }
    }
    for (k, v) in &d.env {
        put(&mut p, k, v.clone(), None);
    }
    for (k, s) in &d.secrets {
        put_secret(&mut p, k, format!("profiles.{name}.secrets.{k}"), s, secret)?;
    }
    put(&mut p, "PTYRO_PROFILE", name.to_owned(), None);
    Ok(p)
}

fn shell_quote(s: &str) -> String {
    if s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~".contains(c)) {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// `KEY=value` lines; `#` comments, `export `, and single or double quotes.
pub fn parse_env_file(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        // A name a shell could export: anything else would only fail later, in the child.
        if k.is_empty()
            || k.starts_with(|c: char| c.is_ascii_digit())
            || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            continue;
        }
        out.push((k.to_owned(), env_value(v.trim())));
    }
    out
}

/// A value as `sh` reads it from an env file: `"double"` (with `\n \" \\ \$` escapes), `'single'`
/// (literal), or bare (up to a ` #` comment).
fn env_value(v: &str) -> String {
    if let Some(rest) = v.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = rest.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return out,
                '\\' => match chars.next() {
                    Some('n') => out.push('\n'),
                    Some('t') => out.push('\t'),
                    Some(o @ ('"' | '\\' | '$' | '`')) => out.push(o),
                    Some(o) => {
                        out.push('\\');
                        out.push(o);
                    }
                    None => out.push('\\'),
                },
                c => out.push(c),
            }
        }
        // No closing quote: keep what was written, as before.
        return format!("\"{rest}");
    }
    if let Some(rest) = v.strip_prefix('\'') {
        return rest.split_once('\'').map_or_else(|| format!("'{rest}"), |(s, _)| s.to_owned());
    }
    v.split_once(" #").map_or(v, |(a, _)| a).trim_end().to_owned()
}

/// What clients may know about a profile (no secret values).
pub fn info(config: &Config, name: &str, managed_root: &Path) -> ProfileInfo {
    let d = def(config, name).cloned().unwrap_or_default();
    let mut dirs = BTreeMap::new();
    for agent in ACCOUNTS {
        let dir = match d.account(agent).and_then(|a| a.dir()) {
            Some(dir) => Some(expand_home(dir)),
            None if name != DEFAULT_PROFILE => Some(managed_root.join(name).join(agent)),
            None => crate::default_dir(agent),
        };
        if let Some(dir) = dir {
            dirs.insert((*agent).to_owned(), dir);
        }
    }
    let mut vars: Vec<String> = Vec::new();
    let _ = plan(config, name, managed_root, &mut |_, _| Ok(String::new())).map(|p| {
        vars = p.set.into_iter().map(|(k, _)| k).collect();
    });
    ProfileInfo {
        name: name.to_owned(),
        label: d.label.clone().unwrap_or_else(|| name.to_owned()),
        color: d.color.clone(),
        defined: config.profiles.contains_key(name),
        isolate: isolates(config, name),
        dirs,
        git_email: d.git.as_ref().and_then(|g| g.email.clone()),
        matches: d.matches.clone(),
        vars,
    }
}

/// One `share` entry's outcome (SPEC §10.10 `profile sync`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShareOutcome {
    Linked(PathBuf),
    AlreadyLinked(PathBuf),
    /// A real file or another link is there; never overwritten.
    Exists(PathBuf),
    MissingSource(PathBuf),
    Refused(String),
}

/// A `share` entry as a relative path of plain names, or `None` for one that is empty, absolute
/// or climbs out (`..`).
fn share_path(entry: &str) -> Option<PathBuf> {
    use std::path::Component;
    let mut rel = PathBuf::new();
    for c in Path::new(entry).components() {
        match c {
            Component::Normal(n) => rel.push(n),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!rel.as_os_str().is_empty()).then_some(rel)
}

/// Symlink each `share` entry from the agent's default dir into the profile's dir. Never
/// overwrites, never shares credentials, and skips profiles whose dir *is* the default dir.
pub fn sync_shares(config: &Config, name: &str, managed_root: &Path) -> Vec<ShareOutcome> {
    sync_shares_from(config, name, managed_root, crate::default_dir)
}

/// [`sync_shares`] with the agents' default dirs given (tests).
pub fn sync_shares_from(
    config: &Config,
    name: &str,
    managed_root: &Path,
    default_dir: impl Fn(&str) -> Option<PathBuf>,
) -> Vec<ShareOutcome> {
    let Some(d) = def(config, name) else { return Vec::new() };
    let info = info(config, name, managed_root);
    let mut out = Vec::new();
    for (agent, entries) in &d.share {
        let (Some(src_root), Some(dst_root)) = (default_dir(agent), info.dirs.get(agent)) else {
            continue;
        };
        if same_dir(&src_root, dst_root) {
            continue;
        }
        for e in entries {
            // By components: `.credentials.json/`, `x/.`, `./.Credentials.json` and the like
            // are the same file, and `..` or an absolute path is no `share` entry at all.
            let Some(rel) = share_path(e) else {
                out.push(ShareOutcome::Refused(format!("{agent}/{e} is not a path inside it")));
                continue;
            };
            let holds = rel.iter().any(|c| {
                let c = c.to_string_lossy().to_lowercase();
                CREDENTIAL_FILES.iter().any(|f| f.to_lowercase() == c)
            });
            if holds {
                out.push(ShareOutcome::Refused(format!("{agent}/{e} holds credentials")));
                continue;
            }
            let (src, dst) = (src_root.join(&rel), dst_root.join(&rel));
            if !src.exists() {
                out.push(ShareOutcome::MissingSource(src));
                continue;
            }
            match std::fs::read_link(&dst) {
                Ok(target) if target == src => {
                    out.push(ShareOutcome::AlreadyLinked(dst));
                    continue;
                }
                Ok(_) => {
                    out.push(ShareOutcome::Exists(dst));
                    continue;
                }
                Err(_) if dst.exists() => {
                    out.push(ShareOutcome::Exists(dst));
                    continue;
                }
                Err(_) => {}
            }
            if let Some(parent) = dst.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match std::os::unix::fs::symlink(&src, &dst) {
                Ok(()) => out.push(ShareOutcome::Linked(dst)),
                Err(e) => out.push(ShareOutcome::Refused(format!("{}: {e}", dst.display()))),
            }
        }
    }
    out
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests;
