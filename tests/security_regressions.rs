//! Static regressions for browser-side security invariants that are not
//! reachable from the Rust unit tests.

#[test]
fn module_icons_are_escaped_and_peer_metadata_is_validated() {
    let dashboard = include_str!("../web/js/dashboard.js");
    assert!(dashboard.contains("function safeModuleIcon"));
    assert!(dashboard.contains("function moduleIconUrl"));
    assert!(dashboard.contains("esc(moduleIconUrl(module))"));
    assert!(!dashboard.contains("${moduleSurfaceUrl(module, module.icon)}"));
    assert!(dashboard.contains("MODULE_ICON_EXTENSIONS"));
}

#[test]
fn module_frames_require_a_document_nonce() {
    let shell = include_str!("../web/js/frames.js");
    let sdk = include_str!("../web/sdk.js");
    assert!(shell.contains("__orqella_frame"));
    assert!(shell.contains("frameNonceMatches"));
    assert!(shell.contains("state.readyFrames"));
    assert!(sdk.contains("__orqella_frame"));
    assert!(sdk.contains("nonce: frameNonce"));
    assert!(shell.contains("__orqella_ws"));
    assert!(sdk.contains("__orqella_ws"));
    assert!(sdk.contains("NativeWebSocket"));
}

#[test]
fn support_bundle_redaction_is_callable_without_live_modules() {
    let source = include_str!("../src/backup.rs");
    assert!(source.contains("redact_url_userinfo"));
    assert!(source.contains("redacted.agent.args"));
}

#[test]
fn publish_repository_changes_require_explicit_confirmation() {
    let shell = include_str!("../web/js/settings.js");
    let source = include_str!("../src/github.rs");
    assert!(shell.contains("publishConfirm"));
    assert!(shell.contains("confirm_repository"));
    assert!(shell.contains("source_repository"));
    assert!(source.contains("repository_change_confirmed"));
}

#[test]
fn unsandboxed_sidecars_require_an_explicit_acknowledgment() {
    let shell = include_str!("../web/app.js");
    let source = include_str!("../src/http/modules.rs");
    assert!(shell.contains("permissionSandboxAck"));
    assert!(shell.contains("sandbox_acknowledged"));
    assert!(source.contains("sandbox_acknowledged"));
}

#[test]
fn web_client_warns_that_module_frames_are_trusted() {
    let html = include_str!("../web-client/client/index.html");
    assert!(html.contains("trust-warning"));
    assert!(html.contains("Module surfaces are disabled"));
}

#[test]
fn web_client_escapes_link_device_fields() {
    let app = include_str!("../web-client/client/app.js");
    assert!(app.contains("${esc(device.web_endpoint_id)}"));
    assert!(app.contains("${esc(device.name)}"));
    assert!(app.contains("${esc(device.kind || 'device')}"));
}

#[test]
fn agent_does_not_inherit_the_daemon_environment() {
    let source = include_str!("../src/agent.rs");
    assert!(source.contains(".env_clear()"));
    assert!(source.contains(".envs(agent.resolved_env())"));
    assert!(!source.contains("std::env::vars()"));
}

#[test]
fn browser_bridge_uses_a_separate_token_from_the_admin_token() {
    let state = include_str!("../src/state.rs");
    let web = include_str!("../src/web.rs");
    let http = include_str!("../src/http.rs");
    assert!(state.contains("bridge_token"));
    assert!(web.contains("X-Orqella-Bridge-Token"));
    assert!(http.contains("x-orqella-bridge-token"));
    assert!(http.contains("x-orqella-admin-token"));
}

#[test]
fn browser_surfaces_send_a_csp_and_do_not_relax_frames() {
    let shell = include_str!("../src/http/shell.rs");
    assert!(shell.contains("CONTENT_SECURITY_POLICY"));
    assert!(shell.contains("X_CONTENT_TYPE_OPTIONS"));
    assert!(shell.contains("frame-ancestors 'self'"));
    let client = include_str!("../web-client/client/index.html");
    assert!(client.contains("Content-Security-Policy"));
    let app_client = include_str!("../web-client/app-client/index.html");
    assert!(app_client.contains("Content-Security-Policy"));
    let sw = include_str!("../web-client/sw.js");
    assert!(!sw.contains("allow-same-origin"));
    assert!(!sw.contains("content-security-policy"));
    assert!(!sw.contains("x-frame-options"));
    let app_sw = include_str!("../web-client/app-sw.js");
    assert!(!app_sw.contains("allow-same-origin"));
}
