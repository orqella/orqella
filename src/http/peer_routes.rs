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

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

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

fn pair_signature(secret: &str, source_device_id: &str, expires: u64) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts arbitrary key lengths");
    mac.update(b"orqella-peer-v1\n");
    mac.update(source_device_id.as_bytes());
    mac.update(b"\n");
    mac.update(expires.to_string().as_bytes());
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Allow a small clock difference between Link and the target. It only extends
/// a token's usable life by this many seconds.
const PEER_TOKEN_CLOCK_SKEW_SECONDS: u64 = 30;

pub(super) fn peer_authorized(state: &AppState, headers: &HeaderMap) -> bool {
    let token = headers
        .get("x-orqella-peer-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let expected = state.config().peer_token().to_string();
    if expected.is_empty() {
        return false;
    }
    // Link tickets return `v1.<source-device>.<unix-expiry>.<hmac>`. The target
    // never needs Link to validate a request, and an expired token cannot be
    // replayed. Rotating the target's peer secret revokes every outstanding
    // token immediately.
    if let Some(rest) = token.strip_prefix("v1.")
        && let Some((source, rest)) = rest.split_once('.')
        && let Some((expires, signature)) = rest.split_once('.')
        && !source.is_empty()
        && let Ok(expires) = expires.parse::<u64>()
        && expires.saturating_add(PEER_TOKEN_CLOCK_SKEW_SECONDS) >= unix_now()
        && constant_time_eq(
            signature.as_bytes(),
            pair_signature(&expected, source, expires).as_bytes(),
        )
    {
        return true;
    }
    // The raw target secret remains usable for local/operator access. It is
    // never distributed to other devices by Link.
    constant_time_eq(token.as_bytes(), expected.as_bytes())
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
    fn pair_tokens_are_deterministic_source_bound_and_expiring() {
        let first = pair_signature("target-secret", "source-a", 2_000_000_000);
        assert_eq!(
            first,
            pair_signature("target-secret", "source-a", 2_000_000_000)
        );
        assert_ne!(
            first,
            pair_signature("target-secret", "source-a", 2_000_000_001)
        );
        assert_ne!(
            first,
            pair_signature("target-secret", "source-b", 2_000_000_000)
        );
        assert_ne!(
            first,
            pair_signature("other-secret", "source-a", 2_000_000_000)
        );
        assert!(!first.is_empty());
    }
}
