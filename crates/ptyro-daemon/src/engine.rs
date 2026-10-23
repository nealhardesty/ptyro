//! The status engine (SPEC §9.2): fuses signals from every source into one status per session,
//! and keeps OSC 7501 records the way the protocol says (rev 0.4: ids, subtrees, a 256-record
//! LRU, lifetimes across OSC 133 `A`, exit and RIS). Pure; the daemon feeds it.
//!
//! Precedence, in short:
//! - **High-confidence sources are peers** (process, hooks, sidecars, OSC 7501, self reports):
//!   the most recent one wins. Within 250 ms, the stronger source breaks a tie.
//! - **Lower ones apply only when the stronger ones went quiet** for `stale_after`: medium
//!   (titles, the Claude roster) after the high ones, low (notifications, the activity
//!   heuristic) after both, so a missed hook can't freeze a status forever.
//! - Every decision, applied or not, is kept for `ptyro status --explain`.

use std::collections::VecDeque;

use ptyro_core::status::{AgentStatus, Confidence, SignalSource, StatusRecord};
use ptyro_protocol::ExplainLine;
use ptyro_term::PspReport;

pub const TIE_MS: u64 = 250;
const EXPLAIN_KEEP: usize = 64;

/// A status change the daemon acts on (attention, notifications, after_turn).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub from: Option<AgentStatus>,
    /// How sure the previous status was (a guessed "working" earns no "finished" attention).
    pub from_confidence: Option<Confidence>,
    pub to: StatusRecord,
    /// How long it had been working, when it stopped working.
    pub worked_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Engine {
    pub record: Option<StatusRecord>,
    last_high_ms: Option<u64>,
    last_medium_ms: Option<u64>,
    last_applied: Option<(SignalSource, AgentStatus, u64)>,
    working_since: Option<u64>,
    /// A high-confidence signal that lost a tie: applied once the window passes, unless a newer
    /// high-confidence signal supersedes it.
    deferred: Option<(Input, u64)>,
    pub activity: Option<String>,
    pub explain: VecDeque<ExplainLine>,
    pub stale_after_ms: u64,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(300_000)
    }
}

/// What a status input says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    pub source: SignalSource,
    pub confidence: Confidence,
    pub status: AgentStatus,
    pub kind: Option<String>,
    pub detail: Option<String>,
    pub progress: Option<u8>,
}

impl Engine {
    pub fn new(stale_after_ms: u64) -> Self {
        Self {
            record: None,
            last_high_ms: None,
            last_medium_ms: None,
            last_applied: None,
            working_since: None,
            deferred: None,
            activity: None,
            explain: VecDeque::new(),
            stale_after_ms,
        }
    }

    /// When high- and medium-confidence sources last spoke and since when it has been working:
    /// carried across a live upgrade, so a restored hook status still outranks heuristics.
    pub fn clocks(&self) -> [Option<u64>; 3] {
        [self.last_high_ms, self.last_medium_ms, self.working_since]
    }

    pub fn restore_clocks(&mut self, [high, medium, working]: [Option<u64>; 3]) {
        (self.last_high_ms, self.last_medium_ms, self.working_since) = (high, medium, working);
    }

    fn note(&mut self, at_ms: u64, i: &Input, applied: bool, reason: String) {
        let mut sig = i.status.label().to_owned();
        if let Some(k) = &i.kind {
            sig.push_str(&format!("/{k}"));
        }
        if let Some(d) = &i.detail {
            sig.push_str(&format!(" {d:?}"));
        }
        self.explain.push_back(ExplainLine {
            at_ms,
            source: format!("{} ({:?})", i.source.label(), i.confidence).to_lowercase(),
            signal: sig,
            applied,
            reason,
        });
        while self.explain.len() > EXPLAIN_KEEP {
            self.explain.pop_front();
        }
    }

    fn stale(&self, t: Option<u64>, now: u64) -> bool {
        t.is_none_or(|t| now.saturating_sub(t) >= self.stale_after_ms)
    }

