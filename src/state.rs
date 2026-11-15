use crate::agent::BuildManager;
use crate::config::{Config, Paths};
use crate::link::{LinkManager, LinkStatus};
use crate::link_ssh::LinkSshServer;
use crate::modules::SidecarManager;
use crate::modules::signing;
use crate::modules::store::ModuleStore;
use crate::peers::PeerHealth;
use crate::util;
use crate::web::WebBridge;
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug)]
struct SessionRecord {
    expires: Instant,
    privileged: bool,
}

pub struct AppState {
    pub paths: Paths,
    pub config: RwLock<Arc<Config>>,
    pub modules: Arc<ModuleStore>,
    pub sidecars: Arc<SidecarManager>,
    pub builder: Arc<BuildManager>,
    pub link: RwLock<Option<Arc<LinkManager>>>,
    pub link_ssh: RwLock<Option<Arc<LinkSshServer>>>,
    pub web: RwLock<Option<Arc<WebBridge>>>,
    pub link_error: RwLock<Option<String>>,
    pub peer_health: RwLock<HashMap<String, PeerHealth>>,
    pub wake_cache: RwLock<crate::wake::WakeCache>,
    sessions: RwLock<HashMap<String, SessionRecord>>,
    ws_tokens: RwLock<HashMap<String, String>>,
    audit_lock: Mutex<()>,
    pub update_lock: tokio::sync::Mutex<()>,
    pub link_restart_lock: tokio::sync::Mutex<()>,
    pub dashboards_lock: tokio::sync::Mutex<()>,
    pub client: reqwest::Client,
    pub started: Instant,
    /// Per-daemon bearer for the browser WebRTC bridge. It is not the CLI
    /// admin token and cannot change the module trust store.
    pub bridge_token: String,
    /// Local CLI capability for trust-store changes and privileged local API
    /// mutations. It is stored outside the module sandbox and is never returned
    /// by the HTTP API.
    pub admin_token: String,
    /// One-time-ish local shell capability. `orqella open` places it in the URL
    /// fragment so a browser can exchange it for a privileged session without
    /// exposing it to other local apps, VM guests, or sandboxed sidecars.
    pub bootstrap_token: String,
}

