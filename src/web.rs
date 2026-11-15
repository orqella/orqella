//! Browser WebRTC access to the loopback dashboard.
//!
//! This module owns an opt-in native iroh endpoint with a WebRTC custom
//! transport. It is deliberately separate from the Link SSH endpoint: Link
//! SSH remains direct-only, while browser traffic uses `orqella/web/1`.
//!
//! A browser authenticates with an Orqella Link browser session. HTTP and
//! WebSocket requests are then proxied to the loopback daemon. The bridge
//! never binds a public TCP port.

use crate::config::{Paths, WebConfig};
use crate::link::{self, LinkManager};
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use futures_util::{SinkExt, StreamExt};
use iroh::{Endpoint, RelayConfig, RelayMap, RelayMode, RelayUrl, SecretKey, endpoint::presets};
use iroh_webrtc_transport::{NativeWebRtcIrohNode, WebRtcNodeConfig};
use n0_future::task;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::sync::Mutex as AsyncMutex;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

pub const WEB_ALPN: &[u8] = b"orqella/web/1";
const MAX_FRAME: usize = 32 * 1024 * 1024;
const MAX_PREAUTH_FRAME: usize = 64 * 1024;
const MAX_STREAMS_PER_CONNECTION: usize = 64;
const AUTH_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Frame {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    status: Option<u16>,
    #[serde(default)]
    response_headers: Option<BTreeMap<String, String>>,
    #[serde(default)]
    response_body: Option<String>,
    #[serde(default)]
    data: Option<String>,
    #[serde(default)]
    message_kind: Option<String>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    relay_urls: Option<Vec<String>>,
    #[serde(default)]
    expires_at: Option<i64>,
}

pub struct WebBridge {
    endpoint_id: String,
    node: NativeWebRtcIrohNode,
    local_port: u16,
    client: reqwest::Client,
    bridge_token: String,
    link: Arc<AsyncMutex<Option<Arc<LinkManager>>>>,
}

impl std::fmt::Debug for WebBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebBridge")
            .field("endpoint_id", &self.endpoint_id)
            .field("local_port", &self.local_port)
            .finish_non_exhaustive()
    }
}

impl WebBridge {
    /// Endpoint ID derived from the persisted bridge secret.
    pub fn prepared_endpoint_id(paths: &Paths) -> Result<String> {
        Ok(
            link::load_or_create_secret(&paths.data_dir.join("web/iroh.key"))?
                .public()
                .to_string(),
        )
    }

    pub async fn start(
        paths: &Paths,
        config: &WebConfig,
        local_port: u16,
        secret: SecretKey,
        relay: Option<&orqella_link_core::RelayCredentials>,
        bridge_token: String,
    ) -> Result<Self> {
        let _ = paths;
        let mut node_config = WebRtcNodeConfig::default();
        if !config.stun_urls.is_empty() {
            node_config.session = iroh_webrtc_transport::config::WebRtcSessionConfig::direct_stun(
                config.stun_urls.clone(),
            )
            .context("invalid web bridge STUN configuration")?;
        }
        let mut endpoint_builder = Endpoint::builder(presets::N0);
        if let Some(relay) = relay {
            let map = RelayMap::empty();
            for raw in &relay.relay_urls {
                let url = raw
                    .parse::<RelayUrl>()
                    .with_context(|| format!("invalid managed relay URL {raw:?}"))?;
                map.insert(
                    url.clone(),
                    std::sync::Arc::new(
                        RelayConfig::from(url).with_auth_token(relay.token.clone()),
                    ),
                );
            }
            endpoint_builder = endpoint_builder.relay_mode(RelayMode::Custom(map));
        }
        let node = NativeWebRtcIrohNode::builder(node_config, secret)
            .endpoint_builder(endpoint_builder)
            .accept_facade(WEB_ALPN)
            .spawn()
            .await
            .context("could not start browser WebRTC endpoint")?;
        let endpoint_id = node.endpoint_id().to_string();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()?;
        let bridge = Self {
            endpoint_id,
            node,
            local_port,
            client,
            bridge_token,
            link: Arc::new(AsyncMutex::new(None)),
        };
        bridge.spawn_accept_loop();
        Ok(bridge)
    }

