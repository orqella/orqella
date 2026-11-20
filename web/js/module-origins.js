/* Per-module browser origins and broker path rules. */
(function (global) {
  "use strict";

  const SHARED_ASSETS = new Set([
    "/assets/theme.css",
    "/assets/module.css",
    "/assets/logo.png",
  ]);
  const SHARED_PREFIXES = ["/assets/themes/"];

  function configuredSuffix() {
    const direct = global.__ORQELLA_MODULE_ORIGIN_SUFFIX;
    if (typeof direct === "string" && direct.trim()) return direct.trim();
    try {
      const top = global.top;
      if (top && top !== global) {
        const value = top.__ORQELLA_MODULE_ORIGIN_SUFFIX;
        if (typeof value === "string" && value.trim()) return value.trim();
      }
    } catch (_) {}
    return "";
  }

  function suffix() {
    let value = configuredSuffix();
    if (!value) {
      try {
        const params = new URLSearchParams(global.location.search);
        const host = global.location.hostname;
        if (params.get("moduleOrigins") === "1" && host) {
          if (host === "localhost" || host === "127.0.0.1") value = ".localhost";
          else if (!/^\d+\.\d+\.\d+\.\d+$/.test(host) && !host.includes(":")) {
            value = `.modules.${host}`;
          }
        }
      } catch (_) {}
    }
    if (!value) return "";
    return value.startsWith(".") ? value : `.${value}`;
  }

  function enabled() {
    return Boolean(suffix());
  }

  function required() {
    if (typeof global.__ORQELLA_REQUIRE_MODULE_ORIGINS === "boolean") {
      return global.__ORQELLA_REQUIRE_MODULE_ORIGINS;
    }
    try {
      const top = global.top;
      if (top && top !== global && typeof top.__ORQELLA_REQUIRE_MODULE_ORIGINS === "boolean") {
        return top.__ORQELLA_REQUIRE_MODULE_ORIGINS;
      }
    } catch (_) {}
    return false;
  }

  function usable(module) {
    return Boolean(originFor(module)) || !required();
  }


  function hashInstance(value) {
    let first = 0x811c9dc5;
    let second = 0x9e3779b9;
    for (let index = 0; index < value.length; index += 1) {
      const code = value.charCodeAt(index);
      first = Math.imul(first ^ code, 0x01000193) >>> 0;
      second = Math.imul(second ^ code, 0x85ebca6b) >>> 0;
    }
    return `${first.toString(16).padStart(8, "0")}${second.toString(16).padStart(8, "0")}`.slice(0, 16);
  }

  let instanceCache = "";

  function instanceToken() {
    if (instanceCache) return instanceCache;
    const override = global.__ORQELLA_MODULE_INSTANCE;
    if (override) {
      instanceCache = String(override).toLowerCase().replace(/[^a-z0-9]/g, "").slice(0, 12);
      return instanceCache;
    }
    // The browser client injects a unique transport id into the shell URL;
    // hash it so two tabs can never share a module origin. Local shells fall
    // back to a random per-tab value.
    let value = "";
    try {
      value = new URLSearchParams(global.location.search).get("__orqella_transport") || "";
    } catch (_) {}
    if (!value) {
      try { value = sessionStorage.getItem("orqella.module.instance") || ""; } catch (_) {}
    }
    if (!value) {
      try {
        const bytes = new Uint8Array(8);
        global.crypto.getRandomValues(bytes);
        value = Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
      } catch (_) {
        value = Math.random().toString(16).slice(2, 18);
      }
      try { sessionStorage.setItem("orqella.module.instance", value); } catch (_) {}
    }
    instanceCache = hashInstance(value);
    return instanceCache;
  }

  function originFor(module) {
    if (!module || !enabled()) return "";
    let configured = null;
    try {
      configured = global.__ORQELLA_MODULE_ORIGINS?.[module.id]
        || global.top?.__ORQELLA_MODULE_ORIGINS?.[module.id]
        || null;
    } catch (_) {}
    if (configured) {
      try {
        const origin = new URL(configured, global.location.href).origin;
        return origin === global.location.origin ? "" : origin;
      } catch (_) {
        return "";
      }
    }
    const label = module.browser_origin_id;
    if (!label || !/^m-[a-f0-9]{16}$/.test(label)) return "";
    const instance = instanceToken();
    if (!instance) return "";
    const node = hashInstance(String(module.remote_node || "local")).slice(0, 8);
    const port = global.location.port ? `:${global.location.port}` : "";
    return `${global.location.protocol}//${label}-${instance}-${node}${suffix()}${port}`;
  }

  function frameSrc(module, surface, path, nonce) {
    const origin = originFor(module);
    if (!origin) return "";
    const url = new URL("/module-bootstrap.html", origin);
    url.searchParams.set("module", module.id);
    url.searchParams.set("surface", surface || "page");
    url.searchParams.set("nonce", nonce || "");
    if (module.remote_node) url.searchParams.set("node", module.remote_node);
    if ((surface || "page") === "page" && path) {
      url.searchParams.set("path", path.startsWith("/") ? path : `/${path}`);
    }
    // The hash is a fallback for browsers that reject history.replaceState on
    // the module document. The SDK reads the same nonce from location.hash.
    url.hash = `#__orqella_frame=${encodeURIComponent(nonce || "")}`;
    return url.toString();
  }

  function sandbox(isolated) {
    return isolated
      ? "allow-scripts allow-same-origin allow-forms allow-popups"
      : "allow-scripts allow-forms allow-popups";
  }

  function cleanPath(value) {
    if (typeof value !== "string" || !value.startsWith("/")) return "";
    if (value.startsWith("//")) return "";
    if (value.includes("\\") || value.includes("\0")) return "";
    if ([...value].some((character) => character.charCodeAt(0) < 0x20)) return "";
    const raw = value.split("?", 1)[0];
    if (raw.split("/").some((part) => part === ".." || part === ".")) return "";
    // URL parsers normalize encoded dot segments, and the daemon may decode
    // the path again. Reject encoded traversal before it reaches fetch().
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

  function brokerPathAllowed(module, requestPath) {
    if (!module) return false;
    const clean = cleanPath(requestPath);
    if (!clean) return false;
    let url;
    try { url = new URL(clean, "https://module.invalid"); } catch (_) { return false; }
    const path = url.pathname;
    if (SHARED_ASSETS.has(path)) return true;
    if (SHARED_PREFIXES.some((prefix) => path.startsWith(prefix))) return true;
    const id = encodeURIComponent(module.id);
    const local = `/m/${id}`;
    if (path === local || path.startsWith(`${local}/`)) return true;
    if (module.remote_node) {
      const remote = `/n/${encodeURIComponent(module.remote_node)}/m/${id}`;
      if (path === remote || path.startsWith(`${remote}/`)) return true;
    }
    return false;
  }

  function brokerTargetPath(module, requestPath) {
    if (!module || !brokerPathAllowed(module, requestPath)) return "";
    let url;
    try { url = new URL(requestPath, "https://module.invalid"); } catch (_) { return ""; }
    const id = encodeURIComponent(module.id);
    const local = `/m/${id}`;
    const path = url.pathname;
    if (module.remote_node) {
      const remote = `/n/${encodeURIComponent(module.remote_node)}/m/${id}`;
      if (path === remote || path.startsWith(`${remote}/`)) {
        return `${path}${url.search}`;
      }
      if (path === local || path.startsWith(`${local}/`)) {
        return `/n/${encodeURIComponent(module.remote_node)}${path}${url.search}`;
      }
      return "";
    }
    return `${path}${url.search}`;
  }

  function brokerWsTargetPath(module, requestPath) {
    const target = brokerTargetPath(module, requestPath);
    if (!target) return "";
    let url;
    try { url = new URL(target, "https://module.invalid"); } catch (_) { return ""; }
    const id = encodeURIComponent(module.id);
    const local = `/m/${id}`;
    const remote = module.remote_node
      ? `/n/${encodeURIComponent(module.remote_node)}/m/${id}`
      : "";
    const prefix = remote && url.pathname.startsWith(remote)
      ? remote
      : url.pathname.startsWith(local) ? local : "";
    if (!prefix) return "";
    const rest = url.pathname.slice(prefix.length);
    if (rest !== "/ws" && !rest.startsWith("/ws/")) return "";
    return target;
  }

  function messageOriginAllowed(module, origin) {
    const expected = originFor(module);
    if (expected) return origin === expected;
    return !required() && origin === "null";
  }

  global.OrqellaModuleOrigins = {
    enabled,
    required,
    usable,
    suffix,
    originFor,
    frameSrc,
    sandbox,
    brokerPathAllowed,
    brokerTargetPath,
    brokerWsTargetPath,
    messageOriginAllowed,
  };
})(typeof window !== "undefined" ? window : globalThis);
