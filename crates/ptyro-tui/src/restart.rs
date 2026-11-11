//! A TUI restarts itself onto a newer ptyro (SPEC §24.8): when the daemon it reconnects to runs
//! another version and this TUI's own binary on disk now answers that version (an update
//! replaced it, then upgraded the daemon live), it waits until nothing of its own is half done,
//! keeps its view (as on any exit) and ends with [`AttachEnd::Restart`]; the CLI then starts the
//! new binary in its place with the same arguments.

use crate::AttachEnd;
use crate::app::{App, AppMsg};

/// This TUI's version.
pub const OURS: &str = env!("CARGO_PKG_VERSION");

/// How long the TUI must have had no key, paste or click before it restarts: someone typing
/// into a session is never cut off mid-word.
const QUIET_FOR: std::time::Duration = std::time::Duration::from_secs(30);

/// `a` is a later version than `b` (`0.0.69` > `0.0.68`).
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
    };
    parts(a) > parts(b)
}

/// The version a `ptyro --version` line names (`ptyro 0.0.69`).
pub fn version_of(line: &str) -> Option<String> {
    let v = line.trim().strip_prefix("ptyro ")?.split_whitespace().next()?;
    (!v.is_empty()).then(|| v.to_owned())
}

impl App {
    /// After (re)connecting: a daemon of another version may mean an update replaced this
    /// binary. Asks the binary on disk which version it is now (off the event loop).
    pub(crate) fn check_binary(&mut self) {
        let Some(theirs) = self.client.as_ref().map(|c| c.daemon_version.clone()) else { return };
        if theirs == OURS || theirs.is_empty() || self.restart_to.is_some() || self.no_restart {
            return;
        }
        let Ok(exe) = std::env::current_exe() else { return };
        // A replaced file's ` (deleted)`, or an image an update renamed aside: the installed name.
        let exe = ptyro_agents::own::installed_name(&exe);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let out = tokio::process::Command::new(exe)
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .await;
            let on_disk = out.ok().and_then(|o| version_of(&String::from_utf8_lossy(&o.stdout)));
            let _ = tx.send(AppMsg::Binary { on_disk, daemon: theirs });
        });
    }

    /// What the binary on disk said: the daemon's version, so this TUI is the one left behind.
    pub(crate) fn got_binary(&mut self, on_disk: Option<String>, daemon: &str) {
        let Some(v) = on_disk else { return };
        if v != daemon || v == OURS {
            return;
        }
        self.toast(format!("ptyro {v} is installed: this TUI restarts into it once you're idle"));
        self.restart_to = Some(v);
        self.dirty = true;
    }

    /// Nothing of the TUI's own would be lost: no dialog, form, picker or menu open, no chat
    /// draft, no key sequence waiting for its next key, no tour. What a session holds is the
    /// daemon's and stays.
    pub(crate) fn idle_for_restart(&self) -> bool {
        self.last_input.elapsed() >= QUIET_FOR
            && self.overlay.is_none()
            && self.first_run.is_none()
            && self.panes.values().all(|p| p.copy.is_none())
            && self.tour.is_none()
            && !self.prefix
            && self.chat_drafts.values().all(|d| d.trim().is_empty())
            && !matches!(&self.chat_panel, Some(crate::chats::ChatPanel::View(v)) if !v.draft.trim().is_empty())
    }

    /// Started in an older TUI's place: say so, instead of a welcome. Whether it was. One that
    /// isn't the version it was started as (a wrapper, a binary replaced again) never restarts
    /// by itself again, so a restart can't loop.
    pub(crate) fn restarted(&mut self) -> bool {
        let Some((from, to)) = self.opts.restarted.take() else { return false };
        if to == OURS {
            self.toast(format!(
                "now ptyro {OURS} (was {from}): restarted in place, your view kept"
            ));
        } else {
            self.no_restart = true;
            self.toast_err(format!(
                "restarted to run ptyro {to}, but this is ptyro {OURS}: it won't restart by itself again (quit and start ptyro to try)"
            ));
        }
        true
    }

    /// From the tick: restart when it is due and nothing would be lost.
    pub(crate) fn maybe_restart(&mut self) {
        if self.quit.is_some() || self.lost_since.is_some() || !self.idle_for_restart() {
            return;
        }
        if let Some(to) = self.restart_to.take() {
            let session =
                self.opts.raw.then(|| self.focused_session().map(|id| id.full())).flatten();
            self.quit = Some(AttachEnd::Restart {
                from: OURS.to_owned(),
                to,
                workspace: self.workspace.clone(),
                session,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_read_from_the_version_line() {
        assert_eq!(version_of("ptyro 0.0.69\n").as_deref(), Some("0.0.69"));
        assert_eq!(version_of("ptyro 1.2.3-rc.1 (abc)").as_deref(), Some("1.2.3-rc.1"));
        assert_eq!(version_of("codex 0.1"), None);
        assert_eq!(version_of(""), None);
    }
}
