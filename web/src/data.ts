// What the daemon knows beyond the sessions: projects, worktrees, workspaces and the agents,
// loaded when the connection opens and kept current by its events (SPEC §11).

import { conn } from "./conn.js";
import { ui } from "./kit.js";

export interface Project { id: string; slug: string; root: string; default_branch?: string | null; remote_url?: string | null; pin?: string | null; created_ms: number; last_used_ms?: number | null }
export interface RepoStatus { branch?: string | null; head?: string | null; ahead?: number; behind?: number; staged?: number; unstaged?: number; untracked?: number; conflicted?: number; insertions?: number; deletions?: number; upstream?: string | null; unpushed?: number | null; pushed?: boolean; [k: string]: any }
export interface PrInfo { number: number; state: string; url: string; title?: string; draft?: boolean; checks?: any[]; review?: string; [k: string]: any }
export interface Worktree {
  id: string; project: string; project_slug: string; name: string; path: string; branch?: string | null; base?: string | null;
  is_root: boolean; profile?: string | null; state: string; ports?: any; created_ms: number; last_used_ms?: number | null;
  status?: RepoStatus | null; locked: boolean; pr?: PrInfo | null; title?: string | null; integrated?: string | null;
}
export interface Workspace { id: string; name: string; profile: string; notify: string; projects: string[]; created_ms: number; machine?: string | null }
/// Another machine this daemon mirrors (MM1.B): its workspaces show here as `laptop2:work`.
export interface Machine {
  name: string; transport: { t: "ssh"; target: string } | { t: "token"; url: string; fingerprint?: string | null };
  state: "connecting" | "online" | "offline" | "mismatch"; since_ms?: number | null; version?: string | null;
  workspaces: string[]; hidden: string[]; error?: string | null; fix?: string | null; added_ms: number;
}
export interface Agent { name: string; program: string; installed: boolean; found?: string }

export const data = {
  projects: new Map<string, Project>(),
  worktrees: new Map<string, Worktree>(),
  workspaces: new Map<string, Workspace>(),
  agents: [] as Agent[],
  defaultAgent: "claude",
  machines: new Map<string, Machine>(),
  /// What each workspace last started (UX3.2: quick new); none: the default agent.
  lastLaunch: new Map<string, LastLaunch | null>(),
};

export interface LastLaunch { agent?: string | null; profile?: string | null; argv?: string[] }

/// `laptop2:work` → ["laptop2", "work"]; this machine's `work` → [null, "work"].
export function splitMachine(name: string): [string | null, string] {
  const i = name.indexOf(":");
  if (i > 0 && data.machines.has(name.slice(0, i))) return [name.slice(0, i), name.slice(i + 1)];
  return [null, name];
}
/// `laptop2 / work`, or `work` for this machine's.
export const wsLabel = (name: string): string => { const [m, w] = splitMachine(name); return m ? `${m} / ${w}` : w; };
/// The machine a name belongs to, when it is another one (and how its link stands).
export const machineOf = (name: string | null | undefined): Machine | undefined => name ? data.machines.get(splitMachine(name)[0] ?? "") : undefined;

/// The workspace a worktree's sessions belong to: its project's first, else `main`.
export const workspaceOf = (w: Worktree): string => [...data.workspaces.values()].find((x) => x.projects.includes(w.project))?.name ?? "main";

/// What quick new starts in `ws`, for people: `claude`, `claude@work`, `shell`, `htop`.
export function againLabel(ws: string): string {
  const l = data.lastLaunch.get(ws);
  if (!l) return data.defaultAgent;
  if (l.agent) return l.profile && !l.agent.includes("@") ? `${l.agent}@${l.profile}` : l.agent;
  const p = l.argv?.[0];
  return p ? p.split("/").pop() ?? p : "shell";
}

/// Ask the daemon what `ws` last started.
export async function refreshLaunch(ws: string): Promise<void> {
  try { data.lastLaunch.set(ws, (await conn.request({ t: "last_launch", workspace: ws })).launch ?? null); } catch { /* the default agent */ }
}

const byId = <T extends { id: string }>(list: T[]): Map<string, T> => new Map(list.map((x) => [x.id, x]));

/// Everything, again (a fresh connection, or after something the events don't cover).
export async function loadAll(): Promise<void> {
  const [p, w, ws, ag, mm] = await Promise.allSettled([
    conn.request({ t: "project_list" }),
    conn.request({ t: "worktree_list" }),
    conn.request({ t: "workspace_list" }),
    conn.request({ t: "agents" }),
    conn.request({ t: "machine_list" }),
  ]);
  if (mm.status === "fulfilled") data.machines = new Map((mm.value.machines as Machine[]).map((m) => [m.name, m]));
  if (p.status === "fulfilled") data.projects = byId<Project>(p.value.projects);
  if (w.status === "fulfilled") data.worktrees = byId<Worktree>(w.value.worktrees);
  if (ws.status === "fulfilled") data.workspaces = byId<Workspace>(ws.value.workspaces);
  if (ag.status === "fulfilled") { data.agents = ag.value.agents; data.defaultAgent = ag.value.default; }
  await Promise.allSettled([...data.workspaces.values()].map((w) => refreshLaunch(w.name)));
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
    case "machine_updated": data.machines.set(e.info.name, e.info); break;
    case "machine_removed": data.machines.delete(e.name); break;
    default: return false;
  }
  return true;
}

