use super::*;

pub(super) async fn fetch_peer_update_check(
    state: &AppState,
    peer: &crate::config::PeerConfig,
) -> Value {
    let Some(url) = state.peer_url(peer) else {
        return json!({ "id": peer.id, "name": peer.name, "ok": false, "error": "peer has no reachable URL" });
    };
    let target = format!("{}/api/v1/peer/update/check", url.trim_end_matches('/'));
    match state
        .client
        .get(target)
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(std::time::Duration::from_secs(25))
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let value = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| {
                if status == StatusCode::NOT_FOUND {
                    json!({ "error": "peer is running an older Orqella build without update support" })
                } else {
                    json!({ "error": body })
                }
            });
            json!({ "id": peer.id, "name": peer.name, "ok": status.is_success(), "result": value })
        }
        Err(error) => {
            json!({ "id": peer.id, "name": peer.name, "ok": false, "error": error.to_string() })
        }
    }
}

pub(super) async fn fetch_peer_update_apply(
    state: &AppState,
    peer: &crate::config::PeerConfig,
) -> Value {
    let Some(url) = state.peer_url(peer) else {
        return json!({ "id": peer.id, "name": peer.name, "ok": false, "error": "peer has no reachable URL" });
    };
    let target = format!("{}/api/v1/peer/update/apply", url.trim_end_matches('/'));
    match state
        .client
        .post(target)
        .header("X-Orqella-Peer-Token", &peer.token)
        .header("Content-Type", "application/json")
        .body("{}")
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let value = serde_json::from_str::<Value>(&body).unwrap_or_else(|_| {
                if status == StatusCode::NOT_FOUND {
                    json!({ "error": "peer is running an older Orqella build without update support" })
                } else {
                    json!({ "error": body })
                }
            });
            json!({ "id": peer.id, "name": peer.name, "ok": status.is_success(), "result": value })
        }
        Err(error) => {
            json!({ "id": peer.id, "name": peer.name, "ok": false, "error": error.to_string() })
        }
    }
}

pub(super) async fn fetch_peer_update_status(
    state: &AppState,
    peer: &crate::config::PeerConfig,
) -> Value {
    let Some(url) = state.peer_url(peer) else {
        return json!({
            "id": peer.id,
            "name": peer.name,
            "ok": false,
            "error": "peer has no reachable URL"
        });
    };
    let target = format!("{}/api/v1/peer/update/status", url.trim_end_matches('/'));
    match state
        .client
        .get(target)
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let value =
                serde_json::from_str::<Value>(&body).unwrap_or_else(|_| json!({ "error": body }));
            json!({
                "id": peer.id,
                "name": peer.name,
                "ok": status.is_success(),
                "status": value
            })
        }
        Err(error) => {
            json!({
                "id": peer.id,
                "name": peer.name,
                "ok": false,
                "error": error.to_string()
            })
        }
    }
}

async fn local_update_check(state: &Arc<AppState>) -> Value {
    let config = state.config();
    let paths = state.paths.clone();
    match tokio::task::spawn_blocking(move || update::check(&config, &paths)).await {
        Ok(Ok(value)) => value,
        Ok(Err(error)) => json!({ "ok": false, "error": format!("{error:#}") }),
        Err(error) => {
            json!({ "ok": false, "error": format!("update check worker failed: {error}") })
        }
    }
}

pub(super) async fn update_check_value(state: &Arc<AppState>) -> Value {
    let local = local_update_check(state).await;
    let peers = state.config().peers.clone();
    let checks = futures_util::future::join_all(
        peers
            .iter()
            .map(|peer| fetch_peer_update_check(state, peer)),
    )
    .await;
    json!({ "local": local, "peers": peers.iter().zip(checks).map(|(peer, check)| {
        let mut value = check;
        value["id"] = json!(peer.id);
        value["name"] = json!(peer.name);
        value
    }).collect::<Vec<_>>() })
}

/// Return why a machine should not be updated, or `None` when a newer
/// GitHub release is available for it.
pub(super) fn update_target_block(value: &Value) -> Option<(&'static str, String)> {
    if !value.is_object() {
        return Some(("blocked", "check failed".to_string()));
    }
    let source = value.get("result").unwrap_or(value);
    if value.get("ok").and_then(Value::as_bool) == Some(false) {
        return Some((
            "blocked",
            value
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("check failed")
                .to_string(),
        ));
    }
    if source.get("ok").and_then(Value::as_bool) == Some(false) {
        return Some((
            "blocked",
            source
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("check failed")
                .to_string(),
        ));
    }
    if source.get("available").and_then(Value::as_bool) != Some(true) {
        return Some(("skipped", "up to date".to_string()));
    }
    None
}

pub(super) async fn update_check(State(state): State<Arc<AppState>>) -> Response {
    Json(update_check_value(&state).await).into_response()
}

pub(super) async fn update_check_node(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
) -> Response {
    if node == "local" {
        return Json(local_update_check(&state).await).into_response();
    }
    let Some(peer) = state
        .config()
        .peers
        .iter()
        .find(|peer| peer.id == node)
        .cloned()
    else {
        return (StatusCode::NOT_FOUND, format!("unknown machine: {node}")).into_response();
    };
    Json(fetch_peer_update_check(&state, &peer).await).into_response()
}

