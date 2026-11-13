//! HTTP surface tests for the daemon router.
//!
//! These tests use the real Axum router and a real `AppState` in a temporary
//! data directory. They cover authentication, CSRF, host checks, module
//! lifecycle, asset containment, configuration redaction, themes, peers, and
//! the audit log without opening a network listener.

use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode, header};
use axum::response::Response;
use orqella::config::{Config, Paths};
use orqella::state::AppState;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use tower::ServiceExt;

const HOST: &str = "orqella.localhost:9477";

struct Harness {
    _root: tempfile::TempDir,
    paths: Paths,
    state: Arc<AppState>,
}

impl Harness {
    async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let paths = test_paths(root.path());
        paths.ensure().unwrap();
        let config = Config {
            peer_token: Some("peer-secret".into()),
            audit: orqella::config::AuditConfig { enabled: true },
            web: orqella::config::WebConfig {
                enabled: false,
                ..Default::default()
            },
            link: orqella::config::LinkConfig {
                enabled: false,
                ..Default::default()
            },
            ..Config::default()
        };
        let state = AppState::new(paths.clone(), config).await.unwrap();
        Self {
            _root: root,
            paths,
            state,
        }
    }

    fn admin_headers(&self) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static(HOST));
        headers.insert("x-orqella", HeaderValue::from_static("1"));
        headers.insert(
            "x-orqella-admin-token",
            HeaderValue::from_str(&self.state.admin_token).unwrap(),
        );
        headers
    }

    async fn request(
        &self,
        method: Method,
        uri: &str,
        headers: HeaderMap,
        body: Option<Body>,
    ) -> Response {
        let mut request = Request::builder().method(method).uri(uri);
        for (name, value) in headers.iter() {
            request = request.header(name, value);
        }
        let request = request
            .body(body.unwrap_or_else(Body::empty))
            .expect("valid request");
        orqella::http::router(Arc::clone(&self.state))
            .oneshot(request)
            .await
            .expect("router response")
    }

    async fn json(&self, method: Method, uri: &str, body: Value) -> Response {
        let mut headers = self.admin_headers();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        self.request(method, uri, headers, Some(Body::from(body.to_string())))
            .await
    }
}

fn test_paths(root: &Path) -> Paths {
    Paths {
        config_dir: root.join("config"),
        data_dir: root.join("data"),
        runtime_dir: root.join("run"),
        config_file: root.join("config/config.toml"),
        state_file: root.join("data/state.json"),
        modules_dir: root.join("data/modules"),
        workshop_dir: root.join("data/workshop"),
        logs_dir: root.join("data/logs"),
    }
}

fn write_module(root: &Path) {
    std::fs::create_dir_all(root.join("ui")).unwrap();
    std::fs::write(
        root.join("ui/index.html"),
        "<html><head></head><body>page</body></html>",
    )
    .unwrap();
    std::fs::write(root.join("ui/overview.html"), "<html>overview</html>").unwrap();
    std::fs::write(root.join("ui/settings.html"), "<html>settings</html>").unwrap();
    std::fs::write(root.join("ui/icon.svg"), "<svg></svg>").unwrap();
    std::fs::create_dir_all(root.join("data")).unwrap();
    std::fs::write(root.join("data/secret.json"), "do-not-serve").unwrap();
    std::fs::write(
        root.join("module.toml"),
        r#"schema = 1
id = "test.http"
name = "HTTP test"
version = "0.1.0"
api_version = "1"
entry = "ui/index.html"
overview = "ui/overview.html"
settings = "ui/settings.html"
icon = "ui/icon.svg"
permissions = ["node.info"]

[[config.fields]]
key = "refresh_ms"
type = "number"
label = "Refresh"
default = 3000
min = 1000
max = 60000

[[config.fields]]
key = "mode"
type = "select"
label = "Mode"
default = "compact"
options = [
  { value = "compact", label = "Compact" },
  { value = "full", label = "Full" },
]
"#,
    )
    .unwrap();
}

async fn body_text(response: Response) -> String {
    let bytes = to_bytes(response.into_body(), 8 * 1024 * 1024)
        .await
        .expect("body");
    String::from_utf8_lossy(&bytes).to_string()
}

