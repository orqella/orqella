//! GitHub App publisher client.
//!
//! The daemon never stores the GitHub App private key. A small publisher bridge
//! owns the app credentials, binds a GitHub App installation to this daemon with
//! a short pairing flow, and mints short-lived installation tokens on demand.
//! Module source is pushed directly from this machine to GitHub with that
//! token; it does not pass through the bridge.

use crate::config::{GithubConfig, Paths};
use crate::modules::git;
use crate::modules::store::{ModuleStore, ModuleView};
use crate::util;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Connection {
    pub bridge_url: String,
    pub token: String,
    pub account_login: String,
    #[serde(default)]
    pub account_type: String,
    pub installation_id: u64,
    pub connected_at: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct GithubStatus {
    pub connected: bool,
    pub bridge_url: String,
    pub account_login: Option<String>,
    pub account_type: Option<String>,
    pub installation_id: Option<u64>,
    pub connected_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct SessionStart {
    id: String,
    connect_url: String,
    poll_secret: String,
    expires_at: String,
}

static PENDING_POLL_SECRETS: LazyLock<Mutex<HashMap<String, (String, Instant)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Deserialize)]
struct SessionPoll {
    status: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    account: Option<Account>,
    #[serde(default)]
    installation_id: Option<u64>,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Account {
    login: String,
    #[serde(rename = "type", default)]
    kind: String,
}

#[derive(Debug, Clone, Deserialize)]
struct InstallationToken {
    token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Repository {
    pub full_name: String,
    pub html_url: String,
    #[serde(default)]
    pub default_branch: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PublishResult {
    pub module: ModuleView,
    pub repository: String,
    pub url: String,
    pub branch: String,
    pub commit: Option<String>,
    pub committed: bool,
    pub created: bool,
    pub forked: bool,
    pub upstream_repository: Option<String>,
}

#[derive(Debug, Clone, Copy)]
pub struct PublishOptions<'a> {
    pub repository: Option<&'a str>,
    /// The repository retyped by the user when it differs from the module's
    /// recorded source. The CLI treats an explicit `--repo` as confirmation.
    pub confirm_repository: Option<&'a str>,
    pub private: bool,
    pub fork: bool,
    pub message: Option<&'a str>,
}

/// A publish may silently reuse the recorded source or an auto-created
/// repository. Pointing a module at a different repository requires an explicit
/// confirmation so a rewritten `.git/config` cannot redirect it.
fn repository_change_confirmed(
    trusted: Option<&str>,
    requested: &str,
    confirmed: Option<&str>,
) -> bool {
    let Some(trusted) = trusted else {
        return true;
    };
    requested.eq_ignore_ascii_case(trusted)
        || confirmed.is_some_and(|value| value.trim().eq_ignore_ascii_case(requested))
}

pub fn connection_path(paths: &Paths) -> PathBuf {
    paths.data_dir.join("github/connection.json")
}

pub fn load_connection(paths: &Paths) -> Result<Option<Connection>> {
    let path = connection_path(paths);
    if !path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read {}", path.display()))?;
    Ok(Some(serde_json::from_str(&raw)?))
}

fn save_connection(paths: &Paths, connection: &Connection) -> Result<()> {
    let raw = serde_json::to_vec_pretty(connection)?;
    let path = connection_path(paths);
    util::write_atomic(&path, &raw)?;
    util::restrict_file(&path)?;
    Ok(())
}

pub fn clear_connection(paths: &Paths) -> Result<()> {
    let path = connection_path(paths);
    if path.exists() {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

/// Remove the local connection and best-effort revoke the bridge token.
pub async fn disconnect(paths: &Paths) -> Result<()> {
    if let Ok(Some(connection)) = load_connection(paths) {
        let bridge = connection.bridge_url.trim_end_matches('/');
        if !bridge.is_empty() {
            let _ = client()?
                .post(format!("{bridge}/v1/revoke"))
                .bearer_auth(&connection.token)
                .send()
                .await;
        }
    }
    clear_connection(paths)
}

pub fn status(paths: &Paths, config: &GithubConfig) -> GithubStatus {
    match load_connection(paths) {
        Ok(Some(connection))
            if connection.bridge_url.trim_end_matches('/')
                == config.bridge_url.trim_end_matches('/') =>
        {
            GithubStatus {
                connected: true,
                bridge_url: config.bridge_url.clone(),
                account_login: Some(connection.account_login),
                account_type: Some(connection.account_type),
                installation_id: Some(connection.installation_id),
                connected_at: Some(connection.connected_at),
            }
        }
        _ => GithubStatus {
            connected: false,
            bridge_url: config.bridge_url.clone(),
            account_login: None,
            account_type: None,
            installation_id: None,
            connected_at: None,
        },
    }
}

fn client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(45))
        .build()
        .context("could not build the GitHub client")
}

fn bridge_base(config: &GithubConfig) -> Result<String> {
    let base = config.bridge_url.trim().trim_end_matches('/');
    if base.is_empty() {
        bail!("set a GitHub publisher bridge URL first");
    }
    if !base.starts_with("http://") && !base.starts_with("https://") {
        bail!("GitHub publisher bridge must use http:// or https://");
    }
    Ok(base.to_string())
}

async fn response_json<T: for<'de> Deserialize<'de>>(response: reqwest::Response) -> Result<T> {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("GitHub publisher bridge returned {status}: {text}");
    }
    serde_json::from_str(&text).context("invalid response from the GitHub publisher bridge")
}

pub async fn start_connect(config: &GithubConfig) -> Result<serde_json::Value> {
    let response = client()?
        .post(format!("{}/v1/sessions", bridge_base(config)?))
        .json(&serde_json::json!({ "client": "orqella-daemon" }))
        .send()
        .await
        .context("could not reach the GitHub publisher bridge")?;
    let session: SessionStart = response_json(response).await?;
    let now = Instant::now();
    if let Ok(mut pending) = PENDING_POLL_SECRETS.lock() {
        pending
            .retain(|_, (_, created)| now.duration_since(*created) < Duration::from_secs(15 * 60));
        pending.insert(session.id.clone(), (session.poll_secret, now));
    }
    Ok(serde_json::json!({
        "id": session.id,
        "connect_url": session.connect_url,
        "expires_at": session.expires_at,
    }))
}

pub async fn poll_connect(
    paths: &Paths,
    config: &GithubConfig,
    session_id: &str,
) -> Result<serde_json::Value> {
    let poll_secret = PENDING_POLL_SECRETS
        .lock()
        .ok()
        .and_then(|pending| pending.get(session_id).map(|(secret, _)| secret.clone()))
        .context("GitHub publisher session expired; start the connection again")?;
    let response = client()?
        .get(format!(
            "{}/v1/sessions/{}",
            bridge_base(config)?,
            urlencoding(session_id)
        ))
        .header("X-Orqella-Poll-Token", poll_secret)
        .send()
        .await
        .context("could not poll the GitHub publisher bridge")?;
    let session: SessionPoll = response_json(response).await?;
    if session.status != "connected" {
        return Ok(serde_json::json!({
            "status": session.status,
            "connected": false,
            "expires_at": session.expires_at,
        }));
    }
    let Some(token) = session.token else {
        bail!("GitHub bridge reported connected without a connection token");
    };
    let Some(account) = session.account else {
        bail!("GitHub bridge did not return the GitHub account");
    };
    let Some(installation_id) = session.installation_id else {
        bail!("GitHub bridge did not return an installation id");
    };
    let connection = Connection {
        bridge_url: bridge_base(config)?,
        token,
        account_login: account.login,
        account_type: account.kind,
        installation_id,
        connected_at: util::now_iso(),
    };
    save_connection(paths, &connection)?;
    if let Ok(mut pending) = PENDING_POLL_SECRETS.lock() {
        pending.remove(session_id);
    }
    Ok(serde_json::json!({
        "status": "connected",
        "connected": true,
        "account_login": connection.account_login,
        "account_type": connection.account_type,
        "installation_id": connection.installation_id,
    }))
}

fn connection_for(paths: &Paths, config: &GithubConfig) -> Result<Connection> {
    let Some(connection) = load_connection(paths)? else {
        bail!("connect a GitHub account first");
    };
    if connection.bridge_url.trim_end_matches('/') != bridge_base(config)? {
        bail!("GitHub connection belongs to a different publisher bridge; reconnect it");
    }
    Ok(connection)
}

async fn installation_token(connection: &Connection) -> Result<InstallationToken> {
    let response = client()?
        .post(format!(
            "{}/v1/token",
            connection.bridge_url.trim_end_matches('/')
        ))
        .bearer_auth(&connection.token)
        .send()
        .await
        .context("could not request a GitHub installation token")?;
    response_json(response).await
}

async fn create_repository(
    connection: &Connection,
    name: &str,
    private: bool,
) -> Result<Repository> {
    let response = client()?
        .post(format!(
            "{}/v1/repositories",
            connection.bridge_url.trim_end_matches('/')
        ))
        .bearer_auth(&connection.token)
        .json(&serde_json::json!({
            "name": name,
            "private": private,
            "description": "An Orqella module",
            "auto_init": false,
        }))
        .send()
        .await
        .context("could not create the GitHub repository")?;
    response_json(response).await
}

async fn create_fork(connection: &Connection, repository: &str) -> Result<Repository> {
    let response = client()?
        .post(format!(
            "{}/v1/forks",
            connection.bridge_url.trim_end_matches('/')
        ))
        .bearer_auth(&connection.token)
        .json(&serde_json::json!({ "repository": repository }))
        .send()
        .await
        .context("could not fork the GitHub repository")?;
    response_json(response).await
}

/// Return an installation token when this daemon is connected to GitHub.
/// Used to authenticate private module updates as well as publishing.
pub async fn token_for(paths: &Paths, config: &GithubConfig) -> Result<Option<String>> {
    let Some(connection) = load_connection(paths)? else {
        return Ok(None);
    };
    if connection.bridge_url.trim_end_matches('/') != bridge_base(config)? {
        return Ok(None);
    }
    let token = installation_token(&connection).await?;
    Ok(Some(token.token))
}

pub async fn publish(
    paths: &Paths,
    config: &GithubConfig,
    store: &ModuleStore,
    module_id: &str,
    options: PublishOptions<'_>,
) -> Result<PublishResult> {
    let PublishOptions {
        repository: requested_repository,
        confirm_repository,
        private,
        fork,
        message,
    } = options;
    let module = store
        .get(module_id)
        .with_context(|| format!("module {module_id} is not installed"))?;
    let connection = connection_for(paths, config)?;
    let root = module.root.clone();
    let status = tokio::task::spawn_blocking(move || git::status(&root))
        .await
        .context("module status worker failed")?;
    let requested_repository = requested_repository
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(repository) = requested_repository {
        validate_repository(repository)?;
    }
    let existing = status
        .repository
        .as_deref()
        .and_then(git::github_repository);
    // A module can write its own `.git/config`. Do not let a changed origin
    // silently redirect a publish (and its installation token) to a different
    // repository than the source recorded at install time. An explicit
    // repository/fork choice from the user remains the override.
    let recorded_source = module
        .state
        .source
        .strip_prefix("git:")
        .and_then(|value| value.split('#').next())
        .and_then(git::github_repository);
    if !fork
        && requested_repository.is_none()
        && let Some(recorded) = recorded_source.as_deref()
        && existing.as_deref() != Some(recorded)
    {
        bail!(
            "module git origin {} does not match its recorded source {}; pass the repository explicitly to publish",
            existing.as_deref().unwrap_or("(none)"),
            recorded
        );
    }
    let trusted_repository = recorded_source.as_deref().or(existing.as_deref());
    if !fork
        && let Some(repository) = requested_repository
        && !repository_change_confirmed(trusted_repository, repository, confirm_repository)
    {
        bail!(
            "publishing to {repository} instead of the recorded source {} requires retyping the repository to confirm",
            trusted_repository.unwrap_or("(none)")
        );
    }
    let mut created = false;
    let mut forked = false;
    let mut upstream_repository = None;
    let repository = if fork {
        let Some(upstream) = existing.clone() else {
            bail!("publishing a fork requires an existing GitHub remote");
        };
        validate_repository(&upstream)?;
        if requested_repository.is_some() {
            bail!(
                "fork publishing uses the module's existing GitHub remote; leave the repository field empty"
            );
        }
        if repository_owner(&upstream)
            .is_some_and(|owner| owner.eq_ignore_ascii_case(connection.account_login.as_str()))
        {
            bail!(
                "{upstream} already belongs to the connected GitHub account; publish without fork"
            );
        }
        let fork = create_fork(&connection, &upstream).await?;
        forked = true;
        upstream_repository = Some(upstream);
        fork.full_name
    } else if let Some(repository) = requested_repository {
        repository.to_string()
    } else if let Some(repository) = existing.clone() {
        repository
    } else {
        let name = default_repository_name(module_id);
        let repository = create_repository(&connection, &name, private).await?;
        created = true;
        repository.full_name
    };
    let mut branch = status
        .branch
        .clone()
        .filter(|branch| !branch.is_empty())
        .unwrap_or_else(|| git::DEFAULT_BRANCH.to_string());
    if created {
        branch = git::DEFAULT_BRANCH.to_string();
    }
    let commit_message = message
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "Update {} {}",
                module.manifest.name, module.manifest.version
            )
        });
    let url = format!("https://github.com/{repository}.git");
    let root = module.root.clone();
    let module_name = module.manifest.name.clone();
    let module_version = module.manifest.version.clone();
    let old_branch = status.branch.clone();
    let publish_branch = branch.clone();
    let origin_url = url.clone();
    let prepared = tokio::task::spawn_blocking(move || {
        git::ensure_repo(&root, &format!("Publish {module_name} {module_version}"))?;
        if git::status(&root).commit.is_none() {
            git::commit_all(&root, &format!("Publish {module_name} {module_version}"))?;
        }
        let committed = git::commit_all(&root, &commit_message)?;
        if created && old_branch.as_deref() != Some(&publish_branch) {
            git::run(&root, &["branch", "-M", &publish_branch])?;
        }
        git::set_origin(&root, &origin_url)?;
        let commit = git::status(&root).commit;
        Ok::<_, anyhow::Error>((committed, commit))
    })
    .await
    .context("publish git worker failed")??;
    let token = installation_token(&connection).await?;
    let token_text = token.token;
    let mut last_error = None;
    for attempt in 1..=8 {
        let root = module.root.clone();
        let push_branch = branch.clone();
        let push_token = token_text.clone();
        let pushed =
            tokio::task::spawn_blocking(move || git::push(&root, &push_token, &push_branch))
                .await
                .context("publish push worker failed")?;
        match pushed {
            Ok(()) => {
                last_error = None;
                break;
            }
            Err(error) => {
                let retry = forked && attempt < 8 && fork_not_ready(&error);
                last_error = Some(error);
                if !retry {
                    break;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    if let Some(error) = last_error {
        return Err(error);
    }
    let updated = store.set_repository(module_id, &url, &branch)?;
    Ok(PublishResult {
        module: updated,
        url: format!("https://github.com/{repository}"),
        repository,
        branch,
        commit: prepared.1,
        committed: prepared.0,
        created,
        forked,
        upstream_repository,
    })
}

fn repository_owner(repository: &str) -> Option<&str> {
    repository.split_once('/').map(|(owner, _)| owner)
}

fn fork_not_ready(error: &anyhow::Error) -> bool {
    let text = format!("{error:#}").to_ascii_lowercase();
    text.contains("not found") || text.contains("does not exist")
}

fn validate_repository(repository: &str) -> Result<()> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    if owner.is_empty() || name.is_empty() || parts.next().is_some() {
        bail!("repository must be in owner/name form");
    }
    if !owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        bail!("repository contains invalid characters");
    }
    Ok(())
}

pub fn default_repository_name(module_id: &str) -> String {
    let mut name = String::from("orqella-");
    for c in module_id.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            name.push(c.to_ascii_lowercase());
        } else {
            name.push('-');
        }
    }
    name.trim_end_matches('-').to_string()
}

