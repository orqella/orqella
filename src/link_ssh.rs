//! Loopback-only SSH server used by the Link bridge.
//!
//! This is deliberately not the system sshd. It only accepts user certificates
//! signed by this target's locally held SSH CA, only listens on 127.0.0.1, and
//! only permits a direct-tcpip channel to the local Orqella dashboard port. No
//! shell, agent forwarding, or arbitrary port forwarding is exposed.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use russh::keys::{
    Algorithm, HashAlg, PrivateKey, PublicKey,
    ssh_key::{
        LineEnding,
        certificate::{Builder, CertType},
    },
};
use russh::server::{self, Auth, ChannelOpenHandle, Msg, run_stream};
use russh::{Channel, ChannelStream};
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

fn _assert_send_sync<T: Send + Sync>() {}
const _: fn() = || _assert_send_sync::<LinkSshServer>();

pub struct LinkSshServer {
    addr: SocketAddr,
    fingerprint: String,
    /// The target's own SSH user CA. Certificates are signed locally and never
    /// by the Link control plane or relay broker.
    ca_key: Arc<PrivateKey>,
    ca_public_key: String,
    target_device_id: Arc<RwLock<String>>,
    task: JoinHandle<()>,
}

impl LinkSshServer {
    pub async fn start(
        data_dir: &Path,
        dashboard_socket: Option<PathBuf>,
        dashboard_port: u16,
    ) -> Result<Self> {
        let link_dir = data_dir.join("link");
        std::fs::create_dir_all(&link_dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&link_dir, std::fs::Permissions::from_mode(0o700));
        }
        let host_key_path = link_dir.join("ssh_host_ed25519_key");
        let host_key = load_or_create_key(&host_key_path, "host key")?;
        let fingerprint = host_key
            .public_key()
            .fingerprint(HashAlg::Sha256)
            .to_string();
        let ca_key_path = link_dir.join("ssh_user_ca");
        let ca_key = Arc::new(load_or_create_key(&ca_key_path, "user CA")?);
        let ca_public_key = ca_key.public_key().to_openssh()?;
        let config = Arc::new(server::Config {
            server_id: russh::SshId::Standard(
                format!("SSH-2.0-OrqellaLink_{}", env!("CARGO_PKG_VERSION")).into(),
            ),
            keys: vec![host_key],
            inactivity_timeout: Some(Duration::from_secs(120)),
            auth_rejection_time: Duration::from_millis(250),
            auth_rejection_time_initial: Some(Duration::ZERO),
            max_auth_attempts: 6,
            ..Default::default()
        });
        let target_device_id = Arc::new(RwLock::new(String::new()));
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .context("could not bind Link SSH server")?;
        let addr = listener.local_addr()?;
        let handler_ca = ca_public_key.clone();
        let handler_target = target_device_id.clone();
        let task = tokio::spawn(async move {
            loop {
                let Ok((socket, _peer)) = listener.accept().await else {
                    break;
                };
                let config = config.clone();
                let handler = LinkSshHandler {
                    dashboard_socket: dashboard_socket.clone(),
                    dashboard_port,
                    ca_public_key: handler_ca.clone(),
                    target_device_id: handler_target.clone(),
                };
                tokio::spawn(async move {
                    if let Ok(session) = run_stream(config, socket, handler).await {
                        let _ = session.await;
                    }
                });
            }
        });
        Ok(Self {
            addr,
            fingerprint,
            ca_key,
            ca_public_key,
            target_device_id,
            task,
        })
    }

    /// Sign a short-lived user certificate for a source device's registered
    /// key. The principal is bound to this target and the key ID to the source.
    pub fn sign_user_certificate(
        &self,
        public_key: &PublicKey,
        principal: &str,
        key_id: &str,
        ttl: Duration,
    ) -> Result<(String, u64)> {
        let principal = principal.trim();
        let key_id = key_id.trim();
        if principal.is_empty()
            || key_id.is_empty()
            || principal.len() > 256
            || key_id.len() > 256
            || principal.chars().any(char::is_control)
            || key_id.chars().any(char::is_control)
        {
            bail!("invalid certificate principal or key id");
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let valid_after = now.saturating_sub(60);
        let valid_before = now.saturating_add(ttl.as_secs().clamp(60, 3600));
        let mut builder = Builder::new_with_random_nonce(
            &mut rand10::rng(),
            public_key.key_data().clone(),
            valid_after,
            valid_before,
        )
        .context("could not build SSH certificate")?;
        builder
            .cert_type(CertType::User)
            .context("could not set SSH certificate type")?;
        builder
            .key_id(key_id)
            .context("could not set SSH certificate key id")?;
        builder
            .valid_principal(principal)
            .context("could not set SSH certificate principal")?;
        builder
            .comment("orqella-link")
            .context("could not set SSH certificate comment")?;
        let certificate = builder
            .sign(self.ca_key.as_ref())
            .and_then(|certificate| certificate.to_openssh())
            .context("could not sign SSH certificate")?;
        Ok((certificate, valid_before))
    }

    pub fn ca_public_key(&self) -> &str {
        &self.ca_public_key
    }

    pub fn set_target_device_id(&self, device_id: impl Into<String>) {
        *self.target_device_id.write().expect("link ssh target lock") = device_id.into();
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn address(&self) -> SocketAddr {
        self.addr
    }

    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    pub fn stop(&self) {
        self.task.abort();
    }
}

impl Drop for LinkSshServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn load_or_create_key(path: &Path, kind: &str) -> Result<PrivateKey> {
    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        let text = std::fs::read_to_string(path)?;
        return PrivateKey::from_openssh(&text)
            .with_context(|| format!("could not read Link SSH {} {}", kind, path.display()));
    }
    let key = PrivateKey::random(&mut rand10::rng(), Algorithm::Ed25519)
        .with_context(|| format!("could not generate Link SSH {kind}"))?;
    let encoded = key.to_openssh(LineEnding::LF)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(path) {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(encoded.as_bytes())?;
            file.sync_all()?;
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let text = std::fs::read_to_string(path)?;
            PrivateKey::from_openssh(&text)
                .with_context(|| format!("could not read Link SSH {} {}", kind, path.display()))
        }
        Err(error) => Err(error)
            .with_context(|| format!("could not write Link SSH {} {}", kind, path.display())),
    }
}

