// Projects, worktrees and workspaces (ROADMAP WEB1.3): the sidebar's tree as the Projects tab,
// a worktree's page, and everything the TUI does to them: new, finish, archive, restore, remove,
// scripts, ports, pair mode, locks, leases, the collision radar, checkpoints and the clean-up.

import { conn } from "./conn.js";
import { data, liveWorktrees, wtTabs } from "./data.js";
import type { Project, Worktree, Workspace } from "./data.js";
import { actions, ago, ask, badge, bar, Cache, confirm, enc, errText, facts, form, go, h, menu, pending, row, safeDecode, section, sheet, textSheet, ui, views, webHref } from "./kit.js";
import type { Item } from "./kit.js";
import { newTask, openQuickShell, allSessions } from "./sessions.js";
import type { Session } from "./screen.js";

const ACTIONS = {
  "projects": "Projects and their worktrees, with what each is doing",
  "project.add": "Add a repository: pick from the ones found on this computer, or give its path",
  "project.show": "A project's page: its branches, scripts, configuration and pin",
  "project.remove": "Forget a project (its worktrees stay on disk)",
  "project.trust": "Trust the project's scripts as they are now, so they run",
  "project.untrust": "Stop trusting the project's scripts",
  "project.config": "The project's configuration, and where each value comes from",
  "project.pin": "Pin the project to a profile (its account)",
  "project.suggest": "What a first worktree could use: a setup script, files to copy",
  "worktree.new": "A new worktree in this project, with an agent in it if you like",
  "worktree.show": "A worktree's page: status, sessions, changes and what to do next",
  "worktree.remove": "Remove the worktree (it refuses to lose work; archive keeps it)",
  "worktree.status": "Where the worktree stands: changes, commits ahead, its pull request",
  "worktree.pair": "Pair mode: show the worktree's state in the project's main checkout",
  "worktree.unpair": "Put the main checkout back, carrying edits made there into the worktree",
  "worktree.run": "Run one of the project's scripts in this worktree",
  "worktree.open": "Open the worktree in an editor",
  "worktree.lock": "Keep the worktree from being removed or cleaned up",
  "worktree.unlock": "Let the worktree be removed again",
  "worktree.rename_branch": "Rename the worktree's branch (until it's pushed)",
  "worktree.retitle": "Change what the worktree is titled",
  "worktree.finish": "Finish: commit what's left, then open a PR or merge into the main branch",
  "worktree.archive": "Archive: keep its work and its branch, remove the worktree",
  "worktree.restore": "Bring an archived worktree back as it was",
  "worktree.gc": "Stale worktrees: merged, abandoned or missing; archive or remove them",
  "workspace.switch": "Show one workspace's projects, or all of them",
  "workspace.new": "A new workspace",
  "workspace.set": "A workspace's profile and how it notifies",
  "workspace.remove": "Remove a workspace",
  "workspace.rename": "Rename a workspace",
  "workspace.member": "Add a project to a workspace, or take it out",
  "leases": "File leases: which agent holds which files",
  "radar": "Predicted collisions between a project's live worktrees",
  "checkpoint.list": "Checkpoints: the worktree as it was at the end of each agent turn",
  "checkpoint.diff": "What changed since a checkpoint",
  "checkpoint.restore": "Put the worktree back to a checkpoint (the state now is checkpointed first)",
} as const;
const tip = actions(ACTIONS);

// ── what the lists show ─────────────────────────────────────────────────────────────────────

const STATUS_GLYPH: Record<string, string> = { working: "●", blocked: "⚠", idle: "○", done: "✓", errored: "✗", starting: "◌" };
const statusOf = (s: Session): string => (s.state.t !== "running" ? s.state.t : s.status?.status ?? (s.kind === "agent" ? "unknown" : "shell"));

/// The sessions running in a worktree, but not its quick shell.
const sessionsIn = (w: Worktree): Session[] => [...allSessions().values()].filter((s) => s.worktree === w.id && !s.quick);

/// `+12 −3 · 2 ahead · 4 dirty`, short.
function summary(w: Worktree): string {
  const st = w.status;
  if (!st) return w.state === "ready" ? "" : w.state;
  const parts: string[] = [];
  if (st.insertions || st.deletions) parts.push(`+${st.insertions} −${st.deletions}`);
  if (st.base_ahead) parts.push(`${st.base_ahead} ahead`);
  if (st.behind) parts.push(`${st.behind} behind`);
  const dirty = (st.staged ?? 0) + (st.modified ?? 0) + (st.untracked ?? 0);
  if (dirty) parts.push(`${dirty} dirty`);
  if (st.conflicts) parts.push(`${st.conflicts} conflicts`);
  return parts.join(" · ") || "clean";
}

