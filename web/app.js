(() => {
  "use strict";

  const $ = (id) => document.getElementById(id);
  const esc = (value) => String(value ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
  // The browser transport assigns one id per top-level tab. Nested frames
  // carry it so the service worker routes their requests to the right tab.
  const transportParam = () => {
    const value = new URLSearchParams(location.search).get("__orqella_transport");
    return value ? `__orqella_transport=${encodeURIComponent(value)}` : "";
  };
  let sessionReady = false;

  async function bootstrapSession() {
    const params = new URLSearchParams(location.hash.replace(/^#/, ""));
    const token = params.get("orqella_bootstrap") || "";
    if (!token) return false;
    try {
      const response = await fetch("/__orqella/bootstrap", {
        method: "POST",
        cache: "no-store",
        headers: { "X-Orqella-Bootstrap": token },
      });
      if (!response.ok) return false;
      history.replaceState(null, "", location.pathname + location.search);
      sessionReady = true;
      window.__orqellaSessionReady = true;
      return true;
    } catch (_) {
      return false;
    }
  }

  function applyViewportHeight() {
    const height = Math.round(window.visualViewport?.height || window.innerHeight || 0);
    const offsetTop = Math.max(0, Math.round(window.visualViewport?.offsetTop || 0));
    if (height > 0) {
      document.documentElement.style.setProperty("--app-height", `${height}px`);
    }
    document.documentElement.style.setProperty("--app-offset", `${offsetTop}px`);
  }
  applyViewportHeight();
  window.addEventListener("resize", applyViewportHeight);
  window.visualViewport?.addEventListener("resize", applyViewportHeight);
  window.visualViewport?.addEventListener("scroll", applyViewportHeight);

  const Store = window.OrqellaStore;
  const {
    OVERVIEW_DEFAULT_COLUMNS,
    OVERVIEW_MAX_COLUMNS,
    OVERVIEW_MAX_ROWS,
    OVERVIEW_ROW_UNIT,
    OVERVIEW_MIN_COLUMN_WIDTH,
    OVERVIEW_MOBILE_QUERY,
    OVERVIEW_DASHBOARD_ID,
    clampInt,
    defaultDesktopLayout,
    defaultMobileLayout,
    defaultDashboardLayout,
    loadOverviewPrefs,
    loadPagePrefs,
    loadPageOrder,
    loadOverviewLayout,
    normalizeDesktopLayout,
    normalizeMobileLayout,
    normalizeDashboardVisible,
    overviewProfile,
    resolveOverviewColumns,
    projectOverviewSpan,
    dashboardPageKey,
    dashboardView,
    dashboardIdFromView,
    loadViewState,
    newDashboardId,
    loadDashboards,
    sharedDashboards,
    sharedDashboardsMigrated,
    markSharedDashboardsMigrated,
    mergeSharedDashboards,
  } = Store;

  // The migration lives in js/migrations.js so it can be unit tested.
  if (window.OrqellaMigrations) {
    window.OrqellaMigrations.migrateDashboardStorage(
      localStorage,
      loadOverviewPrefs(),
      loadOverviewLayout(),
    );
  }

  const initialDashboards = loadDashboards();
  const initialView = loadViewState();
  const initialViewDashboardId = dashboardIdFromView(initialView.view);
  const initialDashboard = (initialViewDashboardId && initialDashboards.find((dashboard) => dashboard.id === initialViewDashboardId))
    || initialDashboards.find((dashboard) => dashboard.id === OVERVIEW_DASHBOARD_ID)
    || initialDashboards[0];

  const state = {
    info: null,
    modules: [],
    localModules: [],
    modulesError: null,
    nodes: [],
    peers: [],
    node: localStorage.getItem("orqella.node") || "local",
    view: initialView.view,
    module: initialView.module,
    modulePath: initialView.modulePath,
    jobs: [],
    buildEditModule: null,
    buildPollTimer: null,
    app: null,
    dashboards: initialDashboards,
    dashboardId: initialDashboard.id,
    overviewPrefs: initialDashboard.visible,
    pagePrefs: loadPagePrefs(),
    pageOrder: loadPageOrder(),
    overviewLayout: initialDashboard.layout,
    overviewEditing: false,
    overviewFrames: new Map(),
    settingsFrames: new Map(),
    readyFrames: new Set(),
    framePorts: new Map(),
    frameMessage: null,
    recordings: new Map(),
    settingsTab: "general",
    settingsModule: null,
    agentRegistry: null,
    agentRegistryHandler: null,
    agentRegistryTimer: null,
    wake: null,
    railCollapsed: localStorage.getItem("orqella.rail.collapsed") === "1",
  };

  // Custom dashboard definitions and card visibility live in the daemon so
  // all clients see them. Layouts are daemon-owned too, split into independent
  // desktop and mobile profiles so one device never rewrites the other.
  let dashboardsRevision = 0;
  let dashboardsLoaded = false;
  let dashboardsPushInFlight = false;
  let dashboardsPushAgain = false;
  let layoutsRevision = 0;
  let layoutsInitialized = false;
  let layoutsLoaded = false;
  let layoutSeedPending = false;
  let overviewSeedPending = false;
  let layoutPushRunning = false;
  const pendingLayoutPushes = new Map();

  function saveViewState() {
    Store.saveViewState(state);
  }

  function saveDashboards() {
    Store.saveDashboards(state);
  }

  function savePageOrder() {
    Store.savePageOrder(state);
  }

  function savePagePrefs() {
    Store.savePagePrefs(state);
  }

  function overviewDashboard() {
    return state.dashboards.find((dashboard) => dashboard.id === OVERVIEW_DASHBOARD_ID);
  }

  function overviewVisibility() {
    return overviewDashboard()?.visible || {};
  }

  function saveSharedDashboards() {
    saveDashboards();
    void pushSharedDashboards();
  }

  async function putSharedDashboards(dashboards, { quiet = false, migrate = false } = {}) {
    if (dashboardsPushInFlight) {
      dashboardsPushAgain = true;
      return false;
    }
    dashboardsPushInFlight = true;
    try {
      const visible = overviewVisibility();
      const payload = { dashboards };
      // An empty map means "no overview overrides yet". Leaving the field out
      // lets a client that actually customized visibility seed it later.
      if (overviewSeedPending || Object.keys(visible).length) {
        payload.overview = { visible };
      }
      const saved = await api("/api/v1/dashboards", {
        method: "PUT",
        body: JSON.stringify(payload),
      });
      dashboardsLoaded = true;
      const revision = Number(saved?.revision);
      if (Number.isFinite(revision)) {
        dashboardsRevision = revision;
        layoutsRevision = revision;
      }
      overviewSeedPending = false;
      if (migrate) markSharedDashboardsMigrated();
      return true;
    } catch (error) {
      if (!quiet) toast("Could not sync dashboards", error.message, "bad");
      return false;
    } finally {
      dashboardsPushInFlight = false;
      if (dashboardsPushAgain) {
        dashboardsPushAgain = false;
        void pushSharedDashboards();
      } else if (pendingLayoutPushes.size) {
        void flushDashboardLayoutPushes();
      }
    }
  }

  async function pushSharedDashboards({ quiet = false } = {}) {
    return putSharedDashboards(sharedDashboards(state.dashboards), { quiet });
  }

  function dashboardLayout(id, profile) {
    const dashboard = state.dashboards.find((entry) => entry.id === id);
    if (!dashboard) return null;
    return profile === "mobile" ? dashboard.layout.profiles.mobile : dashboard.layout.profiles.desktop;
  }

  function queueDashboardLayoutPush(id, profile, { quiet = false } = {}) {
    if (!layoutsLoaded || (profile !== "desktop" && profile !== "mobile")) return;
    if (!dashboardLayout(id, profile)) return;
    const key = `${id}\u0000${profile}`;
    const previous = pendingLayoutPushes.get(key);
    pendingLayoutPushes.set(key, {
      id,
      profile,
      quiet: previous ? previous.quiet && quiet : quiet,
    });
    if (!dashboardsPushInFlight && !dashboardsPushAgain) void flushDashboardLayoutPushes();
  }

  async function flushDashboardLayoutPushes() {
    if (layoutPushRunning) return;
    layoutPushRunning = true;
    try {
      while (pendingLayoutPushes.size) {
        const [key, entry] = pendingLayoutPushes.entries().next().value;
        pendingLayoutPushes.delete(key);
        const layout = dashboardLayout(entry.id, entry.profile);
        if (!layout) continue;
        try {
          const saved = await api(
            `/api/v1/dashboards/${encodeURIComponent(entry.id)}/layout/${encodeURIComponent(entry.profile)}`,
            { method: "PUT", body: JSON.stringify({ layout }) },
          );
          const revision = Number(saved?.revision);
          if (Number.isFinite(revision)) {
            layoutsRevision = revision;
            // A layout write does not change dashboard definitions, so keep
            // the next poll from re-rendering every card for a revision bump.
            dashboardsRevision = revision;
          }
          layoutsInitialized = true;
        } catch (error) {
          if (!entry.quiet) toast("Could not sync layout", error.message, "bad");
          if (!layoutsInitialized) layoutSeedPending = false;
        }
      }
    } finally {
      layoutPushRunning = false;
      if (pendingLayoutPushes.size) void flushDashboardLayoutPushes();
      else layoutSeedPending = false;
    }
  }

  function saveDashboardLayout(id, profile) {
    queueDashboardLayoutPush(id, profile);
  }

  function dashboardLayoutsChanged(layouts, overview) {
    if (!layouts || typeof layouts !== "object" || Array.isArray(layouts)) return false;
    for (const dashboard of state.dashboards) {
      const entry = layouts[dashboard.id];
      if (!entry || typeof entry !== "object" || Array.isArray(entry)) continue;
      if (entry.desktop
        && stableJson(normalizeDesktopLayout(entry.desktop)) !== stableJson(dashboard.layout.profiles.desktop)) {
        return true;
      }
      if (entry.mobile
        && stableJson(normalizeMobileLayout(entry.mobile, dashboard.layout.profiles.desktop.order)) !== stableJson(dashboard.layout.profiles.mobile)) {
        return true;
      }
    }
    if (overview?.initialized) {
      const visible = normalizeDashboardVisible(overview.visible);
      if (stableJson(visible) !== stableJson(overviewDashboard()?.visible)) return true;
    }
    return false;
  }

  function applyDashboardLayouts(layouts, overview) {
    if (!layouts || typeof layouts !== "object" || Array.isArray(layouts)) return false;
    let changed = false;
    for (const dashboard of state.dashboards) {
      const entry = layouts[dashboard.id];
      if (!entry || typeof entry !== "object" || Array.isArray(entry)) continue;
      if (entry.desktop) {
        const next = normalizeDesktopLayout(entry.desktop);
        if (stableJson(next) !== stableJson(dashboard.layout.profiles.desktop)) {
          dashboard.layout.profiles.desktop = next;
          changed = true;
        }
      }
      if (entry.mobile) {
        const next = normalizeMobileLayout(entry.mobile, dashboard.layout.profiles.desktop.order);
        if (stableJson(next) !== stableJson(dashboard.layout.profiles.mobile)) {
          dashboard.layout.profiles.mobile = next;
          changed = true;
        }
      }
    }
    if (overview?.initialized) {
      const dashboard = overviewDashboard();
      if (dashboard) {
        const visible = normalizeDashboardVisible(overview.visible);
        if (stableJson(visible) !== stableJson(dashboard.visible)) {
          dashboard.visible = visible;
          changed = true;
        }
      }
    }
    return changed;
  }

  function stableJson(value) {
    if (Array.isArray(value)) return `[${value.map((entry) => stableJson(entry)).join(",")}]`;
    if (value && typeof value === "object") {
      return `{${Object.keys(value)
        .sort()
        .map((key) => `${JSON.stringify(key)}:${stableJson(value[key])}`)
        .join(",")}}`;
    }
    return JSON.stringify(value);
  }

  function sharedDashboardsChanged(server) {
    return stableJson(sharedDashboards(state.dashboards)) !== stableJson(server);
  }

  function seedDashboardState(remote) {
    if (!remote.layoutsInitialized && !layoutSeedPending) {
      layoutSeedPending = true;
      for (const dashboard of state.dashboards) {
        queueDashboardLayoutPush(dashboard.id, "desktop", { quiet: true });
        queueDashboardLayoutPush(dashboard.id, "mobile", { quiet: true });
      }
    }
    if (remote.initialized
      && !remote.overview?.initialized
      && !overviewSeedPending
      && Object.keys(overviewVisibility()).length) {
      overviewSeedPending = true;
      void putSharedDashboards(sharedDashboards(state.dashboards), { quiet: true })
        .then((saved) => { if (!saved) overviewSeedPending = false; });
    }
  }

  async function loadSharedDashboards({ rerender = true, quiet = true } = {}) {
    if (dashboardsPushInFlight || dashboardsPushAgain) return false;
    let remote;
    try {
      remote = await api("/api/v1/dashboards", { cache: "no-store" });
    } catch (error) {
      if (!quiet) toast("Could not load dashboards", error.message, "bad");
      return false;
    }
    if (!remote || typeof remote !== "object") return false;
    layoutsLoaded = true;
    if (state.overviewEditing) return false;
    const migrate = !sharedDashboardsMigrated();
    if (!remote.initialized) {
      dashboardsLoaded = true;
      dashboardsRevision = 0;
      layoutsRevision = 0;
      layoutsInitialized = false;
      const local = sharedDashboards(state.dashboards);
      if (local.length) {
        const saved = await putSharedDashboards(local, { quiet, migrate: true });
        if (saved) {
          seedDashboardState(remote);
          return true;
        }
        return false;
      }
      markSharedDashboardsMigrated();
      seedDashboardState(remote);
      return false;
    }
    const server = Array.isArray(remote.dashboards) ? remote.dashboards : [];
    if (migrate) {
      // One-time upgrade path: keep dashboards that only exist in this client's
      // pre-sync local storage. Server entries win when an id or name collides.
      const local = sharedDashboards(state.dashboards);
      const serverIds = new Set(server.map((dashboard) => dashboard.id));
      const serverNames = new Set(server.map((dashboard) => String(dashboard.name || "").toLocaleLowerCase()));
      const additions = local.filter((dashboard) => !serverIds.has(dashboard.id)
        && !serverNames.has(String(dashboard.name || "").toLocaleLowerCase()));
      if (additions.length) {
        const merged = [...server, ...additions];
        const saved = await putSharedDashboards(merged, { quiet, migrate: true });
        if (!saved) return false;
        layoutsInitialized = remote.layoutsInitialized === true;
        applySharedDashboards(merged, {
          rerender,
          layouts: remote.layouts,
          overview: remote.overview,
        });
        layoutsRevision = dashboardsRevision;
        seedDashboardState(remote);
        return true;
      }
      markSharedDashboardsMigrated();
    }
    const revision = Number(remote.revision) || 0;
    layoutsInitialized = remote.layoutsInitialized === true;
    const localLayoutPending = layoutPushRunning || pendingLayoutPushes.size > 0;
    // A layout write bumps the shared revision, so our own push can hide a
    // profile another device saved just before it. Compare the actual payloads
    // as well as the revision instead of trusting the counter alone.
    const remoteLayoutsChanged = dashboardLayoutsChanged(remote.layouts, remote.overview);
    const applyLayouts = !state.overviewEditing
      && !localLayoutPending
      && (revision !== layoutsRevision || remoteLayoutsChanged);
    const refreshDefinitions = !dashboardsLoaded
      || revision !== dashboardsRevision
      || sharedDashboardsChanged(server);
    if (!applyLayouts && !refreshDefinitions) {
      seedDashboardState(remote);
      return false;
    }
    dashboardsRevision = revision;
    dashboardsLoaded = true;
    if (applyLayouts) layoutsRevision = revision;
    applySharedDashboards(server, {
      rerender: rerender && !(state.overviewEditing && dashboardIdFromView(state.view)),
      layouts: applyLayouts ? remote.layouts : null,
      overview: applyLayouts ? remote.overview : null,
    });
    if (applyLayouts) saveDashboards();
    seedDashboardState(remote);
    return true;
  }

  function mergeSharedDashboardOrder(shared) {
    const keys = (Array.isArray(shared) ? shared : [])
      .filter((dashboard) => dashboard && typeof dashboard.id === "string" && dashboard.id && dashboard.id !== OVERVIEW_DASHBOARD_ID)
      .map((dashboard) => dashboardPageKey(dashboard.id));
    const wanted = new Set(keys);
    const next = state.pageOrder.filter((key) => !key.startsWith("dashboard:") || wanted.has(key));
    const slots = [];
    next.forEach((key, index) => {
      if (key.startsWith("dashboard:")) slots.push(index);
    });
    // Fill the existing dashboard slots in daemon order, then insert any new
    // dashboards after the last slot so their relative order is preserved.
    const assigned = keys.slice(0, slots.length);
    slots.forEach((slot, index) => {
      if (assigned[index]) next[slot] = assigned[index];
    });
    let insertAt = slots.length ? slots[slots.length - 1] + 1 : next.length;
    for (const key of keys.slice(slots.length)) {
      next.splice(insertAt, 0, key);
      insertAt += 1;
    }
    if (next.join("\u0000") !== state.pageOrder.join("\u0000")) {
      state.pageOrder = next;
      savePageOrder();
    }
  }

  function applySharedDashboards(shared, { rerender = true, layouts = null, overview = null } = {}) {
    state.dashboards = mergeSharedDashboards(state.dashboards, shared);
    applyDashboardLayouts(layouts, overview);
    mergeSharedDashboardOrder(shared);
    const viewDashboardId = dashboardIdFromView(state.view);
    if (viewDashboardId && state.dashboards.some((dashboard) => dashboard.id === viewDashboardId)) {
      activateDashboard(viewDashboardId);
    } else if (viewDashboardId) {
      state.view = "home";
      state.module = null;
      state.modulePath = "";
      activateDashboard(OVERVIEW_DASHBOARD_ID);
      saveViewState();
    } else if (!state.dashboards.some((dashboard) => dashboard.id === state.dashboardId)) {
      state.dashboardId = OVERVIEW_DASHBOARD_ID;
      activateDashboard(OVERVIEW_DASHBOARD_ID);
    } else {
      activateDashboard(state.dashboardId);
    }
    ensureDashboardPageOrder();
    saveDashboards();
    if (rerender) {
      renderNav();
      if (dashboardIdFromView(state.view)) renderHome();
      else if (state.view === "settings" && state.settingsTab === "dashboards") renderSettingsDashboards();
    }
  }

  function saveSharedDashboardOrder() {
    if (!dashboardsLoaded) return;
    const rank = new Map(state.pageOrder.map((key, index) => [key, index]));
    const overview = state.dashboards.find((dashboard) => dashboard.id === OVERVIEW_DASHBOARD_ID);
    const custom = state.dashboards
      .filter((dashboard) => dashboard.id !== OVERVIEW_DASHBOARD_ID)
      .map((dashboard, index) => ({
        dashboard,
        index,
        rank: rank.has(dashboardPageKey(dashboard.id)) ? rank.get(dashboardPageKey(dashboard.id)) : Number.POSITIVE_INFINITY,
      }));
    const before = custom.map((entry) => entry.dashboard.id).join("\u0000");
    custom.sort((a, b) => a.rank - b.rank || a.index - b.index);
    state.dashboards = overview
      ? [overview, ...custom.map((entry) => entry.dashboard)]
      : custom.map((entry) => entry.dashboard);
    if (before !== custom.map((entry) => entry.dashboard.id).join("\u0000")) {
      saveSharedDashboards();
    }
  }

  function activeDashboard() {
    return state.dashboards.find((dashboard) => dashboard.id === state.dashboardId)
      || state.dashboards[0];
  }

  function validateViewState() {
    const dashboardId = dashboardIdFromView(state.view);
    if (dashboardId && !state.dashboards.some((dashboard) => dashboard.id === dashboardId)) {
      state.view = "home";
      state.module = null;
      state.modulePath = "";
      activateDashboard(OVERVIEW_DASHBOARD_ID);
      return;
    }
    if (state.view.startsWith("module:")) {
      const id = state.view.slice("module:".length);
      const module = state.modules.find((entry) => entry.id === id && entry.has_page && entry.enabled);
      if (!module) {
        state.view = "home";
        state.module = null;
        state.modulePath = "";
        return;
      }
      state.module = id;
    }
  }

  function ensureDashboardPageOrder() {
    const missing = state.dashboards
      .filter((dashboard) => dashboard.id !== OVERVIEW_DASHBOARD_ID)
      .map((dashboard) => dashboardPageKey(dashboard.id))
      .filter((key) => !state.pageOrder.includes(key));
    if (!missing.length) return;
    state.pageOrder = [...missing, ...state.pageOrder];
    savePageOrder();
  }

  ensureDashboardPageOrder();

  function activateDashboard(id) {
    const dashboard = state.dashboards.find((entry) => entry.id === id) || state.dashboards[0];
    if (!dashboard) return null;
    state.dashboardId = dashboard.id;
    state.overviewPrefs = dashboard.visible;
    state.overviewLayout = dashboard.layout;
    return dashboard;
  }

  function nativeShell() {
    return typeof window.OrqellaNative?.disconnect === "function"
      || Boolean(window.webkit?.messageHandlers?.orqellaNative);
  }

  function nativeDisconnect() {
    if (typeof window.OrqellaNative?.disconnect === "function") {
      return () => window.OrqellaNative.disconnect();
    }
    const handler = window.webkit?.messageHandlers?.orqellaNative;
    if (handler && typeof handler.postMessage === "function") {
      return () => handler.postMessage("disconnect");
    }
    return null;
  }

  document.documentElement.classList.toggle("native-shell", nativeShell());

  const api = window.OrqellaApi.api;

  const moduleId = (view = state.view) => view.startsWith("module:") ? view.slice(7) : null;
  const currentModule = () => state.modules.find((module) => module.id === state.module);
  const currentNode = () => state.nodes.find((node) => node.id === state.node) || state.nodes[0] || { id: "local", name: "local" };
  const moduleKey = (module) => module?.remote_node ? `${module.remote_node}:${module.id}` : module?.id;
  const pageDragHandle = (key) => state.overviewEditing
    ? `<span class="page-drag-handle" data-page-drag="${esc(key)}" aria-hidden="true" title="Drag to reorder">⠿</span>`
    : "";
  const moduleNameKey = (module) => String(module?.name ?? "").trim().toLocaleLowerCase();

  const Frames = window.OrqellaFrames;
  Frames.install({ state, $, api, render, currentNode });
  const {
    frameKey,
    newFrameNonce,
    frameHash,
    setFrameNonce,
    frameNonceMatches,
    moduleOriginFor,
    moduleFrameSrc,
    moduleFrameSandbox,
    moduleUsable,
    moduleMessageAllowed,
    configureModuleFrame,
    handleModuleBootstrap,
    handleModuleBroker,
    closeModuleBroker,
    moduleContext,
    sendContextTo,
    sendContext,
    sendOverviewContext,
    sendSettingsContext,
    sendThemeContext,
    rpc,
    waitForAppFrame,
    appPayloadForSelectedNode,
    openApp,
    recorderMime,
    blobBase64,
    startModuleRecording,
    stopAllRecordings,
    stopModuleRecording,
    frameTarget,
  } = Frames;

  const Dashboard = window.OrqellaDashboard;
  Dashboard.install({
    state, $, esc, render, selectModule, saveDashboards, saveSharedDashboards,
    saveSharedDashboardOrder, savePageOrder, savePagePrefs, toast, moduleId,
    currentNode, activeDashboard, moduleKey, clampInt, frameKey, newFrameNonce, frameHash, renderNav, dashboardIdFromView, dashboardPageKey, api, refresh,
    moduleFrameSrc, moduleFrameSandbox, moduleUsable, moduleOriginFor, closeModuleBroker,
    defaultDesktopLayout, defaultMobileLayout, overviewProfile,
    resolveOverviewColumns, projectOverviewSpan, saveDashboardLayout,
    loadSharedDashboards,
    OVERVIEW_DEFAULT_COLUMNS, OVERVIEW_MIN_COLUMN_WIDTH, OVERVIEW_MOBILE_QUERY,
    OVERVIEW_MAX_COLUMNS, OVERVIEW_MAX_ROWS, OVERVIEW_ROW_UNIT,
    OVERVIEW_DASHBOARD_ID,
  });
  const {
    renderHome,
    bindOverviewEditing,
    bindPageReorder,
    toggleModule,
    sortedLocalModules,
    ensureSelectedSettingsModule,
    overviewVisible,
    setOverviewVisible,
    saveOverviewLayout,
    orderedOverviewModules,
    overviewCardSize,
    overviewCardPosition,
    ensureOverviewPositions,
    setOverviewColumns,
    resetOverviewLayout,
    setOverviewEditing,
    pageVisible,
    setPageVisible,
    allPageKeys,
    orderedPageEntries,
    moduleIconUrl,
    moduleSurfaceUrl,
  } = Dashboard;

  const Settings = window.OrqellaSettings;
  Settings.install({
    state, $, esc, api, toast, render, renderNav, refresh, selectModule,
    saveViewState, moduleId, frameKey, renderHome, saveSharedDashboards,
    savePageOrder, dashboardIdFromView, selectDashboard, activateDashboard,
    selectView, setPageVisible, dashboardPageKey, newDashboardId,
    defaultDashboardLayout, sendContext, sendSettingsContext, sendThemeContext,
    toggleModule, sortedLocalModules, ensureSelectedSettingsModule,
    nativeDisconnect, newFrameNonce, frameHash, OVERVIEW_DASHBOARD_ID,
    moduleKey, pageVisible, openPermissions,
    moduleFrameSrc, moduleFrameSandbox, moduleUsable, moduleOriginFor,
  });
  const {
    renderSettings,
    renderSettingsModules,
    renderSettingsDashboards,
    renderSettingsGeneral,
    renderSettingsModule,
    fieldControl,
    buildApi,
  } = Settings;

  const Build = window.OrqellaBuild;
  Build.install({ state, $, esc, api, toast, refresh, currentNode, buildApi, moduleIconUrl, openConversationPayload });
  const { renderBuild, pollJobs } = Build;

  function toast(title, message, kind = "") {
    const node = document.createElement("div");
    node.className = `toast ${kind}`;
    node.innerHTML = `<b>${esc(title)}</b><span>${esc(message)}</span>`;
    $("toasts").append(node);
    setTimeout(() => node.remove(), 4200);
  }

  async function boot() {
    try {
      state.info = await api("/api/v1/info");
      await loadSharedDashboards({ rerender: false });
      await refresh();
      if (state.info?.onboarding?.completed === false) $("onboarding").hidden = false;
      window.addEventListener("resize", () => {});
      setInterval(refreshNodes, 8000);
      setInterval(() => { if (!document.hidden) void loadSharedDashboards(); }, 15000);
      document.addEventListener("visibilitychange", () => {
        if (!document.hidden) void loadSharedDashboards();
      });
      applyRail();
      render();
    } catch (error) {
      $("coreView").innerHTML = `<div class="stack"><div class="empty"><h2>Orqella is not answering</h2><p>${esc(error.message)}</p></div></div>`;
      $("coreView").hidden = false;
    }
  }

  async function loadNodeModules() {
    if (state.node === "local") return state.localModules;
    const node = state.nodes.find((candidate) => candidate.id === state.node);
    if (!node || !node.online) {
      throw new Error(`${node?.name || state.node} is offline`);
    }
    const modules = await api(`/api/v1/nodes/${encodeURIComponent(state.node)}/modules`);
    return Array.isArray(modules) ? modules.map((module) => ({ ...module, remote_node: state.node })) : [];
  }

  async function refresh() {
    const coreView = $("coreView");
    const viewBefore = state.view;
    const scrollTop = coreView?.scrollTop || 0;
    const [modules, nodes, peers] = await Promise.all([
      api("/api/v1/modules"),
      api("/api/v1/nodes"),
      api("/api/v1/peers"),
    ]);
    state.localModules = modules;
    state.nodes = nodes;
    state.peers = peers;
    if (state.node !== "local") {
      const selected = nodes.find((node) => node.id === state.node);
      if (!selected || !selected.online) {
        state.node = "local";
        localStorage.setItem("orqella.node", state.node);
      }
    }
    try {
      state.modules = await loadNodeModules();
      state.modulesError = null;
    } catch (error) {
      state.modules = [];
      state.modulesError = error.message || "Could not load modules from this machine.";
    }
    if (!state.module || !state.modules.some((module) => module.id === state.module && module.has_page && module.enabled)) {
      state.module = state.modules.find((module) => module.has_page && module.enabled)?.id || null;
    }
    validateViewState();
    renderNav();
    renderNodeSelect();
    render();
    saveViewState();
    if (state.view === viewBefore && coreView) {
      coreView.scrollTop = scrollTop;
    }
  }

  async function refreshNodes() {
    if (document.hidden) return;
    try {
      state.nodes = await api("/api/v1/nodes");
      renderNodeSelect();
      if (state.wake) {
        const target = state.nodes.find((node) => node.id === state.wake.node);
        if (target?.online) await finishWake(true);
        return;
      }
      if (state.node !== "local") {
        const selected = state.nodes.find((node) => node.id === state.node);
        if (!selected || !selected.online) {
          state.node = "local";
          localStorage.setItem("orqella.node", state.node);
          renderNodeSelect();
          await refresh();
          return;
        }
      }
      if (!state.nodes.some((node) => node.id === state.node)) {
        state.node = "local";
        renderNodeSelect();
        if (dashboardIdFromView(state.view)) renderHome();
        else sendContext();
      }
    } catch {}
  }

  function applyRail() {
    $("app").classList.toggle("rail-collapsed", state.railCollapsed);
    const toggle = $("railToggle");
    toggle.textContent = state.railCollapsed ? "›" : "‹";
    toggle.title = state.railCollapsed ? "Expand sidebar" : "Collapse sidebar";
    toggle.setAttribute("aria-label", toggle.title);
  }

  function toggleRail() {
    state.railCollapsed = !state.railCollapsed;
    localStorage.setItem("orqella.rail.collapsed", state.railCollapsed ? "1" : "0");
    applyRail();
  }

  function setMobileMenu(open) {
    const menu = $("mobileMenu");
    menu.hidden = !open;
    $("mobileMenuButton").setAttribute("aria-expanded", String(open));
  }

  function toggleMobileMenu() {
    setMobileMenu($("mobileMenu").hidden);
  }

  function renderMobileMenu() {
    const menu = $("mobileMenu");
    const navItem = (attributes, glyph, label, active, extra = "") => `<button class="nav-item${active ? " active" : ""}" ${attributes}><span class="nav-glyph">${glyph}</span><span class="nav-label">${esc(label)}</span>${extra}</button>`;
    const pages = orderedPageEntries().map((entry) => {
      if (entry.kind === "dashboard") {
        const active = state.view === dashboardView(entry.dashboard.id) && !state.app;
        return navItem(
          `data-mobile-dashboard="${esc(entry.dashboard.id)}" data-page-key="${esc(entry.key)}" title="${esc(entry.dashboard.name)}"`,
          entry.dashboard.id === OVERVIEW_DASHBOARD_ID ? "⌂" : "▦",
          entry.dashboard.name,
          active,
          pageDragHandle(entry.key)
        );
      }
      const module = entry.module;
      const active = state.view === `module:${module.id}` && !state.app;
      const glyph = moduleIconUrl(module)
        ? `<img src="${esc(moduleIconUrl(module))}" alt="">`
        : "◇";
      return navItem(`data-mobile-module="${esc(module.id)}" data-page-key="${esc(entry.key)}" title="${esc(module.name)}"`, glyph, module.name, active, pageDragHandle(entry.key));
    }).join("");
    const utility = [
      ["build", "✦", "Build", state.view === "build" && !state.app],
      ["settings", "⌘", "Settings", state.view === "settings" && !state.app],
    ].map(([view, glyph, label, active]) =>
      navItem(`data-mobile-view="${view}" title="${esc(label)}"`, glyph, label, active)
    ).join("");

    menu.innerHTML = `<div class="mobile-menu-section"><p class="mobile-menu-heading">Pages</p>${pages}</div><div class="mobile-menu-section"><p class="mobile-menu-heading">Dashboard</p>${utility}</div>`;
    bindPageReorder(menu);
    menu.querySelectorAll("[data-mobile-view]").forEach((button) => {
      button.onclick = () => {
        setMobileMenu(false);
        selectView(button.dataset.mobileView);
      };
    });
    menu.querySelectorAll("[data-mobile-dashboard]").forEach((button) => {
      button.onclick = () => {
        setMobileMenu(false);
        selectDashboard(button.dataset.mobileDashboard);
      };
    });
    menu.querySelectorAll("[data-mobile-module]").forEach((button) => {
      button.onclick = () => {
        setMobileMenu(false);
        selectModule(button.dataset.mobileModule);
      };
    });
  }

  function render() {
    const viewDashboardId = dashboardIdFromView(state.view);
    if (viewDashboardId) {
      if (state.dashboards.some((dashboard) => dashboard.id === viewDashboardId)) {
        activateDashboard(viewDashboardId);
      } else {
        state.view = "home";
        activateDashboard(OVERVIEW_DASHBOARD_ID);
        saveViewState();
      }
    }
    renderNav();
    renderNodeSelect();

    if (state.app) {
      $("coreView").hidden = true;
      $("moduleHost").hidden = true;
      $("appSurface").hidden = false;
      return;
    }
    $("appSurface").hidden = true;
    if (state.view.startsWith("module:")) {
      $("coreView").hidden = true;
      $("moduleHost").hidden = false;
      const module = currentModule();
      if (module && module.has_page && !moduleUsable(module)) {
        $("moduleHost").hidden = true;
        $("coreView").hidden = false;
        $("coreView").innerHTML = `<div class="empty"><h2>Module origin unavailable</h2><p>This client requires an isolated module origin. Enable module origins and reload.</p></div>`;
        return;
      }
      if (module && module.has_page) {
        const frame = $("moduleFrame");
        const key = moduleKey(module);
        const path = state.modulePath || "";
        const suffix = path.startsWith("/") ? path.slice(1) : path;
        const target = module.remote_node
          ? `/n/${encodeURIComponent(module.remote_node)}/m/${encodeURIComponent(module.id)}/${suffix}`
          : `/m/${encodeURIComponent(module.id)}/${suffix}`;
        const moduleFrameKey = `${key}|${path}`;
        if (frame.dataset.module !== moduleFrameKey || !frame.dataset.frameNonce) {
          frame.dataset.module = moduleFrameKey;
          const nonce = setFrameNonce(frame, "page", state.module);
          configureModuleFrame(frame, module);
          const isolated = Boolean(moduleOriginFor(module));
          if (isolated) {
            frame.src = moduleFrameSrc(module, "page", path, nonce);
          } else {
            // The nonce must also appear in the query. When the page is
            // re-entered at the same path, a hash-only change is a same-document
            // navigation and would not reload the iframe (and its one-shot
            // `ready` callbacks would never run again).
            const separator = target.includes("?") ? "&" : "?";
            const transport = transportParam();
            frame.src = `${target.split("#")[0]}${separator}__orqella_reload=${encodeURIComponent(nonce)}${transport ? `&${transport}` : ""}${frameHash(nonce, module.ws_token)}`;
          }
        }
        setTimeout(sendContext, 40);
      } else {
        state.view = "home";
        state.module = null;
        state.modulePath = "";
        saveViewState();
        $("moduleHost").hidden = true;
        $("coreView").hidden = false;
        renderCore();
      }
      return;
    }
    $("moduleHost").hidden = true;
    $("coreView").hidden = false;
    renderCore();
  }

  function renderNav() {
    const nav = $("moduleNav");
    const items = orderedPageEntries().map((entry) => {
      if (entry.kind === "dashboard") {
        const active = state.view === dashboardView(entry.dashboard.id) && !state.app ? " active" : "";
        const glyph = entry.dashboard.id === OVERVIEW_DASHBOARD_ID ? "⌂" : "▦";
        return `<button class="nav-item${active}" data-dashboard="${esc(entry.dashboard.id)}" data-page-key="${esc(entry.key)}" title="${esc(entry.dashboard.name)}"><span class="nav-glyph">${glyph}</span><span class="nav-label">${esc(entry.dashboard.name)}</span>${pageDragHandle(entry.key)}</button>`;
      }
      const module = entry.module;
      const active = state.view === `module:${module.id}` && !state.app ? " active" : "";
      const icon = moduleIconUrl(module)
        ? `<img src="${esc(moduleIconUrl(module))}" alt="">`
        : "◇";
      return `<button class="nav-item${active}" data-module="${esc(module.id)}" data-page-key="${esc(entry.key)}" title="${esc(module.name)}"><span class="nav-glyph">${icon}</span><span class="nav-label">${esc(module.name)}</span>${pageDragHandle(entry.key)}</button>`;
    }).join("");
    nav.innerHTML = items;
    bindPageReorder(nav);
    nav.querySelectorAll("[data-dashboard]").forEach((button) => {
      button.addEventListener("click", () => selectDashboard(button.dataset.dashboard));
    });
    nav.querySelectorAll("[data-module]").forEach((button) => {
      button.addEventListener("click", () => selectModule(button.dataset.module));
    });
    document.querySelectorAll("[data-view]").forEach((button) => {
      button.classList.toggle("active", button.dataset.view === state.view && !state.app);
      if (button.dataset.view) button.onclick = () => selectView(button.dataset.view);
    });
    renderMobileMenu();
  }

  function renderNodeSelect() {
    const select = $("nodeSelect");
    const selectedId = state.wake?.node || state.node;
    select.innerHTML = state.nodes.map((node) =>
      `<option value="${esc(node.id)}"${node.id === selectedId ? " selected" : ""}>${esc(node.name)}${node.online ? "" : " · offline"}</option>`
    ).join("");
    select.disabled = Boolean(state.wake);
    const selected = state.nodes.find((node) => node.id === selectedId);
    $("railNode").textContent = selected?.name || "local";
    const active = state.nodes.find((node) => node.id === state.node);
    $("railStatus").className = `status-dot ${active?.online ? "online" : ""}`;
  }

  function selectView(view) {
    stopAllRecordings();
    const dashboardId = dashboardIdFromView(view);
    if (dashboardId) {
      if (!state.dashboards.some((dashboard) => dashboard.id === dashboardId)) {
        view = "home";
      } else {
        activateDashboard(dashboardId);
      }
    }
    state.view = view;
    state.app = null;
    state.module = null;
    state.modulePath = "";
    state.overviewEditing = false;
    state.buildEditModule = null;
    if (view === "settings") state.settingsTab = "general";
    render();
    saveViewState();
  }

  function selectDashboard(id) {
    const dashboard = state.dashboards.find((entry) => entry.id === id);
    if (!dashboard) {
      toast("Dashboard not found", id, "bad");
      return;
    }
    selectView(dashboardView(id));
  }

  function selectModule(id, path = "") {
    stopAllRecordings();
    state.overviewEditing = false;
    state.buildEditModule = null;
    const module = state.modules.find((entry) => entry.id === id);
    if (!module || !module.has_page) {
      toast("No module page", "This module only provides an overview card.", "bad");
      return false;
    }
    if (!module.enabled) {
      toast("Module is disabled", "Enable it in Settings first.", "bad");
      return false;
    }
    state.module = id;
    state.modulePath = path;
    state.view = `module:${id}`;
    state.app = null;
    render();
    saveViewState();
    return true;
  }

  async function switchNode(nodeId) {
    if (state.node === nodeId) return true;
    if (nodeId !== "local") {
      const target = state.nodes.find((node) => node.id === nodeId);
      if (!target?.online) return false;
    }
    state.node = nodeId;
    localStorage.setItem("orqella.node", state.node);
    state.jobs = [];
    state.buildEditModule = null;
    await refresh();
    return state.node === nodeId;
  }

  async function openModuleViewer(payload) {
    if (!payload?.module) return false;
    if (payload.node && payload.node !== state.node && !(await switchNode(payload.node))) {
      toast("Machine unavailable", `${payload.node} is offline.`, "bad");
      return false;
    }
    return selectModule(payload.module, payload.path || "");
  }

  async function openConversationPayload(open) {
    if (!open) return false;
    if (open.external && open.url) {
      window.open(open.url, "_blank", "noopener");
      return true;
    }
    if (open.module) {
      return openModuleViewer(open);
    }
    if (open.app) {
      const node = open.app.node;
      if (node && node !== state.node && !(await switchNode(node))) {
        toast("Machine unavailable", `${node} is offline.`, "bad");
        return false;
      }
      openApp(appPayloadForSelectedNode(open.app))
        .catch((error) => toast("Could not open app", error.message, "bad"));
      return true;
    }
    return false;
  }

  let pickDirRequest = null;
  const pickDirState = { node: "local", path: "", parent: null };

  function directoryPicker(moduleId, params) {
    return new Promise((resolve) => {
      if (pickDirRequest) pickDirRequest.resolve({ cancelled: true });
      pickDirRequest = { resolve };
      const module = state.modules.find((entry) => entry.id === moduleId);
      const node = module?.remote_node || params.node || "local";
      $("pickDirTitle").textContent = params.title || "Choose a folder";
      $("pickDirModal").hidden = false;
      loadPickerDirectory(node, params.start || "");
    });
  }

  async function loadPickerDirectory(node, path) {
    pickDirState.node = node;
    $("pickDirList").innerHTML = `<p class="small muted">Loading…</p>`;
    try {
      const query = new URLSearchParams();
      if (path) query.set("path", path);
      const endpoint = node === "local"
        ? `/api/v1/fs/dirs?${query}`
        : `/api/v1/nodes/${encodeURIComponent(node)}/fs/dirs?${query}`;
      const result = await api(endpoint);
      pickDirState.path = result.path;
      pickDirState.parent = result.parent;
      $("pickDirPath").textContent = result.path;
      $("pickDirList").innerHTML = result.entries?.length
        ? result.entries.map((entry) => `<button type="button" class="pick-dir-row" data-path="${esc(entry.path)}">${esc(entry.name)}</button>`).join("")
        : `<p class="small muted">No folders here.</p>`;
      $("pickDirList").querySelectorAll("[data-path]").forEach((button) => {
        button.onclick = () => loadPickerDirectory(node, button.dataset.path);
      });
      $("pickDirUp").disabled = !result.parent;
    } catch (error) {
      $("pickDirList").innerHTML = `<p class="small bad">${esc(error.message)}</p>`;
    }
  }

  function finishDirectoryPicker(result) {
    $("pickDirModal").hidden = true;
    if (pickDirRequest) {
      pickDirRequest.resolve(result);
      pickDirRequest = null;
    }
  }

  async function openModulePath(moduleId, params) {
    const path = typeof params.path === "string" ? params.path : "";
    if (
      path
      && (!path.startsWith("/")
        || path.startsWith("//")
        || path.includes("\\")
        || path.split("/").some((part) => part === "..")
        || path.split("").some((character) => character.charCodeAt(0) < 32))
    ) {
      throw new Error("invalid module path");
    }
    const module = state.modules.find((entry) => entry.id === moduleId);
    if (!module) throw new Error("module not found");
    if (module.remote_node && module.remote_node !== state.node) {
      if (!(await switchNode(module.remote_node))) throw new Error("machine unavailable");
    }
    if (!selectModule(moduleId, path)) throw new Error("could not open the module page");
    return { ok: true, module: moduleId, path };
  }

  async function handleFrameRequest(target, message, respond) {
    if (message.type !== "orqella:request") return;
    if (!frameNonceMatches(target.frame, target.surface, target.id, message.nonce)
      || !state.readyFrames.has(frameKey(target.surface, target.id))) return;
    const id = message.id;
    try {
      let response;
      if (message.method === "media.record.start") {
        response = { result: await startModuleRecording(target.id) };
      } else if (message.method === "media.record.stop") {
        response = { result: await stopModuleRecording(target.id) };
      } else if (message.method === "system.pick_directory") {
        response = { result: await directoryPicker(target.id, message.params || {}) };
      } else if (message.method === "module.open") {
        response = { result: await openModulePath(target.id, message.params || {}) };
      } else {
        response = await rpc(target.id, message.method, message.params || {});
      }
      respond({ type: "orqella:response", id, ok: true, result: response.result });
      if (message.method === "app.open" && response.result?.url) {
        openApp(appPayloadForSelectedNode(response.result))
          .catch((error) => toast("Could not open app", error.message, "bad"));
      }
      if (message.method === "hooks.run" && response.result?.open) {
        await openConversationPayload(response.result.open);
      }
    } catch (error) {
      respond({ type: "orqella:response", id, ok: false, error: error.message });
    }
  }

  state.frameMessage = (frame, surface, id, message, port) => {
    handleFrameRequest({ frame, surface, id }, message, (value) => port.postMessage(value));
  };

  $("closePickDir").onclick = () => finishDirectoryPicker({ cancelled: true });
  $("pickDirCancel").onclick = () => finishDirectoryPicker({ cancelled: true });
  $("pickDirUp").onclick = () => {
    if (pickDirState.parent) loadPickerDirectory(pickDirState.node, pickDirState.parent);
  };
  $("pickDirChoose").onclick = () => finishDirectoryPicker({ path: pickDirState.path });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !$("pickDirModal").hidden) {
      finishDirectoryPicker({ cancelled: true });
    }
  });

  window.addEventListener("message", (event) => {
    const target = frameTarget(event.source);
    if (!target) return;
    const module = state.modules.find((entry) => entry.id === target.id);
    // Module frames are either opaque sandbox origins (legacy path) or the
    // isolated origin assigned to that module. Never trust a navigated frame.
    if (!moduleMessageAllowed(module, event.origin)) return;
    const message = event.data;
    if (!message || typeof message !== "object") return;
    if (message.type === "orqella:module-bootstrap") {
      handleModuleBootstrap(target, message, event.ports?.[0]);
      return;
    }
    if (message.type === "orqella:module-broker") {
      handleModuleBroker(target, message, event.ports?.[0]);
      return;
    }
    if (message.type === "orqella:frame-height") {
      const target = frameTarget(event.source);
      if (!target || target.surface !== "settings") return;
      const frame = state.settingsFrames.get(target.id);
      const height = Number(message.height);
      if (!frame || !Number.isFinite(height)) return;
      frame.style.height = `${Math.max(200, Math.min(20000, Math.round(height)))}px`;
      return;
    }
    if (message.type === "orqella:frame-ready") {
      if (!frameNonceMatches(target.frame, target.surface, target.id, message.nonce)) return;
      state.readyFrames.add(frameKey(target.surface, target.id));
      if (target.surface === "overview") sendOverviewContext(target.id);
      else if (target.surface === "settings") sendSettingsContext(target.id);
      else sendContext();
    }
  });

  $("finishOnboarding").onclick = async () => {
    try {
      await api("/api/v1/onboarding/complete", { method: "POST", body: "{}" });
      $("onboarding").hidden = true;
      toast("Welcome to Orqella", "Your local dashboard is ready.", "ok");
    } catch (error) { toast("Could not finish onboarding", error.message, "bad"); }
  };

  async function applyNodeSelection(nodeId) {
    const previousModule = currentModule();
    const preserveModule = state.view.startsWith("module:") && !state.app && previousModule;
    state.node = nodeId;
    localStorage.setItem("orqella.node", state.node);
    state.jobs = [];
    state.buildEditModule = null;
    if (!preserveModule) {
      state.view = "home";
      state.module = null;
      state.app = null;
      state.overviewEditing = false;
      $("moduleFrame").dataset.module = "";
    }
    try {
      await refresh();
    } catch (error) {
      toast("Could not load machine modules", error.message, "bad");
      state.view = "home";
      state.module = null;
      state.app = null;
      $("moduleFrame").dataset.module = "";
      render();
      saveViewState();
      return;
    }
    if (preserveModule) {
      const previousName = moduleNameKey(previousModule);
      const match = state.modules.find((module) =>
        module.enabled
        && module.has_page
        && (previousName ? moduleNameKey(module) === previousName : module.id === previousModule.id)
      );
      if (match) {
        state.module = match.id;
        state.view = `module:${match.id}`;
        state.app = null;
        render();
      } else {
        state.view = "home";
        state.module = null;
        state.app = null;
        $("moduleFrame").dataset.module = "";
        render();
      }
      saveViewState();
    }
  }

  const WAKE_TIMEOUT_MS = 60000;
  const WAKE_POLL_MS = 2000;

  function showWake(visible) {
    $("wakeModal").hidden = !visible;
  }

  async function wakeAndSelect(target) {
    if (state.wake) return;
    const attempt = {
      node: target.id,
      name: target.name,
      previous: state.node,
    };
    state.wake = attempt;
    $("wakeTitle").textContent = `Waking ${target.name}…`;
    $("wakeHint").textContent = "Sending a wake-on-LAN packet and waiting for the machine to come online. This can take up to a minute.";
    showWake(true);
    renderNodeSelect();
    try {
      await api(`/api/v1/nodes/${encodeURIComponent(target.id)}/wake`, { method: "POST", body: "{}" });
    } catch (error) {
      await finishWake(false, error.message);
      return;
    }
    const deadline = Date.now() + WAKE_TIMEOUT_MS;
    while (state.wake === attempt && Date.now() < deadline) {
      await new Promise((resolve) => setTimeout(resolve, WAKE_POLL_MS));
      if (state.wake !== attempt) return;
      try {
        state.nodes = await api("/api/v1/nodes");
        renderNodeSelect();
        const node = state.nodes.find((candidate) => candidate.id === target.id);
        if (node?.online) {
          await finishWake(true);
          return;
        }
      } catch {}
    }
    if (state.wake === attempt) {
      await finishWake(false, `${target.name} did not come online.`);
    }
  }

  async function finishWake(success, detail = "", kind = "bad", title = "Could not wake machine") {
    const attempt = state.wake;
    if (!attempt) return;
    state.wake = null;
    showWake(false);
    if (success) {
      toast("Machine awake", `${attempt.name} is online.`, "ok");
      await applyNodeSelection(attempt.node);
      return;
    }
    const previous = state.nodes.find((node) => node.id === attempt.previous && node.online);
    state.node = previous ? attempt.previous : "local";
    localStorage.setItem("orqella.node", state.node);
    renderNodeSelect();
    render();
    const machine = state.nodes.find((node) => node.id === state.node);
    toast(title, detail || `Staying on ${machine?.name || "this machine"}.`, kind);
  }

  $("cancelWake").addEventListener("click", () => {
    const attempt = state.wake;
    if (!attempt) return;
    const machine = state.nodes.find((node) => node.id === attempt.previous);
    finishWake(false, `Staying on ${machine?.name || "this machine"}.`, "", "Wake cancelled");
  });

  $("nodeSelect").addEventListener("change", async (event) => {
    if (state.wake) return;
    const target = state.nodes.find((node) => node.id === event.target.value);
    if (!target) {
      renderNodeSelect();
      return;
    }
    if (!target.online) {
      if (target.wake) {
        await wakeAndSelect(target);
        return;
      }
      toast("Machine offline", `No wake-on-LAN available for ${target.name}.`, "bad");
      renderNodeSelect();
      return;
    }
    await applyNodeSelection(target.id);
  });
  $("railToggle").addEventListener("click", toggleRail);
  $("mobileMenuButton").addEventListener("click", (event) => {
    event.stopPropagation();
    toggleMobileMenu();
  });
  $("mobileMenu").addEventListener("click", (event) => event.stopPropagation());
  document.addEventListener("click", () => setMobileMenu(false));
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") {
      setMobileMenu(false);
      if (state.overviewEditing) setOverviewEditing(false);
      if (state.buildEditModule) {
        state.buildEditModule = null;
        if (state.view === "build") renderBuild();
      }
    }
  });
  const mobileViewport = window.matchMedia("(max-width: 680px)");
  mobileViewport.addEventListener("change", (event) => {
    if (!event.matches) setMobileMenu(false);
  });

  function renderCore() {
    const view = state.view;
    if (view === "build") return renderBuild();
    if (view === "settings") return renderSettings();
    return renderHome();
  }

  function openPermissions(id) {
    const module = state.localModules.find((entry) => entry.id === id);
    if (!module) return;
    const unsandboxed = module.backend === "sidecar"
      && (module.backend_sandbox_effective ?? module.backend_sandbox) === false;
    $("permissionTitle").textContent = module.name;
    $("permissionIntro").textContent = module.pending_permissions?.length
      ? "This update is asking for access that was not granted before. Review it before allowing."
      : "Choose which host capabilities this module may use.";
    $("permissionList").innerHTML = (module.permissions || []).map((permission) => {
      const granted = (module.granted_permissions || []).includes(permission);
      const pending = (module.pending_permissions || []).includes(permission);
      return `<label class="permission-row"><input type="checkbox" data-permission="${esc(permission)}" ${granted ? "checked" : ""}><span class="perm">${esc(permission)}</span><span>${pending ? "newly requested — leave unchecked to deny" : granted ? "already granted" : "declared by module"}</span></label>`;
    }).join("") || `<div class="empty">This module declares no host permissions.</div>`;
    $("permissionSandboxAck").checked = false;
    $("permissionSandboxHelp").textContent =
      `The ${module.name} sidecar runs as your logged-in user with access outside the module sandbox. Only grant backend if you trust it.`;
    const syncSandboxAck = () => {
      const backend = $("permissionList").querySelector('[data-permission="backend"]');
      const required = Boolean(unsandboxed && backend?.checked);
      $("permissionSandboxRow").hidden = !required;
      if (!required) $("permissionSandboxAck").checked = false;
    };
    $("permissionList").querySelectorAll("[data-permission]").forEach((box) => {
      box.onchange = syncSandboxAck;
    });
    syncSandboxAck();
    $("permissionModal").hidden = false;
    $("permissionModal").dataset.module = id;
  }
  function closePermissions() { $("permissionModal").hidden = true; }
  $("closePermissions").onclick = closePermissions;
  $("cancelPermissions").onclick = closePermissions;
  $("permissionModal").addEventListener("click", (event) => { if (event.target === $("permissionModal")) closePermissions(); });
  $("grantPermissions").onclick = async () => {
    const id = $("permissionModal").dataset.module;
    const module = state.localModules.find((entry) => entry.id === id);
    const permissions = [...$("permissionList").querySelectorAll("[data-permission]")].filter((box) => box.checked).map((box) => box.dataset.permission);
    const unsandboxedBackend = Boolean(
      module
      && module.backend === "sidecar"
      && (module.backend_sandbox_effective ?? module.backend_sandbox) === false
      && permissions.includes("backend")
    );
    const sandboxAcknowledged = !$("permissionSandboxRow").hidden && $("permissionSandboxAck").checked;
    if (unsandboxedBackend && !sandboxAcknowledged) {
      toast("Confirm sandbox access", "Check the unsandboxed warning before granting backend.", "bad");
      return;
    }
    try {
      await api(`/api/v1/modules/${encodeURIComponent(id)}/permissions`, { method: "POST", body: JSON.stringify({ permissions, sandbox_acknowledged: sandboxAcknowledged }) });
      closePermissions();
      await refresh();
      toast("Permissions saved", id, "ok");
    } catch (error) { toast("Could not save permissions", error.message, "bad"); }
  };

  async function updateSessionBanner() {
    try {
      const response = await fetch("/__orqella/session", { cache: "no-store" });
      const status = response.ok ? await response.json() : {};
      $("sessionBanner").hidden = Boolean(status.privileged) || !status.authenticated;
    } catch (_) {
      $("sessionBanner").hidden = true;
    }
  }

  const linkAccountOrigin = "https://link.orqella.io";
  $("linkUnlockOpen").onclick = () => {
    const url = `${linkAccountOrigin}/login?origin=${encodeURIComponent(location.origin)}`;
    window.open(url, "orqella-link-login", "popup,width=520,height=760");
  };
  window.addEventListener("message", (event) => {
    if (event.origin !== linkAccountOrigin) return;
    const data = event.data;
    if (!data || data.type !== "orqella-link-token" || typeof data.token !== "string") return;
    $("linkUnlockToken").value = data.token;
    $("linkUnlockButton").click();
  });
  $("linkUnlockButton").onclick = async () => {
    const token = $("linkUnlockToken").value.trim();
    if (!token) return;
    $("linkUnlockButton").disabled = true;
    try {
      const response = await fetch("/__orqella/link-session", {
        method: "POST",
        cache: "no-store",
        headers: { "Content-Type": "application/json", "X-Orqella": "1" },
        body: JSON.stringify({ token }),
      });
      if (!response.ok) {
        const text = await response.text().catch(() => "");
        let detail = text;
        try { detail = JSON.parse(text).error || text; } catch (_) {}
        throw new Error(detail || `Link unlock failed (HTTP ${response.status})`);
      }
      window.__orqellaSessionReady = true;
      location.reload();
    } catch (error) {
      toast("Could not unlock", error.message, "bad");
      $("linkUnlockButton").disabled = false;
    }
  };

  applyRail();
  (async () => {
    sessionReady = await bootstrapSession();
    window.__orqellaSessionReady = sessionReady;
    await updateSessionBanner();
    boot();
  })();
})();
