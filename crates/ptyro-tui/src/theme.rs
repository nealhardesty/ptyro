//! Chrome colours (SPEC §11.4, M2.14): three built-in themes, `NO_COLOR`, and downsampling of every colour (chrome *and* session content) to what the outer
//! terminal can show.

use ratatui::style::Color;

pub const BUILTIN: &[&str] = &["ptyro-dark", "ptyro-light", "high-contrast"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    pub accent: Color,
    pub text: Color,
    pub dim: Color,
    pub border: Color,
    pub border_focus: Color,
    pub bar_fg: Color,
    pub bar_bg: Color,
    pub selection_fg: Color,
    pub selection_bg: Color,
    pub search: Color,
    pub working: Color,
    pub blocked: Color,
    pub idle: Color,
    pub done: Color,
    pub error: Color,
    pub exited: Color,
    /// `NO_COLOR`: draw emphasis with attributes only.
    pub monochrome: bool,
}

impl Theme {
    pub fn dark() -> Self {
        let i = Color::Indexed;
        Self {
            accent: i(75),
            text: i(252),
            dim: i(244),
            border: i(238),
            border_focus: i(75),
            bar_fg: i(252),
            bar_bg: i(236),
            selection_fg: i(255),
            selection_bg: i(25),
            search: i(220),
            working: i(214),
            blocked: i(203),
            idle: i(110),
            done: i(114),
            error: i(196),
            exited: i(242),
            monochrome: false,
        }
    }

    pub fn light() -> Self {
        let i = Color::Indexed;
        Self {
            accent: i(25),
            text: i(236),
            dim: i(244),
            border: i(250),
            border_focus: i(25),
            bar_fg: i(236),
            bar_bg: i(254),
            selection_fg: i(16),
            selection_bg: i(153),
            search: i(220),
            working: i(130),
            blocked: i(160),
            idle: i(31),
            done: i(28),
            error: i(160),
            exited: i(246),
            monochrome: false,
        }
    }

    pub fn high_contrast() -> Self {
        let i = Color::Indexed;
        Self {
            accent: i(14),
            text: i(15),
            dim: i(7),
            border: i(15),
            border_focus: i(14),
            bar_fg: i(15),
            bar_bg: i(0),
            selection_fg: i(0),
            selection_bg: i(15),
            search: i(11),
            working: i(11),
            blocked: i(9),
            idle: i(12),
            done: i(10),
            error: i(9),
            exited: i(7),
            monochrome: false,
        }
    }

    pub fn monochrome() -> Self {
        let r = Color::Reset;
        Self {
            accent: r,
            text: r,
            dim: r,
            border: r,
            border_focus: r,
            bar_fg: r,
            bar_bg: r,
            selection_fg: r,
            selection_bg: r,
            search: r,
            working: r,
            blocked: r,
            idle: r,
            done: r,
            error: r,
            exited: r,
            monochrome: true,
        }
    }

    fn builtin(name: &str) -> Option<Self> {
        match name {
            "ptyro-dark" => Some(Self::dark()),
            "ptyro-light" => Some(Self::light()),
            "high-contrast" => Some(Self::high_contrast()),
            _ => None,
        }
    }

    /// The theme named `name`; one ptyro doesn't have (a theme of your own, which were removed
    /// on 2026-11-17, or a typo) is the default, and `ptyro doctor` offers the fix.
    pub fn resolve(name: &str, no_color: bool) -> Self {
        if no_color {
            return Self::monochrome();
        }
        Self::builtin(name).unwrap_or_else(Self::dark)
    }
}

/// One of the built-in themes.
pub fn exists(name: &str) -> bool {
    BUILTIN.contains(&name)
}

/// Why `name` isn't a theme, with the fix (`ptyro doctor` applies it).
pub fn unknown(name: &str) -> String {
    format!(
        "unknown theme {name:?}: ptyro has {} (themes of your own were removed on 2026-11-17); `ptyro config set ui.theme ptyro-dark`",
        BUILTIN.join(", ")
    )
}

/// How many colours the outer terminal shows (SPEC §11.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Depth {
    True,
    C256,
    C16,
}

