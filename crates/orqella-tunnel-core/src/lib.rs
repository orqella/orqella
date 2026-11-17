//! Shared SSH tunnel core for Orqella native clients.
//!
//! The core is deliberately foreground-oriented: it opens a local loopback
//! listener and keeps an SSH direct-tcpip channel per accepted connection. The
//! mobile shell should call `stop` when the app is backgrounded.

uniffi::setup_scaffolding!();

mod link;
mod mobile;

pub use link::{
    MobileLinkDevice, MobileLinkError, MobileLinkRegistration, MobileLinkTunnel,
    MobileLinkTunnelConfig, link_devices, link_generate_secret, link_login, link_register_device,
    start_link,
};
pub use mobile::{
    GeneratedSshKey, MobileTunnel, MobileTunnelConfig, MobileTunnelError, SshKeyInfo,
    generate_ssh_key, ssh_key_info,
};

use anyhow::{Context, Result, bail};
use russh::client::{self, Handle};
use russh::keys::{
    Certificate, PrivateKey, PrivateKeyWithHashAlg, PublicKeyOrCertificate, decode_secret_key,
    load_secret_key,
};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

/// Isolated app origins occupy the loopback /8. The desktop gateway uses
/// 127.0.0.2-254; mobile caps the range at 127 so the shell CSP stays small
/// enough for every WebView.
const APP_ORIGIN_FIRST: u8 = 2;
const APP_ORIGIN_LAST: u8 = 127;

#[derive(Debug, Clone)]
pub struct TunnelConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub private_key: std::path::PathBuf,
    pub key_passphrase: Option<String>,
    pub expected_host_fingerprint: Option<String>,
    pub remote_host: String,
    pub remote_port: u16,
    pub local_port: u16,
    /// Optional per-launch bearer checked on the local HTTP listener. Mobile
    /// clients set this so other apps on the device cannot use the fixed
    /// loopback port.
    pub local_token: Option<String>,
}

/// The per-launch tunnel cookie is derived from the device's public key. The
/// mobile shell can compute the same value from its stored public key without
/// sending the key itself to the hub.
pub fn local_tunnel_token(public_key: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(public_key.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Clone)]
struct Handler {
    expected_fingerprint: Option<String>,
}

impl client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        key: &PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        if let Some(expected) = &self.expected_fingerprint {
            let actual = key.public_key().fingerprint(Default::default()).to_string();
            if &actual != expected {
                tracing::warn!(%actual, %expected, "SSH host key fingerprint mismatch");
                return Ok(false);
            }
        }
        Ok(true)
    }
}

struct AppListener {
    host: String,
    addr: SocketAddr,
    stop_tx: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

pub struct Tunnel {
    pub local_addr: SocketAddr,
    session: Arc<Handle<Handler>>,
    remote_host: String,
    remote_port: u16,
    local_token: Option<String>,
    stop_tx: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    app_listeners: Mutex<Vec<AppListener>>,
}

impl Tunnel {
    /// Allocate an isolated loopback origin for one full app mount.
    ///
    /// The listener speaks plain HTTP to the WebView and rewrites the Host
    /// header to the app's canonical host before forwarding over the existing
    /// SSH direct-tcpip channel. Each app gets its own 127.0.0.x address, so
    /// cookies and origin storage cannot bleed between apps or into the shell.
    /// The same tunnel cookie is still required on the app listener.
    pub async fn open_app_origin(&self, host: &str) -> Result<SocketAddr> {
        let host = validate_app_host(host)?;
        {
            let listeners = self
                .app_listeners
                .lock()
                .map_err(|_| anyhow::anyhow!("app origin state is poisoned"))?;
            if let Some(listener) = listeners
                .iter()
                .find(|listener| listener.host.eq_ignore_ascii_case(&host))
            {
                return Ok(listener.addr);
            }
        }

        let slots = u16::from(APP_ORIGIN_LAST - APP_ORIGIN_FIRST) + 1;
        let start = (stable_hash(&host) % u64::from(slots)) as u8;
        let mut bound = None;
        for offset in 0..slots {
            let last = APP_ORIGIN_FIRST + (start + offset as u8) % (slots as u8);
            let addr = SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(127, 0, 0, last)),
                self.local_addr.port(),
            );
            if let Ok(listener) = TcpListener::bind(addr).await {
                bound = Some((addr, listener));
                break;
            }
        }
        let (_requested_addr, listener) = bound.context(
            "could not allocate an isolated loopback origin for the app; \
             127.0.0.2-127 are unavailable",
        )?;
        let local_addr = listener.local_addr()?;

