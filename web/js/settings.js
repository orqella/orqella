(function (global) {
  "use strict";

  let state;
  let $;
  let esc;
  let api;
  let toast;
  let render;
  let renderNav;
  let refresh;
  let selectModule;
  let saveViewState;
  let moduleId;
  let frameKey;
  let renderHome;
  let saveSharedDashboards;
  let savePageOrder;
  let dashboardIdFromView;
  let selectDashboard;
  let activateDashboard;
  let selectView;
  let setPageVisible;
  let dashboardPageKey;
  let newDashboardId;
  let defaultDashboardLayout;
  let sendContext;
  let sendSettingsContext;
  let sendThemeContext;
  let toggleModule;
  let sortedSettingsModules;
  let ensureSelectedSettingsModule;
  let nativeDisconnect;
  let nativeKeepScreenOn;
  let newFrameNonce;
  let frameHash;
  let OVERVIEW_DASHBOARD_ID;
  let moduleKey;
  let pageVisible;
  let openPermissions;
  let moduleFrameSrc;
  let moduleFrameSandbox;
  let moduleUsable;
  let moduleOriginFor;
  let closeModuleBroker;
  let linkSignInFlow = 0;
  let themeAgentJob = null;
  let themeAgentTimer = null;
  let moduleIndexData = null;
  let moduleIndexNode = null;

  function nodeApi(path, node = state.node) {
    return `/api/v1/nodes/${encodeURIComponent(node || "local")}${path}`;
  }

  function selectedModules() {
    const modules = state.node === "local" ? state.localModules : state.modules;
    return Array.isArray(modules) ? modules : [];
  }

  function adminModuleApi(id, suffix = "") {
    return nodeApi(`/modules/${encodeURIComponent(id)}${suffix}`);
  }

  function install(ctx) {
    ({
      state, $, esc, api, toast, render, renderNav, refresh, selectModule,
      saveViewState, moduleId, frameKey, renderHome, saveSharedDashboards,
      savePageOrder, dashboardIdFromView, selectDashboard, activateDashboard,
      selectView, setPageVisible, dashboardPageKey, newDashboardId,
      defaultDashboardLayout, sendContext, sendSettingsContext, sendThemeContext,
      toggleModule, sortedSettingsModules, ensureSelectedSettingsModule,
      nativeDisconnect, nativeKeepScreenOn, newFrameNonce, frameHash, OVERVIEW_DASHBOARD_ID,
      moduleKey, pageVisible, openPermissions,
      moduleFrameSrc, moduleFrameSandbox, moduleUsable, moduleOriginFor, closeModuleBroker,
    } = ctx);
    // Native shells push external changes (system Settings, native sheets)
    // back into the open Mobile card so the checkbox cannot go stale.
    window.addEventListener("orqella-keep-screen-on", (event) => {
      const toggle = document.getElementById("keepScreenOnToggle");
      if (toggle) toggle.checked = event.detail === true;
    });
  }

  // The first-party Agent Harness module is the interactive agent workspace.
  // Its settings live in Settings → Agents instead of the generic module page.
  const AGENT_HARNESS_ID = "agent-harness";
  const AGENTS_TAB = "agents";

  function agentHarnessModule() {
    return state.localModules.find((module) => module.id === AGENT_HARNESS_ID) || null;
  }

  function renderSettings() {
    const root = $("coreView");
    const settingsTabs = new Set(["general", "agents", "dashboards", "modules"]);
    if (!settingsTabs.has(state.settingsTab)) {
      const legacyModule = selectedModules().find((module) => module.id === state.settingsTab);
      if (legacyModule) {
        state.settingsModule = legacyModule.id;
        state.settingsTab = "modules";
      } else {
        state.settingsTab = "general";
      }
    }
    const pending = selectedModules().reduce((total, module) => total + (module.pending_permissions?.length || 0), 0);
    const tabs = [
      { id: "general", label: "General" },
      { id: "agents", label: "Agents" },
      { id: "dashboards", label: "Dashboards" },
      { id: "modules", label: "Modules", pending },
    ];
    root.innerHTML = `<div class="stack"><nav class="settings-tabs" aria-label="Settings sections">${tabs.map((tab) => `
      <button class="settings-tab ${state.settingsTab === tab.id ? "active" : ""}" data-settings-tab="${esc(tab.id)}">${esc(tab.label)}${tab.pending ? `<span class="tab-count">${tab.pending}</span>` : ""}</button>`).join("")}</nav><div id="settingsPanel"></div></div>`;
    root.querySelectorAll("[data-settings-tab]").forEach((button) => button.onclick = () => {
      state.settingsTab = button.dataset.settingsTab;
      renderSettings();
    });
    if (state.settingsTab !== AGENTS_TAB && state.agentRegistryHandler) {
      window.removeEventListener("message", state.agentRegistryHandler);
      state.agentRegistryHandler = null;
      state.agentRegistry = null;
      clearTimeout(state.agentRegistryTimer);
      state.agentRegistryTimer = null;
    }
    if (state.settingsTab === "general") renderSettingsGeneral();
    else if (state.settingsTab === AGENTS_TAB) renderSettingsAgents();
    else if (state.settingsTab === "dashboards") renderSettingsDashboards();
    else renderSettingsModules();
  }

  function renderSettingsModules() {
    const panel = $("settingsPanel");
    if (!panel) return;
    state.settingsFrames = new Map();
    const modules = ensureSelectedSettingsModule();
    const options = modules.map((module) => {
      const pending = module.pending_permissions?.length || 0;
      return `<option value="${esc(module.id)}" ${module.id === state.settingsModule ? "selected" : ""}>${esc(module.name)}${pending ? ` · ${pending} pending` : ""}</option>`;
    }).join("");
    panel.innerHTML = `<div class="stack">
      <article class="card">
        <label class="settings-module-picker" for="settingsModuleSelect">
          <span class="form-label">Module</span>
          <select class="field" id="settingsModuleSelect" ${modules.length ? "" : "disabled"}>${options || '<option value="">No modules installed</option>'}</select>
        </label>
        <p class="small muted" style="margin:10px 0 0">Managing modules on <b>${esc(machineName(state.node))}</b>.</p>
      </article>
      <div id="settingsModulePanel"></div>
    </div>`;
    const select = $("settingsModuleSelect");
    if (select) select.onchange = () => {
      state.settingsModule = select.value;
      renderSettingsModules();
    };
    if (!state.settingsModule) {
      $("settingsModulePanel").innerHTML = `<div class="empty"><h2>No modules installed</h2><p>Install a module from General or build one from the Build page.</p></div>`;
      return;
    }
    renderSettingsModule(state.settingsModule, $("settingsModulePanel"));
  }

  function dashboardCardCount(dashboard) {
    return state.modules.filter((module) => module.has_overview && module.enabled).filter((module) => {
      const preference = dashboard.visible[moduleKey(module)];
      if (typeof preference === "boolean") return preference;
      return dashboard.defaultVisible === true && module.overview_default !== false;
    }).length;
  }

  function renderSettingsDashboards() {
    const panel = $("settingsPanel");
    if (!panel) return;
    const dashboards = state.dashboards.filter((dashboard) => dashboard.id !== OVERVIEW_DASHBOARD_ID);
    const rows = dashboards.map((dashboard) => `
      <div class="dashboard-settings-row" data-dashboard-row="${esc(dashboard.id)}">
        <input class="field" data-dashboard-name="${esc(dashboard.id)}" value="${esc(dashboard.name)}" maxlength="60" aria-label="Dashboard name">
        <span class="chip">${dashboardCardCount(dashboard)} card${dashboardCardCount(dashboard) === 1 ? "" : "s"}</span>
        <button class="button ghost small-button" type="button" data-dashboard-rename="${esc(dashboard.id)}">Rename</button>
        <button class="button ghost small-button" type="button" data-dashboard-delete="${esc(dashboard.id)}">Delete</button>
      </div>`).join("");
    panel.innerHTML = `<div class="stack">
      <article class="card">
        <div class="row spread"><h2>Dashboards</h2><span class="small muted">${dashboards.length}</span></div>
        <p class="small muted" style="margin:8px 0 14px">Dashboards, their cards, and their layouts are shared with every client connected to this machine. The desktop grid and the phone card order are saved separately, and each screen resolves the grid to its own width. Overview opens from the Orqella brand in the top-left.</p>
        <div class="dashboard-settings-list">${rows || `<div class="empty small">No custom dashboards yet.</div>`}</div>
        <div class="dashboard-settings-create">
          <input class="field" id="newDashboardName" maxlength="60" placeholder="New dashboard name" autocomplete="off">
          <button class="button" type="button" id="createDashboard">Create dashboard</button>
        </div>
      </article>
    </div>`;
    panel.querySelectorAll("[data-dashboard-rename]").forEach((button) => button.onclick = () => {
      const row = button.closest("[data-dashboard-row]");
      const dashboard = state.dashboards.find((entry) => entry.id === button.dataset.dashboardRename);
      const name = row?.querySelector("[data-dashboard-name]")?.value.trim() || "";
      if (!dashboard) return;
      if (!name) {
        toast("Name required", "Give the dashboard a name.", "bad");
        return;
      }
      if (state.dashboards.some((entry) => entry.id !== dashboard.id && entry.name.toLocaleLowerCase() === name.toLocaleLowerCase())) {
        toast("Name already used", name, "bad");
        return;
      }
      dashboard.name = name.slice(0, 60);
      saveSharedDashboards();
      renderNav();
      if (dashboardIdFromView(state.view) === dashboard.id) renderHome();
      renderSettings();
      toast("Dashboard renamed", dashboard.name, "ok");
    });
    panel.querySelectorAll("[data-dashboard-delete]").forEach((button) => button.onclick = () => {
      const dashboard = state.dashboards.find((entry) => entry.id === button.dataset.dashboardDelete);
      if (!dashboard || dashboard.protected) return;
      if (!window.confirm(`Delete the “${dashboard.name}” dashboard? It will be removed for every client connected to this machine.`)) return;
      state.dashboards = state.dashboards.filter((entry) => entry.id !== dashboard.id);
      state.pageOrder = state.pageOrder.filter((key) => key !== dashboardPageKey(dashboard.id));
      savePageOrder();
      if (dashboardIdFromView(state.view) === dashboard.id) {
        state.view = "home";
        state.module = null;
        state.modulePath = "";
        activateDashboard(OVERVIEW_DASHBOARD_ID);
        saveViewState();
      }
      saveSharedDashboards();
      render();
      toast("Dashboard deleted", dashboard.name, "ok");
    });
    $("createDashboard").onclick = () => {
      const input = $("newDashboardName");
      const name = input.value.trim();
      if (!name) {
        toast("Name required", "Give the dashboard a name.", "bad");
        return;
      }
      if (state.dashboards.some((entry) => entry.name.toLocaleLowerCase() === name.toLocaleLowerCase())) {
        toast("Name already used", name, "bad");
        return;
      }
      const dashboard = {
        id: newDashboardId(),
        name: name.slice(0, 60),
        protected: false,
        defaultVisible: false,
        layout: defaultDashboardLayout(),
        visible: {},
      };
      state.dashboards.push(dashboard);
      state.pageOrder.push(dashboardPageKey(dashboard.id));
      saveSharedDashboards();
      savePageOrder();
      selectDashboard(dashboard.id);
      toast("Dashboard created", dashboard.name, "ok");
    };
    $("newDashboardName")?.addEventListener("keydown", (event) => {
      if (event.key === "Enter") {
        event.preventDefault();
        $("createDashboard")?.click();
      }
    });
  }

  async function applyTheme(themeId) {
    const selected = state.info?.themes?.find((theme) => theme.id === themeId);
    if (!selected) throw new Error("Unknown theme");
    const oldLink = document.getElementById("themeStylesheet");
    if (!oldLink) throw new Error("Theme stylesheet is missing");
    const link = oldLink.cloneNode();
    link.id = "themeStylesheet";
    link.href = `/assets/theme.css?theme=${encodeURIComponent(themeId)}&v=${Date.now()}`;
    await new Promise((resolve, reject) => {
      link.onload = resolve;
      link.onerror = () => reject(new Error("Could not load the selected theme"));
      document.head.appendChild(link);
    });
    oldLink.remove();
    state.info.config.theme = themeId;
    document.documentElement.style.colorScheme = selected.color_scheme || "dark";
    sendThemeContext();
  }

  function syncThemePicker(themeId) {
    const select = $("themeSelect");
    const themes = state.info?.themes || [];
    if (select) {
      select.innerHTML = themes.map((theme) => `<option value="${esc(theme.id)}" ${theme.id === themeId ? "selected" : ""}>${esc(theme.name)}</option>`).join("");
      select.value = themeId;
      select.disabled = false;
    }
  }

  function themeAgentActive(job) {
    return job?.status === "queued" || job?.status === "running";
  }

  function updateThemeAgentUi() {
    const button = $("generateTheme");
    const status = $("themeAgentStatus");
    const select = $("themeSelect");
    if (!button && !status) return;
    const job = themeAgentJob;
    if (select) select.disabled = themeAgentActive(job);
    if (!job) {
      if (button) button.disabled = false;
      if (status) status.textContent = "";
      return;
    }
    if (button) button.disabled = themeAgentActive(job);
    if (!status) return;
    if (job.status === "queued") status.textContent = "Queued…";
    else if (job.status === "running") status.textContent = "Working…";
    else if (job.status === "succeeded") status.textContent = "Theme created.";
    else if (job.status === "failed") status.textContent = `Failed: ${job.error || "unknown error"}`;
    else status.textContent = job.status || "";
  }

  async function pollThemeAgent() {
    const job = themeAgentJob;
    if (!job || !themeAgentActive(job)) return;
    try {
      const result = await api(`/api/v1/config/theme/agent/${encodeURIComponent(job.id)}`);
      if (!themeAgentJob || themeAgentJob.id !== job.id) return;
      themeAgentJob = { ...result, themeId: job.themeId, themeName: job.themeName };
      updateThemeAgentUi();
      if (result.status === "succeeded") {
        const themeId = themeAgentJob.themeId;
        if (!themeId) {
          toast("Theme created", "Reload the page to see the new theme.", "ok");
          return;
        }
        try {
          const info = await api("/api/v1/info");
          state.info.themes = info.themes || state.info.themes;
          state.info.theme_path = info.theme_path || state.info.theme_path;
          await api("/api/v1/config/theme", {
            method: "POST",
            body: JSON.stringify({ theme: themeId }),
          });
          syncThemePicker(themeId);
          await applyTheme(themeId);
          const name = state.info.themes?.find((theme) => theme.id === themeId)?.name;
          toast("Theme created", `${name || themeAgentJob.themeName || "The new theme"} is selected.`, "ok");
        } catch (error) {
          const status = $("themeAgentStatus");
          if (status) status.textContent = "Theme created, but selecting it failed.";
          toast("Theme created, but selecting it failed", error.message, "bad");
        }
        return;
      }
      if (result.status === "failed") {
        toast("Theme agent failed", result.error || "unknown error", "bad");
        return;
      }
      themeAgentTimer = setTimeout(pollThemeAgent, 2000);
    } catch (error) {
      if (!themeAgentJob || themeAgentJob.id !== job.id) return;
      const status = $("themeAgentStatus");
      if (status) status.textContent = error.message;
      themeAgentTimer = setTimeout(pollThemeAgent, 4000);
    }
  }

  async function generateTheme() {
    const prompt = $("themePrompt").value.trim();
    if (!prompt) {
      $("themePrompt").focus();
      return;
    }
    if (themeAgentTimer) clearTimeout(themeAgentTimer);
    themeAgentTimer = null;
    $("generateTheme").disabled = true;
    $("themeAgentStatus").textContent = "Starting…";
    try {
      const result = await api("/api/v1/config/theme/agent", {
        method: "POST",
        body: JSON.stringify({ prompt }),
      });
      themeAgentJob = {
        ...result.job,
        themeId: result.theme_id,
        themeName: result.theme_name,
      };
      $("themePrompt").value = "";
      updateThemeAgentUi();
      pollThemeAgent();
      toast("Theme agent started", "The new theme will appear in the picker.", "ok");
    } catch (error) {
      $("generateTheme").disabled = false;
      $("themeAgentStatus").textContent = error.message;
      toast("Could not start theme agent", error.message, "bad");
    }
  }

  function moduleIndexMeta(module) {
    const parts = [];
    if (module.publisher?.name) parts.push(module.publisher.name);
    if (module.category) parts.push(module.category);
    if (module.installed_version) parts.push(`Installed v${module.installed_version}`);
    if (module.update_available) parts.push("Update available");
    return parts;
  }

  function moduleIndexPlatforms(module) {
    if (Array.isArray(module.platforms) && module.platforms.length) {
      return module.platforms.join(", ");
    }
    return "All platforms";
  }

  function renderModuleIndex() {
    const list = $("moduleIndexList");
    const status = $("moduleIndexStatus");
    if (!list || !moduleIndexData) return;
    const modules = Array.isArray(moduleIndexData.modules) ? moduleIndexData.modules : [];
    const query = ($("moduleSearch")?.value || "").trim().toLowerCase();
    const filtered = modules.filter((module) => {
      if (!query) return true;
      return [module.id, module.name, module.description, module.category, module.publisher?.name, ...(module.tags || [])]
        .filter(Boolean)
        .some((value) => String(value).toLowerCase().includes(query));
    });
    if (status) {
      status.textContent = moduleIndexData
        ? `${modules.length} module${modules.length === 1 ? "" : "s"} · ${moduleIndexData.platform || "current host"} · ${machineName(moduleIndexNode || state.node)}`
        : "";
    }
    if (!filtered.length) {
      list.innerHTML = `<p class="small muted" style="padding:12px">${modules.length ? "No modules match this search." : "The module index is empty."}</p>`;
      return;
    }
    list.innerHTML = filtered.map((module) => {
      const indexManaged = String(module.installed_source || "").startsWith("index:");
      let action = "Install";
      let disabled = !module.compatible;
      if (module.installed) {
        if (module.update_available) {
          action = "Update";
        } else if (!indexManaged) {
          action = "Replace";
        } else {
          action = "Installed";
          disabled = true;
        }
      }
      if (!module.compatible) action = "Unavailable";
      const permissions = (module.permissions || []).map((permission) =>
        `<span class="chip ${permission === "backend" ? "warn" : ""}">${esc(permission)}</span>`
      ).join("");
      return `<article class="module-index-item ${module.compatible ? "" : "unavailable"}" data-index-id="${esc(module.id)}">
        <div class="module-index-main">
          <div class="module-index-title"><h3>${esc(module.name)}</h3>${module.latest_version ? `<span class="chip">v${esc(module.latest_version)}</span>` : ""}${module.official ? '<span class="chip ok">official</span>' : ""}${module.compiled_backend ? '<span class="chip">compiled</span>' : ""}${module.compatible ? "" : '<span class="chip bad">incompatible</span>'}</div>
          <p>${esc(module.description)}</p>
          <div class="module-index-meta">${moduleIndexMeta(module).map((part) => `<span>${esc(part)}</span>`).join("")}<span>${esc(moduleIndexPlatforms(module))}</span></div>
          ${permissions ? `<div class="module-index-permissions">${permissions}</div>` : ""}
          ${module.compatible ? "" : `<div class="small muted" style="margin-top:6px">${esc(module.incompatible_reason || "Not available for this platform.")}</div>`}
        </div>
        <div class="module-index-action">
          <button class="button ${action === "Update" || action === "Install" ? "" : "ghost"}" type="button" data-install-index="${esc(module.id)}" ${disabled ? "disabled" : ""}>${action}</button>
          ${module.source_type ? `<span class="small muted">${module.source_type === "archive" ? "release" : "git"}</span>` : ""}
        </div>
      </article>`;
    }).join("");
    list.querySelectorAll("[data-install-index]").forEach((button) => {
      const module = modules.find((entry) => entry.id === button.dataset.installIndex);
      if (!module) return;
      button.onclick = () => installIndexModule(module, button);
    });
  }

  async function loadModuleIndex() {
    const list = $("moduleIndexList");
    const status = $("moduleIndexStatus");
    if (!list) return;
    const node = state.node || "local";
    moduleIndexData = null;
    moduleIndexNode = node;
    list.setAttribute("aria-busy", "true");
    if (status) status.textContent = "Loading…";
    try {
      const data = await api(nodeApi("/module-index", node));
      if ((state.node || "local") !== node) return;
      moduleIndexData = data;
      renderModuleIndex();
    } catch (error) {
      if ((state.node || "local") !== node) return;
      list.innerHTML = `<div class="empty" style="padding:16px"><h3>Module index unavailable</h3><p>${esc(error.message)}</p><p class="small muted">Check the index URL in config.toml on ${esc(machineName(node))}.</p></div>`;
      if (status) status.textContent = "Unavailable";
    } finally {
      if ((state.node || "local") === node) list.removeAttribute("aria-busy");
    }
  }

  async function installIndexModule(module, button) {
    if (!module?.id) return;
    const node = state.node || "local";
    const indexManaged = String(module.installed_source || "").startsWith("index:");
    if (module.installed && !indexManaged && !module.update_available) {
      const replace = confirm(`Replace ${module.name} v${module.installed_version} with v${module.latest_version} from the module index on ${machineName(node)}?`);
      if (!replace) return;
    }
    const original = button.textContent;
    button.disabled = true;
    button.textContent = "Installing…";
    try {
      const result = await api(nodeApi("/modules/install", node), {
        method: "POST",
        body: JSON.stringify({ index_id: module.id, index_version: module.latest_version || "" }),
      });
      if ((state.node || "local") !== node) return;
      await refresh();
      if (result?.module?.id) {
        state.settingsTab = "modules";
        state.settingsModule = result.module.id;
        renderSettings();
      }
      if (result?.sidecar_error) {
        toast("Module installed with a setup error", result.sidecar_error, "bad");
      } else {
        toast(`${module.update_available ? "Module updated" : "Module installed"} on ${machineName(node)}`, result?.module?.name || module.name, "ok");
      }
    } catch (error) {
      button.disabled = false;
      button.textContent = original;
      toast("Install failed", error.message, "bad");
    }
  }

  function renderSettingsGeneral() {
    const panel = $("settingsPanel");
    const config = state.info?.config || {};
    const themePath = state.info?.theme_path || "theme.css";
    const themes = state.info?.themes?.length ? state.info.themes : [
      { id: "nocturne", name: "Nocturne" },
    ];
    const selectedTheme = themes.find((theme) => theme.id === config.theme) || themes[0];
    const disconnectTunnel = nativeDisconnect();
    const keepScreenOn = nativeKeepScreenOn();
    const mobileSettings = (disconnectTunnel || keepScreenOn)
      ? `<article class="card flat settings-section" id="mobileSettings"><div class="card-title settings-section-head"><h2>Mobile</h2></div><div class="settings-section-body">
        ${keepScreenOn ? `<label class="row small" style="margin:0 0 8px"><input type="checkbox" id="keepScreenOnToggle" ${keepScreenOn.get() ? "checked" : ""}> Keep screen on</label><p class="small muted" style="margin:0 0 14px">Prevents the screen from sleeping while Orqella is in the foreground. The system screen timeout is unchanged.</p>` : ""}
        ${disconnectTunnel ? `<p class="small muted" style="margin:0 0 10px">Close the foreground SSH tunnel, return to the connection form, and stop reconnecting when the app opens.</p><button class="button ghost" id="disconnectTunnel">Disconnect tunnel</button>` : ""}
        </div></article>`
      : "";
    const version = state.info?.version || "unknown";
    const build = state.info?.commit && state.info.commit !== "unknown" ? state.info.commit : "unknown";
    const linkEnabled = !!config.link?.enabled;
    panel.innerHTML = `<div class="stack">
      <article class="card flat settings-section" id="generalSettings">
        <div class="card-title settings-section-head"><h2>General</h2></div>
        <div class="settings-section-body">
        <div class="stack-sm small muted"><span>Origin <b class="mono">${esc(config.origin || location.origin)}</b></span><span>Version <b class="mono">${esc(version)}</b></span><span>Build <b class="mono">${esc(build)}</b></span></div>
        <label class="row small" style="margin-top:14px;color:var(--muted)"><input type="checkbox" id="auditToggle" ${config.audit?.enabled ? "checked" : ""}> Log Events</label>
        <div class="row wrap" style="margin-top:12px"><button class="button ghost small-button" id="linkRefresh">Refresh</button><button class="button" id="linkSignIn" ${linkEnabled ? "hidden" : ""}>Sign In</button><button class="button ghost small-button" id="linkRestart" ${linkEnabled ? "" : "hidden"}>Restart Link</button><button class="button ghost small-button" id="linkSignOut" ${linkEnabled ? "" : "hidden"}>Sign Out</button></div>
        <div class="small muted" id="linkStatusLine" style="margin-top:9px">Checking Link…</div>
        <div class="small muted" id="linkHint" style="margin-top:4px"></div>
        <div class="small" id="linkError" style="margin-top:6px;color:var(--red)" hidden></div>
        <div class="small muted" id="linkSignInStatus" style="margin-top:9px" hidden></div>
        <div class="peer-list" id="machineList"></div>
        </div>
      </article>
      <article class="card flat settings-section" id="themeSettings">
        <div class="card-title settings-section-head"><h2>Theme</h2></div>
        <div class="settings-section-body">
        <div class="theme-row"><select class="field" id="themeSelect" aria-label="Dashboard theme">${themes.map((theme) => `<option value="${esc(theme.id)}" ${theme.id === selectedTheme.id ? "selected" : ""}>${esc(theme.name)}</option>`).join("")}</select><button class="button ghost small-button" id="reloadTheme">Reload</button></div><div class="small muted" style="margin-top:8px">Personal override: <span class="mono theme-path">${esc(themePath)}</span></div>
        <div class="theme-agent" style="margin-top:16px"><label class="form-label" for="themePrompt">Create a theme with the default agent</label><textarea class="field" id="themePrompt" rows="3" placeholder="Describe the colors, mood, and contrast you want…"></textarea><div class="small muted" style="margin-top:5px">The agent creates a new standalone theme and adds it to the picker.</div><div class="row wrap" style="margin-top:8px"><button class="button" id="generateTheme">Create theme</button><span class="small muted" id="themeAgentStatus"></span></div></div>
        </div>
      </article>
      ${mobileSettings}
      <article class="card flat settings-section" id="moduleIndexSettings">
        <div class="card-title settings-section-head"><div><h2>Install module</h2><p class="small muted" style="margin:4px 0 0">Curated modules from the Orqella module index. Installed status and installs apply to <b>${esc(machineName(state.node))}</b>.</p></div><button class="button ghost small-button" type="button" id="moduleIndexRefresh">Refresh</button></div>
        <div class="settings-section-body">
          <details class="module-install-advanced">
            <summary>Install from a URL or local path</summary>
            <div class="install-bar" style="padding:9px 0 0"><input class="field" id="installPath" placeholder="git URL or /path/to/module"><input class="field install-branch" id="installBranch" placeholder="branch (optional)"><button class="button ghost" id="installButton">Install</button><button class="button" id="newModule">Build</button></div>
          </details>
          <div class="module-index-toolbar"><input class="field" id="moduleSearch" placeholder="Search modules" autocomplete="off"><span class="small muted" id="moduleIndexStatus">Loading…</span></div>
          <div class="module-index-list" id="moduleIndexList"><p class="small muted" style="padding:12px">Loading module index…</p></div>
        </div>
      </article>
    </div>`;
    $("auditToggle").onchange = async (event) => {
      try {
        const result = await api("/api/v1/config/audit", { method: "POST", body: JSON.stringify({ enabled: event.target.checked }) });
        if (state.info?.config?.audit) state.info.config.audit.enabled = result.enabled;
        toast("Log events updated", result.enabled ? "Recording." : "Off.", "ok");
      } catch (error) {
        event.target.checked = !event.target.checked;
        toast("Could not update log events", error.message, "bad");
      }
    };
    $("reloadTheme").onclick = () => location.reload();
    $("themeSelect").onchange = async (event) => {
      const previous = state.info?.config?.theme;
      try {
        await api("/api/v1/config/theme", { method: "POST", body: JSON.stringify({ theme: event.target.value }) });
        await applyTheme(event.target.value);
        toast("Theme changed", event.target.selectedOptions[0]?.textContent || event.target.value, "ok");
      } catch (error) {
        if (previous) event.target.value = previous;
        toast("Could not change theme", error.message, "bad");
      }
    };
    $("generateTheme").onclick = generateTheme;
    updateThemeAgentUi();
    if (disconnectTunnel) {
      $("disconnectTunnel").onclick = () => {
        toast("Disconnecting", "Closing the foreground SSH tunnel.", "ok");
        disconnectTunnel();
      };
    }
    if (keepScreenOn) {
      $("keepScreenOnToggle").onchange = (event) => {
        const enabled = event.target.checked;
        keepScreenOn.set(enabled);
        toast(
          enabled ? "Screen will stay on" : "Screen timeout restored",
          enabled
            ? "Orqella will keep the screen awake while it is in the foreground."
            : "The system screen timeout applies again.",
          "ok",
        );
      };
    }
    $("linkRefresh").onclick = refreshLinkedDevices;
    $("linkSignIn").onclick = signInLink;
    $("linkRestart").onclick = restartLink;
    $("linkSignOut").onclick = signOutLink;
    $("newModule").onclick = () => selectView("build");
    $("moduleIndexRefresh").onclick = () => void loadModuleIndex();
    $("moduleSearch").oninput = () => renderModuleIndex();
    void loadModuleIndex();
    renderMachineList();
    void refreshLinkStatus();
    $("installButton").onclick = async () => {
      const source = $("installPath").value.trim();
      const branch = $("installBranch").value.trim();
      if (!source) return;
      const node = state.node || "local";
      const payload = /^(https?:|ssh:|git@|git:|file:)/.test(source) ? { url: source, branch } : { path: source };
      const install = (allowUnsigned) => api(nodeApi("/modules/install", node), {
        method: "POST",
        body: JSON.stringify({ ...payload, allow_unsigned: allowUnsigned }),
      });
      try {
        let result;
        try {
          result = await install(false);
        } catch (error) {
          if (!/unsigned/i.test(error.message)) throw error;
          if (!confirm(`This module is not signed by a trusted publisher. Install it on ${machineName(node)} anyway? Only do this for code you trust.`)) return;
          result = await install(true);
        }
        if ((state.node || "local") !== node) return;
        await refresh();
        if (result?.module?.id) {
          state.settingsTab = "modules";
          state.settingsModule = result.module.id;
          renderSettings();
        }
        if (result?.sidecar_error) {
          toast("Module installed with a setup error", result.sidecar_error, "bad");
        } else {
          toast(`Module installed on ${machineName(node)}`, source, "ok");
        }
      } catch (error) { toast("Install failed", error.message, "bad"); }
    };
  }


  function mountSettingsFrames(panel, modules = selectedModules()) {
    for (const key of [...state.readyFrames]) {
      if (key.startsWith("settings:")) state.readyFrames.delete(key);
    }
    state.settingsFrames = new Map();
    panel.querySelectorAll("iframe[data-settings-frame]").forEach((frame) => {
      const moduleId = frame.dataset.settingsFrame;
      const module = modules.find((entry) => entry.id === moduleId);
      if (!module || !moduleUsable(module)) {
        frame.remove();
        return;
      }
      closeModuleBroker("settings", moduleId);
      const nonce = newFrameNonce();
      frame.dataset.frameNonce = nonce;
      const isolated = Boolean(moduleOriginFor(module));
      if (isolated) {
        frame.setAttribute("sandbox", moduleFrameSandbox(module));
        frame.src = moduleFrameSrc(module, "settings", "", nonce);
      } else {
        frame.src = `/m/${encodeURIComponent(module.id)}/settings${frameHash(nonce, module.ws_token)}`;
      }
      state.settingsFrames.set(moduleId, frame);
      frame.addEventListener("load", () => {
        try {
          frame.contentWindow?.postMessage(
            { type: "orqella:ping", nonce: frame.dataset.frameNonce || "" },
            moduleOriginFor(module) || "*",
          );
        } catch (_) {}
      });
    });
  }

  function workspaceAgentOptions(selected) {
    const registry = state.agentRegistry || {};
    let entries = Array.isArray(registry.harnesses)
      ? registry.harnesses.filter((entry) => entry.command !== "")
      : [];
    if (!entries.length) {
      entries = (state.info?.config?.agent_presets || [])
        .filter((preset) => preset.id !== "custom")
        .map((preset) => ({ id: preset.id, label: preset.name }));
    }
    if (selected && !entries.some((entry) => entry.id === selected)) {
      entries = [{ id: selected, label: `${selected} (custom)` }, ...entries];
    }
    return entries;
  }

  function updateWorkspaceAgentUi(panel) {
    const harness = agentHarnessModule();
    if (!harness) return;
    const select = panel.querySelector("#workspaceDefaultAgent");
    if (!select) return;
    const selected = state.agentRegistry?.default || select.value || harness.config?.default_harness || "";
    const entries = workspaceAgentOptions(selected);
    select.innerHTML = entries
      .map((entry) => `<option value="${esc(entry.id)}" ${entry.id === selected ? "selected" : ""}>${esc(entry.label)}</option>`)
      .join("");
    const waiting = Boolean(panel.querySelector("iframe[data-agent-registry]")) && !state.agentRegistry;
    select.disabled = waiting || entries.length === 0;
    const hidden = panel.querySelector('[data-config-field="default_harness"]');
    if (hidden) hidden.value = selected;
    const status = panel.querySelector("#workspaceDefaultStatus");
    if (status) {
      if (waiting) status.textContent = "Loading agents…";
      else if (entries.length) status.textContent = "Changes apply to new conversations.";
      else status.textContent = "No enabled agents in the registry.";
    }
  }

  async function saveWorkspaceDefault(harness, value) {
    if (!harness) return;
    const values = {};
    for (const field of harness.config_schema?.fields || []) {
      values[field.key] = field.key === "default_harness"
        ? value
        : (harness.config?.[field.key] ?? field.default);
    }
    const result = await api(`/api/v1/modules/${encodeURIComponent(harness.id)}/config`, {
      method: "POST",
      body: JSON.stringify({ values }),
    });
    const updated = result.module;
    if (updated) {
      const local = state.localModules.findIndex((entry) => entry.id === harness.id);
      if (local >= 0) state.localModules[local] = { ...state.localModules[local], ...updated };
      const installed = state.modules.findIndex((entry) => entry.id === harness.id);
      if (installed >= 0) state.modules[installed] = { ...state.modules[installed], ...updated };
    }
  }

  function renderSettingsAgents() {
    const panel = $("settingsPanel");
    if (!panel) return;
    const config = state.info?.config || {};
    const agent = config.agent || {};
    const agentViewer = agent.viewer || {};
    const resolvedViewer = agent.resolved_viewer || {};
    const agentPresets = config.agent_presets || [];
    const agentEnv = Object.entries(agent.env || {}).map(([key, value]) => `${key}=${value}`).join("\n");
    const agentPreset = agentPresets.find((item) => item.id === (agent.preset || "custom"));
    const harness = agentHarnessModule();
    if (harness?.has_settings) state.agentRegistry = null;
    const harnessConfig = harness?.config || {};
    const workspaceFields = (harness?.config_schema?.fields || [])
      .filter((field) => field.key !== "default_harness");
    const workspaceDefault = state.agentRegistry?.default || harnessConfig.default_harness || "opencode";
    const workspaceChoices = workspaceAgentOptions(workspaceDefault);
    const workspaceStatus = !harness
      ? "Not installed"
      : !harness.enabled
        ? "Disabled"
        : harness.sidecar?.state === "running"
          ? "Running"
          : "Stopped";
    const workspaceChip = harness && harness.enabled && harness.sidecar?.state === "running" ? "ok" : "warn";
    const pendingPermissions = harness?.pending_permissions?.length || 0;

    panel.innerHTML = `<div class="stack">
      <article class="card">
        <div class="row spread"><h2>Defaults</h2><span class="small muted">Headless and interactive</span></div>
        <div class="separator" style="margin:13px 0 15px"></div>
        <div class="row spread"><h3>New conversations</h3><span class="small muted">Agent workspace</span></div>
        <p class="small muted" style="margin:8px 0 12px">Default agent for new sessions in the Agent Harness workspace.</p>
        ${!harness ? `<div class="empty"><h2>Agent Harness is not installed</h2><p>Install it to manage interactive agents and conversations.</p><button class="button ghost" type="button" data-open-install>Open install</button></div>` : !harness.enabled ? `<div class="empty"><h2>Agent Harness is disabled</h2><p>Enable it in Module details to manage agents and choose a default.</p><button class="button ghost" type="button" data-open-module-details>Module details</button></div>` : `<select class="field" id="workspaceDefaultAgent" aria-label="Default agent for new conversations" ${harness.has_settings || !workspaceChoices.length ? "disabled" : ""}>${workspaceChoices.map((entry) => `<option value="${esc(entry.id)}" ${entry.id === workspaceDefault ? "selected" : ""}>${esc(entry.label)}</option>`).join("")}</select><p class="small muted" id="workspaceDefaultStatus" style="margin-top:7px">Changes apply to new conversations.</p>`}
        <div class="separator" style="margin:16px 0 15px"></div>
        <div class="row spread"><h3>Build &amp; background jobs</h3><span class="small muted" id="agentCurrent">${esc(agentPreset?.name || agent.preset || "Custom")}</span></div>
        <p class="small muted" style="margin:8px 0 12px">Used by Build, background jobs, and modules you grant the <span class="mono">agent</span> permission.</p>
        <div class="form-grid">
          <div><label class="form-label">Preset</label><select class="field" id="agentPreset">${agentPresets.map((item) => `<option value="${esc(item.id)}" ${item.id === (agent.preset || "custom") ? "selected" : ""}>${esc(item.name)}</option>`).join("")}</select></div>
          <div><label class="form-label">Command</label><input class="field" id="agentCommand" value="${esc(agent.command || "")}" autocomplete="off"></div>
          <div><label class="form-label">Timeout (seconds, 0 = none)</label><input class="field" id="agentTimeout" type="number" min="0" max="86400" value="${esc(agent.timeout_secs ?? 0)}"></div>
        </div>
        <label class="form-label" style="display:block;margin:12px 0 5px">Arguments — one per line</label>
        <textarea class="field" id="agentArgs" rows="4" spellcheck="false">${esc((agent.args || []).join("\n"))}</textarea>
        <label class="form-label" style="display:block;margin:12px 0 5px">Environment — KEY=VALUE per line</label>
        <textarea class="field" id="agentEnv" rows="4" spellcheck="false" placeholder="ANTHROPIC_BASE_URL=http://127.0.0.1:8080&#10;ANTHROPIC_AUTH_TOKEN=${"${"}ANTHROPIC_AUTH_TOKEN}">${esc(agentEnv)}</textarea>
        <p class="small muted" id="agentPresetHint" style="margin-top:7px">${esc(agentPreset?.hint || "The command receives the prompt as its final argument.")}</p>
        <details class="agent-advanced" style="margin-top:12px">
          <summary class="small muted" style="cursor:pointer">Advanced: conversation viewer</summary>
          <p class="small muted" style="margin:8px 0 10px">Conversations open in the Agent Harness workspace by default. Override only if you use another viewer. Use <span class="mono">{session}</span> and <span class="mono">{root}</span> in the path or URL.</p>
          <div class="form-grid">
            <div><label class="form-label">Viewer module</label><input class="field" id="agentViewerModule" value="${esc(agentViewer.module || "")}" placeholder="${esc(resolvedViewer.module || "agent-harness")}" autocomplete="off"></div>
            <div><label class="form-label">Viewer mount</label><input class="field" id="agentViewerMount" value="${esc(agentViewer.mount || "")}" placeholder="${esc(resolvedViewer.mount || "opencode")}" autocomplete="off"></div>
            <div><label class="form-label">Viewer path</label><input class="field" id="agentViewerPath" value="${esc(agentViewer.path || "")}" placeholder="${esc(resolvedViewer.path || "/?session={session}")}" autocomplete="off"></div>
            <div><label class="form-label">External viewer URL</label><input class="field" id="agentViewerUrl" value="${esc(agentViewer.url || "")}" placeholder="https://example.com/conversation/{session}" autocomplete="off"></div>
          </div>
          <p class="small muted" style="margin-top:7px">Build and edit runs open the agent's conversation when the harness exposes a session id. Keep the timeout at <span class="mono">0</span> for long builds; a positive value stops the process and can leave a service-backed session running.</p>
          <p class="small muted">For custom endpoints, set the variables the selected CLI expects. Claude Code uses <span class="mono">ANTHROPIC_BASE_URL</span> / <span class="mono">ANTHROPIC_AUTH_TOKEN</span>; Codex can use its provider config or <span class="mono">--oss --local-provider</span>; OpenCode uses its provider config.</p>
        </details>
        <div class="row wrap" style="margin-top:14px"><button class="button" id="saveAgent">Save agent</button><span class="small muted" id="agentSaveStatus"></span></div>
      </article>

      <article class="card">
        <div class="row spread"><h2>Workspace</h2><span class="chip ${workspaceChip}">${esc(workspaceStatus)}</span></div>
        <p class="small muted" style="margin:8px 0 12px">Interactive sessions, approvals, and transcripts. The agent registry defines the agents available here.</p>
        <div class="row wrap">
          ${harness ? `<button class="button ghost small-button" type="button" data-open-workspace ${harness.enabled && harness.has_page ? "" : "disabled"}>Open workspace</button><button class="button ghost small-button" type="button" data-open-module-details>Module details</button><button class="button ghost small-button" type="button" id="installOpencode">Install OpenCode</button>` : `<button class="button ghost small-button" type="button" data-open-install>Install Agent Harness</button>`}
          ${pendingPermissions ? `<button class="button small-button" type="button" data-review-agent-permissions>Review permissions</button>` : ""}
        </div>
        ${harness ? `<p class="small muted" style="margin:9px 0 0">Install OpenCode if you do not already use a coding agent. It downloads the official build and selects a free default model.</p>` : ""}
        ${workspaceFields.length ? `<form class="form-grid config-form" data-config-form="${esc(harness.id)}" style="margin-top:14px">
          ${workspaceFields.map((field) => fieldControl(harness, field)).join("")}
          <input type="hidden" data-config-field="default_harness" data-config-type="text" value="${esc(workspaceDefault)}">
          <div class="row wrap"><button class="button" type="submit">Save workspace settings</button><span class="small muted" data-config-status="${esc(harness.id)}"></span></div>
        </form>` : ""}
      </article>
      <article class="card flat settings-ui-card">
        ${harness?.has_settings && harness.enabled && moduleUsable(harness) ? `<iframe class="settings-frame" data-settings-frame="${esc(harness.id)}" data-agent-registry title="Agent registry" allow="clipboard-write" allowfullscreen sandbox="${moduleFrameSandbox(harness)}"></iframe>` : `<div style="padding:18px"><div class="row spread"><h2>Agent registry</h2><span class="small muted">Agents</span></div><p class="small muted" style="margin:8px 0 0">${!harness ? "Install Agent Harness to define the agents available in the workspace." : "Enable Agent Harness in Module details to manage the agent registry."}</p></div>`}
      </article>

    </div>`;

    $("agentPreset").onchange = (event) => {
      const selected = agentPresets.find((item) => item.id === event.target.value);
      if (selected && selected.id !== "custom") {
        $("agentCommand").value = selected.command || "";
        $("agentArgs").value = (selected.args || []).join("\n");
      }
      $("agentPresetHint").textContent = selected?.hint || "The command receives the prompt as its final argument.";
    };
    $("saveAgent").onclick = async () => {
      const env = {};
      for (const line of $("agentEnv").value.split("\n")) {
        const trimmed = line.trim();
        if (!trimmed) continue;
        const index = trimmed.indexOf("=");
        if (index <= 0) {
          $("agentSaveStatus").textContent = "Environment lines must be KEY=VALUE.";
          return;
        }
        env[trimmed.slice(0, index).trim()] = trimmed.slice(index + 1).trim();
      }
      const args = $("agentArgs").value.split("\n").map((line) => line.trim()).filter(Boolean);
      const timeout = Number($("agentTimeout").value);
      if (!Number.isFinite(timeout) || timeout < 0) {
        $("agentSaveStatus").textContent = "Timeout must be 0 (no timeout) or a positive number of seconds.";
        return;
      }
      $("saveAgent").disabled = true;
      $("agentSaveStatus").textContent = "Saving…";
      try {
        const result = await api("/api/v1/config/agent", {
          method: "POST",
          body: JSON.stringify({
            preset: $("agentPreset").value,
            command: $("agentCommand").value.trim(),
            args,
            env,
            timeout_secs: timeout,
            viewer: {
              kind: $("agentViewerUrl").value.trim() ? "url" : $("agentViewerModule").value.trim() ? "module" : $("agentViewerMount").value.trim() ? "app" : "",
              module: $("agentViewerModule").value.trim(),
              mount: $("agentViewerMount").value.trim(),
              path: $("agentViewerPath").value.trim(),
              url: $("agentViewerUrl").value.trim(),
              title: "",
            },
          }),
        });
        state.info.config.agent = result.agent;
        const savedPreset = agentPresets.find((item) => item.id === result.agent.preset);
        $("agentCurrent").textContent = savedPreset?.name || result.agent.preset;
        $("agentSaveStatus").textContent = "Saved";
        toast("Agent updated", result.agent.preset, "ok");
      } catch (error) {
        $("agentSaveStatus").textContent = error.message;
        toast("Could not update agent", error.message, "bad");
      } finally {
        $("saveAgent").disabled = false;
      }
    };

    const installOpencode = $("installOpencode");
    if (installOpencode && harness) {
      installOpencode.onclick = async () => {
        const label = installOpencode.textContent;
        installOpencode.disabled = true;
        installOpencode.textContent = "Installing…";
        try {
          const result = await api("/api/v1/setup/opencode", { method: "POST", body: "{}" });
          const detail = result.opencode?.model
            ? `free model ${result.opencode.model}`
            : (result.actions?.[0] || "installed");
          const warning = result.warnings?.[0];
          toast("OpenCode ready", warning || detail, warning ? "" : "ok");
          // The module caches probes for 30 seconds; refresh its OpenCode
          // entry so the registry and New session show it immediately.
          await api(`/api/v1/modules/${encodeURIComponent(AGENT_HARNESS_ID)}/rpc`, {
            method: "POST",
            body: JSON.stringify({
              method: "backend.request",
              params: { method: "POST", path: "/api/registry/probe", body: { id: "opencode" } },
            }),
          }).catch(() => {});
          state.info = await api("/api/v1/info");
          renderSettingsAgents();
        } catch (error) {
          installOpencode.disabled = false;
          installOpencode.textContent = label;
          toast("Could not install OpenCode", error.message, "bad");
        }
      };
    }

    const workspaceSelect = $("workspaceDefaultAgent");
    if (workspaceSelect && harness) {
      workspaceSelect.onchange = async () => {
        const value = workspaceSelect.value;
        const status = $("workspaceDefaultStatus");
        const frame = panel.querySelector('iframe[data-agent-registry]');
        const hidden = panel.querySelector('[data-config-field="default_harness"]');
        if (hidden) hidden.value = value;
        if (frame) {
          if (status) status.textContent = "Saving…";
          frame.contentWindow?.postMessage(
            { type: "orqella:set-agent-default", id: value },
            moduleOriginFor(harness) || "*",
          );
          return;
        }
        if (status) status.textContent = "Saving…";
        try {
          await saveWorkspaceDefault(harness, value);
          if (status) status.textContent = "Saved";
          toast("Default agent updated", value, "ok");
        } catch (error) {
          if (status) status.textContent = error.message;
          toast("Could not update default agent", error.message, "bad");
        }
      };
      updateWorkspaceAgentUi(panel);
    }

    const registryFrame = panel.querySelector("iframe[data-agent-registry]");
    if (state.agentRegistryHandler) window.removeEventListener("message", state.agentRegistryHandler);
    state.agentRegistryHandler = (event) => {
      const frame = panel.querySelector("iframe[data-agent-registry]");
      if (!frame || event.source !== frame.contentWindow) return;
      const expectedOrigin = moduleOriginFor(harness);
      if (expectedOrigin ? event.origin !== expectedOrigin : event.origin !== "null") return;
      const message = event.data;
      if (!message || typeof message !== "object") return;
      if (message.type === "orqella:agent-registry") {
        state.agentRegistry = {
          harnesses: Array.isArray(message.harnesses) ? message.harnesses : [],
          default: typeof message.default === "string" ? message.default : "",
        };
        updateWorkspaceAgentUi(panel);
        return;
      }
      if (message.type === "orqella:agent-registry-error") {
        const status = panel.querySelector("#workspaceDefaultStatus");
        if (status) status.textContent = message.message || "Could not update the default agent.";
        toast("Could not update default agent", message.message || "Registry error", "bad");
        updateWorkspaceAgentUi(panel);
      }
    };
    window.addEventListener("message", state.agentRegistryHandler);
    clearTimeout(state.agentRegistryTimer);
    state.agentRegistryTimer = harness?.has_settings && harness.enabled
      ? setTimeout(() => {
          if (state.settingsTab !== AGENTS_TAB || state.agentRegistry) return;
          const status = panel.querySelector("#workspaceDefaultStatus");
          if (status) status.textContent = "Update Agent Harness to manage the default here.";
        }, 4000)
      : null;

    if (harness) {
      bindModuleConfigForm(harness, panel, "local");
      panel.querySelectorAll("[data-open-workspace]").forEach((button) => button.onclick = () => selectModule(harness.id));
      panel.querySelectorAll("[data-open-module-details]").forEach((button) => button.onclick = () => {
        state.settingsTab = "modules";
        state.settingsModule = harness.id;
        renderSettings();
      });
      panel.querySelectorAll("[data-review-agent-permissions]").forEach((button) => button.onclick = () => openPermissions(harness.id));
    }
    panel.querySelectorAll("[data-open-install]").forEach((button) => button.onclick = () => {
      state.settingsTab = "general";
      renderSettings();
      requestAnimationFrame(() => {
        const advanced = panel.querySelector(".module-install-advanced");
        if (advanced) advanced.open = true;
        const input = $("installPath");
        input?.scrollIntoView({ block: "center", behavior: "smooth" });
        input?.focus();
      });
    });
    mountSettingsFrames(panel, state.localModules);
  }

  function updateSource(value) {
    return value?.result || value || {};
  }

  function updateOutcome(value) {
    const source = updateSource(value);
    if (!value || value.ok === false || source.ok === false) {
      return {
        action: "blocked",
        reason: value?.error || source.error || "check failed",
        source,
      };
    }
    if (source.available !== true) return { action: "current", reason: "up to date", source };
    return { action: "update", reason: "", source };
  }

  function machineName(nodeId) {
    const node = state.nodes?.find((candidate) => candidate.id === nodeId);
    const peer = state.peers?.find((candidate) => candidate.id === nodeId);
    return node?.name || peer?.name || (nodeId === "local" ? "this machine" : nodeId);
  }

  function setMachineButton(button, label, { disabled = false, ghost = true, phase = "idle" } = {}) {
    if (!button) return;
    button.dataset.phase = phase;
    button.disabled = disabled;
    button.textContent = label;
    button.classList.toggle("ghost", ghost);
  }

  function resetMachineButton(button) {
    setMachineButton(button, "Check for updates");
  }

  function renderMachineList() {
    const list = $("machineList");
    if (!list) return;
    const nodes = state.nodes || [];
    const health = new Map(nodes.map((node) => [node.id, node]));
    const local = nodes.find((node) => node.local) || {
      id: "local",
      name: "this machine",
      online: true,
      local: true,
    };
    const machines = [
      local,
      ...(state.peers || []).map((peer) => {
        const status = health.get(peer.id) || {};
        return { ...peer, ...status, id: peer.id, name: status.name || peer.name };
      }),
    ];
    list.innerHTML = machines.map((node) => {
      const online = node.local || node.online;
      return `<article class="peer-card machine-row" data-machine="${esc(node.id)}"><div class="peer-main"><b>${esc(node.name || node.id)}</b><small>${node.local ? "This machine" : esc(node.id)}</small></div><div class="machine-actions"><span class="chip ${online ? "ok" : "bad"}">${online ? "online" : "offline"}</span><button class="button ghost small-button" data-machine-action="${esc(node.id)}" ${online ? "" : "disabled"}>Check for updates</button></div></article>`;
    }).join("");
    list.querySelectorAll("[data-machine-action]").forEach((button) => {
      button.onclick = () => machineAction(button.dataset.machineAction, button);
    });
  }

  async function reloadMachines() {
    const [nodes, peers] = await Promise.all([
      api("/api/v1/nodes"),
      api("/api/v1/peers"),
    ]);
    state.nodes = Array.isArray(nodes) ? nodes : [];
    state.peers = Array.isArray(peers) ? peers : [];
    renderMachineList();
  }

  async function refreshLinkStatus() {
    try {
      state.linkView = await api("/api/v1/link");
    } catch (error) {
      state.linkView = {
        error: error.message,
        status: { enabled: false },
        config: state.info?.config?.link || {},
      };
    }
    renderLinkStatus();
  }

  function renderLinkStatus() {
    const view = state.linkView || {};
    const status = view.status || {};
    const config = view.config || state.info?.config?.link || {};
    const running = Boolean(status.enabled);
    const configured = Boolean(config.enabled);
    const error = String(view.error || "");
    const needsAttention = Boolean(error) || configured;
    const readOnly = /read-only|privileged|HTTP 401/i.test(error);
    const signIn = $("linkSignIn");
    const restart = $("linkRestart");
    const signOut = $("linkSignOut");
    const refresh = $("linkRefresh");
    if (signIn) {
      signIn.hidden = running;
      signIn.disabled = false;
      signIn.textContent = needsAttention ? "Sign In Again" : "Sign In";
    }
    if (restart) {
      restart.hidden = !(configured || running);
      restart.disabled = !(configured || running);
      restart.textContent = "Restart Link";
    }
    if (signOut) {
      signOut.hidden = !(configured || running);
      signOut.disabled = !(configured || running);
    }
    if (refresh) {
      refresh.hidden = !running;
      refresh.disabled = !running;
      refresh.textContent = "Refresh";
    }
    const line = $("linkStatusLine");
    if (line) {
      const plan = status.link_plan || "free";
      const trial = status.trial_end
        ? ` · trial ends ${new Date(status.trial_end).toLocaleDateString()}`
        : "";
      line.textContent = running
        ? `Connected${status.device_name ? ` as ${status.device_name}` : ""} · ${plan}${trial}`
        : needsAttention ? "Sign-in needs attention" : "Not signed in";
    }
    const hint = $("linkHint");
    if (hint) {
      hint.textContent = running
        ? "This machine is registered with Orqella Link."
        : readOnly
          ? "Run `orqella open` on the hub to manage Link."
          : needsAttention
            ? "Link rejected this machine's saved credentials. Sign in again to re-enroll it."
            : "Sign in to Link to reach this machine from your phone, browser, or another computer.";
    }
    const errorNode = $("linkError");
    if (errorNode) {
      errorNode.hidden = !error;
      errorNode.textContent = error;
    }
  }

  async function signInLink(event) {
    const button = event.currentTarget;
    const status = $("linkSignInStatus");
    const flow = ++linkSignInFlow;
    button.disabled = true;
    button.textContent = "Signing in…";
    if (status) {
      status.hidden = false;
      status.textContent = "Starting sign-in…";
    }
    let popup = null;
    try {
      popup = window.open("", "_blank");
      const start = await api("/api/v1/link/device/start", {
        method: "POST",
        body: "{}",
      });
      if (flow !== linkSignInFlow) {
        if (popup) popup.close();
        return;
      }
      if (status) {
        status.innerHTML = `Code <b class="mono">${esc(start.user_code || "")}</b> · <a href="${esc(start.verification_uri)}" target="_blank" rel="noopener noreferrer">approve in browser</a>`;
      }
      if (popup) {
        popup.opener = null;
        popup.location.href = start.verification_uri;
      } else {
        window.open(start.verification_uri, "_blank", "noopener,noreferrer");
      }
      const deadline = Date.now() + Math.max(1, Number(start.expires_in) || 600) * 1000;
      const interval = Math.max(1, Number(start.interval) || 2) * 1000;
      while (Date.now() < deadline) {
        await new Promise((resolve) => setTimeout(resolve, interval));
        if (flow !== linkSignInFlow) {
          if (popup) popup.close();
          return;
        }
        const result = await api("/api/v1/link/device/poll", {
          method: "POST",
          body: JSON.stringify({ device_code: start.device_code }),
        });
        if (flow !== linkSignInFlow) {
          if (popup) popup.close();
          return;
        }
        if (result.status === "approved") {
          if (popup) {
            popup.close();
            popup = null;
          }
          if (state.info?.config?.link) state.info.config.link.enabled = true;
          await refreshLinkStatus();
          toast("Link signed in", "This machine is registered.", "ok");
          if (status) {
            status.textContent = "Signed in.";
            setTimeout(() => {
              if (status.isConnected) status.hidden = true;
            }, 2500);
          }
          await reloadMachines().catch(() => {});
          return;
        }
        if (result.status === "denied" || result.status === "expired") {
          throw new Error(`Link sign-in ${result.status}.`);
        }
      }
      throw new Error("The browser sign-in code expired. Try again.");
    } catch (error) {
      if (flow !== linkSignInFlow) return;
      if (popup) popup.close();
      button.disabled = false;
      renderLinkStatus();
      if (status) {
        status.hidden = false;
        status.textContent = error.message;
      }
      toast("Could not sign in to Link", error.message, "bad");
    }
  }

  async function refreshLinkedDevices(event) {
    const button = event.currentTarget;
    button.disabled = true;
    button.textContent = "Refreshing…";
    let error = null;
    try {
      await api("/api/v1/link/devices");
    } catch (caught) {
      error = caught;
    }
    try {
      await reloadMachines();
    } catch (caught) {
      if (!error) error = caught;
    }
    await refreshLinkStatus();
    if (error) toast("Could not refresh linked devices", error.message, "bad");
    else toast("Machines refreshed", "Machine status is up to date.", "ok");
    const current = $("linkRefresh");
    if (current) {
      current.disabled = false;
      current.textContent = "Refresh";
    }
  }

  async function restartLink(event) {
    const configured = Boolean(state.linkView?.config?.enabled ?? state.info?.config?.link?.enabled);
    if (!configured && !state.linkView?.status?.enabled) {
      toast("Link is not signed in", "Sign in from Settings → General.", "bad");
      return;
    }
    const button = event.currentTarget;
    button.disabled = true;
    button.textContent = "Restarting…";
    try {
      await api("/api/v1/link/start", { method: "POST", body: "{}" });
    } catch (error) {
      button.disabled = false;
      button.textContent = "Restart Link";
      await refreshLinkStatus();
      toast("Could not restart Link", error.message, "bad");
      return;
    }
    toast("Link restarted", "This device is re-registered.", "ok");
    await refreshLinkStatus();
    await reloadMachines().catch(() => {});
    button.disabled = false;
    button.textContent = "Restart Link";
  }

  async function signOutLink(event) {
    const configured = Boolean(state.linkView?.config?.enabled ?? state.info?.config?.link?.enabled);
    if (!configured && !state.linkView?.status?.enabled) {
      toast("Link is not signed in", "There are no account credentials to remove.", "ok");
      return;
    }
    const button = event.currentTarget;
    button.disabled = true;
    try {
      await api("/api/v1/link/logout", { method: "POST", body: "{}" });
    } catch (error) {
      button.disabled = false;
      await refreshLinkStatus();
      toast("Could not sign out", error.message, "bad");
      return;
    }
    linkSignInFlow += 1;
    if (state.info?.config?.link) state.info.config.link.enabled = false;
    await refreshLinkStatus();
    const status = $("linkSignInStatus");
    if (status) status.hidden = true;
    toast("Link signed out", "Account credentials removed.", "ok");
    await reloadMachines().catch(() => {});
  }

  async function machineAction(nodeId, button) {
    if (button.dataset.phase === "update") return updateMachine(nodeId, button);
    if (button.dataset.phase === "checking" || button.dataset.phase === "updating") return;
    return checkMachine(nodeId, button);
  }

  async function checkMachine(nodeId, button, { silent = false } = {}) {
    setMachineButton(button, "Checking…", { disabled: true, phase: "checking" });
    try {
      const data = await api(`/api/v1/update/check/${encodeURIComponent(nodeId)}`);
      const outcome = updateOutcome(data);
      if (outcome.action === "update") {
        setMachineButton(button, "Update", { ghost: false, phase: "update" });
        if (!silent) {
          toast("Update available", `${machineName(nodeId)} · ${outcome.source.latest ? `v${outcome.source.latest}` : "new release"}`, "ok");
        }
      } else if (outcome.action === "current") {
        resetMachineButton(button);
        if (!silent) toast("Up to date", machineName(nodeId), "ok");
      } else {
        resetMachineButton(button);
        if (!silent) {
          toast("Could not check for updates", `${machineName(nodeId)}: ${outcome.reason}`, "bad");
        }
      }
      return outcome;
    } catch (error) {
      resetMachineButton(button);
      if (!silent) toast("Could not check for updates", `${machineName(nodeId)}: ${error.message}`, "bad");
      return null;
    }
  }

  async function updateMachine(nodeId, button) {
    const name = machineName(nodeId);
    if (!window.confirm(`Update ${name}? It will download the latest GitHub release and restart Orqella.`)) return;
    setMachineButton(button, "Updating…", { disabled: true, phase: "updating" });
    try {
      const result = await api(`/api/v1/update/apply/${encodeURIComponent(nodeId)}`, {
        method: "POST",
        body: "{}",
      });
      if (result.action !== "accepted") {
        resetMachineButton(button);
        toast("Could not update", `${name}: ${result.reason || result.action || "not started"}`, "bad");
        return;
      }
      toast("Update started", name, "ok");
      if (nodeId === "local") pollLocalUpdate(button);
      else pollPeerUpdate(nodeId, button);
    } catch (error) {
      resetMachineButton(button);
      toast("Could not update", `${name}: ${error.message}`, "bad");
    }
  }

  async function pollLocalUpdate(button, deadline = Date.now() + 10 * 60 * 1000) {
    try {
      const status = await api("/api/v1/update/status");
      if (status.status === "succeeded") {
        toast("Update finished", "Reloading…", "ok");
        setTimeout(() => location.reload(), 1800);
        return;
      }
      if (status.status === "failed") {
        resetMachineButton(button);
        toast("Update failed", status.error || status.status || "failed", "bad");
        return;
      }
      if (Date.now() < deadline) {
        setTimeout(() => pollLocalUpdate(button, deadline), 2000);
        return;
      }
      resetMachineButton(button);
      toast("Update is taking longer than expected", "This machine", "bad");
    } catch {
      if (Date.now() < deadline) {
        setTimeout(() => pollLocalUpdate(button, deadline), 2500);
        return;
      }
      resetMachineButton(button);
      toast("Could not confirm the update", "This machine did not come back yet.", "bad");
    }
  }

  async function pollPeerUpdate(nodeId, button, deadline = Date.now() + 10 * 60 * 1000) {
    try {
      const statuses = await api("/api/v1/update/peer-status");
      const entry = Array.isArray(statuses)
        ? statuses.find((item) => item.id === nodeId)
        : null;
      const status = entry?.status || {};
      if (status.status === "succeeded") {
        toast("Update finished", machineName(nodeId), "ok");
        await checkMachine(nodeId, button, { silent: true });
        return;
      }
      if (status.status === "failed") {
        resetMachineButton(button);
        toast("Update failed", `${machineName(nodeId)}: ${status.error || "failed"}`, "bad");
        return;
      }
      if (Date.now() >= deadline) {
        resetMachineButton(button);
        toast("Update is taking longer than expected", machineName(nodeId), "bad");
        return;
      }
      setTimeout(
        () => pollPeerUpdate(nodeId, button, deadline),
        entry && entry.ok === false ? 5000 : 2500
      );
    } catch (error) {
      if (Date.now() >= deadline) {
        resetMachineButton(button);
        toast("Could not check update status", `${machineName(nodeId)}: ${error.message}`, "bad");
        return;
      }
      setTimeout(() => pollPeerUpdate(nodeId, button, deadline), 3000);
    }
  }

  function renderSettingsModule(id, root = $("settingsPanel")) {
    const panel = root;
    const module = selectedModules().find((entry) => entry.id === id);
    if (!module) {
      state.settingsModule = null;
      state.settingsTab = "modules";
      return renderSettings();
    }
    const fields = module.config_schema?.fields || [];
    const isAgentHarness = module.id === AGENT_HARNESS_ID && (state.node || "local") === "local";
    const showGenericSettings = fields.length > 0 && !isAgentHarness;
    const showCustomSettings = module.has_settings && !isAgentHarness;
    const dependencies = module.dependencies || [];
    const installedIds = new Set(selectedModules().map((entry) => entry.id));
    const dependencyChips = dependencies.map((dependency) => {
      const present = installedIds.has(dependency.id);
      const chip = present ? (dependency.optional ? "" : "ok") : (dependency.optional ? "warn" : "bad");
      const suffix = `${dependency.optional ? "optional" : "required"}${present ? "" : " · missing"}`;
      return `<span class="chip ${chip}" title="${esc(dependency.reason || "")}">${esc(dependency.id)} · ${suffix}</span>`;
    }).join("");
    const git = module.git || {};
    const sidecar = module.sidecar || {};
    const sidecarState = sidecar.state || "stopped";
    const sidecarChip = sidecarState === "running" ? "ok" : sidecarState === "failed" ? "bad" : "warn";
    const indexManaged = String(module.source || "").startsWith("index:");
    const gitLine = git.is_repo
      ? `${git.repository_name ? `<b>${esc(git.repository_name)}</b> · ` : "No remote · "}${esc(git.branch || "detached")} · <span class="mono">${esc(git.commit_short || "no commit")}</span>${git.dirty ? ` · <span class="chip warn">uncommitted</span>` : ""}${git.behind ? ` · ${git.behind} behind` : ""}${git.ahead ? ` · ${git.ahead} ahead` : ""}`
      : "Not a git repository";
    panel.innerHTML = `<div class="stack">
      <article class="card">
        <div class="row spread"><h2>${esc(module.name)}</h2><span class="chip ${module.enabled ? "ok" : "bad"}">${module.enabled ? "enabled" : "disabled"}</span></div>
        <div class="row wrap module-meta"><span class="chip">${esc(module.version)}</span><span class="chip">${esc(module.backend)}</span>${module.backend === "sidecar" && (module.backend_sandbox_effective ?? module.backend_sandbox) === false ? `<span class="chip bad">unsandboxed</span>` : ""}${module.has_page ? `<span class="chip">page</span>` : ""}${module.has_overview ? `<span class="chip">overview</span>` : ""}${module.node_scope ? `<span class="chip">${esc(module.node_scope)}</span>` : ""}</div>
        <div class="row wrap" style="margin-top:14px"><button class="button ghost" data-toggle="${esc(module.id)}" data-enabled="${module.enabled}">${module.enabled ? "Disable" : "Enable"}</button>${module.has_page && module.enabled && pageVisible(module) ? `<button class="button ghost" data-open="${esc(module.id)}">Open</button>` : ""}${module.pending_permissions?.length ? `<button class="button" data-review="${esc(module.id)}">Review permissions</button>` : ""}<a class="button ghost" href="${esc(adminModuleApi(module.id, "/export"))}">Export</a><button class="button danger" data-remove="${esc(module.id)}">Remove</button></div>
        ${module.has_page ? `<label class="config-check" style="margin-top:12px"><input type="checkbox" data-page-toggle="${esc(module.id)}" ${pageVisible(module) ? "checked" : ""}><span><b>Show page tab</b></span></label>` : ""}
        <div class="perm-list">${(module.permissions || []).map((permission) => `<span class="perm" title="${esc(permission === "agent" ? "Runs your configured coding agent with your user privileges" : permission === "backend" ? "Runs a sidecar process" : permission.startsWith("node.proxy.manage:") ? `Changes the upstream URL for the ${permission.slice("node.proxy.manage:".length)} app proxy` : permission.startsWith("node.proxy:") ? `Uses the ${permission.slice("node.proxy:".length)} app proxy` : "")}">${esc(permission)}</span>`).join("") || '<span class="small muted">No host permissions</span>'}</div>
        ${(module.permissions || []).includes("agent") ? `<p class="small bad" style="margin:8px 0 0">The agent permission can run your configured coding agent, which can execute commands and read files as you. Grant it only to code you trust.</p>` : ""}
        ${dependencies.length ? `<div class="stack-sm small" style="margin-top:10px"><b>Dependencies</b><div class="row wrap">${dependencyChips}</div></div>` : ""}
      </article>
      <article class="card flat">
        <div class="row spread"><h2>Module repository</h2><span class="small muted">Git</span></div>
        <p class="small muted" style="margin:8px 0 10px">${gitLine}</p>
        <p class="small muted" style="margin:0 0 10px">Source <span class="mono">${esc(module.source || "unknown")}</span></p>
        ${indexManaged ? '<p class="small muted" style="margin:0 0 10px">Installed from the module index. Update it from Settings → General → Install module.</p>' : ""}
        <div class="row wrap"><button class="button ghost small-button" data-git-update="${esc(module.id)}" ${git.is_repo && !indexManaged ? "" : "disabled"}>Update from git (developer)</button><button class="button ghost small-button" data-git-checkpoint="${esc(module.id)}">Checkpoint</button></div>
      </article>
      ${module.backend === "sidecar" ? `<article class="card flat">
        <div class="row spread"><h2>Sidecar</h2><span class="chip ${sidecarChip}">${esc(sidecarState)}</span></div>
        <p class="small muted" style="margin:8px 0 0">${sidecar.attempts ? `${sidecar.attempts} failed attempt${sidecar.attempts === 1 ? "" : "s"}${sidecar.next_retry_in_secs ? ` · retry in ${sidecar.next_retry_in_secs}s` : ""}` : "Process is managed by the daemon supervisor."}</p>
        ${sidecar.last_error ? `<p class="small bad" style="margin:8px 0 0">${esc(sidecar.last_error)}</p>` : ""}
        <p class="small muted" style="margin:8px 0 0">Command <span class="mono">${esc((module.backend_command || []).join(" ") || "none")}</span> · ${module.backend_network ? "network opt-in requested" : "network policy pending"}${(module.backend_sandbox_effective ?? module.backend_sandbox) === false ? " · unsandboxed" : " · sandboxed"}</p>
        ${(module.backend_read_paths || []).length ? `<p class="small muted" style="margin:8px 0 0">Host read access: ${module.backend_read_paths.map((path) => `<span class="mono">${esc(path)}</span>`).join(", ")}</p>` : ""}
        <div class="row wrap" style="margin-top:12px"><button class="button ghost small-button" data-sidecar-restart="${esc(module.id)}" ${module.enabled ? "" : "disabled"}>Restart sidecar</button><button class="button ghost small-button" data-sidecar-log="${esc(module.id)}">Show log</button></div>
        <pre class="build-log" data-sidecar-log-panel="${esc(module.id)}" hidden style="margin-top:10px"></pre>
      </article>` : ""}
      ${showGenericSettings ? `<article class="card flat"><h2>Settings</h2><form class="form-grid config-form" data-config-form="${esc(module.id)}" style="margin-top:12px">${fields.map((field) => fieldControl(module, field)).join("")}<div class="row wrap"><button class="button" type="submit">Save</button><button class="button ghost" type="button" data-reset-config="${esc(module.id)}">Reset</button><span class="small muted" data-config-status="${esc(module.id)}"></span></div></form></article>` : ""}
      ${isAgentHarness ? `<article class="card flat"><div class="row spread"><h2>Agent settings</h2><span class="small muted">Settings → Agents</span></div><p class="small muted" style="margin:8px 0 12px">Agent defaults, the agent registry, and workspace behavior are managed in Settings → Agents.</p><button class="button ghost small-button" type="button" data-open-agent-settings>Open Agents settings</button></article>` : ""}
      ${showCustomSettings ? `<article class="card flat settings-ui-card"><iframe class="settings-frame" data-settings-frame="${esc(module.id)}" title="${esc(module.name)} settings" allow="clipboard-write" allowfullscreen sandbox="allow-scripts allow-forms allow-popups"></iframe></article>` : ""}
    </div>`;
    mountSettingsFrames(panel);
    panel.querySelectorAll("[data-open]").forEach((button) => button.onclick = () => selectModule(button.dataset.open));
    panel.querySelectorAll("[data-open-agent-settings]").forEach((button) => button.onclick = () => {
      state.settingsTab = AGENTS_TAB;
      renderSettings();
    });
    panel.querySelectorAll("[data-page-toggle]").forEach((input) => input.onchange = () => {
      setPageVisible(input.dataset.pageToggle, input.checked);
      renderNav();
      render();
    });
    panel.querySelectorAll("[data-review]").forEach((button) => button.onclick = () => openPermissions(button.dataset.review, state.node));
    panel.querySelectorAll("[data-toggle]").forEach((button) => button.onclick = () => toggleModule(button.dataset.toggle, button.dataset.enabled !== "true"));
    panel.querySelectorAll("[data-remove]").forEach((button) => button.onclick = async () => {
      if (!confirm(`Remove ${button.dataset.remove} from ${machineName(state.node)}?`)) return;
      try {
        await api(adminModuleApi(button.dataset.remove), { method: "DELETE", body: "{}" });
        state.settingsTab = "modules";
        state.settingsModule = null;
        await refresh();
        toast("Module removed", button.dataset.remove, "ok");
      } catch (error) { toast("Could not remove module", error.message, "bad"); }
    });
    panel.querySelectorAll("[data-git-update]").forEach((button) => button.onclick = async () => {
      button.disabled = true;
      const update = (allowUnsigned) => api(adminModuleApi(button.dataset.gitUpdate, "/update"), {
        method: "POST",
        body: JSON.stringify({ allow_unsigned: allowUnsigned }),
      });
      try {
        try {
          await update(false);
        } catch (error) {
          if (!/unsigned/i.test(error.message)) throw error;
          if (!confirm(`This module update is not signed by a trusted publisher. Update it on ${machineName(state.node)} anyway? Only do this for code you trust.`)) {
            button.disabled = false;
            return;
          }
          await update(true);
        }
        await refresh();
        renderSettings();
        toast("Module updated", button.dataset.gitUpdate, "ok");
      } catch (error) {
        button.disabled = false;
        toast("Could not update module", error.message, "bad");
      }
    });
    panel.querySelectorAll("[data-git-checkpoint]").forEach((button) => button.onclick = async () => {
      const message = prompt("Checkpoint message", `Checkpoint ${module.name}`);
      if (message === null) return;
      try {
        await api(adminModuleApi(button.dataset.gitCheckpoint, "/checkpoint"), { method: "POST", body: JSON.stringify({ message }) });
        await refresh();
        renderSettings();
        toast("Module checkpointed", module.name, "ok");
      } catch (error) { toast("Could not checkpoint module", error.message, "bad"); }
    });
    panel.querySelectorAll("[data-sidecar-restart]").forEach((button) => button.onclick = async () => {
      button.disabled = true;
      try {
        await api(adminModuleApi(button.dataset.sidecarRestart, "/sidecar/restart"), { method: "POST", body: "{}" });
        await refresh();
        renderSettings();
        toast("Sidecar restarted", module.name, "ok");
      } catch (error) {
        await refresh();
        renderSettings();
        toast("Could not restart sidecar", error.message, "bad");
      }
    });
    panel.querySelectorAll("[data-sidecar-log]").forEach((button) => button.onclick = async () => {
      const log = panel.querySelector(`[data-sidecar-log-panel="${CSS.escape(button.dataset.sidecarLog)}"]`);
      if (!log) return;
      if (!log.hidden) {
        log.hidden = true;
        button.textContent = "Show log";
        return;
      }
      button.disabled = true;
      try {
        const data = await api(adminModuleApi(button.dataset.sidecarLog, "/log"));
        log.textContent = data.text || "No sidecar log yet.";
        log.hidden = false;
        button.textContent = "Hide log";
        log.scrollTop = log.scrollHeight;
      } catch (error) {
        toast("Could not read sidecar log", error.message, "bad");
      } finally { button.disabled = false; }
    });
    bindModuleConfigForm(module, panel);
  }

  function bindModuleConfigForm(module, root, node = state.node || "local") {
    const form = root.querySelector(`[data-config-form="${CSS.escape(module.id)}"]`);
    if (!form) return;
    const configPath = `/api/v1/nodes/${encodeURIComponent(node)}/modules/${encodeURIComponent(module.id)}/config`;
    form.onsubmit = async (event) => {
      event.preventDefault();
      const values = {};
      form.querySelectorAll("[data-config-field]").forEach((input) => {
        const key = input.dataset.configField;
        if (input.dataset.configType === "boolean") values[key] = input.checked;
        else if (input.dataset.configType === "number") values[key] = Number(input.value);
        else values[key] = input.value;
      });
      const status = form.querySelector("[data-config-status]");
      if (status) status.textContent = "saving…";
      try {
        const response = await api(configPath, { method: "POST", body: JSON.stringify({ values }) });
        const updated = response.module;
        const modules = node === "local" ? state.localModules : state.modules;
        const index = modules.findIndex((entry) => entry.id === module.id);
        if (index >= 0 && updated) modules[index] = { ...modules[index], ...updated };
        if (state.module === module.id) sendContext();
        if ((state.settingsTab === "modules" && state.settingsModule === module.id)
          || (state.settingsTab === AGENTS_TAB && module.id === AGENT_HARNESS_ID)) {
          sendSettingsContext(module.id);
        }
        if (status) status.textContent = "saved";
        toast("Settings saved", module.name, "ok");
      } catch (error) {
        if (status) status.textContent = "";
        toast("Could not save settings", error.message, "bad");
      }
    };
    const reset = root.querySelector(`[data-reset-config="${CSS.escape(module.id)}"]`);
    if (reset) reset.onclick = () => {
      for (const field of module.config_schema.fields) {
        const input = form.querySelector(`[data-config-field="${CSS.escape(field.key)}"]`);
        if (!input) continue;
        if (input.dataset.configType === "boolean") input.checked = !!field.default;
        else input.value = field.default ?? "";
      }
    };
  }

  function fieldControl(module, field) {
    const value = module.config?.[field.key] ?? field.default;
    const help = field.help ? `<small class="config-help">${esc(field.help)}</small>` : "";
    if (field.type === "boolean") {
      return `<label class="config-check"><input type="checkbox" data-config-field="${esc(field.key)}" data-config-type="boolean" ${value ? "checked" : ""}><span><b>${esc(field.label)}</b>${help}</span></label>`;
    }
    if (field.type === "select") {
      return `<label class="config-field"><span class="form-label">${esc(field.label)}</span><select class="field" data-config-field="${esc(field.key)}" data-config-type="select">${(field.options || []).map((option) => `<option value="${esc(option.value)}" ${String(value) === String(option.value) ? "selected" : ""}>${esc(option.label)}</option>`).join("")}</select>${help}</label>`;
    }
    const type = field.type === "number" ? "number" : "text";
    const min = field.min != null ? ` min="${field.min}"` : "";
    const max = field.max != null ? ` max="${field.max}"` : "";
    const step = field.step != null ? ` step="${field.step}"` : "";
    return `<label class="config-field"><span class="form-label">${esc(field.label)}</span><input class="field" type="${type}" data-config-field="${esc(field.key)}" data-config-type="${type}" value="${esc(value)}"${min}${max}${step}>${help}</label>`;
  }

  function buildApi(node, suffix = "") {
    return `/api/v1/nodes/${encodeURIComponent(node || state.node)}/build${suffix}`;
  }

  global.OrqellaSettings = {
    install,
    renderSettings,
    renderSettingsModules,
    renderSettingsDashboards,
    renderSettingsGeneral,
    renderSettingsModule,
    fieldControl,
    buildApi,
  };
})(typeof window !== "undefined" ? window : globalThis);
