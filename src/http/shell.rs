use super::*;

const SHELL_CSP_FALLBACK: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; frame-src 'self' http://*.orqella.localhost:*; base-uri 'none'; form-action 'self'; frame-ancestors 'self'";

/// The shell may be reached through the desktop gateway or a mobile tunnel on
/// a different loopback port. Its app iframes use isolated 127.0.0.x origins
/// on that same port, so the CSP has to enumerate that small range.
fn shell_csp(port: u16) -> String {
    let mut frame_src = String::from(
        "'self' http://*.orqella.localhost:* http://*.modules.orqella.localhost:* \
         https://*.modules.orqella.io https://*.apps.orqella.io",
    );
    if cfg!(debug_assertions) {
        // Staging module/app origins are development-only. Release builds do
        // not need to allow a staging page to frame the production shell.
        frame_src.push_str(
            " https://*.modules.staging.orqella.io \
             https://*.apps.staging.orqella.io",
        );
    }
    for last in 2..=127u8 {
        frame_src.push_str(&format!(" http://127.0.0.{last}:{port}"));
    }
    format!(
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' ws: wss:; frame-src {frame_src}; base-uri 'none'; form-action 'self'; frame-ancestors 'self'"
    )
}

fn request_port(headers: &HeaderMap, fallback: u16) -> u16 {
    headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .and_then(|host| {
            let host = host.trim();
            if let Some(rest) = host.strip_prefix('[') {
                rest.split_once("]:")
                    .and_then(|(_, port)| port.parse().ok())
            } else {
                host.rsplit_once(':')
                    .and_then(|(_, port)| port.parse().ok())
            }
        })
        .unwrap_or(fallback)
}

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
    let mut response = match WebAssets::get("index.html") {
        Some(asset) => Html(String::from_utf8_lossy(&asset.data).to_string()).into_response(),
        None => return (StatusCode::INTERNAL_SERVER_ERROR, "shell missing").into_response(),
    };
    // A direct browser visit must remain usable without a local CLI. Mint a
    // read-only session on document navigation; privileged actions still
    // require the bootstrap capability or an explicit Link unlock.
    if super::api_auth_level(&state, &headers) == super::AuthLevel::None {
        let session = state.issue_session(false);
        let secure = request_is_https(&headers);
        let cookie = format!(
            "orqella_session={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400{}",
            if secure { "; Secure" } else { "" }
        );
        if let Ok(value) = HeaderValue::from_str(&cookie) {
            response.headers_mut().insert(header::SET_COOKIE, value);
        }
    }
    let shell_port = request_port(&headers, state.config().server.port);
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(&shell_csp(shell_port))
            .unwrap_or_else(|_| HeaderValue::from_static(SHELL_CSP_FALLBACK)),
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
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// Exchange the local bootstrap capability for a privileged browser session.
/// The capability is delivered in the URL fragment by `orqella open` and is
/// never accepted from a plain document navigation.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_csp_allows_mobile_app_origins_on_the_tunnel_port() {
        let csp = shell_csp(19477);
        assert!(csp.contains("http://127.0.0.2:19477"));
        assert!(csp.contains("http://127.0.0.127:19477"));
        assert!(!csp.contains("http://127.0.0.128:19477"));
        assert!(csp.contains("http://*.modules.orqella.localhost:*"));
        assert!(csp.contains("https://*.modules.orqella.io"));
        assert!(csp.contains("https://*.modules.staging.orqella.io"));
        assert!(csp.contains("https://*.apps.orqella.io"));
        assert!(csp.contains("https://*.apps.staging.orqella.io"));
        assert!(csp.len() < 8 * 1024, "CSP is too large: {}", csp.len());
    }

    #[test]
    fn request_port_handles_ipv4_and_ipv6_hosts() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:19477"));
        assert_eq!(request_port(&headers, 9477), 19477);
        headers.insert(header::HOST, HeaderValue::from_static("[::1]:19478"));
        assert_eq!(request_port(&headers, 9477), 19478);
        headers.insert(header::HOST, HeaderValue::from_static("orqella.localhost"));
        assert_eq!(request_port(&headers, 9477), 9477);
    }
}

