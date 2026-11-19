//! Redaction (SPEC §25.7): secrets out of text before it is stored in the knowledge base, or
//! sent to the small model that titles a chat.
//!
//! What is replaced:
//! - the **exact values of the profile's secrets**, by `<secret:NAME>`;
//! - a **PEM block**, whole (`-----BEGIN …-----` to its `-----END …-----`);
//! - **known token shapes**: `sk-…`, `ghp_…`, `github_pat_…`, `glpat-…`, `xoxb-…`, `AKIA…`,
//!   `AIza…` and their kin, and a JWT;
//! - the word after **`Bearer`**;
//! - the **value of a key that names a secret** (`password`, `passwd`, `secret`, `token`,
//!   `api_key`, `access_key`, `private_key`), written `key=value`, `key: value` or
//!   `"key": "value"`;
//! - the **password in a URL** (`https://user:…@host`);
//! - a **long run of mixed letters and digits** (32 or more): a key, not a word;
//! - the user's own patterns (`kb.redact`).
//!
//! It is a net, not a proof (§25.7): a secret in no known shape gets through. It is also not a
//! shredder: the knowledge base is read by people, so what merely looks a little like a secret
//! stays (a git SHA, a UUID, `token = get_token()`, `max_tokens=4096`, "the bearer of news").
//! The titler's text leaves the machine, so [`Redactor::strict`] keeps none of those doubts.
//!
//! Redacting what was already redacted changes nothing, and counts nothing.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::{Regex, RegexBuilder};

/// What stands where a credential stood.
pub const MARK: &str = "<redacted>";

/// A source with more hits than this is not stored at all: it is probably a keystore or a
/// fixture of credentials (§25.7).
pub const MAX_HITS: usize = 25;

/// Text with its secrets replaced, and how many were.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Redacted {
    pub text: String,
    pub hits: usize,
}

/// The rules, with one profile's secrets and the user's patterns. Made once and used for many
/// texts: the built-in rules are compiled once a process, the user's patterns once here.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    /// (the real value, what to show instead).
    secrets: Vec<(String, String)>,
    own: Vec<Regex>,
    strict: bool,
}

impl Redactor {
    /// For the knowledge base. `secrets`: the profile's secret values with what to show
    /// instead (`<secret:NAME>`); a value shorter than four characters is not looked for.
    /// `patterns`: `kb.redact`, regexes. The second result names each pattern that isn't a
    /// regex (it is left out; `Config` refuses such a file before it gets here).
    pub fn new(secrets: Vec<(String, String)>, patterns: &[String]) -> (Self, Vec<String>) {
        let mut errors = Vec::new();
        let own = patterns
            .iter()
            .filter(|p| !p.trim().is_empty())
            .filter_map(|p| match pattern(p) {
                Ok(r) => Some(r),
                Err(e) => {
                    errors.push(e);
                    None
                }
            })
            .collect();
        (Self { secrets: usable(secrets), own, strict: false }, errors)
    }

    /// For text that leaves the machine (a chat's title prompt): every doubt is redacted, so
    /// also any word after `Bearer`, any value of a key with a secret's name anywhere in it,
    /// and any long mixed run (a git SHA too).
    pub fn strict(secrets: Vec<(String, String)>) -> Self {
        Self { secrets: usable(secrets), own: Vec::new(), strict: true }
    }

    /// The same rules for another profile's secrets (the patterns are not compiled again).
    pub fn with_secrets(&self, secrets: Vec<(String, String)>) -> Self {
        Self { secrets: usable(secrets), own: self.own.clone(), strict: self.strict }
    }

    /// `text` with every secret replaced.
    pub fn redact(&self, text: &str) -> Redacted {
        let mut hits = 0;
        let mut text = Cow::Borrowed(text);
        for (real, shown) in &self.secrets {
            let n = text.matches(real.as_str()).count();
            if n > 0 {
                hits += n;
                text = Cow::Owned(text.replace(real.as_str(), shown));
            }
        }
        if let Some(t) = pem(&text, &mut hits) {
            text = Cow::Owned(t);
        }
        if let Some(t) = shapes(&text, self.strict, &mut hits) {
            text = Cow::Owned(t);
        }
        for re in &self.own {
            let n = re.find_iter(&text).filter(|m| m.as_str() != MARK).count();
            if n > 0 {
                hits += n;
                text = Cow::Owned(re.replace_all(&text, MARK).into_owned());
            }
        }
        Redacted { text: text.into_owned(), hits }
    }
}

