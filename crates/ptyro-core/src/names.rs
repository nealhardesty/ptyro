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

/// Names are used in addresses and paths, so keep them to `[a-z0-9._-]`, 1–64 chars, not all
/// digits, not starting with `.`.
pub fn validate(name: &str) -> Result<(), String> {
    let ok_chars =
        name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c));
    if name.is_empty() || name.len() > 64 || !ok_chars {
        return Err(format!("invalid name {name:?}: use 1-64 of a-z 0-9 . _ -"));
    }
    if name.chars().all(|c| c.is_ascii_digit()) || name.starts_with('@') {
        return Err(format!("invalid name {name:?}: must not look like a handle"));
    }
    // Names become path components: `.`, `..` and hidden names are out.
    if name.starts_with('.') {
        return Err(format!("invalid name {name:?}: must not start with ."));
    }
    Ok(())
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
    fn exhausted_space_gets_suffix() {
        let n = friendly(3, |n| !n.ends_with("-2"));
        assert!(n.ends_with("-2"));
    }
}
