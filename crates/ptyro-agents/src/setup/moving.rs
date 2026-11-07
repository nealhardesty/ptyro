//! A move to the recommended installer (ROADMAP BOOT7, SPEC §24.9): an agent installed another
//! way (Codex through npm) is installed the recommended way first, the new copy is confirmed to
//! run (it answers `--version`), and only then is the old copy removed with its own installer's
//! uninstall. Nothing is lost: settings and logins live in the home, where neither installer
//! looks; a failed install leaves the old copy as it was; a failed removal says the command that
//! finishes it; and afterwards the new copy must be what runs, or that is said with the fix.

use super::host::{Host, Step};
use super::run::Event;
use super::{Entry, Moving};
use crate::catalog::{Item, Method, Platform, Via};

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
    // A copy the recommended method installed that answers `--version` (the vendor's installer
    // leaves a copy no package manager claims); a third installer's copy never counts.
    let ours = Some(mv.to).filter(|v| *v != Via::Vendor);
    let fresh = |host: &dyn Host| {
        host.copies(item).into_iter().find(|c| c.via == ours && c.version.is_some())
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
                        "the install through {to} finished, but no new {program} installed that way answers `{program} --version` (a new terminal may be needed to find it)"
                    ),
                    kept.clone(),
                )
            })?
        }
    };
    let new_path = new.path.clone().unwrap_or_else(|| program.clone());
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
    host.uninstall(item, old, &mv.uninstall, &mut |text| {
        on(Event::Line { id: id.clone(), text: text.to_owned() });
    })
    .map_err(|err| {
        (
            format!(
                "{name} {} through {to} works ({new_path}), but removing {from}'s copy ({old_path}) failed: {err}",
                new.version.as_deref().unwrap_or_default()
            ),
            format!(
                "finish the move by removing it yourself: `{}`; until then {from}'s copy may still be the one that runs ({})",
                mv.uninstall,
                mv.keeps_words()
            ),
        )
    })?;
    // The old copy must be gone: the removal said it worked, but it may have acted elsewhere.
    if host
        .copies(item)
        .iter()
        .any(|c| c.via == Some(mv.from) && c.path.as_deref() == Some(&old_path))
    {
        return Err((
            format!(
                "{name} through {to} is installed ({new_path}), but {from}'s copy ({old_path}) is still there after `{}`",
                mv.uninstall
            ),
            format!(
                "remove it yourself: delete {old_path}, or find which {from} holds it and run its uninstall there; {}",
                mv.keeps_words()
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
                format!(
                    "install it again: `ptyro install {program}`, or run `{install}` yourself; {}",
                    mv.keeps_words()
                ),
            ));
        }
    }
    // The new copy must be what runs now.
    match host.installed(item) {
        Some(f) if f.via == ours => Ok(f.version.or(new.version)),
        Some(f) => {
            let first = f.path.unwrap_or_else(|| program.clone());
            let how = f.via.map_or("another", |v| v.words());
            Err((
                format!(
                    "{name} through {to} is installed ({new_path}), but {how}'s {program} ({first}) comes first on PATH, so it is the one that runs"
                ),
                format!(
                    "delete {first} (or uninstall it with the tool that put it there), or put {new_path}'s folder ahead of it on PATH"
                ),
            ))
        }
        None => Err((
            format!("after the move no {program} is found to run"),
            format!("install it again: `ptyro install {program}`, or run `{install}` yourself"),
        )),
    }
}
