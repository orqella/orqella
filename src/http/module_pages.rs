use super::rpc::{RpcRequest, module_rpc};
use super::*;
use futures_util::StreamExt;
use std::io::Read;

const MAX_MODULE_ASSET_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ModuleSurface {
    Page,
    Overview,
    Settings,
}

pub(super) async fn module_index(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    serve_module_file(state, id, String::new(), ModuleSurface::Page).await
}

pub(super) async fn module_overview(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    serve_module_file(state, id, String::new(), ModuleSurface::Overview).await
}

pub(super) async fn module_settings(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    serve_module_file(state, id, String::new(), ModuleSurface::Settings).await
}

pub(super) async fn module_asset(
    State(state): State<Arc<AppState>>,
    Path((id, path)): Path<(String, String)>,
) -> Response {
    serve_module_file(state, id, path, ModuleSurface::Page).await
}

/// Runtime module data belongs to the sidecar, never to the static asset
/// route. The first path segment `data` is reserved for daemon-preserved
/// module state and may contain credentials or large model files.
pub(super) fn module_asset_allowed(relative: &str) -> bool {
    if relative.contains('\\') {
        return false;
    }
    let first = relative.split('/').next().unwrap_or_default();
    if ["data", "backend", "tests", "scripts", ".github"]
        .iter()
        .any(|part| first.eq_ignore_ascii_case(part))
    {
        return false;
    }
    !relative.split('/').any(|part| {
        part.starts_with('.')
            || part.eq_ignore_ascii_case("sidecar.log")
            || part.eq_ignore_ascii_case("module.toml")
            || part.eq_ignore_ascii_case(crate::modules::signing::SIGNATURE_FILE)
    })
}

pub(super) async fn serve_module_file(
    state: Arc<AppState>,
    id: String,
    requested: String,
    surface: ModuleSurface,
) -> Response {
    let Some(module) = state.modules.get(&id) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !module.state.enabled {
        return (StatusCode::FORBIDDEN, "module is disabled").into_response();
    }
    let relative = if requested.is_empty() {
        match surface {
            ModuleSurface::Overview => module.manifest.overview.clone(),
            ModuleSurface::Settings => module.manifest.settings.clone(),
            ModuleSurface::Page => module.manifest.entry.clone(),
        }
    } else {
        Some(requested)
    };
    let Some(relative) = relative else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !module_asset_allowed(&relative) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(path) = util::safe_join(&module.root, &relative) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(root) = module.root.canonicalize().ok() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Ok(resolved_relative) = path.strip_prefix(&root) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let resolved_relative = resolved_relative.to_string_lossy().replace('\\', "/");
    if !module_asset_allowed(&resolved_relative) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let root = module.root.clone();
    let relative_for_open = resolved_relative.clone();
    let bytes = match tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let mut file = util::open_read_beneath(&root, std::path::Path::new(&relative_for_open))?;
        let mut bytes = Vec::new();
        file.by_ref()
            .take(MAX_MODULE_ASSET_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MODULE_ASSET_BYTES {
            anyhow::bail!("module asset is too large");
        }
        Ok(bytes)
    })
    .await
    {
        Ok(Ok(bytes)) => bytes,
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    if mime.type_() == mime_guess::mime::TEXT && mime.subtype() == "html" {
        let html = String::from_utf8_lossy(&bytes);
        let sdk = include_str!("../../web/sdk.js");
        let parent = std::path::Path::new(&relative)
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""))
            .to_string_lossy()
            .trim_matches('/')
            .to_string();
        let base = if parent.is_empty() {
            format!("/m/{id}/")
        } else {
            format!("/m/{id}/{parent}/")
        };
        let inject = format!("<base href=\"{base}\"><script>{sdk}</script>");
        let html = if let Some(head) = html.find("<head>") {
            let insert = head + "<head>".len();
            format!("{}{inject}{}", &html[..insert], &html[insert..])
        } else {
            format!("{inject}{html}")
        };
        return (
            [
                (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                (header::CACHE_CONTROL, "no-store"),
                (
                    header::CONTENT_SECURITY_POLICY,
                    "sandbox allow-scripts allow-forms allow-popups; default-src 'self' data: blob:; script-src 'unsafe-inline' 'unsafe-eval' *; style-src 'unsafe-inline' *; img-src * data:; connect-src 'self' ws: wss:; frame-src 'self'",
                ),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            ],
            html,
        )
            .into_response();
    }
    (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, "no-cache"),
            (
                header::CONTENT_SECURITY_POLICY,
                "sandbox; default-src 'none'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
        ],
        bytes,
    )
        .into_response()
}