    fn spawn_accept_loop(&self) {
        let node = self.node.clone();
        let port = self.local_port;
        let client = self.client.clone();
        let bridge_token = self.bridge_token.clone();
        let link = self.link.clone();
        let bridge_endpoint = self.endpoint_id.clone();
        task::spawn(async move {
            loop {
                let connection = match node.accept(WEB_ALPN).await {
                    Ok(connection) => connection,
                    Err(error) => {
                        tracing::warn!(%error, "browser WebRTC accept loop stopped");
                        break;
                    }
                };
                if !is_direct_webrtc_connection(&connection) {
                    tracing::warn!(
                        remote = %connection.remote_id(),
                        "rejecting non-WebRTC browser application connection"
                    );
                    connection.close(0u32.into(), b"relay application data is disabled");
                    continue;
                }
                let client = client.clone();
                let bridge_token = bridge_token.clone();
                let link = link.clone();
                let bridge_endpoint = bridge_endpoint.clone();
                task::spawn(async move {
                    if let Err(error) = serve_connection(
                        connection,
                        client,
                        bridge_token,
                        link,
                        bridge_endpoint,
                        port,
                    )
                    .await
                    {
                        tracing::debug!(%error, "browser WebRTC connection closed");
                    }
                });
            }
        });
    }

    pub fn endpoint_id(&self) -> &str {
        &self.endpoint_id
    }

    pub async fn set_link_manager(&self, link: Arc<LinkManager>) {
        *self.link.lock().await = Some(link);
    }

    /// Install managed relay credentials on the bridge's signaling endpoint.
    pub async fn install_managed_relay(&self, link: &LinkManager) -> Result<()> {
        let credentials = link.web_relay_credentials(&self.endpoint_id, None).await?;
        self.install_relay_credentials(&credentials).await
    }

    pub async fn install_relay_credentials(
        &self,
        credentials: &orqella_link_core::RelayCredentials,
    ) -> Result<()> {
        link::install_brokered_relays(
            &self.node.endpoint(),
            &credentials.relay_urls,
            &credentials.token,
        )
        .await?;
        tracing::info!(relay_urls = ?credentials.relay_urls, "installed managed relay on browser WebRTC bridge");
        Ok(())
    }

    pub fn stop(&self) {
        let node = self.node.clone();
        task::spawn(async move { node.close().await });
    }
}

fn is_direct_webrtc_connection(connection: &iroh::endpoint::Connection) -> bool {
    let paths = connection.paths();
    let selected = paths
        .iter()
        .filter(|path| path.is_selected())
        .collect::<Vec<_>>();
    !selected.is_empty() && selected.iter().all(|path| !path.is_relay())
}

