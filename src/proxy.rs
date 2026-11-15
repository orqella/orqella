use crate::config::{ProxyConfig, redact_url_userinfo};
use crate::state::AppState;
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Path, State};
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

pub async fn status(State(state): State<Arc<AppState>>, Path(mount): Path<String>) -> Response {
    let Some(config) = state
        .config
        .read()
        .expect("config lock")
        .proxy(&mount)
        .cloned()
    else {
        return (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "ok": false, "error": "proxy mount not found" })),
        )
            .into_response();
    };
    let started = std::time::Instant::now();
    let result = state
        .client
        .get(&config.base_url)
        .timeout(std::time::Duration::from_millis(1500))
        .send()
        .await;
    let (ok, detail) = match result {
        Ok(response) => (
            response.status().is_success(),
            format!("HTTP {}", response.status()),
        ),
        Err(error) => (
            false,
            error
                .to_string()
                .replace(&config.base_url, &redact_url_userinfo(&config.base_url)),
        ),
    };
    let url = config
        .host
        .as_ref()
        .map(|host| {
            format!(
                "http://{host}:{}/",
                state.config.read().expect("config lock").server.port
            )
        })
        .unwrap_or_default();
    axum::Json(serde_json::json!({
        "ok": ok,
        "id": config.id,
        "title": if config.title.is_empty() { &config.id } else { &config.title },
        "base_url": redact_url_userinfo(&config.base_url),
        "url": url,
        "latency_ms": started.elapsed().as_millis(),
        "detail": detail,
    }))
    .into_response()
}

pub fn safe_rest(rest: &str) -> Option<String> {
    let mut clean = Vec::new();
    for segment in rest.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." || segment.contains('\\') || segment.contains('\0') {
            return None;
        }
        clean.push(segment);
    }
    Some(clean.join("/"))
}

const SESSION_COOKIE: &str = "orqella_session";

/// Remove the dashboard session cookie from an upstream request while leaving
/// application cookies intact. The shell cookie is authority over the local
/// daemon and must never reach a proxied app or a peer machine.
pub(crate) fn strip_session_cookie(headers: &mut HeaderMap) {
    let values = headers
        .get_all(header::COOKIE)
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    if values.is_empty() {
        return;
    }
    headers.remove(header::COOKIE);
    for value in values {
        let Ok(text) = value.to_str() else {
            continue;
        };
        let kept = text
            .split(';')
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .filter(|part| {
                let name = part.split('=').next().unwrap_or_default().trim();
                !name.eq_ignore_ascii_case(SESSION_COOKIE)
            })
            .collect::<Vec<_>>();
        if !kept.is_empty()
            && let Ok(value) = HeaderValue::from_str(&kept.join("; "))
        {
            headers.append(header::COOKIE, value);
        }
    }
}

/// A proxied upstream must not be able to set or clear the dashboard session
/// cookie on the shell origin (path-mode apps share that origin).
pub(crate) fn set_cookie_allowed(value: &HeaderValue) -> bool {
    value.to_str().ok().is_none_or(|text| {
        let name = text.split('=').next().unwrap_or_default().trim();
        !name.eq_ignore_ascii_case(SESSION_COOKIE)
    })
}

pub fn limited_body(
    body: Body,
) -> impl futures_util::Stream<Item = Result<axum::body::Bytes, std::io::Error>> + Send + 'static {
    const MAX: usize = 64 * 1024 * 1024;
    body.into_data_stream()
        .map(|result| result.map_err(std::io::Error::other))
        .scan(0usize, |total, result| {
            let result = result.and_then(|chunk| {
                *total += chunk.len();
                if *total > MAX {
                    Err(std::io::Error::other("request body too large"))
                } else {
                    Ok(chunk)
                }
            });
            futures_util::future::ready(Some(result))
        })
}

