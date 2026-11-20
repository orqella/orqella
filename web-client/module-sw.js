/* Generic service worker for a per-module browser origin. It holds no module
   credentials and relays module traffic through the page bridge. */
"use strict";

const MODULE_PAGE_CSP = "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' blob: data:; connect-src 'self'; worker-src 'self' blob:; frame-src 'self'; base-uri 'self'; form-action 'self'; frame-ancestors 'self' http://orqella.localhost:* http://*.orqella.localhost:* http://127.0.0.1:* http://localhost:* https://orqella-web.fly.dev https://orqella-web-staging.fly.dev";

const registrations = new Map();
const bridgeClients = new Set();
let bridgeReady = false;
let bridgeResolve = null;
let bridgePromise = null;

function resetBridge() {
  bridgeReady = false;
  bridgeResolve = null;
  bridgePromise = null;
}

async function pingBridges() {
  const all = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  for (const client of all) {
    if (!isModuleClient(client)) continue;
    try { client.postMessage({ type: "orqella-module-bridge-ping" }); } catch (_) {}
  }
}

async function waitForBridge(timeoutMs = 20000) {
  if (bridgeReady) return true;
  if (!bridgePromise) {
    bridgePromise = new Promise((resolve) => {
      bridgeResolve = resolve;
    });
    // Service workers are terminated aggressively on iOS. A live bridge can
    // be asked to re-announce readiness and its registration.
    pingBridges().catch(() => {});
  }
  const current = bridgePromise;
  const result = await Promise.race([
    current,
    new Promise((resolve) => setTimeout(() => resolve(false), timeoutMs)),
  ]);
  if (!result && bridgePromise === current) {
    bridgePromise = null;
    bridgeResolve = null;
  }
  return Boolean(result);
}

function isModulePath(path) {
  return path === "/assets/theme.css"
    || path === "/assets/module.css"
    || path === "/assets/logo.png"
    || path.startsWith("/assets/themes/")
    || path.startsWith("/m/")
    || path.startsWith("/n/");
}

self.addEventListener("install", () => {
  self.skipWaiting();
});

