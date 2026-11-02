//! Globs for profile match rules: `*` and `?` stay within a path segment, `**` crosses them.

/// Match `text` against `pattern`.
pub fn matches(pattern: &str, text: &str) -> bool {
    // `dir/**` includes `dir` itself.
    if pattern.strip_suffix("/**").is_some_and(|dir| matches(dir, text)) {
        return true;
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (n, m) = (p.len(), t.len());
    // `ok[i * w + j]`: does `p[i..]` match `t[j..]`? Filled from the ends backwards, each cell from
    // cells already decided, so a pattern with several `*` or `**` against a long path costs
    // their product, not an exponential, and no wildcard rescans the text.
    let w = m + 1;
    let mut ok = vec![false; (n + 1) * w];
    ok[n * w + m] = true;
    for i in (0..n).rev() {
        for j in (0..=m).rev() {
            let next = |di: usize, dj: usize| ok[(i + di) * w + j + dj];
            let more = j < m;
            ok[i * w + j] = match (p[i], p.get(i + 1), p.get(i + 2)) {
                // `**/` is zero or more whole directories: the rest starts at a segment boundary.
                ('*', Some('*'), Some('/')) => {
                    ((j == 0 || t[j - 1] == '/') && next(3, 0)) || (more && next(0, 1))
                }
                ('*', Some('*'), _) => next(2, 0) || (more && next(0, 1)),
                // `*` stays within the segment.
                ('*', ..) => next(1, 0) || (more && t[j] != '/' && next(0, 1)),
                ('?', ..) => more && t[j] != '/' && next(1, 1),
                (c, ..) => more && t[j] == c && next(1, 1),
            };
        }
    }
    ok[0]
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
