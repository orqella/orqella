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
    else if (path === "/m/demo-page/" || path === "/m/demo-launcher/") {
      const launcher = path === "/m/demo-launcher/";
      res.writeHead(200, { "content-type": "text/html" });
      res.end(`<!doctype html><html><head><meta charset="utf-8"><script src="/sdk/orqella.js"></script></head><body><p id="status">boot</p><script>
window.orqella.ready(async () => {
  document.getElementById("status").textContent = ${launcher ? '"launching"' : '"ready"'};
  try {
    ${launcher
      ? 'await window.orqella.call("app.open", { mount: "demo" }); document.getElementById("status").textContent = "opened";'
      : 'const result = await window.orqella.call("module.info", {}); document.getElementById("status").textContent = result?.name || "ok";'}
  } catch (error) {
    document.getElementById("status").textContent = "error:" + error.message;
  }
});
</script></body></html>`);
      return;
    } else if (path.startsWith("/m/")) {
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

const demoIndex = {
  schema: 1,
  platform: "linux-x86_64",
  api_version: "1",
  index_url: "https://example.test/index.json",
  modules: [
    {
      id: "demo-index",
      name: "Demo index",
      description: "A module from the test index.",
      category: "Test",
      tags: ["test"],
      publisher: { name: "Test", url: "https://example.test" },
      official: true,
      homepage: "https://example.test/demo-index",
      license: "MIT",
      node_scope: "node",
      permissions: ["backend"],
      latest_version: "1.0.0",
      latest_released_at: "2026-11-22",
      notes_url: "",
      platforms: ["linux-x86_64"],
      compiled_backend: false,
      backend_platforms: [],
      compatible: true,
      incompatible_reason: null,
      installed: false,
      installed_version: null,
      installed_source: null,
      update_available: false,
      source_type: "git",
    },
  ],
};

const demoPage = {
  ...demoModule,
  id: "demo-page",
  name: "Demo page",
  has_page: true,
  has_overview: false,
  overview_default: false,
};

const demoLauncher = {
  ...demoModule,
  id: "demo-launcher",
  name: "Demo launcher",
  has_page: true,
  has_overview: false,
  overview_default: false,
};

async function withPage(fn, {
  dashboards = null,
  localStorage: localStorageSeed = null,
  modules = [demoModule],
  viewport = null,
} = {}) {
  const server = staticServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const origin = `http://127.0.0.1:${port}`;
  const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH || "/usr/bin/chromium" });
  const context = await browser.newContext(viewport ? { viewport } : {});
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
  let linkConnected = false;
  let dashboardStore = dashboards
    ? {
        version: 2,
        revision: 1,
        initialized: true,
        dashboards,
        overview: { initialized: true, visible: {} },
        layouts: {},
        layoutsInitialized: true,
      }
    : {
        version: 2,
        revision: 0,
        initialized: false,
        dashboards: [],
        overview: { initialized: false, visible: {} },
        layouts: {},
        layoutsInitialized: false,
      };
  let themeCatalog = info(origin).themes;
  const generatedTheme = {
    id: "custom-moonlit-12345678",
    name: "Moonlit",
    description: "A moonlit theme.",
    color_scheme: "dark",
    custom: true,
  };
  await page.route("**/api/v1/**", async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/api/v1/info") {
      return route.fulfill({ json: { ...info(origin), themes: themeCatalog } });
    }
    if (path === "/api/v1/modules") return route.fulfill({ json: modules });
    if (path === "/api/v1/module-index" || path === "/api/v1/nodes/local/module-index") {
      return route.fulfill({ json: demoIndex });
    }
    if (path === "/api/v1/modules/demo-page/rpc" || path === "/api/v1/modules/demo-launcher/rpc") {
      const body = route.request().postDataJSON();
      if (body?.method === "module.info") {
        return route.fulfill({ json: { ok: true, result: { name: "Page ready" } } });
      }
      if (body?.method === "app.open") {
        return route.fulfill({
          json: {
            ok: true,
            result: {
              mount: "demo",
              host: "demo.orqella.localhost",
              url: "http://demo.orqella.localhost/",
              path: "/",
            },
          },
        });
      }
      return route.fulfill({ json: { ok: true, result: {} } });
    }
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
    if (path === "/api/v1/link") {
      return route.fulfill({
        json: {
          status: {
            enabled: linkConnected,
            device_name: linkConnected ? "test" : "",
            link_plan: "free",
          },
          config: { enabled: linkConnected },
          error: null,
        },
      });
    }
    if (path === "/api/v1/link/devices") return route.fulfill({ json: { devices: [] } });
    if (path === "/api/v1/link/device/start") {
      return route.fulfill({
        json: {
          device_code: "device-code",
          user_code: "ABCD1234",
          verification_uri: "https://example.test/login/cli",
          expires_in: 600,
          interval: 1,
        },
      });
    }
    if (path === "/api/v1/link/device/poll") {
      linkConnected = true;
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
          theme_id: generatedTheme.id,
          theme_name: generatedTheme.name,
        },
      });
    }
    if (path === "/api/v1/config/theme/agent/theme-job") {
      if (!themeCatalog.some((theme) => theme.id === generatedTheme.id)) {
        themeCatalog = [...themeCatalog, generatedTheme];
      }
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
    const layoutRoute = path.match(/^\/api\/v1\/dashboards\/([^/]+)\/layout\/(desktop|mobile)$/);
    if (layoutRoute && route.request().method() === "PUT") {
      const [, id, profile] = layoutRoute;
      const body = route.request().postDataJSON();
      dashboardStore = {
        ...dashboardStore,
        version: 2,
        revision: dashboardStore.revision + 1,
        initialized: true,
        layoutsInitialized: true,
        layouts: {
          ...dashboardStore.layouts,
          [id]: { ...(dashboardStore.layouts[id] || {}), [profile]: body?.layout },
        },
      };
      return route.fulfill({ json: dashboardStore });
    }
    if (path === "/api/v1/dashboards") {
      if (route.request().method() === "PUT") {
        const body = route.request().postDataJSON();
        dashboardStore = {
          ...dashboardStore,
          version: 2,
          revision: dashboardStore.revision + 1,
          initialized: true,
          dashboards: Array.isArray(body?.dashboards) ? body.dashboards : [],
          overview: body?.overview
            ? { initialized: true, visible: body.overview.visible || {} }
            : dashboardStore.overview,
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
    assert.equal(await page.locator("[data-settings-tab]").count(), 4);
    assert.equal(await page.locator("#generalSettings").count(), 1);
    assert.equal(await page.locator("#daemonSettings, #updateSettings, #linkSettings, #peerSettings").count(), 0);
    await page.waitForSelector(".module-index-item");
    assert.match(await page.locator(".module-index-item").first().textContent(), /Demo index/);
    assert.equal(await page.locator("#auditToggle").count(), 1);
    assert.equal(await page.locator("#linkRefresh").textContent(), "Refresh");
    await page.waitForFunction(() =>
      document.querySelector("#linkStatusLine")?.textContent === "Not signed in"
    );
    assert.equal(await page.locator("#linkSignIn").textContent(), "Sign In");
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

test("btop keeps the install module header clear of the index body", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.addStyleTag({ path: new URL("../themes/btop.css", import.meta.url).pathname });
    await page.click("[data-view='settings']");
    await page.waitForSelector(".module-index-item");
    const gap = await page.evaluate(() => {
      const head = document.querySelector("#moduleIndexSettings .settings-section-head");
      const body = document.querySelector("#moduleIndexSettings .settings-section-body");
      const contentTop = body.getBoundingClientRect().top + parseFloat(getComputedStyle(body).paddingTop);
      return Math.round(contentTop - head.getBoundingClientRect().bottom);
    });
    assert.ok(gap >= 0, `the inlaid header overlaps the module index by ${-gap}px`);
    assert.deepEqual(errors, []);
  }, { viewport: { width: 390, height: 844 } });
});

test("machine check turns into an update action", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-view='settings']");
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
    await page.fill("#themePrompt", "a moonlit theme");
    await page.click("#generateTheme");
    await page.waitForFunction(() =>
      document.querySelector("#themeAgentStatus")?.textContent === "Theme created."
    );
    assert.equal(await page.locator("#themeSelect").inputValue(), "custom-moonlit-12345678");
    assert.equal(await page.locator("#themeSelect option:checked").textContent(), "Moonlit");
    assert.equal(await page.locator("#themePrompt").inputValue(), "");
    assert.deepEqual(errors, []);
  });
});