async fn serve_connection(
    connection: iroh::endpoint::Connection,
    client: reqwest::Client,
    bridge_token: String,
    link: Arc<AsyncMutex<Option<Arc<LinkManager>>>>,
    bridge_endpoint: String,
    local_port: u16,
) -> Result<()> {
    let authenticated = Arc::new(AsyncMutex::new(None::<String>));
    let streams = Arc::new(tokio::sync::Semaphore::new(MAX_STREAMS_PER_CONNECTION));
    let remote_id = connection.remote_id().to_string();
    loop {
        let (mut send, mut recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        let Ok(stream_permit) = streams.clone().try_acquire_owned() else {
            tracing::warn!(%remote_id, "browser web connection exceeded its stream limit");
            drop(send);
            drop(recv);
            continue;
        };
        let authenticated = authenticated.clone();
        let client = client.clone();
        let bridge_token = bridge_token.clone();
        let link = link.clone();
        let remote_id = remote_id.clone();
        let bridge_endpoint = bridge_endpoint.clone();
        task::spawn(async move {
            let _stream_permit = stream_permit;
            let result = handle_stream(
                &mut send,
                &mut recv,
                authenticated,
                client,
                bridge_token,
                link,
                remote_id,
                bridge_endpoint,
                local_port,
            )
            .await;
            if let Err(error) = result {
                tracing::debug!(%error, "browser web stream failed");
            }
            let _ = send.finish();
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_stream(
    send: &mut iroh::endpoint::SendStream,
    recv: &mut iroh::endpoint::RecvStream,
    authenticated: Arc<AsyncMutex<Option<String>>>,
    client: reqwest::Client,
    bridge_token: String,
    link: Arc<AsyncMutex<Option<Arc<LinkManager>>>>,
    remote_id: String,
    bridge_endpoint: String,
    local_port: u16,
) -> Result<()> {
    let frame = tokio::time::timeout(AUTH_TIMEOUT, read_frame(recv, MAX_PREAUTH_FRAME))
        .await
        .map_err(|_| anyhow::anyhow!("browser web authentication timed out"))??
        .context("browser web stream closed before a request")?;
    let frame: Frame = serde_json::from_slice(&frame).context("invalid browser web frame")?;
    match frame.kind.as_str() {
        "auth" => {
            let token = frame.token.as_deref().unwrap_or_default();
            let link = link.lock().await.clone();
            let allowed = match link {
                Some(link) => link
                    .web_session_authorized(token, &bridge_endpoint)
                    .await
                    .unwrap_or(false),
                None => false,
            };
            if !allowed {
                write_frame(
                    send,
                    &json!({"type":"auth","ok":false,"error":"invalid Link browser session"}),
                )
                .await?;
                bail!("browser web authentication failed");
            }
            *authenticated.lock().await = Some(token.to_string());
            write_frame(send, &json!({"type":"auth","ok":true})).await?;
            Ok(())
        }
        "http" => {
            if authenticated.lock().await.is_none() {
                bail!("browser web request before authentication");
            }
            let response = proxy_http(&client, local_port, &frame, &bridge_token).await?;
            write_frame(send, &response).await
        }
        "ws_open" => {
            if authenticated.lock().await.is_none() {
                bail!("browser WebSocket before authentication");
            }
            proxy_websocket(send, recv, &client, local_port, &frame, &bridge_token).await
        }
        "relay" => {
            if authenticated.lock().await.is_none() {
                bail!("browser relay request before authentication");
            }
            let session_token = authenticated.lock().await.clone();
            let link = link.lock().await.clone();
            let Some(link) = link else {
                write_frame(
                    send,
                    &json!({"type":"relay","ok":false,"error":"Link is not available"}),
                )
                .await?;
                return Ok(());
            };
            match link
                .web_relay_credentials(&remote_id, session_token.as_deref())
                .await
            {
                Ok(credentials) => {
                    write_frame(
                        send,
                        &json!({
                            "type":"relay",
                            "ok":true,
                            "relay_urls":credentials.relay_urls,
                            "token":credentials.token,
                            "expires_at":credentials.expires_at,
                        }),
                    )
                    .await?;
                }
                Err(error) => {
                    write_frame(
                        send,
                        &json!({
                            "type":"relay",
                            "ok":false,
                            "error":format!("{error:#}"),
                        }),
                    )
                    .await?;
                }
            }
            Ok(())
        }
        _ => {
            write_frame(
                send,
                &json!({"type":"error","error":format!("unknown frame type {:?}", frame.kind)}),
            )
            .await
        }
    }
}

fn is_app_transport_path(path: &str) -> bool {
    path.starts_with("/p/") || (path.starts_with("/n/") && path.split('/').nth(3) == Some("p"))
}

async fn proxy_http(
    client: &reqwest::Client,
    local_port: u16,
    frame: &Frame,
    bridge_token: &str,
) -> Result<Value> {
    let method = frame.method.as_deref().unwrap_or("GET");
    let path = frame.path.as_deref().context("HTTP frame missing path")?;
    if !path.starts_with('/') {
        bail!("browser HTTP path must start with /");
    }
    let url = format!("http://127.0.0.1:{local_port}{path}");
    let method = reqwest::Method::from_bytes(method.as_bytes()).context("invalid HTTP method")?;
    let mut request = client.request(method, url);
    for (name, value) in &frame.headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host"
                | "origin"
                | "connection"
                | "content-length"
                | "transfer-encoding"
                | "upgrade"
                | "x-orqella-bridge-token"
                | "x-orqella-admin-token"
                | "x-orqella-peer-token"
                | "x-orqella-app-mode"
        ) {
            continue;
        }
        request = request.header(name, value);
    }
    request = request.header("X-Orqella-Bridge-Token", bridge_token);
    if is_app_transport_path(path) {
        // The authenticated browser transport is allowed to use the hub's
        // path-based app bridge. The app itself still runs on its own origin.
        request = request.header("X-Orqella-App-Mode", "1");
    }
    if let Some(body) = frame.body.as_deref() {
        let body = BASE64.decode(body).context("invalid browser HTTP body")?;
        request = request.body(body);
    }
    let response = request
        .send()
        .await
        .context("loopback HTTP request failed")?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            let name = name.as_str();
            if matches!(
                name,
                "connection" | "transfer-encoding" | "content-encoding" | "set-cookie"
            ) {
                return None;
            }
            value
                .to_str()
                .ok()
                .map(|value| (name.to_string(), value.to_string()))
        })
        .collect::<BTreeMap<_, _>>();
    let body = response
        .bytes()
        .await
        .context("could not read loopback response")?;
    Ok(json!({
        "type": "http",
        "id": frame.id,
        "status": status,
        "response_headers": headers,
        "response_body": BASE64.encode(body),
    }))
}

/// Carry an upstream SSE response over the already-authenticated WebSocket
/// bridge. The browser EventSource/fetch shim consumes these binary frames as
/// the original `text/event-stream` body.
async fn proxy_sse_websocket(
    send: &mut iroh::endpoint::SendStream,
    recv: &mut iroh::endpoint::RecvStream,
    client: &reqwest::Client,
    local_port: u16,
    frame: &Frame,
    bridge_token: &str,
) -> Result<()> {
    let path = frame.path.as_deref().context("SSE frame missing path")?;
    let bridge_url = reqwest::Url::parse(&format!("http://127.0.0.1:{local_port}{path}"))
        .context("invalid SSE bridge path")?;
    let target = bridge_url
        .query_pairs()
        .find(|(key, _)| key == "target")
        .map(|(_, value)| value.into_owned())
        .context("SSE bridge request missing target")?;
    if !target.starts_with('/') {
        bail!("SSE target must start with /");
    }
    let url = format!("http://127.0.0.1:{local_port}{target}");
    let mut request = client
        .get(url)
        .header("accept", "text/event-stream")
        .header("X-Orqella-Bridge-Token", bridge_token);
    if is_app_transport_path(&target) {
        request = request.header("X-Orqella-App-Mode", "1");
    }
    for (name, value) in &frame.headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host"
                | "origin"
                | "connection"
                | "content-length"
                | "transfer-encoding"
                | "upgrade"
                | "accept"
                | "x-orqella-bridge-token"
                | "x-orqella-admin-token"
                | "x-orqella-peer-token"
                | "x-orqella-app-mode"
        ) {
            continue;
        }
        request = request.header(name, value);
    }
    let response = request
        .send()
        .await
        .context("upstream SSE request failed")?;
    let status = response.status();
    if !status.is_success() {
        write_frame(
            send,
            &json!({"type":"ws_open","id":frame.id,"ok":false,"error":format!("upstream SSE returned {status}")}),
        )
        .await?;
        return Ok(());
    }
    write_frame(send, &json!({"type":"ws_open","id":frame.id,"ok":true})).await?;
    let id = frame.id.clone();
    let mut chunks = response.bytes_stream();
    loop {
        tokio::select! {
            incoming = read_frame(recv, MAX_FRAME) => {
                let payload = match incoming? {
                    Some(payload) => payload,
                    None => break,
                };
                let message: Frame = serde_json::from_slice(&payload).context("invalid SSE control frame")?;
                if message.kind == "ws_close" {
                    break;
                }
            }
            next = chunks.next() => {
                match next {
                    Some(Ok(bytes)) => {
                        if !bytes.is_empty() {
                            write_frame(send, &json!({
                                "type":"ws_message",
                                "id":id,
                                "message_kind":"binary",
                                "data":BASE64.encode(bytes),
                            })).await?;
                        }
                    }
                    Some(Err(error)) => {
                        let _ = write_frame(send, &json!({
                            "type":"ws_error",
                            "id":id,
                            "error":format!("upstream SSE read failed: {error}"),
                        })).await;
                        break;
                    }
                    None => break,
                }
            }
        }
    }
    let _ = write_frame(send, &json!({"type":"ws_close","id":frame.id})).await;
    Ok(())
}

