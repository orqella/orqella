use super::*;
use std::time::Duration;

/// Remote module index reads may need to download the peer's index.
const PEER_MODULE_INDEX_TIMEOUT_SECS: u64 = 30;
/// Remote installs may clone or download a module archive.
const PEER_MODULE_INSTALL_TIMEOUT_SECS: u64 = 600;
/// Remote module management may restart sidecars or update a checkout.
const PEER_MODULE_MANAGE_TIMEOUT_SECS: u64 = 300;
/// Remote exports stream a signed module package.
const PEER_MODULE_EXPORT_TIMEOUT_SECS: u64 = 120;

pub(super) async fn sync_module(
    State(state): State<Arc<AppState>>,
    Path((peer_id, module_id)): Path<(String, String)>,
) -> Response {
    let Some(module) = state.modules.get(&module_id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    let peer = match peer_by_id(&state, &peer_id) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let peer_url = match state.peer_url(&peer) {
        Some(url) => url,
        None => return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response(),
    };
    let key = match crate::modules::signing::load_or_create_key(&state.paths) {
        Ok(key) => key,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response();
        }
    };
    let temporary = match tempfile::Builder::new()
        .prefix("orqella-module-sync-")
        .tempfile_in(&state.paths.runtime_dir)
    {
        Ok(file) => file.into_temp_path(),
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not create sync archive: {error}"),
            )
                .into_response();
        }
    };
    let export_path = temporary.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        crate::modules::package::export_signed_to(&module, &key, &export_path)
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("module sync export worker failed: {error}"),
            )
                .into_response();
        }
    }
    let file = match tokio::fs::File::open(temporary.to_path_buf()).await {
        Ok(file) => file,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not open sync archive: {error}"),
            )
                .into_response();
        }
    };
    let target = format!(
        "{}/api/v1/peer/modules/import",
        peer_url.trim_end_matches('/')
    );
    let response = state
        .client
        .post(target)
        .header("X-Orqella", "1")
        .header("X-Orqella-Peer-Token", &peer.token)
        .header("Content-Type", "application/zip")
        .body(reqwest::Body::from(file))
        .timeout(std::time::Duration::from_secs(60))
        .send()
        .await;
    match response {
        Ok(response) if response.status().is_success() => {
            state.audit(
                "module.sync",
                json!({ "module": module_id, "peer": peer_id }),
            );
            Json(json!({ "ok": true, "peer": peer_id, "module": module_id })).into_response()
        }
        Ok(response) => (
            StatusCode::BAD_GATEWAY,
            format!(
                "peer import failed: {}",
                response.text().await.unwrap_or_default()
            ),
        )
            .into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("peer sync failed: {error}"),
        )
            .into_response(),
    }
}

pub(super) async fn list_modules(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let auth = api_auth_level(&state, &headers);
    let privileged = auth == AuthLevel::Privileged;
    let authenticated = matches!(auth, AuthLevel::Privileged | AuthLevel::ReadOnly);
    let modules = state
        .modules
        .list()
        .into_iter()
        .map(|module| {
            let mut value = serde_json::to_value(&module).unwrap_or_else(|_| json!({}));
            value["sidecar"] = serde_json::to_value(state.sidecars.status(&module.manifest.id))
                .unwrap_or(Value::Null);
            // The browser shell needs this module-scoped token to open the
            // module WebSocket through its broker. A read-only shell can
            // already load the module UI, so the token grants no extra access.
            if authenticated {
                value["ws_token"] = json!(state.ws_token(&module.manifest.id));
            }
            if !privileged && let Some(object) = value.as_object_mut() {
                object.remove("config");
                object.remove("source");
                object.remove("git");
            }
            value
        })
        .collect::<Vec<_>>();
    Json(modules).into_response()
}

pub(super) async fn module_detail(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.modules.get(&id) {
        Some(module) => Json(json!({
            "manifest": module.manifest.summary(),
            "enabled": module.state.enabled,
            "granted_permissions": module.state.granted_permissions,
            "pending_permissions": module.state.pending_permissions,
            "config_schema": module.manifest.config,
            "config": module.state.config,
            "sidecar_running": state.sidecars.is_running(&id),
            "sidecar": state.sidecars.status(&id),
            "signature": signature_report(&state, &module),
            "ws_token": state.ws_token(&id),
        }))
        .into_response(),
        None => (StatusCode::NOT_FOUND, "module not found").into_response(),
    }
}

