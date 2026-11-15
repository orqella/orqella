use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    #[serde(default = "default_api_version")]
    pub api_version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub entry: Option<String>,
    #[serde(default)]
    pub overview: Option<String>,
    #[serde(default)]
    pub settings: Option<String>,
    #[serde(default = "default_true")]
    pub overview_default: bool,
    #[serde(default = "default_true")]
    pub page_default: bool,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub node_scope: NodeScope,
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Optional or required module dependencies. This is metadata for install,
    /// UI, and future update ordering; it does not imply automatic installation
    /// yet.
    #[serde(default)]
    pub dependencies: Vec<ModuleDependency>,
    #[serde(default)]
    pub backend: BackendConfig,
    #[serde(default)]
    pub ui: UiConfig,
    #[serde(default)]
    pub config: ModuleConfigSchema,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModuleDependency {
    pub id: String,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NodeScope {
    #[default]
    Node,
    Aggregate,
    Hub,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    #[default]
    Page,
    App,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModuleConfigSchema {
    #[serde(default)]
    pub fields: Vec<ConfigField>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigField {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: ConfigFieldType,
    #[serde(default = "default_config_value")]
    pub default: toml::Value,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub min: Option<f64>,
    #[serde(default)]
    pub max: Option<f64>,
    #[serde(default)]
    pub step: Option<f64>,
    #[serde(default)]
    pub options: Vec<ConfigOption>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ConfigFieldType {
    Boolean,
    Number,
    Text,
    Select,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    #[serde(default)]
    pub surface: Surface,
    #[serde(default)]
    pub min_width: Option<u16>,
    /// Preferred overview card width in dashboard grid columns.
    #[serde(default = "default_overview_width")]
    pub overview_width: u16,
    /// Preferred overview card height in dashboard grid rows.
    #[serde(default = "default_overview_height")]
    pub overview_height: u16,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            surface: Surface::Page,
            min_width: None,
            overview_width: default_overview_width(),
            overview_height: default_overview_height(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BackendConfig {
    #[serde(default)]
    pub kind: BackendKind,
    #[serde(default)]
    pub command: Vec<String>,
    /// Optional prebuilt sidecar executables selected by host platform.
    #[serde(default)]
    pub artifacts: Vec<BackendArtifact>,
    #[serde(default = "default_health_path")]
    pub health_path: String,
    #[serde(default = "default_ready_timeout")]
    pub ready_timeout_ms: u64,
    #[serde(default)]
    pub read_paths: Vec<String>,
    #[serde(default)]
    pub sandbox: Option<bool>,
    /// Allow a sandboxed sidecar to use the host network. Default false: a
    /// sandboxed sidecar gets a private network namespace.
    #[serde(default)]
    pub network: bool,
    /// Optional platform allow-list. Empty means every supported host.
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Optional Python version requirement, for example `>=3.11`.
    /// When set, Orqella creates a managed virtual environment when
    /// `requirements` is also set and runs the sidecar with that interpreter.
    #[serde(default)]
    pub python: Option<String>,
    /// Optional requirements file, relative to the module root. The venv is
    /// created under the daemon data directory at `sidecar-venvs/<module-id>`
    /// and refreshed when this file changes.
    #[serde(default)]
    pub requirements: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackendArtifact {
    /// Host platform as `<os>-<arch>`, for example `linux-x86_64` or
    /// `macos-aarch64`.
    pub platform: String,
    /// Direct HTTP(S) URL for the executable. Mutually exclusive with `path`.
    #[serde(default)]
    pub url: String,
    /// Executable path relative to the module root. Mutually exclusive with
    /// `url`; useful for private modules that ship their own artifacts.
    #[serde(default)]
    pub path: String,
    /// Lowercase or uppercase SHA-256 digest of the executable.
    pub sha256: String,
    /// Optional arguments passed to the downloaded executable.
    #[serde(default)]
    pub args: Vec<String>,
}

impl BackendConfig {
    pub fn supported_here(&self) -> bool {
        if self.platforms.is_empty() {
            return true;
        }
        let current = host_os();
        self.platforms
            .iter()
            .any(|platform| platform.eq_ignore_ascii_case(current))
    }

    pub fn artifact_for_current_platform(&self) -> Option<&BackendArtifact> {
        let current = host_platform();
        self.artifacts
            .iter()
            .find(|artifact| artifact.platform.eq_ignore_ascii_case(&current))
    }
}

pub fn host_os() -> &'static str {
    std::env::consts::OS
}

pub fn host_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    #[default]
    None,
    Sidecar,
}

impl Manifest {
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join("module.toml");
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("could not read {}", path.display()))?;
        let manifest: Self = toml::from_str(&raw)
            .with_context(|| format!("invalid module manifest {}", path.display()))?;
        manifest.validate(root)?;
        Ok(manifest)
    }

    pub fn validate(&self, root: &Path) -> Result<()> {
        crate::util::validate_module_id(&self.id)?;
        if self.schema != 1 {
            bail!("unsupported module schema {} (expected 1)", self.schema);
        }
        if self.api_version != "1" {
            bail!(
                "module requires host API {}, this daemon supports 1",
                self.api_version
            );
        }
        if self.name.trim().is_empty() {
            bail!("module name may not be empty");
        }
        if self.version.trim().is_empty() {
            bail!("module version may not be empty");
        }
        if self.entry.is_none() && self.overview.is_none() {
            bail!("module must define entry (page), overview, or both");
        }
        if let Some(entry) = &self.entry {
            validate_module_asset(root, "entry", entry)?;
        }
        if let Some(overview) = &self.overview {
            validate_module_asset(root, "overview", overview)?;
        }
        if let Some(settings) = &self.settings {
            validate_module_asset(root, "settings", settings)?;
        }
        if let Some(icon) = &self.icon {
            validate_module_asset(root, "icon", icon)?;
            // Icons are rendered by the shell. Keep them to a boring path
            // shape and require an actual image so a module cannot turn its
            // icon into an HTML/script injection payload.
            if icon.chars().any(|character| {
                character.is_control()
                    || matches!(character, '<' | '>' | '"' | '\'' | '&' | '`' | '\\')
            }) {
                bail!("icon path contains unsupported characters");
            }
            let path = crate::util::safe_join(root, icon)
                .ok_or_else(|| anyhow::anyhow!("icon {icon} is not a safe relative path"))?;
            let mime = mime_guess::from_path(&path).first_or_octet_stream();
            if mime.type_() != mime_guess::mime::IMAGE {
                bail!("icon {icon} must be an image file");
            }
        }
        for permission in &self.permissions {
            validate_permission(permission)?;
        }
        let mut dependency_ids = std::collections::HashSet::new();
        for dependency in &self.dependencies {
            crate::util::validate_module_id(&dependency.id)?;
            if dependency.id == self.id {
                bail!("module may not depend on itself");
            }
            if !dependency_ids.insert(dependency.id.clone()) {
                bail!("duplicate module dependency {}", dependency.id);
            }
        }
        if self.ui.overview_width == 0
            || self.ui.overview_width > MAX_OVERVIEW_COLUMNS
            || self.ui.overview_height == 0
            || self.ui.overview_height > MAX_OVERVIEW_ROWS
        {
            bail!(
                "ui.overview_width/height must be within 1..={MAX_OVERVIEW_COLUMNS} and 1..={MAX_OVERVIEW_ROWS}"
            );
        }
        self.config.validate()?;
        match self.backend.kind {
            BackendKind::None => {
                if !self.backend.artifacts.is_empty() {
                    bail!("backend.artifacts requires backend.kind = \"sidecar\"");
                }
            }
            BackendKind::Sidecar => {
                if self.backend.command.is_empty() && self.backend.artifacts.is_empty() {
                    bail!("a sidecar backend requires backend.command or backend.artifacts");
                }
                if self.backend.ready_timeout_ms == 0 {
                    bail!("backend.ready_timeout_ms must be greater than zero");
                }
                for path in &self.backend.read_paths {
                    validate_read_path(path)?;
                }
                if let Some(python) = &self.backend.python
                    && python.trim().is_empty()
                {
                    bail!("backend.python may not be empty");
                }
                if (self.backend.python.is_some() || self.backend.requirements.is_some())
                    && self.backend.artifacts.is_empty()
                {
                    let program = self
                        .backend
                        .command
                        .first()
                        .map(String::as_str)
                        .unwrap_or_default();
                    if !matches!(program, "python" | "python3") {
                        bail!(
                            "backend.python/requirements require a command that starts with python or python3"
                        );
                    }
                }
                if let Some(requirements) = &self.backend.requirements {
                    let trimmed = requirements.trim();
                    if trimmed.is_empty() {
                        bail!("backend.requirements may not be empty");
                    }
                    let path = Path::new(trimmed);
                    if path.is_absolute()
                        || path
                            .components()
                            .any(|part| matches!(part, std::path::Component::ParentDir))
                    {
                        bail!("backend.requirements must be relative to the module root");
                    }
                    if !root.join(path).is_file() {
                        bail!("backend.requirements file does not exist: {trimmed}");
                    }
                }
                for platform in &self.backend.platforms {
                    if !matches!(platform.as_str(), "linux" | "macos" | "windows") {
                        bail!("unknown backend platform `{platform}`");
                    }
                }
                let mut artifact_platforms = std::collections::HashSet::new();
                for artifact in &self.backend.artifacts {
                    validate_artifact_platform(&artifact.platform)?;
                    if !artifact_platforms.insert(artifact.platform.to_ascii_lowercase()) {
                        bail!(
                            "duplicate backend artifact platform `{}`",
                            artifact.platform
                        );
                    }
                    let has_url = !artifact.url.trim().is_empty();
                    let has_path = !artifact.path.trim().is_empty();
                    if has_url == has_path {
                        bail!(
                            "backend artifact for {} must define exactly one of url or path",
                            artifact.platform
                        );
                    }
                    if has_path {
                        let artifact_path = Path::new(artifact.path.trim());
                        if artifact_path.is_absolute()
                            || artifact_path
                                .components()
                                .any(|part| matches!(part, std::path::Component::ParentDir))
                        {
                            bail!(
                                "backend artifact path for {} must be relative to the module root",
                                artifact.platform
                            );
                        }
                        if !root.join(artifact_path).is_file() {
                            bail!(
                                "backend artifact file for {} does not exist: {}",
                                artifact.platform,
                                artifact.path.trim()
                            );
                        }
                    } else {
                        let url = url::Url::parse(artifact.url.trim()).with_context(|| {
                            format!(
                                "backend artifact for {} has an invalid URL",
                                artifact.platform
                            )
                        })?;
                        if !matches!(url.scheme(), "http" | "https") {
                            bail!(
                                "backend artifact for {} must use http:// or https://",
                                artifact.platform
                            );
                        }
                        if !url.username().is_empty() || url.password().is_some() {
                            bail!(
                                "backend artifact URL for {} must not contain credentials",
                                artifact.platform
                            );
                        }
                    }
                    let digest = artifact.sha256.trim();
                    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                        bail!(
                            "backend artifact for {} needs a 64-character SHA-256 digest",
                            artifact.platform
                        );
                    }
                    if artifact.args.iter().any(|arg| arg.contains('\0')) {
                        bail!(
                            "backend artifact arguments for {} may not contain NUL bytes",
                            artifact.platform
                        );
                    }
                }
            }
        }
        Ok(())
    }

    pub fn summary(&self) -> ManifestSummary {
        ManifestSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            version: self.version.clone(),
            api_version: self.api_version.clone(),
            description: self.description.clone(),
            icon: self.icon.clone(),
            node_scope: self.node_scope.clone(),
            surface: self.ui.surface.clone(),
            has_page: self.entry.is_some(),
            has_overview: self.overview.is_some(),
            has_settings: self.settings.is_some(),
            overview_default: self.overview_default,
            overview_width: self.ui.overview_width,
            overview_height: self.ui.overview_height,
            page_default: self.page_default,
            permissions: self.permissions.clone(),
            dependencies: self.dependencies.clone(),
            backend: self.backend.kind.clone(),
            backend_sandbox: self.backend.sandbox,
            backend_network: self.backend.network,
            backend_command: self.backend.command.clone(),
            backend_read_paths: self.backend.read_paths.clone(),
        }
    }
}

impl ModuleConfigSchema {
    pub fn validate(&self) -> Result<()> {
        let mut keys = std::collections::HashSet::new();
        for field in &self.fields {
            if field.key.trim().is_empty() {
                bail!("config field key may not be empty");
            }
            if !field
                .key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
            {
                bail!("invalid config field key {}", field.key);
            }
            if !keys.insert(field.key.clone()) {
                bail!("duplicate config field {}", field.key);
            }
            if field.label.trim().is_empty() {
                bail!("config field {} needs a label", field.key);
            }
            match field.kind {
                ConfigFieldType::Boolean => {
                    if !field.default.is_bool() {
                        bail!("boolean config field {} needs a boolean default", field.key);
                    }
                }
                ConfigFieldType::Number => {
                    if !field.default.is_integer() && !field.default.is_float() {
                        bail!("number config field {} needs a numeric default", field.key);
                    }
                }
                ConfigFieldType::Text => {
                    if !field.default.is_str() {
                        bail!("text config field {} needs a string default", field.key);
                    }
                }
                ConfigFieldType::Select => {
                    if field.options.is_empty() {
                        bail!("select config field {} needs options", field.key);
                    }
                    let default = field.default.as_str().unwrap_or_default();
                    if !field.options.iter().any(|option| option.value == default) {
                        bail!(
                            "select config field {} default must be one of its options",
                            field.key
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ManifestSummary {
    pub id: String,
    pub name: String,
    pub version: String,
    pub api_version: String,
    pub description: String,
    pub icon: Option<String>,
    pub node_scope: NodeScope,
    pub surface: Surface,
    pub has_page: bool,
    pub has_overview: bool,
    pub has_settings: bool,
    pub overview_default: bool,
    pub overview_width: u16,
    pub overview_height: u16,
    pub page_default: bool,
    pub permissions: Vec<String>,
    pub dependencies: Vec<ModuleDependency>,
    pub backend: BackendKind,
    pub backend_sandbox: Option<bool>,
    pub backend_network: bool,
    pub backend_command: Vec<String>,
    pub backend_read_paths: Vec<String>,
}

pub const MAX_OVERVIEW_COLUMNS: u16 = 12;
pub const MAX_OVERVIEW_ROWS: u16 = 24;

fn default_overview_width() -> u16 {
    2
}
fn default_overview_height() -> u16 {
    3
}

fn default_true() -> bool {
    true
}

fn default_config_value() -> toml::Value {
    toml::Value::String(String::new())
}
fn default_api_version() -> String {
    "1".into()
}
fn default_health_path() -> String {
    "/health".into()
}
fn default_ready_timeout() -> u64 {
    15_000
}

fn validate_artifact_platform(platform: &str) -> Result<()> {
    let Some((os, arch)) = platform.split_once('-') else {
        bail!("backend artifact platform must be <os>-<arch>, for example linux-x86_64");
    };
    if !matches!(os, "linux" | "macos" | "windows") {
        bail!("unknown backend artifact operating system `{os}`");
    }
    if arch.is_empty() || !arch.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        bail!("invalid backend artifact architecture `{arch}`");
    }
    Ok(())
}

fn normalize_read_path(value: &str) -> Option<std::path::PathBuf> {
    let path = value.trim();
    if path.is_empty() || path.contains('\0') {
        return None;
    }
    if path == "~" {
        return dirs::home_dir();
    }
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir().map(|home| home.join(rest.trim_start_matches('/')));
    }
    if path.starts_with('~') {
        // `~user` expansion is not supported and would be ambiguous.
        return None;
    }
    let path = std::path::Path::new(path);
    if !path.is_absolute() {
        return None;
    }
    // Collapse `.` and repeated separators without touching the filesystem.
    Some(path.components().collect())
}

fn validate_read_path(value: &str) -> Result<()> {
    let raw = value.trim();
    if raw.is_empty() || raw.contains('\0') {
        bail!("backend.read_paths entries must be non-empty paths");
    }
    if std::path::Path::new(raw)
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        bail!("backend.read_paths may not contain '..'");
    }
    let Some(path) = normalize_read_path(raw) else {
        bail!("backend.read_paths entries must be absolute or start with ~/");
    };
    let Some(home) = dirs::home_dir().map(|home| home.components().collect::<std::path::PathBuf>())
    else {
        bail!("could not determine the home directory for read-path validation");
    };
    if path == home || path == std::path::Path::new("/") || path == std::path::Path::new("/root") {
        bail!("backend.read_paths may not expose the whole filesystem or home directory");
    }
    if matches!(path.to_str(), Some("/proc") | Some("/sys") | Some("/dev")) {
        bail!("backend.read_paths may not include broad system roots");
    }
    if home.starts_with(&path) {
        // `/home`, `/Users`, and other ancestors of the home directory.
        bail!("backend.read_paths may not expose a parent of the home directory");
    }
    if path.starts_with(&home) {
        let relative = path
            .strip_prefix(&home)
            .unwrap_or(std::path::Path::new(""))
            .to_string_lossy()
            .replace('\\', "/")
            .to_ascii_lowercase();
        let broad = [".config", ".local", ".local/share", ".cache", ".mozilla"];
        if broad.contains(&relative.as_str()) {
            bail!("backend.read_paths may not include broad config or cache roots");
        }
        let sensitive = [
            ".ssh",
            ".gnupg",
            ".aws",
            ".azure",
            ".docker",
            ".kube",
            ".password-store",
            ".netrc",
            ".git-credentials",
            ".config/orqella",
            ".local/share/orqella",
            ".config/google-chrome",
            ".config/chromium",
            ".config/bravesoftware",
            ".mozilla",
            "library/keychains",
            "library/application support/google/chrome",
            "library/application support/firefox",
        ];
        if sensitive
            .iter()
            .any(|prefix| relative == *prefix || relative.starts_with(&format!("{prefix}/")))
        {
            bail!("backend.read_paths may not include Orqella or credential directories");
        }
    }
    Ok(())
}

fn validate_module_asset(root: &Path, label: &str, value: &str) -> Result<()> {
    if value.contains('\\') || value.chars().any(char::is_control) {
        bail!("{label} must be a relative path");
    }
    let Some(path) = crate::util::safe_join(root, value) else {
        bail!("{label} {value} is not a safe relative path");
    };
    if !path.is_file() {
        bail!("{label} file {value} does not exist");
    }
    Ok(())
}

/// Permissions that have a real host-API implementation in this daemon.
/// Keep this list in sync with the `rpc`/module routes; `validate_permission`
/// must never accept a grant that has no effect.
pub const IMPLEMENTED_PERMISSIONS: &[&str] = &[
    "node.info",
    "node.metrics",
    "node.services",
    "nodes.read",
    "storage",
    "backend",
    "app.open",
    "agent",
    "media.record",
];

/// Permission namespaces reserved by the manifest schema but not implemented
/// yet. They are rejected instead of silently granted so the permission sheet
/// cannot collect no-op capabilities.
pub const RESERVED_PERMISSION_PREFIXES: &[&str] = &[
    "node.action:",
    "fs.read:",
    "fs.write:",
    "net.fetch:",
    "secrets:",
];

pub fn validate_permission(permission: &str) -> Result<()> {
    if IMPLEMENTED_PERMISSIONS.contains(&permission) {
        return Ok(());
    }
    if let Some(mount) = permission.strip_prefix("node.proxy:") {
        if !mount.trim().is_empty() {
            return Ok(());
        }
        bail!("node.proxy permission requires a mount id");
    }
    if RESERVED_PERMISSION_PREFIXES
        .iter()
        .any(|prefix| permission.starts_with(prefix))
    {
        bail!("permission {permission} is reserved and is not implemented yet");
    }
    bail!("unknown permission {permission}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_a_minimal_manifest() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.demo"
name = "Demo"
version = "0.1.0"
entry = "ui/index.html"
permissions = ["node.metrics"]
"#,
        )
        .unwrap();
        let manifest = Manifest::load(root.path()).unwrap();
        assert_eq!(manifest.id, "com.example.demo");
        assert_eq!(manifest.backend.kind, BackendKind::None);
    }

    #[test]
    fn validates_module_dependencies() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.dependent"
name = "Dependent"
version = "0.1.0"
entry = "ui/index.html"

[[dependencies]]
id = "voice-assistant"
optional = true
reason = "Adds voice integration"
"#,
        )
        .unwrap();
        let manifest = Manifest::load(root.path()).unwrap();
        assert_eq!(manifest.summary().dependencies.len(), 1);
        assert!(manifest.summary().dependencies[0].optional);

        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.dependent"
name = "Dependent"
version = "0.1.0"
entry = "ui/index.html"

[[dependencies]]
id = "com.example.dependent"
"#,
        )
        .unwrap();
        assert!(Manifest::load(root.path()).is_err());
    }

    #[test]
    fn rejects_unknown_permissions() {
        assert!(validate_permission("node.metrics").is_ok());
        assert!(validate_permission("root.everything").is_err());
    }

    #[test]
    fn allows_an_overview_only_module() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/overview.html"), "hello").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.card"
name = "Card"
version = "0.1.0"
overview = "ui/overview.html"
"#,
        )
        .unwrap();
        let manifest = Manifest::load(root.path()).unwrap();
        let summary = manifest.summary();
        assert!(!summary.has_page);
        assert!(summary.has_overview);
        assert!(summary.overview_default);
        assert_eq!(summary.overview_width, 2);
        assert_eq!(summary.overview_height, 3);
    }

    #[test]
    fn validates_overview_preferred_size() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/overview.html"), "hello").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.size"
