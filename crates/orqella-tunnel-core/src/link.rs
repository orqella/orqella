//! Account-assisted mobile link transport.
//!
//! Mobile clients use the same direct iroh/SSH bridge as the daemon, but keep
//! their SSH key and iroh identity in platform secure storage. The account
//! service supplies device metadata and short-lived tickets; the SSH tunnel
//! itself stays inside the direct iroh connection.

use crate::{Tunnel, TunnelConfig, decode_private_key, start_with_key_cert_gateway};
use anyhow::Result;
use orqella_link_core::iroh::SecretKey;
use orqella_link_core::{
    AccountClient, LinkNode, LoginOutcome, ReauthOutcome, RegisterDevice, device_endpoint_addr,
    install_brokered_relays, request_ssh_certificate, start_local_forward_with_endpoint,
};
use russh::keys::Certificate;
use std::time::Duration;
use tokio::time::timeout;

const MOBILE_START_TIMEOUT: Duration = Duration::from_secs(35);
/// Reuse material this far before its real expiry so a slow connect does not
/// race a just-expired certificate or relay credential.
const RECONNECT_CACHE_SKEW_SECONDS: u64 = 30;

#[derive(Debug, Clone)]
struct CachedSshCertificate {
    target_device_id: String,
    public_key: String,
    certificate: String,
    expires_at: u64,
}

#[derive(Debug, Clone)]
struct CachedRelayCredentials {
    endpoint_id: String,
    relay_urls: Vec<String>,
    token: String,
    expires_at: u64,
}

/// In-memory fast-reconnect material owned by [`crate::MobileTunnel`].
///
/// Nothing here is persisted. A user sign-out calls `clear` so a later account
/// on the same process cannot reuse another account's relay token.
#[derive(Debug, Default)]
pub(crate) struct LinkReconnectCache {
    certificate: Option<CachedSshCertificate>,
    relay: Option<CachedRelayCredentials>,
}