self.addEventListener("activate", (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("message", (event) => {
  const data = event.data;
  if (!data) return;
  if (data.type === "orqella-module-bridge-ready") {
    if (event.source?.id) bridgeClients.add(event.source.id);
    bridgeReady = true;
    if (bridgeResolve) bridgeResolve(true);
    return;
  }
  if (data.type !== "orqella-module-register") return;
  if (!event.source?.id || !data.module || !data.nonce) return;
  bridgeClients.add(event.source.id);
  const surface = data.surface || "page";
  registrations.set(`${data.module}\u0000${surface}`, {
    module: data.module,
    nonce: data.nonce,
    surface,
    path: data.path || "",
    clientId: event.source.id,
  });
});

function registrationFor(moduleId, surface) {
  return registrations.get(`${moduleId}\u0000${surface}`)
    || registrations.get(`${moduleId}\u0000page`)
    || [...registrations.values()].find((entry) => entry.module === moduleId)
    || null;
}

function surfaceFromPath(path, moduleId) {
  const prefix = `/m/${encodeURIComponent(moduleId)}`;
  const normalized = path.length > 1 ? path.replace(/\/+$/, "") : path;
  if (normalized === `${prefix}/overview`) return "overview";
  if (normalized === `${prefix}/settings`) return "settings";
  return "page";
}

function moduleIdFromPath(path) {
  const match = /^\/m\/([^/]+)/.exec(path);
  if (!match) return "";
  try { return decodeURIComponent(match[1]); } catch (_) { return match[1]; }
}

function isModuleClient(client) {
  try {
    const path = new URL(client.url).pathname;
    return path === "/module-bootstrap.html" || path.startsWith("/m/");
  } catch (_) {
    return false;
  }
}

async function relayClients(clientId = "") {
  const seen = new Set();
  const clients = [];
  if (clientId && bridgeClients.has(clientId)) {
    const preferred = await self.clients.get(clientId);
    if (preferred && isModuleClient(preferred)) {
      seen.add(preferred.id);
      clients.push(preferred);
    }
  }
  // iOS Safari has not always included nested frames in matchAll. Ask the
  // clients that identified themselves to this worker first.
  for (const id of bridgeClients) {
    const client = await self.clients.get(id);
    if (!client) {
      bridgeClients.delete(id);
      continue;
    }
    if (!isModuleClient(client)) continue;
    seen.add(client.id);
    clients.push(client);
  }
  if (!clients.length) {
    const all = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
    for (const client of all) {
      if (seen.has(client.id) || !isModuleClient(client)) continue;
      clients.push(client);
    }
  }
  return clients;
}

function askClient(client, request) {
  return new Promise((resolve, reject) => {
    const channel = new MessageChannel();
    const timer = setTimeout(() => reject(new Error("module bridge timed out")), 10000);
    channel.port1.onmessage = (event) => {
      clearTimeout(timer);
      resolve(event.data);
    };
    try {
      client.postMessage(
        { type: "orqella-module-fetch", request },
        [channel.port2],
      );
    } catch (error) {
      clearTimeout(timer);
      reject(error);
    }
  });
}

async function relay(request, clientId = "") {
  await waitForBridge();
  const url = new URL(request.url);
  const requestBody = ["GET", "HEAD"].includes(request.method)
    ? null
    : await request.arrayBuffer();
  const headers = Object.fromEntries(request.headers.entries());
  const payload = {
    path: `${url.pathname}${url.search}`,
    method: request.method,
    headers,
    body: requestBody,
  };

  // A module document navigation must go back through the bootstrap, not
  // through a raw relayed document. Otherwise the replacement document loses
  // the broker bridge and its SDK context.
  let fromModulePage = false;
  if (request.mode === "navigate" && request.referrer) {
    try { fromModulePage = new URL(request.referrer).origin === self.location.origin; } catch (_) {}
  }
  if (request.mode === "navigate") {
    const moduleId = moduleIdFromPath(url.pathname);
    const prefix = moduleId ? `/m/${encodeURIComponent(moduleId)}` : "";
    const path = url.pathname.replace(/\/+$/, "");
    const canonicalTop = Boolean(prefix)
      && (path === prefix || path === `${prefix}/overview` || path === `${prefix}/settings`);
    if (moduleId && (!fromModulePage || canonicalTop)) {
      const surface = surfaceFromPath(url.pathname, moduleId);
      const registration = registrationFor(moduleId, surface);
      if (registration) {
        const target = new URL("/module-bootstrap.html", url.origin);
        target.searchParams.set("module", moduleId);
        target.searchParams.set("surface", surface);
        target.searchParams.set("nonce", registration.nonce);
        if (surface === "page") {
          const prefix = `/m/${encodeURIComponent(moduleId)}`;
          const inner = url.pathname.startsWith(prefix)
            ? (url.pathname.slice(prefix.length) || "/")
            : "/";
          target.searchParams.set("path", `${inner}${url.search}`);
        }
        return Response.redirect(target.toString(), 302);
      }
    }
  }

  const clients = await relayClients(clientId);
  const idempotent = ["GET", "HEAD", "OPTIONS"].includes(request.method);
  let lastError = null;
  for (const client of clients) {
    try {
      const result = await askClient(client, payload);
      if (!result) continue;
      if (result.error) {
        lastError = new Error(result.error);
        if (!idempotent) break;
        continue;
      }
      const responseHeaders = new Headers();
      for (const [name, value] of Object.entries(result.headers || {})) {
        const lower = name.toLowerCase();
        if (["content-length", "content-encoding", "transfer-encoding", "connection", "set-cookie"].includes(lower)) {
          continue;
        }
        try { responseHeaders.set(name, value); } catch (_) {}
      }
      const status = result.status || 200;
      let body = [204, 205, 304].includes(status) ? null : (result.body || null);
      const contentType = responseHeaders.get("content-type") || "";
      if (request.mode === "navigate" && body && contentType.includes("text/html")) {
        let html = new TextDecoder().decode(body);
        const inject = '<script>window.__ORQELLA_MODULE_NESTED=true;</script><script src="/module-bridge.js"></script>';
        html = /<head(\s[^>]*)?>/i.test(html)
          ? html.replace(/<head(\s[^>]*)?>/i, (match) => `${match}${inject}`)
          : `${inject}${html}`;
        body = new TextEncoder().encode(html).buffer;
        try { responseHeaders.set("Content-Security-Policy", MODULE_PAGE_CSP); } catch (_) {}
      }
      return new Response(body, { status, headers: responseHeaders });
    } catch (error) {
      lastError = error;
      // Never retry a mutation on another surface; that could duplicate a
      // write even though the first shell may have applied it.
      if (!idempotent) break;
    }
  }
  return new Response(
    lastError ? `module relay failed: ${lastError.message}` : "module is not connected to an Orqella shell",
    { status: 503, headers: { "content-type": "text/plain; charset=utf-8" } },
  );
}

self.addEventListener("fetch", (event) => {
  const request = event.request;
  let url;
  try { url = new URL(request.url); } catch (_) { return; }
  if (url.origin !== self.location.origin) return;
  // A new module bootstrap will announce a fresh bridge. Do not relay its
  // first stylesheet or script requests against the previous document's
  // readiness. Nested module iframes navigate normally and must not reset the
  // existing bridge.
  if (request.mode === "navigate" && url.pathname === "/module-bootstrap.html") {
    resetBridge();
  }
  if (!isModulePath(url.pathname)) return;
  if (request.method === "OPTIONS") return;
  event.respondWith(relay(request, event.clientId));
});
