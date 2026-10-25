//! Detect existing multi-account setups (SPEC §10.10 onboarding): shell aliases and functions
//! that point an agent at a separate config dir, `export`s, extra `~/.claude-*` style dirs, and
//! git `includeIf` identities. Text only: nothing is evaluated and rc files are never edited.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Variables that select an account, and the agent each belongs to.
const VARS: &[(&str, &str)] = &[
    ("CLAUDE_CONFIG_DIR", "claude"),
    ("CODEX_HOME", "codex"),
    ("GEMINI_CLI_HOME", "gemini"),
    ("GH_CONFIG_DIR", "gh"),
];

const RC_FILES: &[&str] = &[
    ".zshrc",
    ".zshenv",
    ".zprofile",
    ".bashrc",
    ".bash_profile",
    ".bash_aliases",
    ".profile",
    ".config/fish/config.fish",
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Suggestion {
    pub profile: String,
    /// Agent (or `gh`) → config dir, as written (`~` kept).
    pub dirs: BTreeMap<String, String>,
    pub git_name: Option<String>,
    pub git_email: Option<String>,
    pub match_paths: Vec<String>,
    /// Where each fact came from (`~/.zshrc:12 alias claude-work`).
    pub sources: Vec<String>,
}

impl Suggestion {
    /// The profile as a `[profiles.<name>]` table for `config.toml`.
    pub fn to_toml(&self) -> String {
        // Every string through the TOML writer: a path or a name can hold a quote or a control
        // character, which Rust's `{:?}` escapes in ways TOML doesn't read.
        let q = |s: &dyn std::fmt::Display| toml::Value::String(s.to_string()).to_string();
        let mut s = format!("[profiles.{}]\n", self.profile);
        for (agent, dir) in &self.dirs {
            s.push_str(&format!("{agent} = {}\n", q(dir)));
        }
        if self.git_name.is_some() || self.git_email.is_some() {
            let mut parts = Vec::new();
            if let Some(n) = &self.git_name {
                parts.push(format!("name = {}", q(n)));
            }
            if let Some(e) = &self.git_email {
                parts.push(format!("email = {}", q(e)));
            }
            s.push_str(&format!("git = {{ {} }}\n", parts.join(", ")));
        }
        if !self.match_paths.is_empty() {
            let paths: Vec<String> = self.match_paths.iter().map(|p| q(p)).collect();
            s.push_str(&format!("match = {{ paths = [{}] }}\n", paths.join(", ")));
        }
        s
    }
}

/// `claude-work` → `work`, `cl_personal` → `personal`, `codexwork` → `codexwork`.
pub fn profile_from_alias(alias: &str) -> String {
    alias_suffix(alias).unwrap_or_else(|| ptyro_core::model::slugify(alias))
}

fn alias_suffix(alias: &str) -> Option<String> {
    for sep in ['-', '_', '.'] {
        if let Some((_, suffix)) = alias.split_once(sep)
            && !suffix.is_empty()
        {
            return Some(ptyro_core::model::slugify(suffix));
        }
    }
    None
}

/// The profile an alias or function suggests: its suffix (`claude-work`), else the account
/// dir's (`cw` running `~/.claude-work` → `work`), else the name itself.
fn profile_for(alias: &str, dir: &str) -> String {
    alias_suffix(alias)
        .or_else(|| dir_profile(dir))
        .unwrap_or_else(|| ptyro_core::model::slugify(alias))
}

fn clean(v: &str) -> String {
    v.trim_matches(['"', '\'']).replace("${HOME}", "~").replace("$HOME", "~")
}

/// Which `VAR=dir` assignments a command line makes: `VAR=dir cmd`, `env VAR=dir`,
/// `export VAR=dir`, and fish's `set -x VAR dir` / `set -gx` / `set -lx`.
fn assignments(text: &str) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    // Before splitting on braces: `${HOME}` is a word, not a block.
    let text = text.replace("${HOME}", "~");
    let words: Vec<&str> = text
        .split(|c: char| {
            c.is_whitespace() || c == ';' || c == '{' || c == '}' || c == '(' || c == ')'
        })
        .filter(|w| !w.is_empty())
        .collect();
    for (i, word) in words.iter().enumerate() {
        let word = word.trim_start_matches("env").trim();
        for (var, agent) in VARS {
            if let Some(v) = word.strip_prefix(var).and_then(|r| r.strip_prefix('=')) {
                let v = clean(v);
                if !v.is_empty() {
                    out.push((*agent, v));
                }
            }
        }
        // fish: `set -gx CLAUDE_CONFIG_DIR ~/.claude-work`
        if word == "set"
            && words.get(i + 1).is_some_and(|f| f.starts_with('-') && f.contains('x'))
            && let (Some(name), Some(value)) = (words.get(i + 2), words.get(i + 3))
            && let Some((_, agent)) = VARS.iter().find(|(v, _)| v == name)
        {
            out.push((*agent, clean(value)));
        }
    }
    out
}