async fn body_json(response: Response) -> Value {
    serde_json::from_str(&body_text(response).await).expect("json body")
}

#[tokio::test]
async fn rejects_unknown_hosts_and_accepts_loopback_aliases() {
    let harness = Harness::new().await;
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("evil.example"));
    let response = harness
        .request(Method::GET, "/healthz", headers, None)
        .await;
    assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);

    for host in ["127.0.0.1:9477", "localhost:9477", "[::1]:9477", HOST] {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_str(host).unwrap());
        let response = harness
            .request(Method::GET, "/healthz", headers, None)
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn shell_issues_a_session_and_requires_csrf_for_api_mutations() {
    let harness = Harness::new().await;
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static(HOST));
    headers.insert(
        HeaderName::from_static("sec-fetch-dest"),
        HeaderValue::from_static("document"),
    );
    headers.insert(
        HeaderName::from_static("sec-fetch-mode"),
        HeaderValue::from_static("navigate"),
    );
    let response = harness.request(Method::GET, "/", headers, None).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().contains_key(header::SET_COOKIE));
    assert!(
        response
            .headers()
            .contains_key(header::CONTENT_SECURITY_POLICY)
    );
    let cookie = response
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static(HOST));
    headers.insert(header::COOKIE, HeaderValue::from_str(&cookie).unwrap());
    let response = harness
        .request(Method::GET, "/api/v1/config", headers.clone(), None)
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // A state-changing API call needs the X-Orqella marker.
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let response = harness
        .request(
            Method::POST,
            "/api/v1/config/theme",
            headers.clone(),
            Some(Body::from(r#"{"theme":"signal"}"#)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    // Without a session or admin token it is still unauthorized.
    let mut unauthenticated = HeaderMap::new();
    unauthenticated.insert(header::HOST, HeaderValue::from_static(HOST));
    unauthenticated.insert("x-orqella", HeaderValue::from_static("1"));
    unauthenticated.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let response = harness
        .request(
            Method::POST,
            "/api/v1/config/theme",
            unauthenticated,
            Some(Body::from(r#"{"theme":"signal"}"#)),
        )
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn info_redacts_secrets_and_theme_assets_include_user_overrides() {
    let harness = Harness::new().await;
    harness
        .state
        .update_config(|config| {
            config
                .agent
                .env
                .insert("TOKEN".into(), "literal-secret".into());
            config.proxy.push(orqella::config::ProxyConfig {
                id: "demo".into(),
                title: "Demo".into(),
                base_url: "http://user:password@127.0.0.1:8080".into(),
                host: None,
                websocket: true,
                frame: false,
            });
        })
        .unwrap();
    let response = harness
        .request(Method::GET, "/api/v1/info", harness.admin_headers(), None)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(value["config"]["agent"]["env"]["TOKEN"], "[redacted]");
    let base = value["config"]["proxy"][0]["base_url"].as_str().unwrap();
    assert!(!base.contains("user"), "{base}");
    assert!(!base.contains("password"), "{base}");

    std::fs::write(
        harness.paths.config_dir.join("theme.css"),
        ":root { --user-override: 1; }",
    )
    .unwrap();
    let response = harness
        .request(
            Method::GET,
            "/assets/theme.css",
            harness.admin_headers(),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let css = body_text(response).await;
    assert!(css.contains("--user-override"));
    assert!(css.contains("--bg"));

    let response = harness
        .request(
            Method::GET,
            "/sdk/orqella.js",
            harness.admin_headers(),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_text(response).await.contains("window.orqella"));

    let response = harness
        .request(Method::GET, "/favicon.ico", harness.admin_headers(), None)
        .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn theme_and_agent_settings_validate_input() {
    let harness = Harness::new().await;
    let response = harness
        .json(
            Method::POST,
            "/api/v1/config/theme",
            json!({"theme": "nope"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = harness
        .json(
            Method::POST,
            "/api/v1/config/theme",
            json!({"theme": "signal"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(harness.state.config().theme, "signal");

    let response = harness
        .json(
            Method::POST,
            "/api/v1/config/agent",
            json!({
                "preset": "custom",
                "command": " ",
                "args": [],
                "env": {},
                "timeout_secs": 0,
                "viewer": {}
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let response = harness
        .json(
            Method::POST,
            "/api/v1/config/agent",
            json!({
                "preset": "custom",
                "command": "my-agent",
                "args": ["--json"],
                "env": {"TOKEN": "[redacted]"},
                "timeout_secs": 999999,
                "viewer": {"kind": "url", "url": "https://example.com/{session}"}
            }),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["agent"]["timeout_secs"], 86400);
    assert_eq!(value["agent"]["viewer"]["kind"], "url");
}

#[tokio::test]
async fn module_lifecycle_serves_assets_enforces_permissions_and_config() {
    let harness = Harness::new().await;
    let source = tempfile::tempdir().unwrap();
    write_module(source.path());

    let response = harness
        .json(
            Method::POST,
            "/api/v1/modules/install",
            json!({"path": source.path().to_string_lossy()}),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        body_text(response).await
    );

    let response = harness
        .request(
            Method::GET,
            "/api/v1/modules",
            harness.admin_headers(),
            None,
        )
        .await;
    let modules = body_json(response).await;
    assert!(
        modules
            .as_array()
            .unwrap()
            .iter()
            .any(|module| module["id"] == "test.http")
    );

    let response = harness
        .request(
            Method::GET,
            "/api/v1/modules/test.http",
            harness.admin_headers(),
            None,
        )
        .await;
    let detail = body_json(response).await;
    assert_eq!(detail["manifest"]["id"], "test.http");
    assert_eq!(detail["config"]["refresh_ms"], 3000);

    // Module pages are injected with the SDK and a relative base.
    let response = harness
        .request(Method::GET, "/m/test.http/", harness.admin_headers(), None)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let page = body_text(response).await;
    assert!(page.contains("<base href=\"/m/test.http/ui/\">"));
    assert!(page.contains("window.orqella"));
    assert!(page.contains("page"));

    // Runtime data and private files never leave the daemon.
    for path in [
        "/m/test.http/data/secret.json",
        "/m/test.http/module.toml",
        "/m/test.http/../module.toml",
    ] {
        let response = harness
            .request(Method::GET, path, harness.admin_headers(), None)
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }

    // RPC has a permission gate. The install granted node.info because it is
    // non-sensitive; revoke it and confirm the same call is rejected.
    let response = harness
        .json(
            Method::POST,
            "/api/v1/modules/test.http/rpc",
            json!({"method": "module.info"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = harness
        .json(
            Method::POST,
            "/api/v1/modules/test.http/permissions",
            json!({"permissions": []}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = harness
        .json(
            Method::POST,
            "/api/v1/modules/test.http/rpc",
            json!({"method": "system.info"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = harness
        .json(
            Method::POST,
            "/api/v1/modules/test.http/permissions",
            json!({"permissions": ["node.info"]}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = harness
        .json(
            Method::POST,
            "/api/v1/modules/test.http/rpc",
            json!({"method": "system.info"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let value = body_json(response).await;
    assert_eq!(value["result"]["local"], true);

    // Config updates validate types and bounds.
    let response = harness
        .json(
            Method::POST,
            "/api/v1/modules/test.http/config",
            json!({"values": {"refresh_ms": 10, "mode": "compact"}}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let response = harness
        .json(
            Method::POST,
            "/api/v1/modules/test.http/config",
            json!({"values": {"refresh_ms": 5000, "mode": "full"}}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        harness.state.modules.get("test.http").unwrap().state.config["mode"],
        "full"
    );

    // Disabling hides both pages and RPC.
    let response = harness
        .json(Method::POST, "/api/v1/modules/test.http/disable", json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = harness
        .request(Method::GET, "/m/test.http/", harness.admin_headers(), None)
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = harness
        .json(
            Method::POST,
            "/api/v1/modules/test.http/rpc",
            json!({"method": "module.info"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);

    let response = harness
        .json(Method::POST, "/api/v1/modules/test.http/enable", json!({}))
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    // Export produces a signed archive that can be removed and re-imported.
    let response = harness
        .request(
            Method::GET,
            "/api/v1/modules/test.http/export",
            harness.admin_headers(),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let archive = to_bytes(response.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap()
        .to_vec();

    let response = harness
        .request(
            Method::DELETE,
            "/api/v1/modules/test.http",
            harness.admin_headers(),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(harness.state.modules.get("test.http").is_none());

    let response = harness
        .request(
            Method::POST,
            "/api/v1/modules/import",
            harness.admin_headers(),
            Some(Body::from(archive)),
        )
        .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "{}",
        body_text(response).await
    );
    assert!(harness.state.modules.get("test.http").is_some());
}

#[tokio::test]
async fn module_surfaces_and_missing_modules_return_expected_statuses() {
    let harness = Harness::new().await;
    let source = tempfile::tempdir().unwrap();
    write_module(source.path());
    harness
        .state
        .modules
        .install_from_path(source.path())
        .unwrap();

    for (path, marker) in [
        ("/m/test.http/overview", "overview"),
        ("/m/test.http/settings", "settings"),
        ("/m/test.http/ui/overview.html", "overview"),
    ] {
        let response = harness
            .request(Method::GET, path, harness.admin_headers(), None)
            .await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert!(body_text(response).await.contains(marker), "{path}");
    }

    for path in [
        "/m/missing/",
        "/m/test.http/missing.html",
        "/api/v1/modules/missing",
        "/api/v1/modules/missing/log",
    ] {
        let response = harness
            .request(Method::GET, path, harness.admin_headers(), None)
            .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
}

#[tokio::test]
async fn peer_endpoints_require_the_peer_token_and_expose_safe_inventory() {
    let harness = Harness::new().await;
    let source = tempfile::tempdir().unwrap();
    write_module(source.path());
    harness
        .state
        .modules
        .install_from_path(source.path())
        .unwrap();

    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static(HOST));
    let response = harness
        .request(Method::GET, "/api/v1/peer/ping", headers.clone(), None)
        .await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

    headers.insert(
        "x-orqella-peer-token",
        HeaderValue::from_static("peer-secret"),
    );
    let response = harness
        .request(Method::GET, "/api/v1/peer/ping", headers.clone(), None)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_json(response).await["ok"], true);

    let response = harness
        .request(Method::GET, "/api/v1/peer/info", headers.clone(), None)
        .await;
    let info = body_json(response).await;
    assert!(
        info["modules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|module| module["id"] == "test.http")
    );
    assert!(info.get("config").is_none());

    let response = harness
        .request(Method::GET, "/api/v1/peer/wake", headers, None)
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(body_json(response).await["interfaces"].is_array());
}

#[tokio::test]
async fn audit_endpoint_is_opt_in_and_returns_newest_first() {
    let harness = Harness::new().await;
    let response = harness
        .json(
            Method::POST,
            "/api/v1/config/theme",
            json!({"theme": "daybreak"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = harness
        .json(
            Method::POST,
            "/api/v1/config/theme",
            json!({"theme": "foundry"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let response = harness
        .request(Method::GET, "/api/v1/audit", harness.admin_headers(), None)
        .await;
    let value = body_json(response).await;
    assert_eq!(value["enabled"], true);
    let events = value["events"].as_array().unwrap();
    assert!(events.len() >= 2);
    assert_eq!(events[0]["detail"]["theme"], "foundry");
    assert_eq!(events[1]["detail"]["theme"], "daybreak");

    harness
        .state
        .update_config(|config| config.audit.enabled = false)
        .unwrap();
    let response = harness
        .request(Method::GET, "/api/v1/audit", harness.admin_headers(), None)
        .await;
    let value = body_json(response).await;
    assert_eq!(value["enabled"], false);
    assert!(value["events"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn trusted_key_endpoint_requires_admin_and_valid_hex() {
    let harness = Harness::new().await;
    let response = harness
        .json(
            Method::POST,
            "/api/v1/trusted-keys",
            json!({"public_key": "not-hex", "label": "bad"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    let key = "ab".repeat(32);
    let response = harness
        .json(
            Method::POST,
            "/api/v1/trusted-keys",
            json!({"public_key": key, "label": "test"}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        harness
            .state
            .config()
            .trusted_module_keys
            .iter()
            .any(|trusted| trusted.public_key == key)
    );

    let response = harness
        .request(
            Method::GET,
            "/api/v1/trusted-keys",
            harness.admin_headers(),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let keys = body_json(response).await;
    assert!(
        keys.as_array()
            .unwrap()
            .iter()
            .any(|trusted| trusted["public_key"] == key)
    );
}