fn urlencoding(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names_are_github_safe() {
        assert_eq!(
            default_repository_name("com.example.weather"),
            "orqella-com-example-weather"
        );
        assert!(validate_repository("acme/weather").is_ok());
        assert!(validate_repository("acme").is_err());
        assert!(validate_repository("../weather").is_err());
    }

    #[test]
    fn changing_the_publish_repository_requires_confirmation() {
        assert!(repository_change_confirmed(
            Some("acme/weather"),
            "acme/weather",
            None
        ));
        assert!(repository_change_confirmed(None, "other/weather", None));
        assert!(!repository_change_confirmed(
            Some("acme/weather"),
            "attacker/weather",
            None
        ));
        assert!(!repository_change_confirmed(
            Some("acme/weather"),
            "attacker/weather",
            Some("acme/weather")
        ));
        assert!(repository_change_confirmed(
            Some("acme/weather"),
            "attacker/weather",
            Some("Attacker/Weather")
        ));
    }

    #[test]
    fn fork_helpers_identify_owners_and_transient_failures() {
        assert_eq!(repository_owner("acme/weather"), Some("acme"));
        assert_eq!(repository_owner("weather"), None);
        assert!(fork_not_ready(&anyhow::anyhow!(
            "remote: Repository not found."
        )));
        assert!(!fork_not_ready(&anyhow::anyhow!("Authentication failed")));
    }

    fn paths(root: &std::path::Path) -> Paths {
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

    #[test]
    fn connection_round_trips_and_clears() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        assert!(load_connection(&paths).unwrap().is_none());
        let connection = Connection {
            bridge_url: "https://bridge.example/".into(),
            token: "secret-token".into(),
            account_login: "octocat".into(),
            account_type: "User".into(),
            installation_id: 42,
            connected_at: util::now_iso(),
        };
        save_connection(&paths, &connection).unwrap();
        let loaded = load_connection(&paths).unwrap().expect("connection");
        assert_eq!(loaded.account_login, "octocat");
        assert_eq!(loaded.installation_id, 42);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(connection_path(&paths))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        clear_connection(&paths).unwrap();
        assert!(load_connection(&paths).unwrap().is_none());
        clear_connection(&paths).unwrap();
    }

    #[test]
    fn status_requires_the_configured_bridge_url() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let mut config = GithubConfig {
            bridge_url: "https://bridge.example".into(),
        };
        assert!(!status(&paths, &config).connected);

        save_connection(
            &paths,
            &Connection {
                bridge_url: "https://bridge.example/".into(),
                token: "token".into(),
                account_login: "octocat".into(),
                account_type: "User".into(),
                installation_id: 7,
                connected_at: "now".into(),
            },
        )
        .unwrap();
        let connected = status(&paths, &config);
        assert!(connected.connected);
        assert_eq!(connected.account_login.as_deref(), Some("octocat"));

        config.bridge_url = "https://other.example".into();
        assert!(!status(&paths, &config).connected);
    }

    #[test]
    fn repository_validation_rejects_unsafe_names() {
        for repository in [
            "",
            "/",
            "owner/",
            "/name",
            "owner/name/extra",
            "own er/name",
            "owner/na me",
            "../name",
            "owner/../name",
        ] {
            assert!(validate_repository(repository).is_err(), "{repository}");
        }
        for repository in ["acme/weather", "Acme/weather_2", "a/b.c"] {
            assert!(validate_repository(repository).is_ok(), "{repository}");
        }
        assert_eq!(default_repository_name("---"), "orqella");
        assert_eq!(default_repository_name("A B/C"), "orqella-a-b-c");
    }

    #[test]
    fn urlencoding_escapes_reserved_characters() {
        assert_eq!(urlencoding("abc-_.~"), "abc-_.~");
        assert_eq!(urlencoding("a b/c?d=e"), "a%20b%2Fc%3Fd%3De");
        assert_eq!(urlencoding("é"), "%C3%A9");
    }
}
