//! The chat view of a structured session (M10.1): messages, thinking, tool calls with their
//! output and diffs, permission requests as cards, plans and turn ends, with the draft being
//! typed at the bottom. The transcript comes from the daemon (`ServerMsg::Chat`); keys still go
//! to the daemon's line editor, so typing, Enter, and `a`/`A`/`d` on a permission card work the
//! same here, in the web app, and in a plain `ptyro attach --raw`.

use ptyro_core::chat::{ChatBody, ChatItem, NoticeLevel, ToolStatus};
use std::cell::{Cell, RefCell};

use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use unicode_width::UnicodeWidthChar;

use crate::theme::{Depth, Theme};

/// One session's transcript as this client has it.
#[derive(Debug, Default)]
pub struct ChatView {
    pub items: Vec<ChatItem>,
    pub rev: u64,
    pub draft: String,
    /// Lines scrolled up from the bottom (0 follows new output).
    pub scroll: usize,
    /// How far up it could scroll when last drawn (Page Up stops there).
    pub max_scroll: Cell<usize>,
    /// The transcript wrapped for the last width, theme and colour depth; `apply` drops it.
    wrapped: RefCell<Option<Wrapped>>,
}

#[derive(Debug)]
struct Wrapped {
    width: usize,
    theme: Theme,
    depth: Depth,
    rows: Vec<Vec<(char, Style)>>,
}

impl ChatView {
    pub fn apply(&mut self, rev: u64, reset: bool, items: Vec<ChatItem>, draft: String) {
        if reset {
            self.items.clear();
        }
        for it in items {
            match self.items.iter_mut().find(|x| x.id == it.id) {
                Some(x) => *x = it,
                None => self.items.push(it),
            }
        }
        self.rev = rev;
        self.draft = draft;
        self.wrapped.take();
    }

    pub fn pending_permission(&self) -> bool {
        self.items.iter().any(|i| matches!(&i.body, ChatBody::Permission { decided: None, .. }))
    }
}

/// A styled run of text before wrapping.
struct Seg {
    text: String,
    style: Style,
}

struct Doc {
    lines: Vec<Vec<Seg>>,
}

impl Doc {
    fn line(&mut self, segs: Vec<Seg>) {
        self.lines.push(segs);
    }

    fn text(&mut self, prefix: &str, pstyle: Style, text: &str, style: Style) {
        for (i, l) in text.split('\n').enumerate() {
            let p = if i == 0 { prefix.to_owned() } else { " ".repeat(prefix.chars().count()) };
            self.line(vec![seg(&p, pstyle), seg(l, style)]);
        }
    }
}

fn seg(t: &str, style: Style) -> Seg {
    Seg { text: t.to_owned(), style }
}

/// Wrap a line of segments to `width` columns.
fn wrap(line: &[Seg], width: usize) -> Vec<Vec<(char, Style)>> {
    let mut out = vec![Vec::new()];
    let mut col = 0;
    for s in line {
        for c in s.text.chars() {
            let w = c.width().unwrap_or(0);
            if col + w > width && col > 0 {
                out.push(Vec::new());
                col = 0;
            }
            out.last_mut().expect("line").push((c, s.style));
            col += w;
        }
    }
    out
}

fn tool_color(st: ToolStatus, t: &Theme) -> Color {
    match st {
        ToolStatus::Done => t.done,
        ToolStatus::Failed | ToolStatus::Declined => t.error,
        _ => t.working,
    }
}

