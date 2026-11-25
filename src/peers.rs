use crate::config::PeerConfig;
use anyhow::{Context, Result};
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PeerHealth {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub latency_ms: u128,
    pub error: Option<String>,
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PeerView {
    pub id: String,
    pub name: String,
    pub transport: &'static str,
}

impl From<&PeerConfig> for PeerView {
    fn from(peer: &PeerConfig) -> Self {
        Self {
            id: peer.id.clone(),
            name: peer.name.clone(),
            transport: "link",
        }
    }
}

pub async fn probe(
    client: &reqwest::Client,
    peer: &PeerConfig,
    resolved_url: Option<&str>,
) -> PeerHealth {
    let started = std::time::Instant::now();
    let Some(url) = resolved_url else {
        return PeerHealth {
            id: peer.id.clone(),
            name: peer.name.clone(),
            online: false,
            latency_ms: 0,
            error: Some("peer has no reachable URL yet".into()),
            url: resolved_url.map(str::to_string),
        };
    };
    let result = client
        .get(format!("{}/api/v1/peer/ping", url.trim_end_matches('/')))
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;
    match result {
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let online = status.is_success();
            PeerHealth {
                id: peer.id.clone(),
                name: peer.name.clone(),
                online,
                latency_ms: started.elapsed().as_millis(),
                error: if online {
                    None
                } else {
                    Some(format!(
                        "HTTP {status}: {}",
                        body.chars().take(180).collect::<String>()
                    ))
                },
                url: resolved_url.map(str::to_string),
            }
        }
        Err(error) => PeerHealth {
            id: peer.id.clone(),
            name: peer.name.clone(),
            online: false,
            latency_ms: started.elapsed().as_millis(),
            error: Some(error.to_string()),
            url: resolved_url.map(str::to_string),
        },
    }
}

pub async fn fetch_json(
    client: &reqwest::Client,
    peer: &PeerConfig,
    resolved_url: Option<&str>,
    path: &str,
) -> Result<serde_json::Value> {
    let url = resolved_url
        .context("peer has no reachable URL yet")?
        .trim_end_matches('/');
    let response = client
        .get(format!("{url}{path}"))
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(std::time::Duration::from_secs(4))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("peer returned {status}: {body}");
    }
    serde_json::from_str(&body).context("peer returned invalid JSON")
}

#[cfg(test)]
mod coverage_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn peer(id: &str) -> PeerConfig {
        PeerConfig {
            id: id.into(),
            name: format!("Peer {id}"),
            url: None,
            token: "peer-secret".into(),
        }
    }

    async fn serve_once(status: &str, body: &str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let status = status.to_string();
        let body = body.to_string();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = vec![0u8; 4096];
            let _ = socket.read(&mut request).await;
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket.write_all(response.as_bytes()).await.unwrap();
        });
        format!("http://{address}")
    }

    #[test]
    fn peer_view_always_uses_the_link_transport() {
        let view = PeerView::from(&peer("link-abc"));
        assert_eq!(view.id, "link-abc");
        assert_eq!(view.name, "Peer link-abc");
        assert_eq!(view.transport, "link");
    }

    #[tokio::test]
    async fn probe_reports_a_missing_url_without_network_access() {
        let health = probe(&reqwest::Client::new(), &peer("offline"), None).await;
        assert!(!health.online);
        assert_eq!(
            health.error.as_deref(),
            Some("peer has no reachable URL yet")
        );
        assert_eq!(health.url, None);
    }

    #[tokio::test]
    async fn probe_accepts_a_healthy_peer_and_sends_its_token() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let request = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buffer = vec![0u8; 4096];
            let read = socket.read(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..read]).to_string();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"ok\":true}")
                .await
                .unwrap();
            request
        });
        let health = probe(
            &reqwest::Client::new(),
            &peer("online"),
            Some(&format!("http://{address}/")),
        )
        .await;
        assert!(health.online, "{health:?}");
        assert!(health.error.is_none());
        assert_eq!(
            health.url.as_deref(),
            Some(format!("http://{address}/").as_str())
        );
        let request = request.await.unwrap();
        assert!(request.starts_with("GET /api/v1/peer/ping HTTP/1.1"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("x-orqella-peer-token: peer-secret")
        );
    }

    #[tokio::test]
    async fn probe_reports_http_failures_without_panicking() {
        let url = serve_once("503 Service Unavailable", "nope").await;
        let health = probe(&reqwest::Client::new(), &peer("bad"), Some(&url)).await;
        assert!(!health.online);
        let error = health.error.unwrap_or_default();
        assert!(error.contains("503"), "{error}");
        assert!(error.contains("nope"), "{error}");
    }

    #[tokio::test]
    async fn fetch_json_round_trips_and_reports_bad_payloads() {
        let client = reqwest::Client::new();
        let good = serve_once("200 OK", r#"{"ok":true}"#).await;
        let value = fetch_json(&client, &peer("good"), Some(&good), "/api/v1/peer/info")
            .await
            .unwrap();
        assert_eq!(value["ok"], true);

        let bad_json = serve_once("200 OK", "not-json").await;
        let error = fetch_json(&client, &peer("bad"), Some(&bad_json), "/x")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("invalid JSON"));

        let denied = serve_once("403 Forbidden", "denied").await;
        let error = fetch_json(&client, &peer("denied"), Some(&denied), "/x")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("403"));

        let error = fetch_json(&client, &peer("none"), None, "/x")
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("no reachable URL"));
    }
}