export function prBadge(w: Worktree): HTMLElement | null {
  const pr = w.pr;
  if (!pr) return null;
  const checks: any[] = pr.checks ?? [];
  const failed = checks.filter((c) => ["fail", "cancel"].includes(c.bucket)).length;
  const pending = checks.filter((c) => c.bucket === "pending").length;
  const kind = pr.state === "MERGED" ? "info" : failed ? "bad" : pending ? "warn" : checks.length ? "ok" : "";
  return badge(`#${pr.number} ${pr.state === "OPEN" ? (pr.draft ? "draft" : "open") : pr.state.toLowerCase()}${failed ? ` · ${failed} failing` : ""}`, kind);
}

function worktreeRow(w: Worktree): HTMLElement {
  const sess = sessionsIn(w);
  const worst = ["blocked", "errored", "working", "starting", "done", "idle"].find((k) => sess.some((s) => statusOf(s) === k));
  const glyph = worst ? h("span", { class: `glyph ${worst}` }, STATUS_GLYPH[worst]) : null;
  return row(
    h("span", {}, glyph, glyph ? " " : "", w.is_root ? "main checkout" : w.title || w.name, w.locked ? " 🔒" : "", " ", prBadge(w)),
    [w.is_root ? w.branch ?? "" : w.title ? w.name : "", summary(w), sess.length ? `${sess.length} session${sess.length === 1 ? "" : "s"}` : ""].filter(Boolean).join(" · "),
    w.last_used_ms ? ago(w.last_used_ms) : "",
    { href: `#/wt/${enc(w.id)}`, cls: w.state !== "ready" ? "dim" : "" });
}

// ── the Projects tab ────────────────────────────────────────────────────────────────────────

let wsFilter = "";

function projectsView(): HTMLElement {
  const wss = [...data.workspaces.values()];
  const inWs = (p: Project) => !wsFilter || data.workspaces.get([...data.workspaces.keys()].find((k) => data.workspaces.get(k)!.name === wsFilter)!)?.projects.includes(p.id);
  const projects = [...data.projects.values()].filter(inWs).sort((a, b) => (b.last_used_ms ?? 0) - (a.last_used_ms ?? 0));
  const chips = wss.length > 1 ? h("div", { class: "chips", title: tip("workspace.switch") },
    h("button", { class: wsFilter ? "" : "on", onclick: () => { wsFilter = ""; ui.render(); } }, "All"),
    ...wss.map((w) => h("button", { class: wsFilter === w.name ? "on" : "", onclick: () => { wsFilter = w.name; ui.render(); } }, w.name))) : null;
  const body = projects.length ? projects.map((p) => {
    const wts = liveWorktrees().filter((w) => w.project === p.id).sort((a, b) => Number(b.is_root) - Number(a.is_root) || (b.last_used_ms ?? 0) - (a.last_used_ms ?? 0));
    return h("section", { class: "proj" },
      h("div", { class: "proj-head" },
        h("a", { class: "proj-name", href: `#/proj/${enc(p.id)}`, title: tip("project.show") }, p.slug),
        p.pin ? badge(p.pin, "info") : null,
        h("span", { class: "spacer" }),
        h("button", { class: "small", title: tip("worktree.new"), onclick: () => newTask(`p:${p.id}`) }, "＋ Task")),
      h("div", { class: "rows" }, ...wts.map(worktreeRow)));
  }) : [h("div", { class: "empty" }, h("p", {}, wsFilter ? `No projects in ${wsFilter}.` : "No projects yet."), h("button", { class: "primary", onclick: addProject }, "Add a project"))];
  return h("div", {}, ui.header("Projects", false,
    h("button", { class: "icon", title: tip("worktree.gc"), "aria-label": "Clean up", onclick: () => go("#/gc") }, "🧹"),
    h("button", { class: "icon plus", title: tip("project.add"), "aria-label": "Add a project", onclick: addProject }, "＋")),
    chips, h("main", { class: "list", title: tip("projects") }, ...body,
      h("div", { class: "foot-links" },
        h("a", { href: "#/archives", title: tip("worktree.restore") }, "Archived"),
        h("a", { href: "#/ws", title: tip("workspace.set") }, "Workspaces"),
        h("a", { href: "#/leases", title: tip("leases") }, "Leases"),
        h("a", { href: "#/radar", title: tip("radar") }, "Radar"))));
}

