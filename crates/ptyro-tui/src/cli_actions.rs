//! The CLI's long tail in the TUI (ROADMAP UX1.21): each action is a `ptyro …` command whose
//! arguments come from what's selected (the session, worktree, project or change set), a pick
//! from a list, or a question; it then runs in a split (`run_cli`), so interactive commands
//! (logins, a QR code to scan, a confirmation) work there and the output stays readable.
//!
//! Template words:
//! - a literal goes as it is;
//! - `{session}`, `{worktree}`, `{project}`, `{set}`: the selected one's name;
//! - `{profile}`, `{agent}`: picked from a list;
//! - `{choose:Label:a|b|c}`: picked from those;
//! - `{ask:Label}`: typed; nothing typed leaves the word out (an optional argument);
//! - `{vars}`: `k=v, k2=v2` typed, as `--var k=v --var k2=v2`;
//! - `{where}`: on Windows, WSL, Windows or both (setup's sides, BOOT8): see [`where_runs`];
//!   elsewhere nothing.

use crate::app::{App, Overlay, PromptKind, Purpose};
use crate::palette::{Item, Palette, Target};
use crate::tree::Row;

/// `(action, template)`. Each action is in `keymap::ACTIONS` too, so the palette lists it.
pub const TEMPLATES: &[(&str, &[&str])] = &[
    ("session.replay", &["replay", "{session}"]),
    (
        "session.export",
        &[
            "export",
            "{session}",
            "--format",
            "{choose:Format:asciicast|text|ptylog}",
            "-o",
            "{ask:File to write (e.g. session.cast)}",
        ],
    ),
    (
        "session.verify",
        &[
            "verify",
            "{session}",
            "{ask:The check its turns must pass (a run script or a command; empty: the project's)}",
        ],
    ),
    ("session.message", &["msg", "send", "{session}", "{ask:Message}"]),
    ("session.lead", &["run", "lead", "{session}"]),
    ("checkpoint.list", &["checkpoint", "ls", "{session}"]),
    (
        "checkpoint.diff",
        &["checkpoint", "diff", "{session}", "{ask:Which checkpoint (its number)}", "--stat"],
    ),
    (
        "checkpoint.restore",
        &["checkpoint", "restore", "{session}", "{ask:Back to which checkpoint (its number)}"],
    ),
    ("pr.reply", &["pr", "reply", "{ask:Your reply on the PR}", "-w", "{worktree}"]),
    ("project.show", &["project", "show", "{project}"]),
    ("project.config", &["project", "config", "{project}", "--explain"]),
    ("profile.list", &["profile", "ls"]),
    ("profile.show", &["profile", "show", "{profile}"]),
    ("profile.new", &["profile", "new", "{ask:New profile's name (e.g. work)}"]),
    ("profile.remove", &["profile", "rm", "{profile}"]),
    ("profile.rename", &["profile", "rename", "{profile}", "{ask:New name}"]),
    ("profile.login", &["profile", "login", "{profile}", "{agent}"]),
    ("profile.sync", &["profile", "sync", "{profile}"]),
    ("recipe.list", &["recipe", "ls"]),
    ("recipe.show", &["recipe", "show", "{ask:Recipe's name (recipe.list lists them)}"]),
    ("recipe.run", &["recipe", "run", "{ask:Recipe's name (e.g. implement-and-review)}", "{vars}"]),
    ("run.fork", &["run", "fork", "{ask:Run id (r-…)}", "--from", "{ask:From which step}"]),
    ("leases", &["lease", "ls"]),
    ("radar", &["radar", "--project", "{project}"]),
    (
        "sets.new",
        &[
            "cs",
            "new",
            "{ask:Change set's name}",
            "--projects",
            "{ask:Its projects (a,b,c)}",
            "-p",
            "{ask:The goal}",
        ],
    ),
    ("sets.templates", &["cs", "templates"]),
    ("set.checkpoints", &["cs", "checkpoints", "{set}"]),
    ("set.restore", &["cs", "restore", "{set}", "{ask:Back to which set checkpoint (its number)}"]),
    ("set.contract", &["cs", "contract", "{set}", "{choose:Contract:show|edit|diff|history}"]),
    (
        "set.overlay",
        &[
            "cs",
            "overlay",
            "{set}",
            "{choose:Build against siblings:auto|go|rust|node|python|compose|off}",
        ],
    ),
    ("set.commit", &["cs", "commit", "{set}", "-m", "{ask:Commit message for every member}"]),
    ("set.push", &["cs", "push", "{set}"]),
    ("set.pr", &["cs", "pr", "{set}"]),
    ("set.land", &["cs", "land", "{set}"]),
    ("set.add", &["cs", "add", "{set}", "{ask:Project to add}"]),
    ("set.drop", &["cs", "drop", "{set}", "{ask:Member to drop}"]),
    ("set.remove", &["cs", "rm", "{set}"]),
    ("remote.serve", &["serve"]),
    ("remote.web", &["web", "status"]),
    ("remote.web_on", &["web", "on"]),
    ("remote.web_off", &["web", "off"]),
    ("remote.pair", &["pair"]),
    ("remote.pair.browser", &["pair", "--open"]),
    ("devices.list", &["devices", "ls"]),
    ("devices.revoke", &["devices", "revoke", "{ask:Device id (devices.list shows them)}"]),
    ("devices.push_test", &["devices", "push-test"]),
    ("config.show", &["config", "show"]),
    ("app.backup", &["backup"]),
    // Setup (BOOT1): the checklist in the split's terminal, where sudo can ask for a password.
    ("app.setup", &["setup", "{where}"]),
    ("app.install", &["install", "{ask:What to install (e.g. agy, or gemini gh)}", "{where}"]),
    // Detach first: the daemon stops, and this screen with it. It asks in the split.
    ("app.uninstall", &["uninstall"]),
];

