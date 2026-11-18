//! Pair mode (M6.7): show a worktree's state in its project's main checkout, and carry the
//! edits made there back to the worktree when it is unpaired.

use ptyro_core::sync::MutexExt;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ptyro_core::Error;
use ptyro_git::Git;
use ptyro_protocol::Reply;

use crate::daemon::Daemon;
use crate::registry::git_err;
use crate::worktrees::blocking;

/// Pair mode in a project's main checkout (M6.7).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct Pair {
    pub worktree: String,
    /// What the main checkout had: a branch, else a commit.
    pub orig: String,
    pub orig_is_branch: bool,
    /// The user's own uncommitted work in the main checkout, stashed (commit id).
    pub stash: Option<String>,
    /// The main checkout as ptyro left it after the last sync.
    pub synced: String,
    /// Untracked files ptyro copied in (removed again on unpair).
    pub copied: Vec<String>,
    /// Untracked files that were already there.
    pub preexisting: Vec<String>,
}

pub type Pairs = HashMap<String, Pair>;

fn untracked(git: &Git, dir: &Path) -> Vec<String> {
    git.run(dir, &["ls-files", "--others", "--exclude-standard", "-z"])
        .map(|s| s.split('\0').filter(|x| !x.is_empty()).map(str::to_owned).collect())
        .unwrap_or_default()
}

/// The tracked changes in `dir` against `from`, as a binary patch.
fn patch(git: &Git, dir: &Path, from: &str) -> Result<String, Error> {
    git.run(dir, &["diff", "--binary", "--no-color", "--no-ext-diff", from, "--"]).map_err(git_err)
}

fn apply(git: &Git, dir: &Path, patch_text: &str) -> Result<(), Error> {
    if patch_text.trim().is_empty() {
        return Ok(());
    }
    // A worktree's changes can hold secrets: a 0600 file with a random name, removed on drop.
    let write = || -> std::io::Result<tempfile::NamedTempFile> {
        let mut f = tempfile::Builder::new().prefix("ptyro-pair-").suffix(".patch").tempfile()?;
        std::io::Write::write_all(&mut f, patch_text.as_bytes())?;
        Ok(f)
    };
    let tmp = write().map_err(|e| Error::internal(e.to_string()))?;
    let r =
        git.run(dir, &["apply", "--binary", "--whitespace=nowarn", &tmp.path().to_string_lossy()]);
    r.map(drop).map_err(|e| Error::conflict(format!("the changes don't apply cleanly ({e})")))
}

/// Copy `files` (paths relative to both) from one checkout to the other. Never through a link:
/// an agent can leave `f -> ~/.ssh/authorized_keys` in its worktree, and a copy that followed it
/// would write there (or carry a key out as an ordinary file). A source that is not a plain file,
/// or a destination that is a link or sits behind one, is skipped.
fn copy_files(from: &Path, to: &Path, files: &[String]) -> Result<(), Error> {
    use ptyro_core::os::{no_follow, with_mode};
    use std::io::Write;
    for f in files {
        let rel = Path::new(f);
        if rel.is_absolute()
            || rel.components().any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            continue;
        }
        let (src, dst) = (from.join(rel), to.join(rel));
        let is_link =
            |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
        if !std::fs::symlink_metadata(&src).is_ok_and(|m| m.file_type().is_file())
            || rel.ancestors().any(|a| !a.as_os_str().is_empty() && is_link(&to.join(a)))
        {
            continue;
        }
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d).map_err(|e| Error::internal(e.to_string()))?;
        }
        let copy = || -> std::io::Result<()> {
            let mut input = no_follow(std::fs::OpenOptions::new().read(true), false).open(&src)?;
            let mode = ptyro_core::os::mode(&input.metadata()?).map_or(0o644, |m| m & 0o777);
            let mut out = no_follow(
                with_mode(
                    std::fs::OpenOptions::new().write(true).create(true).truncate(true),
                    mode,
                ),
                false,
            )
            .open(&dst)?;
            std::io::copy(&mut input, &mut out)?;
            out.flush()
        };
        copy().map_err(|e| Error::internal(format!("{f}: {e}")))?;
    }
    Ok(())
}

