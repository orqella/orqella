import init, { WebIdentity, WebClient } from '/pkg/orqella_web_client.js?v=20261119-8';

const $ = (id) => document.getElementById(id);
const SERVICE_WORKER_URL = '/sw.js?v=20261120-1';
const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (c) => ({
  "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
}[c]));
const setup = $('setup');
const app = $('app');
const shell = $('shell');
let client = null;
let linkToken = sessionStorage.getItem('orqella.link.token') || '';
let linkDevices = [];
const REQUESTED_MACHINE = new URLSearchParams(location.search).get('machine') || '';
// One transport id per top-level tab. The service worker uses it to keep
// parallel hub tabs from sharing each other's HTTP transport.
const TRANSPORT_ID = (() => {
  try {
    let value = sessionStorage.getItem('orqella.web.transport');
    if (!value) {
      value = crypto.randomUUID
        ? crypto.randomUUID()
        : Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) => byte.toString(16).padStart(2, '0')).join('');
      sessionStorage.setItem('orqella.web.transport', value);
    }
    return value;
  } catch (_) {
    return Array.from(crypto.getRandomValues(new Uint8Array(16)), (byte) => byte.toString(16).padStart(2, '0')).join('');
  }
})();
try {
  const transportUrl = new URL(location.href);
  if (!transportUrl.searchParams.get('__orqella_transport')) {
    transportUrl.searchParams.set('__orqella_transport', TRANSPORT_ID);
    history.replaceState(null, '', transportUrl.toString());
  }
} catch (_) {}
let wasmReady = null;
async function ensureWasm() {
  if (!wasmReady) wasmReady = init();
  await wasmReady;
}
window.__orqellaRelay = null;
let serviceWorkerReady = null;
let transportChannel = null;
let wsSequence = 0;
window.__orqellaDebug = [];
const sockets = new Map();

function setStatus(text, bad = false) {
  const node = $('status');
  node.textContent = text;
  node.style.color = bad ? 'var(--bad)' : '';
}

function showError(error) {
  const node = $('error');
  node.hidden = false;
  let text = String(error?.stack || error);
  const diag = window.__orqellaDiagnostics;
  if (diag) {
    text += `\nBrowser endpoint: ${diag.browser || 'unknown'}`;
    text += `\nHub endpoint: ${diag.hub || 'unknown'}`;
    text += `\nRelay hints: ${(diag.relays || []).join(', ') || 'none'}`;
    text += `\nRelay token configured: ${diag.relayToken ? 'yes' : 'no'}`;
    text += `\nRelay HTTPS reachability: ${(diag.relayReachability || []).map((probe) => `${probe.host}=${probe.ok ? 'ok' : 'failed'}`).join(', ') || 'not run'}`;
    text += `\nRelay WebSocket: ${(diag.relayReachability || []).map((probe) => `${probe.host}=${probe.websocket || 'not run'}`).join(', ') || 'not run'}`;
  }
  node.textContent = text;
}

function bytesToBase64(bytes) {
  let text = '';
  const chunk = 0x8000;
  for (let i = 0; i < bytes.length; i += chunk) {
    text += String.fromCharCode(...bytes.subarray(i, i + chunk));
  }
  return btoa(text);
}