fn signature_report(state: &AppState, module: &crate::modules::store::InstalledModule) -> Value {
    let Ok(Some(record)) = crate::modules::signing::read_signature(&module.root) else {
        return json!({ "signed": false, "trusted": false, "fingerprint": null });
    };
    let trusted = state
        .config()
        .trusted_module_keys
        .iter()
        .any(|key| key.public_key.eq_ignore_ascii_case(&record.public_key));
    let signature_valid =
        crate::modules::signing::verify_tree(&module.root, &[], false, true).is_ok();
    json!({
        "signed": true,
        "signature_valid": signature_valid,
        "trusted": trusted,
        "fingerprint": record.fingerprint,
        "public_key": record.public_key,
    })
}

pub(super) async fn remove_module(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    state.sidecars.stop(&id);
    let modules = Arc::clone(&state.modules);
    let remove_id = id.clone();
    let result = tokio::task::spawn_blocking(move || modules.remove(&remove_id)).await;
    match result {
        Ok(Ok(())) => {
            state.invalidate_ws_token(&id);
            state.audit("module.remove", json!({ "module": id }));
            Json(json!({ "ok": true })).into_response()
        }
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("module remove worker failed: {error}"),
        )
            .into_response(),
    }
}

pub(super) async fn restart_sidecar(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(module) = state.modules.get(&id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    if module.manifest.backend.kind != BackendKind::Sidecar {
        return (StatusCode::BAD_REQUEST, "module has no sidecar").into_response();
    }
    if !module.grants("backend") {
        return (
            StatusCode::FORBIDDEN,
            "module has not been granted the backend permission",
        )
            .into_response();
    }
    if !module.state.enabled {
        return (StatusCode::CONFLICT, "module is disabled").into_response();
    }
    state.sidecars.stop(&id);
    match state.sidecars.start(&module).await {
        Ok(()) => {
            state.audit("module.sidecar.restart", json!({ "module": id }));
            Json(json!({
                "ok": true,
                "sidecar": state.sidecars.status(&id),
            }))
            .into_response()
        }
        Err(error) => {
            let sidecar = state.sidecars.record_failure(&id, error.to_string());
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "ok": false,
                    "error": error.to_string(),
                    "sidecar": sidecar,
                })),
            )
                .into_response()
        }
    }
}

