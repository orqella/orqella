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

async function withPage(fn, { dashboards = null, localStorage: localStorageSeed = null } = {}) {
  const server = staticServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const origin = `http://127.0.0.1:${port}`;
  const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH || "/usr/bin/chromium" });
  const context = await browser.newContext();
  const page = await context.newPage();
  const errors = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  if (localStorageSeed) {
    await page.addInitScript((entries) => {
      try {
        for (const [key, value] of Object.entries(entries)) localStorage.setItem(key, value);
      } catch {}
    }, localStorageSeed);
  }
  let dashboardStore = dashboards
    ? { version: 1, revision: 1, initialized: true, dashboards }
    : { version: 1, revision: 0, initialized: false, dashboards: [] };
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/api/v1/info") return route.fulfill({ json: info(origin) });
    if (path === "/api/v1/modules") return route.fulfill({ json: [demoModule] });
    if (path === "/api/v1/nodes") {
      return route.fulfill({ json: [{ id: "local", name: "test", online: true, local: true, latency_ms: 0 }] });
    }
    if (path === "/api/v1/peers") return route.fulfill({ json: [] });
    if (path === "/api/v1/update/check/local") {
      return route.fulfill({
        json: {
          ok: true,
          available: true,
          behind: 2,
          dirty: false,
          current_short: "aaaaaaa1",
          latest_short: "bbbbbbb2",
        },
      });
    }
    if (path === "/api/v1/link/devices") return route.fulfill({ json: { devices: [] } });
    if (path === "/api/v1/link/device/start") {
      return route.fulfill({
        json: {
          device_code: "device-code",
          user_code: "ABCD1234",
          confirmation_code: "123456",
          verification_uri: "https://example.test/login/cli",
          expires_in: 600,
          interval: 1,
        },
      });
    }
    if (path === "/api/v1/link/device/poll") {
      return route.fulfill({ json: { status: "approved" } });
    }
    if (path === "/api/v1/config/theme/agent" && route.request().method() === "POST") {
      return route.fulfill({
        json: {
          ok: true,
          job: {
            id: "theme-job",
            prompt: "a moonlit theme",
            status: "queued",
            error: null,
            module_id: null,
            workspace: "/tmp/theme-job",
            kind: "theme",
            started_at: "2026-11-14T20:00:00.000Z",
            finished_at: null,
            session_id: null,
          },
        },
      });
    }
    if (path === "/api/v1/config/theme/agent/theme-job") {
      return route.fulfill({
        json: {
          id: "theme-job",
          prompt: "a moonlit theme",
          status: "succeeded",
          error: null,
          module_id: null,
          workspace: "/tmp/theme-job",
          kind: "theme",
          started_at: "2026-11-14T20:00:00.000Z",
          finished_at: "2026-11-14T20:02:00.000Z",
          session_id: null,
        },
      });
    }
    if (path === "/api/v1/dashboards") {
      if (route.request().method() === "PUT") {
        const body = route.request().postDataJSON();
        dashboardStore = {
          version: 1,
          revision: dashboardStore.revision + 1,
          initialized: true,
          dashboards: Array.isArray(body?.dashboards) ? body.dashboards : [],
        };
        return route.fulfill({ json: dashboardStore });
      }
      return route.fulfill({ json: dashboardStore });
    }
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
    assert.equal(await page.locator("#settingsPanel details[open]").count(), 0);
    assert.equal(await page.locator("#generalSettings").count(), 1);
    assert.equal(await page.locator("#daemonSettings, #updateSettings, #linkSettings, #peerSettings").count(), 0);
    assert.equal(await page.locator("#auditToggle").count(), 1);
    assert.equal(await page.locator("#linkRefresh").textContent(), "Refresh Linked Devices");
    assert.equal(await page.locator("#linkSignIn").textContent(), "Sign In");
    await page.click("#generalSettings > summary");
    assert.equal(await page.locator("#linkSignIn").isVisible(), true);
    assert.equal(await page.locator("#linkRestart").isVisible(), false);
    assert.equal(await page.locator("#linkSignOut").isVisible(), false);
    await page.click("[data-settings-tab='dashboards']");
    await page.waitForSelector("[data-settings-tab='dashboards'].active");
    await page.click("[data-settings-tab='modules']");
    await page.waitForSelector("[data-settings-tab='modules'].active");
    assert.deepEqual(errors, []);
  });
});