pub fn template(action: &str) -> Option<&'static [&'static str]> {
    TEMPLATES.iter().find(|(a, _)| *a == action).map(|(_, t)| *t)
}

/// A command being put together: what's resolved, what's left.
#[derive(Debug, Clone)]
pub struct CliJob {
    pub done: Vec<String>,
    pub rest: Vec<&'static str>,
}

impl App {
    pub(crate) fn start_cli(&mut self, action: &str) {
        if let Some(t) = template(action) {
            self.cli_step(CliJob { done: Vec::new(), rest: t.to_vec() });
        }
    }

    /// The selected change set's name: the open set view's, else the sidebar's set row.
    fn current_set(&self) -> Option<String> {
        let run = self.sets_view.as_ref().and_then(|v| v.run.clone()).or_else(|| {
            match self.selected_row() {
                Some(Row::Set(r)) => Some(r),
                _ => None,
            }
        });
        let named = |r: &str| {
            self.runs
                .iter()
                .find(|x| x.state.id == r)
                .and_then(|x| x.state.changeset.as_ref())
                .map(|c| c.name.clone())
        };
        run.as_deref().and_then(named).or_else(|| {
            // Just one: that one.
            let mut sets = self.runs.iter().filter_map(|r| r.state.changeset.as_ref());
            match (sets.next(), sets.next()) {
                (Some(c), None) => Some(c.name.clone()),
                _ => None,
            }
        })
    }

    fn selected_session_name(&self) -> Option<String> {
        let id = match (self.focus, self.selected_row()) {
            (crate::app::Focus::Sidebar, Some(Row::Session(id) | Row::Quick(id))) => Some(id),
            _ => self.focused_session(),
        };
        id.and_then(|id| self.session(id)).map(|s| s.name.clone())
    }

