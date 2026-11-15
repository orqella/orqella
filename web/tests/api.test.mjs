import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../js/api.js", import.meta.url), "utf8");

function load(fetchImpl, extras = {}) {
  const window = { ...extras };
  vm.runInNewContext(source, { window, fetch: fetchImpl, JSON, console, ...extras });
  return window.OrqellaApi.api;
}

test("api adds the Orqella headers and parses JSON", async () => {
  let seen;
  const api = load(async (path, options) => {
    seen = { path, options };
    return {
      ok: true,
      status: 200,
      text: async () => JSON.stringify({ ok: true }),
    };
  });
  assert.deepEqual(await api("/api/v1/info", { method: "POST" }), { ok: true });
  assert.equal(seen.path, "/api/v1/info");
  assert.equal(seen.options.headers["X-Orqella"], "1");
  assert.equal(seen.options.headers["Content-Type"], "application/json");
});

test("api surfaces the daemon error body", async () => {
  const api = load(async () => ({
    ok: false,
    status: 403,
    text: async () => JSON.stringify({ error: "not allowed" }),
  }));
  await assert.rejects(() => api("/api/v1/private"), /not allowed/);
});

test("api falls back to plain text and status errors", async () => {
  const api = load(async () => ({
    ok: false,
    status: 500,
    text: async () => "boom",
  }));
  await assert.rejects(() => api("/api/v1/boom"), /boom/);
});

test("api reloads the shell once when the session has expired", async () => {
  let reloads = 0;
  const api = load(
    async () => ({ ok: false, status: 401, text: async () => "session required" }),
    {
      __orqellaSessionReady: true,
      location: { reload: () => { reloads += 1; } },
      sessionStorage: {
        getItem: () => null,
        setItem: () => {},
        removeItem: () => {},
      },
    },
  );
  void api("/api/v1/modules");
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(reloads, 1);
});
