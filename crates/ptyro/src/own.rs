//! `ptyro update ptyro` (ROADMAP BOOT6, SPEC §24.8): the CLI's part of ptyro updating itself.
//! The download, check and swap are `ptyro_agents::own`'s; here are the words, the daemon's live
//! upgrade afterwards, and on Windows with WSL the other half: Windows' `ptyro.exe` and the
//! ptyro inside the distro are kept at the same version, each updating itself (`ptyro update
//! ptyro --yes`, run through `wsl.exe` or WSL's interop), so the two never disagree about the
//! flags one passes the other.

use std::path::PathBuf;
use std::process::Stdio;

use ptyro_agents::catalog::{Platform, Version};
use ptyro_agents::own::{self, How, Install};
use ptyro_agents::setup::WslStatus;
use ptyro_agents::updates::Outdated;
use ptyro_client::wsl_setup;
use ptyro_core::paths::Paths;

use ptyro_core::Error;

use crate::cli::Ctx;

/// The question's line for ptyro: what the update does, then the live upgrade.
pub(crate) fn question(o: &Outdated, install: &Install) -> Vec<String> {
    let mut out =
        vec![format!("  ptyro {} → {}: {}", o.version, o.latest, how(install, &o.latest))];
    out.push("    then the daemon is upgraded live: every session carries on".into());
    out
}

/// What the update runs, as the question says it.
fn how(install: &Install, version: &str) -> String {
    match install.how {
        How::Brew => "`brew upgrade ptyro`".into(),
        _ => own::describe(install, version),
    }
}

/// The other half of a Windows install with WSL: the ptyro there, its version, and how to ask it
/// to update itself.
pub(crate) struct Half {
    /// `WSL's ptyro (Ubuntu)`, `Windows' ptyro.exe`.
    pub label: String,
    pub version: Option<String>,
    /// `ptyro update ptyro --yes` there, as a command line to run here.
    argv: Vec<String>,
    /// What to run by hand if it can't update itself (one older than BOOT6 doesn't know how).
    fix: String,
}

/// The other half, when this is one: on Windows the ptyro in the WSL distro your daemon is in
/// (or WSL's default), in WSL Windows' `ptyro.exe`. `None` elsewhere, or when it isn't there;
/// on Windows with a WSL that has a distro but no ptyro that answers, that is said (stderr), so
/// one half is never updated without a word about the other.
pub(crate) async fn other_half(paths: &Paths, platform: Platform) -> Option<Half> {
    if platform.is_windows() {
        let remote = crate::winclient::resolve(None, paths, false).await;
        let prefer =
            remote.as_deref().and_then(ptyro_client::remote::wsl_distro).map(str::to_owned);
        let (distro, version) = match wsl_setup::probe(prefer.as_deref()).await {
            WslStatus::Usable { distro, version } => (distro, version),
            WslStatus::Unusable { reason, .. } => {
                // No WSL, or no distro: there's no other half.
                if !reason.starts_with("WSL isn't installed") && !reason.contains("no Linux distro")
                {
                    eprintln!(
                        "note: WSL's ptyro couldn't be checked ({reason}); if it has one, `wsl ptyro update ptyro` brings it to the same version"
                    );
                }
                return None;
            }
            _ => return None,
        };
        let words = ["update", "ptyro", "--yes"].map(str::to_owned);
        let mut argv = vec![ptyro_client::remote::wsl_exe()];
        argv.extend(wsl_setup::ptyro_args(&distro, &words).ok()?);
        return Some(Half {
            label: format!("WSL's ptyro ({distro})"),
            version: version.strip_prefix("ptyro ").map(|v| v.trim().to_owned()),
            argv,
            fix: format!(
                "`wsl -d {distro} -- ptyro update ptyro`; a ptyro there from before ptyro updated itself: `{}`, then `wsl -d {distro} -- ptyro daemon upgrade`",
                wsl_setup::install_line(&distro)
            ),
        });
    }
    if platform != Platform::Wsl {
        return None;
    }
    let exe = windows_ptyro()?;
    let version = {
        let exe = exe.clone();
        tokio::task::spawn_blocking(move || own::version_of(&exe)).await.ok().flatten()
    };
    let shown = exe.to_string_lossy().into_owned();
    Some(Half {
        label: "Windows' ptyro.exe".into(),
        version,
        argv: vec![shown, "update".into(), "ptyro".into(), "--yes".into()],
        fix: format!(
            "`ptyro update ptyro` in a Windows terminal; a ptyro.exe from before ptyro updated itself: `irm https://github.com/{}/releases/latest/download/install.ps1 | iex` in PowerShell",
            own::REPO
        ),
    })
}

/// Windows' `ptyro.exe`, seen from WSL: on `PATH` (WSL adds Windows' own), else where
/// `install.ps1` puts it. A test's `PTYRO_TEST_WINDOWS_PTYRO` instead whenever it replaces a
/// binary of its own (`PTYRO_TEST_SELF_EXE`): never the developer's real one.
fn windows_ptyro() -> Option<PathBuf> {
    if ptyro_core::test_knob("PTYRO_TEST_SELF_EXE").is_some() {
        return ptyro_core::test_knob("PTYRO_TEST_WINDOWS_PTYRO")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from);
    }
    let w = ptyro_core::wsl::detect()?;
    if !w.interop {
        return None;
    }
    let on_path = std::env::var_os("PATH")
        .and_then(|p| std::env::split_paths(&p).map(|d| d.join("ptyro.exe")).find(|p| p.is_file()));
    on_path.or_else(|| {
        w.windows_home()
            .map(|h| h.join("AppData/Local/Programs/ptyro/bin/ptyro.exe"))
            .filter(|p| p.is_file())
    })
}

