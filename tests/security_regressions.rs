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
    assert!(html.contains("Trusted modules only"));
}
