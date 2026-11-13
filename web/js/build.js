(function (global) {
  "use strict";

  let state;
  let $;
  let esc;
  let api;
  let toast;
  let refresh;
  let currentNode;
  let buildApi;
  let moduleIconUrl;

  function install(ctx) {
    ({ state, $, esc, api, toast, refresh, currentNode, buildApi, moduleIconUrl } = ctx);
  }

  function renderBuild() {
    const root = $("coreView");
    const nodeName = currentNode().name;
    const modules = [...state.modules].sort((a, b) => a.name.localeCompare(b.name));
    const editModule = state.buildEditModule
      ? modules.find((module) => module.id === state.buildEditModule)
      : null;
    const moduleRows = modules.map((module) => {
      const icon = moduleIconUrl(module)
        ? `<img src="${esc(moduleIconUrl(module))}" alt="">`
        : "◇";
      return `<div class="build-module-row">
        <span class="overview-glyph">${icon}</span>
        <div class="build-module-copy"><b>${esc(module.name)}</b><small>${esc(module.version)} · ${esc(module.id)}${module.enabled ? "" : " · disabled"}</small></div>
        <button type="button" class="button ghost small-button" data-edit-module="${esc(module.id)}">Edit</button>
      </div>`;
    }).join("");
    const editPanel = editModule
      ? `<div class="modal" id="moduleEditModal" role="presentation">
        <div class="modal-box" role="dialog" aria-modal="true" aria-labelledby="moduleEditTitle">
          <div class="row spread"><div><p class="eyebrow">Module edit</p><h2 id="moduleEditTitle">${esc(editModule.name)}</h2></div><button class="icon-button" id="closeModuleEdit" aria-label="Close">×</button></div>
          <p class="small muted" style="margin-top:8px">Describe the change. The agent works on a copy, then Orqella validates and installs it. Module data and permissions are preserved.</p>
          <textarea class="field" id="moduleEditPrompt" rows="5" placeholder="Describe what to change or add…" style="margin-top:14px"></textarea>
          <div class="row wrap" style="margin-top:12px"><button class="button ghost" id="cancelModuleEdit">Cancel</button><button class="button" id="startModuleEdit">Ask agent</button><span class="small muted" id="moduleEditStatus"></span></div>
        </div>
      </div>`
      : "";
    root.innerHTML = `<div class="stack">
      <article class="card flat"><div class="row spread"><h2>Build a new page</h2><span class="small muted">${esc(nodeName)}</span></div><textarea class="field" id="buildPrompt" placeholder="Describe the page you want to build." style="margin-top:12px"></textarea><div class="row" style="margin-top:10px"><button class="button" id="startBuild">Start build</button></div></article>
      <article class="card flat"><div class="row spread"><h2>Existing modules</h2><span class="small muted">${modules.length} on ${esc(nodeName)}</span></div><div class="build-module-list">${moduleRows || `<div class="empty small">No modules installed on ${esc(nodeName)}.</div>`}</div></article>
      <div class="page-tools"><button class="button ghost small-button" id="refreshJobs">Refresh</button></div><div class="stack-sm" id="jobList"></div>${editPanel}
    </div>`;
    $("startBuild").onclick = async () => {
      const prompt = $("buildPrompt").value.trim();
      if (!prompt) return;
      $("startBuild").disabled = true;
      try {
        const node = state.node;
        const result = await api(buildApi(node), { method: "POST", body: JSON.stringify({ prompt }) });
        $("buildPrompt").value = "";
        if (state.node === node) await pollJobs();
        if (result?.open && await openConversationPayload(result.open)) {
          toast("Build started", "Opened the agent session.", "ok");
        } else {
          toast("Build started", "You can leave this page; the job keeps running.", "ok");
        }
      } catch (error) { toast("Could not start build", error.message, "bad"); }
      finally { $("startBuild").disabled = false; }
    };
    root.querySelectorAll("[data-edit-module]").forEach((button) => {
      button.onclick = () => {
        state.buildEditModule = button.dataset.editModule;
        renderBuild();
      };
    });
    if (editModule) {
      const close = () => {
        state.buildEditModule = null;
        renderBuild();
      };
      $("closeModuleEdit").onclick = close;
      $("cancelModuleEdit").onclick = close;
      $("moduleEditModal").addEventListener("click", (event) => { if (event.target === $("moduleEditModal")) close(); });
      $("startModuleEdit").onclick = async () => {
        const prompt = $("moduleEditPrompt").value.trim();
        if (!prompt) return;
        $("startModuleEdit").disabled = true;
        $("moduleEditStatus").textContent = "Starting…";
        try {
          const node = state.node;
          const result = await api(buildApi(node), { method: "POST", body: JSON.stringify({ module_id: editModule.id, prompt }) });
          state.buildEditModule = null;
          renderBuild();
          if (result?.open && await openConversationPayload(result.open)) {
            toast("Module edit started", "Opened the agent session.", "ok");
          } else {
            toast("Module edit started", editModule.name, "ok");
          }
        } catch (error) {
          $("startModuleEdit").disabled = false;
          $("moduleEditStatus").textContent = error.message;
          toast("Could not start module edit", error.message, "bad");
        }
      };
      setTimeout(() => $("moduleEditPrompt")?.focus(), 30);
    }
    $("refreshJobs").onclick = pollJobs;
    pollJobs();
  }

  async function pollJobs() {
    const node = state.node;
    try {
      const jobs = await api(buildApi(node));
      if (state.node !== node || state.view !== "build") return;
      state.jobs = jobs;
      const list = $("jobList");
      if (!list) return;
      list.innerHTML = state.jobs.length ? state.jobs.map((job) => {
        const name = state.modules.find((module) => module.id === job.module_id)?.name || job.module_id || "module";
        const label = job.kind === "edit" ? `Edit ${name}` : name;
        return `<article class="job"><div class="job-head"><div class="row"><span class="chip ${job.status === "succeeded" ? "ok" : job.status === "failed" ? "bad" : "warn"}">${esc(job.status)}</span><b>${esc(label)}</b>${job.session_id ? `<button type="button" class="button ghost small-button" data-open-job="${esc(job.id)}">Open session</button>` : ""}</div><span class="small muted mono">${esc(job.id.slice(0, 8))}</span></div><p class="prompt">${esc(job.prompt)}</p>${job.error ? `<p class="small" style="color:var(--red)">${esc(job.error)}</p>` : ""}</article>`;
      }).join("") : `<div class="empty">No builds yet.</div>`;
      list.querySelectorAll("[data-open-job]").forEach((button) => {
        button.onclick = async () => {
          button.disabled = true;
          try {
            const result = await api(buildApi(node, `/${encodeURIComponent(button.dataset.openJob)}/open`));
            if (!(await openConversationPayload(result.open))) throw new Error(result.open_error || "No conversation viewer is configured.");
          } catch (error) {
            toast("Could not open session", error.message, "bad");
          } finally {
            button.disabled = false;
          }
        };
      });
      if (state.buildPollTimer) clearTimeout(state.buildPollTimer);
      state.buildPollTimer = state.view === "build" && state.jobs.some((job) => job.status === "queued" || job.status === "running")
        ? setTimeout(() => { if (state.view === "build" && $("jobList")) pollJobs(); }, 3000)
        : null;
    } catch (error) {
      if (state.node !== node || state.view !== "build") return;
      const list = $("jobList");
      if (list) list.innerHTML = `<div class="empty small">${esc(error.message || "Could not load builds.")}</div>`;
    }
  }

  global.OrqellaBuild = { install, renderBuild, pollJobs };
})(typeof window !== "undefined" ? window : globalThis);
