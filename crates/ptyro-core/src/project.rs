//! Project config (SPEC §13.3): `<repo>/.ptyro.toml` (committed) layered with
//! `<repo>/.ptyro.local.toml` (uncommitted). Repository config may pick prompts, scripts and
//! worktree settings, never agent executables or profiles (SPEC §13.1).

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::StartAgents;

pub const FILE: &str = ".ptyro.toml";
pub const LOCAL_FILE: &str = ".ptyro.local.toml";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectSection {
    pub name: Option<String>,
    /// Which *configured* agent new sessions use by default.
    pub default_agent: Option<String>,
    /// Base for new worktrees, e.g. `origin/main`.
    pub base: Option<String>,
    /// Read other tools' committed config (Conductor, Superset, Codex, Claude Desktop, Cursor,
    /// Copilot) for scripts this file doesn't set (default true; SPEC §10.5).
    pub import: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShareMode {
    /// Copy-on-write clone; a symlink where the file system can't.
    #[default]
    Clone,
    Symlink,
    /// Copy-on-write clone where possible, else a full copy.
    Copy,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorktreeSection {
    /// Untracked files to copy into new worktrees (gitignore-style patterns).
    pub copy: Vec<String>,
    /// Heavy directories to share (`node_modules`, `.venv`).
    pub share: Vec<String>,
    pub share_mode: ShareMode,
    pub start_agents: Option<StartAgents>,
    pub branch_template: Option<String>,
}

/// `[verify]`: a turn is done only when this check passes (SPEC §15, M7.9). `check` names a
/// run script, so it is covered by the scripts' trust like everything else that executes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VerifySection {
    /// A run script's name (`test`, for `[scripts.run.test]`).
    pub check: Option<String>,
    pub max_bounces: Option<u32>,
    /// Verify every agent session in this project's worktrees, not only recipe steps.
    pub auto: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunMode {
    #[default]
    Concurrent,
    /// Starting a run script stops the run scripts of the project's other worktrees.
    Exclusive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunScript {
    pub command: String,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Scripts {
    pub setup: Option<String>,
    pub archive: Option<String>,
    /// After each agent turn (lint/format); run by the status engine from M4.
    pub after_turn: Option<String>,
    pub run_mode: RunMode,
    pub run: BTreeMap<String, RunScript>,
    /// Which shell runs them (WIN6). Left out of the trust fingerprint while `auto`, so
    /// scripts trusted before still are; choosing another changes what runs, so it asks again.
    pub shell: crate::shell::ScriptShell,
}

impl Scripts {
    pub fn is_empty(&self) -> bool {
        self.setup.is_none()
            && self.archive.is_none()
            && self.after_turn.is_none()
            && self.run.is_empty()
    }

    /// The run script `name`, or the default one (the one marked `default`, else the only one).
    pub fn run_script(&self, name: Option<&str>) -> Option<(&str, &RunScript)> {
        match name {
            Some(n) => self.run.get_key_value(n).map(|(k, v)| (k.as_str(), v)),
            None => self
                .run
                .iter()
                .find(|(_, r)| r.default)
                .or_else(|| (self.run.len() == 1).then(|| self.run.iter().next()).flatten())
                .map(|(k, v)| (k.as_str(), v)),
        }
    }

    /// A script by role: `setup`, `archive`, `after_turn`, or `run:<name>` / a run script name.
    pub fn command(&self, which: &str) -> Option<String> {
        match which {
            "setup" => self.setup.clone(),
            "archive" => self.archive.clone(),
            "after_turn" => self.after_turn.clone(),
            other => {
                let name = other.strip_prefix("run:").unwrap_or(other);
                self.run.get(name).map(|r| r.command.clone())
            }
        }
    }

    /// Everything that would run, as shown to the user before trusting it.
    pub fn describe(&self) -> String {
        self.describe_from(&BTreeMap::new())
    }

    /// [`describe`](Self::describe), naming the file each script was imported from.
    pub fn describe_from(&self, origin: &BTreeMap<String, String>) -> String {
        let mut out = String::new();
        let mut put = |key: &str, title: &str, body: &str| {
            let from = origin
                .get(key)
                .filter(|f| !f.ends_with(FILE) && !f.ends_with(LOCAL_FILE))
                .map(|f| format!(" (from {f})"))
                .unwrap_or_default();
            out.push_str(&format!("── {title}{from} ──\n{}\n", body.trim_end()));
        };
        if let Some(s) = &self.setup {
            put("scripts.setup", "setup", s);
        }
        for (name, r) in &self.run {
            let title = format!("run.{name}{}", if r.default { " (default)" } else { "" });
            put(&format!("scripts.run.{name}"), &title, &r.command);
        }
        if let Some(s) = &self.archive {
            put("scripts.archive", "archive", s);
        }
        if let Some(s) = &self.after_turn {
            put("scripts.after_turn", "after_turn", s);
        }
        out
    }

    /// SHA-256 of the canonical JSON of the whole table (SPEC §10.5): any change re-asks.
    pub fn fingerprint(&self) -> String {
        let canonical = serde_json::to_string(&self.trusted_view()).unwrap_or_default();
        let digest = Sha256::digest(canonical.as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

impl Scripts {
    /// The files these scripts run with bash (a `.sh` file, or one whose `#!` line names a
    /// POSIX shell) that have Windows (CRLF) line endings in `checkout`: bash reads the `\r`
    /// as part of each command and fails. Paths relative to `checkout`. A `.bat`, `.cmd` or
    /// `.ps1` file is left alone (cmd wants CRLF, PowerShell doesn't mind).
    pub fn crlf_files(&self, checkout: &Path) -> Vec<String> {
        let commands = [&self.setup, &self.archive, &self.after_turn]
            .into_iter()
            .flatten()
            .chain(self.run.values().map(|r| &r.command));
        let mut out: Vec<String> = Vec::new();
        for word in commands.flat_map(|c| c.split_whitespace()) {
            let word = word.trim_matches(['"', '\'']);
            let word = word.strip_prefix("./").or_else(|| word.strip_prefix(".\\")).unwrap_or(word);
            if word.is_empty() || word.starts_with('-') || out.iter().any(|o| o == word) {
                continue;
            }
            let rel = Path::new(word);
            if rel.is_absolute()
                || rel.components().any(|c| matches!(c, std::path::Component::ParentDir))
            {
                continue;
            }
            let f = checkout.join(rel);
            let Ok(meta) = std::fs::symlink_metadata(&f) else { continue };
            if !meta.is_file() || meta.len() > 4 << 20 {
                continue;
            }
            let Ok(bytes) = std::fs::read(&f) else { continue };
            if bash_reads(rel, &bytes) && bytes.windows(2).any(|p| p == b"\r\n") {
                out.push(word.to_owned());
            }
        }
        out
    }

    /// What trust covers: every field as before `shell` existed, in the same order, and
    /// `shell` after them only when it isn't `auto`, so a fingerprint taken before is the same.
    fn trusted_view(&self) -> TrustedScripts<'_> {
        TrustedScripts {
            setup: &self.setup,
            archive: &self.archive,
            after_turn: &self.after_turn,
            run_mode: self.run_mode,
            run: &self.run,
            shell: (!self.shell.is_auto()).then_some(self.shell),
        }
    }
}

/// Whether bash (or sh) is what runs this file: a `.sh` file, or a `#!` line naming a POSIX
/// shell.
fn bash_reads(path: &Path, bytes: &[u8]) -> bool {
    if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("sh")) {
        return true;
    }
    let first = bytes.split(|b| *b == b'\n').next().unwrap_or_default();
    let first = String::from_utf8_lossy(first);
    first.starts_with("#!")
        && ["sh", "bash", "zsh", "dash"]
            .iter()
            .any(|s| first.split(['/', ' ', '\r', '\t']).any(|w| w == *s))
}

/// Rewrite `files` (relative to `checkout`) with LF line endings: a temporary file beside each,
/// then a rename, so a reader never sees half of one. Its permissions are kept. What was
/// rewritten, relative.
pub fn rewrite_lf(checkout: &Path, files: &[String]) -> std::io::Result<Vec<String>> {
    let mut done = Vec::new();
    for rel in files {
        let f = checkout.join(rel);
        let bytes = std::fs::read(&f)?;
        let mut lf = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                i += 1;
                continue;
            }
            lf.push(bytes[i]);
            i += 1;
        }
        if lf == bytes {
            continue;
        }
        let perms = std::fs::metadata(&f)?.permissions();
        let tmp = f.with_extension(format!("ptyro-lf-{}", std::process::id()));
        std::fs::write(&tmp, &lf)?;
        std::fs::set_permissions(&tmp, perms)?;
        std::fs::rename(&tmp, &f)?;
        done.push(rel.clone());
    }
    Ok(done)
}

#[derive(Serialize)]
struct TrustedScripts<'a> {
    setup: &'a Option<String>,
    archive: &'a Option<String>,
    after_turn: &'a Option<String>,
    run_mode: RunMode,
    run: &'a BTreeMap<String, RunScript>,
    #[serde(skip_serializing_if = "Option::is_none")]
    shell: Option<crate::shell::ScriptShell>,
}

impl ProjectConfig {
    /// Whether the script for `role` (`setup`, `archive`, `after_turn`, `run:<name>`) came from
    /// another tool's committed config (M5.10), which writes for bash.
    pub fn imported(&self, role: &str) -> bool {
        let key = match role.strip_prefix("run:") {
            Some(name) => format!("scripts.run.{name}"),
            None => format!("scripts.{role}"),
        };
        [key.clone(), format!("{key}.command")]
            .iter()
            .filter_map(|k| self.origin.get(k))
            .any(|f| f != FILE && f != LOCAL_FILE)
    }

    /// The argv `role`'s script line runs as here: `[scripts] shell`, `auto` by default (WIN6).
    pub fn script_argv(
        &self,
        role: &str,
        line: &str,
    ) -> Result<Vec<String>, crate::shell::ShellMissing> {
        let imported = self.imported(role);
        crate::shell::ScriptEnv::here(|env| {
            crate::shell::script_argv(self.scripts.shell, line, imported, env)
        })
    }

    /// What a person trusts when they trust a project's scripts: the scripts, and (when set)
    /// which of them is the verified-done check and whether it runs after every turn. With the
    /// defaults it is exactly [`Scripts::fingerprint`], so scripts trusted before still are.
    pub fn trust_fingerprint(&self) -> String {
        let v = &self.verify;
        if v.check.is_none() && !v.auto && v.max_bounces.is_none() {
            return self.scripts.fingerprint();
        }
        let canonical = serde_json::to_string(&(&self.scripts.trusted_view(), &self.verify))
            .unwrap_or_default();
        let digest = Sha256::digest(canonical.as_bytes());
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectConfig {
    pub project: ProjectSection,
    pub worktree: WorktreeSection,
    pub scripts: Scripts,
    /// Prompt snippets offered by the wizard and `--prompt @name`.
    pub prompts: BTreeMap<String, String>,
    /// Verified-done (M7.9): the run script agents' turns must pass.
    pub verify: VerifySection,
    /// Where each value came from: `scripts.setup` → `.ptyro.toml`, `scripts.run.web` →
    /// `.conductor/settings.toml` (`project config --explain`).
    #[serde(skip)]
    pub origin: BTreeMap<String, String>,
    /// Other tools' files that were read (M5.10), with what didn't carry over.
    #[serde(skip)]
    pub imported: Vec<crate::imports::Imported>,
}

/// Sections a repository may never contain.
const USER_ONLY: &[(&str, &str)] = &[
    ("agents", "agent commands"),
    ("profiles", "profiles"),
    ("ui", "UI settings"),
    ("daemon", "daemon settings"),
    ("keys", "key bindings"),
];

impl ProjectConfig {
    /// Parse one file's text (layering is [`load`](Self::load)'s job).
    pub fn parse(text: &str) -> Result<toml::Table, String> {
        let table: toml::Table =
            toml::from_str(text).map_err(|e| e.to_string().trim_end().to_owned())?;
        for (key, what) in USER_ONLY {
            if table.contains_key(*key) {
                return Err(format!(
                    "[{key}]: {what} can only be set in your user config, never in a repository (SPEC §13.1)"
                ));
            }
        }
        Ok(table)
    }

    fn from_table(t: toml::Table) -> Result<Self, String> {
        let c: Self =
            t.try_into().map_err(|e: toml::de::Error| e.to_string().trim_end().to_owned())?;
        // The base becomes a git revision argument: never let it read as an option.
        if c.project.base.as_deref().is_some_and(|b| b.trim_start().starts_with('-')) {
            return Err("project.base: must not start with `-`".into());
        }
        if let Some(t) = &c.worktree.branch_template {
            crate::model::check_branch_template(t)
                .map_err(|e| format!("worktree.branch_template: {e}"))?;
        }
        Ok(c)
    }

    /// `.ptyro.toml` from `checkout`, then `.ptyro.local.toml` from `root` (the main checkout) and
    /// from `checkout`, later files overriding earlier ones key by key. Missing files are fine.
    ///
    /// Then, unless `[project] import = false`, scripts it leaves unset come from other tools'
    /// committed config (M5.10): the first file in [`crate::imports::FILES`] order wins.
    pub fn load(root: &Path, checkout: &Path) -> Result<Self, String> {
        let mut merged = toml::Table::new();
        let mut origin = BTreeMap::new();
        let mut files = vec![(checkout.join(FILE), FILE), (root.join(LOCAL_FILE), LOCAL_FILE)];
        if checkout != root {
            files.push((checkout.join(LOCAL_FILE), LOCAL_FILE));
        }
        for (path, name) in files {
            // The committed file is whatever a clone brought: never through a link, never huge.
            // Your own local file may be a link a dotfiles manager keeps.
            let text = match crate::imports::read_repo_file(checkout, &path, name == LOCAL_FILE) {
                Ok(Some(t)) => t,
                Ok(None) => continue,
                Err(e) => return Err(format!("{}: {e}", path.display())),
            };
            let t = Self::parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            leaves(&t, "", &mut |k| {
                origin.insert(k, name.to_owned());
            });
            merge(&mut merged, t);
        }
        let mut c = Self::from_table(merged)?;
        c.origin = origin;
        if c.project.import != Some(false) {
            for i in crate::imports::discover(checkout) {
                c.absorb(i);
            }
        }
        Ok(c)
    }

    /// Take what `i` defines that nothing before it did.
    fn absorb(&mut self, mut i: crate::imports::Imported) {
        let file = i.file.clone();
        let mut took = false;
        let skip = |what: &str, by: &str, notes: &mut Vec<String>| {
            notes.push(format!("{what} not used: {by} defines it"));
        };
        for (key, mine, theirs) in [
            ("scripts.setup", &mut self.scripts.setup, i.scripts.setup.take()),
            ("scripts.archive", &mut self.scripts.archive, i.scripts.archive.take()),
        ] {
            let Some(t) = theirs else { continue };
            if mine.is_none() {
                *mine = Some(t);
                self.origin.insert(key.into(), file.clone());
                took = true;
            } else {
                let by = self.origin.get(key).cloned().unwrap_or_else(|| FILE.into());
                skip(key.trim_start_matches("scripts."), &by, &mut i.notes);
            }
        }
        let has_default = self.scripts.run.values().any(|r| r.default);
        for (name, mut r) in std::mem::take(&mut i.scripts.run) {
            let key = format!("scripts.run.{name}");
            if self.scripts.run.contains_key(&name) {
                let by = self.origin.get(&key).cloned().unwrap_or_else(|| FILE.into());
                skip(&format!("run.{name}"), &by, &mut i.notes);
                continue;
            }
            r.default &= !has_default;
            self.scripts.run.insert(name, r);
            self.origin.insert(key, file.clone());
            took = true;
        }
        if i.scripts.run_mode == RunMode::Exclusive && !self.origin.contains_key("scripts.run_mode")
        {
            self.scripts.run_mode = RunMode::Exclusive;
            self.origin.insert("scripts.run_mode".into(), file.clone());
        }
        if took || !i.notes.is_empty() {
            self.imported.push(i);
        }
    }
}

/// Every value's dotted key, treating a run script or a prompt as one value.
fn leaves(t: &toml::Table, prefix: &str, f: &mut impl FnMut(String)) {
    for (k, v) in t {
        let key = if prefix.is_empty() { k.clone() } else { format!("{prefix}.{k}") };
        match v {
            toml::Value::Table(sub)
                if key != format!("scripts.run.{k}") && key != format!("prompts.{k}") =>
            {
                leaves(sub, &key, f)
            }
            _ => f(key),
        }
    }
}

/// Deep merge: tables merge recursively, anything else is replaced.
fn merge(into: &mut toml::Table, from: toml::Table) {
    for (k, v) in from {
        match (into.get_mut(&k), v) {
            (Some(toml::Value::Table(a)), toml::Value::Table(b)) => merge(a, b),
            (_, v) => {
                into.insert(k, v);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC_EXAMPLE: &str = r#"
[project]
name = "myrepo"
default_agent = "claude"
base = "origin/main"

[worktree]
copy = [".env", "config/local.yml"]
share = ["node_modules"]
share_mode = "clone"

[scripts]
setup = """
pnpm install --frozen-lockfile
cp -n .env.example .env
"""
archive = "docker compose down -v"
run_mode = "exclusive"
after_turn = "pnpm -s lint --fix"

[scripts.run.dev]
command = "pnpm dev --port $PTYRO_PORT"
default = true

[scripts.run.test]
command = "pnpm test"

[prompts]
review = "Review the diff against origin/main and list risks."
"#;

    #[test]
    fn option_like_bases_are_refused() {
        let t = ProjectConfig::parse("[project]\nbase = \"--output=/tmp/x\"\n").unwrap();
        assert!(ProjectConfig::from_table(t).unwrap_err().contains("project.base"));
    }

    #[test]
    fn the_spec_example_parses() {
        let c = ProjectConfig::from_table(ProjectConfig::parse(SPEC_EXAMPLE).unwrap()).unwrap();
        assert_eq!(c.project.base.as_deref(), Some("origin/main"));
        assert_eq!(c.scripts.run_mode, RunMode::Exclusive);
        assert_eq!(c.scripts.run_script(None).unwrap().0, "dev");
        assert_eq!(c.scripts.command("run:test").as_deref(), Some("pnpm test"));
        assert!(c.scripts.describe().contains("pnpm install"));
    }

    #[test]
    fn imports_fill_only_what_ptyro_toml_leaves_unset() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        std::fs::create_dir_all(d.join(".conductor")).unwrap();
        std::fs::write(
            d.join(".conductor/settings.toml"),
            "[scripts]\nsetup = \"pnpm i\"\narchive = \"down\"\n[scripts.run.web]\ncommand = \"pnpm dev\"\ndefault = true\n",
        )
        .unwrap();
        std::fs::write(d.join(FILE), "[scripts]\nsetup = \"make deps\"\n[scripts.run.dev]\ncommand = \"make run\"\ndefault = true\n").unwrap();
        let c = ProjectConfig::load(d, d).unwrap();
        assert_eq!(c.scripts.setup.as_deref(), Some("make deps"), ".ptyro.toml wins");
        assert_eq!(c.scripts.archive.as_deref(), Some("down"), "the gap is filled");
        assert!(c.scripts.run["dev"].default && !c.scripts.run["web"].default);
        assert_eq!(c.origin["scripts.archive"], ".conductor/settings.toml");
        assert_eq!(c.origin["scripts.setup"], FILE);
        assert!(c.imported[0].notes.iter().any(|n| n.contains("setup not used")));
        let text = c.scripts.describe_from(&c.origin);
        assert!(text.contains("── archive (from .conductor/settings.toml) ──"), "{text}");
        assert!(text.contains("── setup ──"));
        std::fs::write(d.join(FILE), "[project]\nimport = false\n").unwrap();
        let off = ProjectConfig::load(d, d).unwrap();
        assert!(off.scripts.is_empty() && off.imported.is_empty());
    }

    #[test]
    fn repositories_cannot_define_agents_or_profiles() {
        let e = ProjectConfig::parse("[agents.claude]\ncommand = \"/tmp/evil\"\n").unwrap_err();
        assert!(e.contains("user config"), "{e}");
        assert!(ProjectConfig::parse("[profiles.x]\n").is_err());
        let e = ProjectConfig::from_table(
            ProjectConfig::parse("[project]\ncommand = \"x\"\n").unwrap(),
        )
        .unwrap_err();
        assert!(e.contains("command"), "{e}");
    }

    #[test]
    fn bash_scripts_with_crlf_are_found_and_rewritten_and_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        let w = |name: &str, text: &str| std::fs::write(d.path().join(name), text).unwrap();
        w("setup.sh", "#!/bin/bash\r\necho hi\r\n");
        w("build", "#!/usr/bin/env sh\r\nmake\r\n");
        w("go.bat", "@echo off\r\n:top\r\n");
        w("lf.sh", "echo fine\n");
        let mut s = Scripts {
            setup: Some("./setup.sh --fast && ./build".into()),
            archive: Some("go.bat".into()),
            ..Default::default()
        };
        s.run.insert("x".into(), RunScript { command: "sh lf.sh".into(), default: false });
        let found = s.crlf_files(d.path());
        assert_eq!(found, ["setup.sh", "build"], "the .bat keeps CRLF; lf.sh is fine");
        let done = rewrite_lf(d.path(), &found).unwrap();
        assert_eq!(done, found);
        assert_eq!(
            std::fs::read_to_string(d.path().join("setup.sh")).unwrap(),
            "#!/bin/bash\necho hi\n"
        );
        assert!(s.crlf_files(d.path()).is_empty());
        assert_eq!(
            std::fs::read_to_string(d.path().join("go.bat")).unwrap(),
            "@echo off\r\n:top\r\n"
        );
        // Outside the checkout is never looked at.
        let out = Scripts { setup: Some("../x.sh /etc/y.sh".into()), ..Default::default() };
        assert!(out.crlf_files(d.path()).is_empty());
    }

    #[test]
    fn an_imported_script_is_known_as_one() {
        let mut c = ProjectConfig::default();
        c.origin.insert("scripts.setup".into(), ".conductor/settings.toml".into());
        c.origin.insert("scripts.run.web.command".into(), FILE.into());
        c.origin.insert("scripts.run.dev".into(), "conductor.json".into());
        assert!(c.imported("setup"));
        assert!(!c.imported("run:web"));
        assert!(c.imported("run:dev"));
        assert!(!c.imported("archive"));
    }

    #[test]
    fn scripts_trusted_before_a_shell_could_be_chosen_stay_trusted() {
        let mut s = Scripts {
            setup: Some("./setup.sh".into()),
            after_turn: Some("make lint".into()),
            ..Default::default()
        };
        s.run.insert(
            "web".into(),
            RunScript { command: "npm run dev -- --port $PTYRO_PORT".into(), default: true },
        );
        // Taken with the code from before `[scripts] shell` (WIN6): it must not move.
        assert_eq!(
            s.fingerprint(),
            "448325184a537bdb87009544cb0c4b8ad87ce2fd7c26c88b1e6eb124901c61dc"
        );
        // Choosing a shell changes what runs: trust is asked for again.
        let bash = Scripts { shell: crate::shell::ScriptShell::Bash, ..s.clone() };
        assert_ne!(bash.fingerprint(), s.fingerprint());
    }

    #[test]
    fn local_file_layers_and_fingerprint_tracks_changes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(FILE),
            "[scripts]\nsetup = \"make deps\"\n[scripts.run.dev]\ncommand = \"make run\"\n",
        )
        .unwrap();
        let a = ProjectConfig::load(dir.path(), dir.path()).unwrap();
        std::fs::write(
            dir.path().join(LOCAL_FILE),
            "[scripts]\nsetup = \"make deps && make db\"\n",
        )
        .unwrap();
        let b = ProjectConfig::load(dir.path(), dir.path()).unwrap();
        assert_eq!(b.scripts.setup.as_deref(), Some("make deps && make db"));
        assert_eq!(b.scripts.run["dev"].command, "make run", "merged, not replaced");
        assert_ne!(a.scripts.fingerprint(), b.scripts.fingerprint());
        assert_eq!(a.scripts.fingerprint(), a.clone().scripts.fingerprint());
        assert!(
            ProjectConfig::load(&dir.path().join("nope"), &dir.path().join("nope"))
                .unwrap()
                .scripts
                .is_empty()
        );
    }
}