name = "Size"
version = "0.1.0"
overview = "ui/overview.html"
[ui]
overview_width = 0
"#,
        )
        .unwrap();
        assert!(Manifest::load(root.path()).is_err());
    }

    #[test]
    fn validates_backend_platform_allow_list() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.platforms"
name = "Platforms"
version = "0.1.0"
entry = "ui/index.html"

[backend]
kind = "sidecar"
command = ["python3", "backend/server.py"]
platforms = ["linux", "macos"]
"#,
        )
        .unwrap();
        let manifest = Manifest::load(root.path()).unwrap();
        assert_eq!(manifest.backend.platforms, vec!["linux", "macos"]);

        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.platforms"
name = "Platforms"
version = "0.1.0"
entry = "ui/index.html"

[backend]
kind = "sidecar"
command = ["python3", "backend/server.py"]
platforms = ["plan9"]
"#,
        )
        .unwrap();
        assert!(Manifest::load(root.path()).is_err());
    }

    #[test]
    fn validates_platform_sidecar_artifacts() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        let digest = "a".repeat(64);
        std::fs::write(
            root.path().join("module.toml"),
            format!(
                r#"schema = 1
id = "com.example.artifacts"
name = "Artifacts"
version = "0.1.0"
entry = "ui/index.html"

[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "{}"
url = "https://example.com/sidecar"
sha256 = "{}"
args = ["--serve"]
"#,
                host_platform(),
                digest
            ),
        )
        .unwrap();
        let manifest = Manifest::load(root.path()).unwrap();
        let artifact = manifest
            .backend
            .artifact_for_current_platform()
            .expect("current platform artifact");
        assert_eq!(artifact.args, vec!["--serve"]);

        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.artifacts"
name = "Artifacts"
version = "0.1.0"
entry = "ui/index.html"

[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "plan9-x86_64"
url = "https://example.com/sidecar"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#,
        )
        .unwrap();
        assert!(Manifest::load(root.path()).is_err());

        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.artifacts"
name = "Artifacts"
version = "0.1.0"
entry = "ui/index.html"

[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/sidecar"
sha256 = "not-a-digest"
"#,
        )
        .unwrap();
        assert!(Manifest::load(root.path()).is_err());

        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.artifacts"
name = "Artifacts"
version = "0.1.0"
entry = "ui/index.html"

[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/sidecar"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/other"
sha256 = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
"#,
        )
        .unwrap();
        assert!(Manifest::load(root.path()).is_err());
    }

    #[test]
    fn validates_module_config_schema() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.config"
