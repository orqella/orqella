import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../js/store.js", import.meta.url), "utf8");

function store() {
  const window = {};
  vm.runInNewContext(source, { window, JSON, console, Math });
  return window.OrqellaStore;
}

const plain = (value) => JSON.parse(JSON.stringify(value));

test("default dashboard layouts have independent desktop and mobile profiles", () => {
  const layout = plain(store().defaultDashboardLayout());
  assert.deepEqual(layout.profiles.desktop, {
    columns: 4,
    order: [],
    sizes: {},
    positions: {},
    fullWidth: [],
  });
  assert.deepEqual(layout.profiles.mobile, { order: [], sizes: {} });
});

test("legacy flat layouts become desktop layouts and seed the mobile order", () => {
  const layout = plain(store().normalizeDashboardLayout({
    columns: 6,
    order: ["alpha", "beta"],
    sizes: { alpha: { width: 3, height: 2 } },
    positions: { alpha: { x: 2, y: 1 } },
  }));
  assert.equal(layout.profiles.desktop.columns, 6);
  assert.deepEqual(layout.profiles.desktop.order, ["alpha", "beta"]);
  assert.deepEqual(layout.profiles.desktop.positions, { alpha: { x: 2, y: 1 } });
  assert.deepEqual(layout.profiles.mobile.order, ["alpha", "beta"]);
  assert.deepEqual(layout.profiles.mobile.sizes, {});
});

test("desktop and mobile orders stay independent once both exist", () => {
  const layout = plain(store().normalizeDashboardLayout({
    profiles: {
      desktop: { columns: 8, order: ["alpha", "beta"] },
      mobile: { order: ["beta", "alpha"], sizes: { beta: { height: 5 } } },
    },
  }));
  assert.deepEqual(layout.profiles.desktop.order, ["alpha", "beta"]);
  assert.deepEqual(layout.profiles.mobile.order, ["beta", "alpha"]);
  assert.deepEqual(layout.profiles.mobile.sizes, { beta: { height: 5 } });
});

test("a new mobile profile seeds once and keeps an explicitly empty order", () => {
  const seeded = plain(store().normalizeDashboardLayout({
    profiles: { desktop: { columns: 4, order: ["alpha"] } },
  }));
  assert.deepEqual(seeded.profiles.mobile.order, ["alpha"]);

  const explicit = plain(store().normalizeDashboardLayout({
    profiles: {
      desktop: { columns: 4, order: ["alpha"] },
      mobile: { order: [] },
    },
  }));
  assert.deepEqual(explicit.profiles.mobile.order, []);
});

test("wide screens keep their authored columns and narrow screens fold down", () => {
  const { resolveOverviewColumns } = store();
  assert.equal(resolveOverviewColumns(12, 1600), 12);
  assert.equal(resolveOverviewColumns(12, 800), 7);
  assert.equal(resolveOverviewColumns(4, 800), 4);
  assert.equal(resolveOverviewColumns(12, 300), 2);
  assert.equal(resolveOverviewColumns(12, 0), 12);
});

test("projected spans preserve proportions without overflowing", () => {
  const { projectOverviewSpan } = store();
  assert.equal(projectOverviewSpan(6, 12, 8), 4);
  assert.equal(projectOverviewSpan(3, 12, 8), 2);
  assert.equal(projectOverviewSpan(1, 12, 8), 1);
  assert.equal(projectOverviewSpan(12, 12, 5), 5);
  assert.equal(projectOverviewSpan(6, 12, 3, 0), 2);
});
