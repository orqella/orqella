const CLIENT_PATHS = ['/client', '/pkg', '/sw.js', '/ws-shim.js', '/app-view-shell-shim.js', '/favicon.ico'];
const PROXY_PATHS = ['/app', '/app-view', '/assets', '/api', '/m', '/p', '/healthz'];

function bypass(pathname) {
  return CLIENT_PATHS.some((prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`));
}

function shouldProxy(pathname) {
  // Remote full apps use /n/<node>/p/<mount>/; remote module pages are
  // proxied only when the shell asks the broker for one.
  if (pathname.startsWith('/n/')) {
    const kind = pathname.split('/')[3];
    return kind === 'p' || kind === 'm';
  }
  return PROXY_PATHS.some((prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`));
}

// A service worker sees one origin, not one tab. Every top-level client
// registers an unguessable transport id, and nested clients prove which tab
// owns them with the same id in their URL. There is deliberately no fallback
// to "some other tab"; a request without affinity fails closed.
const transportOwners = new Map();
const clientTransports = new Map();
const transportPorts = new Map();
const appClientMounts = new Map();

function transportFromUrl(value) {
  try {
    return new URL(value).searchParams.get('__orqella_transport') || '';
  } catch (_) {
    return '';
  }
}

function isClientPage(client) {
  try {
    const path = new URL(client.url).pathname;
    return path === '/client' || path === '/client/' || path === '/client/index.html';
  } catch (_) {
    return false;
  }
}

self.addEventListener('message', (event) => {
  const source = event.source;
  if (!source?.id) return;
  try {
    if (new URL(source.url).origin !== self.location.origin) return;
  } catch (_) {
    return;
  }
  // Identify the client page by URL, not WindowClient.frameType; Safari has
  // shipped frameType as missing or non-standard across versions.
  if (!isClientPage(source)) return;
  if (event.data?.type === 'orqella-register') {
    const transportId = typeof event.data.transportId === 'string' ? event.data.transportId : '';
    if (!transportId) return;
    transportOwners.set(transportId, source.id);
    clientTransports.set(source.id, transportId);
    if (event.ports?.[0]) {
      const previous = transportPorts.get(transportId);
      if (previous) {
        try { previous.close(); } catch (_) {}
      }
      transportPorts.set(transportId, event.ports[0]);
    }
    try {
      source.postMessage({ type: 'orqella-register-ack', transportId });
    } catch (_) {}
  }
  if (event.data?.type === 'orqella-app-register') {
    appClientMounts.set(source.id, event.data.mount);
  }
});

function mountFromPath(pathname) {
  if (!pathname.startsWith('/app-view/')) return '';
  const rest = pathname.slice('/app-view/'.length);
  const mount = rest.split('/')[0];
  if (!mount) return '';
  try { return decodeURIComponent(mount); } catch (_) { return mount; }
}

async function appMountFor(request, clientId) {
  const url = new URL(request.url);
  let mount = mountFromPath(url.pathname);
  if (!mount) mount = url.searchParams.get('__orqella_app') || '';
  if (!mount && clientId) {
    mount = appClientMounts.get(clientId) || '';
    if (!mount) {
      const appClient = await self.clients.get(clientId);
      if (appClient) {
        try {
          const clientUrl = new URL(appClient.url);
          mount = mountFromPath(clientUrl.pathname) || clientUrl.searchParams.get('__orqella_app') || '';
        } catch (_) {}
      }
    }
  }
  if (!mount && request.referrer) {
    try {
      const referrer = new URL(request.referrer);
      mount = mountFromPath(referrer.pathname) || referrer.searchParams.get('__orqella_app') || '';
    } catch (_) {}
  }
  return mount;
}

function shellTargetPath(pathname, search) {
  if (pathname === '/app') return `/${search || ''}`;
  if (pathname.startsWith('/app/')) return `${pathname.slice(4)}${search || ''}`;
  return `${pathname}${search || ''}`;
}

