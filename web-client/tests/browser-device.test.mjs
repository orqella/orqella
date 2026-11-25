import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../client/app.js", import.meta.url), "utf8");
const start = source.indexOf("const BROWSER_DEVICE_KEY");
const end = source.indexOf("async function browserRelayConfig", start);
if (start < 0 || end < 0) {
  throw new Error("could not locate browser device helpers in app.js");
}
const helpers = source.slice(start, end);

function response(status) {
  return { ok: status >= 200 && status < 300, status };
}

function createStorage(initial = {}) {
  const values = new Map(Object.entries(initial));
  return {
    getItem(key) {
      return values.has(key) ? values.get(key) : null;
    },
    setItem(key, value) {
      values.set(key, String(value));
    },
    removeItem(key) {
      values.delete(key);
    },
    values,
  };
}

function load({ storage, linkApi, fetch }) {
  const context = vm.createContext({
    localStorage: storage,
    linkAccountUrl: () => "https://link.test",
    linkApi,
    fetch,
    encodeURIComponent,
    JSON,
    String,
    Error,
    Promise,
  });
  vm.runInContext(helpers, context);
  return context;
}

const cached = (id, token) => ({
  "orqella.web.browserDevice": JSON.stringify({ id, token }),
  "orqella.web.browserDeviceId": id,
});

test("reuses a cached device token after the sign-in freshness window", async () => {
  const storage = createStorage(cached("dev-1", "tok-1"));
  const requests = [];
  const fetch = async (url, options = {}) => {
    requests.push({
      url,
      method: options.method || "GET",
      auth: options.headers?.Authorization,
      body: options.body,
    });
    if (url.endsWith("/v1/device-token/refresh")) return response(200);
    if (url.endsWith("/v1/devices/dev-1/heartbeat")) return response(200);
    throw new Error(`unexpected fetch ${url}`);
  };
  let minted = false;
  const linkApi = async (path) => {
    if (path === "/v1/devices") return { devices: [{ id: "dev-1", kind: "browser" }] };
    minted = true;
    throw new Error("should not mint a device token");
  };

  const context = load({ storage, linkApi, fetch });
  const token = await context.browserDeviceToken({ endpoint_id: "endpoint-new" });

  assert.equal(token, "tok-1");
  assert.equal(minted, false);
  assert.deepEqual(requests.map((request) => request.method), ["POST", "POST"]);
  assert.equal(requests[0].auth, "Bearer tok-1");
  assert.equal(requests[1].auth, "Bearer tok-1");
  assert.deepEqual(JSON.parse(requests[1].body), { iroh_endpoint_id: "endpoint-new" });
  assert.deepEqual(JSON.parse(storage.getItem("orqella.web.browserDevice")), {
    id: "dev-1",
    token: "tok-1",
  });
});

test("mints and caches a new token when the cached token is rejected", async () => {
  const storage = createStorage(cached("dev-1", "expired"));
  const fetch = async (url) => {
    if (url.endsWith("/v1/device-token/refresh")) return response(401);
    if (url.endsWith("/v1/devices/dev-1/heartbeat")) return response(200);
    throw new Error(`unexpected fetch ${url}`);
  };
  const linkApi = async (path, options = {}) => {
    if (path === "/v1/devices") return { devices: [{ id: "dev-1", kind: "browser" }] };
    assert.equal(path, "/v1/device-token");
    assert.equal(JSON.parse(options.body).device_id, "dev-1");
    return { token: "tok-2" };
  };

  const context = load({ storage, linkApi, fetch });
  const token = await context.browserDeviceToken({ endpoint_id: "endpoint-2" });

  assert.equal(token, "tok-2");
  assert.deepEqual(JSON.parse(storage.getItem("orqella.web.browserDevice")), {
    id: "dev-1",
    token: "tok-2",
  });
});

