//! Machine and profile tabs (TABS, SPEC §11.2k): the strip over the sidebar that picks one
//! machine and one profile. This is the one definition of the rule, like [`Show`]: the TUI's
//! strip, `ptyro ls --machine/--profile` and the web's strip (`web/src/`, which mirrors it in
//! TypeScript and must stay in step) all say what it says.
//!
//! - A tab is a machine and a profile **in use** there: one of the machine's workspaces has the
//!   profile, or one of its sessions runs as it. A defined profile with neither has no tab.
//! - A tab keeps the sessions that **run as** its profile on its machine (the session's own
//!   profile, which a worktree or a project's pin can make differ from its workspace's, SPEC
//!   §10.10).
//! - A tab shows a workspace of its machine when the workspace's own profile is the tab's, or
//!   when a session listed under it runs as the tab's profile. Under it, only the tab's sessions.
//!   Which workspace a session is listed under is the sidebar's business (the TUI's
//!   `Inputs::home`), so [`Scope::keeps_workspace`] is handed those sessions.
//! - `all` comes first and is today's sidebar; it is not a [`Tab`] (`None` where a scope is
//!   kept, `Option<Scope>`). The strip is drawn only with two tabs or more besides it
//!   ([`drawn`]).
//!
//! Another machine's profiles reach this one with its name on (`laptop2:work`, as its sessions'
//! and workspaces' `profile` say, SPEC §16.4b), so [`Scope::profile`] is that name and
//! [`Scope::profile_name`] the one people read (`work`).
//!
//! A chat's engine (SPEC §22.2) is hidden from the session tree, so it neither makes a tab nor
//! counts on one. A quick terminal (UX1.25) goes with its worktree: it never makes a tab nor
//! counts on one (and, in the clients, going to it never switches the tab).
//!
//! [`Show`]: crate::session::Show

use serde::{Deserialize, Serialize};

use crate::model::{DEFAULT_PROFILE, WorkspaceInfo};
use crate::session::SessionInfo;
use crate::status::AgentStatus;

/// One machine and one profile: what a tab picks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scope {
    /// The machine, as [`SessionInfo::machine`] and [`WorkspaceInfo::machine`] name it: `None`
    /// is this machine, `Some("laptop2")` a linked one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// The profile as the wire names it: with the machine on for a linked one (`laptop2:work`).
    pub profile: String,
}

impl Scope {
    /// A profile on a machine, the profile as people name it (`work`) or as the wire does
    /// (`laptop2:work`): `ptyro ls --machine laptop2 --profile work`.
    pub fn new(machine: Option<String>, profile: &str) -> Self {
        let profile = match &machine {
            Some(m) if bare(profile, Some(m)) == profile => format!("{m}:{profile}"),
            _ => profile.to_owned(),
        };
        Self { machine, profile }
    }

    /// The profile as people read it: `work`, without a linked machine's name.
    pub fn profile_name(&self) -> &str {
        bare(&self.profile, self.machine.as_deref())
    }

    /// The tab a session is in: its machine and the profile it runs as. That tab always exists
    /// (the session puts its profile in use), so going to a session outside the current tab
    /// switches to this one.
    pub fn of(s: &SessionInfo) -> Self {
        Self { machine: s.machine.clone(), profile: s.profile.clone() }
    }

    /// The tab a workspace is in by its own profile (it always exists, as with [`Scope::of`]).
    pub fn of_workspace(w: &WorkspaceInfo) -> Self {
        Self { machine: w.machine.clone(), profile: w.profile.clone() }
    }

    /// Whether this tab lists the session: same machine, and it runs as this profile.
    pub fn keeps_session(&self, s: &SessionInfo) -> bool {
        s.machine == self.machine && s.profile == self.profile
    }

    /// Whether this tab shows the workspace: same machine, and its own profile is this one or
    /// one of `under` (the sessions the sidebar lists under it) runs as this profile. So a
    /// workspace can show under two tabs; under each, only [`Scope::keeps_session`]'s sessions.
    pub fn keeps_workspace<'a>(
        &self,
        w: &WorkspaceInfo,
        under: impl IntoIterator<Item = &'a SessionInfo>,
    ) -> bool {
        w.machine == self.machine
            && (w.profile == self.profile || under.into_iter().any(|s| self.keeps_session(s)))
    }
}

