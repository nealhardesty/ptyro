//! Globs for profile match rules: `*` and `?` stay within a path segment, `**` crosses them.

/// Match `text` against `pattern`.
pub fn matches(pattern: &str, text: &str) -> bool {
    // `dir/**` includes `dir` itself.
    if pattern.strip_suffix("/**").is_some_and(|dir| matches(dir, text)) {
        return true;
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

fn go(p: &[char], t: &[char]) -> bool {
    match p {
        [] => t.is_empty(),
        ['*', '*', '/', rest @ ..] => {
            // `**/` is zero or more whole directories: the rest starts at a segment boundary.
            (0..=t.len()).any(|i| (i == 0 || t[i - 1] == '/') && go(rest, &t[i..]))
        }
        ['*', '*', rest @ ..] => (0..=t.len()).any(|i| go(rest, &t[i..])),
        ['*', rest @ ..] => {
            for i in 0..=t.len() {
                if go(rest, &t[i..]) {
                    return true;
                }
                if t.get(i) == Some(&'/') {
                    return false;
                }
            }
            false
        }
        ['?', rest @ ..] => t.first().is_some_and(|c| *c != '/') && go(rest, &t[1..]),
        [c, rest @ ..] => t.first() == Some(c) && go(rest, &t[1..]),
    }
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
}
