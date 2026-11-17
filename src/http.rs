use crate::config::{AgentConfig, AgentViewerConfig, Config, Paths};
use crate::link::AccountClient;
use crate::metrics::{self};
use crate::modules::manifest::BackendKind;
use crate::peers;
use crate::proxy;
use crate::supervisors;
use crate::theme;
use crate::update;
use crate::util;
use anyhow::{Context, Result, bail};
use axum::body::{Body, Bytes};
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{DefaultBodyLimit, FromRequestParts, OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{any, get, post, put};
use axum::{Json, Router};
use base64::Engine as _;
use futures_util::{StreamExt, stream};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::future::IntoFuture;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tower::{ServiceExt, make::Shared};

#[derive(RustEmbed)]
#[folder = "web"]
struct WebAssets;

mod build;
mod dashboards;
mod module_pages;
mod modules;
mod paths;
mod peer_routes;

use peer_routes::{constant_time_eq, peer_authorized};
mod proxies;
mod rpc;
mod settings;
mod shell;
mod updates;

pub use crate::state::AppState;
use crate::state::{restart_link, stop_link};

pub async fn serve(
    mut config: Config,
    paths: Paths,
    port_override: Option<u16>,
    no_dashboard: bool,
) -> Result<()> {
    if no_dashboard {
        config.server.dashboard = false;
    }
    if let Some(port) = port_override {
        config.server.port = port;
    }
    let state = AppState::new(paths.clone(), config.clone()).await?;
    supervisors::refresh_all_peers(Arc::clone(&state)).await;
    tokio::spawn(supervisors::peer_refresh_loop(Arc::clone(&state)));

    let app = router(Arc::clone(&state));
    let trusted_app = app.clone();
    let trusted_token = state.bootstrap_token.clone();
    #[cfg(not(unix))]
    let _ = (&trusted_app, &trusted_token);
    let host_state = Arc::clone(&state);
    let service = tower::service_fn(move |mut request: Request<Body>| {
        let app = app.clone();
        let host_state = Arc::clone(&host_state);
        async move {
            let config = host_state.config();
            if let Some((mount, node)) = app_route_for_host(request.headers(), &config) {
                let path = request.uri().path().to_string();
                let query = request
                    .uri()
                    .query()
                    .map(|q| format!("?{q}"))
                    .unwrap_or_default();
                let rewritten = match node {
                    Some(node) => format!("/n/{node}/p/{mount}{path}{query}"),
                    None => format!("/p/{mount}{path}{query}"),
                };
                if let Ok(uri) = rewritten.parse::<axum::http::Uri>() {
                    *request.uri_mut() = uri;
                }
            }
            Ok::<_, std::convert::Infallible>(app.oneshot(request).await.unwrap_or_else(|_| {
                (StatusCode::INTERNAL_SERVER_ERROR, "router failed").into_response()
            }))
        }
    });
    let addr: SocketAddr = format!("{}:{}", config.server.bind, config.server.port)
        .parse()
        .context("invalid bind address")?;
    if !addr.ip().is_loopback() && !config.server.allow_remote {
        bail!(
            "refusing to bind {addr}: Orqella's API has no remote authentication; \
             set [server] allow_remote = true only behind a trusted network/tunnel"
        );
    }
    if config.server.allow_remote {
        tracing::warn!(
            %addr,
            "allow_remote is enabled; the local API is reachable beyond loopback and has no remote authentication"
        );
    }
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("could not bind {addr}; is another Orqella already running?"))?;
    let local = listener.local_addr()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let socket_path = state.paths.runtime_dir.join("orqella.sock");
        let _ = std::fs::remove_file(&socket_path);
        match tokio::net::UnixListener::bind(&socket_path) {
            Ok(unix) => {
                let _ =
                    std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600));
                let trusted = tower::service_fn(move |mut request: Request<Body>| {
                    let app = trusted_app.clone();
                    let token = trusted_token.clone();
                    async move {
                        if let Ok(value) = HeaderValue::from_str(&token) {
                            request.headers_mut().insert("x-orqella-bootstrap", value);
                        }
                        Ok::<_, std::convert::Infallible>(
                            app.oneshot(request).await.unwrap_or_else(|_| {
                                (StatusCode::INTERNAL_SERVER_ERROR, "router failed").into_response()
                            }),
                        )
                    }
                });
                tokio::spawn(async move {
                    if let Err(error) = axum::serve(unix, Shared::new(trusted)).await {
                        tracing::warn!(%error, "trusted local socket listener stopped");
                    }
                });
            }
            Err(error) => {
                tracing::warn!(
                    path = %socket_path.display(),
                    %error,
                    "could not start the trusted local socket"
                );
            }
        }
    }
    tokio::spawn(supervisors::sidecar_supervisor(Arc::clone(&state)));
    let canonical = format!("http://{}:{}/", config.canonical_host(), local.port());
    tracing::info!(%local, %canonical, "Orqella is listening");
    println!("\n  Orqella  {canonical}");
    #[cfg(target_os = "macos")]
    if !crate::hosts::missing(&config).is_empty() {
        println!("  loopback {}", config.loopback_origin());
        println!("  safari   optional host entries: `orqella hosts install`");
    }
    println!("  data     {}\n", paths.data_dir.display());

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let server = axum::serve(listener, Shared::new(service))
        .with_graceful_shutdown(async move {
            let mut shutdown_rx = shutdown_rx;
            let _ = shutdown_rx.changed().await;
        })
        .into_future();
    tokio::pin!(server);
    let result = tokio::select! {
        result = server.as_mut() => result.context("HTTP server stopped unexpectedly"),
        _ = shutdown_signal() => {
            let _ = shutdown_tx.send(true);
            match tokio::time::timeout(std::time::Duration::from_secs(10), server.as_mut()).await {
                Ok(result) => result.context("HTTP server stopped during shutdown"),
                Err(_) => {
                    tracing::warn!("graceful shutdown timed out; closing remaining connections");
                    Ok(())
                }
            }
        }
    };
    state.builder.shutdown();
    state.sidecars.stop_all();
    if let Some(web) = state.web.read().expect("web bridge lock").clone() {
        web.stop();
    }
    result
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(shell::shell))
        .route("/__orqella/bootstrap", post(shell::bootstrap))
        .route("/__orqella/link-session", post(shell::link_session))
        .route("/__orqella/session", get(shell::session_status))
        .route("/assets/theme.css", get(shell::theme_css))
        .route("/assets/{*path}", get(shell::asset))
        .route("/sdk/orqella.js", get(shell::sdk))
        .route("/favicon.ico", get(shell::favicon))
        .route("/healthz", get(shell::health))
        .route("/api/v1/info", get(shell::info))
        .route(
            "/api/v1/dashboards",
            get(dashboards::list_dashboards).put(dashboards::save_dashboards),
        )
        .route(
            "/api/v1/dashboards/{id}/layout/{profile}",
            put(dashboards::save_layout),
        )
        .route("/api/v1/system", get(shell::system))
        .route("/api/v1/nodes", get(nodes))
        .route("/api/v1/fs/dirs", get(paths::list_dirs))
        .route("/api/v1/nodes/{node}/fs/dirs", get(paths::node_list_dirs))
        .route("/api/v1/peer/fs/dirs", get(paths::peer_list_dirs))
        .route("/api/v1/nodes/{node}/wake", post(wake_node))
        .route(
            "/api/v1/nodes/{node}/modules",
            get(module_pages::node_modules),
        )
        .route(
            "/api/v1/nodes/{node}/modules/{id}/rpc",
            post(module_pages::node_module_rpc),
        )
        .route("/api/v1/services", get(services))
        .route("/api/v1/peers", get(list_peers))
        .route(
            "/api/v1/peers/{id}/modules/{module}/sync",
            post(modules::sync_module),
        )
        .route("/api/v1/modules", get(modules::list_modules))
        .route(
            "/api/v1/modules/{id}",
            get(modules::module_detail).delete(modules::remove_module),
        )
        .route("/api/v1/modules/{id}/enable", post(modules::enable_module))
        .route(
            "/api/v1/modules/{id}/disable",
            post(modules::disable_module),
        )
        .route("/api/v1/modules/{id}/rpc", post(rpc::module_rpc))
        .route(
            "/api/v1/modules/{id}/permissions",
            post(modules::set_module_permissions),
        )
        .route(
            "/api/v1/modules/{id}/config",
            post(modules::set_module_config),
        )
        .route("/api/v1/modules/{id}/log", get(modules::module_log))
        .route(
            "/api/v1/modules/{id}/sidecar/restart",
            post(modules::restart_sidecar),
        )
        .route(
            "/api/v1/modules/{id}/update",
            post(modules::update_module_git),
        )
        .route(
            "/api/v1/modules/{id}/checkpoint",
            post(modules::checkpoint_module),
        )
        .route("/api/v1/modules/install", post(modules::install_module))
        .route("/api/v1/modules/import", post(modules::import_module))
        .route("/api/v1/modules/{id}/export", get(modules::export_module))
        .route(
            "/api/v1/build",
            get(build::list_builds).post(build::start_build),
        )
        .route("/api/v1/build/{id}", get(build::get_build))
        .route("/api/v1/build/{id}/open", get(build::open_build_session))
        .route(
            "/api/v1/nodes/{node}/build",
            get(build::node_list_builds).post(build::node_start_build),
        )
        .route(
            "/api/v1/nodes/{node}/build/{id}/open",
            get(build::node_open_build),
        )
        .route("/api/v1/update/check", get(updates::update_check))
        .route(
            "/api/v1/update/check/{node}",
            get(updates::update_check_node),
        )
        .route("/api/v1/update/apply", post(updates::update_apply))
        .route(
            "/api/v1/update/apply/{node}",
            post(updates::update_apply_node),
        )
        .route("/api/v1/update/status", get(updates::update_status))
        .route(
            "/api/v1/update/peer-status",
            get(updates::update_peer_status),
        )
        .route("/api/v1/proxy/{mount}", get(proxies::proxy_status))
        .route("/api/v1/peer/ping", get(peer_routes::peer_ping))
        .route("/api/v1/peer/info", get(peer_routes::peer_info))
        .route("/api/v1/peer/wake", get(peer_wake))
        .route("/api/v1/peer/system", get(peer_routes::peer_system))
        .route("/api/v1/peer/update/check", get(updates::peer_update_check))
        .route(
            "/api/v1/peer/update/apply",
            post(updates::peer_update_apply),
        )
        .route(
            "/api/v1/peer/update/status",
            get(updates::peer_update_status),
        )
        .route("/api/v1/peer/modules", get(module_pages::peer_modules))
        .route(
            "/api/v1/peer/modules/import",
            post(modules::peer_import_module),
        )
        .route(
            "/api/v1/peer/build",
            get(build::peer_list_builds).post(build::peer_start_build),
        )
        .route("/api/v1/peer/build/{id}/open", get(build::peer_open_build))
        .route(
            "/api/v1/peer/modules/{id}/rpc",
            post(module_pages::peer_module_rpc),
        )
        .route(
            "/api/v1/peer/modules/{id}",
            get(module_pages::peer_module_page),
        )
        .route(
            "/api/v1/peer/modules/{id}/overview",
            get(module_pages::peer_module_overview),
        )
        .route(
            "/api/v1/peer/modules/{id}/settings",
            get(module_pages::peer_module_settings),
        )
        .route(
            "/api/v1/peer/modules/{id}/asset/{*path}",
            get(module_pages::peer_module_asset),
        )
        .route(
            "/api/v1/peer/modules/{id}/ws/{*path}",
            get(module_pages::peer_module_ws),
        )
        .route("/api/v1/peer/proxy/{mount}", any(proxies::peer_proxy_root))
        .route("/api/v1/peer/proxy/{mount}/", any(proxies::peer_proxy_root))
        .route(
            "/api/v1/peer/proxy/{mount}/{*path}",
            any(proxies::peer_proxy_path),
        )
        .route("/api/v1/config", get(shell::config_view))
        .route("/api/v1/config/audit", post(settings::set_audit))
        .route("/api/v1/config/theme", post(settings::set_theme))
        .route(
            "/api/v1/config/theme/agent",
            post(settings::start_theme_agent),
        )
        .route(
            "/api/v1/config/theme/agent/{id}",
            get(settings::theme_agent_status),
        )
        .route("/api/v1/config/agent", post(settings::set_agent))
        .route("/api/v1/link", get(settings::link_view))
        .route("/api/v1/link/login", post(settings::link_login))
        .route(
            "/api/v1/link/device/start",
            post(settings::link_device_start),
        )
        .route("/api/v1/link/device/poll", post(settings::link_device_poll))
        .route("/api/v1/link/start", post(settings::link_start))
        .route("/api/v1/link/logout", post(settings::link_logout))
        .route("/api/v1/link/peer/rotate", post(settings::rotate_peer))
        .route("/api/v1/link/devices", get(settings::link_devices))
        .route(
            "/api/v1/onboarding/complete",
            post(settings::complete_onboarding),
        )
        .route(
            "/api/v1/trusted-keys",
            get(modules::list_trusted_keys).post(modules::add_trusted_key),
        )
        .route("/api/v1/audit", get(peer_routes::audit))
        .route("/m/{id}", get(module_pages::module_index))
        .route("/m/{id}/", get(module_pages::module_index))
        .route("/m/{id}/overview", get(module_pages::module_overview))
        .route("/m/{id}/settings", get(module_pages::module_settings))
        .route("/m/{id}/ws/{*path}", get(module_pages::module_ws))
        .route("/m/{id}/{*path}", get(module_pages::module_asset))
        .route("/p/{mount}", any(proxies::proxy_redirect))
        .route("/p/{mount}/", any(proxies::proxy_root))
        .route("/p/{mount}/{*path}", any(proxy::handle))
        .route("/n/{node}/m/{id}", get(module_pages::remote_module_index))
        .route("/n/{node}/m/{id}/", get(module_pages::remote_module_index))
        .route(
            "/n/{node}/m/{id}/overview",
            get(module_pages::remote_module_overview),
        )
        .route(
            "/n/{node}/m/{id}/settings",
            get(module_pages::remote_module_settings),
        )
        .route(
            "/n/{node}/m/{id}/asset/{*path}",
            get(module_pages::remote_module_asset),
        )
        .route(
            "/n/{node}/m/{id}/ws/{*path}",
            get(module_pages::remote_module_ws),
        )
        .route(
            "/n/{node}/m/{id}/{*path}",
            get(module_pages::remote_module_asset),
        )
        .route("/n/{node}/p/{mount}", any(proxies::remote_proxy_root))
        .route("/n/{node}/p/{mount}/", any(proxies::remote_proxy_root))
        .route(
            "/n/{node}/p/{mount}/{*path}",
            any(proxies::remote_proxy_path),
        )
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            host_and_csrf,
        ))
        .with_state(state)
}

