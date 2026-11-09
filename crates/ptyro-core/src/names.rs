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
/// digits, not starting with `.`. Every name a person, an agent or a script gives is checked
/// here and refused when it's wrong, never rewritten (owner, 2026-11-09). The error offers the
/// fix when there is one: `invalid name "my feature": a space isn't allowed (only a-z 0-9 . _ -);
/// try "my-feature"`.
pub fn validate(name: &str) -> Result<(), String> {
    let Some(p) = problem(name) else { return Ok(()) };
    Err(match tidy(name) {
        Some(t) => format!("invalid name {name:?}: {p}; try {t:?}"),
        None => format!("invalid name {name:?}: {p}"),
    })
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
        return Some(format!("{what} isn't allowed (only a-z 0-9 . _ -)"));
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

/// What a name field says under itself, in the TUI and (the same words) the web.
pub const TYPED_HINT: &str = "A space or other odd character becomes - (a-z 0-9 . _ -)";

/// `raw` as it is typed into a name field: uppercase is lowercased, any other character a name
/// can't hold (a space, `/`, an accent) becomes `-`, as does the second `.` of `..`, and what a
/// name can't start with (`.`, `-`, a space) is dropped at the start. `-` is the one replacement
/// character everywhere: [`tidy`], [`derive`] and generated names use it too. Past the start one
/// character in is one out, so a cursor stays where it was. What no single character fixes (all
/// digits, `.lock` at the end, too long) is left for [`problem`] to explain.
pub fn typed(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars().map(|c| c.to_ascii_lowercase()) {
        if out.is_empty() && !(c.is_ascii_alphanumeric() || c == '_') {
            continue;
        }
        let bad = !allowed(c) || (c == '.' && out.ends_with('.'));
        out.push(if bad { '-' } else { c });
    }
    out
}

/// The same for a git branch name: it keeps case and `/`, swaps what git refuses in a ref
/// (whitespace, control characters, `~ ^ : ? * [ \`) for `-`, and drops that, or a `-`, at the
/// start.
pub fn branch_typed(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        let bad = c.is_whitespace() || c.is_control() || "~^:?*[\\".contains(c);
        if out.is_empty() && (bad || c == '-') {
            continue;
        }
        out.push(if bad { '-' } else { c });
    }
    out
}

/// `raw` as a name: itself when valid, else tidied (`Laptop 2` → `laptop-2`). For a name ptyro
/// derives (what a machine calls itself); a name someone gives is [`validate`]d instead.
pub fn as_name(raw: &str) -> Option<String> {
    if problem(raw).is_none() { Some(raw.to_owned()) } else { tidy(raw) }
}

/// The fix a refused name's error offers: `raw` turned into a valid name, when that changes it
/// and works (`My Feature/2` → `my-feature-2`).
pub fn tidy(raw: &str) -> Option<String> {
    let out = fold(raw, allowed, 64);
    (out != raw && problem(&out).is_none()).then_some(out)
}

/// A name made from text that isn't a name (a title, a directory, a recipe's template), always
/// valid: [`fold`]ed, or `None` when nothing valid is left. Tidying it silently is fine, since
/// it isn't anyone's input.
pub fn derive(raw: &str) -> Option<String> {
    let out = fold(raw, allowed, 64);
    problem(&out).is_none().then_some(out)
}