fn build(view: &ChatView, t: &Theme, depth: Depth) -> Doc {
    let c = |c: Color| Style::new().fg(depth.fit(c));
    let mut d = Doc { lines: Vec::new() };
    for it in &view.items {
        match &it.body {
            ChatBody::User { text } => {
                d.line(vec![]);
                d.text(
                    "› ",
                    c(t.accent).add_modifier(Modifier::BOLD),
                    text,
                    c(t.text).add_modifier(Modifier::BOLD),
                );
            }
            ChatBody::Assistant { text, streaming } => {
                d.text("", Style::new(), text.trim_end_matches('\n'), c(t.text));
                if *streaming {
                    d.line(vec![seg("▍", c(t.accent))]);
                }
            }
            ChatBody::Thinking { text } => {
                let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("…");
                d.line(vec![
                    seg("· ", c(t.dim)),
                    seg(first, c(t.dim).add_modifier(Modifier::ITALIC)),
                ]);
            }
            ChatBody::Tool { name, title, status, output, diff, .. } => {
                d.line(vec![
                    seg(&format!("{} ", status.glyph()), c(tool_color(*status, t))),
                    seg(&format!("{name} "), c(t.accent).add_modifier(Modifier::BOLD)),
                    seg(title, c(t.text)),
                ]);
                if let Some(df) = diff {
                    for l in df
                        .lines()
                        .filter(|l| !l.starts_with("---") && !l.starts_with("+++"))
                        .take(20)
                    {
                        let st = if l.starts_with('+') {
                            c(t.done)
                        } else if l.starts_with('-') {
                            c(t.error)
                        } else {
                            c(t.dim)
                        };
                        d.line(vec![seg("   ", Style::new()), seg(l, st)]);
                    }
                }
                if let Some(o) = output.as_deref().filter(|o| !o.trim().is_empty()) {
                    let lines: Vec<&str> = o.lines().collect();
                    let n = if matches!(status, ToolStatus::Failed) { 6 } else { 3 };
                    let show = if matches!(status, ToolStatus::Failed) {
                        &lines[lines.len().saturating_sub(n)..]
                    } else {
                        &lines[..lines.len().min(n)]
                    };
                    for l in show {
                        d.line(vec![seg("   ", Style::new()), seg(l, c(t.dim))]);
                    }
                    if lines.len() > n {
                        d.line(vec![seg(&format!("   … {} lines", lines.len()), c(t.dim))]);
                    }
                }
            }
            ChatBody::Permission { tool, title, decided, options, .. } => match decided {
                None => {
                    let warn = c(t.blocked).add_modifier(Modifier::BOLD);
                    d.line(vec![
                        seg("╭─ ", c(t.blocked)),
                        seg("wants permission: ", warn),
                        seg(tool, c(t.text).add_modifier(Modifier::BOLD)),
                    ]);
                    for l in title.lines() {
                        d.line(vec![seg("│  ", c(t.blocked)), seg(l, c(t.text))]);
                    }
                    let keys: Vec<String> = options
                        .iter()
                        .map(|o| match o.kind {
                            ptyro_core::chat::PermKind::AllowOnce => format!("[a] {}", o.label),
                            ptyro_core::chat::PermKind::AllowAlways => format!("[A] {}", o.label),
                            _ => format!("[d] {}", o.label),
                        })
                        .collect();
                    d.line(vec![seg("╰─ ", c(t.blocked)), seg(&keys.join("   "), c(t.accent))]);
                }
                Some(dec) => {
                    let line = title.lines().next().unwrap_or(tool);
                    d.line(vec![seg("⚑ ", c(t.dim)), seg(&format!("{line} → {dec}"), c(t.dim))]);
                }
            },
            ChatBody::Plan { steps } => {
                d.line(vec![seg("☰ plan", c(t.accent).add_modifier(Modifier::BOLD))]);
                for s in steps {
                    let (g, st) = match s.status.as_str() {
                        "completed" => ("✓", c(t.done)),
                        "in_progress" => ("▸", c(t.working)),
                        _ => ("○", c(t.dim)),
                    };
                    d.line(vec![seg(&format!("  {g} "), st), seg(&s.text, c(t.text))]);
                }
            }
            ChatBody::Notice { level, text } => {
                let col = match level {
                    NoticeLevel::Info => t.dim,
                    NoticeLevel::Warn => t.blocked,
                    NoticeLevel::Error => t.error,
                };
                d.text("! ", c(col), text, c(col));
            }
            ChatBody::TurnEnd { stop, cost_micro_usd, duration_ms, .. } => {
                let mut parts = vec![stop.clone()];
                if let Some(m) = cost_micro_usd {
                    parts.push(format!("${:.4}", *m as f64 / 1e6));
                }
                if let Some(ms) = duration_ms {
                    parts.push(format!("{:.1}s", *ms as f64 / 1000.0));
                }
                d.line(vec![seg(&format!("── {} ──", parts.join(" · ")), c(t.dim))]);
            }
        }
    }
    d
}

