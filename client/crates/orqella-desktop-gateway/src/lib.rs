//! Loopback origin gateway for the Orqella desktop client.
//!
//! The gateway presents a stable local origin to the desktop WebView and
//! forwards HTTP, WebSocket, and SSE traffic to the active Orqella hub. It is
//! deliberately not a general-purpose proxy: the caller supplies the only
//! upstream host that may be reached.

use anyhow::{Context, Result, bail};
use axum::Router;
use axum::body::Body;
use axum::extract::ws::{CloseFrame, Message as ClientMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, Request, State};
use axum::http::header::{
    CACHE_CONTROL, CONNECTION, CONTENT_SECURITY_POLICY, CONTENT_TYPE, HOST, LOCATION, ORIGIN,
    REFERER, SET_COOKIE, TRANSFER_ENCODING, UPGRADE,
};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::collections::HashMap;
#[cfg(target_os = "macos")]
use std::net::ToSocketAddrs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(target_os = "macos")]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
use tracing::{debug, warn};
use uuid::Uuid;

/// The hub endpoint the gateway forwards to.
#[derive(Debug, Clone)]
pub struct Upstream {
    /// The address actually connected to, usually loopback or a local tunnel.
    pub host: String,
    pub port: u16,
    /// Value used for the upstream `Host` header.
    pub canonical_host: String,
    /// Port used in canonical Host/Origin values. Defaults to `port`.
    pub canonical_port: u16,
    /// Whether the upstream is reached over TLS.
    pub tls: bool,
}

impl Upstream {
    pub fn http(canonical_host: impl Into<String>, host: impl Into<String>, port: u16) -> Self {
        Self {
            host: host.into(),
            port,
            canonical_host: canonical_host.into(),
            canonical_port: port,
            tls: false,
        }
    }

    /// Set the canonical port separately from the local tunnel port.
    pub fn with_canonical_port(mut self, port: u16) -> Self {
        self.canonical_port = port;
        self
    }

    fn authority(&self) -> String {
        format!("{}:{}", self.canonical_host, self.canonical_port)
    }

    fn tcp_authority(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }

    fn http_base(&self) -> String {
        let scheme = if self.tls { "https" } else { "http" };
        format!("{scheme}://{}", self.tcp_authority())
    }

    fn ws_base(&self) -> String {
        let scheme = if self.tls { "wss" } else { "ws" };
        format!("{scheme}://{}", self.tcp_authority())
    }
}

/// A local app origin mapped to a canonical upstream host.
#[derive(Debug, Clone)]
pub struct AppOriginConfig {
    pub ip: IpAddr,
    pub mount: String,
    pub node: String,
    pub canonical_host: String,
    /// Optional per-hub label inserted into the macOS browser host. This keeps
    /// app cookies separated when the same app is opened through two hubs.
    pub browser_label: Option<String>,
    /// Preferred local port for platforms that need one (macOS). The caller
    /// persists the chosen port so the app origin survives restarts.
    pub preferred_port: Option<u16>,
}

impl AppOriginConfig {
    pub fn local(ip: IpAddr, mount: impl Into<String>, canonical_host: impl Into<String>) -> Self {
        Self {
            ip,
            mount: mount.into(),
            node: "local".into(),
            canonical_host: canonical_host.into(),
            browser_label: None,
            preferred_port: None,
        }
    }
}

/// Request to open a full app in a dedicated native window.
#[derive(Debug, Clone, Deserialize)]
pub struct AppWindowRequest {
    pub url: String,
    #[serde(default)]
    pub title: String,
}

/// Request to allocate a loopback origin for a full app mount.
#[derive(Debug, Clone, Deserialize)]
pub struct AppOriginRequest {
    pub node: String,
    pub mount: String,
    pub canonical_host: String,
}

/// Callback used by the desktop shell to create a dedicated app window.
pub type AppWindowOpener = Arc<dyn Fn(AppWindowRequest) + Send + Sync>;

/// Callback used by the desktop shell to open its settings window.
pub type SettingsOpener = Arc<dyn Fn() + Send + Sync>;

/// Callback used by the desktop shell to show a native notification.
pub type NotificationSender = Arc<dyn Fn(String, String) + Send + Sync>;

/// Payload accepted by the native notification endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct NotificationRequest {
    pub title: String,
    #[serde(default)]
    pub body: String,
}

/// Configuration for one gateway listener.
#[derive(Debug, Clone)]
pub struct GatewayConfig {
    pub bind: IpAddr,
    pub port: u16,
    /// Canonical host expected by the hub for the shell.
    pub shell_host: String,
    pub upstream: Upstream,
    /// Full app surfaces to expose on isolated loopback origins.
    pub app_origins: Vec<AppOriginConfig>,
    /// Second loopback octet used for dynamic (peer) app origins. Zero keeps
    /// the historical `127.0.0.x` range; nonzero isolates peer apps by hub.
    pub origin_namespace: u8,
}

impl GatewayConfig {
    pub fn local(port: u16) -> Self {
        Self {
            bind: IpAddr::from([127, 0, 0, 1]),
            port,
            shell_host: "orqella.localhost".into(),
            upstream: Upstream::http("orqella.localhost", "127.0.0.1", 9477),
            app_origins: Vec::new(),
            origin_namespace: 0,
        }
    }
}

#[derive(Clone)]
struct ProxyState {
    client: reqwest::Client,
    upstream: Upstream,
    /// Host header value sent to the upstream.
    upstream_host: String,
    /// Origin and Referer value sent to the upstream.
    upstream_origin: String,
    /// Whether this listener serves the hub shell rather than an isolated app.
    shell: bool,
    /// Canonical host expected by the upstream for this listener.
    canonical_host: String,
    /// Upstream canonical host used for Host/Origin/Location rewriting.
    upstream_canonical_host: String,
    /// Loopback origin of the shell, used to constrain app framing.
    shell_origin: String,
    /// Local origin map injected into the shell for app surfaces.
    app_origins: Arc<RwLock<HashMap<String, String>>>,
    /// Per-run bearer token for native-only gateway endpoints.
    native_token: Arc<String>,
    /// Per-run unguessable path for the shell shim. Modules and apps must not
    /// be able to fetch the token from a fixed URL.
    shim_path: String,
    /// Optional native window opener supplied by the Tauri shell.
    app_window_opener: Arc<RwLock<Option<AppWindowOpener>>>,
    /// Optional settings-window opener supplied by the Tauri shell.
    settings_opener: Arc<RwLock<Option<SettingsOpener>>>,
    /// Optional native notification sender supplied by the Tauri shell.
    notification_sender: Arc<RwLock<Option<NotificationSender>>>,
    /// Shared dynamic origin allocator.
    manager: Arc<AppOriginManager>,
}

fn _assert_proxy_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<ProxyState>();
    assert_send_sync::<AppOriginManager>();
}

/// A running gateway listener.
pub struct Gateway {
    pub local_addr: SocketAddr,
    pub base_url: String,
    shim_path: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    manager: Arc<AppOriginManager>,
}

struct AppListener {
    origin: String,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<()>>,
    #[cfg(target_os = "macos")]
    mdns: Option<std::process::Child>,
}

impl Drop for AppListener {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        #[cfg(target_os = "macos")]
        if let Some(mut child) = self.mdns.take() {
            stop_mdns(&mut child);
        }
    }
}

struct AppOriginManager {
    client: reqwest::Client,
    upstream: Upstream,
    shell_host: String,
    port: AtomicU16,
    native_token: Arc<String>,
    shim_path: String,
    origin_namespace: u8,
    app_origins: Arc<RwLock<HashMap<String, String>>>,
    app_window_opener: Arc<RwLock<Option<AppWindowOpener>>>,
    settings_opener: Arc<RwLock<Option<SettingsOpener>>>,
    notification_sender: Arc<RwLock<Option<NotificationSender>>>,
    listeners: tokio::sync::Mutex<HashMap<IpAddr, AppListener>>,
}

#[cfg(target_os = "macos")]
fn macos_browser_host(canonical_host: &str) -> String {
    canonical_host
        .strip_suffix(".orqella.localhost")
        .map(|prefix| format!("{prefix}.orqella.local"))
        .unwrap_or_else(|| canonical_host.to_string())
}

