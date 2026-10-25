//! Comparing candidates (M7.3, M7.10, R-ORC-04): evidence, ranking, and what you do with the
//! winner. Evidence beats opinion: tests (and whether the base passed them), a repro that must
//! fail on the base and pass after, each candidate's tests against the others, identical changes
//! clustered, diff size and time. An LLM judge may add a score, but only as advice. The ranking
//! is lexicographic and always shows its reasons.

use ptyro_core::sync::MutexExt;
use std::collections::BTreeMap;
use std::sync::Arc;

use ptyro_core::model::{DiffAgainst, LineKind, RemoveOpts, WorktreeInfo};
use ptyro_core::{Error, now_ms};
use ptyro_protocol::{Reply, Request};
use ptyro_run::recipe::{Kind, VerifyDef};
use ptyro_run::wire::{Candidate, CompareDoc, PickMode};
use ptyro_run::{RunEvent, Step};
use serde_json::{Value, json};

use crate::daemon::Daemon;
use crate::runs::StepResult;
use crate::worktrees::blocking;

/// Evidence about one candidate, kept on the verify step.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
struct Found {
    tests: Option<i32>,
    repro: Option<bool>,
    cross: Option<(u32, u32)>,
    judge: Option<u8>,
    judge_note: Option<String>,
}

/// A change's fingerprint with whitespace and comment-only lines left out: equal means the
/// candidates did the same thing.
fn signature(doc: &ptyro_core::model::DiffDoc) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for f in &doc.files {
        f.path.hash(&mut h);
        let mut lines: Vec<String> = f
            .hunks
            .iter()
            .flat_map(|hk| hk.lines.iter())
            .filter(|l| l.kind != LineKind::Context)
            .map(|l| (l.kind == LineKind::Added, l.text.split_whitespace().collect::<String>()))
            .filter(|(_, t)| {
                !t.is_empty()
                    && !["//", "#", "/*", "*", "--", "<!--"].iter().any(|c| t.starts_with(c))
            })
            .map(|(added, t)| format!("{}{t}", if added { '+' } else { '-' }))
            .collect();
        lines.sort();
        lines.hash(&mut h);
    }
    h.finish()
}

/// The lock that serializes gathering a run's evidence.
fn gather_lock(run: &str) -> Arc<tokio::sync::Mutex<()>> {
    static LOCKS: std::sync::Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>> =
        std::sync::Mutex::new(BTreeMap::new());
    let mut m = LOCKS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    Arc::clone(m.entry(run.to_owned()).or_default())
}