// ── projects ────────────────────────────────────────────────────────────────────────────────

function addProject(): void {
  const list = h("div", { class: "rows" }, h("p", { class: "empty" }, "Looking for repositories…"));
  const path = h("input", { placeholder: "/path/to/a/repository", autocapitalize: "off", spellcheck: "false" });
  const add = async (p: string) => {
    const r = await ask({ t: "project_add", path: p, workspace: wsFilter || null });
    if (r) { s.close(); for (const n of r.notes ?? []) ui.toast(n); go(`#/proj/${enc(r.info.id)}`); }
  };
  const s = sheet("Add a project", [
    h("p", { class: "intro" }, "Repositories found on this computer:"), list,
    h("label", { class: "field" }, h("span", {}, "Or a path"), path),
  ], [h("button", { onclick: () => s.close() }, "Cancel"), h("button", { class: "primary", onclick: () => path.value.trim() && void add(path.value.trim()) }, "Add")]);
  conn.request({ t: "discover", refresh: false }).then((r) => {
    const known = new Set([...data.projects.values()].map((p) => p.root));
    const paths = (r.paths as string[]).filter((p) => !known.has(p));
    list.replaceChildren(...(paths.length ? paths.map((p) => row(p.split("/").pop() ?? p, p, null, { onclick: () => void add(p) })) : [h("p", { class: "empty" }, "None found: give a path.")]));
  }, (e) => list.replaceChildren(h("p", { class: "empty err" }, errText(e))));
}

const projectScripts = new Cache<any>((id) => conn.request({ t: "project_scripts", addr: id, worktree: null }).then((r) => r.scripts));

function trustProject(p: Project, scripts: any): void {
  const s = sheet(`Trust ${p.slug}'s scripts?`, [
    h("p", { class: "intro" }, `They run on your computer, as you, when a worktree is made or removed (${scripts.names.join(", ")}).`),
    h("pre", { class: "block" }, scripts.text)], [
    h("button", { onclick: () => s.close() }, "Cancel"),
    h("button", { class: "primary", onclick: async () => { s.close(); if (await ask({ t: "project_trust", addr: p.id, fingerprint: scripts.fingerprint }, "Trusted")) projectScripts.reload(p.id); } }, "Trust"),
  ]);
}

async function pinProject(p: Project): Promise<void> {
  const r = await ask({ t: "profile_list" });
  if (!r) return;
  menu(`Pin ${p.slug} to a profile`, [
    ...(r.profiles as any[]).map((pr): Item => ({ label: pr.label || pr.name, hint: pr.name === p.pin ? "pinned now" : pr.git_email ?? undefined, run: async () => { await ask({ t: "project_pin", addr: p.id, profile: pr.name }, `Pinned to ${pr.name}`); } })),
    p.pin ? { label: "Unpin", danger: true, run: async () => { await ask({ t: "project_pin", addr: p.id, profile: null }, "Unpinned"); } } : null,
  ]);
}

async function suggest(p: Project): Promise<void> {
  const r = await ask({ t: "project_suggest", addr: p.id, apply: null });
  if (!r) return;
  const sg = r.suggestions;
  if (!sg.setup && !sg.copy.length) { ui.toast("Nothing to suggest: it doesn't need more"); return; }
  const s = sheet(`For ${p.slug}'s worktrees`, [
    sg.setup ? h("div", {}, h("p", { class: "intro" }, `A setup script, from ${sg.from}:`), h("pre", { class: "block" }, sg.setup)) : null,
    sg.copy.length ? h("div", {}, h("p", { class: "intro" }, "Ignored files copied into each new worktree:"), h("pre", { class: "block" }, sg.copy.join("\n"))) : null,
    h("p", { class: "intro" }, "Saved to .ptyro.local.toml, which isn't committed."),
  ], [
    h("button", { onclick: async () => { s.close(); await ask({ t: "project_suggest", addr: p.id, apply: false }, "Not offered again"); } }, "No thanks"),
    h("button", { class: "primary", onclick: async () => { s.close(); await ask({ t: "project_suggest", addr: p.id, apply: true }, "Saved"); } }, "Use these"),
  ]);
}

function removeProject(p: Project, release = false): void {
  confirm(`Forget ${p.slug}?`, release ? "Its running sessions keep going, outside any project. Its worktrees stay on disk." : "Its worktrees stay on disk. ptyro won't register it on its own again.", "Forget it", async () => {
    try {
      await conn.request({ t: "project_remove", addr: p.id, release });
      ui.toast(`Forgot ${p.slug}`);
      go("#/p");
    } catch (e) {
      if (!release && (e as { code?: string }).code === "conflict") confirm("Sessions are running", errText(e), "Let them go", () => removeProject(p, true));
      else ui.toast(errText(e));
    }
  });
}

