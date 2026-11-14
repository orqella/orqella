use super::*;

const MAX_DASHBOARDS: usize = 50;
const MAX_NAME_LEN: usize = 60;
const MAX_ID_LEN: usize = 80;
const MAX_VISIBLE_ENTRIES: usize = 512;
const MAX_VISIBLE_KEY_LEN: usize = 200;

/// A custom dashboard shared by every client connected to this daemon.
///
/// Layout stays client-local: positions, sizes, and column count depend on the
/// screen, so they are deliberately not stored here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SharedDashboard {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub visible: std::collections::BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DashboardDocument {
    #[serde(default = "document_version")]
    version: u32,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    dashboards: Vec<SharedDashboard>,
}

#[derive(Deserialize)]
pub(super) struct DashboardUpdate {
    #[serde(default)]
    dashboards: Vec<SharedDashboard>,
}

fn document_version() -> u32 {
    1
}

fn store_path(state: &AppState) -> std::path::PathBuf {
    state.paths.data_dir.join("dashboards.json")
}

fn read_document(state: &AppState) -> Result<Option<DashboardDocument>> {
    let path = store_path(state);
    if !path.exists() {
        return Ok(None);
    }
    let _ = util::restrict_file(&path);
    let raw = std::fs::read_to_string(&path)
        .with_context(|| format!("could not read {}", path.display()))?;
    let document = serde_json::from_str(&raw)
        .with_context(|| format!("invalid dashboard store {}", path.display()))?;
    Ok(Some(document))
}

fn document_json(document: &DashboardDocument, initialized: bool) -> Value {
    json!({
        "version": document.version,
        "revision": document.revision,
        "initialized": initialized,
        "dashboards": document.dashboards,
    })
}

fn normalize(
    mut dashboards: Vec<SharedDashboard>,
) -> std::result::Result<Vec<SharedDashboard>, String> {
    if dashboards.len() > MAX_DASHBOARDS {
        return Err(format!(
            "at most {MAX_DASHBOARDS} custom dashboards are supported"
        ));
    }
    let mut ids = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    names.insert("overview".to_string());
    for dashboard in &mut dashboards {
        dashboard.id = dashboard.id.trim().to_string();
        dashboard.name = dashboard.name.trim().to_string();
        if dashboard.id.is_empty()
            || dashboard.id.chars().count() > MAX_ID_LEN
            || !dashboard
                .id
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
        {
            return Err(
                "dashboard ids may contain only letters, numbers, dashes, underscores, and dots"
                    .into(),
            );
        }
        if dashboard.id.eq_ignore_ascii_case("overview") {
            return Err("the overview dashboard id is reserved".into());
        }
        if !ids.insert(dashboard.id.clone()) {
            return Err(format!("duplicate dashboard id `{}`", dashboard.id));
        }
        if dashboard.name.is_empty() || dashboard.name.chars().count() > MAX_NAME_LEN {
            return Err(format!(
                "dashboard names must be 1–{MAX_NAME_LEN} characters"
            ));
        }
        let name_key = dashboard.name.to_lowercase();
        if !names.insert(name_key) {
            return Err(format!("duplicate dashboard name `{}`", dashboard.name));
        }
        if dashboard.visible.len() > MAX_VISIBLE_ENTRIES {
            return Err("too many card visibility entries".into());
        }
        if dashboard
            .visible
            .keys()
            .any(|key| key.is_empty() || key.chars().count() > MAX_VISIBLE_KEY_LEN)
        {
            return Err("invalid card visibility key".into());
        }
    }
    Ok(dashboards)
}

pub(super) async fn list_dashboards(State(state): State<Arc<AppState>>) -> Response {
    match read_document(&state) {
        Ok(Some(document)) => Json(document_json(&document, true)).into_response(),
        Ok(None) => Json(json!({
            "version": 1,
            "revision": 0,
            "initialized": false,
            "dashboards": [],
        }))
        .into_response(),
        Err(error) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response(),
    }
}

pub(super) async fn save_dashboards(
    State(state): State<Arc<AppState>>,
    Json(body): Json<DashboardUpdate>,
) -> Response {
    let _guard = state.dashboards_lock.lock().await;
    let dashboards = match normalize(body.dashboards) {
        Ok(dashboards) => dashboards,
        Err(error) => return (StatusCode::BAD_REQUEST, error).into_response(),
    };
    let revision = match read_document(&state) {
        Ok(current) => current.map(|document| document.revision).unwrap_or(0) + 1,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response();
        }
    };
    let document = DashboardDocument {
        version: 1,
        revision,
        dashboards,
    };
    let raw = match serde_json::to_vec_pretty(&document) {
        Ok(raw) => raw,
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response();
        }
    };
    if let Err(error) = util::write_atomic(&store_path(&state), &raw) {
        return (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response();
    }
    state.audit(
        "dashboards.save",
        json!({ "count": document.dashboards.len() }),
    );
    Json(document_json(&document, true)).into_response()
}
