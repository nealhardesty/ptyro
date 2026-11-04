//! Where this ptyro's daemon is (ROADMAP WIN3): `--remote`, else `PTYRO_REMOTE`, else
//! `[client] default_remote`. On Windows, with none of them, the first command that needs a
//! daemon looks for a WSL distro that has ptyro and offers it (kept in `client.default_remote`),
//! or offers to install ptyro in the default distro.

use std::io::IsTerminal;

use ptyro_core::config::Config;
use ptyro_core::paths::Paths;

/// Distros that are some tool's own, never where you work.
const NOT_YOURS: &[&str] =
    &["docker-desktop", "docker-desktop-data", "rancher-desktop", "podman-machine"];

/// What the README installs with (the release tarball, checked against `SHA256SUMS`).
pub const INSTALL: &str = "curl -fsSL https://raw.githubusercontent.com/nealhardesty/ptyro/master/scripts/install.sh | sh";

/// The daemon to use. `given` is `--remote`.
pub async fn resolve(given: Option<String>, paths: &Paths, discover: bool) -> Option<String> {
    if given.is_some() {
        return given;
    }
    if let Ok(r) = std::env::var("PTYRO_REMOTE")
        && !r.trim().is_empty()
    {
        return Some(r);
    }
    let config = Config::load(&Config::path(paths)).unwrap_or_default();
    if let Some(r) = config.client.default_remote.filter(|r| !r.trim().is_empty()) {
        return Some(r);
    }
    if cfg!(windows) && discover { first_run(paths).await } else { None }
}

/// The distros worth asking, in WSL's order (the default first), tools' own left out.
pub fn candidates(all: &[String]) -> Vec<String> {
    all.iter()
        .filter(|d| !NOT_YOURS.iter().any(|n| d.to_ascii_lowercase().starts_with(n)))
        .cloned()
        .collect()
}

/// What first run says and offers, from what it found: `(distro, version)` pairs that have ptyro,
/// and the distro to install into when none has.
#[derive(Debug, PartialEq, Eq)]
pub enum Found {
    /// WSL has no distro to work in.
    NoDistro,
    /// This one has ptyro (the first that answered, in WSL's order).
    Ptyro { distro: String, version: String },
    /// None has ptyro: this one (the default) could.
    Install { distro: String },
}

pub fn what_was_found(candidates: &[String], versions: &[Option<String>]) -> Found {
    match candidates.iter().zip(versions).find_map(|(d, v)| v.clone().map(|v| (d.clone(), v))) {
        Some((distro, version)) => Found::Ptyro { distro, version },
        None => match candidates.first() {
            Some(d) => Found::Install { distro: d.clone() },
            None => Found::NoDistro,
        },
    }
}

async fn first_run(paths: &Paths) -> Option<String> {
    let all = ptyro_client::remote::distros().await.unwrap_or_default();
    let names = candidates(&all);
    let mut versions = Vec::new();
    for d in &names {
        let v = ptyro_client::remote::version_in(d).await;
        let has = v.is_some();
        versions.push(v);
        if has {
            break;
        }
    }
    let tty = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    match what_was_found(&names, &versions) {
        Found::NoDistro => {
            eprintln!(
                "ptyro on Windows works with ptyro in WSL, and WSL has no Linux distro yet: \
                 `wsl --install` (then open it, and ptyro's install line there)"
            );
            None
        }
        Found::Ptyro { distro, version } => {
            let target = format!("wsl://{distro}");
            if !tty {
                eprintln!(
                    "note: using {distro} (WSL), which runs {version}; \
                     `ptyro config set client.default_remote {target}` keeps it"
                );
                return Some(target);
            }
            if !ask_yes(&format!("{distro} (WSL) runs {version}. Use it?")) {
                eprintln!("left as it is: `ptyro --remote {target}` uses it once");
                return None;
            }
            keep(paths, &target);
            Some(target)
        }
        Found::Install { distro } => {
            let line = format!("wsl -d {distro} -- sh -c '{INSTALL}'");
            if !tty
                || !crate::cli::confirm(&format!(
                    "No WSL distro has ptyro yet. Install it in {distro}?"
                ))
            {
                eprintln!(
                    "ptyro on Windows works with ptyro in WSL. To install it in {distro}: {line}"
                );
                return None;
            }
            let ok = std::process::Command::new(ptyro_client::remote::wsl_exe())
                .args(["-d", &distro, "--", "sh", "-c", INSTALL])
                .status()
                .is_ok_and(|s| s.success());
            match ptyro_client::remote::version_in(&distro).await {
                Some(v) if ok => {
                    eprintln!("{distro} (WSL) runs {v} now");
                    let target = format!("wsl://{distro}");
                    keep(paths, &target);
                    Some(target)
                }
                _ => {
                    eprintln!("the install didn't finish: run it yourself with {line}");
                    None
                }
            }
        }
    }
}

