// The setup checklist's logic (ROADMAP BOOT3, SPEC §24.6), kept apart from the page so it is
// tested without a browser (web/test/setup.mjs): following a setup run's progress (this page's,
// or one already going when the page opened), whether the first-run banner shows, and what an
// administrator step says to run on the computer instead.

type Plan = { platform?: string; wsl?: any; entries?: any[] } | null | undefined;

/// How many sides a setup run reports on (each ends with its own `finished`, BOOT8): those with
/// a ticked entry that runs or is blocked, in the plan the daemon actually runs.
export function setupSides(plan: { entries?: { side?: string; ticked?: boolean; action?: unknown; blocked?: unknown }[] } | null | undefined): number {
  const sides = new Set((plan?.entries ?? []).filter((e) => e.ticked && (e.action || e.blocked)).map((e) => e.side ?? "local"));
  return Math.max(1, sides.size);
}

/// Installed already, as wanted (the engine's `here_already`).
export const isHere = (e: any): boolean => e.state === "installed" || e.state === "other";

/// No coding agent on any side (missing everywhere; an outdated one is still there): the
/// first-run banner shows.
export const noAgent = (plan: Plan): boolean => !!plan?.entries?.length && !plan.entries.some((e) => e.kind === "agent" && e.state !== "missing");

/// What a ticked administrator step needs instead of running from the web (the owner's parity
/// exception, 2026-11-06): per side, its items and the command to run on the computer.
export interface AdminStep { side: string; names: string[]; command: string }
export function adminSteps(plan: Plan): AdminStep[] {
  const entries = plan?.entries ?? [];
  const sides = [...new Set(entries.map((e) => e.side ?? "local"))];
  const two = sides.length > 1;
  const out: AdminStep[] = [];
  for (const side of sides) {
    const these = entries.filter((e) => (e.side ?? "local") === side && e.ticked && e.action?.admin);
    if (!these.length) continue;
    const where = two ? ` --where ${side === "wsl" ? "wsl" : "windows"}` : "";
    out.push({ side, names: these.map((e) => e.name), command: `ptyro install ${these.map((e) => e.id).join(" ")}${where}` });
  }
  return out;
}

/// Ticked items the web can run that wait for an administrator step first (Gemini CLI after Node.js
/// from apt): `["Gemini CLI", "Node.js"]` pairs, so the page says to run that one first.
export function waitsForAdmin(plan: Plan): [string, string][] {
  const entries = plan?.entries ?? [];
  const out: [string, string][] = [];
  for (const e of entries) {
    if (!e.ticked || !e.action || e.action.admin) continue;
    for (const id of e.action.after ?? []) {
      const dep = entries.find((x) => x.id === id && x.side === e.side);
      if (dep?.action?.admin) out.push([e.name, dep.name]);
    }
  }
  return out;
}

/// The steps the web runs: ticked, with an action that needs no administrator rights.
export const webSteps = (plan: Plan): any[] => (plan?.entries ?? []).filter((e) => e.ticked && e.action && !e.action.admin);

/// A `setup_progress` event: `{run, side, event, seq}` (`seq` from 1, 0 from an older daemon).
export interface Progress { run: string; side?: string; event: any; seq?: number }

/// A setup run as this page follows it: its lines, then its report once every side has
/// finished. `theirs`: it was going already (another device, or before a reload), not started
/// here.
export class SetupLog {
  id = "";
  starting = false;
  theirs = false;
  sides = 1;
  seq = 0;
  lines: string[] = [];
  reports: any[] = [];
  report: { done: string[]; failed: string[]; skipped: string[]; blocked: string[] } | null = null;
  /// Events from before this page's own run said its id.
  private early: Progress[] = [];
  /// The latest events of a run this page doesn't follow (yet): kept, so joining it from a
  /// plan's `running` misses nothing that came between.
  private stray: { run: string; events: Progress[] } = { run: "", events: [] };

  get running(): boolean { return this.id !== "" && !this.report; }

  private reset(): void {
    Object.assign(this, { id: "", starting: false, theirs: false, sides: 1, seq: 0, lines: [], reports: [], report: null, early: [] });
  }

  /// This page asks for a run.
  start(): void { this.reset(); this.starting = true; }

  /// It was refused or failed: nothing is followed.
  failed(): void { this.starting = false; this.early = []; }

  /// The daemon said this page's run started (`SetupStarted`): count the sides from the plan it
  /// runs, never the page's own.
  started(run: string, plan: Plan): void {
    this.id = run;
    this.sides = setupSides(plan);
    this.starting = false;
    for (const e of this.early.splice(0)) this.event(e);
  }

  /// A plan's `running`: follow that run, from its events so far, then what came since.
  adopt(running: { run: string; plan: Plan; events: { seq: number; side?: string; event: any }[] } | null | undefined): boolean {
    if (!running || running.run === this.id || this.starting) return false;
    this.reset();
    this.id = running.run;
    this.theirs = true;
    this.sides = setupSides(running.plan);
    for (const e of running.events) this.event({ run: running.run, ...e });
    if (this.stray.run === running.run) for (const e of this.stray.events.splice(0)) this.event(e);
    return true;
  }

  /// One event. Returns `"new"` for the first event of a run this page doesn't know (so it asks
  /// the daemon about it), `"done"` when the run it follows finished, else `""`.
  event(e: Progress): "" | "new" | "done" {
    if (this.starting && !this.id) { this.early.push(e); return ""; }
    if (e.run !== this.id) {
      const first = this.stray.run !== e.run;
      if (first) this.stray = { run: e.run, events: [] };
      this.stray.events.push(e);
      if (this.stray.events.length > 600) this.stray.events.shift();
      return first && !this.running ? "new" : "";
    }
    if (e.seq) {
      if (e.seq <= this.seq) return "";
      this.seq = e.seq;
    }
    const ev = e.event;
    const at = this.sides > 1 ? (e.side === "wsl" ? "WSL: " : "Windows: ") : "";
    const line = (t: string): void => { this.lines.push(t); if (this.lines.length > 400) this.lines.shift(); };
    if (ev.event === "started") line(`▸ ${at}${ev.name}: ${ev.command}`);
    else if (ev.event === "line") line(`  │ ${ev.text}`);
    else if (ev.event === "done") line(`✓ ${at}${ev.id}${ev.version ? " " + ev.version : ""}`);
    else if (ev.event === "failed") line(`✗ ${at}${ev.id}: ${ev.error}\n  fix: ${ev.fix}`);
    else if (ev.event === "skipped") line(`– ${at}${ev.id}: ${ev.reason}\n  fix: ${ev.fix}`);
    else if (ev.event === "blocked") line(`✗ ${at}${ev.id}: can't be installed here: ${ev.reason}\n  fix: ${ev.fix}`);
    else if (ev.event === "finished") {
      this.reports.push(ev.report);
      if (this.reports.length >= this.sides) {
        const all = (k: string): string[] => this.reports.flatMap((r) => r[k] ?? []);
        this.report = { done: all("done"), failed: all("failed"), skipped: all("skipped"), blocked: all("blocked") };
        return "done";
      }
    }
    return "";
  }

  /// The report in words: `2 installed, 0 failed, 1 not run.`
  summary(): string {
    const r = this.report;
    if (!r) return "";
    return `${r.done.length} installed, ${r.failed.length} failed, ${r.skipped.length} not run${r.blocked.length ? `, ${r.blocked.length} can't be installed here` : ""}.`;
  }
}
