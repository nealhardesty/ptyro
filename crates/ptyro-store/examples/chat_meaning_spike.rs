//! CHAT5 spike (SPEC §22.10 tier 3): what meaning search costs over `NODES` chat messages
//! (100000 by default) with a model the size of `potion-base-8M` (30000 tokens × 256, f32), but
//! synthetic, so nothing is downloaded. Run with `make spike-chat-meaning`.
//!
//! It times what the daemon does: loading the model, embedding every message in batches and
//! storing the int8 vectors (`vec_todo`/`put_vecs`, the backfill), and a search's brute-force scan
//! (`ChatDb::meaning`), and reports the vectors' size in `chat.db`.

use std::time::Instant;

use ptyro_meaning::synthetic::{self, Rng};
use ptyro_store::chat::{ChatDb, NewChat, SearchFilter};

const VOCAB: usize = 30_000;
const DIM: usize = 256;

#[tokio::main]
async fn main() {
    let nodes: usize = std::env::var("NODES").ok().and_then(|v| v.parse().ok()).unwrap_or(100_000);
    let dir = tempfile::tempdir().expect("a temp dir");
    println!("meaning search over {nodes} messages, a {VOCAB}×{DIM} synthetic model");

    let t = Instant::now();
    synthetic::random(&dir.path().join("model"), VOCAB, DIM, 7).expect("model written");
    println!("  model written           {:>8.0} ms", ms(t));
    let t = Instant::now();
    let model = ptyro_meaning::Model::load(&dir.path().join("model")).expect("model loads");
    println!("  model loaded            {:>8.0} ms", ms(t));

    // Messages of 10–200 words, common words common (a cube skews the draw to the front).
    let db = ChatDb::open(&dir.path().join("chat.db")).expect("chat.db");
    let chats = (nodes / 50).max(1);
    for c in 0..chats {
        db.create(NewChat {
            id: format!("c{c}"),
            workspace: "w".into(),
            title: None,
            title_src: "auto".into(),
            agent: "claude".into(),
            profile: "default".into(),
            tools: "chat".into(),
            forked_from: None,
            side_of: None,
            folders: Vec::new(),
        })
        .await
        .expect("chat");
    }
    let t = Instant::now();
    db.call(move |c| {
        let mut rng = Rng(42);
        let tx = c.transaction()?;
        {
            let mut st = tx.prepare(
                "INSERT INTO node (id, chat, kind, body, text, at_ms, origin)
                 VALUES (?1, ?2, ?3, '{}', ?4, ?5, 'm')",
            )?;
            for i in 0..nodes {
                let words = 10 + rng.next_u64() % 190;
                let text = (0..words)
                    .map(|_| {
                        let u = (rng.next_u64() % 1_000_000) as f64 / 1e6;
                        format!("w{}", (u * u * u * VOCAB as f64) as usize)
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                let kind = if i % 2 == 0 { "user" } else { "assistant" };
                st.execute(rusqlite::params![
                    format!("n{i}"),
                    format!("c{}", i % chats),
                    kind,
                    text,
                    i as i64
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    })
    .await
    .expect("messages");
    println!("  {nodes} messages written {:>7.0} ms", ms(t));

    // The backfill, as the daemon runs it (without its pauses).
    let model = std::sync::Arc::new(model);
    let (t, mut embed_ms) = (Instant::now(), 0.0);
    let mut below = db.vec_top(0).await.expect("top") + 1;
    loop {
        let todo = db.vec_todo("spike".into(), false, (0, below), 128).await.expect("todo");
        let Some(last) = todo.last() else { break };
        below = last.seq;
        let e = Instant::now();
        let rows: Vec<_> = todo
            .into_iter()
            .map(|t| (t.node, ptyro_meaning::quantize(&model.embed(&t.text))))
            .collect();
        embed_ms += ms(e);
        db.put_vecs("spike".into(), rows).await.expect("put");
    }
    let all = ms(t);
    println!(
        "  backfill                {all:>8.0} ms ({embed_ms:.0} ms embedding, {:.0} µs a message)",
        embed_ms * 1000.0 / nodes as f64
    );
    let (done, of) = db.vec_counts("spike".into(), false).await.expect("counts");
    assert_eq!(done, of);

    // Searches: the scan of every vector, best 30.
    let mut times = Vec::new();
    let mut rng = Rng(9);
    for _ in 0..21 {
        let q = model.embed(&format!("w{} w{}", rng.next_u64() % 500, rng.next_u64() % 500));
        let t = Instant::now();
        let f = SearchFilter { limit: 30, ..SearchFilter::default() };
        let hits = db.meaning(f, q, "spike".into(), String::new(), -1.0).await.expect("search");
        times.push(ms(t));
        assert_eq!(hits.len(), 30.min(nodes));
    }
    times.sort_by(f64::total_cmp);
    println!(
        "  search (scan, best 30)  {:>8.1} ms median, {:.1} ms worst",
        times[times.len() / 2],
        times[times.len() - 1]
    );
    let vec_bytes = db
        .call(|c| {
            Ok(c.query_row("SELECT sum(length(vec)) FROM node_vec", [], |r| r.get::<_, i64>(0))?)
        })
        .await
        .expect("size");
    let file = std::fs::metadata(dir.path().join("chat.db")).map_or(0, |m| m.len())
        + std::fs::metadata(dir.path().join("chat.db-wal")).map_or(0, |m| m.len());
    println!(
        "  vectors                 {:>8.1} MB ({} B a message); chat.db {:.1} MB",
        vec_bytes as f64 / 1e6,
        vec_bytes / nodes as i64,
        file as f64 / 1e6
    );
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}