test("registers a browser device and caches its first device token", async () => {
  const storage = createStorage();
  const linkApi = async (path, options = {}) => {
    if (path === "/v1/devices" && !options.method) return { devices: [] };
    if (path === "/v1/devices" && options.method === "POST") {
      assert.deepEqual(JSON.parse(options.body), {
        name: "Orqella Web",
        kind: "browser",
        iroh_endpoint_id: "endpoint-3",
        ssh_port: 22,
        peer_port: 9477,
      });
      return { device: { id: "dev-new" } };
    }
    if (path === "/v1/device-token" && options.method === "POST") {
      assert.equal(JSON.parse(options.body).device_id, "dev-new");
      return { token: "tok-3" };
    }
    throw new Error(`unexpected linkApi ${path}`);
  };
  const fetch = async (url) => {
    throw new Error(`unexpected fetch ${url}`);
  };

  const context = load({ storage, linkApi, fetch });
  const token = await context.browserDeviceToken({ endpoint_id: "endpoint-3" });

  assert.equal(token, "tok-3");
  assert.deepEqual(JSON.parse(storage.getItem("orqella.web.browserDevice")), {
    id: "dev-new",
    token: "tok-3",
  });
  assert.equal(storage.getItem("orqella.web.browserDeviceId"), "dev-new");
});

test("picks up a replacement token written by another tab", async () => {
  const storage = createStorage(cached("dev-1", "old"));
  const authHeaders = [];
  const fetch = async (url, options = {}) => {
    const auth = options.headers?.Authorization || "";
    authHeaders.push(auth);
    if (url.endsWith("/v1/device-token/refresh")) {
      if (auth === "Bearer old") {
        storage.setItem("orqella.web.browserDevice", JSON.stringify({ id: "dev-1", token: "new" }));
        return response(401);
      }
      return response(200);
    }
    if (url.endsWith("/v1/devices/dev-1/heartbeat")) return response(200);
    throw new Error(`unexpected fetch ${url}`);
  };
  const linkApi = async (path) => {
    if (path === "/v1/devices") return { devices: [{ id: "dev-1", kind: "browser" }] };
    throw new Error("should not mint a device token");
  };

  const context = load({ storage, linkApi, fetch });
  const token = await context.browserDeviceToken({ endpoint_id: "endpoint-4" });

  assert.equal(token, "new");
  assert.deepEqual(authHeaders, ["Bearer old", "Bearer new", "Bearer new"]);
  assert.deepEqual(JSON.parse(storage.getItem("orqella.web.browserDevice")), {
    id: "dev-1",
    token: "new",
  });
});

test("does not reuse a token from a different account", async () => {
  const storage = createStorage(cached("dev-old", "tok-old"));
  const fetch = async (url) => {
    throw new Error(`unexpected fetch ${url}`);
  };
  const linkApi = async (path, options = {}) => {
    if (path === "/v1/devices" && !options.method) return { devices: [] };
    if (path === "/v1/devices" && options.method === "POST") {
      assert.equal(JSON.parse(options.body).iroh_endpoint_id, "endpoint-6");
      return { device: { id: "dev-new" } };
    }
    if (path === "/v1/device-token" && options.method === "POST") {
      const deviceId = JSON.parse(options.body).device_id;
      if (deviceId === "dev-old") throw new Error("device not found");
      assert.equal(deviceId, "dev-new");
      return { token: "tok-new" };
    }
    throw new Error(`unexpected linkApi ${path}`);
  };

  const context = load({ storage, linkApi, fetch });
  const token = await context.browserDeviceToken({ endpoint_id: "endpoint-6" });

  assert.equal(token, "tok-new");
  assert.deepEqual(JSON.parse(storage.getItem("orqella.web.browserDevice")), {
    id: "dev-new",
    token: "tok-new",
  });
  assert.equal(storage.getItem("orqella.web.browserDeviceId"), "dev-new");
});

test("surfaces a transient heartbeat failure instead of registering a duplicate", async () => {
  const storage = createStorage({ "orqella.web.browserDeviceId": "dev-1" });
  const fetch = async (url) => {
    if (url.endsWith("/v1/devices/dev-1/heartbeat")) return response(503);
    throw new Error(`unexpected fetch ${url}`);
  };
  let listed = false;
  const linkApi = async (path) => {
    if (path === "/v1/device-token") return { token: "tok-5" };
    if (path === "/v1/devices") listed = true;
    throw new Error(`unexpected linkApi ${path}`);
  };

  const context = load({ storage, linkApi, fetch });
  await assert.rejects(
    context.browserDeviceToken({ endpoint_id: "endpoint-5" }),
    /heartbeat failed \(HTTP 503\)/,
  );
  assert.equal(listed, false);
});
