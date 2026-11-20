(function (global) {
  "use strict";

  let state;
  let $;
  let api;
  let render;
  let currentNode;
  const ModuleOrigins = global.OrqellaModuleOrigins;

  function install(ctx) {
    ({ state, $, api, render, currentNode } = ctx);
    if (!state.moduleBrokers) state.moduleBrokers = new Map();
  }

  function moduleById(id) {
    return state.modules.find((entry) => entry.id === id) || null;
  }

  function moduleOriginFor(module) {
    return ModuleOrigins ? ModuleOrigins.originFor(module) : "";
  }

  function moduleFrameSrc(module, surface, path, nonce) {
    return ModuleOrigins ? ModuleOrigins.frameSrc(module, surface, path, nonce) : "";
  }

  function moduleFrameSandbox(module) {
    return ModuleOrigins ? ModuleOrigins.sandbox(Boolean(moduleOriginFor(module))) : "allow-scripts allow-forms allow-popups";
  }

  function moduleUsable(module) {
    return ModuleOrigins ? ModuleOrigins.usable(module) : true;
  }

  function moduleMessageAllowed(module, origin) {
    return ModuleOrigins ? ModuleOrigins.messageOriginAllowed(module, origin) : origin === "null";
  }

  function frameTargetOrigin(frame, surface, id) {
    const module = moduleById(id || frame?.dataset.frameId || "");
    return moduleOriginFor(module) || "*";
  }

  function configureModuleFrame(frame, module) {
    if (!frame) return;
    frame.setAttribute("sandbox", moduleFrameSandbox(module));
  }

  function cleanModuleInnerPath(value) {
    if (typeof value !== "string" || !value) return "/";
    if (!value.startsWith("/") || value.startsWith("//")) return "";
    if (value.includes("\\") || value.includes("\0")) return "";
    if ([...value].some((character) => character.charCodeAt(0) < 0x20)) return "";
    const path = value.split("?", 1)[0];
    if (path.split("/").some((part) => part === ".." || part === ".")) return "";
    let decoded = value;
    for (let pass = 0; pass < 2; pass += 1) {
      let next;
      try { next = decodeURIComponent(decoded); } catch (_) { return ""; }
      if (next === decoded) break;
      decoded = next;
    }
    const decodedPath = decoded.split("?", 1)[0];
    if (decodedPath.startsWith("//")) return "";
    if (decodedPath.split("/").some((part) => part === ".." || part === ".")) return "";
    return value;
  }

  function moduleDocumentPath(module, surface, path) {
    const id = encodeURIComponent(module.id);
    const prefix = module.remote_node
      ? `/n/${encodeURIComponent(module.remote_node)}/m/${id}`
      : `/m/${id}`;
    if (surface === "overview") return `${prefix}/overview`;
    if (surface === "settings") return `${prefix}/settings`;
    const inner = cleanModuleInnerPath(path);
    if (!inner) throw new Error("invalid module path");
    return `${prefix}${inner}`;
  }

  async function fetchModuleDocument(module, surface, path) {
    const target = moduleDocumentPath(module, surface, path);
    const prefix = module.remote_node
      ? `/n/${encodeURIComponent(module.remote_node)}/m/${encodeURIComponent(module.id)}`
      : `/m/${encodeURIComponent(module.id)}`;
    let resolved;
    try { resolved = new URL(target, location.origin); } catch (_) {
      throw new Error("invalid module path");
    }
    if (resolved.pathname !== prefix && !resolved.pathname.startsWith(`${prefix}/`)) {
      throw new Error("module path left its origin scope");
    }
    const response = await fetch(target, {
      credentials: "same-origin",
      cache: "no-store",
      redirect: "error",
      headers: { "X-Orqella": "1" },
    });
    const contentType = response.headers.get("content-type") || "";
    const body = await response.text();
    if (!response.ok) {
      throw new Error(body || `module document failed with HTTP ${response.status}`);
    }
    if (!contentType.includes("text/html")) {
      throw new Error("module document was not HTML");
    }
    return body;
  }

  function closeModuleBroker(surface, id) {
    const key = frameKey(surface, id);
    const entry = state.moduleBrokers?.get(key);
    if (!entry) return;
    entry.closed = true;
    for (const socket of entry.sockets?.values() || []) {
      try { socket.close(); } catch (_) {}
    }
    try { entry.port.close(); } catch (_) {}
    state.moduleBrokers.delete(key);
  }

  function frameKey(surface, id) {
    return `${surface}:${id}`;
  }

  function newFrameNonce() {
    const bytes = new Uint8Array(16);
    if (window.crypto?.getRandomValues) {
      window.crypto.getRandomValues(bytes);
    } else {
      for (let index = 0; index < bytes.length; index += 1) {
        bytes[index] = Math.floor(Math.random() * 256);
      }
    }
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  }

  function frameHash(nonce, wsToken = "") {
    const frame = `#__orqella_frame=${encodeURIComponent(nonce)}`;
    return wsToken
      ? `${frame}&__orqella_ws=${encodeURIComponent(wsToken)}`
      : frame;
  }

  function setFrameNonce(frame, surface, id) {
    if (frame.dataset.frameId) {
      const previousSurface = frame.dataset.frameSurface || surface;
      const previousId = frame.dataset.frameId;
      closeModuleBroker(previousSurface, previousId);
      state.readyFrames.delete(frameKey(previousSurface, previousId));
    }
    closeModuleBroker(surface, id);
    const nonce = newFrameNonce();
    frame.dataset.frameNonce = nonce;
    frame.dataset.frameSurface = surface;
    frame.dataset.frameId = id;
    // A document change invalidates readiness and the private context port.
    // Ping the replacement document for a fresh frame-ready instead of
    // assuming the previous document still owns the iframe.
    if (!frame.dataset.frameGuard) {
      frame.dataset.frameGuard = "1";
      frame.addEventListener("load", () => {
        const surface = frame.dataset.frameSurface || "";
        const id = frame.dataset.frameId || "";
        try {
          frame.contentWindow?.postMessage(
            { type: "orqella:ping", nonce: frame.dataset.frameNonce || "" },
            frameTargetOrigin(frame, surface, id),
          );
        } catch (_) {}
      });
    }
    state.readyFrames.delete(frameKey(surface, id));
    return nonce;
  }

  function frameNonceMatches(frame, surface, id, nonce) {
    return Boolean(frame?.dataset.frameNonce) && frame.dataset.frameNonce === nonce;
  }

  function moduleContext(id, surface) {
    const module = state.modules.find((entry) => entry.id === id) || null;
    const remote = !!module?.remote_node;
    const isolated = Boolean(moduleOriginFor(module));
    const selectedTheme = state.info?.themes?.find((theme) => theme.id === state.info?.config?.theme)
      || state.info?.themes?.[0]
      || { id: "nocturne", color_scheme: "dark" };
    // Isolated module origins never receive the module WebSocket token in
    // context; the broker injects it shell-side. Legacy opaque frames still
    // need it for their native WebSocket wrapper.
    const moduleView = isolated && module
      ? { ...module, ws_token: undefined }
      : module;
    return {
      module: moduleView,
      node: remote ? { id: "local", name: currentNode().name, remote: true } : currentNode(),
      nodes: state.nodes,
      config: module?.config || {},
      ws_token: isolated ? "" : (module?.ws_token || ""),
      surface,
      theme: selectedTheme.color_scheme || "dark",
      theme_id: selectedTheme.id,
      origin: location.origin,
    };
  }

  function sendContextTo(frame, id, surface) {
    if (!frame?.contentWindow || !state.readyFrames.has(frameKey(surface, id))) return;
    const key = frameKey(surface, id);
    const nonce = frame.dataset.frameNonce || "";
    let entry = state.framePorts?.get(key);
    if (!entry || entry.nonce !== nonce) {
      if (entry) entry.port.close();
      const channel = new MessageChannel();
      entry = { nonce, port: channel.port1 };
      state.framePorts?.set(key, entry);
      entry.port.onmessage = (event) => {
        state.frameMessage?.(frame, surface, id, event.data, entry.port);
      };
      // The window message carries no token. The private context, including
      // ws_token, only travels over the transferred port so a document that
      // navigates the iframe after the handshake cannot receive it.
      const publicContext = { ...moduleContext(id, surface), ws_token: "" };
      frame.contentWindow.postMessage(
        { type: "orqella:init", context: publicContext },
        frameTargetOrigin(frame, surface, id),
        [channel.port2],
      );
    }
    entry.port.postMessage({ type: "orqella:context", context: moduleContext(id, surface) });
  }

  function sendContext() {
    const frame = $("moduleFrame");
    if (!frame.contentWindow || !state.module) return;
    sendContextTo(frame, state.module, "page");
  }

  function sendOverviewContext(id) {
    const frame = state.overviewFrames.get(id);
    if (!frame) return;
    sendContextTo(frame, id, "overview");
  }

  function sendSettingsContext(id) {
    const frame = state.settingsFrames.get(id);
    if (!frame) return;
    sendContextTo(frame, id, "settings");
  }

  function sendThemeContext() {
    if (state.module) sendContextTo($("moduleFrame"), state.module, "page");
    for (const [id, frame] of state.overviewFrames) sendContextTo(frame, id, "overview");
    for (const [id, frame] of state.settingsFrames) sendContextTo(frame, id, "settings");
  }

  function handleModuleBootstrap(target, message, port) {
    if (!port) return;
    const module = moduleById(target?.id || "");
    const nonce = target?.nonce || "";
    if (!module
      || !message
      || message.module !== target.id
      || message.surface !== target.surface
      || message.nonce !== nonce
      || !moduleOriginFor(module)) {
      try { port.postMessage({ type: "orqella:module-error", error: "module origin is not available" }); } catch (_) {}
      try { port.close(); } catch (_) {}
      return;
    }
    fetchModuleDocument(module, target.surface, message.path || "")
      .then((html) => {
        port.postMessage({ type: "orqella:module-document", html });
        port.close();
      })
      .catch((error) => {
        try {
          port.postMessage({ type: "orqella:module-error", error: String(error?.message || error) });
        } catch (_) {}
        port.close();
      });
  }

  function handleModuleBroker(target, message, port) {
    if (!port) return;
    const module = moduleById(target?.id || "");
    if (!module
      || !message
      || message.module !== target.id
      || message.nonce !== (target.nonce || "")
      || !moduleOriginFor(module)) {
      try { port.close(); } catch (_) {}
      return;
    }
    closeModuleBroker(target.surface, target.id);
    const entry = {
      module,
      surface: target.surface,
      frame: target.frame,
      nonce: target.nonce,
      port,
      sockets: new Map(),
    };
    state.moduleBrokers.set(frameKey(target.surface, target.id), entry);
    port.onmessage = (event) => { handleBrokerMessage(entry, event.data); };
    port.start?.();
  }

  function base64FromBytes(view) {
    let text = "";
    for (let index = 0; index < view.length; index += 0x8000) {
      text += String.fromCharCode(...view.subarray(index, index + 0x8000));
    }
    return btoa(text);
  }

  function bytesFromBase64(value) {
    const text = atob(value || "");
    const out = new Uint8Array(text.length);
    for (let index = 0; index < text.length; index += 1) out[index] = text.charCodeAt(index);
    return out;
  }

  function brokerHeaders(headers) {
    const safe = new Set([
      "accept",
      "accept-language",
      "content-type",
      "if-modified-since",
      "if-none-match",
      "range",
    ]);
    const out = {};
    for (const [name, value] of Object.entries(headers || {})) {
      if (safe.has(name.toLowerCase())) out[name] = String(value);
    }
    return out;
  }

  function brokerResponseHeaders(headers) {
    const out = {};
    for (const [name, value] of headers.entries()) {
      const lower = name.toLowerCase();
      if ([
        "connection",
        "content-encoding",
        "content-length",
        "content-security-policy",
        "keep-alive",
        "location",
        "set-cookie",
        "transfer-encoding",
      ].includes(lower)) continue;
      out[name] = value;
    }
    return out;
  }

  async function handleBrokerHttp(entry, message) {
    const id = message.id;
    const target = ModuleOrigins
      ? ModuleOrigins.brokerTargetPath(entry.module, message.path || "")
      : "";
    if (!target) {
      entry.port.postMessage({ type: "http.response", id, status: 403, headers: {}, body: null });
      return;
    }
    const method = String(message.method || "GET").toUpperCase();
    if (!["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"].includes(method)) {
      entry.port.postMessage({ type: "http.response", id, status: 405, headers: {}, body: null });
      return;
    }
    let body;
    if (!["GET", "HEAD"].includes(method) && message.body) {
      const view = message.body instanceof ArrayBuffer
        ? new Uint8Array(message.body)
        : new Uint8Array(message.body);
      if (view.byteLength > 32 * 1024 * 1024) {
        entry.port.postMessage({ type: "http.response", id, status: 413, headers: {}, body: null });
        return;
      }
      body = view;
    }
    try {
      const response = await fetch(target, {
        method,
        headers: brokerHeaders(message.headers),
        body,
        credentials: "same-origin",
        cache: "no-store",
        // Never let a module route redirect the shell's authenticated fetch
        // into /api, another module, or an app surface.
        redirect: "error",
      });
      const declared = Number(response.headers.get("content-length") || 0);
      if (declared > 32 * 1024 * 1024) {
        entry.port.postMessage({ type: "http.response", id, status: 413, headers: {}, body: null });
        return;
      }
      const buffer = method === "HEAD" ? null : await response.arrayBuffer();
      if (buffer && buffer.byteLength > 32 * 1024 * 1024) {
        entry.port.postMessage({ type: "http.response", id, status: 413, headers: {}, body: null });
        return;
      }
      if (entry.closed) return;
      entry.port.postMessage({
        type: "http.response",
        id,
        status: response.status,
        headers: brokerResponseHeaders(response.headers),
        body: buffer,
      });
    } catch (error) {
      if (entry.closed) return;
      entry.port.postMessage({
        type: "http.response",
        id,
        status: 502,
        headers: {},
        body: new TextEncoder().encode(String(error?.message || error)).buffer,
      });
    }
  }

  function openBrokerSocket(entry, message) {
    const id = String(message.id || "");
    const target = ModuleOrigins
      ? ModuleOrigins.brokerWsTargetPath(entry.module, message.path || "")
      : "";
    if (!id || !target) {
      entry.port.postMessage({ type: "ws.event", id, event: "error" });
      return;
    }
    let url;
    try {
      url = new URL(target, location.href);
      url.protocol = location.protocol === "https:" ? "wss:" : "ws:";
      const token = entry.module.ws_token || "";
      if (token) url.searchParams.set("__orqella_ws", token);
    } catch (_) {
      entry.port.postMessage({ type: "ws.event", id, event: "error" });
      return;
    }
    const existing = entry.sockets.get(id);
    if (existing) {
      entry.sockets.delete(id);
      try { existing.close(1000, "replaced"); } catch (_) {}
    }
    if (entry.sockets.size >= 32) {
      entry.port.postMessage({ type: "ws.event", id, event: "error" });
      return;
    }
    let socket;
    try {
      const protocols = Array.isArray(message.protocols) && message.protocols.length
        ? message.protocols
        : undefined;
      socket = protocols ? new WebSocket(url.toString(), protocols) : new WebSocket(url.toString());
    } catch (_) {
      entry.port.postMessage({ type: "ws.event", id, event: "error" });
      return;
    }
    socket.binaryType = "arraybuffer";
    entry.sockets.set(id, socket);
    socket.onopen = () => {
      if (entry.closed) return;
      entry.port.postMessage({ type: "ws.event", id, event: "open" });
    };
    socket.onmessage = (event) => {
      if (entry.closed) return;
      if (typeof event.data === "string") {
        entry.port.postMessage({ type: "ws.event", id, event: "message", kind: "text", data: event.data });
      } else if (event.data instanceof ArrayBuffer) {
        entry.port.postMessage({
          type: "ws.event",
          id,
          event: "message",
          kind: "binary",
          data: base64FromBytes(new Uint8Array(event.data)),
        });
      }
    };
    socket.onerror = () => {
      if (entry.closed) return;
      entry.port.postMessage({ type: "ws.event", id, event: "error" });
    };
    socket.onclose = (event) => {
      if (entry.sockets.get(id) !== socket) return;
      entry.sockets.delete(id);
      if (entry.closed) return;
      entry.port.postMessage({
        type: "ws.event",
        id,
        event: "close",
        code: event.code || 1000,
        reason: event.reason || "",
        clean: event.wasClean !== false,
      });
    };
  }

  function handleBrokerMessage(entry, message) {
    if (!entry || entry.closed || !message || typeof message !== "object") return;
    if (message.type === "http.request") {
      handleBrokerHttp(entry, message).catch((error) => {
        if (entry.closed) return;
        try {
          entry.port.postMessage({
            type: "http.response",
            id: message.id,
            status: 502,
            headers: {},
            body: new TextEncoder().encode(String(error?.message || error)).buffer,
          });
        } catch (_) {}
      });
      return;
    }
    if (message.type === "ws.open") {
      openBrokerSocket(entry, message);
      return;
    }
    if (message.type === "ws.send") {
      const socket = entry.sockets.get(String(message.id || ""));
      if (!socket || socket.readyState !== 1) return;
      try {
        if (message.kind === "text") socket.send(String(message.data || ""));
        else socket.send(bytesFromBase64(message.data));
      } catch (_) {}
      return;
    }
    if (message.type === "ws.close") {
      const id = String(message.id || "");
      const socket = entry.sockets.get(id);
      if (!socket) return;
      const requested = Number(message.code || 1000);
      const code = (requested >= 1000
        && requested <= 1015
        && ![1004, 1005, 1006].includes(requested))
        || (requested >= 3000 && requested <= 4999)
        ? requested
        : 1000;
      const reason = String(message.reason || "").slice(0, 120);
      try { socket.close(code, reason); } catch (_) { entry.sockets.delete(id); }
    }
  }

  async function rpc(module, method, params) {
    const installed = state.modules.find((entry) => entry.id === module);
    const remote = installed?.remote_node;
    const endpoint = remote
      ? `/api/v1/nodes/${encodeURIComponent(remote)}/modules/${encodeURIComponent(module)}/rpc`
      : `/api/v1/modules/${encodeURIComponent(module)}/rpc`;
    return api(endpoint, {
      method: "POST",
      body: JSON.stringify({ method, params }),
    });
  }

  function waitForAppFrame() {
    return new Promise((resolve) => {
      const frame = $("appFrame");
      let done = false;
      const finish = () => { if (done) return; done = true; frame.removeEventListener("load", finish); setTimeout(resolve, 120); };
      frame.addEventListener("load", finish);
      setTimeout(finish, 7000);
    });
  }

  function appPayloadForSelectedNode(payload) {
    if (!payload || !payload.mount) return payload;
    let result = payload;
    if (state.node !== "local" && payload.node === "local") {
      const origin = new URL(state.info?.node?.origin || location.origin);
      const host = `${payload.mount}.${state.node}.${origin.hostname}`;
      const authority = `${host}${origin.port ? `:${origin.port}` : ""}`;
      result = { ...payload, node: state.node, host, url: `${origin.protocol}//${authority}/` };
    }
    // Some app routes encode their origin in the path (OpenCode uses
    // /server/<base64url(origin)>/session/<id>). Keep the key aligned with the
    // app URL the shell is about to load, including peer-host rewrites.
    const appUrl = typeof result.url === "string" && /^https?:\/\//.test(result.url) ? result.url : "";
    if (appUrl) {
      let path = result.path || "";
      const encoded = path.match(/^\/server\/[^/]+\/session\/(.+)$/);
      if (encoded) {
        const key = btoa(appUrl.replace(/\/$/, "")).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
        result = { ...result, path: `/server/${key}/session/${encoded[1]}` };
      }
    }
    return result;
  }

  async function openApp(payload) {
    // Native clients can place full apps on a transport-owned origin. The
    // resolver is optional, so browsers keep the canonical host behavior.
    let resolvedBase = "";
    if (typeof window.__ORQELLA_APP_ORIGIN_RESOLVER === "function") {
      try {
        const value = await window.__ORQELLA_APP_ORIGIN_RESOLVER(payload);
        if (value) resolvedBase = new URL("/", value).toString();
      } catch (error) {
        console.warn("Orqella app origin resolver failed", error);
      }
    }
    const nativeClient = Boolean(
      window.__ORQELLA_NATIVE_TOKEN
      || document.getElementById("__orqella_native_marker"),
    );
    if (nativeClient) {
      // A native shell must never fall back to the shell origin or to a hub
      // URL: both share the shell cookie jar and defeat app isolation.
      if (!resolvedBase) {
        throw new Error("Orqella could not resolve an isolated app origin for this app.");
      }
      if (new URL(resolvedBase).origin === location.origin) {
        throw new Error("Orqella refused to open an app on the shell origin.");
      }
    }
    const baseUrl = resolvedBase || (payload.host
      ? `${location.protocol}//${payload.host}:${location.port}/`
      : new URL(payload.url, location.origin).toString());
    let appUrl = baseUrl;
    if (typeof payload.path === "string" && payload.path.startsWith("/") && !payload.path.startsWith("//")) {
      try { appUrl = new URL(payload.path, baseUrl).toString(); } catch {}
    }
    // Same-origin app frames still use the browser transport service worker.
    // Keep their requests bound to this tab; dedicated app hosts do not pass
    // through the worker and need no transport id.
    try {
      const transport = new URLSearchParams(location.search).get("__orqella_transport");
      const target = new URL(appUrl);
      if (transport && target.origin === location.origin) {
        target.searchParams.set("__orqella_transport", transport);
        appUrl = target.toString();
      }
    } catch (_) {}
    payload = { ...payload, url: appUrl };
    // Mobile shells can present full apps in a top-level native WebView.
    // Keeping the app out of an iframe preserves first-party cookies and
    // avoids frame-ancestors restrictions.
    if (typeof window.__ORQELLA_NATIVE_APP_OPEN === "function") {
      try {
        let handoffUrl = "";
        if (
          typeof payload.handoff === "string"
          && payload.handoff.startsWith("/")
          && !payload.handoff.startsWith("//")
        ) {
          try { handoffUrl = new URL(payload.handoff, baseUrl).toString(); } catch (_) {}
        }
        const handled = await window.__ORQELLA_NATIVE_APP_OPEN({
          url: appUrl,
          handoff: handoffUrl,
          title: payload.title || payload.mount || "Application",
        });
        if (handled !== false) return;
      } catch (error) {
        console.warn("Orqella native app opener failed", error);
      }
    }
    state.app = payload;
    // Opening an app hides the module page, but its document stays loaded and
    // its SDK only runs `ready` callbacks once. Invalidate the page so the
    // next visit loads a fresh document and repeats its launch flow.
    const moduleFrame = $("moduleFrame");
    if (moduleFrame) moduleFrame.dataset.module = "";
    if (state.module) closeModuleBroker("page", state.module);
    const appLabel = payload.title || payload.mount || "Application";
    const frame = $("appFrame");
    // Delegate media only to the app's resolved origin. A frame that
    // navigates somewhere else does not inherit these capabilities.
    try {
      const appOrigin = new URL(appUrl).origin;
      const features = ["fullscreen"];
      if (appOrigin !== location.origin) {
        for (const feature of [
          "microphone",
          "camera",
          "display-capture",
          "clipboard-read",
          "clipboard-write",
        ]) {
          features.push(`${feature} ${appOrigin}`);
        }
      }
      frame.setAttribute("allow", features.join("; "));
    } catch (_) {
      frame.setAttribute("allow", "fullscreen");
    }
    frame.dataset.appTitle = appLabel;
    $("appLoadingText").textContent = `Opening ${appLabel}…`;
    $("appLoading").hidden = false;
    frame.classList.add("app-frame-loading");
    $("appSurface").hidden = false;
    $("coreView").hidden = true;
    $("moduleHost").hidden = true;
    const loadFrame = async (url) => {
      const loaded = waitForAppFrame();
      frame.src = url;
      await loaded;
    };
    const loadHandoff = (url) => new Promise((resolve) => {
      const hidden = document.createElement("iframe");
      let done = false;
      const finish = () => {
        if (done) return;
        done = true;
        setTimeout(() => { hidden.remove(); resolve(); }, 120);
      };
      hidden.tabIndex = -1;
      hidden.setAttribute("aria-hidden", "true");
      hidden.style.cssText = "position:absolute;width:1px;height:1px;opacity:0;pointer-events:none;border:0";
      hidden.addEventListener("load", finish, { once: true });
      hidden.src = url;
      $("appSurface").append(hidden);
      setTimeout(finish, 5000);
    });
    try {
      if (typeof payload.handoff === "string" && payload.handoff.startsWith("/") && !payload.handoff.startsWith("//")) {
        try { await loadHandoff(new URL(payload.handoff, baseUrl).toString()); } catch {}
      }
      await loadFrame(appUrl);
    } finally {
      $("appLoading").hidden = true;
      frame.classList.remove("app-frame-loading");
    }
  }

  function recorderMime() {
    if (!window.MediaRecorder) return "";
    const types = ["audio/webm;codecs=opus", "audio/webm", "audio/mp4", "audio/ogg;codecs=opus", "audio/ogg"];
    return types.find((type) => MediaRecorder.isTypeSupported(type)) || "";
  }

  function blobBase64(blob) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result || "").split(",", 2)[1] || "");
      reader.onerror = () => reject(reader.error || new Error("could not read audio"));
      reader.readAsDataURL(blob);
    });
  }

  async function startModuleRecording(moduleId) {
    const module = state.modules.find((entry) => entry.id === moduleId);
    if (!module) throw new Error("module not found");
    if (!module.granted_permissions?.includes("media.record")) {
      throw new Error("microphone permission has not been granted");
    }
    if (state.recordings.has(moduleId)) throw new Error("microphone is already recording");
    if (!navigator.mediaDevices?.getUserMedia || !window.MediaRecorder) {
      throw new Error("microphone recording is not available in this browser");
    }
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    const type = recorderMime();
    const recorder = type ? new MediaRecorder(stream, { mimeType: type }) : new MediaRecorder(stream);
    const entry = { stream, recorder, chunks: [], mime: recorder.mimeType || type || "audio/webm" };
    recorder.ondataavailable = (event) => { if (event.data?.size) entry.chunks.push(event.data); };
    recorder.start();
    state.recordings.set(moduleId, entry);
    return { ok: true };
  }

  function stopAllRecordings() {
    for (const entry of state.recordings.values()) {
      try { entry.recorder.stop(); } catch {}
      entry.stream.getTracks().forEach((track) => track.stop());
    }
    state.recordings.clear();
  }

  function stopModuleRecording(moduleId) {
    const entry = state.recordings.get(moduleId);
    if (!entry) throw new Error("module is not recording");
    return new Promise((resolve, reject) => {
      const done = async () => {
        try {
          const blob = new Blob(entry.chunks, { type: entry.mime });
          entry.stream.getTracks().forEach((track) => track.stop());
          state.recordings.delete(moduleId);
          resolve({ audio: await blobBase64(blob), mime: entry.mime, size: blob.size });
        } catch (error) {
          reject(error);
        }
      };
      entry.recorder.addEventListener("stop", done, { once: true });
      try { entry.recorder.stop(); } catch (error) {
        entry.stream.getTracks().forEach((track) => track.stop());
        state.recordings.delete(moduleId);
        reject(error);
      }
    });
  }

  function frameTarget(source) {
    if (source === $("moduleFrame").contentWindow && state.module) {
      return {
        id: state.module,
        surface: "page",
        nonce: $("moduleFrame").dataset.frameNonce || "",
        frame: $("moduleFrame"),
      };
    }
    for (const [id, frame] of state.overviewFrames) {
      if (frame.contentWindow === source) {
        return { id, surface: "overview", nonce: frame.dataset.frameNonce || "", frame };
      }
    }
    for (const [id, frame] of state.settingsFrames) {
      if (frame.contentWindow === source) {
        return { id, surface: "settings", nonce: frame.dataset.frameNonce || "", frame };
      }
    }
    return null;
  }

  global.OrqellaFrames = {
    install,
    frameKey,
    newFrameNonce,
    frameHash,
    setFrameNonce,
    frameNonceMatches,
    moduleOriginFor,
    moduleFrameSrc,
    moduleFrameSandbox,
    moduleUsable,
    cleanModuleInnerPath,
    moduleMessageAllowed,
    configureModuleFrame,
    handleModuleBootstrap,
    handleModuleBroker,
    closeModuleBroker,
    moduleContext,
    sendContextTo,
    sendContext,
    sendOverviewContext,
    sendSettingsContext,
    sendThemeContext,
    rpc,
    waitForAppFrame,
    appPayloadForSelectedNode,
    openApp,
    recorderMime,
    blobBase64,
    startModuleRecording,
    stopAllRecordings,
    stopModuleRecording,
    frameTarget,
  };
})(typeof window !== "undefined" ? window : globalThis);
