use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_PORT: u16 = 9477;
pub const DEFAULT_HOST: &str = "orqella.localhost";

/// Current on-disk config format. Increment when adding a one-time migration.
const CONFIG_VERSION: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// On-disk config format version. Orqella uses this to run one-time
    /// migrations; it is not a user setting.
    #[serde(default)]
    pub config_version: u32,
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default = "default_proxy_mounts")]
    pub proxy: Vec<ProxyConfig>,
    /// Link-managed remote machines. This is populated by the Link manager and
    /// is not user-configurable.
    #[serde(default)]
    pub peers: Vec<PeerConfig>,
    #[serde(default)]
    pub link: LinkConfig,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub audit: AuditConfig,
    #[serde(default)]
    pub sidecar: SidecarPolicy,
    #[serde(default)]
    pub trusted_module_keys: Vec<TrustedKey>,
    #[serde(default)]
    pub onboarding: OnboardingConfig,
    #[serde(default)]
    pub update: UpdateConfig,
    #[serde(default = "default_theme")]
    pub theme: String,
    #[serde(default)]
    pub peer_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_bind")]
    pub bind: String,
    #[serde(default = "default_true")]
    pub dashboard: bool,
    #[serde(default = "default_canonical_host")]
    pub canonical_host: String,
    /// Explicitly allow binding to a non-loopback address. Remote exposure
    /// still has no authentication and is intended only behind a trusted
    /// private network or tunnel.
    #[serde(default)]
    pub allow_remote: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AgentViewerConfig {
    /// `module` for an Orqella module page, `app` for a configured proxy
    /// mount, `url` for an external conversation URL, or empty to infer.
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub module: String,
    #[serde(default)]
    pub mount: String,
    /// Path template for an app viewer. `{session}` is replaced with the
    /// conversation id; defaults to `/`.
    #[serde(default)]
    pub path: String,
    /// URL template for an external viewer. `{session}` is replaced with the
    /// conversation id.
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub title: String,
}

impl AgentViewerConfig {
    fn is_empty(&self) -> bool {
        self.kind.trim().is_empty()
            && self.module.trim().is_empty()
            && self.mount.trim().is_empty()
            && self.path.trim().is_empty()
            && self.url.trim().is_empty()
            && self.title.trim().is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    /// Metadata for the Settings picker. Command and args remain authoritative.
    #[serde(default)]
    pub preset: String,
    #[serde(default = "default_agent_command")]
    pub command: String,
    #[serde(default = "default_agent_args")]
    pub args: Vec<String>,
    /// Extra environment for the agent CLI. Values may use `${VAR}` to read a
    /// secret from the daemon environment instead of storing it in config.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Stop the agent process after this many seconds. `0` (the default)
    /// leaves it running until it exits, so long module builds are not
    /// reported as failed while the harness is still working.
    #[serde(default = "default_agent_timeout")]
    pub timeout_secs: u64,
    /// Where to open the conversation created by the agent.
    #[serde(default)]
    pub viewer: AgentViewerConfig,
}

impl AgentConfig {
    pub fn effective_preset(&self) -> String {
        let preset = self.preset.trim();
        if !preset.is_empty() {
            return preset.to_string();
        }
        let name = Path::new(&self.command)
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match name.as_str() {
            "opencode" => "opencode",
            "codex" => "codex",
            "claude" => "claude",
            _ => "custom",
        }
        .to_string()
    }

