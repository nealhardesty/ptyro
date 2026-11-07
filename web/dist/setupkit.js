// The setup checklist's logic (ROADMAP BOOT3, SPEC §24.6), kept apart from the page so it is
// tested without a browser (web/test/setup.mjs): following a setup run's progress (this page's,
// or one already going when the page opened), whether the first-run banner shows, and what an
// administrator step says to run on the computer instead.
/// How many sides a setup run reports on (each ends with its own `finished`, BOOT8): those with
/// a ticked entry that runs or is blocked, in the plan the daemon actually runs.
export function setupSides(plan) {
    const sides = new Set((plan?.entries ?? []).filter((e) => e.ticked && (e.action || e.blocked)).map((e) => e.side ?? "local"));
    return Math.max(1, sides.size);
}
/// Installed already, as wanted (the engine's `here_already`): one installed another way and
/// offered the move to the recommended installer (BOOT7, `moves`) is a row to tick instead.
export const isHere = (e) => e.state === "installed" || (e.state === "other" && !e.moves);
/// No coding agent on any side (missing everywhere; an outdated one is still there): the
/// first-run banner shows.
export const noAgent = (plan) => !!plan?.entries?.length && !plan.entries.some((e) => e.kind === "agent" && e.state !== "missing");
/// Whether the first-run banner may ask (a survey) and show: only on a device that can install
/// (`full`: on another the page could only say it can't), not after "Not now", and not once this
/// device has seen an agent on the computer. `plan`: the answer, once asked.
export function bannerWants(o) {
    return o.scope === "full" && !o.later && !o.seen;
}
/// What is left for the computer (the owner's parity exception, 2026-11-06): the ticked
/// administrator steps, and what waits for one on its side (Gemini CLI after Node.js from apt),
/// in the plan's order. The daemon skips them all (`Admin::Never`).
export function leftForComputer(plan) {
    const entries = plan?.entries ?? [];
    const key = (side, id) => `${side ?? "local"}:${id}`;
    const left = new Set(entries.filter((e) => e.ticked && e.action?.admin).map((e) => key(e.side, e.id)));
    for (let grew = true; grew;) {
        grew = false;
        for (const e of entries) {
            if (!e.ticked || !e.action || left.has(key(e.side, e.id)))
                continue;
            if ((e.action.after ?? []).some((id) => left.has(key(e.side, id)))) {
                left.add(key(e.side, e.id));
                grew = true;
            }
        }
    }
    return entries.filter((e) => left.has(key(e.side, e.id)));
}
export function adminSteps(plan) {
    const entries = plan?.entries ?? [];
    const sides = [...new Set(entries.map((e) => e.side ?? "local"))];
    const two = sides.length > 1;
    const left = leftForComputer(plan);
    const out = [];
    for (const side of sides) {
        const these = left.filter((e) => (e.side ?? "local") === side);
        if (!these.length)
            continue;
        const where = two ? ` --where ${side === "wsl" ? "wsl" : "windows"}` : "";
        out.push({ side, names: these.map((e) => e.name), command: `ptyro install ${these.map((e) => e.id).join(" ")}${where}` });
    }
    return out;
}
/// Ticked items with no administrator step of their own that wait for one (Gemini CLI after
/// Node.js from apt): `["Gemini CLI", "Node.js"]` pairs, so the page says why they're left too.
export function waitsForAdmin(plan) {
    const left = leftForComputer(plan);
    const out = [];
    for (const e of left) {
        if (e.action?.admin)
            continue;
        for (const id of e.action?.after ?? []) {
            const dep = left.find((x) => x.id === id && x.side === e.side);
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
/// A setup run as this page follows it: its lines, then its report once every side has
/// finished. `theirs`: it was going already (another device, or before a reload), not started
/// here. `ended`: it finished while this page was away, so there is no report to show.
export class SetupLog {
    id = "";
    starting = false;
    theirs = false;
    ended = false;
    sides = 1;
    seq = 0;
    /// The plan the run runs (`SetupStarted`'s or `running.plan`), for its sides and its words.
    plan = null;
    lines = [];
    /// Each side's report, as its `finished` said, with the side.
    reports = [];
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
        Object.assign(this, { id: "", starting: false, theirs: false, ended: false, sides: 1, seq: 0, plan: null, lines: [], reports: [], report: null, retry: [], early: [], held: [] });
    }
    line(t) { this.lines.push(t); if (this.lines.length > 400)
        this.lines.shift(); }
    /// This page asks for a run.
    start() { this.reset(); this.starting = true; }
    /// It was refused or failed: nothing is followed.
    failed() { this.starting = false; this.early = []; }
    /// The daemon said this page's run started (`SetupStarted`): count the sides from the plan it
    /// runs, never the page's own.
    started(run, plan) {
        this.id = run;
        this.plan = plan;
        this.sides = setupSides(plan);
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
            this.sides = setupSides(running.plan);
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
        const at = this.sides > 1 ? (e.side === "wsl" ? "WSL: " : "Windows: ") : "";
        const left = (id) => leftForComputer(this.plan).some((x) => x.id === id && (x.side ?? "local") === (e.side ?? "local"));
        if (ev.event === "started")
            this.line(`▸ ${at}${ev.name}: ${ev.command}`);
        else if (ev.event === "line")
            this.line(`  │ ${ev.text}`);
        else if (ev.event === "done")
            this.line(`✓ ${at}${ev.id}${ev.version ? " " + ev.version : ""}`);
        else if (ev.event === "failed") {
            this.line(`✗ ${at}${ev.id}: ${ev.error}\n  fix: ${ev.fix}`);
            if (ev.in_use)
                this.retry.push({ id: ev.id, side: e.side ?? "local" });
        }
        else if (ev.event === "skipped" && left(ev.id))
            this.line(`– ${at}${ev.id}: left for the computer: ${ev.fix}`);
        else if (ev.event === "skipped")
            this.line(`– ${at}${ev.id}: ${ev.reason}\n  fix: ${ev.fix}`);
        else if (ev.event === "blocked")
            this.line(`✗ ${at}${ev.id}: can't be installed here: ${ev.reason}\n  fix: ${ev.fix}`);
        else if (ev.event === "finished") {
            this.reports.push({ side: e.side ?? "local", report: ev.report });
            if (this.reports.length >= this.sides) {
                const all = (k) => this.reports.flatMap((r) => r.report[k] ?? []);
                this.report = { done: all("done"), failed: all("failed"), skipped: all("skipped"), blocked: all("blocked") };
                return "done";
            }
        }
        return "";
    }
    /// The report in words: `2 installed, 1 failed. git and GitHub CLI are left for the computer
    /// (administrator rights): run ptyro install git gh there.` What was left for the computer is
    /// told apart by side and id: a skip on one side for another reason stays "not run".
    summary() {
        const r = this.report;
        if (!r)
            return "";
        const left = leftForComputer(this.plan);
        const leftOn = (side, id) => left.find((e) => e.id === id && (e.side ?? "local") === side);
        const kept = this.reports.flatMap(({ side, report }) => (report.skipped ?? []).map((id) => leftOn(side, id)).filter(Boolean));
        // An outdated agent ticked (BOOT5) runs its update: said as updated.
        const doneAs = (what) => this.reports.flatMap(({ side, report }) => (report.done ?? []).filter((id) => (this.plan?.entries ?? []).some((e) => e.id === id && (e.side ?? "local") === side && e.action && what(e.action))));
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
export const VIA_WORDS = { vendor: "the vendor's installer", npm: "npm", brew: "Homebrew", winget: "WinGet", apt: "apt", dnf: "dnf", xcode: "Xcode's Command Line Tools", "node-dist": "nodejs.org's build" };
export const viaWords = (v) => (v && VIA_WORDS[v]) || v || "its installer";
export function moveOffer(c) {
    const m = c.data?.move;
    if (c.section !== "updates" || !m?.id || !m.program || !m.from || !m.to)
        return null;
    return { id: m.id, program: m.program, name: m.name ?? m.id, from: m.from, to: m.to, uninstall: m.uninstall ?? "", admin: !!m.admin, keeps: m.keeps ?? [] };
}
/// The moves a run asks for (BOOT7): the agents ticked where the move is offered, either side.
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
/// The live sessions here running an agent (`id`, the engine's: `claude`, `antigravity`): an
/// update leaves them on the old version until they restart, so the page names them.
export function runningAgent(sessions, id) {
    const out = [];
    for (const s of sessions)
        if (!s.machine && s.state.t === "running" && (s.agent === id || s.inner_agent === id))
            out.push(s.name);
    return out;
}
/// The Try again of an update refused because the agent runs (BOOT5): its label, and what to run
/// (`SetupRun`'s picks for the daemon's side, or WSL's `wsl_picks` for one on WSL's side).
export function retryOf(r, name) {
    const wsl = r.side === "wsl";
    return { label: `Try updating ${name} again`, picks: wsl ? [] : [r.id], wslPicks: wsl ? [r.id] : null };
}