#[derive(Clone)]
struct LinkSshHandler {
    dashboard_socket: Option<PathBuf>,
    dashboard_port: u16,
    ca_public_key: String,
    target_device_id: Arc<RwLock<String>>,
}

impl server::Handler for LinkSshHandler {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        _user: &str,
        _key: server::VerifiedPublicKey<'_>,
    ) -> Result<Auth, Self::Error> {
        // Link is certificate-only. Raw device keys are retained in the account
        // as certificate subjects, not as direct authentication credentials.
        Ok(Auth::reject())
    }

    async fn auth_openssh_certificate(
        &mut self,
        _user: &str,
        certificate: &russh::keys::Certificate,
    ) -> Result<Auth, Self::Error> {
        let target = self
            .target_device_id
            .read()
            .expect("link ssh target lock")
            .clone();
        if target.is_empty() {
            return Ok(Auth::reject());
        }
        let Ok(ca_key) = PublicKey::from_openssh(&self.ca_public_key) else {
            return Ok(Auth::reject());
        };
        let ca_fingerprint = ca_key.fingerprint(HashAlg::Sha256);
        if certificate.validate([&ca_fingerprint]).is_err()
            || certificate.cert_type() != CertType::User
            || certificate.key_id().is_empty()
        {
            return Ok(Auth::reject());
        }
        let expected = format!("orqella-target-{target}");
        if !certificate
            .valid_principals()
            .iter()
            .any(|principal| principal == &expected)
        {
            return Ok(Auth::reject());
        }
        Ok(Auth::Accept)
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut server::Session,
    ) -> Result<(), Self::Error> {
        if !matches!(host_to_connect, "127.0.0.1" | "localhost" | "::1")
            || port_to_connect != u32::from(self.dashboard_port)
        {
            tracing::warn!(
                %host_to_connect,
                port = port_to_connect,
                "rejected non-dashboard Link SSH forward"
            );
            return Ok(());
        }
        reply.accept().await;
        let stream = channel.into_stream();
        let socket = self.dashboard_socket.clone();
        let port = self.dashboard_port;
        tokio::spawn(async move {
            let result = if let Some(socket) = socket {
                forward_dashboard_socket(stream, &socket).await
            } else {
                forward_dashboard_tcp(stream, port).await
            };
            if let Err(error) = result {
                tracing::debug!(%error, "Link SSH dashboard forward ended");
            }
        });
        Ok(())
    }
}