function base64ToBytes(value) {
  const raw = atob(value);
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

const DEFAULT_APP_PORTS = { homeassistant: '8810', opencode: '8811' };

function appOriginForMount(mount) {
  if (!mount) return '';
  try {
    const configured = window.__ORQELLA_APP_ORIGINS || {};
    if (configured[mount]) return new URL(configured[mount], location.href).origin;
  } catch (_) {}
  const suffix = window.__ORQELLA_APP_ORIGIN_SUFFIX || '';
  if (suffix) return `${location.protocol}//${mount}${suffix}`;
  const port = DEFAULT_APP_PORTS[mount];
  return port ? `${location.protocol}//${location.hostname}:${port}` : '';
}

function appMountForOrigin(origin) {
  try {
    const candidate = new URL(origin).origin;
    const configured = window.__ORQELLA_APP_ORIGINS || {};
    for (const [mount, value] of Object.entries(configured)) {
      try {
        if (new URL(value, location.href).origin === candidate) return mount;
      } catch (_) {}
    }
    const suffix = window.__ORQELLA_APP_ORIGIN_SUFFIX || '';
    if (suffix) {
      const host = new URL(candidate).hostname;
      if (host.endsWith(suffix)) {
        const mount = host.slice(0, -suffix.length);
        if (mount && !mount.includes('.')) return mount;
      }
    }
    for (const [mount, port] of Object.entries(DEFAULT_APP_PORTS)) {
      if (candidate === `${location.protocol}//${location.hostname}:${port}`) return mount;
    }
  } catch (_) {}
  return '';
}

function appRequestPathAllowed(mount, path) {
  try {
    const normalized = new URL(path, location.origin).pathname;
    const prefix = `/p/${encodeURIComponent(mount)}`;
    return normalized === prefix || normalized.startsWith(`${prefix}/`);
  } catch (_) {
    return false;
  }
}

function appWsPathAllowed(mount, path) {
  try {
    const parsed = new URL(path, location.origin);
    if (parsed.pathname === '/__orqella_sse') {
      const target = parsed.searchParams.get('target') || '';
      return appRequestPathAllowed(mount, target);
    }
    return appRequestPathAllowed(mount, parsed.pathname + parsed.search);
  } catch (_) {
    return false;
  }
}

function requestHeaders(headers) {
  const out = {};
  for (const [name, value] of Object.entries(headers || {})) {
    const lower = name.toLowerCase();
    if (['host', 'origin', 'connection', 'content-length', 'transfer-encoding', 'upgrade'].includes(lower)) continue;
    out[name] = value;
  }
  return out;
}

async function proxyRequest(request) {
  const parsed = new URL(request.url);
  let path = request.path || (parsed.pathname + parsed.search);
  if (path === '/app' || path.startsWith('/app/')) {
    path = path.slice(4) || '/';
  }
  const headers = requestHeaders(request.headers || {});
  let body;
  if (!['GET', 'HEAD'].includes(request.method) && request.body) {
    body = new Uint8Array(request.body);
  }
  const raw = await client.http(request.method, path, JSON.stringify(headers), body);
  const response = JSON.parse(raw);
  return {
    status: response.status ?? 502,
    headers: response.response_headers || {},
    body: base64ToBytes(response.response_body || ''),
  };
}

async function appProxyRequest(request) {
  if (!client) throw new Error('WebRTC transport is not connected');
  const headers = requestHeaders(request.headers || {});
  const body = request.body ? new Uint8Array(request.body) : undefined;
  const raw = await client.http(request.method, request.path, JSON.stringify(headers), body);
  const response = JSON.parse(raw);
  return {
    status: response.status ?? 502,
    headers: response.response_headers || {},
    body: base64ToBytes(response.response_body || ''),
  };
}

async function topClient() {
  const all = await navigator.serviceWorker.getRegistrations();
  return all.length > 0;
}

function registerTransport(worker) {
  return new Promise((resolve) => {
    let settled = false;
    let timer;
    if (transportChannel) {
      try { transportChannel.port1.close(); } catch (_) {}
      transportChannel = null;
    }
    if (!worker) {
      resolve();
      return;
    }
    const channel = new MessageChannel();
    transportChannel = channel;
    channel.port1.onmessage = async (event) => {
      const data = event.data;
      if (data?.type !== 'orqella-http') return;
      const reply = event.ports[0];
      try {
        reply.postMessage(await proxyRequest(data.request));
      } catch (error) {
        reply.postMessage({ error: String(error?.stack || error) });
      }
    };
    const done = () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      navigator.serviceWorker.removeEventListener('message', onMessage);
      resolve();
    };
    const onMessage = (event) => {
      if (event.data?.type === 'orqella-register-ack' && event.data.transportId === TRANSPORT_ID) {
        done();
      }
    };
    navigator.serviceWorker.addEventListener('message', onMessage);
    worker.postMessage({ type: 'orqella-register', transportId: TRANSPORT_ID }, [channel.port2]);
    timer = setTimeout(done, 1500);
  });
}

