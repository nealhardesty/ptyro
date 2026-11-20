use serde_json::json;

use super::*;

const CLAUDE: &str = "claude-2.1.329/transcripts";
const CODEX: &str = "codex-0.167.1/transcripts";

fn fixture(dir: &str, name: &str) -> Vec<u8> {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(dir).join(name);
    std::fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// Each part as `kind anchor| text`: the anchor's first eight characters, the text's first line.
fn shown(t: &Transcript) -> Vec<String> {
    t.parts
        .iter()
        .map(|p| {
            let a: String = p.anchor.as_deref().unwrap_or("-").chars().take(8).collect();
            format!("{} {a}| {}", p.kind, p.text.lines().next().unwrap_or(""))
        })
        .collect()
}

fn lines_of(values: &[serde_json::Value]) -> Vec<u8> {
    values.iter().flat_map(|v| format!("{v}\n").into_bytes()).collect()
}

const CLAUDE_TURN_1: [&str; 5] = [
    "user 938d40b1| Do these four things in order. 1: write the file notes.txt containing the single line hello. 2: run the shell command `cat notes.txt`. 3: call the recall tool with the query 'retry policy'. 4: reply with only the knowledge base's check word (the ptyro server's instructions gave you one), or NONE if you were given none.",
    "tool df3dc90f| Write: /home/u/proj/notes.txt",
    "tool 6460f64c| Bash: cat notes.txt",
    "tool 1436b74b| mcp__ptyro__recall: retry policy",
    "assistant e38b2828| QUOKKA-58",
];
const CLAUDE_TURNS_2_3: [&str; 5] = [
    "user ebf7296b| Call the recall tool with the query 'again', then reply with only DONE. If the call is refused reply with only REFUSED.",
    "tool 138fb225| mcp__ptyro__recall: again",
    "assistant 5cb68b75| REFUSED",
    "user bb11d4e5| Reply with only the word OK3.",
    "assistant 143e62a9| OK3",
];

#[test]
fn claude_plain() {
    let bytes = fixture(CLAUDE, "plain.jsonl");
    let t = claude(&bytes, 0);
    assert_eq!(
        shown(&t),
        ["user 5bd44de8| Reply with only the word OK.", "assistant 9d7de215| OK"]
    );
    assert_eq!(t.to, bytes.len() as u64);
    assert_eq!(t.conversation.as_deref(), Some("1428ccdc-2336-4067-abd0-ac2ef1665b1f"));
    assert_eq!(t.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(t.last_reply(), Some("OK"));
    assert_eq!((t.title, t.files.len()), (None, 0));
    // 2026-11-19T22:38:47.370Z.
    assert_eq!(t.parts[0].at_ms, 1_795_127_927_370);
}

#[test]
fn claude_tools_and_a_resume() {
    let t = claude(&fixture(CLAUDE, "tools.jsonl"), 0);
    // `Write`, `Bash` and the MCP tool each give a title; `ToolSearch` and every result none.
    assert_eq!(shown(&t), CLAUDE_TURN_1);
    assert_eq!(t.files, ["/home/u/proj/notes.txt"]);

    let resumed = claude(&fixture(CLAUDE, "resumed.jsonl"), 0);
    assert_eq!(shown(&resumed), [CLAUDE_TURN_1, CLAUDE_TURNS_2_3].concat());
    assert_eq!(resumed.conversation, t.conversation, "a resume keeps the id");
    assert_eq!(resumed.parts[..5], t.parts[..], "and what came before it");
}

#[test]
fn claude_compacted() {
    let t = claude(&fixture(CLAUDE, "compacted.jsonl"), 0);
    let mut want: Vec<&str> = [CLAUDE_TURN_1, CLAUDE_TURNS_2_3].concat();
    want.extend(["summary d7d139b9| 1. Primary Request and Intent:", "user 5e5247ed| /compact"]);
    assert_eq!(shown(&t), want);
    let summary = &t.parts[10].text;
    assert!(summary.ends_with("the ptyro recall permission was refused on the second attempt."));
    for said_to_the_model in [
        "This session is being continued",
        "system-reminder",
        "tools-used-before-summary",
        "If you need specific details",
        "Continue the conversation",
    ] {
        assert!(!summary.contains(said_to_the_model), "{said_to_the_model}");
    }
    // Nothing of the lines that are not conversation: the caveat, the command's output.
    let all = shown(&t).join("\n");
    assert!(!all.contains("local-command") && !all.contains("Compacted"), "{all}");
}

#[test]
fn claude_takes_only_what_was_said() {
    let line = |kind: &str, uuid: &str, more: serde_json::Value| {
        let mut v = json!({ "type": kind, "uuid": uuid, "sessionId": "c1",
                            "timestamp": "2026-11-19T10:00:00+02:00" });
        for (k, x) in more.as_object().unwrap() {
            v[k] = x.clone();
        }
        v
    };
    let user = |uuid: &str, content: serde_json::Value| {
        line("user", uuid, json!({ "message": { "role": "user", "content": content } }))
    };
    let said = |uuid: &str, model: &str, blocks: serde_json::Value| {
        line("assistant", uuid, json!({ "message": { "model": model, "content": blocks } }))
    };
    let mut meta = user("u-meta", json!("<local-command-caveat>not typed</local-command-caveat>"));
    meta["isMeta"] = json!(true);
    let mut side = user("u-side", json!("a sub-agent's prompt"));
    side["isSidechain"] = json!(true);
    let mut api =
        said("a-api", "<synthetic>", json!([{ "type": "text", "text": "API Error: 529" }]));
    api["isApiErrorMessage"] = json!(true);
    let bytes = lines_of(&[
        json!({ "type": "attachment", "attachment": { "email": "me@example.com" }, "uuid": "x" }),
        json!({ "type": "queue-operation", "content": "the prompt again" }),
        json!({ "type": "last-prompt", "lastPrompt": "the prompt again" }),
        json!({ "type": "agent-name", "agentName": "fix-login" }),
        user("u1", json!("fix the login")),
        meta,
        side,
        user("u-out", json!("<local-command-stdout>ok</local-command-stdout>")),
        user("u-bash", json!("<bash-input>ls</bash-input>")),
        user("u-task", json!("<task-notification>done</task-notification>")),
        user("u-rem", json!("<system-reminder>be good</system-reminder>")),
        user(
            "u-res",
            json!([{ "type": "tool_result", "tool_use_id": "t1", "content": "SECRET OUTPUT" }]),
        ),
        user(
            "u-img",
            json!([{ "type": "image", "source": {} }, { "type": "text", "text": "what is this?" }]),
        ),
        user(
            "u-cmd",
            json!(
                "<command-name>/model</command-name>\n<command-message>model</command-message>\n<command-args>opus</command-args>"
            ),
        ),
        said(
            "a1",
            "claude-opus-5-5",
            json!([
                { "type": "thinking", "thinking": "private thoughts" },
                { "type": "text", "text": "On it." },
                { "type": "tool_use", "id": "t1", "name": "Edit", "input": { "file_path": "/r/src/a.rs", "old_string": "x" } },
                { "type": "tool_use", "id": "t2", "name": "Grep", "input": { "pattern": "fn login", "path": "/r" } },
                { "type": "tool_use", "id": "t3", "name": "ToolSearch", "input": { "query": "select:x" } },
                { "type": "tool_use", "id": "t4", "name": "Bash", "input": { "command": format!("echo {}\nsecond line", "x".repeat(300)) } },
                { "type": "tool_use", "id": "t5", "name": "TodoWrite", "input": { "todos": [] } },
                { "type": "tool_use", "id": "t6", "name": "NotebookEdit", "input": { "notebook_path": "/r/n.ipynb" } },
                { "type": "tool_use", "id": "t7", "name": "Write", "input": { "file_path": "/r/src/a.rs" } },
            ]),
        ),
        api,
        json!({ "type": "custom-title", "customTitle": "  Fix the login  " }),
        json!({ "type": "agent-name", "agentName": "later, but a lesser title" }),
        json!({ "type": "user", "message": { "content": "a line with no uuid" } }),
        json!("not an object"),
        json!({ "type": 7 }),
    ]);
    let t = claude(&bytes, 0);
    let command = format!("tool a1| Bash: echo {}…", "x".repeat(194));
    assert_eq!(
        shown(&t),
        [
            "user u1| fix the login",
            "user u-img| what is this?",
            "user u-cmd| /model opus",
            "assistant a1| On it.",
            "tool a1| Edit: /r/src/a.rs",
            "tool a1| Grep: fn login",
            command.as_str(),
            "tool a1| TodoWrite",
            "tool a1| NotebookEdit: /r/n.ipynb",
            "tool a1| Write: /r/src/a.rs",
        ]
    );
    assert_eq!(t.title.as_deref(), Some("Fix the login"));
    assert_eq!(t.model.as_deref(), Some("claude-opus-5-5"), "not the placeholder's");
    assert_eq!(t.files, ["/r/src/a.rs", "/r/n.ipynb"], "each file once");
    // 10:00 at +02:00 is 08:00 UTC.
    assert_eq!(t.parts[0].at_ms, 1_795_075_200_000);
    let all = format!("{t:?}");
    for never in ["me@example.com", "the prompt again", "private thoughts", "SECRET OUTPUT"] {
        assert!(!all.contains(never), "{never}");
    }
}

#[test]
fn codex_plain_tools_and_a_resume() {
    let bytes = fixture(CODEX, "plain.jsonl");
    let t = codex(&bytes, 0);
    assert_eq!(
        shown(&t),
        ["user msg_01a1| Reply with only the word OK.", "assistant msg_0092| OK"]
    );
    assert_eq!(t.to, bytes.len() as u64);
    assert_eq!(t.conversation.as_deref(), Some("01a1f5f6-b278-76d1-8a83-cfe1291c7e0d"));
    assert_eq!(t.model.as_deref(), Some("gpt-6-luna"));

    // The titles are what ran (`item_completed`), not the `exec` call and its program.
    let turn_1 = [
        "user msg_01a1| Do these two things in order. 1: create the file notes.txt containing the single line hello, with apply_patch. 2: run the shell command `cat notes.txt`. Then reply with only DONE.",
        "tool exec-799| patch: /home/u/proj/notes.txt",
        "tool exec-d58| shell: cat notes.txt",
        "assistant msg_06a4| DONE",
    ];
    let t = codex(&fixture(CODEX, "tools.jsonl"), 0);
    assert_eq!(shown(&t), turn_1);
    assert_eq!(t.files, ["/home/u/proj/notes.txt"]);
    assert!(!format!("{t:?}").contains("tools.apply_patch"), "the program is not a title");

    let resumed = codex(&fixture(CODEX, "resumed.jsonl"), 0);
    let mut want = turn_1.to_vec();
    want.extend(["user msg_01a1| Reply with only the word OK2.", "assistant msg_027a| OK2"]);
    assert_eq!(shown(&resumed), want);
    assert_eq!(resumed.conversation, t.conversation);
    assert_eq!(resumed.parts[..4], t.parts[..]);
}

#[test]
fn codex_mcp() {
    let t = codex(&fixture(CODEX, "mcp.jsonl"), 0);
    assert_eq!(
        shown(&t),
        [
            "user msg_01a1| Call the recall tool of the ptyro MCP server with the query 'retry policy'. Then reply with only the knowledge base's check word (the ptyro server's instructions gave you one), or NONE if you were given none. If the call is refused reply with only REFUSED.",
            "tool exec-f64| ptyro.recall: retry policy",
            "assistant msg_0e71| QUOKKA-58",
        ]
    );
    // Not the tool's answer, the context messages, or the look-up through `ALL_TOOLS`.
    let all = format!("{t:?}");
    for never in ["KBSPIKE-HIT", "AGENTS.md", "environment_context", "ALL_TOOLS", "reduced"] {
        assert!(!all.contains(never), "{never}");
    }
}

#[test]
fn an_older_codex_names_its_tools_in_the_calls() {
    let item = |kind: &str, payload: serde_json::Value| {
        let mut p = payload;
        p["type"] = json!(kind);
        json!({ "timestamp": "2026-10-30T12:00:00.000Z", "type": "response_item", "payload": p })
    };
    let message = |role: &str, text: &str| {
        item(
            "message",
            json!({ "role": role, "content": [{ "type": "input_text", "text": text }] }),
        )
    };
    let bytes = lines_of(&[
        json!({ "type": "session_meta", "payload": { "id": "t1", "cwd": "/r" } }),
        message("user", "<environment_context>cwd: /r</environment_context>"),
        message("user", "# AGENTS.md instructions"),
        message("developer", "be terse"),
        message("user", "add a test"),
        item("reasoning", json!({ "summary": [{ "text": "private reasoning" }] })),
        item(
            "function_call",
            json!({ "name": "shell", "call_id": "c1",
            "arguments": "{\"command\":[\"bash\",\"-lc\",\"cargo test\"]}" }),
        ),
        item("function_call_output", json!({ "call_id": "c1", "output": "SECRET OUTPUT" })),
        item(
            "custom_tool_call",
            json!({ "name": "apply_patch", "call_id": "c2", "id": "ctc_2",
            "input": "*** Begin Patch\n*** Add File: tests/a.rs\n+x\n*** Update File: src/lib.rs\n*** End Patch" }),
        ),
        item(
            "local_shell_call",
            json!({ "call_id": "c3", "action": { "command": ["ls", "-la"] } }),
        ),
        item("web_search_call", json!({ "id": "ws_1", "action": { "query": "rust proptest" } })),
        message("assistant", "Added."),
    ]);
    let t = codex(&bytes, 0);
    assert_eq!(
        shown(&t)[..6],
        [
            "user L597| add a test",
            "tool c1| shell: cargo test",
            "tool ctc_2| apply_patch: tests/a.rs, src/lib.rs",
            "tool c3| shell: ls -la",
            "tool ws_1| web_search: rust proptest",
            "assistant L1781| Added.",
        ]
    );
    assert_eq!(t.files, ["tests/a.rs", "src/lib.rs"]);
    assert_eq!(t.conversation.as_deref(), Some("t1"));
    let all = format!("{t:?}");
    assert!(!all.contains("private reasoning") && !all.contains("SECRET OUTPUT"), "{all}");
}

/// A file read in two pieces at any byte gives what reading it at once gives, and the first
/// piece stops at the end of its last whole line.
fn any_cut(read: fn(&[u8], u64) -> Transcript, bytes: &[u8], step: usize, tools_too: bool) {
    let whole = read(bytes, 0);
    let keep = |t: &Transcript| -> Vec<Part> {
        t.parts.iter().filter(|p| tools_too || p.kind != ChunkKind::Tool).cloned().collect()
    };
    for cut in (0..=bytes.len()).step_by(step) {
        let first = read(&bytes[..cut], 0);
        let to = first.to as usize;
        assert!(to <= cut && (to == 0 || bytes[to - 1] == b'\n'), "cut {cut}: stopped at {to}");
        let second = read(&bytes[to..], first.to);
        assert_eq!(second.to, bytes.len() as u64, "cut {cut}");
        let mut parts = keep(&first);
        parts.extend(keep(&second));
        assert_eq!(parts, keep(&whole), "cut {cut}");
    }
}

#[test]
fn a_torn_last_line_waits_and_a_file_cut_anywhere_reads_the_same() {
    let plain = fixture(CLAUDE, "plain.jsonl");
    // Without its last newline, the last line (the answer) is not read yet.
    let torn = claude(&plain[..plain.len() - 1], 0);
    assert!(torn.to < plain.len() as u64 - 1);
    any_cut(claude, &plain, 1, true);
    any_cut(claude, &fixture(CLAUDE, "compacted.jsonl"), 97, true);
    // (Codex's tool titles depend on what else a piece holds: its messages never do.)
    any_cut(codex, &fixture(CODEX, "plain.jsonl"), 1, true);
    any_cut(codex, &fixture(CODEX, "resumed.jsonl"), 53, false);
    // And at the turn's end, where a cursor really stands, the titles too.
    let (tools, resumed) = (fixture(CODEX, "tools.jsonl"), fixture(CODEX, "resumed.jsonl"));
    let first = codex(&resumed[..tools.len()], 0);
    assert_eq!(first, codex(&tools, 0));
    let second = codex(&resumed[tools.len()..], first.to);
    assert_eq!(shown(&second), shown(&codex(&resumed, 0))[4..]);
}

#[test]
fn odd_bytes_are_no_panic() {
    for bytes in [
        &b""[..],
        b"\n\n\n",
        b"{",
        b"\xff\xfe\n{\"type\":\"user\"}\n",
        b"{\"type\":\"user\",\"uuid\":\"u\",\"timestamp\":\"2026-\xc3\",\"message\":{\"content\":\"x\"}}\n",
        b"{\"type\":\"assistant\",\"uuid\":\"u\",\"timestamp\":\"9999-99-99T99:99:99.9+99:99\",\"message\":{\"content\":[{\"type\":\"tool_use\",\"input\":\"<\"}]}}\n",
        b"{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"text\":\"<\"}]}}\n",
    ] {
        let (a, b) = (claude(bytes, 7), codex(bytes, 7));
        assert!(a.to >= 7 && a.to <= 7 + bytes.len() as u64);
        assert_eq!(a.to, b.to);
    }
    let long =
        format!("{{\"type\":\"user\",\"uuid\":\"u\",\"x\":\"{}\"}}\nrest", "y".repeat(MAX_LINE));
    let t = claude(long.as_bytes(), 0);
    assert!(t.parts.is_empty());
    assert_eq!(t.to, long.len() as u64 - 4, "a line too long is passed over");
    assert_eq!(iso_ms("2026-10-25T05:05:24.025Z"), Some(1_792_904_724_025));
    assert_eq!(iso_ms("2026-10-25 05:05:24-01:00"), Some(1_792_908_324_000));
    assert_eq!(iso_ms("yesterday"), None);
}

#[test]
fn the_file_is_the_hooks_only_when_it_is_that_conversations() {
    let t = tempfile::tempdir().unwrap();
    let home = t.path().join("claude");
    let id = "6992c0ad-dafa-428e-8e80-8de66de55326";
    let own = home.join("projects/-r-my-app").join(format!("{id}.jsonl"));
    std::fs::create_dir_all(own.parent().unwrap()).unwrap();
    let at = |cwd: &'static str, hook: Option<&'static Path>| TranscriptAt {
        home: &home,
        cwd: Path::new(cwd),
        conversation: id,
        hook,
    };
    let claude = crate::adapter("claude");
    assert_eq!(claude.transcript(&at("/r/my_app", None)), None, "not written yet");
    std::fs::write(&own, "").unwrap();
    assert_eq!(claude.transcript(&at("/r/my_app", None)), Some(own.clone()));
    // Started in another folder: found by its name.
    assert_eq!(claude.transcript(&at("/elsewhere", None)), Some(own.clone()));
    // A hook's path is taken when it is this conversation's file in this home, and only then.
    let hooked: &'static Path = Box::leak(own.clone().into_boxed_path());
    assert_eq!(claude.transcript(&at("/elsewhere", Some(hooked))), Some(own.clone()));
    for bad in ["/etc/passwd", "/tmp/6992c0ad-dafa-428e-8e80-8de66de55326.jsonl", "relative.jsonl"]
    {
        assert_eq!(claude.transcript(&at("/r/my_app", Some(Path::new(bad)))), Some(own.clone()));
    }
    assert!(crate::adapter("generic").transcript(&at("/r", None)).is_none());
    assert!(crate::adapter("opencode").read_transcript(b"{}\n", 0).is_none());

    // Codex: the newest rollout of the thread, by day; its title from the index.
    let home = t.path().join("codex");
    let old = home.join(format!("sessions/2026/10/30/rollout-2026-10-30T10-00-00-{id}.jsonl"));
    let new = home.join(format!("sessions/2026/11/19/rollout-2026-11-19T17-39-14-{id}.jsonl"));
    let other = home.join("sessions/2026/11/20/rollout-2026-11-20T09-00-00-another.jsonl");
    for p in [&old, &new, &other] {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }
    std::fs::write(
        home.join("session_index.jsonl"),
        format!("{{\"id\":\"{id}\",\"thread_name\":\"first\"}}\nnot json {id}\n{{\"id\":\"another\",\"thread_name\":\"no\"}}\n{{\"id\":\"{id}\",\"thread_name\":\"Fix the login\"}}\n"),
    )
    .unwrap();
    let codex = crate::adapter("codex");
    let at = TranscriptAt { home: &home, cwd: Path::new("/r"), conversation: id, hook: None };
    assert_eq!(codex.transcript(&at), Some(new));
    assert_eq!(codex.transcript(&TranscriptAt { hook: Some(&old), ..at }), Some(old));
    assert_eq!(
        codex.transcript(&TranscriptAt { hook: Some(&other), ..at }).as_deref().map(inside_day),
        Some(true)
    );
    assert_eq!(codex.transcript_title(&at).as_deref(), Some("Fix the login"));
    assert_eq!(codex.transcript(&TranscriptAt { conversation: "nobody", ..at }), None);
    assert_eq!(codex.transcript_title(&TranscriptAt { conversation: "nobody", ..at }), None);
}

/// (The thread's own rollout of 2026-11-19, not the hook's file of another thread.)
fn inside_day(p: &Path) -> bool {
    p.to_string_lossy().contains("/2026/11/19/")
}