    /// Resolve words until one needs asking; then ask (a picker or a prompt) and come back.
    pub(crate) fn cli_step(&mut self, mut job: CliJob) {
        while let Some(tok) = (!job.rest.is_empty()).then(|| job.rest.remove(0)) {
            let missing =
                |what: &str| format!("pick {what} first (in the sidebar), then run this again");
            let fill = match tok {
                "{session}" => self.selected_session_name().ok_or_else(|| missing("a session")),
                "{worktree}" => self
                    .context()
                    .1
                    .and_then(|w| self.worktree(&w).map(|w| w.qualified()))
                    .ok_or_else(|| missing("a worktree")),
                "{project}" => self
                    .context()
                    .0
                    .and_then(|p| self.project(&p).map(|p| p.slug.clone()))
                    .ok_or_else(|| missing("a project")),
                "{set}" => self.current_set().ok_or_else(|| missing("a change set")),
                "{profile}" => {
                    let items =
                        self.profiles.iter().map(|p| (p.name.clone(), p.label.clone())).collect();
                    return self.cli_pick("Which profile?", items, job);
                }
                "{agent}" => {
                    let items = ptyro_agents::BUILTIN
                        .iter()
                        .map(|(n, _)| ((*n).to_owned(), "agent".to_owned()))
                        .chain(std::iter::once(("gh".to_owned(), "the GitHub CLI".to_owned())))
                        .collect();
                    return self.cli_pick("Which agent?", items, job);
                }
                "{where}" => match where_offered(cfg!(windows), self.connect.remote.as_deref()) {
                    Some(wsl_daemon) => return self.where_pick(wsl_daemon, job),
                    None => continue,
                },
                t if t.starts_with("{choose:") => {
                    let body = t.trim_start_matches("{choose:").trim_end_matches('}');
                    let (label, opts) = body.split_once(':').unwrap_or((body, ""));
                    let items = opts.split('|').map(|o| (o.to_owned(), String::new())).collect();
                    return self.cli_pick(label, items, job);
                }
                t if t.starts_with("{ask:") || t == "{vars}" => {
                    let label = if t == "{vars}" {
                        "Variables, `k=v, k2=v2` (empty: none)".to_owned()
                    } else {
                        t.trim_start_matches("{ask:").trim_end_matches('}').to_owned()
                    };
                    let vars = t == "{vars}";
                    self.overlay = Some(Overlay::Prompt {
                        title: format!("ptyro {} — {label}", job.done.join(" ")),
                        text: String::new(),
                        kind: PromptKind::CliAsk(Box::new(job), vars),
                    });
                    return;
                }
                lit => Ok(lit.to_owned()),
            };
            match fill {
                Ok(v) => job.done.push(v),
                Err(why) => {
                    self.toast_err(why);
                    return;
                }
            }
        }
        let Some(i) = job.done.iter().position(|w| w.starts_with(WHERE)) else {
            return self.run_cli(&job.done);
        };
        let choice = job.done.remove(i)[WHERE.len()..].to_owned();
        let wsl_daemon = where_offered(cfg!(windows), self.connect.remote.as_deref()) == Some(true);
        let exe = std::env::current_exe()
            .ok()
            .and_then(|p| ptyro_core::wsl::path_in_distro(&p.to_string_lossy(), "/mnt/"));
        let runs = where_runs(&choice, wsl_daemon, exe.as_deref());
        if runs.is_empty() {
            return self.toast_err("this ptyro.exe isn't where WSL can reach it: run `ptyro setup --where windows` in a Windows terminal");
        }
        for (bin, extra) in runs {
            let mut args = job.done.clone();
            args.extend(extra);
            match bin {
                Some(bin) => self.run_cli_with(bin, &args),
                None => self.run_cli(&args),
            }
        }
    }

    /// Where setup installs, on Windows (BOOT8): the daemon's side first.
    fn where_pick(&mut self, wsl_daemon: bool, job: CliJob) {
        let wsl = match self.connect.remote.as_deref().and_then(|r| r.strip_prefix("wsl://")) {
            Some(d) if !d.is_empty() => format!("WSL ({d})"),
            _ => "WSL".to_owned(),
        };
        let mut choices = vec![
            ("wsl", wsl, "into the WSL distro (needs ptyro there)"),
            ("windows", "Windows".to_owned(), "onto Windows itself"),
            ("both", "both".to_owned(), "into WSL and onto Windows"),
        ];
        if !wsl_daemon {
            choices.swap(0, 1);
        }
        let items = choices
            .into_iter()
            .map(|(value, label, detail)| Item {
                label,
                detail: detail.to_owned(),
                key: None,
                target: Target::Path(format!("{WHERE}{value}").into()),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new("Where to install?", items),
            purpose: Purpose::CliPick(Box::new(job)),
        });
    }

    fn cli_pick(&mut self, title: &str, choices: Vec<(String, String)>, job: CliJob) {
        let items = choices
            .into_iter()
            .map(|(value, detail)| Item {
                label: value.clone(),
                detail,
                key: None,
                target: Target::Path(value.into()),
            })
            .collect();
        self.overlay = Some(Overlay::Picker {
            palette: Palette::new(title, items),
            purpose: Purpose::CliPick(Box::new(job)),
        });
    }

    /// A typed answer: the word (none when empty), or `--var` pairs.
    pub(crate) fn cli_answer(&mut self, mut job: CliJob, text: &str, vars: bool) {
        add_answer(&mut job.done, text, vars);
        self.cli_step(job);
    }
}

/// How a `{where}` pick travels through the command until it is run.
const WHERE: &str = "\0where=";

/// Whether `{where}` asks (Windows, with this computer's daemon or WSL's: `Some(true)` for
/// WSL's); `None` elsewhere, and for a daemon on another machine, where sides don't apply.
pub fn where_offered(windows: bool, remote: Option<&str>) -> Option<bool> {
    if !windows {
        return None;
    }
    match remote {
        None => Some(false),
        Some(r) if r.starts_with("wsl://") => Some(true),
        Some(r) if ptyro_client::is_local(Some(r)) => Some(false),
        Some(_) => None,
    }
}

