// Runs and change sets (ROADMAP WEB1.5): recipes and fan-outs with their steps, gates and the
// compare-and-pick view, and change sets (one change across repositories) with their contract,
// checks, checkpoints and merge train.

import { conn } from "./conn.js";
import { data } from "./data.js";
import { actions, ago, ask, badge, bar, Cache, confirm, enc, errText, facts, form, go, h, menu, pending, poll, row, section, sheet, textSheet, ui, views } from "./kit.js";
import type { Child, Field, Item } from "./kit.js";
import { allSessions } from "./sessions.js";

const ACTIONS = {
  "runs": "Runs: recipes, fan-outs and leads, with the gates waiting for you",
  "fanout": "Start several agents on one prompt, then compare what they made",
  "compare": "The candidates of a fan-out, ranked, with tests and what each changed",
  "pick": "Take a candidate: merge it, cherry-pick it, open a PR, or apply some files",
  "discard": "Remove a candidate's worktree safely, or all the ones not picked",
  "iterate": "A follow-up for one candidate's agent, or for all of them",
  "run.show": "A run: its steps, what they did and why they ended",
  "run.events": "A run's event log",
  "run.cancel": "Cancel the run",
  "run.retry": "Put a finished, failed or held step back to pending",
  "run.fork": "A new run from a step on, with other variables",
  "run.lead": "Make a session the lead of a new run",
  "recipe.list": "Recipes: built-in, yours and the repository's",
  "recipe.show": "A recipe's source, variables and steps",
  "recipe.run": "Run a recipe: its variables, then the run",
  "gate.list": "Open gates: questions a run waits on you for",
  "gate.resolve": "Answer a gate",
  "sets": "Change sets: one change across several repositories",
  "set.new": "Start a change set: its name, projects, template and goal",
  "set.templates": "Change-set templates, yours and the repositories'",
  "set.show": "A change set: members, gates, what's wrong and what to do about it",
  "set.log": "The set's ledger: events and messages, oldest first",
  "set.message": "Message the set's members or its lead",
  "set.check": "Run the integration and contract checks",
  "set.checkpoint": "Checkpoint every member of the set",
  "set.checkpoints": "The set's checkpoints",
  "set.restore": "Put every member back to a checkpoint",
  "set.diff": "What a member changed",
  "set.contract": "The contract: its text, history and drift",
  "set.contract_edit": "Revise the contract yourself",
  "set.contract_approve": "Approve the contract revision waiting for you: the members start",
  "set.contract_reject": "Send the contract revision back to the lead, with a note",
  "set.overlay": "Wire members to their siblings' working trees (go.work, cargo patch…)",
  "set.commit": "Commit every member",
  "set.push": "Push every member, in dependency order",
  "set.pr": "Open a pull request for every member, linked together",
  "set.land": "Land the set: merge its members in order, one by one",
  "set.add": "Add a project to the set",
  "set.drop": "Take a member out of the set",
  "set.remove": "Remove the set and its worktrees",
} as const;
const tip = actions(ACTIONS);

const rid = (): string => Math.random().toString(36).slice(2, 10);
const asArray = (v: any): any[] => (Array.isArray(v) ? v : []);

const GLYPH: Record<string, string> = { running: "●", waiting: "⚠", succeeded: "✓", failed: "✗", cancelled: "–", pending: "○", skipped: "–", crashed: "✗" };
const KIND_OF: Record<string, string> = { succeeded: "ok", failed: "bad", crashed: "bad", waiting: "warn", running: "info" };
const status = (s: string): HTMLElement => badge(`${GLYPH[s] ?? "·"} ${s}`, KIND_OF[s] ?? "");

// ── gates ───────────────────────────────────────────────────────────────────────────────────

const gates = new Cache<any[]>(() => conn.request({ t: "gate_list", all: false }).then((r) => r.gates));

function resolveGate(g: any, choice: string, withNote = false): void {
  const send = async (note: string | null) => {
    await conn.request({ t: "gate_resolve", id: g.gate.id, choice, note, key: rid(), by: "web" });
    ui.toast(`Answered: ${choice}`);
    gates.refresh(""); runs.refresh(""); run.refresh(g.run); sets.refresh("");
  };
  if (!withNote) { void send(null).catch((e) => ui.toast(errText(e))); return; }
  form(`${choice}: add a note`, [{ key: "note", label: "Note", type: "textarea", rows: 3, placeholder: "It goes back with the answer" }], choice, (v) => send((v.note as string) || null));
}

function gateCard(g: any): HTMLElement {
  return h("section", { class: "prompt gate", title: tip("gate.resolve") },
    h("div", {}, h("b", {}, g.gate.ask), " ", h("a", { class: "sub", href: `#/run/${enc(g.run)}` }, `${g.run} · ${g.step}`)),
    h("div", { class: "actions" },
      ...g.gate.options.map((o: string, i: number) => h("button", { class: i === 0 ? "allow" : /stop|no|reject|deny|cancel/i.test(o) ? "deny" : "", onclick: () => resolveGate(g, o) }, o)),
      h("button", { class: "small", onclick: () => menu("Answer with a note", g.gate.options.map((o: string): Item => ({ label: o, run: () => resolveGate(g, o, true) }))) }, "+ note")));
}

// ── the Runs tab ────────────────────────────────────────────────────────────────────────────

const runs = new Cache<any[]>(() => conn.request({ t: "run_list", active: false, limit: 60 }).then((r) => r.runs));