#[cfg(target_os = "macos")]
fn register_mdns(host: &str, port: u16) -> Result<std::process::Child> {
    // dns-sd does not exit when its stdin closes and has no parent-death
    // signal. Keep a tiny supervisor that stops the registrar when the client
    // exits, including an unclean exit.
    let script = r#"
host="$1"
port="$2"
instance="orqella-$(printf '%s' "$host" | tr '.' '-')-$port"
trap 'kill "$child" 2>/dev/null; exit 0' TERM INT HUP
/usr/bin/dns-sd -P "$instance" _orqella._tcp local "$port" "$host" 127.0.0.1 &
child=$!
while kill -0 "$PPID" 2>/dev/null; do sleep 1; done
kill "$child" 2>/dev/null
"#;
    Command::new("/bin/sh")
        .args(["-c", script, "orqella-mdns", host, &port.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("start dns-sd supervisor")
}

#[cfg(target_os = "macos")]
fn stop_mdns(child: &mut std::process::Child) {
    let _ = Command::new("/bin/kill")
        .args(["-TERM", &child.id().to_string()])
        .status();
    let _ = child.wait();
}

#[cfg(target_os = "macos")]
async fn wait_for_mdns(host: String, port: u16) -> bool {
    tokio::task::spawn_blocking(move || {
        for _ in 0..30 {
            if (host.as_str(), port)
                .to_socket_addrs()
                .is_ok_and(|mut addresses| addresses.any(|address| address.ip().is_loopback()))
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        false
    })
    .await
    .unwrap_or(false)
}

fn stable_hash(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

impl AppOriginManager {
    fn canonical_allowed(&self, host: &str) -> bool {
        !host.is_empty()
            && (host.eq_ignore_ascii_case(&self.shell_host)
                || host.ends_with(&format!(".{}", self.shell_host)))
    }

    fn mount_host_allowed(&self, node: &str, mount: &str, canonical_host: &str) -> bool {
        let valid_label = |value: &str| {
            !value.is_empty()
                && value.len() <= 63
                && !value.starts_with('-')
                && !value.ends_with('-')
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
        };
        if !valid_label(mount) || !(node == "local" || valid_label(node)) {
            return false;
        }
        if !self.canonical_allowed(canonical_host) {
            return false;
        }

        // The canonical host must be rooted at the mount it claims to serve:
        // `mount.<shell>` for a local app or `mount.<node>.<shell>` for a peer.
        let local_host = format!("{mount}.{}", self.shell_host);
        let peer_suffix = format!(".{}", self.shell_host);
        canonical_host.eq_ignore_ascii_case(&local_host)
            || (canonical_host.ends_with(&peer_suffix)
                && canonical_host.starts_with(&format!("{mount}.")))
    }

    fn app_state(
        self: &Arc<Self>,
        upstream_canonical_host: &str,
        browser_host: &str,
    ) -> Arc<ProxyState> {
        Arc::new(ProxyState {
            client: self.client.clone(),
            upstream: self.upstream.clone(),
            upstream_host: format!(
                "{}:{}",
                upstream_canonical_host, self.upstream.canonical_port
            ),
            upstream_origin: format!(
                "http://{}:{}",
                upstream_canonical_host, self.upstream.canonical_port
            ),
            shell: false,
            canonical_host: browser_host.into(),
            upstream_canonical_host: upstream_canonical_host.into(),
            shell_origin: format!("http://127.0.0.1:{}", self.port.load(Ordering::SeqCst)),
            app_origins: self.app_origins.clone(),
            native_token: self.native_token.clone(),
            shim_path: self.shim_path.clone(),
            app_window_opener: self.app_window_opener.clone(),
            settings_opener: self.settings_opener.clone(),
            notification_sender: self.notification_sender.clone(),
            manager: self.clone(),
        })
    }

    async fn origin_for_ip(&self, ip: IpAddr) -> Option<String> {
        self.listeners
            .lock()
            .await
            .get(&ip)
            .map(|listener| listener.origin.clone())
    }

    async fn start_origin_listener(
        self: &Arc<Self>,
        ip: IpAddr,
        canonical_host: &str,
        browser_label: Option<&str>,
        preferred_port: Option<u16>,
    ) -> Result<String> {
        // macOS only has 127.0.0.1 on lo0. Use a per-app `.orqella.local`
        // hostname registered over mDNS plus a unique loopback port; this
        // avoids /etc/hosts, preserves host-scoped cookies, and needs no sudo.
        #[cfg(target_os = "macos")]
        let (bind, port, browser_host) = (
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            preferred_port.unwrap_or(0),
            {
                let mut host = macos_browser_host(canonical_host);
                let label = browser_label.map(str::to_string).or_else(|| {
                    (self.origin_namespace != 0)
                        .then(|| format!("hub-{:02x}", self.origin_namespace))
                });
                if let Some(label) = label {
                    if let Some(prefix) = host.strip_suffix(".orqella.local") {
                        host = format!("{prefix}.{label}.orqella.local");
                    }
                }
                host
            },
        );
        #[cfg(not(target_os = "macos"))]
        let (bind, port, browser_host) = (
            ip,
            self.port.load(Ordering::SeqCst),
            canonical_host.to_string(),
        );
        let _ = preferred_port;
        let _ = browser_label;
        let state = self.app_state(canonical_host, &browser_host);
        let (local_addr, shutdown, task) = spawn_app_listener(bind, port, state).await?;
        #[cfg(target_os = "macos")]
        let mdns = match register_mdns(&browser_host, local_addr.port()) {
            Ok(child) => {
                if wait_for_mdns(browser_host.clone(), local_addr.port()).await {
                    Some(child)
                } else {
                    let mut child = child;
                    stop_mdns(&mut child);
                    warn!(host = %browser_host, "mDNS app host did not resolve");
                    None
                }
            }
            Err(error) => {
                warn!(%error, host = %browser_host, "could not register mDNS app host");
                None
            }
        };
        #[cfg(target_os = "macos")]
        let origin = format!("http://{browser_host}:{}/", local_addr.port());
        #[cfg(not(target_os = "macos"))]
        let origin = format!("http://{local_addr}/");
        self.listeners.lock().await.insert(
            ip,
            AppListener {
                origin: origin.clone(),
                shutdown: Some(shutdown),
                task: Some(task),
                #[cfg(target_os = "macos")]
                mdns,
            },
        );
        Ok(origin)
    }

    async fn lease(
        self: &Arc<Self>,
        node: &str,
        mount: &str,
        canonical_host: &str,
    ) -> Result<String> {
        if mount.trim().is_empty() {
            bail!("app mount is empty");
        }
        if !self.mount_host_allowed(node, mount, canonical_host) {
            bail!("app origin request is outside the hub namespace");
        }
        let key = format!("{node}:{mount}");
        if let Some(origin) = self
            .app_origins
            .read()
            .expect("app origin map")
            .get(&key)
            .cloned()
        {
            return Ok(origin);
        }

        let ip = {
            let listeners = self.listeners.lock().await;
            let start = (stable_hash(&format!("{node}|{mount}")) % 253) as u16;
            (0..253u16)
                .find_map(|offset| {
                    let last = 2 + ((start + offset) % 253) as u8;
                    let address = if self.origin_namespace == 0 {
                        Ipv4Addr::new(127, 0, 0, last)
                    } else {
                        Ipv4Addr::new(127, self.origin_namespace, last, 1)
                    };
                    let ip = IpAddr::V4(address);
                    (!listeners.contains_key(&ip)).then_some(ip)
                })
                .context("app origin pool is exhausted")?
        };
        let origin = self
            .start_origin_listener(ip, canonical_host, None, None)
            .await?;
        let mut map = self.app_origins.write().expect("app origin map");
        map.insert(key, origin.clone());
        map.insert(mount.to_string(), origin.clone());
        map.insert(canonical_host.to_string(), origin.clone());
        Ok(origin)
    }

    async fn lease_at(
        self: &Arc<Self>,
        ip: IpAddr,
        node: &str,
        mount: &str,
        canonical_host: &str,
        browser_label: Option<&str>,
        preferred_port: Option<u16>,
    ) -> Result<String> {
        if !self.canonical_allowed(canonical_host) {
            bail!("app canonical host is outside the hub namespace");
        }
        if let Some(origin) = self.origin_for_ip(ip).await {
            let mut map = self.app_origins.write().expect("app origin map");
            map.insert(format!("{node}:{mount}"), origin.clone());
            map.insert(mount.to_string(), origin.clone());
            map.insert(canonical_host.to_string(), origin.clone());
            return Ok(origin);
        }
        let origin = match self
            .start_origin_listener(ip, canonical_host, browser_label, preferred_port)
            .await
        {
            Ok(origin) => origin,
            Err(error) if preferred_port.is_some() => {
                warn!(
                    %error,
                    host = %canonical_host,
                    "preferred app origin port was unavailable; using an ephemeral port"
                );
                self.start_origin_listener(ip, canonical_host, browser_label, None)
                    .await?
            }
            Err(error) => return Err(error),
        };
        let mut map = self.app_origins.write().expect("app origin map");
        map.insert(format!("{node}:{mount}"), origin.clone());
        map.insert(mount.to_string(), origin.clone());
        map.insert(canonical_host.to_string(), origin.clone());
        Ok(origin)
    }

    async fn stop(&self) {
        let listeners = self
            .listeners
            .lock()
            .await
            .drain()
            .filter_map(|(_, mut listener)| {
                if let Some(shutdown) = listener.shutdown.take() {
                    let _ = shutdown.send(());
                }
                listener.task.take()
            })
            .collect::<Vec<_>>();
        for task in listeners {
            let mut task = task;
            match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
                Ok(result) => {
                    let _ = result;
                }
                Err(_) => {
                    warn!("app origin listener did not stop in time; aborting");
                    task.abort();
                }
            }
        }
    }

    fn abort(&self) {
        let Ok(mut listeners) = self.listeners.try_lock() else {
            return;
        };
        for (_, mut listener) in listeners.drain() {
            if let Some(shutdown) = listener.shutdown.take() {
                let _ = shutdown.send(());
            }
            if let Some(task) = listener.task.take() {
                task.abort();
            }
        }
    }
}

impl Gateway {
    /// Start the shell gateway.
    pub async fn start(config: GatewayConfig) -> Result<Self> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .context("build gateway HTTP client")?;
        let app_origins = Arc::new(RwLock::new(HashMap::new()));
        let native_token = Arc::new(Uuid::new_v4().to_string());
        let shim_path = format!("/__orqella/native/shim/{native_token}");
        let app_window_opener = Arc::new(RwLock::new(None));
        let settings_opener = Arc::new(RwLock::new(None));
        let notification_sender = Arc::new(RwLock::new(None));
        let manager = Arc::new(AppOriginManager {
            client: client.clone(),
            upstream: config.upstream.clone(),
            shell_host: config.shell_host.clone(),
            port: AtomicU16::new(0),
            native_token: native_token.clone(),
            shim_path: shim_path.clone(),
            origin_namespace: config.origin_namespace,
            app_origins: app_origins.clone(),
            app_window_opener: app_window_opener.clone(),
            settings_opener: settings_opener.clone(),
            notification_sender: notification_sender.clone(),
            listeners: tokio::sync::Mutex::new(HashMap::new()),
        });
        let state = Arc::new(ProxyState {
            client,
            upstream_host: config.upstream.authority(),
            upstream_origin: format!("http://{}", config.upstream.authority()),
            upstream: config.upstream.clone(),
            shell: true,
            canonical_host: config.shell_host.clone(),
            upstream_canonical_host: config.shell_host.clone(),
            shell_origin: String::new(),
            app_origins: app_origins.clone(),
            native_token,
            shim_path: shim_path.clone(),
            app_window_opener,
            settings_opener,
            notification_sender,
            manager: manager.clone(),
        });
        let (local_addr, shutdown, task) =
            spawn_shell_listener(config.bind, config.port, state.clone()).await?;
        manager.port.store(local_addr.port(), Ordering::SeqCst);
        let gateway = Self {
            base_url: format!("http://{local_addr}/"),
            local_addr,
            shim_path,
            shutdown: Some(shutdown),
            task: Some(task),
            manager,
        };
        for app in config.app_origins {
            if let Err(error) = gateway
                .manager
                .lease_at(
                    app.ip,
                    &app.node,
                    &app.mount,
                    &app.canonical_host,
                    app.browser_label.as_deref(),
                    app.preferred_port,
                )
                .await
            {
                warn!(%error, host = %app.canonical_host, "could not start app origin");
            }
        }
        Ok(gateway)
    }

    /// Per-run path of the shell shim script.
    pub fn shim_path(&self) -> &str {
        &self.shim_path
    }

    /// Allocate or reuse an isolated app origin at `ip`.
    pub async fn add_app_origin(
        &mut self,
        ip: IpAddr,
        canonical_host: impl Into<String>,
    ) -> Result<String> {
        let canonical_host = canonical_host.into();
        self.manager
            .lease_at(ip, "local", "app", &canonical_host, None, None)
            .await
    }

    /// Allocate or reuse an isolated app origin for a node/mount pair.
    pub async fn lease_app_origin(
        &self,
        node: &str,
        mount: &str,
        canonical_host: &str,
    ) -> Result<String> {
        self.manager.lease(node, mount, canonical_host).await
    }

    pub fn set_app_window_opener(&self, opener: AppWindowOpener) {
        *self
            .manager
            .app_window_opener
            .write()
            .expect("app window opener lock") = Some(opener);
    }

    pub fn set_settings_opener(&self, opener: SettingsOpener) {
        *self
            .manager
            .settings_opener
            .write()
            .expect("settings opener lock") = Some(opener);
    }

    pub fn set_notification_sender(&self, sender: NotificationSender) {
        *self
            .manager
            .notification_sender
            .write()
            .expect("notification sender lock") = Some(sender);
    }

    pub async fn app_origin(&self, ip: IpAddr) -> Option<String> {
        self.manager.origin_for_ip(ip).await
    }

    /// Stop all listeners. Safe to call more than once.
    pub async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            let mut task = task;
            if tokio::time::timeout(Duration::from_secs(3), &mut task)
                .await
                .is_err()
            {
                warn!("gateway shell listener did not stop in time; aborting");
                task.abort();
            }
        }
        self.manager.stop().await;
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.manager.abort();
    }
}