/// A profile as people read it: `laptop2:work` on laptop2 is `work`.
pub fn bare<'a>(profile: &'a str, machine: Option<&str>) -> &'a str {
    machine
        .and_then(|m| profile.strip_prefix(m))
        .and_then(|rest| rest.strip_prefix(':'))
        .unwrap_or(profile)
}

/// A tab of the strip, after `all`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub scope: Scope,
    /// `laptop1 [work]`, `[me]`, `laptop2`: the machine's name while there is more than one
    /// machine, `[profile]` while that machine has more than one tab (and alone when neither
    /// applies, though then no strip is drawn).
    pub label: String,
    /// Its running sessions waiting for you (`⚠N`).
    pub blocked: usize,
    /// Its running sessions working (`●N`).
    pub working: usize,
}

/// The tabs in use, in strip order (after `all`): this machine's first, then each other
/// machine's sorted by name (the sidebar's machine order); within a machine `default` first,
/// then the other profiles alphabetically. `here` is this machine's own name (the daemon's
/// `Status.machine`), used in labels only.
pub fn tabs(workspaces: &[WorkspaceInfo], sessions: &[SessionInfo], here: &str) -> Vec<Tab> {
    let mut scopes: Vec<Scope> = workspaces
        .iter()
        .map(Scope::of_workspace)
        .chain(sessions.iter().filter(|s| listed(s) && !s.quick).map(Scope::of))
        .collect();
    let key =
        |s: &Scope| (s.machine.clone(), s.profile_name() != DEFAULT_PROFILE, s.profile.clone());
    scopes.sort_by_key(key);
    scopes.dedup();
    let machines = {
        let mut m: Vec<&Option<String>> = scopes.iter().map(|s| &s.machine).collect();
        m.dedup();
        m.len()
    };
    scopes
        .iter()
        .map(|scope| {
            let mine = scopes.iter().filter(|s| s.machine == scope.machine).count();
            let name = scope.machine.as_deref().unwrap_or(here);
            let label = match (machines > 1, mine > 1) {
                (true, true) => format!("{name} [{}]", scope.profile_name()),
                (true, false) => name.to_owned(),
                (false, _) => format!("[{}]", scope.profile_name()),
            };
            let (blocked, working) = count(sessions.iter().filter(|s| scope.keeps_session(s)));
            Tab { scope: scope.clone(), label, blocked, working }
        })
        .collect()
}

/// Whether the strip is drawn: two tabs or more besides `all`. With one (one machine, one
/// profile) nothing changes from the sidebar without tabs.
pub fn drawn(tabs: &[Tab]) -> bool {
    tabs.len() >= 2
}

/// The tab to show for a saved one: it, while it is still a tab and the strip is drawn; else
/// `None`, which is `all`.
pub fn resolve(saved: Option<&Scope>, tabs: &[Tab]) -> Option<Scope> {
    let saved = saved?;
    (drawn(tabs) && tabs.iter().any(|t| t.scope == *saved)).then(|| saved.clone())
}

/// (blocked, working) among `sessions`: running real sessions only (not quick terminals, not
/// chats' engines). Over every session, it is what `all` carries.
pub fn count<'a>(sessions: impl IntoIterator<Item = &'a SessionInfo>) -> (usize, usize) {
    let mut n = (0, 0);
    for s in sessions.into_iter().filter(|s| listed(s) && s.is_running()) {
        match s.status.as_ref().map(|r| r.status) {
            Some(AgentStatus::Blocked) => n.0 += 1,
            Some(AgentStatus::Working) => n.1 += 1,
            _ => {}
        }
    }
    n
}

