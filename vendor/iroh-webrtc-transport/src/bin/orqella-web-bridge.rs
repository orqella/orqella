//! Orqella web bridge spike.
//!
//! Accepts `orqella/web/1` over the WebRTC custom transport and exposes a very
//! small HTTP/TCP tunnel to the local Orqella dashboard. This is intentionally
//! a spike harness, not the final hub integration:
//!
//! - `PROXY <raw HTTP request>` returns one raw HTTP response frame.
//! - `TUNNEL <raw HTTP upgrade request>` returns the upgrade response, then
//!   tunnels raw bytes (including WebSocket frames) in both directions.
//!
//! Run it, then use the browser example's Terminal probe with the printed
//! endpoint ID. It forwards to 127.0.0.1:9477.

use anyhow::{Context, Result};
use iroh::{Endpoint, SecretKey, endpoint::presets};
use iroh_webrtc_transport::{NativeWebRtcIrohNode, WebRtcNodeConfig, config::WebRtcSessionConfig};
use n0_future::task;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const WEB_ALPN: &[u8] = b"orqella/web/1";
const ORQELLA_ADDR: &str = "127.0.0.1:9477";
const MAX_FRAME: usize = 128 * 1024 * 1024;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let config = WebRtcNodeConfig {
        session: WebRtcSessionConfig::default(),
        ..WebRtcNodeConfig::default()
    };
    let node = NativeWebRtcIrohNode::builder(config, SecretKey::generate())
        .endpoint_builder(Endpoint::builder(presets::N0))
        .accept_facade(WEB_ALPN)
        .spawn()
        .await?;

    println!("{}", node.endpoint_id());
    loop {
        let connection = match node.accept(WEB_ALPN).await {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!("accept web: {error:#}");
                break;
            }
        };
        task::spawn(async move {
            if let Err(error) = serve(connection).await {
                eprintln!("web connection: {error:#}");
            }
        });
    }
    Ok(())
}

async fn serve(connection: iroh::endpoint::Connection) -> Result<()> {
    loop {
        let (mut send, mut recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        task::spawn(async move {
            while let Ok(Some(request)) = read_frame(&mut recv).await {
                if let Some(http_request) = request.strip_prefix(b"PROXY ") {
                    let response = proxy_http(http_request).await?;
                    write_frame(&mut send, &response).await?;
                    continue;
                }
                if let Some(upgrade_request) = request.strip_prefix(b"TUNNEL ") {
                    tunnel(upgrade_request, &mut send, &mut recv).await?;
                    return Ok::<(), anyhow::Error>(());
                }
                let response =
                    b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
                write_frame(&mut send, response).await?;
            }
            let _ = send.finish();
            Ok::<(), anyhow::Error>(())
        });
    }
}

async fn proxy_http(request: &[u8]) -> Result<Vec<u8>> {
    let mut stream = TcpStream::connect(ORQELLA_ADDR)
        .await
        .with_context(|| format!("connect to Orqella at {ORQELLA_ADDR}"))?;
    stream.write_all(request).await?;
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    if response.len() > MAX_FRAME {
        anyhow::bail!("HTTP response exceeds spike frame limit");
    }
    Ok(response)
}

async fn tunnel(
    upgrade_request: &[u8],
    send: &mut iroh::endpoint::SendStream,
    recv: &mut iroh::endpoint::RecvStream,
) -> Result<()> {
    let mut upstream = TcpStream::connect(ORQELLA_ADDR)
        .await
        .with_context(|| format!("connect to Orqella at {ORQELLA_ADDR}"))?;
    upstream.write_all(upgrade_request).await?;
    let response = read_http_headers(&mut upstream).await?;
    write_frame(send, &response).await?;

    let mut buffer = [0u8; 64 * 1024];
    loop {
        tokio::select! {
            frame = read_frame(recv) => {
                match frame? {
                    Some(payload) => {
                        upstream.write_all(&payload).await?;
                    }
                    None => break,
                }
            }
            read = upstream.read(&mut buffer) => {
                let read = read?;
                if read == 0 {
                    break;
                }
                write_frame(send, &buffer[..read]).await?;
            }
        }
    }
    let _ = upstream.shutdown().await;
    let _ = send.finish();
    Ok(())
}

async fn read_http_headers(stream: &mut TcpStream) -> Result<Vec<u8>> {
    let mut response = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            anyhow::bail!("upstream closed before HTTP upgrade response");
        }
        response.extend_from_slice(&buffer[..read]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            return Ok(response);
        }
        if response.len() > 64 * 1024 {
            anyhow::bail!("HTTP upgrade response headers too large");
        }
    }
}

async fn read_frame(recv: &mut iroh::endpoint::RecvStream) -> Result<Option<Vec<u8>>> {
    let mut header = [0u8; 4];
    if recv.read_exact(&mut header).await.is_err() {
        return Ok(None);
    }
    let len = u32::from_be_bytes(header) as usize;
    if len > MAX_FRAME {
        anyhow::bail!("frame too large");
    }
    let mut payload = vec![0u8; len];
    recv.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

async fn write_frame(send: &mut iroh::endpoint::SendStream, payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_FRAME {
        anyhow::bail!("frame too large");
    }
    let mut header = [0u8; 4];
    header.copy_from_slice(&(payload.len() as u32).to_be_bytes());
    send.write_all(&header).await?;
    send.write_all(payload).await?;
    Ok(())
}
