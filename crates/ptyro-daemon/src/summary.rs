//! A session in a line, from a small model (ROADMAP UX1.3, SPEC §11.2h). Off unless
//! `[summaries] enabled`; `ptyro summarize` asks for one either way.
//!
//! - **When**: an agent's turn ends or it stops to ask (and on request). One at a time per
//!   session; a turn that ended meanwhile gets its own once that one is back.
//! - **What it's sent**: its title and task, what it's doing or asks, its last reply or a
//!   shell's command, and the end of its screen; trimmed, with the profile's secrets and
//!   anything that looks like a token replaced.
//! - **How**: `[summaries] command`, the prompt on stdin, under the session's profile (so on its
//!   account), in a directory of its own (no project files, no tools), at most `daily_cap` a
//!   day for each profile.

use std::sync::Arc;
use std::time::Duration;

use ptyro_core::session::{SessionKind, Summary, brief};
use ptyro_core::{Error, now_ms};
use ptyro_protocol::{DomainEvent, Reply, ScreenFormat};

use crate::daemon::Daemon;
use crate::session::Session;

/// Lets a turn's last reply (sent alongside its end) land first.
const SETTLE: Duration = Duration::from_millis(1500);
const LIMIT: Duration = Duration::from_secs(60);
/// Screen lines sent, from the bottom.
const SCREEN_LINES: usize = 25;

impl Daemon {
    /// After a turn ended or it stopped to ask: a summary, when they're on.
    pub(crate) fn summarize_after(self: &Arc<Self>, s: &Arc<Session>) {
        if self.config().summaries.enabled {
            self.summarize_in_background(s);
        }
    }

    fn summarize_in_background(self: &Arc<Self>, s: &Arc<Session>) {
        let (me, s) = (Arc::clone(self), Arc::clone(s));
        tokio::spawn(async move {
            tokio::time::sleep(SETTLE).await;
            if let Err(e) = me.summarize(&s).await {
                tracing::debug!("summary of {}: {}", s.id, e.message);
            }
        });
    }

    /// `Summarize`: one now, whatever `enabled` says.
    pub(crate) async fn summarize_now(self: &Arc<Self>, addr: &str) -> Result<Reply, Error> {
        let s = self.resolve(addr)?;
        self.summarize(&s).await?;
        Ok(Reply::Session { info: self.info(&s) })
    }

    async fn summarize(self: &Arc<Self>, s: &Arc<Session>) -> Result<(), Error> {
        let (turn, profile) = {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if m.summarizing {
                return Err(Error::conflict(format!("{} is being summarized already", m.name)));
            }
            m.summarizing = true;
            (m.turns, m.place.profile.clone())
        };
        let made = self.make_summary(s, &profile).await;
        let again = {
            let mut m = s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            m.summarizing = false;
            if let Ok(text) = &made {
                m.summary = Some(Summary { text: text.clone(), turn, at_ms: now_ms() });
            }
            // A turn ended while it was out: that one too.
            made.is_ok() && m.turns > turn && self.config().summaries.enabled
        };
        made?;
        let _ = self.events.send(DomainEvent::SessionUpdated { info: self.info(s) });
        if again {
            self.summarize_in_background(s);
        }
        Ok(())
    }

    async fn make_summary(
        self: &Arc<Self>,
        s: &Arc<Session>,
        profile: &str,
    ) -> Result<String, Error> {
        let cfg = self.config().summaries;
        if cfg.command.is_empty() {
            return Err(Error::invalid("[summaries] command is empty"));
        }
        self.count_summary(profile, cfg.daily_cap)?;
        let plan = self.profile_plan(profile).await?;
        let prompt = redact(&self.summary_prompt(s), &secrets(&plan));
        let env = plan.apply(self.base_env.iter().cloned());
        let dir = self.paths.data.join("summaries");
        let argv = cfg.command.clone();
        let out = tokio::task::spawn_blocking(move || {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let mut c = std::process::Command::new(&argv[0]);
            c.args(&argv[1..])
                .current_dir(&dir)
                .env_clear()
                .envs(env)
                .stderr(std::process::Stdio::null());
            crate::notifier::run_bounded(c, Some(prompt.into_bytes()), LIMIT)
        })
        .await
        .map_err(|e| Error::internal(e.to_string()))?;
        let (ok, stdout) = out.map_err(|e| Error::internal(format!("{}: {e}", cfg.command[0])))?;
        let text = parse(&String::from_utf8_lossy(&stdout));
        match text {
            Some(t) if ok => Ok(t),
            _ => Err(Error::internal(format!("{} gave no summary", cfg.command[0]))),
        }
    }

