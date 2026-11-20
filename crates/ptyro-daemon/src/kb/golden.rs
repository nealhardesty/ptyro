//! The golden set (SPEC §25.16; `make kb-eval`, `make kb-eval-real`): a made-up corpus of 300
//! items of every kind in two profiles, with chats beside it, and 63 questions, each with what
//! must be among the first five hits and what must never be in the answer at all (another
//! profile's, a forgotten one, what a filter rules out).
//!
//! Everything goes the real way: each item through the gate, redaction and the cutters into
//! `kb.db` ([`Daemon::kb_ingest`]), its vectors from the ingest's embed step, each question
//! through [`Daemon::kb_search`]. It reports recall at 5 and the mean reciprocal rank, by
//! words alone and with meaning, and fails when one is more than two points under
//! [`COMMITTED`] or a never row shows. **Every change to the ranking (§25.8) is made against
//! it**, and the numbers below are changed in the same commit.
//!
//! The model is a synthetic one (nothing is downloaded): a topic's telling words and the
//! words that mean the same without being in the corpus ([`Topic::like`]) lie along one axis.
//! `make kb-eval-real` runs the same questions with `potion-base-8M` when it is on this
//! machine, and reports only: its numbers are a model's, not this code's.

use std::sync::Arc;

use ptyro_core::kb::chunk::Part;
use ptyro_core::kb::ingest::{Body, Source, Stamp};
use ptyro_core::kb::search::{KbHits, KbSearch};
use ptyro_core::kb::{ChunkKind, ItemKind, Writer};
use ptyro_core::now_ms;
use ptyro_store::Registry;

use super::search::KbScope;
use super::search::tests::{chat, embed_chats, prepared, put_model, set_config};
use super::{Facts, Prepared};
use crate::daemon::Daemon;

/// Recall at 5 and the mean reciprocal rank, as measured on 2026-11-19: by words alone, and
/// with meaning (the synthetic model). A change that lowers one by more than [`SLACK`] fails;
/// one that raises it should raise these.
const COMMITTED: [(&str, f64, f64); 2] = [("words", 0.746, 0.576), ("meaning", 1.0, 0.761)];
const SLACK: f64 = 0.02;
/// Of the prompts that must be offered something (KB5, [`prompts`]), the share that was, as
/// measured on 2026-11-20. A change to the floor of `kb/offer.rs` is made against it.
const OFFERED: [(&str, f64); 2] = [("words", 1.0), ("meaning", 1.0)];

const DAY: u64 = 86_400_000;

