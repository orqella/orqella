import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../sw.js", import.meta.url), "utf8");

function load(clients = {}) {
  const listeners = [];
  const context = vm.createContext({
    URL,
    URLSearchParams,
    Headers,
    Response,
    Request,
    TextDecoder,
    TextEncoder,
    Uint8Array,
    MessageChannel: class {},
    Promise,
    Map,
    console,
    fetch: async () => new Response("<html></html>", { status: 200 }),
    self: {
      location: { origin: "http://127.0.0.1:8801" },
      clients: {
        get: async (id) => clients[id] || null,
        matchAll: async () => Object.values(clients),
        claim: async () => {},
      },
      addEventListener(type, callback) {
        listeners.push({ type, callback });
      },
      skipWaiting() {},
    },
  });
  vm.runInContext(
    `${source}\nglobalThis.__sw = { bypass, shouldProxy, mountFromPath, shellTargetPath, appTargetPath, ownerClient, transportOwners, clientTransports };`,
    context,
  );
  return { helpers: context.__sw, listeners };
}

test("client and proxy path classification is precise", () => {
  const { helpers } = load();
  for (const path of ["/client", "/client/", "/client/app.js", "/pkg/wasm.js", "/sw.js", "/ws-shim.js", "/app-view-shell-shim.js", "/favicon.ico"]) {
    assert.equal(helpers.bypass(path), true, path);
  }
  for (const path of ["/", "/api/v1/info", "/assets/app.js", "/m/system/", "/p/opencode/"]) {
    assert.equal(helpers.bypass(path), false, path);
  }
  for (const path of ["/app", "/app/", "/assets/app.js", "/api/v1/info", "/p/opencode/", "/n/laptop/p/opencode/", "/healthz"]) {
    assert.equal(helpers.shouldProxy(path), true, path);
  }
  for (const path of ["/m/system/", "/n/laptop/m/system/", "/sdk/orqella.js"]) {
    assert.equal(helpers.shouldProxy(path), false, path);
  }
  assert.equal(helpers.shouldProxy("/client/app.js"), false);
});

test("app mount parsing decodes names and ignores unrelated paths", () => {
  const { helpers } = load();
  assert.equal(helpers.mountFromPath("/app-view/homeassistant/lovelace"), "homeassistant");
  assert.equal(helpers.mountFromPath("/app-view/demo%20app/"), "demo app");
  assert.equal(helpers.mountFromPath("/app-view/"), "");
  assert.equal(helpers.mountFromPath("/client/"), "");
  assert.equal(helpers.mountFromPath("/"), "");
});

test("shell paths map the /app prefix back to the hub root", () => {
  const { helpers } = load();
  assert.equal(helpers.shellTargetPath("/app", ""), "/");
  assert.equal(helpers.shellTargetPath("/app/", "?__orqella_transport=abc"), "/?__orqella_transport=abc");
  assert.equal(helpers.shellTargetPath("/app/assets/app.js", ""), "/assets/app.js");
  assert.equal(helpers.shellTargetPath("/assets/app.js", ""), "/assets/app.js");
  assert.equal(helpers.shellTargetPath("/api/v1/info", "?x=1"), "/api/v1/info?x=1");
});

test("app target paths preserve the mount and strip transport query parameters", () => {
  const { helpers } = load();
  const url = new URL(
    "http://127.0.0.1:8801/app-view/demo/settings?tab=network&__orqella_app=demo",
  );
  assert.equal(helpers.appTargetPath(url, "demo"), "/p/demo/settings?tab=network");
  const root = new URL("http://127.0.0.1:8801/app-view/demo?__orqella_app=demo");
  assert.equal(helpers.appTargetPath(root, "demo"), "/p/demo/");
  const encoded = new URL("http://127.0.0.1:8801/app-view/my%20app/");
  assert.equal(helpers.appTargetPath(encoded, "my app"), "/p/my%20app/");
});

test("nested requests resolve only to their registered tab", async () => {
  const { helpers } = load({
    "tab-a": { id: "tab-a", url: "http://127.0.0.1:8801/client/", frameType: "top-level" },
    "tab-b": { id: "tab-b", url: "http://127.0.0.1:8801/client/", frameType: "top-level" },
    "shell-a": {
      id: "shell-a",
      url: "http://127.0.0.1:8801/app/?__orqella_transport=transport-a",
      frameType: "nested",
    },
    "shell-b": {
      id: "shell-b",
      url: "http://127.0.0.1:8801/app/?__orqella_transport=transport-b",
      frameType: "nested",
    },
  });
  helpers.transportOwners.set("transport-a", "tab-a");
  helpers.transportOwners.set("transport-b", "tab-b");
  helpers.clientTransports.set("tab-a", "transport-a");
  helpers.clientTransports.set("tab-b", "transport-b");

  const request = { referrer: "", url: "http://127.0.0.1:8801/api/v1/info" };
  assert.equal((await helpers.ownerClient(request, "tab-a")).id, "tab-a");
  assert.equal((await helpers.ownerClient(request, "shell-a")).id, "tab-a");
  assert.equal((await helpers.ownerClient(request, "shell-b")).id, "tab-b");
  assert.equal((await helpers.ownerClient(request, "unknown")), null);
});

test("fetch handling bypasses the client shell and serves registration pings", async () => {
  const { listeners } = load();
  const fetchListener = listeners.find((listener) => listener.type === "fetch").callback;
  const responses = [];
  await new Promise((resolve) => {
    fetchListener({
      request: { mode: "navigate", url: "http://127.0.0.1:8801/client/" },
      respondWith(value) {
        responses.push(value);
        resolve();
      },
    });
  });
  assert.equal(responses.length, 1);
  const documentResponse = await responses[0];
  assert.equal(documentResponse.status, 200);
  assert.equal(await documentResponse.text(), "<html></html>");

  let handled;
  fetchListener({
    request: { mode: "cors", url: "http://127.0.0.1:8801/assets/app.js" },
    clientId: "client-1",
    respondWith(value) {
      handled = value;
    },
  });
  assert.ok(handled);
  fetchListener({
    request: { mode: "cors", url: "http://127.0.0.1:8801/__orqella_app_register/demo" },
    clientId: "client-1",
    respondWith(value) {
      handled = value;
    },
  });
  const registration = await handled;
  assert.equal(registration.status, 204);
});
