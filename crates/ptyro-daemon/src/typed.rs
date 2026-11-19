//! Every key exactly once (WEB2.3; SPEC §16.4 WEB1.8, "Every key exactly once").
//!
//! A web client numbers its input within a stream of its own (`ClientMsg::Keys`) and sends again
//! what wasn't settled when its connection dropped. Each session keeps, per device and stream,
//! the highest number it has settled (typed or refused): a number at or below it is never typed
//! again. A replay is typed only while the session still waits for what it waited for when the
//! keys were typed: the first key at most `REPLAY_WINDOW_MS` old, every key carrying the
//! session's state token as it is now, and the stream known (or provably new). Otherwise every
//! key of it is refused, and settled, so no later copy of them is typed either.
//!
//! The table is bounded ([`MAX_STREAMS`] per session, each forgotten after [`STREAM_TTL_MS`]
//! unused) and crosses a live upgrade in the handoff image.

use std::collections::HashMap;

use ptyro_core::status::{SignalSource, StatusRecord};
use ptyro_protocol::{MAX_SEQ_KEYS, REPLAY_WINDOW_MS, Refused, RefusedWhy, SeqKey};
use serde::{Deserialize, Serialize};

/// Streams remembered per session; the least recently used goes first.
pub const MAX_STREAMS: usize = 64;
/// A stream unused this long is forgotten (far longer than any replay may wait).
pub const STREAM_TTL_MS: u64 = 10 * 60 * 1000;
/// Clock slack when judging whether a forgotten stream could have typed a replay's keys.
const SLACK_MS: u64 = 1_000;

/// Who a stream belongs to: the paired device (`None`: a local client).
pub type Who = Option<u64>;

/// Refusals remembered per stream: an answer lost with its connection is given again when the
/// keys come again (else they would read as typed), and a replay that brings refused keys back
/// types nothing after them.
pub const MAX_REFUSALS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Rec {
    /// Every number up to this is settled.
    high: u64,
    used_ms: u64,
    /// The latest refusals, oldest first.
    #[serde(default)]
    refused: Vec<Refused>,
}

/// One session's streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Streams {
    recs: HashMap<(Who, String), Rec>,
    /// Nothing before this is known: when the table began (the session started, or came from an
    /// image without one), or the last use of a stream it forgot. A stream it doesn't know may
    /// replay only keys typed after it.
    floor_ms: u64,
}

/// What a `Keys` message comes to: what to type, the number to acknowledge, and the refusals to
/// say (its own, and earlier ones of keys it brought again).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub plan: Plan,
    pub upto: u64,
    pub refused: Vec<Refused>,
}

/// What to do with a `Keys` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Type `keys[from..]`, in order.
    Type { from: usize },
    /// Every key was settled already (a copy): type nothing.
    Nothing,
    /// Type none of `keys[from..]`: they are settled as refused.
    Refuse { from: usize, why: RefusedWhy, reason: String },
}

/// The table as the handoff image carries it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamsImage {
    pub floor_ms: u64,
    pub streams: Vec<StreamImage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamImage {
    pub who: Who,
    pub stream: String,
    pub high: u64,
    pub used_ms: u64,
    #[serde(default)]
    pub refused: Vec<Refused>,
}

impl StreamsImage {
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty() && self.floor_ms == 0
    }
}

impl Streams {
    /// A table that knows nothing before `now_ms`.
    pub fn new(now_ms: u64) -> Self {
        Self { recs: HashMap::new(), floor_ms: now_ms }
    }

    /// A new session's table: nothing could have been typed into it before it began, so a stream
    /// it hasn't seen is new (the floor is 1, not 0: 0 is an image without a table).
    pub fn fresh() -> Self {
        Self::new(1)
    }

    /// From an image; an image without a table (an older daemon's) knows nothing before `now_ms`.
    pub fn from_image(img: StreamsImage, now_ms: u64) -> Self {
        if img.floor_ms == 0 {
            return Self::new(now_ms);
        }
        let recs = img
            .streams
            .into_iter()
            .map(|s| {
                let rec = Rec { high: s.high, used_ms: s.used_ms, refused: s.refused };
                ((s.who, s.stream), rec)
            })
            .collect();
        Self { recs, floor_ms: img.floor_ms }
    }