async function ensureServiceWorker() {
  if (!('serviceWorker' in navigator)) throw new Error('This browser does not support service workers.');
  const registration = await navigator.serviceWorker.register(SERVICE_WORKER_URL, {
    scope: '/',
    updateViaCache: 'none',
  });
  await registration.update().catch(() => {});
  serviceWorkerReady = await navigator.serviceWorker.ready;
  if (!navigator.serviceWorker.controller) {
    await new Promise((resolve) => {
      const done = () => resolve();
      navigator.serviceWorker.addEventListener('controllerchange', done, { once: true });
      setTimeout(done, 1500);
    });
  }
  const worker = navigator.serviceWorker.controller || registration.active;
  await registerTransport(worker);
  navigator.serviceWorker.addEventListener('controllerchange', () => {
    registerTransport(navigator.serviceWorker.controller).catch(() => {});
  });
  document.addEventListener('visibilitychange', () => {
    if (!document.hidden && navigator.serviceWorker.controller) {
      registerTransport(navigator.serviceWorker.controller).catch(() => {});
    }
  });
  navigator.serviceWorker.addEventListener('message', async (event) => {
    const data = event.data;
    if (!data) return;
    if (data.type === 'orqella-http') {
      const port = event.ports[0];
      try {
        port.postMessage(await proxyRequest(data.request));
      } catch (error) {
        port.postMessage({ error: String(error?.stack || error) });
      }
    }
  });
}

function socketKey(appMount, id) {
  return `${appMount}\u0000${id}`;
}

function moduleShellWsPath(path) {
  const pathname = String(path || "").split("?", 1)[0];
  return /^\/m\/[^/]+\/ws(?:\/|$)/.test(pathname)
    || /^\/n\/[^/]+\/m\/[^/]+\/ws(?:\/|$)/.test(pathname);
}

async function handleWsControl(data, source, appMount = '') {
  if (!data) return false;
  const key = socketKey(appMount, data.id || '');
  if (data.type === 'orqella-ws-open') {
    const path = data.path || '';
    if (appMount) {
      if (!appWsPathAllowed(appMount, path)) {
        source?.postMessage({ type: 'orqella-ws-error', id: data.id, error: 'app path is outside its mount' }, '*');
        return true;
      }
    } else if (!path.startsWith('/api/') && path !== '/__orqella_sse' && !moduleShellWsPath(path)) {
      source?.postMessage({ type: 'orqella-ws-error', id: data.id, error: 'socket path is not allowed on the shell' }, '*');
      return true;
    }
    await openWs(data, source, appMount);
    return true;
  }
  if (data.type === 'orqella-ws-send') {
    const entry = sockets.get(key);
    if (!entry || entry.appMount !== appMount) return true;
    const bytes = base64ToBytes(data.data || '');
    try {
      window.__orqellaDebug.push(['send', data.kind, bytes.length]);
      await entry.sender.send(data.kind, bytes);
    } catch (error) {
      source?.postMessage({ type: 'orqella-ws-error', id: data.id, error: String(error?.stack || error) }, '*');
    }
    return true;
  }
  if (data.type === 'orqella-ws-close') {
    const entry = sockets.get(key);
    if (entry && entry.appMount !== appMount) return true;
    sockets.delete(key);
    if (entry) {
      try { await entry.sender.close(); } catch (_) {}
    }
    return true;
  }
  return false;
}

async function openWs(data, source, appMount = '') {
  const id = data.id || `ws-${++wsSequence}`;
  const key = socketKey(appMount, id);
  try {
    const handle = await client.ws_open(data.path, JSON.stringify(requestHeaders(data.headers || {})));
    const entry = { sender: handle.take_sender(), receiver: handle.take_receiver(), appMount };
    window.__orqellaDebug.push(['open', data.path]);
    sockets.set(key, entry);
    source?.postMessage({ type: 'orqella-ws-open-ok', id }, '*');
    (async () => {
      while (sockets.get(key) === entry) {
        const next = await entry.receiver.next();
        if (!next) break;
        const message = JSON.parse(next);
        source?.postMessage({
          type: 'orqella-ws-message',
          id,
          kind: message.kind,
          data: message.data,
        }, '*');
      }
      try { await entry.sender.close(); } catch (_) {}
      sockets.delete(key);
      source?.postMessage({ type: 'orqella-ws-close', id }, '*');
    })().catch((error) => {
      source?.postMessage({ type: 'orqella-ws-error', id, error: String(error?.stack || error) }, '*');
    });
  } catch (error) {
    source?.postMessage({ type: 'orqella-ws-error', id, error: String(error?.stack || error) }, '*');
  }
}