async fn host_and_csrf(
    State(state): State<Arc<AppState>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let config = state.config();
    let method = request.method().clone();
    if !host_allowed(request.headers(), &config) {
        return (StatusCode::MISDIRECTED_REQUEST, "unrecognized Host header").into_response();
    }
    let path = request.uri().path();
    if matches!(
        method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    ) && path.starts_with("/api/")
        && !path.starts_with("/api/v1/peer/")
        && request
            .headers()
            .get("x-orqella")
            .and_then(|v| v.to_str().ok())
            != Some("1")
    {
        return (StatusCode::FORBIDDEN, "missing X-Orqella header").into_response();
    }
    // These GETs perform network work or expose Link metadata; require the
    // same header as state-changing calls so an <img>/iframe cannot trigger
    // them cross-site.
    if method == Method::GET
        && (path == "/api/v1/update/check"
            || path == "/api/v1/link/devices"
            || path == "/api/v1/fs/dirs"
            || path.ends_with("/fs/dirs")
            || path.starts_with("/api/v1/proxy/"))
        && request
            .headers()
            .get("x-orqella")
            .and_then(|v| v.to_str().ok())
            != Some("1")
    {
        return (StatusCode::FORBIDDEN, "missing X-Orqella header").into_response();
    }
    if let Some(origin) = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
    {
        if module_websocket_path(path) && origin != "null" && !shell_origin(origin, &config) {
            return (StatusCode::FORBIDDEN, "origin is not allowed").into_response();
        }
        if !origin_allowed(origin, &config) && !(origin == "null" && module_sandbox_path(path)) {
            return (StatusCode::FORBIDDEN, "origin is not allowed").into_response();
        }
    }
    // Peer handlers authenticate with their own bearer token. Everything else
    // under /api requires either this browser's session cookie or the private
    // CLI admin token. X-Orqella is only CSRF protection.
    let auth = api_auth_level(&state, request.headers());
    if path.starts_with("/api/") && !path.starts_with("/api/v1/peer/") && auth == AuthLevel::None {
        return (
            StatusCode::UNAUTHORIZED,
            "a local Orqella session is required",
        )
            .into_response();
    }
    if auth != AuthLevel::Privileged && privileged_api_path(&method, path) {
        return (
            StatusCode::FORBIDDEN,
            "run `orqella open` to mint a privileged local session",
        )
            .into_response();
    }
    // Remote module pages/assets attach the peer token and must only be
    // reachable from the shell's privileged session. Local /p app surfaces
    // are allowed only on their configured app host; the shell's app frame
    // and the browser bridge use that path.
    let app_host = app_route_for_host(request.headers(), &config).is_some();
    let remote_module = path.starts_with("/n/") && path.split('/').nth(3) == Some("m");
    let local_app_path =
        path.starts_with("/p/") || (path.starts_with("/n/") && path.split('/').nth(3) == Some("p"));
    if auth != AuthLevel::Privileged && (remote_module || (local_app_path && !app_host)) {
        return (
            StatusCode::UNAUTHORIZED,
            "a privileged local session is required for this surface",
        )
            .into_response();
    }
    next.run(request).await
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AuthLevel {
    None,
    ReadOnly,
    Privileged,
}

pub(super) fn api_auth_level(state: &AppState, headers: &HeaderMap) -> AuthLevel {
    let bridge = headers
        .get("x-orqella-bridge-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if constant_time_eq(bridge.as_bytes(), state.bridge_token.as_bytes()) {
        return AuthLevel::Privileged;
    }
    let bootstrap = headers
        .get("x-orqella-bootstrap")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if constant_time_eq(bootstrap.as_bytes(), state.bootstrap_token.as_bytes()) {
        return AuthLevel::Privileged;
    }
    let admin = headers
        .get("x-orqella-admin-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if constant_time_eq(admin.as_bytes(), state.admin_token.as_bytes()) {
        return AuthLevel::Privileged;
    }
    let Some(cookie) = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
    else {
        return AuthLevel::None;
    };
    for part in cookie.split(';') {
        let Some(token) = part.trim().strip_prefix("orqella_session=") else {
            continue;
        };
        return match state.session_privileged(token) {
            Some(true) => AuthLevel::Privileged,
            Some(false) => AuthLevel::ReadOnly,
            None => AuthLevel::None,
        };
    }
    AuthLevel::None
}

fn privileged_api_path(method: &Method, path: &str) -> bool {
    if method == Method::GET
        && (path == "/api/v1/fs/dirs"
            || (path.starts_with("/api/v1/nodes/") && path.ends_with("/fs/dirs")))
    {
        return true;
    }
    if method == Method::POST {
        if matches!(
            path,
            "/api/v1/modules/install"
                | "/api/v1/modules/import"
                | "/api/v1/build"
                | "/api/v1/config/agent"
                | "/api/v1/config/audit"
                | "/api/v1/config/theme/agent"
                | "/api/v1/update/apply"
                | "/api/v1/link/login"
                | "/api/v1/link/device/start"
                | "/api/v1/link/device/poll"
                | "/api/v1/link/logout"
                | "/api/v1/link/peer/rotate"
                | "/api/v1/trusted-keys"
        ) {
            return true;
        }
        if path.starts_with("/api/v1/update/apply/")
            || path.starts_with("/api/v1/peers/")
            || (path.starts_with("/api/v1/nodes/") && path.ends_with("/build"))
        {
            return true;
        }
        if let Some(rest) = path.strip_prefix("/api/v1/modules/") {
            return rest.ends_with("/permissions")
                || rest.ends_with("/config")
                || rest.ends_with("/update")
                || rest.ends_with("/checkpoint")
                || rest.ends_with("/sidecar/restart")
                || rest.ends_with("/enable")
                || rest.ends_with("/disable")
                || rest.ends_with("/rpc");
        }
        if path.starts_with("/api/v1/nodes/")
            && path.contains("/modules/")
            && path.ends_with("/rpc")
        {
            return true;
        }
    }
    if method == Method::DELETE && path.starts_with("/api/v1/modules/") {
        return true;
    }
    if method == Method::GET && path == "/api/v1/modules" {
        return false;
    }
    if method == Method::GET && path.starts_with("/api/v1/nodes/") && path.ends_with("/modules") {
        return false;
    }
    if method == Method::GET
        && (path.starts_with("/api/v1/modules")
            || path.starts_with("/api/v1/audit")
            || path.starts_with("/api/v1/config")
            || path.starts_with("/api/v1/build")
            || (path.starts_with("/api/v1/nodes/") && path.contains("/modules")))
    {
        return true;
    }
    false
}

fn request_is_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("https"))
}

