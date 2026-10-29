//! Friendly session names: `brisk-rook`, `quiet-jackdaw`, `sly-magpie` (SPEC §2.3).

const ADJECTIVES: &[&str] = &[
    "amber", "bold", "brisk", "bright", "calm", "clever", "crisp", "dapper", "deft", "eager",
    "fleet", "gentle", "glad", "hardy", "keen", "lively", "lucky", "merry", "mellow", "nimble",
    "noble", "plucky", "proud", "quick", "quiet", "rapid", "sharp", "shy", "sly", "snug", "spry",
    "steady", "stout", "sunny", "swift", "tidy", "vivid", "wary", "wise", "witty", "zesty",
];

const CORVIDS: &[&str] = &[
    "rook",
    "raven",
    "crow",
    "jackdaw",
    "magpie",
    "jay",
    "chough",
    "nutcracker",
    "treepie",
    "piapiac",
    "nutkin",
    "grackle",
    "currawong",
    "jaybird",
    "bluejay",
];

/// Pick a name from `seed` that isn't in `taken`; falls back to a numeric suffix.
pub fn friendly(seed: u128, taken: impl Fn(&str) -> bool) -> String {
    let a = ADJECTIVES.len() as u128;
    let c = CORVIDS.len() as u128;
    let total = a * c;
    let start = seed % total;
    for i in 0..total {
        let n = (start + i) % total;
        let name = format!("{}-{}", ADJECTIVES[(n / c) as usize], CORVIDS[(n % c) as usize]);
        if !taken(&name) {
            return name;
        }
    }
    let base = format!("{}-{}", ADJECTIVES[(start / c) as usize], CORVIDS[(start % c) as usize]);
    (2..).map(|k| format!("{base}-{k}")).find(|n| !taken(n)).expect("infinite")
}

/// `claude-rook`: `prefix` and a corvid, not `claude-2` (UX1.24). Falls back to
/// [`friendly`]'s two-word names, then numbers, when every corvid is taken.
pub fn corvid(seed: u128, prefix: &str, taken: impl Fn(&str) -> bool) -> String {
    let c = CORVIDS.len() as u128;
    for i in 0..c {
        let name = format!("{prefix}-{}", CORVIDS[((seed + i) % c) as usize]);
        if !taken(&name) {
            return name;
        }
    }
    let base = friendly(seed, |n| taken(&format!("{prefix}-{n}")));
    format!("{prefix}-{base}")
}

/// Words a name made from a sentence leaves out.
const FILLER: &[&str] = &[
    "a", "an", "the", "and", "or", "of", "in", "on", "at", "to", "for", "with", "from", "by",
    "into", "is", "are", "be", "it", "its", "this", "that", "these", "those", "my", "our", "your",
    "please", "can", "could", "would", "should", "you", "we", "i", "me", "so", "some", "all",
    "just", "also",
];

/// A name from free text (a prompt, a PR's title): its first three meaningful words, slugged.
/// `"Fix the flaky test in tests/api.rs"` → `fix-flaky-test`. `None` when nothing is left.
pub fn from_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty() && !FILLER.contains(w))
        .take(3)
        .collect();
    let mut name = words.join("-");
    if name.len() > 32 {
        name.truncate(32);
        name = name.trim_end_matches('-').to_owned();
    }
    validate(&name).is_ok().then_some(name)
}

/// A character a name may hold.
pub fn allowed(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c)
}

/// Names are used in addresses and paths, so keep them to `[a-z0-9._-]`, 1–64 chars, not all
/// digits, not starting with `.`.
pub fn validate(name: &str) -> Result<(), String> {
    problem(name).map_or(Ok(()), |p| Err(format!("invalid name {name:?}: {p}")))
}

/// Why `name` isn't a valid name, short enough to show while it's being typed: the first
/// character that's out, not just the rule.
pub fn problem(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("empty: use 1-64 of a-z 0-9 . _ -".into());
    }
    if let Some(c) = name.chars().find(|c| !allowed(*c)) {
        let what = match c {
            ' ' => "a space".to_owned(),
            c if c.is_ascii_uppercase() => format!("{c:?} (uppercase)"),
            c => format!("{c:?}"),
        };
        return Some(format!("{what} isn't allowed: use a-z 0-9 . _ -"));
    }
    if name.len() > 64 {
        return Some(format!("too long: {} of at most 64", name.len()));
    }
    if name.chars().all(|c| c.is_ascii_digit()) {
        return Some("all digits: it would look like a handle".into());
    }
    // Names become path components: `.`, `..` and hidden names are out.
    if name.starts_with('.') {
        return Some("must not start with .".into());
    }
    // A name becomes part of a branch (`ptyro/<name>`) and is given to git as a word: what git
    // refuses in a ref, or reads as an option, is refused here, not late inside `git worktree add`.
    if name.starts_with('-') {
        return Some("must not start with -".into());
    }
    if name.contains("..") {
        return Some("must not contain .. (git refuses it in a branch name)".into());
    }
    if name.ends_with('.') || name.ends_with(".lock") {
        return Some("must not end with . or .lock (git refuses it in a branch name)".into());
    }
    None
}

