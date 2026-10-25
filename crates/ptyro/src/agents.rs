//! `ptyro status`, `resume`, `hibernate`, `inbox`, `mute`, `usage`, `notify`, `integrations`
//! (SPEC §8, §9, M4).

use std::path::PathBuf;

use clap::Subcommand;
use ptyro_core::config::{Config, parse_duration};
use ptyro_core::session::SessionInfo;
use ptyro_core::{Error, ErrorCode, now_ms};
use ptyro_protocol::{Reply, Request};

use crate::cli::{Ctx, age, no_daemon, truncate, unexpected};

#[derive(Subcommand)]
pub enum NotifyCmd {
    /// Send a test notification through every channel and sink
    Test,
}

#[derive(Subcommand)]
pub enum IntegrationsCmd {
    /// What's installed and what each agent gets per launch
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Install an opt-in integration: `gemini` (a linked extension), `antigravity` (hooks)
    Install {
        agent: String,
        /// Into this profile's agent home (default: the default profile's)
        #[arg(long)]
        profile: Option<String>,
    },
    /// Remove it again
    Uninstall {
        agent: String,
        #[arg(long)]
        profile: Option<String>,
    },
}

fn status_text(s: &SessionInfo) -> String {
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
                age(r.since_ms)
            );
            if let Some(p) = r.progress {
                println!("  progress  {p}%");
            }
        }
        if let Some(a) = &info.activity {
            println!("  doing     {a}");
        }
        if let Some(a) = &info.attention {
            println!("  wants you {} ({} ago)", a.text, age(a.since_ms));
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
                    age(l.at_ms),
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
            .map_or(String::new(), |a| format!("{} ({})", a.text, age(a.since_ms)));
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
            _ => return Err(Error::new(ErrorCode::NotFound, format!("no session matches {a:?}"))),
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

pub async fn inbox(
    ctx: &Ctx,
    workspace: Option<String>,
    unread: bool,
    read: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(no_daemon)?;
    if read {
        c.request(Request::InboxMark { ids: Vec::new(), all: true, session: None, read: true })
            .await?;
        eprintln!("all read");
        return Ok(0);
    }
    let Reply::Inbox { entries } =
        c.request(Request::Inbox { workspace, unread_only: unread, limit: Some(200) }).await?
    else {
        return Err(Error::internal("unexpected reply"));
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&entries).unwrap_or_default());
        return Ok(0);
    }
    if entries.is_empty() {
        eprintln!("the inbox is empty");
    }
    for e in entries {
        let mark = if e.read_ms.is_none() { "●" } else { " " };
        println!("{mark} {:>4} {:<12} {} — {}", age(e.at_ms), e.kind.label(), e.title, e.body);
    }
    Ok(0)
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

/// The agent's home under a profile (`GEMINI_CLI_HOME`, or the default home).
async fn agent_home(ctx: &Ctx, profile: Option<&str>) -> Result<PathBuf, Error> {
    let c = ctx.connect(true).await?;
    let Reply::Profiles { profiles } = c.request(Request::ProfileList).await? else {
        return Err(Error::internal("unexpected reply"));
    };
    let name = profile.unwrap_or(ptyro_core::model::DEFAULT_PROFILE);
    let p =
        profiles.iter().find(|p| p.name == name).ok_or_else(|| Error::missing("profile", name))?;
    // Gemini and Antigravity both live under the Gemini home: `<GEMINI_CLI_HOME>/.gemini`.
    let base =
        p.dirs.get("gemini").cloned().unwrap_or_else(|| ptyro_core::config::expand_home("~"));
    let base = if name == ptyro_core::model::DEFAULT_PROFILE && !p.defined {
        ptyro_core::config::expand_home("~")
    } else {
        base
    };
    Ok(base.join(".gemini"))
}

/// Antigravity's `hooks.json`: `None` when there is none, the parsed object when there is. A file
/// that exists and doesn't parse as a JSON object (comments, a trailing comma, an array) is the
/// user's own: refuse, don't replace it with a guess.
fn read_hooks(file: &std::path::Path) -> Result<Option<serde_json::Value>, Error> {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::internal(format!("{}: {e}", file.display()))),
    };
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) if v.is_object() => Ok(Some(v)),
        Ok(_) => Err(Error::invalid(format!(
            "{} isn't a JSON object, so ptyro won't change it; add its hooks by hand or fix the file",
            file.display()
        ))),
        Err(e) => Err(Error::invalid(format!(
            "{} isn't plain JSON ({e}), so ptyro won't rewrite it; remove the comments or trailing commas and run this again",
            file.display()
        ))),
    }
}