fn host_name(headers: &HeaderMap) -> Option<&str> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())?
        .trim()
        .trim_end_matches('.');
    if let Some(rest) = host.strip_prefix('[') {
        let (addr, after) = rest.split_once(']')?;
        if !after.is_empty()
            && !after
                .strip_prefix(':')
                .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        {
            return None;
        }
        return Some(addr);
    }
    Some(host.split(':').next().unwrap_or(host))
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn host_allowed(headers: &HeaderMap, config: &Config) -> bool {
    let Some(host) = host_name(headers) else {
        return false;
    };
    if is_loopback_host(host) {
        return true;
    }
    if host.eq_ignore_ascii_case(config.canonical_host()) {
        return true;
    }
    app_route_for_host(headers, config).is_some()
}

fn valid_host_label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn app_route_for_host(headers: &HeaderMap, config: &Config) -> Option<(String, Option<String>)> {
    let host = host_name(headers)?;
    if let Some(mount) = config.proxy.iter().find(|mount| {
        mount
            .host
            .as_deref()
            .is_some_and(|configured| configured.trim_end_matches('.').eq_ignore_ascii_case(host))
    }) {
        return Some((mount.id.clone(), None));
    }
    let suffix = format!(".{}", config.canonical_host());
    let prefix = host.len().checked_sub(suffix.len()).and_then(|idx| {
        host[idx..]
            .eq_ignore_ascii_case(&suffix)
            .then_some(&host[..idx])
    })?;
    let (mount, node) = prefix.split_once('.')?;
    if !valid_host_label(mount) || !valid_host_label(node) {
        return None;
    }
    if !config.peers.iter().any(|peer| peer.id == node) {
        return None;
    }
    Some((mount.to_string(), Some(node.to_string())))
}

