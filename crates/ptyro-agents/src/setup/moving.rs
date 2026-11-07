//! A move to the recommended installer (ROADMAP BOOT7, SPEC §24.9): an agent installed another
//! way (Codex through npm) is installed the recommended way first, the new copy is confirmed to
//! run (it answers `--version`), and only then is the old copy removed with its own installer's
//! uninstall. Nothing is lost: settings and logins live in the home, where neither installer
//! looks; a failed install leaves the old copy as it was; a failed removal says the command that
//! finishes it; and afterwards the new copy must be what runs, or that is said with the fix.

use super::host::{Host, Step};
use super::run::Event;
use super::{Entry, Moving, installed_by};
use crate::catalog::{Item, Method, Platform};

/// Move `e` (its plan entry, `mv` its offer) onto `method`, the recommended one: the version that
/// runs afterwards, or what went wrong and its fix.
pub(super) fn run(
    host: &dyn Host,
    item: &Item,
    method: &Method,
    e: &Entry,
    mv: &Moving,
    platform: Platform,
    on: &mut dyn FnMut(Event),
) -> Result<Option<String>, (String, String)> {
    let (name, program) = (&e.name, &item.program);
    let (from, to) = (mv.from.words(), mv.to.words());
    let old_path = e.state.path().unwrap_or(program).to_owned();
    let install = method.install.clone().unwrap_or_default();
    let kept = format!(
        "nothing was removed: {name} through {from} is still installed and works. `ptyro install {program}` tries the move again; or run it yourself to see why: `{install}`; the vendor's page: {}",
        item.doc
    );
    let id = e.id.clone();
    // A copy the recommended method installed, known by its path, that answers `--version`; a
    // copy from any other installer (a third package manager, Nix, Scoop, a wrapper) never
    // counts.
    let fresh = |host: &dyn Host| {
        host.copies(item).into_iter().find(|c| installed_by(c, method) && c.version.is_some())
    };
    let step = Step { item, method, update: false };
    let install_now = |on: &mut dyn FnMut(Event), what: String| {
        on(Event::Started {
            id: id.clone(),
            name: what,
            command: install.clone(),
            admin: method.admin,
        });
        host.run(&step, &mut |text| {
            on(Event::Line { id: id.clone(), text: text.to_owned() });
        })
    };
    let new = match fresh(host) {
        // Here already (a move whose removal failed before, tried again): only the old is left.
        Some(c) => {
            on(Event::Line {
                id: id.clone(),
                text: format!(
                    "{name} through {to} is here already ({}): only {from}'s copy is left to remove",
                    c.path.as_deref().unwrap_or(program)
                ),
            });
            c
        }
        None => {
            install_now(on, format!("{name}: installing it through {to}"))
                .map_err(|err| (format!("the install through {to} failed: {err}"), kept.clone()))?;
            fresh(host).ok_or_else(|| {
                (
                    format!(
                        "the install through {to} finished, but no {program} where it installs answers `{program} --version` (a new terminal may be needed to find it)"
                    ),
                    kept.clone(),
                )
            })?
        }
    };
    let new_path = new.path.clone().unwrap_or_else(|| program.clone());
    let reinstall =
        format!("install it again: `ptyro install {program}`, or run `{install}` yourself");
    // What the new copy is now, for a message after the removal: looked at again, since a removal
    // can take a link the new install put in a shared folder.
    let now = |host: &dyn Host| match fresh(host) {
        Some(c) => format!(
            "{name} {} through {to} works ({})",
            c.version.as_deref().unwrap_or_default(),
            c.path.as_deref().unwrap_or(program)
        ),
        None => format!("{name} through {to} no longer runs either ({reinstall})"),
    };
    let old = item
        .others
        .iter()
        .find(|m| m.via == mv.from && m.on(platform))
        .ok_or_else(|| (format!("the table has no {from} row for {name}"), kept.clone()))?;
    on(Event::Started {
        id: id.clone(),
        name: format!("{name}: removing {from}'s copy"),
        command: mv.uninstall.clone(),
        admin: mv.admin,
    });
    if let Err(err) = host.uninstall(item, old, &mv.uninstall, &mut |text| {
        on(Event::Line { id: id.clone(), text: text.to_owned() });
    }) {
        return Err((
            format!("removing {from}'s copy ({old_path}) failed: {err}; {}", now(host)),
            format!(
                "finish the move by removing it yourself: `{}`; until then {from}'s copy may still be the one that runs ({})",
                mv.uninstall,
                mv.keeps_words()
            ),
        ));
    }
    // The old copy must be gone: the removal said it worked, but it may have acted elsewhere.
    if let Some(left) = host
        .copies(item)
        .into_iter()
        .find(|c| c.via == Some(mv.from) && c.path.as_deref() == Some(&old_path))
    {
        let file = left.file.clone().unwrap_or_else(|| old_path.clone());
        let pkg = super::npm_package(item, mv.from, platform);
        return Err((
            format!(
                "{from}'s copy ({old_path}) is still there after `{}`; {}",
                mv.uninstall,
                now(host)
            ),
            format!(
                "remove it yourself: {}",
                delete_command(&file, left.prefix.as_deref(), pkg.as_deref(), program, platform)
            ),
        ));
    }
    // The new copy must still be there and run: a removal can take a link the new install put
    // in the same folder (an npm prefix of `~/.local` shares `~/.local/bin`). Put it back now.
    if fresh(host).is_none() {
        let again = install_now(
            on,
            format!("{name}: the removal took the new copy too, installing it again"),
        );
        if again.is_err() || fresh(host).is_none() {
            let why = again.err().map(|e| format!(": {e}")).unwrap_or_default();
            return Err((
                format!(
                    "{from}'s copy is removed, but the removal took the new {program} with it and installing it again didn't bring it back{why}: no {name} runs now"
                ),
                format!("{reinstall}; {}", mv.keeps_words()),
            ));
        }
    }
    // The new copy must be what runs now, and answer.
    match host.installed(item) {
        Some(f) if installed_by(&f, method) && f.version.is_some() => Ok(f.version),
        Some(f) => {
            let first = f.path.clone().unwrap_or_else(|| program.clone());
            let how = f.via.map_or("another", |v| v.words());
            let file = f.file.clone().unwrap_or_else(|| first.clone());
            // The vendor's own copy, not answering: put it right, never delete it.
            if installed_by(&f, method) {
                return Err((
                    format!(
                        "{name} through {to} is installed ({first}), but it doesn't answer `{program} --version`"
                    ),
                    format!("repair it by installing it again: {reinstall}"),
                ));
            }
            let pkg = f.via.and_then(|v| super::npm_package(item, v, platform));
            Err((
                format!(
                    "{name} through {to} is installed ({new_path}), but {how}'s {program} ({first}) comes first on PATH, so it is the one that runs"
                ),
                format!(
                    "remove it ({}), or uninstall it with the tool that put it there, or put {new_path}'s folder ahead of it on PATH",
                    delete_command(&file, f.prefix.as_deref(), pkg.as_deref(), program, platform)
                ),
            ))
        }
        None => Err((format!("after the move no {program} is found to run"), reinstall)),
    }
}

