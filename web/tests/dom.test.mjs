import assert from "node:assert/strict";
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { extname, join, normalize } from "node:path";
import test from "node:test";

// Playwright is optional for local runs. CI can install it or skip this file.
const require = createRequire(import.meta.url);
let chromium;
try {
  ({ chromium } = require("playwright"));
} catch {
  test("dashboard DOM smoke tests", { skip: "playwright is not installed" }, () => {});
}

const ROOT = new URL("..", import.meta.url).pathname;
const MIME = {
  ".html": "text/html; charset=utf-8",
  ".js": "application/javascript; charset=utf-8",
  ".mjs": "application/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
};

function staticServer() {
  return createServer(async (req, res) => {
    const path = new URL(req.url, "http://localhost").pathname;
    let file;
    if (path === "/" || path === "/index.html") file = join(ROOT, "index.html");
    else if (path.startsWith("/assets/")) file = join(ROOT, path.slice("/assets/".length));
    else if (path === "/sdk/orqella.js") file = join(ROOT, "sdk.js");
    else if (path.startsWith("/m/")) {
      res.writeHead(200, { "content-type": "text/html" });
      res.end("<!doctype html><html><body>module</body></html>");
      return;
    } else file = join(ROOT, normalize(path).replace(/^(\.\.[/\\])+/, ""));
    try {
      const body = await readFile(file);
      res.writeHead(200, { "content-type": MIME[extname(file)] || "application/octet-stream" });
      res.end(body);
    } catch {
      res.writeHead(404);
      res.end("not found");
    }
  });
}

const info = (origin) => ({
  name: "Orqella",
  version: "0.1.0",
  commit: "test",
  api_version: "1",
  node: { id: "local", name: "test", dashboard: true, origin },
  capabilities: ["system", "modules"],
  onboarding: { completed: true },
  themes: [{ id: "nocturne", name: "Nocturne" }],
  theme_path: "/tmp/theme.css",
  config: {
    port: 9477,
    bind: "127.0.0.1",
    dashboard: true,
    allow_remote: false,
    canonical_host: "orqella.localhost",
    origin,
    proxy: [],
    peers: [],
    audit: { enabled: false },
    sidecar: { sandbox: true, max_memory_mb: 1024 },
    update: {},
    theme: "nocturne",
    web: { enabled: false, stun_urls: [] },
    link: { enabled: false },
    github: { bridge_url: "" },
    agent: { preset: "opencode", command: "opencode", args: [], env: {}, timeout_secs: 0, viewer: {}, resolved_viewer: {} },
    agent_presets: [],
  },
});

const demoModule = {
  id: "demo",
  name: "Demo",
  version: "1.0.0",
  enabled: true,
  has_page: false,
  has_overview: true,
  backend: "none",
  backend_sandbox: true,
  permissions: [],
  granted_permissions: [],
  pending_permissions: [],
  overview_default: true,
  overview_width: 2,
  overview_height: 2,
  node_scope: "node",
  remote_node: null,
  ws_token: "",
};

async function withPage(fn) {
  const server = staticServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const origin = `http://127.0.0.1:${port}`;
  const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH || "/usr/bin/chromium" });
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/api/v1/info") return route.fulfill({ json: info(origin) });
    if (path === "/api/v1/modules") return route.fulfill({ json: [demoModule] });
    if (path === "/api/v1/nodes") {
      return route.fulfill({ json: [{ id: "local", name: "test", online: true, local: true, latency_ms: 0 }] });
    }
    if (path === "/api/v1/peers") return route.fulfill({ json: [] });
    return route.fulfill({ json: {} });
  });
  try {
    await page.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await fn(page, errors);
  } finally {
    await context.close();
    await browser.close();
    await new Promise((resolve) => server.close(resolve));
  }
}

test("shell boots and renders the overview card", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.waitForSelector("[data-overview-card='demo']");
    assert.equal(await page.locator("#railNode").textContent(), "test");
    assert.deepEqual(errors, []);
  });
});

test("settings tabs render and switch", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-view='settings']");
    await page.waitForSelector("[data-settings-tab='general']");
    assert.equal(await page.locator("[data-settings-tab]").count(), 3);
    await page.click("[data-settings-tab='dashboards']");
    await page.waitForSelector("[data-settings-tab='dashboards'].active");
    await page.click("[data-settings-tab='modules']");
    await page.waitForSelector("[data-settings-tab='modules'].active");
    assert.deepEqual(errors, []);
  });
});

test("build view renders the prompt form", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-view='build']");
    await page.waitForSelector("#buildPrompt");
    await page.waitForSelector("#startBuild");
    assert.deepEqual(errors, []);
  });
});

test("legacy overview storage is migrated before the first render", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    const stored = await page.evaluate(() => ({
      dashboards: localStorage.getItem("orqella.dashboards.v1"),
      marker: localStorage.getItem("orqella.migrations.v1"),
      legacyVisible: localStorage.getItem("orqella.overview.visible"),
    }));
    assert.ok(stored.dashboards);
    assert.ok(stored.marker);
    assert.equal(stored.legacyVisible, null);
    assert.deepEqual(errors, []);
  });
});

test("overview edit mode toggles with a rendered card", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.waitForSelector("[data-overview-card='demo']");
    await page.click("#overviewEditToggle");
    await page.waitForSelector(".overview-editbar");
    await page.click("#overviewEditDone");
    await page.waitForSelector("#overviewEditToggle");
    assert.deepEqual(errors, []);
  });
});