/// `[Y/n]`: yes unless the answer says no.
fn ask_yes(question: &str) -> bool {
    use std::io::{BufRead, Write};
    eprint!("{question} [Y/n] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    !matches!(line.trim(), "n" | "N" | "no" | "No")
}

/// Keep the answer in this computer's config.
fn keep(paths: &Paths, target: &str) {
    match crate::config::set_user(paths, "client.default_remote", target) {
        Ok(()) => {
            eprintln!("kept as client.default_remote = {target:?} (Settings → remote changes it)")
        }
        Err(e) => eprintln!(
            "note: couldn't keep it ({e}): `ptyro config set client.default_remote {target}`"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn tools_own_distros_are_never_offered() {
        assert_eq!(
            candidates(&s(&["Ubuntu", "docker-desktop", "docker-desktop-data", "Debian"])),
            ["Ubuntu", "Debian"]
        );
        assert!(candidates(&s(&["docker-desktop"])).is_empty());
    }

    #[test]
    fn the_first_distro_with_ptyro_is_offered_else_the_default_to_install_into() {
        let names = s(&["Ubuntu", "Debian"]);
        assert_eq!(
            what_was_found(&names, &[None, Some("ptyro 0.0.48".into())]),
            Found::Ptyro { distro: "Debian".into(), version: "ptyro 0.0.48".into() }
        );
        assert_eq!(
            what_was_found(&names, &[None, None]),
            Found::Install { distro: "Ubuntu".into() }
        );
        assert_eq!(
            what_was_found(&names, &[None]),
            Found::Install { distro: "Ubuntu".into() },
            "asked one so far"
        );
        assert_eq!(what_was_found(&[], &[]), Found::NoDistro);
    }

    #[test]
    fn a_remote_given_or_configured_wins_and_nothing_is_discovered_elsewhere() {
        let d = tempfile::tempdir().unwrap();
        let paths =
            Paths::resolve(|k| (k == "PTYRO_HOME").then(|| d.path().to_path_buf())).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        // PTYRO_REMOTE may be set where tests run: only assert what doesn't depend on it.
        assert_eq!(rt.block_on(resolve(Some("box".into()), &paths, true)).as_deref(), Some("box"));
        if std::env::var_os("PTYRO_REMOTE").is_none() {
            std::fs::create_dir_all(&paths.config).unwrap();
            std::fs::write(
                paths.config.join("config.toml"),
                "[client]\ndefault_remote = \"wsl://Ubuntu\"\n",
            )
            .unwrap();
            assert_eq!(rt.block_on(resolve(None, &paths, true)).as_deref(), Some("wsl://Ubuntu"));
            std::fs::write(paths.config.join("config.toml"), "").unwrap();
            if !cfg!(windows) {
                assert_eq!(
                    rt.block_on(resolve(None, &paths, true)),
                    None,
                    "this computer's daemon"
                );
            }
        }
    }
}
