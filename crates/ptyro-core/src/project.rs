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
        let canonical = serde_json::to_string(self).unwrap_or_default();
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
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
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