impl AppState {
    pub async fn new(paths: Paths, mut config: Config) -> Result<Arc<Self>> {
        tighten_runtime_permissions(&paths);
        let modules = Arc::new(ModuleStore::load(paths.clone()).await?);
        let signing_key = signing::load_or_create_key(&paths)?;
        let public_key = signing::public_key_hex(&signing_key);
        if !config
            .trusted_module_keys
            .iter()
            .any(|key| key.public_key.eq_ignore_ascii_case(&public_key))
        {
            config.trusted_module_keys.push(crate::config::TrustedKey {
                public_key,
                label: "this machine".into(),
            });
            config.save(&paths)?;
        }
        modules.set_trusted_keys(config.trusted_module_keys.clone());
        let sidecars = Arc::new(SidecarManager::new(
            config.sidecar.clone(),
            paths.runtime_dir.clone(),
            paths.logs_dir.clone(),
            paths.data_dir.join("sidecar-venvs"),
            vec![paths.config_dir.clone(), paths.data_dir.clone()],
        ));
        let builder = Arc::new(BuildManager::new(
            paths.clone(),
            config.agent.clone(),
            Arc::clone(&sidecars),
        ));
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        let wake_cache = crate::wake::WakeCache::load(&paths.data_dir.join("wake.json"));
        let admin_token = load_or_create_admin_token(&paths)?;
        let bootstrap_token = load_or_create_bootstrap_token(&paths)?;
        let bridge_token = uuid::Uuid::new_v4().simple().to_string();
        let state = Arc::new(Self {
            paths,
            config: RwLock::new(Arc::new(config)),
            modules,
            sidecars,
            builder,
            link: RwLock::new(None),
            link_ssh: RwLock::new(None),
            web: RwLock::new(None),
            link_error: RwLock::new(None),
            peer_health: RwLock::new(HashMap::new()),
            wake_cache: RwLock::new(wake_cache),
            sessions: RwLock::new(HashMap::new()),
            ws_tokens: RwLock::new(HashMap::new()),
            audit_lock: Mutex::new(()),
            update_lock: tokio::sync::Mutex::new(()),
            link_restart_lock: tokio::sync::Mutex::new(()),
            dashboards_lock: tokio::sync::Mutex::new(()),
            client,
            started: Instant::now(),
            bridge_token,
            admin_token,
            bootstrap_token,
        });
        let link_config = state.config().link.clone();
        if link_config.enabled && link_config.token.is_some() {
            match restart_link(Arc::clone(&state)).await {
                Ok(()) => {}
                Err(error) => {
                    *state.link_error.write().expect("link error lock") =
                        Some(format!("{error:#}"));
                    tracing::warn!(%error, "could not start Link; retrying in the background");
                    spawn_link_retry(Arc::clone(&state));
                }
            }
        }
        if state.config().web.enabled {
            let web_secret =
                crate::link::load_or_create_secret(&state.paths.data_dir.join("web/iroh.key"))?;
            let web_endpoint_id = web_secret.public().to_string();
            let link = state.link.read().expect("link lock").clone();
            let relay = if let Some(link) = link {
                match link.web_relay_credentials(&web_endpoint_id, None).await {
                    Ok(credentials) => Some(credentials),
                    Err(error) => {
                        tracing::warn!(%error, "could not fetch managed relays for browser WebRTC bridge");
                        None
                    }
                }
            } else {
                None
            };
            match WebBridge::start(
                &state.paths,
                &state.config().web,
                state.config().server.port,
                web_secret,
                relay.as_ref(),
                state.bridge_token.clone(),
            )
            .await
            {
                Ok(web) => {
                    let web = Arc::new(web);
                    *state.web.write().expect("web bridge lock") = Some(web.clone());
                    let link = state.link.read().expect("link lock").clone();
                    if let Some(link) = link {
                        web.set_link_manager(link).await;
                    }
                    let refresh_state = Arc::clone(&state);
                    let refresh_web = web.clone();
                    tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                            let link = refresh_state.link.read().expect("link lock").clone();
                            let Some(link) = link else {
                                continue;
                            };
                            match link
                                .web_relay_credentials(refresh_web.endpoint_id(), None)
                                .await
                            {
                                Ok(credentials) => {
                                    if let Err(error) =
                                        refresh_web.install_relay_credentials(&credentials).await
                                    {
                                        tracing::warn!(%error, "could not refresh browser bridge relay credentials");
                                    }
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "could not refresh browser bridge relay credentials");
                                }
                            }
                        }
                    });
                }
                Err(error) => {
                    tracing::warn!(%error, "could not start browser WebRTC bridge");
                }
            }
        }
        Ok(state)
    }

    pub fn link_status(&self) -> LinkStatus {
        let config = self.config();
        self.link
            .read()
            .expect("link lock")
            .as_ref()
            .map(|link| link.status())
            .unwrap_or_else(|| LinkStatus {
                enabled: false,
                device_id: config.link.device_id.clone().unwrap_or_default(),
                device_name: config.link.device_name.clone().unwrap_or_default(),
                endpoint_id: String::new(),
                direct_addresses: Vec::new(),
                ssh_port: config.link.ssh_port,
                ssh_user: config.link.ssh_user.clone().unwrap_or_default(),
                ssh_host_key_fingerprint: config.link.ssh_host_key_fingerprint.clone(),
                relay_active: false,
                ssh_ca_configured: false,
                link_entitled: false,
                link_plan: "free".into(),
                billing_url: String::new(),
            })
    }

    pub fn config(&self) -> Arc<Config> {
        self.config.read().expect("config lock").clone()
    }

    pub fn update_config(&self, change: impl FnOnce(&mut Config)) -> Result<()> {
        let mut guard = self.config.write().expect("config lock");
        let config = Arc::make_mut(&mut *guard);
        change(config);
        config.save(&self.paths)?;
        Ok(())
    }

    pub fn peer_url(&self, peer: &crate::config::PeerConfig) -> Option<String> {
        peer.url.clone()
    }

    pub fn wake_cache_fresh(&self, node: &str) -> bool {
        self.wake_cache
            .read()
            .expect("wake cache lock")
            .is_fresh(node)
    }

    pub fn wake_targets(&self, node: &str, name: &str) -> Vec<crate::wake::WakeTarget> {
        let interfaces = self
            .wake_cache
            .read()
            .expect("wake cache lock")
            .interfaces(node, name);
        crate::wake::matching_targets(&interfaces, &crate::wake::local_interfaces())
    }

    pub fn record_wake_interfaces(
        &self,
        node: &str,
        name: &str,
        interfaces: Vec<crate::wake::WakeInterface>,
    ) -> Result<()> {
        let mut cache = self.wake_cache.write().expect("wake cache lock");
        cache.record(node, name, interfaces);
        cache.save(&self.paths.data_dir.join("wake.json"))
    }

    pub fn audit(&self, action: &str, detail: Value) {
        if !self.config().audit.enabled {
            return;
        }
        let _guard = self.audit_lock.lock().expect("audit lock");
        let record = json!({
            "at": util::now_iso(),
            "action": action,
            "detail": detail,
        });
        let path = self.paths.data_dir.join("audit.jsonl");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = util::restrict_file(&path);
            use std::io::Write;
            let _ = writeln!(file, "{record}");
        }
        prune_audit(&path);
    }
}