async fn forward_dashboard_tcp(mut stream: ChannelStream<Msg>, port: u16) -> Result<()> {
    let target = format!("127.0.0.1:{port}");
    let mut tcp = TcpStream::connect(&target)
        .await
        .with_context(|| format!("could not connect to {target}"))?;
    copy_bidirectional(&mut stream, &mut tcp).await?;
    Ok(())
}

#[cfg(unix)]
async fn forward_dashboard_socket(mut stream: ChannelStream<Msg>, socket: &Path) -> Result<()> {
    let mut unix = tokio::net::UnixStream::connect(socket)
        .await
        .with_context(|| format!("could not connect to {}", socket.display()))?;
    copy_bidirectional(&mut stream, &mut unix).await?;
    Ok(())
}

#[cfg(not(unix))]
async fn forward_dashboard_socket(mut stream: ChannelStream<Msg>, _socket: &Path) -> Result<()> {
    forward_dashboard_tcp(stream, 0).await
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[tokio::test]
    async fn host_key_is_created_privately_and_reused() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("host_key");
        let first = load_or_create_key(&path, "host key").unwrap();
        let second = load_or_create_key(&path, "host key").unwrap();
        assert_eq!(
            first.public_key().fingerprint(HashAlg::Sha256),
            second.public_key().fingerprint(HashAlg::Sha256)
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("OPENSSH PRIVATE KEY"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn invalid_host_key_files_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("host_key");
        std::fs::write(&path, "not a key").unwrap();
        let error = load_or_create_key(&path, "host key").unwrap_err();
        assert!(format!("{error:#}").contains("host key"));
    }

    #[tokio::test]
    async fn server_binds_loopback_only_and_stops_cleanly() {
        let root = tempfile::tempdir().unwrap();
        let server = LinkSshServer::start(root.path(), None, 9477).await.unwrap();
        assert!(server.address().ip().is_loopback());
        assert_ne!(server.port(), 0);
        assert!(server.fingerprint().starts_with("SHA256:"));
        assert!(server.ca_public_key().starts_with("ssh-ed25519 "));
        server.set_target_device_id("device-1");
        server.stop();
        assert!(root.path().join("link/ssh_host_ed25519_key").is_file());
        assert!(root.path().join("link/ssh_user_ca").is_file());
    }

    #[tokio::test]
    async fn target_signs_source_keys_with_its_own_ca() {
        let root = tempfile::tempdir().unwrap();
        let server = LinkSshServer::start(root.path(), None, 9477).await.unwrap();
        let source = PrivateKey::random(&mut rand10::rng(), Algorithm::Ed25519).unwrap();
        let (certificate, expires_at) = server
            .sign_user_certificate(
                source.public_key(),
                "orqella-target-device-1",
                "source-device",
                Duration::from_secs(600),
            )
            .unwrap();
        assert!(certificate.starts_with("ssh-ed25519-cert-v01@openssh.com "));
        assert!(expires_at > 0);
        let parsed = russh::keys::Certificate::from_openssh(&certificate).unwrap();
        let ca = PublicKey::from_openssh(server.ca_public_key()).unwrap();
        assert!(parsed.validate([&ca.fingerprint(HashAlg::Sha256)]).is_ok());
        assert!(
            parsed
                .valid_principals()
                .iter()
                .any(|value| value == "orqella-target-device-1")
        );
        server.stop();
    }
}