pub(super) async fn module_log(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(_module) = state.modules.get(&id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    let path = state.sidecars.log_path(&id);
    let text = tokio::task::spawn_blocking(move || read_log_tail(&path))
        .await
        .unwrap_or_default();
    Json(json!({ "module": id, "text": text })).into_response()
}

fn read_log_tail(path: &std::path::Path) -> String {
    use std::io::{Read, Seek, SeekFrom};
    const TAIL_BYTES: u64 = 64 * 1024;
    const TAIL_CHARS: usize = 30_000;
    let Ok(mut file) = crate::util::open_read_no_follow(path) else {
        return String::new();
    };
    let Ok(len) = file.metadata().map(|metadata| metadata.len()) else {
        return String::new();
    };
    if len > TAIL_BYTES && file.seek(SeekFrom::End(-(TAIL_BYTES as i64))).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let skip = text.chars().count().saturating_sub(TAIL_CHARS);
    text.chars().skip(skip).collect()
}

pub(super) async fn list_trusted_keys(State(state): State<Arc<AppState>>) -> Response {
    Json(state.config().trusted_module_keys.clone()).into_response()
}

#[derive(Deserialize)]
pub(super) struct TrustedKeyInput {
    public_key: String,
    #[serde(default)]
    label: String,
}

pub(super) async fn add_trusted_key(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<TrustedKeyInput>,
) -> Response {
    let supplied = headers
        .get("x-orqella-admin-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if !constant_time_eq(supplied.as_bytes(), state.admin_token.as_bytes()) {
        return (StatusCode::UNAUTHORIZED, "invalid admin token").into_response();
    }
    let public_key = body.public_key.trim();
    if public_key.len() != 64 || hex::decode(public_key).is_err() {
        return (
            StatusCode::BAD_REQUEST,
            "public key must be 32 bytes of hex",
        )
            .into_response();
    }
    let already_trusted = state
        .config()
        .trusted_module_keys
        .iter()
        .any(|key| key.public_key.eq_ignore_ascii_case(public_key));
    if !already_trusted {
        let trusted = crate::config::TrustedKey {
            public_key: public_key.to_string(),
            label: body.label,
        };
        if let Err(error) = state.update_config(|config| {
            config.trusted_module_keys.push(trusted);
        }) {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
        state
            .modules
            .set_trusted_keys(state.config().trusted_module_keys.clone());
    }
    state.audit("trusted_key.add", json!({ "public_key": public_key }));
    Json(json!({ "ok": true })).into_response()
}

pub(super) async fn export_module(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(module) = state.modules.get(&id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    let key = match crate::modules::signing::load_or_create_key(&state.paths) {
        Ok(key) => key,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response();
        }
    };
    let temporary = match tempfile::Builder::new()
        .prefix("orqella-module-export-")
        .tempfile_in(&state.paths.runtime_dir)
    {
        Ok(file) => file,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not create export file: {error}"),
            )
                .into_response();
        }
    };
    let export_path = temporary.path().to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        crate::modules::package::export_signed_to(&module, &key, &export_path)
    })
    .await;
    match result {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("module export worker failed: {error}"),
            )
                .into_response();
        }
    }
    let temp_path = temporary.into_temp_path();
    let file = match tokio::fs::File::open(temp_path.to_path_buf()).await {
        Ok(file) => file,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not open module export: {error}"),
            )
                .into_response();
        }
    };
    let body = stream::unfold(
        (file, temp_path, vec![0_u8; 64 * 1024]),
        |(mut file, temp_path, mut buffer)| async move {
            match file.read(&mut buffer).await {
                Ok(0) => None,
                Ok(read) => Some((
                    Ok::<Bytes, std::io::Error>(Bytes::copy_from_slice(&buffer[..read])),
                    (file, temp_path, buffer),
                )),
                Err(error) => Some((
                    Err::<Bytes, std::io::Error>(error),
                    (file, temp_path, buffer),
                )),
            }
        },
    );
    (
        [
            (header::CONTENT_TYPE, "application/zip".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{id}.orqella-module.zip\""),
            ),
        ],
        Body::from_stream(body),
    )
        .into_response()
}

pub(super) async fn peer_import_module(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    import_module(State(state), body).await
}

pub(super) async fn import_module(State(state): State<Arc<AppState>>, body: Bytes) -> Response {
    let modules = Arc::clone(&state.modules);
    let trusted = state.config().trusted_module_keys.clone();
    let result = tokio::task::spawn_blocking(move || {
        let temp = crate::modules::package::import(&body)?;
        crate::modules::signing::verify_tree(temp.path(), &trusted, false, false)?;
        modules.install_from_path_restricted(temp.path())
    })
    .await;
    let installed = match result {
        Ok(result) => result,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("module import worker failed: {error}"),
            )
                .into_response();
        }
    };
    match installed {
        Ok(module) => {
            state.invalidate_ws_token(&module.manifest.id);
            state.sidecars.stop(&module.manifest.id);
            if module.manifest.backend == BackendKind::Sidecar
                && let Some(installed) = state.modules.get(&module.manifest.id)
                && installed.state.enabled
                && installed.grants("backend")
                && let Err(error) = state.sidecars.start(&installed).await
            {
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("module imported, but sidecar failed: {error:#}"),
                )
                    .into_response();
            }
            state.audit("module.import", json!({ "module": module.manifest.id }));
            Json(json!({ "ok": true, "module": module })).into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct InstallModule {
    #[serde(default)]
    path: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    branch: String,
    #[serde(default)]
    allow_unsigned: bool,
    /// Install by module ID from the configured module index.
    #[serde(default)]
    index_id: String,
    /// Optional version pin for an index install.
    #[serde(default)]
    index_version: String,
}

pub(super) async fn get_module_index(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config();
    match crate::modules::index::fetch(&state.index_client, &config.modules.index_url).await {
        Ok(index) => Json(index.catalog(
            &state.modules,
            &crate::modules::manifest::host_platform(),
            crate::modules::index::MODULE_API_VERSION,
            &config.modules.index_url,
        ))
        .into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("module index unavailable: {error:#}"),
        )
            .into_response(),
    }
}

/// A peer's 401 means the hub's stored peer token is stale, not that this
/// browser session expired. Do not echo it to the shell, where api() would
/// reload the page.
fn peer_error_status(status: StatusCode) -> StatusCode {
    if status.is_client_error() && status != StatusCode::UNAUTHORIZED {
        status
    } else {
        StatusCode::BAD_GATEWAY
    }
}

