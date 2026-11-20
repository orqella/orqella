import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../module-sw.js", import.meta.url), "utf8");

function load() {
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
    Set,
    console,
    setTimeout,
    clearTimeout,
    self: {
      location: { origin: "https://m-abc.modules.example.com" },
      clients: {
        get: async () => null,
        matchAll: async () => [],
        claim: async () => {},
      },
      addEventListener(type, callback) {
        listeners.push({ type, callback });
      },
      skipWaiting() {},
    },
  });
  vm.runInContext(
    `${source}\nglobalThis.__sw = { isModulePath, surfaceFromPath, registrationFor, resetBridge };`,
    context,
  );
  const dispatchMessage = (data, sourceClient) => {
    for (const listener of listeners) {
      if (listener.type === "message") {
        listener.callback({ data, source: sourceClient });
      }
    }
  };
  return { helpers: context.__sw, dispatchMessage };
}

test("module SW path classification matches the broker contract", () => {
  const { helpers } = load();
  for (const path of [
    "/m/voice-assistant/",
    "/m/voice-assistant/ui/app.js",
    "/n/laptop/m/voice-assistant/ui/app.js",
    "/assets/theme.css",
    "/assets/module.css",
    "/assets/themes/theme.css",
  ]) {
    assert.equal(helpers.isModulePath(path), true, path);
  }
  for (const path of ["/module-bootstrap.html", "/module-bootstrap.js", "/module-sw.js", "/module-bridge.js", "/api/v1/info"]) {
    assert.equal(helpers.isModulePath(path), false, path);
  }
});

test("module SW derives the correct surface", () => {
  const { helpers } = load();
  assert.equal(helpers.surfaceFromPath("/m/voice-assistant/", "voice-assistant"), "page");
  assert.equal(helpers.surfaceFromPath("/m/voice-assistant", "voice-assistant"), "page");
  assert.equal(helpers.surfaceFromPath("/m/voice-assistant/overview", "voice-assistant"), "overview");
  assert.equal(helpers.surfaceFromPath("/m/voice-assistant/overview/", "voice-assistant"), "overview");
  assert.equal(helpers.surfaceFromPath("/m/voice-assistant/settings", "voice-assistant"), "settings");
  assert.equal(helpers.surfaceFromPath("/m/voice-assistant/ui/console.html", "voice-assistant"), "page");
});

test("module SW registrations are scoped by surface", () => {
  const { helpers, dispatchMessage } = load();
  dispatchMessage(
    { type: "orqella-module-register", module: "voice-assistant", surface: "page", nonce: "page-nonce" },
    { id: "page-client" },
  );
  dispatchMessage(
    { type: "orqella-module-register", module: "voice-assistant", surface: "settings", nonce: "settings-nonce" },
    { id: "settings-client" },
  );
  assert.equal(helpers.registrationFor("voice-assistant", "page").nonce, "page-nonce");
  assert.equal(helpers.registrationFor("voice-assistant", "settings").nonce, "settings-nonce");
  assert.equal(helpers.registrationFor("voice-assistant", "overview").nonce, "page-nonce");
});
