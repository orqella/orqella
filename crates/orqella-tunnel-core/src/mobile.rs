//! Mobile-facing UniFFI surface.
//!
//! The native shells use this object to keep one foreground tunnel alive. Key
//! material stays in the platform secure store; UniFFI accepts the key bytes
//! rather than a filesystem path so iOS Keychain and Android Keystore can pass
//! it directly.

use crate::{Tunnel, TunnelConfig, TunnelGateway, decode_private_key, start_with_key_cert_gateway};
use rand::rng;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;

/// A newly generated device key. Only `public_key` is safe to display or copy;
/// `private_key` must go straight into platform secure storage.
#[derive(Clone, uniffi::Record)]
pub struct GeneratedSshKey {
    /// OpenSSH-format private key bytes.
    pub private_key: Vec<u8>,
    /// One-line OpenSSH public key, used as a certificate subject.
    pub public_key: String,
    /// SHA256 fingerprint of the public key.
    pub fingerprint: String,
}

impl std::fmt::Debug for GeneratedSshKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GeneratedSshKey")
            .field(
                "private_key",
                &format!("<{} bytes>", self.private_key.len()),
            )
            .field("public_key", &self.public_key)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct SshKeyInfo {
    pub public_key: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileTunnelConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    /// OpenSSH private key bytes, typically exported from Keychain/Keystore.
    pub private_key: Vec<u8>,
    pub key_passphrase: Option<String>,
    /// Required pin from the hub pairing flow.
    pub expected_host_fingerprint: String,
    pub remote_host: String,
    pub remote_port: u16,
    /// Preferred loopback port for the WebView listener. Keep this stable
    /// across launches so origin-scoped browser storage survives; 0 asks the
    /// operating system for an ephemeral port.
    pub local_port: u16,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileTunnelError {
    #[error("a tunnel is already running")]
    AlreadyRunning,
    #[error("the SSH key could not be decoded: {detail}")]
    InvalidKey { detail: String },
    #[error("the SSH key could not be generated: {detail}")]
    KeyGenerationFailed { detail: String },
    #[error("the tunnel could not start: {detail}")]
    StartFailed { detail: String },
    #[error("the tunnel could not stop: {detail}")]
    StopFailed { detail: String },
    #[error("the tunnel runtime could not start: {detail}")]
    RuntimeFailed { detail: String },
}

/// Generate a fresh Ed25519 device key in OpenSSH format.
///
/// The private key is returned only so the caller can immediately encrypt it
/// with Android Keystore or store it in the iOS Keychain. The public key is the
/// one-line public key used as the subject of a target-bound certificate.
#[uniffi::export]
pub fn generate_ssh_key(comment: String) -> Result<GeneratedSshKey, MobileTunnelError> {
    let comment = comment.trim();
    if comment.len() > 128 || comment.chars().any(|c| c.is_control()) {
        return Err(MobileTunnelError::KeyGenerationFailed {
            detail: "the key comment is invalid".into(),
        });
    }
    let comment = if comment.is_empty() {
        "orqella-mobile"
    } else {
        comment
    };

    let mut key = PrivateKey::random(&mut rng(), Algorithm::Ed25519).map_err(|error| {
        MobileTunnelError::KeyGenerationFailed {
            detail: error.to_string(),
        }
    })?;
    key.set_comment(comment.to_string());
    let public_key =
        key.public_key()
            .to_openssh()
            .map_err(|error| MobileTunnelError::KeyGenerationFailed {
                detail: error.to_string(),
            })?;
    let fingerprint = key.public_key().fingerprint(HashAlg::Sha256).to_string();
    let private_key = key
        .to_openssh(LineEnding::LF)
        .map_err(|error| MobileTunnelError::KeyGenerationFailed {
            detail: error.to_string(),
        })?
        .as_bytes()
        .to_vec();

    Ok(GeneratedSshKey {
        private_key,
        public_key,
        fingerprint,
    })
}

/// Inspect an existing private key and return the safe public identity.
#[uniffi::export]
pub fn ssh_key_info(
    private_key: Vec<u8>,
    passphrase: Option<String>,
) -> Result<SshKeyInfo, MobileTunnelError> {
    let key = decode_private_key(&private_key, passphrase.as_deref()).map_err(|error| {
        MobileTunnelError::InvalidKey {
            detail: format!("{error:#}"),
        }
    })?;
    let public = key.public_key();
    let public_key =
        public
            .to_openssh()
            .map_err(|error| MobileTunnelError::KeyGenerationFailed {
                detail: error.to_string(),
            })?;
    Ok(SshKeyInfo {
        public_key,
        fingerprint: public.fingerprint(HashAlg::Sha256).to_string(),
    })
}

/// One foreground tunnel owned by the native shell.
#[derive(uniffi::Object)]
pub struct MobileTunnel {
    runtime: Runtime,
    tunnel: Mutex<Option<Tunnel>>,
    link: Mutex<Option<crate::MobileLinkTunnel>>,
    link_cancel: Mutex<Option<tokio::sync::watch::Sender<bool>>>,
    link_cache: Mutex<crate::link::LinkReconnectCache>,
    gateway: Mutex<Option<Arc<TunnelGateway>>>,
    link_generation: AtomicU64,
}

#[uniffi::export]
impl MobileTunnel {
    #[uniffi::constructor]
    pub fn new() -> Result<Arc<Self>, MobileTunnelError> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| MobileTunnelError::RuntimeFailed {
                detail: error.to_string(),
            })?;
        Ok(Arc::new(Self {
            runtime,
            tunnel: Mutex::new(None),
            link: Mutex::new(None),
            link_cancel: Mutex::new(None),
            link_cache: Mutex::new(crate::link::LinkReconnectCache::default()),
            gateway: Mutex::new(None),
            link_generation: AtomicU64::new(0),
        }))
    }

    /// Start the tunnel and return the loopback dashboard URL.
    pub fn start(&self, config: MobileTunnelConfig) -> Result<String, MobileTunnelError> {
        let mut slot = self
            .tunnel
            .lock()
            .map_err(|_| MobileTunnelError::StartFailed {
                detail: "tunnel state is poisoned".into(),
            })?;
        if slot.is_some() {
            return Err(MobileTunnelError::AlreadyRunning);
        }

        let key = decode_private_key(&config.private_key, config.key_passphrase.as_deref())
            .map_err(|error| MobileTunnelError::InvalidKey {
                detail: format!("{error:#}"),
            })?;
        let public_key =
            key.public_key()
                .to_openssh()
                .map_err(|error| MobileTunnelError::InvalidKey {
                    detail: error.to_string(),
                })?;
        let local_token = crate::local_tunnel_token(&public_key);
        let gateway = self
            .ensure_gateway(config.local_port, Some(local_token.clone()))
            .map_err(|detail| MobileTunnelError::StartFailed { detail })?;
        let inner = TunnelConfig {
            host: config.host,
            port: config.port,
            user: config.user,
            private_key: Default::default(),
            key_passphrase: config.key_passphrase,
            expected_host_fingerprint: Some(config.expected_host_fingerprint),
            remote_host: config.remote_host,
            remote_port: config.remote_port,
            local_port: config.local_port,
            local_token: Some(local_token),
        };

        let tunnel = self
            .runtime
            .block_on(start_with_key_cert_gateway(inner, key, None, Some(gateway)))
            .map_err(|error| MobileTunnelError::StartFailed {
                detail: format!("{error:#}"),
            })?;
        let url = format!("http://{}", tunnel.local_addr);
        *slot = Some(tunnel);
        Ok(url)
    }

    /// Start a direct iroh Link tunnel and return the loopback dashboard URL.
    pub fn start_link(
        &self,
        config: crate::MobileLinkTunnelConfig,
    ) -> Result<String, crate::MobileLinkError> {
        let generation = self.link_generation.fetch_add(1, Ordering::SeqCst) + 1;
        // Replace a live transport in place. The durable gateway stays bound,
        // so the WebView keeps its page and origin during the swap.
        if let Ok(mut slot) = self.link.lock()
            && let Some(existing) = slot.take()
        {
            self.runtime.block_on(existing.stop());
        }
        if let Ok(mut slot) = self.tunnel.lock()
            && let Some(existing) = slot.take()
        {
            self.runtime.block_on(existing.stop());
        }

        let key = decode_private_key(&config.private_key, config.key_passphrase.as_deref())
            .map_err(|error| crate::MobileLinkError::Configuration {
                detail: format!("{error:#}"),
            })?;
        let public_key = key.public_key().to_openssh().map_err(|error| {
            crate::MobileLinkError::Configuration {
                detail: error.to_string(),
            }
        })?;
        let gateway = self
            .ensure_gateway(
                config.local_port,
                Some(crate::local_tunnel_token(&public_key)),
            )
            .map_err(|detail| crate::MobileLinkError::Connection { detail })?;
        if self.link_generation.load(Ordering::SeqCst) != generation {
            self.release_gateway_if_current(&gateway);
            return Err(crate::MobileLinkError::Connection {
                detail: "connection cancelled".into(),
            });
        }

        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        *self
            .link_cancel
            .lock()
            .map_err(|_| crate::MobileLinkError::Connection {
                detail: "link state is poisoned".into(),
            })? = Some(cancel_tx);
        let mut cache = self
            .link_cache
            .lock()
            .map_err(|_| crate::MobileLinkError::Connection {
                detail: "link reconnect cache is poisoned".into(),
            })?;
        let start_gateway = Arc::clone(&gateway);
        let result = self.runtime.block_on(async {
            tokio::select! {
                result = async {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(45),
                        crate::link::start_link_with_cache_gateway(
                            config,
                            &mut cache,
                            Some(start_gateway),
                        ),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => Err(crate::MobileLinkError::Connection {
                            detail: "connection timed out".into(),
                        }),
                    }
                } => result,
                _ = async {
                    loop {
                        if *cancel_rx.borrow() {
                            break;
                        }
                        if cancel_rx.changed().await.is_err() {
                            break;
                        }
                    }
                } => Err(crate::MobileLinkError::Connection {
                    detail: "connection cancelled".into(),
                }),
            }
        });
        if let Ok(mut slot) = self.link_cancel.lock() {
            slot.take();
        }
        let link = result.map_err(|error| crate::MobileLinkError::Connection {
            detail: format!("{error}"),
        })?;
        if self.link_generation.load(Ordering::SeqCst) != generation {
            self.runtime.block_on(link.stop());
            self.release_gateway_if_current(&gateway);
            return Err(crate::MobileLinkError::Connection {
                detail: "connection cancelled".into(),
            });
        }
        let url = format!("http://{}", link.ssh.local_addr);
        *self
            .link
            .lock()
            .map_err(|_| crate::MobileLinkError::Connection {
                detail: "link state is poisoned".into(),
            })? = Some(link);
        Ok(url)
    }

    /// Stop the SSH transport but keep the durable loopback listener.
    ///
    /// The WebView origin and app listeners stay alive so the next `start` or
    /// `start_link` can connect again without reloading the page. Call
    /// [`Self::shutdown`] on an explicit disconnect or sign-out.
    pub fn stop(&self) -> Result<(), MobileTunnelError> {
        self.link_generation.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut slot) = self.link_cancel.lock()
            && let Some(cancel) = slot.take()
        {
            let _ = cancel.send(true);
        }
        let tunnel = self
            .tunnel
            .lock()
            .map_err(|_| MobileTunnelError::StopFailed {
                detail: "tunnel state is poisoned".into(),
            })?
            .take();
        if let Some(tunnel) = tunnel {
            self.runtime.block_on(tunnel.stop());
        }
        let link = self.link.lock().ok().and_then(|mut slot| slot.take());
        if let Some(link) = link {
            self.runtime.block_on(link.stop());
        }
        Ok(())
    }

    /// Stop the transport and release the stable loopback listener.
    ///
    /// Use this for an explicit disconnect or sign-out. A transient reconnect
    /// should call [`Self::stop`] instead so the WebView keeps its origin.
    pub fn shutdown(&self) -> Result<(), MobileTunnelError> {
        self.stop()?;
        self.close_gateway();
        self.clear_reconnect_cache();
        Ok(())
    }

    pub fn is_running(&self) -> bool {
        self.tunnel
            .lock()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
            || self.link.lock().map(|slot| slot.is_some()).unwrap_or(false)
    }

    pub fn local_url(&self) -> Option<String> {
        if let Some(tunnel) = self.tunnel.lock().ok().and_then(|slot| {
            slot.as_ref()
                .map(|tunnel| format!("http://{}", tunnel.local_addr))
        }) {
            return Some(tunnel);
        }
        if let Some(link) = self.link.lock().ok().and_then(|slot| {
            slot.as_ref()
                .map(|link| format!("http://{}", link.ssh.local_addr))
        }) {
            return Some(link);
        }
        self.gateway
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|gateway| gateway.local_url()))
    }

    /// Verify the live SSH transport with a bounded keepalive/ping exchange.
    ///
    /// Mobile shells use this on resume instead of an HTTP health request
    /// through the hub, so a busy dashboard does not look like a dead tunnel.
    pub fn ping(&self, timeout_ms: u32) -> bool {
        let timeout = std::time::Duration::from_millis(u64::from(timeout_ms.clamp(50, 5_000)));
        if let Ok(link) = self.link.lock()
            && let Some(link) = link.as_ref()
        {
            return self.runtime.block_on(link.ssh.ping(timeout));
        }
        if let Ok(tunnel) = self.tunnel.lock()
            && let Some(tunnel) = tunnel.as_ref()
        {
            return self.runtime.block_on(tunnel.ping(timeout));
        }
        false
    }

    /// Drop cached reconnect material. Call this on explicit sign-out or
    /// disconnect, not on a transient reconnect.
    pub fn clear_reconnect_cache(&self) {
        if let Ok(mut cache) = self.link_cache.lock() {
            cache.clear();
        }
    }

    /// Allocate an isolated loopback origin for one full app surface.
    ///
    /// `host` should be the canonical app host from `app.open` (for example
    /// `opencode.orqella.localhost`). When it is empty the host is reconstructed
    /// from `mount` and `node` for the default `orqella.localhost` namespace.
    /// The returned URL keeps the app on its own `127.0.0.x` origin while the
    /// tunnel rewrites the Host header to the canonical app host.
    pub fn app_origin(
        &self,
        node: String,
        mount: String,
        host: String,
    ) -> Result<String, crate::MobileLinkError> {
        let app_host = crate::app_host_for_request(&node, &mount, &host).map_err(|error| {
            crate::MobileLinkError::Configuration {
                detail: error.to_string(),
            }
        })?;
        let gateway = self
            .gateway
            .lock()
            .map_err(|_| crate::MobileLinkError::Connection {
                detail: "gateway state is poisoned".into(),
            })?
            .as_ref()
            .cloned()
            .ok_or_else(|| crate::MobileLinkError::Connection {
                detail: "start a tunnel before opening an app".into(),
            })?;
        let addr = self
            .runtime
            .block_on(gateway.open_app_origin(&app_host))
            .map_err(|error| crate::MobileLinkError::Connection {
                detail: format!("{error:#}"),
            })?;
        Ok(format!("http://{addr}/"))
    }
}

