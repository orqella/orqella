const BYPASS = ['/app-client', '/app-sw.js', '/ws-shim.js', '/event-source-shim.js', '/favicon.ico'];

function bypass(pathname) {
  return BYPASS.some((prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`));
}

let appMount = '';
let appAuthToken = '';
let appClientId = null;

self.addEventListener('message', (event) => {
  const source = event.source;
  if (event.data?.type !== 'orqella-app-register' || !source?.id || source.frameType !== 'top-level') return;
  try {
    const url = new URL(source.url);
    if (url.origin !== self.location.origin || !url.pathname.startsWith('/app-client/')) return;
  } catch (_) {
    return;
  }
  appMount = event.data.mount || appMount;
  appAuthToken = event.data.authToken || appAuthToken;
  appClientId = source.id;
});

async function findAppClient() {
  if (appClientId) {
    const client = await self.clients.get(appClientId);
    if (client) return client;
  }
  const clients = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
  return clients.find((client) => {
    try {
      return new URL(client.url).pathname.startsWith('/app-client/');
    } catch (_) {
      return false;
    }
  });
}

async function proxy(request) {
  const url = new URL(request.url);
  if (bypass(url.pathname)) return fetch(request);
  if (!appMount) appMount = new URLSearchParams(new URL(request.referrer).search).get('__orqella_app') || '';
  const client = await findAppClient();
  if (!client || !appMount) {
    return new Response('Orqella app transport is not ready.', { status: 503 });
  }
  const headers = Object.fromEntries(request.headers.entries());
  let body;
  if (!['GET', 'HEAD'].includes(request.method)) {
    body = new Uint8Array(await request.arrayBuffer());
  }
  const channel = new MessageChannel();
  const responsePromise = new Promise((resolve) => {
    channel.port1.onmessage = (event) => resolve(event.data);
  });
  let targetPath = `/p/${encodeURIComponent(appMount)}${url.pathname}${url.search}`;
  if (appAuthToken) {
    const target = new URL(targetPath, location.origin);
    if (!target.searchParams.has('auth_token')) target.searchParams.set('auth_token', appAuthToken);
    targetPath = `${target.pathname}${target.search}`;
  }
  client.postMessage({
    type: 'orqella-app-request',
    request: {
      method: request.method,
      path: targetPath,
      headers,
      body,
    },
  }, [channel.port2]);
  const result = await responsePromise;
  if (result?.error) {
    return new Response(result.error, { status: 502, headers: { 'content-type': 'text/plain' } });
  }
  let responseHeaders = new Headers(result.headers || {});
  let responseBody = result.body || new Uint8Array();
  const contentType = responseHeaders.get('content-type') || '';
  if (contentType.includes('text/html')) {
    let html = new TextDecoder().decode(responseBody);
    const json = (value) => JSON.stringify(value).replace(/</g, '\\u003c');
    const inline = `<script>window.__orqellaAppMount=${json(appMount)};window.__orqellaAppAuthToken=${json(appAuthToken || '')};try{navigator.serviceWorker.register=function(){return Promise.reject(new Error('app service workers are disabled in the Orqella transport'));};}catch(_){}</script><script src="/ws-shim.js?v=20261112-01"></script><script src="/event-source-shim.js?v=20261112-01"></script>`;
    html = html.replace(/<head(\s[^>]*)?>/i, (match) => `${match}${inline}`);
    responseBody = new TextEncoder().encode(html);
    responseHeaders.set('content-type', 'text/html; charset=utf-8');
  }
  responseHeaders.delete('content-length');
  responseHeaders.delete('content-encoding');
  responseHeaders.delete('transfer-encoding');
  return new Response(responseBody, { status: result.status || 200, headers: responseHeaders });
}

self.addEventListener('install', () => self.skipWaiting());
self.addEventListener('activate', (event) => event.waitUntil(self.clients.claim()));
self.addEventListener('fetch', (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin || bypass(url.pathname)) return;
  event.respondWith(proxy(request));
});