function projectView(id: string): HTMLElement {
  const p = data.projects.get(id);
  if (!p) return h("div", {}, ui.header("Project", "#/p"), h("main", { class: "stack" }, h("p", { class: "empty" }, conn.state === "open" ? "No such project." : "Connecting…")));
  const scripts = projectScripts.get(id);
  const wts = liveWorktrees().filter((w) => w.project === p.id);
  const ws = [...data.workspaces.values()];
  return h("div", {}, ui.header(p.slug, "#/p", h("button", { class: "icon", "aria-label": "More", onclick: () => menu(p.slug, [
    { title: tip("project.suggest"), label: "Suggestions for new worktrees", run: () => suggest(p) },
    { title: tip("project.config"), label: "Configuration", hint: "And where each value comes from", run: async () => { const r = await ask({ t: "project_config", addr: p.id }); if (r) textSheet(`${p.slug}: configuration`, r.text); } },
    { title: tip("project.pin"), label: "Pin to a profile…", run: () => pinProject(p) },
    { title: tip("radar"), label: "Collision radar", run: () => go(`#/radar/${enc(p.id)}`) },
    { title: tip("workspace.member"), label: "Workspaces…", run: () => projectWorkspaces(p) },
    { title: tip("project.remove"), label: "Forget this project", danger: true, run: () => removeProject(p) },
  ]) }, "⋯")),
  h("main", { class: "stack", title: tip("project.show") },
    facts([["Path", p.root], ["Default branch", p.default_branch], ["Remote", p.remote_url], ["Pinned to", p.pin], ["Workspaces", ws.filter((w) => w.projects.includes(p.id)).map((w) => w.name).join(", ")]]),
    bar(h("button", { class: "primary", title: tip("worktree.new"), onclick: () => newTask(`p:${p.id}`) }, "New task"), h("button", { onclick: () => void openQuickShell(wts.find((w) => w.is_root), p.root) }, "Shell")),
    section("Scripts", pending(projectScripts, id) ?? (scripts.text.trim() ? h("div", {},
      h("div", { class: "bar" }, badge(scripts.trusted ? "trusted" : "not trusted", scripts.trusted ? "ok" : "warn"), h("span", { class: "sub" }, scripts.names.join(", "))),
      h("pre", { class: "block" }, scripts.text),
      scripts.trusted
        ? h("button", { title: tip("project.untrust"), onclick: async () => { if (await ask({ t: "project_untrust", addr: p.id }, "No longer trusted")) projectScripts.reload(id); } }, "Stop trusting")
        : h("button", { class: "primary", title: tip("project.trust"), onclick: () => trustProject(p, scripts) }, "Review and trust…"))
      : h("p", { class: "sub" }, "This project has no scripts."))),
    section("Worktrees", h("div", { class: "rows" }, ...wts.map(worktreeRow)))));
}

function projectWorkspaces(p: Project): void {
  menu(`${p.slug}: workspaces`, [...data.workspaces.values()].map((w): Item => {
    const member = w.projects.includes(p.id);
    return { title: tip("workspace.member"), label: member ? `Take out of ${w.name}` : `Add to ${w.name}`, run: async () => { await ask({ t: "workspace_member", name: w.name, project: p.id, remove: member }, member ? "Taken out" : "Added"); } };
  }));
}

// ── workspaces ──────────────────────────────────────────────────────────────────────────────

function workspaceForm(w?: Workspace): void {
  form(w ? `Workspace ${w.name}` : "New workspace", [
    ...(w ? [] : [{ key: "name", label: "Name", required: true } as const]),
    { key: "profile", label: "Profile", value: w?.profile ?? "", placeholder: "default", hint: "The account its sessions use unless a worktree or project says otherwise" },
    { key: "notify", label: "Notifications", type: "select", options: [["all", "Everything"], ["badge", "A badge only"], ["mute", "Muted"]], value: w?.notify ?? "all" },
  ], w ? "Save" : "Create", async (v) => {
    if (w) await conn.request({ t: "workspace_set", name: w.name, profile: (v.profile as string) || null, notify: v.notify });
    else await conn.request({ t: "workspace_new", name: v.name, profile: (v.profile as string) || null, notify: v.notify });
  });
}

