(function (global) {
  "use strict";

  const OVERVIEW_DEFAULT_COLUMNS = 4;
  const OVERVIEW_MAX_COLUMNS = 12;
  const OVERVIEW_MAX_ROWS = 24;
  const OVERVIEW_ROW_UNIT = 90;
  const OVERVIEW_DASHBOARD_ID = "overview";
  const DASHBOARDS_KEY = "orqella.dashboards.v1";
  const VIEW_KEY = "orqella.view.v1";

  function storage() {
    return global.localStorage;
  }

  function loadOverviewPrefs() {
    try { return JSON.parse(storage().getItem("orqella.overview.visible") || "{}") || {}; }
    catch { return {}; }
  }

  function loadPagePrefs() {
    try { return JSON.parse(storage().getItem("orqella.page.visible") || "{}") || {}; }
    catch { return {}; }
  }

  function loadPageOrder() {
    try {
      const value = JSON.parse(storage().getItem("orqella.page.order") || "[]");
      return Array.isArray(value) ? value.filter((key) => typeof key === "string") : [];
    } catch { return []; }
  }

  function clampInt(value, min, max, fallback) {
    const number = Number(value);
    if (!Number.isFinite(number)) return fallback;
    return Math.max(min, Math.min(max, Math.round(number)));
  }

  function defaultDashboardLayout() {
    return { columns: OVERVIEW_DEFAULT_COLUMNS, order: [], sizes: {}, positions: {}, fullWidth: [] };
  }

  function normalizeDashboardLayout(value) {
    return {
      columns: clampInt(value?.columns, 1, OVERVIEW_MAX_COLUMNS, OVERVIEW_DEFAULT_COLUMNS),
      order: Array.isArray(value?.order) ? value.order.filter((id) => typeof id === "string") : [],
      sizes: value?.sizes && typeof value.sizes === "object" && !Array.isArray(value.sizes) ? value.sizes : {},
      positions: value?.positions && typeof value.positions === "object" && !Array.isArray(value.positions) ? value.positions : {},
      fullWidth: Array.isArray(value?.fullWidth) ? value.fullWidth.filter((id) => typeof id === "string") : [],
    };
  }

  function loadOverviewLayout() {
    try {
      return normalizeDashboardLayout(JSON.parse(storage().getItem("orqella.overview.layout") || "{}"));
    } catch {
      return defaultDashboardLayout();
    }
  }

  function normalizeDashboardVisible(value) {
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    return Object.fromEntries(Object.entries(value).filter(([, visible]) => typeof visible === "boolean"));
  }

  function dashboardPageKey(id) {
    return `dashboard:${id}`;
  }

  function dashboardView(id) {
    return id === OVERVIEW_DASHBOARD_ID ? "home" : dashboardPageKey(id);
  }

  function dashboardIdFromView(view) {
    if (view === "home") return OVERVIEW_DASHBOARD_ID;
    return view.startsWith("dashboard:") ? view.slice("dashboard:".length) : null;
  }

  function loadViewState() {
    const fallback = { view: "home", module: null, modulePath: "" };
    try {
      const stored = JSON.parse(storage().getItem(VIEW_KEY) || "null");
      if (!stored || typeof stored.view !== "string") return fallback;
      const view = stored.view;
      const valid = view === "home"
        || view === "build"
        || view === "settings"
        || view.startsWith("dashboard:")
        || (view.startsWith("module:") && view.length > "module:".length);
      if (!valid) return fallback;
      const module = view.startsWith("module:") ? view.slice("module:".length) : null;
      return {
        view,
        module,
        modulePath: module && typeof stored.modulePath === "string" ? stored.modulePath : "",
      };
    } catch {
      return fallback;
    }
  }

  function saveViewState(state) {
    try {
      storage().setItem(VIEW_KEY, JSON.stringify({
        view: state.view,
        modulePath: state.modulePath || "",
      }));
    } catch {}
  }

  function newDashboardId() {
    return `dashboard-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
  }

  function loadDashboards() {
    try {
      const stored = JSON.parse(storage().getItem(DASHBOARDS_KEY) || "null");
      if (stored && Array.isArray(stored.dashboards)) {
        const seen = new Set();
        const dashboards = stored.dashboards.flatMap((dashboard, index) => {
          if (!dashboard || typeof dashboard.id !== "string" || !dashboard.id || seen.has(dashboard.id)) return [];
          seen.add(dashboard.id);
          const isOverview = dashboard.id === OVERVIEW_DASHBOARD_ID;
          return [{
            id: dashboard.id,
            name: typeof dashboard.name === "string" && dashboard.name.trim()
              ? dashboard.name.trim().slice(0, 60)
              : `Dashboard ${index + 1}`,
            protected: isOverview || dashboard.protected === true,
            defaultVisible: isOverview,
            layout: normalizeDashboardLayout(dashboard.layout),
            visible: normalizeDashboardVisible(dashboard.visible),
          }];
        });
        const overview = dashboards.find((dashboard) => dashboard.id === OVERVIEW_DASHBOARD_ID);
        if (overview) {
          overview.protected = true;
          overview.defaultVisible = true;
          return dashboards;
        }
        dashboards.unshift({
          id: OVERVIEW_DASHBOARD_ID,
          name: "Overview",
          protected: true,
          defaultVisible: true,
          layout: loadOverviewLayout(),
          visible: loadOverviewPrefs(),
        });
        return dashboards;
      }
    } catch {}
    return [{
      id: OVERVIEW_DASHBOARD_ID,
      name: "Overview",
      protected: true,
      defaultVisible: true,
      layout: loadOverviewLayout(),
      visible: loadOverviewPrefs(),
    }];
  }

  function saveDashboards(state) {
    try {
      storage().setItem(DASHBOARDS_KEY, JSON.stringify({ version: 1, dashboards: state.dashboards }));
    } catch {}
  }

  function savePageOrder(state) {
    try { storage().setItem("orqella.page.order", JSON.stringify(state.pageOrder)); } catch {}
  }

  function savePagePrefs(state) {
    try { storage().setItem("orqella.page.visible", JSON.stringify(state.pagePrefs)); } catch {}
  }

  global.OrqellaStore = {
    OVERVIEW_DEFAULT_COLUMNS,
    OVERVIEW_MAX_COLUMNS,
    OVERVIEW_MAX_ROWS,
    OVERVIEW_ROW_UNIT,
    OVERVIEW_DASHBOARD_ID,
    DASHBOARDS_KEY,
    VIEW_KEY,
    clampInt,
    loadOverviewPrefs,
    loadPagePrefs,
    loadPageOrder,
    loadOverviewLayout,
    defaultDashboardLayout,
    normalizeDashboardLayout,
    normalizeDashboardVisible,
    dashboardPageKey,
    dashboardView,
    dashboardIdFromView,
    loadViewState,
    saveViewState,
    newDashboardId,
    loadDashboards,
    saveDashboards,
    savePageOrder,
    savePagePrefs,
  };
})(typeof window !== "undefined" ? window : globalThis);