/// `text` as a chat's titler may see it: [`Redactor::strict`], for one call.
pub fn for_model(text: &str, secrets: &[(String, String)]) -> String {
    Redactor::strict(secrets.to_vec()).redact(text).text
}

/// One of the user's patterns (`kb.redact`), compiled; the error says which and why.
pub fn pattern(p: &str) -> Result<Regex, String> {
    RegexBuilder::new(p)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| format!("{p:?} is not a pattern ptyro can use: {}", brief(&e.to_string())))
}

/// The last line of a regex error (the rest draws the pattern again).
fn brief(e: &str) -> &str {
    e.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or(e).trim().trim_start_matches("error: ")
}

fn usable(secrets: Vec<(String, String)>) -> Vec<(String, String)> {
    secrets.into_iter().filter(|(real, shown)| real.len() >= 4 && real != shown).collect()
}

// ── PEM blocks ──────────────────────────────────────────────────────────────────────────────

/// Every PEM block replaced whole, or `None` when there is none. One that is cut off (no END
/// line) loses its header and the lines of base64 under it.
fn pem(text: &str, hits: &mut usize) -> Option<String> {
    const BEGIN: &str = "-----BEGIN ";
    const END: &str = "-----END ";
    let first = text.find(BEGIN)?;
    let base64 = |b: u8| b.is_ascii_alphanumeric() || b"+/=\\".contains(&b);
    let mut out = String::with_capacity(text.len());
    let (mut at, mut next, mut ends) = (0, Some(first), true);
    while let Some(start) = next {
        // To the end of the END marker's own dashes; an END with none is cut off there.
        let end = if ends { text[start..].find(END).map(|e| start + e + END.len()) } else { None };
        let stop = match end {
            Some(e) => {
                let line = text[e..].find('\n').map_or(text.len(), |n| e + n);
                text[e..line].find("-----").map_or(line, |d| e + d + 5)
            }
            None => {
                // No END from here on, so none for any later BEGIN either.
                ends = false;
                let head = start + BEGIN.len();
                let line = text[head..].find('\n').map_or(text.len(), |n| head + n);
                let mut stop = text[head..line].find("-----").map_or(line, |d| head + d + 5);
                loop {
                    stop += text.as_bytes()[stop..].iter().take_while(|b| base64(**b)).count();
                    let rest = &text[stop..];
                    let nl = match rest {
                        r if r.starts_with("\r\n") => 2,
                        r if r.starts_with('\n') => 1,
                        _ => break,
                    };
                    let line = rest[nl..].split(['\r', '\n']).next().unwrap_or("");
                    if line.is_empty() || !line.bytes().all(base64) {
                        break;
                    }
                    stop += nl;
                }
                stop
            }
        };
        out.push_str(&text[at..start]);
        out.push_str(MARK);
        *hits += 1;
        at = stop;
        next = text[at..].find(BEGIN).map(|i| at + i);
    }
    out.push_str(&text[at..]);
    Some(out)
}

// ── Shapes ──────────────────────────────────────────────────────────────────────────────────