/// What a side choice runs, one split each: `(ptyro to run, None for the daemon's; words to add)`.
/// With this computer's daemon, Windows' ptyro does both sides itself (`--where`). With WSL's,
/// a split runs in WSL: `wsl` is its own ptyro (WSL's side natively, sudo in that terminal), and
/// Windows' is `exe_in_wsl` (this ptyro.exe as WSL names it) through interop, whose console is a
/// pipe there (`--tty`: a person answers anyway); `both` is the two splits. Empty: Windows'
/// ptyro can't be reached from WSL.
pub fn where_runs(
    choice: &str,
    wsl_daemon: bool,
    exe_in_wsl: Option<&str>,
) -> Vec<(Option<String>, Vec<String>)> {
    let words = |w: &[&str]| w.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    if !wsl_daemon {
        return vec![(None, words(&["--where", choice]))];
    }
    let windows = exe_in_wsl.map(|e| (Some(e.to_owned()), words(&["--where", "windows", "--tty"])));
    match choice {
        "wsl" => vec![(None, Vec::new())],
        "windows" => windows.into_iter().collect(),
        _ => match windows {
            Some(w) => vec![(None, Vec::new()), w],
            None => Vec::new(),
        },
    }
}

/// Put a typed answer on the command: the word, or `--var` pairs. An empty answer leaves the
/// word out, and the flag that was to carry it (`-o`, `--from`) with it, so an optional
/// argument skipped never leaves `-o` dangling.
fn add_answer(done: &mut Vec<String>, text: &str, vars: bool) {
    let t = text.trim();
    if vars {
        for kv in t.split(',').map(str::trim).filter(|kv| kv.contains('=')) {
            done.push("--var".into());
            done.push(kv.to_owned());
        }
    } else if !t.is_empty() {
        done.push(t.to_owned());
    } else if done.last().is_some_and(|w| w.len() > 1 && w.starts_with('-')) {
        done.pop();
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_skipped_answer_takes_its_flag_with_it() {
        let words = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let mut done = words(&["export", "s", "--format", "text", "-o"]);
        super::add_answer(&mut done, "  ", false);
        assert_eq!(done, words(&["export", "s", "--format", "text"]));
        super::add_answer(&mut done, "out.txt", false);
        assert_eq!(done.last().map(String::as_str), Some("out.txt"));
        // A positional that is empty leaves nothing behind either.
        let mut done = words(&["cs", "new", "x"]);
        super::add_answer(&mut done, "", false);
        assert_eq!(done, words(&["cs", "new", "x"]));
        let mut done = words(&["recipe", "run", "r"]);
        super::add_answer(&mut done, "a=1, b=2", true);
        assert_eq!(done, words(&["recipe", "run", "r", "--var", "a=1", "--var", "b=2"]));
    }

    #[test]
    fn setups_sides_are_offered_on_windows_and_run_where_they_can() {
        use super::{where_offered, where_runs};
        assert_eq!(where_offered(false, None), None, "sides are Windows'");
        assert_eq!(where_offered(true, None), Some(false));
        assert_eq!(where_offered(true, Some("wsl://Ubuntu")), Some(true));
        assert_eq!(where_offered(true, Some("me@box")), None, "another machine's daemon");
        let w = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        // This computer's daemon: Windows' ptyro does it all.
        assert_eq!(where_runs("both", false, None), [(None, w(&["--where", "both"]))]);
        // WSL's: WSL's own ptyro for its side, Windows' through interop for Windows'.
        let exe = "/mnt/c/Users/me/ptyro.exe";
        assert_eq!(where_runs("wsl", true, Some(exe)), [(None, vec![])]);
        let win = (Some(exe.to_owned()), w(&["--where", "windows", "--tty"]));
        assert_eq!(where_runs("windows", true, Some(exe)), std::slice::from_ref(&win));
        assert_eq!(where_runs("both", true, Some(exe)), [(None, vec![]), win]);
        assert!(where_runs("both", true, None).is_empty());
    }

    #[test]
    fn every_template_is_an_action_and_every_word_is_known() {
        for (action, words) in super::TEMPLATES {
            assert!(crate::keymap::spec(action).is_some(), "{action} isn't in ACTIONS");
            for w in *words {
                if w.starts_with('{') {
                    assert!(
                        matches!(
                            *w,
                            "{session}"
                                | "{worktree}"
                                | "{project}"
                                | "{set}"
                                | "{profile}"
                                | "{agent}"
                                | "{vars}"
                                | "{where}"
                        ) || w.starts_with("{ask:")
                            || (w.starts_with("{choose:") && w.contains('|')),
                        "{action}: {w}"
                    );
                }
            }
        }
    }
}
