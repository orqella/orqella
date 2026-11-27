//! Privileged bridge used by the MCP stdio gateway.
//!
//! These routes are not the MCP protocol themselves. `orqella mcp` fetches the
//! tool catalog and invokes module-contributed tools through this small JSON
//! surface, which keeps the registry, permission checks, sidecar routing, and
//! audit in the daemon.

use super::*;
use crate::mcp::{DaemonClient, McpProfile, OrqellaMcp};
use http_body_util::{BodyExt, Full, combinators::BoxBody};
use rmcp::transport::{
    StreamableHttpServerConfig, StreamableHttpService,
    streamable_http_server::session::local::LocalSessionManager,
};
use std::convert::Infallible;

type McpHttpResponse = Response<BoxBody<Bytes, Infallible>>;

#[derive(Debug, Serialize)]
pub(super) struct ModuleToolSpec {
    pub module: String,
    pub name: String,
    pub qualified: String,
    pub title: Option<String>,
    pub description: String,
    pub input_schema: Value,
    pub mutating: bool,
    pub sidecar_running: bool,
}

pub(super) async fn list_module_tools(State(state): State<Arc<AppState>>) -> Response {
    Json(collect_module_tools(&state)).into_response()
}

fn collect_module_tools(state: &AppState) -> Vec<ModuleToolSpec> {
    let mut tools = Vec::new();
    for module in state.modules.installed() {
        if !module.state.enabled
            || !module.manifest.mcp.enabled
            || !module.grants("mcp")
            || !module.grants("backend")
        {
            continue;
        }
        for tool in &module.manifest.mcp.tools {
            let Some(schema_path) = crate::util::safe_join(&module.root, &tool.input_schema) else {
                tracing::warn!(
                    module = %module.manifest.id,
                    tool = %tool.name,
                    "skipping MCP tool with an unsafe schema path"
                );
                continue;
            };
            let schema = match crate::util::open_read_no_follow(&schema_path)
                .ok()
                .and_then(|mut file| {
                    use std::io::Read as _;
                    let mut raw = String::new();
                    file.read_to_string(&mut raw).ok()?;
                    serde_json::from_str::<Value>(&raw).ok()
                }) {
                Some(schema) if schema.is_object() => schema,
                _ => {
                    tracing::warn!(
                        module = %module.manifest.id,
                        tool = %tool.name,
                        "skipping MCP tool with an unreadable schema"
                    );
                    continue;
                }
            };
            tools.push(ModuleToolSpec {
                module: module.manifest.id.clone(),
                name: tool.name.clone(),
                qualified: format!("module.{}.{}", module.manifest.id, tool.name),
                title: tool.title.clone(),
                description: tool.description.clone(),
                input_schema: schema,
                mutating: tool.mutating,
                sidecar_running: state.sidecars.is_running(&module.manifest.id),
            });
        }
    }
    tools.sort_by(|left, right| left.qualified.cmp(&right.qualified));
    tools
}