function workspacesView(): HTMLElement {
  const wss = [...data.workspaces.values()];
  return h("div", {}, ui.header("Workspaces", "#/p", h("button", { class: "icon plus", title: tip("workspace.new"), onclick: () => workspaceForm() }, "＋")),
    h("main", { class: "stack", title: tip("workspace.set") }, h("div", { class: "rows" }, ...wss.map((w) => row(
      w.name, `${w.projects.length} project${w.projects.length === 1 ? "" : "s"} · ${w.profile} · notifies ${w.notify}`, null,
      { onclick: () => menu(w.name, [
        { title: tip("workspace.set"), label: "Profile and notifications…", run: () => workspaceForm(w) },
        { title: tip("workspace.rename"), label: "Rename…", run: () => form("Rename workspace", [{ key: "name", label: "New name", value: w.name, required: true }], "Rename", (v) => conn.request({ t: "workspace_rename", name: w.name, new_name: v.name })) },
        { title: tip("workspace.remove"), label: "Remove", danger: true, run: () => confirm(`Remove ${w.name}?`, "Its projects stay; they just leave this workspace.", "Remove", () => ask({ t: "workspace_remove", name: w.name }, "Removed")) },
      ]) })))));
}

// ── a worktree ──────────────────────────────────────────────────────────────────────────────

/// Waiting for something that can take minutes (finish asks the agent to commit first).
function busy(title: string, text: string): { close: () => void } {
  return sheet(title, [h("p", { class: "intro" }, "⏳ ", text, " You can leave this page; it carries on.")]);
}

function showNotes(title: string, notes: string[]): void {
  if (!notes.length) ui.toast(title);
  else if (notes.length === 1) ui.toast(notes[0]);
  else textSheet(title, notes.join("\n"));
}

function finish(w: Worktree): void {
  menu(`Finish ${w.title || w.name}`, [
    { label: "Open a pull request", hint: "Commits what's left (the agent writes the message), pushes, opens the PR", run: async () => {
      const b = busy("Finishing…", "Asking the agent to commit what's left, then opening the PR.");
      try { const r = await conn.request({ t: "worktree_finish", addr: w.id, integrate: false }); b.close(); showNotes("Finished", r.notes ?? []); } catch (e) { b.close(); ui.toast(errText(e)); }
    } },
    { label: "Merge into the main branch", hint: "Rebases onto it and fast-forwards, no PR", run: async () => {
      const b = busy("Finishing…", "Asking the agent to commit what's left, then merging.");
      try { const r = await conn.request({ t: "worktree_finish", addr: w.id, integrate: true }); b.close(); showNotes("Merged", r.notes ?? []); } catch (e) { b.close(); ui.toast(errText(e)); }
    } },
  ]);
}

function archive(w: Worktree): void {
  const n = sessionsIn(w).filter((s) => s.state.t === "running").length;
  confirm(`Archive ${w.title || w.name}?`, `Its files (uncommitted ones too) are saved and its branch kept; the worktree is removed. Restore brings it back.${n ? ` ${n} running session${n === 1 ? " is" : "s are"} stopped.` : ""}`, "Archive", async () => {
    const r = await ask({ t: "worktree_archive", addr: w.id, stop_sessions: n > 0 }, "Archived");
    if (r) go("#/p");
  }, false);
}

function remove(w: Worktree, force = false): void {
  const n = sessionsIn(w).filter((s) => s.state.t === "running").length;
  confirm(`Remove ${w.title || w.name}?`, force ? "This throws away work that isn't saved anywhere else." : `The worktree's directory is deleted; its branch is kept.${n ? ` ${n} running session${n === 1 ? " is" : "s are"} stopped.` : ""}`, force ? "Remove and lose it" : "Remove", async () => {
    try {
      const r = await conn.request({ t: "worktree_remove", addr: w.id, opts: { force, stop_sessions: n > 0, keep_branch: false } });
      for (const note of r.notes ?? []) ui.toast(note);
      go("#/p");
    } catch (e) {
      const code = (e as { code?: string }).code;
      if (code === "dirty_worktree") {
        menu("It has work that isn't saved", [
          { label: "Archive it instead", hint: "Keeps everything; restore brings it back", run: () => archive(w) },
          { label: "Remove anyway", danger: true, hint: errText(e), run: () => remove(w, true) },
        ]);
      } else ui.toast(errText(e));
    }
  });
}