function newRunMenu(): void {
  menu("Start", [
    { title: tip("fanout"), label: "A fan-out", hint: "Several agents on one prompt, then compare", run: fanoutForm },
    { title: tip("recipe.run"), label: "A recipe", hint: "A declarative multi-step workflow", run: () => go("#/recipes") },
    { title: tip("run.lead"), label: "A lead", hint: "Make a session coordinate workers", run: leadForm },
  ]);
}

function fanoutForm(): void {
  const agents = data.agents.filter((a) => a.installed).map((a) => a.name);
  const projects = [...data.projects.values()];
  if (!projects.length) { ui.toast("Add a project first"); return; }
  form("Fan out", [
    { key: "prompt", label: "Prompt", type: "textarea", rows: 4, required: true, placeholder: "The task every candidate gets" },
    { key: "agents", label: "Agents", value: `${data.defaultAgent}, ${data.defaultAgent}`, required: true, hint: `Comma separated, repeats allowed: ${agents.join(", ")}` },
    { key: "project", label: "Project", type: "select", options: projects.map((p): [string, string] => [p.id, p.slug]) },
    { key: "test", label: "Test command", placeholder: "e.g. cargo test", hint: "Run in each candidate when its agent finishes" },
    { key: "verify", label: "Done when", placeholder: "e.g. cargo test", hint: "A candidate only counts as done once this passes" },
    { key: "judge", label: "Judge", placeholder: "an agent, e.g. claude", hint: "Scores the candidates, advisory" },
    { key: "structured", label: "Chat mode (full tool-call visibility)", type: "check" },
  ], "Start", async (v) => {
    const r = await conn.request({ t: "fanout", spec: {
      agents: (v.agents as string).split(",").map((s) => s.trim()).filter(Boolean), prompt: v.prompt, project: v.project,
      test: v.test || null, verify: v.verify || null, judge: v.judge || null, structured: !!v.structured, base: null, name: null, timeout: null } });
    go(`#/run/${enc(r.run.state.id)}`);
  });
}

function leadForm(): void {
  const live = [...allSessions().values()].filter((s) => s.kind === "agent" && s.state.t === "running");
  if (!live.length) { ui.toast("Start an agent first"); return; }
  form("Lead a run", [
    { key: "session", label: "Session", type: "select", options: live.map((s): [string, string] => [s.id, s.name]) },
    { key: "max", label: "Workers at once", type: "number", value: 4 },
    { key: "agents", label: "Agents it may start", placeholder: "any configured agent", hint: "Comma separated" },
  ], "Make it the lead", async (v) => {
    const r = await conn.request({ t: "lead_start", addr: v.session, policy: { max_sessions: Number(v.max) || 4, max_spawns: null, agents: ((v.agents as string) || "").split(",").map((s) => s.trim()).filter(Boolean), worktrees: ["new", "own"], destructive: false } });
    go(`#/run/${enc(r.run.state.id)}`);
  });
}

function runCard(r: any): HTMLElement {
  const s = r.state;
  const done = r.order.filter((k: string) => s.steps[k]?.status === "succeeded").length;
  return row(h("span", {}, s.name, " ", status(s.status), s.kind !== "recipe" ? badge(s.kind) : null),
    `${s.id} · ${done}/${r.order.length} steps${s.error ? ` · ${s.error}` : ""}`, ago(s.ended_ms ?? s.created_ms), { href: `#/run/${enc(s.id)}` });
}

function runsView(): HTMLElement {
  poll("runs", () => { runs.refresh(""); gates.refresh(""); });
  const list = runs.get("");
  const open = gates.get("") ?? [];
  const mine = list?.filter((r) => r.state.kind !== "changeset");
  return h("div", {}, ui.header("Runs", false,
    h("button", { class: "icon", title: tip("recipe.list"), "aria-label": "Recipes", onclick: () => go("#/recipes") }, "📜"),
    h("button", { class: "icon plus", title: tip("fanout"), "aria-label": "Start a run", onclick: newRunMenu }, "＋")),
    h("main", { class: "list", title: tip("runs") },
      open.length ? h("div", { class: "gates", title: tip("gate.list") }, ...open.map(gateCard)) : null,
      pending(runs, "") ?? (mine!.length ? h("div", { class: "rows" }, ...mine!.map(runCard)) : h("div", { class: "empty" }, h("p", {}, "No runs yet."), h("button", { class: "primary", onclick: newRunMenu }, "Start one")))));
}

// ── a run ───────────────────────────────────────────────────────────────────────────────────

const run = new Cache<any>((id) => conn.request({ t: "run_get", id }).then((r) => r.run));

function stepMenu(r: any, k: string): void {
  const st = r.state.steps[k];
  const a = st.attempts[st.attempts.length - 1];
  menu(k, [
    a?.session && { label: "Open its session", run: () => go(`#/s/${enc(a.session)}`) },
    a?.worktree && { label: "Open its worktree", run: () => go(`#/wt/${enc(a.worktree)}`) },
    ["succeeded", "failed", "crashed", "waiting", "cancelled"].includes(st.status) && { title: tip("run.retry"), label: "Run it again", hint: "A fresh attempt", run: async () => { await ask({ t: "run_retry", id: r.state.id, step: k }, `${k} runs again`); run.refresh(r.state.id); } },
    { title: tip("run.fork"), label: "Fork the run from here", hint: "Keeps the steps before it", run: () => forkForm(r, k) },
    { label: "Details", run: () => textSheet(`${k}`, JSON.stringify({ status: st.status, reason: st.reason, decided_by: st.decided_by, outputs: st.outputs, attempts: st.attempts.map((x: any) => ({ n: x.n, session: x.session, ended: x.ended_ms })) }, null, 2)) },
  ]);
}

