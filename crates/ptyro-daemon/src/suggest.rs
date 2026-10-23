//! What a project's first worktree could use (ROADMAP UX1.11, SPEC §10.4–§10.5): a setup script
//! its lockfile implies, and the ignored `.env` files a worktree won't have. Offered once; a yes
//! writes them to `.ptyro.local.toml` (never the committed `.ptyro.toml`), and trusts them when
//! they're the project's only scripts.

use std::path::Path;
use std::sync::Arc;

use ptyro_core::model::SetupSuggestions;
use ptyro_core::project::{LOCAL_FILE, ProjectConfig};
use ptyro_core::{Error, now_ms};
use ptyro_git::Git;
use ptyro_protocol::Reply;

use crate::daemon::Daemon;
use crate::worktrees::blocking;

/// Lockfiles and the install each implies, most specific first.
const LOCKFILES: &[(&str, &str)] = &[
    ("pnpm-lock.yaml", "pnpm install --frozen-lockfile"),
    ("bun.lock", "bun install --frozen-lockfile"),
    ("bun.lockb", "bun install --frozen-lockfile"),
    ("yarn.lock", "yarn install --frozen-lockfile"),
    ("package-lock.json", "npm ci"),
    ("uv.lock", "uv sync"),
    ("poetry.lock", "poetry install"),
    ("Pipfile.lock", "pipenv sync"),
    ("Gemfile.lock", "bundle install"),
    ("composer.lock", "composer install"),
    ("mix.lock", "mix deps.get"),
    ("go.sum", "go mod download"),
];

/// What `root` suggests (without asking git: see [`ignored_env_files`]).
pub fn detect(root: &Path) -> (Option<(String, String)>, Vec<String>) {
    let setup = LOCKFILES
        .iter()
        .find(|(f, _)| root.join(f).is_file())
        .map(|(f, cmd)| ((*f).to_owned(), (*cmd).to_owned()));
    let env: Vec<String> = std::fs::read_dir(root)
        .map(|d| {
            let mut v: Vec<String> = d
                .flatten()
                .filter(|e| e.path().is_file())
                .filter_map(|e| e.file_name().to_str().map(str::to_owned))
                .filter(|n| n == ".env" || n.starts_with(".env."))
                // Templates are committed; the real files are what a new checkout lacks.
                .filter(|n| {
                    !n.ends_with(".example") && !n.ends_with(".sample") && !n.ends_with(".template")
                })
                .collect();
            v.sort();
            v
        })
        .unwrap_or_default();
    (setup, env)
}

/// Of `names` at `root`, the ones git ignores (so a new worktree won't have them).
fn ignored_env_files(root: &Path, names: Vec<String>) -> Vec<String> {
    let git = Git::new();
    names.into_iter().filter(|n| git.run(root, &["check-ignore", "-q", "--", n]).is_ok()).collect()
}

impl Daemon {
    fn offered_key(project: &str) -> String {
        format!("suggested:{project}")
    }

    async fn was_offered(&self, project: &str) -> bool {
        match &self.store {
            Some(store) => store.kv_get(Self::offered_key(project)).await.ok().flatten().is_some(),
            None => self.views.lock().expect("views").contains_key(&Self::offered_key(project)),
        }
    }

    async fn mark_offered(&self, project: &str, taken: bool) {
        let v = serde_json::json!({ "at": now_ms(), "taken": taken });
        match &self.store {
            Some(store) => {
                let _ = store.kv_set(Self::offered_key(project), v).await;
            }
            None => {
                self.views.lock().expect("views").insert(Self::offered_key(project), v);
            }
        }
    }

    /// What the project could use, minus what its config already has. Offered (`offered`) only
    /// before its first worktree.
    pub(crate) async fn suggestions(&self, addr: &str) -> Result<SetupSuggestions, Error> {
        let p = self.find_project(addr)?;
        let root = p.root.clone();
        let cfg = ProjectConfig::load(&root, &root).unwrap_or_default();
        let has_include = root.join(".worktreeinclude").is_file() || !cfg.worktree.copy.is_empty();
        let (setup, copy) = blocking(move || {
            let (setup, env) = detect(&root);
            (setup, ignored_env_files(&root, env))
        })
        .await?;
        let first = !self
            .registry
            .lock()
            .expect("registry")
            .worktrees
            .iter()
            .any(|w| w.project == p.id && !w.is_root);
        let setup = setup.filter(|_| cfg.scripts.setup.is_none());
        Ok(SetupSuggestions {
            project: p.slug.clone(),
            from: setup.as_ref().map(|(f, _)| f.clone()),
            setup: setup.map(|(_, cmd)| cmd),
            copy: if has_include { Vec::new() } else { copy },
            offered: !first || self.was_offered(&p.id).await,
        })
    }

