//! `ptyro-latency`: the key → screen round trip through a real daemon (SPEC §18.3, M8.3). Starts a
//! daemon in a throwaway home, runs `cat` in raw mode, then for each sample types one key as the
//! TUI does and times until a frame shows the echo. That one number covers key → child write, the
//! child's echo, parsing, frame building and the wire. Run via `make latency`.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ptyro_core::TermSize;
use ptyro_core::cache::ScreenCache;
use ptyro_core::input::{InputEvent, KeyCode, KeyEvent, Mods};
use ptyro_core::session::SpawnSpec;
use ptyro_protocol::{ClientMsg, Reply, Request, ServerMsg};
use ptyro_testkit::TempDaemon;

const USAGE: &str = "usage: ptyro-latency [--samples N] [--gap MS] [--compress] [PTYRO_BINARY]";

async fn main_async() -> Result<()> {
    let mut samples = 500usize;
    let mut compress = false;
    let mut gap = 20u64;
    let mut bin = "target/release/ptyro".to_owned();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--samples" => samples = args.next().context(USAGE)?.parse()?,
            "--gap" => gap = args.next().context(USAGE)?.parse()?,
            "--compress" => compress = true,
            "-h" | "--help" => {
                println!("{USAGE}");
                return Ok(());
            }
            other if !other.starts_with('-') => bin = other.to_owned(),
            other => bail!("unknown argument {other:?}\n{USAGE}"),
        }
    }
    let bin = std::fs::canonicalize(&bin).with_context(|| format!("no binary at {bin}"))?;
    let d = TempDaemon::start(&bin)?;
    let mut client = if compress {
        let mut opts =
            ptyro_client::ConnectOptions::new(d.paths.clone(), ptyro_protocol::ClientKind::Test);
        opts.autospawn = false;
        opts.compress = true;
        ptyro_client::Client::connect(&opts).await?
    } else {
        d.client().await?
    };
    let size = TermSize::new(200, 50);
    let argv =
        ["sh", "-c", "stty raw -echo; printf 'ready\r\n'; exec cat"].map(String::from).into();
    let spec =
        SpawnSpec { argv, cwd: std::env::temp_dir(), size: Some(size), ..SpawnSpec::default() };
    let Reply::Session { info } = client.request(Request::Spawn { spec }).await? else {
        bail!("spawn failed");
    };
    let mut pushes = client.pushes();
    client.send(ClientMsg::Subscribe { session: info.id, size: Some(size), control: true })?;
    let mut cache = ScreenCache::new(size);
    let mut until =
        async |cache: &mut ScreenCache, want: &dyn Fn(&ScreenCache) -> bool| -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !want(cache) {
                let left = deadline.saturating_duration_since(Instant::now());
                match tokio::time::timeout(left, pushes.recv()).await {
                    Ok(Some(ServerMsg::Screen { frame })) => {
                        cache.apply(frame.keyframe, frame.seq, &frame.delta);
                        client.send(ClientMsg::Ack { session: frame.session, seq: frame.seq })?;
                    }
                    Ok(Some(_)) => {}
                    _ => bail!("timed out; screen:\n{}", cache.plain_text()),
                }
            }
            Ok(())
        };
    until(&mut cache, &|c| c.plain_text().contains("ready")).await?;
    // Typed keys wrap along 200 columns; keep well inside the 50 rows (and away from scrolling).
    let samples = samples.min(40 * 200);
    let mut took = Vec::with_capacity(samples);
    for i in 1..=samples {
        let c = char::from(b'a' + (i % 26) as u8);
        let key = KeyEvent::new(KeyCode::Char { c }, Mods::NONE);
        let t = Instant::now();
        client.send(ClientMsg::Input { session: info.id, event: InputEvent::Key { key } })?;
        let (row, col) = (1 + (i - 1) / 200, (i - 1) % 200);
        until(&mut cache, &|s| {
            s.plain_text().lines().nth(row).and_then(|l| l.chars().nth(col)) == Some(c)
        })
        .await?;
        took.push(t.elapsed());
        // Keystrokes, not a burst: a gap shorter than `frame_interval_ms` (8 ms) measures the
        // frame pacing instead of the path.
        tokio::time::sleep(Duration::from_millis(gap)).await;
    }
    took.sort_unstable();
    let pct = |p: usize| took[(took.len() * p / 100).min(took.len() - 1)];
    let us = |d: Duration| d.as_secs_f64() * 1e6;
    println!(
        "key → echo → frame{} over {samples} keys {gap} ms apart: p50 {:.0} µs, p99 {:.0} µs, max {:.0} µs",
        if compress { " (zstd)" } else { "" },
        us(pct(50)),
        us(pct(99)),
        us(*took.last().expect("samples")),
    );
    Ok(())
}

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
    if let Err(e) = rt.block_on(main_async()) {
        eprintln!("ptyro-latency: {e:#}");
        std::process::exit(1);
    }
}