pub(super) async fn call_module_tool(
    State(state): State<Arc<AppState>>,
    Path((module_id, tool_name)): Path<(String, String)>,
    body: Option<Json<Value>>,
) -> Response {
    let Some(module) = state.modules.get(&module_id) else {
        return (StatusCode::NOT_FOUND, "module not found").into_response();
    };
    if !module.state.enabled {
        return (StatusCode::CONFLICT, "module is disabled").into_response();
    }
    if !module.manifest.mcp.enabled {
        return (StatusCode::NOT_FOUND, "module does not expose MCP tools").into_response();
    }
    if !module.grants("mcp") {
        return (
            StatusCode::FORBIDDEN,
            "module has not been granted the mcp permission",
        )
            .into_response();
    }
    if !module.grants("backend") {
        return (
            StatusCode::FORBIDDEN,
            "module has not been granted the backend permission",
        )
            .into_response();
    }
    let Some(tool) = module
        .manifest
        .mcp
        .tools
        .iter()
        .find(|tool| tool.name == tool_name)
    else {
        return (StatusCode::NOT_FOUND, "MCP tool not found").into_response();
    };
    if !state.sidecars.is_running(&module_id) {
        return (StatusCode::CONFLICT, "module sidecar is not running").into_response();
    }
    let arguments = body.map(|Json(value)| value).unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return (
            StatusCode::BAD_REQUEST,
            "MCP tool arguments must be an object",
        )
            .into_response();
    }
    match state
        .sidecars
        .request(&module_id, "POST", &tool.path, Some(arguments))
        .await
    {
        Ok(result) => {
            let encoded = serde_json::to_vec(&result).unwrap_or_default();
            if encoded.len() > 2 * 1024 * 1024 {
                return (
                    StatusCode::BAD_GATEWAY,
                    "module MCP tool result exceeds the 2 MiB limit",
                )
                    .into_response();
            }
            state.audit(
                "mcp.module_tool",
                json!({ "module": module_id, "tool": tool_name }),
            );
            Json(json!({
                "ok": true,
                "module": module_id,
                "tool": tool_name,
                "result": result,
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("module MCP tool failed: {error:#}"),
        )
            .into_response(),
    }
}

/// Mount the opt-in Streamable HTTP MCP endpoint at `/mcp`.
///
/// The endpoint is a thin front for the same `OrqellaMcp` handler used by the
/// stdio gateway. Its self-client uses the private admin token; external
/// clients must present the dedicated `data/mcp.token` bearer capability.
pub(super) fn mount(state: Arc<AppState>, router: Router<Arc<AppState>>) -> Router<Arc<AppState>> {
    if !state.config().mcp.enabled {
        return router;
    }
    let config = state.config();
    let profile = McpProfile::parse(&config.mcp.profile).unwrap_or_else(|error| {
        tracing::warn!(%error, "invalid [mcp] profile; using read");
        McpProfile::Read
    });
    let template = OrqellaMcp::new(
        DaemonClient::new(self_origin(&config), state.admin_token.clone()),
        profile,
    );
    let mut allowed_hosts = vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
        "::1".to_string(),
        config.canonical_host().to_string(),
    ];
    let bind = config.server.bind.trim();
    if !bind.is_empty() {
        allowed_hosts.push(bind.to_string());
    }
    allowed_hosts.sort();
    allowed_hosts.dedup();
    let http_config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(allowed_hosts)
        .with_legacy_session_mode(true);
    let service = StreamableHttpService::new(
        move || Ok(template.clone()),
        Arc::new(LocalSessionManager::default()),
        http_config,
    );
    let auth_state = Arc::clone(&state);
    let guarded = tower::service_fn(move |request: Request<Body>| {
        let service = service.clone();
        let state = Arc::clone(&auth_state);
        async move {
            if !authorized_http_mcp(&state, request.headers()) {
                return Ok::<McpHttpResponse, Infallible>(http_error(
                    StatusCode::UNAUTHORIZED,
                    "unauthorized",
                ));
            }
            service.oneshot(request).await
        }
    });
    router.nest_service("/mcp", guarded)
}

/// Origin the MCP service uses to call the daemon's own API. Use the actual
/// bind address, not the canonical host, so a remote-bind opt-in can still
/// reach itself without depending on hostname resolution.
fn self_origin(config: &Config) -> String {
    let bind = config.server.bind.trim();
    let host = match bind {
        "" | "0.0.0.0" => "127.0.0.1".to_string(),
        "::" => "[::1]".to_string(),
        value if value.contains(':') && !value.starts_with('[') => format!("[{value}]"),
        value => value.to_string(),
    };
    format!("http://{host}:{}/", config.server.port)
}

fn authorized_http_mcp(state: &AppState, headers: &HeaderMap) -> bool {
    authorized_token(state.mcp_token.as_deref(), headers)
}

fn authorized_token(expected: Option<&str>, headers: &HeaderMap) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    let supplied = bearer_token(headers)
        .or_else(|| {
            headers
                .get("x-orqella-mcp-token")
                .and_then(|value| value.to_str().ok())
        })
        .unwrap_or_default()
        .trim();
    token_eq(supplied.as_bytes(), expected.as_bytes())
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        Some(token.trim())
    } else {
        None
    }
}

fn token_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

