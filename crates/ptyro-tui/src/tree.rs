//! The sidebar tree (SPEC §11.1, M3.12; stacked in MM1.B, §16.4b): every workspace, this
//! machine's first and then each other machine's, as a header with its projects → worktrees →
//! sessions under it and then its other sessions. Pure: built from what the daemon reported, so it
//! is unit-tested.

use std::collections::HashSet;

use ptyro_core::SessionId;
use ptyro_core::model::{
    MAIN_WORKSPACE, ProjectInfo, WorkspaceInfo, WorktreeInfo, WorktreeState, split_machine,
};
use ptyro_core::session::{SessionInfo, Show};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// A workspace's header (by name: `work`, `laptop2:work`); folds what is under it.
    Workspace(String),
    Project(String),
    Worktree(String),
    Session(SessionId),
    /// A quick terminal (UX1.25): first under its worktree, with no number, and not one of the
    /// sessions `j`/`k` cycle through.
    Quick(SessionId),
    /// A non-selectable label (`other sessions`).
    Group(String),
    /// A change set (its run id), above its lead (M11.11).
    Set(String),
    /// A workspace's CHATS heading (SPEC §22.12); ⏎ lists its chats.
    Chats(String),
    /// A chat (its id).
    Chat(String),
    /// `＋ new chat` in a workspace with none yet.
    ChatNew(String),
    /// `all chats… (N)`: the workspace's chat list.
    ChatAll(String),
}

impl Row {
    pub fn selectable(&self) -> bool {
        !matches!(self, Self::Group(_))
    }

    /// The chat a chat row stands for.
    pub fn chat(&self) -> Option<&str> {
        match self {
            Self::Chat(id) => Some(id),
            _ => None,
        }
    }

    /// The session a session or quick-terminal row stands for.
    pub fn session(&self) -> Option<SessionId> {
        match self {
            Self::Session(id) | Self::Quick(id) => Some(*id),
            _ => None,
        }
    }
}

/// The key a folded workspace has in `collapsed` (never a project's or a worktree's id).
pub fn fold_key(workspace: &str) -> String {
    format!("ws:{workspace}")
}

/// The key a workspace's folded CHATS section has in `collapsed`.
pub fn chats_key(workspace: &str) -> String {
    format!("chats:{workspace}")
}

pub struct Inputs<'a> {
    pub workspaces: &'a [WorkspaceInfo],
    pub projects: &'a [ProjectInfo],
    pub worktrees: &'a [WorktreeInfo],
    pub sessions: &'a [SessionInfo],
    /// Collapsed project and worktree ids, and folded workspaces (`fold_key`).
    pub collapsed: &'a HashSet<String>,
    /// Change sets still going (M11): shown first, each with its lead.
    pub sets: &'a [crate::sets::SetNode],
    /// The sidebar's filter: everything, what is running, or what is busy now. A header with
    /// nothing under it is dropped while it filters.
    pub show: Show,
    /// Chats not archived, pinned first then the newest (SPEC §22.12).
    pub chats: &'a [ptyro_core::chats::ChatInfo],
    /// How many unpinned chats each CHATS section shows; `None`: chats are off.
    pub recent: Option<usize>,
}

