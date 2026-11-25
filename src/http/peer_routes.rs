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
    match metrics::snapshot_async().await {
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

fn peer_identity(token: &str, source: &str, expected: &str) -> Option<String> {
    if expected.is_empty() || !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return None;
    }
    Some(source.to_string())
}

pub(super) fn peer_source_device(state: &AppState, headers: &HeaderMap) -> Option<String> {
    let token = headers
        .get("x-orqella-peer-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let source = headers
        .get("x-orqella-source-device")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    peer_identity(token, source, state.config().peer_token())
}

pub(super) fn peer_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    peer_source_device(state, headers).is_some()
}

/// Sign a source device's registered SSH key with this target's local CA.
///
/// A source reaches this endpoint through this target's iroh bridge, which
/// validates the signed Link ticket and injects the source device identity.
/// The key that gets signed is always the one the account registered for that
/// device, so a caller cannot substitute a key it controls.
pub(super) async fn peer_ssh_cert(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let Some(source_id) = peer_source_device(&state, &headers) else {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    };
    if source_id.is_empty() {
        return (
            StatusCode::FORBIDDEN,
            "an SSH certificate requires a source device token",
        )
            .into_response();
    }
    let link = state.link.read().expect("link lock").clone();
    let ssh = state.link_ssh.read().expect("link ssh lock").clone();
    let (Some(link), Some(ssh)) = (link, ssh) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Link is not running on this machine",
        )
            .into_response();
    };
    let public_key_text = match link.source_ssh_public_key(&source_id).await {
        Ok(Some(key)) => key,
        Ok(None) => {
            return (
                StatusCode::CONFLICT,
                "source device has no registered SSH key",
            )
                .into_response();
        }
        Err(error) => {
            tracing::warn!(%error, "could not verify the source device for SSH");
            return (
                StatusCode::BAD_GATEWAY,
                "could not verify the source device",
            )
                .into_response();
        }
    };
    let public_key = match russh::keys::PublicKey::from_openssh(public_key_text.trim()) {
        Ok(public_key) => public_key,
        Err(error) => {
            tracing::warn!(%error, "registered source SSH key is invalid");
            return (StatusCode::CONFLICT, "source device has an invalid SSH key").into_response();
        }
    };
    let target_id = link.status().device_id;
    let principal = format!("orqella-target-{target_id}");
    let (certificate, expires_at) = match ssh.sign_user_certificate(
        &public_key,
        &principal,
        &source_id,
        std::time::Duration::from_secs(600),
    ) {
        Ok(issued) => issued,
        Err(error) => {
            tracing::warn!(%error, "could not sign source SSH certificate");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not issue SSH certificate",
            )
                .into_response();
        }
    };
    state.audit(
        "ssh.certificate_issued",
        json!({
            "source_device_id": source_id,
            "target_device_id": target_id,
        }),
    );
    Json(json!({
        "certificate": certificate,
        "expires_at": expires_at,
        "ttl_seconds": 600,
        "principal": principal,
        "key_id": source_id,
    }))
    .into_response()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_identity_requires_the_local_secret() {
        assert_eq!(
            peer_identity("target-secret", "source-device", "target-secret"),
            Some("source-device".to_string())
        );
        assert_eq!(
            peer_identity("wrong", "source-device", "target-secret"),
            None
        );
        assert_eq!(
            peer_identity("target-secret", "", "target-secret"),
            Some(String::new())
        );
        assert_eq!(peer_identity("target-secret", "source", ""), None);
    }
}