pub async fn handle(
    State(state): State<Arc<AppState>>,
    Path((mount, rest)): Path<(String, String)>,
    request: Request<Body>,
) -> Response {
    let Some(config) = state
        .config
        .read()
        .expect("config lock")
        .proxy(&mount)
        .cloned()
    else {
        return (StatusCode::NOT_FOUND, "unknown proxy mount").into_response();
    };
    let request_host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .trim()
                .trim_end_matches('.')
                .split(':')
                .next()
                .unwrap_or(value)
                .trim_end_matches('.')
                .to_ascii_lowercase()
        });
    let configured_host = config
        .host
        .as_deref()
        .map(|value| value.trim_end_matches('.').to_ascii_lowercase());
    let app_mode = request
        .headers()
        .get("x-orqella-app-mode")
        .and_then(|value| value.to_str().ok())
        == Some("1");
    let host_mode = configured_host
        .as_deref()
        .is_some_and(|configured| request_host.as_deref() == Some(configured));
    if !host_mode && !(app_mode && configured_host.is_some()) {
        return (
            StatusCode::NOT_FOUND,
            "full apps require a configured proxy host; add host = \"<name>.orqella.localhost\"",
        )
            .into_response();
    }
    let Some(rest) = safe_rest(&rest) else {
        return (StatusCode::BAD_REQUEST, "invalid proxy path").into_response();
    };
    let host_mode = true;
    let (mut parts, body) = request.into_parts();
    if parts.headers.contains_key(header::UPGRADE) {
        if !config.websocket {
            return (
                StatusCode::FORBIDDEN,
                "websocket proxying is disabled for this mount",
            )
                .into_response();
        }
        let mut headers = parts.headers.clone();
        strip_session_cookie(&mut headers);
        let uri = parts.uri.clone();
        return match WebSocketUpgrade::from_request_parts(&mut parts, &state).await {
            Ok(ws) => proxy_websocket(ws, config, rest, headers, uri).await,
            Err(_) => (StatusCode::BAD_REQUEST, "invalid websocket upgrade").into_response(),
        };
    }
    proxy_http(
        state,
        config,
        rest,
        host_mode,
        Request::from_parts(parts, body),
    )
    .await
}

async fn proxy_http(
    state: Arc<AppState>,
    config: ProxyConfig,
    rest: String,
    host_mode: bool,
    request: Request<Body>,
) -> Response {
    let (mut parts, body) = request.into_parts();
    strip_session_cookie(&mut parts.headers);
    let referer = parts
        .headers
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    if parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > 64 * 1024 * 1024)
    {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response();
    }
    let url = upstream_url(&config, &rest, parts.uri.query());
    let mut builder = state.client.request(parts.method.clone(), &url);
    for (name, value) in parts.headers.iter() {
        if skip_request_header(name.as_str()) {
            continue;
        }
        builder = builder.header(name, value);
    }
    if let Ok(upstream_origin) = origin_of(&config.base_url) {
        builder = builder.header(header::ORIGIN, upstream_origin);
    }
    let upstream = match builder
        .body(reqwest::Body::wrap_stream(limited_body(body)))
        // Long-lived app streams still need a bound. This is deliberately
        // generous; a dead upstream must not pin a request forever.
        .timeout(Duration::from_secs(300))
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                axum::Json(
                    serde_json::json!({ "error": format!("proxy upstream failed: {error}") }),
                ),
            )
                .into_response();
        }
    };
    let status = upstream.status();
    let headers = upstream.headers().clone();
    let stream = upstream
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    let (shell_origin, loopback_origin) = {
        let config = state.config();
        (
            format!("http://{}:{}", config.canonical_host(), config.server.port),
            config.loopback_origin(),
        )
    };
    let frame_ancestors = frame_ancestor_sources(
        referer.as_deref(),
        &shell_origin,
        loopback_origin.trim_end_matches('/'),
    );
    let mut response = Response::builder().status(status.as_u16());
    let mut out_headers = HeaderMap::new();
    for (name, value) in headers.iter() {
        if skip_response_header(name.as_str()) {
            continue;
        }
        if name == header::SET_COOKIE && !set_cookie_allowed(value) {
            continue;
        }
        if config.frame && name == "x-frame-options" {
            continue;
        }
        let value = if name == header::LOCATION {
            rewrite_location(value, &config, &rest, host_mode)
        } else if config.frame
            && matches!(
                name.as_str(),
                "content-security-policy" | "content-security-policy-report-only"
            )
        {
            strip_frame_ancestors(value, &frame_ancestors)
        } else {
            value.clone()
        };
        out_headers.append(name.clone(), value);
    }
    if config.frame
        && !out_headers.contains_key(header::CONTENT_SECURITY_POLICY)
        && let Ok(policy) = HeaderValue::from_str(&format!("frame-ancestors {frame_ancestors}"))
    {
        out_headers.insert(header::CONTENT_SECURITY_POLICY, policy);
    }
    out_headers.insert(
        "x-orqella-proxy",
        HeaderValue::from_str(&config.id).unwrap_or_else(|_| HeaderValue::from_static("proxy")),
    );
    // Keep each app origin origin-keyed. App hosts are subdomains of the shell
    // host in the browser path, and macOS app hosts share `orqella.local`.
    out_headers.insert("origin-agent-cluster", HeaderValue::from_static("?1"));
    *response.headers_mut().expect("response builder") = out_headers;
    response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| {
            (StatusCode::BAD_GATEWAY, "could not build proxy response").into_response()
        })
}

