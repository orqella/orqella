/* Orqella module SDK. Injected by the daemon into every module page. */
(() => {
  if (window.orqella) return;

  let sequence = 0;
  let initialized = false;
  let context = null;
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
  // from a different module (or a navigated third-party page). Modules that
  // create sockets inside a Worker bypass this wrapper and must add the token
  // from context.ws_token themselves.
  const NativeWebSocket = window.WebSocket;
  if (NativeWebSocket && frameWsToken) {
    const withWsToken = (value) => {
      let target;
      try { target = new URL(String(value), location.href); } catch (_) { return value; }
      if (target.host !== location.host || !(target.protocol === "ws:" || target.protocol === "wss:")) {
        return value;
      }
      const modulePath = target.pathname.startsWith("/m/")
        || (target.pathname.startsWith("/n/") && target.pathname.includes("/m/"));
      if (!modulePath) return value;
      if (!target.searchParams.has("__orqella_ws")) {
        target.searchParams.set("__orqella_ws", frameWsToken);
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
      document.head.appendChild(next);
    }
  };

  const send = (method, params) => new Promise((resolve, reject) => {
    const id = `m${++sequence}`;
    pending.set(id, { resolve, reject });
    parent.postMessage({ type: "orqella:request", id, nonce: frameNonce, method, params: params || {} }, "*");
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

  const parentOrigin = (() => {
    try {
      return location.ancestorOrigins?.[0]
        || (document.referrer ? new URL(document.referrer).origin : "");
    } catch (_) {
      return "";
    }
  })();

  window.addEventListener("message", (event) => {
    if (event.source !== parent) return;
    if (parentOrigin && event.origin !== parentOrigin) return;
    const message = event.data;
    if (!message || typeof message !== "object") return;
    if (message.type === "orqella:init") {
      context = message.context || {};
      initialized = true;
      document.documentElement.dataset.orqellaTheme = context.theme || "dark";
      resolveReady();
      while (queued.length) queued.shift()();
      return;
    }
    if (message.type === "orqella:context") {
      context = message.context || context;
      refreshThemeStylesheet();
      return;
    }
    if (message.type === "orqella:response") {
      const request = pending.get(message.id);
      if (!request) return;
      pending.delete(message.id);
      if (message.ok) request.resolve(message.result);
      else request.reject(new Error(message.error || "Orqella request failed"));
    }
  });

  const announceReady = () => {
    parent.postMessage({ type: "orqella:frame-ready", nonce: frameNonce }, "*");
  };
  if (document.readyState === "complete") {
    setTimeout(announceReady, 0);
  } else {
    window.addEventListener("load", () => setTimeout(announceReady, 0), { once: true });
  }
})();
