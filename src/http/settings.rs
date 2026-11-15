use super::*;

#[derive(Deserialize)]
pub(super) struct LinkLogin {
    email: String,
    password: String,
}

pub(super) async fn link_view(State(state): State<Arc<AppState>>) -> Response {
    let error = state.link_error.read().expect("link error lock").clone();
    Json(json!({
        "status": state.link_status(),
        "error": error,
        "config": state.config().config_summary()["link"],
    }))
    .into_response()
}

pub(super) async fn link_login(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LinkLogin>,
) -> Response {
    let account = match AccountClient::production() {
        Ok(account) => account,
        Err(error) => return (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    };
    let auth = match account.login(&body.email, &body.password).await {
        Ok(auth) => auth,
        Err(error) => {
            return (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response();
        }
    };
    if let Err(error) = state.update_config(|config| {
        config.link.enabled = true;
        config.link.token = Some(auth.token);
        config.link.device_id = None;
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    match restart_link(Arc::clone(&state)).await {
        Ok(()) => {
            state.audit("link.login", json!({ "user": auth.user.id }));
            Json(json!({ "ok": true, "status": state.link_status() })).into_response()
        }
        Err(error) => {
            *state.link_error.write().expect("link error lock") = Some(format!("{error:#}"));
            (
                StatusCode::BAD_GATEWAY,
                format!("signed in, but Link did not start: {error:#}"),
            )
                .into_response()
        }
    }
}

pub(super) async fn link_start(State(state): State<Arc<AppState>>) -> Response {
    match restart_link(Arc::clone(&state)).await {
        Ok(()) => Json(json!({ "ok": true, "status": state.link_status() })).into_response(),
        Err(error) => {
            *state.link_error.write().expect("link error lock") = Some(format!("{error:#}"));
            (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response()
        }
    }
}

/// Rotate this machine's peer API secret and re-register it with Link. Other
/// machines refresh the derived token on their next Link restart.
pub(super) async fn rotate_peer(State(state): State<Arc<AppState>>) -> Response {
    let token = uuid::Uuid::new_v4().simple().to_string();
    if let Err(error) = state.update_config(|config| {
        config.peer_token = Some(token.clone());
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    state.audit("peer.rotate", json!({}));
    if state.config().link.enabled
        && let Err(error) = restart_link(Arc::clone(&state)).await
    {
        *state.link_error.write().expect("link error lock") = Some(format!("{error:#}"));
        return (
            StatusCode::BAD_GATEWAY,
            format!("peer secret rotated, but Link did not re-register: {error:#}"),
        )
            .into_response();
    }
    Json(json!({ "ok": true, "status": state.link_status() })).into_response()
}

#[derive(Deserialize)]
pub(super) struct LinkDevicePoll {
    device_code: String,
}

pub(super) async fn link_device_start(State(_state): State<Arc<AppState>>) -> Response {
    let account = match AccountClient::production() {
        Ok(account) => account,
        Err(error) => return (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    };
    match account.device_start().await {
        Ok(start) => Json(json!({
            "device_code": start.device_code,
            "user_code": start.user_code,
            "confirmation_code": start.confirmation_code,
            "verification_uri": start.verification_uri,
            "expires_in": start.expires_in,
            "interval": start.interval,
        }))
        .into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response(),
    }
}

pub(super) async fn link_device_poll(
    State(state): State<Arc<AppState>>,
    Json(body): Json<LinkDevicePoll>,
) -> Response {
    let account = match AccountClient::production() {
        Ok(account) => account,
        Err(error) => return (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    };
    let poll = match account.device_poll(&body.device_code).await {
        Ok(poll) => poll,
        Err(error) => return (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response(),
    };
    if poll.status != "approved" {
        return Json(json!({ "status": poll.status })).into_response();
    }
    let Some(token) = poll.token else {
        return (
            StatusCode::BAD_GATEWAY,
            "device approval did not return an account token",
        )
            .into_response();
    };
    let Some(user) = poll.user else {
        return (
            StatusCode::BAD_GATEWAY,
            "device approval did not return an account user",
        )
            .into_response();
    };
    if let Err(error) = state.update_config(|config| {
        config.link.enabled = true;
        config.link.token = Some(token);
        config.link.device_id = None;
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    match restart_link(Arc::clone(&state)).await {
        Ok(()) => {
            state.audit("link.login", json!({ "user": user.id, "method": "device" }));
            Json(json!({ "status": "approved", "status_detail": state.link_status() }))
                .into_response()
        }
        Err(error) => {
            *state.link_error.write().expect("link error lock") = Some(format!("{error:#}"));
            (
                StatusCode::BAD_GATEWAY,
                format!("device approved, but Link did not start: {error:#}"),
            )
                .into_response()
        }
    }
}

pub(super) async fn link_logout(State(state): State<Arc<AppState>>) -> Response {
    stop_link(Arc::clone(&state)).await;
    if let Err(error) = state.update_config(|config| {
        config.link.enabled = false;
        config.link.token = None;
        config.link.device_id = None;
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    *state.link_error.write().expect("link error lock") = None;
    state.audit("link.logout", json!({}));
    Json(json!({ "ok": true })).into_response()
}

pub(super) async fn link_devices(State(state): State<Arc<AppState>>) -> Response {
    let link = state.link.read().expect("link lock").clone();
    let Some(link) = link else {
        return (StatusCode::CONFLICT, "Link is not running").into_response();
    };
    match link.devices().await {
        Ok(devices) => Json(json!({ "devices": devices })).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct AuditSetting {
    enabled: bool,
}

#[derive(Deserialize)]
pub(super) struct ThemeSetting {
    theme: String,
}

#[derive(Deserialize)]
pub(super) struct ThemeAgentRequest {
    prompt: String,
}

pub(super) async fn complete_onboarding(State(state): State<Arc<AppState>>) -> Response {
    if let Err(error) = state.update_config(|config| {
        config.onboarding.completed = true;
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    state.audit("onboarding.complete", json!({}));
    Json(json!({ "ok": true })).into_response()
}

pub(super) async fn set_theme(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ThemeSetting>,
) -> Response {
    let requested = body.theme.trim().to_string();
    let theme_id = if let Some(canonical) = theme::canonical(&requested) {
        canonical.to_string()
    } else if theme::custom_css(&state.paths.config_dir, &requested).is_some() {
        requested
    } else {
        return (StatusCode::BAD_REQUEST, "unknown theme").into_response();
    };
    if let Err(error) = state.update_config(|config| {
        config.theme = theme_id.clone();
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    state.audit("theme.set", json!({ "theme": theme_id }));
    Json(json!({ "ok": true, "theme": theme_id })).into_response()
}

pub(super) async fn start_theme_agent(
    State(state): State<Arc<AppState>>,
    Json(body): Json<ThemeAgentRequest>,
) -> Response {
    let prompt = body.prompt.trim().to_string();
    if prompt.is_empty() {
        return (StatusCode::BAD_REQUEST, "describe the theme you want").into_response();
    }
    if prompt.len() > 8_000 {
        return (StatusCode::BAD_REQUEST, "theme prompt is too long").into_response();
    }
    let config = state.config();
    let base_theme = config.theme.clone();
    let base_css = theme::custom_css(&state.paths.config_dir, &base_theme)
        .or_else(|| {
            let selected = theme::normalize(&base_theme);
            WebAssets::get(&theme::asset_path(selected))
                .or_else(|| WebAssets::get("theme.css"))
                .map(|asset| String::from_utf8_lossy(&asset.data).to_string())
        })
        .unwrap_or_default();
    if base_css.is_empty() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "base theme is missing").into_response();
    }
    let fallback_name = theme::suggest_name(&prompt);
    let fallback_description = prompt.chars().take(200).collect::<String>();
    let theme_id = theme::new_custom_id(&fallback_name);
    let request = crate::agent::ThemeJobRequest {
        base_theme: base_theme.clone(),
        base_css,
        base_scheme: theme::selected_color_scheme(&state.paths.config_dir, &base_theme),
        theme_id: theme_id.clone(),
        fallback_name: fallback_name.clone(),
        fallback_description,
        prompt: prompt.clone(),
    };
    match state.builder.clone().start_theme(request) {
        Ok(job) => {
            state.audit(
                "theme.agent",
                json!({ "job": job.id, "base": base_theme, "theme": theme_id.clone(), "prompt": prompt }),
            );
            Json(json!({
                "ok": true,
                "job": job,
                "theme_id": theme_id,
                "theme_name": fallback_name
            }))
            .into_response()
        }
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}

pub(super) async fn theme_agent_status(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    match state.builder.get(&id) {
        Some(job) if job.kind == "theme" => Json(job).into_response(),
        _ => (StatusCode::NOT_FOUND, "theme job not found").into_response(),
    }
}

pub(super) async fn set_agent(
    State(state): State<Arc<AppState>>,
    Json(mut body): Json<AgentConfig>,
) -> Response {
    let preset = body.effective_preset();
    if !matches!(preset.as_str(), "opencode" | "codex" | "claude" | "custom") {
        return (StatusCode::BAD_REQUEST, "unknown agent preset").into_response();
    }
    body.preset = preset.clone();
    body.command = body.command.trim().to_string();
    if body.command.is_empty() {
        return (StatusCode::BAD_REQUEST, "agent command is required").into_response();
    }
    if body.args.len() > 64 || body.args.iter().any(|arg| arg.len() > 4096) {
        return (StatusCode::BAD_REQUEST, "agent args are too large").into_response();
    }
    if body.env.len() > 64
        || body
            .env
            .iter()
            .any(|(key, value)| key.is_empty() || key.contains('=') || value.len() > 8192)
    {
        return (StatusCode::BAD_REQUEST, "invalid agent environment").into_response();
    }
    // The API returns literal environment values as `[redacted]`. Preserve the
    // existing value when the settings form saves that unchanged marker.
    let previous_env = state.config().agent.env.clone();
    for (key, value) in &mut body.env {
        if value == "[redacted]"
            && let Some(previous) = previous_env.get(key)
        {
            *value = previous.clone();
        }
    }
    if body.timeout_secs > 86_400 {
        body.timeout_secs = 86_400;
    }
    body.viewer.kind = body.viewer.kind.trim().to_ascii_lowercase();
    body.viewer.module = body.viewer.module.trim().to_string();
    body.viewer.mount = body.viewer.mount.trim().to_string();
    body.viewer.path = body.viewer.path.trim().to_string();
    body.viewer.url = body.viewer.url.trim().to_string();
    body.viewer.title = body.viewer.title.trim().to_string();
    if !matches!(
        body.viewer.kind.as_str(),
        "" | "module" | "app" | "url" | "none"
    ) {
        return (StatusCode::BAD_REQUEST, "unknown conversation viewer kind").into_response();
    }
    if body.viewer.kind == "module" && body.viewer.module.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "conversation viewer module is required",
        )
            .into_response();
    }
    if body.viewer.kind == "app" && body.viewer.mount.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            "conversation viewer mount is required",
        )
            .into_response();
    }
    if body.viewer.kind == "url"
        && !(body.viewer.url.starts_with("http://") || body.viewer.url.starts_with("https://"))
    {
        return (
            StatusCode::BAD_REQUEST,
            "conversation viewer URL must start with http:// or https://",
        )
            .into_response();
    }

    if let Err(error) = state.update_config(|config| {
        config.agent = body.clone();
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    state.builder.set_agent(body.clone());
    state.audit(
        "agent.set",
        json!({ "preset": preset, "command": body.command }),
    );
    Json(json!({ "ok": true, "agent": body })).into_response()
}

pub(super) async fn set_audit(
    State(state): State<Arc<AppState>>,
    Json(body): Json<AuditSetting>,
) -> Response {
    if let Err(error) = state.update_config(|config| {
        config.audit.enabled = body.enabled;
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    if body.enabled {
        state.audit("audit.enable", json!({}));
    }
    Json(json!({ "ok": true, "enabled": body.enabled })).into_response()
}
