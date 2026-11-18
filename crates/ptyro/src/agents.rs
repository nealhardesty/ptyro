//! `ptyro status`, `resume`, `hibernate`, `mute`, `usage`, `notify` (SPEC §8, §9, M4).

use clap::Subcommand;
use ptyro_core::config::parse_duration;
use ptyro_core::session::SessionInfo;
use ptyro_core::{Error, now_ms};
use ptyro_protocol::{Reply, Request};

use crate::cli::{Ctx, ago, no_daemon, truncate, unexpected};

#[derive(Subcommand)]
pub enum NotifyCmd {
    /// Send a test notification through every channel and sink
    Test,
}

pub(crate) fn status_text(s: &SessionInfo) -> String {
    if !s.state.is_live() {
        return s.state.label();
    }
    match &s.status {
        Some(r) => {
            let mut t = r.describe();
            if r.confidence == ptyro_core::status::Confidence::Low {
                t.push_str(" (guess)");
            }
            t
        }
        None => "-".into(),
    }
}

pub async fn status(
    ctx: &Ctx,
    session: Option<String>,
    explain: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    if let Some(addr) = session {
        let Reply::Session { info } = c.request(Request::Get { addr: addr.clone() }).await? else {
            return Err(Error::internal("unexpected reply"));
        };
        let lines = if explain {
            match c.request(Request::StatusExplain { addr }).await? {
                Reply::Explain { lines } => lines,
                other => return Err(unexpected(other)),
            }
        } else {
            Vec::new()
        };
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({ "session": info, "explain": lines })
                )
                .unwrap_or_default()
            );
            return Ok(0);
        }
        println!("{}  {}", info.name, status_text(&info));
        if let Some(r) = &info.status {
            println!(
                "  source    {} ({:?}), since {} ago",
                r.source.label(),
                r.confidence,
                ago(r.since_ms)
            );
            if let Some(p) = r.progress {
                println!("  progress  {p}%");
            }
        }
        if let Some(a) = &info.activity {
            println!("  doing     {a}");
        }
        if let Some(a) = &info.attention {
            println!("  wants you {} ({} ago)", a.text, ago(a.since_ms));
        }
        if let Some(conv) = &info.conversation {
            println!("  conversation {conv}");
        }
        let running: Vec<&str> =
            info.children.iter().filter(|c| c.running).map(|c| c.id.as_str()).collect();
        if !running.is_empty() {
            println!("  subagents {}", running.join(", "));
        }
        if explain {
            println!("\n  recent decisions (newest last):");
            for l in lines {
                println!(
                    "  {:>5} {} {:<24} {:<36} {}",
                    ago(l.at_ms),
                    if l.applied { "✓" } else { "·" },
                    l.source,
                    truncate(&l.signal, 36),
                    l.reason
                );
            }
        }
        return Ok(0);
    }
    let Reply::Sessions { sessions } = c.request(Request::List).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        let v: Vec<serde_json::Value> = sessions
            .iter()
            .map(|s| serde_json::json!({ "name": s.name, "handle": s.handle, "status": s.status, "attention": s.attention, "activity": s.activity }))
            .collect();
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(0);
    }
    println!("{:<18} {:<10} {:<34} WANTS", "NAME", "AGENT", "STATUS");
    for s in &sessions {
        let wants = s
            .attention
            .as_ref()
            .map_or(String::new(), |a| format!("{} ({})", a.text, ago(a.since_ms)));
        let doing = s.activity.as_deref().map(|a| format!(" · {a}")).unwrap_or_default();
        println!(
            "{:<18} {:<10} {:<34} {}",
            truncate(&s.name, 18),
            s.agent.as_deref().unwrap_or(match s.kind {
                ptyro_core::session::SessionKind::Shell => "shell",
                ptyro_core::session::SessionKind::Script => "script",
                _ => "-",
            }),
            truncate(&format!("{}{doing}", status_text(s)), 34),
            wants
        );
    }
    Ok(0)
}