async function runScript(w: Worktree): Promise<void> {
  const r = await ask({ t: "project_scripts", addr: w.project, worktree: w.id });
  if (!r) return;
  const names = (r.scripts.names as string[]).filter((n) => n.startsWith("run:")).map((n) => n.slice(4));
  if (!names.length) { ui.toast("This project has no run scripts"); return; }
  const start = async (script: string | null) => { const s = await ask({ t: "worktree_run", addr: w.id, script, size: null }); if (s?.info) go(`#/s/${enc(s.info.id)}`); };
  if (names.length === 1) return start(names[0]);
  menu("Run a script", names.map((n, i): Item => ({ label: n, hint: i === 0 ? "default" : undefined, run: () => start(n) })));
}

async function openIn(w: Worktree): Promise<void> {
  const r = await ask({ t: "editors" });
  if (!r) return;
  menu(`Open ${w.name} in…`, (r.names as string[]).map((n): Item => ({ label: n, hint: n === r.default ? "default" : undefined, run: async () => {
    const o = await ask({ t: "worktree_open", addr: w.id, editor: n });
    if (o?.t === "session") go(`#/s/${enc(o.info.id)}`); else if (o?.notes) showNotes("Opened", o.notes);
  } })));
}

function renameBranch(w: Worktree): void {
  form("Rename the branch", [{ key: "branch", label: "New name", value: w.branch ?? "", placeholder: "from the first commit", hint: "Refused once it's pushed. Empty derives a name from the first commit." }], "Rename", async (v) => {
    await conn.request({ t: "worktree_rename_branch", addr: w.id, branch: (v.branch as string) || null });
    ui.toast("Branch renamed");
  });
}

function retitleWorktree(w: Worktree): void {
  form("Title", [{ key: "title", label: "Title", value: w.title ?? "", hint: "What this worktree is for. Empty goes back to the default." }], "Save", (v) => conn.request({ t: "retitle", addr: w.id, title: (v.title as string) || null, worktree: true }));
}

async function pairMode(w: Worktree, off: boolean): Promise<void> {
  const r = await ask({ t: "worktree_pair", addr: w.id, off, refresh: false });
  if (r) showNotes(off ? "Main checkout restored" : "Paired", r.notes ?? []);
}

function worktreeMenu(w: Worktree): void {
  menu(w.title || w.name, [
    { title: tip("worktree.open"), label: "Open in an editor…", run: () => openIn(w) },
    { title: tip("worktree.run"), label: "Run a script…", run: () => runScript(w) },
    { title: tip("worktree.pair"), label: "Pair mode: show it in the main checkout", hint: "Run it from the project's own directory", run: () => pairMode(w, false) },
    { title: tip("worktree.unpair"), label: "End pair mode", hint: "Puts the main checkout back, carrying edits over", run: () => pairMode(w, true) },
    !w.is_root && { title: tip("worktree.retitle"), label: "Set title…", run: () => retitleWorktree(w) },
    !w.is_root && { title: tip("worktree.rename_branch"), label: "Rename the branch…", run: () => renameBranch(w) },
    !w.is_root && { title: w.locked ? tip("worktree.unlock") : tip("worktree.lock"), label: w.locked ? "Unlock" : "Lock", hint: w.locked ? "Allow removing it" : "Keep it from being removed", run: async () => { await ask({ t: "worktree_lock", addr: w.id, lock: !w.locked }, w.locked ? "Unlocked" : "Locked"); } },
    !w.is_root && { title: tip("worktree.archive"), label: "Archive", hint: "Keeps its work and branch", run: () => archive(w) },
    !w.is_root && { title: tip("worktree.remove"), label: "Remove…", danger: true, run: () => remove(w) },
  ]);
}

let tab = "overview";