        let (stop_tx, mut stop_rx) = oneshot::channel();
        let session = self.session.clone();
        let remote_host = self.remote_host.clone();
        let remote_port = self.remote_port;
        let local_token = self.local_token.clone();
        let app_host = host.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    _ = &mut stop_rx => {
                        connections.abort_all();
                        break;
                    }
                    accepted = listener.accept() => {
                        let Ok((stream, origin)) = accepted else { break; };
                        if connections.len() >= 64 {
                            tracing::warn!("app origin connection limit reached");
                            continue;
                        }
                        let session = session.clone();
                        let remote_host = remote_host.clone();
                        let local_token = local_token.clone();
                        let app_host = app_host.clone();
                        connections.spawn(async move {
                            if let Err(error) = forward_app(
                                session,
                                stream,
                                remote_host,
                                remote_port,
                                origin,
                                local_token,
                                app_host,
                            )
                            .await
                            {
                                tracing::debug!(%error, "app origin connection ended");
                            }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });

        self.app_listeners
            .lock()
            .map_err(|_| anyhow::anyhow!("app origin state is poisoned"))?
            .push(AppListener {
                host,
                addr: local_addr,
                stop_tx: Some(stop_tx),
                task,
            });
        Ok(local_addr)
    }

    async fn stop_app_listeners(&mut self) {
        let mut listeners = self
            .app_listeners
            .lock()
            .map(|mut listeners| std::mem::take(&mut *listeners))
            .unwrap_or_default();
        for listener in &mut listeners {
            if let Some(stop) = listener.stop_tx.take() {
                let _ = stop.send(());
            }
        }
        for mut listener in listeners {
            let _ = tokio::time::timeout(Duration::from_secs(3), &mut listener.task).await;
            listener.task.abort();
        }
    }