/// Join multi-line function bodies onto their definition line: `name() {` … `}` (bash, zsh)
/// and `function name` … `end` (fish). Each item is (first line number, text).
fn logical_lines(text: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    let mut open: Option<(usize, String, i32, bool)> = None; // (line, text, depth, fish)
    for (n, line) in text.lines().enumerate() {
        let t = line.trim();
        if let Some((at, mut body, depth, fish)) = open.take() {
            body.push('\n');
            body.push_str(line);
            let depth = depth + t.matches('{').count() as i32 - t.matches('}').count() as i32;
            let closed = if fish { t == "end" } else { depth <= 0 };
            if closed {
                out.push((at, body));
            } else {
                open = Some((at, body, depth, fish));
            }
            continue;
        }
        let fish = t.starts_with("function ") && !t.contains('{') && !t.contains("()");
        let depth = t.matches('{').count() as i32 - t.matches('}').count() as i32;
        if fish || (depth > 0 && (t.contains("()") || t.starts_with("function "))) {
            open = Some((n, line.to_owned(), depth, fish));
        } else {
            out.push((n, line.to_owned()));
        }
    }
    if let Some((at, body, _, _)) = open {
        out.push((at, body));
    }
    out
}

/// One alias or function definition on a line: (name, body).
fn definition(line: &str) -> Option<(String, String)> {
    let l = line.trim();
    if let Some(rest) = l.strip_prefix("alias ") {
        // bash/zsh `alias n='…'`, fish `alias n '…'` or `alias n="…"`.
        let (name, body) = rest.split_once(['=', ' '])?;
        return Some((name.trim().to_owned(), body.trim().trim_matches(['"', '\'']).to_owned()));
    }
    let fish = l.starts_with("function ") && !l.contains('{') && !l.contains("()");
    let l = l.strip_prefix("function ").unwrap_or(l);
    let (name, body) = if fish {
        l.split_once(|c: char| c.is_whitespace() || c == '\n').unwrap_or((l, ""))
    } else {
        l.split_once("()").or_else(|| l.split_once(' '))?
    };
    let name = name.trim();
    if name.is_empty()
        || name.contains(char::is_whitespace)
        || (!fish && !body.contains('{') && !body.contains(';'))
    {
        return None;
    }
    Some((name.to_owned(), body.to_owned()))
}

fn display(home: &Path, p: &Path) -> String {
    p.strip_prefix(home).map_or_else(|_| p.display().to_string(), |r| format!("~/{}", r.display()))
}

