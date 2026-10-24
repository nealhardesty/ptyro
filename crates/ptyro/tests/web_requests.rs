//! The web app's requests (TypeScript, `web/src`) against the protocol (`ptyro-protocol`): a
//! typo in a request's name, or a field the daemon wants that the page doesn't send, only shows
//! when someone taps the button. So the names the pages use must be requests that exist, and the
//! shapes they send must parse.

use std::path::PathBuf;

use ptyro_protocol::Request;
use serde_json::{Value, json};

fn sources() -> Vec<(String, String)> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/src");
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "ts") {
            out.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read_to_string(&p).unwrap(),
            ));
        }
    }
    out
}

/// Every `t: "name"` that starts a request in the sources, with where it is.
fn request_names() -> Vec<(String, String)> {
    let mut names = Vec::new();
    for (file, src) in sources() {
        for opener in ["request({ t: \"", "ask({ t: \"", "withConfirm({ t: \"", "req: { t: \""] {
            for part in src.split(opener).skip(1) {
                let name: String =
                    part.chars().take_while(|c| c.is_ascii_lowercase() || *c == '_').collect();
                names.push((name, file.clone()));
            }
        }
    }
    names
}

#[test]
fn every_request_the_web_app_names_exists() {
    let names = request_names();
    assert!(names.len() > 100, "the scan found only {} requests", names.len());
    for (name, file) in names {
        // A request with a missing field is a real one; an unknown variant is a typo.
        let err =
            serde_json::from_value::<Request>(json!({ "t": name })).err().map(|e| e.to_string());
        assert!(
            err.as_deref().is_none_or(|e| !e.contains("unknown variant")),
            "web/src/{file} sends a request called {name:?}, which the protocol doesn't have"
        );
    }
}

/// The change-set operations (`{t: "cs", op: {op: …}}`) the pages send.
#[test]
fn every_change_set_operation_the_web_app_names_exists() {
    let mut seen = 0;
    for (file, src) in sources() {
        for part in src.split("op: { op: \"").skip(1).chain(src.split("csOp({ op: \"").skip(1)) {
            let name: String =
                part.chars().take_while(|c| c.is_ascii_lowercase() || *c == '_').collect();
            seen += 1;
            let err = serde_json::from_value::<Request>(json!({ "t": "cs", "op": { "op": name } }))
                .err()
                .map(|e| e.to_string());
            assert!(
                err.as_deref().is_none_or(|e| !e.contains("unknown variant")),
                "web/src/{file}: no change-set operation {name:?}"
            );
        }
    }
    assert!(seen > 20, "the scan found only {seen} operations");
}

