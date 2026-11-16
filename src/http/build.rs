use super::*;

#[derive(Deserialize, Serialize, Default)]
pub(super) struct BuildRequest {
    prompt: String,
    #[serde(default)]
    module_id: Option<String>,
}

pub(super) async fn start_build(
    State(state): State<Arc<AppState>>,
    Json(body): Json<BuildRequest>,
) -> Response {
    let result = match body
        .module_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    {
        Some(module_id) => state.builder.clone().start_edit(
            Arc::clone(&state.modules),
            module_id.to_string(),
            body.prompt,
        ),
        None => state
            .builder
            .clone()
            .start(Arc::clone(&state.modules), body.prompt),
    };
    match result {
        Ok(job) => {
            let action = if job.kind == "edit" {
                "build.edit"
            } else {
                "build.start"
            };
            // Wait briefly for the harness to expose its conversation id, so
            // the shell can open the session where the work is happening.
            let job = if job.session_id.is_none()
                && matches!(job.status.as_str(), "queued" | "running")
            {
                state
                    .builder
                    .wait_for_session(&job.id, std::time::Duration::from_secs(6))
                    .await
                    .unwrap_or(job)
            } else {
                job
            };
            state.audit(
                action,
                json!({
                    "job": job.id,
                    "prompt": job.prompt,
                    "module": job.module_id,
                    "session": job.session_id,
                }),
            );
            let session = job.session_id.clone();
            let viewer = state.config().agent.resolved_viewer();
            let mut response = json!({ "ok": true, "job": job });
            if let Some(viewer) = viewer {
                if let Ok(value) = serde_json::to_value(&viewer) {
                    response["viewer"] = value;
                }
                if let Some(session) = session.as_deref() {
                    match conversation_open_payload(
                        &state,
                        &viewer,
                        session,
                        "local",
                        Some(job.workspace.as_str()),
                    ) {
                        Ok(open) => response["open"] = open,
                        Err(error) => response["open_error"] = json!(format!("{error:#}")),
                    }
                }
            }
            Json(response).into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}

pub(super) async fn list_builds(State(state): State<Arc<AppState>>) -> Response {
    Json(
        state
            .builder
            .list()
            .into_iter()
            .filter(|job| job.kind == "module" || job.kind == "edit")
            .collect::<Vec<_>>(),
    )
    .into_response()
}

pub(super) async fn get_build(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.builder.get(&id) {
        Some(job) => Json(job).into_response(),
        None => (StatusCode::NOT_FOUND, "build job not found").into_response(),
    }
}

pub(super) async fn open_build_session(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(job) = state.builder.get(&id) else {
        return (StatusCode::NOT_FOUND, "build job not found").into_response();
    };
    let Some(session) = job.session_id.clone() else {
        return (
            StatusCode::CONFLICT,
            "this agent job has not exposed a conversation id yet",
        )
            .into_response();
    };
    let Some(viewer) = state.config().agent.resolved_viewer() else {
        return (StatusCode::CONFLICT, "no conversation viewer is configured").into_response();
    };
    match conversation_open_payload(
        &state,
        &viewer,
        &session,
        "local",
        Some(job.workspace.as_str()),
    ) {
        Ok(open) => Json(json!({
            "ok": true,
            "job": job.id,
            "session_id": session,
            "viewer": viewer,
            "open": open,
        }))
        .into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}

pub(super) fn build_peer(
    state: &AppState,
    node: &str,
) -> Result<crate::config::PeerConfig, (StatusCode, String)> {
    let peer =
        peer_by_id(state, node).map_err(|error| (StatusCode::NOT_FOUND, error.to_string()))?;
    if state.peer_url(&peer).is_none() {
        return Err((
            StatusCode::BAD_GATEWAY,
            format!("{} is offline or has no reachable URL", peer.name),
        ));
    }
    Ok(peer)
}

pub(super) fn rewrite_open_node(value: &mut Value, node: &str) {
    let Some(open) = value.get_mut("open").and_then(Value::as_object_mut) else {
        return;
    };
    if open.contains_key("module") {
        open.insert("node".into(), json!(node));
    } else if let Some(app) = open.get_mut("app").and_then(Value::as_object_mut) {
        app.insert("node".into(), json!(node));
    }
}

pub(super) async fn node_list_builds(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
) -> Response {
    if node == "local" {
        return list_builds(State(state)).await;
    }
    let peer = match build_peer(&state, &node) {
        Ok(peer) => peer,
        Err((status, message)) => return (status, message).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    match peers::fetch_json(&state.client, &peer, Some(&url), "/api/v1/peer/build").await {
        Ok(value) => Json(value).into_response(),
        Err(error) if error.to_string().contains("404") => (
            StatusCode::BAD_GATEWAY,
            format!(
                "{} does not support remote builds yet; update Orqella on that machine",
                peer.name
            ),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("could not list builds on {}: {error}", peer.name),
        )
            .into_response(),
    }
}

pub(super) async fn node_start_build(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Json(body): Json<BuildRequest>,
) -> Response {
    if node == "local" {
        return start_build(State(state), Json(body)).await;
    }
    let peer = match build_peer(&state, &node) {
        Ok(peer) => peer,
        Err((status, message)) => return (status, message).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let response = state
        .client
        .post(format!("{}/api/v1/peer/build", url.trim_end_matches('/')))
        .header("X-Orqella-Peer-Token", &peer.token)
        .json(&body)
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("could not reach {}: {error}", peer.name),
            )
                .into_response();
        }
    };
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status == StatusCode::NOT_FOUND {
        return (
            StatusCode::BAD_GATEWAY,
            format!(
                "{} does not support remote builds yet; update Orqella on that machine",
                peer.name
            ),
        )
            .into_response();
    }
    if !status.is_success() {
        let code = if status.is_client_error() {
            status
        } else {
            StatusCode::BAD_GATEWAY
        };
        return (code, format!("{}: {text}", peer.name)).into_response();
    }
    let mut value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("{} returned an invalid build response: {error}", peer.name),
            )
                .into_response();
        }
    };
    rewrite_open_node(&mut value, &node);
    Json(value).into_response()
}

pub(super) async fn node_open_build(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    if util::validate_module_id(&id).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid build job id").into_response();
    }
    if node == "local" {
        return open_build_session(State(state), Path(id)).await;
    }
    let peer = match build_peer(&state, &node) {
        Ok(peer) => peer,
        Err((status, message)) => return (status, message).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let response = state
        .client
        .get(format!(
            "{}/api/v1/peer/build/{}/open",
            url.trim_end_matches('/'),
            id
        ))
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("could not reach {}: {error}", peer.name),
            )
                .into_response();
        }
    };
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if status == StatusCode::NOT_FOUND {
        return (
            StatusCode::BAD_GATEWAY,
            format!(
                "{} does not support remote builds yet; update Orqella on that machine",
                peer.name
            ),
        )
            .into_response();
    }
    if !status.is_success() {
        let code = if status.is_client_error() {
            status
        } else {
            StatusCode::BAD_GATEWAY
        };
        return (code, format!("{}: {text}", peer.name)).into_response();
    }
    let mut value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("{} returned an invalid build response: {error}", peer.name),
            )
                .into_response();
        }
    };
    rewrite_open_node(&mut value, &node);
    Json(value).into_response()
}

pub(super) async fn peer_list_builds(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    list_builds(State(state)).await
}

pub(super) async fn peer_start_build(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<BuildRequest>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    state.audit("peer.build", json!({ "remote": true }));
    start_build(State(state), Json(body)).await
}

pub(super) async fn peer_open_build(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    open_build_session(State(state), Path(id)).await
}
