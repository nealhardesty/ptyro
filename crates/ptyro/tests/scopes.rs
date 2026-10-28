//! What each device scope may ask of the daemon (H2.4). `devices::needed` has no catch-all, so a
//! new request cannot compile without a decision; this checks the decisions. A table lists a
//! sample of every `Request` variant with the scope it needs, a completeness test holds the table
//! to the enum in `ptyro-protocol`, and a real daemon is asked through `ptyro serve` by devices
//! paired at each scope: a request is `permission_denied` exactly when the table says the scope
//! is too low.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Child, Stdio};
use std::time::{Duration, Instant};

use ptyro_testkit::TempDaemon;
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_ptyro");
const WAIT: Duration = Duration::from_secs(30);

/// Scope levels, lowest first. `NOBODY` is above `full`: not even a `full` device may ask.
const READ: u8 = 0;
const APPROVE: u8 = 1;
const CONTROL: u8 = 2;
const FULL: u8 = 3;
const NOBODY: u8 = 4;
const SCOPES: [&str; 4] = ["read", "approve", "control", "full"];

/// One sample: the request, the scope it needs, and whether sending it to a real daemon is
/// harmless (it names nothing that exists, or only reads) when the scope is enough.
struct Sample {
    req: Value,
    need: u8,
    harmless: bool,
}

fn s(req: Value, need: u8, harmless: bool) -> Sample {
    Sample { req, need, harmless }
}

fn spawn_spec(extra: Value) -> Value {
    let mut spec = json!({ "cwd": "/nonexistent-scopes-test" });
    for (k, v) in extra.as_object().unwrap() {
        spec[k] = v.clone();
    }
    spec
}