/// Sandboxed module frames have an opaque origin and send `Origin: null`.
/// Their module scripts, imports, assets, and WebSockets all use the module
/// paths, so accept that origin only there. The module backend still
/// authenticates the daemon's module token; a module that needs stronger
/// browser authentication should add its own short token (the Terminal module
/// does this).
fn module_sandbox_path(path: &str) -> bool {
    if path.starts_with("/m/") {
        return true;
    }
    let Some(rest) = path.strip_prefix("/n/") else {
        return false;
    };
    let mut parts = rest.split('/');
    parts.next(); // node id
    parts.next() == Some("m")
}

/// Module WebSocket routes are reached from the shell, from the sandboxed
/// module frame (`Origin: null`), or from the trusted native/browser bridge
/// (no Origin). A configured full-app origin has no business opening another
/// module's sidecar WebSocket.
fn module_websocket_path(path: &str) -> bool {
    module_sandbox_path(path) && (path.ends_with("/ws") || path.contains("/ws/"))
}

fn shell_origin(origin: &str, config: &Config) -> bool {
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    is_loopback_host(host) || host.eq_ignore_ascii_case(config.canonical_host())
}

fn origin_allowed(origin: &str, config: &Config) -> bool {
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    let suffix = format!(".{}", config.canonical_host());
    is_loopback_host(host)
        || host.eq_ignore_ascii_case(config.canonical_host())
        || host
            .len()
            .checked_sub(suffix.len())
            .is_some_and(|idx| host[idx..].eq_ignore_ascii_case(&suffix))
}

