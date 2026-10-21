//! The child's terminal modes that matter for encoding input and for viewers (SPEC §5.5).

use alacritty_terminal::term::TermMode;
use ptyro_core::screen::ScreenModes;

/// Kitty keyboard progressive-enhancement flags, numbered as in the kitty spec.
pub mod kitty {
    pub const DISAMBIGUATE: u8 = 1;
    pub const REPORT_EVENT_TYPES: u8 = 2;
    pub const REPORT_ALTERNATE_KEYS: u8 = 4;
    pub const REPORT_ALL_KEYS: u8 = 8;
    pub const REPORT_TEXT: u8 = 16;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MouseProtocol {
    #[default]
    Off,
    /// Button press/release only (1000).
    Click,
    /// Plus motion while a button is held (1002).
    Drag,
    /// Plus all motion (1003).
    Motion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TermModes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub bracketed_paste: bool,
    pub focus_events: bool,
    pub alt_screen: bool,
    pub show_cursor: bool,
    pub mouse: MouseProtocol,
    /// SGR (1006) mouse encoding; otherwise legacy X10.
    pub sgr_mouse: bool,
    pub kitty: u8,
}

impl TermModes {
    pub fn from_alacritty(m: TermMode) -> Self {
        let mouse = if m.contains(TermMode::MOUSE_MOTION) {
            MouseProtocol::Motion
        } else if m.contains(TermMode::MOUSE_DRAG) {
            MouseProtocol::Drag
        } else if m.contains(TermMode::MOUSE_REPORT_CLICK) {
            MouseProtocol::Click
        } else {
            MouseProtocol::Off
        };
        let mut kitty = 0;
        for (tm, k) in [
            (TermMode::DISAMBIGUATE_ESC_CODES, kitty::DISAMBIGUATE),
            (TermMode::REPORT_EVENT_TYPES, kitty::REPORT_EVENT_TYPES),
            (TermMode::REPORT_ALTERNATE_KEYS, kitty::REPORT_ALTERNATE_KEYS),
            (TermMode::REPORT_ALL_KEYS_AS_ESC, kitty::REPORT_ALL_KEYS),
            (TermMode::REPORT_ASSOCIATED_TEXT, kitty::REPORT_TEXT),
        ] {
            if m.contains(tm) {
                kitty |= k;
            }
        }
        Self {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            app_keypad: m.contains(TermMode::APP_KEYPAD),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            focus_events: m.contains(TermMode::FOCUS_IN_OUT),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
            show_cursor: m.contains(TermMode::SHOW_CURSOR),
            mouse,
            sgr_mouse: m.contains(TermMode::SGR_MOUSE),
            kitty,
        }
    }

    pub fn viewer(&self) -> ScreenModes {
        ScreenModes {
            mouse: self.mouse != MouseProtocol::Off,
            alt_screen: self.alt_screen,
            bracketed_paste: self.bracketed_paste,
            focus_events: self.focus_events,
            kitty_keyboard: self.kitty != 0,
            app_cursor: self.app_cursor,
        }
    }
}
