use super::*;

pub(super) async fn github_view(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config().github.clone();
    Json(github::status(&state.paths, &config)).into_response()
}

#[derive(Deserialize)]
pub(super) struct GithubConfigUpdate {
    #[serde(default)]
    bridge_url: String,
}

pub(super) async fn github_config(
    State(state): State<Arc<AppState>>,
    Json(body): Json<GithubConfigUpdate>,
) -> Response {
    let bridge_url = body.bridge_url.trim().trim_end_matches('/').to_string();
    if !bridge_url.is_empty()
        && !bridge_url.starts_with("http://")
        && !bridge_url.starts_with("https://")
    {
        return (
            StatusCode::BAD_REQUEST,
            "bridge URL must use http:// or https://",
        )
            .into_response();
    }
    if let Err(error) = state.update_config(|config| {
        config.github.bridge_url = bridge_url;
    }) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
    }
    let config = state.config().github.clone();
    state.audit("github.config", json!({ "bridge_url": config.bridge_url }));
    Json(github::status(&state.paths, &config)).into_response()
}

pub(super) async fn github_connect(State(state): State<Arc<AppState>>) -> Response {
    let config = state.config().github.clone();
    match github::start_connect(&config).await {
        Ok(session) => Json(session).into_response(),
        Err(error) => (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response(),
    }
}

#[derive(Deserialize)]
pub(super) struct GithubPoll {
    session_id: String,
}

pub(super) async fn github_connect_poll(
    State(state): State<Arc<AppState>>,
    Json(body): Json<GithubPoll>,
) -> Response {
    let config = state.config().github.clone();
    match github::poll_connect(&state.paths, &config, body.session_id.trim()).await {
        Ok(result) => {
            if result["connected"].as_bool().unwrap_or(false) {
                state.audit(
                    "github.connect",
                    json!({ "account": result["account_login"] }),
                );
            }
            Json(result).into_response()
        }
        Err(error) => (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response(),
    }
}

pub(super) async fn github_disconnect(State(state): State<Arc<AppState>>) -> Response {
    match github::disconnect(&state.paths).await {
        Ok(()) => {
            state.audit("github.disconnect", json!({}));
            Json(json!({ "ok": true })).into_response()
        }
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response(),
    }
}

fn default_true() -> bool {
    true
}

#[derive(Deserialize)]
pub(super) struct GithubPublish {
    module_id: String,
    #[serde(default)]
    repository: String,
    #[serde(default)]
    confirm_repository: String,
    #[serde(default = "default_true")]
    private: bool,
    #[serde(default)]
    fork: bool,
    #[serde(default)]
    message: String,
}

pub(super) async fn github_publish(
    State(state): State<Arc<AppState>>,
    Json(body): Json<GithubPublish>,
) -> Response {
    let repository = body.repository.trim();
    let confirm_repository = body.confirm_repository.trim();
    let message = body.message.trim();
    let config = state.config().github.clone();
    match github::publish(
        &state.paths,
        &config,
        &state.modules,
        body.module_id.trim(),
        github::PublishOptions {
            repository: if repository.is_empty() {
                None
            } else {
                Some(repository)
            },
            confirm_repository: if confirm_repository.is_empty() {
                None
            } else {
                Some(confirm_repository)
            },
            private: body.private,
            fork: body.fork,
            message: if message.is_empty() {
                None
            } else {
                Some(message)
            },
        },
    )
    .await
    {
        Ok(result) => {
            state.audit(
                "module.publish",
                json!({
                    "module": body.module_id,
                    "repository": result.repository,
                    "branch": result.branch,
                    "forked": result.forked,
                }),
            );
            Json(result).into_response()
        }
        Err(error) => (StatusCode::BAD_GATEWAY, format!("{error:#}")).into_response(),
    }
}
