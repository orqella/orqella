use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey};
use std::str::FromStr as _;
use iroh_webrtc_transport::browser::{
    BrowserDialOptions, BrowserWebRtcConnection, BrowserWebRtcNode, BrowserWebRtcNodeConfig,
    BrowserWebRtcStream,
};
use serde_json::{Value, json};
use wasm_bindgen::prelude::*;

const WEB_ALPN: &[u8] = b"orqella/web/1";
const MAX_FRAME: usize = 32 * 1024 * 1024;

#[wasm_bindgen]
pub struct WebIdentity {
    secret_hex: String,
    endpoint_id: String,
}

#[wasm_bindgen]
impl WebIdentity {
    #[wasm_bindgen(constructor)]
    pub fn generate() -> Result<WebIdentity, JsValue> {
        let secret = SecretKey::generate();
        Ok(Self {
            secret_hex: hex::encode(secret.to_bytes()),
            endpoint_id: secret.public().to_string(),
        })
    }

    #[wasm_bindgen]
    pub fn from_secret(secret_hex: String) -> Result<WebIdentity, JsValue> {
        let bytes = hex::decode(secret_hex.trim())
            .map_err(|error| JsValue::from_str(&format!("invalid identity secret: {error}")))?;
        let bytes: [u8; 32] = bytes
            .try_into()
            .map_err(|_| JsValue::from_str("identity secret must be 32 bytes"))?;
        let secret = SecretKey::from_bytes(&bytes);
        Ok(Self {
            secret_hex: hex::encode(secret.to_bytes()),
            endpoint_id: secret.public().to_string(),
        })
    }

    #[wasm_bindgen(getter)]
    pub fn secret_hex(&self) -> String {
        self.secret_hex.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn endpoint_id(&self) -> String {
        self.endpoint_id.clone()
    }
}

#[wasm_bindgen]
pub struct WebClient {
    node: BrowserWebRtcNode,
    connection: BrowserWebRtcConnection,
}

#[wasm_bindgen]
impl WebClient {
    pub async fn connect(
        secret_hex: String,
        remote_endpoint: String,
        auth_token: String,
        relay_urls: String,
        relay_token: Option<String>,
        remote_relay_url: String,
    ) -> Result<WebClient, JsValue> {
        let secret_bytes = hex::decode(secret_hex.trim())
            .map_err(|error| JsValue::from_str(&format!("invalid identity secret: {error}")))?;
        let secret_bytes: [u8; 32] = secret_bytes
            .try_into()
            .map_err(|_| JsValue::from_str("identity secret must be 32 bytes"))?;
        let secret = SecretKey::from_bytes(&secret_bytes);
        let relay_urls = relay_urls
            .split(',')
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let config = BrowserWebRtcNodeConfig::default()
            .with_relay_config(relay_urls, relay_token)
            .with_low_latency_quic_acks(true);
        let node = BrowserWebRtcNode::builder(config, secret).spawn().await?;
        let remote_id = EndpointId::from_str(remote_endpoint.trim())
            .map_err(|error| JsValue::from_str(&format!("invalid hub endpoint id: {error}")))?;
        let mut remote_addr = EndpointAddr::from(remote_id);
        for raw in remote_relay_url.split(',') {
            let raw = raw.trim();
            if raw.is_empty() {
                continue;
            }
            let relay = RelayUrl::from_str(raw)
                .map_err(|error| JsValue::from_str(&format!("invalid hub relay URL: {error}")))?;
            remote_addr = remote_addr.with_relay_url(relay);
        }
        // Browser application data is WebRTC-only. The relay credentials on
        // the node are used solely for the WebRTC bootstrap/signaling path.
        let connection = node
            .dial_addr(remote_addr, WEB_ALPN, BrowserDialOptions::webrtc_only())
            .await?;
        let stream = connection.open_bi().await?;
        send_frame(
            &stream,
            &json!({"type": "auth", "token": auth_token}).to_string(),
        )
        .await?;
        stream.close_send().await?;
        let mut reader = FrameReader::new(stream);
        let reply = reader
            .read_frame()
            .await?
            .ok_or_else(|| JsValue::from_str("browser bridge closed during authentication"))?;
        let reply: Value = serde_json::from_slice(&reply)
            .map_err(|error| JsValue::from_str(&format!("invalid auth reply: {error}")))?;
        if reply["ok"].as_bool() != Some(true) {
            return Err(JsValue::from_str(
                reply["error"].as_str().unwrap_or("browser bridge authentication failed"),
            ));
        }
        Ok(Self { node, connection })
    }