/// Shown in the session tree: anything but a chat's engine.
fn listed(s: &SessionInfo) -> bool {
    s.chat.is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{Confidence, SignalSource, StatusRecord};
    use crate::{SessionId, TermSize};

    fn ws(machine: Option<&str>, name: &str, profile: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            id: name.into(),
            machine: machine.map(Into::into),
            name: name.into(),
            profile: profile.into(),
            notify: "all".into(),
            projects: Vec::new(),
            created_ms: 0,
        }
    }

    fn session(machine: Option<&str>, name: &str, profile: &str) -> SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": SessionId::new().full(),
            "machine": machine,
            "handle": "",
            "name": name,
            "kind": "agent",
            "argv": [],
            "cwd": "/",
            "state": {"t": "running"},
            "size": TermSize::new(80, 24),
            "created_ms": 0,
            "profile": profile,
        }))
        .expect("a session")
    }

    fn with(mut s: SessionInfo, status: AgentStatus) -> SessionInfo {
        s.status = Some(StatusRecord::new(status, Confidence::High, SignalSource::Hook));
        s
    }

    fn labels(t: &[Tab]) -> Vec<&str> {
        t.iter().map(|t| t.label.as_str()).collect()
    }

    fn scope(machine: Option<&str>, profile: &str) -> Scope {
        Scope { machine: machine.map(Into::into), profile: profile.into() }
    }

    #[test]
    fn one_machine_one_profile_draws_no_strip() {
        let w = [ws(None, "main", "default")];
        let s = [session(None, "a", "default")];
        let t = tabs(&w, &s, "laptop1");
        assert_eq!(t.len(), 1);
        assert!(!drawn(&t));
        // A saved tab falls back to `all` while there is no strip.
        assert_eq!(resolve(Some(&scope(None, "default")), &t), None);
    }

    #[test]
    fn two_profiles_on_one_machine_name_only_the_profile() {
        let w = [ws(None, "main", "default"), ws(None, "job", "work")];
        let t = tabs(&w, &[], "laptop1");
        assert_eq!(labels(&t), ["[default]", "[work]"]);
        assert!(drawn(&t));
    }

    #[test]
    fn a_linked_machine_names_the_machine_on_every_tab() {
        let w = [
            ws(Some("laptop2"), "laptop2:main", "laptop2:default"),
            ws(None, "main", "work"),
            ws(None, "home", "me"),
        ];
        let t = tabs(&w, &[], "laptop1");
        assert_eq!(labels(&t), ["laptop1 [me]", "laptop1 [work]", "laptop2"]);
        assert_eq!(t[2].scope, scope(Some("laptop2"), "laptop2:default"));
        assert_eq!(t[2].scope.profile_name(), "default");
    }

    #[test]
    fn machines_come_here_first_then_by_name_and_default_leads_its_machine() {
        let w = [
            ws(Some("zed"), "zed:main", "zed:default"),
            ws(Some("alpha"), "alpha:main", "alpha:work"),
            ws(Some("alpha"), "alpha:x", "alpha:default"),
            ws(None, "b", "beta"),
            ws(None, "main", "default"),
            ws(None, "a", "acme"),
        ];
        let t = tabs(&w, &[], "here1");
        assert_eq!(
            labels(&t),
            [
                "here1 [default]",
                "here1 [acme]",
                "here1 [beta]",
                "alpha [default]",
                "alpha [work]",
                "zed"
            ]
        );
    }

    #[test]
    fn a_me_session_in_a_work_workspace_shows_its_workspace_under_both() {
        let job = ws(None, "job", "work");
        let mine = session(None, "side", "me");
        let theirs = session(None, "build", "work");
        let s = [mine.clone(), theirs.clone()];
        let t = tabs(std::slice::from_ref(&job), &s, "laptop1");
        // `me` is in use only through the session.
        assert_eq!(labels(&t), ["[me]", "[work]"]);
        let (me, work) = (&t[0].scope, &t[1].scope);
        assert!(me.keeps_workspace(&job, &s) && work.keeps_workspace(&job, &s));
        assert!(me.keeps_session(&mine) && !me.keeps_session(&theirs));
        assert!(work.keeps_session(&theirs) && !work.keeps_session(&mine));
        // With nothing of `me` under it, `[me]` doesn't show it.
        assert!(!me.keeps_workspace(&job, std::slice::from_ref(&theirs)));
    }

    #[test]
    fn another_machine_never_shows_under_this_ones_tab() {
        let there = ws(Some("laptop2"), "laptop2:main", "laptop2:default");
        let s = session(Some("laptop2"), "laptop2:a", "laptop2:default");
        let here = scope(None, "default");
        assert!(!here.keeps_workspace(&there, [&s]));
        assert!(!here.keeps_session(&s));
        assert_eq!(Scope::of(&s), scope(Some("laptop2"), "laptop2:default"));
        assert_eq!(Scope::of_workspace(&there), Scope::of(&s));
        assert!(Scope::of(&s).keeps_workspace(&there, []));
    }

    #[test]
    fn a_profile_is_named_with_or_without_its_machine() {
        let s = session(Some("laptop2"), "laptop2:a", "laptop2:work");
        assert!(Scope::new(Some("laptop2".into()), "work").keeps_session(&s));
        assert!(Scope::new(Some("laptop2".into()), "laptop2:work").keeps_session(&s));
        assert!(!Scope::new(None, "work").keeps_session(&s));
        assert_eq!(Scope::new(None, "work"), scope(None, "work"));
        assert_eq!(bare("laptop2:work", Some("laptop2")), "work");
        assert_eq!(bare("work", None), "work");
    }

    #[test]
    fn counts_are_running_real_sessions_blocked_and_working() {
        let w = [ws(None, "main", "default"), ws(None, "job", "work")];
        let mut quick = with(session(None, "q", "work"), AgentStatus::Blocked);
        quick.quick = true;
        let mut chat = with(session(None, "c", "work"), AgentStatus::Working);
        chat.chat = Some("c1".into());
        let mut gone = with(session(None, "g", "work"), AgentStatus::Blocked);
        gone.state = crate::session::ProcessState::Exited { code: Some(0), signal: None };
        let s = [
            with(session(None, "b1", "work"), AgentStatus::Blocked),
            with(session(None, "b2", "work"), AgentStatus::Blocked),
            with(session(None, "w1", "work"), AgentStatus::Working),
            with(session(None, "w2", "default"), AgentStatus::Working),
            with(session(None, "i", "default"), AgentStatus::Idle),
            quick,
            chat,
            gone,
        ];
        let t = tabs(&w, &s, "laptop1");
        assert_eq!((t[0].blocked, t[0].working), (0, 1));
        assert_eq!((t[1].blocked, t[1].working), (2, 1));
        assert_eq!(count(&s), (2, 2), "what `all` carries");
    }

    #[test]
    fn a_profile_in_use_only_by_a_session_has_a_tab_and_a_chat_engine_makes_none() {
        let w = [ws(None, "main", "default")];
        let mut engine = session(None, "c", "ghost");
        engine.chat = Some("c1".into());
        let t = tabs(&w, &[engine], "laptop1");
        assert_eq!(labels(&t), ["[default]"]);
        let t = tabs(&w, &[session(None, "x", "solo")], "laptop1");
        assert_eq!(labels(&t), ["[default]", "[solo]"]);
    }

    #[test]
    fn a_quick_terminal_makes_no_tab() {
        let w = [ws(None, "main", "default")];
        let mut quick = session(None, "q", "ghost");
        quick.quick = true;
        let t = tabs(&w, std::slice::from_ref(&quick), "laptop1");
        assert_eq!(labels(&t), ["[default]"]);
        assert!(!drawn(&t));
    }

    #[test]
    fn a_saved_tab_that_is_gone_falls_back_to_all() {
        let w = [ws(None, "main", "default"), ws(None, "job", "work")];
        let t = tabs(&w, &[], "laptop1");
        assert_eq!(resolve(Some(&scope(None, "work")), &t), Some(scope(None, "work")));
        assert_eq!(resolve(Some(&scope(None, "gone")), &t), None);
        assert_eq!(resolve(None, &t), None);
    }

    #[test]
    fn a_scope_round_trips_and_this_machine_is_left_out() {
        let here = scope(None, "work");
        let text = serde_json::to_string(&here).unwrap();
        assert_eq!(text, r#"{"profile":"work"}"#);
        assert_eq!(serde_json::from_str::<Scope>(&text).unwrap(), here);
        let there = scope(Some("laptop2"), "me");
        let back: Scope = serde_json::from_str(&serde_json::to_string(&there).unwrap()).unwrap();
        assert_eq!(back, there);
    }
}