async fn spawn_shell_listener(
    bind: IpAddr,
    port: u16,
    state: Arc<ProxyState>,
) -> Result<(SocketAddr, oneshot::Sender<()>, JoinHandle<()>)> {
    #[allow(unused_mut)]
    let mut app = Router::new()
        .route("/__orqella/native/shim/{token}", get(native_shim))
        .route("/__orqella/native/health", any(native_health))
        .route("/__orqella/native/app-origin", post(native_app_origin))
        .route("/__orqella/native/notify", post(native_notify))
        .route("/__orqella/native/settings", post(native_settings))
        .route("/__orqella/native/open-app", post(open_app_window));
    #[cfg(debug_assertions)]
    {
        app = app.route("/__orqella/native/test-download", any(test_download));
    }
    let app = app.fallback(any(proxy)).with_state(state);
    serve_listener(bind, port, app).await
}

async fn spawn_app_listener(
    bind: IpAddr,
    port: u16,
    state: Arc<ProxyState>,
) -> Result<(SocketAddr, oneshot::Sender<()>, JoinHandle<()>)> {
    let app = Router::new().fallback(any(proxy)).with_state(state);
    serve_listener(bind, port, app).await
}

async fn serve_listener(
    bind: IpAddr,
    port: u16,
    app: Router,
) -> Result<(SocketAddr, oneshot::Sender<()>, JoinHandle<()>)> {
    let listener = TcpListener::bind((bind, port))
        .await
        .with_context(|| format!("bind gateway to {bind}:{port}"))?;
    let local_addr = listener.local_addr()?;
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        let server = axum::serve(listener, app).with_graceful_shutdown(async move {
            let _ = shutdown_rx.await;
        });
        if let Err(error) = server.await {
            warn!(%error, "gateway listener stopped");
        }
    });
    Ok((local_addr, shutdown_tx, task))
}

fn host_without_port(value: &str) -> &str {
    if let Some(rest) = value.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    value.split(':').next().unwrap_or(value)
}

/// True only for IP literals in the loopback range or the name `localhost`.
///
/// Do not replace this with a string prefix check: names such as
/// `127.0.0.1.attacker.example` are valid DNS names and enable DNS rebinding.
pub fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback())
}

fn host_header_port(headers: &HeaderMap) -> Option<u16> {
    host_value_port(headers.get(HOST)?.to_str().ok()?)
}

/// Native endpoints are called by the shell document. A browser sends `Origin`
/// for POSTs; it must match the shell origin, not merely any loopback host.
/// This blocks DNS-rebinding pages and cross-site module/app requests even when
/// they somehow obtain the native token.
fn native_origin_allowed(state: &ProxyState, headers: &HeaderMap) -> bool {
    if let Some(site) = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        && (site.eq_ignore_ascii_case("cross-site") || site.eq_ignore_ascii_case("same-site"))
    {
        return false;
    }
    let Some(origin) = headers.get(ORIGIN).and_then(|value| value.to_str().ok()) else {
        return true;
    };
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    if url.scheme() != "http" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let trusted = host.eq_ignore_ascii_case("127.0.0.1")
        || host.eq_ignore_ascii_case("localhost")
        || host.eq_ignore_ascii_case(&state.canonical_host);
    let Some(origin_port) = url.port_or_known_default() else {
        return false;
    };
    trusted && Some(origin_port) == host_header_port(headers)
}

fn request_host_allowed(state: &ProxyState, headers: &HeaderMap) -> bool {
    let Some(host) = headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(host_without_port)
    else {
        return false;
    };
    let loopback = is_loopback_host(host);
    if state.shell {
        return loopback
            || host.eq_ignore_ascii_case(&state.canonical_host)
            || host.ends_with(&format!(".{}", state.canonical_host));
    }
    loopback || host.eq_ignore_ascii_case(&state.canonical_host)
}

fn native_token_ok(state: &ProxyState, headers: &HeaderMap) -> bool {
    headers
        .get("x-orqella-native")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == state.native_token.as_str())
        && native_origin_allowed(state, headers)
}

