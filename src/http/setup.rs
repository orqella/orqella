use super::*;

/// Install OpenCode from the dashboard, after the daemon is already running.
///
/// This deliberately does not touch the module store, so it is safe while the
/// daemon is live. The Agent Harness module discovers the new binary on its
/// next probe; the daemon picks up the app surface and headless agent now.
pub(super) async fn install_opencode(State(state): State<Arc<AppState>>) -> Response {
    let mut updated = (*state.config()).clone();
    let result = match crate::setup::install_opencode_integration(&mut updated).await {
        Ok(result) => result,
        Err(error) => {
            return (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response();
        }
    };
    if let Err(error) = state.update_config(|config| {
        config.agent = updated.agent.clone();
        config.proxy = updated.proxy.clone();
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    state.builder.set_agent(state.config().agent.clone());
    state.audit(
        "setup.opencode",
        json!({
            "installed": result.opencode.installed,
            "model": result.opencode.model,
        }),
    );
    Json(json!({
        "ok": true,
        "opencode": result.opencode,
        "actions": result.actions,
        "warnings": result.warnings,
    }))
    .into_response()
}
