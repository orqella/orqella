//! Directory listing for the trusted-shell directory picker.
//!
//! Only the shell calls these routes, only privileged sessions may use them,
//! and only directory names are ever returned. File contents are never exposed
//! and module iframes never see a listing.

use super::*;
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};

const MAX_ENTRIES: usize = 500;

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn expand(raw: Option<&String>) -> PathBuf {
    let Some(value) = raw
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
    else {
        return home_dir();
    };
    if value == "~" {
        return home_dir();
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(value)
}

pub(super) fn listing(raw: Option<&String>) -> Result<Value> {
    let requested = expand(raw);
    let path = requested
        .canonicalize()
        .with_context(|| format!("could not open {}", requested.display()))?;
    if !path.is_dir() {
        bail!("{} is not a directory", path.display());
    }
    let mut entries = Vec::new();
    let mut names = Vec::new();
    for entry in
        std::fs::read_dir(&path).with_context(|| format!("could not read {}", path.display()))?
    {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with('.') {
            continue;
        }
        names.push(name.to_string());
    }
    names.sort_by_key(|name| name.to_ascii_lowercase());
    for name in names.into_iter().take(MAX_ENTRIES) {
        let candidate = path.join(&name);
        if candidate.is_dir() {
            entries.push(json!({ "name": name, "path": candidate }));
        }
    }
    let parent = path.parent().filter(|parent| *parent != FsPath::new("/"));
    Ok(json!({
        "path": path,
        "parent": parent,
        "home": home_dir(),
        "entries": entries,
    }))
}

pub(super) async fn list_dirs(Query(query): Query<HashMap<String, String>>) -> Response {
    match listing(query.get("path")) {
        Ok(value) => Json(value).into_response(),
        Err(error) => (StatusCode::BAD_REQUEST, format!("{error:#}")).into_response(),
    }
}

pub(super) async fn node_list_dirs(
    State(state): State<Arc<AppState>>,
    Path(node): Path<String>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if node == "local" {
        return list_dirs(Query(query)).await;
    }
    let peer = match peer_by_id(&state, &node) {
        Ok(peer) => peer,
        Err(error) => return (StatusCode::NOT_FOUND, error.to_string()).into_response(),
    };
    let Some(url) = state.peer_url(&peer) else {
        return (StatusCode::BAD_GATEWAY, "peer has no reachable URL").into_response();
    };
    let mut target = format!("{}/api/v1/peer/fs/dirs", url.trim_end_matches('/'));
    if let Some(path) = query.get("path") {
        target.push_str("?path=");
        target.push_str(&urlencode(path));
    }
    match state
        .client
        .get(target)
        .header("X-Orqella-Peer-Token", &peer.token)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(response) => {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            let value: Value =
                serde_json::from_str(&text).unwrap_or_else(|_| json!({ "error": text }));
            (
                StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
                Json(value),
            )
                .into_response()
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            format!("could not list peer directories: {error}"),
        )
            .into_response(),
    }
}

pub(super) async fn peer_list_dirs(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if !peer_authorized(&state, &headers) {
        return (StatusCode::UNAUTHORIZED, "invalid peer token").into_response();
    }
    list_dirs(Query(query)).await
}

fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_only_directories_and_hides_dotfiles() {
        let root = std::env::temp_dir().join(format!("orqella-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join("file.txt"), "x").unwrap();
        let value = listing(Some(&root.to_string_lossy().to_string())).expect("listing");
        let names = value["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter_map(|entry| entry["name"].as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["src"]);
        assert_eq!(
            value["parent"].as_str(),
            root.canonicalize()
                .unwrap()
                .parent()
                .map(|path| path.to_string_lossy().to_string())
                .as_deref()
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_files() {
        let file = std::env::temp_dir().join(format!("orqella-path-file-{}", std::process::id()));
        std::fs::write(&file, "x").unwrap();
        let error = listing(Some(&file.to_string_lossy().to_string())).unwrap_err();
        assert!(error.to_string().contains("not a directory"));
        let _ = std::fs::remove_file(file);
    }

    #[test]
    fn expands_home() {
        let value = listing(Some(&"~".to_string())).expect("home listing");
        assert_eq!(
            value["path"].as_str().unwrap(),
            home_dir().canonicalize().unwrap().to_string_lossy()
        );
    }
}