async fn nodes(State(state): State<Arc<AppState>>) -> Response {
    let local = crate::wake::local_interfaces();
    let cache = state.wake_cache.read().expect("wake cache lock");
    let mut nodes = vec![json!({
        "id": "local",
        "name": local_name(),
        "online": true,
        "local": true,
        "latency_ms": 0,
    })];
    for peer in state.peer_health.read().expect("peer health lock").values() {
        let wake = !crate::wake::matching_targets(&cache.interfaces(&peer.id, &peer.name), &local)
            .is_empty();
        nodes.push(json!({
            "id": peer.id,
            "name": peer.name,
            "online": peer.online,
            "local": false,
            "latency_ms": peer.latency_ms,
            "error": peer.error,
            "wake": wake,
        }));
    }
    Json(nodes).into_response()
}

async fn wake_node(State(state): State<Arc<AppState>>, Path(node): Path<String>) -> Response {
    let config = state.config();
    let Some(peer) = config.peers.iter().find(|peer| peer.id == node).cloned() else {
        return (StatusCode::NOT_FOUND, format!("unknown machine: {node}")).into_response();
    };
    let targets = state.wake_targets(&peer.id, &peer.name);
    if targets.is_empty() {
        return (
            StatusCode::NOT_FOUND,
            format!("no wake-on-LAN available for {}", peer.name),
        )
            .into_response();
    }
    match crate::wake::send_targets(&targets, 3).await {
        Ok(sent) => {
            state.audit(
                "node.wake",
                json!({
                    "node": peer.id,
                    "name": peer.name,
                    "targets": targets.len(),
                    "sent": sent,
                }),
            );
            Json(json!({
                "ok": true,
                "node": peer.id,
                "name": peer.name,
                "sent": sent,
            }))
            .into_response()
        }
        Err(error) => (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response(),
    }
}

async fn peer_wake(State(state): State<Arc<AppState>>, headers: HeaderMap) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    Json(json!({"interfaces": crate::wake::local_interfaces()})).into_response()
}