test("link sign-in fallback restores the connected actions", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-view='settings']");
    assert.equal(await page.locator("#linkSignIn").isVisible(), true);
    await page.click("#linkSignIn");
    await page.waitForSelector("#linkSignIn", { state: "hidden" });
    assert.match(await page.locator("#linkStatusLine").textContent(), /Connected as test/);
    assert.equal(await page.locator("#linkRestart").isVisible(), true);
    assert.equal(await page.locator("#linkSignOut").isVisible(), true);
    assert.equal(await page.locator("#linkRefresh").isVisible(), true);
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

test("overview edit toggle sits below the cards at the left", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.waitForSelector("[data-overview-card='demo']");
    const placement = await page.evaluate(() => {
      const stack = document.querySelector(".overview-stack");
      const grid = document.querySelector(".overview-grid");
      const toolbar = document.querySelector(".overview-toolbar");
      const button = document.querySelector("#overviewEditToggle");
      return {
        afterGrid: Boolean(grid.compareDocumentPosition(toolbar) & Node.DOCUMENT_POSITION_FOLLOWING),
        belowGrid: button.getBoundingClientRect().top >= grid.getBoundingClientRect().bottom,
        alignedLeft: Math.abs(button.getBoundingClientRect().left - stack.getBoundingClientRect().left) < 1,
      };
    });
    assert.ok(placement.afterGrid, "Edit control should follow the overview grid");
    assert.ok(placement.belowGrid, "Edit button should sit below the cards");
    assert.ok(placement.alignedLeft, "Edit button should align to the left edge");
    assert.deepEqual(errors, []);
  });
});