pub(super) async fn node_modules(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    headers: HeaderMap,
) -> Response {
    let privileged = api_auth_level(&state, &headers) == AuthLevel::Privileged;
    if node == "local" {
        let modules = state
            .modules
            .list()
            .into_iter()
            .map(|module| {
                let mut value = serde_json::to_value(&module).unwrap_or_else(|_| json!({}));
                value["sidecar"] = serde_json::to_value(state.sidecars.status(&module.manifest.id))
                    .unwrap_or(Value::Null);
                if privileged {
                    value["ws_token"] = json!(state.ws_token(&module.manifest.id));
                } else {
                    if let Some(object) = value.as_object_mut() {
                        object.remove("config");
                        object.remove("source");
                        object.remove("git");
                    }
                }
                value
            })
            .collect::<Vec<_>>();
        return Json(modules).into_response();
    }
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    match peers::fetch_json(&state.client, &peer, Some(&url), "/api/v1/peer/modules").await {
        Ok(mut value) => {
            if let Some(modules) = value.as_array_mut() {
                for module in modules {
                    if privileged && let Some(id) = module.get("id").and_then(Value::as_str) {
                        let scope = format!("{node}:{id}");
                        module["ws_token"] = json!(state.ws_token(&scope));
                    } else if let Some(object) = module.as_object_mut() {
                        object.remove("config");
                        object.remove("source");
                        object.remove("git");
                    }
                }
            }
            Json(value).into_response()
        }
        Err(error) => {
            let message = if error.to_string().contains("404") {
                "this machine is running an older Orqella build; update it to enable remote modules"
                    .to_string()
            } else {
                format!("could not load peer modules: {error:#}")
            };
            (StatusCode::BAD_GATEWAY, message).into_response()
        }
    }
}