    /// Counts one against today's cap for `profile` (UTC days; counted in memory).
    fn count_summary(&self, profile: &str, cap: u32) -> Result<(), Error> {
        let key = format!("summaries:{profile}:{}", now_ms() / 86_400_000);
        let mut views = self.views.lock().expect("views");
        let n = views.get(&key).and_then(serde_json::Value::as_u64).unwrap_or(0);
        if n >= u64::from(cap) {
            return Err(Error::conflict(format!(
                "{profile} had its {cap} summaries today ([summaries] daily_cap)"
            )));
        }
        views.insert(key, (n + 1).into());
        Ok(())
    }

    fn summary_prompt(&self, s: &Arc<Session>) -> String {
        let info = self.info(s);
        let mut lines = vec![
            "Summarize this terminal session for a sidebar: in at most 15 words, what it is doing \
             now, or what it waits on. Plain text, one line, no preamble, no quotes."
                .to_owned(),
            String::new(),
        ];
        let mut add = |label: &str, v: Option<String>| {
            if let Some(v) = v.filter(|v| !v.trim().is_empty()) {
                lines.push(format!("{label}: {v}"));
            }
        };
        let what = match info.kind {
            SessionKind::Agent => format!("agent ({})", info.agent.clone().unwrap_or_default()),
            k => match &info.inner_agent {
                Some(a) => format!("shell running {a}"),
                None => format!("{k:?}").to_lowercase(),
            },
        };
        add("Session", Some(what));
        add("Title", info.title.clone());
        add("Asked to", info.task.clone());
        add(
            "Status",
            info.status.as_ref().map(|r| {
                let mut v = r.status.label().to_owned();
                if let Some(d) = &r.detail {
                    v.push_str(&format!(" ({d})"));
                }
                v
            }),
        );
        add("Waiting on", info.attention.as_ref().map(|a| a.text.clone()));
        add("Doing", info.activity.as_ref().map(|a| format!("{a:?}")));
        add(
            "Last reply",
            s.meta
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .last_reply
                .as_deref()
                .map(|r| brief(r, 1500)),
        );
        add(
            "Command",
            info.command.as_ref().and_then(|c| {
                let exit = c.exit.map(|e| format!(" (exit {e})")).unwrap_or_default();
                c.text.as_ref().map(|t| format!("{t}{exit}"))
            }),
        );
        let screen = s.read_screen(ScreenFormat::Plain);
        let tail: Vec<&str> = screen.lines().map(str::trim_end).filter(|l| !l.is_empty()).collect();
        let tail = &tail[tail.len().saturating_sub(SCREEN_LINES)..];
        if !tail.is_empty() {
            lines.push(String::new());
            lines.push("The end of its screen:".into());
            lines.extend(tail.iter().map(|l| brief(l, 200)));
        }
        lines.join("\n")
    }
}

/// The profile's secret values, with what to show instead (`<secret:NAME>`).
fn secrets(plan: &ptyro_agents::profile::Plan) -> Vec<(String, String)> {
    plan.set
        .iter()
        .zip(&plan.shown)
        .filter(|((_, real), (_, shown))| real != shown && real.len() >= 4)
        .map(|((_, real), (_, shown))| (real.clone(), shown.clone()))
        .collect()
}

/// `text` with `secrets` and anything that looks like a credential replaced.
pub(crate) fn redact(text: &str, secrets: &[(String, String)]) -> String {
    let mut text = text.to_owned();
    for (real, shown) in secrets {
        text = text.replace(real.as_str(), shown);
    }
    let mut out = String::with_capacity(text.len());
    let mut after_bearer = false;
    for piece in text.split_inclusive(char::is_whitespace) {
        let word = piece.trim_end();
        let gap = &piece[word.len()..];
        let replaced = if after_bearer && !word.is_empty() {
            Some("<redacted>".to_owned())
        } else {
            redact_word(word)
        };
        after_bearer = word.eq_ignore_ascii_case("bearer");
        out.push_str(replaced.as_deref().unwrap_or(word));
        out.push_str(gap);
    }
    out
}