impl Inputs<'_> {
    /// The workspaces in the order they are shown: this machine's as the daemon lists them,
    /// then each other machine's, machine by machine.
    pub fn order(&self) -> Vec<&WorkspaceInfo> {
        let mut v: Vec<&WorkspaceInfo> = self.workspaces.iter().collect();
        v.sort_by_key(|w| w.machine.clone());
        v
    }

    /// The workspace a session is listed under: its project's (the first that lists it), else
    /// its home, else its machine's `main` (or this one's).
    pub fn home(&self, s: &SessionInfo) -> String {
        if let Some(p) = &s.project
            && let Some(w) = self.order().into_iter().find(|w| w.projects.contains(p))
        {
            return w.name.clone();
        }
        let names: Vec<&str> = self.workspaces.iter().map(|w| w.name.as_str()).collect();
        if let Some(h) = s.workspace.as_deref().filter(|h| names.contains(h)) {
            return h.to_owned();
        }
        let main = match &s.machine {
            Some(m) => format!("{m}:{MAIN_WORKSPACE}"),
            None => MAIN_WORKSPACE.to_owned(),
        };
        if names.contains(&main.as_str()) {
            return main;
        }
        // A machine without a `main` here (hidden): its first shown workspace, else ours.
        s.machine
            .as_ref()
            .and_then(|m| {
                self.order()
                    .into_iter()
                    .find(|w| w.name.split_once(':').is_some_and(|(x, _)| x == m))
                    .map(|w| w.name.clone())
            })
            .unwrap_or_else(|| MAIN_WORKSPACE.to_owned())
    }

    /// Every project shown, in tree order (each once).
    pub fn project_ids(&self) -> Vec<String> {
        let mut seen = HashSet::new();
        self.order()
            .into_iter()
            .flat_map(|w| w.projects.iter())
            .filter(|id| self.projects.iter().any(|p| p.id == **id))
            .filter(|id| seen.insert((*id).clone()))
            .cloned()
            .collect()
    }

    /// One workspace's projects, in its order.
    pub fn project_ids_of(&self, workspace: &str) -> Vec<String> {
        self.workspaces
            .iter()
            .find(|w| w.name == workspace)
            .map(|w| {
                w.projects
                    .iter()
                    .filter(|id| self.projects.iter().any(|p| p.id == **id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn worktrees_of(&self, project: &str) -> Vec<&WorktreeInfo> {
        let mut v: Vec<&WorktreeInfo> = self
            .worktrees
            .iter()
            .filter(|w| w.project == project && w.state != WorktreeState::Removed)
            .collect();
        v.sort_by_key(|w| (!w.is_root, w.created_ms));
        v
    }

    fn filtering(&self) -> bool {
        self.show != Show::All
    }

    fn led(&self, s: &SessionInfo) -> bool {
        self.sets.iter().any(|n| n.lead.as_deref() == Some(s.id.full().as_str()))
    }

    /// The rows, honouring collapsed nodes.
    pub fn rows(&self) -> Vec<Row> {
        self.build(true)
    }

    /// Every session in tree order, collapsed or not (cycling, numbering).
    pub fn sessions(&self) -> Vec<SessionId> {
        self.build(false)
            .into_iter()
            .filter_map(|r| match r {
                Row::Session(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    /// The workspace a row is in: the header above it.
    pub fn workspace_of(rows: &[Row], i: usize) -> Option<String> {
        rows.get(..=i.min(rows.len().saturating_sub(1)))?.iter().rev().find_map(|r| match r {
            Row::Workspace(w) => Some(w.clone()),
            _ => None,
        })
    }

    fn build(&self, honour_collapse: bool) -> Vec<Row> {
        let mut rows = Vec::new();
        for n in self.sets {
            let lead =
                n.lead.as_ref().and_then(|l| self.sessions.iter().find(|s| s.id.full() == *l));
            if self.filtering() && !lead.is_some_and(|s| self.show.keeps(s)) {
                continue;
            }
            rows.push(Row::Set(n.run.clone()));
            if !(honour_collapse && self.collapsed.contains(&n.run))
                && let Some(s) = lead
            {
                rows.push(Row::Session(s.id));
            }
        }
        // Nothing known yet (before the first answer): the sessions, without headers.
        if self.workspaces.is_empty() {
            rows.extend(
                self.sessions
                    .iter()
                    .filter(|s| !s.pad && !self.led(s) && self.show.keeps(s))
                    .map(|s| if s.quick { Row::Quick(s.id) } else { Row::Session(s.id) }),
            );
            return rows;
        }
        // A project shows once, under the first workspace that has it.
        let mut placed: HashSet<&str> = HashSet::new();
        for ws in self.order() {
            let folded = honour_collapse && self.collapsed.contains(&fold_key(&ws.name));
            let body = self.body(ws, &mut placed, honour_collapse);
            if self.filtering() && body.is_empty() {
                continue;
            }
            rows.push(Row::Workspace(ws.name.clone()));
            if !folded {
                rows.extend(body);
                // Chats aren't sessions: not while the list is filtered to running or active ones.
                if !self.filtering() {
                    rows.extend(self.chat_rows(&ws.name, honour_collapse));
                }
            }
        }
        rows
    }

    fn body<'b>(
        &'b self,
        ws: &'b WorkspaceInfo,
        placed: &mut HashSet<&'b str>,
        honour_collapse: bool,
    ) -> Vec<Row> {
        let mut rows = Vec::new();
        let projects: Vec<&ProjectInfo> = ws
            .projects
            .iter()
            .filter_map(|id| self.projects.iter().find(|p| p.id == *id))
            .filter(|p| placed.insert(p.id.as_str()))
            .collect();
        for p in &projects {
            let mut inner = Vec::new();
            for w in self.worktrees_of(&p.id) {
                let (quick, sessions): (Vec<&SessionInfo>, Vec<&SessionInfo>) = self
                    .sessions
                    .iter()
                    .filter(|s| s.worktree.as_deref() == Some(w.id.as_str()) && !s.pad)
                    // A change set's lead has its row under the set, not twice.
                    .filter(|s| !self.led(s))
                    .filter(|s| self.show.keeps(s))
                    .partition(|s| s.quick);
                if self.filtering() && sessions.is_empty() {
                    continue;
                }
                inner.push(Row::Worktree(w.id.clone()));
                if honour_collapse && self.collapsed.contains(&w.id) {
                    continue;
                }
                // The quick terminal first: it is the worktree's own, so it sits right under it
                // and never gets lost among the sessions.
                inner.extend(quick.into_iter().map(|s| Row::Quick(s.id)));
                inner.extend(sessions.into_iter().map(|s| Row::Session(s.id)));
            }
            if self.filtering() && inner.is_empty() {
                continue;
            }
            rows.push(Row::Project(p.id.clone()));
            if !(honour_collapse && self.collapsed.contains(&p.id)) {
                rows.extend(inner);
            }
        }
        // The rest whose workspace this is: no project here, or a worktree that is gone (it has
        // no row to hang under, so it is listed rather than vanishing).
        let shown: Vec<&str> = projects.iter().map(|p| p.id.as_str()).collect();
        let others: Vec<&SessionInfo> = self
            .sessions
            .iter()
            .filter(|s| !s.pad && !self.led(s) && self.show.keeps(s))
            .filter(|s| {
                let under_project = s.project.as_ref().is_some_and(|p| shown.contains(&p.as_str()))
                    && s.worktree.as_ref().is_some_and(|w| {
                        self.worktrees.iter().any(|x| {
                            x.id == *w
                                && x.state != WorktreeState::Removed
                                && shown.contains(&x.project.as_str())
                        })
                    });
                // Under another workspace's project: there, not here.
                let elsewhere = s.project.as_ref().is_some_and(|p| {
                    !shown.contains(&p.as_str())
                        && self.worktrees.iter().any(|x| {
                            Some(x.id.as_str()) == s.worktree.as_deref()
                                && x.state != WorktreeState::Removed
                                && x.project == *p
                        })
                        && self.workspaces.iter().any(|w| w.projects.contains(p))
                });
                !under_project && !elsewhere && self.home(s) == ws.name
            })
            .collect();
        if !others.is_empty() {
            if !projects.is_empty() {
                rows.push(Row::Group("other sessions".into()));
            }
            let (quick, others): (Vec<&SessionInfo>, Vec<&SessionInfo>) =
                others.into_iter().partition(|s| s.quick);
            rows.extend(quick.into_iter().map(|s| Row::Quick(s.id)));
            rows.extend(others.into_iter().map(|s| Row::Session(s.id)));
        }
        rows
    }
}

impl Inputs<'_> {
    /// A workspace's CHATS section: its pinned chats, then the newest `recent`, then `all chats…`
    /// when there are more; `＋ new chat` when it has none. Other machines' workspaces have one too
    /// (SPEC §22.11): their chats are mirrored, and a new one is made there.
    fn chat_rows(&self, ws: &str, honour_collapse: bool) -> Vec<Row> {
        let Some(recent) = self.recent else { return Vec::new() };
        // Side chats sit under their chat (SPEC §22.12), not among the others.
        let mine: Vec<&ptyro_core::chats::ChatInfo> =
            self.chats.iter().filter(|c| c.workspace == ws && c.side_of.is_none()).collect();
        let sides_of = |id: &str| -> Vec<Row> {
            self.chats
                .iter()
                .filter(|c| c.side_of.as_deref() == Some(id))
                .map(|c| Row::Chat(c.id.clone()))
                .collect()
        };
        let mut rows = vec![Row::Chats(ws.to_owned())];
        if honour_collapse && self.collapsed.contains(&chats_key(ws)) {
            return rows;
        }
        if mine.is_empty() {
            rows.push(Row::ChatNew(ws.to_owned()));
            return rows;
        }
        let pinned = mine.iter().filter(|c| c.pinned);
        let newest = mine.iter().filter(|c| !c.pinned).take(recent);
        let mut shown = 0;
        for c in pinned.chain(newest) {
            rows.push(Row::Chat(c.id.clone()));
            rows.extend(sides_of(&c.id));
            shown += 1;
        }
        if shown < mine.len() {
            rows.push(Row::ChatAll(ws.to_owned()));
        }
        rows
    }

    /// How many chats a workspace has (`all chats… (N)`).
    pub fn chat_count(&self, ws: &str) -> usize {
        self.chats.iter().filter(|c| c.workspace == ws).count()
    }
}

/// `laptop2:work` → (`laptop2`, `work`); `work` → (`None`, `work`).
pub fn machine_and_name(workspace: &str) -> (Option<&str>, &str) {
    match split_machine(workspace) {
        Some((m, w)) => (Some(m), w),
        None => (None, workspace),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ptyro_core::TermSize;
    use ptyro_core::session::{ProcessState, SessionKind};

    fn session(
        name: &str,
        worktree: Option<&str>,
        project: Option<&str>,
        ws: Option<&str>,
    ) -> SessionInfo {
        SessionInfo {
            machine: None,
            id: SessionId::new(),
            handle: String::new(),
            name: name.into(),
            kind: SessionKind::Shell,
            argv: vec![],
            cwd: "/".into(),
            state: ProcessState::Running,
            size: TermSize::new(80, 24),
            pid: None,
            created_ms: 0,
            ended_ms: None,
            viewers: 0,
            controller: None,
            title: None,
            foreground: None,
            live_cwd: None,
            program_status: None,
            progress: None,
            last_command_exit: None,
            profile: "default".into(),
            worktree: worktree.map(Into::into),
            project: project.map(Into::into),
            workspace: ws.map(Into::into),
            agent: None,
            script: None,
            ports: vec![],
            status: None,
            attention: None,
            activity: None,
            conversation: None,
            children: vec![],
            dangerous: false,
            prompt_pending: false,
            last_prompt_ms: None,
            turns: 0,
            last_output_ms: None,
            verify: None,
            run: None,
            held: false,
            structured: None,
            task: None,
            last_reply: None,
            command: None,
            inner_agent: None,
            summary: None,
            window_title: None,
            quick: false,
            pad: false,
            chat: None,
        }
    }

    fn wt(id: &str, project: &str, root: bool, created: u64) -> WorktreeInfo {
        WorktreeInfo {
            machine: None,
            id: id.into(),
            project: project.into(),
            project_slug: project.into(),
            name: if root { "@root".into() } else { id.into() },
            path: format!("/{id}").into(),
            branch: None,
            base: None,
            is_root: root,
            profile: None,
            state: WorktreeState::Ready,
            ports: None,
            created_ms: created,
            removed_ms: None,
            last_used_ms: None,
            status: None,
            locked: false,
            pr: None,
            title: None,
            integrated: None,
        }
    }

    #[test]
    fn a_removed_worktrees_sessions_and_a_set_lead_show_once() {
        let ws = vec![WorkspaceInfo {
            machine: None,
            id: "w".into(),
            name: "main".into(),
            profile: "default".into(),
            notify: "all".into(),
            projects: vec!["p".into()],
            created_ms: 0,
        }];
        let projects = vec![ProjectInfo {
            machine: None,
            id: "p".into(),
            slug: "app".into(),
            root: "/app".into(),
            default_branch: None,
            remote_url: None,
            pin: None,
            created_ms: 0,
            last_used_ms: None,
        }];
        let mut gone = wt("t9", "p", false, 9);
        gone.state = WorktreeState::Removed;
        let worktrees = vec![wt("root", "p", true, 1), gone];
        let orphan = session("orphan", Some("t9"), Some("p"), Some("main"));
        let lead = session("lead", Some("root"), Some("p"), Some("main"));
        let sessions = vec![orphan.clone(), lead.clone()];
        let nodes = [crate::sets::SetNode { run: "r-1".into(), lead: Some(lead.id.full()) }];
        let collapsed = HashSet::new();
        let rows = Inputs {
            workspaces: &ws,
            projects: &projects,
            worktrees: &worktrees,
            sessions: &sessions,
            collapsed: &collapsed,
            sets: &nodes,
            show: Show::All,
            chats: &[],
            recent: None,
        }
        .rows();
        // The session of a removed worktree has no worktree row to hang under: it is listed with
        // the others rather than vanishing, and the change set's lead is under the set only.
        assert_eq!(rows.iter().filter(|r| **r == Row::Session(orphan.id)).count(), 1, "{rows:?}");
        assert_eq!(rows.iter().filter(|r| **r == Row::Session(lead.id)).count(), 1, "{rows:?}");
        assert_eq!(rows[0], Row::Set("r-1".into()));
    }

    fn ws(id: &str, name: &str, projects: &[&str]) -> WorkspaceInfo {
        WorkspaceInfo {
            machine: ptyro_core::model::split_machine(name).map(|(m, _)| m.to_owned()),
            id: id.into(),
            name: name.into(),
            profile: "default".into(),
            notify: "all".into(),
            projects: projects.iter().map(|p| (*p).to_owned()).collect(),
            created_ms: 0,
        }
    }

    fn project(id: &str) -> ProjectInfo {
        ProjectInfo {
            machine: None,
            id: id.into(),
            slug: id.into(),
            root: "/app".into(),
            default_branch: None,
            remote_url: None,
            pin: None,
            created_ms: 0,
            last_used_ms: None,
        }
    }

    fn chat(id: &str, ws: &str, pinned: bool) -> ptyro_core::chats::ChatInfo {
        ptyro_core::chats::ChatInfo {
            id: id.into(),
            handle: id.into(),
            workspace: ws.into(),
            machine: None,
            title: Some(id.into()),
            title_src: "auto".into(),
            agent: "claude".into(),
            profile: "default".into(),
            tools: "chat".into(),
            pinned,
            archived: false,
            incognito: false,
            forked: false,
            side_of: None,
            leaf: None,
            nodes: 2,
            unread: 0,
            created_ms: 0,
            last_ms: 0,
            cost_micro_usd: 0,
            status: None,
            attention: None,
            engine: None,
            folders: Vec::new(),
            best_effort: false,
            shell: false,
            comparing: None,
            source: None,
            source_dir: None,
        }
    }

    #[test]
    fn chats_under_each_workspace_pinned_then_recent_then_all() {
        let wss = vec![ws("w", "main", &[]), ws("x", "side", &[])];
        let chats =
            vec![chat("pin", "main", true), chat("a", "main", false), chat("b", "main", false)];
        let collapsed = HashSet::new();
        let i = Inputs {
            workspaces: &wss,
            projects: &[],
            worktrees: &[],
            sessions: &[],
            collapsed: &collapsed,
            sets: &[],
            chats: &chats,
            recent: Some(1),
            show: Show::All,
        };
        assert_eq!(
            i.rows(),
            vec![
                Row::Workspace("main".into()),
                Row::Chats("main".into()),
                Row::Chat("pin".into()),
                Row::Chat("a".into()),
                Row::ChatAll("main".into()),
                Row::Workspace("side".into()),
                Row::Chats("side".into()),
                Row::ChatNew("side".into()),
            ],
            "pinned, then the newest `recent`, then `all chats…`; `＋ new chat` where there are none"
        );
        assert_eq!(i.chat_count("main"), 3);
        let mut folded = HashSet::new();
        folded.insert(chats_key("main"));
        let rows = Inputs { collapsed: &folded, ..i }.rows();
        assert!(
            rows.contains(&Row::Chats("main".into())) && !rows.contains(&Row::Chat("a".into()))
        );
        // Off: no section at all.
        let off = Inputs { recent: None, ..i }.rows();
        assert!(!off.iter().any(|r| matches!(r, Row::Chats(_) | Row::ChatNew(_))));
    }

    #[test]
    fn every_workspace_stacked_with_its_tree_and_other_sessions() {
        let wss = vec![ws("w", "main", &["p"]), ws("x", "side", &[])];
        let projects = vec![project("p")];
        let worktrees = vec![wt("t2", "p", false, 5), wt("root", "p", true, 1)];
        let sessions = vec![
            session("in-t2", Some("t2"), Some("p"), Some("main")),
            session("adhoc", None, None, None),
            session("elsewhere", None, None, Some("side")),
            session("in-root", Some("root"), Some("p"), Some("main")),
        ];
        let collapsed = HashSet::new();
        let i = Inputs {
            workspaces: &wss,
            projects: &projects,
            worktrees: &worktrees,
            sessions: &sessions,
            collapsed: &collapsed,
            sets: &[],
            show: Show::All,
            chats: &[],
            recent: None,
        };
        let rows = i.rows();
        assert_eq!(
            rows,
            vec![
                Row::Workspace("main".into()),
                Row::Project("p".into()),
                Row::Worktree("root".into()),
                Row::Session(sessions[3].id),
                Row::Worktree("t2".into()),
                Row::Session(sessions[0].id),
                Row::Group("other sessions".into()),
                Row::Session(sessions[1].id),
                Row::Workspace("side".into()),
                Row::Session(sessions[2].id),
            ],
            "root first, then by creation; every workspace with its own"
        );
        assert_eq!(Inputs::workspace_of(&rows, 3).as_deref(), Some("main"));
        assert_eq!(Inputs::workspace_of(&rows, 9).as_deref(), Some("side"));
        let mut folded = collapsed.clone();
        folded.insert("root".to_owned());
        folded.insert(fold_key("side"));
        let i = Inputs { collapsed: &folded, ..i };
        let rows = i.rows();
        assert!(!rows.contains(&Row::Session(sessions[3].id)));
        assert!(
            rows.contains(&Row::Workspace("side".into()))
                && !rows.contains(&Row::Session(sessions[2].id))
        );
        assert_eq!(
            i.sessions(),
            vec![sessions[3].id, sessions[0].id, sessions[1].id, sessions[2].id],
            "cycling goes through every workspace and ignores folding"
        );
        // A change set comes first, with its lead under it instead of under "other sessions".
        let lead = sessions[2].id;
        let nodes = [crate::sets::SetNode { run: "r-1".into(), lead: Some(lead.full()) }];
        let with = Inputs { sets: &nodes, collapsed: &HashSet::new(), ..i };
        let rows = with.rows();
        assert_eq!(rows[..2], [Row::Set("r-1".into()), Row::Session(lead)]);
        assert_eq!(rows.iter().filter(|r| **r == Row::Session(lead)).count(), 1);
    }

    #[test]
    fn another_machines_workspaces_come_after_ours_and_a_project_shows_once() {
        let wss =
            vec![ws("r", "laptop2:main", &["q"]), ws("w", "main", &["p"]), ws("x", "side", &["p"])];
        let projects = vec![project("p"), project("q")];
        let worktrees = vec![wt("pr", "p", true, 1), wt("qr", "q", true, 1)];
        let mut remote = session("laptop2:fix", Some("qr"), Some("q"), Some("laptop2:main"));
        remote.machine = Some("laptop2".into());
        let mut loose = session("laptop2:loose", None, None, Some("laptop2:gone"));
        loose.machine = Some("laptop2".into());
        let sessions = vec![
            session("here", Some("pr"), Some("p"), Some("side")),
            remote.clone(),
            loose.clone(),
        ];
        let collapsed = HashSet::new();
        let i = Inputs {
            workspaces: &wss,
            projects: &projects,
            worktrees: &worktrees,
            sessions: &sessions,
            collapsed: &collapsed,
            sets: &[],
            show: Show::All,
            chats: &[],
            recent: None,
        };
        let rows = i.rows();
        assert_eq!(
            rows,
            vec![
                Row::Workspace("main".into()),
                Row::Project("p".into()),
                Row::Worktree("pr".into()),
                Row::Session(sessions[0].id),
                Row::Workspace("side".into()),
                Row::Workspace("laptop2:main".into()),
                Row::Project("q".into()),
                Row::Worktree("qr".into()),
                Row::Session(remote.id),
                Row::Group("other sessions".into()),
                Row::Session(loose.id),
            ],
            "ours first; `p` once, under the first workspace with it; a remote session whose \
             workspace isn't shown sits under its machine's main"
        );
        assert_eq!(i.home(&loose), "laptop2:main");
        assert_eq!(machine_and_name("laptop2:main"), (Some("laptop2"), "main"));
    }

    #[test]
    fn a_quick_terminal_is_its_worktree_s_last_line_and_not_cycled_through() {
        let ws = [WorkspaceInfo {
            machine: None,
            id: "w".into(),
            name: "main".into(),
            profile: "default".into(),
            notify: "all".into(),
            projects: vec!["p".into()],
            created_ms: 0,
        }];
        let projects = [ProjectInfo {
            machine: None,
            id: "p".into(),
            slug: "app".into(),
            root: "/app".into(),
            default_branch: None,
            remote_url: None,
            pin: None,
            created_ms: 0,
            last_used_ms: None,
        }];
        let worktrees = [wt("root", "p", true, 1)];
        let quick = |name, wt| SessionInfo { quick: true, ..session(name, wt, Some("p"), None) };
        let sessions = [
            quick("app.shell", Some("root")),
            session("claude", Some("root"), Some("p"), None),
            quick("notes.shell", None),
        ];
        let collapsed = HashSet::new();
        let i = Inputs {
            workspaces: &ws,
            projects: &projects,
            worktrees: &worktrees,
            sessions: &sessions,
            collapsed: &collapsed,
            sets: &[],
            show: Show::All,
            chats: &[],
            recent: None,
        };
        assert_eq!(
            i.rows(),
            vec![
                Row::Workspace("main".into()),
                Row::Project("p".into()),
                Row::Worktree("root".into()),
                Row::Quick(sessions[0].id),
                Row::Session(sessions[1].id),
                Row::Group("other sessions".into()),
                Row::Quick(sessions[2].id),
            ]
        );
        assert_eq!(i.sessions(), vec![sessions[1].id], "j/k and numbers skip quick terminals");
    }

    #[test]
    fn the_filter_keeps_running_and_active_sessions_and_drops_the_headers_left_empty() {
        use ptyro_core::status::{AgentStatus, Confidence, SignalSource, StatusRecord};
        let status = |s: &mut SessionInfo, st| {
            s.kind = ptyro_core::session::SessionKind::Agent;
            s.status = Some(StatusRecord::new(st, Confidence::High, SignalSource::Hook));
        };
        let workspaces = vec![ws("w", "main", &["p", "q"]), ws("w2", "box:main", &["r"])];
        let projects = vec![project("p"), project("q"), project("r")];
        let worktrees = vec![
            wt("p-root", "p", true, 1),
            wt("q-root", "q", true, 1),
            wt("r-root", "r", true, 1),
        ];
        let mut working = session("working", Some("p-root"), Some("p"), Some("main"));
        status(&mut working, AgentStatus::Working);
        let mut waiting = session("waiting", Some("p-root"), Some("p"), Some("main"));
        status(&mut waiting, AgentStatus::Blocked);
        let mut idle = session("idle", Some("q-root"), Some("q"), Some("main"));
        status(&mut idle, AgentStatus::Idle);
        // A shell at its prompt, and one running a command.
        let prompt = session("prompt", Some("q-root"), Some("q"), Some("main"));
        let mut building = session("building", Some("q-root"), Some("q"), Some("main"));
        building.command = Some(ptyro_core::session::ShellCommand {
            text: Some("make".into()),
            started_ms: 1,
            ended_ms: None,
            exit: None,
        });
        let mut quick = session("quick", Some("p-root"), Some("p"), Some("main"));
        quick.quick = true;
        quick.command = building.command.clone();
        let mut done = session("done", Some("r-root"), Some("r"), Some("box:main"));
        status(&mut done, AgentStatus::Working);
        done.state = ptyro_core::session::ProcessState::Exited { code: Some(0), signal: None };
        let sessions = vec![
            working.clone(),
            waiting.clone(),
            idle.clone(),
            prompt.clone(),
            building.clone(),
            quick.clone(),
            done,
        ];
        let collapsed = HashSet::new();
        let inputs = |show| Inputs {
            workspaces: &workspaces,
            projects: &projects,
            worktrees: &worktrees,
            sessions: &sessions,
            collapsed: &collapsed,
            sets: &[],
            chats: &[],
            recent: None,
            show,
        };
        let ids = |show| -> Vec<SessionId> {
            inputs(show).rows().iter().filter_map(Row::session).collect()
        };
        // Everything, as before.
        assert_eq!(inputs(Show::All).rows().len(), 2 + 3 + 3 + 7, "{:?}", inputs(Show::All).rows());
        // Running: alive and real. Not the quick terminal, not the exited one.
        assert_eq!(ids(Show::Running), [working.id, waiting.id, idle.id, prompt.id, building.id]);
        // Active: working, waiting for you, a command running. Not idle, not at a prompt.
        assert_eq!(ids(Show::Active), [working.id, waiting.id, building.id]);
        // The other machine's workspace and project had nothing: no header either.
        let rows = inputs(Show::Active).rows();
        assert!(!rows.contains(&Row::Workspace("box:main".into())), "{rows:?}");
        assert!(!rows.contains(&Row::Project("r".into())), "{rows:?}");
        assert!(
            rows.contains(&Row::Project("p".into())) && rows.contains(&Row::Project("q".into()))
        );
        // Numbering and cycling follow the filter.
        assert_eq!(inputs(Show::Active).sessions(), [working.id, waiting.id, building.id]);
        // A folded project still shows its header, with nothing under it.
        let folded: HashSet<String> = ["p".to_owned()].into();
        let rows = Inputs { collapsed: &folded, ..inputs(Show::Active) }.rows();
        assert!(
            rows.contains(&Row::Project("p".into())) && !rows.contains(&Row::Session(working.id))
        );
    }
}