impl Daemon {
    pub(crate) async fn pair(
        self: &Arc<Self>,
        addr: &str,
        off: bool,
        refresh: bool,
    ) -> Result<Reply, Error> {
        let wt = self.find_worktree(addr)?;
        let p = self
            .project_by_id(&wt.project)
            .ok_or_else(|| Error::missing("project", &wt.project))?;
        let current = self.pairs.locked().get(&p.id).cloned();
        if let Some(c) = &current
            && c.worktree != wt.id
        {
            let other = self
                .worktree_by_id(&c.worktree)
                .map_or_else(|| c.worktree.clone(), |w| w.qualified());
            return Err(Error::conflict(format!(
                "the main checkout is paired with {other}: unpair it first"
            )));
        }
        if wt.is_root {
            return Err(Error::invalid(
                "pair a worktree with the main checkout, not the main checkout itself",
            ));
        }
        let env = self.tool_env(&wt).await?;
        let (root, path, id) = (p.root.clone(), wt.path.clone(), wt.id.clone());
        let label = wt.qualified();
        let result = blocking(move || -> Result<(Option<Pair>, Vec<String>), Error> {
            let git = Git::with_env(env);
            match (current, off) {
                (None, true) => Err(Error::conflict("the main checkout isn't paired")),
                (None, false) => {
                    pair_on(&git, &root, &path, &id, &label).map(|(p, n)| (Some(p), n))
                }
                (Some(mut c), false) => {
                    let notes = carry_back(&git, &root, &path, &c)?;
                    let (synced, copied) = sync_into(&git, &root, &path, &c.preexisting)?;
                    c.synced = synced;
                    c.copied = copied;
                    let mut n = notes;
                    n.push(if refresh {
                        format!("main checkout re-synced with {label}")
                    } else {
                        format!("already paired with {label}; re-synced")
                    });
                    Ok((Some(c), n))
                }
                (Some(c), true) => pair_off(&git, &root, &path, &c).map(|n| (None, n)),
            }
        })
        .await??;
        {
            let mut pairs = self.pairs.locked();
            match result.0 {
                Some(pair) => {
                    pairs.insert(p.id.clone(), pair);
                }
                None => {
                    pairs.remove(&p.id);
                }
            }
        }
        self.save_kv("pairs", serde_json::to_value(&*self.pairs.locked()).unwrap_or_default());
        for w in [Some(wt.clone()), self.root_worktree(&p.id)].into_iter().flatten() {
            let _ = self.refresh_status(&w.id).await;
        }
        Ok(Reply::Notes { notes: result.1 })
    }

    pub(crate) async fn load_pairs(&self) {
        let Some(store) = &self.store else { return };
        if let Ok(Some(v)) = store.kv_get("pairs".into()).await
            && let Ok(p) = serde_json::from_value(v)
        {
            *self.pairs.locked() = p;
        }
    }
}

