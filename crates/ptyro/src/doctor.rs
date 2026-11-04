//! `ptyro doctor` and `ptyro profile doctor` (M5.6): everything that makes ptyro misbehave
//! without saying so, checked in one place. Nothing here changes anything, and secrets are never
//! read: logins are asked of each tool's own status command (`claude auth status`,
//! `codex login status`, `gh auth status`) under the profile's config dir.

use std::path::{Path, PathBuf};
use std::time::Duration;

use ptyro_agents::profile;
use ptyro_core::config::{Config, expand_home};
use ptyro_core::model::DEFAULT_PROFILE;
use ptyro_core::{Error, ErrorCode};
use ptyro_protocol::{PROTO, Reply, Request};
use serde::Serialize;

use crate::cli::Ctx;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Ok,
    /// Worth knowing, nothing wrong.
    Note,
    /// Probably wrong, or will be.
    Warn,
    /// Broken.
    Problem,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub section: String,
    pub name: String,
    pub level: Level,
    pub text: String,
}

#[derive(Default)]
pub(crate) struct Report(pub Vec<Check>);

impl Report {
    fn add(&mut self, section: &str, name: &str, level: Level, text: impl Into<String>) {
        self.0.push(Check { section: section.into(), name: name.into(), level, text: text.into() });
    }
}

/// Agent versions ptyro was verified against (SPEC §8, §20).
const TESTED: &[(&str, &str)] =
    &[("claude", "2.1.304"), ("codex", "0.143.0"), ("gemini", "0.64.0")];

/// Run a short command; `None` if it can't start or takes longer than `t`.
async fn run(argv: &[String], env: &[(String, PathBuf)], t: Duration) -> Option<(bool, String)> {
    let mut cmd = tokio::process::Command::new(argv.first()?);
    cmd.args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true);
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = tokio::time::timeout(t, cmd.output()).await.ok()?.ok()?;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    Some((out.status.success(), text.trim().to_owned()))
}

pub fn which(prog: &str) -> Option<PathBuf> {
    if prog.contains('/') {
        return Some(PathBuf::from(prog)).filter(|p| p.is_file());
    }
    which_in(prog, &std::env::var("PATH").unwrap_or_default())
}

fn which_in(prog: &str, path: &str) -> Option<PathBuf> {
    std::env::split_paths(path).map(|d| d.join(prog)).find(|p| p.is_file())
}

fn first_version(text: &str) -> Option<String> {
    text.split(|c: char| c.is_whitespace() || c == '(' || c == 'v')
        .find(|w| w.split('.').count() >= 2 && w.split('.').all(|p| p.parse::<u32>().is_ok()))
        .map(str::to_owned)
}

pub async fn run_cmd(ctx: &Ctx, quick: bool, json: bool) -> Result<i32, Error> {
    let r = collect(ctx, quick, true).await;
    print(&r, json);
    Ok(exit_code(&r))
}

/// Every check, in order. `terminal`: also the terminal this process runs in (a web page has
/// none worth checking).
pub(crate) async fn collect(ctx: &Ctx, quick: bool, terminal_too: bool) -> Report {
    let config_path = Config::path(&ctx.paths);
    let mut r = Report::default();
    let config = match Config::load(&config_path) {
        Ok(c) => {
            r.add("ptyro", "config", Level::Ok, format!("{} is valid", config_path.display()));
            if let Some(mode) = crate::config::too_open(&config_path) {
                r.add(
                    "ptyro",
                    "config mode",
                    Level::Warn,
                    format!(
                        "{} is {mode:03o}: other users can read it, and it can hold secrets \
                         (`chmod 600 {}`)",
                        config_path.display(),
                        config_path.display()
                    ),
                );
            }
            c
        }
        Err(e) => {
            r.add("ptyro", "config", Level::Problem, format!("{}: {e}", config_path.display()));
            Config::default()
        }
    };
    r.add(
        "ptyro",
        "version",
        Level::Ok,
        format!(
            "ptyro {} at {}",
            env!("CARGO_PKG_VERSION"),
            std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default()
        ),
    );
    if let Some(w) = ptyro_core::wsl::detect() {
        wsl(ctx, &config, w, &mut r).await;
    }
    daemon(ctx, &config, quick, &mut r).await;
    #[cfg(windows)]
    if ctx.local {
        conpty(&mut r);
    }
    remote(ctx, &config, &mut r);
    scratchpad(ctx, &config, &mut r);
    shell(&config, &mut r);
    if terminal_too {
        terminal(&mut r).await;
        windows_terminal(&config, &mut r);
    }
    git(&mut r).await;
    agents(&config, &mut r).await;
    profiles(ctx, &config, None, &mut r).await;
    r
}

/// Inside WSL (WIN2, SPEC §3.4): which one, whether Windows programs can be started from it,
/// whether ptyro keeps running with every terminal closed, whether a phone can reach the web
/// app, and projects that WSL makes slow or breaks.
async fn wsl(ctx: &Ctx, config: &Config, w: &ptyro_core::wsl::Wsl, r: &mut Report) {
    const S: &str = "wsl";
    if w.version == 1 {
        r.add(
            S,
            "version",
            Level::Warn,
            format!(
                "{}: ptyro supports WSL2 (in PowerShell on Windows: wsl --set-version {} 2)",
                w.describe(),
                w.distro.as_deref().unwrap_or("<distro>")
            ),
        );
    } else {
        r.add(
            S,
            "version",
            Level::Ok,
            format!("{}, Windows drives under {}", w.describe(), w.mount_root),
        );
    }
    if w.interop {
        r.add(S, "interop", Level::Ok, "Windows programs can be started from here");
    } else {
        let conf = std::fs::read_to_string("/etc/wsl.conf").unwrap_or_default();
        r.add(S, "interop", Level::Warn, interop_off(&conf));
    }
    // WSL's default NAT network hides the distro from the Wi-Fi: `access = open` reaches only
    // this computer. Mirrored networking or Tailscale reach a phone.
    if config.remote.access == ptyro_core::config::Access::Open {
        let mode =
            run(&["wslinfo".into(), "--networking-mode".into()], &[], Duration::from_secs(3))
                .await
                .filter(|(ok, _)| *ok)
                .map(|(_, m)| m)
                .unwrap_or_else(|| "nat".into());
        if mode.trim() == "nat" {
            r.add(
                S,
                "network",
                Level::Warn,
                "the web app is open to the network, but WSL's NAT networking hides it from your \
                 Wi-Fi: a phone can't reach it. Set networkingMode=mirrored under [wsl2] in \
                 %USERPROFILE%\\.wslconfig (then `wsl --shutdown` in Windows), or use Tailscale \
                 (Settings → remote → access)",
            );
        }
    }
    let c = ctx.connect(false).await.ok();
    let status = match &c {
        Some(c) => match c.request(Request::Status).await {
            Ok(Reply::Status { status }) => Some(status),
            _ => None,
        },
        None => None,
    };
    let (level, text) =
        survival(status.as_ref().map(|s| s.wsl_hold.as_ref()), config.daemon.hold_wsl);
    r.add(S, "survival", level, text);
    let Some(c) = c else { return };
    let Ok(Reply::Projects { projects }) = c.request(Request::ProjectList).await else { return };
    for p in projects.iter().filter(|p| p.machine.is_none()) {
        if on_windows_drive(w, &p.root) {
            r.add(
                S,
                &p.slug,
                Level::Warn,
                format!(
                    "{} is on a Windows drive: git, builds and agents run many times slower there \
                     than inside the distro. Clone it into the distro (~/dev/{}) and add that copy",
                    p.root.display(),
                    p.slug
                ),
            );
        }
        let Ok(pc) = ptyro_core::project::ProjectConfig::load(&p.root, &p.root) else { continue };
        let crlf = crlf_scripts(&p.root, &pc.scripts);
        if !crlf.is_empty() {
            r.add(
                S,
                &p.slug,
                Level::Warn,
                format!(
                    "{} {} Windows (CRLF) line endings: Linux runs it as `bash\\r` and fails. \
                     Save it with LF endings (and `git config core.autocrlf input` stops it coming back)",
                    crlf.join(", "),
                    if crlf.len() == 1 { "has" } else { "have" }
                ),
            );
        }
    }
}

