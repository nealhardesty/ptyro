//! `ptyro replay` and `ptyro export` (SPEC §7.6, M8.2): a session's recording, played back with
//! its timing, or turned into asciicast v2, plain text or a `.ptylog` fixture. They read the
//! files directly, so they work for sessions that ended long ago (and with no daemon).

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{Reply, Request};
use ptyro_term::Emulator;
use ptyro_term::record::{self, Record, Timed};

use crate::cli::Ctx;

/// A session name, handle or id (live, or from history) → its id.
async fn session_id(ctx: &Ctx, addr: &str) -> Result<String, Error> {
    if let Ok(c) = ctx.connect(false).await {
        if let Ok(Reply::Session { info }) = c.request(Request::Get { addr: addr.into() }).await {
            return Ok(info.id.full());
        }
        if let Ok(Reply::History { entries }) =
            c.request(Request::History { limit: Some(5000) }).await
            && let Some(e) = entries.iter().find(|e| {
                e.name == addr || e.id == addr || e.id.ends_with(&addr.to_ascii_lowercase())
            })
        {
            return Ok(e.id.clone());
        }
    }
    Ok(addr.to_owned())
}

async fn records(ctx: &Ctx, addr: &str) -> Result<(String, Vec<Timed>), Error> {
    let id = session_id(ctx, addr).await?;
    let dir = ctx.paths.data.join("sessions").join(&id);
    let recs = record::read_dir(&dir).map_err(|e| {
        Error::new(
            ErrorCode::NotFound,
            format!("no recording for {addr:?} ({e}); is [recorder] enabled = false?"),
        )
    })?;
    Ok((id, recs))
}

pub async fn replay(
    ctx: &Ctx,
    session: String,
    speed: f64,
    max_idle: String,
    from: Option<String>,
) -> Result<i32, Error> {
    if !speed.is_finite() || speed < 0.01 {
        return Err(Error::invalid(format!("--speed {speed}: use a number from 0.01 up")));
    }
    let (_, recs) = records(ctx, &session).await?;
    let max_idle = crate::script::duration(&max_idle)?;
    let skip_us = from
        .as_deref()
        .map(crate::script::duration)
        .transpose()?
        .map_or(0, |d| d.as_micros() as u64);
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(b"\x1b[0m\x1b[H\x1b[2J");
    if let Some(s) = record::first_size(&recs) {
        let _ = writeln!(
            std::io::stderr(),
            "replaying {} ({}x{}); Ctrl-C stops",
            session,
            s.cols,
            s.rows
        );
    }
    let mut prev = None;
    for r in &recs {
        if r.t_us >= skip_us {
            if let Some(p) = prev {
                let gap =
                    Duration::from_micros(r.t_us.saturating_sub(p)).div_f64(speed).min(max_idle);
                let _ = out.flush();
                tokio::time::sleep(gap).await;
            }
            prev = Some(r.t_us);
        }
        if let Record::Output(b) | Record::Keyframe(b) = &r.record
            && out.write_all(b).is_err()
        {
            return Ok(0);
        }
    }
    let _ = out.write_all(b"\x1b[0m\r\n");
    let _ = out.flush();
    Ok(0)
}

pub async fn export(
    ctx: &Ctx,
    session: String,
    format: String,
    output: Option<PathBuf>,
) -> Result<i32, Error> {
    let (id, recs) = records(ctx, &session).await?;
    let bytes = match format.as_str() {
        "asciicast" | "cast" => record::asciicast(&recs, &session).into_bytes(),
        "ptylog" | "raw" => record::raw_output(&recs),
        "text" | "txt" => {
            let size = record::first_size(&recs).unwrap_or(ptyro_core::TermSize::DEFAULT);
            let mut emu = ptyro_term::AlacrittyEmulator::new(size, 1_000_000);
            record::replay(&recs, &mut emu);
            let doc = ptyro_term::frame::scrollback_doc(&emu, u32::MAX);
            let mut t = ptyro_core::cache::plain_text(&ptyro_core::cache::doc_rows(&doc));
            if !t.ends_with('\n') {
                t.push('\n');
            }
            let _ = emu.size();
            t.into_bytes()
        }
        other => {
            return Err(Error::invalid(format!(
                "unknown format {other:?} (asciicast, text, ptylog)"
            )));
        }
    };
    match output {
        Some(p) => {
            std::fs::write(&p, &bytes)
                .map_err(|e| Error::invalid(format!("{}: {e}", p.display())))?;
            eprintln!("wrote {} ({} bytes) from session {id}", p.display(), bytes.len());
        }
        None => {
            let _ = std::io::stdout().write_all(&bytes);
        }
    }
    Ok(0)
}
