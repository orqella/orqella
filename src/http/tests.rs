use super::build::rewrite_open_node;
use super::module_pages::module_asset_allowed;
use super::peer_routes::constant_time_eq;
use super::updates::update_target_block;
use super::*;
use axum::http::HeaderValue;

fn config() -> Config {
    let mut config = Config::default();
    config.proxy.push(crate::config::ProxyConfig {
        id: "demo".into(),
        title: "Demo".into(),
        base_url: "http://127.0.0.1:49374".into(),
        host: Some("demo.orqella.localhost".into()),
        websocket: true,
        frame: false,
    });
    config
}

#[test]
fn accepts_loopback_and_canonical_hosts() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("127.0.0.1:9477"));
    assert!(host_allowed(&headers, &config()));
    headers.insert(header::HOST, HeaderValue::from_static("127.0.0.5:9477"));
    assert!(host_allowed(&headers, &config()));
    headers.insert(
        header::HOST,
        HeaderValue::from_static("orqella.localhost:9477"),
    );
    assert!(host_allowed(&headers, &config()));
    headers.insert(
        header::HOST,
        HeaderValue::from_static("demo.orqella.localhost:9477"),
    );
    assert!(host_allowed(&headers, &config()));
    headers.insert(header::HOST, HeaderValue::from_static("evil.example:9477"));
    assert!(!host_allowed(&headers, &config()));
    headers.remove(header::HOST);
    assert!(!host_allowed(&headers, &config()));
}

#[test]
fn accepts_ipv6_loopback_host() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, HeaderValue::from_static("[::1]:9477"));
    assert!(host_allowed(&headers, &config()));
    headers.insert(header::HOST, HeaderValue::from_static("[::1]garbage"));
    assert!(!host_allowed(&headers, &config()));
}

#[test]
fn module_data_and_private_files_are_not_assets() {
    assert!(!module_asset_allowed("data/config.toml"));
    assert!(!module_asset_allowed("Data/config.toml"));
    assert!(!module_asset_allowed("data\\config.toml"));
    assert!(!module_asset_allowed("data/models/model.bin"));
    assert!(!module_asset_allowed("ui/.hidden"));
    assert!(!module_asset_allowed("module.toml"));
    assert!(!module_asset_allowed("backend/data/catalog.json"));
    assert!(module_asset_allowed("ui/index.html"));
}

#[test]
fn maps_remote_mount_hosts_without_a_local_mount() {
    let mut config = config();
    config.peers.push(crate::config::PeerConfig {
        id: "link-laptop".into(),
        name: "Laptop".into(),
        url: Some("http://127.0.0.1:9478".into()),
        token: "test".into(),
    });
    let mut headers = HeaderMap::new();
    headers.insert(
        header::HOST,
        HeaderValue::from_static("opencode.link-laptop.orqella.localhost:9477"),
    );
    assert_eq!(
        app_route_for_host(&headers, &config),
        Some(("opencode".to_string(), Some("link-laptop".to_string())))
    );
}

#[test]
fn maps_mount_hosts_to_proxy_mounts() {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::HOST,
        HeaderValue::from_static("demo.orqella.localhost:9477"),
    );
    assert_eq!(app_route_for_host(&headers, &config()).unwrap().0, "demo");
    headers.insert(
        header::HOST,
        HeaderValue::from_static("orqella.localhost:9477"),
    );
    assert!(app_route_for_host(&headers, &config()).is_none());
}

#[test]
fn module_paths_allow_opaque_origins() {
    assert!(module_sandbox_path("/m/terminal/ws/terminal"));
    assert!(module_sandbox_path("/n/laptop/m/terminal/ws/terminal"));
    assert!(module_sandbox_path("/m/vm-lab/ui/console.js"));
    assert!(!module_sandbox_path("/n/laptop/p/opencode/m/not-a-module"));
    assert!(!module_sandbox_path("/api/v1/modules/terminal/rpc"));
}

#[test]
fn module_websockets_are_limited_to_shell_and_opaque_origins() {
    assert!(module_websocket_path("/m/terminal/ws/terminal"));
    assert!(module_websocket_path("/n/laptop/m/terminal/ws/terminal"));
    assert!(!module_websocket_path("/p/opencode/ws"));
    assert!(!module_websocket_path("/n/laptop/p/opencode/ws"));
    assert!(shell_origin("http://orqella.localhost:9477", &config()));
    assert!(shell_origin("http://127.0.0.1:9477", &config()));
    assert!(shell_origin("http://127.0.0.5:9477", &config()));
    assert!(!shell_origin(
        "http://opencode.orqella.localhost:9477",
        &config()
    ));
}

#[test]
fn peer_tokens_compare_without_early_exit() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"ab"));
}

#[test]
fn origins_are_allowlisted() {
    assert!(origin_allowed("http://127.0.0.1:9477", &config()));
    assert!(origin_allowed("http://127.0.0.5:9477", &config()));
    assert!(origin_allowed("http://[::1]:9477", &config()));
    assert!(shell_origin("http://[::1]:9477", &config()));
    assert!(origin_allowed(
        "http://demo.orqella.localhost:9477",
        &config()
    ));
    assert!(origin_allowed(
        "http://DEMO.ORQELLA.LOCALHOST:9477",
        &config()
    ));
    assert!(!origin_allowed("https://example.com", &config()));
}

#[test]
fn update_targets_only_outdated_clean_machines() {
    let up_to_date = json!({ "ok": true, "available": false, "dirty": false });
    assert_eq!(
        update_target_block(&up_to_date),
        Some(("skipped", "up to date".to_string()))
    );

    let outdated = json!({ "ok": true, "available": true, "dirty": false });
    assert!(update_target_block(&outdated).is_none());

    let dirty = json!({ "ok": true, "available": true, "dirty": true });
    assert_eq!(
        update_target_block(&dirty),
        Some((
            "blocked",
            "local changes; commit or stash first".to_string()
        ))
    );

    let unavailable = json!({ "ok": false, "error": "no update repository configured" });
    assert_eq!(
        update_target_block(&unavailable),
        Some(("blocked", "no update repository configured".to_string()))
    );

    let peer = json!({
        "ok": true,
        "result": { "ok": true, "available": true, "dirty": false }
    });
    assert!(update_target_block(&peer).is_none());
}

#[test]
fn viewer_paths_expand_session_and_root() {
    assert_eq!(
        expand_viewer_path(
            "/?harness=codex&session={session}&cwd={root}",
            "thread-1",
            Some("/home/me/My Project")
        ),
        "/?harness=codex&session=thread-1&cwd=%2Fhome%2Fme%2FMy+Project"
    );
    assert_eq!(
        expand_viewer_path("/?session={session_id}&cwd={root}", "ses_1", None),
        "/?session=ses_1&cwd="
    );
}

#[test]
fn remote_build_open_payloads_target_the_peer() {
    let mut module = json!({
        "open": { "module": "opencode", "node": "local", "path": "/?session=ses_1" }
    });
    rewrite_open_node(&mut module, "peer-a");
    assert_eq!(module["open"]["node"], "peer-a");

    let mut app = json!({
        "open": { "app": { "mount": "opencode", "node": "local", "path": "/session/ses_1" } }
    });
    rewrite_open_node(&mut app, "peer-a");
    assert_eq!(app["open"]["app"]["node"], "peer-a");

    let mut external = json!({
        "open": { "external": true, "url": "https://example.com/ses_1" }
    });
    rewrite_open_node(&mut external, "peer-a");
    assert!(external["open"].get("node").is_none());
}