/// Whether ptyro keeps running once the last WSL terminal closes (SPEC §20.10): the daemon's hold
/// on WSL, as its status says. `status`: `None` when no daemon runs, `Some(None)` from a daemon
/// that predates the hold.
pub(crate) fn survival(
    status: Option<Option<&ptyro_protocol::WslHold>>,
    wanted: bool,
) -> (Level, String) {
    const STOPS: &str = "WSL stops ptyro, and every agent in it, about 15 seconds after you close the last terminal";
    match status {
        None if wanted => (
            Level::Ok,
            "the daemon isn't running; when it is, it keeps WSL running, so agents keep working \
             with every terminal closed"
                .into(),
        ),
        None => (
            Level::Warn,
            format!("[daemon] hold_wsl is off: {STOPS}. Turn it on in Settings → daemon"),
        ),
        Some(None) => (
            Level::Warn,
            format!(
                "the running daemon is older than this ptyro and doesn't hold WSL open: {STOPS}. \
                 `ptyro daemon upgrade` (sessions keep running)"
            ),
        ),
        Some(Some(h)) if h.holding => (
            Level::Ok,
            format!(
                "the daemon keeps WSL running (wsl.exe, pid {}), so agents keep working with every \
                 terminal closed",
                h.pid.map_or_else(|| "?".into(), |p| p.to_string())
            ),
        ),
        Some(Some(h)) => match h.why_not.as_deref() {
            Some(w) if w.contains("hold_wsl is off") => (
                Level::Warn,
                format!("[daemon] hold_wsl is off: {STOPS}. Turn it on in Settings → daemon"),
            ),
            Some(w) if w.contains("PTYRO_WSL_HOLD") => {
                (Level::Note, format!("the daemon doesn't hold WSL open ({w}): {STOPS}"))
            }
            Some(w) => {
                (Level::Warn, format!("the daemon can't hold WSL open, because {w}: {STOPS}"))
            }
            None => (
                Level::Warn,
                format!(
                    "the daemon's hold on WSL keeps ending ({}; it retries): {STOPS}. \
                     `ptyro daemon logs` says more",
                    h.last_exit.as_deref().unwrap_or("not started yet")
                ),
            ),
        },
    }
}

/// Why Windows programs can't start here, with the fix: interop turned off in `/etc/wsl.conf`, or
/// WSL's binfmt entry gone (another distro's systemd can drop it for the whole VM; SPEC §20.10).
fn interop_off(wsl_conf: &str) -> String {
    let mut section = String::new();
    let off = wsl_conf.lines().any(|l| {
        let l = l.split('#').next().unwrap_or("").trim();
        if let Some(s) = l.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = s.trim().to_ascii_lowercase();
            return false;
        }
        section == "interop"
            && l.split_once('=').is_some_and(|(k, v)| {
                k.trim().eq_ignore_ascii_case("enabled") && v.trim().eq_ignore_ascii_case("false")
            })
    });
    if off {
        "Windows programs can't be started from here, so no toasts, no opening links in Windows, \
         and no hold on WSL: interop is off in /etc/wsl.conf. Remove `enabled = false` under \
         [interop], then restart WSL (`wsl --shutdown` in PowerShell)"
            .into()
    } else {
        "Windows programs can't be started from here, so no toasts, no opening links in Windows, \
         and no hold on WSL: WSL's interop entry is missing (another distro's systemd can remove \
         it). Restore it with `sudo sh -c \"echo :WSLInterop:M::MZ::/init:PF > \
         /proc/sys/fs/binfmt_misc/register\"`, or restart WSL (`wsl --shutdown` in PowerShell)"
            .into()
    }
}

/// `/mnt/c/…`: the project lives on a Windows drive, not in the distro.
fn on_windows_drive(w: &ptyro_core::wsl::Wsl, root: &Path) -> bool {
    w.windows_path(root).is_some_and(|p| p.as_bytes().get(1) == Some(&b':'))
}

/// The files a project's scripts run (any word of a command naming a file in the repo) that have
/// CRLF line endings.
fn crlf_scripts(root: &Path, scripts: &ptyro_core::project::Scripts) -> Vec<String> {
    let commands = [&scripts.setup, &scripts.archive, &scripts.after_turn]
        .into_iter()
        .flatten()
        .chain(scripts.run.values().map(|r| &r.command));
    let mut out: Vec<String> = Vec::new();
    for word in commands.flat_map(|c| c.split_whitespace()) {
        let word = word.trim_matches(['"', '\'']);
        let word = word.strip_prefix("./").unwrap_or(word);
        if word.starts_with('-') || word.starts_with('/') || out.iter().any(|o| o == word) {
            continue;
        }
        let f = root.join(word);
        if f.is_file() && std::fs::read(&f).is_ok_and(|b| b.windows(2).any(|p| p == b"\r\n")) {
            out.push(word.to_owned());
        }
    }
    out
}