/// Draw the chat view into `area`; returns where the cursor goes (end of the draft) when focused.
pub fn draw(
    view: &ChatView,
    buf: &mut Buffer,
    area: Rect,
    t: &Theme,
    depth: Depth,
    focused: bool,
) -> Option<Position> {
    if area.width < 4 || area.height < 2 {
        return None;
    }
    let width = area.width as usize;
    let mut cache = view.wrapped.borrow_mut();
    if !cache.as_ref().is_some_and(|c| c.width == width && c.theme == *t && c.depth == depth) {
        let rows = build(view, t, depth).lines.iter().flat_map(|l| wrap(l, width)).collect();
        *cache = Some(Wrapped { width, theme: *t, depth, rows });
    }
    let rows = &cache.as_ref().expect("wrapped").rows;
    // The composer: the draft, or a hint.
    let accent = Style::new().fg(depth.fit(t.accent)).add_modifier(Modifier::BOLD);
    let hint = if view.pending_permission() && view.draft.is_empty() {
        "a allow · A always · d deny".to_owned()
    } else if view.pending_permission() {
        "(Esc clears the draft, then a/A/d)".to_owned()
    } else if view.draft.is_empty() {
        "type a message · Enter sends · Ctrl-C interrupts".to_owned()
    } else {
        String::new()
    };
    let mut composer: Vec<Vec<(char, Style)>> = Vec::new();
    for (i, l) in view.draft.split('\n').enumerate() {
        let segs = vec![
            seg(if i == 0 { "› " } else { "  " }, accent),
            seg(l, Style::new().fg(depth.fit(t.text))),
        ];
        composer.extend(wrap(&segs, width));
    }
    // The cursor goes after the draft's last wrapped row (before any hint).
    let draft_end: usize =
        composer.last().map_or(0, |r| r.iter().map(|(c, _)| c.width().unwrap_or(0)).sum());
    if !hint.is_empty() {
        let last = composer.last_mut().expect("composer");
        if last.len() + hint.len() + 1 < width {
            last.extend(
                std::iter::once((' ', Style::new()))
                    .chain(hint.chars().map(|c| (c, Style::new().fg(depth.fit(t.dim))))),
            );
        }
    }
    let comp_h = composer.len().min(area.height as usize / 2).max(1);
    let body_h = area.height as usize - comp_h;
    let max_scroll = rows.len().saturating_sub(body_h);
    view.max_scroll.set(max_scroll);
    let scroll = view.scroll.min(max_scroll);
    let end = rows.len() - scroll;
    let start = end.saturating_sub(body_h);
    let put = |buf: &mut Buffer, y: u16, row: &[(char, Style)]| {
        let mut x = area.x;
        for (ch, st) in row {
            let w = ch.width().unwrap_or(0) as u16;
            if w == 0 || x + w > area.right() {
                continue;
            }
            if let Some(cell) = buf.cell_mut(Position::new(x, y)) {
                cell.set_char(*ch).set_style(*st);
            }
            x += w;
        }
    };
    for (i, row) in rows[start..end].iter().enumerate() {
        put(buf, area.y + i as u16, row);
    }
    if scroll > 0 {
        let note = format!(" ↓ {scroll} more ");
        let x = area.right().saturating_sub(note.len() as u16);
        let y = area.y + body_h.saturating_sub(1) as u16;
        for (i, ch) in note.chars().enumerate() {
            if let Some(cell) = buf.cell_mut(Position::new(x + i as u16, y)) {
                cell.set_char(ch)
                    .set_style(Style::new().fg(depth.fit(t.bar_fg)).bg(depth.fit(t.bar_bg)));
            }
        }
    }
    let comp_y = area.y + body_h as u16;
    let shown = &composer[composer.len() - comp_h..];
    for (i, row) in shown.iter().enumerate() {
        put(buf, comp_y + i as u16, row);
    }
    // Cursor at the end of the draft: the last composer row is shown last.
    focused.then(|| {
        Position::new(area.x + draft_end.min(width - 1) as u16, comp_y + comp_h as u16 - 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::chat::PermOption;

    #[test]
    fn draws_cards_and_the_draft() {
        let mut v = ChatView::default();
        v.apply(
            3,
            true,
            vec![
                ChatItem::new("u", ChatBody::User { text: "fix the test".into() }),
                ChatItem::new(
                    "t",
                    ChatBody::Tool {
                        name: "Bash".into(),
                        title: "cargo test".into(),
                        status: ToolStatus::Failed,
                        output: Some("error: 1 failed".into()),
                        diff: None,
                        files: vec![],
                    },
                ),
                ChatItem::new(
                    "p",
                    ChatBody::Permission {
                        request: "r".into(),
                        tool: "Bash".into(),
                        title: "rm -rf target".into(),
                        options: PermOption::standard(),
                        decided: None,
                    },
                ),
            ],
            "ok".into(),
        );
        let area = Rect::new(0, 0, 50, 12);
        let mut buf = Buffer::empty(area);
        let cur = draw(&v, &mut buf, area, &Theme::dark(), Depth::True, true);
        let text: String = (0..12)
            .map(|y| (0..50).map(|x| buf[(x, y)].symbol().to_owned()).collect::<String>() + "\n")
            .collect();
        assert!(text.contains("› fix the test"), "{text}");
        assert!(text.contains("✗ Bash cargo test") && text.contains("error: 1 failed"), "{text}");
        assert!(text.contains("wants permission: Bash") && text.contains("[a] Allow"), "{text}");
        assert!(text.contains("› ok"), "{text}");
        assert_eq!(cur, Some(Position::new(4, 11)));
        // Updates replace by id; scrolling is clamped.
        v.apply(
            4,
            false,
            vec![ChatItem::new(
                "p",
                ChatBody::Permission {
                    request: "r".into(),
                    tool: "Bash".into(),
                    title: "rm -rf target".into(),
                    options: PermOption::standard(),
                    decided: Some("deny".into()),
                },
            )],
            String::new(),
        );
        assert_eq!(v.items.len(), 3);
        assert!(!v.pending_permission());
        v.scroll = 1000;
        draw(&v, &mut buf, area, &Theme::dark(), Depth::True, false);
    }

    #[test]
    fn the_cursor_follows_a_wrapped_draft() {
        let mut v = ChatView::default();
        // 2 + 30 columns wrap into 20 + 12; then a second line "ab".
        v.apply(1, true, vec![], format!("{}\nab", "x".repeat(30)));
        let area = Rect::new(0, 0, 20, 10);
        let mut buf = Buffer::empty(area);
        let cur = draw(&v, &mut buf, area, &Theme::dark(), Depth::True, true);
        // Three composer rows at the bottom; the cursor after "  ab".
        assert_eq!(cur, Some(Position::new(4, 9)));
        assert_eq!(buf[(2, 9)].symbol(), "a");
    }
}
