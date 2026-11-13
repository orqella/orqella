import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../js/migrations.js", import.meta.url), "utf8");

function migrations() {
  const window = {};
  vm.runInNewContext(source, { window, JSON, console });
  return window.OrqellaMigrations;
}

class MemoryStorage {
  constructor(initial = {}) {
    this.values = new Map(Object.entries(initial));
  }
  getItem(key) {
    return this.values.has(key) ? this.values.get(key) : null;
  }
  setItem(key, value) {
    this.values.set(key, String(value));
  }
  removeItem(key) {
    this.values.delete(key);
  }
}

test("legacy overview storage becomes an Overview dashboard and is deleted", () => {
  const storage = new MemoryStorage({
    "orqella.overview.visible": JSON.stringify({ system: true }),
    "orqella.overview.layout": JSON.stringify({ columns: 2 }),
  });
  assert.equal(
    migrations().migrateDashboardStorage(storage, { system: true }, { columns: 2 }),
    true,
  );
  const dashboards = JSON.parse(storage.getItem("orqella.dashboards.v1"));
  assert.equal(dashboards.dashboards.length, 1);
  assert.equal(dashboards.dashboards[0].id, "overview");
  assert.deepEqual(dashboards.dashboards[0].visible, { system: true });
  assert.deepEqual(dashboards.dashboards[0].layout, { columns: 2 });
  assert.equal(storage.getItem("orqella.overview.visible"), null);
  assert.equal(storage.getItem("orqella.overview.layout"), null);
  assert.equal(
    JSON.parse(storage.getItem("orqella.migrations.v1")).dashboardStorage,
    true,
  );
});

test("the migration marker prevents a second write", () => {
  const storage = new MemoryStorage({
    "orqella.overview.visible": JSON.stringify({ system: false }),
  });
  const migration = migrations();
  assert.equal(migration.migrateDashboardStorage(storage, { system: false }, {}), true);
  const first = storage.getItem("orqella.dashboards.v1");
  assert.equal(migration.migrateDashboardStorage(storage, { system: true }, { columns: 6 }), false);
  assert.equal(storage.getItem("orqella.dashboards.v1"), first);
});

test("an existing dashboards record wins and legacy keys are still removed", () => {
  const existing = JSON.stringify({
    version: 1,
    dashboards: [{ id: "overview", name: "Overview", visible: { terminal: true } }],
  });
  const storage = new MemoryStorage({
    "orqella.dashboards.v1": existing,
    "orqella.overview.visible": JSON.stringify({ system: true }),
  });
  assert.equal(migrations().migrateDashboardStorage(storage, { system: true }, {}), true);
  assert.equal(storage.getItem("orqella.dashboards.v1"), existing);
  assert.equal(storage.getItem("orqella.overview.visible"), null);
});
