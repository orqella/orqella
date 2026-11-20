/* Module-origin bridge. Runs inside the module document and talks only to the
   Orqella shell broker. It owns module asset/HTTP relay and WebSockets. */
(() => {
  "use strict";

  if (window.__orqellaModuleBridge) return;
  window.__orqellaModuleBridge = true;

  const match = /^\/m\/([^/]+)/.exec(location.pathname);
  let moduleId = "";
  try { moduleId = match ? decodeURIComponent(match[1]) : ""; } catch (_) { moduleId = match ? match[1] : ""; }
  const nonceMatch = location.hash.match(/[#&]__orqella_frame=([^&]+)/);
  const nonce = nonceMatch ? decodeURIComponent(nonceMatch[1]) : "";

  const nestedPage = window.__ORQELLA_MODULE_NESTED === true;
  if (!moduleId || (!nonce && !nestedPage)) return;

  const surface = ["page", "overview", "settings"].includes(window.__ORQELLA_MODULE_SURFACE)
    ? window.__ORQELLA_MODULE_SURFACE
    : location.pathname.endsWith("/overview")
      ? "overview"
      : location.pathname.endsWith("/settings") ? "settings" : "page";

  const NativeWebSocket = window.WebSocket;
  const sockets = new Map();
  const nestedSockets = new Map();
  const pending = new Map();
  let nextId = 1;
  let port = null;

  function base64(bytes) {
    let text = "";
    for (let i = 0; i < bytes.length; i += 0x8000) {
      text += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
    }
    return btoa(text);
  }

  function bytes(value) {
    const text = atob(value || "");
    const out = new Uint8Array(text.length);
    for (let i = 0; i < text.length; i += 1) out[i] = text.charCodeAt(i);
    return out;
  }

  if (window.__ORQELLA_MODULE_NESTED === true) {
    installNestedBridge();
    return;
  }

  function installNestedBridge() {
    const parentWindow = window.parent;
    let broker;
    try { broker = parentWindow?.__orqellaModuleBroker; } catch (_) { broker = null; }
    if (!broker) return;

    function emit(socket, type, event) {
      if (typeof socket[`on${type}`] === "function") socket[`on${type}`](event);
      try { socket.dispatchEvent(event); } catch (_) {}
    }

    class NestedWebSocket extends EventTarget {
      static CONNECTING = 0;
      static OPEN = 1;
      static CLOSING = 2;
      static CLOSED = 3;

      constructor(url, protocols) {
        super();
        this.url = String(url);
        this.protocol = "";
        this.extensions = "";
        this.binaryType = "blob";
        this.readyState = NestedWebSocket.CONNECTING;
        this.onopen = null;
        this.onmessage = null;
        this.onerror = null;
        this.onclose = null;
        let parsed;
        try { parsed = new URL(this.url, location.href); } catch (_) { parsed = null; }
        const local = parsed
          && (parsed.protocol === "ws:" || parsed.protocol === "wss:")
          && parsed.pathname.startsWith(`/m/${encodeURIComponent(moduleId)}/`)
          && /\/ws(?:\/|$)/.test(parsed.pathname);
        if (!local) {
          this._native = new NativeWebSocket(url, protocols);
          this._native.binaryType = this.binaryType;
          this._native.onopen = (event) => emit(this, "open", event);
          this._native.onmessage = (event) => emit(this, "message", event);
          this._native.onerror = (event) => emit(this, "error", event);
          this._native.onclose = (event) => emit(this, "close", event);
          return;
        }
        const list = protocols == null ? [] : Array.isArray(protocols) ? protocols : [protocols];
        this._id = broker.open(`${parsed.pathname}${parsed.search}`, list, {
          open: () => {
            this.readyState = NestedWebSocket.OPEN;
            emit(this, "open", new Event("open"));
          },
          message: (kind, data) => {
            let payload;
            if (kind === "text") payload = data;
            else payload = this.binaryType === "arraybuffer" ? data.buffer : new Blob([data]);
            emit(this, "message", messageEvent(payload));
          },
          error: () => emit(this, "error", new Event("error")),
          close: (code, reason, clean) => {
            this.readyState = NestedWebSocket.CLOSED;
            emit(this, "close", closeEvent(code || 1000, reason || "", clean !== false));
          },
        });
      }

      send(data) {
        if (this._native) return this._native.send(data);
        if (this.readyState !== NestedWebSocket.OPEN) {
          throw new DOMException("WebSocket is not open", "InvalidStateError");
        }
        if (typeof data === "string") {
          broker.send(this._id, "text", data);
          return;
        }
        if (data instanceof Blob) {
          data.arrayBuffer().then((buffer) => broker.send(this._id, "binary", new Uint8Array(buffer)));
          return;
        }
        if (data instanceof ArrayBuffer) {
          broker.send(this._id, "binary", new Uint8Array(data));
          return;
        }
        if (ArrayBuffer.isView(data)) {
          broker.send(this._id, "binary", new Uint8Array(data.buffer, data.byteOffset, data.byteLength));
          return;
        }
        throw new TypeError("Unsupported WebSocket payload");
      }

      close(code = 1000, reason = "") {
        if (this._native) return this._native.close(code, reason);
        if (this.readyState >= NestedWebSocket.CLOSING) return;
        this.readyState = NestedWebSocket.CLOSING;
        broker.close(this._id, code, reason);
      }
    }
    try { window.WebSocket = NestedWebSocket; } catch (_) {}
  }

  function request(type, payload) {
    const id = String(nextId++);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        pending.delete(id);
        reject(new Error("module broker request timed out"));
      }, 30000);
      pending.set(id, {
        resolve(value) {
          clearTimeout(timer);
          resolve(value);
        },
        reject(error) {
          clearTimeout(timer);
          reject(error);
        },
      });
      port.postMessage({ type, id, ...payload });
    });
  }

  function notify(type, payload) {
    port.postMessage({ type, ...payload });
  }

  function emit(socket, type, event) {
    if (typeof socket[`on${type}`] === "function") socket[`on${type}`](event);
    try { socket.dispatchEvent(event); } catch (_) {}
  }

  function messageEvent(data) {
    try { return new MessageEvent("message", { data, origin: location.origin }); } catch (_) {
      const event = new Event("message");
      Object.defineProperty(event, "data", { value: data });
      return event;
    }
  }

  function closeEvent(code, reason, clean) {
    try { return new CloseEvent("close", { code, reason, wasClean: clean }); } catch (_) {
      const event = new Event("close");
      Object.defineProperties(event, {
        code: { value: code },
        reason: { value: reason },
        wasClean: { value: clean },
      });
      return event;
    }
  }

  class BrokerWebSocket extends EventTarget {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;

    constructor(url, protocols) {
      super();
      this.url = String(url);
      this.protocol = "";
      this.extensions = "";
      this.binaryType = "blob";
      this.readyState = BrokerWebSocket.CONNECTING;
      this.onopen = null;
      this.onmessage = null;
      this.onerror = null;
      this.onclose = null;
      let parsed;
      try { parsed = new URL(this.url, location.href); } catch (_) { parsed = null; }
      const local = parsed
        && (parsed.protocol === "ws:" || parsed.protocol === "wss:")
        && parsed.pathname.startsWith(`/m/${encodeURIComponent(moduleId)}/`)
        && /\/ws(?:\/|$)/.test(parsed.pathname);
      if (!local) {
        // There is no direct socket path from a module origin. Keep the native
        // constructor for compatibility; CSP and the shell broker reject it.
        this._native = new NativeWebSocket(url, protocols);
        this._native.binaryType = this.binaryType;
        this._native.onopen = (event) => emit(this, "open", event);
        this._native.onmessage = (event) => emit(this, "message", event);
        this._native.onerror = (event) => emit(this, "error", event);
        this._native.onclose = (event) => emit(this, "close", event);
        return;
      }
      this._id = `ws-${nextId++}`;
      sockets.set(this._id, this);
      const list = protocols == null ? [] : Array.isArray(protocols) ? protocols : [protocols];
      notify("ws.open", {
        id: this._id,
        path: `${parsed.pathname}${parsed.search}`,
        protocols: list,
      });
    }

    send(data) {
      if (this._native) return this._native.send(data);
      if (this.readyState !== BrokerWebSocket.OPEN) {
        throw new DOMException("WebSocket is not open", "InvalidStateError");
      }
      if (typeof data === "string") {
        notify("ws.send", { id: this._id, kind: "text", data });
        return;
      }
      if (data instanceof Blob) {
        data.arrayBuffer().then((buffer) => {
          notify("ws.send", { id: this._id, kind: "binary", data: base64(new Uint8Array(buffer)) });
        });
        return;
      }
      let view;
      if (data instanceof ArrayBuffer) view = new Uint8Array(data);
      else if (ArrayBuffer.isView(data)) view = new Uint8Array(data.buffer, data.byteOffset, data.byteLength);
      else throw new TypeError("Unsupported WebSocket payload");
      notify("ws.send", { id: this._id, kind: "binary", data: base64(view) });
    }

    close(code = 1000, reason = "") {
      if (this._native) return this._native.close(code, reason);
      if (this.readyState >= BrokerWebSocket.CLOSING) return;
      this.readyState = BrokerWebSocket.CLOSING;
      notify("ws.close", { id: this._id, code, reason });
    }
  }

  function applySocketEvent(message) {
    const socket = sockets.get(message.id);
    if (!socket) return;
    if (message.event === "open") {
      socket.readyState = BrokerWebSocket.OPEN;
      emit(socket, "open", new Event("open"));
      return;
    }
    if (message.event === "message") {
      let data;
      if (message.kind === "text") data = message.data;
      else {
        const view = bytes(message.data);
        data = socket.binaryType === "arraybuffer" ? view.buffer : new Blob([view]);
      }
      emit(socket, "message", messageEvent(data));
      return;
    }
    if (message.event === "error") {
      emit(socket, "error", new Event("error"));
      return;
    }
    if (message.event === "close") {
      socket.readyState = BrokerWebSocket.CLOSED;
      sockets.delete(message.id);
      emit(socket, "close", closeEvent(message.code || 1000, message.reason || "", message.clean !== false));
    }
  }

  function dispatch(event) {
    const message = event.data;
    if (!message || typeof message !== "object") return;
    if (message.type === "http.response" || message.type === "orqella:module-broker-ok") {
      const waiter = pending.get(message.id);
      if (!waiter) return;
      pending.delete(message.id);
      if (message.type === "http.response") waiter.resolve(message);
      else waiter.resolve(message);
      return;
    }
    if (message.type === "ws.event") {
      const nested = nestedSockets.get(message.id);
      if (nested) {
        if (message.event === "open") nested.open?.();
        else if (message.event === "message") {
          nested.message?.(message.kind, message.kind === "binary" ? bytes(message.data) : message.data);
        } else if (message.event === "error") nested.error?.();
        else if (message.event === "close") {
          nestedSockets.delete(message.id);
          nested.close?.(message.code, message.reason, message.clean);
        }
        return;
      }
      applySocketEvent(message);
      return;
    }
    if (message.type === "orqella:module-broker-error") {
      for (const waiter of pending.values()) waiter.reject(new Error(message.error || "module broker error"));
      pending.clear();
    }
  }

  const channel = new MessageChannel();
  port = channel.port1;
  port.onmessage = dispatch;
  port.start?.();
  parent.postMessage(
    { type: "orqella:module-broker", module: moduleId, nonce },
    "*",
    [channel.port2],
  );

  window.__orqellaModuleBroker = {
    open(path, protocols, handlers) {
      const id = `nested-${nextId++}`;
      nestedSockets.set(id, handlers || {});
      notify("ws.open", {
        id,
        path: String(path || ""),
        protocols: Array.isArray(protocols) ? protocols : [],
      });
      return id;
    },
    send(id, kind, data) {
      if (kind === "binary") {
        notify("ws.send", { id, kind, data: base64(new Uint8Array(data)) });
      } else {
        notify("ws.send", { id, kind: "text", data: String(data ?? "") });
      }
    },
    close(id, code, reason) {
      nestedSockets.delete(id);
      notify("ws.close", { id, code: code || 1000, reason: reason || "" });
    },
  };

  const announce = (worker) => {
    if (!worker) return false;
    worker.postMessage({
      type: "orqella-module-register",
      module: moduleId,
      nonce,
      surface,
      path: "",
    });
    worker.postMessage({ type: "orqella-module-bridge-ready" });
    return true;
  };
  if (!announce(navigator.serviceWorker?.controller)) {
    navigator.serviceWorker?.ready
      .then((registration) => {
        announce(navigator.serviceWorker.controller || registration.active);
      })
      .catch(() => {});
  }
  navigator.serviceWorker?.addEventListener("controllerchange", () => {
    announce(navigator.serviceWorker.controller);
  });

  navigator.serviceWorker?.addEventListener("message", (event) => {
    const message = event.data;
    if (!message) return;
    if (message.type === "orqella-module-bridge-ping") {
      announce(navigator.serviceWorker.controller);
      return;
    }
    if (message.type !== "orqella-module-fetch") return;
    const reply = event.ports?.[0];
    if (!reply) return;
    request("http.request", message.request || {})
      .then((result) => reply.postMessage(result))
      .catch((error) => reply.postMessage({ error: String(error?.message || error) }));
  });

  // Make the broker shim the module's WebSocket implementation. The SDK
  // skips its same-origin token wrapper when __ORQELLA_MODULE_ORIGIN is set.
  try {
    window.WebSocket = BrokerWebSocket;
  } catch (_) {}
})();