test("overview grid uses the full content width on wide screens", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.waitForSelector("[data-overview-card='demo']");
    const metrics = await page.evaluate(() => {
      const stack = document.querySelector(".overview-stack");
      const grid = document.querySelector(".overview-grid");
      return {
        stack: stack.getBoundingClientRect().width,
        grid: grid.getBoundingClientRect().width,
        columns: Number(grid.dataset.overviewColumns),
      };
    });
    assert.ok(metrics.stack > 1100, `dashboard should not be capped at 1100px, got ${metrics.stack}`);
    assert.ok(Math.abs(metrics.grid - metrics.stack) < 1, `grid ${metrics.grid} should fill stack ${metrics.stack}`);
    assert.equal(metrics.columns, 4);
    assert.deepEqual(errors, []);
  }, { viewport: { width: 1920, height: 1080 } });
});

test("desktop and mobile card orders render independently", { skip: chromium ? false : "playwright is not installed" }, async () => {
  const second = { ...demoModule, id: "second", name: "Second" };
  const layout = {
    profiles: {
      desktop: {
        columns: 4,
        order: ["demo", "second"],
        sizes: {},
        positions: {},
        fullWidth: [],
      },
      mobile: { order: ["second", "demo"], sizes: {} },
    },
  };
  await withPage(async (page, errors) => {
    const order = () => page.evaluate(() =>
      [...document.querySelectorAll("[data-overview-card]")].map((card) => card.dataset.overviewCard));
    await page.waitForSelector("[data-overview-card='demo']");
    await page.waitForSelector("[data-overview-card='second']");
    assert.deepEqual(await order(), ["demo", "second"]);
    await page.setViewportSize({ width: 600, height: 900 });
    await page.waitForFunction(() => {
      const cards = [...document.querySelectorAll("[data-overview-card]")].map((card) => card.dataset.overviewCard);
      return cards.join(",") === "second,demo";
    });
    assert.deepEqual(await order(), ["second", "demo"]);
    await page.setViewportSize({ width: 1280, height: 900 });
    await page.waitForFunction(() => {
      const cards = [...document.querySelectorAll("[data-overview-card]")].map((card) => card.dataset.overviewCard);
      return cards.join(",") === "demo,second";
    });
    assert.deepEqual(await order(), ["demo", "second"]);
    assert.deepEqual(errors, []);
  }, {
    modules: [demoModule, second],
    localStorage: {
      "orqella.dashboards.v1": JSON.stringify({
        version: 1,
        dashboards: [{
          id: "overview",
          name: "Overview",
          protected: true,
          defaultVisible: true,
          layout,
          visible: {},
        }],
      }),
    },
  });
});