async fn services(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config();
    let services: Vec<_> = config
        .proxy
        .iter()
        .map(|mount| {
            json!({
                "id": mount.id,
                "title": if mount.title.is_empty() { &mount.id } else { &mount.title },
                "base_url": crate::config::redact_url_userinfo(&mount.base_url),
                "host": mount.host,
                "websocket": mount.websocket,
                "kind": "web-service",
            })
        })
        .collect();
    Json(services).into_response()
}

async fn list_peers(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config();
    let health = state.peer_health.read().expect("peer health lock");
    let peers: Vec<_> = config
        .peers
        .iter()
        .map(|peer| {
            let mut value = serde_json::to_value(peers::PeerView::from(peer)).unwrap_or_default();
            if let Some(status) = health.get(&peer.id) {
                value["online"] = json!(status.online);
                value["latency_ms"] = json!(status.latency_ms);
                value["error"] = json!(status.error);
            }
            value
        })
        .collect();
    Json(peers).into_response()
}

fn app_url_for_node(state: &AppState, mount: &str, node: &str) -> Option<String> {
    let config = state.config();
    if node == "local" {
        let mount_cfg = config.proxy(mount)?;
        return mount_cfg
            .host
            .as_ref()
            .map(|host| format!("http://{host}:{}/", config.server.port));
    }
    if !config.peers.iter().any(|peer| peer.id == node) {
        return None;
    }
    let canonical = config.canonical_host();
    let hostname = config
        .proxy(mount)
        .and_then(|mount_cfg| mount_cfg.host.as_ref())
        .map(|host| host.replace(&format!(".{canonical}"), &format!(".{node}.{canonical}")))
        .unwrap_or_else(|| format!("{mount}.{node}.{canonical}"));
    Some(format!("http://{hostname}:{}/", config.server.port))
}