async fn proxy_websocket(
    ws: WebSocketUpgrade,
    config: ProxyConfig,
    rest: String,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let url = upstream_url(&config, &rest, uri.query());
    let ws_url = if let Some(rest) = url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        return (StatusCode::BAD_GATEWAY, "unsupported upstream scheme").into_response();
    };
    let upstream_origin = origin_of(&config.base_url).ok();
    ws.on_upgrade(move |socket| async move {
        let mut request = match ws_url.as_str().into_client_request() {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!(%error, "invalid websocket upstream request");
                return;
            }
        };
        if let Some(origin) = upstream_origin
            && let Ok(value) = HeaderValue::from_str(&origin)
        {
            request.headers_mut().insert(header::ORIGIN, value);
        }
        for name in [
            header::COOKIE,
            header::USER_AGENT,
            header::SEC_WEBSOCKET_PROTOCOL,
        ] {
            if let Some(value) = headers.get(&name) {
                request.headers_mut().insert(name, value.clone());
            }
        }
        let upstream = match tokio::time::timeout(
            Duration::from_secs(15),
            tokio_tungstenite::connect_async(request),
        )
        .await
        {
            Ok(Ok((stream, _response))) => stream,
            Ok(Err(error)) => {
                tracing::warn!(%error, "websocket upstream connection failed");
                return;
            }
            Err(_) => {
                tracing::warn!("websocket upstream connection timed out");
                return;
            }
        };
        bridge(socket, upstream).await;
    })
    .into_response()
}

pub(crate) async fn bridge(
    client: WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let (mut client_tx, mut client_rx) = client.split();
    let (mut upstream_tx, mut upstream_rx) = upstream.split();
    loop {
        tokio::select! {
            client_message = client_rx.next() => {
                let Some(Ok(message)) = client_message else { break; };
                let outbound = match message {
                    Message::Text(text) => UpstreamMessage::Text(text.to_string().into()),
                    Message::Binary(data) => UpstreamMessage::Binary(data),
                    Message::Ping(data) => UpstreamMessage::Ping(data),
                    Message::Pong(data) => UpstreamMessage::Pong(data),
                    Message::Close(frame) => UpstreamMessage::Close(frame.map(|frame| tokio_tungstenite::tungstenite::protocol::CloseFrame {
                        code: frame.code.into(),
                        reason: frame.reason.to_string().into(),
                    })),
                };
                if upstream_tx.send(outbound).await.is_err() { break; }
            }
            upstream_message = upstream_rx.next() => {
                let Some(Ok(message)) = upstream_message else { break; };
                let outbound = match message {
                    UpstreamMessage::Text(text) => Message::Text(text.to_string().into()),
                    UpstreamMessage::Binary(data) => Message::Binary(data),
                    UpstreamMessage::Ping(data) => Message::Ping(data),
                    UpstreamMessage::Pong(data) => Message::Pong(data),
                    UpstreamMessage::Close(frame) => Message::Close(frame.map(|frame| axum::extract::ws::CloseFrame {
                        code: frame.code.into(),
                        reason: frame.reason.to_string().into(),
                    })),
                    UpstreamMessage::Frame(_) => continue,
                };
                if client_tx.send(outbound).await.is_err() { break; }
            }
        }
    }
}

fn upstream_url(config: &ProxyConfig, rest: &str, query: Option<&str>) -> String {
    let base = config.base_url.trim_end_matches('/');
    let rest = rest.trim_start_matches('/');
    let mut url = if rest.is_empty() {
        format!("{base}/")
    } else {
        format!("{base}/{rest}")
    };
    if let Some(query) = query {
        url.push('?');
        url.push_str(query);
    }
    url
}