function worktreeView(rest: string): HTMLElement {
  const [id, which] = rest.split("/").map(safeDecode);
  const w = data.worktrees.get(id);
  if (!w) return h("div", {}, ui.header("Worktree", "#/p"), h("main", { class: "stack" }, h("p", { class: "empty" }, conn.state === "open" ? "No such worktree (it may have been removed)." : "Connecting…")));
  const current = which ?? tab;
  const tabs = [{ id: "overview", label: "Overview" }, ...wtTabs];
  const sess = sessionsIn(w);
  const proj = data.projects.get(w.project);
  const st = w.status;
  const overview = h("div", { class: "stack-in" },
    bar(
      h("button", { class: "primary", title: tip("worktree.new"), onclick: () => newTask(`w:${w.id}`) }, "New session"),
      h("button", { title: tip("worktree.finish"), onclick: () => finish(w) }, "Finish"),
      h("button", { onclick: () => void openQuickShell(w) }, "Shell")),
    section("Sessions", sess.length ? h("div", { class: "rows" }, ...sess.map((s) => row(
      h("span", {}, h("span", { class: `glyph ${statusOf(s)}` }, STATUS_GLYPH[statusOf(s)] ?? "·"), " ", s.name, s.agent ? h("small", {}, ` ${s.agent}`) : null),
      s.activity ?? s.title ?? statusOf(s), ago(s.status?.since_ms ?? s.created_ms), { href: `#/s/${enc(s.id)}` }))) : h("p", { class: "sub" }, "Nothing is running here.")),
    section("Status", h("div", { title: tip("worktree.status") }, facts([
      ["Branch", w.branch], ["Base", w.base], ["Changes", st ? `${summary(w)}${st.subject ? ` — ${st.subject}` : ""}` : ""],
      ["Pull request", w.pr ? (webHref(w.pr.url) ? h("a", { href: webHref(w.pr.url)!, target: "_blank", rel: "noreferrer" }, `#${w.pr.number} ${w.pr.title}`) : h("span", {}, `#${w.pr.number} ${w.pr.title}`)) : null],
      ["Merged into main", w.integrated ? w.integrated.slice(0, 8) : null],
      ["Ports", w.ports ? `${w.ports.base}–${w.ports.base + w.ports.count - 1}` : null],
      ["Profile", w.profile], ["Path", w.path], ["State", w.state === "ready" ? null : w.state]]))));
  const content = current === "overview" ? overview : (wtTabs.find((t) => t.id === current)?.render(w) ?? overview);
  return h("div", {}, ui.header(w.is_root ? `${proj?.slug ?? ""} · main` : w.title || w.name, "#/p", h("button", { class: "icon", title: tip("worktree.show"), "aria-label": "More", onclick: () => worktreeMenu(w) }, "⋯")),
    h("div", { class: "chips tabs-in" }, ...tabs.map((t) => h("button", { class: t.id === current ? "on" : "", onclick: () => { tab = t.id; go(`#/wt/${enc(w.id)}/${t.id}`); } }, t.label))),
    h("main", { class: "stack", title: tip("worktree.show") }, content));
}

// ── clean-up, archives ──────────────────────────────────────────────────────────────────────

const gc = new Cache<any[]>(() => conn.request({ t: "gc" }).then((r) => r.entries));
const archives = new Cache<any[]>(() => conn.request({ t: "worktree_archives", project: null }).then((r) => r.entries));

function gcView(): HTMLElement {
  const entries = gc.get("");
  const body = pending(gc, "", "Looking for stale worktrees…") ?? (entries!.length
    ? h("div", { class: "rows" }, ...entries!.map((e) => row(e.worktree.title || e.worktree.name, `${e.worktree.project_slug} · ${e.reasons.join(", ")}`, null, {
        onclick: () => menu(e.worktree.title || e.worktree.name, [
          { label: "Archive", hint: "Keeps its work", run: async () => { await ask({ t: "worktree_archive", addr: e.worktree.id, stop_sessions: false }, "Archived"); gc.reload(""); } },
          { label: "Open", run: () => go(`#/wt/${enc(e.worktree.id)}`) },
        ]) })))
    : h("p", { class: "empty" }, "Nothing is stale."));
  return h("div", {}, ui.header("Clean up", "#/p", h("button", { class: "icon", onclick: () => gc.reload(""), "aria-label": "Look again" }, "↻")),
    h("main", { class: "stack", title: tip("worktree.gc") }, h("p", { class: "intro" }, "Worktrees that are merged, abandoned or gone. Nothing is removed unless you say so."), body));
}

function archivesView(): HTMLElement {
  const entries = archives.get("");
  const body = pending(archives, "") ?? (entries!.length
    ? h("div", { class: "rows" }, ...entries!.map((e) => row(e.title || e.name, `${e.project_slug} · ${e.branch}`, ago(e.archived_ms), {
        onclick: () => confirm(`Restore ${e.title || e.name}?`, "The branch is checked out again and the saved files come back as uncommitted work.", "Restore", async () => {
          const r = await ask({ t: "worktree_restore", addr: `${e.project_slug}/${e.name}` }, "Restored");
          if (r?.info) go(`#/wt/${enc(r.info.id)}`);
        }, false) })))
    : h("p", { class: "empty" }, "No archived worktrees."));
  return h("div", {}, ui.header("Archived", "#/p"), h("main", { class: "stack", title: tip("worktree.restore") }, body));
}

// ── leases, radar, checkpoints ──────────────────────────────────────────────────────────────

