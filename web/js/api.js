(function (global) {
  "use strict";

  async function api(path, options = {}) {
    const response = await fetch(path, {
      ...options,
      headers: {
        "Content-Type": "application/json",
        "X-Orqella": "1",
        ...(options.headers || {}),
      },
    });
    if (response.status === 401 && typeof location !== "undefined") {
      // A daemon restart invalidates in-memory sessions. Reload the shell so
      // the document navigation can mint a fresh session cookie. Stop after
      // two attempts so a persistent auth failure cannot reload forever.
      let reloads = 0;
      try { reloads = Number(sessionStorage.getItem("orqella.sessionReloads") || 0); } catch {}
      if (reloads < 2) {
        try { sessionStorage.setItem("orqella.sessionReloads", String(reloads + 1)); } catch {}
        location.reload();
        return new Promise(() => {});
      }
    }
    if (response.ok) {
      try { sessionStorage.removeItem("orqella.sessionReloads"); } catch {}
    }
    const text = await response.text();
    let body = null;
    try { body = text ? JSON.parse(text) : null; } catch { body = { text }; }
    if (!response.ok) {
      throw new Error(body?.error || body?.text || `HTTP ${response.status}`);
    }
    return body;
  }

  global.OrqellaApi = { api };
})(typeof window !== "undefined" ? window : globalThis);