pub(super) async fn node_module_index(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
) -> Response {
    if node == "local" {
        return get_module_index(State(state)).await;
    }
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let response = state
        .client
        .get(format!(
            "{}/api/v1/peer/module-index",
            url.trim_end_matches('/')
        ))
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(Duration::from_secs(PEER_MODULE_INDEX_TIMEOUT_SECS))
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
                "{} does not support the module index yet; update Orqella on that machine",
                peer.name
            ),
        )
            .into_response();
    }
    if !status.is_success() {
        let code = peer_error_status(status);
        return (code, format!("{}: {text}", peer.name)).into_response();
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("{} returned an invalid module index: {error}", peer.name),
        )
            .into_response(),
    }
}

pub(super) async fn peer_module_index(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    get_module_index(State(state)).await
}

pub(super) async fn node_install_module(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Json(body): Json<InstallModule>,
) -> Response {
    if node == "local" {
        return install_module(State(state), Json(body)).await;
    }
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let response = state
        .client
        .post(format!(
            "{}/api/v1/peer/modules/install",
            url.trim_end_matches('/')
        ))
        .header("X-Orqella-Peer-Token", &peer.token)
        .json(&body)
        .timeout(Duration::from_secs(PEER_MODULE_INSTALL_TIMEOUT_SECS))
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
                "{} does not support remote module installs yet; update Orqella on that machine",
                peer.name
            ),
        )
            .into_response();
    }
    if !status.is_success() {
        let code = peer_error_status(status);
        return (code, format!("{}: {text}", peer.name)).into_response();
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(mut value) => {
            if let Some(module) = value.get_mut("module").and_then(Value::as_object_mut) {
                // The peer's module token is scoped to the peer's own shell.
                module.remove("ws_token");
            }
            Json(value).into_response()
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("{} returned an invalid install result: {error}", peer.name),
        )
            .into_response(),
    }
}

pub(super) async fn peer_install_module(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<InstallModule>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    state.audit("peer.module.install", json!({ "remote": true }));
    install_module(State(state), Json(body)).await
}

async fn forward_peer_module(
    state: &Arc<AppState>,
    node: &str,
    id: &str,
    method: Method,
    suffix: &str,
    body: Option<Value>,
) -> Response {
    if let Err(error) = util::validate_module_id(id) {
        return (StatusCode::BAD_REQUEST, error.to_string()).into_response();
    }
    let peer = match peer_by_id(state, node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let target = format!(
        "{}/api/v1/peer/modules/{}{}",
        url.trim_end_matches('/'),
        id,
        suffix
    );
    let mut request = state
        .client
        .request(method, target)
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(Duration::from_secs(PEER_MODULE_MANAGE_TIMEOUT_SECS));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = match request.send().await {
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
    if status == StatusCode::NOT_FOUND && text.trim().is_empty() {
        return (
            StatusCode::BAD_GATEWAY,
            format!(
                "{} does not support remote module management yet; update Orqella on that machine",
                peer.name
            ),
        )
            .into_response();
    }
    if !status.is_success() {
        let code = peer_error_status(status);
        return (code, format!("{}: {text}", peer.name)).into_response();
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(value) => Json(value).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("{} returned an invalid response: {error}", peer.name),
        )
            .into_response(),
    }
}

async fn forward_peer_module_export(state: &Arc<AppState>, node: &str, id: &str) -> Response {
    if let Err(error) = util::validate_module_id(id) {
        return (StatusCode::BAD_REQUEST, error.to_string()).into_response();
    }
    let peer = match peer_by_id(state, node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let response = state
        .client
        .get(format!(
            "{}/api/v1/peer/modules/{}/export",
            url.trim_end_matches('/'),
            id
        ))
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(Duration::from_secs(PEER_MODULE_EXPORT_TIMEOUT_SECS))
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
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        if status == StatusCode::NOT_FOUND && text.trim().is_empty() {
            return (
                StatusCode::BAD_GATEWAY,
                format!(
                    "{} does not support remote module exports yet; update Orqella on that machine",
                    peer.name
                ),
            )
                .into_response();
        }
        let code = peer_error_status(status);
        return (code, format!("{}: {text}", peer.name)).into_response();
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/zip")
        .to_string();
    let disposition = response
        .headers()
        .get(header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .unwrap_or_else(|| format!("attachment; filename=\"{id}.orqella-module.zip\""));
    let body = Body::from_stream(response.bytes_stream());
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_DISPOSITION, disposition),
        ],
        body,
    )
        .into_response()
}