    /// Resolve the configured conversation viewer, falling back to the
    /// OpenCode module page for the OpenCode preset.
    pub fn resolved_viewer(&self) -> Option<AgentViewerConfig> {
        if !self.viewer.is_empty() {
            let mut viewer = self.viewer.clone();
            viewer.kind = viewer.kind.trim().to_string();
            viewer.module = viewer.module.trim().to_string();
            viewer.mount = viewer.mount.trim().to_string();
            viewer.path = viewer.path.trim().to_string();
            viewer.url = viewer.url.trim().to_string();
            viewer.title = viewer.title.trim().to_string();
            if viewer.kind.eq_ignore_ascii_case("none") {
                return None;
            }
            let kind = if !viewer.kind.is_empty() {
                viewer.kind.to_ascii_lowercase()
            } else if !viewer.url.is_empty() {
                "url".to_string()
            } else if !viewer.module.is_empty() {
                "module".to_string()
            } else if !viewer.mount.is_empty() {
                "app".to_string()
            } else {
                return None;
            };
            if kind == "url" {
                if viewer.url.is_empty() {
                    return None;
                }
                viewer.kind = "url".to_string();
                if viewer.title.is_empty() {
                    viewer.title = "Conversation".to_string();
                }
                return Some(viewer);
            }
            if kind == "module" {
                if viewer.module.is_empty() {
                    return None;
                }
                viewer.kind = "module".to_string();
                if viewer.path.is_empty() {
                    viewer.path = "/".to_string();
                }
                if viewer.title.is_empty() {
                    viewer.title = viewer.module.clone();
                }
                return Some(viewer);
            }
            if kind == "app" {
                if viewer.mount.is_empty() {
                    return None;
                }
                viewer.kind = "app".to_string();
                if viewer.path.is_empty() {
                    viewer.path = "/".to_string();
                }
                if viewer.title.is_empty() {
                    viewer.title = viewer.mount.clone();
                }
                return Some(viewer);
            }
            return None;
        }
        // Preset defaults point at the Agent Harness module, which owns the
        // per-harness registry, session index, and conversation UI. The path
        // carries the preset id so the module can route without guessing.
        let preset = self.effective_preset();
        let (title, path) = match preset.as_str() {
            "opencode" => (
                "OpenCode",
                "/?harness=opencode&session={session}&cwd={root}".to_string(),
            ),
            "codex" => (
                "Codex",
                "/?harness=codex&session={session}&cwd={root}".to_string(),
            ),
            "claude" => (
                "Claude Code",
                "/?harness=claude&session={session}&cwd={root}".to_string(),
            ),
            "gemini" => (
                "Gemini CLI",
                "/?harness=gemini&session={session}&cwd={root}".to_string(),
            ),
            "custom" => ("Conversation", "/?session={session}&cwd={root}".to_string()),
            _ => return None,
        };
        Some(AgentViewerConfig {
            kind: "module".to_string(),
            module: "agent-harness".to_string(),
            mount: String::new(),
            path,
            title: title.to_string(),
            url: String::new(),
        })
    }

    /// Resolve `${VAR}` and `$VAR` references from the daemon environment.
    /// Missing variables are omitted so the CLI can fall back to its own auth.
    pub fn resolved_env(&self) -> Vec<(String, String)> {
        self.env
            .iter()
            .filter_map(|(key, value)| {
                let value = value.trim();
                let name = value
                    .strip_prefix("${")
                    .and_then(|value| value.strip_suffix('}'))
                    .or_else(|| value.strip_prefix('$'))
                    .filter(|name| !name.is_empty());
                match name {
                    Some(name) => std::env::var(name).ok().map(|value| (key.clone(), value)),
                    None => Some((key.clone(), value.to_string())),
                }
            })
            .collect()
    }
}

pub fn agent_presets() -> Vec<serde_json::Value> {
    vec![
        serde_json::json!({
            "id": "opencode",
            "name": "OpenCode",
            "command": "opencode",
            "args": ["run", "--auto", "--format", "json"],
            "hint": "Default. Uses OpenCode's configured providers."
        }),
        serde_json::json!({
            "id": "codex",
            "name": "Codex CLI",
            "command": "codex",
            "args": ["-a", "never", "exec", "--skip-git-repo-check", "--json", "--sandbox", "workspace-write"],
            "hint": "OpenAI Codex CLI. Use --oss/--local-provider or its config for custom providers."
        }),
        serde_json::json!({
            "id": "claude",
            "name": "Claude Code",
            "command": "claude",
            "args": ["-p", "--dangerously-skip-permissions", "--output-format", "text"],
            "hint": "Claude Code non-interactive. Supports ANTHROPIC_BASE_URL/ANTHROPIC_AUTH_TOKEN."
        }),
        serde_json::json!({
            "id": "custom",
            "name": "Custom harness",
            "command": "",
            "args": [],
            "hint": "Any CLI harness. Prompt is appended as the final arg, or use {prompt} in an argument."
        }),
    ]
}

pub fn preset_defaults(preset: &str) -> Option<(&'static str, Vec<String>)> {
    match preset {
        "opencode" => Some((
            "opencode",
            vec![
                "run".into(),
                "--auto".into(),
                "--format".into(),
                "json".into(),
            ],
        )),
        "codex" => Some((
            "codex",
            vec![
                "-a".into(),
                "never".into(),
                "exec".into(),
                "--skip-git-repo-check".into(),
                "--json".into(),
                "--sandbox".into(),
                "workspace-write".into(),
            ],
        )),
        "claude" => Some((
            "claude",
            vec![
                "-p".into(),
                "--dangerously-skip-permissions".into(),
                "--output-format".into(),
                "text".into(),
            ],
        )),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

impl Default for AuditConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedKey {
    pub public_key: String,
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SidecarPolicy {
    #[serde(default = "default_sidecar_sandbox")]
    pub sandbox: bool,
    #[serde(default = "default_sidecar_memory")]
    pub max_memory_mb: u64,
}

impl Default for SidecarPolicy {
    fn default() -> Self {
        Self {
            sandbox: default_sidecar_sandbox(),
            max_memory_mb: default_sidecar_memory(),
        }
    }
}

fn default_sidecar_sandbox() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "macos",
        target_os = "windows"
    ))
}
fn default_sidecar_memory() -> u64 {
    8192
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    #[serde(default)]
    pub repo: String,
    #[serde(default = "default_update_remote")]
    pub remote: String,
    #[serde(default = "default_update_branch")]
    pub branch: String,
    #[serde(default)]
    pub installer: String,
    /// Minisign public key (base64 or a `minisign.pub` path) that must sign the
    /// update manifest. Official builds bake this in with
    /// `ORQELLA_UPDATE_PUBLIC_KEY`; local source builds can set it here.
    #[serde(default)]
    pub public_key: String,
    /// Explicitly accept an unsigned checkout. This exists for local source
    /// development only and is refused for remote update requests.
    #[serde(default)]
    pub allow_unsigned: bool,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            repo: String::new(),
            remote: default_update_remote(),
            branch: default_update_branch(),
            installer: String::new(),
            public_key: String::new(),
            allow_unsigned: false,
        }
    }
}