/// Put the worktree's state into the main checkout (detached), stashing what was there.
fn pair_on(
    git: &Git,
    root: &Path,
    wt: &Path,
    id: &str,
    label: &str,
) -> Result<(Pair, Vec<String>), Error> {
    let mut notes = Vec::new();
    let orig_branch = git.current_branch(root);
    let orig = orig_branch
        .clone()
        .or_else(|| git.rev(root, "HEAD"))
        .ok_or_else(|| Error::internal("the main checkout has no HEAD"))?;
    let stash = if git.dirty(root) {
        git.run(
            root,
            &[
                "stash",
                "push",
                "--include-untracked",
                "--quiet",
                "-m",
                &format!("ptyro pair: {label}"),
            ],
        )
        .map_err(git_err)?;
        let sha = git.rev(root, "stash@{0}").ok_or_else(|| Error::internal("stash vanished"))?;
        notes.push(format!(
            "stashed the main checkout's uncommitted work ({})",
            &sha[..12.min(sha.len())]
        ));
        Some(sha)
    } else {
        None
    };
    let preexisting = untracked(git, root);
    let synced = sync_into(git, root, wt, &preexisting);
    let (synced, copied) = match synced {
        Ok(x) => x,
        Err(e) => {
            // Put everything back as it was.
            let _ = git.run(root, &["checkout", "--force", "--quiet", &orig]);
            if stash.is_some() {
                let _ = git.run(root, &["stash", "pop", "--quiet"]);
            }
            return Err(e);
        }
    };
    notes.push(format!(
        "the main checkout ({}) now shows {label}; edits you make there go back to it on `ptyro wt unpair`",
        root.display()
    ));
    Ok((
        Pair {
            worktree: id.to_owned(),
            orig,
            orig_is_branch: orig_branch.is_some(),
            stash,
            synced,
            copied,
            preexisting,
        },
        notes,
    ))
}

/// Make the main checkout show the worktree: its commit (detached), its uncommitted changes and
/// its new files. Returns the synced snapshot and the files copied in.
fn sync_into(
    git: &Git,
    root: &Path,
    wt: &Path,
    preexisting: &[String],
) -> Result<(String, Vec<String>), Error> {
    let head = git.rev(wt, "HEAD").ok_or_else(|| Error::internal("the worktree has no HEAD"))?;
    // Back to a clean state first: drop what an earlier sync applied (never the user's own
    // files; carry_back moved those to the worktree already).
    git.run(root, &["checkout", "--force", "--quiet", "--detach", &head]).map_err(git_err)?;
    git.run(root, &["reset", "--hard", "--quiet", &head]).map_err(git_err)?;
    for f in untracked(git, root) {
        if !preexisting.contains(&f) {
            let _ = std::fs::remove_file(root.join(&f));
        }
    }
    apply(git, root, &patch(git, wt, "HEAD")?)?;
    let new: Vec<String> =
        untracked(git, wt).into_iter().filter(|f| !preexisting.contains(f)).collect();
    copy_files(wt, root, &new)?;
    let synced = git.snapshot(root).unwrap_or(head);
    Ok((synced, new))
}

/// Edits made in the main checkout since the last sync go to the worktree.
fn carry_back(git: &Git, root: &Path, wt: &Path, c: &Pair) -> Result<Vec<String>, Error> {
    let mut notes = Vec::new();
    let tracked = patch(git, root, &c.synced)?;
    // Files the user created in the main checkout during the pair.
    let created: Vec<String> = untracked(git, root)
        .into_iter()
        .filter(|f| !c.preexisting.contains(f) && !c.copied.contains(f))
        .collect();
    // Files ptyro copied in that the user changed since.
    let changed_copies: Vec<String> = c
        .copied
        .iter()
        .filter(|f| {
            // Plain files on both sides: a link left where a copy was is not "an edit".
            let plain = |p: PathBuf| {
                std::fs::symlink_metadata(&p)
                    .is_ok_and(|m| m.file_type().is_file())
                    .then(|| std::fs::read(p).ok())
                    .flatten()
            };
            let (a, b) = (plain(root.join(f)), plain(wt.join(f)));
            a.is_some() && a != b
        })
        .cloned()
        .collect();
    if !tracked.trim().is_empty() {
        apply(git, wt, &tracked).map_err(|e| Error::conflict(format!("your edits in the main checkout don't apply to the worktree, so nothing changed: {}", e.message)))?;
        notes.push("carried your edits in the main checkout over to the worktree".into());
    }
    copy_files(root, wt, &created)?;
    copy_files(root, wt, &changed_copies)?;
    if !created.is_empty() {
        notes.push(format!("carried {} new file(s) over to the worktree", created.len()));
    }
    Ok(notes)
}