fn origin_of(url: &str) -> Result<String, url::ParseError> {
    let parsed = url::Url::parse(url)?;
    Ok(format!("{}://{}", parsed.scheme(), parsed.authority()))
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

#[allow(dead_code)]
fn is_app_host(host: &str, canonical: &str) -> bool {
    let host = host.trim().trim_end_matches('.');
    let hostname = if let Some(rest) = host.strip_prefix('[') {
        rest.split(']').next().unwrap_or(rest)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    !is_loopback_host(hostname) && !hostname.eq_ignore_ascii_case(canonical)
}

fn rewrite_location(
    value: &HeaderValue,
    config: &ProxyConfig,
    _rest: &str,
    host_mode: bool,
) -> HeaderValue {
    let Ok(location) = value.to_str() else {
        return value.clone();
    };
    let base = config.base_url.trim_end_matches('/');
    let rewritten = if let Some(path) = location.strip_prefix(base)
        && (path.is_empty() || path.starts_with(['/', '?', '#']))
    {
        if host_mode {
            if path.is_empty() || !path.starts_with('/') {
                format!("/{path}")
            } else {
                path.to_string()
            }
        } else if path.is_empty() {
            format!("/p/{}/", config.id)
        } else if path.starts_with('/') {
            format!("/p/{}{}", config.id, path)
        } else {
            format!("/p/{}/{path}", config.id)
        }
    } else if location.starts_with('/') && !host_mode {
        format!("/p/{}{}", config.id, location)
    } else {
        location.to_string()
    };
    HeaderValue::from_str(&rewritten).unwrap_or_else(|_| value.clone())
}

fn skip_request_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "host"
            | "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "content-length"
            | "x-orqella-admin-token"
            | "x-orqella-bridge-token"
            | "x-orqella-peer-token"
            | "x-orqella-app-mode"
    )
}

fn frame_ancestor_sources(
    referer: Option<&str>,
    canonical_origin: &str,
    loopback_origin: &str,
) -> String {
    let mut sources = vec![
        "'self'".to_string(),
        canonical_origin.to_string(),
        loopback_origin.to_string(),
    ];
    for loopback in ["http://127.0.0.1:*", "http://localhost:*"] {
        if !sources.iter().any(|source| source == loopback) {
            sources.push(loopback.to_string());
        }
    }
    if let Some(referer) = referer
        && let Ok(parsed) = url::Url::parse(referer)
        && matches!(parsed.scheme(), "http" | "https")
        && let Some(host) = parsed.host_str()
        && is_shell_host(host, canonical_origin)
    {
        let origin = parsed.origin().ascii_serialization();
        if !origin.is_empty() && !sources.contains(&origin) {
            sources.push(origin);
        }
    }
    sources.join(" ")
}

fn is_shell_host(host: &str, canonical_origin: &str) -> bool {
    if is_loopback_host(host) {
        return true;
    }
    url::Url::parse(canonical_origin)
        .ok()
        .and_then(|url| url.host_str().map(str::to_string))
        .is_some_and(|canonical| host.eq_ignore_ascii_case(&canonical))
}