fn table() -> Vec<Sample> {
    let a = "nosuch";
    vec![
        // ── read ──
        s(json!({"t":"list"}), READ, true),
        s(json!({"t":"get","addr":a}), READ, true),
        s(json!({"t":"read_screen","addr":a,"format":"plain"}), READ, true),
        s(json!({"t":"read_scrollback","addr":a,"lines":5}), READ, true),
        s(json!({"t":"status"}), READ, true),
        s(json!({"t":"resolve","addr":a}), READ, true),
        s(json!({"t":"status_explain","addr":a}), READ, true),
        s(json!({"t":"inbox","workspace":null,"unread_only":false,"limit":5}), READ, true),
        s(json!({"t":"events","session":null,"limit":5}), READ, true),
        s(json!({"t":"usage"}), READ, true),
        s(json!({"t":"machine_list"}), READ, true),
        s(json!({"t":"history","limit":5}), READ, true),
        s(json!({"t":"project_list"}), READ, true),
        s(json!({"t":"project_get","addr":a}), READ, true),
        s(json!({"t":"workspace_list"}), READ, true),
        s(json!({"t":"worktree_list","project":null}), READ, true),
        s(json!({"t":"worktree_get","addr":a}), READ, true),
        s(json!({"t":"worktree_diff","addr":a,"against":"base"}), READ, true),
        s(json!({"t":"worktree_log","addr":a}), READ, true),
        s(json!({"t":"run_list","active":false,"limit":5}), READ, true),
        s(json!({"t":"run_get","id":a}), READ, true),
        s(json!({"t":"run_events","id":a}), READ, true),
        s(json!({"t":"gate_list","all":false}), READ, true),
        s(json!({"t":"msg_list","run":null,"limit":5}), READ, true),
        s(json!({"t":"lease_list"}), READ, true),
        s(json!({"t":"radar","project":null}), READ, true),
        s(json!({"t":"checkpoint_list","addr":a}), READ, true),
        s(json!({"t":"checkpoint_diff","addr":a,"n":1,"to":null}), READ, true),
        s(json!({"t":"worktree_archives","project":null}), READ, true),
        s(json!({"t":"branches","project":a}), READ, true),
        s(json!({"t":"profile_list"}), READ, true),
        s(json!({"t":"agents"}), READ, true),
        s(json!({"t":"editors"}), READ, true),
        s(json!({"t":"project_config","addr":a}), READ, true),
        s(json!({"t":"replay","addr":a}), READ, true),
        s(json!({"t":"export","addr":a,"format":"text"}), READ, true),
        s(json!({"t":"web","on":null,"via":null}), READ, true),
        s(json!({"t":"review_list","addr":a}), READ, true),
        s(json!({"t":"review_queue"}), READ, true),
        s(json!({"t":"pr_get","addr":a,"refresh":false}), READ, true),
        s(json!({"t":"recipe_list","project":null}), READ, true),
        s(json!({"t":"recipe_show","name":a,"project":null}), READ, true),
        s(json!({"t":"load_view","name":a}), READ, true),
        s(json!({"t":"push_key"}), READ, true),
        s(
            json!({"t":"push_subscribe","sub":{"endpoint":"https://push.invalid/x","p256dh":"AAAA","auth":"AAAA"}}),
            READ,
            false,
        ),
        s(json!({"t":"push_unsubscribe","endpoint":"https://push.invalid/x"}), READ, true),
        s(json!({"t":"push_test"}), READ, false),
        s(json!({"t":"chat_get","addr":a}), READ, true),
        s(json!({"t":"scratch_get"}), READ, true),
        s(json!({"t":"last_launch","workspace":"main"}), READ, true),
        s(json!({"t":"scratch_todo","all":false}), READ, true),
        s(json!({"t":"scratch_history"}), READ, true),
        // Finding the shell or the pad's pane reads; starting one is a shell.
        s(
            json!({"t":"scratch_pane","create":false,"env":{},"size":null,"workspace":null}),
            READ,
            true,
        ),
        s(
            json!({"t":"quick_shell","worktree":null,"cwd":"/nonexistent-scopes-test","create":false,"env":{},"size":null,"workspace":null}),
            READ,
            true,
        ),
        // ── approve ──
        s(json!({"t":"answer","addr":a,"decision":"yes","expect":null}), APPROVE, true),
        s(json!({"t":"answer","addr":a,"decision":"no","expect":null}), APPROVE, true),
        // "Always" writes a standing permission into the agent's settings: control.
        s(json!({"t":"answer","addr":a,"decision":"always","expect":null}), CONTROL, true),
        s(
            json!({"t":"gate_resolve","id":a,"choice":"yes","note":null,"key":null,"by":null}),
            APPROVE,
            false,
        ),
        s(json!({"t":"clear_attention","addr":a}), APPROVE, true),
        s(
            json!({"t":"inbox_mark","ids":[999999],"all":false,"session":null,"read":true}),
            APPROVE,
            true,
        ),
        s(json!({"t":"mute","target":a,"minutes":1,"unmute":true}), APPROVE, false),
        // ── control ──
        s(
            json!({"t":"send_text","addr":a,"text":"x","enter":false,"wait_ready_ms":null}),
            CONTROL,
            true,
        ),
        s(json!({"t":"send_bytes","addr":a,"bytes":[120]}), CONTROL, true),
        s(json!({"t":"resume","addr":a}), CONTROL, true),
        s(json!({"t":"hibernate","addr":a}), CONTROL, true),
        s(json!({"t":"kill","addr":a,"signal":null}), CONTROL, true),
        s(json!({"t":"remove","addr":a}), CONTROL, true),
        s(json!({"t":"restart","addr":a}), CONTROL, true),
        s(json!({"t":"rename","addr":a,"name":"x"}), CONTROL, true),
        s(
            json!({"t":"msg_post","from":null,"to":a,"kind":null,"body":"x","run":null,"in_reply_to":null,"priority":null,"ttl_ms":null,"key":null}),
            CONTROL,
            false,
        ),
        s(
            json!({"t":"review_add","addr":a,"path":"a","line":1,"code":null,"text":"x"}),
            CONTROL,
            true,
        ),
        s(json!({"t":"review_send","addr":a,"session":null,"note":null}), CONTROL, true),
        s(json!({"t":"iterate","run":a,"candidate":null,"text":"x"}), CONTROL, true),
        s(json!({"t":"run_cancel","id":a}), CONTROL, true),
        s(json!({"t":"run_retry","id":a,"step":"x"}), CONTROL, true),
        s(json!({"t":"compare","run":a,"refresh":false}), CONTROL, true),
        s(json!({"t":"convert","addr":a,"structured":false,"force":false}), CONTROL, true),
        s(json!({"t":"retitle","addr":a,"title":"x","worktree":false}), CONTROL, true),
        s(json!({"t":"summarize","addr":a}), CONTROL, true),
        s(json!({"t":"worktree_open","addr":a,"editor":null}), CONTROL, true),
        s(json!({"t":"scratch_put","base":"","text":"x"}), CONTROL, false),
        s(
            json!({"t":"scratch_add","text":"x","section":null,"note":false,"stamp":false,"session":null,"cwd":null}),
            CONTROL,
            false,
        ),
        s(json!({"t":"scratch_mark","sel":"nosuch","mark":"open"}), CONTROL, true),
        s(json!({"t":"scratch_restore","rev":a}), CONTROL, true),
        s(json!({"t":"adopt","addr":a}), CONTROL, true),
        // A configured agent (its command comes from the user's config) is control…
        s(json!({"t":"spawn","spec":spawn_spec(json!({"agent":"claude"}))}), CONTROL, false),
        // …any other command line, a verify check, a lead or `ptyro mcp` is full.
        s(json!({"t":"spawn","spec":spawn_spec(json!({"argv":["true"]}))}), FULL, false),
        s(json!({"t":"spawn","spec":spawn_spec(json!({}))}), FULL, false),
        s(
            json!({"t":"spawn","spec":spawn_spec(json!({"agent":"claude","argv":["x"]}))}),
            FULL,
            false,
        ),
        s(
            json!({"t":"spawn","spec":spawn_spec(json!({"agent":"claude","verify":"true"}))}),
            FULL,
            false,
        ),
        s(json!({"t":"spawn","spec":spawn_spec(json!({"agent":"claude","lead":{}}))}), FULL, false),
        s(
            json!({"t":"spawn","spec":spawn_spec(json!({"agent":"claude","mcp":true}))}),
            FULL,
            false,
        ),
        // ── full ──
        s(
            json!({"t":"quick_shell","worktree":null,"cwd":"/nonexistent-scopes-test","create":true,"env":{},"size":null,"workspace":null}),
            FULL,
            false,
        ),
        s(
            json!({"t":"scratch_pane","create":true,"env":{},"size":null,"workspace":null}),
            FULL,
            false,
        ),
        s(json!({"t":"web","on":true,"via":null}), FULL, false),
        s(json!({"t":"web","on":false,"via":null}), FULL, false),
        s(json!({"t":"shutdown","force":false}), FULL, false),
        s(json!({"t":"upgrade","binary":null}), FULL, false),
        s(json!({"t":"device_pair","name":"x","scope":"read"}), FULL, false),
        // A code is claimed by `ptyro serve` for a device that isn't one yet: no device may.
        s(json!({"t":"device_claim","code":"ptp_x","via":null}), NOBODY, false),
        s(json!({"t":"device_list"}), FULL, false),
        // Other machines (MM1.B): listing them reads; anything else reaches another machine.
        s(
            json!({"t":"machine_add","name":"nosuch","transport":{"t":"ssh","target":"nosuch.invalid"},"token":null,"force":false}),
            FULL,
            false,
        ),
        s(json!({"t":"machine_remove","name":a}), FULL, true),
        s(json!({"t":"machine_hide","name":a,"workspace":"main","hidden":true}), FULL, true),
        s(json!({"t":"machine_retry","name":a}), FULL, true),
        s(json!({"t":"device_revoke","id":a}), FULL, false),
        s(json!({"t":"save_view","name":"x","view":{}}), FULL, false),
        s(json!({"t":"reload_config"}), FULL, false),
        s(
            json!({"t":"project_add","path":"/nonexistent-scopes-test","workspace":null}),
            FULL,
            false,
        ),
        s(json!({"t":"project_remove","addr":a,"release":false}), FULL, false),
        s(json!({"t":"project_pin","addr":a,"profile":null}), FULL, false),
        s(json!({"t":"project_scripts","addr":a,"worktree":null}), FULL, false),
        s(json!({"t":"project_trust","addr":a,"fingerprint":"x"}), FULL, false),
        s(json!({"t":"project_untrust","addr":a}), FULL, false),
        s(json!({"t":"project_suggest","addr":a,"apply":null}), FULL, false),
        s(json!({"t":"discover","refresh":false}), FULL, false),
        s(json!({"t":"workspace_new","name":"x","profile":null,"notify":null}), FULL, false),
        s(json!({"t":"workspace_set","name":"x","profile":null,"notify":null}), FULL, false),
        s(json!({"t":"workspace_rename","name":"x","new_name":"y"}), FULL, false),
        s(json!({"t":"workspace_remove","name":"x"}), FULL, false),
        s(json!({"t":"workspace_member","name":"x","project":a,"remove":true}), FULL, false),
        s(json!({"t":"worktree_new","spec":{"project":a}}), FULL, false),
        s(json!({"t":"worktree_remove","addr":a,"opts":{}}), FULL, false),
        s(json!({"t":"worktree_refresh","addr":a}), FULL, false),
        s(json!({"t":"worktree_run","addr":a,"script":null,"size":null}), FULL, false),
        s(json!({"t":"worktree_lock","addr":a,"lock":true}), FULL, false),
        s(json!({"t":"worktree_rename_branch","addr":a,"branch":null}), FULL, false),
        s(json!({"t":"gc"}), FULL, false),
        s(json!({"t":"worktree_archive","addr":a,"stop_sessions":false}), FULL, false),
        s(json!({"t":"worktree_restore","addr":a}), FULL, false),
        s(json!({"t":"worktree_finish","addr":a,"integrate":false}), FULL, false),
        s(json!({"t":"profile_env","name":a}), FULL, false),
        s(json!({"t":"profile_rename","old":a,"new":"x"}), FULL, false),
        s(json!({"t":"profile_resolve","spec":spawn_spec(json!({}))}), FULL, false),
        s(
            json!({"t":"hook","session":a,"adapter":"claude","event":"x","payload":{},"env":{}}),
            FULL,
            false,
        ),
        s(json!({"t":"status_line","session":a,"payload":{}}), FULL, false),
        s(
            json!({"t":"report","addr":a,"state":"working","kind":null,"msg":null,"progress":null}),
            FULL,
            false,
        ),
        s(json!({"t":"notify_test"}), FULL, false),
        s(json!({"t":"doctor"}), FULL, false),
        s(json!({"t":"self_test","adapter":a}), FULL, false),
        s(
            json!({"t":"worktree_commit","addr":a,"message":"x","paths":[],"all":false}),
            FULL,
            false,
        ),
        s(
            json!({"t":"worktree_integrate","addr":a,"into":null,"cherry_pick":false,"push":false}),
            FULL,
            false,
        ),
        s(json!({"t":"worktree_pair","addr":a,"off":true,"refresh":false}), FULL, false),
        s(
            json!({"t":"pr_create","addr":a,"draft":true,"title":null,"body":null,"base":null}),
            FULL,
            false,
        ),
        s(json!({"t":"pr_fix","addr":a}), FULL, false),
        s(json!({"t":"pr_automerge","addr":a,"method":null,"off":true}), FULL, false),
        s(json!({"t":"pr_watch","addr":a,"off":true}), FULL, false),
        s(json!({"t":"pr_reply","addr":a,"comment":null,"text":"x"}), FULL, false),
        s(json!({"t":"review_remove","addr":a,"id":null}), FULL, false),
        s(json!({"t":"review_done","addr":a}), FULL, false),
        s(json!({"t":"run_fork","id":a,"from":"x","set":{}}), FULL, false),
        s(
            json!({"t":"recipe_run","name":a,"src":null,"vars":{},"project":null,"base":null,"trust":null}),
            FULL,
            false,
        ),
        s(json!({"t":"fanout","spec":{"agents":["claude"],"prompt":"x"}}), FULL, false),
        s(
            json!({"t":"pick","run":a,"candidate":"x","mode":"merge","into":null,"paths":[],"hunks":[]}),
            FULL,
            false,
        ),
        s(json!({"t":"discard","run":a,"candidate":null,"force":false}), FULL, false),
        s(json!({"t":"msg_check","me":a,"since":null,"wait_ms":null}), FULL, false),
        s(json!({"t":"msg_ack","me":a,"ids":[]}), FULL, false),
        s(
            json!({"t":"lease_claim","holder":a,"patterns":["x"],"exclusive":false,"ttl_ms":null,"reason":null}),
            FULL,
            false,
        ),
        s(json!({"t":"lease_release","holder":a,"patterns":["x"]}), FULL, false),
        s(json!({"t":"checkpoint_restore","addr":a,"n":null,"turn":null}), FULL, false),
        s(json!({"t":"set_verify","addr":a,"check":null,"max_bounces":null}), FULL, false),
        s(json!({"t":"lead_start","addr":a,"policy":null}), FULL, false),
        s(json!({"t":"lead","lead":a,"op":{"op":"list_sessions"}}), FULL, false),
        s(json!({"t":"cs","op":{"op":"list"}}), FULL, false),
    ]
}

