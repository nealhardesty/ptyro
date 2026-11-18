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

/// The shapes the pages send, written as the pages write them (nulls for what's left out).
#[test]
fn the_shapes_the_web_app_sends_parse() {
    let shapes: Vec<Value> = vec![
        json!({ "t": "spawn", "spec": { "cwd": "", "argv": [], "agent": "claude", "worktree": "w1", "prompt": "do it", "title": "do it", "confirmed": false } }),
        json!({ "t": "spawn", "spec": { "cwd": "", "argv": ["sh", "-c", "make"], "worktree": "w1" } }),
        json!({ "t": "worktree_new", "spec": { "setup": true, "project": "p", "agent": "claude", "prompt": "x", "title": "x", "pr": 12, "existing": true, "branch": "b", "trust": "fp", "confirmed": false } }),
        json!({ "t": "kill", "addr": "a", "signal": null }),
        json!({ "t": "remove", "addr": "a", "stop": true }),
        json!({ "t": "retitle", "addr": "a", "title": null, "worktree": true }),
        json!({ "t": "quick_shell", "worktree": null, "cwd": "", "create": true, "env": {}, "size": null, "workspace": null }),
        json!({ "t": "mute", "target": "a", "minutes": null, "unmute": false }),
        json!({ "t": "project_add", "path": "/r", "workspace": null }),
        json!({ "t": "project_scripts", "addr": "p", "worktree": null }),
        json!({ "t": "project_pin", "addr": "p", "profile": null }),
        json!({ "t": "project_remove", "addr": "p", "release": false }),
        json!({ "t": "worktree_remove", "addr": "w", "opts": { "force": false, "stop_sessions": true, "keep_branch": false } }),
        json!({ "t": "worktree_run", "addr": "w", "script": null, "size": null }),
        json!({ "t": "worktree_open", "addr": "w", "editor": "nvim" }),
        json!({ "t": "workspace_new", "name": "n", "profile": null, "notify": "all" }),
        json!({ "t": "workspace_member", "name": "n", "project": "p", "remove": false }),
        json!({ "t": "checkpoint_diff", "addr": "w", "n": 1, "to": null }),
        json!({ "t": "checkpoint_restore", "addr": "w", "n": 1, "turn": null }),
        json!({ "t": "device_pair", "name": "phone", "scope": "control" }),
        json!({ "t": "upgrade", "binary": null }),
        json!({ "t": "discover", "refresh": false }),
        json!({ "t": "web" }),
        json!({ "t": "web", "on": true, "via": "lan" }),
        json!({ "t": "web", "on": false, "via": null }),
    ];
    for s in shapes {
        if let Err(e) = serde_json::from_value::<Request>(s.clone()) {
            panic!("the web app sends {s}, which the protocol rejects: {e}");
        }
    }
}