/// Every shape in one expression, so the text is walked once. The arm that matched is told by
/// its group; `decide` then says what of the match goes, if anything.
fn rules(strict: bool) -> Option<Regex> {
    // A key that names a secret. For the knowledge base it must *end* in the word, so that
    // `max_tokens`, `token_count` and `secret_santa` are not keys; strictly, anywhere will do.
    let key = if strict {
        r"[A-Za-z0-9_.-]*(?i:password|passwd|secret|token|api_key|apikey)[A-Za-z0-9_.-]*"
    } else {
        r"[A-Za-z0-9_.-]*(?i:password|passwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key)"
    };
    let run = if strict { r"[A-Za-z0-9_+=.-]{32,}" } else { r"[A-Za-z0-9_+-]{32,}" };
    let all = [
        r"(?P<jwt>(?-u:\b)eyJ[A-Za-z0-9_-]{8,}\.eyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]*)".to_owned(),
        r"(?P<pre>(?-u:\b)(?:sk-|sk_live_|rk_live_|ghp_|gho_|ghs_|ghu_|ghr_|github_pat_|glpat-|xox[abposr]-|AKIA|ASIA|AIza)[A-Za-z0-9_+/=.-]{8,})".to_owned(),
        r#"(?-u:\b)(?i:bearer)[ \t]+(?P<bearer>[^\s"'`,;)]+)"#.to_owned(),
        r"[A-Za-z][A-Za-z0-9+.-]*://[^\s:/@]+:(?P<urlpw>[^\s@/]+)@".to_owned(),
        format!(
            r#"(?P<key>{key})["'`]?(?P<sep>[ \t]*[=:][ \t]*)(?P<val>"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|[^\s"'`,;&)\]}}]+)"#
        ),
        format!("(?P<run>{run})"),
    ];
    // (Built from constants: a test holds that both compile.)
    Regex::new(&all.join("|")).ok()
}

static LOOSE: LazyLock<Option<Regex>> = LazyLock::new(|| rules(false));
static STRICT: LazyLock<Option<Regex>> = LazyLock::new(|| rules(true));

/// What to do with a match: replace `cut` (byte offsets in the text), or nothing; and where to
/// look next (a value that is left alone is still looked at by the other rules).
struct Verdict {
    cut: Option<(usize, usize)>,
    next: usize,
}

fn shapes(text: &str, strict: bool, hits: &mut usize) -> Option<String> {
    let re = if strict { STRICT.as_ref() } else { LOOSE.as_ref() }?;
    let mut out: Option<String> = None;
    let (mut at, mut copied) = (0, 0);
    while at <= text.len()
        && let Some(caps) = re.captures_at(text, at)
    {
        let v = decide(text, &caps, strict);
        if let Some((from, to)) = v.cut {
            let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
            out.push_str(&text[copied..from]);
            out.push_str(MARK);
            copied = to;
            *hits += 1;
        }
        // (Every arm matches at least a character, so this moves on.)
        at = v.next.max(at + 1);
        while !text.is_char_boundary(at) {
            at += 1;
        }
    }
    out.map(|mut o| {
        o.push_str(&text[copied..]);
        o
    })
}

/// Something already redacted, or that stands for a value: `<redacted>`, `<secret:NAME>`,
/// `<your token>`, `$TOKEN`, `${{ secrets.X }}`, `{token}`, `%s`.
fn placeholder(v: &str) -> bool {
    v.starts_with(['<', '$', '{', '%'])
}

fn decide(text: &str, caps: &regex::Captures<'_>, strict: bool) -> Verdict {
    let whole = caps.get(0).map_or((0, 0), |m| (m.start(), m.end()));
    let keep = |next: usize| Verdict { cut: None, next };
    let cut = |from: usize, to: usize| Verdict { cut: Some((from, to)), next: to };
    if let Some(m) = caps.name("jwt") {
        return cut(m.start(), m.end());
    }
    if let Some(m) = caps.name("pre") {
        // (A sentence's full stop is not the token's.)
        let token = m.as_str().trim_end_matches('.');
        let digit = token.bytes().any(|b| b.is_ascii_digit());
        if token.len() < 16 || !(strict || digit) {
            return keep(m.end());
        }
        return cut(m.start(), m.start() + token.len());
    }
    if let Some(m) = caps.name("bearer") {
        let w = m.as_str();
        if w.starts_with("<secret:") || w == MARK {
            return keep(m.end());
        }
        // "The bearer of bad news": an ordinary word is no token. One with a digit or a
        // capital in it, or a long one, is.
        let tokenish =
            w.bytes().any(|b| b.is_ascii_digit() || b.is_ascii_uppercase()) || w.len() >= 20;
        if strict || (tokenish && !placeholder(w)) {
            return cut(m.start(), m.end());
        }
        return keep(m.start());
    }
    if let Some(m) = caps.name("urlpw") {
        if placeholder(m.as_str()) {
            return keep(m.end());
        }
        return cut(m.start(), m.end());
    }
    if let (Some(key), Some(sep), Some(val)) =
        (caps.name("key"), caps.name("sep"), caps.name("val"))
    {
        // `<secret:NAME>` is what a secret was replaced by, not a key and its value.
        if key.as_str() == "secret" && text[..key.start()].ends_with('<') {
            return keep(whole.1);
        }
        return match value(key.as_str(), sep.as_str(), val.as_str(), strict) {
            Some((from, to)) => cut(val.start() + from, val.start() + to),
            None => keep(val.start()),
        };
    }
    if let Some(m) = caps.name("run") {
        let before = text.as_bytes()[..m.start()].last().copied();
        let after = text.as_bytes().get(m.end()).copied();
        // Part of a path or a URL is a name, not a key.
        if before == Some(b'/')
            || after == Some(b'/')
            || !long_run(text, m.start(), m.as_str(), strict)
        {
            return keep(m.end());
        }
        return cut(m.start(), m.end());
    }
    keep(whole.1)
}

