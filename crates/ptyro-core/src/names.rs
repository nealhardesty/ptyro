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
    None
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
    fn avoids_taken_names() {
        let first = friendly(7, |_| false);
        let second = friendly(7, |n| n == first);
        assert_ne!(first, second);
        assert!(validate(&first).is_ok());
    }

    #[test]
    fn dot_names_are_refused() {
        for bad in [".", "..", ".hidden", "", "123", "A"] {
            assert!(validate(bad).is_err(), "{bad:?}");
        }
        for good in ["a.b", "x..y", "v1.2", "brisk-rook"] {
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