    pub fn endpoint_id(&self) -> String {
        self.node.endpoint_id().to_string()
    }

    /// "webrtc" for a direct WebRTC path, "relay" for the Iroh relay fallback.
    pub fn transport_kind(&self) -> String {
        self.connection.transport_kind().to_string()
    }

    pub async fn http(
        &self,
        method: String,
        path: String,
        headers_json: String,
        body: Option<Vec<u8>>,
    ) -> Result<String, JsValue> {
        let headers: Value = serde_json::from_str(&headers_json)
            .map_err(|error| JsValue::from_str(&format!("invalid headers: {error}")))?;
        let stream = self.connection.open_bi().await?;
        let body = body.map(|body| BASE64.encode(body));
        let request = json!({
            "type": "http",
            "id": "http",
            "method": method,
            "path": path,
            "headers": headers,
            "body": body,
        });
        send_frame(&stream, &request.to_string()).await?;
        stream.close_send().await?;
        let mut reader = FrameReader::new(stream);
        let response = reader
            .read_frame()
            .await?
            .ok_or_else(|| JsValue::from_str("browser bridge closed during HTTP request"))?;
        String::from_utf8(response)
            .map_err(|error| JsValue::from_str(&format!("invalid bridge response: {error}")))
    }

    /// Ask the hub for endpoint-bound managed relay credentials.
    pub async fn relay_config(&self) -> Result<String, JsValue> {
        let stream = self.connection.open_bi().await?;
        send_frame(&stream, "{\"type\":\"relay\"}").await?;
        stream.close_send().await?;
        let mut reader = FrameReader::new(stream);
        let response = reader
            .read_frame()
            .await?
            .ok_or_else(|| JsValue::from_str("browser bridge closed during relay request"))?;
        String::from_utf8(response)
            .map_err(|error| JsValue::from_str(&format!("invalid relay response: {error}")))
    }

    pub async fn ws_open(
        &self,
        path: String,
        headers_json: String,
    ) -> Result<WebSocketHandle, JsValue> {
        let headers: Value = serde_json::from_str(&headers_json)
            .map_err(|error| JsValue::from_str(&format!("invalid headers: {error}")))?;
        let stream = self.connection.open_bi().await?;
        let request = json!({
            "type": "ws_open",
            "id": "ws",
            "path": path,
            "headers": headers,
        });
        send_frame(&stream, &request.to_string()).await?;
        let mut reader = FrameReader::new(stream.clone());
        let reply = reader
            .read_frame()
            .await?
            .ok_or_else(|| JsValue::from_str("browser bridge closed during WebSocket open"))?;
        let reply: Value = serde_json::from_slice(&reply)
            .map_err(|error| JsValue::from_str(&format!("invalid WebSocket open reply: {error}")))?;
        if reply["ok"].as_bool() != Some(true) {
            return Err(JsValue::from_str(
                reply["error"].as_str().unwrap_or("browser bridge WebSocket open failed"),
            ));
        }
        Ok(WebSocketHandle {
            stream,
            reader: Some(reader),
        })
    }