pub(super) async fn node_module_rpc(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
    Json(body): Json<RpcRequest>,
) -> Response {
    if util::validate_module_id(&id).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid module id").into_response();
    }
    if node == "local" {
        return module_rpc(State(state), Path(id), Json(body)).await;
    }
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let target = format!(
        "{}/api/v1/peer/modules/{}/rpc",
        url.trim_end_matches('/'),
        id
    );
    let response = state
        .client
        .post(target)
        .header("X-Orqella-Peer-Token", &peer.token)
        .json(&json!({ "method": body.method, "params": body.params }))
        .timeout(std::time::Duration::from_secs(180))
        .send()
        .await;
    match response {
        Ok(response) => {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            let mut value: Value =
                serde_json::from_str(&text).unwrap_or_else(|_| json!({ "error": text }));
            let ok = value.get("ok").and_then(Value::as_bool) == Some(true);
            if ok {
                match body.method.as_str() {
                    "app.open" | "proxy.status" => {
                        if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut)
                        {
                            let mount = body
                                .params
                                .get("mount")
                                .and_then(Value::as_str)
                                .unwrap_or_default();
                            let url = app_url_for_node(&state, mount, &node).unwrap_or_default();
                            let host = url
                                .strip_prefix("http://")
                                .and_then(|rest| rest.split(':').next())
                                .map(str::to_string);
                            result.insert("node".into(), json!(node));
                            result.insert("remote".into(), json!(true));
                            result.insert("host".into(), json!(host));
                            result.insert("url".into(), json!(url));
                        }
                    }
                    "download.prepare" => {
                        if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut)
                        {
                            let url = result
                                .get("url")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            if let Some(rest) = url.strip_prefix("/api/v1/downloads/") {
                                result.insert(
                                    "url".into(),
                                    json!(format!("/api/v1/nodes/{node}/downloads/{rest}")),
                                );
                            }
                        }
                    }
                    "media.capability" => {
                        if let Some(result) = value.get_mut("result").and_then(Value::as_object_mut)
                        {
                            let prefix = result
                                .get("prefix")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string();
                            let local = format!("/m/{id}/media/");
                            if let Some(rest) = prefix.strip_prefix(&local) {
                                result.insert(
                                    "prefix".into(),
                                    json!(format!("/n/{node}/m/{id}/media/{rest}")),
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                Json(value),
            )
                .into_response()
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("remote module request failed: {error}"),
        )
            .into_response(),
    }
}

pub(super) async fn peer_modules(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    let modules = state
        .modules
        .list()
        .into_iter()
        .map(|module| {
            let mut value = serde_json::to_value(&module).unwrap_or_else(|_| json!({}));
            value["sidecar"] = serde_json::to_value(state.sidecars.status(&module.manifest.id))
                .unwrap_or(Value::Null);
            value
        })
        .collect::<Vec<_>>();
    Json(modules).into_response()
}

pub(super) async fn peer_module_rpc(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<RpcRequest>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    state.audit(
        "peer.module.rpc",
        json!({ "module": id, "method": body.method }),
    );
    module_rpc(State(state), Path(id), Json(body)).await
}

pub(super) async fn peer_module_page(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    serve_module_file(state, id, String::new(), ModuleSurface::Page).await
}

pub(super) async fn peer_module_overview(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    serve_module_file(state, id, String::new(), ModuleSurface::Overview).await
}

pub(super) async fn peer_module_settings(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    serve_module_file(state, id, String::new(), ModuleSurface::Settings).await
}

pub(super) async fn peer_module_asset(
    State(state): State<Arc<AppState>>,
    Path((id, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    serve_module_file(state, id, path, ModuleSurface::Page).await
}

pub(super) async fn remote_module_index(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    remote_module_file(state, node, id, ModuleSurface::Page, None).await
}

pub(super) async fn remote_module_overview(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    remote_module_file(state, node, id, ModuleSurface::Overview, None).await
}

pub(super) async fn remote_module_settings(
    State(state): State<Arc<AppState>>,
    Path((node, id)): Path<(String, String)>,
) -> Response {
    remote_module_file(state, node, id, ModuleSurface::Settings, None).await
}

pub(super) async fn remote_module_asset(
    State(state): State<Arc<AppState>>,
    Path((node, id, path)): Path<(String, String, String)>,
) -> Response {
    remote_module_file(state, node, id, ModuleSurface::Page, Some(path)).await
}

fn module_csp_with_sandbox(peer: Option<&HeaderValue>) -> HeaderValue {
    const DEFAULT: &str = "default-src 'self' data: blob:; script-src 'unsafe-inline' 'unsafe-eval' *; style-src 'unsafe-inline' *; img-src * data:; connect-src 'self' ws: wss:; frame-src 'self'";
    let peer = peer
        .and_then(|value| value.to_str().ok())
        .unwrap_or(DEFAULT);
    let directives = peer
        .split(';')
        .map(str::trim)
        .filter(|directive| {
            !directive.is_empty()
                && !directive.to_ascii_lowercase().starts_with("sandbox")
                && !directive
                    .to_ascii_lowercase()
                    .starts_with("frame-ancestors")
        })
        .collect::<Vec<_>>();
    // Module documents can be nested inside another sandboxed module surface
    // (for example the VM Lab VNC console). That parent has an opaque origin,
    // so `frame-ancestors 'self'` would block the nested document. The shell
    // still owns top-level framing; module scripts remain sandboxed and cannot
    // read the parent origin.
    let policy = format!(
        "sandbox allow-scripts allow-forms allow-popups; {}",
        directives.join("; ")
    );
    HeaderValue::from_str(&policy).unwrap_or_else(|_| {
        HeaderValue::from_static("sandbox allow-scripts allow-forms allow-popups")
    })
}

pub(super) async fn remote_module_file(
    state: Arc<AppState>,
    node: String,
    id: String,
    surface: ModuleSurface,
    asset: Option<String>,
) -> Response {
    if util::validate_module_id(&id).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid module id").into_response();
    }
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let target = if let Some(path) = asset {
        let Some(path) = proxy::safe_rest(&path) else {
            return (StatusCode::BAD_REQUEST, "invalid module asset path").into_response();
        };
        format!(
            "{}/api/v1/peer/modules/{}/asset/{}",
            url.trim_end_matches('/'),
            id,
            path
        )
    } else {
        let suffix = match surface {
            ModuleSurface::Page => "",
            ModuleSurface::Overview => "/overview",
            ModuleSurface::Settings => "/settings",
        };
        format!(
            "{}/api/v1/peer/modules/{}{}",
            url.trim_end_matches('/'),
            id,
            suffix
        )
    };
    let response = state
        .client
        .get(target)
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await;
    let response = match response {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("remote module request failed: {error}"),
            )
                .into_response();
        }
    };
    let status = response.status();
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/octet-stream")
        .to_string();
    let content_security_policy = response
        .headers()
        .get(header::CONTENT_SECURITY_POLICY)
        .cloned();
    let content_type_options = response
        .headers()
        .get(header::X_CONTENT_TYPE_OPTIONS)
        .cloned();
    let bytes = match response.bytes().await {
        Ok(bytes) => bytes,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("remote module response failed: {error}"),
            )
                .into_response();
        }
    };
    let bytes = if content_type.starts_with("text/html") {
        let html = String::from_utf8_lossy(&bytes);
        let peer_base = format!("href=\"/m/{id}/");
        let local_base = format!("href=\"/n/{node}/m/{id}/");
        let mut html = html.replace(&peer_base, &local_base);
        // Use this daemon's SDK for remote pages too. Besides keeping the
        // frame nonce contract consistent across a rolling upgrade, it avoids
        // trusting a peer's older (or modified) SDK for the local frame.
        let sdk = include_str!("../../web/sdk.js");
        let inject = format!("<script>{sdk}</script>");
        html = if let Some(head) = html.find("<head>") {
            let insert = head + "<head>".len();
            format!("{}{inject}{}", &html[..insert], &html[insert..])
        } else {
            format!("{inject}{html}")
        };
        html.into_bytes()
    } else {
        bytes.to_vec()
    };
    let mut headers = HeaderMap::new();
    if let Ok(value) = HeaderValue::from_str(&content_type) {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        module_csp_with_sandbox(content_security_policy.as_ref()),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        content_type_options.unwrap_or_else(|| HeaderValue::from_static("nosniff")),
    );
    (
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
        headers,
        bytes,
    )
        .into_response()
}

