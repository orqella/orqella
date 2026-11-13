//! Native WebRTC peer for the Orqella browser spike.
//!
//! Run this, then use the browser ping-pong page with the printed endpoint ID.
//! It accepts the example ALPNs and implements the same tiny request frames as
//! the browser example.

use anyhow::Result;
use iroh::{Endpoint, SecretKey, endpoint::presets};
use iroh_webrtc_transport::{NativeWebRtcIrohNode, WebRtcNodeConfig, config::WebRtcSessionConfig};
use n0_future::task;

const ALPN: &[u8] = b"example/iroh-webrtc-rust-browser-ping-pong/1";
const BENCHMARK_ALPN: &[u8] = b"example/iroh-webrtc-rust-browser-ping-pong/benchmark/1";
const WEB_ALPN: &[u8] = b"orqella/web-spike/1";
const MAX_FRAME: usize = 128 * 1024 * 1024;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let config = WebRtcNodeConfig {
        session: WebRtcSessionConfig::default(),
        ..WebRtcNodeConfig::default()
    };
    let node = NativeWebRtcIrohNode::builder(config, SecretKey::generate())
        .endpoint_builder(Endpoint::builder(presets::N0))
        .accept_facade(ALPN)
        .accept_facade(BENCHMARK_ALPN)
        .accept_facade(WEB_ALPN)
        .spawn()
        .await?;

    println!("{}", node.endpoint_id());
    for alpn in [ALPN, BENCHMARK_ALPN] {
        let acceptor = node.clone();
        let alpn = alpn.to_vec();
        task::spawn(async move {
            loop {
                let connection = match acceptor.accept(&alpn).await {
                    Ok(connection) => connection,
                    Err(error) => {
                        eprintln!("accept {}: {error:#}", String::from_utf8_lossy(&alpn));
                        break;
                    }
                };
                task::spawn(async move {
                    if let Err(error) = serve(connection).await {
                        eprintln!("connection: {error:#}");
                    }
                });
            }
        });
    }
    {
        let acceptor = node.clone();
        task::spawn(async move {
            loop {
                let connection = match acceptor.accept(WEB_ALPN).await {
                    Ok(connection) => connection,
                    Err(error) => {
                        eprintln!("accept web: {error:#}");
                        break;
                    }
                };
                task::spawn(async move {
                    if let Err(error) = serve_web(connection).await {
                        eprintln!("web connection: {error:#}");
                    }
                });
            }
        });
    }

    // Keep the node and its endpoint alive.
    std::future::pending::<()>().await;
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
                let response = match request.as_slice() {
                    b"ping" => b"pong".to_vec(),
                    value if value.starts_with(b"latency:") => {
                        [b"latency-ok:".as_slice(), &value[8..]].concat()
                    }
                    value if value.starts_with(b"throughput-download:") => {
                        let size = std::str::from_utf8(&value[20..])
                            .ok()
                            .and_then(|s| s.parse::<usize>().ok())
                            .unwrap_or(0)
                            .min(64 * 1024 * 1024);
                        vec![0u8; size]
                    }
                    value if value.starts_with(b"throughput-upload:") => b"throughput-ok".to_vec(),
                    _ => b"unknown".to_vec(),
                };
                if response.len() > MAX_FRAME {
                    break;
                }
                let mut frame = Vec::with_capacity(4 + response.len());
                frame.extend_from_slice(&(response.len() as u32).to_be_bytes());
                frame.extend_from_slice(&response);
                if send.write_all(&frame).await.is_err() {
                    break;
                }
            }
            let _ = send.finish();
        });
    }
}

async fn serve_web(connection: iroh::endpoint::Connection) -> Result<()> {
    loop {
        let (mut send, mut recv) = match connection.accept_bi().await {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        task::spawn(async move {
            while let Ok(Some(request)) = read_frame(&mut recv).await {
                let response = if request.starts_with(b"GET ") {
                    b"HTTP/1.1 200 OK\r\ncontent-type: text/html\r\n\
                      x-orqella-transport: iroh-webrtc\r\n\r\n\
                      <!doctype html><title>Orqella web ALPN</title>\
                      <h1>ORQELLA_WEB_SPIKE_OK</h1><p>HTTP over direct WebRTC.</p>"
                        .to_vec()
                } else if let Some(message) = request.strip_prefix(b"WS ") {
                    [b"echo:".as_slice(), message].concat()
                } else {
                    b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\n\r\n".to_vec()
                };
                if response.len() > MAX_FRAME {
                    break;
                }
                let mut frame = Vec::with_capacity(4 + response.len());
                frame.extend_from_slice(&(response.len() as u32).to_be_bytes());
                frame.extend_from_slice(&response);
                if send.write_all(&frame).await.is_err() {
                    break;
                }
            }
            let _ = send.finish();
        });
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