impl Depth {
    pub fn detect(env: impl Fn(&str) -> Option<String>) -> Self {
        let colorterm = env("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        let term = env("TERM").unwrap_or_default().to_ascii_lowercase();
        let program = env("TERM_PROGRAM").unwrap_or_default();
        if colorterm.contains("truecolor")
            || colorterm.contains("24bit")
            || term.contains("direct")
            || term.contains("truecolor")
            || matches!(program.as_str(), "iTerm.app" | "WezTerm" | "ghostty" | "vscode" | "Hyper")
            || term.starts_with("xterm-kitty")
            || term.starts_with("xterm-ghostty")
            || term.starts_with("alacritty")
            || term.starts_with("foot")
        {
            Self::True
        } else if term.contains("256") || term.starts_with("tmux") || term.starts_with("screen") {
            Self::C256
        } else if term.is_empty() || term == "dumb" {
            Self::C16
        } else {
            // Most terminals in use today do 256 colours even when TERM undersells them.
            Self::C256
        }
    }

    /// Map a colour to this depth (nearest match).
    pub fn fit(self, c: Color) -> Color {
        match (self, c) {
            (Self::True, c) => c,
            (Self::C256, Color::Rgb(r, g, b)) => Color::Indexed(nearest_256(r, g, b)),
            (Self::C16, Color::Rgb(r, g, b)) => Color::Indexed(nearest_16(r, g, b)),
            (Self::C16, Color::Indexed(i)) if i >= 16 => {
                let (r, g, b) = palette(i);
                Color::Indexed(nearest_16(r, g, b))
            }
            (_, c) => c,
        }
    }
}

const ANSI: [(u8, u8, u8); 16] = [
    (0, 0, 0),
    (205, 0, 0),
    (0, 205, 0),
    (205, 205, 0),
    (0, 0, 238),
    (205, 0, 205),
    (0, 205, 205),
    (229, 229, 229),
    (127, 127, 127),
    (255, 0, 0),
    (0, 255, 0),
    (255, 255, 0),
    (92, 92, 255),
    (255, 0, 255),
    (0, 255, 255),
    (255, 255, 255),
];

fn palette(i: u8) -> (u8, u8, u8) {
    match i {
        0..=15 => ANSI[i as usize],
        16..=231 => {
            let v = i - 16;
            let level = |x: u8| if x == 0 { 0 } else { 55 + x * 40 };
            (level(v / 36), level(v / 6 % 6), level(v % 6))
        }
        _ => {
            let g = 8 + (i - 232) * 10;
            (g, g, g)
        }
    }
}

fn dist(a: (u8, u8, u8), b: (u8, u8, u8)) -> u32 {
    let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2) as u32;
    d(a.0, b.0) * 3 + d(a.1, b.1) * 4 + d(a.2, b.2) * 2
}

fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    (16..=255u8).min_by_key(|&i| dist(palette(i), (r, g, b))).unwrap_or(16)
}

fn nearest_16(r: u8, g: u8, b: u8) -> u8 {
    (0..16u8).min_by_key(|&i| dist(ANSI[i as usize], (r, g, b))).unwrap_or(7)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A theme ptyro doesn't have (one of your own, from before they were removed) is the
    /// default, never a crash; NO_COLOR wins over any.
    #[test]
    fn an_unknown_theme_is_the_default() {
        assert_eq!(Theme::resolve("ptyro-light", false), Theme::light());
        assert_eq!(Theme::resolve("mine", false), Theme::dark());
        assert!(Theme::resolve("high-contrast", true).monochrome);
        assert!(exists("high-contrast") && !exists("mine"));
    }

    #[test]
    fn downsampling() {
        assert_eq!(Depth::C256.fit(Color::Rgb(255, 0, 0)), Color::Indexed(196));
        assert_eq!(Depth::C16.fit(Color::Rgb(250, 250, 250)), Color::Indexed(15));
        assert_eq!(Depth::C16.fit(Color::Indexed(196)), Color::Indexed(9));
        assert_eq!(Depth::True.fit(Color::Rgb(1, 2, 3)), Color::Rgb(1, 2, 3));
        let env = |k: &str| match k {
            "COLORTERM" => Some("truecolor".into()),
            _ => None,
        };
        assert_eq!(Depth::detect(env), Depth::True);
        assert_eq!(Depth::detect(|k| (k == "TERM").then(|| "xterm-256color".into())), Depth::C256);
    }
}