impl MobileTunnel {
    /// Close `gateway` only if it is still the one installed on this object.
    /// A newer start may already have replaced it, and must not be torn down
    /// by an older cancelled attempt.
    fn release_gateway_if_current(&self, gateway: &Arc<TunnelGateway>) {
        let current = self.gateway.lock().ok().and_then(|mut slot| {
            let matches = slot
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, gateway));
            matches.then(|| slot.take()).flatten()
        });
        if let Some(current) = current {
            self.runtime.block_on(current.close());
        }
    }

    fn ensure_gateway(
        &self,
        local_port: u16,
        local_token: Option<String>,
    ) -> Result<Arc<TunnelGateway>, String> {
        let mut slot = self
            .gateway
            .lock()
            .map_err(|_| "gateway state is poisoned".to_string())?;
        if let Some(existing) = slot.as_ref()
            && existing.matches(local_port, local_token.as_deref())
        {
            return Ok(Arc::clone(existing));
        }
        if let Some(existing) = slot.take() {
            self.runtime.block_on(existing.close());
        }
        let gateway = self
            .runtime
            .block_on(TunnelGateway::bind(local_port, local_token))
            .map_err(|error| format!("{error:#}"))?;
        *slot = Some(Arc::clone(&gateway));
        Ok(gateway)
    }

    fn close_gateway(&self) {
        let gateway = self.gateway.lock().ok().and_then(|mut slot| slot.take());
        if let Some(gateway) = gateway {
            self.runtime.block_on(gateway.close());
        }
    }
}

