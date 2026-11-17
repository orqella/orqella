//! Account-assisted mobile link transport.
//!
//! Mobile clients use the same direct iroh/SSH bridge as the daemon, but keep
//! their SSH key and iroh identity in platform secure storage. The account
//! service supplies device metadata and short-lived tickets; the SSH tunnel
//! itself stays inside the direct iroh connection.

use crate::{Tunnel, TunnelConfig, decode_private_key, start_with_key_and_cert};
use anyhow::Result;
use orqella_link_core::iroh::SecretKey;
use orqella_link_core::{
    AccountClient, LinkNode, RegisterDevice, TicketSource, device_endpoint_addr,
    install_brokered_relays, start_local_forward_with_endpoint,
};
use russh::keys::Certificate;
use std::time::Duration;
use tokio::time::timeout;

const MOBILE_START_TIMEOUT: Duration = Duration::from_secs(35);

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileLinkDevice {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub ssh_user: String,
    pub ssh_port: u16,
    pub host_fingerprint: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileLinkRegistration {
    pub device_id: String,
    pub endpoint_id: String,
    pub direct_addresses: Vec<String>,
}

#[derive(Clone, uniffi::Record)]
pub struct MobileLinkTunnelConfig {
    pub token: String,
    pub source_device_id: String,
    pub target_device_id: String,
    /// OpenSSH private key bytes, typically exported from Keychain/Keystore.
    pub private_key: Vec<u8>,
    pub key_passphrase: Option<String>,
    /// 32-byte iroh identity secret. Store this in platform secure storage.
    pub iroh_secret: Vec<u8>,
    pub remote_port: u16,
    /// Preferred loopback port for the WebView listener. Keep this stable
    /// across launches so origin-scoped browser storage survives; 0 asks the
    /// OS for an ephemeral port.
    pub local_port: u16,
}

impl std::fmt::Debug for MobileLinkTunnelConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MobileLinkTunnelConfig")
            .field("token", &"[redacted]")
            .field("source_device_id", &self.source_device_id)
            .field("target_device_id", &self.target_device_id)
            .field(
                "private_key",
                &format!("<{} bytes>", self.private_key.len()),
            )
            .field(
                "key_passphrase",
                &self.key_passphrase.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "iroh_secret",
                &format!("<{} bytes>", self.iroh_secret.len()),
            )
            .field("remote_port", &self.remote_port)
            .field("local_port", &self.local_port)
            .finish()
    }
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileLinkError {
    #[error("Link is not configured: {detail}")]
    Configuration { detail: String },
    #[error("Account request failed: {detail}")]
    Account { detail: String },
    #[error("Confirm your password to continue")]
    StepUpRequired,
    #[error("iroh identity is invalid")]
    InvalidIdentity,
    #[error("Link connection failed: {detail}")]
    Connection { detail: String },
}

impl From<anyhow::Error> for MobileLinkError {
    fn from(error: anyhow::Error) -> Self {
        Self::Connection {
            detail: format!("{error:#}"),
        }
    }
}

fn account_error(error: anyhow::Error) -> MobileLinkError {
    let detail = format!("{error:#}");
    if detail.contains("403") || detail.to_ascii_lowercase().contains("forbidden") {
        MobileLinkError::StepUpRequired
    } else {
        MobileLinkError::Account { detail }
    }
}

#[uniffi::export]
pub fn link_generate_secret() -> Vec<u8> {
    SecretKey::generate().to_bytes().to_vec()
}