pub async fn integrations(ctx: &Ctx, cmd: IntegrationsCmd) -> Result<i32, Error> {
    let ptyro = std::env::current_exe()
        .map_or_else(|_| "ptyro".into(), |p| p.to_string_lossy().into_owned());
    match cmd {
        IntegrationsCmd::Ls { json } => {
            let gem = agent_home(ctx, None).await.ok();
            let linked = gem.as_ref().is_some_and(|g| {
                g.join("extensions").join("ptyro").exists()
                    || std::fs::read_to_string(g.join("extensions/extension-enablement.json"))
                        .is_ok_and(|t| t.contains("ptyro"))
            });
            let agy = gem
                .map(|g| g.join("config/hooks.json"))
                .and_then(|f| std::fs::read_to_string(f).ok())
                .is_some_and(|t| t.contains("hook antigravity"));
            let rows: [(&str, bool, bool, &str); 5] = [
                (
                    "claude",
                    false,
                    true,
                    "per launch: --plugin-dir with ptyro's hooks, --settings statusLine relay (nothing installed)",
                ),
                (
                    "codex",
                    false,
                    true,
                    "per launch: -c hooks, pre-trusted by hash, and notify (nothing installed)",
                ),
                (
                    "opencode",
                    false,
                    true,
                    "per launch: its server's event stream (nothing installed)",
                ),
                (
                    "gemini",
                    true,
                    linked,
                    if linked {
                        "extension linked"
                    } else {
                        "not installed (`ptyro integrations install gemini`)"
                    },
                ),
                (
                    "antigravity",
                    true,
                    agy,
                    if agy {
                        "hooks installed"
                    } else {
                        "not installed (`ptyro integrations install antigravity`)"
                    },
                ),
            ];
            if json {
                let v: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|(a, install, ok, text)| serde_json::json!({ "agent": a, "needs_install": install, "active": ok, "detail": text }))
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            } else {
                for (a, _, _, text) in rows {
                    println!("{a:<12} {text}");
                }
            }
            Ok(0)
        }
        IntegrationsCmd::Install { agent, profile } => match agent.as_str() {
            "gemini" => {
                let dir = ctx.paths.data.join("integrations/gemini/ptyro");
                for (rel, text) in ptyro_agents::others::gemini_extension(&ptyro) {
                    let p = dir.join(rel);
                    std::fs::create_dir_all(p.parent().unwrap_or(&dir))
                        .map_err(|e| Error::internal(e.to_string()))?;
                    std::fs::write(&p, text).map_err(|e| Error::internal(e.to_string()))?;
                }
                let home = agent_home(ctx, profile.as_deref()).await?;
                let config = Config::load(&Config::path(&ctx.paths)).unwrap_or_default();
                let argv = ptyro_agents::command(&config, "gemini")
                    .unwrap_or_else(|| vec!["gemini".into()]);
                let mut cmd = std::process::Command::new(&argv[0]);
                cmd.args(&argv[1..]).args(["extensions", "link"]).arg(&dir).arg("--consent");
                if profile.is_some() {
                    cmd.env("GEMINI_CLI_HOME", home.parent().unwrap_or(&home));
                }
                match cmd.status() {
                    Ok(s) if s.success() => {
                        eprintln!(
                            "linked the ptyro extension ({}); inert outside ptyro",
                            dir.display()
                        );
                        Ok(0)
                    }
                    other => {
                        eprintln!(
                            "couldn't run `gemini extensions link` ({other:?}). Run it yourself:"
                        );
                        eprintln!("  gemini extensions link {} --consent", dir.display());
                        Ok(ErrorCode::Unsupported.exit_code())
                    }
                }
            }
            "antigravity" | "agy" => {
                let home = agent_home(ctx, profile.as_deref()).await?;
                let file = home.join("config/hooks.json");
                let ours: serde_json::Value =
                    serde_json::from_str(&ptyro_agents::others::agy_hooks(&ptyro))
                        .unwrap_or_default();
                let mut cur =
                    read_hooks(&file)?.unwrap_or_else(|| serde_json::json!({ "hooks": {} }));
                if cur.get("hooks").is_some_and(|h| !h.is_object()) {
                    return Err(Error::invalid(format!(
                        "{}: \"hooks\" isn't an object, so ptyro won't change it",
                        file.display()
                    )));
                }
                // Merge: add our command hooks per event, never replace the user's.
                for (ev, groups) in ours["hooks"].as_object().cloned().unwrap_or_default() {
                    let list = cur["hooks"][&ev].as_array().cloned().unwrap_or_default();
                    let has = list.iter().any(|g| g.to_string().contains("hook antigravity"));
                    if !has {
                        let mut list = list;
                        list.extend(groups.as_array().cloned().unwrap_or_default());
                        cur["hooks"][&ev] = serde_json::Value::Array(list);
                    }
                }
                std::fs::create_dir_all(file.parent().unwrap_or(&home))
                    .map_err(|e| Error::internal(e.to_string()))?;
                crate::config::write_atomic(
                    &file,
                    &serde_json::to_string_pretty(&cur).unwrap_or_default(),
                    0o644,
                )?;
                eprintln!(
                    "added ptyro's hooks to {} (inert outside ptyro; [unverified] against agy)",
                    file.display()
                );
                Ok(0)
            }
            other => Err(Error::invalid(format!(
                "{other} needs no installing (ptyro wires it per launch)"
            ))),
        },
        IntegrationsCmd::Uninstall { agent, profile } => match agent.as_str() {
            "gemini" => {
                let config = Config::load(&Config::path(&ctx.paths)).unwrap_or_default();
                let argv = ptyro_agents::command(&config, "gemini")
                    .unwrap_or_else(|| vec!["gemini".into()]);
                let mut cmd = std::process::Command::new(&argv[0]);
                cmd.args(&argv[1..]).args(["extensions", "uninstall", "ptyro"]);
                // The profile it was installed under, not the default account's.
                if profile.is_some() {
                    let home = agent_home(ctx, profile.as_deref()).await?;
                    cmd.env("GEMINI_CLI_HOME", home.parent().unwrap_or(&home));
                }
                let status = cmd.status();
                eprintln!(
                    "{}",
                    if status.is_ok_and(|s| s.success()) {
                        "removed"
                    } else {
                        "run: gemini extensions uninstall ptyro"
                    }
                );
                Ok(0)
            }
            "antigravity" | "agy" => {
                let home = agent_home(ctx, profile.as_deref()).await?;
                let file = home.join("config/hooks.json");
                // A file that isn't there has nothing of ours; one that doesn't parse is the user's
                // to look at (it is not rewritten from a guess).
                let mut cur = read_hooks(&file)?.unwrap_or_default();
                if let Some(hooks) = cur["hooks"].as_object_mut() {
                    for groups in hooks.values_mut() {
                        if let Some(list) = groups.as_array_mut() {
                            list.retain(|g| !g.to_string().contains("hook antigravity"));
                        }
                    }
                    crate::config::write_atomic(
                        &file,
                        &serde_json::to_string_pretty(&cur).unwrap_or_default(),
                        0o644,
                    )?;
                }
                eprintln!("removed ptyro's hooks from {}", file.display());
                Ok(0)
            }
            other => Err(Error::invalid(format!("nothing to uninstall for {other}"))),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hooks_file_that_is_not_plain_json_is_never_rewritten() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("hooks.json");
        assert_eq!(read_hooks(&f).unwrap(), None, "no file: nothing of the user's to lose");
        std::fs::write(&f, "{ // mine\n \"hooks\": {} }").unwrap();
        let e = read_hooks(&f).unwrap_err();
        assert!(
            e.message.contains("isn't plain JSON") && e.message.contains("hooks.json"),
            "{}",
            e.message
        );
        std::fs::write(&f, "[1]").unwrap();
        assert!(read_hooks(&f).unwrap_err().message.contains("isn't a JSON object"));
        std::fs::write(&f, r#"{"hooks":{"Stop":[]}}"#).unwrap();
        assert!(read_hooks(&f).unwrap().is_some());
        // The file the user had is untouched by all of that.
        assert_eq!(std::fs::read_to_string(&f).unwrap(), r#"{"hooks":{"Stop":[]}}"#);
    }
}
