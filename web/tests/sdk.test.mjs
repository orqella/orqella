import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../sdk.js", import.meta.url), "utf8");

function harness({ hash = "", websocket = true } = {}) {
  const sent = [];
  const listeners = [];
  const appended = [];
  const removed = [];
  const documents = [];

  class FakeCustomEvent {
    constructor(type, init = {}) {
      this.type = type;
      this.detail = init.detail;
    }
  }

  class FakeWebSocket {
    static CONNECTING = 0;
    static OPEN = 1;
    static CLOSING = 2;
    static CLOSED = 3;

    constructor(url, protocols) {
      this.url = url;
      this.protocols = protocols;
      this.sent = sent;
    }
  }

  const parent = {
    postMessage(message, targetOrigin) {
      sent.push({ message, targetOrigin });
    },
  };
  const window = {
    location: {
      hash,
      href: `http://orqella.localhost:9477/m/demo/index.html${hash}`,
      host: "orqella.localhost:9477",
    },
    addEventListener(type, callback) {
      listeners.push({ type, callback });
    },
    WebSocket: websocket ? FakeWebSocket : undefined,
  };
  const document = {
    readyState: "complete",
    documentElement: { dataset: {} },
    querySelectorAll(selector) {
      assert.equal(selector, 'link[rel="stylesheet"]');
      return documents.slice();
    },
    head: {
      appendChild(node) {
        appended.push(node);
      },
    },
    createElement(tag) {
      return { tag, href: "", onload: null, cloneNode() { return { ...this }; } };
    },
    dispatchEvent(event) {
      return event;
    },
    addEventListener() {},
  };
  const context = vm.createContext({
    window,
    document,
    location: window.location,
    parent,
    URL,
    console,
    setTimeout: (callback) => callback(),
    CustomEvent: FakeCustomEvent,
  });
  vm.runInContext(source, context);
  const readyMessages = sent.splice(0);

  const dispatch = (data) => {
    for (const listener of listeners) {
      if (listener.type === "message") {
        listener.callback({ source: parent, data });
      }
    }
  };
  return {
    window,
    document,
    parent,
    sent,
    dispatch,
    appended,
    removed,
    documents,
    readyMessages,
    FakeWebSocket,
  };
}

test("requests are queued until init and carry the frame nonce", async () => {
  const h = harness({ hash: "#__orqella_frame=nonce%201" });
  assert.equal(h.readyMessages.at(-1).message.type, "orqella:frame-ready");
  assert.equal(h.readyMessages.at(-1).message.nonce, "nonce 1");
  const pending = h.window.orqella.call("module.info", { x: 1 });
  assert.equal(h.sent.length, 0);
  h.dispatch({ type: "orqella:init", context: { theme: "signal", module: { id: "demo" } } });
  await Promise.resolve();
  assert.equal(h.sent.length, 1);
  const request = h.sent[0].message;
  assert.equal(request.type, "orqella:request");
  assert.equal(request.method, "module.info");
  assert.deepEqual(request.params, { x: 1 });
  assert.equal(request.nonce, "nonce 1");
  assert.equal(h.window.orqella.context.theme, "signal");
  assert.equal(h.document.documentElement.dataset.orqellaTheme, "signal");
  h.dispatch({ type: "orqella:response", id: request.id, ok: true, result: { ok: true } });
  assert.deepEqual(await pending, { ok: true });
});

test("ready callbacks run immediately or after init", () => {
  const h = harness();
  const seen = [];
  h.window.orqella.ready((context) => seen.push(context));
  assert.equal(seen.length, 0);
  h.dispatch({ type: "orqella:init", context: { theme: "daybreak" } });
  assert.deepEqual(seen, [{ theme: "daybreak" }]);
  h.window.orqella.ready((context) => seen.push(context));
  assert.equal(seen.length, 2);
  h.window.orqella.onContext((context) => seen.push(context));
  h.dispatch({ type: "orqella:context", context: { theme: "foundry" } });
  assert.deepEqual(h.window.orqella.context, { theme: "foundry" });
});

test("failed responses reject with the daemon error", async () => {
  const h = harness();
  h.dispatch({ type: "orqella:init", context: {} });
  const pending = h.window.orqella.call("system.metrics", {});
  await Promise.resolve();
  const request = h.sent[0].message;
  h.dispatch({
    type: "orqella:response",
    id: request.id,
    ok: false,
    error: "permission denied",
  });
  await assert.rejects(pending, /permission denied/);
});

test("hook helpers use the generic host API", async () => {
  const h = harness();
  h.dispatch({ type: "orqella:init", context: {} });
  const listed = h.window.orqella.hooks.list();
  await Promise.resolve();
  assert.equal(h.sent.at(-1).message.method, "hooks.list");
  h.dispatch({ type: "orqella:response", id: h.sent.at(-1).message.id, ok: true, result: [] });
  assert.deepEqual(await listed, []);

  const ran = h.window.orqella.hooks.run("agent.prompt", { prompt: "hi" });
  await Promise.resolve();
  const request = h.sent.at(-1).message;
  assert.equal(request.method, "hooks.run");
  assert.equal(request.params.name, "agent.prompt");
  assert.equal(request.params.params.prompt, "hi");
  h.dispatch({ type: "orqella:response", id: request.id, ok: true, result: { job: 1 } });
  assert.deepEqual(await ran, { job: 1 });
});

test("module websocket URLs receive the frame token but other URLs do not", () => {
  const h = harness({ hash: "#__orqella_ws=ws-secret" });
  const socket = new h.window.WebSocket("ws://orqella.localhost:9477/m/terminal/ws/terminal");
  const target = new URL(socket.url);
  assert.equal(target.searchParams.get("__orqella_ws"), "ws-secret");
  assert.equal(target.pathname, "/m/terminal/ws/terminal");

  const remote = new h.window.WebSocket(
    "ws://orqella.localhost:9477/n/laptop/m/terminal/ws/terminal",
  );
  assert.equal(new URL(remote.url).searchParams.get("__orqella_ws"), "ws-secret");

  const app = new h.window.WebSocket("ws://orqella.localhost:9477/p/opencode/ws");
  assert.equal(new URL(app.url).searchParams.has("__orqella_ws"), false);

  const external = new h.window.WebSocket("wss://evil.example/m/demo/ws");
  assert.equal(external.url, "wss://evil.example/m/demo/ws");

  const existing = new h.window.WebSocket(
    "ws://orqella.localhost:9477/m/demo/ws?__orqella_ws=already",
  );
  assert.equal(new URL(existing.url).searchParams.get("__orqella_ws"), "already");
});

test("context updates refresh the theme stylesheet with a cache bust", () => {
  const h = harness();
  const oldLink = {
    href: "http://orqella.localhost:9477/assets/theme.css",
    cloneNode() {
      return { href: "", onload: null, remove() {} };
    },
    remove() {
      h.removed.push(this);
    },
  };
  h.documents.push(oldLink);
  h.dispatch({ type: "orqella:init", context: {} });
  h.dispatch({ type: "orqella:context", context: { theme: "signal" } });
  assert.equal(h.appended.length, 1);
  assert.match(h.appended[0].href, /^\/assets\/theme\.css\?v=\d+$/);
  h.appended[0].onload();
  assert.deepEqual(h.removed, [oldLink]);
});
