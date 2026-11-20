//! What the knowledge base reads of a repository (SPEC §25.6.3): which files a checkout
//! holds, and its main branch's commits.

use std::path::Path;

use crate::{Git, Result};

/// One commit, as `git log` tells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    /// When it was committed.
    pub at_ms: u64,
    pub subject: String,
    pub body: String,
    /// The files it changed, as paths in the repository.
    pub files: Vec<String>,
}

/// Between two commits, and between a commit's fields: bytes no message holds.
const RECORD: char = '\x1e';
const FIELD: char = '\x1f';

impl Git {
    /// The files under `dir` that git knows or would add: tracked, and untracked that
    /// nothing ignores (`ls-files -co --exclude-standard`). Paths inside `dir`, with `/`.
    pub fn ls_files(&self, dir: &Path) -> Result<Vec<String>> {
        let out = self.run(dir, &["ls-files", "-co", "--exclude-standard", "-z"])?;
        Ok(out.split('\0').filter(|p| !p.is_empty()).map(str::to_owned).collect())
    }

    /// The branch whose history is the repository's: its default branch when that is a
    /// commit here, else whatever is checked out. `None`: a repository with no commit.
    pub fn history_tip(&self, dir: &Path) -> Option<(String, String)> {
        let branch = self.default_branch(dir).filter(|b| self.rev(dir, b).is_some());
        let name = branch.unwrap_or_else(|| "HEAD".into());
        let sha = self.rev(dir, &name)?;
        Some((name, sha))
    }

    /// The commits of `tip`'s history, newest first, at most `max`: all of them, or only
    /// those `since` (a commit) doesn't have. Merges are left out (they say nothing of their
    /// own). `here`: only commits that changed something under `dir` (a folder inside a
    /// repository).
    pub fn log(
        &self,
        dir: &Path,
        tip: &str,
        since: Option<&str>,
        max: usize,
        here: bool,
    ) -> Result<Vec<Commit>> {
        self.check_rev_arg("a commit", tip)?;
        let range = match since {
            Some(s) => {
                self.check_rev_arg("a commit", s)?;
                format!("{s}..{tip}")
            }
            None => tip.to_owned(),
        };
        let n = format!("-n{max}");
        let format = format!("--format={RECORD}%H{FIELD}%ct{FIELD}%s{FIELD}%b{FIELD}");
        let mut args = vec![
            "-c",
            "core.quotePath=false",
            "log",
            "--no-merges",
            "--name-only",
            n.as_str(),
            format.as_str(),
            range.as_str(),
        ];
        if here {
            args.extend(["--", "."]);
        }
        Ok(parse_log(&self.run(dir, &args)?))
    }
}

/// `git log`'s output in [`Git::log`]'s format. Whatever the text, it gives commits or
/// nothing: a record that is not one is passed over.
pub fn parse_log(out: &str) -> Vec<Commit> {
    out.split(RECORD)
        .filter_map(|record| {
            let mut fields = record.splitn(5, FIELD);
            let sha = fields.next()?.trim();
            let at = fields.next()?.trim().parse::<u64>().ok()?;
            let (subject, body, files) = (fields.next()?, fields.next()?, fields.next()?);
            if sha.len() < 7 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            Some(Commit {
                sha: sha.to_owned(),
                at_ms: at.saturating_mul(1000),
                subject: subject.trim().to_owned(),
                body: body.trim().to_owned(),
                files: crate::lines(files).map(|f| f.trim().to_owned()).collect(),
            })
        })
        .collect()
}