#[cfg(debug_assertions)]
async fn test_download() -> Response {
    let body = "orqella desktop download test\n".repeat(16 * 1024);
    (
        [
            (CONTENT_TYPE, "text/plain; charset=utf-8"),
            (
                axum::http::header::CONTENT_DISPOSITION,
                "attachment; filename=orqella-download-test.txt",
            ),
        ],
        body,
    )
        .into_response()
}

async fn open_app_window(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    axum::Json(mut request): axum::Json<AppWindowRequest>,
) -> Response {
    if !native_token_ok(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Ok(url) = url::Url::parse(&request.url) else {
        return (StatusCode::BAD_REQUEST, "invalid app window URL").into_response();
    };
    let loopback = url.host_str().is_some_and(|host| {
        is_loopback_host(host) || host == "orqella.local" || host.ends_with(".orqella.local")
    });
    let known_app_origin = url.host_str().is_some_and(|host| {
        state
            .app_origins
            .read()
            .expect("app origin map")
            .values()
            .any(|origin| {
                url::Url::parse(origin).is_ok_and(|origin| {
                    origin.scheme() == url.scheme()
                        && origin.host_str() == Some(host)
                        && origin.port_or_known_default() == url.port_or_known_default()
                })
            })
    });
    let allowed = url.scheme() == "http" && (loopback || known_app_origin);
    if !allowed || url.username() != "" || url.password().is_some() {
        return (
            StatusCode::BAD_REQUEST,
            "app window URL is outside loopback",
        )
            .into_response();
    }
    let opener = state
        .app_window_opener
        .read()
        .expect("app window opener lock")
        .clone();
    let Some(opener) = opener else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "native app windows are unavailable",
        )
            .into_response();
    };
    request.title = request.title.chars().take(120).collect();
    opener(request);
    StatusCode::ACCEPTED.into_response()
}

async fn native_health(State(state): State<Arc<ProxyState>>, headers: HeaderMap) -> Response {
    if !native_token_ok(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    (StatusCode::OK, "ok").into_response()
}

async fn native_app_origin(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<AppOriginRequest>,
) -> Response {
    if !native_token_ok(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    match state
        .manager
        .lease(&request.node, &request.mount, &request.canonical_host)
        .await
    {
        Ok(origin) => axum::Json(serde_json::json!({ "origin": origin })).into_response(),
        Err(error) => (
            StatusCode::BAD_REQUEST,
            format!("could not allocate app origin: {error:#}"),
        )
            .into_response(),
    }
}

async fn native_notify(
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
    axum::Json(request): axum::Json<NotificationRequest>,
) -> Response {
    if !native_token_ok(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let title = request.title.trim();
    let body = request.body.trim();
    if title.is_empty() || title.chars().count() > 120 || body.chars().count() > 500 {
        return (StatusCode::BAD_REQUEST, "invalid notification payload").into_response();
    }
    let sender = state
        .notification_sender
        .read()
        .expect("notification sender lock")
        .clone();
    let Some(sender) = sender else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    sender(title.to_string(), body.to_string());
    StatusCode::OK.into_response()
}

async fn native_settings(State(state): State<Arc<ProxyState>>, headers: HeaderMap) -> Response {
    if !native_token_ok(&state, &headers) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let opener = state
        .settings_opener
        .read()
        .expect("settings opener lock")
        .clone();
    let Some(opener) = opener else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    opener();
    StatusCode::ACCEPTED.into_response()
}

async fn native_shim(
    Path(token): Path<String>,
    State(state): State<Arc<ProxyState>>,
    headers: HeaderMap,
) -> Response {
    if token != state.native_token.as_str() || !native_origin_allowed(&state, &headers) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let origins = state.app_origins.read().expect("app origin map").clone();
    let json = serde_json::to_string(&origins).unwrap_or_else(|_| "{}".into());
    let script = format!(
        r#"(() => {{
  const origins = {json};
  const nativeToken = "{token}";
  window.__ORQELLA_NATIVE_TOKEN = nativeToken;
  window.__ORQELLA_APP_ORIGINS = origins;
  window.__ORQELLA_APP_ORIGIN_RESOLVER = async (payload) => {{
    if (!payload || !payload.mount) return '';
    const node = payload.node || 'local';
    const known = origins[`${{node}}:${{payload.mount}}`]
      || (node === 'local' ? origins[payload.mount] : '');
    if (known) return known;
    try {{
      const response = await fetch('/__orqella/native/app-origin', {{
        method: 'POST',
        headers: {{ 'Content-Type': 'application/json', 'X-Orqella-Native': nativeToken }},
        body: JSON.stringify({{
          node,
          mount: payload.mount,
          canonical_host: payload.host || '',
        }}),
      }});
      if (!response.ok) return '';
      const data = await response.json();
      return data.origin || '';
    }} catch (_) {{
      return '';
    }}
  }};

  // Older hub shells do not call the resolver. Rewrite iframe URLs that point
  // at a canonical app host so native clients still get isolated origins.
  const descriptor = Object.getOwnPropertyDescriptor(HTMLIFrameElement.prototype, 'src');
  if (!descriptor || !descriptor.set) return;
  const localFor = (value) => {{
    try {{
      const url = new URL(String(value), location.href);
      const origin = origins[url.hostname];
      return origin ? new URL(url.pathname + url.search + url.hash, origin).toString() : '';
    }} catch (_) {{
      return '';
    }}
  }};
  Object.defineProperty(HTMLIFrameElement.prototype, 'src', {{
    configurable: true,
    enumerable: descriptor.enumerable,
    get() {{ return descriptor.get.call(this); }},
    set(value) {{
      const local = localFor(value);
      descriptor.set.call(this, local || value);
    }},
  }});

  window.addEventListener('keydown', (event) => {{
    if (!(event.ctrlKey || event.metaKey) || event.key !== ',') return;
    event.preventDefault();
    fetch('/__orqella/native/settings', {{
      method: 'POST',
      headers: {{ 'X-Orqella-Native': nativeToken }},
    }}).catch(() => {{}});
  }});
  window.__ORQELLA_NOTIFY = async (title, body = '') => {{
    if (!title) return false;
    try {{
      const response = await fetch('/__orqella/native/notify', {{
        method: 'POST',
        headers: {{ 'Content-Type': 'application/json', 'X-Orqella-Native': nativeToken }},
        body: JSON.stringify({{ title: String(title), body: String(body) }}),
      }});
      return response.ok;
    }} catch (_) {{
      return false;
    }}
  }};
  const installWindowButton = () => {{
    const foot = document.querySelector('.rail-foot');
    if (!foot) return;
    let button = foot.querySelector('#__orqella_open_window');
    if (!button) {{
      button = document.createElement('button');
      button.id = '__orqella_open_window';
      button.type = 'button';
      button.className = 'icon-button';
      button.title = 'Open app in a window';
      button.setAttribute('aria-label', 'Open app in a window');
      button.textContent = '⧉';
      button.onclick = async () => {{
        const frame = document.querySelector('#appFrame');
        const url = frame?.src || '';
        if (!url || url === 'about:blank') return;
        const title = frame?.dataset.appTitle || 'Orqella app';
        try {{
          const response = await fetch('/__orqella/native/open-app', {{
            method: 'POST',
            headers: {{ 'Content-Type': 'application/json', 'X-Orqella-Native': nativeToken }},
            body: JSON.stringify({{ url, title }}),
          }});
          if (!response.ok) throw new Error(await response.text());
        }} catch (error) {{
          button.title = String(error);
          setTimeout(() => {{ button.title = 'Open app in a window'; }}, 3000);
        }}
      }};
      foot.insertBefore(button, foot.querySelector('.rail-toggle') || foot.querySelector('.mobile-menu-button'));
    }}
    const surface = document.querySelector('#appSurface');
    const hidden = !surface || surface.hidden;
    if (button.hidden !== hidden) button.hidden = hidden;
  }};
  new MutationObserver(installWindowButton).observe(document.documentElement, {{ childList: true, subtree: true, attributes: true, attributeFilter: ['hidden'] }});
  installWindowButton();
}})();
"#,
        token = state.native_token
    );
    (
        [
            (CONTENT_TYPE, "application/javascript; charset=utf-8"),
            (CACHE_CONTROL, "no-store"),
        ],
        script,
    )
        .into_response()
}

async fn proxy(State(state): State<Arc<ProxyState>>, mut request: Request) -> Response {
    if !request_host_allowed(&state, request.headers()) {
        return (StatusCode::MISDIRECTED_REQUEST, "unrecognized gateway Host").into_response();
    }
    if matches!(request.method(), &Method::CONNECT | &Method::TRACE) {
        return (StatusCode::METHOD_NOT_ALLOWED, "method is not proxied").into_response();
    }
    let is_websocket = request
        .headers()
        .get(UPGRADE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    if is_websocket {
        let (mut parts, body) = request.into_parts();
        let ws = match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
            Ok(ws) => ws,
            Err(_) => {
                return (StatusCode::BAD_REQUEST, "invalid WebSocket upgrade").into_response();
            }
        };
        request = Request::from_parts(parts, body);
        return proxy_websocket(state, ws, request).await;
    }
    proxy_http(state, request).await
}