    pub async fn stop(mut self) {
        self.stop_app_listeners().await;
        if let Some(stop) = self.stop_tx.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Ok(mut listeners) = self.app_listeners.lock() {
            for listener in listeners.iter_mut() {
                if let Some(stop) = listener.stop_tx.take() {
                    let _ = stop.send(());
                }
                listener.task.abort();
            }
        }
        if let Some(stop) = self.stop_tx.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
}

fn stable_hash(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub(crate) fn app_host_for_request(node: &str, mount: &str, host: &str) -> Result<String> {
    let host = host.trim();
    if !host.is_empty() {
        return validate_app_host(host);
    }
    let valid_label = |value: &str| {
        !value.is_empty()
            && value.len() <= 63
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    };
    if !valid_label(mount) {
        bail!("app mount is not a valid host label");
    }
    let node = node.trim();
    if !node.is_empty() && !node.eq_ignore_ascii_case("local") && !valid_label(node) {
        bail!("app node is not a valid host label");
    }
    let fallback = if node.is_empty() || node.eq_ignore_ascii_case("local") {
        format!("{mount}.orqella.localhost")
    } else {
        format!("{mount}.{node}.orqella.localhost")
    };
    validate_app_host(&fallback)
}

fn validate_app_host(host: &str) -> Result<String> {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() || host.len() > 253 {
        bail!("app host is empty or too long");
    }
    if !host.is_ascii()
        || host.bytes().any(|byte| {
            byte.is_ascii_control()
                || byte.is_ascii_whitespace()
                || matches!(byte, b'/' | b'\\' | b':' | b'@' | b'#' | b'?')
        })
    {
        bail!("app host contains invalid characters");
    }
    if host.split('.').any(|label| {
        label.is_empty()
            || label.len() > 63
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    }) {
        bail!("app host is not a valid hostname");
    }
    Ok(host)
}

pub async fn start(config: TunnelConfig) -> Result<Tunnel> {
    let key = load_secret_key(&config.private_key, config.key_passphrase.as_deref())
        .with_context(|| format!("could not read SSH key {}", config.private_key.display()))?;
    start_with_key(config, key).await
}

/// Start a tunnel with an already-decoded private key.
///
/// The CLI uses [`start`] so it can read a key from disk. Mobile clients hold
/// key material in the platform secure store and call this through UniFFI.
pub async fn start_with_key(config: TunnelConfig, key: PrivateKey) -> Result<Tunnel> {
    start_with_key_and_cert(config, key, None).await
}

/// Bind the loopback listener that serves the dashboard to the WebView.
///
/// A stable port keeps the WebView origin stable across app launches and
/// tunnel reconnects. Storage such as dashboard layouts and page order is
/// scoped to the origin, so an ephemeral port would silently discard it.
/// Fall back to an OS-assigned port only when the preferred port is taken.
async fn bind_local_listener(preferred_port: u16) -> Result<TcpListener> {
    if preferred_port != 0 {
        match TcpListener::bind(("127.0.0.1", preferred_port)).await {
            Ok(listener) => return Ok(listener),
            Err(error) => {
                tracing::warn!(
                    port = preferred_port,
                    %error,
                    "preferred local dashboard port is unavailable; using an ephemeral port"
                );
            }
        }
    }
    TcpListener::bind(("127.0.0.1", 0))
        .await
        .context("could not bind the local tunnel listener")
}

/// Start a tunnel with an optional short-lived OpenSSH user certificate.
pub async fn start_with_key_and_cert(
    config: TunnelConfig,
    key: PrivateKey,
    certificate: Option<Certificate>,
) -> Result<Tunnel> {
    if config.expected_host_fingerprint.is_none() {
        bail!("host-key fingerprint is required; pass the SHA256 fingerprint from pairing");
    }
    let key = Arc::new(key);
    let ssh_config = Arc::new(client::Config {
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        nodelay: true,
        ..Default::default()
    });
    let mut session = client::connect(
        ssh_config,
        (config.host.as_str(), config.port),
        Handler {
            expected_fingerprint: config.expected_host_fingerprint.clone(),
        },
    )
    .await
    .context("SSH connection failed")?;
    if let Some(certificate) = certificate {
        let auth = session
            .authenticate_openssh_cert(&config.user, key, certificate)
            .await
            .context("SSH certificate authentication failed")?;
        if !auth.success() {
            bail!("SSH server rejected the device certificate: {auth:?}");
        }
    } else {
        let rsa_hash = if key.algorithm().is_rsa() {
            session.best_supported_rsa_hash().await?.flatten()
        } else {
            None
        };
        let auth_key = PrivateKeyWithHashAlg::new(key, rsa_hash);
        let auth = session
            .authenticate_publickey(&config.user, auth_key)
            .await
            .context("SSH key authentication failed")?;
        if !auth.success() {
            bail!("SSH server rejected the key: {auth:?}");
        }
    }

    let listener = bind_local_listener(config.local_port).await?;
    let local_addr = listener.local_addr()?;
    let (stop_tx, mut stop_rx) = oneshot::channel();
    let session = Arc::new(session);
    let remote_host = config.remote_host;
    let remote_port = config.remote_port;
    let local_token = config.local_token;
    let task_session = session.clone();
    let task_remote_host = remote_host.clone();
    let task_local_token = local_token.clone();
    let local_port = local_addr.port();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        loop {
            tokio::select! {
                _ = &mut stop_rx => {
                    connections.abort_all();
                    break;
                }
                accepted = listener.accept() => {
                    let Ok((stream, origin)) = accepted else { break; };
                    if connections.len() >= 64 {
                        tracing::warn!("tunnel connection limit reached");
                        continue;
                    }
                    let session = task_session.clone();
                    let remote_host = task_remote_host.clone();
                    let local_token = task_local_token.clone();
                    connections.spawn(async move {
                        if let Err(error) = forward(
                            session,
                            stream,
                            remote_host,
                            remote_port,
                            origin,
                            local_token,
                            local_port,
                        )
                        .await
                        {
                            tracing::debug!(%error, "tunneled connection ended");
                        }
                    });
                }
                _ = connections.join_next(), if !connections.is_empty() => {}
            }
        }
    });
    Ok(Tunnel {
        local_addr,
        session,
        remote_host,
        remote_port,
        local_token,
        stop_tx: Some(stop_tx),
        task,
        app_listeners: Mutex::new(Vec::new()),
    })
}