/// `CamelCase` → `camel_case`, as serde's `rename_all = "snake_case"` writes variants.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// The variant names of `pub enum Request`, read from the protocol's source.
fn request_variants() -> Vec<String> {
    let src = include_str!("../../ptyro-protocol/src/lib.rs");
    let start = src.find("pub enum Request {").expect("the Request enum");
    let body = &src[start + "pub enum Request {".len()..];
    let mut depth = 1usize;
    let mut names = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if depth == 1
            && !t.is_empty()
            && !t.starts_with("//")
            && !t.starts_with('#')
            && let Some(first) = t.chars().next()
            && first.is_ascii_uppercase()
        {
            let end = t.find(|c: char| !c.is_alphanumeric()).unwrap_or(t.len());
            names.push(t[..end].to_owned());
        }
        // Attributes and comments hold no braces that matter; fields and doc text might.
        if !t.starts_with("//") {
            depth += t.matches('{').count();
            depth = depth.saturating_sub(t.matches('}').count());
        }
        if depth == 0 {
            break;
        }
    }
    names
}

#[test]
fn every_request_has_a_sample_and_every_sample_is_a_request() {
    let variants: Vec<String> = request_variants().iter().map(|v| snake(v)).collect();
    assert!(variants.len() > 100, "parsed only {} variants: {variants:?}", variants.len());
    let samples = table();
    let tags: std::collections::BTreeSet<&str> =
        samples.iter().map(|x| x.req["t"].as_str().expect("a sample has a tag")).collect();
    for v in &variants {
        assert!(tags.contains(v.as_str()), "no sample in tests/scopes.rs for Request::{v}");
    }
    for t in &tags {
        assert!(variants.iter().any(|v| v == t), "sample for {t}, which is not a Request");
    }
}