    pub fn image(&self) -> StreamsImage {
        let mut streams: Vec<_> = self
            .recs
            .iter()
            .map(|((who, stream), r)| StreamImage {
                who: *who,
                stream: stream.clone(),
                high: r.high,
                used_ms: r.used_ms,
                refused: r.refused.clone(),
            })
            .collect();
        streams.sort_by(|a, b| (a.who, &a.stream).cmp(&(b.who, &b.stream)));
        StreamsImage { floor_ms: self.floor_ms, streams }
    }

    pub fn len(&self) -> usize {
        self.recs.len()
    }

    /// The highest number settled for a stream, if it is known.
    pub fn high(&self, who: Who, stream: &str) -> Option<u64> {
        self.recs.get(&(who, stream.to_owned())).map(|r| r.high)
    }

    /// Decide on a `Keys` message and settle what it settles: call it with the table locked and
    /// type `Plan::Type`'s keys before unlocking, so a copy arriving meanwhile is a copy.
    pub fn decide(
        &mut self,
        who: Who,
        stream: &str,
        keys: &[SeqKey],
        replay: bool,
        token: &str,
        now_ms: u64,
    ) -> Decision {
        self.expire(now_ms);
        let key = (who, stream.to_owned());
        let known = self.recs.contains_key(&key);
        let high = self.recs.get(&key).map_or(0, |r| r.high);
        let rising = keys.windows(2).all(|w| w[0].n < w[1].n);
        if !ptyro_protocol::valid_stream(stream)
            || keys.len() > MAX_SEQ_KEYS
            || !rising
            || keys.first().is_some_and(|k| k.n == 0)
        {
            // Nothing is recorded: a garbage number must not settle a stream for good.
            let reason = "the keys were malformed (a bad stream name, numbers not rising, or too many at once)";
            let refused = match (keys.first(), keys.last()) {
                (Some(f), Some(l)) => vec![Refused {
                    from: f.n,
                    to: l.n,
                    why: RefusedWhy::Invalid,
                    reason: reason.into(),
                }],
                _ => vec![],
            };
            let plan = Plan::Refuse { from: 0, why: RefusedWhy::Invalid, reason: reason.into() };
            return Decision { plan, upto: high, refused };
        }
        // Refusals this message's settled keys had: said again (their answer may have been lost).
        let mut refused: Vec<Refused> = self.recs.get(&key).map_or_else(Vec::new, |r| {
            r.refused
                .iter()
                .filter(|x| keys.iter().any(|k| k.n <= high && (x.from..=x.to).contains(&k.n)))
                .cloned()
                .collect()
        });
        let Some(from) = keys.iter().position(|k| k.n > high) else {
            if let Some(r) = self.recs.get_mut(&key) {
                r.used_ms = now_ms;
            }
            return Decision { plan: Plan::Nothing, upto: high, refused };
        };
        let last = keys.last().map_or(high, |k| k.n);
        let no = if !replay {
            None
        } else if let Some(before) = refused.last() {
            // Refused keys came back with new ones after them: those follow keys that weren't
            // typed, so they aren't either (half a command is worse than none).
            Some((before.why, format!("keys before them weren't typed: {}", before.reason)))
        } else {
            self.judge(known, &keys[from..], token, now_ms)
        };
        self.settle(key.clone(), last, now_ms);
        let plan = match no {
            Some((why, reason)) => {
                let r = Refused { from: keys[from].n, to: last, why, reason: reason.clone() };
                if let Some(rec) = self.recs.get_mut(&key) {
                    rec.refused.push(r.clone());
                    let extra = rec.refused.len().saturating_sub(MAX_REFUSALS);
                    rec.refused.drain(..extra);
                }
                refused.push(r);
                Plan::Refuse { from, why, reason }
            }
            None => Plan::Type { from },
        };
        Decision { plan, upto: last, refused }
    }

    /// Why a replay's unsettled keys may not be typed, if they may not.
    fn judge(
        &self,
        known: bool,
        keys: &[SeqKey],
        token: &str,
        now_ms: u64,
    ) -> Option<(RefusedWhy, String)> {
        let first = &keys[0];
        let typed_ms = now_ms.saturating_sub(first.age_ms);
        if !known && self.floor_ms.saturating_add(SLACK_MS) >= typed_ms {
            return Some((
                RefusedWhy::Unknown,
                "ptyro can't tell whether they were typed already".into(),
            ));
        }
        if first.age_ms > REPLAY_WINDOW_MS {
            return Some((
                RefusedWhy::Stale,
                format!(
                    "they waited {:.0} s to be sent, more than {} s",
                    first.age_ms as f64 / 1000.0,
                    REPLAY_WINDOW_MS / 1000
                ),
            ));
        }
        if keys.iter().any(|k| k.token.as_deref() != Some(token)) {
            return Some((
                RefusedWhy::Changed,
                "the session changed since they were typed (it asked something new, or started or finished something)".into(),
            ));
        }
        None
    }