window.addEventListener('message', async (event) => {
  const data = event.data;
  if (data?.type === 'orqella-app-http') {
    const port = event.ports?.[0];
    if (!port) return;
    try {
      const appMount = appMountForOrigin(event.origin);
      if (!appMount) throw new Error('app transport origin is not allowed');
      if (!appRequestPathAllowed(appMount, data.request?.path || '')) {
        throw new Error('app request path is outside its mount');
      }
      port.postMessage(await appProxyRequest(data.request));
    } catch (error) {
      port.postMessage({ error: String(error?.stack || error) });
    }
    return;
  }
  const appMount = appMountForOrigin(event.origin);
  // Module iframes are opaque origins. The browser client does not support
  // module surfaces, so do not accept a bare `null` origin as authority.
  if (event.origin !== location.origin && !appMount) return;
  handleWsControl(data, event.source, appMount);
});



let heartbeatTimer = null;
let heartbeatFailures = 0;
let heartbeatInFlight = false;
let reconnecting = false;

async function probeRelayHttp(url) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), 5000);
  try {
    const target = new URL('/ping', url);
    await fetch(target, { mode: 'no-cors', cache: 'no-store', signal: controller.signal });
    return true;
  } catch (_) {
    return false;
  } finally {
    clearTimeout(timer);
  }
}

async function probeRelayWebSocket(url, token) {
  return new Promise((resolve) => {
    let settled = false;
    let socket;
    const finish = (result) => {
      if (settled) return;
      settled = true;
      try { socket?.close(); } catch (_) {}
      resolve(result);
    };
    try {
      const socketUrl = new URL('/relay', url);
      socketUrl.protocol = 'wss:';
      if (token) socketUrl.searchParams.set('token', token);
      socket = new WebSocket(socketUrl.toString(), ['iroh-relay-v2', 'iroh-relay-v1']);
      socket.onopen = () => finish('ok');
      socket.onerror = () => finish('failed');
      socket.onclose = (event) => finish(event.code === 1000 ? 'ok' : `closed:${event.code}`);
      setTimeout(() => finish('timeout'), 5000);
    } catch (_) {
      finish('failed');
    }
  });
}

async function probeRelayReachability(relay) {
  const urls = relay?.relay_urls || [];
  return Promise.all(urls.map(async (url) => {
    let host = url;
    try { host = new URL(url).host; } catch (_) {}
    // Do not open diagnostic WebSockets here. iOS can keep those sockets
    // around long enough to race or exhaust the real relay/bootstrap sockets.
    const https = await probeRelayHttp(url);
    return { host, ok: https, websocket: 'not probed' };
  }));
}

function stopHeartbeat() {
  if (heartbeatTimer) clearInterval(heartbeatTimer);
  heartbeatTimer = null;
  heartbeatFailures = 0;
}

function startHeartbeat() {
  stopHeartbeat();
  heartbeatTimer = setInterval(checkHeartbeat, 10000);
}

async function checkHeartbeat() {
  if (!client || reconnecting || heartbeatInFlight) return;
  heartbeatInFlight = true;
  let timer;
  try {
    const healthy = await Promise.race([
      client.http('GET', '/healthz', '{}', undefined).then((raw) => {
        const response = JSON.parse(raw);
        return response.status >= 200 && response.status < 500;
      }),
      new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('heartbeat timed out')), 8000);
      }),
    ]);
    clearTimeout(timer);
    if (!healthy) throw new Error('health check failed');
    heartbeatFailures = 0;
  } catch (_) {
    clearTimeout(timer);
    heartbeatFailures += 1;
    if (heartbeatFailures >= 2) {
      reconnectTransport().catch(() => {});
    }
  } finally {
    heartbeatInFlight = false;
  }
}

async function reconnectTransport() {
  if (reconnecting) return;
  reconnecting = true;
  stopHeartbeat();
  setStatus('Connection lost. Reconnecting…', true);
  for (const entry of sockets.values()) {
    try { await entry.sender.close(); } catch (_) {}
  }
  sockets.clear();
  if (client) {
    try {
      await Promise.race([
        client.close(),
        new Promise((resolve) => setTimeout(resolve, 3000)),
      ]);
    } catch (_) {}
  }
  client = null;
  try { localStorage.removeItem('orqella.web.identity'); } catch (_) {}
  const restored = linkToken ? await connect() : false;
  if (!restored) {
    app.hidden = true;
    setup.hidden = false;
    $('connect').disabled = false;
  }
  reconnecting = false;
}

async function prepareServiceWorker() {
  if (!('serviceWorker' in navigator)) return;
  try {
    await navigator.serviceWorker.register(SERVICE_WORKER_URL, { scope: '/', updateViaCache: 'none' });
    if (!navigator.serviceWorker.controller && sessionStorage.getItem('orqella.sw-reloaded') !== '1') {
      sessionStorage.setItem('orqella.sw-reloaded', '1');
      location.reload();
    }
  } catch (_) {}
}