    /// `ProjectSuggest`: report, take (write `.ptyro.local.toml`) or decline.
    pub(crate) async fn project_suggest(
        self: &Arc<Self>,
        addr: &str,
        apply: Option<bool>,
    ) -> Result<Reply, Error> {
        let p = self.find_project(addr)?;
        let s = self.suggestions(addr).await?;
        match apply {
            None => {}
            Some(false) => self.mark_offered(&p.id, false).await,
            Some(true) => {
                if !s.is_empty() {
                    let (root, s2) = (p.root.clone(), s.clone());
                    blocking(move || write_local(&root, &s2)).await??;
                    // The project's only scripts, approved just now: trusted.
                    let cfg = ProjectConfig::load(&p.root, &p.root).unwrap_or_default();
                    let only_ours = cfg.scripts.archive.is_none()
                        && cfg.scripts.after_turn.is_none()
                        && cfg.scripts.run.is_empty();
                    if only_ours && let Some(wt) = self.root_worktree(&p.id) {
                        let info = self.scripts_info(&p, &wt)?;
                        self.trust(&p.id, &info.fingerprint);
                    }
                }
                self.mark_offered(&p.id, true).await;
            }
        }
        Ok(Reply::Suggestions { suggestions: self.suggestions(addr).await? })
    }
}

/// Add the suggestions to `<root>/.ptyro.local.toml`, keeping what's there (and its comments).
fn write_local(root: &Path, s: &SetupSuggestions) -> Result<(), Error> {
    let path = root.join(LOCAL_FILE);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        "# Your own settings for this repository (not committed; see `ptyro project config`).\n"
            .to_owned()
    });
    let mut doc: toml_edit::DocumentMut =
        text.parse().map_err(|e| Error::invalid(format!("{}: {e}", path.display())))?;
    if let Some(setup) = &s.setup {
        let scripts = doc["scripts"].or_insert(toml_edit::table());
        if scripts.get("setup").is_none() {
            scripts["setup"] = toml_edit::value(setup.as_str());
        }
    }
    if !s.copy.is_empty() {
        let worktree = doc["worktree"].or_insert(toml_edit::table());
        if worktree.get("copy").is_none() {
            let mut a = toml_edit::Array::new();
            for f in &s.copy {
                a.push(f.as_str());
            }
            worktree["copy"] = toml_edit::value(a);
        }
    }
    std::fs::write(&path, doc.to_string())
        .map_err(|e| Error::internal(format!("{}: {e}", path.display())))?;
    // Never committed by accident.
    let _ = Git::new().exclude(root, &format!("/{LOCAL_FILE}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lockfiles_and_env_files_are_found() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("package-lock.json"), "{}").unwrap();
        std::fs::write(d.path().join("pnpm-lock.yaml"), "").unwrap();
        for f in [".env", ".env.local", ".env.example", "README"] {
            std::fs::write(d.path().join(f), "").unwrap();
        }
        let (setup, env) = detect(d.path());
        assert_eq!(setup, Some(("pnpm-lock.yaml".into(), "pnpm install --frozen-lockfile".into())));
        assert_eq!(env, vec![".env".to_owned(), ".env.local".to_owned()]);
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(detect(empty.path()), (None, Vec::new()));
    }

    #[test]
    fn suggestions_merge_into_the_local_file() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(LOCAL_FILE), "# mine\n[scripts]\nsetup = \"make deps\"\n")
            .unwrap();
        let s = SetupSuggestions {
            project: "p".into(),
            setup: Some("npm ci".into()),
            from: None,
            copy: vec![".env".into()],
            offered: false,
        };
        write_local(d.path(), &s).unwrap();
        let text = std::fs::read_to_string(d.path().join(LOCAL_FILE)).unwrap();
        assert!(text.contains("# mine") && text.contains("setup = \"make deps\""), "kept: {text}");
        assert!(!text.contains("npm ci"), "never overwrites: {text}");
        assert!(text.contains("copy = [\".env\"]"), "{text}");
    }
}
