// Which machine this ptyro runs on (SPEC §16.6): in the tab's title, the header and a favicon dot,
// so several pages, one per machine, tell apart at a glance. The name comes from the daemon (a
// page served from another computer shows that computer's name). Pure functions here; `apply`
// touches the page.
/// A stable hue (0..359) for a name: FNV-1a over its lowercase UTF-8 bytes, the same as
/// `ptyro_core::machine::hue`, so the TUI's marker and the web's agree.
export function hue(name) {
    let h = 0x811c9dc5;
    for (const b of new TextEncoder().encode(name.toLowerCase())) {
        h = Math.imul(h ^ b, 0x01000193) >>> 0;
    }
    return h % 360;
}
/// The machine's accent colour: muted, and a little darker on a light page so it reads on both.
export function accent(name, dark) {
    return `hsl(${hue(name)} 55% ${dark ? 62 : 42}%)`;
}
/// The tab title: `studio · ptyro`, with the number of agents waiting in front: `(2) studio · ptyro`.
export function titleFor(machine, waiting) {
    const base = machine ? `${machine} · ptyro` : "ptyro";
    return waiting ? `(${waiting}) ${base}` : base;
}
/// ptyro's icon with a dot in the machine's colour in the corner, as a data: URL for the favicon.
export function faviconUrl(name, dark) {
    const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64"><rect width="64" height="64" rx="12" fill="#161b22"/><path d="M17 19 32 32 17 45" fill="none" stroke="#e6edf3" stroke-width="7" stroke-linecap="round" stroke-linejoin="round"/><rect x="36" y="40" width="13" height="6" fill="#58a6ff"/><circle cx="50" cy="14" r="11" fill="${accent(name, dark)}" stroke="#161b22" stroke-width="3"/></svg>`;
    return `data:image/svg+xml,${encodeURIComponent(svg)}`;
}
let shown = "";
/// Put the machine on the page: title, favicon, the accent colour for the header's thin bar, and
/// the name an installed app gets. Cheap to call on every render.
export function apply(machine, waiting) {
    document.title = titleFor(machine, waiting);
    if (machine === shown)
        return;
    shown = machine;
    const dark = !(typeof matchMedia === "function" && matchMedia("(prefers-color-scheme: light)").matches);
    const root = document.documentElement;
    if (machine)
        root.style.setProperty("--machine", accent(machine, dark));
    else
        root.style.removeProperty("--machine");
    const icon = document.querySelector('link[rel="icon"]');
    if (icon && machine) {
        icon.type = "image/svg+xml";
        icon.href = faviconUrl(machine, dark);
    }
    // The same name the server gives an installed app (`serve.rs` `app_names`).
    document.querySelector('meta[name="apple-mobile-web-app-title"]')?.setAttribute("content", machine ? `ptyro ${machine}` : "ptyro");
}
/// Whether the page was opened over Tailscale: a MagicDNS `*.ts.net` name or a tailnet address
/// (100.64.0.0/10, fd7a:115c:a1e0::/48). An offline page can't ask the server, so the address
/// it was opened at is what says so.
export function viaTailscale(host) {
    const h = host.toLowerCase().replace(/^\[|\]$/g, "").replace(/\.$/, "");
    if (h.endsWith(".ts.net"))
        return true;
    const v4 = /^100\.(\d+)\.\d+\.\d+$/.exec(h);
    if (v4)
        return Number(v4[1]) >= 64 && Number(v4[1]) <= 127;
    return h.startsWith("fd7a:115c:a1e0:");
}
/// The banner while the connection is down. Over Tailscale the usual cause is Tailscale being
/// off on this device, so it says where to look.
export function offlineText(state, host) {
    if (state === "connecting")
        return "Connecting…";
    return viaTailscale(host) ? "Offline: reconnecting… Can't reach this computer over Tailscale: check that Tailscale is on on this device." : "Offline: reconnecting…";
}
// ── machine and profile tabs (TABS, SPEC §11.2k) ────────────────────────────────────────────
// The strip over the session list that picks one machine and one profile. `ptyro_core::scope`
// (crates/ptyro-core/src/scope.rs) is the one definition of the rule; this says it again, pure
// so it runs without a page, and web/test/scope.mjs keeps it equal to the Rust tests: change
// both together. `machine` is null for this machine, a linked one's name otherwise; a linked
// machine's profiles arrive with its name on (`laptop2:work`), so `Scope.profile` is that and
// `profileName` the one people read (`work`). `all` is not a tab: a kept scope is
// `Scope | null`, null for `all`.
export const DEFAULT_PROFILE = "default";
const mach = (m) => m ?? null;
const prof = (s) => s.profile ?? DEFAULT_PROFILE;
/// A profile as people read it: `laptop2:work` on laptop2 is `work`.
export function bare(profile, machine) {
    return machine && profile.startsWith(`${machine}:`) ? profile.slice(machine.length + 1) : profile;
}
/// A profile on a machine, named as people do (`work`) or as the wire does (`laptop2:work`).
export function scopeNew(machine, profile) {
    return { machine, profile: machine && bare(profile, machine) === profile ? `${machine}:${profile}` : profile };
}
export const profileName = (s) => bare(s.profile, s.machine);
/// The tab a session is in (it always exists: the session puts its profile in use).
export const scopeOf = (s) => ({ machine: mach(s.machine), profile: prof(s) });
export const scopeOfWorkspace = (w) => ({ machine: mach(w.machine), profile: w.profile });
export const sameScope = (a, b) => a === b || (!!a && !!b && a.machine === b.machine && a.profile === b.profile);
/// This tab lists the session: same machine, and it runs as this profile.
export const keepsSession = (t, s) => mach(s.machine) === t.machine && prof(s) === t.profile;
/// This tab shows the workspace: same machine, and its own profile is this one or one of
/// `under` (the sessions listed under it) runs as this profile.
export function keepsWorkspace(t, w, under) {
    if (mach(w.machine) !== t.machine)
        return false;
    if (w.profile === t.profile)
        return true;
    for (const s of under)
        if (keepsSession(t, s))
            return true;
    return false;
}
/// Shown in the session tree: anything but a chat's engine.
const listed = (s) => !s.chat;
/// Makes a tab: shown in the tree, and not a quick terminal (it goes with its worktree).
const ownsTab = (s) => listed(s) && !s.quick;
/// [blocked, working] among `sessions`: running real sessions only (not quick terminals, not
/// chats' engines).
export function count(sessions) {
    const n = [0, 0];
    for (const s of sessions) {
        if (!listed(s) || s.quick || !(s.state.t === "running" || s.state.t === "starting"))
            continue;
        if (s.status?.status === "blocked")
            n[0]++;
        else if (s.status?.status === "working")
            n[1]++;
    }
    return n;
}
const cmp = (a, b) => (a < b ? -1 : a > b ? 1 : 0);
/// The tabs in use, in strip order (after `all`): this machine's first, then each other
/// machine's by name; within a machine `default` first, then the rest alphabetically. `here` is
/// this machine's own name, used in labels only.
export function tabs(workspaces, sessions, here) {
    const all = [...sessions];
    const scopes = [];
    for (const s of [...[...workspaces].map(scopeOfWorkspace), ...all.filter(ownsTab).map(scopeOf)]) {
        if (!scopes.some((x) => sameScope(x, s)))
            scopes.push(s);
    }
    scopes.sort((a, b) => (a.machine === b.machine ? 0 : a.machine === null ? -1 : b.machine === null ? 1 : cmp(a.machine, b.machine))
        || Number(profileName(a) !== DEFAULT_PROFILE) - Number(profileName(b) !== DEFAULT_PROFILE)
        || cmp(a.profile, b.profile));
    const machines = new Set(scopes.map((s) => s.machine)).size;
    return scopes.map((scope) => {
        const mine = scopes.filter((s) => s.machine === scope.machine).length;
        const name = scope.machine ?? here;
        const label = machines > 1 ? (mine > 1 ? `${name} [${profileName(scope)}]` : name) : `[${profileName(scope)}]`;
        const [blocked, working] = count(all.filter((s) => keepsSession(scope, s)));
        return { scope, label, blocked, working };
    });
}
/// The strip is drawn with two tabs or more besides `all`.
export const drawn = (t) => t.length >= 2;
/// The tab to show for a saved one: it, while it is still a tab and the strip is drawn; else
/// null, which is `all`.
export function resolve(saved, t) {
    return saved && drawn(t) && t.some((x) => sameScope(x.scope, saved)) ? saved : null;
}
/// The tab to keep on going to a session: the one kept while it lists the session (`all` lists
/// everything), else the session's own. That one is always a tab while the strip is drawn, and
/// `resolve` makes it `all` when it isn't, so it holds even before everything has loaded. A quick
/// terminal goes with its worktree and never moves the tab.
export const follow = (kept, s) => !kept || s.quick || keepsSession(kept, s) ? kept : scopeOf(s);
/// The profile a form that creates something preselects under a tab: the tab's (as people read
/// it), while the machine picked is the tab's and the profile is among `names` there; else null
/// (the form's own default).
export function tabProfile(t, machine, names) {
    return t && t.machine === machine && names.includes(profileName(t)) ? profileName(t) : null;
}
/// Where something started under a tab lands in it: a workspace of the tab's machine whose own
/// profile is the tab's (`prefer`, the one the list shows, first); else the profile to ask for
/// (as people read it), in a workspace of that machine to reach it by (null: this machine's own).
export function startIn(t, workspaces, prefer) {
    const there = [...workspaces].filter((w) => mach(w.machine) === t.machine);
    const own = there.filter((w) => w.profile === t.profile);
    const w = own.find((x) => x.name === prefer) ?? own[0];
    if (w)
        return { workspace: w.name };
    return { workspace: t.machine ? there[0]?.name ?? null : null, profile: profileName(t) };
}
/// A scope as the browser keeps it (`{"profile":"work"}`, this machine's left out, as the Rust
/// one serializes), and back; anything else is `all`.
export const scopeText = (s) => s ? JSON.stringify(s.machine ? { machine: s.machine, profile: s.profile } : { profile: s.profile }) : "";
export function scopeParse(text) {
    try {
        const v = text ? JSON.parse(text) : null;
        return v && typeof v.profile === "string" ? { machine: typeof v.machine === "string" ? v.machine : null, profile: v.profile } : null;
    }
    catch {
        return null;
    }
}