test("overview visibility is saved to the daemon", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.waitForSelector("[data-overview-card='demo']");
    await page.click("#overviewEditToggle");
    await page.waitForSelector("[data-overview-toggle='demo']");
    await page.uncheck("[data-overview-toggle='demo']");
    await page.waitForFunction(async () => {
      const response = await fetch("/api/v1/dashboards", { headers: { "X-Orqella": "1" }, cache: "no-store" });
      const data = await response.json();
      return data.overview?.initialized === true && data.overview?.visible?.demo === false;
    });
    assert.equal(await page.locator("[data-overview-card='demo']").count(), 0);
    assert.deepEqual(errors, []);
  }, { dashboards: [] });
});

test("narrow windows fold grid columns without horizontal overflow", { skip: chromium ? false : "playwright is not installed" }, async () => {
  const second = { ...demoModule, id: "second", name: "Second" };
  const layout = {
    profiles: {
      desktop: {
        columns: 12,
        order: ["demo", "second"],
        sizes: {},
        positions: { demo: { x: 0, y: 0 }, second: { x: 10, y: 0 } },
        fullWidth: [],
      },
      mobile: { order: ["demo", "second"], sizes: {} },
    },
  };
  await withPage(async (page, errors) => {
    await page.waitForSelector("[data-overview-card='second']");
    const gridMetrics = () => page.evaluate(() => {
      const grid = document.querySelector(".overview-grid");
      return {
        columns: Number(grid.dataset.overviewColumns),
        client: grid.clientWidth,
        scroll: grid.scrollWidth,
      };
    });
    assert.equal((await gridMetrics()).columns, 12);
    await page.setViewportSize({ width: 1000, height: 900 });
    await page.waitForFunction(() => Number(document.querySelector(".overview-grid")?.dataset.overviewColumns) === 6);
    const narrow = await gridMetrics();
    assert.ok(narrow.scroll <= narrow.client + 1, `grid overflowed: ${narrow.scroll} > ${narrow.client}`);
    await page.setViewportSize({ width: 600, height: 900 });
    await page.waitForFunction(() => Number(document.querySelector(".overview-grid")?.dataset.overviewColumns) === 1);
    const phone = await gridMetrics();
    assert.ok(phone.scroll <= phone.client + 1, `phone grid overflowed: ${phone.scroll} > ${phone.client}`);
    assert.deepEqual(errors, []);
  }, {
    modules: [demoModule, second],
    localStorage: {
      "orqella.dashboards.v1": JSON.stringify({
        version: 1,
        dashboards: [{
          id: "overview",
          name: "Overview",
          protected: true,
          defaultVisible: true,
          layout,
          visible: {},
        }],
      }),
    },
    viewport: { width: 1920, height: 1080 },
  });
});

test("dragging on desktop leaves the mobile order untouched", { skip: chromium ? false : "playwright is not installed" }, async () => {
  const second = { ...demoModule, id: "second", name: "Second" };
  const layout = {
    profiles: {
      desktop: {
        columns: 4,
        order: ["demo", "second"],
        sizes: {},
        positions: { demo: { x: 0, y: 0 }, second: { x: 2, y: 0 } },
        fullWidth: [],
      },
      mobile: { order: ["second", "demo"], sizes: {} },
    },
  };
  await withPage(async (page, errors) => {
    await page.waitForSelector("[data-overview-card='second']");
    await page.click("#overviewEditToggle");
    await page.waitForSelector(".overview-editbar");
    const head = await page.locator("[data-overview-card='demo'] .overview-card-head").boundingBox();
    const target = await page.locator("[data-overview-card='second']").boundingBox();
    assert.ok(head && target);
    await page.mouse.move(head.x + head.width / 2, head.y + head.height / 2);
    await page.mouse.down();
    await page.mouse.move(head.x + head.width / 2 + 8, head.y + head.height / 2, { steps: 2 });
    await page.mouse.move(target.x + 20, target.y + 20, { steps: 12 });
    await page.mouse.up();
    await page.waitForTimeout(400);
    const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("orqella.dashboards.v1")));
    const overview = stored.dashboards.find((dashboard) => dashboard.id === "overview");
    assert.deepEqual(overview.layout.profiles.mobile.order, ["second", "demo"]);
    assert.notDeepEqual(overview.layout.profiles.desktop.positions, {
      demo: { x: 0, y: 0 },
      second: { x: 2, y: 0 },
    });
    await page.setViewportSize({ width: 600, height: 900 });
    await page.waitForFunction(() =>
      [...document.querySelectorAll("[data-overview-card]")]
        .map((card) => card.dataset.overviewCard)
        .join(",") === "second,demo",
    );
    assert.deepEqual(errors, []);
  }, {
    modules: [demoModule, second],
    localStorage: {
      "orqella.dashboards.v1": JSON.stringify({
        version: 1,
        dashboards: [{
          id: "overview",
          name: "Overview",
          protected: true,
          defaultVisible: true,
          layout,
          visible: {},
        }],
      }),
    },
    viewport: { width: 1600, height: 1000 },
  });
});