struct Topic {
    slug: &'static str,
    title: &'static str,
    /// Its two telling words: in every piece about it.
    terms: [&'static str; 2],
    /// Words that mean it and are nowhere in the corpus: what a question by meaning asks in.
    like: [&'static str; 3],
    /// What was asked, what was done, and what was decided.
    ask: &'static str,
    fix: &'static str,
    rule: &'static str,
}

const fn t(
    slug: &'static str,
    title: &'static str,
    terms: [&'static str; 2],
    like: [&'static str; 3],
    said: [&'static str; 3],
) -> Topic {
    Topic { slug, title, terms, like, ask: said[0], fix: said[1], rule: said[2] }
}

#[rustfmt::skip]
const TOPICS: [Topic; 30] = [
    t("retry-backoff", "Retries with backoff", ["retry", "backoff"], ["reattempt", "delay", "jitter"], [
        "Why does the retry loop hammer the server with no backoff between calls?",
        "Added exponential backoff to the retry loop, capped at thirty seconds.",
        "Every retry uses exponential backoff with a cap; never loop without waiting."]),
    t("cache-eviction", "Cache eviction", ["cache", "eviction"], ["memoize", "purge", "stale"], [
        "The cache grows without bound because eviction never runs.",
        "Switched the cache to least recently used eviction with a size limit.",
        "The cache has a fixed size and eviction is least recently used."]),
    t("login-token", "Login token lifetime", ["login", "token"], ["signin", "authenticate", "expiry"], [
        "After login the token runs out in a minute and users are thrown out.",
        "The login token now lasts a day and is renewed on use.",
        "A login token lives one day and is renewed whenever it is used."]),
    t("schema-migration", "Schema migrations", ["schema", "migration"], ["database", "ddl", "alter"], [
        "The schema migration fails halfway and leaves the tables broken.",
        "Each schema migration now runs inside one transaction and rolls back whole.",
        "A schema migration is one transaction and is never edited after release."]),
    t("flaky-test", "The flaky upload test", ["flaky", "timeout"], ["intermittent", "unreliable", "sporadic"], [
        "The flaky upload test hits its timeout once in ten runs.",
        "Raised the timeout and removed the sleep that made the test flaky.",
        "A flaky test is fixed or deleted the same week; a longer timeout is not a fix."]),
    t("memory-leak", "The worker's leak", ["leak", "heap"], ["memory", "growth", "allocation"], [
        "The worker has a leak: its heap doubles every hour.",
        "Found the leak in the listener list; the heap is flat now.",
        "Check the heap after a soak run; any leak blocks the release."]),
    t("log-rotation", "Log rotation", ["logs", "rotation"], ["logfile", "archive", "truncate"], [
        "The logs fill the volume because rotation is off.",
        "Turned on daily rotation and compress logs older than a week.",
        "Rotation is daily and logs are kept for thirty days."]),
    t("dark-theme", "Dark theme contrast", ["theme", "contrast"], ["colors", "palette", "appearance"], [
        "The dark theme has poor contrast on the status bar.",
        "Raised the contrast of the dark theme to pass the readability check.",
        "Every theme meets the contrast ratio of four and a half to one."]),
    t("keyboard-shortcuts", "Keyboard shortcuts", ["shortcut", "keybinding"], ["hotkey", "keystroke", "chord"], [
        "The shortcut for search clashes with the terminal keybinding.",
        "Moved the search shortcut and made every keybinding configurable.",
        "A new shortcut never shadows a terminal keybinding."]),
    t("rate-limit", "The shared throttle", ["throttle", "quota"], ["ratelimit", "burst", "limiter"], [
        "Clients exceed their quota because the throttle counts per process.",
        "The throttle is now shared and enforces the quota across processes.",
        "The quota is a hundred calls a minute and the throttle is shared."]),
    t("csv-export", "CSV export", ["export", "csv"], ["download", "spreadsheet", "comma"], [
        "The csv export breaks on names with quotes.",
        "Quoted every field in the csv export and added a test with quotes.",
        "The export writes csv with every field quoted."]),
    t("image-thumbnails", "Thumbnails", ["thumbnail", "resize"], ["picture", "preview", "scale"], [
        "Each thumbnail is blurry after the resize step.",
        "The resize now uses a sharper filter and the thumbnail is crisp.",
        "A thumbnail is made by one resize to 256 pixels, never made larger."]),
    t("websocket-reconnect", "Websocket reconnects", ["websocket", "reconnect"], ["socket", "disconnect", "connection"], [
        "The websocket drops on sleep and does not reconnect.",
        "The client now does reconnect the websocket with a growing wait.",
        "A websocket must reconnect by itself and replay what was missed."]),
    t("time-zones", "Time zones", ["timezone", "utc"], ["clock", "daylight", "zone"], [
        "Reports are a day off because the timezone is applied twice on top of utc.",
        "Store utc everywhere and apply the timezone only when showing.",
        "Times are stored in utc; a timezone is for display only."]),
    t("pagination", "Cursor pagination", ["pagination", "cursor"], ["paging", "page", "scroll"], [
        "Pagination by offset skips rows when new ones arrive; would a cursor help?",
        "Switched pagination to an opaque cursor so nothing is skipped.",
        "Lists use cursor pagination; offsets are not allowed."]),
    t("feature-flags", "Feature flags", ["flag", "rollout"], ["toggle", "gradual", "experiment"], [
        "The new editor flag went to everyone instead of a slow rollout.",
        "The flag now supports a percentage rollout and a kill switch.",
        "Every risky change ships behind a flag with a staged rollout."]),
    t("backup-restore", "Backups that restore", ["backup", "restore"], ["snapshot", "recovery", "disaster"], [
        "Nobody has tried to restore from the nightly backup.",
        "Ran a full restore from backup into staging; it took nine minutes.",
        "A backup counts only after a restore from it was tested."]),
    t("dependency-updates", "Dependency updates", ["dependency", "lockfile"], ["library", "package", "version"], [
        "A dependency moved under us because the lockfile was not committed.",
        "Committed the lockfile and pinned the dependency that broke.",
        "The lockfile is committed and a dependency is updated on purpose."]),
    t("search-index", "The search index", ["index", "tokenizer"], ["lookup", "inverted", "fulltext"], [
        "The index misses word endings; which tokenizer is it using?",
        "Rebuilt the index with a stemming tokenizer so endings match.",
        "The index uses the porter tokenizer; rebuild it after changing that."]),
    t("container-size", "Container size", ["container", "layer"], ["docker", "dockerfile", "registry"], [
        "The container is two gigabytes; one layer holds the whole toolchain.",
        "Split the build so the final container has no toolchain layer.",
        "A release container has no compiler layer and stays under 200 MB."]),
    t("ci-pipeline", "The pipeline's wait", ["pipeline", "runner"], ["continuous", "integration", "workflow"], [
        "The pipeline waits twenty minutes for a free runner.",
        "Added a second runner and cached the first step of the pipeline.",
        "The pipeline finishes in ten minutes; add a runner before it does not."]),
    t("refusal-wording", "What a refusal says", ["refusal", "wording"], ["complaint", "unhelpful", "guidance"], [
        "Each refusal says what went wrong but the wording gives no next step.",
        "Rewrote the wording so every refusal names the fix.",
        "A refusal says what to do next, in plain wording."]),
    t("onboarding-docs", "Onboarding", ["onboarding", "readme"], ["newcomer", "introduction", "tutorial"], [
        "The onboarding takes a day because the readme is out of date.",
        "Rewrote the readme; onboarding is now three commands.",
        "The readme is tried by the newest hire; onboarding takes an hour."]),
    t("password-hashing", "Password hashing", ["password", "hash"], ["bcrypt", "argon", "scrypt"], [
        "Is the password hash still the fast unsalted one?",
        "Moved every password to a slow salted hash at the next sign in.",
        "A password is stored as a slow salted hash, never anything else."]),
    t("screen-readers", "Screen readers", ["screenreader", "aria"], ["accessibility", "blind", "a11y"], [
        "The screenreader reads nothing for the menu; aria labels are missing.",
        "Added aria labels and checked the menu with a screenreader.",
        "Every control has an aria label and is tried with a screenreader."]),
    t("invoice-rounding", "Invoice rounding", ["invoice", "rounding"], ["billing", "penny", "decimal"], [
        "The invoice total is a cent off; rounding happens per line.",
        "Rounding now happens once, on the invoice total.",
        "Money is whole cents; rounding is done once per invoice."]),
    t("email-bounce", "Bounced mail", ["bounce", "mailer"], ["email", "undelivered", "smtp"], [
        "Half the invites bounce and the mailer keeps sending.",
        "The mailer now stops after one bounce and marks the address.",
        "After a bounce the mailer never writes to that address again."]),
    t("dns-cutover", "The DNS cutover", ["dns", "ttl"], ["domain", "nameserver", "propagation"], [
        "The cutover took a day because the dns ttl was a day.",
        "Lowered the ttl a week ahead; the dns change took five minutes.",
        "Lower the ttl before any dns change, and raise it after."]),
    t("gpu-driver", "The GPU driver", ["gpu", "driver"], ["graphics", "cuda", "nvidia"], [
        "Training crashes after the gpu driver update.",
        "Pinned the gpu driver to the last release that passes the suite.",
        "The gpu driver is pinned; update it only with the suite green."]),
    t("spellcheck", "Spellcheck and names", ["spellcheck", "dictionary"], ["typo", "misspelling", "grammar"], [
        "The spellcheck marks every product name; can the dictionary learn them?",
        "Added a project dictionary so spellcheck knows our names.",
        "Names go in the project dictionary; spellcheck must pass in review."]),
];

/// Words of no topic, for the documents that are about nothing asked.
const FILLER: [&str; 12] = [
    "lunch", "parking", "holiday", "office", "plant", "window", "coffee", "meeting", "agenda",
    "budget", "travel", "badge",
];
/// Documents about nothing: each also says one topic's first word, once.
const DISTRACTORS: usize = 90;
/// The topics whose note is forgotten after it was kept.
const FORGOTTEN: std::ops::Range<usize> = 27..30;
/// The topics that also have a chat.
const CHATS: usize = 7;

impl Topic {
    fn profile(i: usize) -> &'static str {
        if i.is_multiple_of(2) { "work" } else { "home" }
    }

    /// A topic's document and source file are everyone's for every third topic, else its
    /// profile's.
    fn shared(i: usize) -> Option<&'static str> {
        (!i.is_multiple_of(3)).then(|| Self::profile(i))
    }

    fn session(&self) -> String {
        format!("session://claude/{}", self.slug)
    }
    fn old(&self) -> String {
        format!("session://codex/{}-old", self.slug)
    }
    fn doc(&self) -> String {
        format!("file:///golden/docs/{}.md", self.slug)
    }
    fn src(&self) -> String {
        format!("file:///golden/src/{}.rs", self.slug)
    }
    fn commit(&self) -> String {
        format!("commit://golden/{}", self.slug)
    }
    fn note(&self, i: usize) -> String {
        format!("note://{}/_global/{}", Self::profile(i), self.slug)
    }
    fn shell(i: usize) -> String {
        format!("shell://golden-sh{i}/2026-11-01")
    }
    fn chat(i: usize) -> String {
        format!("chat:golden-chat-{i}")
    }
    fn words(&self) -> String {
        self.terms.join(" ")
    }
}

/// The synthetic model: a topic's words along its own axis, filler words along one more, each
/// with a little of the others so that no two are the same.
fn model(d: &Daemon) {
    let dim = TOPICS.len() + 1;
    let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
    let mut jitter = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        ((seed >> 40) as f32 / (1u64 << 24) as f32 - 0.5) * 0.2
    };
    let mut rows: Vec<(&str, Vec<f32>)> = Vec::new();
    let mut row = |word: &'static str, axis: usize| {
        let mut v: Vec<f32> = (0..dim).map(|_| jitter()).collect();
        v[axis] += 1.0;
        rows.push((word, v));
    };
    for (i, topic) in TOPICS.iter().enumerate() {
        for w in topic.terms.iter().chain(&topic.like) {
            row(w, i);
        }
    }
    for w in FILLER {
        row(w, TOPICS.len());
    }
    let tokens: Vec<(&str, &[f32])> = rows.iter().map(|(w, v)| (*w, v.as_slice())).collect();
    put_model(d, &tokens);
}

fn source(kind: ItemKind, uri: String, title: &str, body: Body) -> Source {
    let mut s = Source::new(kind, uri, body);
    s.title = Some(title.to_owned());
    s
}

/// The corpus: seven items a topic (a session, an older one, a document, a source file, a
/// commit, a note, a shell's day) and the documents about nothing.
fn corpus(now: u64) -> Vec<Prepared> {
    let mut out = Vec::new();
    let stamp = |writer, profile: Option<&str>| Stamp::new(writer, profile.map(str::to_owned));
    let mut put = |source: Source, stamp: Stamp| {
        out.push(Prepared { source, stamp, facts: Facts::default() });
    };
    for (i, topic) in TOPICS.iter().enumerate() {
        let Topic { slug, title, terms: [a, b], ask, fix, rule, .. } = *topic;
        let at = now - (i as u64 + 1) * DAY;
        let profile = Topic::profile(i);
        let project = ["p-web", "p-api", "p-infra"][i % 3];
        let agent = |name: &str, session: String| Stamp {
            project: Some(project.into()),
            agent: Some(name.into()),
            session: Some(session),
            ..stamp(Writer::Agent, Some(profile))
        };
        let session = prepared(
            ItemKind::Session,
            &topic.session(),
            title,
            vec![
                Part::new(ChunkKind::Card, format!("task: {title}. outcome: {fix}"), at)
                    .anchor("card"),
                Part::new(ChunkKind::User, ask, at).anchor("u1"),
                Part::new(ChunkKind::Assistant, fix, at).anchor("a1"),
                Part::new(ChunkKind::Tool, format!("Edit src/{slug}.rs"), at).anchor("t1"),
            ],
            agent("claude", format!("golden-s{i}")),
        );
        put(session.source, session.stamp);
        let old = now - 400 * DAY;
        let first = format!("First try at {title}: it did not hold. ({a} {b})");
        let earlier = prepared(
            ItemKind::Session,
            &topic.old(),
            title,
            vec![
                Part::new(ChunkKind::User, ask, old).anchor("u1"),
                Part::new(ChunkKind::Assistant, first, old).anchor("a1"),
            ],
            agent("codex", format!("golden-o{i}")),
        );
        put(earlier.source, earlier.stamp);
        let text = format!("# {title}\n\n{rule}\n\n## Background\n\n{ask} {fix}\n");
        let body = Body::Markdown { name: format!("{slug}.md"), text, at_ms: at };
        put(
            source(ItemKind::File, topic.doc(), title, body),
            stamp(Writer::File, Topic::shared(i)),
        );
        let snake = slug.replace('-', "_");
        let parts = vec![
            Part::new(
                ChunkKind::Path,
                format!("src/{slug}.rs src {} rs", slug.replace('-', " ")),
                at,
            ),
            Part::new(ChunkKind::Comment, rule, at).heading("L10"),
            Part::new(ChunkKind::Code, format!("fn {snake}() {{ /* {a} {b} */ }}"), at),
        ];
        let file = source(ItemKind::File, topic.src(), &format!("{slug}.rs"), Body::Parts(parts));
        put(file, stamp(Writer::File, Topic::shared(i)));
        let parts = vec![
            Part::new(ChunkKind::Message, format!("fix: {title} ({a}, {b})"), at),
            Part::new(ChunkKind::Path, format!("src/{slug}.rs"), at),
        ];
        let mut commit = source(ItemKind::Commit, topic.commit(), title, Body::Parts(parts));
        commit.files = vec![format!("src/{slug}.rs")];
        put(commit, stamp(Writer::File, None));
        let writer = if i % 4 < 2 { Writer::User } else { Writer::Agent };
        let body = Body::Parts(vec![Part::new(ChunkKind::Text, rule, at)]);
        put(source(ItemKind::Note, topic.note(i), title, body), stamp(writer, Some(profile)));
        let parts = vec![
            Part::new(ChunkKind::Command, format!("make {slug}-check # {a} {b}"), at),
            Part::new(ChunkKind::Output, format!("{a} {b}: ok"), at),
        ];
        let shell = source(ItemKind::Shell, Topic::shell(i), "shell", Body::Parts(parts));
        put(shell, stamp(Writer::User, Some(profile)));
    }
    for n in 0..DISTRACTORS {
        let mut words: Vec<&str> =
            (0..12).map(|k| FILLER[(n * 5 + k * 7) % FILLER.len()]).collect();
        words.insert(n % 12, TOPICS[n % TOPICS.len()].terms[0]);
        let body = Body::Plain {
            name: format!("misc-{n}.md"),
            text: format!("Notes of week {n}: {}.", words.join(", ")),
            at_ms: now - (n as u64 + 2) * DAY,
        };
        let profile = [None, Some("work"), Some("home")][n % 3];
        let uri = format!("file:///golden/misc/{n}.md");
        put(source(ItemKind::File, uri, "Weekly notes", body), stamp(Writer::File, profile));
    }
    out
}

/// One question: what is asked, by whom, what must be among the first five, and what must
/// not be in the answer.
struct Question {
    query: String,
    /// The wall: `None` is you.
    profile: Option<&'static str>,
    must: Vec<String>,
    never: Vec<String>,
}

fn questions() -> Vec<Question> {
    let mut qs = Vec::new();
    let mut ask = |query: String, profile, must: Vec<String>, never: Vec<String>| {
        qs.push(Question { query, profile, must, never });
    };
    for (i, topic) in TOPICS.iter().enumerate() {
        // By its words: the decision is what you want first. A forgotten one never shows,
        // and the document that says the same is found instead.
        match FORGOTTEN.contains(&i) {
            false => ask(topic.words(), None, vec![topic.note(i)], vec![]),
            true => ask(topic.words(), None, vec![topic.doc()], vec![topic.note(i)]),
        }
    }
    for (i, topic) in TOPICS.iter().enumerate().take(16) {
        // In other words, none of them in the corpus: found by meaning, or not at all.
        ask(topic.like.join(" "), None, vec![topic.note(i)], vec![]);
    }
    let private = |i: usize| {
        let topic = &TOPICS[i];
        let mut never = vec![topic.session(), topic.old(), topic.note(i), Topic::shell(i)];
        if Topic::shared(i).is_some() {
            never.extend([topic.doc(), topic.src()]);
        }
        if i < CHATS {
            never.push(Topic::chat(i));
        }
        never
    };
    // The wall: a work profile's model asks about what the home profile did, and the other
    // way round. What is nobody's (the commit) is found; nothing of the other profile is.
    for i in [1, 3, 5, 7, 9, 11, 13] {
        ask(TOPICS[i].words(), Some("work"), vec![TOPICS[i].commit()], private(i));
    }
    for i in [2, 4] {
        ask(TOPICS[i].words(), Some("home"), vec![TOPICS[i].commit()], private(i));
    }
    // The query's own filters.
    let f = |i: usize, with: &str| format!("{} {with}", TOPICS[i].words());
    ask(
        f(2, "kind:commit"),
        None,
        vec![TOPICS[2].commit()],
        vec![TOPICS[2].note(2), TOPICS[2].doc()],
    );
    ask(f(4, "kind:session"), None, vec![TOPICS[4].session()], vec![TOPICS[4].note(4)]);
    ask(
        f(6, "kind:comment"),
        None,
        vec![TOPICS[6].src()],
        vec![TOPICS[6].doc(), TOPICS[6].session()],
    );
    ask(
        f(0, "kind:chat"),
        None,
        vec![Topic::chat(0)],
        vec![TOPICS[0].note(0), TOPICS[0].session()],
    );
    ask(f(8, "kind:session since:30d"), None, vec![TOPICS[8].session()], vec![TOPICS[8].old()]);
    ask(
        f(10, "wrote:agent"),
        None,
        vec![TOPICS[10].note(10)],
        vec![TOPICS[10].doc(), Topic::shell(10)],
    );
    ask(f(12, "file:src/websocket"), None, vec![TOPICS[12].src()], vec![TOPICS[12].note(12)]);
    ask(
        f(14, "before:100d"),
        None,
        vec![TOPICS[14].old()],
        vec![TOPICS[14].session(), TOPICS[14].note(14)],
    );
    qs
}

/// A prompt an agent is given (KB5, §25.9): what it may be offered, and what it must not.
struct Prompt {
    text: String,
    /// The session's profile: the wall.
    profile: &'static str,
    /// The topic it is about: every pointer must be of it. `None`: it must be offered
    /// nothing at all.
    topic: Option<usize>,
    /// Whether something must be offered by words alone too (it says the thing again in
    /// its own words); else only with meaning.
    by_words: bool,
}

/// The prompts: each topic asked again as it was asked, each asked about loosely, and
/// prompts that are about nothing kept (too short, a command, everyday requests, one
/// telling word of a topic among words that are nowhere).
fn prompts() -> Vec<Prompt> {
    let mut ps = Vec::new();
    for (i, topic) in TOPICS.iter().enumerate() {
        let [a, b] = topic.terms;
        let profile = Topic::profile(i);
        ps.push(Prompt { text: topic.ask.to_owned(), profile, topic: Some(i), by_words: true });
        ps.push(Prompt {
            text: format!("remind me what we settled about {a} and {b}"),
            profile,
            topic: Some(i),
            by_words: false,
        });
    }
    let nothing = [
        "fix it",
        "yes go ahead",
        "continue with the next one please",
        "/compact keep the retry backoff decision",
        "<ptyro-message from=\"lead\">the retry backoff cache eviction work is yours</ptyro-message>",
        "please rename that variable to something clearer and shorter",
        "can you explain what this function returns when the list is empty",
        "commit what you have and open a pull request for review",
        "why is the build red on the main branch this morning",
        // One telling word of a topic among words that are nowhere: not enough.
        "the retry button on the checkout page looks misaligned on mobile",
        "translate the login screen and the footer into german",
        "what is the capital of france and its population",
    ];
    for text in nothing {
        ps.push(Prompt { text: text.to_owned(), profile: "work", topic: None, by_words: false });
    }
    ps
}

/// Every prompt through [`Daemon::kb_pointers`]: of the prompts that must be offered
/// something, how many were (the offer rate), and how many pointers were given in all. It
/// fails on any pointer that is not of the prompt's topic, is behind the wall, is of a kind
/// never offered or of a forgotten note, on more than three, and on a prompt about nothing
/// that was offered anything.
async fn measure_offers(d: &Daemon, mode: &str) -> (f64, usize) {
    let (mut wanted, mut offered, mut pointers) = (0usize, 0usize, 0usize);
    for p in prompts() {
        let scope = KbScope::of("profile", p.profile, None);
        let seen = std::collections::HashSet::new();
        let hits = match d.kb_pointers(&p.text, Some(&scope), None, &seen).await {
            Ok(h) => h,
            Err(e) => panic!("{mode}: {:?}: {e:?}", p.text),
        };
        let got: Vec<String> = hits.iter().filter_map(|h| h.uri.clone()).collect();
        assert_eq!(got.len(), hits.len(), "{mode}: {:?}: a chat was offered", p.text);
        assert!(got.len() <= ptyro_core::kb::offer::MAX, "{mode}: {:?}: {got:?}", p.text);
        if std::env::var_os("KB_EVAL_VERBOSE").is_some() {
            println!("  {mode:8} offer {:?} as {}: {got:?}", p.text, p.profile);
        }
        let Some(i) = p.topic else {
            assert!(got.is_empty(), "{mode}: {:?} must be offered nothing: {got:?}", p.text);
            continue;
        };
        let topic = &TOPICS[i];
        let other = if p.profile == "work" { "home" } else { "work" };
        for (h, uri) in hits.iter().zip(&got) {
            assert!(uri.contains(topic.slug), "{mode}: {:?} was offered {uri}", p.text);
            assert_ne!(
                h.profile.as_deref(),
                Some(other),
                "{mode}: {:?}: over the wall: {uri}",
                p.text
            );
            assert!(
                *uri != topic.src() && !uri.starts_with("shell://"),
                "{mode}: {:?}: a kind that is never offered: {uri}",
                p.text
            );
            assert!(
                !(FORGOTTEN.contains(&i) && *uri == topic.note(i)),
                "{mode}: {:?}: a forgotten note: {uri}",
                p.text
            );
        }
        if mode == "meaning" || p.by_words {
            wanted += 1;
            offered += usize::from(!got.is_empty());
        } else {
            // By words alone a loose question shares too few words with anything: silence.
            assert!(got.is_empty(), "{mode}: {:?}: {got:?}", p.text);
        }
        pointers += got.len();
    }
    (offered as f64 / wanted as f64, pointers)
}

/// A daemon with the corpus kept, the chats written and three notes forgotten. Meaning
/// search is on, so every prose chunk has its vector.
async fn daemon(t: &tempfile::TempDir, now: u64) -> Arc<Daemon> {
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    set_config(&d, "[kb]\nmeaning = true\n");
    d.start_kb();
    let corpus = corpus(now);
    assert_eq!(corpus.len(), 300);
    for p in corpus {
        let uri = p.source.uri.clone();
        if let Err(e) = d.kb_ingest(p).await {
            panic!("{uri}: {e:?}");
        }
    }
    for (i, topic) in TOPICS.iter().enumerate().take(CHATS) {
        let id = format!("golden-chat-{i}");
        chat(&d, &id, Topic::profile(i), "w1", &[topic.ask, topic.fix], now - (i as u64 + 1) * DAY)
            .await;
    }
    embed_chats(&d).await;
    let db = d.kb_db().unwrap();
    for i in FORGOTTEN {
        let note = db.item_by_uri(TOPICS[i].note(i)).await.unwrap().unwrap();
        db.forget(note.id).await.unwrap();
    }
    d
}

/// What an answer holds, in order, as the questions name things: an item's URI, a chat's id.
fn answer(hits: &KbHits) -> Vec<String> {
    hits.hits
        .iter()
        .map(|h| match &h.uri {
            Some(u) => u.clone(),
            None => format!("chat:{}", h.item),
        })
        .collect()
}

/// Ask every question: recall at 5 (of all the musts, how many were among the first five)
/// and the mean reciprocal rank (of the first must, anywhere in the answer). A never row is
/// a failure whatever the numbers.
async fn measure(d: &Daemon, mode: &str) -> (f64, f64) {
    let (mut wanted, mut found, mut rr) = (0usize, 0usize, 0.0);
    let questions = questions();
    for q in &questions {
        let scope = q.profile.map(|p| KbScope::of("profile", p, None));
        let ask = KbSearch { query: q.query.clone(), limit: 20, ..KbSearch::default() };
        let hits = match d.kb_search(ask, scope.as_ref()).await {
            Ok(h) => h,
            Err(e) => panic!("{mode}: {:?}: {e:?}", q.query),
        };
        let got = answer(&hits);
        for n in &q.never {
            assert!(
                !got.contains(n),
                "{mode}: {:?} as {:?} must never find {n}: {got:?}",
                q.query,
                q.profile
            );
        }
        wanted += q.must.len();
        found += q.must.iter().filter(|m| got.iter().take(5).any(|g| g == *m)).count();
        let first = got.iter().position(|g| q.must.contains(g));
        rr += first.map_or(0.0, |at| 1.0 / (at as f64 + 1.0));
        if std::env::var_os("KB_EVAL_VERBOSE").is_some() {
            let at = first.map_or("-".to_owned(), |a| (a + 1).to_string());
            println!(
                "  {mode:8} {at:>2}  {:?} as {:?}: {:?}",
                q.query,
                q.profile,
                &got[..got.len().min(5)]
            );
        }
    }
    (found as f64 / wanted as f64, rr / questions.len() as f64)
}

fn report(mode: &str, (recall, mrr): (f64, f64)) {
    println!("kb-eval  {mode:8}  recall@5 {recall:.3}  mrr {mrr:.3}");
}

#[tokio::test]
async fn the_golden_set() {
    let t = tempfile::tempdir().unwrap();
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    model(&d);
    drop(d);
    let d = daemon(&t, now_ms()).await;
    println!(
        "kb-eval  {} items, {} chats, {} questions, the synthetic model",
        300,
        CHATS,
        questions().len()
    );
    let mut failed = Vec::new();
    for (mode, config) in [("meaning", "[kb]\nmeaning = true\n"), ("words", "")] {
        set_config(&d, config);
        let got = measure(&d, mode).await;
        report(mode, got);
        // What is offered at a prompt (KB5): the rate among the prompts that must be
        // offered something. Its hard rows (nothing for a prompt about nothing, nothing
        // over the wall, nothing of another topic) fail inside.
        let (rate, pointers) = measure_offers(&d, mode).await;
        println!(
            "kb-eval  {mode:8}  offered {rate:.3} of the prompts that ask, {pointers} pointers"
        );
        let Some((_, floor)) = OFFERED.iter().find(|c| c.0 == mode) else { continue };
        if rate < floor - SLACK {
            failed.push(format!("{mode} offers fell to {rate:.3} from {floor:.3}"));
        }
        let Some((_, recall, mrr)) = COMMITTED.iter().find(|c| c.0 == mode) else { continue };
        for (name, got, committed) in [("recall@5", got.0, *recall), ("mrr", got.1, *mrr)] {
            if got < committed - SLACK {
                failed.push(format!("{mode} {name} fell to {got:.3} from {committed:.3}"));
            } else if got > committed + SLACK {
                println!(
                    "kb-eval  {mode} {name} is {got:.3}, above the committed {committed:.3}: raise it in golden.rs"
                );
            }
        }
    }
    assert!(failed.is_empty(), "the golden set: {}", failed.join("; "));
}

/// `make kb-eval-real`: the same corpus and questions with `potion-base-8M`, when it is on
/// this machine (`PTYRO_KB_EVAL_MODEL`, a folder with the model's files). It reports, and
/// holds only the never rows: the numbers are the model's.
#[tokio::test]
#[ignore = "needs the real model: run by `make kb-eval-real`"]
async fn the_golden_set_with_the_real_model() {
    let from = std::env::var_os("PTYRO_KB_EVAL_MODEL").map(std::path::PathBuf::from);
    let Some(from) = from.filter(|f| ptyro_meaning::FILES.iter().all(|x| f.join(x.name).is_file()))
    else {
        println!(
            "kb-eval-real: skipped, the model isn't on this machine. Switch meaning search on \
             once (`ptyro config set kb.meaning true` downloads it), or point \
             PTYRO_KB_EVAL_MODEL at a folder that holds {}",
            ptyro_meaning::NAME
        );
        return;
    };
    let t = tempfile::tempdir().unwrap();
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    let to = d.paths.data.join("models").join(ptyro_meaning::NAME);
    std::fs::create_dir_all(&to).unwrap();
    for f in ptyro_meaning::FILES {
        std::fs::copy(from.join(f.name), to.join(f.name)).unwrap();
    }
    drop(d);
    let d = daemon(&t, now_ms()).await;
    println!("kb-eval-real  {} from {}", ptyro_meaning::NAME, from.display());
    for (mode, config) in [("meaning", "[kb]\nmeaning = true\n"), ("words", "")] {
        set_config(&d, config);
        report(mode, measure(&d, mode).await);
    }
}
