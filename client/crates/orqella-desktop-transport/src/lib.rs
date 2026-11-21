//! Native Link transport for the Orqella desktop client.
//!
//! This crate deliberately delegates account, ticket, SSH, and direct-path
//! policy to the core-owned implementation. The desktop client adds profile
//! and UI concerns; it must not grow a second copy of Link.

use anyhow::{Context, Result, bail};
use orqella_link_core::iroh::SecretKey;
use orqella_link_core::{AccountClient, Device, LinkNode, LoginOutcome, RegisterDevice};
use orqella_tunnel_core::{
    MobileLinkTunnel, MobileLinkTunnelConfig, generate_ssh_key, link_generate_secret, start_link,
};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// A device-code login challenge suitable for the local onboarding UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginChallenge {
    pub device_code: String,
    pub user_code: String,
    #[serde(default)]
    pub confirmation_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// A completed Link login.
#[derive(Clone, Serialize, Deserialize)]
pub struct LoginSession {
    pub token: String,
    pub user_id: String,
    pub email: String,
    pub entitled: bool,
}

impl std::fmt::Debug for LoginSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoginSession")
            .field("token", &"[redacted]")
            .field("user_id", &self.user_id)
            .field("email", &self.email)
            .field("entitled", &self.entitled)
            .finish()
    }
}

/// The identity material generated for this desktop installation.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct ClientIdentity {
    /// OpenSSH private key bytes. Store in the platform keychain.
    pub ssh_private_key: Vec<u8>,
    /// One-line public key, safe to display.
    pub ssh_public_key: String,
    /// 32-byte iroh identity secret. Store in the platform keychain.
    pub iroh_secret: Vec<u8>,
}

impl std::fmt::Debug for ClientIdentity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientIdentity")
            .field("ssh_public_key", &self.ssh_public_key)
            .field("iroh_secret", &"<redacted>")
            .finish()
    }
}

impl ClientIdentity {
    pub fn generate() -> Result<Self> {
        let key =
            generate_ssh_key("orqella-desktop".into()).map_err(|error| anyhow::anyhow!(error))?;
        Ok(Self {
            ssh_private_key: key.private_key,
            ssh_public_key: key.public_key,
            iroh_secret: link_generate_secret(),
        })
    }
}

/// A registered desktop client device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisteredClient {
    pub device_id: String,
    pub endpoint_id: String,
    pub name: String,
}

/// A connected remote tunnel.
pub struct RemoteTunnel {
    /// Loopback URL that serves the hub dashboard.
    pub local_url: String,
    inner: Option<MobileLinkTunnel>,
}

impl RemoteTunnel {
    pub fn local_addr(&self) -> Result<std::net::SocketAddr> {
        let url = url::Url::parse(&self.local_url)?;
        let host = url.host_str().context("tunnel URL has no host")?;
        let port = url.port().context("tunnel URL has no port")?;
        format!("{host}:{port}")
            .parse()
            .context("invalid tunnel address")
    }

    pub async fn stop(mut self) {
        if let Some(inner) = self.inner.take() {
            inner.stop().await;
        }
    }
}

impl Drop for RemoteTunnel {
    fn drop(&mut self) {
        // The owned MobileLinkTunnel has its own drop/stop behavior. Avoid
        // doing async work here; callers should call stop for a clean teardown.
    }
}

/// Password login for automated development and smoke tests. The desktop UI
/// should use the browser device-code flow instead.
pub async fn login_password(email: &str, password: &str) -> Result<LoginSession> {
    let account = AccountClient::production().context("create Link account client")?;
    let result = account
        .login(email, password)
        .await
        .context("Link password login")?;
    let LoginOutcome::Session(result) = result else {
        bail!("this Link account requires a second factor; use the browser device-code flow");
    };
    Ok(LoginSession {
        token: result.token,
        user_id: result.user.id,
        email: result.user.email,
        entitled: result.entitlement.is_some_and(|value| value.link_entitled),
    })
}

/// Begin browser approval for this desktop installation.
pub async fn begin_login() -> Result<LoginChallenge> {
    let account = AccountClient::production().context("create Link account client")?;
    let start = account
        .device_start()
        .await
        .context("start Link device-code login")?;
    Ok(LoginChallenge {
        device_code: start.device_code,
        user_code: start.user_code,
        confirmation_code: start.confirmation_code,
        verification_uri: start.verification_uri,
        expires_in: start.expires_in,
        interval: start.interval,
    })
}

/// Poll a device-code challenge and return both the provider status and an
/// approved session, if any. This lets the UI stop polling on `expired` or
/// `denied` instead of treating every non-approved state as pending.
pub async fn poll_login_status(
    challenge: &LoginChallenge,
) -> Result<(String, Option<LoginSession>)> {
    let account = AccountClient::production().context("create Link account client")?;
    let result = account
        .device_poll(&challenge.device_code)
        .await
        .context("poll Link device-code login")?;
    if result.status != "approved" {
        return Ok((result.status, None));
    }
    let token = result
        .token
        .context("Link approval did not return a token")?;
    let user = result.user.context("Link approval did not return a user")?;
    Ok((
        result.status,
        Some(LoginSession {
            token,
            user_id: user.id,
            email: user.email,
            entitled: result.entitlement.is_some_and(|value| value.link_entitled),
        }),
    ))
}

/// Poll a device-code challenge until approval, expiry, or the supplied
/// deadline. Returns `Ok(None)` while the challenge is still pending.
pub async fn poll_login(challenge: &LoginChallenge) -> Result<Option<LoginSession>> {
    Ok(poll_login_status(challenge).await?.1)
}