fn default_update_remote() -> String {
    "origin".into()
}

fn default_update_branch() -> String {
    "main".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OnboardingConfig {
    #[serde(default)]
    pub completed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinkConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub device_id: Option<String>,
    #[serde(default)]
    pub device_name: Option<String>,
    /// Fixed local UDP port for the Link iroh endpoint. `0` asks the OS for an
    /// ephemeral port. A fixed port keeps direct-only connectivity stable across
    /// restarts and makes any manual firewall rule predictable.
    #[serde(default)]
    pub port: u16,
    #[serde(default = "default_link_ssh_port")]
    pub ssh_port: u16,
    #[serde(default)]
    pub ssh_user: Option<String>,
    #[serde(default)]
    pub ssh_host_key_fingerprint: String,
}

impl Default for LinkConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: None,
            device_id: None,
            device_name: None,
            port: 0,
            ssh_port: default_link_ssh_port(),
            ssh_user: None,
            ssh_host_key_fingerprint: String::new(),
        }
    }
}

fn default_link_ssh_port() -> u16 {
    22
}

/// Browser WebRTC transport settings.
///
/// The bridge is opt-in. It never opens a public TCP port; clients connect
/// through an iroh endpoint and authenticate with an Orqella Link browser
/// session before any local HTTP or WebSocket request is proxied.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Optional STUN URLs for the bridge's WebRTC path.
    #[serde(default)]
    pub stun_urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyConfig {
    pub id: String,
    #[serde(default)]
    pub title: String,
    pub base_url: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default = "default_true")]
    pub websocket: bool,
    /// Allow this upstream to be framed by the Orqella app surface. Only use
    /// this for a trusted app that sends X-Frame-Options/CSP frame-ancestors.
    #[serde(default)]
    pub frame: bool,
}

/// A Link-managed remote machine. `url` is the loopback forward created by the
/// Link manager; it is not a user-configured direct endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerConfig {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub token: String,
}

#[derive(Debug, Clone)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub config_file: PathBuf,
    pub state_file: PathBuf,
    pub modules_dir: PathBuf,
    pub workshop_dir: PathBuf,
    pub logs_dir: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            config_version: CONFIG_VERSION,
            server: ServerConfig::default(),
            agent: AgentConfig::default(),
            proxy: default_proxy_mounts(),
            peers: Vec::new(),
            link: LinkConfig::default(),
            web: WebConfig::default(),
            audit: AuditConfig::default(),
            sidecar: SidecarPolicy::default(),
            trusted_module_keys: Vec::new(),
            onboarding: OnboardingConfig::default(),
            update: UpdateConfig::default(),
            theme: default_theme(),
            peer_token: None,
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            port: DEFAULT_PORT,
            bind: default_bind(),
            dashboard: true,
            canonical_host: default_canonical_host(),
            allow_remote: false,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            preset: "opencode".into(),
            command: default_agent_command(),
            args: default_agent_args(),
            env: BTreeMap::new(),
            timeout_secs: default_agent_timeout(),
            viewer: AgentViewerConfig::default(),
        }
    }
}

