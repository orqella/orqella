use super::*;
use std::collections::{BTreeMap, HashSet};

const MAX_DASHBOARDS: usize = 50;
const MAX_NAME_LEN: usize = 60;
const MAX_ID_LEN: usize = 80;
const MAX_VISIBLE_ENTRIES: usize = 512;
const MAX_VISIBLE_KEY_LEN: usize = 200;
const MAX_LAYOUT_BYTES: usize = 128 * 1024;
const MAX_LAYOUT_ENTRIES: usize = 4096;
const MAX_LAYOUT_KEY_LEN: usize = 200;
const MAX_LAYOUT_COLUMNS: u64 = 48;
const MAX_DOCUMENT_BYTES: usize = 8 * 1024 * 1024;
const OVERVIEW_ID: &str = "overview";
const LAYOUT_PROFILES: [&str; 2] = ["desktop", "mobile"];

/// A custom dashboard shared by every client connected to this daemon.
///
/// Card visibility is daemon-owned. Geometry lives in `layouts`, split into
/// independent desktop and mobile profiles so devices resolve separately.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct SharedDashboard {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub visible: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct OverviewStore {
    #[serde(default)]
    initialized: bool,
    #[serde(default)]
    visible: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DashboardDocument {
    #[serde(default = "document_version")]
    version: u32,
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    dashboards: Vec<SharedDashboard>,
    #[serde(default)]
    overview: OverviewStore,
    /// Dashboard id -> profile (`desktop` or `mobile`) -> layout document.
    #[serde(default)]
    layouts: BTreeMap<String, BTreeMap<String, Value>>,
    #[serde(default, rename = "layoutsInitialized")]
    layouts_initialized: bool,
}

impl Default for DashboardDocument {
    fn default() -> Self {
        Self {
            version: document_version(),
            revision: 0,
            dashboards: Vec::new(),
            overview: OverviewStore::default(),
            layouts: BTreeMap::new(),
            layouts_initialized: false,
        }
    }
}

#[derive(Deserialize)]
pub(super) struct DashboardUpdate {
    #[serde(default)]
    dashboards: Vec<SharedDashboard>,
    #[serde(default)]
    overview: Option<OverviewUpdate>,
}

#[derive(Deserialize)]
pub(super) struct OverviewUpdate {
    #[serde(default)]
    visible: BTreeMap<String, bool>,
}

#[derive(Deserialize)]
pub(super) struct LayoutUpdate {
    layout: Value,
}

fn document_version() -> u32 {
    2
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

fn write_document(
    state: &AppState,
    document: &DashboardDocument,
) -> std::result::Result<(), String> {
    let raw = serde_json::to_vec_pretty(document).map_err(|error| error.to_string())?;
    if raw.len() > MAX_DOCUMENT_BYTES {
        return Err("dashboard store is too large".into());
    }
    util::write_atomic(&store_path(state), &raw).map_err(|error| format!("{error:#}"))
}

fn document_json(document: &DashboardDocument, initialized: bool) -> Value {
    json!({
        "version": document.version,
        "revision": document.revision,
        "initialized": initialized,
        "dashboards": document.dashboards,
        "overview": {
            "initialized": document.overview.initialized,
            "visible": document.overview.visible,
        },
        "layouts": document.layouts,
        "layoutsInitialized": document.layouts_initialized,
    })
}

fn empty_json() -> Value {
    document_json(&DashboardDocument::default(), false)
}

fn validate_visible(visible: &BTreeMap<String, bool>) -> std::result::Result<(), String> {
    if visible.len() > MAX_VISIBLE_ENTRIES {
        return Err("too many card visibility entries".into());
    }
    if visible
        .keys()
        .any(|key| key.is_empty() || key.chars().count() > MAX_VISIBLE_KEY_LEN)
    {
        return Err("invalid card visibility key".into());
    }
    Ok(())
}

fn normalize(
    mut dashboards: Vec<SharedDashboard>,
) -> std::result::Result<Vec<SharedDashboard>, String> {
    if dashboards.len() > MAX_DASHBOARDS {
        return Err(format!(
            "at most {MAX_DASHBOARDS} custom dashboards are supported"
        ));
    }
    let mut ids = HashSet::new();
    let mut names = HashSet::new();
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
        validate_visible(&dashboard.visible)?;
    }
    Ok(dashboards)
}

