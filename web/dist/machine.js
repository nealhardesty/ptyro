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
    document.querySelector('meta[name="apple-mobile-web-app-title"]')?.setAttribute("content", machine ? `${machine} · ptyro` : "ptyro");
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
