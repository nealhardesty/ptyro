//! The normalized screen model the daemon sends to clients (SPEC §6.5): styled runs per row, a
//! per-subscription style table, cursor and the modes a client needs for its own UX.

use serde::{Deserialize, Serialize};

/// Deserializes through [`TermSize::new`], so a size off the wire is always in range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(from = "RawSize")]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

#[derive(Deserialize)]
struct RawSize {
    cols: u16,
    rows: u16,
}

impl From<RawSize> for TermSize {
    fn from(r: RawSize) -> Self {
        Self::new(r.cols, r.rows)
    }
}

impl Default for TermSize {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TermSize {
    pub const DEFAULT: Self = Self { cols: 120, rows: 40 };
    pub const MIN: Self = Self { cols: 2, rows: 1 };
    /// Larger than any real screen; bounds what one session's grid can cost.
    pub const MAX: Self = Self { cols: 1000, rows: 500 };

    pub fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols: cols.clamp(Self::MIN.cols, Self::MAX.cols),
            rows: rows.clamp(Self::MIN.rows, Self::MAX.rows),
        }
    }

    /// This size brought within [`MIN`](Self::MIN)..=[`MAX`](Self::MAX).
    pub fn clamped(self) -> Self {
        Self::new(self.cols, self.rows)
    }
}

impl std::fmt::Display for TermSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}x{}", self.cols, self.rows)
    }
}

/// `Default` means "the viewer's own default colour", so themes pass through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Color {
    #[default]
    Default,
    Indexed {
        i: u8,
    },
    Rgb {
        r: u8,
        g: u8,
        b: u8,
    },
}

/// Bit set of text attributes. Plain `u16` on the wire so every encoding agrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct StyleFlags(pub u16);

impl StyleFlags {
    pub const BOLD: u16 = 1;
    pub const DIM: u16 = 1 << 1;
    pub const ITALIC: u16 = 1 << 2;
    pub const UNDERLINE: u16 = 1 << 3;
    pub const DOUBLE_UNDERLINE: u16 = 1 << 4;
    pub const CURLY_UNDERLINE: u16 = 1 << 5;
    pub const DOTTED_UNDERLINE: u16 = 1 << 6;
    pub const DASHED_UNDERLINE: u16 = 1 << 7;
    pub const INVERSE: u16 = 1 << 8;
    pub const HIDDEN: u16 = 1 << 9;
    pub const STRIKE: u16 = 1 << 10;
    pub const ANY_UNDERLINE: u16 = Self::UNDERLINE
        | Self::DOUBLE_UNDERLINE
        | Self::CURLY_UNDERLINE
        | Self::DOTTED_UNDERLINE
        | Self::DASHED_UNDERLINE;

    pub fn has(self, flag: u16) -> bool {
        self.0 & flag != 0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub struct Style {
    #[serde(default)]
    pub fg: Color,
    #[serde(default)]
    pub bg: Color,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ul: Option<Color>,
    #[serde(default)]
    pub flags: StyleFlags,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StyleDef {
    pub id: u32,
    pub style: Style,
}

/// Consecutive cells sharing one style and one width.
///
/// `text` is the concatenation of the cells' contents. Normally each `char` is one cell;
/// `clusters`, present only when some cell holds combining characters, gives the number of
/// `char`s in each cell. `wide` means every cell of this run is two columns wide. The client never
/// has to compute character widths itself, so it can't drift from the daemon's emulator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub style: u32,
    pub text: String,
    #[serde(default)]
    pub wide: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clusters: Option<Vec<u8>>,
}

impl Run {
    /// Cell contents, one entry per cell.
    pub fn cells(&self) -> Vec<&str> {
        match &self.clusters {
            None => {
                self.text.char_indices().map(|(i, c)| &self.text[i..i + c.len_utf8()]).collect()
            }
            Some(lens) => {
                let mut out = Vec::with_capacity(lens.len());
                let mut rest = self.text.as_str();
                for &n in lens {
                    let end = rest.char_indices().nth(n as usize).map_or(rest.len(), |(i, _)| i);
                    out.push(&rest[..end]);
                    rest = &rest[end..];
                }
                out
            }
        }
    }

    /// Columns covered by this run.
    pub fn width(&self) -> usize {
        let n = self.clusters.as_ref().map_or_else(|| self.text.chars().count(), Vec::len);
        if self.wide { n * 2 } else { n }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowUpdate {
    pub y: u16,
    pub runs: Vec<Run>,
    /// The row soft-wraps into the next one (copy mode joins them).
    #[serde(default)]
    pub wrapped: bool,
    /// A shell prompt starts on this row (OSC 133;A shell integration; copy mode jumps here).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub prompt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Bar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CursorState {
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    #[serde(default)]
    pub shape: CursorShape,
    #[serde(default)]
    pub blinking: bool,
}

/// Modes a viewer needs to know about (whether to capture the mouse, how to paste, …). Encoding
/// of keys and mouse against the child's modes happens in the daemon (SPEC §5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScreenModes {
    pub mouse: bool,
    pub alt_screen: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub kitty_keyboard: bool,
    pub app_cursor: bool,
}

/// The body of a screen frame: a full image (keyframe) or the changes since the last frame.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScreenDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<TermSize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub styles: Vec<StyleDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rows: Vec<RowUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<CursorState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modes: Option<ScreenModes>,
    /// `Some("")` clears the title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// A frozen copy of a session's scrollback plus its current screen, top to bottom (SPEC §5.4):
/// copy mode works on it while the live screen keeps updating.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScrollbackDoc {
    pub size: TermSize,
    /// How many of `rows` are history (the rest is the screen at the time of the request).
    pub history: u32,
    /// In order; each row's `y` is not meaningful here.
    pub rows: Vec<RowUpdate>,
    pub styles: Vec<StyleDef>,
    /// Cursor position on the screen part.
    pub cursor: CursorState,
}

impl ScreenDelta {
    pub fn is_empty(&self) -> bool {
        self.size.is_none()
            && self.rows.is_empty()
            && self.cursor.is_none()
            && self.modes.is_none()
            && self.title.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_cells_honour_clusters() {
        let plain = Run { style: 0, text: "ab漢".into(), wide: false, clusters: None };
        assert_eq!(plain.cells(), ["a", "b", "漢"]);
        let combined =
            Run { style: 0, text: "e\u{301}x".into(), wide: false, clusters: Some(vec![2, 1]) };
        assert_eq!(combined.cells(), ["e\u{301}", "x"]);
        assert_eq!(combined.width(), 2);
        let wide = Run { style: 0, text: "漢字".into(), wide: true, clusters: None };
        assert_eq!(wide.width(), 4);
    }

    #[test]
    fn sizes_off_the_wire_are_clamped() {
        let s: TermSize = serde_json::from_str(r#"{"cols":0,"rows":0}"#).unwrap();
        assert_eq!(s, TermSize::MIN);
        let s: TermSize = serde_json::from_str(r#"{"cols":65535,"rows":65535}"#).unwrap();
        assert_eq!(s, TermSize::MAX);
        let s: TermSize = serde_json::from_str(r#"{"cols":80,"rows":24}"#).unwrap();
        assert_eq!(s, TermSize::new(80, 24));
        assert_eq!(serde_json::to_string(&s).unwrap(), r#"{"cols":80,"rows":24}"#);
    }

    #[test]
    fn color_json_shape() {
        let s = serde_json::to_string(&Color::Rgb { r: 1, g: 2, b: 3 }).unwrap();
        assert_eq!(s, r#"{"t":"rgb","r":1,"g":2,"b":3}"#);
    }
}
