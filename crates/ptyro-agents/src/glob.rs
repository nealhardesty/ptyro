//! Globs for profile match rules: `*` and `?` stay within a path segment, `**` crosses them.

/// Match `text` against `pattern`.
pub fn matches(pattern: &str, text: &str) -> bool {
    // `dir/**` includes `dir` itself.
    if pattern.strip_suffix("/**").is_some_and(|dir| matches(dir, text)) {
        return true;
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    // Each (pattern position, text position) is decided once, so a pattern with several `*` or
    // `**` against a long path costs its product, not an exponential.
    go(&p, 0, &t, 0, &mut std::collections::HashMap::new())
}

type Memo = std::collections::HashMap<(usize, usize), bool>;

fn go(p: &[char], pi: usize, t: &[char], ti: usize, memo: &mut Memo) -> bool {
    if let Some(&known) = memo.get(&(pi, ti)) {
        return known;
    }
    let rest_t = &t[ti.min(t.len())..];
    let hit = match &p[pi.min(p.len())..] {
        [] => rest_t.is_empty(),
        ['*', '*', '/', ..] => {
            // `**/` is zero or more whole directories: the rest starts at a segment boundary.
            (ti..=t.len()).any(|i| (i == 0 || t[i - 1] == '/') && go(p, pi + 3, t, i, memo))
        }
        ['*', '*', ..] => (ti..=t.len()).any(|i| go(p, pi + 2, t, i, memo)),
        ['*', ..] => {
            let mut found = false;
            for i in ti..=t.len() {
                if go(p, pi + 1, t, i, memo) {
                    found = true;
                    break;
                }
                if t.get(i) == Some(&'/') {
                    break;
                }
            }
            found
        }
        ['?', ..] => rest_t.first().is_some_and(|c| *c != '/') && go(p, pi + 1, t, ti + 1, memo),
        [c, ..] => rest_t.first() == Some(c) && go(p, pi + 1, t, ti + 1, memo),
    };
    memo.insert((pi, ti), hit);
    hit
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn segments() {
        assert!(matches("github.com/acme/*", "github.com/acme/repo"));
        assert!(!matches("github.com/acme/*", "github.com/acme/repo/sub"));
        assert!(matches("/home/u/work/**", "/home/u/work/a/b"));
        assert!(matches("/home/u/work/**", "/home/u/work"));
        assert!(matches("**/acme/*", "gitlab.com/group/acme/x"));
        assert!(!matches("/home/u/work/**", "/home/u/workshop"));
        assert!(matches("a?c", "abc") && !matches("a?c", "a/c"));
    }

    #[test]
    fn double_star_slash_is_whole_directories() {
        assert!(matches("**/acme/*", "acme/x"));
        assert!(matches("**/acme/*", "github.com/acme/x"));
        assert!(!matches("**/acme/*", "github.com/notacme/x"));
        assert!(matches("a/**/b", "a/b"));
        assert!(matches("a/**/b", "a/x/y/b"));
        assert!(!matches("a/**/b", "a/xb"));
        assert!(!matches("a/**/b", "a/x/yb"));
        assert!(matches("a/**", "a/x/y") && matches("a**", "abc/d"));
    }

    #[test]
    fn many_wildcards_against_a_long_path_stay_fast() {
        let text = "a/".repeat(2000) + "z";
        let pat = "**/*/**/*/**/*/**/*/**/*/**/*/nope";
        let start = std::time::Instant::now();
        assert!(!matches(pat, &text));
        assert!(start.elapsed() < std::time::Duration::from_secs(5), "{:?}", start.elapsed());
        assert!(matches("**/*/z", &text));
    }
}