pub(super) async fn node_module_enable(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    if node == "local" {
        return enable_module(State(state), Path(id)).await;
    }
    forward_peer_module(&state, &node, &id, Method::POST, "/enable", Some(json!({}))).await
}

pub(super) async fn node_module_disable(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    if node == "local" {
        return disable_module(State(state), Path(id)).await;
    }
    forward_peer_module(
        &state,
        &node,
        &id,
        Method::POST,
        "/disable",
        Some(json!({})),
    )
    .await
}

pub(super) async fn node_module_permissions(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
    Json(body): Json<ModulePermissions>,
) -> Response {
    if node == "local" {
        return set_module_permissions(State(state), Path(id), Json(body)).await;
    }
    forward_peer_module(
        &state,
        &node,
        &id,
        Method::POST,
        "/permissions",
        Some(json!(body)),
    )
    .await
}

pub(super) async fn node_module_config(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
    Json(body): Json<ModuleConfigInput>,
) -> Response {
    if node == "local" {
        return set_module_config(State(state), Path(id), Json(body)).await;
    }
    forward_peer_module(
        &state,
        &node,
        &id,
        Method::POST,
        "/config",
        Some(json!(body)),
    )
    .await
}

pub(super) async fn node_module_log(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    if node == "local" {
        return module_log(State(state), Path(id)).await;
    }
    forward_peer_module(&state, &node, &id, Method::GET, "/log", None).await
}

pub(super) async fn node_module_restart_sidecar(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    if node == "local" {
        return restart_sidecar(State(state), Path(id)).await;
    }
    forward_peer_module(
        &state,
        &node,
        &id,
        Method::POST,
        "/sidecar/restart",
        Some(json!({})),
    )
    .await
}

pub(super) async fn node_module_update(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
    body: Option<Json<UpdateModuleGit>>,
) -> Response {
    if node == "local" {
        return update_module_git(State(state), Path(id), body).await;
    }
    let payload = body
        .map(|Json(body)| json!(body))
        .unwrap_or_else(|| json!({}));
    forward_peer_module(&state, &node, &id, Method::POST, "/update", Some(payload)).await
}

pub(super) async fn node_module_checkpoint(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
    Json(body): Json<CheckpointModule>,
) -> Response {
    if node == "local" {
        return checkpoint_module(State(state), Path(id), Json(body)).await;
    }
    forward_peer_module(
        &state,
        &node,
        &id,
        Method::POST,
        "/checkpoint",
        Some(json!(body)),
    )
    .await
}

pub(super) async fn node_module_remove(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    if node == "local" {
        return remove_module(State(state), Path(id)).await;
    }
    forward_peer_module(&state, &node, &id, Method::DELETE, "", None).await
}

pub(super) async fn node_module_export(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    if node == "local" {
        return export_module(State(state), Path(id)).await;
    }
    forward_peer_module_export(&state, &node, &id).await
}

pub(super) async fn peer_module_enable(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    enable_module(State(state), Path(id)).await
}

pub(super) async fn peer_module_disable(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    disable_module(State(state), Path(id)).await
}

pub(super) async fn peer_module_permissions(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ModulePermissions>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    set_module_permissions(State(state), Path(id), Json(body)).await
}

pub(super) async fn peer_module_config(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<ModuleConfigInput>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    set_module_config(State(state), Path(id), Json(body)).await
}

pub(super) async fn peer_module_log(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    module_log(State(state), Path(id)).await
}

pub(super) async fn peer_module_restart_sidecar(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    restart_sidecar(State(state), Path(id)).await
}

pub(super) async fn peer_module_update(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Option<Json<UpdateModuleGit>>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    update_module_git(State(state), Path(id), body).await
}

pub(super) async fn peer_module_checkpoint(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<CheckpointModule>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    checkpoint_module(State(state), Path(id), Json(body)).await
}

pub(super) async fn peer_module_remove(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    remove_module(State(state), Path(id)).await
}

pub(super) async fn peer_module_export(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    export_module(State(state), Path(id)).await
}