function forkForm(r: any, from: string): void {
  form(`Fork from ${from}`, [{ key: "set", label: "Variables to change", type: "textarea", rows: 3, placeholder: "name=value, one per line", hint: `Run vars now: ${Object.entries(r.state.vars).map(([a, b]) => `${a}=${b}`).join(", ") || "none"}` }], "Fork", async (v) => {
    const set: Record<string, string> = {};
    for (const line of ((v.set as string) || "").split("\n")) { const m = line.match(/^\s*([^=\s]+)\s*=\s*(.*)$/); if (m) set[m[1]] = m[2]; }
    const f = await conn.request({ t: "run_fork", id: r.state.id, from, set });
    go(`#/run/${enc(f.run.state.id)}`);
  });
}

const runEvents = new Cache<any[]>((id) => conn.request({ t: "run_events", id }).then((r) => r.events));

function runView(rest: string): HTMLElement {
  const id = decodeURIComponent(rest);
  if (id.endsWith("/events")) return runEventsView(id.slice(0, -7));
  poll("run", () => run.refresh(id));
  const r = run.get(id);
  const body = pending(run, id) ?? (() => {
    const s = r.state;
    const myGates = (gates.get("") ?? []).filter((g) => g.run === id);
    return h("div", { class: "stack-in" },
      myGates.map(gateCard).length ? h("div", {}, ...myGates.map(gateCard)) : null,
      h("div", { class: "pr-head" }, status(s.status), badge(s.kind), s.forked_from ? h("span", { class: "sub" }, `forked from ${s.forked_from[0]}`) : null),
      s.error ? h("p", { class: "form-err" }, s.error) : null,
      bar(
        s.kind === "fanout" || s.picked?.length || s.discarded?.length ? h("button", { class: "primary", title: tip("compare"), onclick: () => go(`#/compare/${enc(id)}`) }, "Compare") : null,
        h("button", { title: tip("run.events"), onclick: () => go(`#/run/${enc(id)}/events`) }, "Events"),
        s.status === "running" || s.status === "waiting" ? h("button", { class: "deny", title: tip("run.cancel"), onclick: () => confirm("Cancel the run?", "Its steps stop; sessions it started are left running.", "Cancel it", async () => { await ask({ t: "run_cancel", id }, "Cancelled"); run.refresh(id); }) }, "Cancel") : null,
        s.changeset ? h("button", { onclick: () => go(`#/set/${enc(s.changeset.name)}`) }, "Open the change set") : null),
      section("Steps", h("div", { class: "rows", title: tip("run.show") }, ...r.order.map((k: string) => {
        const st = s.steps[k] ?? { status: "pending", attempts: [] };
        const a = st.attempts[st.attempts.length - 1];
        return row(h("span", {}, k, " ", status(st.status)), [st.reason, st.decided_by ? `by ${st.decided_by}` : "", st.attempts.length > 1 ? `${st.attempts.length} attempts` : ""].filter(Boolean).join(" · ") || (a?.session ? "has a session" : ""), st.started_ms ? ago(st.ended_ms ?? st.started_ms) : "", { onclick: () => stepMenu(r, k) });
      }))),
      Object.keys(s.vars).length ? section("Variables", facts(Object.entries(s.vars) as [string, Child][])) : null,
      r.children.length ? section("Forks", h("div", { class: "rows" }, ...r.children.map((c: string) => row(c, "", null, { href: `#/run/${enc(c)}` })))) : null);
  })();
  return h("div", {}, ui.header(r?.state.name ?? "Run", "#/runs"), h("main", { class: "stack" }, body));
}

function runEventsView(id: string): HTMLElement {
  const evs = runEvents.get(id);
  return h("div", {}, ui.header("Run events", `#/run/${enc(id)}`, h("button", { class: "icon", onclick: () => runEvents.reload(id), "aria-label": "Refresh" }, "↻")),
    h("main", { class: "stack", title: tip("run.events") }, pending(runEvents, id) ?? h("div", { class: "timeline" }, ...[...evs!].reverse().map(([seq, at, e]) => {
      const { e: kind, step, at: _at, ...rest } = e as any; void _at;
      return h("div", { class: "tl" }, h("time", {}, new Date(at).toLocaleTimeString()), h("b", {}, `${kind ?? "?"}${step ? ` ${step}` : ""}`), h("span", {}, `${seq} ${Object.keys(rest).length ? JSON.stringify(rest).slice(0, 200) : ""}`));
    }))));
}

// ── compare and pick ────────────────────────────────────────────────────────────────────────

const compare = new Cache<any>((id) => conn.request({ t: "compare", run: id, refresh: false }).then((r) => r.doc));

function pickMenu(id: string, c: any): void {
  const pick = (mode: string, into: string | null = null) => async () => {
    const r = await ask({ t: "pick", run: id, candidate: c.step, mode, into, paths: [], hunks: [] });
    if (r) { for (const n of r.notes ?? []) ui.toast(n); compare.reload(id); }
  };
  menu(`Take ${c.step}`, [
    { label: "Merge it", hint: "Rebase onto the main branch and fast-forward", run: pick("merge") },
    { label: "Cherry-pick its commits", run: pick("cherry_pick") },
    { label: "Open a pull request", run: pick("pr") },
    { label: "Apply some of its files…", hint: "Copied in, uncommitted", run: () => applyForm(id, c) },
  ]);
}

function applyForm(id: string, c: any): void {
  form(`Apply from ${c.step}`, [{ key: "paths", label: "Files", type: "textarea", rows: 4, required: true, placeholder: "one path per line" }, { key: "into", label: "Into the worktree", placeholder: "default: the project's main checkout" }], "Apply", async (v) => {
    await conn.request({ t: "pick", run: id, candidate: c.step, mode: "apply", into: (v.into as string) || null, paths: (v.paths as string).split("\n").map((s) => s.trim()).filter(Boolean), hunks: [] });
    ui.toast("Applied"); compare.reload(id);
  });
}