fn pair_off(git: &Git, root: &Path, wt: &Path, c: &Pair) -> Result<Vec<String>, Error> {
    let mut notes = carry_back(git, root, wt, c)?;
    let created: Vec<String> =
        untracked(git, root).into_iter().filter(|f| !c.preexisting.contains(f)).collect();
    git.run(root, &["reset", "--hard", "--quiet"]).map_err(git_err)?;
    for f in created {
        let _ = std::fs::remove_file(root.join(&f));
    }
    git.run(root, &["checkout", "--quiet", &c.orig]).map_err(git_err)?;
    notes.push(format!(
        "the main checkout is back on {}",
        if c.orig_is_branch { c.orig.clone() } else { c.orig[..12.min(c.orig.len())].to_owned() }
    ));
    if let Some(sha) = &c.stash {
        match git.run(root, &["stash", "apply", "--quiet", sha]) {
            Ok(_) => {
                let list = git.run(root, &["stash", "list", "--format=%H"]).unwrap_or_default();
                if let Some(i) = list.lines().position(|l| l.trim() == sha) {
                    let _ = git.run(root, &["stash", "drop", "--quiet", &format!("stash@{{{i}}}")]);
                }
                notes.push("restored the main checkout's uncommitted work".into());
            }
            Err(e) => notes.push(format!(
                "your stashed work didn't apply cleanly; it's still in `git stash list` ({e})"
            )),
        }
    }
    Ok(notes)
}

#[cfg(all(test, unix))] // symlinks
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[test]
    fn copying_between_checkouts_never_goes_through_a_link() {
        let d = tempfile::tempdir().unwrap();
        let (root, wt, secret) =
            (d.path().join("root"), d.path().join("wt"), d.path().join("secret"));
        for p in [&root, &wt, &secret] {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(secret.join("authorized_keys"), "mine").unwrap();
        std::fs::write(secret.join("id_ed25519"), "PRIVATE").unwrap();
        std::fs::write(root.join("f"), "from the main checkout").unwrap();
        std::fs::write(root.join("ok"), "plain").unwrap();
        // The agent's worktree: `f` was swapped for a link into the secret directory, and a
        // planted link would carry a key out as an ordinary file.
        symlink(secret.join("authorized_keys"), wt.join("f")).unwrap();
        symlink(secret.join("id_ed25519"), wt.join("leak")).unwrap();
        symlink(&secret, wt.join("dir")).unwrap();
        std::fs::write(root.join("dir_file"), "x").unwrap();
        copy_files(&root, &wt, &["f".into(), "ok".into(), "dir/x".into(), "../escape".into()])
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(secret.join("authorized_keys")).unwrap(),
            "mine",
            "not written through the link"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("ok")).unwrap(),
            "plain",
            "a plain file still copies"
        );
        assert!(!secret.join("x").exists(), "nor into a linked directory");
        assert!(!d.path().join("escape").exists());
        // Source side: a link in the worktree is not copied into the main checkout.
        copy_files(&wt, &root, &["leak".into()]).unwrap();
        assert!(!root.join("leak").exists(), "a planted link is not carried out as a file");
        // The mode comes along.
        std::fs::set_permissions(
            root.join("ok"),
            std::os::unix::fs::PermissionsExt::from_mode(0o750),
        )
        .unwrap();
        std::fs::remove_file(wt.join("ok")).unwrap();
        copy_files(&root, &wt, &["ok".into()]).unwrap();
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(wt.join("ok")).unwrap().permissions(),
        );
        // Within the umask, which another test binding a socket may hold at 077 for a moment
        // (it is the process's): the owner's bits always come, and nothing the source lacks.
        assert_eq!((mode & 0o700, mode & 0o777 & !0o750), (0o700, 0), "{mode:o}");
    }
}