fn safe_app_path(value: Option<&str>) -> String {
    value
        .filter(|path| {
            path.starts_with('/')
                && !path.starts_with("//")
                && !path.contains('\\')
                && !path.chars().any(char::is_control)
                && !path.split('/').any(|part| part == "..")
        })
        .unwrap_or_default()
        .to_string()
}

fn app_open_payload(
    state: &AppState,
    mount: &str,
    node: &str,
    path: Option<&str>,
    handoff: Option<&str>,
    title: Option<&str>,
) -> Result<Value> {
    let config = state
        .config()
        .proxy(mount)
        .cloned()
        .context("proxy mount not found")?;
    let Some(url) = app_url_for_node(state, &config.id, node) else {
        anyhow::bail!(
            "proxy mount `{}` has no configured host; add `host = \"{}.orqella.localhost\"` and restart",
            config.id,
            config.id
        );
    };
    let host = url
        .strip_prefix("http://")
        .and_then(|rest| rest.split(':').next())
        .map(str::to_string);
    let title = title
        .map(str::to_string)
        .filter(|title| !title.is_empty())
        .unwrap_or_else(|| {
            if config.title.is_empty() {
                config.id.clone()
            } else {
                config.title.clone()
            }
        });
    let server_key =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(url.trim_end_matches('/'));
    let path = safe_app_path(path).replace("{server_key}", &server_key);
    Ok(json!({
        "mount": config.id,
        "title": title,
        "node": node,
        "host": host,
        "url": url,
        "path": path,
        "handoff": safe_app_path(handoff),
    }))
}

