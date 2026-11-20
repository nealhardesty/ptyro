use std::collections::HashSet;
use std::time::Instant;

use ptyro_core::kb::chunk::Part;
use ptyro_core::kb::ingest::Stamp;
use ptyro_core::kb::{ChunkKind, ItemKind, Writer};
use ptyro_store::Registry;

use super::super::search::tests::{prepared, set_config};
use super::*;

fn session(n: usize, title: &str, said: &[&str], profile: &str) -> super::super::Prepared {
    let now = now_ms();
    let parts = said
        .iter()
        .enumerate()
        .map(|(i, text)| Part::new(ChunkKind::User, *text, now).anchor(format!("u{i}")))
        .collect();
    prepared(
        ItemKind::Session,
        &format!("session://claude/s{n}"),
        title,
        parts,
        Stamp {
            session: Some(format!("ptyro-s{n}")),
            agent: Some("claude".into()),
            ..Stamp::new(Writer::Agent, Some(profile.to_owned()))
        },
    )
}

/// One pointer an item however many of its pieces match, never an item seen before, never
/// the asking session's own, and nothing from behind the wall.
#[tokio::test]
async fn one_pointer_an_item_never_one_seen_never_its_own() {
    let t = tempfile::tempdir().unwrap();
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    set_config(&d, "");
    d.start_kb();
    let twice = ["the upload queue retries with backoff", "backoff of the upload queue retries"];
    for p in [
        session(1, "Upload retries", &twice, "work"),
        session(2, "Upload retries again", &twice[..1], "work"),
        session(3, "Home uploads", &twice[..1], "home"),
    ] {
        d.kb_ingest(p).await.unwrap();
    }
    let prompt = "how do the upload queue retries back off";
    let scope = KbScope::of("profile", "work", None);
    let uris = |hits: Vec<KbHit>| -> Vec<String> {
        let mut u: Vec<String> = hits.into_iter().filter_map(|h| h.uri).collect();
        u.sort();
        u
    };
    let hits = d.kb_pointers(prompt, Some(&scope), None, &HashSet::new()).await.unwrap();
    let first = hits[0].item.clone();
    assert_eq!(uris(hits), ["session://claude/s1", "session://claude/s2"]);
    // What it was offered before is not offered again.
    let seen = HashSet::from([first]);
    assert_eq!(d.kb_pointers(prompt, Some(&scope), None, &seen).await.unwrap().len(), 1);
    // Its own session's words are in its context already.
    let own = KbScope { session: Some("ptyro-s1".into()), ..scope.clone() };
    let hits = d.kb_pointers(prompt, Some(&own), None, &HashSet::new()).await.unwrap();
    assert_eq!(uris(hits), ["session://claude/s2"]);
    // Two of the prompt's words in a piece are not enough by words alone.
    let none = HashSet::new();
    let hits = d.kb_pointers("is the upload queue healthy today", Some(&scope), None, &none);
    assert!(hits.await.unwrap().is_empty());
}

/// `make bench-kb-offer`: how long the search behind an offer takes over a knowledge base
/// of some size (20,000 sessions, 80,000 pieces of 40 words from 3,000 words, the common
/// ones very common), for prompts of eight words: every way to pick three of the six that
/// are kept, 20 terms. It reports; the cut-off is what keeps a slow one from a prompt.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement: run by `make bench-kb-offer`"]
async fn the_search_behind_an_offer_at_size() {
    let t = tempfile::tempdir().unwrap();
    let d = crate::daemon::test_daemon(t.path(), Registry::default());
    set_config(&d, "");
    d.start_kb();
    let mut seed = 0x2545_f491_4f6c_dd1d_u64;
    let mut next = || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    // (Squared: a few words are in most pieces, most words in few.)
    let mut word = || {
        let r = (next() % 1_000_000) as f64 / 1_000_000.0;
        format!("w{}", (r * r * 3000.0) as usize)
    };
    let started = Instant::now();
    for n in 0..20_000 {
        let said: Vec<String> =
            (0..4).map(|_| (0..40).map(|_| word()).collect::<Vec<_>>().join(" ")).collect();
        let said: Vec<&str> = said.iter().map(String::as_str).collect();
        d.kb_ingest(session(n, "A session", &said, "work")).await.unwrap();
    }
    println!("bench-kb-offer  20,000 sessions kept in {:.0?}", started.elapsed());
    let scope = KbScope::of("profile", "work", None);
    let mut took = Vec::new();
    let mut offered = 0;
    for _ in 0..200 {
        let prompt = (0..8).map(|_| word()).collect::<Vec<_>>().join(" ");
        let at = Instant::now();
        let hits = d.kb_pointers(&prompt, Some(&scope), None, &HashSet::new()).await.unwrap();
        took.push(at.elapsed());
        offered += usize::from(!hits.is_empty());
    }
    took.sort();
    let over = took.iter().filter(|t| t.as_millis() as u64 > offer::CUT_OFF_MS).count();
    println!(
        "bench-kb-offer  200 prompts: p50 {:.1?}, p95 {:.1?}, the slowest {:.1?}; {over} over the {} ms cut-off; {offered} were offered something",
        took[100],
        took[190],
        took[199],
        offer::CUT_OFF_MS
    );
}