fn validate_layout(profile: &str, layout: &Value) -> std::result::Result<(), String> {
    if !LAYOUT_PROFILES.contains(&profile) {
        return Err("layout profile must be desktop or mobile".into());
    }
    let Some(object) = layout.as_object() else {
        return Err("layout must be an object".into());
    };
    let size = serde_json::to_vec(layout)
        .map_err(|error| error.to_string())?
        .len();
    if size > MAX_LAYOUT_BYTES {
        return Err("layout is too large".into());
    }
    if let Some(columns) = object.get("columns") {
        let Some(columns) = columns.as_u64() else {
            return Err("layout columns must be a positive integer".into());
        };
        if columns == 0 || columns > MAX_LAYOUT_COLUMNS {
            return Err(format!(
                "layout columns must be between 1 and {MAX_LAYOUT_COLUMNS}"
            ));
        }
    }
    for key in ["order", "fullWidth"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        let Some(entries) = value.as_array() else {
            return Err(format!("layout {key} must be an array"));
        };
        if entries.len() > MAX_LAYOUT_ENTRIES {
            return Err(format!("layout {key} has too many entries"));
        }
        if entries.iter().any(|entry| {
            entry
                .as_str()
                .is_none_or(|value| value.is_empty() || value.chars().count() > MAX_LAYOUT_KEY_LEN)
        }) {
            return Err(format!("layout {key} contains an invalid card id"));
        }
    }
    for key in ["sizes", "positions"] {
        let Some(value) = object.get(key) else {
            continue;
        };
        let Some(entries) = value.as_object() else {
            return Err(format!("layout {key} must be an object"));
        };
        if entries.len() > MAX_LAYOUT_ENTRIES {
            return Err(format!("layout {key} has too many entries"));
        }
        if entries
            .keys()
            .any(|id| id.is_empty() || id.chars().count() > MAX_LAYOUT_KEY_LEN)
        {
            return Err(format!("layout {key} contains an invalid card id"));
        }
    }
    Ok(())
}

pub(super) async fn list_dashboards(State(state): State<Arc<AppState>>) -> Response {
    match read_document(&state) {
        Ok(Some(document)) => Json(document_json(&document, true)).into_response(),
        Ok(None) => Json(empty_json()).into_response(),
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
    let mut document = match read_document(&state) {
        Ok(Some(document)) => document,
        Ok(None) => DashboardDocument::default(),
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response();
        }
    };
    if let Some(overview) = body.overview {
        if let Err(error) = validate_visible(&overview.visible) {
            return (StatusCode::BAD_REQUEST, error).into_response();
        }
        document.overview.visible = overview.visible;
        document.overview.initialized = true;
    }
    let keep: HashSet<&str> = dashboards
        .iter()
        .map(|dashboard| dashboard.id.as_str())
        .collect();
    document
        .layouts
        .retain(|id, _| id == OVERVIEW_ID || keep.contains(id.as_str()));
    document.version = document_version();
    document.dashboards = dashboards;
    document.revision = document.revision.saturating_add(1);
    if let Err(error) = write_document(&state, &document) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response();
    }
    state.audit(
        "dashboards.save",
        json!({ "count": document.dashboards.len() }),
    );
    Json(document_json(&document, true)).into_response()
}

/// Save one layout profile for one dashboard.
///
/// Separate writes keep a phone reorder from clobbering the desktop layout even
/// when both clients save around the same time.
pub(super) async fn save_layout(
    State(state): State<Arc<AppState>>,
    Path((id, profile)): Path<(String, String)>,
    Json(body): Json<LayoutUpdate>,
) -> Response {
    if let Err(error) = validate_layout(&profile, &body.layout) {
        return (StatusCode::BAD_REQUEST, error).into_response();
    }
    if id != OVERVIEW_ID && (id.is_empty() || id.chars().count() > MAX_ID_LEN) {
        return (StatusCode::BAD_REQUEST, "invalid dashboard id".to_string()).into_response();
    }
    let _guard = state.dashboards_lock.lock().await;
    let mut document = match read_document(&state) {
        Ok(Some(document)) => document,
        Ok(None) => DashboardDocument::default(),
        Err(error) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, format!("{error:#}")).into_response();
        }
    };
    if id != OVERVIEW_ID
        && !document
            .dashboards
            .iter()
            .any(|dashboard| dashboard.id == id)
    {
        return (StatusCode::NOT_FOUND, "dashboard not found").into_response();
    }
    document
        .layouts
        .entry(id.clone())
        .or_default()
        .insert(profile.clone(), body.layout);
    document.layouts_initialized = true;
    document.version = document_version();
    document.revision = document.revision.saturating_add(1);
    if let Err(error) = write_document(&state, &document) {
        return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response();
    }
    state.audit(
        "dashboards.layout",
        json!({ "dashboard": id, "profile": profile }),
    );
    Json(document_json(&document, true)).into_response()
}
