/* Orqella module SDK. Injected by the daemon into every module page. */
(() => {
  if (window.orqella) return;

  let sequence = 0;
  let initialized = false;
  let announced = false;
  let context = null;
  let contextPort = null;
  let currentWsToken = "";
  const frameNonce = (() => {
    const match = location.hash.match(/[#&]__orqella_frame=([^&]+)/);
    if (!match) return "";
    try { return decodeURIComponent(match[1]); } catch (_) { return ""; }
  })();
  const frameWsToken = (() => {
    const match = location.hash.match(/[#&]__orqella_ws=([^&]+)/);
    if (!match) return "";
    try { return decodeURIComponent(match[1]); } catch (_) { return ""; }
  })();

  // Browser WebSocket clients cannot set headers. Add the core module token to
  // same-host module WebSocket URLs so the daemon can distinguish this frame
  // from a different module (or a navigated third-party page). Isolated module
  // origins use module-bridge.js instead; Worker sockets are not brokered yet.
  const NativeWebSocket = window.WebSocket;
  // On an isolated module origin, module-bridge.js owns WebSockets and adds
  // the module token shell-side. Do not wrap the native constructor twice.
  if (NativeWebSocket && !window.__ORQELLA_MODULE_ORIGIN && !window.__ORQELLA_MODULE_NESTED) {
    const withWsToken = (value) => {
      const token = currentWsToken || frameWsToken;
      if (!token) return value;
      let target;
      try { target = new URL(String(value), location.href); } catch (_) { return value; }
      if (target.host !== location.host || !(target.protocol === "ws:" || target.protocol === "wss:")) {
        return value;
      }
      const modulePath = target.pathname.startsWith("/m/")
        || (target.pathname.startsWith("/n/") && target.pathname.includes("/m/"));
      if (!modulePath) return value;
      if (!target.searchParams.has("__orqella_ws")) {
        target.searchParams.set("__orqella_ws", token);
      }
      return target.toString();
    };
    const OrqellaWebSocket = function (url, protocols) {
      const target = withWsToken(url);
      return protocols === undefined
        ? new NativeWebSocket(target)
        : new NativeWebSocket(target, protocols);
    };
    OrqellaWebSocket.prototype = NativeWebSocket.prototype;
    for (const key of ["CONNECTING", "OPEN", "CLOSING", "CLOSED"]) {
      if (key in NativeWebSocket) OrqellaWebSocket[key] = NativeWebSocket[key];
    }
    window.WebSocket = OrqellaWebSocket;
  }
  const pending = new Map();
  const readyCallbacks = [];
  const queued = [];

  const resolveReady = () => {
    while (readyCallbacks.length) {
      try { readyCallbacks.shift()(context); } catch (error) { console.error(error); }
    }
  };

  const refreshThemeStylesheet = () => {
    for (const link of document.querySelectorAll('link[rel="stylesheet"]')) {
      let path = "";
      try { path = new URL(link.href, location.href).pathname; } catch (_) {}
      if (path !== "/assets/theme.css") continue;
      const next = link.cloneNode();
      next.href = `/assets/theme.css?v=${Date.now()}`;
      next.onload = () => {
        link.remove();
        document.dispatchEvent(new CustomEvent("orqella:theme-loaded"));
      };
      // Keep the theme ahead of module.css and the module's own stylesheet.
      // Appending it to the end of <head> would let the theme override module
      // rules on equal specificity after every live theme refresh.
      if (link.parentNode) link.parentNode.insertBefore(next, link);
      else document.head.appendChild(next);
    }
  };

  const send = (method, params) => new Promise((resolve, reject) => {
    const id = `m${++sequence}`;
    pending.set(id, { resolve, reject });
    const request = { type: "orqella:request", id, nonce: frameNonce, method, params: params || {} };
    if (contextPort) contextPort.postMessage(request);
    else parent.postMessage(request, "*");
  });

  window.orqella = {
    get context() { return context; },
    call(method, params) {
      if (!initialized) {
        return new Promise((resolve, reject) => queued.push(() => send(method, params).then(resolve, reject)));
      }
      return send(method, params);
    },
    hooks: {
      list() { return window.orqella.call("hooks.list", {}); },
      run(name, params) { return window.orqella.call("hooks.run", { name, params: params || {} }); },
    },
    ready(callback) {
      if (typeof callback !== "function") return;
      if (initialized) callback(context);
      else readyCallbacks.push(callback);
    },
    onContext(callback) {
      if (typeof callback === "function") readyCallbacks.push(callback);
    },
  };

  const finishInit = () => {
    if (initialized) return;
    initialized = true;
    document.documentElement.dataset.orqellaTheme = context?.theme || "dark";
    resolveReady();
    while (queued.length) queued.shift()();
  };

  const acceptContext = (next) => {
    context = next || context;
    if (context?.ws_token) currentWsToken = context.ws_token;
    refreshThemeStylesheet();
  };

  const acceptMessage = (message) => {
    if (!message || typeof message !== "object") return;
    if (message.type === "orqella:context") {
      acceptContext(message.context);
      finishInit();
      return;
    }
    if (message.type === "orqella:response") {
      const request = pending.get(message.id);
      if (!request) return;
      pending.delete(message.id);
      if (message.ok) request.resolve(message.result);
      else request.reject(new Error(message.error || "Orqella request failed"));
    }
  };

  window.addEventListener("message", (event) => {
    // Source identity is the security boundary. A parent can be cross-origin
    // and its document.referrer may be suppressed by Referrer-Policy, so do
    // not trust a computed parent origin here. The shell also validates the
    // module origin and the frame nonce before sending this message.
    if (event.source !== parent) return;
    const message = event.data;
    if (!message || typeof message !== "object") return;
    if (message.type === "orqella:ping") {
      if (message.nonce === frameNonce) announceReady();
      return;
    }
    if (message.type === "orqella:init") {
      context = message.context || {};
      const port = event.ports?.[0];
      if (port) {
        contextPort = port;
        contextPort.onmessage = (portEvent) => acceptMessage(portEvent.data);
        // The private context (including ws_token) follows on the port.
        return;
      }
      finishInit();
      return;
    }
    // Compatibility path for shells that predate the private port. New shells
    // never send context or responses this way.
    acceptMessage(message);
  });

  const announceReady = () => {
    if (announced) return;
    announced = true;
    parent.postMessage({ type: "orqella:frame-ready", nonce: frameNonce }, "*");
  };
  // DOMContentLoaded is enough to hand over the context. Waiting for every
  // image and stylesheet makes host APIs arrive late on slow links.
  if (document.readyState !== "loading") {
    setTimeout(announceReady, 0);
  } else {
    document.addEventListener("DOMContentLoaded", () => setTimeout(announceReady, 0), { once: true });
    // Some module documents are written after their load event. Keep a
    // fallback so a missed DOMContentLoaded cannot deadlock the frame.
    setTimeout(announceReady, 3000);
  }
})();
