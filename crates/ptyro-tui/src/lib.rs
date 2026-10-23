//! The ptyro TUI (SPEC §11): a sidebar of sessions, up to four panes, a status bar, the prefix
//! key, a palette and finder, copy mode, themes and mouse support (M2). Raw mode shows a single
//! session with no chrome.

mod app;
pub mod chat;
mod cli_actions;
pub mod copy;
mod health;
mod inspector;
pub mod keymap;
mod keys;
pub mod layout;
mod onboard;
mod outer;
#[doc(hidden)] // public for the benches
pub mod paint;
pub mod palette;
mod runs;
mod sets;
pub mod theme;
pub mod tree;
mod ui;

use std::io::{IsTerminal, Write};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, EventStream, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use futures::StreamExt;
use ptyro_client::ConnectOptions;
use ptyro_core::config::Config;
use ptyro_core::paths::Paths;
use ptyro_core::session::ProcessState;
use ptyro_core::{SessionId, TermSize};
use ratatui::layout::Rect;

use crate::app::App;

/// How attaching ended.
#[derive(Debug)]
pub enum AttachEnd {
    /// The user detached; sessions keep running.
    Detached,
    /// Raw mode: the session's process exited.
    Exited(ProcessState),
    /// Raw mode: the session was removed.
    Gone,
    /// The daemon went away and didn't come back.
    Lost(String),
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Show this session in the focused pane.
    pub focus: Option<SessionId>,
    /// One session, full screen, no chrome.
    pub raw: bool,
    /// Never type, resize or take control.
    pub view_only: bool,
    /// Open in this workspace (default: the last one used).
    pub workspace: Option<String>,
    /// Show the agent that starts in this worktree (after its setup script) as soon as it does.
    pub follow_worktree: Option<String>,
    /// Start the guided tour (`ptyro tour`).
    pub tour: bool,
    /// `ptyro` started the first session itself: the first run (no config yet) or home (UX1.19).
    pub welcome: bool,
}

const FRAME: Duration = Duration::from_millis(8);
const RECONNECT_FOR: Duration = Duration::from_secs(15);

/// The size a session started from this terminal should have: the focused pane's, given the
/// sidebar and status bar the TUI will draw.
pub fn pane_size(paths: &Paths) -> Option<TermSize> {
    if !std::io::stdout().is_terminal() {
        return None;
    }
    let (cols, rows) = crossterm::terminal::size().ok()?;
    let config = Config::load(&Config::path(paths)).unwrap_or_default();
    let side = if config.ui.sidebar && cols > config.ui.sidebar_width + 20 {
        config.ui.sidebar_width + 1
    } else {
        0
    };
    let bar = u16::from(config.ui.status_bar);
    Some(TermSize::new(cols - side, rows.saturating_sub(bar + 1)))
}

/// Run the TUI until the user detaches (or, in raw mode, the session ends). Owns the terminal
/// meanwhile and always restores it, also on errors and panics (ratatui's hook, wrapped).
pub async fn run(mut connect: ConnectOptions, paths: &Paths, opts: Options) -> Result<AttachEnd> {
    let config = Config::load(&Config::path(paths)).unwrap_or_default();
    let mut tui = ratatui::init();
    let outer = outer::query();
    let kitty = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    connect.outer = Some(ptyro_protocol::OuterTerm { kitty_keyboard: kitty, ..outer });
    let mut out = std::io::stdout();
    if kitty {
        // Lets us tell Shift+Enter, Ctrl-I/Tab etc. apart and re-encode them faithfully.
        let _ = execute!(
            out,
            PushKeyboardEnhancementFlags(
                KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                    | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
            )
        );
    }
    let mouse = config.ui.mouse;
    let _ = execute!(out, EnableBracketedPaste, EnableFocusChange);
    if mouse {
        let _ = execute!(out, EnableMouseCapture);
    }
    // On a panic, undo the modes we turned on too, then ratatui's hook leaves raw mode and the
    // alternate screen (kitty keeps a flag stack per screen, so pop before leaving it).
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = std::io::stdout();
        if kitty {
            let _ = execute!(out, PopKeyboardEnhancementFlags);
        }
        let _ = execute!(out, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture);
        previous(info);
    }));
    let size = tui.size().unwrap_or_default();
    let mut app = App::new(
        opts,
        connect,
        paths.clone(),
        config,
        kitty,
        Rect::new(0, 0, size.width, size.height),
    );
    let result = event_loop(&mut app, &mut tui).await;
    if app.client.is_some() {
        app.save_view();
        // Give the save a moment to leave before the connection drops.
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    if kitty {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        crossterm::cursor::SetCursorStyle::DefaultUserShape
    );
    ratatui::restore();
    let _ = out.flush();
    result
}