fn strip_frame_ancestors(value: &HeaderValue, frame_sources: &str) -> HeaderValue {
    let Ok(text) = value.to_str() else {
        return value.clone();
    };
    let mut directives = text
        .split(';')
        .filter(|directive| {
            !directive
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("frame-ancestors")
        })
        .map(str::trim)
        .filter(|directive| !directive.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    directives.push(format!("frame-ancestors {frame_sources}"));
    HeaderValue::from_str(&directives.join("; ")).unwrap_or_else(|_| value.clone())
}

fn skip_response_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_peer_tokens_before_forwarding() {
        assert!(skip_request_header("x-orqella-peer-token"));
        assert!(skip_request_header("X-Orqella-Peer-Token"));
        assert!(skip_request_header("x-orqella-admin-token"));
        assert!(skip_request_header("X-Orqella-Admin-Token"));
    }

    #[test]
    fn strips_only_the_dashboard_session_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("orqella_session=secret; app=keep; theme=dark"),
        );
        strip_session_cookie(&mut headers);
        assert_eq!(headers.get(header::COOKIE).unwrap(), "app=keep; theme=dark");

        let mut only_session = HeaderMap::new();
        only_session.insert(
            header::COOKIE,
            HeaderValue::from_static("orqella_session=secret"),
        );
        strip_session_cookie(&mut only_session);
        assert!(!only_session.contains_key(header::COOKIE));

        let mut multiple = HeaderMap::new();
        multiple.append(
            header::COOKIE,
            HeaderValue::from_static("orqella_session=secret; app=one"),
        );
        multiple.append(
            header::COOKIE,
            HeaderValue::from_static("Orqella_Session=secret2; app=two"),
        );
        strip_session_cookie(&mut multiple);
        let values = multiple
            .get_all(header::COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert_eq!(values, vec!["app=one", "app=two"]);
    }

    #[test]
    fn upstream_cannot_set_the_dashboard_session_cookie() {
        assert!(!set_cookie_allowed(&HeaderValue::from_static(
            "orqella_session=attacker; Path=/; HttpOnly"
        )));
        assert!(!set_cookie_allowed(&HeaderValue::from_static(
            "Orqella_Session=attacker; Path=/"
        )));
        assert!(set_cookie_allowed(&HeaderValue::from_static(
            "app_session=ok; Path=/"
        )));
    }

    #[test]
    fn canonical_and_loopback_hosts_are_path_mode() {
        let canonical = "orqella.localhost";
        assert!(!is_app_host("orqella.localhost:9477", canonical));
        assert!(!is_app_host("localhost:9477", canonical));
        assert!(!is_app_host("127.0.0.1:9477", canonical));
        assert!(!is_app_host("127.0.0.5:9477", canonical));
        assert!(!is_app_host("[::1]:9477", canonical));
        assert!(is_app_host("demo.orqella.localhost:9477", canonical));
        assert!(is_app_host("demo.laptop.orqella.localhost:9477", canonical));
    }

    #[test]
    fn path_mode_rewrites_upstream_redirects() {
        let config = ProxyConfig {
            id: "demo".into(),
            title: "Demo".into(),
            base_url: "http://127.0.0.1:9999".into(),
            host: None,
            websocket: true,
            frame: false,
        };
        let value = HeaderValue::from_static("/login");
        assert_eq!(
            rewrite_location(&value, &config, "", false),
            HeaderValue::from_static("/p/demo/login")
        );
        assert_eq!(
            rewrite_location(&value, &config, "", true),
            HeaderValue::from_static("/login")
        );
        let absolute = HeaderValue::from_static("http://127.0.0.1:9999/login");
        assert_eq!(
            rewrite_location(&absolute, &config, "", false),
            HeaderValue::from_static("/p/demo/login")
        );
        assert_eq!(
            rewrite_location(&absolute, &config, "", true),
            HeaderValue::from_static("/login")
        );
        let lookalike = HeaderValue::from_static("http://127.0.0.1:99990/evil");
        assert_eq!(rewrite_location(&lookalike, &config, "", true), lookalike);
    }

    #[test]
    fn frame_ancestors_include_loopback_and_actual_shell() {
        let sources = frame_ancestor_sources(
            Some("http://127.0.0.1:50123/"),
            "http://orqella.localhost:9477",
            "http://127.0.0.1:9477",
        );
        assert!(sources.contains("http://orqella.localhost:9477"));
        assert!(sources.contains("http://127.0.0.1:9477"));
        assert!(sources.contains("http://127.0.0.1:*"));
        assert!(sources.contains("http://127.0.0.1:50123"));
    }

    #[test]
    fn frame_ancestors_include_a_custom_loopback_shell() {
        let sources = frame_ancestor_sources(
            None,
            "http://orqella.localhost:9477",
            "http://127.0.0.5:9477",
        );
        assert!(sources.contains("http://127.0.0.5:9477"));
    }

    #[test]
    fn frame_ancestors_ignore_external_referers() {
        let sources = frame_ancestor_sources(
            Some("https://evil.example/attack"),
            "http://orqella.localhost:9477",
            "http://127.0.0.1:9477",
        );
        assert!(!sources.contains("evil.example"));
        assert!(sources.contains("http://orqella.localhost:9477"));
    }

    #[test]
    fn frame_opt_in_strips_only_frame_ancestors() {
        let original = HeaderValue::from_static(
            "default-src 'self'; frame-ancestors 'self' https://example.com; script-src 'self'",
        );
        let sources = frame_ancestor_sources(
            None,
            "http://orqella.localhost:9477",
            "http://127.0.0.1:9477",
        );
        let rewritten = strip_frame_ancestors(&original, &sources);
        let value = rewritten.to_str().unwrap();
        assert!(!value.contains("https://example.com"));
        assert!(value.contains("frame-ancestors 'self' http://orqella.localhost:9477"));
        assert!(value.contains("default-src 'self'"));
        assert!(value.contains("script-src 'self'"));
    }
}