// A page reload creates a new JS/WASM endpoint anyway. Keeping the old
// endpoint identity can collide with the still-closing WebRTC connection, so
// rotate it on reload and reconnect through the saved Link session.
(function rotateIdentityOnReload() {
  try {
    if (sessionStorage.getItem('orqella.page-loaded') === '1') {
      localStorage.removeItem('orqella.web.identity');
    }
    sessionStorage.setItem('orqella.page-loaded', '1');
  } catch (_) {}
})();

const LINK_ACCOUNT_URL = window.__ORQELLA_LINK_ACCOUNT_URL
  || (location.hostname.includes('staging')
    ? 'https://link.staging.orqella.io'
    : 'https://link.orqella.io');

function linkAccountUrl() {
  return LINK_ACCOUNT_URL;
}

function getIdentity() {
  const stored = localStorage.getItem('orqella.web.identity');
  if (stored) {
    try { return WebIdentity.from_secret(stored); } catch (_) {}
  }
  const identity = new WebIdentity();
  localStorage.setItem('orqella.web.identity', identity.secret_hex);
  return identity;
}

async function linkApi(path, options = {}, retried = false) {
  const headers = { 'Content-Type': 'application/json', ...(options.headers || {}) };
  if (linkToken) headers.Authorization = `Bearer ${linkToken}`;
  const response = await fetch(`${linkAccountUrl()}${path}`, { ...options, headers });
  const text = await response.text();
  let body = {};
  try { body = text ? JSON.parse(text) : {}; } catch { body = { detail: text }; }
  if (!response.ok) {
    const detail = body.detail || body.message || `HTTP ${response.status}`;
    if (response.status === 403 && !retried && /sign out and sign in again/i.test(detail)) {
      const password = window.prompt('Confirm your password to continue');
      if (password) {
        const confirm = await fetch(`${linkAccountUrl()}/v1/accounts/reauth`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${linkToken}` },
          body: JSON.stringify({ password }),
        });
        const confirmBody = await confirm.json().catch(() => ({}));
        if (!confirm.ok) {
          throw new Error(confirmBody.detail || 'Re-authentication failed');
        }
        if (confirmBody.status === 'mfa_required') {
          const code = window.prompt('Enter your authenticator code or a recovery code');
          if (!code) throw new Error('Two-factor authentication is required.');
          const verified = await fetch(`${linkAccountUrl()}/v1/accounts/reauth/mfa`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${linkToken}` },
            body: JSON.stringify({
              challenge_token: confirmBody.challenge_token,
              code,
            }),
          });
          if (!verified.ok) {
            const error = await verified.json().catch(() => ({}));
            throw new Error(error.detail || 'Two-factor authentication failed');
          }
        } else if (confirmBody.status === 'passkey_required') {
          throw new Error('Sign in again with your passkey before continuing.');
        }
        return linkApi(path, options, true);
      }
    }
    throw new Error(detail);
  }
  return body;
}

function setLinkStatus(text, bad = false) {
  const node = $('linkStatus');
  if (!node) return;
  node.textContent = text;
  node.style.color = bad ? 'var(--bad)' : '';
}

async function loadLinkDevices() {
  if (!linkToken) {
    setLinkStatus('Not signed in');
    $('machineField').hidden = true;
    return;
  }
  setLinkStatus('Loading machines…');
  const result = await linkApi('/v1/devices');
  linkDevices = (result.devices || []).filter((device) => device.web_endpoint_id);
  const select = $('machine');
  let selected = select.value;
  try { selected = selected || localStorage.getItem('orqella.web.machine') || ''; } catch (_) {}
  select.innerHTML = linkDevices.map((device) =>
    `<option value="${esc(device.web_endpoint_id)}">${esc(device.name)} · ${esc(device.kind || 'device')}</option>`
  ).join('');
  if (selected && [...select.options].some((option) => option.value === selected)) {
    select.value = selected;
  }
  $('machineField').hidden = linkDevices.length === 0;
  setLinkStatus(linkDevices.length
    ? `Signed in · ${linkDevices.length} browser-ready machine${linkDevices.length === 1 ? '' : 's'}`
    : 'Signed in · no browser-ready machines');
}