/// Shell sessions (WIN1, SPEC §8.7): the shell they run exists, and whether ptyro's integration
/// reaches it. A PowerShell that's installed but not the default is mentioned, with how to use it.
fn shell(config: &Config, r: &mut Report) {
    use ptyro_core::shell::ShellKind;
    const S: &str = "shell";
    let argv = if config.shell.command.is_empty() {
        ptyro_core::shell::default_shell_argv()
    } else {
        config.shell.command.clone()
    };
    let prog = &argv[0];
    let found = if prog.contains('/') { Path::new(prog).is_file() } else { which(prog).is_some() };
    if !found {
        r.add(
            S,
            "command",
            Level::Problem,
            format!("{prog} not found: new shell sessions can't start (install it, or set [shell] command)"),
        );
        return;
    }
    r.add(S, "command", Level::Ok, format!("new shell sessions run `{}`", argv.join(" ")));
    let kind = ptyro_core::shell::interactive(&argv);
    match (config.shell.integration, kind) {
        (true, Some(k)) => r.add(
            S,
            "integration",
            Level::Ok,
            format!("added to the {} sessions ptyro starts: prompt jumps, working/idle, each command and the directory", k.name()),
        ),
        (true, None) => r.add(
            S,
            "integration",
            Level::Note,
            if ShellKind::of_program(prog).is_some() {
                format!(
                    "not added to `{}`: only to a bare bash, zsh, fish or PowerShell, with at most login switches",
                    argv.join(" ")
                )
            } else {
                format!(
                    "ptyro has none for `{}` (it has bash, zsh, fish and PowerShell): prompt jumps, status and the directory need a shell that sends OSC 133 and OSC 7 itself",
                    argv.join(" ")
                )
            },
        ),
        (false, k) => {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            let rc = k.map(|k| k.rc_line(&home));
            let has = rc.as_ref().is_some_and(|(f, _)| {
                std::fs::read_to_string(f).is_ok_and(|t| t.contains("ptyro shell-init"))
            });
            let text = match (&rc, has) {
                (Some((f, _)), true) => format!("off; {} sets it up instead", f.display()),
                (Some((f, line)), false) => format!(
                    "off, and not in {}: shell sessions have no prompt marks (turn [shell] integration back on, or add `{line}` there)",
                    f.display()
                ),
                (None, _) => "off".to_owned(),
            };
            // A choice, not a fault: a note (the TUI's own finding offers to add the line).
            r.add(S, "integration", Level::Note, text);
        }
    }
    if kind != Some(ShellKind::Pwsh)
        && let Some(p) =
            ["pwsh", "pwsh-preview", "powershell"].into_iter().find(|p| which(p).is_some())
    {
        r.add(
            S,
            "powershell",
            Level::Note,
            format!("{p} is installed: `ptyro new -- {p}` starts a PowerShell session, or make it the default with `ptyro config set shell.command \"{p} -Login\"`"),
        );
    }
}

/// The scratchpad (NOTE1): the file can be written, and there is a terminal editor for its pane.
fn scratchpad(ctx: &Ctx, config: &Config, r: &mut Report) {
    let path = config.scratchpad_path(&ctx.paths.data);
    let pad = ptyro_core::scratchpad::Pad::new(
        path.clone(),
        ctx.paths.data.join("scratchpad.history"),
        config.scratchpad.keep,
    );
    match pad.ensure() {
        Ok(()) if std::fs::OpenOptions::new().append(true).open(&path).is_ok() => {
            r.add(
                "scratchpad",
                "file",
                Level::Ok,
                format!("{} (also $PTYRO_SCRATCHPAD)", path.display()),
            );
        }
        Ok(()) => r.add(
            "scratchpad",
            "file",
            Level::Problem,
            format!(
                "{} isn't writable (`chmod u+w {}`, or set [scratchpad] path)",
                path.display(),
                path.display()
            ),
        ),
        Err(e) => r.add(
            "scratchpad",
            "file",
            Level::Problem,
            format!("{}: {e} (set [scratchpad] path in `ptyro config edit`)", path.display()),
        ),
    }
    match config.scratchpad_editor(true) {
        Some(cmd) if which(&cmd[0]).is_some() || cmd[0].contains('/') => {
            r.add("scratchpad", "editor", Level::Ok, cmd.join(" "));
        }
        Some(cmd) => r.add(
            "scratchpad",
            "editor",
            Level::Warn,
            format!("{} not found: the pane can't start it (install it, or set [scratchpad] editor)", cmd[0]),
        ),
        None => r.add(
            "scratchpad",
            "editor",
            Level::Warn,
            "no terminal editor found (install nvim, vim or nano, or set [scratchpad] editor): `C-q ;` can't open the pad",
        ),
    }
}

/// The web app's server (WEB1.9): wanted and up, wanted and failing, or off.
fn web_app(w: &ptyro_protocol::WebServe, r: &mut Report) {
    const S: &str = "remote";
    let mut lines = crate::webapp::lines(w);
    let (level, head) = match (w.wanted, w.up, w.by_hand) {
        (true, true, _) => (Level::Ok, lines.remove(0)),
        // Just started: the certificate and the port take a moment.
        (true, false, _) if w.last_exit.is_none() => {
            (Level::Note, "on, starting (check again in a moment)".into())
        }
        (true, false, None) => (Level::Problem, lines.remove(0)),
        (true, false, Some(_)) => (Level::Warn, lines.remove(0)),
        (false, _, Some(_)) => (Level::Warn, lines.remove(0)),
        (false, _, None) => (
            Level::Note,
            "off: `ptyro web on` has the daemon serve the web app for your phone".into(),
        ),
    };
    let rest = lines
        .into_iter()
        .filter(|l| {
            !l.starts_with("its log")
                && !(w.up && l.starts_with("restarted"))
                && !l.starts_with("certificate:")
        })
        .collect::<Vec<_>>();
    let text = std::iter::once(head).chain(rest).collect::<Vec<_>>().join("; ");
    let fix = if level == Level::Problem {
        format!(" ({}; log: {})", crate::webapp::next(w), w.log.display())
    } else {
        String::new()
    };
    r.add(S, "web app", level, format!("{text}{fix}"));
    match (w.up, w.cert.as_str(), &w.cert_note) {
        (true, "tailscale", None) => r.add(
            S,
            "tailscale certificate",
            Level::Ok,
            "phones open this computer's tailnet name with Tailscale's certificate: nothing to install",
        ),
        (true, _, Some(note)) => r.add(
            S,
            "tailscale certificate",
            Level::Warn,
            format!("not in use ({note}); phones trust ptyro's own certificate instead"),
        ),
        _ => {}
    }
}