/// `raw` as it is typed into a name field: uppercase is lowercased, and any other character a
/// name can't hold (a space, `/`, an accent) becomes `_`, as does a leading `.` or `-` and the
/// second `.` of `..`. One character in, one out, so a cursor stays where it was. What no single
/// character fixes (all digits, `.lock` at the end, too long) is left for [`problem`] to explain.
pub fn typed(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars().map(|c| c.to_ascii_lowercase()) {
        let bad = !allowed(c)
            || (out.is_empty() && matches!(c, '.' | '-'))
            || (c == '.' && out.ends_with('.'));
        out.push(if bad { '_' } else { c });
    }
    out
}

/// The same for a git branch name: it keeps case and `/`, and swaps what git refuses in a ref
/// (whitespace, control characters, `~ ^ : ? * [ \`) and a leading `-` for `_`.
pub fn branch_typed(raw: &str) -> String {
    raw.chars()
        .enumerate()
        .map(|(i, c)| {
            let bad = c.is_whitespace()
                || c.is_control()
                || "~^:?*[\\".contains(c)
                || (i == 0 && c == '-');
            if bad { '_' } else { c }
        })
        .collect()
}

/// `raw` as a name: itself when valid, else tidied (`Laptop 2` → `laptop-2`).
pub fn as_name(raw: &str) -> Option<String> {
    if problem(raw).is_none() { Some(raw.to_owned()) } else { tidy(raw) }
}

/// `raw` turned into a valid name (lowercased, anything else folded into `-`), when that
/// changes it and works: `My Feature/2` → `my-feature-2`.
pub fn tidy(raw: &str) -> Option<String> {
    let mut out = String::new();
    for c in raw.trim().chars().map(|c| c.to_ascii_lowercase()) {
        if allowed(c) {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_start_matches(['.', '-']).chars().take(64).collect();
    let out = out.trim_end_matches('-');
    (out != raw && problem(out).is_none()).then(|| out.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_what_its_for() {
        assert_eq!(
            from_text("Fix the flaky test in tests/api.rs").as_deref(),
            Some("fix-flaky-test")
        );
        assert_eq!(from_text("please add --json to `ls`").as_deref(), Some("add-json-ls"));
        assert_eq!(from_text("#12 Users get a tier: free or pro").as_deref(), Some("12-users-get"));
        assert_eq!(from_text("the a an"), None, "nothing meaningful left");
        assert_eq!(from_text("12345"), None, "all digits isn't a name");
        assert_eq!(from_text("日本語のテスト"), None);
        let long = from_text("supercalifragilisticexpialidocious antidisestablishment").unwrap();
        assert!(long.len() <= 32 && !long.ends_with('-'), "{long}");
        // An agent and a corvid, never `claude-2`.
        let taken = ["claude-rook".to_owned()];
        let n = corvid(0, "claude", |n| taken.iter().any(|t| t == n));
        assert!(n.starts_with("claude-") && n != "claude-rook", "{n}");
        let all = |n: &str| n.split('-').count() == 2;
        assert!(
            corvid(3, "claude", all).split('-').count() > 2,
            "every corvid taken: a longer name"
        );
    }

    #[test]
    fn avoids_taken_names() {
        let first = friendly(7, |_| false);
        let second = friendly(7, |n| n == first);
        assert_ne!(first, second);
        assert!(validate(&first).is_ok());
    }

    #[test]
    fn typing_swaps_what_a_name_cant_hold_for_an_underscore() {
        assert_eq!(typed("My Feature/2"), "my_feature_2");
        assert_eq!(typed("-x"), "_x");
        assert_eq!(typed(".hidden"), "_hidden");
        assert_eq!(typed("x..y"), "x._y");
        assert_eq!(typed("héllo"), "h_llo");
        assert_eq!(typed("brisk-rook.v1"), "brisk-rook.v1", "a valid name is left alone");
        assert_eq!(typed("12"), "12", "all digits is still the name's problem to explain");
        assert_eq!(branch_typed("fix this/Now: ok"), "fix_this/Now__ok");
        assert_eq!(branch_typed("-x"), "_x");
    }

    #[test]
    fn dot_names_are_refused() {
        for bad in [".", "..", ".hidden", "", "123", "A", "-x", "x..y", "a.", "x.lock", "-"] {
            assert!(validate(bad).is_err(), "{bad:?}");
        }
        for good in ["a.b", "v1.2", "brisk-rook", "x.locked", "a-b"] {
            assert!(validate(good).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn problems_name_the_character() {
        assert!(problem("Brisk").unwrap().contains("'B' (uppercase)"));
        assert!(problem("a b").unwrap().contains("a space"));
        assert!(problem("a/b").unwrap().contains("'/'"));
        assert!(problem(&"a".repeat(65)).unwrap().contains("65"));
        assert_eq!(problem("brisk-rook"), None);
    }

    #[test]
    fn tidy_suggests_a_valid_name() {
        assert_eq!(tidy("My Feature/2").as_deref(), Some("my-feature-2"));
        assert_eq!(tidy(" .Hidden ").as_deref(), Some("hidden"));
        assert_eq!(tidy("ok-already"), None, "nothing to change");
        assert_eq!(tidy("123"), None, "still all digits");
        assert_eq!(tidy("!!!"), None, "nothing left");
        assert_eq!(tidy(&"x".repeat(80)).map(|s| s.len()), Some(64));
    }

    #[test]
    fn exhausted_space_gets_suffix() {
        let n = friendly(3, |n| !n.ends_with("-2"));
        assert!(n.ends_with("-2"));
    }
}