async fn proxy_websocket(
    send: &mut iroh::endpoint::SendStream,
    recv: &mut iroh::endpoint::RecvStream,
    client: &reqwest::Client,
    local_port: u16,
    frame: &Frame,
    bridge_token: &str,
) -> Result<()> {
    let path = frame
        .path
        .as_deref()
        .context("WebSocket frame missing path")?;
    if !path.starts_with('/') {
        bail!("browser WebSocket path must start with /");
    }
    if path.starts_with("/__orqella_sse") {
        return proxy_sse_websocket(send, recv, client, local_port, frame, bridge_token).await;
    }
    let url = format!("ws://127.0.0.1:{local_port}{path}");
    let mut request = url.into_client_request().context("invalid WebSocket URL")?;
    for (name, value) in &frame.headers {
        if matches!(
            name.to_ascii_lowercase().as_str(),
            "host"
                | "origin"
                | "connection"
                | "upgrade"
                | "sec-websocket-key"
                | "sec-websocket-version"
                | "sec-websocket-extensions"
                | "sec-websocket-protocol"
                | "x-orqella-bridge-token"
                | "x-orqella-admin-token"
                | "x-orqella-peer-token"
                | "x-orqella-app-mode"
        ) {
            continue;
        }
        if let (Ok(name), Ok(value)) = (
            name.parse::<reqwest::header::HeaderName>(),
            value.parse::<reqwest::header::HeaderValue>(),
        ) {
            request.headers_mut().insert(name, value);
        }
    }
    if let Ok(value) = reqwest::header::HeaderValue::from_str(bridge_token) {
        request
            .headers_mut()
            .insert("x-orqella-bridge-token", value);
    }
    if is_app_transport_path(path)
        && let Ok(value) = reqwest::header::HeaderValue::from_str("1")
    {
        request.headers_mut().insert("x-orqella-app-mode", value);
    }
    let (upstream, _) = connect_async(request)
        .await
        .context("loopback WebSocket connection failed")?;
    write_frame(send, &json!({"type":"ws_open","id":frame.id,"ok":true})).await?;
    let (mut ws_send, mut ws_recv) = upstream.split();
    loop {
        tokio::select! {
            incoming = read_frame(recv, MAX_FRAME) => {
                let payload = match incoming? {
                    Some(payload) => payload,
                    None => break,
                };
                let message: Frame = serde_json::from_slice(&payload).context("invalid WebSocket frame")?;
                match message.kind.as_str() {
                    "ws_send" => {
                        let data = BASE64.decode(message.data.as_deref().unwrap_or_default())?;
                        let message = match message.message_kind.as_deref() {
                            Some("text") => Message::Text(String::from_utf8_lossy(&data).into_owned().into()),
                            _ => Message::Binary(data.into()),
                        };
                        if ws_send.send(message).await.is_err() {
                            break;
                        }
                    }
                    "ws_close" => {
                        let _ = ws_send.send(Message::Close(None)).await;
                        break;
                    }
                    _ => {}
                }
            }
            incoming = ws_recv.next() => {
                let Some(message) = incoming else { break; };
                let message = message.context("loopback WebSocket read failed")?;
                match message {
                    Message::Text(text) => {
                        write_frame(send, &json!({
                            "type":"ws_message",
                            "id":frame.id,
                            "message_kind":"text",
                            "data":BASE64.encode(text.as_bytes()),
                        })).await?;
                    }
                    Message::Binary(data) => {
                        write_frame(send, &json!({
                            "type":"ws_message",
                            "id":frame.id,
                            "message_kind":"binary",
                            "data":BASE64.encode(data),
                        })).await?;
                    }
                    Message::Ping(data) => {
                        let _ = ws_send.send(Message::Pong(data)).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

async fn read_frame(recv: &mut iroh::endpoint::RecvStream, max: usize) -> Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    if recv.read_exact(&mut header).await.is_err() {
        return Ok(None);
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > max {
        bail!("browser web frame too large");
    }
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload)
        .await
        .context("could not read browser web frame")?;
    Ok(Some(payload))
}

async fn write_frame(send: &mut iroh::endpoint::SendStream, value: &Value) -> Result<()> {
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_FRAME {
        bail!("browser web response too large");
    }
    send.write_all(&(payload.len() as u32).to_be_bytes())
        .await?;
    send.write_all(&payload).await?;
    Ok(())
}
