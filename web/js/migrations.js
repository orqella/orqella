(function (global) {
  "use strict";

  const DASHBOARDS_KEY = "orqella.dashboards.v1";
  const MIGRATIONS_KEY = "orqella.migrations.v1";
  const OVERVIEW_DASHBOARD_ID = "overview";

  // One-time migration from the pre-dashboard `orqella.overview.*` keys.
  // Returns true when it wrote the dashboards record, false when the marker
  // already existed or storage is unavailable.
  function migrateDashboardStorage(storage, legacyVisible, legacyLayout) {
    try {
      const raw = storage.getItem(MIGRATIONS_KEY);
      const applied = raw ? JSON.parse(raw) || {} : {};
      if (applied.dashboardStorage) return false;
      let stored = null;
      try { stored = JSON.parse(storage.getItem(DASHBOARDS_KEY) || "null"); } catch {}
      if (!stored || !Array.isArray(stored.dashboards)) {
        storage.setItem(DASHBOARDS_KEY, JSON.stringify({
          version: 1,
          dashboards: [{
            id: OVERVIEW_DASHBOARD_ID,
            name: "Overview",
            protected: true,
            defaultVisible: true,
            layout: legacyLayout || {},
            visible: legacyVisible || {},
          }],
        }));
      }
      storage.removeItem("orqella.overview.visible");
      storage.removeItem("orqella.overview.layout");
      applied.dashboardStorage = true;
      storage.setItem(MIGRATIONS_KEY, JSON.stringify(applied));
      return true;
    } catch {
      return false;
    }
  }

  global.OrqellaMigrations = { migrateDashboardStorage };
})(typeof window !== "undefined" ? window : globalThis);
