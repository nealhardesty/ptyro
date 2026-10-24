//! What the web app asks of the machine ptyro runs on (ROADMAP WEB1.6): the config file, the
//! doctor's checks, profiles, agent integrations, the login service, the daemon's log and its
//! restart. The CLI does these with files and subprocesses of its own; a browser can't, so
//! `ptyro serve` does them for it at `POST /api/host`, for a device with `full` scope only (they
//! change the machine). The work is the CLI's own, shared, not copied.
//!
//! A request is `{"op": "config.get", …}`; the answer is JSON, or an error `{code, message}`.

use ptyro_core::config::Config;
use ptyro_core::{Error, ErrorCode};
use serde_json::{Value, json};

use crate::cli::Ctx;

fn str_of<'a>(a: &'a Value, k: &str) -> Result<&'a str, Error> {
    a[k].as_str().ok_or_else(|| Error::invalid(format!("{k} is needed")))
}

fn opt(a: &Value, k: &str) -> Option<String> {
    a[k].as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Run ptyro itself with `args` (the CLI's own non-interactive commands) and say what it did.
async fn cli(args: &[&str]) -> Result<Value, Error> {
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("ptyro's path: {e}")))?;
    let out = tokio::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map_err(|e| Error::internal(format!("ptyro {}: {e}", args.join(" "))))?;
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim().to_owned();
    Ok(json!({ "ok": out.status.success(), "out": text(&out.stdout), "err": text(&out.stderr) }))
}

/// Start `args` as a ptyro of its own, detached: it may outlive (or end) whatever asked.
fn detached(args: &[&str]) -> Result<(), Error> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("ptyro's path: {e}")))?;
    std::process::Command::new(exe)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .map(|_| ())
        .map_err(|e| Error::internal(format!("ptyro {}: {e}", args.join(" "))))
}

/// What a CLI run said, as lines.
fn notes(r: &Value) -> Vec<String> {
    [&r["err"], &r["out"]]
        .iter()
        .filter_map(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_owned))
        .collect()
}

fn checks(r: &crate::doctor::Report) -> Value {
    json!({ "checks": r.0 })
}