/// The new name in a `diff --git a/X b/Y` line, unquoted as git quotes it (`"b/\303\244"`).
fn header_name(file: &str) -> String {
    let head = file.lines().next().unwrap_or("");
    let rest = head.strip_prefix("diff --git ").unwrap_or(head);
    let unquote = |q: &str| -> String {
        let inner = q.strip_prefix('"').and_then(|s| s.strip_suffix('"')).unwrap_or(q);
        let (mut bytes, mut it) = (Vec::new(), inner.bytes().peekable());
        while let Some(b) = it.next() {
            if b != b'\\' {
                bytes.push(b);
                continue;
            }
            match it.next() {
                Some(b'n') => bytes.push(b'\n'),
                Some(b't') => bytes.push(b'\t'),
                Some(d @ b'0'..=b'7') => {
                    let mut v = u32::from(d - b'0');
                    for _ in 0..2 {
                        match it.peek() {
                            Some(n @ b'0'..=b'7') => {
                                v = v * 8 + u32::from(n - b'0');
                                it.next();
                            }
                            _ => break,
                        }
                    }
                    bytes.push(v as u8);
                }
                Some(other) => bytes.push(other),
                None => {}
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let name = if let Some(i) = rest.rfind(" \"b/") {
        unquote(&rest[i + 1..]).strip_prefix("b/").map(str::to_owned)
    } else {
        rest.rfind(" b/").map(|i| rest[i + 3..].to_owned())
    };
    name.unwrap_or_default()
}

/// Keep only the chosen files and hunks (`FILE:N`, 1-based) of a unified diff.
pub fn filter_patch(patch: &str, paths: &[String], hunks: &[String]) -> String {
    let mut out = String::new();
    for file in patch
        .split("\ndiff --git ")
        .enumerate()
        .map(|(i, f)| if i == 0 { f.to_owned() } else { format!("diff --git {f}") })
    {
        if file.trim().is_empty() {
            continue;
        }
        // A binary file or a pure rename has no `+++` line, and git quotes odd names: the
        // `diff --git` line is the one every file has.
        let name = file
            .lines()
            .find_map(|l| l.strip_prefix("+++ b/").or_else(|| l.strip_prefix("--- a/")))
            .map(str::to_owned)
            .unwrap_or_else(|| header_name(&file));
        let wanted_hunks: Vec<usize> = hunks
            .iter()
            .filter_map(|h| h.rsplit_once(':'))
            .filter(|(f, _)| *f == name)
            .filter_map(|(_, n)| n.parse().ok())
            .collect();
        let file_wanted = paths.iter().any(|p| *p == name || ptyro_agents::glob::matches(p, &name));
        if !file_wanted && wanted_hunks.is_empty() {
            continue;
        }
        if file_wanted && wanted_hunks.is_empty() {
            out.push_str(file.trim_end_matches('\n'));
            out.push('\n');
            continue;
        }
        let mut parts = file.split("\n@@");
        let header = parts.next().unwrap_or("");
        out.push_str(header.trim_end_matches('\n'));
        out.push('\n');
        for (i, h) in parts.enumerate() {
            if wanted_hunks.contains(&(i + 1)) {
                out.push_str("@@");
                out.push_str(h.trim_end_matches('\n'));
                out.push('\n');
            }
        }
    }
    out
}

impl Daemon {
    /// The steps that compete in a run: the verify step's candidates, else every spawn step.
    fn candidates_of(&self, run: &str) -> Result<(Vec<String>, Option<Step>), Error> {
        let r = self
            .run_recipe_pub(run)
            .ok_or_else(|| Error::invalid(format!("run {run} has no steps to compare")))?;
        let verify = r.steps.iter().find(|s| s.kind() == Some(Kind::Verify)).cloned();
        let ids = match verify
            .as_ref()
            .and_then(|v| v.verify.as_ref())
            .filter(|v| !v.candidates.is_empty())
        {
            Some(v) => v.candidates.clone(),
            None => r
                .steps
                .iter()
                .filter(|s| s.kind() == Some(Kind::Spawn))
                .map(|s| s.id.clone())
                .collect(),
        };
        if ids.is_empty() {
            return Err(Error::invalid(format!("run {run} has no candidates (spawn steps)")));
        }
        Ok((ids, verify))
    }

    fn candidate_worktree(&self, run: &str, step: &str) -> Result<WorktreeInfo, Error> {
        let st = self.run_info(run).ok_or_else(|| Error::missing("run", run))?.state;
        let w = st
            .steps
            .get(step)
            .and_then(|s| s.attempts.iter().rev().find_map(|a| a.worktree.clone()))
            .ok_or_else(|| Error::missing("worktree", format!("of candidate {step}")))?;
        self.worktree_by_id(&w)
            .ok_or_else(|| Error::conflict(format!("candidate {step}'s worktree is gone")))
    }

    /// Run the evidence-gathering of a verify step (or a fanout's `rank`).
    pub(crate) async fn exec_verify(
        self: &Arc<Self>,
        run: &str,
        def: &Step,
    ) -> Result<StepResult, Error> {
        let v = def.verify.clone().unwrap_or_default();
        let (ids, _) = self.candidates_of(run)?;
        let ctx = self.run_info(run).map(|i| i.state.context(&i.dir)).unwrap_or_default();
        let found = self.gather(run, &def.id, &ids, &v, &ctx).await?;
        let doc = self.compare_with(run, &ids, &found.0, found.1).await?;
        let best = doc
            .candidates
            .iter()
            .find(|c| c.rank == 1 && c.status == "succeeded")
            .map(|c| c.step.clone());
        let mut res = match &best {
            Some(_) => StepResult::ok("evidence"),
            None => StepResult::fail("failed", "no candidate succeeded"),
        };
        res.outputs.insert("best".into(), json!(best));
        res.outputs.insert("evidence".into(), serde_json::to_value(&found.0).unwrap_or_default());
        res.outputs.insert("base_tests".into(), json!(found.1));
        res.outputs.insert(
            "ranking".into(),
            json!(
                doc.candidates
                    .iter()
                    .map(|c| json!({ "step": c.step, "rank": c.rank, "reasons": c.reasons }))
                    .collect::<Vec<_>>()
            ),
        );
        Ok(res)
    }

    /// Tests, repro, cross-tests and the judge, per candidate (plus the base's test result).
    async fn gather(
        self: &Arc<Self>,
        run: &str,
        step: &str,
        ids: &[String],
        v: &VerifyDef,
        ctx: &Value,
    ) -> Result<(BTreeMap<String, Found>, Option<i32>), Error> {
        let render = |s: &Option<String>, shell: bool| {
            s.as_deref()
                .map(|x| {
                    if shell {
                        ptyro_run::template::render_shell(x, ctx)
                    } else {
                        ptyro_run::template::render(x, ctx)
                    }
                })
                .filter(|x| !x.trim().is_empty())
        };
        // Commands get shell-quoted values; a glob and an agent name don't.
        let (tests, repro, cross, judge) = (
            render(&v.tests, true),
            render(&v.repro, true),
            render(&v.cross_tests, false),
            render(&v.judge, false),
        );
        let dir = self.run_dir(run).join(step);
        let mut found: BTreeMap<String, Found> =
            ids.iter().map(|i| (i.clone(), Found::default())).collect();
        let live: Vec<(String, WorktreeInfo)> = ids
            .iter()
            .filter_map(|i| self.candidate_worktree(run, i).ok().map(|w| (i.clone(), w)))
            .collect();
        let Some((_, first)) = live.first().cloned() else { return Ok((found, None)) };
        // The base, in a throwaway worktree, for "did it pass before?".
        let mut base_tests = None;
        let base = self.diff_base(&first).await.unwrap_or_else(|_| "HEAD".into());
        let p = self
            .project_by_id(&first.project)
            .ok_or_else(|| Error::missing("project", &first.project))?;
        if tests.is_some() || repro.is_some() {
            let scratch = dir.join("base");
            let (root, b, sc) = (p.root.clone(), base.clone(), scratch.clone());
            if blocking(move || ptyro_git::Git::new().scratch(&root, &sc, &b)).await?.is_ok() {
                // Whatever happens in here, the scratch worktree is dropped after.
                let r: Result<(), Error> = async {
                let env = self.worktree_env(&first);
                if let Some(t) = &tests {
                    let cmd = self.resolve_command(&first, t)?;
                    base_tests = Some(
                        self.run_in(&scratch, &cmd, &dir.join("base-tests.log"), env.clone(), None)
                            .await?
                            .exit_code,
                    );
                }
                if let Some(r) = &repro {
                    let cmd = self.resolve_command(&first, r)?;
                    let on_base = self
                        .run_in(&scratch, &cmd, &dir.join("base-repro.log"), env, None)
                        .await?
                        .exit_code;
                    if on_base == 0 {
                        self.emit(run, vec![RunEvent::Note { text: format!("{step}: the repro passes on the base already, so it proves nothing") }]);
                    }
                }
                Ok(())
                }
                .await;
                let (root, sc) = (p.root.clone(), scratch.clone());
                let _ = blocking(move || ptyro_git::Git::new().drop_scratch(&root, &sc)).await;
                r?;
            }
        }
        for (id, wt) in &live {
            let f = found.entry(id.clone()).or_default();
            if let Some(t) = &tests {
                f.tests = Some(
                    self.run_check(wt, t, &dir.join(format!("{id}-tests.log")), None)
                        .await?
                        .exit_code,
                );
            }
            if let Some(r) = &repro {
                f.repro = Some(
                    self.run_check(wt, r, &dir.join(format!("{id}-repro.log")), None)
                        .await?
                        .exit_code
                        == 0,
                );
            }
        }
        // Cross-tests: candidate X's changed test files, run against candidate Y's tree.
        if let (Some(glob), Some(t)) = (&cross, &tests) {
            let mut snaps = BTreeMap::new();
            for (id, wt) in &live {
                let path = wt.path.clone();
                if let Ok(Ok(s)) =
                    blocking(move || ptyro_git::Git::new().checkpoint(&path, "ptyro cross-test"))
                        .await
                {
                    snaps.insert(id.clone(), s.commit);
                }
            }
            for (y, wt_y) in &live {
                let (mut passed, mut of) = (0, 0);
                for (x, _) in &live {
                    if x == y {
                        continue;
                    }
                    let (Some(sx), Some(sy)) = (snaps.get(x).cloned(), snaps.get(y).cloned())
                    else {
                        continue;
                    };
                    let (root, b, g) = (p.root.clone(), base.clone(), glob.clone());
                    let sx2 = sx.clone();
                    let files: Vec<String> = blocking(move || {
                        ptyro_git::Git::new().changed(&root, &b, Some(&sx2)).unwrap_or_default()
                    })
                    .await?
                    .into_iter()
                    .filter(|f| ptyro_agents::glob::matches(&g, f))
                    .collect();
                    if files.is_empty() {
                        continue;
                    }
                    of += 1;
                    let sc = dir.join(format!("cross-{x}-on-{y}"));
                    let (root, sc2) = (p.root.clone(), sc.clone());
                    let ready = blocking(move || -> bool {
                        let g = ptyro_git::Git::new();
                        if g.scratch(&root, &sc2, &sy).is_err() {
                            return false;
                        }
                        let mut args = vec!["checkout".to_owned(), sx.clone(), "--".to_owned()];
                        args.extend(files);
                        g.run(&sc2, &args.iter().map(String::as_str).collect::<Vec<_>>()).is_ok()
                    })
                    .await?;
                    let r: Result<bool, Error> = async {
                        if !ready {
                            return Ok(false);
                        }
                        let cmd = self.resolve_command(wt_y, t)?;
                        let code = self
                            .run_in(
                                &sc,
                                &cmd,
                                &dir.join(format!("cross-{x}-on-{y}.log")),
                                self.worktree_env(wt_y),
                                None,
                            )
                            .await?
                            .exit_code;
                        Ok(code == 0)
                    }
                    .await;
                    let (root, sc2) = (p.root.clone(), sc.clone());
                    let _ = blocking(move || ptyro_git::Git::new().drop_scratch(&root, &sc2)).await;
                    if r? {
                        passed += 1;
                    }
                }
                if of > 0 {
                    found.entry(y.clone()).or_default().cross = Some((passed, of));
                }
            }
        }
        if let Some(agent) = &judge {
            self.judge(run, step, agent, &live, &base, &mut found).await;
        }
        Ok((found, base_tests))
    }

    /// An advisory judge: another agent, a clean context, the diffs as files, rubric JSON back.
    async fn judge(
        self: &Arc<Self>,
        run: &str,
        step: &str,
        agent: &str,
        live: &[(String, WorktreeInfo)],
        base: &str,
        found: &mut BTreeMap<String, Found>,
    ) {
        let dir = self.run_dir(run).join(step).join("judge");
        let _ = std::fs::create_dir_all(&dir);
        for (id, wt) in live {
            let (path, b) = (wt.path.clone(), base.to_owned());
            let diff = blocking(move || {
                let g = ptyro_git::Git::new();
                let snap = g.checkpoint(&path, "ptyro judge").ok()?;
                g.diff_between(&path, &b, &snap.commit, false, &[]).ok()
            })
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
            let _ = std::fs::write(dir.join(format!("{id}.diff")), diff);
        }
        let task = self
            .run_info(run)
            .and_then(|i| i.state.vars.get("prompt").or(i.state.vars.get("task")).cloned())
            .unwrap_or_default();
        let out = dir.join("verdict.json");
        let ids: Vec<&str> = live.iter().map(|(i, _)| i.as_str()).collect();
        let prompt = format!(
            "You are judging {} candidate changes for this task:\n{task}\n\nEach candidate's diff is in this directory: {}. Read them all. Score each 0-10 for correctness and fit (not style). Don't edit anything.\nWrite JSON to {} shaped like {{\"scores\": {{\"{}\": 7, …}}, \"notes\": {{\"{}\": \"one line\", …}}}}",
            ids.len(),
            ids.iter().map(|i| format!("{i}.diff")).collect::<Vec<_>>().join(", "),
            out.display(),
            ids[0],
            ids[0]
        );
        let spec = ptyro_core::session::SpawnSpec {
            agent: Some(agent.to_owned()),
            cwd: dir.clone(),
            prompt: Some(prompt),
            name: Some(self.free_name("judge")),
            confirmed: true,
            unqueued: true,
            ..Default::default()
        };
        let Ok(s) = self.spawn(spec).await else { return };
        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).run =
            Some((run.to_owned(), step.to_owned()));
        self.emit(run, vec![RunEvent::SessionAdded { session: s.id.full() }]);
        let t = ptyro_run::recipe::TimeoutFull {
            start: Some("3m".into()),
            stall: Some("10m".into()),
            total: None,
        };
        let ok = tokio::time::timeout(
            std::time::Duration::from_secs(1200),
            self.wait_turn(&s, ptyro_run::recipe::WaitFor::Idle, &t),
        )
        .await;
        if matches!(ok, Ok(Ok(()))) {
            crate::runs::wait_for_file(&out, std::time::Duration::from_secs(30)).await;
        }
        if s.is_live() {
            self.stop_session(&s).await;
        }
        if !matches!(ok, Ok(Ok(()))) {
            self.emit(
                run,
                vec![RunEvent::Note {
                    text: format!("{step}: the judge didn't finish; ranking without it"),
                }],
            );
            return;
        }
        let Ok(v) = std::fs::read_to_string(&out)
            .map(|t| serde_json::from_str::<Value>(&t).unwrap_or_default())
        else {
            return;
        };
        for (id, f) in found.iter_mut() {
            f.judge = v["scores"][id].as_f64().map(|x| x.clamp(0.0, 10.0).round() as u8);
            f.judge_note = v["notes"][id].as_str().map(str::to_owned);
        }
    }

    /// The compare view: every candidate with its evidence, ranked.
    pub(crate) async fn compare(
        self: &Arc<Self>,
        run: &str,
        refresh: bool,
    ) -> Result<CompareDoc, Error> {
        let (ids, verify) = self.candidates_of(run)?;
        let st = self.run_info(run).ok_or_else(|| Error::missing("run", run))?.state;
        let cached =
            verify.as_ref().and_then(|v| st.steps.get(&v.id)).filter(|s| s.status.is_over());
        let (found, base_tests) = match (cached, refresh) {
            (Some(s), false) => (
                s.outputs
                    .get("evidence")
                    .and_then(|e| serde_json::from_value(e.clone()).ok())
                    .unwrap_or_default(),
                s.outputs.get("base_tests").and_then(Value::as_i64).map(|x| x as i32),
            ),
            _ => {
                let v = verify.as_ref().and_then(|v| v.verify.clone()).unwrap_or_default();
                let sid = verify.as_ref().map_or_else(|| "compare".to_owned(), |v| v.id.clone());
                let ctx = self.run_info(run).map(|i| i.state.context(&i.dir)).unwrap_or_default();
                // One gathering per run at a time: they share scratch directories and logs, and
                // one's cleanup would delete the other's.
                let lock = gather_lock(run);
                let _one = lock.lock().await;
                let (f, b) = self.gather(run, &sid, &ids, &v, &ctx).await?;
                if let Some(v) = &verify {
                    self.emit(
                        run,
                        vec![RunEvent::Outputs {
                            step: v.id.clone(),
                            outputs: BTreeMap::from([
                                ("evidence".into(), serde_json::to_value(&f).unwrap_or_default()),
                                ("base_tests".into(), json!(b)),
                            ]),
                        }],
                    );
                }
                (f, b)
            }
        };
        self.compare_with(run, &ids, &found, base_tests).await
    }

    async fn compare_with(
        self: &Arc<Self>,
        run: &str,
        ids: &[String],
        found: &BTreeMap<String, Found>,
        base_tests: Option<i32>,
    ) -> Result<CompareDoc, Error> {
        let info = self.run_info(run).ok_or_else(|| Error::missing("run", run))?;
        let st = &info.state;
        let recipe = self.run_recipe_pub(run);
        let ctx = st.context(&info.dir);
        let mut out = Vec::new();
        let mut sigs: Vec<(u64, String)> = Vec::new();
        let mut agents = std::collections::BTreeSet::new();
        for id in ids {
            let ss = st.steps.get(id).cloned().unwrap_or_default();
            let a = ss.attempt();
            let agent = recipe
                .as_ref()
                .and_then(|r| r.step(id))
                .and_then(|s| s.spawn.as_ref())
                .map(|sp| ptyro_run::template::render(&sp.agent, &ctx));
            if let Some(a) = &agent {
                agents.insert(ptyro_agents::split_agent(a).0.to_owned());
            }
            let mut c = Candidate {
                step: id.clone(),
                agent,
                status: ss.status.label().into(),
                duration_ms: ss
                    .started_ms
                    .map_or(0, |s| ss.ended_ms.unwrap_or_else(now_ms).saturating_sub(s)),
                session: a.and_then(|a| a.session.clone()),
                last_message: ss
                    .outputs
                    .get("last_message")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                picked: st.picked.iter().any(|(p, _)| p == id),
                discarded: st.discarded.contains(id),
                ..Candidate::default()
            };
            // Structured candidates (M10): what their tool calls add up to.
            c.tools = c
                .session
                .as_deref()
                .and_then(|id| self.resolve(id).ok())
                .and_then(|sess| {
                    let st = sess.structured.as_ref()?;
                    Some(crate::structured::tool_use(st.chat.locked().tool_summary()))
                })
                .or_else(|| {
                    ss.outputs.get("tools").and_then(|v| serde_json::from_value(v.clone()).ok())
                });
            if let Some(v) = ss.outputs.get("verified").and_then(Value::as_bool) {
                c.verified = Some(v);
            }
            if let Some(f) = found.get(id) {
                c.tests = f.tests;
                c.repro = f.repro;
                c.cross = f.cross;
                c.judge = f.judge;
                c.judge_note = f.judge_note.clone();
                c.broke_tests = base_tests == Some(0) && f.tests.is_some_and(|t| t != 0);
            }
            if let Ok(wt) = self.candidate_worktree(run, id) {
                c.worktree = Some(wt.id.clone());
                c.worktree_name = Some(wt.qualified());
                if let Ok(doc) = self.worktree_diff(&wt.id, DiffAgainst::Base).await {
                    c.files = doc.files.len() as u32;
                    c.added = doc.files.iter().map(|f| f.added).sum();
                    c.removed = doc.files.iter().map(|f| f.removed).sum();
                    c.hunks = doc.files.iter().map(|f| f.hunks.len() as u32).sum();
                    if !doc.files.is_empty() {
                        let sig = signature(&doc);
                        let n = match sigs.iter().position(|(s, _)| *s == sig) {
                            Some(i) => i as u32 + 1,
                            None => {
                                sigs.push((sig, id.clone()));
                                sigs.len() as u32
                            }
                        };
                        c.cluster = n;
                    }
                }
            }
            out.push(c);
        }
        // Lexicographic: each signal only breaks ties left by the ones before it.
        let key = |c: &Candidate| {
            (
                u8::from(c.status == "succeeded"),
                match c.tests {
                    Some(0) => 2u8,
                    None => 1,
                    _ => 0,
                },
                u8::from(!c.broke_tests),
                match c.repro {
                    Some(true) => 2u8,
                    None => 1,
                    _ => 0,
                },
                c.cross.map_or(50, |(p, o)| (p * 100).checked_div(o).unwrap_or(50)),
                match c.verified {
                    Some(true) => 2u8,
                    None => 1,
                    _ => 0,
                },
                u8::from(c.files > 0),
                c.judge.unwrap_or(5),
                std::cmp::Reverse(c.added + c.removed),
                std::cmp::Reverse(c.duration_ms),
            )
        };
        out.sort_by_key(|c| std::cmp::Reverse(key(c)));
        let sizes: Vec<u32> =
            out.iter().filter(|c| c.files > 0).map(|c| c.added + c.removed).collect();
        // "smallest" only says something when one candidate is.
        let smallest =
            sizes.iter().min().copied().filter(|m| sizes.iter().filter(|x| *x == m).count() == 1);
        let cluster_sizes: BTreeMap<u32, usize> =
            out.iter().filter(|c| c.cluster > 0).fold(BTreeMap::new(), |mut m, c| {
                *m.entry(c.cluster).or_default() += 1;
                m
            });
        for (i, c) in out.iter_mut().enumerate() {
            c.rank = i as u32 + 1;
            let mut r = Vec::new();
            if c.status != "succeeded" {
                r.push(format!("step {}", c.status));
            }
            match c.tests {
                Some(0) => r.push("tests pass".into()),
                Some(code) => r.push(format!("tests fail (exit {code})")),
                None => {}
            }
            if c.broke_tests {
                r.push("broke tests that pass on the base".into());
            }
            match c.repro {
                Some(true) => r.push("repro fixed".into()),
                Some(false) => r.push("repro still fails".into()),
                None => {}
            }
            if let Some((p, o)) = c.cross {
                r.push(format!("passes {p}/{o} of the others' tests"));
            }
            match c.verified {
                Some(true) => r.push("verified".into()),
                Some(false) => r.push("claimed done, not verified".into()),
                None => {}
            }
            if c.files == 0 {
                r.push("changed nothing".into());
            }
            if let Some(j) = c.judge {
                r.push(format!("judge {j}/10"));
            }
            if c.files > 0 {
                let lines = c.added + c.removed;
                r.push(format!(
                    "{} file(s) +{} −{}{}",
                    c.files,
                    c.added,
                    c.removed,
                    if Some(lines) == smallest { " (smallest)" } else { "" }
                ));
            }
            if cluster_sizes.get(&c.cluster).is_some_and(|n| *n > 1) {
                r.push(format!(
                    "same change as {} other(s) (cluster {})",
                    cluster_sizes[&c.cluster] - 1,
                    c.cluster
                ));
            }
            c.reasons = r;
        }
        let mut warnings = Vec::new();
        if out.len() > 6 {
            warnings.push("more than 6 candidates rarely finds a better answer".into());
        }
        if out.len() > 1 && agents.len() == 1 {
            warnings.push("every candidate is the same agent; mixed agents fail differently (--agents claude,codex,gemini)".into());
        }
        if out.iter().all(|c| c.tests.is_none()) {
            warnings.push(
                "no tests ran: ranked on status and diff only (fanout --test, or verify.tests)"
                    .into(),
            );
        }
        let top_difference = match (
            out.first().and_then(|c| c.worktree.clone()),
            out.get(1).and_then(|c| c.worktree.clone()),
        ) {
            (Some(a), Some(b)) => {
                let (wa, wb) = (self.worktree_by_id(&a), self.worktree_by_id(&b));
                match (wa, wb) {
                    (Some(wa), Some(wb)) => blocking(move || {
                        let g = ptyro_git::Git::new();
                        let sa = g.checkpoint(&wa.path, "ptyro compare").ok()?;
                        let sb = g.checkpoint(&wb.path, "ptyro compare").ok()?;
                        g.diff_between(&wa.path, &sa.commit, &sb.commit, true, &[]).ok()
                    })
                    .await
                    .ok()
                    .flatten(),
                    _ => None,
                }
            }
            _ => None,
        };
        let base = out
            .first()
            .and_then(|c| c.worktree.as_deref())
            .and_then(|w| self.worktree_by_id(w))
            .and_then(|w| w.base);
        Ok(CompareDoc {
            run: run.into(),
            base,
            base_tests,
            candidates: out,
            top_difference,
            warnings,
        })
    }

    pub(crate) async fn handle_compare(self: &Arc<Self>, req: Request) -> Result<Reply, Error> {
        match req {
            Request::Compare { run, refresh } => {
                let run = self.find_run(&run)?;
                Ok(Reply::Compare { doc: self.compare(&run, refresh).await? })
            }
            Request::Pick { run, candidate, mode, into, paths, hunks } => {
                let run = self.find_run(&run)?;
                let wt = self.candidate_worktree(&run, &candidate)?;
                let mut notes = Vec::new();
                let label = match mode {
                    PickMode::Merge => "merge",
                    PickMode::CherryPick => "cherry-pick",
                    PickMode::Pr => "pr",
                    PickMode::Apply => "apply",
                };
                if mode != PickMode::Apply {
                    let path = wt.path.clone();
                    if blocking(move || ptyro_git::Git::new().dirty(&path)).await? {
                        let msg = format!("ptyro: {candidate} from run {run}");
                        if let Reply::Committed { sha } = self
                            .handle_review(Request::WorktreeCommit {
                                addr: wt.id.clone(),
                                message: msg,
                                paths: vec![],
                                all: true,
                            })
                            .await?
                        {
                            notes.push(format!(
                                "committed {}'s uncommitted work as {}",
                                candidate,
                                &sha[..sha.len().min(10)]
                            ));
                        }
                    }
                }
                match mode {
                    PickMode::Merge | PickMode::CherryPick => {
                        if let Reply::Notes { notes: n } = self
                            .handle_review(Request::WorktreeIntegrate {
                                addr: wt.id.clone(),
                                into,
                                cherry_pick: mode == PickMode::CherryPick,
                                push: false,
                            })
                            .await?
                        {
                            notes.extend(n);
                        }
                    }
                    PickMode::Pr => match self
                        .pr_create(&wt.id, self.config().github.draft, None, None, None)
                        .await?
                    {
                        Reply::Pr { info: Some(pr) } => {
                            notes.push(format!("opened PR #{} {}", pr.number, pr.url))
                        }
                        _ => notes.push("opened a PR".into()),
                    },
                    PickMode::Apply => {
                        if paths.is_empty() && hunks.is_empty() {
                            return Err(Error::invalid(
                                "say what to take: --path FILE (repeatable) or --hunk FILE:N",
                            ));
                        }
                        let target = match &into {
                            Some(a) => self.find_worktree(a)?,
                            None => self
                                .root_worktree(&wt.project)
                                .ok_or_else(|| Error::missing("root worktree", &wt.project))?,
                        };
                        let base = self.diff_base(&wt).await?;
                        let (src, dst) = (wt.path.clone(), target.path.clone());
                        let n = blocking(move || -> Result<usize, Error> {
                            let g = ptyro_git::Git::new();
                            let snap = g
                                .checkpoint(&src, "ptyro pick")
                                .map_err(crate::registry::git_err)?;
                            let patch = g
                                .diff_between(&src, &base, &snap.commit, false, &[])
                                .map_err(crate::registry::git_err)?;
                            let chosen = filter_patch(&patch, &paths, &hunks);
                            if chosen.trim().is_empty() {
                                return Err(Error::invalid(
                                    "nothing matched those files or hunks (see `ptyro wt diff`)",
                                ));
                            }
                            g.apply(&dst, &chosen).map_err(crate::registry::git_err)?;
                            Ok(chosen.matches("\n@@").count()
                                + usize::from(chosen.starts_with("@@")))
                        })
                        .await??;
                        notes.push(format!(
                            "applied {n} hunk(s) from {candidate} to {} (uncommitted)",
                            target.qualified()
                        ));
                    }
                }
                self.emit(&run, vec![RunEvent::Picked { step: candidate, mode: label.into() }]);
                Ok(Reply::Notes { notes })
            }
            Request::Discard { run, candidate, force } => {
                let run = self.find_run(&run)?;
                let st = self.run_info(&run).ok_or_else(|| Error::missing("run", &run))?.state;
                let (ids, _) = self.candidates_of(&run)?;
                let targets: Vec<String> = match candidate {
                    Some(c) => vec![c],
                    None => ids
                        .into_iter()
                        .filter(|i| {
                            !st.picked.iter().any(|(p, _)| p == i) && !st.discarded.contains(i)
                        })
                        .collect(),
                };
                let mut notes = Vec::new();
                for c in targets {
                    let Ok(wt) = self.candidate_worktree(&run, &c) else {
                        notes.push(format!("{c}: no worktree (already gone)"));
                        continue;
                    };
                    match self
                        .remove_worktree(
                            &wt.id,
                            RemoveOpts { force, stop_sessions: true, keep_branch: false },
                        )
                        .await
                    {
                        Ok(Reply::Removed { notes: n }) => {
                            notes.push(format!("{c}: removed {}", wt.qualified()));
                            notes.extend(n.into_iter().map(|x| format!("  {x}")));
                            self.emit(&run, vec![RunEvent::Discarded { step: c }]);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            notes.push(format!("{c}: kept {} — {}", wt.qualified(), e.message))
                        }
                    }
                }
                Ok(Reply::Notes { notes })
            }
            Request::Iterate { run, candidate, text } => {
                let run = self.find_run(&run)?;
                let st = self.run_info(&run).ok_or_else(|| Error::missing("run", &run))?.state;
                let (ids, _) = self.candidates_of(&run)?;
                let targets: Vec<String> = match candidate {
                    Some(c) => vec![c],
                    None => ids.into_iter().filter(|i| !st.discarded.contains(i)).collect(),
                };
                let mut notes = Vec::new();
                for c in targets {
                    let Some(sid) =
                        st.steps.get(&c).and_then(|s| s.attempt()).and_then(|a| a.session.clone())
                    else {
                        continue;
                    };
                    let Ok(mut s) = self.resolve(&sid) else { continue };
                    if !s.is_live() {
                        if self.resume(&sid).await.is_err() {
                            notes.push(format!("{c}: its agent has exited and can't resume"));
                            continue;
                        }
                        s = self.resolve(&sid)?;
                    }
                    self.reset_verify(&s);
                    self.deliver_when_idle(&s, text.clone());
                    notes.push(format!(
                        "{c}: sent to {} (typed once its turn is over)",
                        s.meta.lock().unwrap_or_else(std::sync::PoisonError::into_inner).name
                    ));
                }
                Ok(Reply::Notes { notes })
            }
            other => Err(Error::invalid(format!("unsupported request {other:?}"))),
        }
    }

    pub(crate) fn run_recipe_pub(&self, run: &str) -> Option<Arc<ptyro_run::Recipe>> {
        self.runs.locked().get(run).and_then(|e| e.recipe.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patches_filter_by_file_and_hunk() {
        let patch = "diff --git a/a.txt b/a.txt\nindex 1..2 100644\n--- a/a.txt\n+++ b/a.txt\n@@ -1,2 +1,2 @@\n-x\n+y\n@@ -10,2 +10,2 @@\n-p\n+q\ndiff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-1\n+2\n";
        let only_b = filter_patch(patch, &["b.txt".into()], &[]);
        assert!(only_b.contains("+++ b/b.txt") && !only_b.contains("a.txt"));
        let second = filter_patch(patch, &[], &["a.txt:2".into()]);
        assert!(second.contains("+q") && !second.contains("+y"), "{second}");
        assert!(second.starts_with("diff --git a/a.txt"));
        assert!(filter_patch(patch, &["nope".into()], &[]).is_empty());
    }

    #[test]
    fn binary_files_renames_and_quoted_names_are_found_by_their_header() {
        // No `+++` line in any of these: only the `diff --git` line has the name.
        let patch = concat!(
            "diff --git a/img.png b/img.png\nindex 1..2 100644\nBinary files a/img.png and b/img.png differ\n",
            "diff --git a/old.txt b/new name.txt\nsimilarity index 100%\nrename from old.txt\nrename to new name.txt\n",
            "diff --git \"a/\\303\\244.txt\" \"b/\\303\\244.txt\"\nindex 1..2 100644\nBinary files differ\n",
        );
        assert!(filter_patch(patch, &["img.png".into()], &[]).contains("img.png"));
        let renamed = filter_patch(patch, &["new name.txt".into()], &[]);
        assert!(
            renamed.contains("rename to new name.txt") && !renamed.contains("img.png"),
            "{renamed}"
        );
        let quoted = filter_patch(patch, &["ä.txt".into()], &[]);
        assert!(quoted.contains("Binary files differ") && !quoted.contains("img.png"), "{quoted}");
        assert_eq!(header_name("diff --git a/x y b/x y\n"), "x y");
    }
}