function appTargetPath(url, mount) {
  const prefix = `/app-view/${encodeURIComponent(mount)}`;
  let pathname = url.pathname;
  if (pathname === prefix) pathname = '/';
  else if (pathname.startsWith(`${prefix}/`)) pathname = pathname.slice(prefix.length);
  const params = new URLSearchParams(url.search);
  params.delete('__orqella_app');
  const search = params.toString();
  return `/p/${encodeURIComponent(mount)}${pathname}${search ? `?${search}` : ''}`;
}

async function ownerFromToken(token) {
  const ownerId = transportOwners.get(token);
  if (ownerId) {
    const owner = await self.clients.get(ownerId);
    if (owner) return owner;
  }
  // iOS can deliver the iframe request before the top page's registration
  // message reaches the worker. The top page also puts the transport id in
  // its own URL, so find the owning top-level client directly.
  const all = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
  for (const client of all) {
    if (!isClientPage(client)) continue;
    if (transportFromUrl(client.url) === token) return client;
  }
  return null;
}

async function ownerClient(request, clientId) {
  const candidates = [];
  if (clientId) {
    const client = await self.clients.get(clientId);
    if (client) {
      if (isClientPage(client) && clientTransports.has(client.id)) {
        return client;
      }
      candidates.push(client.url);
    }
  }
  if (request.referrer) candidates.push(request.referrer);
  // The first shell navigation carries the transport id in its own URL, not
  // in the referrer. Nested assets later carry it in the iframe document URL.
  if (request.url) candidates.push(request.url);
  for (const candidate of candidates) {
    const token = transportFromUrl(candidate);
    if (!token) continue;
    const owner = await ownerFromToken(token);
    if (owner) return owner;
  }
  return null;
}

function requestOverTarget(target, payload) {
  return new Promise((resolve, reject) => {
    const channel = new MessageChannel();
    const timer = setTimeout(() => reject(new Error('transport request timed out')), 15000);
    channel.port1.onmessage = (event) => {
      clearTimeout(timer);
      resolve(event.data);
    };
    try {
      target.postMessage(payload, [channel.port2]);
    } catch (error) {
      clearTimeout(timer);
      reject(error);
    }
  });
}

