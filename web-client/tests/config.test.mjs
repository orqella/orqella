import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../client/config.js", import.meta.url), "utf8");

function load(hostname, search = "", overrides = {}) {
  const location = { hostname, search };
  const window = { location, ...overrides };
  const context = vm.createContext({ window, location, URLSearchParams });
  vm.runInContext(source, context);
  return window;
}

test("production browser client enables module and app origins by default", () => {
  const config = load("orqella-web.fly.dev");
  assert.equal(config.__ORQELLA_REQUIRE_MODULE_ORIGINS, true);
  assert.equal(config.__ORQELLA_MODULE_ORIGIN_SUFFIX, ".modules.orqella.io");
  assert.equal(config.__ORQELLA_APP_ORIGIN_SUFFIX, ".apps.orqella.io");
});

test("canonical production browser client uses production wildcard origins", () => {
  const config = load("web.orqella.io");
  assert.equal(config.__ORQELLA_REQUIRE_MODULE_ORIGINS, true);
  assert.equal(config.__ORQELLA_MODULE_ORIGIN_SUFFIX, ".modules.orqella.io");
  assert.equal(config.__ORQELLA_APP_ORIGIN_SUFFIX, ".apps.orqella.io");
});

test("staging browser client uses staging wildcard origins", () => {
  const config = load("orqella-web-staging.fly.dev");
  assert.equal(config.__ORQELLA_MODULE_ORIGIN_SUFFIX, ".modules.staging.orqella.io");
  assert.equal(config.__ORQELLA_APP_ORIGIN_SUFFIX, ".apps.staging.orqella.io");
});

test("local browser client uses localhost module origins and app ports", () => {
  const config = load("127.0.0.1", "?machine=abc");
  assert.equal(config.__ORQELLA_MODULE_ORIGIN_SUFFIX, ".localhost");
  assert.equal(config.__ORQELLA_APP_ORIGIN_SUFFIX, "");
});

test("module origins can be disabled fail-closed", () => {
  const config = load("orqella-web.fly.dev", "?moduleOrigins=0");
  assert.equal(config.__ORQELLA_REQUIRE_MODULE_ORIGINS, true);
  assert.equal(config.__ORQELLA_MODULE_ORIGIN_SUFFIX, "");
});

test("an explicit suffix is preserved", () => {
  const config = load("orqella.example", "", {
    __ORQELLA_MODULE_ORIGIN_SUFFIX: ".modules.example",
  });
  assert.equal(config.__ORQELLA_MODULE_ORIGIN_SUFFIX, ".modules.example");
});