function iterateForm(id: string, c?: any): void {
  form(c ? `Follow up with ${c.step}` : "Follow up with every candidate", [{ key: "text", label: "Message", type: "textarea", required: true, rows: 3 }], "Send", async (v) => {
    await conn.request({ t: "iterate", run: id, candidate: c?.step ?? null, text: v.text });
    ui.toast("Sent");
  });
}

function candidateCard(id: string, c: any): HTMLElement {
  const sessionId = c.session;
  return h("section", { class: `cand ${c.picked ? "picked" : ""} ${c.discarded ? "dim" : ""}` },
    h("div", { class: "cand-head" }, h("b", {}, `#${c.rank} ${c.step}`), " ", c.agent ? badge(c.agent) : null, " ", status(c.status), c.picked ? badge("picked", "ok") : null, c.discarded ? badge("discarded") : null, c.verified === true ? badge("verified", "ok") : c.verified === false ? badge("not verified", "bad") : null),
    h("div", { class: "sub" }, `${c.files} file${c.files === 1 ? "" : "s"} · +${c.added} −${c.removed} · ${c.hunks} hunks · ${Math.round(c.duration_ms / 1000)}s${c.tests != null ? ` · tests ${c.tests === 0 ? "✓" : `✗ (${c.tests})`}` : ""}${c.broke_tests ? " · broke tests" : ""}${c.judge != null ? ` · judge ${c.judge}/10` : ""}`),
    c.reasons?.length ? h("div", { class: "sub" }, c.reasons.slice(0, 3).join(" · ")) : null,
    c.last_message ? h("div", { class: "detail" }, c.last_message) : null,
    !c.discarded ? bar(
      h("button", { class: "small primary", title: tip("pick"), onclick: () => pickMenu(id, c) }, "Take"),
      h("button", { class: "small", title: tip("iterate"), onclick: () => iterateForm(id, c) }, "Follow up"),
      c.worktree ? h("button", { class: "small", onclick: () => go(`#/wt/${enc(c.worktree)}/changes`) }, "Changes") : null,
      sessionId ? h("button", { class: "small", onclick: () => go(`#/s/${enc(sessionId)}`) }, "Session") : null,
      h("button", { class: "small", title: tip("discard"), onclick: () => confirm(`Discard ${c.step}?`, "Its worktree is removed, if that loses nothing.", "Discard", async () => { await ask({ t: "discard", run: id, candidate: c.step, force: false }, "Discarded"); compare.reload(id); }) }, "Discard")) : null);
}

function compareView(rest: string): HTMLElement {
  const id = decodeURIComponent(rest);
  poll("compare", () => compare.refresh(id), 6000);
  const d = compare.get(id);
  const body = pending(compare, id, "Comparing the candidates…") ?? h("div", { class: "stack-in", title: tip("compare") },
    ...(d.warnings as string[]).map((w) => h("p", { class: "form-err" }, w)),
    h("div", { class: "sub" }, `against ${d.base ?? "the main branch"}${d.base_tests != null ? ` (its tests ${d.base_tests === 0 ? "pass" : "fail"})` : ""}`),
    bar(h("button", { title: tip("iterate"), onclick: () => iterateForm(id) }, "Follow up with all"),
      h("button", { title: tip("discard"), onclick: () => confirm("Discard the rest?", "Every candidate that wasn't picked has its worktree removed, if that loses nothing.", "Discard them", async () => { await ask({ t: "discard", run: id, candidate: null, force: false }, "Discarded"); compare.reload(id); }) }, "Discard the rest"),
      h("button", { class: "icon", "aria-label": "Run the evidence again", onclick: () => { conn.request({ t: "compare", run: id, refresh: true }).then((r) => { compare.set(id, r.doc); ui.render(); }, (e) => ui.toast(errText(e))); } }, "↻")),
    ...(d.candidates as any[]).map((c) => candidateCard(id, c)),
    d.top_difference ? section("Between the top two", h("pre", { class: "block" }, d.top_difference)) : null);
  return h("div", {}, ui.header("Compare", `#/run/${enc(id)}`), h("main", { class: "stack" }, body));
}

// ── recipes ─────────────────────────────────────────────────────────────────────────────────

const recipes = new Cache<any[]>(() => conn.request({ t: "recipe_list", project: null }).then((r) => r.recipes));

/// `name (required)` / `name = default` → a form field.
function recipeVar(v: string): Field {
  const m = v.match(/^(\S+)\s*(\(required\)|=\s*(.*))?$/);
  const name = m?.[1] ?? v;
  const required = m?.[2] === "(required)";
  return { key: name, label: name, value: m?.[3] ?? "", required, hint: required ? "required" : m?.[3] ? `default: ${m[3]}` : undefined };
}

function runRecipe(info: any): void {
  const project = [...data.projects.values()];
  const go1 = async (vars: Record<string, string>, trust: string | null, projectId: string | null) => {
    const r = await conn.request({ t: "recipe_run", name: info.name, src: null, vars, project: projectId, base: null, trust });
    go(`#/run/${enc(r.run.state.id)}`);
  };
  form(`Run ${info.name}`, [
    ...(info.vars as string[]).map(recipeVar),
    ...(project.length ? [{ key: "@project", label: "Project", type: "select", options: [["", "(none)"], ...project.map((p): [string, string] => [p.id, p.slug])] } as Field] : []),
  ], "Run", async (v) => {
    const vars: Record<string, string> = {};
    let projectId: string | null = null;
    for (const [k, val] of Object.entries(v)) { if (k === "@project") projectId = (val as string) || null; else if (val !== "") vars[k] = String(val); }
    try { await go1(vars, null, projectId); } catch (e) {
      if (info.untrusted) { await new Promise<void>((ok, bad) => { confirm("Trust this recipe?", `${errText(e)} It's from the repository, and runs commands on your computer.`, "Trust and run", () => go1(vars, info.untrusted, projectId).then(ok, bad), false); }); } else throw e;
    }
  });
}