test("swap preview stays stable while the pointer remains over the same card", { skip: chromium ? false : "playwright is not installed" }, async () => {
  const second = { ...demoModule, id: "second", name: "Second" };
  await withPage(async (page, errors) => {
    await page.waitForSelector("[data-overview-card='second']");
    await page.click("#overviewEditToggle");
    await page.waitForSelector(".overview-editbar");
    const head = await page.locator("[data-overview-card='demo'] .overview-card-head").boundingBox();
    const target = await page.locator("[data-overview-card='second']").boundingBox();
    assert.ok(head && target);
    await page.mouse.move(head.x + head.width / 2, head.y + head.height / 2);
    await page.mouse.down();
    await page.mouse.move(target.x + target.width / 2, target.y + target.height / 2, { steps: 12 });
    await page.waitForFunction(() =>
      document.querySelector("[data-overview-card='second']")?.classList.contains("overview-swap-target"),
    );
    await page.evaluate(() => {
      window.__swapMutations = 0;
      const card = document.querySelector("[data-overview-card='second']");
      window.__swapObserver = new MutationObserver((records) => {
        window.__swapMutations += records.length;
      });
      window.__swapObserver.observe(card, {
        attributes: true,
        attributeFilter: ["class", "style"],
      });
    });
    const centerX = target.x + target.width / 2;
    const centerY = target.y + target.height / 2;
    for (const [dx, dy] of [[1, 0], [0, 1], [-1, 0], [0, -1], [1, 1], [-1, -1]]) {
      await page.mouse.move(centerX + dx, centerY + dy);
    }
    await page.waitForTimeout(120);
    const mutations = await page.evaluate(() => {
      window.__swapObserver?.disconnect();
      return window.__swapMutations;
    });
    await page.mouse.up();
    assert.equal(mutations, 0, "the swap preview should not reset on every pointermove");
    assert.deepEqual(errors, []);
  }, {
    modules: [demoModule, second],
    viewport: { width: 1600, height: 1000 },
  });
});

test("dragging a folded grid writes back into the authored columns", { skip: chromium ? false : "playwright is not installed" }, async () => {
  const second = { ...demoModule, id: "second", name: "Second" };
  const layout = {
    profiles: {
      desktop: {
        columns: 12,
        order: ["demo", "second"],
        sizes: { demo: { width: 6, height: 3 }, second: { width: 6, height: 3 } },
        positions: { demo: { x: 0, y: 0 }, second: { x: 6, y: 0 } },
        fullWidth: [],
      },
      mobile: { order: ["demo", "second"], sizes: {} },
    },
  };
  await withPage(async (page, errors) => {
    await page.waitForSelector("[data-overview-card='second']");
    assert.equal(await page.evaluate(() => Number(document.querySelector(".overview-grid").dataset.overviewColumns)), 6);
    await page.click("#overviewEditToggle");
    await page.waitForSelector(".overview-editbar");
    const head = await page.locator("[data-overview-card='demo'] .overview-card-head").boundingBox();
    const target = await page.locator("[data-overview-card='second']").boundingBox();
    assert.ok(head && target);
    await page.mouse.move(head.x + head.width / 2, head.y + head.height / 2);
    await page.mouse.down();
    await page.mouse.move(head.x + head.width / 2 + 8, head.y + head.height / 2, { steps: 2 });
    await page.mouse.move(target.x + target.width / 2, target.y + 20, { steps: 12 });
    await page.mouse.up();
    await page.waitForTimeout(400);
    const stored = await page.evaluate(() => JSON.parse(localStorage.getItem("orqella.dashboards.v1")));
    const desktop = stored.dashboards.find((dashboard) => dashboard.id === "overview").layout.profiles.desktop;
    for (const id of ["demo", "second"]) {
      const placement = desktop.positions[id];
      const width = desktop.sizes[id]?.width || 12;
      assert.ok(placement.x >= 0 && placement.x + width <= desktop.columns, `${id} overflows: ${JSON.stringify(placement)}`);
    }
    const [a, b] = [desktop.positions.demo, desktop.positions.second];
    assert.ok(a.x !== b.x || a.y !== b.y, "cards should not overlap after the folded drag");
    await page.setViewportSize({ width: 1920, height: 1080 });
    await page.waitForFunction(() => Number(document.querySelector(".overview-grid")?.dataset.overviewColumns) === 12);
    assert.deepEqual(errors, []);
  }, {
    modules: [demoModule, second],
    localStorage: {
      "orqella.dashboards.v1": JSON.stringify({
        version: 1,
        dashboards: [{
          id: "overview",
          name: "Overview",
          protected: true,
          defaultVisible: true,
          layout,
          visible: {},
        }],
      }),
    },
    viewport: { width: 1000, height: 900 },
  });
});

