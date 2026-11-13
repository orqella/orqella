//! Mobile-facing UniFFI surface.
//!
//! The native shells use this object to keep one foreground tunnel alive. Key
//! material stays in the platform secure store; UniFFI accepts the key bytes
//! rather than a filesystem path so iOS Keychain and Android Keystore can pass
//! it directly.

use crate::{Tunnel, TunnelConfig, decode_private_key, start_with_key};
use rand::rng;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, HashAlg, PrivateKey};
use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;

/// A newly generated device key. Only `public_key` is safe to display or copy;
/// `private_key` must go straight into platform secure storage.
#[derive(Debug, Clone, uniffi::Record)]
pub struct GeneratedSshKey {
    /// OpenSSH-format private key bytes.
    pub private_key: Vec<u8>,
    /// One-line OpenSSH public key, used as a certificate subject.
    pub public_key: String,
    /// SHA256 fingerprint of the public key.
    pub fingerprint: String,
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
        };

        let tunnel = self
            .runtime
            .block_on(start_with_key(inner, key))
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
        if self.is_running() {
            return Err(crate::MobileLinkError::Connection {
                detail: "a tunnel is already running".into(),
            });
        }
        let (cancel_tx, mut cancel_rx) = tokio::sync::watch::channel(false);
        *self
            .link_cancel
            .lock()
            .map_err(|_| crate::MobileLinkError::Connection {
                detail: "link state is poisoned".into(),
            })? = Some(cancel_tx);
        let result = self.runtime.block_on(async {
            tokio::select! {
                result = async {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(45),
                        crate::start_link(config),
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
        let url = format!("http://{}", link.ssh.local_addr);
        *self
            .link
            .lock()
            .map_err(|_| crate::MobileLinkError::Connection {
                detail: "link state is poisoned".into(),
            })? = Some(link);
        Ok(url)
    }

    /// Stop and release the tunnel. Safe to call when no tunnel is running.
    pub fn stop(&self) -> Result<(), MobileTunnelError> {
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
        self.link.lock().ok().and_then(|slot| {
            slot.as_ref()
                .map(|link| format!("http://{}", link.ssh.local_addr))
        })
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