/// `ptyro serve` (WEB1.7): plain HTTP where tokens would cross a network, and its certificate.
fn remote(ctx: &Ctx, config: &Config, r: &mut Report) {
    use ptyro_core::config::Access;
    if config.remote.serve {
        match config.remote.access {
            Access::Open => r.add(
                "remote",
                "web app reach",
                Level::Note,
                "`[remote] access = \"open\"`: every network interface answers (HTTPS; a device still needs a pairing token). `tailscale` keeps it to your tailnet, `localhost` to this computer",
            ),
            Access::Tailscale if crate::remote::tailscale_ips().is_empty() => r.add(
                "remote",
                "web app reach",
                Level::Warn,
                "`[remote] access = \"tailscale\"` but Tailscale isn't up on this computer, so only this computer reaches the web app until it is (it starts listening on the tailnet by itself). Start Tailscale, or pick `open` for the Wi-Fi",
            ),
            _ => {}
        }
    }
    let Some(ends) = crate::tls::expires(&crate::tls::dir(&ctx.paths.data)) else { return };
    let days = (ends - time::OffsetDateTime::now_utc()).whole_days();
    let (level, text) = if days < 0 {
        (Level::Problem, "ptyro serve's certificate has ended: `ptyro serve` makes a new one when it starts (`ptyro daemon restart` restarts a managed one)".to_owned())
    } else if days < crate::tls::RENEW_DAYS {
        (
            Level::Warn,
            format!(
                "ptyro serve's certificate ends in {days} days and wasn't renewed: `ptyro serve` renews it when it starts (`ptyro daemon restart` restarts a managed one)"
            ),
        )
    } else {
        (Level::Ok, format!("ptyro serve's HTTPS certificate is good for {days} more days"))
    };
    r.add("remote", "certificate", level, text);
}

/// `ptyro profile doctor [NAME]`.
pub async fn profile_doctor(ctx: &Ctx, only: Option<String>, json: bool) -> Result<i32, Error> {
    let r = profile_checks(ctx, only).await?;
    print(&r, json);
    Ok(exit_code(&r))
}

/// The profile checks alone.
pub(crate) async fn profile_checks(ctx: &Ctx, only: Option<String>) -> Result<Report, Error> {
    let config =
        Config::load(&Config::path(&ctx.paths)).map_err(|e| Error::invalid(e.to_string()))?;
    let mut r = Report::default();
    profiles(ctx, &config, only, &mut r).await;
    Ok(r)
}

fn exit_code(r: &Report) -> i32 {
    if r.0.iter().any(|c| c.level == Level::Problem) { 1 } else { 0 }
}

pub(crate) fn print_checks(r: &Report) {
    for c in &r.0 {
        let mark = match c.level {
            Level::Ok => "✓",
            Level::Note => "·",
            Level::Warn => "!",
            Level::Problem => "✗",
        };
        eprintln!("  {mark} {:<12} {}", c.name, c.text);
    }
}

fn print(r: &Report, json: bool) {
    if json {
        println!("{}", serde_json::to_string_pretty(&r.0).unwrap_or_default());
        return;
    }
    let mut section = "";
    for c in &r.0 {
        if c.section != section {
            section = &c.section;
            println!("{section}");
        }
        let mark = match c.level {
            Level::Ok => "✓",
            Level::Note => "·",
            Level::Warn => "!",
            Level::Problem => "✗",
        };
        println!("  {mark} {:<12} {}", c.name, c.text);
    }
    let (w, p) = (
        r.0.iter().filter(|c| c.level == Level::Warn).count(),
        r.0.iter().filter(|c| c.level == Level::Problem).count(),
    );
    println!();
    println!(
        "{}",
        match (w, p) {
            (0, 0) => "all good".to_owned(),
            _ => format!("{p} problem(s), {w} warning(s)"),
        }
    );
}

async fn daemon(ctx: &Ctx, config: &Config, quick: bool, r: &mut Report) {
    const S: &str = "daemon";
    // The socket directory must be private (SPEC §17). Windows has no socket of its own.
    #[cfg(unix)]
    if let Some(dir) = ctx.paths.socket.parent() {
        use std::os::unix::fs::MetadataExt;
        match std::fs::metadata(dir) {
            Ok(m) if m.mode() & 0o077 != 0 => r.add(
                S,
                "socket dir",
                Level::Problem,
                format!("{} is mode {:o}; it must be 0700", dir.display(), m.mode() & 0o777),
            ),
            Ok(m) if m.uid() != rustix::process::getuid().as_raw() => {
                r.add(S, "socket dir", Level::Problem, format!("{} isn't yours", dir.display()))
            }
            Ok(_) => r.add(S, "socket dir", Level::Ok, format!("{} (private)", dir.display())),
            Err(_) => {}
        }
    }
    let c = match ctx.connect(!quick).await {
        Ok(c) => c,
        Err(e) if e.code == ErrorCode::DaemonUnavailable => {
            r.add(S, "running", Level::Note, "not running (it starts on demand)");
            return;
        }
        Err(e) => {
            r.add(S, "running", Level::Problem, e.message);
            return;
        }
    };
    if let Ok(Reply::Status { status }) = c.request(Request::Status).await {
        let same = status.version == env!("CARGO_PKG_VERSION") && status.proto == PROTO;
        r.add(
            S,
            "running",
            if same { Level::Ok } else { Level::Warn },
            format!(
                "pid {}, version {} (protocol {}.{}){}",
                status.pid,
                status.version,
                status.proto.major,
                status.proto.minor,
                if same {
                    ""
                } else {
                    "; not this ptyro: `ptyro daemon upgrade` (sessions keep running)"
                }
            ),
        );
    }
    if let Ok(Reply::Web { web, .. }) = c.request(Request::Web { on: None, via: None }).await {
        web_app(&web, r);
    }
    let Ok(Reply::Doctor { report }) = c.request(Request::Doctor).await else { return };
    for v in &report.leaked_vars {
        r.add(
            S,
            "environment",
            Level::Note,
            format!("{v} is set in the daemon's environment; sessions don't inherit it (profiles set it)"),
        );
    }
    // A daemon started by a service manager can have a short PATH: agents then "don't exist".
    // (The daemon also looks where npm, nvm and `npx` put agents: only nowhere is a problem.)
    for agent in ptyro_agents::names(config) {
        let Some(argv) = ptyro_agents::command(config, &agent) else { continue };
        if which(&argv[0]).is_some()
            && !argv[0].contains('/')
            && ptyro_agents::locate::find(&argv[0], &report.path).is_none()
        {
            r.add(
                S,
                "PATH",
                Level::Problem,
                format!("{} is on your PATH but not on the daemon's: sessions can't start it (reinstall the service, or set [agents.{agent}] command to a full path)", argv[0]),
            );
        }
    }
    for w in &report.warnings {
        r.add(S, "noticed", Level::Warn, w.clone());
    }
    if let Some(c) = &report.chats {
        let bad = !c.starts_with("chats: ") || c.contains("integrity") || c.contains("FTS5");
        let level = if bad { Level::Problem } else { Level::Ok };
        r.add(S, "chats", level, c.clone());
    }
    if let Some(m) = &report.meaning {
        // Meaning search (SPEC §22.10): a download or a model that failed is worth a look.
        let level = if m.contains("didn't") || m.contains("couldn't") || m.contains("damaged") {
            Level::Warn
        } else {
            Level::Ok
        };
        r.add(S, "chat meaning", level, m.clone());
    }
    worktrees(&c, config, r).await;
    if quick {
        return;
    }
    // The hook self-test: the fake agent through each per-launch integration.
    for adapter in ["claude", "codex", "opencode"] {
        match c.request(Request::SelfTest { adapter: adapter.into() }).await {
            Ok(Reply::SelfTest { result }) => r.add(
                "integrations",
                adapter,
                Level::Ok,
                format!("self-test passed: {}", result.seen.join(" → ")),
            ),
            Ok(_) => {}
            Err(e) => r.add("integrations", adapter, Level::Problem, e.message),
        }
    }
}

