// What the daemon knows beyond the sessions: projects, worktrees, workspaces and the agents,
// loaded when the connection opens and kept current by its events (SPEC §11).

import { conn } from "./conn.js";
import { ui } from "./kit.js";

export interface Project { id: string; slug: string; root: string; default_branch?: string | null; remote_url?: string | null; pin?: string | null; created_ms: number; last_used_ms?: number | null }
export interface RepoStatus { branch?: string | null; head?: string | null; ahead?: number; behind?: number; staged?: number; unstaged?: number; untracked?: number; conflicted?: number; insertions?: number; deletions?: number; [k: string]: any }
export interface PrInfo { number: number; state: string; url: string; title?: string; draft?: boolean; checks?: string; review?: string; [k: string]: any }
export interface Worktree {
  id: string; project: string; project_slug: string; name: string; path: string; branch?: string | null; base?: string | null;
  is_root: boolean; profile?: string | null; state: string; ports?: any; created_ms: number; last_used_ms?: number | null;
  status?: RepoStatus | null; locked: boolean; pr?: PrInfo | null; title?: string | null; integrated?: string | null;
}
export interface Workspace { id: string; name: string; profile: string; notify: string; projects: string[]; created_ms: number }
export interface Agent { name: string; program: string; installed: boolean }

export const data = {
  projects: new Map<string, Project>(),
  worktrees: new Map<string, Worktree>(),
  workspaces: new Map<string, Workspace>(),
  agents: [] as Agent[],
  defaultAgent: "claude",
};

const byId = <T extends { id: string }>(list: T[]): Map<string, T> => new Map(list.map((x) => [x.id, x]));

/// Everything, again (a fresh connection, or after something the events don't cover).
export async function loadAll(): Promise<void> {
  const [p, w, ws, ag] = await Promise.allSettled([
    conn.request({ t: "project_list" }),
    conn.request({ t: "worktree_list" }),
    conn.request({ t: "workspace_list" }),
    conn.request({ t: "agents" }),
  ]);
  if (p.status === "fulfilled") data.projects = byId<Project>(p.value.projects);
  if (w.status === "fulfilled") data.worktrees = byId<Worktree>(w.value.worktrees);
  if (ws.status === "fulfilled") data.workspaces = byId<Workspace>(ws.value.workspaces);
  if (ag.status === "fulfilled") { data.agents = ag.value.agents; data.defaultAgent = ag.value.default; }
  ui.render();
}

/// Keep up with a domain event; true when it was one of ours.
export function onEvent(e: any): boolean {
  switch (e.t) {
    case "project_updated": case "project_auto_added": data.projects.set(e.info.id, e.info); break;
    case "project_removed": data.projects.delete(e.id); break;
    case "workspace_updated": data.workspaces.set(e.info.id, e.info); break;
    case "workspace_removed": data.workspaces.delete(e.id); break;
    case "worktree_updated": data.worktrees.set(e.info.id, e.info); break;
    case "worktree_removed": data.worktrees.delete(e.id); break;
    default: return false;
  }
  return true;
}

export const projectOf = (w: Worktree): Project | undefined => data.projects.get(w.project);
/// A worktree by id, name, `project/name` or the end of its id (how addresses read).
export function findWorktree(addr: string): Worktree | undefined {
  const all = [...data.worktrees.values()];
  return data.worktrees.get(addr) ?? all.find((w) => `${w.project_slug}/${w.name}` === addr) ?? all.find((w) => w.name === addr) ?? all.find((w) => w.id.endsWith(addr));
}
export const liveWorktrees = (): Worktree[] => [...data.worktrees.values()].filter((w) => w.state !== "removed");

/// Does this device's scope allow `need`? (The daemon decides; this only hides what would fail.)
const RANK: Record<string, number> = { read: 0, approve: 1, control: 2, full: 3 };
export const can = (need: "approve" | "control" | "full"): boolean => (RANK[conn.scope] ?? 3) >= RANK[need];