impl AppState {
    /// Issue a browser session. Only a session minted through the local
    /// bootstrap capability may change modules, run agent jobs, or change
    /// daemon configuration; ordinary sessions are read-only.
    pub fn issue_session(&self, privileged: bool) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let now = Instant::now();
        let mut sessions = self.sessions.write().expect("session lock");
        sessions.retain(|_, record| record.expires > now);
        while sessions.len() >= 1024 {
            if let Some(key) = sessions.keys().next().cloned() {
                sessions.remove(&key);
            } else {
                break;
            }
        }
        sessions.insert(
            token.clone(),
            SessionRecord {
                expires: now + Duration::from_secs(24 * 60 * 60),
                privileged,
            },
        );
        token
    }

    /// Stable per-scope token for the module WebSocket bridge. Scopes are
    /// `module-id` for local modules and `node:module-id` for remote frames.
    pub fn ws_token(&self, scope: &str) -> String {
        let mut tokens = self.ws_tokens.write().expect("ws token lock");
        tokens
            .entry(scope.to_string())
            .or_insert_with(|| uuid::Uuid::new_v4().simple().to_string())
            .clone()
    }

    /// Rotate a module's WebSocket token when its identity/code changes.
    pub fn invalidate_ws_token(&self, scope: &str) {
        self.ws_tokens.write().expect("ws token lock").remove(scope);
    }

    pub fn session_valid(&self, token: &str) -> bool {
        self.session_privileged(token).is_some()
    }

    /// Return `Some(true)` for a privileged session, `Some(false)` for a
    /// read-only session, and `None` for an unknown/expired token.
    pub fn session_privileged(&self, token: &str) -> Option<bool> {
        if token.is_empty() {
            return None;
        }
        let now = Instant::now();
        let mut sessions = self.sessions.write().expect("session lock");
        sessions.retain(|_, record| record.expires > now);
        sessions.get(token).map(|record| record.privileged)
    }
}

/// Link startup can race the network after a reboot. Retry with backoff until
/// the daemon connects, the account is disabled, or a manual restart wins.
fn spawn_link_retry(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut delay = Duration::from_secs(5);
        loop {
            tokio::time::sleep(delay).await;
            if state.link.read().expect("link lock").is_some() {
                return;
            }
            let config = state.config().link.clone();
            if !config.enabled || config.token.as_deref().is_none_or(|token| token.is_empty()) {
                return;
            }
            match restart_link(Arc::clone(&state)).await {
                Ok(()) => {
                    tracing::info!("Link connected after startup retry");
                    return;
                }
                Err(error) => {
                    *state.link_error.write().expect("link error lock") =
                        Some(format!("{error:#}"));
                    tracing::warn!(
                        %error,
                        retry_in_secs = delay.as_secs(),
                        "Link retry failed"
                    );
                    delay = (delay * 2).min(Duration::from_secs(60));
                }
            }
        }
    });
}

