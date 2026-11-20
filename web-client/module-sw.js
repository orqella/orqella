/* Generic service worker for a per-module browser origin. It holds no module
   credentials and relays module traffic through the page bridge. */
"use strict";

const registrations = new Map();
let bridgeReady = false;
let bridgeResolve = null;
let bridgePromise = null;

function waitForBridge(timeoutMs = 20000) {
  if (bridgeReady) return Promise.resolve(true);
  if (!bridgePromise) {
    bridgePromise = new Promise((resolve) => {
      bridgeResolve = resolve;
    });
  }
  return Promise.race([
    bridgePromise,
    new Promise((resolve) => setTimeout(() => resolve(false), timeoutMs)),
  ]);
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
    bridgeReady = true;
    if (bridgeResolve) bridgeResolve(true);
    return;
  }
  if (data.type !== "orqella-module-register") return;
  if (!event.source?.id || !data.module || !data.nonce) return;
  registrations.set(data.module, {
    nonce: data.nonce,
    surface: data.surface || "page",
    path: data.path || "",
    clientId: event.source.id,
  });
});

function registrationFor(moduleId) {
  const entry = registrations.get(moduleId);
  if (entry && entry.surface === "page") return entry;
  for (const value of registrations.values()) {
    if (value.surface === "page") return value;
  }
  return entry || null;
}

function moduleIdFromPath(path) {
  const match = /^\/m\/([^/]+)/.exec(path);
  if (!match) return "";
  try { return decodeURIComponent(match[1]); } catch (_) { return match[1]; }
}

async function relayClients() {
  const all = await self.clients.matchAll({ type: "window", includeUncontrolled: true });
  return all.filter((client) => {
    try {
      const path = new URL(client.url).pathname;
      return path === "/module-bootstrap.html" || path.startsWith("/m/");
    } catch (_) {
      return false;
    }
  });
}

function askClient(client, request) {
  return new Promise((resolve, reject) => {
    const channel = new MessageChannel();
    const timer = setTimeout(() => reject(new Error("module bridge timed out")), 20000);
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

async function relay(request) {
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
  if (request.mode === "navigate") {
    const moduleId = moduleIdFromPath(url.pathname);
    const registration = moduleId ? registrationFor(moduleId) : null;
    if (registration) {
      const prefix = `/m/${encodeURIComponent(moduleId)}`;
      const inner = url.pathname.startsWith(prefix)
        ? (url.pathname.slice(prefix.length) || "/")
        : "/";
      const target = new URL("/module-bootstrap.html", url.origin);
      target.searchParams.set("module", moduleId);
      target.searchParams.set("surface", "page");
      target.searchParams.set("nonce", registration.nonce);
      target.searchParams.set("path", `${inner}${url.search}`);
      return Response.redirect(target.toString(), 302);
    }
  }

  const clients = await relayClients();
  let lastError = null;
  for (const client of clients) {
    try {
      const result = await askClient(client, payload);
      if (!result) continue;
      if (result.error) {
        lastError = new Error(result.error);
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
      const body = [204, 205, 304].includes(status) ? null : (result.body || null);
      return new Response(body, { status, headers: responseHeaders });
    } catch (error) {
      lastError = error;
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
  if (!isModulePath(url.pathname)) return;
  if (request.method === "OPTIONS") return;
  event.respondWith(relay(request));
});
