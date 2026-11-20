//! A shell's commands into the knowledge base (SPEC §25.6.5): the debatable source, so the
//! one that keeps least and says so.
//!
//! **When.** A command ends in a shell session (its OSC 133 `D` mark, from ptyro's shell
//! integration): the session hands over the command line, where it ran, how it ended and how
//! long it took ([`Daemon::kb_shell_ran`], off the session's path). Nothing is asked of the
//! shell and nothing is read from its screen.
//!
//! **What.** One item for each shell and local day, `shell://<session id>/<YYYY-MM-DD>`. A
//! command line is one `command` chunk; the same line in the same folder that day is the same
//! chunk again, counted. With `kb.shell = "output"` (off unless the user chose it) what the
//! command printed stands beside it as `output` chunks: the first 2 KiB and the last 6 KiB,
//! collected by the session's emulator only while that setting is on
//! ([`Daemon::kb_output_switch`]), and deleted after `kb.shell_output_days`.
//!
//! **Never.** A private or an unrecorded shell (the session never hands anything over, and
//! the gate refuses it besides); `kb.shell = "off"`; a command typed with a space before
//! it, a trivial one (`kb.shell_skip`), one that names a key or credential file
//! (`ptyro_core::kb::shell::left_out`): of these not even a line in `ptyro kb status`. The
//! output of a command that drew the screen (an editor, a pager), that was typed into while
//! it ran (a password prompt, a REPL, `ssh`: what it showed holds what was typed), or that
//! was an agent started by hand (its conversation is not a command's output). Commands an
//! agent runs are in its own session's transcript, not here: this is for shell sessions only.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use ptyro_core::kb::ingest::{Body, Source, Stamp};
use ptyro_core::kb::shell::{self, Ran};
use ptyro_core::kb::{ChunkKind, ItemKind, Writer};
use ptyro_core::session::SessionKind;
use ptyro_core::sync::MutexExt;
use ptyro_store::kb::KbDb;

use super::{Facts, Job, Prepared, Refused, store_err};
use crate::daemon::Daemon;
use crate::session::Session;

/// A command to keep, with what the daemon knew of its shell when it ended (the shell may
/// be gone by the time the actor takes it: `exit` closes a quick terminal).
pub(crate) struct ShellJob {
    pub uri: String,
    title: String,
    stamp: Stamp,
    facts: Facts,
    ran: Ran,
}

impl Daemon {
    /// Whether shells' emulators collect what commands print: only with the knowledge base
    /// on and `kb.shell = "output"`. At start and at every change of the configuration.
    pub(crate) fn kb_output_switch(&self) {
        let kb = self.config().kb;
        self.io.kb_output.store(kb.enabled && kb.shell_output(), Ordering::Relaxed);
    }

    /// A command ended in the shell `s`: queue it, unless nothing is kept of it. Only locks:
    /// nothing here waits.
    pub(crate) fn kb_shell_ran(&self, s: &Session, mut ran: Ran) {
        let kb = self.config().kb;
        if !kb.enabled || !kb.shell_commands() {
            return;
        }
        let m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        // (A private shell is never queued, §25.7a: the gate would refuse it too.)
        if m.kind != SessionKind::Shell || m.place.private || m.place.unrecorded {
            return;
        }
        if shell::left_out(&ran.text, &kb.shell_skip, &self.kb_rules().never) {
            return;
        }
        // What an agent started by hand showed is its conversation, and its own to keep.
        let program = ran.text.split_whitespace().next().unwrap_or_default();
        let program = program.rsplit('/').next().unwrap_or(program);
        if !kb.shell_output()
            || ptyro_agents::adapter_for_program(&self.config(), program).is_some()
        {
            ran.output = None;
        }
        let local = i64::try_from(ran.started_ms / 1000)
            .ok()
            .and_then(ptyro_pty::local_offset_at)
            .unwrap_or(0);
        let day = shell::day(ran.started_ms.saturating_add_signed(local.saturating_mul(1000)));
        let worktree = m.place.worktree.as_deref().and_then(|id| self.worktree_by_id(id));
        let stamp = Stamp {
            project: m.place.project.clone(),
            workspace: self.kb_workspace_id(m.place.workspace.as_deref()),
            worktree: m.place.worktree.clone(),
            branch: worktree.and_then(|w| w.branch),
            session: Some(s.id.full()),
            ..Stamp::new(Writer::User, Some(m.place.profile.clone()))
        };
        let job = ShellJob {
            uri: shell::uri(&s.id.full(), &day),
            title: format!("{} · {day}", m.title.as_deref().unwrap_or(&m.name)),
            stamp,
            facts: Facts { shell: true, ..Facts::default() },
            ran,
        };
        self.kb_enqueue(Job::Shell(Box::new(job)));
    }