const TOKEN_PREFIXES: &[&str] = &[
    "sk-",
    "ghp_",
    "gho_",
    "ghs_",
    "ghu_",
    "github_pat_",
    "glpat-",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "AKIA",
    "ASIA",
    "AIza",
];
const SECRET_KEYS: &[&str] = &["token", "secret", "password", "passwd", "api_key", "apikey"];

fn redact_word(word: &str) -> Option<String> {
    let w = word.trim_matches(|c: char| matches!(c, '"' | '\'' | '`' | ',' | ';' | '(' | ')'));
    if w.starts_with("<secret:") {
        return None;
    }
    // `KEY=value`, `--password=value`, `token:value`.
    if let Some((k, v)) = w.split_once('=').or_else(|| w.split_once(':'))
        && !v.is_empty()
        && SECRET_KEYS.iter().any(|s| k.to_ascii_lowercase().contains(s))
    {
        return Some(word.replacen(v, "<redacted>", 1));
    }
    let tokenish = |t: &str| t.chars().all(|c| c.is_ascii_alphanumeric() || "-_+/=.".contains(c));
    if w.len() >= 16 && tokenish(w) && TOKEN_PREFIXES.iter().any(|p| w.starts_with(p)) {
        return Some(word.replacen(w, "<redacted>", 1));
    }
    // A long run of letters and digits mixed: a key, not a word.
    let (digits, letters) = (
        w.chars().filter(char::is_ascii_digit).count(),
        w.chars().filter(char::is_ascii_alphabetic).count(),
    );
    if w.len() >= 32 && tokenish(w) && !w.contains('/') && digits >= 4 && letters >= 8 {
        return Some(word.replacen(w, "<redacted>", 1));
    }
    None
}

/// The summary from the command's output: Claude's JSON `result`, else its first line.
fn parse(out: &str) -> Option<String> {
    let text = match serde_json::from_str::<serde_json::Value>(out.trim()) {
        Ok(v) if v.get("is_error").and_then(serde_json::Value::as_bool) == Some(true) => {
            return None;
        }
        Ok(v) => v.get("result").and_then(|r| r.as_str()).map(str::to_owned)?,
        Err(_) => out.to_owned(),
    };
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let line = line.trim_matches(|c| c == '"' || c == '\'');
    Some(brief(line, 160)).filter(|l| !l.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_tokens_are_redacted() {
        let secrets = vec![("hunter2-value".to_owned(), "<secret:DB_PASS>".to_owned())];
        let text = "connect hunter2-value\n\
                    export GITHUB_TOKEN=abc123 and --password=pw\n\
                    key sk-ant-api03-aaaaaaaaaaaaaaaaaaaa here\n\
                    curl -H 'Authorization: Bearer eyJhbGciOi' x\n\
                    blob 9f8e7d6c5b4a39281706f5e4d3c2b1a0ZZZZYYYY end\n\
                    a normal sentence with src/main.rs and 1234 stays";
        let r = redact(text, &secrets);
        assert!(r.contains("connect <secret:DB_PASS>"), "{r}");
        assert!(
            r.contains("GITHUB_TOKEN=<redacted>") && r.contains("--password=<redacted>"),
            "{r}"
        );
        assert!(r.contains("key <redacted> here"), "{r}");
        assert!(r.contains("Bearer <redacted> x"), "{r}");
        assert!(r.contains("blob <redacted> end"), "{r}");
        assert!(r.contains("a normal sentence with src/main.rs and 1234 stays"), "{r}");
        for leaked in ["hunter2", "abc123", "sk-ant", "eyJhbG", "9f8e7d"] {
            assert!(!r.contains(leaked), "{leaked} in {r}");
        }
    }

    #[test]
    fn claude_json_and_plain_output_parse() {
        let json = r#"{"type":"result","is_error":false,"result":"Fixing the login test.\n"}"#;
        assert_eq!(parse(json).as_deref(), Some("Fixing the login test."));
        assert_eq!(parse(r#"{"is_error":true,"result":"Not logged in"}"#), None);
        assert_eq!(
            parse("\n  \"Waiting on a yes to run migrations\"\nmore").as_deref(),
            Some("Waiting on a yes to run migrations")
        );
        assert_eq!(parse("   \n"), None);
    }
}
