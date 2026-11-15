use super::*;

pub(super) async fn shell(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !state.config().server.dashboard {
        return Json(json!({
            "name": "Orqella",
            "mode": "node-only",
            "message": "Dashboard serving is disabled on this machine.",
            "api": "/api/v1/info",
        }))
        .into_response();
    }
    match WebAssets::get("index.html") {
        Some(asset) => {
            let mut response =
                Html(String::from_utf8_lossy(&asset.data).to_string()).into_response();
            if document_navigation(&headers) {
                let session = state.issue_session();
                let secure = request_is_https(&headers);
                let cookie = format!(
                    "orqella_session={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400{}",
                    if secure { "; Secure" } else { "" }
                );
                if let Ok(value) = HeaderValue::from_str(&cookie) {
                    response.headers_mut().insert(header::SET_COOKIE, value);
                }
            }
            response.headers_mut().insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; frame-src 'self' http://*.orqella.localhost:*; base-uri 'none'; form-action 'self'; frame-ancestors 'self'",
                ),
            );
            response.headers_mut().insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            response.headers_mut().insert(
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            );
            response
        }
        None => (StatusCode::INTERNAL_SERVER_ERROR, "shell missing").into_response(),
    }
}

pub(super) async fn theme_css(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config();
    let selected = theme::normalize(&config.theme);
    let mut css = WebAssets::get(&theme::asset_path(selected))
        .or_else(|| WebAssets::get("theme.css"))
        .map(|asset| String::from_utf8_lossy(&asset.data).to_string())
        .unwrap_or_default();
    let custom = state.paths.config_dir.join("theme.css");
    if let Ok(extra) = std::fs::read_to_string(custom) {
        css.push_str("\n/* user theme */\n");
        css.push_str(&extra);
    }
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        css,
    )
        .into_response()
}

pub(super) async fn asset(Path(path): Path<String>) -> Response {
    let Some(asset) = WebAssets::get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mime = mime_guess::from_path(&path).first_or_octet_stream();
    ([(header::CONTENT_TYPE, mime.as_ref())], asset.data.to_vec()).into_response()
}

pub(super) async fn favicon() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

pub(super) async fn sdk() -> Response {
    match WebAssets::get("sdk.js") {
        Some(asset) => (
            [
                (header::CONTENT_TYPE, "application/javascript"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            asset.data.to_vec(),
        )
            .into_response(),
        None => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub(super) async fn health(State(state): State<Arc<AppState>>) -> Response {
    Json(json!({
        "ok": true,
        "name": "Orqella",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("ORQELLA_BUILD_COMMIT").unwrap_or("unknown"),
        "uptime_secs": state.started.elapsed().as_secs(),
        "dashboard": state.config().server.dashboard,
    }))
    .into_response()
}

pub(super) async fn info(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config();
    Json(json!({
        "name": "Orqella",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": option_env!("ORQELLA_BUILD_COMMIT").unwrap_or("unknown"),
        "api_version": "1",
        "node": {
            "id": "local",
            "name": local_name(),
            "dashboard": config.server.dashboard,
            "origin": config.origin(),
        },
        "capabilities": ["system", "modules", "proxy", "peers", "agent", "hooks"],
        "onboarding": config.onboarding,
        "themes": theme::THEMES,
        "theme_path": state.paths.config_dir.join("theme.css").display().to_string(),
        "config": config.config_summary(),
    }))
    .into_response()
}

pub(super) async fn system(State(state): State<Arc<AppState>>) -> Response {
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

pub(super) async fn config_view(State(state): State<Arc<AppState>>) -> Response {
    Json(state.config().config_summary()).into_response()
}
