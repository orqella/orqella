//! Account-assisted, direct-only SSH connectivity.
//!
//! This module is the first implementation slice of the Orqella Link design. It
//! talks to an account/control-plane service for device registration and
//! short-lived tickets, then carries SSH over a direct iroh QUIC stream.
//!
//! Relays are disabled by default. Optional assisted mode may use an iroh relay
//! for address discovery and hole punching, but SSH bytes are never opened until
//! a direct path is selected. The target verifies a signed ticket before opening
//! a connection to its local SSH server.

pub use iroh;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, VerifyingKey};
use futures_util::StreamExt;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey,
    TransportAddr,
    endpoint::{Builder, Connection, RecvStream, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

pub const SSH_ALPN: &[u8] = b"orqella/ssh/1";
pub const PEER_ALPN: &[u8] = b"orqella/peer/1";
/// How long SSH may wait for a direct path before failing. This is deliberately
/// separate from the relay/tunnel timeout so cellular hole punching has time to
/// settle without allowing a relayed SSH stream.
pub const SSH_DIRECT_WAIT: Duration = Duration::from_secs(25);
const MAX_TICKET_BYTES: usize = 64 * 1024;

/// The Orqella Link account service used by release builds.
///
/// Development builds may override this at compile time with
/// `ORQELLA_LINK_ACCOUNT_URL`. There is no runtime account-service setting.
pub const LINK_ACCOUNT_URL: &str = match option_env!("ORQELLA_LINK_ACCOUNT_URL") {
    Some(value) => value,
    None => "https://link.orqella.io",
};

fn endpoint_builder(secret: SecretKey, port: u16) -> Result<Builder> {
    let builder = Endpoint::builder(presets::N0)
        .secret_key(secret)
        .relay_mode(RelayMode::Custom(RelayMap::empty()));
    if port == 0 {
        Ok(builder)
    } else {
        builder
            .bind_addr(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port))
            .with_context(|| format!("could not bind the Link UDP port {port}"))
    }
}

async fn wait_for_relay(endpoint: &Endpoint, label: &str) {
    if endpoint.addr().relay_urls().next().is_none() {
        return;
    }
    if tokio::time::timeout(Duration::from_secs(10), endpoint.online())
        .await
        .is_err()
    {
        tracing::warn!("{label} relay did not become online within 10s");
    }
}

pub fn endpoint_relay_urls(endpoint: &Endpoint) -> Vec<String> {
    endpoint
        .addr()
        .relay_urls()
        .map(|url| url.to_string())
        .collect()
}

/// Relay URLs to publish in device records and tickets.
pub fn advertised_relay_urls(endpoint: &Endpoint) -> Vec<String> {
    endpoint_relay_urls(endpoint)
}

/// Direct addresses to publish in device records and tickets.
pub fn advertised_direct_addresses(endpoint: &Endpoint) -> Vec<String> {
    endpoint
        .addr()
        .ip_addrs()
        .map(|addr| addr.to_string())
        .collect()
}