async function signInWithLink() {
  const account = linkAccountUrl();
  const popup = window.open(
    `${account}/login?origin=${encodeURIComponent(location.origin)}`,
    'orqella-link-login',
    'popup,width=520,height=760',
  );
  if (!popup) {
    setLinkStatus('Allow popups to sign in with Link.', true);
    return;
  }
  setLinkStatus('Waiting for Link sign-in…');
}

async function browserDeviceToken(identity) {
  // Reuse one Link browser device across reloads and endpoint rotations. The
  // endpoint id changes when the page reloads, so update the device record
  // best-effort instead of creating a new browser device for every load.
  let saved = '';
  try { saved = localStorage.getItem('orqella.web.browserDeviceId') || ''; } catch (_) {}
  if (saved) {
    try {
      const exchange = await linkApi('/v1/device-token', {
        method: 'POST',
        body: JSON.stringify({ device_id: saved }),
      });
      if (exchange.token) {
        const heartbeat = await fetch(`${linkAccountUrl()}/v1/devices/${encodeURIComponent(saved)}/heartbeat`, {
          method: 'POST',
          headers: {
            'Content-Type': 'application/json',
            Authorization: `Bearer ${exchange.token}`,
          },
          body: JSON.stringify({ iroh_endpoint_id: identity.endpoint_id }),
        });
        if (heartbeat.ok) return exchange.token;
        throw new Error('browser device heartbeat failed');
      }
    } catch (_) {}
  }
  const existing = await linkApi('/v1/devices');
  const known = (existing.devices || []).find((device) =>
    device.kind === 'browser' && device.iroh_endpoint_id === identity.endpoint_id
  );
  let deviceId = known?.id;
  if (!deviceId) {
    const device = await linkApi('/v1/devices', {
      method: 'POST',
      body: JSON.stringify({
        name: 'Orqella Web',
        kind: 'browser',
        iroh_endpoint_id: identity.endpoint_id,
        ssh_port: 22,
        peer_port: 9477,
      }),
    });
    deviceId = device.device?.id;
  }
  if (!deviceId) throw new Error('Link did not return a browser device id');
  try { localStorage.setItem('orqella.web.browserDeviceId', deviceId); } catch (_) {}
  const exchange = await linkApi('/v1/device-token', {
    method: 'POST',
    body: JSON.stringify({ device_id: deviceId }),
  });
  if (!exchange.token) throw new Error('Link did not issue a browser device token');
  return exchange.token;
}

async function browserRelayConfig(identity) {
  // Mint endpoint-bound relay credentials for WebRTC bootstrap/signaling.
  const deviceToken = await browserDeviceToken(identity);
  const credentials = await fetch(`${linkAccountUrl()}/v1/web/relay`, {
    method: 'POST',
    headers: {
      'Content-Type': 'application/json',
      Authorization: `Bearer ${deviceToken}`,
    },
    body: JSON.stringify({
      iroh_endpoint_id: identity.endpoint_id,
      session_token: linkToken,
    }),
  });
  const text = await credentials.text();
  let body = {};
  try { body = text ? JSON.parse(text) : {}; } catch { body = { detail: text }; }
  if (!credentials.ok) throw new Error(body.detail || `HTTP ${credentials.status}`);
  return body;
}

window.addEventListener('message', (event) => {
  if (event.data?.type !== 'orqella-link-token') return;
  const account = linkAccountUrl();
  if (event.origin !== new URL(account).origin) return;
  linkToken = event.data.token || '';
  if (!linkToken) return;
  sessionStorage.setItem('orqella.link.token', linkToken);
  resumeLinkMachine().catch((error) => setLinkStatus(String(error.message || error), true));
});

if (window.opener) {
  try {
    window.opener.postMessage(
      { type: 'orqella-browser-client-ready' },
      new URL(linkAccountUrl()).origin,
    );
  } catch (_) {}
}

async function issueWebSession() {
  const result = await linkApi('/v1/web/session', { method: 'POST', body: '{}' });
  if (!result.token) throw new Error('Link did not issue a browser session');
  return result.token;
}

async function clearHubStorage() {
  try { localStorage.clear(); } catch (_) {}
  try {
    const databases = await indexedDB.databases?.();
    for (const database of databases || []) {
      if (database.name) indexedDB.deleteDatabase(database.name);
    }
  } catch (_) {}
  try { await caches.keys().then((keys) => Promise.all(keys.map((key) => caches.delete(key)))); } catch (_) {}
  try {
    await fetch('/__orqella_clear_site_data', { cache: 'no-store' });
  } catch (_) {
    try {
      for (const cookie of document.cookie.split(';')) {
        const name = cookie.split('=')[0].trim();
        if (name) document.cookie = `${name}=; Max-Age=0; Path=/`;
      }
    } catch (_) {}
  }
}

