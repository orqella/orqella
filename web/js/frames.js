(function (global) {
  "use strict";

  let state;
  let $;
  let api;
  let render;
  let currentNode;

  function install(ctx) {
    ({ state, $, api, render, currentNode } = ctx);
  }

  function frameKey(surface, id) {
    return `${surface}:${id}`;
  }

  function newFrameNonce() {
    const bytes = new Uint8Array(16);
    if (window.crypto?.getRandomValues) {
      window.crypto.getRandomValues(bytes);
    } else {
      for (let index = 0; index < bytes.length; index += 1) {
        bytes[index] = Math.floor(Math.random() * 256);
      }
    }
    return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("");
  }

  function frameHash(nonce, wsToken = "") {
    const frame = `#__orqella_frame=${encodeURIComponent(nonce)}`;
    return wsToken
      ? `${frame}&__orqella_ws=${encodeURIComponent(wsToken)}`
      : frame;
  }

  function setFrameNonce(frame, surface, id) {
    const nonce = newFrameNonce();
    frame.dataset.frameNonce = nonce;
    state.readyFrames.delete(frameKey(surface, id));
    frame.addEventListener("load", () => {
      state.readyFrames.delete(frameKey(surface, id));
    });
    return nonce;
  }

  function frameNonceMatches(frame, surface, id, nonce) {
    return Boolean(frame?.dataset.frameNonce) && frame.dataset.frameNonce === nonce;
  }

  function moduleContext(id, surface) {
    const module = state.modules.find((entry) => entry.id === id) || null;
    const remote = !!module?.remote_node;
    const selectedTheme = state.info?.themes?.find((theme) => theme.id === state.info?.config?.theme)
      || state.info?.themes?.[0]
      || { id: "nocturne", color_scheme: "dark" };
    return {
      module,
      node: remote ? { id: "local", name: currentNode().name, remote: true } : currentNode(),
      nodes: state.nodes,
      config: module?.config || {},
      ws_token: module?.ws_token || "",
      surface,
      theme: selectedTheme.color_scheme || "dark",
      theme_id: selectedTheme.id,
      origin: location.origin,
    };
  }

  function sendContextTo(frame, id, surface) {
    if (!frame?.contentWindow || !state.readyFrames.has(frameKey(surface, id))) return;
    frame.contentWindow.postMessage({ type: "orqella:init", context: moduleContext(id, surface) }, "*");
  }

  function sendContext() {
    const frame = $("moduleFrame");
    if (!frame.contentWindow || !state.module) return;
    sendContextTo(frame, state.module, "page");
  }

  function sendOverviewContext(id) {
    const frame = state.overviewFrames.get(id);
    if (!frame) return;
    sendContextTo(frame, id, "overview");
  }

  function sendSettingsContext(id) {
    const frame = state.settingsFrames.get(id);
    if (!frame) return;
    sendContextTo(frame, id, "settings");
  }

  function sendThemeContext() {
    const send = (frame, id, surface) => {
      if (!frame?.contentWindow || !state.readyFrames.has(frameKey(surface, id))) return;
      frame.contentWindow.postMessage({
        type: "orqella:context",
        context: moduleContext(id, surface),
      }, "*");
    };
    if (state.module) send($("moduleFrame"), state.module, "page");
    for (const [id, frame] of state.overviewFrames) send(frame, id, "overview");
    for (const [id, frame] of state.settingsFrames) send(frame, id, "settings");
  }

  async function rpc(module, method, params) {
    const installed = state.modules.find((entry) => entry.id === module);
    const remote = installed?.remote_node;
    const endpoint = remote
      ? `/api/v1/nodes/${encodeURIComponent(remote)}/modules/${encodeURIComponent(module)}/rpc`
      : `/api/v1/modules/${encodeURIComponent(module)}/rpc`;
    return api(endpoint, {
      method: "POST",
      body: JSON.stringify({ method, params }),
    });
  }

  function waitForAppFrame() {
    return new Promise((resolve) => {
      const frame = $("appFrame");
      let done = false;
      const finish = () => { if (done) return; done = true; frame.removeEventListener("load", finish); setTimeout(resolve, 120); };
      frame.addEventListener("load", finish);
      setTimeout(finish, 7000);
    });
  }

  function appPayloadForSelectedNode(payload) {
    if (!payload || !payload.mount) return payload;
    let result = payload;
    if (state.node !== "local" && payload.node === "local") {
      const origin = new URL(state.info?.node?.origin || location.origin);
      const host = `${payload.mount}.${state.node}.${origin.hostname}`;
      const authority = `${host}${origin.port ? `:${origin.port}` : ""}`;
      result = { ...payload, node: state.node, host, url: `${origin.protocol}//${authority}/` };
    }
    // Some app routes encode their origin in the path (OpenCode uses
    // /server/<base64url(origin)>/session/<id>). Keep the key aligned with the
    // app URL the shell is about to load, including peer-host rewrites.
    const appUrl = typeof result.url === "string" && /^https?:\/\//.test(result.url) ? result.url : "";
    if (appUrl) {
      let path = result.path || "";
      const encoded = path.match(/^\/server\/[^/]+\/session\/(.+)$/);
      if (encoded) {
        const key = btoa(appUrl.replace(/\/$/, "")).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
        result = { ...result, path: `/server/${key}/session/${encoded[1]}` };
      }
    }
    return result;
  }

  async function openApp(payload) {
    // Native clients can place full apps on a transport-owned origin. The
    // resolver is optional, so browsers keep the canonical host behavior.
    let resolvedBase = "";
    if (typeof window.__ORQELLA_APP_ORIGIN_RESOLVER === "function") {
      try {
        const value = await window.__ORQELLA_APP_ORIGIN_RESOLVER(payload);
        if (value) resolvedBase = new URL("/", value).toString();
      } catch (error) {
        console.warn("Orqella app origin resolver failed", error);
      }
    }
    const baseUrl = resolvedBase || (payload.host
      ? `${location.protocol}//${payload.host}:${location.port}/`
      : new URL(payload.url, location.origin).toString());
    let appUrl = baseUrl;
    if (typeof payload.path === "string" && payload.path.startsWith("/") && !payload.path.startsWith("//")) {
      try { appUrl = new URL(payload.path, baseUrl).toString(); } catch {}
    }
    payload = { ...payload, url: appUrl };
    state.app = payload;
    const appLabel = payload.title || payload.mount || "Application";
    const frame = $("appFrame");
    frame.dataset.appTitle = appLabel;
    $("appLoadingText").textContent = `Opening ${appLabel}…`;
    $("appLoading").hidden = false;
    frame.classList.add("app-frame-loading");
    $("appSurface").hidden = false;
    $("coreView").hidden = true;
    $("moduleHost").hidden = true;
    const loadFrame = async (url) => {
      const loaded = waitForAppFrame();
      frame.src = url;
      await loaded;
    };
    const loadHandoff = (url) => new Promise((resolve) => {
      const hidden = document.createElement("iframe");
      let done = false;
      const finish = () => {
        if (done) return;
        done = true;
        setTimeout(() => { hidden.remove(); resolve(); }, 120);
      };
      hidden.tabIndex = -1;
      hidden.setAttribute("aria-hidden", "true");
      hidden.style.cssText = "position:absolute;width:1px;height:1px;opacity:0;pointer-events:none;border:0";
      hidden.addEventListener("load", finish, { once: true });
      hidden.src = url;
      $("appSurface").append(hidden);
      setTimeout(finish, 5000);
    });
    try {
      if (typeof payload.handoff === "string" && payload.handoff.startsWith("/") && !payload.handoff.startsWith("//")) {
        try { await loadHandoff(new URL(payload.handoff, baseUrl).toString()); } catch {}
      }
      await loadFrame(appUrl);
    } finally {
      $("appLoading").hidden = true;
      frame.classList.remove("app-frame-loading");
    }
  }

  function recorderMime() {
    if (!window.MediaRecorder) return "";
    const types = ["audio/webm;codecs=opus", "audio/webm", "audio/mp4", "audio/ogg;codecs=opus", "audio/ogg"];
    return types.find((type) => MediaRecorder.isTypeSupported(type)) || "";
  }

  function blobBase64(blob) {
    return new Promise((resolve, reject) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result || "").split(",", 2)[1] || "");
      reader.onerror = () => reject(reader.error || new Error("could not read audio"));
      reader.readAsDataURL(blob);
    });
  }

  async function startModuleRecording(moduleId) {
    const module = state.modules.find((entry) => entry.id === moduleId);
    if (!module) throw new Error("module not found");
    if (!module.granted_permissions?.includes("media.record")) {
      throw new Error("microphone permission has not been granted");
    }
    if (state.recordings.has(moduleId)) throw new Error("microphone is already recording");
    if (!navigator.mediaDevices?.getUserMedia || !window.MediaRecorder) {
      throw new Error("microphone recording is not available in this browser");
    }
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
    });
    const type = recorderMime();
    const recorder = type ? new MediaRecorder(stream, { mimeType: type }) : new MediaRecorder(stream);
    const entry = { stream, recorder, chunks: [], mime: recorder.mimeType || type || "audio/webm" };
    recorder.ondataavailable = (event) => { if (event.data?.size) entry.chunks.push(event.data); };
    recorder.start();
    state.recordings.set(moduleId, entry);
    return { ok: true };
  }

  function stopAllRecordings() {
    for (const entry of state.recordings.values()) {
      try { entry.recorder.stop(); } catch {}
      entry.stream.getTracks().forEach((track) => track.stop());
    }
    state.recordings.clear();
  }

  function stopModuleRecording(moduleId) {
    const entry = state.recordings.get(moduleId);
    if (!entry) throw new Error("module is not recording");
    return new Promise((resolve, reject) => {
      const done = async () => {
        try {
          const blob = new Blob(entry.chunks, { type: entry.mime });
          entry.stream.getTracks().forEach((track) => track.stop());
          state.recordings.delete(moduleId);
          resolve({ audio: await blobBase64(blob), mime: entry.mime, size: blob.size });
        } catch (error) {
          reject(error);
        }
      };
      entry.recorder.addEventListener("stop", done, { once: true });
      try { entry.recorder.stop(); } catch (error) {
        entry.stream.getTracks().forEach((track) => track.stop());
        state.recordings.delete(moduleId);
        reject(error);
      }
    });
  }

  function frameTarget(source) {
    if (source === $("moduleFrame").contentWindow && state.module) {
      return {
        id: state.module,
        surface: "page",
        nonce: $("moduleFrame").dataset.frameNonce || "",
        frame: $("moduleFrame"),
      };
    }
    for (const [id, frame] of state.overviewFrames) {
      if (frame.contentWindow === source) {
        return { id, surface: "overview", nonce: frame.dataset.frameNonce || "", frame };
      }
    }
    for (const [id, frame] of state.settingsFrames) {
      if (frame.contentWindow === source) {
        return { id, surface: "settings", nonce: frame.dataset.frameNonce || "", frame };
      }
    }
    return null;
  }

  global.OrqellaFrames = {
    install,
    frameKey,
    newFrameNonce,
    frameHash,
    setFrameNonce,
    frameNonceMatches,
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
  };
})(typeof window !== "undefined" ? window : globalThis);
