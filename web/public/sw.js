// Service worker: lets the app install and open offline.
// - The page, lifeui.js and the manifest are NETWORK-FIRST: a new deployment shows up on the next load, and the
//   cached copy is used only when the network fails (offline). (Version 1 was cache-first for these, so a browser
//   that had visited once kept showing the old app forever.)
// - Hashed build files (web-<hash>.js / _bg.wasm) never change, so they are cache-first.
// - /api/* and non-GET requests are never touched.
// Bump CACHE when this logic changes: activate deletes every other cache.
const CACHE = "lifeui-shell-v3";
const SHELL = ["/", "/manifest.webmanifest", "/lifeui.js", "/number-flow.js"];
const NETWORK_FIRST = new Set(SHELL);

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(CACHE).then((cache) => cache.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches.keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

async function networkFirst(request) {
  try {
    const response = await fetch(request, { cache: "no-cache" });
    if (response.ok) {
      const copy = response.clone();
      caches.open(CACHE).then((c) => c.put(request, copy));
    }
    return response;
  } catch (error) {
    const hit = await caches.match(request);
    if (hit) return hit;
    throw error;
  }
}

async function cacheFirst(request) {
  const hit = await caches.match(request);
  if (hit) return hit;
  const response = await fetch(request);
  if (response.ok) {
    const copy = response.clone();
    caches.open(CACHE).then((c) => c.put(request, copy));
  }
  return response;
}

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (event.request.method !== "GET" || url.origin !== location.origin || url.pathname.startsWith("/api/")) return;
  const pageOrShell = event.request.mode === "navigate" || NETWORK_FIRST.has(url.pathname);
  event.respondWith(pageOrShell ? networkFirst(event.request) : cacheFirst(event.request));
});