/// The part of a secret key's value to replace (offsets in `val`), or `None` to leave it.
fn value(key: &str, sep: &str, val: &str, strict: bool) -> Option<(usize, usize)> {
    let quoted = val.len() >= 2 && val.starts_with(['"', '\'']);
    let (from, to) = if quoted { (1, val.len() - 1) } else { (0, val.len()) };
    let inner = &val[from..to];
    if inner.is_empty() || inner.starts_with("<secret:") || inner == MARK {
        return None;
    }
    if strict {
        return Some((from, to));
    }
    if placeholder(inner) {
        return None;
    }
    if quoted {
        return Some((from, to));
    }
    // Bare: code, a literal and a type are not values (`token = get_token()`, `secret=None`,
    // `password: String`).
    if inner.contains(['(', ')', '[', ']', '{', '}', '<', '>', '$', '%', '*', '|', '\\']) {
        return None;
    }
    let lower = inner.to_ascii_lowercase();
    if matches!(lower.as_str(), "true" | "false" | "null" | "none" | "nil" | "undefined") {
        return None;
    }
    let digit = inner.bytes().any(|b| b.is_ascii_digit());
    let letter = inner.bytes().any(|b| b.is_ascii_alphabetic());
    if sep == "=" {
        // `key=value` with no spaces: a shell's, a flag's, a query's. Not a variable handed on
        // under its own name (`token=token`, `password=self.password`).
        let name = key.rsplit(['.', '-']).next().unwrap_or(key).to_ascii_lowercase();
        let path = !digit && inner.contains('.') && !inner.starts_with('.');
        if path || lower == name || name.ends_with(&format!("_{lower}")) {
            return None;
        }
        return Some((from, to));
    }
    // `key: value`, `key = value`: prose, YAML, a type, an assignment. Only what looks like a
    // secret goes.
    ((inner.len() >= 6 && digit && letter) || inner.len() >= 20).then_some((from, to))
}

/// Whether a run of 32 or more is a key: enough digits and letters, and (for the knowledge
/// base) not a hash, a UUID, inline base64 data or a long name made of words.
fn long_run(text: &str, start: usize, run: &str, strict: bool) -> bool {
    let digits = run.bytes().filter(u8::is_ascii_digit).count();
    let letters = run.bytes().filter(u8::is_ascii_alphabetic).count();
    if digits < 4 || letters < 8 {
        return false;
    }
    if strict {
        return true;
    }
    if run.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return false; // a git SHA, a checksum, a UUID
    }
    if text[..start].ends_with("base64,") {
        return false; // an inline image
    }
    // `test_parse_utf8_and_sha256_of_2024_files` is words; a key has a long mixed part.
    run.split(['_', '-', '+']).any(|part| {
        part.len() >= 16
            && part.bytes().any(|b| b.is_ascii_digit())
            && part.bytes().any(|b| b.is_ascii_alphabetic())
            && !part.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

#[cfg(test)]
mod tests;