async fn proxy_http(state: Arc<ProxyState>, request: Request) -> Response {
    let request_host = request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let local_origin = request_host.as_ref().map(|value| format!("http://{value}"));
    let (parts, body) = request.into_parts();
    let is_shell_document = parts.uri.path() == "/" || parts.uri.path() == "/index.html";
    let path = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/");
    let url = format!("{}{}", state.upstream.http_base(), path);
    let method = parts.method.clone();

    let mut headers = parts.headers;
    clean_request_headers(&mut headers);
    let host = HeaderValue::from_str(&state.upstream_host)
        .unwrap_or_else(|_| HeaderValue::from_static("invalid.localhost"));
    headers.insert(HOST, host);
    if !state.shell {
        rewrite_app_origin_headers(
            &mut headers,
            &state.upstream_origin,
            request_host.as_deref(),
        );
    }

    let mut upstream = state.client.request(method.clone(), url).headers(headers);
    if !matches!(method, Method::GET | Method::HEAD) {
        let stream = body.into_data_stream();
        upstream = upstream.body(reqwest::Body::wrap_stream(stream));
    }

    let response = match upstream.send().await {
        Ok(response) => response,
        Err(error) => {
            debug!(%error, "gateway upstream request failed");
            return (
                StatusCode::BAD_GATEWAY,
                format!("orqella gateway could not reach the hub: {error}"),
            )
                .into_response();
        }
    };

    let status = response.status();
    let mut headers = response.headers().clone();
    clean_response_headers(&mut headers);
    if !state.shell {
        rewrite_app_response_headers(
            &mut headers,
            local_origin.as_deref(),
            &state.upstream_canonical_host,
            state.upstream.canonical_port,
            &state.shell_origin,
        );
    }

    if state.shell && is_shell_document {
        let is_html = headers
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/html"));
        if is_html {
            let bytes = match response.bytes().await {
                Ok(bytes) => bytes,
                Err(error) => {
                    return (
                        StatusCode::BAD_GATEWAY,
                        format!("orqella gateway could not read the shell: {error}"),
                    )
                        .into_response();
                }
            };
            let mut html = String::from_utf8_lossy(&bytes).to_string();
            let shim = format!("<script src=\"{}\"></script>", state.shim_path);
            if html.contains("<script src=\"/assets/app.js\"></script>") {
                html = html.replace(
                    "<script src=\"/assets/app.js\"></script>",
                    &format!("{shim}<script src=\"/assets/app.js\"></script>"),
                );
            } else if let Some(index) = html.rfind("</body>") {
                html.insert_str(index, &shim);
            }
            rewrite_shell_csp(&mut headers, &state.app_origins);
            headers.remove("content-length");
            let mut output = Response::new(Body::from(html));
            *output.status_mut() = status;
            *output.headers_mut() = headers;
            return output;
        }
    }

    let body = Body::from_stream(response.bytes_stream());
    let mut output = Response::new(body);
    *output.status_mut() = status;
    *output.headers_mut() = headers;
    output
}

async fn proxy_websocket(
    state: Arc<ProxyState>,
    ws: WebSocketUpgrade,
    request: Request,
) -> Response {
    let (parts, _) = request.into_parts();
    let request_host = parts
        .headers
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let path = parts
        .uri
        .path_and_query()
        .map(|value| value.as_str())
        .unwrap_or("/")
        .to_string();
    let request_path = parts.uri.path().to_string();
    let mut headers = parts.headers;
    clean_request_headers(&mut headers);
    let host = HeaderValue::from_str(&state.upstream_host)
        .unwrap_or_else(|_| HeaderValue::from_static("invalid.localhost"));
    headers.insert(HOST, host);
    if !state.shell {
        rewrite_app_origin_headers(
            &mut headers,
            &state.upstream_origin,
            request_host.as_deref(),
        );
    }
    let upstream_origin = state.upstream_origin.clone();
    let upstream_url = format!("{}{}", state.upstream.ws_base(), path);
    debug!(
        path = %request_path,
        "gateway WebSocket upgrade"
    );

    ws.on_upgrade(move |client| async move {
        if let Err(error) = bridge_websocket(client, upstream_url, headers, upstream_origin).await {
            debug!(%error, "gateway WebSocket bridge ended");
        }
    })
}