    fn settle(&mut self, key: (Who, String), high: u64, now_ms: u64) {
        let r = self.recs.entry(key).or_insert(Rec { high: 0, used_ms: now_ms, refused: vec![] });
        r.high = r.high.max(high);
        r.used_ms = now_ms;
        while self.recs.len() > MAX_STREAMS {
            let Some((k, used)) = self
                .recs
                .iter()
                .min_by_key(|(_, r)| r.used_ms)
                .map(|(k, r)| (k.clone(), r.used_ms))
            else {
                break;
            };
            self.recs.remove(&k);
            self.floor_ms = self.floor_ms.max(used);
        }
    }

    fn expire(&mut self, now_ms: u64) {
        let mut floor = self.floor_ms;
        self.recs.retain(|_, r| {
            let keep = now_ms.saturating_sub(r.used_ms) < STREAM_TTL_MS;
            if !keep {
                floor = floor.max(r.used_ms);
            }
            keep
        });
        self.floor_ms = floor;
    }
}

/// Named sends (`SendText.once`, WEB2.3): what each name's send came to, so the same name again
/// (a retry after the connection dropped) is answered as the first was and types nothing. A
/// retry while the first is still going waits for it. A send that failed is forgotten: trying
/// again may type it. Bounded ([`MAX_SENDS`], [`SEND_TTL_MS`]); the ones that went cross a live
/// upgrade.
pub const MAX_SENDS: usize = 256;
pub const SEND_TTL_MS: u64 = 10 * 60 * 1000;

type SendCell = std::sync::Arc<tokio::sync::OnceCell<Result<(), ptyro_core::Error>>>;

/// (device, session, name).
pub type SendKey = (Who, String, String);

#[derive(Default)]
pub struct Sends {
    map: HashMap<SendKey, (u64, SendCell)>,
}

impl Sends {
    /// The cell for a name, made if it is new (the oldest forgotten past the bound).
    pub fn cell(&mut self, key: SendKey, now_ms: u64) -> SendCell {
        self.map.retain(|_, (at, _)| now_ms.saturating_sub(*at) < SEND_TTL_MS);
        if !self.map.contains_key(&key)
            && self.map.len() >= MAX_SENDS
            && let Some(old) =
                self.map.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone())
        {
            self.map.remove(&old);
        }
        self.map.entry(key).or_insert_with(|| (now_ms, SendCell::default())).1.clone()
    }

    /// Forget a name whose send failed (only if it is still that send's).
    pub fn forget(&mut self, key: &SendKey, cell: &SendCell) {
        if self.map.get(key).is_some_and(|(_, c)| std::sync::Arc::ptr_eq(c, cell)) {
            self.map.remove(key);
        }
    }

    /// The sends that went, for the handoff image: (device, session, name, when).
    pub fn image(&self) -> Vec<(Who, String, String, u64)> {
        let mut v: Vec<_> = self
            .map
            .iter()
            .filter(|(_, (_, c))| matches!(c.get(), Some(Ok(()))))
            .map(|((w, s, n), (at, _))| (*w, s.clone(), n.clone(), *at))
            .collect();
        v.sort();
        v
    }

    pub fn from_image(v: Vec<(Who, String, String, u64)>) -> Self {
        let map = v
            .into_iter()
            .map(|(w, s, n, at)| {
                let cell = SendCell::new(tokio::sync::OnceCell::new_with(Some(Ok(()))));
                ((w, s, n), (at, cell))
            })
            .collect();
        Self { map }
    }
}

