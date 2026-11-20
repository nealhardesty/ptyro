// Projects, worktrees and workspaces (ROADMAP WEB1.3): the sidebar's tree as the Projects tab,
// a worktree's page, and everything the TUI does to them: new, remove, scripts, checkpoints and
// the clean-up.
import { conn } from "./conn.js";
import { data, leftWords, liveWorktrees, machineOf, repoLeft, splitMachine, wsLabel, wtTabs } from "./data.js";
import { machineChip } from "./machines.js";
import { actions, NAME_HINT, SLUG_HINT, tipOf, ago, ask, badge, bar, Cache, confirm, enc, errText, facts, fixOf, form, go, h, menu, pending, profileFor, row, safeDecode, section, sheet, statusWord, textSheet, ui, views, withFix } from "./kit.js";
import { fillProfiles } from "./settings.js";
import { againWords, newShell, newTask, openQuickShell, allSessions, startAgain } from "./sessions.js";
import { historyOf } from "./library.js";
const ACTIONS = {
    "projects": "Projects and their worktrees, with what each is doing",
    "project.add": "Add a repository: pick from the ones found on this computer, or give its path",
    "project.show": "A project's page: its branches, scripts, configuration and pin",
    "project.remove": "Forget a project (its worktrees stay on disk)",
    "project.trust": "Trust the project's scripts as they are now, so they run",
    "project.untrust": "Stop trusting the project's scripts",
    "project.config": "The project's configuration, and where each value comes from",
    "project.pin": "Pin the project to a profile (its account)",
    "project.rename": "Rename the project: its address, which finds its worktrees and sessions (project/name)",
    "project.move": "Its repository moved: point the project at where it is now; it keeps its trust, pin, workspaces and worktrees (a project registered there is folded in)",
    "worktree.new": "A new worktree in this project, with an agent in it if you like",
    "worktree.show": "A worktree's page: its status, its sessions and what to do next",
    "worktree.remove": "Remove the worktree (it refuses to lose work, saying what it would lose)",
    "worktree.status": "Where the worktree stands: what is left to commit or push (✓ pushed when nothing is)",
    "worktree.run": "Run one of the project's scripts in this worktree",
    "worktree.open": "Open the worktree in an editor installed on the computer, its file manager, a terminal app there, or a new shell",
    "worktree.rename": "Rename the worktree: its name is its address (its branch and directory stay), its title is what people read",
    "worktree.gc": "Stale worktrees: merged, abandoned or missing; remove them or keep them",
    "worktree.keep": "Keep a stale worktree: clean-up leaves it be until it is unused for another worktrees.stale_days, or its branch moves on",
    "workspace.switch": "Show one workspace's projects, or all of them",
    "workspace.new": "A new workspace",
    "workspace.set": "A workspace's profile and how it notifies",
    "workspace.remove": "Remove a workspace",
    "workspace.rename": "Rename a workspace",
    "workspace.member": "Add a project to a workspace, or take it out",
    "checkpoint.list": "Checkpoints: the worktree as it was at the end of each agent turn",
    "checkpoint.diff": "What changed since a checkpoint",
    "checkpoint.restore": "Put the worktree back to a checkpoint (the state now is checkpointed first)",
};
const tip = actions(ACTIONS);
// ── what the lists show ─────────────────────────────────────────────────────────────────────
const STATUS_GLYPH = { working: "●", blocked: "⚠", idle: "○", done: "✓", errored: "✗", starting: "◌" };
const statusOf = (s) => (s.state.t !== "running" ? s.state.t : s.status?.status ?? (s.kind === "agent" ? "unknown" : "shell"));
/// The sessions running in a worktree, but not its quick shell.
const sessionsIn = (w) => [...allSessions().values()].filter((s) => s.worktree === w.id && !s.quick);
/// What is left to do, short (UX2.2): `+12 −3 · ↑2 ● 4`, or a quiet `✓ pushed`. The line counts
/// show only while something is uncommitted or unpushed.
function summary(w) {
    const st = w.status;
    if (!st)
        return w.state === "ready" ? "" : w.state;
    const parts = [];
    const l = repoLeft(st);
    if ((l.uncommitted || l.unpushed) && (st.insertions || st.deletions))
        parts.push(`+${st.insertions} −${st.deletions}`);
    const words = leftWords(l);
    if (words)
        parts.push(words);
    if (st.behind)
        parts.push(`${st.behind} behind`);
    if (st.conflicts)
        parts.push(`${st.conflicts} conflicts`);
    return parts.join(" · ") || "clean";
}
function worktreeRow(w) {
    const sess = sessionsIn(w);
    const worst = ["blocked", "errored", "working", "starting", "done", "idle"].find((k) => sess.some((s) => statusOf(s) === k));
    const glyph = worst ? h("span", { class: `glyph ${worst}` }, STATUS_GLYPH[worst]) : null;
    return row(h("span", {}, glyph, glyph ? " " : "", w.is_root ? "main checkout" : w.title || w.name, w.locked ? " 🔒" : ""), [w.is_root ? w.branch ?? "" : w.title ? w.name : "", summary(w), sess.length ? `${sess.length} session${sess.length === 1 ? "" : "s"}` : ""].filter(Boolean).join(" · "), w.last_used_ms ? ago(w.last_used_ms) : "", { href: `#/wt/${enc(w.id)}`, cls: w.state !== "ready" ? "dim" : "" });
}
// ── the Projects tab ────────────────────────────────────────────────────────────────────────
let wsFilter = "";
function projectsView() {
    const wss = [...data.workspaces.values()];
    const inWs = (p) => !wsFilter || data.workspaces.get([...data.workspaces.keys()].find((k) => data.workspaces.get(k).name === wsFilter))?.projects.includes(p.id);
    const projects = [...data.projects.values()].filter(inWs).sort((a, b) => (b.last_used_ms ?? 0) - (a.last_used_ms ?? 0));
    const chips = wss.length > 1 ? h("div", { class: "chips", title: tip("workspace.switch") }, h("button", { class: wsFilter ? "" : "on", onclick: () => { wsFilter = ""; ui.render(); } }, "All"), ...wss.map((w) => h("button", { class: wsFilter === w.name ? "on" : "", title: machineOf(w.name)?.fix ?? undefined, onclick: () => { wsFilter = w.name; ui.render(); } }, wsLabel(w.name), machineOf(w.name) && machineOf(w.name).state !== "online" ? " ⚠" : ""))) : null;
    const body = projects.length ? projects.map((p) => {
        const wts = liveWorktrees().filter((w) => w.project === p.id).sort((a, b) => Number(b.is_root) - Number(a.is_root) || (b.last_used_ms ?? 0) - (a.last_used_ms ?? 0));
        return h("section", { class: "proj" }, h("div", { class: "proj-head" }, h("a", { class: "proj-name", href: `#/proj/${enc(p.id)}`, title: tip("project.show") }, wsLabel(p.slug)), machineChip(p.slug), p.pin ? badge(p.pin, "info") : null, projectMissing(p) ? badge("missing", "warn") : null, h("span", { class: "spacer" }), projectMissing(p) ? null : h("button", { class: "small", title: tip("worktree.new"), onclick: () => newTask(`p:${p.id}`) }, "＋ Task")), movedBanner(p), h("div", { class: "rows" }, ...wts.map(worktreeRow)));
    }) : [h("div", { class: "empty" }, h("p", {}, wsFilter ? `No projects in ${wsFilter}.` : "No projects yet."), h("button", { class: "primary", onclick: addProject }, "Add a project"))];
    return h("div", {}, ui.header("Projects", false, cleanUpButton(), h("button", { class: "icon plus", title: tip("project.add"), "aria-label": "Add a project", onclick: addProject }, "＋")), chips, h("main", { class: "list", title: tip("projects") }, ...body, h("div", { class: "foot-links" }, h("a", { href: "#/ws", title: tip("workspace.set") }, "Workspaces"))));
}
// ── projects ────────────────────────────────────────────────────────────────────────────────
function addProject() {
    const list = h("div", { class: "rows" }, h("p", { class: "empty" }, "Looking for repositories…"));
    const path = h("input", { placeholder: "/path/to/a/repository", autocapitalize: "off", spellcheck: "false" });
    // This machine's workspaces (a path is on this machine); the one the list shows is picked.
    const wss = [...data.workspaces.values()].filter((w) => !splitMachine(w.name)[0]).map((w) => w.name);
    if (!wss.includes("main"))
        wss.unshift("main");
    const ws = h("select", {}, ...wss.map((n) => h("option", { value: n }, n)));
    ws.value = wss.includes(wsFilter) ? wsFilter : "main";
    // `--moved` / `--separate`: what to do when a project whose directory is gone is this
    // repository, moved (SPEC §10.1). Asked when it happens, unless said here.
    const moved = h("select", {}, h("option", { value: "" }, "Ask me"), h("option", { value: "moved" }, "Move that project here"), h("option", { value: "separate" }, "Add it as a project of its own"));
    const add = async (p) => {
        if (!isFull(p)) {
            ui.toast(RELATIVE);
            return;
        }
        const r = await ask({ t: "project_add", path: p, workspace: ws.value || null, moved: moved.value === "moved", separate: moved.value === "separate" });
        if (r) {
            s.close();
            for (const n of r.notes ?? [])
                ui.toast(n);
            go(`#/proj/${enc(r.info.id)}`);
        }
    };
    const s = sheet("Add a project", [
        h("label", { class: "field" }, h("span", {}, "Add to the workspace"), ws),
        h("label", { class: "field" }, h("span", {}, "If a project whose directory is gone is this repository, moved"), moved),
        h("p", { class: "intro" }, "Repositories found on this computer:"), list,
        h("label", { class: "field" }, h("span", {}, "Or a path"), path),
    ], [h("button", { onclick: () => s.close() }, "Cancel"), h("button", { class: "primary", onclick: () => path.value.trim() && void add(path.value.trim()) }, "Add")]);
    requestAnimationFrame(() => path.focus());
    conn.request({ t: "discover", refresh: false }).then((r) => {
        const known = new Set([...data.projects.values()].map((p) => p.root));
        const paths = r.paths.filter((p) => !known.has(p));
        list.replaceChildren(...(paths.length ? paths.map((p) => row(p.split("/").pop() ?? p, p, null, { onclick: () => void add(p) })) : [h("p", { class: "empty" }, "None found: give a path.")]));
    }, (e) => list.replaceChildren(h("p", { class: "empty err" }, errText(e))));
}
const projectScripts = new Cache((id) => conn.request({ t: "project_scripts", addr: id, worktree: null }).then((r) => r.scripts));
function trustProject(p, scripts) {
    const s = sheet(`Trust ${p.slug}'s scripts?`, [
        h("p", { class: "intro" }, `They run on your computer, as you, when a worktree is made or you run one (${scripts.names.join(", ")}).`),
        h("pre", { class: "block" }, scripts.text)
    ], [
        h("button", { onclick: () => s.close() }, "Cancel"),
        h("button", { class: "primary", onclick: async () => { s.close(); if (await ask({ t: "project_trust", addr: p.id, fingerprint: scripts.fingerprint }, "Trusted"))
                projectScripts.reload(p.id); } }, "Trust"),
    ]);
}
async function pinProject(p) {
    const r = await ask({ t: "profile_list" });
    if (!r)
        return;
    menu(`Pin ${p.slug} to a profile`, [
        ...r.profiles.map((pr) => ({ label: pr.label || pr.name, hint: pr.name === p.pin ? "pinned now" : pr.git_email ?? undefined, run: async () => { await ask({ t: "project_pin", addr: p.id, profile: pr.name }, `Pinned to ${pr.name}`); } })),
        p.pin ? { label: "Unpin", danger: true, run: async () => { await ask({ t: "project_pin", addr: p.id, profile: null }, "Unpinned"); } } : null,
    ]);
}
function renameProject(p) {
    // Another machine's project is renamed there, by the name it has there (MM1.B).
    const slug = splitMachine(p.slug)[1];
    form(`Rename ${p.slug}`, [{ key: "name", label: "Name", value: slug, required: true, slug: "project", hint: `Its address: ${p.slug}/name finds its worktrees and sessions. Its worktrees' directories stay where they are. ${SLUG_HINT}` }], "Rename", async (v) => {
        if (v.name === slug)
            return;
        await conn.request({ t: "project_rename", addr: p.id, name: v.name });
        ui.toast(`Renamed to ${v.name}`);
    });
}
// ── a repository that moved (SPEC §10.1) ─────────────────────────────────────────────────
/// Its main checkout is gone (its @root missing), or ptyro found where it went.
export const projectMissing = (p) => !!p.moved_to || liveWorktrees().some((w) => w.project === p.id && w.is_root && w.state === "missing");
/// The project registered at the place `p` moved to, if any: moving folds it in.
const registeredAt = (p) => p.moved_to ? [...data.projects.values()].find((d) => d.id !== p.id && d.root === p.moved_to) : undefined;
/// A project ptyro found moved: said once (called before the event is applied); the Projects
/// tab and its page offer the move.
export function moveMaybeSpotted(e) {
    if (e.t !== "project_updated" || !e.info?.moved_to || data.projects.get(e.info.id)?.moved_to === e.info.moved_to)
        return;
    ui.toast(`${e.info.slug} seems to have moved to ${e.info.moved_to}: its page offers the move`);
}
/// A path typed here: the web has no folder of its own for a relative one to start from (the
/// CLI and the TUI start from theirs), so it is refused, saying how to write it.
const RELATIVE = "A full path is needed, like /Users/me/src/app or ~/src/app: the web has no current folder for a relative one to start from";
const isFull = (path) => /^(\/|~(\/|$))/.test(path.trim());
function fullPath(path) {
    if (!isFull(path))
        throw new Error(RELATIVE);
    return path.trim();
}
/// Move it: the notes say what followed (worktrees linked, a duplicate folded in).
export async function moveProjectTo(p, path) {
    const r = await ask({ t: "project_move", addr: p.id, path });
    if (!r)
        return false;
    for (const n of r.notes?.length ? r.notes : [`Moved ${p.slug}`])
        ui.toast(n);
    return true;
}
/// `project.move`: where its repository is now, the place ptyro found filled in.
export function moveProject(p) {
    form(`Move ${p.slug}`, [{ key: "path", label: "It is now at", value: p.moved_to ?? "", placeholder: "/path/to/the/repository", required: true, hint: `Its main checkout (it was at ${p.root}). It keeps its trust, pin, workspaces and worktrees, and its worktrees are linked to the new place; a project registered there is folded in. Another repository, or a copy (the old one still there), is refused.` }], "Move", async (v) => {
        const r = await conn.request({ t: "project_move", addr: p.id, path: fullPath(String(v.path)) });
        for (const n of r?.notes?.length ? r.notes : [`Moved ${p.slug}`])
            ui.toast(n);
    });
}
/// What a project whose directory is gone offers: the move ptyro found (folding in a project
/// registered there), saying where it went, keeping a place apart, or forgetting it.
function movedBanner(p) {
    if (!projectMissing(p))
        return null;
    const into = registeredAt(p);
    return h("div", { class: "banner warn moved" }, p.moved_to
        ? `${p.slug}'s directory (${p.root}) is gone; it seems to have moved to ${p.moved_to}${into ? `, registered again there as ${into.slug}: moving folds ${into.slug} into ${p.slug}` : ""}. `
        : `${p.slug}'s directory (${p.root}) is gone. `, p.moved_to ? h("button", { class: "small primary", title: tip("project.move"), onclick: () => void moveProjectTo(p, p.moved_to) }, "Move it there") : null, h("button", { class: "small", title: tip("project.move"), onclick: () => moveProject(p) }, p.moved_to ? "Somewhere else…" : "Moved it? Say where…"), p.moved_to && !into ? h("button", { class: "small", title: tip("project.add"), onclick: async () => { const r = await ask({ t: "project_add", path: p.moved_to, workspace: null, moved: false, separate: true }, "Added as a project of its own"); if (r)
            go(`#/proj/${enc(r.info.id)}`); } }, "It's another project") : null, h("button", { class: "small", title: tip("project.remove"), onclick: () => removeProject(p) }, "Forget it"));
}
// Asks as the CLI's `project rm` and the TUI do (ptyro_core::removal); running sessions in it
// refuse with a way through, "Let them go" (SPEC §6.6), which `ask` offers.
export function removeProject(p) {
    confirm(`Forget project ${p.slug}?`, "ptyro forgets it: its trust, its pin and its place in workspaces. Its worktrees stay on disk, and it won't register on its own again.", "Forget it", async () => {
        // Its notes say what stays on disk, and any session let go under a new name.
        const r = await ask({ t: "project_remove", addr: p.id, release: false });
        if (r) {
            for (const n of r.notes?.length ? r.notes : [`Forgot ${p.slug}`])
                ui.toast(n);
            go("#/p");
        }
    });
}
function projectView(id) {
    const p = data.projects.get(id);
    if (!p)
        return h("div", {}, ui.header("Project", "#/p"), h("main", { class: "stack" }, h("p", { class: "empty" }, conn.state === "open" ? "No such project." : "Connecting…")));
    const scripts = projectScripts.get(id);
    const wts = liveWorktrees().filter((w) => w.project === p.id);
    const ws = [...data.workspaces.values()];
    const missing = projectMissing(p);
    const move = { title: tip("project.move"), label: "Move…", hint: missing ? "Its directory is gone: where is it now?" : "Its repository moved: where it is now", run: () => moveProject(p) };
    return h("div", {}, ui.header(p.slug, "#/p", h("button", { class: "icon", "aria-label": "More", onclick: () => menu(p.slug, [
            // One whose directory is gone leads with finding it, then forgetting it (SPEC §10.1).
            ...(missing ? [move, { title: tip("project.remove"), label: "Forget this project", danger: true, run: () => removeProject(p) }] : []),
            { title: tip("project.config"), label: "Configuration", hint: "And where each value comes from", run: async () => { const r = await ask({ t: "project_config", addr: p.id }); if (r)
                    textSheet(`${p.slug}: configuration`, r.text); } },
            { title: tipOf("kb.history"), label: "History", hint: "What the knowledge base kept of work in it", run: () => historyOf(null, p.id) },
            { title: tip("project.rename"), label: "Rename…", run: () => renameProject(p) },
            ...(missing ? [] : [move]),
            { title: tip("project.pin"), label: "Pin to a profile…", run: () => pinProject(p) },
            { title: tip("workspace.member"), label: "Workspaces…", run: () => projectWorkspaces(p) },
            ...(missing ? [] : [{ title: tip("project.remove"), label: "Forget this project", danger: true, run: () => removeProject(p) }]),
        ]) }, "⋯")), h("main", { class: "stack", title: tip("project.show") }, movedBanner(p), facts([["Path", missing ? `${p.root} (gone)` : p.root], ["Default branch", p.default_branch], ["Remote", p.remote_url], ["Pinned to", p.pin], ["Workspaces", ws.filter((w) => w.projects.includes(p.id)).map((w) => w.name).join(", ")]]), bar(h("button", { class: "primary", title: tip("worktree.new"), onclick: () => newTask(`p:${p.id}`) }, "New task"), ...((r) => r ? [h("button", { title: tipOf("session.again"), onclick: () => void startAgain(r) }, againWords(r)), h("button", { title: tipOf("shell.new"), onclick: () => void newShell(r) }, "New shell")] : [])(wts.find((w) => w.is_root)), h("button", { title: tipOf("shell.here"), onclick: () => void openQuickShell(wts.find((w) => w.is_root), p.root) }, "Quick shell")), section("Scripts", pending(projectScripts, id) ?? (scripts.text.trim() ? h("div", {}, h("div", { class: "bar" }, badge(scripts.trusted ? "trusted" : "not trusted", scripts.trusted ? "ok" : "warn"), h("span", { class: "sub" }, scripts.names.join(", "))), h("pre", { class: "block" }, scripts.text), scripts.trusted
        ? h("button", { title: tip("project.untrust"), onclick: async () => { if (await ask({ t: "project_untrust", addr: p.id }, "No longer trusted"))
                projectScripts.reload(id); } }, "Stop trusting")
        : h("button", { class: "primary", title: tip("project.trust"), onclick: () => trustProject(p, scripts) }, "Review and trust…"))
        : h("p", { class: "sub" }, "This project has no scripts."))), section("Worktrees", h("div", { class: "rows" }, ...wts.map(worktreeRow)))));
}
function projectWorkspaces(p) {
    menu(`${p.slug}: workspaces`, [...data.workspaces.values()].map((w) => {
        const member = w.projects.includes(p.id);
        return { title: tip("workspace.member"), label: member ? `Take out of ${w.name}` : `Add to ${w.name}`, run: async () => { await ask({ t: "workspace_member", name: w.name, project: p.id, remove: member }, member ? "Taken out" : "Added"); } };
    }));
}
// ── workspaces ──────────────────────────────────────────────────────────────────────────────
function workspaceForm(w) {
    const machines = [...data.machines.values()];
    const s = form(w ? `Workspace ${wsLabel(w.name)}` : "New workspace", [
        ...(w ? [] : [{ key: "name", label: "Name", required: true, slug: "name", hint: NAME_HINT }]),
        // With other machines (MM1.B): which one it's made on.
        ...(w || !machines.length ? [] : [{ key: "machine", label: "On which machine", type: "select", options: [["", "This machine"], ...machines.map((m) => [m.name, m.state === "online" ? m.name : `${m.name} (${m.state})`])], value: "" }]),
        { key: "profile", label: "Profile", type: "select", options: [[w?.profile ?? "default", w?.profile ?? "default"]], hint: "The account its sessions use unless a worktree or project says otherwise" },
        { key: "notify", label: "Notifications", type: "select", options: [["all", "Everything"], ["badge", "A badge only"], ["mute", "Muted"]], value: w?.notify ?? "all" },
    ], w ? "Save" : "Create", async (v) => {
        if (v.profile === "@new")
            throw new Error("Pick a profile.");
        if (w)
            await conn.request({ t: "workspace_set", name: w.name, profile: v.profile || null, notify: v.notify });
        else {
            // Known at once, not when its event arrives (UX3.3).
            const name = v.machine ? `${v.machine}:${v.name}` : v.name;
            const r = await conn.request({ t: "workspace_new", name, profile: v.profile || null, notify: v.notify });
            if (r?.info) {
                data.workspaces.set(r.info.id, r.info);
                ui.render();
            }
        }
    });
    // The profiles of the machine it's on; a new one starts with the profile named like it, if
    // any, until a profile is picked by hand.
    const prof = s.input("profile");
    const nameBox = s.input("name");
    const machineSel = s.input("machine");
    const onMachine = () => (w ? splitMachine(w.name)[0] : machineSel?.value || null);
    let names = [];
    let picked = !!w;
    const fill = async () => {
        names = await fillProfiles(prof, onMachine(), w ? (w.profile ? splitMachine(w.profile)[1] : "default") : prof.value);
        if (!picked)
            prof.value = profileFor(names, nameBox?.value.trim() ?? "");
    };
    void fill();
    prof.addEventListener("change", () => { if (prof.value !== "@new")
        picked = true; });
    nameBox?.addEventListener("input", () => { if (!picked)
        prof.value = profileFor(names, nameBox.value.trim()); });
    machineSel?.addEventListener("change", () => void fill());
}
function workspacesView() {
    const wss = [...data.workspaces.values()];
    return h("div", {}, ui.header("Workspaces", "#/p", h("button", { class: "icon plus", title: tip("workspace.new"), onclick: () => workspaceForm() }, "＋")), h("main", { class: "stack", title: tip("workspace.set") }, h("div", { class: "rows" }, ...wss.map((w) => row(w.name, `${w.projects.length} project${w.projects.length === 1 ? "" : "s"} · ${w.profile} · notifies ${w.notify}`, null, { onclick: () => menu(w.name, [
            { title: tip("workspace.set"), label: "Profile and notifications…", run: () => workspaceForm(w) },
            { title: tip("workspace.rename"), label: "Rename…", run: () => form("Rename workspace", [{ key: "name", label: "New name", value: splitMachine(w.name)[1], required: true, slug: "name", hint: NAME_HINT }], "Rename", (v) => conn.request({ t: "workspace_rename", name: w.name, new_name: v.name })) },
            { title: tip("workspace.remove"), label: "Remove", danger: true, run: () => confirm(`Remove workspace ${w.name}?`, "Its sessions keep running and move to main, with its chats and the projects no other workspace has.", "Remove", () => ask({ t: "workspace_remove", name: w.name }, "Removed")) },
        ]) })))));
}
// ── a worktree ──────────────────────────────────────────────────────────────────────────────
function showNotes(title, notes) {
    if (!notes.length)
        ui.toast(title);
    else if (notes.length === 1)
        ui.toast(notes[0]);
    else
        textSheet(title, notes.join("\n"));
}
function remove(w, force = false, stopAgreed = false) {
    const n = sessionsIn(w).filter((s) => s.state.t === "running" || s.state.t === "starting").length;
    // Unsaved work says what would be lost and offers removing anyway; any other refusal with a
    // way through (a session the count above missed: a loose shell, one starting) offers it, as
    // `ask` does (SPEC §6.6).
    const send = async (req) => {
        try {
            const r = await conn.request(req);
            for (const note of r.notes ?? [])
                ui.toast(note);
            go("#/p");
        }
        catch (e) {
            // Work would be lost: what, and the way through (or commit and push it first).
            const f = fixOf(e), again = f && withFix(req, f);
            if (f?.fix === "force" && !force) {
                menu("It has work that isn't saved", [
                    { label: "Keep it", hint: "Commit and push it first, then remove it", run: () => ui.toast("Kept: nothing was removed") },
                    { label: "Remove anyway", danger: true, hint: errText(e), run: () => remove(w, true, req.opts?.stop_sessions === true) },
                ]);
            }
            else if (f && again)
                confirm(f.label, errText(e), f.label, () => send(again));
            else
                ui.toast(errText(e));
        }
    };
    confirm(`Remove worktree ${w.title || w.name}?`, `Its directory is deleted. Its branch is deleted too if it is merged into its base or the same as its pushed copy; otherwise it is kept. ${force ? "Work that isn't saved anywhere else is thrown away." : "Nothing is removed if that would lose work."}${n ? ` ${n} running session${n === 1 ? " is" : "s are"} stopped.` : ""}`, force ? "Remove and lose it" : "Remove", () => send({ t: "worktree_remove", addr: w.id, opts: { force, stop_sessions: stopAgreed || n > 0, keep_branch: false } }));
}
async function runScript(w) {
    const r = await ask({ t: "project_scripts", addr: w.project, worktree: w.id });
    if (!r)
        return;
    const names = r.scripts.names.filter((n) => n.startsWith("run:")).map((n) => n.slice(4));
    if (!names.length) {
        ui.toast("This project has no run scripts");
        return;
    }
    const start = async (script) => { const s = await ask({ t: "worktree_run", addr: w.id, script, size: null }); if (s?.info)
        go(`#/s/${enc(s.info.id)}`); };
    if (names.length === 1)
        return start(names[0]);
    menu("Run a script", names.map((n, i) => ({ label: n, hint: i === 0 ? "default" : undefined, run: () => start(n) })));
}
/// The file manager, by any of its names (the daemon's computer calls it one of them), and the
/// terminal apps (`TERMINAL_APPS`, crates/ptyro-core/src/config.rs): both open on the computer.
const FILE_MANAGERS = ["finder", "files", "explorer"];
const TERMINAL_APPS = ["terminal", "iterm", "ghostty", "wezterm", "kitty", "alacritty", "gnome-terminal", "kgx", "konsole", "xfce4-terminal", "foot"];
/// Open in: the editors installed on the computer, its file manager and terminal apps, then a new
/// shell here.
async function openIn(w) {
    const r = await ask({ t: "editors" });
    if (!r)
        return;
    const hint = (n) => [n === r.default && "default", FILE_MANAGERS.includes(n) && "Shows the folder, on the computer", TERMINAL_APPS.includes(n) && "A terminal window, on the computer"].filter(Boolean).join(" · ") || undefined;
    menu(`Open ${w.name} in…`, [
        ...r.names.map((n) => ({ label: n, hint: hint(n), run: async () => {
                const o = await ask({ t: "worktree_open", addr: w.id, editor: n });
                if (o?.t === "session")
                    go(`#/s/${enc(o.info.id)}`);
                else if (o?.notes)
                    showNotes("Opened", o.notes);
            } })),
        { title: tipOf("shell.new"), label: "shell", hint: "A new shell in it", run: () => void newShell(w) },
    ]);
}
/// One sheet for both: its name (the address) and its title (what people read). The main
/// checkout is always `@root`: only its title changes.
function renameWorktree(w) {
    const titleField = { key: "title", label: "Title", value: w.title ?? "", hint: "What people read: what it's for. Empty clears it." };
    const fields = w.is_root ? [titleField] : [
        { key: "name", label: "Name", value: w.name, required: true, slug: "name", hint: `Its address: ${w.project_slug}/name, unique in its project. Its branch and its directory stay, so what runs in it carries on. ${NAME_HINT}` },
        titleField,
    ];
    form(w.is_root ? `Title of ${w.project_slug}/@root` : `Rename ${w.name}`, fields, "Save", async (v) => {
        const name = String(v.name ?? "").trim(), title = String(v.title ?? "").trim();
        const said = [];
        if (!w.is_root && name && name !== w.name) {
            await conn.request({ t: "worktree_rename", addr: w.id, name });
            said.push(`Renamed to ${name}`);
        }
        if (title !== (w.title ?? "")) {
            await conn.request({ t: "retitle", addr: w.id, title: title || null, worktree: true });
            said.push(title ? "title set" : "title cleared");
        }
        if (said.length)
            ui.toast(said.join(", ").replace(/^t/, "T"));
    });
}
function worktreeMenu(w) {
    menu(w.title || w.name, [
        { title: tip("worktree.open"), label: "Open in…", hint: "An editor, the file manager, a terminal or a shell", run: () => openIn(w) },
        { title: tip("worktree.run"), label: "Run a script…", run: () => runScript(w) },
        { title: tipOf("kb.history"), label: "History", hint: "What the knowledge base kept of work in it", run: () => historyOf({ t: "worktree", id: w.id, name: `${w.project_slug}/${w.name}` }) },
        { title: tip("worktree.rename"), label: w.is_root ? "Set title…" : "Rename…", hint: w.is_root ? "What people read" : "Its name and title; the branch stays", run: () => renameWorktree(w) },
        !w.is_root && { title: tip("worktree.remove"), label: "Remove…", danger: true, run: () => remove(w) },
    ]);
}
let tab = "overview";
function worktreeView(rest) {
    const [id, which] = rest.split("/").map(safeDecode);
    const w = data.worktrees.get(id);
    if (!w)
        return h("div", {}, ui.header("Worktree", "#/p"), h("main", { class: "stack" }, h("p", { class: "empty" }, conn.state === "open" ? "No such worktree (it may have been removed)." : "Connecting…")));
    const current = which ?? tab;
    const tabs = [{ id: "overview", label: "Overview" }, ...wtTabs];
    const sess = sessionsIn(w);
    const proj = data.projects.get(w.project);
    const st = w.status;
    const overview = h("div", { class: "stack-in" }, bar(h("button", { class: "primary", title: tip("worktree.new"), onclick: () => newTask(`w:${w.id}`) }, "New session"), h("button", { title: tipOf("session.again"), onclick: () => void startAgain(w) }, againWords(w)), h("button", { title: tipOf("shell.new"), onclick: () => void newShell(w) }, "New shell"), h("button", { title: tipOf("shell.pane"), onclick: () => void openQuickShell(w) }, "Quick shell")), section("Sessions", sess.length ? h("div", { class: "rows" }, ...sess.map((s) => row(h("span", {}, h("span", { class: `glyph ${statusOf(s)}` }, STATUS_GLYPH[statusOf(s)] ?? "·"), " ", s.name, s.agent ? h("small", {}, ` ${s.agent}`) : null), s.activity ?? s.title ?? statusWord(statusOf(s)), ago(s.status?.since_ms ?? s.created_ms), { href: `#/s/${enc(s.id)}` }))) : h("p", { class: "sub" }, "Nothing is running here.")), section("Status", h("div", { title: tip("worktree.status") }, facts([
        ["Branch", w.branch], ["Base", w.base], ["Changes", st ? `${summary(w)}${st.subject ? ` — ${st.subject}` : ""}` : ""],
        ["Profile", w.profile], ["Path", w.path], ["State", w.state === "ready" ? null : w.state]
    ]))));
    const content = current === "overview" ? overview : (wtTabs.find((t) => t.id === current)?.render(w) ?? overview);
    return h("div", {}, ui.header(w.is_root ? `${proj?.slug ?? ""} · main` : w.title || w.name, "#/p", h("button", { class: "icon", title: tip("worktree.show"), "aria-label": "More", onclick: () => worktreeMenu(w) }, "⋯")), h("div", { class: "chips tabs-in" }, ...tabs.map((t) => h("button", { class: t.id === current ? "on" : "", onclick: () => { tab = t.id; go(`#/wt/${enc(w.id)}/${t.id}`); } }, t.label))), h("main", { class: "stack", title: tip("worktree.show") }, content));
}
// ── clean-up ──────────────────────────────────────────────────────────────────────
// Stale worktrees, and scratch shells' directories left behind (SPEC §23.8).
const gc = new Cache(() => conn.request({ t: "gc" }).then((r) => ({ entries: r.entries, scratch: r.scratch ?? [] })));
/// `512 B`, `3.4 MiB`, `12.0 GiB`, as the CLI and the TUI say it.
function bytes(b) {
    const u = ["B", "KiB", "MiB", "GiB", "TiB"];
    let i = 0;
    while (b >= 1024 && i < u.length - 1) {
        b /= 1024;
        i++;
    }
    return i ? `${b.toFixed(1)} ${u[i]}` : `${b} B`;
}
const sizeOf = (e) => (e.bytes == null ? null : `${e.bytes_partial ? "≥ " : ""}${bytes(e.bytes)}`);
/// A worktree went, or something started in it (it's locked while sessions run, and clean-up
/// leaves it be; called before the event is applied): the stale count is asked again, once it
/// was known.
export function staleMaybeChanged(e) {
    if (conn.scope === "full" && (e.t === "worktree_removed" || (e.t === "worktree_updated" && e.info?.locked && data.worktrees.get(e.info.id)?.locked === false)))
        gc.refresh("");
}
/// The Projects tab's 🧹, with how many worktrees are stale once it's known (the TUI's ⚕ says
/// the same): the reminder to clean up, where you'd see it.
function cleanUpButton() {
    const n = conn.scope === "full" ? gc.get("")?.entries.length ?? 0 : 0;
    return h("button", { class: "icon", title: n ? `${n} stale worktree${n === 1 ? "" : "s"}: ${tip("worktree.gc")}` : tip("worktree.gc"), "aria-label": n ? `Clean up ${n}` : "Clean up", onclick: () => go("#/gc") }, n ? `🧹${n}` : "🧹");
}
function gcView() {
    const entries = gc.get("")?.entries;
    const scratch = gc.get("")?.scratch ?? [];
    const total = (entries ?? []).reduce((t, e) => t + (e.bytes ?? 0), 0);
    const done = (msg) => async (req) => { await ask(req, msg); gc.reload(""); };
    // Kept from clean-up (`ptyro gc --keep-wt`): the daemon says until when.
    const keep = async (worktree) => {
        const r = await ask({ t: "gc_keep", worktree });
        if (!r)
            return;
        showNotes("Kept", r.notes ?? []);
        gc.reload("");
    };
    const body = pending(gc, "", "Looking for stale worktrees…") ?? (entries.length
        ? h("div", { class: "rows" }, ...entries.map((e) => row(e.worktree.title || e.worktree.name, `${e.worktree.project_slug} · ${e.reasons.join(", ")}`, sizeOf(e), {
            onclick: () => menu(e.worktree.title || e.worktree.name, [
                { title: tip("worktree.remove"), label: "Remove…", hint: "It refuses to lose work, saying what", danger: true, run: () => remove(e.worktree) },
                { title: tip("worktree.keep"), label: "Keep it", hint: "Clean-up leaves it be until it is unused for another worktrees.stale_days, or its branch moves on", run: () => keep(e.worktree.id) },
                { label: "Open", run: () => go(`#/wt/${enc(e.worktree.id)}`) },
            ])
        })))
        : h("p", { class: "empty" }, "Nothing is stale."));
    // A scratch shell's directory stays when files are left in it: shown here, deleted when asked.
    const left = scratch.length ? section("Left behind by scratch shells", h("div", { class: "rows" }, ...scratch.map((l) => {
        const name = String(l.path).split("/").pop() ?? l.path;
        return row(name, `${l.files} files · changed ${ago(l.modified_ms)} ago`, bytes(l.bytes), {
            onclick: () => confirm(`Delete the scratch directory ${name}?`, `${l.path}, ${l.files} files. A scratch shell left it behind. It is deleted with everything in it; this can't be undone.`, "Delete", () => done("Deleted")({ t: "shell_scratch_remove", path: l.path })),
        });
    }))) : null;
    return h("div", {}, ui.header("Clean up", "#/p", h("button", { class: "icon", onclick: () => gc.reload(""), "aria-label": "Look again" }, "↻")), h("main", { class: "stack", title: tip("worktree.gc") }, h("p", { class: "intro" }, `Worktrees that are merged, abandoned or gone, biggest first${total ? ` (${bytes(total)} in all, build output included)` : ""}. Nothing is removed unless you say so.`), body, left));
}
// ── checkpoints ──────────────────────────────────────────────────────────────────────────────
const checkpoints = new Cache((id) => conn.request({ t: "checkpoint_list", addr: id }).then((r) => r.checkpoints));
function checkpointsTab(w) {
    const cps = checkpoints.get(w.id);
    return pending(checkpoints, w.id) ?? (cps.length ? h("div", { class: "rows", title: tip("checkpoint.list") }, ...[...cps].reverse().map((c) => row(`#${c.n} · turn ${c.turn}`, `${c.files} file${c.files === 1 ? "" : "s"} · ${c.session}`, ago(c.at_ms), { onclick: () => menu(`Checkpoint ${c.n}`, [
            { title: tip("checkpoint.diff"), label: "What changed since", run: async () => { const r = await ask({ t: "checkpoint_diff", addr: c.session, n: c.n, to: null }); if (r)
                    textSheet(`Since checkpoint ${c.n}`, diffText(r.doc ?? r)); } },
            { title: tip("checkpoint.restore"), label: "Put the worktree back to this", danger: true, run: () => confirm(`Restore checkpoint ${c.n}?`, "The worktree goes back to how it was then. Where it is now is checkpointed first, so you can undo this.", "Restore", async () => { await ask({ t: "checkpoint_restore", addr: c.session, n: c.n, turn: null }, "Restored"); checkpoints.reload(w.id); }) },
        ]) }))) : h("p", { class: "empty" }, "No checkpoints yet: one is taken at the end of each agent turn."));
}
/// A diff document as unified text.
export function diffText(doc) {
    if (typeof doc === "string")
        return doc;
    const files = (doc.files ?? []);
    if (!files.length)
        return "No changes.";
    return files.map((f) => `${f.path}${f.old_path ? ` (from ${f.old_path})` : ""}\n${(f.hunks ?? []).map((hk) => `${hk.header ?? ""}\n${(hk.lines ?? []).map((l) => `${l.kind === "added" ? "+" : l.kind === "removed" ? "-" : " "}${l.text}`).join("\n")}`).join("\n")}`).join("\n\n");
}
wtTabs.push({ id: "checkpoints", label: "Checkpoints", render: checkpointsTab });
views.set("p", () => projectsView());
views.set("proj", projectView);
views.set("wt", worktreeView);
views.set("ws", () => workspacesView());
views.set("gc", () => gcView());