/// Scan `home` (normally `$HOME`) for multi-account setups.
pub fn detect(home: &Path) -> Vec<Suggestion> {
    let mut by_profile: BTreeMap<String, Suggestion> = BTreeMap::new();
    let mut files: Vec<PathBuf> = RC_FILES.iter().map(|f| home.join(f)).collect();
    let mut seen_dirs: Vec<String> = Vec::new();
    let mut i = 0;
    while i < files.len() {
        let path = files[i].clone();
        let top_level = i < RC_FILES.len();
        i += 1;
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (n, line) in logical_lines(&text) {
            let line = line.as_str();
            let trimmed = line.trim();
            if trimmed.starts_with('#') {
                continue;
            }
            // One level of `source` / `.`.
            if top_level
                && let Some(f) =
                    trimmed.strip_prefix("source ").or_else(|| trimmed.strip_prefix(". "))
            {
                let f = f.trim().trim_matches(['"', '\'']);
                let f = f.replace("$HOME", "~").replace("${HOME}", "~");
                let p = match f.strip_prefix("~/") {
                    Some(r) => home.join(r),
                    None => PathBuf::from(&f),
                };
                if !files.contains(&p) {
                    files.push(p);
                }
                continue;
            }
            let at = format!("{}:{}", display(home, &path), n + 1);
            if let Some((name, body)) = definition(line) {
                for (agent, dir) in assignments(&body) {
                    let profile = profile_for(&name, &dir);
                    let s = by_profile
                        .entry(profile.clone())
                        .or_insert_with(|| Suggestion { profile, ..Suggestion::default() });
                    s.dirs.insert(agent.to_owned(), dir.clone());
                    s.sources.push(format!("{at} {name}"));
                    seen_dirs.push(dir);
                }
            } else if let Some(rest) = trimmed
                .strip_prefix("export ")
                .or_else(|| trimmed.starts_with("set ").then_some(trimmed))
            {
                for (agent, dir) in assignments(rest) {
                    let profile = dir_profile(&dir).unwrap_or_else(|| "default".into());
                    let s = by_profile
                        .entry(profile.clone())
                        .or_insert_with(|| Suggestion { profile, ..Suggestion::default() });
                    s.dirs.insert(agent.to_owned(), dir.clone());
                    s.sources.push(format!("{at} export"));
                    seen_dirs.push(dir);
                }
            }
        }
    }
    // Extra account dirs nobody points at: `~/.claude-work`, `~/.codex-personal`.
    if let Ok(entries) = std::fs::read_dir(home) {
        let mut names: Vec<String> = entries
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            for (prefix, agent) in
                [(".claude-", "claude"), (".codex-", "codex"), (".gemini-", "gemini")]
            {
                let Some(suffix) = name.strip_prefix(prefix) else { continue };
                let dir = format!("~/{name}");
                if seen_dirs.iter().any(|d| d == &dir || d.ends_with(&name))
                    || !home.join(&name).is_dir()
                {
                    continue;
                }
                let profile = ptyro_core::model::slugify(suffix);
                let s = by_profile
                    .entry(profile.clone())
                    .or_insert_with(|| Suggestion { profile, ..Suggestion::default() });
                s.dirs.entry(agent.to_owned()).or_insert(dir.clone());
                s.sources.push(format!("{dir} (directory)"));
            }
        }
    }
    for (gitdir, name, email, at) in include_if_identities(home) {
        let profile = Path::new(gitdir.trim_end_matches('/'))
            .file_name()
            .map(|f| ptyro_core::model::slugify(&f.to_string_lossy()))
            .unwrap_or_else(|| "work".into());
        let s = by_profile
            .entry(profile.clone())
            .or_insert_with(|| Suggestion { profile, ..Suggestion::default() });
        s.git_name = s.git_name.take().or(name);
        s.git_email = s.git_email.take().or(email);
        s.match_paths.push(format!(
            "{}**",
            if gitdir.ends_with('/') { gitdir.clone() } else { format!("{gitdir}/") }
        ));
        s.sources.push(at);
    }
    by_profile.into_values().collect()
}

/// `~/.claude-work` → `work`.
fn dir_profile(dir: &str) -> Option<String> {
    let leaf = dir.trim_end_matches('/').rsplit('/').next()?;
    let (_, suffix) = leaf.trim_start_matches('.').split_once(['-', '_'])?;
    Some(ptyro_core::model::slugify(suffix))
}