/// The command that deletes a copy, ready to paste in the platform's shell. An npm copy on
/// Windows is its prefix's shims (`codex`, `codex.cmd`, `codex.ps1`, which npm writes and `PATH`
/// finds) and its package folder: never the script inside it alone, which would leave the shim
/// on `PATH`, broken. Elsewhere `file` is what `PATH` finds (npm's link in `bin`).
pub(super) fn delete_command(
    file: &str,
    prefix: Option<&str>,
    pkg: Option<&str>,
    program: &str,
    platform: Platform,
) -> String {
    let ps = |p: &str| format!("'{}'", p.replace('\'', "''"));
    if platform.is_windows() {
        if let (Some(prefix), Some(pkg)) = (prefix, pkg) {
            let prefix = prefix.trim_end_matches(['\\', '/']);
            let shims: Vec<String> = ["", ".cmd", ".ps1"]
                .iter()
                .map(|ext| ps(&format!("{prefix}\\{program}{ext}")))
                .collect();
            let folder = format!("{prefix}\\node_modules\\{}", pkg.replace('/', "\\"));
            return format!(
                "`Remove-Item -LiteralPath {}; Remove-Item -Recurse -LiteralPath {}`",
                shims.join(", "),
                ps(&folder)
            );
        }
        format!("`Remove-Item -LiteralPath {}`", ps(file))
    } else {
        format!("`rm '{}'`", file.replace('\'', "'\\''"))
    }
}