fn default_port() -> u16 {
    DEFAULT_PORT
}
fn default_bind() -> String {
    "127.0.0.1".into()
}
fn default_true() -> bool {
    true
}
fn default_canonical_host() -> String {
    DEFAULT_HOST.into()
}
fn default_theme() -> String {
    crate::theme::default_theme().into()
}
fn default_agent_command() -> String {
    "opencode".into()
}
fn default_agent_args() -> Vec<String> {
    vec![
        "run".into(),
        "--auto".into(),
        "--format".into(),
        "json".into(),
    ]
}
fn default_agent_timeout() -> u64 {
    0
}

fn default_proxy_mounts() -> Vec<ProxyConfig> {
    Vec::new()
}

impl Paths {
    pub fn discover() -> Result<Self> {
        if let Some(root) = std::env::var_os("ORQELLA_HOME") {
            let root = PathBuf::from(root);
            return Self::from_root(&root);
        }
        let config_dir = dirs::config_dir()
            .context("could not find the platform config directory")?
            .join("orqella");
        let data_dir = dirs::data_local_dir()
            .context("could not find the platform data directory")?
            .join("orqella");
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::temp_dir().join("orqella-runtime"))
            .join("orqella");
        Self::from_parts(config_dir, data_dir, runtime_dir)
    }

    /// Portable layout used by `ORQELLA_HOME` and tests:
    /// `<root>/config`, `<root>/data`, and `<root>/run`.
    pub fn from_root(root: &Path) -> Result<Self> {
        Self::from_parts(root.join("config"), root.join("data"), root.join("run"))
    }

    fn from_parts(config_dir: PathBuf, data_dir: PathBuf, runtime_dir: PathBuf) -> Result<Self> {
        let paths = Self {
            config_file: config_dir.join("config.toml"),
            state_file: data_dir.join("state.json"),
            modules_dir: data_dir.join("modules"),
            workshop_dir: data_dir.join("workshop"),
            logs_dir: data_dir.join("logs"),
            config_dir,
            data_dir,
            runtime_dir,
        };
        paths.ensure()?;
        Ok(paths)
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.modules_dir,
            &self.workshop_dir,
            &self.logs_dir,
            &self.runtime_dir,
        ] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
            let _ = crate::util::restrict_dir(dir);
        }
        Ok(())
    }

    pub fn module_dir(&self, id: &str) -> PathBuf {
        self.modules_dir.join(id)
    }
}

impl Config {
    pub fn load_or_create(paths: &Paths) -> Result<Self> {
        if !paths.config_file.exists() {
            let mut config = Self::default();
            config.ensure_secrets();
            config.save(paths)?;
            return Ok(config);
        }
        let _ = crate::util::restrict_file(&paths.config_file);
        let raw = std::fs::read_to_string(&paths.config_file)
            .with_context(|| format!("could not read {}", paths.config_file.display()))?;
        let mut config: Self = toml::from_str(&raw)
            .with_context(|| format!("invalid config at {}", paths.config_file.display()))?;
        let mut changed = config.ensure_secrets();
        if let Some(canonical) = crate::theme::canonical(&config.theme)
            && canonical != config.theme
        {
            config.theme = canonical.to_string();
            changed = true;
        }
        let removed = config.prune_legacy_peers();
        if removed > 0 {
            tracing::warn!(
                count = removed,
                "removed legacy direct/SSH peers; remote machines are managed by Link"
            );
            changed = true;
        }
        if config.config_version < CONFIG_VERSION {
            if config.migrate_legacy_agent_timeout() {
                tracing::info!(
                    "disabled the legacy 15-minute agent timeout; set [agent].timeout_secs to a positive value to re-enable"
                );
            }
            config.config_version = CONFIG_VERSION;
            changed = true;
        }
        if changed {
            config.save(paths)?;
        }
        Ok(config)
    }

    pub fn save(&self, paths: &Paths) -> Result<()> {
        paths.ensure()?;
        let raw = toml::to_string_pretty(self).context("could not serialize config")?;
        crate::util::write_atomic(&paths.config_file, raw.as_bytes())?;
        crate::util::restrict_file(&paths.config_file)?;
        Ok(())
    }

    fn ensure_secrets(&mut self) -> bool {
        if self.peer_token.as_deref().unwrap_or_default().is_empty() {
            self.peer_token = Some(uuid::Uuid::new_v4().simple().to_string());
            true
        } else {
            false
        }
    }

    /// Remove peers that were added through the retired direct-URL or SSH
    /// configuration paths. Link is the only supported remote-machine
    /// transport, and it writes peers with a `link-` prefix.
    pub fn prune_legacy_peers(&mut self) -> usize {
        let before = self.peers.len();
        self.peers.retain(|peer| peer.id.starts_with("link-"));
        before - self.peers.len()
    }