    pub async fn close(&self) -> Result<(), JsValue> {
        self.connection.close("web client closed").await
    }
}

#[wasm_bindgen]
pub struct WebSocketHandle {
    stream: BrowserWebRtcStream,
    reader: Option<FrameReader>,
}

#[wasm_bindgen]
impl WebSocketHandle {
    pub fn take_sender(&self) -> WebSocketSender {
        WebSocketSender {
            stream: self.stream.clone(),
        }
    }

    pub fn take_receiver(&mut self) -> Result<WebSocketReceiver, JsValue> {
        let reader = self
            .reader
            .take()
            .ok_or_else(|| JsValue::from_str("WebSocket receiver already taken"))?;
        Ok(WebSocketReceiver { reader })
    }
}

#[wasm_bindgen]
pub struct WebSocketSender {
    stream: BrowserWebRtcStream,
}

#[wasm_bindgen]
impl WebSocketSender {
    pub async fn send(&self, kind: String, data: Vec<u8>) -> Result<(), JsValue> {
        let request = json!({
            "type": "ws_send",
            "id": "ws",
            "message_kind": kind,
            "data": BASE64.encode(data),
        });
        send_frame(&self.stream, &request.to_string()).await
    }

    pub async fn close(&self) -> Result<(), JsValue> {
        let request = json!({"type": "ws_close", "id": "ws"});
        send_frame(&self.stream, &request.to_string()).await?;
        self.stream.close_send().await
    }
}

#[wasm_bindgen]
pub struct WebSocketReceiver {
    reader: FrameReader,
}

#[wasm_bindgen]
impl WebSocketReceiver {
    pub async fn next(&mut self) -> Result<Option<String>, JsValue> {
        loop {
            let Some(frame) = self.reader.read_frame().await? else {
                return Ok(None);
            };
            let value: Value = match serde_json::from_slice(&frame) {
                Ok(value) => value,
                Err(_) => continue,
            };
            match value["type"].as_str() {
                Some("ws_message") => {
                    let kind = value["message_kind"].as_str().unwrap_or("binary");
                    let data = BASE64
                        .decode(value["data"].as_str().unwrap_or_default())
                        .map_err(|error| JsValue::from_str(&format!("invalid WebSocket data: {error}")))?;
                    return Ok(Some(json!({"kind": kind, "data": BASE64.encode(data)}).to_string()));
                }
                Some("ws_open") => continue,
                _ => continue,
            }
        }
    }
}

async fn send_frame(stream: &BrowserWebRtcStream, payload: &str) -> Result<(), JsValue> {
    let bytes = payload.as_bytes();
    if bytes.len() > MAX_FRAME {
        return Err(JsValue::from_str("browser bridge frame too large"));
    }
    let mut frame = Vec::with_capacity(4 + bytes.len());
    frame.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    frame.extend_from_slice(bytes);
    stream.send_all(&frame).await
}

struct FrameReader {
    stream: BrowserWebRtcStream,
    buffer: Vec<u8>,
    eof: bool,
}

impl FrameReader {
    fn new(stream: BrowserWebRtcStream) -> Self {
        Self {
            stream,
            buffer: Vec::new(),
            eof: false,
        }
    }

    async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, JsValue> {
        if !self.fill_until(4).await? {
            return Ok(None);
        }
        let len = u32::from_be_bytes([
            self.buffer[0],
            self.buffer[1],
            self.buffer[2],
            self.buffer[3],
        ]) as usize;
        if len > MAX_FRAME {
            return Err(JsValue::from_str("bridge frame too large"));
        }
        if !self.fill_until(4 + len).await? {
            return Err(JsValue::from_str("bridge frame ended early"));
        }
        self.buffer.drain(..4);
        Ok(Some(self.buffer.drain(..len).collect()))
    }

    async fn fill_until(&mut self, len: usize) -> Result<bool, JsValue> {
        while self.buffer.len() < len {
            if self.eof {
                return Ok(false);
            }
            match self.stream.read_chunk().await? {
                Some(chunk) => self.buffer.extend_from_slice(&chunk),
                None => self.eof = true,
            }
        }
        Ok(true)
    }
}