pub(super) async fn update_apply(State(state): State<Arc<AppState>>) -> Response {
    let _update_guard = state.update_lock.lock().await;
    if update::in_progress(&state.paths) {
        return (
            StatusCode::CONFLICT,
            "an update is already queued or running",
        )
            .into_response();
    }

    let check = update_check_value(&state).await;
    let local_check = check.get("local").cloned().unwrap_or(Value::Null);
    let local = match update_target_block(&local_check) {
        None => match update::spawn(&state.config(), &state.paths, std::process::id()) {
            Ok(()) => json!({ "action": "accepted", "check": local_check }),
            Err(error) => {
                update::mark_failed(&state.paths, &format!("{error:#}"));
                json!({
                    "action": "failed",
                    "reason": format!("{error:#}"),
                    "check": local_check
                })
            }
        },
        Some((action, reason)) => {
            json!({ "action": action, "reason": reason, "check": local_check })
        }
    };

    let peers = state.config().peers.clone();
    let peer_checks = check
        .get("peers")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut peer_results = Vec::new();
    for peer in &peers {
        let peer_check = peer_checks
            .iter()
            .find(|entry| entry.get("id").and_then(Value::as_str) == Some(peer.id.as_str()))
            .cloned()
            .unwrap_or(Value::Null);
        match update_target_block(&peer_check) {
            None => {
                let result = fetch_peer_update_apply(&state, peer).await;
                let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
                let action = if ok { "accepted" } else { "failed" };
                let reason = if ok {
                    Value::Null
                } else {
                    result
                        .get("error")
                        .cloned()
                        .unwrap_or_else(|| json!("peer did not accept the update"))
                };
                peer_results.push(json!({
                    "id": peer.id,
                    "name": peer.name,
                    "action": action,
                    "reason": reason,
                    "result": result
                }));
            }
            Some((action, reason)) => {
                peer_results.push(json!({
                    "id": peer.id,
                    "name": peer.name,
                    "action": action,
                    "reason": reason
                }));
            }
        }
    }

    Json(json!({ "local": local, "peers": peer_results })).into_response()
}

pub(super) async fn update_apply_node(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
) -> Response {
    let _update_guard = state.update_lock.lock().await;
    if update::in_progress(&state.paths) {
        return (
            StatusCode::CONFLICT,
            "an update is already queued or running",
        )
            .into_response();
    }

    if node == "local" {
        let check = local_update_check(&state).await;
        if let Some((action, reason)) = update_target_block(&check) {
            return Json(json!({
                "node": node,
                "action": action,
                "reason": reason
            }))
            .into_response();
        }
        return match update::spawn(&state.config(), &state.paths, std::process::id()) {
            Ok(()) => Json(json!({ "node": node, "action": "accepted" })).into_response(),
            Err(error) => {
                update::mark_failed(&state.paths, &format!("{error:#}"));
                Json(json!({
                    "node": node,
                    "action": "failed",
                    "reason": format!("{error:#}")
                }))
                .into_response()
            }
        };
    }

    let Some(peer) = state
        .config()
        .peers
        .iter()
        .find(|peer| peer.id == node)
        .cloned()
    else {
        return (StatusCode::NOT_FOUND, format!("unknown machine: {node}")).into_response();
    };
    let check = fetch_peer_update_check(&state, &peer).await;
    if let Some((action, reason)) = update_target_block(&check) {
        return Json(json!({
            "node": node,
            "name": peer.name,
            "action": action,
            "reason": reason
        }))
        .into_response();
    }
    let result = fetch_peer_update_apply(&state, &peer).await;
    let ok = result.get("ok").and_then(Value::as_bool).unwrap_or(false);
    let reason = if ok {
        Value::Null
    } else {
        result
            .get("error")
            .cloned()
            .unwrap_or_else(|| json!("peer did not accept the update"))
    };
    Json(json!({
        "node": node,
        "name": peer.name,
        "action": if ok { "accepted" } else { "failed" },
        "reason": reason
    }))
    .into_response()
}

pub(super) async fn update_status(State(state): State<Arc<AppState>>) -> Response {
    Json(update::status(&state.paths)).into_response()
}

pub(super) async fn update_peer_status(State(state): State<Arc<AppState>>) -> Response {
    let peers = state.config().peers.clone();
    let statuses = futures_util::future::join_all(
        peers
            .iter()
            .map(|peer| fetch_peer_update_status(&state, peer)),
    )
    .await;
    Json(statuses).into_response()
}

pub(super) async fn peer_update_check(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    let config = state.config();
    let paths = state.paths.clone();
    match tokio::task::spawn_blocking(move || update::check(&config, &paths)).await {
        Ok(Ok(value)) => Json(value).into_response(),
        Ok(Err(error)) => {
            Json(json!({ "ok": false, "error": format!("{error:#}") })).into_response()
        }
        Err(error) => Json(json!({
            "ok": false,
            "error": format!("update check worker failed: {error}")
        }))
        .into_response(),
    }
}

pub(super) async fn peer_update_apply(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    let _update_guard = state.update_lock.lock().await;
    if update::in_progress(&state.paths) {
        return (
            StatusCode::CONFLICT,
            "an update is already queued or running",
        )
            .into_response();
    }
    match update::spawn(&state.config(), &state.paths, std::process::id()) {
        Ok(()) => {
            state.audit("peer.update", json!({ "remote": true }));
            Json(json!({ "ok": true, "status": "accepted" })).into_response()
        }
        Err(error) => {
            update::mark_failed(&state.paths, &format!("{error:#}"));
            Json(json!({ "ok": false, "error": format!("{error:#}") })).into_response()
        }
    }
}

pub(super) async fn peer_update_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    Json(update::status(&state.paths)).into_response()
}