#[uniffi::export]
pub fn link_login(email: String, password: String) -> Result<String, MobileLinkError> {
    let account = AccountClient::production().map_err(|error| MobileLinkError::Configuration {
        detail: format!("{error:#}"),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MobileLinkError::Connection {
            detail: error.to_string(),
        })?;
    let response = runtime
        .block_on(account.login(&email, &password))
        .map_err(|error| MobileLinkError::Account {
            detail: format!("{error:#}"),
        })?;
    Ok(response.token)
}

#[uniffi::export]
pub fn link_reauth(token: String, password: String) -> Result<(), MobileLinkError> {
    let account = AccountClient::production().map_err(|error| MobileLinkError::Configuration {
        detail: format!("{error:#}"),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MobileLinkError::Connection {
            detail: error.to_string(),
        })?;
    runtime
        .block_on(account.reauth(&token, &password))
        .map_err(account_error)?;
    Ok(())
}

#[uniffi::export]
pub fn link_register_device(
    token: String,
    name: String,
    ssh_public_key: String,
    iroh_secret: Vec<u8>,
) -> Result<MobileLinkRegistration, MobileLinkError> {
    let secret: [u8; 32] = iroh_secret
        .try_into()
        .map_err(|_| MobileLinkError::InvalidIdentity)?;
    let secret = SecretKey::from_bytes(&secret);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MobileLinkError::Connection {
            detail: error.to_string(),
        })?;
    runtime.block_on(async move {
        let endpoint = LinkNode::bind(secret).await?;
        let endpoint_id = endpoint.id().to_string();
        let addresses: Vec<String> = endpoint
            .addr()
            .ip_addrs()
            .map(|addr| addr.to_string())
            .collect();
        let account = AccountClient::production()?;
        let device = account
            .add_device(
                &token,
                &RegisterDevice {
                    name,
                    kind: "mobile".into(),
                    iroh_endpoint_id: endpoint_id.clone(),
                    web_endpoint_id: String::new(),
                    ssh_user: String::new(),
                    ssh_port: 22,
                    ssh_public_key,
                    ssh_host_key_fingerprint: String::new(),
                    direct_addresses: addresses.clone(),
                    relay_url: endpoint
                        .addr()
                        .relay_urls()
                        .next()
                        .map(|url| url.to_string())
                        .unwrap_or_default(),
                    relay_urls: endpoint
                        .addr()
                        .relay_urls()
                        .map(|url| url.to_string())
                        .collect(),
                    peer_token: String::new(),
                    peer_port: 9477,
                },
            )
            .await
            .map_err(account_error)?;
        if let Ok(exchange) = account.device_token(&token, &device.id).await
            && let Some(device_token) = exchange.token
            && let Ok(credentials) = account.relay_credentials(&device_token, &endpoint_id).await
        {
            let _ = install_brokered_relays(&endpoint, &credentials.relay_urls, &credentials.token)
                .await;
            let relay_url = credentials.relay_urls.first().cloned().unwrap_or_default();
            let _ = account
                .heartbeat(
                    &device_token,
                    &device.id,
                    &endpoint_id,
                    &addresses,
                    &relay_url,
                    &credentials.relay_urls,
                )
                .await;
        }
        endpoint.close().await;
        Ok(MobileLinkRegistration {
            device_id: device.id,
            endpoint_id,
            direct_addresses: addresses,
        })
    })
}

#[uniffi::export]
pub fn link_devices(token: String) -> Result<Vec<MobileLinkDevice>, MobileLinkError> {
    let account = AccountClient::production().map_err(|error| MobileLinkError::Configuration {
        detail: format!("{error:#}"),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MobileLinkError::Connection {
            detail: error.to_string(),
        })?;
    let devices = runtime
        .block_on(account.list_devices(&token))
        .map_err(|error| MobileLinkError::Account {
            detail: format!("{error:#}"),
        })?;
    Ok(devices
        .into_iter()
        .map(|device| MobileLinkDevice {
            id: device.id,
            name: device.name,
            kind: device.kind,
            ssh_user: device.ssh_user,
            ssh_port: device.ssh_port,
            host_fingerprint: device.ssh_host_key_fingerprint,
        })
        .collect())
}

pub struct MobileLinkTunnel {
    pub ssh: Tunnel,
    pub forward: orqella_link_core::LocalForward,
}

impl MobileLinkTunnel {
    pub async fn stop(self) {
        self.forward.stop().await;
        self.ssh.stop().await;
    }
}

pub async fn start_link(
    config: MobileLinkTunnelConfig,
) -> Result<MobileLinkTunnel, MobileLinkError> {
    let key = decode_private_key(&config.private_key, config.key_passphrase.as_deref()).map_err(
        |error| MobileLinkError::Configuration {
            detail: format!("{error:#}"),
        },
    )?;
    let public_key =
        key.public_key()
            .to_openssh()
            .map_err(|error| MobileLinkError::Configuration {
                detail: error.to_string(),
            })?;
    let local_token = Some(crate::local_tunnel_token(&public_key));
    let secret: [u8; 32] = config
        .iroh_secret
        .try_into()
        .map_err(|_| MobileLinkError::InvalidIdentity)?;
    let secret = SecretKey::from_bytes(&secret);
    let account = AccountClient::production().map_err(|error| MobileLinkError::Configuration {
        detail: format!("{error:#}"),
    })?;
    let device_token = match account
        .device_token(&config.token, &config.source_device_id)
        .await
    {
        Ok(response) => response.token.unwrap_or_else(|| config.token.clone()),
        Err(_) => config.token.clone(),
    };
    let ticket = account
        .ticket(
            &device_token,
            &config.source_device_id,
            &config.target_device_id,
        )
        .await
        .map_err(|error| MobileLinkError::Account {
            detail: format!("{error:#}"),
        })?;
    let target = device_endpoint_addr(&ticket.target)?;
    let credentials = account
        .ssh_credentials(&device_token, &config.target_device_id)
        .await
        .map_err(|error| MobileLinkError::Account {
            detail: format!("{error:#}"),
        })?;
    let certificate = Certificate::from_openssh(&credentials.certificate).map_err(|error| {
        MobileLinkError::Account {
            detail: format!("invalid SSH certificate: {error}"),
        }
    })?;
    let source = TicketSource::new(
        account.clone(),
        device_token.clone(),
        config.source_device_id.clone(),
        config.target_device_id,
    );
    let endpoint = LinkNode::bind(secret).await?;
    let mut installed_relay_urls = Vec::new();
    match account
        .relay_credentials(&device_token, &endpoint.id().to_string())
        .await
    {
        Ok(credentials) => {
            match install_brokered_relays(&endpoint, &credentials.relay_urls, &credentials.token)
                .await
            {
                Ok(installed) => {
                    installed_relay_urls = installed;
                    // The endpoint publishes relay URLs only after the home
                    // relay connects. Wait briefly so the heartbeat and the
                    // subsequent iroh connect see a usable relay path.
                    if timeout(Duration::from_secs(10), endpoint.online())
                        .await
                        .is_err()
                    {
                        tracing::warn!("managed Link relay did not become online within 10s");
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, "could not install managed Link relay credentials");
                    installed_relay_urls = credentials.relay_urls;
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "managed Link relay credentials unavailable; using direct paths");
        }
    }

    // Publish the live endpoint addresses before connecting. Mobile devices
    // move between Wi-Fi, cellular, and VPNs; an old registration can leave the
    // control plane advertising addresses from a previous network.
    let addresses: Vec<String> = endpoint
        .addr()
        .ip_addrs()
        .map(|addr| addr.to_string())
        .collect();
    let mut relay_urls: Vec<String> = endpoint
        .addr()
        .relay_urls()
        .map(|url| url.to_string())
        .collect();
    if relay_urls.is_empty() {
        relay_urls = installed_relay_urls;
    }
    let relay_url = relay_urls.first().cloned().unwrap_or_default();
    // Best effort: never delay the user's connect attempt on heartbeat I/O.
    let _ = timeout(
        Duration::from_secs(3),
        account.heartbeat(
            &device_token,
            &config.source_device_id,
            &endpoint.id().to_string(),
            &addresses,
            &relay_url,
            &relay_urls,
        ),
    )
    .await;

    // The iroh forward is internal to this process; keep it ephemeral. The
    // SSH listener below is what the WebView loads, so it must use the
    // caller's stable loopback port to preserve origin-scoped browser data.
    let forward = start_local_forward_with_endpoint(
        endpoint,
        target,
        ticket.ticket,
        0,
        orqella_link_core::SSH_ALPN.to_vec(),
        Some(source),
        None,
    )
    .await?;
    let ssh_result = timeout(
        MOBILE_START_TIMEOUT,
        start_with_key_and_cert(
            TunnelConfig {
                host: forward.local_addr.ip().to_string(),
                port: forward.local_addr.port(),
                user: ticket.target.ssh_user,
                private_key: Default::default(),
                key_passphrase: config.key_passphrase,
                expected_host_fingerprint: (!ticket.target.ssh_host_key_fingerprint.is_empty())
                    .then_some(ticket.target.ssh_host_key_fingerprint),
                remote_host: "127.0.0.1".into(),
                remote_port: config.remote_port,
                local_port: config.local_port,
                local_token: local_token.clone(),
            },
            key,
            Some(certificate),
        ),
    )
    .await;
    let ssh = match ssh_result {
        Ok(Ok(ssh)) => ssh,
        Ok(Err(error)) => {
            let forward_error = forward.last_error();
            forward.stop().await;
            let mut detail = friendly_connection_error(&error);
            if let Some(forward_error) = forward_error {
                detail = format!("{detail}; iroh forward: {forward_error}");
            }
            return Err(MobileLinkError::Connection { detail });
        }
        Err(_) => {
            let forward_error = forward.last_error();
            forward.stop().await;
            let mut detail = "connection timed out".to_string();
            if let Some(forward_error) = forward_error {
                detail = format!("{detail}; iroh forward: {forward_error}");
            }
            return Err(MobileLinkError::Connection { detail });
        }
    };
    Ok(MobileLinkTunnel { ssh, forward })
}

fn friendly_connection_error(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    let lower = text.to_lowercase();
    if lower.contains("no direct path") || lower.contains("relayed ssh") {
        "could not establish a secure connection to the hub".into()
    } else {
        text
    }
}
