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
  let sortedLocalModules;
  let ensureSelectedSettingsModule;
  let nativeDisconnect;
  let newFrameNonce;
  let frameHash;
  let OVERVIEW_DASHBOARD_ID;
  let moduleKey;
  let pageVisible;
  let openPermissions;

  function install(ctx) {
    ({
      state, $, esc, api, toast, render, renderNav, refresh, selectModule,
      saveViewState, moduleId, frameKey, renderHome, saveSharedDashboards,
      savePageOrder, dashboardIdFromView, selectDashboard, activateDashboard,
      selectView, setPageVisible, dashboardPageKey, newDashboardId,
      defaultDashboardLayout, sendContext, sendSettingsContext, sendThemeContext,
      toggleModule, sortedLocalModules, ensureSelectedSettingsModule,
      nativeDisconnect, newFrameNonce, frameHash, OVERVIEW_DASHBOARD_ID,
      moduleKey, pageVisible, openPermissions,
    } = ctx);
  }

  function renderSettings() {
    const root = $("coreView");
    const settingsTabs = new Set(["general", "dashboards", "modules"]);
    if (!settingsTabs.has(state.settingsTab)) {
      const legacyModule = state.localModules.find((module) => module.id === state.settingsTab);
      if (legacyModule) {
        state.settingsModule = legacyModule.id;
        state.settingsTab = "modules";
      } else {
        state.settingsTab = "general";
      }
    }
    const pending = state.localModules.reduce((total, module) => total + (module.pending_permissions?.length || 0), 0);
    const tabs = [
      { id: "general", label: "General" },
      { id: "dashboards", label: "Dashboards" },
      { id: "modules", label: "Modules", pending },
    ];
    root.innerHTML = `<div class="stack"><nav class="settings-tabs" aria-label="Settings sections">${tabs.map((tab) => `
      <button class="settings-tab ${state.settingsTab === tab.id ? "active" : ""}" data-settings-tab="${esc(tab.id)}">${esc(tab.label)}${tab.pending ? `<span class="tab-count">${tab.pending}</span>` : ""}</button>`).join("")}</nav><div id="settingsPanel"></div></div>`;
    root.querySelectorAll("[data-settings-tab]").forEach((button) => button.onclick = () => {
      state.settingsTab = button.dataset.settingsTab;
      renderSettings();
    });
    if (state.settingsTab === "general") renderSettingsGeneral();
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
        <p class="small muted" style="margin:8px 0 14px">Dashboards and their cards are shared with every client connected to this machine. Card positions and sizes stay local to each device. Overview opens from the Orqella brand in the top-left.</p>
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
    const label = document.getElementById("currentThemeLabel");
    if (label) label.textContent = selected.name;
    sendThemeContext();
  }

  function renderSettingsGeneral() {
    const panel = $("settingsPanel");
    const config = state.info?.config || {};
    const themePath = state.info?.theme_path || "theme.css";
    const themes = state.info?.themes?.length ? state.info.themes : [
      { id: "nocturne", name: "Nocturne" },
    ];
    const selectedTheme = themes.find((theme) => theme.id === config.theme) || themes[0];
    const agent = config.agent || {};
    const agentViewer = agent.viewer || {};
    const resolvedViewer = agent.resolved_viewer || {};
    const agentPresets = config.agent_presets || [];
    const agentEnv = Object.entries(agent.env || {}).map(([key, value]) => `${key}=${value}`).join("\n");
    const agentPreset = agentPresets.find((item) => item.id === (agent.preset || "custom"));
    const disconnectTunnel = nativeDisconnect();
    panel.innerHTML = `<div class="stack">
      <div class="grid-2">
        <article class="card"><h2>Daemon</h2><div class="stack-sm small muted"><span>Origin <b class="mono">${esc(config.origin || location.origin)}</b></span><span>Bind <b class="mono">${esc(config.bind || "127.0.0.1")}:${esc(config.port || "")}</b></span><span>Dashboard <b>${config.dashboard ? "enabled" : "disabled"}</b></span><span>Version <b class="mono">${esc(state.info?.version || "unknown")}</b>${state.info?.commit && state.info.commit !== "unknown" ? ` · <span class="mono">${esc(state.info.commit)}</span>` : ""}</span></div><label class="row small" style="margin-top:14px;color:var(--muted)"><input type="checkbox" id="auditToggle" ${config.audit?.enabled ? "checked" : ""}> Audit changes</label></article>
        <article class="card"><div class="row spread"><h2>Theme</h2><span class="small muted" id="currentThemeLabel">${esc(selectedTheme.name)}</span></div><div class="theme-row"><select class="field" id="themeSelect" aria-label="Dashboard theme">${themes.map((theme) => `<option value="${esc(theme.id)}" ${theme.id === selectedTheme.id ? "selected" : ""}>${esc(theme.name)}</option>`).join("")}</select><button class="button ghost small-button" id="reloadTheme">Reload</button></div><div class="small muted" style="margin-top:8px">Custom overrides: <span class="mono theme-path">${esc(themePath)}</span></div></article>
      </div>
      <details class="card flat settings-disclosure" id="updateSettings">
        <summary class="settings-disclosure-head"><span class="settings-disclosure-title"><span class="settings-chevron" aria-hidden="true">›</span><h2>Updates</h2></span><span class="small muted" id="updateCurrent">Developer channel</span></summary>
        <div class="settings-disclosure-body">
        <p class="small muted" style="margin:0 0 10px">Preview update path: machines behind the configured git branch are pulled, rebuilt with Cargo, and restarted. A signed binary update channel will replace this before release.</p>
        <div class="stack-sm small" id="updateSummary"><span class="muted">Check for updates to compare this machine and its peers.</span></div>
        <div class="row wrap" style="margin-top:12px"><button class="button ghost" id="checkUpdates">Check for updates</button><button class="button" id="applyUpdates">Update connected machines</button></div>
        <pre class="build-log" id="updateLog" hidden></pre>
        </div>
      </details>
      <details class="card flat settings-disclosure" id="agentSettings">
        <summary class="settings-disclosure-head"><span class="settings-disclosure-title"><span class="settings-chevron" aria-hidden="true">›</span><h2>AI agent</h2></span><span class="small muted" id="agentCurrent">${esc(agent.preset || "custom")}</span></summary>
        <div class="settings-disclosure-body">
        <p class="small muted" style="margin:0 0 12px">Default agent for Build, VM Lab, System identification, and modules with the <span class="mono">agent</span> permission.</p>
        <div class="form-grid">
          <div><label class="form-label">Preset</label><select class="field" id="agentPreset">${agentPresets.map((item) => `<option value="${esc(item.id)}" ${item.id === (agent.preset || "custom") ? "selected" : ""}>${esc(item.name)}</option>`).join("")}</select></div>
          <div><label class="form-label">Command</label><input class="field" id="agentCommand" value="${esc(agent.command || "")}" autocomplete="off"></div>
          <div><label class="form-label">Timeout (seconds, 0 = none)</label><input class="field" id="agentTimeout" type="number" min="0" max="86400" value="${esc(agent.timeout_secs ?? 0)}"></div>
        </div>
        <label class="form-label" style="display:block;margin:12px 0 5px">Arguments — one per line</label>
        <textarea class="field" id="agentArgs" rows="4" spellcheck="false">${esc((agent.args || []).join("\n"))}</textarea>
        <label class="form-label" style="display:block;margin:12px 0 5px">Environment — KEY=VALUE per line</label>
        <textarea class="field" id="agentEnv" rows="4" spellcheck="false" placeholder="ANTHROPIC_BASE_URL=http://127.0.0.1:8080&#10;ANTHROPIC_AUTH_TOKEN=${"${"}ANTHROPIC_AUTH_TOKEN}">${esc(agentEnv)}</textarea>
        <div class="form-grid" style="margin-top:12px">
          <div><label class="form-label">Viewer module</label><input class="field" id="agentViewerModule" value="${esc(agentViewer.module || "")}" placeholder="${esc(resolvedViewer.module || "opencode")}" autocomplete="off"></div>
          <div><label class="form-label">Viewer mount</label><input class="field" id="agentViewerMount" value="${esc(agentViewer.mount || "")}" placeholder="${esc(resolvedViewer.mount || "opencode")}" autocomplete="off"></div>
          <div><label class="form-label">Viewer path</label><input class="field" id="agentViewerPath" value="${esc(agentViewer.path || "")}" placeholder="${esc(resolvedViewer.path || "/?session={session}")}" autocomplete="off"></div>
          <div><label class="form-label">External viewer URL</label><input class="field" id="agentViewerUrl" value="${esc(agentViewer.url || "")}" placeholder="https://example.com/conversation/{session}" autocomplete="off"></div>
        </div>
        <p class="small muted" style="margin-top:7px">Leave blank to use the OpenCode module for the OpenCode preset. Use <span class="mono">{session}</span> in the path or URL. A module must be installed; a mount must be a configured proxy mount.</p>
        <p class="small muted" id="agentPresetHint" style="margin-top:7px">${esc(agentPreset?.hint || "The command receives the prompt as its final argument.")}</p>
        <p class="small muted">Build and edit runs open the agent's conversation when the harness exposes a session id. Keep the timeout at <span class="mono">0</span> for long builds; a positive value stops the process and can leave a service-backed session running.</p>
        <p class="small muted">For custom endpoints, set the variables the selected CLI expects. Claude Code uses <span class="mono">ANTHROPIC_BASE_URL</span> / <span class="mono">ANTHROPIC_AUTH_TOKEN</span>; Codex can use its provider config or <span class="mono">--oss --local-provider</span>; OpenCode uses its provider config.</p>
        <div class="row wrap" style="margin-top:12px"><button class="button" id="saveAgent">Save agent</button><span class="small muted" id="agentSaveStatus"></span></div>
        </div>
      </details>
      <details class="card flat settings-disclosure" id="linkSettings">
        <summary class="settings-disclosure-head"><span class="settings-disclosure-title"><span class="settings-chevron" aria-hidden="true">›</span><h2>Link account</h2></span><span class="small muted" id="linkCurrent">Checking…</span></summary>
        <div class="settings-disclosure-body">
        <p class="small muted" style="margin:0 0 12px">Link is the paid machine transport. Local Orqella remains free. Sign in once and other machines enrolled in the same account appear automatically; managed relay credentials are issued to subscribed devices. The control plane issues tickets only, and SSH traffic stays direct.</p>
        <div id="linkBody"><div class="empty small">Loading Link status…</div></div>
        </div>
      </details>
      <details class="card flat settings-disclosure" id="githubSettings">
        <summary class="settings-disclosure-head"><span class="settings-disclosure-title"><span class="settings-chevron" aria-hidden="true">›</span><h2>GitHub publishing</h2></span><span class="small muted" id="githubCurrent">Checking…</span></summary>
        <div class="settings-disclosure-body">
        <p class="small muted" style="margin:0 0 12px">Connect a GitHub account through the Orqella GitHub App. Modules are pushed directly from this machine; the publisher bridge only mints short-lived installation tokens.</p>
        <div id="githubBody"><div class="empty small">Loading GitHub status…</div></div>
        </div>
      </details>
      ${disconnectTunnel ? `<article class="card flat" id="mobileSettings"><div class="row spread"><h2>Mobile</h2><span class="small muted">This session</span></div><p class="small muted" style="margin:8px 0 10px">Close the foreground SSH tunnel, return to the connection form, and stop reconnecting when the app opens.</p><button class="button ghost" id="disconnectTunnel">Disconnect tunnel</button></article>` : ""}
      <details class="card flat settings-disclosure" id="peerSettings">
        <summary class="settings-disclosure-head"><span class="settings-disclosure-title"><span class="settings-chevron" aria-hidden="true">›</span><h2>Machines</h2></span><span class="small muted">Connected through Link</span></summary>
        <div class="settings-disclosure-body">
          <div class="peer-list" id="peerList"></div>
          <p class="small muted" style="margin:12px 0 0">Machines report their LAN interfaces while online. Orqella caches a wake-on-LAN address automatically when one is available, so no manual setup is needed.</p>
        </div>
      </details>
      <details class="card flat settings-disclosure"><summary class="settings-disclosure-head"><span class="settings-disclosure-title"><span class="settings-chevron" aria-hidden="true">›</span><h2>Install module</h2></span><span class="small muted">Git URL or local path</span></summary><div class="settings-disclosure-body"><div class="install-bar" style="padding:0"><input class="field" id="installPath" placeholder="git URL or /path/to/module"><input class="field" id="installBranch" placeholder="branch (optional)" style="max-width:170px"><button class="button ghost" id="installButton">Install</button><button class="button" id="newModule">Build</button></div></div></details>
    </div>`;
    $("auditToggle").onchange = async (event) => {
      try {
        const result = await api("/api/v1/config/audit", { method: "POST", body: JSON.stringify({ enabled: event.target.checked }) });
        if (state.info?.config?.audit) state.info.config.audit.enabled = result.enabled;
        toast("Audit updated", result.enabled ? "Recording." : "Off.", "ok");
      } catch (error) {
        event.target.checked = !event.target.checked;
        toast("Could not update audit", error.message, "bad");
      }
    };
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
      const envLines = $("agentEnv").value.split("\n");
      for (const line of envLines) {
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
        $("agentCurrent").textContent = result.agent.preset;
        $("agentSaveStatus").textContent = "Saved";
        toast("Agent updated", result.agent.preset, "ok");
      } catch (error) {
        $("agentSaveStatus").textContent = error.message;
        toast("Could not update agent", error.message, "bad");
      } finally {
        $("saveAgent").disabled = false;
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
    if (disconnectTunnel) {
      $("disconnectTunnel").onclick = () => {
        toast("Disconnecting", "Closing the foreground SSH tunnel.", "ok");
        disconnectTunnel();
      };
    }
    $("checkUpdates").onclick = checkUpdates;
    $("applyUpdates").onclick = applyUpdates;
    $("newModule").onclick = () => selectView("build");
    loadLinkSettings();
    loadGithubSettings();
    renderPeerSettings();
    $("installButton").onclick = async () => {
      const source = $("installPath").value.trim();
      const branch = $("installBranch").value.trim();
      if (!source) return;
      const payload = /^(https?:|ssh:|git@|git:|file:)/.test(source) ? { url: source, branch } : { path: source };
      const install = (allowUnsigned) => api("/api/v1/modules/install", {
        method: "POST",
        body: JSON.stringify({ ...payload, allow_unsigned: allowUnsigned }),
      });
      try {
        let result;
        try {
          result = await install(false);
        } catch (error) {
          if (!/unsigned/i.test(error.message)) throw error;
          if (!confirm("This module is not signed by a trusted publisher. Install it anyway? Only do this for code you trust.")) return;
          result = await install(true);
        }
        await refresh();
        if (result?.module?.id) {
          state.settingsTab = "modules";
          state.settingsModule = result.module.id;
          renderSettings();
        }
        if (result?.sidecar_error) {
          toast("Module installed with a setup error", result.sidecar_error, "bad");
        } else {
          toast("Module installed", source, "ok");
        }
      } catch (error) { toast("Install failed", error.message, "bad"); }
    };
  }


  function updateSource(value) {
    return value?.result || value || {};
  }

  function updateSourceLine(label, data) {
    if (!data) return `<span><b>${esc(label)}</b> · unavailable</span>`;
    const source = updateSource(data);
    if (data.ok === false || source.ok === false) {
      const detail = data.error || source.error || "check failed";
      return `<span><b>${esc(label)}</b> · ${esc(detail)}</span>`;
    }
    const current = source.current_short || "?";
    const latest = source.latest_short || "?";
    const state = source.available ? `${source.behind || 0} behind` : "up to date";
    const dirty = source.dirty ? " · local changes" : "";
    return `<span><b>${esc(label)}</b> · <span class="mono">${esc(current)} → ${esc(latest)}</span> · ${esc(state)}${dirty}</span>`;
  }

  function updatePlan(data) {
    const items = [];
    const add = (label, value) => {
      const source = updateSource(value);
      let action = "update";
      let reason = "";
      if (!value || value.ok === false || source.ok === false) {
        action = "blocked";
        reason = value?.error || source.error || "check failed";
      } else if (!source.available) {
        action = "skip";
        reason = "up to date";
      } else if (source.dirty) {
        action = "blocked";
        reason = "local changes; commit or stash first";
      }
      items.push({ label, action, reason, value, source });
    };
    add("this machine", data.local);
    (data.peers || []).forEach((peer) => add(peer.name || peer.id, peer));
    return items;
  }

  function renderUpdateCheck(data) {
    const root = $("updateSummary");
    if (!root) return;
    state.updateCheck = data;
    const items = updatePlan(data);
    root.innerHTML = items.map((item) => updateSourceLine(item.label, item.value)).join("");
    const outdated = items.filter((item) => item.action === "update").length;
    const blocked = items.filter((item) => item.action === "blocked").length;
    const current = $("updateCurrent");
    if (current) current.textContent = state.updatePolling ? "Updating…" : outdated ? `${outdated} outdated` : blocked ? `${blocked} blocked` : "Up to date";
    const button = $("applyUpdates");
    if (button) {
      button.disabled = state.updatePolling || outdated === 0;
      button.textContent = state.updatePolling ? "Updating…" : outdated ? `Update ${outdated} machine${outdated === 1 ? "" : "s"}` : "Nothing to update";
    }
  }

  async function checkUpdates(silent = false) {
    const button = $("checkUpdates");
    if (button) button.disabled = true;
    const root = $("updateSummary");
    if (!silent && root) root.innerHTML = `<span class="muted">Checking this machine and connected machines…</span>`;
    try {
      const data = await api("/api/v1/update/check");
      renderUpdateCheck(data);
      return data;
    } catch (error) {
      if (root) root.innerHTML = `<span class="bad">Check failed: ${esc(error.message)}</span>`;
      return null;
    } finally {
      if (button) button.disabled = false;
    }
  }

  async function pollUpdate() {
    try {
      const status = await api("/api/v1/update/status");
      $("updateLog").hidden = false;
      $("updateLog").textContent = (status.log || `${status.status || "working"}${status.phase ? ` · ${status.phase}` : ""}`);
      $("updateLog").scrollTop = $("updateLog").scrollHeight;
      $("updateSummary").innerHTML = `<span class="muted">${esc(status.status || "working")}${status.phase ? ` · ${esc(status.phase)}` : ""}${status.error ? ` · ${esc(status.error)}` : ""}</span>`;
      if (status.status === "succeeded") {
        setTimeout(() => location.reload(), 1800);
        return;
      }
      if (status.status === "failed") {
        const button = $("applyUpdates");
        if (button) button.disabled = false;
        return;
      }
      setTimeout(pollUpdate, 2000);
    } catch {
      $("updateSummary").innerHTML = `<span class="muted">Daemon restarting…</span>`;
      setTimeout(pollUpdate, 2500);
    }
  }

  async function pollPeerUpdates(deadline, peerIds) {
    const [data, statuses] = await Promise.all([
      checkUpdates(true),
      api("/api/v1/update/peer-status").catch(() => []),
    ]);
    if (!data) {
      state.updatePolling = false;
      return;
    }
    const statusList = Array.isArray(statuses) ? statuses : [];
    const targetStatuses = statusList.filter((item) => (peerIds || []).includes(item.id));
    const failed = targetStatuses.some((item) => item.status?.status === "failed");
    const pending = (peerIds || []).some((id) => {
      const entry = statusList.find((item) => item.id === id);
      if (!entry || entry.ok === false) return true;
      const status = entry.status?.status;
      return status === "queued" || status === "running";
    });
    const outdated = updatePlan(data).filter((item) => item.action === "update");
    if ((pending || outdated.length) && !failed && Date.now() < deadline) {
      setTimeout(() => pollPeerUpdates(deadline, peerIds), 10000);
      return;
    }
    state.updatePolling = false;
    renderUpdateCheck(data);
    if (failed) {
      const root = $("updateSummary");
      if (root) {
        root.innerHTML += targetStatuses
          .filter((item) => item.status?.status === "failed")
          .map((item) => `<span class="bad">${esc(item.name || item.id)}: ${esc(item.status?.error || "update failed")}</span>`)
          .join("");
      }
    }
  }

  async function applyUpdates() {
    const button = $("applyUpdates");
    const root = $("updateSummary");
    const log = $("updateLog");
    if (button) button.disabled = true;
    if (root) root.innerHTML = `<span class="muted">Checking machines…</span>`;
    let check;
    try {
      check = await api("/api/v1/update/check");
    } catch (error) {
      if (root) root.innerHTML = `<span class="bad">Check failed: ${esc(error.message)}</span>`;
      if (button) button.disabled = false;
      return;
    }
    renderUpdateCheck(check);
    const plan = updatePlan(check);
    const targets = plan.filter((item) => item.action === "update");
    const blocked = plan.filter((item) => item.action === "blocked");
    if (!targets.length) {
      if (root) root.innerHTML = blocked.length
        ? `<span class="muted">Nothing can be updated. ${blocked.map((item) => `${esc(item.label)}: ${esc(item.reason)}`).join("; ")}.</span>`
        : `<span class="muted">All machines are up to date.</span>`;
      if (button) button.disabled = false;
      return;
    }
    const names = targets.map((item) => item.label).join(", ");
    if (!window.confirm(`Update ${targets.length} machine${targets.length === 1 ? "" : "s"} (${names})? Only those machines will rebuild and restart. This is the developer update path.`)) {
      if (button) button.disabled = false;
      return;
    }
    if (log) {
      log.hidden = false;
      log.textContent = "Starting update…";
    }
    try {
      const result = await api("/api/v1/update/apply", { method: "POST", body: "{}" });
      const local = result.local || {};
      const peers = result.peers || [];
      const acceptedPeers = peers.filter((peer) => peer.action === "accepted");
      const accepted = [
        ...(local.action === "accepted" ? ["this machine"] : []),
        ...acceptedPeers.map((peer) => peer.name || peer.id),
      ];
      const failed = [
        ...(local.action === "failed" || local.action === "blocked" ? [`this machine: ${local.reason || local.action}`] : []),
        ...peers
          .filter((peer) => peer.action === "failed" || peer.action === "blocked")
          .map((peer) => `${peer.name || peer.id}: ${peer.reason || peer.action}`),
      ];
      if (root) root.innerHTML = `<span class="muted">Update started for ${accepted.length} machine${accepted.length === 1 ? "" : "s"}${failed.length ? `; ${failed.length} blocked or failed` : ""}.</span>`;
      if (log) log.textContent = failed.length ? failed.join("\n") : "Waiting for the update to finish…";
      if (local.action === "accepted") {
        setTimeout(pollUpdate, 1500);
      } else if (accepted.length) {
        state.updatePolling = true;
        renderUpdateCheck(check);
        setTimeout(
          () => pollPeerUpdates(Date.now() + 10 * 60 * 1000, acceptedPeers.map((peer) => peer.id)),
          10000
        );
      } else {
        if (button) button.disabled = false;
      }
    } catch (error) {
      if (log) log.textContent = error.message || "Could not start update";
      if (button) button.disabled = false;
    }
  }

  function renderSettingsModule(id, root = $("settingsPanel")) {
    const panel = root;
    const module = state.localModules.find((entry) => entry.id === id);
    if (!module) {
      state.settingsModule = null;
      state.settingsTab = "modules";
      return renderSettings();
    }
    const fields = module.config_schema?.fields || [];
    const dependencies = module.dependencies || [];
    const installedIds = new Set(state.modules.map((entry) => entry.id));
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
    const settingsNonce = newFrameNonce();
    const gitLine = git.is_repo
      ? `${git.repository_name ? `<b>${esc(git.repository_name)}</b> · ` : "No GitHub remote · "}${esc(git.branch || "detached")} · <span class="mono">${esc(git.commit_short || "no commit")}</span>${git.dirty ? ` · <span class="chip warn">uncommitted</span>` : ""}${git.behind ? ` · ${git.behind} behind` : ""}${git.ahead ? ` · ${git.ahead} ahead` : ""}`
      : "Not a git repository";
    panel.innerHTML = `<div class="stack">
      <article class="card">
        <div class="row spread"><h2>${esc(module.name)}</h2><span class="chip ${module.enabled ? "ok" : "bad"}">${module.enabled ? "enabled" : "disabled"}</span></div>
        <div class="row wrap module-meta"><span class="chip">${esc(module.version)}</span><span class="chip">${esc(module.backend)}</span>${module.backend === "sidecar" && module.backend_sandbox === false ? `<span class="chip bad">unsandboxed</span>` : ""}${module.has_page ? `<span class="chip">page</span>` : ""}${module.has_overview ? `<span class="chip">overview</span>` : ""}${module.node_scope ? `<span class="chip">${esc(module.node_scope)}</span>` : ""}</div>
        <div class="row wrap" style="margin-top:14px"><button class="button ghost" data-toggle="${esc(module.id)}" data-enabled="${module.enabled}">${module.enabled ? "Disable" : "Enable"}</button>${module.has_page && module.enabled && pageVisible(module) ? `<button class="button ghost" data-open="${esc(module.id)}">Open</button>` : ""}${module.pending_permissions?.length ? `<button class="button" data-review="${esc(module.id)}">Review permissions</button>` : ""}<a class="button ghost" href="/api/v1/modules/${encodeURIComponent(module.id)}/export">Export</a><button class="button danger" data-remove="${esc(module.id)}">Remove</button></div>
        ${module.has_page ? `<label class="config-check" style="margin-top:12px"><input type="checkbox" data-page-toggle="${esc(module.id)}" ${pageVisible(module) ? "checked" : ""}><span><b>Show page tab</b></span></label>` : ""}
        <div class="perm-list">${(module.permissions || []).map((permission) => `<span class="perm" title="${esc(permission === "agent" ? "Runs your configured coding agent with your user privileges" : permission === "backend" ? "Runs a sidecar process" : "")}">${esc(permission)}</span>`).join("") || '<span class="small muted">No host permissions</span>'}</div>
        ${(module.permissions || []).includes("agent") ? `<p class="small bad" style="margin:8px 0 0">The agent permission can run your configured coding agent, which can execute commands and read files as you. Grant it only to code you trust.</p>` : ""}
        ${dependencies.length ? `<div class="stack-sm small" style="margin-top:10px"><b>Dependencies</b><div class="row wrap">${dependencyChips}</div></div>` : ""}
      </article>
      <article class="card flat">
        <div class="row spread"><h2>Module repository</h2><span class="small muted">Git</span></div>
        <p class="small muted" style="margin:8px 0 10px">${gitLine}</p>
        <p class="small muted" style="margin:0 0 10px">Source <span class="mono">${esc(module.source || "unknown")}</span></p>
        <div class="row wrap"><button class="button ghost small-button" data-git-update="${esc(module.id)}" ${git.is_repo ? "" : "disabled"}>Update from git (developer)</button><button class="button ghost small-button" data-git-checkpoint="${esc(module.id)}">Checkpoint</button><button class="button small-button" data-git-publish="${esc(module.id)}">Publish to GitHub</button></div>
      </article>
      ${module.backend === "sidecar" ? `<article class="card flat">
        <div class="row spread"><h2>Sidecar</h2><span class="chip ${sidecarChip}">${esc(sidecarState)}</span></div>
        <p class="small muted" style="margin:8px 0 0">${sidecar.attempts ? `${sidecar.attempts} failed attempt${sidecar.attempts === 1 ? "" : "s"}${sidecar.next_retry_in_secs ? ` · retry in ${sidecar.next_retry_in_secs}s` : ""}` : "Process is managed by the daemon supervisor."}</p>
        ${sidecar.last_error ? `<p class="small bad" style="margin:8px 0 0">${esc(sidecar.last_error)}</p>` : ""}
        <p class="small muted" style="margin:8px 0 0">Command <span class="mono">${esc((module.backend_command || []).join(" ") || "none")}</span> · ${module.backend_network ? "network opt-in requested" : "network policy pending"}${module.backend_sandbox === false ? " · unsandboxed" : " · sandboxed"}</p>
        ${(module.backend_read_paths || []).length ? `<p class="small muted" style="margin:8px 0 0">Host read access: ${module.backend_read_paths.map((path) => `<span class="mono">${esc(path)}</span>`).join(", ")}</p>` : ""}
        <div class="row wrap" style="margin-top:12px"><button class="button ghost small-button" data-sidecar-restart="${esc(module.id)}" ${module.enabled ? "" : "disabled"}>Restart sidecar</button><button class="button ghost small-button" data-sidecar-log="${esc(module.id)}">Show log</button></div>
        <pre class="build-log" data-sidecar-log-panel="${esc(module.id)}" hidden style="margin-top:10px"></pre>
      </article>` : ""}
      ${module.has_settings ? `<article class="card flat settings-ui-card"><iframe class="settings-frame" data-settings-frame="${esc(module.id)}" src="/m/${encodeURIComponent(module.id)}/settings${frameHash(settingsNonce, module.ws_token)}" title="${esc(module.name)} settings" allow="clipboard-write" allowfullscreen sandbox="allow-scripts allow-forms allow-popups"></iframe></article>` : ""}
      ${fields.length ? `<article class="card flat"><h2>Settings</h2><form class="form-grid config-form" data-config-form="${esc(module.id)}" style="margin-top:12px">${fields.map((field) => fieldControl(module, field)).join("")}<div class="row wrap"><button class="button" type="submit">Save</button><button class="button ghost" type="button" data-reset-config="${esc(module.id)}">Reset</button><span class="small muted" data-config-status="${esc(module.id)}"></span></div></form></article>` : ""}
    </div>`;
    for (const key of [...state.readyFrames]) {
      if (key.startsWith("settings:")) state.readyFrames.delete(key);
    }
    state.settingsFrames = new Map();
    const settingsFrame = panel.querySelector("iframe[data-settings-frame]");
    if (settingsFrame) {
      const moduleId = settingsFrame.dataset.settingsFrame;
      settingsFrame.dataset.frameNonce = settingsNonce;
      state.settingsFrames.set(moduleId, settingsFrame);
      settingsFrame.addEventListener("load", () => {
        state.readyFrames.delete(frameKey("settings", moduleId));
      });
    }
    panel.querySelectorAll("[data-open]").forEach((button) => button.onclick = () => selectModule(button.dataset.open));
    panel.querySelectorAll("[data-page-toggle]").forEach((input) => input.onchange = () => {
      setPageVisible(input.dataset.pageToggle, input.checked);
      renderNav();
      render();
    });
    panel.querySelectorAll("[data-review]").forEach((button) => button.onclick = () => openPermissions(button.dataset.review));
    panel.querySelectorAll("[data-toggle]").forEach((button) => button.onclick = () => toggleModule(button.dataset.toggle, button.dataset.enabled !== "true"));
    panel.querySelectorAll("[data-remove]").forEach((button) => button.onclick = async () => {
      if (!confirm(`Remove ${button.dataset.remove}?`)) return;
      try {
        await api(`/api/v1/modules/${encodeURIComponent(button.dataset.remove)}`, { method: "DELETE", body: "{}" });
        state.settingsTab = "modules";
        state.settingsModule = null;
        await refresh();
        toast("Module removed", button.dataset.remove, "ok");
      } catch (error) { toast("Could not remove module", error.message, "bad"); }
    });
    panel.querySelectorAll("[data-git-update]").forEach((button) => button.onclick = async () => {
      button.disabled = true;
      const update = (allowUnsigned) => api(`/api/v1/modules/${encodeURIComponent(button.dataset.gitUpdate)}/update`, {
        method: "POST",
        body: JSON.stringify({ allow_unsigned: allowUnsigned }),
      });
      try {
        try {
          await update(false);
        } catch (error) {
          if (!/unsigned/i.test(error.message)) throw error;
          if (!confirm("This module update is not signed by a trusted publisher. Update it anyway? Only do this for code you trust.")) {
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
        await api(`/api/v1/modules/${encodeURIComponent(button.dataset.gitCheckpoint)}/checkpoint`, { method: "POST", body: JSON.stringify({ message }) });
        await refresh();
        renderSettings();
        toast("Module checkpointed", module.name, "ok");
      } catch (error) { toast("Could not checkpoint module", error.message, "bad"); }
    });
    panel.querySelectorAll("[data-sidecar-restart]").forEach((button) => button.onclick = async () => {
      button.disabled = true;
      try {
        await api(`/api/v1/modules/${encodeURIComponent(button.dataset.sidecarRestart)}/sidecar/restart`, { method: "POST", body: "{}" });
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
        const data = await api(`/api/v1/modules/${encodeURIComponent(button.dataset.sidecarLog)}/log`);
        log.textContent = data.text || "No sidecar log yet.";
        log.hidden = false;
        button.textContent = "Hide log";
        log.scrollTop = log.scrollHeight;
      } catch (error) {
        toast("Could not read sidecar log", error.message, "bad");
      } finally { button.disabled = false; }
    });
    panel.querySelectorAll("[data-git-publish]").forEach((button) => button.onclick = () => {
      openPublishDialog(module);
    });
    bindModuleConfigForm(module, panel);
  }

  async function openPublishDialog(module) {
    let status;
    try {
      status = await api("/api/v1/github");
    } catch (error) {
      toast("Could not check GitHub", error.message, "bad");
      return;
    }
    if (!status.connected) {
      toast("GitHub is not connected", "Connect GitHub in Settings → General first.", "bad");
      return;
    }
    const git = module.git || {};
    const existing = git.repository_name || "";
    const recorded = git.source_repository || "";
    const trusted = recorded || existing;
    const account = status.account_login || "";
    const owner = existing.includes("/") ? existing.split("/")[0] : "";
    const canFork = Boolean(existing && account && owner.toLowerCase() !== account.toLowerCase());
    state.publishModule = module.id;
    $("publishTitle").textContent = `Publish ${module.name}`;
    $("publishIntro").textContent = trusted
      ? `This module is recorded from ${trusted}. Confirm the repository and visibility before publishing.`
      : `Publish this module to a repository in @${account}.`;
    $("publishRepository").disabled = false;
    $("publishRepository").value = existing;
    $("publishRepository").placeholder = "owner/name";
    $("publishRepositoryHelp").textContent = trusted
      ? `Recorded source: ${trusted}. Leave it unchanged to publish there.`
      : "Leave blank to create a new repository in the connected account. Enter an existing owner/name to publish there instead.";
    $("publishConfirm").value = "";
    $("publishConfirmRow").hidden = true;
    $("publishVisibility").value = "private";
    $("publishVisibility").disabled = false;
    $("publishVisibilityRow").hidden = false;
    $("publishFork").checked = false;
    $("publishForkRow").hidden = !canFork;
    $("publishForkHelp").textContent = canFork
      ? `Create a fork of ${existing} in @${account}, then push this module to the fork. Forks keep the upstream repository visibility.`
      : "";
    $("publishStatus").textContent = "";
    $("confirmPublish").disabled = false;
    $("confirmPublish").textContent = "Publish";
    $("cancelPublish").disabled = false;
    $("closePublish").disabled = false;

    const syncRepoConfirm = () => {
      const value = $("publishRepository").value.trim();
      const changed = Boolean(
        trusted && value && value.toLowerCase() !== trusted.toLowerCase()
      );
      $("publishConfirmRow").hidden = !changed || $("publishFork").checked;
      if (!changed) $("publishConfirm").value = "";
      if (changed) {
        $("publishConfirmHelp").textContent =
          `This module is recorded from ${trusted}. Retype ${value} to confirm the repository change.`;
      }
    };
    const syncForkMode = () => {
      const forking = $("publishFork").checked;
      $("publishRepository").disabled = forking;
      $("publishRepository").value = forking ? "" : existing;
      $("publishRepository").placeholder = forking ? `Fork of ${existing}` : "owner/name";
      $("publishVisibilityRow").hidden = forking;
      $("publishConfirm").value = "";
      syncRepoConfirm();
    };
    $("publishRepository").oninput = syncRepoConfirm;
    $("publishFork").onchange = syncForkMode;
    syncForkMode();

    $("closePublish").onclick = closePublishDialog;
    $("cancelPublish").onclick = closePublishDialog;
    $("publishModal").onclick = (event) => {
      if (event.target === $("publishModal")) closePublishDialog();
    };
    $("confirmPublish").onclick = async () => {
      const moduleId = state.publishModule;
      if (!moduleId) return;
      const forking = $("publishFork").checked;
      const repository = forking ? "" : $("publishRepository").value.trim();
      const confirmRepository = forking ? "" : $("publishConfirm").value.trim();
      const visibility = $("publishVisibility").value;
      if (
        !forking &&
        trusted &&
        repository &&
        repository.toLowerCase() !== trusted.toLowerCase() &&
        confirmRepository.toLowerCase() !== repository.toLowerCase()
      ) {
        $("publishStatus").textContent = `Retype ${repository} to confirm the repository change.`;
        $("publishConfirm").focus();
        return;
      }
      $("confirmPublish").disabled = true;
      $("cancelPublish").disabled = true;
      $("closePublish").disabled = true;
      $("confirmPublish").textContent = forking ? "Forking…" : "Publishing…";
      $("publishStatus").textContent = forking
        ? "Creating the fork, then pushing this module…"
        : "Pushing this module to GitHub…";
      try {
        const result = await api("/api/v1/github/publish", {
          method: "POST",
          body: JSON.stringify({
            module_id: moduleId,
            repository,
            confirm_repository: confirmRepository,
            private: forking ? false : visibility === "private",
            fork: forking,
          }),
        });
        closePublishDialog();
        await refresh();
        renderSettings();
        toast(result.forked ? "Published fork" : "Published to GitHub", result.repository, "ok");
        if (result.url) window.open(result.url, "_blank", "noopener,noreferrer");
      } catch (error) {
        $("publishStatus").textContent = error.message;
        toast("Could not publish module", error.message, "bad");
      } finally {
        $("confirmPublish").disabled = false;
        $("confirmPublish").textContent = "Publish";
        $("cancelPublish").disabled = false;
        $("closePublish").disabled = false;
      }
    };

    $("publishModal").hidden = false;
    $("publishRepository").focus();
  }

  function closePublishDialog() {
    $("publishModal").hidden = true;
    $("publishStatus").textContent = "";
    state.publishModule = null;
  }

  function bindModuleConfigForm(module, root) {
    const form = root.querySelector(`[data-config-form="${CSS.escape(module.id)}"]`);
    if (!form) return;
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
        const response = await api(`/api/v1/modules/${encodeURIComponent(module.id)}/config`, { method: "POST", body: JSON.stringify({ values }) });
        const updated = response.module;
        const index = state.modules.findIndex((entry) => entry.id === module.id);
        if (index >= 0 && updated) state.modules[index] = { ...state.modules[index], ...updated };
        if (state.module === module.id) sendContext();
        if (state.settingsTab === "modules" && state.settingsModule === module.id) sendSettingsContext(module.id);
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

  async function loadGithubSettings() {
    const body = $("githubBody");
    if (!body) return;
    if (state.githubPoll) {
      clearInterval(state.githubPoll);
      state.githubPoll = null;
    }
    try {
      const status = await api("/api/v1/github");
      if (!$("githubBody")) return;
      const config = state.info?.config?.github || {};
      $("githubCurrent").textContent = status.connected ? `@${status.account_login}` : "Not connected";
      if (status.connected) {
        body.innerHTML = `<div class="stack-sm small muted"><span>Account <b>@${esc(status.account_login || "")}</b>${status.account_type ? ` · ${esc(status.account_type)}` : ""}</span><span>Installation <b class="mono">${esc(status.installation_id || "")}</b></span><span>Connected <b>${esc(status.connected_at || "")}</b></span><span>Bridge <b class="mono">${esc(status.bridge_url || "")}</b></span></div><div class="row wrap" style="margin-top:12px"><button class="button ghost small-button" id="githubRefresh">Refresh</button><button class="button ghost small-button" id="githubDisconnect">Disconnect</button></div>`;
        $("githubRefresh").onclick = loadGithubSettings;
        $("githubDisconnect").onclick = async () => {
          try {
            await api("/api/v1/github/disconnect", { method: "POST", body: "{}" });
            toast("GitHub disconnected", "Publisher credentials removed.", "ok");
            await loadGithubSettings();
          } catch (error) { toast("Could not disconnect GitHub", error.message, "bad"); }
        };
        return;
      }
      body.innerHTML = `<div class="form-grid"><div><label class="form-label">Publisher bridge</label><input class="field" id="githubBridge" value="${esc(status.bridge_url || config.bridge_url || "")}" placeholder="https://github.example.com"></div></div><div class="row wrap" style="margin-top:12px"><button class="button" id="githubConnect">Connect GitHub</button><span class="small muted" id="githubConnectStatus"></span></div><p class="small muted" style="margin-top:10px">The app needs Contents write and Administration write so it can create a module repository, fork a module, and push commits. See <span class="mono">docs/MODULE_REPOS.md</span>.</p>`;
      $("githubConnect").onclick = async () => {
        $("githubConnect").disabled = true;
        $("githubConnectStatus").textContent = "Starting…";
        try {
          await api("/api/v1/github/config", {
            method: "POST",
            body: JSON.stringify({ bridge_url: $("githubBridge").value.trim() }),
          });
          const session = await api("/api/v1/github/connect", { method: "POST", body: "{}" });
          window.open(session.connect_url, "_blank", "noopener,noreferrer");
          $("githubConnectStatus").textContent = "Finish in the browser; waiting for GitHub…";
          const expires = Date.now() + 10 * 60 * 1000;
          state.githubPoll = setInterval(async () => {
            if (!$("githubBody")) {
              clearInterval(state.githubPoll);
              state.githubPoll = null;
              return;
            }
            if (Date.now() > expires) {
              clearInterval(state.githubPoll);
              state.githubPoll = null;
              $("githubConnectStatus").textContent = "Connection timed out.";
              $("githubConnect").disabled = false;
              return;
            }
            try {
              const result = await api("/api/v1/github/connect/poll", {
                method: "POST",
                body: JSON.stringify({ session_id: session.id }),
              });
              if (result.connected) {
                clearInterval(state.githubPoll);
                state.githubPoll = null;
                toast("GitHub connected", `@${result.account_login}`, "ok");
                await loadGithubSettings();
              }
            } catch (error) {
              $("githubConnectStatus").textContent = error.message;
            }
          }, 2500);
        } catch (error) {
          $("githubConnect").disabled = false;
          $("githubConnectStatus").textContent = error.message;
          toast("Could not start GitHub connection", error.message, "bad");
        }
      };
    } catch (error) {
      if ($("githubBody")) $("githubBody").innerHTML = `<div class="small" style="color:var(--red)">${esc(error.message)}</div>`;
      if ($("githubCurrent")) $("githubCurrent").textContent = "Unavailable";
    }
  }

  async function loadLinkSettings() {
    const body = $("linkBody");
    if (!body) return;
    try {
      const result = await api("/api/v1/link");
      if (!$("linkBody")) return;
      const status = result.status || {};
      $("linkCurrent").textContent = status.enabled ? (status.device_name || "connected") : "Not connected";
      const error = result.error ? `<div class="small" style="color:var(--red);margin-bottom:9px">${esc(result.error)}</div>` : "";
      if (!status.enabled) {
        body.innerHTML = `${error}<div class="form-grid"><div><label class="form-label">Email</label><input class="field" id="linkEmail" type="email" autocomplete="username"></div><div><label class="form-label">Password</label><input class="field" id="linkPassword" type="password" autocomplete="current-password"></div></div><div class="row wrap" style="margin-top:12px"><button class="button" id="linkSignIn">Sign in</button><button class="button ghost" id="linkBrowserSignIn">Sign in with browser</button><span class="small muted" id="linkStatus"></span></div><p class="small muted" style="margin-top:10px">Browser sign-in opens the passkey page on the Orqella Link service, then approves this machine.</p>`;
        $("linkSignIn").onclick = async () => {
          const email = $("linkEmail").value.trim();
          const password = $("linkPassword").value;
          if (!email || !password) return;
          $("linkSignIn").disabled = true;
          $("linkStatus").textContent = "Signing in…";
          try {
            await api("/api/v1/link/login", {
              method: "POST",
              body: JSON.stringify({ email, password }),
            });
            toast("Link signed in", "This machine is registered.", "ok");
            await loadLinkSettings();
          } catch (signInError) {
            $("linkSignIn").disabled = false;
            $("linkStatus").textContent = signInError.message;
            toast("Could not sign in to Link", signInError.message, "bad");
          }
        };
        $("linkBrowserSignIn").onclick = async () => {
          $("linkBrowserSignIn").disabled = true;
          $("linkStatus").textContent = "Starting browser sign-in…";
          try {
            const start = await api("/api/v1/link/device/start", {
              method: "POST",
              body: "{}",
            });
            $("linkStatus").innerHTML = `Approve code <b class="mono">${esc(start.user_code)}</b> with confirmation code <b class="mono">${esc(start.confirmation_code || "")}</b> at <a href="${esc(start.verification_uri)}" target="_blank" rel="noopener noreferrer">the Link approval page</a>.`;
            window.open(start.verification_uri, "_blank", "noopener,noreferrer");
            const deadline = Date.now() + Math.max(1, Number(start.expires_in) || 600) * 1000;
            const interval = Math.max(1, Number(start.interval) || 2) * 1000;
            while (Date.now() < deadline) {
              await new Promise((resolve) => setTimeout(resolve, interval));
              const result = await api("/api/v1/link/device/poll", {
                method: "POST",
                body: JSON.stringify({ device_code: start.device_code }),
              });
              if (result.status === "approved") {
                toast("Link signed in", "This machine is registered.", "ok");
                await loadLinkSettings();
                return;
              }
            }
            throw new Error("The browser sign-in code expired. Try again.");
          } catch (browserError) {
            $("linkStatus").textContent = browserError.message;
            toast("Could not sign in with the browser", browserError.message, "bad");
          } finally {
            if ($("linkBrowserSignIn")) $("linkBrowserSignIn").disabled = false;
          }
        };
        return;
      }
      let devices = [];
      let devicesError = "";
      try {
        devices = (await api("/api/v1/link/devices")).devices || [];
      } catch (deviceError) {
        devicesError = deviceError.message;
      }
      const deviceRows = devices.map((device) => `<div class="link-device-row${device.id === status.device_id ? " current" : ""}"><span class="overview-glyph">◇</span><div class="build-module-copy"><b>${esc(device.name)}${device.id === status.device_id ? " · this device" : ""}</b><small>${esc(device.id)} · ${esc(device.kind || "device")} · ${esc(device.last_seen_at || "not seen")}</small></div></div>`).join("");
      const plan = status.link_entitled
        ? `${esc(status.link_plan || "paid")} · active`
        : "Link subscription required";
      const billingLink = status.billing_url
        ? ` <a href="${esc(status.billing_url)}" target="_blank" rel="noopener noreferrer">Subscribe</a>`
        : "";
      const planWarning = status.link_entitled
        ? ""
        : `<div class="small" style="color:var(--red);margin-bottom:9px">This account does not have an active Link subscription. Local Orqella remains free; remote-machine access requires Link.${billingLink}</div>`;
      body.innerHTML = `${planWarning}${error}<div class="stack-sm small muted"><span>Plan <b>${plan}</b></span><span>Device <b class="mono">${esc(status.device_id || "")}</b></span><span>Endpoint <b class="mono">${esc(status.endpoint_id || "")}</b></span><span>Direct <b class="mono">${esc((status.direct_addresses || []).join(", ") || "none")}</b></span><span>SSH <b class="mono">${esc(status.ssh_user || "")}@${esc(status.ssh_port || "")}</b></span><span>SSH trust <b>${status.ssh_ca_configured ? "short-lived certificate" : "device keys"}</b></span><span>Host key <b class="mono">${esc(status.ssh_host_key_fingerprint || "not detected")}</b></span><span>Relay <b>${status.relay_active ? "managed" : "direct"}</b></span></div><div class="row wrap" style="margin-top:10px"><button class="button ghost small-button" id="linkRefresh">Refresh</button><button class="button ghost small-button" id="linkRestart">Restart</button><button class="button ghost small-button" id="linkSignOut">Sign out</button></div><div class="link-device-list">${deviceRows || `<div class="empty small">No devices yet.</div>`}${devicesError ? `<div class="small" style="color:var(--red)">${esc(devicesError)}</div>` : ""}</div>`;
      $("linkRefresh").onclick = loadLinkSettings;
      $("linkRestart").onclick = async () => {
        $("linkRestart").disabled = true;
        try {
          await api("/api/v1/link/start", { method: "POST", body: "{}" });
          toast("Link restarted", "This device is re-registered.", "ok");
        } catch (restartError) {
          toast("Could not restart Link", restartError.message, "bad");
        }
        await loadLinkSettings();
      };
      $("linkSignOut").onclick = async () => {
        try {
          await api("/api/v1/link/logout", { method: "POST", body: "{}" });
          toast("Link signed out", "Account credentials removed.", "ok");
          await loadLinkSettings();
        } catch (logoutError) {
          toast("Could not sign out", logoutError.message, "bad");
        }
      };
    } catch (error) {
      if ($("linkBody")) $("linkBody").innerHTML = `<div class="small" style="color:var(--red)">${esc(error.message)}</div>`;
      if ($("linkCurrent")) $("linkCurrent").textContent = "Unavailable";
    }
  }

  function renderPeerSettings() {
    const list = $("peerList");
    if (!list) return;
    const local = state.nodes.find((node) => node.local) || { id: "local", name: "this machine", online: true, local: true };
    const health = new Map(state.nodes.map((node) => [node.id, node]));
    const localCard = `<article class="peer-card"><div class="peer-main"><b>${esc(local.name)}</b><small>This machine · ${esc(local.id)}</small></div><span class="chip ok">local</span></article>`;
    const peerCards = state.peers.map((peer) => {
      const status = health.get(peer.id) || {};
      const wake = status.wake ? "Wake-on-LAN available" : "No wake-on-LAN";
      return `<article class="peer-card">
        <div class="peer-main"><b>${esc(peer.name)}</b><small class="mono">${esc(peer.id)} · Link</small><div class="wake-peer-row"><span class="small muted">${wake}</span></div></div>
        <span class="chip ${status.online ? "ok" : "bad"}">${status.online ? "online" : "offline"}</span>
      </article>`;
    }).join("");
    list.innerHTML = localCard + (peerCards || `<div class="empty small">No Link machines yet.</div>`);
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
    loadLinkSettings,
    renderPeerSettings,
    fieldControl,
    buildApi,
    closePublishDialog,
  };
})(typeof window !== "undefined" ? window : globalThis);