/// Worktrees left behind once their work is done (and their build output with them): a warning,
/// so the TUI's health badge and the web's Health page say so and offer the clean-up list.
/// Archives are only a note: they take next to nothing.
async fn worktrees(c: &ptyro_client::Client, config: &Config, r: &mut Report) {
    const S: &str = "worktrees";
    if let Ok(Reply::Gc { entries }) = c.request(Request::Gc).await {
        r.add(S, "stale", stale_level(&entries), stale_text(&entries, config.worktrees.stale_days));
    }
    if let Ok(Reply::Archives { entries }) =
        c.request(Request::WorktreeArchives { project: None }).await
        && let Some(oldest) = entries.iter().map(|e| e.archived_ms).min()
    {
        let purge = match config.worktrees.archive_purge_days {
            0 => "kept until you delete them (`ptyro wt archive-rm`, or set [worktrees] archive_purge_days)".to_owned(),
            d => format!("deleted after {d} days"),
        };
        r.add(
            S,
            "archived",
            Level::Note,
            format!(
                "{} archived, the oldest {} ago; {purge}",
                entries.len(),
                crate::cli::age(oldest)
            ),
        );
    }
}

fn stale_level(entries: &[ptyro_core::model::GcEntry]) -> Level {
    if entries.is_empty() { Level::Ok } else { Level::Warn }
}

/// "3 stale worktrees, taking 12.4 GiB (2 merged, 1 unused for 14+ days): `ptyro gc` …".
fn stale_text(entries: &[ptyro_core::model::GcEntry], stale_days: u32) -> String {
    if entries.is_empty() {
        return "nothing stale".into();
    }
    let any = |e: &ptyro_core::model::GcEntry, words: &[&str]| {
        e.reasons.iter().any(|r| words.iter().any(|w| r.contains(w)))
    };
    let merged = entries.iter().filter(|e| any(e, &["merged", "integrated"])).count();
    let gone = entries.iter().filter(|e| any(e, &["gone"])).count();
    let unused = entries.len() - merged - gone;
    let why: Vec<String> = [
        (merged, "merged".to_owned()),
        (unused, format!("unused for {stale_days}+ days")),
        (gone, "gone from disk".to_owned()),
    ]
    .into_iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} {what}"))
    .collect();
    let bytes: u64 = entries.iter().filter_map(|e| e.bytes).sum();
    let partial = entries.iter().any(|e| e.bytes_partial);
    let size = match (bytes, partial) {
        (0, _) => String::new(),
        (b, false) => format!(", taking {}", ptyro_core::model::human_bytes(b)),
        (b, true) => format!(", taking at least {}", ptyro_core::model::human_bytes(b)),
    };
    let n = entries.len();
    format!(
        "{n} stale worktree{}{size} ({}): `ptyro gc` archives or removes them; lock one to keep it",
        if n == 1 { "" } else { "s" },
        why.join(", ")
    )
}

/// Which ConPTY a native Windows daemon's sessions run in (WIN5, SPEC §20.13), when it is the
/// packaged one. Windows' own is the daemon's warning (shown under `daemon`, and in the TUI and
/// the web), not said twice here.
#[cfg(windows)]
fn conpty(r: &mut Report) {
    if let Some(dll) = ptyro_pty::packaged_conpty() {
        r.add(
            "windows",
            "conpty",
            Level::Ok,
            format!(
                "the packaged ConPTY ({}): programs' output passes through as it is",
                dll.display()
            ),
        );
    }
}

/// Inside Windows Terminal (`WT_SESSION`, from Windows or WSL): a key binding of its own that
/// takes ptyro's prefix, so the key never reaches ptyro (WIN3). Its built-in bindings don't use
/// Ctrl+Q or Ctrl+Space \[S\], so only the user's `settings.json` can.
fn windows_terminal(config: &Config, r: &mut Report) {
    if std::env::var_os("WT_SESSION").is_none_or(|v| v.is_empty()) {
        return;
    }
    let prefixes: Vec<String> = config.ui.prefix.keys().iter().filter_map(|k| wt_key(k)).collect();
    for file in wt_settings_files() {
        let Ok(text) = std::fs::read_to_string(&file) else { continue };
        let taken = wt_takes(&text, &prefixes);
        if taken.is_empty() {
            r.add(
                "terminal",
                "windows terminal",
                Level::Ok,
                "its key bindings leave ptyro's prefix alone",
            );
        } else {
            r.add(
                "terminal",
                "windows terminal",
                Level::Warn,
                format!(
                    "Windows Terminal binds {} ({}), so it never reaches ptyro: remove that binding \
                     there, or change ptyro's prefix (Settings → interface → prefix)",
                    taken.join(" and "),
                    file.display()
                ),
            );
        }
        return;
    }
}

/// ptyro's key (`C-q`, `C-Space`, `M-x`) as Windows Terminal writes it (`ctrl+q`, `ctrl+space`).
pub(crate) fn wt_key(k: &str) -> Option<String> {
    let mut mods: Vec<&str> = Vec::new();
    let mut rest = k;
    loop {
        if let Some(r) = rest.strip_prefix("C-") {
            mods.push("ctrl");
            rest = r;
        } else if let Some(r) = rest.strip_prefix("M-") {
            mods.push("alt");
            rest = r;
        } else if let Some(r) = rest.strip_prefix("S-") {
            mods.push("shift");
            rest = r;
        } else {
            break;
        }
    }
    if rest.is_empty() {
        return None;
    }
    mods.push(rest);
    Some(mods.join("+").to_ascii_lowercase())
}