/// Install or refresh endpoint-bound managed relay credentials.
///
/// The relay token is a bearer credential. It is never logged or returned by
/// status; only the URL list is published.
pub async fn install_brokered_relays(
    endpoint: &Endpoint,
    urls: &[String],
    token: &str,
) -> Result<Vec<String>> {
    let mut installed = Vec::new();
    for value in urls {
        let relay: RelayUrl = value
            .parse()
            .with_context(|| format!("invalid managed relay URL `{value}`"))?;
        let config = RelayConfig::from(relay.clone()).with_auth_token(token.to_string());
        endpoint.insert_relay(relay.clone(), Arc::new(config)).await;
        installed.push(relay.to_string());
    }
    Ok(installed)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountUser {
    pub id: String,
    pub email: String,
    #[serde(default)]
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entitlement {
    #[serde(default)]
    pub link_entitled: bool,
    #[serde(default)]
    pub link_plan: String,
    #[serde(default)]
    pub relay_enabled: bool,
    #[serde(default)]
    pub max_devices: u32,
    #[serde(default)]
    pub billing_url: String,
    /// Set while the account is in a Link free trial. During the trial this is
    /// the trial end; after conversion it is absent.
    #[serde(default)]
    pub trial_end: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct HeartbeatResponse {
    #[serde(default)]
    pub entitlement: Option<Entitlement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: AccountUser,
    #[serde(default)]
    pub entitlement: Option<Entitlement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MfaChallenge {
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub challenge_token: String,
    #[serde(default)]
    pub enrollment_token: String,
    #[serde(default)]
    pub expires_at: String,
}

/// The complete result of a password sign-in attempt.
///
/// Passkey sign-in always returns a session. Password sign-in is a fallback:
/// it may require a second factor, first-factor enrollment, or email
/// verification.
#[derive(Debug, Clone)]
pub enum LoginOutcome {
    Session(AuthResponse),
    MfaRequired(MfaChallenge),
    EnrollmentRequired(MfaChallenge),
    VerificationRequired,
    PasskeyRequired,
}

/// The complete result of a password re-authentication attempt.
#[derive(Debug, Clone)]
pub enum ReauthOutcome {
    Complete,
    MfaRequired(MfaChallenge),
    PasskeyRequired,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SecurityStatus {
    #[serde(default)]
    pub totp_enabled: bool,
    #[serde(default)]
    pub passkey_count: u32,
    #[serde(default)]
    pub recovery_codes_remaining: u32,
    #[serde(default)]
    pub second_factor_required: bool,
    #[serde(default)]
    pub second_factor_enabled: bool,
    #[serde(default)]
    pub password_login_methods: Vec<String>,
    #[serde(default)]
    pub passkey_login_available: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpSetup {
    pub secret: String,
    pub otpauth_uri: String,
    #[serde(default)]
    pub expires_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TotpEnableResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub recovery_codes: Vec<String>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub user: Option<AccountUser>,
    #[serde(default)]
    pub entitlement: Option<Entitlement>,
    #[serde(default)]
    pub security: Option<SecurityStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryCodesResponse {
    #[serde(default)]
    pub ok: bool,
    #[serde(default)]
    pub recovery_codes: Vec<String>,
    #[serde(default)]
    pub security: Option<SecurityStatus>,
}

#[derive(Debug, Deserialize)]
struct LoginEnvelope {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    user: Option<AccountUser>,
    #[serde(default)]
    entitlement: Option<Entitlement>,
    #[serde(default)]
    methods: Vec<String>,
    #[serde(default)]
    challenge_token: Option<String>,
    #[serde(default)]
    enrollment_token: Option<String>,
    #[serde(default)]
    expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceStartResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicePollResponse {
    pub status: String,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub user: Option<AccountUser>,
    #[serde(default)]
    pub entitlement: Option<Entitlement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceTokenResponse {
    pub device_id: String,
    #[serde(default)]
    pub token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayCredentials {
    pub relay_urls: Vec<String>,
    pub token: String,
    pub expires_at: u64,
    #[serde(default)]
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SshCredentials {
    pub certificate: String,
    pub expires_at: u64,
    #[serde(default)]
    pub ttl_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountConfig {
    pub version: u32,
    pub signing_public_key: String,
    #[serde(default)]
    pub signing_key_id: String,
    #[serde(default)]
    pub signing_public_keys: Vec<SigningPublicKey>,
    pub ticket_ttl_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SigningPublicKey {
    #[serde(default)]
    pub kid: String,
    pub public_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: String,
    pub iroh_endpoint_id: String,
    #[serde(default)]
    pub web_endpoint_id: String,
    #[serde(default)]
    pub ssh_user: String,
    #[serde(default = "default_ssh_port")]
    pub ssh_port: u16,
    #[serde(default)]
    pub ssh_public_key: String,
    #[serde(default)]
    pub ssh_host_key_fingerprint: String,
    #[serde(default)]
    pub direct_addresses: Vec<String>,
    #[serde(default)]
    pub relay_url: String,
    #[serde(default)]
    pub relay_urls: Vec<String>,
    #[serde(default = "default_peer_port")]
    pub peer_port: u16,
    #[serde(default)]
    pub last_seen_at: String,
    #[serde(default)]
    pub created_at: String,
}

/// Device metadata that is safe to hand to the browser.
///
/// SSH public keys and peer credentials are intentionally omitted. Keep this
/// view explicit so adding a sensitive field to [`Device`] does not
/// accidentally expose it.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceView {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub iroh_endpoint_id: String,
    pub web_endpoint_id: String,
    pub ssh_user: String,
    pub ssh_port: u16,
    pub ssh_host_key_fingerprint: String,
    pub direct_addresses: Vec<String>,
    pub relay_url: String,
    pub relay_urls: Vec<String>,
    pub peer_port: u16,
    pub last_seen_at: String,
    pub created_at: String,
}

impl From<&Device> for DeviceView {
    fn from(device: &Device) -> Self {
        Self {
            id: device.id.clone(),
            name: device.name.clone(),
            kind: device.kind.clone(),
            iroh_endpoint_id: device.iroh_endpoint_id.clone(),
            web_endpoint_id: device.web_endpoint_id.clone(),
            ssh_user: device.ssh_user.clone(),
            ssh_port: device.ssh_port,
            ssh_host_key_fingerprint: device.ssh_host_key_fingerprint.clone(),
            direct_addresses: device.direct_addresses.clone(),
            relay_url: device.relay_url.clone(),
            relay_urls: device.relay_urls.clone(),
            peer_port: device.peer_port,
            last_seen_at: device.last_seen_at.clone(),
            created_at: device.created_at.clone(),
        }
    }
}

fn default_ssh_port() -> u16 {
    22
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterDevice {
    pub name: String,
    pub kind: String,
    pub iroh_endpoint_id: String,
    #[serde(default)]
    pub web_endpoint_id: String,
    pub ssh_user: String,
    pub ssh_port: u16,
    pub ssh_public_key: String,
    pub ssh_host_key_fingerprint: String,
    pub direct_addresses: Vec<String>,
    pub relay_url: String,
    #[serde(default)]
    pub relay_urls: Vec<String>,
    #[serde(default = "default_peer_port")]
    pub peer_port: u16,
}

fn default_peer_port() -> u16 {
    9477
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketResponse {
    pub ticket: String,
    pub expires_at: String,
    pub target: Device,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketPayload {
    pub v: u32,
    #[serde(default)]
    pub kid: String,
    pub source_device_id: String,
    pub source_endpoint_id: String,
    pub target_device_id: String,
    pub target_endpoint_id: String,
    pub target_addresses: Vec<String>,
    #[serde(default)]
    pub target_relay_url: String,
    #[serde(default)]
    pub target_relay_urls: Vec<String>,
    pub target_ssh_user: String,
    pub target_ssh_port: u16,
    pub target_host_fingerprint: String,
    pub exp: u64,
    pub nonce: String,
}

#[derive(Clone, Debug)]
pub struct AccountClient {
    http: reqwest::Client,
    base: String,
}

impl AccountClient {
    /// Client for the fixed Orqella Link service.
    pub fn production() -> Result<Self> {
        Self::new(LINK_ACCOUNT_URL)
    }

    fn new(base_url: impl Into<String>) -> Result<Self> {
        let base = base_url.into().trim_end_matches('/').to_string();
        if !(base.starts_with("http://") || base.starts_with("https://")) {
            bail!("account URL must start with http:// or https://");
        }
        Ok(Self {
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(20))
                .build()?,
            base,
        })
    }

    pub async fn config(&self) -> Result<AccountConfig> {
        Ok(self
            .http
            .get(format!("{}/v1/config", self.base))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn register_account(&self, email: &str, password: &str) -> Result<AuthResponse> {
        Ok(self
            .http
            .post(format!("{}/v1/accounts/register", self.base))
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn login(&self, email: &str, password: &str) -> Result<LoginOutcome> {
        let response: LoginEnvelope = self
            .http
            .post(format!("{}/v1/accounts/login", self.base))
            .json(&serde_json::json!({ "email": email, "password": password }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(token) = response.token {
            let user = response
                .user
                .ok_or_else(|| anyhow::anyhow!("login returned a token without a user"))?;
            return Ok(LoginOutcome::Session(AuthResponse {
                token,
                user,
                entitlement: response.entitlement,
            }));
        }
        let challenge = MfaChallenge {
            methods: response.methods.clone(),
            challenge_token: response.challenge_token.unwrap_or_default(),
            enrollment_token: response.enrollment_token.unwrap_or_default(),
            expires_at: response.expires_at.unwrap_or_default(),
        };
        match response.status.as_deref() {
            Some("mfa_required") => Ok(LoginOutcome::MfaRequired(challenge)),
            Some("enrollment_required") => Ok(LoginOutcome::EnrollmentRequired(challenge)),
            Some("verification_required") => Ok(LoginOutcome::VerificationRequired),
            Some("passkey_required") => Ok(LoginOutcome::PasskeyRequired),
            Some(other) => anyhow::bail!("unexpected sign-in status `{other}`"),
            None => anyhow::bail!("sign-in response did not include a session"),
        }
    }

    /// Complete a password sign-in that required a second factor.
    pub async fn login_mfa(&self, challenge_token: &str, code: &str) -> Result<AuthResponse> {
        Ok(self
            .http
            .post(format!("{}/v1/accounts/login/mfa", self.base))
            .json(&serde_json::json!({
                "challenge_token": challenge_token,
                "code": code,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn security_status(&self, token: &str) -> Result<SecurityStatus> {
        #[derive(Deserialize)]
        struct Response {
            security: SecurityStatus,
        }
        let response: Response = self
            .http
            .get(format!("{}/v1/accounts/security", self.base))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.security)
    }

    pub async fn totp_setup(&self, token: &str) -> Result<TotpSetup> {
        Ok(self
            .http
            .post(format!("{}/v1/accounts/mfa/totp/setup", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({}))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn totp_enable(&self, token: &str, code: &str) -> Result<TotpEnableResponse> {
        Ok(self
            .http
            .post(format!("{}/v1/accounts/mfa/totp/enable", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "code": code }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn totp_disable(
        &self,
        token: &str,
        password: &str,
        code: &str,
        sign_out_devices: bool,
    ) -> Result<()> {
        self.http
            .post(format!("{}/v1/accounts/mfa/totp/disable", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "password": password,
                "code": code,
                "sign_out_devices": sign_out_devices,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn regenerate_recovery_codes(
        &self,
        token: &str,
        password: &str,
        code: &str,
    ) -> Result<RecoveryCodesResponse> {
        Ok(self
            .http
            .post(format!("{}/v1/accounts/mfa/recovery-codes", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "password": password,
                "code": code,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Confirm a password and either complete the step-up or return the
    /// second-factor challenge that is still required.
    pub async fn reauth(&self, token: &str, password: &str) -> Result<ReauthOutcome> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            status: Option<String>,
            #[serde(default)]
            methods: Vec<String>,
            #[serde(default)]
            challenge_token: Option<String>,
            #[serde(default)]
            expires_at: Option<String>,
        }
        let response: Response = self
            .http
            .post(format!("{}/v1/accounts/reauth", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "password": password }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match response.status.as_deref() {
            Some("mfa_required") => Ok(ReauthOutcome::MfaRequired(MfaChallenge {
                methods: response.methods,
                challenge_token: response.challenge_token.unwrap_or_default(),
                enrollment_token: String::new(),
                expires_at: response.expires_at.unwrap_or_default(),
            })),
            Some("passkey_required") => Ok(ReauthOutcome::PasskeyRequired),
            _ => Ok(ReauthOutcome::Complete),
        }
    }

    pub async fn reauth_mfa(&self, token: &str, challenge_token: &str, code: &str) -> Result<()> {
        self.http
            .post(format!("{}/v1/accounts/reauth/mfa", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "challenge_token": challenge_token,
                "code": code,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn add_device(&self, token: &str, device: &RegisterDevice) -> Result<Device> {
        #[derive(Deserialize)]
        struct Response {
            device: Device,
        }
        let response: Response = self
            .http
            .post(format!("{}/v1/devices", self.base))
            .bearer_auth(token)
            .json(device)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.device)
    }

    pub async fn device_start(&self) -> Result<DeviceStartResponse> {
        let response = self
            .http
            .post(format!("{}/v1/device/start", self.base))
            .json(&serde_json::json!({
                "client_name": "Orqella client",
                "platform": std::env::consts::OS,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response)
    }

    pub async fn device_poll(&self, device_code: &str) -> Result<DevicePollResponse> {
        let response = self
            .http
            .post(format!("{}/v1/device/poll", self.base))
            .json(&serde_json::json!({ "device_code": device_code }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response)
    }

    /// Exchange a one-time local handoff code using this hub's device token.
    pub async fn exchange_web_handoff(
        &self,
        device_token: &str,
        code: &str,
    ) -> Result<AccountUser> {
        #[derive(Deserialize)]
        struct Response {
            user: AccountUser,
        }
        let response: Response = self
            .http
            .post(format!("{}/v1/web/handoff/exchange", self.base))
            .bearer_auth(device_token)
            .json(&serde_json::json!({ "code": code }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.user)
    }

    pub async fn list_devices(&self, token: &str) -> Result<Vec<Device>> {
        #[derive(Deserialize)]
        struct Response {
            devices: Vec<Device>,
        }
        let response: Response = self
            .http
            .get(format!("{}/v1/devices", self.base))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.devices)
    }

    pub async fn device_token(&self, token: &str, device_id: &str) -> Result<DeviceTokenResponse> {
        Ok(self
            .http
            .post(format!("{}/v1/device-token", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "device_id": device_id }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Extend a device token's expiry without changing the bearer.
    ///
    /// The daemon calls this periodically so a short TTL does not force
    /// re-enrollment while it is running.
    pub async fn refresh_device_token(&self, token: &str) -> Result<()> {
        self.http
            .post(format!("{}/v1/device-token/refresh", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({}))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    pub async fn entitlement(&self, token: &str) -> Result<Entitlement> {
        #[derive(Deserialize)]
        struct Response {
            entitlement: Entitlement,
        }
        let response: Response = self
            .http
            .get(format!("{}/v1/entitlement", self.base))
            .bearer_auth(token)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.entitlement)
    }

    pub async fn relay_credentials(
        &self,
        token: &str,
        endpoint_id: &str,
    ) -> Result<RelayCredentials> {
        Ok(self
            .http
            .post(format!("{}/v1/relay/credentials", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({ "iroh_endpoint_id": endpoint_id }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Mint endpoint-bound managed relay credentials for a browser endpoint.
    ///
    /// The caller must be a registered device. The hub uses this to delegate
    /// managed relay access to a paired browser without exposing the project
    /// API secret.
    pub async fn web_relay_credentials(
        &self,
        token: &str,
        endpoint_id: &str,
        session_token: Option<&str>,
    ) -> Result<RelayCredentials> {
        let mut payload = serde_json::json!({ "iroh_endpoint_id": endpoint_id });
        if let Some(session_token) = session_token {
            payload["session_token"] = serde_json::json!(session_token);
        }
        Ok(self
            .http
            .post(format!("{}/v1/web/relay", self.base))
            .bearer_auth(token)
            .json(&payload)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    /// Ask Link to authorize a browser session for this hub's web endpoint.
    pub async fn authorize_web_session(
        &self,
        hub_token: &str,
        session_token: &str,
        web_endpoint_id: &str,
    ) -> Result<bool> {
        #[derive(Deserialize)]
        struct Response {
            #[serde(default)]
            ok: bool,
        }
        let response: Response = self
            .http
            .post(format!("{}/v1/web/authorize", self.base))
            .bearer_auth(hub_token)
            .json(&serde_json::json!({
                "session_token": session_token,
                "web_endpoint_id": web_endpoint_id,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response.ok)
    }

    pub async fn heartbeat(
        &self,
        token: &str,
        device_id: &str,
        endpoint_id: &str,
        addresses: &[String],
        relay_url: &str,
        relay_urls: &[String],
    ) -> Result<HeartbeatResponse> {
        let response: HeartbeatResponse = self
            .http
            .post(format!("{}/v1/devices/{device_id}/heartbeat", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "iroh_endpoint_id": endpoint_id,
                "direct_addresses": addresses,
                "relay_url": relay_url,
                "relay_urls": relay_urls,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(response)
    }

    pub async fn ticket(
        &self,
        token: &str,
        source_device_id: &str,
        target_device_id: &str,
    ) -> Result<TicketResponse> {
        Ok(self
            .http
            .post(format!("{}/v1/tickets", self.base))
            .bearer_auth(token)
            .json(&serde_json::json!({
                "source_device_id": source_device_id,
                "target_device_id": target_device_id,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }
}

#[derive(Clone, Debug)]
pub struct TicketSource {
    account: AccountClient,
    token: String,
    source_device_id: String,
    target_device_id: String,
}

impl TicketSource {
    pub fn new(
        account: AccountClient,
        token: String,
        source_device_id: String,
        target_device_id: String,
    ) -> Self {
        Self {
            account,
            token,
            source_device_id,
            target_device_id,
        }
    }

    /// Fetch a ticket and its current target descriptor. Peer forwards use the
    /// target from this response on every local connection so a peer that
    /// rebound to a new direct address is picked up without restarting Link.
    pub async fn ticket_response(&self) -> Result<TicketResponse> {
        self.account
            .ticket(&self.token, &self.source_device_id, &self.target_device_id)
            .await
    }

    pub async fn ticket(&self) -> Result<String> {
        Ok(self.ticket_response().await?.ticket)
    }
}

pub fn load_or_create_secret(path: &Path) -> Result<SecretKey> {
    if path.exists() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        let bytes = std::fs::read(path)?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid iroh secret key length"))?;
        return Ok(SecretKey::from_bytes(&bytes));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let key = SecretKey::generate();
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
            file.write_all(&key.to_bytes())?;
            file.sync_all()?;
            Ok(key)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let bytes = std::fs::read(path)?;
            let bytes: [u8; 32] = bytes
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid iroh secret key length"))?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(error) => Err(error).with_context(|| format!("could not write {}", path.display())),
    }
}

fn parse_endpoint_id(value: &str) -> Result<EndpointId> {
    value
        .parse()
        .with_context(|| format!("invalid iroh endpoint id `{value}`"))
}

fn parse_direct_addresses(values: &[String]) -> Result<Vec<TransportAddr>> {
    values
        .iter()
        .map(|value| {
            let addr: SocketAddr = value
                .parse()
                .with_context(|| format!("invalid direct address `{value}`"))?;
            Ok(TransportAddr::Ip(addr))
        })
        .collect()
}

pub fn device_endpoint_addr(device: &Device) -> Result<EndpointAddr> {
    let id = parse_endpoint_id(&device.iroh_endpoint_id)?;
    let addrs = parse_direct_addresses(&device.direct_addresses)?;
    let mut relays = Vec::new();
    for value in device
        .relay_urls
        .iter()
        .chain(std::iter::once(&device.relay_url))
    {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        let relay = value
            .parse::<RelayUrl>()
            .with_context(|| format!("invalid relay URL `{value}`"))?;
        if !relays.contains(&relay) {
            relays.push(relay);
        }
    }
    if addrs.is_empty() && relays.is_empty() {
        bail!("device has no direct or relay address");
    }
    let mut endpoint = EndpointAddr::from_parts(id, addrs);
    for relay in relays {
        endpoint = endpoint.with_relay_url(relay);
    }
    Ok(endpoint)
}

pub fn ticket_endpoint_addr(payload: &TicketPayload) -> Result<EndpointAddr> {
    let device = Device {
        id: payload.target_device_id.clone(),
        name: String::new(),
        kind: String::new(),
        iroh_endpoint_id: payload.target_endpoint_id.clone(),
        web_endpoint_id: String::new(),
        ssh_user: payload.target_ssh_user.clone(),
        ssh_port: payload.target_ssh_port,
        ssh_public_key: String::new(),
        ssh_host_key_fingerprint: payload.target_host_fingerprint.clone(),
        direct_addresses: payload.target_addresses.clone(),
        relay_url: payload.target_relay_url.clone(),
        relay_urls: payload.target_relay_urls.clone(),
        peer_port: 9477,
        last_seen_at: String::new(),
        created_at: String::new(),
    };
    device_endpoint_addr(&device)
}

fn collect_signing_keys(config: &AccountConfig) -> Vec<SigningPublicKey> {
    let mut keys = Vec::new();
    if !config.signing_public_key.trim().is_empty() {
        keys.push(SigningPublicKey {
            kid: config.signing_key_id.clone(),
            public_key: config.signing_public_key.clone(),
        });
    }
    for key in &config.signing_public_keys {
        if !key.public_key.trim().is_empty() {
            keys.push(key.clone());
        }
    }
    keys.sort_by(|left, right| left.kid.cmp(&right.kid));
    keys.dedup_by(|left, right| left.kid == right.kid && left.public_key == right.public_key);
    keys
}

pub fn verify_ticket(
    ticket: &str,
    public_key: &str,
    expected_target_device: &str,
    expected_target_endpoint: Option<&str>,
) -> Result<TicketPayload> {
    verify_ticket_with_keys(
        ticket,
        &[SigningPublicKey {
            kid: String::new(),
            public_key: public_key.to_string(),
        }],
        expected_target_device,
        expected_target_endpoint,
    )
}

/// Verify a ticket against a trusted key ring. The ticket's `kid` selects the
/// candidate key; a ticket without `kid` is tried against every key. The key ID
/// is not trusted until a signature verifies, so a forged `kid` cannot bypass
/// verification.
pub fn verify_ticket_with_keys(
    ticket: &str,
    keys: &[SigningPublicKey],
    expected_target_device: &str,
    expected_target_endpoint: Option<&str>,
) -> Result<TicketPayload> {
    let (encoded, signature_text) = ticket
        .split_once('.')
        .context("ticket must contain a payload and signature")?;
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature_text.trim_end_matches('='))
        .context("invalid ticket signature encoding")?;
    let signature = Signature::from_slice(&signature_bytes).context("invalid ticket signature")?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(encoded.trim_end_matches('='))
        .context("invalid ticket payload encoding")?;
    let unverified: serde_json::Value =
        serde_json::from_slice(&payload_bytes).context("invalid ticket payload")?;
    let kid = unverified
        .get("kid")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let candidates = keys
        .iter()
        .filter(|key| kid.is_empty() || key.kid.is_empty() || key.kid == kid)
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        bail!("ticket was signed with an unknown key");
    }
    let mut verified = None;
    for candidate in candidates {
        let key_bytes = URL_SAFE_NO_PAD
            .decode(candidate.public_key.trim_end_matches('='))
            .context("invalid ticket public key")?;
        let key_bytes: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| anyhow::anyhow!("ticket public key must be 32 bytes"))?;
        let key = VerifyingKey::from_bytes(&key_bytes).context("invalid ticket verifying key")?;
        if key.verify_strict(encoded.as_bytes(), &signature).is_ok() {
            verified = Some(());
            break;
        }
    }
    if verified.is_none() {
        bail!("ticket signature verification failed");
    }
    let payload: TicketPayload =
        serde_json::from_slice(&payload_bytes).context("invalid ticket payload")?;
    if payload.target_device_id != expected_target_device {
        bail!("ticket is for a different target device");
    }
    if let Some(expected) = expected_target_endpoint
        && payload.target_endpoint_id != expected
    {
        bail!("ticket target endpoint does not match this bridge");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if payload.exp <= now {
        bail!("ticket has expired");
    }
    Ok(payload)
}

async fn write_frame<W: AsyncWriteExt + Unpin>(writer: &mut W, value: &[u8]) -> Result<()> {
    if value.len() > MAX_TICKET_BYTES {
        bail!("ticket is too large");
    }
    writer.write_u32(value.len() as u32).await?;
    writer.write_all(value).await?;
    Ok(())
}

async fn read_frame<R: AsyncReadExt + Unpin>(reader: &mut R) -> Result<Vec<u8>> {
    let length = reader.read_u32().await? as usize;
    if length == 0 || length > MAX_TICKET_BYTES {
        bail!("invalid ticket frame length");
    }
    let mut value = vec![0; length];
    reader.read_exact(&mut value).await?;
    Ok(value)
}

async fn wait_for_direct(connection: &Connection, timeout: Duration) -> bool {
    let mut paths = connection.paths_stream();
    let wait = async {
        while let Some(list) = paths.next().await {
            if list.iter().any(|path| path.is_selected() && path.is_ip()) {
                return true;
            }
        }
        false
    };
    tokio::time::timeout(timeout, wait).await.unwrap_or(false)
}

#[allow(clippy::too_many_arguments)]
async fn copy_ssh_bridge(
    connection: Connection,
    ticket_keys: Arc<RwLock<Vec<SigningPublicKey>>>,
    device_id: String,
    endpoint_id: String,
    target_port: u16,
    require_direct: bool,
    guard: Option<LinkGuard>,
    seen_nonces: Arc<Mutex<HashMap<String, u64>>>,
    peer_secret: String,
) -> Result<()> {
    if require_direct && !wait_for_direct(&connection, SSH_DIRECT_WAIT).await {
        bail!("no direct path was established; relayed SSH is disabled");
    }
    let bridge = copy_ssh_bridge_body(
        connection.clone(),
        ticket_keys,
        device_id,
        endpoint_id,
        target_port,
        guard,
        seen_nonces,
        peer_secret,
    );
    if require_direct {
        tokio::select! {
            result = bridge => result,
            result = watch_direct_path(connection) => result,
        }
    } else {
        bridge.await
    }
}

/// Close an SSH connection if iroh selects a relay path after the direct path
/// was established. SSH must never fall back to a relay mid-stream.
async fn watch_direct_path(connection: Connection) -> Result<()> {
    let mut paths = connection.paths_stream();
    while let Some(list) = paths.next().await {
        if !list.iter().any(|path| path.is_selected() && path.is_ip()) {
            connection.close(0u32.into(), b"relayed SSH is disabled");
            bail!("relay path selected; relayed SSH is disabled");
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn copy_ssh_bridge_body(
    connection: Connection,
    ticket_keys: Arc<RwLock<Vec<SigningPublicKey>>>,
    device_id: String,
    endpoint_id: String,
    target_port: u16,
    guard: Option<LinkGuard>,
    seen_nonces: Arc<Mutex<HashMap<String, u64>>>,
    peer_secret: String,
) -> Result<()> {
    let remote_id = connection.remote_id().to_string();
    let (mut send, mut recv) = connection.accept_bi().await?;
    let ticket_bytes = read_frame(&mut recv).await?;
    let ticket_text = String::from_utf8(ticket_bytes).context("ticket is not UTF-8")?;
    let keys = ticket_keys.read().expect("ticket key lock").clone();
    let payload =
        verify_ticket_with_keys(&ticket_text, &keys, &device_id, Some(endpoint_id.as_str()))?;
    if payload.source_endpoint_id != remote_id {
        bail!("ticket source endpoint does not match the connected device");
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    {
        let mut seen = seen_nonces.lock().expect("ticket nonce lock");
        seen.retain(|_, expires| *expires > now);
        if seen.insert(payload.nonce.clone(), payload.exp).is_some() {
            bail!("ticket has already been used");
        }
    }
    if let Some(guard) = guard
        && !guard.allow(&payload.source_device_id).await?
    {
        bail!("source or target device has been revoked");
    }
    let mut local = TcpStream::connect(("127.0.0.1", target_port))
        .await
        .context("could not reach the local service")?;
    if connection.alpn() == PEER_ALPN {
        let initial =
            match tokio::time::timeout(Duration::from_secs(10), read_peer_headers(&mut recv)).await
            {
                Ok(result) => result?,
                Err(_) => bail!("peer request headers timed out"),
            };
        let initial = inject_peer_headers(initial, &peer_secret, &payload.source_device_id)?;
        local
            .write_all(&initial)
            .await
            .context("could not forward the peer request headers")?;
    }
    let (mut local_read, mut local_write) = tokio::io::split(&mut local);
    let client_to_local = tokio::io::copy(&mut recv, &mut local_write);
    let local_to_client = tokio::io::copy(&mut local_read, &mut send);
    let _ = tokio::join!(client_to_local, local_to_client);
    Ok(())
}

#[derive(Clone, Debug)]
pub struct LinkGuard {
    account: AccountClient,
    token: String,
    device_id: String,
}

impl LinkGuard {
    pub fn new(account: AccountClient, token: String, device_id: String) -> Self {
        Self {
            account,
            token,
            device_id,
        }
    }

    async fn allow(&self, source_device_id: &str) -> Result<bool> {
        let devices = self.account.list_devices(&self.token).await?;
        Ok(devices.iter().any(|device| device.id == source_device_id)
            && devices.iter().any(|device| device.id == self.device_id))
    }
}

#[derive(Debug)]
struct SshBridge {
    ticket_keys: Arc<RwLock<Vec<SigningPublicKey>>>,
    device_id: String,
    endpoint_id: String,
    target_port: u16,
    require_direct: bool,
    guard: Option<LinkGuard>,
    seen_nonces: Arc<Mutex<HashMap<String, u64>>>,
    peer_secret: String,
}

impl ProtocolHandler for SshBridge {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        // The ticket is read from the first bidirectional stream before the
        // local SSH server is opened. No SSH bytes are accepted before then.
        if let Err(error) = copy_ssh_bridge(
            connection,
            Arc::clone(&self.ticket_keys),
            self.device_id.clone(),
            self.endpoint_id.clone(),
            self.target_port,
            self.require_direct,
            self.guard.clone(),
            Arc::clone(&self.seen_nonces),
            self.peer_secret.clone(),
        )
        .await
        {
            tracing::warn!(%error, "iroh SSH bridge connection rejected");
        }
        Ok(())
    }
}

pub struct LinkNode {
    pub endpoint: Endpoint,
    pub router: Router,
}

impl LinkNode {
    /// Bind a managed Link endpoint. SSH bytes are only opened on a direct path
    /// once the endpoint is connected; managed relay credentials may assist
    /// path setup.
    pub async fn bind(secret: SecretKey) -> Result<Endpoint> {
        Self::bind_with_port(secret, 0).await
    }

    /// Bind a managed Link endpoint to a fixed local UDP port.
    ///
    /// A fixed port is for hubs that accept direct-only connections from other
    /// devices. `0` asks the OS for an ephemeral port. The endpoint still only
    /// carries authenticated Link traffic; the port is not a public service.
    pub async fn bind_with_port(secret: SecretKey, port: u16) -> Result<Endpoint> {
        let endpoint = endpoint_builder(secret, port)?
            .alpns(vec![SSH_ALPN.to_vec(), PEER_ALPN.to_vec()])
            .bind()
            .await?;
        wait_for_relay(&endpoint, "Link").await;
        Ok(endpoint)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn start(
        endpoint: Endpoint,
        ticket_public_key: String,
        device_id: String,
        ssh_port: u16,
        peer_port: Option<u16>,
        require_direct: bool,
        guard: Option<LinkGuard>,
        peer_secret: String,
    ) -> Result<Self> {
        Self::start_with_keys(
            endpoint,
            Arc::new(RwLock::new(vec![SigningPublicKey {
                kid: String::new(),
                public_key: ticket_public_key,
            }])),
            device_id,
            ssh_port,
            peer_port,
            require_direct,
            guard,
            peer_secret,
        )
    }

    /// Start with a shared ticket key ring. A refresh task can replace the ring
    /// contents to pick up a rotated signing key without restarting Link.
    #[allow(clippy::too_many_arguments)]
    pub fn start_with_keys(
        endpoint: Endpoint,
        ticket_keys: Arc<RwLock<Vec<SigningPublicKey>>>,
        device_id: String,
        ssh_port: u16,
        peer_port: Option<u16>,
        require_direct: bool,
        guard: Option<LinkGuard>,
        peer_secret: String,
    ) -> Result<Self> {
        let endpoint_id = endpoint.id().to_string();
        let handler = SshBridge {
            ticket_keys: Arc::clone(&ticket_keys),
            device_id: device_id.clone(),
            endpoint_id: endpoint_id.clone(),
            target_port: ssh_port,
            require_direct,
            guard: guard.clone(),
            seen_nonces: Arc::new(Mutex::new(HashMap::new())),
            peer_secret: String::new(),
        };
        let mut builder = Router::builder(endpoint.clone()).accept(SSH_ALPN, handler);
        if let Some(peer_port) = peer_port {
            // Machine API traffic may use the relay when the target is behind
            // NAT; SSH remains direct-only through the handler above.
            let peer_handler = SshBridge {
                ticket_keys,
                device_id,
                endpoint_id,
                target_port: peer_port,
                require_direct: false,
                guard,
                seen_nonces: Arc::new(Mutex::new(HashMap::new())),
                peer_secret,
            };
            builder = builder.accept(PEER_ALPN, peer_handler);
        }
        let router = builder.spawn();
        Ok(Self { endpoint, router })
    }

    pub fn id(&self) -> String {
        self.endpoint.id().to_string()
    }

    pub fn direct_addresses(&self) -> Vec<String> {
        self.endpoint
            .addr()
            .ip_addrs()
            .map(|addr| addr.to_string())
            .collect()
    }

    pub fn relay_urls(&self) -> Vec<String> {
        endpoint_relay_urls(&self.endpoint)
    }

    pub fn relay_url(&self) -> Option<String> {
        self.relay_urls().into_iter().next()
    }

    pub async fn shutdown(self) -> Result<()> {
        self.router.shutdown().await?;
        self.endpoint.close().await;
        Ok(())
    }
}

pub struct LocalForward {
    pub endpoint_id: String,
    pub local_addr: SocketAddr,
    task: JoinHandle<()>,
    last_error: Arc<Mutex<Option<String>>>,
}

impl LocalForward {
    pub async fn stop(self) {
        self.task.abort();
    }

    /// The most recent failure from the iroh side of the forward.
    ///
    /// A caller that only sees its loopback socket reset cannot tell why the
    /// forward closed. This preserves the underlying error across that reset
    /// so mobile clients can report it instead of guessing.
    pub fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().and_then(|slot| slot.clone())
    }
}

impl Drop for LocalForward {
    fn drop(&mut self) {
        // A failed mobile/CLI start may never call stop(). Detaching the
        // listener task would leave its iroh endpoint and local port alive and
        // can poison later attempts with the same identity.
        self.task.abort();
    }
}

pub async fn start_local_forward(
    secret: SecretKey,
    target: EndpointAddr,
    ticket: String,
    local_port: u16,
    alpn: Vec<u8>,
    tickets: Option<TicketSource>,
) -> Result<LocalForward> {
    let endpoint = endpoint_builder(secret, 0)?.bind().await?;
    wait_for_relay(&endpoint, "Link forward").await;
    start_local_forward_with_endpoint(endpoint, target, ticket, local_port, alpn, tickets, None)
        .await
}

/// Ask the target device to sign this source device's registered key with the
/// target's locally held SSH CA.
///
/// The request travels over the peer ALPN with a ticket-minted peer token. The
/// target resolves the source device from that token and signs only the key the
/// account registered for it; there is no control-plane signing authority.
pub async fn request_ssh_certificate(
    endpoint: Endpoint,
    target: EndpointAddr,
    ticket: String,
    tickets: Option<TicketSource>,
) -> Result<SshCredentials> {
    let capability = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    let forward = start_local_forward_with_endpoint(
        endpoint,
        target,
        ticket,
        0,
        PEER_ALPN.to_vec(),
        tickets,
        Some(capability.clone()),
    )
    .await?;
    let url = format!("http://{}/api/v1/peer/ssh/cert", forward.local_addr);
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()?;
    let response = client
        .post(url)
        .header("x-orqella-peer-token", capability)
        .json(&serde_json::json!({}))
        .send()
        .await;
    let result = match response {
        Ok(response) => {
            let status = response.status();
            if status.is_success() {
                response.json::<SshCredentials>().await.map_err(Into::into)
            } else {
                let detail = response.text().await.unwrap_or_default();
                Err(anyhow::anyhow!(
                    "target refused the SSH certificate: HTTP {status} {detail}"
                ))
            }
        }
        Err(error) => Err(error.into()),
    };
    forward.stop().await;
    result
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

/// Read the first HTTP request header block from a peer bridge client.
async fn read_peer_headers(recv: &mut RecvStream) -> Result<Vec<u8>> {
    const MAX_HEADERS: usize = 64 * 1024;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if buffer.len() > MAX_HEADERS {
            bail!("peer request headers are too large");
        }
        let Some(read) = recv.read(&mut chunk).await? else {
            bail!("peer connection closed before sending request headers");
        };
        if read == 0 {
            bail!("peer connection closed before sending request headers");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(buffer);
        }
    }
}

/// The peer bridge authenticates only the first HTTP request on a forwarded
/// connection. Keep-alive would send later requests without target credentials,
/// so ordinary requests must not reuse the connection. WebSocket upgrades need
/// their Connection header intact.
fn force_connection_close(buffer: Vec<u8>) -> Vec<u8> {
    let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
        return buffer;
    };
    let headers = &buffer[..end];
    let upgrade = std::str::from_utf8(headers)
        .unwrap_or_default()
        .lines()
        .skip(1)
        .any(|line| {
            line.split_once(':').is_some_and(|(name, value)| {
                name.eq_ignore_ascii_case("connection")
                    && value
                        .split(',')
                        .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
        });
    if upgrade {
        return buffer;
    }
    let mut output = Vec::with_capacity(buffer.len() + 32);
    let mut request_line = true;
    for line in headers.split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if request_line {
            request_line = false;
            output.extend_from_slice(line);
            output.extend_from_slice(b"\r\n");
            continue;
        }
        if line.is_empty() {
            continue;
        }
        if line.to_ascii_lowercase().starts_with(b"connection:") {
            continue;
        }
        output.extend_from_slice(line);
        output.extend_from_slice(b"\r\n");
    }
    output.extend_from_slice(b"Connection: close\r\n\r\n");
    output.extend_from_slice(&buffer[end + 4..]);
    output
}

/// Replace the source-controlled peer capability with the target's local
/// secret and the source device identified by the signed ticket.
///
/// This runs in the target's iroh bridge, so neither the control plane nor the
/// source device ever holds the peer API credential.
fn inject_peer_headers(
    buffer: Vec<u8>,
    peer_secret: &str,
    source_device_id: &str,
) -> Result<Vec<u8>> {
    if peer_secret.is_empty()
        || peer_secret.len() > 256
        || source_device_id.len() > 256
        || peer_secret.chars().any(char::is_control)
        || source_device_id.chars().any(char::is_control)
    {
        bail!("target peer credential contains invalid characters");
    }
    let buffer = force_connection_close(buffer);
    let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") else {
        bail!("peer request has no header terminator");
    };
    const TOKEN_HEADER: &str = "x-orqella-peer-token";
    const SOURCE_HEADER: &str = "x-orqella-source-device";
    let mut output = Vec::with_capacity(buffer.len() + peer_secret.len() + 128);
    let mut request_line = true;
    for line in buffer[..end].split(|byte| *byte == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        if request_line {
            request_line = false;
            output.extend_from_slice(line);
            output.extend_from_slice(b"\r\n");
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if lower.starts_with(TOKEN_HEADER.as_bytes()) || lower.starts_with(SOURCE_HEADER.as_bytes())
        {
            continue;
        }
        output.extend_from_slice(line);
        output.extend_from_slice(b"\r\n");
    }
    output.extend_from_slice(format!("X-Orqella-Peer-Token: {peer_secret}\r\n").as_bytes());
    output.extend_from_slice(
        format!("X-Orqella-Source-Device: {source_device_id}\r\n\r\n").as_bytes(),
    );
    output.extend_from_slice(&buffer[end + 4..]);
    Ok(output)
}

/// Read the first request headers from a local forward client and verify the
/// per-forward capability. This only authorizes use of the loopback listener;
/// the target's iroh bridge replaces it with the target's peer secret before
/// proxying the request.
async fn forward_auth(local: &mut TcpStream, expected: &str) -> Result<Vec<u8>> {
    const MAX_HEADERS: usize = 64 * 1024;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if buffer.len() > MAX_HEADERS {
            bail!("local forward request headers are too large");
        }
        let read = local.read(&mut chunk).await?;
        if read == 0 {
            bail!("local forward client closed before sending a request");
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    let headers = String::from_utf8_lossy(&buffer);
    let supplied = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.trim().eq_ignore_ascii_case("x-orqella-peer-token") {
                Some(value.trim())
            } else {
                None
            }
        })
        .unwrap_or_default();
    if !constant_time_eq(supplied.as_bytes(), expected.as_bytes()) {
        bail!("local forward request is missing the peer token");
    }
    Ok(force_connection_close(buffer))
}

/// Serve a loopback forward over an existing endpoint. Link peer forwards must
/// share the primary endpoint: iroh relays allow only one active connection per
/// endpoint identity.
#[allow(clippy::too_many_arguments)]
pub async fn start_local_forward_with_endpoint(
    endpoint: Endpoint,
    target: EndpointAddr,
    ticket: String,
    local_port: u16,
    alpn: Vec<u8>,
    tickets: Option<TicketSource>,
    local_token: Option<String>,
) -> Result<LocalForward> {
    let endpoint_id = endpoint.id().to_string();
    let listener = TcpListener::bind(("127.0.0.1", local_port))
        .await
        .context("could not bind the local forward listener")?;
    let local_addr = listener.local_addr()?;
    let last_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let task_last_error = Arc::clone(&last_error);
    let task = tokio::spawn(async move {
        loop {
            let Ok((mut local, _)) = listener.accept().await else {
                break;
            };
            let endpoint = endpoint.clone();
            let target = target.clone();
            let ticket = ticket.clone();
            let tickets = tickets.clone();
            let alpn = alpn.clone();
            let local_token = local_token.clone();
            let task_last_error = Arc::clone(&task_last_error);
            tokio::spawn(async move {
                let result = async {
                    let initial = match &local_token {
                        Some(expected) => match forward_auth(&mut local, expected).await {
                            Ok(buffer) => buffer,
                            Err(error) => {
                                let _ = local
                                    .write_all(
                                        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                                    )
                                    .await;
                                return Err(error);
                            }
                        },
                        None => Vec::new(),
                    };
                    let refreshed = match &tickets {
                        Some(source) => Some(source.ticket_response().await?),
                        None => None,
                    };
                    let (ticket, target) = match refreshed {
                        Some(response) => {
                            let target = match device_endpoint_addr(&response.target) {
                                Ok(target) => target,
                                Err(error) => {
                                    tracing::debug!(
                                        %error,
                                        "Link ticket target address was invalid; using the last known address"
                                    );
                                    target
                                }
                            };
                            (response.ticket, target)
                        }
                        None => (ticket, target),
                    };
                    let connection = endpoint.connect(target, &alpn).await?;
                    if alpn.as_slice() == SSH_ALPN
                        && !wait_for_direct(&connection, SSH_DIRECT_WAIT).await
                    {
                        anyhow::bail!("no direct path was established; relayed SSH is disabled");
                    }
                    let (mut send, mut recv) = connection.open_bi().await?;
                    write_frame(&mut send, ticket.as_bytes()).await?;
                    if !initial.is_empty() {
                        send.write_all(&initial).await?;
                    }
                    let (mut local_read, mut local_write) = tokio::io::split(&mut local);
                    let copy = async {
                        let client_to_target = tokio::io::copy(&mut local_read, &mut send);
                        let target_to_client = tokio::io::copy(&mut recv, &mut local_write);
                        let _ = tokio::join!(client_to_target, target_to_client);
                    };
                    if alpn.as_slice() == SSH_ALPN {
                        tokio::select! {
                            _ = copy => Ok(()),
                            result = watch_direct_path(connection) => result,
                        }
                    } else {
                        copy.await;
                        Ok::<(), anyhow::Error>(())
                    }
                }
                .await;
                if let Err(error) = result {
                    if let Ok(mut slot) = task_last_error.lock() {
                        *slot = Some(format!("{error:#}"));
                    }
                    tracing::debug!(%error, "local iroh SSH forward closed");
                }
            });
        }
    });
    Ok(LocalForward {
        endpoint_id,
        local_addr,
        task,
        last_error,
    })
}

pub fn default_secret_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("orqella/link/iroh.key")
}

/// Best-effort local SSH host-key fingerprint detection.
///
/// The account ticket carries this value so clients can pin the host key. A
/// caller may override it with an explicit configured fingerprint.
pub fn detect_ssh_host_fingerprint() -> Option<String> {
    let candidates = [
        "/etc/ssh/ssh_host_ed25519_key.pub",
        "/etc/ssh/ssh_host_rsa_key.pub",
        "/etc/ssh/ssh_host_ecdsa_key.pub",
    ];
    for path in candidates {
        if !Path::new(path).is_file() {
            continue;
        }
        let output = std::process::Command::new("ssh-keygen")
            .args(["-lf", path])
            .output()
            .ok()?;
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        if let Some(fingerprint) = text.split_whitespace().nth(1) {
            return Some(fingerprint.trim().to_string());
        }
    }
    None
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkPeer {
    pub id: String,
    pub name: String,
    pub url: String,
    pub token: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LinkStatus {
    pub enabled: bool,
    pub device_id: String,
    pub device_name: String,
    pub endpoint_id: String,
    pub direct_addresses: Vec<String>,
    pub ssh_port: u16,
    pub ssh_user: String,
    pub ssh_host_key_fingerprint: String,
    pub relay_active: bool,
    pub ssh_ca_configured: bool,
    pub link_entitled: bool,
    pub link_plan: String,
    pub trial_end: Option<String>,
    pub billing_url: String,
}

pub struct LinkManager {
    account: AccountClient,
    token: String,
    device: Device,
    endpoint_id: String,
    endpoint: Endpoint,
    direct_addresses: Vec<String>,
    relay_urls: Arc<Mutex<Vec<String>>>,
    entitlement: Arc<RwLock<Entitlement>>,
    node: Mutex<Option<LinkNode>>,
    peers: Arc<Mutex<Vec<LinkPeer>>>,
    forwards: Arc<Mutex<Vec<LocalForward>>>,
    heartbeat: JoinHandle<()>,
    relay_refresh: JoinHandle<()>,
    token_refresh: JoinHandle<()>,
}

fn is_hub_kind(kind: &str) -> bool {
    !matches!(kind, "mobile" | "browser" | "desktop-client")
}

impl LinkManager {
    #[allow(clippy::too_many_arguments)]
    pub async fn start(
        mut token: String,
        device_id: Option<String>,
        device_name: String,
        ssh_user: String,
        ssh_port: u16,
        ssh_host_fingerprint: String,
        peer_secret: String,
        peer_port: u16,
        web_endpoint_id: String,
        link_port: u16,
        secret_path: &Path,
    ) -> Result<Self> {
        let account = AccountClient::production()?;
        let config = account.config().await?;
        let ticket_keys = Arc::new(RwLock::new(collect_signing_keys(&config)));
        let secret = load_or_create_secret(secret_path)?;
        let endpoint = LinkNode::bind_with_port(secret, link_port).await?;
        let forward_endpoint = endpoint.clone();
        let mut advertised_urls = advertised_relay_urls(&endpoint);
        let relay_url = advertised_urls.first().cloned().unwrap_or_default();
        let ssh_host_fingerprint = if ssh_host_fingerprint.trim().is_empty() {
            detect_ssh_host_fingerprint().unwrap_or_default()
        } else {
            ssh_host_fingerprint
        };
        let endpoint_id = endpoint.id().to_string();
        let addresses = advertised_direct_addresses(&endpoint);
        let _ = device_id;
        // Upsert by endpoint ID on every start. The SSH port and host
        // fingerprint must be refreshed after the embedded server binds.
        let device = account
            .add_device(
                &token,
                &RegisterDevice {
                    name: device_name,
                    kind: "desktop".into(),
                    iroh_endpoint_id: endpoint_id.clone(),
                    web_endpoint_id,
                    ssh_user,
                    ssh_port,
                    ssh_public_key: String::new(),
                    ssh_host_key_fingerprint: ssh_host_fingerprint,
                    direct_addresses: addresses.clone(),
                    relay_url: relay_url.clone(),
                    relay_urls: advertised_urls.clone(),
                    peer_port,
                },
            )
            .await?;
        let device_id = device.id.clone();
        // Password/passkey login starts with a user session. Exchange it for a
        // token scoped to this device; enrollment tokens are already bound by
        // the device registration call.
        match account.device_token(&token, &device_id).await {
            Ok(response) => {
                if let Some(device_token) = response.token {
                    token = device_token;
                }
            }
            Err(error) => {
                tracing::warn!(%error, "could not exchange Link device token");
            }
        }
        if let Err(error) = account.refresh_device_token(&token).await {
            tracing::warn!(%error, "could not refresh Link device token");
        }
        let entitlement = Arc::new(RwLock::new(
            account.entitlement(&token).await.unwrap_or_default(),
        ));
        let mut relay_ttl_seconds = 300;
        match account.relay_credentials(&token, &endpoint_id).await {
            Ok(credentials) => {
                relay_ttl_seconds = credentials.ttl_seconds.max(60);
                match install_brokered_relays(
                    &endpoint,
                    &credentials.relay_urls,
                    &credentials.token,
                )
                .await
                {
                    Ok(urls) => {
                        advertised_urls = urls;
                        if tokio::time::timeout(Duration::from_secs(10), endpoint.online())
                            .await
                            .is_err()
                        {
                            tracing::warn!("managed Link relay did not become online within 10s");
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not install managed relay credentials");
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "managed relay credentials unavailable; using direct paths");
            }
        }
        let relay_urls = Arc::new(Mutex::new(advertised_urls.clone()));
        let endpoint_for_heartbeat = endpoint_id.clone();
        let addresses_for_heartbeat = addresses.clone();
        let account_for_heartbeat = account.clone();
        let token_for_heartbeat = token.clone();
        let heartbeat_device = device_id.clone();
        let heartbeat_addresses = addresses_for_heartbeat.clone();
        let heartbeat_relays = relay_urls.clone();
        let heartbeat_keys = Arc::clone(&ticket_keys);
        let heartbeat_entitlement = Arc::clone(&entitlement);
        let heartbeat = tokio::spawn(async move {
            let mut last_key_refresh = std::time::Instant::now();
            loop {
                if last_key_refresh.elapsed() >= Duration::from_secs(60) {
                    if let Ok(refreshed) = account_for_heartbeat.config().await {
                        let keys = collect_signing_keys(&refreshed);
                        if !keys.is_empty() {
                            *heartbeat_keys.write().expect("ticket key lock") = keys;
                        }
                    }
                    last_key_refresh = std::time::Instant::now();
                }
                let relays = heartbeat_relays.lock().expect("link relay lock").clone();
                let relay = relays.first().cloned().unwrap_or_default();
                match account_for_heartbeat
                    .heartbeat(
                        &token_for_heartbeat,
                        &heartbeat_device,
                        &endpoint_for_heartbeat,
                        &heartbeat_addresses,
                        &relay,
                        &relays,
                    )
                    .await
                {
                    Ok(response) => {
                        if let Some(updated) = response.entitlement {
                            let mut current = heartbeat_entitlement
                                .write()
                                .expect("link entitlement lock");
                            if *current != updated {
                                tracing::info!(
                                    entitled = updated.link_entitled,
                                    plan = %updated.link_plan,
                                    trial_end = ?updated.trial_end,
                                    "Link entitlement updated"
                                );
                                *current = updated;
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not publish Link heartbeat");
                    }
                }
                tokio::time::sleep(Duration::from_secs(20)).await;
            }
        });
        let relay_refresh = {
            let account = account.clone();
            let token = token.clone();
            let endpoint = endpoint.clone();
            let endpoint_id = endpoint_id.clone();
            let relay_urls = relay_urls.clone();
            let interval = Duration::from_secs((relay_ttl_seconds / 2).max(15));
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(interval).await;
                    match account.relay_credentials(&token, &endpoint_id).await {
                        Ok(credentials) => {
                            match install_brokered_relays(
                                &endpoint,
                                &credentials.relay_urls,
                                &credentials.token,
                            )
                            .await
                            {
                                Ok(urls) => {
                                    *relay_urls.lock().expect("link relay lock") = urls;
                                    tracing::info!("refreshed managed relay credentials");
                                }
                                Err(error) => {
                                    tracing::warn!(%error, "could not refresh managed relay credentials");
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(%error, "could not refresh managed relay credentials");
                        }
                    }
                }
            })
        };
        let token_refresh = {
            let account = account.clone();
            let token = token.clone();
            tokio::spawn(async move {
                loop {
                    // TTL is 90 days by default; refresh well before it expires.
                    tokio::time::sleep(Duration::from_secs(30 * 24 * 3600)).await;
                    if let Err(error) = account.refresh_device_token(&token).await {
                        tracing::warn!(%error, "could not refresh Link device token");
                    }
                }
            })
        };
        let guard = LinkGuard::new(account.clone(), token.clone(), device_id.clone());
        let manager_endpoint = endpoint.clone();
        let node = LinkNode::start_with_keys(
            endpoint,
            ticket_keys,
            device_id.clone(),
            ssh_port,
            Some(peer_port),
            true,
            Some(guard),
            peer_secret,
        )?;
        let manager = Self {
            account,
            token,
            device: Device {
                direct_addresses: addresses.clone(),
                ..device
            },
            endpoint_id,
            endpoint: manager_endpoint,
            direct_addresses: addresses_for_heartbeat,
            relay_urls,
            entitlement,
            node: Mutex::new(Some(node)),
            peers: Arc::new(Mutex::new(Vec::new())),
            forwards: Arc::new(Mutex::new(Vec::new())),
            heartbeat,
            relay_refresh,
            token_refresh,
        };
        manager.start_peer_forwards(forward_endpoint).await;
        Ok(manager)
    }

    pub fn status(&self) -> LinkStatus {
        let relay_urls = self.relay_urls.lock().expect("link relay lock").clone();
        let entitlement = self
            .entitlement
            .read()
            .expect("link entitlement lock")
            .clone();
        LinkStatus {
            enabled: true,
            device_id: self.device.id.clone(),
            device_name: self.device.name.clone(),
            endpoint_id: self.endpoint_id.clone(),
            direct_addresses: self.direct_addresses.clone(),
            ssh_port: self.device.ssh_port,
            ssh_user: self.device.ssh_user.clone(),
            ssh_host_key_fingerprint: self.device.ssh_host_key_fingerprint.clone(),
            relay_active: !relay_urls.is_empty(),
            // The target holds its own SSH CA, so certificates are always
            // available once the embedded Link SSH server has started.
            ssh_ca_configured: true,
            link_entitled: entitlement.link_entitled,
            link_plan: entitlement.link_plan,
            trial_end: entitlement.trial_end,
            billing_url: entitlement.billing_url,
        }
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    /// Mint managed relay credentials for a paired browser endpoint.
    pub async fn web_relay_credentials(
        &self,
        endpoint_id: &str,
        session_token: Option<&str>,
    ) -> Result<RelayCredentials> {
        self.account
            .web_relay_credentials(&self.token, endpoint_id, session_token)
            .await
    }

    /// Check whether a browser Link session owns a device with this web endpoint.
    pub async fn web_session_authorized(&self, token: &str, endpoint_id: &str) -> Result<bool> {
        self.account
            .authorize_web_session(&self.token, token, endpoint_id)
            .await
    }

    pub async fn devices(&self) -> Result<Vec<DeviceView>> {
        Ok(self
            .account
            .list_devices(&self.token)
            .await?
            .iter()
            .map(DeviceView::from)
            .collect())
    }

    /// Return the SSH public key registered for a live source device.
    ///
    /// This deliberately bypasses [`DeviceView`] so the key is available to the
    /// target's certificate signer without serializing device key material into
    /// browser or module payloads.
    pub async fn source_ssh_public_key(&self, device_id: &str) -> Result<Option<String>> {
        Ok(self
            .account
            .list_devices(&self.token)
            .await?
            .into_iter()
            .find(|device| device.id == device_id)
            .map(|device| device.ssh_public_key)
            .filter(|key| !key.trim().is_empty()))
    }

    pub fn entitlement(&self) -> Entitlement {
        self.entitlement
            .read()
            .expect("link entitlement lock")
            .clone()
    }

    pub fn peers(&self) -> Vec<LinkPeer> {
        self.peers.lock().expect("link peers lock").clone()
    }

    /// Refresh the local peer forwards after machines are added to or removed
    /// from the account. Existing forwards are kept; new hubs are added.
    pub async fn refresh_peers(&self) {
        self.start_peer_forwards(self.endpoint.clone()).await;
    }

    pub async fn start_peer_forwards(&self, endpoint: Endpoint) {
        let devices = match self.account.list_devices(&self.token).await {
            Ok(devices) => devices,
            Err(error) => {
                tracing::warn!(%error, "could not list Link peers");
                return;
            }
        };
        for device in devices {
            if device.id == self.device.id || !is_hub_kind(&device.kind) {
                continue;
            }
            if self
                .peers
                .lock()
                .expect("link peers lock")
                .iter()
                .any(|peer| peer.id == device.id)
            {
                continue;
            }
            let ticket = match self
                .account
                .ticket(&self.token, &self.device.id, &device.id)
                .await
            {
                Ok(ticket) => ticket,
                Err(error) => {
                    tracing::warn!(%error, device = %device.id, "could not create Link peer ticket");
                    continue;
                }
            };
            // Local clients only need to prove they are allowed to use this
            // loopback forward. The target's iroh bridge injects the peer API
            // credential and source identity on every connection.
            let local_token = URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
            let target = match device_endpoint_addr(&ticket.target) {
                Ok(target) => target,
                Err(error) => {
                    tracing::warn!(%error, device = %device.id, "invalid Link peer address");
                    continue;
                }
            };
            let source = TicketSource::new(
                self.account.clone(),
                self.token.clone(),
                self.device.id.clone(),
                device.id.clone(),
            );
            match start_local_forward_with_endpoint(
                endpoint.clone(),
                target,
                ticket.ticket,
                0,
                PEER_ALPN.to_vec(),
                Some(source),
                Some(local_token.clone()),
            )
            .await
            {
                Ok(forward) => {
                    self.peers.lock().expect("link peers lock").push(LinkPeer {
                        id: device.id.clone(),
                        name: device.name.clone(),
                        url: format!("http://{}", forward.local_addr),
                        token: local_token,
                    });
                    self.forwards
                        .lock()
                        .expect("link forwards lock")
                        .push(forward);
                }
                Err(error) => {
                    tracing::warn!(%error, device = %device.id, "could not start Link peer forward");
                }
            }
        }
    }

    pub async fn shutdown(&self) {
        self.heartbeat.abort();
        self.relay_refresh.abort();
        self.token_refresh.abort();
        let forwards: Vec<_> = self
            .forwards
            .lock()
            .expect("link forwards lock")
            .drain(..)
            .collect();
        for forward in forwards {
            forward.stop().await;
        }
        let node = self.node.lock().expect("link node lock").take();
        if let Some(node) = node {
            let _ = node.shutdown().await;
        }
    }
}

impl Drop for LinkManager {
    fn drop(&mut self) {
        self.heartbeat.abort();
        self.relay_refresh.abort();
        self.token_refresh.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn ticket_payload() -> TicketPayload {
        TicketPayload {
            v: 1,
            kid: "test-key".into(),
            source_device_id: "source".into(),
            source_endpoint_id: "a".repeat(64),
            target_device_id: "target".into(),
            target_endpoint_id: "b".repeat(64),
            target_addresses: vec!["127.0.0.1:7842".into()],
            target_relay_url: String::new(),
            target_relay_urls: Vec::new(),
            target_ssh_user: "user".into(),
            target_ssh_port: 22,
            target_host_fingerprint: "SHA256:target".into(),
            exp: u64::MAX,
            nonce: "nonce".into(),
        }
    }

    #[test]
    fn heartbeat_response_decodes_a_trial_entitlement() {
        let response: HeartbeatResponse = serde_json::from_str(
            r#"{"device":{"id":"hub"},"entitlement":{"link_entitled":true,"link_plan":"link_pro","relay_enabled":true,"max_devices":0,"billing_url":"https://link.orqella.io/account/billing","trial_end":"2026-12-01T00:00:00+00:00"}}"#,
        )
        .unwrap();
        let entitlement = response.entitlement.expect("entitlement");
        assert!(entitlement.link_entitled);
        assert_eq!(entitlement.link_plan, "link_pro");
        assert_eq!(
            entitlement.trial_end.as_deref(),
            Some("2026-12-01T00:00:00+00:00")
        );
    }

    #[test]
    fn heartbeat_response_tolerates_older_servers() {
        let response: HeartbeatResponse =
            serde_json::from_str(r#"{"device":{"id":"hub"}}"#).unwrap();
        assert!(response.entitlement.is_none());
    }

    #[test]
    fn only_hub_kinds_become_peers() {
        assert!(is_hub_kind("desktop"));
        assert!(!is_hub_kind("mobile"));
        assert!(!is_hub_kind("browser"));
        assert!(!is_hub_kind("desktop-client"));
    }

    #[test]
    fn ticket_endpoint_requires_direct_address() {
        let mut payload = ticket_payload();
        payload.target_addresses.clear();
        assert!(ticket_endpoint_addr(&payload).is_err());
    }

    #[test]
    fn ticket_endpoint_parses_ipv4_and_ipv6() {
        let endpoint = SecretKey::generate().public();
        let mut payload = ticket_payload();
        payload.target_endpoint_id = endpoint.to_string();
        payload.target_addresses = vec!["127.0.0.1:7842".into(), "[::1]:7842".into()];
        let addr = ticket_endpoint_addr(&payload).unwrap();
        assert!(
            addr.addrs
                .contains(&TransportAddr::Ip("127.0.0.1:7842".parse().unwrap()))
        );
        assert!(
            addr.addrs
                .contains(&TransportAddr::Ip("[::1]:7842".parse().unwrap()))
        );
    }

    #[test]
    fn device_view_omits_key_material() {
        let device = Device {
            id: "device".into(),
            name: "device".into(),
            kind: "desktop".into(),
            iroh_endpoint_id: "a".repeat(64),
            web_endpoint_id: "b".repeat(64),
            ssh_user: "user".into(),
            ssh_port: 22,
            ssh_public_key: "ssh-ed25519 AAAA".into(),
            ssh_host_key_fingerprint: "SHA256:host".into(),
            direct_addresses: vec!["127.0.0.1:1".into()],
            relay_url: String::new(),
            relay_urls: Vec::new(),
            peer_port: 9477,
            last_seen_at: String::new(),
            created_at: String::new(),
        };
        let value = serde_json::to_value(DeviceView::from(&device)).unwrap();
        assert!(value.get("peer_token").is_none());
        assert!(value.get("ssh_public_key").is_none());
    }

    #[test]
    fn device_endpoint_addr_includes_all_relays() {
        let device = Device {
            id: "device".into(),
            name: "device".into(),
            kind: "desktop".into(),
            iroh_endpoint_id: SecretKey::generate().public().to_string(),
            web_endpoint_id: String::new(),
            ssh_user: String::new(),
            ssh_port: 22,
            ssh_public_key: String::new(),
            ssh_host_key_fingerprint: String::new(),
            direct_addresses: Vec::new(),
            relay_url: "https://one.example.com/".into(),
            relay_urls: vec![
                "https://one.example.com/".into(),
                "https://two.example.com/".into(),
            ],
            peer_port: 9477,
            last_seen_at: String::new(),
            created_at: String::new(),
        };
        let addr = device_endpoint_addr(&device).unwrap();
        let urls: Vec<String> = addr.relay_urls().map(|url| url.to_string()).collect();
        assert_eq!(urls.len(), 2);
    }

    #[test]
    fn ticket_frame_round_trips() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let (mut a, mut b) = tokio::io::duplex(1024);
            write_frame(&mut a, b"ticket-data").await.unwrap();
            assert_eq!(read_frame(&mut b).await.unwrap(), b"ticket-data");
        });
    }

    #[tokio::test]
    async fn peer_forward_auth_requires_the_token() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            forward_auth(&mut socket, "expected-token").await.is_ok()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"GET / HTTP/1.1\r\nHost: forward\r\nX-Orqella-Peer-Token: expected-token\r\n\r\n",
            )
            .await
            .unwrap();
        assert!(server.await.unwrap());

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            forward_auth(&mut socket, "expected-token").await.is_err()
        });
        let mut client = TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: forward\r\n\r\n")
            .await
            .unwrap();
        assert!(server.await.unwrap());
    }

    #[tokio::test]
    async fn link_endpoint_binds_a_fixed_udp_port() {
        let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let endpoint = LinkNode::bind_with_port(SecretKey::generate(), port)
            .await
            .unwrap();
        let sockets = endpoint.bound_sockets();
        assert!(
            sockets.iter().any(|address| address.port() == port),
            "{sockets:?}"
        );
        endpoint.close().await;
    }

    #[test]
    fn endpoint_secret_is_created_privately_and_reused() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("iroh.key");
        let first = load_or_create_secret(&path).unwrap();
        let second = load_or_create_secret(&path).unwrap();
        assert_eq!(first.public(), second.public());
        assert_eq!(std::fs::read(&path).unwrap(), first.to_bytes());
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
    fn invalid_endpoint_secret_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("iroh.key");
        std::fs::write(&path, b"short").unwrap();
        assert!(load_or_create_secret(&path).is_err());
    }

    #[test]
    fn device_endpoint_requires_a_reachable_address() {
        let mut device = Device {
            id: "device".into(),
            name: "device".into(),
            kind: "desktop".into(),
            iroh_endpoint_id: SecretKey::generate().public().to_string(),
            web_endpoint_id: String::new(),
            ssh_user: String::new(),
            ssh_port: 22,
            ssh_public_key: String::new(),
            ssh_host_key_fingerprint: String::new(),
            direct_addresses: Vec::new(),
            relay_url: String::new(),
            relay_urls: Vec::new(),
            peer_port: 9477,
            last_seen_at: String::new(),
            created_at: String::new(),
        };
        assert!(device_endpoint_addr(&device).is_err());
        device.direct_addresses = vec!["not-an-address".into()];
        assert!(device_endpoint_addr(&device).is_err());
        device.direct_addresses.clear();
        device.relay_url = "https://relay.example.com/".into();
        assert!(device_endpoint_addr(&device).is_ok());
    }

    #[test]
    fn ticket_verification_checks_signature_target_and_expiry() {
        use ed25519_dalek::{Signer, SigningKey};

        let key = SigningKey::from_bytes(&[7u8; 32]);
        let public = URL_SAFE_NO_PAD.encode(key.verifying_key().to_bytes());
        let mut payload = ticket_payload();
        payload.exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let signature = key.sign(encoded.as_bytes());
        let ticket = format!("{encoded}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        let decoded = verify_ticket(
            &ticket,
            &public,
            "target",
            Some(&payload.target_endpoint_id),
        )
        .unwrap();
        assert_eq!(decoded.target_device_id, "target");

        assert!(verify_ticket(&ticket, &public, "other", None).is_err());
        assert!(verify_ticket(&ticket, &public, "target", Some(&"c".repeat(64))).is_err());
        let tampered = format!(
            "{encoded}x.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        );
        assert!(verify_ticket(&tampered, &public, "target", None).is_err());
        assert!(verify_ticket("not-a-ticket", &public, "target", None).is_err());

        let mut expired = payload.clone();
        expired.exp = 1;
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&expired).unwrap());
        let signature = key.sign(encoded.as_bytes());
        let ticket = format!("{encoded}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        assert!(verify_ticket(&ticket, &public, "target", None).is_err());
    }

    #[test]
    fn target_bridge_replaces_client_capabilities_with_local_credentials() {
        let request = b"GET /api/v1/peer/ping HTTP/1.1\r\nHost: peer\r\nConnection: keep-alive\r\nX-Orqella-Peer-Token: local-capability\r\nX-Orqella-Source-Device: forged\r\nX-Keep: yes\r\n\r\nbody";
        let rewritten =
            inject_peer_headers(request.to_vec(), "target-secret", "source-device").unwrap();
        let text = String::from_utf8(rewritten).unwrap();
        assert!(text.contains("X-Orqella-Peer-Token: target-secret\r\n"));
        assert!(text.contains("X-Orqella-Source-Device: source-device\r\n"));
        assert!(!text.contains("local-capability"));
        assert!(!text.contains("forged"));
        assert!(!text.contains("keep-alive"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.contains("X-Keep: yes\r\n"));
        assert!(text.ends_with("\r\n\r\nbody"));
        assert_eq!(text.matches("X-Orqella-Peer-Token").count(), 1);
        assert_eq!(text.matches("Connection:").count(), 1);
    }

    #[test]
    fn target_bridge_preserves_websocket_upgrade_connections() {
        let request = b"GET /m/example/ws HTTP/1.1\r\nHost: peer\r\nConnection: keep-alive, Upgrade\r\nUpgrade: websocket\r\n\r\n";
        let rewritten =
            inject_peer_headers(request.to_vec(), "target-secret", "source-device").unwrap();
        let text = String::from_utf8(rewritten).unwrap();
        assert!(text.contains("Connection: keep-alive, Upgrade\r\n"));
        assert!(!text.contains("Connection: close"));
    }

    #[test]
    fn forwarded_requests_disable_keep_alive_except_upgrades() {
        let request = b"POST /api HTTP/1.1\r\nHost: forward\r\nConnection: keep-alive\r\n\r\nbody";
        let text = String::from_utf8(force_connection_close(request.to_vec())).unwrap();
        assert!(!text.contains("keep-alive"));
        assert!(text.contains("Connection: close\r\n"));
        assert!(text.ends_with("\r\n\r\nbody"));

        let upgrade = b"GET /ws HTTP/1.1\r\nHost: forward\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n";
        assert_eq!(force_connection_close(upgrade.to_vec()), upgrade);
    }

    #[test]
    fn peer_header_injection_fails_closed() {
        assert!(inject_peer_headers(b"not http".to_vec(), "secret", "source").is_err());
        assert!(inject_peer_headers(b"GET / HTTP/1.1\r\n\r\n".to_vec(), "", "source").is_err());
    }

    #[test]
    fn local_forward_tokens_compare_without_early_exit() {
        assert!(constant_time_eq(b"same", b"same"));
        assert!(!constant_time_eq(b"same", b"diff"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }

    #[test]
    fn ticket_key_rotation_selects_by_key_id() {
        use ed25519_dalek::{Signer, SigningKey};

        let old_key = SigningKey::from_bytes(&[3u8; 32]);
        let new_key = SigningKey::from_bytes(&[4u8; 32]);
        let old_public = URL_SAFE_NO_PAD.encode(old_key.verifying_key().to_bytes());
        let new_public = URL_SAFE_NO_PAD.encode(new_key.verifying_key().to_bytes());
        let keys = vec![
            SigningPublicKey {
                kid: "old".into(),
                public_key: old_public,
            },
            SigningPublicKey {
                kid: "new".into(),
                public_key: new_public,
            },
        ];
        let mut payload = ticket_payload();
        payload.kid = "new".into();
        payload.exp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 60;
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let signature = new_key.sign(encoded.as_bytes());
        let ticket = format!("{encoded}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        assert!(verify_ticket_with_keys(&ticket, &keys, "target", None).is_ok());

        payload.kid = "unknown".into();
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let signature = new_key.sign(encoded.as_bytes());
        let unknown = format!("{encoded}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        assert!(verify_ticket_with_keys(&unknown, &keys, "target", None).is_err());

        payload.kid = "old".into();
        let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let signature = new_key.sign(encoded.as_bytes());
        let mislabeled = format!("{encoded}.{}", URL_SAFE_NO_PAD.encode(signature.to_bytes()));
        assert!(verify_ticket_with_keys(&mislabeled, &keys, "target", None).is_err());
    }
}
