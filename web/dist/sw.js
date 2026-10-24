"use strict";
// ptyro's service worker (M9.3, M9.4): keeps the app shell for offline starts, shows push
// notifications, and answers permission prompts straight from them ("Allow" / "Deny" buttons,
// where the platform shows actions) through `POST /api/request` with this device's token.
/// <reference lib="webworker" />
const sw = self;
const SHELL = "ptyro-shell-v2";
const FILES = [
    "/", "/style.css", "/manifest.webmanifest", "/icon.svg", "/icon-192.png",
    ...["app", "color", "conn", "data", "flow", "kit", "projects", "qr", "review", "runs", "screen", "sessions", "settings", "pad", "notes"].map((m) => `/${m}.js`),
];
sw.addEventListener("install", (e) => {
    e.waitUntil(caches.open(SHELL).then((c) => c.addAll(FILES)).then(() => sw.skipWaiting()));
});
sw.addEventListener("activate", (e) => {
    e.waitUntil((async () => {
        for (const k of await caches.keys())
            if (k !== SHELL)
                await caches.delete(k);
        await sw.clients.claim();
    })());
});
// Network first (the app updates with ptyro), the cached shell when offline. Never the API.
sw.addEventListener("fetch", (e) => {
    const url = new URL(e.request.url);
    if (e.request.method !== "GET" || url.origin !== location.origin || url.pathname.startsWith("/api") || url.pathname === "/ws")
        return;
    e.respondWith(fetch(e.request)
        .then((r) => {
        if (r.ok && FILES.includes(url.pathname) && !url.search) {
            const copy = r.clone();
            void caches.open(SHELL).then((c) => c.put(e.request, copy));
        }
        return r;
    })
        .catch(async () => (await caches.match(e.request)) ?? (await caches.match("/")) ?? Response.error()));
});
sw.addEventListener("push", (e) => {
    let p;
    try {
        p = e.data?.json();
    }
    catch {
        p = { title: "ptyro", body: e.data?.text() ?? "", kind: "notification", tag: "ptyro" };
    }
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