/// Which of `keys` (Windows Terminal's spelling) `settings.json` binds to something. An entry
/// that frees a key (`"command": "unbound"` or `null`) doesn't count. JSON with comments and
/// trailing commas, as Windows Terminal allows: each `"keys"` is looked at in its own `{…}`.
pub(crate) fn wt_takes(settings: &str, keys: &[String]) -> Vec<String> {
    let norm = |s: &str| s.to_ascii_lowercase().replace(' ', "");
    let mut out: Vec<String> = Vec::new();
    let mut at = 0;
    while let Some(i) = settings[at..].find("\"keys\"") {
        let i = at + i;
        at = i + 6;
        let start = settings[..i].rfind('{').unwrap_or(0);
        let end = settings[i..].find('}').map_or(settings.len(), |e| i + e);
        let entry = &settings[start..end];
        let freed = entry.contains("\"unbound\"")
            || entry
                .split("\"command\"")
                .nth(1)
                .is_some_and(|c| c.trim_start_matches([' ', ':']).starts_with("null"));
        if freed {
            continue;
        }
        // The value: a string, or an array of them, after `"keys":`.
        let value = &settings[i + 6..end];
        let value = value.trim_start_matches([' ', '\t', ':']);
        for k in keys {
            let found =
                value.split(['"', ',']).map(norm).take_while(|w| !w.contains('}')).any(|w| w == *k);
            if found && !out.contains(k) {
                out.push(k.clone());
            }
        }
    }
    out
}

/// Windows Terminal's `settings.json` (the Store build, its Preview, an unpackaged one), from
/// Windows or from WSL.
fn wt_settings_files() -> Vec<std::path::PathBuf> {
    let local = if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from)
    } else {
        ptyro_core::wsl::detect().and_then(|w| w.windows_home()).map(|h| h.join("AppData/Local"))
    };
    let Some(local) = local else { return Vec::new() };
    [
        "Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState/settings.json",
        "Packages/Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe/LocalState/settings.json",
        "Microsoft/Windows Terminal/settings.json",
    ]
    .iter()
    .map(|p| local.join(p))
    .filter(|p| p.is_file())
    .collect()
}

pub(crate) async fn terminal(r: &mut Report) {
    const S: &str = "terminal";
    let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let term = var("TERM").unwrap_or_else(|| "(unset)".into());
    let program = var("TERM_PROGRAM").map(|p| format!(" in {p}")).unwrap_or_default();
    r.add(S, "TERM", Level::Ok, format!("{term}{program}"));
    match var("COLORTERM").as_deref() {
        Some("truecolor" | "24bit") => r.add(S, "colour", Level::Ok, "truecolor"),
        _ => r.add(
            S,
            "colour",
            Level::Note,
            "COLORTERM doesn't say truecolor; colours may be reduced",
        ),
    }
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) && var("TMUX").is_none() {
        match crossterm::terminal::supports_keyboard_enhancement() {
            Ok(true) => r.add(S, "keys", Level::Ok, "kitty keyboard protocol (Shift+Enter reaches agents)"),
            Ok(false) => r.add(
                S,
                "keys",
                Level::Note,
                "no kitty keyboard protocol: Shift+Enter can't be told from Enter (Ghostty, kitty, WezTerm, foot have it)",
            ),
            Err(_) => {}
        }
    }
    if var("TMUX").is_some() {
        let opt = |name: &'static str| async move {
            run(
                &["tmux".into(), "show".into(), "-gv".into(), name.into()],
                &[],
                Duration::from_secs(2),
            )
            .await
            .map(|(_, v)| v)
        };
        for (name, want, why) in [
            (
                "allow-passthrough",
                "on",
                "notifications and clipboard escapes can't reach your terminal",
            ),
            ("extended-keys", "on", "Shift+Enter and other modified keys don't reach ptyro"),
            ("set-clipboard", "on", "copies don't reach your system clipboard"),
        ] {
            match opt(name).await.as_deref() {
                Some(v) if v == want || (name == "set-clipboard" && v == "external") => {
                    r.add("tmux", name, Level::Ok, v.to_owned())
                }
                Some(v) => r.add(
                    "tmux",
                    name,
                    Level::Warn,
                    format!("is {v}: {why}; add `set -g {name} {want}` to ~/.tmux.conf"),
                ),
                None => {}
            }
        }
    }
}

pub(crate) async fn git(r: &mut Report) {
    match run(&["git".into(), "--version".into()], &[], Duration::from_secs(3)).await {
        Some((_, out)) => {
            let v = first_version(&out).unwrap_or_default();
            let parts: Vec<u32> = v.split('.').filter_map(|p| p.parse().ok()).collect();
            let old = parts.as_slice() < [2, 40].as_slice();
            r.add(
                "git",
                "version",
                if old { Level::Warn } else { Level::Ok },
                if old {
                    format!("{v}: ptyro is tested with git 2.53; worktree features may be missing")
                } else {
                    v
                },
            );
        }
        None => {
            r.add("git", "version", Level::Problem, "git not found: projects and worktrees need it")
        }
    }
    if which("gh").is_none() {
        r.add("git", "gh", Level::Note, "gh not found: `wt new --pr` and PR features need it");
    }
}

pub(crate) async fn agents(config: &Config, r: &mut Report) {
    const S: &str = "agents";
    let path_var = std::env::var("PATH").unwrap_or_default();
    let mut any = false;
    for agent in ptyro_agents::names(config) {
        let Some(argv) = ptyro_agents::command(config, &agent) else { continue };
        let configured = config.agents.contains_key(&agent);
        let Some(found) = ptyro_agents::locate::find(&argv[0], &path_var) else {
            if configured {
                let fix = ptyro_agents::locate::install_hint(&argv[0])
                    .map(|h| format!(": `{h}` installs it"))
                    .unwrap_or_default();
                r.add(S, &agent, Level::Problem, format!("{} not found{fix}", argv[0]));
            }
            continue;
        };
        any |= agent != "antigravity";
        let path = PathBuf::from(found.describe());
        let adapter = ptyro_agents::adapter_name(config, &agent);
        // `fake-agent` and wrappers may not answer --version: only ask real CLIs.
        let version = if argv.len() == 1 {
            let mut v = found.argv.clone();
            v.push("--version".into());
            run(&v, &[], Duration::from_secs(5)).await.and_then(|(_, out)| first_version(&out))
        } else {
            None
        };
        let tested = TESTED.iter().find(|(a, _)| *a == adapter).map(|(_, v)| *v);
        let text = match (&version, tested) {
            (Some(v), Some(t)) if v == t => format!("{v} ({})", path.display()),
            (Some(v), Some(t)) => format!("{v}, verified with {t} ({})", path.display()),
            (Some(v), None) => format!("{v} ({})", path.display()),
            (None, _) => format!("{} (adapter {adapter})", path.display()),
        };
        let level = match (&version, tested) {
            (Some(v), Some(t)) if v.split('.').next() != t.split('.').next() => Level::Warn,
            _ => Level::Ok,
        };
        r.add(S, &agent, level, text);
        // `npx` keeps its copy in a cache npm may clear: say how to keep it for good.
        if found.via.as_deref() == Some("npx cache")
            && let Some(h) = ptyro_agents::locate::install_hint(&argv[0])
        {
            r.add(
                S,
                &agent,
                Level::Note,
                format!("runs from npx's cache, which npm may clear: `{h}` installs it for good"),
            );
        }
    }
    if !any {
        r.add(
            S,
            "agents",
            Level::Warn,
            "no agent found: install one, e.g. `npm i -g @google/gemini-cli` (Gemini CLI), `npm i -g @openai/codex` (Codex) or `curl -fsSL https://claude.ai/install.sh | bash` (Claude Code)",
        );
    }
    // Gemini CLI stopped serving unpaid / Google One accounts on 2026-06-18 (RESEARCH G3).
    let gemini_key = std::env::var_os("GEMINI_API_KEY").is_some()
        || std::env::var_os("GOOGLE_CLOUD_PROJECT").is_some()
        || config
            .profiles
            .values()
            .any(|p| p.gemini.as_ref().and_then(|g| g.def()).is_some_and(|d| d.api_key.is_some()));
    if ptyro_agents::locate::find("gemini", &path_var).is_some() && !gemini_key {
        r.add(
            S,
            "gemini",
            Level::Note,
            "Gemini CLI no longer serves consumer Google accounts (since 2026-06-18); use the Antigravity CLI (`agy`) with a personal account",
        );
    }
}

