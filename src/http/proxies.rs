use super::*;

pub(super) async fn proxy_status(
    State(state): State<Arc<AppState>>,
    Path(mount): Path<String>,
) -> Response {
    proxy::status(State(state), Path(mount)).await
}

pub(super) async fn proxy_redirect(
    State(state): State<Arc<AppState>>,
    Path(mount): Path<String>,
) -> Response {
    if state.config().proxy(&mount).is_none() {
        return (StatusCode::NOT_FOUND, "unknown proxy mount").into_response();
    }
    axum::response::Redirect::permanent(&format!("/p/{mount}/")).into_response()
}

pub(super) async fn proxy_root(
    State(state): State<Arc<AppState>>,
    Path(mount): Path<String>,
    request: Request<Body>,
) -> Response {
    proxy::handle(State(state), Path((mount, String::new())), request).await
}

pub(super) async fn peer_proxy_root(
    State(state): State<Arc<AppState>>,
    Path(mount): Path<String>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    let mut request = request;
    request.headers_mut().remove("x-orqella-peer-token");
    proxy::handle(State(state), Path((mount, String::new())), request).await
}

pub(super) async fn peer_proxy_path(
    State(state): State<Arc<AppState>>,
    Path((mount, path)): Path<(String, String)>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    let mut request = request;
    request.headers_mut().remove("x-orqella-peer-token");
    proxy::handle(State(state), Path((mount, path)), request).await
}

pub(super) async fn remote_proxy_root(
    State(state): State<Arc<AppState>>,
    Path((node, mount)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    remote_proxy(state, node, mount, String::new(), request).await
}

pub(super) async fn remote_proxy_path(
    State(state): State<Arc<AppState>>,
    Path((node, mount, path)): Path<(String, String, String)>,
    request: Request<Body>,
) -> Response {
    remote_proxy(state, node, mount, path, request).await
}

pub(super) async fn remote_proxy(
    state: Arc<AppState>,
    node: String,
    mount: String,
    rest: String,
    request: Request<Body>,
) -> Response {
    let Some(rest) = proxy::safe_rest(&rest) else {
        return (StatusCode::BAD_REQUEST, "invalid proxy path").into_response();
    };
    if node == "local" {
        return proxy::handle(State(state), Path((mount, rest)), request).await;
    }
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(peer_url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    if request.headers().contains_key(header::UPGRADE) {
        let (mut parts, _body) = request.into_parts();
        let ws: WebSocketUpgrade =
            match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
                Ok(ws) => ws,
                Err(_) => {
                    return (StatusCode::BAD_REQUEST, "invalid websocket upgrade").into_response();
                }
            };
        let query = parts
            .uri
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        let ws_base = peer_url
            .replacen("http://", "ws://", 1)
            .replacen("https://", "wss://", 1);
        let target = format!(
            "{}/api/v1/peer/proxy/{}/{}{}",
            ws_base.trim_end_matches('/'),
            mount,
            rest.trim_start_matches('/'),
            query
        );
        let token = peer.token.clone();
        return ws
            .on_upgrade(move |socket| async move {
                let mut request = match target.as_str().into_client_request() {
                    Ok(request) => request,
                    Err(_) => return,
                };
                if let Ok(value) = axum::http::HeaderValue::from_str(&token) {
                    request.headers_mut().insert("X-Orqella-Peer-Token", value);
                }
                request.headers_mut().insert(
                    "X-Orqella-App-Mode",
                    axum::http::HeaderValue::from_static("1"),
                );
                if let Ok((upstream, _)) = tokio_tungstenite::connect_async(request).await {
                    proxy::bridge(socket, upstream).await;
                }
            })
            .into_response();
    }
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let target = format!(
        "{}/api/v1/peer/proxy/{}/{}{}",
        peer_url.trim_end_matches('/'),
        mount,
        rest.trim_start_matches('/'),
        query
    );
    let (parts, body) = request.into_parts();
    if parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > 64 * 1024 * 1024)
    {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response();
    }
    let mut builder = state.client.request(parts.method, target);
    for (name, value) in parts.headers.iter() {
        if matches!(
            name.as_str().to_ascii_lowercase().as_str(),
            "host"
                | "connection"
                | "content-length"
                | "upgrade"
                | "transfer-encoding"
                | "x-orqella-peer-token"
        ) {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder = builder
        .header("X-Orqella-Peer-Token", &peer.token)
        .header("X-Orqella-App-Mode", "1");
    let upstream = match builder
        .body(reqwest::Body::wrap_stream(proxy::limited_body(body)))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("remote proxy failed: {error}"),
            )
                .into_response();
        }
    };
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let stream = upstream
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let mut response = Response::builder().status(status.as_u16());
    for (name, value) in headers.iter() {
        if matches!(
            name.as_str().to_ascii_lowercase().as_str(),
            "connection" | "transfer-encoding" | "upgrade"
        ) {
            continue;
        }
        response = response.header(name, value);
    }
    response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| (StatusCode::BAD_GATEWAY, "could not build response").into_response())
}