/// The session's state token (`SessionInfo::state_token`): FNV-1a over what it waits for, the
/// same in every build (it crosses live upgrades), 16 hex digits. The status counts with when it
/// began (a new prompt, a turn that ended, is a new `since`), not its detail, and not when it is
/// only the output heuristic's guess (§9.3: output → working, quiet → idle), which the keys' own
/// echo moves and which says nothing about what the session waits for. Attention is left out
/// (it is cleared by being looked at, which changes nothing the keys meet).
pub fn token(
    pid: Option<u32>,
    live: bool,
    foreground: Option<u32>,
    alt_screen: bool,
    status: Option<&StatusRecord>,
) -> String {
    let status = status.filter(|r| r.source != SignalSource::Heuristic).map(|r| {
        format!("{}/{}@{}", r.status.label(), r.kind.as_deref().unwrap_or(""), r.since_ms)
    });
    let text = format!(
        "1|{}|{live}|{}|{alt_screen}|{}",
        pid.map_or(String::new(), |p| p.to_string()),
        foreground.map_or(String::new(), |p| p.to_string()),
        status.unwrap_or_default()
    );
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::input::InputEvent;
    use ptyro_core::status::{AgentStatus, Confidence, SignalSource};

    const T: &str = "tok";

    fn k(n: u64) -> SeqKey {
        SeqKey { n, event: InputEvent::Paste { text: n.to_string() }, token: None, age_ms: 0 }
    }

    fn r(n: u64, age_ms: u64, token: &str) -> SeqKey {
        SeqKey { token: Some(token.into()), age_ms, ..k(n) }
    }

    fn dec(
        t: &mut Streams,
        who: Who,
        stream: &str,
        keys: &[SeqKey],
        replay: bool,
        token: &str,
        now: u64,
    ) -> (Plan, u64) {
        let d = t.decide(who, stream, keys, replay, token, now);
        (d.plan, d.upto)
    }

    /// The numbers a plan types.
    fn typed(keys: &[SeqKey], plan: &Plan) -> Vec<u64> {
        match plan {
            Plan::Type { from } => keys[*from..].iter().map(|k| k.n).collect(),
            _ => vec![],
        }
    }

    #[test]
    fn each_number_is_typed_once_however_often_it_comes() {
        let mut t = Streams::new(0);
        let now = 100_000;
        let a = [k(1), k(2), k(3)];
        let (p, up) = dec(&mut t, None, "s", &a, false, T, now);
        assert_eq!((typed(&a, &p), up), (vec![1, 2, 3], 3));
        // The same again, live or replayed: nothing.
        assert_eq!(dec(&mut t, None, "s", &a, false, T, now), (Plan::Nothing, 3));
        let b = [r(2, 10, T), r(3, 10, T), r(4, 10, T), r(5, 10, T)];
        let (p, up) = dec(&mut t, None, "s", &b, true, T, now);
        assert_eq!((typed(&b, &p), up), (vec![4, 5], 5));
        assert_eq!(dec(&mut t, None, "s", &b, true, T, now).0, Plan::Nothing);
    }

    #[test]
    fn streams_are_per_device_and_name() {
        let mut t = Streams::new(0);
        let now = 100_000;
        dec(&mut t, Some(1), "s", &[k(1), k(2)], false, T, now);
        // Another device's stream of the same name is its own.
        let (p, _) = dec(&mut t, Some(2), "s", &[k(1)], false, T, now);
        assert_eq!(p, Plan::Type { from: 0 });
        let (p, _) = dec(&mut t, None, "s", &[k(1)], false, T, now);
        assert_eq!(p, Plan::Type { from: 0 });
        assert_eq!(t.high(Some(1), "s"), Some(2));
    }

    #[test]
    fn a_replay_is_refused_when_the_session_changed_and_settled_for_good() {
        let mut t = Streams::new(0);
        let now = 100_000;
        dec(&mut t, None, "s", &[k(1)], false, "old", now);
        let b = [r(2, 500, "old"), r(3, 400, "old")];
        let (p, up) = dec(&mut t, None, "s", &b, true, "new", now);
        assert!(matches!(p, Plan::Refuse { from: 0, why: RefusedWhy::Changed, .. }), "{p:?}");
        assert_eq!(up, 3);
        // A later copy of them, even one that would now match, types nothing.
        let c = [r(2, 600, "new"), r(3, 600, "new")];
        assert_eq!(dec(&mut t, None, "s", &c, true, "new", now).0, Plan::Nothing);
        // What is typed next goes.
        assert_eq!(dec(&mut t, None, "s", &[k(4)], false, "new", now).0, Plan::Type { from: 0 });
    }

    #[test]
    fn only_the_keys_not_yet_typed_are_judged() {
        let mut t = Streams::new(0);
        let now = 100_000;
        // 1–2 typed; then the connection dropped before their ack. 1–2 carry the old token, 3 the
        // one seen after: the session changed because of 1–2, and 3 was typed knowing it.
        dec(&mut t, None, "s", &[k(1), k(2)], false, "a", now);
        let b = [r(1, 900, "a"), r(2, 800, "a"), r(3, 100, "b")];
        let (p, up) = dec(&mut t, None, "s", &b, true, "b", now);
        assert_eq!((typed(&b, &p), up), (vec![3], 3));
        // But one stale token among the rest refuses them all: a batch is typed whole or not.
        let c = [r(4, 300, "b"), r(5, 200, "a"), r(6, 100, "b")];
        let (p, up) = dec(&mut t, None, "s", &c, true, "b", now);
        assert!(matches!(p, Plan::Refuse { from: 0, why: RefusedWhy::Changed, .. }), "{p:?}");
        assert_eq!(up, 6);
    }

    /// The connection drops with a refusal on its way: the keys come again (with more after
    /// them), and the refusal is said again; nothing after them is typed, whatever its token.
    #[test]
    fn a_refusal_whose_answer_was_lost_is_said_again_and_nothing_after_it_is_typed() {
        let mut t = Streams::new(1);
        let now = 100_000;
        t.decide(None, "s", &[k(1)], false, "a", now);
        let first = t.decide(None, "s", &[r(2, 100, "a"), r(3, 100, "a")], true, "b", now);
        assert_eq!(first.refused.len(), 1);
        assert_eq!((first.refused[0].from, first.refused[0].to), (2, 3));
        // (That answer was lost.) Again, with 4 and 5 typed since, carrying today's token.
        let keys = [r(2, 900, "a"), r(3, 900, "a"), r(4, 100, "b"), r(5, 50, "b")];
        let again = t.decide(None, "s", &keys, true, "b", now);
        assert!(
            matches!(again.plan, Plan::Refuse { from: 2, why: RefusedWhy::Changed, .. }),
            "{again:?}"
        );
        assert_eq!(again.upto, 5);
        let ranges: Vec<_> = again.refused.iter().map(|x| (x.from, x.to)).collect();
        assert_eq!(ranges, vec![(2, 3), (4, 5)]);
        assert!(again.refused[1].reason.starts_with("keys before them weren't typed"));
        // A plain copy of them is told the same, and types nothing.
        let copy = t.decide(None, "s", &keys, true, "b", now);
        assert_eq!(copy.plan, Plan::Nothing);
        assert_eq!(copy.refused.len(), 2);
        // Only so many are remembered.
        for i in 0..20u64 {
            let n = 6 + i;
            t.decide(None, "s", &[r(n, 10, "x")], true, "b", now);
        }
        assert_eq!(t.recs[&(None, "s".to_owned())].refused.len(), MAX_REFUSALS);
        // And they cross an upgrade.
        let back = Streams::from_image(t.image(), now);
        assert_eq!(back, t);
    }

    #[test]
    fn a_replay_too_old_is_refused() {
        let mut t = Streams::new(0);
        let now = 100_000;
        dec(&mut t, None, "s", &[k(1)], false, T, now);
        let b = [r(2, REPLAY_WINDOW_MS + 1, T), r(3, 10, T)];
        let (p, _) = dec(&mut t, None, "s", &b, true, T, now);
        assert!(matches!(p, Plan::Refuse { why: RefusedWhy::Stale, .. }), "{p:?}");
        // Exactly at the window: typed.
        let b = [r(4, REPLAY_WINDOW_MS, T)];
        assert_eq!(dec(&mut t, None, "s", &b, true, T, now).0, Plan::Type { from: 0 });
        // A replay without tokens can't be checked: refused.
        let b = [k(5)];
        let (p, _) = dec(&mut t, None, "s", &b, true, T, now);
        assert!(matches!(p, Plan::Refuse { why: RefusedWhy::Changed, .. }), "{p:?}");
    }

    #[test]
    fn a_stream_it_never_saw_replays_only_keys_typed_after_it_knew_everything() {
        let now = 100_000;
        // A table from long ago: a new stream's first keys, typed offline, go.
        let mut t = Streams::new(now - 60_000);
        let b = [r(1, 2000, T), r(2, 1000, T)];
        assert_eq!(dec(&mut t, None, "s", &b, true, T, now).0, Plan::Type { from: 0 });
        // A new session's table vouches for any stream: nothing was typed before it began.
        let mut fresh = Streams::fresh();
        assert_eq!(dec(&mut fresh, None, "s", &b, true, T, now).0, Plan::Type { from: 0 });
        // A table that began after they were typed (an image without one) can't vouch for them.
        let mut t = Streams::new(now - 1500);
        let (p, _) = dec(&mut t, None, "s", &b, true, T, now);
        assert!(matches!(p, Plan::Refuse { why: RefusedWhy::Unknown, .. }), "{p:?}");
    }

    #[test]
    fn memory_is_bounded_and_a_forgotten_stream_cannot_replay_what_it_might_have_typed() {
        let start = 1_000_000;
        let mut t = Streams::new(0);
        for i in 0..(MAX_STREAMS as u64 + 10) {
            dec(&mut t, Some(i), "s", &[k(1)], false, T, start + i);
        }
        assert_eq!(t.len(), MAX_STREAMS);
        // The oldest went; the floor moved to the newest of those forgotten.
        assert_eq!(t.high(Some(0), "s"), None);
        assert_eq!(t.high(Some(MAX_STREAMS as u64 + 9), "s"), Some(1));
        assert_eq!(t.floor_ms, start + 9);
        // Device 0 replays keys typed before its stream was forgotten: refused.
        let now = start + 100;
        let b = [r(2, 95, T)];
        let (p, _) = dec(&mut t, Some(0), "s", &b, true, T, now);
        assert!(matches!(p, Plan::Refuse { why: RefusedWhy::Unknown, .. }), "{p:?}");
        // Unused for the TTL: every stream goes.
        let later = start + STREAM_TTL_MS + 1000;
        dec(&mut t, Some(999), "x", &[k(1)], false, T, later);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn malformed_keys_are_refused_and_settle_nothing() {
        let mut t = Streams::new(0);
        let now = 100_000;
        for (stream, keys) in [
            ("s", vec![k(2), k(1)]),
            ("s", vec![k(1), k(1)]),
            ("s", vec![k(0)]),
            ("bad stream", vec![k(1)]),
            ("s", (1..=MAX_SEQ_KEYS as u64 + 1).map(k).collect()),
        ] {
            let (p, up) = dec(&mut t, None, stream, &keys, false, T, now);
            assert!(matches!(p, Plan::Refuse { why: RefusedWhy::Invalid, from: 0, .. }), "{p:?}");
            assert_eq!(up, 0);
        }
        assert_eq!(t.len(), 0);
        // An empty message settles nothing either.
        assert_eq!(dec(&mut t, None, "s", &[], false, T, now), (Plan::Nothing, 0));
    }

    #[test]
    fn a_gap_in_live_keys_still_types_what_came() {
        // Live keys (not a replay) are typed as they come: the client replays what went missing
        // before anything newer, so a gap is a loss already reported, not a reason to stall.
        let mut t = Streams::new(0);
        dec(&mut t, None, "s", &[k(1)], false, T, 10);
        assert_eq!(dec(&mut t, None, "s", &[k(5)], false, T, 10), (Plan::Type { from: 0 }, 5));
    }

    #[test]
    fn the_table_crosses_an_upgrade() {
        let mut t = Streams::new(5);
        dec(&mut t, Some(3), "s", &[k(1), k(2)], false, T, 50);
        let img = t.image();
        let back: StreamsImage =
            rmp_serde::from_slice(&rmp_serde::to_vec_named(&img).unwrap()).unwrap();
        let t2 = Streams::from_image(back, 999);
        assert_eq!(t2, t);
        // An older daemon's image has none: nothing before the upgrade is vouched for.
        let t3 = Streams::from_image(StreamsImage::default(), 999);
        assert_eq!((t3.len(), t3.floor_ms), (0, 999));
    }

    #[tokio::test]
    async fn a_named_send_happens_once_and_a_failed_one_may_be_tried_again() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let mut sends = Sends::default();
        let typed = AtomicU32::new(0);
        let key = |n: &str| (Some(1), "s".to_owned(), n.to_owned());
        let send = || async {
            typed.fetch_add(1, Ordering::SeqCst);
            Ok::<(), ptyro_core::Error>(())
        };
        for _ in 0..3 {
            let c = sends.cell(key("a"), 10);
            assert!(c.get_or_init(send).await.is_ok());
        }
        assert_eq!(typed.load(Ordering::SeqCst), 1);
        // Another name, device or session is another send.
        sends.cell(key("b"), 10).get_or_init(send).await.clone().unwrap();
        sends.cell((Some(2), "s".into(), "a".into()), 10).get_or_init(send).await.clone().unwrap();
        sends.cell((Some(1), "t".into(), "a".into()), 10).get_or_init(send).await.clone().unwrap();
        assert_eq!(typed.load(Ordering::SeqCst), 4);
        // A failure is forgotten: the retry sends.
        let c = sends.cell(key("f"), 10);
        let r = c.get_or_init(|| async { Err(ptyro_core::Error::conflict("ended")) }).await.clone();
        assert!(r.is_err());
        sends.forget(&key("f"), &c);
        sends.cell(key("f"), 10).get_or_init(send).await.clone().unwrap();
        assert_eq!(typed.load(Ordering::SeqCst), 5);
        // What went crosses an upgrade; the failure doesn't (it was forgotten).
        let img = sends.image();
        assert_eq!(img.len(), 5);
        let mut back = Sends::from_image(img);
        back.cell(key("a"), 20).get_or_init(send).await.clone().unwrap();
        assert_eq!(typed.load(Ordering::SeqCst), 5);
        // Bounded, and forgotten after the TTL.
        for i in 0..(MAX_SENDS + 5) {
            back.cell((None, "s".into(), i.to_string()), 30 + i as u64);
        }
        assert_eq!(back.map.len(), MAX_SENDS);
        back.cell(key("z"), 30 + SEND_TTL_MS + 10_000);
        assert_eq!(back.map.len(), 1);
    }

    #[test]
    fn the_token_follows_what_the_session_waits_for() {
        let rec = |status, since_ms| StatusRecord {
            status,
            kind: None,
            detail: Some("Bash: ls".into()),
            confidence: Confidence::High,
            source: SignalSource::Hook,
            since_ms,
            progress: None,
        };
        let idle = rec(AgentStatus::Idle, 10);
        let base = token(Some(7), true, Some(7), false, Some(&idle));
        assert_eq!(base.len(), 16);
        assert_eq!(base, token(Some(7), true, Some(7), false, Some(&idle)), "stable");
        // The detail alone doesn't count; a new status, a new `since` (a new prompt), a
        // restart, an end, another foreground program or the alternate screen do.
        let mut other = idle.clone();
        other.detail = Some("Read: x".into());
        assert_eq!(base, token(Some(7), true, Some(7), false, Some(&other)));
        for t in [
            token(Some(7), true, Some(7), false, Some(&rec(AgentStatus::Blocked, 10))),
            token(Some(7), true, Some(7), false, Some(&rec(AgentStatus::Idle, 11))),
            token(Some(8), true, Some(8), false, Some(&idle)),
            token(Some(7), false, Some(7), false, Some(&idle)),
            token(Some(7), true, Some(9), false, Some(&idle)),
            token(Some(7), true, Some(7), true, Some(&idle)),
            token(Some(7), true, Some(7), false, None),
        ] {
            assert_ne!(t, base);
        }
        // The output heuristic's guesses don't count: they follow the keys' own echo.
        let guess = |status, since_ms| StatusRecord {
            source: SignalSource::Heuristic,
            confidence: Confidence::Low,
            ..rec(status, since_ms)
        };
        assert_eq!(
            token(Some(7), true, Some(7), false, Some(&guess(AgentStatus::Working, 50))),
            token(Some(7), true, Some(7), false, Some(&guess(AgentStatus::Idle, 90)))
        );
        assert_eq!(
            token(Some(7), true, Some(7), false, Some(&guess(AgentStatus::Idle, 90))),
            token(Some(7), true, Some(7), false, None)
        );
        let mut blocked = rec(AgentStatus::Blocked, 10);
        blocked.kind = Some("permission".into());
        assert_ne!(
            token(Some(7), true, Some(7), false, Some(&blocked)),
            token(Some(7), true, Some(7), false, Some(&rec(AgentStatus::Blocked, 10)))
        );
    }
}