    /// Apply one status input at `now`; `Some` when the status (or its kind) changed.
    pub fn apply(&mut self, i: Input, now: u64) -> Option<Transition> {
        let allowed = match i.confidence {
            Confidence::High => Ok(()),
            Confidence::Medium if self.stale(self.last_high_ms, now) => Ok(()),
            Confidence::Low
                if self.stale(self.last_high_ms, now) && self.stale(self.last_medium_ms, now) =>
            {
                Ok(())
            }
            _ => {
                let t = if i.confidence == Confidence::Low && !self.stale(self.last_medium_ms, now)
                {
                    self.last_medium_ms
                } else {
                    self.last_high_ms
                };
                let ago = now.saturating_sub(t.unwrap_or(now)) / 1000;
                Err(format!(
                    "ignored: a more reliable source spoke {ago}s ago (stale after {}s)",
                    self.stale_after_ms / 1000
                ))
            }
        };
        if let Err(why) = allowed {
            self.note(now, &i, false, why);
            return None;
        }
        if i.confidence == Confidence::High
            && let Some((src, st, t)) = self.last_applied
            && now.saturating_sub(t) < TIE_MS
            && src < i.source
            && st != i.status
        {
            self.note(now, &i, false, format!("deferred: within {TIE_MS} ms of the stronger {} saying {}; applies unless something newer comes", src.label(), st.label()));
            self.deferred = Some((i, now));
            return None;
        }
        if i.confidence == Confidence::High {
            // Newer information supersedes a deferred one.
            self.deferred = None;
        }
        match i.confidence {
            Confidence::High => self.last_high_ms = Some(now),
            Confidence::Medium => self.last_medium_ms = Some(now),
            Confidence::Low => {}
        }
        if i.confidence == Confidence::High {
            self.last_applied = Some((i.source, i.status, now));
        }
        if let Some(r) = self.record.as_mut()
            && r.status == i.status
            && (r.kind == i.kind || i.kind.is_none())
        {
            // Same state: refresh what it's about, keep `since`. A weaker source never replaces
            // a stronger one's detail ("Bash: rm -rf build" from a hook beats 7501's "approve").
            // Except a different prompt while blocked: that is a new question, with its own
            // `since` (answers name the prompt by it).
            if r.status == AgentStatus::Blocked
                && i.detail.is_some()
                && r.detail.is_some()
                && i.detail != r.detail
                && i.source <= r.source
            {
                r.since_ms = now;
            }
            if i.detail.is_some() && (i.source <= r.source || r.detail.is_none()) {
                r.detail = i.detail.clone();
            }
            r.progress = i.progress.or(r.progress.filter(|_| r.status == AgentStatus::Working));
            if i.confidence > r.confidence {
                r.confidence = i.confidence;
                r.source = i.source;
            }
            self.note(now, &i, true, "unchanged; details refreshed".into());
            return None;
        }
        let from = self.record.as_ref().map(|r| r.status);
        let from_confidence = self.record.as_ref().map(|r| r.confidence);
        let worked_ms = match (from, self.working_since) {
            (Some(AgentStatus::Working), Some(t)) => now.saturating_sub(t),
            _ => 0,
        };
        self.working_since = (i.status == AgentStatus::Working).then_some(now);
        if i.status != AgentStatus::Working {
            self.activity = None;
        }
        let to = StatusRecord {
            status: i.status,
            kind: i.kind.clone(),
            detail: i.detail.clone(),
            confidence: i.confidence,
            source: i.source,
            since_ms: now,
            progress: i.progress,
        };
        let reason = match from {
            Some(f) => format!("applied: {} → {}", f.label(), i.status.label()),
            None => "applied: first status".into(),
        };
        self.note(now, &i, true, reason);
        self.record = Some(to.clone());
        Some(Transition { from, from_confidence, to, worked_ms })
    }

    /// Apply a deferred signal whose tie window has passed (the daemon calls this after
    /// [`TIE_MS`]).
    pub fn flush(&mut self, now: u64) -> Option<Transition> {
        let (i, at) = self.deferred.take()?;
        if now.saturating_sub(at) < TIE_MS {
            self.deferred = Some((i, at));
            return None;
        }
        // Past the window it is simply the most recent word.
        self.last_applied = None;
        self.apply(i, now)
    }

    pub fn has_deferred(&self) -> bool {
        self.deferred.is_some()
    }

    /// The process is gone: status is cleared (SPEC §9.1).
    pub fn clear(&mut self) {
        self.record = None;
        self.working_since = None;
        self.activity = None;
        self.last_applied = None;
    }

    /// Start over on who spoke last: a different program took over the terminal (UX1.4: an agent
    /// started in a shell, or back to the shell), so what the shell said no longer holds.
    /// `clear` drops the status too.
    pub fn forget_sources(&mut self, clear: bool) {
        self.last_high_ms = None;
        self.last_medium_ms = None;
        self.last_applied = None;
        self.deferred = None;
        if clear {
            self.record = None;
            self.activity = None;
            self.working_since = None;
        }
    }

    /// When high-confidence sources last spoke (for the heuristic and the roster poll).
    pub fn high_quiet_ms(&self, now: u64) -> u64 {
        self.last_high_ms.map_or(u64::MAX, |t| now.saturating_sub(t))
    }
}