pub async fn restart_link(state: Arc<AppState>) -> Result<()> {
    // A resume watchdog and the settings/account handlers can all request a
    // restart. Keep them from tearing down and rebuilding Link concurrently.
    let _restart = state.link_restart_lock.lock().await;
    let old = state.link.write().expect("link lock").take();
    if let Some(link) = old {
        link.shutdown().await;
    }
    let config = state.config().link.clone();
    if !config.enabled {
        return Ok(());
    }
    let Some(token) = config.token.filter(|token| !token.is_empty()) else {
        anyhow::bail!("Link account token is missing");
    };
    let name = config.device_name.unwrap_or_else(|| {
        hostname::get()
            .ok()
            .and_then(|name| name.into_string().ok())
            .unwrap_or_else(|| "orqella-device".into())
    });
    let ssh_user = config
        .ssh_user
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "orqella".into());
    let secret_path = state.paths.data_dir.join("link/iroh.key");
    let peer_token = state.config().peer_token.clone().unwrap_or_default();
    let old_ssh = state.link_ssh.write().expect("link ssh lock").take();
    if let Some(old) = old_ssh {
        old.stop();
    }
    #[cfg(unix)]
    let dashboard_socket = Some(state.paths.runtime_dir.join("orqella.sock"));
    #[cfg(not(unix))]
    let dashboard_socket = None;
    let link_ssh = LinkSshServer::start(
        &state.paths.data_dir,
        dashboard_socket,
        state.config().server.port,
    )
    .await?;
    let embedded_ssh_port = link_ssh.port();
    let embedded_ssh_fingerprint = link_ssh.fingerprint().to_string();
    let web_endpoint_id = if state.config().web.enabled {
        WebBridge::prepared_endpoint_id(&state.paths).unwrap_or_default()
    } else {
        String::new()
    };
    let manager = LinkManager::start(
        token,
        config.device_id,
        name,
        ssh_user,
        embedded_ssh_port,
        embedded_ssh_fingerprint,
        peer_token,
        state.config().server.port,
        web_endpoint_id,
        &secret_path,
    )
    .await?;
    let device_id = manager.status().device_id;
    link_ssh.set_target_device_id(device_id.clone());
    if let Some(ca) = manager.ssh_ca_public_key() {
        link_ssh.set_ca_public_key(ca);
    }
    *state.link_ssh.write().expect("link ssh lock") = Some(Arc::new(link_ssh));
    let device_token = manager.token().to_string();
    let link_peers = manager.peers();
    state.update_config(|config| {
        config.link.device_id = Some(device_id);
        config.link.token = Some(device_token);
        config.peers.clear();
        for peer in link_peers {
            config.peers.push(crate::config::PeerConfig {
                id: format!("link-{}", peer.id),
                name: peer.name,
                url: Some(peer.url),
                token: peer.token,
            });
        }
    })?;
    *state.link.write().expect("link lock") = Some(Arc::new(manager));
    *state.link_error.write().expect("link error lock") = None;
    let web = state.web.read().expect("web bridge lock").clone();
    let link = state.link.read().expect("link lock").clone();
    if let (Some(web), Some(link)) = (web, link) {
        web.set_link_manager(link.clone()).await;
        if let Err(error) = web.install_managed_relay(&link).await {
            tracing::warn!(%error, "could not install managed relay on browser WebRTC bridge");
        }
    }
    Ok(())
}

pub async fn stop_link(state: Arc<AppState>) {
    let _restart = state.link_restart_lock.lock().await;
    let old = state.link.write().expect("link lock").take();
    if let Some(link) = old {
        link.shutdown().await;
    }
    if let Some(ssh) = state.link_ssh.write().expect("link ssh lock").take() {
        ssh.stop();
    }
    if !state.config().peers.is_empty() {
        let _ = state.update_config(|config| {
            config.peers.clear();
        });
    }
}

fn load_or_create_admin_token(paths: &Paths) -> Result<String> {
    load_or_create_token(&paths.data_dir.join("admin.token"))
}

fn load_or_create_bootstrap_token(paths: &Paths) -> Result<String> {
    load_or_create_token(&paths.data_dir.join("bootstrap.token"))
}