name = "Config"
version = "0.1.0"
entry = "ui/index.html"

[[config.fields]]
key = "enabled"
type = "boolean"
label = "Enabled"
default = true

[[config.fields]]
key = "mode"
type = "select"
label = "Mode"
default = "fast"
options = [
  { value = "fast", label = "Fast" },
  { value = "safe", label = "Safe" },
]
"#,
        )
        .unwrap();
        let manifest = Manifest::load(root.path()).unwrap();
        assert_eq!(manifest.config.fields.len(), 2);

        std::fs::write(
            root.path().join("module.toml"),
            r#"schema = 1
id = "com.example.config"
name = "Config"
version = "0.1.0"
entry = "ui/index.html"

[[config.fields]]
key = "enabled"
type = "boolean"
label = "Enabled"
default = "yes"
"#,
        )
        .unwrap();
        assert!(Manifest::load(root.path()).is_err());
    }

    #[test]
    fn rejects_unsafe_or_non_image_icons() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("ui")).unwrap();
        std::fs::write(root.path().join("ui/index.html"), "hello").unwrap();
        std::fs::write(root.path().join("ui/icon.svg"), "<svg/>").unwrap();
        let manifest = |icon: &str| {
            format!(
                "schema = 1\nid = \"com.example.icon\"\nname = \"Icon\"\nversion = \"0.1.0\"\nentry = \"ui/index.html\"\nicon = \"{icon}\"\n"
            )
        };

        std::fs::write(root.path().join("module.toml"), manifest("ui/icon.svg")).unwrap();
        assert!(Manifest::load(root.path()).is_ok());

        std::fs::write(root.path().join("module.toml"), manifest("ui/index.html")).unwrap();
        assert!(Manifest::load(root.path()).is_err());

        std::fs::write(
            root.path().join("module.toml"),
            manifest("ui/icon.svg' onerror='alert(1)"),
        )
        .unwrap();
        assert!(Manifest::load(root.path()).is_err());

        std::fs::write(root.path().join("module.toml"), manifest("../outside.svg")).unwrap();
        assert!(Manifest::load(root.path()).is_err());
    }
}