/// The shapes the pages send, written as the pages write them (nulls for what's left out).
#[test]
fn the_shapes_the_web_app_sends_parse() {
    let shapes: Vec<Value> = vec![
        json!({ "t": "spawn", "spec": { "cwd": "", "argv": [], "agent": "claude", "worktree": "w1", "prompt": "do it", "title": "do it", "confirmed": false } }),
        json!({ "t": "spawn", "spec": { "cwd": "", "argv": ["sh", "-c", "make"], "worktree": "w1", "unqueued": true } }),
        json!({ "t": "worktree_new", "spec": { "setup": true, "project": "p", "agent": "claude", "prompt": "x", "title": "x", "pr": 12, "existing": true, "branch": "b", "take_changes": true, "trust": "fp", "confirmed": false } }),
        json!({ "t": "kill", "addr": "a", "signal": null }),
        json!({ "t": "retitle", "addr": "a", "title": null, "worktree": true }),
        json!({ "t": "msg_post", "from": null, "to": "a", "kind": "info", "body": "b", "run": null, "in_reply_to": null, "priority": null, "ttl_ms": null, "key": null }),
        json!({ "t": "set_verify", "addr": "a", "check": null, "max_bounces": 3 }),
        json!({ "t": "quick_shell", "worktree": null, "cwd": "", "create": true, "env": {}, "size": null, "workspace": null }),
        json!({ "t": "convert", "addr": "a", "structured": true, "force": false }),
        json!({ "t": "mute", "target": "a", "minutes": null, "unmute": false }),
        json!({ "t": "project_add", "path": "/r", "workspace": null }),
        json!({ "t": "project_scripts", "addr": "p", "worktree": null }),
        json!({ "t": "project_pin", "addr": "p", "profile": null }),
        json!({ "t": "project_suggest", "addr": "p", "apply": null }),
        json!({ "t": "project_remove", "addr": "p", "release": false }),
        json!({ "t": "worktree_remove", "addr": "w", "opts": { "force": false, "stop_sessions": true, "keep_branch": false } }),
        json!({ "t": "worktree_archive", "addr": "w", "stop_sessions": false }),
        json!({ "t": "worktree_archives", "project": null }),
        json!({ "t": "worktree_run", "addr": "w", "script": null, "size": null }),
        json!({ "t": "worktree_pair", "addr": "w", "off": false, "refresh": false }),
        json!({ "t": "worktree_open", "addr": "w", "editor": "nvim" }),
        json!({ "t": "worktree_finish", "addr": "w", "integrate": true }),
        json!({ "t": "worktree_rename_branch", "addr": "w", "branch": null }),
        json!({ "t": "worktree_diff", "addr": "w", "against": "last_turn" }),
        json!({ "t": "worktree_commit", "addr": "w", "message": "m", "paths": [], "all": true }),
        json!({ "t": "worktree_integrate", "addr": "w", "into": null, "cherry_pick": false, "push": false }),
        json!({ "t": "review_add", "addr": "w", "path": "a.rs", "line": 3, "code": "x", "text": "t" }),
        json!({ "t": "review_send", "addr": "w", "session": null, "note": null }),
        json!({ "t": "review_remove", "addr": "w", "id": null }),
        json!({ "t": "pr_get", "addr": "w", "refresh": true }),
        json!({ "t": "pr_create", "addr": "w", "draft": false, "title": null, "body": null, "base": null }),
        json!({ "t": "pr_automerge", "addr": "w", "method": null, "off": true }),
        json!({ "t": "pr_watch", "addr": "w", "off": false }),
        json!({ "t": "pr_reply", "addr": "w", "comment": null, "text": "t" }),
        json!({ "t": "run_list", "active": false, "limit": 60 }),
        json!({ "t": "run_retry", "id": "r", "step": "s" }),
        json!({ "t": "run_fork", "id": "r", "from": "s", "set": { "a": "b" } }),
        json!({ "t": "recipe_run", "name": "n", "src": null, "vars": {}, "project": null, "base": null, "trust": null }),
        json!({ "t": "fanout", "spec": { "agents": ["claude"], "prompt": "p", "project": "p", "test": null, "verify": null, "judge": null, "structured": false, "base": null, "name": null, "timeout": null } }),
        json!({ "t": "gate_resolve", "id": "g", "choice": "approve", "note": null, "key": "k", "by": "web" }),
        json!({ "t": "compare", "run": "r", "refresh": false }),
        json!({ "t": "pick", "run": "r", "candidate": "c", "mode": "cherry_pick", "into": null, "paths": [], "hunks": [] }),
        json!({ "t": "pick", "run": "r", "candidate": "c", "mode": "apply", "into": "w", "paths": ["a"], "hunks": [] }),
        json!({ "t": "discard", "run": "r", "candidate": null, "force": false }),
        json!({ "t": "iterate", "run": "r", "candidate": null, "text": "t" }),
        json!({ "t": "lead_start", "addr": "a", "policy": { "max_sessions": 4, "max_spawns": null, "agents": [], "worktrees": ["new", "own"], "destructive": false } }),
        json!({ "t": "workspace_new", "name": "n", "profile": null, "notify": "all" }),
        json!({ "t": "workspace_member", "name": "n", "project": "p", "remove": false }),
        json!({ "t": "checkpoint_diff", "addr": "w", "n": 1, "to": null }),
        json!({ "t": "checkpoint_restore", "addr": "w", "n": 1, "turn": null }),
        json!({ "t": "radar", "project": null }),
        json!({ "t": "device_pair", "name": "phone", "scope": "control" }),
        json!({ "t": "upgrade", "binary": null }),
        json!({ "t": "discover", "refresh": false }),
        json!({ "t": "replay", "addr": "a" }),
        json!({ "t": "export", "addr": "a", "format": "text" }),
        json!({ "t": "web" }),
        json!({ "t": "web", "on": true, "via": "lan" }),
        json!({ "t": "web", "on": false, "via": null }),
        json!({ "t": "cs", "op": { "op": "new", "spec": { "name": "n", "projects": ["p"], "template": null, "lead": null, "layout": "manual", "goal": null, "from_issue": null, "depends": {}, "agents": {}, "bases": {}, "gates": null, "integration": [], "workspace": null, "overlay": false, "env": {}, "trust": null } } }),
        json!({ "t": "cs", "op": { "op": "log", "set": "s", "since": null } }),
        json!({ "t": "cs", "op": { "op": "msg", "set": "s", "to": "@all", "text": "t", "kind": null, "interrupt": false } }),
        json!({ "t": "cs", "op": { "op": "contract", "set": "s", "action": { "do": "show", "rev": null } } }),
        json!({ "t": "cs", "op": { "op": "contract", "set": "s", "action": { "do": "history" } } }),
        json!({ "t": "cs", "op": { "op": "contract", "set": "s", "action": { "do": "diff", "a": null, "b": null, "file": null } } }),
        json!({ "t": "cs", "op": { "op": "contract", "set": "s", "action": { "do": "revise", "text": "t", "diff": null, "affects": [], "summary": null } } }),
        json!({ "t": "cs", "op": { "op": "checkpoint", "set": "s", "label": null } }),
        json!({ "t": "cs", "op": { "op": "restore", "set": "s", "n": 1, "force": false } }),
        json!({ "t": "cs", "op": { "op": "overlay", "set": "s", "member": null, "mode": "auto" } }),
        json!({ "t": "cs", "op": { "op": "add", "set": "s", "project": "p", "agent": null, "depends_on": [] } }),
        json!({ "t": "cs", "op": { "op": "rm", "set": "s", "keep": [], "force": false } }),
        json!({ "t": "cs", "op": { "op": "push", "set": "s", "yes": false } }),
        json!({ "t": "cs", "op": { "op": "land", "set": "s", "yes": false } }),
    ];
    for s in shapes {
        if let Err(e) = serde_json::from_value::<Request>(s.clone()) {
            panic!("the web app sends {s}, which the protocol rejects: {e}");
        }
    }
}