    /// The old default stopped every agent job after 15 minutes, which could
    /// report a failure while a service-backed harness kept working. Treat an
    /// unmodified legacy value as the new "no timeout" default.
    fn migrate_legacy_agent_timeout(&mut self) -> bool {
        if self.agent.timeout_secs == 900 {
            self.agent.timeout_secs = 0;
            return true;
        }
        false
    }

    pub fn peer_token(&self) -> &str {
        self.peer_token.as_deref().unwrap_or_default()
    }

    pub fn proxy(&self, id: &str) -> Option<&ProxyConfig> {
        self.proxy.iter().find(|mount| mount.id == id)
    }

    pub fn canonical_host(&self) -> &str {
        self.server.canonical_host.trim_end_matches('.')
    }

    pub fn origin(&self) -> String {
        format!("http://{}:{}", self.canonical_host(), self.server.port)
    }

    /// A loopback URL that works without `/etc/hosts` or browser-specific
    /// `*.localhost` resolution. Use the configured bind when it is a
    /// loopback address; otherwise fall back to `127.0.0.1`.
    pub fn loopback_origin(&self) -> String {
        let bind = self.server.bind.trim();
        let host = match bind.parse::<std::net::IpAddr>() {
            Ok(std::net::IpAddr::V4(ip)) if ip.is_loopback() => ip.to_string(),
            Ok(std::net::IpAddr::V6(ip)) if ip.is_loopback() => format!("[{ip}]"),
            _ => "127.0.0.1".into(),
        };
        format!("http://{host}:{}/", self.server.port)
    }

    fn redacted_agent_env(&self) -> BTreeMap<String, String> {
        self.agent
            .env
            .iter()
            .map(|(key, value)| {
                let value = if value.trim().starts_with("${") || value.trim().starts_with('$') {
                    value.clone()
                } else {
                    "[redacted]".to_string()
                };
                (key.clone(), value)
            })
            .collect()
    }

    fn redacted_agent_args(&self) -> Vec<String> {
        let mut redact_next = false;
        self.agent
            .args
            .iter()
            .map(|arg| {
                if redact_next {
                    redact_next = false;
                    return "[redacted]".to_string();
                }
                let lower = arg.to_ascii_lowercase();
                let sensitive_flag = [
                    "--api-key",
                    "--apikey",
                    "--token",
                    "--access-token",
                    "--auth-token",
                    "--password",
                    "--secret",
                    "--client-secret",
                ]
                .iter()
                .any(|flag| lower == *flag);
                if sensitive_flag {
                    redact_next = true;
                    return arg.clone();
                }
                if let Some((flag, _value)) = arg.split_once('=')
                    && [
                        "--api-key",
                        "--apikey",
                        "--token",
                        "--access-token",
                        "--auth-token",
                        "--password",
                        "--secret",
                        "--client-secret",
                    ]
                    .iter()
                    .any(|sensitive| flag.eq_ignore_ascii_case(sensitive))
                {
                    return format!("{flag}=[redacted]");
                }
                arg.clone()
            })
            .collect()
    }

    pub fn config_summary(&self) -> serde_json::Value {
        serde_json::json!({
            "port": self.server.port,
            "bind": self.server.bind,
            "dashboard": self.server.dashboard,
            "allow_remote": self.server.allow_remote,
            "canonical_host": self.canonical_host(),
            "origin": self.origin(),
            "proxy": self
                .proxy
                .iter()
                .map(|mount| {
                    let mut mount = mount.clone();
                    mount.base_url = redact_url_userinfo(&mount.base_url);
                    mount
                })
                .collect::<Vec<_>>(),
            "peers": self.peers.iter().map(|p| serde_json::json!({
                "id": p.id,
                "name": p.name,
                "transport": "link",
            })).collect::<Vec<_>>(),
            "audit": self.audit,
            "sidecar": self.sidecar,
            "onboarding": self.onboarding,
            "update": self.update,
            "theme": self.theme.clone(),
            "web": {
                "enabled": self.web.enabled,
                "stun_urls": self.web.stun_urls,
            },
            "link": {
                "enabled": self.link.enabled,
                "device_id": self.link.device_id,
                "device_name": self.link.device_name,
                "ssh_port": self.link.ssh_port,
                "ssh_user": self.link.ssh_user,
                "ssh_host_key_fingerprint": self.link.ssh_host_key_fingerprint,
            },
            "agent": {
                "preset": self.agent.effective_preset(),
                "command": self.agent.command,
                "args": self.redacted_agent_args(),
                "env": self.redacted_agent_env(),
                "timeout_secs": self.agent.timeout_secs,
                "viewer": self.agent.viewer,
                "resolved_viewer": self.agent.resolved_viewer(),
            },
            "agent_presets": agent_presets(),
        })
    }
}