#[cfg(test)]
mod coverage_tests {
    use super::*;

    fn module(root: &Path, manifest: &str) {
        std::fs::create_dir_all(root.join("ui")).unwrap();
        std::fs::write(root.join("ui/index.html"), "page").unwrap();
        std::fs::write(root.join("ui/overview.html"), "card").unwrap();
        std::fs::write(root.join("module.toml"), manifest).unwrap();
    }

    fn base(id: &str, extra: &str) -> String {
        format!(
            r#"schema = 1
id = "{id}"
name = "Demo"
version = "0.1.0"
entry = "ui/index.html"
{extra}
"#
        )
    }

    #[test]
    fn rejects_bad_identity_schema_and_api_versions() {
        for (manifest, expected) in [
            (
                r#"schema = 2
id = "com.example.demo"
name = "Demo"
version = "0.1.0"
entry = "ui/index.html"
"#,
                "unsupported module schema",
            ),
            (
                r#"schema = 1
id = "com.example.demo"
name = "Demo"
version = "0.1.0"
api_version = "2"
entry = "ui/index.html"
"#,
                "host api",
            ),
            (
                r#"schema = 1
id = "../escape"
name = "Demo"
version = "0.1.0"
entry = "ui/index.html"
"#,
                "module id",
            ),
            (
                r#"schema = 1
id = "com.example.demo"
name = " "
version = "0.1.0"
entry = "ui/index.html"
"#,
                "name",
            ),
            (
                r#"schema = 1
id = "com.example.demo"
name = "Demo"
version = ""
entry = "ui/index.html"
"#,
                "version",
            ),
            (
                r#"schema = 1
id = "com.example.demo"
name = "Demo"
version = "0.1.0"
"#,
                "must define entry",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            module(root.path(), manifest);
            let error = Manifest::load(root.path()).unwrap_err();
            assert!(
                format!("{error:#}").to_lowercase().contains(expected),
                "expected {expected:?} in {error:#}"
            );
        }
    }

    #[test]
    fn rejects_unsafe_or_missing_assets() {
        for entry in ["../outside.html", "ui/missing.html", "/etc/passwd"] {
            let root = tempfile::tempdir().unwrap();
            module(
                root.path(),
                &format!(
                    r#"schema = 1
id = "com.example.demo"
name = "Demo"
version = "0.1.0"
entry = "{entry}"
"#
                ),
            );
            assert!(Manifest::load(root.path()).is_err(), "{entry}");
        }
        let root = tempfile::tempdir().unwrap();
        module(
            root.path(),
            &format!("{}\nicon = \"ui/icon.html\"", base("com.example.demo", "")),
        );
        std::fs::write(root.path().join("ui/icon.html"), "<script>").unwrap();
        assert!(Manifest::load(root.path()).is_err());
    }

    #[test]
    fn validates_config_schema_types_and_select_defaults() {
        let cases = [
            (
                r#"
[[config.fields]]
key = "mode"
type = "select"
label = "Mode"
default = "missing"
options = [{ value = "a", label = "A" }]
"#,
                "default must be one of its options",
            ),
            (
                r#"
[[config.fields]]
key = "refresh"
type = "number"
label = "Refresh"
default = "fast"
"#,
                "numeric default",
            ),
            (
                r#"
[[config.fields]]
key = "enabled"
type = "boolean"
label = "Enabled"
default = "yes"
"#,
                "boolean default",
            ),
            (
                r#"
[[config.fields]]
key = "name"
type = "text"
default = "Demo"
"#,
                "label",
            ),
            (
                r#"
[[config.fields]]
key = "bad key"
type = "text"
label = "Bad"
default = ""
"#,
                "invalid config field key",
            ),
            (
                r#"
[[config.fields]]
key = "same"
type = "text"
label = "One"
default = ""

[[config.fields]]
key = "same"
type = "text"
label = "Two"
default = ""
"#,
                "duplicate config field",
            ),
        ];
        for (extra, expected) in cases {
            let root = tempfile::tempdir().unwrap();
            module(root.path(), &base("com.example.config", extra));
            let error = Manifest::load(root.path()).unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "expected {expected:?} in {error:#}"
            );
        }
    }