pub(super) async fn bootstrap(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    let supplied = headers
        .get("x-orqella-bootstrap")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if supplied.is_empty()
        || !constant_time_eq(supplied.as_bytes(), state.bootstrap_token.as_bytes())
    {
        return (
            StatusCode::UNAUTHORIZED,
            "invalid local bootstrap capability",
        )
            .into_response();
    }
    let session = state.issue_session(true);
    let secure = request_is_https(&headers);
    let cookie = format!(
        "orqella_session={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400{}",
        if secure { "; Secure" } else { "" }
    );
    let mut response = Json(json!({ "ok": true, "privileged": true })).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[derive(Deserialize)]
pub(super) struct LinkSessionRequest {
    code: String,
}

/// Exchange a one-time Link handoff code for a privileged local shell session.
/// The code is bound to the account and consumed with this hub's device token.
pub(super) async fn link_session(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(body): Json<LinkSessionRequest>,
) -> Response {
    if headers
        .get("x-orqella")
        .and_then(|value| value.to_str().ok())
        != Some("1")
    {
        return (StatusCode::FORBIDDEN, "missing X-Orqella header").into_response();
    }
    let code = body.code.trim();
    if code.is_empty() {
        return (StatusCode::BAD_REQUEST, "a Link handoff code is required").into_response();
    }
    if state.link.read().expect("link lock").is_none() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "this machine is not connected to Orqella Link; sign in from Settings → General first",
        )
            .into_response();
    }
    let link = state.config().link.clone();
    let hub_signed_in = link
        .token
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if !hub_signed_in {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "this machine is not signed in to Orqella Link",
        )
            .into_response();
    }
    let account = match AccountClient::production() {
        Ok(account) => account,
        Err(error) => return (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    };
    let device_token = link.token.as_deref().unwrap_or("").trim();
    if device_token.is_empty() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "this machine is not signed in to Link",
        )
            .into_response();
    }
    let browser_user = match account.exchange_web_handoff(device_token, code).await {
        Ok(user) => user,
        Err(error) => {
            tracing::debug!(%error, "link handoff exchange rejected");
            return match crate::state::link_error_status(&error) {
                Some(StatusCode::FORBIDDEN) => (
                    StatusCode::FORBIDDEN,
                    "this Link account does not own this machine",
                )
                    .into_response(),
                Some(StatusCode::BAD_REQUEST) | Some(StatusCode::UNAUTHORIZED) => {
                    (StatusCode::UNAUTHORIZED, "invalid or expired handoff code").into_response()
                }
                _ => (
                    StatusCode::BAD_GATEWAY,
                    "could not reach Orqella Link; try again",
                )
                    .into_response(),
            };
        }
    };
    let session = state.issue_session(true);
    let secure = request_is_https(&headers);
    let cookie = format!(
        "orqella_session={session}; Path=/; HttpOnly; SameSite=Strict; Max-Age=86400{}",
        if secure { "; Secure" } else { "" }
    );
    let mut response = Json(json!({ "ok": true, "privileged": true })).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    state.audit("session.link_unlock", json!({ "user": browser_user.id }));
    response
}

pub(super) async fn session_status(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    let level = super::api_auth_level(&state, &headers);
    Json(json!({
        "authenticated": level != super::AuthLevel::None,
        "privileged": level == super::AuthLevel::Privileged,
    }))
    .into_response()
}

pub(super) async fn theme_css(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config();
    let mut css = theme::custom_css(&state.paths.config_dir, &config.theme)
        .or_else(|| {
            let selected = theme::normalize(&config.theme);
            WebAssets::get(&theme::asset_path(selected))
                .or_else(|| WebAssets::get("theme.css"))
                .map(|asset| String::from_utf8_lossy(&asset.data).to_string())
        })
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
    (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        asset.data.to_vec(),
    )
        .into_response()
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

/// The module bootstrap is served on every per-module origin. It is deliberately
/// generic and carries no module content or credentials.
const MODULE_ORIGIN_CSP_PRODUCTION: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' blob: data:; connect-src 'self'; worker-src 'self' blob:; frame-src 'self'; base-uri 'self'; form-action 'self'; frame-ancestors 'self' http://orqella.localhost:* http://*.orqella.localhost:* http://127.0.0.1:* http://localhost:* https://orqella-web.fly.dev";
const MODULE_ORIGIN_CSP_DEVELOPMENT: &str = "default-src 'self'; script-src 'self' 'unsafe-inline' 'unsafe-eval' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' blob: data:; connect-src 'self'; worker-src 'self' blob:; frame-src 'self'; base-uri 'self'; form-action 'self'; frame-ancestors 'self' http://orqella.localhost:* http://*.orqella.localhost:* http://127.0.0.1:* http://localhost:* https://orqella-web.fly.dev https://orqella-web-staging.fly.dev";

fn module_origin_csp() -> &'static str {
    if cfg!(debug_assertions) {
        MODULE_ORIGIN_CSP_DEVELOPMENT
    } else {
        MODULE_ORIGIN_CSP_PRODUCTION
    }
}

fn module_origin_asset(body: &'static str, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_SECURITY_POLICY, module_origin_csp()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        body,
    )
        .into_response()
}

pub(super) async fn module_bootstrap() -> Response {
    module_origin_asset(
        include_str!("../../web-client/module-bootstrap.html"),
        "text/html; charset=utf-8",
    )
}

pub(super) async fn module_bootstrap_js() -> Response {
    module_origin_asset(
        include_str!("../../web-client/module-bootstrap.js"),
        "application/javascript; charset=utf-8",
    )
}

pub(super) async fn module_sw() -> Response {
    module_origin_asset(
        include_str!("../../web-client/module-sw.js"),
        "application/javascript; charset=utf-8",
    )
}

pub(super) async fn module_bridge() -> Response {
    module_origin_asset(
        include_str!("../../web-client/module-bridge.js"),
        "application/javascript; charset=utf-8",
    )
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
        "themes": theme::catalog(&state.paths.config_dir),
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