/// The other half at `version` too: asked to update itself when it says another version (it
/// then sees this half at `version`, so it doesn't ask back). Its own output shows live (in
/// `--json`, only its words, on stderr). True when the halves agree afterwards.
pub(crate) async fn keep_in_step(half: &Half, version: &str, json: bool) -> bool {
    let same = |v: &Option<String>| {
        v.as_deref().and_then(Version::parse) == Version::parse(version) && v.is_some()
    };
    if same(&half.version) {
        if !json {
            eprintln!("✓ {} is {version} too", half.label);
        }
        return true;
    }
    eprintln!(
        "\n▸ {} {} → {version}, so both halves agree",
        half.label,
        half.version.as_deref().unwrap_or("(version unknown)")
    );
    let Some((prog, args)) = half.argv.split_first() else { return false };
    let mut cmd = tokio::process::Command::new(prog);
    cmd.args(args).env("WSL_UTF8", "1").stdin(Stdio::null());
    if json {
        cmd.stdout(Stdio::null());
    }
    let ran = cmd.status().await;
    // Asked again: what it says now, whatever its exit.
    let now = match other_version(half).await {
        Some(v) => Some(v),
        None => half.version.clone(),
    };
    if same(&now) {
        eprintln!("✓ {} {version}", half.label);
        return true;
    }
    let why = match ran {
        Ok(s) if s.success() => format!("it still says {}", now.as_deref().unwrap_or("nothing")),
        Ok(s) => format!("it exited with {}", s.code().unwrap_or(1)),
        Err(e) => format!("it couldn't be run: {e}"),
    };
    eprintln!("✗ {} wasn't updated ({why})", half.label);
    eprintln!("  fix: {}", half.fix);
    false
}

/// The other half's version, asked again after its update.
async fn other_version(half: &Half) -> Option<String> {
    let (prog, args) = half.argv.split_first()?;
    // The same command line, `--version` in place of `update ptyro --yes`.
    let mut args: Vec<String> = args.to_vec();
    args.truncate(args.len().saturating_sub(3));
    args.push("--version".into());
    let out = tokio::process::Command::new(prog)
        .args(&args)
        .env("WSL_UTF8", "1")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.trim().starts_with("ptyro "))?;
    line.trim().strip_prefix("ptyro ").map(|v| v.trim().to_owned())
}

/// After ptyro was replaced: this computer's daemon switched to it live (`daemon upgrade`, every
/// session carries on), when one runs. Its words are `daemon upgrade`'s own.
pub(crate) async fn upgrade_daemon(ctx: &Ctx, binary: PathBuf, version: &str) -> bool {
    if ctx.connect(false).await.is_err() {
        eprintln!("No daemon runs here: the next one starts as ptyro {version}.");
        return true;
    }
    eprintln!();
    match crate::cli::upgrade(ctx, Some(binary), false).await {
        Ok(0) => true,
        Ok(_) => false,
        Err(e) => {
            eprintln!("✗ the daemon wasn't upgraded: {e}");
            eprintln!(
                "  fix: `ptyro daemon upgrade` (sessions carry on); the new ptyro is installed either way"
            );
            false
        }
    }
}

/// A session on the daemon running `ptyro update ptyro --yes` (the web's "Update ptyro"): its
/// progress live in a terminal the page shows; the daemon's live upgrade at its end keeps the
/// session. Refused at once while setup runs (one install or update at a time), and for a binary
/// only an administrator can replace (the owner's exception: the command is the page's to show)
/// or one a package manager other than Homebrew owns.
pub(crate) async fn spawn_update(ctx: &Ctx) -> Result<ptyro_core::session::SessionInfo, Error> {
    use ptyro_core::session::SpawnSpec;
    if let Some(by) = ptyro_agents::setup::lock::Lock::held_by(&ctx.paths.state) {
        return Err(Error::conflict(ptyro_agents::setup::lock::busy(&by)));
    }
    let install = tokio::task::spawn_blocking(Install::here)
        .await
        .map_err(|e| Error::internal(e.to_string()))?;
    match &install.how {
        How::Admin => {
            return Err(Error::invalid(format!(
                "{} can only be replaced as administrator, which never goes through the web: run `ptyro update ptyro` in a terminal on the computer",
                install.path.display()
            )));
        }
        How::Managed(m) => {
            return Err(Error::invalid(format!(
                "this ptyro was installed with {m}: update it that way"
            )));
        }
        How::Build => {
            return Err(Error::invalid(format!(
                "{} is a build: it updates by building again",
                install.path.display()
            )));
        }
        How::Release | How::Brew => {}
    }
    // This process's own binary runs it (on Linux even once it was replaced on disk).
    let exe = std::env::current_exe().map_err(|e| Error::internal(format!("ptyro's path: {e}")))?;
    // A replaced file's ` (deleted)`, or an image an update renamed aside (`ptyro.exe.old`):
    // the installed binary's own name.
    let exe = own::installed_name(&exe).to_string_lossy().into_owned();
    let home = ["HOME", "USERPROFILE"]
        .into_iter()
        .find_map(|v| std::env::var_os(v).filter(|h| !h.is_empty()))
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let c = ctx.connect(true).await?;
    let spec = SpawnSpec {
        argv: vec![exe, "update".into(), "ptyro".into(), "--yes".into()],
        cwd: home,
        env: crate::cli::client_env(),
        confirmed: true,
        ..SpawnSpec::default()
    };
    crate::cli::session_of(c.request(ptyro_protocol::Request::Spawn { spec }).await?)
}
