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
        if (params.get("moduleOrigins") === "1"
          && global.location.hostname.endsWith("orqella.localhost")) {
          value = ".modules.orqella.localhost";
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

  function originFor(module) {
    if (!module || !enabled()) return "";
    let configured = null;
    try {
      configured = global.__ORQELLA_MODULE_ORIGINS?.[module.id]
        || global.top?.__ORQELLA_MODULE_ORIGINS?.[module.id]
        || null;
    } catch (_) {}
    if (configured) {
      try { return new URL(configured, global.location.href).origin; } catch (_) { return ""; }
    }
    const label = module.browser_origin_id;
    if (!label || !/^m-[a-f0-9]{16}$/.test(label)) return "";
    const local = /\.localhost$/i.test(suffix());
    const port = local && global.location.port ? `:${global.location.port}` : "";
    return `${global.location.protocol}//${label}${suffix()}${port}`;
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
    const path = target.split("?", 1)[0];
    if (!path.endsWith("/ws") && !path.includes("/ws/")) return "";
    return target;
  }

  function messageOriginAllowed(module, origin) {
    const expected = originFor(module);
    if (expected) return origin === expected;
    return origin === "null";
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