/// Remove URL userinfo before returning a proxy mount over the API. A
/// `base_url` may legitimately contain basic-auth credentials; those belong to
/// the daemon's outbound request, not to the shell or a module.
pub fn redact_url_userinfo(value: &str) -> String {
    let Ok(mut url) = url::Url::parse(value) else {
        return value.to_string();
    };
    if url.username().is_empty() && url.password().is_none() {
        return value.to_string();
    }
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn audit_is_enabled_by_default_but_can_be_disabled() {
        assert!(Config::default().audit.enabled);
        let config = AuditConfig::default();
        assert!(config.enabled);
        assert!(!AuditConfig { enabled: false }.enabled);
    }

    #[test]
    fn link_port_defaults_to_ephemeral_and_parses_a_fixed_port() {
        assert_eq!(LinkConfig::default().port, 0);
        let config: Config = toml::from_str("[link]\nport = 7843\n").expect("config");
        assert_eq!(config.link.port, 7843);
    }

    #[test]
    fn config_summary_redacts_agent_env_literals_and_proxy_credentials() {
        let mut config = Config::default();
        config
            .agent
            .env
            .insert("API_TOKEN".into(), "literal-secret".into());
        config
            .agent
            .env
            .insert("FROM_ENV".into(), "${FROM_ENV}".into());
        config.proxy.push(ProxyConfig {
            id: "demo".into(),
            title: "Demo".into(),
            base_url: "http://user:pass@127.0.0.1:8080".into(),
            host: None,
            websocket: true,
            frame: false,
        });
        let summary = config.config_summary();
        assert_eq!(summary["agent"]["env"]["API_TOKEN"], "[redacted]");
        assert_eq!(summary["agent"]["env"]["FROM_ENV"], "${FROM_ENV}");
        let base = summary["proxy"][0]["base_url"].as_str().unwrap();
        assert!(!base.contains("user"), "{base}");
        assert!(!base.contains("pass"), "{base}");
        assert!(base.contains("127.0.0.1:8080"), "{base}");
    }

    #[test]
    fn remote_bind_requires_an_explicit_opt_in() {
        let config = ServerConfig::default();
        assert!(!config.allow_remote);
        assert_eq!(config.bind, "127.0.0.1");
    }

    #[test]
    fn agent_presets_have_commands_and_expected_ids() {
        let presets = agent_presets();
        let ids: Vec<_> = presets
            .iter()
            .filter_map(|preset| preset["id"].as_str())
            .collect();
        assert_eq!(ids, vec!["opencode", "codex", "claude", "custom"]);
        for preset in presets {
            if preset["id"] == "custom" {
                assert!(preset["command"].as_str().unwrap().is_empty());
            } else {
                assert!(!preset["command"].as_str().unwrap().is_empty());
            }
        }
        assert!(preset_defaults("codex").is_some());
        assert!(preset_defaults("claude").is_some());
        assert!(preset_defaults("unknown").is_none());
    }

    #[test]
    fn legacy_agent_timeout_is_migrated_to_no_timeout() {
        let mut config = Config::default();
        config.agent.timeout_secs = 900;
        assert!(config.migrate_legacy_agent_timeout());
        assert_eq!(config.agent.timeout_secs, 0);
        assert!(!config.migrate_legacy_agent_timeout());
        config.agent.timeout_secs = 1800;
        assert!(!config.migrate_legacy_agent_timeout());
        assert_eq!(config.agent.timeout_secs, 1800);
    }

    #[test]
    fn legacy_link_transport_keys_are_ignored() {
        let raw = r#"
[link]
account_url = "https://link.example.test"
relay = true
relay_urls = ["https://relay.example.test"]
relay_broker = false
bind_addr = "0.0.0.0:7842"
external_addrs = ["192.0.2.1:7842"]
"#;
        let config: Config = toml::from_str(raw).unwrap();
        assert!(!config.link.enabled);
        assert!(config.link.token.is_none());
        assert_eq!(config.link.ssh_port, 22);
    }

    #[test]
    fn agent_preset_is_inferred_from_command_for_legacy_configs() {
        let mut agent = AgentConfig::default();
        agent.preset.clear();
        agent.command = "/usr/local/bin/codex".into();
        assert_eq!(agent.effective_preset(), "codex");
        agent.command = "my-agent".into();
        assert_eq!(agent.effective_preset(), "custom");
    }

    #[test]
    fn opencode_preset_has_a_default_conversation_viewer() {
        let agent = AgentConfig::default();
        let viewer = agent.resolved_viewer().expect("viewer");
        assert_eq!(viewer.kind, "module");
        assert_eq!(viewer.module, "agent-harness");
        assert_eq!(
            viewer.path,
            "/?harness=opencode&session={session}&cwd={root}"
        );
    }

    #[test]
    fn other_presets_route_through_the_agent_harness_module() {
        for (preset, path) in [
            ("codex", "/?harness=codex&session={session}&cwd={root}"),
            ("claude", "/?harness=claude&session={session}&cwd={root}"),
            ("gemini", "/?harness=gemini&session={session}&cwd={root}"),
            ("custom", "/?session={session}&cwd={root}"),
        ] {
            let agent = AgentConfig {
                preset: preset.into(),
                ..AgentConfig::default()
            };
            let viewer = agent.resolved_viewer().expect(preset);
            assert_eq!(viewer.kind, "module", "{preset}");
            assert_eq!(viewer.module, "agent-harness", "{preset}");
            assert_eq!(viewer.path, path, "{preset}");
        }
    }

    #[test]
    fn custom_conversation_viewer_wins() {
        let agent = AgentConfig {
            viewer: AgentViewerConfig {
                kind: "url".into(),
                url: "https://example.com/c/{session}".into(),
                ..AgentViewerConfig::default()
            },
            ..AgentConfig::default()
        };
        let viewer = agent.resolved_viewer().expect("viewer");
        assert_eq!(viewer.kind, "url");
        assert_eq!(viewer.url, "https://example.com/c/{session}");
    }

    #[test]
    fn conversation_viewer_can_be_disabled() {
        let agent = AgentConfig {
            viewer: AgentViewerConfig {
                kind: "none".into(),
                ..AgentViewerConfig::default()
            },
            ..AgentConfig::default()
        };
        assert!(agent.resolved_viewer().is_none());
    }

    #[test]
    fn agent_env_expands_literal_values() {
        let mut agent = AgentConfig::default();
        agent
            .env
            .insert("OPENAI_BASE_URL".into(), "http://127.0.0.1:1234/v1".into());
        let resolved = agent.resolved_env();
        assert!(resolved.contains(&("OPENAI_BASE_URL".into(), "http://127.0.0.1:1234/v1".into())));
    }

    #[test]
    fn legacy_peer_entries_are_pruned_to_link_peers() {
        let mut config = Config::default();
        config.peers.push(PeerConfig {
            id: "laptop".into(),
            name: "Laptop".into(),
            url: Some("http://127.0.0.1:9478".into()),
            token: "old".into(),
        });
        config.peers.push(PeerConfig {
            id: "link-abc".into(),
            name: "Linked".into(),
            url: Some("http://127.0.0.1:41000".into()),
            token: "link".into(),
        });

        assert_eq!(config.prune_legacy_peers(), 1);
        assert_eq!(config.peers.len(), 1);
        assert_eq!(config.peers[0].id, "link-abc");
    }

    #[test]
    fn legacy_ssh_peer_blocks_are_ignored() {
        let raw = r#"
[[peers]]
id = "studio"
name = "Studio"
token = "old"

[peers.ssh]
target = "user@studio"
remote_port = 9477
"#;
        let mut config: Config = toml::from_str(raw).expect("legacy config parses");
        assert_eq!(config.prune_legacy_peers(), 1);
        assert!(config.peers.is_empty());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    fn paths(root: &Path) -> Paths {
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
    fn paths_ensure_creates_every_runtime_directory() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        for dir in [
            &paths.config_dir,
            &paths.data_dir,
            &paths.modules_dir,
            &paths.workshop_dir,
            &paths.logs_dir,
            &paths.runtime_dir,
        ] {
            assert!(dir.is_dir(), "{} was not created", dir.display());
        }
        assert_eq!(paths.module_dir("demo"), paths.modules_dir.join("demo"));
    }

    #[test]
    fn custom_root_uses_a_portable_config_data_run_layout() {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths::from_root(root.path()).unwrap();
        assert_eq!(paths.config_dir, root.path().join("config"));
        assert_eq!(paths.data_dir, root.path().join("data"));
        assert_eq!(paths.runtime_dir, root.path().join("run"));
        assert_eq!(paths.config_file, root.path().join("config/config.toml"));
        assert!(paths.config_dir.is_dir());
        assert!(paths.data_dir.is_dir());
        assert!(paths.logs_dir.is_dir());
    }

    #[test]
    fn config_load_or_create_persists_a_stable_peer_token() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        let first = Config::load_or_create(&paths).unwrap();
        let token = first.peer_token().to_string();
        assert!(!token.is_empty());
        assert!(paths.config_file.is_file());
        let second = Config::load_or_create(&paths).unwrap();
        assert_eq!(second.peer_token(), token);
        assert_eq!(second.config_version, CONFIG_VERSION);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&paths.config_file)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_config_reports_the_offending_path() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        std::fs::write(&paths.config_file, "server = [not valid").unwrap();
        let error = Config::load_or_create(&paths).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("invalid config"), "{message}");
        assert!(message.contains("config.toml"), "{message}");
    }

    #[test]
    fn legacy_config_is_migrated_and_saved() {
        let root = tempfile::tempdir().unwrap();
        let paths = paths(root.path());
        paths.ensure().unwrap();
        std::fs::write(
            &paths.config_file,
            r#"
[agent]
timeout_secs = 900

[[peers]]
id = "old-direct"
name = "Old"
url = "http://127.0.0.1:9478"
token = "legacy"
"#,
        )
        .unwrap();
        let config = Config::load_or_create(&paths).unwrap();
        assert_eq!(config.agent.timeout_secs, 0);
        assert!(config.peers.is_empty());
        assert!(config.peer_token().len() >= 32);
        let saved = std::fs::read_to_string(&paths.config_file).unwrap();
        assert!(!saved.contains("old-direct"));
        assert!(saved.contains("config_version = 2"));
    }

    #[test]
    fn url_userinfo_redaction_is_conservative() {
        assert_eq!(
            redact_url_userinfo("http://user:pass@127.0.0.1:8080/path"),
            "http://127.0.0.1:8080/path"
        );
        assert_eq!(
            redact_url_userinfo("http://127.0.0.1:8080/path"),
            "http://127.0.0.1:8080/path"
        );
        assert_eq!(redact_url_userinfo("not a URL"), "not a URL");
        assert_eq!(
            redact_url_userinfo("http://token@127.0.0.1:1/"),
            "http://127.0.0.1:1/"
        );
    }

    #[test]
    fn canonical_host_and_origin_ignore_trailing_dots() {
        let mut config = Config::default();
        config.server.canonical_host = "orqella.localhost.".into();
        config.server.port = 9999;
        assert_eq!(config.canonical_host(), "orqella.localhost");
        assert_eq!(config.origin(), "http://orqella.localhost:9999");
    }

    #[test]
    fn loopback_origin_follows_a_loopback_bind_and_falls_back_to_v4() {
        let mut config = Config::default();
        config.server.port = 9999;
        assert_eq!(config.loopback_origin(), "http://127.0.0.1:9999/");
        config.server.bind = "127.0.0.5".into();
        assert_eq!(config.loopback_origin(), "http://127.0.0.5:9999/");
        config.server.bind = "::1".into();
        assert_eq!(config.loopback_origin(), "http://[::1]:9999/");
        config.server.bind = "0.0.0.0".into();
        assert_eq!(config.loopback_origin(), "http://127.0.0.1:9999/");
    }

    #[test]
    fn proxy_lookup_matches_only_the_exact_mount() {
        let mut config = Config::default();
        config.proxy.push(ProxyConfig {
            id: "demo".into(),
            title: "Demo".into(),
            base_url: "http://127.0.0.1:8080".into(),
            host: None,
            websocket: true,
            frame: false,
        });
        assert_eq!(
            config.proxy("demo").map(|mount| mount.id.as_str()),
            Some("demo")
        );
        assert!(config.proxy("Demo").is_none());
        assert!(config.proxy("").is_none());
    }

    #[test]
    fn agent_viewer_app_and_module_templates_resolve() {
        let app = AgentConfig {
            viewer: AgentViewerConfig {
                kind: "app".into(),
                mount: "opencode".into(),
                path: "/session/{session}".into(),
                ..AgentViewerConfig::default()
            },
            ..AgentConfig::default()
        };
        let viewer = app.resolved_viewer().expect("app viewer");
        assert_eq!(viewer.kind, "app");
        assert_eq!(viewer.mount, "opencode");
        assert_eq!(viewer.path, "/session/{session}");

        let module = AgentConfig {
            viewer: AgentViewerConfig {
                kind: "module".into(),
                module: "opencode".into(),
                ..AgentViewerConfig::default()
            },
            ..AgentConfig::default()
        };
        assert_eq!(module.resolved_viewer().unwrap().module, "opencode");

        let invalid = AgentConfig {
            viewer: AgentViewerConfig {
                kind: "module".into(),
                ..AgentViewerConfig::default()
            },
            ..AgentConfig::default()
        };
        assert!(invalid.resolved_viewer().is_none());
    }
}