fn load_or_create_token(path: &std::path::Path) -> Result<String> {
    if path.is_file() {
        let _ = util::restrict_file(path);
        let token = std::fs::read_to_string(path)?.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    let token = uuid::Uuid::new_v4().simple().to_string();
    util::write_atomic(path, token.as_bytes())?;
    Ok(token)
}

fn tighten_runtime_permissions(paths: &Paths) {
    for path in [
        paths.data_dir.join("audit.jsonl"),
        paths.data_dir.join("update.json"),
        paths.data_dir.join("update.log"),
        paths.data_dir.join("wake.json"),
        paths.data_dir.join("module-signing.key"),
        paths.data_dir.join("link/iroh.key"),
        paths.data_dir.join("web/iroh.key"),
        paths.config_file.clone(),
        paths.data_dir.join("admin.token"),
        paths.data_dir.join("bootstrap.token"),
    ] {
        if path.is_file() {
            let _ = util::restrict_file(&path);
        }
    }
    if let Ok(entries) = std::fs::read_dir(&paths.logs_dir) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                let _ = util::restrict_file(&entry.path());
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(paths.data_dir.join("module-data")) {
        for entry in entries.flatten() {
            if entry.path().is_file() {
                let _ = util::restrict_file(&entry.path());
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(&paths.modules_dir) {
        for entry in entries.flatten() {
            let log = entry.path().join("sidecar.log");
            if std::fs::symlink_metadata(&log).is_ok_and(|metadata| metadata.file_type().is_file())
            {
                let _ = util::restrict_file(&log);
            }
        }
    }
}

fn prune_audit(path: &std::path::Path) {
    const MAX_BYTES: u64 = 2 * 1024 * 1024;
    const KEEP_LINES: usize = 2_000;
    let Ok(metadata) = std::fs::metadata(path) else {
        return;
    };
    if metadata.len() <= MAX_BYTES {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = raw.lines().collect();
    let start = lines.len().saturating_sub(KEEP_LINES);
    let mut kept = lines[start..].join("\n");
    kept.push('\n');
    let _ = crate::util::write_atomic(path, kept.as_bytes());
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

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
    fn admin_token_is_created_once_private_and_stable() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let first = load_or_create_admin_token(&paths).unwrap();
        assert!(first.len() >= 32);
        let second = load_or_create_admin_token(&paths).unwrap();
        assert_eq!(first, second);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(paths.data_dir.join("admin.token"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn blank_admin_token_is_replaced() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        std::fs::write(paths.data_dir.join("admin.token"), "  \n").unwrap();
        let token = load_or_create_admin_token(&paths).unwrap();
        assert!(!token.trim().is_empty());
        assert_eq!(
            std::fs::read_to_string(paths.data_dir.join("admin.token"))
                .unwrap()
                .trim(),
            token
        );
    }

    #[test]
    fn audit_pruning_keeps_the_newest_lines_and_bounds_the_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("audit.jsonl");
        let line = "x".repeat(900);
        let mut raw = String::new();
        for index in 0..2500 {
            raw.push_str(&format!("{index:04} {line}\n"));
        }
        std::fs::write(&path, raw).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 2 * 1024 * 1024);
        prune_audit(&path);
        let pruned = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = pruned.lines().collect();
        assert_eq!(lines.len(), 2000);
        assert!(lines.last().unwrap().starts_with("2499 "));
    }

    #[test]
    fn audit_pruning_ignores_small_missing_and_binary_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("audit.jsonl");
        prune_audit(&path);
        std::fs::write(&path, "small\n").unwrap();
        prune_audit(&path);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "small\n");
        std::fs::write(&path, [0xff, 0xfe, 0x00]).unwrap();
        std::fs::write(&path, vec![0xff_u8; 3 * 1024 * 1024]).unwrap();
        prune_audit(&path);
        // Invalid UTF-8 is left untouched rather than truncated to garbage.
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 3 * 1024 * 1024);
    }

    #[tokio::test]
    async fn app_state_starts_private_and_records_sessions_and_wake_data() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let mut config = Config::default();
        config.audit.enabled = true;
        let state = AppState::new(paths.clone(), config).await.unwrap();

        let session = state.issue_session(false);
        assert!(state.session_valid(&session));
        assert_eq!(state.session_privileged(&session), Some(false));
        let privileged = state.issue_session(true);
        assert_eq!(state.session_privileged(&privileged), Some(true));
        assert!(!state.session_valid(""));
        assert!(!state.session_valid("missing"));
        assert_eq!(state.ws_token("module"), state.ws_token("module"));
        assert_ne!(state.ws_token("module"), state.ws_token("other"));

        state.audit("test.event", json!({"value": 1}));
        let audit = std::fs::read_to_string(paths.data_dir.join("audit.jsonl")).unwrap();
        assert!(audit.contains("test.event"));
        assert!(audit.contains("\"value\":1"));

        let interface = crate::wake::WakeInterface {
            name: "enp6s0".into(),
            mac: "aa:bb:cc:dd:ee:ff".into(),
            ip: "192.168.68.10".parse().unwrap(),
            netmask: "255.255.255.0".parse().unwrap(),
            broadcast: "192.168.68.255".parse().unwrap(),
        };
        state
            .record_wake_interfaces("link-peer", "Peer", vec![interface])
            .unwrap();
        assert!(paths.data_dir.join("wake.json").is_file());
        assert!(state.wake_cache_fresh("link-peer"));

        state
            .update_config(|config| config.theme = "signal".into())
            .unwrap();
        assert_eq!(state.config().theme, "signal");
        let saved = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(saved.contains("theme = \"signal\""));
    }

    #[tokio::test]
    async fn stopping_an_unstarted_link_is_safe_and_resets_peers() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        let mut config = Config::default();
        config.peers.push(crate::config::PeerConfig {
            id: "link-test".into(),
            name: "Test".into(),
            url: None,
            token: "t".into(),
        });
        let state = AppState::new(paths, config).await.unwrap();
        stop_link(Arc::clone(&state)).await;
        assert!(state.config().peers.is_empty());
        assert!(!state.link_status().relay_active);
    }
}