test("machine check turns into an update action", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-view='settings']");
    await page.click("#generalSettings > summary");
    const action = page.locator("[data-machine-action='local']");
    assert.equal(await action.textContent(), "Check for updates");
    await action.click();
    await page.waitForFunction(() =>
      document.querySelector("[data-machine-action='local']")?.textContent === "Update"
    );
    assert.deepEqual(errors, []);
  });
});

test("theme prompt starts the default agent and applies the result", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-view='settings']");
    await page.click("#themeSettings > summary");
    await page.fill("#themePrompt", "a moonlit theme");
    await page.click("#generateTheme");
    await page.waitForFunction(() =>
      document.querySelector("#themeAgentStatus")?.textContent === "Theme applied."
    );
    assert.equal(await page.locator("#themePrompt").inputValue(), "");
    assert.deepEqual(errors, []);
  });
});

test("link sign-in fallback restores the connected actions", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-view='settings']");
    await page.click("#generalSettings > summary");
    assert.equal(await page.locator("#linkSignIn").isVisible(), true);
    await page.click("#linkSignIn");
    await page.waitForSelector("#linkSignIn", { state: "hidden" });
    assert.equal(await page.locator("#linkRestart").isVisible(), true);
    assert.equal(await page.locator("#linkSignOut").isVisible(), true);
    assert.match(await page.locator("#linkSignInStatus").textContent(), /Signed in/);
    assert.deepEqual(errors, []);
  });
});

test("creating a dashboard from settings persists it locally and in the daemon", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-view='settings']");
    await page.click("[data-settings-tab='dashboards']");
    await page.fill("#newDashboardName", "Lab");
    await page.click("#createDashboard");
    await page.locator("[data-dashboard]", { hasText: "Lab" }).waitFor();
    await page.waitForFunction(async () => {
      const response = await fetch("/api/v1/dashboards", { headers: { "X-Orqella": "1" } });
      const data = await response.json();
      return data.initialized && data.dashboards?.some((dashboard) => dashboard.name === "Lab");
    });
    const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("orqella.dashboards.v1")));
    assert.ok(stored.dashboards.some((dashboard) => dashboard.name === "Lab"));
    const remote = await page.evaluate(async () => {
      const response = await fetch("/api/v1/dashboards", { headers: { "X-Orqella": "1" } });
      return response.json();
    });
    assert.ok(remote.dashboards.some((dashboard) => dashboard.name === "Lab"));
    assert.ok(remote.dashboards.every((dashboard) => !("layout" in dashboard)));
    assert.deepEqual(errors, []);
  });
});

test("pre-sync local dashboards merge into the daemon once", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.locator("[data-dashboard]", { hasText: "Legacy" }).waitFor();
    await page.locator("[data-dashboard]", { hasText: "Team" }).waitFor();
    const remote = await page.evaluate(async () => {
      const response = await fetch("/api/v1/dashboards", { headers: { "X-Orqella": "1" }, cache: "no-store" });
      return response.json();
    });
    assert.ok(remote.dashboards.some((dashboard) => dashboard.name === "Legacy"));
    assert.ok(remote.dashboards.some((dashboard) => dashboard.name === "Team"));
    assert.equal(await page.evaluate(() => localStorage.getItem("orqella.dashboards.shared.v1")), "1");
    assert.deepEqual(errors, []);
  }, {
    dashboards: [{ id: "dashboard-team", name: "Team", visible: { demo: true } }],
    localStorage: {
      "orqella.dashboards.v1": JSON.stringify({
        version: 1,
        dashboards: [
          { id: "overview", name: "Overview", protected: true, defaultVisible: true, layout: {}, visible: {} },
          { id: "dashboard-legacy", name: "Legacy", protected: false, defaultVisible: false, layout: {}, visible: { demo: true } },
        ],
      }),
    },
  });
});

test("dashboards stored by the daemon appear for a new client", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.locator("[data-dashboard]", { hasText: "Team" }).waitFor();
    await page.locator("[data-dashboard]", { hasText: "Team" }).click();
    await page.waitForSelector("[data-overview-card='demo']");
    const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("orqella.dashboards.v1")));
    const team = stored.dashboards.find((dashboard) => dashboard.name === "Team");
    assert.ok(team);
    assert.ok(team.layout, "the new client should create a local default layout");
    assert.deepEqual(errors, []);
  }, {
    dashboards: [{ id: "dashboard-team", name: "Team", visible: { demo: true } }],
  });
});

test("settings build button opens the build view", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-view='settings']");
    await page.locator("details", { hasText: "Install module" }).locator("summary").click();
    await page.click("#newModule");
    await page.waitForSelector("#buildPrompt");
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