pub(super) fn ws_query(uri: &Uri) -> String {
    uri.query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default()
}

pub(super) fn ws_token(uri: &Uri) -> Option<String> {
    url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes())
        .find(|(key, _)| key == "__orqella_ws")
        .map(|(_, value)| value.into_owned())
}

pub(super) fn ws_token_authorized(state: &AppState, scope: &str, uri: &Uri) -> bool {
    let expected = state.ws_token(scope);
    ws_token(uri).is_some_and(|supplied| constant_time_eq(supplied.as_bytes(), expected.as_bytes()))
}

pub(super) async fn connect_sidecar_websocket(
    state: &AppState,
    id: &str,
    path: &str,
    query: &str,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let (port, token) = state.sidecars.websocket_target(id)?;
    let target = format!(
        "ws://127.0.0.1:{port}/ws/{}{query}",
        path.trim_start_matches('/')
    );
    let mut request = target
        .as_str()
        .into_client_request()
        .context("invalid sidecar websocket request")?;
    request.headers_mut().insert(
        "X-Orqella-Module-Token",
        HeaderValue::from_str(&token).context("invalid sidecar token")?,
    );
    let (stream, _) = tokio_tungstenite::connect_async(request)
        .await
        .context("sidecar websocket connection failed")?;
    Ok(stream)
}

pub(super) async fn module_ws(
    State(state): State<Arc<AppState>>,
    Path((id, path)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    ws: WebSocketUpgrade,
) -> Response {
    let Some(module) = state.modules.get(&id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    if !module.state.enabled || !module.grants("backend") || !state.sidecars.is_running(&id) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "module backend is not running or is not permitted",
        )
            .into_response();
    }
    if !ws_token_authorized(&state, &id, &uri) {
        return (StatusCode::FORBIDDEN, "invalid module WebSocket token").into_response();
    }
    let query = ws_query(&uri);
    ws.on_upgrade(move |socket| async move {
        if let Ok(upstream) = connect_sidecar_websocket(&state, &id, &path, &query).await {
            proxy::bridge(socket, upstream).await;
        }
    })
    .into_response()
}