pub(super) async fn install_module(
    State(state): State<Arc<AppState>>,
    Json(body): Json<InstallModule>,
) -> Response {
    let url = body.url.trim().to_string();
    let branch = body.branch.trim().to_string();
    let path = body.path.trim().to_string();
    let allow_unsigned = body.allow_unsigned;
    let index_id = body.index_id.trim().to_string();
    let index_version = body.index_version.trim().to_string();

    if !index_id.is_empty() {
        let config = state.config();
        let index = match crate::modules::index::fetch(
            &state.index_client,
            &config.modules.index_url,
        )
        .await
        {
            Ok(index) => index,
            Err(error) => {
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("module index unavailable: {error:#}"),
                )
                    .into_response();
            }
        };
        let target = match index.resolve(
            &index_id,
            (!index_version.is_empty()).then_some(index_version.as_str()),
            &crate::modules::manifest::host_platform(),
            crate::modules::index::MODULE_API_VERSION,
        ) {
            Ok(target) => target,
            Err(error) => return (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
        };
        let installed_version = target.version.clone();
        let installed =
            crate::modules::index::install(&state.index_client, Arc::clone(&state.modules), target)
                .await;
        return finish_install(
            &state,
            installed,
            "module.index_install",
            Some(json!({ "index": index_id, "version": installed_version })),
        )
        .await;
    }

    if url.is_empty() && path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "module path, repository URL, or index ID is required",
        )
            .into_response();
    }
    let modules = Arc::clone(&state.modules);
    let result = tokio::task::spawn_blocking(move || {
        if !url.is_empty() {
            modules.install_from_source_with_options(
                &url,
                if branch.is_empty() {
                    None
                } else {
                    Some(branch.as_str())
                },
                allow_unsigned,
            )
        } else {
            modules.install_from_path(std::path::Path::new(&path))
        }
    })
    .await;
    let installed = match result {
        Ok(result) => result,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("module install worker failed: {error}"),
            )
                .into_response();
        }
    };
    finish_install(&state, installed, "module.install", None).await
}