/// Register this desktop installation as a Link device. Existing devices with
/// the same iroh endpoint are upserted by the Link API.
pub async fn register_client(
    token: &str,
    name: impl Into<String>,
    identity: &ClientIdentity,
) -> Result<RegisteredClient> {
    let account = AccountClient::production().context("create Link account client")?;
    let secret: [u8; 32] = identity
        .iroh_secret
        .as_slice()
        .try_into()
        .context("iroh identity must be 32 bytes")?;
    let endpoint = LinkNode::bind(SecretKey::from_bytes(&secret))
        .await
        .context("bind Link client endpoint")?;
    let endpoint_id = endpoint.id().to_string();
    let addresses = endpoint
        .addr()
        .ip_addrs()
        .map(|addr| addr.to_string())
        .collect::<Vec<_>>();
    let relay_urls = endpoint
        .addr()
        .relay_urls()
        .map(|url| url.to_string())
        .collect::<Vec<_>>();
    let relay_url = relay_urls.first().cloned().unwrap_or_default();
    let name = name.into();
    let device = account
        .add_device(
            token,
            &RegisterDevice {
                name: name.clone(),
                kind: "desktop-client".into(),
                iroh_endpoint_id: endpoint_id.clone(),
                web_endpoint_id: String::new(),
                ssh_user: String::new(),
                ssh_port: 22,
                ssh_public_key: identity.ssh_public_key.clone(),
                ssh_host_key_fingerprint: String::new(),
                direct_addresses: addresses,
                relay_url,
                relay_urls,
                peer_port: 9477,
            },
        )
        .await
        .context("register Link desktop device")?;
    endpoint.close().await;
    Ok(RegisteredClient {
        device_id: device.id,
        endpoint_id,
        name,
    })
}

/// List devices on the account, including hubs that expose an SSH bridge.
pub async fn list_devices(token: &str) -> Result<Vec<Device>> {
    let account = AccountClient::production().context("create Link account client")?;
    account
        .list_devices(token)
        .await
        .context("list Link devices")
}

/// Start a direct-only SSH forward to a hub. The core tunnel rejects a
/// relay-only path before opening SSH.
#[allow(clippy::too_many_arguments)]
pub async fn connect_remote(
    token: &str,
    source_device_id: &str,
    target_device_id: &str,
    target_port: u16,
    identity: &ClientIdentity,
    local_port: u16,
) -> Result<RemoteTunnel> {
    let tunnel = start_link(MobileLinkTunnelConfig {
        token: token.to_string(),
        source_device_id: source_device_id.to_string(),
        target_device_id: target_device_id.to_string(),
        private_key: identity.ssh_private_key.clone(),
        key_passphrase: None,
        iroh_secret: identity.iroh_secret.clone(),
        remote_port: target_port,
        local_port,
        using_device_token: false,
    })
    .await
    .map_err(|error| anyhow::anyhow!(error))
    .context("start direct-only Link tunnel")?;
    let local_url = format!("http://{}/", tunnel.ssh.local_addr);
    Ok(RemoteTunnel {
        local_url,
        inner: Some(tunnel),
    })
}

/// Optional helper for integration tests: wait until a hub is reachable.
pub async fn wait_for_http(url: &str) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .build()?;
    for _ in 0..40 {
        if client.get(url).send().await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    anyhow::bail!("timed out waiting for {url}")
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    #[test]
    fn generated_identity_has_usable_ssh_and_iroh_material() {
        let identity = ClientIdentity::generate().unwrap();
        assert!(
            identity
                .ssh_private_key
                .starts_with(b"-----BEGIN OPENSSH PRIVATE KEY-----")
        );
        assert!(identity.ssh_public_key.starts_with("ssh-ed25519 "));
        assert_eq!(identity.iroh_secret.len(), 32);
        let encoded = serde_json::to_value(&identity).unwrap();
        assert!(encoded["ssh_private_key"].is_array());
        let decoded: ClientIdentity = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded.ssh_public_key, identity.ssh_public_key);
        assert_eq!(decoded.iroh_secret, identity.iroh_secret);
    }

    #[test]
    fn debug_output_redacts_the_iroh_secret_and_private_key() {
        let identity = ClientIdentity::generate().unwrap();
        let debug = format!("{identity:?}");
        assert!(debug.contains("<redacted>"));
        assert!(debug.contains(&identity.ssh_public_key));
        assert!(!debug.contains(&format!("{:?}", identity.iroh_secret)));
        assert!(!debug.contains("OPENSSH PRIVATE KEY"));
        assert!(!debug.contains("ssh_private_key"));
    }

    #[test]
    fn login_payloads_round_trip() {
        let challenge = LoginChallenge {
            device_code: "device".into(),
            user_code: "user".into(),
            confirmation_code: "123456".into(),
            verification_uri: "https://example.com/device".into(),
            expires_in: 300,
            interval: 5,
        };
        let value = serde_json::to_value(&challenge).unwrap();
        let decoded: LoginChallenge = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.device_code, "device");
        assert_eq!(decoded.interval, 5);

        let session = LoginSession {
            token: "token".into(),
            user_id: "user".into(),
            email: "user@example.com".into(),
            entitled: true,
        };
        let value = serde_json::to_value(&session).unwrap();
        let decoded: LoginSession = serde_json::from_value(value).unwrap();
        assert!(decoded.entitled);
        assert_eq!(decoded.email, "user@example.com");
    }

    #[test]
    fn remote_tunnel_local_addr_requires_a_complete_url() {
        let tunnel = |url: &str| RemoteTunnel {
            local_url: url.into(),
            inner: None,
        };
        assert_eq!(
            tunnel("http://127.0.0.1:49152/").local_addr().unwrap(),
            "127.0.0.1:49152".parse().unwrap()
        );
        assert!(tunnel("not-a-url").local_addr().is_err());
        assert!(tunnel("http://127.0.0.1/").local_addr().is_err());
    }
}