    /// The actor's part: the gate, the count of the same command that day, the write.
    pub(super) async fn kb_shell(self: &Arc<Self>, job: ShellJob) -> Result<(), Refused> {
        let ShellJob { uri, title, stamp, mut facts, mut ran } = job;
        let db = self.kb_db()?;
        // Made private while it waited (`kb forget --session`): nothing more of it.
        facts.private = stamp
            .session
            .as_deref()
            .and_then(|id| id.parse::<ptyro_core::SessionId>().ok())
            .and_then(|id| self.session(id))
            .is_some_and(|s| s.meta.locked().place.private);
        facts.project_off =
            self.kb_project_off(stamp.project.as_deref(), stamp.worktree.as_deref());
        let passed = self.kb_gate(&db, &uri, &facts).await?;
        // (The setting may have changed while it waited.)
        if !self.config().kb.shell_output() {
            ran.output = None;
        }
        // A command line's own secrets (`-phunter2`, `export FOO_KEY=…`), before the rules
        // every source goes through.
        ran.text = shell::hide(&ran.text);
        // The chunk is found again by the line as it is stored: redacted.
        let secrets = self.kb_secrets(stamp.profile.as_deref()).await?;
        let stored = self.kb_rules().redactor.with_secrets(secrets).redact(ran.text.trim()).text;
        let anchor = shell::anchor(&ran.cwd, &stored);
        let item = db.item_by_uri(uri.clone()).await.map_err(store_err)?;
        let before = match &item {
            Some(i) => db
                .chunks(i.id.clone(), 0, 0)
                .await
                .map_err(store_err)?
                .iter()
                .find(|c| c.kind == ChunkKind::Command && c.anchor.as_deref() == Some(&anchor))
                .map_or(0, |c| shell::count(c.meta.as_ref())),
            None => 0,
        };
        let mut source =
            Source::new(ItemKind::Shell, uri, Body::Parts(shell::parts(&ran, &anchor, before + 1)));
        source.append = true;
        source.title = Some(title);
        source.started_ms = Some(ran.started_ms);
        source.ended_ms = Some(ran.ended_ms);
        self.kb_write(&db, passed, Prepared { source, stamp, facts }).await?;
        Ok(())
    }

    /// Retention of what commands printed (`kb.shell_output_days`; 0: as long as its
    /// command): in the daily pass.
    pub(crate) async fn kb_shell_output_pass(&self, db: &KbDb) -> usize {
        let cut = match self.config().kb.shell_output_days {
            0 => return 0,
            days => ptyro_core::now_ms().saturating_sub(u64::from(days) * 86_400_000),
        };
        match db.expire_chunks(ChunkKind::Output, cut).await {
            Ok(n) => {
                if n > 0 {
                    self.kb_changed([ItemKind::Shell]);
                    tracing::info!("knowledge base: what {n} old command(s) printed deleted");
                }
                n
            }
            Err(e) => {
                tracing::warn!("knowledge base retention: {}", store_err(e).message);
                0
            }
        }
    }
}

#[cfg(test)]
mod tests;