async fn bridge_websocket(
    client: WebSocket,
    upstream_url: String,
    headers: HeaderMap,
    _upstream_origin: String,
) -> Result<()> {
    let mut request = http::Request::builder()
        .method(Method::GET)
        .uri(&upstream_url)
        .body(())
        .context("build upstream WebSocket request")?;
    {
        let target = request.headers_mut();
        for (name, value) in headers.iter() {
            target.insert(name.clone(), value.clone());
        }
        // `connect_async` uses a caller-supplied request as-is, so add the
        // fresh client handshake headers ourselves.
        target.insert("connection", HeaderValue::from_static("Upgrade"));
        target.insert("upgrade", HeaderValue::from_static("websocket"));
        target.insert("sec-websocket-version", HeaderValue::from_static("13"));
        let key = tokio_tungstenite::tungstenite::handshake::client::generate_key();
        target.insert(
            "sec-websocket-key",
            HeaderValue::from_str(&key).context("WebSocket key")?,
        );
    }
    let (upstream, response) = tokio_tungstenite::connect_async(request)
        .await
        .with_context(|| format!("connect upstream WebSocket {upstream_url}"))?;
    debug!(status = ?response.status(), "gateway upstream WebSocket connected");
    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();

    loop {
        tokio::select! {
            message = client_rx.next() => {
                let Some(message) = message else { break; };
                let message = message.context("read client WebSocket message")?;
                let close = matches!(message, ClientMessage::Close(_));
                upstream_tx
                    .send(to_upstream_message(message))
                    .await
                    .context("write upstream WebSocket message")?;
                if close {
                    break;
                }
            }
            message = upstream_rx.next() => {
                let Some(message) = message else { break; };
                let message = message.context("read upstream WebSocket message")?;
                let close = matches!(message, UpstreamMessage::Close(_));
                client_tx
                    .send(to_client_message(message))
                    .await
                    .context("write client WebSocket message")?;
                if close {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn to_upstream_message(message: ClientMessage) -> UpstreamMessage {
    match message {
        ClientMessage::Text(value) => UpstreamMessage::Text(value.as_str().into()),
        ClientMessage::Binary(value) => UpstreamMessage::Binary(value),
        ClientMessage::Ping(value) => UpstreamMessage::Ping(value),
        ClientMessage::Pong(value) => UpstreamMessage::Pong(value),
        ClientMessage::Close(frame) => UpstreamMessage::Close(frame.map(|frame| {
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: frame.code.into(),
                reason: frame.reason.as_str().into(),
            }
        })),
    }
}

fn to_client_message(message: UpstreamMessage) -> ClientMessage {
    match message {
        UpstreamMessage::Text(value) => ClientMessage::Text(value.as_str().into()),
        UpstreamMessage::Binary(value) => ClientMessage::Binary(value),
        UpstreamMessage::Ping(value) => ClientMessage::Ping(value),
        UpstreamMessage::Pong(value) => ClientMessage::Pong(value),
        UpstreamMessage::Close(frame) => ClientMessage::Close(frame.map(|frame| CloseFrame {
            code: frame.code.into(),
            reason: frame.reason.as_str().into(),
        })),
        UpstreamMessage::Frame(_) => ClientMessage::Close(None),
    }
}

fn rewrite_app_response_headers(
    headers: &mut HeaderMap,
    local_origin: Option<&str>,
    canonical_host: &str,
    upstream_port: u16,
    shell_origin: &str,
) {
    if let Some(local_origin) = local_origin
        && let Some(value) = headers.get(LOCATION).cloned()
        && let Ok(location) = value.to_str()
    {
        let canonical = format!("http://{canonical_host}:{upstream_port}");
        let canonical_bare = format!("http://{canonical_host}");
        if let Some(path) = location
            .strip_prefix(&canonical)
            .or_else(|| location.strip_prefix(&canonical_bare))
            && (path.is_empty() || path.starts_with(['/', '?', '#']))
        {
            let next = format!("{}{}", local_origin.trim_end_matches('/'), path);
            if let Ok(next) = HeaderValue::from_str(&next) {
                headers.insert(LOCATION, next);
            }
        }
    }
    // Upstream cookies belong to the upstream host. The desktop app origin is
    // a loopback alias, so an explicit Domain would be rejected by the WebView.
    let cookies = headers
        .get_all(SET_COOKIE)
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    if !cookies.is_empty() {
        headers.remove(SET_COOKIE);
        for value in cookies {
            let Ok(text) = value.to_str() else {
                continue;
            };
            let rebuilt = text
                .split(';')
                .filter(|part| !part.trim().to_ascii_lowercase().starts_with("domain="))
                .collect::<Vec<_>>()
                .join(";");
            if let Ok(value) = HeaderValue::from_str(&rebuilt) {
                headers.append(SET_COOKIE, value);
            }
        }
    }
    constrain_app_frame_ancestors(headers, shell_origin);
}

/// Only the desktop shell may frame a proxied app. This blocks external sites
/// from framing an app origin that carries the user's app cookies.
fn constrain_app_frame_ancestors(headers: &mut HeaderMap, shell_origin: &str) {
    if shell_origin.is_empty() {
        return;
    }
    let existing = headers
        .get(CONTENT_SECURITY_POLICY)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let mut parts = existing
        .as_deref()
        .map(|value| {
            value
                .split(';')
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut found = false;
    for part in &mut parts {
        if part
            .split_whitespace()
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("frame-ancestors"))
        {
            *part = format!("frame-ancestors {shell_origin}");
            found = true;
            break;
        }
    }
    if !found {
        parts.push(format!("frame-ancestors {shell_origin}"));
    }
    if let Ok(value) = HeaderValue::from_str(&parts.join("; ")) {
        headers.insert(CONTENT_SECURITY_POLICY, value);
    }
}

fn rewrite_shell_csp(headers: &mut HeaderMap, app_origins: &Arc<RwLock<HashMap<String, String>>>) {
    let origins = app_origins.read().expect("app origin map");
    if origins.is_empty() {
        return;
    }
    let mut sources = origins
        .values()
        .filter_map(|origin| {
            url::Url::parse(origin)
                .ok()
                .map(|url| format!("http://{}", url.authority()))
        })
        .collect::<Vec<_>>();
    sources.sort();
    sources.dedup();
    let Some(value) = headers
        .get(CONTENT_SECURITY_POLICY)
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    let directive = "frame-src";
    let mut parts = value
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut found = false;
    for part in &mut parts {
        if part.split_whitespace().next() == Some(directive) {
            for source in &sources {
                if !part.split_whitespace().any(|existing| existing == source) {
                    part.push(' ');
                    part.push_str(source);
                }
            }
            found = true;
            break;
        }
    }
    if !found {
        let mut frame = format!("{directive} 'self'");
        for source in &sources {
            frame.push(' ');
            frame.push_str(source);
        }
        parts.push(frame);
    }
    if let Ok(value) = HeaderValue::from_str(&parts.join("; ")) {
        headers.insert(CONTENT_SECURITY_POLICY, value);
    }
}

fn clean_request_headers(headers: &mut HeaderMap) {
    let connection_headers = headers
        .get(CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for name in [
        HOST.as_str(),
        CONNECTION.as_str(),
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        UPGRADE.as_str(),
        "sec-websocket-key",
        "sec-websocket-version",
        "sec-websocket-extensions",
        "forwarded",
        "x-forwarded-for",
        "x-forwarded-host",
        "x-forwarded-proto",
        "x-forwarded-port",
        "x-orqella-origin",
        "x-orqella-referer",
    ] {
        headers.remove(name);
    }
    for name in connection_headers {
        if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
            headers.remove(name);
        }
    }
    // The upstream hub is loopback-only; never let a client supply a proxy
    // route through this process.
    headers.remove("proxy-connection");
}

fn clean_response_headers(headers: &mut HeaderMap) {
    let connection_headers = headers
        .get(CONNECTION)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for name in [
        CONNECTION.as_str(),
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        TRANSFER_ENCODING.as_str(),
        UPGRADE.as_str(),
        "proxy-connection",
    ] {
        headers.remove(name);
    }
    for name in connection_headers {
        if let Ok(name) = HeaderName::from_bytes(name.as_bytes()) {
            headers.remove(name);
        }
    }
}

fn host_value_port(value: &str) -> Option<u16> {
    if let Some(rest) = value.strip_prefix('[') {
        return rest
            .split_once("]:")
            .and_then(|(_, port)| port.parse().ok());
    }
    value
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
}

fn origin_matches_request_host(origin: &str, request_host: &str) -> bool {
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    host.eq_ignore_ascii_case(host_without_port(request_host))
        && url.port_or_known_default() == host_value_port(request_host)
}

/// Rewrite the app's Origin/Referer to the canonical upstream origin only for
/// requests that originate from the local app origin. Cross-site requests keep
/// their real Origin/Referer so the app's own CSRF/CSWSH checks still work.
fn rewrite_app_origin_headers(
    headers: &mut HeaderMap,
    upstream_origin: &str,
    request_host: Option<&str>,
) {
    if let Some(origin) = headers.get(ORIGIN).cloned() {
        headers.insert("x-orqella-origin", origin);
    }
    if let Some(referer) = headers.get(REFERER).cloned() {
        headers.insert("x-orqella-referer", referer);
    }

    let same_origin = headers
        .get("sec-fetch-site")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("same-origin") || value.eq_ignore_ascii_case("none")
        });
    let origin_matches = headers
        .get(ORIGIN)
        .and_then(|value| value.to_str().ok())
        .zip(request_host)
        .is_some_and(|(origin, request_host)| origin_matches_request_host(origin, request_host));
    if !(same_origin || origin_matches) {
        return;
    }

    if headers.contains_key(ORIGIN)
        && let Ok(value) = HeaderValue::from_str(upstream_origin)
    {
        headers.insert(ORIGIN, value);
    }
    if headers.contains_key(REFERER)
        && let Ok(value) = HeaderValue::from_str(&format!("{upstream_origin}/"))
    {
        headers.insert(REFERER, value);
    }
}

/// Unused imports kept out of the public surface until the app lease is added.
#[allow(dead_code)]
fn _header_name(value: &'static str) -> HeaderName {
    HeaderName::from_static(value)
}

/// True when a header should not be forwarded across the gateway.
pub fn is_hop_by_hop(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

/// Return true when the request is safe to forward to the configured upstream.
pub fn host_is_allowed(host: &str, shell_host: &str, _port: u16) -> bool {
    let host = host_without_port(host);
    is_loopback_host(host)
        || host.eq_ignore_ascii_case(shell_host)
        || host.ends_with(&format!(".{shell_host}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::header::CONTENT_LENGTH;
    use axum::routing::get;
    use axum::{Router, extract::ws::Message};
    use proptest::prelude::*;
    use std::time::Duration;

    async fn get_app_origin(client: &reqwest::Client, origin: &str) -> reqwest::Response {
        let url = url::Url::parse(origin).expect("app origin URL");
        let host = url.host_str().expect("app origin host");
        if host.starts_with("127.") || host == "localhost" {
            return client.get(origin).send().await.expect("app origin request");
        }
        // The app host is normally supplied by the daemon's managed hosts
        // entries. Tests avoid requiring /etc/hosts by dialing loopback and
        // sending the canonical Host header directly.
        let port = url.port().expect("app origin port");
        client
            .get(format!("http://127.0.0.1:{port}/"))
            .header(HOST, format!("{host}:{port}"))
            .send()
            .await
            .expect("app origin request")
    }

    async fn start_fake_hub() -> (SocketAddr, tokio::task::JoinHandle<()>) {
        let app = Router::new()
            .route("/", get(|headers: HeaderMap| async move {
                let host = headers
                    .get(HOST)
                    .and_then(|value| value.to_str().ok())
                    .unwrap_or("missing")
                    .to_string();
                (
                    [
                        (CONTENT_TYPE, "text/html; charset=utf-8"),
                        (
                            CONTENT_SECURITY_POLICY,
                            "default-src 'self'; frame-src 'self' http://*.orqella.localhost:*",
                        ),
                    ],
                    format!(
                        "<html><body>host={host}<script src=\"/assets/app.js\"></script></body></html>"
                    ),
                )
            }))
            .route("/event", get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    "data: hello\n\n",
                )
            }))
            .route("/soak/event", get(|| async {
                let stream = futures_util::stream::unfold(0u32, |count| async move {
                    if count >= 30 {
                        return None;
                    }
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    let item = Ok::<_, std::convert::Infallible>(format!("data: tick {count}\n\n"));
                    Some((item, count + 1))
                });
                (
                    [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                    Body::from_stream(stream),
                )
            }))
            .route("/ws", get(|ws: WebSocketUpgrade| async move {
                ws.on_upgrade(|mut socket| async move {
                    while let Some(Ok(message)) = socket.recv().await {
                        if let Message::Text(text) = message {
                            let _ = socket.send(Message::Text(text)).await;
                        }
                    }
                })
            }));
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (addr, task)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn maps_localhost_app_hosts_to_mdns_names() {
        assert_eq!(
            macos_browser_host("opencode.orqella.localhost"),
            "opencode.orqella.local"
        );
        assert_eq!(
            macos_browser_host("opencode.laptop.orqella.localhost"),
            "opencode.laptop.orqella.local"
        );
    }

    #[test]
    fn accepts_shell_and_loopback_hosts() {
        assert!(host_is_allowed(
            "orqella.localhost:19477",
            "orqella.localhost",
            19477
        ));
        assert!(host_is_allowed(
            "127.0.0.1:19477",
            "orqella.localhost",
            19477
        ));
        assert!(host_is_allowed(
            "opencode.orqella.localhost:19477",
            "orqella.localhost",
            19477
        ));
        assert!(!host_is_allowed(
            "evil.example:19477",
            "orqella.localhost",
            19477
        ));
    }

    #[test]
    fn strips_hop_by_hop_headers() {
        let mut headers = HeaderMap::new();
        headers.insert(CONNECTION, HeaderValue::from_static("close"));
        headers.insert(CONTENT_LENGTH, HeaderValue::from_static("4"));
        clean_response_headers(&mut headers);
        assert!(!headers.contains_key(CONNECTION));
        assert!(headers.contains_key(CONTENT_LENGTH));
    }
    #[tokio::test]
    async fn proxies_http_sse_and_websocket() {
        let (upstream_addr, upstream_task) = start_fake_hub().await;
        let gateway = Gateway::start(GatewayConfig {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            shell_host: "orqella.localhost".into(),
            upstream: Upstream::http("orqella.localhost", "127.0.0.1", upstream_addr.port()),
            app_origins: Vec::new(),
            origin_namespace: 0,
        })
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let body = client
            .get(gateway.base_url.clone())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(body.contains("host=orqella.localhost:"), "{body}");

        let event = client
            .get(format!("{}event", gateway.base_url))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(event.contains("data: hello"));

        let ws_url = format!("ws://{}/ws", gateway.local_addr);
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text("ping".into()))
            .await
            .unwrap();
        let reply = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(
            reply,
            tokio_tungstenite::tungstenite::Message::Text("ping".into())
        );

        gateway.stop().await;
        upstream_task.abort();
    }

    #[tokio::test]
    async fn exposes_isolated_app_origin_and_shell_shim() {
        let (upstream_addr, upstream_task) = start_fake_hub().await;
        let gateway = Gateway::start(GatewayConfig {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            shell_host: "orqella.localhost".into(),
            upstream: Upstream::http("orqella.localhost", "127.0.0.1", upstream_addr.port()),
            app_origins: vec![AppOriginConfig::local(
                IpAddr::from([127, 0, 0, 2]),
                "opencode",
                "opencode.orqella.localhost",
            )],
            origin_namespace: 0,
        })
        .await
        .unwrap();
        let app_origin = gateway
            .app_origin(IpAddr::from([127, 0, 0, 2]))
            .await
            .expect("app origin");
        let client = reqwest::Client::new();
        let body = client
            .get(&app_origin)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(body.contains("host=opencode.orqella.localhost:"), "{body}");
        let shell = client.get(gateway.base_url.clone()).send().await.unwrap();
        let csp = shell
            .headers()
            .get(CONTENT_SECURITY_POLICY)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(
            csp.contains(app_origin.trim_end_matches('/')),
            "origin {app_origin} missing from CSP: {csp}"
        );
        let html = shell.text().await.unwrap();
        assert!(html.contains(gateway.shim_path()), "{html}");
        let shim = client
            .get(format!(
                "{}{}",
                gateway.base_url,
                gateway.shim_path().trim_start_matches('/')
            ))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(shim.contains("__ORQELLA_NATIVE_TOKEN"), "{shim}");
        let token = shim
            .split("const nativeToken = \"")
            .nth(1)
            .and_then(|value| value.split('"').next())
            .unwrap();
        let health = client
            .get(format!("{}__orqella/native/health", gateway.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(health.status(), reqwest::StatusCode::FORBIDDEN);
        let health = client
            .get(format!("{}__orqella/native/health", gateway.base_url))
            .header("x-orqella-native", token)
            .send()
            .await
            .unwrap();
        assert!(health.status().is_success());

        let opened = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let opened_for_opener = opened.clone();
        let expected_open_url = if cfg!(target_os = "macos") {
            app_origin.clone()
        } else {
            format!("http://127.0.0.2:{}/", gateway.local_addr.port())
        };
        let expected_for_opener = expected_open_url.clone();
        gateway.set_app_window_opener(Arc::new(move |request| {
            assert_eq!(request.url, expected_for_opener);
            opened_for_opener.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let open = client
            .post(format!("{}__orqella/native/open-app", gateway.base_url))
            .header("x-orqella-native", token)
            .json(&serde_json::json!({
                "url": expected_open_url,
                "title": "OpenCode"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(open.status(), reqwest::StatusCode::ACCEPTED);
        assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 1);
        gateway.stop().await;
        upstream_task.abort();
    }
    #[test]
    fn rewrites_app_cookies_and_locations() {
        let mut headers = HeaderMap::new();
        headers.insert(
            SET_COOKIE,
            HeaderValue::from_static(
                "sid=abc; Domain=opencode.orqella.localhost; Path=/; HttpOnly",
            ),
        );
        headers.insert(
            LOCATION,
            HeaderValue::from_static("http://opencode.orqella.localhost:9477/login"),
        );
        rewrite_app_response_headers(
            &mut headers,
            Some("http://127.0.0.2:19477"),
            "opencode.orqella.localhost",
            9477,
            "http://127.0.0.1:19477",
        );
        let cookie = headers.get(SET_COOKIE).unwrap().to_str().unwrap();
        assert!(!cookie.to_ascii_lowercase().contains("domain="), "{cookie}");
        assert_eq!(
            headers.get(LOCATION).unwrap().to_str().unwrap(),
            "http://127.0.0.2:19477/login"
        );
        assert_eq!(
            headers
                .get(CONTENT_SECURITY_POLICY)
                .unwrap()
                .to_str()
                .unwrap(),
            "frame-ancestors http://127.0.0.1:19477"
        );

        let mut lookalike = HeaderMap::new();
        lookalike.insert(
            LOCATION,
            HeaderValue::from_static("http://opencode.orqella.localhost.evil.com/phish"),
        );
        rewrite_app_response_headers(
            &mut lookalike,
            Some("http://127.0.0.2:19477"),
            "opencode.orqella.localhost",
            9477,
            "http://127.0.0.1:19477",
        );
        assert_eq!(
            lookalike.get(LOCATION).unwrap().to_str().unwrap(),
            "http://opencode.orqella.localhost.evil.com/phish"
        );
    }
    #[tokio::test]
    async fn leases_new_app_origin_without_restart() {
        let (upstream_addr, upstream_task) = start_fake_hub().await;
        let gateway = Gateway::start(GatewayConfig {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            shell_host: "orqella.localhost".into(),
            upstream: Upstream::http("orqella.localhost", "127.0.0.1", upstream_addr.port()),
            app_origins: Vec::new(),
            origin_namespace: 0,
        })
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let shim = client
            .get(format!(
                "{}{}",
                gateway.base_url,
                gateway.shim_path().trim_start_matches('/')
            ))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let token = shim
            .split("const nativeToken = \"")
            .nth(1)
            .and_then(|value| value.split('"').next())
            .unwrap();
        let response = client
            .post(format!("{}__orqella/native/app-origin", gateway.base_url))
            .header("x-orqella-native", token)
            .json(&serde_json::json!({
                "node": "local",
                "mount": "newapp",
                "canonical_host": "newapp.orqella.localhost"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK, "{response:?}");
        let body: serde_json::Value = response.json().await.unwrap();
        let origin = body["origin"].as_str().unwrap();
        if cfg!(target_os = "macos") {
            assert!(
                origin.starts_with("http://newapp.orqella.local:"),
                "{origin}"
            );
        } else {
            assert!(origin.starts_with("http://127."), "{origin}");
        }
        let app = get_app_origin(&client, origin).await.text().await.unwrap();
        assert!(app.contains("host=newapp.orqella.localhost:"), "{app}");
        let shell = client
            .get(gateway.base_url.clone())
            .send()
            .await
            .unwrap()
            .headers()
            .get(CONTENT_SECURITY_POLICY)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_string();
        assert!(shell.contains(origin.trim_end_matches('/')), "{shell}");
        gateway.stop().await;
        upstream_task.abort();
    }
    #[tokio::test]
    #[ignore = "30-second SSE and WebSocket soak"]
    async fn soak_keeps_sse_and_websocket_alive() {
        let (upstream_addr, upstream_task) = start_fake_hub().await;
        let gateway = Gateway::start(GatewayConfig {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            shell_host: "orqella.localhost".into(),
            upstream: Upstream::http("orqella.localhost", "127.0.0.1", upstream_addr.port()),
            app_origins: Vec::new(),
            origin_namespace: 0,
        })
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let event = client
            .get(format!("{}soak/event", gateway.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(event.status(), reqwest::StatusCode::OK);
        let mut stream = event.bytes_stream();
        let sse = tokio::spawn(async move {
            let mut ticks = 0;
            while let Some(chunk) = stream.next().await {
                let value = chunk.expect("SSE chunk");
                ticks += String::from_utf8_lossy(&value)
                    .matches("data: tick")
                    .count();
            }
            ticks
        });

        let ws_url = format!("ws://{}/ws", gateway.local_addr);
        let (mut socket, _) = tokio_tungstenite::connect_async(ws_url).await.unwrap();
        for index in 0..25 {
            let payload = format!("soak-{index}");
            socket
                .send(tokio_tungstenite::tungstenite::Message::Text(
                    payload.clone().into(),
                ))
                .await
                .unwrap();
            let reply = tokio::time::timeout(Duration::from_secs(3), socket.next())
                .await
                .expect("websocket reply timed out")
                .expect("websocket closed")
                .expect("websocket frame");
            assert_eq!(
                reply,
                tokio_tungstenite::tungstenite::Message::Text(payload.into())
            );
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let ticks = tokio::time::timeout(Duration::from_secs(15), sse)
            .await
            .expect("SSE stream did not finish")
            .expect("SSE task");
        assert!(ticks >= 25, "only {ticks} SSE ticks arrived");

        gateway.stop().await;
        upstream_task.abort();
    }
    proptest! {
        #[test]
        fn loopback_hosts_are_allowed(last in 2u8..=254) {
            let host = format!("127.0.0.{last}");
            prop_assert!(host_is_allowed(&host, "orqella.localhost", 19477));
        }

        #[test]
        fn hub_subdomains_are_allowed(label in "[a-z][a-z0-9-]{0,20}") {
            let host = format!("{label}.orqella.localhost");
            prop_assert!(host_is_allowed(&host, "orqella.localhost", 19477));
        }

        #[test]
        fn unrelated_hosts_are_rejected(label in "[a-z][a-z0-9-]{0,20}") {
            let host = format!("{label}.example.com");
            prop_assert!(!host_is_allowed(&host, "orqella.localhost", 19477));
        }

        #[test]
        fn arbitrary_ports_do_not_break_host_matching(port in proptest::arbitrary::any::<u16>()) {
            let host = format!("orqella.localhost:{port}");
            prop_assert!(host_is_allowed(&host, "orqella.localhost", port));
        }

        #[test]
        fn ipv6_loopback_with_port_is_allowed(port in proptest::arbitrary::any::<u16>()) {
            let host = format!("[::1]:{port}");
            prop_assert!(host_is_allowed(&host, "orqella.localhost", port));
        }

        #[test]
        fn custom_headers_survive_request_cleaning(name in "x-[a-z]{1,12}") {
            let header = HeaderName::from_bytes(name.as_bytes()).unwrap();
            let mut headers = HeaderMap::new();
            headers.insert(header.clone(), HeaderValue::from_static("value"));
            clean_request_headers(&mut headers);
            prop_assert_eq!(headers.get(&header).and_then(|value| value.to_str().ok()), Some("value"));
        }

    }

    #[test]
    fn same_origin_app_requests_are_rewritten() {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static("http://127.0.0.2:19477"));
        headers.insert(
            REFERER,
            HeaderValue::from_static("http://127.0.0.2:19477/app"),
        );
        rewrite_app_origin_headers(
            &mut headers,
            "http://opencode.orqella.localhost:9477",
            Some("127.0.0.2:19477"),
        );
        assert_eq!(
            headers.get(ORIGIN).unwrap().to_str().unwrap(),
            "http://opencode.orqella.localhost:9477"
        );
        assert_eq!(
            headers.get(REFERER).unwrap().to_str().unwrap(),
            "http://opencode.orqella.localhost:9477/"
        );
    }

    #[test]
    fn cross_site_app_requests_keep_their_origin() {
        let mut headers = HeaderMap::new();
        headers.insert(ORIGIN, HeaderValue::from_static("http://evil.example"));
        headers.insert(
            REFERER,
            HeaderValue::from_static("http://evil.example/path"),
        );
        headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
        rewrite_app_origin_headers(
            &mut headers,
            "http://opencode.orqella.localhost:9477",
            Some("127.0.0.2:19477"),
        );
        assert_eq!(
            headers.get(ORIGIN).unwrap().to_str().unwrap(),
            "http://evil.example"
        );
        assert_eq!(
            headers.get(REFERER).unwrap().to_str().unwrap(),
            "http://evil.example/path"
        );
    }

    #[test]
    fn rebinding_hostnames_are_rejected() {
        assert!(!host_is_allowed(
            "127.0.0.1.attacker.example:19477",
            "orqella.localhost",
            19477
        ));
        assert!(!host_is_allowed(
            "127.0.0.1.nip.io:19477",
            "orqella.localhost",
            19477
        ));
        assert!(host_is_allowed("127.0.0.2", "orqella.localhost", 19477));
    }
    #[tokio::test]
    async fn rejects_rebinding_hosts_at_the_proxy() {
        let (upstream_addr, upstream_task) = start_fake_hub().await;
        let gateway = Gateway::start(GatewayConfig {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            shell_host: "orqella.localhost".into(),
            upstream: Upstream::http("orqella.localhost", "127.0.0.1", upstream_addr.port()),
            app_origins: Vec::new(),
            origin_namespace: 0,
        })
        .await
        .unwrap();
        let client = reqwest::Client::new();
        let response = client
            .get(gateway.base_url.clone())
            .header(HOST, "127.0.0.1.attacker.example:19477")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::MISDIRECTED_REQUEST);
        gateway.stop().await;
        upstream_task.abort();
    }

    #[tokio::test]
    async fn routes_authenticated_native_notifications() {
        let (upstream_addr, upstream_task) = start_fake_hub().await;
        let gateway = Gateway::start(GatewayConfig {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            shell_host: "orqella.localhost".into(),
            upstream: Upstream::http("orqella.localhost", "127.0.0.1", upstream_addr.port()),
            app_origins: Vec::new(),
            origin_namespace: 0,
        })
        .await
        .unwrap();
        let received = Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = received.clone();
        gateway.set_notification_sender(Arc::new(move |title, body| {
            sink.lock().unwrap().push((title, body));
        }));
        let client = reqwest::Client::new();
        let shim = client
            .get(format!(
                "{}{}",
                gateway.base_url,
                gateway.shim_path().trim_start_matches('/')
            ))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let token = shim
            .split("const nativeToken = \"")
            .nth(1)
            .and_then(|value| value.split('"').next())
            .unwrap();

        let denied = client
            .post(format!("{}__orqella/native/notify", gateway.base_url))
            .json(&serde_json::json!({"title": "Denied"}))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

        let response = client
            .post(format!("{}__orqella/native/notify", gateway.base_url))
            .header("x-orqella-native", token)
            .json(&serde_json::json!({"title": "Build finished", "body": "The module is ready."}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::OK);
        let payloads = received.lock().unwrap().clone();
        assert_eq!(
            payloads,
            vec![(
                "Build finished".to_string(),
                "The module is ready.".to_string()
            )]
        );

        let invalid = client
            .post(format!("{}__orqella/native/notify", gateway.base_url))
            .header("x-orqella-native", token)
            .json(&serde_json::json!({"title": "  "}))
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), reqwest::StatusCode::BAD_REQUEST);

        let cross_origin = client
            .post(format!("{}__orqella/native/notify", gateway.base_url))
            .header("x-orqella-native", token)
            .header(ORIGIN, "http://evil.example")
            .json(&serde_json::json!({"title": "Cross origin"}))
            .send()
            .await
            .unwrap();
        assert_eq!(cross_origin.status(), reqwest::StatusCode::FORBIDDEN);

        gateway.stop().await;
        upstream_task.abort();
    }

    #[tokio::test]
    async fn routes_authenticated_settings_open() {
        let (upstream_addr, upstream_task) = start_fake_hub().await;
        let gateway = Gateway::start(GatewayConfig {
            bind: IpAddr::from([127, 0, 0, 1]),
            port: 0,
            shell_host: "orqella.localhost".into(),
            upstream: Upstream::http("orqella.localhost", "127.0.0.1", upstream_addr.port()),
            app_origins: Vec::new(),
            origin_namespace: 0,
        })
        .await
        .unwrap();
        let opened = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sink = opened.clone();
        gateway.set_settings_opener(Arc::new(move || {
            sink.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        let client = reqwest::Client::new();
        let shim = client
            .get(format!(
                "{}{}",
                gateway.base_url,
                gateway.shim_path().trim_start_matches('/')
            ))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        let token = shim
            .split("const nativeToken = \"")
            .nth(1)
            .and_then(|value| value.split('"').next())
            .unwrap();
        assert!(shim.contains("native/settings"), "{shim}");

        let denied = client
            .post(format!("{}__orqella/native/settings", gateway.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
        let response = client
            .post(format!("{}__orqella/native/settings", gateway.base_url))
            .header("x-orqella-native", token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::ACCEPTED);
        assert_eq!(opened.load(std::sync::atomic::Ordering::SeqCst), 1);

        gateway.stop().await;
        upstream_task.abort();
    }
}
