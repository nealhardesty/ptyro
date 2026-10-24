// What the daemon knows beyond the sessions: projects, worktrees, workspaces and the agents,
// loaded when the connection opens and kept current by its events (SPEC §11).
import { conn } from "./conn.js";
import { ui } from "./kit.js";
export const data = {
    projects: new Map(),
    worktrees: new Map(),
    workspaces: new Map(),
    agents: [],
    defaultAgent: "claude",
};
const byId = (list) => new Map(list.map((x) => [x.id, x]));
/// Everything, again (a fresh connection, or after something the events don't cover).
export async function loadAll() {
    const [p, w, ws, ag] = await Promise.allSettled([
        conn.request({ t: "project_list" }),
        conn.request({ t: "worktree_list" }),
        conn.request({ t: "workspace_list" }),
        conn.request({ t: "agents" }),
    ]);
    if (p.status === "fulfilled")
        data.projects = byId(p.value.projects);
    if (w.status === "fulfilled")
        data.worktrees = byId(w.value.worktrees);
    if (ws.status === "fulfilled")
        data.workspaces = byId(ws.value.workspaces);
    if (ag.status === "fulfilled") {
        data.agents = ag.value.agents;
        data.defaultAgent = ag.value.default;
    }
    ui.render();
}
/// Keep up with a domain event; true when it was one of ours.
export function onEvent(e) {
    switch (e.t) {
        case "project_updated":
        case "project_auto_added":
            data.projects.set(e.info.id, e.info);
            break;
        case "project_removed":
            data.projects.delete(e.id);
            break;
        case "workspace_updated":
            data.workspaces.set(e.info.id, e.info);
            break;
        case "workspace_removed":
            data.workspaces.delete(e.id);
            break;
        case "worktree_updated":
            data.worktrees.set(e.info.id, e.info);
            break;
        case "worktree_removed":
            data.worktrees.delete(e.id);
            break;
        default: return false;
    }
    return true;
}
export const projectOf = (w) => data.projects.get(w.project);
/// A worktree by id, name, `project/name` or the end of its id (how addresses read).
export function findWorktree(addr) {
    const all = [...data.worktrees.values()];
    return data.worktrees.get(addr) ?? all.find((w) => `${w.project_slug}/${w.name}` === addr) ?? all.find((w) => w.name === addr) ?? all.find((w) => w.id.endsWith(addr));
}
export const liveWorktrees = () => [...data.worktrees.values()].filter((w) => w.state !== "removed");
/// Does this device's scope allow `need`? (The daemon decides; this only hides what would fail.)
const RANK = { read: 0, approve: 1, control: 2, full: 3 };
export const can = (need) => (RANK[conn.scope] ?? 3) >= RANK[need];