test("a new client adopts the daemon dashboard layout", { skip: chromium ? false : "playwright is not installed" }, async () => {
  const second = { ...demoModule, id: "second", name: "Second" };
  const server = staticServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const port = server.address().port;
  const origin = `http://127.0.0.1:${port}`;
  const browser = await chromium.launch({ headless: true, executablePath: process.env.CHROMIUM_PATH || "/usr/bin/chromium" });
  let store = {
    version: 2,
    revision: 0,
    initialized: false,
    dashboards: [],
    overview: { initialized: false, visible: {} },
    layouts: {},
    layoutsInitialized: false,
  };
  const routeApi = async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    if (path === "/api/v1/info") return route.fulfill({ json: info(origin) });
    if (path === "/api/v1/modules") return route.fulfill({ json: [demoModule, second] });
    if (path === "/api/v1/nodes") {
      return route.fulfill({ json: [{ id: "local", name: "test", online: true, local: true, latency_ms: 0 }] });
    }
    if (path === "/api/v1/peers") return route.fulfill({ json: [] });
    const layoutRoute = path.match(/^\/api\/v1\/dashboards\/([^/]+)\/layout\/(desktop|mobile)$/);
    if (layoutRoute && route.request().method() === "PUT") {
      const [, id, profile] = layoutRoute;
      const body = route.request().postDataJSON();
      store = {
        ...store,
        revision: store.revision + 1,
        initialized: true,
        layoutsInitialized: true,
        layouts: {
          ...store.layouts,
          [id]: { ...(store.layouts[id] || {}), [profile]: body?.layout },
        },
      };
      return route.fulfill({ json: store });
    }
    if (path === "/api/v1/dashboards") {
      if (route.request().method() === "PUT") {
        const body = route.request().postDataJSON();
        store = {
          ...store,
          revision: store.revision + 1,
          initialized: true,
          dashboards: Array.isArray(body?.dashboards) ? body.dashboards : [],
          overview: body?.overview
            ? { initialized: true, visible: body.overview.visible || {} }
            : store.overview,
        };
      }
      return route.fulfill({ json: store });
    }
    return route.fulfill({ json: {} });
  };
  try {
    const contextA = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
    const pageA = await contextA.newPage();
    await pageA.route("**/api/v1/**", routeApi);
    await pageA.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await pageA.waitForSelector("[data-overview-card='second']");
    await pageA.click("#overviewEditToggle");
    await pageA.waitForSelector(".overview-editbar");
    const head = await pageA.locator("[data-overview-card='demo'] .overview-card-head").boundingBox();
    const target = await pageA.locator("[data-overview-card='second']").boundingBox();
    assert.ok(head && target);
    await pageA.mouse.move(head.x + head.width / 2, head.y + head.height / 2);
    await pageA.mouse.down();
    await pageA.mouse.move(head.x + head.width / 2 + 8, head.y + head.height / 2, { steps: 2 });
    await pageA.mouse.move(target.x + 20, target.y + 20, { steps: 12 });
    await pageA.mouse.up();
    await pageA.waitForFunction(async () => {
      const response = await fetch("/api/v1/dashboards", { headers: { "X-Orqella": "1" }, cache: "no-store" });
      const data = await response.json();
      return Boolean(data.layouts?.overview?.desktop?.positions);
    });
    const snapshot = await pageA.evaluate(async () => {
      const response = await fetch("/api/v1/dashboards", { headers: { "X-Orqella": "1" }, cache: "no-store" });
      return response.json();
    });
    const desktop = snapshot.layouts.overview.desktop;
    assert.equal(desktop.order.length, 2);
    await contextA.close();

    const contextB = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
    const pageB = await contextB.newPage();
    await pageB.route("**/api/v1/**", routeApi);
    await pageB.goto(origin + "/", { waitUntil: "domcontentloaded" });
    await pageB.waitForSelector("[data-overview-card='second']");
    const order = await pageB.evaluate(() =>
      [...document.querySelectorAll("[data-overview-card]")].map((card) => card.dataset.overviewCard));
    const local = await pageB.evaluate(() => {
      const stored = JSON.parse(localStorage.getItem("orqella.dashboards.v1"));
      return stored.dashboards.find((dashboard) => dashboard.id === "overview").layout.profiles.desktop;
    });
    assert.deepEqual(order, desktop.order);
    assert.deepEqual(local.positions, desktop.positions);
    await contextB.close();
  } finally {
    await browser.close();
    await new Promise((resolve) => server.close(resolve));
  }
});