/// Run a per-session request over selectors.
pub async fn each(
    ctx: &Ctx,
    addrs: &[String],
    verb: &str,
    req: impl Fn(String) -> Request,
) -> Result<i32, Error> {
    let c = ctx.connect(true).await?;
    let mut targets = Vec::new();
    for a in addrs {
        match c.request(Request::Resolve { addr: a.clone() }).await? {
            Reply::Sessions { sessions } if !sessions.is_empty() => targets.extend(sessions),
            _ => return Err(ptyro_core::lookup::not_found("session", a, [])),
        }
    }
    let mut code = 0;
    for s in targets {
        match c.request(req(s.id.full())).await {
            Ok(_) => eprintln!("{verb} {}", s.name),
            Err(e) => {
                eprintln!("{}: {}", s.name, e.message);
                code = e.code.exit_code();
            }
        }
    }
    Ok(code)
}

pub async fn mute(
    ctx: &Ctx,
    target: String,
    duration: Option<String>,
    off: bool,
) -> Result<i32, Error> {
    let minutes = duration
        .map(|d| {
            parse_duration(&d).map_err(Error::invalid).map(|d| (d.as_secs() / 60).max(1) as u32)
        })
        .transpose()?;
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    c.request(Request::Mute { target: target.clone(), minutes, unmute: off }).await?;
    eprintln!(
        "{target}: {}",
        if off {
            "notifications back on".to_owned()
        } else {
            minutes.map_or("muted until unmuted".into(), |m| format!("muted for {m} min"))
        }
    );
    Ok(0)
}

pub async fn usage(ctx: &Ctx, json: bool) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    let Reply::Usage { usage } = c.request(Request::Usage).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&usage).unwrap_or_default());
        return Ok(0);
    }
    if usage.is_empty() {
        eprintln!(
            "no usage yet: Claude reports it once a session's first reply arrives (Pro/Max plans);"
        );
        eprintln!("Codex needs `[agents.codex] usage = true`.");
    }
    for u in &usage {
        let windows: Vec<String> = u
            .windows
            .iter()
            .map(|w| {
                let reset = w
                    .resets_at_ms
                    .filter(|t| *t > now_ms())
                    .map(|t| format!(" (resets in {})", crate::agents::until(t)))
                    .unwrap_or_default();
                format!("{} {}%{reset}", w.name, w.used_percent)
            })
            .collect();
        let limited = u
            .limited_until_ms
            .filter(|t| *t > now_ms())
            .map(|t| format!("  RATE-LIMITED for {}", until(t)))
            .unwrap_or_default();
        let cost = u
            .cost_millis
            .map(|c| format!("  session ${:.2}", c as f64 / 1000.0))
            .unwrap_or_default();
        println!("{:<12} {:<8} {}{cost}{limited}", u.profile, u.agent, windows.join("  "));
    }
    // ccusage keeps Claude's history, when installed.
    if !json
        && let Ok(out) = std::process::Command::new("ccusage")
            .args(["daily", "--json"])
            .stderr(std::process::Stdio::null())
            .output()
        && out.status.success()
        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout)
        && let Some(total) = v["totals"]["totalCost"].as_f64()
    {
        println!(
            "ccusage: ${total:.2} in total across {} day(s)",
            v["daily"].as_array().map_or(0, Vec::len)
        );
    }
    Ok(0)
}

pub fn until(t: u64) -> String {
    let m = t.saturating_sub(now_ms()) / 60_000;
    if m >= 60 { format!("{}h{:02}m", m / 60, m % 60) } else { format!("{m}m") }
}

pub async fn notify(ctx: &Ctx, cmd: NotifyCmd) -> Result<i32, Error> {
    match cmd {
        NotifyCmd::Test => {
            let c = ctx.connect(true).await?;
            let Reply::Notified { results } = c.request(Request::NotifyTest).await? else {
                return Err(Error::internal("unexpected reply"));
            };
            for r in results {
                println!("{r}");
            }
            Ok(0)
        }
    }
}