async fn forward(
    session: Arc<Handle<Handler>>,
    mut stream: TcpStream,
    remote_host: String,
    remote_port: u16,
    origin: SocketAddr,
    local_token: Option<String>,
    local_port: u16,
) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let Some(buffered) = read_request_head(&mut stream, local_token.as_deref()).await? else {
        return Ok(());
    };
    let shell_document = is_shell_document_request(&buffered);
    let mut channel = open_channel(session, remote_host, remote_port, origin).await?;
    if !buffered.is_empty() {
        channel.write_all(&buffered).await?;
    }
    if shell_document {
        // The mobile shell may be served by a hub release that predates the
        // isolated-app CSP sources. Add them in the tunnel so app iframes work
        // regardless of the hub version.
        let mut head = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let read = channel.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            head.extend_from_slice(&chunk[..read]);
            if head.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
            if head.len() > 64 * 1024 {
                break;
            }
        }
        if !head.is_empty() {
            let rewritten = rewrite_shell_response_csp(&head, local_port)?;
            stream.write_all(&rewritten).await?;
        }
    }
    tokio::io::copy_bidirectional(&mut stream, &mut channel)
        .await
        .context("tunnel copy failed")?;
    Ok(())
}

fn is_shell_document_request(request: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(request) else {
        return false;
    };
    let Some(line) = text.split("\r\n").next() else {
        return false;
    };
    let mut parts = line.split_whitespace();
    let method = parts.next();
    let path = parts.next().unwrap_or_default();
    method == Some("GET") && (path == "/" || path.starts_with("/?"))
}

fn rewrite_shell_response_csp(head: &[u8], port: u16) -> Result<Vec<u8>> {
    let Some(head_end) = head
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
    else {
        return Ok(head.to_vec());
    };
    let head_bytes = &head[..head_end];
    let rest = &head[head_end..];
    let text = std::str::from_utf8(head_bytes).context("HTTP response head is not UTF-8")?;
    let mut lines = text.split("\r\n").peekable();
    let Some(status) = lines.next() else {
        return Ok(head.to_vec());
    };
    if !status.starts_with("HTTP/") {
        return Ok(head.to_vec());
    }
    let mut output = String::with_capacity(text.len() + 4200);
    output.push_str(status);
    output.push_str("\r\n");
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            output.push_str(line);
            output.push_str("\r\n");
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-security-policy") {
            output.push_str("Content-Security-Policy: ");
            output.push_str(&add_app_origins_to_csp(value.trim(), port));
            output.push_str("\r\n");
        } else {
            output.push_str(line);
            output.push_str("\r\n");
        }
    }
    output.push_str("\r\n");
    let mut result = output.into_bytes();
    result.extend_from_slice(rest);
    Ok(result)
}

fn add_app_origins_to_csp(value: &str, port: u16) -> String {
    let mut sources = Vec::new();
    for last in 2..=APP_ORIGIN_LAST {
        sources.push(format!("http://127.0.0.{last}:{port}"));
    }
    let mut directives = value
        .split(';')
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    let mut found = false;
    for directive in &mut directives {
        if directive.split_whitespace().next() == Some("frame-src") {
            found = true;
            for source in &sources {
                if !directive.split_whitespace().any(|item| item == source) {
                    directive.push(' ');
                    directive.push_str(source);
                }
            }
        }
    }
    if !found {
        let mut directive = String::from("frame-src 'self'");
        for source in &sources {
            directive.push(' ');
            directive.push_str(source);
        }
        directives.push(directive);
    }
    directives.join("; ")
}

/// Forward one connection from an isolated app origin.
///
/// The WebView sees `http://127.0.0.x:<port>/` and the app's own relative
/// paths. Before forwarding we replace the Host header with the canonical app
/// host so the hub's app router selects the right proxy mount, and remove the
/// tunnel credential so it never reaches the app. Origin is removed so the hub
/// synthesizes the upstream app origin, matching what the app sees for its own
/// CSRF checks.
async fn forward_app(
    session: Arc<Handle<Handler>>,
    mut stream: TcpStream,
    remote_host: String,
    remote_port: u16,
    origin: SocketAddr,
    local_token: Option<String>,
    app_host: String,
) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let Some(buffered) = read_request_head(&mut stream, local_token.as_deref()).await? else {
        return Ok(());
    };
    let buffered = rewrite_app_request(&buffered, &app_host)?;
    let mut channel = open_channel(session, remote_host, remote_port, origin).await?;
    if !buffered.is_empty() {
        channel.write_all(&buffered).await?;
    }
    tokio::io::copy_bidirectional(&mut stream, &mut channel)
        .await
        .context("app tunnel copy failed")?;
    Ok(())
}