/// OSC 7501 records of one terminal (spec rev 0.4).
#[derive(Debug, Default)]
pub struct PspRecords {
    /// (id, report, last-update sequence)
    recs: Vec<(Option<String>, PspReport, u64)>,
    seq: u64,
    /// A 7501 report arrived since the last reset: OSC 9;4 is no longer mapped.
    pub seen: bool,
}

pub const MAX_RECORDS: usize = 256;

impl PspRecords {
    /// The record that speaks for the session: the most urgent one (blocked > error > working >
    /// done > idle), the most recently updated among equals. A subtask that's blocked makes the
    /// session blocked even while the root says idle.
    pub fn effective(&self) -> Option<&PspReport> {
        let rank = |s: &str| match s {
            "blocked" => 4,
            "error" => 3,
            "working" => 2,
            "done" => 1,
            _ => 0,
        };
        self.recs.iter().max_by_key(|(_, r, s)| (rank(&r.state), *s)).map(|(_, r, _)| r)
    }

    fn snapshot(&self) -> Option<(String, Option<String>, Option<String>)> {
        self.effective().map(|r| (r.state.clone(), r.kind.clone(), r.msg.clone()))
    }

    /// Apply a report; `true` if the effective record changed.
    pub fn report(&mut self, mut r: PspReport) -> bool {
        let before = self.snapshot();
        self.seen = true;
        let id = r.id.clone();
        if r.state == "clear" {
            match &id {
                None => self.recs.clear(),
                Some(id) => self.recs.retain(|(rid, _, _)| {
                    rid.as_deref().is_none_or(|x| x != id && !x.starts_with(&format!("{id}/")))
                }),
            }
            return self.snapshot() != before;
        }
        // `app` is inherited from the nearest ancestor that has one.
        if r.app.is_none()
            && let Some(id) = &id
        {
            let mut parts: Vec<&str> = id.split('/').collect();
            while parts.pop().is_some() {
                let parent = parts.join("/");
                let found = self.recs.iter().find(|(rid, rr, _)| {
                    rr.app.is_some()
                        && if parent.is_empty() {
                            rid.is_none()
                        } else {
                            rid.as_deref() == Some(parent.as_str())
                        }
                });
                if let Some((_, rr, _)) = found {
                    r.app = rr.app.clone();
                    break;
                }
                if parent.is_empty() {
                    break;
                }
            }
        }
        self.seq += 1;
        self.recs.retain(|(rid, _, _)| *rid != id);
        self.recs.push((id, r, self.seq));
        while self.recs.len() > MAX_RECORDS {
            // Least recently updated goes first.
            if let Some(oldest) =
                self.recs.iter().enumerate().min_by_key(|(_, (_, _, s))| *s).map(|(i, _)| i)
            {
                self.recs.remove(oldest);
            }
        }
        self.snapshot() != before
    }

    /// A new shell prompt (OSC 133 `A`) or the process exiting: `working` and `blocked` records
    /// go; `done` and `error` survive (and `idle` too, which the spec allows). `true` if the
    /// effective record changed.
    pub fn drop_active(&mut self) -> bool {
        let before = self.snapshot();
        self.recs.retain(|(_, r, _)| r.state != "working" && r.state != "blocked");
        self.snapshot() != before
    }