async fn recv<T>(rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<T>>) -> Option<T> {
    match rx {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn event_loop(app: &mut App, tui: &mut ratatui::DefaultTerminal) -> Result<AttachEnd> {
    if let Err(e) = app.connect().await {
        return Ok(AttachEnd::Lost(e));
    }
    app.restore_view().await;
    app.welcome();
    let mut events = EventStream::new();
    let mut last_draw = Instant::now() - FRAME;
    let mut cursor_shape = None;
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    loop {
        if let Some(end) = app.quit.take() {
            return Ok(end);
        }
        if app.dirty && last_draw.elapsed() >= FRAME {
            sync_cursor_shape(app, &mut cursor_shape);
            tui.draw(|f| ui::draw(app, f))?;
            app.dirty = false;
            last_draw = Instant::now();
        }
        let next_draw = last_draw + FRAME;
        // The which-key menu, once the prefix has waited for its key (UX1.14).
        let which_key = app.which_key_due().filter(|t| *t > Instant::now());
        tokio::select! {
            () = tokio::time::sleep_until(which_key.unwrap_or_else(Instant::now).into()), if which_key.is_some() => {
                app.dirty = true;
            }
            msg = recv(&mut app.pushes) => match msg {
                Some(msg) => app.on_server(msg),
                None => app.lost("connection closed"),
            },
            ev = events.next() => {
                let Some(ev) = ev else { return Ok(AttachEnd::Detached) };
                match ev? {
                    Event::Key(k) => app.on_key(&k),
                    Event::Paste(text) => app.on_paste(text),
                    Event::FocusGained => app.on_focus(true),
                    Event::FocusLost => app.on_focus(false),
                    Event::Mouse(m) => app.on_mouse(&m),
                    Event::Resize(w, h) => app.on_resize(w, h),
                }
            }
            Some(msg) = app.rx.recv() => app.on_reply(msg),
            () = tokio::time::sleep_until(next_draw.into()), if app.dirty => {}
            _ = tick.tick() => {
                app.tick();
                if let Some(since) = app.lost_since {
                    if app.connect().await.is_ok() {
                        app.toast("reconnected");
                        app.restore_view().await;
                    } else if since.elapsed() > RECONNECT_FOR {
                        return Ok(AttachEnd::Lost("the daemon did not come back".into()));
                    }
                }
            }
        }
    }
}

/// The real cursor's shape follows what the focused program asked for.
fn sync_cursor_shape(app: &App, last: &mut Option<(ptyro_core::screen::CursorShape, bool)>) {
    use crossterm::cursor::SetCursorStyle as S;
    use ptyro_core::screen::CursorShape as C;
    let Some(c) = app.focused_session().and_then(|id| app.caches.get(&id)).map(|c| c.cursor) else {
        return;
    };
    if *last == Some((c.shape, c.blinking)) {
        return;
    }
    *last = Some((c.shape, c.blinking));
    let style = match (c.shape, c.blinking) {
        (C::Block, true) => S::BlinkingBlock,
        (C::Block, false) => S::SteadyBlock,
        (C::Underline, true) => S::BlinkingUnderScore,
        (C::Underline, false) => S::SteadyUnderScore,
        (C::Bar, true) => S::BlinkingBar,
        (C::Bar, false) => S::SteadyBar,
    };
    let _ = execute!(std::io::stdout(), style);
}

pub(crate) fn bell() {
    let mut out = std::io::stdout();
    let _ = out.write_all(b"\x07");
    let _ = out.flush();
}

/// A desktop notification through the outer terminal (OSC 777 `notify`, which Ghostty, foot,
/// WezTerm, kitty and urxvt show; iTerm2 and others take OSC 9), wrapped for tmux when inside it.
pub(crate) fn notify_outer(title: &str, body: &str) {
    let clean = |s: &str| s.chars().filter(|c| !c.is_control() && *c != ';').collect::<String>();
    let program = std::env::var("TERM_PROGRAM").unwrap_or_default();
    let seq = if program == "iTerm.app" || program == "Apple_Terminal" {
        format!("\x1b]9;{}: {}\x07", clean(title), clean(body))
    } else {
        format!("\x1b]777;notify;{};{}\x07", clean(title), clean(body))
    };
    let seq = if std::env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
    } else {
        seq
    };
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

/// Put text on the real terminal's clipboard (OSC 52), wrapped for tmux when inside it.
pub(crate) fn osc52(text: &str) {
    let seq = format!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let seq = if std::env::var_os("TMUX").is_some() {
        format!("\x1bPtmux;{}\x1b\\", seq.replace('\x1b', "\x1b\x1b"))
    } else {
        seq
    };
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                s.push(T[(n >> (18 - 6 * i)) as usize & 63] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_matches_rfc4648() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
    }
}