function recipesView(): HTMLElement {
  const list = recipes.get("");
  return h("div", {}, ui.header("Recipes", "#/runs"), h("main", { class: "stack", title: tip("recipe.list") },
    pending(recipes, "") ?? (list!.length ? h("div", { class: "rows" }, ...list!.map((r) => row(
      h("span", {}, r.name, " ", badge(r.source), r.untrusted ? badge("untrusted", "warn") : null, r.error ? badge("error", "bad") : null),
      r.description ?? `${r.steps} steps`, null,
      { onclick: () => menu(r.name, [
        { title: tip("recipe.run"), label: "Run it…", run: () => runRecipe(r) },
        { title: tip("recipe.show"), label: "Show its source", run: async () => { const x = await ask({ t: "recipe_show", name: r.name, project: null }); if (x) textSheet(r.name, x.src); } },
      ]) }))) : h("p", { class: "empty" }, "No recipes."))));
}

// ── change sets ─────────────────────────────────────────────────────────────────────────────

const sets = new Cache<any[]>(() => conn.request({ t: "cs", op: { op: "list" } }).then((r) => asArray(r.value)));
const set = new Cache<any>((name) => conn.request({ t: "cs", op: { op: "show", set: name } }).then((r) => r.value));

const csOp = async (op: any, ok?: string): Promise<any | undefined> => { const r = await ask({ t: "cs", op }, ok); return r?.value; };

const headline = (i: any): string => {
  const c = i.set;
  const head = [...(c.contract ?? [])].pop();
  const appr = [...(c.contract ?? [])].reverse().find((x: any) => x.approved);
  const contract = !head ? "no contract yet" : appr && appr.n === head.n ? `contract r${head.n}` : appr ? `contract r${head.n} (approved r${appr.n})` : `contract r${head.n} (not approved)`;
  return `${i.report.state} · ${i.report.ready[0]}/${i.report.ready[1]} ready · ${contract}${i.job ? ` · ${i.job} running` : ""}`;
};

function newSet(): void {
  const projects = [...data.projects.values()];
  const picks = new Map<string, HTMLInputElement>();
  const name = h("input", { placeholder: "e.g. add-user-tier", autocapitalize: "off", spellcheck: "false" });
  const goal = h("textarea", { rows: 3, placeholder: "What the change is, across the repositories" });
  const tpl = h("select", {}, h("option", { value: "" }, "No template"));
  const layout = h("select", {}, ...[["lead", "A lead agent drafts the contract"], ["solo", "One agent, every member"], ["manual", "Nobody: you drive"]].map(([v, l]) => h("option", { value: v }, l)));
  const err = h("p", { class: "form-err" });
  const s = sheet("New change set", [
    h("label", { class: "field" }, h("span", {}, "Name"), name),
    h("div", { class: "field" }, h("span", {}, "Projects"), ...projects.map((p) => { const c = h("input", { type: "checkbox" }); picks.set(p.id, c); return h("label", { class: "field check" }, c, h("span", {}, p.slug)); })),
    h("label", { class: "field" }, h("span", {}, "Goal"), goal),
    h("label", { class: "field" }, h("span", {}, "Template"), tpl),
    h("label", { class: "field" }, h("span", {}, "Layout"), layout), err,
  ], [h("button", { onclick: () => s.close() }, "Cancel"), h("button", { class: "primary", onclick: async () => {
    const chosen = [...picks].filter(([, c]) => c.checked).map(([p]) => p);
    if (!name.value.trim()) { err.textContent = "Name it."; return; }
    if (!chosen.length && !tpl.value) { err.textContent = "Pick the projects, or a template."; return; }
    const spec = (trust: string | null) => ({ name: name.value.trim(), projects: chosen, template: tpl.value || null, lead: null, layout: layout.value, goal: goal.value.trim() || null, from_issue: null, depends: {}, agents: {}, bases: {}, gates: null, integration: [], workspace: null, overlay: false, env: {}, trust });
    try {
      let r: any;
      try { r = await conn.request({ t: "cs", op: { op: "new", spec: spec(null) } }); } catch (e) {
        const fp = (e as { code?: string }).code === "untrusted_project" ? (e as Error).message.split("--trust ").pop() : null;
        if (!fp) throw e;
        const yes = await new Promise<boolean>((ok) => confirm("Trust this template?", (e as Error).message, "Trust and go on", () => ok(true), false).el.addEventListener("mousedown", () => setTimeout(() => ok(false), 0)));
        if (!yes) return;
        r = await conn.request({ t: "cs", op: { op: "new", spec: spec(fp.trim()) } });
      }
      s.close(); sets.reload(""); go(`#/set/${enc(r.value.set.name)}`);
    } catch (e) { err.textContent = errText(e); }
  } }, "Start")]);
  conn.request({ t: "cs", op: { op: "templates" } }).then((r) => { for (const t of asArray(r.value)) tpl.append(h("option", { value: t.name }, `${t.name} — ${t.members.join(", ")}`)); }, () => {});
}

