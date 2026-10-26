"use strict";
// ptyro's service worker (M9.3, M9.4): keeps the app shell for offline starts, shows push
// notifications, and answers permission prompts straight from them ("Allow" / "Deny" buttons,
// where the platform shows actions) through `POST /api/request` with this device's token.
/// <reference lib="webworker" />
const sw = self;
// The shell is one generation: every file of it, named by a hash of their bytes. The cache is
// only ever replaced as a whole, so a flaky network can't give a page `app.js` from one version of
// ptyro and `settings.js` from another (the import of a name that isn't there is a blank app).
const SHELL = "ptyro-shell-";
const FILES = [
    "/", "/style.css", "/manifest.webmanifest", "/icon.svg", "/icon-192.png", "/icon-512.png", "/icon-maskable-512.png", "/apple-touch-icon.png",
    ...["app", "color", "conn", "data", "flow", "kit", "projects", "qr", "review", "runs", "screen", "sessions", "settings", "pad", "notes", "grid", "keys", "term", "panel", "cfgkit", "machine"].map((m) => `/${m}.js`),
];
const hex = (b) => [...new Uint8Array(b)].map((x) => x.toString(16).padStart(2, "0")).join("");
/// Fetch every file fresh; when all of them came, keep them as a generation of their own and
/// drop the others. Nothing changes if any one is missing.
async function snapshot() {
    const got = await Promise.all(FILES.map(async (f) => {
        const r = await fetch(f, { cache: "reload" });
        if (!r.ok)
            throw new Error(`${f}: ${r.status}`);
        return [f, r];
    }));
    const bodies = await Promise.all(got.map(([, r]) => r.clone().arrayBuffer()));
    const all = new Uint8Array(bodies.reduce((n, b) => n + b.byteLength, 0));
    let at = 0;
    for (const b of bodies) {
        all.set(new Uint8Array(b), at);
        at += b.byteLength;
    }
    const name = SHELL + hex(await crypto.subtle.digest("SHA-256", all)).slice(0, 16);
    if (!(await caches.keys()).includes(name)) {
        const c = await caches.open(name);
        for (const [f, r] of got)
            await c.put(f, r);
    }
    for (const k of await caches.keys())
        if (k.startsWith(SHELL) && k !== name)
            await caches.delete(k);
}
/// The generation to answer from when the network can't.
async function shell() {
    const name = (await caches.keys()).filter((k) => k.startsWith(SHELL)).sort().pop();
    return name ? caches.open(name) : null;
}
let refreshed = 0;
/// After a page has loaded from the network, look again at most every ten minutes: ptyro may have
/// been upgraded since the generation was made.
function refreshSoon() {
    if (Date.now() - refreshed < 600_000)
        return;
    refreshed = Date.now();
    void snapshot().catch(() => { refreshed = 0; });
}
sw.addEventListener("install", (e) => {
    e.waitUntil(snapshot().then(() => sw.skipWaiting()));
});
sw.addEventListener("activate", (e) => {
    e.waitUntil((async () => {
        // Anything from before generations had names goes.
        for (const k of await caches.keys())
            if (k.startsWith("ptyro-shell-v"))
                await caches.delete(k);
        await sw.clients.claim();
    })());
});
// Network first (the app updates with ptyro), the cached generation when offline. Never the API.
sw.addEventListener("fetch", (e) => {
    const url = new URL(e.request.url);
    if (e.request.method !== "GET" || url.origin !== location.origin || url.pathname.startsWith("/api") || url.pathname === "/ws")
        return;
    e.respondWith(fetch(e.request)
        .then((r) => { if (r.ok && url.pathname === "/")
        refreshSoon(); return r; })
        .catch(async () => (await (await shell())?.match(url.search ? url.pathname : e.request)) ?? (await (await shell())?.match("/")) ?? Response.error()));
});
sw.addEventListener("push", (e) => {
    let p;
    try {
        p = e.data?.json();
    }
    catch {
        p = { title: "ptyro", body: e.data?.text() ?? "", kind: "notification", tag: "ptyro" };
    }
    // A push with no data (a wake-up) still shows something rather than throwing.
    p ??= { title: "ptyro", body: "Something needs you", kind: "notification", tag: "ptyro" };
    const actions = p.kind === "blocked" && p.session ? [{ action: "allow", title: "Allow" }, { action: "deny", title: "Deny" }] : [];
    e.waitUntil(sw.registration.showNotification(p.title, {
        body: p.body,
        tag: p.tag,
        icon: "/icon-192.png",
        badge: "/icon-192.png",
        data: { session: p.session, kind: p.kind, since: p.since },
        requireInteraction: p.kind === "blocked",
        // `actions` isn't in every lib.dom version yet.
        ...{ actions },
    }));
});
function token() {
    return new Promise((ok) => {
        const r = indexedDB.open("ptyro", 1);
        r.onupgradeneeded = () => r.result.createObjectStore("kv");
        r.onsuccess = () => {
            const get = r.result.transaction("kv").objectStore("kv").get("token");
            get.onsuccess = () => ok(get.result ?? null);
            get.onerror = () => ok(null);
        };
        r.onerror = () => ok(null);
    });
}
sw.addEventListener("notificationclick", (e) => {
    const n = e.notification;
    const session = n.data?.session ?? undefined;
    const since = n.data?.since ?? undefined;
    n.close();
    if ((e.action === "allow" || e.action === "deny") && session) {
        e.waitUntil((async () => {
            const t = await token();
            if (!t) {
                await sw.registration.showNotification("ptyro", { body: "Open ptyro to sign in", tag: "ptyro-answer" });
                return;
            }
            const r = await fetch("/api/request", {
                method: "POST",
                headers: { "Authorization": `Bearer ${t}`, "Content-Type": "application/json" },
                body: JSON.stringify({ t: "answer", addr: session, decision: e.action, expect: since }),
            }).then((r) => r.json()).catch(() => ({ t: "err", error: { message: "ptyro is unreachable" } }));
            if (r.t === "ok")
                return;
            const body = r.error?.code === "conflict" ? "That prompt changed — open ptyro" : `Couldn't answer: ${r.error?.message ?? r.message}`;
            await sw.registration.showNotification("ptyro", { body, tag: "ptyro-answer" });
        })());
        return;
    }
    const target = session ? `/#/s/${encodeURIComponent(session)}` : "/";
    e.waitUntil((async () => {
        const wins = await sw.clients.matchAll({ type: "window", includeUncontrolled: true });
        for (const w of wins) {
            if ("focus" in w) {
                await w.navigate(target).catch(() => null);
                return void (await w.focus());
            }
        }
        await sw.clients.openWindow(target);
    })());
});