/// Logged in, and as whom, from the tool's own status command.
async fn login(agent: &str, dir: &Path, custom: bool) -> Option<(Level, String)> {
    let var = ptyro_agents::dir_var(agent)?;
    // The default location stays implicit: `CLAUDE_CONFIG_DIR=~/.claude` would move
    // `.claude.json` and look like a different account.
    let env: Vec<(String, PathBuf)> =
        if custom { vec![(var.to_owned(), dir.to_path_buf())] } else { Vec::new() };
    let t = Duration::from_secs(8);
    match agent {
        "claude" => {
            let (_, out) =
                run(&["claude".into(), "auth".into(), "status".into(), "--json".into()], &env, t)
                    .await?;
            let v: serde_json::Value = serde_json::from_str(&out).ok()?;
            if v["loggedIn"].as_bool() == Some(true) {
                let who = v["email"].as_str().unwrap_or("?");
                let org = v["orgName"].as_str().map(|o| format!(", {o}")).unwrap_or_default();
                let plan =
                    v["subscriptionType"].as_str().map(|p| format!(" ({p})")).unwrap_or_default();
                Some((Level::Ok, format!("logged in as {who}{org}{plan}")))
            } else {
                Some((Level::Warn, "not logged in (`ptyro profile login <profile> claude`)".into()))
            }
        }
        "codex" => {
            let (ok, out) =
                run(&["codex".into(), "login".into(), "status".into()], &env, t).await?;
            let line = out.lines().next().unwrap_or("").to_owned();
            Some(if ok {
                (Level::Ok, line)
            } else {
                (Level::Warn, format!("not logged in: {line}"))
            })
        }
        "gh" => {
            let (ok, out) = run(&["gh".into(), "auth".into(), "status".into()], &env, t).await?;
            let who = out
                .lines()
                .find(|l| l.contains("Logged in to"))
                .map(|l| l.trim().trim_start_matches('✓').trim().to_owned());
            Some(if ok {
                (Level::Ok, who.unwrap_or_else(|| "logged in".into()))
            } else {
                (Level::Warn, "not logged in (`ptyro profile login <profile> gh`)".into())
            })
        }
        _ => None,
    }
}

