//! What each removal loses and what it keeps, one sentence each (SPEC §12.1, owner 2026-11-09).
//!
//! The CLI's question, the TUI's dialog and the web's say the same words: the CLI and the TUI
//! use these, and `web/src` repeats them word for word in the same dialogs (`DIALOGS`, checked
//! by the test below).

/// `chat rm`: the daemon asks with it (`ChatDelete` unconfirmed).
pub const CHAT: &str =
    "It goes from every list, on every device, with its side chats. This can't be undone.";
/// `project rm`.
pub const PROJECT: &str = "ptyro forgets it: its trust, its pin and its place in workspaces. Its worktrees stay on disk, and it won't register on its own again.";
/// Letting a project's running sessions go: after `PROJECT` in `project rm --release`, and in
/// the daemon's refusal that every client's "Let them go?" offer shows.
pub const PROJECT_RELEASE: &str = "Sessions let go keep running, outside any project.";
/// `wt rm` (the daemon's rule: `BranchFate`).
pub const WORKTREE: &str = "Its directory is deleted. Its branch is deleted too if it is merged or the same as its pushed copy; otherwise it is kept.";
/// `wt rm --keep-branch`.
pub const WORKTREE_KEEP_BRANCH: &str = "Its directory is deleted; its branch is kept.";
/// After a removal of worktrees that refuses to lose work (`wt rm`, `cs`, `discard`).
pub const SAFE: &str = "Nothing is removed if that would lose work.";
/// The same, forced (`--force`, the web's "Remove anyway").
pub const FORCED: &str = "Work that isn't saved anywhere else is thrown away.";
/// `wt archive-rm`.
pub const ARCHIVE: &str = "Its saved files are deleted, and its branch too if it is merged or pushed. This can't be undone.";
/// `ws rm` (`WorkspaceRemove`).
pub const WORKSPACE: &str = "Its sessions keep running and move to main, with its chats and the projects no other workspace has.";
/// `profile rm`.
pub const PROFILE: &str = "It leaves your config; its directories aren't touched.";
/// `profile rm --force`, after `PROFILE`.
pub const PROFILE_FORCE: &str = "Whatever uses it falls back to the default profile.";
/// `devices revoke`.
pub const DEVICE: &str =
    "Its token stops working at once and its connections close. Pair it again to bring it back.";
/// `machine rm`.
pub const MACHINE: &str = "Its workspaces leave this list. Nothing changes on it, and its pairing stays until you revoke it there.";
/// `cs rm`, then `SAFE` or `FORCED`.
pub const SET: &str = "Every member's worktree is removed, or none is.";
/// `cs drop`, then `SAFE` or `FORCED`.
pub const SET_MEMBER: &str = "It leaves the set and its worktree is removed.";
/// `discard RUN CANDIDATE`, then `SAFE` or `FORCED`.
pub const DISCARD: &str = "Its agent stops and its worktree is removed.";
/// `discard RUN`: every candidate not picked; then `SAFE` or `FORCED`.
pub const DISCARD_REST: &str = "Their agents stop and their worktrees are removed.";

/// `SAFE`, or `FORCED` when `force`.
pub fn safety(force: bool) -> &'static str {
    if force { FORCED } else { SAFE }
}
/// `review rm --all`: comments typed on a worktree's diff, not sent yet.
pub const REVIEW_ALL: &str =
    "They haven't been sent to its agent, and what you typed in them can't be brought back.";
/// `config rm`.
pub const CONFIG_ENTRY: &str = "Everything set in it goes too.";

/// Each dialog: how its question starts (in every client), and the sentences it says. The web
/// repeats them word for word in the dialog that asks that question (the test below checks it).
pub const DIALOGS: &[(&str, &[&str])] = &[
    ("Delete the chat", &[CHAT]),
    ("Forget project", &[PROJECT]),
    ("Remove worktree", &[WORKTREE, SAFE, FORCED]),
    ("Delete the archive of", &[ARCHIVE]),
    ("Remove workspace", &[WORKSPACE]),
    ("Remove profile", &[PROFILE, PROFILE_FORCE]),
    ("Revoke device", &[DEVICE]),
    ("Stop mirroring", &[MACHINE]),
    ("Remove change set", &[SET, SAFE]),
    ("from change set", &[SET_MEMBER, SAFE]),
    ("Discard candidate", &[DISCARD, SAFE]),
    ("Discard every candidate", &[DISCARD_REST, SAFE]),
    ("Remove ${c.noun}", &[CONFIG_ENTRY]),
    ("Remove every comment on", &[REVIEW_ALL]),
];

#[cfg(test)]
mod tests {
    /// Per dialog, not anywhere in the sources: the line that asks the question (a web dialog
    /// is one `confirm(title, text, …)` call on one line) says its sentences.
    #[test]
    fn each_web_dialog_says_the_same_words() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src");
        let mut lines = Vec::new();
        for e in std::fs::read_dir(&dir).expect("web/src") {
            let p = e.expect("entry").path();
            if p.extension().is_some_and(|x| x == "ts") {
                let text = std::fs::read_to_string(&p).expect("read");
                lines.extend(text.lines().filter(|l| l.contains("confirm(")).map(str::to_owned));
            }
        }
        let mut wrong = Vec::new();
        for (title, says) in super::DIALOGS {
            let asks: Vec<&String> = lines.iter().filter(|l| l.contains(title)).collect();
            if asks.is_empty() {
                wrong.push(format!("no web dialog asks {title:?}"));
            }
            for s in *says {
                if !asks.iter().any(|l| l.contains(s)) {
                    wrong.push(format!("the web's {title:?} dialog doesn't say {s:?}"));
                }
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }
}