async fn open_channel(
    session: Arc<Handle<Handler>>,
    remote_host: String,
    remote_port: u16,
    origin: SocketAddr,
) -> Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> {
    let channel = session
        .channel_open_direct_tcpip(
            remote_host,
            remote_port as u32,
            origin.ip().to_string(),
            origin.port() as u32,
        )
        .await
        .context("SSH direct-tcpip channel failed")?;
    Ok(channel.into_stream())
}

async fn read_request_head(
    stream: &mut TcpStream,
    local_token: Option<&str>,
) -> Result<Option<Vec<u8>>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const MAX_HEADER_BYTES: usize = 16 * 1024;
    let mut buffered = Vec::new();
    let mut chunk = [0u8; 2048];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffered.extend_from_slice(&chunk[..read]);
        if buffered.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if buffered.len() > MAX_HEADER_BYTES {
            let _ = stream
                .write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
            return Ok(None);
        }
    }
    if let Some(expected) = local_token {
        let supplied = cookie_value(&buffered, "orqella_tunnel");
        if !supplied
            .as_deref()
            .is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes()))
        {
            let _ = stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            return Ok(None);
        }
    }
    Ok(Some(buffered))
}

fn rewrite_app_request(request: &[u8], app_host: &str) -> Result<Vec<u8>> {
    let head_end = request
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .context("HTTP request head is incomplete")?;
    let head = std::str::from_utf8(&request[..head_end])
        .context("HTTP request head is not valid UTF-8")?;
    let body = &request[head_end..];
    let mut lines = head.split("\r\n");
    let request_line = lines.next().context("HTTP request line is missing")?;
    if request_line.is_empty() {
        bail!("HTTP request line is empty");
    }

    let mut rewritten = String::with_capacity(head.len() + app_host.len() + 16);
    rewritten.push_str(request_line);
    rewritten.push_str("\r\n");
    let mut wrote_host = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, _)) = line.split_once(':') else {
            rewritten.push_str(line);
            rewritten.push_str("\r\n");
            continue;
        };
        let name = name.trim();
        if name.eq_ignore_ascii_case("host") {
            if !wrote_host {
                rewritten.push_str("Host: ");
                rewritten.push_str(app_host);
                rewritten.push_str("\r\n");
                wrote_host = true;
            }
            continue;
        }
        if name.eq_ignore_ascii_case("origin") {
            continue;
        }
        if name.eq_ignore_ascii_case("cookie") {
            if let Some(value) = strip_cookie(line, "orqella_tunnel") {
                rewritten.push_str(&value);
                rewritten.push_str("\r\n");
            }
            continue;
        }
        rewritten.push_str(line);
        rewritten.push_str("\r\n");
    }
    if !wrote_host {
        rewritten.push_str("Host: ");
        rewritten.push_str(app_host);
        rewritten.push_str("\r\n");
    }
    rewritten.push_str("\r\n");

    let mut result = rewritten.into_bytes();
    result.extend_from_slice(body);
    Ok(result)
}

/// Remove one cookie from a Cookie header line while preserving the rest.
fn strip_cookie(line: &str, name: &str) -> Option<String> {
    let (header, value) = line.split_once(':')?;
    let kept = value
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .filter(|part| {
            part.split('=')
                .next()
                .is_some_and(|key| !key.trim().eq_ignore_ascii_case(name))
        })
        .collect::<Vec<_>>();
    if kept.is_empty() {
        None
    } else {
        Some(format!("{}: {}", header.trim(), kept.join("; ")))
    }
}

