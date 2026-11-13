use super::*;

pub(super) async fn peer_ping(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    Json(json!({ "ok": true })).into_response()
}

pub(super) async fn peer_info(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    Json(json!({
        "id": "local",
        "name": local_name(),
        "version": env!("CARGO_PKG_VERSION"),
        "dashboard": state.config().server.dashboard,
        "modules": state.modules.summaries(),
    }))
    .into_response()
}

pub(super) async fn peer_system(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    let _ = state;
    match tokio::task::spawn_blocking(metrics::snapshot).await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("metrics unavailable: {error}"),
        )
            .into_response(),
    }
}

pub(super) fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

pub(super) fn peer_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let token = headers
        .get("x-orqella-peer-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let expected = state.config().peer_token().to_string();
    !expected.is_empty() && constant_time_eq(token.as_bytes(), expected.as_bytes())
}

pub(super) async fn audit(State(state): State<Arc<AppState>>) -> Response {
    if !state.config().audit.enabled {
        return Json(json!({ "enabled": false, "events": [] })).into_response();
    }
    let path = state.paths.data_dir.join("audit.jsonl");
    let mut events: Vec<Value> = std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    events.reverse();
    events.truncate(200);
    Json(json!({ "enabled": true, "events": events })).into_response()
}
