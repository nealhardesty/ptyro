//! Keeping secrets out of what is sent to a small model (a chat's title, an imported chat's
//! lines): the profile's secret values and anything that looks like a credential are replaced
//! before the text leaves.

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