async fn finish_install(
    state: &Arc<AppState>,
    installed: Result<crate::modules::store::ModuleView>,
    event: &str,
    extra: Option<serde_json::Value>,
) -> Response {
    match installed {
        Ok(module) => {
            state.invalidate_ws_token(&module.manifest.id);
            state.sidecars.stop(&module.manifest.id);
            let mut sidecar_error = None;
            if module.manifest.backend == BackendKind::Sidecar
                && let Some(installed) = state.modules.get(&module.manifest.id)
                && installed.state.enabled
                && installed.grants("backend")
                && let Err(error) = state.sidecars.start(&installed).await
            {
                sidecar_error = Some(format!("{error:#}"));
            }
            let mut payload = json!({
                "module": module.manifest.id,
                "source": module.source,
                "sidecar_error": sidecar_error,
            });
            if let (Some(serde_json::Value::Object(extra)), Some(payload)) =
                (extra, payload.as_object_mut())
            {
                payload.extend(extra);
            }
            state.audit(event, payload);
            Json(json!({
                "ok": true,
                "module": module,
                "sidecar_error": sidecar_error
            }))
            .into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct CheckpointModule {
    #[serde(default)]
    message: String,
}

pub(super) async fn checkpoint_module(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<CheckpointModule>,
) -> Response {
    let message = body.message.trim();
    let message = if message.is_empty() {
        "Checkpoint from Orqella"
    } else {
        message
    }
    .to_string();
    let modules = Arc::clone(&state.modules);
    let checkpoint_id = id.clone();
    let result =
        tokio::task::spawn_blocking(move || modules.checkpoint(&checkpoint_id, &message)).await;
    let checkpoint = match result {
        Ok(result) => result,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("module checkpoint worker failed: {error}"),
            )
                .into_response();
        }
    };
    match checkpoint {
        Ok(module) => {
            state.audit("module.checkpoint", json!({ "module": id }));
            Json(json!({ "ok": true, "module": module })).into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}

#[derive(Deserialize, Serialize, Default)]
pub(super) struct UpdateModuleGit {
    #[serde(default)]
    allow_unsigned: bool,
}

pub(super) async fn update_module_git(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<UpdateModuleGit>>,
) -> Response {
    let allow_unsigned = body.is_some_and(|Json(body)| body.allow_unsigned);
    let was_running = state.sidecars.is_running(&id);
    if was_running {
        // Stop the old sidecar before changing files. A watcher-backed
        // sidecar must not load an unverified tree during the pull.
        state.sidecars.stop(&id);
    }
    let modules = Arc::clone(&state.modules);
    let update_id = id.clone();
    let result = tokio::task::spawn_blocking(move || {
        modules.update_from_git_with_options(&update_id, allow_unsigned)
    })
    .await;
    let updated = match result {
        Ok(result) => result,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("module update worker failed: {error}"),
            )
                .into_response();
        }
    };
    match updated {
        Ok(module) => {
            state.invalidate_ws_token(&id);
            if was_running
                && let Some(installed) = state.modules.get(&id)
                && installed.state.enabled
                && installed.grants("backend")
                && let Err(error) = state.sidecars.start(&installed).await
            {
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("module updated, but its sidecar failed to restart: {error:#}"),
                )
                    .into_response();
            }
            state.audit("module.update", json!({ "module": id }));
            Json(json!({ "ok": true, "module": module })).into_response()
        }
        Err(error) => {
            if was_running
                && let Some(installed) = state.modules.get(&id)
                && installed.state.enabled
                && installed.grants("backend")
            {
                let _ = state.sidecars.start(&installed).await;
            }
            (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response()
        }
    }
}

pub(super) async fn enable_module(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.modules.set_enabled(&id, true) {
        Ok(()) => {
            if let Some(module) = state.modules.get(&id)
                && module.manifest.backend.kind == BackendKind::Sidecar
                && module.grants("backend")
                && let Err(error) = state.sidecars.start(&module).await
            {
                return (StatusCode::BAD_GATEWAY, error.to_string()).into_response();
            }
            state.audit("module.enable", json!({ "module": id }));
            Json(json!({ "ok": true })).into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

pub(super) async fn disable_module(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.modules.set_enabled(&id, false) {
        Ok(()) => {
            state.sidecars.stop(&id);
            state.audit("module.disable", json!({ "module": id }));
            Json(json!({ "ok": true })).into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct ModulePermissions {
    permissions: Vec<String>,
    #[serde(default)]
    sandbox_acknowledged: bool,
}

pub(super) async fn set_module_permissions(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ModulePermissions>,
) -> Response {
    let Some(module) = state.modules.get(&id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    let grants_backend = body
        .permissions
        .iter()
        .any(|permission| permission == "backend");
    if grants_backend && !state.sidecars.effective_sandbox(&module) && !body.sandbox_acknowledged {
        return (
            StatusCode::BAD_REQUEST,
            "confirm that this module's sidecar runs unsandboxed before granting backend",
        )
            .into_response();
    }
    match state.modules.set_permissions(&id, body.permissions) {
        Ok(module) => {
            if module.manifest.backend == BackendKind::Sidecar {
                let backend_granted = module
                    .granted_permissions
                    .iter()
                    .any(|permission| permission == "backend");
                if backend_granted && module.enabled {
                    if let Some(installed) = state.modules.get(&id)
                        && let Err(error) = state.sidecars.start(&installed).await
                    {
                        return (
                            StatusCode::BAD_GATEWAY,
                            format!("permissions saved, but sidecar failed to start: {error:#}"),
                        )
                            .into_response();
                    }
                } else {
                    state.sidecars.stop(&id);
                }
            }
            state.audit(
                "module.permissions",
                json!({ "module": id, "granted": module.granted_permissions }),
            );
            Json(json!({ "ok": true, "module": module })).into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}

#[derive(Deserialize, Serialize)]
pub(super) struct ModuleConfigInput {
    values: std::collections::BTreeMap<String, serde_json::Value>,
}

pub(super) async fn set_module_config(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    Json(body): Json<ModuleConfigInput>,
) -> Response {
    match state.modules.set_config(&id, body.values) {
        Ok(module) => {
            if module.manifest.backend == BackendKind::Sidecar {
                let backend_granted = module
                    .granted_permissions
                    .iter()
                    .any(|permission| permission == "backend");
                state.sidecars.stop(&id);
                if backend_granted
                    && module.enabled
                    && let Some(installed) = state.modules.get(&id)
                    && let Err(error) = state.sidecars.start(&installed).await
                {
                    return (
                        StatusCode::BAD_GATEWAY,
                        format!("settings saved, but sidecar restart failed: {error:#}"),
                    )
                        .into_response();
                }
            }
            state.audit("module.config", json!({ "module": id }));
            Json(json!({ "ok": true, "module": module })).into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    }
}
