use axum::Router;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::{SinkExt, StreamExt};
use orqella::config::{Config, Paths};
use orqella::http::{AppState, router};
use std::net::SocketAddr;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

async fn mock_ws(ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|mut socket| async move {
        while let Some(Ok(message)) = socket.recv().await {
            match message {
                Message::Text(text) => {
                    if socket.send(Message::Text(text)).await.is_err() {
                        break;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    })
}

async fn mock_root() -> Response {
    (
        [(header::CONTENT_TYPE, "text/html")],
        "<html><body>mock upstream</body></html>",
    )
        .into_response()
}

async fn mock_headers(headers: HeaderMap) -> Response {
    let peer_token = headers
        .get("x-orqella-peer-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("absent");
    let admin_token = headers
        .get("x-orqella-admin-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("absent");
    (
        [(header::CONTENT_TYPE, "text/plain")],
        format!("peer={peer_token} admin={admin_token}"),
    )
        .into_response()
}

async fn mock_cookie(headers: HeaderMap) -> Response {
    let cookie = headers
        .get(header::COOKIE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("absent");
    let mut response = (
        [(header::CONTENT_TYPE, "text/plain")],
        format!("cookie={cookie}"),
    )
        .into_response();
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("orqella_session=attacker; Path=/"),
    );
    response.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_static("app_session=ok; Path=/"),
    );
    response
}

async fn mock_sse() -> Response {
    let stream = futures_util::stream::iter(vec![
        Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"data: one\n\n")),
        Ok(axum::body::Bytes::from_static(b"data: two\n\n")),
    ]);
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

async fn start_mock() -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route("/", get(mock_root))
        .route("/secret", get(|| async { "SECRET" }))
        .route("/headers", get(mock_headers))
        .route("/cookie", get(mock_cookie))
        .route("/sse", get(mock_sse))
        .route("/ws", get(mock_ws));
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (address, task)
}

#[tokio::test]
async fn proxies_http_and_sse() {
    let (mock, mock_task) = start_mock().await;
    let (temp, orqella_task, _guard) = {
        let temp = tempfile::tempdir().unwrap();
        let paths = test_paths_from(temp.path());
        paths.ensure().unwrap();
        let mut config = Config::default();
        config.server.port = 0;
        config.proxy = vec![orqella::config::ProxyConfig {
            id: "mock".into(),
            title: "Mock".into(),
            base_url: format!("http://{mock}"),
            host: Some("mock.orqella.localhost".into()),
            websocket: true,
            frame: false,
        }];
        let state = AppState::new(paths, config).await.unwrap();
        let app = router(state);
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let actual = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (actual, task, temp)
    };
    let client = reqwest::Client::new();
    let path_mode = client
        .get(format!("http://{temp}/p/mock/"))
        .send()
        .await
        .unwrap();
    assert_eq!(path_mode.status(), StatusCode::NOT_FOUND);

    let root = client
        .get(format!("http://{temp}/p/mock/"))
        .header(header::HOST, "mock.orqella.localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(root.status(), StatusCode::OK);
    assert_eq!(root.headers().get("origin-agent-cluster").unwrap(), "?1");
    assert!(root.text().await.unwrap().contains("mock upstream"));

    let cookie_response = client
        .get(format!("http://{temp}/p/mock/cookie"))
        .header(header::HOST, "mock.orqella.localhost")
        .header(
            header::COOKIE,
            "orqella_session=secret; app=keep; other=one",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(cookie_response.status(), StatusCode::OK);
    let set_cookies = cookie_response
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert!(
        set_cookies
            .iter()
            .all(|value| !value.starts_with("orqella_session=")),
        "{set_cookies:?}"
    );
    assert!(
        set_cookies
            .iter()
            .any(|value| value.starts_with("app_session=")),
        "{set_cookies:?}"
    );
    let cookie_body = cookie_response.text().await.unwrap();
    assert_eq!(cookie_body, "cookie=app=keep; other=one");

    let traversal = client
        .get(format!("http://{temp}/p/mock/%2e%2e/api/v1/audit"))
        .header(header::HOST, "mock.orqella.localhost")
        .send()
        .await
        .unwrap();
    assert!(matches!(
        traversal.status(),
        StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND
    ));

    let sse = client
        .get(format!("http://{temp}/p/mock/sse"))
        .header(header::HOST, "mock.orqella.localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(
        sse.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/event-stream"
    );
    let body = sse.text().await.unwrap();
    assert!(body.contains("data: one"));
    assert!(body.contains("data: two"));

    mock_task.abort();
    orqella_task.abort();
}

#[tokio::test]
async fn proxies_websocket_frames_both_directions() {
    let (mock, mock_task) = start_mock().await;
    let temp = tempfile::tempdir().unwrap();
    let paths = test_paths_from(temp.path());
    paths.ensure().unwrap();
    let mut config = Config::default();
    config.server.port = 0;
    config.proxy = vec![orqella::config::ProxyConfig {
        id: "mock".into(),
        title: "Mock".into(),
        base_url: format!("http://{mock}"),
        host: Some("mock.orqella.localhost".into()),
        websocket: true,
        frame: false,
    }];
    let state = AppState::new(paths, config).await.unwrap();
    let app = router(state);
    let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let orqella_address = listener.local_addr().unwrap();
    let orqella_task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let mut request = format!("ws://{orqella_address}/p/mock/ws")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(header::HOST, "mock.orqella.localhost".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    socket
        .send(WsMessage::Text("hello from browser".into()))
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match reply {
        WsMessage::Text(text) => assert_eq!(text.as_str(), "hello from browser"),
        other => panic!("unexpected websocket reply: {other:?}"),
    }

    mock_task.abort();
    orqella_task.abort();
}

fn test_paths_from(root: &std::path::Path) -> Paths {
    Paths {
        config_dir: root.join("config"),
        data_dir: root.join("data"),
        runtime_dir: root.join("runtime"),
        config_file: root.join("config/config.toml"),
        state_file: root.join("data/state.json"),
        modules_dir: root.join("data/modules"),
        workshop_dir: root.join("data/workshop"),
        logs_dir: root.join("data/logs"),
    }
}

#[tokio::test]
async fn proxies_remote_http_and_websocket_through_peer_api() {
    let (mock, mock_task) = start_mock().await;
    let node_temp = tempfile::tempdir().unwrap();
    let node_paths = test_paths_from(node_temp.path());
    node_paths.ensure().unwrap();
    let mut node_config = Config::default();
    node_config.server.port = 0;
    node_config.peer_token = Some("test-peer-token".into());
    node_config.proxy = vec![orqella::config::ProxyConfig {
        id: "mock".into(),
        title: "Mock".into(),
        base_url: format!("http://{mock}"),
        host: Some("mock.orqella.localhost".into()),
        websocket: true,
        frame: false,
    }];
    let node_state = AppState::new(node_paths, node_config).await.unwrap();
    let node_app = router(node_state);
    let node_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let node_address = node_listener.local_addr().unwrap();
    let node_task = tokio::spawn(async move {
        axum::serve(node_listener, node_app).await.unwrap();
    });

    let hub_temp = tempfile::tempdir().unwrap();
    let hub_paths = test_paths_from(hub_temp.path());
    hub_paths.ensure().unwrap();
    let mut hub_config = Config::default();
    hub_config.server.port = 0;
    hub_config.proxy = vec![orqella::config::ProxyConfig {
        id: "mock".into(),
        title: "Mock".into(),
        base_url: "http://127.0.0.1:1".into(),
        host: Some("mock.orqella.localhost".into()),
        websocket: true,
        frame: false,
    }];
    hub_config.peers = vec![orqella::config::PeerConfig {
        id: "node".into(),
        name: "node".into(),
        url: Some(format!("http://{node_address}")),
        token: "test-peer-token".into(),
    }];
    let hub_state = AppState::new(hub_paths, hub_config).await.unwrap();
    let hub_app = router(hub_state);
    let hub_listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let hub_address = hub_listener.local_addr().unwrap();
    let hub_task = tokio::spawn(async move {
        axum::serve(hub_listener, hub_app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let root = client
        .get(format!("http://{hub_address}/n/node/p/mock/"))
        .header("x-orqella-app-mode", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(root.status(), StatusCode::OK);
    assert!(root.text().await.unwrap().contains("mock upstream"));

    let host_mode = client
        .get(format!("http://{hub_address}/n/node/p/mock/"))
        .header(header::HOST, "mock.node.orqella.localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(host_mode.status(), StatusCode::OK);
    assert!(host_mode.text().await.unwrap().contains("mock upstream"));

    let headers = client
        .get(format!("http://{hub_address}/n/node/p/mock/headers"))
        .header("x-orqella-admin-token", "secret-admin-token")
        .header("x-orqella-app-mode", "1")
        .send()
        .await
        .unwrap();
    assert_eq!(headers.status(), StatusCode::OK);
    assert_eq!(headers.text().await.unwrap(), "peer=absent admin=absent");

    let local_headers = client
        .get(format!("http://{node_address}/p/mock/headers"))
        .header(header::HOST, "mock.orqella.localhost")
        .header("x-orqella-admin-token", "secret-admin-token")
        .header("x-orqella-peer-token", "secret-peer-token")
        .send()
        .await
        .unwrap();
    assert_eq!(local_headers.status(), StatusCode::OK);
    assert_eq!(
        local_headers.text().await.unwrap(),
        "peer=absent admin=absent"
    );

    let mut ws_request = format!("ws://{hub_address}/n/node/p/mock/ws")
        .into_client_request()
        .unwrap();
    ws_request
        .headers_mut()
        .insert("x-orqella-app-mode", HeaderValue::from_static("1"));
    let (mut socket, _) = tokio_tungstenite::connect_async(ws_request).await.unwrap();
    socket
        .send(WsMessage::Text("remote hello".into()))
        .await
        .unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match reply {
        WsMessage::Text(text) => assert_eq!(text.as_str(), "remote hello"),
        other => panic!("unexpected remote websocket reply: {other:?}"),
    }

    mock_task.abort();
    node_task.abort();
    hub_task.abort();
}