fn http_error(status: StatusCode, message: &'static str) -> McpHttpResponse {
    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Full::new(Bytes::from_static(message.as_bytes())).boxed())
        .expect("valid MCP HTTP response");
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Paths};

    fn write_fixture(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("ui")).unwrap();
        std::fs::create_dir_all(root.join("mcp")).unwrap();
        std::fs::write(root.join("ui/index.html"), "hello").unwrap();
        std::fs::write(
            root.join("mcp/status.schema.json"),
            r#"{"type":"object","properties":{},"additionalProperties":false}"#,
        )
        .unwrap();
        std::fs::write(
            root.join("module.toml"),
            r#"schema = 1
id = "com.example.mcp-fixture"
name = "MCP fixture"
version = "0.1.0"
entry = "ui/index.html"
permissions = ["backend", "mcp"]

[backend]
kind = "sidecar"
command = ["python3", "server.py"]
health_path = "/health"

[mcp]
enabled = true

[[mcp.tools]]
name = "status"
title = "Fixture status"
description = "Read the fixture status."
input_schema = "mcp/status.schema.json"
path = "/api/mcp/status"
mutating = false
"#,
        )
        .unwrap();
    }

    #[test]
    fn self_origin_uses_the_bind_address() {
        let mut config = Config::default();
        assert_eq!(self_origin(&config), "http://127.0.0.1:9477/");
        config.server.bind = "192.168.1.5".into();
        assert_eq!(self_origin(&config), "http://192.168.1.5:9477/");
        config.server.bind = "0.0.0.0".into();
        assert_eq!(self_origin(&config), "http://127.0.0.1:9477/");
        config.server.bind = "::1".into();
        assert_eq!(self_origin(&config), "http://[::1]:9477/");
    }

    #[tokio::test]
    async fn http_mcp_is_mounted_only_when_enabled_and_requires_a_token() {
        use tower::ServiceExt as _;

        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::from_root(temp.path()).unwrap();
        let mut config = Config::default();
        config.mcp.enabled = true;
        let state = AppState::new(paths, config).await.unwrap();
        let app = super::super::router(state);
        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1:9477")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(Body::from("{}"))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::from_root(temp.path()).unwrap();
        let state = AppState::new(paths, Config::default()).await.unwrap();
        let app = super::super::router(state);
        let request = Request::builder()
            .method(Method::POST)
            .uri("/mcp")
            .header(header::HOST, "127.0.0.1:9477")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn http_mcp_accepts_bearer_and_rejects_missing_or_wrong_tokens() {
        let mut headers = HeaderMap::new();
        assert!(!authorized_token(Some("secret"), &headers));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        assert!(authorized_token(Some("secret"), &headers));
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer wrong"),
        );
        assert!(!authorized_token(Some("secret"), &headers));
        headers.remove(header::AUTHORIZATION);
        headers.insert("x-orqella-mcp-token", HeaderValue::from_static("secret"));
        assert!(authorized_token(Some("secret"), &headers));
        assert!(!authorized_token(None, &headers));
    }

    #[tokio::test]
    async fn module_tools_require_the_mcp_and_backend_grants() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::from_root(temp.path()).unwrap();
        let state = AppState::new(paths, Config::default()).await.unwrap();
        let fixture = temp.path().join("fixture");
        write_fixture(&fixture);
        state.modules.install_from_path(&fixture).unwrap();

        assert!(
            collect_module_tools(&state).is_empty(),
            "tools must stay hidden until the user grants them"
        );

        state
            .modules
            .set_permissions(
                "com.example.mcp-fixture",
                vec!["backend".into(), "mcp".into()],
            )
            .unwrap();
        let tools = collect_module_tools(&state);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].qualified, "module.com.example.mcp-fixture.status");
        assert_eq!(tools[0].input_schema["type"], "object");
        assert!(!tools[0].mutating);
        assert!(!tools[0].sidecar_running);
    }

    #[tokio::test]
    async fn module_tool_listing_skips_disabled_modules() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::from_root(temp.path()).unwrap();
        let state = AppState::new(paths, Config::default()).await.unwrap();
        let fixture = temp.path().join("fixture");
        write_fixture(&fixture);
        state.modules.install_from_path(&fixture).unwrap();
        state
            .modules
            .set_permissions(
                "com.example.mcp-fixture",
                vec!["backend".into(), "mcp".into()],
            )
            .unwrap();
        state
            .modules
            .set_enabled("com.example.mcp-fixture", false)
            .unwrap();
        assert!(collect_module_tools(&state).is_empty());
    }
}
