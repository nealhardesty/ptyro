//! Keeping secrets out of what is sent to a small model (a chat's title): the profile's secret
//! values and anything that looks like a credential are replaced before the text leaves. The
//! rules are the knowledge base's (`ptyro_core::kb::redact`, SPEC §25.7).

use ptyro_core::session::brief;

/// The profile's secret values, with what to show instead (`<secret:NAME>`).
pub(crate) fn secrets(plan: &ptyro_agents::profile::Plan) -> Vec<(String, String)> {
    plan.set
        .iter()
        .zip(&plan.shown)
        .filter(|((_, real), (_, shown))| real != shown && real.len() >= 4)
        .map(|((_, real), (_, shown))| (real.clone(), shown.clone()))
        .collect()
}

/// `text` with `secrets` and anything that looks like a credential replaced: the knowledge
/// base's rules at their strictest (`ptyro_core::kb::redact`), since this text leaves the
/// machine.
pub(crate) fn redact(text: &str, secrets: &[(String, String)]) -> String {
    ptyro_core::kb::redact::for_model(text, secrets)
}

/// A one-line answer from a small model's output: Claude's JSON `result`, else its first line.
pub(crate) fn parse(out: &str) -> Option<String> {
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

    // (The rules' own tests are `ptyro_core::kb::redact`'s; this is that titles use them.)
    #[test]
    fn a_title_prompt_goes_through_the_strict_rules() {
        let secrets = vec![("hunter2-value".to_owned(), "<secret:DB_PASS>".to_owned())];
        let r = redact("connect hunter2-value with GITHUB_TOKEN=abc123 as bearer xyz", &secrets);
        assert_eq!(r, "connect <secret:DB_PASS> with GITHUB_TOKEN=<redacted> as bearer <redacted>");
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
