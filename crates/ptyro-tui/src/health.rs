//! The health badge (ROADMAP UX1.20): what `ptyro doctor` finds, run in the background, plus
//! what the TUI can see itself (no shell integration), as a status-bar badge; `M` (mend) lists
//! the findings, each with its fix where there is one: the shell-init line, tmux's options,
//! `chmod 600`, or the `ptyro` command the doctor suggests, run in a split. Running `ptyro …`
//! commands in a split is also how the TUI reaches the CLI's long tail (UX1.21).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::app::{App, AppMsg, Confirmed, Overlay, Purpose};
use crate::palette::{Item, Palette, Target};

/// How often the doctor runs again.
const EVERY: Duration = Duration::from_secs(600);
/// The first run, a while after attaching: it isn't urgent, and it starts a few processes.
const FIRST: Duration = Duration::from_secs(20);

/// A doctor check, as `ptyro doctor --json` prints it.
#[derive(Debug, Clone, Deserialize)]
pub struct Check {
    pub section: String,
    pub name: String,
    pub level: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fix {
    /// Run `ptyro <args>` in a split.
    Cli(Vec<String>),
    /// Append `line` to `file` (asks first).
    Append { file: PathBuf, line: String },
    /// `chmod 600` the file.
    Private(PathBuf),
    /// `tmux set -g <name> <value>`, now and in `~/.tmux.conf`.
    Tmux { name: String, value: String },
}

#[derive(Debug, Clone)]
pub struct Finding {
    pub problem: bool,
    pub what: String,
    pub text: String,
    pub fix: Option<Fix>,
}

#[derive(Debug, Default)]
pub struct Health {
    pub findings: Vec<Finding>,
    pub last: Option<Instant>,
    pub running: bool,
    /// How many there were when last announced (a new one is announced once).
    pub announced: usize,
    /// A fresh result that arrived while the mend list was open: its rows point into
    /// `findings`, so it is taken once the list is closed.
    pub deferred: Option<Vec<Check>>,
}

/// The command a doctor message suggests: the first `` `ptyro …` `` in it, with a
/// `<profile>` filled in from a `profile <name>` section.
pub fn suggested_cli(c: &Check) -> Option<Vec<String>> {
    let start = c.text.find("`ptyro ")? + 1;
    let end = start + c.text[start..].find('`')?;
    let profile = c.section.strip_prefix("profile ").map(str::trim);
    let words: Vec<String> = c.text[start..end]
        .split_whitespace()
        .skip(1)
        .map(|w| match (w, profile) {
            ("<profile>", Some(p)) => p.to_owned(),
            _ => w.to_owned(),
        })
        .collect();
    (!words.is_empty() && !words.iter().any(|w| w.starts_with('<'))).then_some(words)
}

/// The path in "`<path> is 644: …`" (the doctor's wording): everything before the mode, so a
/// path with a space in it stays whole.
fn private_path(text: &str) -> Option<&str> {
    text.rmatch_indices(" is ").find_map(|(i, _)| {
        let mode = &text[i + 4..];
        (mode.len() >= 4
            && mode.as_bytes()[..3].iter().all(u8::is_ascii_digit)
            && mode.as_bytes()[3] == b':')
            .then_some(&text[..i])
    })
}

/// A doctor check as a finding: only warnings and problems, with the fix its text implies.
pub fn finding(c: &Check) -> Option<Finding> {
    let problem = match c.level.as_str() {
        "problem" => true,
        "warn" => false,
        _ => return None,
    };
    let fix = if c.section == "tmux" {
        Some(Fix::Tmux { name: c.name.clone(), value: "on".into() })
    } else if c.name == "config mode" {
        private_path(&c.text).map(|p| Fix::Private(PathBuf::from(p)))
    } else {
        suggested_cli(c).map(Fix::Cli)
    };
    Some(Finding { problem, what: format!("{} {}", c.section, c.name), text: c.text.clone(), fix })
}

/// The shell's rc file and the line that sets up ptyro's integration in it, when it's missing:
/// prompt marks (copy-mode jumps), the live directory and each command in the sidebar.
pub fn shell_init_missing() -> Option<(PathBuf, String)> {
    let shell = std::env::var("SHELL").ok()?;
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let (rc, line) = if shell.ends_with("zsh") {
        (home.join(".zshrc"), "eval \"$(ptyro shell-init zsh)\"")
    } else if shell.ends_with("bash") {
        (home.join(".bashrc"), "eval \"$(ptyro shell-init bash)\"")
    } else if shell.ends_with("fish") {
        (home.join(".config/fish/config.fish"), "ptyro shell-init fish | source")
    } else {
        return None;
    };
    let text = std::fs::read_to_string(&rc).unwrap_or_default();
    (!text.contains("ptyro shell-init")).then(|| (rc, line.to_owned()))
}

impl App {
    /// Periodically, in the background (from `tick`).
    pub(crate) fn health_tick(&mut self, started: Instant) {
        if !self.health_open()
            && let Some(checks) = self.health.deferred.take()
        {
            self.got_health(checks);
        }
        if self.health.running || self.client.is_none() || self.opts.raw {
            return;
        }
        let due = match self.health.last {
            None => started.elapsed() >= FIRST,
            Some(t) => t.elapsed() >= EVERY,
        };
        if due {
            self.check_health();
        }
    }

