import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../js/module-origins.js", import.meta.url), "utf8");

function load({ suffix = "", port = "", protocol = "https:", explicit = null, required = false, search = "", instance = "tab0" } = {}) {
  const location = {
    protocol,
    hostname: "orqella.localhost",
    port,
    href: `${protocol}//orqella.localhost${port ? `:${port}` : ""}/`,
    origin: `${protocol}//orqella.localhost${port ? `:${port}` : ""}`,
    search,
  };
  const window = {
    location,
    __ORQELLA_MODULE_ORIGIN_SUFFIX: suffix,
    __ORQELLA_MODULE_ORIGINS: explicit,
    __ORQELLA_REQUIRE_MODULE_ORIGINS: required,
    __ORQELLA_MODULE_INSTANCE: instance,
  };
  window.top = window;
  const context = vm.createContext({
    window,
    location,
    URL,
    URLSearchParams,
    globalThis: window,
  });
  vm.runInContext(source, context);
  return window.OrqellaModuleOrigins;
}

const local = { id: "voice-assistant", browser_origin_id: "m-0123456789abcdef" };
const remote = { ...local, remote_node: "laptop" };

test("module origins stay disabled without a configured suffix", () => {
  const origins = load();
  assert.equal(origins.enabled(), false);
  assert.equal(origins.originFor(local), "");
  assert.equal(origins.frameSrc(local, "page", "/", "nonce"), "");
});

test("required origins reject legacy opaque modules", () => {
  const disabled = load({ required: true });
  assert.equal(disabled.required(), true);
  assert.equal(disabled.usable(local), false);
  const enabled = load({ required: true, suffix: ".modules.example.com" });
  assert.equal(enabled.usable(local), true);
});

test("module origins use the stable browser id and configured suffix", () => {
  const origins = load({ suffix: ".modules.example.com" });
  assert.match(origins.originFor(local), /^https:\/\/m-0123456789abcdef-tab0-[a-f0-9]{8}\.modules\.example\.com$/);
  const url = new URL(origins.frameSrc(local, "page", "/settings?tab=voice", "abc"));
  assert.equal(url.origin, origins.originFor(local));
  assert.equal(url.pathname, "/module-bootstrap.html");
  assert.equal(url.searchParams.get("module"), "voice-assistant");
  assert.equal(url.searchParams.get("surface"), "page");
  assert.equal(url.searchParams.get("nonce"), "abc");
  assert.equal(url.searchParams.get("path"), "/settings?tab=voice");
  assert.equal(url.hash, "#__orqella_frame=abc");
});

test("local module origins keep the shell port", () => {
  const origins = load({ suffix: ".localhost", port: "8801", protocol: "http:" });
  assert.match(origins.originFor(local), /^http:\/\/m-0123456789abcdef-tab0-[a-f0-9]{8}\.localhost:8801$/);
});

test("explicit origins override the suffix", () => {
  const origins = load({
    suffix: ".modules.example.com",
    explicit: { "voice-assistant": "https://voice.example.test/path" },
  });
  assert.equal(origins.originFor(local), "https://voice.example.test");
});

test("isolated module frames may keep their own origin", () => {
  const origins = load({ suffix: ".modules.example.com" });
  assert.match(origins.sandbox(true), /allow-same-origin/);
  assert.doesNotMatch(origins.sandbox(false), /allow-same-origin/);
});

test("broker paths allow only the module's own data and shared assets", () => {
  const origins = load({ suffix: ".modules.example.com" });
  for (const path of [
    "/m/voice-assistant/",
    "/m/voice-assistant/ui/app.js?v=1",
    "/m/voice-assistant/ws/session",
    "/assets/theme.css",
    "/assets/module.css",
    "/assets/themes/nocturne.css",
  ]) {
    assert.equal(origins.brokerPathAllowed(local, path), true, path);
  }
  for (const path of [
    "/api/v1/info",
    "/v1/web",
    "/p/opencode",
    "/m/terminal/",
    "/m/voice-assistant/../terminal/",
    "/m/voice-assistant/%2e%2e/terminal/",
    "/m/voice-assistant/%252e%252e/terminal/",
    "//evil.example/m/voice-assistant/",
    "/assets/../../api",
    "/module-bootstrap.html",
  ]) {
    assert.equal(origins.brokerPathAllowed(local, path), false, path);
  }
});

test("remote module paths are rewritten to the selected node", () => {
  const origins = load({ suffix: ".modules.example.com" });
  assert.equal(
    origins.brokerTargetPath(remote, "/m/voice-assistant/ui/app.js?v=1"),
    "/n/laptop/m/voice-assistant/ui/app.js?v=1",
  );
  assert.equal(
    origins.brokerTargetPath(remote, "/n/laptop/m/voice-assistant/ui/app.js"),
    "/n/laptop/m/voice-assistant/ui/app.js",
  );
  assert.equal(
    origins.brokerTargetPath(remote, "/n/other/m/voice-assistant/ui/app.js"),
    "",
  );
  assert.equal(origins.brokerTargetPath(local, "/n/laptop/m/voice-assistant/ui/app.js"), "");
});

test("only module WebSocket paths are brokered as sockets", () => {
  const origins = load({ suffix: ".modules.example.com" });
  assert.equal(
    origins.brokerWsTargetPath(local, "/m/voice-assistant/ws/session?since=1"),
    "/m/voice-assistant/ws/session?since=1",
  );
  assert.equal(
    origins.brokerWsTargetPath(local, "/m/voice-assistant/ws"),
    "/m/voice-assistant/ws",
  );
  assert.equal(origins.brokerWsTargetPath(local, "/m/voice-assistant/ui/ws/app.js"), "");
  assert.equal(origins.brokerWsTargetPath(local, "/m/voice-assistant/ui/app.js"), "");
  assert.equal(origins.brokerWsTargetPath(local, "/api/v1/web"), "");
});

test("frame messages are accepted only from the assigned module origin", () => {
  const origins = load({ suffix: ".modules.example.com" });
  assert.equal(origins.messageOriginAllowed(local, origins.originFor(local)), true);
  assert.equal(origins.messageOriginAllowed(local, "https://m-fedcba9876543210-tab0-00000000.modules.example.com"), false);
  assert.equal(origins.messageOriginAllowed(local, "null"), false);
  assert.equal(origins.messageOriginAllowed({ id: "legacy" }, "null"), true);
  const required = load({ required: true });
  assert.equal(required.messageOriginAllowed({ id: "legacy" }, "null"), false);
});

test("different browser tabs get different module origins", () => {
  const first = load({ suffix: ".modules.example.com", search: "?__orqella_transport=tab-one", instance: null });
  const second = load({ suffix: ".modules.example.com", search: "?__orqella_transport=tab-two", instance: null });
  assert.notEqual(first.originFor(local), second.originFor(local));
  assert.equal(first.originFor(local), first.originFor({ ...local, remote_node: "" }));
});

test("different nodes get different module origins", () => {
  const origins = load({ suffix: ".modules.example.com" });
  assert.notEqual(origins.originFor(local), origins.originFor(remote));
});

test("an explicit origin cannot reuse the shell origin", () => {
  const origins = load({
    suffix: ".modules.example.com",
    explicit: { "voice-assistant": "https://orqella.localhost" },
  });
  assert.equal(origins.originFor(local), "");
});
