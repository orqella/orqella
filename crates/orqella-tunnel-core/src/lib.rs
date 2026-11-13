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
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::JoinSet;

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

pub struct Tunnel {
    pub local_addr: SocketAddr,
    stop_tx: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl Tunnel {
    pub async fn stop(mut self) {
        if let Some(stop) = self.stop_tx.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        if let Some(stop) = self.stop_tx.take() {
            let _ = stop.send(());
        }
        self.task.abort();
    }
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
    let remote_host = config.remote_host.clone();
    let remote_port = config.remote_port;
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
                    let session = session.clone();
                    let remote_host = remote_host.clone();
                    connections.spawn(async move {
                        if let Err(error) = forward(session, stream, remote_host, remote_port, origin).await {
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
        stop_tx: Some(stop_tx),
        task,
    })
}

async fn forward(
    session: Arc<Handle<Handler>>,
    mut stream: TcpStream,
    remote_host: String,
    remote_port: u16,
    origin: SocketAddr,
) -> Result<()> {
    let channel = session
        .channel_open_direct_tcpip(
            remote_host,
            remote_port as u32,
            origin.ip().to_string(),
            origin.port() as u32,
        )
        .await
        .context("SSH direct-tcpip channel failed")?;
    let mut channel = channel.into_stream();
    tokio::io::copy_bidirectional(&mut stream, &mut channel)
        .await
        .context("tunnel copy failed")?;
    Ok(())
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