async function proxy(request, clientId, appMount, ownerTransportId) {
  const url = new URL(request.url);
  if (!appMount && !shouldProxy(url.pathname)) return fetch(request);
  if (bypass(url.pathname)) return fetch(request);
  const targetPath = appMount ? appTargetPath(url, appMount) : shellTargetPath(url.pathname, url.search);

  const top = await ownerClient(request, clientId);
  const requestTransportId = transportFromUrl(request.url)
    || transportFromUrl(request.referrer)
    || ownerTransportId
    || '';
  const transportId = (top && clientTransports.get(top.id)) || requestTransportId;
  const headers = request.headers ? Object.fromEntries(request.headers.entries()) : {};
  let body;
  if (!['GET', 'HEAD'].includes(request.method)) {
    body = new Uint8Array(await request.arrayBuffer());
  }
  const payload = {
    type: 'orqella-http',
    request: {
      url: request.url,
      path: targetPath,
      method: request.method,
      headers,
      body,
    },
  };
  let result;
  const transportPort = transportId ? transportPorts.get(transportId) : null;
  if (transportPort) {
    try {
      result = await requestOverTarget(transportPort, payload);
    } catch (_) {
      transportPorts.delete(transportId);
    }
  }
  if (!result) {
    if (!top) {
      try {
        const openClients = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
        console.warn('[orqella-sw] no transport owner', {
          token: requestTransportId,
          clients: openClients.map((client) => ({
            id: client.id,
            frameType: client.frameType || 'unknown',
            url: client.url,
          })),
        });
      } catch (_) {}
      return new Response('Orqella transport page is not open for this tab.', { status: 503 });
    }
    result = await requestOverTarget(top, payload);
  }
  if (result?.error) {
    return new Response(result.error, { status: 502, headers: { 'content-type': 'text/plain' } });
  }
  let responseHeaders = new Headers(result.headers || {});
  let responseBody = result.body || new Uint8Array();
  const contentType = responseHeaders.get('content-type') || '';
  if (contentType.includes('text/html')) {
    let html = new TextDecoder().decode(responseBody);
    let headScripts;
    const json = (value) => JSON.stringify(value).replace(/</g, '\\u003c');
    const transportScript = transportId
      ? `<script>(function(){try{var u=new URL(location.href);if(!u.searchParams.get('__orqella_transport')){u.searchParams.set('__orqella_transport',${json(transportId)});history.replaceState(null,'',u.toString());}}catch(_){}})();</script>`
      : '';
    if (appMount) {
      const prefix = `/p/${encodeURIComponent(appMount)}`;
      let appPath = targetPath.startsWith(prefix) ? targetPath.slice(prefix.length) : targetPath;
      if (!appPath.startsWith('/')) appPath = `/${appPath}`;
      const mountJson = json(appMount);
      const appPathJson = json(appPath);
      headScripts = `${transportScript}<script>(function(){try{var mount=${mountJson};var path=${appPathJson};window.__orqellaAppMount=mount;var params=new URLSearchParams(location.search);params.set('__orqella_app',mount);history.replaceState(null,'',path+(path.indexOf('?')>=0?'&':'?')+'__orqella_app='+encodeURIComponent(mount)+(location.hash||''));document.documentElement.dataset.orqellaAppShim=mount;document.documentElement.dataset.orqellaSwController=String(!!navigator.serviceWorker.controller);try{navigator.serviceWorker.register=function(){return Promise.reject(new Error('app service workers are disabled in the Orqella transport'));};}catch(_){}var announce=function(){navigator.serviceWorker.controller&&navigator.serviceWorker.controller.postMessage({type:'orqella-app-register',mount:mount});fetch('/__orqella_app_register/'+encodeURIComponent(mount),{cache:'no-store'}).catch(function(){});};announce();navigator.serviceWorker.addEventListener&&navigator.serviceWorker.addEventListener('controllerchange',announce);}catch(_){}})();</script><script src="/ws-shim.js"></script>`;
    } else {
      headScripts = `${transportScript}<script src="/app-view-shell-shim.js?v=20261112-01"></script>`;
    }
    html = html.replace(/<head(\s[^>]*)?>/i, (match) => `${match}${headScripts}`);
    if (!appMount && !html.includes('/ws-shim.js')) {
      html = html.replace('</body>', '<script src="/ws-shim.js"></script></body>');
      if (!html.includes('/ws-shim.js')) html += '<script src="/ws-shim.js"></script>';
    }
    responseBody = new TextEncoder().encode(html);
    responseHeaders.set('content-type', 'text/html; charset=utf-8');
  }
  responseHeaders.delete('content-length');
  responseHeaders.delete('content-encoding');
  responseHeaders.delete('transfer-encoding');
  return new Response(responseBody, {
    status: result.status || 200,
    headers: responseHeaders,
  });
}

self.addEventListener('install', (event) => {
  self.skipWaiting();
});

self.addEventListener('activate', (event) => {
  event.waitUntil(self.clients.claim());
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  if (request.mode === 'navigate' && (url.pathname === '/client' || url.pathname === '/client/' || url.pathname === '/client/index.html')) {
    event.respondWith(fetch(request));
    return;
  }
  if (url.pathname === '/__orqella_clear_site_data') {
    event.respondWith(new Response(null, {
      status: 204,
      headers: { 'Clear-Site-Data': '"cookies", "cache"' },
    }));
    return;
  }
  if (url.pathname.startsWith('/__orqella_app_register/')) {
    const mount = decodeURIComponent(url.pathname.slice('/__orqella_app_register/'.length));
    if (event.clientId && mount) {
      appClientMounts.set(event.clientId, mount);
    }
    event.respondWith(new Response(null, { status: 204 }));
    return;
  }
  if (bypass(url.pathname)) return;
  event.respondWith((async () => {
    try {
      const appMount = await appMountFor(request, event.clientId);
      console.log('[orqella-sw]', url.pathname, request.mode, event.clientId, appMount);
      if (!appMount && !shouldProxy(url.pathname) && request.mode !== 'navigate') {
        return fetch(request);
      }
      return proxy(request, event.clientId, appMount);
    } catch (error) {
      console.error('[orqella-sw] proxy failed', error);
      return new Response(`Orqella transport error: ${error?.stack || error}`, {
        status: 502,
        headers: { 'content-type': 'text/plain' },
      });
    }
  })());
});
