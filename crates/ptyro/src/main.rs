//! The single `ptyro` binary (SPEC §3.3, ADR 12): CLI, TUI, daemon and dev helpers.

// Native Windows was removed on 2026-11-18 (SPEC §3.4): on Windows, ptyro is the Linux build in WSL2.
#[cfg(target_os = "windows")]
compile_error!(
    "ptyro doesn't build for Windows itself: on Windows it runs inside WSL2. In a WSL terminal, \
     install it with `curl -fsSL https://github.com/nealhardesty/ptyro/releases/latest/download/install.sh | sh`, \
     or build it there with `make build`"
);

mod about;
mod agents;
mod chat;
mod checkpoint;
mod cli;
mod complete;
mod config;
mod doctor;
mod home;
mod host;
mod ingest;
mod kb;
mod linked;
mod logins;
mod machines;
mod mcp;
mod own;
#[cfg(test)]
mod parity;
mod profiles;
mod projects;
mod relay;
mod remote;
mod script;
mod serve;
mod service;
mod setup;
mod shells;
mod tls;
mod uninstall;
mod webapp;

mod wsl_conf;

use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::ArgValueCandidates;

#[derive(Parser)]
#[command(
    name = "ptyro",
    version,
    about = "A terminal-native home for your coding agents",
    long_about = "A terminal-native home for your coding agents.\n\nRun with no arguments to attach to the most recent session (or start a shell session).",
    after_help = "Sessions keep running in the background daemon when you detach (Ctrl-Q d).\n\nStart on a task: `ptyro \"fix the flaky test\"` runs your default agent in a new worktree of the repository you're in (or here, outside one) with that as its first prompt, and attaches."
)]
struct Cli {
    /// Talk to the daemon in JSON instead of MessagePack (debugging).
    #[arg(long, global = true, hide = true)]
    json_wire: bool,
    /// Use the daemon on another machine, over ssh: `[ssh://][user@]host[:port]`, or `local` for
    /// this computer's own (or set PTYRO_REMOTE; `client.default_remote` keeps one). Needs ptyro
    /// there, and a key-based login
    #[arg(long, global = true, value_name = "HOST")]
    remote: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// A task for your default agent, in quotes, in a new worktree of the repository you're in
    /// (UX1.9)
    #[arg(value_name = "TASK", num_args = 0..)]
    task: Vec<String>,
}

