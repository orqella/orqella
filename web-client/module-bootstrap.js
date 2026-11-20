/* Generic per-module origin bootstrap. It carries no module credentials. */
(() => {
  "use strict";

  const params = new URLSearchParams(location.search);
  const moduleId = params.get("module") || "";
  const surface = params.get("surface") || "page";
  const nonce = params.get("nonce") || "";
  const node = params.get("node") || "";
  const requestedPath = params.get("path") || "";

  const errorBox = document.getElementById("error");
  const errorText = document.getElementById("errorText");
  const loadingBox = document.getElementById("loading");


  const fail = (message) => {
    if (loadingBox) loadingBox.hidden = true;
    if (errorBox) errorBox.hidden = false;
    if (errorText) errorText.textContent = String(message || "Unknown error");
    try {
      parent.postMessage(
        { type: "orqella:module-bootstrap-error", module: moduleId, nonce, error: String(message || "") },
        "*",
      );
    } catch (_) {}
  };

  if (!moduleId || !nonce) {
    fail("The module bootstrap URL is incomplete.");
    return;
  }

  const escapeRegExp = (value) => String(value).replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

  async function registerWorker() {
    if (!("serviceWorker" in navigator)) return false;
    if (navigator.serviceWorker.controller) return true;
    try {
      const registration = await Promise.race([
        navigator.serviceWorker.register("/module-sw.js", {
          scope: "/",
          updateViaCache: "none",
        }),
        new Promise((_, reject) => setTimeout(
          () => reject(new Error("module service worker registration timed out")),
          1500,
        )),
      ]);
      await Promise.race([
        navigator.serviceWorker.ready,
        new Promise((resolve) => setTimeout(resolve, 1500)),
      ]);
      if (!navigator.serviceWorker.controller) {
        await new Promise((resolve) => {
          let settled = false;
          const done = () => {
            if (settled) return;
            settled = true;
            resolve();
          };
          navigator.serviceWorker.addEventListener("controllerchange", done, { once: true });
          setTimeout(done, 1000);
        });
      }
      const controller = navigator.serviceWorker.controller;
      if (controller) {
        controller.postMessage({
          type: "orqella-module-register",
          module: moduleId,
          nonce,
          surface,
          path: requestedPath,
        });
        return true;
      }
      return Boolean(registration.active && navigator.serviceWorker.controller);
    } catch (_) {
      return false;
    }
  }

  function modulePath() {
    const base = `/m/${encodeURIComponent(moduleId)}/`;
    const hash = `#__orqella_frame=${encodeURIComponent(nonce)}`;
    if (surface !== "page") return `${base}${surface}${hash}`;
    if (!requestedPath) return `${base}${hash}`;
    const suffix = requestedPath.startsWith("/") ? requestedPath.slice(1) : requestedPath;
    return `${base}${suffix}${hash}`;
  }

  function rewrite(html) {
    let out = String(html || "");
    if (node) {
      const remote = new RegExp(
        `/n/${escapeRegExp(node)}/m/${escapeRegExp(moduleId)}/`,
        "g",
      );
      out = out.replace(remote, `/m/${encodeURIComponent(moduleId)}/`);
    }
    const base = `<base href="/m/${encodeURIComponent(moduleId)}/">`;
    if (!/<base\s/i.test(out)) {
      if (/<head(\s[^>]*)?>/i.test(out)) {
        out = out.replace(/<head(\s[^>]*)?>/i, (match) => `${match}${base}`);
      } else {
        out = `${base}${out}`;
      }
    }
    // The marker makes the injected SDK skip its same-origin WebSocket token
    // wrapper; the bridge below owns module WebSockets on this origin.
    const surfaceJson = JSON.stringify(surface).replace(/</g, "\\u003c");
    const marker = `<script>window.__ORQELLA_MODULE_ORIGIN=true;window.__ORQELLA_MODULE_SURFACE=${surfaceJson};</script>`;
    const bridge = '<script src="/module-bridge.js"></script>';
    const injected = `${marker}${bridge}`;
    if (/<head(\s[^>]*)?>/i.test(out)) {
      out = out.replace(/<head(\s[^>]*)?>/i, (match) => `${match}${injected}`);
    } else {
      out = `${injected}${out}`;
    }
    return out;
  }

  function install(html) {
    try {
      // The document URL becomes the module path before scripts run, so
      // relative URLs and the SDK's frame nonce have the right base.
      history.replaceState(null, "", modulePath());
    } catch (_) {
      // A replaceState failure is not fatal; the injected base still applies.
    }
    try {
      document.open();
      document.write(rewrite(html));
      document.close();
    } catch (error) {
      fail(error?.message || error);
    }
  }

  async function boot() {
    // Not every browser controls a cross-origin iframe with a service worker
    // (Firefox and some Safari configurations do not). Fall back to a bundled,
    // fully inlined module document instead of leaving a blank frame. Safari
    // and all iOS browsers use WebKit, where iframe service-worker control is
    // not reliable enough to make module pages depend on it.
    const userAgent = navigator.userAgent || "";
    const webkitOnly = /iP(hone|ad|od)/i.test(userAgent);
    const controlled = webkitOnly
      ? false
      : await registerWorker().catch(() => false);
    if (webkitOnly) registerWorker().catch(() => {});
    const channel = new MessageChannel();
    let finished = false;
    const port = channel.port1;
    const timer = setTimeout(() => {
      if (finished) return;
      finished = true;
      fail("The Orqella shell did not return this module in time.");
    }, 30000);

    port.onmessage = (event) => {
      const message = event.data;
      if (!message || typeof message !== "object") return;
      if (message.type === "orqella:module-document") {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        try { port.close(); } catch (_) {}
        install(message.html);
        return;
      }
      if (message.type === "orqella:module-error") {
        if (finished) return;
        finished = true;
        clearTimeout(timer);
        fail(message.error || "The module document was not available.");
      }
    };
    port.start?.();

    parent.postMessage(
      {
        type: "orqella:module-bootstrap",
        module: moduleId,
        surface,
        node,
        nonce,
        path: requestedPath,
        inline: !controlled,
      },
      "*",
      [channel.port2],
    );
  }

  boot();
})();