impl LinkReconnectCache {
    pub(crate) fn clear(&mut self) {
        self.certificate = None;
        self.relay = None;
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum MobileLoginOutcome {
    Session {
        token: String,
    },
    MfaRequired {
        challenge_token: String,
        methods: Vec<String>,
        expires_at: String,
    },
    EnrollmentRequired {
        enrollment_token: String,
        methods: Vec<String>,
        expires_at: String,
    },
    VerificationRequired,
    PasskeyRequired,
}

#[derive(Debug, Clone, uniffi::Enum)]
pub enum MobileReauthOutcome {
    Complete,
    MfaRequired {
        challenge_token: String,
        methods: Vec<String>,
        expires_at: String,
    },
    PasskeyRequired,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileSecurityStatus {
    pub totp_enabled: bool,
    pub passkey_count: u32,
    pub recovery_codes_remaining: u32,
    pub second_factor_required: bool,
    pub second_factor_enabled: bool,
    pub password_login_methods: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileTotpSetup {
    pub secret: String,
    pub otpauth_uri: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileTotpEnableResult {
    pub token: Option<String>,
    pub recovery_codes: Vec<String>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct MobileRecoveryCodes {
    pub recovery_codes: Vec<String>,
}

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
    /// True when `token` is already a device-scoped Link token. Reconnecting
    /// can then skip the `/v1/device-token` exchange.
    pub using_device_token: bool,
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
            .field("using_device_token", &self.using_device_token)
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
pub fn link_login(email: String, password: String) -> Result<MobileLoginOutcome, MobileLinkError> {
    let account = AccountClient::production().map_err(|error| MobileLinkError::Configuration {
        detail: format!("{error:#}"),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MobileLinkError::Connection {
            detail: error.to_string(),
        })?;
    let outcome = runtime
        .block_on(account.login(&email, &password))
        .map_err(|error| MobileLinkError::Account {
            detail: format!("{error:#}"),
        })?;
    Ok(match outcome {
        LoginOutcome::Session(auth) => MobileLoginOutcome::Session { token: auth.token },
        LoginOutcome::MfaRequired(challenge) => MobileLoginOutcome::MfaRequired {
            challenge_token: challenge.challenge_token,
            methods: challenge.methods,
            expires_at: challenge.expires_at,
        },
        LoginOutcome::EnrollmentRequired(challenge) => MobileLoginOutcome::EnrollmentRequired {
            enrollment_token: challenge.enrollment_token,
            methods: challenge.methods,
            expires_at: challenge.expires_at,
        },
        LoginOutcome::VerificationRequired => MobileLoginOutcome::VerificationRequired,
        LoginOutcome::PasskeyRequired => MobileLoginOutcome::PasskeyRequired,
    })
}

#[uniffi::export]
pub fn link_login_mfa(challenge_token: String, code: String) -> Result<String, MobileLinkError> {
    let account = AccountClient::production().map_err(|error| MobileLinkError::Configuration {
        detail: format!("{error:#}"),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MobileLinkError::Connection {
            detail: error.to_string(),
        })?;
    let auth = runtime
        .block_on(account.login_mfa(&challenge_token, &code))
        .map_err(|error| MobileLinkError::Account {
            detail: format!("{error:#}"),
        })?;
    Ok(auth.token)
}

#[uniffi::export]
pub fn link_reauth(
    token: String,
    password: String,
) -> Result<MobileReauthOutcome, MobileLinkError> {
    let account = AccountClient::production().map_err(|error| MobileLinkError::Configuration {
        detail: format!("{error:#}"),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MobileLinkError::Connection {
            detail: error.to_string(),
        })?;
    let outcome = runtime
        .block_on(account.reauth(&token, &password))
        .map_err(account_error)?;
    Ok(match outcome {
        ReauthOutcome::Complete => MobileReauthOutcome::Complete,
        ReauthOutcome::MfaRequired(challenge) => MobileReauthOutcome::MfaRequired {
            challenge_token: challenge.challenge_token,
            methods: challenge.methods,
            expires_at: challenge.expires_at,
        },
        ReauthOutcome::PasskeyRequired => MobileReauthOutcome::PasskeyRequired,
    })
}

#[uniffi::export]
pub fn link_reauth_mfa(
    token: String,
    challenge_token: String,
    code: String,
) -> Result<(), MobileLinkError> {
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
        .block_on(account.reauth_mfa(&token, &challenge_token, &code))
        .map_err(account_error)?;
    Ok(())
}

#[uniffi::export]
pub fn link_security_status(token: String) -> Result<MobileSecurityStatus, MobileLinkError> {
    let account = AccountClient::production().map_err(|error| MobileLinkError::Configuration {
        detail: format!("{error:#}"),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MobileLinkError::Connection {
            detail: error.to_string(),
        })?;
    let status = runtime
        .block_on(account.security_status(&token))
        .map_err(account_error)?;
    Ok(MobileSecurityStatus {
        totp_enabled: status.totp_enabled,
        passkey_count: status.passkey_count,
        recovery_codes_remaining: status.recovery_codes_remaining,
        second_factor_required: status.second_factor_required,
        second_factor_enabled: status.second_factor_enabled,
        password_login_methods: status.password_login_methods,
    })
}

#[uniffi::export]
pub fn link_totp_setup(token: String) -> Result<MobileTotpSetup, MobileLinkError> {
    let account = AccountClient::production().map_err(|error| MobileLinkError::Configuration {
        detail: format!("{error:#}"),
    })?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| MobileLinkError::Connection {
            detail: error.to_string(),
        })?;
    let setup = runtime
        .block_on(account.totp_setup(&token))
        .map_err(account_error)?;
    Ok(MobileTotpSetup {
        secret: setup.secret,
        otpauth_uri: setup.otpauth_uri,
        expires_at: setup.expires_at,
    })
}

#[uniffi::export]
pub fn link_totp_enable(
    token: String,
    code: String,
) -> Result<MobileTotpEnableResult, MobileLinkError> {
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
        .block_on(account.totp_enable(&token, &code))
        .map_err(account_error)?;
    Ok(MobileTotpEnableResult {
        token: response.token,
        recovery_codes: response.recovery_codes,
    })
}

#[uniffi::export]
pub fn link_totp_disable(
    token: String,
    password: String,
    code: String,
) -> Result<(), MobileLinkError> {
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
        .block_on(account.totp_disable(&token, &password, &code, true))
        .map_err(account_error)?;
    Ok(())
}

#[uniffi::export]
pub fn link_recovery_codes(
    token: String,
    password: String,
    code: String,
) -> Result<MobileRecoveryCodes, MobileLinkError> {
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
        .block_on(account.regenerate_recovery_codes(&token, &password, &code))
        .map_err(account_error)?;
    Ok(MobileRecoveryCodes {
        recovery_codes: response.recovery_codes,
    })
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
        // Detach before stopping the forward so new WebView connections wait
        // for the replacement transport instead of using a dying session.
        self.ssh.clear_backend();
        self.forward.stop().await;
        self.ssh.stop().await;
    }
}

pub async fn start_link(
    config: MobileLinkTunnelConfig,
) -> Result<MobileLinkTunnel, MobileLinkError> {
    start_link_with_cache(config, &mut LinkReconnectCache::default()).await
}

pub(crate) async fn start_link_with_cache(
    config: MobileLinkTunnelConfig,
    cache: &mut LinkReconnectCache,
) -> Result<MobileLinkTunnel, MobileLinkError> {
    start_link_with_cache_gateway(config, cache, None).await
}

pub(crate) async fn start_link_with_cache_gateway(
    config: MobileLinkTunnelConfig,
    cache: &mut LinkReconnectCache,
    gateway: Option<std::sync::Arc<crate::TunnelGateway>>,
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
    let device_token = if config.using_device_token {
        config.token.clone()
    } else {
        match account
            .device_token(&config.token, &config.source_device_id)
            .await
        {
            Ok(response) => response.token.unwrap_or_else(|| config.token.clone()),
            Err(_) => config.token.clone(),
        }
    };
    let endpoint = LinkNode::bind(secret).await?;
    let endpoint_id = endpoint.id().to_string();
    let cached_relay = cache
        .relay
        .as_ref()
        .filter(|cached| {
            cached.endpoint_id == endpoint_id
                && cached.expires_at > unix_now() + RECONNECT_CACHE_SKEW_SECONDS
        })
        .cloned();
    let relay_credentials = match cached_relay {
        Some(cached) => Some((cached.relay_urls, cached.token)),
        None => match account.relay_credentials(&device_token, &endpoint_id).await {
            Ok(credentials) => {
                cache.relay = Some(CachedRelayCredentials {
                    endpoint_id: endpoint_id.clone(),
                    relay_urls: credentials.relay_urls.clone(),
                    token: credentials.token.clone(),
                    expires_at: credentials.expires_at,
                });
                Some((credentials.relay_urls, credentials.token))
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "managed Link relay credentials unavailable; using direct paths"
                );
                None
            }
        },
    };
    let mut installed_relay_urls = Vec::new();
    if let Some((relay_urls, token)) = relay_credentials {
        match install_brokered_relays(&endpoint, &relay_urls, &token).await {
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
                cache.relay = None;
                installed_relay_urls = relay_urls;
            }
        }
    }

    // Fetch the ticket only after endpoint/relay setup so its short TTL is
    // spent on the certificate and SSH handshakes, not on local setup.
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

    let cached_certificate = cache
        .certificate
        .as_ref()
        .filter(|cached| {
            cached.target_device_id == config.target_device_id
                && cached.public_key == public_key
                && cached.expires_at > unix_now() + RECONNECT_CACHE_SKEW_SECONDS
        })
        .and_then(|cached| Certificate::from_openssh(&cached.certificate).ok());
    let used_cached_certificate = cached_certificate.is_some();
    let certificate = match cached_certificate {
        Some(certificate) => certificate,
        None => {
            let credentials = request_ssh_certificate(
                endpoint.clone(),
                target.clone(),
                ticket.ticket.clone(),
                None,
            )
            .await
            .map_err(|error| MobileLinkError::Account {
                detail: format!("{error:#}"),
            })?;
            let certificate =
                Certificate::from_openssh(&credentials.certificate).map_err(|error| {
                    MobileLinkError::Account {
                        detail: format!("invalid SSH certificate: {error}"),
                    }
                })?;
            cache.certificate = Some(CachedSshCertificate {
                target_device_id: config.target_device_id.clone(),
                public_key: public_key.clone(),
                certificate: credentials.certificate,
                expires_at: credentials.expires_at,
            });
            certificate
        }
    };
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
    let heartbeat_account = account.clone();
    let heartbeat_token = device_token.clone();
    let heartbeat_source = config.source_device_id.clone();
    let heartbeat_endpoint = endpoint_id.clone();
    tokio::spawn(async move {
        let _ = timeout(
            Duration::from_secs(3),
            heartbeat_account.heartbeat(
                &heartbeat_token,
                &heartbeat_source,
                &heartbeat_endpoint,
                &addresses,
                &relay_url,
                &relay_urls,
            ),
        )
        .await;
    });

    // A ticket is single-use. If the certificate request consumed the first
    // ticket, fetch a fresh one for the SSH ALPN. With a cached certificate
    // the original ticket is still unused and can carry SSH directly.
    let (ssh_ticket, ssh_target, ssh_device) = if used_cached_certificate {
        (ticket.ticket, target, ticket.target)
    } else {
        let fresh = account
            .ticket(
                &device_token,
                &config.source_device_id,
                &config.target_device_id,
            )
            .await
            .map_err(|error| MobileLinkError::Account {
                detail: format!("{error:#}"),
            })?;
        let target = device_endpoint_addr(&fresh.target)?;
        (fresh.ticket, target, fresh.target)
    };

    // The iroh forward is internal to this process; keep it ephemeral. The
    // SSH listener below is what the WebView loads, so it must use the
    // caller's stable loopback port to preserve origin-scoped browser data.
    let forward = start_local_forward_with_endpoint(
        endpoint,
        ssh_target,
        ssh_ticket,
        0,
        orqella_link_core::SSH_ALPN.to_vec(),
        None,
        None,
    )
    .await?;
    let ssh_result = timeout(
        MOBILE_START_TIMEOUT,
        start_with_key_cert_gateway(
            TunnelConfig {
                host: forward.local_addr.ip().to_string(),
                port: forward.local_addr.port(),
                user: ssh_device.ssh_user,
                private_key: Default::default(),
                key_passphrase: config.key_passphrase,
                expected_host_fingerprint: (!ssh_device.ssh_host_key_fingerprint.is_empty())
                    .then_some(ssh_device.ssh_host_key_fingerprint),
                remote_host: "127.0.0.1".into(),
                remote_port: config.remote_port,
                local_port: config.local_port,
                local_token: local_token.clone(),
            },
            key,
            Some(certificate),
            gateway,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clearing_the_reconnect_cache_drops_certificate_and_relay_material() {
        let mut cache = LinkReconnectCache {
            certificate: Some(CachedSshCertificate {
                target_device_id: "target".into(),
                public_key: "ssh-ed25519 AAAA".into(),
                certificate: "certificate".into(),
                expires_at: unix_now() + 600,
            }),
            relay: Some(CachedRelayCredentials {
                endpoint_id: "endpoint".into(),
                relay_urls: vec!["https://relay.example".into()],
                token: "secret".into(),
                expires_at: unix_now() + 900,
            }),
        };

        cache.clear();

        assert!(cache.certificate.is_none());
        assert!(cache.relay.is_none());
    }
}
