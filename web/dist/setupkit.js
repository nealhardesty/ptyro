// The setup checklist's logic (ROADMAP BOOT3, SPEC §24.6), kept apart from the page so it is
// tested without a browser (web/test/setup.mjs): following a setup run's progress (this page's,
// or one already going when the page opened), whether the first-run banner shows, and what an
// administrator step says to run on the computer instead.
/// Installed already, as wanted (the engine's `here_already`): one installed another way and
/// offered the move to the recommended installer (BOOT7, `moves`) is a row to tick instead.
export const isHere = (e) => e.state === "installed" || (e.state === "other" && !e.moves);
/// No coding agent (an outdated one is still there): the first-run banner shows.
export const noAgent = (plan) => !!plan?.entries?.length && !plan.entries.some((e) => e.kind === "agent" && e.state !== "missing");
/// Whether the first-run banner may ask (a survey) and show: only on a device that can install
/// (`full`: on another the page could only say it can't), not after "Not now", and not once this
/// device has seen an agent on the computer. `plan`: the answer, once asked.
export function bannerWants(o) {
    return o.scope === "full" && !o.later && !o.seen;
}
/// What is left for the computer (the owner's parity exception, 2026-11-06): the ticked
/// administrator steps, and what waits for one, in the plan's order. The daemon skips them all
/// (`Admin::Never`).
export function leftForComputer(plan) {
    const entries = plan?.entries ?? [];
    const left = new Set(entries.filter((e) => e.ticked && e.action?.admin).map((e) => e.id));
    for (let grew = true; grew;) {
        grew = false;
        for (const e of entries) {
            if (!e.ticked || !e.action || left.has(e.id))
                continue;
            if ((e.action.after ?? []).some((id) => left.has(id))) {
                left.add(e.id);
                grew = true;
            }
        }
    }
    return entries.filter((e) => left.has(e.id));
}
export function adminSteps(plan) {
    const left = leftForComputer(plan);
    if (!left.length)
        return [];
    return [{ names: left.map((e) => e.name), command: `ptyro install ${left.map((e) => e.id).join(" ")}` }];
}
/// Ticked items with no administrator step of their own that wait for one:
/// `["OpenCode", "Node.js"]` pairs, so the page says why they're left too.
export function waitsForAdmin(plan) {
    const left = leftForComputer(plan);
    const out = [];
    for (const e of left) {
        if (e.action?.admin)
            continue;
        for (const id of e.action?.after ?? []) {
            const dep = left.find((x) => x.id === id);
            if (dep)
                out.push([e.name, dep.name]);
        }
    }
    return out;
}
/// The steps the web runs: ticked, with an action, and not left for the computer.
export function webSteps(plan) {
    const left = new Set(leftForComputer(plan));
    return (plan?.entries ?? []).filter((e) => e.ticked && e.action && !left.has(e));
}
/// A setup run as this page follows it: its lines, then its report once it has finished. `theirs`: it was going already (another device, or before a reload), not started
/// here. `ended`: it finished while this page was away, so there is no report to show.
export class SetupLog {
    id = "";
    starting = false;
    theirs = false;
    ended = false;
    seq = 0;
    /// The plan the run runs (`SetupStarted`'s or `running.plan`), for its words.
    plan = null;
    lines = [];
    report = null;
    /// Updates that failed because the program is running (BOOT5, `failed.in_use`): the page offers
    /// to try each again once its sessions end.
    retry = [];
    /// Events from before this page's own run said its id.
    early = [];
    /// Events of the followed run that came after a gap in `seq`: held until the plan's `running`
    /// fills it.
    held = [];
    /// The latest events of a run this page doesn't follow (yet): kept, so joining it from a
    /// plan's `running` misses nothing that came between.
    stray = { run: "", events: [] };
    get running() { return this.id !== "" && !this.report && !this.ended; }
    reset() {
        Object.assign(this, { id: "", starting: false, theirs: false, ended: false, seq: 0, plan: null, lines: [], report: null, retry: [], early: [], held: [] });
    }
    line(t) { this.lines.push(t); if (this.lines.length > 400)
        this.lines.shift(); }
    /// This page asks for a run.
    start() { this.reset(); this.starting = true; }
    /// It was refused or failed: nothing is followed.
    failed() { this.starting = false; this.early = []; }
    /// The daemon said this page's run started (`SetupStarted`), with the plan it runs (never the
    /// page's own).
    started(run, plan) {
        this.id = run;
        this.plan = plan;
        this.starting = false;
        for (const e of this.early.splice(0))
            this.event(e);
    }
    /// A plan's `running`, from a request sent while this page followed `asked` (`id` then):
    /// - another run: follow it, from its events so far, then what came since (`"joined"`);
    /// - the run followed: add what this page missed (`"merged"`, or `"done"` if it ended);
    /// - nothing, while the run followed when it was asked is still followed here: it ended while
    ///   this page was away (`"ended"`, or `"done"` when held events had its finish).
    sync(running, asked) {
        if (this.starting)
            return "";
        if (running && running.run === this.id) {
            if (!this.running)
                return "";
            return this.merge(running.events) ? "done" : "merged";
        }
        if (running) {
            this.reset();
            this.id = running.run;
            this.theirs = true;
            this.plan = running.plan;
            const stray = this.stray.run === running.run ? this.stray.events.splice(0) : [];
            this.merge([...running.events.map((e) => ({ run: running.run, ...e })), ...stray]);
            return "joined";
        }
        if (!this.running || asked !== this.id)
            return "";
        if (this.merge([]))
            return "done";
        this.ended = true;
        this.line("… It finished while this page was away: the checklist below is how the computer is now.");
        return "ended";
    }
    /// Add `events` (in `seq` order, those not seen yet) and what was held, saying so when some
    /// are gone (the daemon keeps the latest 500). True when that ended the run.
    merge(events) {
        const all = [...events, ...this.held.splice(0)].map((e) => ({ run: this.id, ...e }))
            .filter((e) => !e.seq || e.seq > this.seq)
            .sort((x, y) => (x.seq ?? 0) - (y.seq ?? 0));
        const first = all.find((e) => e.seq);
        if (first?.seq && first.seq > this.seq + 1)
            this.line(`… ${first.seq - this.seq - 1} earlier line(s) from while this page was away aren't shown`);
        for (const e of all) {
            if (e.seq) {
                if (e.seq <= this.seq)
                    continue;
                this.seq = e.seq - 1;
            }
            this.event(e);
        }
        return !!this.report;
    }
    /// One event. Returns `"new"` for the first event of a run this page doesn't know, and
    /// `"gap"` when the followed run's events skipped some (this page was away), so it asks the
    /// daemon; `"done"` when the run it follows finished; else `""`.
    event(e) {
        if (this.starting && !this.id) {
            this.early.push(e);
            return "";
        }
        if (e.run !== this.id) {
            const first = this.stray.run !== e.run;
            if (first)
                this.stray = { run: e.run, events: [] };
            this.stray.events.push(e);
            if (this.stray.events.length > 600)
                this.stray.events.shift();
            return first && !this.running ? "new" : "";
        }
        if (!this.running)
            return "";
        if (e.seq) {
            if (e.seq <= this.seq)
                return "";
            if (e.seq > this.seq + 1) {
                this.held.push(e);
                return this.held.length === 1 ? "gap" : "";
            }
            this.seq = e.seq;
        }
        const ev = e.event;
        const left = (id) => leftForComputer(this.plan).some((x) => x.id === id);
        if (ev.event === "started")
            this.line(`▸ ${ev.name}: ${ev.command}`);
        else if (ev.event === "line")
            this.line(`  │ ${ev.text}`);
        else if (ev.event === "done")
            this.line(`✓ ${ev.id}${ev.version ? " " + ev.version : ""}`);
        else if (ev.event === "failed") {
            this.line(`✗ ${ev.id}: ${ev.error}\n  fix: ${ev.fix}`);
            if (ev.in_use)
                this.retry.push({ id: ev.id });
        }
        else if (ev.event === "skipped" && left(ev.id))
            this.line(`– ${ev.id}: left for the computer: ${ev.fix}`);
        else if (ev.event === "skipped")
            this.line(`– ${ev.id}: ${ev.reason}\n  fix: ${ev.fix}`);
        else if (ev.event === "blocked")
            this.line(`✗ ${ev.id}: can't be installed here: ${ev.reason}\n  fix: ${ev.fix}`);
        else if (ev.event === "finished") {
            const rep = ev.report ?? {};
            this.report = { done: rep.done ?? [], failed: rep.failed ?? [], skipped: rep.skipped ?? [], blocked: rep.blocked ?? [] };
            return "done";
        }
        return "";
    }
    /// The report in words: `2 installed, 1 failed. git and GitHub CLI are left for the computer
    /// (administrator rights): run ptyro install git gh there.` A skip for another reason stays
    /// "not run".
    summary() {
        const r = this.report;
        if (!r)
            return "";
        const left = leftForComputer(this.plan);
        const kept = r.skipped.map((id) => left.find((e) => e.id === id)).filter(Boolean);
        // An outdated agent ticked (BOOT5) runs its update: said as updated.
        const doneAs = (what) => r.done.filter((id) => (this.plan?.entries ?? []).some((e) => e.id === id && e.action && what(e.action)));
        const updated = doneAs((a) => a.update);
        // One installed another way, ticked (BOOT7): moved to the recommended installer.
        const moved = doneAs((a) => a.moving);
        const installed = r.done.length - updated.length - moved.length;
        const parts = installed || (!updated.length && !moved.length) ? [`${installed} installed`] : [];
        if (updated.length)
            parts.push(`${updated.length} updated`);
        if (moved.length)
            parts.push(`${moved.length} moved to the recommended installer`);
        if (r.failed.length)
            parts.push(`${r.failed.length} failed`);
        const other = r.skipped.length - kept.length;
        if (other)
            parts.push(`${other} not run`);
        if (r.blocked.length)
            parts.push(`${r.blocked.length} can't be installed here`);
        let text = `${parts.join(", ")}.`;
        if (kept.length) {
            const names = [...new Set(kept.map((e) => e.name ?? e.id))];
            const list = names.length > 1 ? `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}` : names[0];
            const cmds = adminSteps(this.plan).map((a) => a.command).join(" and ");
            text += ` ${list} ${names.length > 1 ? "are" : "is"} left for the computer (administrator rights): run ${cmds} there.`;
        }
        return text;
    }
}
export function updateOffer(c) {
    const d = c.data;
    if (c.section !== "updates" || c.level !== "warn" || !d?.id || !d.program)
        return null;
    return { id: d.id, program: d.program, name: d.name ?? d.id, latest: d.latest, version: d.version, command: d.command ?? null, admin: !!d.admin, path: d.path ?? null, managed: d.managed ?? null, via: d.via ?? null };
}
/// How an installer is said (the engine's `Via::words`).
export const VIA_WORDS = { vendor: "the vendor's installer", npm: "npm", brew: "Homebrew", apt: "apt", dnf: "dnf", xcode: "Xcode's Command Line Tools", "node-dist": "nodejs.org's build" };
export const viaWords = (v) => (v && VIA_WORDS[v]) || v || "its installer";
export function moveOffer(c) {
    const m = c.data?.move;
    if (c.section !== "updates" || !m?.id || !m.program || !m.from || !m.to)
        return null;
    return { id: m.id, program: m.program, name: m.name ?? m.id, from: m.from, to: m.to, uninstall: m.uninstall ?? "", admin: !!m.admin, keeps: m.keeps ?? [] };
}
/// The moves a run asks for (BOOT7): the agents ticked where the move is offered.
export function movesOf(plan) {
    return [...new Set((plan?.entries ?? []).filter((e) => e.picked && e.moves).map((e) => e.id))];
}
/// What a move says before it runs: what it installs, what it removes, and what stays.
export function moveWords(m) {
    const keeps = m.keeps.length ? `your settings and logins (${m.keeps.join(", ")}) stay` : "your settings and logins stay";
    return `${m.name} is installed with ${viaWords(m.from)}. Moving it installs it through ${viaWords(m.to)}, the recommended way, checks the new one runs, and only then removes ${viaWords(m.from)}'s copy (${m.uninstall}); ${keeps}. If the new install fails, the old one stays as it was.`;
}
export function selfUpdate(o) {
    if (o.id !== "ptyro")
        return null;
    if (o.managed)
        return { kind: "managed", how: o.managed };
    if (o.admin)
        return { kind: "command", command: o.command };
    const what = o.command
        ? `runs ${o.command}`
        : `downloads the release, checks it against its SHA256SUMS (and its signed provenance where gh is installed), and replaces ${o.path ?? "ptyro"}`;
    return { kind: "run", what };
}
/// `a` is a later version than `b` (`0.0.69` > `0.0.68`), as the CLI compares them.
export function newer(a, b) {
    const parts = (v) => v.split(/[^0-9]+/).filter(Boolean).map(Number);
    const x = parts(a), y = parts(b);
    for (let i = 0; i < Math.max(x.length, y.length); i++) {
        const d = (x[i] ?? -1) - (y[i] ?? -1);
        if (d)
            return d > 0;
    }
    return false;
}
/// The machines linked here that `ptyro update ptyro` brings to `target` too (SPEC §24.8): those
/// reachable (online, or down only for their version) on an older ptyro.
export function linkedBehind(machines, target) {
    const out = [];
    for (const m of machines)
        if ((m.state === "online" || m.state === "mismatch") && m.version && newer(target, m.version))
            out.push(`${m.name} ${m.version}`);
    return out;
}
/// The live sessions here running an agent (`id`, the engine's: `claude`, `opencode`): an
/// update leaves them on the old version until they restart, so the page names them.
export function runningAgent(sessions, id) {
    const out = [];
    for (const s of sessions)
        if (!s.machine && s.state.t === "running" && (s.agent === id || s.inner_agent === id))
            out.push(s.name);
    return out;
}
/// The Try again of an update refused because the agent runs (BOOT5): its label, and what to run
/// (`SetupRun`'s picks).
export function retryOf(r, name) {
    return { label: `Try updating ${name} again`, picks: [r.id] };
}
/// A setup offer's button (BOOT9): each pick by the name the checklist gives it ("Set up
/// Claude Code", never "claude"). A program goes to its agent through `agents` (the daemon's list),
/// then to the checklist's name through `entries` (a plan's), else stays the agent's name. No
/// picks: the checklist's defaults.
export function setupLabel(picks, agents, entries) {
    if (!picks.length)
        return "Set up agents and tools";
    const named = picks.map((p) => {
        const id = agents.find((a) => a.program === p)?.name ?? p;
        return entries.find((e) => e.id === id || e.id === p)?.name ?? id;
    });
    return `Set up ${[...new Set(named)].join(", ")}`;
}
/// What a message's setup offer installs (BOOT9, `ptyro_agents::locate::setup_offer`'s rule):
/// "`ptyro setup` installs one" is the checklist's defaults (`[]`), "`ptyro install codex`
/// installs it" those names; `null` for anything else (a lock held by `ptyro install`, a move).
export function setupOffer(text) {
    const m = /`ptyro (setup|install)((?: [^`\s]+)*)` installs/.exec(text);
    if (!m)
        return null;
    const names = m[2].split(" ").filter(Boolean);
    if (m[1] === "setup")
        return [];
    return names.length ? names : null;
}