// ── a real daemon ──

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A raw HTTP/1.1 request; returns (status, body).
fn http(port: u16, req: &str) -> (u16, String) {
    let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(40))).unwrap();
    c.write_all(req.as_bytes()).unwrap();
    let mut out = Vec::new();
    let _ = c.read_to_end(&mut out);
    let text = String::from_utf8_lossy(&out).into_owned();
    let code = text.split_whitespace().nth(1).and_then(|c| c.parse().ok()).unwrap_or(0);
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_owned();
    (code, body)
}

fn post(port: u16, path: &str, token: Option<&str>, body: &Value) -> (u16, String) {
    let body = body.to_string();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    http(
        port,
        &format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )
}

struct Serve {
    child: Child,
    port: u16,
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_device_is_denied_exactly_what_its_scope_is_too_low_for() {
    let home = tempfile::Builder::new().prefix("ptyro-scopes-").tempdir().unwrap();
    let hs = home.path().canonicalize().unwrap();
    let hs_str = hs.to_string_lossy().into_owned();
    let d = TempDaemon::start_with(
        BIN,
        &[("HOME", hs_str.as_str()), ("SHELL", "/bin/sh")],
        Some("[attention]\nmin_work = \"1s\"\n"),
    )
    .unwrap();
    let port = free_port();
    let child = d
        .cmd()
        .args(["serve", "--loopback-http", "--port", &port.to_string()])
        .env("HOME", &hs)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let serve = Serve { child, port };
    let deadline = Instant::now() + WAIT;
    while !ptyro_testkit::listening(port) {
        assert!(Instant::now() < deadline, "ptyro serve never listened on {port}");
        std::thread::sleep(Duration::from_millis(50));
    }

    // One device per scope, paired as the owner does and claimed as the web app does.
    let tokens: Vec<String> = SCOPES
        .iter()
        .map(|scope| {
            let out = d
                .cmd()
                .args(["pair", &format!("dev-{scope}"), "--scope", scope, "--json"])
                .env("HOME", &hs)
                .current_dir(&hs)
                .output()
                .unwrap();
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let v: Value = serde_json::from_slice(&out.stdout).unwrap();
            let code = v["url"].as_str().unwrap().split("#pair=").nth(1).unwrap().to_owned();
            let (status, body) = post(serve.port, "/api/pair", None, &json!({ "code": code }));
            assert_eq!(status, 200, "{body}");
            let v: Value = serde_json::from_str(&body).unwrap();
            v["token"].as_str().unwrap().to_owned()
        })
        .collect();

    let ask = |level: usize, req: &Value| -> String {
        let (status, body) = post(serve.port, "/api/request", Some(&tokens[level]), req);
        assert_eq!(status, 200, "{req}: {body}");
        let v: Value = serde_json::from_str(&body).unwrap();
        match v["t"].as_str() {
            Some("ok") => "ok".to_owned(),
            Some("err") => v["error"]["code"].as_str().unwrap_or("?").to_owned(),
            _ => panic!("{req}: {body}"),
        }
    };

    let mut sent = 0;
    for x in table() {
        for (level, name) in SCOPES.iter().enumerate() {
            let allowed = level as u8 >= x.need;
            let code = if allowed && !x.harmless {
                continue;
            } else {
                ask(level, &x.req)
            };
            sent += 1;
            if allowed {
                assert_ne!(code, "permission_denied", "{name} device was refused {}", x.req);
            } else {
                assert_eq!(
                    code, "permission_denied",
                    "{name} device was not refused {} (needs level {})",
                    x.req, x.need
                );
            }
        }
    }
    assert!(sent > 300, "only {sent} requests were sent");

    // The daemon is still up and answering after all of that.
    assert_eq!(ask(0, &json!({"t":"list"})), "ok");
}