fn expand_viewer_path(template: &str, session: &str, root: Option<&str>) -> String {
    let encoded_root: String =
        url::form_urlencoded::byte_serialize(root.unwrap_or_default().as_bytes()).collect();
    template
        .replace("{session}", session)
        .replace("{session_id}", session)
        .replace("{root}", &encoded_root)
}

fn conversation_open_payload(
    state: &AppState,
    viewer: &AgentViewerConfig,
    session: &str,
    node: &str,
    root: Option<&str>,
) -> Result<Value> {
    let session = session.trim();
    if session.is_empty() {
        bail!("conversation session is empty");
    }
    let expand = |value: &str| expand_viewer_path(value, session, root);
    match viewer.kind.as_str() {
        "module" => {
            let path = if viewer.path.is_empty() {
                "/".to_string()
            } else {
                expand(&viewer.path)
            };
            Ok(json!({
                "module": viewer.module,
                "node": node,
                "path": safe_app_path(Some(&path)),
                "title": viewer.title,
            }))
        }
        "app" => {
            let path = if viewer.path.is_empty() {
                "/".to_string()
            } else {
                expand(&viewer.path)
            };
            let app = app_open_payload(
                state,
                &viewer.mount,
                node,
                Some(&path),
                None,
                Some(&viewer.title),
            )?;
            Ok(json!({ "app": app }))
        }
        "url" => {
            let url = expand(&viewer.url);
            if !(url.starts_with("http://") || url.starts_with("https://")) {
                bail!("conversation viewer URL must start with http:// or https://");
            }
            Ok(json!({ "external": true, "url": url }))
        }
        _ => bail!("conversation viewer is not configured"),
    }
}

fn node_from_params(params: &Value) -> String {
    params
        .get("node")
        .and_then(Value::as_str)
        .filter(|node| !node.is_empty())
        .unwrap_or("local")
        .to_string()
}

fn peer_by_id(state: &AppState, id: &str) -> Result<crate::config::PeerConfig> {
    state
        .config()
        .peers
        .iter()
        .find(|peer| peer.id == id)
        .cloned()
        .with_context(|| format!("unknown node {id}"))
}

fn local_name() -> String {
    hostname::get()
        .ok()
        .and_then(|name| name.into_string().ok())
        .unwrap_or_else(|| "local".into())
}

fn storage_path(paths: &Paths, id: &str) -> std::path::PathBuf {
    paths
        .data_dir
        .join("module-data")
        .join(format!("{id}.json"))
}

fn read_module_storage(paths: &Paths, id: &str) -> Result<serde_json::Map<String, Value>> {
    let path = storage_path(paths, id);
    if !path.exists() {
        return Ok(serde_json::Map::new());
    }
    let _ = util::restrict_file(&path);
    let raw = std::fs::read_to_string(&path)?;
    let value: Value = serde_json::from_str(&raw)?;
    Ok(value.as_object().cloned().unwrap_or_default())
}

fn write_module_storage(
    paths: &Paths,
    id: &str,
    value: &serde_json::Map<String, Value>,
) -> Result<()> {
    let path = storage_path(paths, id);
    util::write_atomic(&path, serde_json::to_vec_pretty(value)?.as_slice())?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            // Windows session 0 / S4U processes may not have a console. Do not
            // turn a missing control handler into an immediate clean shutdown.
            tracing::debug!(%error, "no console control handler; running until terminated");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        let mut signal =
            match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
                Ok(signal) => signal,
                Err(_) => return,
            };
        signal.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

#[cfg(test)]
mod tests;