fn cookie_value(request: &[u8], name: &str) -> Option<String> {
    let text = String::from_utf8_lossy(request);
    for line in text.split("\r\n") {
        let Some((header, value)) = line.split_once(':') else {
            continue;
        };
        if !header.trim().eq_ignore_ascii_case("cookie") {
            continue;
        }
        for part in value.split(';') {
            let Some((key, value)) = part.trim().split_once('=') else {
                continue;
            };
            if key.eq_ignore_ascii_case(name) {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }
    difference == 0
}

pub fn load_private_key(path: &Path, passphrase: Option<&str>) -> Result<()> {
    load_secret_key(path, passphrase).context("could not load SSH key")?;
    Ok(())
}

/// Decode an OpenSSH private key without touching the filesystem.
pub fn decode_private_key(secret: &[u8], passphrase: Option<&str>) -> Result<PrivateKey> {
    let secret = std::str::from_utf8(secret).context("SSH key is not valid UTF-8")?;
    decode_secret_key(secret, passphrase).context("could not decode SSH key")
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn local_tunnel_tokens_are_stable_and_key_bound() {
        let first = local_tunnel_token("ssh-ed25519 AAAA one");
        let second = local_tunnel_token("ssh-ed25519 AAAA one");
        let other = local_tunnel_token("ssh-ed25519 BBBB two");
        assert_eq!(first, second);
        assert_ne!(first, other);
        assert_eq!(first.len(), 64);
    }

    #[test]
    fn app_request_rewrites_host_and_strips_tunnel_credentials() {
        let request = b"POST /session/1 HTTP/1.1\r\nHost: 127.0.0.2:29477\r\nOrigin: http://127.0.0.2:29477\r\nCookie: orqella_tunnel=secret; app_session=keep\r\nContent-Length: 4\r\n\r\nbody";
        let rewritten =
            rewrite_app_request(request, "opencode.orqella.localhost").expect("rewrite");
        let text = String::from_utf8(rewritten).expect("utf8");
        assert!(text.contains("Host: opencode.orqella.localhost\r\n"));
        assert!(!text.contains("Origin:"));
        assert!(!text.contains("orqella_tunnel=secret"));
        assert!(text.contains("app_session=keep"));
        assert!(text.ends_with("\r\n\r\nbody"));
    }

    #[test]
    fn app_request_adds_a_host_header_when_missing() {
        let request = b"GET / HTTP/1.1\r\nAccept: */*\r\n\r\n";
        let rewritten = rewrite_app_request(request, "demo.orqella.localhost").expect("rewrite");
        let text = String::from_utf8(rewritten).expect("utf8");
        assert!(text.starts_with("GET / HTTP/1.1\r\n"));
        assert!(text.contains("Host: demo.orqella.localhost\r\n"));
    }

    #[test]
    fn shell_response_csp_gets_mobile_app_origins() {
        let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Security-Policy: default-src 'self'; frame-src 'self' http://*.orqella.localhost:*\r\n\r\n";
        let rewritten = rewrite_shell_response_csp(head, 29477).expect("rewrite");
        let text = String::from_utf8(rewritten).expect("utf8");
        assert!(text.contains("http://127.0.0.2:29477"));
        assert!(text.contains("http://127.0.0.127:29477"));
        assert!(text.contains("frame-src 'self' http://*.orqella.localhost:*"));
        assert!(is_shell_document_request(
            b"GET / HTTP/1.1\r\nHost: 127.0.0.1:29477\r\n\r\n"
        ));
    }

    #[test]
    fn app_hosts_reject_header_injection() {
        assert!(validate_app_host("demo.orqella.localhost").is_ok());
        assert!(validate_app_host("bad\r\nHost: evil").is_err());
        assert!(validate_app_host("bad host").is_err());
        assert!(app_host_for_request("local", "demo", "").is_ok());
        assert_eq!(
            app_host_for_request("local", "demo", "").unwrap(),
            "demo.orqella.localhost"
        );
        assert_eq!(
            app_host_for_request("hub-1", "demo", "").unwrap(),
            "demo.hub-1.orqella.localhost"
        );
    }

    #[test]
    fn cookie_parser_finds_the_tunnel_token() {
        let request = b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nCookie: a=1; orqella_tunnel=deadbeef; z=2\r\n\r\n";
        assert_eq!(
            cookie_value(request, "orqella_tunnel").as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn missing_and_invalid_key_files_fail_with_context() {
        let root =
            std::env::temp_dir().join(format!("orqella-tunnel-key-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let missing = root.join("missing");
        let error = load_private_key(&missing, None).unwrap_err();
        assert!(format!("{error:#}").contains("SSH key"));

        std::fs::write(&missing, b"not a key").unwrap();
        assert!(load_private_key(&missing, None).is_err());
        assert!(decode_private_key(b"", None).is_err());
        assert!(decode_private_key(b"\xff", None).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn local_listener_prefers_a_stable_port_and_falls_back() {
        let occupied = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();
        let fallback = bind_local_listener(occupied_port).await.unwrap();
        assert_ne!(fallback.local_addr().unwrap().port(), occupied_port);

        let probe = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let free_port = probe.local_addr().unwrap().port();
        drop(probe);
        let stable = bind_local_listener(free_port).await.unwrap();
        assert_eq!(stable.local_addr().unwrap().port(), free_port);

        let ephemeral = bind_local_listener(0).await.unwrap();
        assert_ne!(ephemeral.local_addr().unwrap().port(), 0);
    }
}
