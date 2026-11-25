use super::*;
use crate::config::{Config, ProxyConfig};

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
                let snapshot = metrics::snapshot_async()
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
        "proxy.configure" => {
            let mount = params
                .get("mount")
                .and_then(Value::as_str)
                .context("proxy.configure requires mount")?;
            permission(&format!("node.proxy:{mount}"))?;
            permission(&format!("node.proxy.manage:{mount}"))?;
            let base_url = params
                .get("base_url")
                .and_then(Value::as_str)
                .context("proxy.configure requires base_url")?;
            let mut next = (*state.config()).clone();
            let updated = configure_proxy_mount(
                &mut next,
                mount,
                base_url,
                params.get("host").and_then(Value::as_str),
                params.get("websocket").and_then(Value::as_bool),
                params.get("frame").and_then(Value::as_bool),
                params.get("title").and_then(Value::as_str),
            )?;
            state.update_config(|config| config.proxy = next.proxy)?;
            state.audit(
                "proxy.configure",
                json!({
                    "mount": updated.id,
                    "base_url": crate::config::redact_url_userinfo(&updated.base_url),
                    "host": updated.host,
                    "websocket": updated.websocket,
                    "frame": updated.frame,
                }),
            );
            let url = app_url_for_node(&state, mount, "local").unwrap_or_default();
            Ok(json!({
                "ok": true,
                "mount": {
                    "id": updated.id,
                    "title": updated.title,
                    "base_url": crate::config::redact_url_userinfo(&updated.base_url),
                    "host": updated.host,
                    "websocket": updated.websocket,
                    "frame": updated.frame,
                },
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
                            match conversation_open_payload(
                                &state,
                                viewer,
                                session,
                                &node,
                                Some(root),
                            ) {
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
                    let mut job_root = None;
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
                        job_root = Some(job.workspace.clone());
                        (Some(job_id), session)
                    };
                    let viewer = state
                        .config()
                        .agent
                        .resolved_viewer()
                        .context("no conversation viewer is configured")?;
                    let node = node_from_params(&hook_params);
                    let open = conversation_open_payload(
                        &state,
                        &viewer,
                        &session,
                        &node,
                        job_root.as_deref(),
                    )?;
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

/// Upsert a proxy mount for a module that has both the use and manage grants.
///
/// The mount id and host are not freely chosen: the host must be
/// `<mount>.<canonical-host>` so the app keeps its isolated origin. The
/// upstream must be a plain HTTP(S) URL without embedded credentials, a query,
/// or a fragment.
fn configure_proxy_mount(
    config: &mut Config,
    mount: &str,
    base_url: &str,
    host: Option<&str>,
    websocket: Option<bool>,
    frame: Option<bool>,
    title: Option<&str>,
) -> Result<ProxyConfig> {
    if !valid_host_label(mount) {
        bail!("proxy mount id must contain only letters, digits, '-' or '_'");
    }
    if mount != mount.to_ascii_lowercase() {
        bail!("proxy mount id must be lowercase");
    }
    let expected_host = format!("{mount}.{}", config.canonical_host());
    let host = host
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_ascii_lowercase())
        .unwrap_or_else(|| expected_host.clone());
    if host != expected_host {
        bail!("proxy host must be {expected_host}");
    }

    let parsed = url::Url::parse(base_url.trim()).context("proxy base_url must be a valid URL")?;
    if !matches!(parsed.scheme(), "http" | "https") {
        bail!("proxy base_url must use http or https");
    }
    if parsed.host_str().is_none() {
        bail!("proxy base_url must include a host");
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        bail!("proxy base_url may not embed credentials");
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        bail!("proxy base_url may not include a query or fragment");
    }
    let normalized = parsed.as_str().trim_end_matches('/').to_string();

    let title = title.unwrap_or("").trim();
    if title.chars().any(char::is_control) || title.len() > 96 {
        bail!("proxy title must be at most 96 printable characters");
    }

    let existing = config.proxy.iter().find(|entry| entry.id == mount).cloned();
    let default_websocket = existing
        .as_ref()
        .map(|entry| entry.websocket)
        .unwrap_or(true);
    let default_frame = existing.as_ref().map(|entry| entry.frame).unwrap_or(false);
    let default_title = existing
        .as_ref()
        .map(|entry| entry.title.clone())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| mount.to_string());
    let updated = ProxyConfig {
        id: mount.to_string(),
        title: if title.is_empty() {
            default_title
        } else {
            title.to_string()
        },
        base_url: normalized,
        host: Some(host),
        websocket: websocket.unwrap_or(default_websocket),
        frame: frame.unwrap_or(default_frame),
    };
    if let Some(entry) = config.proxy.iter_mut().find(|entry| entry.id == mount) {
        *entry = updated.clone();
    } else {
        config.proxy.push(updated.clone());
    }
    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_proxy_mount_upserts_and_preserves_origin() {
        let mut config = Config::default();
        let updated = configure_proxy_mount(
            &mut config,
            "homeassistant",
            "http://127.0.0.1:8123/",
            None,
            Some(true),
            Some(true),
            Some("Home Assistant"),
        )
        .expect("configure");
        assert_eq!(updated.id, "homeassistant");
        assert_eq!(updated.base_url, "http://127.0.0.1:8123");
        assert_eq!(
            updated.host.as_deref(),
            Some("homeassistant.orqella.localhost")
        );
        assert!(updated.websocket);
        assert!(updated.frame);
        assert_eq!(
            config
                .proxy
                .iter()
                .filter(|entry| entry.id == "homeassistant")
                .count(),
            1
        );

        let updated = configure_proxy_mount(
            &mut config,
            "homeassistant",
            "https://ha.example.com/subpath",
            None,
            None,
            None,
            None,
        )
        .expect("update");
        assert_eq!(updated.base_url, "https://ha.example.com/subpath");
        assert!(updated.websocket);
        assert!(updated.frame);
        assert_eq!(updated.title, "Home Assistant");
        assert_eq!(
            config
                .proxy
                .iter()
                .filter(|entry| entry.id == "homeassistant")
                .count(),
            1
        );
    }

    #[test]
    fn configure_proxy_mount_rejects_unsafe_values() {
        let mut config = Config::default();
        for (base_url, host) in [
            ("ftp://127.0.0.1:8123", None),
            ("http://user:pass@127.0.0.1:8123", None),
            ("http://127.0.0.1:8123/?query=1", None),
            ("http://127.0.0.1:8123/#fragment", None),
            ("http://127.0.0.1:8123", Some("evil.example.com")),
        ] {
            assert!(
                configure_proxy_mount(
                    &mut config,
                    "homeassistant",
                    base_url,
                    host,
                    None,
                    None,
                    None
                )
                .is_err(),
                "{base_url}"
            );
        }
    }
}
