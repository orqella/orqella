const CLIENT_PATHS = ['/client', '/pkg', '/sw.js', '/ws-shim.js', '/app-view-shell-shim.js', '/favicon.ico'];
const PROXY_PATHS = ['/app', '/app-view', '/assets', '/api', '/m', '/sdk', '/p', '/n', '/healthz'];

function bypass(pathname) {
  return CLIENT_PATHS.some((prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`));
}

function shouldProxy(pathname) {
  return PROXY_PATHS.some((prefix) => pathname === prefix || pathname.startsWith(`${prefix}/`));
}

let registeredTopClientId = null;
const appClientMounts = new Map();
let lastAppMount = '';

self.addEventListener('message', (event) => {
  const source = event.source;
  if (!source?.id || source.frameType !== 'top-level') return;
  try {
    if (new URL(source.url).origin !== self.location.origin) return;
  } catch (_) {
    return;
  }
  if (event.data?.type === 'orqella-register') {
    registeredTopClientId = source.id;
  }
  if (event.data?.type === 'orqella-app-register') {
    appClientMounts.set(source.id, event.data.mount);
    lastAppMount = event.data.mount || lastAppMount;
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
  if (mount) lastAppMount = mount;
  return mount;
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

async function findTopClient() {
  if (registeredTopClientId) {
    const registered = await self.clients.get(registeredTopClientId);
    if (registered) return registered;
  }
  const clients = await self.clients.matchAll({ type: 'window', includeUncontrolled: true });
  const entries = clients.map((client) => {
    try {
      return { client, path: new URL(client.url).pathname };
    } catch (_) {
      return { client, path: '' };
    }
  });
  // The shell and modules are nested clients. Pick the top-level client that
  // owns the WebRTC transport, independent of whether the app was mounted at
  // /client/ or at the root of a static host.
  return entries.find(({ client, path }) =>
    client.frameType === 'top-level'
    && path !== '/app'
    && !path.startsWith('/app/')
    && !path.startsWith('/m/')
  )?.client || entries.find(({ path }) =>
    path === '/' || path === '/index.html'
    || path === '/client/' || path === '/client/index.html'
    || path.startsWith('/client/')
  )?.client;
}

async function proxy(request, clientId, appMount) {
  const url = new URL(request.url);
  if (!appMount && !shouldProxy(url.pathname)) return fetch(request);
  if (bypass(url.pathname)) return fetch(request);
  const targetPath = appMount ? appTargetPath(url, appMount) : url.pathname + url.search;

  const top = await findTopClient();
  if (!top) {
    return new Response('Orqella transport page is not open.', { status: 503 });
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
  top.postMessage({
    type: 'orqella-http',
    request: {
      url: request.url,
      path: targetPath,
      method: request.method,
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
    let headScripts;
    const json = (value) => JSON.stringify(value).replace(/</g, '\\u003c');
    if (appMount) {
      const prefix = `/p/${encodeURIComponent(appMount)}`;
      let appPath = targetPath.startsWith(prefix) ? targetPath.slice(prefix.length) : targetPath;
      if (!appPath.startsWith('/')) appPath = `/${appPath}`;
      const mountJson = json(appMount);
      const appPathJson = json(appPath);
      headScripts = `<script>(function(){try{var mount=${mountJson};var path=${appPathJson};window.__orqellaAppMount=mount;var params=new URLSearchParams(location.search);params.set('__orqella_app',mount);history.replaceState(null,'',path+(path.indexOf('?')>=0?'&':'?')+'__orqella_app='+encodeURIComponent(mount)+(location.hash||''));document.documentElement.dataset.orqellaAppShim=mount;document.documentElement.dataset.orqellaSwController=String(!!navigator.serviceWorker.controller);try{navigator.serviceWorker.register=function(){return Promise.reject(new Error('app service workers are disabled in the Orqella transport'));};}catch(_){}var announce=function(){navigator.serviceWorker.controller&&navigator.serviceWorker.controller.postMessage({type:'orqella-app-register',mount:mount});fetch('/__orqella_app_register/'+encodeURIComponent(mount),{cache:'no-store'}).catch(function(){});};announce();navigator.serviceWorker.addEventListener&&navigator.serviceWorker.addEventListener('controllerchange',announce);}catch(_){}})();</script><script src="/ws-shim.js"></script>`;
    } else {
      headScripts = '<script src="/app-view-shell-shim.js?v=20261112-01"></script>';
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
  if (url.pathname.startsWith('/__orqella_app_register/')) {
    const mount = decodeURIComponent(url.pathname.slice('/__orqella_app_register/'.length));
    if (event.clientId && mount) {
      appClientMounts.set(event.clientId, mount);
      lastAppMount = mount;
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
