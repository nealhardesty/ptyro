//! Which machine a ptyro runs on, as a short name people tell apart at a glance: in the web
//! page's title, the TUI's sidebar and the terminal's title (SPEC §16.6). The daemon reports it,
//! so a client of a remote daemon shows the daemon's machine, not its own.

/// The longest name shown.
pub const MAX_LEN: usize = 24;

/// A raw host name as a short, plain, printable name: up to the first dot (`box.local`,
/// `box.example.com` are `box`; an IPv4-looking name stays whole), control characters dropped,
/// at most [`MAX_LEN`] characters. Empty if nothing printable is left.
pub fn short_name(raw: &str) -> String {
    let raw = raw.trim();
    let ip_like = raw.chars().any(|c| c.is_ascii_digit())
        && raw.chars().all(|c| c.is_ascii_digit() || c == '.');
    let head = if ip_like { raw } else { raw.split('.').next().unwrap_or("") };
    let clean: String = head.chars().map(blank).filter(|c| !c.is_control()).collect();
    let clean = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = clean.chars().take(MAX_LEN).collect();
    cut.trim_matches(|c: char| c == '-' || c == '_' || c.is_whitespace()).to_owned()
}

/// Tabs and line breaks are spaces; other controls are dropped by the callers.
fn blank(c: char) -> char {
    if c.is_whitespace() { ' ' } else { c }
}

/// A friendly macOS name (`Neal's MacBook`) is used only when it is plain: short, letters,
/// digits, spaces and a few marks.
fn plain(name: &str) -> bool {
    let n = name.trim();
    !n.is_empty()
        && n.chars().count() <= MAX_LEN
        && n.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '\'' | '\u{2019}'))
}

/// The name for this machine: `override_` (`[daemon] machine_name`) if set, else the friendly
/// name on macOS when it is plain, else the host name. `"computer"` when nothing is known.
pub fn name(override_: Option<&str>) -> String {
    if let Some(o) = override_.map(short_name_keep_dots).filter(|o| !o.is_empty()) {
        return o;
    }
    detected().to_owned()
}

/// What the owner typed is theirs: sanitised and capped, but not cut at a dot.
fn short_name_keep_dots(raw: &str) -> String {
    let clean: String = raw.chars().map(blank).filter(|c| !c.is_control()).collect();
    let clean: String = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    clean.chars().take(MAX_LEN).collect::<String>().trim().to_owned()
}

fn detected() -> &'static str {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let run = |prog: &str, args: &[&str]| {
            std::process::Command::new(prog)
                .args(args)
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        };
        let friendly = if cfg!(target_os = "macos") {
            run("scutil", &["--get", "ComputerName"]).filter(|n| plain(n))
        } else {
            None
        };
        friendly
            .map(|n| short_name_keep_dots(&n))
            .or_else(|| Some(short_name(&run("hostname", &[])?)).filter(|n| !n.is_empty()))
            .or_else(|| std::env::var("HOSTNAME").ok().map(|h| short_name(&h)))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "computer".into())
    })
}

/// A stable hue (0..360) for a machine name, so its tabs and sidebar get one colour that is the
/// same everywhere (the web computes the same hash). FNV-1a over the name's lowercase bytes.
pub fn hue(name: &str) -> u16 {
    let mut h: u32 = 0x811c_9dc5;
    for b in name.to_lowercase().bytes() {
        h ^= u32::from(b);
        h = h.wrapping_mul(0x0100_0193);
    }
    (h % 360) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_names_become_short_plain_names() {
        assert_eq!(short_name("box.example.com"), "box");
        assert_eq!(short_name("Neals-MacBook-Pro.local"), "Neals-MacBook-Pro");
        assert_eq!(short_name("devbox.lan\n"), "devbox");
        assert_eq!(short_name("10.0.0.5"), "10.0.0.5");
        assert_eq!(short_name(""), "");
        assert_eq!(short_name("  .  "), "");
        assert_eq!(short_name(&"a".repeat(60)).chars().count(), MAX_LEN);
        assert_eq!(short_name("büro-rechner.örtlich"), "büro-rechner");
        assert_eq!(short_name("ho\u{1b}[31mst"), "ho[31mst", "control characters are dropped");
        assert_eq!(short_name("日本語のコンピューター"), "日本語のコンピューター");
    }

    #[test]
    fn friendly_names_must_be_plain() {
        assert!(plain("Neal\u{2019}s MacBook"));
        assert!(!plain("Neal's MacBook Pro 16-inch (work) laptop"));
        assert!(!plain("a.b"));
        assert!(!plain(""));
    }

    #[test]
    fn the_configured_name_wins_and_is_cleaned() {
        assert_eq!(name(Some("studio.mac")), "studio.mac");
        assert_eq!(name(Some("  work\tlaptop \u{7}")), "work laptop");
        assert_eq!(name(Some(&"x".repeat(80))).chars().count(), MAX_LEN);
        assert_eq!(name(Some("   ")), name(None), "blank means auto");
        assert!(!name(None).is_empty());
    }

    #[test]
    fn the_hue_is_stable_and_ignores_case() {
        assert_eq!(hue("Studio"), hue("studio"));
        assert!(hue("studio") < 360);
        assert_ne!(hue("studio"), hue("laptop"));
    }
}
