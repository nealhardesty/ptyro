//! `ptyro home` (SPEC §26.8): everything, ranked, as text. What needs you, what was left
//! hanging, what is being worked on and what is quiet, a line a project, with every profile's
//! usage under it. `ptyro home <project>` unfolds one: where it was left off, what happened
//! there lately, and the files that work touched most.

use std::io::IsTerminal;

use ptyro_core::Error;
use ptyro_core::home::{self, HomeView, Section};
use ptyro_protocol::{HomeDismiss, Reply, Request};

use crate::cli::Ctx;

pub struct Args {
    pub project: Option<String>,
    pub json: bool,
    pub put_away: bool,
    pub restore: bool,
}

async fn ask(
    c: &ptyro_client::Client,
    trail: Option<String>,
    dismiss: Option<HomeDismiss>,
    restore: bool,
) -> Result<HomeView, Error> {
    match c.request(Request::Home { trail, dismiss, restore, here: true }).await? {
        Reply::Home { home } => Ok(*home),
        _ => Err(Error::internal("unexpected reply")),
    }
}

pub async fn run(ctx: &Ctx, a: Args) -> Result<i32, Error> {
    // Home is worth having with nothing running: the daemon is started for it.
    let c = ctx.connect(true).await?;
    let mut view = ask(&c, a.project.clone(), None, a.restore).await?;
    if let Some(word) = &a.project
        && view.trail.is_none()
    {
        let names: Vec<&str> = view.rows.iter().map(|r| r.name.as_str()).collect();
        return Err(Error::invalid(format!(
            "no project \"{word}\" on home (there {}: {})",
            if names.len() == 1 { "is" } else { "are" },
            if names.is_empty() { "none yet".to_owned() } else { names.join(", ") }
        )));
    }
    if a.put_away {
        let Some(row) = a.project.as_deref().and_then(|w| view.find(w)) else {
            return Err(Error::invalid(
                "--put-away needs the project: `ptyro home <project> --put-away`",
            ));
        };
        let t = row.next();
        if !t.next.dismissable() {
            return Err(Error::invalid(format!(
                "{}'s next thing is \"{}\": only what was left hanging can be put away",
                row.name,
                t.next.verb()
            )));
        }
        let put = HomeDismiss { key: t.key.clone(), stamp: t.stamp.clone() };
        let was = format!("{}: {} {}", row.name, t.next.verb(), t.text);
        view = ask(&c, a.project.clone(), Some(put), false).await?;
        if !a.json {
            eprintln!(
                "put away until it changes ({was}); `ptyro home --restore` brings everything back"
            );
        }
    }
    if a.json {
        println!("{}", serde_json::to_string_pretty(&view).unwrap_or_default());
        return Ok(0);
    }
    print!("{}", paint(&home::text(&view), std::io::stdout().is_terminal()));
    Ok(0)
}

/// The text with its section titles bold and the first one's in colour, on a terminal.
fn paint(text: &str, terminal: bool) -> String {
    // (What was said is already without control characters: `home::clean`.)
    if !terminal {
        return text.to_owned();
    }
    text.lines()
        .map(|l| {
            if l == Section::Needs.title() {
                format!("\x1b[1;33m{l}\x1b[0m\n")
            } else if Section::ALL.iter().any(|s| l == s.title()) {
                format!("\x1b[1m{l}\x1b[0m\n")
            } else {
                format!("{l}\n")
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_terminal_gets_colour() {
        let text = "1 needs you\n\nNEEDS YOU\n  ptyro  answer  go?\n\nQUIET\n  blog\n";
        assert_eq!(paint(text, false), text);
        let painted = paint(text, true);
        assert!(painted.contains("\x1b[1;33mNEEDS YOU\x1b[0m\n"));
        assert!(painted.contains("\x1b[1mQUIET\x1b[0m\n"));
        assert!(painted.contains("  ptyro  answer  go?\n"));
    }
}