test("module page requests survive a late iframe load event", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    // Some WebViews fire the iframe load event after the page has announced
    // frame-ready. Reproduce that ordering and make sure the page's first
    // request is still accepted instead of leaving it on its boot screen.
    await page.evaluate(() => {
      window.addEventListener("message", (event) => {
        if (event.data?.type !== "orqella:frame-ready") return;
        const frame = document.getElementById("moduleFrame");
        if (event.source === frame.contentWindow) frame.dispatchEvent(new Event("load"));
      });
    });
    await page.click("[data-module='demo-page']");
    await page.waitForFunction(() => document.getElementById("moduleFrame")?.src.includes("/m/demo-page/"));
    const frame = page.frames().find((candidate) => new URL(candidate.url()).pathname === "/m/demo-page/");
    assert.ok(frame, "module page frame should exist");
    await frame.waitForFunction(() => document.getElementById("status")?.textContent === "Page ready", null, { timeout: 5000 });
    assert.deepEqual(errors, []);
  }, { modules: [demoModule, demoPage] });
});

test("returning to a launcher module reloads its page", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    const loads = [];
    page.on("request", (request) => {
      if (request.resourceType() === "document" && new URL(request.url()).pathname === "/m/demo-launcher/") {
        loads.push(request.url());
      }
    });
    await page.click("[data-module='demo-launcher']");
    await page.waitForFunction(() => document.getElementById("appSurface").hidden === false, null, { timeout: 5000 });
    await page.click("[data-view='home']");
    await page.waitForTimeout(200);
    await page.click("[data-module='demo-launcher']");
    await page.waitForFunction(() => document.getElementById("appSurface").hidden === false, null, { timeout: 5000 });
    assert.ok(loads.length >= 2, `launcher page should reload, saw ${loads.length} load(s)`);
    assert.deepEqual(errors, []);
  }, { modules: [demoModule, demoLauncher] });
});

test("leaving an app unloads its hidden frame", { skip: chromium ? false : "playwright is not installed" }, async () => {
  await withPage(async (page, errors) => {
    await page.click("[data-module='demo-launcher']");
    await page.waitForFunction(() => {
      const frame = document.getElementById("appFrame");
      return document.getElementById("appSurface").hidden === false && frame?.src && !frame.src.endsWith("about:blank");
    }, null, { timeout: 5000 });
    const loadedSrc = await page.evaluate(() => document.getElementById("appFrame").src);
    const openState = await page.evaluate(() => ({
      moduleSrc: document.getElementById("moduleFrame").src,
      appSrc: document.getElementById("appFrame").src,
    }));
    assert.match(openState.moduleSrc, /about:blank$/, `module page should be unloaded behind the app, still at ${openState.moduleSrc}`);
    await page.click("[data-view='home']");
    await page.waitForTimeout(250);
    const state = await page.evaluate(() => ({
      hidden: document.getElementById("appSurface").hidden,
      src: document.getElementById("appFrame").src,
      moduleSrc: document.getElementById("moduleFrame").src,
    }));
    assert.equal(state.hidden, true, "app surface should be hidden");
    assert.match(state.src, /about:blank$/, `hidden app frame should be unloaded, still at ${state.src} (was ${loadedSrc})`);
    assert.match(state.moduleSrc, /about:blank$/, `hidden module frame should be unloaded, still at ${state.moduleSrc}`);
    assert.deepEqual(errors, []);
  }, { modules: [demoModule, demoLauncher] });
});
