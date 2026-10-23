//! The sidebar tree (SPEC §11.1, M3.12): workspace → project → worktree → sessions, then the
//! workspace's other sessions. Pure: built from what the daemon reported, so it is unit-tested.

use std::collections::HashSet;

use ptyro_core::SessionId;
use ptyro_core::model::{MAIN_WORKSPACE, ProjectInfo, WorkspaceInfo, WorktreeInfo, WorktreeState};
use ptyro_core::session::SessionInfo;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    Project(String),
    Worktree(String),
    Session(SessionId),
    /// A non-selectable label (`other sessions`).
    Group(String),
    /// A change set (its run id), above its lead (M11.11).
    Set(String),
}

impl Row {
    pub fn selectable(&self) -> bool {
        !matches!(self, Self::Group(_))
    }
}

pub struct Inputs<'a> {
    pub workspace: &'a str,
    pub workspaces: &'a [WorkspaceInfo],
    pub projects: &'a [ProjectInfo],
    pub worktrees: &'a [WorktreeInfo],
    pub sessions: &'a [SessionInfo],
    /// Collapsed project and worktree ids.
    pub collapsed: &'a HashSet<String>,
    /// Change sets still going (M11): shown first, each with its lead.
    pub sets: &'a [crate::sets::SetNode],
}

impl Inputs<'_> {
    fn ws(&self) -> Option<&WorkspaceInfo> {
        self.workspaces.iter().find(|w| w.name == self.workspace)
    }

    /// Project ids of the workspace, in order.
    pub fn project_ids(&self) -> Vec<String> {
        self.ws()
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

    /// Sessions that belong in this workspace's "other sessions": their home is this workspace
    /// (or none, for `main`) and they aren't shown under one of its projects.
    fn others(&self, shown_projects: &[String]) -> Vec<&SessionInfo> {
        self.sessions
            .iter()
            .filter(|s| {
                let under_project = s.project.as_ref().is_some_and(|p| shown_projects.contains(p))
                    && s.worktree
                        .as_ref()
                        .is_some_and(|w| self.worktrees.iter().any(|x| x.id == *w));
                let home = s.workspace.as_deref().unwrap_or(MAIN_WORKSPACE);
                let under_set =
                    self.sets.iter().any(|n| n.lead.as_deref() == Some(s.id.full().as_str()));
                !under_project
                    && !under_set
                    && (home == self.workspace || self.ws().is_none() && home == MAIN_WORKSPACE)
            })
            .collect()
    }

    /// The rows, honouring collapsed nodes.
    pub fn rows(&self) -> Vec<Row> {
        self.build(true)
    }

    /// Every session of the workspace in tree order, collapsed or not (cycling, numbering).
    pub fn sessions(&self) -> Vec<SessionId> {
        self.build(false)
            .into_iter()
            .filter_map(|r| match r {
                Row::Session(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    fn build(&self, honour_collapse: bool) -> Vec<Row> {
        let mut rows = Vec::new();
        for n in self.sets {
            rows.push(Row::Set(n.run.clone()));
            if !(honour_collapse && self.collapsed.contains(&n.run))
                && let Some(l) = &n.lead
                && let Some(s) = self.sessions.iter().find(|s| s.id.full() == *l)
            {
                rows.push(Row::Session(s.id));
            }
        }
        let projects = self.project_ids();
        for pid in &projects {
            rows.push(Row::Project(pid.clone()));
            if honour_collapse && self.collapsed.contains(pid) {
                continue;
            }
            for w in self.worktrees_of(pid) {
                rows.push(Row::Worktree(w.id.clone()));
                if honour_collapse && self.collapsed.contains(&w.id) {
                    continue;
                }
                for s in
                    self.sessions.iter().filter(|s| s.worktree.as_deref() == Some(w.id.as_str()))
                {
                    rows.push(Row::Session(s.id));
                }
            }
        }
        let others = self.others(&projects);
        if !others.is_empty() {
            if !projects.is_empty() {
                rows.push(Row::Group("other sessions".into()));
            }
            rows.extend(others.into_iter().map(|s| Row::Session(s.id)));
        }
        rows
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
        }
    }

    fn wt(id: &str, project: &str, root: bool, created: u64) -> WorktreeInfo {
        WorktreeInfo {
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
        }
    }

    #[test]
    fn tree_order_collapse_and_other_sessions() {
        let ws = vec![
            WorkspaceInfo {
                id: "w".into(),
                name: "main".into(),
                profile: "default".into(),
                notify: "all".into(),
                projects: vec!["p".into()],
                created_ms: 0,
            },
            WorkspaceInfo {
                id: "x".into(),
                name: "side".into(),
                profile: "default".into(),
                notify: "all".into(),
                projects: vec![],
                created_ms: 0,
            },
        ];
        let projects = vec![ProjectInfo {
            id: "p".into(),
            slug: "app".into(),
            root: "/app".into(),
            default_branch: None,
            remote_url: None,
            pin: None,
            created_ms: 0,
            last_used_ms: None,
        }];
        let worktrees = vec![wt("t2", "p", false, 5), wt("root", "p", true, 1)];
        let sessions = vec![
            session("in-t2", Some("t2"), Some("p"), Some("main")),
            session("adhoc", None, None, None),
            session("elsewhere", None, None, Some("side")),
            session("in-root", Some("root"), Some("p"), Some("main")),
        ];
        let mut collapsed = HashSet::new();
        let rows = Inputs {
            workspace: "main",
            workspaces: &ws,
            projects: &projects,
            worktrees: &worktrees,
            sessions: &sessions,
            collapsed: &collapsed,
            sets: &[],
        }
        .rows();
        assert_eq!(
            rows,
            vec![
                Row::Project("p".into()),
                Row::Worktree("root".into()),
                Row::Session(sessions[3].id),
                Row::Worktree("t2".into()),
                Row::Session(sessions[0].id),
                Row::Group("other sessions".into()),
                Row::Session(sessions[1].id),
            ],
            "root first, then by creation; other workspaces' sessions hidden"
        );
        collapsed.insert("root".to_owned());
        let i = Inputs {
            workspace: "main",
            workspaces: &ws,
            projects: &projects,
            worktrees: &worktrees,
            sessions: &sessions,
            collapsed: &collapsed,
            sets: &[],
        };
        assert!(!i.rows().contains(&Row::Session(sessions[3].id)));
        assert_eq!(
            i.sessions(),
            vec![sessions[3].id, sessions[0].id, sessions[1].id],
            "cycling ignores collapse"
        );
        let side = Inputs { workspace: "side", ..i };
        assert_eq!(side.rows(), vec![Row::Session(sessions[2].id)]);
        // A change set comes first, with its lead under it instead of under "other sessions".
        let lead = sessions[2].id;
        let nodes = [crate::sets::SetNode { run: "r-1".into(), lead: Some(lead.full()) }];
        let with = Inputs { workspace: "side", sets: &nodes, ..i };
        assert_eq!(with.rows(), vec![Row::Set("r-1".into()), Row::Session(lead)]);
    }
}