async fn profiles(ctx: &Ctx, config: &Config, only: Option<String>, r: &mut Report) {
    let root = ctx.paths.data.join("profiles");
    for name in profile::names(config) {
        if only.as_ref().is_some_and(|o| *o != name) {
            continue;
        }
        let section = format!("profile {name}");
        let info = profile::info(config, &name, &root);
        for (agent, dir) in &info.dirs {
            let prog = if agent == "gh" { "gh" } else { agent.as_str() };
            if which(prog).is_none() {
                continue;
            }
            let custom = ptyro_agents::default_dir(agent).as_deref() != Some(dir.as_path());
            let shown = dir.display().to_string();
            if !dir.exists() {
                r.add(
                    &section,
                    agent,
                    Level::Note,
                    format!(
                        "{shown}: no dir yet (`ptyro profile login {name} {agent}` creates it)"
                    ),
                );
                continue;
            }
            let (level, state) = match login(agent, dir, custom).await {
                Some(x) => x,
                None => {
                    let cred = dir.join(crate::profiles::credential_file(agent));
                    if cred.exists() {
                        (
                            Level::Ok,
                            format!(
                                "logged in ({} present)",
                                crate::profiles::credential_file(agent)
                            ),
                        )
                    } else {
                        (
                            Level::Note,
                            format!(
                                "not logged in? (no {})",
                                crate::profiles::credential_file(agent)
                            ),
                        )
                    }
                }
            };
            r.add(&section, agent, level, format!("{state} · {shown}"));
        }
        match &info.git_email {
            Some(e) => r.add(&section, "git", Level::Ok, e.clone()),
            None if info.defined && name != DEFAULT_PROFILE => {
                r.add(&section, "git", Level::Note, "inherits your global git identity")
            }
            None => {}
        }
        if let Some(def) = config.profiles.get(&name)
            && let Some(key) = def.git.as_ref().and_then(|g| g.ssh_key.as_deref())
            && !expand_home(key).exists()
        {
            r.add(&section, "ssh key", Level::Problem, format!("{key} does not exist"));
        }
    }
    // Pins that contradict the match rules, and pins to profiles that are gone.
    if let Ok(c) = ctx.connect(false).await
        && let Ok(Reply::Projects { projects }) = c.request(Request::ProjectList).await
    {
        for p in projects {
            let remotes: Vec<String> = p.remote_url.iter().cloned().collect();
            let claim = profile::claimants(config, &remotes, &p.root);
            if let Some(pin) = &p.pin {
                if !claim.is_empty() && !claim.contains(pin) {
                    r.add(
                        "projects",
                        &p.slug,
                        Level::Problem,
                        format!("pinned to {pin} but matches {}", claim.join(", ")),
                    );
                }
                if !profile::exists(config, pin) {
                    r.add(
                        "projects",
                        &p.slug,
                        Level::Problem,
                        format!("pinned to {pin}, which isn't defined"),
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn wsl_survival_says_whether_the_daemon_holds_wsl_and_what_to_do() {
        use super::{Level, survival};
        use ptyro_protocol::WslHold;
        let held = WslHold { holding: true, pid: Some(42), ..WslHold::default() };
        let (l, t) = survival(Some(Some(&held)), true);
        assert!(l == Level::Ok && t.contains("pid 42"), "{t}");
        let off =
            WslHold { why_not: Some("[daemon] hold_wsl is off".into()), ..WslHold::default() };
        let (l, t) = survival(Some(Some(&off)), false);
        assert!(
            l == Level::Warn && t.contains("hold_wsl is off") && t.contains("15 seconds"),
            "{t}"
        );
        assert_eq!(survival(None, true).0, Level::Ok, "no daemon yet, the setting on");
        assert_eq!(survival(None, false).0, Level::Warn, "no daemon, the setting off");
        let (l, t) = survival(Some(None), true);
        assert!(l == Level::Warn && t.contains("`ptyro daemon upgrade`"), "an older daemon: {t}");
        let no = WslHold { why_not: Some("interop is off".into()), ..WslHold::default() };
        assert!(survival(Some(Some(&no)), true).1.contains("because interop is off"));
        let tests = WslHold { why_not: Some("PTYRO_WSL_HOLD=0".into()), ..WslHold::default() };
        assert_eq!(survival(Some(Some(&tests)), true).0, Level::Note);
        let dying = WslHold { last_exit: Some("wsl.exe exited (1)".into()), ..WslHold::default() };
        let (l, t) = survival(Some(Some(&dying)), true);
        assert!(l == Level::Warn && t.contains("exited (1)"), "{t}");
    }

    #[test]
    fn windows_terminal_bindings_that_take_the_prefix_are_found() {
        use super::{wt_key, wt_takes};
        assert_eq!(wt_key("C-q").as_deref(), Some("ctrl+q"));
        assert_eq!(wt_key("C-Space").as_deref(), Some("ctrl+space"));
        assert_eq!(wt_key("M-x").as_deref(), Some("alt+x"));
        let ours = vec!["ctrl+q".to_owned(), "ctrl+space".to_owned()];
        // The owner's real settings (2026-11-03): nothing of ours.
        let mine = r#"{ "actions": [ { "command": { "action": "copy" }, "id": "User.copy", "keys": "ctrl+shift+c" },
            { "command": "paste", "keys": "ctrl+shift+v" }, ], // trailing comma, a comment
            "keybindings": [ { "id": "User.find", "keys": "ctrl+shift+f" } ] }"#;
        assert!(wt_takes(mine, &ours).is_empty());
        let taken = r#"{ "keybindings": [ { "id": "Terminal.CloseWindow", "keys": "Ctrl+Q" },
            { "command": "unbound", "keys": "ctrl+space" } ] }"#;
        assert_eq!(wt_takes(taken, &ours), ["ctrl+q"], "case and an unbinding");
        let arrays =
            r#"{ "actions": [ { "command": "find", "keys": ["ctrl+space", "ctrl+f"] } ] }"#;
        assert_eq!(wt_takes(arrays, &ours), ["ctrl+space"]);
        let nulled = r#"{ "keybindings": [ { "command": null, "keys": "ctrl+q" } ] }"#;
        assert!(wt_takes(nulled, &ours).is_empty());
    }

    #[test]
    fn interop_off_names_the_cause_and_its_fix() {
        use super::interop_off;
        let t = interop_off("[interop]\nenabled = false\n");
        assert!(t.contains("off in /etc/wsl.conf"), "{t}");
        let t = interop_off("[boot]\nsystemd=true\n[network]\nenabled=false\n");
        assert!(t.contains("binfmt_misc/register") && t.contains("wsl --shutdown"), "{t}");
    }

    #[test]
    fn wsl_finds_projects_on_windows_drives_and_crlf_scripts() {
        use std::path::Path;
        let w = ptyro_core::wsl::from_facts("6.6-microsoft-standard-WSL2", Some("U"), true, "", "")
            .unwrap();
        assert!(super::on_windows_drive(&w, Path::new("/mnt/c/Users/me/src/app")));
        assert!(!super::on_windows_drive(&w, Path::new("/home/me/dev/app")));
        assert!(!super::on_windows_drive(&w, Path::new("/mnt/wsl/shared")));

        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir(t.path().join("scripts")).unwrap();
        std::fs::write(t.path().join("scripts/setup.sh"), "#!/bin/bash\r\necho hi\r\n").unwrap();
        std::fs::write(t.path().join("run.sh"), "#!/bin/sh\necho ok\n").unwrap();
        let scripts: ptyro_core::project::Scripts = toml::from_str(
            "setup = \"bash scripts/setup.sh --quiet\"\narchive = \"./scripts/setup.sh\"\n\
             [run.web]\ncommand = \"./run.sh\"\n[run.db]\ncommand = \"'scripts/setup.sh'\"\n",
        )
        .unwrap();
        assert_eq!(super::crlf_scripts(t.path(), &scripts), ["scripts/setup.sh"]);
        assert!(super::crlf_scripts(t.path(), &Default::default()).is_empty());
    }

    fn stale(name: &str, reason: &str, bytes: Option<u64>) -> ptyro_core::model::GcEntry {
        let worktree = serde_json::from_value(serde_json::json!({
            "id": name, "project": "p", "project_slug": "p", "name": name, "path": "/x",
            "is_root": false, "state": "ready", "created_ms": 0,
        }))
        .unwrap();
        ptyro_core::model::GcEntry {
            worktree,
            reasons: vec![reason.into()],
            bytes,
            bytes_partial: false,
        }
    }

    /// The nudge says how many, why and how much disk, and how to act on it or keep one.
    #[test]
    fn stale_worktrees_are_a_warning_saying_why_and_how_big() {
        use super::{Level, stale_level, stale_text};
        assert_eq!(stale_level(&[]), Level::Ok);
        let entries = [
            stale("a", "branch merged into origin/main", Some(3 << 30)),
            stale("b", "its PR merged", Some(1 << 30)),
            stale("c", "no sessions for 20 days", None),
        ];
        assert_eq!(stale_level(&entries), Level::Warn);
        assert_eq!(
            stale_text(&entries, 14),
            "3 stale worktrees, taking 4.0 GiB (2 merged, 1 unused for 14+ days): `ptyro gc` archives or removes them; lock one to keep it"
        );
        let mut one = stale("d", "its directory is gone", None);
        assert_eq!(
            stale_text(std::slice::from_ref(&one), 14),
            "1 stale worktree (1 gone from disk): `ptyro gc` archives or removes them; lock one to keep it"
        );
        (one.bytes, one.bytes_partial) = (Some(2048), true);
        assert!(stale_text(&[one], 14).contains("taking at least 2.0 KiB"));
    }

    #[test]
    fn versions() {
        assert_eq!(super::first_version("2.1.304 (Claude Code)").as_deref(), Some("2.1.304"));
        assert_eq!(super::first_version("codex-cli 0.143.0").as_deref(), Some("0.143.0"));
        assert_eq!(super::first_version("git version 2.53.0").as_deref(), Some("2.53.0"));
        assert_eq!(super::first_version("gemini v0.64.0").as_deref(), Some("0.64.0"));
    }
}