    #[test]
    fn rejects_broad_or_sensitive_read_paths() {
        for read_path in [
            "/",
            "~",
            "~/",
            "~/.ssh",
            "~//.ssh",
            "~/.ssh/",
            "~/.gnupg",
            "~/.aws",
            "~/.config/orqella",
            "~/.config",
            "~/.local/share/orqella",
            "/proc",
            "/sys",
            "/dev",
            "../outside",
        ] {
            let root = tempfile::tempdir().unwrap();
            module(
                root.path(),
                &base(
                    "com.example.read",
                    &format!(
                        r#"
[backend]
kind = "sidecar"
command = ["python3", "backend/server.py"]
python = ">=3.10"
read_paths = ["{read_path}"]
"#
                    ),
                ),
            );
            assert!(Manifest::load(root.path()).is_err(), "{read_path}");
        }

        if let Some(home) = dirs::home_dir() {
            for read_path in [
                home.join(".ssh").display().to_string(),
                home.join(".aws").display().to_string(),
                home.join(".config/orqella").display().to_string(),
                format!("{}//.ssh", home.display()),
            ] {
                let root = tempfile::tempdir().unwrap();
                module(
                    root.path(),
                    &base(
                        "com.example.read-absolute",
                        &format!(
                            r#"
[backend]
kind = "sidecar"
command = ["python3", "backend/server.py"]
python = ">=3.10"
read_paths = ["{read_path}"]
"#
                        ),
                    ),
                );
                assert!(Manifest::load(root.path()).is_err(), "{read_path}");
            }
        }
    }