function templatesSheet(): void {
  conn.request({ t: "cs", op: { op: "templates" } }).then((r) => {
    const list = asArray(r.value);
    sheet("Templates", [list.length ? h("div", { class: "rows", title: tip("set.templates") }, ...list.map((t) => row(h("span", {}, t.name, " ", badge(t.source), t.untrusted ? badge("untrusted", "warn") : null), `${t.members.join(", ")}${t.description ? ` — ${t.description}` : ""}`, null))) : h("p", { class: "empty" }, "No templates: add them in ~/.config/ptyro/changesets/*.toml or a repository's .ptyro/changesets/.")]);
  }, (e) => ui.toast(errText(e)));
}

function setsView(): HTMLElement {
  poll("sets", () => sets.refresh(""));
  const list = sets.get("");
  return h("div", {}, ui.header("Change sets", false,
    h("button", { class: "icon", title: tip("set.templates"), "aria-label": "Templates", onclick: templatesSheet }, "📋"),
    h("button", { class: "icon plus", title: tip("set.new"), "aria-label": "New change set", onclick: newSet }, "＋")),
    h("main", { class: "list", title: tip("sets") }, ...(gates.get("") ?? []).filter((g) => /contract|push|land/.test(g.step)).map(gateCard),
      pending(sets, "") ?? (list!.length ? h("div", { class: "rows" }, ...list!.map((i) => row(h("span", {}, i.set.name, " ", badge(i.set.phase), i.report.issues.length ? badge(`${i.report.issues.length} issue${i.report.issues.length === 1 ? "" : "s"}`, "bad") : null), headline(i), null, { href: `#/set/${enc(i.set.name)}` })))
        : h("div", { class: "empty" }, h("p", {}, "No change sets: one change across several repositories."), h("button", { class: "primary", onclick: newSet }, "Start one")))));
}

const MARK: Record<string, string> = { ready: "✓", landed: "✓", working: "●", failed: "✗", conflict: "✗", missing: "✗", stale: "!" };
const markOf = (w: string): string => MARK[w] ?? (w.startsWith("blocked") ? "◐" : "·");

function setMemberMenu(i: any, m: any): void {
  const name = i.set.name;
  menu(m.name, [
    i.sessions[m.name] && { label: "Open its agent", run: () => go(`#/s/${enc(i.sessions[m.name])}`) },
    m.worktree && { label: "Its worktree", run: () => go(`#/wt/${enc(m.worktree)}`) },
    { title: tip("set.diff"), label: "What it changed", run: async () => { const v = await csOp({ op: "diff", set: name, member: m.name }); if (v) textSheet(`${m.name}: changes`, v.diff || "No changes."); } },
    { title: tip("set.message"), label: "Message it…", run: () => msgSet(name, m.name) },
    { title: tip("set.overlay"), label: "Wire it to its siblings…", run: () => overlaySet(name, m.name) },
    m.pr_url && { label: `Pull request #${m.pr}`, run: () => window.open(m.pr_url, "_blank", "noreferrer") },
    { title: tip("set.drop"), label: "Take it out of the set", danger: true, run: () => confirm(`Drop ${m.name}?`, "It leaves the set; its worktree stays unless it can't.", "Drop", async () => { await csOp({ op: "drop", set: name, member: m.name, force: false }, "Dropped"); set.reload(name); }) },
  ]);
}

function msgSet(name: string, to?: string): void {
  form(`Message ${to ?? "the set"}`, [
    { key: "to", label: "To", value: to ?? "@all", hint: "A member, @lead or @all" },
    { key: "text", label: "Message", type: "textarea", required: true },
    { key: "interrupt", label: "Interrupt a working agent", type: "check" },
  ], "Send", async (v) => { const r = await conn.request({ t: "cs", op: { op: "msg", set: name, to: v.to, text: v.text, kind: null, interrupt: !!v.interrupt } }); ui.toast(asArray(r.value?.to).length ? `Sent to ${r.value.to.join(", ")}` : "Not sent (a repeat, or the queue is full)"); });
}

function overlaySet(name: string, member?: string): void {
  form("Wire to siblings", [{ key: "mode", label: "Mode", type: "select", options: [["auto", "Automatic"], ["go", "Go (go.work)"], ["rust", "Rust (cargo patch)"], ["node", "Node"], ["python", "Python"], ["compose", "Compose"], ["off", "Off"]] }], "Apply", async (v) => {
    const r = await conn.request({ t: "cs", op: { op: "overlay", set: name, member: member ?? null, mode: v.mode } });
    const notes = asArray(r.value?.notes);
    ui.toast(notes[0] ?? "Nothing to wire: no member consumes a sibling in that ecosystem");
    set.reload(name);
  });
}

async function checkSet(name: string): Promise<void> {
  const b = sheet("Checking…", [h("p", { class: "intro" }, "⏳ Running the integration and contract checks.")]);
  try {
    const r = await conn.request({ t: "cs", op: { op: "check", set: name } });
    b.close();
    const v = r.value;
    const line = (x: any, label: string) => `${x.ok ? "✓" : "✗"} ${label}: ${x.command} (${x.duration_ms} ms)${x.ok ? "" : `\n${String(x.tail ?? "").split("\n").slice(-12).map((l) => `    ${l}`).join("\n")}`}`;
    const out = [...asArray(v.integration).map((x) => line(x, "integration")), ...Object.entries(v.members ?? {}).map(([m, x]) => line(x, m))];
    textSheet(v.ok ? "Checks pass" : "Checks failed", out.join("\n") || "No checks are defined (a template's integration and contract_checks, or `cs new --integration`).");
    set.reload(name);
  } catch (e) { b.close(); ui.toast(errText(e)); }
}

