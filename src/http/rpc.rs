use super::*;

#[derive(Deserialize)]
pub(super) struct RpcRequest {
    pub(super) method: String,
    #[serde(default)]
    pub(super) params: Value,
}

pub(super) async fn module_rpc(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<RpcRequest>,
) -> Response {
    let Some(module) = state.modules.get(&id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    if !module.state.enabled {
        return (StatusCode::FORBIDDEN, "module is disabled").into_response();
    }
    match rpc(state, module, body.method, body.params).await {
        Ok(value) => Json(json!({ "ok": true, "result": value })).into_response(),
        Err(error) => (
            StatusCode::FORBIDDEN,
            json!({ "ok": false, "error": format!("{error:#}") }).to_string(),
        )
            .into_response(),
    }
}

pub(super) async fn rpc(
    state: Arc<AppState>,
    module: crate::modules::store::InstalledModule,
    method: String,
    params: Value,
) -> Result<Value> {
    let permission = |permission: &str| -> Result<()> {
        if module
            .state
            .granted_permissions
            .iter()
            .any(|granted| granted == permission)
        {
            Ok(())
        } else if module
            .manifest
            .permissions
            .iter()
            .any(|declared| declared == permission)
        {
            bail!(
                "module `{}` has not been granted permission `{permission}`",
                module.manifest.id
            )
        } else {
            bail!(
                "module `{}` does not declare permission `{permission}`",
                module.manifest.id
            )
        }
    };
    match method.as_str() {
        "module.info" => Ok(json!({
            "id": module.manifest.id,
            "name": module.manifest.name,
            "version": module.manifest.version,
            "node_scope": module.manifest.node_scope,
        })),
        "system.info" => {
            permission("node.info")?;
            Ok(json!({
                "hostname": local_name(),
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "local": true,
            }))
        }
        "system.metrics" => {
            permission("node.metrics")?;
            let node = node_from_params(&params);
            if node == "local" {
                let snapshot = tokio::task::spawn_blocking(metrics::snapshot)
                    .await
                    .map_err(|error| anyhow::anyhow!("metrics unavailable: {error}"))?;
                Ok(serde_json::to_value(snapshot)?)
            } else {
                let peer = peer_by_id(&state, &node)?;
                let url = state.peer_url(&peer);
                Ok(
                    peers::fetch_json(&state.client, &peer, url.as_deref(), "/api/v1/peer/system")
                        .await?,
                )
            }
        }
        "services.list" | "proxy.list" => {
            permission("node.services")?;
            let config = state.config();
            Ok(json!(
                config
                    .proxy
                    .iter()
                    .map(|mount| json!({
                        "id": mount.id,
                        "title": if mount.title.is_empty() { &mount.id } else { &mount.title },
                        "base_url": crate::config::redact_url_userinfo(&mount.base_url),
                        "websocket": mount.websocket,
                    }))
                    .collect::<Vec<_>>()
            ))
        }
        "nodes.list" => {
            permission("nodes.read")?;
            let mut values = vec![json!({
                "id": "local",
                "name": local_name(),
                "online": true,
                "local": true,
                "latency_ms": 0,
            })];
            for peer in state.peer_health.read().expect("peer health lock").values() {
                values.push(json!({
                    "id": peer.id,
                    "name": peer.name,
                    "online": peer.online,
                    "local": false,
                    "latency_ms": peer.latency_ms,
                    "error": peer.error,
                }));
            }
            Ok(json!(values))
        }
        "storage.get" => {
            permission("storage")?;
            let storage = read_module_storage(&state.paths, &module.manifest.id)?;
            let key = params
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("default");
            Ok(storage.get(key).cloned().unwrap_or(Value::Null))
        }
        "storage.set" => {
            permission("storage")?;
            let key = params
                .get("key")
                .and_then(Value::as_str)
                .unwrap_or("default");
            let value = params.get("value").cloned().unwrap_or(Value::Null);
            let mut storage = read_module_storage(&state.paths, &module.manifest.id)?;
            storage.insert(key.to_string(), value.clone());
            write_module_storage(&state.paths, &module.manifest.id, &storage)?;
            Ok(value)
        }
        "backend.request" => {
            permission("backend")?;
            let method = params
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET");
            let path = params
                .get("path")
                .and_then(Value::as_str)
                .context("backend.request requires path")?;
            let body = params.get("body").cloned();
            state
                .sidecars
                .request(&module.manifest.id, method, path, body)
                .await
        }
        "proxy.status" => {
            let mount = params
                .get("mount")
                .and_then(Value::as_str)
                .context("proxy.status requires mount")?;
            permission(&format!("node.proxy:{mount}"))?;
            let config = state
                .config()
                .proxy(mount)
                .cloned()
                .context("proxy mount not found")?;
            let node = node_from_params(&params);
            let (ok, url) = if node == "local" {
                let ok = state
                    .client
                    .get(&config.base_url)
                    .timeout(std::time::Duration::from_millis(1200))
                    .send()
                    .await
                    .map(|response| response.status().is_success())
                    .unwrap_or(false);
                (
                    ok,
                    app_url_for_node(&state, mount, "local").unwrap_or_default(),
                )
            } else {
                let peer = peer_by_id(&state, &node)?;
                let peer_url = state.peer_url(&peer).context("peer has no reachable URL")?;
                let target = format!(
                    "{}/api/v1/peer/proxy/{}/",
                    peer_url.trim_end_matches('/'),
                    config.id
                );
                let ok = state
                    .client
                    .get(target)
                    .header("X-Orqella-Peer-Token", &peer.token)
                    .timeout(std::time::Duration::from_millis(1600))
                    .send()
                    .await
                    .map(|r| r.status().is_success())
                    .unwrap_or(false);
                (
                    ok,
                    app_url_for_node(&state, mount, &node).unwrap_or_default(),
                )
            };
            let host = url
                .strip_prefix("http://")
                .and_then(|rest| rest.split(':').next())
                .map(str::to_string);
            Ok(json!({
                "ok": ok,
                "id": config.id,
                "base_url": crate::config::redact_url_userinfo(&config.base_url),
                "host": host,
                "url": url,
            }))
        }
        "hooks.list" => Ok(crate::hooks::list()),
        "hooks.run" => {
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .context("hooks.run requires name")?;
            let hook_params = params.get("params").cloned().unwrap_or_else(|| json!({}));
            match name {
                crate::hooks::AGENT_PROMPT => {
                    permission("agent")?;
                    let prompt = hook_params
                        .get("prompt")
                        .and_then(Value::as_str)
                        .context("agent.prompt requires a prompt")?;
                    let root = hook_params
                        .get("root")
                        .and_then(Value::as_str)
                        .context("agent.prompt requires a root")?;
                    let wait_ms = hook_params
                        .get("wait_ms")
                        .and_then(Value::as_u64)
                        .unwrap_or(6000)
                        .min(15_000);
                    let open = hook_params
                        .get("open")
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    let node = node_from_params(&hook_params);
                    let job = state
                        .builder
                        .start_agent(prompt.to_string(), root.to_string())?;
                    let job = if wait_ms > 0 {
                        state
                            .builder
                            .wait_for_session(&job.id, std::time::Duration::from_millis(wait_ms))
                            .await
                            .unwrap_or(job)
                    } else {
                        job
                    };
                    let viewer = state.config().agent.resolved_viewer();
                    let session = job.session_id.clone();
                    let mut result = serde_json::to_value(&job)?;
                    result["hook"] = json!(crate::hooks::AGENT_PROMPT);
                    if let Some(viewer) = viewer.as_ref() {
                        result["viewer"] = serde_json::to_value(viewer)?;
                    }
                    if open {
                        if let (Some(viewer), Some(session)) = (viewer.as_ref(), session.as_ref()) {
                            match conversation_open_payload(&state, viewer, session, &node) {
                                Ok(payload) => result["open"] = payload,
                                Err(error) => {
                                    result["open_error"] = json!(format!("{error:#}"));
                                }
                            }
                        } else {
                            result["open_error"] =
                                json!("the agent did not expose a conversation id");
                        }
                    }
                    state.audit(
                        "hooks.run",
                        json!({
                            "hook": crate::hooks::AGENT_PROMPT,
                            "module": module.manifest.id,
                            "job": job.id,
                            "session": job.session_id,
                        }),
                    );
                    Ok(result)
                }
                crate::hooks::CONVERSATION_OPEN => {
                    permission("agent")?;
                    let requested_job = hook_params
                        .get("job")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let requested_session = hook_params
                        .get("session")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    let (job_id, session) = if let Some(session) = requested_session {
                        (requested_job, session)
                    } else {
                        let job_id =
                            requested_job.context("conversation.open requires a job or session")?;
                        let job = state.builder.get(&job_id).context("agent job not found")?;
                        let session = job
                            .session_id
                            .clone()
                            .context("agent job has no conversation id yet")?;
                        (Some(job_id), session)
                    };
                    let viewer = state
                        .config()
                        .agent
                        .resolved_viewer()
                        .context("no conversation viewer is configured")?;
                    let node = node_from_params(&hook_params);
                    let open = conversation_open_payload(&state, &viewer, &session, &node)?;
                    state.audit(
                        "hooks.run",
                        json!({
                            "hook": crate::hooks::CONVERSATION_OPEN,
                            "module": module.manifest.id,
                            "job": job_id,
                            "session": session,
                        }),
                    );
                    Ok(json!({
                        "hook": crate::hooks::CONVERSATION_OPEN,
                        "job": job_id,
                        "session_id": session,
                        "viewer": viewer,
                        "open": open,
                    }))
                }
                other => bail!("unknown hook `{other}`"),
            }
        }
        "agent.start" => {
            permission("agent")?;
            let prompt = params
                .get("prompt")
                .and_then(Value::as_str)
                .context("agent.start requires a prompt")?;
            let root = params
                .get("root")
                .and_then(Value::as_str)
                .context("agent.start requires a root")?;
            let job = state
                .builder
                .start_agent(prompt.to_string(), root.to_string())?;
            state.audit(
                "agent.start",
                json!({ "job": job.id, "module": module.manifest.id, "root": job.workspace }),
            );
            Ok(serde_json::to_value(job)?)
        }
        "agent.status" => {
            permission("agent")?;
            let id = params
                .get("id")
                .and_then(Value::as_str)
                .context("agent.status requires an id")?;
            state
                .builder
                .get(id)
                .map(serde_json::to_value)
                .transpose()?
                .context("agent job not found")
        }
        "app.open" => {
            permission("app.open")?;
            let mount = params
                .get("mount")
                .and_then(Value::as_str)
                .context("app.open requires mount")?;
            let node = node_from_params(&params);
            app_open_payload(
                &state,
                mount,
                &node,
                params.get("path").and_then(Value::as_str),
                params.get("handoff").and_then(Value::as_str),
                None,
            )
        }
        other => bail!("unknown module API method `{other}`"),
    }
}