    /// Run `ptyro doctor --quick --json` now.
    pub fn check_health(&mut self) {
        let Ok(exe) = std::env::current_exe() else { return };
        self.health.running = true;
        self.health.last = Some(Instant::now());
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let out = tokio::process::Command::new(exe)
                .args(["doctor", "--quick", "--json"])
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .await;
            let checks: Vec<Check> =
                out.ok().and_then(|o| serde_json::from_slice(&o.stdout).ok()).unwrap_or_default();
            let _ = tx.send(AppMsg::Health(checks));
        });
    }

    /// The mend list is on screen (its rows are numbers into the findings).
    fn health_open(&self) -> bool {
        matches!(
            self.overlay,
            Some(crate::app::Overlay::Picker { purpose: crate::app::Purpose::Health, .. })
        )
    }

    pub(crate) fn got_health(&mut self, checks: Vec<Check>) {
        self.health.running = false;
        if self.health_open() {
            self.health.deferred = Some(checks);
            return;
        }
        let mut findings: Vec<Finding> = checks.iter().filter_map(finding).collect();
        if let Some((rc, line)) = shell_init_missing() {
            findings.push(Finding {
                problem: false,
                what: "shell integration".into(),
                text: format!(
                    "not set up in {}: copy mode's prompt jumps, the live directory and each shell's last command in the sidebar need it",
                    rc.display()
                ),
                fix: Some(Fix::Append { file: rc, line }),
            });
        }
        let n = findings.len();
        self.health.findings = findings;
        if n > self.health.announced {
            let key = self.keymap.key_for("health").unwrap_or_else(|| "health".into());
            self.toast(format!("{n} thing(s) to set up or fix: {key} shows them"));
        }
        self.health.announced = n;
        self.dirty = true;
    }

    /// `M` (mend): the findings, each picked to fix.
    pub fn open_health(&mut self) {
        let items: Vec<Item> = self
            .health
            .findings
            .iter()
            .enumerate()
            .map(|(i, f)| Item {
                label: format!("{} {}", if f.problem { "✗" } else { "!" }, f.what),
                detail: match &f.fix {
                    Some(Fix::Cli(args)) => {
                        format!("{} · ⏎ runs `ptyro {}`", f.text, args.join(" "))
                    }
                    Some(Fix::Append { file, .. }) => {
                        format!("{} · ⏎ adds the line to {}", f.text, file.display())
                    }
                    Some(Fix::Private(_)) => format!("{} · ⏎ makes it private", f.text),
                    Some(Fix::Tmux { name, value }) => {
                        format!("{} · ⏎ sets {name} {value}", f.text)
                    }
                    None => f.text.clone(),
                },
                key: None,
                target: Target::Fix(i),
            })
            .collect();
        if items.is_empty() {
            self.toast(if self.health.running {
                "checking…"
            } else {
                "all good: the doctor found nothing to fix"
            });
            if !self.health.running {
                self.check_health();
            }
            return;
        }
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Health: ⏎ fixes one", items),
            purpose: Purpose::Health,
        });
    }

    /// Fix finding `i` (asking first before touching a file).
    pub(crate) fn fix(&mut self, i: usize) {
        let Some(f) = self.health.findings.get(i).cloned() else { return };
        match f.fix {
            Some(Fix::Cli(args)) => self.run_cli(&args),
            Some(Fix::Append { file, line }) => {
                self.overlay = Some(Overlay::Confirm {
                    text: format!(
                        "Add this to {}?\n\n  {line}\n\nNew shells pick it up. [y/N]",
                        file.display()
                    ),
                    action: Confirmed::Append(file, line),
                });
            }
            Some(Fix::Private(path)) => {
                use std::os::unix::fs::PermissionsExt;
                // Only a file: 600 on a directory would shut its owner out of it.
                if !path.is_file() {
                    self.toast_err(format!("{} isn't a file I can make private", path.display()));
                    return;
                }
                match std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
                    Ok(()) => {
                        self.health.findings.remove(i);
                        self.toast(format!("{} is private now (600)", path.display()));
                    }
                    Err(e) => self.toast_err(format!("{}: {e}", path.display())),
                }
            }
            Some(Fix::Tmux { name, value }) => {
                let now = std::process::Command::new("tmux")
                    .args(["set", "-g", &name, &value])
                    .status()
                    .is_ok_and(|s| s.success());
                let conf =
                    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".tmux.conf");
                self.overlay = Some(Overlay::Confirm {
                    text: format!(
                        "{}\n\nKeep it: add `set -g {name} {value}` to {}? [y/N]",
                        if now {
                            format!("set {name} {value} in this tmux")
                        } else {
                            "couldn't set it in this tmux".into()
                        },
                        conf.display()
                    ),
                    action: Confirmed::Append(conf, format!("set -g {name} {value}")),
                });
            }
            None => self.toast(f.text),
        }
    }

    /// Append `line` to `file` (confirmed).
    pub(crate) fn append_line(&mut self, file: PathBuf, line: String) {
        use std::io::Write;
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file)
            .and_then(|mut f| writeln!(f, "\n# added by ptyro\n{line}"));
        match res {
            Ok(()) => {
                self.health.findings.retain(|f| {
                    f.fix != Some(Fix::Append { file: file.clone(), line: line.clone() })
                });
                self.toast(format!("added to {}", file.display()));
            }
            Err(e) => self.toast_err(format!("{}: {e}", file.display())),
        }
    }

    /// `ptyro <args>`, full sized: its output stays readable once it's done, and `` ` `` goes
    /// back. The CLI's long tail reaches the TUI this way (UX1.20, UX1.21), interactive ones
    /// included.
    pub fn run_cli(&mut self, args: &[String]) {
        self.last_cli = Some(args.to_vec());
        let exe = std::env::current_exe()
            .map_or_else(|_| "ptyro".to_owned(), |p| p.to_string_lossy().into_owned());
        let argv: Vec<String> = std::iter::once(exe).chain(args.iter().cloned()).collect();
        let base = format!("ptyro-{}", args.first().map_or("cli", String::as_str));
        let base = ptyro_core::names::tidy(&base).unwrap_or(base);
        let name = std::iter::once(base.clone())
            .chain((2..).map(|k| format!("{base}-{k}")))
            .find(|n| self.sessions.iter().all(|s| s.name != *n))
            .expect("infinite");
        self.spawn_tool(argv, Some(name), false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(section: &str, name: &str, level: &str, text: &str) -> Check {
        Check { section: section.into(), name: name.into(), level: level.into(), text: text.into() }
    }

    #[test]
    fn findings_carry_the_fix_their_text_implies() {
        assert!(finding(&check("terminal", "TERM", "ok", "xterm")).is_none());
        let f = finding(&check(
            "daemon",
            "running",
            "warn",
            "pid 1, version 0.0.1; not this ptyro: `ptyro daemon upgrade` (sessions keep running)",
        ))
        .unwrap();
        assert_eq!(f.fix, Some(Fix::Cli(vec!["daemon".into(), "upgrade".into()])));
        let login = finding(&check(
            "profile work",
            "claude",
            "warn",
            "not logged in (`ptyro profile login <profile> claude`)",
        ))
        .unwrap();
        assert_eq!(
            login.fix,
            Some(Fix::Cli(vec!["profile".into(), "login".into(), "work".into(), "claude".into()]))
        );
        let tmux = finding(&check("tmux", "extended-keys", "warn", "is off: …")).unwrap();
        assert_eq!(tmux.fix, Some(Fix::Tmux { name: "extended-keys".into(), value: "on".into() }));
        let private =
            finding(&check("ptyro", "config mode", "warn", "/h/c.toml is 644: …")).unwrap();
        assert_eq!(private.fix, Some(Fix::Private("/h/c.toml".into())));
        // A path with a space in it is one path, not its first word.
        let spaced =
            finding(&check("ptyro", "config mode", "warn", "/Users/John Smith/c.toml is 644: …"))
                .unwrap();
        assert_eq!(spaced.fix, Some(Fix::Private("/Users/John Smith/c.toml".into())));
        // A placeholder it can't fill: no fix rather than a wrong one.
        assert!(suggested_cli(&check("x", "y", "warn", "run `ptyro thing <name>`")).is_none());
    }
}