const leases = new Cache<any[]>(() => conn.request({ t: "lease_list" }).then((r) => r.leases));

function leasesView(): HTMLElement {
  const ls = leases.get("");
  const body = pending(leases, "") ?? (ls!.length
    ? h("div", { class: "rows" }, ...ls!.map((l) => row(l.pattern, `${l.holder_name}${l.reason ? ` · ${l.reason}` : ""}`, l.exclusive ? badge("exclusive", "warn") : badge("shared"))))
    : h("p", { class: "empty" }, "No leases: agents haven't claimed any files."));
  return h("div", {}, ui.header("File leases", "#/p", h("button", { class: "icon", onclick: () => leases.reload(""), "aria-label": "Refresh" }, "↻")),
    h("main", { class: "stack", title: tip("leases") }, h("p", { class: "intro" }, "Agents claim the files they edit, so two of them don't work on the same ones."), body));
}

const radar = new Cache<any>((id) => conn.request({ t: "radar", project: id || null }).then((r) => r.radar));

function radarView(rest: string): HTMLElement {
  const id = safeDecode(rest);
  const r = radar.get(id);
  const body = pending(radar, id, "Comparing worktrees…") ?? (() => {
    const ov = r.overlaps as any[];
    if (!ov.length) return h("p", { class: "empty" }, `No collisions between ${r.worktrees.length} worktree${r.worktrees.length === 1 ? "" : "s"}.`);
    return h("div", { class: "rows" }, ...ov.map((o) => row(`${o.a} ↔ ${o.b}`, o.files.slice(0, 4).join(", ") + (o.files.length > 4 ? ` +${o.files.length - 4}` : ""), o.conflicts.length ? badge(`${o.conflicts.length} would conflict`, "bad") : badge(`${o.files.length} shared`, "warn"))));
  })();
  return h("div", {}, ui.header("Collision radar", id ? `#/proj/${enc(id)}` : "#/p", h("button", { class: "icon", onclick: () => radar.reload(id), "aria-label": "Refresh" }, "↻")),
    h("main", { class: "stack", title: tip("radar") }, body));
}

const checkpoints = new Cache<any[]>((id) => conn.request({ t: "checkpoint_list", addr: id }).then((r) => r.checkpoints));

function checkpointsTab(w: Worktree): HTMLElement {
  const cps = checkpoints.get(w.id);
  return pending(checkpoints, w.id) ?? (cps!.length ? h("div", { class: "rows", title: tip("checkpoint.list") }, ...[...cps!].reverse().map((c) => row(
    `#${c.n} · turn ${c.turn}`, `${c.files} file${c.files === 1 ? "" : "s"} · ${c.session}`, ago(c.at_ms),
    { onclick: () => menu(`Checkpoint ${c.n}`, [
      { title: tip("checkpoint.diff"), label: "What changed since", run: async () => { const r = await ask({ t: "checkpoint_diff", addr: w.id, n: c.n, to: null }); if (r) textSheet(`Since checkpoint ${c.n}`, diffText(r.doc ?? r)); } },
      { title: tip("checkpoint.restore"), label: "Put the worktree back to this", danger: true, run: () => confirm(`Restore checkpoint ${c.n}?`, "The worktree goes back to how it was then. Where it is now is checkpointed first, so you can undo this.", "Restore", async () => { await ask({ t: "checkpoint_restore", addr: w.id, n: c.n, turn: null }, "Restored"); checkpoints.reload(w.id); }) },
    ]) }))) : h("p", { class: "empty" }, "No checkpoints yet: one is taken at the end of each agent turn."));
}

/// A diff document as unified text.
export function diffText(doc: any): string {
  if (typeof doc === "string") return doc;
  const files = (doc.files ?? []) as any[];
  if (!files.length) return "No changes.";
  return files.map((f) => `${f.path}${f.old_path ? ` (from ${f.old_path})` : ""}\n${(f.hunks ?? []).map((hk: any) => `${hk.header ?? ""}\n${(hk.lines ?? []).map((l: any) => `${l.kind === "add" ? "+" : l.kind === "del" ? "-" : " "}${l.text}`).join("\n")}`).join("\n")}`).join("\n\n");
}

wtTabs.push({ id: "checkpoints", label: "Checkpoints", render: checkpointsTab });

views.set("p", () => projectsView());
views.set("proj", projectView);
views.set("wt", worktreeView);
views.set("ws", () => workspacesView());
views.set("gc", () => gcView());
views.set("archives", () => archivesView());
views.set("leases", () => leasesView());
views.set("radar", radarView);