async function connect() {
  const endpoint = $('machine')?.value || '';
  if (!linkToken) {
    setStatus('Sign in to Orqella Link first.', true);
    return false;
  }
  if (!endpoint) {
    setStatus('Choose a Link machine first.', true);
    return false;
  }
  $('connect').disabled = true;
  let previousMachine = '';
  try { previousMachine = localStorage.getItem('orqella.web.machine') || ''; } catch (_) {}
  if (previousMachine && previousMachine !== endpoint) {
    setStatus('Clearing the previous machine…');
    await clearHubStorage();
  }
  try {
    setStatus('Starting transport…');
    await ensureWasm();
    const identity = getIdentity();
    setStatus('Requesting managed relay signaling…');
    const relay = await browserRelayConfig(identity);
    const relayHints = relay.relay_urls?.join(',') || '';
    window.__orqellaDiagnostics = {
      ...(window.__orqellaDiagnostics || {}),
      browser: identity.endpoint_id,
      hub: endpoint,
      relays: (relay.relay_urls || []).map((value) => {
        try { return new URL(value).host; } catch (_) { return value; }
      }),
      relayToken: !!relay.token,
      directOnly: true,
    };
    setStatus('Checking managed relay signaling…');
    window.__orqellaDiagnostics.relayReachability = await probeRelayReachability(relay);
    setStatus('Authorizing browser session…');
    const webToken = await issueWebSession();
    setStatus('Connecting to hub…');
    client = await WebClient.connect(
      identity.secret_hex,
      endpoint,
      webToken,
      relay.relay_urls?.join(',') || '',
      relay.token || null,
      relayHints,
    );
    if (client.transport_kind?.() === 'relay') {
      try { await client.close(); } catch (_) {}
      client = null;
      throw new Error('Relay application data is disabled; direct WebRTC was not established.');
    }
    setStatus('Preparing app shell…');
    await ensureServiceWorker();
    setup.hidden = true;
    app.hidden = false;
    $('shell-error').hidden = true;
    window.__orqellaShellLoaded = false;
    shell.onload = () => { window.__orqellaShellLoaded = true; };
    shell.src = `/app/?__orqella_transport=${encodeURIComponent(TRANSPORT_ID)}`;
    document.title = 'Orqella';
    try { localStorage.setItem('orqella.web.machine', endpoint); } catch (_) {}
    setLinkStatus('Connected');
    startHeartbeat();
    setTimeout(() => {
      let loaded = window.__orqellaShellLoaded;
      try {
        loaded = loaded && !!shell.contentDocument?.body?.innerText?.trim();
      } catch (_) {
        loaded = false;
      }
      if (!loaded) $('shell-error').hidden = false;
    }, 8000);
    return true;
  } catch (error) {
    console.error(error);
    stopHeartbeat();
    setStatus('Connection failed.', true);
    showError(error);
    $('connect').disabled = false;
    return false;
  }
}


async function resumeLinkMachine() {
  await loadLinkDevices();
  let machine = REQUESTED_MACHINE;
  if (!machine) {
    try { machine = localStorage.getItem('orqella.web.machine') || ''; } catch (_) {}
  }
  if (!machine || ![...$('machine').options].some((option) => option.value === machine)) return;
  $('machine').value = machine;
  setStatus('Restoring Link session…');
  await connect();
}

async function bootLinkSession() {
  if (!linkToken) return;
  try {
    await resumeLinkMachine();
  } catch (error) {
    setLinkStatus(String(error.message || error), true);
  }
}

$('connect').addEventListener('click', connect);
$('reload-shell').addEventListener('click', () => location.reload());
$('linkSignIn').addEventListener('click', signInWithLink);
$('machine').addEventListener('change', () => {
  try { localStorage.setItem('orqella.web.machine', $('machine').value); } catch (_) {}
});
document.addEventListener('visibilitychange', () => {
  if (!document.hidden) checkHeartbeat();
});
window.addEventListener('online', () => checkHeartbeat());
window.addEventListener('pagehide', () => { try { client?.close(); } catch (_) {} });
prepareServiceWorker();
bootLinkSession();