pub async fn run(ctx: &Ctx, op: &str, a: &Value) -> Result<Value, Error> {
    let config_path = Config::path(&ctx.paths);
    match op {
        // ── the config file ──
        "config.get" => {
            let exists = config_path.exists();
            let text = if exists {
                std::fs::read_to_string(&config_path)
                    .map_err(|e| Error::internal(format!("{}: {e}", config_path.display())))?
            } else {
                crate::config::STARTER.to_owned()
            };
            Ok(json!({ "path": config_path, "exists": exists, "text": text }))
        }
        "config.check" => {
            let notes = crate::config::check_text(str_of(a, "text")?, &config_path)
                .map_err(|e| Error::new(ErrorCode::Invalid, e))?;
            Ok(json!({ "notes": notes }))
        }
        "config.save" => {
            let text = str_of(a, "text")?;
            let notes = crate::config::check_text(text, &config_path)
                .map_err(|e| Error::new(ErrorCode::Invalid, e))?;
            crate::config::write_private(&config_path, text)?;
            if let Ok(c) = ctx.connect(false).await {
                let _ = c.request(ptyro_protocol::Request::ReloadConfig).await;
            }
            Ok(json!({ "notes": notes }))
        }
        "config.show" => {
            let config = Config::load(&config_path)
                .map_err(|e| Error::new(ErrorCode::Invalid, e.to_string()))?;
            let text =
                toml::to_string_pretty(&config).map_err(|e| Error::internal(e.to_string()))?;
            Ok(json!({ "path": config_path, "text": text }))
        }

        // ── health ──
        "doctor" => Ok(checks(
            &crate::doctor::collect(ctx, a["quick"].as_bool().unwrap_or(false), false).await,
        )),
        "profile.doctor" => Ok(checks(&crate::doctor::profile_checks(ctx, opt(a, "name")).await?)),

        // ── profiles ──
        "profile.new" => {
            let adopt = a["adopt"]
                .as_object()
                .map(|m| {
                    m.iter()
                        .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                        .collect()
                })
                .unwrap_or_default();
            let new = crate::profiles::NewProfile {
                name: str_of(a, "name")?.to_owned(),
                adopt,
                label: opt(a, "label"),
                color: opt(a, "color"),
                git_name: opt(a, "git_name"),
                git_email: opt(a, "git_email"),
                ssh_key: opt(a, "ssh_key"),
                matches: a["matches"]
                    .as_array()
                    .map(|m| m.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default(),
            };
            Ok(json!({ "notes": crate::profiles::create(ctx, new).await? }))
        }
        "profile.rm" => Ok(
            json!({ "notes": crate::profiles::remove(ctx, str_of(a, "name")?, a["force"].as_bool().unwrap_or(false)).await? }),
        ),
        "profile.rename" => Ok(
            json!({ "notes": crate::profiles::rename(ctx, str_of(a, "old")?, str_of(a, "new")?).await? }),
        ),
        "profile.sync" => Ok(json!({ "notes": crate::profiles::sync(ctx, str_of(a, "name")?)? })),
        "profile.detect" => {
            let found: Vec<Value> = crate::profiles::detect_list(ctx)?
                .into_iter()
                .map(|(s, known)| json!({ "profile": s.profile, "sources": s.sources, "toml": s.to_toml(), "known": known }))
                .collect();
            Ok(json!({ "found": found }))
        }
        "profile.detect_apply" => {
            let only: Option<Vec<String>> = a["profiles"]
                .as_array()
                .map(|m| m.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect());
            Ok(json!({ "notes": crate::profiles::detect_apply(ctx, only.as_deref()).await? }))
        }
        "profile.login" => {
            let (_c, info) =
                crate::profiles::login(ctx, str_of(a, "name")?, str_of(a, "target")?, false)
                    .await?;
            Ok(json!({ "session": info.id.full() }))
        }

        // ── agent integrations, the login service ──
        "integrations.ls" => {
            let r = cli(&["integrations", "ls", "--json"]).await?;
            Ok(
                json!({ "integrations": serde_json::from_str::<Value>(r["out"].as_str().unwrap_or("[]")).unwrap_or(json!([])) }),
            )
        }
        "integrations.install" | "integrations.uninstall" => {
            let verb = if op.ends_with("uninstall") { "uninstall" } else { "install" };
            let agent = str_of(a, "agent")?.to_owned();
            let mut args = vec!["integrations", verb, &agent];
            let profile = opt(a, "profile");
            if let Some(p) = &profile {
                args.extend(["--profile", p]);
            }
            let r = cli(&args).await?;
            if r["ok"] == json!(false) {
                return Err(Error::invalid(
                    format!(
                        "{} {}",
                        r["err"].as_str().unwrap_or(""),
                        r["out"].as_str().unwrap_or("")
                    )
                    .trim()
                    .to_owned(),
                ));
            }
            Ok(json!({ "notes": notes(&r) }))
        }
        "service.install" | "service.uninstall" => {
            let verb =
                if op.ends_with("uninstall") { "uninstall-service" } else { "install-service" };
            let r = cli(&["daemon", verb]).await?;
            if r["ok"] == json!(false) {
                return Err(Error::invalid(r["err"].as_str().unwrap_or("failed").to_owned()));
            }
            Ok(json!({ "notes": notes(&r) }))
        }

        // ── the daemon ──
        "daemon.logs" => {
            let dir = ctx.paths.log_dir();
            let newest = std::fs::read_dir(&dir)
                .map_err(|e| Error::new(ErrorCode::NotFound, format!("{}: {e}", dir.display())))?
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_string_lossy().starts_with("daemon"))
                .max_by_key(|e| e.metadata().and_then(|m| m.modified()).ok())
                .ok_or_else(|| {
                    Error::new(ErrorCode::NotFound, format!("no daemon logs in {}", dir.display()))
                })?;
            let lines = a["lines"].as_u64().unwrap_or(200).min(5000) as usize;
            let text = std::fs::read_to_string(newest.path()).unwrap_or_default();
            let all: Vec<&str> = text.lines().collect();
            Ok(
                json!({ "path": newest.path(), "text": all[all.len().saturating_sub(lines)..].join("\n") }),
            )
        }
        "web.logs" => {
            let path = ctx.paths.state.join("serve.log");
            let lines = a["lines"].as_u64().unwrap_or(200).min(5000) as usize;
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            let all: Vec<&str> = text.lines().collect();
            Ok(json!({ "path": path, "text": all[all.len().saturating_sub(lines)..].join("\n") }))
        }
        // These end the connection that asked: they go on by themselves, and the app reconnects.
        "daemon.restart" => {
            detached(&["daemon", "restart", "--force"])?;
            Ok(json!({ "started": true }))
        }
        "daemon.stop" => {
            detached(&["daemon", "stop", "--force"])?;
            Ok(json!({ "started": true }))
        }

        // ── the tour ──
        "tour.start" => crate::tour::web_start(ctx).await,
        "tour.end" => crate::tour::web_end().await,

        other => Err(Error::invalid(format!("no such operation: {other}"))),
    }
}
