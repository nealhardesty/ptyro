//! The machines linked to this one (MM1.B) kept at its ptyro version (SPEC §24.8): `ptyro update`
//! offers to bring those behind along, and `ptyro machine update` asks them by name. Each machine
//! updates itself (`SelfUpdate`: its own verified download, its own live upgrade), asked over a
//! connection of its own, so a link down for a version mismatch still carries the request.

use ptyro_core::Error;
use ptyro_core::model::{MachineInfo, MachineState};
use ptyro_protocol::{Reply, Request};

use crate::cli::Ctx;

/// `a` is a later version than `b` (`0.0.69` > `0.0.68`; `1.0.0` > `0.9.9`).
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| -> Vec<u64> {
        v.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
    };
    parts(a) > parts(b)
}

/// A linked machine on a ptyro older than `target`: (its name, its version).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Behind {
    pub name: String,
    pub version: String,
}

/// Of `machines`, those reachable (online, or down only for their version) on an older ptyro.
pub fn behind_of(machines: &[MachineInfo], target: &str) -> Vec<Behind> {
    machines
        .iter()
        .filter(|m| matches!(m.state, MachineState::Online | MachineState::Mismatch))
        .filter_map(|m| {
            let v = m.version.as_deref()?;
            newer(target, v).then(|| Behind { name: m.name.clone(), version: v.to_owned() })
        })
        .collect()
}

/// The machines linked to this one's daemon that are behind `target` (none without a daemon).
pub async fn behind(ctx: &Ctx, target: &str) -> Vec<Behind> {
    let Ok(c) = ctx.connect(false).await else { return Vec::new() };
    match c.request(Request::MachineList).await {
        Ok(Reply::Machines { machines }) => behind_of(&machines, target),
        _ => Vec::new(),
    }
}

/// The question's line for them.
pub fn question_line(them: &[Behind], target: &str) -> String {
    let list: Vec<String> =
        them.iter().map(|b| format!("{} {} → {target}", b.name, b.version)).collect();
    format!(
        "  and the machines linked here, each updating itself (checked the same way): {}",
        list.join(", ")
    )
}

/// Ask each to update itself; what each said. Whether every one started.
pub async fn update_them(ctx: &Ctx, names: &[String], json: bool) -> bool {
    let c = match ctx.connect(false).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("✗ the machines linked here weren't asked: {}", e.message);
            return false;
        }
    };
    let mut all = true;
    for name in names {
        match c.request(Request::MachineUpdate { name: name.clone() }).await {
            Ok(Reply::Session { info }) => {
                if json {
                    println!("{}", serde_json::json!({ "machine": name, "session": info.name }));
                } else {
                    eprintln!(
                        "⇡ {name} is updating itself, in a session there ({}); its link comes back by itself once it's done, and the session shows here then",
                        info.name
                    );
                }
            }
            Ok(r) => {
                all = false;
                eprintln!("✗ {name}: {}", crate::cli::unexpected(r).message);
            }
            Err(e) => {
                all = false;
                if json {
                    println!("{}", serde_json::json!({ "machine": name, "error": e.message }));
                } else {
                    eprintln!("✗ {}", e.message);
                }
            }
        }
    }
    all
}

/// `ptyro machine update [NAME…] [--all] [-y] [--json]`: those named, or (`--all`, or none
/// named) every linked machine behind this one's version.
pub async fn machine_update(
    ctx: &Ctx,
    names: Vec<String>,
    all: bool,
    yes: bool,
    json: bool,
) -> Result<i32, Error> {
    let c = ctx.connect(false).await.map_err(crate::cli::no_daemon)?;
    let machines = match c.request(Request::MachineList).await? {
        Reply::Machines { machines } => machines,
        r => return Err(crate::cli::unexpected(r)),
    };
    let ours = match c.request(Request::Status).await {
        Ok(Reply::Status { status }) => status.version,
        _ => env!("CARGO_PKG_VERSION").to_owned(),
    };
    let picked: Vec<String> = if names.is_empty() || all {
        let them = behind_of(&machines, &ours);
        if them.is_empty() {
            let known: Vec<String> = machines.iter().map(|m| m.name.clone()).collect();
            if known.is_empty() {
                eprintln!("No machines are linked here (`ptyro machine add`).");
            } else {
                eprintln!(
                    "Every machine linked here that answers runs ptyro {ours} or newer; name one to update it anyway: `ptyro machine update NAME`"
                );
            }
            return Ok(0);
        }
        them.into_iter().map(|b| b.name).collect()
    } else {
        let mut out = Vec::new();
        for n in names {
            let Some(m) = machines.iter().find(|m| ptyro_core::lookup::same(&m.name, &n)) else {
                let known: Vec<String> = machines.iter().map(|m| m.name.clone()).collect();
                return Err(ptyro_core::lookup::not_found_among("machine", &n, &known));
            };
            out.push(m.name.clone());
        }
        out
    };
    if !yes {
        if !crate::setup::interactive(false) {
            return Err(Error::invalid(
                "`ptyro machine update` asks first: run it in a terminal, or pass --yes",
            ));
        }
        for n in &picked {
            let v = machines
                .iter()
                .find(|m| &m.name == n)
                .and_then(|m| m.version.clone())
                .unwrap_or_else(|| "version unknown".into());
            eprintln!(
                "  {n} ({v}): downloads the newest ptyro release there, checks it against SHA256SUMS, replaces its binary, upgrades its daemon live (its sessions carry on)"
            );
        }
        if !crate::setup::confirm("Update? [Y/n] ") {
            eprintln!("Nothing updated.");
            return Ok(0);
        }
    }
    Ok(if update_them(ctx, &picked, json).await { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::model::MachineTransport;

    #[test]
    fn machines_behind_are_those_reachable_on_an_older_version() {
        let m = |name: &str, v: Option<&str>, state| MachineInfo {
            name: name.into(),
            transport: MachineTransport::Ssh { target: name.into() },
            state,
            version: v.map(str::to_owned),
            since_ms: None,
            calls_itself: None,
            workspaces: Vec::new(),
            hidden: Vec::new(),
            error: None,
            fix: None,
            added_ms: 0,
        };
        let list = [
            m("turkey", Some("0.0.68"), MachineState::Online),
            m("bear", Some("0.0.68"), MachineState::Mismatch),
            m("walrus", Some("0.0.69"), MachineState::Online),
            m("asleep", Some("0.0.60"), MachineState::Offline),
            m("newer", Some("0.1.0"), MachineState::Mismatch),
            m("unknown", None, MachineState::Online),
        ];
        let names: Vec<String> = behind_of(&list, "0.0.69").into_iter().map(|b| b.name).collect();
        assert_eq!(names, ["turkey", "bear"]);
        assert!(newer("0.0.10", "0.0.9") && !newer("0.0.9", "0.0.9") && newer("1.0.0", "0.99.0"));
    }
}