function shipMenu(i: any): void {
  const name = i.set.name;
  menu("Ship the set", [
    { title: tip("set.commit"), label: "Commit every member…", run: () => form("Commit the set", [{ key: "message", label: "Message", type: "textarea", required: true, rows: 3 }], "Commit", async (v) => { const r = await conn.request({ t: "cs", op: { op: "commit", set: name, message: v.message } }); ui.toast(`Committed ${asArray(r.value?.committed).length} member(s)`); set.reload(name); }) },
    { title: tip("set.push"), label: "Push, in dependency order", run: async () => { const v = await csOp({ op: "push", set: name, yes: false }); if (v) { ui.toast(v.gate ? "Waiting for your approval below" : `Pushed: ${asArray(v.pushed).join(", ") || "none"}`); set.reload(name); } } },
    { title: tip("set.pr"), label: "Open the pull requests", run: async () => { const v = await csOp({ op: "pr", set: name, draft: false }); if (v) { textSheet("Pull requests", asArray(v.opened).map((x) => `${x.member}  #${x.number}  ${x.url}`).join("\n") || "None opened."); set.reload(name); } } },
    { title: tip("set.land"), label: "Land the set", hint: "Merge the members one by one, in order", run: () => confirm("Land the set?", `Merges ${(i.set.train?.order ?? i.set.members.map((m: any) => m.name)).join(" → ")}, stopping at the first that fails.`, "Land it", async () => { const v = await csOp({ op: "land", set: name, yes: false }); if (v) { ui.toast(`Landing ${asArray(v.landing).join(" → ")}`); set.reload(name); } }, false) },
  ]);
}

function setView(rest: string): HTMLElement {
  const [raw, sub] = rest.split("/");
  const name = decodeURIComponent(raw);
  if (sub === "contract") return contractView(name);
  if (sub === "log") return ledgerView(name);
  if (sub === "checkpoints") return setCheckpointsView(name);
  poll("set", () => set.refresh(name));
  const i = set.get(name);
  const body = pending(set, name) ?? (() => {
    const c = i.set;
    const myGates = i.gates as any[];
    return h("div", { class: "stack-in", title: tip("set.show") },
      ...myGates.map(gateCard),
      h("div", { class: "pr-head" }, badge(i.report.state, i.report.issues.length ? "bad" : "info"), badge(c.phase), badge(c.layout), i.job ? badge(`${i.job} running`, "warn") : null),
      c.goal ? h("p", { class: "intro" }, c.goal.split("\n")[0]) : null,
      bar(
        h("button", { class: "primary", title: tip("set.contract"), onclick: () => go(`#/set/${enc(name)}/contract`) }, "Contract"),
        h("button", { title: tip("set.check"), onclick: () => void checkSet(name) }, "Check"),
        h("button", { onclick: () => shipMenu(i) }, "Ship…"),
        h("button", { title: tip("set.message"), onclick: () => msgSet(name) }, "Message"),
        h("button", { class: "icon", "aria-label": "More", onclick: () => menu(name, [
          { title: tip("set.log"), label: "Ledger", run: () => go(`#/set/${enc(name)}/log`) },
          { title: tip("set.checkpoint"), label: "Checkpoint now", run: async () => { const v = await csOp({ op: "checkpoint", set: name, label: null }); if (v) { ui.toast(`Checkpoint #${v.n}`); set.reload(name); } } },
          { title: tip("set.checkpoints"), label: "Checkpoints", run: () => go(`#/set/${enc(name)}/checkpoints`) },
          { title: tip("set.overlay"), label: "Wire members to each other…", run: () => overlaySet(name) },
          i.lead && { label: "Open the lead", run: () => go(`#/s/${enc(i.lead)}`) },
          { title: tip("set.add"), label: "Add a project…", run: () => addMember(name) },
          { title: tip("set.remove"), label: "Remove the set", danger: true, run: () => confirm(`Remove ${name}?`, "Its worktrees are removed, unless they hold work: then it says so.", "Remove", async () => { const v = await csOp({ op: "rm", set: name, keep: [], force: false }, "Removed"); if (v) go("#/sets"); }) },
        ]) }, "⋯")),
      ...(i.report.issues as any[]).map((x) => h("section", { class: "issue" }, h("b", {}, `✗ ${x.state.replace(/_/g, " ")}${x.member ? ` · ${x.member}` : ""}`), h("div", {}, x.detail), h("div", { class: "sub" }, `→ ${x.action}`))),
      section("Members", h("div", { class: "rows" }, ...(c.members as any[]).map((m) => {
        const word = i.report.members[m.name] ?? "";
        const live = i.live[m.name] ?? {};
        const bits = [live.ahead ? `+${live.ahead} commit(s)` : "", live.dirty ? "uncommitted" : "", m.pr ? `PR #${m.pr}` : "", m.overlay ? `overlay ${m.overlay}` : "", m.landed ?? ""].filter(Boolean).join(", ");
        return row(h("span", {}, markOf(word), " ", m.name, " ", badge(word || "idle")), `${data.projects.get(m.project)?.slug ?? m.project} · ${m.branch}${bits ? ` · ${bits}` : ""}`, m.depends_on?.length ? `after ${m.depends_on.join(", ")}` : "", { onclick: () => setMemberMenu(i, m) });
      }))));
  })();
  return h("div", {}, ui.header(name, "#/sets"), h("main", { class: "stack" }, body));
}

