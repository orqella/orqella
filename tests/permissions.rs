use axum::http::StatusCode;
use orqella::config::{Config, Paths};
use orqella::http::{AppState, router};
use tokio::net::TcpListener;

fn paths(root: &std::path::Path) -> Paths {
    Paths {
        config_dir: root.join("config"),
        data_dir: root.join("data"),
        runtime_dir: root.join("runtime"),
        config_file: root.join("config/config.toml"),
        state_file: root.join("data/state.json"),
        modules_dir: root.join("data/modules"),
        workshop_dir: root.join("data/workshop"),
        logs_dir: root.join("data/logs"),
    }
}

fn write_module(root: &std::path::Path, version: &str, permissions: &str) {
    std::fs::create_dir_all(root.join("ui")).unwrap();
    std::fs::write(root.join("ui/index.html"), "permission fixture").unwrap();
    std::fs::write(
        root.join("module.toml"),
        format!(
            "schema = 1\nid = \"permission-test\"\nname = \"Permission test\"\nversion = \"{version}\"\nentry = \"ui/index.html\"\npermissions = [{permissions}]\n"
        ),
    )
    .unwrap();
}

#[tokio::test]
async fn pending_permission_blocks_rpc_until_granted() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    paths.ensure().unwrap();
    let source = temp.path().join("source");
    write_module(&source, "0.1.0", "\"node.metrics\"");
    let state = AppState::new(paths, Config::default()).await.unwrap();
    state.modules.install_from_path(&source).unwrap();
    write_module(&source, "0.2.0", "\"node.metrics\", \"storage\"");
    let updated = state.modules.install_from_path(&source).unwrap();
    assert_eq!(updated.pending_permissions, vec!["storage".to_string()]);

    let app = router(state.clone());
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let blocked = client
        .post(format!(
            "http://{address}/api/v1/modules/permission-test/rpc"
        ))
        .header("X-Orqella", "1")
        .header("X-Orqella-Admin-Token", &state.admin_token)
        .json(&serde_json::json!({ "method": "storage.get", "params": { "key": "x" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);

    let granted = client
        .post(format!(
            "http://{address}/api/v1/modules/permission-test/permissions"
        ))
        .header("X-Orqella", "1")
        .header("X-Orqella-Admin-Token", &state.admin_token)
        .json(&serde_json::json!({ "permissions": ["node.metrics", "storage"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(granted.status(), StatusCode::OK);

    let allowed = client
        .post(format!(
            "http://{address}/api/v1/modules/permission-test/rpc"
        ))
        .header("X-Orqella", "1")
        .header("X-Orqella-Admin-Token", &state.admin_token)
        .json(&serde_json::json!({ "method": "storage.get", "params": { "key": "x" } }))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);

    task.abort();
}

#[tokio::test]
async fn disabled_module_cannot_restart_its_sidecar() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    paths.ensure().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir_all(source.join("ui")).unwrap();
    std::fs::write(source.join("ui/index.html"), "disabled fixture").unwrap();
    std::fs::write(
        source.join("module.toml"),
        r#"schema = 1
id = "permission-test"
name = "Permission test"
version = "0.1.0"
entry = "ui/index.html"
permissions = ["backend"]

[backend]
kind = "sidecar"
command = ["sh", "-c", "sleep 60"]
"#,
    )
    .unwrap();
    let state = AppState::new(paths, Config::default()).await.unwrap();
    state.modules.install_from_path(&source).unwrap();
    state
        .modules
        .set_permissions("permission-test", vec!["backend".into()])
        .unwrap();
    state.modules.set_enabled("permission-test", false).unwrap();

    let app = router(state.clone());
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let response = reqwest::Client::new()
        .post(format!(
            "http://{address}/api/v1/modules/permission-test/sidecar/restart"
        ))
        .header("X-Orqella", "1")
        .header("X-Orqella-Admin-Token", &state.admin_token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(!state.sidecars.is_running("permission-test"));
    task.abort();
}

#[tokio::test]
async fn trust_changes_require_the_private_admin_token() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    paths.ensure().unwrap();
    let state = AppState::new(paths, Config::default()).await.unwrap();
    let app = router(state.clone());
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    let public_key = "a".repeat(64);
    let body = serde_json::json!({ "public_key": public_key, "label": "test" });

    let denied = client
        .post(format!("http://{address}/api/v1/trusted-keys"))
        .header("X-Orqella", "1")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let allowed = client
        .post(format!("http://{address}/api/v1/trusted-keys"))
        .header("X-Orqella", "1")
        .header("X-Orqella-Admin-Token", &state.admin_token)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
    task.abort();
}

#[tokio::test]
async fn unsandboxed_backend_requires_explicit_acknowledgment() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    paths.ensure().unwrap();
    let source = temp.path().join("source");
    std::fs::create_dir_all(source.join("ui")).unwrap();
    std::fs::write(source.join("ui/index.html"), "unsandboxed fixture").unwrap();
    std::fs::write(
        source.join("module.toml"),
        r#"schema = 1
id = "unsandboxed-test"
name = "Unsandboxed test"
version = "0.1.0"
entry = "ui/index.html"
permissions = ["backend"]

[backend]
kind = "sidecar"
command = ["sh", "-c", "sleep 60"]
sandbox = false
"#,
    )
    .unwrap();
    let state = AppState::new(paths, Config::default()).await.unwrap();
    state.modules.install_from_path(&source).unwrap();
    state
        .modules
        .set_enabled("unsandboxed-test", false)
        .unwrap();

    let app = router(state.clone());
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();
    let url = format!("http://{address}/api/v1/modules/unsandboxed-test/permissions");

    let denied = client
        .post(&url)
        .header("X-Orqella", "1")
        .header("X-Orqella-Admin-Token", &state.admin_token)
        .json(&serde_json::json!({ "permissions": ["backend"] }))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::BAD_REQUEST);
    assert!(
        !state
            .modules
            .get("unsandboxed-test")
            .unwrap()
            .grants("backend")
    );

    let allowed = client
        .post(&url)
        .header("X-Orqella", "1")
        .header("X-Orqella-Admin-Token", &state.admin_token)
        .json(&serde_json::json!({
            "permissions": ["backend"],
            "sandbox_acknowledged": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed.status(), StatusCode::OK);
    assert!(
        state
            .modules
            .get("unsandboxed-test")
            .unwrap()
            .grants("backend")
    );
    task.abort();
}

#[tokio::test]
async fn browser_session_cookie_is_created_automatically() {
    let temp = tempfile::tempdir().unwrap();
    let paths = paths(temp.path());
    paths.ensure().unwrap();
    let state = AppState::new(paths, Config::default()).await.unwrap();
    let app = router(state);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = reqwest::Client::new();

    let unauthorized = client
        .get(format!("http://{address}/api/v1/info"))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);

    let page = client
        .get(format!("http://{address}/"))
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Accept", "text/html")
        .header("User-Agent", "Mozilla/5.0 OrqellaTest")
        .send()
        .await
        .unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    let cookie = page
        .headers()
        .get("set-cookie")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .expect("session cookie")
        .to_string();

    let authorized = client
        .get(format!("http://{address}/api/v1/info"))
        .header("Cookie", cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), StatusCode::OK);
    task.abort();
}
