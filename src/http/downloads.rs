//! Streaming downloads for module sidecars.
//!
//! `download.prepare` mints a ticket (see `crate::downloads`). The shell then
//! loads the returned URL as a download. This route streams the sidecar
//! response, sets `Content-Disposition`, and forwards simple range requests so
//! browsers can resume.
//!
//! Local downloads require a privileged shell session. Peer downloads are
//! proxied by the hub with the peer token and served on the peer by the
//! matching `/api/v1/peer/downloads/...` route.

use super::*;
use futures_util::StreamExt;

pub(super) async fn local_download(
    State(state): State<Arc<AppState>>,
    Path((id, _filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if api_auth_level(&state, &headers) != AuthLevel::Privileged {
        return download_error(
            StatusCode::FORBIDDEN,
            "a privileged local Orqella session is required",
        );
    }
    serve_ticket(&state, &id, &headers).await
}

pub(super) async fn local_download_bare(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    local_download(State(state), Path((id, String::new())), headers).await
}

pub(super) async fn peer_download(
    State(state): State<Arc<AppState>>,
    Path((id, _filename)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    serve_ticket(&state, &id, &headers).await
}

pub(super) async fn peer_download_bare(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    peer_download(State(state), Path((id, String::new())), headers).await
}

pub(super) async fn node_download(
    State(state): State<Arc<AppState>>,
    Path((node, id, filename)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    if api_auth_level(&state, &headers) != AuthLevel::Privileged {
        return download_error(
            StatusCode::FORBIDDEN,
            "a privileged local Orqella session is required",
        );
    }
    if node == "local" {
        return serve_ticket(&state, &id, &headers).await;
    }
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(peer_url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let encoded = crate::util::percent_encode(&filename);
    let target = format!(
        "{}/api/v1/peer/downloads/{}{}",
        peer_url.trim_end_matches('/'),
        id,
        if encoded.is_empty() {
            String::new()
        } else {
            format!("/{encoded}")
        }
    );
    let mut request = state
        .client
        .get(target)
        .header("X-Orqella-Peer-Token", &peer.token);
    if let Some(range) = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
    {
        request = request.header(header::RANGE, range);
    }
    let upstream = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("could not reach {}: {error}", peer.name),
            )
                .into_response();
        }
    };
    let status = upstream.status();
    if !status.is_success() {
        let text = upstream.text().await.unwrap_or_default();
        let code = if status == StatusCode::UNAUTHORIZED {
            StatusCode::BAD_GATEWAY
        } else {
            status
        };
        return download_error(code, &format!("{}: {text}", peer.name));
    }
    let mut response = Response::builder().status(status.as_u16());
    for (name, value) in upstream.headers() {
        if matches!(
            name.as_str().to_ascii_lowercase().as_str(),
            "connection"
                | "transfer-encoding"
                | "set-cookie"
                | "content-encoding"
                | "server"
                | "date"
        ) {
            continue;
        }
        response = response.header(name, value);
    }
    let stream = upstream
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| {
            (StatusCode::BAD_GATEWAY, "could not build download response").into_response()
        })
}

async fn serve_ticket(state: &Arc<AppState>, id: &str, headers: &HeaderMap) -> Response {
    let Some(ticket) = state.downloads.get(id) else {
        return download_error(StatusCode::NOT_FOUND, "this download link has expired");
    };
    let Some(module) = state.modules.get(&ticket.module) else {
        return download_error(StatusCode::NOT_FOUND, "this download link has expired");
    };
    if !module.state.enabled || !module.grants("backend") || !module.grants("downloads") {
        return download_error(
            StatusCode::FORBIDDEN,
            "the module is disabled or no longer has download permission",
        );
    }
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    let upstream = match state
        .sidecars
        .download_stream(&ticket.module, &ticket.path, range)
        .await
    {
        Ok(response) => response,
        Err(error) => {
            return download_error(
                StatusCode::BAD_GATEWAY,
                &format!("could not read the sidecar file: {error:#}"),
            );
        }
    };
    let status = upstream.status();
    if !status.is_success() {
        let text = upstream.text().await.unwrap_or_default();
        let message = text.trim();
        return download_error(
            status,
            if message.is_empty() {
                "the sidecar did not return a file"
            } else {
                message
            },
        );
    }
    let mut response = Response::builder().status(status.as_u16());
    for (name, value) in upstream.headers() {
        if matches!(
            name.as_str().to_ascii_lowercase().as_str(),
            "connection"
                | "transfer-encoding"
                | "content-disposition"
                | "set-cookie"
                | "content-encoding"
                | "cache-control"
                | "server"
                | "date"
        ) {
            continue;
        }
        response = response.header(name, value);
    }
    response = response
        .header(
            header::CONTENT_DISPOSITION,
            content_disposition(&ticket.filename),
        )
        .header(header::CACHE_CONTROL, "private, no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    if response
        .headers_ref()
        .and_then(|headers| headers.get(header::CONTENT_TYPE))
        .is_none()
    {
        response = response.header(
            header::CONTENT_TYPE,
            ticket
                .content_type
                .as_deref()
                .unwrap_or("application/octet-stream"),
        );
    }
    let stream = upstream
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| {
            (StatusCode::BAD_GATEWAY, "could not build download response").into_response()
        })
}

/// Stream an arbitrary GET path from a module sidecar through the daemon,
/// preserving status and byte-range headers. Used by the same-origin media
/// capability route (`/m/<id>/media/<token>/…`) so `<video>`, HLS playlists and
/// segments can be read from the page origin without exposing the sidecar
/// token or relaxing CSP.
pub(super) async fn stream_sidecar_path(
    state: &Arc<AppState>,
    module: &str,
    path: &str,
    headers: &HeaderMap,
    cache_control: &str,
) -> Response {
    let range = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok());
    let upstream = match state.sidecars.download_stream(module, path, range).await {
        Ok(response) => response,
        Err(error) => {
            return (
                StatusCode::BAD_GATEWAY,
                format!("could not read the sidecar: {error:#}"),
            )
                .into_response();
        }
    };
    let status = upstream.status();
    if !status.is_success() {
        let text = upstream.text().await.unwrap_or_default();
        let message = text.trim();
        return (
            status,
            if message.is_empty() {
                "the sidecar did not return the file".to_string()
            } else {
                message.to_string()
            },
        )
            .into_response();
    }
    let mut response = Response::builder().status(status.as_u16());
    for (name, value) in upstream.headers() {
        if matches!(
            name.as_str().to_ascii_lowercase().as_str(),
            "connection"
                | "transfer-encoding"
                | "set-cookie"
                | "content-encoding"
                | "content-disposition"
                | "cache-control"
                | "server"
                | "date"
                | "x-content-type-options"
        ) {
            continue;
        }
        response = response.header(name, value);
    }
    response = response
        .header(header::CACHE_CONTROL, cache_control)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    let stream = upstream
        .bytes_stream()
        .map(|result| result.map_err(std::io::Error::other));
    response
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| {
            (StatusCode::BAD_GATEWAY, "could not build media response").into_response()
        })
}

/// Error responses are attachments too. The shell starts downloads from a
/// top-level link; a plain error page would navigate the dashboard away.
fn download_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        [
            (
                header::CONTENT_TYPE,
                "text/plain; charset=utf-8".to_string(),
            ),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"orqella-download-error.txt\"".to_string(),
            ),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
        ],
        format!("{message}\n"),
    )
        .into_response()
}

pub(super) fn content_disposition(filename: &str) -> String {
    let ascii = filename
        .chars()
        .map(|character| {
            if character.is_ascii()
                && !character.is_ascii_control()
                && character != '"'
                && character != '\\'
            {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let encoded = crate::util::percent_encode(filename);
    format!("attachment; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_disposition_keeps_ascii_and_encodes_unicode() {
        assert_eq!(
            content_disposition("report.pdf"),
            "attachment; filename=\"report.pdf\"; filename*=UTF-8''report.pdf"
        );
        let value = content_disposition("résumé \"2026\".pdf");
        assert!(value.contains("filename=\"r_sum_ _2026_.pdf\""));
        assert!(value.contains("filename*=UTF-8''r%C3%A9sum%C3%A9%20%222026%22.pdf"));
    }
}