function addMember(name: string): void {
  form("Add a project", [
    { key: "project", label: "Project", type: "select", options: [...data.projects.values()].map((p): [string, string] => [p.id, p.slug]) },
    { key: "agent", label: "Agent", placeholder: "default" },
    { key: "after", label: "Depends on", placeholder: "members, comma separated" },
  ], "Add", async (v) => { await conn.request({ t: "cs", op: { op: "add", set: name, project: v.project, agent: (v.agent as string) || null, depends_on: ((v.after as string) || "").split(",").map((s) => s.trim()).filter(Boolean) } }); set.reload(name); });
}

// the contract
const contract = new Cache<any>((name) => conn.request({ t: "cs", op: { op: "contract", set: name, action: { do: "show", rev: null } } }).then((r) => r.value));
const contractLog = new Cache<any[]>((name) => conn.request({ t: "cs", op: { op: "contract", set: name, action: { do: "history" } } }).then((r) => asArray(r.value)));

function contractView(name: string): HTMLElement {
  const c = contract.get(name);
  const hist = contractLog.get(name);
  const i = set.get(name);
  const waiting = (i?.gates as any[] | undefined)?.find((g) => g.step.startsWith("contract-r"));
  const body = pending(contract, name) ?? h("div", { class: "stack-in", title: tip("set.contract") },
    waiting ? h("section", { class: "prompt gate" }, h("b", {}, `${waiting.step.replace("contract-", "Contract ")} waits for you`),
      h("div", { class: "actions" },
        h("button", { class: "allow", title: tip("set.contract_approve"), onclick: () => resolveGate(waiting, "approve") }, "Approve"),
        h("button", { class: "deny", title: tip("set.contract_reject"), onclick: () => resolveGate(waiting, "revise", true) }, "Send back…"))) : null,
    h("div", { class: "pr-head" }, c.draft ? badge("draft: no revision yet") : badge(`r${c.n} by ${c.by}`), c.approved ? badge(`approved by ${c.approved}`, "ok") : null),
    bar(
      h("button", { class: "primary", title: tip("set.contract_edit"), onclick: () => form("Revise the contract", [{ key: "text", label: "CONTRACT.md", type: "textarea", rows: 16, value: c.text ?? "", required: true }, { key: "summary", label: "What changed", value: "edited by the human" }], "Save a revision", async (v) => { await conn.request({ t: "cs", op: { op: "contract", set: name, action: { do: "revise", text: v.text, diff: null, affects: [], summary: v.summary || null } } }); contract.reload(name); contractLog.reload(name); set.reload(name); }) }, "Edit"),
      h("button", { onclick: async () => { const v = await csOp({ op: "contract", set: name, action: { do: "diff", a: null, b: null, file: null } }); if (v) textSheet(v.same ? "No drift" : "Revision to revision", v.same ? `The file matches contract r${v.a}.` : v.diff); } }, "Diff"),
      h("button", { class: "icon", "aria-label": "Refresh", onclick: () => { contract.reload(name); contractLog.reload(name); } }, "↻")),
    h("pre", { class: "block" }, c.text ?? ""),
    section("History", pending(contractLog, name) ?? h("div", { class: "rows" }, ...hist!.map((r) => row(`r${r.n} · by ${r.by}`, `${r.summary ?? ""}${r.affects?.length ? ` · affects ${r.affects.join(", ")}` : ""}`, r.approved ? badge("approved", "ok") : ago(r.at_ms), { onclick: async () => { const v = await csOp({ op: "contract", set: name, action: { do: "show", rev: r.n } }); if (v) textSheet(`Contract r${r.n}`, v.text); } })))));
  return h("div", {}, ui.header("Contract", `#/set/${enc(name)}`), h("main", { class: "stack" }, body));
}

const ledger = new Cache<any[]>((name) => conn.request({ t: "cs", op: { op: "log", set: name, since: null } }).then((r) => asArray(r.value)));

function ledgerView(name: string): HTMLElement {
  poll("ledger", () => ledger.refresh(name), 3000);
  const l = ledger.get(name);
  return h("div", {}, ui.header("Ledger", `#/set/${enc(name)}`), h("main", { class: "stack", title: tip("set.log") },
    pending(ledger, name) ?? h("div", { class: "timeline" }, ...[...l!].reverse().map((e) => h("div", { class: "tl" }, h("time", {}, new Date(e.at_ms).toLocaleTimeString()),
      h("b", {}, e.t === "message" ? `${e.from ?? ""} → ${e.to ?? ""}` : e.kind), h("span", {}, e.text))))));
}

function setCheckpointsView(name: string): HTMLElement {
  const i = set.get(name);
  const cps: any[] = i?.set.checkpoints ?? [];
  return h("div", {}, ui.header("Set checkpoints", `#/set/${enc(name)}`), h("main", { class: "stack", title: tip("set.checkpoints") },
    pending(set, name) ?? (cps.length ? h("div", { class: "rows" }, ...[...cps].reverse().map((k) => row(`#${k.n} ${k.label ?? ""}`, `${k.reason} · ${Object.keys(k.members).length} members`, ago(k.at_ms), {
      onclick: () => menu(`Checkpoint ${k.n}`, [{ title: tip("set.restore"), label: "Put every member back to this", danger: true, run: () => confirm(`Restore checkpoint ${k.n}?`, "Every member's worktree goes back to how it was then.", "Restore", async () => { const v = await csOp({ op: "restore", set: name, n: k.n, force: false }); if (v) { ui.toast(asArray(v.notes)[0] ?? "Restored"); set.reload(name); } }) }]) })))
      : h("p", { class: "empty" }, "No checkpoints yet."))));
}

views.set("runs", () => runsView());
views.set("run", runView);
views.set("compare", compareView);
views.set("recipes", () => recipesView());
views.set("sets", () => setsView());
views.set("set", setView);