/// The commands alone, without the task: what several unquoted words are parsed as instead, so
/// the first one gets clap's own "unrecognized subcommand" and its did-you-mean (U-11).
#[derive(Parser)]
#[command(name = "ptyro", subcommand_required = true)]
struct Commands {
    #[arg(long, global = true, hide = true)]
    json_wire: bool,
    #[arg(long, global = true, value_name = "HOST")]
    remote: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

/// The command line. A task is one argument, quoted: several words with no command are a
/// mistyped command (`ptyro sessions list`), never a task an agent starts on.
fn parse_cli<I, T>(argv: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    use clap::error::{ContextKind, ContextValue, ErrorKind};
    let argv: Vec<std::ffi::OsString> = argv.into_iter().map(Into::into).collect();
    if let Some(e) = removed_command(&argv) {
        return Err(Cli::command().error(ErrorKind::InvalidSubcommand, e));
    }
    let cli = match Cli::try_parse_from(&argv) {
        Ok(cli) => cli,
        // A part of a command that was removed (`ptyro wt archive`, `new --verify`): said, with
        // what to use. Only once clap refused it, so it never stands in for what parses.
        Err(e) => match removed_part(&argv) {
            Some(said) => return Err(Cli::command().error(ErrorKind::InvalidSubcommand, said)),
            None => return Err(e),
        },
    };
    if cli.cmd.is_some() || cli.task.len() < 2 {
        return Ok(cli);
    }
    let mut e = match Commands::try_parse_from(&argv) {
        Err(e) => e,
        Ok(_) => Cli::command().error(ErrorKind::InvalidSubcommand, "no such command"),
    };
    let mut tips = match e.get(ContextKind::Suggested) {
        Some(ContextValue::StyledStrs(t)) => t.clone(),
        _ => Vec::new(),
    };
    tips.push(
        format!("to give your agent a task, quote it: ptyro \"{}\"", cli.task.join(" ")).into(),
    );
    e.insert(ContextKind::Suggested, ContextValue::StyledStrs(tips));
    Err(e)
}

/// Commands that were removed (the pare-down, 2026-11-17): the names they answered to, what
/// each was, and what to use instead (empty when nothing replaces it).
const REMOVED_COMMANDS: &[(&[&str], &str, &str)] = &[
    (
        &["init"],
        "it set ptyro up",
        "`ptyro setup` does what it did: your agents, accounts, preferences and repositories",
    ),
    (
        &["tour"],
        "a guided tour with a pretend agent",
        "`?` in the TUI lists the keys, and `ptyro --help` the commands",
    ),
    (
        &["backup"],
        "it saved ptyro's state to a .tar.gz",
        "`ptyro about` says where ptyro keeps its config and data, to copy while the daemon is stopped",
    ),
    (
        &["integrations"],
        "Gemini and Antigravity extensions",
        "Claude Code, Codex and OpenCode need nothing installed for ptyro",
    ),
    (
        &["cs", "changeset"],
        "change sets across several repositories",
        "a worktree per repository, `ptyro wt new`",
    ),
    (&["msg"], "mail between agents", "`ptyro send SESSION TEXT` types into a session"),
    (&["run", "runs"], "runs of recipes and fan-outs", ""),
    (&["recipe", "recipes"], "multi-step workflows", ""),
    (
        &["fanout"],
        "one task to several agents",
        "`ptyro new -a AGENT` starts each one in its own worktree",
    ),
    (
        &["compare", "pick", "iterate", "discard"],
        "a fan-out's candidates",
        "`ptyro wt ls`, and `ptyro wt rm` for the ones you don't keep",
    ),
    (
        &["gate", "gates"],
        "questions runs waited on",
        "`ptyro answer` answers an agent's permission prompt",
    ),
    (&["lease", "leases"], "file leases between agents", ""),
    (&["radar"], "overlapping changes between worktrees", ""),
    (&["pr", "watch-pr"], "pull requests", "`gh pr` creates and watches them"),
    (
        &["review"],
        "the review queue and line comments",
        "`ptyro checkpoint diff` shows what an agent changed",
    ),
    (&["verify"], "verified-done checks", ""),
    (&["scratchpad", "pad"], "the scratchpad", ""),
    (
        &["inbox"],
        "the inbox",
        "`ptyro status` says what every agent is doing, and `ptyro events` what happened",
    ),
    (
        &["adopt"],
        "it made a hand-started Claude a managed session",
        "start it with `ptyro new -a claude`",
    ),
    (&["report", "report-agent"], "a program's own state report", ""),
    (
        &["replay", "export"],
        "session recordings",
        "`ptyro read SESSION --scrollback` prints what a session shows",
    ),
    (&["convert"], "it switched an agent between chat view and its own UI", ""),
    (&["summarize"], "one-line session summaries", "`ptyro status` says what every agent is doing"),
    (&["retitle"], "it is part of rename now", "`ptyro rename SESSION --title TITLE`"),
    (
        &["open-url"],
        "it opened a toast's `ptyro://` link for native Windows' ptyro",
        "on Windows, run ptyro inside WSL",
    ),
];

/// Subcommands and flags that were removed (the pare-down, 2026-11-17) from commands that stay:
/// the commands they belonged to, what they were (`--flag` for a flag, anywhere after the
/// command), what they did, and what to use instead ("" for nothing).
const REMOVED_PARTS: &[(&[&str], &[&str], &str, &str)] = &[
    (
        &["wt", "worktree"],
        &["retitle"],
        "it is part of rename now",
        "`ptyro wt rename WT --title TITLE`",
    ),
    (
        &["wt", "worktree"],
        &["archive"],
        "archiving a worktree",
        "`ptyro wt rm WT` removes one (it refuses to lose work)",
    ),
    (
        &["wt", "worktree"],
        &["restore", "archives", "archive-rm"],
        "worktree archives",
        "`ptyro doctor` lists the ones archived before, with the git commands that bring one back",
    ),
    (
        &["wt", "worktree"],
        &["finish", "integrate", "commit", "log"],
        "landing a worktree's work",
        "git is yours: commit, merge and push in the worktree (`ptyro wt open WT -e shell`)",
    ),
    (
        &["wt", "worktree"],
        &["diff"],
        "the worktree diff",
        "`ptyro checkpoint diff` shows what an agent changed, turn by turn",
    ),
    (
        &["wt", "worktree"],
        &["lock", "unlock"],
        "locking a worktree",
        "`ptyro gc --keep-wt WT` keeps one from clean-up, and `wt rm` refuses to lose work",
    ),
    (&["wt", "worktree"], &["pair", "unpair"], "pair mode", ""),
    (
        &["wt", "worktree"],
        &["rename-branch"],
        "renaming a worktree's branch",
        "`git branch -m NEW` in the worktree",
    ),
    (
        &["project", "projects"],
        &["discover"],
        "finding repositories under a folder",
        "`ptyro project add DIR` adds one, and `ptyro setup` asks where yours are",
    ),
    (&["project", "projects"], &["suggest"], "suggested lifecycle scripts", ""),
    (&["project", "projects"], &["line-endings"], "the line-endings check", ""),
    (
        &["new", "wt", "worktree"],
        &["--take-changes"],
        "it carried your checkout's uncommitted changes into the new worktree",
        "commit them on a branch and check that out: `ptyro wt new --branch B --existing`",
    ),
    (&["new"], &["--verify"], "verified-done checks", ""),
    (&["new"], &["--structured"], "structured sessions (an agent in chat view)", "`ptyro chat`"),
    (&["new"], &["--lead", "--lead-agent", "--max-workers"], "lead agents", ""),
    (&["new"], &["--unqueued"], "the review queue", ""),
    (
        &["chat"],
        &["--preset", "--tools", "--compare"],
        "tool presets and comparing agents",
        "every chat searches the web and reads the folders you give it (`--folder DIR`)",
    ),
];

/// The error for a removed subcommand or flag of a command that stays (`ptyro wt archive x`,
/// `ptyro new --verify`), when the command line has one.
fn removed_part(argv: &[std::ffi::OsString]) -> Option<String> {
    let words: Vec<String> =
        argv.iter().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    // The command: the first word that isn't a global flag (or `--remote`'s value).
    let mut rest = words.iter().enumerate();
    let (at, command) = loop {
        match rest.next()? {
            (_, w) if w == "--remote" => {
                rest.next();
            }
            (_, w) if w.starts_with('-') => {}
            (i, w) => break (i, w.as_str()),
        }
    };
    let after = &words[at + 1..];
    let sub = after.iter().find(|w| !w.starts_with('-')).map(String::as_str);
    let (parts, what, instead) =
        REMOVED_PARTS.iter().find_map(|(commands, parts, what, instead)| {
            if !commands.contains(&command) {
                return None;
            }
            let hit = parts.iter().find(|p| match p.strip_prefix("--") {
                Some(_) => after.iter().any(|w| w == *p || w.starts_with(&format!("{p}="))),
                None => sub == Some(**p),
            })?;
            Some((*hit, *what, *instead))
        })?;
    // A flag is named with its subcommand (`wt new --take-changes`); `new` has none (its first
    // word is a task).
    let named = match sub.filter(|_| parts.starts_with("--") && command != "new") {
        Some(sub) => format!("{command} {sub} {parts}"),
        None => format!("{command} {parts}"),
    };
    let instead = if instead.is_empty() { "nothing replaces it" } else { instead };
    Some(format!("`ptyro {named}` was removed on 2026-11-17 ({what}): {instead}"))
}

/// The error for a removed command (`ptyro init`), when the command line names one, whatever
/// follows it (`ptyro inbox --json`, `ptyro pad add -`).
fn removed_command(argv: &[std::ffi::OsString]) -> Option<String> {
    let mut words = argv.iter().skip(1).map(|a| a.to_string_lossy());
    let first = loop {
        match words.next()? {
            w if w == "--remote" => {
                words.next();
            }
            w if w.starts_with('-') => {}
            w => break w,
        }
    };
    let (names, what, instead) = REMOVED_COMMANDS.iter().find(|(n, ..)| n.contains(&&*first))?;
    let instead = match names[0] {
        "scratchpad" => format!("your notes file stays where it was: {}", scratchpad_file()),
        _ if instead.is_empty() => "nothing replaces it".to_owned(),
        _ => instead.to_string(),
    };
    // Native Windows' own went later.
    let when = if names[0] == "open-url" { "2026-11-18" } else { "2026-11-17" };
    Some(format!("`ptyro {first}` was removed on {when} ({what}): {instead}"))
}

/// Where the old scratchpad kept its notes: the retired `[scratchpad] path`, else
/// `scratchpad.md` in ptyro's data directory.
fn scratchpad_file() -> String {
    let Ok(paths) = ptyro_core::paths::Paths::from_env() else { return "scratchpad.md".into() };
    let text =
        std::fs::read_to_string(ptyro_core::config::Config::path(&paths)).unwrap_or_default();
    let set = toml::from_str::<toml::Table>(&text)
        .ok()
        .and_then(|t| Some(t.get("scratchpad")?.get("path")?.as_str()?.to_owned()));
    set.map_or_else(|| paths.data.join("scratchpad.md"), |p| ptyro_core::config::expand_home(&p))
        .display()
        .to_string()
}

#[derive(Subcommand)]
enum Cmd {
    /// Start a new session (attaches unless -d)
    New {
        /// Don't attach; print the new session's handle
        #[arg(short, long)]
        detach: bool,
        /// Session name (default: a friendly random name)
        #[arg(short, long)]
        name: Option<String>,
        /// Working directory (default: the current one)
        #[arg(short = 'C', long)]
        cwd: Option<PathBuf>,
        /// Size for a detached session, COLSxROWS (attached sessions use your terminal's size)
        #[arg(long, value_parser = cli::parse_size)]
        size: Option<ptyro_core::TermSize>,
        /// Print the session as JSON (doesn't attach)
        #[arg(long)]
        json: bool,
        /// Start an agent: claude, codex, opencode, or one from [agents]; `claude@work`
        /// picks a profile too
        #[arg(short, long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        /// Run under this profile (default: the worktree's, the project's pin, the workspace's)
        #[arg(long, add = ArgValueCandidates::new(complete::profiles))]
        profile: Option<String>,
        /// Quick new: another of the agent this worktree last started (its profile and
        /// arguments too); the project's default agent, else `[agents] default`, when none has
        #[arg(long, conflicts_with_all = ["agent", "argv", "new_wt", "pr", "issue"])]
        again: bool,
        /// Run in this worktree (`brisk-rook`, `myrepo/@root`)
        #[arg(long, conflicts_with = "new_wt", add = ArgValueCandidates::new(complete::worktrees))]
        wt: Option<String>,
        /// Create a new worktree (optionally named) and run there
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        new_wt: Option<String>,
        /// Project for --new-wt (default: the one containing the current directory)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// Run under a profile other than the worktree's or the project's pin
        #[arg(long)]
        override_pin: bool,
        /// Answer yes to a profile guard's question
        #[arg(short, long)]
        yes: bool,
        /// Type this prompt into the agent once it's ready (`@name`: a project prompt snippet)
        #[arg(long, conflicts_with = "prompt_file")]
        prompt: Option<String>,
        /// Read the prompt from a file
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        /// What it's for, for people to read (default: the prompt); the name stays its address
        #[arg(long)]
        title: Option<String>,
        /// In a new worktree checked out from this pull request (number or URL)
        #[arg(long, value_parser = parse_number, conflicts_with_all = ["wt", "issue"])]
        pr: Option<u32>,
        /// In a new worktree for this GitHub issue (number or URL): its branch and the agent's
        /// first prompt come from the issue
        #[arg(long, conflicts_with = "wt")]
        issue: Option<String>,
        /// Private: the knowledge base keeps nothing of it (the agent keeps its own history)
        #[arg(long)]
        private: bool,
        /// Keep it, though `[kb] private` is on
        #[arg(long, conflicts_with = "private")]
        no_private: bool,
        /// The command to run (default: your $SHELL); with --agent, extra agent arguments
        #[arg(last = true)]
        argv: Vec<String>,
    },
    /// Attach to a session (default: the most recent)
    #[command(alias = "a")]
    Attach {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: Option<String>,
        /// Open this workspace (default: the session's, else the last one)
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
        /// Just this session, full screen: no sidebar, no status line
        #[arg(long)]
        raw: bool,
        /// Watch without typing or resizing (another client keeps control)
        #[arg(long)]
        view_only: bool,
        /// Started in an older TUI's place after an update (`FROM:TO`, SPEC §24.8a)
        #[arg(long, hide = true, value_name = "FROM:TO")]
        restarted: Option<String>,
        /// Open it in a new tab of this terminal instead (tmux, Windows Terminal)
        #[arg(long, conflicts_with = "pane")]
        tab: bool,
        /// Open it in a new pane beside this one instead (tmux, Windows Terminal)
        #[arg(long)]
        pane: bool,
    },
    /// List sessions
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
        /// Include sessions from earlier daemon runs (from the store)
        #[arg(long)]
        all: bool,
        /// Only sessions in this state or status (the @ selectors' words): live (running), active
        /// (working, blocked or running a command), exited, dormant, working, blocked, idle,
        /// done, error (errored)
        #[arg(long, value_parser = parse_state)]
        status: Option<ptyro_core::selector::State>,
        /// Only sessions of this project
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// Only sessions whose home is this workspace
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
        /// Also the agents answering chats (hidden otherwise)
        #[arg(long)]
        chats: bool,
    },
    /// Kill sessions now: SIGKILL, no chance to clean up. They stay listed (`rm` forgets them).
    /// SESSION may be a selector: `@all`, `@live`, `@exited`, `@blocked`, `build-*`, `a,b`…
    Kill {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
        /// Send this signal number instead of SIGKILL (9)
        #[arg(short, long)]
        signal: Option<i32>,
        /// Does nothing: kill always keeps the session listed now (it used to remove it)
        #[arg(long, hide = true)]
        keep: bool,
    },
    /// Stop sessions and keep them listed: an agent's turn is interrupted and it gets a moment
    /// to exit, then the process gets HUP, TERM and KILL
    Stop {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
    },
    /// Start exited sessions' commands again, in place
    Restart {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
    },
    /// Forget ended sessions (exited or dormant): they leave the list, and a dormant agent's
    /// conversation is forgotten (asked first unless --yes). A running one is refused, unless
    /// `--stop`. Each is tried; a failure is said and the rest go on
    Rm {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
        /// Stop a running one first (as `stop`), then forget it
        #[arg(long)]
        stop: bool,
        /// Don't ask before forgetting a dormant agent
        #[arg(short, long)]
        yes: bool,
    },
    /// Rename a session, or retitle it: its name is its address (what you type to find it), its
    /// title is what people read (`--title`)
    Rename {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        /// The new name
        #[arg(required_unless_present = "title")]
        name: Option<String>,
        /// The new title, for people to read; `--title ""` clears it (it then shows what its
        /// program titles its window)
        #[arg(long)]
        title: Option<String>,
    },
    /// Print a session's screen (or scrollback)
    Read {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        /// The visible screen (the default)
        #[arg(long, conflicts_with = "scrollback")]
        screen: bool,
        /// The scrollback above the screen: what's kept in memory, or N lines (which can reach
        /// back into the recording)
        #[arg(long, num_args = 0..=1)]
        scrollback: Option<Option<u32>>,
        /// Keep colours and attributes (SGR)
        #[arg(long, conflicts_with = "plain")]
        ansi: bool,
        /// Plain text (the default)
        #[arg(long)]
        plain: bool,
        /// The text plus the session's status, as JSON
        #[arg(long)]
        json: bool,
    },
    /// Type text into sessions: `ptyro send S… TEXT` (Enter is pressed afterwards unless
    /// --no-enter)
    Send {
        /// SESSION… then TEXT (a session may be a selector: `@blocked`, `build-*`, `a,b`)
        #[arg(required = true, value_name = "SESSION… TEXT", add = ArgValueCandidates::new(complete::sessions))]
        args: Vec<String>,
        #[arg(long)]
        no_enter: bool,
        /// Send keys: `\e` Esc, `\r` Enter, `\t` Tab, `\xHH` any byte (no Enter added)
        #[arg(long)]
        raw: bool,
        /// Wait until each agent is ready for input before typing (up to 2m, or `--wait-ready 5m`).
        /// Only a duration is taken as its value, so a misplaced flag fails instead of eating
        /// the text.
        #[arg(long, num_args = 0..=1, default_missing_value = "2m", value_name = "TIMEOUT", value_parser = duration_arg)]
        wait_ready: Option<String>,
        /// Then wait for the turn to end and print how: idle, blocked, stalled, exited
        #[arg(long)]
        wait: bool,
        /// With --wait: give up after this long (exit 7)
        #[arg(long, requires = "wait")]
        timeout: Option<String>,
        /// With --wait: "stalled" after this long without output while working
        #[arg(long, requires = "wait")]
        stall: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Type the same text into several sessions, each once it's ready
    Broadcast {
        /// SESSION… then TEXT
        #[arg(required = true, value_name = "SESSION… TEXT", add = ArgValueCandidates::new(complete::sessions))]
        args: Vec<String>,
        #[arg(long)]
        no_enter: bool,
        /// How long to wait for each to be ready
        #[arg(long, default_value = "2m")]
        timeout: String,
    },
    /// Wait until sessions reach a state (exit 0), or time out (exit 7)
    Wait {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
        /// working, blocked, idle, done, error (errored), exit, any-attention (idle and done mean
        /// the turn ptyro last typed has ended; exit, that it stopped running, hibernating too)
        #[arg(long = "for", default_value = "idle", value_parser = script::condition())]
        condition: String,
        /// Instead: until this regex appears on the screen
        #[arg(long = "match", value_name = "REGEX")]
        pattern: Option<String>,
        /// With --match: search the scrollback above the screen too, as `read --scrollback [N]`
        /// shows it (what's kept in memory, or N lines)
        #[arg(long, requires = "pattern", num_args = 0..=1)]
        scrollback: Option<Option<u32>>,
        /// Give up after this long (`30s`, `10m`; default: never)
        #[arg(long)]
        timeout: Option<String>,
        /// Return as soon as one session gets there (default: all of them)
        #[arg(long, conflicts_with = "all")]
        any: bool,
        #[arg(long)]
        all: bool,
        /// Report "stalled" (exit 7) if a working session prints nothing for this long
        #[arg(long)]
        stall: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// What happened: session events (newest last); --follow streams them live as NDJSON
    Events {
        #[arg(short, long)]
        follow: bool,
        /// Only this session's
        #[arg(long, add = ArgValueCandidates::new(complete::sessions))]
        session: Option<String>,
        /// How many past events (without --follow)
        #[arg(short, long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Checkpoints: the worktree after every agent turn; diff or restore any of them
    #[command(alias = "checkpoints", alias = "ckpt")]
    Checkpoint {
        #[command(subcommand)]
        cmd: checkpoint::CheckpointCmd,
    },
    /// MCP server with the knowledge base's tools (`recall`, `recall_read`) for an agent's
    /// session, and the chat history tools (`chat_search`, `chat_read`, `chat_list`) for a
    /// chat's agent (started for you)
    Mcp {
        /// The chat history tools, as a chat's agent gets them (a bare `ptyro mcp` in an
        /// agent's session serves the knowledge base's)
        #[arg(long, hide = true)]
        chat: bool,
    },
    /// Bridge stdin/stdout to this machine's daemon (what `--remote` runs over ssh)
    Proxy,
    /// Your workspace's SHELLS: numbered terminals (any command) that belong to no worktree,
    /// `Alt-1…9` in the TUI. `ptyro shells new -- htop`, `ptyro shells go 2`; every session
    /// command takes `%N` (`ptyro send %3 make`)
    #[command(args_conflicts_with_subcommands = true)]
    Shells {
        #[command(subcommand)]
        cmd: Option<shells::ShellsCmd>,
        #[command(flatten)]
        ls: shells::LsArgs,
    },
    /// Your worktree's quick terminal: one shell per worktree (or directory), reused, which the
    /// TUI shows as a drawer under the pane (`C-q C-t`). Your workspace's numbered shells are
    /// `ptyro shells`. Attaches, starting it if there's none
    Shell {
        /// The worktree (default: the one you're in)
        #[arg(add = ArgValueCandidates::new(complete::worktrees))]
        worktree: Option<String>,
        /// Print the session as JSON instead of attaching (starting it if there's none)
        #[arg(long, conflicts_with = "read")]
        json: bool,
        /// Print its screen instead, what you'd see: `ptyro shell --read` from an agent shows
        /// it the error in your terminal. Never starts one
        #[arg(long)]
        read: bool,
        /// With --read: the scrollback above the screen too, as `read --scrollback [N]` shows it
        #[arg(long, requires = "read", num_args = 0..=1)]
        scrollback: Option<Option<u32>>,
    },
    /// Chats with your agents: `ptyro chat "a question"` asks and keeps it (the CHATS section of
    /// the TUI and the web show it too); `ptyro chat ask` answers once and keeps nothing. A
    /// question whose first word is a subcommand (`show me how…`, `find`, `set`) needs quotes or
    /// `ptyro chat new`
    #[command(args_conflicts_with_subcommands = true)]
    Chat {
        #[command(subcommand)]
        cmd: Option<chat::ChatCmd>,
        /// A new chat's first message and options (the same as `ptyro chat new …`)
        #[command(flatten)]
        new: chat::NewArgs,
    },
    /// Search everything ptyro kept, here and in your chats: what you asked your agents, what
    /// they answered and did. `ptyro recall retry policy`; `ptyro kb show` opens a hit
    Recall {
        #[command(flatten)]
        args: kb::RecallArgs,
    },
    /// Read a folder or a file into the knowledge base, and keep reading it as it changes: its
    /// documents, its files' names, its source comments, its commits. `ptyro ingest ~/notes`;
    /// `ptyro ingest --history` reads what your agents kept before ptyro did
    Ingest {
        #[command(flatten)]
        args: ingest::IngestArgs,
    },
    /// The knowledge base: what ptyro keeps of your work, on this computer only. Plain `ptyro
    /// kb` says whether it is on and what it holds
    #[command(args_conflicts_with_subcommands = true)]
    Kb {
        #[command(subcommand)]
        cmd: Option<kb::KbCmd>,
        /// The status as JSON
        #[arg(long)]
        json: bool,
    },
    /// Internal: the web app's server itself, which `ptyro web on` has the daemon run (and a
    /// machine's link reaches). Run by hand, it serves until Ctrl-C
    #[command(hide = true)]
    Serve {
        /// Who can reach it: localhost (this computer only), tailscale (also your tailnet) or
        /// open (every network interface). Default `[remote] access`. Always HTTPS
        #[arg(long, value_parser = parse_access)]
        access: Option<ptyro_core::config::Access>,
        /// The port (default `[remote] port`, 7091)
        #[arg(long)]
        port: Option<u16>,
        /// Your own certificate (a PEM chain, leaf first) instead of the one ptyro makes; with
        /// --key
        #[arg(long, requires = "key")]
        cert: Option<PathBuf>,
        /// The key for --cert (PEM)
        #[arg(long, requires = "cert")]
        key: Option<PathBuf>,
        /// Internal: plain HTTP on this computer only (the tests)
        #[arg(long, hide = true)]
        loopback_http: bool,
        /// Internal: started by the daemon (`[remote] serve = true`)
        #[arg(long, hide = true)]
        managed: bool,
    },
    /// The web app for your phone: have the daemon run its server (`on`), stop it (`off`), or say
    /// how it is (`status`, the default). Who can reach it is `--access` (`remote.access`)
    Web {
        #[arg(default_value = "status", value_parser = ["on", "off", "status"])]
        action: String,
        /// Who can reach it when turning it on: tailscale (this computer and your tailnet),
        /// localhost (this computer only) or open (every network interface). Default: what the
        /// config says, else tailscale if it is up, else localhost. (`--via` is the old name;
        /// here = localhost, lan = open)
        #[arg(long, visible_alias = "via", value_parser = parse_access)]
        access: Option<ptyro_core::config::Access>,
        /// With `on`: then show the pairing QR code for a phone
        #[arg(long)]
        pair: bool,
        #[arg(long)]
        json: bool,
    },
    /// Pair a phone or browser: prints a one-time QR code that opens the web app signed in
    Pair {
        /// What to call it in `ptyro devices` (default: phone; with --open, browser)
        #[arg(short, long)]
        name: Option<String>,
        /// (The name, the old way: `ptyro pair NAME`)
        #[arg(hide = true, conflicts_with = "name")]
        name_arg: Option<String>,
        /// read, approve, control or full (default: full, with a warning, since the whole web app
        /// needs it; name a scope for less)
        #[arg(long)]
        scope: Option<String>,
        /// The address the QR code points at (default: `[remote] public_url`, your Tailscale
        /// HTTPS name, or the listen address)
        #[arg(long)]
        url: Option<String>,
        /// Print the device and its link as JSON (no QR code)
        #[arg(long)]
        json: bool,
        /// Open the link in this computer's own browser instead, pairing that browser (inside
        /// WSL, the Windows one)
        #[arg(long)]
        open: bool,
    },
    /// Paired devices
    Devices {
        #[command(subcommand)]
        cmd: Option<DevicesCmd>,
    },
    /// Other machines whose workspaces show here beside yours (`laptop2:work`)
    Machine {
        #[command(subcommand)]
        cmd: Option<MachineCmd>,
    },
    /// Carry the protocol to another machine's `ptyro serve` (what a machine's link runs)
    #[command(hide = true)]
    MachineBridge {
        #[arg(long)]
        url: String,
        #[arg(long)]
        fingerprint: Option<String>,
    },
    /// Registered git repositories
    #[command(alias = "projects")]
    Project {
        #[command(subcommand)]
        cmd: projects::ProjectCmd,
    },
    /// Workspaces: named groups of projects with a profile and a saved view
    #[command(alias = "workspace")]
    Ws {
        #[command(subcommand)]
        cmd: projects::WsCmd,
    },
    /// Worktrees: isolated checkouts of a project, each on its own branch
    #[command(alias = "worktree")]
    Wt {
        #[command(subcommand)]
        cmd: projects::WtCmd,
    },
    /// Profiles: one identity (accounts, git author, keys) per workspace
    #[command(alias = "profiles")]
    Profile {
        #[command(subcommand)]
        cmd: profiles::ProfileCmd,
    },
    /// List stale worktrees and offer to remove them (never without asking)
    Gc {
        /// Only list them
        #[arg(long)]
        dry_run: bool,
        /// List them as JSON (implies --dry-run)
        #[arg(long)]
        json: bool,
        /// Only scratch shells' directories left behind (SHELLS); with --json, those as JSON
        #[arg(long)]
        scratch: bool,
        /// Keep this stale worktree: gc leaves it out until it is unused for another
        /// `worktrees.stale_days`, or its branch moves on (repeatable)
        #[arg(long = "keep-wt", value_name = "WT", add = ArgValueCandidates::new(complete::worktrees))]
        keep_wt: Vec<String>,
    },
    /// Show, check or edit the configuration (~/.config/ptyro/config.toml)
    Config {
        #[command(subcommand)]
        cmd: ConfigCmd,
    },
    /// Print the shell completion script: `source <(ptyro completions zsh)` in ~/.zshrc
    Completions {
        /// bash, zsh, fish, elvish or powershell
        shell: String,
    },
    /// Set ptyro up: the checklist to install coding agents (Claude Code, Codex, OpenCode) and
    /// the tools they need (git, the GitHub CLI, Node.js), their logins, then your accounts,
    /// preferences and repositories, written to your config
    Setup {
        /// Ask nothing: install what --agents and --tools name (with neither, the defaults: Claude
        /// Code, Codex, git, GitHub CLI). A step that needs administrator rights runs only if it
        /// can without a password prompt; otherwise it is reported with its command
        #[arg(short, long)]
        yes: bool,
        /// The agents to tick: claude, codex, opencode (comma-separated; `none`)
        #[arg(long, value_delimiter = ',', value_name = "NAMES")]
        agents: Option<Vec<String>>,
        /// The tools to tick: git, gh, node (comma-separated; `none`)
        #[arg(long, value_delimiter = ',', value_name = "NAMES")]
        tools: Option<Vec<String>>,
        /// Show the checklist and what would run; change nothing
        #[arg(long)]
        dry_run: bool,
        /// With --dry-run, the plan as JSON; otherwise each step's progress as a JSON line
        #[arg(long)]
        json: bool,
        /// Removed with native Windows (2026-11-18): says so
        #[arg(long = "where", value_name = "WHERE", hide = true)]
        place: Option<String>,
        /// With --yes, move a ticked agent installed another way (npm, Homebrew) to the
        /// recommended installer; without it, --yes leaves such an agent as it is
        #[arg(long = "move")]
        move_: bool,
    },
    /// Install agents or tools by name, with what they need first: `ptyro install codex`,
    /// `ptyro install opencode gh`. An agent installed another way (npm, Homebrew) is
    /// offered the move to the recommended installer, which asks first; with --yes it is left as
    /// it is (updated the way it's installed) unless --move
    Install {
        /// claude, codex, opencode, git, gh, node
        #[arg(required = true, value_name = "NAME")]
        names: Vec<String>,
        /// Don't ask first; a step that needs administrator rights runs only if it can without a
        /// password prompt
        #[arg(short, long)]
        yes: bool,
        /// Show what would run; change nothing
        #[arg(long)]
        dry_run: bool,
        /// With --dry-run, the plan as JSON; otherwise each step's progress as a JSON line
        #[arg(long)]
        json: bool,
        /// Removed with native Windows (2026-11-18): says so
        #[arg(long = "where", value_name = "WHERE", hide = true)]
        place: Option<String>,
        /// The person said yes already, in the TUI's checklist (BOOT2): no question first;
        /// administrator rights are still asked for here, and the logins walked through
        #[arg(long, hide = true)]
        confirmed: bool,
        /// Move a named agent installed another way (npm, Homebrew) to the recommended
        /// installer without asking (with --yes): the new one is installed and checked first,
        /// then the old copy removed; settings and logins stay
        #[arg(long = "move")]
        move_: bool,
    },
    /// Update agents, and ptyro itself, when a newer version is out (or those named), each the
    /// way it was installed: `ptyro update`, `ptyro update codex`, `ptyro update ptyro`;
    /// `--check` only looks
    Update {
        /// claude, codex, opencode, ptyro (default: every one with a newer version,
        /// less what `updates.ignore` skips)
        #[arg(value_name = "NAME")]
        names: Vec<String>,
        /// Only look for newer versions now and say what's out of date (changes nothing)
        #[arg(long)]
        check: bool,
        /// Don't ask first
        #[arg(short, long)]
        yes: bool,
        /// With --check, what's out of date as JSON; otherwise each step's progress as a JSON
        /// line
        #[arg(long)]
        json: bool,
        /// Only this machine: not the machines linked to it that run an older ptyro (which are
        /// otherwise offered too, each updating itself)
        #[arg(long)]
        here: bool,
    },
    /// Log in to the agents and the GitHub CLI, one account each: each tool's own sign-in, in
    /// this terminal, confirmed by its own status command (ptyro never sees a credential)
    Login {
        /// claude, codex, opencode, gh (default: every one installed that isn't
        /// logged in)
        #[arg(value_name = "NAME")]
        names: Vec<String>,
        /// Only say where each stands (exit 1 when one is logged out)
        #[arg(long)]
        check: bool,
        /// Where each stands, as JSON (implies --check)
        #[arg(long)]
        json: bool,
        /// Removed with native Windows (2026-11-18): says so
        #[arg(long = "where", value_name = "WHERE", hide = true)]
        place: Option<String>,
    },
    /// Which ptyro this is: version, repository, licence, build, where things live, whether the
    /// daemon and the web app run, and a block to paste into a bug report
    About {
        #[arg(long)]
        json: bool,
    },
    /// Diagnose: agents and logins per profile, their hooks, terminal, tmux, daemon, git
    Doctor {
        /// Skip the hook self-test (it runs a fake agent through each agent's hooks)
        #[arg(long)]
        quick: bool,
        #[arg(long)]
        json: bool,
        /// Then make the fixes it names that change a config file, each file copied aside first
        #[arg(long, conflicts_with = "json")]
        fix: bool,
    },
    /// Take ptyro off this computer: stops the daemon and every session, removes its worktrees,
    /// config, data, login service and shell lines, and the binary when it was
    /// installed by hand. Shows what goes and what stays first; your repositories are untouched
    Uninstall {
        /// Show the plan and stop
        #[arg(long)]
        dry_run: bool,
        /// Don't ask "uninstall? [y/N]" (work that would be lost is still asked about)
        #[arg(short, long)]
        yes: bool,
        /// For scripts that mean it: go ahead even though worktrees have uncommitted changes
        #[arg(long)]
        delete_work: bool,
        /// Keep the config directory
        #[arg(long)]
        keep_config: bool,
        /// Keep the data directory (history, profiles) and so the worktrees
        #[arg(long)]
        keep_data: bool,
        /// Don't remove the ptyro program itself
        #[arg(long)]
        keep_binary: bool,
        /// Also delete the notes you and your agents wrote (the knowledge base's notes
        /// folder, which is otherwise left where it is)
        #[arg(long)]
        delete_notes: bool,
        /// Also delete the snapshots under refs/ptyro/ in your repositories (archived and
        /// taken-over work)
        #[arg(long)]
        purge_refs: bool,
        #[arg(long)]
        json: bool,
    },
    /// Internal: what the line `ptyro setup` adds to a shell's rc file runs: the tools it
    /// installed on PATH, and the shell integration ptyro's own shells get by themselves
    #[command(hide = true)]
    ShellInit {
        #[arg(value_enum)]
        shell: cli::Shell,
    },
    /// Manage the background daemon
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// What every agent is doing (working, blocked, idle…); with a session, its details
    Status {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: Option<String>,
        /// Why it's in this state: the status engine's recent decisions
        #[arg(long)]
        explain: bool,
        #[arg(long)]
        json: bool,
    },
    /// Relaunch dormant agents into their conversations
    Resume {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
    },
    /// Stop idle agents, keeping them resumable
    Hibernate {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
    },
    /// Answer an agent's permission prompt: allow, always or deny
    Answer {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        #[arg(value_parser = ["allow", "always", "deny"])]
        decision: String,
    },
    /// Silence notifications for a session, `project:<p>` or `workspace:<w>` (its row still shows
    /// when it wants you)
    Mute {
        target: String,
        /// For this long (`30m`, `2h`); default until unmuted
        #[arg(long = "for")]
        duration: Option<String>,
        /// Unmute instead
        #[arg(long)]
        off: bool,
    },
    /// Everything, ranked: what needs you, what was left hanging, what is being worked on
    /// and what is quiet, a line a project, with every profile's usage. `ptyro home <project>`
    /// unfolds one: where it was left off, what happened there, the files it touched most
    Home {
        /// Unfold this project (its name, or the start of it)
        project: Option<String>,
        #[arg(long)]
        json: bool,
        /// Put the project's next thing away until it changes (only what was left hanging)
        #[arg(long, requires = "project")]
        put_away: bool,
        /// Bring back everything that was put away
        #[arg(long)]
        restore: bool,
    },
    /// Rate limits and usage per profile (Claude, Codex)
    Usage {
        #[arg(long)]
        json: bool,
    },
    /// Notifications: `ptyro notify test` sends one through every channel and sink
    Notify {
        #[command(subcommand)]
        cmd: agents::NotifyCmd,
    },
    /// Internal: called by agent hooks (SPEC §8.3)
    #[command(hide = true)]
    Hook {
        adapter: String,
        event: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Internal: Claude's statusLine command (usage meters)
    #[command(hide = true)]
    StatuslineRelay,
    /// Built-in fake GitHub CLI for tests (M6); debug builds only
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    FakeGh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Built-in fake installer for setup's tests (BOOT1); debug builds only
    #[cfg(debug_assertions)]
    #[command(hide = true)]
    FakeInstaller {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Built-in fake agent for tests and `ptyro doctor` (see SPEC §18.2)
    #[command(hide = true)]
    FakeAgent {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Print the config file's path
    Path,
    /// Validate the config file
    Check {
        #[arg(long)]
        json: bool,
    },
    /// Print the effective configuration (defaults merged in)
    Show {
        /// (The default: what ptyro uses, defaults filled in)
        #[arg(long)]
        effective: bool,
        #[arg(long)]
        json: bool,
    },
    /// Open the config file in $EDITOR (at a setting, if you name one), then validate it
    Edit {
        /// Open at this setting's line, when the file sets it
        #[arg(add = ArgValueCandidates::new(complete::settings))]
        key: Option<String>,
    },
    /// List the settings with their values and where each comes from
    List {
        /// Only this section (`ui`, `remote`…) or keys starting with this
        section: Option<String>,
        /// Only what is set differently from the default
        #[arg(long)]
        changed: bool,
        /// A repository's settings (`.ptyro.toml`) instead of your own: its name or path (`.` for
        /// the one you're in)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Print one setting's value
    Get {
        #[arg(add = ArgValueCandidates::new(complete::settings))]
        key: String,
        /// Say what it is, where it comes from and what it does
        #[arg(long)]
        explain: bool,
        /// A repository's value instead of your own: its name or path (`.` for the one you're in)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Change a setting: checked first, the file's comments kept, applied live where it can be
    Set {
        #[arg(add = ArgValueCandidates::new(complete::settings))]
        key: String,
        /// The value: `on`/`off`, a number, `30s`, text; several words make a list or a command
        /// (after `--` when one starts with a dash)
        #[arg(required = true, num_args = 1..)]
        value: Vec<String>,
        /// Into a repository's `.ptyro.local.toml` (yours), or its `.ptyro.toml` with --shared:
        /// its name or path (`.` for the one you're in)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// With --project: the committed `.ptyro.toml` instead of your `.ptyro.local.toml`
        #[arg(long, requires = "project")]
        shared: bool,
        /// Copy the file aside first (`<file>.bak-<when>`): how a doctor's fix is made
        #[arg(long, hide = true)]
        backup: bool,
    },
    /// Put a setting back to its default (remove it from the file)
    Unset {
        #[arg(required = true, add = ArgValueCandidates::new(complete::settings))]
        keys: Vec<String>,
        /// In a repository's file, as for `set` (`.` for the one you're in)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        #[arg(long, requires = "project")]
        shared: bool,
        /// Copy the file aside first (`<file>.bak-<when>`): how a doctor's fix is made
        #[arg(long, hide = true)]
        backup: bool,
    },
    /// Add an agent, editor, key binding, notification sink, run script or prompt
    Add {
        /// agents, editors, keys, notifications.sinks (project: scripts.run, prompts)
        #[arg(add = ArgValueCandidates::new(complete::collections))]
        collection: String,
        /// Its name (a key binding's key; a sink has none)
        name: Option<String>,
        /// Its settings, as FIELD=VALUE (`command="aider --model sonnet"`); a binding's action
        /// or a prompt's text is `value=…`; a sink's `type=ntfy`
        fields: Vec<String>,
        /// In a repository's file, as for `set` (`.` for the one you're in)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        #[arg(long, requires = "project")]
        shared: bool,
    },
    /// Remove one of those, and everything in it. Asks first unless --yes
    Rm {
        #[arg(add = ArgValueCandidates::new(complete::collections))]
        collection: String,
        /// Its name (a sink's position, from `config list notifications`)
        name: String,
        /// In a repository's file, as for `set` (`.` for the one you're in)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        #[arg(long, requires = "project")]
        shared: bool,
        /// Don't ask
        #[arg(short, long)]
        yes: bool,
        /// Copy the file aside first (`<file>.bak-<when>`): how a doctor's fix is made
        #[arg(long, hide = true)]
        backup: bool,
    },
    /// Give one of those another name
    Rename {
        #[arg(add = ArgValueCandidates::new(complete::collections))]
        collection: String,
        from: String,
        to: String,
        /// In a repository's file, as for `set` (`.` for the one you're in)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        #[arg(long, requires = "project")]
        shared: bool,
    },
    /// Print every setting's description as JSON, for tools that build forms
    Schema {
        /// The settings a repository's `.ptyro.toml` takes instead of yours (the same for every
        /// repository: `-p .`)
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
    },
}

#[derive(Subcommand)]
enum MachineCmd {
    /// List them, their state and their workspaces (default)
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Mirror another machine: `ssh://[user@]host[:port]`, or the code `ptyro machine pair`
    /// printed on it. It must answer, and run this version (unless --force)
    Add {
        target: String,
        /// What its names carry here (default: what it calls itself)
        #[arg(short, long)]
        name: Option<String>,
        /// Keep it even if it doesn't answer now (over ssh: a claimed code is always kept)
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Stop mirroring it (nothing changes on it). Asks first unless --yes
    Rm {
        #[arg(add = ArgValueCandidates::new(complete::machines))]
        name: String,
        /// Don't ask
        #[arg(short, long)]
        yes: bool,
    },
    /// Don't show one of its workspaces here (nothing changes on it)
    Hide {
        #[arg(add = ArgValueCandidates::new(complete::machines))]
        name: String,
        workspace: String,
    },
    /// Show a hidden workspace of it again
    Unhide {
        #[arg(add = ArgValueCandidates::new(complete::machines))]
        name: String,
        workspace: String,
    },
    /// Try its link again now
    Retry {
        #[arg(add = ArgValueCandidates::new(complete::machines))]
        name: String,
    },
    /// Have those machines update their own ptyro (each downloads and checks the release there
    /// and upgrades its daemon live); none named: every linked machine behind this one's version
    Update {
        #[arg(add = ArgValueCandidates::new(complete::machines))]
        names: Vec<String>,
        /// Every linked machine behind this one's version (what no name means too)
        #[arg(long)]
        all: bool,
        /// Don't ask first
        #[arg(short, long)]
        yes: bool,
        /// Each machine's answer as a JSON line
        #[arg(long)]
        json: bool,
    },
    /// Print a code for another machine to mirror this one (`ptyro machine add CODE` there)
    Pair {
        /// The address it reaches this machine at (default: this web app server's)
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Subcommand)]
enum DevicesCmd {
    /// List paired devices (default)
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Revoke a device: its token stops working at once, connections close. Asks first unless
    /// --yes
    Revoke {
        id: String,
        /// Don't ask
        #[arg(short, long)]
        yes: bool,
    },
    /// Send a test push notification to every subscribed device
    PushTest,
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Start the daemon in the background (normally automatic)
    Start {
        /// Stay in this terminal and log to stderr too (Ctrl-C stops it)
        #[arg(long)]
        foreground: bool,
        /// Internal: started by a client; detach from its terminal
        #[arg(long, hide = true)]
        spawned: bool,
        /// Removed on 2026-11-17: `ptyro web on` (it says so)
        #[arg(long, hide = true)]
        serve: bool,
    },
    /// Stop the daemon
    Stop {
        /// Also stop running sessions (they are lost; resumable agents hibernate)
        #[arg(long)]
        force: bool,
        /// Hibernate resumable agents first (they come back dormant); refuse if anything else
        /// runs, unless --force
        #[arg(long)]
        hibernate: bool,
    },
    /// Stop the daemon and start a new one (sessions as for `stop`)
    Restart {
        #[arg(long)]
        force: bool,
        #[arg(long)]
        hibernate: bool,
    },
    /// Start the daemon at login: a systemd user unit (Linux) or a launchd agent (macOS)
    InstallService {
        /// Print the unit and the commands instead
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove the service installed by install-service
    UninstallService {
        #[arg(long)]
        dry_run: bool,
    },
    /// Show daemon status
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Replace the running daemon with this ptyro (or --binary) without stopping a session:
    /// agents keep working, clients reconnect (a live upgrade)
    Upgrade {
        /// The new binary (default: the ptyro on disk)
        #[arg(long)]
        binary: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Internal: the handoff image versions this binary can take over from
    #[command(hide = true)]
    HandoffVersion,
    /// Show the end of the daemon log
    Logs {
        /// How many lines
        #[arg(long, default_value_t = 50)]
        lines: usize,
        /// Keep printing new lines
        #[arg(short, long)]
        follow: bool,
    },
}

/// `123`, `#123` or a GitHub URL ending in the number.
fn parse_access(s: &str) -> Result<ptyro_core::config::Access, String> {
    s.parse()
}

/// `--status`: a selector's state word (`ptyro_core::selector`), aliases too.
fn parse_state(s: &str) -> Result<ptyro_core::selector::State, String> {
    ptyro_core::selector::State::parse(s.trim().trim_start_matches('@'))
        .ok_or_else(|| format!("unknown state {s:?}; try {}", ptyro_core::selector::state_words()))
}

fn parse_number(s: &str) -> Result<u32, String> {
    let t = s.trim().trim_end_matches('/');
    t.rsplit(['/', '#'])
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| format!("{s:?}: expected a number or a GitHub URL"))
}

fn main() {
    // A shell asking for completions: answer and exit before anything else happens.
    complete::handle(Cli::command);
    let cli = parse_cli(std::env::args_os()).unwrap_or_else(|e| e.exit());
    let code = match cli.cmd {
        Some(Cmd::Daemon { cmd: DaemonCmd::Start { serve: true, .. } }) => {
            eprintln!(
                "ptyro: `ptyro daemon start --serve` was removed on 2026-11-17: `ptyro web on` starts the daemon with the web app"
            );
            ptyro_core::ErrorCode::Invalid.exit_code()
        }
        Some(Cmd::Daemon { cmd: DaemonCmd::Start { foreground, spawned, .. } }) => {
            cli::daemon_start(foreground, spawned)
        }
        Some(Cmd::FakeAgent { args }) => ptyro_testkit::fake_agent::main(&args),
        #[cfg(debug_assertions)]
        Some(Cmd::FakeGh { args }) => ptyro_testkit::fake_gh::main(&args),
        #[cfg(debug_assertions)]
        Some(Cmd::FakeInstaller { args }) => ptyro_agents::setup::fake_installer(&args),

        // These run inside agents on every hook: no runtime, no config, never a failure.
        Some(Cmd::Hook { adapter, event, args }) => relay::hook(&adapter, &event, &args),
        Some(Cmd::StatuslineRelay) => relay::statusline(),

        cmd => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            rt.block_on(cli::run(cmd, cli.task, cli.json_wire, cli.remote))
        }
    };
    std::process::exit(code);
}

/// A clap value parser that takes only durations (`90s`, `5m`).
fn duration_arg(s: &str) -> Result<String, String> {
    ptyro_core::config::parse_duration(s)
        .map(|_| s.to_owned())
        .map_err(|e| format!("not a duration ({e})"))
}

#[cfg(test)]
mod tests {
    /// clap's own consistency checks over every subcommand (they only run when parsed otherwise).
    #[test]
    fn the_cli_definition_is_consistent() {
        use clap::CommandFactory;
        // The whole command tree is built on the stack: more than a test thread's 2 MiB in a
        // debug build (the binary's main thread has 8 MiB).
        std::thread::Builder::new()
            .stack_size(32 << 20)
            .spawn(|| super::Cli::command().debug_assert())
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn a_quoted_task_is_a_task_and_unquoted_words_are_a_mistyped_command() {
        let parse = |argv: &'static [&'static str]| {
            std::thread::Builder::new()
                .stack_size(32 << 20)
                .spawn(move || super::parse_cli(argv).map(|c| (c.cmd.is_some(), c.task)))
                .unwrap()
                .join()
                .unwrap()
        };
        assert_eq!(
            parse(&["ptyro", "fix the flaky test"]).ok(),
            Some((false, vec!["fix the flaky test".to_owned()]))
        );
        assert_eq!(parse(&["ptyro", "ls"]).ok(), Some((true, vec![])));
        // Global flags still go before a command.
        assert_eq!(parse(&["ptyro", "--json-wire", "ls", "--json"]).ok(), Some((true, vec![])));
        assert_eq!(parse(&["ptyro", "--remote", "box", "ls"]).ok(), Some((true, vec![])));
        assert_eq!(parse(&["ptyro"]).ok(), Some((false, vec![])));
        // Several words unquoted (U-11): clap's unknown command, its did-you-mean, and how to
        // give a task instead.
        let e = parse(&["ptyro", "sessions", "list"]).unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::InvalidSubcommand);
        let text = e.render().to_string();
        assert!(text.contains("unrecognized subcommand 'sessions'"), "{text}");
        let near = parse(&["ptyro", "staus", "now"]).unwrap_err().render().to_string();
        assert!(near.contains("'status'"), "a did-you-mean: {near}");
        assert!(
            text.contains("to give your agent a task, quote it: ptyro \"sessions list\""),
            "{text}"
        );
        let e = parse(&["ptyro", "fix", "it", "now"]).unwrap_err();
        assert!(e.render().to_string().contains("ptyro \"fix it now\""));
    }

    /// `wait --scrollback [N]` reads as `read --scrollback [N]`: N lines above the screen, or
    /// bare, what's kept in memory (never the whole recording, which `wait` would read every
    /// poll).
    #[test]
    fn wait_scrollback_takes_lines_as_read_does() {
        use clap::Parser;
        let parse = |argv: &'static [&'static str]| {
            std::thread::Builder::new()
                .stack_size(32 << 20)
                .spawn(move || match super::Cli::try_parse_from(argv).map(|c| c.cmd) {
                    Ok(Some(super::Cmd::Wait { scrollback, .. })) => Some(scrollback),
                    Ok(Some(super::Cmd::Read { scrollback, .. })) => Some(scrollback),
                    _ => None,
                })
                .unwrap()
                .join()
                .unwrap()
        };
        let wait = parse(&["ptyro", "wait", "s", "--match", "x", "--scrollback", "50"]);
        assert_eq!(wait, Some(Some(Some(50))));
        assert_eq!(wait, parse(&["ptyro", "read", "s", "--scrollback", "50"]));
        let all = parse(&["ptyro", "wait", "s", "--match", "x", "--scrollback"]);
        assert_eq!(all, Some(Some(None)));
        assert_eq!(all, parse(&["ptyro", "read", "s", "--scrollback"]));
        assert_eq!(parse(&["ptyro", "wait", "s", "--match", "x"]), Some(None));
        // Still only with --match.
        assert_eq!(parse(&["ptyro", "wait", "s", "--scrollback", "5"]), None);
    }

    /// Commands removed on 2026-11-17 say so and name what to use, however they're typed.
    #[test]
    fn a_removed_command_says_what_to_use() {
        let said = |argv: &[&str]| {
            let argv: Vec<std::ffi::OsString> = argv.iter().map(Into::into).collect();
            super::removed_command(&argv)
        };
        // Native Windows' own, on the day it went.
        let url = said(&["ptyro", "open-url", "ptyro://attach/x"]).unwrap();
        assert!(url.contains("removed on 2026-11-18") && url.contains("inside WSL"), "{url}");
        let init = said(&["ptyro", "init", "--yes"]).unwrap();
        assert!(
            init.contains("`ptyro init` was removed") && init.contains("`ptyro setup`"),
            "{init}"
        );
        assert!(said(&["ptyro", "--remote", "box", "tour"]).unwrap().contains("`ptyro tour`"));
        assert!(said(&["ptyro", "integrations", "ls"]).is_some());
        assert!(said(&["ptyro", "backup"]).is_some());
        // Every name a removed command answered to, with whatever follows it.
        let inbox = said(&["ptyro", "inbox", "--json"]).unwrap();
        assert!(
            inbox.contains("`ptyro inbox` was removed") && inbox.contains("ptyro status"),
            "{inbox}"
        );
        let pad = said(&["ptyro", "pad", "add", "-"]).unwrap();
        assert!(pad.contains("`ptyro pad`") && pad.contains("notes file stays"), "{pad}");
        assert!(said(&["ptyro", "lease", "ls"]).unwrap().contains("nothing replaces it"));
        for c in ["cs", "changeset", "msg", "run", "fanout", "recipe", "pr", "review", "verify"] {
            assert!(said(&["ptyro", c, "ls"]).is_some(), "{c}");
        }
        assert_eq!(said(&["ptyro", "setup"]), None);
        assert_eq!(said(&["ptyro", "chat", "new", "backup"]), None, "only the command word");
        assert_eq!(said(&["ptyro"]), None);
    }

    /// Removed subcommands and flags of commands that stay say so and name what to use, but only
    /// where clap refuses the line: what parses is never taken for them.
    #[test]
    fn a_removed_subcommand_or_flag_says_what_to_use() {
        // clap's whole command tree wants more stack than a test thread has, in a debug build.
        std::thread::Builder::new()
            .stack_size(32 << 20)
            .spawn(|| {
                let err = |argv: &[&str]| match super::parse_cli(argv) {
                    Err(e) => e.to_string(),
                    Ok(_) => panic!("{argv:?} parsed"),
                };
                let e = err(&["ptyro", "wt", "retitle", "x", "t"]);
                assert!(
                    e.contains("`ptyro wt retitle` was removed")
                        && e.contains("wt rename WT --title"),
                    "{e}"
                );
                let e = err(&["ptyro", "--remote", "box", "worktree", "archive", "x"]);
                assert!(e.contains("`ptyro worktree archive`") && e.contains("wt rm"), "{e}");
                assert!(err(&["ptyro", "wt", "lock", "x"]).contains("gc --keep-wt"));
                assert!(err(&["ptyro", "wt", "pair", "x"]).contains("nothing replaces it"));
                assert!(err(&["ptyro", "project", "discover"]).contains("`ptyro project add DIR`"));
                let e = err(&["ptyro", "wt", "new", "x", "--take-changes"]);
                assert!(
                    e.contains("`ptyro wt new --take-changes`") && e.contains("--existing"),
                    "{e}"
                );
                let e = err(&["ptyro", "new", "fix it", "--verify=cargo test"]);
                assert!(e.contains("`ptyro new --verify` was removed"), "{e}");
                assert!(
                    err(&["ptyro", "chat", "new", "--preset", "none", "hi"])
                        .contains("`--folder DIR`")
                );
                // Mistyped, not removed: clap's own words.
                let e = err(&["ptyro", "wt", "rename", "x", "--titel", "y"]);
                assert!(!e.contains("was removed"), "{e}");
                // The same words where they parse are what they say.
                assert!(super::parse_cli(["ptyro", "send", "s", "wt", "archive"]).is_ok());
                assert!(
                    super::parse_cli(["ptyro", "new", "-d", "--", "sh", "-c", "echo --verify"])
                        .is_ok()
                );
                assert!(
                    super::parse_cli(["ptyro", "gc", "--keep-wt", "x", "--keep-wt", "y"]).is_ok()
                );
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn send_takes_flags_between_its_words() {
        use clap::Parser;
        let parse = |argv: &'static [&'static str]| {
            std::thread::Builder::new()
                .stack_size(32 << 20)
                .spawn(move || match super::Cli::try_parse_from(argv).map(|c| c.cmd) {
                    Ok(Some(super::Cmd::Send { args, wait_ready, .. })) => Some((args, wait_ready)),
                    _ => None,
                })
                .unwrap()
                .join()
                .unwrap()
        };
        let (args, _) = parse(&["ptyro", "send", "s", "--raw", "x"]).unwrap();
        assert_eq!(args, ["s", "x"]);
        // A value that isn't a duration is an error, never quietly the session or the text.
        assert!(parse(&["ptyro", "send", "--wait-ready", "s", "x"]).is_none());
        let (args, ready) = parse(&["ptyro", "send", "s", "x", "--wait-ready"]).unwrap();
        assert_eq!((args, ready.as_deref()), (vec!["s".to_owned(), "x".to_owned()], Some("2m")));
        let (args, ready) = parse(&["ptyro", "send", "--wait-ready", "60s", "s", "x"]).unwrap();
        assert_eq!((args, ready.as_deref()), (vec!["s".to_owned(), "x".to_owned()], Some("60s")));
        let (_, ready) = parse(&["ptyro", "send", "--wait-ready=5m", "s", "x"]).unwrap();
        assert_eq!(ready.as_deref(), Some("5m"));
    }

    /// Every argument of every command, as `(command path, short, long, takes a value)`.
    fn every_flag() -> Vec<(String, Option<char>, Option<String>, bool)> {
        fn walk(
            c: &clap::Command,
            path: &str,
            out: &mut Vec<(String, Option<char>, Option<String>, bool)>,
        ) {
            for a in c.get_arguments().filter(|a| !a.is_positional()) {
                let id = a.get_id().as_str();
                if id == "help" || id == "version" {
                    continue;
                }
                let takes = a.get_num_args().is_some_and(|n| n.takes_values());
                out.push((path.to_owned(), a.get_short(), a.get_long().map(str::to_owned), takes));
            }
            for s in c.get_subcommands() {
                walk(s, &format!("{path} {}", s.get_name()), out);
            }
        }
        // (The whole tree is built on the stack: more than a test thread's 2 MiB.)
        std::thread::Builder::new()
            .stack_size(32 << 20)
            .spawn(|| {
                let mut out = Vec::new();
                let mut c = <super::Cli as clap::CommandFactory>::command();
                c.build();
                walk(&c, "ptyro", &mut out);
                out
            })
            .unwrap()
            .join()
            .unwrap()
    }

    /// One meaning per short flag (owner, 2026-11-09; docs/commands.md): a letter is the same
    /// long flag in every command (`-p` is always `--project`), and a long flag that has a
    /// letter somewhere has it everywhere it is taken.
    #[test]
    fn a_short_flag_means_one_thing_everywhere() {
        use std::collections::BTreeMap;
        let flags = every_flag();
        let mut by_short = BTreeMap::<char, BTreeMap<String, Vec<String>>>::new();
        let mut by_long = BTreeMap::<String, BTreeMap<Option<char>, Vec<String>>>::new();
        for (path, short, long, _) in &flags {
            let long = long.clone().unwrap_or_else(|| "(no long name)".into());
            if let Some(c) = short {
                by_short.entry(*c).or_default().entry(long.clone()).or_default().push(path.clone());
            }
            by_long.entry(long).or_default().entry(*short).or_default().push(path.clone());
        }
        let mut problems = Vec::new();
        for (c, longs) in by_short.iter().filter(|(_, l)| l.len() > 1) {
            problems.push(format!("-{c} means {longs:?}"));
        }
        for (l, shorts) in by_long.iter().filter(|(_, s)| s.len() > 1) {
            problems.push(format!("--{l} has the letters {shorts:?}"));
        }
        assert!(
            problems.is_empty(),
            "a short flag means one thing across the CLI (the table in docs/commands.md):\n  {}",
            problems.join("\n  ")
        );
    }

    /// A long flag is a switch everywhere or takes a value everywhere: `--x V` never means a
    /// switch plus a word in another command. No exceptions: the last ones were renamed
    /// (owner, 2026-11-10).
    #[test]
    fn a_flag_is_a_switch_everywhere_or_takes_a_value_everywhere() {
        use std::collections::BTreeMap;
        let mut by_long = BTreeMap::<String, BTreeMap<bool, Vec<String>>>::new();
        for (path, _, long, takes) in every_flag() {
            if let Some(l) = long {
                by_long.entry(l).or_default().entry(takes).or_default().push(path);
            }
        }
        let mixed: Vec<String> = by_long
            .iter()
            .filter(|(_, k)| k.len() > 1)
            .map(|(l, k)| format!("--{l}: a switch in {:?}, a value in {:?}", k[&false], k[&true]))
            .collect();
        assert!(mixed.is_empty(), "\n  {}", mixed.join("\n  "));
    }

    /// Value syntax is the same everywhere (owner, 2026-11-09): `--flag V` and `--flag=V` both
    /// work, so no flag may insist on `=`.
    #[test]
    fn no_flag_insists_on_an_equals_sign() {
        std::thread::Builder::new()
            .stack_size(32 << 20)
            .spawn(|| {
                fn walk(c: &clap::Command, path: &str, out: &mut Vec<String>) {
                    for a in c.get_arguments().filter(|a| a.is_require_equals_set()) {
                        out.push(format!("{path} --{}", a.get_long().unwrap_or("?")));
                    }
                    for s in c.get_subcommands() {
                        walk(s, &format!("{path} {}", s.get_name()), out);
                    }
                }
                let mut out = Vec::new();
                walk(&<super::Cli as clap::CommandFactory>::command(), "ptyro", &mut out);
                assert!(out.is_empty(), "these insist on `--flag=VALUE`: {out:?}");
            })
            .unwrap()
            .join()
            .unwrap();
    }

    #[test]
    fn the_guard_sees_the_flags() {
        let flags = every_flag();
        let has = |path: &str, short: char, long: &str| {
            flags
                .iter()
                .any(|(p, s, l, _)| p == path && *s == Some(short) && l.as_deref() == Some(long))
        };
        assert!(has("ptyro new", 'p', "project"));
        assert!(has("ptyro wt ls", 'p', "project"));
        assert!(has("ptyro config list", 'p', "project"));
        assert!(has("ptyro events", 'l', "limit"));
    }

    /// An interactive session's commands attach the same way (owner, 2026-11-09): `-d` says
    /// don't, everywhere one is started, and an optional name is `-n/--name`.
    #[test]
    fn starting_a_session_takes_detach_and_name_alike() {
        let flags = every_flag();
        for cmd in ["ptyro new", "ptyro wt new", "ptyro shells new", "ptyro chat task"] {
            for (short, long) in [('d', "detach"), ('n', "name")] {
                assert!(
                    flags.iter().any(|(p, s, l, _)| p == cmd
                        && *s == Some(short)
                        && l.as_deref() == Some(long)),
                    "{cmd} takes -{short}/--{long}"
                );
            }
        }
    }
}
