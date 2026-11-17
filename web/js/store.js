(function (global) {
  "use strict";

  const OVERVIEW_DEFAULT_COLUMNS = 4;
  const OVERVIEW_MAX_COLUMNS = 12;
  const OVERVIEW_MAX_ROWS = 24;
  const OVERVIEW_ROW_UNIT = 90;
  const OVERVIEW_MIN_COLUMN_WIDTH = 96;
  const OVERVIEW_MOBILE_QUERY = "(max-width: 680px)";
  const OVERVIEW_DASHBOARD_ID = "overview";
  const DASHBOARDS_KEY = "orqella.dashboards.v1";
  const DASHBOARDS_SHARED_KEY = "orqella.dashboards.shared.v1";
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

  function defaultDesktopLayout() {
    return { columns: OVERVIEW_DEFAULT_COLUMNS, order: [], sizes: {}, positions: {}, fullWidth: [] };
  }

  function defaultMobileLayout(seedOrder = []) {
    return { order: Array.isArray(seedOrder) ? [...seedOrder] : [], sizes: {} };
  }

  function defaultDashboardLayout() {
    return { profiles: { desktop: defaultDesktopLayout(), mobile: defaultMobileLayout() } };
  }

  function normalizeOrder(value) {
    return Array.isArray(value) ? value.filter((id) => typeof id === "string") : [];
  }

  function normalizeObject(value) {
    return value && typeof value === "object" && !Array.isArray(value) ? value : {};
  }

  function normalizeDesktopLayout(value) {
    return {
      columns: clampInt(value?.columns, 1, OVERVIEW_MAX_COLUMNS, OVERVIEW_DEFAULT_COLUMNS),
      order: normalizeOrder(value?.order),
      sizes: normalizeObject(value?.sizes),
      positions: normalizeObject(value?.positions),
      fullWidth: normalizeOrder(value?.fullWidth),
    };
  }

  function normalizeMobileLayout(value, seedOrder = []) {
    const source = value && typeof value === "object" && !Array.isArray(value) ? value : null;
    // The mobile profile is deliberately independent after migration. Seed
    // its order from desktop only when the profile does not exist yet.
    const order = source && Array.isArray(source.order) ? normalizeOrder(source.order) : [...seedOrder];
    return { order, sizes: normalizeObject(source?.sizes) };
  }

  function normalizeDashboardLayout(value) {
    const profiles = value?.profiles;
    if (profiles && typeof profiles === "object" && !Array.isArray(profiles)) {
      const desktop = normalizeDesktopLayout(profiles.desktop);
      return {
        profiles: {
          desktop,
          mobile: normalizeMobileLayout(profiles.mobile, desktop.order),
        },
      };
    }
    if (value && typeof value === "object" && !Array.isArray(value)
      && ("columns" in value || "order" in value || "positions" in value
        || "sizes" in value || "fullWidth" in value)) {
      // Pre-profile layouts were the desktop layout. Copy their order once
      // into a new mobile profile so existing phones keep their card order.
      const desktop = normalizeDesktopLayout(value);
      return {
        profiles: {
          desktop,
          mobile: normalizeMobileLayout(null, desktop.order),
        },
      };
    }
    return defaultDashboardLayout();
  }

  function overviewProfile(layout, compact) {
    return compact ? layout?.profiles?.mobile : layout?.profiles?.desktop;
  }

  function resolveOverviewColumns(authored, width, gap = 13, minColumnWidth = OVERVIEW_MIN_COLUMN_WIDTH) {
    const columns = clampInt(authored, 1, OVERVIEW_MAX_COLUMNS, OVERVIEW_DEFAULT_COLUMNS);
    const available = Number(width);
    if (!Number.isFinite(available) || available <= 0) return columns;
    const spacing = Number.isFinite(Number(gap)) ? Math.max(0, Number(gap)) : 13;
    const minimum = Math.max(1, Number(minColumnWidth) || OVERVIEW_MIN_COLUMN_WIDTH);
    const fit = Math.floor((available + spacing) / (minimum + spacing));
    return Math.max(1, Math.min(columns, fit));
  }

  function projectOverviewSpan(value, authored, rendered, min = 1) {
    const from = Math.max(1, Number(authored) || 1);
    const to = Math.max(1, Number(rendered) || 1);
    return clampInt(Math.round((Number(value) || 0) * to / from), min, to, min);
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

  // Custom dashboard definitions are shared by every client of this daemon.
  // Geometry is daemon-owned too, but travels through the per-profile layout
  // endpoint so a phone write cannot replace the desktop profile.
  function sharedDashboards(dashboards) {
    return (Array.isArray(dashboards) ? dashboards : [])
      .filter((dashboard) => dashboard && typeof dashboard.id === "string" && dashboard.id && dashboard.id !== OVERVIEW_DASHBOARD_ID)
      .map((dashboard) => ({
        id: dashboard.id,
        name: String(dashboard.name || "").trim().slice(0, 60),
        visible: normalizeDashboardVisible(dashboard.visible),
      }));
  }

  function sharedDashboardsMigrated() {
    try { return storage().getItem(DASHBOARDS_SHARED_KEY) === "1"; } catch { return false; }
  }

  function markSharedDashboardsMigrated() {
    try { storage().setItem(DASHBOARDS_SHARED_KEY, "1"); } catch {}
  }

  function mergeSharedDashboards(local, shared) {
    const localList = Array.isArray(local) ? local : [];
    const overview = localList.find((dashboard) => dashboard?.id === OVERVIEW_DASHBOARD_ID)
      || {
        id: OVERVIEW_DASHBOARD_ID,
        name: "Overview",
        protected: true,
        defaultVisible: true,
        layout: loadOverviewLayout(),
        visible: loadOverviewPrefs(),
      };
    const localById = new Map(localList.filter((dashboard) => dashboard?.id).map((dashboard) => [dashboard.id, dashboard]));
    const seen = new Set([OVERVIEW_DASHBOARD_ID]);
    const custom = [];
    for (const entry of Array.isArray(shared) ? shared : []) {
      if (!entry || typeof entry.id !== "string" || !entry.id || seen.has(entry.id)) continue;
      seen.add(entry.id);
      const name = typeof entry.name === "string" && entry.name.trim()
        ? entry.name.trim().slice(0, 60)
        : `Dashboard ${custom.length + 1}`;
      custom.push({
        id: entry.id,
        name,
        protected: false,
        defaultVisible: false,
        layout: normalizeDashboardLayout(localById.get(entry.id)?.layout),
        visible: normalizeDashboardVisible(entry.visible),
      });
    }
    return [{ ...overview, protected: true, defaultVisible: true }, ...custom];
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
    OVERVIEW_MIN_COLUMN_WIDTH,
    OVERVIEW_MOBILE_QUERY,
    OVERVIEW_DASHBOARD_ID,
    DASHBOARDS_KEY,
    VIEW_KEY,
    clampInt,
    loadOverviewPrefs,
    loadPagePrefs,
    loadPageOrder,
    loadOverviewLayout,
    defaultDesktopLayout,
    defaultMobileLayout,
    defaultDashboardLayout,
    normalizeDesktopLayout,
    normalizeMobileLayout,
    normalizeDashboardLayout,
    normalizeDashboardVisible,
    overviewProfile,
    resolveOverviewColumns,
    projectOverviewSpan,
    dashboardPageKey,
    dashboardView,
    dashboardIdFromView,
    loadViewState,
    saveViewState,
    newDashboardId,
    loadDashboards,
    saveDashboards,
    sharedDashboards,
    sharedDashboardsMigrated,
    markSharedDashboardsMigrated,
    mergeSharedDashboards,
    savePageOrder,
    savePagePrefs,
  };
})(typeof window !== "undefined" ? window : globalThis);
