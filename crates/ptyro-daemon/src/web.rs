//! What only the web app needs from the daemon (ROADMAP WEB1): the things the CLI and the TUI
//! read from the config or from disk themselves, here as requests, since a browser can do
//! neither.

use ptyro_core::{Error, ErrorCode, TermSize};
use ptyro_protocol::{AgentChoice, ReplayDoc, ReplayStep};
use ptyro_term::record::{self, Timed};
use ptyro_term::{AlacrittyEmulator, Emulator, FrameTracker, StyleTable};

use crate::daemon::Daemon;

/// A replay is sampled to about this many steps, and stops at about this many bytes (as JSON,
/// the web's encoding, it must fit one frame).
const REPLAY_STEPS: usize = 300;
const REPLAY_BYTES: usize = 6 << 20;

/// `prog` on the daemon's `PATH` (what sessions see).
fn on_path(prog: &str, path: &str) -> bool {
    std::env::split_paths(path).any(|d| d.join(prog).is_file())
}

impl Daemon {
    /// The agents a session can run: built-ins, then configured ones, each with whether its
    /// program is installed.
    pub(crate) fn agent_choices(&self) -> (Vec<AgentChoice>, String) {
        let config = self.config();
        let path = self
            .base_env
            .iter()
            .find(|(n, _)| n == "PATH")
            .map(|(_, v)| v.to_string_lossy().into_owned())
            .unwrap_or_default();
        let agents = ptyro_agents::names(&config)
            .into_iter()
            .map(|name| {
                let program = ptyro_agents::command(&config, &name)
                    .and_then(|argv| argv.into_iter().next())
                    .unwrap_or_default();
                let installed = if program.contains('/') {
                    std::path::Path::new(&program).is_file()
                } else {
                    on_path(&program, &path)
                };
                AgentChoice { name, program, installed }
            })
            .collect();
        (agents, ptyro_agents::default_agent(&config))
    }

    /// A session's recording (it may have ended long ago, or be in an earlier daemon run).
    fn recording(&self, addr: &str) -> Result<(String, Vec<Timed>), Error> {
        let id = match self.resolve(addr) {
            Ok(s) => s.id.full(),
            Err(_) => addr.to_owned(),
        };
        let dir = self.paths.data.join("sessions").join(&id);
        let recs = record::read_dir(&dir).map_err(|_| {
            Error::new(
                ErrorCode::NotFound,
                format!("no recording of {addr:?} (is `[recorder] enabled = false`?)"),
            )
        })?;
        Ok((id, recs))
    }

    /// The recording as frames the web app draws like a live screen: a keyframe, then the
    /// changes between about `REPLAY_STEPS` moments.
    pub(crate) fn replay(&self, addr: &str) -> Result<ReplayDoc, Error> {
        let (_, recs) = self.recording(addr)?;
        let size = record::first_size(&recs).unwrap_or(TermSize::DEFAULT);
        let duration_ms = recs.last().map_or(0, |r| r.t_us / 1000);
        let interval_ms = (duration_ms / REPLAY_STEPS as u64).max(40);
        let mut emu = AlacrittyEmulator::new(size, 10_000);
        let mut tracker = FrameTracker::new(&mut emu);
        let mut styles = StyleTable::default();
        let mut sink = ptyro_term::FeedOutput::default();
        let mut steps: Vec<ReplayStep> = Vec::new();
        let (mut since, mut next, mut bytes, mut truncated) = (None, 0u64, 0usize, false);
        let mut snap = |emu: &AlacrittyEmulator, tracker: &mut FrameTracker, at_ms: u64| -> usize {
            let keyframe = since.is_none();
            let delta = tracker.build(emu, since, &mut styles);
            since = Some(tracker.seq());
            let cost = delta.rows.iter().map(|r| r.runs.iter().map(|x| x.text.len() + 48).sum::<usize>()).sum::<usize>();
            steps.push(ReplayStep { at_ms, keyframe, delta });
            cost
        };
        for (i, r) in recs.iter().enumerate() {
            match &r.record {
                record::Record::Output(b) | record::Record::Keyframe(b) => emu.feed(b, &mut sink),
                record::Record::Resize(s) | record::Record::Start(s) => {
                    if emu.size() != *s {
                        emu.resize(*s);
                    }
                }
            }
            sink.replies.clear();
            sink.events.clear();
            let ms = r.t_us / 1000;
            if (ms >= next || i + 1 == recs.len()) && !matches!(r.record, record::Record::Start(_)) {
                tracker.update(&mut emu);
                bytes += snap(&emu, &mut tracker, ms);
                next = ms + interval_ms;
                if bytes > REPLAY_BYTES {
                    truncated = i + 1 < recs.len();
                    break;
                }
            }
        }
        Ok(ReplayDoc { size, duration_ms, steps, truncated })
    }

    /// The recording as `asciicast` (for any asciinema player) or the final `text`.
    pub(crate) fn export(&self, addr: &str, format: &str) -> Result<String, Error> {
        let (_, recs) = self.recording(addr)?;
        match format {
            "asciicast" | "cast" => Ok(record::asciicast(&recs, addr)),
            "text" | "txt" => {
                let size = record::first_size(&recs).unwrap_or(TermSize::DEFAULT);
                let mut emu = AlacrittyEmulator::new(size, 1_000_000);
                record::replay(&recs, &mut emu);
                let doc = ptyro_term::frame::scrollback_doc(&emu, u32::MAX);
                let mut t = ptyro_core::cache::plain_text(&ptyro_core::cache::doc_rows(&doc));
                if !t.ends_with('\n') {
                    t.push('\n');
                }
                Ok(t)
            }
            other => Err(Error::invalid(format!("unknown format {other:?} (asciicast, text)"))),
        }
    }
}
