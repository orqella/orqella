(function (global) {
  "use strict";

  let state;
  let $;
  let esc;
  let render;
  let selectModule;
  let saveDashboards;
  let saveSharedDashboards;
  let saveSharedDashboardOrder;
  let savePageOrder;
  let savePagePrefs;
  let toast;
  let moduleId;
  let currentNode;
  let activeDashboard;
  let moduleKey;
  let clampInt;
  let frameKey;
  let newFrameNonce;
  let frameHash;
  let moduleFrameSrc;
  let moduleFrameSandbox;
  let moduleUsable;
  let moduleOriginFor;
  let closeModuleBroker;
  let renderNav;
  let dashboardIdFromView;
  let dashboardPageKey;
  let defaultDesktopLayout;
  let defaultMobileLayout;
  let overviewProfile;
  let resolveOverviewColumns;
  let projectOverviewSpan;
  let saveDashboardLayout;
  let loadSharedDashboards;
  let api;
  let refresh;
  let OVERVIEW_DEFAULT_COLUMNS;
  let OVERVIEW_MAX_COLUMNS;
  let OVERVIEW_MAX_ROWS;
  let OVERVIEW_ROW_UNIT;
  let OVERVIEW_MIN_COLUMN_WIDTH;
  let OVERVIEW_MOBILE_QUERY;
  let OVERVIEW_DASHBOARD_ID;

  function install(ctx) {
    ({
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
    } = ctx);
    installCompactWatcher();
  }

  function overviewCompact() {
    return typeof window.matchMedia === "function"
      && window.matchMedia(OVERVIEW_MOBILE_QUERY).matches;
  }

  function activeOverviewProfile() {
    return overviewProfile(state.overviewLayout, overviewCompact());
  }

  function installCompactWatcher() {
    if (typeof window.matchMedia !== "function" || state.overviewCompactWatcher) return;
    const query = window.matchMedia(OVERVIEW_MOBILE_QUERY);
    let compact = query.matches;
    const onChange = (event) => {
      if (event.matches === compact) return;
      compact = event.matches;
      if (dashboardIdFromView(state.view)) renderHome();
    };
    if (typeof query.addEventListener === "function") query.addEventListener("change", onChange);
    else if (typeof query.addListener === "function") query.addListener(onChange);
    state.overviewCompactWatcher = query;
  }

  function overviewVisible(module) {
    const preference = state.overviewPrefs[moduleKey(module)];
    if (typeof preference === "boolean") return preference;
    return activeDashboard()?.defaultVisible === true && module.overview_default !== false;
  }

  function setOverviewVisible(id, visible) {
    state.overviewPrefs[id] = visible;
    saveSharedDashboards();
  }

  function saveOverviewLayout() {
    const dashboard = activeDashboard();
    if (!dashboard) return;
    dashboard.layout = state.overviewLayout;
    saveDashboards();
    saveDashboardLayout(dashboard.id, overviewCompact() ? "mobile" : "desktop");
  }

  function orderedOverviewModules(modules, profile = activeOverviewProfile()) {
    const order = Array.isArray(profile?.order) ? profile.order : [];
    const rank = new Map(order.map((id, index) => [id, index]));
    return modules
      .map((module, index) => ({ module, index, rank: rank.has(module.id) ? rank.get(module.id) : Number.POSITIVE_INFINITY }))
      .sort((a, b) => a.rank - b.rank || a.index - b.index)
      .map((entry) => entry.module);
  }

  function overviewCardSize(module, profile = activeOverviewProfile(), compact = overviewCompact()) {
    const fallbackHeight = clampInt(module.overview_height, 1, OVERVIEW_MAX_ROWS, 3);
    if (compact) {
      const stored = profile?.sizes?.[module.id] || {};
      return {
        full: true,
        width: 1,
        height: clampInt(stored.height, 1, OVERVIEW_MAX_ROWS, fallbackHeight),
      };
    }
    const columns = profile?.columns || OVERVIEW_DEFAULT_COLUMNS;
    const stored = profile?.sizes?.[module.id] || {};
    const legacyFull = Array.isArray(profile?.fullWidth) && profile.fullWidth.includes(module.id);
    const storedWidth = Object.prototype.hasOwnProperty.call(stored, "width") ? stored.width : (legacyFull ? 0 : module.overview_width);
    const storedHeight = Object.prototype.hasOwnProperty.call(stored, "height") ? stored.height : module.overview_height;
    const full = storedWidth === 0 || storedWidth === "full";
    return {
      full,
      width: full ? columns : clampInt(storedWidth, 1, columns, Math.min(2, columns)),
      height: clampInt(storedHeight, 1, OVERVIEW_MAX_ROWS, 3),
    };
  }

  function overviewCellsOverlap(a, b) {
    return a.x < b.x + b.width
      && a.x + a.width > b.x
      && a.y < b.y + b.height
      && a.y + a.height > b.y;
  }

  function overviewCardPosition(module, profile = activeOverviewProfile(), compact = overviewCompact()) {
    if (compact) return { x: 0, y: 0 };
    const stored = profile?.positions?.[module.id];
    if (stored && Number.isInteger(stored.x) && Number.isInteger(stored.y) && stored.x >= 0 && stored.y >= 0) {
      return { x: stored.x, y: stored.y };
    }
    return { x: 0, y: 0 };
  }

  function findOverviewSlot(placed, width, height, columns) {
    for (let y = 0; y < 200; y += 1) {
      for (let x = 0; x <= columns - width; x += 1) {
        const candidate = { x, y, width, height };
        if (!placed.some((entry) => overviewCellsOverlap(candidate, entry))) {
          return { x, y };
        }
      }
    }
    return { x: 0, y: 0 };
  }

  function ensureOverviewPositions(modules, profile = activeOverviewProfile(), compact = overviewCompact()) {
    if (compact) return [];
    const columns = profile?.columns || OVERVIEW_DEFAULT_COLUMNS;
    const positions = profile.positions || {};
    const placed = [];
    let changed = false;
    modules.forEach((module) => {
      const size = overviewCardSize(module, profile, false);
      const stored = positions[module.id];
      const candidate = stored
        && Number.isInteger(stored.x)
        && Number.isInteger(stored.y)
        && stored.x >= 0
        && stored.y >= 0
        ? { id: module.id, x: stored.x, y: stored.y, width: size.width, height: size.height }
        : null;
      if (candidate && candidate.x + candidate.width <= columns
        && !placed.some((entry) => overviewCellsOverlap(candidate, entry))) {
        placed.push(candidate);
        return;
      }
      const slot = findOverviewSlot(placed, size.width, size.height, columns);
      positions[module.id] = slot;
      placed.push({ id: module.id, ...slot, width: size.width, height: size.height });
      changed = true;
    });
    profile.positions = positions;
    if (changed) saveOverviewLayout();
    return placed;
  }

  function resolveOverviewPlacements(modules, width, profile = activeOverviewProfile(), gap = 13) {
    const authored = profile?.columns || OVERVIEW_DEFAULT_COLUMNS;
    const columns = resolveOverviewColumns(authored, width, gap);
    const placed = [];
    const byId = new Map();
    modules.forEach((module) => {
      const size = overviewCardSize(module, profile, false);
      const position = overviewCardPosition(module, profile, false);
      const width = columns === authored
        ? size.width
        : projectOverviewSpan(size.width, authored, columns, 1);
      let x = columns === authored
        ? position.x
        : projectOverviewSpan(position.x, authored, columns, 0);
      x = Math.min(Math.max(0, x), Math.max(0, columns - width));
      let placement = { id: module.id, x, y: position.y, width, height: size.height };
      if (placed.some((entry) => overviewCellsOverlap(placement, entry))) {
        const slot = findOverviewSlot(placed, width, size.height, columns);
        placement = { id: module.id, ...slot, width, height: size.height };
      }
      placed.push(placement);
      byId.set(module.id, placement);
    });
    return { columns, authored, placements: byId };
  }

  function commitOverviewCardSize(id, width, height, renderedColumns = 0) {
    const compact = overviewCompact();
    const profile = activeOverviewProfile();
    if (compact) {
      profile.sizes[id] = { height: clampInt(height, 1, OVERVIEW_MAX_ROWS, 3) };
      saveOverviewLayout();
      return;
    }
    const columns = profile.columns;
    const authoredWidth = renderedColumns && renderedColumns !== columns
      ? clampInt(Math.round(width * columns / renderedColumns), 1, columns, 1)
      : clampInt(width, 1, columns, 1);
    profile.sizes[id] = {
      width: authoredWidth >= columns ? 0 : authoredWidth,
      height: clampInt(height, 1, OVERVIEW_MAX_ROWS, 3),
    };
    saveOverviewLayout();
  }

  function setOverviewColumns(value) {
    const profile = state.overviewLayout.profiles.desktop;
    profile.columns = clampInt(value, 1, OVERVIEW_MAX_COLUMNS, OVERVIEW_DEFAULT_COLUMNS);
    profile.positions = {};
    saveOverviewLayout();
    if (dashboardIdFromView(state.view)) renderHome();
  }

  function moveOverviewCardTo(id, targetId) {
    const profile = activeOverviewProfile();
    const modules = orderedOverviewModules(
      state.modules.filter((module) => module.enabled && module.has_overview),
      profile,
    );
    const from = modules.findIndex((module) => module.id === id);
    const to = modules.findIndex((module) => module.id === targetId);
    if (from < 0 || to < 0 || from === to) return;
    const order = modules.map((module) => module.id);
    order.splice(to, 0, order.splice(from, 1)[0]);
    profile.order = order;
    saveOverviewLayout();
  }

  function resetOverviewLayout() {
    const dashboard = activeDashboard();
    if (!dashboard) return;
    if (overviewCompact()) {
      state.overviewLayout.profiles.mobile = defaultMobileLayout();
    } else {
      state.overviewLayout.profiles.desktop = defaultDesktopLayout();
    }
    dashboard.layout = state.overviewLayout;
    saveOverviewLayout();
    if (dashboardIdFromView(state.view)) renderHome();
  }

  function setOverviewEditing(editing) {
    state.overviewEditing = !!editing;
    renderNav();
    renderHome();
    if (!editing) void loadSharedDashboards({ quiet: true });
  }

  function pageVisible(module) {
    const preference = state.pagePrefs[moduleKey(module)];
    return typeof preference === "boolean" ? preference : module.page_default !== false;
  }

  function setPageVisible(id, visible) {
    state.pagePrefs[id] = visible;
    savePagePrefs(state);
  }

  function pageRank(key) {
    const index = state.pageOrder.indexOf(key);
    return index >= 0 ? index : Number.POSITIVE_INFINITY;
  }

  function orderedPageModules(modules) {
    return modules
      .map((module, index) => ({ module, index, rank: pageRank(moduleKey(module)) }))
      .sort((a, b) => a.rank - b.rank || a.index - b.index)
      .map((entry) => entry.module);
  }

  function orderedPageEntries() {
    const dashboards = state.dashboards.filter((dashboard) => dashboard.id !== OVERVIEW_DASHBOARD_ID);
    const entries = [
      ...dashboards.map((dashboard, index) => ({
        kind: "dashboard",
        dashboard,
        key: dashboardPageKey(dashboard.id),
        fallback: index,
      })),
      ...state.modules
        .filter((module) => module.has_page && module.enabled && pageVisible(module))
        .map((module, index) => ({
          kind: "module",
          module,
          key: moduleKey(module),
          fallback: dashboards.length + index,
        })),
    ];
    return entries.sort((a, b) => pageRank(a.key) - pageRank(b.key) || a.fallback - b.fallback);
  }

  function allPageKeys() {
    const dashboards = state.dashboards.filter((dashboard) => dashboard.id !== OVERVIEW_DASHBOARD_ID);
    const entries = [
      ...dashboards.map((dashboard, index) => ({
        key: dashboardPageKey(dashboard.id),
        fallback: index,
      })),
      ...state.modules
        .filter((module) => module.has_page && module.enabled)
        .map((module, index) => ({
          key: moduleKey(module),
          fallback: dashboards.length + index,
        })),
    ];
    return entries
      .sort((a, b) => pageRank(a.key) - pageRank(b.key) || a.fallback - b.fallback)
      .map((entry) => entry.key);
  }

  function mergeVisiblePageOrder(visibleKeys) {
    const allKeys = allPageKeys();
    const visible = new Set(visibleKeys);
    const queue = [...visibleKeys];
    const merged = allKeys.map((key) => (visible.has(key) ? queue.shift() : key));
    state.pageOrder = merged.concat(queue);
    savePageOrder();
    saveSharedDashboardOrder();
  }

  function bindPageReorder(container) {
    if (!state.overviewEditing) return;
    container.querySelectorAll("[data-page-drag]").forEach((handle) => {
      handle.onclick = (event) => {
        event.preventDefault();
        event.stopPropagation();
      };
      handle.addEventListener("pointerdown", (event) => startPageReorder(event, handle));
    });
  }

  function startPageReorder(event, handle) {
    if (event.button !== undefined && event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const item = handle.closest("[data-page-key]");
    if (!item) return;
    const list = item.parentElement;
    const pageItems = () => [...list.querySelectorAll("[data-page-key]")];
    const originalKeys = pageItems().map((node) => node.dataset.pageKey);
    let changed = false;
    item.classList.add("page-dragging");
    const move = (pointerEvent) => {
      const under = document.elementsFromPoint(pointerEvent.clientX, pointerEvent.clientY)
        .map((node) => node.closest?.("[data-page-key]"))
        .find((node) => node && node !== item && node.parentElement === list);
      if (!under) {
        const first = pageItems().find((node) => node !== item);
        if (first && pointerEvent.clientY < first.getBoundingClientRect().top) {
          if (item.nextElementSibling !== first) {
            list.insertBefore(item, first);
            changed = true;
          }
        }
        return;
      }
      const rect = under.getBoundingClientRect();
      const reference = pointerEvent.clientY > rect.top + rect.height / 2 ? under.nextSibling : under;
      if (reference === item) return;
      list.insertBefore(item, reference);
      changed = true;
    };
    const finish = () => {
      document.removeEventListener("pointermove", move);
      document.removeEventListener("pointerup", finish);
      document.removeEventListener("pointercancel", finish);
      item.classList.remove("page-dragging");
      const nextKeys = pageItems().map((node) => node.dataset.pageKey);
      if (changed && nextKeys.join("\u0000") !== originalKeys.join("\u0000")) {
        mergeVisiblePageOrder(nextKeys);
        renderNav();
      }
    };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", finish);
    document.addEventListener("pointercancel", finish);
  }

  // Icons are module metadata, including metadata received from peers. Keep
  // them on a conservative path shape and always render the URL escaped; a
  // quote in an icon name must never become an HTML/script injection.
  const MODULE_ICON_EXTENSIONS = /\.(svg|png|jpe?g|gif|webp|ico)$/i;
  function safeModuleIcon(module) {
    const icon = String(module?.icon ?? "").trim();
    if (!icon || icon.startsWith("/") || icon.includes("\\")) return "";
    const parts = icon.split("/");
    if (parts.some((part) => !part || part === "." || part === "..")) return "";
    if (!/^[A-Za-z0-9._ /-]+$/.test(icon)) return "";
    if (!MODULE_ICON_EXTENSIONS.test(icon)) return "";
    return icon;
  }
  function moduleIconUrl(module) {
    const icon = safeModuleIcon(module);
    return icon ? moduleSurfaceUrl(module, icon) : "";
  }
  function moduleSurfaceUrl(module, surface = "") {
    const suffix = surface
      ? `/${surface.split("/").map((part) => encodeURIComponent(part)).join("/")}`
      : "/";
    return module.remote_node
      ? `/n/${encodeURIComponent(module.remote_node)}/m/${encodeURIComponent(module.id)}${suffix}`
      : `/m/${encodeURIComponent(module.id)}${suffix}`;
  }

  function overviewEditorTools() {
    const overviewModules = orderedOverviewModules(
      state.modules.filter((module) => module.has_overview && module.enabled),
      activeOverviewProfile(),
    );
    const cardOptions = overviewModules.length
      ? `<div class="overview-picker">${overviewModules.map((module) => `<label class="overview-pick"><input type="checkbox" data-overview-toggle="${esc(moduleKey(module))}" ${overviewVisible(module) ? "checked" : ""}><span>${esc(module.name)}</span></label>`).join("")}</div>`
      : `<span class="small muted">No enabled module defines an overview card.</span>`;
    return `<div class="overview-editor">
      <div class="overview-editor-row"><span class="overview-editor-label">Cards</span>${cardOptions}</div>
    </div>`;
  }

  function overviewCardElements(grid) {
    return new Map(
      [...grid.querySelectorAll("[data-overview-card]")]
        .map((card) => [card.dataset.overviewCard, card]),
    );
  }

  function positionOverviewGrid(grid, modules, profile, compact) {
    if (!grid) return;
    if (compact) {
      grid.style.setProperty("--overview-columns", 1);
      grid.dataset.overviewColumns = "1";
      const cards = overviewCardElements(grid);
      modules.forEach((module) => {
        const size = overviewCardSize(module, profile, true);
        const card = cards.get(module.id);
        if (!card) return;
        card.style.setProperty("--overview-x", 0);
        card.style.setProperty("--overview-y", 0);
        card.style.setProperty("--overview-card-width", 1);
        card.style.setProperty("--overview-card-height", size.height);
        const badge = card.querySelector("[data-overview-size]");
        if (badge) badge.textContent = `${size.height} rows`;
      });
      return;
    }
    const width = grid.getBoundingClientRect().width;
    const gap = parseFloat(getComputedStyle(grid).columnGap) || 13;
    const resolved = resolveOverviewPlacements(modules, width, profile, gap);
    grid.style.setProperty("--overview-columns", resolved.columns);
    grid.dataset.overviewColumns = String(resolved.columns);
    const cards = overviewCardElements(grid);
    for (const [id, placement] of resolved.placements) {
      const card = cards.get(id);
      if (!card) continue;
      card.style.setProperty("--overview-x", placement.x);
      card.style.setProperty("--overview-y", placement.y);
      card.style.setProperty("--overview-card-width", placement.width);
      card.style.setProperty("--overview-card-height", placement.height);
      const badge = card.querySelector("[data-overview-size]");
      if (badge) badge.textContent = `${placement.width} × ${placement.height}`;
    }
  }

  function bindOverviewGridResize(root, modules, profile, compact) {
    if (state.overviewResizeHandler) {
      window.removeEventListener("resize", state.overviewResizeHandler);
      state.overviewResizeHandler = null;
    }
    state.overviewResizeObserver?.disconnect?.();
    state.overviewResizeObserver = null;
    const grid = root.querySelector(".overview-grid");
    if (!grid) return;
    positionOverviewGrid(grid, modules, profile, compact);
    if (typeof ResizeObserver === "function") {
      const observer = new ResizeObserver(() => positionOverviewGrid(grid, modules, profile, compact));
      observer.observe(grid);
      state.overviewResizeObserver = observer;
      return;
    }
    const handler = () => positionOverviewGrid(grid, modules, profile, compact);
    window.addEventListener("resize", handler);
    state.overviewResizeHandler = handler;
  }

  function renderHome() {
    const root = $("coreView");
    const scrollTop = root.scrollTop;
    const editing = state.overviewEditing;
    const compact = overviewCompact();
    const profile = activeOverviewProfile();
    const dashboardName = activeDashboard()?.name || "Overview";
    const overviewModules = state.modules.filter((module) => module.has_overview && module.enabled && moduleUsable(module));
    const modules = orderedOverviewModules(overviewModules.filter(overviewVisible), profile);
    ensureOverviewPositions(modules, profile, compact);
    const authoredColumns = state.overviewLayout.profiles.desktop.columns;
    const overviewNonces = new Map();
    const cards = modules.map((module) => {
      const size = overviewCardSize(module, profile, compact);
      const position = overviewCardPosition(module, profile, compact);
      closeModuleBroker("overview", module.id);
      const nonce = newFrameNonce();
      overviewNonces.set(module.id, nonce);
      const isolated = Boolean(moduleOriginFor(module));
      const src = isolated
        ? moduleFrameSrc(module, "overview", "", nonce)
        : `${moduleSurfaceUrl(module, "overview")}${frameHash(nonce, module.ws_token)}`;
      return `
      <article class="overview-card${editing ? " is-editing" : ""}" style="--overview-x:${position.x};--overview-y:${position.y};--overview-card-width:${size.width};--overview-card-height:${size.height}" data-overview-card="${esc(module.id)}">
        <div class="overview-card-head">
          <div class="overview-card-name">${editing ? `<span class="overview-drag-handle" aria-hidden="true" title="Drag to move">⠿</span>` : ""}<span class="overview-glyph">${moduleIconUrl(module) ? `<img src="${esc(moduleIconUrl(module))}" alt="">` : "◇"}</span><b>${esc(module.name)}</b></div>
          <div class="overview-card-actions">${editing ? `<span class="overview-size-badge" data-overview-size>${compact ? `${size.height} rows` : `${size.width} × ${size.height}`}</span>` : (module.has_page && pageVisible(module) ? `<button class="overview-open" data-open-module="${esc(module.id)}">Open</button>` : "")}</div>
        </div>
        <iframe data-overview-frame="${esc(module.id)}" src="${esc(src)}" title="${esc(module.name)} overview" scrolling="no" allow="clipboard-write; fullscreen" allowfullscreen sandbox="${moduleFrameSandbox(module)}"></iframe>
        ${editing ? `<button type="button" class="overview-resize-handle" data-overview-resize="${esc(module.id)}" aria-label="Resize ${esc(module.name)}" title="Drag to resize">⌟</button>` : ""}
      </article>`;
    }).join("");
    const empty = state.modulesError
      ? `<div class="empty"><h2>Modules unavailable</h2><p>${esc(state.modulesError)}</p></div>`
      : overviewModules.length
        ? `<div class="empty"><h2>No cards shown</h2><p>Choose cards while editing ${esc(dashboardName)}.</p></div>`
        : `<div class="empty"><h2>No overview cards</h2><p>No enabled module defines an overview card.</p></div>`;
    const editbar = editing && overviewModules.length
      ? `<div class="overview-editbar" role="toolbar" aria-label="Edit ${esc(dashboardName)} layout"><span class="overview-edit-hint">${compact ? "Drag card headers to move · drag page tabs in the menu · drag the corner to resize" : "Drag card headers to move · drag page tabs in the sidebar · drag the corner to resize"}</span><div class="overview-edit-actions"><label class="overview-columns">Columns <input type="number" min="1" max="${OVERVIEW_MAX_COLUMNS}" value="${authoredColumns}" id="overviewEditColumns" aria-label="Dashboard grid columns"></label><button type="button" class="button ghost small-button" id="overviewEditReset">Reset</button><button type="button" class="button small-button" id="overviewEditDone">Done</button></div></div>`
      : "";
    const editToggle = overviewModules.length && !editing
      ? `<div class="overview-toolbar"><button type="button" class="overview-edit-button" id="overviewEditToggle" aria-label="Edit ${esc(dashboardName)} layout" title="Edit ${esc(dashboardName)} layout">✎ Edit</button></div>`
      : "";
    const editor = editing && overviewModules.length ? overviewEditorTools() : "";
    root.innerHTML = `<div class="stack overview-stack">${cards ? `<div class="overview-grid" style="--overview-columns:${compact ? 1 : authoredColumns}">${cards}</div>` : empty}${editbar}${editor}${editToggle}</div>`;
    for (const key of [...state.readyFrames]) {
      if (key.startsWith("overview:")) state.readyFrames.delete(key);
    }
    state.overviewFrames = new Map();
    root.querySelectorAll("iframe[data-overview-frame]").forEach((frame) => {
      const id = frame.dataset.overviewFrame;
      frame.dataset.frameNonce = overviewNonces.get(id) || "";
      state.overviewFrames.set(id, frame);
      frame.addEventListener("load", () => {
        const module = state.modules.find((entry) => entry.id === id);
        try {
          frame.contentWindow?.postMessage(
            { type: "orqella:ping", nonce: frame.dataset.frameNonce || "" },
            moduleOriginFor(module) || "*",
          );
        } catch (_) {}
      });
    });
    root.querySelectorAll("[data-open-module]").forEach((button) => button.onclick = () => selectModule(button.dataset.openModule));
    root.querySelectorAll("[data-overview-toggle]").forEach((input) => input.onchange = () => {
      setOverviewVisible(input.dataset.overviewToggle, input.checked);
      renderHome();
    });
    $("overviewEditToggle")?.addEventListener("click", () => setOverviewEditing(true));
    $("overviewEditDone")?.addEventListener("click", () => setOverviewEditing(false));
    $("overviewEditReset")?.addEventListener("click", resetOverviewLayout);
    $("overviewEditColumns")?.addEventListener("change", (event) => setOverviewColumns(event.target.value));
    bindOverviewEditing(root);
    bindOverviewGridResize(root, modules, profile, compact);
    root.scrollTop = scrollTop;
  }

  function bindOverviewEditing(root) {
    if (!state.overviewEditing) return;
    const grid = root.querySelector(".overview-grid");
    if (!grid) return;
    root.querySelectorAll("[data-overview-card]").forEach((card) => {
      const head = card.querySelector(".overview-card-head");
      head?.addEventListener("pointerdown", (event) => {
        if (event.target.closest("[data-overview-resize]")) return;
        startCardDrag(event, card.dataset.overviewCard, grid);
      });
    });
    root.querySelectorAll("[data-overview-resize]").forEach((handle) => {
      handle.addEventListener("pointerdown", (event) => startCardResize(event, handle.dataset.overviewResize, grid));
    });
  }

  function overviewVisiblePlacements(grid) {
    return [...grid.querySelectorAll("[data-overview-card]")].map((card) => ({
      id: card.dataset.overviewCard,
      x: Number(card.style.getPropertyValue("--overview-x")) || 0,
      y: Number(card.style.getPropertyValue("--overview-y")) || 0,
      width: Number(card.style.getPropertyValue("--overview-card-width")) || 1,
      height: Number(card.style.getPropertyValue("--overview-card-height")) || 3,
      card,
    }));
  }

  function overviewGridMetrics(grid) {
    const columns = Number(grid.dataset.overviewColumns)
      || Number(grid.style.getPropertyValue("--overview-columns"))
      || OVERVIEW_DEFAULT_COLUMNS;
    const style = getComputedStyle(grid);
    const gapX = parseFloat(style.columnGap) || 13;
    const gapY = parseFloat(style.rowGap) || 13;
    const rect = grid.getBoundingClientRect();
    const columnWidth = (rect.width - gapX * (columns - 1)) / columns;
    const rowHeight = parseFloat(style.gridAutoRows) || OVERVIEW_ROW_UNIT;
    return { columns, gapX, gapY, rect, columnWidth, rowHeight };
  }

  function overviewIntersectionArea(a, b) {
    const width = Math.min(a.x + a.width, b.x + b.width) - Math.max(a.x, b.x);
    const height = Math.min(a.y + a.height, b.y + b.height) - Math.max(a.y, b.y);
    return width > 0 && height > 0 ? width * height : 0;
  }

  function overviewCanPlace(placements, id, x, y, width, height, columns) {
    if (x < 0 || y < 0 || width < 1 || height < 1) return false;
    if (x + width > columns) return false;
    const candidate = { x, y, width, height };
    return !placements.some((entry) => entry.id !== id && overviewCellsOverlap(candidate, entry));
  }

  function overviewMaxWidth(placements, id, x, y, proposed, height, columns) {
    for (let width = proposed; width >= 1; width -= 1) {
      if (overviewCanPlace(placements, id, x, y, width, height, columns)) return width;
    }
    return 1;
  }

  function overviewMaxHeight(placements, id, x, y, width, proposed, columns) {
    for (let height = proposed; height >= 1; height -= 1) {
      if (overviewCanPlace(placements, id, x, y, width, height, columns)) return height;
    }
    return 1;
  }

  function overviewDropPlacement(placements, id, target, width, height, original, columns) {
    const candidate = { x: target.x, y: target.y, width, height };
    const overlaps = placements.filter((entry) => entry.id !== id && overviewCellsOverlap(candidate, entry));
    if (!overlaps.length) return { kind: "move", target };
    const other = overlaps.sort((a, b) =>
      overviewIntersectionArea(b, candidate) - overviewIntersectionArea(a, candidate)
    )[0];
    const others = placements.filter((entry) => entry.id !== id && entry.id !== other.id);
    const fits = overviewCanPlace(others, id, other.x, other.y, width, height, columns)
      && overviewCanPlace(others, other.id, original.x, original.y, other.width, other.height, columns);
    if (fits) return { kind: "swap", target: { x: other.x, y: other.y }, other };
    return { kind: "invalid", target };
  }

  function overviewPreviewPosition(preview, target, width, height, metrics) {
    preview.style.left = `${target.x * (metrics.columnWidth + metrics.gapX)}px`;
    preview.style.top = `${target.y * (metrics.rowHeight + metrics.gapY)}px`;
    preview.style.width = `${width * metrics.columnWidth + (width - 1) * metrics.gapX}px`;
    preview.style.height = `${height * metrics.rowHeight + (height - 1) * metrics.gapY}px`;
  }

  function overviewMergeVisibleOrder(visibleIds) {
    const profile = activeOverviewProfile();
    const all = orderedOverviewModules(
      state.modules.filter((module) => module.has_overview && module.enabled),
      profile,
    ).map((module) => module.id);
    const visible = new Set(visibleIds);
    const queue = [...visibleIds];
    const merged = all.map((id) => (visible.has(id) ? queue.shift() : id));
    profile.order = merged.concat(queue);
  }

  function overviewSyncOrderFromPositions() {
    const profile = state.overviewLayout.profiles.desktop;
    const visible = state.modules
      .filter((module) => module.has_overview && module.enabled && overviewVisible(module))
      .slice()
      .sort((a, b) => {
        const pa = overviewCardPosition(a, profile, false);
        const pb = overviewCardPosition(b, profile, false);
        return pa.y - pb.y || pa.x - pb.x;
      });
    overviewMergeVisibleOrder(visible.map((module) => module.id));
  }

  function overviewMobileDropTarget(grid, card, clientX, clientY) {
    const cards = [...grid.querySelectorAll("[data-overview-card]")].filter((entry) => entry !== card);
    if (!cards.length) return null;
    const under = document.elementsFromPoint(clientX, clientY)
      .map((node) => node.closest?.("[data-overview-card]"))
      .find((node) => node && node !== card);
    let target = under;
    if (!target) {
      target = cards.reduce((best, entry) => {
        const rect = entry.getBoundingClientRect();
        const distance = Math.abs(clientY - (rect.top + rect.height / 2));
        return !best || distance < best.distance ? { entry, distance } : best;
      }, null)?.entry;
    }
    if (!target) return null;
    const rect = target.getBoundingClientRect();
    return { target, before: clientY < rect.top + rect.height / 2 };
  }

  function startCardDrag(event, id, grid) {
    if (event.button !== undefined && event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const card = [...grid.querySelectorAll("[data-overview-card]")].find((entry) => entry.dataset.overviewCard === id);
    if (!card) return;
    const placements = overviewVisiblePlacements(grid);
    const self = placements.find((entry) => entry.id === id);
    if (!self) return;
    const compact = overviewCompact();
    const profile = activeOverviewProfile();
    const original = { x: self.x, y: self.y };
    const originalAuthored = overviewCardPosition({ id }, profile, compact);
    const rect = card.getBoundingClientRect();
    const offsetX = event.clientX - rect.left;
    const offsetY = event.clientY - rect.top;
    const clone = card.cloneNode(true);
    clone.querySelectorAll("iframe, [data-overview-resize]").forEach((node) => node.remove());
    clone.removeAttribute("data-overview-card");
    clone.classList.remove("is-editing", "overview-dragging");
    clone.classList.add("overview-drag-clone");
    clone.style.left = `${rect.left}px`;
    clone.style.top = `${rect.top}px`;
    clone.style.width = `${rect.width}px`;
    clone.style.height = `${rect.height}px`;
    document.body.appendChild(clone);
    card.classList.add("overview-dragging");
    const preview = compact ? null : document.createElement("div");
    if (preview) {
      preview.className = "overview-drop-preview";
      grid.appendChild(preview);
    }
    const line = compact ? document.createElement("div") : null;
    if (line) {
      line.className = "overview-drop-line";
      grid.appendChild(line);
    }
    let swapCard = null;
    let dropPlacement = null;
    let dropLine = null;
    const clearSwap = () => {
      if (!swapCard) return;
      swapCard.classList.remove("overview-swap-target");
      swapCard.style.transform = "";
      swapCard = null;
    };
    const showSwap = (placement) => {
      if (placement.kind !== "swap") {
        clearSwap();
        return;
      }
      const next = placement.other.card
        || [...grid.querySelectorAll("[data-overview-card]")]
          .find((entry) => entry.dataset.overviewCard === placement.other.id)
        || null;
      if (!next) {
        clearSwap();
        return;
      }
      // Keep the same swap preview in place. Removing and re-adding the
      // transform on every pointermove restarts its CSS transition, which
      // reads as rapid flicker even when the drop target has not changed.
      if (next === swapCard) return;
      clearSwap();
      swapCard = next;
      const otherRect = next.getBoundingClientRect();
      next.classList.add("overview-swap-target");
      next.style.transform = `translate(${rect.left - otherRect.left}px, ${rect.top - otherRect.top}px)`;
    };
    const move = (pointerEvent) => {
      clone.style.left = `${pointerEvent.clientX - offsetX}px`;
      clone.style.top = `${pointerEvent.clientY - offsetY}px`;
      if (compact) {
        dropLine = overviewMobileDropTarget(grid, card, pointerEvent.clientX, pointerEvent.clientY);
        if (!dropLine) {
          line.style.display = "none";
          return;
        }
        const gridRect = grid.getBoundingClientRect();
        const targetRect = dropLine.target.getBoundingClientRect();
        line.style.display = "";
        line.style.top = `${targetRect.top - gridRect.top + (dropLine.before ? 0 : targetRect.height)}px`;
        return;
      }
      const metrics = overviewGridMetrics(grid);
      if (!Number.isFinite(metrics.columnWidth) || metrics.columnWidth <= 0) return;
      const target = {
        x: clampInt(Math.round((parseFloat(clone.style.left) - metrics.rect.left) / (metrics.columnWidth + metrics.gapX)), 0, Math.max(0, metrics.columns - self.width), 0),
        y: Math.max(0, Math.round((parseFloat(clone.style.top) - metrics.rect.top) / (metrics.rowHeight + metrics.gapY))),
      };
      dropPlacement = overviewDropPlacement(placements, id, target, self.width, self.height, original, metrics.columns);
      showSwap(dropPlacement);
      preview.classList.toggle("is-invalid", dropPlacement.kind === "invalid");
      overviewPreviewPosition(
        preview,
        dropPlacement.kind === "swap" ? dropPlacement.target : target,
        self.width,
        self.height,
        metrics,
      );
    };
    const finish = () => {
      document.removeEventListener("pointermove", move);
      document.removeEventListener("pointerup", finish);
      document.removeEventListener("pointercancel", finish);
      clone.remove();
      preview?.remove();
      line?.remove();
      card.classList.remove("overview-dragging");
      clearSwap();
      if (compact) {
        if (!dropLine) return;
        const cards = [...grid.querySelectorAll("[data-overview-card]")];
        const ordered = cards.map((entry) => entry.dataset.overviewCard).filter((entry) => entry !== id);
        const targetIndex = ordered.indexOf(dropLine.target.dataset.overviewCard);
        if (targetIndex < 0) return;
        ordered.splice(dropLine.before ? targetIndex : targetIndex + 1, 0, id);
        const originalOrder = cards.map((entry) => entry.dataset.overviewCard);
        if (ordered.join("\u0000") === originalOrder.join("\u0000")) return;
        overviewMergeVisibleOrder(ordered);
        saveOverviewLayout();
        renderHome();
        return;
      }
      if (!dropPlacement || dropPlacement.kind === "invalid") return;
      if (dropPlacement.kind === "move"
        && dropPlacement.target.x === original.x
        && dropPlacement.target.y === original.y) return;
      const metrics = overviewGridMetrics(grid);
      const renderedColumns = metrics.columns || profile.columns;
      const authoredColumns = profile.columns;
      const toAuthoredX = (x) => (renderedColumns === authoredColumns
        ? x
        : clampInt(Math.round(x * authoredColumns / renderedColumns), 0, authoredColumns - 1, 0));
      const positions = profile.positions || {};
      if (dropPlacement.kind === "swap") {
        positions[dropPlacement.other.id] = originalAuthored;
      }
      const size = overviewCardSize(
        state.modules.find((module) => module.id === id) || { id },
        profile,
        false,
      );
      const targetX = toAuthoredX(dropPlacement.target.x);
      positions[id] = {
        x: Math.min(targetX, Math.max(0, authoredColumns - size.width)),
        y: dropPlacement.target.y,
      };
      profile.positions = positions;
      const visibleModules = orderedOverviewModules(
        state.modules.filter((module) => module.has_overview && module.enabled && overviewVisible(module)),
        profile,
      );
      ensureOverviewPositions(visibleModules, profile, false);
      overviewSyncOrderFromPositions();
      saveOverviewLayout();
      renderHome();
    };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", finish);
    document.addEventListener("pointercancel", finish);
  }

  function startCardResize(event, id, grid) {
    if (event.button !== undefined && event.button !== 0) return;
    event.preventDefault();
    event.stopPropagation();
    const card = [...grid.querySelectorAll("[data-overview-card]")].find((entry) => entry.dataset.overviewCard === id);
    if (!card) return;
    const handle = event.currentTarget;
    const compact = overviewCompact();
    const metrics = overviewGridMetrics(grid);
    const columns = metrics.columns;
    const placements = overviewVisiblePlacements(grid);
    const self = placements.find((entry) => entry.id === id);
    if (!self || !Number.isFinite(metrics.columnWidth) || metrics.columnWidth <= 0) return;
    const startX = event.clientX;
    const startY = event.clientY;
    const startWidth = self.width;
    const startHeight = self.height;
    let width = startWidth;
    let height = startHeight;
    const badge = card.querySelector("[data-overview-size]");
    card.classList.add("overview-resizing");
    const move = (pointerEvent) => {
      let proposedWidth = compact
        ? startWidth
        : clampInt(startWidth + Math.round((pointerEvent.clientX - startX) / (metrics.columnWidth + metrics.gapX)), 1, columns, startWidth);
      let proposedHeight = clampInt(startHeight + Math.round((pointerEvent.clientY - startY) / (metrics.rowHeight + metrics.gapY)), 1, OVERVIEW_MAX_ROWS, startHeight);
      if (!compact) {
        proposedWidth = overviewMaxWidth(placements, id, self.x, self.y, proposedWidth, startHeight, columns);
        proposedHeight = overviewMaxHeight(placements, id, self.x, self.y, proposedWidth, proposedHeight, columns);
      }
      width = proposedWidth;
      height = proposedHeight;
      card.style.setProperty("--overview-card-width", width);
      card.style.setProperty("--overview-card-height", height);
      if (badge) badge.textContent = compact ? `${height} rows` : `${width} × ${height}`;
    };
    const finish = () => {
      document.removeEventListener("pointermove", move);
      document.removeEventListener("pointerup", finish);
      document.removeEventListener("pointercancel", finish);
      card.classList.remove("overview-resizing");
      commitOverviewCardSize(id, width, height, columns);
      renderHome();
    };
    document.addEventListener("pointermove", move);
    document.addEventListener("pointerup", finish);
    document.addEventListener("pointercancel", finish);
  }

  async function toggleModule(id, enabled) {
    const node = state.node || "local";
    try {
      await api(`/api/v1/nodes/${encodeURIComponent(node)}/modules/${encodeURIComponent(id)}/${enabled ? "enable" : "disable"}`, { method: "POST", body: "{}" });
      await refresh();
      toast(enabled ? "Module enabled" : "Module disabled", id, "ok");
    } catch (error) { toast("Could not update module", error.message, "bad"); }
  }

  function sortedSettingsModules() {
    const modules = state.node === "local" ? state.localModules : state.modules;
    return [...(Array.isArray(modules) ? modules : [])]
      .sort((a, b) => (a.name || a.id).localeCompare(b.name || b.id));
  }

  function ensureSelectedSettingsModule() {
    const modules = sortedSettingsModules();
    if (!modules.some((module) => module.id === state.settingsModule)) {
      state.settingsModule = modules[0]?.id || null;
    }
    return modules;
  }

  global.OrqellaDashboard = {
    install,
    renderHome,
    bindOverviewEditing,
    bindPageReorder,
    toggleModule,
    sortedSettingsModules,
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
    savePageOrder,
    allPageKeys,
    orderedPageEntries,
    moduleIconUrl,
    moduleSurfaceUrl,
  };
})(typeof window !== "undefined" ? window : globalThis);
