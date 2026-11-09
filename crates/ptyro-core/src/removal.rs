//! What each removal loses and what it keeps, one sentence each (SPEC §12.1, owner 2026-11-09).
//!
//! The CLI's question, the TUI's dialog and the web's say the same words: the CLI and the TUI
//! use these, and `web/src` repeats them word for word (the test below checks that).

/// `chat rm`: the daemon asks with it (`ChatDelete` unconfirmed).
pub const CHAT: &str =
    "It goes from every list, on every device, with its side chats. This can't be undone.";
/// `project rm`.
pub const PROJECT: &str = "ptyro forgets it: its trust, its pin and its place in workspaces. Its worktrees stay on disk, and it won't register on its own again.";
/// `project rm --release`, after `PROJECT`; also the TUI's and the web's offer.
pub const PROJECT_RELEASE: &str = "Its running sessions keep going, outside any project.";
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
/// `config rm`.
pub const CONFIG_ENTRY: &str = "Everything set in it goes too.";

/// Every sentence, for the check that the web says them too.
pub const ALL: &[&str] = &[
    CHAT,
    PROJECT,
    PROJECT_RELEASE,
    WORKTREE,
    WORKTREE_KEEP_BRANCH,
    SAFE,
    FORCED,
    ARCHIVE,
    WORKSPACE,
    PROFILE,
    PROFILE_FORCE,
    DEVICE,
    MACHINE,
    SET,
    SET_MEMBER,
    DISCARD,
    DISCARD_REST,
    CONFIG_ENTRY,
];

#[cfg(test)]
mod tests {
    #[test]
    fn the_web_says_the_same_words() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src");
        let mut web = String::new();
        for e in std::fs::read_dir(&dir).expect("web/src") {
            let p = e.expect("entry").path();
            if p.extension().is_some_and(|x| x == "ts") {
                web.push_str(&std::fs::read_to_string(&p).expect("read"));
            }
        }
        // The web never keeps a branch or lets sessions go before asking: the CLI's flags.
        let cli_only = [super::WORKTREE_KEEP_BRANCH, super::PROJECT_RELEASE];
        let missing: Vec<&str> = super::ALL
            .iter()
            .copied()
            .filter(|s| !cli_only.contains(s) && !web.contains(s))
            .collect();
        assert!(missing.is_empty(), "web/src doesn't say these word for word: {missing:#?}");
    }
}