    /// RIS.
    pub fn reset(&mut self) {
        self.recs.clear();
        self.seen = false;
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.recs.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.recs.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(source: SignalSource, confidence: Confidence, status: AgentStatus) -> Input {
        Input { source, confidence, status, kind: None, detail: None, progress: None }
    }

    fn hi(s: AgentStatus) -> Input {
        input(SignalSource::Hook, Confidence::High, s)
    }

    #[test]
    fn most_recent_high_wins_and_denials_return_to_idle() {
        let mut e = Engine::new(300_000);
        assert!(e.apply(hi(AgentStatus::Idle), 0).is_some());
        assert!(e.apply(hi(AgentStatus::Working), 1_000).is_some());
        let t = e
            .apply(Input { kind: Some("permission".into()), ..hi(AgentStatus::Blocked) }, 2_000)
            .unwrap();
        assert_eq!(t.worked_ms, 1_000);
        // A denial: no hook, only OSC 7501 says idle — and must win (SPEC §9.2).
        let t =
            e.apply(input(SignalSource::Osc, Confidence::High, AgentStatus::Idle), 5_000).unwrap();
        assert_eq!(t.from, Some(AgentStatus::Blocked));
    }

    #[test]
    fn ties_go_to_the_stronger_source() {
        let mut e = Engine::new(300_000);
        e.apply(hi(AgentStatus::Working), 0);
        // 7501 arrives 100 ms after a hook with a different story: deferred, not applied yet…
        assert!(
            e.apply(input(SignalSource::Osc, Confidence::High, AgentStatus::Idle), 100).is_none()
        );
        assert_eq!(e.record.as_ref().unwrap().status, AgentStatus::Working);
        assert!(!e.explain.back().unwrap().applied && e.has_deferred());
        assert!(e.flush(200).is_none(), "still inside the window");
        // …and applied when nothing newer came (a denial's lone 7501 idle).
        assert!(e.flush(400).is_some());
        assert_eq!(e.record.as_ref().unwrap().status, AgentStatus::Idle);
        // A newer high signal supersedes a deferred one (the Stop hook after 7501 done).
        e.apply(hi(AgentStatus::Working), 1_000);
        e.apply(input(SignalSource::Osc, Confidence::High, AgentStatus::Blocked), 1_050);
        e.apply(hi(AgentStatus::Idle), 1_100);
        assert!(!e.has_deferred() && e.flush(2_000).is_none());
        // Later, it's simply the most recent.
        assert!(
            e.apply(input(SignalSource::Osc, Confidence::High, AgentStatus::Working), 2_600)
                .is_some()
        );
        // The stronger source arriving second is not blocked by the tie rule.
        assert!(e.apply(hi(AgentStatus::Idle), 2_700).is_some());
    }

    #[test]
    fn low_confidence_waits_for_staleness() {
        let mut e = Engine::new(10_000);
        e.apply(hi(AgentStatus::Working), 0);
        let low = input(SignalSource::Heuristic, Confidence::Low, AgentStatus::Idle);
        assert!(e.apply(low.clone(), 5_000).is_none());
        assert!(e.explain.back().unwrap().reason.contains("more reliable"));
        assert!(e.apply(low.clone(), 10_000).is_some(), "stale after 10 s");
        // Medium shields low too.
        e.apply(input(SignalSource::Title, Confidence::Medium, AgentStatus::Working), 11_000);
        assert!(e.apply(low, 12_000).is_none());
    }

    #[test]
    fn same_status_refreshes_detail_without_transition() {
        let mut e = Engine::new(300_000);
        e.apply(hi(AgentStatus::Working), 0);
        assert!(
            e.apply(
                Input {
                    detail: Some("Bash: ls".into()),
                    progress: Some(30),
                    ..hi(AgentStatus::Working)
                },
                10
            )
            .is_none()
        );
        let r = e.record.clone().unwrap();
        assert_eq!((r.detail.as_deref(), r.progress, r.since_ms), (Some("Bash: ls"), Some(30), 0));
        // OSC 7501's vaguer words don't replace the hook's.
        e.apply(
            Input {
                detail: Some("working".into()),
                ..input(SignalSource::Osc, Confidence::High, AgentStatus::Working)
            },
            2_000,
        );
        assert_eq!(e.record.as_ref().unwrap().detail.as_deref(), Some("Bash: ls"));
    }

    fn psp(state: &str, id: Option<&str>) -> PspReport {
        PspReport { state: state.into(), id: id.map(Into::into), app: None, ..PspReport::default() }
    }

    #[test]
    fn records_ids_subtrees_lifetimes() {
        let mut r = PspRecords::default();
        assert!(r.report(PspReport { app: Some("tool".into()), ..psp("working", Some("build")) }));
        r.report(psp("blocked", Some("build/step")));
        assert_eq!(r.effective().unwrap().state, "blocked", "the most urgent record");
        assert_eq!(
            r.recs.iter().find(|x| x.0.as_deref() == Some("build/step")).unwrap().1.app.as_deref(),
            Some("tool"),
            "app inherited"
        );
        r.report(psp("idle", None));
        assert_eq!(
            r.effective().unwrap().state,
            "blocked",
            "a blocked subtask outranks an idle root"
        );
        // clear with an id removes the subtree only.
        r.report(psp("clear", Some("build")));
        assert_eq!(r.len(), 1);
        r.report(psp("working", Some("x")));
        r.report(psp("done", Some("y")));
        r.report(psp("clear", None));
        assert!(r.is_empty());
        // A new prompt drops working/blocked but keeps done/error.
        r.report(psp("working", Some("a")));
        r.report(psp("done", Some("b")));
        r.report(psp("error", Some("c")));
        r.drop_active();
        assert_eq!(r.len(), 2);
        // The LRU cap.
        for i in 0..300 {
            r.report(psp("idle", Some(&format!("n{i}"))));
        }
        assert_eq!(r.len(), MAX_RECORDS);
        assert!(
            r.recs.iter().all(|x| x.0.as_deref() != Some("b")),
            "least recently updated went first"
        );
        r.reset();
        assert!(r.is_empty() && !r.seen);
    }
}