impl Drop for MobileTunnel {
    fn drop(&mut self) {
        let tunnel = self.tunnel.lock().ok().and_then(|mut slot| slot.take());
        if let Some(tunnel) = tunnel {
            self.runtime.block_on(tunnel.stop());
        }
        let link = self.link.lock().ok().and_then(|mut slot| slot.take());
        if let Some(link) = link {
            self.runtime.block_on(link.stop());
        }
        self.close_gateway();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_key_is_rejected_before_network_access() {
        let tunnel = MobileTunnel::new().expect("runtime");
        let result = tunnel.start(MobileTunnelConfig {
            host: "127.0.0.1".into(),
            port: 22,
            user: "nobody".into(),
            private_key: b"not an SSH key".to_vec(),
            key_passphrase: None,
            expected_host_fingerprint: "SHA256:invalid".into(),
            remote_host: "127.0.0.1".into(),
            remote_port: 9477,
            local_port: 0,
        });
        assert!(matches!(result, Err(MobileTunnelError::InvalidKey { .. })));
        assert!(!tunnel.is_running());
        assert!(tunnel.local_url().is_none());
    }

    #[test]
    fn stopping_an_idle_tunnel_is_safe() {
        let tunnel = MobileTunnel::new().expect("runtime");
        assert!(tunnel.stop().is_ok());
        assert!(tunnel.stop().is_ok());
        assert!(!tunnel.is_running());
    }

    #[test]
    fn pinging_an_idle_tunnel_is_false_and_cache_clear_is_safe() {
        let tunnel = MobileTunnel::new().expect("runtime");
        assert!(!tunnel.ping(250));
        tunnel.clear_reconnect_cache();
        tunnel.clear_reconnect_cache();
    }

    #[test]
    fn gateway_survives_stop_and_shutdown_releases_it() {
        let tunnel = MobileTunnel::new().expect("runtime");
        let gateway = tunnel
            .ensure_gateway(0, Some("token".into()))
            .expect("gateway");
        let url = tunnel.local_url().expect("gateway url");
        assert_eq!(url, gateway.local_url());

        assert!(tunnel.stop().is_ok());
        assert_eq!(
            tunnel.local_url().as_deref(),
            Some(url.as_str()),
            "stop keeps the stable origin for a reconnect"
        );
        let origin = tunnel
            .app_origin("local".into(), "demo".into(), String::new())
            .expect("app origin during reconnect");
        assert!(origin.starts_with("http://127.0.0."));

        assert!(tunnel.shutdown().is_ok());
        assert!(!tunnel.is_running());
        assert!(tunnel.local_url().is_none());
        assert!(tunnel.shutdown().is_ok());
    }

    #[test]
    fn non_utf8_key_is_rejected() {
        let error = crate::decode_private_key(&[0xff, 0xfe], None).unwrap_err();
        assert!(error.to_string().contains("UTF-8"));
    }

    #[test]
    fn generated_key_is_openssh_and_unique() {
        let first = generate_ssh_key("orqella@test".into()).expect("generate key");
        let second = generate_ssh_key("orqella@test".into()).expect("generate key");

        assert!(
            first
                .private_key
                .starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----")
        );
        assert!(first.public_key.starts_with("ssh-ed25519 "));
        assert!(first.public_key.ends_with(" orqella@test"));
        assert!(first.fingerprint.starts_with("SHA256:"));
        assert_ne!(first.private_key, second.private_key);
        assert_ne!(first.public_key, second.public_key);

        let decoded = crate::decode_private_key(&first.private_key, None).expect("decode key");
        assert_eq!(
            decoded.public_key().to_openssh().expect("public key"),
            first.public_key
        );
    }

    #[test]
    fn generated_key_rejects_invalid_comment() {
        assert!(matches!(
            generate_ssh_key("bad\ncomment".into()),
            Err(MobileTunnelError::KeyGenerationFailed { .. })
        ));
    }
}