/// Same-origin streaming proxy for a module's own sidecar. Authorized by the
/// short-lived capability minted by `media.capability`; the daemon injects the
/// sidecar token server-side, so the page never sees it. Forwards Range/status
/// and streams the body, which lets `<video>` (and native iOS HLS) play straight
/// from the sidecar through the daemon.
pub(super) async fn module_media(
    State(state): State<Arc<AppState>>,
    Path((id, token, path)): Path<(String, String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    serve_module_media(&state, &id, &token, &path, &uri, &headers).await
}

pub(super) async fn peer_module_media(
    State(state): State<Arc<AppState>>,
    Path((id, token, path)): Path<(String, String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    serve_module_media(&state, &id, &token, &path, &uri, &headers).await
}

pub(super) async fn remote_module_media(
    State(state): State<Arc<AppState>>,
    Path((node, id, token, path)): Path<(String, String, String, String)>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    if util::validate_module_id(&id).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid module id").into_response();
    }
    if node == "local" {
        return serve_module_media(&state, &id, &token, &path, &uri, &headers).await;
    }
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(peer_url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let Some(rest) = proxy::safe_rest(&path) else {
        return (StatusCode::BAD_REQUEST, "invalid media path").into_response();
    };
    let encoded = rest
        .split('/')
        .map(crate::util::percent_encode)
        .collect::<Vec<_>>()
        .join("/");
    let query = uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let target = format!(
        "{}/api/v1/peer/modules/{id}/media/{token}/{encoded}{query}",
        peer_url.trim_end_matches('/')
    );
    let mut request = state
        .client
        .get(target)
        .header("X-Orqella-Peer-Token", &peer.token);
    for name in [header::RANGE, header::IF_RANGE] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value);
        }
    }
    let upstream = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("could not reach {}: {error}", peer.name),
            )
                .into_response();
        }
    };
    let status = upstream.status();
    if !status.is_success() {
        let text = upstream.text().await.unwrap_or_default();
        return (status, text).into_response();
    }
    let mut response = Response::builder().status(status.as_u16());
    for (name, value) in upstream.headers() {
        if matches!(
            name.as_str().to_ascii_lowercase().as_str(),
            "connection" | "transfer-encoding" | "set-cookie" | "content-encoding"
        ) {
            continue;
        }
        response = response.header(name, value);
    }
    let stream = upstream
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| {
            (StatusCode::BAD_GATEWAY, "could not build media response").into_response()
        })
}

async fn serve_module_media(
    state: &Arc<AppState>,
    id: &str,
    token: &str,
    path: &str,
    uri: &Uri,
    headers: &HeaderMap,
) -> Response {
    if util::validate_module_id(id).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid module id").into_response();
    }
    let Some(module) = state.modules.get(id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    if !module.state.enabled {
        return (StatusCode::SERVICE_UNAVAILABLE, "module is disabled").into_response();
    }
    if !module.grants("backend") || !state.sidecars.is_running(id) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "module backend is not running or is not permitted",
        )
            .into_response();
    }
    if !crate::media::verify(token, id) {
        return (StatusCode::FORBIDDEN, "invalid or expired media capability").into_response();
    }
    let Some(rest) = crate::proxy::safe_rest(path) else {
        return (StatusCode::BAD_REQUEST, "invalid media path").into_response();
    };
    if rest.is_empty() {
        return (StatusCode::NOT_FOUND, "media path is empty").into_response();
    }
    let query = uri
        .query()
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    let target = format!("/{rest}{query}");
    super::downloads::stream_sidecar_path(state, id, &target, headers, "private, no-store").await
}

pub(super) async fn remote_module_ws(
    State(state): State<Arc<AppState>>,
    Path((node, id, path)): Path<(String, String, String)>,
    OriginalUri(uri): OriginalUri,
    ws: WebSocketUpgrade,
) -> Response {
    if util::validate_module_id(&id).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid module id").into_response();
    }
    let Some(path) = proxy::safe_rest(&path) else {
        return (StatusCode::BAD_REQUEST, "invalid module websocket path").into_response();
    };
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(peer_url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    if !ws_token_authorized(&state, &format!("{node}:{id}"), &uri) {
        return (
            StatusCode::FORBIDDEN,
            "invalid remote module WebSocket token",
        )
            .into_response();
    }
    let query = ws_query(&uri);
    let ws_base = peer_url
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    let target = format!(
        "{}/api/v1/peer/modules/{}/ws/{}{query}",
        ws_base.trim_end_matches('/'),
        id,
        path.trim_start_matches('/')
    );
    let token = peer.token.clone();
    ws.on_upgrade(move |socket| async move {
        let Ok(mut request) = target.as_str().into_client_request() else {
            return;
        };
        if let Ok(value) = HeaderValue::from_str(&token) {
            request.headers_mut().insert("X-Orqella-Peer-Token", value);
        }
        if let Ok((upstream, _)) = tokio_tungstenite::connect_async(request).await {
            proxy::bridge(socket, upstream).await;
        }
    })
    .into_response()
}

pub(super) async fn peer_module_ws(
    State(state): State<Arc<AppState>>,
    Path((id, path)): Path<(String, String)>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    ws: WebSocketUpgrade,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    let Some(module) = state.modules.get(&id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    if !module.state.enabled || !module.grants("backend") || !state.sidecars.is_running(&id) {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "module backend is not running or is not permitted",
        )
            .into_response();
    }
    let query = ws_query(&uri);
    ws.on_upgrade(move |socket| async move {
        if let Ok(upstream) = connect_sidecar_websocket(&state, &id, &path, &query).await {
            proxy::bridge(socket, upstream).await;
        }
    })
    .into_response()
}
