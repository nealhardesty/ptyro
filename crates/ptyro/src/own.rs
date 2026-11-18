//! `ptyro update ptyro` (ROADMAP BOOT6, SPEC §24.8): the CLI's part of ptyro updating itself.
//! The download, check and swap are `ptyro_agents::own`'s; here are the words and the daemon's
//! live upgrade afterwards.

use std::path::PathBuf;

use ptyro_agents::own::{self, How, Install};
use ptyro_agents::updates::Outdated;

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
    // A replaced file's ` (deleted)`: the installed binary's own name.
    let exe = own::installed_name(&exe).to_string_lossy().into_owned();
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
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