/// The one way ptyro turns text into a name: lowercased, each run of characters `keep` refuses
/// becomes one `-`, with no `.` or `-` at either end and at most `max` characters. [`tidy`] and
/// [`derive`] keep a name's characters; `model::slugify` keeps only `a-z0-9` (project slugs).
/// It may return `""`: there is no made-up stand-in.
pub fn fold(raw: &str, keep: impl Fn(char) -> bool, max: usize) -> String {
    let mut out = String::new();
    for c in raw.trim().chars().map(|c| c.to_ascii_lowercase()) {
        if keep(c) {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out: String = out.trim_start_matches(['.', '-']).chars().take(max).collect();
    out.trim_end_matches(['.', '-']).to_owned()
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
    fn typing_swaps_what_a_name_cant_hold_for_a_dash() {
        assert_eq!(typed("My Feature/2"), "my-feature-2");
        assert_eq!(typed("My Feature/2"), tidy("My Feature/2").unwrap(), "the same fix as tidy");
        assert_eq!(typed("-x"), "x", "a name can't start with -: dropped");
        assert_eq!(typed(".hidden"), "hidden");
        assert_eq!(typed(" x"), "x");
        assert_eq!(typed("_x"), "_x", "_ is a name's own character");
        assert_eq!(typed("x..y"), "x.-y");
        assert_eq!(typed("héllo"), "h-llo");
        assert_eq!(typed("my_fix"), "my_fix", "what a name can hold is kept as typed");
        assert_eq!(typed("brisk-rook.v1"), "brisk-rook.v1", "a valid name is left alone");
        assert_eq!(typed("12"), "12", "all digits is still the name's problem to explain");
        assert_eq!(branch_typed("fix this/Now: ok"), "fix-this/Now--ok");
        assert_eq!(branch_typed("-x"), "x");
        assert_eq!(branch_typed(" x"), "x");
        // A character at a time ends where all at once does.
        let mut one = String::new();
        for c in "  My Feature/2".chars() {
            one.push(c);
            one = typed(&one);
        }
        assert_eq!(one, typed("  My Feature/2"));
    }

    #[test]
    fn refusals_offer_the_fix() {
        let e = validate("My Feature").unwrap_err();
        assert!(e.starts_with("invalid name \"My Feature\": "), "{e}");
        assert!(e.ends_with("; try \"my-feature\""), "{e}");
        let e = validate("a b").unwrap_err();
        assert!(e.contains("a space isn't allowed") && e.ends_with("try \"a-b\""), "{e}");
        // Nothing to offer: no "try".
        assert!(!validate("!!!").unwrap_err().contains("try"));
        assert!(!validate("123").unwrap_err().contains("try"));
        assert!(validate("my_fix").is_ok(), "_ is allowed, and kept");
        // What it offers is valid, and typing the bad name gives a valid one too.
        for bad in ["My Feature", " .Hidden ", "a/b c", "x.", "Fix: the THING!", "-lead"] {
            let t = tidy(bad).unwrap();
            assert!(validate(&t).is_ok(), "{bad:?} → {t:?}");
            // `x.` is still flagged as it's typed: no one character fixes it.
            if bad != "x." {
                assert!(validate(&typed(bad)).is_ok(), "{bad:?} typed → {:?}", typed(bad));
            }
        }
    }

    #[test]
    fn derived_names_are_always_valid() {
        assert_eq!(derive("My Feature/2").as_deref(), Some("my-feature-2"));
        assert_eq!(derive("01abc-Review step").as_deref(), Some("01abc-review-step"));
        assert_eq!(derive("ok_name").as_deref(), Some("ok_name"));
        assert_eq!(derive("v1.").as_deref(), Some("v1"));
        assert_eq!(derive(""), None, "no made-up stand-in");
        assert_eq!(derive("!!!"), None);
        assert_eq!(derive("123"), None);
        let long = "y".repeat(99);
        for raw in ["", "...", "a..b", "x.lock", "Ünïcødé", long.as_str(), "--x--", "a .b"] {
            if let Some(n) = derive(raw) {
                assert!(validate(&n).is_ok(), "{raw:?} → {n:?}");
            }
        }
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
    fn the_web_says_what_the_tui_says() {
        let kit = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/src/kit.ts");
        let kit = std::fs::read_to_string(kit).unwrap();
        assert!(kit.contains(&format!("NAME_HINT = {TYPED_HINT:?}")), "web/src/kit.ts NAME_HINT");
    }

    #[test]
    fn exhausted_space_gets_suffix() {
        let n = friendly(3, |n| !n.ends_with("-2"));
        assert!(n.ends_with("-2"));
    }
}