    #[test]
    fn validates_requirements_and_artifact_paths() {
        let root = tempfile::tempdir().unwrap();
        module(
            root.path(),
            &base(
                "com.example.reqs",
                r#"
[backend]
kind = "sidecar"
command = ["python3", "backend/server.py"]
python = ">=3.10"
requirements = "backend/requirements.txt"
"#,
            ),
        );
        assert!(Manifest::load(root.path()).is_err());

        std::fs::create_dir(root.path().join("backend")).unwrap();
        std::fs::write(root.path().join("backend/requirements.txt"), "pytest\n").unwrap();
        assert!(Manifest::load(root.path()).is_ok());

        for requirements in ["../requirements.txt", "/tmp/requirements.txt"] {
            let root = tempfile::tempdir().unwrap();
            module(
                root.path(),
                &base(
                    "com.example.reqs",
                    &format!(
                        r#"
[backend]
kind = "sidecar"
command = ["python3", "backend/server.py"]
python = ">=3.10"
requirements = "{requirements}"
"#
                    ),
                ),
            );
            assert!(Manifest::load(root.path()).is_err(), "{requirements}");
        }
    }

    #[test]
    fn rejects_sidecars_without_commands_and_bad_artifacts() {
        let digest = "a".repeat(64);
        let cases = [
            (
                r#"
[backend]
kind = "sidecar"
"#
                .to_string(),
                "requires backend.command or backend.artifacts",
            ),
            (
                format!(
                    r#"
[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/sidecar"
path = "backend/sidecar"
sha256 = "{digest}"
"#
                ),
                "exactly one of url or path",
            ),
            (
                format!(
                    r#"
[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "ftp://example.com/sidecar"
sha256 = "{digest}"
"#
                ),
                "must use http",
            ),
            (
                format!(
                    r#"
[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://user:pass@example.com/sidecar"
sha256 = "{digest}"
"#
                ),
                "credentials",
            ),
            (
                r#"
[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/sidecar"
sha256 = "short"
"#
                .to_string(),
                "SHA-256",
            ),
            (
                format!(
                    r#"
[backend]
kind = "sidecar"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/a"
sha256 = "{digest}"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/b"
sha256 = "{digest}"
"#
                ),
                "duplicate backend artifact platform",
            ),
            (
                r#"
[backend]
kind = "none"

[[backend.artifacts]]
platform = "linux-x86_64"
url = "https://example.com/sidecar"
sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
"#
                .to_string(),
                "requires backend.kind",
            ),
        ];
        for (extra, expected) in cases {
            let root = tempfile::tempdir().unwrap();
            module(root.path(), &base("com.example.backend", &extra));
            let error = Manifest::load(root.path()).unwrap_err();
            assert!(
                format!("{error:#}").contains(expected),
                "expected {expected:?} in {error:#}"
            );
        }
    }

    #[test]
    fn implemented_permissions_are_wired_into_the_daemon() {
        let sources = [
            include_str!("../http.rs"),
            include_str!("../http/rpc.rs"),
            include_str!("../http/module_pages.rs"),
            include_str!("../http/modules.rs"),
        ];
        let shell = include_str!("../../web/app.js");
        for permission in IMPLEMENTED_PERMISSIONS {
            if *permission == "media.record" {
                assert!(shell.contains("media.record"));
            } else {
                let needle = format!("\"{permission}\"");
                assert!(
                    sources.iter().any(|source| source.contains(&needle)),
                    "{permission} has no host-API branch"
                );
            }
        }
        assert!(sources.iter().any(|source| source.contains("node.proxy:")));
    }

    #[test]
    fn permission_allow_list_covers_implemented_and_rejects_reserved() {
        for permission in [
            "node.info",
            "node.metrics",
            "node.services",
            "nodes.read",
            "storage",
            "backend",
            "app.open",
            "agent",
            "media.record",
            "node.proxy:opencode",
        ] {
            assert!(validate_permission(permission).is_ok(), "{permission}");
        }
        for permission in [
            "",
            "root",
            "node.proxy",
            "node.proxy:",
            "notifications",
            "audit",
            "proxy.read",
            "node.action:restart",
            "fs.read:/tmp/data",
            "fs.write:/tmp/data",
            "net.fetch:https://example.com",
            "secrets:example",
            "fs.read",
            "NET.FETCH:x",
        ] {
            assert!(validate_permission(permission).is_err(), "{permission}");
        }
        let error = validate_permission("fs.read:/tmp").unwrap_err();
        assert!(format!("{error:#}").contains("reserved"));
    }
}