/// `[includeIf "gitdir:~/work/"] path = ~/.gitconfig-work` → the identity in that file.
fn include_if_identities(home: &Path) -> Vec<(String, Option<String>, Option<String>, String)> {
    let Ok(text) = std::fs::read_to_string(home.join(".gitconfig")) else { return Vec::new() };
    let mut out = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            current = l
                .strip_prefix("[includeIf \"gitdir:")
                .or_else(|| l.strip_prefix("[includeIf \"gitdir/i:"))
                .and_then(|r| r.strip_suffix("\"]"))
                .map(str::to_owned);
            continue;
        }
        let (Some(gitdir), Some(path)) = (
            &current,
            l.strip_prefix("path").map(|r| r.trim_start().trim_start_matches('=').trim()),
        ) else {
            continue;
        };
        let file = match path.strip_prefix("~/") {
            Some(r) => home.join(r),
            None => home.join(path),
        };
        let inc = std::fs::read_to_string(&file).unwrap_or_default();
        let field = |key: &str| {
            inc.lines().map(str::trim).find_map(|l| {
                l.strip_prefix(key).map(|r| {
                    r.trim_start().trim_start_matches('=').trim().trim_matches('"').to_owned()
                })
            })
        };
        out.push((
            gitdir.clone(),
            field("name"),
            field("email"),
            format!("~/.gitconfig includeIf gitdir:{gitdir}"),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real-world rc shapes (`tests/fixtures/rc/*`): each file is installed as the rc file its
    /// extension names, and the suggestions must equal `<file>.expected` exactly.
    /// `PTYRO_BLESS=1` rewrites the expectations (review the diff!).
    #[test]
    fn golden_rc_snippets() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rc");
        let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e != "expected"))
            .collect();
        cases.sort();
        assert!(cases.len() >= 4);
        for case in cases {
            let home = tempfile::tempdir().unwrap();
            let rc = match case.extension().and_then(|e| e.to_str()) {
                Some("zshrc") => ".zshrc",
                Some("bashrc") => ".bashrc",
                Some("fish") => ".config/fish/config.fish",
                other => panic!("unknown fixture kind {other:?}"),
            };
            let target = home.path().join(rc);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::copy(&case, &target).unwrap();
            let mut got = String::new();
            for s in detect(home.path()) {
                got.push_str(&s.to_toml());
                for src in &s.sources {
                    got.push_str(&format!("# {src}\n"));
                }
                got.push('\n');
            }
            let expected = case.with_extension(format!(
                "{}.expected",
                case.extension().unwrap().to_string_lossy()
            ));
            if std::env::var_os("PTYRO_BLESS").is_some() {
                std::fs::write(&expected, &got).unwrap();
            }
            let want = std::fs::read_to_string(&expected).unwrap_or_else(|_| {
                panic!("{} missing (PTYRO_BLESS=1 writes it)", expected.display())
            });
            assert_eq!(got, want, "{}", case.display());
        }
    }

    #[test]
    fn finds_the_owners_alias_setup() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::write(
            home.join(".zshrc"),
            "# my aliases\nalias claude-work='CLAUDE_CONFIG_DIR=~/.claude-work claude'\nalias codex-work=\"CODEX_HOME=$HOME/.codex-work codex\"\ncl-personal() { CLAUDE_CONFIG_DIR=~/.claude-me claude \"$@\"; }\nsource ~/.zsh_extra\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".zsh_extra"),
            "alias gh-work='GH_CONFIG_DIR=~/.config/gh-work gh'\n",
        )
        .unwrap();
        std::fs::create_dir(home.join(".claude-old")).unwrap();
        std::fs::create_dir(home.join(".claude-work")).unwrap();
        std::fs::write(
            home.join(".gitconfig"),
            "[user]\n  name = Me\n[includeIf \"gitdir:~/work/\"]\n  path = ~/.gitconfig-work\n",
        )
        .unwrap();
        std::fs::write(
            home.join(".gitconfig-work"),
            "[user]\n  email = me@acme.example\n  name = Work Me\n",
        )
        .unwrap();
        let found = detect(home);
        let get = |p: &str| {
            found.iter().find(|s| s.profile == p).unwrap_or_else(|| panic!("{p}: {found:?}"))
        };
        let work = get("work");
        assert_eq!(work.dirs["claude"], "~/.claude-work");
        assert_eq!(work.dirs["codex"], "~/.codex-work");
        assert_eq!(work.dirs["gh"], "~/.config/gh-work", "followed `source`");
        assert_eq!(work.git_email.as_deref(), Some("me@acme.example"));
        assert_eq!(work.match_paths, ["~/work/**"]);
        assert_eq!(get("personal").dirs["claude"], "~/.claude-me");
        assert_eq!(get("old").dirs["claude"], "~/.claude-old", "unreferenced dir");
        assert!(work.to_toml().contains("[profiles.work]\nclaude = \"~/.claude-work\""));
        assert_eq!(profile_from_alias("claude_Work"), "work");
    }
}
