//! What each removal loses and what it keeps, one sentence each (SPEC §12.1, owner 2026-11-09).
//!
//! The CLI's question, the TUI's dialog and the web's say the same words: the CLI and the TUI
//! use these, and `web/src` repeats them word for word in the same dialogs (`DIALOGS`, checked
//! by the test below).

/// `chat rm`: the daemon asks with it (`ChatDelete` unconfirmed).
pub const CHAT: &str = "It goes from every list, on every device. This can't be undone.";
/// `kb forget`: the daemon asks with it (`KbForget` unconfirmed).
pub const KB: &str = "The text is deleted from the knowledge base for good, and nothing of it is kept again. Your files and the agents' own transcripts aren't touched.";
/// `kb forget` of a note: after the count, in place of `KB` (or after it, with other items).
pub const KB_NOTE: &str = "A note's file is deleted from the notes folder. This can't be undone.";
/// `project rm`.
pub const PROJECT: &str = "ptyro forgets it: its trust, its pin and its place in workspaces. Its worktrees stay on disk, and it won't register on its own again.";
/// Letting a project's running sessions go: after `PROJECT` in `project rm --release`, and in
/// the daemon's refusal that every client's "Let them go?" offer shows.
pub const PROJECT_RELEASE: &str = "Sessions let go keep running, outside any project.";
/// `wt rm` (the daemon's rule: `BranchFate`).
pub const WORKTREE: &str = "Its directory is deleted. Its branch is deleted too if it is merged into its base or the same as its pushed copy; otherwise it is kept.";
/// `wt rm --keep-branch`.
pub const WORKTREE_KEEP_BRANCH: &str = "Its directory is deleted; its branch is kept.";
/// After a removal of worktrees that refuses to lose work (`wt rm`, `discard`).
pub const SAFE: &str = "Nothing is removed if that would lose work.";
/// The same, forced (`--force`, the web's "Remove anyway").
pub const FORCED: &str = "Work that isn't saved anywhere else is thrown away.";
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
/// `config rm`.
pub const CONFIG_ENTRY: &str = "Everything set in it goes too.";
/// `rm` of a dormant session (owner, 2026-11-09: it asks, as `chat rm` does).
pub const DORMANT: &str =
    "Its agent's conversation, which it could resume, is forgotten. This can't be undone.";
/// `gc`'s question about a scratch shell's leftover directory (`ShellScratchRemove`).
pub const SCRATCH: &str =
    "A scratch shell left it behind. It is deleted with everything in it; this can't be undone.";
/// `daemon restart --force`, the TUI's `daemon.restart`, the web's Restart.
pub const DAEMON_RESTART: &str =
    "Running sessions are stopped; agents hibernate and come back resumable. Clients reconnect.";

/// Ending a session (SPEC §7.3): Stop, Kill, and Stop and remove, asked about in the TUI and
/// the web under `ui.confirm_kill` (`VERB NAME?` then the sentence).
pub const STOP: &str = "Its agent's turn is interrupted and it gets a moment to exit; then it's signalled. It stays in the list.";
/// Kill: SIGKILL.
pub const KILL: &str =
    "It's killed at once (SIGKILL), with no chance to clean up. It stays in the list.";
/// Stop and remove (`rm --stop`).
pub const STOP_REMOVE: &str = "It's stopped, then forgotten: it leaves the list.";
/// The sentences above, which the web's session dialogs (`ENDS` in `web/src/sessions.ts`) say.
pub const SESSION_ENDS: &[&str] = &[STOP, KILL, STOP_REMOVE];

/// Each dialog: how its question starts (in every client), and the sentences it says. The web
/// repeats them word for word in the dialog that asks that question (the test below checks it).
pub const DIALOGS: &[(&str, &[&str])] = &[
    ("Delete the chat", &[CHAT]),
    ("Forget 1 item", &[KB]),
    ("Forget project", &[PROJECT]),
    ("Remove worktree", &[WORKTREE, SAFE, FORCED]),
    ("Remove workspace", &[WORKSPACE]),
    ("Remove profile", &[PROFILE, PROFILE_FORCE]),
    ("Revoke device", &[DEVICE]),
    ("Stop mirroring", &[MACHINE]),
    ("Remove ${c.noun}", &[CONFIG_ENTRY]),
    ("Remove the dormant session", &[DORMANT]),
    ("Delete the scratch directory", &[SCRATCH]),
    ("Restart the daemon", &[DAEMON_RESTART]),
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

    /// The web's Stop, Kill, and Stop and remove dialogs take their words from one table.
    #[test]
    fn the_web_ends_a_session_in_the_same_words() {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/sessions.ts");
        let text = std::fs::read_to_string(p).expect("web/src/sessions.ts");
        for s in super::SESSION_ENDS {
            assert!(text.contains(&format!("what: {s:?}")), "web/src/sessions.ts ENDS: {s:?}");
        }
    }
}
