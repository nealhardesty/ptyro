//! The single `ptyro` binary (SPEC §3.3, ADR 12): CLI, TUI, daemon and dev helpers.

#[cfg(not(unix))]
compile_error!(
    "ptyro runs on Linux and macOS (Windows through WSL2); see the Requirements section of README.md"
);

mod about;
mod agents;
mod backup;
mod cli;
mod complete;
mod config;
mod cs;
mod doctor;
mod host;
mod init;
mod machines;
mod mcp;
mod orch;
mod pad;
#[cfg(test)]
mod parity;
mod profiles;
mod projects;
mod record;
mod relay;
mod remote;
mod script;
mod serve;
mod service;
mod ship;
mod tls;
mod tour;
mod uninstall;
mod webapp;

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
    /// Use the daemon on another machine, over ssh: `[ssh://][user@]host[:port]` (or set
    /// PTYRO_REMOTE). Needs ptyro there and key-based ssh login
    #[arg(long, global = true, value_name = "HOST")]
    remote: Option<String>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
    /// A task for your default agent, in a new worktree of the repository you're in (UX1.9)
    #[arg(value_name = "TASK", num_args = 0..)]
    task: Vec<String>,
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
        /// Print the session as JSON (with -d)
        #[arg(long)]
        json: bool,
        /// Start an agent: claude, codex, gemini, opencode, or one from [agents]; `claude@work`
        /// picks a profile too
        #[arg(short, long, add = ArgValueCandidates::new(complete::agents))]
        agent: Option<String>,
        /// Run under this profile (default: the worktree's, the project's pin, the workspace's)
        #[arg(long, add = ArgValueCandidates::new(complete::profiles))]
        profile: Option<String>,
        /// Start another of what this workspace last started (an agent, a shell, a command);
        /// the default agent when nothing has started there yet
        #[arg(long, conflicts_with_all = ["agent", "argv", "new_wt", "pr", "issue", "take_changes"])]
        again: bool,
        /// Run in this worktree (`brisk-rook`, `myrepo/@root`)
        #[arg(long, conflicts_with = "new_wt", add = ArgValueCandidates::new(complete::worktrees))]
        wt: Option<String>,
        /// Create a new worktree (optionally named) and run there
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        new_wt: Option<String>,
        /// Project for --new-wt (default: the one containing the current directory)
        #[arg(long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// Run under a profile other than the worktree's or the project's pin
        #[arg(long)]
        override_pin: bool,
        /// Answer yes to a profile guard's question
        #[arg(short, long)]
        yes: bool,
        /// Type this prompt into the agent once it's ready (`@name`: a project prompt snippet)
        #[arg(short, long, conflicts_with = "prompt_file")]
        prompt: Option<String>,
        /// Read the prompt from a file
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        /// What it's for, for people to read (default: the prompt); the name stays its address
        #[arg(long)]
        title: Option<String>,
        /// In a new worktree that takes the main checkout's uncommitted changes with it
        #[arg(long, conflicts_with_all = ["wt", "pr", "issue"])]
        take_changes: bool,
        /// In a new worktree checked out from this pull request (number or URL)
        #[arg(long, value_parser = parse_number, conflicts_with_all = ["wt", "issue"])]
        pr: Option<u32>,
        /// In a new worktree for this GitHub issue (number or URL): its branch and the agent's
        /// first prompt come from the issue
        #[arg(long, conflicts_with = "wt")]
        issue: Option<String>,
        /// Start even if `review.max_unreviewed` worktrees wait for review
        #[arg(long)]
        unqueued: bool,
        /// Verified-done: a turn counts as done only when this passes (`scripts.run.test`, a run
        /// script's name, or a command); failures go back to the agent
        #[arg(long)]
        verify: Option<String>,
        /// Make it a lead agent: it gets `ptyro mcp` to start and coordinate workers, within limits
        #[arg(long)]
        lead: bool,
        /// Lead: live workers at once (default 4)
        #[arg(long, value_name = "N")]
        max_workers: Option<u32>,
        /// Lead: the agents it may start (default: any)
        #[arg(long = "lead-agent", value_name = "AGENT")]
        lead_agents: Vec<String>,
        /// Drive the agent over its structured protocol (Claude stream-json, Codex app-server,
        /// Gemini ACP, OpenCode's server) and show a chat view: every tool call visible
        #[arg(long)]
        structured: bool,
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
    },
    /// List sessions
    #[command(alias = "ls")]
    List {
        #[arg(long)]
        json: bool,
        /// Include sessions from earlier daemon runs (from the store)
        #[arg(short, long)]
        all: bool,
        /// Only sessions in this state or status: running, exited, dormant, working, blocked,
        /// idle, done, errored
        #[arg(long)]
        status: Option<String>,
        /// Only sessions of this project
        #[arg(short, long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// Only sessions whose home is this workspace
        #[arg(short, long, add = ArgValueCandidates::new(complete::workspaces))]
        workspace: Option<String>,
    },
    /// Stop sessions (SIGHUP, then TERM, then KILL) and remove them.
    /// SESSION may be a selector: `@all`, `@live`, `@exited`, `@blocked`, `build-*`, `a,b`…
    Kill {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
        /// Send this signal number instead of the graceful sequence
        #[arg(short, long)]
        signal: Option<i32>,
        /// Keep the exited session listed (with its final screen)
        #[arg(long)]
        keep: bool,
    },
    /// Stop sessions gracefully and keep them listed (agents finish their turn first)
    Stop {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
    },
    /// Start exited sessions' commands again, in place
    Restart {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
    },
    /// Remove exited sessions from the list
    Rm {
        #[arg(required = true, add = ArgValueCandidates::new(complete::sessions))]
        sessions: Vec<String>,
    },
    /// Rename a session
    Rename {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        name: String,
    },
    /// Print a session's screen (or scrollback)
    Read {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        /// The visible screen (the default)
        #[arg(long, conflicts_with = "scrollback")]
        screen: bool,
        /// Up to N lines of scrollback above the screen (all of it without N)
        #[arg(long, num_args = 0..=1, default_missing_value = "4294967295")]
        scrollback: Option<u32>,
        /// Keep colours and attributes (SGR)
        #[arg(long, conflicts_with = "plain")]
        ansi: bool,
        /// Plain text (the default)
        #[arg(long)]
        plain: bool,
        /// The text plus the session's status, as JSON
        #[arg(long)]
        json: bool,
        /// A structured session's transcript: one JSON item per line (messages, tool calls,
        /// permission requests, turn ends); with --json, one array
        #[arg(long, conflicts_with_all = ["screen", "scrollback", "ansi"])]
        chat: bool,
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
        /// working, blocked, idle, done, errored, exit, any-attention (idle and done mean the
        /// turn ptyro last typed has ended)
        #[arg(long = "for", default_value = "idle", value_parser = script::CONDITIONS)]
        condition: String,
        /// Instead: until this regex appears on the screen
        #[arg(long = "match", value_name = "REGEX")]
        pattern: Option<String>,
        /// With --match: search the scrollback too
        #[arg(long, requires = "pattern")]
        scrollback: bool,
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
        #[arg(short, long, add = ArgValueCandidates::new(complete::sessions))]
        session: Option<String>,
        /// How many past events (without --follow)
        #[arg(short = 'n', long, default_value_t = 50)]
        limit: u32,
        #[arg(long)]
        json: bool,
    },
    /// Pull requests: create, status and checks, fix failing checks, auto-merge, watch
    Pr {
        #[command(subcommand)]
        cmd: ship::PrCmd,
    },
    /// Watch a PR: new CI failures and review comments go to the worktree's agent
    #[command(name = "watch-pr")]
    WatchPr {
        #[arg(add = ArgValueCandidates::new(complete::worktrees))]
        worktree: Option<String>,
        #[arg(long)]
        off: bool,
    },
    /// Review: line comments for the agent, and the queue of worktrees ready for review
    Review {
        #[command(subcommand)]
        cmd: ship::ReviewCmd,
    },
    /// Runs: recipes, fan-outs and leads, with their steps, gates and events
    #[command(alias = "runs")]
    Run {
        #[command(subcommand)]
        cmd: orch::RunCmd,
    },
    /// Recipes: declarative multi-step workflows (built-in, yours, the repo's)
    #[command(alias = "recipes")]
    Recipe {
        #[command(subcommand)]
        cmd: orch::RecipeCmd,
    },
    /// The same task to several agents, each in its own worktree; then `ptyro compare`
    Fanout {
        /// Agents, comma-separated or repeated: claude,codex,gemini (`claude@work` too)
        #[arg(short, long = "agents", value_delimiter = ',', required_unless_present = "n")]
        agents: Vec<String>,
        /// How many candidates (cycling through --agents; default agent claude)
        #[arg(short, value_name = "N")]
        n: Option<u32>,
        #[arg(short, long, conflicts_with = "prompt_file")]
        prompt: Option<String>,
        #[arg(long)]
        prompt_file: Option<PathBuf>,
        #[arg(long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        /// Base for the worktrees (default: the project's)
        #[arg(long)]
        base: Option<String>,
        /// Worktree name prefix (default: from the prompt)
        #[arg(long)]
        name: Option<String>,
        /// Rank by this check when they're done (default `scripts.run.test`)
        #[arg(long, num_args = 0..=1, default_missing_value = "scripts.run.test")]
        test: Option<String>,
        /// Verified-done for every candidate: bounce it until this passes
        #[arg(long)]
        verify: Option<String>,
        /// An advisory judge: this agent scores the candidates' diffs (0–10) in a clean context
        #[arg(long, add = ArgValueCandidates::new(complete::agents))]
        judge: Option<String>,
        /// Per candidate (`45m`)
        #[arg(long)]
        timeout: Option<String>,
        /// Workers run as structured sessions: their tool calls show in `compare`
        #[arg(long)]
        structured: bool,
        /// Wait for all of them, then print the comparison
        #[arg(long)]
        wait: bool,
        #[arg(long)]
        json: bool,
    },
    /// A fan-out's candidates ranked by evidence: tests, repro, cross-tests, diff, time
    Compare {
        /// A run id (or `@last`)
        run: String,
        /// Run the evidence again (tests in every candidate and on the base)
        #[arg(long)]
        refresh: bool,
        #[arg(long)]
        json: bool,
    },
    /// Take a candidate: merge it (default), cherry-pick, open a PR, or apply some files/hunks
    Pick {
        run: String,
        candidate: String,
        #[arg(long, conflicts_with_all = ["pr", "apply"])]
        cherry_pick: bool,
        #[arg(long, conflicts_with = "apply")]
        pr: bool,
        /// Copy changes into a worktree (default: the main checkout), uncommitted
        #[arg(long)]
        apply: bool,
        /// Target branch (merge, cherry-pick) or worktree (apply)
        #[arg(long)]
        into: Option<String>,
        /// Only this file (apply; repeatable, globs work)
        #[arg(long = "path")]
        paths: Vec<String>,
        /// Only this hunk, FILE:N (apply; repeatable)
        #[arg(long = "hunk")]
        hunks: Vec<String>,
    },
    /// Remove candidates' worktrees (every one not picked, or one); refuses to lose work
    Discard {
        run: String,
        candidate: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// A follow-up for a candidate's agent (or all of them): `ptyro iterate @last codex "also …"`
    Iterate {
        run: String,
        /// `[CANDIDATE]` TEXT
        #[arg(required = true, num_args = 1..=2)]
        args: Vec<String>,
    },
    /// Change sets: one change across several repositories, with a lead, a contract, a
    /// ledger, set checkpoints and a dependency-ordered ship train
    #[command(alias = "changeset")]
    Cs {
        #[command(subcommand)]
        cmd: cs::CsCmd,
    },
    /// Gates: questions runs wait on, answered from here, the TUI or your phone
    #[command(alias = "gates")]
    Gate {
        #[command(subcommand)]
        cmd: orch::GateCmd,
    },
    /// The mailbox: messages between agents (and you), delivered when they're idle
    Msg {
        #[command(subcommand)]
        cmd: orch::MsgCmd,
    },
    /// File leases: keep other agents out of files one is changing
    #[command(alias = "leases")]
    Lease {
        #[command(subcommand)]
        cmd: orch::LeaseCmd,
    },
    /// Overlapping changes between a project's live worktrees, and predicted merge conflicts
    Radar {
        #[arg(long, add = ArgValueCandidates::new(complete::projects))]
        project: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Checkpoints: the worktree after every agent turn; diff or restore any of them
    #[command(alias = "checkpoints", alias = "ckpt")]
    Checkpoint {
        #[command(subcommand)]
        cmd: orch::CheckpointCmd,
    },
    /// Verified-done: a session's turns count as done only when CHECK passes
    Verify {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        /// `scripts.run.test`, a run script's name, or a command
        check: Option<String>,
        #[arg(long)]
        off: bool,
        #[arg(long)]
        max_bounces: Option<u32>,
    },
    /// MCP server for lead agents (started for you by `ptyro new --agent … --lead`)
    Mcp,
    /// Bridge stdin/stdout to this machine's daemon (what `--remote` runs over ssh)
    Proxy,
    /// Switch an agent between structured mode (chat view) and its own terminal UI, in the
    /// same conversation: `--to tui` takes over by hand, `--to structured` goes back
    Convert {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        /// `tui` (default) or `structured`
        #[arg(long, default_value = "tui", value_parser = ["tui", "structured"])]
        to: String,
        /// Interrupt a turn in progress first
        #[arg(long)]
        force: bool,
    },
    /// Change a session's title, what people read; no title clears it. Its name, the address,
    /// stays (`ptyro rename` changes that; `ptyro wt retitle` titles a worktree)
    Retitle {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        target: String,
        /// The new title (several words are joined)
        title: Vec<String>,
    },
    /// What a session is doing, in a line, from a small model (`[summaries] command`; it runs
    /// on the session's profile's account, about $0.0007 a time with Haiku)
    Summarize {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        /// Print the summary as JSON (`text`, `turn`, `at_ms`)
        #[arg(long)]
        json: bool,
    },
    /// Make the Claude you started by hand in a shell session a managed session (hooks,
    /// hibernation, mail), resuming the same conversation
    Adopt {
        /// The shell session it runs in
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
    },
    /// Your worktree's quick terminal: one shell per worktree (or directory), reused, which the
    /// TUI shows as a drawer under the pane (`C-q C-t`). Attaches, starting it if there's none
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
        /// With --read: up to N lines of scrollback above the screen (all of it without N)
        #[arg(long, requires = "read", num_args = 0..=1, default_missing_value = "4294967295")]
        scrollback: Option<u32>,
    },
    /// The scratchpad: one notes file for ideas and bugs (`$PTYRO_SCRATCHPAD`), the same in every
    /// session. Opens in your editor, here; `--pane` opens it as a session (`C-q ;` in the TUI)
    #[command(visible_alias = "pad")]
    Scratchpad {
        #[command(subcommand)]
        cmd: Option<pad::PadCmd>,
        /// Open it as a session of its own (the editor, kept running) and attach
        #[arg(long)]
        pane: bool,
        /// With --pane: print the session as JSON instead of attaching
        #[arg(long, requires = "pane")]
        json: bool,
    },
    /// Serve the web app and the protocol over WebSocket for paired devices (phones)
    Serve {
        /// Listen here (default `[remote] listen`, 127.0.0.1:7091)
        #[arg(long)]
        listen: Option<String>,
        /// Plain HTTP instead of HTTPS, on any address (tokens in clear). Without it ptyro speaks
        /// HTTPS with a certificate it makes; phones trust it once, from /ca.crt
        #[arg(long)]
        insecure: bool,
        /// Listen on every interface, letting in only this machine, private networks and
        /// tailnets (over HTTPS)
        #[arg(long)]
        local: bool,
        /// Your own certificate (a PEM chain, leaf first) instead of the one ptyro makes; with
        /// --key
        #[arg(long, requires = "key")]
        cert: Option<PathBuf>,
        /// The key for --cert (PEM)
        #[arg(long, requires = "cert")]
        key: Option<PathBuf>,
        /// Put Tailscale HTTPS in front (`tailscale serve`), then print the URL to open
        #[arg(long)]
        tailscale: bool,
        /// Internal: started by the daemon (`[remote] serve = true`)
        #[arg(long, hide = true)]
        managed: bool,
    },
    /// The web app for your phone: have the daemon run its server (`on`), stop it (`off`), or say
    /// how it is (`status`, the default)
    Web {
        #[arg(default_value = "status", value_parser = ["on", "off", "status"])]
        action: String,
        /// How phones reach it when turning it on: tailscale (HTTPS over your tailnet), lan (this
        /// network, HTTPS from a certificate ptyro makes) or here (this computer only). Default:
        /// what the config says, else Tailscale if it is up, else lan
        #[arg(long, value_parser = ["tailscale", "lan", "here"])]
        via: Option<String>,
        /// With `on`: then show the pairing QR code for a phone
        #[arg(long)]
        pair: bool,
        #[arg(long)]
        json: bool,
    },
    /// Pair a phone or browser: prints a one-time QR code that opens the web app signed in
    Pair {
        /// What to call it in `ptyro devices`
        #[arg(default_value = "phone")]
        name: String,
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
    /// Play a session's recording back with its timing (also for sessions that ended)
    Replay {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        /// 2 = twice as fast
        #[arg(long, default_value_t = 1.0)]
        speed: f64,
        /// Cap every pause at this
        #[arg(long, default_value = "2s")]
        max_idle: String,
        /// Fast-forward this far into it first
        #[arg(long)]
        from: Option<String>,
    },
    /// A session's recording as asciicast v2 (asciinema), plain text, or a `.ptylog` fixture
    Export {
        #[arg(add = ArgValueCandidates::new(complete::sessions))]
        session: String,
        #[arg(long, default_value = "asciicast", value_parser = ["asciicast", "text", "ptylog"])]
        format: String,
        /// Write here (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
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
    /// Set ptyro up: detect agents, accounts and repositories, then write your config
    Init {
        /// Accept every suggestion without asking (scripts, CI)
        #[arg(short, long)]
        yes: bool,
        /// Show what it would write, change nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// Which ptyro this is: version, repository, licence, build, where things live, whether the
    /// daemon and the web app run, and a block to paste into a bug report
    About {
        #[arg(long)]
        json: bool,
    },
    /// Diagnose: agents and logins per profile, integrations, terminal, tmux, daemon, git
    Doctor {
        /// Skip the hook self-test (it runs a fake agent through each integration)
        #[arg(long)]
        quick: bool,
        #[arg(long)]
        json: bool,
    },
    /// The tour: start a task, answer the agent, review, finish and clean up, for real, with a
    /// pretend agent in a ptyro of its own (deleted afterwards)
    Tour {
        /// Internal: the coach on this ptyro (what the tour itself runs)
        #[arg(long, hide = true)]
        here: bool,
    },
    /// Take ptyro off this computer: stops the daemon and every session, removes its worktrees,
    /// config, data, login service, integrations and shell lines, and the binary when it was
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
        /// Keep the data directory (history, scratchpad, profiles) and so the worktrees
        #[arg(long)]
        keep_data: bool,
        /// Don't remove the ptyro program itself
        #[arg(long)]
        keep_binary: bool,
        /// Save a backup here first (`ptyro backup`'s file); nothing is removed if it fails
        #[arg(long, value_name = "FILE")]
        backup: Option<PathBuf>,
        /// Also delete the snapshots under refs/ptyro/ in your repositories (archived and
        /// taken-over work)
        #[arg(long)]
        purge_refs: bool,
        #[arg(long)]
        json: bool,
    },
    /// Save ptyro's own state (config, database, scratchpad) to a private .tar.gz
    Backup {
        /// Where to write it (default: `~/ptyro-backup-DATE.tar.gz`); never replaces a file
        #[arg(short, long, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Include every session's recording (big; it holds what was on screen)
        #[arg(long)]
        with_recordings: bool,
        /// Include the web app's certificate authority, with its private key
        #[arg(long)]
        with_ca: bool,
        #[arg(long)]
        json: bool,
    },
    /// Print shell integration (prompt marks and cwd reporting) for your shell's rc file
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
    /// The inbox: what wanted you, newest first
    Inbox {
        /// Only this workspace (default: all)
        #[arg(long)]
        workspace: Option<String>,
        /// Only unread entries
        #[arg(short, long)]
        unread: bool,
        /// Mark everything read
        #[arg(long)]
        read: bool,
        #[arg(long)]
        json: bool,
    },
    /// Silence notifications for a session, `project:<p>` or `workspace:<w>` (the inbox still
    /// gets them)
    Mute {
        target: String,
        /// For this long (`30m`, `2h`); default until unmuted
        #[arg(long = "for")]
        duration: Option<String>,
        /// Unmute instead
        #[arg(long)]
        off: bool,
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
    /// Agent integrations that need installing once (Gemini extension, Antigravity hooks)
    Integrations {
        #[command(subcommand)]
        cmd: agents::IntegrationsCmd,
    },
    /// Report your program's state to ptyro from inside a session (herdr-compatible)
    #[command(alias = "report-agent")]
    Report {
        /// working, blocked, idle, done, error (also needs_input, errored…)
        #[arg(long)]
        state: String,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        msg: Option<String>,
        #[arg(long)]
        progress: Option<u8>,
        /// Default: this session ($PTYRO_SESSION_ID)
        #[arg(long)]
        session: Option<String>,
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
    /// Built-in fake GitHub CLI for tests (M6)
    #[command(hide = true)]
    FakeGh {
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
        /// A repository's settings (`.ptyro.toml`) instead of your own: the one you are in, or
        /// `--project=NAME` (its name or path)
        #[arg(short, long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
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
        #[arg(short, long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
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
        /// Into a repository's `.ptyro.local.toml` (yours), or its `.ptyro.toml` with --shared
        #[arg(short, long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
        project: Option<String>,
        /// With --project: the committed `.ptyro.toml` instead of your `.ptyro.local.toml`
        #[arg(long, requires = "project")]
        shared: bool,
    },
    /// Put a setting back to its default (remove it from the file)
    Unset {
        #[arg(required = true, add = ArgValueCandidates::new(complete::settings))]
        keys: Vec<String>,
        #[arg(short, long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
        project: Option<String>,
        #[arg(long, requires = "project")]
        shared: bool,
    },
    /// Add an agent, theme, editor, key binding, notification sink, run script or prompt
    Add {
        /// agents, themes, editors, keys, notifications.sinks (project: scripts.run, prompts)
        #[arg(add = ArgValueCandidates::new(complete::collections))]
        collection: String,
        /// Its name (a key binding's key; a sink has none)
        name: Option<String>,
        /// Its settings, as FIELD=VALUE (`command="aider --model sonnet"`); a binding's action
        /// or a prompt's text is `value=…`; a sink's `type=ntfy`
        fields: Vec<String>,
        #[arg(short, long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
        project: Option<String>,
        #[arg(long, requires = "project")]
        shared: bool,
    },
    /// Remove one of those, and everything in it
    Rm {
        #[arg(add = ArgValueCandidates::new(complete::collections))]
        collection: String,
        /// Its name (a sink's position, from `config list notifications`)
        name: String,
        #[arg(short, long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
        project: Option<String>,
        #[arg(long, requires = "project")]
        shared: bool,
    },
    /// Give one of those another name
    Rename {
        #[arg(add = ArgValueCandidates::new(complete::collections))]
        collection: String,
        from: String,
        to: String,
        #[arg(short, long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
        project: Option<String>,
        #[arg(long, requires = "project")]
        shared: bool,
    },
    /// Print every setting's description as JSON, for tools that build forms
    Schema {
        #[arg(short, long, num_args = 0..=1, require_equals = true, default_missing_value = "")]
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
        #[arg(long)]
        name: Option<String>,
        /// Keep it even if it doesn't answer now
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Stop mirroring it (nothing changes on it)
    Rm {
        #[arg(add = ArgValueCandidates::new(complete::machines))]
        name: String,
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
    /// Revoke a device: its token stops working at once, connections close
    Revoke { id: String },
    /// Send a test push notification to every subscribed device
    PushTest,
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Start the daemon (normally automatic)
    Start {
        /// Stay in the foreground and log to stderr too
        #[arg(long)]
        foreground: bool,
        /// Internal: started by a client; detach from its terminal
        #[arg(long, hide = true)]
        spawned: bool,
        /// Also turn on the web app's server (`[remote] serve`), which the daemon runs and
        /// restarts: the same as `ptyro web on`
        #[arg(long)]
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
        #[arg(short = 'n', long, default_value_t = 50)]
        lines: usize,
        /// Keep printing new lines
        #[arg(short, long)]
        follow: bool,
    },
}

/// `123`, `#123` or a GitHub URL ending in the number.
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
    let cli = Cli::parse();
    let code = match cli.cmd {
        Some(Cmd::Daemon { cmd: DaemonCmd::Start { foreground, spawned, serve } }) => {
            cli::daemon_start(foreground, spawned, serve)
        }
        Some(Cmd::FakeAgent { args }) => ptyro_testkit::fake_agent::main(&args),
        Some(Cmd::FakeGh { args }) => ptyro_testkit::fake_gh::main(&args),
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
    fn words_alone_are_a_task_and_commands_stay_commands() {
        use clap::Parser;
        let parse = |argv: &'static [&'static str]| {
            std::thread::Builder::new()
                .stack_size(32 << 20)
                .spawn(move || {
                    super::Cli::try_parse_from(argv).map(|c| (c.cmd.is_some(), c.task)).ok()
                })
                .unwrap()
                .join()
                .unwrap()
        };
        assert_eq!(
            parse(&["ptyro", "fix the flaky test"]),
            Some((false, vec!["fix the flaky test".to_owned()]))
        );
        assert_eq!(
            parse(&["ptyro", "fix", "it", "now"]).map(|(c, t)| (c, t.len())),
            Some((false, 3))
        );
        assert_eq!(parse(&["ptyro", "ls"]), Some((true, vec![])));
        // Global flags still go before a command.
        assert_eq!(parse(&["ptyro", "--json-wire", "ls", "--json"]), Some((true, vec![])));
        assert_eq!(parse(&["ptyro", "--remote", "box", "ls"]), Some((true, vec![])));
        assert_eq!(parse(&["ptyro"]), Some((false, vec![])));
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
}