// ── About (UX2.1) ───────────────────────────────────────────────────────────────────────────

/// Where ptyro lives; a test keeps these equal to Cargo.toml's `repository` and `license`.
export const REPOSITORY = "https://github.com/nealhardesty/ptyro";
export const ISSUES = `${REPOSITORY}/issues`;
export const LICENSE = "MIT";

/// The About document (`ptyro about --json`, the `about` host operation), or what the daemon's
/// status alone says when this device may not ask the machine (anything below `full`).
export function aboutDoc(host: any | null | undefined, status: any): any {
  if (host) return host;
  const d = status ?? {};
  return {
    version: d.version ?? "", protocol: d.proto ? `${d.proto.major}.${d.proto.minor}` : "",
    repository: REPOSITORY, issues: ISSUES, license: LICENSE, build: {}, system: "",
    places: [d.socket && { name: "socket", path: d.socket, exists: true }, d.log_dir && { name: "logs", path: d.log_dir, exists: true }].filter(Boolean),
    daemon: { running: !!status, version: d.version, protocol: d.proto ? `${d.proto.major}.${d.proto.minor}` : "", pid: d.pid, started_ms: d.started_ms, same: null, fix: null },
    web: d.web ? { wanted: d.web.wanted, up: d.web.up, url: d.web.url, text: [d.web.up ? `up at ${d.web.url}` : d.web.wanted ? "on, but not up yet" : "off"] } : null,
    tools: [], limited: true,
  };
}

/// What to paste into a bug report: the machine's own `report` when there is one, else the facts
/// this device can see.
export function aboutText(doc: any): string {
  if (doc.report) return doc.report;
  const v = (k: string, s: string): string => `${k.padEnd(12)} ${s}`;
  const d = doc.daemon ?? {};
  return [
    v("ptyro", `${doc.version} (protocol ${doc.protocol})`),
    v("source", doc.repository), v("licence", doc.license), v("report", `${doc.issues} (paste this block)`),
    v("daemon", d.running ? `running, version ${d.version} (protocol ${d.protocol}), pid ${d.pid}` : "not running"),
    ...(doc.web ? [v("web app", doc.web.text?.[0] ?? "")] : []),
    ...(doc.places ?? []).map((p: any) => v(p.name, p.path)),
  ].join("\n") + "\n";
}

/// What is left to do with a worktree's work (UX2.2), the same rule as the TUI and the CLI
/// (`RepoStatus::left` in ptyro-core): uncommitted files, unpushed commits (ahead of the upstream,
/// or on no remote), whether there is no upstream yet, and whether it is all committed and pushed.
export interface Left { uncommitted: number; unpushed: number; notPushed: boolean; pushed: boolean }
export function repoLeft(st?: RepoStatus | null): Left {
  if (!st) return { uncommitted: 0, unpushed: 0, notPushed: false, pushed: false };
  const uncommitted = (st.staged ?? 0) + (st.modified ?? 0) + (st.untracked ?? 0) + (st.conflicts ?? 0);
  const unpushed = st.upstream ? st.ahead ?? 0 : st.unpushed ?? 0;
  return { uncommitted, unpushed, notPushed: !st.upstream && unpushed > 0, pushed: !!st.pushed };
}
/// `✓ pushed`, `↑2`, `● 3`, `↑2 ● 3`, `not pushed ↑2`; empty when there is nothing to say.
export function leftWords(l: Left): string {
  const v: string[] = [];
  if (l.unpushed > 0) v.push(l.notPushed ? `not pushed ↑${l.unpushed}` : `↑${l.unpushed}`);
  if (l.uncommitted > 0) v.push(`● ${l.uncommitted}`);
  if (l.pushed) v.push("✓ pushed");
  return v.join(" ");
}
/// Its work has shipped (`WorktreeInfo::shipped`): the PR merged, or integrated and unchanged since.
export function shipped(w: Worktree): boolean {
  if (w.is_root) return false;
  if (w.pr?.state === "MERGED") return true;
  const st = w.status;
  return !!(w.integrated && st && repoLeft(st).uncommitted === 0 && st.head && w.integrated.startsWith(st.head));
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

/// Tabs of a worktree's page, added by the modules that own them (review, checkpoints…).
export const wtTabs: { id: string; label: string; render: (w: Worktree) => HTMLElement }[] = [];
