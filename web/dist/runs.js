// Runs (ROADMAP WEB1.5): recipes and fan-outs with their steps, gates and the compare-and-pick
// view.
import { conn } from "./conn.js";
import { data } from "./data.js";
import { actions, NAME_HINT, ago, ask, badge, bar, Cache, confirm, cycle, enc, errText, facts, form, go, h, menu, pending, poll, row, safeDecode, section, textSheet, ui, views } from "./kit.js";
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
};
const tip = actions(ACTIONS);
const GLYPH = { running: "●", waiting: "⚠", succeeded: "✓", failed: "✗", cancelled: "–", pending: "○", skipped: "–", crashed: "✗" };
const KIND_OF = { succeeded: "ok", failed: "bad", crashed: "bad", waiting: "warn", running: "info" };
const status = (s) => badge(`${GLYPH[s] ?? "·"} ${s}`, KIND_OF[s] ?? "");
// ── gates ───────────────────────────────────────────────────────────────────────────────────
const gates = new Cache(() => conn.request({ t: "gate_list", all: false }).then((r) => r.gates));
function resolveGate(g, choice, withNote = false) {
    const send = async (note) => {
        await conn.request({ t: "gate_resolve", id: g.gate.id, choice, note, key: `${g.gate.id}:${choice}`, by: "web" });
        ui.toast(`Answered: ${choice}`);
        gates.refresh("");
        runs.refresh("");
        run.refresh(g.run);
    };
    if (!withNote) {
        void send(null).catch((e) => ui.toast(errText(e)));
        return;
    }
    form(`${choice}: add a note`, [{ key: "note", label: "Note", type: "textarea", rows: 3, placeholder: "It goes back with the answer" }], choice, (v) => send(v.note || null));
}
function gateCard(g) {
    return h("section", { class: "prompt gate", title: tip("gate.resolve") }, h("div", {}, h("b", {}, g.gate.ask), " ", h("a", { class: "sub", href: `#/run/${enc(g.run)}` }, `${g.run} · ${g.step}`)), h("div", { class: "actions" }, ...g.gate.options.map((o, i) => h("button", { class: /^(stop|no|reject|deny|cancel)/i.test(o) ? "deny" : /^(allow|approve|yes|ok|go|continue|accept)/i.test(o) ? "allow" : "", onclick: () => resolveGate(g, o) }, o)), h("button", { class: "small", onclick: () => menu("Answer with a note", g.gate.options.map((o) => ({ label: o, run: () => resolveGate(g, o, true) }))) }, "+ note")));
}
// ── the Runs tab ────────────────────────────────────────────────────────────────────────────
const runs = new Cache(() => conn.request({ t: "run_list", active: false, limit: 60 }).then((r) => r.runs));
function newRunMenu() {
    menu("Start", [
        { title: tip("fanout"), label: "A fan-out", hint: "Several agents on one prompt, then compare", run: fanoutForm },
        { title: tip("recipe.run"), label: "A recipe", hint: "A declarative multi-step workflow", run: () => go("#/recipes") },
        { title: tip("run.lead"), label: "A lead", hint: "Make a session coordinate workers", run: leadForm },
    ]);
}
function fanoutForm() {
    const agents = data.agents.filter((a) => a.installed).map((a) => a.name);
    const projects = [...data.projects.values()];
    if (!projects.length) {
        ui.toast("Add a project first");
        return;
    }
    if (!agents.length) {
        ui.toast("Install an agent first");
        return;
    }
    form("Fan out", [
        { key: "prompt", label: "Prompt", type: "textarea", rows: 4, required: true, placeholder: "The task every candidate gets" },
        { key: "agents", label: "Agents", type: "checks", options: agents, value: [agents.includes(data.defaultAgent) ? data.defaultAgent : agents[0]], required: true, hint: "The candidates take turns through the ticked agents" },
        { key: "n", label: "Candidates", type: "number", value: 2, hint: "How many worktrees, each with its own agent" },
        { key: "project", label: "Project", type: "select", options: projects.map((p) => [p.id, p.slug]) },
        { key: "test", label: "Test command", placeholder: "e.g. cargo test, or scripts.run.test", hint: "Run in each candidate when its agent finishes, to rank them" },
        { key: "verify", label: "Done when", placeholder: "e.g. cargo test", hint: "A candidate only counts as done once this passes", more: true },
        { key: "judge", label: "Judge", type: "select", options: [["", "No judge"], ...agents], hint: "An agent that scores the candidates' diffs, advisory", more: true },
        { key: "base", label: "Base", placeholder: "default: the project's base, e.g. origin/main", hint: "The ref every candidate's branch starts from", more: true },
        { key: "name", label: "Name prefix", slug: "name", placeholder: "default: from the prompt", hint: `The worktrees are called this, numbered. ${NAME_HINT}`, more: true },
        { key: "timeout", label: "Time limit", placeholder: "e.g. 45m", hint: "Per candidate", more: true },
        { key: "structured", label: "Chat mode (full tool-call visibility)", type: "check", more: true },
    ], "Start", async (v) => {
        const r = await conn.request({ t: "fanout", spec: {
                agents: cycle(v.agents, Number(v.n) || 2), prompt: v.prompt, project: v.project,
                test: v.test || null, verify: v.verify || null, judge: v.judge || null, structured: !!v.structured, base: v.base || null, name: v.name || null, timeout: v.timeout || null
            } });
        go(`#/run/${enc(r.run.state.id)}`);
    });
}
function leadForm() {
    const live = [...allSessions().values()].filter((s) => s.kind === "agent" && s.state.t === "running");
    if (!live.length) {
        ui.toast("Start an agent first");
        return;
    }
    form("Lead a run", [
        { key: "session", label: "Session", type: "select", options: live.map((s) => [s.id, s.name]) },
        { key: "max", label: "Workers at once", type: "number", value: 4 },
        { key: "agents", label: "Agents it may start", placeholder: "any configured agent", hint: "Comma separated" },
    ], "Make it the lead", async (v) => {
        const r = await conn.request({ t: "lead_start", addr: v.session, policy: { max_sessions: Number(v.max) || 4, max_spawns: null, agents: (v.agents || "").split(",").map((s) => s.trim()).filter(Boolean), worktrees: ["new", "own"], destructive: false } });
        go(`#/run/${enc(r.run.state.id)}`);
    });
}
function runCard(r) {
    const s = r.state;
    const done = r.order.filter((k) => s.steps[k]?.status === "succeeded").length;
    return row(h("span", {}, s.name, " ", status(s.status), s.kind !== "recipe" ? badge(s.kind) : null), `${s.id} · ${done}/${r.order.length} steps${s.error ? ` · ${s.error}` : ""}`, ago(s.ended_ms ?? s.created_ms), { href: `#/run/${enc(s.id)}` });
}
function runsView() {
    poll("runs", () => { runs.refresh(""); gates.refresh(""); });
    const list = runs.get("");
    const open = gates.get("") ?? [];
    return h("div", {}, ui.header("Runs", false, h("button", { class: "icon", title: tip("recipe.list"), "aria-label": "Recipes", onclick: () => go("#/recipes") }, "📜"), h("button", { class: "icon plus", title: tip("fanout"), "aria-label": "Start a run", onclick: newRunMenu }, "＋")), h("main", { class: "list", title: tip("runs") }, open.length ? h("div", { class: "gates", title: tip("gate.list") }, ...open.map(gateCard)) : null, pending(runs, "") ?? (list.length ? h("div", { class: "rows" }, ...list.map(runCard)) : h("div", { class: "empty" }, h("p", {}, "No runs yet."), h("button", { class: "primary", onclick: newRunMenu }, "Start one")))));
}
// ── a run ───────────────────────────────────────────────────────────────────────────────────
const run = new Cache((id) => conn.request({ t: "run_get", id }).then((r) => r.run));
function stepMenu(r, k) {
    const st = r.state.steps[k];
    const a = st.attempts[st.attempts.length - 1];
    menu(k, [
        a?.session && { label: "Open its session", run: () => go(`#/s/${enc(a.session)}`) },
        a?.worktree && { label: "Open its worktree", run: () => go(`#/wt/${enc(a.worktree)}`) },
        ["succeeded", "failed", "crashed", "waiting", "cancelled"].includes(st.status) && { title: tip("run.retry"), label: "Run it again", hint: "A fresh attempt", run: async () => { await ask({ t: "run_retry", id: r.state.id, step: k }, `${k} runs again`); run.refresh(r.state.id); } },
        { title: tip("run.fork"), label: "Fork the run from here", hint: "Keeps the steps before it", run: () => forkForm(r, k) },
        { label: "Details", run: () => textSheet(`${k}`, JSON.stringify({ status: st.status, reason: st.reason, decided_by: st.decided_by, outputs: st.outputs, attempts: st.attempts.map((x) => ({ n: x.n, session: x.session, ended: x.ended_ms })) }, null, 2)) },
    ]);
}
function forkForm(r, from) {
    form(`Fork from ${from}`, [{ key: "set", label: "Variables to change", type: "textarea", rows: 3, placeholder: "name=value, one per line", hint: `Run vars now: ${Object.entries(r.state.vars).map(([a, b]) => `${a}=${b}`).join(", ") || "none"}` }], "Fork", async (v) => {
        const set = {};
        for (const line of (v.set || "").split("\n")) {
            const m = line.match(/^\s*([^=\s]+)\s*=\s*(.*)$/);
            if (m)
                set[m[1]] = m[2];
        }
        const f = await conn.request({ t: "run_fork", id: r.state.id, from, set });
        go(`#/run/${enc(f.run.state.id)}`);
    });
}
const runEvents = new Cache((id) => conn.request({ t: "run_events", id }).then((r) => r.events));
function runView(rest) {
    const id = safeDecode(rest);
    if (id.endsWith("/events"))
        return runEventsView(id.slice(0, -7));
    poll("run", () => run.refresh(id));
    const r = run.get(id);
    const body = pending(run, id) ?? (() => {
        const s = r.state;
        const myGates = (gates.get("") ?? []).filter((g) => g.run === id);
        return h("div", { class: "stack-in" }, myGates.map(gateCard).length ? h("div", {}, ...myGates.map(gateCard)) : null, h("div", { class: "pr-head" }, status(s.status), badge(s.kind), s.forked_from ? h("span", { class: "sub" }, `forked from ${s.forked_from[0]}`) : null), s.error ? h("p", { class: "form-err" }, s.error) : null, bar(s.kind === "fanout" || s.picked?.length || s.discarded?.length ? h("button", { class: "primary", title: tip("compare"), onclick: () => go(`#/compare/${enc(id)}`) }, "Compare") : null, h("button", { title: tip("run.events"), onclick: () => go(`#/run/${enc(id)}/events`) }, "Events"), s.status === "running" || s.status === "waiting" ? h("button", { class: "deny", title: tip("run.cancel"), onclick: () => confirm(`Cancel run ${id}?`, "Its running steps are cancelled and the agents they started are stopped; their worktrees stay.", "Cancel it", async () => { await ask({ t: "run_cancel", id }, "Cancelled"); run.refresh(id); }) }, "Cancel") : null), section("Steps", h("div", { class: "rows", title: tip("run.show") }, ...r.order.map((k) => {
            const st = s.steps[k] ?? { status: "pending", attempts: [] };
            const a = st.attempts[st.attempts.length - 1];
            return row(h("span", {}, k, " ", status(st.status)), [st.reason, st.decided_by ? `by ${st.decided_by}` : "", st.attempts.length > 1 ? `${st.attempts.length} attempts` : ""].filter(Boolean).join(" · ") || (a?.session ? "has a session" : ""), st.started_ms ? ago(st.ended_ms ?? st.started_ms) : "", { onclick: () => stepMenu(r, k) });
        }))), Object.keys(s.vars).length ? section("Variables", facts(Object.entries(s.vars))) : null, r.children.length ? section("Forks", h("div", { class: "rows" }, ...r.children.map((c) => row(c, "", null, { href: `#/run/${enc(c)}` })))) : null);
    })();
    return h("div", {}, ui.header(r?.state.name ?? "Run", "#/runs"), h("main", { class: "stack" }, body));
}
function runEventsView(id) {
    const evs = runEvents.get(id);
    return h("div", {}, ui.header("Run events", `#/run/${enc(id)}`, h("button", { class: "icon", onclick: () => runEvents.reload(id), "aria-label": "Refresh" }, "↻")), h("main", { class: "stack", title: tip("run.events") }, pending(runEvents, id) ?? h("div", { class: "timeline" }, ...[...evs].reverse().map(([seq, at, e]) => {
        const { e: kind, step, at: _at, ...rest } = e;
        void _at;
        return h("div", { class: "tl" }, h("time", {}, new Date(at).toLocaleTimeString()), h("b", {}, `${kind ?? "?"}${step ? ` ${step}` : ""}`), h("span", {}, `${seq} ${Object.keys(rest).length ? JSON.stringify(rest).slice(0, 200) : ""}`));
    }))));
}
// ── compare and pick ────────────────────────────────────────────────────────────────────────
const compare = new Cache((id) => conn.request({ t: "compare", run: id, refresh: false }).then((r) => r.doc));
function pickMenu(id, c) {
    const pick = (mode, into = null) => async () => {
        const r = await ask({ t: "pick", run: id, candidate: c.step, mode, into, paths: [], hunks: [] });
        if (r) {
            for (const n of r.notes ?? [])
                ui.toast(n);
            compare.reload(id);
        }
    };
    menu(`Take ${c.step}`, [
        { label: "Merge it", hint: "Rebase onto the main branch and fast-forward", run: pick("merge") },
        { label: "Cherry-pick its commits", run: pick("cherry_pick") },
        { label: "Open a pull request", run: pick("pr") },
        { label: "Apply some of its files…", hint: "Copied in, uncommitted", run: () => applyForm(id, c) },
    ]);
}
function applyForm(id, c) {
    form(`Apply from ${c.step}`, [{ key: "paths", label: "Files", type: "textarea", rows: 4, required: true, placeholder: "one path per line" }, { key: "into", label: "Into the worktree", placeholder: "default: the project's main checkout" }], "Apply", async (v) => {
        await conn.request({ t: "pick", run: id, candidate: c.step, mode: "apply", into: v.into || null, paths: v.paths.split("\n").map((s) => s.trim()).filter(Boolean), hunks: [] });
        ui.toast("Applied");
        compare.reload(id);
    });
}
function iterateForm(id, c) {
    form(c ? `Follow up with ${c.step}` : "Follow up with every candidate", [{ key: "text", label: "Message", type: "textarea", required: true, rows: 3 }], "Send", async (v) => {
        await conn.request({ t: "iterate", run: id, candidate: c?.step ?? null, text: v.text });
        ui.toast("Sent");
    });
}
function candidateCard(id, c) {
    const sessionId = c.session;
    return h("section", { class: `cand ${c.picked ? "picked" : ""} ${c.discarded ? "dim" : ""}` }, h("div", { class: "cand-head" }, h("b", {}, `#${c.rank} ${c.step}`), " ", c.agent ? badge(c.agent) : null, " ", status(c.status), c.picked ? badge("picked", "ok") : null, c.discarded ? badge("discarded") : null, c.verified === true ? badge("verified", "ok") : c.verified === false ? badge("not verified", "bad") : null), h("div", { class: "sub" }, `${c.files} file${c.files === 1 ? "" : "s"} · +${c.added} −${c.removed} · ${c.hunks} hunks · ${Math.round(c.duration_ms / 1000)}s${c.tests != null ? ` · tests ${c.tests === 0 ? "✓" : `✗ (${c.tests})`}` : ""}${c.broke_tests ? " · broke tests" : ""}${c.judge != null ? ` · judge ${c.judge}/10` : ""}`), c.reasons?.length ? h("div", { class: "sub" }, c.reasons.slice(0, 3).join(" · ")) : null, c.last_message ? h("div", { class: "detail" }, c.last_message) : null, !c.discarded ? bar(h("button", { class: "small primary", title: tip("pick"), onclick: () => pickMenu(id, c) }, "Take"), h("button", { class: "small", title: tip("iterate"), onclick: () => iterateForm(id, c) }, "Follow up"), c.worktree ? h("button", { class: "small", onclick: () => go(`#/wt/${enc(c.worktree)}/changes`) }, "Changes") : null, sessionId ? h("button", { class: "small", onclick: () => go(`#/s/${enc(sessionId)}`) }, "Session") : null, h("button", { class: "small", title: tip("discard"), onclick: () => confirm(`Discard candidate ${c.step}?`, "Its agent stops and its worktree is removed. Nothing is removed if that would lose work.", "Discard", async () => { await ask({ t: "discard", run: id, candidate: c.step, force: false }, "Discarded"); compare.reload(id); }) }, "Discard")) : null);
}
function compareView(rest) {
    const id = safeDecode(rest);
    poll("compare", () => compare.refresh(id), 6000);
    const d = compare.get(id);
    const body = pending(compare, id, "Comparing the candidates…") ?? h("div", { class: "stack-in", title: tip("compare") }, ...d.warnings.map((w) => h("p", { class: "form-err" }, w)), h("div", { class: "sub" }, `against ${d.base ?? "the main branch"}${d.base_tests != null ? ` (its tests ${d.base_tests === 0 ? "pass" : "fail"})` : ""}`), bar(h("button", { title: tip("iterate"), onclick: () => iterateForm(id) }, "Follow up with all"), h("button", { title: tip("discard"), onclick: () => confirm("Discard every candidate you didn't pick?", "Their agents stop and their worktrees are removed. Nothing is removed if that would lose work.", "Discard them", async () => { await ask({ t: "discard", run: id, candidate: null, force: false }, "Discarded"); compare.reload(id); }) }, "Discard the rest"), h("button", { class: "icon", "aria-label": "Run the evidence again", onclick: () => { conn.request({ t: "compare", run: id, refresh: true }).then((r) => { compare.set(id, r.doc); ui.render(); }, (e) => ui.toast(errText(e))); } }, "↻")), ...d.candidates.map((c) => candidateCard(id, c)), d.top_difference ? section("Between the top two", h("pre", { class: "block" }, d.top_difference)) : null);
    return h("div", {}, ui.header("Compare", `#/run/${enc(id)}`), h("main", { class: "stack" }, body));
}
// ── recipes ─────────────────────────────────────────────────────────────────────────────────
const recipes = new Cache(() => conn.request({ t: "recipe_list", project: null }).then((r) => r.recipes));
/// `name (required)` / `name = default` → a form field.
function recipeVar(v) {
    const m = v.match(/^(\S+)\s*(\(required\)|=\s*(.*))?$/);
    const name = m?.[1] ?? v;
    const required = m?.[2] === "(required)";
    return { key: name, label: name, value: m?.[3] ?? "", required, hint: required ? "required" : m?.[3] ? `default: ${m[3]}` : undefined };
}
function runRecipe(info) {
    const project = [...data.projects.values()];
    const go1 = async (vars, trust, projectId, base) => {
        const r = await conn.request({ t: "recipe_run", name: info.name, src: null, vars, project: projectId, base, trust });
        go(`#/run/${enc(r.run.state.id)}`);
    };
    form(`Run ${info.name}`, [
        ...info.vars.map(recipeVar),
        ...(project.length ? [{ key: "@project", label: "Project", type: "select", options: [["", "(none)"], ...project.map((p) => [p.id, p.slug])] }] : []),
        { key: "@base", label: "Base", placeholder: "default: the project's base, e.g. origin/main", hint: "The ref its new worktrees start from", more: true },
    ], "Run", async (v) => {
        const vars = {};
        let projectId = null;
        let base = null;
        for (const [k, val] of Object.entries(v)) {
            if (k === "@project")
                projectId = val || null;
            else if (k === "@base")
                base = val || null;
            else if (val !== "")
                vars[k] = String(val);
        }
        try {
            await go1(vars, null, projectId, base);
        }
        catch (e) {
            if (info.untrusted) {
                await new Promise((ok, bad) => { confirm("Trust this recipe?", `${errText(e)} It's from the repository, and runs commands on your computer.`, "Trust and run", () => go1(vars, info.untrusted, projectId, base).then(ok, bad), false); });
            }
            else
                throw e;
        }
    });
}
function recipesView() {
    const list = recipes.get("");
    return h("div", {}, ui.header("Recipes", "#/runs"), h("main", { class: "stack", title: tip("recipe.list") }, pending(recipes, "") ?? (list.length ? h("div", { class: "rows" }, ...list.map((r) => row(h("span", {}, r.name, " ", badge(r.source), r.untrusted ? badge("untrusted", "warn") : null, r.error ? badge("error", "bad") : null), r.description ?? `${r.steps} steps`, null, { onclick: () => menu(r.name, [
            { title: tip("recipe.run"), label: "Run it…", run: () => runRecipe(r) },
            { title: tip("recipe.show"), label: "Show its source", run: async () => { const x = await ask({ t: "recipe_show", name: r.name, project: null }); if (x)
                    textSheet(r.name, x.src); } },
        ]) }))) : h("p", { class: "empty" }, "No recipes."))));
}
views.set("runs", () => runsView());
views.set("run", runView);
views.set("compare", compareView);
views.set("recipes", () => recipesView());
